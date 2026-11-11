//! Kata 2107's Rust template writes `let k = k as usize;` and then
//! `for i in k..candies.len()`. Kāra's `len()` is `i64`, so the range's
//! bounds were a `usize` and an `i64`, and the bounds check let the two
//! meet: the template ran, with nothing said about the FFI-only `usize`.
//! An operator over the same pair (`k + v.len()`) is an error whose fix
//! turns the `as usize` into `as i64`; a range is now the same.

use std::process::{Command, Stdio};

#[test]
fn kata_2107_usize_start_against_a_length_is_fixed() {
    let src = "fn f(v: Vec[i64], k: i32) -> i64 {
    let k = k as usize;
    let mut s = 0;
    for i in k..v.len() {
        s += v[i - k];
    }
    s
}
fn main() {
    println(f([1, 2, 3].to_vec(), 1));
}
";
    let path = std::env::temp_dir().join(format!("karac-k2107-range-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    let file = path.to_str().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", file])
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(!out.status.success(), "{stderr}");
    assert!(
        stderr.contains(":4:14: range bounds must have same type: 'usize' and 'i64' mix signed"),
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
    assert!(fixed.contains("let k = k as i64;"), "{fixed}");
    let out = karac::mir::lower::run_source(&fixed)
        .expect("MIR interpreter")
        .output;
    assert_eq!(out, "3\n");
}

#[test]
fn kata_2107_same_sign_ranges_are_left() {
    // Two unsigned bounds, a literal beside a `usize`, and a borrowed
    // `i64` beside an `i64` all stay one type.
    let src = "fn main() {
    let a: usize = 1;
    let b: usize = 3;
    let mut n = 0;
    for _ in a..b {
        n += 1;
    }
    for _ in 0..b {
        n += 1;
    }
    let v = [1, 2];
    for x in v.iter() {
        for _ in x..x + 2 {
            n += 1;
        }
    }
    println(n);
}
";
    let checked = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(checked.output, "9\n");
}
