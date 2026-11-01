//! Auto-par's cross-task gate and function values (core semantics §6.3,
//! B-2026-11-01-26 ruling (d)): a loop body holding an unmarked `Fn` value
//! from outside the loop stays sequential, while a function item, a
//! constructor and a `+ CrossTask` value do not stop it.

use karac::concurrency::ConcurrencyAnalysis;
use karac::index_disjoint::DisjointDecline;
use karac::{concurrency_analyze_typed, effectcheck, lower, parse, resolve, typecheck};

fn analyze(source: &str) -> ConcurrencyAnalysis {
    let mut parsed = parse(source);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let resolved = resolve(&parsed.program);
    let tc = typecheck(&parsed.program, &resolved);
    assert!(tc.errors.is_empty(), "{:?}", tc.errors);
    lower(&mut parsed.program, &tc);
    let effects = effectcheck(&parsed.program);
    concurrency_analyze_typed(&parsed.program, &effects, Some(&tc))
}

/// The cross-task verdict of `run`'s loop over `i` when its body is `body`
/// and `run` takes `f: <param>`.
fn verdict(param: &str, body: &str) -> Option<DisjointDecline> {
    let src = format!(
        "enum W {{ V(i64) }}
fn heavy(k: i64) -> i64 {{
    let mut acc: i64 = 0;
    let mut t: i64 = 0;
    while t < 200 {{ acc = acc + k + t; t = t + 1; }}
    acc
}}
fn un(w: W) -> i64 {{ match w {{ W.V(n) => n }} }}
fn run(f: {param}, out: mut ref Vec[i64]) {{
    for i in 0..2048 {{
        out[i] = {body};
    }}
}}
fn main() {{
    let mut out: Vec[i64] = Vec.filled(2048, 0);
    run(heavy, mut out);
    println(out[10]);
}}
"
    );
    let a = analyze(&src);
    // `None` is the whole proof passing, so a pass cannot hide a decline
    // for some other reason.
    a.function_decisions["run"]
        .disjoint_write_loops
        .iter()
        .find(|d| d.loop_var == "i")
        .unwrap_or_else(|| panic!("no disjoint-write candidate in:\n{src}"))
        .decline
}

#[test]
fn an_unmarked_fn_value_keeps_the_loop_sequential() {
    assert_eq!(
        verdict("Fn(i64) -> i64", "f(i)"),
        Some(DisjointDecline::NotCrossTaskSafe)
    );
}

#[test]
fn a_cross_task_fn_value_passes_the_gate() {
    assert_eq!(verdict("Fn(i64) -> i64 + CrossTask", "f(i)"), None);
}

#[test]
fn function_items_and_constructors_pass_the_gate() {
    assert_eq!(verdict("Fn(i64) -> i64 + CrossTask", "heavy(i)"), None);
    assert_eq!(verdict("Fn(i64) -> i64 + CrossTask", "un(W.V(i))"), None);
}
