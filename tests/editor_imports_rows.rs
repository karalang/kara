//! Editor position queries in a module that imports a sibling module
//! (B-2026-11-17-71): hover, definition, completion and signature help
//! answered null in any such file, since the buffer was checked alone and
//! its import named no module.

use std::path::PathBuf;

const UTIL: &str = "pub struct Point { pub x: i64, pub y: i64 }\n\
impl Point {\n    pub fn norm1(self) -> i64 { self.x.abs() + self.y.abs() }\n}\n\
pub fn helper(n: i64) -> i64 { n * 2 }\n";

const MAIN: &str = "import util.{Point, helper};\n\
fn main() {\n    let p = Point { x: 3, y: -4 };\n    let q = helper(p.norm1());\n    \
let s = \"hi\";\n    println(f\"{s} {q} {p.x}\");\n}\n";

fn package(name: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "karac-editor-imports-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("kara.toml"), "[package]\nname = \"demo\"\n").unwrap();
    std::fs::write(dir.join("src/util.kara"), UTIL).unwrap();
    let main = dir.join("src/main.kara");
    std::fs::write(&main, MAIN).unwrap();
    (dir, main)
}

fn at(text: &str, needle: &str) -> usize {
    text.find(needle)
        .unwrap_or_else(|| panic!("{needle:?} not in buffer"))
}

#[test]
fn hover_answers_in_a_module_that_imports_a_sibling() {
    let (dir, main) = package("hover");
    let ty = |needle: &str, skip: usize| {
        karac::hover_at_in(MAIN, at(MAIN, needle) + skip, &main)
            .unwrap_or_else(|| panic!("no hover on {needle:?}"))
            .type_display
    };
    assert_eq!(ty("let q", 4), "i64");
    assert_eq!(ty("let p", 4), "Point");
    assert_eq!(ty("let s", 4), "String");
    assert!(ty("norm1", 0).contains("fn norm1(self) -> i64"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn definition_of_an_imported_name_is_in_its_module() {
    let (dir, main) = package("def");
    let util = dir.join("src/util.kara");
    let def = |needle: &str, skip: usize| match karac::goto_definition_site_in(
        MAIN,
        at(MAIN, needle) + skip,
        &main,
    ) {
        Some(karac::DefSite::Module {
            file,
            span_offset,
            span_length,
        }) => {
            assert_eq!(
                std::fs::canonicalize(&file).unwrap(),
                std::fs::canonicalize(&util).unwrap()
            );
            UTIL[span_offset..span_offset + span_length].to_string()
        }
        other => panic!("{needle:?}: {other:?}"),
    };
    assert_eq!(def("norm1", 0), "norm1");
    assert_eq!(def("p.x", 2), "x");
    assert!(
        def("helper(p", 0).contains("helper"),
        "{}",
        def("helper(p", 0)
    );
    assert!(def("Point {", 0).contains("Point"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn completion_and_signature_help_answer_after_an_import() {
    let (dir, main) = package("complete");
    // `p.` just typed, inside the call.
    let typing = MAIN.replace("p.norm1()", "p.");
    std::fs::write(&main, &typing).unwrap();
    let off = at(&typing, "p.)") + 2;
    let labels: Vec<String> = karac::completions_at_in(&typing, off, &main)
        .into_iter()
        .map(|c| c.label)
        .collect();
    assert!(labels.iter().any(|l| l == "norm1"), "{labels:?}");
    assert!(labels.iter().any(|l| l == "x"), "{labels:?}");
    let sig = karac::signature_help_at_in(MAIN, at(MAIN, "helper(") + 7, &main)
        .expect("signature help inside helper(...)");
    assert!(sig.label.contains("helper(n: i64) -> i64"), "{}", sig.label);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_file_outside_any_package_is_answered_alone() {
    // The control: a lone file keeps the single-buffer answers.
    let dir =
        std::env::temp_dir().join(format!("karac-editor-imports-lone-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = "fn main() {\n    let q = 3;\n    println(q);\n}\n";
    let file = dir.join("lone.kara");
    std::fs::write(&file, src).unwrap();
    let h = karac::hover_at_in(src, at(src, "q ="), &file).expect("hover");
    assert_eq!(h.type_display, "i64");
    let _ = std::fs::remove_dir_all(&dir);
}
