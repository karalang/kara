//! Comptime fold pass — the compile-time evaluator (substrate 1).
//!
//! This is the first of the four comptime substrates from
//! [`docs/deferred.md` § Comptime — "Implementation phases"]: a treewalk
//! evaluator over the typed AST that runs `comptime { ... }` blocks at
//! compile time and splices their constant results back into the program.
//! As the spec puts it, the evaluator "is essentially the existing Phase 4
//! interpreter retargeted to compile-time invocation" — so rather than
//! re-implement evaluation, this pass drives the real
//! [`crate::interpreter::Interpreter`] against a snapshot of the program
//! and folds the resulting [`Value`] into a literal [`Expr`].
//!
//! ## Pipeline position
//!
//! Runs after `lower` and before the interpreter / codegen consume the
//! program:
//!
//! ```text
//! … → typecheck → lower → [comptime fold] → effectcheck/ownership → interpret/codegen
//! ```
//!
//! Folding after `lower` means the snapshot interpreter sees exactly the
//! same (lowered) tree the real interpreter would, and downstream phases
//! see plain constants in place of every `comptime { ... }` node.
//!
//! ## Scope (slice 2)
//!
//! - Evaluates `comptime { ... }` block expressions anywhere an expression
//!   can appear (function bodies, `const` initializers, impl methods, …).
//! - Folds scalar results (int / float / bool / char / string / unit) plus
//!   homogeneous compound results (tuples, arrays / vecs) into literals.
//! - Surfaces a comptime panic as a compile error
//!   (`E_COMPTIME_PANIC`), a non-terminating evaluation as
//!   `E_COMPTIME_ITER_LIMIT_EXCEEDED` (enforced via a wall-clock guard on
//!   the shared interpreter deadline hook), and a result shape that can't
//!   be expressed as a literal as `E_COMPTIME_NON_FOLDABLE_RESULT`.
//!
//! Effect restriction (`E_RUNTIME_EFFECT_AT_COMPTIME`), `Type` reflection,
//! the AST builder, and derive desugaring are later substrates and are not
//! part of this pass.

use std::time::{Duration, Instant};

use crate::ast::*;
use crate::interpreter::{Interpreter, Value};
use crate::resolver::SpanKey;
use crate::token::{FloatSuffix, IntSuffix, Span};
use crate::typechecker::{FloatSize, IntSize, Type, TypeCheckResult, UIntSize};

/// Wall-clock ceiling for a single `comptime { ... }` evaluation. The
/// language spec mandates an instruction ceiling (`2^24` per top-level
/// invocation, `--comptime-iter-limit`); slice 2 approximates it with a
/// wall-clock deadline routed through the interpreter's existing
/// statement-boundary deadline poll (`set_test_deadline`). A real
/// instruction counter is future work; the deadline is the safety net that
/// keeps a runaway `comptime` loop from hanging the compiler.
const COMPTIME_WALL_CLOCK_LIMIT: Duration = Duration::from_secs(5);

/// A diagnostic produced by the comptime fold pass. Mirrors the
/// span+message shape of the other phases' error records so the CLI can
/// render and count them alongside typecheck / effect / ownership errors.
#[derive(Debug, Clone)]
pub struct ComptimeError {
    pub message: String,
    pub span: Span,
}

/// Evaluate every `comptime { ... }` block in `program` at compile time and
/// replace each with its folded constant, in place. Returns the
/// diagnostics produced (empty on success). `typed` is the typecheck result
/// for `program`; it supplies the result type used to pick literal suffixes
/// and to distinguish `Vec`-typed from `Array`-typed collection results.
pub fn evaluate(program: &mut Program, typed: &TypeCheckResult) -> Vec<ComptimeError> {
    // Fast path: no comptime nodes ⇒ no snapshot, no interpreter.
    if !program_has_comptime(program) {
        return Vec::new();
    }

    // The interpreter borrows its program immutably for its whole lifetime,
    // but this pass mutates `program` to splice in folded constants. Resolve
    // the conflict by evaluating against an independent snapshot: the
    // interpreter borrows `snapshot`, while the mutable walk rewrites
    // `program`. Spans are preserved by `Clone`, so a node found in `program`
    // matches its twin in `snapshot` for any span-keyed lookup the
    // interpreter performs.
    let snapshot = program.clone();
    let mut interp = Interpreter::new(&snapshot, typed);
    // Prime the global environment exactly as `run()` does (prelude variants,
    // baked stdlib impls, user items / functions) — but without calling
    // `main()`. Comptime fn calls resolve through these registrations.
    interp.register_items();
    // Capture (and discard) any output a comptime block produces so stray
    // prints don't leak into the build log. `compiler.print` is a later
    // substrate; until then this just keeps the channel quiet.
    interp.captured_output = Some(Vec::new());

    let mut folder = Folder {
        interp,
        typed,
        errors: Vec::new(),
    };
    for item in &mut program.items {
        folder.fold_item(item);
    }
    folder.errors
}

