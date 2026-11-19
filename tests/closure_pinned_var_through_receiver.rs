//! A type variable that only a closure argument's result pins, left open by
//! the closure's body (`E` of `|| Ok(1)` against `Fn() -> Result[T, E]`),
//! stays the call's variable, so a chained method into an annotated `let`
//! fixes it (B-2026-11-18-74, the user-generic shape; the `Pool.new` and
//! `Ok(call())` shapes are still open on that row).

use karac::{check_source, run_playground};

const SRC: &str = "struct W[T, E] { t: T, f: Fn() -> Result[T, E] }\nimpl[T, E] W[T, E] {\n    fn new(f: escaping Fn() -> Result[T, E], t: own T) -> W[T, E] { W { t: t, f: f } }\n    fn same(own self, k: i64) -> W[T, E] { self }\n}\nenum Er { A }\nfn main() {\n    let w: W[i64, Er] = W.new(|| Ok(1), 5).same(1);\n    let w1 = W.new(|| Ok(2), 6).same(1);\n    let w2: W[i64, Er] = w1;\n    match (w.f)() { Ok(v) => println(f\"{w.t} {v} {w2.t}\"), Err(_) => println(\"err\") }\n}\n";

#[test]
fn a_closure_pinned_variable_survives_a_chained_method() {
    let errs: Vec<_> = check_source(SRC)
        .into_iter()
        .filter(|d| d.severity == "error")
        .collect();
    assert!(errs.is_empty(), "{errs:#?}");
    let r = run_playground(SRC);
    assert!(r.ok, "{:#?}", r.diagnostics);
    assert_eq!(r.stdout.concat(), "5 1 6\n");
}
