//! B-2026-10-01-15 / B-2026-10-02-55 / B-2026-09-29-112 -- element field reads
//! on fixed arrays codegen had no element type for. These shapes failed to
//! build before; this checks that compiling them owns each element once.

use super::*;

/// A `match`/`if`/block-initialized array binding, a `for` over a struct
/// array, and arrays held in unannotated tuples, each element body once and
/// no leak (the leak half is carried by the `-O0` leg).
#[test]
fn asan_array_element_field_reads_without_recorded_type() {
    assert_clean_asan_run(
        r#"struct W { w: i64, k: i64 }
struct D { id: i64, w: W, s: String }
impl Drop for D { fn drop(mut ref self) { println(f"dD{self.id}") } }
fn mkd(n: i64) -> D { return D { id: n, w: W { w: n * 10, k: n * 100 }, s: f"sss{n}" } }
enum EArr { A(Array[D, 2]), B }
fn z() -> Array[D, 2] { return [mkd(0), mkd(0)]; }
fn e1(x: EArr) -> i64 { let k = match x { EArr.A(v) => v, EArr.B => z() }; return k[1].id + k[0].w.k; }
fn blk() -> i64 { let a = { let q = 3; z() }; return a[0].w.w + 1; }
fn main() {
  { let a: Array[D, 2] = [mkd(1), mkd(2)]; let t = (a, 7); println(f"n1 {t.0[1].w.w} {t.0[0].w.k} {t.0[1].s}") }
  { let a: Array[D, 2] = [mkd(3), mkd(4)]; let t = (7, a); println(f"n2 {t.1[0].w.w} {t.1[1].id}") }
  println(f"n3 {e1(EArr.A([mkd(5), mkd(6)]))}");
  println(f"n4 {e1(EArr.B)}");
  println(f"n5 {blk()}");
  { let c = false; let l = if c { z() } else { [mkd(8), mkd(9)] }; for q in l { println(f"n6 {q.w.k} {q.s}") } }
  println("end")
}
"#,
        &[
            "n1 20 100 sss2",
            "dD1",
            "dD2",
            "n2 30 4",
            "dD3",
            "dD4",
            "dD5",
            "dD6",
            "n3 506",
            "dD0",
            "dD0",
            "n4 0",
            "dD0",
            "dD0",
            "n5 1",
            "n6 800 sss8",
            "n6 900 sss9",
            "dD8",
            "dD9",
            "end",
        ],
        "array_elem_field_unrecorded_type",
    );
}
