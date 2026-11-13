//! `karac check src/x_test.kara` in a package checks the package's test
//! files as `karac check` with no file does, and `karac fix` of a test
//! companion fixes its imports (B-2026-11-13-42). The named companion was
//! left out of the package check, so its errors went unreported: "All checks
//! passed." with exit 0, and `fix` found nothing to fix.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn package(tag: &str, test_src: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("karac-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("src/db")).unwrap();
    std::fs::write(
        root.join("kara.toml"),
        "[package]\nname = \"tc\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    std::fs::write(root.join("src/main.kara"), "fn main() { println(1); }\n").unwrap();
    std::fs::write(root.join("src/ui.kara"), "pub fn u() -> i64 { 1 }\n").unwrap();
    std::fs::write(root.join("src/db/conn.kara"), "pub fn x() -> i64 { 7 }\n").unwrap();
    std::fs::write(root.join("src/ui_test.kara"), test_src).unwrap();
    root
}

fn karac(args: &[&str], cwd: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap()
}

/// Both streams, with the host's path separator as `/`: karac prints
/// `src\ui.kara` on Windows, and `src\\ui.kara` inside JSON.
fn text(out: &Output) -> String {
    (String::from_utf8_lossy(&out.stderr).into_owned() + &String::from_utf8_lossy(&out.stdout))
        .replace("\\\\", "/")
        .replace('\\', "/")
}

#[test]
fn checking_a_named_test_companion_reports_its_errors() {
    let root = package(
        "named-companion-err",
        "test \"t\" { let n: i64 = \"x\"; assert_eq(n, u()); }\n",
    );
    let check = karac(&["check", "src/ui_test.kara"], &root);
    let json = karac(&["check", "--output=json", "src/ui_test.kara"], &root);
    // From outside the package, by its path.
    let file = root.join("src/ui_test.kara");
    let outside = karac(&["check", file.to_str().unwrap()], &std::env::temp_dir());
    let _ = std::fs::remove_dir_all(&root);
    for out in [&check, &json, &outside] {
        assert!(!out.status.success(), "{}", text(out));
        assert!(!text(out).contains("All checks passed"), "{}", text(out));
    }
    assert!(
        text(&check).contains("src/ui_test.kara:1:25: expected 'i64', found 'String'"),
        "{}",
        text(&check)
    );
    assert!(
        text(&json).contains("\"file\":\"src/ui_test.kara\",\"line\":1,\"column\":25"),
        "{}",
        text(&json)
    );
}

#[test]
fn a_clean_named_test_companion_still_passes() {
    let root = package("named-companion-ok", "test \"t\" { assert_eq(u(), 1); }\n");
    let check = karac(&["check", "src/ui_test.kara"], &root);
    let _ = std::fs::remove_dir_all(&root);
    assert!(check.status.success(), "{}", text(&check));
    assert!(
        text(&check).contains("All checks passed"),
        "{}",
        text(&check)
    );
}

#[test]
fn fix_of_a_test_companion_fixes_a_mistyped_import() {
    let root = package(
        "named-companion-fix",
        "\nimport db.conx.x;\ntest \"s\" { assert_eq(x(), 7); }\n",
    );
    let fix = karac(&["fix", "src/ui_test.kara"], &root);
    let fixed = std::fs::read_to_string(root.join("src/ui_test.kara")).unwrap();
    let check = karac(&["check", "src/ui_test.kara"], &root);
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(
        fixed,
        "\nimport db.conn.x;\ntest \"s\" { assert_eq(x(), 7); }\n",
        "{}",
        text(&fix)
    );
    assert!(check.status.success(), "{}", text(&check));
}
