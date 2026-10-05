//! B-2026-10-05-70 -- a fresh String or Vec temporary passed to a generic
//! `ref T` / `mut ref` parameter was freed twice when compiled: the call site
//! gave it an owned-temp drop AND the borrow path's own temp drop.

use super::*;

/// Fresh temporaries (function returns, `vec![...]`, `.to_string()`) passed
/// to `ref Vec[T]`, `ref T` and `mut ref Vec[T]` generic params, next to the
/// by-value and named-binding spellings that already worked.
#[test]
fn e2e_generic_ref_param_fresh_temp() {
    let src = r#"fn mkv() -> Vec[i64] { vec![1, 2, 3] }
fn mks() -> String { "heap-string-long-enough".to_string() }
fn rlen[T](x: ref Vec[T]) -> i64 { x.len() }
fn rt[T](x: ref T) -> i64 { 7 }
fn rc[T: Clone](x: ref T) -> T { x.clone() }
fn mr[T](x: mut ref Vec[T], y: T) { x.push(y); }
fn byv[T](x: T) -> i64 { 3 }
fn main() {
    println(f"a:{rlen(mkv())} {rlen(vec![4, 5])}");
    println(f"b:{rt(mks())} {rt(mkv())} {rt("ab".to_string())}");
    let c = rc(mks());
    println(f"c:{c}");
    mr(mut mkv(), 9);
    println(f"d:{byv(mks())} {byv(mkv())}");
    let s = mks();
    let t = rc(s);
    println(f"e:{rt(s)} {s} {t}");
}
"#;
    let want = "a:3 2\nb:7 7 7\nc:heap-string-long-enough\nd:3 3\ne:7 heap-string-long-enough heap-string-long-enough\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
