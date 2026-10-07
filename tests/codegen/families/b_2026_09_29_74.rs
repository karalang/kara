//! B-2026-09-29-74 -- a whole-value store through a `mut ref` param runs the
//! displaced value's `Drop` body at the store, and a fresh temp lent `mut`
//! dies holding what the callee left in it.

use super::*;

/// B-2026-09-29-74 — `x = Some(mk(99))` over `x: mut ref Option[R]` ran no
/// displaced body on any compiled surface (nor under `--interp`) and leaked
/// it; a struct pointee ran its body only under `--interp`; and a fresh temp
/// lent `mut` (`hm(mut mk(4))`) ran its pre-call value's body twice under
/// `--interp` and never the stored one. Covered: `Option`, a plain struct,
/// both fresh-temp spellings, a `Result`, a struct holding an `Option`, a user
/// enum, a GENERIC callee (monomorphised) and an `Option[String]`.
#[test]
fn e2e_store_through_mut_ref_param_runs_displaced_body() {
    let src = r#"struct R { name: String, id: i64 }
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
"#;
    let want = "dR1/a-1-heap-string-longer-than-sso\nn5\ndR99/z-1-heap-string-longer-than-sso\na1\ndR2/b-1-heap-string-longer-than-sso\nn6\ndR98/y-1-heap-string-longer-than-sso\na2\ndR3/c-1-heap-string-longer-than-sso\ndR99/z-1-heap-string-longer-than-sso\nn5\na3\ndR4/d-1-heap-string-longer-than-sso\ndR98/y-1-heap-string-longer-than-sso\nn6\na4\ndR5/r-1-heap-string-longer-than-sso\nn1\na5\ndR6/o-1-heap-string-longer-than-sso\nn1\ndR70/h-1-heap-string-longer-than-sso\na6\ndR7/v-1-heap-string-longer-than-sso\nn2\ndR80/e-1-heap-string-longer-than-sso\na7\ndR8/t-1-heap-string-longer-than-sso\nn3\ndR90/g-1-heap-string-longer-than-sso\na8\nn4\nnew-3-heap-string-longer-than-sso\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}

/// B-2026-09-29-74 — `x = Err(7)`, `x = None`, two stores in one callee, a
/// store reached through a FORWARDED `mut ref` (`outer(x)` -> `inner(x)`), a
/// store after an arm has read the param, and a store that displaces a
/// body-less `Err`.
#[test]
fn e2e_store_through_mut_ref_param_variants() {
    let src = r#"struct R { name: String, id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}/{self.name}") } }
fn mk(n: i64, s: String) -> R { R { name: s, id: n } }
fn gr(x: mut ref Result[R, i64]) -> i64 { x = Err(7); 1 }
fn gn(x: mut ref Option[R]) -> i64 { x = None; 2 }
fn g2(x: mut ref Option[R]) -> i64 { x = Some(mk(50, f"p")); x = Some(mk(51, f"q")); 3 }
fn inner(x: mut ref Option[R]) -> i64 { x = Some(mk(60, f"i")); 4 }
fn outer(x: mut ref Option[R]) -> i64 { inner(x) }
fn gk(x: mut ref Option[R]) -> i64 { let n = match x { Some(r) => r.id, None => 0 }; x = Some(mk(n + 100, f"k")); 5 }
fn main() {
    let mut a: Result[R, i64] = Ok(mk(1, f"a")); println(f"n{gr(mut a)}"); println("a1");
    let mut b = Some(mk(2, f"b")); println(f"n{gn(mut b)}"); println("a2");
    let mut c = Some(mk(3, f"c")); println(f"n{g2(mut c)}"); println("a3");
    let mut d = Some(mk(4, f"d")); println(f"n{outer(mut d)}"); println("a4");
    let mut e = Some(mk(5, f"e")); println(f"n{gk(mut e)}"); println("a5");
    let mut f: Result[R, i64] = Err(3); println(f"n{gr(mut f)}"); println("end")
}
"#;
    let want = "dR1/a\nn1\na1\ndR2/b\nn2\na2\ndR3/c\ndR50/p\nn3\ndR51/q\na3\ndR4/d\nn4\ndR60/i\na4\ndR5/e\nn5\ndR105/k\na5\nn1\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
