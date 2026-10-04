//! B-2026-10-03-28 / B-2026-10-03-5 -- a nested-envelope by-value param
//! (`Result[Option[R], E]`, `Option[Option[R]]`) whose leaf runs a user
//! `Drop`: the payload body read freed memory, or ran twice when an arm moved
//! the leaf on.

use super::*;

/// B-2026-10-03-28 — the callee freed the inner box and the caller's bodies
/// walk read it after the call (`dR<garbage> 1`). The callee now runs the
/// bodies, ahead of its box drop. B-2026-10-03-5 — an arm that moves the leaf
/// on (`Some(r)`, `v.push(w)`) ran the body in the callee and again at the
/// leaf's new owner. Covers a field read as the arm's value, a rewrap, a push,
/// and a leaf handed to a plain function, whose param's body the callee's walk
/// still runs once.
#[test]
fn e2e_nested_envelope_param_runs_leaf_body_once() {
    let src = r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, s: f"heap-string-longer-than-sso-{i}" } }
fn rr(x: Result[Option[R], i64]) -> i64 {
    match x {
        Ok(Some(w)) => w.id,
        Ok(None) => 2,
        Err(e) => e,
    }
}
fn keep(x: Result[Option[R], i64], v: mut ref Vec[R]) -> i64 {
    match x {
        Ok(Some(w)) => { v.push(w); return 1 }
        _ => 0,
    }
}
fn eat(r: R) { println(f"e{r.id}") }
fn re(x: Result[Option[R], i64]) -> i64 {
    match x {
        Ok(Some(w)) => { eat(w); return 1 }
        _ => 0,
    }
}
fn okeep(x: Option[Option[R]], v: mut ref Vec[R]) -> i64 {
    match x {
        Some(Some(w)) => { v.push(w); return 1 }
        _ => 0,
    }
}
fn take(x: Option[Option[R]]) -> Option[R] {
    match x {
        Some(Some(r)) => Some(r),
        _ => None,
    }
}
fn main() {
    println(rr(Ok(Some(mk(1)))));
    let mut v: Vec[R] = Vec.new();
    println(keep(Ok(Some(mk(2))), mut v));
    println(okeep(Some(Some(mk(3))), mut v));
    println(re(Ok(Some(mk(5)))));
    let a = take(Some(Some(mk(4))));
    match a { Some(r) => println(r.id), None => println("none") }
    println(v.len());
    println("end");
}
"#;
    let want = "dR1\n1\n1\n1\ne5\ndR5\n1\n4\ndR4\n2\ndR2\ndR3\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}

/// B-2026-10-03-28 / B-2026-10-03-5 — the `if let` / `let ... else` spellings.
/// The first fix stood the callee's bodies walk down on the `match` arm path
/// only, so `if let Ok(Some(w)) = x { v.push(w) }` still ran the body inside
/// the callee and again when `v` dropped, and a read-only `if let` over the
/// `Result` spelling lost its body. The `let ... else` push cell is asserted
/// on both backends since B-2026-10-03-49 (the interpreter ran its body twice).
#[test]
fn e2e_nested_envelope_param_block_forms_run_leaf_body_once() {
    let src = r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, s: f"heap-string-longer-than-sso-{i}" } }
fn keep(x: Option[Option[R]], v: mut ref Vec[R]) -> i64 {
    if let Some(Some(w)) = x { v.push(w); return 1 }
    return 0
}
fn rkeep(x: Result[Option[R], i64], v: mut ref Vec[R]) -> i64 {
    if let Ok(Some(w)) = x { v.push(w); return 1 }
    return 0
}
fn rd(x: Result[Option[R], i64]) -> i64 {
    if let Ok(Some(w)) = x { return w.id }
    return 0
}
fn lr(x: Option[Option[R]]) -> i64 {
    let Some(Some(w)) = x else { return 0 };
    return w.id
}
fn main() {
    let mut v: Vec[R] = Vec.new();
    println(keep(Some(Some(mk(1))), mut v));
    println(rkeep(Ok(Some(mk(2))), mut v));
    println(rd(Ok(Some(mk(3)))));
    println(lr(Some(Some(mk(4)))));
    println(v.len());
    println("end");
}
"#;
    let want = "1\n1\ndR3\n3\ndR4\n4\n2\ndR1\ndR2\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");

    let le = r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn le(x: Option[Option[R]], v: mut ref Vec[R]) -> i64 {
    let Some(Some(w)) = x else { return 0 };
    v.push(w);
    return 1
}
fn main() {
    let mut v: Vec[R] = Vec.new();
    println(le(Some(Some(R { id: 5, s: f"heap-string-longer-than-sso-5" })), mut v));
    println(v.len());
    println("end");
}
"#;
    let (le_out, le_errs, _, _) = karac::run_program_full_checked(le);
    assert!(le_errs.is_empty(), "interp errored: {le_errs:?}");
    assert_eq!(le_out.join(""), "1\n1\ndR5\nend\n", "interpreter let-else");
    assert_eq!(
        run_program(le).as_deref(),
        Some("1\n1\ndR5\nend\n"),
        "AOT let-else"
    );
}
