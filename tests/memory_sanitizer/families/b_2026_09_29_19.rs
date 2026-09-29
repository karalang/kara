//! B-2026-09-29-19 -- a field store on a generic struct runs the displaced
//! value's `Drop` body.

use super::*;

/// B-2026-09-29-19 — a field store on a GENERIC struct (`q.u = mk(2)` over
/// `Q[S2]`, `self.u = v` in `impl[U] Q[U]`, `q.u = v` through a `mut ref
/// Q[U]` param) freed the displaced value's memory and never ran its `Drop`
/// body on any compiled surface. The displaced-field body walk declined every
/// generic root. Covers an owned local, a typed local assigned twice, a
/// generic method, a generic free fn, a concrete fn over `mut ref Q[S2]`, a
/// generic root with its own `Drop`, an enum field, a loop, and a `String`
/// field that has no body to run.
#[test]
fn asan_generic_struct_field_store_runs_displaced_drop_body() {
    assert_clean_asan_run(
        r#"shared struct Sh { k: i64 }
struct S2 { h: Sh, id: i64 }
impl Drop for S2 { fn drop(mut ref self) { println(f"dS{self.id}") } }
enum E { A(S2), B }
struct Q[U] { u: U }
impl[U] Q[U] { fn wr(mut ref self, v: U) { self.u = v; } }
struct D[U] { u: U, n: i64 }
impl[U] Drop for D[U] { fn drop(mut ref self) { println(f"dD{self.n}") } }
fn mq[U](q: mut ref Q[U], v: U) { q.u = v; }
fn mc(q: mut ref Q[S2]) { q.u = mk(5); }
fn mk(i: i64) -> S2 { return S2 { h: Sh { k: i }, id: i } }
fn main() {
    { let mut q = Q { u: mk(1) }; q.u = mk(2); println(f"a{q.u.id}") }
    { let mut z: Q[S2] = Q { u: mk(3) }; z.u = mk(4); z.u = mk(6); println(f"b{z.u.id}") }
    { let mut q = Q { u: mk(7) }; q.wr(mk(8)); println(f"c{q.u.id}") }
    { let mut q = Q { u: mk(10) }; mq(mut q, mk(11)); println(f"d{q.u.id}") }
    { let mut q = Q { u: mk(12) }; mc(mut q); println(f"e{q.u.id}") }
    { let mut d = D { u: mk(13), n: 14 }; d.u = mk(15); println(f"f{d.u.id}") }
    { let mut q = Q { u: E.A(mk(16)) }; q.u = E.B; println("g") }
    { let mut q = Q { u: mk(19) }; let mut i = 0; while i < 2 { q.u = mk(20 + i); i = i + 1; } println(f"i{q.u.id}") }
    { let mut s: Q[String] = Q { u: f"a" }; s.u = f"b"; println(f"j{s.u}") }
    println("end")
}
"#,
        &[
            "dS1", "a2", "dS2", "dS3", "dS4", "b6", "dS6", "dS7", "c8", "dS8", "dS10", "d11",
            "dS11", "dS12", "e5", "dS5", "dS13", "f15", "dD14", "dS15", "dS16", "g", "dS19",
            "dS20", "i21", "dS21", "jb", "end",
        ],
        "asan_generic_struct_field_store_runs_displaced_drop_body",
    );
}
