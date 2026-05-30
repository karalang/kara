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

use crate::ast::{Block, Expr, ExprKind, Function, Item, Program, Stmt, StmtKind};
use crate::token::Span;

/// Append a synthesized `fn main()` to `program` that calls
/// `test_fn_name()`. Any existing `Item::Function` named `main` is
/// removed first — codegen would otherwise emit two `@main` symbols and
/// the LLVM module verifier rejects the duplicate. The test runner's
/// per-module program is built from a module's items, which in
/// practice never carry a `main` for a `_test.kara` file; the filter is
/// defensive for the future where a single `.kara` source carries both
/// production code and inline tests.
///
/// `test_fn_name` is the mangled identifier already registered by
/// `lower_and_discover_test_cases` — i.e., the same value the
/// interpreter path passes to `Interpreter::run_test_function`. The
/// synthesizer does not invent a new name; it routes through the same
/// symbol the existing test-discovery contract pins.
pub fn append_test_main(program: &mut Program, test_fn_name: &str) {
    program.items.retain(|item| !is_main_function(item));

    let call_expr = synth_call_expr(test_fn_name);
    let body = synth_block_with_expr_stmt(call_expr);
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

/// Wrap an expression as the sole `ExprStmt` of a fresh `Block` with no
/// final expression — codegen treats this as a unit-valued statement.
fn synth_block_with_expr_stmt(expr: Expr) -> Block {
    let zero = Span::default();
    Block {
        stmts: vec![Stmt {
            kind: StmtKind::Expr(expr),
            span: zero.clone(),
        }],
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
        is_track_caller: false,
        lint_overrides: Vec::new(),
        profile_compat: Vec::new(),
    }
}
