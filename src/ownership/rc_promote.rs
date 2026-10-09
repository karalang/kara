//! Rc → Arc promotion + RC fallback note emission + `@no_rc`
//! enforcement (Phases 2 / 3 / K2).
//!
//! Houses:
//!
//! - `emit_rc_fallback_notes` — Phase 3: emit one `RcFallbackNote`
//!   per RC binding, flavored "Rc" or "Arc" per Phase 2's outcome.
//! - `promote_rc_to_arc` — Phase 2: walk each function's body via
//!   `scan_block_for_par_uses` to find bindings live across a
//!   `par {}` region, and promote them.
//! - `promote_for_function` — per-function helper for `promote_rc_to_arc`.
//! - `enforce_no_rc_attrs` — K2: enforce `#[no_rc]` and `@no_rc`
//!   attributes on functions and impl methods, erroring on any
//!   RC trigger.
//!
//! Lives in a sibling `impl<'a> super::OwnershipChecker<'a>` block.

use std::collections::{HashMap, HashSet};

use crate::resolver::SpanKey;

use crate::ast::*;
use crate::token::Span;

use super::par_helpers::{collect_channel_param_types, has_attr, scan_block_for_par_uses};
use super::{OwnershipError, OwnershipErrorKind};

impl<'a> super::OwnershipChecker<'a> {
    // ── RC Fallback Notes (emitted after Phase 2) ────────────────

    /// Emit one `RcFallbackNote` per RC binding, with the flavor determined
    /// by Phase 2: bindings in `arc_values` get "shared (Arc) — promoted:
    /// value crosses a parallel region"; others get "shared (Rc) — value
    /// does not cross a parallel region".
    ///
    /// Slice-6 enrichment (line 353 phase-5 checklist disjoint-capture
    /// slice 6): when the RC binding was captured *whole* by a closure
    /// inside the same enclosing function, append the
    /// `WholeRootCaptureReason` explanation to the note message and
    /// replace the generic suggestion with a fix-it that names the
    /// rewrite (hoist the field access outside the stopping
    /// construct). Lets the user see *why* their natural sibling-path
    /// access forced an RC promotion — without the explanation, the
    /// N0503 note tells the user RC happened but not which construct
    /// in the closure body forced the whole-root capture choice.
    pub(crate) fn emit_rc_fallback_notes(&mut self) {
        let mut notes = Vec::new();
        // B-2026-08-14-34 leg B — flatten and SORT before building notes.
        // `rc_values` and its inner maps are `HashMap`s, so walking them
        // directly emitted the notes in per-process hash order: two RC
        // fallbacks in one function came out in either order run-to-run
        // (measured 16/4 over 20 runs of one binary on the same input), and
        // the order reaches `karac check --output=json`, which the Mend loop
        // diffs. Note that this was not "sorted with an unstable tie-break" —
        // there was no ordering at all, which is why the note printed SECOND
        // could cite the EARLIER column.
        //
        // The ordering rule is the one `rc_fallback_queries::analyze` already
        // applies to the same two maps for the same reason: use-site offset,
        // then fn key, then binding name. Both surfaces describe the same
        // sites, so they should not disagree about their order.
        let mut sites: Vec<(&String, &String, &crate::ownership::RcEntry)> = Vec::new();
        for (fn_key, rc_map) in &self.rc_values {
            if self.suppressed_rc_fn_keys.contains(fn_key) {
                continue;
            }
            for (binding, entry) in rc_map {
                sites.push((fn_key, binding, entry));
            }
        }
        sites.sort_by(|(ak, ab, ae), (bk, bb, be)| {
            ae.other_use_span
                .offset
                .cmp(&be.other_use_span.offset)
                .then_with(|| ak.cmp(bk))
                .then_with(|| ab.cmp(bb))
        });
        for (fn_key, binding, entry) in sites {
            let arc_set = self.arc_values.get(fn_key);
            let is_arc = arc_set.is_some_and(|s| s.contains(binding));
            let flavor = if is_arc {
                "shared (Arc) — promoted: value crosses a parallel region"
            } else {
                "shared (Rc) — value does not cross a parallel region"
            };
            let base_message = format!(
                "RC fallback inserted for '{}' ({}); {}; consume at line {}:{}, other use at line {}:{}",
                entry.binding,
                entry.trigger.label(),
                flavor,
                entry.consume_span.line,
                entry.consume_span.column,
                entry.other_use_span.line,
                entry.other_use_span.column,
            );
            let (message, suggestion) =
                match self.find_whole_root_closure_reason(fn_key, binding) {
                    Some((closure_span, reason)) => {
                        let reason_text = reason.describe(binding);
                        let enriched = format!(
                            "{} — closure at line {}:{} captured `{}` whole because {}",
                            base_message,
                            closure_span.line,
                            closure_span.column,
                            binding,
                            reason_text,
                        );
                        (enriched, Some(Self::slice6_fix_it_suggestion(&reason)))
                    }
                    None => (
                        base_message,
                        Some(
                            "restructure to a single ownership path, or accept the RC and silence with #[allow(rc_fallback)]"
                                .to_string(),
                        ),
                    ),
                };
            notes.push(OwnershipError {
                message,
                span: entry.other_use_span,
                kind: OwnershipErrorKind::RcFallbackNote,
                suggestion,
                replacement: None,
                consume_span: Some(entry.consume_span),
            });
        }
        self.notes.extend(notes);
    }

