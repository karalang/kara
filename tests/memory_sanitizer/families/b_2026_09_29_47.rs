//! B-2026-09-29-47 -- a field moved out before a `..` destructure of the same
//! struct is copied with its `shared` refcount bumped, so each owner releases
//! its own reference.

use super::*;

/// B-2026-09-29-47 -- a field moved out of a struct (`let x = q.u`) before a
/// `..` destructure of the same struct (`let W3 { n, .. } = q`). The
/// destructure makes the move a use-after-move site, so codegen copies the
/// field and the source keeps it; the copy shared the field's `shared` box
/// without a reference of its own. `x`'s drop freed the box and `q`'s drop
/// read and wrote it again: `Invalid read/write of size 8`, and `malloc():
/// unaligned tcache chunk` at -O0. Covers an i64 / String / droppable rest, a
/// `u: _` pattern, a nested struct field, a generic struct, a bare `shared`
/// field, a `match` destructure, a loop, a by-value param (temp and named
/// argument) and a by-value `self` method; the last three must NOT take a
/// reference, since the caller releases the argument.
#[test]
fn asan_field_moved_before_rest_destructure_owns_its_shared_box() {
    assert_clean_asan_run(
        r#"shared struct Sh { k: i64 }
struct S2 { h: Sh, id: i64 }
impl Drop for S2 { fn drop(mut ref self) { println(f"dS{self.id}") } }
fn mk(i: i64) -> S2 { return S2 { h: Sh { k: i }, id: i } }
struct W3 { u: S2, n: i64 }
struct W5 { u: S2, s: String }
struct W6 { u: S2, v: S2 }
struct S3 { s2: S2, t: String }
struct W7 { u: S3, n: i64 }
struct Q[U] { u: U, n: i64 }
struct W4 { s: Sh, n: i64 }
impl W3 { fn go(self) -> i64 { let x = self.u; let W3 { n, .. } = self; println(f"x{x.id}"); return n } }
fn pv(q: W3) -> i64 { let x = q.u; let W3 { n, .. } = q; println(f"x{x.id}"); return n }
fn c1() { let mut q = W3 { u: mk(9), n: 2 }; let x = q.u; let W3 { n, .. } = q; println(f"x{x.id}{n}") }
fn c2() { let q = W5 { u: mk(9), s: "a" }; let x = q.u; let W5 { s, .. } = q; println(f"x{x.id}{s}") }
fn c3() { let q = W6 { u: mk(9), v: mk(4) }; let x = q.u; let W6 { v, .. } = q; println(f"x{x.id}{v.id}") }
fn c4() { let q = W3 { u: mk(9), n: 2 }; let x = q.u; let W3 { u: _, n } = q; println(f"x{x.id}{n}") }
fn c5() { let q = W7 { u: S3 { s2: mk(9), t: "tt" }, n: 2 }; let x = q.u; let W7 { n, .. } = q; println(f"x{x.s2.id}{x.t}{n}") }
fn c6() { let q = Q { u: mk(9), n: 2 }; let x = q.u; let Q { n, .. } = q; println(f"x{x.id}{n}") }
fn c7() { let q = W4 { s: Sh { k: 5 }, n: 2 }; let x = q.s; let W4 { n, .. } = q; println(f"x{x.k}{n}") }
fn c8() { let q = W3 { u: mk(9), n: 2 }; let x = q.u; match q { W3 { n, .. } => println(f"m{n}") } println(f"x{x.id}") }
fn c9() { let mut i = 0; while i < 2 { let q = W3 { u: mk(i), n: 2 }; let x = q.u; let W3 { n, .. } = q; println(f"x{x.id}{n}"); i += 1; } }
fn c10() { println(f"r{pv(W3 { u: mk(9), n: 2 })}") }
fn c11() { let w = W3 { u: mk(9), n: 2 }; println(f"r{pv(w)}") }
fn c12() { let w = W3 { u: mk(9), n: 2 }; println(f"r{w.go()}") }
fn main() {
  println("-c1"); c1();
  println("-c2"); c2();
  println("-c3"); c3();
  println("-c4"); c4();
  println("-c5"); c5();
  println("-c6"); c6();
  println("-c7"); c7();
  println("-c8"); c8();
  println("-c9"); c9();
  println("-c10"); c10();
  println("-c11"); c11();
  println("-c12"); c12();
  println("end")
}
"#,
        &[
            "-c1", "x92", "dS9", "-c2", "x9a", "dS9", "-c3", "x94", "dS4", "dS9", "-c4", "x92",
            "dS9", "-c5", "x9tt2", "dS9", "-c6", "x92", "dS9", "-c7", "x52", "-c8", "m2", "x9",
            "dS9", "-c9", "x02", "dS0", "x12", "dS1", "-c10", "x9", "dS9", "r2", "-c11", "x9",
            "r2", "dS9", "-c12", "x9", "r2", "dS9", "end",
        ],
        "field_moved_before_rest_destructure",
    );
}
