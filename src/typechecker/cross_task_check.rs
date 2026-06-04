//! Cross-task-safe boundary-site enforcement (Phase 6 line 170 slice 3a).
//!
//! Wires the structural cross-task-safe walker from
//! [`crate::cross_task_safe`] into the typechecker so a closure passed
//! to `spawn(...)` or `TaskGroup.spawn(...)` is rejected when any of its
//! captures' types reach a not-cross-task-safe leaf (`Rc[T]`,
//! `shared struct`, `shared enum`, `OnceCell[T]`, raw pointer).
//!
//! Scope of this slice — only the two boundary sites introduced by the
//! `spawn` / `TaskGroup` work (line 218):
//!
//! - `spawn(closure)` — bare free-fn call site in `expr_call.rs::infer_call`.
//! - `tg.spawn(closure)` — method dispatch on `TaskGroup` in
//!   `expr_method_call.rs::infer_method_call`.
//!
//! Remaining boundary sites carry their own follow-on entries (the
//! `par {}` block, `Channel.send` across a par/spawn boundary, and
//! `with_provider[R](provider, closure)`); see the line-170 entry's
//! slice 3b / 3c notes in `docs/implementation_checklist/phase-6-runtime.md`.
//!
//! ## Walker contract at this layer
//!
//! Each call site:
//!
//! 1. Snapshots the *outer* local-scope binding-name → Type map BEFORE
//!    the closure's params get pushed onto the local scope (the closure
//!    body hasn't been typechecked yet at this point).
//! 2. Walks the closure body collecting identifier references whose name:
//!    is not a closure param, is not shadowed by a body-local `let` /
//!    `match` / `for` / `if let` binding, AND resolves to an entry in
//!    the outer-scope snapshot.
//! 3. For each captured binding, runs `is_cross_task_safe_with` against
//!    the in-progress `env.structs` / `env.enums` index.
//! 4. Emits one `CrossTaskUnsafeCapture` diagnostic per unsafe capture,
//!    anchored at the *capture's first reference* inside the closure
//!    body so the user sees where the unsafe value leaks across the
//!    task boundary.
//!
//! ## Why mid-typecheck, not post-typecheck
//!
//! The `provider_escape` precedent at `src/provider_escape.rs` runs as
//! a separate phase against the finalized `TypeCheckResult`. That works
//! when the check needs `expr_types` populated (instance-method-call
//! resolution etc.). The cross-task-safe boundary check only needs the
//! captured *binding's* declared type, which `flatten_local_scope_snapshot`
//! already exposes mid-typecheck, plus the struct/enum index in `env`.
//! Running inline at the call site keeps the diagnostic flow alongside
//! the call's regular typecheck without a second AST walk.

use crate::ast::*;
use crate::cross_task_safe::{is_cross_task_safe_with, CrossTaskUnsafeFixIt, CrossTaskUnsafePath};
use crate::token::Span;
use std::collections::{HashMap, HashSet};

use super::types::{type_display, Type};
use super::TypeErrorKind;

impl<'a> super::TypeChecker<'a> {
    /// Boundary-site cross-task-safe check for `spawn(closure)` and
    /// `tg.spawn(closure)` call sites.
    ///
    /// `closure_expr` is the first argument expression of the call — when
    /// it isn't a closure literal, no check fires (the call still gets
    /// regular typechecking; an `OnceFn` / `Fn` passed by name would
    /// route through future slices once those shapes have a recoverable
    /// capture set at the type system layer).
    ///
    /// `call_span` is the span of the surrounding call, used as a fallback
    /// anchor when the captured-name reference's own span isn't available.
    ///
    /// `site_label` distinguishes "spawn" vs "TaskGroup.spawn" in the
    /// emitted diagnostic.
    pub(super) fn check_cross_task_safe_captures(
        &mut self,
        closure_expr: &Expr,
        call_span: &Span,
        site_label: &str,
    ) {
        let ExprKind::Closure { params, body, .. } = &closure_expr.kind else {
            return;
        };

        let outer_snapshot = self.flatten_local_scope_snapshot();

        let mut closure_param_names: HashSet<String> = HashSet::new();
        for p in params {
            for n in p.pattern.binding_names() {
                closure_param_names.insert(n);
            }
        }

        let mut captures: Vec<(String, Span)> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let mut shadow_stack: Vec<HashSet<String>> = vec![closure_param_names];
        collect_captures_expr(
            body,
            &outer_snapshot,
            &mut shadow_stack,
            &mut captures,
            &mut seen,
        );

        for (name, anchor_span) in captures {
            let Some(ty) = outer_snapshot.get(&name) else {
                continue;
            };
            if let Err(path) = is_cross_task_safe_with(ty, &self.env.structs, &self.env.enums) {
                self.emit_cross_task_unsafe(
                    name.as_str(),
                    ty,
                    &path,
                    &anchor_span,
                    call_span,
                    site_label,
                );
            }
        }
    }

