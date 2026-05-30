//! Per-test `main` synthesizer for the `karac test` JIT cutover.
//!
//! Slice (c).2 — given a per-module `Program` (with `Item::TestCase`s
//! already lowered to `Item::Function`s by `cli.rs::lower_and_discover_test_cases`)
//! and the mangled name of a specific test function, append a synthesized
//! `fn main()` that calls that test fn. The codegen pipeline then lowers
//! the modified `Program` to LLVM IR; the runner's per-test JIT
//! subprocess (slice c.3) executes the binary, captures stderr for the
//! `KARAC_TEST_FAILURE` markers emitted by c.1's `assert*` lowering,
//! and maps the resulting exit code + stderr lines into a `TestOutcome`.
//!
//! Why a synthesizer rather than a fresh codegen path: every other karac
//! compilation (build, run) lowers a `Program` that already has its own
//! `main`. Bolting a JIT entry point into codegen would duplicate the
//! main-emission machinery; appending a synthesized `Item::Function`
//! keeps the entry-point story uniform across `build` / `run` / `test`.
//!
//! Source-order fixture wrapping (`#[with_provider(R, ctor)]`) is the
//! follow-up sub-slice c.2b — it nests `with_provider[R](ctor, || ...)`
//! calls around the test-fn call so the provider stack is pushed before
//! the body runs and popped after. The core c.2a form here covers the
//! no-fixture path used by every test in the existing kara corpus that
//! doesn't take a provider.

use crate::ast::{
    Block, CallArg, Expr, ExprKind, Function, Item, Pattern, PatternKind, Program, Stmt, StmtKind,
};
use crate::token::Span;

/// One `#[with_provider(R, ctor)]` fixture to wrap around the test fn.
/// Iterated source-order; the synthesizer nests `with_provider[R](ctor,
/// || ...)` calls so the first fixture is the *outermost* push (matches
/// the interpreter's `cmd_test` semantics, where the first-pushed frame
/// sits at the bottom of the stack and pops last).
#[derive(Debug, Clone)]
pub struct ProviderFixture {
    /// Fully-qualified resource path — the type the `effect resource R: T`
    /// declaration names. Becomes the `R` in `with_provider[R](...)`.
    pub resource_path: String,
    /// Constructor expression evaluated at fixture-push time. Already a
    /// parsed AST node — `cmd_test`'s `extract_with_providers` reads it
    /// out of the `#[with_provider(R, ctor)]` attribute payload, and
    /// the synthesizer passes it through verbatim into the
    /// `with_provider[R](ctor, ...)` call's first positional arg.
    pub constructor: Expr,
}

/// Append a synthesized `fn main()` to `program` that calls
/// `test_fn_name()`, optionally wrapped in source-order `with_provider`
/// calls for each fixture. Any existing `Item::Function` named `main`
/// is removed first — codegen would otherwise emit two `@main` symbols
/// and the LLVM module verifier rejects the duplicate. The test
/// runner's per-module program is built from a module's items, which
/// in practice never carry a `main` for a `_test.kara` file; the
/// filter is defensive for the future where a single `.kara` source
/// carries both production code and inline tests.
///
/// `test_fn_name` is the mangled identifier already registered by
/// `lower_and_discover_test_cases` — i.e., the same value the
/// interpreter path passes to `Interpreter::run_test_function`. The
/// synthesizer does not invent a new name; it routes through the same
/// symbol the existing test-discovery contract pins.
///
/// Fixture wrapping produces nested closures: for two fixtures
/// `[(R1, c1), (R2, c2)]` in source order the body becomes
/// `with_provider[R1](c1, || with_provider[R2](c2, || test_fn()))`,
/// which matches the interpreter's push-in-source-order /
/// pop-in-reverse contract — `R1`'s frame survives until `R2`'s pop
/// fires.
pub fn append_test_main(program: &mut Program, test_fn_name: &str, fixtures: &[ProviderFixture]) {
    program.items.retain(|item| !is_main_function(item));

    // The codegen `with_provider` lowering at `src/codegen/provider.rs`
    // requires the provider arg to be either an `Identifier` with a
    // known struct type or a `StructLiteral` — it cannot infer a
    // concrete type from arbitrary call expressions (e.g.
    // `MyProvider.new()`). Bind each fixture's constructor to a fresh
    // `let` first, then reference the binding by name. Mirrors the
    // parallax_lite source pattern (`let pa = InMemoryMetrics.new();
    // with_provider[MetricsA](pa, || ...)`).
    let mut let_stmts: Vec<Stmt> = Vec::with_capacity(fixtures.len());
    let mut provider_idents: Vec<String> = Vec::with_capacity(fixtures.len());
    for (i, fx) in fixtures.iter().enumerate() {
        let binding = format!("__karac_test_provider_{i}");
        let_stmts.push(synth_let_binding(&binding, fx.constructor.clone()));
        provider_idents.push(binding);
    }

    // Innermost expression is the test-fn call. Wrap source-last to
    // source-first so the first fixture becomes the outermost
    // `with_provider`.
    let mut inner: Expr = synth_call_expr(test_fn_name);
    for (fx, binding) in fixtures.iter().zip(provider_idents.iter()).rev() {
        let provider_ref = synth_identifier_expr(binding);
        inner = synth_with_provider_call(&fx.resource_path, provider_ref, inner);
    }

    let body = synth_block_with_lets_and_final_call(let_stmts, inner);
    let main_fn = synth_main_function(body);
    program.items.push(Item::Function(main_fn));
}

