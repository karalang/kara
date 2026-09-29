//! B-2026-09-29-14 -- a generic struct borrowed by a generic callee keeps
//! its caller's cleanup.

use super::*;

/// B-2026-09-29-14 — a generic struct passed by `ref` to a GENERIC callee
/// (`q.rd()` over `impl[U] Q[U] { fn rd(ref self) -> i64 }`, `rq(q)` over `fn
/// rq[U](q: ref Q[U])`) never ran its field's `Drop` body and leaked the
/// field's `shared` box on every compiled surface: `compile_generic_call`
/// retracted the caller's cleanup for every named struct argument whose type
/// is owned by transfer, whatever the param's mode. Covers a `ref self`
/// receiver, a `ref` free-fn param, a method with more arguments, the
/// conditional hand-back `qu` (whose double free B-2026-09-27-1 fixed), a
/// generic struct with its own `Drop`, an `Array` field, and a loop.
#[test]
fn asan_generic_struct_borrowed_by_generic_callee_keeps_its_cleanup() {
    assert_clean_asan_run(
        r#"shared struct Sh { k: i64 }
struct S2 { h: Sh, id: i64 }
impl Drop for S2 { fn drop(mut ref self) { println(f"dS{self.id}") } }
struct Q[U] { u: U }
impl[U] Q[U] {
    fn rd(ref self) -> i64 { return 1 }
    fn eat(ref self, v: U) -> i64 { return 2 }
    fn qu(ref self, v: U, c: bool, w: U) -> U { if c { return v } return w }
}
struct D[U] { u: U, n: i64 }
impl[U] Drop for D[U] { fn drop(mut ref self) { println(f"dD{self.n}") } }
impl[U] D[U] { fn rd(ref self) -> i64 { return self.n } }
fn rq[U](q: ref Q[U]) -> i64 { return 3 }
fn mk(i: i64) -> S2 { return S2 { h: Sh { k: i }, id: i } }
fn main() {
    { let q = Q { u: mk(1) }; println(f"a{q.rd()}") }
    { let q = Q { u: mk(2) }; println(f"b{rq(q)}") }
    { let q: Q[S2] = Q { u: mk(3) }; let n = q.rd(); println(f"c{n}") }
    { let q = Q { u: mk(4) }; println(f"d{q.eat(mk(5))}") }
    { let q = Q { u: mk(6) }; let t = q.qu(mk(7), true, mk(8)); println(f"e{t.id} {q.u.id}") }
    { let q = Q { u: mk(9) }; let t = q.qu(mk(10), false, mk(11)); println(f"f{t.id}") }
    { let d = D { u: mk(12), n: 13 }; println(f"g{d.rd()}") }
    { let q = Q { u: [mk(14)] }; println(f"h{q.rd()} {rq(q)}") }
    { let q = Q { u: mk(15) }; let mut i = 0; while i < 2 { println(f"i{q.rd()}"); i = i + 1; } }
    println("end")
}
"#,
        &[
            "a1", "dS1", "b3", "dS2", "dS3", "c1", "dS5", "d2", "dS4", "dS8", "e7 6", "dS7", "dS6",
            "dS10", "dS9", "f11", "dS11", "g13", "dD13", "dS12", "h1 3", "dS14", "i1", "i1",
            "dS15", "end",
        ],
        "asan_generic_struct_borrowed_by_generic_callee_keeps_its_cleanup",
    );
}