    /// Boundary-site cross-task-safe check for a `par { ... }` block
    /// (Phase 6 line 170 slice 3b).
    ///
    /// Unlike `spawn(closure)` / `tg.spawn(closure)`, a `par {}` block has
    /// no closure wrapper — each top-level statement becomes a parallel
    /// branch that reads directly from the enclosing scope. So every
    /// outer-scope binding the block references is a cross-boundary
    /// capture. We snapshot the enclosing local scope *before*
    /// `infer_block` pushes the par block's own scope, then run the same
    /// capture walker (`collect_captures_block`) and `is_cross_task_safe_with`
    /// predicate the spawn sites use. Bindings introduced *inside* the par
    /// block (`let` in a branch) are shadow-tracked by the walker, so they
    /// are correctly excluded — only values flowing in from the outside
    /// cross the boundary.
    ///
    /// ## Division of labor with the ownership phase
    ///
    /// `shared struct` / `shared enum` captures get the **sole-ownership
    /// carve-out** (design.md § Rc vs Arc — Two-Phase Algorithm, the
    /// "Rule for `shared struct`"): a value moved into *exactly one*
    /// branch is safe; only when it is reachable from two-or-more branches
    /// is it an error. That branch-precise determination is the ownership
    /// phase's `E_CONCURRENT_SHARED_STRUCT` / `E_CONCURRENT_PLAIN_STRUCT`
    /// pass (`src/ownership/concurrent_shared.rs`), which counts per-branch
    /// uses. This type-only pass cannot see branch aliasing, so it would
    /// falsely reject the sole-ownership case — therefore it **defers** any
    /// unsafe path rooted at a shared struct/enum leaf (`SharedToPar`
    /// fix-it) to that pass.
    ///
    /// The remaining cross-task-unsafe leaves — `Rc[T]`, `OnceCell[T]`,
    /// raw pointers — have **no** sole-ownership carve-out at a par-block
    /// boundary (design.md line 1407 for `OnceCell`, line 8197 for the
    /// `Rc` / par-region-escape pass), so the categorical type-only
    /// rejection here is correct for them. This is the par-block half of
    /// the "check fires at five sites" commitment in design.md § Structured
    /// Concurrency Lifetime Guarantees.
    pub(super) fn check_cross_task_safe_par_block(&mut self, block: &Block, par_span: &Span) {
        let outer_snapshot = self.flatten_local_scope_snapshot();

        let mut captures: Vec<(String, Span)> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let mut shadow_stack: Vec<HashSet<String>> = vec![HashSet::new()];
        collect_captures_block(
            block,
            &outer_snapshot,
            &mut shadow_stack,
            &mut captures,
            &mut seen,
        );

        for (name, anchor_span) in captures {
            let Some(ty) = outer_snapshot.get(&name) else {
                continue;
            };
            if let Err(path) = is_cross_task_safe_with(ty, &self.env.structs, &self.env.enums) {
                // Sole-ownership carve-out: shared struct/enum leaves are
                // the branch-precise ownership phase's territory (see the
                // doc comment above). Deferring here avoids both double-
                // reporting and a false rejection of the sole-branch case.
                if path.fix_it == CrossTaskUnsafeFixIt::SharedToPar {
                    continue;
                }
                self.emit_cross_task_unsafe(
                    name.as_str(),
                    ty,
                    &path,
                    &anchor_span,
                    par_span,
                    "par {}",
                );
            }
        }
    }

    fn emit_cross_task_unsafe(
        &mut self,
        capture_name: &str,
        capture_ty: &Type,
        path: &CrossTaskUnsafePath,
        anchor_span: &Span,
        call_span: &Span,
        site_label: &str,
    ) {
        let path_suffix = if path.path.is_empty() {
            String::new()
        } else {
            format!(" at {}", path.path.join(" -> "))
        };
        let help = path.fix_it.help_text(&path.unsafe_leaf);
        let msg = format!(
            "error[E_NOT_CROSS_TASK]: capture of `{name}` (type `{ty}`) cannot cross a {site} \
             task boundary -- type `{ty}` reaches `{unsafe_leaf}`{path_suffix}; help: {help}",
            name = capture_name,
            ty = type_display(capture_ty),
            site = site_label,
            unsafe_leaf = path.unsafe_leaf,
            path_suffix = path_suffix,
            help = help,
        );
        let span = if anchor_span.line == 0 && anchor_span.column == 0 {
            call_span.clone()
        } else {
            anchor_span.clone()
        };
        self.type_error(msg, span, TypeErrorKind::CrossTaskUnsafeCapture);
    }

