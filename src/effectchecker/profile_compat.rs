//! `#[profile(P1, P2, ...)]` attribute enforcement — slice 3 of the
//! profile-attribute entry.
//!
//! For every function (free or inherent-method) carrying a non-empty
//! `profile_compat` list, walk the function's transitive effect set
//! (declared if `Explicit`, otherwise the inference result — the
//! seeding in `collect_function_info` makes the two paths converge in
//! `inferred_effects`). For each effect, intersect the listed profiles'
//! constraint sets — equivalently, take the *union* of their forbidden
//! sets — and emit one diagnostic per offending effect, naming every
//! listed profile that forbids it.
//!
//! Per-profile forbidden-effect table mirrors
//! `extern_ffi::profile_forbids` but is keyed on a `CompileProfile`
//! argument (not `self.profile`), because the attribute names target
//! profiles independently of the active build profile.

use crate::ast::*;
use crate::manifest::CompileProfile;

use super::{verb_name, Effect, EffectError, EffectErrorKind};

impl super::EffectChecker<'_> {
    pub(crate) fn check_profile_compat(&mut self) {
        let items: Vec<Item> = self.program.items.clone();
        for item in &items {
            match item {
                Item::Function(f) if !f.profile_compat.is_empty() => {
                    self.check_one_profile_compat(&f.name, f);
                }
                Item::ImplBlock(imp) => {
                    let target = match &imp.target_type.kind {
                        TypeKind::Path(p) => p.segments.last().cloned().unwrap_or_default(),
                        _ => continue,
                    };
                    for it in &imp.items {
                        if let ImplItem::Method(m) = it {
                            if !m.profile_compat.is_empty() {
                                let key = format!("{}.{}", target, m.name);
                                self.check_one_profile_compat(&key, m);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn check_one_profile_compat(&mut self, key: &str, f: &Function) {
        // Dedup the declared profile list — `#[profile(a, a)]` or two
        // `#[profile]` attrs naming the same profile collapse to one
        // constraint contributor.
        let mut profile_names: Vec<String> = Vec::new();
        for name in &f.profile_compat {
            if !profile_names.iter().any(|n| n == name) {
                profile_names.push(name.clone());
            }
        }
        // Resolve to `CompileProfile`; bail if any name is unknown — the
        // resolver already emitted `E_UNKNOWN_PROFILE` for it, so we
        // don't pile a stale-data effect error on top.
        let mut parsed: Vec<CompileProfile> = Vec::with_capacity(profile_names.len());
        for name in &profile_names {
            match CompileProfile::parse(name) {
                Some(p) => parsed.push(p),
                None => return,
            }
        }

        let effect_set = match self.inferred_effects.get(key) {
            Some(s) => s.clone(),
            None => return,
        };

        for tagged in &effect_set.effects {
            let forbidding: Vec<&'static str> = parsed
                .iter()
                .copied()
                .filter(|p| profile_forbids_effect(*p, &tagged.effect))
                .map(|p| p.as_str())
                .collect();
            if forbidding.is_empty() {
                continue;
            }
            let effect_str = if tagged.effect.resource.is_empty() {
                verb_name(&tagged.effect.verb)
            } else {
                format!(
                    "{}({})",
                    verb_name(&tagged.effect.verb),
                    tagged.effect.resource
                )
            };
            let profile_list = profile_names.join(", ");
            let forbidding_str = if forbidding.len() == 1 {
                format!("the `{}` profile", forbidding[0])
            } else {
                format!(
                    "the strictest of `{}` (forbidden by `{}`)",
                    profile_list,
                    forbidding.join("`, `")
                )
            };
            let message = format!(
                "error[E_PROFILE_INCOMPATIBLE_EFFECT]: fn `{}` declares `#[profile({})]` but its effect set includes `{}`, which is forbidden by {}",
                f.name, profile_list, effect_str, forbidding_str,
            );
            self.errors.push(EffectError {
                message,
                span: f.span.clone(),
                kind: EffectErrorKind::ProfileIncompatibleEffect,
                subtype_trace: None,
            });
        }
    }
}

/// Per-profile forbidden-effect predicate. Mirrors the table in
/// `extern_ffi::profile_forbids` but keyed on an explicit
/// `CompileProfile` argument so it can iterate the attribute's listed
/// profiles rather than the active build profile.
fn profile_forbids_effect(profile: CompileProfile, effect: &Effect) -> bool {
    match profile {
        CompileProfile::Default => false,
        CompileProfile::Embedded => matches!(
            (&effect.verb, effect.resource.as_str()),
            (EffectVerbKind::Allocates, "Heap")
        ),
        CompileProfile::Kernel => matches!(
            &effect.verb,
            EffectVerbKind::Allocates
                | EffectVerbKind::Panics
                | EffectVerbKind::Blocks
                | EffectVerbKind::Suspends
        ),
    }
}
