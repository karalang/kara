//! B-2026-11-21-34: references and rename stayed in one document,
//! so renaming a `pub fn` (or a `pub` method) at its declaration left the
//! modules importing it naming the old one, and the package no longer
//! checked. Both now cover the package; a rename that would add an error
//! anywhere is refused, in a package or in a lone document.

use std::collections::HashMap;

use kara_lsp::workspace;
use lsp_types::{Position, TextEdit};

const UTIL: &str = "pub struct Point { pub x: i64, pub y: i64 }
impl Point {
    pub fn norm1(self) -> i64 { self.x.abs() + self.y.abs() }
}
pub fn helper(n: i64) -> i64 { n * 2 }
pub fn twice(n: i64) -> i64 { helper(helper(n)) }
";

const MAIN: &str = "import util.{Point, helper};
fn main() {
    let p = Point { x: 3, y: -4 };
    let q = helper(p.norm1());
    println(f\"{q} {p.x} é🎉 {helper(1)}\");
}
";

fn package(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("kara-lsp-ws-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("kara.toml"), "[package]\nname = \"app\"\n").unwrap();
    std::fs::write(dir.join("src/util.kara"), UTIL).unwrap();
    std::fs::write(dir.join("src/main.kara"), MAIN).unwrap();
    std::fs::canonicalize(dir).unwrap()
}

fn pos(text: &str, needle: &str) -> Position {
    let at = text.find(needle).unwrap();
    let line = text[..at].matches('\n').count();
    let start = text[..at].rfind('\n').map_or(0, |i| i + 1);
    Position::new(line as u32, text[start..at].encode_utf16().count() as u32)
}

fn apply(text: &str, edits: &[TextEdit]) -> String {
    let index = kara_lsp::analysis::LineIndex::new(text);
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
    spans.sort_by_key(|a| std::cmp::Reverse(a.0));
    let mut out = text.to_string();
    for (a, b, t) in spans {
        out.replace_range(a..b, t);
    }
    out
}

#[test]
fn conformance_references_cover_the_package() {
    let dir = package("refs");
    let docs = HashMap::new();
    let refs = workspace::references(
        &dir.join("src/util.kara"),
        &docs,
        pos(UTIL, "helper(n"),
        true,
    )
    .expect("a pub fn of a package module");
    let in_main = refs
        .iter()
        .filter(|l| l.uri.as_str().ends_with("/src/main.kara"))
        .count();
    let in_util = refs
        .iter()
        .filter(|l| l.uri.as_str().ends_with("/src/util.kara"))
        .count();
    // The import, `helper(p.norm1())` and `helper(1)`; the declaration and
    // `twice`'s two calls.
    assert_eq!((in_main, in_util), (3, 3), "{refs:#?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn conformance_rename_of_a_pub_item_edits_every_module_and_still_checks() {
    for (file, needle, new) in [
        ("util", "helper(n", "dbl"),
        ("main", "helper(p", "dbl"),
        ("util", "norm1", "manhattan"),
    ] {
        let dir = package(&format!("rn-{new}-{file}"));
        let path = dir.join(format!("src/{file}.kara"));
        let text = if file == "util" { UTIL } else { MAIN };
        // The editor's buffer is what is renamed, not the file on disk.
        let mut docs = HashMap::new();
        docs.insert(
            kara_lsp::analysis::path_uri(&path).unwrap().to_string(),
            text.to_string(),
        );
        #[allow(clippy::mutable_key_type)]
        let changes = workspace::rename(&path, &docs, pos(text, needle), new)
            .expect("allowed")
            .expect("a pub item");
        assert_eq!(changes.len(), 2, "{needle}: both modules: {changes:#?}");
        for (uri, edits) in &changes {
            let p = kara_lsp::analysis::file_path(uri).unwrap();
            let t = std::fs::read_to_string(&p).unwrap();
            std::fs::write(&p, apply(&t, edits)).unwrap();
        }
        for f in ["util", "main"] {
            let p = dir.join(format!("src/{f}.kara"));
            let t = std::fs::read_to_string(&p).unwrap();
            let errs: Vec<_> = karac::check_source_at(&t, &p)
                .into_iter()
                .filter(|d| d.severity == "error")
                .collect();
            assert!(errs.is_empty(), "{needle} -> {new}: {f}: {errs:#?}\n{t}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn conformance_rename_that_would_break_is_refused() {
    let dir = package("refuse");
    let docs = HashMap::new();
    // `twice` is taken in util.
    let r = workspace::rename(
        &dir.join("src/util.kara"),
        &docs,
        pos(UTIL, "helper(n"),
        "twice",
    );
    let why = r.expect_err("a rename onto a name in use is refused");
    assert!(why.contains("failing `karac check`"), "{why}");
    // A lone document: a local renamed onto another local's name.
    let text = "fn main() {\n    let a = 1;\n    let b = \"s\";\n    println(a + 1);\n    println(b);\n}\n";
    let edits = kara_lsp::analysis::rename(text, pos(text, "a = 1"), "b")
        .unwrap()
        .unwrap();
    let why = workspace::checked_single_file(text, None, &edits, "b").expect_err("refused");
    assert!(why.contains("failing `karac check`"), "{why}");
    let _ = std::fs::remove_dir_all(&dir);
}
