//! B-2026-09-28-37 — a fresh-temp `Option` scrutinee frees the box its wide
//! payload was spilled into when the temp wraps an owned param
//! (`match mk2o(x) { .. }`) or when no pattern names the payload
//! (`if let None = mk2(8)`).

use super::*;

/// B-2026-09-28-37 — `match` / `if let` / `while let` / `let .. else` over a
/// hand-back that wraps an owned param (concrete and generic), and `None`-only
/// patterns over a fresh call and over borrow accessors, each leaked the
/// 32-byte payload box (384 B in 12 blocks at `-O0`).
#[test]
fn asan_freshtemp_option_scrutinee_param_wrap_and_none_only_free_box() {
    assert_clean_asan_run_min_allocs(
        r#"
struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
struct S { r: R, s: String }
fn mk(i: i64) -> S { S { r: R { id: i }, s: f"heap-string-longer-than-sso-{i}" } }
fn mk2(i: i64) -> Option[S] { Some(mk(i)) }
fn mk2o(x: S) -> Option[S] { Some(x) }
fn wg[T](x: T) -> Option[T] { Some(x) }
fn keep(s: S) { println(f"k{s.r.id}") }
fn c1(x: S) -> bool { if let None = mk2o(x) { false } else { true } }
fn c2(x: S) -> bool { match mk2o(x) { Some(_) => println("m2"), None => println("n") } true }
fn c3(x: S) -> bool { while let Some(_) = mk2o(x) { break; } true }
fn c4(x: S) -> bool { let Some(_) = mk2o(x) else { return false }; true }
fn c5(x: S) -> bool { match mk2o(x) { Some(s) => println(f"m{s.r.id}"), None => println("n") } true }
fn c6(x: S) -> bool { match mk2o(x) { Some(s) => keep(s), None => println("n") } true }
fn c7(x: S) -> bool { match wg(x) { Some(_) => println("m7"), None => println("n") } true }
fn main() {
    println(f"a{c1(mk(1))}");
    println(f"a{c2(mk(2))}");
    println(f"a{c3(mk(3))}");
    println(f"a{c4(mk(4))}");
    println(f"a{c5(mk(5))}");
    println(f"a{c6(mk(6))}");
    println(f"a{c7(mk(7))}");
    let b = if let None = mk2(8) { 1 } else { 2 };
    println(f"b{b}");
    let mut i = 0;
    while i < 2 {
        if let None = mk2(10 + i) { println("n") }
        i = i + 1;
    }
    let v = vec![mk(20)];
    if let None = v.first() { println("n") }
    match v.get(0) { None => println("n"), _ => println("s") }
    println("end")
}
"#,
        &[
            "d1", "atrue", "m2", "d2", "atrue", "d3", "atrue", "d4", "atrue", "m5", "d5", "atrue",
            "k6", "d6", "atrue", "m7", "d7", "atrue", "d8", "b2", "d10", "d11", "s", "d20", "end",
        ],
        "asan_freshtemp_option_scrutinee_param_wrap_and_none_only_free_box",
        20,
    );
}
