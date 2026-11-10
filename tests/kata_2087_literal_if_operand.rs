//! Kata 2087's Rust arm, LeetCode's template, walks the robot home with
//! `r += if hr > r { 1 } else { -1 };` over `r: i32`, and kata 2055's adds
//! `prefix[i] + if b[i] == b'*' { 1 } else { 0 }` into a `Vec[i32]`. An
//! `if`, `match` or block whose every value is an unsuffixed literal was
//! typed `i64` at once, so the operator widened and the store was refused
//! as narrowing (B-2026-11-10-6). As an operand it now takes the other
//! operand's numeric type, as a literal written there does.

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
fn kata_2087_literal_if_takes_the_other_operands_type() {
    let src = "fn walk(from: i32, to: i32) -> i32 {
    let mut r = from;
    let mut steps: i32 = 0;
    while r != to {
        r += if to > r { 1 } else { -1 };
        steps = steps + 1;
    }
    r * 10 + steps
}

fn main() {
    let b = \"*|**|\".bytes();
    let mut prefix: Vec[i32] = Vec.filled(b.len() + 1, 0);
    for i in 0..b.len() {
        prefix[i + 1] = prefix[i] + if b[i] == b'*' { 1 } else { 0 };
    }
    let mut small: u8 = 250;
    small -= match prefix[5] { 3 => 7, _ => 0 };
    let f: f32 = 1.5;
    let g = f * if small > 200 { 2.0 } else { 0.5 };
    let h: i32 = 6;
    let k = h * { 4 };
    let near = h < if g > 1.0 { 9 } else { 0 };
    println(f\"{walk(2, -3)} {walk(0, 4)} {prefix} {small} {g} {k} {near}\");
}
";
    let file = write("k2087-lit-if", src);
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
    assert_eq!(out, "-25 44 [0, 1, 1, 2, 3, 3] 243 3.0 24 true\n");
}

/// An `if` of literals beside an `i64` is unchanged, and one beside a
/// narrower type whose literal does not fit it is still refused.
#[test]
fn kata_2087_literal_if_beside_i64_and_unfit() {
    let src = "fn main() {
    let c = true;
    let k: i64 = 4;
    let n = k + if c { 1 } else { 2 };
    println(f\"{n}\");
}
";
    let out = karac::mir::lower::run_source(src)
        .expect("MIR interpreter")
        .output;
    assert_eq!(out, "5\n");
    let bad = "fn main() {
    let c = true;
    let mut b: u8 = 1;
    b += if c { 300 } else { 2 };
    println(f\"{b}\");
}
";
    let file = write("k2087-lit-if-unfit", bad);
    let check = karac(&["check", &file]);
    let _ = std::fs::remove_file(&file);
    assert!(!check.status.success(), "300 into a u8 should be refused");
}
