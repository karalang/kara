//! Kata 2108's Rust template finds a palindrome with
//! `w.chars().rev().collect::<String>() == *w`. Deleting the turbofish, the
//! fix every other turbofish gets, left `collect()` with nothing to fix
//! what it builds, since `==` takes its own right-hand type
//! (B-2026-10-29-36), and the template stopped at "cannot infer". The
//! turbofish of a `collect` that is an operand of `==` or `!=` now becomes
//! the receiver of the collection's `from_iter`, and `String.from_iter`,
//! which the checker typed but the MIR builder could not resolve, is the
//! library's `impl FromIterator[char] for String`.

use std::process::{Command, Stdio};

#[test]
fn kata_2108_compared_collect_turbofish_becomes_from_iter() {
    let src = "fn same(w: String) -> bool {
    w.chars().rev().collect::<String>() == *w
}
fn differs(v: Vec[i64]) -> bool {
    v != v.iter().map(|x| x + 1).collect::<Vec<i64>>()
}
fn main() {
    println(same(\"aba\".to_string()));
    println(same(\"ab\".to_string()));
    println(differs([1, 2].to_vec()));
}
";
    let path = std::env::temp_dir().join(format!("karac-k2108-tf-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    let file = path.to_str().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", file])
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        stderr.contains("a comparison does not fix what `collect` builds"),
        "{stderr}"
    );
    let fix = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["fix", file])
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    assert!(
        fix.status.success(),
        "{}",
        String::from_utf8_lossy(&fix.stderr)
    );
    let fixed = std::fs::read_to_string(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    assert!(
        fixed.contains("String.from_iter(w.chars().rev()) == *w"),
        "{fixed}"
    );
    assert!(
        fixed.contains("v != Vec[i64].from_iter(v.iter().map(|x| x + 1))"),
        "{fixed}"
    );
    let out = karac::mir::lower::run_source(&fixed)
        .expect("MIR interpreter")
        .output;
    assert_eq!(out, "true\nfalse\ntrue\n");
}

#[test]
fn kata_2108_string_from_iter_builds_the_string() {
    let src = "fn main() {
    let w = String.from_iter(\"abc\".chars().rev());
    println(w);
    println(String.from_iter(w.chars().filter(|c| c != 'b')).len());
}
";
    let out = karac::mir::lower::run_source(src)
        .expect("MIR interpreter")
        .output;
    assert_eq!(out, "cba\n2\n");
}
