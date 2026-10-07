//! B-2026-09-29-100 -- a view local of a caller-retained param's nested
//! struct field leaves as a clone at every owning sink.

use super::*;

/// B-2026-09-29-100 -- a local bound straight from a nested struct field of a
/// by-value struct param that holds a `shared` value (`let x = q.u;` over
/// `W { u: S }`, `S { h: Sh, id: i64 }`) is a view of the caller's argument,
/// which the caller frees whole. Put into an owning sink -- a struct literal,
/// `Some`, `Vec.push` or a tuple -- it went in bit-for-bit, so the sink and the
/// caller's argument released one heap (a garbage `h.k`, an abort in `malloc`
/// at -O0). B-2026-09-29-79 covered only `return x`. Covers each sink, the
/// struct literal after a `..` destructure, and a field with a `String`.
#[test]
fn asan_caller_retained_view_local_into_owning_sink_is_cloned() {
    assert_clean_asan_run(
        r#"shared struct Sh { k: i64 }
struct S2 { h: Sh, id: i64 }
impl Drop for S2 { fn drop(mut ref self) { println(f"dS{self.id}") } }
fn mk(i: i64) -> S2 { return S2 { h: Sh { k: i }, id: i } }
struct S5 { h: Sh, t: String, id: i64 }
impl Drop for S5 { fn drop(mut ref self) { println(f"dT{self.id}") } }
struct W3 { u: S2, n: i64 }
struct W7 { u: S5, n: i64 }
struct K { s: S2 }
fn f1(q: W3) -> K { let x = q.u; return K { s: x } }
fn f2(q: W3) -> K { let x = q.u; let W3 { n, .. } = q; println(f"n{n}"); return K { s: x } }
fn f3(q: W3) -> Option[S2] { let x = q.u; return Some(x) }
fn f4(q: W3) -> Vec[S2] { let x = q.u; let mut v: Vec[S2] = Vec.new(); v.push(x); return v }
fn f5(q: W3) -> (S2, i64) { let x = q.u; return (x, q.n) }
fn f6(q: W7) -> Option[S5] { let x = q.u; return Some(x) }
fn f7(q: W7) -> Vec[S5] { let x = q.u; let W7 { n, .. } = q; println(f"n{n}"); let mut v: Vec[S5] = Vec.new(); v.push(x); return v }
fn main() {
  println("-c1"); let a = f1(W3 { u: mk(9), n: 2 }); println(f"r{a.s.id}{a.s.h.k}");
  println("-c2"); let b = f2(W3 { u: mk(8), n: 3 }); println(f"r{b.s.id}{b.s.h.k}");
  println("-c3"); let c = f3(W3 { u: mk(7), n: 2 }); if let Some(x) = c { println(f"r{x.id}{x.h.k}") }
  println("-c4"); let d = f4(W3 { u: mk(6), n: 2 }); println(f"r{d.len()}{d[0].id}{d[0].h.k}");
  println("-c5"); let (e, m) = f5(W3 { u: mk(5), n: 4 }); println(f"r{e.id}{e.h.k}{m}");
  println("-c6"); let g = f6(W7 { u: S5 { h: Sh { k: 4 }, t: f"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa{1}", id: 4 }, n: 2 }); if let Some(x) = g { println(f"r{x.id}{x.h.k}{x.t.len()}") }
  println("-c7"); let h = f7(W7 { u: S5 { h: Sh { k: 3 }, t: f"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbb{2}", id: 3 }, n: 5 }); println(f"r{h.len()}{h[0].id}{h[0].t.len()}");
  println("end")
}
"#,
        &[
            "-c1", "r99", "dS9", "-c2", "n3", "r88", "dS8", "-c3", "r77", "dS7", "-c4", "r166",
            "dS6", "-c5", "r554", "dS5", "-c6", "r4431", "dT4", "-c7", "n5", "r1331", "dT3", "end",
        ],
        "caller_retained_view_local_into_owning_sink_is_cloned",
    );
}
