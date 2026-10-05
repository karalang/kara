//! B-2026-10-05-70 -- a fresh String or Vec temporary passed to a generic
//! `ref T` / `mut ref` parameter was freed twice when compiled: the call site
//! gave it an owned-temp drop AND the borrow path's own temp drop.

use super::*;

/// Each temporary is released exactly once.
#[test]
fn asan_generic_ref_param_fresh_temp() {
    assert_clean_asan_run(
        r#"fn mkv() -> Vec[i64] { vec![1, 2, 3] }
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
"#,
        &[
            "a:3 2",
            "b:7 7 7",
            "c:heap-string-long-enough",
            "d:3 3",
            "e:7 heap-string-long-enough heap-string-long-enough",
        ],
        "asan_generic_ref_param_fresh_temp",
    );
}
