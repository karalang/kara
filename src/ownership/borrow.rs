//! Borrow tracking, slice-source attribution, and call-site /
//! method-call analysis.
//!
//! Houses:
//!
//! - Call-arg formal mode lookups: `callee_modes_for_call`,
//!   `arg_is_borrow_position`, `arg_formal_slice_kind`,
//!   `arg_formal_ref_borrow_kind`.
//! - Place-expression root + slice-source attribution:
//!   `place_expr_root`, `record_slice_creation`,
//!   `slice_creation_source`.
//! - Active-borrow stack management (Slice 2 conflict detection):
//!   `push_active_borrow`, `classify_borrow_conflict`,
//!   `drain_borrows_at_depth`, `snapshot_active_borrow_lens`,
//!   `restore_active_borrows_to_snapshot`, `check_move_of_borrowed`.
//! - Method-call receiver mode lookups:
//!   `method_call_consumes_receiver`, `method_self_borrow_kind`,
//!   `method_call_receiver_is_mut_ref`.
//!
//! Lives in a sibling `impl<'a> super::OwnershipChecker<'a>` block.

use std::collections::HashMap;

use crate::ast::*;
use crate::resolver::SpanKey;
use crate::token::Span;

use super::{
    borrow_kind_display, slice_conflict_message, stdlib_method_self_borrow_kind, ActiveBorrow,
    ActiveClosureCapture, BorrowConflict, BorrowKind, CapturePath, OwnershipError,
    OwnershipErrorKind, OwnershipMode, PlaceExpr, Projection, SliceConflictShape,
};

impl<'a> super::OwnershipChecker<'a> {
    /// Look up the callee's parameter modes for a free-function or static-
    /// method `Call` expression. Returns `None` for callees we can't name
    /// (function-typed values, complex expressions); those fall back to
    /// the prior conservative consume-everything behavior.
    pub(crate) fn callee_modes_for_call(&self, callee: &Expr) -> Option<&Vec<OwnershipMode>> {
        let key = match &callee.kind {
            ExprKind::Identifier(name) => name.clone(),
            ExprKind::Path { segments, .. } => segments.join("."),
            _ => return None,
        };
        self.callee_param_modes.get(&key)
    }

    /// Whether the argument at `arg_index` of `callee` is a borrow position
    /// (param declared `ref T` / `mut ref T` / `mut Slice[T]`). Args at
    /// borrow positions are *read*, not consumed, regardless of the
    /// `mut_marker` flag (which is itself only legal on `MutRef` slots).
    ///
    /// B-2026-07-02-23: a desugared comparison operator (`Type.eq(a, b)` &
    /// friends — see `lowering::callee_is_relational_operator`) borrows both
    /// operands by the relational-trait contract, but its callee is an
    /// *instance* method whose modes never reach `callee_param_modes`
    /// (static-methods-only), so we recognise the shape directly and report
    /// every arg as a borrow. Mirrors the same gate in `use_classifier` (the
    /// predicate pipeline that actually emits the UAM diagnostic) so the
    /// legacy state machine's `states` don't diverge and mis-drive closure
    /// classification.
    pub(crate) fn arg_is_borrow_position(&self, callee: &Expr, arg_index: usize) -> bool {
        if crate::lowering::callee_is_relational_operator(callee) {
            return true;
        }
        self.callee_modes_for_call(callee)
            .and_then(|modes| modes.get(arg_index))
            .is_some_and(|m| matches!(m, OwnershipMode::Ref | OwnershipMode::MutRef))
    }

    /// `MethodCall` analogue of `arg_is_borrow_position`: whether call-arg
    /// `arg_index` of a method call lands in a borrow position (the resolved
    /// method's NON-self param `arg_index` is `ref`/`mut ref`/`mut Slice`).
    /// Arg indices map 1:1 to `method_param_modes` positions (the receiver
    /// is tracked separately via `method_self_modes`). Returns `false` for
    /// methods that don't resolve — stdlib methods without user impls,
    /// upstream typecheck errors — the conservative consume default that
    /// matches the prior behavior. Without this, a `ref`/`mut ref` struct
    /// arg of a `mut ref self` method was classified as a consume and the
    /// borrowed binding spuriously RC-promoted (B-2026-06-12-8).
    pub(crate) fn method_arg_is_borrow_position(
        &self,
        method_call: &Expr,
        arg_index: usize,
    ) -> bool {
        let Some(key) = self
            .typecheck_result
            .method_callee_types
            .get(&SpanKey::from_span(&method_call.span))
        else {
            return false;
        };
        self.method_param_modes
            .get(key)
            .and_then(|modes| modes.get(arg_index))
            .is_some_and(|m| matches!(m, OwnershipMode::Ref | OwnershipMode::MutRef))
    }

