//! B-2026-09-20-24 / B-2026-09-29-108 -- a fresh tuple, `Array` or `Vec`
//! temporary passed to a by-value parameter runs its elements' `Drop` bodies.

use super::*;

/// B-2026-09-20-24 / B-2026-09-29-108 — an argument that is a fresh NAMELESS
/// aggregate (an array or `Vec` literal, a call returning a tuple, `Array`
/// or `Vec`, or a tuple literal holding a collection literal) handed to an
/// owned parameter ran NONE of its elements' `Drop` bodies on any surface,
/// while the same call over a named local ran each once. The parameter is
/// caller-retained, so the caller owes the bodies after the call. Covers the
/// free, method, associated and generic callees, a forwarding callee, a
/// statement call, the escape controls (a callee that hands the value or an
/// element back) and a literal holding a place, whose binding keeps its body.
#[test]
fn e2e_fresh_nameless_aggregate_arg_runs_element_bodies() {
    let src = r#"struct D { id: i64, s: String }
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
    println(f"a {afirst([mkd(1)])}");
    println(f"b {afirst(mka())}");
    println(f"c {apair([mkd(3), mkd(4)])}");
    aseven([mkd(9)]);
    println("d");
    println(f"e {afwd([mkd(10)])}");
    println(f"f {vlen([mkd(11), mkd(12)])}");
    println(f"g {vlen(mkv())}");
    println(f"h {tsnd(mkt(6))}");
    println(f"i {tone(mktt())}");
    println(f"j {qv(([mkd(13)], 7))}");
    println(f"k {qa(([mkd(14)], 7))}");
    println(f"l {qd(([mkd(15), mkd(16)], 7))}");
    let h = H { k: 0 };
    println(f"m {h.mlen([mkd(17)])}");
    println(f"n {H.afirst([mkd(18)])}");
    println(f"o {glen([mkd(19)])}");
    { let w = tkeep(mkt(22)); println(f"p {w.id}") }
    { let v = vkeep([mkd(20)]); println(f"q {v.len()}") }
    { let x = mkd(21); println(f"r {vlen([x])}") }
    println("end")
}
"#;
    let want = "dD1\na 1\ndD2\nb 2\ndD3\ndD4\nc 3\ndD9\nd\ndD10\ne 10\ndD11\ndD12\nf 2\ndD5\ng 1\ndD6\nh 7\ndD7\ndD8\ni 1\ndD13\nj 7\ndD14\nk 7\ndD15\ndD16\nl 9\ndD17\nm 1\ndD18\nn 18\ndD19\no 1\np 22\ndD22\nq 1\ndD20\nr 1\ndD21\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
