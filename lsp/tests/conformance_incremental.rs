//! B-2026-11-21-32: a resolve error in one body stops `karac
//! check` before the ownership pass, but an edit to another body checked
//! that body with the erroring one emptied, so the edited body's ownership
//! errors (E0313) were published though the whole check reports none. The
//! focused check must stop where the whole one does, or the edit is a whole
//! check. Found by the LSP conformance pass's edit sweep (corpus/core
//! err_move_out_of_ref_to_keeping_callee, a line duplicated after one was
//! deleted).

use karac::{DocumentChecker, PlaygroundDiagnostic};

fn keys(d: &[PlaygroundDiagnostic]) -> Vec<String> {
    let mut v: Vec<String> = d
        .iter()
        .map(|d| format!("{:?} {} {}", d.code, d.line, d.column))
        .collect();
    v.sort();
    v
}

const BASE: &str = "struct D { id: i64 }
struct W { r: D }
struct K { items: Vec[D] }
impl K {
    fn put(mut ref self, x: own D) { self.items.push(x); }
}
fn k(w: ref W, kk: mut ref K) {
    kk.put(w.r);
}
fn main() {
    let mut kk = K { items: Vec.new() };
    k(w, mut kk);
}
";

#[test]
fn conformance_edit_beside_a_resolve_error_reports_what_check_does() {
    let mut c = DocumentChecker::new();
    let base = c.full(BASE);
    assert_eq!(keys(&base), keys(&karac::check_source(BASE)));
    assert!(
        base.iter().any(|d| d.code.as_deref() == Some("E0100")),
        "{base:#?}"
    );
    let edited = BASE.replacen(
        "    kk.put(w.r);\n",
        "    kk.put(w.r);\n    kk.put(w.r);\n",
        1,
    );
    let (inc, _) = c.check_reporting(&edited);
    assert_eq!(keys(&inc), keys(&karac::check_source(&edited)));
}

/// B-2026-11-21-33: range edits splice in at UTF-16 positions, a
/// character outside the basic plane counting two, and changes apply in
/// order, each to the text the one before it left.
#[test]
fn conformance_range_edits_splice_at_utf16_positions() {
    use lsp_types::{Position, Range, TextDocumentContentChangeEvent};
    let change = |l0, c0, l1, c1, text: &str| TextDocumentContentChangeEvent {
        range: Some(Range {
            start: Position::new(l0, c0),
            end: Position::new(l1, c1),
        }),
        range_length: None,
        text: text.to_string(),
    };
    let mut text = "let s = \"🎉é\"; x\nfn f() {}\n".to_string();
    // After `"🎉é"` (🎉 is two UTF-16 units): replace `x` with `y`.
    kara_lsp::apply_change(&mut text, change(0, 15, 0, 16, "y"));
    assert_eq!(text, "let s = \"🎉é\"; y\nfn f() {}\n");
    // Two changes, the second against the first's result; a position past
    // the line's end clamps to it.
    kara_lsp::apply_change(&mut text, change(1, 0, 1, 0, "// c\n"));
    kara_lsp::apply_change(&mut text, change(2, 99, 2, 99, " // end"));
    assert_eq!(text, "let s = \"🎉é\"; y\n// c\nfn f() {} // end\n");
    // No range: the whole buffer.
    kara_lsp::apply_change(
        &mut text,
        TextDocumentContentChangeEvent {
            range: None,
            range_length: None,
            text: "fn main() {}\n".to_string(),
        },
    );
    assert_eq!(text, "fn main() {}\n");
}
