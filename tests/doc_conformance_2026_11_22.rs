//! `karac doc` against design.md § Doc comments > Rendering
//! (B-2026-11-24-1): signatures render as written, a library's docs show
//! only its public surface, a `pub import` lists the item at the shorter
//! path, a page puts the doc text before its sections, and the output is
//! the same across runs and hash seeds.

use std::path::{Path, PathBuf};
use std::process::Command;

fn build_pkg(tag: &str, files: &[(&str, &str)], seed: Option<&str>) -> PathBuf {
    let root = std::env::temp_dir().join(format!("karac-doc-conf-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    for (rel, src) in files {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, src).unwrap();
    }
    std::fs::write(
        root.join("kara.toml"),
        "[package]\nname = \"probe\"\nversion = \"0.1.0\"\nedition = \"2026\"\n",
    )
    .unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_karac"));
    cmd.arg("doc").current_dir(&root);
    if let Some(seed) = seed {
        cmd.env("KARAC_HASH_SEED", seed);
    }
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "karac doc failed: {}{}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    root
}

fn read(root: &Path, rel: &str) -> String {
    std::fs::read_to_string(root.join("dist/doc").join(rel))
        .unwrap_or_else(|e| panic!("{rel}: {e}"))
}

fn exists(root: &Path, rel: &str) -> bool {
    root.join("dist/doc").join(rel).exists()
}

/// The text of the page's `<pre class="signature">`, unescaped.
fn signature(page: &str) -> String {
    let start = page
        .find("<pre class=\"signature\">")
        .expect("signature block")
        + "<pre class=\"signature\">".len();
    let end = start + page[start..].find("</pre>").unwrap();
    page[start..end]
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&amp;", "&")
}

const SIG_LIB: &str = "\
/// Registers callbacks.
pub fn reg(h: escaping Fn(i64) -> bool, m: MutFn(i64), f: own OnceFn() -> i64 + CrossTask) -> i64 { 1 }

/// A C-ABI export.
#[unsafe(no_mangle)]
pub extern \"C\" fn kara_add(a: i32, b: i32) -> i32 { a + b }

unsafe extern \"C\" {
    /// C abs.
    pub fn abs(x: i32) -> i32;
    /// C strlen.
    pub pure fn strlen(s: CStr) -> i64;
}

