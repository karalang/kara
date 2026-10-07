//! B-2026-10-02-66 -- a `let` local handed to a GENERIC callee inside one
//! branch keeps its `Drop` body on the branch not taken.

use super::*;

/// B-2026-10-02-66 — `compile_generic_call` retracted the local's payload
/// bodies on every path (`suppress_container_elem_bodies_for_var`), so
/// `a1(false)`, which never handed `t` to `keep`, printed `r 0` with no
/// `dS1`. It now retracts per path for a `let` local whose walk an
/// enclosing frame owns, as the non-generic twin (`a2`) already did
/// (B-2026-09-29-116).
#[test]
fn e2e_generic_call_in_branch_keeps_body_on_branch_not_taken() {
    let src = r#"struct S { id: i64, s: String }
impl Drop for S { fn drop(mut ref self) { println(f"dS{self.id}") } }
fn mks(i: i64) -> S { return S { id: i, s: f"heap-string-longer-than-sso-{i}" } }
enum E { A(S), B(i64) }
fn keep[T](e: T) -> T { return e }
fn keepE(e: E) -> E { return e }
fn a1(c: bool) -> i64 { let t = E.A(mks(1)); if c { let k = keep(t); println("k") }; return 0 }
fn a2(c: bool) -> i64 { let t = E.A(mks(2)); if c { let k = keepE(t); println("k") }; return 0 }
fn main() {
    println(f"r {a1(false)}");
    println(f"r {a1(true)}");
    println(f"r {a2(false)}");
    println(f"r {a2(true)}");
    println("end")
}
"#;
    let want = "dS1\nr 0\ndS1\nk\nr 0\ndS2\nr 0\ndS2\nk\nr 0\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