    /// v2 core (`docs/core-semantics.md` §3.2, §3.3, §6.4) — the E0500 error each
    /// RC-fallback site is under the core rules. The compiler never shares a
    /// value to make a program legal, so every site that legacy turns into an
    /// RC is a use of a moved or maybe-moved place: a use after a move on one
    /// branch, a move in a loop that runs again, a move into a container or a
    /// closure followed by another use.
    ///
    /// Computed for every site, including functions carrying
    /// `#[allow(rc_fallback)]` (the attribute silences a performance note, and
    /// under the core there is no fallback left to accept). Returned rather
    /// than pushed: only the strict commands (`karac check`, `karac fix`)
    /// report them, so the legacy backends keep compiling these programs as
    /// the old-behaviour oracle. A `Drop`-bodied value never reaches here: E0514
    /// already took it out of `rc_values`.
    pub(crate) fn core_move_errors(&self) -> Vec<OwnershipError> {
        use crate::ownership::RcTrigger;
        let mut sites: Vec<&crate::ownership::RcEntry> =
            self.rc_values.values().flat_map(|m| m.values()).collect();
        sites.sort_by(|a, b| {
            a.other_use_span
                .offset
                .cmp(&b.other_use_span.offset)
                .then_with(|| a.binding.cmp(&b.binding))
        });
        let mut out = Vec::new();
        for entry in sites {
            // A moved field projection (`h.o`) is named, and cloned, as itself.
            let projection = self
                .core_projection_consumes
                .get(&SpanKey::from_span(&entry.consume_span));
            let binding = match projection {
                Some((_, text)) => text.clone(),
                None => super::demangle_binding(&entry.binding).to_string(),
            };
            let moved_span = projection.map_or(entry.consume_span, |(span, _)| *span);
            let (line, col) = (entry.consume_span.line, entry.consume_span.column);
            let message = match entry.trigger {
                RcTrigger::DirectReuseAfterConsume | RcTrigger::ContainerStoreWithSubsequentUse
                    if entry.other_use_span.offset <= entry.consume_span.offset =>
                {
                    format!(
                        "value '{binding}' is moved inside a loop and not assigned again before \
                         the next iteration (moved at line {line}:{col}; core-semantics.md §3.3)"
                    )
                }
                RcTrigger::DirectReuseAfterConsume => format!(
                    "value '{binding}' may have been moved (moved at line {line}:{col} on some \
                     path), used again here"
                ),
                RcTrigger::ContainerStoreWithSubsequentUse => format!(
                    "value '{binding}' was moved into a container (moved at line {line}:{col}), \
                     used again here"
                ),
                RcTrigger::ClosureCaptureWithOuterUse
                    if self.in_escaping_closure(&entry.consume_span) =>
                {
                    format!(
                        "value '{binding}' was moved into an escaping closure (captured at line \
                         {line}:{col}), used again here: a closure that is returned, stored, or \
                         sent to a task captures by move (core-semantics.md §9.3)"
                    )
                }
                RcTrigger::ClosureCaptureWithOuterUse => format!(
                    "value '{binding}' was moved into a closure (captured at line {line}:{col}), \
                     used again here"
                ),
            };
            // A capture site is not an expression `.clone()` can follow; the
            // clone belongs in a `let` before the closure, which is not a
            // one-span edit.
            let capture = entry.trigger == RcTrigger::ClosureCaptureWithOuterUse;
            let supports_clone = self.moved_type_supports_clone(&moved_span);
            // A local type that could be `Copy` is fixed by saying so.
            let derive_copy = if supports_clone {
                None
            } else {
                self.derive_copy_fix(&moved_span)
            };
            let replacement = if supports_clone {
                (!capture).then(|| {
                    Box::new(crate::resolver::TextEdit {
                        offset: moved_span.offset + moved_span.length,
                        length: 0,
                        replacement: ".clone()".to_string(),
                    })
                })
            } else {
                derive_copy.map(|(_, edit)| Box::new(edit.clone()))
            };
            let suggestion = if let Some((ty, edit)) = derive_copy {
                if edit.replacement.contains("Clone") {
                    let step = crate::ownership::clone_step(ty, edit);
                    format!(
                        "{step} and clone '{binding}' at the move site \
                         (`{binding}.clone()`), or restructure to avoid the reuse"
                    )
                } else {
                    format!(
                        "add `#[derive(Copy)]` to '{ty}' (its fields are all `Copy`, so \
                         '{binding}' is copied instead of moved), or restructure to avoid the reuse"
                    )
                }
            } else if capture && supports_clone {
                format!(
                    "clone '{binding}' into a new binding before the closure and capture that, \
                     or restructure to avoid the reuse"
                )
            } else if supports_clone {
                format!(
                    "clone '{binding}' at the move site (`{binding}.clone()`), declare the \
                     callee parameter `ref` if it only reads, or restructure to avoid reuse"
                )
            } else {
                format!(
                    "declare the callee parameter `ref` if it only reads, or restructure to \
                     avoid reuse ('{binding}' has no `.clone()`)"
                )
            };
            out.push(OwnershipError {
                message: crate::binding_rename::display_names(&message),
                span: entry.other_use_span,
                kind: OwnershipErrorKind::UseAfterMove,
                suggestion: Some(crate::binding_rename::display_names(&suggestion)),
                replacement,
                consume_span: Some(entry.consume_span),
            });
        }
        // C3: an element moved out of a collection that still owns it.
        let mut seen = HashSet::new();
        for span in &self.core_index_moves {
            let key = SpanKey::from_span(span);
            if self.core_non_move_sources.contains(&key) || !seen.insert(key) {
                continue;
            }
            let supports_clone = self.moved_type_supports_clone(span);
            let derive_copy = if supports_clone {
                None
            } else {
                self.derive_copy_fix(span)
            };
            let replacement = if supports_clone {
                Some(Box::new(crate::resolver::TextEdit {
                    offset: span.offset + span.length,
                    length: 0,
                    replacement: ".clone()".to_string(),
                }))
            } else {
                derive_copy.map(|(_, edit)| Box::new(edit.clone()))
            };
            let suggestion = if let Some((ty, edit)) = derive_copy {
                if edit.replacement.contains("Clone") {
                    let step = crate::ownership::clone_step(ty, edit);
                    format!(
                        "{step} and write `.clone()` to take a copy, \
                         read the element in place, or take it out with `v.swap(i, j)` / \
                         `mem.replace`"
                    )
                } else {
                    format!(
                        "add `#[derive(Copy)]` to '{ty}' (its fields are all `Copy`), read the \
                         element in place, or take it out with `v.swap(i, j)` / `mem.replace`"
                    )
                }
            } else if supports_clone {
                "write `.clone()` to take a copy, read the element in place, or take it out \
                 with `v.swap(i, j)` / `mem.replace`"
                    .to_string()
            } else {
                "read the element in place, or take it out with `v.swap(i, j)` / \
                 `mem.replace` (this type has no `.clone()`)"
                    .to_string()
            };
            out.push(OwnershipError {
                message: "cannot move a non-`Copy` value out of a collection element: the \
                          collection still owns it"
                    .to_string(),
                span: *span,
                kind: OwnershipErrorKind::MoveOutOfPlace,
                suggestion: Some(suggestion.to_string()),
                replacement,
                consume_span: None,
            });
        }
        // C3: a borrowed place handed to a method that takes `self` by value.
        let mut seen = HashSet::new();
        for span in &self.core_borrow_moves {
            let key = SpanKey::from_span(span);
            if !seen.insert(key) {
                continue;
            }
            let supports_clone = self.moved_type_supports_clone(span);
            let derive_copy = if supports_clone {
                None
            } else {
                self.derive_copy_fix(span)
            };
            let replacement = if supports_clone {
                Some(Box::new(crate::resolver::TextEdit {
                    offset: span.offset + span.length,
                    length: 0,
                    replacement: ".clone()".to_string(),
                }))
            } else {
                derive_copy.map(|(_, edit)| Box::new(edit.clone()))
            };
            let suggestion = if let Some((ty, edit)) = derive_copy {
                if edit.replacement.contains("Clone") {
                    let step = crate::ownership::clone_step(ty, edit);
                    format!(
                        "{step} and write `.clone()` to call it on a \
                         copy, or use a method that borrows"
                    )
                } else {
                    format!(
                        "add `#[derive(Copy)]` to '{ty}' (its fields are all `Copy`), or use a \
                         method that borrows"
                    )
                }
            } else if supports_clone {
                "write `.clone()` to call it on a copy, or use a method that borrows".to_string()
            } else {
                "use a method that borrows, or take the value with `mem.replace` / \
                 `Option.take()` (this type has no `.clone()`)"
                    .to_string()
            };
            out.push(OwnershipError {
                message: "cannot move a non-`Copy` value out of a borrowed place: this method \
                          takes `self` by value"
                    .to_string(),
                span: *span,
                kind: OwnershipErrorKind::MoveOutOfPlace,
                suggestion: Some(suggestion.to_string()),
                replacement,
                consume_span: None,
            });
        }
        out.sort_by_key(|e| e.span.offset);
        out
    }

