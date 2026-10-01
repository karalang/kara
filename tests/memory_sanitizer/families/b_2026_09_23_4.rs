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
fn asan_struct_param_moved_into_fresh_ctor_scrutinee_runs_body_once() {
    assert_clean_asan_run(
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
        &[
            "-t1", "d1", "y7", "-t2", "d2", "y2", "-t3", "d3", "y3", "-t4", "d4", "y4", "-t6",
            "e6", "d6", "y6", "-t7", "d7", "y70", "-t9", "d9", "y9", "-ta", "d10", "y10", "-tb",
            "d12", "d11", "y23", "-tc", "d13", "y13", "-tn", "d14", "y14", "-h2f", "d15", "y0",
            "d0", "-h3", "y1", "d16", "-h4", "d17", "y3", "-h7", "d18", "y18", "end",
        ],
        "asan_struct_param_moved_into_fresh_ctor_scrutinee_runs_body_once",
    );
}
