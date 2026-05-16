//! Block + statement ownership walking.
//!
//! Houses `check_block` (drives the per-stmt walk + per-stmt
//! borrow-scope cleanup) and `check_stmt` (the per-statement
//! dispatch on `StmtKind` — `Let` / `Defer` / `ErrDefer` /
//! `Expr` / `Return` / `Break` / `Continue` / compound-assign /
//! regular assignment / index assignment, with the slice-borrow
//! source recording and `let pat = rhs` binding state setup).
//!
//! Lives in a sibling `impl<'a> super::OwnershipChecker<'a>` block.

use std::collections::HashMap;

use crate::ast::*;
use crate::resolver::SpanKey;
use crate::typechecker::Type;

use super::{OwnershipError, OwnershipErrorKind, ParamUsage, ValueState};

impl<'a> super::OwnershipChecker<'a> {
    // ── Block / Statement / Expression Walking ──────────────────

    pub(crate) fn check_block(
        &mut self,
        block: &Block,
        states: &mut HashMap<String, ValueState>,
        param_types: &HashMap<String, Type>,
        param_usage: &mut HashMap<String, ParamUsage>,
    ) {
        // Slice 2 — bracket the block walk with scope-depth tracking. On
        // exit, drain any active borrows whose `scope_depth` is at or
        // beyond this block's depth.
        self.current_scope_depth += 1;
        let entered_depth = self.current_scope_depth;
        for stmt in &block.stmts {
            self.check_stmt(stmt, states, param_types, param_usage);
        }
        if let Some(ref expr) = block.final_expr {
            self.check_expr_consuming(expr, states, param_types, param_usage);
        }
        self.drain_borrows_at_depth(entered_depth);
        self.current_scope_depth = entered_depth - 1;
    }

