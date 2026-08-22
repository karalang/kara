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
fn test_lower_into_wraps_option_in_some_ident_callee() {
    // `let o: Option[i64] = 5.into();` lowers to `Some(5)` — a `Call` on a
    // single-`Identifier` callee (NOT a `Path`), because both backends key
    // built-in variant construction on the `Identifier` shape.
    let program = lower_program("fn f() { let o: Option[i64] = 5.into(); }");
    let Item::Function(func) = program
        .items
        .iter()
        .find(|i| matches!(i, Item::Function(f) if f.name == "f"))
        .unwrap()
    else {
        unreachable!()
    };
    let value = match &func.body.stmts[0].kind {
        StmtKind::Let { value, .. } => value,
        other => panic!("expected Let, got {:?}", other),
    };
    let ExprKind::Call { callee, args } = &value.kind else {
        panic!("expected Call, got {:?}", value.kind);
    };
    assert_eq!(args.len(), 1);
    let ExprKind::Identifier(name) = &callee.kind else {
        panic!("expected Identifier callee, got {:?}", callee.kind);
    };
    assert_eq!(name, "Some");
}

#[test]
fn test_lower_into_wraps_result_in_ok_ident_callee() {
    // `let r: Result[i64, String] = 7.into();` lowers to `Ok(7)` — Identifier
    // callee, same as the Option/Some case.
    let program = lower_program("fn f() { let r: Result[i64, String] = 7.into(); }");
    let Item::Function(func) = program
        .items
        .iter()
        .find(|i| matches!(i, Item::Function(f) if f.name == "f"))
        .unwrap()
    else {
        unreachable!()
    };
    let value = match &func.body.stmts[0].kind {
        StmtKind::Let { value, .. } => value,
        other => panic!("expected Let, got {:?}", other),
    };
    let ExprKind::Call { callee, .. } = &value.kind else {
        panic!("expected Call, got {:?}", value.kind);
    };
    let ExprKind::Identifier(name) = &callee.kind else {
        panic!("expected Identifier callee, got {:?}", callee.kind);
    };
    assert_eq!(name, "Ok");
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

// ── Return-position `impl Trait` → its concrete witness (B-2026-08-22-12) ──
//
// A method call through a return-position existential had NO codegen
// dispatcher: `make().get()` was check-green, `--interp`-green, and red under
// both `karac run` and `karac build`. Rather than teach the backend to carry
// an existential — a parallel dispatch path beside every name-keyed mechanism
// it has — lowering hands it the concrete type. The opacity is a caller-side
// abstraction that the TYPECHECKER enforces and that has no work left to do by
// the time this pass runs, and design.md's one-witness-per-monomorphization
// rule says the hidden type is a single statically-known type.
//
// These tests pin the substitution itself. The behavioural half — four
// surfaces byte-identical — is `test_e2e_return_position_impl_trait_witness`
// in tests/codegen.rs.

/// The return type of `fn name` after lowering, rendered as a path segment
/// (`Some("S")`) or `None` when it is still an `impl Trait`.
fn return_path_segment(program: &karac::ast::Program, name: &str) -> Option<String> {
    for item in &program.items {
        if let Item::Function(f) = item {
            if f.name == name {
                return match &f.return_type.as_ref()?.kind {
                    karac::ast::TypeKind::Path(p) => p.segments.first().cloned(),
                    _ => None,
                };
            }
        }
    }
    panic!("function {name} not found");
}

const SRC_PREFIX: &str = "trait Src { type Item; fn get(ref self) -> Self.Item; }\n\
     struct S { v: i64 }\n\
     impl Src for S { type Item = i64; fn get(ref self) -> i64 { self.v } }\n";

#[test]
fn impl_trait_return_is_replaced_by_its_witness() {
    let prog = lower_program(&format!(
        "{SRC_PREFIX}fn make() -> impl Src[Item = i64] {{ S {{ v: 7 }} }}\n\
         fn main() {{ let s = make(); println(s.get()); }}"
    ));
    assert_eq!(return_path_segment(&prog, "make").as_deref(), Some("S"));
}

#[test]
fn impl_trait_return_witness_survives_a_call_site_checked_first() {
    // `main` is walked BEFORE `make`, so at the moment the call is checked the
    // witness does not exist yet. The resolution is deferred to export for
    // exactly this reason; a version that resolved inline would leave this
    // one unrewritten.
    let prog = lower_program(&format!(
        "{SRC_PREFIX}fn main() {{ let s = make(); println(s.get()); }}\n\
         fn make() -> impl Src[Item = i64] {{ S {{ v: 7 }} }}"
    ));
    assert_eq!(return_path_segment(&prog, "make").as_deref(), Some("S"));
}

#[test]
fn impl_trait_return_with_branches_agreeing_on_one_witness_is_replaced() {
    // Two `return` sites, one concrete type: still "one concrete return per
    // monomorphization", so still substitutable.
    let prog = lower_program(&format!(
        "{SRC_PREFIX}fn pick(hi: bool) -> impl Src[Item = i64] {{\n\
             if hi {{ S {{ v: 9 }} }} else {{ S {{ v: 1 }} }}\n\
         }}\n\
         fn main() {{ println(pick(true).get()); }}"
    ));
    assert_eq!(return_path_segment(&prog, "pick").as_deref(), Some("S"));
}

#[test]
fn impl_trait_return_on_an_impl_method_is_replaced() {
    let prog = lower_program(&format!(
        "{SRC_PREFIX}struct F {{}}\n\
         impl F {{ fn build(ref self) -> impl Src[Item = i64] {{ S {{ v: 3 }} }} }}\n\
         fn main() {{ let f = F {{}}; println(f.build().get()); }}"
    ));
    let mut found = false;
    for item in &prog.items {
        if let Item::ImplBlock(imp) = item {
            for it in &imp.items {
                if let karac::ast::ImplItem::Method(m) = it {
                    if m.name == "build" {
                        found = true;
                        assert!(
                            matches!(&m.return_type.as_ref().unwrap().kind,
                                     karac::ast::TypeKind::Path(p) if p.segments == vec!["S".to_string()]),
                            "impl-method return not substituted: {:?}",
                            m.return_type
                        );
                    }
                }
            }
        }
    }
    assert!(found, "impl method `build` not found");
}

#[test]
fn a_contested_existential_is_left_alone() {
    // Two DISTINCT witnesses is already `E_IMPL_TRAIT_MULTIPLE_WITNESSES`.
    // Substituting one of them would compile a program the typechecker
    // rejected, and pick arbitrarily; leaving it keeps codegen's loud
    // fall-through, which is the honest outcome for an erroring program.
    let prog = lower_program(
        "trait Src { fn get(ref self) -> i64; }\n\
         struct A {}\n\
         struct B {}\n\
         impl Src for A { fn get(ref self) -> i64 { 1 } }\n\
         impl Src for B { fn get(ref self) -> i64 { 2 } }\n\
         fn pick(hi: bool) -> impl Src { if hi { A {} } else { B {} } }\n\
         fn main() { println(pick(true).get()); }",
    );
    assert_eq!(
        return_path_segment(&prog, "pick"),
        None,
        "a multi-witness existential must not be substituted"
    );
}

#[test]
fn an_existential_declaring_effect_variables_is_left_alone() {
    // `collect_effect_var_names_in_type` harvests polymorphic effect params
    // off the `impl Trait` node and nowhere else, so erasing the node erases
    // the variable's declaration site. Concrete verbs are re-derived from the
    // witness's own impl methods and do NOT block the rewrite (the test
    // below); an effect VARIABLE does. B-2026-08-22-13.
    let prog = lower_program(
        "trait Emit { fn emit(ref self) -> i64; }\n\
         struct E { n: i64 }\n\
         impl Emit for E { fn emit(ref self) -> i64 { self.n } }\n\
         fn make[with F]() -> impl Emit with F { E { n: 4 } }\n\
         fn main() { let e = make(); println(e.emit()); }",
    );
    assert_eq!(
        return_path_segment(&prog, "make"),
        None,
        "an effect-variable existential must keep its `impl Trait` node"
    );
}

#[test]
fn an_existential_declaring_only_concrete_effects_is_replaced() {
    // The contrast to the test above. `writes(Log)` names no variable, and
    // after the rewrite `e.emit()` resolves to `E.emit`, which declares its
    // own effects — so the caller's inferred set is unchanged or more precise
    // rather than weaker. Verified behaviourally too: the public-fn
    // "performs effects [writes(Log)] but has no effect declaration"
    // rejection is byte-identical with and without the substitution.
    let prog = lower_program(
        "effect resource Log;\n\
         trait Emit { fn emit(ref self) -> i64; }\n\
         struct E { n: i64 }\n\
         impl Emit for E { fn emit(ref self) -> i64 with writes(Log) { self.n } }\n\
         fn make() -> impl Emit with writes(Log) { E { n: 4 } }\n\
         fn main() { let e = make(); println(e.emit()); }",
    );
    assert_eq!(return_path_segment(&prog, "make").as_deref(), Some("E"));
}
