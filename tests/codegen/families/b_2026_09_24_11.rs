//! B-2026-09-24-11 -- a `let`-bound conditional hand-back (`let r = if c { a }
//! else { None }; …; r`) of an `Option` / `Result` whose struct payload runs a
//! user `Drop` only through a FIELD (`struct M { r: R, s: String }`, `R: Drop`)
//! has one owner on every path.

use super::*;

/// B-2026-09-24-11 — the `let`-bound hand-back over a payload whose `Drop` is
/// NESTED in a field is expanded like the tail spelling, so both backends
/// register the dies-inside body callee-side and the caller stands its result
/// down. Before: no output at all on the compiled surfaces (valgrind 4 errors
/// at -O0) and the body twice under `--interp` (`mid d1 y1 d1 end`) on every
/// taken path; free, `Result`, `match`, deeper-nested, associated and method
/// spellings, named and temp arguments. The `c = false` named cell is the
/// guard that the dies-inside path keeps its one body.
#[test]
fn e2e_let_bound_optres_handback_with_nested_drop_payload_has_one_owner() {
    let Some(out) = run_program(
        r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
struct M { r: R, s: String }
struct N { m: M, k: i64 }
struct H { k: i64 }
fn mk(i: i64) -> M { M { r: R { id: i }, s: f"heap-string-longer-than-sso-{i}" } }
fn f(a: Option[M], c: bool) -> Option[M] { let r: Option[M] = if c { a } else { None }; println("mid"); r }
fn g(a: Result[M, i64], c: bool) -> Result[M, i64] { let r: Result[M, i64] = if c { a } else { Err(7) }; println("mid"); r }
fn fm(a: Option[M], c: bool) -> Option[M] { let r: Option[M] = match c { true => a, false => None }; println("mid"); r }
fn fnn(a: Option[N], c: bool) -> Option[N] { let r: Option[N] = if c { a } else { None }; println("mid"); r }
impl H {
    fn af(a: Option[M], c: bool) -> Option[M] { let r: Option[M] = if c { a } else { None }; println("mid"); r }
    fn mf(ref self, a: Option[M], c: bool) -> Option[M] { let r: Option[M] = if c { a } else { None }; println("mid"); r }
}
fn main() {
    let a1 = Some(mk(1));
    let b1 = f(a1, true);
    match b1 { Some(x) => println(f"y{x.r.id}"), None => println("none") }
    let b2 = f(Some(mk(2)), true);
    match b2 { Some(x) => println(f"y{x.r.id}"), None => println("none") }
    let a3 = Some(mk(3));
    let b3 = f(a3, false);
    match b3 { Some(x) => println(f"y{x.r.id}"), None => println("none") }
    let a4: Result[M, i64] = Ok(mk(4));
    let b4 = g(a4, true);
    match b4 { Ok(x) => println(f"y{x.r.id}"), Err(e) => println(f"e{e}") }
    let b5 = g(Ok(mk(5)), true);
    match b5 { Ok(x) => println(f"y{x.r.id}"), Err(e) => println(f"e{e}") }
    let a6 = Some(mk(6));
    let b6 = fm(a6, true);
    match b6 { Some(x) => println(f"y{x.r.id}"), None => println("none") }
    let a7 = Some(N { m: mk(7), k: 1 });
    let b7 = fnn(a7, true);
    match b7 { Some(x) => println(f"y{x.m.r.id}"), None => println("none") }
    let a8 = Some(mk(8));
    let b8 = H.af(a8, true);
    match b8 { Some(x) => println(f"y{x.r.id}"), None => println("none") }
    let h = H { k: 1 };
    let a9 = Some(mk(9));
    let b9 = h.mf(a9, true);
    match b9 { Some(x) => println(f"y{x.r.id}"), None => println("none") }
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "mid\ny1\nd1\nmid\ny2\nd2\nmid\nd3\nnone\nmid\ny4\nd4\nmid\ny5\nd5\nmid\ny6\nd6\nmid\ny7\nd7\nmid\ny8\nd8\nmid\ny9\nd9\nend\n", "got:\n{out}");
}
