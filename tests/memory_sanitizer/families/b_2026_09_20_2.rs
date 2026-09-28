//! B-2026-09-20-2 -- an index read of a primitive field off a heap-boxed
//! generic `Array` payload frees every element once.

use super::*;

/// B-2026-09-20-2 — the memory retraction stripped the box's interior with no
/// other owner, leaking each element's `String` (16 B in 7 blocks).
#[test]
fn asan_boxed_generic_array_payload_index_read_frees_elements() {
    assert_clean_asan_run(
        r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
struct Sd { v: i64 }
impl Drop for Sd { fn drop(mut ref self) { println(f"dS{self.v}") } }
enum G1[T] { Y(T), N }
fn mkr(i: i64) -> R { return R { id: i, s: f"s{i}" } }
fn hr(g: G1[Array[R, 1]]) -> i64 { match g { G1.Y(x) => { return x[0].id; } G1.N => { return 0; } } }
fn ha(g: G1[Array[R, 2]]) -> i64 { match g { G1.Y(x) => { let k = x[1].id; return k; } G1.N => { return 0; } } }
fn hc(g: G1[Array[R, 1]]) -> i64 { match g { G1.Y(x) => { x[0].id } G1.N => { 0 } } }
fn hi(g: G1[Array[R, 2]], i: i64) -> i64 { match g { G1.Y(x) => { x[i].id + x[1].id } G1.N => { 0 } } }
fn hl(g: G1[Array[R, 1]]) -> i64 { if let G1.Y(x) = g { return x[0].id; }; return 0 }
fn hs(g: G1[Array[Sd, 2]]) -> i64 { match g { G1.Y(x) => { return x[0].v; } G1.N => { return 0; } } }
fn main() {
    { let a: Array[R, 1] = [mkr(1)]; let g: G1[Array[R, 1]] = G1.Y(a); println(f"r:{hr(g)}"); }
    { let g: G1[Array[R, 2]] = G1.Y([mkr(2), mkr(3)]); println(f"a:{ha(g)}"); }
    { let a: Array[R, 1] = [mkr(4)]; let g: G1[Array[R, 1]] = G1.Y(a); println(f"c:{hc(g)}"); }
    { let g: G1[Array[R, 2]] = G1.Y([mkr(5), mkr(6)]); println(f"i:{hi(g, 0)}"); }
    { let a: Array[R, 1] = [mkr(7)]; let g: G1[Array[R, 1]] = G1.Y(a); println(f"l:{hl(g)}"); }
    { let g: G1[Array[Sd, 2]] = G1.Y([Sd { v: 8 }, Sd { v: 9 }]); println(f"s:{hs(g)}"); }
    { let a: Array[R, 1] = [mkr(10)]; let g: G1[Array[R, 1]] = G1.Y(a); let n = match g { G1.Y(x) => { x[0].id } G1.N => { 0 } }; println(f"m:{n}"); }
    { let a: Array[R, 1] = [mkr(11)]; let g: G1[Array[R, 1]] = G1.Y(a); if let G1.Y(x) = g { let k = x[0].id; println(f"f:{k}"); } println("x"); }
    println("end");
}
"#,
        &[
            "dR1", "r:1", "dR2", "dR3", "a:3", "dR4", "c:4", "dR5", "dR6", "i:11", "dR7", "l:7",
            "dS8", "dS9", "s:8", "dR10", "m:10", "f:11", "dR11", "x", "end",
        ],
        "asan_boxed_generic_array_payload_index_read_frees_elements",
    );
}
