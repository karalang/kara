//! Negative bounds and counts that saturate rather than panic, on the
//! interpreter, a built binary and the ASan lane alike:
//! `String.with_capacity` / `Vec.with_capacity` and their `try_` twins
//! take a negative capacity as 0, as `Vec.filled` takes a negative count
//! (collections.md, strings.md; B-2026-11-03-15: the compiled backend
//! panicked `explicit panic`, the interpreter panicked for `Vec` only),
//! and `substring` clamps each bound to `[0, len]`, a negative start
//! included (strings.md; B-2026-11-03-16: a negative start gave "").
#![cfg(feature = "llvm")]

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!(
            "karac-saturating-bounds-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        Scratch(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn asan_available() -> bool {
    if std::env::var_os("KARAC_SKIP_ASAN_TESTS").is_some() {
        return false;
    }
    let dir = Scratch::new("asan-probe");
    let c = dir.path().join("probe.c");
    let exe = dir.path().join("probe");
    std::fs::write(&c, "int main(void){return 0;}\n").expect("write probe");
    Command::new("cc")
        .arg("-fsanitize=address")
        .arg(&c)
        .arg("-o")
        .arg(&exe)
        .status()
        .is_ok_and(|s| s.success())
        && Command::new(&exe).status().is_ok_and(|s| s.success())
}

/// The program's stdout on the interpreter, as a built binary and on the
/// ASan lane, each where it can run here.
fn every_lane(name: &str, src: &str, want: &str) {
    let dir = Scratch::new(name);
    let file = dir.path().join("main.kara");
    std::fs::write(&file, src).unwrap();
    let karac = env!("CARGO_BIN_EXE_karac");
    let f = file.to_str().unwrap();

    let out = Command::new(karac)
        .args(["__mir-run", f])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{name}: interpreter: {err}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        want,
        "{name}: interpreter"
    );

    let exe = dir.path().join("main");
    let built = Command::new(karac)
        .args(["build", f, "-o", exe.to_str().unwrap()])
        .output()
        .unwrap();
    let linked = common::link_or_skip(if built.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&built.stderr).to_string())
    });
    if linked.is_some() {
        let out = Command::new(&exe).stdin(Stdio::null()).output().unwrap();
        assert!(out.status.success(), "{name}: binary: {:?}", out.status);
        assert_eq!(String::from_utf8_lossy(&out.stdout), want, "{name}: binary");
    }

    if !asan_available() {
        eprintln!("{name}: skipping the ASan lane: no ASan-capable cc");
        return;
    }
    let out = Command::new(karac)
        .args(["__mir-llvm-asan", f])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    if out.status.code() == Some(3) && err.contains("Linker failed") {
        let _ = common::link_or_skip(Err(err.into_owned()));
        return;
    }
    assert_eq!(out.status.code(), Some(0), "{name}: ASan:\n{err}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), want, "{name}: ASan");
}

#[test]
fn a_negative_capacity_counts_as_zero() {
    every_lane(
        "capacity",
        r#"fn main() {
    let mut s = String.with_capacity(-3);
    s.push_str("ab");
    println(f"{s} {s.len()}");
    let mut v: Vec[i64] = Vec.with_capacity(-3);
    v.push(4);
    v.push(5);
    println(f"{v} {v.len()}");
    let w: Vec[i64] = Vec.with_capacity(0);
    println(w.len());
    let r: Result[Vec[i64], AllocError] = Vec.try_with_capacity(-2);
    match r {
        Ok(x) => {
            let mut x = x;
            x.push(7);
            println(f"ok {x}");
        }
        Err(_) => println("err"),
    }
    let t: Result[String, AllocError] = String.try_with_capacity(-1);
    match t {
        Ok(x) => println(f"ok [{x}] {x.len()}"),
        Err(_) => println("err"),
    }
}
"#,
        "ab 2\n[4, 5] 2\n0\nok [7]\nok [] 0\n",
    );
}

#[test]
fn substring_clamps_a_negative_start_to_zero() {
    every_lane(
        "substring",
        r#"fn main() {
    let s = "hello";
    println(f"[{s.substring(-5, 2)}] [{s.substring(-1)}] [{s.substring(-1, 100)}] [{s.substring(0, 2)}]");
    println(f"[{s.substring(-9, -2)}] [{s.substring(4, -1)}] [{s.substring(9, 2)}] [{s.substring(5)}]");
    let t = "日本語";
    println(f"[{t.substring(-2, 3)}] [{t.substring(-100, 100)}] [{t.substring(6, -4)}]");
}
"#,
        "[he] [hello] [hello] [he]\n[] [] [] []\n[日] [日本語] []\n",
    );
}