    /// Emit `E_NOT_CROSS_TASK` for a cross-task boundary site that
    /// transfers a *value* rather than capturing a named binding —
    /// `Channel.send(value)` and `with_provider[R](provider, …)` (Phase 6
    /// line 170 slice 3c). Unlike the spawn/par capture sites there is no
    /// captured binding name to anchor on, so `descr` carries the leading
    /// clause naming the site (e.g. `"value sent across a channel"` or
    /// `"provider for resource \`Clock\`"`). Shares the type-path /
    /// fix-it rendering with [`Self::emit_cross_task_unsafe`].
    ///
    /// Neither site has a sole-ownership carve-out: a channel transfers
    /// its value to an unknown receiving task (possibly many sends), and a
    /// provider is shared with a closure body that may run across spawned
    /// tasks. So this rejects the *full* cross-task-unsafe set including
    /// `shared struct` / `shared enum` — it does NOT apply the
    /// `SharedToPar` deferral that the par-block check uses (design.md
    /// line 1407 for `Channel`, line 7213 for `with_provider`).
    pub(super) fn emit_cross_task_unsafe_value(
        &mut self,
        descr: &str,
        value_ty: &Type,
        path: &CrossTaskUnsafePath,
        span: &Span,
    ) {
        let path_suffix = if path.path.is_empty() {
            String::new()
        } else {
            format!(" at {}", path.path.join(" -> "))
        };
        let help = path.fix_it.help_text(&path.unsafe_leaf);
        let msg = format!(
            "error[E_NOT_CROSS_TASK]: {descr} (type `{ty}`) cannot cross a task boundary -- \
             type `{ty}` reaches `{unsafe_leaf}`{path_suffix}; help: {help}",
            descr = descr,
            ty = type_display(value_ty),
            unsafe_leaf = path.unsafe_leaf,
            path_suffix = path_suffix,
            help = help,
        );
        self.type_error(msg, span.clone(), TypeErrorKind::CrossTaskUnsafeCapture);
    }
}

// ── Capture walker ──────────────────────────────────────────────
//
// Free-standing helpers (do NOT take `&TypeChecker`) so the walker
// stays a pure AST function. Same role as
// `src/codegen/closures.rs::refs_in_expr` but at the typecheck layer
// and returning a (name, first-reference-span) pair so the diagnostic
// anchors at the actual leak site inside the body.