    /// E0514 (B-2026-09-27-15) — reject RC fallback for a value whose type runs
    /// a user `Drop` body, and take the binding out of `rc_values` so it is
    /// reported once, as this error, rather than also as a performance note.
    ///
    /// RC fallback shares one value between the owners the dominance test
    /// found, and a `Drop` body has no shared meaning. Measured before the
    /// rejection, on `let q = mk(5); while i < 1 { let p = q; .. }` and on
    /// `if c { let p = q; .. } q.id`: every compiled surface freed the value
    /// twice (`let p = q` copies the payload out of the box and both owners
    /// free it), the call spellings (`let p = idr(q)`) ran the body once per
    /// owner, and the interpreter, which has no RC at all, ran the body at the
    /// first owner's death and then read the destroyed value through `q`.
    /// Values with no user `Drop` anywhere inside them keep RC fallback: their
    /// drop is memory only, which the box already balances on both backends.
    ///
    /// The type is read off the typechecker at the witness's two spans, since
    /// `binding_type_names` holds params only at this point. Either span
    /// suffices: the consume of `w.r` answers for a `Drop` field of `w` just as
    /// the whole binding would.
    pub(crate) fn emit_rc_fallback_drop_type_errors(&mut self) {
        let mut rejected: Vec<(String, String)> = Vec::new();
        let mut errors = Vec::new();
        let mut sites: Vec<(&String, &String, &crate::ownership::RcEntry)> = Vec::new();
        for (fn_key, rc_map) in &self.rc_values {
            for (binding, entry) in rc_map {
                sites.push((fn_key, binding, entry));
            }
        }
        // Same order as `emit_rc_fallback_notes`, for the same reason: the
        // maps are hash-ordered and the errors reach `--output=json`.
        sites.sort_by(|(ak, ab, ae), (bk, bb, be)| {
            ae.other_use_span
                .offset
                .cmp(&be.other_use_span.offset)
                .then_with(|| ak.cmp(bk))
                .then_with(|| ab.cmp(bb))
        });
        for (fn_key, binding, entry) in sites {
            let drop_type = [&entry.consume_span, &entry.other_use_span]
                .iter()
                .find_map(|sp| {
                    let ty = self
                        .typecheck_result
                        .expr_types
                        .get(&crate::resolver::SpanKey::from_span(sp))?;
                    self.type_runs_user_drop(ty, &mut Vec::new())
                        .then(|| match ty {
                            crate::typechecker::Type::Named { name, .. } => name.clone(),
                            other => crate::typechecker::type_display(other),
                        })
                });
            let Some(ty_text) = drop_type else {
                continue;
            };
            // A witness whose two sites coincide is a move inside a loop, whose
            // other use is the same site on the next iteration.
            let reuse = if entry.consume_span.offset == entry.other_use_span.offset {
                format!(
                    "'{}' is moved at line {}:{} inside a loop, so the next iteration would \
                     move it again",
                    entry.binding, entry.consume_span.line, entry.consume_span.column,
                )
            } else {
                format!(
                    "'{}' is moved at line {}:{} and used again at line {}:{} on a path the \
                     move does not cover",
                    entry.binding,
                    entry.consume_span.line,
                    entry.consume_span.column,
                    entry.other_use_span.line,
                    entry.other_use_span.column,
                )
            };
            errors.push(OwnershipError {
                message: format!(
                    "{reuse}; its type `{ty_text}` runs a user `Drop` body, so it cannot be \
                     shared by RC fallback"
                ),
                span: entry.other_use_span,
                kind: OwnershipErrorKind::RcFallbackOfDropType,
                suggestion: Some(format!(
                    "restructure so '{}' is moved on one path only: move it out of the loop, \
                     pass it by `ref` where the callee only reads it, or reassign it before \
                     the next use",
                    entry.binding
                )),
                replacement: None,
                consume_span: Some(entry.consume_span),
            });
            rejected.push((fn_key.clone(), binding.clone()));
        }
        for (fn_key, binding) in rejected {
            if let Some(map) = self.rc_values.get_mut(&fn_key) {
                map.remove(&binding);
                if map.is_empty() {
                    self.rc_values.remove(&fn_key);
                }
            }
            if let Some(set) = self.arc_values.get_mut(&fn_key) {
                set.remove(&binding);
            }
        }
        self.errors.extend(errors);
    }

