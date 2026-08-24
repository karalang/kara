//! Call-site effect subtyping check (Phase D).
//!
//! Verifies that every Fn-typed argument's actual effect set is a
//! subset of the callee's declared `with [effects...]` constraint
//! on that parameter slot. Drives E0404 / E0410 diagnostics with
//! a structured subtype-trace.
//!
//! Houses `check_call_site_subtyping` (the driver) and the
//! three-way body walker (`check_subtyping_in_block`,
//! `check_subtyping_in_stmt`, `check_subtyping_in_expr`)
//! plus the per-call-args check (`check_call_args_subtyping`).
//!
//! Lives in a sibling `impl<'a> super::EffectChecker<'a>` block.

use std::collections::HashSet;

use crate::ast::*;
use crate::intern::Symbol;
use crate::resolver::SpanKey;
use crate::token::Span;

use super::{
    format_monomorphized_signature, verb_name, Effect, EffectError, EffectErrorKind,
    EffectSubtypeTrace,
};

/// The `subject` string for a binding's diagnostic. Shared so the `let`,
/// `let else` and assignment positions all name the binding the same way.
fn binding_subject(pattern: &Pattern) -> String {
    match &pattern.kind {
        PatternKind::Binding(name) => format!("binding `{name}`"),
        _ => "binding".to_string(),
    }
}

/// Record a simple binding's declared type so a later assignment to it can
/// find the slot. Only `PatternKind::Binding` — a destructuring pattern
/// binds no name whose annotation is the whole `TypeExpr`, and guessing
/// which sub-type belongs to which name is how a false positive gets in.
fn remember_slot(w: &mut SubtypingWalk, pattern: &Pattern, ty: &TypeExpr) {
    if let PatternKind::Binding(name) = &pattern.kind {
        w.slots.push((name.clone(), ty.clone()));
    }
}

/// Walk-scoped state for the Fn-slot subtyping pass (B-2026-08-24-1).
///
/// Held in a parameter rather than on [`super::EffectChecker`] because it
/// is per-BODY, not per-program: a field would have to be reset at every
/// body boundary, and forgetting that is a cross-function false positive —
/// the one failure mode this family must not acquire.
#[derive(Default)]
struct SubtypingWalk {
    /// Locals whose DECLARATION carried an `Fn(..)` annotation, innermost
    /// last, so a later `f = save;` can find the slot the annotation named.
    /// A stack, not a flat map: an inner `f: Fn() with writes` must not
    /// outlive its block and reject an outer, legitimate `f = save`.
    slots: Vec<(String, TypeExpr)>,
    /// The enclosing function's declared return type, for the tail
    /// expression and every `return`.
    return_ty: Option<TypeExpr>,
}

impl SubtypingWalk {
    /// Innermost-wins, matching Kāra's shadowing.
    fn slot(&self, name: &str) -> Option<&TypeExpr> {
        self.slots
            .iter()
            .rev()
            .find(|(n, _)| n == name)
            .map(|(_, t)| t)
    }
}

impl<'a> super::EffectChecker<'a> {
    pub(crate) fn check_call_site_subtyping(&mut self) {
        let bodies: Vec<super::FnHandle> = self
            .function_bodies
            .values()
            .cloned()
            .chain(self.method_bodies.values().cloned())
            .collect();
        for f in bodies {
            // `FnHandle` derefs to the decl, so the declared return type is
            // in hand at the top of every walk — that is what makes the
            // RETURN-position slot (B-2026-08-24-1 case 3) cheap.
            let mut w = SubtypingWalk {
                slots: Vec::new(),
                return_ty: f.return_type.clone(),
            };
            // The body's TAIL is a return position too, and the commoner
            // spelling: `fn get() -> Fn(i64) -> i64 { save }` has no
            // `return` node at all. Done here rather than in
            // `check_subtyping_in_block` because only the FUNCTION body's
            // tail returns — a nested block's tail is just a value.
            if let (Some(ret), Some(tail)) = (w.return_ty.clone(), f.body.final_expr.as_ref()) {
                self.check_return_tail_subtyping(&ret, tail);
            }
            self.check_subtyping_in_block(&f.body, &mut w);
        }
    }

