//! B-2026-11-13-76: `karac test` collects `test` blocks only from
//! `_test.kara` files (design.md §14 Testing), so a block in `main.kara`
//! never ran, and `karac check` reported nothing. It is now
//! `E_TEST_OUTSIDE_TEST_FILE`, naming the companion file to move it to.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn package(tag: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-test-outside-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("kara.toml"),
        "[package]\nname = \"tq\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    for (name, text) in files {
        std::fs::write(dir.join(name), text).unwrap();
    }
    dir
}

fn karac(dir: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

const MAIN: &str =
    "pub fn one() -> i64 { 1 }\nfn main() { println(one()); }\ntest \"lost\" { assert(false); }\n";

#[test]
fn test_block_in_main_is_refused_naming_its_companion() {
    let dir = package("alone", &[("src/main.kara", MAIN)]);
    for args in [&["check"][..], &["check", "src/main.kara"]] {
        let (ok, out) = karac(&dir, args);
        assert!(!ok, "{args:?}: {out}");
        assert!(out.contains("E_TEST_OUTSIDE_TEST_FILE"), "{args:?}: {out}");
        assert!(
            out.contains("test \"lost\" is in `main.kara`"),
            "{args:?}: {out}"
        );
        assert!(out.contains("main_test.kara"), "{args:?}: {out}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_block_in_main_is_refused_beside_a_companion() {
    let dir = package(
        "beside",
        &[
            ("src/main.kara", MAIN),
            (
                "src/main_test.kara",
                "test \"kept\" { assert_eq(one(), 1); }\n",
            ),
        ],
    );
    let (ok, out) = karac(&dir, &["check"]);
    assert!(!ok, "{out}");
    assert!(out.contains("test \"lost\" is in `main.kara`"), "{out}");
    assert!(!out.contains("test \"kept\""), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_block_in_a_test_file_or_a_lone_file_is_accepted() {
    let dir = package(
        "kept",
        &[
            (
                "src/main.kara",
                "pub fn one() -> i64 { 1 }\nfn main() { println(one()); }\n",
            ),
            (
                "src/main_test.kara",
                "test \"kept\" { assert_eq(one(), 1); }\n",
            ),
        ],
    );
    let (ok, out) = karac(&dir, &["check"]);
    assert!(ok, "{out}");
    let _ = std::fs::remove_dir_all(&dir);

    // Outside a package no runner collects tests, so a lone file is left
    // as it was.
    let lone = std::env::temp_dir().join(format!("karac-test-outside-lone-{}", std::process::id()));
    std::fs::create_dir_all(&lone).unwrap();
    std::fs::write(lone.join("cell.kara"), MAIN).unwrap();
    let (ok, out) = karac(&lone, &["check", "cell.kara"]);
    assert!(ok, "{out}");
    let _ = std::fs::remove_dir_all(&lone);
}
