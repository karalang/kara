//! B-2026-11-25-23: a `ref` to a narrower integer handed to a number's `max` or
//! `min` is read through and widened once the iterator it came from is
//! adapted. `mx.max(f)` with `mx` an `i64` (its literal kept the default)
//! and `f: ref i32` from `flips.iter().rev()` passed `karac check`, which
//! B-2026-11-21-66 taught to accept it, and then stopped both lanes: the
//! argument reached the native as its address, "call of unknown function
//! i64.max" in the interpreter and "native `i64.max` is not lowered yet"
//! in the LLVM backend. Over a plain `flips.iter()` it ran. Kata 1375's
//! mutant of its Rust arm, reading the flips from the last.

use std::process::{Command, Output, Stdio};

fn karac(tag: &str, backend: &str, src: &str) -> Output {
    let path = std::env::temp_dir().join(format!(
        "karac-kata-1375-{tag}-{backend}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).expect("write");
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg(backend)
        .arg(&path)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_file(&path);
    out
}

fn backends() -> &'static [&'static str] {
    if cfg!(feature = "llvm") {
        &["check", "__mir-run", "__mir-llvm-run"]
    } else {
        &["check", "__mir-run"]
    }
}

/// `src` checks clean and prints `want` on every backend.
fn runs(tag: &str, src: &str, want: &str) {
    for b in backends() {
        let out = karac(tag, b, src);
        let (stdout, stderr) = (
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
        assert_eq!(out.status.code(), Some(0), "{tag} on {b}: {stderr}");
        if *b != "check" {
            assert_eq!(stdout, want, "{tag} on {b}: {stderr}");
        }
    }
}

#[test]
fn kata_1375_ref_i32_widens_at_max_over_rev() {
    let src = "fn from_last(flips: Vec[i32]) -> i32 {
    let mut mx = 0;
    let mut ans = 0;
    for (i, f) in flips.iter().rev().enumerate() {
        mx = mx.max(f);
        if mx == i as i32 + 1 {
            ans += 1;
        }
    }
    ans
}

fn main() {
    println(f\"{from_last([3, 2, 4, 1, 5])} {from_last([1, 2, 3])}\");
}
";
    runs("rev", src, "1 1\n");
}

#[test]
fn kata_1375_ref_narrow_widens_at_min_max_over_adapters() {
    let src = "fn main() {
    let v: Vec[i32] = [3, -7, 2, 9];
    let mut hi = 0;
    for x in v.iter().skip(1) {
        hi = hi.max(x);
    }
    let mut lo = 0;
    for x in v.iter().filter(|x| x < 5) {
        lo = lo.min(x);
    }
    let w: Vec[u8] = [200, 9, 250];
    let mut top: u32 = 0;
    for b in w.iter().rev() {
        top = top.max(b);
    }
    let mut least: i64 = 1000;
    for b in w.iter().take(2) {
        least = least.min(b);
    }
    println(f\"{hi} {lo} {top} {least}\");
}
";
    runs("adapters", src, "9 -7 250 9\n");
}
