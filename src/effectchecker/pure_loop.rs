//! `pure_loop_in_par` (B-2026-08-21-2) — a loop inside a `par { }` branch
//! whose body contains no effect boundary.
//!
//! design.md § Parallel Failure and Cleanup, "Termination": *"a `par` branch
//! containing a loop whose body has no effect-boundary checks is not
//! cancellable mid-iteration — the cooperative cancellation mechanism has
//! nowhere to observe the flag. Termination of such branches is the
//! programmer's responsibility. The compiler emits `warn[pure_loop_in_par]`
//! when it detects a loop body with no effect boundaries inside a `par`
//! branch."*
//!
//! THE TRIGGER IS CANCELLABILITY, NOT PARALLELISM. The registered description
//! said "a loop whose body has no parallelisable work", which is a different
//! (and unimplementable) rule — the row that tracks this class warns that a
//! lint's registry description is not a reliable statement of its trigger, and
//! this was the second instance. The description is corrected alongside this
//! pass.
//!
//! WHAT COUNTS AS A BOUNDARY is not re-derived here. Codegen's
//! [`crate::codegen`] `emit_branch_cancel_check` skips a call site exactly
//! when `callee_effectful[name] == Some(false)`, and that table is built from
//! [`super::effect_set_is_effectful`] — so this pass calls the SAME predicate
//! rather than restating it. A lint that disagreed with the checks actually
//! emitted would be worse than no lint: it would tell the programmer a loop is
//! uncancellable when the compiler had in fact placed a check in it.
//!
//! Unknown callees are treated as boundaries, matching that same rule: the
//! cancel check is skipped only on a definite `Some(false)`, so anything the
//! table cannot classify already gets a check emitted and must not be warned
//! about here.

use crate::ast::{Block, Expr, ExprKind, Program, Stmt, StmtKind};
use crate::effectchecker::DeclaredEffects;
use crate::token::Span;

/// Collect the spans of loops inside `par` branches whose bodies contain no
/// effect boundary. `is_effectful` answers whether a callee name carries a
/// resource effect; `None` means "not classifiable", which counts as a
/// boundary.
pub(crate) fn pure_loops_in_par(
    program: &Program,
    is_effectful: &dyn Fn(&str) -> Option<bool>,
) -> Vec<Span> {
    let mut out = Vec::new();
    for item in &program.items {
        if let crate::ast::Item::Function(f) = item {
            scan_block_for_par(&f.body, is_effectful, &mut out);
        }
    }
    out
}

/// Find `par { }` blocks anywhere in `block`, including nested ones.
fn scan_block_for_par(block: &Block, eff: &dyn Fn(&str) -> Option<bool>, out: &mut Vec<Span>) {
    for stmt in &block.stmts {
        walk_stmt(stmt, &mut |e| scan_expr_for_par(e, eff, out));
    }
    if let Some(tail) = &block.final_expr {
        walk_expr(tail, &mut |e| scan_expr_for_par(e, eff, out));
    }
}

fn scan_expr_for_par(expr: &Expr, eff: &dyn Fn(&str) -> Option<bool>, out: &mut Vec<Span>) {
    let ExprKind::Par(body) = &expr.kind else {
        return;
    };
    // Each TOP-LEVEL statement of a `par { }` block is one concurrent branch
    // with its own scope (the resolver enforces the isolation), so a loop is
    // "in a branch" exactly when it sits under one of these statements. The
    // tail expression is the join, not a branch, so it is not scanned.
    for stmt in &body.stmts {
        walk_stmt(stmt, &mut |e| {
            if let Some((body, span)) = loop_parts(e) {
                if !block_has_boundary(body, eff) {
                    out.push(span);
                }
            }
        });
    }
}

