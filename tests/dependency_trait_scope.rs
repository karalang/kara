//! A dependency's trait supplies methods only where it is imported
//! (design.md § Method resolution, B-2026-11-16-180). The traits in scope in a file
//! are its package's own, those it imports by name or through a wildcard,
//! and the prelude's; a call only an unimported dependency trait answers is
//! refused with the import that brings it in, and with two dependency traits
//! providing the method, importing one picks it.

use std::path::{Path, PathBuf};
use std::process::Command;

const KDEP: &str = "pub struct W { pub v: i64 }\n\
pub trait Show { fn show(self) -> i64; }\n\
impl Show for W { fn show(self) -> i64 { self.v + 7 } }\n\
pub trait Show2 { fn show(self) -> i64; }\n\
impl Show2 for W { fn show(self) -> i64 { 99 } }\n";

fn packages(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "karac-dep-trait-scope-{}-{tag}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let manifest = |name: &str, deps: &str| {
        format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\n\n[dependencies]\n{deps}")
    };
    std::fs::create_dir_all(dir.join("kdep/src")).unwrap();
    std::fs::create_dir_all(dir.join("app/src")).unwrap();
    std::fs::write(dir.join("kdep/kara.toml"), manifest("kdep", "")).unwrap();
    std::fs::write(
        dir.join("kdep/src/lib.kara"),
        "pub fn root() -> i64 { 0 }\n",
    )
    .unwrap();
    std::fs::write(dir.join("kdep/src/util.kara"), KDEP).unwrap();
    std::fs::write(
        dir.join("app/kara.toml"),
        manifest("app", "kdep = { path = \"../kdep\" }\n"),
    )
    .unwrap();
    dir
}

fn karac(app: &Path, args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .current_dir(app)
        .output()
        .unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// `main` checked and run under the interpreter: (check ok, run stdout, all output).
fn check_and_run(tag: &str, files: &[(&str, &str)]) -> (bool, bool, String, String) {
    let dir = packages(tag);
    let app = dir.join("app");
    for (path, text) in files {
        std::fs::write(app.join("src").join(path), text).unwrap();
    }
    let (checked, cout, cerr) = karac(&app, &["check"]);
    let (ran, rout, rerr) = karac(&app, &["run", "--interp"]);
    let _ = std::fs::remove_dir_all(&dir);
    (checked, ran, rout, format!("{cout}{cerr}\n--- run\n{rerr}"))
}

const CALL: &str = "fn main() {\n    println(W { v: 1 }.show());\n}\n";

#[test]
fn an_unimported_dependency_trait_supplies_no_method() {
    let main = format!("import kdep.util.W;\n{CALL}");
    let (checked, ran, _, out) = check_and_run("hidden", &[("main.kara", &main)]);
    assert!(
        !checked && !ran,
        "an unimported trait's method must be refused:\n{out}"
    );
    assert!(
        out.contains("no method 'show' on type 'W'")
            && out.contains("`import kdep.util.Show;`")
            && out.contains("`import kdep.util.Show2;`")
            && out.contains("which are not in scope here"),
        "the refusal names the imports that bring the method in:\n{out}"
    );
}

#[test]
fn importing_one_of_two_dependency_traits_picks_it() {
    for (tag, imp, want) in [("show", "Show", "8"), ("show2", "Show2", "99")] {
        let main = format!("import kdep.util.{{W, {imp}}};\n{CALL}");
        let (checked, ran, stdout, out) = check_and_run(tag, &[("main.kara", &main)]);
        assert!(checked && ran, "`{imp}` is imported, so it answers:\n{out}");
        assert_eq!(stdout.trim(), want, "{imp}:\n{out}");
    }
}

#[test]
fn a_wildcard_import_brings_a_dependency_modules_traits_in() {
    let main = format!("import kdep.util.*;\n{CALL}");
    let (checked, _, _, out) = check_and_run("wildcard", &[("main.kara", &main)]);
    // Both traits are in scope, so the call is the ambiguity, not the
    // missing import.
    assert!(!checked, "{out}");
    assert!(out.contains("ambiguous method 'show'"), "{out}");
    assert!(!out.contains("not in scope here"), "{out}");
}

#[test]
fn a_packages_own_trait_needs_no_import_in_its_other_modules() {
    let own = "pub struct P { pub v: i64 }\n\
pub trait Mine { fn mine(self) -> i64; }\n\
impl Mine for P { fn mine(self) -> i64 { self.v * 3 } }\n";
    let main = "import kdep.util.{W, Show};\nimport own.P;\nfn main() {\n    println(W { v: 1 }.show());\n    println(P { v: 2 }.mine());\n}\n";
    let (checked, ran, stdout, out) =
        check_and_run("own", &[("own.kara", own), ("main.kara", main)]);
    assert!(checked && ran, "{out}");
    assert_eq!(stdout.trim(), "8\n6", "{out}");
}
