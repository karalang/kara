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
use crate::token::Span;

use super::{OwnershipError, OwnershipErrorKind};

/// Which spelling of a freeze this site is, and — for the statement form — the
/// stage-3b step-1 uniqueness answer.
///
/// An enum rather than a bare `bool` because three states are live, not two: a
/// PARAMETER can never carry the proof (its instance is the caller's), while a
/// STATEMENT either carries it or fails it, and those two failures need
/// different advice. Collapsing them is how the `MutableState` suggestion came
/// to tell every author that no per-instance check exists — true when it was
/// written, and made false by the very change that consults this.
#[derive(Clone, Copy)]
pub(crate) enum FreezeSite {
    /// A `frozen T` parameter or `frozen self` receiver.
    Param,
    /// A `let g = freeze <place>;` statement.
    Statement { uniquely_bound: bool },
}

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
            .then(|| impl_type.map(|t| (Some(t.to_string()), f.span)))
            .flatten();
        // `false` — a PARAMETER freeze site never carries the stage-3b
        // uniqueness proof, and this is a deliberate asymmetry rather than a
        // conservative default. The proof is about one INSTANCE in one scope;
        // a parameter's instance belongs to the caller, whose other live
        // handles this pass cannot see. Establishing it here would be the
        // interprocedural analysis mechanism 3 exists to avoid — so a
        // `frozen T` parameter of a `mut`-bearing type stays refused, and
        // inherits its guarantee from an already-frozen argument instead.
        let sites: Vec<(Option<String>, Span, FreezeSite)> = f
            .params
            .iter()
            .filter(|p| p.is_frozen)
            .map(|p| (frozen_type_name(&p.ty), p.span, FreezeSite::Param))
            .chain(receiver.map(|(n, s)| (n, s, FreezeSite::Param)))
            .collect();
        self.report_freeze_sites(sites);
    }

    /// Classify a batch of freeze sites and emit `E0512` for each refusal.
    ///
    /// Shared by the DECLARED sites (a `frozen` parameter or `frozen self`
    /// receiver, above) and the stage-3 STATEMENT sites, which the escape walk
    /// enumerates as it goes and hands over — one classifier, so a type that
    /// may not be frozen is refused whichever way the freeze was spelled.
    pub(crate) fn report_freeze_sites(&mut self, sites: Vec<(Option<String>, Span, FreezeSite)>) {
        for (name, site_span, site) in sites {
            let Some(refusal) = name.as_deref().map_or(Some(Refusal::NotShared), |n| {
                self.classify_freezable(n, site)
            }) else {
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
                // One message again: there is now exactly one way to reach
                // this arm — a STATEMENT whose source failed uniqueness. A
                // parameter of a `mut`-bearing type is admitted, so the
                // parameter-shaped advice this briefly carried described a
                // refusal that can no longer happen. A suggestion for an
                // unreachable state is worse than none; this row already
                // records an ownership `suggestion` that reached nobody at all.
                Refusal::MutableState => (
                    format!("`{shown}` has mutable state, so it cannot be frozen yet"),
                    format!(
                        "freezing claims the value is deeply immutable for the region. For a \
                         `mut`-bearing type like `{shown}` that claim rests on this being the \
                         ONLY live name for the instance, and something else here names it too \
                         — an earlier use of the source, or a source that is itself bound from \
                         another place. Move or remove that other binding so the `freeze` is \
                         the first thing to touch the value, or remove `mut` from the fields \
                         reachable from `{shown}`"
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
    ///
    /// `uniquely_bound` is the stage-3b step-1 proof, and it relaxes exactly
    /// one arm: [`Refusal::MutableState`]. The type-level deep-immutability
    /// demand exists as a STAND-IN for the per-instance check the design
    /// actually specifies ("no live mutable handle to the instance"), which is
    /// what this module's own header says. When the per-instance fact is
    /// established, the stand-in has nothing left to stand in for and would be
    /// refusing on a property the rule never required.
    ///
    /// It relaxes NOTHING else, and the two other arms are not oversights:
    /// `NotShared` and `AlreadyPar` are about the REPRESENTATION (no refcount
    /// header to skip, or an atomic one that needs no skipping), which no fact
    /// about aliasing can change.
    ///
    /// ## Why a PARAMETER is admitted without a proof of its own
    ///
    /// The design expects a `frozen T` parameter to "inherit the guarantee from
    /// its caller's already-frozen argument, which is how the mode composes
    /// today". MEASURED, that composition does not exist: a `frozen` slot
    /// accepts an ordinary binding (`frz(t)` checks clean), so there is nothing
    /// to inherit. Building it would mean demanding a frozen argument at every
    /// call site — and two call forms resolve no signature today (a free
    /// function taken as a VALUE, `let f = frz; f(t)`; and a `frozen` parameter
    /// on a method whose RECEIVER is not frozen), so that enforcement would
    /// have carried exactly the silent hole this family keeps producing.
    ///
    /// It is not needed, because the parameter is not where the guarantee is
    /// established — the PAR CAPTURE is. Probed with this arm relaxed, every
    /// route for an unfrozen `mut`-bearing handle to reach a `frozen` parameter
    /// concurrently was refused, and refused at the capture:
    ///
    /// * an unfrozen `shared` root reaching a branch — refused, "cannot be
    ///   accessed from multiple concurrent branches" — through a direct call,
    ///   through a function-as-value, and through a method;
    /// * a FROZEN place passed to a slot whose signature cannot be resolved —
    ///   refused as a non-`frozen` slot, i.e. the unresolvable case fails
    ///   CLOSED rather than open;
    /// * the `mut` field unreadable and unwritable through the parameter, the
    ///   `frozen self` receiver, and the frozen source alike — the projection
    ///   guard, untouched;
    /// * a non-counting handle escaping the call — refused, E0511, untouched.
    ///
    /// So a `mut`-bearing `frozen` parameter can only be reached with a handle
    /// some caller already froze, and the freeze is where the per-instance
    /// proof is taken. Demanding a second proof here would refuse the callee
    /// traversal — the shape #133 is written in — while adding nothing.
    ///
    /// HONEST CHARACTERISATION, because it differs in kind from the statement
    /// check above and the difference should not be lost: that one is
    /// FAIL-CLOSED (refuse unless uniqueness is proven), this one is FAIL-OPEN
    /// (admit because no unsafe route was found). The probe list is broad but
    /// it is evidence of absence. What pays for it is that every guard it leans
    /// on — the capture gate, the projection walk, the escape check — is itself
    /// fail-closed, so a route this reasoning missed still has three refusals
    /// to get past.
    ///
    /// What keeps the relaxation sound is that it does not stand alone. The
    /// escape walk freezes the SOURCE root for the rest of the scope, so the
    /// owner cannot be moved, reassigned, returned or captured while the
    /// frozen handle lives; the projection guard still refuses every `mut`
    /// field in read and write position alike (stage 3b step 3, deliberately
    /// untouched here), so no interior handle can be materialized through the
    /// frozen place; and uniqueness means no OTHER name existed to write
    /// through in the first place. Removing any one of the three re-opens the
    /// race, which is why step 3 is specified to land last and alone.
    fn classify_freezable(&self, type_name: &str, site: FreezeSite) -> Option<Refusal> {
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
            // A `mut`-bearing type: refused only when this is a STATEMENT
            // whose source failed uniqueness. A PARAMETER is admitted without a
            // proof of its own — see the doc comment above for why that is not
            // a hole.
            None => match site {
                FreezeSite::Statement {
                    uniquely_bound: false,
                } => Some(Refusal::MutableState),
                _ => None,
            },
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
