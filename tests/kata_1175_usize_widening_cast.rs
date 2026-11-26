//! Kata 1175's sieve arm, a Rust solution over `usize`, ran into two of
//! `karac fix`'s edits that cannot both hold. `let n = n as usize;` becomes
//! `let n = n as i64;`, so the count and the loop variable `i` become `i64`
//! too; but in the same round `ans * i as u64` over the old `usize` `i` was
//! flagged `lossless_cast` ("`usize` widens to `u64`") and its `as u64`
//! deleted. With `i` an `i64` the product mixed `u64` and `i64`, and nothing
//! offered a fix. A `usize` or `isize` operand that the pointer-width pass
//! makes `i64` is no longer flagged lossless. B-2026-11-16-205.

use std::path::Path;
use std::process::{Command, Stdio};

/// Runs `karac` on `cell.kara` in `dir`: its verdict, its output, and the
/// file as it stands after.
fn karac(dir: &Path, args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg("cell.kara")
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let fixed = std::fs::read_to_string(dir.join("cell.kara")).unwrap_or_default();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr),
        fixed,
    )
}

fn cell(tag: &str, src: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-kata-1175-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("cell.kara"), src).unwrap();
    dir
}

/// One `karac fix` round over `src`, then a second that must find nothing,
/// then `__mir-run`: the source after the one round, and what it prints.
fn one_round(tag: &str, src: &str) -> (String, String) {
    let dir = cell(tag, src);
    let (_, out, fixed) = karac(&dir, &["fix"]);
    assert!(out.contains("applied"), "{tag}: {out}");
    let (_, out, again) = karac(&dir, &["fix"]);
    assert_eq!(
        again, fixed,
        "{tag}: a second round changed the source: {out}"
    );
    let (ok, out, _) = karac(&dir, &["check"]);
    assert!(ok, "{tag}: {out}\n{fixed}");
    let (ok, out, _) = karac(&dir, &["__mir-run"]);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(ok, "{tag}: {out}\n{fixed}");
    (fixed, out)
}

#[test]
fn kata_1175_usize_cast_to_u64_survives_the_usize_fix() {
    let (fixed, out) = one_round(
        "usize",
        r#"fn arrangements(n: i32) -> i64 {
    let n = n as usize;
    let mut evens: i64 = 0;
    for i in 2..=n {
        if i % 2 == 0 {
            evens += 1;
        }
    }
    let m = 1_000_000_007u64;
    let mut ans = 1u64;
    for i in 1..=evens {
        ans = ans * i as u64 % m;
    }
    for i in 1..=(n - evens) {
        ans = ans * i as u64 % m;
    }
    ans as i64
}

fn main() {
    println(f"{arrangements(7)}");
}
"#,
    );
    assert!(fixed.contains("let n = n as i64;"), "{fixed}");
    assert_eq!(
        fixed.matches("ans = ans * i as u64 % m;").count(),
        2,
        "{fixed}"
    );
    assert_eq!(out.trim(), "144");
}

/// The kata's own arm, with `evens` unannotated: the count sits beside the
/// `usize` `n` in `n - evens`, so its literal takes `usize` (design.md §8
/// Literals) and nothing needs a fix. Before B-2026-11-26-1 the `i as u64`
/// over `1..=evens` settled the count to `i64` early, and the arm needed
/// the `usize` fix above; the test above annotates `evens: i64` to keep
/// that path.
#[test]
fn kata_1175_unannotated_count_takes_usize_and_checks_clean() {
    let dir = cell(
        "usize-clean",
        r#"fn arrangements(n: i32) -> i64 {
    let n = n as usize;
    let mut evens = 0;
    for i in 2..=n {
        if i % 2 == 0 {
            evens += 1;
        }
    }
    let m = 1_000_000_007u64;
    let mut ans = 1u64;
    for i in 1..=evens {
        ans = ans * i as u64 % m;
    }
    for i in 1..=(n - evens) {
        ans = ans * i as u64 % m;
    }
    ans as i64
}

fn main() {
    println(f"{arrangements(7)}");
}
"#,
    );
    let (ok, out, _) = karac(&dir, &["check"]);
    assert!(ok, "{out}");
    let (ok, out, _) = karac(&dir, &["__mir-run"]);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(ok, "{out}");
    assert_eq!(out.trim(), "144");
}

/// A widening that no fix retypes is still flagged, with its fix.
#[test]
fn kata_1175_u32_cast_to_u64_is_still_lossless() {
    let (fixed, out) = one_round(
        "u32",
        r#"fn total(x: u32) -> u64 {
    let y: u64 = x as u64;
    y + 1
}

fn main() {
    println(f"{total(41)}");
}
"#,
    );
    assert!(fixed.contains("let y: u64 = x;"), "{fixed}");
    assert_eq!(out.trim(), "42");
}
