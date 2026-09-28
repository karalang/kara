//! B-2026-09-28-64 -- a view bound out of a generic `shared enum`'s boxed
//! payload and handed on is freed once.

use super::*;

/// B-2026-09-28-64 — B-2026-09-19-53 gave a generic `shared enum G[T]`'s
/// boxed payload an interior walk for every payload type, but a tuple, enum,
/// `Option` or `Array` arm binding is a VIEW of that interior: returning it,
/// making it an arm's value, or storing it into a struct field or a `Vec`
/// handed the interior to a new owner while the box still freed it
/// (`free(): double free detected in tcache 2` on every compiled backend).
/// Also pins the `G[String]` object that is never matched: its payload type
/// spells `str`, and must keep its interior walk.
#[test]
fn e2e_shared_generic_enum_payload_view_handed_on_freed_once() {
    let Some(out) = run_program(
        r#"shared enum G[T] { Y(T), N }
enum E { A(String), B }
struct W { t: (String, i64) }
fn tt(g: G[(String, i64)]) -> (String, i64) { match g { G.Y(x) => { return x } G.N => { return (f"n", 0) } } }
fn tt2(g: G[(String, i64)]) -> (String, i64) { match g { G.Y(x) => x, G.N => (f"n", 0) } }
fn te(g: G[E]) -> E { match g { G.Y(x) => x, G.N => E.B } }
fn to(g: G[Option[String]]) -> Option[String] { match g { G.Y(x) => { return x } G.N => { return None } } }
fn ta(g: G[Array[String, 2]]) -> Array[String, 2] { match g { G.Y(x) => x, G.N => [f"n", f"n"] } }
fn tw(g: G[(String, i64)]) -> W { match g { G.Y(x) => W { t: x }, G.N => W { t: (f"n", 0) } } }
fn tp(g: G[(String, i64)]) -> Vec[(String, i64)] { let mut v: Vec[(String, i64)] = Vec.new(); if let G.Y(x) = g { v.push(x) } return v }
fn main() {
    let mut i = 0;
    while i < 2 {
        let a = tt(G.Y((f"a{i}", 1)));
        let b = tt2(G.Y((f"bb{i}", 2)));
        let c = te(G.Y(E.A(f"ccc{i}")));
        let d = to(G.Y(Some(f"dd{i}")));
        let e = ta(G.Y([f"e{i}", f"ee{i}"]));
        let w = tw(G.Y((f"w{i}", 3)));
        let v = tp(G.Y((f"v{i}", 4)));
        let cl = match c { E.A(s) => s.len(), E.B => 0 };
        let dl = match d { Some(s) => s.len(), None => 0 };
        println(f"{a.0}{a.1} {b.0}{b.1} {cl} {dl} {e[1]} {w.t.0}{w.t.1} {v[0].0}{v[0].1}");
        i = i + 1;
    }
    { let g: G[String] = G.Y(f"ab"); println("s"); }
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out, "a01 bb02 4 3 ee0 w03 v04\na11 bb12 4 3 ee1 w13 v14\ns\nend\n",
        "got:\n{out}"
    );
}
