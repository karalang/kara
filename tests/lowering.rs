// tests/lowering.rs
//
// Exercises the operator lowering pass directly: rewrites `Binary` and
// `Unary` AST nodes into `Call(Path(...))` shape on the way to downstream
// phases.

use karac::ast::{Expr, ExprKind, Item, Stmt, StmtKind};
use karac::{lower, parse, resolve, typecheck};

/// Lower a program and return its (mutated) AST root.
fn lower_program(src: &str) -> karac::ast::Program {
    let mut parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let resolved = resolve(&parsed.program);
    let typed = typecheck(&parsed.program, &resolved);
    lower(&mut parsed.program, &typed);
    parsed.program
}

/// Find the body expression of `fn name(...)` in the program.
fn fn_body_final<'a>(program: &'a karac::ast::Program, name: &str) -> &'a Expr {
    for item in &program.items {
        if let Item::Function(f) = item {
            if f.name == name {
                return f
                    .body
                    .final_expr
                    .as_deref()
                    .expect("function body has no final expression");
            }
        }
    }
    panic!("function {} not found", name);
}

#[test]
fn test_lower_int_add_to_call_path() {
    // Non-entry function name (`app_main`): `fn main` is bound by the
    // entry-point return-type contract (must be `()`/`Result[(),E]`/`ExitCode`),
    // and this lowering scaffold returns `i64` only to exercise the
    // operator-to-call-path lowering.
    let program = lower_program("fn app_main() -> i64 { 1 + 2 }");
    let body = fn_body_final(&program, "app_main");
    match &body.kind {
        ExprKind::Call { callee, args } => {
            assert_eq!(args.len(), 2);
            match &callee.kind {
                ExprKind::Path { segments, .. } => {
                    assert_eq!(segments, &["i64".to_string(), "add".to_string()]);
                }
                other => panic!("expected Path callee, got {:?}", other),
            }
        }
        other => panic!("expected Call, got {:?}", other),
    }
}

#[test]
fn test_lower_float_mul_to_call_path() {
    let program = lower_program("fn calc() -> f64 { 1.5 * 2.0 }");
    let body = fn_body_final(&program, "calc");
    let ExprKind::Call { callee, .. } = &body.kind else {
        panic!("expected Call");
    };
    let ExprKind::Path { segments, .. } = &callee.kind else {
        panic!("expected Path callee");
    };
    assert_eq!(segments, &["f64".to_string(), "mul".to_string()]);
}

#[test]
fn test_lower_neg_to_call_path() {
    let program = lower_program("fn n() -> i64 { let x: i64 = 5; -x }");
    let body = fn_body_final(&program, "n");
    let ExprKind::Call { callee, args } = &body.kind else {
        panic!("expected Call, got {:?}", body.kind);
    };
    assert_eq!(args.len(), 1);
    let ExprKind::Path { segments, .. } = &callee.kind else {
        panic!("expected Path callee");
    };
    assert_eq!(segments, &["i64".to_string(), "neg".to_string()]);
}

#[test]
fn test_lower_recursive_descent() {
    // Lowering must descend into nested expressions: `(1 + 2) * (3 - 4)`
    // both operands of the outer Mul are themselves lowered first.
    let program = lower_program("fn f() -> i64 { (1 + 2) * (3 - 4) }");
    let body = fn_body_final(&program, "f");
    let ExprKind::Call { callee, args } = &body.kind else {
        panic!("expected outer Call");
    };
    let ExprKind::Path { segments, .. } = &callee.kind else {
        panic!("expected Path callee");
    };
    assert_eq!(segments, &["i64".to_string(), "mul".to_string()]);

    for (i, expected) in [(0, "add"), (1, "sub")] {
        match &args[i].value.kind {
            ExprKind::Call { callee, .. } => {
                let ExprKind::Path { segments: segs, .. } = &callee.kind else {
                    panic!("inner [{i}]: expected Path");
                };
                assert_eq!(segs, &["i64".to_string(), expected.to_string()]);
            }
            other => panic!("inner [{i}]: expected Call, got {:?}", other),
        }
    }
}

