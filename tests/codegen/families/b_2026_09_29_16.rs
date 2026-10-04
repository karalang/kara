//! B-2026-09-29-16: an unconditional unwrap of a by-value Option/Result param hands its payload to the callee

use super::*;

/// B-2026-09-29-16: `t.unwrap()` on a by-value `Option` / `Result` param, read through a
/// field (`println(f"u{t.unwrap().id}")`, `return t.expect("boom").id`, `-t.unwrap().id`),
/// ran the payload's `Drop` body in the callee and again in the caller under `--interp`,
/// and the compiled caller freed the boxed `Option` the callee had already freed. An
/// unconditional unwrap now hands the payload (and its box) to the callee on every
/// backend, as the `match` B-2026-09-29-12 lowers a `let`-bound unwrap to already did.
#[test]
fn e2e_unwrap_of_a_by_value_optres_param_read_through_a_field_runs_its_body_once() {
    let Some(out) = run_program(
        r#"struct S { id: i64, s: String }
impl Drop for S { fn drop(mut ref self) { println(f"dS{self.id}") } }
fn mks(i: i64) -> S { return S { id: i, s: f"heap-string-longer-than-sso-{i}" } }
struct P { id: i64 }
impl Drop for P { fn drop(mut ref self) { println(f"dP{self.id}") } }
struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
struct Q { r: R, n: i64 }
fn mkq(i: i64) -> Q { return Q { r: R { id: i }, n: i } }
struct W { id: i64, s: String }
fn mkw(i: i64) -> W { return W { id: i, s: f"heap-string-longer-than-sso-{i}" } }
fn eatr(r: R) -> i64 { return r.id }
fn cu(t: Option[S]) { println(f"u{t.unwrap().id}") }
fn ce(t: Option[S]) -> i64 { return t.expect("boom").id }
fn cx(t: Result[S, i64]) -> i64 { return t.expect("boom").id }
fn cr(t: Result[i64, S]) { println(f"r{t.unwrap_err().id}") }
fn cg(t: Option[S]) -> i64 { let k = -t.unwrap().id; return k }
fn cl(t: Option[S]) { println(f"l{t.unwrap().s.len()}") }
fn cq(t: Option[Q]) { println(f"q{eatr(t.unwrap().r)}") }
fn cp(t: Option[P]) { println(f"p{t.unwrap().id}") }
fn cw(t: Option[W]) { println(f"w{t.unwrap().id}") }
fn ge[T](t: Option[T]) -> T { return t.unwrap() }
fn main() {
    cu(Option.Some(mks(1)));
    let a = Option.Some(mks(2));
    cu(a);
    println("-");
    println(f"e{ce(Option.Some(mks(3)))}");
    println(f"x{cx(Result.Ok(mks(4)))}");
    cr(Result.Err(mks(5)));
    println(f"g{cg(Option.Some(mks(6)))}");
    cl(Option.Some(mks(7)));
    cq(Option.Some(mkq(8)));
    cp(Option.Some(P { id: 9 }));
    cw(Option.Some(mkw(10)));
    let w = Option.Some(mkw(11));
    cw(w);
    let g = ge(Option.Some(mks(12)));
    println(f"g{g.id}");
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "u1\ndS1\nu2\ndS2\n-\ndS3\ne3\ndS4\nx4\nr5\ndS5\ndS6\ng-6\nl29\ndS7\ndR8\nq8\np9\ndP9\nw10\nw11\ng12\ndS12\nend\n", "got:\n{out}");
}
