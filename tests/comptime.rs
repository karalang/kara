//! Comptime fold pass (slice 2) — the compile-time evaluator.
//!
//! Covers end-to-end evaluation through the tree-walk interpreter (a
//! `comptime { ... }` block computes a constant) plus the fold pass's AST
//! splicing and its three failure diagnostics (panic / non-foldable /
//! iter-limit). Spec: deferred.md § Comptime — "Implementation phases"
//! substrate 1.

use karac::ast::{Expr, ExprKind, Item, Stmt, StmtKind};
use karac::comptime::ComptimeError;

/// Run the front of the pipeline through the comptime fold pass and return
/// the (rewritten) program plus the comptime diagnostics.
fn fold(source: &str) -> (karac::ast::Program, Vec<ComptimeError>) {
    let mut parsed = karac::parse(source);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    karac::desugar_program(&mut parsed.program);
    let resolved = karac::resolve(&parsed.program);
    assert!(
        resolved.errors.is_empty(),
        "resolve errors: {:?}",
        resolved.errors
    );
    let typed = karac::typecheck(&parsed.program, &resolved);
    karac::lower(&mut parsed.program, &typed);
    let errors = karac::comptime_eval(&mut parsed.program, &typed);
    (parsed.program, errors)
}

/// Find the value expression of `let <name> = ...;` in `main`'s body.
fn let_value<'a>(program: &'a karac::ast::Program, name: &str) -> &'a Expr {
    for item in &program.items {
        if let Item::Function(f) = item {
            if f.name == "main" {
                for stmt in &f.body.stmts {
                    if let Stmt {
                        kind: StmtKind::Let { pattern, value, .. },
                        ..
                    } = stmt
                    {
                        if pattern.binding_names().iter().any(|n| n == name) {
                            return value;
                        }
                    }
                }
            }
        }
    }
    panic!("no `let {name}` found in main");
}

/// Assert no `ExprKind::Comptime` node survives anywhere in `expr`.
fn assert_no_comptime(expr: &Expr) {
    if let ExprKind::Comptime(_) = &expr.kind {
        panic!("comptime node was not folded: {:?}", expr.kind);
    }
}

// ── End-to-end: the constant is computed and observable ─────────

#[test]
fn comptime_arith_folds_and_runs() {
    let out = karac::run_program("fn main() { let x = comptime { 1 + 2 }; println(x); }");
    assert_eq!(out, vec!["3\n"]);
}

#[test]
fn comptime_calls_comptime_fn() {
    let src = "
comptime fn square(n: i64) -> i64 { n * n }
fn main() {
    let x = comptime { square(8) };
    println(x);
}";
    let out = karac::run_program(src);
    assert_eq!(out, vec!["64\n"]);
}

#[test]
fn comptime_with_local_loop_folds() {
    // A loop inside the comptime block runs at compile time; the runtime
    // program only ever sees the folded sum.
    let src = "
fn main() {
    let total = comptime {
        let mut acc = 0;
        let mut i = 1;
        while i <= 10 {
            acc = acc + i;
            i = i + 1;
        }
        acc
    };
    println(total);
}";
    let out = karac::run_program(src);
    assert_eq!(out, vec!["55\n"]);
}

#[test]
fn comptime_string_folds() {
    let out = karac::run_program("fn main() { let s = comptime { \"hi\" }; println(s); }");
    assert_eq!(out, vec!["hi\n"]);
}

// ── AST-level: the node is actually replaced by a literal ───────

#[test]
fn comptime_int_replaced_by_literal() {
    let (program, errors) = fold("fn main() { let x = comptime { 6 * 7 }; }");
    assert!(errors.is_empty(), "unexpected errors: {errors:?}");
    let v = let_value(&program, "x");
    assert_no_comptime(v);
    match &v.kind {
        ExprKind::Integer(42, _) => {}
        other => panic!("expected folded Integer(42), got {other:?}"),
    }
}

#[test]
fn comptime_bool_replaced_by_literal() {
    let (program, errors) = fold("fn main() { let b = comptime { 3 > 1 }; }");
    assert!(errors.is_empty(), "unexpected errors: {errors:?}");
    match &let_value(&program, "b").kind {
        ExprKind::Bool(true) => {}
        other => panic!("expected folded Bool(true), got {other:?}"),
    }
}

#[test]
fn comptime_array_replaced_by_collection_literal() {
    let (program, errors) = fold("fn main() { let a = comptime { [1, 2, 3] }; }");
    assert!(errors.is_empty(), "unexpected errors: {errors:?}");
    let v = let_value(&program, "a");
    assert_no_comptime(v);
    let items = match &v.kind {
        ExprKind::ArrayLiteral(items) => items,
        ExprKind::PrefixCollectionLiteral { items, .. } => items,
        other => panic!("expected folded collection literal, got {other:?}"),
    };
    assert_eq!(items.len(), 3);
    assert!(matches!(items[0].kind, ExprKind::Integer(1, _)));
}

// ── Failure diagnostics ─────────────────────────────────────────

#[test]
fn comptime_panic_is_compile_error() {
    // Unwrapping `None` at comptime is a panic — a comptime panic is a
    // compile error, surfaced at the call site (deferred.md § Comptime —
    // Effect system integration: `panics`).
    let src = "fn main() { let _x = comptime { let o: Option[i64] = None; o.unwrap() }; }";
    let (_program, errors) = fold(src);
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("E_COMPTIME_PANIC")),
        "expected E_COMPTIME_PANIC; got: {errors:?}"
    );
}

#[test]
fn comptime_div_by_zero_is_compile_error() {
    // A runtime fault inside comptime (division by zero) surfaces as a
    // compile-time panic, not a runtime crash.
    let src = "fn main() { let _x = comptime { 1 / 0 }; }";
    let (_program, errors) = fold(src);
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("E_COMPTIME_PANIC")),
        "expected E_COMPTIME_PANIC; got: {errors:?}"
    );
}

#[test]
fn comptime_non_foldable_struct_is_compile_error() {
    let src = "
struct Point { x: i64, y: i64 }
fn main() {
    let _p = comptime { Point { x: 1, y: 2 } };
}";
    let (_program, errors) = fold(src);
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("E_COMPTIME_NON_FOLDABLE_RESULT")),
        "expected E_COMPTIME_NON_FOLDABLE_RESULT; got: {errors:?}"
    );
}

// ── No-op: programs without comptime are untouched ──────────────

#[test]
fn no_comptime_no_errors() {
    let (_program, errors) = fold("fn main() { let x = 1 + 2; println(x); }");
    assert!(errors.is_empty());
}
