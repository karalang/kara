//! B-2026-10-04-20: the interpreter's ASCII predicates on an integer wider than a byte answer from the whole value

use super::*;

/// B-2026-10-04-20: `is_ascii_digit` / `is_ascii_alphabetic` /
/// `is_ascii_hexdigit` on an integer used to mask the value to its low byte
/// first, so `304` (0x130) read as `'0'` and answered true. Codegen compares
/// the whole value, unsigned, at the receiver's width, and answers false for
/// everything outside 0..=255. The expected text is the compiled output.
#[test]
fn interp_ascii_predicates_do_not_mask_a_wide_integer_to_a_byte() {
    let out = run(r#"fn main() {
    let a: i64 = 304;
    let b: i64 = 0x141;
    let c: u32 = 0x166;
    let d: i32 = -208;
    let e: i8 = -48;
    println(f"{a.is_ascii_digit()} {b.is_ascii_alphabetic()} {c.is_ascii_hexdigit()} {d.is_ascii_digit()} {e.is_ascii_digit()}");
    let f: u8 = 0x30;
    let g: i64 = 0x41;
    let h: u64 = 0x66;
    println(f"{f.is_ascii_digit()} {g.is_ascii_alphabetic()} {h.is_ascii_hexdigit()}");
}"#);
    assert_eq!(out, "false false false false false\ntrue true true\n");
}
