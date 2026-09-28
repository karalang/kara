//! B-2026-09-28-61 -- a generic `shared enum`'s boxed payload runs its
//! `Drop` bodies when the object dies.

use super::*;

/// B-2026-09-28-61 — a generic `shared enum G[T]` over a payload that runs a
/// user `Drop` body (`G[R]`, `G[Vec[R]]`, `G[Array[R, 2]]`, a struct holding
/// an `R`) ran none of those bodies on any compiled backend and leaked the
/// payload's heap: the per-object release fn freed only the box envelope.
/// It now runs the payload's bodies and then its memory. Moving such a
/// payload out by value is an E0514 compile error (tests/rc_fallback.rs),
/// so the release fn is its one owner; every cell here reads the payload
/// in place.
#[test]
fn e2e_shared_generic_enum_drop_payload_bodies_run_once() {
    let Some(out) = run_program(
        r#"shared enum G[T] { Y(T), N }
shared enum M { Y(R), N }
struct R { id: i64, s: String }
struct W { r: R, k: i64 }
shared enum T2 { Y((R, i64)), N }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(n: i64) -> R { return R { id: n, s: f"s{n}" } }
fn eat(r: R) -> i64 { return r.id }
fn look(r: ref R) -> i64 { return r.id }
fn main() {
    { let r: R = mk(1); let g: G[R] = G.Y(r); println("r"); }
    { let g: G[R] = G.Y(mk(2)); println("rt"); }
    { let g: G[R] = G.Y(mk(3)); match g { G.Y(x) => { println(f"x{x.id} {look(x)}") } G.N => {} }; println("a"); }
    { let g: G[R] = G.Y(mk(4)); let h = g; if let G.Y(x) = h { println(f"i{x.id}") }; println(f"a"); }
    { let g: G[R] = G.Y(mk(5)); let v: Vec[G[R]] = [g]; println(f"v{v.len()}"); }
    { let g: G[Vec[R]] = G.Y([mk(6), mk(7)]); match g { G.Y(v) => { println(f"n{v.len()}") } G.N => {} }; println("a"); }
    { let m: M = M.Y(mk(8)); match m { M.Y(x) => { println(f"m{x.id}") } M.N => {} }; println("a"); }
    { let g: G[W] = G.Y(W { r: mk(10), k: 1 }); match g { G.Y(w) => { println(f"w{w.r.id}") } G.N => {} }; println("a"); }
    { let g: G[R] = G.Y(mk(12)); match g { G.Y(x) => { println(f"p{x.id}") } G.N => {} }; match g { G.Y(x) => { println(f"q{x.id}") } G.N => {} }; println("a"); }
    { let g: G[R] = G.Y(mk(14)); let h = g; println("h"); match h { G.Y(x) => { println(f"x{x.id}") } G.N => {} }; println("a"); }
    { let g: G[Array[R, 2]] = G.Y([mk(15), mk(16)]); match g { G.Y(a) => { println(f"a{a[1].id}") } G.N => {} }; println("a"); }
    { let g: G[R] = G.Y(mk(17)); let r = match g { G.Y(x) => x.id, G.N => 0 }; println(f"{r}"); }
    { let g: G[R] = G.Y(mk(18)); if let G.Y(x) = g { println(f"{x.id}") }; println("b") }
    { let g: G[R] = G.Y(mk(19)); match g { G.Y(x) => { println("m"); println(f"{x.id}") } G.N => {} } }
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "dR1\nr\ndR2\nrt\nx3 3\ndR3\na\ni4\ndR4\na\nv1\ndR5\nn2\ndR6\ndR7\na\nm8\ndR8\na\nw10\ndR10\na\np12\nq12\ndR12\na\nh\nx14\ndR14\na\na16\ndR15\ndR16\na\ndR17\n17\n18\ndR18\nb\nm\n19\ndR19\nend\n", "got:\n{out}");
}
