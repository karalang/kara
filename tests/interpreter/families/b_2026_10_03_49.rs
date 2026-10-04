//! B-2026-10-03-49: a by-value param stored through a `let ... else` binding runs its Drop body once in the interpreter

use super::*;

/// B-2026-10-03-49: the interpreter's outliving-store check followed `match`
/// and `if let` bindings but not `let ... else` ones, so a param destructured
/// by `let Some(w) = x else { .. }` and then pushed into a caller-owned `Vec`
/// counted as dying with the call: R's `Drop` body ran at the call's end and
/// again when the `Vec` dropped. Covers the single-level, nested and two-step
/// spellings; the compiled backends print the same for the first two.
#[test]
fn interp_let_else_stored_param_runs_leaf_body_once() {
    let out = run(r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, s: f"heap-string-longer-than-sso-{i}" } }
fn one(x: Option[R], v: mut ref Vec[R]) -> i64 { let Some(w) = x else { return 0 }; v.push(w); return 1 }
fn two(x: Option[Option[R]], v: mut ref Vec[R]) -> i64 { let Some(Some(w)) = x else { return 0 }; v.push(w); return 1 }
fn step(x: Option[Option[R]], v: mut ref Vec[R]) -> i64 { let Some(o) = x else { return 0 }; let Some(w) = o else { return 2 }; v.push(w); return 1 }
fn main() {
    let mut v: Vec[R] = Vec.new();
    println(one(Some(mk(1)), mut v));
    println(two(Some(Some(mk(2))), mut v));
    println(step(Some(Some(mk(3))), mut v));
    println(v.len());
    println("end");
}
"#);
    assert_eq!(out, "1\n1\n1\n3\ndR1\ndR2\ndR3\nend\n");
}
