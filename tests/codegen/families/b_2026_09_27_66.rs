//! B-2026-09-27-66 — a `for` loop copy of a boxed generic enum element handed on by value.

use super::*;

/// B-2026-09-27-66 — a `for` binding over a `Vec` of a BOXED generic enum
/// (`Ho[P]`, `Ho[String]`) owns a box copy of its own for the iteration, and a
/// by-value call hands that box over correctly. The binding never recorded its
/// enum instantiation, so the argument paths that key on it declined: a callee
/// that hands the box back (`keep(h)`) left the binding freeing the box its
/// result also owned, and two by-value calls in turn (`shows(h); shows(h)`)
/// handed both callees the one box. Both crashed on every compiled surface.
/// The payloads carry no `Drop` body on purpose: moving a bare `for` element
/// whose type runs one is being made a compile error (`for_element_drop_copy`),
/// and the crash never needed one. Covers the escape into a `Vec` (then walked),
/// the twice-consumed spelling, a bound and a chained hand-back, a hand-back
/// followed by a second use, a two-param callee that hands one param back and
/// consumes the other, and a `String` payload.
#[test]
fn e2e_for_loop_boxed_generic_enum_copy_handed_on_by_value() {
    let Some(out) = run_program(
        r#"struct P { id: i64, s: String }
enum Ho[T] { Full(T), Empty }
fn mkp(i: i64) -> P { return P { id: i, s: "ab".to_string() + "cd" } }
fn shows(h: Ho[P]) { match h { Ho.Full(r) => println(f"s{r.id}{r.s}"), Ho.Empty => println("e") } }
fn keep(h: Ho[P]) -> Ho[P] { h }
fn pick(a: Ho[P], b: Ho[P]) -> Ho[P] { shows(b); a }
fn keept(h: Ho[String]) -> Ho[String] { h }
fn showt(h: Ho[String]) { match h { Ho.Full(r) => println(f"t{r}"), Ho.Empty => println("e") } }
fn mkv(a: i64, b: i64) -> Vec[Ho[P]] {
    let mut v: Vec[Ho[P]] = Vec.new();
    v.push(Ho.Full(mkp(a)));
    v.push(Ho.Empty);
    v.push(Ho.Full(mkp(b)));
    return v
}
fn main() {
    println("escape");
    {
        let v = mkv(1, 2);
        let mut w: Vec[Ho[P]] = Vec.new();
        for h in v { w.push(keep(h)) }
        println(f"n{v.len()}{w.len()}");
        for x in w { shows(x) }
    }
    println("twice");
    {
        let v = mkv(3, 4);
        for h in v { shows(h); shows(h) }
    }
    println("bound");
    {
        let v = mkv(5, 6);
        for h in v { let k = keep(h); println("got") }
    }
    println("chain");
    {
        let v = mkv(7, 8);
        for h in v { let k = keep(h); let j = keep(k); shows(j) }
    }
    println("mixed");
    {
        let v = mkv(9, 10);
        let mut w: Vec[Ho[P]] = Vec.new();
        for h in v { w.push(keep(h)); shows(h) }
        println(f"n{w.len()}")
    }
    println("pick");
    {
        let v = mkv(11, 12);
        for h in v { let k = pick(h, Ho.Full(mkp(99))); shows(k) }
    }
    println("string");
    {
        let mut v: Vec[Ho[String]] = Vec.new();
        v.push(Ho.Full("x".to_string() + "y"));
        v.push(Ho.Empty);
        let mut w: Vec[Ho[String]] = Vec.new();
        for h in v { w.push(keept(h)); showt(h) }
        for x in w { showt(x) }
    }
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "escape\nn33\ns1abcd\ne\ns2abcd\ntwice\ns3abcd\ns3abcd\ne\ne\ns4abcd\ns4abcd\nbound\ngot\ngot\ngot\nchain\ns7abcd\ne\ns8abcd\nmixed\ns9abcd\ne\ns10abcd\nn3\npick\ns99abcd\ns11abcd\ns99abcd\ne\ns99abcd\ns12abcd\nstring\ntxy\ne\ntxy\ne\nend\n", "got:\n{out}");
}
