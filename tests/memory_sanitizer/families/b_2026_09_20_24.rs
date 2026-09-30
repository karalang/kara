//! B-2026-09-20-24 / B-2026-09-29-108 -- a fresh tuple, `Array` or `Vec`
//! temporary argument frees each element once and runs each body once.

use super::*;

/// B-2026-09-20-24 / B-2026-09-29-108 — the caller now owns a fresh nameless
/// aggregate argument's element bodies and, for an `Array` whose elements own
/// heap, its memory (`afirst([mkd(1)])` leaked the element's `String`).
#[test]
fn asan_fresh_nameless_aggregate_arg_frees_once() {
    assert_clean_asan_run(
        r#"struct D { id: i64, s: String }
impl Drop for D { fn drop(mut ref self) { println(f"dD{self.id}") } }
fn mkd(n: i64) -> D { return D { id: n, s: f"sssssssssssssssssssssssssssssssssss{n}" } }
struct H { k: i64 }
impl H {
    fn mlen(ref self, x: Vec[D]) -> i64 { return x.len() }
    fn afirst(x: Array[D, 1]) -> i64 { return x[0].id }
}
fn afirst(x: Array[D, 1]) -> i64 { return x[0].id }
fn apair(x: Array[D, 2]) -> i64 { return x[0].id }
fn aseven(x: Array[D, 1]) -> i64 { return 7 }
fn afwd(x: Array[D, 1]) -> i64 { return afirst(x) }
fn vlen(x: Vec[D]) -> i64 { return x.len() }
fn glen[T](x: Vec[T]) -> i64 { return x.len() }
fn tsnd(t: (D, i64)) -> i64 { return t.1 }
fn tone(t: (D, D)) -> i64 { return 1 }
fn qv(t: (Vec[D], i64)) -> i64 { return t.1 }
fn qa(t: (Array[D, 1], i64)) -> i64 { return t.1 }
fn qd(t: (Vec[D], i64)) -> i64 { let (a, j) = t; return a.len() + j }
fn tkeep(t: (D, i64)) -> D { let (w, k) = t; return w }
fn vkeep(x: Vec[D]) -> Vec[D] { return x }
fn mka() -> Array[D, 1] { return [mkd(2)] }
fn mkv() -> Vec[D] { return [mkd(5)] }
fn mkt(n: i64) -> (D, i64) { return (mkd(n), 7) }
fn mktt() -> (D, D) { return (mkd(7), mkd(8)) }
fn main() {
    println(f"a {afirst([mkd(1)])}")
    println(f"b {afirst(mka())}")
    println(f"c {apair([mkd(3), mkd(4)])}")
    aseven([mkd(9)]);
    println("d")
    println(f"e {afwd([mkd(10)])}")
    println(f"f {vlen([mkd(11), mkd(12)])}")
    println(f"g {vlen(mkv())}")
    println(f"h {tsnd(mkt(6))}")
    println(f"i {tone(mktt())}")
    println(f"j {qv(([mkd(13)], 7))}")
    println(f"k {qa(([mkd(14)], 7))}")
    println(f"l {qd(([mkd(15), mkd(16)], 7))}")
    let h = H { k: 0 };
    println(f"m {h.mlen([mkd(17)])}")
    println(f"n {H.afirst([mkd(18)])}")
    println(f"o {glen([mkd(19)])}")
    { let w = tkeep(mkt(22)); println(f"p {w.id}") }
    { let v = vkeep([mkd(20)]); println(f"q {v.len()}") }
    { let x = mkd(21); println(f"r {vlen([x])}") }
    println("end")
}
"#,
        &[
            "dD1", "a 1", "dD2", "b 2", "dD3", "dD4", "c 3", "dD9", "d", "dD10", "e 10", "dD11",
            "dD12", "f 2", "dD5", "g 1", "dD6", "h 7", "dD7", "dD8", "i 1", "dD13", "j 7", "dD14",
            "k 7", "dD15", "dD16", "l 9", "dD17", "m 1", "dD18", "n 18", "dD19", "o 1", "p 22",
            "dD22", "q 1", "dD20", "r 1", "dD21", "end",
        ],
        "B-2026-09-20-24 fresh nameless aggregate argument",
    );
}
