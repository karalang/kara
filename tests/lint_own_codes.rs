//! B-2026-11-16-299 / -300: every typecheck lint has its own code.
//!
//! A lint used to report a code it shared with unrelated diagnostics:
//! `redundant_clone`, `unused_mut`, `needless_return` and the rest all said
//! `W0295`, the cast lints `W0291`, the drop-copy lints `W0299`. Each lint is
//! now one number in the `W03xx` band, the same lint under `-D` is the
//! matching `E03xx`, and the shared code moves to `parent_code` so a tool
//! keyed on it keeps working (review-thread ruling 2026-11-17). `karac explain`
//! takes the lint name, the new code, or the escalated one; its `--help` names
//! every catalogued band (-300).

use std::path::PathBuf;
use std::process::{Command, Output};

fn karac(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .output()
        .expect("karac")
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

const CLONE_SRC: &str = "fn show(s: String) {\n    println(s);\n}\n\n\
fn main() {\n    let s = \"hi\".to_string();\n    show(s.clone());\n}\n";

fn scratch(tag: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-lintcode-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, src).unwrap();
    f
}

/// The JSON records' `(severity, code, parent_code, lint_name)`.
fn records(json: &str) -> Vec<(String, String, String, String)> {
    let field = |rec: &str, key: &str| -> String {
        let pat = format!("\"{key}\":\"");
        rec.find(&pat)
            .map(|i| {
                let rest = &rec[i + pat.len()..];
                rest[..rest.find('"').unwrap()].to_string()
            })
            .unwrap_or_default()
    };
    json.split("{\"id\":")
        .skip(1)
        .map(|rec| {
            (
                field(rec, "severity"),
                field(rec, "code"),
                field(rec, "parent_code"),
                field(rec, "lint_name"),
            )
        })
        .collect()
}

#[test]
fn a_lint_warning_reports_its_own_code_with_the_shared_one_as_parent() {
    let f = scratch("warn", CLONE_SRC);
    let p = f.to_str().unwrap();
    let out = karac(&["check", "--output=json", p]);
    let _ = std::fs::remove_dir_all(f.parent().unwrap());
    let json = text(&out.stdout);
    // Measured before the fix: `"code":"W0295"` with no `parent_code`.
    assert_eq!(
        records(&json),
        vec![(
            "warning".to_string(),
            "W0300".to_string(),
            "W0295".to_string(),
            "redundant_clone".to_string()
        )],
        "{json}"
    );
}

#[test]
fn a_denied_lint_is_the_matching_e03_code_not_the_shared_e029x() {
    let f = scratch("deny", CLONE_SRC);
    let p = f.to_str().unwrap();
    let out = karac(&["check", "--output=json", "-D", "redundant_clone", p]);
    let _ = std::fs::remove_dir_all(f.parent().unwrap());
    let json = text(&out.stdout);
    assert_eq!(out.status.code(), Some(1), "{json}");
    assert_eq!(
        records(&json),
        vec![(
            "error".to_string(),
            "E0300".to_string(),
            "E0295".to_string(),
            "redundant_clone".to_string()
        )],
        "{json}"
    );
}

#[test]
fn explain_takes_a_lint_name_and_its_own_codes() {
    let by_name = karac(&["explain", "redundant_clone"]);
    let by_code = karac(&["explain", "W0300"]);
    let by_error = karac(&["explain", "E0300"]);
    for (what, out) in [
        ("name", &by_name),
        ("W0300", &by_code),
        ("E0300", &by_error),
    ] {
        let s = text(&out.stdout);
        assert!(out.status.success(), "{what}: {s}{}", text(&out.stderr));
        assert!(s.contains("redundant_clone"), "{what}: {s}");
        assert!(s.contains("W0300") && s.contains("E0300"), "{what}: {s}");
        assert!(s.contains("parent_code"), "{what}: {s}");
    }
    let json = karac(&["explain", "W0300", "--format=json"]);
    let s = text(&json.stdout);
    assert!(json.status.success(), "{s}");
    assert!(s.contains("\"code\":\"W0300\""), "{s}");
    assert!(s.contains("\"kind\":\"redundant_clone\""), "{s}");
}

/// A lint that never shared a code has no `W03xx` number, and `explain` still
/// takes its name: a page from the lint registry.
#[test]
fn explain_takes_a_lint_name_without_a_code_of_its_own() {
    let out = karac(&["explain", "redundant_suffix"]);
    let s = text(&out.stdout);
    assert!(out.status.success(), "{s}{}", text(&out.stderr));
    assert!(s.contains("lint: redundant_suffix"), "{s}");
    assert!(s.contains("`warn` by default"), "{s}");
    let json = karac(&["explain", "redundant_suffix", "--format=json"]);
    let j = text(&json.stdout);
    assert!(json.status.success(), "{j}");
    assert!(j.contains("\"kind\":\"lint\""), "{j}");
}

/// Every lint in the band explains, under both codes.
#[test]
fn every_lint_band_code_explains() {
    for n in 300..=319 {
        for prefix in ["W", "E"] {
            let code = format!("{prefix}0{n}");
            let out = karac(&["explain", &code]);
            assert!(
                out.status.success(),
                "{code}: {}{}",
                text(&out.stdout),
                text(&out.stderr)
            );
        }
    }
}

/// -300: the help said parse / effect / ownership codes were not catalogued
/// and to look them up by class; every band is catalogued, and E0002 explains.
#[test]
fn explain_help_names_every_catalogued_band() {
    let out = karac(&["explain", "--help"]);
    let s = text(&out.stdout) + &text(&out.stderr);
    assert!(!s.contains("not\n    catalogued yet"), "{s}");
    assert!(!s.contains("not catalogued yet"), "{s}");
    for band in [
        "E00xx", "E01xx", "W03xx", "E04xx", "E05xx", "E0600", "E08xx",
    ] {
        assert!(s.contains(band), "{band}: {s}");
    }
    assert!(s.contains("karac explain redundant_clone"), "{s}");
    let parse = karac(&["explain", "E0002"]);
    assert!(parse.status.success(), "{}", text(&parse.stderr));
}
