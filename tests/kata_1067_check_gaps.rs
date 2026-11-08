//! Spellings kata 1067's arms met, each carried by `karac fix` to Kāra.
//!
//! - `10.pow(k)` over an `i64` `k`: the exponent of an integer `pow` is a
//!   `u32` (docs/library/core-types.md), and the fix writes the cast the
//!   error names, `k as u32`, or `(k - 1) as u32` around an operator.
//! - `Self::count(d, n)` inside `impl Solution`, the Rust template's call of
//!   an associated function: the fix writes the type's name with a `.`,
//!   `Solution.count(d, n)`. It used to be "expected `;` after this
//!   expression statement" at `Self`, whose fix wrote `Self;::count(..)`.

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
    std::env::temp_dir().join(format!("karac-kata-1067-{name}-{}", std::process::id()))
}

#[test]
fn kata_1067_pow_exponent_gets_its_u32_cast() {
    let src = "fn main() {\n    let k = 3;\n    let v = [2, 4];\n    let powers: Vec[i64] = (1..4).map(|j| 10.pow(j)).collect();\n    println(f\"{10.pow(k)} {2.pow(k - 1)} {3.pow(v.len())} {powers}\");\n}";
    let (first, text, clean, out) = mend(&scratch("pow"), src);
    assert!(
        first.contains("pow expects an exponent of type `u32`"),
        "{first}"
    );
    assert!(clean, "still refused after fix:\n{text}");
    assert!(
        text.contains("10.pow(j as u32)")
            && text.contains("10.pow(k as u32)")
            && text.contains("2.pow((k - 1) as u32)")
            && text.contains("3.pow(v.len() as u32)"),
        "{text}"
    );
    assert_eq!(out.trim_end(), "1000 4 9 [10, 100, 1000]", "{text}");
}

#[test]
fn kata_1067_self_path_call_becomes_the_type_name() {
    let src = "struct Solution {}\n\nimpl Solution {\n    pub fn twice_plus(n: i64) -> i64 {\n        Self::twice(n) + Self::one()\n    }\n\n    fn twice(n: i64) -> i64 {\n        let x = Self::one() + 1;\n        n * x\n    }\n\n    fn one() -> i64 {\n        1\n    }\n}\n\nfn main() {\n    println(Solution.twice_plus(20));\n}";
    let (first, text, clean, out) = mend(&scratch("self-path"), src);
    assert!(
        first.contains("`Self::twice` is Rust's `::` path"),
        "{first}"
    );
    assert!(!first.contains("expected `;`"), "{first}");
    assert!(clean, "still refused after fix:\n{text}");
    assert!(
        text.contains("Solution.twice(n) + Solution.one()")
            && text.contains("let x = Solution.one() + 1;")
            && !text.contains("Self"),
        "{text}"
    );
    assert_eq!(out.trim_end(), "41", "{text}");
}

#[test]
fn kata_1067_self_path_outside_an_impl_has_no_fix_and_no_semicolon() {
    let src = "trait Unit {\n    fn one() -> i64;\n    fn two() -> i64 {\n        Self::one() + Self::one()\n    }\n}\n\nfn main() {\n    println(2);\n}";
    let (first, text, _, _) = mend(&scratch("self-path-trait"), src);
    assert!(first.contains("`Self::one` is Rust's `::` path"), "{first}");
    assert!(!first.contains("expected `;`"), "{first}");
    assert!(text.contains("Self::one() + Self::one()"), "{text}");
}

#[test]
fn kata_1067_self_path_in_a_condition_head_gets_the_fix() {
    // The 2001+ kata thread's kata 2014 shape: `if Self::check(..) {`. In a
    // condition head the `::` used to read as "Expected `{`" and take the
    // rest of the impl with it.
    let src = "struct Solution {}\n\nimpl Solution {\n    fn check(n: i64) -> bool {\n        n > 3\n    }\n\n    fn run(n: i64) -> i64 {\n        if Self::check(n) {\n            return 1;\n        }\n        while Self::check(n + 10) {\n            return 2;\n        }\n        0\n    }\n}\n\nfn main() {\n    println(f\"{Solution.run(5)} {Solution.run(1)}\");\n}";
    let (first, text, clean, out) = mend(&scratch("self-path-cond"), src);
    assert!(
        first.contains("`Self::check` is Rust's `::` path"),
        "{first}"
    );
    assert!(!first.contains("Expected `{`"), "{first}");
    assert!(clean, "still refused after fix:\n{text}");
    assert!(
        text.contains("if Solution.check(n) {") && text.contains("while Solution.check(n + 10) {"),
        "{text}"
    );
    assert_eq!(out.trim_end(), "1 2", "{text}");
}