    /// Returns `Some(mutable)` if the formal at `arg_index` of `callee` is a
    /// slice type (`Slice[T]` or `mut Slice[T]`); `None` for non-slice
    /// formals or unresolvable callees. Drives Slice 1's call-arg coercion
    /// site detection.
    pub(crate) fn arg_formal_slice_kind(&self, callee: &Expr, arg_index: usize) -> Option<bool> {
        let key = match &callee.kind {
            ExprKind::Identifier(name) => name.clone(),
            ExprKind::Path { segments, .. } => segments.join("."),
            _ => return None,
        };
        self.callee_param_slice_kind
            .get(&key)
            .and_then(|kinds| kinds.get(arg_index).copied().flatten())
    }

    /// Slice 2 follow-up — return the call-arg-side borrow kind for a
    /// non-slice ref formal at `arg_index`. `Ref T` formals push
    /// `BorrowKind::ImmRef`; `MutRef T` formals push `BorrowKind::MutRef`.
    /// Slice formals (`Slice[T]` / `mut Slice[T]`) return `None` here so
    /// the existing slice creation hook owns those — the slice push
    /// already routes through the conflict matrix as `ImmSlice` / `MutSlice`.
    /// Owned formals and unresolvable callees also return `None`.
    pub(crate) fn arg_formal_ref_borrow_kind(
        &self,
        callee: &Expr,
        arg_index: usize,
    ) -> Option<BorrowKind> {
        if self.arg_formal_slice_kind(callee, arg_index).is_some() {
            return None;
        }
        let modes = self.callee_modes_for_call(callee)?;
        match modes.get(arg_index)? {
            OwnershipMode::Ref => Some(BorrowKind::ImmRef),
            OwnershipMode::MutRef => Some(BorrowKind::MutRef),
            OwnershipMode::Own => None,
        }
    }

    /// Exclusive-borrow rule at a call site (B-2026-06-17-6). A `mut ref` /
    /// `mut Slice` argument is an EXCLUSIVE borrow, so the place it borrows
    /// must not be borrowed again — shared or exclusive — by any other
    /// argument of the same call (all arguments' borrows are live together
    /// for the call's duration). Two arguments whose places overlap (equal,
    /// or one a prefix of the other) where at least one is exclusive violate
    /// the rule — `f(mut v, mut v)` and `f(mut v, v)` are the canonical
    /// cases. Without this the aliasing compiles and codegen miscompiles it
    /// (the Vec header is passed by value per borrow). `split_at_mut` is the
    /// sanctioned path for two disjoint mutable halves. Enforcing this is the
    /// soundness precondition for emitting LLVM `noalias` on `mut` params
    /// (B-2026-06-17-5). Keyed on the callee's declared parameter modes, so a
    /// missing/anonymous callee (function-typed value) is conservatively
    /// skipped rather than mis-flagged.
    pub(crate) fn check_exclusive_borrow_arg_aliasing(&mut self, callee: &Expr, args: &[CallArg]) {
        // (place, is_exclusive, span) for each argument that borrows a place.
        let mut borrows: Vec<(PlaceExpr, bool, Span)> = Vec::new();
        for (i, arg) in args.iter().enumerate() {
            let kind = match self.arg_formal_slice_kind(callee, i) {
                Some(true) => Some(BorrowKind::MutSlice),
                Some(false) => Some(BorrowKind::ImmSlice),
                None => self.arg_formal_ref_borrow_kind(callee, i),
            };
            let Some(kind) = kind else {
                continue;
            };
            let Some(place) = self.place_expr_root(&arg.value) else {
                continue;
            };
            let is_excl = kind.is_mut();
            for (prev_place, prev_excl, prev_span) in &borrows {
                if (is_excl || *prev_excl) && places_overlap(prev_place, &place) {
                    self.errors.push(OwnershipError {
                        message: format!(
                            "exclusive-borrow conflict: `{place}` is borrowed by two arguments of \
                             the same call and at least one is a `mut ref` / `mut Slice` \
                             (exclusive) borrow. An exclusive borrow must be the only active \
                             borrow of its place; pass distinct values, or use `split_at_mut` for \
                             two disjoint mutable halves.",
                            place = render_place(&place),
                        ),
                        span: arg.value.span.clone(),
                        kind: OwnershipErrorKind::ExclusiveBorrowAliasedArgs,
                        suggestion: Some(
                            "pass distinct bindings to the two parameters, or split the value \
                             into non-overlapping mutable borrows with `split_at_mut`"
                                .to_string(),
                        ),
                        replacement: None,
                        consume_span: Some(prev_span.clone()),
                    });
                    break;
                }
            }
            borrows.push((place, is_excl, arg.value.span.clone()));
        }
    }