    /// Check a function body's tail expression against the declared return
    /// type, following the tail through the forms that have one
    /// (B-2026-08-24-1 case 3).
    ///
    /// `if` / `match` / `if let` recurse into their branch tails, because
    /// `fn get() -> Fn(i64) -> i64 { if c { save } else { pure_fn } }`
    /// returns from either arm and only one of them may be a lie.
    ///
    /// `ExprKind::Return` is deliberately NOT followed: the walker's own
    /// `Return` arm checks it, and following it here too would report the
    /// same violation twice for `fn get() -> ... { return save; }`, whose
    /// body tail IS the return node.
    fn check_return_tail_subtyping(&mut self, ret: &TypeExpr, tail: &Expr) {
        match &tail.kind {
            ExprKind::Return(_) => {}
            ExprKind::Block(b) | ExprKind::LabeledBlock { body: b, .. } => {
                if let Some(inner) = b.final_expr.as_ref() {
                    self.check_return_tail_subtyping(ret, inner);
                }
            }
            ExprKind::If {
                then_block,
                else_branch,
                ..
            } => {
                if let Some(inner) = then_block.final_expr.as_ref() {
                    self.check_return_tail_subtyping(ret, inner);
                }
                if let Some(e) = else_branch {
                    self.check_return_tail_subtyping(ret, e);
                }
            }
            ExprKind::IfLet {
                then_block,
                else_branch,
                ..
            } => {
                if let Some(inner) = then_block.final_expr.as_ref() {
                    self.check_return_tail_subtyping(ret, inner);
                }
                if let Some(e) = else_branch {
                    self.check_return_tail_subtyping(ret, e);
                }
            }
            ExprKind::Match { arms, .. } => {
                for arm in arms {
                    self.check_return_tail_subtyping(ret, &arm.body);
                }
            }
            _ => self.check_binding_annotation_subtyping(ret, "returned value", tail),
        }
    }

    fn check_subtyping_in_block(&mut self, block: &Block, w: &mut SubtypingWalk) {
        let saved = w.slots.len();
        for stmt in &block.stmts {
            self.check_subtyping_in_stmt(stmt, w);
        }
        if let Some(expr) = &block.final_expr {
            self.check_subtyping_in_expr(expr, w);
        }
        w.slots.truncate(saved);
    }