/// True if any item in the program contains a `comptime { ... }` node.
fn program_has_comptime(program: &Program) -> bool {
    program.items.iter().any(item_has_comptime)
}

fn item_has_comptime(item: &Item) -> bool {
    match item {
        Item::Function(f) => block_has_comptime(&f.body),
        Item::ImplBlock(imp) => imp.items.iter().any(|it| match it {
            ImplItem::Method(m) => block_has_comptime(&m.body),
            _ => false,
        }),
        Item::ConstDecl(c) => expr_has_comptime(&c.value),
        _ => false,
    }
}

fn block_has_comptime(block: &Block) -> bool {
    block.stmts.iter().any(stmt_has_comptime)
        || block
            .final_expr
            .as_ref()
            .is_some_and(|e| expr_has_comptime(e))
}

fn stmt_has_comptime(stmt: &Stmt) -> bool {
    match &stmt.kind {
        StmtKind::Let { value, .. } => expr_has_comptime(value),
        StmtKind::LetElse {
            value, else_block, ..
        } => expr_has_comptime(value) || block_has_comptime(else_block),
        StmtKind::LetUninit { .. } => false,
        StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => block_has_comptime(body),
        StmtKind::Assign { target, value } => expr_has_comptime(target) || expr_has_comptime(value),
        StmtKind::CompoundAssign { target, value, .. } => {
            expr_has_comptime(target) || expr_has_comptime(value)
        }
        StmtKind::Expr(e) => expr_has_comptime(e),
        StmtKind::MultiAssign { .. } => false,
    }
}

fn expr_has_comptime(expr: &Expr) -> bool {
    let mut found = false;
    walk_child_exprs(expr, &mut |e| {
        if matches!(e.kind, ExprKind::Comptime(_)) {
            found = true;
        }
        !found // keep descending only while nothing found
    });
    found
}

