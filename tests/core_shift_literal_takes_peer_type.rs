//! design.md § Division, remainder and shifts: a shift has the type of its
//! left operand, and an unsuffixed literal takes its type from context. So
//! in `r | (255 << b)` with `r: u64` the literal, the shift and the `|` are
//! all `u64`. The checker typed the literal tree at `u64` by span only, so
//! the MIR build kept the shift at `i64` and refused its own output ("binary
//! operands of different types u64 and i64"), and beside an arithmetic
//! operator or a comparison the tree was refused as a mix of `u64` and `i64`
//! (B-2026-10-29-28).

use std::process::Command;

fn check_and_run(name: &str, src: &str) -> String {
    let dir = std::env::temp_dir().join(format!("karac-shiftlit-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&path)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "{name}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    karac::mir::lower::run_source(src)
        .unwrap_or_else(|e| panic!("{name}: {e}"))
        .output
}

#[test]
fn a_literal_shifted_by_a_variable_takes_the_bitwise_peer_s_type() {
    let src = "fn fill(r: u64, b: i64) -> u64 { r | (255 << b) }
fn mask(r: u64, b: i64) -> u64 { (1 << b) ^ r }
fn main() {
    println(fill(1, 4));
    println(mask(1, 4));
}
";
    assert_eq!(check_and_run("bitwise", src), "4081\n17\n");
}

#[test]
fn a_literal_tree_takes_the_arithmetic_peer_s_type() {
    let src = "fn add(r: u64, b: i64) -> u64 { r + (255 << b) }
fn mul(r: u64, b: i64) -> u64 { (1 << b) * r }
fn sum(r: u64) -> u64 { r + (2 + 3) }
fn main() {
    println(add(1, 4));
    println(mul(3, 4));
    println(sum(1));
}
";
    assert_eq!(check_and_run("arith", src), "4081\n48\n6\n");
}

#[test]
fn a_literal_tree_takes_the_compared_peer_s_type() {
    let src = "fn is_bit(r: u64, b: i64) -> bool { r == (1 << b) }
fn below(r: u32, b: i64) -> bool { r < (1 << b) }
fn main() {
    println(is_bit(16, 4));
    println(is_bit(17, 4));
    println(below(3, 2));
}
";
    assert_eq!(check_and_run("compare", src), "true\nfalse\ntrue\n");
}

#[test]
fn a_promoted_literal_tree_is_range_checked_at_the_peer_s_type() {
    let src = "fn f(r: u8, b: i64) -> u8 { r + (300 << b) }
fn main() { println(f(1, 0)); }
";
    let dir = std::env::temp_dir().join(format!("karac-shiftlit-{}-range", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&path)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "check must refuse");
    assert!(err.contains("out of range for 'u8'"), "{err}");
}
