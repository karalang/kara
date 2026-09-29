//! B-2026-09-26-54 -- a named `Array[T, N]` moved into a tuple literal hands
//! its elements' drop to the tuple's owner.

use super::*;

/// B-2026-09-26-54 — the source array kept its own drop when moved into a
/// tuple literal, so an owner that walks the tuple (an annotated `let`, a
/// struct field, a by-value tuple param) freed the elements a second time,
/// and an unannotated `let` walked nothing, running no element bodies. One
/// tuple deeper the interpreter was silent as well. Covered: unannotated and
/// annotated `let`, a struct field, a nested tuple in a `let` and in a
/// field, a by-value tuple argument (read and destructured), a destructuring
/// `let`, a tuple rebind, the array second, `match` / `if let` scrutinee
/// literals, a returned tuple from an array param, a tuple local in a callee,
/// and a discarded literal.
#[test]
fn e2e_named_array_moved_into_tuple_literal() {
    let src = r#"struct D { id: i64, s: String }
impl Drop for D { fn drop(mut ref self) { println(f"dD{self.id}") } }
fn mkd(n: i64) -> D { return D { id: n, s: f"{n}-heap-string-longer-than-sso" } }
struct W { t: (Array[D, 2], i64) }
struct W3 { t: ((Array[D, 2], i64), i64) }
fn tk(t: (Array[D, 2], i64)) -> i64 { return t.1 }
fn tkd(t: (Array[D, 2], i64)) -> i64 { let (x, n) = t; return x[0].id + n }
fn pa(a: Array[D, 2]) -> (Array[D, 2], i64) { return (a, 1) }
fn tt(a: Array[D, 2]) -> i64 { let t = (a, 7); return t.1 }
fn main() {
    { let a: Array[D, 2] = [mkd(1), mkd(2)]; let t = (a, 7); println(f"s10 {t.1}") }
    { let a: Array[D, 2] = [mkd(3), mkd(4)]; let t: (Array[D, 2], i64) = (a, 7); println(f"s13 {t.1}") }
    { let a: Array[D, 2] = [mkd(5), mkd(6)]; let w = W { t: (a, 7) }; println(f"s9 {w.t.1}") }
    { let a: Array[D, 2] = [mkd(7), mkd(8)]; let t = ((a, 7), 8); println(f"s11 {t.1}") }
    { let a: Array[D, 2] = [mkd(9), mkd(10)]; let w = W3 { t: ((a, 7), 8) }; println(f"s3 {w.t.1}") }
    { let a: Array[D, 2] = [mkd(11), mkd(12)]; println(f"g6 {tk((a, 7))}") }
    { let a: Array[D, 2] = [mkd(13), mkd(14)]; println(f"g7 {tkd((a, 7))}") }
    { let a: Array[D, 2] = [mkd(15), mkd(16)]; let (x, n) = (a, 7); println(f"g12 {x[1].id} {n}") }
    { let a: Array[D, 2] = [mkd(17), mkd(18)]; let t = (a, 7); let u = t; println(f"g15 {u.1}") }
    { let a: Array[D, 2] = [mkd(19), mkd(20)]; let t = (7, a); println(f"g18 {t.0}") }
    { let a: Array[D, 2] = [mkd(21), mkd(22)]; match (a, 7) { (x, n) => println(f"g19 {n} {x[0].id}") } }
    { let a: Array[D, 2] = [mkd(23), mkd(24)]; let t = pa(a); println(f"g20 {t.1}") }
    { let a: Array[D, 2] = [mkd(25), mkd(26)]; if let (x, n) = (a, 7) { println(f"g25 {n} {x[0].id}") } }
    { let a: Array[D, 2] = [mkd(27), mkd(28)]; let t = (a, 7); match t { (x, n) => println(f"g26 {n} {x[0].id}") } }
    { let a: Array[D, 2] = [mkd(29), mkd(30)]; println(f"g27 {tt(a)}") }
    { let a: Array[D, 2] = [mkd(31), mkd(32)]; (a, 7); println("g10") }
    println("end")
}
"#;
    let want = "s10 7\ndD1\ndD2\ns13 7\ndD3\ndD4\ns9 7\ndD5\ndD6\ns11 8\ndD7\ndD8\ns3 8\ndD9\ndD10\ng6 7\ndD11\ndD12\ng7 20\ndD13\ndD14\ng12 16 7\ndD15\ndD16\ng15 7\ndD17\ndD18\ng18 7\ndD19\ndD20\ng19 7 21\ndD21\ndD22\ng20 1\ndD23\ndD24\ng25 7 25\ndD25\ndD26\ng26 7 27\ndD27\ndD28\ng27 7\ndD29\ndD30\ndD31\ndD32\ng10\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
