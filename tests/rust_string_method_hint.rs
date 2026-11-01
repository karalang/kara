//! Rust's `trim_start_matches`, `trim_end_matches`, `trim_matches` and
//! `to_owned` are not Kāra string methods, and no edit distance reaches
//! their Kāra spellings, so they got a bare "no method" (B-2026-11-01-9).
//! Each now names its twin; `to_owned()` is renamed to `to_string()` by
//! `karac fix`, and `trim_matches(|c| ..)` no longer adds a second error
//! for its closure.

use std::process::{Command, Stdio};

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn temp(tag: &str, src: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "karac-rust-string-method-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    path
}

#[test]
fn rust_string_methods_name_their_kara_twins() {
    for (tag, line, want) in [
        (
            "trim-start-matches",
            "let t = s.trim_start_matches(\"[\");",
            "no method 'trim_start_matches' on type 'String': Kāra strips a prefix with `strip_prefix(p)`",
        ),
        (
            "trim-end-matches",
            "let t = v.trim_end_matches(\"]\");",
            "no method 'trim_end_matches' on type 'Str': Kāra strips a suffix with `strip_suffix(p)`",
        ),
        (
            "trim-matches",
            "let t = s.trim_matches(|c| c == 0);",
            "no method 'trim_matches' on type 'String': Kāra strips each end",
        ),
    ] {
        let src = format!(
            "fn main() {{\n    let s = \"[12]\";\n    let v: Str = s.trim();\n    {line}\n}}\n"
        );
        let path = temp(tag, &src);
        let check = karac(&["check", "--output=json", path.to_str().unwrap()]);
        let _ = std::fs::remove_file(&path);
        let json = String::from_utf8_lossy(&check.stdout);
        assert!(json.contains(want), "{tag}: missing {want:?} in {json}");
        assert_eq!(
            json.matches("\"code\":").count(),
            1,
            "{tag}: one error, no cascade: {json}"
        );
        assert!(!json.contains("\"replacement\":{"), "{tag}: no edit: {json}");
    }
}

#[test]
fn karac_fix_renames_to_owned_to_to_string() {
    let src = "fn keep(s: ref String) -> String {\n    s.to_owned()\n}\n\
               fn main() {\n    let s = \"ab\";\n    let t = s.to_owned();\n    \
               println(f\"{keep(t)}\");\n}\n";
    let path = temp("to-owned", src);
    let file = path.to_str().unwrap();
    let check = karac(&["check", "--output=json", file]);
    let json = String::from_utf8_lossy(&check.stdout);
    assert_eq!(json.matches("\"text\":\"to_string\"").count(), 2, "{json}");
    let fix = karac(&["fix", file]);
    assert!(
        fix.status.success(),
        "{}",
        String::from_utf8_lossy(&fix.stderr)
    );
    let out = std::fs::read_to_string(&path).unwrap();
    assert!(
        out.contains("s.to_string()\n") && out.contains("let t = s.to_string();"),
        "{out}"
    );
    let run = karac(&["__mir-run", file]);
    let _ = std::fs::remove_file(&path);
    assert_eq!(
        (
            run.status.code(),
            String::from_utf8_lossy(&run.stdout).as_ref()
        ),
        (Some(0), "ab\n"),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
}
