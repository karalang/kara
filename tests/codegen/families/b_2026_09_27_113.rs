//! B-2026-09-27-113 — a fresh-temp struct argument the callee stores into a
//! caller-held container is freed once.

use super::*;

/// B-2026-09-27-113 — `rb(mut b, mk(1))` over `fn rb(b: mut ref B, a: S) ->
/// i64 { b.v.push(a); 5 }`: the callee entry-copies the struct and the
/// container frees the copy, so the caller's fresh temp is an orphaned original
/// that only the caller can free. The caller declined it on every store route,
/// as a stand-in for "the callee RC-promoted the param"; it now asks that
/// question directly (`callee_param_rc_promoted`). Covers the free-function,
/// method-receiver and generic spellings, with named-argument controls.
#[test]
fn e2e_freshtemp_struct_stored_in_caller_container_runs_bodies_once() {
    let src = r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
struct S { r: R, s: String }
fn mk(i: i64) -> S { S { r: R { id: i }, s: f"heap-string-longer-than-sso-{i}" } }
struct B { v: Vec[S] }
impl B { fn put(mut ref self, a: S) { self.v.push(a) } }
fn rb(b: mut ref B, a: S) -> i64 { b.v.push(a); 5 }
fn rw[T](b: mut ref Vec[T], a: T) -> i64 { b.push(a); 6 }
fn main() {
    let mut b = B { v: Vec.new() };
    println(f"k{rb(mut b, mk(1))}");
    let x = mk(2);
    println(f"k{rb(mut b, x)}");
    b.put(mk(3));
    let y = mk(4);
    b.put(y);
    let mut w: Vec[S] = Vec.new();
    println(f"k{rw(mut w, mk(5))}");
    println(f"n{b.v.len()} {w.len()}");
    println("end")
}"#;
    let want = "k5\nk5\nk6\nn4 1\nd5\nd1\nd2\nd3\nd4\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    if let Some(aot) = run_program(src) {
        assert_eq!(aot, want, "AOT");
    }
}