    /// Does a value of `ty` run a user `Drop` body when it dies: its own, or
    /// one reachable inside it BY VALUE? The typechecker's
    /// `type_runs_user_drop`, over the tables it exports: a `shared` type is
    /// not descended into, since sharing its handle retains rather than
    /// duplicates, and `seen` breaks recursive declarations.
    /// E0514's second face (B-2026-09-28-61) — moving a payload whose type
    /// runs a user `Drop` body out of a `shared enum` BY VALUE.
    ///
    /// The object is shared, so its payload belongs to every holder, and a
    /// `Drop` body has no shared meaning, the same argument as RC fallback
    /// above. Measured before the rejection, on `match g { M.Y(x) => x }`: the
    /// compiled backends copied the payload out and ran its body once for the
    /// copy and again when the object died, and the interpreter ran it once
    /// and then read the destroyed value through `g`, for a generic and a
    /// plain `shared enum` alike. Reading the payload stays legal; moving it
    /// needs a non-shared enum. Gowtham's choice, 2026-09-28.
    ///
    /// Asked of each binding the arm's pattern introduces, after the arm body
    /// ran: a binding the body CONSUMED (`states` says `Moved`), or one that is
    /// the arm's own value (`tail`), whose type runs a `Drop` body. A binding
    /// of a `shared` type is exempt, since moving it only shares it again.
    pub(crate) fn reject_shared_enum_drop_payload_move(
        &mut self,
        scrutinee: &crate::ast::Expr,
        pattern: &crate::ast::Pattern,
        states: &std::collections::HashMap<String, crate::ownership::ValueState>,
        tail: Option<&crate::ast::Expr>,
    ) {
        use crate::typechecker::Type;
        let tc = self.typecheck_result;
        let Some(Type::Named {
            name: enum_name, ..
        }) = tc
            .expr_types
            .get(&crate::resolver::SpanKey::from_span(&scrutinee.span))
        else {
            return;
        };
        if !tc
            .enum_info
            .get(enum_name)
            .is_some_and(|i| i.is_shared || i.is_par)
        {
            return;
        }
        let enum_name = enum_name.clone();
        // The whole object bound by `v @ ..` is the shared value itself.
        let top_at = match &pattern.kind {
            crate::ast::PatternKind::AtBinding { name, .. } => Some(name.clone()),
            _ => None,
        };
        let tail_ident = tail.and_then(|t| match &t.kind {
            crate::ast::ExprKind::Identifier(n) => Some((n.clone(), t.span)),
            _ => None,
        });
        for binding in pattern.binding_names() {
            if top_at.as_deref() == Some(binding.as_str()) {
                continue;
            }
            let moved_at = match states.get(&binding) {
                Some(crate::ownership::ValueState::Moved { at }) => Some(*at),
                _ => None,
            };
            let site = moved_at.or_else(|| {
                tail_ident
                    .as_ref()
                    .filter(|(n, _)| *n == binding)
                    .map(|(_, sp)| *sp)
            });
            let Some(site) = site else {
                continue;
            };
            let ty = tc
                .expr_types
                .get(&crate::resolver::SpanKey::from_span(&site))
                .or_else(|| self.binding_types.get(&binding));
            let Some(ty) = ty.cloned() else {
                continue;
            };
            if !self.type_runs_user_drop(&ty, &mut Vec::new()) {
                continue;
            }
            let ty_text = match &ty {
                Type::Named { name, .. } => name.clone(),
                other => crate::typechecker::type_display(other),
            };
            self.errors.push(OwnershipError {
                message: format!(
                    "'{binding}' is moved out of the `shared enum {enum_name}` here; its type \
                     `{ty_text}` runs a user `Drop` body, so it cannot be moved out of a \
                     shared value"
                ),
                span: site,
                kind: OwnershipErrorKind::RcFallbackOfDropType,
                suggestion: Some(format!(
                    "read '{binding}' in place (field reads and `ref` parameters are fine), \
                     or make `{enum_name}` a non-shared enum so the payload can be moved"
                )),
                replacement: None,
                consume_span: Some(site),
            });
        }
    }

