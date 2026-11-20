//! B-2026-11-19-121: references and rename saw only the declaration of a
//! method or an enum variant, and an enum's rename missed it as a unit
//! variant's qualifier in a pattern (`Slot.N =>`) and a type argument in an
//! expression path (`Mix[R].N(63)`), so the renamed program no longer
//! checked. A method of a trait the file does not declare (`drop` in
//! `impl Drop for R`) is refused.

use kara_lsp::analysis;
use lsp_types::{Position, TextEdit};

const SRC: &str = r#"enum Slot[T] { S(T), N }
struct Lx { s: String }
impl Lx { fn text(ref self) -> String { self.s.clone() } }
trait Tr { fn go(ref self) -> i64; }
impl Tr for Lx { fn go(ref self) -> i64 { 1 } }
fn f(x: Slot[i64]) -> i64 { match x { Slot.S(v) => v, Slot.N => 0 } }
fn main() {
    let a = Slot[i64].N;
    let l = Lx { s: "q".to_string() };
    println(l.text());
    println(f(a) + l.go());
}
"#;

/// The (line, column) of each range `references` gives at `line:col`.
fn refs(src: &str, line: u32, col: u32) -> Vec<(u32, u32)> {
    let mut v: Vec<(u32, u32)> = analysis::references(src, Position::new(line, col), true)
        .into_iter()
        .map(|r| (r.start.line, r.start.character))
        .collect();
    v.sort();
    v
}

fn apply(src: &str, edits: &[TextEdit]) -> String {
    let index = analysis::LineIndex::new(src);
    let mut spans: Vec<(usize, usize, &str)> = edits
        .iter()
        .map(|e| {
            (
                index.offset(e.range.start),
                index.offset(e.range.end),
                e.new_text.as_str(),
            )
        })
        .collect();
    spans.sort_by_key(|s| std::cmp::Reverse(s.0));
    let mut out = src.to_string();
    for (a, b, t) in spans {
        out.replace_range(a..b, t);
    }
    out
}

fn renamed(src: &str, line: u32, col: u32, to: &str) -> String {
    let edits = analysis::rename(src, Position::new(line, col), to)
        .expect("a rename")
        .expect("edits");
    apply(src, &edits)
}

fn checks(src: &str) {
    let errs: Vec<_> = karac::check_source(src)
        .into_iter()
        .filter(|d| d.severity == "error")
        .collect();
    assert!(errs.is_empty(), "{src}\n{errs:#?}");
}

#[test]
fn rename_members_inherent_method_reaches_its_calls() {
    assert_eq!(refs(SRC, 2, 13), [(2, 13), (9, 14)]);
    let out = renamed(SRC, 2, 13, "body");
    assert!(
        out.contains("fn body(ref self)") && out.contains("l.body()"),
        "{out}"
    );
    checks(&out);
}

#[test]
fn rename_members_trait_method_is_one_member_with_its_impls_and_calls() {
    // From the trait's declaration, the impl's and the call alike.
    for (line, col) in [(3, 14), (4, 20), (10, 21)] {
        assert_eq!(
            refs(SRC, line, col),
            [(3, 14), (4, 20), (10, 21)],
            "at {line}:{col}"
        );
        let out = renamed(SRC, line, col, "run");
        assert!(
            out.contains("trait Tr { fn run(")
                && out.contains("impl Tr for Lx { fn run(")
                && out.contains("l.run()"),
            "{out}"
        );
        checks(&out);
    }
}

#[test]
fn rename_members_variant_reaches_patterns_and_paths() {
    let out = renamed(SRC, 0, 21, "Empty");
    assert!(
        out.contains("S(T), Empty }")
            && out.contains("Slot.Empty => 0")
            && out.contains("Slot[i64].Empty;"),
        "{out}"
    );
    checks(&out);
}

#[test]
fn rename_members_enum_reaches_a_unit_variant_pattern() {
    let out = renamed(SRC, 0, 5, "Cell");
    assert!(!out.contains("Slot"), "{out}");
    checks(&out);
}

#[test]
fn rename_members_type_argument_in_an_expression_path() {
    let src = "struct R { a: i64 }
enum Mix[T] { N(i64), S(T) }
fn main() {
    let m = Mix[R].N(63);
    match m { Mix.N(v) => println(v), Mix.S(_) => println(\"s\") }
}
";
    let out = renamed(src, 0, 7, "Rec");
    assert!(out.contains("Mix[Rec].N(63)"), "{out}");
    checks(&out);
}

#[test]
fn rename_members_library_trait_method_is_refused() {
    let src = "struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(self.id) } }
fn main() { let _r = R { id: 1 }; }
";
    let err = analysis::rename(src, Position::new(1, 22), "dropx").unwrap_err();
    assert!(err.contains("trait `Drop`"), "{err}");
}