#[test]
fn test_lower_skips_logical_short_circuit() {
    // v2 scope lowers `==`/`<`/bitwise, but logical `and`/`or` stay as Binary
    // because short-circuit semantics can't be faithfully expressed as a
    // strict trait-method call.
    let program = lower_program("fn both(a: bool, b: bool) -> bool { a and b }");
    let body = fn_body_final(&program, "both");
    assert!(
        matches!(body.kind, ExprKind::Binary { .. }),
        "expected Binary for and, got {:?}",
        body.kind
    );
}

#[test]
fn test_lower_eq_ne_to_call_path() {
    let program = lower_program(
        "fn a(x: i64, y: i64) -> bool { x == y }
         fn b(x: i64, y: i64) -> bool { x != y }",
    );
    for (name, expected_method) in [("a", "eq"), ("b", "ne")] {
        let body = fn_body_final(&program, name);
        let ExprKind::Call { callee, .. } = &body.kind else {
            panic!("fn {name}: expected Call, got {:?}", body.kind);
        };
        let ExprKind::Path { segments, .. } = &callee.kind else {
            panic!("fn {name}: expected Path callee");
        };
        assert_eq!(segments, &["i64".to_string(), expected_method.to_string()]);
    }
}

#[test]
fn test_lower_comparison_to_call_path() {
    let program = lower_program(
        "fn a(x: i32, y: i32) -> bool { x < y }
         fn b(x: i32, y: i32) -> bool { x <= y }
         fn c(x: i32, y: i32) -> bool { x > y }
         fn d(x: i32, y: i32) -> bool { x >= y }",
    );
    for (name, expected) in [("a", "lt"), ("b", "le"), ("c", "gt"), ("d", "ge")] {
        let body = fn_body_final(&program, name);
        let ExprKind::Call { callee, .. } = &body.kind else {
            panic!("fn {name}: expected Call");
        };
        let ExprKind::Path { segments: segs, .. } = &callee.kind else {
            panic!("fn {name}: expected Path");
        };
        assert_eq!(segs, &["i32".to_string(), expected.to_string()]);
    }
}

#[test]
fn test_lower_bitwise_to_call_path() {
    let program = lower_program(
        "fn a(x: i32, y: i32) -> i32 { x & y }
         fn b(x: i32, y: i32) -> i32 { x | y }
         fn c(x: i32, y: i32) -> i32 { x ^ y }
         fn d(x: i32, y: i32) -> i32 { x << y }
         fn e(x: i32, y: i32) -> i32 { x >> y }",
    );
    for (name, expected) in [
        ("a", "bitand"),
        ("b", "bitor"),
        ("c", "bitxor"),
        ("d", "shl"),
        ("e", "shr"),
    ] {
        let body = fn_body_final(&program, name);
        let ExprKind::Call { callee, .. } = &body.kind else {
            panic!("fn {name}: expected Call");
        };
        let ExprKind::Path { segments: segs, .. } = &callee.kind else {
            panic!("fn {name}: expected Path");
        };
        assert_eq!(segs, &["i32".to_string(), expected.to_string()]);
    }
}

#[test]
fn test_lower_bitnot_and_not_to_call_path() {
    // `~int` and `not bool` both lower to `.not()` on their respective primitive.
    let program = lower_program(
        "fn a(x: i32) -> i32 { ~x }
         fn b(x: bool) -> bool { not x }",
    );
    let a_body = fn_body_final(&program, "a");
    let ExprKind::Call { callee, .. } = &a_body.kind else {
        panic!("expected Call for ~x, got {:?}", a_body.kind);
    };
    let ExprKind::Path { segments: segs, .. } = &callee.kind else {
        panic!("expected Path");
    };
    assert_eq!(segs, &["i32".to_string(), "not".to_string()]);

    let b_body = fn_body_final(&program, "b");
    let ExprKind::Call { callee, .. } = &b_body.kind else {
        panic!("expected Call for not x");
    };
    let ExprKind::Path { segments: segs, .. } = &callee.kind else {
        panic!("expected Path");
    };
    assert_eq!(segs, &["bool".to_string(), "not".to_string()]);
}

