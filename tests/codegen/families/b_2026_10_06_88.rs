//! B-2026-10-06-88: an operator tree of bare integer literals takes a
//! narrow type context.

use super::*;

/// B-2026-10-06-88: `let x: u16 = 1 + 2;`, `let bit: u16 = 1 << (d as
/// u16);`, `let m: u8 = 7 & 3;`, `let x: u64 = 1 << 40;` and a negated tree
/// were refused as an `i64` narrowing; they now type at the slot's type and
/// compute the same values compiled as interpreted -- including a `u16` mask
/// array built from them, the sudoku bitmask shape that found the row.
#[test]
fn e2e_int_literal_operator_tree_takes_narrow_context() {
    let src = r#"fn main() {
    let a: u16 = 1 + 2;
    let b: u8 = 7 & 3;
    let c: u8 = 255 >> 1;
    let d: i64 = 3;
    let bit: u16 = 1 << (d as u16);
    let big: u64 = 1 << 40;
    let k: u64 = 5;
    let sh: u64 = 1 << k;
    let n: i32 = -(2 * 3);
    let w: i8 = -128 + 1;
    let mut masks: Array[u16, 3] = [0; 3];
    for i in 0..3 {
        let m: u16 = 1 << (i as u16);
        masks[i] = masks[i] | m | (1 << 8);
    }
    println(f"{a} {b} {c} {bit} {big} {sh} {n} {w}");
    println(f"{masks[0]} {masks[1]} {masks[2]}");
}
"#;
    assert_eq!(
        run_program(src),
        Some("3 3 127 8 1099511627776 32 -6 -127\n257 258 260\n".to_string())
    );
}
