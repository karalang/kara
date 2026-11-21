//! Kata 2842's Rust arm reads `c = c * ((need - i) as i64) % MOD;` and
//! `Self::pow((i + 1) as i64, MOD - 2)` over `usize` values that are `i64`
//! in Kāra. `karac fix` dropped each redundant `as i64` and left the
//! operand's parentheses behind, `c * ((need - i)) % MOD` and
//! `pow((i + 1), ..)`, which nothing needs. A cast in parentheses of its
//! own now goes with them while its parenthesized operand keeps its own,
//! and a whole argument takes its operand's parentheses with the cast, as
//! a `let`'s value and an index already did. The same goes for an `i32`
//! widened by `as i64` (the `lossless_cast` lint).

use std::path::Path;
use std::process::{Command, Stdio};

/// Runs `karac` on `cell.kara` in `dir`: its verdict, its output, and the
/// file as it stands after.
fn karac(dir: &Path, args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg("cell.kara")
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let fixed = std::fs::read_to_string(dir.join("cell.kara")).unwrap_or_default();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr),
        fixed,
    )
}

/// One `karac fix` round over `src`, then a second that must find nothing,
/// then `__mir-run`: the source after the one round, and what it prints.
fn one_round(tag: &str, src: &str) -> (String, String) {
    let dir = std::env::temp_dir().join(format!("karac-kata-2842-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("cell.kara"), src).unwrap();
    let (_, out, fixed) = karac(&dir, &["fix"]);
    assert!(out.contains("applied"), "{tag}: {out}");
    let (_, out, again) = karac(&dir, &["fix"]);
    assert_eq!(
        again, fixed,
        "{tag}: a second round changed the source: {out}"
    );
    let (ok, out, _) = karac(&dir, &["check"]);
    assert!(ok, "{tag}: {out}\n{fixed}");
    let (ok, out, _) = karac(&dir, &["__mir-run"]);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(ok, "{tag}: {out}\n{fixed}");
    (fixed, out)
}

#[test]
fn kata_2842_redundant_cast_in_its_own_parentheses_keeps_one_pair() {
    let (fixed, out) = one_round(
        "own",
        r#"fn main() {
    let n = 9;
    let mut c = 1;
    for i in 0..3 {
        c = c * ((n - i) as i64) % 1000;
    }
    println(f"{c}");
}
"#,
    );
    assert!(fixed.contains("c = c * (n - i) % 1000;"), "{fixed}");
    assert_eq!(out.trim(), "504");
}

#[test]
fn kata_2842_redundant_cast_as_a_whole_argument_takes_its_parentheses() {
    let (fixed, out) = one_round(
        "argument",
        r#"fn sq(x: i64) -> i64 {
    x * x
}

fn main() {
    let mut c = 0;
    for i in 0..3 {
        c = c + sq((i + 1) as i64);
    }
    println(f"{c}");
}
"#,
    );
    assert!(fixed.contains("c = c + sq(i + 1);"), "{fixed}");
    assert_eq!(out.trim(), "14");
}

#[test]
fn kata_2842_widening_cast_as_an_argument_or_in_parentheses_leaves_none_extra() {
    let (fixed, out) = one_round(
        "widening",
        r#"fn sq(x: i64) -> i64 {
    x * x
}

fn main() {
    let a: i32 = 5;
    let b: i32 = 2;
    let c: i64 = 4;
    let w = sq((a - b) as i64);
    let t = c * ((a - b) as i64);
    println(f"{w} {t}");
}
"#,
    );
    assert!(fixed.contains("let w = sq(a - b);"), "{fixed}");
    assert!(fixed.contains("let t = c * (a - b);"), "{fixed}");
    assert_eq!(out.trim(), "9 12");
}

#[test]
fn kata_2842_cast_inside_a_larger_argument_keeps_its_parentheses() {
    let (fixed, out) = one_round(
        "larger",
        r#"fn sq(x: i64) -> i64 {
    x * x
}

fn main() {
    let n = 9;
    let c = 1;
    let mut t = 0;
    for i in 0..3 {
        t += sq(c * (n - i) as i64);
    }
    println(f"{t}");
}
"#,
    );
    assert!(fixed.contains("t += sq(c * (n - i));"), "{fixed}");
    assert_eq!(out.trim(), "194");
}
