//! A module's file or directory name is a value name (design.md CN-2):
//! `src/DbClient.kara` is a compile error, and the refusal names the
//! snake_case path to rename it to (B-2026-11-16-258). The platform and
//! test suffixes survive the rename; a directory is refused at its own
//! segment.

use karac::walker::{walk_project, Platform, WalkerError, WalkerOpts};
use std::path::{Path, PathBuf};

fn project(name: &str, files: &[(&str, &str)]) -> PathBuf {
    let root = std::env::temp_dir().join(format!("karac-cn2-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    for (rel, text) in files {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }
    root
}

fn refusal(root: &Path) -> (PathBuf, String, PathBuf) {
    let opts = WalkerOpts {
        target: Platform::Linux,
        include_tests: true,
    };
    let err = walk_project(root, opts).expect_err("a non-value module name is refused");
    let _ = std::fs::remove_dir_all(root);
    match err {
        WalkerError::ModuleNameNotValueClass {
            path,
            name,
            rename_to,
        } => (path, name, rename_to),
        other => panic!("expected the CN-2 refusal, got {other}"),
    }
}

#[test]
fn a_type_class_module_file_is_refused_with_its_snake_case_name() {
    let root = project(
        "file",
        &[
            (
                "src/main.kara",
                "import Db.one;\nfn main() { println(one()); }\n",
            ),
            ("src/Db.kara", "pub fn one() -> i64 { 1 }\n"),
        ],
    );
    let (path, name, to) = refusal(&root);
    assert_eq!(path, PathBuf::from("src/Db.kara"));
    assert_eq!(name, "Db");
    assert_eq!(to, PathBuf::from("src/db.kara"));
}

#[test]
fn a_platform_file_keeps_its_suffix_in_the_rename() {
    let root = project(
        "platform",
        &[
            ("src/main.kara", "fn main() {}\n"),
            ("src/HttpClient_linux.kara", "pub fn two() -> i64 { 2 }\n"),
        ],
    );
    let (_, name, to) = refusal(&root);
    assert_eq!(name, "HttpClient");
    assert_eq!(to, PathBuf::from("src/http_client_linux.kara"));
}

#[test]
fn a_type_class_module_directory_is_refused_at_the_directory() {
    let root = project(
        "dir",
        &[
            ("src/main.kara", "fn main() {}\n"),
            ("src/Net/http.kara", "pub fn two() -> i64 { 2 }\n"),
        ],
    );
    let (path, name, to) = refusal(&root);
    assert_eq!(path, PathBuf::from("src/Net"));
    assert_eq!(name, "Net");
    assert_eq!(to, PathBuf::from("src/net"));
}

#[test]
fn snake_case_module_files_are_accepted() {
    let root = project(
        "ok",
        &[
            ("src/main.kara", "fn main() {}\n"),
            ("src/db_client.kara", "pub fn one() -> i64 { 1 }\n"),
            ("src/db_client_test.kara", "test \"one\" { }\n"),
            ("src/net/http2.kara", "pub fn two() -> i64 { 2 }\n"),
        ],
    );
    let opts = WalkerOpts {
        target: Platform::Linux,
        include_tests: true,
    };
    let walked = walk_project(&root, opts);
    let _ = std::fs::remove_dir_all(&root);
    walked.expect("snake_case module names pass");
}
