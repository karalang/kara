//! The type a value position is checked against reaches the expressions
//! that produce its value: an `if` or `match` in a function's tail, or under
//! an annotated `let`, hands the type to each branch (B-2026-10-29-6).
//! A constructor that does not seed keeps the expectation from a generic
//! call inside its payload (B-2026-10-29-5). `karac check` accepts each
//! program and every backend prints its oracle.

use std::process::{Command, Output, Stdio};

/// `karac <backend> <file>` on `src`.
fn karac(tag: &str, backend: &str, src: &str) -> Output {
    let path = std::env::temp_dir().join(format!(
        "karac-expected-tails-{tag}-{backend}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).expect("write");
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg(backend)
        .arg(&path)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_file(&path);
    out
}

fn backends() -> &'static [&'static str] {
    if cfg!(feature = "llvm") {
        &["check", "__mir-run", "__mir-llvm-run"]
    } else {
        &["check", "__mir-run"]
    }
}

/// `src` checks and prints `want` on every backend.
fn runs(tag: &str, src: &str, want: &str) {
    for b in backends() {
        let out = karac(tag, b, src);
        let (stdout, stderr) = (
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
        // `karac check` reports to stderr and prints nothing on success.
        let want = if *b == "check" { "" } else { want };
        assert_eq!(
            (out.status.code(), stdout.as_ref()),
            (Some(0), want),
            "{tag} on {b}: {stderr}"
        );
    }
}

/// The row's repro and its siblings: a closure's inner `collect()` in a
/// branch takes the element type of the `Vec[String]` the branch yields,
/// through an `if`, a `match`, an annotated `let`, a block and an `if let`
/// whose `else` nests both again.
#[test]
fn expected_type_reaches_branch_tails_of_a_mapped_collect() {
    runs(
        "mapped-collect",
        "fn cut(strs: Vec[String], n: i64) -> Vec[String] {
    if n > 0 {
        strs.iter().map(|s| s.chars().skip(n).collect()).collect()
    } else {
        strs.clone()
    }
}
fn cut_match(strs: Vec[String], n: i64) -> Vec[String] {
    match n {
        0 => strs.clone(),
        _ => strs.iter().map(|s| s.chars().skip(n).collect()).collect(),
    }
}
fn cut_let(strs: Vec[String], n: i64) -> Vec[String] {
    let r: Vec[String] = if n > 0 {
        strs.iter().map(|s| s.chars().skip(n).collect()).collect()
    } else {
        strs.clone()
    };
    r
}
fn cut_block(strs: Vec[String], n: i64) -> Vec[String] {
    {
        strs.iter().map(|s| s.chars().skip(n).collect()).collect()
    }
}
fn cut_if_let(strs: Vec[String], n: Option[i64]) -> Vec[String] {
    if let Some(k) = n {
        strs.iter().map(|s| s.chars().rev().skip(k).collect()).collect()
    } else if strs.len() > 5 {
        strs.clone()
    } else {
        match strs.len() {
            1 => strs.iter().map(|s| s.chars().take(1).collect()).collect(),
            _ => strs.clone(),
        }
    }
}
fn main() {
    let v = [\"abc\".to_string(), \"defg\".to_string()].to_vec();
    println(cut(v, 1).join(\",\"));
    println(cut(v, 0).join(\",\"));
    println(cut_match(v, 2).join(\",\"));
    println(cut_match(v, 0).join(\",\"));
    println(cut_let(v, 1).join(\",\"));
    println(cut_block(v, 3).join(\",\"));
    println(cut_if_let(v, Some(1)).join(\",\"));
    println(cut_if_let(v, None).join(\",\"));
    println(cut_if_let([\"xyz\".to_string()].to_vec(), None).join(\",\"));
}
",
        "bc,efg\nabc,defg\nc,fg\nabc,defg\nbc,efg\n,g\nba,fed\nabc,defg\nx\n",
    );
}

/// A non-generic enum's constructor keeps the expected type to itself: the
/// `fold` in its payload takes its accumulator from the `0`, not from the
/// function's return type `Val` (B-2026-10-29-5). A generic call still
/// takes the expectation where it is the checked call itself.
#[test]
fn expected_type_stays_with_a_variant_constructor() {
    runs(
        "variant-payload",
        "enum Val {
    I(i64),
    S(String),
}
struct Box[T] { v: T }
fn id[T](x: own T) -> T { x }
fn total(v: Vec[i64]) -> Val {
    Val.I(v.iter().fold(0, |a, x| a.wrapping_add(*x)))
}
fn wrapped(v: Vec[i64]) -> Option[Val] {
    Some(Val.I(v.iter().fold(0, |a, x| a + *x)))
}
fn boxed() -> Box[u8] {
    Box { v: id(7) }
}
fn show(v: Val) -> String {
    match v {
        Val.I(n) => f\"{n}\",
        Val.S(s) => s.clone(),
    }
}
fn main() {
    let b: Box[i32] = Box { v: 5 };
    let u: u8 = id(200);
    let s = Val.S(id(\"x\".to_string()));
    println(f\"{show(total([1, 2, 3].to_vec()))} {show(wrapped([4, 5].to_vec()).unwrap())} {show(s)}\");
    println(f\"{b.v} {u} {boxed().v}\");
}
",
        "6 9 x\n5 200 7\n",
    );
}
