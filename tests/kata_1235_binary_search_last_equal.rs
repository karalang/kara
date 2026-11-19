//! Kata 1235's search arm: Rust's `binary_search` family names the LAST of
//! several equal elements in `Ok` (rustc's implementation, and Kāra's own
//! `binary_search`), and a carried program over repeated keys depends on it.
//! The `karac fix` rewrite took the first (B-2026-11-19-32).

use std::process::Command;

fn karac() -> &'static str {
    env!("CARGO_BIN_EXE_karac")
}

fn check_fix_run(tag: &str, src: &str) -> (String, String, String, String) {
    let dir = std::env::temp_dir().join(format!("kata1235_bs_{}_{}", tag, std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("m.kara");
    std::fs::write(&path, src).unwrap();
    let run = |args: &[&str]| {
        let o = Command::new(karac())
            .args(args)
            .arg(&path)
            .output()
            .unwrap();
        format!(
            "{}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        )
    };
    let err = run(&["check"]);
    run(&["fix"]);
    let fixed = std::fs::read_to_string(&path).unwrap();
    let after = run(&["check"]);
    let out = Command::new(karac())
        .args(["__mir-run"])
        .arg(&path)
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&out.stdout).to_string();
    let _ = std::fs::remove_dir_all(&dir);
    (err, fixed, after, out)
}

/// `at` over a list with a run of three equal keys, at every value.
fn wrap(body: &str) -> String {
    format!(
        "fn at(v: Vec[i64], x: i64) -> i64 {{\n    {body}\n}}\n\nfn main() {{\n    let v = [1, 3, 3, 3, 5];\n    let got: Vec[i64] = (0..7).map(|x| at(v, x)).collect();\n    println(f\"{{got}}\");\n}}\n"
    )
}

/// Rust's answers for `x` in 0..7 over `[1, 3, 3, 3, 5]`.
const LAST_AT_MOST: &str = "[-1, 0, 0, 3, 3, 4, 4]\n";
const EITHER: &str = "[0, 0, 1, 3, 4, 4, 5]\n";

#[test]
fn kata_1235_binary_search_ok_names_the_last_equal_element() {
    for (tag, body, want) in [
        (
            "match",
            "match v.binary_search(x) { Ok(i) => i, Err(i) => i - 1 }",
            LAST_AT_MOST,
        ),
        (
            "either",
            "match v.binary_search(x) { Ok(i) => i, Err(i) => i }",
            EITHER,
        ),
        ("unwrap", "v.binary_search(x).unwrap_or_else(|i| i)", EITHER),
        (
            "by",
            "match v.binary_search_by(|y| y.cmp(x)) { Ok(i) => i, Err(i) => i - 1 }",
            LAST_AT_MOST,
        ),
        (
            "by_key",
            "match v.binary_search_by_key(x, |y| y) { Ok(i) => i, Err(i) => i - 1 }",
            LAST_AT_MOST,
        ),
    ] {
        let (err, fixed, after, out) = check_fix_run(tag, &wrap(body));
        assert!(err.contains("binary_search"), "{tag}: {err}");
        assert!(!fixed.contains("binary_search"), "{tag}: {fixed}");
        assert!(
            after.contains("All checks passed"),
            "{tag}: {after}\n{fixed}"
        );
        assert_eq!(out, want, "{tag}: {fixed}");
    }
}

/// The two sides alike after the rewrite (`Ok(i) => i`, `Err(i) => i - 1`
/// both name the run's last index) give one branch, with no test.
#[test]
fn kata_1235_binary_search_sides_alike_after_the_rewrite_drop_the_test() {
    let (_, fixed, after, out) = check_fix_run(
        "alike",
        &wrap("match v.binary_search(x) { Ok(i) => i, Err(i) => i - 1 }"),
    );
    assert!(!fixed.contains(" else "), "{fixed}");
    assert!(after.contains("All checks passed"), "{after}\n{fixed}");
    assert_eq!(out, LAST_AT_MOST, "{fixed}");
}
