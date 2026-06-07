//! Source-pinning check for borrow returns (`-> ref T` / `-> mut ref T`).
//!
//! design.md § Feature 4 Part 3: "every `ref` value in a well-typed
//! program has a traceable source (a requirement of source pinning) ...
//! if a `ref` can't be traced to a parameter, that's a source pinning
//! error." A function that returns a borrow of a local / owned value /
//! temporary would hand the caller a reference into storage dropped at
//! function exit — a dangling reference.
//!
//! This is the callee half of the borrow-return feature (B-2026-06-07-5).
//! It is polish over codegen rather than a soundness backstop: codegen
//! only produces a return pointer for a `ref`-param-rooted source, so a
//! dangling source already fails at module verification — this check
//! upgrades that raw LLVM error into a clean, spanned diagnostic.
//!
//! Tier-1 scope mirrors `compile_ref_return_ptr` exactly: a returned
//! borrow is accepted iff it is a `ref` parameter / ref-local identifier,
//! or a field reached through one. Other valid-per-spec forms (`if`/`match`
//! of multiple `ref` params, method-call chains) are reported as
//! not-yet-supported rather than dangling.

use std::collections::HashSet;

use crate::ast::*;

use super::{BorrowKind, BorrowReturnShape, OwnershipError, OwnershipErrorKind};

impl<'a> super::OwnershipChecker<'a> {
    /// Verify every borrow returned by `f` (`-> ref T` / `-> mut ref T`)
    /// traces to a `ref` parameter. Emits `E0509` at each offending
    /// return expression. No-op for non-borrow-returning functions.
    pub(crate) fn check_ref_return_source_pinning(&mut self, f: &Function) {
        let Some(ret) = &f.return_type else {
            return;
        };
        if !matches!(ret.kind, TypeKind::Ref(_) | TypeKind::MutRef(_)) {
            // Tier-1: plain `ref T` / `mut ref T` returns only. Borrows
            // nested in generic wrappers (`Option[ref T]`) are a follow-on.
            return;
        }

        // Valid borrow sources: `ref` parameters, plus ref-locals — a
        // `let x = <call to a ref-returning fn>;` whose result is itself
        // a borrow that traces (transitively) to a `ref` parameter.
        let ref_params: HashSet<String> = f
            .params
            .iter()
            .filter(|p| matches!(p.ty.kind, TypeKind::Ref(_) | TypeKind::MutRef(_)))
            .flat_map(|p| p.pattern.binding_names())
            .collect();
        let ref_returning_fns = self.ref_returning_fn_names();
        let mut ref_locals: HashSet<String> = HashSet::new();
        collect_ref_locals(&f.body, &ref_returning_fns, &mut ref_locals);

        // Every return site: explicit `return e;` anywhere in the body,
        // plus the body's tail expression.
        let mut returns: Vec<&Expr> = Vec::new();
        collect_return_exprs_in_block(&f.body, &mut returns);
        if let Some(tail) = &f.body.final_expr {
            returns.push(tail);
        }

        for e in returns {
            let Some(shape) = classify_borrow_return(e, &ref_params, &ref_locals) else {
                continue;
            };
            let (message, suggestion) = match shape {
                BorrowReturnShape::DanglingSource => (
                    "returned borrow does not originate from a `ref` parameter; its source is \
                     dropped when the function returns, leaving a dangling reference"
                        .to_string(),
                    Some(
                        "a borrow return must trace to a `ref` parameter — e.g. \
                         `fn f(x: ref T) -> ref T { x }` or `fn f(u: ref U) -> ref F { u.field }`. \
                         To return an owned value instead, drop `ref` from the return type."
                            .to_string(),
                    ),
                ),
                BorrowReturnShape::UnsupportedForm => (
                    "this borrow-return form is not yet supported".to_string(),
                    Some(
                        "supported today: returning a `ref` parameter directly, or a field \
                         reached through one. `if`/`match` over several `ref` parameters and \
                         method-call chains are tracked follow-ons (B-2026-06-07-5)."
                            .to_string(),
                    ),
                ),
            };
            self.errors.push(OwnershipError {
                message,
                span: e.span.clone(),
                kind: OwnershipErrorKind::BorrowReturnNotSourcePinned { shape },
                suggestion,
                replacement: None,
                consume_span: None,
            });
        }
    }

