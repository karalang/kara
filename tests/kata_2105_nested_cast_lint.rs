//! Kata 2105's Rust-template adapter returns `Solution.minimum_refill(..,
//! capacity_a as i32, capacity_b as i32) as i64` from a function
//! returning `i64`. The outer `as i64` is a lossless widening that
//! happens implicitly there, and kata 2103's adapter, with no cast among
//! its arguments, drew `lossless_cast` for it. Here it did not: the type
//! the return wants of the outer cast sat in a single slot, which the
//! argument's own cast, checked inside the outer one's operand, took
//! over. The slots are a stack now, so both adapters are linted alike.

use std::process::{Command, Stdio};

#[test]
fn kata_2105_outer_cast_with_a_cast_argument_is_linted() {
    let src = "fn one(c: i32) -> i32 {
    c + 1
}
fn wrap(c: i64) -> i64 {
    one(c as i32) as i64
}
fn twice(c: i64) -> i64 {
    let r: i64 = one(one(c as i32) as i32) as i64;
    r
}
fn main() {
    println(wrap(1));
    println(twice(1));
}
";
    let path = std::env::temp_dir().join(format!("karac-k2105-cast-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    let file = path.to_str().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", file])
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    // `one(c as i32) as i64` in `wrap`, and in `twice` the outer `as i64`
    // and the inner `as i32`, which is already the type it casts to.
    assert!(
        stderr.contains(":5:5: unnecessary cast: 'i32' widens to 'i64'"),
        "{stderr}"
    );
    assert!(
        stderr.contains(":8:18: unnecessary cast: 'i32' widens to 'i64'"),
        "{stderr}"
    );
    assert!(
        stderr.contains("redundant cast: the value is already 'i32'"),
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
        fixed.contains("    one(c as i32)\n") && fixed.contains("let r: i64 = one(one(c as i32));"),
        "{fixed}"
    );
    let out = karac::mir::lower::run_source(&fixed)
        .expect("MIR interpreter")
        .output;
    assert_eq!(out, "2\n3\n");
}