    pub(crate) fn check_stmt(
        &mut self,
        stmt: &Stmt,
        states: &mut HashMap<String, ValueState>,
        param_types: &HashMap<String, Type>,
        param_usage: &mut HashMap<String, ParamUsage>,
    ) {
        match &stmt.kind {
            StmtKind::Let { pattern, value, .. } => {
                // If the RHS is a closure, detect once-callability before
                // processing so we can check which outer bindings it consumed.
                // Value is consumed by the let binding
                self.check_expr_consuming(value, states, param_types, param_usage);

                // Define bindings as Live
                self.define_pattern_states(pattern, states);

                // Record the binding's type from the RHS span. The RHS's
                // span is unaliased (unlike LHS chains), so this is the
                // reliable source of binding types for later consume sites
                // that walk through chained accesses (`c.inner.unwrap()`).
                if let Some(rhs_ty) = self
                    .typecheck_result
                    .expr_types
                    .get(&SpanKey::from_span(&value.span))
                {
                    for name in pattern.binding_names() {
                        self.binding_types.insert(name.clone(), rhs_ty.clone());
                    }
                }

                // Slice 1: escape-from-temp detection. When the RHS is a
                // direct slice creation (`.as_slice()` / `.as_slice_mut()` /
                // range-indexing) whose source has no rooted attribution
                // (the receiver is a function call result, composite
                // literal, etc.), the slice's storage is dropped at end of
                // statement — binding it to a name that outlives the
                // statement points at freed memory. In-statement uses
                // (`make_vec().as_slice().len()`) are not let-RHS so they
                // accept. Future expansions (return-of-temp-slice, escape
                // through call-arg-into-borrow) ride on Slice 2's conflict
                // detector.
                if let Some((source, _)) = Self::slice_creation_source(value) {
                    if self.place_expr_root(source).is_none() {
                        self.errors.push(OwnershipError {
                            message: "slice from temporary value escapes the enclosing statement"
                                .to_string(),
                            span: value.span.clone(),
                            kind: OwnershipErrorKind::SliceFromTemporaryEscapes,
                            suggestion: Some(
                                "bind the receiver to a local first, then take a slice into it"
                                    .to_string(),
                            ),
                            replacement: None,
                            consume_span: None,
                        });
                    }
                }

                // Slice 1: chain-through population for slice-of-slice. If
                // the RHS produced a `slice_borrow_sources` entry (any of
                // the four creation sites fired), propagate it to each
                // binding name introduced by the pattern. A later slice
                // creation whose source is the binding name walks through
                // this map in `place_expr_root` so the recorded root is
                // the original storage (`v`), not the intermediate slice.
                if let Some(entry) = self
                    .slice_borrow_sources
                    .get(&SpanKey::from_span(&value.span))
                    .cloned()
                {
                    for name in pattern.binding_names() {
                        self.slice_binding_sources
                            .insert(name.clone(), entry.clone());
                        // Slice 2 — record the scope at which this slice
                        // binding lives, keyed by the source's root. Used
                        // by drop-of-borrowed detection at the source's
                        // scope-exit drain.
                        self.slice_binding_scope_depth
                            .insert(entry.0.root.clone(), self.current_scope_depth);
                        let _ = name;
                    }
                }

                // Slice 2 — record this binding's scope depth so the
                // drop-of-borrowed trigger can detect "source going out
                // of scope while a slice into it is still bound" cases.
                for name in pattern.binding_names() {
                    self.binding_scope_depth
                        .insert(name, self.current_scope_depth);
                }
            }
            StmtKind::LetUninit {
                is_mut,
                name,
                name_span,
                ..
            } => {
                states.insert(
                    name.clone(),
                    ValueState::Uninit {
                        let_span: stmt.span.clone(),
                        is_mut: *is_mut,
                    },
                );
                // Pull the declared type from the typechecker's expr_types
                // map (recorded at the binding's name span). Lets later
                // consume sites classify Copy-vs-non-Copy without a real RHS
                // span to look up.
                if let Some(t) = self
                    .typecheck_result
                    .expr_types
                    .get(&SpanKey::from_span(name_span))
                {
                    self.binding_types.insert(name.clone(), t.clone());
                }
                // Slice 2 polish (D5) — record scope depth so the
                // shape D drain can match the LHS's "outer" scope
                // against the source's "inner" scope at exit.
                self.binding_scope_depth
                    .insert(name.clone(), self.current_scope_depth);
            }
            StmtKind::LetElse {
                pattern,
                value,
                else_block,
                ..
            } => {
                self.check_expr_consuming(value, states, param_types, param_usage);
                self.define_pattern_states(pattern, states);
                let mut else_states = states.clone();
                self.check_block(else_block, &mut else_states, param_types, param_usage);
            }
            StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
                let mut defer_states = states.clone();
                self.check_block(body, &mut defer_states, param_types, param_usage);
            }
            StmtKind::Assign { target, value } => {
                // Check if target is a variable — reassignment resets state
                if let ExprKind::Identifier(name) = &target.kind {
                    // Process the RHS first so reads of `name` in the RHS see
                    // the pre-assignment state. (e.g. `let x: T; x = f(x);`
                    // — the `x` inside `f(x)` is still Uninit and errors.)
                    self.check_expr_consuming(value, states, param_types, param_usage);
                    let pre = states.get(name).cloned();
                    match pre {
                        // First assignment to a `let mut x: T;` — promote.
                        Some(ValueState::Uninit { is_mut: true, .. }) => {
                            states.insert(name.clone(), ValueState::Live);
                        }
                        // First assignment to a `let x: T;` (non-mut) — this
                        // counts as initialization, not reassignment, so it
                        // succeeds without `mut`. Subsequent assigns will fail.
                        Some(ValueState::Uninit { is_mut: false, .. }) => {
                            states.insert(
                                name.clone(),
                                ValueState::InitOnce {
                                    first_assign: target.span.clone(),
                                },
                            );
                        }
                        // Second-and-beyond assignment to a non-mut LetUninit
                        // binding. Per design.md "first assignment is
                        // initialization, not reassignment" — anything more
                        // requires `let mut`.
                        Some(ValueState::InitOnce { first_assign }) => {
                            self.errors.push(OwnershipError {
                                message: format!(
                                    "cannot reassign `{}` — declared without `mut` (first assignment at line {}:{})",
                                    name, first_assign.line, first_assign.column
                                ),
                                span: target.span.clone(),
                                kind: OwnershipErrorKind::ReassignToImmutable,
                                suggestion: Some(format!(
                                    "change the declaration to `let mut {}: ...;`",
                                    name
                                )),
                                replacement: None,
                                consume_span: None,
                            });
                            // Leave state as InitOnce — further reads still
                            // succeed, further reassigns still fire.
                        }
                        // Live / Moved / not-yet-bound: existing behavior —
                        // reassignment resets to Live.
                        _ => {
                            states.insert(name.clone(), ValueState::Live);
                        }
                    }
                    // Track mutation of parameters
                    if let Some(usage) = param_usage.get_mut(name) {
                        *usage = ParamUsage::Mutated;
                    }
                    // Slice 2 polish (D5) — when the LHS is a slice-typed
                    // binding (typically a `LetUninit` outer binding) and
                    // the RHS produced a slice creation, record the LHS's
                    // scope depth against the source root so block-exit
                    // drain can detect "slice outlives source" cases.
                    // The LHS's scope depth is already captured in
                    // `binding_scope_depth` from the `LetUninit` arm.
                    if let Some(entry) = self
                        .slice_borrow_sources
                        .get(&SpanKey::from_span(&value.span))
                        .cloned()
                    {
                        if let Some(&lhs_depth) = self.binding_scope_depth.get(name) {
                            self.slice_binding_sources
                                .insert(name.clone(), entry.clone());
                            self.slice_binding_scope_depth
                                .insert(entry.0.root.clone(), lhs_depth);
                        }
                    }
                } else {
                    // Field/index assignment — track mutation on the root object
                    if let Some(root) = Self::root_identifier(target) {
                        if let Some(usage) = param_usage.get_mut(&root) {
                            *usage = ParamUsage::Mutated;
                        }
                    }
                    self.check_expr_reading(target, states, param_types, param_usage);
                    self.check_expr_consuming(value, states, param_types, param_usage);
                }
            }
            StmtKind::CompoundAssign { target, value, .. } => {
                // Compound assignment (+=, -=, etc.) mutates the target
                if let ExprKind::Identifier(name) = &target.kind {
                    if let Some(usage) = param_usage.get_mut(name) {
                        *usage = ParamUsage::Mutated;
                    }
                } else if let Some(root) = Self::root_identifier(target) {
                    if let Some(usage) = param_usage.get_mut(&root) {
                        *usage = ParamUsage::Mutated;
                    }
                }
                self.check_expr_reading(target, states, param_types, param_usage);
                self.check_expr_consuming(value, states, param_types, param_usage);
            }
            StmtKind::Expr(expr) => {
                self.check_expr_reading(expr, states, param_types, param_usage);
            }
        }
    }
}
