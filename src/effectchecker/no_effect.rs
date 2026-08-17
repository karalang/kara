//! `#[no_effect(allocates(Heap), panics)]` enforcement (B-2026-08-17-8).
//!
//! Sibling of `profile_compat.rs`, and deliberately built as one: both walk a
//! function's transitive effect set and reject the effects a declaration says
//! must not be there. The only difference is where the forbidden set comes
//! from — `#[profile]` names profiles and looks their constraints up, while
//! `#[no_effect]` names the effects directly. So the guarantee is the same
//! guarantee, available per-function on the default profile instead of only
//! project-wide on a constrained one.
//!
//! MATCHING IS BY VERB, THEN BY RESOURCE IF ONE IS NAMED. `#[no_effect(allocates)]`
//! forbids every `allocates(...)`, whatever the resource; `#[no_effect(allocates(Heap))]`
//! forbids only `allocates(Heap)` and says nothing about `allocates(Arena)`. The
//! bare form has to be the broad one: `panics`, `blocks` and `suspends` take no
//! resource at all, so reading a bare verb as "resource-less effects only" would
//! make `#[no_effect(allocates)]` a near-no-op while `#[no_effect(panics)]` worked
//! — the same spelling meaning two different strengths depending on the verb.
//!
//! The effect set consulted is `inferred_effects`, which is the declared set for
//! an `Explicit` signature and the inferred one otherwise (the seeding in
//! `collect_function_info` converges the two). That is what makes the check
//! transitive for free: a function that calls an allocating helper carries the
//! helper's `allocates` in its own set.

use crate::ast::*;
use crate::intern::Symbol;

use super::{verb_name, EffectError, EffectErrorKind};

impl super::EffectChecker<'_> {
    pub(crate) fn check_no_effect(&mut self) {
        let items: &[Item] = &self.program.items;
        for item in items {
            match item {
                Item::Function(f) if !f.no_effect.is_empty() => {
                    let key = self.interner.intern(&f.name);
                    self.check_one_no_effect(key, f);
                }
                Item::ImplBlock(imp) => {
                    let target = match &imp.target_type.kind {
                        TypeKind::Path(p) => p.segments.last().cloned().unwrap_or_default(),
                        _ => continue,
                    };
                    for it in &imp.items {
                        if let ImplItem::Method(m) = it {
                            if !m.no_effect.is_empty() {
                                let key = self.interner.dotted_str(&target, &m.name);
                                self.check_one_no_effect(key, m);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn check_one_no_effect(&mut self, key: Symbol, f: &Function) {
        let effect_set = match self.inferred_effects.get(&key) {
            Some(s) => s.clone(),
            None => return,
        };

        for tagged in &effect_set.effects {
            let Some(forbidden) = f.no_effect.iter().find(|v| {
                if v.kind != tagged.effect.verb {
                    return false;
                }
                // A bare verb forbids the whole verb; a resource list narrows
                // it to exactly those resources.
                v.resources.is_empty()
                    || v.resources
                        .iter()
                        .any(|r| r.path.join(".") == tagged.effect.resource)
            }) else {
                continue;
            };

            let effect_str = if tagged.effect.resource.is_empty() {
                verb_name(&tagged.effect.verb)
            } else {
                format!(
                    "{}({})",
                    verb_name(&tagged.effect.verb),
                    tagged.effect.resource
                )
            };
            let declared_str = if forbidden.resources.is_empty() {
                verb_name(&forbidden.kind)
            } else {
                format!(
                    "{}({})",
                    verb_name(&forbidden.kind),
                    forbidden
                        .resources
                        .iter()
                        .map(|r| r.path.join("."))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            };
            let message = format!(
                "error[E_NO_EFFECT_VIOLATED]: fn `{}` declares `#[no_effect({})]` but its effect set includes `{}`",
                f.name, declared_str, effect_str,
            );
            self.errors.push(EffectError {
                message,
                span: f.span,
                kind: EffectErrorKind::NoEffectViolated,
                subtype_trace: None,
                replacement: None,
            });
        }
    }
}
