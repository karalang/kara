//! Kata 2057's Rust arm: LeetCode's template returns
//! `nums.iter().enumerate().find(..).map_or(-1, |(i, _)| i as i32)` from a
//! function that returns `i32`. A literal argument for the parameter a
//! generic call returns is left open for a later argument to fix (the
//! closure's `i32` here), but `-1` is a negation, not an `Integer` node, so
//! it was typed `i64` at once and the `i32` return was refused as
//! narrowing. `0` in its place checked.

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
fn kata_2057_negative_literal_default_takes_the_closures_type() {
    let src = "fn take(n: i32) -> i32 { n }

fn first(v: Vec[i64]) -> i32 {
    v.iter().enumerate().find(|(i, x)| i == x).map_or(-1, |(i, _)| i as i32)
}

fn main() {
    let o: Option[i64] = Some(3);
    let a: i32 = o.map_or(-1, |x| x as i32);
    let b = o.map_or(-2, |x| x as i32);
    let n: Option[i64] = None;
    let c: f32 = n.map_or(-0.5, |x| x as f32);
    println(f\"{a} {take(b)} {c} {first([3, 1, 2])} {first([4])}\");
}
";
    let file = write("k2057-neg", src);
    let check = karac(&["check", &file]);
    let _ = std::fs::remove_file(&file);
    assert!(
        check.status.success(),
        "{}",
        String::from_utf8_lossy(&check.stderr)
    );
    let out = karac::mir::lower::run_source(src)
        .expect("MIR interpreter")
        .output;
    assert_eq!(out, "3 3 -0.5 1 -1\n");
}

/// A negative literal for a parameter the call does not return is still
/// typed at the call, as a positive one is.
#[test]
fn kata_2057_negative_literal_unreturned_param_still_defaults() {
    let src = "fn show[T: Display](x: T) -> String { f\"{x}\" }

fn main() {
    let s = show(-7);
    println(s);
}
";
    let out = karac::mir::lower::run_source(src)
        .expect("MIR interpreter")
        .output;
    assert_eq!(out, "-7\n");
}
