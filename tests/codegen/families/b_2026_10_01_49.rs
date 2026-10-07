//! B-2026-10-01-49: a payload binding nested one tuple deeper at an arm tail lost its Drop body compiled.

use super::*;

/// B-2026-10-01-49 — a payload binding nested a tuple deeper at a branch tail
/// (`Some(w) => ((w, 1), 2)`) ran its `Drop` body nowhere on the compiled
/// surfaces. The tuple binding's element types are filled per position from
/// the other tails, and an integer literal in the same nested tuple
/// (`None => ((mk(0), 0), 0)`) comes back unnamed too, so the fill never took
/// the other tail's `(W1, _)`. The binding then got no bodies walker. Covers a
/// local and a by-value `Option`, a `Result` whose every tail is a payload
/// binding, the binding in the second position, three levels, and `if let`.
#[test]
fn e2e_nested_tuple_payload_binding_at_arm_tail_runs_body_once() {
    let src = r#"struct W1 { v: i64, s: String }
impl Drop for W1 { fn drop(mut ref self) { println(f"dW1_{self.v}") } }
fn mk(n: i64) -> W1 { return W1 { v: n, s: f"ssssssssssssssssssssssssssssss{n}" } }
fn a() { let o = Some(mk(1)); let t = match o { Some(w) => ((w, 1), 2), None => ((mk(0), 0), 0) }; println(f"a {t.0.0.v} {t.1}") }
fn b() { let o = Some(mk(2)); let t = match o { Some(w) => ((w, 1), 2), None => ((mk(0), 0), 0) }; println(f"b {t.1}") }
fn c(o: Option[W1]) { let t = match o { Some(w) => ((w, 1), 2), None => ((mk(0), 0), 0) }; println(f"c {t.1}") }
fn d() { let r: Result[W1, W1] = Ok(mk(4)); let t = match r { Ok(w) => ((w, 1), 2), Err(w) => ((w, 3), 4) }; println(f"d {t.1}") }
fn e() { let o = Some(mk(5)); let t = match o { Some(w) => (2, (w, 1)), None => (0, (mk(0), 0)) }; println(f"e {t.0}") }
fn g() { let o = Some(mk(6)); let t = match o { Some(w) => (((w, 1), 2), 3), None => (((mk(0), 0), 0), 0) }; println(f"g {t.1}") }
fn h() { let o = Some(mk(7)); let t = if let Some(w) = o { ((w, 1), 2) } else { ((mk(0), 0), 0) }; println(f"h {t.1}") }
fn main() {
  a(); b(); c(Some(mk(3))); d(); e(); g(); h();
  println("end")
}
"#;
    let want = "a 1 2\ndW1_1\nb 2\ndW1_2\nc 2\ndW1_3\nd 2\ndW1_4\ne 2\ndW1_5\ng 3\ndW1_6\nh 2\ndW1_7\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
