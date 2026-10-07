//! B-2026-09-26-53 -- a `String` / `Vec` leaf destructured out of a tuple
//! owns its buffer and its elements' `Drop` bodies.

use super::*;

/// B-2026-09-26-53 — a `String`/`Vec` leaf off a tuple LOCAL kept the
/// source-owns model, so a whole rebind (`let b = a;`) double-freed, and a
/// `Vec[D]` leaf (local or fresh source) or a discarded `Vec[D]` element ran
/// none of its elements' bodies on any compiled surface. Covered: String,
/// `Vec[i64]`, `Vec[D]` rebinds; unused, fresh-call and handed-off leaves; a
/// struct-field projection source; a loop; a conditional move; a returned leaf;
/// `Vec[Vec[D]]`, `Vec[E]`, `Vec[Option[D]]`, `Vec[String]` leaves; a
/// mutated rebind; and a discarded `Vec[D]` on a local and a fresh source.
#[test]
fn e2e_tuple_destructure_vec_string_leaf_owns() {
    let src = r#"struct D { id: i64, s: String }
impl Drop for D { fn drop(mut ref self) { println(f"dD{self.id}") } }
enum E { A(D), B(i64) }
struct H { pe: (Vec[D], i64), n: i64 }
fn mkd(n: i64) -> D { return D { id: n, s: f"{n}-heap-string-longer-than-sso" } }
fn mkt(n: i64) -> (Vec[D], i64) { return ([mkd(n), mkd(n + 1)], 7) }
fn mk2(n: i64) -> (Vec[D], Vec[D]) { return ([mkd(n)], [mkd(n + 1)]) }
fn take(v: Vec[D]) -> i64 { return v.len() }
fn ret() -> Vec[D] { let t: (Vec[D], i64) = ([mkd(80), mkd(81)], 7); let (a, j) = t; println(f"r{j}"); return a }
fn main() {
    { let t: (String, i64) = (f"string-that-is-longer-than-sso", 7); let (a, j) = t; let b = a; println(f"k1 {j} {b.len()}") }
    { let t: (Vec[i64], i64) = ([1, 2, 3], 7); let (a, j) = t; let b = a; println(f"k2 {j} {b.len()}") }
    { let t: (Vec[D], i64) = ([mkd(1), mkd(2)], 7); let (a, j) = t; let b = a; println(f"h3 {j} {b.len()}") }
    { let t: (Vec[D], i64) = ([mkd(3), mkd(4)], 7); let (a, j) = t; println(f"g5 {j}") }
    { let (a, j) = mkt(5); println(f"h1 {j} {a.len()}") }
    { let t: (Vec[D], i64) = ([mkd(7), mkd(8)], 7); let (a, j) = t; println(f"h4 {j} {take(a)}") }
    let h = H { pe: ([mkd(9), mkd(10)], 7), n: 3 };
    { let (a, j) = h.pe; let b = a; println(f"p4 {j} {b.len()}") }
    for i in 11..13 { let t: (Vec[D], i64) = ([mkd(i)], i); let (a, j) = t; println(f"p5 {j} {a.len()}") }
    let c = true;
    { let t: (Vec[D], i64) = ([mkd(13), mkd(14)], 7); let (a, j) = t; if c { let b = a; println(f"p6 {b.len()}") } println(f"p6 {j}") }
    let v = ret();
    println(f"p7 {v.len()}");
    { let t: (Vec[Vec[D]], i64) = ([[mkd(15)], [mkd(16)]], 7); let (a, j) = t; println(f"p8 {j} {a.len()}") }
    { let t: (Vec[E], i64) = ([E.A(mkd(17)), E.B(2)], 7); let (a, j) = t; println(f"p9 {j} {a.len()}") }
    { let t: (Vec[Option[D]], i64) = ([Option.Some(mkd(18)), Option.None], 7); let (a, j) = t; println(f"p10 {j} {a.len()}") }
    { let t: (Vec[String], i64) = ([f"string-a-longer-than-sso-xx", f"string-b-longer-than-sso-yy"], 7); let (a, j) = t; let b = a; println(f"p11 {j} {b.len()}") }
    { let t: (Vec[D], i64) = ([mkd(19)], 7); let (a, j) = t; let mut m = a; m.push(mkd(20)); println(f"p12 {j} {m.len()}") }
    { let t: (Vec[D], Vec[D]) = ([mkd(21)], [mkd(22)]); let (a, _) = t; println(f"p17 {a.len()}") }
    { let (a, _) = mk2(23); println(f"q5 {a.len()}") }
    println("end")
}
"#;
    let want = "k1 7 30\nk2 7 3\nh3 7 2\ndD1\ndD2\ndD3\ndD4\ng5 7\nh1 7 2\ndD5\ndD6\nh4 7 2\ndD7\ndD8\np4 7 2\ndD9\ndD10\np5 11 1\ndD11\np5 12 1\ndD12\np6 2\ndD13\ndD14\np6 7\nr7\np7 2\ndD80\ndD81\np8 7 2\ndD15\ndD16\np9 7 2\ndD17\np10 7 2\ndD18\np11 7 2\np12 7 2\ndD19\ndD20\ndD22\np17 1\ndD21\ndD24\nq5 1\ndD23\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
