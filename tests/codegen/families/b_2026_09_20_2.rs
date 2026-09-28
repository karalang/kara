//! B-2026-09-20-2 -- an arm over a heap-boxed generic `Array[T, N]` payload
//! that READS one primitive field through an index (`x[0].id`) leaves the
//! payload with its owner: every element's `Drop` body runs once and its heap
//! is freed.

use super::*;

/// B-2026-09-20-2 — before the fix the leaf-aware copy-read test stopped at
/// the binding, so `x[0].id` read as a partial move of `x`: every cell but the
/// interpolation-only `i:` lost its `dR`/`dS` lines, and valgrind reported
/// 16 B lost in 7 blocks (one `String` per `R`). The bodies land at the
/// callee's return, which is the due order for a by-value generic-enum param
/// (B-2026-09-20-32 / B-2026-09-27-108 track the late spellings).
#[test]
fn e2e_boxed_generic_array_payload_index_read_keeps_bodies() {
    let Some(out) = run_program(
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
    ) else {
        return;
    };
    assert_eq!(out, "dR1\nr:1\ndR2\ndR3\na:3\ndR4\nc:4\ndR5\ndR6\ni:11\ndR7\nl:7\ndS8\ndS9\ns:8\ndR10\nm:10\nf:11\ndR11\nx\nend\n", "got:\n{out}");
}
