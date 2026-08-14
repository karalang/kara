// src/chained_receiver_lint.rs
//! `chained_field_receiver` — surface codegen's FR4 deferral at CHECK time.
//!
//! B-2026-08-13-12. A method call or index whose receiver is a field chain of
//! depth ≥ 2 (`e.doc.lines.len()`, `b.a.lines[0]`) is deliberately deferred in
//! codegen — `lower_field_access_ptr` rejects it up front with a clear message
//! naming the remedy. The deferral is documented and intentional; what was
//! missing is that NOTHING said so before `karac build`:
//!
//! ```text
//! karac check                    All checks passed.
//! karac check --targets=native   All checks passed under target 'native'.
//! karac check --output=json      "diagnostics": []
//! karac fix                      (no fixable diagnostics in <file>)
//! karac build                    error: codegen failed: codegen: chained field
//!                                receivers (`a.b.c…`) are deferred to v1.x …
//! ```
//!
//! ## Why the empty diagnostics array is the bug
//!
//! Per CLAUDE.md the Mend loop's primary path is `karac check --output=json` →
//! `karac fix` → re-verify. An empty array gives an LLM author nothing to feed
//! back and nothing to apply, so it cannot converge: it hands back a program
//! that checks clean and does not build. That is the same failure mode as
//! B-2026-08-11-2 and -4, both filed and fixed as `run-vs-build`.
//!
//! ## The shape, which is purely syntactic
//!
//! Measured against `karac build`, and mirrored here exactly:
//!
//! ```text
//! d.lines.len()        depth 1, method     builds
//! e.doc.n              depth 2, FIELD only builds
//! let d = e.doc; d.lines.len()             builds
//! e.doc.lines.len()    depth 2, method     REFUSED
//! e.doc.name.len()     depth 2, String     REFUSED
//! c.b.a.lines.len()    depth 3, method     REFUSED
//! e.doc.lines[0]       depth 2, INDEX      REFUSED   (not in the row's table)
//! ```
//!
//! So: a METHOD CALL or an INDEX whose receiver is `<FieldAccess>.<field>` —
//! i.e. a field access whose own object is itself a field access. Plain field
//! reads at any depth are fine, and both refusals come from the same helper
//! (`lower_field_access_ptr`, shared by the method-receiver and index paths),
//! which is why one predicate covers both.
//!
//! ## No machine-applicable fix, and that is a measurement
//!
//! The row expected a free fix-it: hoist the receiver prefix into a `let` and
//! rewrite the call, which is what codegen's own message prescribes. That
//! rewrite is NOT semantics-preserving, and the two ways it fails were measured
//! rather than reasoned about:
//!
//! * For a MUTATING call it silently drops the write. `b.a.lines.push("y")`
//!   refuses to build today; hoisted to `let mut t = b.a; t.lines.push("y")` it
//!   builds and the container `b.a.lines` never sees the element. Trading a
//!   build error for a silent wrong answer is strictly worse.
//! * The hoist itself diverges run-vs-build even before the mutation:
//!   `let mut t = b.a;` leaves `b.a.lines` EMPTY under both compiled backends
//!   while the interpreter keeps it intact and treats the binding as an alias.
//!   Filed separately.
//!
//! An autofix that is right for readers and silently wrong for mutators is not
//! a fix-it, and the lint has no reliable syntactic test for which one it is
//! looking at — `remove` reads like a reader at a call site and mutates. So the
//! diagnostic names the remedy in prose, exactly as codegen does, and leaves
//! the edit to the author. When the divergence above is fixed, a fix-it gated
//! to provably read-only uses becomes a defensible follow-up.
//!
//! ## Level
//!
//! Registry default `Deny`, i.e. this fails `karac check`. The program does not
//! compile, so a warning would leave `check` exiting 0 on a program that cannot
//! build — the exact gap the row is about. `-A chained_field_receiver` opts out
//! for a program that only ever runs under `--interp`, where the shape is fine.