    /// Resolve the root binding of a place expression at a slice creation
    /// site. Walks identifier / field / index / tuple-index / `.as_slice` /
    /// `.as_slice_mut` chains down to a root binding; returns `None` for
    /// expressions that don't start at a named binding (function-call
    /// results, struct / tuple / collection literals, etc.). For chains that
    /// pass through a slice binding (`s2 = s1[0..3]` where `s1` is itself a
    /// slice into `v`), the lookup walks transitively through
    /// `slice_binding_sources` so the returned root is the original storage
    /// (`v`), not the intermediate slice.
    pub(crate) fn place_expr_root(&self, expr: &Expr) -> Option<PlaceExpr> {
        match &expr.kind {
            ExprKind::Identifier(name) => {
                if let Some((parent, _)) = self.slice_binding_sources.get(name) {
                    Some(parent.clone())
                } else {
                    Some(PlaceExpr {
                        root: name.clone(),
                        projections: Vec::new(),
                    })
                }
            }
            ExprKind::FieldAccess { object, field, .. } => {
                let mut p = self.place_expr_root(object)?;
                p.projections.push(Projection::Field(field.clone()));
                Some(p)
            }
            ExprKind::Index { object, index } => {
                let mut p = self.place_expr_root(object)?;
                let proj = if matches!(&index.kind, ExprKind::Range { .. }) {
                    Projection::Range
                } else {
                    Projection::Index
                };
                p.projections.push(proj);
                Some(p)
            }
            ExprKind::TupleIndex { object, .. } => {
                let mut p = self.place_expr_root(object)?;
                p.projections.push(Projection::Index);
                Some(p)
            }
            ExprKind::MethodCall {
                object,
                method,
                args,
                ..
            } if (method == "as_slice" || method == "as_slice_mut") && args.is_empty() => {
                self.place_expr_root(object)
            }
            _ => None,
        }
    }

    /// Record a slice creation site if the source resolves to a rooted
    /// place. Called from each of the four slice creation hook points:
    /// `.as_slice()` / `.as_slice_mut()`, range-indexing, call-arg
    /// coercion, and let-binding-rhs coercion. Idempotent — recording the
    /// same span twice is a no-op (later writes overwrite with the same
    /// value). Slice 2: also pushes an `ActiveBorrow` so the conflict
    /// matrix sees this slice when later borrows are added.
    pub(crate) fn record_slice_creation(
        &mut self,
        slice_span: &Span,
        source: &Expr,
        mutable: bool,
    ) {
        if let Some(place) = self.place_expr_root(source) {
            let key = SpanKey::from_span(slice_span);
            if let std::collections::hash_map::Entry::Vacant(e) =
                self.slice_borrow_sources.entry(key)
            {
                e.insert((place.clone(), mutable));
                let kind = if mutable {
                    BorrowKind::MutSlice
                } else {
                    BorrowKind::ImmSlice
                };
                self.push_active_borrow(kind, place, slice_span.clone());
            }
        }
    }

