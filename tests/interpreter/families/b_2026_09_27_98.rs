//! B-2026-09-27-98 — a struct, `Option` or `Result` param handed back on some exits and forwarded to a consumer on the others.

use super::*;

/// B-2026-09-27-98 — a by-value plain `Drop` struct, `Option` or `Result`
/// param that the callee hands back on some exits and passes to a by-value
/// consumer that keeps nothing on the others. The forward cleared the param's
/// per-path drop as if the consumer owned it, but such a consumer runs
/// nothing (the forwarding frame's slot does), so the body was lost on every
/// surface. Struct returned bare and wrapped, `Option`, `Result`, a consumer
/// that matches the payload, the branches swapped, an if-else tail, a loop.
#[test]
fn interp_struct_or_optres_param_handed_back_on_some_paths_forwarded_to_a_consumer() {
    let out = run(r#"struct S { id: i64, s: String }
impl Drop for S { fn drop(mut ref self) { println(f"dS{self.id}") } }
fn mks(i: i64) -> S { return S { id: i, s: "ab".to_string() + "cd" } }
fn eat(s: S) { println("x") }
fn eat2(o: Option[S]) { println("x") }
fn eatr(o: Result[S, i64]) { println("x") }
fn eat3(o: Option[S]) { match o { Option.Some(s) => println(f"m{s.id}"), Option.None => println("n") } }
fn pk(s: S, k: bool) -> S { if k { return s } eat(s); return mks(0) }
fn pa(s: S, k: bool) -> S { if k { return s } eat(s); println("after"); return mks(0) }
fn pw(s: S, k: bool) -> Option[S] { if k { return Option.Some(s) } eat(s); return Option.None }
fn po(h: Option[S], k: bool) -> Option[S] { if k { return h } eat2(h); return Option.None }
fn pr(h: Result[S, i64], k: bool) -> Result[S, i64] { if k { return h } eatr(h); return Result.Err(0) }
fn pm(h: Option[S], k: bool) -> Option[S] { if k { return h } eat3(h); return Option.None }
fn pv(s: S, k: bool) -> S { if k { eat(s); return mks(0) } return s }
fn pe(s: S, k: bool) -> S { if k { return s } else { eat(s); return mks(0) } }
fn main() {
    let a1 = mks(1); let r1 = pk(a1, false); println(f"r{r1.id}");
    let r2 = pk(mks(2), true); println(f"r{r2.id}");
    let r3 = pa(mks(3), false); println(f"r{r3.id}");
    let w4 = pw(mks(4), false); println(f"w{w4.is_some()}");
    let w5 = pw(mks(5), true); println(f"w{w5.is_some()}");
    let o6 = po(Option.Some(mks(6)), false); println(f"o{o6.is_some()}");
    let o7 = po(Option.Some(mks(7)), true); println(f"o{o7.is_some()}");
    let e8 = pr(Result.Ok(mks(8)), false); println(f"e{e8.is_ok()}");
    let e9 = pr(Result.Ok(mks(9)), true); println(f"e{e9.is_ok()}");
    let m10 = pm(Option.Some(mks(10)), false); println(f"m{m10.is_some()}");
    let v11 = pv(mks(11), true); println(f"v{v11.id}");
    let v12 = pe(mks(12), false); println(f"v{v12.id}");
    let mut i = 13;
    while i < 16 { let r = pk(mks(i), i == 14); println(f"l{r.id}"); i = i + 1; }
    println("end")
}
"#);
    assert_eq!(out, "x\ndS1\nr0\ndS0\nr2\ndS2\nx\nafter\ndS3\nr0\ndS0\nx\ndS4\nwfalse\nwtrue\ndS5\nx\ndS6\nofalse\notrue\ndS7\nx\ndS8\nefalse\netrue\ndS9\nm10\ndS10\nmfalse\nx\ndS11\nv0\ndS0\nx\ndS12\nv0\ndS0\nx\ndS13\nl0\ndS0\nl14\ndS14\nx\ndS15\nl0\ndS0\nend\n");
}

/// B-2026-09-27-98 — the same forward from a whole REBIND of the param
/// (`let m = s;`), before and after the returning branch, for a struct, an
/// `Option` and a `Result`.
#[test]
fn interp_struct_or_optres_param_rebound_then_forwarded_to_a_consumer() {
    let out = run(r#"struct S { id: i64, s: String }
impl Drop for S { fn drop(mut ref self) { println(f"dS{self.id}") } }
fn mks(i: i64) -> S { return S { id: i, s: "ab".to_string() + "cd" } }
fn eat(s: S) { println("x") }
fn eat2(o: Option[S]) { println("x") }
fn eatr(o: Result[S, i64]) { println("x") }
fn rs(s: S, k: bool) -> S { let m = s; if k { return m } eat(m); return mks(0) }
fn ro(h: Option[S], k: bool) -> Option[S] { let m = h; if k { return m } eat2(m); return Option.None }
fn rt(h: Result[S, i64], k: bool) -> Result[S, i64] { let m = h; if k { return m } eatr(m); return Result.Err(0) }
fn ra(s: S, k: bool) -> S { if k { return s } let g = s; eat(g); return mks(0) }
fn main() {
    let a1 = rs(mks(1), false); println(f"a{a1.id}");
    let a2 = rs(mks(2), true); println(f"a{a2.id}");
    let b3 = ro(Option.Some(mks(3)), false); println(f"b{b3.is_some()}");
    let b4 = ro(Option.Some(mks(4)), true); println(f"b{b4.is_some()}");
    let c5 = rt(Result.Ok(mks(5)), false); println(f"c{c5.is_ok()}");
    let d6 = ra(mks(6), false); println(f"d{d6.id}");
    println("end")
}
"#);
    assert_eq!(out, "x\ndS1\na0\ndS0\na2\ndS2\nx\ndS3\nbfalse\nbtrue\ndS4\nx\ndS5\ncfalse\nx\ndS6\nd0\ndS0\nend\n");
}
