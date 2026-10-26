//! A coroutine's frame holds a borrowed local only while one of its loans
//! is live at a suspension (M4 item 4's tail): just before the suspending
//! call, since the callee may hold an argument's borrow while it is
//! pending, or when the call returns. A local borrowed and dead before
//! every wait lives in its resume call's own stack, so the frame shrinks.
//! `KARAC_MIR_NARROW_FRAMES=0` keeps every borrowed local in the frame.
#![cfg(feature = "llvm")]

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn scratch(name: &str, src: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "karac-frame-narrowing-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    (dir, file)
}

fn karac(file: &Path, cmd: &str, envs: &[(&str, &str)]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg(cmd)
        .arg(file)
        .envs(envs.iter().copied())
        .output()
        .unwrap()
}

/// `src` prints `want` on the interpreter, the compiled backend and under
/// ASan (which also finds a frame that lost a local a loan still needs).
fn runs_everywhere(name: &str, src: &str, want: &str) {
    let (dir, file) = scratch(name, src);
    for cmd in ["__mir-run", "__mir-llvm-run", "__mir-llvm-asan"] {
        if cmd == "__mir-llvm-asan" && std::env::var_os("KARAC_SKIP_ASAN_TESTS").is_some() {
            continue;
        }
        let out = karac(&file, cmd, &[]);
        let err = String::from_utf8_lossy(&out.stderr).to_string();
        if cmd != "__mir-run"
            && common::link_or_skip(if out.status.success() {
                Ok(())
            } else {
                Err(err.clone())
            })
            .is_none()
        {
            continue;
        }
        assert!(out.status.success(), "{name}: {cmd}: {err}");
        assert_eq!(
            String::from_utf8(out.stdout).unwrap(),
            want,
            "{name}: {cmd}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// The size of `main`'s frame in the IR `karac build` writes.
fn main_frame_bytes(file: &Path, dir: &Path, envs: &[(&str, &str)]) -> Option<usize> {
    let ll = dir.join("main.ll");
    let exe = dir.join("main");
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("build")
        .arg(file)
        .arg("-o")
        .arg(&exe)
        .env("KARAC_MIR_LL", &ll)
        .envs(envs.iter().copied())
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    common::link_or_skip(if out.status.success() {
        Ok(())
    } else {
        Err(err.clone())
    })?;
    assert!(out.status.success(), "build: {err}");
    let ir = std::fs::read_to_string(&ll).unwrap();
    let at = ir.find("%frame = alloca [").expect("main's frame") + "%frame = alloca [".len();
    let n = &ir[at..];
    Some(n[..n.find(' ').unwrap()].parse().unwrap())
}

/// A `Copy` local, not used again, whose borrow the suspending callee
/// holds while it is pending: only the loan live before the call keeps it
/// in the frame.
const HELD_BY_THE_CALLEE: &str = "fn show(n: ref i64) {\n    sleep_ms(1);\n    \
    let mut junk: Vec[i64] = Vec.new();\n    for i in 0..64 {\n        junk.push(i * 1000);\n    }\n    \
    println(f\"{*n} {junk.len()}\");\n}\n\n\
    fn main() {\n    let x = 41 + 1;\n    show(x);\n    println(\"done\");\n}\n";

/// A reference read after the wait, and a local borrowed and dropped
/// before it.
const BORROWED_BEFORE: &str = "fn show(s: ref String, n: i64) {\n    sleep_ms(1);\n    \
    println(f\"{s} {n}\");\n}\n\n\
    fn main() {\n    show(\"held by the callee\".to_string(), 1);\n    \
    let b = \"read after the wait\".to_string();\n    let r = ref b;\n    sleep_ms(1);\n    println(r);\n    \
    let mut k = 0;\n    {\n        let c = \"borrowed before the wait only\".to_string();\n        k = c.len();\n    }\n    \
    sleep_ms(1);\n    println(k);\n}\n";

#[test]
fn mir_frame_keeps_a_local_the_pending_callee_borrows() {
    runs_everywhere("held", HELD_BY_THE_CALLEE, "42 64\ndone\n");
}

#[test]
fn mir_frame_keeps_a_loan_live_after_the_wait() {
    runs_everywhere(
        "after",
        BORROWED_BEFORE,
        "held by the callee 1\nread after the wait\n29\n",
    );
}

#[test]
fn mir_frame_leaves_out_a_local_borrowed_only_before_the_wait() {
    let (dir, file) = scratch("size", BORROWED_BEFORE);
    let Some(narrow) = main_frame_bytes(&file, &dir, &[]) else {
        return;
    };
    let wide = main_frame_bytes(&file, &dir, &[("KARAC_MIR_NARROW_FRAMES", "0")]).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(narrow < wide, "frame {narrow} bytes narrowed, {wide} not");
}
