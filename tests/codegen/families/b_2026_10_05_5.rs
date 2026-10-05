//! B-2026-10-05-5 -- a one-word `Array` held as a field of a tuple or struct
//! inside an enum payload read its element as 0 compiled (`Some((Array[5], 7))`
//! printed `0 7`), and the struct-field spelling failed module verification.

use super::*;

/// Every one-word array field keeps its element through the payload: tuple
/// fields of `Option` / `Result` / a user enum, destructured and bound, a
/// nested tuple, a struct field, an array two struct levels down, a `Vec`
/// element, `bool` and `f64` neighbours, a `Drop`-bodied element, and an
/// `unwrap()` read.
#[test]
fn e2e_one_word_array_field_in_enum_payload() {
    let src = r#"struct P { n: i64 }
struct S { a: Array[i64, 1], b: i64 }
struct I { a: Array[i64, 1] }
struct S2 { i: I, b: i64 }
struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}"); } }
enum E { A((Array[i64, 1], i64)), B }
fn mk() -> Option[(Array[i64, 1], i64)] { Some((Array[6], 7)) }
fn main() {
    let d = Some((Array[5], 7));
    match d { Some(t) => println(f"a:{t.0[0]} {t.1}"), None => {} }
    let r: Result[(Array[i64, 1], i64), i64] = Ok((Array[8], 9));
    match r { Ok((x, y)) => println(f"b:{x[0]} {y}"), Err(e) => println(f"e{e}") }
    match mk() { Some(t) => { let a = t.0; println(f"c:{a[0]} {a.len()}") }, None => {} }
    let n = Some((1, (Array[P { n: 11 }], 2.5)));
    match n { Some(t) => println(f"d:{t.1.0[0].n} {t.1.1} {t.0}"), None => {} }
    match Some(S { a: Array[12], b: 13 }) { Some(s) => println(f"e:{s.a[0]} {s.b}"), None => {} }
    match Some(S2 { i: I { a: Array[14] }, b: 15 }) { Some(s) => { let i = s.i; println(f"f:{i.a[0]} {s.b}") }, None => {} }
    match E.A((Array[16], 17)) { E.A(t) => println(f"g:{t.0[0]} {t.1}"), E.B => {} }
    let mut v: Vec[Option[(Array[bool, 1], i64)]] = Vec.new();
    v.push(Some((Array[true], 18)));
    match v[0] { Some(t) => println(f"h:{t.0[0]} {t.1}"), None => {} }
    let w = Some((Array[R { id: 19 }], 20));
    match w { Some(t) => println(f"i:{t.0[0].id} {t.1}"), None => {} }
    println(f"j:{Some((Array[21], 22)).unwrap().0[0]}");
}
"#;
    let want = "a:5 7\nb:8 9\nc:6 1\nd:11 2.5 1\ne:12 13\nf:14 15\ng:16 17\nh:true 18\ni:19 20\nd19\nj:21\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
