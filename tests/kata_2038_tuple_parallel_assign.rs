//! B-2026-11-09-38, as the review ruled (design.md § Parallel assignment):
//! a single expression of tuple type on the right of a parallel assignment
//! is taken apart, `lo, hi = if c { (lo, mid) } else { (mid + 1, hi) };`,
//! after the whole of it is evaluated, and Rust's `(lo, hi) = e` is refused
//! with a machine fix that drops the parentheses.

use std::process::{Command, Stdio};

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn run(src: &str) -> String {
    karac::mir::lower::run_source(src)
        .expect("MIR interpreter")
        .output
}

#[test]
fn kata_2038_tuple_value_is_taken_apart() {
    let src = "fn div_rem(a: i64, b: i64) -> (i64, i64) {
    (a / b, a % b)
}

fn main() {
    let mut lo = 0;
    let mut hi = 10;
    let mid = 4;
    lo, hi = if lo < hi { (lo, mid) } else { (mid + 1, hi) };
    let mut q = 0;
    let mut r = 0;
    q, r = div_rem(17, 5);
    let mut v = [1, 2, 3].to_vec();
    let mut i = 0;
    v[i], i = (9, 2);
    let mut s = String.new();
    let mut t = String.new();
    s, t = (\"a\".to_string(), \"b\".to_string());
    println(f\"{lo} {hi} {q} {r} {v} {i} {s}{t}\");
}
";
    assert_eq!(run(src), "0 4 3 2 [9, 2, 3] 2 ab\n");
}

#[test]
fn kata_2038_tuple_value_of_another_arity_is_a_type_error() {
    let src = "fn main() {
    let mut a = 0;
    let mut b = 0;
    let mut c = 0;
    a, b, c = (1, 2);
    println(f\"{a}{b}{c}\");
}
";
    let path = std::env::temp_dir().join(format!("karac-k2038-arity-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    let out = karac(&["check", path.to_str().unwrap()]);
    let _ = std::fs::remove_file(&path);
    let msg = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(msg.contains("3 element(s) but type has 2"), "{msg}");
}

#[test]
fn kata_2038_parenthesized_targets_get_the_fix() {
    let src = "fn main() {
    let mut lo = 0;
    let mut hi = 9;
    let mid = 4;
    (lo, hi) = if lo < hi { (lo, mid) } else { (mid + 1, hi) };
    println(f\"{lo} {hi}\");
}
";
    let path = std::env::temp_dir().join(format!("karac-k2038-fix-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    let _ = karac(&["fix", path.to_str().unwrap()]);
    let fixed = std::fs::read_to_string(&path).unwrap();
    let check = karac(&["check", path.to_str().unwrap()]);
    let _ = std::fs::remove_file(&path);
    assert!(fixed.contains("    lo, hi = if lo < hi"), "{fixed}");
    assert!(
        check.status.success(),
        "{}",
        String::from_utf8_lossy(&check.stderr)
    );
    assert_eq!(run(&fixed), "0 4\n");
}
