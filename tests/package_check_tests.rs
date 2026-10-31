//! `karac check` in a package checks its `_test.kara` files too, with the
//! test prelude, as `karac test` builds them: a package whose tests do not
//! compile is not reported clean (design.md § Colocated tests, ruling
//! 2026-10-30). Before it, `check` said "All checks passed." and `test` then
//! refused the same package.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn karac_in(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

fn new_package(tag: &str, kind: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "karac-package-check-tests-{tag}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let new = karac_in(&dir, &["new", "pkg", kind]);
    assert!(new.status.success(), "{}", text(&new));
    dir.join("pkg")
}

const BAD_TEST: &str =
    "\ntest \"mistyped\" {\n    let n: i64 = \"seven\";\n    assert(n > 0);\n}\n";

#[test]
fn karac_check_reports_a_type_error_in_a_lone_files_test() {
    let proj = new_package("lone", "--cli");
    let clean = karac_in(&proj, &["check"]);
    assert!(clean.status.success(), "{}", text(&clean));
    let path = proj.join("src/main_test.kara");
    let mut src = std::fs::read_to_string(&path).unwrap();
    src.push_str(BAD_TEST);
    std::fs::write(&path, src).unwrap();
    let out = karac_in(&proj, &["check"]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    assert!(
        text(&out).contains("src/main_test.kara") && !text(&out).contains("All checks passed"),
        "{}",
        text(&out)
    );
    let json = karac_in(&proj, &["check", "--output=json"]);
    assert_eq!(json.status.code(), Some(1), "{}", text(&json));
    let stdout = String::from_utf8_lossy(&json.stdout);
    assert!(
        stdout.starts_with("{\"diagnostics\":[{")
            && stdout.contains("\"file\":\"src/main_test.kara\""),
        "{stdout}"
    );
}

#[test]
fn karac_check_reports_a_type_error_in_a_modules_test() {
    let proj = new_package("modules", "--cli");
    std::fs::write(
        proj.join("src/util.kara"),
        "pub fn twice(n: i64) -> i64 {\n    n * 2\n}\n",
    )
    .unwrap();
    std::fs::write(
        proj.join("src/util_test.kara"),
        format!("test \"twice\" {{\n    assert_eq(twice(2), 4);\n}}\n{BAD_TEST}"),
    )
    .unwrap();
    let main = std::fs::read_to_string(proj.join("src/main.kara")).unwrap();
    std::fs::write(
        proj.join("src/main.kara"),
        format!("import util.twice;\n\n{main}"),
    )
    .unwrap();
    let out = karac_in(&proj, &["check"]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    assert!(
        text(&out).contains("src/util_test.kara") && !text(&out).contains("All checks passed"),
        "{}",
        text(&out)
    );
    // Without the bad case the package, tests included, is clean.
    std::fs::write(
        proj.join("src/util_test.kara"),
        "test \"twice\" {\n    assert_eq(twice(2), 4);\n}\n",
    )
    .unwrap();
    let ok = karac_in(&proj, &["check"]);
    assert!(ok.status.success(), "{}", text(&ok));
}

#[test]
fn a_resolve_error_in_a_test_file_names_the_test_file() {
    // A `_test.kara` is merged into its sibling's module, and its spans used
    // to overlap the sibling's: the error carried the test file's line and
    // the production file's name (B-2026-10-31-1).
    let proj = new_package("resolve", "--cli");
    let path = proj.join("src/main_test.kara");
    let mut src = std::fs::read_to_string(&path).unwrap();
    let line = src.lines().count() + 2;
    src.push_str("\ntest \"misspelled\" {\n    assert(undefined_fn(1));\n}\n");
    std::fs::write(&path, src).unwrap();
    let check = karac_in(&proj, &["check"]);
    assert_eq!(check.status.code(), Some(1), "{}", text(&check));
    let want = format!(
        "src/main_test.kara:{}:12: undefined name 'undefined_fn'",
        line + 1
    );
    assert!(
        text(&check).contains(&want),
        "want {want:?} in {}",
        text(&check)
    );
    let test = karac_in(&proj, &["test"]);
    let want = format!(
        "\"file\":\"src/main_test.kara\",\"line\":{},\"column\":12",
        line + 1
    );
    assert!(
        text(&test).contains(&want),
        "want {want:?} in {}",
        text(&test)
    );
}
