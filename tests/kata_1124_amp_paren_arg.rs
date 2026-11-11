//! Kata 1124's solution arm, written as Rust, looks a score up with
//! `seen.get(&(score - 1))`. `karac fix` dropped the `&` and left
//! `seen.get((score - 1))`, parentheses that group nothing. With the `&`
//! gone, parentheses around a whole argument go too; a tuple keeps them.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn karac(dir: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "kata_1124_amp_paren_arg_{name}_{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The text `karac fix` leaves once it has nothing left to apply, and
/// what the program prints.
fn mend(name: &str, src: &str) -> (String, String) {
    let dir = scratch(name);
    let file = dir.join("m.kara");
    std::fs::write(&file, src).unwrap();
    for _ in 0..6 {
        karac(&dir, &["fix", "m.kara"]);
    }
    let text = std::fs::read_to_string(&file).unwrap();
    let (_, out) = karac(&dir, &["__mir-run", "m.kara"]);
    let _ = std::fs::remove_dir_all(&dir);
    (text, out)
}

#[test]
fn a_borrowed_parenthesized_argument_loses_its_parentheses() {
    let src = "fn main() {
    let mut seen: Map[i64, i64] = Map.new();
    seen[2] = 7;
    let score = 3;
    if let Some(j) = seen.get(&(score - 1)) {
        println(f\"{j}\");
    }
}
";
    let (text, out) = mend("get", src);
    assert!(text.contains("seen.get(score - 1)"), "{text}");
    assert_eq!(out, "7\n", "{text}");
}

#[test]
fn a_borrowed_tuple_argument_keeps_its_parentheses() {
    let src = "fn sum(p: (i64, i64), k: i64) -> i64 {
    p.0 + p.1 + k
}

fn main() {
    let a = 1;
    println(f\"{sum(&(a, 2), &(a + 3))}\");
}
";
    let (text, out) = mend("tuple", src);
    assert!(text.contains("sum((a, 2), a + 3)"), "{text}");
    assert_eq!(out, "7\n", "{text}");
}
