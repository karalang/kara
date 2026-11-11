//! A literal at a generic parameter stays open when the call's result
//! carries that parameter as a type argument, as it does when the call
//! returns it (design.md §8 Literals, B-2026-11-11-61). A `scan`'s state is
//! held in the `IterScan[I, S, F, OutV]` it returns, so its starting literal
//! stays open through the closure that combines it with other open literals
//! and through further adapters, until the `collect` fixes the state, the
//! items and a literal `lengths` together. Before, the closure's first read
//! of the state settled it to `i64`, and the `Vec[i32]` was refused as
//! `C: FromIterator[i64]`.
//!
//! Beside an item type already fixed (`lengths: Vec[i32]`), the state
//! literal keeps the default by the operator rule (beside a type that widens
//! into the default a literal keeps `i64`), so that form keeps its refusal
//! and its `0i32` fix.

use std::process::{Command, Output, Stdio};

fn karac(args: &[&str], path: &std::path::Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn temp(tag: &str, src: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "karac-scan-state-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    path
}

fn runs_to(tag: &str, src: &str, want: &str) {
    let path = temp(tag, src);
    for lane in [&["run"][..], &["run", "--interp"][..]] {
        let out = karac(lane, &path);
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            want,
            "{tag} {lane:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let _ = std::fs::remove_file(&path);
}

/// Kata 1118's Rust shape: the month starts are `0` chained before the
/// running sums of the month lengths.
#[test]
fn kata_1118_rust_shape_scan_state_takes_the_collected_width() {
    runs_to(
        "1118",
        "fn main() {\n    let lengths = [31, 28, 31, 30];\n    \
         let starts: Vec[i32] = [0].into_iter().chain(lengths.iter().scan(0, |total, d| {\n        \
         let total = total + d;\n        Some((total, total))\n    })).collect();\n    \
         println(starts);\n}\n",
        "[0, 31, 59, 90, 120]\n",
    );
}

#[test]
fn plain_scan_over_a_literal_list_collects_into_the_annotated_width() {
    runs_to(
        "plain",
        "fn main() {\n    let lengths = [31, 28, 31];\n    \
         let v: Vec[i32] = lengths.iter().scan(0, |total, d| {\n        \
         let total = total + d;\n        Some((total, total))\n    }).collect();\n    \
         println(v);\n    \
         let small = [3, 4, 5];\n    let w: Vec[u8] = small.iter().scan(0, |s, d| Some((s + d, s + d))).collect();\n    \
         println(w);\n    \
         let z: Vec[i32] = lengths.iter().scan(0, |s, _d| Some((s, s))).collect();\n    \
         println(z);\n}\n",
        "[31, 59, 90]\n[3, 7, 12]\n[0, 0, 0]\n",
    );
}

#[test]
fn scan_state_literal_beside_fixed_items_keeps_its_default_and_fix() {
    let src = "fn main() {\n    let lengths: Vec[i32] = [31, 28, 31];\n    \
               let v: Vec[i32] = lengths.iter().scan(0, |total, d| {\n        \
               let total = total + d;\n        Some((total, total))\n    }).collect();\n    \
               println(v);\n}\n";
    let path = temp("fixed-items", src);
    let out = karac(&["check"], &path);
    let _ = std::fs::remove_file(&path);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "checked clean");
    assert!(
        stderr.contains("the `scan`'s state is an 'i64'") && stderr.contains("write `0i32`"),
        "{stderr}"
    );
    runs_to(
        "fixed-items-fixed",
        &src.replace("scan(0,", "scan(0i32,"),
        "[31, 59, 90]\n",
    );
}