    /// `impl Trait` slice 4 — at a `Call` site whose callee has captured
    /// input borrows (per `callee_existential_capture_indices`), register
    /// each captured argument as a slice-borrow source keyed by the
    /// call's span. The let-binding propagation at `StmtKind::Let` then
    /// copies the entry to `slice_binding_sources[name]` and records the
    /// LHS binding's scope depth against the source root, so the
    /// source-scope-exit drain fires `SliceBorrowConflict::DropOfBorrowed`
    /// when the captured source's scope ends while the existential
    /// binding is still alive at a broader scope.
    ///
    /// `BorrowKind::ImmSlice` is reused so the existing slice-shape drain
    /// path applies; the existential is conceptually a "slice into the
    /// captured input" for borrow-tracking purposes. A future slice may
    /// introduce a dedicated `Existential` borrow kind if diagnostic
    /// messaging needs to distinguish.
    pub(crate) fn record_existential_capture_borrows(
        &mut self,
        callee: &Expr,
        args: &[CallArg],
        call_span: &Span,
    ) {
        let key = match &callee.kind {
            ExprKind::Identifier(name) => name.clone(),
            ExprKind::Path { segments, .. } => segments.join("."),
            _ => return,
        };
        let Some(indices) = self.callee_existential_capture_indices.get(&key).cloned() else {
            return;
        };
        let span_key = SpanKey::from_span(call_span);
        for i in indices {
            let Some(arg) = args.get(i) else { continue };
            let Some(place) = self.place_expr_root(&arg.value) else {
                continue;
            };
            if let std::collections::hash_map::Entry::Vacant(e) =
                self.slice_borrow_sources.entry(span_key)
            {
                e.insert((place.clone(), false));
            }
            self.push_active_borrow(BorrowKind::ImmSlice, place, call_span.clone());
        }
    }

    /// If `expr` is a direct slice creation form (`.as_slice()` /
    /// `.as_slice_mut()` MethodCall, or `Index` with a `Range` index),
    /// return the source expression and the resulting slice's mutability.
    /// Used by the let-binding-rhs escape detector.
    pub(crate) fn slice_creation_source(expr: &Expr) -> Option<(&Expr, bool)> {
        match &expr.kind {
            ExprKind::MethodCall {
                object,
                method,
                args,
                ..
            } if (method == "as_slice" || method == "as_slice_mut") && args.is_empty() => {
                Some((object.as_ref(), method == "as_slice_mut"))
            }
            ExprKind::Index { object, index } if matches!(&index.kind, ExprKind::Range { .. }) => {
                Some((object.as_ref(), false))
            }
            _ => None,
        }
    }

    /// Slice 2 — push an active borrow into `active_borrows[source.root]`,
    /// scanning the existing entries first to detect slice-vs-slice and
    /// slice-vs-ref conflicts. Conflicts emit `SliceBorrowConflict` (same
    /// shape: A imm+mut, B mut+mut) or `CrossBorrowConflict` (slice + ref
    /// of same root) with the existing borrow's span as the secondary
    /// label. The new borrow is recorded regardless — we keep both so a
    /// later third operation can still detect against either.
    pub(crate) fn push_active_borrow(&mut self, kind: BorrowKind, source: PlaceExpr, span: Span) {
        // Scan existing borrows on the same root for conflicts.
        if let Some(existing) = self.active_borrows.get(&source.root) {
            for prior in existing {
                let conflict = self.classify_borrow_conflict(&prior.kind, &kind);
                match conflict {
                    BorrowConflict::SliceShape(shape) => {
                        self.errors.push(OwnershipError {
                            message: format!(
                                "{}: existing borrow at line {}:{}",
                                slice_conflict_message(&shape, &source.root),
                                prior.span.line,
                                prior.span.column
                            ),
                            span: span.clone(),
                            kind: OwnershipErrorKind::SliceBorrowConflict { shape },
                            suggestion: Some(
                                "drop the prior borrow before creating a new one (or restructure so they don't overlap)"
                                    .to_string(),
                            ),
                            replacement: None,
                            consume_span: Some(prior.span.clone()),
                        });
                    }
                    BorrowConflict::CrossForm => {
                        self.errors.push(OwnershipError {
                            message: format!(
                                "`{}` cannot be borrowed as `{}` because it is also borrowed as `{}` at line {}:{}",
                                source.root,
                                borrow_kind_display(&kind),
                                borrow_kind_display(&prior.kind),
                                prior.span.line,
                                prior.span.column
                            ),
                            span: span.clone(),
                            kind: OwnershipErrorKind::CrossBorrowConflict,
                            suggestion: Some(
                                "drop the slice borrow before mutating the source (or restructure so they don't overlap)"
                                    .to_string(),
                            ),
                            replacement: None,
                            consume_span: Some(prior.span.clone()),
                        });
                    }
                    BorrowConflict::None => {}
                }
            }
        }
        self.active_borrows
            .entry(source.root.clone())
            .or_default()
            .push(ActiveBorrow {
                kind,
                source,
                span,
                scope_depth: self.current_scope_depth,
            });
    }

