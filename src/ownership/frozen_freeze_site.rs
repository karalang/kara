//! `frozen` freeze-site check — B-2026-08-01-33 mechanism 3, stage 1.
//!
//! The sibling of [`super::frozen_escape`]. That module asks "can this handle
//! get out?"; this one asks the prior question: **may this type be frozen at
//! all?**
//!
//! ## Where the freeze site is, in the shape stage 1 actually has
//!
//! The design sketches an explicit `let g = freeze graph;` statement, and says
//! the check at that site is "the value must be deeply immutable for the
//! region". Stage 1 has no `freeze` expression — the only way to obtain a
//! `frozen` value is to *declare a parameter* `frozen T`. So the freeze site
//! today is the **parameter declaration**, and that is where this check lives.
//!
//! ## Why the stage-1 form is structural, not per-instance
//!
//! The design's check is a property of the *instance*: no live mutable handle
//! to it or to anything reachable from it, for the duration of the region.
//! Stage 1 cannot evaluate that — it has no region, no freeze statement, and no
//! instance-level liveness. What it can do is demand the type make the claim
//! unnecessary: if no type in the reachable closure has a `mut` field, then no
//! mutable handle to any part of it can exist, and deep immutability holds for
//! every instance, unconditionally.
//!
//! That is strictly stronger than the design's rule and therefore sound as a
//! stage-1 stand-in. It is also exactly the staging's item 3 ("freezing a
//! `mut`-bearing type … the deep-immutability check at the freeze site")
//! stated as a rejection instead of a silence: `frozen M` where `M` has a `mut`
//! field is refused *now*, rather than accepted while the check that would
//! justify it does not exist. #133's `Node.neighbors` is `mut`, so #133 stays
//! blocked here — as the staging always said it would be — but it is blocked
//! with a diagnostic that names the missing piece.
//!
//! The predicate itself is [`super::OwnershipChecker::deep_immutability_closure`],
//! shared with mechanism 2's atomic promotion rather than reimplemented, so the
//! compiler cannot come to hold two opinions about what "deeply immutable"
//! means.
//!
//! ## Why the type must be `shared`
//!
//! `frozen` buys exactly one thing: permission to skip refcount traffic on a
//! handle. A plain struct has no refcount and is copied by value; a scalar has
//! neither. On those, the mode is not merely useless — it is a claim about a
//! representation the value does not have, and accepting it would let programs
//! be written against a meaning that never held. A `par struct` is refused for
//! the opposite reason: it is *already* safe to share across branches (atomic
//! RC by construction), so `frozen` would be a no-op dressed as a guarantee.
//!
//! Every rejection here is fail-closed: an unresolvable type name is refused,
//! not assumed freezable.

use crate::ast::*;

use super::{OwnershipError, OwnershipErrorKind};

/// Why a `frozen` parameter's type was refused. Each variant names the specific
/// property that failed, because "cannot be frozen" alone would leave the
/// reader guessing which of three unrelated conditions they tripped.
enum Refusal {
    /// Not a `shared` type — a plain struct, an enum, a scalar, a generic
    /// parameter, or a name this pass could not resolve.
    NotShared,
    /// A `par struct`: already atomic, already shareable, nothing to freeze.
    AlreadyPar,
    /// A `shared` type with a `mut` field somewhere in its reachable closure.
    /// Stage 1 has no per-instance immutability check to justify freezing it.
    MutableState,
}

