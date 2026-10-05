//! B-2026-09-29-33 / B-2026-10-04-75 — a user enum whose payload is an
//! `Option` / `Result` (heap-boxed, `EnumDropKind::BoxedOptRes`) freed only
//! the box on drop, so the payload's own heap leaked on every compiled
//! surface and an `Option[shared]` payload never ran its `Drop` body. The box
//! drop now walks the interior, and every arm that takes the interior (a leaf
//! binding with an owner of its own, a whole-payload view handed on through a
//! nested `match` or as the arm's value) stands the box's copy down.

use super::*;

/// B-2026-09-29-33 / B-2026-10-04-75 — release with no match, read-only and
/// consuming arms, `if let`, `let ... else`, struct variants, by-value and
/// `ref` params, a nested `match` through a whole-payload binding, loops,
/// `Vec` elements, `Result`, nested `Option`, and `Option[shared]` with a
/// `Drop` body.
#[test]
fn e2e_user_enum_boxed_option_payload_is_dropped_once() {
    let src = r#"
shared struct Nd { v: i64 }
impl Drop for Nd { fn drop(mut ref self) { println(f"dN{self.v}") } }
struct S1 { v: i64, s: String }
enum Hs { P(Option[String]), Q }
enum Hv { P(Option[Vec[i64]]), Q }
enum Hr { P(Result[String, i64]), Q }
enum Hm { P(Option[String], i64), Q }
enum Ho { P(Option[S1]), Q }
enum Hn { N(Option[Nd]), E }
struct W { h: Hs }
fn hs(k: i64) -> String { f"ssssssss{k}" }
fn mk(k: i64) -> Hs { Hs.P(Option.Some(hs(k))) }
fn take(h: Hs) -> i64 { match h { Hs.P(o) => match o { Some(s) => s.len(), None => 0 }, Hs.Q => -1 } }
fn peek(h: ref Hs) -> i64 { match h { Hs.P(o) => match o { Some(s) => s.len(), None => 0 }, Hs.Q => -1 } }
fn gn(h: ref Hn) -> i64 { match h { Hn.N(o) => match o { Some(n) => n.v, None => 1 }, Hn.E => 0 } }
fn outs(h: Hs) -> String { match h { Hs.P(Some(s)) => s, _ => "none" } }
fn outo(h: Hs) -> Option[String] { match h { Hs.P(o) => o, Hs.Q => None } }
fn nest(h: Hs) -> i64 { match h { Hs.P(o) => match o { Some(s) => s.len(), None => 0 }, Hs.Q => -1 } }
fn keepo(o: Option[String]) -> i64 { match o { Some(s) => s.len(), None => 0 } }
fn pass(h: Hs) -> i64 { match h { Hs.P(o) => keepo(o), Hs.Q => -1 } }
fn back(h: Hs) -> Hs { h }
enum Hk { P { o: Option[String], k: i64 }, Q }
enum Hw { P(Option[Vec[String]]), Q }
enum Hq { P(Option[Option[String]]), Q }
fn kk(h: Hk) -> Option[String] { match h { Hk.P { o, k } => o, Hk.Q => None } }
fn le(h: Hs) -> i64 { let Hs.P(Some(s)) = h else { return -1 }; s.len() }
fn main() {
    { let h = Hs.P(Option.Some(hs(1))); println("a") }
    { let h = Hv.P(Option.Some([1, 2, 3])); println("b") }
    { let h = Hr.P(Result.Ok(hs(2))); println("c") }
    { let h = Hm.P(Option.Some(hs(3)), 4); println("d") }
    { let h = mk(5); println(f"e{peek(h)}") }
    { let w = W { h: mk(6) }; println(f"f{peek(w.h)}") }
    { let h = mk(7); match h { Hs.P(o) => println(f"g{o.is_some()}"), Hs.Q => println("q") } }
    { let h = Ho.P(Option.Some(S1 { v: 1, s: hs(8) })); println("h") }
    { let h = Hn.N(Some(Nd { v: 1 })); println("i") }
    { let h = Hn.N(Some(Nd { v: 2 })); println(f"j{gn(h)}") }
    { let h = Hn.N(Some(Nd { v: 3 })); match h { Hn.N(o) => println(f"k{o.is_some()}"), Hn.E => println("e") } }
    { let h = mk(12); println(f"l{take(h)}") }
    { let h = mk(13); let s = outs(h); println(f"m{s}") }
    { let h = mk(14); let o = outo(h); println(f"n{o.is_some()}") }
    { let h = mk(15); match h { Hs.P(Some(s)) => { let t = s; println(f"o{t}") }, _ => println("q") } }
    { let h = mk(16); let g = h; println(f"p{peek(g)}") }
    { let v: Vec[Hs] = [mk(1), mk(2), Hs.Q]; let mut n = 0; for h in v { n = n + take(h); }; println(f"q{n}") }
    { let mut h = mk(18); h = mk(19); println(f"r{peek(h)}") }
    { let h = mk(20); let g = h; println(f"s{peek(g)}") }
    { let h = mk(21); if let Hs.P(Some(s)) = h { println(f"t{s}") } }
    { let h = Hn.N(Some(Nd { v: 4 })); match h { Hn.N(Some(n)) => println(f"u{n.v}"), _ => println("e") } }
    { let h = mk(22); match h { Hs.P(o) => { let p = o; println(f"v{p.is_some()}") }, Hs.Q => println("q") } }
    { let h = mk(23); match h { Hs.P(o) => match o { Some(s) => println(f"w{s.len()}"), None => println("w0") }, Hs.Q => println("q") } }
    { let h = mk(24); println(f"x{nest(h)}") }
    { let h = mk(25); println(f"y{pass(h)}") }
    { let h = mk(26); match h { Hs.P(o) => println(f"z{keepo(o)}"), Hs.Q => println("q") } }
    { let h = mk(28); let w = W { h: h }; println(f"B{peek(w.h)}") }
    { let h = Hr.P(Result.Ok(hs(29))); match h { Hr.P(Ok(s)) => println(f"C{s}"), _ => println("q") } }
    { let h = Ho.P(Option.Some(S1 { v: 30, s: hs(30) })); match h { Ho.P(Some(x)) => println(f"D{x.s}"), _ => println("q") } }
    { let mut i = 0; while i < 3 { let h = mk(i); match h { Hs.P(Some(s)) => println(f"E{s}"), _ => println("q") }; i = i + 1; } }
    { let h = Hm.P(Option.Some(hs(32)), 4); match h { Hm.P(o, k) => println(f"F{o.is_some()}{k}"), Hm.Q => println("q") } }
    { let h = Hk.P { o: Some(hs(33)), k: 1 }; println(f"G{kk(h).is_some()}") }
    { let h = Hk.P { o: Some(hs(34)), k: 1 }; match h { Hk.P { o: Some(s), k } => println(f"H{s}{k}"), _ => println("q") } }
    { let h = Hk.P { o: Some(hs(35)), k: 1 }; println("I") }
    { println(f"J{le(mk(36))}{le(Hs.Q)}") }
    { let h = Hw.P(Some([hs(1), hs(2)])); match h { Hw.P(Some(v)) => println(f"K{v.len()}"), _ => println("q") } }
    { let h = Hw.P(Some([hs(1), hs(2)])); println("L") }
    { let h = Hq.P(Some(Some(hs(39)))); match h { Hq.P(Some(Some(s))) => println(f"M{s}"), _ => println("q") } }
    { let h = Hq.P(Some(Some(hs(40)))); println("N") }
    { let h = Hr.P(Result.Err(7)); match h { Hr.P(Err(e)) => println(f"O{e}"), _ => println("q") } }
    { let h = mk(43); let t = match h { Hs.P(o) => o, Hs.Q => None }; println(f"Q{t.is_some()}") }
    { let h = mk(44); if let Hs.P(o) = h { match o { Some(s) => println(f"R{s}"), None => println("R") } } }
    println("end")
}
"#;
    let want = "a\nb\nc\nd\ne9\nf9\ngtrue\nh\ni\ndN1\nj2\ndN2\nktrue\ndN3\nl10\nmssssssss13\nntrue\nossssssss15\np10\nq17\nr10\ns10\ntssssssss21\nu4\ndN4\nvtrue\nw10\nx10\ny10\nz10\nB10\nCssssssss29\nDssssssss30\nEssssssss0\nEssssssss1\nEssssssss2\nFtrue4\nGtrue\nHssssssss341\nI\nJ10-1\nK2\nL\nMssssssss39\nN\nO7\nQtrue\nRssssssss44\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}

/// B-2026-09-29-33 — a whole-payload view of the boxed `Option` handed on whole
/// (`let u = o`, a by-value argument, a struct field, a `Vec` literal or push,
/// a re-wrapping `Some(o)`, the arm's tail, a move on one branch): the box
/// stands down exactly when the destination took an owner, so the interior is
/// freed once. An `Option[String]` rebind takes no owner, so its box keeps it.
///
/// Before: the gate caught `let u = o` over `Option[S1]` freeing `S1` twice
/// once the box walked its interior, and on the parent tree the by-value
/// argument and `Option[String]` rebind cells each leaked.
#[test]
fn e2e_user_enum_boxed_option_view_moves_whole() {
    let src = r#"struct S1 { v: i64, s: String }
impl Drop for S1 { fn drop(mut ref self) { println(f"dS{self.v}") } }
enum Ho { P(Option[S1]), Q }
enum Hs { P(Option[String]), Q }
struct Wo { o: Option[S1] }
fn s(v: i64) -> S1 { S1 { v: v, s: f"ssssssss{v}" } }
fn tako(o: Option[S1]) -> i64 { 1 }
fn keep(o: Option[S1]) -> i64 { match o { Some(r) => r.v, None => 0 } }
fn outo(h: Ho) -> Option[S1] { match h { Ho.P(o) => o, Ho.Q => None } }
struct Ws { o: Option[String] }
fn st(k: i64) -> String { f"ssssssss{k}" }
fn takes(o: Option[String]) -> i64 { 1 }
fn keeps(o: Option[String]) -> i64 { match o { Some(r) => r.len(), None => 0 } }
fn main() {
    { let h = Ho.P(Option.Some(s(1))); match h { Ho.P(o) => { let u = o; println("in") } Ho.Q => {} } }
    { let h = Ho.P(Option.Some(s(2))); match h { Ho.P(o) => { println(f"{tako(o)}") } Ho.Q => {} } }
    { let h = Ho.P(Option.Some(s(3))); match h { Ho.P(o) => { println(f"{keep(o)}") } Ho.Q => {} } }
    { let h = Ho.P(Option.Some(s(4))); let o2 = match h { Ho.P(o) => o, Ho.Q => None }; println(f"{o2.is_some()}") }
    { let h = Ho.P(Option.Some(s(6))); match h { Ho.P(o) => { let w = Wo { o: o }; println("in") } Ho.Q => {} } }
    { let h = Ho.P(Option.Some(s(7))); match h { Ho.P(o) => { let v: Vec[Option[S1]] = [o]; println("in") } Ho.Q => {} } }
    { let h = Ho.P(Option.Some(s(8))); match h { Ho.P(o) => { let c = true; if c { let u = o; println("a") } else { println("b") } } Ho.Q => {} } }
    { let h = Hs.P(Option.Some(st(1))); match h { Hs.P(o) => { let u = o; println(f"{u.is_some()}") } Hs.Q => {} } }
    { let h = Hs.P(Option.Some(st(2))); match h { Hs.P(o) => { println(f"{takes(o)}") } Hs.Q => {} } }
    { let h = Hs.P(Option.Some(st(3))); match h { Hs.P(o) => { println(f"{keeps(o)}") } Hs.Q => {} } }
    { let h = Hs.P(Option.Some(st(6))); match h { Hs.P(o) => { let w = Ws { o: o }; println("in") } Hs.Q => {} } }
    { let h = Hs.P(Option.Some(st(7))); match h { Hs.P(o) => { let v: Vec[Option[String]] = [o]; println(f"{v.len()}") } Hs.Q => {} } }
    { let h = Hs.P(Option.Some(st(12))); match h { Hs.P(o) => { let w = Some(o); println("in") } Hs.Q => {} } }
    { let h = Hs.P(Option.Some(st(13))); match h { Hs.P(o) => { let mut v: Vec[Option[String]] = []; v.push(o); println(f"{v.len()}") } Hs.Q => {} } }
    println("end")
}
"#;
    let want =
        "dS1\nin\n1\n3\ntrue\ndS4\ndS6\nin\ndS7\nin\ndS8\na\ntrue\n1\n9\nin\n1\nin\n1\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
