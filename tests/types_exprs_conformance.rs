//! Claims of design.md § 5 Types, § 6 Expressions and statements and § 7
//! Functions and closures checked against the binary.
//!
//! - A range pattern's bounds were not checked against the scrutinee:
//!   `'a'..='z'` on an `i64` was accepted and matched 100
//!   (B-2026-11-16-244).
//! - An irrefutable `if let` or `while let` pattern was silent; § Pattern
//!   exhaustiveness makes it a warning (B-2026-11-16-245).
//! - Comparisons chained: `a == b == c` and `1 < 2 == true` were accepted,
//!   where § Operators and precedence says they do not (B-2026-11-16-246).

use std::path::PathBuf;
use std::process::{Command, Output};

fn file(tag: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "karac-types-exprs-conf-{}-{tag}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("main.kara"), src).unwrap();
    dir
}

fn check(tag: &str, src: &str) -> (bool, String) {
    let dir = file(tag, src);
    let out: Output = Command::new(env!("CARGO_BIN_EXE_karac"))
        .current_dir(&dir)
        .args(["check", "main.kara"])
        .output()
        .unwrap();
    let text =
        String::from_utf8_lossy(&out.stderr).into_owned() + &String::from_utf8_lossy(&out.stdout);
    (out.status.success(), text)
}

#[test]
fn types_exprs_char_range_on_an_int_is_refused() {
    let (ok, out) = check(
        "charrange",
        "fn main() {\n    let n = 100;\n    let s = match n { 'a'..='z' => 1, _ => 3 };\n    println(f\"{s}\");\n}\n",
    );
    assert!(!ok, "{out}");
    assert!(
        out.contains(
            "expected 'i64', found 'char': a range pattern's bounds have the scrutinee's type"
        ),
        "{out}"
    );
}

#[test]
fn types_exprs_int_range_on_a_char_is_refused() {
    let (ok, out) = check(
        "intrange",
        "fn main() {\n    let c = 'b';\n    let s = match c { 1..=5 => 1, _ => 3 };\n    println(f\"{s}\");\n}\n",
    );
    assert!(!ok, "{out}");
    assert!(out.contains("expected 'char', found 'i64'"), "{out}");
}

#[test]
fn types_exprs_matching_ranges_still_check() {
    let (ok, out) = check(
        "okrange",
        "fn main() {\n    let c = 'b';\n    let n: u8 = 7;\n    let a = match c { 'a'..='z' => 1, _ => 2 };\n    let b = match n { 0..=9 => 1, _ => 2 };\n    println(f\"{a} {b}\");\n}\n",
    );
    assert!(ok, "{out}");
}

#[test]
fn types_exprs_irrefutable_if_let_warns() {
    let (ok, out) = check(
        "iflet",
        "fn main() {\n    let o: Option[i64] = Some(1);\n    if let x = o { println(f\"{x}\"); }\n}\n",
    );
    assert!(ok, "{out}");
    assert!(
        out.contains("warning[irrefutable_let_pattern]") && out.contains("irrefutable `if let`"),
        "{out}"
    );
}

#[test]
fn types_exprs_irrefutable_while_let_warns() {
    let (ok, out) = check(
        "whilelet",
        "fn main() {\n    let mut n = 0;\n    while let (a, b) = (n, 1) {\n        n = a + b;\n        if n > 3 { break; }\n    }\n    println(f\"{n}\");\n}\n",
    );
    assert!(ok, "{out}");
    assert!(out.contains("irrefutable `while let`"), "{out}");
}

#[test]
fn types_exprs_refutable_if_let_is_silent() {
    let (ok, out) = check(
        "refutable",
        "fn main() {\n    let o: Option[i64] = Some(1);\n    if let Some(x) = o { println(f\"{x}\"); }\n}\n",
    );
    assert!(ok, "{out}");
    assert!(!out.contains("irrefutable"), "{out}");
}

#[test]
fn types_exprs_comparisons_do_not_chain() {
    for (tag, e) in [
        ("eqeq", "a == false == false"),
        ("lteq", "1 < 2 == true"),
        ("ltlt", "1 < 2 < 3"),
    ] {
        let src = format!("fn main() {{\n    let a = true;\n    let c = {e};\n    println(f\"{{a}} {{c}}\");\n}}\n");
        let (ok, out) = check(tag, &src);
        assert!(!ok, "{e}: {out}");
        assert!(out.contains("comparisons do not chain"), "{e}: {out}");
    }
}

