//! B-2026-10-02-66 -- a `let` local handed to a GENERIC callee inside one
//! branch keeps its `Drop` body on the branch not taken.

use super::*;

/// B-2026-10-02-66 — the memory half: running the body again on the branch
/// not taken must not free the payload twice or leak it on the other branch.
/// Same program as the codegen twin.
#[test]
fn asan_generic_call_in_branch_keeps_body_on_branch_not_taken() {
    assert_clean_asan_run(
        r#"struct S { id: i64, s: String }
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
"#,
        &[
            "dS1", "r 0", "dS1", "k", "r 0", "dS2", "r 0", "dS2", "k", "r 0", "end",
        ],
        "generic_call_in_branch_keeps_body",
    );
}
