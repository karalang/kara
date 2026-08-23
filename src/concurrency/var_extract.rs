//! Variable extraction: pattern-binding and definition collection.
//!
//! Extracted verbatim from `concurrency.rs`'s `ConcurrencyChecker` impl
//! (structural-debt extraction, 2026-08-16). Lives in a sibling
//! `impl super::ConcurrencyChecker` block; methods are `pub(super)`.

use super::*;

impl<'a> super::ConcurrencyChecker<'a> {
    pub(super) fn collect_pattern_bindings(
        &self,
        pattern: &Pattern,
        defines: &mut HashSet<String>,
    ) {
        match &pattern.kind {
            PatternKind::Binding(name) => {
                defines.insert(name.clone());
            }
            PatternKind::AtBinding { name, pattern, .. } => {
                defines.insert(name.clone());
                self.collect_pattern_bindings(pattern, defines);
            }
            PatternKind::Struct { fields, .. } => {
                for f in fields {
                    if let Some(ref p) = f.pattern {
                        self.collect_pattern_bindings(p, defines);
                    } else {
                        // Shorthand field: `Foo { x }` — the field name is the binding
                        defines.insert(f.name.clone());
                    }
                }
            }
            PatternKind::TupleVariant { patterns, .. } | PatternKind::Tuple(patterns) => {
                for p in patterns {
                    self.collect_pattern_bindings(p, defines);
                }
            }
            PatternKind::Or(patterns) => {
                for p in patterns {
                    self.collect_pattern_bindings(p, defines);
                }
            }
            PatternKind::Slice {
                prefix,
                rest,
                suffix,
            } => {
                for p in prefix.iter().chain(suffix.iter()) {
                    self.collect_pattern_bindings(p, defines);
                }
                if let Some(RestPattern::Bound(name)) = rest {
                    defines.insert(name.clone());
                }
            }
            PatternKind::Wildcard | PatternKind::Literal(_) | PatternKind::RangePattern { .. } => {}
        }
    }

    /// `true` iff any binding introduced by `pattern` has surface type
    /// `Sender` / `Receiver` — a channel end. Keyed off the typechecker's
    /// `pattern_binding_types` (the same span-stable table codegen consults to
    /// emit the scope-exit `DropChannelEnd`), so it fires for both the
    /// single-binding `let rx = after(…)` and the `let (tx, rx) = Channel.new()`
    /// destructure.
    ///
    /// Used by `find_parallel_groups` to keep a statement that *produces* a
    /// channel-end binding out of auto-par groups. `stmt_has_channel_op`
    /// already excludes a statement that performs a channel op syntactically
    /// (`Channel.new()` / `send` / `recv`), but a plain call whose RETURN is a
    /// channel end — `std.web.time.after` returns `Receiver[()]` — is invisible
    /// to that AST walk. Lifting such a `let` into a `__par_branch` worker
    /// duplicates the channel end's `DropChannelEnd`: the branch writes the
    /// {handle} back into the parent frame by bit-copy, so both the branch's
    /// captured alloca and the parent's binding emit a `drop_receiver`, driving
    /// the channel's `total` one below its true live-end count. On the
    /// host-async timer path (`let rx = after(ms); rx.recv()`) that made a live
    /// receiver read as dropped, so the host's timer `channel_send` panicked
    /// with "send on a channel with no live receiver". Sequential is always
    /// correct; auto-par is only an optimization.
    pub(super) fn pattern_binds_channel_end(&self, pattern: &Pattern) -> bool {
        let Some(types) = self.types else {
            return false;
        };
        self.pattern_binding_is_channel_end(pattern, &types.pattern_binding_types)
    }

    fn pattern_binding_is_channel_end(
        &self,
        pattern: &Pattern,
        binding_types: &rustc_hash::FxHashMap<SpanKey, String>,
    ) -> bool {
        let is_end = |p: &Pattern| {
            matches!(
                binding_types
                    .get(&SpanKey::from_span(&p.span))
                    .map(String::as_str),
                Some("Sender") | Some("Receiver")
            )
        };
        match &pattern.kind {
            PatternKind::Binding(_) => is_end(pattern),
            PatternKind::AtBinding { pattern: inner, .. } => {
                is_end(pattern) || self.pattern_binding_is_channel_end(inner, binding_types)
            }
            PatternKind::Struct { fields, .. } => fields.iter().any(|f| {
                f.pattern
                    .as_ref()
                    .is_some_and(|p| self.pattern_binding_is_channel_end(p, binding_types))
            }),
            PatternKind::TupleVariant { patterns, .. }
            | PatternKind::Tuple(patterns)
            | PatternKind::Or(patterns) => patterns
                .iter()
                .any(|p| self.pattern_binding_is_channel_end(p, binding_types)),
            PatternKind::Slice { prefix, suffix, .. } => prefix
                .iter()
                .chain(suffix.iter())
                .any(|p| self.pattern_binding_is_channel_end(p, binding_types)),
            PatternKind::Wildcard | PatternKind::Literal(_) | PatternKind::RangePattern { .. } => {
                false
            }
        }
    }

