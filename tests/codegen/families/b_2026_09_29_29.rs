//! B-2026-09-29-29: a payload take that rebinds the param's own name.

use super::*;

/// B-2026-09-29-29 — a take that rebinds the param's own name (`let t =
/// match t { .. }`, `let t = if let Some(v) = t { v } else { .. }`) is a take
/// like any other: the payload's body runs once on every path, the new
/// binding's once where it dies, and a store of the param on another path
/// keeps its one owner. Before, the tally counted the rebind as an unknown
/// mention, so the path that never reached the take ran no body on any
/// surface, and the store+take mix with a boxed payload double-freed compiled.
#[test]
fn e2e_take_that_rebinds_the_param_name_runs_each_body_once() {
    let Some(out) = run_program(
        r#"struct S { id: i64, s: String }
impl Drop for S { fn drop(mut ref self) { println(f"dS{self.id}") } }
fn mks(i: i64) -> S { return S { id: i, s: f"heap-string-longer-than-sso-{i}" } }
struct P { id: i64 }
impl Drop for P { fn drop(mut ref self) { println(f"dP{self.id}") } }
enum G { A(S), B(i64) }
struct H { k: i64 }
impl H { fn mp(ref self, t: Option[S], c: bool) -> i64 { if c { let t = match t { Option.Some(v) => v, Option.None => mks(0) }; println(f"m{t.id}") }; return self.k } }
fn ct(t: Option[S], c: bool) -> i64 { if c { let t = match t { Option.Some(q) => q, Option.None => mks(0) }; println(f"t{t.id}") }; return 1 }
fn cg(t: G, c: bool) -> i64 { if c { let t = match t { G.A(v) => v, G.B(n) => mks(n) }; println(f"g{t.id}") }; return 1 }
fn cr(t: Result[S, i64], c: bool) -> i64 { if c { let t = match t { Result.Ok(v) => v, Result.Err(e) => mks(e) }; println(f"r{t.id}") } else { println("rn") }; return 1 }
fn cl(t: Option[S], c: bool) -> i64 { if c { let t = if let Option.Some(v) = t { v } else { mks(0) }; println(f"l{t.id}") }; return 1 }
fn cs(t: Option[S], v: mut ref Vec[Option[S]], c: i64) -> i64 { if c == 0 { v.push(t) } else if c == 1 { let t = match t { Option.Some(q) => q, Option.None => mks(0) }; println(f"s{t.id}") } else { println("sn") }; return 1 }
fn ci(t: Option[P], v: mut ref Vec[Option[P]], c: i64) -> i64 { if c == 0 { v.push(t) } else if c == 1 { let t = match t { Option.Some(q) => q, Option.None => P { id: 0 } }; println(f"i{t.id}") } else { println("in") }; return 1 }
fn cv(t: Option[S], v: mut ref Vec[S], c: bool, k: bool) -> i64 { if c { let t = match t { Option.Some(q) => q, Option.None => mks(0) }; if k { v.push(t) } else { println(f"v{t.id}") } }; return 1 }
fn cb(t: Option[S], c: bool) -> Option[S] { if c { let t = match t { Option.Some(q) => q, Option.None => mks(0) }; println(f"b{t.id}"); return Option.None }; return t }
fn main() {
    let a = ct(Option.Some(mks(1)), false) + ct(Option.Some(mks(2)), true);
    println(f"a{a}");
    let b = cg(G.A(mks(3)), false) + cg(G.A(mks(4)), true) + cg(G.B(5), true);
    println(f"b{b}");
    let c = cr(Result.Ok(mks(6)), false) + cr(Result.Ok(mks(7)), true) + cr(Result.Err(8), true);
    println(f"c{c}");
    let d = cl(Option.Some(mks(9)), false) + cl(Option.Some(mks(10)), true);
    println(f"d{d}");
    let h = H { k: 1 };
    let e = h.mp(Option.Some(mks(11)), false) + h.mp(Option.Some(mks(12)), true);
    println(f"e{e}");
    let mut w: Vec[Option[S]] = Vec.new();
    let f = cs(Option.Some(mks(13)), mut w, 1) + cs(Option.Some(mks(14)), mut w, 0) + cs(Option.Some(mks(15)), mut w, 2) + cs(Option.None, mut w, 1);
    println(f"f{f}");
    let mut wi: Vec[Option[P]] = Vec.new();
    let g = ci(Option.Some(P { id: 16 }), mut wi, 1) + ci(Option.Some(P { id: 17 }), mut wi, 0) + ci(Option.Some(P { id: 18 }), mut wi, 2);
    println(f"g{g}");
    let mut ws: Vec[S] = Vec.new();
    let k = cv(Option.Some(mks(19)), mut ws, false, false) + cv(Option.Some(mks(20)), mut ws, true, false) + cv(Option.Some(mks(21)), mut ws, true, true);
    println(f"k{k}");
    let r = cb(Option.Some(mks(22)), false);
    println(f"r{r.is_some()}");
    let r2 = cb(Option.Some(mks(23)), true);
    println(f"r{r2.is_some()}");
    println(f"w{w.len()} wi{wi.len()} ws{ws.len()}");
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "dS1\nt2\ndS2\na2\ndS3\ng4\ndS4\ng5\ndS5\nb3\nrn\ndS6\nr7\ndS7\nr8\ndS8\nc3\ndS9\nl10\ndS10\nd2\ndS11\nm12\ndS12\ne2\ns13\ndS13\nsn\ndS15\ns0\ndS0\nf4\ni16\ndP16\nin\ndP18\ng3\ndS19\nv20\ndS20\nk3\nrtrue\ndS22\nb23\ndS23\nrfalse\nw1 wi1 ws1\ndS21\ndP17\ndS14\nend\n");
}
