//! Kata 2119's Rust arm reverses a number as text:
//! `n.to_string().chars().rev().collect::<String>().parse::<i32>()`.
//! `karac fix` moved the `parse` first, to `i32.parse(...collect::<String>())`,
//! and then deleted the `collect`'s list, which left a `collect` into the
//! `Str` that `parse` reads: "an expression whose type does not lower".
//! A named `collect` that is a receiver is now rewritten as
//! `String.from_iter(..)`, and the `parse` on it waits for that round.

use std::process::{Command, Stdio};

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

#[test]
fn kata_2119_collect_then_parse_is_carried_by_fix() {
    let path = std::env::temp_dir().join(format!("karac-k2119-{}.kara", std::process::id()));
    std::fs::write(
        &path,
        "fn main() {
    let n = 1230;
    let back = n.to_string().chars().rev().collect::<String>().parse::<i64>().unwrap();
    println(f\"{back}\");
}
",
    )
    .unwrap();
    let file = path.to_str().unwrap();
    for _ in 0..3 {
        let fix = karac(&["fix", file]);
        assert!(
            fix.status.success(),
            "{}",
            String::from_utf8_lossy(&fix.stderr)
        );
    }
    let fixed = std::fs::read_to_string(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    assert!(
        fixed.contains("i64.parse(String.from_iter(n.to_string().chars().rev())).unwrap()"),
        "{fixed}"
    );
    let out = karac::mir::lower::run_source(&fixed)
        .expect("MIR interpreter")
        .output;
    assert_eq!(out, "321\n");
}
