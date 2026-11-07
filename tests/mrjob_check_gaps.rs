//! Gaps mini_mrjob found in `karac check` and `karac fix`.
//!
//! `match (self, other)` in a `ref self` or `mut ref self` method was
//! refused as a move out of a borrowed place, though the same match over
//! two borrowed parameters is accepted: the checker typed the tuple's
//! `self` as the value it names, so MIR moved it (B-2026-11-07-51).
//!
//! `impl Ord` for a type with no `impl PartialOrd` is refused, rightly,
//! and now with the impl `karac fix` writes in front of it
//! (B-2026-11-07-52).
//!
//! Python's `Dict` was "did you mean 'Div'?"; it is a `Map`
//! (B-2026-11-07-53).

use std::process::{Command, Output, Stdio};

fn karac(args: &[&str], path: &std::path::Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn write(tag: &str, src: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "karac-mrjob-gaps-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).expect("write");
    path
}

/// `src` checks clean and prints `out` under `__mir-run`.
fn runs(tag: &str, src: &str, out: &str) {
    let path = write(tag, src);
    let check = karac(&["check"], &path);
    let run = karac(&["__mir-run"], &path);
    let _ = std::fs::remove_file(&path);
    assert!(
        check.status.success(),
        "{tag}: {}",
        String::from_utf8_lossy(&check.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), out, "{tag}");
}

/// `karac check` refuses `src` with `message`; `karac fix` repairs it so
/// that it checks clean, contains `want`, and prints `out`.
fn fixed_to(tag: &str, src: &str, message: &str, want: &str, out: &str) {
    let path = write(tag, src);
    let before = karac(&["check"], &path);
    let said = String::from_utf8_lossy(&before.stderr).to_string();
    assert!(!before.status.success(), "{tag}: check accepted it");
    assert!(said.contains(message), "{tag}: {said}");
    let fix = karac(&["fix"], &path);
    assert!(fix.status.success(), "{tag}: fix failed");
    let text = std::fs::read_to_string(&path).expect("read");
    let after = karac(&["check"], &path);
    let run = karac(&["__mir-run"], &path);
    let _ = std::fs::remove_file(&path);
    assert!(
        after.status.success(),
        "{tag}: still refused after the fix:\n{}\n{text}",
        String::from_utf8_lossy(&after.stderr)
    );
    assert!(text.contains(want), "{tag}: {text}");
    assert_eq!(String::from_utf8_lossy(&run.stdout), out, "{tag}");
}

#[test]
fn a_tuple_of_ref_self_and_a_ref_parameter_lends_both() {
    runs("ref-self", "#[derive(Clone, PartialEq)]\nenum V {\n    I(i64),\n    T(String),\n}\n\nfn same_kind(a: ref V, b: ref V) -> bool {\n    match (a, b) {\n        (V.I(_), V.I(_)) => true,\n        (V.T(x), V.T(y)) => x.len() == y.len(),\n        _ => false,\n    }\n}\n\nimpl V {\n    fn same(ref self, other: ref V) -> bool {\n        match (self, other) {\n            (V.I(_), V.I(_)) => true,\n            (V.T(x), V.T(y)) => x.len() == y.len(),\n            _ => false,\n        }\n    }\n}\n\nfn main() {\n    let a = V.T(\"ab\".to_string());\n    let b = V.T(\"cd\".to_string());\n    println(same_kind(a, b));\n    println(a.same(b));\n}\n", "true\ntrue\n");
}

#[test]
fn a_tuple_of_mut_ref_self_and_a_ref_parameter_lends_both() {
    runs("mut-self", "#[derive(Clone, PartialEq)]\nenum V {\n    I(i64),\n    T(String),\n}\n\nfn same_kind(a: ref V, b: ref V) -> bool {\n    match (a, b) {\n        (V.I(_), V.I(_)) => true,\n        (V.T(x), V.T(y)) => x.len() == y.len(),\n        _ => false,\n    }\n}\n\nimpl V {\n    fn same(mut ref self, other: ref V) -> bool {\n        match (self, other) {\n            (V.I(_), V.I(_)) => true,\n            (V.T(x), V.T(y)) => x.len() == y.len(),\n            _ => false,\n        }\n    }\n}\n\nfn main() {\n    let mut a = V.T(\"ab\".to_string());\n    let b = V.T(\"cd\".to_string());\n    println(same_kind(a, b));\n    println(a.same(b));\n}\n", "true\ntrue\n");
}

#[test]
fn an_ord_impl_without_partial_ord_gets_the_impl_as_a_fix() {
    fixed_to(
        "ord",
        "#[derive(Clone, PartialEq, Eq)]\nenum Value {\n    Int(i64),\n    Text(String),\n}\n\nimpl Ord for Value {\n    fn cmp(ref self, other: ref Value) -> Ordering {\n        match (self, other) {\n            (Value.Int(a), Value.Int(b)) => a.cmp(b),\n            (Value.Text(a), Value.Text(b)) => a.cmp(b),\n            (Value.Int(_), _) => Ordering.Less,\n            _ => Ordering.Greater,\n        }\n    }\n}\n\nfn main() {\n    let mut v: Vec[Value] = [Value.Text(\"b\".to_string()), Value.Int(3), Value.Text(\"a\".to_string()), Value.Int(1)];\n    v.sort();\n    for x in v.iter() {\n        match x {\n            Value.Int(n) => println(n),\n            Value.Text(s) => println(s),\n        }\n    }\n}\n",
        "impl Ord for Value requires impl PartialOrd for Value",
        "impl PartialOrd for Value {\n    fn partial_cmp(ref self, other: ref Value) -> Option[Ordering] {\n        Some(self.cmp(other))\n    }\n}\n\nimpl Ord for Value {",
        "1\n3\na\nb\n",
    );
}

#[test]
fn python_s_dict_is_a_map() {
    fixed_to(
        "dict",
        "fn main() {\n    let mut b: Dict[String, i64] = Dict.new();\n    b.insert(\"k\".to_string(), 2);\n    println(b.len());\n}\n",
        "undefined type 'Dict', did you mean 'Map'?",
        "let mut b: Map[String, i64] = Map.new();",
        "1\n",
    );
}
