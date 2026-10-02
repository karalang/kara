//! B-2026-10-02-60 -- a local closure in one function hijacked calls to the
//! same-named free function in every function compiled after it.

use super::*;

/// B-2026-10-02-60 — `main` binds a local closure `helper`; `f` (declared
/// AFTER `main`) and the method `P.get` call the free function `helper`.
///
/// Before: `karac build` failed with `Undefined variable 'helper'` at the
/// first of those calls, because the name-keyed closure table outlived `main`
/// and routed the call through a closure slot `f` does not have.
#[test]
fn e2e_local_closure_does_not_hijack_a_later_free_fn_call() {
    let src = r#"fn helper() -> i64 {
    return 1;
}
struct P {
    n: i64,
}
impl P {
    fn get(self) -> i64 {
        return self.n + helper();
    }
}
fn main() {
    let helper = || 99;
    let p = P { n: 3 };
    println(f"{f()} {helper()} {p.get()}");
}
fn f() -> i64 {
    return helper();
}
"#;
    assert_eq!(run_program(src), Some("1 99 4\n".to_string()));
}
