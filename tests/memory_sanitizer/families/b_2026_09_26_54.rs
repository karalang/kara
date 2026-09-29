//! B-2026-09-26-54 -- a named `Array[T, N]` moved into a tuple literal is
//! freed exactly once.

use super::*;

/// B-2026-09-26-54 — the source array and the tuple's owner both freed the
/// elements (a double free on the annotated `let`, struct-field, nested-field
/// and by-value-argument spellings; 10 valgrind errors on this program at -O0
/// before the fix).
#[test]
fn asan_named_array_moved_into_tuple_literal() {
    assert_clean_asan_run(
        r#"struct D { id: i64, s: String }
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
"#,
        &[
            "s10 7", "dD1", "dD2", "s13 7", "dD3", "dD4", "s9 7", "dD5", "dD6", "s11 8", "dD7",
            "dD8", "s3 8", "dD9", "dD10", "g6 7", "dD11", "dD12", "g7 20", "dD13", "dD14",
            "g12 16 7", "dD15", "dD16", "g15 7", "dD17", "dD18", "g18 7", "dD19", "dD20",
            "g19 7 21", "dD21", "dD22", "g20 1", "dD23", "dD24", "g25 7 25", "dD25", "dD26",
            "g26 7 27", "dD27", "dD28", "g27 7", "dD29", "dD30", "dD31", "dD32", "g10", "end",
        ],
        "B-2026-09-26-54 named array moved into tuple literal",
    );
}
