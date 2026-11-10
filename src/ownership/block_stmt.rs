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

    /// Record each parameter a statement first wrote or moved, for the
    /// ownership query's derivation. A nested block records its own
    /// statements first, so the line is the innermost statement's.
    fn note_usage_changes(
        &mut self,
        before: &HashMap<String, ParamUsage>,
        after: &HashMap<String, ParamUsage>,
        span: crate::token::Span,
    ) {
        for (name, usage) in after {
            if !matches!(usage, ParamUsage::Mutated | ParamUsage::Consumed)
                || before.get(name) == Some(usage)
                || self
                    .usage_sites
                    .iter()
                    .any(|(n, u, _)| n == name && u == usage)
            {
                continue;
            }
            self.usage_sites.push((name.clone(), usage.clone(), span));
        }
    }

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
        // A `let` inside the block ends with it: an inner `let x = 3;` must
        // not leave an outer `let mut x` unwritable after the block.
        let lets_at_entry = self.immutable_lets.clone();
        // The slice bindings this block's `let`s made, each with the index
        // of its `let`.
        let mut holders: Vec<(String, usize)> = Vec::new();
        for (i, stmt) in block.stmts.iter().enumerate() {
            self.end_dead_slice_holders(block, i, &mut holders);
            let before = param_usage.clone();
            self.check_stmt(stmt, states, param_types, param_usage);
            self.note_usage_changes(&before, param_usage, stmt.span);
            if let StmtKind::Let { pattern, .. } = &stmt.kind {
                for name in pattern.binding_names() {
                    holders.retain(|(h, _)| *h != name);
                    if self.slice_binding_keys.contains_key(&name) {
                        holders.push((name, i));
                    }
                }
            }
        }
        if let Some(ref expr) = block.final_expr {
            let before = param_usage.clone();
            self.check_expr_consuming(expr, states, param_types, param_usage);
            self.note_usage_changes(&before, param_usage, expr.span);
        }
        // B-2026-09-28-61 — a `let ... else` binding lives to the end of the
        // enclosing block, so whether it was moved out of a `shared enum` is
        // known only here.
        for stmt in &block.stmts {
            if let StmtKind::LetElse { pattern, value, .. } = &stmt.kind {
                self.reject_shared_enum_drop_payload_move(value, pattern, states, None);
            }
        }
        self.drain_borrows_at_depth(entered_depth);
        self.current_scope_depth = entered_depth - 1;
        self.immutable_lets = lets_at_entry;
    }

    /// Ends the slice borrow a binding of this block holds once nothing in
    /// the rest of the block names the binding (core-semantics.md §5: a
    /// borrow is live from its creation to its last use), so
    /// `let tail = v[1..]; let k = tail.len(); v.swap(0, k);` is accepted.
    /// Only a binding every use of which so far was a `let` of a value that
    /// holds no borrow (`tail.len()`, `tail[k]`) is ended: a use that could
    /// have handed the view on (`w.push(tail.first())`, `let it =
    /// tail.iter()`) keeps the borrow to the end of the scope, as before.
    fn end_dead_slice_holders(
        &mut self,
        block: &Block,
        at: usize,
        holders: &mut Vec<(String, usize)>,
    ) {
        let names = |s: &Stmt, name: &str| {
            let mut hit = false;
            crate::rc_elide::walk_stmt_children_pub(s, &mut |e| {
                hit = hit || crate::deque_head::expr_mentions_name_deep(e, name);
            });
            hit
        };
        let mut ended = Vec::new();
        holders.retain(|(name, made)| {
            let later = block.stmts[at..].iter().any(|s| names(s, name))
                || block
                    .final_expr
                    .as_deref()
                    .is_some_and(|e| crate::deque_head::expr_mentions_name_deep(e, name));
            if later {
                return true;
            }
            let owned_uses = block.stmts[made + 1..at].iter().all(|s| {
                !names(s, name)
                    || matches!(&s.kind, StmtKind::Let { value, .. }
                        if self
                            .typecheck_result
                            .expr_type(value)
                            .is_some_and(crate::ownership::type_is_deeply_owned))
            });
            if owned_uses {
                ended.push(name.clone());
            }
            false
        });
        for name in ended {
            let Some(key) = self.slice_binding_keys.remove(&name) else {
                continue;
            };
            let Some((place, _)) = self.slice_borrow_sources.get(&key) else {
                continue;
            };
            let root = place.root.clone();
            if let Some(borrows) = self.active_borrows.get_mut(&root) {
                borrows.retain(|b| !(b.kind.is_slice() && SpanKey::from_span(&b.span) == key));
                if borrows.is_empty() {
                    self.active_borrows.remove(&root);
                }
            }
        }
    }

    pub(crate) fn check_stmt(
        &mut self,
        stmt: &Stmt,
        states: &mut HashMap<String, ValueState>,
        param_types: &HashMap<String, Type>,
        param_usage: &mut HashMap<String, ParamUsage>,
    ) {
        match &stmt.kind {
            StmtKind::MultiAssign { .. } => unreachable!(
                "StmtKind::MultiAssign is removed by the desugar pass before reaching this phase"
            ),
            StmtKind::Let {
                is_mut,
                pattern,
                ty,
                value,
            } => {
                self.report_mut_ref_pattern_write(pattern, value);
                // `let p: mut ref P = w.p;` is check's error with a fix that
                // writes `mut ref` (core-semantics §5.1, B-2026-10-15-14).
                // Read it as that borrow here, so the fix is the only one
                // offered and no move or `let mut` cascade follows it.
                let annotated_borrow = crate::ownership::core_rules()
                    && crate::ownership::is_place_projection(value)
                    && ty
                        .as_ref()
                        .and_then(|t| match t.kind {
                            crate::ast::TypeKind::Ref(_) => Some(false),
                            crate::ast::TypeKind::MutRef(_) => Some(true),
                            _ => None,
                        })
                        .is_some();
                let annotated_mut = matches!(
                    ty.as_ref().map(|t| &t.kind),
                    Some(crate::ast::TypeKind::MutRef(_))
                );
                // If the RHS is a closure, detect once-callability before
                // processing so we can check which outer bindings it consumed.
                // Value is consumed by the let binding — unless the pattern
                // is `ref name @ PATTERN` (design.md § @ Bindings): the
                // whole subtree borrows, so the RHS is read, not moved. So is
                // a pattern that binds only `ref` / `mut ref` names under the
                // v2 core (§4.6).
                if annotated_borrow
                    || matches!(&pattern.kind, PatternKind::AtBinding { by_ref: true, .. })
                    || (crate::ownership::core_rules()
                        && !pattern.binding_names().is_empty()
                        && !self.pattern_binds_anything(pattern))
                {
                    self.check_expr_reading(value, states, param_types, param_usage);
                } else if let Some(parts) = self.core_copied_parts(pattern, value) {
                    // v2 core §4.6: only bindings move, so a struct pattern
                    // that copies or borrows its parts reads just those
                    // parts. `let Q { n, .. } = q` after `q.u` moved out is
                    // fine.
                    for part in &parts {
                        self.check_expr_reading(part, states, param_types, param_usage);
                    }
                } else {
                    // v2 core §4.6: `_` never binds and never moves, so `let _ =
                    // x;` leaves `x` intact. Legacy still drops `x` here, so the
                    // walk below keeps treating it as a move (the build's
                    // defensive copy depends on it); the strict commands drop
                    // any E0500 that cites this site.
                    if matches!(pattern.kind, PatternKind::Wildcard) {
                        self.core_non_move_sources
                            .insert(SpanKey::from_span(&value.span));
                        self.fn_non_move_sources
                            .insert(SpanKey::from_span(&value.span));
                    }
                    self.check_expr_consuming(value, states, param_types, param_usage);
                }

                // Define bindings as Live
                self.define_pattern_states(pattern, states);

                // B-2026-07-27-9 — record immutability so later writes to
                // these names are rejected. Runs AFTER the RHS walk so
                // `let x = x;` (shadowing an outer mutable `x`) records the
                // NEW binding, and after `define_pattern_states` so the two
                // maps agree on which names this `let` introduced.
                self.record_let_mutability(pattern, *is_mut, &stmt.span);

                // Record the binding's type from the RHS span. The RHS's
                // span is unaliased (unlike LHS chains), so this is the
                // reliable source of binding types for later consume sites
                // that walk through chained accesses (`c.inner.unwrap()`).
                if let Some(rhs_ty) = self.typecheck_result.expr_type(value) {
                    let rhs_ty = rhs_ty.clone();
                    let rhs_ty = match (annotated_borrow, annotated_mut, rhs_ty) {
                        (true, _, t @ (Type::Ref(_) | Type::MutRef(_))) => t,
                        (true, true, t) => Type::MutRef(Box::new(t)),
                        (true, false, t) => Type::Ref(Box::new(t)),
                        (false, _, t) => t,
                    };
                    self.record_let_binding_types(pattern, &rhs_ty);
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
                    // A place under `self` is no temporary either
                    // (B-2026-10-14-51): `let s = self.bytes[a..b];`.
                    if self.place_expr_root(source).is_none() && !place_rooted_at_self(source) {
                        self.errors.push(OwnershipError {
                            message: "slice from temporary value escapes the enclosing statement"
                                .to_string(),
                            span: value.span,
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
                let borrow_key = self.slice_borrow_key_for(value);
                // A name this `let` binds holds no earlier slice borrow.
                for name in pattern.binding_names() {
                    self.slice_binding_keys.remove(&name);
                    self.slice_binding_sources.remove(&name);
                }
                if let Some(entry) = self.slice_borrow_sources.get(&borrow_key).cloned() {
                    // B-2026-08-17-16 — the span-keyed hit cannot tell "the
                    // RHS IS the slice" from "the RHS passed through a
                    // slice and copied out of it": the parser aliases a
                    // postfix chain's span to its receiver's, so `let mc =
                    // m.as_slice_mut().to_vec();` carries the same key as
                    // `let s = m.as_slice_mut();`. Discriminate on the
                    // RHS's recorded type (span-keyed too, and the
                    // OUTERMOST expression's record wins — exactly the
                    // value the binding receives): a deeply-owned type
                    // means `.to_vec()`-style copy-out, so the binding is
                    // independent of the source and the creation-site
                    // borrow ends here instead of extending to scope exit.
                    // Anything borrow-carrying (`mut Slice[T]` itself, or
                    // `Option[ref T]` from `.first()`) keeps the
                    // conservative propagation.
                    let copied_out = self
                        .typecheck_result
                        .expr_type(value)
                        .is_some_and(crate::ownership::type_is_deeply_owned);
                    // A destructuring pattern hands each name its own part:
                    // a name whose part is deeply owned (`at: i64` in
                    // `let (at, _) = s.iter().enumerate().next().unwrap()`)
                    // copied it out and holds no borrow (B-2026-11-08-12).
                    let mut owned_parts = Vec::new();
                    if let Some(ty) = self.typecheck_result.expr_type(value) {
                        crate::ownership::deeply_owned_pattern_names(pattern, ty, &mut owned_parts);
                    }
                    let holders: Vec<String> = pattern
                        .binding_names()
                        .into_iter()
                        .filter(|n| !owned_parts.contains(n))
                        .collect();
                    if copied_out || holders.is_empty() {
                        self.end_copied_out_slice_borrow(&borrow_key);
                    } else {
                        for name in holders {
                            self.slice_binding_keys.insert(name.clone(), borrow_key);
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
                }

                // Slice 2 — record this binding's scope depth so the
                // drop-of-borrowed trigger can detect "source going out
                // of scope while a slice into it is still bound" cases.
                for name in pattern.binding_names() {
                    self.binding_scope_depth
                        .insert(name, self.current_scope_depth);
                }

                // Check 3b — caller-side borrow registration. If the RHS is
                // a borrow-returning call, the binding aliases its source
                // argument(s); register a persistent borrow so moving the
                // source while this binding is live is rejected. Runs after
                // the RHS walk so it sits outside the call's snapshot
                // restore. B-2026-06-07-5.
                let holder = match &pattern.kind {
                    PatternKind::Binding(n) => Some(n.as_str()),
                    _ => None,
                };
                self.register_ref_return_borrows(value, holder);
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
                        let_span: stmt.span,
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
                // `let ref name @ PATTERN = … else { }` — borrow, not move
                // (same rule as the `Let` arm above).
                if matches!(&pattern.kind, PatternKind::AtBinding { by_ref: true, .. }) {
                    self.check_expr_reading(value, states, param_types, param_usage);
                } else {
                    self.check_expr_consuming(value, states, param_types, param_usage);
                }
                self.define_pattern_states(pattern, states);
                let mut else_states = states.clone();
                self.check_block(else_block, &mut else_states, param_types, param_usage);
            }
            StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
                let mut defer_states = states.clone();
                self.check_block(body, &mut defer_states, param_types, param_usage);
                // design.md §10: a cleanup block never captures by move. A
                // binding of the enclosing scope that the block moves is an
                // error at the move (B-2026-11-06-28); a binding the block
                // declares itself is its own to move.
                let cleanup = if matches!(stmt.kind, StmtKind::Defer { .. }) {
                    "defer"
                } else {
                    "errdefer"
                };
                let mut moved: Vec<(&String, crate::token::Span)> = states
                    .iter()
                    .filter(|(_, before)| !matches!(before, ValueState::Moved { .. }))
                    .filter_map(|(name, _)| match defer_states.get(name) {
                        Some(ValueState::Moved { at }) => Some((name, *at)),
                        _ => None,
                    })
                    .collect();
                moved.sort_by_key(|(_, at)| (at.line, at.column));
                for (name, at) in moved {
                    self.errors.push(OwnershipError {
                        message: format!(
                            "`{cleanup}` block moves `{name}`: a cleanup block never captures by move, \
                             so the bindings it uses stay valid until the scope ends (design.md §10)"
                        ),
                        span: at,
                        kind: OwnershipErrorKind::MoveInCleanupBlock,
                        suggestion: Some(format!(
                            "borrow `{name}` in the block (a `ref` parameter or a `ref self` method), \
                             or move a clone: `{name}.clone()`"
                        )),
                        replacement: None,
                        consume_span: None,
                    });
                }
            }
            StmtKind::Assign { target, value } => {
                // Check if target is a variable — reassignment resets state
                if let ExprKind::Identifier(name) = &target.kind {
                    // Process the RHS first so reads of `name` in the RHS see
                    // the pre-assignment state. (e.g. `let x: T; x = f(x);`
                    // — the `x` inside `f(x)` is still Uninit and errors.)
                    self.check_expr_consuming(value, states, param_types, param_usage);
                    // B-2026-07-27-9 — `let x = e;` has no first-assignment
                    // phase (design.md § Variable Binding Rules), so ANY
                    // assignment to such a binding is a reassignment. Cannot
                    // double-report against the `Uninit`/`InitOnce` arms
                    // below: `LetUninit` never enters `immutable_lets`.
                    self.report_write_to_immutable_binding(
                        name,
                        &target.span,
                        OwnershipErrorKind::ReassignToImmutable,
                        format!("cannot reassign `{}`", name),
                    );
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
                                    first_assign: target.span,
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
                                span: target.span,
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
                    let borrow_key = self.slice_borrow_key_for(value);
                    if let Some(entry) = self.slice_borrow_sources.get(&borrow_key).cloned() {
                        // Same copy-out gate as the `Let` arm
                        // (B-2026-08-17-16): a deeply-owned RHS type means
                        // the chain copied out of the slice, so the
                        // binding is independent and the creation borrow
                        // ends at this statement.
                        let copied_out = self
                            .typecheck_result
                            .expr_type(value)
                            .is_some_and(crate::ownership::type_is_deeply_owned);
                        if copied_out {
                            self.end_copied_out_slice_borrow(&borrow_key);
                        } else if let Some(&lhs_depth) = self.binding_scope_depth.get(name) {
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
                        // `unused_mut`: a write through the place uses the
                        // root's `mut` even where the rule below exempts it
                        // (`let mut a = p;` copies a borrowed `p`).
                        self.mark_mut_let_written(&root);
                        // B-2026-07-27-9 — writing through a place rooted at
                        // an immutable binding (`p.x = 1;`, `v[0] = 1;`)
                        // mutates that binding's value just as surely as a
                        // rebind does — unless the path crosses a `shared`
                        // handle, which writes the pointee, not the binding.
                        if !self.place_writes_through_shared(target) {
                            self.report_write_to_immutable_binding(
                                &root,
                                &target.span,
                                OwnershipErrorKind::ReassignToImmutable,
                                format!("cannot assign through `{}`", root),
                            );
                        }
                    }
                    match self.map_index_assign_key(target) {
                        // `m[k] = v` inserts when `k` is missing, so the key
                        // moves into the map as `m.insert(k, v)`'s does
                        // (decided 2026-10-08).
                        Some((object, key)) => {
                            self.check_expr_reading(object, states, param_types, param_usage);
                            self.check_expr_consuming(key, states, param_types, param_usage);
                        }
                        None => {
                            // A range target is written through, not kept
                            // as a view (B-2026-10-21-36): `v[1..] = [5, 6];`.
                            let snapshot = self.snapshot_active_borrow_lens();
                            self.check_expr_reading(target, states, param_types, param_usage);
                            if matches!(&target.kind, ExprKind::Index { index, .. }
                                if matches!(index.kind, ExprKind::Range { .. }))
                            {
                                self.restore_active_borrows_to_snapshot(&snapshot);
                            }
                        }
                    }
                    self.check_expr_consuming(value, states, param_types, param_usage);
                }
            }
            StmtKind::CompoundAssign { target, value, .. } => {
                // Compound assignment (+=, -=, etc.) mutates the target
                if let ExprKind::Identifier(name) = &target.kind {
                    if let Some(usage) = param_usage.get_mut(name) {
                        *usage = ParamUsage::Mutated;
                    }
                    // B-2026-07-27-9 — `x += 1` is a read-modify-WRITE of the
                    // binding; same rule as the plain `=` arm above.
                    self.report_write_to_immutable_binding(
                        name,
                        &target.span,
                        OwnershipErrorKind::ReassignToImmutable,
                        format!("cannot compound-assign `{}`", name),
                    );
                } else if let Some(root) = Self::root_identifier(target) {
                    if let Some(usage) = param_usage.get_mut(&root) {
                        *usage = ParamUsage::Mutated;
                    }
                    self.mark_mut_let_written(&root);
                    // A compound assignment to a `mut ref` field writes the
                    // value it borrows: `g.value += 1` on a `MutexGuard[i64]`
                    // (concurrency.md; B-2026-10-21-72).
                    if !self.place_writes_through_shared(target)
                        && !self.expr_is_mut_ref_field(target)
                    {
                        self.report_write_to_immutable_binding(
                            &root,
                            &target.span,
                            OwnershipErrorKind::ReassignToImmutable,
                            format!("cannot compound-assign through `{}`", root),
                        );
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

impl super::OwnershipChecker<'_> {
    /// Record each name a `let` pattern binds with ITS type: a tuple pattern
    /// takes its elements' types, so `let (s, k) = pair();` types `s` as the
    /// shared struct it is rather than as the whole `(N, i64)` — which hid the
    /// shared-handle exemption from writes through `s` (B-2026-10-12-27).
    /// Any other pattern gives each name the value's type, as before.
    fn record_let_binding_types(&mut self, pattern: &Pattern, ty: &Type) {
        match (&pattern.kind, ty) {
            (PatternKind::Tuple(items), Type::Tuple(elems)) if items.len() == elems.len() => {
                for (item, elem) in items.iter().zip(elems) {
                    self.record_let_binding_types(item, elem);
                }
            }
            _ => {
                for name in pattern.binding_names() {
                    self.binding_types.insert(name.clone(), ty.clone());
                }
            }
        }
    }

    /// `m[k]` as an assignment target over a `Map` or `SortedMap`, under the
    /// v2 core or the D5 migration: the map and the key expressions.
    fn map_index_assign_key<'e>(&self, target: &'e Expr) -> Option<(&'e Expr, &'e Expr)> {
        if !crate::ownership::core_rules() && self.d5.is_none() {
            return None;
        }
        let ExprKind::Index { object, index } = &target.kind else {
            return None;
        };
        let ty = self.typecheck_result.expr_type(object)?;
        let ty = match ty {
            Type::Ref(t) | Type::MutRef(t) => t.as_ref(),
            t => t,
        };
        matches!(ty, Type::Named { name, .. } if name == "Map" || name == "SortedMap")
            .then_some((object.as_ref(), index.as_ref()))
    }
}

/// `self`, or a field / index / tuple projection of it: a place the method
/// borrows, never a temporary.
fn place_rooted_at_self(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::SelfValue => true,
        ExprKind::FieldAccess { object, .. }
        | ExprKind::Index { object, .. }
        | ExprKind::TupleIndex { object, .. } => place_rooted_at_self(object),
        _ => false,
    }
}
