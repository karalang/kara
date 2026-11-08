//! Kata 2007's ★ arm: a range loop up to a `ref i64`.
//!
//! - `for v in 1..=top` with `top` from `v.iter().max().unwrap()`, a
//!   `ref i64`: the checker reads the bound as its value (core semantics
//!   §5.10), but MIR typed the loop's counter, and the literal start, as
//!   the reference, so both backends refused it ("scalar constant of a
//!   non-scalar type"). The counter is now the integer.
//! - LeetCode's Rust template takes `changed: Vec<i32>` and changes it with
//!   `let mut changed = changed; changed.sort();`, Rust's way of changing
//!   an owned argument. The fix for the write declared the parameter
//!   `mut ref`, which changes the caller's value and needs `mut` at every
//!   call; it now declares it `own`, which keeps Rust's meaning.

use std::path::Path;
use std::process::{Command, Stdio};

fn karac(dir: &Path, args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Every check's diagnostics, the text `karac fix` leaves, whether that
/// text checks clean, and what it prints under the MIR interpreter.
fn mend(dir: &Path, src: &str) -> (String, String, bool, String) {
    std::fs::create_dir_all(dir).unwrap();
    let file = dir.join("cell.kara");
    std::fs::write(&file, format!("{src}\n")).unwrap();
    let mut first = String::new();
    for _ in 0..4 {
        first += &karac(dir, &["check", "cell.kara"]).2;
        let before = std::fs::read_to_string(&file).unwrap();
        karac(dir, &["fix", "cell.kara"]);
        if std::fs::read_to_string(&file).unwrap() == before {
            break;
        }
    }
    let text = std::fs::read_to_string(&file).unwrap();
    let (clean, _, _) = karac(dir, &["check", "cell.kara"]);
    let (_, out, _) = karac(dir, &["__mir-run", "cell.kara"]);
    let _ = std::fs::remove_dir_all(dir);
    (first, text, clean, out)
}

fn scratch(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("karac-kata-2007-{name}-{}", std::process::id()))
}

#[test]
fn kata_2007_range_loop_up_to_a_ref_bound_runs() {
    let src = "fn main() {\n    let xs = [3, 5, 4];\n    let top = xs.iter().max().unwrap();\n    let mut s = 0;\n    for v in 1..=top {\n        s += v;\n    }\n    for v in top..7 {\n        s += v;\n    }\n    let low = xs.iter().min().unwrap();\n    for v in low..top {\n        s += v;\n    }\n    println(s);\n}";
    let (first, text, clean, out) = mend(&scratch("range_loop_up_to_a_r"), src);
    assert!(!first.contains("error"), "{first}");
    assert!(clean, "{text}");
    assert_eq!(out, "33\n");
}

#[test]
fn kata_2007_let_mut_rebinding_a_bare_param_becomes_own() {
    let src = "fn sorted(xs: Vec[i64]) -> Vec[i64] {\n    let mut xs = xs;\n    xs.sort();\n    xs\n}\n\nfn main() {\n    let v = [3, 1, 2];\n    println(f\"{sorted(v.iter().map(|x| x * 2).collect())}\");\n    println(f\"{sorted(v.clone())}\");\n}";
    let (first, text, clean, out) = mend(&scratch("let_mut_rebinding"), src);
    assert!(first.contains("Rust's `let mut xs = xs;`"), "{first}");
    assert!(text.contains("fn sorted(xs: own Vec[i64])"), "{text}");
    assert!(!text.contains("mut ref"), "{text}");
    assert!(clean, "{text}");
    assert_eq!(out, "[2, 4, 6]\n[1, 2, 3]\n");
}
