//! B-2026-09-28-6 — a bare seeded constructor discarded in statement position
//! (`Some(mk(1));`) runs its payload's `Drop` body under `--interp` as it does
//! compiled; a discarded NESTED envelope (`Option[Option[S]]`) frees every box;
//! and a nested constructor over an owned param (`Some(Some(x))`) leaves the
//! body to the caller at every position.

use super::*;

/// B-2026-09-28-6 — the ASAN twin of
/// `e2e_seeded_ctor_discard_and_nested_param_view_run_once`: every nested
/// envelope box and every `String` is freed once (272 B in 8 blocks leaked
/// before, at `-O0`).
#[test]
fn asan_seeded_ctor_discard_and_nested_param_view_free_once() {
    assert_clean_asan_run_min_allocs(
        r#"
struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
struct S { r: R, s: String }
fn mk(i: i64) -> S { S { r: R { id: i }, s: f"heap-string-longer-than-sso-{i}" } }
fn mm(i: i64) -> Option[Option[S]] { Some(Some(mk(i))) }
fn mr(i: i64) -> Option[Result[S, i64]] { Some(Ok(mk(i))) }
fn c1(x: S) { Some(Some(x)); println("in1") }
fn c2(x: S) { let _ = Some(Some(x)); println("in2") }
fn c3(x: S) -> bool { Some(Some(x)).is_some() }
fn c4(x: S) { let o = Some(Some(x)); println("in4") }
fn c5(x: S) { let o = Some(Some(x)); match o { Some(Some(s)) => println(f"m{s.r.id}"), _ => println("x") } println("in5") }
fn c6(x: S) { let o = Some(Some(x)); let o2 = o; println("in6") }
fn main() {
    Some(mk(1));
    println("a");
    Ok(mk(2));
    println("b");
    Err(mk(3));
    println("c");
    Some((mk(4), 4));
    println("e");
    let r = mk(5);
    Some(r);
    println("f");
    Some(Some(mk(6)));
    println("g");
    let _ = Some(Some(mk(7)));
    println("h");
    mm(8);
    println("i");
    mr(9);
    println("j");
    let b = Some(Some(mk(10))).is_some();
    println(f"k{b}");
    c1(mk(11));
    println("o1");
    c2(mk(12));
    println("o2");
    let t = c3(mk(13));
    println(f"o3{t}");
    c4(mk(14));
    println("o4");
    c5(mk(15));
    println("o5");
    c6(mk(16));
    println("o6");
    println("end")
}
"#,
        &[
            "d1", "a", "d2", "b", "d3", "c", "d4", "e", "d5", "f", "d6", "g", "d7", "h", "d8", "i",
            "d9", "j", "d10", "ktrue", "in1", "d11", "o1", "in2", "d12", "o2", "d13", "o3true",
            "in4", "d14", "o4", "m15", "in5", "d15", "o5", "in6", "d16", "o6", "end",
        ],
        "asan_seeded_ctor_discard_and_nested_param_view_free_once",
        20,
    );
}
