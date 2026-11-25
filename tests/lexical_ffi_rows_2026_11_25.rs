//! Rows found by the sentence-map audits of design.md §3 (Lexical
//! structure) and §15 (Unsafe, FFI and layout control), 2026-11-25. Each
//! test runs the cell its row cites, from `conformance/design/lexical/` or
//! `conformance/design/ffi/`, against the expectation written from the page.

use std::path::{Path, PathBuf};
use std::process::Command;

fn cell(dir: &str, name: &str) -> (String, String) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("conformance/design")
        .join(dir);
    let src = std::fs::read_to_string(root.join(format!("{name}.kara"))).unwrap();
    let want = std::fs::read_to_string(root.join(format!("{name}.out"))).unwrap_or_default();
    (src, want)
}

fn karac(tag: &str, args: &[&str], src: &str) -> (bool, String, String) {
    let dir: PathBuf = std::env::temp_dir().join(format!(
        "karac-lex-ffi-{tag}-{}-{}",
        args[0].trim_start_matches('_'),
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("main.kara"), src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg("main.kara")
        .current_dir(&dir)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// The cell prints its `.out` on the interpreter and, with the backend,
/// compiled.
fn runs(dir: &str, name: &str) {
    let (src, want) = cell(dir, name);
    let mut modes = vec!["__mir-run"];
    if cfg!(feature = "llvm") {
        modes.push("__mir-llvm-run");
    }
    for m in modes {
        let (ok, out, err) = karac(name, &[m], &src);
        assert!(ok, "{name} {m}: {err}");
        assert_eq!(out, want, "{name} {m}");
    }
}

/// `karac check` output of the cell (stdout and stderr).
fn check(dir: &str, name: &str) -> String {
    let (src, _) = cell(dir, name);
    let (_, out, err) = karac(name, &["check"], &src);
    out + &err
}

/// B-2026-11-25-3: `r#true` names a binding; bare `true` stays the literal.
#[test]
fn raw_true_binding_leaves_the_literal() {
    runs("lexical", "raw_ident_true_false_literals_still_literals");
}

/// B-2026-11-25-4: `const n` is refused, and the help names `N` (CN-7).
#[test]
fn one_letter_const_help_names_the_capital() {
    let out = check("lexical", "cn7_one_letter_lowercase_const");
    assert!(out.contains("consider renaming to `N`"), "{out}");
    assert!(
        !out.contains("no capitalization change can make it"),
        "{out}"
    );
}

/// B-2026-11-25-5: the byte-literal `\u{...}` error is the page's sentence.
#[test]
fn byte_unicode_escape_error_is_the_pages_sentence() {
    let out = check("lexical", "byte_unicode_escape_full_message");
    assert!(
        out.contains(
            "Unicode escapes are not permitted in byte literals; use \\xFF for byte 0xFF."
        ),
        "{out}"
    );
}

/// B-2026-11-25-6: `String.from_utf8` takes a `b"..."` array by value.
#[test]
fn from_utf8_takes_a_byte_string_by_value() {
    runs("lexical", "byte_string_from_utf8");
}

/// B-2026-11-25-7: a `c"..."` literal's NUL follows its bytes.
#[test]
fn c_string_literal_ends_in_a_nul() {
    runs("lexical", "c_string_nul_terminated");
}
