//! Sorting or filtering a `Vec[ref T]` in place hands the closure each
//! element's reference. The compiled backends handed it the address of the
//! slot holding the reference, so `b[1]` read the slot as a `Vec`: `karac
//! run` stopped on an index out of bounds, `karac build` left the vector
//! unsorted and ASan reported a stack-buffer-overflow in the comparator,
//! while the MIR interpreter sorted it (B-2026-11-21-6).
#![cfg(feature = "llvm")]

mod common;

use std::process::Command;

/// The program's stdout from `karac <cmd> main.kara`, after `karac check`
/// accepted it; `None` when the compiled backend cannot link.
fn checked_and_run(name: &str, src: &str, cmd: &str) -> Option<String> {
    let dir = std::env::temp_dir().join(format!(
        "karac-ref-elem-sort-{name}-{cmd}-{}",
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
fn sort_by_on_a_vec_of_refs_compares_the_referents() {
    runs_everywhere(
        "sort_by",
        "fn pick(rows: Vec[Vec[i32]]) -> Vec[i32] {\n\
         let mut v: Vec[ref Vec[i32]] = rows.iter().filter(|r| r[0] > 0).collect();\n\
         v.sort_by(|a, b| b[1].cmp(a[1]));\n\
         v.iter().map(|r| r[0]).collect()\n\
         }\n\
         fn main() {\n\
         let rows: Vec[Vec[i32]] = [[1, 5], [2, 9], [-3, 7], [4, 1]];\n\
         let out = pick(rows);\n\
         println(f\"{out[0]} {out[1]} {out[2]}\");\n\
         }\n",
        "2 1 4\n",
    );
}

#[test]
fn sort_by_key_and_retain_on_a_vec_of_refs_see_the_referents() {
    runs_everywhere(
        "by_key_retain",
        "fn main() {\n\
         let rows: Vec[Vec[i32]] = [[1, 5], [2, 9], [-3, 7], [4, 1]];\n\
         let mut v: Vec[ref Vec[i32]] = rows.iter().collect();\n\
         v.sort_by_key(|r| r[1]);\n\
         println(f\"{v[0][0]} {v[1][0]} {v[2][0]} {v[3][0]}\");\n\
         let words: Vec[String] = [\"pear\", \"fig\", \"apple\", \"kiwi\"];\n\
         let mut w: Vec[ref String] = words.iter().collect();\n\
         w.sort_by(|x, y| x.len().cmp(y.len()));\n\
         println(f\"{w[0]} {w[1]} {w[2]} {w[3]}\");\n\
         w.retain(|x| x.len() > 3);\n\
         println(f\"{w.len()} {w[0]} {w[2]}\");\n\
         w.sort();\n\
         println(f\"{w[0]} {w[1]} {w[2]}\");\n\
         }\n",
        "4 1 -3 2\nfig pear kiwi apple\n3 pear apple\napple kiwi pear\n",
    );
}