use crate::ast::{Block, Expr, ExprKind, Item, Program, Stmt, StmtKind};
use crate::typechecker::{TypeError, TypeErrorKind};

const LINT_NAME: &str = "chained_field_receiver";

/// Is `expr` a receiver codegen's `lower_field_access_ptr` refuses — a field
/// access whose own object is a field access?
///
/// Mirrors the FR4 gate rather than restating it in different terms: that
/// helper rejects exactly when the inner of the `FieldAccess` it is handed is
/// itself a `FieldAccess`, and it is reached from both the method-receiver and
/// the index path.
fn is_chained_field_receiver(expr: &Expr) -> bool {
    let ExprKind::FieldAccess { object, .. } = &expr.kind else {
        return false;
    };
    matches!(object.kind, ExprKind::FieldAccess { .. })
}

/// Render `a.b.c` back from the AST for the message, so the diagnostic names
/// the author's own chain rather than a generic placeholder. Falls back to
/// `None` for any shape that is not a pure field chain (which cannot reach the
/// caller, but keeps this total).
fn render_chain(expr: &Expr) -> Option<String> {
    match &expr.kind {
        ExprKind::Identifier(n) => Some(n.clone()),
        ExprKind::SelfValue => Some("self".to_string()),
        ExprKind::FieldAccess { object, field } => {
            Some(format!("{}.{}", render_chain(object)?, field))
        }
        _ => None,
    }
}

fn diagnostic(receiver: &Expr, ctx: &str) -> TypeError {
    let chain = render_chain(receiver).unwrap_or_else(|| "a.b.c".to_string());
    // The prefix to hoist is the receiver's own object — `e.doc` for
    // `e.doc.lines.len()` — which is what codegen's message means by "the inner
    // field" and what the measured working rewrite binds.
    let prefix = match &receiver.kind {
        ExprKind::FieldAccess { object, .. } => render_chain(object),
        _ => None,
    };
    let remedy = match prefix {
        Some(p) => {
            format!("bind the inner field to a temporary first — `let tmp = {p};`, then `tmp.…`")
        }
        None => "bind the inner field to a temporary first".to_string(),
    };
    TypeError {
        message: format!(
            "chained field receivers (`a.b.c…`) are deferred to v1.x in codegen, so `{chain}` \
             as the receiver of {ctx} checks clean but fails `karac build`; {remedy}. \
             (The tree-walk interpreter accepts this shape — `-A {LINT_NAME}` if the program \
             is only ever run with `--interp`.)"
        ),
        span: receiver.span.clone(),
        kind: TypeErrorKind::TypeMismatch,
        lint_name: Some(LINT_NAME.to_string()),
        // No `fix_it`: the hoist codegen prescribes is not semantics-preserving
        // for a mutating receiver, and diverges run-vs-build on its own. See the
        // module doc — this is a measurement, not an omission.
        fix_it: None,
        class: Some(crate::diagnostic_class::DiagnosticClass::LintWarning),
        expected: None,
        got: None,
    }
}