    fn check_subtyping_in_stmt(&mut self, stmt: &Stmt, w: &mut SubtypingWalk) {
        match &stmt.kind {
            StmtKind::MultiAssign { .. } => unreachable!(
                "StmtKind::MultiAssign is removed by the desugar pass before reaching this phase"
            ),
            StmtKind::Let {
                ty, pattern, value, ..
            } => {
                if let Some(ty) = ty {
                    self.check_binding_annotation_subtyping(ty, &binding_subject(pattern), value);
                    remember_slot(w, pattern, ty);
                }
                self.check_subtyping_in_expr(value, w);
            }
            // `let f: Fn(..);` has no value to check against the slot HERE —
            // but it declares one, and the later `f = save;` fills it
            // (B-2026-08-24-1 case 2). Recording the annotation is the whole
            // of that case: the `Assign` arm below does the comparing.
            StmtKind::LetUninit { name, ty, .. } => {
                // `ty` is mandatory on this form — an uninitialised binding
                // has nothing to infer from — so there is no `Option` here.
                w.slots.push((name.clone(), ty.clone()));
            }
            StmtKind::LetElse {
                ty,
                pattern,
                value,
                else_block,
            } => {
                if let Some(ty) = ty {
                    self.check_binding_annotation_subtyping(ty, &binding_subject(pattern), value);
                    remember_slot(w, pattern, ty);
                }
                self.check_subtyping_in_expr(value, w);
                self.check_subtyping_in_block(else_block, w);
            }
            StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
                self.check_subtyping_in_block(body, w);
            }
            StmtKind::Assign { target, value } | StmtKind::CompoundAssign { target, value, .. } => {
                // ASSIGNMENT to a declared-`Fn` binding (B-2026-08-24-1 cases
                // 1 and 2). The annotation is on the DECLARATION, not here, so
                // the slot has to be looked up — hence the scope stack.
                if let ExprKind::Identifier(name) = &target.kind {
                    if let Some(slot) = w.slot(name).cloned() {
                        self.check_binding_annotation_subtyping(
                            &slot,
                            &format!("binding `{name}`"),
                            value,
                        );
                    }
                }
                self.check_subtyping_in_expr(target, w);
                self.check_subtyping_in_expr(value, w);
            }
            StmtKind::Expr(expr) => self.check_subtyping_in_expr(expr, w),
        }
    }

    /// Per-argument Fn-slot subtyping check, shared between `Call` and
    /// `MethodCall` arms of `check_subtyping_in_expr`. Resolves the
    /// callee's parameter list (via `function_bodies` or `method_bodies`)
    /// and emits `EffectSubtypeViolation` for any function-valued argument
    /// whose effect set exceeds its slot's declared effects.
    ///
    /// `args` indices align with `params` indices for both call shapes:
    /// method-call `args` exclude the receiver, and `method_bodies[k].params`
    /// excludes `self_param` (it is stored separately).
    ///
    /// `call_span` is the span of the call expression itself (not the args).
    /// Used to look up `call_type_subs` so the diagnostic can render a fully
    /// monomorphized callee signature when the call is generic.
    fn check_call_args_subtyping(
        &mut self,
        callee_name: Symbol,
        args: &[CallArg],
        call_span: &Span,
    ) {
        let params = self
            .function_bodies
            .get(&callee_name)
            .map(|f| f.params.clone())
            .or_else(|| {
                self.method_bodies
                    .get(&callee_name)
                    .map(|f| f.params.clone())
            });
        let Some(params) = params else {
            return;
        };
        let return_type = self
            .function_bodies
            .get(&callee_name)
            .map(|f| f.return_type.clone())
            .or_else(|| {
                self.method_bodies
                    .get(&callee_name)
                    .map(|f| f.return_type.clone())
            })
            .flatten();
        // Per-call bindings for `with E` slots: each named effect variable is
        // resolved to the union of effects supplied at every parameter
        // position that references it. A slot typed `Fn(...) with E` is then
        // checked against this concrete set rather than the empty set.
        // Round 9's unification check separately diagnoses disagreement
        // between positions.
        let var_bindings = self.compute_call_var_bindings(callee_name, args);
        // Phase 6 line 26 slice 8aa: persist the per-call-site
        // effect-variable substitutions into `call_effect_subs` so
        // slice 8ab can forward them to codegen (and slice 8y can
        // gate state-machine emission on whether the resolved
        // effects include any network-yield verb). Only record when
        // the callee has effect variables (`compute_call_var_bindings`
        // returns an empty map for non-polymorphic callees) — the
        // absence of an entry signals "no polymorphic-effect to
        // resolve" downstream, distinct from "resolved to ⊥".
        if !var_bindings.is_empty() {
            self.call_effect_subs
                .insert(SpanKey::from_span(call_span), var_bindings.clone());
        }
        // Look up type-parameter substitutions for this call (Round 10.3
        // step 7). Empty when the callee is non-generic or the typechecker
        // didn't run with `with_call_type_subs` wired in.
        let type_subs = self
            .call_type_subs
            .get(&SpanKey::from_span(call_span))
            .cloned()
            .unwrap_or_default();
        for (i, call_arg) in args.iter().enumerate() {
            let Some(param) = params.get(i) else {
                continue;
            };
            let slot_effects = match &param.ty.kind {
                TypeKind::FnType {
                    effect_spec: Some(EffectSpec::Polymorphic),
                    ..
                } => continue,
                TypeKind::FnType {
                    effect_spec: Some(EffectSpec::Specific(list)),
                    ..
                } => self.resolve_effect_list_to_set(list, Some(&var_bindings)),
                TypeKind::FnType {
                    effect_spec: None, ..
                } => HashSet::new(),
                _ => continue,
            };
            let arg_effects = self.get_arg_effects(&call_arg.value);
            let arg_span = call_arg.value.span;

            // Pre-compute trace fields shared across all E0404 errors for
            // this argument position (slot / argument / offending sets).
            // Sorted for the same reason the binding-annotation path sorts:
            // `slot_effects` is a `HashSet`, so a slot declaring two or more
            // effects rendered them in a per-process-random order and the
            // shipped message differed between runs of one binary.
            let mut slot_str: Vec<String> = slot_effects
                .iter()
                .map(|e| format!("{}({})", verb_name(&e.verb), e.resource))
                .collect();
            slot_str.sort();
            let arg_str: Vec<String> = arg_effects
                .effects
                .iter()
                .filter(|te| !self.is_slot_transparent_verb(&te.effect.verb))
                .map(|te| format!("{}({})", verb_name(&te.effect.verb), te.effect.resource))
                .collect();
            let offending_str: Vec<String> = arg_effects
                .effects
                .iter()
                .filter(|te| {
                    !self.is_slot_transparent_verb(&te.effect.verb)
                        && !slot_effects.contains(&te.effect)
                })
                .map(|te| format!("{}({})", verb_name(&te.effect.verb), te.effect.resource))
                .collect();

            // Render the monomorphized callee signature (Round 10.3 step 7).
            // Only emitted when the callee has at least one type parameter
            // for which a substitution is known — otherwise it would just be
            // a verbose echo of the source.
            let monomorphized = if type_subs.is_empty() && var_bindings.is_empty() {
                None
            } else {
                Some(format_monomorphized_signature(
                    &self.interner.resolve(callee_name),
                    &params,
                    return_type.as_ref(),
                    &type_subs,
                    &var_bindings,
                ))
            };

            for te in &arg_effects.effects {
                let is_transparent = self.is_slot_transparent_verb(&te.effect.verb);
                if !slot_effects.contains(&te.effect) && !is_transparent {
                    let effect_str =
                        format!("{}({})", verb_name(&te.effect.verb), te.effect.resource);
                    let mut message = format!(
                        "argument {} has effect {} not declared in slot [{}]",
                        i + 1,
                        effect_str,
                        if slot_str.is_empty() {
                            "pure".to_string()
                        } else {
                            slot_str.join(", ")
                        },
                    );
                    if let Some(ref sig) = monomorphized {
                        message.push_str(&format!("; callee: {sig}"));
                    }
                    self.errors.push(EffectError {
                        message,
                        span: arg_span,
                        kind: EffectErrorKind::EffectSubtypeViolation,
                        subtype_trace: Some(EffectSubtypeTrace {
                            slot_effects: slot_str.clone(),
                            argument_effects: arg_str.clone(),
                            offending_effects: offending_str.clone(),
                            monomorphized_signature: monomorphized.clone(),
                        }),
                        replacement: None,
                    });
                }
            }
        }
    }

    /// Effects admitted by an explicit `Fn(..)` / `OnceFn(..)` type annotation.
    ///
    /// `Some(set)` when the annotation is a function type whose effect clause
    /// resolves to a concrete set at this site -- an ABSENT clause reading as
    /// the empty (pure) set, the same way the argument-position rule reads an
    /// unannotated slot (`test_subtype_unannotated_slot_treated_as_pure`).
    ///
    /// `None` -- meaning "no check here" -- in three cases:
    ///
    ///   * `ty` is not a function type at all, so there is no slot.
    ///   * `with _` ([`EffectSpec::Polymorphic`]) admits any effects by
    ///     definition, exactly as the argument path `continue`s on it.
    ///   * the clause names an effect VARIABLE (`with E`). Those are bound per
    ///     CALL SITE by `compute_call_var_bindings`, which needs a callee
    ///     symbol and an argument list; a binding annotation has neither. The
    ///     alternative -- resolving `E` against no bindings -- yields the EMPTY
    ///     set and would reject every effectful value assigned to a
    ///     legitimately polymorphic slot, so this fails open.
    fn fn_annotation_slot_effects(&self, ty: &TypeExpr) -> Option<HashSet<Effect>> {
        let TypeKind::FnType { effect_spec, .. } = &ty.kind else {
            return None;
        };
        match effect_spec {
            Some(EffectSpec::Polymorphic) => None,
            Some(EffectSpec::Specific(list)) => {
                let has_unresolvable_var = list
                    .items
                    .iter()
                    .any(|i| matches!(i, EffectItem::Variable(_) | EffectItem::Polymorphic));
                if has_unresolvable_var {
                    return None;
                }
                Some(self.resolve_effect_list_to_set(list, None))
            }
            None => Some(HashSet::new()),
        }
    }

    /// Is `verb` ignored when comparing a value's effects against a declared
    /// `Fn(..)` SLOT? (B-2026-08-24-8, option (a).)
    ///
    /// The ordinary transparent set, plus `panics`. `panics` is inferred
    /// from any indexing, division or overflow-checked arithmetic, so under
    /// the old rule a closure as plain as `|w, i| w[i]` did not fit
    /// `Fn(ref Vec[u8], i64) -> u8` and had to be spelled `... with panics`.
    /// That made the annotation boilerplate on nearly every non-trivial
    /// function value — the shape users cargo-cult rather than read — and
    /// the standard library disagreed with its own rule in practice:
    /// 27 of its 33 `Fn(..)` slots are bare (`Vec.map`, `Pool.new`,
    /// `Once.get_or_init`, `autograd.grad`), and none was written with
    /// `panics`, yet an indexing callback could not fill any of them.
    ///
    /// SCOPED TO SLOTS ON PURPOSE. This does NOT make `panics` transparent
    /// anywhere else: it is still inferred, still declared on functions,
    /// still surfaced by the public-effect rule, and still reported by
    /// `karac explain`. Only the value-vs-slot comparison ignores it.
    ///
    /// `blocks` and `suspends` stay CHECKED. They are execution verbs that
    /// drive scheduler placement, so "this callback must not block" is a
    /// constraint a slot may really mean; "this callback must not panic" is
    /// not expressible in v1 anyway (design.md deliberately has no
    /// `forbids(verb)` predicate), so a bare slot was never a deliberate way
    /// to say it.
    ///
    /// PRECEDENT: the auto-par conflict rule already singles `panics` out as
    /// non-conflicting (`src/concurrency/conflicts.rs`), because a Kāra panic
    /// is a process exit rather than an unwind. Different question, same verb,
    /// same reason it behaves unlike the other five resource verbs.
    fn is_slot_transparent_verb(&self, verb: &EffectVerbKind) -> bool {
        matches!(verb, EffectVerbKind::Panics) || self.is_transparent_verb(verb)
    }

    /// A struct field's declared `TypeExpr`, by struct name and field name.
    ///
    /// Scans `self.program` rather than consulting an index: the effect
    /// checker holds no struct table (it is a call-graph pass), and the only
    /// caller is a struct literal whose field is a naked function value —
    /// rare enough that building and invalidating a table would cost more
    /// than it saves. Returns `None` for an unknown struct or field, which
    /// is the right answer for a literal the typechecker will reject anyway.
    fn struct_field_type(&self, struct_name: &str, field: &str) -> Option<TypeExpr> {
        self.program.items.iter().find_map(|item| match item {
            Item::StructDef(sd) if sd.name == struct_name => sd
                .fields
                .iter()
                .find(|f| f.name == field)
                .map(|f| f.ty.clone()),
            _ => None,
        })
    }

    /// Check a binding's `Fn(..)` annotation against the effects of the
    /// function value assigned to it (B-2026-08-23-12).
    ///
    /// An annotation is a slot in exactly the sense `check_call_args_subtyping`
    /// means one: `let f: Fn(i64) -> i64 = save;` declares `f` pure and is
    /// handed a `save` that writes. Until this check existed the IDENTICAL
    /// parameter slot -- `fn apply(f: Fn(i64) -> i64)` called as `apply(save)`
    /// -- was rejected while the binding was accepted in silence, which is the
    /// same E0404 rule applied at one site and not the other.
    ///
    /// This is a PRECISION check, not a soundness one, and deliberately so:
    /// B-2026-08-23-7 makes the mere MENTION of `save` contribute its effects
    /// to the enclosing function, so a `pub fn` still cannot hide the write
    /// (measured on every shape this walker reaches, annotated and not). What
    /// was missing was any diagnostic for the annotation itself being a lie.
    ///
    /// POSITION-AGNOSTIC: `subject` is the only thing that varies between the
    /// five slots this now serves — a `let` annotation, an assignment to a
    /// declared binding, its `LetUninit` twin, a declared return type, and a
    /// struct field (B-2026-08-24-1). Finding the right `TypeExpr` is each
    /// caller's job; comparing it is entirely here, so there is one
    /// implementation of the rule and one wording of the diagnostic.
    fn check_binding_annotation_subtyping(&mut self, ty: &TypeExpr, subject: &str, value: &Expr) {
        let Some(slot_effects) = self.fn_annotation_slot_effects(ty) else {
            return;
        };
        let value_effects = self.get_arg_effects(value);

        // Sorted, not raw `HashSet` order: the rendered slot list is user-facing
        // text, and an unsorted set makes the message vary between runs of the
        // same binary once a slot declares two or more effects.
        let mut slot_str: Vec<String> = slot_effects
            .iter()
            .map(|e| format!("{}({})", verb_name(&e.verb), e.resource))
            .collect();
        slot_str.sort();
        let value_str: Vec<String> = value_effects
            .effects
            .iter()
            .filter(|te| !self.is_slot_transparent_verb(&te.effect.verb))
            .map(|te| format!("{}({})", verb_name(&te.effect.verb), te.effect.resource))
            .collect();
        let offending_str: Vec<String> = value_effects
            .effects
            .iter()
            .filter(|te| {
                !self.is_slot_transparent_verb(&te.effect.verb)
                    && !slot_effects.contains(&te.effect)
            })
            .map(|te| format!("{}({})", verb_name(&te.effect.verb), te.effect.resource))
            .collect();

        for te in &value_effects.effects {
            if slot_effects.contains(&te.effect) || self.is_slot_transparent_verb(&te.effect.verb) {
                continue;
            }
            let effect_str = format!("{}({})", verb_name(&te.effect.verb), te.effect.resource);
            let message = format!(
                "{} has effect {} not declared in slot [{}]",
                subject,
                effect_str,
                if slot_str.is_empty() {
                    "pure".to_string()
                } else {
                    slot_str.join(", ")
                },
            );
            self.errors.push(EffectError {
                message,
                span: value.span,
                kind: EffectErrorKind::EffectSubtypeViolation,
                subtype_trace: Some(EffectSubtypeTrace {
                    slot_effects: slot_str.clone(),
                    argument_effects: value_str.clone(),
                    offending_effects: offending_str.clone(),
                    monomorphized_signature: None,
                }),
                replacement: None,
            });
        }
    }

    fn check_subtyping_in_expr(&mut self, expr: &Expr, w: &mut SubtypingWalk) {
        match &expr.kind {
            ExprKind::Call { callee, args } => {
                if let Some(cname) = self.extract_callee_name(callee) {
                    self.check_call_args_subtyping(cname, args, &expr.span);
                }
                // Recurse into callee and args
                self.check_subtyping_in_expr(callee, w);
                for arg in args {
                    self.check_subtyping_in_expr(&arg.value, w);
                }
            }
            ExprKind::Block(block) | ExprKind::Comptime(block) => {
                self.check_subtyping_in_block(block, w)
            }
            ExprKind::If {
                condition,
                then_block,
                else_branch,
            } => {
                self.check_subtyping_in_expr(condition, w);
                self.check_subtyping_in_block(then_block, w);
                if let Some(e) = else_branch {
                    self.check_subtyping_in_expr(e, w);
                }
            }
            ExprKind::IfLet {
                value,
                then_block,
                else_branch,
                ..
            } => {
                self.check_subtyping_in_expr(value, w);
                self.check_subtyping_in_block(then_block, w);
                if let Some(e) = else_branch {
                    self.check_subtyping_in_expr(e, w);
                }
            }
            ExprKind::Match { scrutinee, arms } => {
                self.check_subtyping_in_expr(scrutinee, w);
                for arm in arms {
                    if let Some(g) = &arm.guard {
                        self.check_subtyping_in_expr(g, w);
                    }
                    self.check_subtyping_in_expr(&arm.body, w);
                }
            }
            ExprKind::While {
                condition, body, ..
            } => {
                self.check_subtyping_in_expr(condition, w);
                self.check_subtyping_in_block(body, w);
            }
            ExprKind::WhileLet { value, body, .. } => {
                self.check_subtyping_in_expr(value, w);
                self.check_subtyping_in_block(body, w);
            }
            ExprKind::For { iterable, body, .. } => {
                self.check_subtyping_in_expr(iterable, w);
                self.check_subtyping_in_block(body, w);
            }
            ExprKind::Loop { body, .. }
            | ExprKind::Unsafe(body)
            | ExprKind::Try(body)
            | ExprKind::Seq(body)
            | ExprKind::Par(body) => {
                self.check_subtyping_in_block(body, w);
            }
            ExprKind::LabeledBlock { body, .. } => self.check_subtyping_in_block(body, w),
            ExprKind::Lock { body, .. } => self.check_subtyping_in_block(body, w),
            ExprKind::Closure { body, .. } => self.check_subtyping_in_expr(body, w),
            ExprKind::MethodCall { object, args, .. } => {
                // Mirror the `Call` branch: resolve to `Type.method` via the
                // typechecker side-table and run the same per-arg Fn-slot
                // subtyping check. Without this, an effectful closure could
                // satisfy a method's pure `Fn()` slot whenever the enclosing
                // caller declared the effects.
                if let Some(callee_key) = self.resolve_method_callee_key(&expr.span) {
                    self.check_call_args_subtyping(callee_key, args, &expr.span);
                }
                self.check_subtyping_in_expr(object, w);
                for arg in args {
                    self.check_subtyping_in_expr(&arg.value, w);
                }
            }
            ExprKind::Binary { left, right, .. } => {
                self.check_subtyping_in_expr(left, w);
                self.check_subtyping_in_expr(right, w);
            }
            ExprKind::Pipe { left, right } => {
                self.check_subtyping_in_expr(left, w);
                self.check_subtyping_in_expr(right, w);
            }
            ExprKind::Unary { operand, .. } => self.check_subtyping_in_expr(operand, w),
            // RETURN position (B-2026-08-24-1 case 3): the slot is the
            // enclosing function's declared return type. This is the shape
            // that hands another module a function value whose declared type
            // lies about it -- `pub fn get() -> Fn(i64) -> i64 { save }`.
            ExprKind::Return(Some(e)) => {
                if let Some(ret) = w.return_ty.clone() {
                    self.check_binding_annotation_subtyping(&ret, "returned value", e);
                }
                self.check_subtyping_in_expr(e, w)
            }
            ExprKind::Question(e) => self.check_subtyping_in_expr(e, w),
            ExprKind::Break { value: Some(e), .. } => self.check_subtyping_in_expr(e, w),
            ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
                self.check_subtyping_in_expr(object, w)
            }
            ExprKind::Index { object, index } => {
                self.check_subtyping_in_expr(object, w);
                self.check_subtyping_in_expr(index, w);
            }
            ExprKind::Tuple(exprs) => {
                for e in exprs {
                    self.check_subtyping_in_expr(e, w);
                }
            }
            ExprKind::ArrayLiteral(elems) => {
                for e in elems {
                    self.check_subtyping_in_expr(e, w);
                }
            }
            ExprKind::RepeatLiteral { value, count, .. } => {
                self.check_subtyping_in_expr(value, w);
                self.check_subtyping_in_expr(count, w);
            }
            ExprKind::PrefixCollectionLiteral { items, .. } => {
                for e in items {
                    self.check_subtyping_in_expr(e, w);
                }
            }
            ExprKind::StructLiteral {
                path,
                fields,
                spread,
                ..
            } => {
                // STRUCT-LITERAL FIELD (B-2026-08-24-1 case 4): the slot is
                // the field's declared type. Looked up by the literal's own
                // path, which is the last path segment -- `mod::Holder { .. }`
                // and `Holder { .. }` name the same struct.
                for f in fields {
                    if let Some(field_ty) = path
                        .last()
                        .and_then(|sname| self.struct_field_type(sname, &f.name))
                    {
                        let subject = format!("field `{}`", f.name);
                        self.check_binding_annotation_subtyping(&field_ty, &subject, &f.value);
                    }
                    self.check_subtyping_in_expr(&f.value, w);
                }
                if let Some(s) = spread {
                    self.check_subtyping_in_expr(s, w);
                }
            }
            ExprKind::MapLiteral(entries) => {
                for (k, v) in entries {
                    self.check_subtyping_in_expr(k, w);
                    self.check_subtyping_in_expr(v, w);
                }
            }
            ExprKind::Cast { expr: inner, .. } => self.check_subtyping_in_expr(inner, w),
            ExprKind::Range { start, end, .. } => {
                if let Some(s) = start {
                    self.check_subtyping_in_expr(s, w);
                }
                if let Some(e) = end {
                    self.check_subtyping_in_expr(e, w);
                }
            }
            ExprKind::NilCoalesce { left, right } => {
                self.check_subtyping_in_expr(left, w);
                self.check_subtyping_in_expr(right, w);
            }
            ExprKind::OptionalChain { object, args, .. } => {
                self.check_subtyping_in_expr(object, w);
                if let Some(args) = args {
                    for a in args {
                        self.check_subtyping_in_expr(&a.value, w);
                    }
                }
            }
            ExprKind::Providers { bindings, body } => {
                for b in bindings {
                    self.check_subtyping_in_expr(&b.value, w);
                }
                self.check_subtyping_in_block(body, w);
            }
            ExprKind::InterpolatedStringLit(parts) => {
                for p in parts {
                    if let ParsedInterpolationPart::Expr(e, _) = p {
                        self.check_subtyping_in_expr(e, w);
                    }
                }
            }
            // Leaf expressions — nothing to recurse into
            ExprKind::Identifier(_)
            | ExprKind::Path { .. }
            | ExprKind::SelfValue
            | ExprKind::SelfType
            | ExprKind::Integer(_, _)
            | ExprKind::Float(_, _)
            | ExprKind::CharLit(_)
            | ExprKind::ByteLit(_)
            | ExprKind::ByteStringLit(_)
            | ExprKind::StringLit(_)
            | ExprKind::MultiStringLit(_)
            | ExprKind::CStringLit { .. }
            | ExprKind::Bool(_)
            | ExprKind::Continue { .. }
            | ExprKind::Return(None)
            | ExprKind::Break { value: None, .. }
            | ExprKind::PipePlaceholder
            | ExprKind::OffsetOf { .. }
            | ExprKind::Error => {}
        }
    }
}
