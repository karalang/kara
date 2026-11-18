//! B-2026-11-18-48: lints that run outside the typechecker's attribute
//! cascade read the CLI levels only, so `#[allow]` / `#[deny]` on the
//! function around a finding did nothing, and `map_value_clone_reinsert`
//! ignored `-D` too. Each case takes the lint's warning pin, puts the
//! attribute on every top-level `fn`, and checks the level it sets.

use std::path::Path;
use std::process::Command;

const LINTS: &[(&str, &str, &str)] = &[
    ("w0278", "must_use", "0278"),
    ("w0296", "unused_doc_comment", "0296"),
    ("w0312", "map_value_clone_reinsert", "0312"),
    ("w0318", "ffi_float_eq", "0318"),
    ("w0319", "ambiguous_not_comparison", "0319"),
];

fn check(name: &str, source: &str, args: &[&str]) -> (bool, String) {
    // One directory per call: the tests run in parallel on the same pins.
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir =
        std::env::temp_dir().join(format!("karac_lint_attr_{name}_{}_{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("source.kara"), source).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .current_dir(&dir)
        .arg("check")
        .args(args)
        .args(["--output=json", "source.kara"])
        .output()
        .expect("karac");
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

fn pin(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("corpus/diagnostics")
        .join(name)
        .join("source.kara");
    std::fs::read_to_string(path).unwrap()
}

fn with_attr(source: &str, attr: &str) -> String {
    source
        .lines()
        .map(|l| {
            if l.starts_with("fn ") {
                format!("{attr}\n{l}")
            } else {
                l.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn lint_item_attributes_allow_on_the_fn_suppresses() {
    for (name, lint, num) in LINTS {
        let (ok, out) = check(
            name,
            &with_attr(&pin(name), &format!("#[allow({lint})]")),
            &[],
        );
        assert!(ok, "{lint}: {out}");
        assert!(!out.contains(&format!("W{num}")), "{lint}: {out}");
        assert!(!out.contains(&format!("E{num}")), "{lint}: {out}");
    }
}

#[test]
fn lint_item_attributes_deny_on_the_fn_is_an_error() {
    for (name, lint, num) in LINTS {
        let (ok, out) = check(
            name,
            &with_attr(&pin(name), &format!("#[deny({lint})]")),
            &[],
        );
        assert!(!ok, "{lint}: {out}");
        assert!(
            out.contains(&format!("\"code\":\"E{num}\"")),
            "{lint}: {out}"
        );
        assert!(
            !out.contains(&format!("\"code\":\"W{num}\"")),
            "{lint}: {out}"
        );
    }
}

#[test]
fn lint_item_attributes_allow_beats_the_command_line_deny() {
    for (name, lint, num) in LINTS {
        let source = with_attr(&pin(name), &format!("#[allow({lint})]"));
        let (ok, out) = check(name, &source, &["-D", lint]);
        assert!(ok, "{lint}: {out}");
        assert!(!out.contains(&format!("E{num}")), "{lint}: {out}");
    }
}

#[test]
fn lint_item_attributes_map_value_clone_reinsert_reads_dash_d() {
    let (ok, out) = check(
        "w0312_d",
        &pin("w0312"),
        &["-D", "map_value_clone_reinsert"],
    );
    assert!(!ok, "{out}");
    assert!(out.contains("\"code\":\"E0312\""), "{out}");
}
