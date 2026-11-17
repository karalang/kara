//! A package imports only the packages its own manifest declares
//! (design.md § The package name in paths: a dependency is recognized "as
//! listed in kara.toml"; B-2026-11-17-38). One reached only through another
//! dependency is refused, E0112, naming the manifest to add it to; a
//! dependency's own imports are held to its manifest the same way.

use std::path::{Path, PathBuf};
use std::process::Command;

fn package(root: &Path, name: &str, deps: &[&str], file: &str, src: &str) {
    let dir = root.join(name);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    let mut toml = format!(
        "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2026\"\n\n[dependencies]\n"
    );
    for d in deps {
        toml.push_str(&format!("{d} = {{ path = \"../{d}\" }}\n"));
    }
    std::fs::write(dir.join("kara.toml"), toml).unwrap();
    std::fs::write(dir.join("src").join(file), src).unwrap();
}

fn scratch(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("karac-undeclared-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    package(&root, "lib", &[], "lib.kara", "pub fn f() -> i64 { 1 }\n");
    root
}

fn karac(dir: &Path, cmd: &str) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg(cmd)
        .current_dir(dir)
        .output()
        .unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr),
    )
}

#[test]
fn a_dependency_of_a_dependency_is_not_importable() {
    let root = scratch("transitive");
    package(
        &root,
        "mid",
        &["lib"],
        "lib.kara",
        "import lib.f;\npub fn g() -> i64 { f() + 1 }\n",
    );
    package(
        &root,
        "app",
        &["mid"],
        "main.kara",
        "import lib.f;\nfn main() { println(f()); }\n",
    );
    let (ok, text) = karac(&root.join("app"), "check");
    let _ = std::fs::remove_dir_all(&root);
    assert!(!ok, "check passed:\n{text}");
    assert!(text.contains("error[E0112]"), "{text}");
    assert!(
        text.contains("`lib` is not a dependency of this package")
            && text.contains("add `lib` to `[dependencies]` in kara.toml")
            && text.contains("it is a dependency of `mid`"),
        "{text}"
    );
}

#[test]
fn a_dependency_declared_directly_and_through_another_is_importable() {
    let root = scratch("direct");
    package(
        &root,
        "mid",
        &["lib"],
        "lib.kara",
        "import lib.f;\npub fn g() -> i64 { f() + 1 }\n",
    );
    package(
        &root,
        "app",
        &["mid", "lib"],
        "main.kara",
        "import lib.f;\nimport mid.g;\nfn main() { println(f() + g()); }\n",
    );
    let (ok, text) = karac(&root.join("app"), "check");
    let _ = std::fs::remove_dir_all(&root);
    assert!(ok, "check refused:\n{text}");
}

#[test]
fn a_dependency_is_held_to_its_own_manifest() {
    let root = scratch("in-dep");
    package(
        &root,
        "mid",
        &[],
        "lib.kara",
        "import lib.f;\npub fn g() -> i64 { f() + 1 }\n",
    );
    package(
        &root,
        "app",
        &["mid", "lib"],
        "main.kara",
        "import mid.g;\nfn main() { println(g()); }\n",
    );
    let (ok, text) = karac(&root.join("app"), "check");
    let _ = std::fs::remove_dir_all(&root);
    assert!(!ok, "check passed:\n{text}");
    assert!(
        text.contains("`lib` is not a dependency of this package")
            && text.contains("in `mid`'s kara.toml"),
        "{text}"
    );
}
