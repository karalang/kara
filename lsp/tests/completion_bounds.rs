//! B-2026-11-19-124: completion after `x.` offered methods whose impl bounds
//! the receiver does not meet (`cloned` and `flatten` on an `Option[Wide]`,
//! `to_string` on a `Map` whose value has no `Display`), each E0236 when
//! chosen. And references on a `let _` gave the enclosing function's name.

use kara_lsp::analysis;
use lsp_types::Position;

fn at(src: &str) -> (String, Position) {
    let i = src.find('|').expect("a cursor");
    let text = format!("{}{}", &src[..i], &src[i + 1..]);
    let line = src[..i].matches('\n').count() as u32;
    let col = (i - src[..i].rfind('\n').map_or(0, |n| n + 1)) as u32;
    (text, Position::new(line, col))
}

fn labels(src: &str) -> Vec<String> {
    let (text, pos) = at(src);
    analysis::completion(&text, pos)
        .into_iter()
        .map(|c| c.label)
        .collect()
}

#[test]
fn completion_leaves_out_option_methods_the_payload_cannot_take() {
    let got = labels(
        "struct Wide { a: i64 }
fn make() -> Option[Wide] { None }
fn main() {
    let o = make();
    o.|
}
",
    );
    for want in ["map", "unwrap", "is_some", "zip", "and_then"] {
        assert!(got.iter().any(|l| l == want), "{want} missing: {got:?}");
    }
    for gone in ["cloned", "flatten", "unwrap_or_default"] {
        assert!(!got.iter().any(|l| l == gone), "{gone} offered: {got:?}");
    }
}

#[test]
fn completion_keeps_option_methods_the_payload_can_take() {
    let got = labels(
        "fn make() -> Option[Option[i64]] { None }
fn main() {
    let o = make();
    o.|
}
",
    );
    for want in ["flatten", "unwrap_or_default", "map"] {
        assert!(got.iter().any(|l| l == want), "{want} missing: {got:?}");
    }
}

#[test]
fn completion_leaves_out_to_string_on_a_map_of_a_non_display_value() {
    let src = "struct P { a: i64 }
fn main() {
    let mut m: Map[i64, P] = Map.new();
    m.|
}
";
    let got = labels(src);
    assert!(!got.iter().any(|l| l == "to_string"), "{got:?}");
    for want in ["insert", "get", "len"] {
        assert!(got.iter().any(|l| l == want), "{want} missing: {got:?}");
    }
    let got = labels(&src.replace("Map[i64, P]", "Map[i64, String]"));
    assert!(got.iter().any(|l| l == "to_string"), "{got:?}");
}

#[test]
fn references_on_a_wildcard_let_are_empty() {
    let (text, pos) = at("fn f() -> i64 { 1 }
fn main() {
    let |_ = f();
}
");
    assert!(analysis::references(&text, pos, true).is_empty());
}
