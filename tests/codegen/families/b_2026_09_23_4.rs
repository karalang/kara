//! B-2026-09-23-4: a by-value struct param moved into a FRESH constructor scrutinee runs its `Drop` body once.

use super::*;

/// B-2026-09-23-4 — `match Option.Some(a) { Some(v) => .. }` inside `fn f(a: R)`
/// binds `v` out of a temporary envelope the callee built around the param. The
/// caller still runs `a`'s body after the call (caller-retains), so `v` is a
/// view of it, as it already was for a named `let o = Some(a)`. The temp had no
/// name to carry that mark, so `v` owned the body too: `d1 d1` on every surface
/// against the by-value control's one `d1`, with memory balanced. Covers `match`,
/// `if let`, `let .. else`, a user enum ctor, a struct whose FIELD has the body,
/// a guard, a rebind, a sink, a second param, a named argument, a wrap into a
/// returned struct and into a dying local, a `match` used as a value, and the
/// not-taken leg of a conditional hand-back.
#[test]
fn e2e_struct_param_moved_into_fresh_ctor_scrutinee_runs_body_once() {
    let Some(out) = run_program(
        r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
fn mkr(i: i64) -> R { return R { id: i, s: f"aaa" } }
struct W { r: R, n: i64 }
enum E { A(R), B }
fn eat(r: R) { println(f"e{r.id}") }
fn t1(a: R) -> i64 { match Option.Some(a) { Option.Some(v) => { return 7 }, Option.None => { return 0 } } }
fn t2(a: R) -> i64 { match Some(a) { Some(v) => { return v.id }, None => { return 0 } } }
fn t3(a: R) -> i64 { if let Some(v) = Some(a) { return v.id } return 0 }
fn t4(a: R) -> i64 { let Some(v) = Some(a) else { return 0 }; return v.id }
fn t6(a: R) -> i64 { match Some(a) { Some(v) => { eat(v); return 6 }, None => { return 0 } } }
fn t7(a: W) -> i64 { match Some(a) { Some(v) => { return v.n }, None => { return 0 } } }
fn t9(a: R) -> i64 { match E.A(a) { E.A(v) => { return v.id }, E.B => { return 0 } } }
fn t10(a: R) -> i64 { match Some(a) { Some(v) => { let m = v; return m.id }, None => { return 0 } } }
fn t11(a: R, b: R) -> i64 { match Some(a) { Some(v) => { return v.id + b.id }, None => { return 0 } } }
fn t12(a: R) -> i64 { match Some(a) { Some(v) if v.id > 100 => { return 1 }, Some(w) => { return w.id }, None => { return 0 } } }
fn h2(a: R, c: bool) -> R { match Some(a) { Some(v) => { if c { return v } return mkr(0) } None => { return mkr(0) } } }
fn h3(a: R) -> W { match Some(a) { Some(v) => { return W { r: v, n: 1 } } None => { return W { r: mkr(0), n: 0 } } } }
fn h4(a: R) -> i64 { match Some(a) { Some(v) => { let s = W { r: v, n: 3 }; return s.n } None => { return 0 } } }
fn h7(a: R) -> i64 { let k = match Some(a) { Some(v) => v.id, None => 0 }; return k }
fn main() {
    println("-t1"); let z = t1(mkr(1)); println(f"y{z}")
    println("-t2"); let z = t2(mkr(2)); println(f"y{z}")
    println("-t3"); let z = t3(mkr(3)); println(f"y{z}")
    println("-t4"); let z = t4(mkr(4)); println(f"y{z}")
    println("-t6"); let z = t6(mkr(6)); println(f"y{z}")
    println("-t7"); let z = t7(W { r: mkr(7), n: 70 }); println(f"y{z}")
    println("-t9"); let z = t9(mkr(9)); println(f"y{z}")
    println("-ta"); let z = t10(mkr(10)); println(f"y{z}")
    println("-tb"); let z = t11(mkr(11), mkr(12)); println(f"y{z}")
    println("-tc"); let z = t12(mkr(13)); println(f"y{z}")
    println("-tn"); let a = mkr(14); let z = t2(a); println(f"y{z}")
    println("-h2f"); let r = h2(mkr(15), false); println(f"y{r.id}")
    println("-h3"); let w = h3(mkr(16)); println(f"y{w.n}")
    println("-h4"); let z = h4(mkr(17)); println(f"y{z}")
    println("-h7"); let z = h7(mkr(18)); println(f"y{z}")
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "-t1\nd1\ny7\n-t2\nd2\ny2\n-t3\nd3\ny3\n-t4\nd4\ny4\n-t6\ne6\nd6\ny6\n-t7\nd7\ny70\n-t9\nd9\ny9\n-ta\nd10\ny10\n-tb\nd12\nd11\ny23\n-tc\nd13\ny13\n-tn\nd14\ny14\n-h2f\nd15\ny0\nd0\n-h3\ny1\nd16\n-h4\nd17\ny3\n-h7\nd18\ny18\nend\n", "got:\n{out}");
}