    fn type_runs_user_drop(&self, ty: &crate::typechecker::Type, seen: &mut Vec<String>) -> bool {
        use crate::typechecker::{Type, VariantTypeInfo};
        let tc = self.typecheck_result;
        match ty {
            Type::Named { name, args } => {
                let shared = tc
                    .struct_info
                    .get(name)
                    .map(|i| i.is_shared || i.is_par)
                    .or_else(|| tc.enum_info.get(name).map(|i| i.is_shared || i.is_par))
                    .unwrap_or(false);
                if shared {
                    return false;
                }
                if args.iter().any(|a| self.type_runs_user_drop(a, seen)) {
                    return true;
                }
                if tc.drop_method_keys.contains_key(name) {
                    return true;
                }
                if seen.iter().any(|s| s == name) {
                    return false;
                }
                seen.push(name.clone());
                if let Some(info) = tc.struct_info.get(name) {
                    return info
                        .fields
                        .iter()
                        .any(|(_, t, _)| self.type_runs_user_drop(t, seen));
                }
                if let Some(info) = tc.enum_info.get(name) {
                    return info.variants.iter().any(|(_, v)| match v {
                        VariantTypeInfo::Unit => false,
                        VariantTypeInfo::Tuple(ts) => {
                            ts.iter().any(|t| self.type_runs_user_drop(t, seen))
                        }
                        VariantTypeInfo::Struct(fs) => {
                            fs.iter().any(|(_, t)| self.type_runs_user_drop(t, seen))
                        }
                    });
                }
                false
            }
            Type::Tuple(elems) => elems.iter().any(|e| self.type_runs_user_drop(e, seen)),
            Type::Array { element, .. } => self.type_runs_user_drop(element, seen),
            _ => false,
        }
    }

