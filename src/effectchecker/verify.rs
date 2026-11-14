//! Public-fn and trait-impl declaration verification.
//!
//! Houses three verification passes:
//!
//! - `verify_declarations` — for each public function, verify the
//!   declared effect list is a sound superset of the inferred set;
//!   emits `MissingEffect` / `UnnecessaryEffect` diagnostics.
//! - `verify_impl_trait_ceilings` — for each impl method that
//!   implements a trait method, verify the impl's effect set is
//!   contained within the trait method's declared ceiling.
//! - `verify_trait_default_bodies` — when a trait method ships a
//!   default body, verify that body's inferred effects fit within
//!   the trait method's declared ceiling.
//!
//! Lives in a sibling `impl<'a> super::EffectChecker<'a>` block.

use crate::ast::*;
use crate::intern::Symbol;
use crate::token::Span;

use super::{
    declared_admits, verb_name, DeclaredEffects, Effect, EffectError, EffectErrorKind,
    PublicEffectsPolicy, TracedEffect, UndeclaredEffects,
};

/// The payload of a missing-declaration error: `function` performs
/// `effects` without declaring them.
fn undeclared<'e>(
    function: &str,
    effects: impl IntoIterator<Item = &'e Effect>,
) -> Option<Box<UndeclaredEffects>> {
    Some(Box::new(UndeclaredEffects {
        function: function.to_string(),
        effects: effects.into_iter().cloned().collect(),
    }))
}

/// `effects` as a `with` clause writes them, space-separated: a resource
/// verb as `verb(Resource)`, a resourceless one (`panics`) bare, sorted.
fn clause_text(effects: &[Effect]) -> String {
    let mut parts: Vec<String> = Vec::new();
    for e in effects {
        let part = if e.resource.is_empty() {
            verb_name(&e.verb).to_string()
        } else {
            format!("{}({})", verb_name(&e.verb), e.resource)
        };
        if !parts.contains(&part) {
            parts.push(part);
        }
    }
    // The errors come from a set, so their order is not stable; the edit
    // must be.
    parts.sort();
    parts.join(" ")
}

impl<'a> super::EffectChecker<'a> {
    // ── Verification ────────────────────────────────────────────

    /// The declared return type of `sym`, but ONLY when it is an `Fn(..)`
    /// / `OnceFn(..)` type (B-2026-08-24-12).
    ///
    /// `None` for every other return type, including absent — which is
    /// what keeps the ordinary suggested-fix text unchanged. This exists
    /// solely to detect the one shape where a `with` clause would bind to
    /// the return type instead of the function.
    fn fn_return_type(&self, sym: Symbol) -> Option<TypeExpr> {
        let ret = self.function_bodies.get(&sym)?.return_type.clone()?;
        matches!(ret.kind, TypeKind::FnType { .. }).then_some(ret)
    }

    /// The edit that declares `add` on `sym`: appended to its `with`
    /// clause, or a new clause where the clause goes (after the return
    /// type, before `requires`, `where` and the body). `None` where no
    /// edit is safe: a library function, an `Fn` return type (a `with`
    /// after it binds to the type), and a clause-less function whose
    /// signature ends in `requires` / `ensures` / `where` with no return
    /// type to anchor on.
    fn with_clause_edit(&self, sym: Symbol, add: &str) -> Option<Box<crate::resolver::TextEdit>> {
        let f = self
            .function_bodies
            .get(&sym)
            .or_else(|| self.method_bodies.get(&sym))?;
        if f.stdlib_origin || add.is_empty() {
            return None;
        }
        let (offset, text) = match &f.effects {
            Some(list) => (list.span.offset + list.span.length, format!(" {add}")),
            None => {
                if matches!(
                    f.return_type.as_ref().map(|t| &t.kind),
                    Some(TypeKind::FnType { .. })
                ) {
                    return None;
                }
                if f.requires.is_empty() && f.ensures.is_empty() && f.where_clause.is_none() {
                    (f.body.span.offset, format!("with {add} "))
                } else {
                    let rt = f.return_type.as_ref()?;
                    (rt.span.offset + rt.span.length, format!(" with {add}"))
                }
            }
        };
        Some(Box::new(crate::resolver::TextEdit {
            offset,
            length: 0,
            replacement: text,
        }))
    }

    /// Give the first missing-declaration error of each function in
    /// `self.errors[from..]` the edit that declares everything its errors
    /// report (B-2026-11-05-29). One edit per function, so `karac fix`
    /// never inserts twice at the same place; `main`, whose fix is to
    /// drop its `pub`, gets none.
    fn attach_with_clause_fixes(&mut self, from: usize) {
        let mut by_function: Vec<(String, usize, Vec<Effect>)> = Vec::new();
        for (i, err) in self.errors.iter().enumerate().skip(from) {
            let Some(u) = err.undeclared.as_deref() else {
                continue;
            };
            if err.replacement.is_some() || u.function == "main" {
                continue;
            }
            match by_function.iter_mut().find(|(f, ..)| *f == u.function) {
                Some((_, _, effects)) => effects.extend(u.effects.iter().cloned()),
                None => by_function.push((u.function.clone(), i, u.effects.clone())),
            }
        }
        for (function, first, effects) in by_function {
            let sym = self.interner.intern(&function);
            let edit = self.with_clause_edit(sym, &clause_text(&effects));
            self.errors[first].replacement = edit;
        }
    }

