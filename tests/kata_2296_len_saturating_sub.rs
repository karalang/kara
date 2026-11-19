//! Kata 2296's Rust template took its tail as `let from =
//! self.left.len().saturating_sub(10); self.left[from..]`. It passed
//! `karac check` and `karac fix`, then failed at run time with an index
//! out of bounds: a length is an `i64`, so `saturating_sub` clamps at
//! `i64.MIN`, not at 0 as Rust's `usize` does. The `len_saturating_sub`
//! lint now names it on a `len()` / `count()` receiver and `karac fix`
//! writes `(x.len() - n).max(0)` (B-2026-11-18-88).

use std::process::{Command, Stdio};

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn write(name: &str, src: &str) -> String {
    let path = std::env::temp_dir().join(format!("karac-{name}-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    path.to_str().unwrap().to_string()
}

#[test]
fn kata_2296_len_saturating_sub_is_named_and_fixed_to_a_floor_at_zero() {
    let src = "fn tail(left: Vec[char]) -> String {
    let from = left.len().saturating_sub(10);
    left[from..].iter().collect()
}

fn main() {
    let v = [1, 2, 3].to_vec();
    let k = 2;
    let n = v.iter().count().saturating_sub(k + 8);
    println(f\"{tail(['a', 'b', 'c'].to_vec())} {n}\");
}
";
    let file = write("k2296-len-sat", src);
    let check = karac(&["check", &file]);
    let stderr = String::from_utf8_lossy(&check.stderr).to_string()
        + &String::from_utf8_lossy(&check.stdout);
    assert_eq!(
        stderr.matches("warning[len_saturating_sub]").count(),
        2,
        "{stderr}"
    );
    assert!(stderr.contains("clamps at `i64.MIN`, not at 0"), "{stderr}");
    let fix = karac(&["fix", &file]);
    assert!(
        fix.status.success(),
        "{}",
        String::from_utf8_lossy(&fix.stderr)
    );
    let fixed = std::fs::read_to_string(&file).unwrap();
    let _ = std::fs::remove_file(&file);
    assert!(
        fixed.contains("let from = (left.len() - 10).max(0);")
            && fixed.contains("let n = (v.iter().count() - (k + 8)).max(0);"),
        "{fixed}"
    );
    let out = karac::mir::lower::run_source(&fixed)
        .expect("MIR interpreter")
        .output;
    assert_eq!(out, "abc 0\n");
}

#[test]
fn kata_2296_saturating_sub_on_a_plain_i64_is_left_alone() {
    let src = "fn main() {
    let x: i64 = 5;
    println(x.saturating_sub(10));
}
";
    let file = write("k2296-plain-sat", src);
    let check = karac(&["check", &file]);
    let _ = std::fs::remove_file(&file);
    let all = String::from_utf8_lossy(&check.stderr).to_string()
        + &String::from_utf8_lossy(&check.stdout);
    assert!(!all.contains("len_saturating_sub"), "{all}");
}
