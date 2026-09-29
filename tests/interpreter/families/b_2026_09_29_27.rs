//! B-2026-09-29-27: an `Option` param stored on one path and payload-taken on another.

use super::*;

/// B-2026-09-29-27 — a by-value `Option` parameter stored whole on some paths and
/// its payload bound out of a `match` / `if let` on others (`if c { v.push(t)
/// } else { let y = match t { .. }; }`) runs the payload's body exactly once
/// on every path, the third path that does neither included. Before, a
/// heap-boxed payload crashed compiled (the caller kept the box the storing
/// path had put in the container, and freed it twice), and on every surface
/// the path that neither stored nor took ran no body.
#[test]
fn interp_store_on_one_path_and_payload_take_on_another_of_a_by_value_option_param() {
    let out = run(r#"struct S { id: i64, s: String }
impl Drop for S { fn drop(mut ref self) { println(f"dS{self.id}") } }
fn mks(i: i64) -> S { return S { id: i, s: f"heap-string-longer-than-sso-{i}" } }
struct P { id: i64 }
impl Drop for P { fn drop(mut ref self) { println(f"dP{self.id}") } }
struct H { k: i64 }
impl H {
    fn mp(ref self, t: Option[S], v: mut ref Vec[Option[S]], c: bool) -> i64 { if c { v.push(t) } else { let y = match t { Option.Some(q) => q, Option.None => mks(0) }; println(f"m{y.id}") }; return self.k }
}
fn cp(t: Option[S], v: mut ref Vec[Option[S]], c: i64) -> i64 { if c == 0 { v.push(t) } else if c == 1 { let y = match t { Option.Some(q) => q, Option.None => mks(0) }; println(f"p{y.id}") } else { println("pn") }; return 1 }
fn ci(t: Option[P], v: mut ref Vec[Option[P]], c: i64) -> i64 { if c == 0 { v.push(t) } else if c == 1 { let y = match t { Option.Some(q) => q, Option.None => P { id: 0 } }; println(f"i{y.id}") } else { println("in") }; return 1 }
fn cl(t: Option[S], v: mut ref Vec[Option[S]], c: bool) -> i64 { if c { let y = if let Option.Some(q) = t { q } else { mks(0) }; println(f"l{y.id}") } else { v.push(t) }; return 1 }
fn ce(t: Option[S], v: mut ref Vec[Option[S]], c: bool) -> i64 { if c { v.push(t); return 2 }; let y = match t { Option.Some(q) => q, Option.None => mks(0) }; println(f"e{y.id}"); return 1 }
fn cn(t: Option[S], v: mut ref Vec[Option[S]], c: bool) -> i64 { if c { v.push(t) } else { let y = match t { Option.Some(q) => q, Option.None => { return 0 } }; println(f"n{y.id}") }; return 1 }
fn main() {
    let mut w: Vec[Option[S]] = Vec.new();
    let a = cp(Option.Some(mks(1)), mut w, 1) + cp(Option.Some(mks(2)), mut w, 0) + cp(Option.Some(mks(3)), mut w, 2) + cp(Option.None, mut w, 1);
    println(f"a{a}");
    let o = Option.Some(mks(4));
    let o2 = Option.Some(mks(5));
    let b = cp(o, mut w, 1) + cp(o2, mut w, 0);
    println(f"b{b}");
    let mut wi: Vec[Option[P]] = Vec.new();
    let c = ci(Option.Some(P { id: 6 }), mut wi, 1) + ci(Option.Some(P { id: 7 }), mut wi, 0) + ci(Option.Some(P { id: 8 }), mut wi, 2);
    println(f"c{c}");
    let d = cl(Option.Some(mks(9)), mut w, true) + cl(Option.Some(mks(10)), mut w, false);
    println(f"d{d}");
    let e = ce(Option.Some(mks(11)), mut w, false) + ce(Option.Some(mks(12)), mut w, true);
    println(f"e{e}");
    let f = cn(Option.Some(mks(13)), mut w, false) + cn(Option.None, mut w, false) + cn(Option.Some(mks(14)), mut w, true);
    println(f"f{f}");
    let h = H { k: 1 };
    let g = h.mp(Option.Some(mks(15)), mut w, false) + h.mp(Option.Some(mks(16)), mut w, true);
    println(f"g{g}");
    println(f"w{w.len()} wi{wi.len()}");
    println("end")
}
"#);
    assert_eq!(out, "p1\ndS1\npn\ndS3\np0\ndS0\na4\np4\ndS4\nb2\ni6\ndP6\nin\ndP8\nc3\nl9\ndS9\nd2\ne11\ndS11\ne3\nn13\ndS13\nf2\nm15\ndS15\ng2\nw6 wi1\ndP7\ndS2\ndS5\ndS10\ndS12\ndS14\ndS16\nend\n");
}
