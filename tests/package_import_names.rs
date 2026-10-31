//! The import name a dependency is reached by (design.md § Packages and
//! manifests, B-2026-10-28-27): a hyphen in its name maps to `_`, its
//! `[package].import` replaces the name, a dependent's `alias` overrides
//! both, and two dependencies claiming one name are a manifest error.
//!
//! Each case is checked with `karac check`, and with the llvm feature also
//! built and run.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const LLVM: bool = cfg!(feature = "llvm");

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-import-names-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Write package `dir/<rel>` with `package_extra` lines under `[package]`,
/// `deps` under `[dependencies]`, and `files` as `(path under src/, text)`.
fn write_package(
    dir: &Path,
    rel: &str,
    name: &str,
    package_extra: &str,
    deps: &str,
    files: &[(&str, &str)],
) {
    let root = dir.join(rel);
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("kara.toml"),
        format!(
            "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2026\"\n{package_extra}\n[dependencies]\n{deps}"
        ),
    )
    .unwrap();
    for (path, text) in files {
        std::fs::write(root.join("src").join(path), text).unwrap();
    }
}

const ANSWER: &str = "pub fn answer() -> i64 {\n    42\n}\n";

fn karac(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .current_dir(dir)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap()
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// `karac check` passes in `app`, and with llvm the built program prints
/// `expected`.
fn checks_and_prints(app: &Path, expected: &str) {
    let out = karac(app, &["check"]);
    assert!(
        out.status.success(),
        "check failed:\n{}{}",
        text(&out.stdout),
        text(&out.stderr)
    );
    if LLVM {
        let out = karac(app, &["build"]);
        assert!(out.status.success(), "build failed:\n{}", text(&out.stderr));
        let run = Command::new(app.join("dist/app")).output().unwrap();
        assert!(run.status.success(), "{}", text(&run.stderr));
        assert_eq!(text(&run.stdout), expected);
    }
}

#[test]
fn a_hyphenated_dependency_is_imported_with_underscores() {
    let dir = scratch("hyphen");
    write_package(&dir, "data", "kara-data", "", "", &[("lib.kara", ANSWER)]);
    write_package(
        &dir,
        "app",
        "app",
        "",
        "kara-data = { path = \"../data\" }\n",
        &[(
            "main.kara",
            "import kara_data.answer;\n\nfn main() {\n    println(f\"{answer()}\");\n}\n",
        )],
    );
    checks_and_prints(&dir.join("app"), "42\n");
}

#[test]
fn a_package_import_key_names_the_dependency_for_its_dependents() {
    let dir = scratch("import-key");
    write_package(
        &dir,
        "data",
        "kara-data",
        "import = \"data\"\n",
        "",
        &[
            ("lib.kara", ANSWER),
            ("frame.kara", "pub fn rows() -> i64 {\n    3\n}\n"),
        ],
    );
    write_package(
        &dir,
        "app",
        "app",
        "",
        "kara-data = { path = \"../data\" }\n",
        &[(
            "main.kara",
            "import data.answer;\nimport data.frame.rows;\n\n\
             fn main() {\n    println(f\"{answer()} {rows()}\");\n}\n",
        )],
    );
    checks_and_prints(&dir.join("app"), "42 3\n");
}

/// A local `db` module hides the dependency `db`; the alias reaches it.
#[test]
fn an_alias_reaches_a_dependency_a_local_module_hides() {
    let dir = scratch("alias");
    write_package(&dir, "db", "db", "", "", &[("lib.kara", ANSWER)]);
    write_package(
        &dir,
        "app",
        "app",
        "",
        "db = { path = \"../db\", alias = \"db_ext\" }\n",
        &[
            (
                "main.kara",
                "import db.local;\nimport db_ext.answer;\n\n\
                 fn main() {\n    println(f\"{local()} {answer()}\");\n}\n",
            ),
            ("db.kara", "pub fn local() -> i64 {\n    7\n}\n"),
        ],
    );
    checks_and_prints(&dir.join("app"), "7 42\n");
}

/// `mid` writes its own alias for `kara-data`, which the app imports by
/// the default name: both resolve to the one package.
#[test]
fn a_dependencys_own_alias_resolves_inside_that_dependency() {
    let dir = scratch("transitive");
    write_package(&dir, "data", "kara-data", "", "", &[("lib.kara", ANSWER)]);
    write_package(
        &dir,
        "mid",
        "mid",
        "",
        "kara-data = { path = \"../data\", alias = \"kd\" }\n",
        &[(
            "lib.kara",
            "import kd.answer;\n\npub fn twice() -> i64 {\n    answer() * 2\n}\n",
        )],
    );
    write_package(
        &dir,
        "app",
        "app",
        "",
        "kara-data = { path = \"../data\" }\nmid = { path = \"../mid\" }\n",
        &[(
            "main.kara",
            "import kara_data.answer;\nimport mid.twice;\n\n\
             fn main() {\n    println(f\"{answer()} {twice()}\");\n}\n",
        )],
    );
    checks_and_prints(&dir.join("app"), "42 84\n");
}

#[test]
fn two_dependencies_claiming_one_import_name_are_a_manifest_error() {
    let dir = scratch("collision");
    write_package(&dir, "a", "kara-data", "", "", &[("lib.kara", ANSWER)]);
    write_package(&dir, "b", "kara_data", "", "", &[("lib.kara", ANSWER)]);
    write_package(
        &dir,
        "app",
        "app",
        "",
        "kara-data = { path = \"../a\" }\nkara_data = { path = \"../b\" }\n",
        &[("main.kara", "fn main() {}\n")],
    );
    let out = karac(&dir.join("app"), &["check"]);
    let all = format!("{}{}", text(&out.stdout), text(&out.stderr));
    assert!(!out.status.success(), "{all}");
    assert!(
        all.contains("`kara-data` and `kara_data` both claim the import name `kara_data`"),
        "{all}"
    );

    // An alias separates them.
    write_package(
        &dir,
        "app",
        "app",
        "",
        "kara-data = { path = \"../a\" }\nkara_data = { path = \"../b\", alias = \"other\" }\n",
        &[(
            "main.kara",
            "import kara_data.answer;\nimport other.answer as other_answer;\n\n\
             fn main() {\n    println(f\"{answer() + other_answer()}\");\n}\n",
        )],
    );
    checks_and_prints(&dir.join("app"), "84\n");
}

/// Two packages whose `[package].import` keys collide are refused when
/// their manifests are read.
#[test]
fn two_dependencies_declaring_one_import_name_are_refused() {
    let dir = scratch("declared-collision");
    let import = "import = \"data\"\n";
    write_package(&dir, "a", "data-a", import, "", &[("lib.kara", ANSWER)]);
    write_package(&dir, "b", "data-b", import, "", &[("lib.kara", ANSWER)]);
    write_package(
        &dir,
        "app",
        "app",
        "",
        "data-a = { path = \"../a\" }\ndata-b = { path = \"../b\" }\n",
        &[("main.kara", "fn main() {}\n")],
    );
    let out = karac(&dir.join("app"), &["build"]);
    let all = format!("{}{}", text(&out.stdout), text(&out.stderr));
    assert!(!out.status.success(), "{all}");
    assert!(
        all.contains("dependencies `data-a` and `data-b` are both imported as `data`"),
        "{all}"
    );
}

#[test]
fn an_alias_or_import_key_must_be_an_identifier() {
    let dir = scratch("bad-names");
    write_package(&dir, "dep", "dep", "", "", &[("lib.kara", ANSWER)]);
    write_package(
        &dir,
        "app",
        "app",
        "",
        "dep = { path = \"../dep\", alias = \"not-one\" }\n",
        &[("main.kara", "fn main() {}\n")],
    );
    let out = karac(&dir.join("app"), &["check"]);
    let all = format!("{}{}", text(&out.stdout), text(&out.stderr));
    assert!(!out.status.success(), "{all}");
    assert!(all.contains("`alias` must be an identifier"), "{all}");

    write_package(
        &dir,
        "app",
        "app",
        "import = \"fn\"\n",
        "",
        &[("main.kara", "fn main() {}\n")],
    );
    let out = karac(&dir.join("app"), &["check"]);
    let all = format!("{}{}", text(&out.stdout), text(&out.stderr));
    assert!(!out.status.success(), "{all}");
    assert!(all.contains("import"), "{all}");
}