fn is_main_function(item: &Item) -> bool {
    matches!(item, Item::Function(f) if f.name == "main")
}

/// Bare-identifier call: `<test_fn_name>()`.
fn synth_call_expr(test_fn_name: &str) -> Expr {
    let zero = Span::default();
    let callee = Expr {
        kind: ExprKind::Identifier(test_fn_name.to_string()),
        span: zero.clone(),
    };
    Expr {
        kind: ExprKind::Call {
            callee: Box::new(callee),
            args: Vec::new(),
        },
        span: zero,
    }
}

/// Build `with_provider[R](provider, || inner)` — the exact AST shape
/// `codegen/helpers.rs::match_with_provider_call` recognizes, and the
/// interpreter's `Interpreter::match_with_provider` /
/// `provider_escape::match_with_provider` both mirror. Callee is
/// `Index { Identifier("with_provider"), Identifier(R) }`; args are
/// `[provider, closure]` with no labels / no `mut` markers (the
/// matcher rejects labels).
fn synth_with_provider_call(resource: &str, provider: Expr, inner: Expr) -> Expr {
    let zero = Span::default();
    let closure = Expr {
        kind: ExprKind::Closure {
            params: Vec::new(),
            capture_mode: None,
            prefix_span: None,
            body: Box::new(inner),
        },
        span: zero.clone(),
    };
    let callee = Expr {
        kind: ExprKind::Index {
            object: Box::new(Expr {
                kind: ExprKind::Identifier("with_provider".to_string()),
                span: zero.clone(),
            }),
            index: Box::new(Expr {
                kind: ExprKind::Identifier(resource.to_string()),
                span: zero.clone(),
            }),
        },
        span: zero.clone(),
    };
    Expr {
        kind: ExprKind::Call {
            callee: Box::new(callee),
            args: vec![
                CallArg {
                    label: None,
                    mut_marker: false,
                    value: provider,
                    span: zero.clone(),
                },
                CallArg {
                    label: None,
                    mut_marker: false,
                    value: closure,
                    span: zero.clone(),
                },
            ],
        },
        span: zero,
    }
}

/// `Identifier(name)` Expr — references a `let`-bound fixture binding
/// from inside the `with_provider` call.
fn synth_identifier_expr(name: &str) -> Expr {
    let zero = Span::default();
    Expr {
        kind: ExprKind::Identifier(name.to_string()),
        span: zero,
    }
}

/// `let NAME = INIT;` — the binding mode codegen needs to register a
/// concrete struct type against the name. Codegen consults the
/// resulting var_type_names entry in `infer_provider_type_name`.
fn synth_let_binding(name: &str, init: Expr) -> Stmt {
    let zero = Span::default();
    Stmt {
        kind: StmtKind::Let {
            is_mut: false,
            pattern: Pattern {
                kind: PatternKind::Binding(name.to_string()),
                span: zero.clone(),
            },
            ty: None,
            value: init,
        },
        span: zero,
    }
}

/// Wrap `let_stmts` + `final_call` as the body of `main`. With no
/// fixtures `let_stmts` is empty and `final_call` is the bare test-fn
/// call; with fixtures `let_stmts` carries one per fixture and
/// `final_call` is the nested `with_provider` chain referencing each
/// binding. The final call sits as a trailing `ExprStmt` (terminated
/// with implicit `;` semantics) so the block has no `final_expr` —
/// matches what codegen's main-emission arm expects (it injects
/// `ret i32 0` regardless of any value the body would have produced).
fn synth_block_with_lets_and_final_call(mut let_stmts: Vec<Stmt>, final_call: Expr) -> Block {
    let zero = Span::default();
    let_stmts.push(Stmt {
        kind: StmtKind::Expr(final_call),
        span: zero.clone(),
    });
    Block {
        stmts: let_stmts,
        final_expr: None,
        span: zero,
    }
}

/// Build a synthesized `fn main()` carrying `body`. Mirrors the
/// default-everything shape `lower_test_case_to_function` uses for
/// `Item::TestCase` lowering — every optional field is `None` / empty
/// so downstream phases see a vanilla zero-arg, unit-return free fn.
fn synth_main_function(body: Block) -> Function {
    let zero = Span::default();
    Function {
        span: zero,
        attributes: Vec::new(),
        doc_comment: None,
        is_pub: false,
        is_private: false,
        is_unsafe: false,
        name: "main".to_string(),
        generic_params: None,
        params: Vec::new(),
        self_param: None,
        return_type: None,
        effects: None,
        requires: Vec::new(),
        ensures: Vec::new(),
        where_clause: None,
        body,
        stdlib_origin: false,
        deprecation: None,
        unstable: None,
        is_track_caller: false,
        lint_overrides: Vec::new(),
        profile_compat: Vec::new(),
    }
}