#[test]
fn types_exprs_parenthesized_and_joined_comparisons_check() {
    let (ok, out) = check(
        "paren",
        "fn main() {\n    let a = true;\n    let c = (a == false) == false;\n    let d = 1 < 2 and 2 < 3;\n    println(f\"{c} {d}\");\n}\n",
    );
    assert!(ok, "{out}");
}

// Review-thread rulings 2026-11-16 on the pass above.

#[test]
fn types_exprs_irrefutable_let_else_warns() {
    let (ok, out) = check(
        "letelse",
        "fn main() {\n    let t = (1, 2);\n    let (a, b) = t else { return; };\n    println(f\"{a} {b}\");\n}\n",
    );
    assert!(ok, "{out}");
    assert!(out.contains("irrefutable `let ... else` pattern"), "{out}");
}

#[test]
fn types_exprs_ref_of_a_temporary_names_the_temporary_rule() {
    let (ok, out) = check(
        "reftemp",
        "fn make_value() -> String { \"hi\" }\nfn main() {\n    let r = ref make_value();\n    println(r);\n}\n",
    );
    assert!(!ok, "{out}");
    assert!(
        out.contains("a reference to a temporary outlives its statement"),
        "{out}"
    );
    assert!(!out.contains("E_REF_OPERAND_UNSUPPORTED"), "{out}");
}

#[test]
fn types_exprs_ref_of_a_temporary_fix_binds_it_first() {
    let dir = file(
        "reftempfix",
        "fn make_value() -> String { \"hi\" }\nfn main() {\n    let r = ref make_value();\n    println(r);\n}\n",
    );
    let fix = Command::new(env!("CARGO_BIN_EXE_karac"))
        .current_dir(&dir)
        .args(["fix", "main.kara"])
        .output()
        .unwrap();
    let fixed = std::fs::read_to_string(dir.join("main.kara")).unwrap();
    assert!(
        fixed.contains("let value = make_value();\n    let r = ref value;"),
        "{fixed}\n{}",
        String::from_utf8_lossy(&fix.stderr)
    );
    let ok = Command::new(env!("CARGO_BIN_EXE_karac"))
        .current_dir(&dir)
        .args(["check", "main.kara"])
        .output()
        .unwrap();
    assert!(ok.status.success(), "{fixed}");
}

#[test]
fn types_exprs_empty_match_names_every_variant() {
    let (ok, out) = check(
        "emptymatch",
        "enum E { A, B(i64), C }\nfn f(e: E) -> i64 { match e { } }\nfn main() { println(f\"{f(E.A)}\"); }\n",
    );
    assert!(!ok, "{out}");
    assert!(
        out.contains("patterns `A`, `B(_)`, `C` not covered"),
        "{out}"
    );
    assert!(!out.contains("missing variants: _"), "{out}");
}

#[test]
fn types_exprs_parallel_assignment_arity_is_one_error() {
    let (ok, out) = check(
        "parallel",
        "fn main() {\n    let mut a = 0;\n    let mut b = 0;\n    a, b = (1, 2, 3);\n    println(f\"{a} {b}\");\n}\n",
    );
    assert!(!ok, "{out}");
    assert!(
        out.contains("tuple pattern has 2 element(s) but type has 3"),
        "{out}"
    );
    assert!(!out.contains("refutable pattern"), "{out}");
}

#[test]
fn types_exprs_float_range_pattern_is_refused_with_the_reason() {
    let (ok, out) = check(
        "floatrange",
        "fn main() {\n    let x = 0.5;\n    let s = match x { 0.0..=1.0 => 1, _ => 2 };\n    println(f\"{s}\");\n}\n",
    );
    assert!(!ok, "{out}");
    assert!(
        out.contains("float range patterns are not supported"),
        "{out}"
    );
    assert!(!out.contains("Expected `=>`"), "{out}");
}

#[test]
fn types_exprs_default_naming_a_parameter_is_one_error() {
    for (tag, main) in [
        ("defuncalled", "fn main() {}\n"),
        ("defcalled", "fn main() { println(f\"{d(1)}\"); }\n"),
    ] {
        let src = format!("fn d(a: i64; b: i64 = a) -> i64 {{ a + b }}\n{main}");
        let (ok, out) = check(tag, &src);
        assert!(!ok, "{out}");
        assert_eq!(out.matches("error[").count(), 1, "{out}");
        assert!(
            out.contains("default parameter value must not reference another parameter ('a')"),
            "{out}"
        );
    }
}
