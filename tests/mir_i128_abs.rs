//! `i128.MIN.abs()` traps as integer overflow and `i128.abs_diff` returns
//! the distance as a `u128`, on the MIR interpreter as on the compiled
//! backend (design.md § abs; B-2026-11-13-24). The interpreter returned
//! `i128.MIN` from the first and wrapped the second through `i128`:
//! `i128.MIN.abs_diff(3)` printed `2^127 - 3`.

use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

fn run(mode: &str, src: &str) -> Output {
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("karac-i128-abs-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg(mode)
        .arg(&file)
        .current_dir(&dir)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    out
}

const DIFF: &str = r#"fn main() {
    let m = i128.MIN;
    println(m.abs_diff(3));
    println(m.abs_diff(i128.MAX));
    println(i128.MAX.abs_diff(m));
    println((7i128).abs_diff(-5));
    println((-7i64).abs_diff(5));
    println(i64.MIN.abs_diff(i64.MAX));
    println((-5i128).abs());
}
"#;

const WANT_DIFF: &str = "170141183460469231731687303715884105731\n\
                         340282366920938463463374607431768211455\n\
                         340282366920938463463374607431768211455\n\
                         12\n12\n18446744073709551615\n5\n";

const ABS_MIN: &str = r#"fn main() {
    println("before");
    let m = i128.MIN;
    println(m.abs());
    println("after");
}
"#;

fn check(mode: &str) {
    let out = run(mode, DIFF);
    assert!(
        out.status.success(),
        "{mode}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), WANT_DIFF, "{mode}");
    let out = run(mode, ABS_MIN);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{mode}: i128.MIN.abs() must trap");
    assert!(err.contains("integer overflow"), "{mode}: {err}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "before\n", "{mode}");
}

#[test]
fn interp_i128_abs_and_abs_diff() {
    check("__mir-run");
}

#[cfg(feature = "llvm")]
#[test]
fn compiled_i128_abs_and_abs_diff() {
    check("__mir-llvm-run");
}
