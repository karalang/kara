//! Kata 2261's iterator arm, written as Rust writes `scan`, yields a pair
//! from the lent state: `*count += ..; Some((end, *count))`. The rewrite
//! of Rust's `scan` closure (B-2026-11-08-45) took any pair in the tail
//! `Some(..)` for Kāra's own `(next_state, item)`, so the closure drew
//! "drop the `*`" fixes whose result reads `end` as the next state and
//! the count as the item (B-2026-11-18-79). A pair whose first half is
//! not the bare state is now Rust's item, and the closure gets the
//! explanation and `Some((count, (end, count)))`.

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
fn kata_2261_rust_scan_pair_item_gets_the_rewrite() {
    let src = "fn main() {
    let xs: Vec[i64] = [3, 1, 4, 1, 5].to_vec();
    let pairs: Vec[(i64, i64)] = xs.into_iter().scan(0, |acc, x| {
        *acc += x;
        Some((x, *acc))
    }).collect();
    println(f\"{pairs}\");
}
";
    let file = write("k2261-scan-pair", src);
    let check = karac(&["check", &file]);
    let stderr = String::from_utf8_lossy(&check.stderr).to_string();
    assert!(
        stderr.contains("this closure writes the state `acc` through `*acc`")
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
        fixed.contains("|acc, x| { let acc = acc + x; Some((acc, (x, acc))) }"),
        "{fixed}"
    );
    let out = karac::mir::lower::run_source(&fixed)
        .expect("MIR interpreter")
        .output;
    assert_eq!(out, "[(3, 3), (1, 4), (4, 8), (1, 9), (5, 14)]\n");
}

#[test]
fn kata_2261_scan_pair_led_by_the_state_is_left() {
    // Already Kāra's shape, `Some((acc, ..))`, failing for another reason:
    // no Rust rewrite is offered.
    let src = "fn main() {
    let xs: Vec[i64] = [3, 1].to_vec();
    let v: Vec[i64] = xs.into_iter().scan(0, |acc, x| {
        Some((acc, *x))
    }).collect();
    println(f\"{v}\");
}
";
    let file = write("k2261-scan-kara-pair", src);
    let check = karac(&["check", &file]);
    let _ = std::fs::remove_file(&file);
    let stderr = String::from_utf8_lossy(&check.stderr).to_string();
    assert!(!check.status.success(), "{stderr}");
    assert!(
        !stderr.contains("this closure writes the state"),
        "{stderr}"
    );
}
