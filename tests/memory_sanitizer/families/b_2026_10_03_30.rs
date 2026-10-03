//! B-2026-10-03-30 -- `self.clone()` inside an impl body lowers like a clone
//! of a named binding.

use super::*;

/// B-2026-10-03-30 — `try_compile_clone` read only an `Identifier` receiver,
/// so `self.clone()` fell through to the loud "no handler for method 'clone'
/// on non-identifier receiver" bail and `karac build` failed while `--interp`
/// ran the program. Covers owned and `ref` receivers over a `shared struct`, a
/// `shared enum`, a `#[derive(Clone)]` struct with heap fields, and a
/// `#[derive(Clone)]` enum.
#[test]
fn asan_self_clone_in_impl_body() {
    assert_clean_asan_run(
        r#"shared struct S { k: i64, s: String }
impl S { fn me(self) -> S { return self.clone(); } fn me2(ref self) -> S { return self.clone(); } }
shared enum E { P(i64, String), Q }
impl E {
    fn me(self) -> E { return self.clone(); }
    fn me2(ref self) -> E { return self.clone(); }
    fn n(ref self) -> i64 { match self { E.P(n, _) => n, E.Q => 0 } }
}
#[derive(Clone)]
struct Pd { s: String, v: Vec[i64] }
impl Pd { fn dup(ref self) -> Pd { return self.clone(); } fn dup2(self) -> Pd { return self.clone(); } }
#[derive(Clone)]
enum O { A(String), B }
impl O { fn dup(ref self) -> O { return self.clone(); } }
fn main() {
    let a = S { k: 1, s: f"s{1}" };
    let b = a.me(); let c = a.me2();
    println(f"s {a.k} {b.k} {c.s}");
    let e = E.P(2, f"x{1}"); let f = e.me(); let g = e.me2();
    println(f"e {e.n()} {f.n()} {g.n()}");
    let p = Pd { s: f"p{1}", v: [1, 2] };
    let q = p.dup(); let r = p.dup2();
    println(f"p {q.s} {q.v.len()} {r.s} {r.v[1]}");
    let o = O.A(f"o{1}"); let o2 = o.dup();
    match o2 { O.A(t) => { println(f"o {t}"); } O.B => {} }
}
"#,
        &["s 1 1 s1", "e 2 2 2", "p p1 2 p1 2", "o o1"],
        "B-2026-10-03-30 self.clone() in an impl body",
    );
}
