//! B-2026-09-28-53 -- a by-value param wrapped in a NEST of `Option` /
//! `Result` constructors in a local and returned (`let o = Some(Some(x)); o`)
//! runs its `Drop` body once.

use super::*;

/// B-2026-09-28-53 — `fn c1(x: S) -> Option[Option[S]] { let o = Some(Some(x)); o }`
/// over `S { r: R, s: String }` with `R`'s own `Drop`. The cells: the tail (c1),
/// an explicit `return` (c2), a `Result[Option[S], i64]` (c3), a return on one
/// path only, both legs (c4), the same through a rebind of the wrapper (c5),
/// beside controls that were already right: the nest built in two `let`s (c6)
/// and the one-level wrap (c7).
///
/// Before: the wrap walk looked through ONE constructor, so `o` was not seen to
/// carry `x`. The caller was never stood down, and the body ran after the call
/// AND over the result: c1, c2, c3, c4's returning leg and c5's returning leg
/// printed it twice on every surface.
#[test]
fn e2e_returned_nested_envelope_over_param_runs_body_once() {
    let Some(out) = run_program(
        r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
struct S { r: R, s: String }
fn mk(i: i64) -> S { S { r: R { id: i }, s: f"heap-string-longer-than-sso-{i}" } }

fn c1(x: S) -> Option[Option[S]] { let o = Some(Some(x)); o }
fn c2(x: S) -> Option[Option[S]] { let o = Some(Some(x)); return o }
fn c3(x: S) -> Result[Option[S], i64] { let o: Result[Option[S], i64] = Ok(Some(x)); o }
fn c4(x: S, k: bool) -> Option[Option[S]] { let o = Some(Some(x)); if k { return o } None }
fn c5(x: S, k: bool) -> Option[Option[S]] { let o = Some(Some(x)); let p = o; if k { return p } None }
fn c6(x: S) -> Option[Option[S]] { let o = Some(x); let p = Some(o); p }
fn c7(x: S) -> Option[S] { let o = Some(x); o }

fn show(t: String, v: Option[Option[S]]) {
    match v { Some(Some(s)) => println(f"{t}:{s.r.id}"), Some(None) => println(f"{t}:sn"), None => println(f"{t}:none") }
}

fn main() {
    let a = c1(mk(1)); println("-1"); show("c1", a); println("-1b");
    let b = c2(mk(2)); println("-2"); show("c2", b); println("-2b");
    let c = c3(mk(3)); println("-3");
    match c { Ok(Some(s)) => println(f"c3:{s.r.id}"), _ => println("c3:x") }
    println("-3b");
    let d = c4(mk(4), true); println("-4"); show("c4t", d); println("-4b");
    let e = c4(mk(5), false); println("-5"); show("c4f", e); println("-5b");
    let f = c5(mk(6), true); println("-6"); show("c5t", f); println("-6b");
    let g = c5(mk(7), false); println("-7"); show("c5f", g); println("-7b");
    let h = c6(mk(8)); println("-8"); show("c6", h); println("-8b");
    let i = c7(mk(9)); println("-9");
    match i { Some(s) => println(f"c7:{s.r.id}"), None => println("c7:none") }
    println("end")
}"#,
    ) else {
        return;
    };
    assert_eq!(
        out.lines().collect::<Vec<_>>(),
        vec![
            "-1", "c1:1", "d1", "-1b", "-2", "c2:2", "d2", "-2b", "-3", "c3:3", "d3", "-3b", "-4",
            "c4t:4", "d4", "-4b", "d5", "-5", "c4f:none", "-5b", "-6", "c5t:6", "d6", "-6b", "d7",
            "-7", "c5f:none", "-7b", "-8", "c6:8", "d8", "-8b", "-9", "c7:9", "d9", "end",
        ],
        "got:\n{out}"
    );
}