    pub(super) fn collect_assign_target_defines(&self, expr: &Expr, defines: &mut HashSet<String>) {
        match &expr.kind {
            ExprKind::Identifier(name) => {
                defines.insert(name.clone());
            }
            // The receiver of a mutating `self.method()` call, and the root of a
            // `self.field = …` / `self.field[i] = …` write, is `self` — record it
            // under the canonical name "self" (matched by `collect_expr_reads`'s
            // SelfValue arm). Without this, a `mut ref self` method call recorded
            // no write and a `self.field` assignment defined nothing, so the
            // data-dependency check missed every self-mutation (self-hosting #8).
            ExprKind::SelfValue => {
                defines.insert("self".to_string());
            }
            // Every PLACE-PROJECTION form recurses to its object: the write
            // lands on the root binding regardless of how the place was spelled.
            // `TupleIndex` was missing here until B-2026-08-04-15, and its
            // absence was a MISCOMPILE, not a missed optimization: for
            // `t.0.push(x)` the `MethodCall` arm below recursed onto the
            // receiver `t.0`, which fell through to `_ => {}` and recorded NO
            // write. Two pushes to the same tuple-element Vec then looked
            // mutually independent, auto-par grouped them (and the read after
            // them) as "no data or effect dependencies", and the program
            // silently lost its stores. The `FieldAccess` sibling was present,
            // which is why `h.items.push(x)` was always correct and only the
            // tuple spelling broke.
            //
            // KEEP THIS ARM COMPLETE. The canonical list of place-projection
            // forms is `place_root` (this file); anything it walks through must
            // be walked through here too, or a write through that spelling goes
            // unrecorded. The arms below (`Deref`, `MethodCall`) extend past
            // `place_root` deliberately — a write can also be rooted through
            // them — so the two are not interchangeable, only overlapping.
            ExprKind::FieldAccess { object, .. }
            | ExprKind::Index { object, .. }
            | ExprKind::TupleIndex { object, .. } => {
                self.collect_assign_target_defines(object, defines);
            }
            ExprKind::Unary {
                op: UnaryOp::Deref,
                operand,
            } => {
                // `*place = …` / `*place += …` writes THROUGH the deref, so the
                // mutated state is rooted at the operand's root. Critically,
                // `*m.entry(k).or_insert(d) += 1` writes the MAP `m`: without
                // recording it, the auto-par dependency check saw a `for`-loop
                // histogram body as not writing the map, then parallelized the
                // loop against a later `m.keys()` read — a read-after-write race
                // on the map (B-2026-06-20-16). A `*ref += …` on a mut-ref local
                // records the binding, which is conservative (it actually writes
                // the pointee) and so always sound for the parallel-safety gate.
                self.collect_assign_target_defines(operand, defines);
            }
            ExprKind::MethodCall { object, .. } => {
                // A method-chain PLACE target — `m.entry(k).or_insert(d)`,
                // `v.get_mut(i)` — is rooted at the receiver; record it so a
                // write through the returned slot serializes against sibling
                // reads of the same container.
                self.collect_assign_target_defines(object, defines);
            }
            _ => {}
        }
    }

    /// Reads performed BY the target place itself — the index expressions a
    /// place walks through (`a[i].f = …` reads `i`), not the assigned-to root.
    ///
    /// `TupleIndex` recurses here for the same reason it does in
    /// `collect_assign_target_defines` (B-2026-08-04-15): a tuple projection is
    /// a place-projection form, so a target spelled through one must keep
    /// walking to reach the `Index` arms that contribute reads. A tuple index
    /// is a compile-time literal and contributes no read of its own, so this
    /// arm only restores the traversal — but stopping here made
    /// `t.0[i] = v` miss the read of `i`.
    pub(super) fn collect_assign_target_reads(&self, expr: &Expr, reads: &mut HashSet<String>) {
        match &expr.kind {
            ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
                self.collect_assign_target_reads(object, reads);
            }
            ExprKind::Index { object, index } => {
                self.collect_assign_target_reads(object, reads);
                self.collect_expr_reads(index, reads);
            }
            _ => {}
        }
    }
}