    /// Slice 2 — classify the conflict shape between an existing borrow
    /// and a newly-pushed one. Symmetric in the slice-vs-slice cases (A
    /// fires whether existing is imm or new is imm). Cross-form pairs
    /// (slice + ref) route through `CrossBorrowConflict` rather than
    /// `SliceBorrowConflict`.
    #[allow(clippy::unused_self)]
    pub(crate) fn classify_borrow_conflict(
        &self,
        existing: &BorrowKind,
        new: &BorrowKind,
    ) -> BorrowConflict {
        match (existing.is_slice(), new.is_slice()) {
            (true, true) => match (existing.is_mut(), new.is_mut()) {
                (false, false) => BorrowConflict::None, // two imm slices — OK
                (true, true) => BorrowConflict::SliceShape(SliceConflictShape::MutSliceVsMutSlice),
                _ => BorrowConflict::SliceShape(SliceConflictShape::ImmSliceVsMutSlice),
            },
            (true, false) | (false, true) => {
                if existing.is_mut() || new.is_mut() {
                    BorrowConflict::CrossForm
                } else {
                    // Two immutable borrows of any form coexist — read-only.
                    BorrowConflict::None
                }
            }
            (false, false) => BorrowConflict::None,
        }
    }

    /// Slice 2 — drain any active borrows whose `scope_depth` exceeds the
    /// current scope depth. Called at block exit (after the in-block walk
    /// completes, before the depth decrements). Drop-of-borrowed detection
    /// rides this drain: a draining slice borrow whose source root is
    /// itself going out of scope here AND was bound at a shallower scope
    /// indicates the slice outlives its source storage.
    pub(crate) fn drain_borrows_at_depth(&mut self, exit_depth: usize) {
        let mut to_emit: Vec<(PlaceExpr, Span, Span)> = Vec::new();
        for (root, borrows) in self.active_borrows.iter_mut() {
            // For each draining slice, check whether its source root is
            // also dropping at this scope. The source's binding scope is
            // tracked separately so we know if the source's storage goes
            // away here.
            let source_dropping_now = self
                .binding_scope_depth
                .get(root)
                .is_some_and(|&depth| depth >= exit_depth);
            borrows.retain(|b| {
                if b.scope_depth >= exit_depth {
                    if source_dropping_now && b.kind.is_slice() {
                        // Slice's binding scope (where the slice value
                        // lives, populated at let time) is shallower
                        // than the source's? Then the slice will live
                        // past the source — shape D. We use
                        // `slice_binding_scope_depth` indexed by the
                        // root to flag this; if not present, conservative
                        // fall-through to drain without emitting.
                        if let Some(&slice_depth) =
                            self.slice_binding_scope_depth.get(&b.source.root)
                        {
                            if slice_depth < exit_depth {
                                to_emit.push((b.source.clone(), b.span.clone(), b.span.clone()));
                            }
                        }
                    }
                    false // drain
                } else {
                    true // keep
                }
            });
        }
        // Drop empty entries so the map stays clean.
        self.active_borrows.retain(|_, v| !v.is_empty());
        // Disjoint capture slice 3 — drain closure-capture borrows
        // whose recording scope is exiting at this depth. The closure
        // borrow's lifetime tracks the scope that holds the closure
        // value, mirroring the slice-borrow scope model.
        self.closure_capture_borrows.retain(|_, captures| {
            captures.retain(|c| c.scope_depth < exit_depth);
            !captures.is_empty()
        });
        for (place, span, secondary) in to_emit {
            self.errors.push(OwnershipError {
                message: format!(
                    "slice into `{}` outlives its source: source dropped at end of scope while slice borrow is still live",
                    place.root,
                ),
                span,
                kind: OwnershipErrorKind::SliceBorrowConflict {
                    shape: SliceConflictShape::DropOfBorrowed,
                },
                suggestion: Some(
                    "extend the source binding's scope to outlive the slice, or restructure so the slice does not escape"
                        .to_string(),
                ),
                replacement: None,
                consume_span: Some(secondary),
            });
        }
    }

