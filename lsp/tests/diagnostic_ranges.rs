//! B-2026-11-19-114: kara-lsp put the E0200 "collect the iterator"
//! diagnostic at a zero-width range after the initializer (the machine
//! fix's insertion point) where `karac check` reports the initializer's
//! start. The diagnostic now spans the initializer, and its quick fix still
//! inserts `.collect()` after it.

use kara_lsp::analysis;
use lsp_types::{CodeActionOrCommand, NumberOrString, Position, Range, Uri};

const SRC: &str = "fn main() {\nlet s = \"abc\";\nlet s = s.chars();\nprintln(s.len());\n}\n";

#[test]
fn lsp_collect_the_iterator_diagnostic_spans_the_initializer() {
    let diags = analysis::diagnostics(SRC);
    let d = diags
        .iter()
        .find(|d| d.message.contains("collect the iterator"))
        .unwrap_or_else(|| panic!("no collect diagnostic: {diags:#?}"));
    assert_eq!(d.code, Some(NumberOrString::String("E0200".to_string())));
    // `s.chars()` on line 3 (0-based 2), columns 8..17: where `karac check`
    // reports it (3:9), and not empty.
    assert_eq!(
        d.range,
        Range::new(Position::new(2, 8), Position::new(2, 17)),
        "{d:#?}"
    );
    for d in &diags {
        assert!(d.range.end > d.range.start, "an empty range: {d:#?}");
    }
}

#[test]
fn lsp_collect_the_iterator_quick_fix_still_inserts_after_it() {
    let uri: Uri = "file:///tmp/main.kara".parse().unwrap();
    let whole = Range::new(Position::new(0, 0), Position::new(5, 0));
    let actions = analysis::code_actions(SRC, &uri, whole);
    let edit = actions
        .iter()
        .filter_map(|a| match a {
            CodeActionOrCommand::CodeAction(a) => a.edit.as_ref(),
            _ => None,
        })
        .flat_map(|e| e.changes.iter().flat_map(|c| c.values().flatten()))
        .find(|e| e.new_text == ".collect()")
        .unwrap_or_else(|| panic!("no .collect() fix: {actions:#?}"));
    assert_eq!(
        edit.range,
        Range::new(Position::new(2, 17), Position::new(2, 17))
    );
}
