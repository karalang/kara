//! Found writing kata 2787 (Ways to Express an Integer as Sum of Powers).
//!
//! B-2026-11-20-149: `let p = i.pow(x);` with `x` an `i64` is refused,
//! "pow expects an exponent of type `u32`", with the `as u32` fix; but the
//! call was then typed as an error, so `p` had no type and the next line,
//! `for s in (p..=n).rev()`, failed a second time: "method 'rev' is not
//! callable on this `RangeInclusive`: the type argument for `T` could not
//! be inferred". The power is the receiver's type whatever the exponent,
//! so only the exponent is reported.

use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

static N: AtomicUsize = AtomicUsize::new(0);

fn karac(cmd: &str, src: &str) -> (String, String) {
    let n = N.fetch_add(1, Ordering::SeqCst);
    let path = std::env::temp_dir().join(format!(
        "karac-pow-exponent-type-{}-{n}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args([cmd, path.to_str().unwrap()])
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let after = std::fs::read_to_string(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    (
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr),
        after,
    )
}

fn run(src: &str) -> String {
    karac::mir::lower::run_source(src)
        .expect("MIR interpreter")
        .output
}

const WRITTEN: &str = "fn count(n: i64, x: i64) -> i64 {
    let mut total = 0;
    let p = 2.pow(x);
    for s in (p..=n).rev() {
        total += s;
    }
    total
}

fn main() {
    println(f\"{count(10, 2)}\");
}
";

#[test]
fn pow_exponent_mismatch_reports_once() {
    let (out, _) = karac("check", WRITTEN);
    assert!(
        out.contains(
            ":3:19: pow expects an exponent of type `u32`, got `i64` (cast with `as u32`)"
        ),
        "{out}"
    );
    assert!(!out.contains("could not be inferred"), "{out}");
    assert!(out.contains("1 error(s) found"), "{out}");
}

#[test]
fn pow_exponent_fix_runs() {
    let (_, after) = karac("fix", WRITTEN);
    assert!(after.contains("let p = 2.pow(x as u32);"), "{after}");
    let (out, _) = karac("check", after.as_str());
    assert!(out.contains("All checks passed"), "{out}");
    // 4 + 5 + ... + 10
    assert_eq!(run(&after), "49\n");
}
