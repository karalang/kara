//! `karac check`, `run`, `build` and `test` validate a package's manifest
//! and dependencies the same way (B-2026-11-13-41): a registry dependency
//! with no proxy is refused with E_REGISTRY_DEP_UNSUPPORTED, a git one that
//! cannot be fetched with E_GIT_FETCH_FAILED, a cycle among path
//! dependencies is E_DEPENDENCY_CYCLE, and an unknown `[package]` key warns.
//! `check` and `run` used to skip registry and git dependencies, the bare
//! package `check` reported a cycle as E0112 at the import, and only
//! `build` warned of an unknown key.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn dir(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("karac-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn package(at: &Path, name: &str, deps: &str, main: Option<&str>) {
    std::fs::create_dir_all(at.join("src")).unwrap();
    std::fs::write(
        at.join("kara.toml"),
        format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\n\n[dependencies]\n{deps}\n"),
    )
    .unwrap();
    match main {
        Some(m) => std::fs::write(at.join("src/main.kara"), m).unwrap(),
        None => std::fs::write(at.join("src/lib.kara"), "pub fn f() -> i64 { 1 }\n").unwrap(),
    }
}

fn karac(args: &[&str], cwd: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .current_dir(cwd)
        .env_remove("KARAC_REGISTRY_PROXY")
        .env_remove("KARAC_REGISTRY_URL")
        .output()
        .unwrap()
}

fn text(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned() + &String::from_utf8_lossy(&out.stdout)
}

const COMMANDS: [&[&str]; 4] = [
    &["check"],
    &["check", "src/main.kara"],
    &["run", "src/main.kara"],
    &["test"],
];

fn every_command_refuses(at: &Path, code: &str) {
    for args in COMMANDS {
        let out = karac(args, at);
        assert!(!out.status.success(), "{args:?}: {}", text(&out));
        assert!(text(&out).contains(code), "{args:?}: {}", text(&out));
    }
}

#[test]
fn a_registry_dependency_without_a_proxy_is_refused_by_every_command() {
    let root = dir("parity-registry");
    package(
        &root,
        "app",
        "http = \"1.2\"",
        Some("fn main() { println(1); }\n"),
    );
    every_command_refuses(&root, "E_REGISTRY_DEP_UNSUPPORTED");
    let check = text(&karac(&["check"], &root));
    let _ = std::fs::remove_dir_all(&root);
    assert!(
        check.contains("error[E_REGISTRY_DEP_UNSUPPORTED]"),
        "{check}"
    );
}

#[test]
fn a_git_dependency_that_cannot_be_fetched_is_refused_by_every_command() {
    let root = dir("parity-git");
    package(
        &root,
        "app",
        "json = { git = \"file:///nonexistent/karac-parity-repo\" }",
        Some("fn main() { println(1); }\n"),
    );
    every_command_refuses(&root, "E_GIT_FETCH_FAILED");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_cycle_among_path_dependencies_is_a_dependency_cycle_everywhere() {
    let root = dir("parity-cycle");
    package(
        &root.join("a"),
        "a",
        "b = { path = \"../b\" }",
        Some("import b.f;\nfn main() { println(f()); }\n"),
    );
    package(&root.join("b"), "b", "c = { path = \"../c\" }", None);
    package(&root.join("c"), "c", "b = { path = \"../b\" }", None);
    every_command_refuses(&root.join("a"), "E_DEPENDENCY_CYCLE");
    let check = text(&karac(&["check"], &root.join("a")));
    let _ = std::fs::remove_dir_all(&root);
    assert!(!check.contains("E0112"), "{check}");
}

#[test]
fn an_unknown_package_key_warns_in_every_command() {
    let root = dir("parity-unknown-key");
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("kara.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nfrobnicate = 3\n",
    )
    .unwrap();
    std::fs::write(root.join("src/main.kara"), "fn main() { println(1); }\n").unwrap();
    for args in COMMANDS {
        let out = karac(args, &root);
        assert!(out.status.success(), "{args:?}: {}", text(&out));
        let err = String::from_utf8_lossy(&out.stderr);
        assert_eq!(
            err.matches("warning[manifest]: unknown key `[package].frobnicate`")
                .count(),
            1,
            "{args:?}: {err}"
        );
    }
    let _ = std::fs::remove_dir_all(&root);
}