fn collect_captures_expr(
    expr: &Expr,
    outer: &HashMap<String, Type>,
    shadows: &mut Vec<HashSet<String>>,
    out: &mut Vec<(String, Span)>,
    seen: &mut HashSet<String>,
) {
    match &expr.kind {
        ExprKind::Identifier(name)
            if !name_is_shadowed(name, shadows)
                && outer.contains_key(name)
                && !seen.contains(name) =>
        {
            seen.insert(name.clone());
            out.push((name.clone(), expr.span.clone()));
        }
        ExprKind::Identifier(_) => {}
        ExprKind::SelfValue
            if !name_is_shadowed("self", shadows)
                && outer.contains_key("self")
                && !seen.contains("self") =>
        {
            seen.insert("self".to_string());
            out.push(("self".to_string(), expr.span.clone()));
        }
        ExprKind::SelfValue => {}
        ExprKind::Binary { left, right, .. } => {
            collect_captures_expr(left, outer, shadows, out, seen);
            collect_captures_expr(right, outer, shadows, out, seen);
        }
        ExprKind::Unary { operand, .. } => {
            collect_captures_expr(operand, outer, shadows, out, seen);
        }
        ExprKind::Question(inner) => {
            collect_captures_expr(inner, outer, shadows, out, seen);
        }
        ExprKind::OptionalChain { object, args, .. } => {
            collect_captures_expr(object, outer, shadows, out, seen);
            if let Some(args) = args {
                for a in args {
                    collect_captures_expr(&a.value, outer, shadows, out, seen);
                }
            }
        }
        ExprKind::NilCoalesce { left, right } => {
            collect_captures_expr(left, outer, shadows, out, seen);
            collect_captures_expr(right, outer, shadows, out, seen);
        }
        ExprKind::Call { callee, args } => {
            collect_captures_expr(callee, outer, shadows, out, seen);
            for a in args {
                collect_captures_expr(&a.value, outer, shadows, out, seen);
            }
        }
        ExprKind::MethodCall { object, args, .. } => {
            collect_captures_expr(object, outer, shadows, out, seen);
            for a in args {
                collect_captures_expr(&a.value, outer, shadows, out, seen);
            }
        }
        ExprKind::FieldAccess { object, .. } => {
            collect_captures_expr(object, outer, shadows, out, seen);
        }
        ExprKind::Index { object, index } => {
            collect_captures_expr(object, outer, shadows, out, seen);
            collect_captures_expr(index, outer, shadows, out, seen);
        }
        ExprKind::TupleIndex { object, .. } => {
            collect_captures_expr(object, outer, shadows, out, seen);
        }
        ExprKind::Cast { expr: inner, .. } => {
            collect_captures_expr(inner, outer, shadows, out, seen);
        }
        ExprKind::Tuple(elems) | ExprKind::ArrayLiteral(elems) => {
            for e in elems {
                collect_captures_expr(e, outer, shadows, out, seen);
            }
        }
        ExprKind::PrefixCollectionLiteral { items, .. } => {
            for e in items {
                collect_captures_expr(e, outer, shadows, out, seen);
            }
        }
        ExprKind::RepeatLiteral { value, count, .. } => {
            collect_captures_expr(value, outer, shadows, out, seen);
            collect_captures_expr(count, outer, shadows, out, seen);
        }
        ExprKind::MapLiteral(entries) => {
            for (k, v) in entries {
                collect_captures_expr(k, outer, shadows, out, seen);
                collect_captures_expr(v, outer, shadows, out, seen);
            }
        }
        ExprKind::StructLiteral { fields, spread, .. } => {
            for f in fields {
                collect_captures_expr(&f.value, outer, shadows, out, seen);
            }
            if let Some(s) = spread {
                collect_captures_expr(s, outer, shadows, out, seen);
            }
        }
        ExprKind::Pipe { left, right } => {
            collect_captures_expr(left, outer, shadows, out, seen);
            collect_captures_expr(right, outer, shadows, out, seen);
        }
        ExprKind::If {
            condition,
            then_block,
            else_branch,
        } => {
            collect_captures_expr(condition, outer, shadows, out, seen);
            collect_captures_block(then_block, outer, shadows, out, seen);
            if let Some(eb) = else_branch {
                collect_captures_expr(eb, outer, shadows, out, seen);
            }
        }
        ExprKind::IfLet {
            pattern,
            value,
            then_block,
            else_branch,
        } => {
            collect_captures_expr(value, outer, shadows, out, seen);
            shadows.push(pattern.binding_names().into_iter().collect());
            collect_captures_block(then_block, outer, shadows, out, seen);
            shadows.pop();
            if let Some(eb) = else_branch {
                collect_captures_expr(eb, outer, shadows, out, seen);
            }
        }
        ExprKind::While {
            condition, body, ..
        } => {
            collect_captures_expr(condition, outer, shadows, out, seen);
            collect_captures_block(body, outer, shadows, out, seen);
        }
        ExprKind::WhileLet {
            pattern,
            value,
            body,
            ..
        } => {
            collect_captures_expr(value, outer, shadows, out, seen);
            shadows.push(pattern.binding_names().into_iter().collect());
            collect_captures_block(body, outer, shadows, out, seen);
            shadows.pop();
        }
        ExprKind::Loop { body, .. } => {
            collect_captures_block(body, outer, shadows, out, seen);
        }
        ExprKind::For {
            pattern,
            iterable,
            body,
            ..
        } => {
            collect_captures_expr(iterable, outer, shadows, out, seen);
            shadows.push(pattern.binding_names().into_iter().collect());
            collect_captures_block(body, outer, shadows, out, seen);
            shadows.pop();
        }
        ExprKind::Match { scrutinee, arms } => {
            collect_captures_expr(scrutinee, outer, shadows, out, seen);
            for arm in arms {
                shadows.push(arm.pattern.binding_names().into_iter().collect());
                if let Some(guard) = &arm.guard {
                    collect_captures_expr(guard, outer, shadows, out, seen);
                }
                collect_captures_expr(&arm.body, outer, shadows, out, seen);
                shadows.pop();
            }
        }
        ExprKind::Block(block)
        | ExprKind::Par(block)
        | ExprKind::Seq(block)
        | ExprKind::Try(block)
        | ExprKind::Unsafe(block) => {
            collect_captures_block(block, outer, shadows, out, seen);
        }
        ExprKind::LabeledBlock { body, .. } => {
            collect_captures_block(body, outer, shadows, out, seen);
        }
        ExprKind::Lock { body, .. } => {
            collect_captures_block(body, outer, shadows, out, seen);
        }
        ExprKind::Providers { bindings, body } => {
            for b in bindings {
                collect_captures_expr(&b.value, outer, shadows, out, seen);
            }
            collect_captures_block(body, outer, shadows, out, seen);
        }
        ExprKind::Closure { params, body, .. } => {
            // Nested closure — its own params shadow inside its body.
            // Capture set still flows out to the enclosing closure's
            // boundary check (the enclosing closure transitively
            // captures whatever the inner closure pulls from the
            // outer scope).
            let mut inner_params: HashSet<String> = HashSet::new();
            for p in params {
                for n in p.pattern.binding_names() {
                    inner_params.insert(n);
                }
            }
            shadows.push(inner_params);
            collect_captures_expr(body, outer, shadows, out, seen);
            shadows.pop();
        }
        ExprKind::Return(Some(e)) => {
            collect_captures_expr(e, outer, shadows, out, seen);
        }
        ExprKind::Return(None) => {}
        ExprKind::Break { value: Some(e), .. } => {
            collect_captures_expr(e, outer, shadows, out, seen);
        }
        ExprKind::Break { value: None, .. } => {}
        ExprKind::Range { start, end, .. } => {
            if let Some(s) = start {
                collect_captures_expr(s, outer, shadows, out, seen);
            }
            if let Some(e) = end {
                collect_captures_expr(e, outer, shadows, out, seen);
            }
        }
        ExprKind::InterpolatedStringLit(parts) => {
            for p in parts {
                if let ParsedInterpolationPart::Expr(e) = p {
                    collect_captures_expr(e, outer, shadows, out, seen);
                }
            }
        }
        // Path callees / type-path expressions are not capture references
        // — `Foo.bar()` resolves through the type environment, not the
        // local scope.
        //
        // Leaf shapes (literals, `Continue`, `PipePlaceholder`,
        // `SelfType`, `Error`, `OffsetOf`) have no nested captures.
        _ => {}
    }
}

