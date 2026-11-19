//! Kata 2263's Rust template returns `ans` from a closure: `let cost =
//! |v: &Vec<i32>| -> i32 { let mut ans = 0; .. ans += t - x; .. ans };`.
//! `karac fix` deletes the closure's `-> i32`, and in Kāra `ans` keeps
//! the default `i64` (design.md §8 Literals), so the closure returns an
//! `i64` and the `i32` tail that calls it is a narrowing. The error
//! landed at the call as a bare narrowing with no fix, where the same
//! `ans` in a function's own tail is explained and fixed as `ans: i32`
//! (B-2026-11-18-80). A call of a closure bound by `let` now reads the literal
//! binding its body ends in.

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
fn kata_2263_closure_tail_literal_gets_the_annotation() {
    let src = "fn total(v: Vec[i32]) -> i32 {
    let sum_of = |w: ref Vec[i32]| {
        let mut ans = 0;
        for x in w {
            ans += x;
        }
        ans
    };
    sum_of(v).min(sum_of(v))
}

fn main() {
    let v: Vec[i32] = [1i32, 2, 3].to_vec();
    println(f\"{total(v)}\");
}
";
    let file = write("k2263-closure-tail", src);
    let check = karac(&["check", &file]);
    let stderr = String::from_utf8_lossy(&check.stderr).to_string();
    assert!(
        stderr.contains("`ans` is an 'i64' because its literal kept the default")
            && stderr.contains("declare it `ans: i32`"),
        "{stderr}"
    );
    let fix = karac(&["fix", &file]);
    assert!(
        fix.status.success(),
        "{}",
        String::from_utf8_lossy(&fix.stderr)
    );
    let fixed = std::fs::read_to_string(&file).unwrap();
    let _ = std::fs::remove_file(&file);
    assert!(fixed.contains("        let mut ans: i32 = 0;\n"), "{fixed}");
    let out = karac::mir::lower::run_source(&fixed)
        .expect("MIR interpreter")
        .output;
    assert_eq!(out, "6\n");
}

#[test]
fn kata_2263_closure_tail_of_a_parameter_is_not_annotated() {
    // The closure returns its own `i64` parameter: there is no literal
    // binding to type, so the narrowing stays the plain one.
    let src = "fn pick(n: i64) -> i32 {
    let same = |k: i64| {
        let ans = k;
        ans
    };
    same(n)
}

fn main() {
    println(f\"{pick(4)}\");
}
";
    let file = write("k2263-closure-param", src);
    let check = karac(&["check", &file]);
    let _ = std::fs::remove_file(&file);
    let stderr = String::from_utf8_lossy(&check.stderr).to_string();
    assert!(!check.status.success(), "{stderr}");
    assert!(stderr.contains("would narrow"), "{stderr}");
    assert!(!stderr.contains("declare it `ans: i32`"), "{stderr}");
}