    pub(crate) fn verify_declarations(&mut self) {
        let from = self.errors.len();
        let fn_names: Vec<Symbol> = self.function_bodies.keys().copied().collect();
        for &sym in &fn_names {
            let is_pub = self.function_visibility.get(&sym).copied().unwrap_or(false);
            if !is_pub {
                continue;
            } // Private functions don't need declarations

            // Resolved once for the diagnostics below; lookups use `sym`.
            let name = self.interner.resolve(sym);
            let declared = self.declared_effects.get(&sym);
            let inferred = self.inferred_effects.get(&sym);
            let span = self.function_spans.get(&sym).cloned().unwrap_or(Span {
                line: 0,
                column: 0,
                offset: 0,
                length: 0,
            });

            match declared {
                Some(DeclaredEffects::Polymorphic) => {
                    // `with _` (anonymous, viral): body may carry any effects —
                    // the wildcard absorbs whatever the closure brings, plus any
                    // concrete operations the body performs. Skip verification.
                    //
                    // `with E` (named, precise): body's concrete effects must
                    // come from E only (i.e. via the polymorphic parameter).
                    // The single shared E across a polymorphic SCC means any
                    // concrete leak in one member propagates to all via the
                    // fixed-point and surfaces here for every leaking member.
                    if self.fn_uses_with_underscore.contains(&sym) {
                        continue;
                    }
                    if let Some(inferred_set) = inferred {
                        for te in &inferred_set.effects {
                            if self.is_transparent_verb(&te.effect.verb) {
                                continue;
                            }
                            if self.is_default_permitted_effect(&te.effect) {
                                continue;
                            }
                            // Synthetic per-binding resources (slice 6 / §1322)
                            // cannot appear in a user-written `with ...` clause
                            // — they're project-internal identifiers. The
                            // generic "add it to the declaration" fix-it would
                            // be wrong; slice 8's dedicated rejection
                            // (`verify_pub_fn_no_synthetic_resource`) owns the
                            // diagnostic for these effects.
                            if self.is_synthetic_modbind_resource(&te.effect.resource) {
                                continue;
                            }
                            // Effects whose origin is a polymorphic callee
                            // (declared `with _` / `with E`, or transitively
                            // poly via `calls_polymorphic`) are contributed
                            // through E, not as concrete body leaks. Trait
                            // dispatch through a typeparam bound (e.g.,
                            // `T.method()` where `T: Processor` and
                            // `Processor.method` is `with _`) routes through
                            // this branch — the design.md `run[T: Processor,
                            // with E]` example would otherwise false-positive.
                            if self.effect_came_via_polymorphic_callee(te, sym) {
                                continue;
                            }
                            let origin_msg = self.format_effect_origin(sym, &te.effect);
                            self.errors.push(EffectError {
                                message: format!(
                                    "public function '{}' is declared `with E` (purely \
                                     polymorphic) but performs {}({}){}; add it to the \
                                     declaration as `with E {}({})` or remove the call",
                                    name,
                                    verb_name(&te.effect.verb),
                                    te.effect.resource,
                                    origin_msg,
                                    verb_name(&te.effect.verb),
                                    te.effect.resource,
                                ),
                                span,
                                kind: EffectErrorKind::MissingEffectDeclaration,
                                subtype_trace: None,
                                replacement: None,
                                undeclared: undeclared(&name, [&te.effect]),
                            });
                        }
                    }
                    continue;
                }
                Some(DeclaredEffects::PolymorphicWithFixed(fixed)) => {
                    // `with _ + fixed`: any `_` makes the declaration viral —
                    // body may carry effects beyond `fixed`. Skip.
                    //
                    // `with E + fixed`: body's concrete effects must be ⊆ fixed
                    // (E is symbolic and resolves at the call site; only the
                    // fixed part licenses concrete body effects).
                    if self.fn_uses_with_underscore.contains(&sym) {
                        continue;
                    }
                    let fixed_set = fixed.effect_set();
                    if let Some(inferred_set) = inferred {
                        for te in &inferred_set.effects {
                            if self.is_transparent_verb(&te.effect.verb) {
                                continue;
                            }
                            if self.is_default_permitted_effect(&te.effect) {
                                continue;
                            }
                            // Synthetic modbind resources are owned by
                            // slice 8's dedicated rejection — skip here so
                            // we don't double-fire missing-declaration.
                            if self.is_synthetic_modbind_resource(&te.effect.resource) {
                                continue;
                            }
                            // Same poly-origin filter as the pure `with E`
                            // arm above — effects propagated through a
                            // polymorphic callee belong to E, not to the
                            // fixed part of the declaration.
                            if self.effect_came_via_polymorphic_callee(te, sym) {
                                continue;
                            }
                            if !declared_admits(&fixed_set, &te.effect) {
                                let origin_msg = self.format_effect_origin(sym, &te.effect);
                                self.errors.push(EffectError {
                                    message: format!(
                                        "public function '{}' performs {}({}){} but it is not \
                                         in the fixed part of its `with E ...` declaration; \
                                         add {}({}) to the declaration",
                                        name,
                                        verb_name(&te.effect.verb),
                                        te.effect.resource,
                                        origin_msg,
                                        verb_name(&te.effect.verb),
                                        te.effect.resource,
                                    ),
                                    span,
                                    kind: EffectErrorKind::MissingEffectDeclaration,
                                    subtype_trace: None,
                                    replacement: None,
                                    undeclared: undeclared(&name, [&te.effect]),
                                });
                            }
                        }
                    }
                    continue;
                }
                Some(DeclaredEffects::Explicit(declared_set)) => {
                    if let Some(inferred_set) = inferred {
                        let declared_effects = declared_set.effect_set();
                        let inferred_effects = inferred_set.effect_set();

                        // Check for missing declarations
                        for effect in &inferred_effects {
                            // Skip transparent effects
                            if self.is_transparent_verb(&effect.verb) {
                                continue;
                            }
                            if self.is_default_permitted_effect(effect) {
                                continue;
                            }
                            // Synthetic modbind resources are owned by
                            // slice 8's dedicated rejection — skip here so
                            // we don't double-fire missing-declaration.
                            if self.is_synthetic_modbind_resource(&effect.resource) {
                                continue;
                            }
                            if !declared_admits(&declared_effects, effect) {
                                let origin_msg = self.format_effect_origin(sym, effect);
                                self.errors.push(EffectError {
                                    message: format!(
                                        "public function '{}' performs {}({}) but does not declare it{}",
                                        name,
                                        verb_name(&effect.verb),
                                        effect.resource,
                                        origin_msg,
                                    ),
                                    span,
                                    kind: EffectErrorKind::MissingEffectDeclaration,
                                    subtype_trace: None,
                                    replacement: None,
                                    undeclared: undeclared(&name, [effect]),
                                });
                            }
                        }

                        // Check for over-declarations
                        for effect in &declared_effects {
                            if self.is_transparent_verb(&effect.verb) {
                                continue;
                            }
                            if !inferred_effects.contains(effect) {
                                self.errors.push(EffectError {
                                    message: format!(
                                        "public function '{}' declares {}({}) but does not perform it",
                                        name,
                                        verb_name(&effect.verb),
                                        effect.resource,
                                    ),
                                    span,
                                    kind: EffectErrorKind::OverDeclaredEffect,
                                    subtype_trace: None,
                                    replacement: None,
                                    undeclared: None,
                                });
                            }
                        }
                    }
                }
                Some(DeclaredEffects::None) | None => {
                    // Under `public_effects = "inferred"`, a pub fn may omit the
                    // declaration entirely — effects are inferred from the body
                    // the same way private functions are treated. If the author
                    // does write an explicit `with ...` clause, the other match
                    // arms above still verify it; this arm only governs the
                    // no-declaration case.
                    //
                    // Intentionally NOT `continue`-ing here, even under Inferred
                    // policy. The `with _` viral rule checked below applies regardless
                    // of policy: calling a polymorphic callee always requires `with _`.
                    if self.public_effects_policy != PublicEffectsPolicy::Inferred {
                        // Under Declared policy, require explicit effect annotations.
                        if let Some(inferred_set) = inferred {
                            // Filter synthetic modbind resources out of the
                            // fix-it list — slice 8's dedicated rejection
                            // owns those diagnostics, and the
                            // "Add: writes(COUNTER_resource)" message
                            // would suggest a name the user cannot legally
                            // write in source.
                            let non_transparent: Vec<&TracedEffect> = inferred_set
                                .effects
                                .iter()
                                .filter(|e| !self.is_transparent_verb(&e.effect.verb))
                                .filter(|e| !self.is_synthetic_modbind_resource(&e.effect.resource))
                                // `allocates(Heap)` is the v1 *substrate* effect
                                // (design.md § Effect Substrate, Principle 2):
                                // default-permitted under the standard profile,
                                // exercised ambiently by ~90% of functions, so
                                // requiring its declaration would be pure noise.
                                // Absence-of-declaration is NOT denial for a
                                // substrate effect. Exempt it from the
                                // must-declare set (and thus from the fix-it) so
                                // a pub fn whose only effect is allocation needs
                                // no `with` clause. `is_default_permitted_effect`
                                // is Default-profile-only, so `embedded`/`isr`
                                // (where heap is scoped) are unaffected — their
                                // rejection is owned by `check_profile_compat`.
                                // See examples/weave GAP-W6 / bugs B-2026-06-13-4.
                                // `panics`, the other default-permitted effect,
                                // takes the same exemption (B-2026-10-21-54).
                                .filter(|e| !self.is_default_permitted_effect(&e.effect))
                                .collect();
                            if !non_transparent.is_empty() {
                                // Render each effect in *source* form: resource
                                // verbs as `verb(Resource)`, but execution /
                                // resourceless verbs (`panics`, `blocks`,
                                // `suspends`) bare — `panics()` with empty
                                // parens does not parse. The fix-it then joins
                                // verbs with spaces inside a single `with`
                                // clause (`with reads(D) writes(D) panics`),
                                // because the effect-clause grammar is
                                // space-separated; a comma-joined suggestion
                                // (`reads(D), writes(D)`) is rejected by the
                                // parser. See examples/weave GAP-W6 — the
                                // pre-fix message suggested `allocates(Heap),
                                // panics()`, which compiled to neither.
                                let effects_list: Vec<String> = non_transparent
                                    .iter()
                                    .map(|e| {
                                        if e.effect.resource.is_empty() {
                                            verb_name(&e.effect.verb).to_string()
                                        } else {
                                            format!(
                                                "{}({})",
                                                verb_name(&e.effect.verb),
                                                e.effect.resource
                                            )
                                        }
                                    })
                                    .collect();
                                let clause = effects_list.join(" ");
                                // B-2026-08-24-12: when the return type is an
                                // `Fn(..)`, the bare advice is UNFOLLOWABLE —
                                // a `with` written directly after an `Fn`
                                // return type binds to that TYPE, not to the
                                // function, so the user re-runs and gets this
                                // same error verbatim. The return type has to
                                // be parenthesised for the clause to attach to
                                // the function. Render the real return type so
                                // the suggestion is copy-pasteable rather than
                                // a shape the reader has to reconstruct.
                                // B-2026-09-20-60: on `main` the bare advice
                                // keeps the `pub` that design.md § Entry Point
                                // forbids ("`main()` ... is never `pub`"), and
                                // following it produced a `pub main` that ran.
                                // The typechecker now rejects a `pub main`
                                // (E_MAIN_SIGNATURE), so the advice that agrees
                                // with it is to drop the `pub`. Free functions
                                // are keyed by bare name and methods dotted, so
                                // `name == "main"` is the free `main` only.
                                // An effect on `*` cannot be written in a clause:
                                // the fix is at the callee that has every effect
                                // (B-2026-11-14-33, B-2026-11-14-34), so name it instead.
                                let every = non_transparent
                                    .iter()
                                    .find(|e| e.effect.resource == super::EVERY_RESOURCE)
                                    .map(|e| self.format_effect_origin(sym, &e.effect));
                                let advice = match self.fn_return_type(sym) {
                                    _ if every.is_some() => format!(
                                        "It calls something that has every effect{}",
                                        every.clone().unwrap_or_default()
                                    ),
                                    _ if &*name == "main" => "`main()` is never `pub` \
                                         (design.md § Entry Point): remove `pub` and \
                                         its effects are inferred like any other \
                                         private function's"
                                        .to_string(),
                                    Some(ret) => format!(
                                        "Add: with {clause} after a PARENTHESISED return type — \
                                         `-> ({}) with {clause}` — because a `with` written \
                                         directly after an `Fn` return type binds to that type, \
                                         not to the function",
                                        crate::formatter::render_type_expr(&ret),
                                    ),
                                    None => {
                                        format!("Add: with {clause} to the function signature")
                                    }
                                };
                                self.errors.push(EffectError {
                                    message: format!(
                                        "public function '{}' performs effects [{}] but has no \
                                         effect declaration. {}",
                                        name,
                                        effects_list.join(", "),
                                        advice,
                                    ),
                                    span,
                                    kind: EffectErrorKind::MissingEffectDeclaration,
                                    subtype_trace: None,
                                    replacement: None,
                                    undeclared: undeclared(
                                        &name,
                                        non_transparent.iter().map(|e| &e.effect),
                                    ),
                                });
                            }
                        }
                    }
                }
            }
            // For any public fn not already declaring `with _` (those arms `continue`d),
            // require `with _` if it calls a polymorphic callee — regardless of whether
            // it has explicit effects, no declaration, or is under Inferred policy.
            if self.calls_polymorphic.contains(&sym) {
                self.errors.push(EffectError {
                    message: format!(
                        "public function '{}' calls a polymorphic (`with _`) function but does \
                         not declare `with _`. Add `with _` to propagate closure effects.",
                        name,
                    ),
                    span,
                    kind: EffectErrorKind::MissingEffectDeclaration,
                    subtype_trace: None,
                    replacement: None,
                    undeclared: None,
                });
            }
        }
        self.attach_with_clause_fixes(from);
    }