/// Render a callee expression to the key `callee_effectful` is indexed by.
/// Deliberately the SAME three shapes codegen's `compile_call` recovers
/// (identifier, two-segment path, field access) so the lint and the emitted
/// cancel checks cannot disagree about what a call site is called.
fn callee_key(callee: &Expr) -> Option<String> {
    match &callee.kind {
        ExprKind::Identifier(name) => Some(name.clone()),
        ExprKind::Path { segments, .. } if segments.len() == 2 => {
            Some(format!("{}.{}", segments[0], segments[1]))
        }
        ExprKind::Path { segments, .. } if segments.len() == 1 => Some(segments[0].clone()),
        ExprKind::FieldAccess { object, field } => match &object.kind {
            ExprKind::Identifier(root) => Some(format!("{root}.{field}")),
            _ => None,
        },
        _ => None,
    }
}

/// The body and span of a loop expression, or `None` for anything else.
fn loop_parts(expr: &Expr) -> Option<(&Block, Span)> {
    match &expr.kind {
        ExprKind::While { body, .. }
        | ExprKind::WhileLet { body, .. }
        | ExprKind::Loop { body, .. }
        | ExprKind::For { body, .. } => Some((body, expr.span)),
        _ => None,
    }
}

/// True when the block contains a call that would carry a cooperative-cancel
/// check. A NESTED loop's body counts too: a check anywhere inside the outer
/// loop's body makes the outer loop cancellable.
fn block_has_boundary(block: &Block, eff: &dyn Fn(&str) -> Option<bool>) -> bool {
    let mut found = false;
    let mut visit = |e: &Expr| {
        if found {
            return;
        }
        match &e.kind {
            ExprKind::Call { callee, .. } => {
                // Unknown callees count as boundaries, matching the emitted
                // check: `emit_branch_cancel_check` skips only on a definite
                // `Some(false)`.
                match callee_key(callee) {
                    Some(k) if eff(&k) == Some(false) => {}
                    _ => found = true,
                }
            }
            // A method call is not resolvable to a bare name here, so it is
            // conservatively a boundary — the same answer codegen reaches,
            // since `callee_effectful` has no entry to return `Some(false)`.
            // A method call carries no `callee_effectful` key here, so it is
            // conservatively a boundary — the same answer codegen reaches.
            ExprKind::MethodCall { .. } => found = true,
            _ => {}
        }
    };
    for stmt in &block.stmts {
        walk_stmt(stmt, &mut visit);
    }
    if let Some(tail) = &block.final_expr {
        walk_expr(tail, &mut visit);
    }
    found
}

/// Apply `f` to every expression in the statement, outermost first.
fn walk_stmt(stmt: &Stmt, f: &mut dyn FnMut(&Expr)) {
    match &stmt.kind {
        StmtKind::Let { value, .. } | StmtKind::Expr(value) => walk_expr(value, f),
        StmtKind::LetElse {
            value, else_block, ..
        } => {
            walk_expr(value, f);
            for s in &else_block.stmts {
                walk_stmt(s, f);
            }
            if let Some(t) = &else_block.final_expr {
                walk_expr(t, f);
            }
        }
        StmtKind::Assign { target, value } => {
            walk_expr(target, f);
            walk_expr(value, f);
        }
        StmtKind::CompoundAssign { target, value, .. } => {
            walk_expr(target, f);
            walk_expr(value, f);
        }
        StmtKind::MultiAssign { targets, values } => {
            for t in targets {
                walk_expr(t, f);
            }
            for v in values {
                walk_expr(v, f);
            }
        }
        StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
            for s in &body.stmts {
                walk_stmt(s, f);
            }
            if let Some(t) = &body.final_expr {
                walk_expr(t, f);
            }
        }
        StmtKind::LetUninit { .. } => {}
    }
}

/// Apply `f` to `expr` and every sub-expression. Deliberately structural and
/// total-by-fallthrough: an unhandled variant simply contributes no
/// sub-expressions, which for both callers is the conservative direction —
/// a missed call site is a missed boundary only inside a body that already
/// has to prove itself EMPTY of boundaries, and `block_has_boundary`'s
/// unknown-callee rule keeps that from silently warning.
/// Apply `f` to every expression in the block.
fn walk_block(b: &Block, f: &mut dyn FnMut(&Expr)) {
    for s in &b.stmts {
        walk_stmt(s, f);
    }
    if let Some(t) = &b.final_expr {
        walk_expr(t, f);
    }
}