    /// Slice 2 — snapshot active-borrow per-root counts before walking a
    /// `Call` or `MethodCall`. Use with `restore_active_borrows_to_snapshot`
    /// after the args walk to drop the call-arg-coerced slice borrows
    /// (they are call-statement-scoped per the slice plan's sub-step (g)
    /// — the slice value lives only for the call's duration). This still
    /// lets the conflict matrix fire mid-call (the push side-effect emits
    /// the diagnostic before the drain), so persistent slice + transient
    /// coerced slice still conflicts. Sequential calls do not stack up.
    pub(crate) fn snapshot_active_borrow_lens(&self) -> HashMap<String, usize> {
        self.active_borrows
            .iter()
            .map(|(k, v)| (k.clone(), v.len()))
            .collect()
    }

    pub(crate) fn restore_active_borrows_to_snapshot(&mut self, snapshot: &HashMap<String, usize>) {
        let roots: Vec<String> = self.active_borrows.keys().cloned().collect();
        for root in roots {
            let target = snapshot.get(&root).copied().unwrap_or(0);
            if let Some(borrows) = self.active_borrows.get_mut(&root) {
                if borrows.len() > target {
                    borrows.truncate(target);
                }
            }
        }
        self.active_borrows.retain(|_, v| !v.is_empty());
    }

    /// Slice 2 — at every consume that would transition `name` to
    /// `Moved`, check whether `name` has any live slice borrows. If so,
    /// emit shape C (move-of-borrowed) before the move proceeds. Returns
    /// `true` iff a conflict was emitted (caller may use this to suppress
    /// the consume — but v1 keeps the consume regardless so downstream
    /// state stays consistent).
    pub(crate) fn check_move_of_borrowed(&mut self, name: &str, move_span: &Span) -> bool {
        let Some(borrows) = self.active_borrows.get(name) else {
            return false;
        };
        if borrows.is_empty() {
            return false;
        }
        // Use the first live borrow as the secondary span — multiple
        // borrows would each fire, but for v1 we keep the diagnostic
        // count to one per move.
        let prior = borrows[0].clone();
        // Word the diagnostic by borrow form: a returned borrow (`let n =
        // f(u)` where `f -> ref T`) registers a non-slice `ImmRef`/`MutRef`
        // borrow on its source — moving the source while that borrow is
        // live is the use-after-free 3b closes (B-2026-06-07-5).
        let borrow_desc = if prior.kind.is_slice() {
            "a slice borrow into it"
        } else {
            "a borrow into it (a returned reference still points at it)"
        };
        self.errors.push(OwnershipError {
            message: format!(
                "cannot move `{}` while {} is still live (borrowed at line {}:{})",
                name, borrow_desc, prior.span.line, prior.span.column
            ),
            span: move_span.clone(),
            kind: OwnershipErrorKind::SliceBorrowConflict {
                shape: SliceConflictShape::MoveOfBorrowed,
            },
            suggestion: Some(
                "drop the slice borrow before moving the source, or restructure so they don't overlap"
                    .to_string(),
            ),
            replacement: None,
            consume_span: Some(prior.span),
        });
        true
    }

    /// Resolve the method's receiver mode for a `MethodCall` expression.
    /// Returns `true` iff the receiver should be consumed (declared
    /// `bare self`). Reads the typechecker's method-callee resolution to
    /// pick the canonical `Type.method` key, then looks up the declared
    /// `SelfParam` from the impl-block / trait declaration.
    ///
    /// Falls back to `false` (read-only receiver, the prior behavior) when
    /// the lookup misses — typecheck errors upstream, methods on stdlib
    /// types whose impls are not in user code, etc. This is a conservative
    /// default: if we can't prove the receiver is consumed, we assume it
    /// isn't.
    pub(crate) fn method_call_consumes_receiver(&self, method_call: &Expr) -> bool {
        let key = match self
            .typecheck_result
            .method_callee_types
            .get(&SpanKey::from_span(&method_call.span))
        {
            Some(k) => k,
            None => return false,
        };
        matches!(self.method_self_modes.get(key), Some(SelfParam::Owned))
    }