    /// `E_RC_FALLBACK_ALLOCATES_UNDER_FALLIBLE_PROFILE` (phase-8-stdlib-floor
    /// item 6). Under `panic_on_alloc_failure = false`, every recorded RC
    /// fallback site (`rc_values`) becomes a hard error: the compiler would emit
    /// `Rc.new(...)` / `Arc.new(...)`, which may panic on OOM, and there is no
    /// fallible form it can synthesize. A pure profile-flag-gated transformation
    /// of the existing records — no new dataflow. Unlike the perf note, this is
    /// *not* silenced by `#[allow(rc_fallback)]`: accepting the RC for
    /// performance is a different decision than accepting an OOM panic, so every
    /// fallback site is reported regardless of the suppression set. No-op in the
    /// default mode.
    pub(crate) fn emit_rc_fallback_fallible_profile_errors(&mut self) {
        if self.panic_on_alloc_failure {
            return;
        }
        let mut errors = Vec::new();
        for (fn_key, rc_map) in &self.rc_values {
            let arc_set = self.arc_values.get(fn_key);
            for (binding, entry) in rc_map {
                let ctor = if arc_set.is_some_and(|s| s.contains(binding)) {
                    "Arc"
                } else {
                    "Rc"
                };
                errors.push(OwnershipError {
                    message: format!(
                        "auto-RC fallback for '{}' at this site emits '{ctor}.new(...)' which may \
                         panic on allocation failure; restructure to remove the fallback (a \
                         `shared struct`/`shared enum`, a lifetime adjustment, or an ownership \
                         reshape into a single ownership path) or use '{ctor}.try_new(...)?' with \
                         explicit '?'-propagation",
                        entry.binding,
                    ),
                    span: entry.other_use_span,
                    kind: OwnershipErrorKind::RcFallbackAllocatesUnderFallibleProfile,
                    suggestion: Some(
                        "remove the RC fallback (single ownership path / `shared` type / lifetime \
                         adjustment) or switch to explicit `try_new(...)?`"
                            .to_string(),
                    ),
                    replacement: None,
                    consume_span: Some(entry.consume_span),
                });
            }
        }
        self.errors.extend(errors);
    }

    /// Find a closure inside function `fn_key` whose whole-root
    /// capture reasons name `binding`. Returns the closure span and
    /// its reason for that root — used by `emit_rc_fallback_notes`
    /// to enrich the N0503 note with the slice-6 explanation. When
    /// multiple closures in the same function whole-root capture the
    /// same binding, the first encountered wins; the user typically
    /// only needs one explanation to understand the pattern. Per
    /// design.md § Rule 2¼ Interaction with Rule 2½, the closure's
    /// capture reason is the spec-mandated "the body called method
    /// `…` on `…`" explanation that completes the N0503 note's
    /// `direct re-use after consume` framing.
    fn find_whole_root_closure_reason(
        &self,
        fn_key: &str,
        binding: &str,
    ) -> Option<(super::Span, super::WholeRootCaptureReason)> {
        for (closure_key, reasons) in &self.whole_root_capture_reasons {
            if self.closure_function.get(closure_key).map(String::as_str) != Some(fn_key) {
                continue;
            }
            if let Some(reason) = reasons.get(binding) {
                if let Some(span) = self.closure_spans.get(closure_key) {
                    return Some((*span, reason.clone()));
                }
            }
        }
        None
    }

