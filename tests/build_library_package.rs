//! `karac build` on a library package, as `karac new --lib` makes it
//! (B-2026-10-31-22): a library has no `main` and 0.8 has no library
//! artifact, so the build checks the package, says there is nothing to
//! link, and exits 0; a library that does not check fails as `karac check`
//! does. A package build's text output is `Built:` alone, with no package
//! summary.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-build-lib-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn karac(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap()
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// The scaffold `karac new <name> [--lib]` makes under a fresh directory.
fn scaffold(tag: &str, name: &str, lib: bool) -> PathBuf {
    let dir = scratch(tag);
    let mut args = vec!["new", name];
    if lib {
        args.push("--lib");
    }
    let out = karac(&dir, &args);
    assert!(out.status.success(), "{}", text(&out.stderr));
    dir.join(name)
}

#[test]
fn build_of_a_new_library_package_checks_it_and_links_nothing() {
    let root = scaffold("new", "mylib", true);
    assert!(root.join("src/lib.kara").is_file());
    let out = karac(&root, &["build"]);
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    assert_eq!(out.status.code(), Some(0), "{stdout}{stderr}");
    assert!(
        stdout.contains("library package `mylib`: checked, nothing to link"),
        "{stdout}"
    );
    for internal in ["v2", "M3", "slice", "project:"] {
        assert!(
            !stdout.contains(internal) && !stderr.contains(internal),
            "`{internal}` in: {stdout}{stderr}"
        );
    }
    assert!(!root.join("dist").exists(), "a library build wrote dist/");
}

#[test]
fn build_of_a_library_package_that_does_not_check_fails_as_check_does() {
    let root = scaffold("bad", "badlib", true);
    std::fs::write(
        root.join("src/lib.kara"),
        "pub fn half(x: i64) -> i64 {\n    x / \"2\"\n}\n",
    )
    .unwrap();
    let check = karac(&root, &["check"]);
    let build = karac(&root, &["build"]);
    assert_eq!(check.status.code(), Some(1), "{}", text(&check.stdout));
    assert_eq!(build.status.code(), Some(1), "{}", text(&build.stdout));
    assert_eq!(text(&build.stdout), text(&check.stdout));
    assert!(!text(&build.stdout).contains("nothing to link"));
}

#[test]
fn build_of_a_new_binary_package_prints_no_package_summary() {
    let root = scaffold("bin", "myapp", false);
    let out = karac(&root, &["build"]);
    let stdout = text(&out.stdout);
    assert_eq!(out.status.code(), Some(0), "{stdout}{}", text(&out.stderr));
    for line in ["project:", "edition:", "root:", "entry:", "modules:"] {
        assert!(!stdout.contains(line), "`{line}` in: {stdout}");
    }
}
