//! B-2026-09-28-67: a by-value Option/Result param stored into a container on only some paths.

use super::*;

/// B-2026-09-28-67 — a by-value `Option` / `Result` param a free function
/// stores on SOME paths only (`if c { v.push(t) }`): boxed and inline
/// payloads, a temporary and a named argument. The path that stores hands the
/// value to the container; the path that does not runs its body in the call.
#[test]
fn asan_own_optres_param_stored_on_some_paths_runs_its_body_once_on_each_path() {
    assert_clean_asan_run(
        r#"struct S { id: i64, s: String }
impl Drop for S { fn drop(mut ref self) { println(f"dS{self.id}") } }
fn mks(i: i64) -> S { return S { id: i, s: f"heap-string-longer-than-sso-{i}" } }
struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
struct W { id: i64, a: String, b: String }
impl Drop for W { fn drop(mut ref self) { println(f"dW{self.id}") } }
fn mkw(i: i64) -> W { return W { id: i, a: f"a-heap-string-longer-than-sso-{i}", b: f"b-heap-string-{i}" } }

fn cp(t: Option[S], v: mut ref Vec[Option[S]], c: bool) { if c { v.push(t) } }
fn cpr(t: Option[R], v: mut ref Vec[Option[R]], c: bool) { if c { v.push(t) } }
fn cw(t: Result[W, i64], v: mut ref Vec[Result[W, i64]], c: bool) { if c { v.push(t) } }
fn cps2(t: Option[S], v: mut ref Vec[Option[S]], c: bool) { if c { v.push(t); println("pushed") } else { println("kept") } }

fn main() {
    let mut u: Vec[Option[S]] = Vec.new();
    cp(Option.Some(mks(1)), mut u, false);
    cp(Option.Some(mks(2)), mut u, true);
    let a = Option.Some(mks(3));
    cp(a, mut u, false);
    println("x");
    let b = Option.Some(mks(4));
    cp(b, mut u, true);
    cps2(Option.Some(mks(5)), mut u, false);
    cps2(Option.None, mut u, false);
    println(f"n{u.len()}");
    let mut w: Vec[Option[R]] = Vec.new();
    cpr(Some(R { id: 6 }), mut w, false);
    cpr(Some(R { id: 7 }), mut w, true);
    println(f"m{w.len()}");
    let mut z: Vec[Result[W, i64]] = Vec.new();
    cw(Result.Ok(mkw(8)), mut z, false);
    cw(Result.Ok(mkw(9)), mut z, true);
    cw(Result.Err(0), mut z, false);
    let e: Result[W, i64] = Result.Ok(mkw(10));
    cw(e, mut z, false);
    println(f"k{z.len()}");
    println("end")
}
"#,
        &[
            "dS1", "dS3", "x", "kept", "dS5", "kept", "n2", "dS2", "dS4", "dR6", "m1", "dR7",
            "dW8", "dW10", "k1", "dW9", "end",
        ],
        "asan_own_optres_param_stored_on_some_paths_runs_its_body_once_on_each_path",
    );
}

/// B-2026-09-28-67 — the same over a method storing into `self`, a `match`
/// arm, both arms of an `if` into different containers, and a callee-local
/// container.
#[test]
fn asan_optres_param_stored_by_method_arm_or_both_branches_runs_its_body_once() {
    assert_clean_asan_run(
        r#"struct S { id: i64, s: String }
impl Drop for S { fn drop(mut ref self) { println(f"dS{self.id}") } }
fn mks(i: i64) -> S { return S { id: i, s: f"heap-string-longer-than-sso-{i}" } }
struct H { xs: Vec[Option[S]] }
impl H { fn put(mut ref self, t: Option[S], c: bool) { if c { self.xs.push(t) } } }
fn cm(t: Option[S], v: mut ref Vec[Option[S]], k: i64) { match k { 1 => v.push(t), 2 => { println("two") } _ => {} } }
fn c2(t: Option[S], v: mut ref Vec[Option[S]], w: mut ref Vec[Option[S]], c: bool) { if c { v.push(t) } else { w.push(t) } }
fn cpl(t: Option[S], c: bool) { let mut l: Vec[Option[S]] = Vec.new(); if c { l.push(t) } println(f"l{l.len()}") }

fn main() {
    let mut h = H { xs: Vec.new() };
    h.put(Option.Some(mks(1)), false);
    h.put(Option.Some(mks(2)), true);
    let a = Option.Some(mks(3));
    h.put(a, false);
    println(f"h{h.xs.len()}");
    let mut u: Vec[Option[S]] = Vec.new();
    let mut i = 0;
    while i < 3 { cm(Option.Some(mks(10 + i)), mut u, i); i = i + 1; }
    cm(Option.None, mut u, 2);
    let mut p: Vec[Option[S]] = Vec.new();
    let mut q: Vec[Option[S]] = Vec.new();
    c2(Option.Some(mks(20)), mut p, mut q, false);
    c2(Option.Some(mks(21)), mut p, mut q, true);
    println(f"n{u.len()} {p.len()} {q.len()}");
    cpl(Option.Some(mks(30)), false);
    cpl(Option.Some(mks(31)), true);
    println("end")
}
"#,
        &[
            "dS1", "dS3", "h1", "dS2", "dS10", "two", "dS12", "two", "n1 1 1", "dS20", "dS21",
            "dS11", "l0", "dS30", "l1", "dS31", "end",
        ],
        "asan_optres_param_stored_by_method_arm_or_both_branches_runs_its_body_once",
    );
}
