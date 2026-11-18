//! Two diagnostics found pinning B-2026-11-16-303.
//!
//! B-2026-11-17-136: a range-pattern bound that is an expression
//! (`0..=(1 + 2) =>`) got E_RANGE_PATTERN_BOUND_NOT_SIMPLE and then a spurious
//! E0002 at the end of the `match`: the parser gave up on the arm list. It
//! now skips the refused bound and goes on, so the one real error stands
//! alone and a later, real error is still found.
//!
//! B-2026-11-17-137: E0240 rendered `impl Grid[3]` as
//! `impl Grid[Array[(), 3]]`, the const argument's internal encoding.

use std::process::Command;

fn check(tag: &str, src: &str) -> String {
    let dir = std::env::temp_dir().join(format!("karac-rb-cir-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&f)
        .output()
        .expect("karac");
    let _ = std::fs::remove_dir_all(&dir);
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn error_codes(out: &str) -> Vec<&str> {
    out.lines()
        .filter_map(|l| l.strip_prefix("error["))
        .filter_map(|l| l.split(']').next())
        .collect()
}

#[test]
fn an_expression_range_bound_is_one_error_without_a_cascade() {
    let out = check(
        "expr",
        "fn main() {\n    let n = 3;\n    let r = match n {\n        0..=(1 + 2) => \"low\",\n        _ => \"high\",\n    };\n    println(r);\n}\n",
    );
    assert_eq!(
        error_codes(&out),
        ["E_RANGE_PATTERN_BOUND_NOT_SIMPLE"],
        "{out}"
    );
}

#[test]
fn arms_after_a_refused_range_bound_still_parse() {
    let out = check(
        "arms",
        "fn main() {\n    let n = 3;\n    match n {\n        0..=(1 + 2) | 9 => println(\"low\"),\n        -5..=[1][0] if n > 0 => println(\"x\"),\n        _ => println(\"high\"),\n    }\n    let m = 1 +;\n}\n",
    );
    assert_eq!(
        error_codes(&out),
        [
            "E_RANGE_PATTERN_BOUND_NOT_SIMPLE",
            "E_RANGE_PATTERN_BOUND_NOT_SIMPLE",
            "E0002"
        ],
        "{out}"
    );
    assert!(out.contains(":8:"), "the real error on line 8: {out}");
}

#[test]
fn a_conflicting_const_generic_impl_names_the_type_as_written() {
    let out = check(
        "grid",
        "struct Grid[const N: i64] { cells: Array[i64, N] }\n\nimpl Grid[3] { fn size(self) -> i64 { 3 } }\nimpl Grid[3] { fn size(self) -> i64 { 4 } }\n\nfn main() {}\n",
    );
    assert!(
        out.contains("`impl Grid[3]` redefines method `size`"),
        "{out}"
    );
    assert!(!out.contains("Array[()"), "{out}");
}
