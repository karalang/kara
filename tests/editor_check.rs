//! The language server checks a buffer exactly as `karac check <file>` does
//! (B-2026-10-31-19): `karac::check_source` and `karac::hover_at`, which
//! `kara-lsp` calls, run the CLI's pipeline with the v2 core rules and the
//! Kāra library, so a program the command line passes shows no errors in the
//! editor. Each `karac new` scaffold is the first file a new user opens.

use std::process::Command;

fn karac() -> Command {
    Command::new(env!("CARGO_BIN_EXE_karac"))
}

/// The entry file of a fresh `karac new <name> <flag>` project.
fn scaffold(flag: &str, entry: &str) -> String {
    let dir = std::env::temp_dir().join(format!(
        "karac-editor-check-{}-{}",
        std::process::id(),
        flag.trim_start_matches('-')
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut cmd = karac();
    cmd.current_dir(&dir).arg("new").arg("app");
    if !flag.is_empty() {
        cmd.arg(flag);
    }
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = std::fs::read_to_string(dir.join("app").join(entry)).unwrap();
    let check = karac()
        .current_dir(dir.join("app"))
        .arg("check")
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        check.status.success(),
        "karac check on the {flag} scaffold: {}",
        String::from_utf8_lossy(&check.stdout)
    );
    text
}

#[test]
fn editor_check_passes_every_karac_new_scaffold() {
    for (flag, entry) in [
        ("", "src/main.kara"),
        ("--cli", "src/main.kara"),
        ("--lib", "src/lib.kara"),
    ] {
        let diags = karac::check_source(&scaffold(flag, entry));
        let shown: Vec<_> = diags.iter().map(|d| &d.message).collect();
        assert!(diags.is_empty(), "{flag} scaffold: {shown:?}");
    }
}

#[test]
fn editor_check_reports_what_karac_check_reports() {
    // A use after a move: the README's fix-loop program. Its span, its fix
    // and its severity are the CLI's.
    let src = "fn archive(log: own Vec[String]) -> i64 {\n    log.len()\n}\n\n\
               fn main() {\n    let log = [\"boot\".to_string(), \"ready\".to_string()];\n    \
               let n = archive(log);\n    println(f\"{n} archived, last was {log[1]}\");\n}\n";
    let diags = karac::check_source(src);
    assert_eq!(diags.len(), 1, "{diags:?}");
    let d = &diags[0];
    assert_eq!((d.phase, d.severity), ("ownership", "error"));
    assert_eq!((d.line, d.column), (8, 39));
    assert_eq!(&src[d.offset..d.offset + d.length], "log");
    assert_eq!(d.fixes.len(), 1, "{d:?}");
    assert_eq!(
        (d.fixes[0].offset, d.fixes[0].replacement.as_str()),
        (151, ".clone()")
    );
}

#[test]
fn editor_hover_types_a_library_iterator() {
    // `lines` is a Kāra-library iterator; the old editor-only pipeline had no
    // library and typed it as a `Vec`, which is where its false `count`
    // errors came from.
    let src = "fn main() {\n    let text = \"a b\\nc\";\n    let n = text.lines().count();\n    println(f\"{n}\");\n}\n";
    assert!(
        karac::check_source(src).is_empty(),
        "{:?}",
        karac::check_source(src)
    );
    let at = src.find("lines").unwrap();
    let hover = karac::hover_at(src, at).expect("a hover");
    assert!(
        !hover.type_display.starts_with("Vec"),
        "`text.lines()` hovers as {}",
        hover.type_display
    );
}
