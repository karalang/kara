//! `karac doc` rows DC2-DC4 (probe tool2): a reference to an item with no
//! page is plain text, a trait method's parameter prose renders, and a
//! file that does not parse fails the run.

use super::{karac_bin, ScratchDir};
use std::fs;

fn doc_project(tag: &str, src: &str) -> (ScratchDir, std::process::Output) {
    let scratch = ScratchDir::new(tag);
    scratch.write(
        "kara.toml",
        "[package]\nname = \"rows\"\nedition = \"2026\"\n",
    );
    scratch.write("src/lib.kara", src);
    let out = karac_bin()
        .arg("doc")
        .current_dir(scratch.root())
        .output()
        .unwrap();
    (scratch, out)
}

/// DC2: `[helper_two]` names an item that exists but has no doc comment,
/// so it has no page: the reference is plain text, not a link to a page
/// that is never written.
#[test]
fn doc_reference_to_an_undocumented_item_is_plain_text() {
    let (scratch, out) = doc_project(
        "xref-undocumented",
        "/// A point.\npub struct Point { pub x: i64 }\n\
         /// See [Point] and [helper_two].\npub fn add(a: i64) -> i64 { a }\n\
         /// detached\n\npub fn helper_two() -> i64 { 2 }\n",
    );
    assert!(out.status.success(), "{out:?}");
    let doc = scratch.root().join("dist/doc");
    assert!(!doc.join("helper_two.html").exists());
    let html = fs::read_to_string(doc.join("add.html")).unwrap();
    assert!(html.contains("href=\"Point.html\""), "{html}");
    assert!(!html.contains("href=\"helper_two.html\""), "{html}");
    assert!(html.contains("helper_two"), "{html}");
}

/// DC3: a trait method's `///` on a parameter renders under the method on
/// the trait's page, as a free function's does on its own.
#[test]
fn doc_renders_trait_method_parameter_docs() {
    let (scratch, out) = doc_project(
        "trait-param-doc",
        "/// A trait.\npub trait Greet {\n    /// says hello\n    fn hello(self, /// who\n        name: Str) -> String;\n}\n",
    );
    assert!(out.status.success(), "{out:?}");
    let html = fs::read_to_string(scratch.root().join("dist/doc/Greet.html")).unwrap();
    assert!(html.contains("<dl class=\"params\">"), "{html}");
    assert!(
        html.contains("<dt>name: Str</dt>\n<dd><p>who</p>"),
        "{html}"
    );
}

/// DC4: a file that does not parse is reported and the docs for the rest
/// are written, but the run exits non-zero.
#[test]
fn doc_with_a_parse_error_exits_nonzero() {
    let (scratch, out) = doc_project(
        "parse-error",
        "/// Fine.\npub fn ok() -> i64 { 1 }\npub fn broken( {\n",
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("error[parse]"), "{stderr}");
    assert!(
        stderr.contains("error[doc]: the docs are incomplete"),
        "{stderr}"
    );
    drop(scratch);
}
