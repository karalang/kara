//! The prelude shrink (B-2026-11-26-96) made `Mutex`, `OnceLock`,
//! `TaskGroup`, `File` and the rest name-only-through-an-import, so an editor
//! meets E0520 often. Pinned here, end to end through kara-lsp: the E0520
//! import fix-it is a quick fix in the document that uses the name (a lone
//! file, a package module, a `_test.kara` companion) and applying it leaves a
//! document that checks clean; completion of such a name adds its import on
//! accept; hover and go-to-definition answer on an imported name.

use std::str::FromStr;

use kara_lsp::analysis::{
    code_actions, completion_in, definition_site_in, diagnostics_for, hover_in,
};
use lsp_types::{CodeActionOrCommand, Position, Range, TextEdit, Uri};

fn plain(path: std::path::PathBuf) -> std::path::PathBuf {
    match path.to_str().and_then(|s| s.strip_prefix(r"\\?\")) {
        Some(rest) => std::path::PathBuf::from(rest),
        None => path,
    }
}

fn uri_of(path: &std::path::Path) -> Uri {
    let mut s = path.display().to_string().replace('\\', "/");
    if !s.starts_with('/') {
        s.insert(0, '/');
    }
    Uri::from_str(&format!("file://{}", s.replace(' ', "%20"))).unwrap()
}

const MAIN: &str = "import util;\n\nfn main() {\n    println(f\"{util.twice(2)}\");\n}\n";
const UTIL: &str = "// The helpers.\n\npub fn twice(n: i64) -> i64 with blocks {\n    let m = Mutex.new(n);\n    let v = m.lock().value;\n    v * 2\n}\n";
const TEST: &str = "import util;\n\ntest \"twice doubles\" {\n    let g = TaskGroup.new();\n    assert_eq(util.twice(3), 6);\n}\n";

fn package(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("kara-lsp-prelude-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("kara.toml"), "[package]\nname = \"app\"\n").unwrap();
    std::fs::write(dir.join("src/main.kara"), MAIN).unwrap();
    std::fs::write(dir.join("src/util.kara"), UTIL).unwrap();
    std::fs::write(dir.join("src/main_test.kara"), TEST).unwrap();
    plain(std::fs::canonicalize(dir).unwrap())
}

fn full() -> Range {
    Range::new(Position::new(0, 0), Position::new(999, 0))
}

/// Byte offset of an LSP position (ASCII sources only).
fn offset(text: &str, p: Position) -> usize {
    let mut off = 0;
    for (i, line) in text.split_inclusive('\n').enumerate() {
        if i as u32 == p.line {
            return off + p.character as usize;
        }
        off += line.len();
    }
    off
}

fn apply(text: &str, edits: &[TextEdit]) -> String {
    let mut edits = edits.to_vec();
    edits.sort_by_key(|e| std::cmp::Reverse(offset(text, e.range.start)));
    let mut out = text.to_string();
    for e in edits {
        let (a, b) = (offset(text, e.range.start), offset(text, e.range.end));
        out.replace_range(a..b, &e.new_text);
    }
    out
}

/// The E0520 quick fix on `text` at `uri`: its edits, which must all be in
/// `uri`'s own document.
// `WorkspaceEdit.changes` keys on `Uri` (see `code_actions`).
#[allow(clippy::mutable_key_type)]
fn import_fix(text: &str, uri: &Uri) -> Vec<TextEdit> {
    let diags = diagnostics_for(text, uri);
    assert!(
        diags
            .iter()
            .any(|d| format!("{:?}", d.code).contains("E0520")),
        "{diags:#?}"
    );
    let actions = code_actions(text, uri, full());
    let fix = actions
        .iter()
        .find_map(|a| match a {
            CodeActionOrCommand::CodeAction(a)
                if a.diagnostics
                    .iter()
                    .flatten()
                    .any(|d| format!("{:?}", d.code).contains("E0520")) =>
            {
                Some(a.clone())
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("no E0520 quick fix: {actions:#?}"));
    let changes = fix.edit.unwrap().changes.unwrap();
    assert_eq!(changes.len(), 1, "{changes:#?}");
    changes.get(uri).cloned().expect("edit names this document")
}

#[test]
fn the_import_quick_fix_lands_in_a_lone_file_and_checks_clean() {
    let dir = std::env::temp_dir().join(format!("kara-lsp-prelude-lone-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = plain(std::fs::canonicalize(&dir).unwrap()).join("lone.kara");
    let text = "// A counter.\n\nfn main() {\n    let m = Mutex.new(1);\n    println(f\"{m.lock().value}\");\n}\n";
    std::fs::write(&path, text).unwrap();
    let uri = uri_of(&path);
    let fixed = apply(text, &import_fix(text, &uri));
    assert!(fixed.contains("import std.sync.{Mutex};\n"), "{fixed}");
    assert!(fixed.starts_with("// A counter."), "{fixed}");
    let after = diagnostics_for(&fixed, &uri);
    assert!(after.is_empty(), "{fixed}\n{after:#?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_import_quick_fix_lands_in_a_package_module_and_checks_clean() {
    let dir = package("module");
    let path = dir.join("src/util.kara");
    let uri = uri_of(&path);
    let fixed = apply(UTIL, &import_fix(UTIL, &uri));
    assert!(fixed.contains("import std.sync.{Mutex};\n"), "{fixed}");
    assert!(fixed.starts_with("// The helpers."), "{fixed}");
    let after = diagnostics_for(&fixed, &uri);
    assert!(after.is_empty(), "{fixed}\n{after:#?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_import_quick_fix_lands_in_a_test_companion_and_checks_clean() {
    let dir = package("testfile");
    // The module is already fixed; only the test file lacks an import.
    std::fs::write(
        dir.join("src/util.kara"),
        format!("import std.sync.Mutex;\n{UTIL}"),
    )
    .unwrap();
    let path = dir.join("src/main_test.kara");
    let uri = uri_of(&path);
    let fixed = apply(TEST, &import_fix(TEST, &uri));
    assert!(fixed.contains("import std.sync.{TaskGroup};\n"), "{fixed}");
    assert!(fixed.starts_with("import util;"), "{fixed}");
    let after = diagnostics_for(&fixed, &uri);
    assert!(after.is_empty(), "{fixed}\n{after:#?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn hover_and_definition_answer_on_an_imported_library_name() {
    let dir = std::env::temp_dir().join(format!("kara-lsp-prelude-nav-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = plain(std::fs::canonicalize(&dir).unwrap()).join("nav.kara");
    let text = "import std.sync.Mutex;\n\nfn main() {\n    let m = Mutex.new(1);\n    println(f\"{m.lock().value}\");\n}\n";
    std::fs::write(&path, text).unwrap();
    // `Mutex` in `Mutex.new(1)`, and in the import itself.
    for at in [Position::new(3, 13), Position::new(0, 17)] {
        let hover = hover_in(text, Some(&path), at);
        assert!(
            format!("{hover:?}").contains("Mutex"),
            "hover at {at:?}: {hover:?}"
        );
        let def = definition_site_in(text, Some(&path), at);
        let (uri, _) = def.unwrap_or_else(|| panic!("no definition at {at:?}"));
        let uri = uri.expect("a library definition names its file");
        assert!(uri.as_str().ends_with("mutex.kara"), "{uri:?}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn completion_of_a_non_prelude_name_adds_its_import() {
    let dir = std::env::temp_dir().join(format!("kara-lsp-prelude-cmp-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = plain(std::fs::canonicalize(&dir).unwrap()).join("cmp.kara");
    let text = "// Counter.\n\nfn main() {\n    let m = Mute\n}\n";
    std::fs::write(&path, text).unwrap();
    let items = completion_in(text, Some(&path), Position::new(3, 16));
    let item = items
        .iter()
        .find(|i| i.label == "Mutex")
        .unwrap_or_else(|| {
            panic!(
                "no Mutex: {:?}",
                items.iter().map(|i| &i.label).collect::<Vec<_>>()
            )
        });
    let extra = item.additional_text_edits.clone().unwrap_or_default();
    let fixed = apply(text, &extra);
    assert!(fixed.contains("import std.sync.{Mutex};\n"), "{fixed}");
    assert!(fixed.starts_with("// Counter."), "{fixed}");
    // Accepted, the import is in place and the completed program checks.
    let done = fixed.replace(
        "Mute\n",
        "Mutex.new(1);\n    println(f\"{m.lock().value}\");\n",
    );
    let after = diagnostics_for(&done, &uri_of(&path));
    assert!(after.is_empty(), "{done}\n{after:#?}");

    // Imported already: no second import. A prelude type: none needed.
    let text = "import std.sync.Mutex;\n\nfn main() {\n    let m = Mute\n}\n";
    let items = completion_in(text, Some(&path), Position::new(3, 16));
    let item = items.iter().find(|i| i.label == "Mutex").expect("Mutex");
    assert_eq!(item.additional_text_edits, None, "{item:?}");
    let text = "fn main() {\n    let v: Vec[i64] = Ve\n}\n";
    let items = completion_in(text, Some(&path), Position::new(1, 24));
    let item = items.iter().find(|i| i.label == "Vec").expect("Vec");
    assert_eq!(item.additional_text_edits, None, "{item:?}");
    // A program's own type of a library name is its own, with no import.
    let text = "struct Mutex { n: i64 }\n\nfn main() {\n    let m = Mute\n}\n";
    let items = completion_in(text, Some(&path), Position::new(3, 16));
    let item = items.iter().find(|i| i.label == "Mutex").expect("Mutex");
    assert_eq!(item.additional_text_edits, None, "{item:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A test companion's own errors are reported in it, on their line: it
/// showed none before (it was checked without the companions).
#[test]
fn a_test_companion_reports_its_own_errors() {
    let dir = package("testerr");
    std::fs::write(
        dir.join("src/util.kara"),
        format!("import std.sync.Mutex;\n{UTIL}"),
    )
    .unwrap();
    let uri = uri_of(&dir.join("src/main_test.kara"));
    let typed = "import util;\n\ntest \"t\" {\n    let s: String = util.twice(1);\n}\n";
    let diags = diagnostics_for(typed, &uri);
    assert_eq!(diags.len(), 1, "{diags:#?}");
    assert_eq!(diags[0].range.start.line, 3, "{diags:#?}");
    let unknown = "import util;\n\ntest \"t\" {\n    assert_eq(util.thrice(1), 3);\n}\n";
    let diags = diagnostics_for(unknown, &uri);
    assert!(!diags.is_empty(), "no error for util.thrice");
    assert!(diags.iter().all(|d| d.range.start.line == 3), "{diags:#?}");
    let clean = "import util;\n\ntest \"t\" {\n    assert_eq(util.twice(1), 2);\n}\n";
    let diags = diagnostics_for(clean, &uri);
    assert!(diags.is_empty(), "{diags:#?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A resolve error in a module after the first is placed in that module's
/// own text: it was placed at its offset in the package tree, past the
/// modules before it.
#[test]
fn a_resolve_error_in_a_later_module_is_placed_in_it() {
    let dir = package("resolve");
    let uri = uri_of(&dir.join("src/util.kara"));
    let text = "pub fn twice(n: i64) -> i64 {\n    n * nope\n}\n";
    let diags = diagnostics_for(text, &uri);
    assert_eq!(diags.len(), 1, "{diags:#?}");
    let r = diags[0].range;
    assert_eq!((r.start.line, r.start.character), (1, 8), "{diags:#?}");
    assert_eq!((r.end.line, r.end.character), (1, 12), "{diags:#?}");
    let _ = std::fs::remove_dir_all(&dir);
}
