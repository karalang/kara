//! Hover and signature help show a function's declaration as written, the
//! same head `karac doc` renders (B-2026-11-24-6): `escaping`, `MutFn` and
//! `+ CrossTask` survive, and the effects line lists effects side by side.

use kara_lsp::analysis;
use lsp_types::{HoverContents, Position};

const SRC: &str = "pub fn reg(h: escaping Fn(i64) -> bool, m: MutFn(i64), f: own OnceFn() -> i64 + CrossTask) -> i64 { 1 }

pub fn greet(name: Str) { println(f\"hi {name}\"); }

fn main() {
    greet(\"x\");
    let r = reg(|x| x > 0, |x| {}, || 1);
}
";

const REG_HEAD: &str =
    "fn reg(h: escaping Fn(i64) -> bool, m: MutFn(i64), f: own OnceFn() -> i64 + CrossTask) -> i64";

fn pos(needle: &str, extra: u32) -> Position {
    for (i, l) in SRC.lines().enumerate() {
        if let Some(c) = l.find(needle) {
            return Position::new(i as u32, c as u32 + extra);
        }
    }
    panic!("{needle}")
}

fn hover_text(p: Position) -> String {
    match analysis::hover(SRC, p).expect("a hover").contents {
        HoverContents::Markup(m) => m.value,
        other => panic!("expected markup, got {other:?}"),
    }
}

#[test]
fn hover_on_a_call_shows_the_declaration_as_written() {
    let h = hover_text(pos("= reg(", 3));
    assert!(h.contains(REG_HEAD), "{h}");
}

#[test]
fn hover_on_a_declarations_name_shows_it_as_written() {
    let h = hover_text(pos("fn reg", 4));
    assert!(h.contains(REG_HEAD), "{h}");
}

#[test]
fn hover_effects_are_listed_side_by_side() {
    let h = hover_text(pos("greet(\"", 1));
    assert!(h.contains("fn greet(name: Str)"), "{h}");
    let line = h
        .lines()
        .find(|l| l.contains("Resource:"))
        .unwrap_or_else(|| panic!("no Resource line: {h}"));
    assert!(line.contains("writes(Stdout)"), "{h}");
    assert!(!line.contains(" + "), "effects joined with `+`: {h}");
}

#[test]
fn signature_help_shows_the_declaration_as_written() {
    let s = analysis::signature_help(SRC, pos("reg(|", 5)).expect("signature help");
    assert_eq!(s.signatures[0].label, REG_HEAD);
}