/// Visit `expr` and every sub-expression, calling `f` on each. The closure
/// returns `true` to continue descending into that node's children, `false`
/// to prune. Used by the read-only `expr_has_comptime` probe; the mutating
/// rewrite lives in `Folder::fold_expr`.
fn walk_child_exprs(expr: &Expr, f: &mut impl FnMut(&Expr) -> bool) {
    if !f(expr) {
        return;
    }
    macro_rules! go {
        ($e:expr) => {
            walk_child_exprs($e, f)
        };
    }
    match &expr.kind {
        ExprKind::Binary { left, right, .. } | ExprKind::NilCoalesce { left, right } => {
            go!(left);
            go!(right);
        }
        ExprKind::Unary { operand, .. } => go!(operand),
        ExprKind::Question(inner) | ExprKind::Cast { expr: inner, .. } => go!(inner),
        ExprKind::OptionalChain { object, args, .. } => {
            go!(object);
            if let Some(args) = args {
                for a in args {
                    go!(&a.value);
                }
            }
        }
        ExprKind::Call { callee, args } => {
            go!(callee);
            for a in args {
                go!(&a.value);
            }
        }
        ExprKind::MethodCall { object, args, .. } => {
            go!(object);
            for a in args {
                go!(&a.value);
            }
        }
        ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => go!(object),
        ExprKind::Index { object, index } => {
            go!(object);
            go!(index);
        }
        ExprKind::Block(b)
        | ExprKind::Comptime(b)
        | ExprKind::Unsafe(b)
        | ExprKind::Try(b)
        | ExprKind::Seq(b)
        | ExprKind::Par(b)
        | ExprKind::LabeledBlock { body: b, .. }
        | ExprKind::Loop { body: b, .. } => walk_child_exprs_block(b, f),
        ExprKind::If {
            condition,
            then_block,
            else_branch,
        } => {
            go!(condition);
            walk_child_exprs_block(then_block, f);
            if let Some(eb) = else_branch {
                go!(eb);
            }
        }
        ExprKind::IfLet {
            value,
            then_block,
            else_branch,
            ..
        } => {
            go!(value);
            walk_child_exprs_block(then_block, f);
            if let Some(eb) = else_branch {
                go!(eb);
            }
        }
        ExprKind::Match { scrutinee, arms } => {
            go!(scrutinee);
            for arm in arms {
                if let Some(g) = &arm.guard {
                    go!(g);
                }
                go!(&arm.body);
            }
        }
        ExprKind::While {
            condition, body, ..
        } => {
            go!(condition);
            walk_child_exprs_block(body, f);
        }
        ExprKind::WhileLet { value, body, .. } => {
            go!(value);
            walk_child_exprs_block(body, f);
        }
        ExprKind::For { iterable, body, .. } => {
            go!(iterable);
            walk_child_exprs_block(body, f);
        }
        ExprKind::Closure { body, .. } => go!(body),
        ExprKind::Return(opt) | ExprKind::Break { value: opt, .. } => {
            if let Some(e) = opt {
                go!(e);
            }
        }
        ExprKind::Tuple(es) | ExprKind::ArrayLiteral(es) => {
            for e in es {
                go!(e);
            }
        }
        ExprKind::PrefixCollectionLiteral { items, .. } => {
            for e in items {
                go!(e);
            }
        }
        ExprKind::RepeatLiteral { value, count, .. } => {
            go!(value);
            go!(count);
        }
        ExprKind::MapLiteral(pairs) => {
            for (k, v) in pairs {
                go!(k);
                go!(v);
            }
        }
        ExprKind::StructLiteral { fields, spread, .. } => {
            for fi in fields {
                go!(&fi.value);
            }
            if let Some(s) = spread {
                go!(s);
            }
        }
        ExprKind::Pipe { left, right } => {
            go!(left);
            go!(right);
        }
        ExprKind::Range { start, end, .. } => {
            if let Some(s) = start {
                go!(s);
            }
            if let Some(e) = end {
                go!(e);
            }
        }
        ExprKind::Lock { mutex, body, .. } => {
            go!(mutex);
            walk_child_exprs_block(body, f);
        }
        ExprKind::Providers { bindings, body } => {
            for b in bindings {
                go!(&b.value);
            }
            walk_child_exprs_block(body, f);
        }
        // Leaf nodes.
        ExprKind::Integer(_, _)
        | ExprKind::Float(_, _)
        | ExprKind::CharLit(_)
        | ExprKind::ByteLit(_)
        | ExprKind::StringLit(_)
        | ExprKind::MultiStringLit(_)
        | ExprKind::InterpolatedStringLit(_)
        | ExprKind::CStringLit { .. }
        | ExprKind::Bool(_)
        | ExprKind::Identifier(_)
        | ExprKind::Path { .. }
        | ExprKind::SelfValue
        | ExprKind::SelfType
        | ExprKind::PipePlaceholder
        | ExprKind::Continue { .. }
        | ExprKind::OffsetOf { .. }
        | ExprKind::Error => {}
    }
}

fn walk_child_exprs_block(block: &Block, f: &mut impl FnMut(&Expr) -> bool) {
    for stmt in &block.stmts {
        match &stmt.kind {
            StmtKind::Let { value, .. } => walk_child_exprs(value, f),
            StmtKind::LetElse {
                value, else_block, ..
            } => {
                walk_child_exprs(value, f);
                walk_child_exprs_block(else_block, f);
            }
            StmtKind::LetUninit { .. } | StmtKind::MultiAssign { .. } => {}
            StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
                walk_child_exprs_block(body, f)
            }
            StmtKind::Assign { target, value } => {
                walk_child_exprs(target, f);
                walk_child_exprs(value, f);
            }
            StmtKind::CompoundAssign { target, value, .. } => {
                walk_child_exprs(target, f);
                walk_child_exprs(value, f);
            }
            StmtKind::Expr(e) => walk_child_exprs(e, f),
        }
    }
    if let Some(e) = &block.final_expr {
        walk_child_exprs(e, f);
    }
}

