//! B-2026-09-28-80: a by-value param handed to a storing function from inside a branch.

use super::*;

/// B-2026-09-28-80 — a by-value struct param handed to a function that stores
/// it, from inside an `if` or a `match` arm: the path that never reaches the
/// call runs the body in this frame, a temporary and a named argument alike.
#[test]
fn asan_struct_param_handed_to_a_storing_fn_in_a_branch_runs_its_body_once() {
    assert_clean_asan_run(
        r#"struct S { id: i64, s: String }
impl Drop for S { fn drop(mut ref self) { println(f"dS{self.id}") } }
fn mks(i: i64) -> S { return S { id: i, s: f"heap-string-longer-than-sso-{i}" } }
struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn skeep(t: S, v: mut ref Vec[S]) { v.push(t) }
fn csf(t: S, v: mut ref Vec[S], c: bool) { if c { skeep(t, v) } }
fn rkeep(t: R, v: mut ref Vec[R]) { v.push(t) }
fn crf(t: R, v: mut ref Vec[R], k: i64) { match k { 1 => rkeep(t, v), _ => println("no") } }
fn main() {
    let mut u: Vec[S] = Vec.new();
    csf(mks(1), mut u, false);
    csf(mks(2), mut u, true);
    let a = mks(3);
    csf(a, mut u, false);
    println(f"n{u.len()}");
    let mut w: Vec[R] = Vec.new();
    crf(R { id: 4 }, mut w, 0);
    crf(R { id: 5 }, mut w, 1);
    println(f"m{w.len()}");
    println("end")
}
"#,
        &["dS1", "dS3", "n1", "dS2", "no", "dR4", "m1", "dR5", "end"],
        "asan_struct_param_handed_to_a_storing_fn_in_a_branch_runs_its_body_once",
    );
}

/// B-2026-09-28-80 — the same for a boxed `Option[S]` and an inline
/// `Option[R]`, including a hand-over to a callee that itself stores on only
/// some paths.
#[test]
fn asan_optres_param_handed_to_a_storing_fn_in_a_branch_runs_its_body_once() {
    assert_clean_asan_run(
        r#"struct S { id: i64, s: String }
impl Drop for S { fn drop(mut ref self) { println(f"dS{self.id}") } }
fn mks(i: i64) -> S { return S { id: i, s: f"heap-string-longer-than-sso-{i}" } }
struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn okeep(t: Option[S], v: mut ref Vec[Option[S]]) { v.push(t) }
fn cp(t: Option[S], v: mut ref Vec[Option[S]], c: bool) { if c { v.push(t) } }
fn cf(t: Option[S], v: mut ref Vec[Option[S]], c: bool) { if c { okeep(t, v) } }
fn cfc(t: Option[S], v: mut ref Vec[Option[S]], c: bool, d: bool) { if c { cp(t, v, d) } }
fn rk(t: Option[R], v: mut ref Vec[Option[R]]) { v.push(t) }
fn crk(t: Option[R], v: mut ref Vec[Option[R]], c: bool) { if c { rk(t, v) } else { println("no") } }
fn main() {
    let mut u: Vec[Option[S]] = Vec.new();
    cf(Option.Some(mks(1)), mut u, false);
    cf(Option.Some(mks(2)), mut u, true);
    let a = Option.Some(mks(3));
    cf(a, mut u, false);
    cfc(Option.Some(mks(4)), mut u, false, true);
    cfc(Option.Some(mks(5)), mut u, true, false);
    cfc(Option.Some(mks(6)), mut u, true, true);
    println(f"n{u.len()}");
    let mut w: Vec[Option[R]] = Vec.new();
    crk(Some(R { id: 7 }), mut w, false);
    crk(Some(R { id: 8 }), mut w, true);
    println(f"m{w.len()}");
    println("end")
}
"#,
        &[
            "dS1", "dS3", "dS4", "dS5", "n2", "dS2", "dS6", "no", "dR7", "m1", "dR8", "end",
        ],
        "asan_optres_param_handed_to_a_storing_fn_in_a_branch_runs_its_body_once",
    );
}

/// B-2026-09-28-80 — methods handing a param to a storing function from a
/// branch, and a free function handing it to one of two containers.
#[test]
fn asan_param_handed_to_a_storing_fn_by_a_method_or_both_branches_runs_its_body_once() {
    assert_clean_asan_run(
        r#"struct S { id: i64, s: String }
impl Drop for S { fn drop(mut ref self) { println(f"dS{self.id}") } }
fn mks(i: i64) -> S { return S { id: i, s: f"heap-string-longer-than-sso-{i}" } }
fn skeep(t: S, v: mut ref Vec[S]) { v.push(t) }
fn okeep(t: Option[S], v: mut ref Vec[Option[S]]) { v.push(t) }
struct H { xs: Vec[S], ys: Vec[Option[S]] }
impl H {
    fn put(mut ref self, t: S, c: bool) { if c { skeep(t, mut self.xs) } }
    fn opt(mut ref self, t: Option[S], c: bool) { if c { okeep(t, mut self.ys) } }
}
fn two(t: Option[S], v: mut ref Vec[Option[S]], w: mut ref Vec[Option[S]], c: bool) { if c { okeep(t, v) } else { okeep(t, w) } }
fn main() {
    let mut h = H { xs: Vec.new(), ys: Vec.new() };
    h.put(mks(1), false);
    h.put(mks(2), true);
    h.opt(Option.Some(mks(3)), false);
    h.opt(Option.Some(mks(4)), true);
    println(f"h{h.xs.len()} {h.ys.len()}");
    let mut p: Vec[Option[S]] = Vec.new();
    let mut q: Vec[Option[S]] = Vec.new();
    two(Option.Some(mks(5)), mut p, mut q, false);
    two(Option.Some(mks(6)), mut p, mut q, true);
    println(f"p{p.len()} {q.len()}");
    println("end")
}
"#,
        &[
            "dS1", "dS3", "h1 1", "dS4", "dS2", "p1 1", "dS5", "dS6", "end",
        ],
        "asan_param_handed_to_a_storing_fn_by_a_method_or_both_branches_runs_its_body_once",
    );
}