#[test]
fn test_lower_string_concat_to_call_path() {
    let program = lower_program(
        "fn greet() -> String {
             let a: String = \"hello \";
             let b: String = \"world\";
             a + b
         }",
    );
    let body = fn_body_final(&program, "greet");
    let ExprKind::Call { callee, .. } = &body.kind else {
        panic!("expected Call, got {:?}", body.kind);
    };
    let ExprKind::Path { segments, .. } = &callee.kind else {
        panic!("expected Path callee");
    };
    assert_eq!(segments, &["String".to_string(), "add".to_string()]);
}

#[test]
fn test_lower_into_to_from_call_at_let_annotation() {
    // `let y: i64 = x.into();` with `x: i32` should lower to `i64.from(x)`.
    let program = lower_program("fn f() { let x: i32 = 42; let y: i64 = x.into(); }");
    let Item::Function(f) = program
        .items
        .iter()
        .find(|i| matches!(i, Item::Function(f) if f.name == "f"))
        .unwrap()
    else {
        unreachable!()
    };
    // Second stmt is `let y: i64 = x.into()` — its value should now be
    // `i64.from(x)`.
    let stmt = &f.body.stmts[1];
    let value = match &stmt.kind {
        StmtKind::Let { value, .. } => value,
        other => panic!("expected Let, got {:?}", other),
    };
    let ExprKind::Call { callee, args } = &value.kind else {
        panic!("expected Call, got {:?}", value.kind);
    };
    assert_eq!(args.len(), 1);
    let ExprKind::Path { segments: segs, .. } = &callee.kind else {
        panic!("expected Path callee");
    };
    assert_eq!(segs, &["i64".to_string(), "from".to_string()]);
}

#[test]
fn test_lower_into_at_return_position() {
    // `fn f(x: i32) -> i64 { x.into() }` — return position threads the
    // expected type, so `.into()` lowers to `i64.from(x)`.
    let program = lower_program("fn f(x: i32) -> i64 { x.into() }");
    let body = fn_body_final(&program, "f");
    let ExprKind::Call { callee, .. } = &body.kind else {
        panic!("expected Call, got {:?}", body.kind);
    };
    let ExprKind::Path { segments: segs, .. } = &callee.kind else {
        panic!("expected Path");
    };
    assert_eq!(segs, &["i64".to_string(), "from".to_string()]);
}

#[test]
fn test_lower_into_at_call_argument() {
    // Function call argument position threads the parameter type.
    let program = lower_program(
        "fn takes(_y: i64) {}\n\
         fn f(x: i32) { takes(x.into()) }",
    );
    let body = fn_body_final(&program, "f");
    let ExprKind::Call { args, .. } = &body.kind else {
        panic!("expected outer Call, got {:?}", body.kind);
    };
    let ExprKind::Call {
        callee: inner_callee,
        ..
    } = &args[0].value.kind
    else {
        panic!("expected inner Call for .into()");
    };
    let ExprKind::Path { segments: segs, .. } = &inner_callee.kind else {
        panic!("expected Path");
    };
    assert_eq!(segs, &["i64".to_string(), "from".to_string()]);
}

#[test]
fn test_lower_inside_let_value() {
    // `let x = 1 + 2;` — the value position must also be lowered.
    let program = lower_program("fn main() { let _x: i64 = 1 + 2; }");
    let item = program
        .items
        .iter()
        .find(|i| matches!(i, Item::Function(f) if f.name == "main"))
        .unwrap();
    let Item::Function(f) = item else {
        unreachable!()
    };
    let stmt = f.body.stmts.first().expect("expected let stmt");
    let value = match &stmt.kind {
        StmtKind::Let { value, .. } => value,
        other => panic!("expected Let, got {:?}", other),
    };
    let _: &Stmt = stmt; // silence dead use of import
    assert!(
        matches!(value.kind, ExprKind::Call { .. }),
        "expected lowered Call inside let, got {:?}",
        value.kind
    );
}
