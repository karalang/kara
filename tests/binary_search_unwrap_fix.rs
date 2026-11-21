//! Found writing kata 1357's search arm (B-2026-11-21-61).
//!
//! A price looked up in pairs sorted by product, as Rust writes it:
//! `catalogue.binary_search_by_key(&id, |&(p, _)| p).unwrap()`. Kāra has
//! no `binary_search_by_key`, and the `match`, `if let` and
//! `unwrap_or_else` shapes over it were carried through `partition_point`
//! (B-2026-11-13-6), but `.unwrap()` and `.expect(..)` were not: `karac
//! check` said only "no method 'binary_search_by_key' on type 'Vec'"
//! beside "tuple pattern used but type is `?T7`", and `karac fix` had
//! nothing to apply. Now they take the last equal element, as Rust's
//! `Ok` does, and panic as Rust's `unwrap` does when there is none.

use std::process::{Command, Output, Stdio};

fn karac(args: &[&str], path: &std::path::Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn temp(tag: &str, src: &str) -> std::path::PathBuf {
    let path =
        std::env::temp_dir().join(format!("karac-bs-unwrap-{tag}-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    path
}

fn program(search: &str, id: i64) -> String {
    format!(
        "fn main() {{\n    let catalogue: Vec[(i32, i32)] = [(1, 10), (3, 30), (3, 31), (5, 50)];\n    let id: i32 = {id};\n    let at = {search};\n    println(catalogue[at].1);\n}}\n"
    )
}

/// What `karac check` said first, the source once `karac fix` has run,
/// whether it then checks, and what `karac run` printed and exited with.
fn check_fix_run(tag: &str, src: &str) -> (String, String, bool, String, bool) {
    let path = temp(tag, src);
    let before = String::from_utf8_lossy(&karac(&["check"], &path).stderr).into_owned();
    let _ = karac(&["fix"], &path);
    let fixed = std::fs::read_to_string(&path).unwrap();
    let checked = karac(&["check"], &path).status.success();
    let run = karac(&["run"], &path);
    let out = String::from_utf8_lossy(&run.stdout).into_owned();
    let _ = std::fs::remove_file(&path);
    (before, fixed, checked, out, run.status.success())
}

#[test]
fn a_binary_search_by_key_unwrapped_is_carried_through_partition_point() {
    let (before, fixed, checked, out, ok) = check_fix_run(
        "key",
        &program(
            "catalogue.binary_search_by_key(&id, |&(p, _)| p).unwrap()",
            5,
        ),
    );
    assert!(!before.contains("tuple pattern used"), "{before}");
    assert!(!fixed.contains("binary_search_by_key"), "{fixed}");
    assert!(fixed.contains("partition_point"), "{fixed}");
    assert!(checked, "{fixed}");
    assert!(ok, "{fixed}");
    assert_eq!(out, "50\n");
}

#[test]
fn a_binary_search_by_key_unwrapped_takes_the_last_equal_element() {
    let (_, fixed, checked, out, _) = check_fix_run(
        "last",
        &program(
            "catalogue.binary_search_by_key(&id, |&(p, _)| p).unwrap()",
            3,
        ),
    );
    assert!(checked, "{fixed}");
    assert_eq!(out, "31\n", "{fixed}");
}

#[test]
fn a_binary_search_by_key_unwrapped_on_a_missing_key_panics() {
    let (_, fixed, checked, out, ok) = check_fix_run(
        "missing",
        &program(
            "catalogue.binary_search_by_key(&id, |&(p, _)| p).unwrap()",
            4,
        ),
    );
    assert!(checked, "{fixed}");
    assert!(!ok, "{fixed}\n{out}");
    assert_eq!(out, "");
}

#[test]
fn a_binary_search_by_expected_keeps_its_message() {
    let (_, fixed, checked, out, ok) = check_fix_run(
        "by",
        &program(
            "catalogue.binary_search_by(|&(p, _)| p.cmp(&id)).expect(\"no such product\")",
            1,
        ),
    );
    assert!(fixed.contains("panic(\"no such product\")"), "{fixed}");
    assert!(checked, "{fixed}");
    assert!(ok, "{fixed}");
    assert_eq!(out, "10\n");
}
