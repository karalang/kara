//! B-2026-11-26-92: `karac query affected-by` lists the `test` blocks that
//! reach the seed, those of the seed's sibling `_test.kara` (the file
//! `karac test` runs with it as one module) included. Before the fix only
//! a `test_*`-named function in a `_test.kara` file counted, which v1 has
//! none of, so `tests` was always empty.

use std::process::Command;

fn dir(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("karac_affby_tests_{}_{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn affected_by(target: &std::path::Path, spec: &str, extra: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["query", "affected-by"])
        .arg(format!("{}{spec}", target.to_str().unwrap()))
        .args(extra)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn affected_by_lists_the_sibling_test_files_blocks_that_reach_the_seed() {
    let d = dir("sibling");
    let lib = d.join("lib.kara");
    std::fs::write(
        &lib,
        "pub fn add(a: i64, b: i64) -> i64 {\n    a + b\n}\npub fn twice(a: i64) -> i64 {\n    add(a, a)\n}\npub fn other() -> i64 {\n    0\n}\n",
    )
    .unwrap();
    std::fs::write(
        d.join("lib_test.kara"),
        "test \"add sums\" {\n    assert_eq(add(2, 3), 5);\n}\n\ntest \"twice doubles\" {\n    assert_eq(twice(2), 4);\n}\n\ntest \"other is zero\" {\n    assert_eq(other(), 0);\n}\n",
    )
    .unwrap();
    let out = affected_by(&lib, ":add", &["--tests-only"]);
    let _ = std::fs::remove_dir_all(&d);
    assert!(out.contains("\"fn\":\"test \\\"add sums\\\"\""), "{out}");
    // Transitively, through `twice`.
    assert!(
        out.contains("\"fn\":\"test \\\"twice doubles\\\"\""),
        "{out}"
    );
    assert!(out.contains("lib_test.kara\",\"line\":5"), "{out}");
    assert!(!out.contains("other is zero"), "{out}");
}

#[test]
fn affected_by_without_a_sibling_test_file_lists_no_tests() {
    let d = dir("alone");
    let lib = d.join("lib.kara");
    std::fs::write(&lib, "pub fn add(a: i64, b: i64) -> i64 {\n    a + b\n}\n").unwrap();
    let out = affected_by(&lib, ":add", &[]);
    let _ = std::fs::remove_dir_all(&d);
    assert!(out.ends_with("\"tests\":[]}"), "{out}");
}

#[test]
fn query_effects_on_a_test_file_still_lists_only_functions() {
    // The test nodes are affected-by's: the effect report lists functions.
    let d = dir("effects");
    let t = d.join("m_test.kara");
    std::fs::write(
        &t,
        "fn helper() -> i64 {\n    1\n}\ntest \"h\" {\n    assert(helper() == 1);\n}\n",
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["query", "effects", t.to_str().unwrap()])
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&d);
    let out = String::from_utf8_lossy(&out.stdout);
    assert!(out.contains("\"function\":\"helper\""), "{out}");
    assert!(!out.contains("test \\\"h\\\""), "{out}");
}
