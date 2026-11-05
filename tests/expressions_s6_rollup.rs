//! design.md § 6 (Expressions), the roll-up B-2026-11-05-23: rules the
//! checker accepted or could not run.
//!
//! - an `if` without `else` has type `()`, so its block must produce `()`;
//! - a struct literal in the head of `if`, `while` or `for` is parenthesized
//!   (`karac fix` adds the parentheses);
//! - `()` compares equal to `()`;
//! - `not x == y` warns (`ambiguous_not_comparison`);
//! - a discarded tuple holding a `Result` warns, as a lone `Result` does;
//! - ranges do not chain.

use std::path::PathBuf;
use std::process::{Command, Output};

fn write(tag: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-s6-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    path
}

fn karac(args: &[&str], path: &PathBuf) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .output()
        .unwrap()
}

fn all_output(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

const IF_WITHOUT_ELSE: &str = "`if` without `else` has type '()', but its block produces";

#[test]
fn s6_valued_if_without_else_is_refused_in_statement_position() {
    let path = write(
        "if_stmt",
        "fn main() {\n    let c = true;\n    if c { 5 }\n    println(\"end\");\n}\n",
    );
    let out = karac(&["check"], &path);
    let text = all_output(&out);
    assert_eq!(out.status.code(), Some(1), "{text}");
    assert!(text.contains(IF_WITHOUT_ELSE), "{text}");
    assert!(text.contains(":3:12:"), "{text}");
}

#[test]
fn s6_valued_if_without_else_is_refused_as_a_let_initializer() {
    let path = write(
        "if_let",
        "struct D { s: String }\nfn main() {\n    let n = 1;\n    let _ = if n >= 0 { D { s: \"x\" } };\n}\n",
    );
    let out = karac(&["check"], &path);
    let text = all_output(&out);
    assert_eq!(out.status.code(), Some(1), "{text}");
    assert!(text.contains(&format!("{IF_WITHOUT_ELSE} 'D'")), "{text}");
}

#[test]
fn s6_unit_if_without_else_and_discarded_value_are_accepted() {
    let path = write(
        "if_unit",
        "fn f() -> i64 { 5 }\nfn main() {\n    let c = true;\n    if c { f(); }\n    if c { println(\"a\") }\n    let v = if c { 1 } else { 2 };\n    println(f\"{v}\");\n}\n",
    );
    let out = karac(&["run", "--interp"], &path);
    let text = all_output(&out);
    assert_eq!(out.status.code(), Some(0), "{text}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "a\n1\n", "{text}");
}

#[test]
fn s6_struct_literal_in_a_condition_is_refused_and_fixed() {
    let src = "#[derive(PartialEq)]\nstruct Point { x: i64, y: i64 }\nfn main() {\n    let p = Point { x: 0, y: 0 };\n    if p == Point { x: 0, y: 0 } {\n        println(\"eq\");\n    }\n}\n";
    let path = write("struct_head", src);
    let out = karac(&["check"], &path);
    let text = all_output(&out);
    assert_eq!(out.status.code(), Some(1), "{text}");
    assert!(
        text.contains("a struct literal in a condition must be parenthesized"),
        "{text}"
    );
    assert!(text.contains(":5:13:"), "{text}");

    let fixed = karac(&["fix"], &path);
    assert!(fixed.status.success(), "{}", all_output(&fixed));
    let after = std::fs::read_to_string(&path).unwrap();
    assert!(
        after.contains("if p == (Point { x: 0, y: 0 }) {"),
        "{after}"
    );
    let run = karac(&["run", "--interp"], &path);
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "eq\n",
        "{}",
        all_output(&run)
    );
}

#[test]
fn s6_struct_literal_in_while_and_for_heads_is_refused() {
    for (tag, head) in [
        (
            "while",
            "while p == Point { x: 1 } {\n        break;\n    }",
        ),
        ("for", "for _ in Wrap { v: [1] }.v {\n    }"),
    ] {
        let src = format!(
            "#[derive(PartialEq)]\nstruct Point {{ x: i64 }}\nstruct Wrap {{ v: Vec[i64] }}\nfn main() {{\n    let p = Point {{ x: 0 }};\n    {head}\n}}\n"
        );
        let path = write(&format!("struct_{tag}"), &src);
        let out = karac(&["check"], &path);
        let text = all_output(&out);
        assert_eq!(out.status.code(), Some(1), "{tag}: {text}");
        assert!(
            text.contains("a struct literal in a condition must be parenthesized"),
            "{tag}: {text}"
        );
    }
}

#[test]
fn s6_parenthesized_struct_literal_in_a_condition_is_accepted() {
    let path = write(
        "struct_paren",
        "#[derive(PartialEq)]\nstruct Point { x: i64 }\nfn main() {\n    let p = Point { x: 0 };\n    if p == (Point { x: 0 }) {\n        println(\"eq\");\n    }\n    while p != (Point { x: 0 }) {}\n    if p == { Point { x: 0 } } {\n        println(\"block\");\n    }\n}\n",
    );
    let out = karac(&["run", "--interp"], &path);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "eq\nblock\n",
        "{}",
        all_output(&out)
    );
}

