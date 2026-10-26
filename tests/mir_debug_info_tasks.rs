//! `KARAC_DEBUG_INFO=1` builds a program that runs tasks: a task's root
//! body, which the lowering makes and which has no source line, gets a
//! line-0 location in its subprogram, so its call of the task's `#resume`
//! passes the LLVM verifier (B-2026-10-26-6).
#![cfg(feature = "llvm")]

mod common;

use std::path::PathBuf;
use std::process::Command;

/// The program's stdout from `karac build` with debug info at `opt`, then
/// running what it built.
fn built_with_debug_info(name: &str, src: &str, opt: &str) -> Option<String> {
    let dir: PathBuf = std::env::temp_dir().join(format!(
        "karac-debug-info-tasks-{name}-{opt}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let exe = dir.join("main");
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("build")
        .arg(&file)
        .arg("-o")
        .arg(&exe)
        .env("KARAC_DEBUG_INFO", "1")
        .env("KARAC_MIR_OPT", opt)
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    common::link_or_skip(if out.status.success() {
        Ok(())
    } else {
        Err(err.clone())
    })?;
    assert!(out.status.success(), "{name} at -O{opt}: build: {err}");
    let run = Command::new(&exe).output().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(run.status.success(), "{name} at -O{opt}: run");
    Some(String::from_utf8(run.stdout).unwrap())
}

/// The row's program: a spawned closure that suspends.
#[test]
fn mir_debug_info_builds_a_spawned_task() {
    let src = "fn work(k: i64) -> i64 {\n    sleep_ms(1);\n    k * 2\n}\n\n\
               fn main() {\n    let g = TaskGroup.new();\n    let h = g.spawn(|| work(21));\n    println(h.join());\n}\n";
    for opt in ["0", "2"] {
        if let Some(out) = built_with_debug_info("spawn", src, opt) {
            assert_eq!(out, "42\n", "at -O{opt}");
        }
    }
}

/// A `par` block's branches, each a task the lowering roots.
#[test]
fn mir_debug_info_builds_par_branches() {
    let src = "fn sq(x: i64) -> i64 {\n    x * x\n}\n\n\
               fn main() {\n    let (a, b) = par { sq(3), sq(4) };\n    println(f\"{a + b}\");\n}\n";
    for opt in ["0", "2"] {
        if let Some(out) = built_with_debug_info("par", src, opt) {
            assert_eq!(out, "25\n", "at -O{opt}");
        }
    }
}