    /// Verify a WRITTEN effect clause where [`Self::verify_declarations`] does
    /// not look: on a private free function and on a program's own method,
    /// public or private (design.md §12: "a declared set is checked like a
    /// public one"; "Declarations are verified, not trusted";
    /// B-2026-11-04-27). Only what the body performs beyond the clause is
    /// reported. A function with no clause is inferred and has nothing to
    /// verify, and a clause naming more than the body does is allowed, so a
    /// library can reserve an effect. The library's own bodies are not
    /// re-verified here, and a trait's default body has its own pass
    /// ([`Self::verify_trait_default_bodies`]).
    pub(crate) fn verify_written_declarations(&mut self) {
        // (key, what it is, is public, the verbs its clause roots at a value)
        let mut targets: Vec<(Symbol, &'static str, bool, Vec<EffectVerbKind>)> = self
            .function_bodies
            .iter()
            .filter(|(sym, f)| {
                !f.stdlib_origin && !self.function_visibility.get(*sym).copied().unwrap_or(false)
            })
            .map(|(&sym, f)| (sym, "function", false, value_rooted_verbs(f)))
            .collect();
        // Inherent methods only: a trait impl's method is held to the trait's
        // ceiling by `verify_impl_trait_ceilings`, and a default body copied
        // into an impl carries the trait's clause, which its own pass checks.
        for item in &self.program.items {
            let Item::ImplBlock(imp) = item else {
                continue;
            };
            let TypeKind::Path(p) = &imp.target_type.kind else {
                continue;
            };
            if imp.trait_name.is_some() {
                continue;
            }
            let Some(type_name) = p.segments.last() else {
                continue;
            };
            for it in &imp.items {
                if let ImplItem::Method(m) = it {
                    if !m.stdlib_origin {
                        targets.push((
                            self.interner.dotted_str(type_name, &m.name),
                            "method",
                            m.is_pub,
                            value_rooted_verbs(m),
                        ));
                    }
                }
            }
        }
        targets.sort_by_cached_key(|(sym, ..)| self.interner.resolve(*sym));
        targets.dedup_by_key(|(sym, ..)| *sym);
        let mut new_errors = Vec::new();
        for (sym, what, is_pub, rooted) in targets {
            if self.fn_uses_with_underscore.contains(&sym) {
                continue;
            }
            let (allowed, poly) = match self.declared_effects.get(&sym) {
                Some(DeclaredEffects::Explicit(set)) => (set.effect_set(), false),
                Some(DeclaredEffects::PolymorphicWithFixed(fixed)) => (fixed.effect_set(), true),
                _ => continue,
            };
            let Some(inferred) = self.inferred_effects.get(&sym) else {
                continue;
            };
            let name = self.interner.resolve(sym);
            let span = self.function_spans.get(&sym).copied().unwrap_or(Span {
                line: 0,
                column: 0,
                offset: 0,
                length: 0,
            });
            for te in &inferred.effects {
                if self.is_transparent_verb(&te.effect.verb)
                    || self.is_default_permitted_effect(&te.effect)
                    || self.is_synthetic_modbind_resource(&te.effect.resource)
                    || (poly && self.effect_came_via_polymorphic_callee(te, sym))
                    || declared_admits(&allowed, &te.effect)
                    // A resource rooted at `self` or a parameter stands for
                    // what the body does to that value, which the MIR pass
                    // keys; this checker has no key for it.
                    || rooted.contains(&te.effect.verb)
                    // "Private functions infer" `blocks` (design.md §12
                    // blocks), clause or none.
                    || (!is_pub
                        && matches!(te.effect.verb, EffectVerbKind::Blocks | EffectVerbKind::Suspends))
                {
                    continue;
                }
                let origin_msg = self.format_effect_origin(sym, &te.effect);
                // `*` cannot be written in a clause; the origin note names
                // the fix (B-2026-11-14-33, B-2026-11-14-34).
                let advice = if te.effect.resource == super::EVERY_RESOURCE {
                    String::new()
                } else {
                    format!(
                        "; add {}({}) to the clause or remove the call",
                        verb_name(&te.effect.verb),
                        te.effect.resource,
                    )
                };
                new_errors.push(EffectError {
                    message: format!(
                        "{what} '{name}' performs {}({}){origin_msg} but its `with` clause does \
                         not declare it{advice}",
                        verb_name(&te.effect.verb),
                        te.effect.resource,
                    ),
                    span,
                    kind: EffectErrorKind::MissingEffectDeclaration,
                    subtype_trace: None,
                    replacement: None,
                    undeclared: undeclared(&name, [&te.effect]),
                });
            }
        }
        let from = self.errors.len();
        self.errors.extend(new_errors);
        self.attach_with_clause_fixes(from);
    }

    /// Enforce design.md § Panic Semantics at the FFI Boundary, case 2:
    /// an exported `extern "C-unwind" fn` whose body may panic **must**
    /// declare `panics` in its `with` clause
    /// (`E_EXTERN_C_UNWIND_REQUIRES_PANICS`, surfaced as E0413).
    ///
    /// This is deliberately separate from `verify_declarations`, which
    /// only governs `pub` functions and honours the `public_effects`
    /// policy. The C-unwind rule fires for the export regardless of
    /// visibility or policy: the `"C-unwind"` ABI selection is itself
    /// the assertion that a panic may cross the boundary as a C++
    /// exception, so the signature must say so. (`extern "C"` exports
    /// carry no such requirement — case 1 auto-aborts a body panic at
    /// the boundary, so the panic never escapes into C.)
    pub(crate) fn verify_extern_export_panics(&mut self) {
        // Under `Abort`, a `"C-unwind"` export is rejected outright by
        // `verify_extern_cunwind_requires_unwind_profile` (E0415) — the ABI
        // itself is unhonorable without an unwinder — so the "must declare
        // `panics`" refinement (which only matters when C-unwind is actually
        // permitted, i.e. under `Unwind`) would just double-report. Skip it.
        if self.profile_config.panic_strategy() == crate::manifest::PanicStrategy::Abort {
            return;
        }
        // Collect offenders first so the immutable borrows of
        // `function_bodies` / `inferred_effects` / group tables are
        // released before we mutate `self.errors`.
        let mut offenders: Vec<(String, Span)> = Vec::new();
        let names: Vec<Symbol> = self.function_bodies.keys().copied().collect();
        for &sym in &names {
            let func = match self.function_bodies.get(&sym) {
                Some(f) if f.abi.as_deref() == Some("C-unwind") => f.clone(),
                _ => continue,
            };

            // The rule only bites when the body can actually panic.
            let body_panics = self.inferred_effects.get(&sym).is_some_and(|set| {
                set.effects
                    .iter()
                    .any(|te| matches!(te.effect.verb, EffectVerbKind::Panics))
            });
            if !body_panics {
                continue;
            }

            // ...and the declaration must omit `panics`. `panics` is a
            // resourceless execution verb, so we scan the `with` clause's
            // items directly for it rather than going through
            // `resolve_effect_list_to_set` (which only materialises
            // *resourced* effects and would silently drop `panics`). A
            // `with SomeGroup` whose expansion mentions `panics` also
            // counts; a bare `with _` (Polymorphic) does NOT spell
            // `panics` out, which is the intended strictness.
            let declares_panics = func
                .effects
                .as_ref()
                .is_some_and(|list| self.effect_list_declares_panics(list));
            if declares_panics {
                continue;
            }

            offenders.push((self.interner.resolve(sym).to_string(), func.span));
        }

        for (name, span) in offenders {
            self.errors.push(EffectError {
                message: format!(
                    "exported `extern \"C-unwind\" fn '{name}'` may panic but does not \
                     declare `panics`; the \"C-unwind\" ABI propagates an unwinding panic \
                     across the FFI boundary, so the signature must say so. Add `with panics` \
                     (or, if a C-shaped error is wanted instead, switch to `extern \"C\"` and \
                     wrap the body in `catch_panic`). \
                     See design.md § Panic Semantics at the FFI Boundary."
                ),
                span,
                kind: EffectErrorKind::ExternCUnwindRequiresPanics,
                subtype_trace: None,
                replacement: None,
                undeclared: None,
            });
        }
    }

    /// Reject every `extern "C-unwind"` site under a build whose panic strategy
    /// is `Abort` (design.md § FFI / Panic Semantics at the FFI Boundary;
    /// phase-8 `panic = "unwind" | "abort"` slice 5). The `"C-unwind"` ABI's
    /// whole point is to let an unwinding panic cross the FFI boundary, which
    /// needs a stack unwinder; under `Abort` there is none, so the ABI cannot
    /// be honored. v1 is abort-only (the unwind codegen is the v1.x-gated
    /// slice 9), so at v1 this fires for every `"C-unwind"` site.
    ///
    /// Covers **foreign-import declarations** — `unsafe extern "C-unwind" { fn
    /// … }` (bare standalone imports are rejected at parse, so blocks are the
    /// only import shape) — and Kāra **exports** (`pub extern "C-unwind" fn …`).
    /// Calls need no separate rule: rejecting the declaration leaves the callee
    /// unresolved, and an export is rejected at its own definition. Being
    /// strategy-driven, it lifts automatically once a build can select
    /// `panic = "unwind"` (slice 9). It supersedes the "must declare `panics`"
    /// refinement under `Abort` ([`Self::verify_extern_export_panics`] returns
    /// early there), so a C-unwind export reports E0415, not E0413.
    pub(crate) fn verify_extern_cunwind_requires_unwind_profile(&mut self) {
        if self.profile_config.panic_strategy() != crate::manifest::PanicStrategy::Abort {
            return;
        }
        // `(what, name, span)` — `what` distinguishes an imported declaration
        // from a Kāra export in the diagnostic, since the fix differs.
        let mut offenders: Vec<(&'static str, String, Span)> = Vec::new();
        for item in &self.program.items {
            match item {
                Item::ExternBlock(block) if block.abi == "C-unwind" => {
                    for extern_item in &block.items {
                        if let ExternItem::Function(f) = extern_item {
                            offenders.push(("import", f.name.clone(), f.span));
                        }
                    }
                }
                Item::ExternFunction(f) if f.abi == "C-unwind" => {
                    offenders.push(("import", f.name.clone(), f.span));
                }
                Item::Function(f) if f.abi.as_deref() == Some("C-unwind") => {
                    offenders.push(("export", f.name.clone(), f.span));
                }
                _ => {}
            }
        }

        for (what, name, span) in offenders {
            let subject = if what == "import" {
                format!("`extern \"C-unwind\"` declaration of `{name}`")
            } else {
                format!("exported `extern \"C-unwind\" fn '{name}'`")
            };
            self.errors.push(EffectError {
                message: format!(
                    "{subject} requires a build with `panic = \"unwind\"`, but this build is \
                     abort-only. The \"C-unwind\" ABI propagates an unwinding panic across the \
                     FFI boundary, which needs a stack unwinder the abort strategy does not \
                     provide (0.8 is abort-only). Use \
                     `extern \"C\"` instead — a body panic auto-aborts at the boundary — or, for \
                     a C-shaped error, return a status/`Result`. See design.md § Panic Semantics \
                     at the FFI Boundary."
                ),
                span,
                kind: EffectErrorKind::ExternCUnwindRequiresUnwindProfile,
                subtype_trace: None,
                replacement: None,
                undeclared: None,
            });
        }
    }

    /// Producer-mode export boundary (additive-interop Slice 3½;
    /// design.md § Exported C ABI). A `pub extern "C"` / `"C-unwind"`
    /// function definition whose *inferred* effect set contains
    /// `suspends` cannot be exported to C at v1: a C caller drives no
    /// Kāra scheduler, so a suspending body has no yield-to substrate on
    /// a bare foreign thread. Sibling of [`verify_extern_export_panics`]
    /// — scans the same `inferred_effects` table (the verified set, so
    /// this catches `suspends` reached through a private helper too, not
    /// just a declared `with suspends`).
    pub(crate) fn verify_extern_export_no_suspends(&mut self) {
        let mut offenders: Vec<(String, Span)> = Vec::new();
        let names: Vec<Symbol> = self.function_bodies.keys().copied().collect();
        for &sym in &names {
            let func = match self.function_bodies.get(&sym) {
                // Any C-ABI export definition (a body callable from C),
                // `"C"` or `"C-unwind"` — the codegen export treatment is
                // keyed on `abi.is_some()`, so the boundary rule is too.
                Some(f) if f.abi.is_some() => f.clone(),
                _ => continue,
            };
            let body_suspends = self.inferred_effects.get(&sym).is_some_and(|set| {
                set.effects
                    .iter()
                    .any(|te| matches!(te.effect.verb, EffectVerbKind::Suspends))
            });
            if body_suspends {
                offenders.push((self.interner.resolve(sym).to_string(), func.span));
            }
        }

        for (name, span) in offenders {
            self.errors.push(EffectError {
                message: format!(
                    "exported `extern \"C\" fn '{name}'` suspends, but the C-export boundary \
                     is synchronous-only: a C caller drives no Kāra scheduler, so a suspending \
                     export cannot yield on a bare foreign thread. Expose a *blocking* wrapper \
                     that owns a runtime-scoped task internally, or keep the suspending work \
                     behind a non-exported function. See design.md § Exported C ABI."
                ),
                span,
                kind: EffectErrorKind::ExternExportSuspendsUnsupported,
                subtype_trace: None,
                replacement: None,
                undeclared: None,
            });
        }
    }

    /// True iff a `with` clause spells out the resourceless `panics`
    /// verb, directly or via a referenced effect group. Used by the
    /// C-unwind export rule; kept separate from the resource-oriented
    /// `resolve_effect_list_to_set` because that helper materialises
    /// only resourced effects and would drop `panics` entirely.
    fn effect_list_declares_panics(&self, list: &EffectList) -> bool {
        list.items.iter().any(|item| match item {
            EffectItem::Verb(v) => matches!(v.kind, EffectVerbKind::Panics),
            EffectItem::Group(name) => self.expanded_groups.get(name).is_some_and(|g| {
                g.effects
                    .iter()
                    .any(|te| matches!(te.effect.verb, EffectVerbKind::Panics))
            }),
            EffectItem::Polymorphic | EffectItem::Variable(_) => false,
        })
    }

    /// For every `impl Trait for Type` block, verify that each impl method's
    /// inferred effect set is a subset of the trait method's declared ceiling.
    ///
    /// - `DeclaredEffects::Explicit(set)` ceiling → inferred must be ⊆ set.
    /// - `Polymorphic` / `PolymorphicWithFixed` / `None` / missing key → no check
    ///   (wildcard or unbound ceiling means impls are free).
    pub(crate) fn verify_impl_trait_ceilings(&mut self) {
        let items: &[Item] = &self.program.items;
        let mut new_errors: Vec<EffectError> = Vec::new();

        for item in items {
            let imp = match item {
                Item::ImplBlock(imp) => imp,
                _ => continue,
            };
            // Only `impl Trait for Type` — inherent impls have no trait ceiling.
            let trait_name = match &imp.trait_name {
                Some(path) => path.segments.last().cloned().unwrap_or_default(),
                None => continue,
            };
            let type_name = match &imp.target_type.kind {
                TypeKind::Path(p) => p.segments.last().cloned().unwrap_or_default(),
                _ => continue,
            };

            for impl_item in &imp.items {
                let method = match impl_item {
                    ImplItem::Method(m) => m,
                    ImplItem::AssocType(_) => continue,
                };

                let impl_key = self.interner.dotted_str(&type_name, &method.name);
                let trait_key = self.interner.dotted_str(&trait_name, &method.name);

                // Look up the trait method's declared ceiling.
                let ceiling_set = match self.declared_effects.get(&trait_key) {
                    Some(DeclaredEffects::Explicit(set)) => set.effect_set(),
                    // Polymorphic, PolymorphicWithFixed, None, or unknown trait → free.
                    _ => continue,
                };

                let inferred = match self.inferred_effects.get(&impl_key) {
                    Some(s) => s.clone(),
                    None => continue,
                };

                let mut ceiling_display: Vec<String> = ceiling_set
                    .iter()
                    .filter(|e| !self.is_transparent_verb(&e.verb))
                    .map(|e| format!("{}({})", verb_name(&e.verb), e.resource))
                    .collect();
                ceiling_display.sort();
                let ceiling_str = if ceiling_display.is_empty() {
                    "pure (no effects)".to_string()
                } else {
                    format!("[{}]", ceiling_display.join(", "))
                };

                for te in &inferred.effects {
                    // `allocates(Heap)` and `panics` never have to be
                    // declared, a ceiling's no more than a signature's
                    // (design.md §12 Default-permitted effects,
                    // B-2026-11-06-29).
                    if self.is_transparent_verb(&te.effect.verb)
                        || self.is_default_permitted_effect(&te.effect)
                    {
                        continue;
                    }
                    if !declared_admits(&ceiling_set, &te.effect) {
                        new_errors.push(EffectError {
                            message: format!(
                                "impl method '{}.{}' performs {}({}) but trait '{}.{}' \
                                 ceiling is {}; narrow the impl or widen the trait ceiling",
                                type_name,
                                method.name,
                                verb_name(&te.effect.verb),
                                te.effect.resource,
                                trait_name,
                                method.name,
                                ceiling_str,
                            ),
                            span: method.span,
                            kind: EffectErrorKind::ImplExceedsTraitCeiling,
                            subtype_trace: None,
                            replacement: None,
                            undeclared: None,
                        });
                    }
                }
            }
        }
        self.errors.extend(new_errors);
    }

    /// B-2026-08-30-29 — enforce design.md § Drop and Destructors, "Drop bodies
    /// must not panic": an `impl Drop for T`'s `fn drop` must be inferred
    /// `panics`-free, and the impl is rejected at the DEFINITION SITE if it is
    /// not.
    ///
    /// The spec is unusually explicit that this belongs here and nowhere else:
    /// "This is a static rule, not a runtime one — there is no 'abort on panic
    /// in drop' fallback to discover at runtime." The rationale it gives is
    /// that destructors run during panic unwind, so a panic-during-unwind
    /// becomes a double-panic process abort (§ Catching Panics) that destroys
    /// the ORIGINAL failure's information — the checker exists to remove that
    /// class of heisenbug rather than to report it after the fact.
    ///
    /// Deliberately keyed on the INFERRED set, not the declared one. `fn drop`
    /// carries no `with` clause to declare effects on, so inference is the only
    /// thing that knows the body indexes a `Vec`, overflows a `#[checked]` add,
    /// or calls a `panics` function — which is exactly the enumeration the
    /// spec sentence gives.
    ///
    /// Structural sibling of [`Self::verify_impl_trait_ceilings`], and placed
    /// beside it on purpose: both walk `impl Trait for Type` blocks and compare
    /// an impl method's inferred effects against a ceiling. Here the ceiling is
    /// fixed by the language rather than by the trait's declaration.
    pub(crate) fn verify_drop_bodies_are_panics_free(&mut self) {
        let items: &[Item] = &self.program.items;
        let mut new_errors: Vec<EffectError> = Vec::new();

        for item in items {
            let imp = match item {
                Item::ImplBlock(imp) => imp,
                _ => continue,
            };
            // `impl Drop for T` only — an inherent `fn drop` is an ordinary
            // method the compiler never calls as a destructor, so the rule
            // does not reach it.
            let is_drop_impl = imp
                .trait_name
                .as_ref()
                .and_then(|p| p.segments.last())
                .is_some_and(|n| n == "Drop");
            if !is_drop_impl {
                continue;
            }
            let type_name = match &imp.target_type.kind {
                TypeKind::Path(p) => p.segments.last().cloned().unwrap_or_default(),
                _ => continue,
            };

            for impl_item in &imp.items {
                let method = match impl_item {
                    ImplItem::Method(m) => m,
                    ImplItem::AssocType(_) => continue,
                };
                if method.name != "drop" {
                    continue;
                }
                let key = self.interner.dotted_str(&type_name, &method.name);
                let inferred = match self.inferred_effects.get(&key) {
                    Some(s) => s.clone(),
                    None => continue,
                };
                if !inferred
                    .effects
                    .iter()
                    .any(|te| matches!(te.effect.verb, EffectVerbKind::Panics))
                {
                    continue;
                }
                new_errors.push(EffectError {
                    message: format!(
                        "drop body for '{type_name}' can panic, but a `Drop` impl must be \
                         `panics`-free — a destructor runs during panic unwind, where a \
                         second panic aborts the process and destroys the original \
                         failure's report; handle the failure in place instead \
                         (`match fallible() {{ Ok(_) => {{}}, Err(e) => log(e) }}`)"
                    ),
                    span: method.span,
                    kind: EffectErrorKind::DropBodyMayPanic,
                    subtype_trace: None,
                    replacement: None,
                    undeclared: None,
                });
            }
        }
        self.errors.extend(new_errors);
    }

    /// For every trait method that has a default body, verify that the body's
    /// inferred effect set is a subset of the method's declared ceiling.
    ///
    /// The trait author cannot smuggle effects into callers by hiding them in a
    /// default body without declaring them on the method's `with` clause.
    ///
    /// - `DeclaredEffects::Explicit(ceiling)` → inferred must be ⊆ ceiling.
    /// - `Polymorphic` / `PolymorphicWithFixed` / `None` / missing key → no check.
    pub(crate) fn verify_trait_default_bodies(&mut self) {
        let items: &[Item] = &self.program.items;
        let mut new_errors: Vec<EffectError> = Vec::new();

        for item in items {
            let t = match item {
                Item::TraitDef(t) => t,
                _ => continue,
            };
            for trait_item in &t.items {
                let m = match trait_item {
                    TraitItem::Method(m) => m,
                    TraitItem::AssocType(_) => continue,
                };
                if m.body.is_none() {
                    continue;
                }

                let key = self.interner.dotted_str(&t.name, &m.name);

                let ceiling_set = match self.declared_effects.get(&key) {
                    Some(DeclaredEffects::Explicit(set)) => set.effect_set(),
                    _ => continue,
                };

                let inferred = match self.inferred_effects.get(&key) {
                    Some(s) => s.clone(),
                    None => continue,
                };

                let mut ceiling_display: Vec<String> = ceiling_set
                    .iter()
                    .filter(|e| !self.is_transparent_verb(&e.verb))
                    .map(|e| format!("{}({})", verb_name(&e.verb), e.resource))
                    .collect();
                ceiling_display.sort();
                let ceiling_str = if ceiling_display.is_empty() {
                    "pure (no effects)".to_string()
                } else {
                    format!("[{}]", ceiling_display.join(", "))
                };

                for te in &inferred.effects {
                    // `allocates(Heap)` and `panics` never have to be
                    // declared, a ceiling's no more than a signature's
                    // (design.md §12 Default-permitted effects,
                    // B-2026-11-06-29).
                    if self.is_transparent_verb(&te.effect.verb)
                        || self.is_default_permitted_effect(&te.effect)
                    {
                        continue;
                    }
                    if !declared_admits(&ceiling_set, &te.effect) {
                        new_errors.push(EffectError {
                            message: format!(
                                "default body of '{}.{}' performs {}({}) but the method \
                                 ceiling is {}; declare the effect on the method or remove \
                                 it from the default body",
                                t.name,
                                m.name,
                                verb_name(&te.effect.verb),
                                te.effect.resource,
                                ceiling_str,
                            ),
                            span: m.span,
                            kind: EffectErrorKind::TraitDefaultExceedsCeiling,
                            subtype_trace: None,
                            replacement: None,
                            undeclared: None,
                        });
                    }
                }
            }
        }
        self.errors.extend(new_errors);
    }
}

/// The verbs whose resources `f`'s clause roots at `self` or at a parameter
/// (design.md § Value-rooted resources): `sends(self)`, `writes(f)`.
fn value_rooted_verbs(f: &Function) -> Vec<EffectVerbKind> {
    let params = crate::ast::value_root_param_names(&f.params);
    let Some(list) = &f.effects else {
        return Vec::new();
    };
    list.items
        .iter()
        .filter_map(|i| match i {
            EffectItem::Verb(v)
                if v.resources.iter().any(|r| {
                    r.path
                        .first()
                        .is_some_and(|root| root == "self" || params.iter().any(|p| p == root))
                }) =>
            {
                Some(v.kind.clone())
            }
            _ => None,
        })
        .collect()
}
