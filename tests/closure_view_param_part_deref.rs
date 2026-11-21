//! `*p.0` on an item lent by `.iter()` reads the part through the same
//! borrow (B-2026-10-12-9), in a `for` loop and in an adaptor's closure
//! parameter alike. `karac check` accepted it, but the MIR pipeline's
//! typecheck marked only pattern-bound parts as views, so `__mir-run` and
//! the compiled backends refused it ("unary '*' requires 'ref T' ..
//! found 'i32'"); it now reads a part of a `ref` item the same way
//! (B-2026-11-20-63).
#![cfg(feature = "llvm")]

mod common;

use std::process::Command;

/// The program's stdout from `karac <cmd> main.kara`, after `karac check`
/// accepted it; `None` when the compiled backend cannot link.
fn checked_and_run(name: &str, src: &str, cmd: &str) -> Option<String> {
    let dir = std::env::temp_dir().join(format!(
        "karac-view-param-part-{name}-{cmd}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let karac = env!("CARGO_BIN_EXE_karac");
    let check = Command::new(karac)
        .arg("check")
        .arg(&file)
        .output()
        .unwrap();
    assert!(
        check.status.success(),
        "{name}: check: {}{}",
        String::from_utf8_lossy(&check.stdout),
        String::from_utf8_lossy(&check.stderr)
    );
    let out = Command::new(karac).arg(cmd).arg(&file).output().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    // Only a failed link may skip: a refusal, a runtime error or a
    // sanitizer report is the program's own failure and fails the test.
    let own_failure = err.contains("error[")
        || err.contains("runtime error")
        || err.contains("Sanitizer")
        || err.contains("panicked");
    if cmd != "__mir-run" && !own_failure {
        common::link_or_skip(if out.status.success() {
            Ok(())
        } else {
            Err(err.clone())
        })?;
    }
    assert!(out.status.success(), "{name}: {cmd}: {err}");
    Some(String::from_utf8(out.stdout).unwrap())
}

fn runs_everywhere(name: &str, src: &str, want: &str) {
    for cmd in ["__mir-run", "__mir-llvm-run", "__mir-llvm-asan"] {
        if let Some(out) = checked_and_run(name, src, cmd) {
            assert_eq!(out, want, "{name}: {cmd}\n{src}");
        }
    }
}

#[test]
fn adaptor_closure_view_param_reads_a_part_through_deref() {
    runs_everywhere(
        "map",
        "fn main() {\n\
         let t: Vec[(i32, i32)] = [(1, 2), (3, 4)];\n\
         let out: Vec[i32] = t.iter().map(|p| *p.0).collect();\n\
         let big: Vec[i32] = t.iter().filter(|p| *p.0 > 1).map(|p| *p.1).collect();\n\
         println(f\"{out[0]} {out[1]} {big.len()} {big[0]}\");\n\
         }\n",
        "1 3 1 4\n",
    );
}

#[test]
fn for_loop_item_reads_a_part_through_deref() {
    runs_everywhere(
        "for",
        "struct P { x: i64, name: String }\n\
         fn main() {\n\
         let t: Vec[(i32, i32)] = [(1, 2), (3, 4)];\n\
         let mut s = 0;\n\
         for p in t.iter() { s += *p.0 * *p.1; }\n\
         let ps: Vec[P] = [P { x: 5, name: \"a\".to_string() }, P { x: 6, name: \"bc\".to_string() }];\n\
         let mut xs = 0;\n\
         for p in ps.iter() { xs += *p.x; }\n\
         let total: i64 = ps.iter().map(|p| *p.x + p.name.len()).sum();\n\
         println(f\"{s} {xs} {total}\");\n\
         }\n",
        "14 11 14\n",
    );
}
