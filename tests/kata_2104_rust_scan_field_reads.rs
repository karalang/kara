//! Kata 2104's scan arm, written as Rust writes `scan`, carries a pair of
//! bounds as the state: `*bounds = (bounds.0.min(x), bounds.1.max(x));
//! Some(bounds.1 - bounds.0)`. Rust reaches the lent pair's fields
//! through the reference, so the body reads `bounds.0` bare. The
//! rewrite of Rust's `scan` closure (B-2026-11-08-45) took any bare use
//! of the state as a sign the closure was not Rust's shape, and the call
//! drew a "drop the `*`" error and a mismatch naming the adaptor's own
//! `OutV`. A field read of the state now counts as a read, so the closure
//! gets the explanation and the rewrite; a field WRITE still does not.
//! The rewrite's lines keep the closure body's indentation, which they
//! lost when the closure sat in a chain.

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
fn kata_2104_scan_state_field_reads_get_the_rewrite() {
    let src = "fn main() {
    let nums = [4, -2, -3, 4, 1].to_vec();
    let mut sum = 0;
    for i in 0..nums.len() {
        let part: i64 = nums[i..]
            .iter()
            .scan((nums[i], nums[i]), |bounds, x| {
                *bounds = (bounds.0.min(x), bounds.1.max(x));
                Some(bounds.1 - bounds.0)
            })
            .sum();
        sum += part;
    }
    println(f\"{sum}\");
}
";
    let file = write("k2104-scan-fields", src);
    let check = karac(&["check", &file]);
    let stderr = String::from_utf8_lossy(&check.stderr).to_string();
    assert!(
        stderr.contains("this closure writes the state `bounds` through `*bounds`")
            && !stderr.contains("OutV")
            && !stderr.contains("drop the `*`"),
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
    assert!(
        fixed.contains("\n                let bounds = (bounds.0.min(x), bounds.1.max(x));")
            && fixed
                .contains("\n                Some((bounds, bounds.1 - bounds.0))\n            })"),
        "{fixed}"
    );
    let out = karac::mir::lower::run_source(&fixed)
        .expect("MIR interpreter")
        .output;
    assert_eq!(out, "59\n");
}

#[test]
fn kata_2104_scan_state_field_write_is_left() {
    let src = "fn main() {
    let total: i64 = [1, 2, 3]
        .iter()
        .scan((0, 0), |acc, x| {
            acc.0 = acc.0 + x;
            *acc = (acc.0, acc.1 + 1);
            Some(acc.0)
        })
        .sum();
    println(f\"{total}\");
}
";
    let file = write("k2104-scan-field-write", src);
    let check = karac(&["check", &file]);
    let _ = std::fs::remove_file(&file);
    let stderr = String::from_utf8_lossy(&check.stderr).to_string();
    assert!(!check.status.success(), "{stderr}");
    assert!(!stderr.contains("Some((acc,"), "{stderr}");
}