    /// Spec-mandated fix-it tail for slice 6: a one-liner steering
    /// the user toward the rewrite that breaks the whole-root
    /// capture. Method calls: hoist the field access outside the
    /// call. Index / Deref: hoist the indexed / dereffed value into
    /// a local. By-value pass: lift the projection out of the call
    /// arg. BareIdentifier has no rewrite — the closure body
    /// *intentionally* names the whole binding — so we fall back to
    /// the generic "accept the RC" framing.
    fn slice6_fix_it_suggestion(reason: &super::WholeRootCaptureReason) -> String {
        match reason {
            super::WholeRootCaptureReason::MethodCall { method_name, .. } => format!(
                "hoist the call outside the closure (assign `{}`'s result to a local before the closure) so the closure body captures only the fields it actually reads",
                method_name,
            ),
            super::WholeRootCaptureReason::Index { .. } => {
                "hoist the indexed element into a local before the closure so the closure body captures only the element, not the whole collection".to_string()
            }
            super::WholeRootCaptureReason::Deref { .. } => {
                "hoist the dereferenced value into a local before the closure so the closure body captures the pointee directly".to_string()
            }
            super::WholeRootCaptureReason::ByValuePass { .. } => {
                "if only specific fields are needed, project them into locals before the closure and pass those locals instead of the whole binding".to_string()
            }
            super::WholeRootCaptureReason::BareIdentifier => {
                "the closure body directly names the whole binding; accept the RC and silence with #[allow(rc_fallback)]".to_string()
            }
        }
    }

    // ── Phase 2: Rc → Arc Promotion ─────────────────────────────