fn collect_captures_block(
    block: &Block,
    outer: &HashMap<String, Type>,
    shadows: &mut Vec<HashSet<String>>,
    out: &mut Vec<(String, Span)>,
    seen: &mut HashSet<String>,
) {
    shadows.push(HashSet::new());
    for stmt in &block.stmts {
        match &stmt.kind {
            StmtKind::Let { pattern, value, .. } => {
                collect_captures_expr(value, outer, shadows, out, seen);
                if let Some(top) = shadows.last_mut() {
                    for n in pattern.binding_names() {
                        top.insert(n);
                    }
                }
            }
            StmtKind::LetUninit { name, .. } => {
                if let Some(top) = shadows.last_mut() {
                    top.insert(name.clone());
                }
            }
            StmtKind::LetElse {
                pattern,
                value,
                else_block,
                ..
            } => {
                collect_captures_expr(value, outer, shadows, out, seen);
                collect_captures_block(else_block, outer, shadows, out, seen);
                if let Some(top) = shadows.last_mut() {
                    for n in pattern.binding_names() {
                        top.insert(n);
                    }
                }
            }
            StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
                collect_captures_block(body, outer, shadows, out, seen);
            }
            StmtKind::Assign { target, value } => {
                collect_captures_expr(target, outer, shadows, out, seen);
                collect_captures_expr(value, outer, shadows, out, seen);
            }
            StmtKind::CompoundAssign { target, value, .. } => {
                collect_captures_expr(target, outer, shadows, out, seen);
                collect_captures_expr(value, outer, shadows, out, seen);
            }
            StmtKind::Expr(e) => {
                collect_captures_expr(e, outer, shadows, out, seen);
            }
        }
    }
    if let Some(final_expr) = &block.final_expr {
        collect_captures_expr(final_expr, outer, shadows, out, seen);
    }
    shadows.pop();
}

fn name_is_shadowed(name: &str, shadows: &[HashSet<String>]) -> bool {
    shadows.iter().any(|s| s.contains(name))
}
