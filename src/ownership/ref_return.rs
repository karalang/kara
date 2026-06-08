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
//! Accepted scope mirrors `compile_ref_return_ptr` exactly (lockstep): a
//! returned borrow is accepted iff it is a `ref` parameter / `ref self` /
//! ref-local identifier, a field reached through one, an `if`/scalar-
//! selector `match` selecting among such borrows, a chained borrow-returning
//! free-fn call, or a *borrowed-struct* construction whose every `ref` field
//! traces to a borrowable source (design.md Feature 4 Part 3). Other
//! valid-per-spec forms (method-call chains, destructuring/guarded `match`
//! arms) are reported as not-yet-supported rather than dangling.

use std::collections::HashSet;

use crate::ast::*;
use crate::resolver::SpanKey;
use crate::typechecker::Type;

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
        let mut ref_params: HashSet<String> = f
            .params
            .iter()
            .filter(|p| matches!(p.ty.kind, TypeKind::Ref(_) | TypeKind::MutRef(_)))
            .flat_map(|p| p.pattern.binding_names())
            .collect();
        // A `ref self` / `mut ref self` receiver is a borrow source too —
        // it lives in `self_param`, not `params` (method accessors:
        // `fn name(ref self) -> ref String { self.name }`).
        if matches!(f.self_param, Some(SelfParam::Ref) | Some(SelfParam::MutRef)) {
            ref_params.insert("self".to_string());
        }
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
            // Chained borrow return (`echo(t)` in return position): a
            // borrow-returning **free-fn** call dispatches to the call
            // classifier, which traces every ref-position argument back to a
            // `ref` param / ref-local. Handled only at the top level — a call
            // nested in an `if`/`match` arm still falls through to
            // `classify_borrow_return` (→ `UnsupportedForm`), keeping 3a in
            // lockstep with codegen (which only lowers a top-level call tail).
            let shape = if matches!(&e.kind, ExprKind::Call { .. }) {
                self.classify_borrow_return_call(e, &ref_params, &ref_locals)
            } else if matches!(&e.kind, ExprKind::StructLiteral { .. }) {
                // Borrowed-struct return (`Parser { source: s, position: 0 }`
                // as `-> ref Parser`): pinned iff every `ref` field's
                // initializer traces to a `ref` param. Top-level only (a
                // struct literal nested in an `if`/`match` arm falls through to
                // `classify_borrow_return`'s conservative `StructLiteral`
                // arm) — in lockstep with codegen, which lowers a top-level
                // borrowed-struct tail/return by value.
                self.classify_borrow_return_struct(e, &ref_params, &ref_locals)
            } else {
                classify_borrow_return(e, &ref_params, &ref_locals)
            };
            let Some(shape) = shape else {
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
                        "supported today: returning a `ref` parameter (or `ref self`) directly, a \
                         field reached through one, an `if`/scalar-`match` selecting among such \
                         borrows, a chained borrow-returning free-fn call, or a borrowed-struct \
                         construction whose `ref` fields trace to parameters. Method-call chains \
                         and destructuring-`match` arms are tracked follow-ons (B-2026-06-07-5)."
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

    /// Source-pinning classification for a borrow-returning **free-function**
    /// call in return position (`echo(t)` — chained borrow returns,
    /// B-2026-06-07-5). The call's result borrows from its `ref`-position
    /// arguments (the callee's own source-pinning guarantees that); so it is
    /// pinned to *this* function's `ref` params iff every `ref`-position
    /// argument is itself a borrowable source. A non-`ref`-returning callee,
    /// or a non-identifier callee, is `UnsupportedForm` (method-call chains
    /// included — kept in lockstep with codegen's free-fn-only
    /// `is_borrow_returning_call_expr`). Returns `None` to accept.
    fn classify_borrow_return_call(
        &self,
        e: &Expr,
        ref_params: &HashSet<String>,
        ref_locals: &HashSet<String>,
    ) -> Option<BorrowReturnShape> {
        let ExprKind::Call { callee, args } = &e.kind else {
            return Some(BorrowReturnShape::UnsupportedForm);
        };
        let ExprKind::Identifier(fname) = &callee.kind else {
            return Some(BorrowReturnShape::UnsupportedForm);
        };
        if !self.ref_returning_fn_names().contains(fname) {
            return Some(BorrowReturnShape::UnsupportedForm);
        }
        // Each ref-position arg must trace to a borrowable source; the worst
        // shape across them dominates (a dangling arg → E0509, an
        // unsupported arg → not-yet-supported). Owned (by-value) args carry
        // no borrow into the result, so they are not checked.
        let mut worst: Option<BorrowReturnShape> = None;
        for (i, arg) in args.iter().enumerate() {
            if self.arg_is_borrow_position(callee, i) {
                worst = combine_borrow_shapes(
                    worst,
                    classify_borrow_return(&arg.value, ref_params, ref_locals),
                );
            }
        }
        worst
    }

    /// Source-pinning classification for a *borrowed-struct* construction
    /// returned as `-> ref Struct` (design.md Feature 4 Part 3): "Returning a
    /// borrowed struct follows the same rule as returning a `ref` value: the
    /// borrowed struct's sources must all be parameters. The compiler traces
    /// each `ref` field to its source parameter automatically." A borrowed
    /// struct is one with at least one `ref` field; the struct is pinned iff
    /// every `ref` field's initializer traces to a borrowable source (worst
    /// shape dominates). Owned-field initializers carry no borrow and are
    /// ignored. An owned struct (no `ref` fields) returned as `ref` is a
    /// dangling borrow of a temporary (`DanglingSource`). A `..spread` base
    /// could carry a borrow from an unknown source, so it is conservatively
    /// `UnsupportedForm`. Returns `None` to accept.
    fn classify_borrow_return_struct(
        &self,
        e: &Expr,
        ref_params: &HashSet<String>,
        ref_locals: &HashSet<String>,
    ) -> Option<BorrowReturnShape> {
        let ExprKind::StructLiteral {
            path,
            fields,
            spread,
        } = &e.kind
        else {
            return Some(BorrowReturnShape::UnsupportedForm);
        };
        if spread.is_some() {
            return Some(BorrowReturnShape::UnsupportedForm);
        }
        let Some(struct_name) = path.first() else {
            return Some(BorrowReturnShape::UnsupportedForm);
        };
        let Some(def) = self.program.items.iter().find_map(|it| match it {
            Item::StructDef(s) if &s.name == struct_name => Some(s),
            _ => None,
        }) else {
            return Some(BorrowReturnShape::DanglingSource);
        };
        let ref_field_names: HashSet<&str> = def
            .fields
            .iter()
            .filter(|f| matches!(f.ty.kind, TypeKind::Ref(_) | TypeKind::MutRef(_)))
            .map(|f| f.name.as_str())
            .collect();
        // Not a borrowed struct: returning an owned struct value as `ref`
        // would hand back a pointer into a temporary dropped at return.
        if ref_field_names.is_empty() {
            return Some(BorrowReturnShape::DanglingSource);
        }
        let mut worst: Option<BorrowReturnShape> = None;
        for fi in fields {
            if ref_field_names.contains(fi.name.as_str()) {
                worst = combine_borrow_shapes(
                    worst,
                    classify_borrow_return(&fi.value, ref_params, ref_locals),
                );
            }
        }
        worst
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
        match &value.kind {
            ExprKind::Call { callee, args } => {
                let ExprKind::Identifier(fname) = &callee.kind else {
                    return;
                };
                if !self.ref_returning_fn_names().contains(fname) {
                    return;
                }
                for (i, arg) in args.iter().enumerate() {
                    if self.arg_is_borrow_position(callee, i) {
                        if let Some(place) = self.place_expr_root(&arg.value) {
                            self.push_active_borrow(
                                BorrowKind::ImmRef,
                                place,
                                arg.value.span.clone(),
                            );
                        }
                    }
                }
            }
            // Borrow-returning method call (`let n = u.name()`): the result
            // borrows from the receiver. The method's ref-ness is read from
            // the typechecker (the call result type sits at the receiver-span
            // key). Register a borrow on the receiver's root so moving it
            // while `n` is live is rejected.
            ExprKind::MethodCall { object, .. } => {
                let is_ref = matches!(
                    self.typecheck_result
                        .expr_types
                        .get(&SpanKey::from_span(&value.span)),
                    Some(Type::Ref(_) | Type::MutRef(_))
                );
                if is_ref {
                    if let Some(place) = self.place_expr_root(object) {
                        self.push_active_borrow(BorrowKind::ImmRef, place, value.span.clone());
                    }
                }
            }
            _ => {}
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
        // `ref self` returned directly from a method.
        ExprKind::SelfValue => {
            if ref_params.contains("self") {
                None
            } else {
                Some(BorrowReturnShape::DanglingSource)
            }
        }
        ExprKind::FieldAccess { object, .. } => match &object.kind {
            ExprKind::Identifier(b) if ref_params.contains(b) || ref_locals.contains(b) => None,
            // Field through a `ref self` receiver (`self.name`).
            ExprKind::SelfValue if ref_params.contains("self") => None,
            ExprKind::Identifier(_) | ExprKind::SelfValue => {
                Some(BorrowReturnShape::DanglingSource)
            }
            // A field reached through a non-identifier (a chained field
            // access, a call, …) — valid per spec but Tier-2/3 codegen.
            _ => Some(BorrowReturnShape::UnsupportedForm),
        },
        // Conditional borrow return (`longer`-style Tier 2): every branch
        // must itself be a borrowable shape. A value `if` needs an `else`.
        ExprKind::If {
            then_block,
            else_branch,
            ..
        } => {
            let Some(else_e) = else_branch.as_deref() else {
                return Some(BorrowReturnShape::UnsupportedForm);
            };
            combine_borrow_shapes(
                classify_borrow_return_block(then_block, ref_params, ref_locals),
                classify_borrow_return(else_e, ref_params, ref_locals),
            )
        }
        ExprKind::Block(b) => classify_borrow_return_block(b, ref_params, ref_locals),
        // Conditional borrow return via `match` (sibling of the `if` arm):
        // every arm body must itself be a borrowable shape, combined across
        // arms. Bounded — in lockstep with codegen's
        // `compile_ref_return_ptr` `Match` arm — to guard-free arms whose
        // patterns are scalar literals (`Integer`/`Char`/`Bool`) or `_`.
        // Destructuring patterns, guards, and non-scalar scrutinees are
        // tracked follow-ons; they report `UnsupportedForm` (clean error).
        ExprKind::Match { arms, .. } => {
            if arms.is_empty() || !arms.iter().all(match_arm_borrowable_shape) {
                return Some(BorrowReturnShape::UnsupportedForm);
            }
            arms.iter()
                .map(|a| classify_borrow_return(&a.body, ref_params, ref_locals))
                .reduce(combine_borrow_shapes)
                .unwrap_or(Some(BorrowReturnShape::UnsupportedForm))
        }
        // Literals and temporaries are unambiguously dangling; the rest
        // (`Call`/`MethodCall`/…) are valid-but-unsupported.
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

/// Classify a block's tail as a borrow source. Matches codegen's Tier-2
/// capability: statement-free blocks only (a block with preceding
/// statements is reported as not-yet-supported, never miscompiled).
fn classify_borrow_return_block(
    b: &Block,
    ref_params: &HashSet<String>,
    ref_locals: &HashSet<String>,
) -> Option<BorrowReturnShape> {
    if !b.stmts.is_empty() {
        return Some(BorrowReturnShape::UnsupportedForm);
    }
    match &b.final_expr {
        Some(e) => classify_borrow_return(e, ref_params, ref_locals),
        None => Some(BorrowReturnShape::UnsupportedForm),
    }
}

/// A `match` arm is in-scope for borrow-return classification only when it
/// has no guard and its pattern is a scalar literal (`Integer`/`Char`/`Bool`)
/// or `_`. Kept identical to codegen's
/// `expr_ops.rs::ref_return_match_arm_ok` so the source-pinning check and the
/// lowering accept the same set (lockstep — see that fn's note).
fn match_arm_borrowable_shape(arm: &MatchArm) -> bool {
    arm.guard.is_none()
        && matches!(
            arm.pattern.kind,
            PatternKind::Wildcard
                | PatternKind::Literal(
                    LiteralPattern::Integer(..)
                        | LiteralPattern::Char(..)
                        | LiteralPattern::Bool(..)
                )
        )
}

/// Merge two branch classifications. `None` is OK; a genuinely dangling
/// branch dominates (it's a real source-pinning error), otherwise any
/// not-yet-supported branch makes the whole form unsupported.
fn combine_borrow_shapes(
    a: Option<BorrowReturnShape>,
    b: Option<BorrowReturnShape>,
) -> Option<BorrowReturnShape> {
    match (a, b) {
        (None, None) => None,
        (Some(BorrowReturnShape::DanglingSource), _)
        | (_, Some(BorrowReturnShape::DanglingSource)) => Some(BorrowReturnShape::DanglingSource),
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