/// Walk the program and collect one diagnostic per refused receiver, plus
/// whether the resolved level makes them ERRORS.
///
/// The caller routes them into `TypeCheckResult::errors` or `::warnings`
/// accordingly — the registry default is `Deny`, so `karac check` fails by
/// default on a program that cannot build, and `-A chained_field_receiver`
/// (or `-W`) moves it without the emitter needing to know which.
pub fn check_chained_field_receivers(
    program: &Program,
    cli_lint_overrides: &crate::lints::CliLintOverrides,
) -> (Vec<TypeError>, bool) {
    let severity = crate::lints::effective_level_for_module_lint(
        false,
        false,
        false,
        cli_lint_overrides,
        LINT_NAME,
    );
    if matches!(severity, crate::lints::ModuleLintSeverity::Suppress) {
        return (Vec::new(), false);
    }
    let deny = matches!(severity, crate::lints::ModuleLintSeverity::Deny);
    let mut out = Vec::new();
    for item in &program.items {
        match item {
            Item::Function(f) => walk_block(&f.body, &mut out),
            Item::ImplBlock(imp) => {
                for it in &imp.items {
                    if let crate::ast::ImplItem::Method(m) = it {
                        walk_block(&m.body, &mut out);
                    }
                }
            }
            Item::TraitDef(t) => {
                for it in &t.items {
                    if let crate::ast::TraitItem::Method(m) = it {
                        if let Some(body) = &m.body {
                            walk_block(body, &mut out);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    (out, deny)
}

fn walk_block(block: &Block, out: &mut Vec<TypeError>) {
    for stmt in &block.stmts {
        walk_stmt(stmt, out);
    }
    if let Some(e) = &block.final_expr {
        walk_expr(e, out);
    }
}

fn walk_stmt(stmt: &Stmt, out: &mut Vec<TypeError>) {
    // Arm set mirrors `map_entry_lint::walk_stmt`, the maintained sibling.
    match &stmt.kind {
        StmtKind::Let { value, .. } => walk_expr(value, out),
        StmtKind::LetElse {
            value, else_block, ..
        } => {
            walk_expr(value, out);
            walk_block(else_block, out);
        }
        StmtKind::Expr(e) => walk_expr(e, out),
        StmtKind::Assign { target, value } | StmtKind::CompoundAssign { target, value, .. } => {
            walk_expr(target, out);
            walk_expr(value, out);
        }
        _ => {}
    }
}

fn walk_expr(expr: &Expr, out: &mut Vec<TypeError>) {
    match &expr.kind {
        ExprKind::MethodCall { object, method, .. } => {
            if is_chained_field_receiver(object) {
                out.push(diagnostic(object, &format!("method '{method}'")));
            }
        }
        ExprKind::Index { object, .. } if is_chained_field_receiver(object) => {
            out.push(diagnostic(object, "an index expression"));
        }
        _ => {}
    }
    // Recurse through the children so a chained receiver nested inside an
    // argument, an operand, a block or a match arm is found too — the walk is
    // what makes this a program-wide gate rather than a statement-shape check.
    // Arm set mirrors `map_entry_lint::walk_expr`, the maintained sibling.
    match &expr.kind {
        ExprKind::Block(b)
        | ExprKind::Par(b)
        | ExprKind::Seq(b)
        | ExprKind::Try(b)
        | ExprKind::Unsafe(b)
        | ExprKind::LabeledBlock { body: b, .. }
        | ExprKind::Loop { body: b, .. }
        | ExprKind::Lock { body: b, .. } => walk_block(b, out),
        ExprKind::If {
            condition,
            then_block,
            else_branch,
        } => {
            walk_expr(condition, out);
            walk_block(then_block, out);
            if let Some(e) = else_branch {
                walk_expr(e, out);
            }
        }
        ExprKind::While {
            condition, body, ..
        } => {
            walk_expr(condition, out);
            walk_block(body, out);
        }
        ExprKind::For { iterable, body, .. } => {
            walk_expr(iterable, out);
            walk_block(body, out);
        }
        ExprKind::Match { scrutinee, arms } => {
            walk_expr(scrutinee, out);
            for arm in arms {
                walk_expr(&arm.body, out);
            }
        }
        ExprKind::MethodCall { object, args, .. } => {
            walk_expr(object, out);
            for a in args {
                walk_expr(&a.value, out);
            }
        }
        ExprKind::Call { callee, args, .. } => {
            walk_expr(callee, out);
            for a in args {
                walk_expr(&a.value, out);
            }
        }
        ExprKind::Index { object, index } => {
            walk_expr(object, out);
            walk_expr(index, out);
        }
        ExprKind::Binary { left, right, .. } => {
            walk_expr(left, out);
            walk_expr(right, out);
        }
        ExprKind::Unary { operand, .. } => walk_expr(operand, out),
        ExprKind::FieldAccess { object, .. } => walk_expr(object, out),
        _ => {}
    }
}
