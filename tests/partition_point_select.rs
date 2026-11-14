//! B-2026-11-13-34: `partition_point` was the branching binary search
//! (`if pred(x) { lo = mid + 1 } else { hi = mid }`), a branch the CPU
//! mispredicts half the time on a random target, where Rust's is the
//! branch-free halving form (kata 1150 at 2x Rust's time). It is now the
//! halving form with `base` chosen by the library's
//! `__select_unpredictable`, which code generation emits as a `select`
//! marked `!unpredictable` so x86's cmov conversion keeps it a select.
//! The answers are the old search's on every backend.

mod common;

use std::process::{Command, Stdio};

const SRC: &str = "fn main() {\n\
    let xs: Vec[i64] = [1, 3, 3, 5, 7, 9];\n\
    let mut out: Vec[i64] = Vec.new();\n\
    for t in [0, 1, 2, 3, 4, 5, 6, 9, 10] {\n\
        out.push(xs.partition_point(|x| x < t));\n\
    }\n\
    println(f\"{out}\");\n\
    let e: Vec[i64] = Vec.new();\n\
    println(e.partition_point(|x| x < 3));\n\
    let one = [4];\n\
    println(f\"{one.partition_point(|x| x < 4)} {one.partition_point(|x| x < 5)}\");\n\
    let words = [\"a\".to_string(), \"c\".to_string(), \"e\".to_string()];\n\
    println(words.partition_point(|w| w.as_str() < \"d\"));\n\
    println(xs[1..5].partition_point(|x| x <= 5));\n\
    let all: Vec[i64] = (0..1000).collect();\n\
    println(f\"{all.partition_point(|x| x < 0)} {all.partition_point(|x| x < 617)} {all.partition_point(|x| x < 5000)}\");\n\
}\n\
";

const WANT: &str = "[0, 0, 1, 1, 3, 3, 4, 5, 6]\n0\n0 1\n2\n3\n0 617 1000\n";

fn dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("karac-ppoint-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn partition_point_halving_interpreted() {
    let d = dir("interp");
    std::fs::write(d.join("cell.kara"), SRC).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["__mir-run", "cell.kara"])
        .current_dir(&d)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_dir_all(&d);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        WANT,
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn partition_point_select_is_unpredictable_compiled() {
    if !cfg!(feature = "llvm") {
        return;
    }
    let d = dir("llvm");
    let file = d.join("main.kara");
    std::fs::write(&file, SRC).unwrap();
    let exe = d.join("main");
    let ll = d.join("main.ll");
    let built = Command::new(env!("CARGO_BIN_EXE_karac"))
        .env("KARAC_AUTO_PAR", "0")
        .env("KARAC_MIR_CARRY_STORE", "report")
        .env("KARAC_MIR_OPT_LL", &ll)
        .args(["build", file.to_str().unwrap(), "-o", exe.to_str().unwrap()])
        .output()
        .unwrap();
    let Some(()) = common::link_or_skip(if built.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&built.stderr).to_string())
    }) else {
        let _ = std::fs::remove_dir_all(&d);
        return;
    };
    let ran = Command::new(&exe).output().unwrap();
    let mut module = String::new();
    for p in [ll.clone(), d.join(format!("{}.0", ll.display()))] {
        if let Ok(m) = std::fs::read_to_string(p) {
            module.push_str(&m);
        }
    }
    let _ = std::fs::remove_dir_all(&d);
    assert_eq!(String::from_utf8_lossy(&ran.stdout), WANT);
    assert!(
        module
            .lines()
            .any(|l| l.contains("select") && l.contains("!unpredictable")),
        "no `select ... !unpredictable` in the optimised module"
    );
}
