//! Found writing kata 2744 (Find Maximum Number of String Pairs).
//!
//! B-2026-11-20-141: Rust's `["cd", "ac"].map(|w| w.to_string()).to_vec()`. `karac
//! fix` already made the array's `.map` an iterator's,
//! `.into_iter().map(..)`, and then `.to_vec()` on that iterator was
//! refused, "no method 'to_vec' on type 'IterMap'", with no fix. An
//! iterator's items become a `Vec` by `collect()`, which the fix now
//! writes.

use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

static N: AtomicUsize = AtomicUsize::new(0);

fn karac(cmd: &str, src: &str) -> (String, String) {
    let n = N.fetch_add(1, Ordering::SeqCst);
    let path =
        std::env::temp_dir().join(format!("karac-iter-to-vec-{}-{n}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args([cmd, path.to_str().unwrap()])
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let after = std::fs::read_to_string(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    (
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr),
        after,
    )
}

fn run(src: &str) -> String {
    karac::mir::lower::run_source(src)
        .expect("MIR interpreter")
        .output
}

const RUST_ARRAY_MAP: &str = "fn count(words: Vec[String]) -> i64 {
    words.len()
}

fn main() {
    println(f\"{count([\"cd\", \"ac\"].map(|w| w.to_string()).to_vec())}\");
    let doubled: Vec[i64] = [1, 2, 3].iter().map(|x| 2 * x).to_vec();
    println(f\"{doubled}\");
}
";

const FIXED: &str = "fn count(words: Vec[String]) -> i64 {
    words.len()
}

fn main() {
    println(f\"{count([\"cd\", \"ac\"].into_iter().map(|w| w.to_string()).collect())}\");
    let doubled: Vec[i64] = [1, 2, 3].iter().map(|x| 2 * x).collect();
    println(f\"{doubled}\");
}
";

#[test]
fn iterator_to_vec_names_collect() {
    let (out, _) = karac("check", FIXED.replace("collect()", "to_vec()").as_str());
    assert!(
        out.contains("no method 'to_vec' on type 'IterMap', an iterator"),
        "{out}"
    );
    assert!(out.contains("become a `Vec` by `collect()`"), "{out}");
}

#[test]
fn iterator_to_vec_fix_writes_collect() {
    // Two passes: the array's `.map` first, then the iterator's `.to_vec()`.
    let n = N.fetch_add(1, Ordering::SeqCst);
    let path = std::env::temp_dir().join(format!(
        "karac-iter-to-vec-fix-{}-{n}.kara",
        std::process::id()
    ));
    std::fs::write(&path, RUST_ARRAY_MAP).unwrap();
    for _ in 0..2 {
        Command::new(env!("CARGO_BIN_EXE_karac"))
            .args(["fix", path.to_str().unwrap()])
            .stdin(Stdio::null())
            .output()
            .expect("karac");
    }
    let after = std::fs::read_to_string(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    assert_eq!(after, FIXED);
    let (out, _) = karac("check", FIXED);
    assert!(out.contains("All checks passed"), "{out}");
}

#[test]
fn iterator_to_vec_fixed_runs() {
    assert_eq!(run(FIXED), "2\n[2, 4, 6]\n");
}

#[test]
fn vec_to_vec_unchanged() {
    let src = "fn main() {
    let v: Vec[i64] = [1, 2].to_vec();
    let w = v.to_vec();
    println(f\"{w}\");
}
";
    assert_eq!(run(src), "[1, 2]\n");
}
