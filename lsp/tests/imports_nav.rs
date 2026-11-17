//! B-2026-11-17-71: hover, definition, completion and signature help answered
//! null in any document importing a sibling module, since the document was
//! checked alone and its import named no module. Each is now answered with
//! the document checked as a module of its package; a definition in the
//! sibling module points into that file.

use kara_lsp::analysis::{completion_in, definition_site_in, hover_in, signature_help_in};
use lsp_types::{HoverContents, MarkedString, Position};

const UTIL: &str = "pub struct Point { pub x: i64, pub y: i64 }\n\
impl Point {\n    pub fn norm1(self) -> i64 { self.x.abs() + self.y.abs() }\n}\n\
pub fn helper(n: i64) -> i64 { n * 2 }\n";

const MAIN: &str = "import util.{Point, helper};\n\
fn main() {\n    let p = Point { x: 3, y: -4 };\n    let q = helper(p.norm1());\n    \
println(f\"{q} {p.x}\");\n}\n";

fn package(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("kara-lsp-imports-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("kara.toml"), "[package]\nname = \"app\"\n").unwrap();
    std::fs::write(dir.join("src/util.kara"), UTIL).unwrap();
    std::fs::write(dir.join("src/main.kara"), MAIN).unwrap();
    std::fs::canonicalize(dir).unwrap()
}

/// The position of the first `needle` in `text`, `skip` characters in (all
/// of the fixture is ASCII, so a byte is a UTF-16 unit).
fn pos(text: &str, needle: &str, skip: usize) -> Position {
    let at = text.find(needle).unwrap() + skip;
    let line = text[..at].matches('\n').count();
    let col = at - text[..at].rfind('\n').map_or(0, |i| i + 1);
    Position::new(line as u32, col as u32)
}

fn hover_text(h: lsp_types::Hover) -> String {
    match h.contents {
        HoverContents::Markup(m) => m.value,
        HoverContents::Scalar(MarkedString::String(s)) => s,
        other => format!("{other:?}"),
    }
}

#[test]
fn hover_and_definition_answer_in_a_module_importing_a_sibling() {
    let dir = package("nav");
    let main = dir.join("src/main.kara");
    let hover = hover_in(MAIN, Some(&main), pos(MAIN, "let q", 4)).expect("hover on q");
    assert!(hover_text(hover).contains("i64"));
    let (uri, range) =
        definition_site_in(MAIN, Some(&main), pos(MAIN, "norm1", 0)).expect("definition of norm1");
    let uri = uri.expect("in util.kara, not this document");
    assert!(uri.as_str().ends_with("/src/util.kara"), "{}", uri.as_str());
    // `pub fn norm1` is on util.kara's line 2 (0-based), at column 11.
    assert_eq!((range.start.line, range.start.character), (2, 11));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn completion_and_signature_help_answer_in_a_module_importing_a_sibling() {
    let dir = package("complete");
    let main = dir.join("src/main.kara");
    let typing = MAIN.replace("p.norm1()", "p.");
    let items = completion_in(&typing, Some(&main), pos(&typing, "p.)", 2));
    assert!(items.iter().any(|i| i.label == "norm1"), "{items:?}");
    let help = signature_help_in(MAIN, Some(&main), pos(MAIN, "helper(p", 7))
        .expect("signature help in helper(...)");
    assert!(help.signatures[0].label.contains("helper(n: i64)"));
    let _ = std::fs::remove_dir_all(&dir);
}
