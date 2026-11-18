//! Kata 2296's differential folds single steps back with `for i in
//! at..at + k`, `at` an unannotated `let mut at = 0` and `k` an `i64`.
//! Since range bounds were read open (B-2026-11-18-13), reading the end
//! fixed `at` to `i64` while the start was still the variable as first
//! read, and the range was refused "range bounds must have same type:
//! 'i64' and 'i64'" (B-2026-11-18-54).

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

fn checks_and_prints(name: &str, src: &str, want: &str) {
    let file = write(name, src);
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
    assert_eq!(out, want);
}

#[test]
fn kata_2296_range_from_a_literal_binding_to_itself_plus_a_typed_value() {
    checks_and_prints(
        "k2296-range-plus",
        "fn main() {
    let mut at = 0;
    let k: i64 = 3;
    let mut sum = 0;
    for i in at..at + k {
        sum += i;
    }
    at += k;
    println(f\"{sum} {at}\");
}
",
        "3 3\n",
    );
}

#[test]
fn kata_2296_range_open_bound_in_a_loop_over_vec_items() {
    checks_and_prints(
        "k2296-range-groups",
        "fn main() {
    let groups: Vec[i64] = [2, 3].to_vec();
    let mut at = 0;
    let mut sum = 0;
    for g in 0..2 {
        let k = groups[g];
        for i in at..at + k {
            sum += i;
        }
        at += k;
    }
    println(f\"{sum}\");
}
",
        "10\n",
    );
}

#[test]
fn kata_2296_range_end_a_literal_binding_minus_a_typed_value() {
    checks_and_prints(
        "k2296-range-minus",
        "fn main() {
    let n = 10;
    let k: i64 = 4;
    let mut count = 0;
    for _ in n - k..n {
        count += 1;
    }
    println(f\"{count}\");
}
",
        "4\n",
    );
}
