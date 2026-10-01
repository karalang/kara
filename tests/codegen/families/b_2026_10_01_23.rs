//! B-2026-10-01-23 -- a boxed `Option`/`Result` payload binding re-wrapped in a
//! discarded USER-enum constructor (`let _ = E.A(w);`) is freed once.

use super::*;

/// B-2026-10-01-23 — `match o { Some(w) => { let _ = E.A(w); .. } .. }` over a
/// local `Option[W1]` aborted with a double free on the JIT: the discarded
/// variant temp frees its payload, and the scrutinee's box freed the same heap.
/// B-2026-10-01-11 stood the box down only for an `Option`/`Result` re-wrap.
/// Covered: the `let _` and bare-statement spellings, an enum with its own
/// `Drop`, a `Result` scrutinee, `if let`, and the discard nested in an `if` on
/// the path that takes it. No two-field variant enum is declared: with one in
/// the program the parent tree was already clean, which would hide the fault.
#[test]
fn e2e_boxed_payload_rewrapped_in_discarded_user_enum_freed_once() {
    let src = r#"struct W1 { v: i64, s: String }
impl Drop for W1 { fn drop(mut ref self) { println(f"dW1_{self.v}") } }
fn mk(n: i64) -> W1 { return W1 { v: n, s: f"ssssssssssssssssssssssssssssss{n}" } }
enum E { A(W1), B }
enum D { A(W1), B }
impl Drop for D { fn drop(mut ref self) { println("dD") } }
fn a1() { let o = Some(mk(1)); match o { Some(w) => { let _ = E.A(w); println("a1") }, None => {} }; println("a1e") }
fn a2() { let o = Some(mk(2)); match o { Some(w) => { E.A(w); println("a2") }, None => {} }; println("a2e") }
fn a3() { let o = Some(mk(3)); match o { Some(w) => { let _ = D.A(w); println("a3") }, None => {} }; println("a3e") }
fn a5() { let o: Result[W1, i64] = Ok(mk(5)); match o { Ok(w) => { let _ = E.A(w); println("a5") }, Err(_) => {} }; println("a5e") }
fn a7() { let o = Some(mk(7)); if let Some(w) = o { let _ = E.A(w); println("a7") }; println("a7e") }
fn a8(c: bool) { let o = Some(mk(8)); match o { Some(w) => { if c { let _ = E.A(w); println("a8") }; println("a8b") }, None => {} }; println("a8e") }
fn main() { a1(); a2(); a3(); a5(); a7(); a8(true); println("end") }
"#;
    let want = "dW1_1\na1\na1e\ndW1_2\na2\na2e\ndD\ndW1_3\na3\na3e\ndW1_5\na5\na5e\ndW1_7\na7\na7e\ndW1_8\na8\na8b\na8e\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