/// True when `inner` lies within `outer`.
fn span_within(inner: Span, outer: Span) -> bool {
    inner.offset >= outer.offset && inner.offset + inner.length <= outer.offset + outer.length
}

fn walk_expr(expr: &Expr, f: &mut dyn FnMut(&Expr)) {
    f(expr);
    match &expr.kind {
        ExprKind::Call { callee, args, .. } => {
            walk_expr(callee, f);
            for a in args {
                walk_expr(&a.value, f);
            }
        }
        ExprKind::MethodCall { object, args, .. } => {
            walk_expr(object, f);
            for a in args {
                walk_expr(&a.value, f);
            }
        }
        ExprKind::Binary { left, right, .. } => {
            walk_expr(left, f);
            walk_expr(right, f);
        }
        ExprKind::Unary { operand, .. } => walk_expr(operand, f),
        ExprKind::Question(inner) => walk_expr(inner, f),
        ExprKind::FieldAccess { object, .. } => walk_expr(object, f),
        ExprKind::Index { object, index } => {
            walk_expr(object, f);
            walk_expr(index, f);
        }
        ExprKind::Cast { expr: inner, .. } => walk_expr(inner, f),
        ExprKind::Tuple(items)
        | ExprKind::ArrayLiteral(items)
        | ExprKind::PrefixCollectionLiteral { items, .. } => {
            for i in items {
                walk_expr(i, f);
            }
        }
        ExprKind::While {
            condition, body, ..
        } => {
            walk_expr(condition, f);
            walk_block(body, f);
        }
        ExprKind::WhileLet { value, body, .. } => {
            walk_expr(value, f);
            walk_block(body, f);
        }
        ExprKind::For { iterable, body, .. } => {
            walk_expr(iterable, f);
            walk_block(body, f);
        }
        ExprKind::Loop { body, .. } | ExprKind::LabeledBlock { body, .. } => walk_block(body, f),
        ExprKind::If {
            condition,
            then_block,
            else_branch,
        } => {
            walk_expr(condition, f);
            walk_block(then_block, f);
            if let Some(eb) = else_branch {
                walk_expr(eb, f);
            }
        }
        ExprKind::IfLet {
            value,
            then_block,
            else_branch,
            ..
        } => {
            walk_expr(value, f);
            walk_block(then_block, f);
            if let Some(eb) = else_branch {
                walk_expr(eb, f);
            }
        }
        ExprKind::Match { scrutinee, arms } => {
            walk_expr(scrutinee, f);
            for arm in arms {
                if let Some(g) = &arm.guard {
                    walk_expr(g, f);
                }
                walk_expr(&arm.body, f);
            }
        }
        ExprKind::Block(b)
        | ExprKind::Comptime(b)
        | ExprKind::Unsafe(b)
        | ExprKind::Try(b)
        | ExprKind::Seq(b)
        | ExprKind::Par(b) => walk_block(b, f),
        ExprKind::Lock { mutex, body, .. } => {
            walk_expr(mutex, f);
            walk_block(body, f);
        }
        ExprKind::Providers { bindings, body } => {
            for b in bindings {
                walk_expr(&b.value, f);
            }
            walk_block(body, f);
        }
        ExprKind::Range { start, end, .. } => {
            if let Some(s) = start {
                walk_expr(s, f);
            }
            if let Some(e) = end {
                walk_expr(e, f);
            }
        }
        ExprKind::Closure { body, .. } => walk_expr(body, f),
        _ => {}
    }
}