    /// Caller-side borrow registration (check 3b). When `value` is a call
    /// to a borrow-returning function, the result borrows from the
    /// arguments at the callee's `ref`-parameter positions (conservative
    /// multi-source overapproximation, design.md § Feature 4 Part 3). Push
    /// a persistent active borrow on each such argument's root binding so a
    /// later move/consume of that source while the borrow is live is
    /// rejected by `check_move_of_borrowed` — closing the use-after-free
    /// hole where `let n = name_of(u); sink(u); use(n)` would dangle.
    ///
    /// Must be invoked from the `let` arm *after* the RHS call has been
    /// walked: call-argument borrows are snapshot-restored when the call
    /// returns, so a borrow pushed here (outside that snapshot) is the one
    /// that persists for the binding's scope and drains at scope exit.
    pub(crate) fn register_ref_return_borrows(&mut self, value: &Expr) {
        let ExprKind::Call { callee, args } = &value.kind else {
            return;
        };
        let ExprKind::Identifier(fname) = &callee.kind else {
            return;
        };
        if !self.ref_returning_fn_names().contains(fname) {
            return;
        }
        for (i, arg) in args.iter().enumerate() {
            if self.arg_is_borrow_position(callee, i) {
                if let Some(place) = self.place_expr_root(&arg.value) {
                    self.push_active_borrow(BorrowKind::ImmRef, place, arg.value.span.clone());
                }
            }
        }
    }

    /// Names of program-level functions whose declared return type is a
    /// borrow. Used to recognise ref-locals (`let x = ref_returning()`).
    fn ref_returning_fn_names(&self) -> HashSet<String> {
        let mut names = HashSet::new();
        for item in &self.program.items {
            if let Item::Function(f) = item {
                if let Some(rt) = &f.return_type {
                    if matches!(rt.kind, TypeKind::Ref(_) | TypeKind::MutRef(_)) {
                        names.insert(f.name.clone());
                    }
                }
            }
        }
        names
    }
}

/// Classify a returned expression as a valid borrow source (`None`) or an
/// offending one. Mirrors `compile_ref_return_ptr`'s accepted shapes.
fn classify_borrow_return(
    e: &Expr,
    ref_params: &HashSet<String>,
    ref_locals: &HashSet<String>,
) -> Option<BorrowReturnShape> {
    match &e.kind {
        ExprKind::Identifier(n) => {
            if ref_params.contains(n) || ref_locals.contains(n) {
                None
            } else {
                Some(BorrowReturnShape::DanglingSource)
            }
        }
        ExprKind::FieldAccess { object, .. } => match &object.kind {
            ExprKind::Identifier(b) if ref_params.contains(b) || ref_locals.contains(b) => None,
            ExprKind::Identifier(_) => Some(BorrowReturnShape::DanglingSource),
            // A field reached through a non-identifier (a chained field
            // access, a call, …) — valid per spec but Tier-2/3 codegen.
            _ => Some(BorrowReturnShape::UnsupportedForm),
        },
        // Literals and temporaries are unambiguously dangling; the rest
        // (`if`/`match`/`Call`/`MethodCall`/…) are valid-but-unsupported.
        ExprKind::Integer(..)
        | ExprKind::Float(..)
        | ExprKind::Bool(..)
        | ExprKind::CharLit(..)
        | ExprKind::StringLit(..)
        | ExprKind::ArrayLiteral(..)
        | ExprKind::StructLiteral { .. }
        | ExprKind::Tuple(..) => Some(BorrowReturnShape::DanglingSource),
        _ => Some(BorrowReturnShape::UnsupportedForm),
    }
}

/// Names bound by `let <name> = <call to a ref-returning fn>;` anywhere in
/// the block tree. These are ref-locals — valid borrow-return sources.
fn collect_ref_locals(block: &Block, ref_fns: &HashSet<String>, out: &mut HashSet<String>) {
    for stmt in &block.stmts {
        if let StmtKind::Let { pattern, value, .. } = &stmt.kind {
            if let PatternKind::Binding(name) = &pattern.kind {
                if let ExprKind::Call { callee, .. } = &value.kind {
                    if let ExprKind::Identifier(fname) = &callee.kind {
                        if ref_fns.contains(fname) {
                            out.insert(name.clone());
                        }
                    }
                }
            }
        }
        collect_ref_locals_in_stmt(stmt, ref_fns, out);
    }
    if let Some(e) = &block.final_expr {
        collect_ref_locals_in_expr(e, ref_fns, out);
    }
}

