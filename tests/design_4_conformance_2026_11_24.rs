//! design.md §4 Modules and packages, from the conformance pass of
//! 2026-11-24: what a package does that a lone file already did.
//! - A block's `const` works in a package module, as in a lone file: the
//!   module tree kept no mark of it and read it as a binding (E0127).
//! - E0113 gives the suggestion and, for a module with at most 10
//!   exports, the list of them; the list was dropped when a suggestion
//!   was found (Resolution, step 3).
//! - `#[test]` on a `fn` in a `_test.kara` file names that file, not the
//!   module's other file, and carries its code once.

use std::path::{Path, PathBuf};
use std::process::Command;

fn package(files: &[(&str, &str)]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "karac-design4-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("kara.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    for (p, s) in files {
        let f = dir.join("src").join(p);
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(f, s).unwrap();
    }
    dir
}

fn karac(dir: &Path, args: &[&str]) -> (i32, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap();
    let text =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    (out.status.code().unwrap_or(-1), text)
}

#[test]
fn design4_a_block_const_works_in_a_package_module() {
    let dir = package(&[
        (
            "main.kara",
            "import util.scaled;\nconst BASE: i64 = 10;\nfn f() -> i64 { const MOD: i64 = BASE * 100; MOD + 7 }\nfn main() { println(f\"{f()} {scaled()}\"); }\n",
        ),
        (
            "util.kara",
            "pub fn scaled() -> i64 {\n    const STEP: i64 = 3;\n    STEP * 2\n}\n",
        ),
    ]);
    let (code, out) = karac(&dir, &["check"]);
    assert_eq!(code, 0, "{out}");
    let (code, out) = karac(&dir, &["run"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("1007 6"), "{out}");
}

#[test]
fn design4_e0113_lists_a_small_modules_exports_beside_the_suggestion() {
    let dir = package(&[
        (
            "main.kara",
            "import db.connection.Conection;\nfn main() { }\n",
        ),
        (
            "db/connection.kara",
            "pub struct Connection { pub id: i64 }\npub struct Pool { pub n: i64 }\n",
        ),
    ]);
    let (code, out) = karac(&dir, &["check"]);
    assert_ne!(code, 0, "{out}");
    assert!(
        out.contains("unknown item `Conection` in module `db.connection`; available: Connection, Pool; did you mean `Connection`?"),
        "{out}"
    );
}

#[test]
fn design4_test_attr_on_a_fn_in_a_test_file_names_the_test_file() {
    let dir = package(&[
        ("main.kara", "fn main() { println(\"ok\"); }\n"),
        ("util.kara", "pub fn one() -> i64 { 1 }\n"),
        (
            "util_test.kara",
            "\n\n#[test]\nfn t() { assert_eq(1, 1); }\n",
        ),
    ]);
    let (code, out) = karac(&dir, &["check"]);
    assert_ne!(code, 0, "{out}");
    assert!(
        out.contains("error[E_TEST_ATTR_INVALID_TARGET]: src/util_test.kara:3:1: `#[test]` is not valid on a `fn`"),
        "{out}"
    );
    assert_eq!(
        out.matches("E_TEST_ATTR_INVALID_TARGET").count(),
        1,
        "{out}"
    );
}