#[test]
fn s6_unit_compares_equal_to_unit() {
    let path = write(
        "unit_eq",
        "fn main() {\n    let u: () = ();\n    println(u == ());\n    println(u != ());\n}\n",
    );
    let out = karac(&["run", "--interp"], &path);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "true\nfalse\n",
        "{}",
        all_output(&out)
    );
}

#[cfg(feature = "llvm")]
#[test]
fn s6_unit_compares_equal_to_unit_compiled() {
    let path = write(
        "unit_eq_llvm",
        "fn main() {\n    let u: () = ();\n    println(u == ());\n    println(u != ());\n}\n",
    );
    let out = karac(&["run"], &path);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "true\nfalse\n",
        "{}",
        all_output(&out)
    );
}

#[test]
fn s6_not_beside_a_comparison_warns() {
    let path = write(
        "not_cmp",
        "fn main() {\n    let b = true;\n    if not b == false {\n        println(\"a\");\n    }\n    let c = not (b == false);\n    println(c);\n}\n",
    );
    let out = karac(&["check"], &path);
    let text = all_output(&out);
    assert_eq!(out.status.code(), Some(0), "{text}");
    assert_eq!(
        text.matches("warning[ambiguous_not_comparison]").count(),
        1,
        "{text}"
    );
    assert!(text.contains(":3:8:"), "{text}");

    let json = karac(&["check", "--output=json"], &path);
    let doc: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&json.stdout).trim()).unwrap();
    let names: Vec<&str> = doc["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|d| d["lint_name"].as_str())
        .collect();
    assert_eq!(names, ["ambiguous_not_comparison"], "{doc}");

    let allowed = karac(&["check", "-A", "ambiguous_not_comparison"], &path);
    assert!(
        !all_output(&allowed).contains("ambiguous_not_comparison"),
        "{}",
        all_output(&allowed)
    );
}

#[test]
fn s6_discarded_tuple_of_results_warns() {
    let path = write(
        "tuple_result",
        "fn ok(n: i64) -> Result[i64, String] { Ok(n) }\nfn main() {\n    par { ok(1), ok(2) };\n    (ok(3), 1);\n    let _ = (ok(4), 2);\n}\n",
    );
    let out = karac(&["check"], &path);
    let text = all_output(&out);
    assert_eq!(out.status.code(), Some(0), "{text}");
    assert_eq!(
        text.matches("discarded tuple holding a `Result` value")
            .count(),
        2,
        "{text}"
    );
    assert!(text.contains(":3:5:") && text.contains(":4:5:"), "{text}");
}

#[test]
fn s6_ranges_do_not_chain() {
    let path = write("range_chain", "fn main() {\n    let r = 0..3..5;\n}\n");
    let out = karac(&["check"], &path);
    let text = all_output(&out);
    assert_eq!(out.status.code(), Some(1), "{text}");
    assert!(text.contains("ranges do not chain"), "{text}");
    assert!(text.contains(":2:17:"), "{text}");
}

/// `karac fmt` keeps the parentheses a head's struct literal needs, and adds
/// none inside a delimiter.
#[test]
fn s6_formatter_keeps_a_head_struct_literal_parenthesized() {
    let src = "struct P { x: i64 }\nfn f(p: P) -> i64 { p.x }\nfn main() {\n    let p = P { x: 0 };\n    if let P { x } = (P { x: 1 }) { println(x); }\n    while f(P { x: 2 }) == 0 { break; }\n    for v in (P { x: 3 }).x..4 { println(v); }\n}\n";
    let once = karac::format_source(src).expect("formats");
    assert!(once.contains("if let P { x } = (P {"), "{once}");
    assert!(once.contains("while f(P {"), "{once}");
    assert!(once.contains("for v in (P {"), "{once}");
    let twice = karac::format_source(&once).expect("the formatted program parses");
    assert_eq!(once, twice);
    assert!(
        karac::check_source(&once).is_empty(),
        "{:?}",
        karac::check_source(&once)
            .iter()
            .map(|d| d.message.clone())
            .collect::<Vec<_>>()
    );
}
