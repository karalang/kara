//! B-2026-09-28-10 and B-2026-09-27-35 — a fresh-temp `Option` / `Result` argument whose callee forwards the param whole to a consumer that keeps nothing.

use super::*;

/// B-2026-09-28-10 — a fresh-temp `Option` argument whose callee hands the
/// param on WHOLE to a by-value consumer that keeps nothing. The caller owns
/// such a temp's payload bodies only when the callee's param does not escape,
/// and the forward counted as an escape, so no frame ran the body on the
/// compiled surfaces (a named local ran it from its let site). Direct, named,
/// through two forwarders, forwarded on one branch and matched on the other,
/// recursive, forwarded from a rebind, and a consumer that DOES keep the
/// payload (pushed into an accumulator), which must stay at one body.
#[test]
fn e2e_temp_option_arg_forwarded_whole_to_a_retaining_consumer() {
    let Some(out) = run_program(
        r#"struct S { id: i64, s: String }
impl Drop for S { fn drop(mut ref self) { println(f"dS{self.id}") } }
fn mks(i: i64) -> S { return S { id: i, s: "ab".to_string() + "cd" } }
fn eat2(o: Option[S]) { println("x") }
fn eatp(o: Option[S], v: mut ref Vec[S]) { match o { Option.Some(s) => v.push(s), Option.None => println("n") } }
fn fw(h: Option[S]) { eat2(h); println("after") }
fn fw2(h: Option[S]) { fw(h) }
fn fc(h: Option[S], c: bool) { if c { eat2(h) } else { match h { Option.Some(s) => println(f"m{s.id}"), Option.None => println("n") } } println("afc") }
fn rec(h: Option[S], n: i64) { if n > 0 { rec(h, n - 1) } else { eat2(h) } }
fn reb(h: Option[S]) { let m = h; eat2(m); println("arb") }
fn fp(h: Option[S], v: mut ref Vec[S]) { eatp(h, v); println("afp") }
fn main() {
    fw(Option.Some(mks(1)));
    let o = Option.Some(mks(2));
    fw(o);
    fw2(Option.Some(mks(3)));
    fc(Option.Some(mks(4)), true);
    fc(Option.Some(mks(5)), false);
    rec(Option.Some(mks(6)), 2);
    reb(Option.Some(mks(7)));
    let mut v: Vec[S] = Vec.new();
    fp(Option.Some(mks(8)), mut v);
    println(f"n{v.len()}");
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "x\nafter\ndS1\nx\nafter\ndS2\nx\nafter\ndS3\nx\nafc\ndS4\nm5\nafc\ndS5\nx\ndS6\nx\narb\ndS7\nafp\nn1\ndS8\nend\n", "got:\n{out}");
}

/// B-2026-09-27-35 — the `Result` spelling of the same forward, where the
/// consumer matches the payload and reads it: a field read, an unused arm
/// binding, a consumer that hands the payload on to a plain `S` consumer, and
/// a forward on one branch only.
#[test]
fn e2e_temp_result_arg_forwarded_whole_to_a_retaining_consumer() {
    let Some(out) = run_program(
        r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
struct S { r: R, s: String }
fn mk(i: i64) -> S { S { r: R { id: i }, s: f"heap-string-longer-than-sso-{i}" } }
fn eat(x: Result[S, i64]) -> i64 { match x { Ok(y) => y.r.id, Err(e) => e } }
fn f(a: Result[S, i64]) -> i64 { eat(a) }
fn eatb(x: Result[S, i64]) -> i64 { match x { Ok(y) => 1, Err(e) => e } }
fn g(a: Result[S, i64]) -> i64 { let r = eatb(a); return r }
fn keep(s: S) { println(f"k{s.r.id}") }
fn eatk(x: Result[S, i64]) { match x { Ok(y) => keep(y), Err(e) => println("e") } }
fn h(a: Result[S, i64]) { eatk(a); println("ah") }
fn eatp(x: Result[S, i64]) { match x { Ok(y) => println(f"p{y.r.id}"), Err(e) => println("e") } }
fn c(a: Result[S, i64], t: bool) { if t { eatp(a) } else { eatp(Err(7)) } println("ac") }
fn main() {
    let k = f(Ok(mk(1)));
    println(f"k{k}");
    let a: Result[S, i64] = Ok(mk(2));
    let j = f(a);
    println(f"j{j}");
    let m = g(Ok(mk(3)));
    println(f"m{m}");
    h(Ok(mk(4)));
    c(Ok(mk(5)), true);
    c(Ok(mk(6)), false);
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out, "d1\nk1\nd2\nj2\nd3\nm1\nk4\nah\nd4\np5\nac\nd5\ne\nac\nd6\nend\n",
        "got:\n{out}"
    );
}