// ── Mutating fold walker ────────────────────────────────────────

struct Folder<'a> {
    interp: Interpreter<'a>,
    typed: &'a TypeCheckResult,
    errors: Vec<ComptimeError>,
}

impl Folder<'_> {
    fn fold_item(&mut self, item: &mut Item) {
        match item {
            Item::Function(f) => self.fold_block(&mut f.body),
            Item::ImplBlock(imp) => {
                for it in &mut imp.items {
                    if let ImplItem::Method(m) = it {
                        self.fold_block(&mut m.body);
                    }
                }
            }
            Item::ConstDecl(c) => self.fold_expr(&mut c.value),
            _ => {}
        }
    }

    fn fold_block(&mut self, block: &mut Block) {
        for stmt in &mut block.stmts {
            self.fold_stmt(stmt);
        }
        if let Some(e) = &mut block.final_expr {
            self.fold_expr(e);
        }
    }

    fn fold_stmt(&mut self, stmt: &mut Stmt) {
        match &mut stmt.kind {
            StmtKind::Let { value, .. } => self.fold_expr(value),
            StmtKind::LetElse {
                value, else_block, ..
            } => {
                self.fold_expr(value);
                self.fold_block(else_block);
            }
            StmtKind::LetUninit { .. } | StmtKind::MultiAssign { .. } => {}
            StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => self.fold_block(body),
            StmtKind::Assign { target, value } => {
                self.fold_expr(target);
                self.fold_expr(value);
            }
            StmtKind::CompoundAssign { target, value, .. } => {
                self.fold_expr(target);
                self.fold_expr(value);
            }
            StmtKind::Expr(e) => self.fold_expr(e),
        }
    }

    fn fold_expr(&mut self, expr: &mut Expr) {
        // A `comptime { ... }` node is evaluated whole — the snapshot
        // interpreter handles any nesting inside it (including nested
        // `comptime` blocks), so we do NOT pre-recurse into its children.
        if matches!(expr.kind, ExprKind::Comptime(_)) {
            self.eval_and_splice(expr);
            return;
        }
        // Every other node: recurse into children so comptime nodes buried
        // anywhere in the tree get folded.
        match &mut expr.kind {
            ExprKind::Binary { left, right, .. } | ExprKind::NilCoalesce { left, right } => {
                self.fold_expr(left);
                self.fold_expr(right);
            }
            ExprKind::Unary { operand, .. } => self.fold_expr(operand),
            ExprKind::Question(inner) | ExprKind::Cast { expr: inner, .. } => self.fold_expr(inner),
            ExprKind::OptionalChain { object, args, .. } => {
                self.fold_expr(object);
                if let Some(args) = args {
                    for a in args {
                        self.fold_expr(&mut a.value);
                    }
                }
            }
            ExprKind::Call { callee, args } => {
                self.fold_expr(callee);
                for a in args {
                    self.fold_expr(&mut a.value);
                }
            }
            ExprKind::MethodCall { object, args, .. } => {
                self.fold_expr(object);
                for a in args {
                    self.fold_expr(&mut a.value);
                }
            }
            ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
                self.fold_expr(object)
            }
            ExprKind::Index { object, index } => {
                self.fold_expr(object);
                self.fold_expr(index);
            }
            ExprKind::Block(b)
            | ExprKind::Unsafe(b)
            | ExprKind::Try(b)
            | ExprKind::Seq(b)
            | ExprKind::Par(b)
            | ExprKind::LabeledBlock { body: b, .. }
            | ExprKind::Loop { body: b, .. } => self.fold_block(b),
            ExprKind::If {
                condition,
                then_block,
                else_branch,
            } => {
                self.fold_expr(condition);
                self.fold_block(then_block);
                if let Some(eb) = else_branch {
                    self.fold_expr(eb);
                }
            }
            ExprKind::IfLet {
                value,
                then_block,
                else_branch,
                ..
            } => {
                self.fold_expr(value);
                self.fold_block(then_block);
                if let Some(eb) = else_branch {
                    self.fold_expr(eb);
                }
            }
            ExprKind::Match { scrutinee, arms } => {
                self.fold_expr(scrutinee);
                for arm in arms {
                    if let Some(g) = &mut arm.guard {
                        self.fold_expr(g);
                    }
                    self.fold_expr(&mut arm.body);
                }
            }
            ExprKind::While {
                condition, body, ..
            } => {
                self.fold_expr(condition);
                self.fold_block(body);
            }
            ExprKind::WhileLet { value, body, .. } => {
                self.fold_expr(value);
                self.fold_block(body);
            }
            ExprKind::For { iterable, body, .. } => {
                self.fold_expr(iterable);
                self.fold_block(body);
            }
            ExprKind::Closure { body, .. } => self.fold_expr(body),
            ExprKind::Return(opt) | ExprKind::Break { value: opt, .. } => {
                if let Some(e) = opt {
                    self.fold_expr(e);
                }
            }
            ExprKind::Tuple(es) | ExprKind::ArrayLiteral(es) => {
                for e in es {
                    self.fold_expr(e);
                }
            }
            ExprKind::PrefixCollectionLiteral { items, .. } => {
                for e in items {
                    self.fold_expr(e);
                }
            }
            ExprKind::RepeatLiteral { value, count, .. } => {
                self.fold_expr(value);
                self.fold_expr(count);
            }
            ExprKind::MapLiteral(pairs) => {
                for (k, v) in pairs {
                    self.fold_expr(k);
                    self.fold_expr(v);
                }
            }
            ExprKind::StructLiteral { fields, spread, .. } => {
                for fi in fields {
                    self.fold_expr(&mut fi.value);
                }
                if let Some(s) = spread {
                    self.fold_expr(s);
                }
            }
            ExprKind::Pipe { left, right } => {
                self.fold_expr(left);
                self.fold_expr(right);
            }
            ExprKind::Range { start, end, .. } => {
                if let Some(s) = start {
                    self.fold_expr(s);
                }
                if let Some(e) = end {
                    self.fold_expr(e);
                }
            }
            ExprKind::Lock { mutex, body, .. } => {
                self.fold_expr(mutex);
                self.fold_block(body);
            }
            ExprKind::Providers { bindings, body } => {
                for b in bindings {
                    self.fold_expr(&mut b.value);
                }
                self.fold_block(body);
            }
            // `Comptime` handled above; leaf nodes have no children.
            ExprKind::Comptime(_)
            | ExprKind::Integer(_, _)
            | ExprKind::Float(_, _)
            | ExprKind::CharLit(_)
            | ExprKind::ByteLit(_)
            | ExprKind::StringLit(_)
            | ExprKind::MultiStringLit(_)
            | ExprKind::InterpolatedStringLit(_)
            | ExprKind::CStringLit { .. }
            | ExprKind::Bool(_)
            | ExprKind::Identifier(_)
            | ExprKind::Path { .. }
            | ExprKind::SelfValue
            | ExprKind::SelfType
            | ExprKind::PipePlaceholder
            | ExprKind::Continue { .. }
            | ExprKind::OffsetOf { .. }
            | ExprKind::Error => {}
        }
    }

    /// Evaluate a `comptime { ... }` node and replace it with the folded
    /// constant literal, recording a diagnostic on any failure.
    fn eval_and_splice(&mut self, expr: &mut Expr) {
        let span = expr.span.clone();
        let block = match &expr.kind {
            ExprKind::Comptime(b) => b.clone(),
            _ => return,
        };

        // Evaluate the inner block via the snapshot interpreter, wrapped as a
        // plain block expression (the interpreter has no separate comptime
        // entry point — same eval path the defensive `Comptime` arm uses).
        let wrapped = Expr {
            kind: ExprKind::Block(block),
            span: span.clone(),
        };

        // Reset per-evaluation interpreter state and arm the wall-clock guard.
        self.interp.pending_cf = None;
        let errors_before = self.interp.runtime_errors.len();
        let user_errors_before = self.interp.comptime_user_errors.len();
        self.interp.timed_out = false;
        self.interp
            .set_test_deadline(Some(Instant::now() + COMPTIME_WALL_CLOCK_LIMIT));

        let value = self.interp.eval_expr(&wrapped);

        self.interp.set_test_deadline(None);

        // Drain any `compiler.error(msg)` diagnostics emitted during this
        // block's evaluation (substrate 3 — compile-time validation). These
        // are non-halting: collect them and continue to the splice/fold below
        // so a block that reports several issues surfaces all of them.
        for diag in self
            .interp
            .comptime_user_errors
            .split_off(user_errors_before)
        {
            self.errors.push(ComptimeError {
                message: format!("error[E_COMPTIME_ERROR]: {}", diag.message),
                span: diag.span,
            });
        }

        // Runaway evaluation hit the wall-clock guard.
        if self.interp.timed_out {
            self.errors.push(ComptimeError {
                message: format!(
                    "error[E_COMPTIME_ITER_LIMIT_EXCEEDED]: `comptime` evaluation did not \
                     terminate within {}s; a `comptime` block must compute a constant in \
                     bounded time (deferred.md § Comptime — Resource limits)",
                    COMPTIME_WALL_CLOCK_LIMIT.as_secs()
                ),
                span,
            });
            return;
        }

        // A comptime panic is a compile error, not a runtime panic — the
        // diagnostic surfaces at the calling site (deferred.md § Comptime —
        // Effect system integration: `panics`).
        if self.interp.runtime_errors.len() > errors_before {
            let detail = self
                .interp
                .runtime_errors
                .last()
                .map(|e| e.message.clone())
                .unwrap_or_else(|| "comptime evaluation failed".to_string());
            self.errors.push(ComptimeError {
                message: format!(
                    "error[E_COMPTIME_PANIC]: `comptime` evaluation panicked: {detail}"
                ),
                span,
            });
            return;
        }

        // Code generation (substrate 3): when the block yields an `Expr` AST
        // value (a quasi-quote `ast.expr(...)`), splice the *generated code*
        // at the comptime site rather than folding a constant. The spliced
        // expression is then evaluated/compiled in the surrounding scope, so
        // it can reference runtime bindings.
        if let Value::AstExpr(generated) = value {
            *expr = *generated;
            return;
        }

        // Fold the resulting value into a literal, using the typechecked
        // result type to pick literal suffixes / collection shape.
        let result_ty = self
            .typed
            .expr_types
            .get(&SpanKey::from_span(&span))
            .cloned();
        match value_to_expr(&value, result_ty.as_ref(), &span) {
            Ok(folded) => *expr = folded,
            Err(reason) => self.errors.push(ComptimeError {
                message: format!(
                    "error[E_COMPTIME_NON_FOLDABLE_RESULT]: `comptime` block produced a value \
                     that cannot be spliced as a constant: {reason}"
                ),
                span,
            }),
        }
    }
}