impl super::OwnershipChecker<'_> {
    /// Verify every `frozen` parameter of `f` names a type that may be frozen.
    /// Emits `E0512` at the parameter.
    ///
    /// No-op for a function with no `frozen` parameter, which today is every
    /// function in every program.
    pub(crate) fn check_frozen_freeze_site(&mut self, f: &Function, impl_type: Option<&str>) {
        // A `frozen self` receiver (stage 2.7) is a freeze site too, and its
        // "declared type" is the impl target. Checked through exactly the same
        // classifier, so a receiver and a parameter cannot end up disagreeing
        // about which types may be frozen. `impl_type` is `None` for a free
        // function, where `self_is_frozen` is never set.
        let receiver = f
            .self_is_frozen
            .then(|| impl_type.map(|t| (Some(t.to_string()), f.span.clone())))
            .flatten();
        let sites = f
            .params
            .iter()
            .filter(|p| p.is_frozen)
            .map(|p| (frozen_type_name(&p.ty), p.span.clone()))
            .chain(receiver);
        for (name, site_span) in sites {
            let Some(refusal) = name
                .as_deref()
                .map_or(Some(Refusal::NotShared), |n| self.classify_freezable(n))
            else {
                continue;
            };
            let shown = name.unwrap_or_else(|| "this type".to_string());
            let (message, suggestion) = match refusal {
                Refusal::NotShared => (
                    format!("`frozen` requires a `shared` type; `{shown}` is not one"),
                    format!(
                        "`frozen` exists to skip refcount traffic on a handle, and `{shown}` has \
                         no refcount — a plain struct is copied by value and a scalar is a \
                         register. Drop `frozen` and pass it normally"
                    ),
                ),
                Refusal::AlreadyPar => (
                    format!("`{shown}` is a `par struct`, so `frozen` adds nothing"),
                    "a `par struct` is already safe to read from several tasks at once — its \
                     refcounting is atomic by construction. Drop `frozen`; the sharing you want \
                     already works"
                        .to_string(),
                ),
                Refusal::MutableState => (
                    format!("`{shown}` has mutable state, so it cannot be frozen yet"),
                    format!(
                        "freezing claims the value is deeply immutable for the region, and stage \
                         1 has no per-instance check to back that claim — so it requires the \
                         TYPE to guarantee it: no `mut` field anywhere reachable from `{shown}`. \
                         Either remove `mut` from the reachable fields, or pass `{shown}` \
                         normally until the per-instance freeze check lands"
                    ),
                ),
            };
            self.errors.push(OwnershipError {
                message,
                span: site_span,
                kind: OwnershipErrorKind::FrozenTypeNotFreezable,
                suggestion: Some(suggestion),
                replacement: None,
                consume_span: None,
            });
        }
    }

    /// `None` when `type_name` may be frozen; the reason otherwise.
    fn classify_freezable(&self, type_name: &str) -> Option<Refusal> {
        let Some(info) = self.typecheck_result.struct_info.get(type_name) else {
            // Not a user struct at all — enum, scalar, builtin container,
            // generic parameter, or unresolvable. None of those carry the
            // `shared` header `frozen` is about. Fail-closed.
            return Some(Refusal::NotShared);
        };
        if info.is_par {
            return Some(Refusal::AlreadyPar);
        }
        if !info.is_shared {
            return Some(Refusal::NotShared);
        }
        // Shared, so it has the header. The remaining question is whether the
        // deep-immutability claim holds structurally — same predicate mechanism
        // 2 uses to decide what it may promote.
        match self.deep_immutability_closure(type_name) {
            Some(_) => None,
            None => Some(Refusal::MutableState),
        }
    }
}

/// The type name a `frozen` parameter denotes, for the freezability lookup.
///
/// Only a plain path is recognised, which is the only shape stage 1's parser
/// accepts in this position anyway. Anything else yields `None` and is refused
/// as not-`shared` rather than waved through.
fn frozen_type_name(ty: &TypeExpr) -> Option<String> {
    // A `frozen` param is stored as `Ref(T)` — see the parser's `frozen` arm.
    let ty = match &ty.kind {
        TypeKind::Ref(inner) => inner.as_ref(),
        _ => ty,
    };
    let TypeKind::Path(path) = &ty.kind else {
        return None;
    };
    // A generic instantiation (`Vec[T]`, `Box[N]`) is not a `shared` struct
    // name; taking the last segment would misidentify `std.foo.N` as `N`, which
    // is the right lookup key for `struct_info`, while `Vec[i64]` correctly
    // fails the lookup and is refused.
    path.segments.last().cloned()
}
