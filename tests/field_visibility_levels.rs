//! B-2026-11-04-22 — design.md §5: "Field visibility takes the three levels
//! of §4 Visibility: a field is visible in its package by default, to users
//! of the package with `pub`, and in its directory only with `private`."
//! A field with no keyword was refused outside its defining module, and
//! `private` did not parse on a field.

use std::path::Path;
use std::process::Command;

fn check(dir: &Path, main: &str) -> (bool, String) {
    std::fs::write(dir.join("src/main.kara"), main).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .current_dir(dir)
        .output()
        .unwrap();
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

#[test]
fn a_field_is_package_visible_by_default_and_private_is_per_directory() {
    let dir = std::env::temp_dir().join(format!("karac-field-vis-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src/db")).unwrap();
    std::fs::write(
        dir.join("kara.toml"),
        "[package]\nname = \"fv\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/db/c.kara"),
        "pub struct S {\n    pub a: i64,\n    inner: i64,\n    private secret: i64,\n}\n\
         pub fn mk() -> S {\n    S { a: 1, inner: 2, secret: 3 }\n}\n",
    )
    .unwrap();
    // A sibling in the same directory reads the `private` field.
    std::fs::write(
        dir.join("src/db/peek.kara"),
        "import db.c.{mk};\npub fn secret() -> i64 {\n    mk().secret\n}\n",
    )
    .unwrap();
    let (ok, out) = check(
        &dir,
        "import db.c.{mk};\nimport db.peek.{secret};\nfn main() {\n    let s = mk();\n    \
         println(f\"{s.a} {s.inner} {secret()}\");\n}\n",
    );
    assert!(
        ok,
        "a default field across directories, and `private` within one: {out}"
    );
    let (ok, out) = check(
        &dir,
        "import db.c.{mk};\nfn main() {\n    let s = mk();\n    println(s.secret);\n}\n",
    );
    assert!(!ok, "a `private` field from another directory: {out}");
    assert!(
        out.contains(
            "field 'secret' of struct 'S' is `private`: visible only to files in the same \
             directory as module `db.c`"
        ),
        "{out}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