// ── Value → literal Expr folding ────────────────────────────────

/// Convert an evaluated [`Value`] into a constant [`Expr`] carrying `span`.
/// `ty` is the typechecked result type of the `comptime` block (when
/// known) — it drives literal-suffix selection and the `Vec`-vs-`Array`
/// collection-literal choice. Returns `Err(reason)` for value shapes that
/// have no constant literal form in slice 2.
fn value_to_expr(value: &Value, ty: Option<&Type>, span: &Span) -> Result<Expr, String> {
    let kind = match value {
        Value::Int(n) => ExprKind::Integer(*n, int_suffix(ty)),
        Value::Float(f) => ExprKind::Float(*f, float_suffix(ty)),
        Value::Bool(b) => ExprKind::Bool(*b),
        Value::Char(c) => ExprKind::CharLit(*c),
        Value::String(s) => ExprKind::StringLit(s.clone()),
        Value::Unit => ExprKind::Tuple(Vec::new()),
        Value::Tuple(items) => {
            let elem_tys = tuple_element_types(ty);
            let folded = items
                .iter()
                .enumerate()
                .map(|(i, v)| value_to_expr(v, elem_tys.and_then(|ts| ts.get(i)), span))
                .collect::<Result<Vec<_>, _>>()?;
            ExprKind::Tuple(folded)
        }
        Value::Array(cell) => {
            let elem_ty = element_type(ty);
            let folded = cell
                .read()
                .unwrap()
                .iter()
                .map(|v| value_to_expr(v, elem_ty.as_ref(), span))
                .collect::<Result<Vec<_>, _>>()?;
            // A `Vec`-typed result must round-trip as a heap Vec literal; a
            // fixed `Array[T, N]` result as a bare array literal. `lower`
            // has already run, so we emit the canonical post-lower shape
            // directly (Vec → prefix literal; Array → array literal).
            if matches!(ty, Some(Type::Named { name, .. }) if name == "Vec") {
                ExprKind::PrefixCollectionLiteral {
                    type_name: "Vec".to_string(),
                    items: folded,
                }
            } else {
                ExprKind::ArrayLiteral(folded)
            }
        }
        Value::Function { .. } => return Err("a function value".to_string()),
        Value::Struct { name, .. } => {
            return Err(format!(
                "a `{name}` struct value (struct folding is a later comptime substrate)"
            ))
        }
        Value::SharedStruct(_) => {
            return Err(
                "a shared-struct value (struct folding is a later comptime substrate)".to_string(),
            )
        }
        Value::EnumVariant { enum_name, .. } => {
            return Err(format!(
                "a `{enum_name}` enum value (enum folding is a later comptime substrate)"
            ))
        }
        Value::Map(_) => {
            return Err("a map value (map folding is a later comptime substrate)".into())
        }
        other => {
            return Err(format!(
                "an unsupported value shape ({}); only scalars, tuples, and arrays fold in slice 2",
                value_kind_name(other)
            ))
        }
    };
    Ok(Expr {
        kind,
        span: span.clone(),
    })
}

