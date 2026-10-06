//! B-2026-10-06-100 — per-arm payload ownership in a `match` over a named enum local.

use super::*;

/// Whether a `match` arm's payload bindings are views of a named enum local
/// is decided per ARM. When one arm moved a payload out, every arm's bindings
/// became owners: an arm that only read a `Vec` payload took the local's
/// buffer and ran no element body on the compiled backends, and the
/// interpreter's whole-match disarm ran it twice. Covers a reading arm beside
/// a moving one (and the reverse), an enum with its own `Drop`, a two-field
/// variant, a partial take, a `let`-bound match value, and a match in a loop.
#[test]
fn interp_match_arm_payload_views_decided_per_arm() {
    let out = run(r#"struct D { n: i64, s: String }
impl Drop for D { fn drop(mut ref self) { println(f"d{self.n}") } }
fn d(n: i64) -> D { D { n: n, s: f"heap-string-longer-than-sso-{n}" } }
fn eat(x: D) { println(f"eat{x.n}") }
fn eatv(x: Vec[D]) { println(f"eatv{x.len()}") }
enum E { One(D), Many(Vec[D]), No }
enum F { One(D), Many(Vec[D]) }
impl Drop for F { fn drop(mut ref self) { println("dF") } }
enum T { Two(D, D), One(D) }
fn main() {
    let a = E.Many(vec![d(1), d(2)]);
    match a { E.One(x) => eat(x), E.Many(xs) => println(f"m{xs.len()}"), E.No => println("no") }
    println("A");
    let b = E.One(d(3));
    match b { E.One(x) => println(f"o{x.n}"), E.Many(xs) => eatv(xs), E.No => println("no") }
    println("B");
    let c = F.Many(vec![d(4), d(5)]);
    match c { F.One(x) => eat(x), F.Many(xs) => println(f"m{xs.len()}") }
    println("C");
    let e = F.One(d(6));
    match e { F.One(x) => eat(x), F.Many(xs) => println(f"m{xs.len()}") }
    println("D");
    let t = T.Two(d(7), d(8));
    match t { T.One(x) => eat(x), T.Two(p, q) => println(f"t{p.n}{q.n}") }
    println("E");
    let u = T.Two(d(9), d(10));
    match u { T.One(x) => println(f"o{x.n}"), T.Two(p, _) => eat(p) }
    println("F");
    let g = E.Many(vec![d(11)]);
    let k = match g { E.One(x) => { eat(x); 0 } E.Many(xs) => xs.len(), E.No => 0 };
    println(f"k{k}");
    let mut v: Vec[D] = Vec.new();
    for i in 0..2 {
        let h = if i == 0 { E.One(d(12)) } else { E.Many(vec![d(13)]) };
        match h { E.One(x) => v.push(x), E.Many(xs) => println(f"m{xs.len()}"), E.No => println("no") }
    }
    println(f"v{v.len()}");
    println("end")
}
"#);
    assert_eq!(out, "m2\nd1\nd2\nA\no3\nd3\nB\nm2\ndF\nd4\nd5\nC\neat6\nd6\ndF\nD\nt78\nd8\nd7\nE\neat9\nd9\nd10\nF\nd11\nk1\nm1\nd13\nv1\nd12\nend\n");
}
