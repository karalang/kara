//! B-2026-09-29-74 -- a whole-value store through a `mut ref` param frees the
//! displaced value once, after running its `Drop` body.

use super::*;

/// B-2026-09-29-74 — the leak half: the displaced `Option` / `Result` / user
/// enum pointee was freed by nobody (7 definite losses at `-O0` on this
/// program before the fix, one per displaced value), including an
/// `Option[String]` with no `Drop` body at all.
#[test]
fn asan_store_through_mut_ref_param_frees_displaced_value() {
    assert_clean_asan_run(
        r#"struct R { name: String, id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}/{self.name}") } }
fn mk(n: i64, s: String) -> R { R { name: s, id: n } }
struct H { o: Option[R], k: i64 }
enum E { A(R), B }
fn gm(x: mut ref Option[R]) -> i64 { x = Some(mk(99, f"z-{1}-heap-string-longer-than-sso")); 5 }
fn hm(x: mut ref R) -> i64 { x = mk(98, f"y-{1}-heap-string-longer-than-sso"); 6 }
fn gr(x: mut ref Result[R, i64]) -> i64 { x = Err(7); 1 }
fn gh(x: mut ref H) -> i64 { x = H { o: Some(mk(70, f"h-{1}-heap-string-longer-than-sso")), k: 1 }; 1 }
fn ge(x: mut ref E) -> i64 { x = E.A(mk(80, f"e-{1}-heap-string-longer-than-sso")); 2 }
fn gg[T](x: mut ref Option[T], v: T) -> i64 { x = Some(v); 3 }
fn gs(x: mut ref Option[String]) -> i64 { x = Some(f"new-{3}-heap-string-longer-than-sso"); 4 }
fn main() {
    let mut o = Some(mk(1, f"a-{1}-heap-string-longer-than-sso")); println(f"n{gm(mut o)}"); println("a1");
    let mut r = mk(2, f"b-{1}-heap-string-longer-than-sso"); println(f"n{hm(mut r)}"); println("a2");
    println(f"n{gm(mut Some(mk(3, f"c-{1}-heap-string-longer-than-sso")))}"); println("a3");
    println(f"n{hm(mut mk(4, f"d-{1}-heap-string-longer-than-sso"))}"); println("a4");
    let mut a: Result[R, i64] = Ok(mk(5, f"r-{1}-heap-string-longer-than-sso")); println(f"n{gr(mut a)}"); println("a5");
    let mut h = H { o: Some(mk(6, f"o-{1}-heap-string-longer-than-sso")), k: 0 }; println(f"n{gh(mut h)}"); println("a6");
    let mut e = E.A(mk(7, f"v-{1}-heap-string-longer-than-sso")); println(f"n{ge(mut e)}"); println("a7");
    let mut g = Some(mk(8, f"t-{1}-heap-string-longer-than-sso")); println(f"n{gg(mut g, mk(90, f"g-{1}-heap-string-longer-than-sso"))}"); println("a8");
    let mut s = Some(f"old-{1}-heap-string-longer-than-sso"); println(f"n{gs(mut s)}"); println(f"{s.unwrap()}");
    println("end")
}
"#,
        &[
            "dR1/a-1-heap-string-longer-than-sso",
            "n5",
            "dR99/z-1-heap-string-longer-than-sso",
            "a1",
            "dR2/b-1-heap-string-longer-than-sso",
            "n6",
            "dR98/y-1-heap-string-longer-than-sso",
            "a2",
            "dR3/c-1-heap-string-longer-than-sso",
            "dR99/z-1-heap-string-longer-than-sso",
            "n5",
            "a3",
            "dR4/d-1-heap-string-longer-than-sso",
            "dR98/y-1-heap-string-longer-than-sso",
            "n6",
            "a4",
            "dR5/r-1-heap-string-longer-than-sso",
            "n1",
            "a5",
            "dR6/o-1-heap-string-longer-than-sso",
            "n1",
            "dR70/h-1-heap-string-longer-than-sso",
            "a6",
            "dR7/v-1-heap-string-longer-than-sso",
            "n2",
            "dR80/e-1-heap-string-longer-than-sso",
            "a7",
            "dR8/t-1-heap-string-longer-than-sso",
            "n3",
            "dR90/g-1-heap-string-longer-than-sso",
            "a8",
            "n4",
            "new-3-heap-string-longer-than-sso",
            "end",
        ],
        "B-2026-09-29-74 store through a mut ref param",
    );
}
