//! The type a value position is checked against reaches the expressions
//! that produce its value: an `if` or `match` in a function's tail, or under
//! an annotated `let`, hands the type to each branch (B-2026-10-29-6).
//! A constructor that does not seed keeps the expectation from a generic
//! call inside its payload (B-2026-10-29-5), and a constructor's literal
//! arguments take their type from any slot that names it (B-2026-10-29-24).
//! A comparison's constructor or tuple operand takes the other operand's
//! type (B-2026-10-29-35). `karac check` accepts each program and every
//! backend prints its oracle.

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

/// `Vec.filled(n, 0)` builds the `Vec` its slot names wherever the slot is
/// known, not only under a `let` annotation: a method's argument, a tuple
/// element under an annotation and an `Option` payload (B-2026-10-29-24).
/// The literal is range-checked against that element type.
#[test]
fn expected_type_reaches_a_constructors_literal_arguments() {
    runs(
        "filled",
        "struct Chunk {
    d: Vec[Vec[u64]],
}
fn takes(v: Vec[u64]) -> i64 {
    v.len()
}
fn main() {
    let a = takes(Vec.filled(3, 0));
    let t: (Vec[u64], i64) = (Vec.filled(2, 0), 1);
    let mut d: Vec[Vec[u64]] = Vec.new();
    d.push(Vec.filled(2, 0));
    let o: Option[Vec[u64]] = Some(Vec.filled(2, 7));
    let mut m: Vec[Vec[u8]] = Vec.new();
    m.push(Vec.filled(2, 255));
    m[0][1] = m[0][1].wrapping_add(3);
    let mut c = Chunk { d: Vec.new() };
    c.d.push(Vec.filled(2, 0));
    println(f\"{a} {t.0.len()} {d.len()} {o.unwrap()[1]} {m[0]} {c.d.len()}\");
}
",
        "3 2 1 7 [255, 2] 1\n",
    );
    let out = karac(
        "filled-range",
        "check",
        "fn main() {
    let mut m: Vec[Vec[u8]] = Vec.new();
    m.push(Vec.filled(2, 300));
    let o: Option[Vec[u8]] = Some(Vec.filled(2, 256));
    println(f\"{m.len()} {o.is_some()}\");
}
",
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success()
            && err.contains("integer literal 300 out of range for 'u8'")
            && err.contains("integer literal 256 out of range for 'u8'"),
        "{err}"
    );
}

/// A comparison checks a constructor or tuple operand against the other
/// operand's type, so a `ref` to a `Copy` item inside it reads as the value
/// (core-semantics §5.10; B-2026-10-29-35), on either side and under `!=`.
/// A `ref String` is not `Copy` and stays refused.
#[test]
fn comparison_reads_a_copy_ref_in_a_constructor_operand() {
    runs(
        "compare",
        "fn main() {
    let ns = vec![1, 2, 3];
    let value: Option[i64] = Some(2);
    let pair = (2, 5);
    let r: Result[i64, String] = Ok(3);
    for n in ns.iter() {
        if value == Some(n) { println(f\"a {n}\"); }
        if Some(n) == value { println(f\"b {n}\"); }
        if value != Some(n) { println(f\"c {n}\"); }
        if pair == (n, 5) { println(f\"d {n}\"); }
        if r == Ok(n) { println(f\"e {n}\"); }
    }
}
",
        "c 1\na 2\nb 2\nd 2\nc 3\ne 3\n",
    );
    let out = karac(
        "compare-string",
        "check",
        "fn main() {
    let names = vec![\"x\".to_string()];
    for w in names.iter() {
        let o: Option[String] = Some(\"x\".to_string());
        if o == Some(w) { println(\"w\"); }
    }
}
",
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success()
            && err.contains("cannot compare 'Option[String]' and 'Option[ref String]'"),
        "{err}"
    );
}
