//! Found writing kata 1146's Rust arm (B-2026-11-13-6, B-2026-11-13-7).
//!
//! LeetCode's Rust for Snapshot Array finds a place's value at a snap with
//! `match h.binary_search_by_key(&snap_id, |&(s, _)| s) { Ok(i) => ..,
//! Err(i) => .. }`. Kāra has no `binary_search_by_key` or
//! `binary_search_by`, and `karac check` said only "no method
//! 'binary_search_by_key' on type 'Vec'", where the same shapes over
//! `binary_search` are carried through `partition_point`. Now they are
//! too. And `Vec.filled(length as i64, ..)`, left once `length as usize`
//! became `as i64`, widens an `i32` for nothing, which `karac check` says
//! at a user function's `i64` parameter but did not at a library one's.

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
        std::env::temp_dir().join(format!("karac-kata-1146-{tag}-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    path
}

fn wrap(body: &str) -> String {
    format!(
        "fn f() -> i64 {{\n    let v: Vec[(i64, i64)] = [(1, 10), (3, 30), (5, 50)];\n    {body}\n}}\n\nfn main() {{\n    println(f\"{{f()}}\");\n}}\n"
    )
}

/// What `karac check` said before each round of `karac fix` (the Rust
/// syntax goes first, then the types), the source once `karac fix` has
/// nothing left to apply, `karac check`'s word on that, and what it prints.
fn check_fix_run(tag: &str, src: &str) -> (String, String, String, String) {
    let path = temp(tag, src);
    let mut err = String::new();
    for _ in 0..4 {
        err += &String::from_utf8_lossy(&karac(&["check"], &path).stderr);
        let _ = karac(&["fix"], &path);
    }
    let fixed = std::fs::read_to_string(&path).unwrap();
    let last = karac(&["check"], &path);
    let after =
        String::from_utf8_lossy(&last.stdout).into_owned() + &String::from_utf8_lossy(&last.stderr);
    let out = String::from_utf8_lossy(&karac(&["__mir-run"], &path).stdout).into_owned();
    let _ = std::fs::remove_file(&path);
    (err, fixed, after, out)
}

#[test]
fn a_rust_binary_search_by_key_or_by_taken_apart_is_carried_through_partition_point() {
    let shapes = [
        (
            "key-match",
            "let k = 3; match v.binary_search_by_key(&k, |&(s, _)| s) { Ok(i) => v[i].1, Err(i) => -(i as i64) }",
            "binary_search_by_key",
            "30\n",
        ),
        (
            "key-missing",
            "match v.binary_search_by_key(&4, |&(s, _)| s) { Ok(i) => v[i].1, Err(i) => v[i - 1].1 }",
            "binary_search_by_key",
            "30\n",
        ),
        (
            "key-unwrap",
            "v.binary_search_by_key(&4, |&(s, _)| s).unwrap_or_else(|i| i)",
            "binary_search_by_key",
            "2\n",
        ),
        (
            "by-match",
            "match v.binary_search_by(|p| p.0.cmp(&5)) { Ok(i) => i, Err(_) => 99 }",
            "binary_search_by",
            "2\n",
        ),
        (
            "by-if-let",
            "if let Err(i) = v.binary_search_by(|p| p.0.cmp(&0)) { i + 7 } else { 77 }",
            "binary_search_by",
            "7\n",
        ),
    ];
    for (tag, body, method, want) in shapes {
        let (err, fixed, after, out) = check_fix_run(tag, &wrap(body));
        assert!(
            err.contains(&format!("Kāra has no `{method}`")) && err.contains("partition_point"),
            "{tag}: {err}"
        );
        assert!(
            fixed.contains(".partition_point(") && !fixed.contains("binary_search"),
            "{tag}: {fixed}"
        );
        assert!(
            after.contains("All checks passed"),
            "{tag}: {after}\n{fixed}"
        );
        assert_eq!(out, want, "{tag}: {fixed}");
    }
}

#[test]
fn a_rust_binary_search_by_key_against_a_call_is_explained_not_rewritten() {
    let (err, fixed, _, _) = check_fix_run(
        "key-impure",
        &wrap("match v.binary_search_by_key(&v.len(), |&(s, _)| s) { Ok(i) => i, Err(i) => i }"),
    );
    assert!(
        err.contains("Kāra has no `binary_search_by_key`")
            && err.contains("names the value more than once"),
        "{err}"
    );
    assert!(fixed.contains("binary_search_by_key"), "{fixed}");
}

#[test]
fn a_widening_cast_at_a_library_i64_argument_is_dropped() {
    let src = "fn f(n: i32) -> i64 {\n    let v: Vec[i64] = Vec.filled(n as i64, 4);\n    v.len() + v[0]\n}\n\nfn main() {\n    println(f\"{f(3)}\");\n}\n";
    let (err, fixed, after, out) = check_fix_run("filled", src);
    assert!(
        err.contains("unnecessary cast: 'i32' widens to 'i64' implicitly here"),
        "{err}"
    );
    assert!(fixed.contains("Vec.filled(n, 4)"), "{fixed}");
    assert!(after.contains("All checks passed"), "{after}");
    assert_eq!(out, "7\n");
}

#[test]
fn a_narrowing_or_needed_cast_at_a_library_i64_argument_stays() {
    // `u64` to `i64` can lose values, so its `as` is needed.
    let src = "fn f(n: u64) -> i64 {\n    let v: Vec[i64] = Vec.filled(n as i64, 4);\n    v.len()\n}\n\nfn main() {\n    println(f\"{f(3)}\");\n}\n";
    let (err, fixed, _, out) = check_fix_run("filled-u64", src);
    assert!(!err.contains("unnecessary cast"), "{err}");
    assert!(fixed.contains("Vec.filled(n as i64, 4)"), "{fixed}");
    assert_eq!(out, "3\n");
}
