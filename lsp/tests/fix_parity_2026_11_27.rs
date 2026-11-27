//! The editor's quick fix makes the change `karac fix` makes (B-2026-11-27-63):
//! a typecheck fix that `karac fix` applies with a companion edit carries that
//! edit in the same code action. Found by scripts/fix-parity.py.

use std::str::FromStr;

use kara_lsp::analysis::{code_actions, diagnostics_for, LineIndex};
use lsp_types::{CodeActionOrCommand, Position, Range, TextEdit, Uri};

fn apply(text: &str, edits: &[TextEdit]) -> String {
    let index = LineIndex::new(text);
    let mut e: Vec<(usize, usize, &str)> = edits
        .iter()
        .map(|x| {
            (
                index.offset(x.range.start),
                index.offset(x.range.end),
                x.new_text.as_str(),
            )
        })
        .collect();
    e.sort_by_key(|x| std::cmp::Reverse(x.0));
    let mut out = text.to_string();
    for (a, b, t) in e {
        out.replace_range(a..b, t);
    }
    out
}

/// The one quick fix offered on `text`, applied.
#[allow(clippy::mutable_key_type)] // `WorkspaceEdit.changes` keys on `Uri`
fn the_quick_fix(text: &str) -> String {
    let uri = Uri::from_str("file:///parity.kara").unwrap();
    let actions = code_actions(
        text,
        &uri,
        Range::new(Position::new(0, 0), Position::new(999, 0)),
    );
    let fixes: Vec<_> = actions
        .into_iter()
        .filter_map(|a| match a {
            CodeActionOrCommand::CodeAction(a) if a.is_preferred == Some(true) => Some(a),
            _ => None,
        })
        .collect();
    assert_eq!(fixes.len(), 1, "{fixes:#?}");
    let edit = fixes[0].edit.clone().unwrap();
    let edits = edit.changes.unwrap().into_values().next().unwrap();
    apply(text, &edits)
}

/// `own` on a trait impl method's parameter goes on the trait's too, in one
/// action: alone, it and E0288's undo alternated (B-2026-11-18-66).
#[test]
fn the_trait_own_quick_fix_writes_both_parameters() {
    let src = "trait Wrapper { fn dup[A](ref self, x: A) -> A; }\n\
               struct C { v: i64 }\n\
               impl Wrapper for C { fn dup[A](ref self, x: A) -> A { x } }\n\
               fn main() { let c = C { v: 0 }; println(f\"{c.dup(99)}\"); }\n";
    let fixed = the_quick_fix(src);
    assert!(
        fixed.contains("trait Wrapper { fn dup[A](ref self, x: own A) -> A; }"),
        "{fixed}"
    );
    assert!(
        fixed.contains("fn dup[A](ref self, x: own A) -> A { x }"),
        "{fixed}"
    );
    let uri = Uri::from_str("file:///parity.kara").unwrap();
    assert!(diagnostics_for(&fixed, &uri).is_empty(), "{fixed}");
}

/// A literal binding's annotation comes with the second one its error's
/// repair names (`res` and `p`), as `karac fix` writes both.
#[test]
fn the_literal_annotation_quick_fix_writes_each_binding() {
    let src = "fn count(d: i32, n: i32) -> i32 {\n    let mut res = 0;\n    let mut p = 1;\n    \
               while p <= n {\n        let (a, b) = (n / p, n % p);\n        \
               res += (a + 9 - d) / 10 * p;\n        if a % 10 == d {\n            res += b + 1;\n        }\n        \
               p *= 10;\n    }\n    res\n}\n\nfn main() {\n    println(count(1, 13));\n}\n";
    let fixed = the_quick_fix(src);
    assert!(fixed.contains("let mut res: i32 = 0;"), "{fixed}");
    assert!(fixed.contains("let mut p: i32 = 1;"), "{fixed}");
    let uri = Uri::from_str("file:///parity.kara").unwrap();
    assert!(diagnostics_for(&fixed, &uri).is_empty(), "{fixed}");
}