impl<'a> super::EffectChecker<'a> {
    /// Emit `warn[pure_loop_in_par]` for each loop in a `par { }` branch whose
    /// body carries no effect boundary (B-2026-08-21-2).
    ///
    /// Suppression follows the spec's own wording — *"the programmer can
    /// suppress the warning with `#[allow(pure_loop_in_par)]` if the loop is
    /// intentionally bounded"* — so the PER-LOOP form is the primary one, read
    /// from the parser's `stmt_lint_overrides` (span-keyed, exactly the frame
    /// the typechecker pushes around a statement). The enclosing function's
    /// own `#[allow]` is honoured too, matching `mutual_recursion_note` and
    /// `ownership.rs`'s `rc_fallback`: this phase has no access to the
    /// typechecker's cascade, so both ends are read directly off the tree.
    /// True when this loop's own statement carries `#[allow(pure_loop_in_par)]`,
    /// or its enclosing function does.
    fn pure_loop_allowed_at(&self, span: Span) -> bool {
        // The override is keyed by the STATEMENT's span, and the loop sits
        // inside that statement (`#[allow(...)] let a = { ... while ... }`), so
        // an exact-key lookup finds nothing. Match any statement whose span
        // CONTAINS the loop — the same containment the typechecker's cascade
        // frame gives a statement over its subexpressions.
        for (k, overrides) in &self.program.stmt_lint_overrides {
            let stmt_span = Span {
                offset: k.0,
                length: k.1,
                ..span
            };
            if !span_within(span, stmt_span) {
                continue;
            }
            if overrides
                .iter()
                .any(|o| o.lint == "pure_loop_in_par" && o.level == crate::lints::LintLevel::Allow)
            {
                return true;
            }
        }
        // Function-level fallback, matching `mutual_recursion_note`.
        self.program.items.iter().any(|item| {
            let crate::ast::Item::Function(f) = item else {
                return false;
            };
            if !span_within(span, f.span) {
                return false;
            }
            f.attributes.iter().any(|a| {
                a.is_bare("allow")
                    && a.args.iter().any(|arg| {
                        matches!(
                            &arg.value,
                            Some(crate::ast::Expr {
                                kind: crate::ast::ExprKind::Identifier(name),
                                ..
                            }) if name == "pure_loop_in_par"
                        )
                    })
            })
        })
    }

    pub(crate) fn emit_pure_loop_in_par_warnings(&mut self) {
        let inferred = &self.inferred_effects;
        let declared = &self.declared_effects;
        let interner = &self.interner;
        // Mirrors `build_callee_effectful_table`: the declared set wins where
        // both exist, and a polymorphic declaration is effectful because a
        // monomorphization may pick up any effect.
        let is_effectful = |name: &str| -> Option<bool> {
            // The lowered builtin operators come first: they have no
            // declaration to look up, and a miss would read as "unknown",
            // which counts as a boundary and would silence this lint on every
            // loop that does arithmetic — i.e. all of them.
            if super::lowered_builtin_op_is_pure(name) {
                return Some(false);
            }
            let sym = interner.get(name)?;
            if let Some(d) = declared.get(&sym) {
                return Some(match d {
                    DeclaredEffects::Explicit(set) => super::effect_set_is_effectful(set),
                    DeclaredEffects::PolymorphicWithFixed(_) | DeclaredEffects::Polymorphic => true,
                    DeclaredEffects::None => false,
                });
            }
            inferred.get(&sym).map(super::effect_set_is_effectful)
        };
        let spans = pure_loops_in_par(self.program, &is_effectful);
        for span in spans {
            if self.pure_loop_allowed_at(span) {
                continue;
            }
            self.errors.push(super::EffectError {
                message: "loop in a `par` branch has no effect boundary, so cooperative \
                          cancellation cannot be observed inside it: a sibling failure \
                          will not interrupt this loop mid-iteration and terminating it \
                          is the programmer's responsibility. Suppress with \
                          `#[allow(pure_loop_in_par)]` if the loop is intentionally \
                          bounded"
                    .to_string(),
                span,
                kind: super::EffectErrorKind::PureLoopInPar,
                subtype_trace: None,
                replacement: None,
            });
        }
    }
}
