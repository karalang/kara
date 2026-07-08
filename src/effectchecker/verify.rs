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
use crate::token::Span;

use super::{
    verb_name, DeclaredEffects, EffectError, EffectErrorKind, PublicEffectsPolicy, TracedEffect,
};

impl<'a> super::EffectChecker<'a> {
    // ── Verification ────────────────────────────────────────────

    pub(crate) fn verify_declarations(&mut self) {
        let fn_names: Vec<String> = self.function_bodies.keys().cloned().collect();
        for name in &fn_names {
            let is_pub = self.function_visibility.get(name).copied().unwrap_or(false);
            if !is_pub {
                continue;
            } // Private functions don't need declarations

            let declared = self.declared_effects.get(name);
            let inferred = self.inferred_effects.get(name);
            let span = self.function_spans.get(name).cloned().unwrap_or(Span {
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
                    if self.fn_uses_with_underscore.contains(name) {
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
                            if self.effect_came_via_polymorphic_callee(te, name) {
                                continue;
                            }
                            let origin_msg = self.format_effect_origin(name, &te.effect);
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
                                span: span.clone(),
                                kind: EffectErrorKind::MissingEffectDeclaration,
                                subtype_trace: None,
                                replacement: None,
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
                    if self.fn_uses_with_underscore.contains(name) {
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
                            if self.effect_came_via_polymorphic_callee(te, name) {
                                continue;
                            }
                            if !fixed_set.contains(&te.effect) {
                                let origin_msg = self.format_effect_origin(name, &te.effect);
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
                                    span: span.clone(),
                                    kind: EffectErrorKind::MissingEffectDeclaration,
                                    subtype_trace: None,
                                    replacement: None,
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
                            if !declared_effects.contains(effect) {
                                let origin_msg = self.format_effect_origin(name, effect);
                                self.errors.push(EffectError {
                                    message: format!(
                                        "public function '{}' performs {}({}) but does not declare it{}",
                                        name,
                                        verb_name(&effect.verb),
                                        effect.resource,
                                        origin_msg,
                                    ),
                                    span: span.clone(),
                                    kind: EffectErrorKind::MissingEffectDeclaration,
                                    subtype_trace: None,
                                    replacement: None,
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
                                    span: span.clone(),
                                    kind: EffectErrorKind::OverDeclaredEffect,
                                    subtype_trace: None,
                                    replacement: None,
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
                                self.errors.push(EffectError {
                                    message: format!(
                                        "public function '{}' performs effects [{}] but has no \
                                         effect declaration. Add: with {} to the function signature",
                                        name,
                                        effects_list.join(", "),
                                        effects_list.join(" "),
                                    ),
                                    span: span.clone(),
                                    kind: EffectErrorKind::MissingEffectDeclaration,
                                    subtype_trace: None,
                                    replacement: None,
                                });
                            }
                        }
                    }
                }
            }
            // For any public fn not already declaring `with _` (those arms `continue`d),
            // require `with _` if it calls a polymorphic callee — regardless of whether
            // it has explicit effects, no declaration, or is under Inferred policy.
            if self.calls_polymorphic.contains(name) {
                self.errors.push(EffectError {
                    message: format!(
                        "public function '{}' calls a polymorphic (`with _`) function but does \
                         not declare `with _`. Add `with _` to propagate closure effects.",
                        name,
                    ),
                    span: span.clone(),
                    kind: EffectErrorKind::MissingEffectDeclaration,
                    subtype_trace: None,
                    replacement: None,
                });
            }
        }
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
        // Collect offenders first so the immutable borrows of
        // `function_bodies` / `inferred_effects` / group tables are
        // released before we mutate `self.errors`.
        let mut offenders: Vec<(String, Span)> = Vec::new();
        let names: Vec<String> = self.function_bodies.keys().cloned().collect();
        for name in &names {
            let func = match self.function_bodies.get(name) {
                Some(f) if f.abi.as_deref() == Some("C-unwind") => f.clone(),
                _ => continue,
            };

            // The rule only bites when the body can actually panic.
            let body_panics = self.inferred_effects.get(name).is_some_and(|set| {
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

            offenders.push((name.clone(), func.span.clone()));
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
        let names: Vec<String> = self.function_bodies.keys().cloned().collect();
        for name in &names {
            let func = match self.function_bodies.get(name) {
                // Any C-ABI export definition (a body callable from C),
                // `"C"` or `"C-unwind"` — the codegen export treatment is
                // keyed on `abi.is_some()`, so the boundary rule is too.
                Some(f) if f.abi.is_some() => f.clone(),
                _ => continue,
            };
            let body_suspends = self.inferred_effects.get(name).is_some_and(|set| {
                set.effects
                    .iter()
                    .any(|te| matches!(te.effect.verb, EffectVerbKind::Suspends))
            });
            if body_suspends {
                offenders.push((name.clone(), func.span.clone()));
            }
        }

        for (name, span) in offenders {
            self.errors.push(EffectError {
                message: format!(
                    "exported `extern \"C\" fn '{name}'` suspends, but the v1 C-export boundary \
                     is synchronous-only: a C caller drives no Kāra scheduler, so a suspending \
                     export cannot yield on a bare foreign thread. Expose a *blocking* wrapper \
                     that owns a runtime-scoped task internally, or keep the suspending work \
                     behind a non-exported function. See design.md § Exported C ABI."
                ),
                span,
                kind: EffectErrorKind::ExternExportSuspendsUnsupported,
                subtype_trace: None,
                replacement: None,
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
        let items: Vec<Item> = self.program.items.clone();
        let mut new_errors: Vec<EffectError> = Vec::new();

        for item in &items {
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

                let impl_key = format!("{}.{}", type_name, method.name);
                let trait_key = format!("{}.{}", trait_name, method.name);

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
                    if self.is_transparent_verb(&te.effect.verb) {
                        continue;
                    }
                    if !ceiling_set.contains(&te.effect) {
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
                            span: method.span.clone(),
                            kind: EffectErrorKind::ImplExceedsTraitCeiling,
                            subtype_trace: None,
                            replacement: None,
                        });
                    }
                }
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
        let items: Vec<Item> = self.program.items.clone();
        let mut new_errors: Vec<EffectError> = Vec::new();

        for item in &items {
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

                let key = format!("{}.{}", t.name, m.name);

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
                    if self.is_transparent_verb(&te.effect.verb) {
                        continue;
                    }
                    if !ceiling_set.contains(&te.effect) {
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
                            span: m.span.clone(),
                            kind: EffectErrorKind::TraitDefaultExceedsCeiling,
                            subtype_trace: None,
                            replacement: None,
                        });
                    }
                }
            }
        }
        self.errors.extend(new_errors);
    }
}
