//! B-2026-10-01-50: a user type named like a seeded generic param (`T`) captured `Option`/`Result`'s payload.

use super::*;

/// B-2026-10-01-50 — a user `struct T` whose values run a `Drop` body made every
/// `Some(w)` / `Ok(w)` arm over a local read as consuming the payload, because
/// the payload is declared as the seeded param `T` and the consumption gate
/// resolved that name against user types. An arm that discards `w` on one path
/// only then leaked the payload's heap on the other path. The output was always
/// right, so this test pins the order; the leak is the memory-sanitizer
/// sibling's to catch. Covers a user-variant re-wrap, an `Option` re-wrap, a
/// `Result` scrutinee and `if let`, each on the skipping and the taken path.
#[test]
fn e2e_user_type_named_like_seeded_param_does_not_capture_payload() {
    let src = r#"struct W1 { v: i64, s: String }
impl Drop for W1 { fn drop(mut ref self) { println(f"dW1_{self.v}") } }
fn mk(n: i64) -> W1 { return W1 { v: n, s: f"ssssssssssssssssssssssssssssss{n}" } }
enum E { A(W1), B }
struct T { w: W1 }
fn a8(c: bool, n: i64) { let o = Some(mk(n)); match o { Some(w) => { if c { let _ = E.A(w); println("a") }; println("b") }, None => {} }; println("e") }
fn o8(c: bool, n: i64) { let o = Some(mk(n)); match o { Some(w) => { if c { let _ = Some(w); println("a") }; println("b") }, None => {} }; println("e") }
fn r8(c: bool, n: i64) { let o: Result[W1, i64] = Ok(mk(n)); match o { Ok(w) => { if c { let _ = Some(w); println("a") }; println("b") }, Err(v) => println(f"{v}") }; println("e") }
fn i8(c: bool, n: i64) { let o = Some(mk(n)); if let Some(w) = o { if c { let _ = E.A(w); println("a") }; println("b") }; println("e") }
fn main() {
  a8(false, 1); a8(true, 2); o8(false, 3); o8(true, 4); r8(false, 5); r8(true, 6); i8(false, 7); i8(true, 8)
  println("end")
}
"#;
    let want = "b\ndW1_1\ne\ndW1_2\na\nb\ne\nb\ndW1_3\ne\ndW1_4\na\nb\ne\nb\ndW1_5\ne\ndW1_6\na\nb\ne\nb\ndW1_7\ne\ndW1_8\na\nb\ne\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