/// A short tag for the runtime `Value` variant, for diagnostic text.
fn value_kind_name(v: &Value) -> &'static str {
    match v {
        Value::Int(_) => "int",
        Value::Float(_) => "float",
        Value::Bool(_) => "bool",
        Value::Char(_) => "char",
        Value::String(_) => "string",
        Value::Unit => "unit",
        Value::Tuple(_) => "tuple",
        Value::Array(_) => "array",
        Value::Map(_) => "map",
        Value::Struct { .. } => "struct",
        Value::EnumVariant { .. } => "enum",
        Value::Function { .. } => "function",
        _ => "value",
    }
}

/// Map an integer result type to its literal suffix. `usize` has no integer
/// literal suffix in the surface syntax, so it folds to a suffixless literal
/// (the type is recovered from context downstream).
fn int_suffix(ty: Option<&Type>) -> Option<IntSuffix> {
    match ty? {
        Type::Int(IntSize::I8) => Some(IntSuffix::I8),
        Type::Int(IntSize::I16) => Some(IntSuffix::I16),
        Type::Int(IntSize::I32) => Some(IntSuffix::I32),
        Type::Int(IntSize::I64) => Some(IntSuffix::I64),
        Type::Int(IntSize::I128) => Some(IntSuffix::I128),
        Type::UInt(UIntSize::U8) => Some(IntSuffix::U8),
        Type::UInt(UIntSize::U16) => Some(IntSuffix::U16),
        Type::UInt(UIntSize::U32) => Some(IntSuffix::U32),
        Type::UInt(UIntSize::U64) => Some(IntSuffix::U64),
        Type::UInt(UIntSize::U128) => Some(IntSuffix::U128),
        _ => None,
    }
}

fn float_suffix(ty: Option<&Type>) -> Option<FloatSuffix> {
    match ty? {
        Type::Float(FloatSize::F32) => Some(FloatSuffix::F32),
        Type::Float(FloatSize::F64) => Some(FloatSuffix::F64),
        _ => None,
    }
}

/// Element type of an `Array[T, N]` or `Vec[T]` result type, if known.
fn element_type(ty: Option<&Type>) -> Option<Type> {
    match ty? {
        Type::Array { element, .. } => Some((**element).clone()),
        Type::Named { name, args } if name == "Vec" && args.len() == 1 => Some(args[0].clone()),
        _ => None,
    }
}

/// Element types of a tuple result type, if known.
fn tuple_element_types(ty: Option<&Type>) -> Option<&[Type]> {
    match ty? {
        Type::Tuple(elems) => Some(elems.as_slice()),
        _ => None,
    }
}
