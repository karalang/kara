//! Found by katas 1162 (B-2026-11-15-48) and 1163; the 1163 gap is
//! B-2026-11-15-21, fixed at bae463436e, and its tests here stay as cover.
//!
//! Kata 1163 rebuilt a string with `String.from_utf8(it.collect())`, Rust's
//! spelling, where `from_utf8` takes `Vec<u8>`. Kāra's takes `Slice[u8]`,
//! and the `collect()` took that slot's type as its target, which nothing
//! gathers into. A `collect()` handed to a `Slice[T]` now gathers a
//! `Vec[T]`, which the slot borrows as it borrows any `Vec`.
//!
//! Kata 1162 misspelt a method inside a `map` closure: after the method's
//! own error, the `collect()` reported `FromIterator[<error>]`, naming the
//! poisoned type. The bound check now says nothing about an item type an
//! earlier error has already been reported for.

use std::process::{Command, Stdio};

fn karac(args: &[&str], path: &std::path::Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn write(tag: &str, src: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("karac-k1163-{tag}-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    path
}

fn run(tag: &str, src: &str) -> (bool, String, String) {
    let path = write(tag, src);
    let out = karac(&["__mir-run"], &path);
    let _ = std::fs::remove_file(&path);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn a_collect_handed_to_from_utf8_gathers_a_vec() {
    let (ok, out, err) = run(
        "utf8",
        "fn main() {\n    let s = \"abc\";\n    let up = String.from_utf8(s.bytes().iter().map(|b| b - 32).collect()).unwrap();\n    println(up);\n}\n",
    );
    assert!(ok, "{err}");
    assert_eq!(out, "ABC\n");
}

#[test]
fn a_collect_handed_to_a_slice_parameter_gathers_a_vec() {
    let (ok, out, err) = run(
        "param",
        "fn total(xs: Slice[i64]) -> i64 {\n    xs.iter().sum()\n}\n\nfn main() {\n    let v = [1, 2, 3];\n    println(f\"{total(v.iter().map(|x| x * 2).collect())}\");\n}\n",
    );
    assert!(ok, "{err}");
    assert_eq!(out, "12\n");
}

#[test]
fn a_collect_after_a_misspelt_method_reports_only_the_method() {
    let path = write(
        "cascade",
        "fn main() {\n    let v: Vec[i64] = \"1,2\".split(\",\").map(|c| c.parse_int().unwrap()).collect();\n    println(f\"{v.len()}\");\n}\n",
    );
    let check = karac(&["check"], &path);
    let _ = std::fs::remove_file(&path);
    let err = String::from_utf8_lossy(&check.stderr).into_owned();
    assert!(err.contains("no method 'parse_int'"), "{err}");
    assert!(!err.contains("<error>"), "{err}");
    assert!(!err.contains("FromIterator"), "{err}");
}