    /// Slice 2 — the receiver-side `BorrowKind` for a `MethodCall`. Drives
    /// the call-statement-scoped ref-side push that Slice 2's sub-step (g)
    /// gates on. Returns `None` for static methods, bare-self consumes
    /// (no borrow), and unresolved methods. Falls through to a small
    /// table of stdlib method receiver modes when the user-impl lookup
    /// misses — `Vec.push` / `Map.insert` etc. don't have user-side
    /// `impl` blocks, so without the table cross-borrow detection would
    /// silently miss for the most common case (`let _s = v.as_slice();
    /// v.push(99);`).
    pub(crate) fn method_self_borrow_kind(&self, method_call: &Expr) -> Option<BorrowKind> {
        let key = self
            .typecheck_result
            .method_callee_types
            .get(&SpanKey::from_span(&method_call.span))?;
        if let Some(self_param) = self.method_self_modes.get(key) {
            return match self_param {
                SelfParam::Owned => None,
                SelfParam::Ref => Some(BorrowKind::ImmRef),
                SelfParam::MutRef => Some(BorrowKind::MutRef),
            };
        }
        stdlib_method_self_borrow_kind(key)
    }

    /// Whether the resolved method's receiver is `mut ref self`. Used by the
    /// trigger 3 detection: a `mut ref self` receiver is a "container" in the
    /// design.md § Part 4 trigger 3 sense — it outlives the call, so an
    /// owned arg consumed into it stays alive on a path parallel to any
    /// subsequent outer use of the source binding.
    pub(crate) fn method_call_receiver_is_mut_ref(&self, method_call: &Expr) -> bool {
        let key = match self
            .typecheck_result
            .method_callee_types
            .get(&SpanKey::from_span(&method_call.span))
        {
            Some(k) => k,
            None => return false,
        };
        matches!(self.method_self_modes.get(key), Some(SelfParam::MutRef))
    }

    /// Disjoint capture slice 3 — record a closure-induced borrow for
    /// each `Ref` / `MutRef` capture path the slice-2 inference produced.
    /// `Own` paths are skipped (those route through the consume machinery,
    /// not borrow tracking). **Whole-root paths (empty projection) are
    /// also skipped**: those are the existing RC-trigger-2 surface (a
    /// bare-identifier or stopping-construct capture composed with an
    /// outer use routes through `RcTrigger::ClosureCaptureWithOuterUse`
    /// for RC promotion, not borrow-style rejection — see design.md
    /// § Closures Rule 2 sub-case (ii)). Slice 3's rejection rule only
    /// applies to *path-precise* captures (non-empty projection), which
    /// is the value-add over per-name capture: a closure body that
    /// projects through a specific field commits the borrow checker to
    /// that path and disjoint sibling-path access remains accessible
    /// while overlapping ancestor / equal-path access is restricted.
    /// Each borrow is scope-stamped with the current scope depth so the
    /// drain at block-exit retires it when the holding scope ends.
    pub(crate) fn push_closure_capture_borrows(
        &mut self,
        closure_span: &Span,
        path_modes: &[(CapturePath, OwnershipMode)],
    ) {
        for (path, mode) in path_modes {
            if matches!(mode, OwnershipMode::Own) {
                continue;
            }
            if path.projection.is_empty() {
                continue;
            }
            self.closure_capture_borrows
                .entry(path.root.clone())
                .or_default()
                .push(ActiveClosureCapture {
                    path: path.clone(),
                    mode: mode.clone(),
                    closure_span: closure_span.clone(),
                    scope_depth: self.current_scope_depth,
                });
        }
    }

    /// Disjoint capture slice 3 — translate a place expression's
    /// projection chain into the `Vec<String>` shape `CapturePath` uses.
    /// Returns `None` if any segment is `Index` / `Range` (a stopping
    /// construct in slice 1's path enumeration), so the conflict
    /// checker can fall back to whole-root overlap rather than try to
    /// reason about index-dependent disjointness.
    fn place_projection_as_field_chain(place: &PlaceExpr) -> Option<Vec<String>> {
        let mut out = Vec::with_capacity(place.projections.len());
        for p in &place.projections {
            match p {
                Projection::Field(name) => out.push(name.clone()),
                Projection::Index | Projection::Range => return None,
            }
        }
        Some(out)
    }