/// Says hi.
pub fn greet(name: Str) { println(f\"hi {name}\"); println(f\"{[1, 2].len()}\"); }
";

#[test]
fn doc_conf_callable_types_render_as_written() {
    let root = build_pkg("callable", &[("src/lib.kara", SIG_LIB)], None);
    let sig = signature(&read(&root, "reg.html"));
    assert!(sig.contains("h: escaping Fn(i64) -> bool"), "{sig}");
    assert!(sig.contains("m: MutFn(i64)"), "{sig}");
    assert!(sig.contains("f: own OnceFn() -> i64 + CrossTask"), "{sig}");
    assert!(sig.starts_with("pub fn reg"), "{sig}");
}

#[test]
fn doc_conf_ffi_signatures_render_as_written() {
    let root = build_pkg("ffi", &[("src/lib.kara", SIG_LIB)], None);
    let export = signature(&read(&root, "kara_add.html"));
    assert_eq!(
        export, "pub extern \"C\" fn kara_add(a: i32, b: i32) -> i32",
        "an FFI export keeps its visibility and ABI"
    );
    let import = signature(&read(&root, "abs.html"));
    assert_eq!(import, "pub extern \"C\" fn abs(x: i32) -> i32");
    let pure_import = signature(&read(&root, "strlen.html"));
    assert_eq!(
        pure_import,
        "pub extern \"C\" pure fn strlen(s: CStr) -> i64"
    );
}

#[test]
fn doc_conf_inferred_effects_are_written_side_by_side() {
    let root = build_pkg("effects", &[("src/lib.kara", SIG_LIB)], None);
    let sig = signature(&read(&root, "greet.html"));
    assert!(sig.contains(" with "), "an inferred clause is shown: {sig}");
    assert!(
        !sig.contains(" + "),
        "effects are written side by side, not joined with `+`: {sig}"
    );
}

const VIS_LIB: &str = "\
import util.shapes.{Shape};
pub import util.shapes.Circle;
pub import util.shapes.{Square as Block};

/// A stack.
pub struct Stack {
    /// The items.
    pub items: Vec[i64],
    /// Internal count.
    count: i64,
}

/// Not public.
fn hidden() -> i64 { 1 }

/// Package-internal struct.
struct Secret { x: i64 }

/// Same-directory struct.
private struct Local { x: i64 }
";

const SHAPES: &str = "\
//! Shapes.

/// A circle.
pub struct Circle { pub r: f64 }

/// A square.
pub struct Square { pub s: f64 }

/// A shape.
pub trait Shape {
    /// Area.
    fn area(self) -> f64;
}
";

#[test]
fn doc_conf_library_shows_only_its_public_surface() {
    let root = build_pkg(
        "vis-lib",
        &[("src/lib.kara", VIS_LIB), ("src/util/shapes.kara", SHAPES)],
        None,
    );
    for private in ["hidden.html", "Secret.html", "Local.html"] {
        assert!(
            !exists(&root, private),
            "{private} leaked into a library's docs"
        );
    }
    let index = read(&root, "index.html");
    assert!(!index.contains("hidden"), "index lists a private item");
    assert!(!index.contains("Secret"), "index lists a private item");
    let search = read(&root, "search-index.json");
    assert!(
        !search.contains("\"hidden\""),
        "search index lists a private item"
    );
    let stack = read(&root, "Stack.html");
    assert!(stack.contains("items"), "a pub field is listed");
    assert!(!stack.contains("count"), "a private field is listed");
}

#[test]
fn doc_conf_executable_keeps_every_documented_item() {
    let main = format!("{VIS_LIB}\nfn main() {{ }}\n");
    let root = build_pkg(
        "vis-bin",
        &[("src/main.kara", &main), ("src/util/shapes.kara", SHAPES)],
        None,
    );
    assert!(exists(&root, "hidden.html"));
    assert!(exists(&root, "Secret.html"));
    assert!(read(&root, "Stack.html").contains("count"));
}

#[test]
fn doc_conf_reexport_shows_at_the_shorter_path() {
    let root = build_pkg(
        "reexport",
        &[("src/lib.kara", VIS_LIB), ("src/util/shapes.kara", SHAPES)],
        None,
    );
    let index = read(&root, "index.html");
    // The crate root's rows list the re-exported names, linking to the
    // pages in `util.shapes`.
    let row = |kind: &str, href: &str, name: &str| {
        format!(
            "<tr><td><code>(crate root)</code></td><td>{kind}</td><td><a href=\"{href}\">{name}</a></td></tr>"
        )
    };
    assert!(
        index.contains(&row("struct", "util/shapes/Circle.html", "Circle")),
        "Circle is not listed at the crate root: {index}"
    );
    assert!(
        index.contains(&row("struct", "util/shapes/Square.html", "Block")),
        "a renamed re-export is listed under its alias: {index}"
    );
    // A plain (non-pub) import re-exports nothing.
    assert!(
        !index.contains(&row("trait", "util/shapes/Shape.html", "Shape")),
        "a plain import was listed as a re-export"
    );
}

#[test]
fn doc_conf_doc_text_precedes_sections() {
    let root = build_pkg(
        "order",
        &[("src/lib.kara", VIS_LIB), ("src/util/shapes.kara", SHAPES)],
        None,
    );
    let page = read(&root, "Stack.html");
    let prose = page.find("A stack.").expect("doc text");
    let fields = page.find("<h2>Fields</h2>").expect("fields section");
    assert!(prose < fields, "doc text renders after the Fields section");
}

#[test]
fn doc_conf_output_is_deterministic_across_hash_seeds() {
    let files = [("src/lib.kara", VIS_LIB), ("src/util/shapes.kara", SHAPES)];
    let mut seen: Option<Vec<(PathBuf, Vec<u8>)>> = None;
    for seed in ["1", "2", "3"] {
        let root = build_pkg(&format!("seed{seed}"), &files, Some(seed));
        let dir = root.join("dist/doc");
        let mut pages = Vec::new();
        let mut stack = vec![dir.clone()];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else {
                    pages.push((
                        p.strip_prefix(&dir).unwrap().to_path_buf(),
                        std::fs::read(&p).unwrap(),
                    ));
                }
            }
        }
        pages.sort();
        assert!(pages.len() > 3, "too few pages: {}", pages.len());
        match &seen {
            None => seen = Some(pages),
            Some(prev) => assert!(prev == &pages, "seed {seed} changed the output"),
        }
    }
}
