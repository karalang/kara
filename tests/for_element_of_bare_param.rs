//! A bare `for` over a bare parameter, `fn sort(hs: Vec[H]) -> Vec[H] { for
//! h in hs { out.push(h) } }`: the parameter borrows (core-semantics.md
//! §4.1) and so does the loop (§4.6), so the element cannot be moved out.
//! The refusal names both and fixes both at once, `hs: own Vec[H]` and
//! `hs.into_iter()`; it was a bare `expected 'H', found 'ref H'` with no
//! fix, which left the Mend task `canonical_request` short of a clean
//! build (B-2026-11-20-136).

use std::path::Path;
use std::process::{Command, Stdio};

fn karac(dir: &Path, args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// The first check's diagnostics, the text one `karac fix` leaves, whether
/// that text checks clean, and what it prints under the MIR interpreter.
fn mend(name: &str, src: &str) -> (String, String, bool, String) {
    let dir = std::env::temp_dir().join(format!(
        "karac-for-param-element-{name}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("cell.kara");
    std::fs::write(&file, format!("{src}\n")).unwrap();
    let first = karac(&dir, &["check", "cell.kara"]).2;
    karac(&dir, &["fix", "cell.kara"]);
    let text = std::fs::read_to_string(&file).unwrap();
    let (clean, _, _) = karac(&dir, &["check", "cell.kara"]);
    let (_, out, _) = karac(&dir, &["__mir-run", "cell.kara"]);
    let _ = std::fs::remove_dir_all(&dir);
    (first, text, clean, out)
}

const HEADER: &str = "struct H {\n    name: String,\n}\n\n";

#[test]
fn an_element_of_a_bare_parameter_moved_out_takes_own_and_into_iter() {
    let src = format!(
        "{HEADER}fn keep(hs: Vec[H]) -> Vec[H] {{\n    let mut out: Vec[H] = Vec.new();\n    for h in hs {{\n        out.insert(0, h);\n    }}\n    out\n}}\n\nfn main() {{\n    let v = [H {{ name: \"a\".to_string() }}, H {{ name: \"b\".to_string() }}];\n    for h in keep(v) {{\n        println(h.name);\n    }}\n}}"
    );
    let (first, text, clean, out) = mend("private", &src);
    assert!(
        first.contains("`h` is an element of `hs`, which a bare `for` borrows"),
        "{first}"
    );
    assert!(
        first.contains("Take `hs` by value with `own` and iterate `hs.into_iter()`"),
        "{first}"
    );
    assert!(text.contains("fn keep(hs: own Vec[H])"), "{text}");
    assert!(text.contains("for h in hs.into_iter() {"), "{text}");
    assert!(!text.contains("derive(Clone)"), "{text}");
    assert!(clean, "{text}");
    assert_eq!(out, "b\na\n");
}

/// A caller that hands on a bare parameter of its own to the function that
/// now takes `own` gets that parameter's `own` in the same `karac fix` run,
/// the shape of `canonical_request`'s `canonical_query`.
#[test]
fn a_caller_handing_on_its_own_parameter_takes_own_in_the_same_run() {
    let src = format!(
        "{HEADER}fn keep(hs: Vec[H]) -> Vec[H] {{\n    let mut out: Vec[H] = Vec.new();\n    for h in hs {{\n        out.push(h);\n    }}\n    out\n}}\n\nfn count(hs: Vec[H]) -> i64 {{\n    keep(hs).len()\n}}\n\nfn main() {{\n    println(count([H {{ name: \"a\".to_string() }}]));\n}}"
    );
    let (_, text, clean, out) = mend("caller", &src);
    assert!(text.contains("fn keep(hs: own Vec[H])"), "{text}");
    assert!(text.contains("fn count(hs: own Vec[H])"), "{text}");
    assert!(clean, "{text}");
    assert_eq!(out, "1\n");
}

/// On a library's `pub fn` the signature stays: `own` is only a note, and
/// nothing is rewritten.
#[test]
fn a_library_pub_fn_element_names_own_only_as_a_note() {
    let src = "pub struct H {\n    name: String,\n}\n\npub fn keep(hs: Vec[H]) -> Vec[H] {\n    let mut out: Vec[H] = Vec.new();\n    for h in hs {\n        out.push(h);\n    }\n    out\n}";
    let (first, text, clean, _) = mend("library-pub", src);
    assert!(first.contains("this is a `pub fn` of a library"), "{first}");
    assert!(text.contains("pub fn keep(hs: Vec[H])"), "{text}");
    assert!(text.contains("for h in hs {"), "{text}");
    assert!(!clean, "{text}");
}