fn collect_ref_locals_in_stmt(stmt: &Stmt, ref_fns: &HashSet<String>, out: &mut HashSet<String>) {
    match &stmt.kind {
        StmtKind::Let { value, .. }
        | StmtKind::LetElse { value, .. }
        | StmtKind::Expr(value)
        | StmtKind::Assign { value, .. }
        | StmtKind::CompoundAssign { value, .. } => collect_ref_locals_in_expr(value, ref_fns, out),
        StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
            collect_ref_locals(body, ref_fns, out)
        }
        StmtKind::LetUninit { .. } => {}
    }
}

fn collect_ref_locals_in_expr(e: &Expr, ref_fns: &HashSet<String>, out: &mut HashSet<String>) {
    for_each_subblock(e, &mut |b| collect_ref_locals(b, ref_fns, out));
}

/// Collect every `return e;` expression in the block tree.
fn collect_return_exprs_in_block<'e>(block: &'e Block, out: &mut Vec<&'e Expr>) {
    for stmt in &block.stmts {
        collect_return_exprs_in_stmt(stmt, out);
    }
    if let Some(e) = &block.final_expr {
        collect_return_exprs_in_expr(e, out);
    }
}

fn collect_return_exprs_in_stmt<'e>(stmt: &'e Stmt, out: &mut Vec<&'e Expr>) {
    match &stmt.kind {
        StmtKind::Let { value, .. }
        | StmtKind::LetElse { value, .. }
        | StmtKind::Expr(value)
        | StmtKind::Assign { value, .. }
        | StmtKind::CompoundAssign { value, .. } => collect_return_exprs_in_expr(value, out),
        StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
            collect_return_exprs_in_block(body, out)
        }
        StmtKind::LetUninit { .. } => {}
    }
}

fn collect_return_exprs_in_expr<'e>(e: &'e Expr, out: &mut Vec<&'e Expr>) {
    if let ExprKind::Return(Some(inner)) = &e.kind {
        out.push(inner);
    }
    for_each_subblock(e, &mut |b| collect_return_exprs_in_block(b, out));
}

/// Invoke `f` on every `Block` directly nested in `e` (one level; the
/// callbacks recurse). Covers the control-flow and grouping expression
/// forms that can host statements / nested returns. Leaf and operator
/// expressions have no nested blocks and are ignored — a missed nested
/// `return` only degrades a dangling diagnostic to the codegen-level
/// verifier error, never a soundness gap (see module docs).
fn for_each_subblock<'e>(e: &'e Expr, f: &mut dyn FnMut(&'e Block)) {
    match &e.kind {
        ExprKind::Block(b)
        | ExprKind::Unsafe(b)
        | ExprKind::Try(b)
        | ExprKind::Seq(b)
        | ExprKind::Par(b) => f(b),
        ExprKind::If {
            then_block,
            else_branch,
            ..
        }
        | ExprKind::IfLet {
            then_block,
            else_branch,
            ..
        } => {
            f(then_block);
            if let Some(eb) = else_branch {
                collect_else_branch(eb, f);
            }
        }
        ExprKind::While { body, .. }
        | ExprKind::WhileLet { body, .. }
        | ExprKind::For { body, .. }
        | ExprKind::Loop { body, .. }
        | ExprKind::LabeledBlock { body, .. } => f(body),
        ExprKind::Match { arms, .. } => {
            for arm in arms {
                collect_return_exprs_or_blocks_in_match_arm(arm, f);
            }
        }
        _ => {}
    }
}

fn collect_else_branch<'e>(eb: &'e Expr, f: &mut dyn FnMut(&'e Block)) {
    // An `else` is either a `Block` expr or a chained `if` — recurse so
    // `else if` chains are covered.
    match &eb.kind {
        ExprKind::Block(b) => f(b),
        _ => for_each_subblock(eb, f),
    }
}

fn collect_return_exprs_or_blocks_in_match_arm<'e>(
    arm: &'e MatchArm,
    f: &mut dyn FnMut(&'e Block),
) {
    // A match-arm body is an expression; route through the block hook so
    // both block-bodied and expression-bodied arms are visited.
    match &arm.body.kind {
        ExprKind::Block(b) => f(b),
        _ => for_each_subblock(&arm.body, f),
    }
}