    /// For each function with RC bindings, walk its body looking for any
    /// use of those bindings that lies inside a `par {}` block. Each
    /// such binding is promoted from Rc to Arc.
    ///
    /// Conservative: a binding whose live range overlaps any parallel
    /// region is Arc for its entire live range (one decision per value,
    /// matching design.md § Rc vs Arc — Two-Phase Algorithm).
    pub(crate) fn promote_rc_to_arc(&mut self) {
        let items: &[Item] = &self.program.items;
        for item in items {
            match item {
                Item::Function(f) => {
                    let params = collect_channel_param_types(&f.params);
                    self.promote_for_function(&f.name, None, &params, &f.body);
                }
                Item::ImplBlock(imp) => {
                    let type_name = match &imp.target_type.kind {
                        TypeKind::Path(p) => p.segments.last().cloned().unwrap_or_default(),
                        _ => continue,
                    };
                    for item in &imp.items {
                        if let ImplItem::Method(method) = item {
                            let params = collect_channel_param_types(&method.params);
                            self.promote_for_function(
                                &method.name,
                                Some(&type_name),
                                &params,
                                &method.body,
                            );
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn promote_for_function(
        &mut self,
        fn_name: &str,
        impl_type: Option<&str>,
        params: &[(String, String)],
        body: &Block,
    ) {
        let fn_key = match impl_type {
            Some(t) => format!("{}.{}", t, fn_name),
            None => fn_name.to_string(),
        };
        let Some(rc_map) = self.rc_values.get(&fn_key) else {
            return;
        };
        let candidates: HashSet<String> = rc_map.keys().cloned().collect();
        if candidates.is_empty() {
            return;
        }
        let mut promoted: HashSet<String> = HashSet::new();
        // Round 12.34 (Step 6): per-function map from closure-binding name
        // to its capture names, populated as the par-walker traverses
        // `let pat = closure_expr;` forms. A subsequent par-region use of
        // the closure binding promotes each capture present in
        // `candidates` to Arc, per design.md § Closures Rule 2's
        // "live range of closure value = live range of each capture for
        // the escape sub-case". Sourced from `self.closure_captures`
        // (round 12.24); only the names are needed downstream.
        let mut closure_bindings: HashMap<String, Vec<String>> = HashMap::new();
        // Theme 2 (wip-list2, 2026-05-08): per-function `let_types` map
        // tracking each binding's structurally-recovered type name —
        // currently only `Sender` / `Receiver` for the channel-send
        // boundary. Seeded from the function's parameters and grown as
        // the walker traverses `let` forms with `Sender[T]` / `Receiver[T]`
        // annotations or `Channel.new()` destructures.
        let mut let_types: HashMap<String, String> = HashMap::new();
        for (name, type_name) in params {
            let_types.insert(name.clone(), type_name.clone());
        }
        scan_block_for_par_uses(
            body,
            false,
            &candidates,
            &self.closure_captures,
            &mut closure_bindings,
            &mut let_types,
            &mut promoted,
        );
        if !promoted.is_empty() {
            self.arc_values.insert(fn_key, promoted);
        }
    }

    // ── #[no_rc] / @no_rc Enforcement ──────────────────────────

    pub(crate) fn enforce_no_rc_attrs(&mut self) {
        // Collect strict-no-rc functions
        let mut strict_fns: Vec<(String, Span)> = Vec::new();
        let mut no_rc_types: HashSet<String> = HashSet::new();

        for item in &self.program.items {
            match item {
                Item::Function(f) if has_attr(&f.attributes, "no_rc") => {
                    strict_fns.push((f.name.clone(), f.span));
                }
                Item::ImplBlock(imp) => {
                    let type_name = match &imp.target_type.kind {
                        TypeKind::Path(p) => p.segments.last().cloned().unwrap_or_default(),
                        _ => continue,
                    };
                    for it in &imp.items {
                        if let ImplItem::Method(m) = it {
                            if has_attr(&m.attributes, "no_rc") {
                                strict_fns.push((format!("{}.{}", type_name, m.name), m.span));
                            }
                        }
                    }
                }
                Item::StructDef(s) if s.no_rc => {
                    no_rc_types.insert(s.name.clone());
                }
                _ => {}
            }
        }

        // #[no_rc] on a function: any RC binding is an error.
        for (fn_key, fn_span) in &strict_fns {
            if let Some(rc_map) = self.rc_values.get(fn_key) {
                for (binding, entry) in rc_map {
                    self.errors.push(OwnershipError {
                        message: format!(
                            "function '{}' is #[no_rc] but value '{}' would require RC fallback ({})",
                            fn_key,
                            binding,
                            entry.trigger.label(),
                        ),
                        span: entry.other_use_span,
                        kind: OwnershipErrorKind::NoRcViolation,
                        suggestion: Some(format!(
                            "restructure '{}' so that consume and reuse lie on a single ownership path, or remove #[no_rc]",
                            binding
                        )),
                        replacement: None,
                        consume_span: None,
                    });
                }
                let _ = fn_span; // span available if we want to attach a secondary later
            }
        }

        // @no_rc on a struct: any RC binding of that type is an error.
        for rc_map in self.rc_values.values() {
            for (binding, entry) in rc_map {
                let Some(ty) = &entry.type_name else { continue };
                if no_rc_types.contains(ty) {
                    self.errors.push(OwnershipError {
                        message: format!(
                            "type '{}' is declared @no_rc but value '{}' would require RC fallback ({})",
                            ty,
                            binding,
                            entry.trigger.label(),
                        ),
                        span: entry.other_use_span,
                        kind: OwnershipErrorKind::NoRcViolation,
                        suggestion: Some(format!(
                            "restructure to keep '{}' on a single ownership path, or drop @no_rc on '{}'",
                            binding, ty
                        )),
                        replacement: None,
                        consume_span: None,
                    });
                }
            }
        }
    }

    // ── Module-level `#![rc_budget(max: N)]` Enforcement ─────────
    //
    // Phase-7 line 43. The author declares a per-module ceiling on RC-
    // promoted bindings via `#![rc_budget(max: N)]` at the top of the
    // source. After Phase 1+2 land (`rc_values` populated, with Phase 2
    // promotion folded in), count the total RC bindings and emit one
    // `RcBudgetExceeded` error if the count exceeds the budget. The
    // diagnostic lists every contributing `<function>.<binding>` so
    // the author knows which one to restructure first.
    pub(crate) fn enforce_rc_budget(&mut self) {
        // Find the `#![rc_budget(max: N)]` attribute, if any. Bare-
        // form `#![rc_budget]` with no `max` arg is treated as absent
        // for v1 (a future slice could land a default ceiling).
        let Some(attr) = self
            .program
            .inner_attrs
            .iter()
            .find(|a| a.path.len() == 1 && a.path[0] == "rc_budget")
        else {
            return;
        };
        let Some(max) = parse_rc_budget_max(attr) else {
            return;
        };

        // Count every RC binding occurrence across every function.
        // Different functions may carry a binding of the same name;
        // each is its own RC instance and contributes to the budget.
        let mut contributing: Vec<String> = Vec::new();
        for (fn_name, rc_map) in &self.rc_values {
            for binding in rc_map.keys() {
                contributing.push(format!("{}.{}", fn_name, binding));
            }
        }
        contributing.sort();

        if contributing.len() > max {
            self.errors.push(OwnershipError {
                message: format!(
                    "module `#![rc_budget(max: {})]` exceeded: {} RC binding(s) inferred",
                    max,
                    contributing.len(),
                ),
                span: attr.span,
                kind: OwnershipErrorKind::RcBudgetExceeded {
                    budget: max,
                    observed: contributing.len(),
                },
                suggestion: Some(format!(
                    "RC-promoted bindings (restructure or raise the budget): {}",
                    contributing.join(", "),
                )),
                replacement: None,
                consume_span: None,
            });
        }
    }
}

/// Parse the `max: N` named argument from a `#![rc_budget(max: N)]`
/// attribute. Returns `None` when the attribute is missing the named
/// argument or the value isn't a non-negative integer literal — the
/// caller treats the absence as "no budget enforced for this module."
fn parse_rc_budget_max(attr: &Attribute) -> Option<usize> {
    let arg = attr
        .args
        .iter()
        .find(|a| a.name.as_deref() == Some("max"))?;
    let value = arg.value.as_ref()?;
    let ExprKind::Integer(n, _) = &value.kind else {
        return None;
    };
    usize::try_from(*n).ok()
}
