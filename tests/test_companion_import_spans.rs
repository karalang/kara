//! An import's segment and name spans move with the rest of a module's
//! spans (B-2026-11-13-40). They did not: a merged `_test.kara`
//! companion's E0111 on an imported name was reported in its sibling's file,
//! and `karac fix` of a mistyped module in a package's non-entry module
//! wrote the fix at the start of the file.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn package(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("karac-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("src/db")).unwrap();
    std::fs::write(
        root.join("kara.toml"),
        "[package]\nname = \"ta\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    std::fs::write(root.join("src/main.kara"), "fn main() { println(1); }\n").unwrap();
    std::fs::write(
        root.join("src/db/conn.kara"),
        "private fn secret() -> i64 { 7 }\npub fn x() -> i64 { secret() }\n",
    )
    .unwrap();
    std::fs::write(root.join("src/ui.kara"), "pub fn u() -> i64 { 1 }\n").unwrap();
    root
}

fn karac(args: &[&str], cwd: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap()
}

fn text(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned() + &String::from_utf8_lossy(&out.stdout)
}

#[test]
fn a_test_companion_s_import_error_names_its_own_file() {
    let root = package("companion-e0111");
    std::fs::write(
        root.join("src/ui_test.kara"),
        "\nimport db.conn.secret;\ntest \"s\" { assert_eq(secret(), 7); }\n",
    )
    .unwrap();
    let check = text(&karac(&["check"], &root));
    let json = text(&karac(&["check", "--output=json"], &root));
    let test = text(&karac(&["test"], &root));
    let _ = std::fs::remove_dir_all(&root);
    assert!(
        check.contains("error[E0111]: src/ui_test.kara:2:16:"),
        "{check}"
    );
    for out in [&json, &test] {
        assert!(
            out.contains("\"file\":\"src/ui_test.kara\",\"line\":2,\"column\":16"),
            "{out}"
        );
        assert!(!out.contains("\"file\":\"src/ui.kara\""), "{out}");
    }
}

#[test]
fn fix_of_a_mistyped_module_in_a_package_module_rewrites_the_segment() {
    let root = package("module-typo-fix");
    std::fs::write(
        root.join("src/ui.kara"),
        "import db.conx.x;\npub fn u() -> i64 { x() }\n",
    )
    .unwrap();
    let fix = karac(&["fix"], &root);
    let fixed = std::fs::read_to_string(root.join("src/ui.kara")).unwrap();
    let check = karac(&["check"], &root);
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(
        fixed,
        "import db.conn.x;\npub fn u() -> i64 { x() }\n",
        "{}",
        text(&fix)
    );
    assert!(check.status.success(), "{}", text(&check));
}
