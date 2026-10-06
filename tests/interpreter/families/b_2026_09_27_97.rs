//! B-2026-09-27-97 / B-2026-09-23-45 — a container or tuple param handed back on some exits.

use super::*;

/// A by-value `Option[Vec[R]]`, `Result[Vec[R], _]`, `Option[(R, i64)]`,
/// tuple or array-in-tuple param handed back on SOME exits lost its `Drop`
/// bodies on the exit where it stayed. With the default param drop schedule,
/// `fn_conditionally_returns_param_bare` makes the caller stand down, but the
/// callee-side flip was registered only for struct, `Array`, one-level
/// `Option` and user-enum params. The interpreter twin ran the body twice on
/// the taken holder path. Covers the `let`-bound holder, the tail spelling,
/// early `return`, `Result`, tuple and array-in-tuple payloads, and named and
/// temporary arguments.
#[test]
fn interp_container_param_handed_back_on_some_exits_runs_bodies_once() {
    let out = run(r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
fn mk(i: i64) -> R { R { id: i, s: f"heap-string-longer-than-sso-{i}" } }
fn mkv(i: i64) -> Vec[R] { let mut v: Vec[R] = Vec.new(); v.push(mk(i)); v.push(mk(i + 1)); v }
fn f(a: Option[Vec[R]], c: bool) -> Option[Vec[R]] { let r: Option[Vec[R]] = if c { a } else { None }; println("mid"); r }
fn ft(a: Option[(R, i64)], c: bool) -> Option[(R, i64)] { let r: Option[(R, i64)] = if c { a } else { None }; println("mid"); r }
fn ftail(a: Option[Vec[R]], c: bool) -> Option[Vec[R]] { println("mid"); if c { a } else { None } }
fn ov(a: Option[Vec[R]], c: bool) -> Option[Vec[R]] { if c { return a } println("in"); None }
fn rv(a: Result[Vec[R], i64], c: bool) -> Result[Vec[R], i64] { if c { return a } println("in"); Err(0) }
fn tp(a: (R, i64), c: bool) -> (R, i64) { if c { return a } println("in"); (mk(90), 0) }
fn th(a: (R, i64), c: bool) -> (R, i64) { let r: (R, i64) = if c { a } else { (mk(9), 0) }; println("mid"); r }
fn ta(a: (Array[R, 2], i64), c: bool) -> (Array[R, 2], i64) { if c { return a } println("in"); ([mk(93), mk(94)], 0) }
fn showv(r: Option[Vec[R]]) { match r { Some(x) => { println(f"n{x.len()}") } None => { println("none") } } }
fn tt(a: Option[(R, i64)], c: bool) -> Option[(R, i64)] { println("mid"); if c { a } else { None } }
fn main() {
    showv(f(Some(mkv(1)), true)); showv(f(Some(mkv(3)), false));
    let a = Some(mkv(5)); showv(f(a, false));
    let p = ft(Some((mk(7), 1)), false); println("p");
    let q = ft(Some((mk(8), 1)), true); println("q");
    showv(ftail(Some(mkv(10)), false)); showv(ftail(Some(mkv(12)), true));
    let x1 = ov(Some(mkv(20)), false); println("a1");
    let n = Some(mkv(22)); let x2 = ov(n, false); println("a2");
    let x3 = ov(Some(mkv(24)), true); println("a3");
    let x4 = rv(Ok(mkv(26)), false); println("a4");
    let x5 = tp((mk(28), 1), false); println("a5");
    let t = (mk(29), 2); let x6 = tp(t, true); println("a6");
    let x7 = th((mk(30), 1), false); println("a7");
    let x8 = th((mk(31), 1), true); println("a8");
    let x9 = ta(([mk(32), mk(33)], 1), false); println("a9");
    let p2 = tt(Some((mk(39), 1)), false); println("p2");
    let q2 = tt(Some((mk(40), 1)), true); println("q2");
    let s = Some((mk(41), 1)); let r2 = tt(s, false); println("r2");
    println("end")
}
"#);
    assert_eq!(
        out,
        "mid
n2
d1
d2
mid
d3
d4
none
mid
d5
d6
none
mid
d7
p
mid
d8
q
mid
d10
d11
none
mid
n2
d12
d13
in
d20
d21
a1
in
d22
d23
a2
d24
d25
a3
in
d26
d27
a4
in
d28
d90
a5
d29
a6
mid
d30
d9
a7
mid
d31
a8
in
d32
d33
d93
d94
a9
mid
d39
p2
mid
d40
q2
mid
d41
r2
end
"
    );
}