    /// Disjoint capture slice 3 — check whether a consume of `place`
    /// (root + projection chain) conflicts with any live closure-
    /// capture borrow under the same root. Overlap is bidirectional
    /// projection-prefix (matches slice-2's mutation-walker overlap
    /// rule): the consume conflicts iff the shorter of the two
    /// projections is a prefix of the longer. Disjoint sibling paths
    /// (first differing segment exists within both) skip cleanly.
    /// An `Index`-bearing consume falls back to whole-root overlap (the
    /// consume's projection is unrecoverable as a field chain), which
    /// conflicts with every captured path under that root.
    pub(crate) fn check_consume_vs_closure_captures(
        &mut self,
        place: &PlaceExpr,
        consume_span: &Span,
    ) {
        let consume_chain = Self::place_projection_as_field_chain(place);
        let Some(captures) = self.closure_capture_borrows.get(&place.root) else {
            return;
        };
        if captures.is_empty() {
            return;
        }
        let captures = captures.clone();
        for cap in &captures {
            let overlaps = match &consume_chain {
                None => true,
                Some(chain) => {
                    let shorter = cap.path.projection.len().min(chain.len());
                    cap.path.projection[..shorter] == chain[..shorter]
                }
            };
            if !overlaps {
                continue;
            }
            let consume_place_str = if place.projections.is_empty() {
                format!("`{}`", place.root)
            } else {
                let chain = consume_chain
                    .as_ref()
                    .map(|c| c.join("."))
                    .unwrap_or_else(|| "<projection>".to_string());
                format!("`{}.{}`", place.root, chain)
            };
            let captured_place_str = if cap.path.projection.is_empty() {
                format!("`{}`", cap.path.root)
            } else {
                format!("`{}.{}`", cap.path.root, cap.path.projection.join("."))
            };
            let mode_str = match cap.mode {
                OwnershipMode::Ref => "ref",
                OwnershipMode::MutRef => "mut ref",
                OwnershipMode::Own => "own",
            };
            self.errors.push(OwnershipError {
                message: format!(
                    "cannot consume {} while closure at line {}:{} captures {} by `{}` (borrow still live)",
                    consume_place_str,
                    cap.closure_span.line,
                    cap.closure_span.column,
                    captured_place_str,
                    mode_str,
                ),
                span: consume_span.clone(),
                kind: OwnershipErrorKind::ClosureCaptureBorrowConflict,
                suggestion: Some(
                    "drop the closure (or restructure so its captures end before the consume) before moving the source"
                        .to_string(),
                ),
                replacement: None,
                consume_span: Some(cap.closure_span.clone()),
            });
        }
    }
}

/// Do two argument places overlap — i.e. could they name memory that aliases?
/// Same root binding AND one projection chain is a prefix of the other (so one
/// place contains or equals the other). Field steps must match by name to stay
/// disjoint (`a.x` vs `a.y` do not overlap); `Index`/`Range` steps are treated
/// as possibly-equal (we don't track index values), so `a[i]` vs `a[j]` is a
/// conservative overlap — matching the coarse borrow model, where `split_at_mut`
/// is the sanctioned way to assert disjoint mutable sub-ranges.
fn places_overlap(a: &PlaceExpr, b: &PlaceExpr) -> bool {
    if a.root != b.root {
        return false;
    }
    let common = a.projections.len().min(b.projections.len());
    a.projections[..common]
        .iter()
        .zip(&b.projections[..common])
        .all(|(pa, pb)| proj_compatible(pa, pb))
}

/// Whether two projection steps at the same depth could address the same slot.
fn proj_compatible(a: &Projection, b: &Projection) -> bool {
    match (a, b) {
        (Projection::Field(x), Projection::Field(y)) => x == y,
        // A field step and an index/range step at the same depth can't both
        // apply to one value — different access shapes, treat as disjoint.
        (Projection::Field(_), _) | (_, Projection::Field(_)) => false,
        // Two index/range steps: index values are not tracked, so assume overlap.
        _ => true,
    }
}

/// Render a place for diagnostics: `v`, `c.inner`, `arr[..]`, `v[..]`.
fn render_place(p: &PlaceExpr) -> String {
    let mut s = p.root.clone();
    for proj in &p.projections {
        match proj {
            Projection::Field(f) => {
                s.push('.');
                s.push_str(f);
            }
            Projection::Index | Projection::Range => s.push_str("[..]"),
        }
    }
    s
}
