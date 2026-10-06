//! B-2026-10-06-90 -- a repeat literal `[v; n]` into a narrow `Array` slot (a
//! struct field beside others, a call argument) took its element width from
//! `v` (`i64`) and failed LLVM module verification. A method or associated-fn
//! argument failed the same way for the element-list form `[a, b]` as well.

use super::*;

#[test]
fn e2e_repeat_literal_takes_narrow_array_slot_width() {
    let src = r#"struct M { b: i64, rows: Array[u16, 3], neg: Array[i8, 2], fl: Array[f32, 2] }
struct W { x: i64, a: Array[u8, 2] }
impl W {
    fn take(ref self, a: Array[u8, 3]) -> i64 { (a[0] as i64) + self.x }
    fn make(a: Array[u8, 2]) -> W { W { x: 1, a } }
}
fn z() -> Array[u8, 4] { [255; 4] }
fn g(a: Array[i16, 2], k: i64) -> i64 { (a[0] as i64) + (a[1] as i64) + k }
fn f(a: Array[u8, 2]) -> u8 { a[1] }
fn mk(b: i64) -> M { M { b, rows: [7; 3], neg: [0; 2], fl: [0.0; 2] } }
fn main() {
    let m = M { b: 1, rows: [65535; 3], neg: [-1; 2], fl: [0.5; 2] };
    let w = W { x: 10, a: [0; 2] };
    let t: (i64, Array[u16, 2]) = (3, [7; 2]);
    println(f"{m.rows[0]} {m.neg[1]} {m.fl[0]} {z()[3]} {g([-300; 2], 1)} {f([9; 2])}");
    println(f"{w.take([200; 3])} {w.take([201, 0, 0])} {W.make([250; 2]).a[1]} {W.make([1, 2]).a[1]} {t.1[1]} {mk(4).rows[2]}");
}
"#;
    let want = "65535 -1 0.5 255 -599 9\n210 211 250 2 7 7\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
