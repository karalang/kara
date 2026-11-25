//! Validation for `#[derive(...)]` attributes.
//!
//! Houses `type_supports_*` predicates (per-trait field-shape checks)
//! and the `validate_derive_*` driver methods that enforce the
//! derive rules on struct / enum definitions. Lives in a sibling
//! `impl<'a> super::TypeChecker<'a>` block so the methods stay on
//! `TypeChecker` (preserving the `self.validate_derive_*` call shape
//! from `super::check()`).

use crate::ast::*;

use super::types::{is_numeric, type_display, Type, UIntSize, VariantTypeInfo};
use crate::resolver::SpanKey;
use rustc_hash::FxHashSet;

use super::{
    extract_derived_traits, FxHashMap, NestedEnumInstKey, NestedEnumInstSite, Span, TypeErrorKind,
};

/// A local type `karac fix` can derive `Copy` or `Clone` on: the edit and
/// its line, and the field types its derive needs the trait on.
pub(super) struct DeriveDecl {
    edit: (crate::resolver::TextEdit, usize),
    fields: Vec<Type>,
}

impl<'a> super::TypeChecker<'a> {
    /// When `ty` is `std.secret`'s `Secret[T]`: why it has no `trait_name`
    /// and what to write instead, for a refusal that would otherwise advise
    /// a derive, which secret.md forbids on `Secret` (B-2026-11-18-56).
    pub(super) fn secret_withheld_hint(&self, ty: &Type, trait_name: &str) -> Option<String> {
        let mut t = ty;
        while let Type::Ref(inner) | Type::MutRef(inner) = t {
            t = inner;
        }
        let Type::Named { name, .. } = t else {
            return None;
        };
        let std_secret = name == "Secret"
            && self
                .env
                .structs
                .get("Secret")
                .is_some_and(|i| i.defining_stdlib_origin);
        if !std_secret {
            return None;
        }
        let instead = match trait_name {
            "Hash" | "Eq" | "PartialEq" => {
                "compare secrets with `.ct_eq(other)`, and key a table by a non-secret digest \
                 of the value, never by the secret"
            }
            "Serialize" => {
                "send it only by an explicit choice: `.expose()` at the use site, or a \
                 non-secret field in its place"
            }
            _ => {
                "print something that is not the secret, or read it deliberately with \
                 `.expose()` at the use site"
            }
        };
        Some(format!(
            "`{}` withholds `{trait_name}` on purpose, so a secret cannot leak by accident, \
             and no derive or impl can add it (docs/library/secret.md); {instead}",
            type_display(t)
        ))
    }

    /// See [`super::env::TypeEnv::distinct_derive_supported`].
    fn distinct_derive_supported(&self, ty: &Type, wanted: &[&str]) -> Option<bool> {
        self.env.distinct_derive_supported(ty, wanted)
    }

    /// See [`super::env::TypeEnv::type_supports_partial_eq`].
    pub(super) fn type_supports_partial_eq(&self, ty: &Type) -> bool {
        self.env.type_supports_partial_eq(ty)
    }

    /// See [`super::env::TypeEnv::type_supports_eq`].
    pub(super) fn type_supports_eq(&self, ty: &Type) -> bool {
        self.env.type_supports_eq(ty)
    }

    /// See [`super::env::TypeEnv::type_supports_hash`].
    pub(super) fn type_supports_hash(&self, ty: &Type) -> bool {
        self.env.type_supports_hash(ty)
    }

    /// See [`super::env::TypeEnv::has_user_impl_ord`].
    pub(super) fn has_user_impl_ord(&self, name: &str) -> bool {
        self.env.has_user_impl_ord(name)
    }

    /// Check whether a type supports total `Ord`. Floats do not (see Eq).
    /// B-2026-08-11-7 — enforce the `T: Ord` bound on a sequence method whose
    /// result is only meaningful under a TOTAL order (`sort`, `sorted`,
    /// `max`, `min`, `binary_search`).
    ///
    /// design.md § Float semantics is explicit that `f32`/`f64` implement
    /// `PartialOrd` but NOT `Ord`, and that they therefore "cannot be used …
    /// in any context requiring `Ord`" — and it gives `scores.sort()` on a
    /// `Vec[f64]` as its own counter-example. That example compiled. The gate
    /// was already wired for `SortedSet`/`SortedMap` and for the generic
    /// bound solver; the Vec/Slice method arms simply never consulted it, so
    /// the same ordering question was REJECTED as a free `max(a, b)` call and
    /// ACCEPTED as `v.max()`.
    ///
    /// The harm is a silent wrong answer, not a missing message: with a NaN
    /// present `sort()` leaves the Vec unsorted (measured identical on all
    /// three backends, so the differential oracle cannot see it either), and
    /// `max`/`min` return an order-dependent element — NaN first makes BOTH
    /// of them return NaN, NaN in the middle is silently skipped.
    ///
    /// Deliberately NOT applied to `contains` (which the row flagged as the
    /// arguable leg) or to `reverse`: `contains` needs only `PartialEq`,
    /// which `f64` genuinely has, and its measured behaviour is IEEE-correct
    /// and backend-identical — it finds elements past a NaN, `contains(NaN)`
    /// is correctly `false`, and `-0.0` finds `0.0`. Rejecting it would break
    /// correct code for no soundness gain. `reverse` involves no comparison
    /// at all.
    /// Whether `ty` has a DEFAULT total order that BOTH backends can lower —
    /// the predicate behind the `sort` / `sorted` / `is_sorted` element gate
    /// (B-2026-08-27-51).
    ///
    /// This is [`Self::type_supports_ord`] with exactly three corrections, and
    /// keeping it that way is deliberate: the sort gate wants "is this
    /// orderable", the derive gate wants the same thing, and any place the two
    /// answers diverge is a place one of them is wrong. Stating the gate as a
    /// short diff against the general predicate makes each divergence
    /// individually arguable, instead of restating the whole comparator family
    /// here and letting the copy drift — which is how this gap opened.
    ///
    /// 1. A `Vec` / `VecDeque` / `Slice` ELEMENT recurses. `type_supports_ord`
    ///    answers `false` for `Vec[String]`, and that is not a useful
    ///    strictness — it is the container being a baked stdlib struct that
    ///    carries no `#[derive(Ord)]`, so it falls to the `Named` arm and is
    ///    judged as a user type. (The `Type::Vector` arm that looks like it
    ///    would catch this is the SIMD `Vector[T, N]`, an unrelated type.)
    ///    B-2026-08-11-7 hit this: its first version gated on
    ///    `type_supports_ord` and broke `Vec[Vec[String]].sort()`, whose
    ///    recursive lexicographic comparator has worked since B-2026-06-30-15,
    ///    and it left a warning to check what the comparators support rather
    ///    than what the predicate says. Measured again here before writing this
    ///    arm: `#[derive(Ord)] struct Z { x: Vec[String] }` is still rejected
    ///    today, so the warning is live, not stale.
    ///
    /// 2. A `Shared` type is NOT ordered by its derive. `type_supports_ord`
    ///    says a `#[derive(Ord)] shared struct S` is `Ord`; codegen's
    ///    `emit_cmp_fn_for_type_expr` excludes shared types outright, and the
    ///    `<` operator already rejects one at check with a purpose-written
    ///    message ("`#[derive(PartialOrd)]` does not order a shared type"). So
    ///    the language had already decided this and only the sort gate had not
    ///    heard; `Vec[S].sort()` on that same type was check-green,
    ///    interp-sorted and build-refused.
    ///
    /// 3. A `Float` returns `true` HERE so that the IEEE gate above stays the
    ///    only voice on floats. `type_supports_ord` correctly says `false`,
    ///    but that answer is already delivered by `reachable_ieee_float` with a
    ///    diagnostic that names the NaN harm and the `F64` wrapper remedy;
    ///    letting this predicate speak too would print a second, vaguer error
    ///    for the same cause.
    ///
    /// Where it is UNCERTAIN it answers `true`, and the asymmetry is the whole
    /// reason: a wrong `false` rejects a program that compiles and runs today,
    /// while a wrong `true` merely leaves the pre-existing loud `karac build`
    /// error in place. So unknown names, type parameters and exotic shapes are
    /// admitted and left to the backend.
    pub(super) fn element_default_order_is_lowerable(&self, ty: &Type) -> bool {
        match ty {
            // Correction 3 — floats are the float gate's business, not ours.
            Type::Float(_) => true,
            // Correction 2 — a derive does not order a shared type.
            Type::Shared(_) => false,
            Type::Refinement { base, .. } => self.element_default_order_is_lowerable(base),
            Type::Ref(inner) | Type::MutRef(inner) | Type::Rc(inner) | Type::Arc(inner) => {
                self.element_default_order_is_lowerable(inner)
            }
            Type::Tuple(elems) => elems
                .iter()
                .all(|e| self.element_default_order_is_lowerable(e)),
            Type::Array { element, .. } | Type::Slice { element, .. } => {
                self.element_default_order_is_lowerable(element)
            }
            Type::Named { name, args } => match name.as_str() {
                // Correction 1 — the baked sequence containers recurse.
                "Vec" | "VecDeque" | "Slice" => args
                    .first()
                    .is_none_or(|e| self.element_default_order_is_lowerable(e)),
                _ => self.type_supports_ord(ty),
            },
            _ => self.type_supports_ord(ty),
        }
    }

    /// The reason clause for a [`Self::element_default_order_is_lowerable`]
    /// rejection, chosen so the message names the actual obstacle and the
    /// remedy that fits it (B-2026-08-27-51).
    ///
    /// A single "not orderable" sentence would be true for all four shapes and
    /// useful for none: the fix for an underived struct is a derive, the fix
    /// for a generic or shared one is a hand-written comparator, and telling
    /// someone to add a derive that cannot help is the failure mode
    /// B-2026-08-27-47 had to correct on the `<` operator's message.
    fn unordered_element_reason(&self, ty: &Type) -> String {
        let (name, shared) = match ty {
            Type::Named { name, .. } => (name.clone(), false),
            Type::Shared(name) => (name.clone(), true),
            _ => return "it has no default ordering".to_string(),
        };
        if shared {
            return format!(
                "'{name}' is a shared type and `#[derive(Ord)]` does not order one — \
                 add `impl Ord for {name}`"
            );
        }
        let generic = self
            .env
            .structs
            .get(&name)
            .map(|i| !i.generic_params.is_empty())
            .or_else(|| {
                self.env
                    .enums
                    .get(&name)
                    .map(|i| !i.generic_params.is_empty())
            })
            .unwrap_or(false);
        if generic {
            format!(
                "'{name}' is generic, and a derived comparator cannot order a \
                 type-parameter payload — add `impl Ord for {name}`"
            )
        } else {
            format!("'{name}' does not implement Ord — add `#[derive(Ord)]` to it")
        }
    }

    pub(super) fn require_ord_element(
        &mut self,
        element: &Type,
        receiver: &str,
        method: &str,
        span: &Span,
    ) {
        let Some(float) = Self::reachable_ieee_float(element) else {
            // Not a float — but the element still needs a default order both
            // backends can lower, or `karac build` refuses a program `karac
            // check` accepted (B-2026-08-27-51). The float arm below returns
            // early precisely so it stays the only voice on floats.
            if !self.element_default_order_is_lowerable(element) {
                let disp = type_display(element);
                let reason = self.unordered_element_reason(element);
                self.type_error(
                    format!(
                        "{receiver}.{method}(): element type '{disp}' has no default \
                         ordering, so the result would not be well-defined; note: \
                         {reason}, or pass an explicit comparator with \
                         `sort_by(|a, b| ...)`"
                    ),
                    *span,
                    TypeErrorKind::TraitBoundNotSatisfied,
                );
            }
            return;
        };
        let wrapper = match float.as_str() {
            "f32" => "F32",
            "f16" => "F16",
            "bf16" => "Bf16",
            _ => "F64",
        };
        let disp = type_display(element);
        // Same remedy wording as the generic bound solver's note
        // (`render_unsatisfied_bound_message`), so a user who meets this gate
        // through either route reads the same fix. The wrapper it names is
        // real as of B-2026-08-11-8 — before that, this advice dead-ended at
        // "no associated function 'from' on type 'F64'".
        let mut msg = format!(
            "{receiver}.{method}(): element type '{disp}' has no total order, so the \
             result would not be well-defined"
        );
        // The harm clause is per-method because the observed wrong answer
        // differs, and a diagnostic that describes the wrong symptom teaches
        // the wrong lesson. Both texts are measured, not paraphrased.
        let harm = if method == "is_sorted" {
            // Measured, not paraphrased: with the gate lifted,
            // `[1.0, NaN].is_sorted()` answered `true` on all four surfaces
            // while `a[0] <= a[1]` on the same pair answered `false`. The two
            // backends AGREE here (both route floats through the IEEE total
            // order), so the harm is not a divergence — it is that the
            // predicate contradicts the comparison operator it reads as a fold
            // of, and a `requires v.is_sorted()` contract therefore passes on a
            // sequence `binary_search` cannot search.
            "`[1.0, NaN].is_sorted()` answers TRUE, while the `v[0] <= v[1]` it \
             reads as a fold of answers false — the predicate needs a TOTAL order \
             and `<=` is the IEEE partial one, so a `requires v.is_sorted()` \
             contract admits a sequence `binary_search` cannot search"
        } else if matches!(method, "max" | "min") {
            "`[3.0, NaN, 1.0].iter().max()` silently SKIPS the NaN and answers 3, \
             while `[NaN, 3.0, 1.0]` makes BOTH `max()` and `min()` answer NaN — \
             the result depends on where the NaN sits"
        } else {
            "`[3.0, NaN, 1.0, 2.0].sort()` leaves the sequence completely unsorted on \
             every backend"
        };
        msg.push_str(&format!(
            "; note: `{float}` is not totally ordered (IEEE-754 NaN), and with a NaN \
             present this silently produces a WRONG ANSWER rather than an error — \
             {harm}. Use the total-order wrapper `{wrapper}` \
             (`{wrapper}.from(x)`), e.g. `xs.iter().map({wrapper}.from).collect()`"
        ));
        self.type_error(msg, *span, TypeErrorKind::TraitBoundNotSatisfied);
    }

    /// The name of an IEEE float primitive reachable inside `ty` by a
    /// structural comparison, or `None`.
    ///
    /// B-2026-08-11-7 gates on THIS rather than on `!type_supports_ord`,
    /// deliberately. `type_supports_ord` is stricter than what the sort
    /// comparators actually implement — it answers `false` for
    /// `Vec[Vec[String]]`, because `Vec` is a baked stdlib struct carrying no
    /// `#[derive(Ord)]`, even though `Vec[Vec[String]].sort()` is supported
    /// and tested (B-2026-06-30-15 added the recursive lexicographic
    /// comparator). Gating on it broke that fixture. Floats are the case with
    /// evidence of a silent wrong answer and the case design.md explicitly
    /// forbids, so the gate is scoped to exactly them: it cannot reject a
    /// sort that works today.
    fn reachable_ieee_float(ty: &Type) -> Option<String> {
        match ty {
            Type::Float(_) => Some(type_display(ty)),
            Type::Refinement { base, .. } => Self::reachable_ieee_float(base),
            Type::Tuple(elems) => elems.iter().find_map(Self::reachable_ieee_float),
            Type::Array { element, .. }
            | Type::Vector { element, .. }
            | Type::Slice { element, .. } => Self::reachable_ieee_float(element),
            Type::Ref(inner) | Type::MutRef(inner) => Self::reachable_ieee_float(inner),
            // `Vec[f64]` / `Option[f64]` spelled as a Named container. The
            // wrappers (`F64` and friends) are Named too and deliberately do
            // NOT match — carrying a total order is their whole purpose.
            Type::Named { name, args } if matches!(name.as_str(), "Vec" | "Option") => {
                args.iter().find_map(Self::reachable_ieee_float)
            }
            _ => None,
        }
    }

    /// A tailored message for the aggregate that DOES carry the ordering derive
    /// but is generic, so [`Self::derived_ord_is_lowerable`] declined it
    /// (B-2026-08-27-47).
    ///
    /// Without this the generic case inherits the general "add
    /// `#[derive(PartialOrd)]`" advice, which is actively wrong here: the
    /// prelude's `Option` already carries `#[derive(Ord, PartialOrd)]`, so a
    /// reader who follows that hint finds the derive present and has been sent
    /// looking in the wrong place. Say what is actually true instead — the
    /// derive is there and the ordering is what is unsupported — and name the
    /// escape hatch that does work, since a hand-written `impl Ord` on a
    /// generic type lowers on both backends.
    pub(super) fn ordering_unsupported_because_generic(&self, ty: &Type) -> Option<String> {
        let name = match ty {
            Type::Named { name, .. } | Type::Shared(name) => name,
            _ => return None,
        };
        if self.has_user_impl_ord(name) {
            return None;
        }
        let (generic_params, derived) = if let Some(info) = self.env.structs.get(name) {
            (&info.generic_params, &info.derived_traits)
        } else {
            let info = self.env.enums.get(name)?;
            (&info.generic_params, &info.derived_traits)
        };
        if generic_params.is_empty() || !(derived.contains("Ord") || derived.contains("PartialOrd"))
        {
            return None;
        }
        Some(format!(
            "type '{}' derives an ordering but '{}' is generic, and a derived \
             comparator cannot order a type-parameter payload; write an \
             `impl Ord for {}` to order it explicitly",
            type_display(ty),
            name,
            name,
        ))
    }

    /// See [`super::env::TypeEnv::type_supports_ord`].
    pub(super) fn type_supports_ord(&self, ty: &Type) -> bool {
        self.env.type_supports_ord(ty)
    }

    /// Check whether a type implements `Display`.
    /// All primitives support Display. Built-in containers (Vec, Map, SortedSet,
    /// Option, Result) support Display when their type arguments do.
    /// Named user types require `#[derive(Display)]`.
    pub(super) fn type_supports_display(&self, ty: &Type) -> bool {
        if let Some(ok) = self.distinct_derive_supported(ty, &["Display"]) {
            // A distinct type inherits nothing from its base, but a
            // hand-written `impl Display for` it is its own: design.md §9's
            // newtype escape hatch (B-2026-11-18-20).
            return ok
                || matches!(ty, Type::Named { name, args } if self.env.has_impl("Display", name, args));
        }
        match ty {
            Type::Refinement { base, .. } => self.type_supports_display(base),
            Type::Int(_)
            | Type::UInt(_)
            | Type::Float(_)
            | Type::Bool
            | Type::Char
            | Type::Str => true,
            // Unit is NOT Display — displaying `()` is meaningless (Rust has no
            // `Display for ()` either). Treating it as Display let a unit-typed
            // f-string interpolation (`f"{()}"`, or the degenerate `f"a{{}}b"`
            // that parses `{}` as an empty unit block) slip past the
            // interpolation Display check and then RENDER DIFFERENTLY per
            // backend — the interpreter prints `()`, codegen prints `0` (the
            // residual real bug behind B-2026-07-08-8). Rejecting it at
            // typecheck closes that run-vs-build divergence at the source and
            // turns the meaningless case into a clear "does not implement
            // Display" error for every Display context (f-string, `println`,
            // `to_string`).
            Type::Unit => false,
            Type::Tuple(elems) => elems.iter().all(|e| self.type_supports_display(e)),
            Type::Array { element, .. } => self.type_supports_display(element),
            Type::Vector { element, .. } => self.type_supports_display(element),
            Type::Slice { element, .. } => self.type_supports_display(element),
            Type::Ref(inner) | Type::MutRef(inner) => self.type_supports_display(inner),
            // A `dyn` value of `Display` or a trait under it shows through
            // its `to_string` slot (design.md § `dyn Trait`).
            Type::Dyn { trait_name, .. } => self
                .env
                .supertrait_closure_traits(trait_name)
                .iter()
                .any(|t| t == "Display"),
            Type::Named { name, args } => match name.as_str() {
                "Vec" | "VecDeque" | "Option" | "SortedSet" | "Set"
                    if crate::typechecker::types::element_args(name, args).len() == 1 =>
                {
                    self.type_supports_display(&args[0])
                }
                "Map" | "SortedMap" | "Result" if crate::typechecker::types::element_args(name, args).len() == 2 => {
                    self.type_supports_display(&args[0]) && self.type_supports_display(&args[1])
                }
                // Total-order float wrappers Display as their inner float
                // (B-2026-08-11-8). Not expressible as a `#[derive(Display)]`
                // on the baked stdlib struct: the derive renders the struct
                // shape (`F64 { value: 1.5 }`), where the wrapper must render
                // exactly like the primitive it wraps (`1.5`). Both backends
                // implement that directly — `synth_display.rs` for codegen,
                // the `TotalFloat*` arms of `Display for Value` in the
                // interpreter — so this arm is what lets a checked program
                // reach either of them. Before it, BOTH `println(x)` and
                // `f"{x}"` rejected every `F64`, which made the wrapper the
                // compiler recommends for `Ord` contexts unprintable.
                "F32" | "F64" | "F16" | "Bf16" if args.is_empty() => true,
                // `Str`, the string view, displays as its text.
                "StringSlice" if args.is_empty() => true,
                _ => {
                    if self.env.has_impl("Display", name, args) {
                        return true;
                    }
                    if let Some(info) = self.env.structs.get(name) {
                        info.derived_traits.contains("Display")
                    } else if let Some(info) = self.env.enums.get(name) {
                        info.derived_traits.contains("Display")
                    } else {
                        true
                    }
                }
            },
            Type::Rc(inner) | Type::Arc(inner) => self.type_supports_display(inner),
            Type::Shared(name) => {
                if self.env.has_impl("Display", name, &[]) {
                    return true;
                }
                if let Some(info) = self.env.structs.get(name) {
                    info.derived_traits.contains("Display")
                } else if let Some(info) = self.env.enums.get(name) {
                    info.derived_traits.contains("Display")
                } else {
                    true
                }
            }
            Type::TypeParam(_) | Type::TypeVar(_) | Type::AssocProjection { .. } | Type::Error => {
                true
            }
            Type::Never => true,
            Type::Function { .. }
            | Type::OnceFunction { .. }
            | Type::Pointer { .. }
            | Type::Weak(_)
            // `impl Trait` existentials only carry the trait surface,
            // not the witness's derive metadata; the derive-matches-bound
            // path is handled directly in `type_satisfies_bound` by the
            // existential-trait-name comparison, not via these helpers.
            | Type::Existential { .. } => false,
            // Shape-kinded args are not value types — no derive surface.
            Type::Shape(_) => false,
        }
    }

    /// See [`super::env::TypeEnv::type_supports_partial_ord`].
    pub(super) fn type_supports_partial_ord(&self, ty: &Type) -> bool {
        self.env.type_supports_partial_ord(ty)
    }
    /// Returns `true` when `ty` is a distinct type that derives `Arithmetic`.
    pub(super) fn distinct_type_has_arithmetic(&self, ty: &Type) -> bool {
        if let Type::Named { name, args } = ty {
            if args.is_empty() {
                return self
                    .env
                    .distinct_types
                    .get(name)
                    .is_some_and(|t| t.contains("Arithmetic"));
            }
        }
        false
    }

    /// Env-aware `Self` type for a `<recv>.clone()` CALL — the method-side
    /// twin of [`Self::type_supports_clone`] (the bound-side predicate).
    /// Returns `Some(Self)` when the receiver has a callable `clone`, `None`
    /// when the arm must fall through to ordinary method resolution.
    ///
    /// B-2026-07-29-27 / B-2026-07-29-31: the two halves of `Clone` had drifted
    /// apart. Bound DISCHARGE consulted `type_supports_clone`, which honours
    /// `#[derive(Clone)]` and `impl Clone` on user types — so a `T: Clone`
    /// bound was satisfiable. Method RESOLUTION went through the pure free fn
    /// `clone_self_type_for` (src/typechecker/types.rs), whose allowlist is
    /// `Str`/`Array`/`Vector` plus the named stdlib collections. It takes no
    /// `env`, which is exactly WHY user types were excluded — it cannot consult
    /// `info.derived_traits`. Result: `fn dup[T: Clone](x: T) -> T { x }`
    /// type-checked with a `#[derive(Clone)]` argument while the same body
    /// written `x.clone()` was rejected `no method 'clone' on type 'T'`. The
    /// bound was decorative.
    ///
    /// The fix EXPOSES the method rather than tightening the bound: codegen
    /// already ships every emitter needed (`emit_clone_fn_for_type_expr`
    /// dispatches user structs, user enums, and `Option` via
    /// `emit_option_value_clone_fn`), so nothing had to be invented and code
    /// that compiles today keeps compiling.
    ///
    /// A user-declared `clone` method WINS: when `env.find_method` resolves one
    /// on the receiver's nominal type, this returns `None` so the ordinary
    /// impl-block dispatch runs the user's body (with its own declared return
    /// type). Only types whose `Clone` comes from a derive — i.e. types with no
    /// method to dispatch to — are answered here.
    pub(super) fn clone_receiver_self_type(&self, ty: &Type) -> Option<Type> {
        // Existing stdlib-collection surface first, byte-for-byte unchanged.
        if let Some(t) = super::types::clone_self_type_for(ty) {
            return Some(t);
        }
        match ty {
            // A borrow of a clonable receiver clones the underlying owned type
            // — same unwrapping rule `clone_self_type_for` applies.
            Type::Ref(inner) | Type::MutRef(inner) => self.clone_receiver_self_type(inner),
            // `Refinement { base }` clones as its base: the predicate is a
            // static fact about the value, and a copy satisfies it too.
            Type::Refinement { base, .. } => self.clone_receiver_self_type(base),
            // `Rc[T]` / `Arc[T]` / a `shared struct` handle: `clone` is the
            // refcount bump (the canonical RC surface).
            Type::Rc(_) | Type::Arc(_) => Some(ty.clone()),
            // A TUPLE clones when every component does — the same
            // structural rule `type_supports_clone` has always applied to
            // `Type::Tuple`, which is why a `T: Clone` bound already discharged
            // against one. Only METHOD RESOLUTION was missing, so
            // `fn f[T: Clone](x: T)` accepted a tuple while `t.clone()` on the
            // same value reported `no method 'clone' on type '(i64, String)'`.
            // The bound was decorative here in exactly the way B-2026-08-17-33
            // describes for user types (B-2026-08-27-23).
            Type::Tuple(elems) => elems
                .iter()
                .all(Self::tuple_component_clone_codegen_safe)
                .then(|| ty.clone()),
            Type::Shared(name) => {
                if self.user_clone_method_exists(name, &[]) {
                    return None;
                }
                Some(ty.clone())
            }
            Type::Named { name, args } => {
                // A user-written `fn clone` (inherent or via `impl Clone`) owns
                // the call — fall through to impl dispatch.
                if self.user_clone_method_exists(name, args) {
                    return None;
                }
                // B-2026-10-02-59 — a `shared enum` / `sync enum` clones as a
                // refcount bump, like the `Type::Shared` arm above (which only
                // ever sees STRUCT names: lowering leaves enums `Named`).
                if self
                    .env
                    .enums
                    .get(name)
                    .is_some_and(|i| i.is_shared || i.is_par)
                {
                    return Some(ty.clone());
                }
                // `Option[T]`: structurally clonable when the payload is. Not
                // payload-heap-specific — `Option[i64]` was rejected too.
                if name == "Option" {
                    return args
                        .iter()
                        .all(|a| self.type_supports_clone(a))
                        .then(|| ty.clone());
                }
                // `Result[T, E]` (B-2026-07-30-10): admitted for the DIRECT
                // String/Vec/scalar-halves class codegen's
                // `emit_result_value_clone_fn` deep-copies in place. A half that
                // owns heap in any other shape (user struct/enum, `shared`
                // handle, nested `Option`/`Result`, `Map`) still has no in-place
                // deep-copy helper, so it stays REJECTED — admitting it would let
                // codegen emit a shallow overlay copy that aliases the source
                // buffer and double-frees at the two drops. `result_halves_
                // clone_codegen_safe` is the typecheck-side twin of codegen's
                // `result_field_direct_vecstr_halves_ok` (widened to also accept
                // the all-scalar heap-free case, where shallow == deep).
                // `Result[T, E]`: as `Option`, clonable when both halves are
                // (FX3). MIR clones it variant by variant, each half with its
                // own clone, so the legacy codegen's in-place helper and its
                // shape limits (B-2026-07-30-10) no longer apply.
                if name == "Result" {
                    return args
                        .iter()
                        .all(|a| self.type_supports_clone(a))
                        .then(|| ty.clone());
                }
                // User struct / enum / distinct type carrying `#[derive(Clone)]`
                // (or a `Clone` impl recorded in the impl table).
                let derived = self.env.has_impl("Clone", name, args)
                    || self
                        .env
                        .structs
                        .get(name)
                        .is_some_and(|i| i.derived_traits.contains("Clone"))
                    || self
                        .env
                        .enums
                        .get(name)
                        .is_some_and(|i| i.derived_traits.contains("Clone"))
                    || self
                        .env
                        .distinct_types
                        .get(name)
                        .is_some_and(|t| t.contains("Clone"));
                derived.then(|| ty.clone())
            }
            // Inside a generic body, `x.clone()` on a `T: Clone`-bounded param
            // is the whole point of the bound. Discharged against the declared
            // bounds only — an unbounded `T` still gets the `no method` error.
            Type::TypeParam(p) => self.type_param_has_clone_bound(p).then(|| ty.clone()),
            // A projection a where clause bounds (`x: I.Item` under
            // `where I.Item: Clone`) likewise (B-2026-10-19-8).
            Type::AssocProjection {
                param,
                assoc,
                args,
                receiver_args,
                ..
            } if args.is_empty() && receiver_args.is_empty() => self
                .enclosing_proj_bounds
                .get(&(param.clone(), assoc.clone()))
                .is_some_and(|bounds| self.bounds_include_clone(bounds))
                .then(|| ty.clone()),
            _ => None,
        }
    }

    /// Which tuple COMPONENTS `emit_tuple_clone_fn` provably clones correctly.
    ///
    /// A strict subset, because the tuple emitter was measured against each
    /// shape rather than assumed. Reading a
    /// tuple element out of a container and cloning it:
    ///
    ///   (i64, String)          ok      (String, String)  ok
    ///   (i64, i64)             ok      (i64, Option[T])  ok for a safe T
    ///   (i64, Vec[i64])        DOUBLE FREE, once the Vec actually owns heap
    ///   (i64, (i64, String))   SILENT MISCOMPILE — printed `94711002536753`
    ///                          where the interpreter printed `32`
    ///   (i64, SomeStruct)      loud codegen error (already rejected)
    ///
    /// The two bad shapes only fail through an INDEX-READ receiver — the same
    /// tuple in a local clones correctly — and the typechecker cannot see the
    /// receiver's shape, so the component class has to exclude them outright.
    /// Admitting them would trade a clean "no method 'clone'" for a double free
    /// and a wrong number, which is the trade this whole line of work exists to
    /// refuse (B-2026-08-27-23).
    fn tuple_component_clone_codegen_safe(ty: &Type) -> bool {
        match ty {
            Type::Refinement { base, .. } => Self::tuple_component_clone_codegen_safe(base),
            Type::Int(_)
            | Type::UInt(_)
            | Type::Float(_)
            | Type::Bool
            | Type::Char
            | Type::Unit
            | Type::Str => true,
            Type::Named { name, args } if name == "Option" && args.len() == 1 => {
                Self::tuple_component_clone_codegen_safe(&args[0])
            }
            _ => false,
        }
    }

    /// True when the program declares a `clone` method on `name` — an inherent
    /// `impl` or a hand-written `impl Clone for name`. Such a method must win
    /// over the derive-driven synthesis, so `clone_receiver_self_type` declines
    /// the receiver and lets ordinary dispatch find the body.
    fn user_clone_method_exists(&self, name: &str, args: &[Type]) -> bool {
        self.env.find_method(name, args, "clone").is_some()
    }

    /// True when the in-scope generic parameter `p` carries a `Clone` bound
    /// (directly, or through a supertrait of one of its bounds). Reads
    /// `enclosing_bounds` — the same table `gat_rhs_satisfies_bound` proves
    /// `TypeParam` bounds against — so `fn dup[T: Clone](x: T) -> T
    /// { x.clone() }` resolves while an unbounded `[T]` still reports
    /// `no method 'clone'`.
    pub(super) fn type_param_has_clone_bound(&self, p: &str) -> bool {
        self.enclosing_bounds
            .get(p)
            .is_some_and(|bounds| self.bounds_include_clone(bounds))
    }

    /// Whether `bounds` name `Clone` or a trait with it as a supertrait.
    fn bounds_include_clone(&self, bounds: &[crate::ast::TraitBound]) -> bool {
        bounds.iter().any(|tb| {
            let trait_name = tb.path.last().cloned().unwrap_or_default();
            trait_name == "Clone"
                || self
                    .env
                    .supertrait_closure_traits(&trait_name)
                    .iter()
                    .any(|st| st == "Clone")
        })
    }

    /// See [`super::env::TypeEnv::type_supports_clone`].
    pub(super) fn type_supports_clone(&self, ty: &Type) -> bool {
        self.env.type_supports_clone(ty)
    }

    /// See [`super::env::TypeEnv::type_supports_debug`].
    pub(super) fn type_supports_debug(&self, ty: &Type) -> bool {
        self.env.type_supports_debug(ty)
    }

    /// Built-in `Numeric` marker trait: satisfied by the primitive numeric
    /// types usable as SIMD `Vector` lanes and `fn f[T: Numeric]` bounds —
    /// `i8`…`i128`/`isize`, `u8`…`u128`, `f32`, `f64`. `usize` is excluded by
    /// design (design.md § Portable SIMD: idiomatic Kāra reserves `usize` for
    /// sizes/indices, not lane/arithmetic data); this mirrors the structural
    /// surrogate it replaced exactly, so `Vector` element acceptance is
    /// unchanged. Not user-derivable or impl-able — `type_satisfies_bound`
    /// routes the `"Numeric"` arm here.
    pub(super) fn type_supports_numeric(&self, ty: &Type) -> bool {
        match ty {
            Type::Int(_) | Type::Float(_) => true,
            Type::UInt(UIntSize::Usize) => false,
            Type::UInt(_) => true,
            _ => false,
        }
    }

    /// See [`super::env::TypeEnv::is_type_copy`].
    pub(super) fn is_type_copy(&self, ty: &Type) -> bool {
        self.env.is_type_copy(ty)
    }

    /// v2 core: the expressions whose type could be made `Copy` by adding
    /// `#[derive(Copy)]` to its declaration ([`Self::copy_derive_decls`]),
    /// for the ownership pass's use-after-move fixes.
    pub(super) fn copy_derivable_expr_spans(
        &self,
        clonable: &FxHashSet<SpanKey>,
    ) -> (
        FxHashMap<SpanKey, (String, crate::resolver::TextEdit)>,
        FxHashSet<SpanKey>,
    ) {
        let decls = self.copy_derive_decls();
        let mut first_steps = FxHashSet::default();
        let edits = self
            .expr_types
            .iter()
            .filter(|(key, _)| !clonable.contains(*key))
            .filter_map(|(key, ty)| {
                if let Some((param, edit)) = self.clone_bound_target(key.0, ty) {
                    return Some((*key, (param, edit)));
                }
                let (name, (edit, _)) = Self::derive_target(ty, &decls, &mut Vec::new())?;
                let mut all = Vec::new();
                Self::derive_names(ty, &decls, &mut all);
                if all.len() > 1 {
                    first_steps.insert(*key);
                }
                Some((*key, (name.to_string(), edit.clone())))
            })
            .collect();
        (edits, first_steps)
    }

    /// Every type [`Self::derive_target`] would derive on, one `karac fix`
    /// run at a time, before a value of `ty` copies or clones.
    fn derive_names(ty: &Type, decls: &FxHashMap<&str, DeriveDecl>, out: &mut Vec<String>) {
        match ty {
            Type::Named { name, args } => {
                for a in args {
                    Self::derive_names(a, decls, out);
                }
                if let Some(decl) = decls.get(name.as_str()) {
                    if !out.contains(name) {
                        out.push(name.clone());
                        for f in &decl.fields {
                            Self::derive_names(f, decls, out);
                        }
                    }
                }
            }
            Type::Tuple(ts) => ts.iter().for_each(|t| Self::derive_names(t, decls, out)),
            Type::Ref(t) | Type::MutRef(t) => Self::derive_names(t, decls, out),
            _ => {}
        }
    }

    /// A type parameter in `ty` whose declaration (on the function or impl
    /// around `offset`) has no `Clone` or `Copy` bound, with the edit that
    /// adds `Clone` to it; the next `karac fix` pass writes the `.clone()`.
    fn clone_bound_target(
        &self,
        offset: usize,
        ty: &Type,
    ) -> Option<(String, crate::resolver::TextEdit)> {
        let param = match ty {
            Type::TypeParam(p) => p,
            Type::Ref(t) | Type::MutRef(t) => match t.as_ref() {
                Type::TypeParam(p) => p,
                _ => return None,
            },
            _ => return None,
        };
        let edit = self.param_bound_edit(offset, param, "Clone")?;
        Some((param.clone(), edit))
    }

    /// B-2026-11-18-25 — design.md §8: a generic body uses only what its
    /// bounds provide. When `ty`, under any borrows, is a type parameter of a
    /// frame in scope and none of its bounds gives any of `traits` (directly
    /// or as a supertrait), its name. `Self` is left to its trait's
    /// supertraits.
    pub(super) fn param_lacking_bound(&self, ty: &Type, traits: &[&str]) -> Option<String> {
        let mut t = ty;
        while let Type::Ref(inner) | Type::MutRef(inner) = t {
            t = inner;
        }
        let Type::TypeParam(p) = t else {
            return None;
        };
        if p == "Self" || !self.enclosing_bounds.contains_key(p) {
            return None;
        }
        (!traits
            .iter()
            .any(|tr| self.type_param_satisfies_bound(p, tr)))
        .then(|| p.clone())
    }

    /// The finding for [`Self::param_lacking_bound`]: `what` (an operator or
    /// an interpolation) on `param` needs `param: add`, with the edit adding
    /// the bound where the parameter is declared. 0.8.x still compares or
    /// renders the value by its shape, so this is a `deprecated` warning, as
    /// a derive over a field that lacks the trait is (B-2026-11-16-330); it
    /// becomes an error at the next minor release.
    pub(super) fn report_param_lacks_bound(
        &mut self,
        param: &str,
        add: &str,
        what: &str,
        span: Span,
    ) {
        self.note_implied_bound(param, add, what, span);
        let does = if add == "Display" {
            "renders"
        } else {
            "compares"
        };
        let message = format!(
            "{what} on `{param}` needs `{param}: {add}`: a generic body uses only what its \
             bounds provide (design.md §8), and no bound of `{param}` gives `{add}`. 0.8.x \
             still {does} the value by its shape; this is deprecated and becomes an error in \
             the next minor release. Add `{add}` where `{param}` is declared"
        );
        let decl = self
            .param_scopes_at(span.offset)
            .into_iter()
            .flatten()
            .flat_map(|g| g.params.iter())
            .find(|g| g.name == param && !g.is_const)
            .map(|g| g.span);
        let fix = match (self.param_bound_edit(span.offset, param, add), decl) {
            (Some(edit), Some(d)) => Some(super::FixIt {
                span: Span {
                    offset: edit.offset,
                    length: 0,
                    line: d.line,
                    column: d.column + d.length,
                },
                replacement: edit.replacement,
            }),
            _ => None,
        };
        self.type_lint_warning_with_fix(
            message,
            span,
            TypeErrorKind::Deprecated,
            "deprecated",
            fix,
        );
    }

    /// Does the in-scope type parameter `p` carry `trait_name`, directly or
    /// through a supertrait (`Copy` counts as `Clone`)?
    pub(super) fn type_param_satisfies_bound(&self, p: &str, trait_name: &str) -> bool {
        self.enclosing_bounds.get(p).is_some_and(|bounds| {
            bounds.iter().any(|tb| {
                let t = tb.path.last().cloned().unwrap_or_default();
                t == trait_name
                    || (trait_name == "Clone" && t == "Copy")
                    || self
                        .env
                        .supertrait_closure_traits(&t)
                        .iter()
                        .any(|st| st == trait_name)
            })
        })
    }

    /// The generic parameter lists in scope at `offset`, innermost first: a
    /// method's before its impl's or trait's, and a struct's, enum's or
    /// alias's own (B-2026-10-27-25).
    pub(super) fn param_scopes_at(&self, offset: usize) -> Vec<&'a Option<GenericParams>> {
        let inside = |sp: &Span| sp.offset <= offset && offset < sp.offset + sp.length;
        let mut scopes: Vec<&'a Option<GenericParams>> = Vec::new();
        let program: &'a Program = self.program;
        for item in &program.items {
            match item {
                Item::Function(f) if inside(&f.span) => scopes.push(&f.generic_params),
                Item::ImplBlock(imp) if inside(&imp.span) => {
                    for it in &imp.items {
                        if let ImplItem::Method(f) = it {
                            if inside(&f.span) {
                                scopes.push(&f.generic_params);
                            }
                        }
                    }
                    scopes.push(&imp.generic_params);
                }
                Item::TraitDef(t) if inside(&t.span) => {
                    for it in &t.items {
                        if let TraitItem::Method(m) = it {
                            if inside(&m.span) {
                                scopes.push(&m.generic_params);
                            }
                        }
                    }
                    scopes.push(&t.generic_params);
                }
                Item::StructDef(s) if inside(&s.span) => scopes.push(&s.generic_params),
                Item::EnumDef(e) if inside(&e.span) => scopes.push(&e.generic_params),
                Item::TypeAlias(t) if inside(&t.span) => scopes.push(&t.generic_params),
                _ => {}
            }
        }
        scopes
    }

    /// The edit adding `trait_name` to the declaration of type parameter
    /// `param` on the function or impl around `offset`, or `None` when that
    /// declaration already names `Clone` / `Copy` (for `Clone`) or the trait.
    pub(super) fn param_bound_edit(
        &self,
        offset: usize,
        param: &str,
        trait_name: &str,
    ) -> Option<crate::resolver::TextEdit> {
        let gp = self
            .param_scopes_at(offset)
            .into_iter()
            .flatten()
            .flat_map(|g| g.params.iter())
            .find(|g| g.name == param && !g.is_const)?;
        if gp.span.length == 0
            || gp.bounds.iter().any(|b| {
                b.path
                    .last()
                    .is_some_and(|t| t == trait_name || (trait_name == "Clone" && t == "Copy"))
            })
        {
            return None;
        }
        Some(crate::resolver::TextEdit {
            offset: gp.span.offset + gp.span.length,
            length: 0,
            replacement: if gp.bounds.is_empty() {
                format!(": {trait_name}")
            } else {
                format!(" + {trait_name}")
            },
        })
    }

    /// v2 core: the derive that makes a copy of `ty` possible, when it is one
    /// of [`Self::copy_derive_decls`]: `Copy`, or `Clone` (whose `.clone()`
    /// the next `karac fix` pass writes at the site).
    pub(super) fn copy_derive_fix(&self, ty: &Type) -> Option<super::FixIt> {
        self.copy_derive_fix_named(ty).map(|(_, f)| f)
    }

    /// [`Self::copy_derive_fix`] with the name of the declaration it edits.
    pub(super) fn copy_derive_fix_named(&self, ty: &Type) -> Option<(String, super::FixIt)> {
        let decls = self.copy_derive_decls();
        let (name, (edit, line)) = Self::derive_target(ty, &decls, &mut Vec::new())?;
        Some((
            name.to_string(),
            super::FixIt {
                span: Span {
                    offset: edit.offset,
                    length: 0,
                    line: *line,
                    column: 1,
                },
                replacement: edit.replacement.clone(),
            },
        ))
    }

    /// The declaration whose derive comes first on the way to making `ty`
    /// copyable: a field's or type argument's local type before the type
    /// that holds it, since a derive needs its parts to have the trait. One
    /// derive per `karac fix` pass; the next pass finds the next one.
    #[allow(clippy::type_complexity)]
    fn derive_target<'d>(
        ty: &Type,
        decls: &'d FxHashMap<&str, DeriveDecl>,
        seen: &mut Vec<String>,
    ) -> Option<(&'d str, &'d (crate::resolver::TextEdit, usize))> {
        match ty {
            Type::Named { name, args } => {
                for a in args {
                    if let Some(t) = Self::derive_target(a, decls, seen) {
                        return Some(t);
                    }
                }
                let (key, decl) = decls.get_key_value(name.as_str())?;
                if seen.contains(name) {
                    return None;
                }
                seen.push(name.clone());
                for f in &decl.fields {
                    if let Some(t) = Self::derive_target(f, decls, seen) {
                        return Some(t);
                    }
                }
                // A generic type takes only the `Clone` derive.
                if !args.is_empty() && !decl.edit.0.adds_clone() {
                    return None;
                }
                Some((key, &decl.edit))
            }
            Type::Tuple(ts) => ts.iter().find_map(|t| Self::derive_target(t, decls, seen)),
            Type::Ref(t) | Type::MutRef(t) => Self::derive_target(t, decls, seen),
            _ => None,
        }
    }

    /// The local types that could be made `Copy` or `Clone` by deriving it:
    /// structs and enums of this program (not `shared` / `par`, not from the
    /// stdlib) with no `Clone` or `Copy` derive. `Copy` when the type is not
    /// generic, has no `Drop` body and every field is `Copy`; otherwise
    /// `Clone` when every field clones (a type parameter's bound propagates,
    /// design.md §9). A field of another such type counts as having the trait,
    /// since [`Self::derive_target`] derives it first. The edit goes at the
    /// start of the declaration's line, ahead of any `pub` / `shared` before
    /// the keyword its span starts at.
    fn copy_derive_decls(&self) -> FxHashMap<&str, DeriveDecl> {
        let drops: FxHashSet<&str> = self
            .env
            .impls
            .iter()
            .filter(|imp| imp.trait_name.as_deref() == Some("Drop"))
            .map(|imp| imp.target_type.as_str())
            .collect();
        // (name, edit offset, line, fields, may be `Copy`)
        let mut cands: Vec<(&str, usize, usize, Vec<Type>, bool)> = Vec::new();
        for item in &self.program.items {
            let (name, span, fields, generic) = match item {
                Item::StructDef(s) if !s.is_shared && !s.is_par => {
                    let Some(info) = self.env.structs.get(&s.name) else {
                        continue;
                    };
                    if info.defining_stdlib_origin
                        || info.derived_traits.contains("Copy")
                        || info.derived_traits.contains("Clone")
                    {
                        continue;
                    }
                    let fields: Vec<Type> = info.fields.iter().map(|(_, t, _)| t.clone()).collect();
                    (s.name.as_str(), s.span, fields, s.generic_params.is_some())
                }
                Item::EnumDef(e) if !e.is_shared && !e.is_par => {
                    let Some(info) = self.env.enums.get(&e.name) else {
                        continue;
                    };
                    if info.defining_stdlib_origin
                        || info.derived_traits.contains("Copy")
                        || info.derived_traits.contains("Clone")
                    {
                        continue;
                    }
                    let fields: Vec<Type> = info
                        .variants
                        .iter()
                        .flat_map(|(_, v)| match v {
                            VariantTypeInfo::Unit => Vec::new(),
                            VariantTypeInfo::Tuple(ts) => ts.clone(),
                            VariantTypeInfo::Struct(fs) => {
                                fs.iter().map(|(_, t)| t.clone()).collect()
                            }
                        })
                        .collect();
                    (e.name.as_str(), e.span, fields, e.generic_params.is_some())
                }
                _ => continue,
            };
            if span.length == 0 || span.column == 0 || span.offset < span.column - 1 {
                continue;
            }
            // A type of the appended library or a spliced library module is
            // not the program's to edit: its span lies past the program's
            // text, so the edit would land in some other file
            // (B-2026-10-19-17).
            let library = self.program.library_start.unwrap_or(usize::MAX);
            if span.offset >= library.min(crate::prelude::gated_span_base(0)) {
                continue;
            }
            let may_copy = !generic && !drops.contains(name);
            cands.push((
                name,
                span.offset - (span.column - 1),
                span.line,
                fields,
                may_copy,
            ));
        }
        // The largest set closed under "every field has the trait, counting
        // a field of a member type as having it": start from every candidate
        // and drop the ones that fail, so a recursive type (`enum N { L(Vec[N])
        // }`) can derive. A member type's field stands in as an `i64`, which
        // is both `Copy` and `Clone`.
        let shrink = |trait_ok: &dyn Fn(&Type) -> bool, copy: bool| {
            let mut set: FxHashSet<&str> =
                cands.iter().filter(|c| !copy || c.4).map(|c| c.0).collect();
            // `Copy` is the smallest such set instead: a type that holds
            // itself (`struct Node { next: Option[Node] }`) is boxed, so it
            // is never `Copy`, while it may still clone (B-2026-11-16-178).
            if copy {
                let mut grown: FxHashSet<&str> = FxHashSet::default();
                loop {
                    let before = grown.len();
                    for c in cands.iter().filter(|c| set.contains(c.0)) {
                        if !grown.contains(c.0)
                            && c.3
                                .iter()
                                .all(|t| trait_ok(&Self::stand_in_members(t, &grown, false)))
                        {
                            grown.insert(c.0);
                        }
                    }
                    if grown.len() == before {
                        return grown;
                    }
                }
            }
            loop {
                let before = set.len();
                let failing: Vec<&str> = cands
                    .iter()
                    .filter(|c| set.contains(c.0))
                    .filter(|c| {
                        !c.3.iter()
                            .all(|t| trait_ok(&Self::stand_in_members(t, &set, !copy)))
                    })
                    .map(|c| c.0)
                    .collect();
                for n in failing {
                    set.remove(n);
                }
                if set.len() == before {
                    return set;
                }
            }
        };
        let copy_set = shrink(&|t| self.is_type_copy(t), true);
        let clone_set = shrink(&|t| self.type_supports_clone(t), false);
        cands
            .into_iter()
            .filter_map(|(name, offset, line, fields, _)| {
                // `Copy` needs `Clone` (a derive never adds another), so the
                // `Copy` derive names both
                let derive = if copy_set.contains(name) {
                    "Copy, Clone"
                } else if clone_set.contains(name) {
                    "Clone"
                } else {
                    return None;
                };
                Some((
                    name,
                    DeriveDecl {
                        edit: (
                            crate::resolver::TextEdit {
                                offset,
                                length: 0,
                                replacement: format!("#[derive({derive})]\n"),
                            },
                            line,
                        ),
                        fields,
                    },
                ))
            })
            .collect()
    }

    /// `t` with each type named in `set` (and, when `params`, each type
    /// parameter) replaced by `i64`.
    fn stand_in_members(t: &Type, set: &FxHashSet<&str>, params: bool) -> Type {
        match t {
            Type::Named { name, .. } if set.contains(name.as_str()) => {
                Type::Int(super::types::IntSize::I64)
            }
            Type::TypeParam(_) if params => Type::Int(super::types::IntSize::I64),
            Type::Named { name, args } => Type::Named {
                name: name.clone(),
                args: args
                    .iter()
                    .map(|a| Self::stand_in_members(a, set, params))
                    .collect(),
            },
            Type::Tuple(ts) => Type::Tuple(
                ts.iter()
                    .map(|a| Self::stand_in_members(a, set, params))
                    .collect(),
            ),
            _ => t.clone(),
        }
    }

    /// Validate that #[derive(Copy)] structs/enums have all-Copy fields, and
    /// that distinct types with #[derive(Copy)] have a Copy base type.
    pub(super) fn validate_derive_copy(&mut self) {
        // On the MIR pipeline a derived `Copy` on a generic struct is
        // conditional, as the MIR's own copy test reads it: `Range[T]` copies
        // when its `T` does.
        self.validate_derived_trait("Copy", |this, ty| {
            this.is_type_copy(ty)
                || (this.library_methods_from_source && matches!(ty, Type::TypeParam(_)))
        });
        self.reject_recursive_copy_derives();
        // Check distinct types: base type must be Copy.
        let distinct_items: Vec<_> = self
            .program
            .items
            .iter()
            .filter_map(|item| {
                if let Item::DistinctType(d) = item {
                    let traits = extract_derived_traits(&d.attributes);
                    if traits.contains("Copy") {
                        return Some((
                            d.name.clone(),
                            d.span,
                            d.base_type.clone(),
                            distinct_scope(d),
                        ));
                    }
                }
                None
            })
            .collect();
        for (name, span, base_ty_expr, scope) in distinct_items {
            let base_ty = self.lower_type_expr(&base_ty_expr, &scope);
            // A type parameter is conditional, as a generic struct's field
            // is: `Wrap[T]` copies when its `T` does (B-2026-11-18-43).
            if !self.is_type_copy(&base_ty) && !matches!(base_ty, Type::TypeParam(_)) {
                self.type_error(
                    format!(
                        "distinct type '{}' derives Copy but its base type '{}' is not Copy",
                        name,
                        type_display(&base_ty)
                    ),
                    span,
                    TypeErrorKind::TypeMismatch,
                );
            }
        }
    }

    /// A `Copy` type cannot hold itself: `struct Node { next: Option[Node] }`
    /// is boxed, and a copy of it would have no end. The field check above
    /// passes such a type, since its own derive makes the field `Copy`, and
    /// the MIR's copy test then recursed until karac's stack overflowed
    /// (B-2026-11-16-178).
    fn reject_recursive_copy_derives(&mut self) {
        fn holds(
            tc: &super::TypeChecker<'_>,
            ty: &Type,
            target: &str,
            seen: &mut FxHashSet<String>,
        ) -> bool {
            match ty {
                Type::Named { name, args } => {
                    if args.iter().any(|a| holds(tc, a, target, seen)) {
                        return true;
                    }
                    if name == target {
                        return true;
                    }
                    if !seen.insert(name.clone()) {
                        return false;
                    }
                    if let Some(info) = tc.env.structs.get(name) {
                        info.derived_traits.contains("Copy")
                            && info
                                .fields
                                .iter()
                                .any(|(_, t, _)| holds(tc, t, target, seen))
                    } else if let Some(info) = tc.env.enums.get(name) {
                        info.derived_traits.contains("Copy")
                            && info.variants.iter().any(|(_, v)| match v {
                                VariantTypeInfo::Unit => false,
                                VariantTypeInfo::Tuple(ts) => {
                                    ts.iter().any(|t| holds(tc, t, target, seen))
                                }
                                VariantTypeInfo::Struct(fs) => {
                                    fs.iter().any(|(_, t)| holds(tc, t, target, seen))
                                }
                            })
                    } else {
                        false
                    }
                }
                Type::Tuple(ts) => ts.iter().any(|t| holds(tc, t, target, seen)),
                Type::Array { element, .. } => holds(tc, element, target, seen),
                _ => false,
            }
        }
        let mut found: Vec<(String, Span, String, Type)> = Vec::new();
        for item in &self.program.items {
            let (name, span, fields): (&String, Span, Vec<(String, Type)>) = match item {
                Item::StructDef(s) => {
                    let Some(info) = self.env.structs.get(&s.name) else {
                        continue;
                    };
                    if !info.derived_traits.contains("Copy") {
                        continue;
                    }
                    let fields = info
                        .fields
                        .iter()
                        .map(|(n, t, _)| (n.clone(), t.clone()))
                        .collect();
                    (&s.name, s.span, fields)
                }
                Item::EnumDef(e) => {
                    let Some(info) = self.env.enums.get(&e.name) else {
                        continue;
                    };
                    if !info.derived_traits.contains("Copy") {
                        continue;
                    }
                    let fields = info
                        .variants
                        .iter()
                        .flat_map(|(vn, v)| match v {
                            VariantTypeInfo::Unit => Vec::new(),
                            VariantTypeInfo::Tuple(ts) => ts
                                .iter()
                                .enumerate()
                                .map(|(i, t)| (format!("{vn}.{i}"), t.clone()))
                                .collect(),
                            VariantTypeInfo::Struct(fs) => fs
                                .iter()
                                .map(|(n, t)| (format!("{vn}.{n}"), t.clone()))
                                .collect(),
                        })
                        .collect();
                    (&e.name, e.span, fields)
                }
                _ => continue,
            };
            if let Some((field, ty)) = fields
                .into_iter()
                .find(|(_, t)| holds(self, t, name, &mut FxHashSet::default()))
            {
                found.push((name.clone(), span, field, ty));
            }
        }
        for (name, span, field, ty) in found {
            self.type_error(
                format!(
                    "'{name}' derives Copy but holds itself (field '{field}': '{}'): a type \
                     that holds itself is boxed, so it cannot be `Copy`; derive `Clone` \
                     instead and take a copy with `.clone()`",
                    type_display(&ty)
                ),
                span,
                TypeErrorKind::TypeMismatch,
            );
        }
    }

    /// Validate that every `#[derive(Trait)]` on a struct/enum implies all
    /// fields recursively support `Trait`. Reports one diagnostic per
    /// offending field. `Copy` is handled separately via `validate_derive_copy`
    /// so the message can reference `is_type_copy`'s broader rules.
    pub(super) fn validate_derived_traits_recursive(&mut self) {
        self.validate_derived_trait("Eq", |this, ty| this.type_supports_eq(ty));
        self.validate_derived_trait("PartialEq", |this, ty| this.type_supports_partial_eq(ty));
        self.validate_derived_trait("Hash", |this, ty| this.type_supports_hash(ty));
        self.validate_derived_trait("Ord", |this, ty| this.type_supports_ord(ty));
        self.validate_derived_trait("PartialOrd", |this, ty| this.type_supports_partial_ord(ty));
        self.validate_derived_trait("Pod", |this, ty| this.env.type_supports_pod(ty));
        self.validate_explicit_pod_impls();
        self.validate_derive_pod_on_enums();
        self.validate_derive_default();
        self.validate_derive_display_on_enums();
        self.validate_derive_serde("Serialize");
        self.validate_derive_serde("Deserialize");
    }

    /// design.md §9: `#[derive(Serialize)]` / `#[derive(Deserialize)]` needs
    /// the trait on every field. The derive's code is generated after this
    /// check, and type errors inside it are not reported against the user's
    /// text, so a field of a program type without the trait checked clean and
    /// failed at run with "no method 'serialize'" (B-2026-11-18-21). The rule
    /// is checked here, on the declaration, for the parts a program declares;
    /// the library's types and type parameters are left to the generated
    /// impl's bounds.
    fn validate_derive_serde(&mut self, trait_name: &str) {
        if !self.is_comptime_derive_fn(&format!(
            "derive_{}",
            crate::comptime::to_snake_case(trait_name)
        )) {
            return;
        }
        // An impl for every type meeting a bound covers types this walk
        // cannot judge.
        let blanket = self.env.impls_by_trait.get(trait_name).is_some_and(|ix| {
            ix.iter().any(|&i| {
                let imp = &self.env.impls[i];
                imp.generic_params
                    .as_ref()
                    .is_some_and(|g| g.params.iter().any(|p| p.name == imp.target_type))
            })
        });
        if blanket {
            return;
        }
        let library = self.program.library_start.unwrap_or(usize::MAX);
        let mut parts: Vec<(String, String, Type, Span)> = Vec::new();
        let mut skipped_without_default: Vec<(String, String, Type, Span)> = Vec::new();
        for item in &self.program.items {
            match item {
                Item::StructDef(s)
                    if s.span.offset < library
                        && extract_derived_traits(&s.attributes).contains(trait_name) =>
                {
                    let Some(info) = self.env.structs.get(&s.name) else {
                        continue;
                    };
                    for (field, ty, _) in &info.fields {
                        let decl = s.fields.iter().find(|f| &f.name == field);
                        let span = decl.map_or(s.span, |f| f.span);
                        // A field `#[serde(skip)]` leaves off the wire in
                        // this direction needs nothing of the trait, as in
                        // serde: a cache, a handle or a lock (B-2026-11-20-27).
                        // Read back, it is its `default: expr` or its type's
                        // `Default`, which it must then have.
                        let (skip_ser, skip_de, default_expr) = decl
                            .map_or((false, false, false), |f| serde_field_skips(&f.attributes));
                        let skipped = if trait_name == "Serialize" {
                            skip_ser
                        } else {
                            skip_de
                        };
                        if skipped {
                            if trait_name == "Deserialize"
                                && !default_expr
                                && !self.derived_default_field_ok(ty)
                            {
                                skipped_without_default.push((
                                    s.name.clone(),
                                    field.clone(),
                                    ty.clone(),
                                    span,
                                ));
                            }
                            continue;
                        }
                        parts.push((
                            format!("struct '{}'", s.name),
                            format!("field '{field}'"),
                            ty.clone(),
                            span,
                        ));
                    }
                }
                Item::EnumDef(e)
                    if e.span.offset < library
                        && extract_derived_traits(&e.attributes).contains(trait_name) =>
                {
                    let Some(info) = self.env.enums.get(&e.name) else {
                        continue;
                    };
                    for (variant, v) in &info.variants {
                        let fields: Vec<(String, Type)> = match v {
                            VariantTypeInfo::Unit => Vec::new(),
                            VariantTypeInfo::Tuple(tys) => tys
                                .iter()
                                .enumerate()
                                .map(|(i, t)| (i.to_string(), t.clone()))
                                .collect(),
                            VariantTypeInfo::Struct(fs) => fs.clone(),
                        };
                        for (field, ty) in fields {
                            parts.push((
                                format!("enum '{}'", e.name),
                                format!("variant '{variant}' field '{field}'"),
                                ty,
                                e.span,
                            ));
                        }
                    }
                }
                Item::DistinctType(d)
                    if d.span.offset < library
                        && extract_derived_traits(&d.attributes).contains(trait_name) =>
                {
                    let Some(base) = self.env.distinct_bases.get(&d.name) else {
                        continue;
                    };
                    parts.push((
                        format!("distinct type '{}'", d.name),
                        "its base".to_string(),
                        base.clone(),
                        d.span,
                    ));
                }
                _ => {}
            }
        }
        for (owner, field, ty, span) in skipped_without_default {
            self.type_error(
                format!(
                    "struct '{owner}' derives Deserialize and skips field '{field}', which is \
                     then built as its type's `Default`, but '{}' does not implement Default; \
                     give it a value with `#[serde(default: expr)]`, or implement `Default` \
                     for it (design.md § Serialize and Deserialize)",
                    type_display(&ty)
                ),
                span,
                TypeErrorKind::TypeMismatch,
            );
        }
        for (owner, part, ty, span) in parts {
            // design.md § Secret Type: a `Secret` is never serialized
            // implicitly. The library's `Secret` has no serde impl, so the
            // derive checked and every lane refused its code with "no method
            // 'serialize' on type 'Secret'" (B-2026-11-19-63).
            if self.serde_secret_part(&ty) {
                self.type_error(
                    format!(
                        "{owner} derives {trait_name} but {part} has type '{}', which holds a \
                         `Secret`: a secret is never {} by a derive (design.md § Secret \
                         Type). Write `impl {trait_name}` for {owner} by hand, deciding what \
                         the secret field becomes on the wire, or derive it on a non-secret \
                         mirror type",
                        type_display(&ty),
                        if trait_name == "Serialize" {
                            "serialized"
                        } else {
                            "deserialized"
                        },
                    ),
                    span,
                    TypeErrorKind::TypeMismatch,
                );
                continue;
            }
            let Some(missing) = self.serde_missing_part(&ty, trait_name) else {
                continue;
            };
            if !self.program_declares_type(&missing) {
                // Review-thread rulings 2026-11-26 (B-2026-11-25-49): why
                // each of these has no impl, and what to write instead.
                let instead = match missing.as_str() {
                    "i128" | "u128" => "a JSON number past 2^53 loses precision in most \
                         readers; write the field as an `i64`, or as a `String` the program \
                         converts"
                        .to_string(),
                    "Result" => "a `Result` has no canonical JSON shape; write an enum \
                         with the variants meant, tagged with `#[serde(..)]`"
                        .to_string(),
                    "Array" => "`Array` support is planned (docs/library/json.md); write \
                         the field as a `Vec` for now"
                        .to_string(),
                    _ => format!(
                        "write the field as a type that has one, or as a program type with \
                         its own `impl {trait_name}`"
                    ),
                };
                self.type_error(
                    format!(
                        "{owner} derives {trait_name} but {part} has type '{}', and the \
                         library has no {trait_name} impl for '{missing}': {instead} \
                         (design.md §9)",
                        type_display(&ty)
                    ),
                    span,
                    TypeErrorKind::TypeMismatch,
                );
                continue;
            }
            self.type_error(
                format!(
                    "{owner} derives {trait_name} but {part} has type '{}', and '{missing}' \
                     does not implement {trait_name}; add `#[derive({trait_name})]` to \
                     `{missing}`'s declaration, or write `impl {trait_name} for {missing}` \
                     (design.md §9)",
                    type_display(&ty)
                ),
                span,
                TypeErrorKind::TypeMismatch,
            );
        }
    }

    /// Whether `ty` holds a `std.secret.Secret` anywhere a derive would
    /// serialize it.
    fn serde_secret_part(&self, ty: &Type) -> bool {
        match ty {
            Type::Named { name, args } => {
                (name == "Secret"
                    && self
                        .env
                        .structs
                        .get("Secret")
                        .is_some_and(|i| i.defining_stdlib_origin))
                    || args.iter().any(|a| self.serde_secret_part(a))
            }
            Type::Tuple(elems) => elems.iter().any(|e| self.serde_secret_part(e)),
            Type::Array { element, .. }
            | Type::Slice { element, .. }
            | Type::Vector { element, .. } => self.serde_secret_part(element),
            Type::Ref(inner) | Type::MutRef(inner) | Type::Rc(inner) | Type::Arc(inner) => {
                self.serde_secret_part(inner)
            }
            Type::Refinement { base, .. } => self.serde_secret_part(base),
            _ => false,
        }
    }

    /// The first type in `ty` that the program declares and that has neither
    /// a derive nor an impl of `trait_name`.
    fn serde_missing_part(&self, ty: &Type, trait_name: &str) -> Option<String> {
        match ty {
            Type::Named { name, args } => {
                let declared = (self.env.structs.contains_key(name)
                    || self.env.enums.contains_key(name)
                    || self.env.distinct_bases.contains_key(name))
                    && self.program_declares_type(name);
                if declared
                    && !self.comptime_derives(name, trait_name)
                    && !self.env.impls_by_trait.get(trait_name).is_some_and(|ix| {
                        ix.iter().any(|&i| self.env.impls[i].target_type == *name)
                    })
                {
                    return Some(name.clone());
                }
                args.iter()
                    .find_map(|a| self.serde_missing_part(a, trait_name))
                    .or_else(|| {
                        (!declared && self.library_lacks(ty, trait_name)).then(|| name.clone())
                    })
            }
            // A primitive or a library type the library has no impl for
            // (`i128`, `u128`, `Array`, `Result`): the derive refuses it by
            // name, as a direct `Json.serialize` of one is refused, rather
            // than leave it to every backend (B-2026-11-25-49).
            Type::Int(_) | Type::UInt(_) | Type::Float(_) | Type::Bool | Type::Char => {
                self.library_lacks(ty, trait_name).then(|| type_display(ty))
            }
            Type::Tuple(elems) => elems
                .iter()
                .find_map(|e| self.serde_missing_part(e, trait_name)),
            Type::Array { element, .. } => {
                self.serde_missing_part(element, trait_name).or_else(|| {
                    self.library_lacks(ty, trait_name)
                        .then(|| "Array".to_string())
                })
            }
            Type::Slice { element, .. } | Type::Vector { element, .. } => {
                self.serde_missing_part(element, trait_name)
            }
            Type::Ref(inner) | Type::MutRef(inner) | Type::Rc(inner) | Type::Arc(inner) => {
                self.serde_missing_part(inner, trait_name)
            }
            Type::Refinement { base, .. } => self.serde_missing_part(base, trait_name),
            _ => None,
        }
    }

    /// Whether the library has no `trait_name` impl for `ty`'s head. The
    /// head is asked with `i64` for each argument: the arguments are asked
    /// part by part, and one the bound cannot see yet (a distinct type
    /// whose impl a derive writes, a tuple the derive refuses by its own
    /// rule) must not make `Vec` look unimplemented.
    fn library_lacks(&self, ty: &Type, trait_name: &str) -> bool {
        let i64_ty = Type::Int(super::types::IntSize::I64);
        let head = match ty {
            Type::Named { name, args } => Type::Named {
                name: name.clone(),
                args: args.iter().map(|_| i64_ty.clone()).collect(),
            },
            Type::Array { .. } => {
                let mut head = ty.clone();
                if let Type::Array { element, .. } = &mut head {
                    **element = i64_ty;
                }
                head
            }
            _ => ty.clone(),
        };
        !self.type_satisfies_bound(&head, trait_name)
    }

    /// design.md §9 Derive, two list rules (B-2026-11-06-27 items 3-5):
    /// - `Drop` is not derivable (§9 Drop: no impl means "do nothing", and
    ///   field drops are what the compiler runs after any `drop` body);
    /// - a derive and a manual impl of the same trait on one type is the
    ///   ordinary duplicate-impl error;
    /// - an impl whose `Self` is `dyn Trait` is refused: a trait object's
    ///   methods are its trait's, so user code cannot add impls to one.
    pub(super) fn validate_derive_lists(&mut self) {
        const DERIVABLE: &[&str] = &[
            "PartialEq",
            "Eq",
            "PartialOrd",
            "Ord",
            "Hash",
            "Clone",
            "Copy",
            "Default",
            "Display",
            "Debug",
            "Serialize",
            "Deserialize",
            "Arithmetic",
        ];
        let mut derived: FxHashMap<String, (std::collections::HashSet<String>, Span)> =
            FxHashMap::default();
        let mut problems: Vec<(String, Span, TypeErrorKind)> = Vec::new();
        for item in &self.program.items {
            let (name, attrs, span) = match item {
                Item::StructDef(d) => (&d.name, &d.attributes, d.span),
                Item::EnumDef(d) => (&d.name, &d.attributes, d.span),
                _ => continue,
            };
            let traits = extract_derived_traits(attrs);
            if traits.contains("Drop") {
                problems.push((
                    format!(
                        "`Drop` is not derivable: `{name}` with no `impl Drop` already does \
                         nothing of its own when dropped, and its fields are dropped after any \
                         `drop` body; remove `Drop` from the derive, or write \
                         `impl Drop for {name} {{ fn drop(mut ref self) {{ ... }} }}` (design.md §9)"
                    ),
                    span,
                    TypeErrorKind::TypeMismatch,
                ));
            }
            derived.insert(name.clone(), (traits, span));
        }
        for item in &self.program.items {
            let Item::ImplBlock(imp) = item else {
                continue;
            };
            let Some(trait_name) = imp.trait_name.as_ref().and_then(|p| p.segments.last()) else {
                continue;
            };
            match &imp.target_type.kind {
                TypeKind::Dyn { .. } => problems.push((
                    format!(
                        "`impl {trait_name} for dyn ...` is not allowed: a trait object's \
                         methods are those of its trait, and an impl cannot have a `dyn` type \
                         as `Self`; add the method to the trait (with a default body) or write \
                         a function that takes the trait object (design.md §9)"
                    ),
                    imp.span,
                    TypeErrorKind::ConflictingImpl,
                )),
                TypeKind::Path(path) if path.segments.len() == 1 => {
                    let target = &path.segments[0];
                    let Some((traits, def_span)) = derived.get(target) else {
                        continue;
                    };
                    if !DERIVABLE.contains(&trait_name.as_str()) || !traits.contains(trait_name) {
                        continue;
                    }
                    // A derive a library `comptime fn derive_*` expands
                    // (`Serialize`) becomes an impl spliced in after the first
                    // check, spanned over the type's definition: a clash is a
                    // second impl, seen at the re-check after expansion.
                    let conflicting = if self.comptime_derives(target, trait_name) {
                        imp.span != *def_span
                            && self.program.items.iter().any(|it| {
                                matches!(it, Item::ImplBlock(other)
                                    if other.span == *def_span
                                        && other.trait_name.as_ref().and_then(|p| p.segments.last())
                                            == Some(trait_name))
                            })
                    } else {
                        true
                    };
                    if conflicting {
                        problems.push((
                            format!(
                                "conflicting impl: `{target}` derives `{trait_name}` and also \
                                 implements it here; a type implements a trait once, so remove \
                                 the derive or this impl (design.md §9 Derive)"
                            ),
                            imp.span,
                            TypeErrorKind::ConflictingImpl,
                        ));
                    }
                }
                _ => {}
            }
        }
        for (message, span, kind) in problems {
            self.type_error(message, span, kind);
        }
    }

    /// design.md §9 "A derive never adds another" (B-2026-11-06-27 items 1,
    /// 2 and 7). `add_implied_derives` still fills a missing supertrait
    /// derive in for 0.8.x; this is the deprecation warning that goes with
    /// it, carrying the machine-applicable fix that writes the derive into
    /// the same `#[derive(..)]` list. It becomes the error at the next minor
    /// release, the B-2026-11-04-79 transition. The standard library and the
    /// library compiled from source are exempt; `#[allow(deprecated)]` on the
    /// type suppresses it.
    /// `unused_doc_comment`: each `///` run the parser found documenting
    /// nothing (design.md §3 Doc comments: "Otherwise the run is discarded
    /// with a warning"). The user's source only, never the appended library.
    pub(super) fn warn_discarded_doc_comments(&mut self) {
        if self.compiling_stdlib {
            return;
        }
        let library = self.program.library_start.unwrap_or(usize::MAX);
        let found: Vec<(Span, &'static str)> = self
            .program
            .discarded_doc_comments
            .iter()
            .filter(|(span, _)| span.offset < library)
            .copied()
            .collect();
        for (span, why) in found {
            // This runs after the item walk, with no frame pushed: the
            // attribute around the comment sets its level (B-2026-11-18-48).
            let frame: Vec<_> =
                super::attribute_lint_level_at(self.program, &span, "unused_doc_comment")
                    .into_iter()
                    .collect();
            self.lint_override_stack.push(frame);
            self.type_lint_warning_with_fix(
                format!(
                    "this doc comment documents nothing ({why}), so it is discarded \
                     (design.md §3 Doc comments); write `//` for a plain comment"
                ),
                span,
                TypeErrorKind::UnusedDocComment,
                "unused_doc_comment",
                None,
            );
            self.lint_override_stack.pop();
        }
    }

    pub(super) fn warn_implied_derives(&mut self) {
        // §9's chains: the derives each derive needs. 0.8.x's fill-in also
        // makes a `Hash` type comparable, so `Hash` is reported as needing
        // what it is given.
        fn needs(t: &str) -> &'static [&'static str] {
            match t {
                "Copy" => &["Clone"],
                "Eq" | "PartialOrd" => &["PartialEq"],
                "Ord" => &["PartialOrd", "Eq"],
                "Hash" => &["PartialEq", "Eq"],
                _ => &[],
            }
        }
        if self.compiling_stdlib {
            return;
        }
        let library = self.program.library_start.unwrap_or(usize::MAX);
        let mut found = Vec::new();
        for item in &self.program.items {
            let (name, attrs, stdlib_origin, lints) = match item {
                Item::StructDef(d) => (&d.name, &d.attributes, d.stdlib_origin, &d.lint_overrides),
                Item::EnumDef(d) => (&d.name, &d.attributes, d.stdlib_origin, &d.lint_overrides),
                // B-2026-11-18-22: a distinct type's derives follow the same
                // chains.
                Item::DistinctType(d) => (&d.name, &d.attributes, false, &d.lint_overrides),
                _ => continue,
            };
            if stdlib_origin {
                continue;
            }
            let derive_attrs: Vec<&Attribute> =
                attrs.iter().filter(|a| a.is_bare("derive")).collect();
            let mut written: Vec<&str> = Vec::new();
            for a in &derive_attrs {
                for arg in &a.args {
                    match arg.value.as_ref().map(|v| &v.kind) {
                        Some(ExprKind::Identifier(n)) => written.push(n),
                        Some(ExprKind::Call { callee, .. }) => {
                            if let ExprKind::Identifier(n) = &callee.kind {
                                written.push(n);
                            }
                        }
                        _ => {}
                    }
                }
            }
            // A trait the program implements by hand for this type is not
            // filled in, and adding it to the derive would be E0240's
            // conflicting impl (B-2026-11-18-69).
            let hand: Vec<&str> = self
                .program
                .items
                .iter()
                .filter_map(|it| match it {
                    Item::ImplBlock(imp) => match &imp.target_type.kind {
                        TypeKind::Path(path)
                            if path.segments.len() == 1 && &path.segments[0] == name =>
                        {
                            imp.trait_name
                                .as_ref()
                                .and_then(|p| p.segments.last())
                                .map(String::as_str)
                        }
                        _ => None,
                    },
                    _ => None,
                })
                .collect();
            // Close the written set under §9's chains; what the closure adds
            // is what the compiler is filling in.
            let mut needed: Vec<&str> = written.clone();
            let mut i = 0;
            while i < needed.len() {
                for w in needs(needed[i]) {
                    if !needed.contains(w) && !hand.contains(w) {
                        needed.push(w);
                    }
                }
                i += 1;
            }
            let missing: Vec<&str> = needed[written.len()..].to_vec();
            if missing.is_empty() {
                continue;
            }
            // The list to extend: the `#[derive(..)]` with the last argument.
            let Some((attr, last)) = derive_attrs
                .iter()
                .rev()
                .find_map(|a| a.args.last().map(|l| (*a, l)))
            else {
                continue;
            };
            if attr.span.offset >= library || last.span.length == 0 {
                continue;
            }
            let at = Span {
                offset: last.span.offset + last.span.length,
                length: 0,
                line: last.span.line,
                column: last.span.column + last.span.length,
            };
            let why: Vec<String> = written
                .iter()
                .filter_map(|w| {
                    let lacking: Vec<&str> = needs(w)
                        .iter()
                        .copied()
                        .filter(|r| !written.contains(r) && !hand.contains(r))
                        .collect();
                    (!lacking.is_empty())
                        .then(|| format!("`{w}` needs `{}`", lacking.join("` and `")))
                })
                .collect();
            // The note names what the derive implies, which 0.8.x adds
            // (review-thread ruling 2026-11-20: `derive(Hash)` implies
            // `PartialEq` and `Eq`, and warns rather than adding silently).
            let message = format!(
                "implied derive on `{name}` is deprecated: {}; note: the derive implies `{}`, \
                 which 0.8.x fills in (design.md §9 Derive). It becomes an error in the next \
                 minor release, and `karac fix` adds `{}` to the derive list",
                why.join("; "),
                missing.join("`, `"),
                missing.join(", "),
            );
            let fix = super::FixIt {
                span: at,
                replacement: missing.iter().map(|m| format!(", {m}")).collect(),
            };
            found.push((message, attr.span, fix, lints.clone()));
        }
        for (message, span, fix, lints) in found {
            self.lint_override_stack.push(lints);
            self.type_lint_warning_with_fix(
                message,
                span,
                TypeErrorKind::Deprecated,
                "deprecated",
                Some(fix),
            );
            self.lint_override_stack.pop();
        }
    }

    /// design.md §9 Derive: a derived `Clone`, `Debug` or `Display` goes
    /// through each field's own impl, so every field needs one. 0.8.x has
    /// cloned and rendered such a field by its shape, so a field without the
    /// trait is a `deprecated` warning here, as an implied derive is
    /// ([`Self::warn_implied_derives`]), and becomes an error in the next
    /// minor release. A `Secret[T]` field renders as `<redacted>`
    /// (library/secret.md), and a `Copy` field clones by copying.
    pub(super) fn warn_derived_field_traits(&mut self) {
        if self.compiling_stdlib {
            return;
        }
        let library = self.program.library_start.unwrap_or(usize::MAX);
        let mut found = Vec::new();
        for item in &self.program.items {
            let (name, span, stdlib_origin, lints, is_enum) = match item {
                Item::StructDef(d) => (&d.name, d.span, d.stdlib_origin, &d.lint_overrides, false),
                Item::EnumDef(d) => (&d.name, d.span, d.stdlib_origin, &d.lint_overrides, true),
                _ => continue,
            };
            if stdlib_origin || span.offset >= library {
                continue;
            }
            let (derived, fields): (Vec<String>, Vec<(String, Type)>) = if is_enum {
                let Some(info) = self.env.enums.get(name) else {
                    continue;
                };
                let mut fields = Vec::new();
                for (variant, v) in &info.variants {
                    match v {
                        VariantTypeInfo::Unit => {}
                        VariantTypeInfo::Tuple(tys) => {
                            fields.extend(tys.iter().enumerate().map(|(i, t)| {
                                (format!("variant '{variant}' field '{i}'"), t.clone())
                            }))
                        }
                        VariantTypeInfo::Struct(fs) => {
                            fields.extend(fs.iter().map(|(n, t)| {
                                (format!("variant '{variant}' field '{n}'"), t.clone())
                            }))
                        }
                    }
                }
                (info.derived_traits.iter().cloned().collect(), fields)
            } else {
                let Some(info) = self.env.structs.get(name) else {
                    continue;
                };
                (
                    info.derived_traits.iter().cloned().collect(),
                    info.fields
                        .iter()
                        .map(|(n, t, _)| (format!("field '{n}'"), t.clone()))
                        .collect(),
                )
            };
            for tr in ["Clone", "Debug", "Display"] {
                if !derived.iter().any(|d| d == tr) {
                    continue;
                }
                for (what, ty) in &fields {
                    let secret = matches!(ty, Type::Named { name, .. } if name == "Secret");
                    let ok = match tr {
                        "Clone" => self.type_supports_clone(ty) || self.is_type_copy(ty),
                        "Debug" => secret || self.type_supports_debug(ty),
                        _ => secret || self.type_supports_display(ty),
                    };
                    if ok {
                        continue;
                    }
                    let does = match tr {
                        "Clone" => "clones",
                        _ => "renders",
                    };
                    let kind = if is_enum { "enum" } else { "struct" };
                    found.push((
                        format!(
                            "{kind} '{name}' derives {tr} but {what} has non-{tr} type '{}': a \
                             derived `{tr}` goes through each field's own impl (design.md §9 \
                             Derive). 0.8.x still {does} the field by its shape; this is \
                             deprecated and becomes an error in the next minor release. \
                             Implement or derive `{tr}` for '{}'",
                            type_display(ty),
                            type_display(ty),
                        ),
                        span,
                        lints.clone(),
                    ));
                }
            }
        }
        for (message, span, lints) in found {
            self.lint_override_stack.push(lints);
            self.type_lint_warning_with_fix(
                message,
                span,
                TypeErrorKind::Deprecated,
                "deprecated",
                None,
            );
            self.lint_override_stack.pop();
        }
    }

    /// A bound's inline `Assoc = T` binding (`I: Iterator[Elem = i64]`) must
    /// name an associated type of the bound's trait (B-2026-11-06-27 item 6).
    /// The desugar pass hoisted each binding onto its item's `where` clause as
    /// an `AssocTypeEq` that remembers the trait, so this walks every `where`
    /// clause for those and applies the return-position check
    /// (`check_trait_declares_assoc_type`, E_UNKNOWN_ASSOC_TYPE_BINDING).
    pub(super) fn validate_bound_assoc_bindings(&mut self) {
        fn collect(wc: &Option<WhereClause>, out: &mut Vec<(String, String, Span)>) {
            let Some(wc) = wc else { return };
            for c in &wc.constraints {
                if let WhereConstraint::AssocTypeEq {
                    assoc_name,
                    span,
                    trait_name: Some(t),
                    ..
                } = c
                {
                    out.push((t.clone(), assoc_name.clone(), *span));
                }
            }
        }
        let mut found = Vec::new();
        for item in &self.program.items {
            match item {
                Item::Function(f) => collect(&f.where_clause, &mut found),
                Item::ImplBlock(imp) => {
                    collect(&imp.where_clause, &mut found);
                    for it in &imp.items {
                        if let ImplItem::Method(m) = it {
                            collect(&m.where_clause, &mut found);
                        }
                    }
                }
                Item::TraitDef(t) => {
                    collect(&t.where_clause, &mut found);
                    for it in &t.items {
                        if let TraitItem::Method(m) = it {
                            collect(&m.where_clause, &mut found);
                        }
                    }
                }
                Item::StructDef(d) => collect(&d.where_clause, &mut found),
                Item::EnumDef(d) => collect(&d.where_clause, &mut found),
                _ => {}
            }
        }
        for (t, assoc, span) in found {
            self.check_trait_declares_assoc_type(&t, &assoc, span);
        }
    }

    /// `#[serde(default: expr)]` takes a constant expression, the class a
    /// `const` initializer accepts (design.md §9; B-2026-11-06-27 item 11): a
    /// run-time expression such as `Vec.new()` is refused here, with the
    /// strict constant-initializer walk that judges `const` items.
    pub(super) fn validate_serde_default_constants(&mut self) {
        let library = self.program.library_start.unwrap_or(usize::MAX);
        let mut found: Vec<(String, Expr)> = Vec::new();
        for item in &self.program.items {
            let Item::StructDef(d) = item else { continue };
            if d.span.offset >= library {
                continue;
            }
            for field in &d.fields {
                for attr in &field.attributes {
                    if !attr.is_bare("serde") {
                        continue;
                    }
                    for arg in &attr.args {
                        if let (Some("default"), Some(value)) = (arg.name.as_deref(), &arg.value) {
                            found.push((field.name.clone(), value.clone()));
                        }
                    }
                }
            }
        }
        for (field, value) in found {
            self.const_init_strict = true;
            self.const_init_serde_default = true;
            self.check_module_binding_init(&value, &field);
            self.const_init_strict = false;
            self.const_init_serde_default = false;
        }
    }

    /// `#[derive(Default)]` validation. Unlike the structural derives
    /// (`Eq` / `Ord` / …), Default's *enum* rule is not a field-shape
    /// walk: the derived `default()` constructs exactly one
    /// `#[default]`-marked, field-less variant, so only that variant
    /// matters — the other variants' field types are irrelevant. The
    /// struct rule is the usual "every field must be Default".
    ///
    /// Diagnostics (phase-8 stdlib-floor — `#[derive(Default)]` /
    /// `#[default]` on enum variants):
    ///   * `E_DERIVE_DEFAULT_MISSING_FIELD_DEFAULT` — a struct field
    ///     whose type is not Default.
    ///   * `E_DEFAULT_NO_VARIANT_MARKED` — a derive-Default enum with
    ///     zero `#[default]` markers.
    ///   * `E_DEFAULT_MULTIPLE_VARIANTS` — two or more markers.
    ///   * `E_DEFAULT_VARIANT_HAS_PAYLOAD` — the single marked variant
    ///     carries fields (tuple or struct form).
    ///
    /// The placement diagnostics (`#[default]` on a non-variant, on a
    /// variant of a non-derive enum, or with arguments) are emitted
    /// earlier by `crate::attribute_validator::validate_default_attribute`.
    pub(super) fn validate_derive_default(&mut self) {
        // Structs: every field must implement Default.
        let structs: Vec<_> = self
            .env
            .structs
            .iter()
            .filter(|(_, info)| info.derived_traits.contains("Default"))
            .map(|(name, info)| (name.clone(), info.clone()))
            .collect();
        for (name, info) in structs {
            let struct_span = self.program.items.iter().find_map(|item| match item {
                Item::StructDef(s) if s.name == name => Some(s.span),
                _ => None,
            });
            let Some(struct_span) = struct_span else {
                continue;
            };
            for (field_name, field_ty, _) in &info.fields {
                if !self.derived_default_field_ok(field_ty) {
                    let field_shown = type_display(field_ty);
                    self.type_error_with_help(
                        format!(
                            "error[E_DERIVE_DEFAULT_MISSING_FIELD_DEFAULT]: cannot \
                             #[derive(Default)] for '{}' — field '{}' of type '{}' does not \
                             implement 'Default'",
                            name, field_name, field_shown
                        ),
                        struct_span,
                        TypeErrorKind::TypeMismatch,
                        format!(
                            "give '{field_shown}' a `Default` (`#[derive(Default)]` on it, or \
                             `impl Default for {field_shown}`), or drop the derive and write \
                             `impl Default for {name}` choosing the field's value"
                        ),
                    );
                }
            }
        }

        // Distinct types (B-2026-11-18-23): `Count.default()` is
        // `Count(<base>.default())`, so the base must be `Default`. A refined
        // distinct type is refused outright: nothing evaluates the `where`
        // predicate against the base's default, which may violate it.
        let distinct: Vec<_> = self
            .program
            .items
            .iter()
            .filter_map(|item| match item {
                Item::DistinctType(d)
                    if extract_derived_traits(&d.attributes).contains("Default") =>
                {
                    Some((
                        d.name.clone(),
                        d.span,
                        d.base_type.clone(),
                        distinct_scope(d),
                        d.refinement.is_some(),
                    ))
                }
                _ => None,
            })
            .collect();
        for (name, span, base, scope, refined) in distinct {
            let base_ty = self.lower_type_expr(&base, &scope);
            let message = if refined {
                format!(
                    "error[E_DERIVE_DEFAULT_REFINED]: cannot #[derive(Default)] for refined \
                     distinct type '{name}': the default of its base type '{}' may violate its \
                     `where` predicate; write `impl Default for {name}` with a value that \
                     satisfies it",
                    type_display(&base_ty)
                )
            } else if !self.derived_default_field_ok(&base_ty) {
                format!(
                    "error[E_DERIVE_DEFAULT_MISSING_FIELD_DEFAULT]: cannot #[derive(Default)] \
                     for distinct type '{name}' — its base type '{}' does not implement \
                     'Default'",
                    type_display(&base_ty)
                )
            } else {
                continue;
            };
            self.type_error(message, span, TypeErrorKind::TypeMismatch);
        }

        // Enums: exactly one `#[default]`-marked, field-less variant.
        // Variant attributes live on the AST, not the typed env, so read
        // the enum decls directly.
        let enums: Vec<EnumDef> = self
            .program
            .items
            .iter()
            .filter_map(|item| match item {
                Item::EnumDef(e) if extract_derived_traits(&e.attributes).contains("Default") => {
                    Some(e.clone())
                }
                _ => None,
            })
            .collect();
        for e in enums {
            let marked: Vec<&Variant> = e
                .variants
                .iter()
                .filter(|v| v.attributes.iter().any(|a| a.is_bare("default")))
                .collect();
            match marked.as_slice() {
                [] => {
                    self.type_error(
                        format!(
                            "error[E_DEFAULT_NO_VARIANT_MARKED]: #[derive(Default)] on enum \
                             '{}' requires exactly one variant to be marked with #[default]; \
                             add it to the variant that represents the starting state",
                            e.name
                        ),
                        e.span,
                        TypeErrorKind::TypeMismatch,
                    );
                }
                [only] => {
                    if !matches!(only.kind, VariantKind::Unit) {
                        self.type_error(
                            format!(
                                "error[E_DEFAULT_VARIANT_HAS_PAYLOAD]: #[default] on enum \
                                 variant '{}' requires a field-less variant; either declare \
                                 the marked variant as '{}' (no fields) or drop the derive \
                                 and write a manual 'impl Default for {} {{ fn default() -> {} \
                                 {{ ... }} }}' that constructs the desired starting state",
                                only.name, only.name, e.name, e.name
                            ),
                            only.span,
                            TypeErrorKind::TypeMismatch,
                        );
                    }
                }
                [first, second, ..] => {
                    self.type_error(
                        format!(
                            "error[E_DEFAULT_MULTIPLE_VARIANTS]: #[derive(Default)] on enum \
                             '{}' requires exactly one variant marked with #[default]; found \
                             markers on '{}' and '{}'",
                            e.name, first.name, second.name
                        ),
                        e.span,
                        TypeErrorKind::TypeMismatch,
                    );
                }
            }
        }
    }

    /// A field a `#[derive(Default)]` can build: a `Default` type, or one of
    /// the struct's own type parameters, which the derived impl bounds by
    /// `Default` (`impl[T: Default] Pair[T]`, design.md §9; B-2026-11-06-25).
    fn derived_default_field_ok(&self, ty: &Type) -> bool {
        match ty {
            Type::TypeParam(_) => true,
            Type::Tuple(parts) => parts.iter().all(|t| self.derived_default_field_ok(t)),
            _ => self.type_supports_default(ty),
        }
    }

    /// See [`super::env::TypeEnv::type_supports_default`].
    pub(super) fn type_supports_default(&self, ty: &Type) -> bool {
        self.env.type_supports_default(ty)
    }

    /// Compound-payload enum codegen (Slice CP, CP5 carve-out) —
    /// reject enum variants whose payload field type is itself a
    /// (non-shared) user enum. v1 ships single-level enum-payload
    /// nesting only; the layout pass cannot size a recursively-nested
    /// payload area without an infinite-recursion guard, and the
    /// canonical workaround is to wrap the inner enum in `Vec`,
    /// `shared` (RC pointer), or a `Box`-style indirection. Recursion
    /// through `Vec[T]`, `Slice[T]`, tuples, or `shared` enums is
    /// fine — those layers stop the size recursion at one indirection.
    pub(super) fn validate_enum_payload_no_nested_enum(&mut self) {
        // v2 core: an enum may carry another enum as a payload
        // (`Step.Do(Command)`). One level of nesting was a limit of the
        // legacy backend, which `karac check` no longer applies, and nor
        // does the MIR pipeline, which shares v2's layout (B-2026-10-06-86).
        if self.cli_lint_overrides.strict_core || self.library_methods_from_source {
            return;
        }
        // Collect enum names for the carve-out check. `shared` enums
        // are heap-allocated via RC, so a payload field of type
        // `SharedFoo` is a single pointer word and is allowed.
        let value_enum_names: std::collections::HashSet<String> = self
            .program
            .items
            .iter()
            .filter_map(|item| match item {
                // `par` counts as a pointer form exactly like `shared`.
                // design.md § Type Kinds: `shared` and `par` are the two
                // reference-semantics tiers — single-task RC and cross-task
                // respectively — so a `sync enum` payload is the same single
                // pointer word a `shared enum` payload is. Omitting it made the
                // two rules contradict each other: this check told you to mark
                // the inner enum `shared`, while E_NOT_CROSS_TASK told you
                // `shared` cannot cross a task boundary and to use `par`. A
                // program needing both could not be written.
                Item::EnumDef(e) if !e.is_shared && !e.is_par => Some(e.name.clone()),
                _ => None,
            })
            .collect();
        // Share the set with the instantiation-side complement
        // (B-2026-09-10-13) and build its watch list now, while the same
        // definition of "value enum" is in hand.
        self.nested_enum_value_names = value_enum_names.iter().cloned().collect();
        self.build_nested_enum_param_payload_watch();

        // Walk every enum variant and inspect its payload field types.
        // The payload field's `TypeExpr` -> head segment is the
        // user-visible type name; if that name is a value enum, emit
        // the diagnostic. We only flag the *direct* head; recursion
        // through `Vec[Inner]` etc. is intentionally allowed
        // (CP5 carve-out is about size-recursion, not name presence).
        let items: &[Item] = &self.program.items;
        for item in items {
            if let Item::EnumDef(e) = item {
                // Pointer-form enums carry their payload behind an indirection,
                // so nesting depth in the VALUE layout is not their problem —
                // true of `par` for the same reason it is true of `shared`.
                if e.is_shared || e.is_par {
                    continue;
                }
                for variant in &e.variants {
                    let field_tys: Vec<&TypeExpr> = match &variant.kind {
                        VariantKind::Unit => Vec::new(),
                        VariantKind::Tuple(tys) => tys.iter().collect(),
                        VariantKind::Struct(fields) => fields.iter().map(|f| &f.ty).collect(),
                    };
                    // Dedup by the nested-enum head name within a variant. A
                    // variant with two payload fields of the same enum type
                    // (`Add(Expr, Expr)`) is one offending relationship, not
                    // two — the diagnostic names only `(variant, head)`, and
                    // its span is `variant.span` for every field, so firing
                    // per-field emits byte-identical duplicate diagnostics.
                    // Distinct nested-enum types in one variant (`Add(Expr,
                    // Stmt)`) still each report once.
                    let mut flagged_heads: std::collections::HashSet<&str> =
                        std::collections::HashSet::new();
                    for ty in field_tys {
                        if let TypeKind::Path(path) = &ty.kind {
                            if let Some(head) = path.segments.first() {
                                if value_enum_names.contains(head)
                                    && flagged_heads.insert(head.as_str())
                                {
                                    self.type_error(
                                        format!(
                                            "error[E_ENUM_NESTED_ENUM_PAYLOAD]: enum variant \
                                             '{}.{}' has a payload of nested enum type '{}' — \
                                             0.8 only supports up to one level of enum nesting; \
                                             either flatten the variant, mark the inner enum as \
                                             `shared` (RC pointer) or `par` (cross-task pointer — \
                                             use this one if the value must cross a task \
                                             boundary), or wrap it in a `Vec` / collection layer",
                                            e.name, variant.name, head
                                        ),
                                        variant.span,
                                        TypeErrorKind::TypeMismatch,
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    /// Build [`Self::nested_enum_param_payloads`] — the watch list behind the
    /// instantiation-side half of the nested-enum-payload rule
    /// (B-2026-09-10-13).
    ///
    /// A payload declared as one of the enum's OWN generic parameters is the
    /// one shape `validate_enum_payload_no_nested_enum` structurally cannot
    /// judge: it reads the declared head, which is `T`, and `T` is never an
    /// enum name. Whether that payload nests an enum is decided by the type
    /// ARGUMENT — `G[R2]` does not, `G[Mono]` and `G[G[R2]]` do — so it can only
    /// be answered once inference has solved the instantiation.
    ///
    /// The two halves partition the variants exactly, the same way the
    /// name-keyed and instantiation-keyed payload WALKERS do one layer down in
    /// codegen (B-2026-09-10-2): a concretely-declared payload is judged at the
    /// declaration and never here, a param-declared payload only here. So the
    /// pair can neither double-report one variant nor leave one unjudged.
    fn build_nested_enum_param_payload_watch(&mut self) {
        let mut watch: FxHashMap<String, Vec<(String, usize)>> = FxHashMap::default();
        for (ename, info) in &self.env.enums {
            if info.is_shared || info.is_par || info.generic_params.is_empty() {
                continue;
            }
            if !self.nested_enum_value_names.contains(ename) {
                continue;
            }
            let mut rows: Vec<(String, usize)> = Vec::new();
            for (vname, vinfo) in &info.variants {
                let field_tys: Vec<&Type> = match vinfo {
                    VariantTypeInfo::Unit => Vec::new(),
                    VariantTypeInfo::Tuple(ts) => ts.iter().collect(),
                    VariantTypeInfo::Struct(fs) => fs.iter().map(|(_, t)| t).collect(),
                };
                for fty in field_tys {
                    // Only a payload that IS the parameter. A payload of
                    // `Vec[T]` / `Slice[T]` / a tuple stops the size recursion at
                    // one indirection and is allowed however `T` is chosen —
                    // the same carve-out the declared pass states for recursion
                    // through a collection layer.
                    let Type::TypeParam(pname) = fty else {
                        continue;
                    };
                    let Some(idx) = info.generic_params.iter().position(|g| g == pname) else {
                        continue;
                    };
                    if !rows.iter().any(|(v, i)| v == vname && *i == idx) {
                        rows.push((vname.clone(), idx));
                    }
                }
            }
            if !rows.is_empty() {
                watch.insert(ename.clone(), rows);
            }
        }
        self.nested_enum_param_payloads = watch;
    }

    /// Record an instantiation whose type ARGUMENT turns a param-declared
    /// payload into a nested enum payload (B-2026-09-10-13). Called from
    /// `record_expr_type` for every `Type::Named` carrying arguments.
    ///
    /// Keeps the EARLIEST span per offending `(enum, variant, nested head)`, so
    /// the diagnostic lands on the construction that introduces the type rather
    /// than on an arbitrary later use of it — every use of the binding is typed
    /// with the same offending type and would otherwise report again.
    pub(super) fn note_nested_enum_instantiation(
        &mut self,
        name: &str,
        args: &[Type],
        span: &Span,
    ) {
        let Some(watch) = self.nested_enum_param_payloads.get(name) else {
            return;
        };
        // One entry per param-declared variant — a handful at most.
        let watch = watch.clone();
        for (vname, idx) in watch {
            let Some(arg) = args.get(idx) else {
                continue;
            };
            let Type::Named { name: head, .. } = arg else {
                continue;
            };
            if !self.nested_enum_value_names.contains(head) {
                continue;
            }
            let key = (name.to_string(), vname, head.clone());
            match self.nested_enum_inst_offenders.get(&key) {
                Some((prev, _)) if prev.offset <= span.offset => {}
                _ => {
                    self.nested_enum_inst_offenders
                        .insert(key, (*span, type_display(arg)));
                }
            }
        }
    }

    /// Emit one `E_ENUM_NESTED_ENUM_PAYLOAD` per offending instantiation
    /// gathered by [`Self::note_nested_enum_instantiation`].
    ///
    /// Deliberately the SAME error code as the declared spelling: it is one
    /// language rule ("v1 supports one level of enum nesting"), and a program
    /// that trips it through a generic argument needs the same four remedies.
    /// The message names the instantiation because that, not the declaration, is
    /// what the author has to change.
    ///
    /// Sorted by span before emission — the offenders live in a hash map, and an
    /// unsorted drain would order diagnostics differently between runs.
    pub(super) fn emit_nested_enum_inst_errors(&mut self) {
        // See `validate_enum_payload_no_nested_enum`.
        if self.cli_lint_overrides.strict_core || self.library_methods_from_source {
            self.nested_enum_inst_offenders.clear();
            return;
        }
        if self.nested_enum_inst_offenders.is_empty() {
            return;
        }
        let mut rows: Vec<(NestedEnumInstKey, NestedEnumInstSite)> =
            std::mem::take(&mut self.nested_enum_inst_offenders)
                .into_iter()
                .collect();
        rows.sort_by(|(ka, (sa, _)), (kb, (sb, _))| {
            sa.offset.cmp(&sb.offset).then_with(|| ka.cmp(kb))
        });
        for ((ename, vname, head), (span, arg_display)) in rows {
            self.type_error(
                format!(
                    "error[E_ENUM_NESTED_ENUM_PAYLOAD]: instantiating '{}[{}]' gives enum \
                     variant '{}.{}' a payload of nested enum type '{}' — 0.8 only supports up \
                     to one level of enum nesting; either use a different type argument, mark \
                     '{}' as `shared` (RC pointer) or `par` (cross-task pointer — use this one \
                     if the value must cross a task boundary), or wrap it in a `Vec` / \
                     collection layer",
                    ename, arg_display, ename, vname, head, head
                ),
                span,
                TypeErrorKind::TypeMismatch,
            );
        }
    }

    /// `#[derive(Display)]` on enums.
    ///
    /// Previously restricted to all-unit-variant enums. As of the payload-enum
    /// Display slice (phase-8 `main()` entry-point work, Slice A) both backends
    /// render payload variants — the interpreter via `Value::EnumVariant`
    /// Display and codegen via `emit_enum_display_fn` (value-driven, read-only
    /// payload rendering), both matching the same `Variant` / `Variant(f0, f1)`
    /// / `Variant { name: v }` format. So `#[derive(Display)]` is now accepted
    /// on tuple/struct-variant enums too. The function is retained as the
    /// hook point for any future per-variant derive validation (and so the
    /// call site stays stable); it currently has nothing to reject.
    pub(super) fn validate_derive_display_on_enums(&mut self) {}

    /// design.md §15 `mem.zeroed`: an enum is not `Pod`, since the
    /// all-zeros bits need not be one of its variants (FF4).
    fn validate_derive_pod_on_enums(&mut self) {
        let found: Vec<(String, Span)> = self
            .program
            .items
            .iter()
            .filter_map(|item| match item {
                Item::EnumDef(d) if extract_derived_traits(&d.attributes).contains("Pod") => {
                    Some((d.name.clone(), d.span))
                }
                _ => None,
            })
            .collect();
        for (name, span) in found {
            self.type_error(
                format!(
                    "enum '{name}' cannot derive Pod: `Pod` holds when the all-zeros bits are a \
                     valid value, which design.md §15 grants the integers, floats, `bool`, and \
                     tuples, arrays and structs of `Pod`, not an enum; remove `Pod` from the derive"
                ),
                span,
                TypeErrorKind::TypeMismatch,
            );
        }
    }

    /// Validate `#[derive(Arithmetic)]` usage:
    /// - Reject on struct/enum (must use manual impls).
    /// - Reject on distinct types whose base type is non-numeric.
    pub(super) fn validate_derive_arithmetic(&mut self) {
        let items: &[Item] = &self.program.items;
        for item in items {
            match item {
                Item::StructDef(s)
                    if extract_derived_traits(&s.attributes).contains("Arithmetic") =>
                {
                    self.type_error(
                        format!(
                            "#[derive(Arithmetic)] is only valid on `distinct type`, not on \
                             struct '{}'; use manual trait impls for structs",
                            s.name
                        ),
                        s.span,
                        TypeErrorKind::TypeMismatch,
                    );
                }
                Item::EnumDef(e)
                    if extract_derived_traits(&e.attributes).contains("Arithmetic") =>
                {
                    self.type_error(
                        format!(
                            "#[derive(Arithmetic)] is only valid on `distinct type`, not on \
                             enum '{}'; use manual trait impls for enums",
                            e.name
                        ),
                        e.span,
                        TypeErrorKind::TypeMismatch,
                    );
                }
                Item::DistinctType(d)
                    if extract_derived_traits(&d.attributes).contains("Arithmetic") =>
                {
                    let base_ty = self.lower_type_expr(&d.base_type, &[]);
                    if !is_numeric(&base_ty) {
                        self.type_error(
                            format!(
                                "distinct type '{}' derives Arithmetic but its base type \
                                 '{}' is not numeric",
                                d.name,
                                type_display(&base_ty)
                            ),
                            d.span,
                            TypeErrorKind::TypeMismatch,
                        );
                    }
                }
                _ => {}
            }
        }
    }

    /// Walk every struct/enum that derives `trait_name`; emit a diagnostic for
    /// each field whose type fails `supports`. Skips types that aren't in the
    /// program AST — those are compiler-provided built-ins (`F32`, `F64`,
    /// `Ordering`, `MemoryOrdering`) whose derived-trait bundles are
    /// hand-verified.
    /// `impl Pod for S { }` written by the program is held to the rule
    /// `#[derive(Pod)]` is: zeroing `S` is sound only when every field's zero
    /// bits are a valid value (design.md § `mem.zeroed()`, B-2026-11-20-117).
    fn validate_explicit_pod_impls(&mut self) {
        let user_end = self.program.library_start.unwrap_or(usize::MAX);
        let impls: Vec<(String, crate::token::Span)> = self
            .program
            .items
            .iter()
            .filter_map(|item| match item {
                Item::ImplBlock(b)
                    if b.span.offset < user_end
                        && b.trait_name.as_ref().is_some_and(|t| {
                            t.segments.last().map(String::as_str) == Some("Pod")
                        }) =>
                {
                    match &b.target_type.kind {
                        crate::ast::TypeKind::Path(p) if p.segments.len() == 1 => {
                            Some((p.segments[0].clone(), b.span))
                        }
                        _ => None,
                    }
                }
                _ => None,
            })
            .collect();
        for (name, span) in impls {
            let fields: Vec<(String, Type)> = if let Some(info) = self.env.structs.get(&name) {
                info.fields
                    .iter()
                    .map(|(n, t, _)| (n.clone(), t.clone()))
                    .collect()
            } else if self.env.enums.contains_key(&name) {
                // The rule `#[derive(Pod)]` on an enum is refused by.
                self.type_error(
                    format!(
                        "`impl Pod for {name}`: an enum is not `Pod`; `Pod` holds when the \
                         all-zeros bits are a valid value, which design.md §15 grants the \
                         integers, floats, `bool`, and tuples, arrays and structs of `Pod`"
                    ),
                    span,
                    TypeErrorKind::TypeMismatch,
                );
                continue;
            } else {
                continue;
            };
            for (field, ty) in fields {
                if !self.env.type_supports_pod(&ty) {
                    self.type_error(
                        format!(
                            "`impl Pod for {name}`: field '{field}' has non-Pod type '{}', \
                             whose all-zeros bits are not a valid value (design.md § \
                             `mem.zeroed()`)",
                            type_display(&ty)
                        ),
                        span,
                        TypeErrorKind::TypeMismatch,
                    );
                }
            }
        }
    }

    pub(super) fn validate_derived_trait(
        &mut self,
        trait_name: &str,
        supports: impl Fn(&Self, &Type) -> bool,
    ) {
        let structs: Vec<_> = self
            .env
            .structs
            .iter()
            .filter(|(_, info)| info.derived_traits.contains(trait_name))
            .map(|(name, info)| (name.clone(), info.clone()))
            .collect();

        for (name, info) in structs {
            let struct_span = self.program.items.iter().find_map(|item| {
                if let Item::StructDef(s) = item {
                    if s.name == name {
                        return Some(s.span);
                    }
                }
                None
            });
            let Some(struct_span) = struct_span else {
                continue;
            };
            for (field_name, field_ty, _) in &info.fields {
                if !supports(self, field_ty) {
                    let span = struct_span;
                    self.type_error(
                        format!(
                            "struct '{}' derives {} but field '{}' has non-{} type '{}'",
                            name,
                            trait_name,
                            field_name,
                            trait_name,
                            type_display(field_ty)
                        ),
                        span,
                        TypeErrorKind::TypeMismatch,
                    );
                }
            }
        }

        let enums: Vec<_> = self
            .env
            .enums
            .iter()
            .filter(|(_, info)| info.derived_traits.contains(trait_name))
            .map(|(name, info)| (name.clone(), info.clone()))
            .collect();

        for (name, info) in enums {
            let enum_span = self.program.items.iter().find_map(|item| {
                if let Item::EnumDef(e) = item {
                    if e.name == name {
                        return Some(e.span);
                    }
                }
                None
            });
            let Some(enum_span) = enum_span else {
                continue;
            };
            for (variant_name, variant_info) in &info.variants {
                let bad_fields: Vec<(String, Type)> = match variant_info {
                    VariantTypeInfo::Unit => Vec::new(),
                    VariantTypeInfo::Tuple(types) => types
                        .iter()
                        .enumerate()
                        .filter(|(_, t)| !supports(self, t))
                        .map(|(i, t)| (i.to_string(), t.clone()))
                        .collect(),
                    VariantTypeInfo::Struct(fields) => fields
                        .iter()
                        .filter(|(_, t)| !supports(self, t))
                        .map(|(n, t)| (n.clone(), t.clone()))
                        .collect(),
                };
                for (field_ref, field_ty) in bad_fields {
                    self.type_error(
                        format!(
                            "enum '{}' derives {} but variant '{}' field '{}' has non-{} type '{}'",
                            name,
                            trait_name,
                            variant_name,
                            field_ref,
                            trait_name,
                            type_display(&field_ty)
                        ),
                        enum_span,
                        TypeErrorKind::TypeMismatch,
                    );
                }
            }
        }

        // A distinct type's one "field" is its base (B-2026-11-18-22):
        // `#[derive(Eq)] distinct type M = f64` is an `Eq` whose `M(NaN)` is
        // not equal to itself. `Copy` has its own distinct-type check in
        // `validate_derive_copy`.
        if trait_name == "Copy" {
            return;
        }
        let distinct: Vec<_> = self
            .program
            .items
            .iter()
            .filter_map(|item| match item {
                Item::DistinctType(d)
                    if extract_derived_traits(&d.attributes).contains(trait_name) =>
                {
                    Some((
                        d.name.clone(),
                        d.span,
                        d.base_type.clone(),
                        distinct_scope(d),
                    ))
                }
                _ => None,
            })
            .collect();
        for (name, span, base, scope) in distinct {
            let base_ty = self.lower_type_expr(&base, &scope);
            if !supports(self, &base_ty) {
                self.type_error(
                    format!(
                        "distinct type '{name}' derives {trait_name} but its base type '{}' is \
                         not {trait_name}",
                        type_display(&base_ty)
                    ),
                    span,
                    TypeErrorKind::TypeMismatch,
                );
            }
        }
    }

    /// `E_DERIVE_CLONE_ALLOCATES` (phase-8-stdlib-floor item 5). Under
    /// `panic_on_alloc_failure = false`, a `#[derive(Clone)]` type whose
    /// synthesized `clone` reaches a panicking allocator (a heap-collection /
    /// `String` / `Box` field, transitively) is rejected — the derived clone has
    /// no fallible form. The author writes a manual `try_clone` instead, or opts
    /// into the panic with `#[allow(derive_clone_allocates)]` on the type.
    /// Pure-`Copy` types and types whose fields all clone infallibly
    /// (primitives, refs, `Rc`/`Arc`, nested all-infallible types) are
    /// unaffected. No-op in the default mode.
    pub(super) fn validate_derive_clone_allocates(&mut self) {
        if self.profile_config.panics_on_alloc_failure() {
            return;
        }
        let items: &[Item] = &self.program.items;
        for item in items {
            let (name, attrs, overrides, span) = match item {
                Item::StructDef(s) => (&s.name, &s.attributes, &s.lint_overrides, &s.span),
                Item::EnumDef(e) => (&e.name, &e.attributes, &e.lint_overrides, &e.span),
                _ => continue,
            };
            if !extract_derived_traits(attrs).contains("Clone") {
                continue;
            }
            if allows_derive_clone_allocates(overrides) {
                continue;
            }
            let Some((descriptor, field_ty)) = self.first_clone_allocating_field(name) else {
                continue;
            };
            self.type_error(
                format!(
                    "`#[derive(Clone)]` on type '{name}' generates a 'clone' that may panic on \
                     allocation failure ({descriptor} of type '{}' allocates); write a manual \
                     'try_clone' method instead, or suppress with \
                     '#[allow(derive_clone_allocates)]' if you accept the panic",
                    type_display(&field_ty)
                ),
                *span,
                TypeErrorKind::DeriveCloneAllocates,
            );
        }
    }

    /// First field / variant payload of the named type whose clone allocates,
    /// as a `(descriptor, type)` pair (`descriptor` is e.g. `"field 'name'"` or
    /// `"variant 'V' payload"`). `None` when the type clones infallibly.
    fn first_clone_allocating_field(&self, name: &str) -> Option<(String, Type)> {
        if let Some(info) = self.env.structs.get(name) {
            for (fname, fty, _) in &info.fields {
                if self.clone_allocates(fty, &mut std::collections::HashSet::new()) {
                    return Some((format!("field '{fname}'"), fty.clone()));
                }
            }
        } else if let Some(info) = self.env.enums.get(name) {
            for (vname, vti) in &info.variants {
                let tys: Vec<&Type> = match vti {
                    VariantTypeInfo::Unit => Vec::new(),
                    VariantTypeInfo::Tuple(ts) => ts.iter().collect(),
                    VariantTypeInfo::Struct(fs) => fs.iter().map(|(_, t)| t).collect(),
                };
                for t in tys {
                    if self.clone_allocates(t, &mut std::collections::HashSet::new()) {
                        return Some((format!("variant '{vname}' payload"), t.clone()));
                    }
                }
            }
        }
        None
    }

    /// Whether cloning a value of `ty` performs a heap allocation that may
    /// panic on OOM. `true` for owned heap collections (`Vec` / `String` /
    /// `Map` / `Set` / `VecDeque` / `SortedSet` / `Box`), transitively for
    /// user structs/enums with such a field, and for `Option`/`Result`/tuples/
    /// arrays wrapping one. `false` for primitives, refs, and `Rc`/`Arc`/`Weak`
    /// (clone is a refcount bump) and `shared` types (RC). `visiting` guards
    /// against recursive type definitions.
    fn clone_allocates(&self, ty: &Type, visiting: &mut std::collections::HashSet<String>) -> bool {
        match ty {
            Type::Str => true,
            Type::Named { name, args } => match name.as_str() {
                "Vec" | "VecDeque" | "Map" | "SortedMap" | "Set" | "SortedSet" | "TreeMap"
                | "TreeSet" | "Box" | "String" => true,
                "Rc" | "Arc" | "Weak" => false,
                "Option" | "Result" => args.iter().any(|a| self.clone_allocates(a, visiting)),
                other => {
                    if !visiting.insert(other.to_string()) {
                        return false; // cycle — already being inspected
                    }
                    let result = if let Some(info) = self.env.structs.get(other) {
                        info.fields
                            .iter()
                            .any(|(_, fty, _)| self.clone_allocates(fty, visiting))
                    } else if let Some(info) = self.env.enums.get(other) {
                        info.variants.iter().any(|(_, vti)| match vti {
                            VariantTypeInfo::Unit => false,
                            VariantTypeInfo::Tuple(ts) => {
                                ts.iter().any(|t| self.clone_allocates(t, visiting))
                            }
                            VariantTypeInfo::Struct(fs) => {
                                fs.iter().any(|(_, t)| self.clone_allocates(t, visiting))
                            }
                        })
                    } else {
                        false // unknown nominal — conservative (don't flag)
                    };
                    visiting.remove(other);
                    result
                }
            },
            Type::Array { element, .. } => self.clone_allocates(element, visiting),
            Type::Tuple(elems) => elems.iter().any(|e| self.clone_allocates(e, visiting)),
            // `Rc`/`Arc`/`Weak` clone is a refcount bump; `shared` types are RC.
            Type::Rc(_) | Type::Arc(_) | Type::Weak(_) | Type::Shared(_) => false,
            _ => false,
        }
    }
}

/// The derive-only builtin traits answered from the type environment alone,
/// so the generic-impl bound gate (`TypeEnv::bound_satisfied`, which method
/// resolution runs) and the free-fn bound gate (`type_satisfies_bound`) give
/// the same answer for the same type and bound (B-2026-10-05-43). Each
/// `TypeChecker` method of the same name delegates here.
impl super::env::TypeEnv {
    /// design.md §15 `mem.zeroed()`'s bound: the all-zeros bits are a valid
    /// value of `ty`. The integers, floats, `bool` and `()`, and tuples,
    /// arrays and `#[derive(Pod)]` structs of them (FF4). A reference, a
    /// raw or function pointer, a `char`, an enum or a library type is not.
    pub(super) fn type_supports_pod(&self, ty: &Type) -> bool {
        match ty {
            Type::Int(_) | Type::UInt(_) | Type::Float(_) | Type::Bool | Type::Unit => true,
            Type::Tuple(parts) => parts.iter().all(|t| self.type_supports_pod(t)),
            Type::Array { element, .. } => self.type_supports_pod(element),
            Type::Named { name, args } => {
                self.structs
                    .get(name)
                    .is_some_and(|s| s.derived_traits.contains("Pod"))
                    || self.has_impl("Pod", name, args)
            }
            Type::TypeVar(_) | Type::AssocProjection { .. } | Type::Error => true,
            _ => false,
        }
    }

    /// If `ty` is a `distinct type`, return whether it derives ANY of
    /// `wanted` (so the caller's trait gate is satisfied), wrapped in
    /// `Some`; `None` when `ty` is not a distinct type (caller continues its
    /// normal struct/enum/primitive logic). Distinct types are opaque — they
    /// inherit NO operations from their base, so a derive-support query must
    /// consult the explicit `#[derive(...)]` set, not the base's support
    /// (design.md § Distinct Types — "No operations carry through by
    /// default"). This is the gate that makes `a == b` / `a < b` / hashing /
    /// `Display` require the corresponding derive on a distinct type.
    fn distinct_derive_supported(&self, ty: &Type, wanted: &[&str]) -> Option<bool> {
        if let Type::Named { name, args } = ty {
            if let Some(traits) = self.distinct_types.get(name) {
                // A hand-written `impl Eq for` the distinct type is its own,
                // as a derive is: design.md §9's newtype escape hatch
                // (B-2026-11-18-20, B-2026-11-18-38).
                return Some(
                    wanted
                        .iter()
                        .any(|w| traits.contains(*w) || self.has_impl(w, name, args)),
                );
            }
        }
        None
    }

    /// Check whether a type supports `==` / `!=` (PartialEq).
    /// All primitives including floats support PartialEq.
    /// Named types (structs/enums) require `#[derive(Eq)]` or `#[derive(PartialEq)]`.
    pub(super) fn type_supports_partial_eq(&self, ty: &Type) -> bool {
        if let Some(ok) = self.distinct_derive_supported(ty, &["Eq", "PartialEq"]) {
            return ok;
        }
        match ty {
            // Refinement types are structurally transparent — derive
            // support follows the base type.
            Type::Refinement { base, .. } => self.type_supports_partial_eq(base),
            Type::Int(_)
            | Type::UInt(_)
            | Type::Float(_)
            | Type::Bool
            | Type::Char
            | Type::Str
            | Type::Unit => true,
            Type::Tuple(elems) => elems.iter().all(|e| self.type_supports_partial_eq(e)),
            Type::Array { element, .. } => self.type_supports_partial_eq(element),
            Type::Vector { element, .. } => self.type_supports_partial_eq(element),
            Type::Slice { element, .. } => self.type_supports_partial_eq(element),
            Type::Ref(inner) | Type::MutRef(inner) => self.type_supports_partial_eq(inner),
            // `Vec[T]` has value (content) equality when `T` does — the exact
            // arm `type_supports_eq` carries, for the exact same reason: the
            // built-in `Vec` is registered in `env.structs` with NO derived
            // traits, so without this it falls through to the generic `Named`
            // lookup below and reports `Vec` as un-`PartialEq`.
            //
            // B-2026-08-18-6: `Eq` and `Hash` both grew this arm and
            // `PartialEq` did not, which made the WEAKER trait the stricter
            // gate — `#[derive(Eq)] struct A { v: Vec[i64] }` compiled while
            // `#[derive(PartialEq)]` on the same field was refused. Nothing
            // about the lowering justified the split: `==` on a Vec-carrying
            // struct routes through `emit_eq_fn_for_struct` (B-2026-08-12-5),
            // the type-directed comparator, whichever of the two traits was
            // written.
            Type::Named { name, args } if (name == "Vec" || name == "VecDeque") && args.len() == 1 => {
                self.type_supports_partial_eq(&args[0])
            }
            // A map compares its keys' values, a set its elements, order
            // playing no part (docs/library/collections.md § Common traits,
            // B-2026-11-08-82). The keys and elements are `Eq` by the
            // collections' own bounds. A `Map` or `Set` may carry its
            // hasher as a last argument (`Map.new()`'s `Map[K, V, H]`),
            // which plays no part either (B-2026-11-19-59).
            Type::Named { name, args }
                if (name == "Map" && (args.len() == 2 || args.len() == 3))
                    || (name == "SortedMap" && args.len() == 2) =>
            {
                self.type_supports_partial_eq(&args[1])
            }
            Type::Named { name, args }
                if (name == "Set" && (args.len() == 1 || args.len() == 2))
                    || (name == "SortedSet" && args.len() == 1) =>
            {
                true
            }
            Type::Named { name, args } => {
                // A hand-written `impl PartialEq` (or `Eq`) is sufficient:
                // `==` calls `PartialEq.eq` (design.md § operator traits),
                // on every path (B-2026-11-20-66). Falls back to
                // `#[derive(Eq)]`/`#[derive(PartialEq)]` when no impl is
                // registered (e.g. for compiler-provided structural eq on
                // built-in enums like `Option`/`Result`).
                if self.has_impl("Eq", name, args) || self.has_impl("PartialEq", name, args) {
                    return true;
                }
                // A derive on a generic type holds for the arguments that have
                // the trait: `#[derive(PartialEq)] struct Pair[T]` is
                // `impl[T: PartialEq] PartialEq for Pair[T]` (design.md §
                // "Derive adds the bounds"), so it is not `PartialEq` at
                // `Pair[NoEq]` (B-2026-11-13-45, as B-2026-10-27-25 for `Eq`).
                let derived = |d: &std::collections::HashSet<String>| {
                    (d.contains("Eq") || d.contains("PartialEq"))
                        && args.iter().all(|a| self.type_supports_partial_eq(a))
                };
                if let Some(info) = self.structs.get(name) {
                    derived(&info.derived_traits)
                } else if let Some(info) = self.enums.get(name) {
                    derived(&info.derived_traits)
                } else {
                    true
                }
            }
            Type::Rc(inner) | Type::Arc(inner) => self.type_supports_partial_eq(inner),
            Type::Shared(name) => {
                if self.has_impl("Eq", name, &[]) || self.has_impl("PartialEq", name, &[]) {
                    return true;
                }
                if let Some(info) = self.structs.get(name) {
                    info.derived_traits.contains("Eq") || info.derived_traits.contains("PartialEq")
                } else if let Some(info) = self.enums.get(name) {
                    info.derived_traits.contains("Eq") || info.derived_traits.contains("PartialEq")
                } else {
                    true
                }
            }
            Type::TypeParam(_) | Type::TypeVar(_) | Type::AssocProjection { .. } | Type::Error => {
                true
            }
            Type::Never => true,
            Type::Function { .. }
            | Type::OnceFunction { .. }
            | Type::Pointer { .. }
            | Type::Weak(_)
            // `impl Trait` existentials only carry the trait surface,
            // not the witness's derive metadata; the derive-matches-bound
            // path is handled directly in `type_satisfies_bound` by the
            // existential-trait-name comparison, not via these helpers.
            | Type::Existential { .. }
            | Type::Dyn { .. } => false,
            // Shape-kinded args are not value types — no derive surface.
            Type::Shape(_) => false,
        }
    }

    /// Check whether a type supports full `Eq` (required for Map/Set keys, etc.).
    /// Floats (f32/f64) do NOT support Eq due to IEEE 754 NaN != NaN.
    /// Named types require `#[derive(Eq)]`.
    pub(super) fn type_supports_eq(&self, ty: &Type) -> bool {
        if let Some(ok) = self.distinct_derive_supported(ty, &["Eq"]) {
            return ok;
        }
        match ty {
            Type::Refinement { base, .. } => self.type_supports_eq(base),
            Type::Int(_) | Type::UInt(_) | Type::Bool | Type::Char | Type::Str | Type::Unit => true,
            // f32/f64 follow IEEE 754: NaN != NaN, so they don't implement Eq
            Type::Float(_) => false,
            Type::Tuple(elems) => elems.iter().all(|e| self.type_supports_eq(e)),
            Type::Array { element, .. } => self.type_supports_eq(element),
            Type::Vector { element, .. } => self.type_supports_eq(element),
            Type::Slice { element, .. } => self.type_supports_eq(element),
            Type::Ref(inner) | Type::MutRef(inner) => self.type_supports_eq(inner),
            // `Vec[T]` has value (content) equality when `T` does — element-wise
            // compare, like the `Array`/`Slice` arms above. The built-in `Vec`
            // is registered in `env.structs` with no derived traits, so without
            // this arm it falls through to the generic `Named` lookup below and
            // (wrongly) reports `Vec` as un-`Eq`, blocking `Set[Vec[T]]` /
            // `Map[Vec[T], _]`. Codegen's per-element `karac_eq_Vec_<elem>`
            // walks the contents to match the interpreter (B-2026-06-20-15).
            Type::Named { name, args } if (name == "Vec" || name == "VecDeque") && args.len() == 1 => {
                self.type_supports_eq(&args[0])
            }
            // B-2026-11-08-82, as in `type_supports_partial_eq`.
            Type::Named { name, args } if (name == "Map" || name == "SortedMap") && args.len() == 2 => {
                self.type_supports_eq(&args[1])
            }
            Type::Named { name, args } if (name == "Set" || name == "SortedSet") && args.len() == 1 => {
                true
            }
            // A derive on a generic type holds for the arguments that have the
            // trait: `#[derive(Eq)] struct Pair[T]` is not `Eq` at `Pair[f64]`
            // (B-2026-10-27-25).
            Type::Named { name, args } => {
                let derived = |d: &std::collections::HashSet<String>| {
                    d.contains("Eq") && args.iter().all(|a| self.type_supports_eq(a))
                };
                if let Some(info) = self.structs.get(name) {
                    derived(&info.derived_traits) || self.has_user_impl_eq(name)
                } else if let Some(info) = self.enums.get(name) {
                    derived(&info.derived_traits) || self.has_user_impl_eq(name)
                } else {
                    // Unknown type — permissive to avoid cascading errors
                    // when the resolver has already flagged it.
                    true
                }
            }
            Type::Rc(inner) | Type::Arc(inner) => self.type_supports_eq(inner),
            Type::Shared(name) => {
                if let Some(info) = self.structs.get(name) {
                    info.derived_traits.contains("Eq") || self.has_user_impl_eq(name)
                } else if let Some(info) = self.enums.get(name) {
                    info.derived_traits.contains("Eq") || self.has_user_impl_eq(name)
                } else {
                    true
                }
            }
            Type::TypeParam(_) | Type::TypeVar(_) | Type::AssocProjection { .. } | Type::Error => {
                true
            }
            Type::Never => true,
            Type::Function { .. }
            | Type::OnceFunction { .. }
            | Type::Pointer { .. }
            | Type::Weak(_)
            // `impl Trait` existentials only carry the trait surface,
            // not the witness's derive metadata; the derive-matches-bound
            // path is handled directly in `type_satisfies_bound` by the
            // existential-trait-name comparison, not via these helpers.
            | Type::Existential { .. }
            | Type::Dyn { .. } => false,
            // Shape-kinded args are not value types — no derive surface.
            Type::Shape(_) => false,
        }
    }

    /// Check whether a type supports `Hash`. Floats do not — NaN-as-key would
    /// break the hash/eq contract. Named types require `#[derive(Hash)]`.
    pub(super) fn type_supports_hash(&self, ty: &Type) -> bool {
        if let Some(ok) = self.distinct_derive_supported(ty, &["Hash"]) {
            return ok;
        }
        match ty {
            Type::Refinement { base, .. } => self.type_supports_hash(base),
            Type::Int(_) | Type::UInt(_) | Type::Bool | Type::Char | Type::Str | Type::Unit => true,
            Type::Float(_) => false,
            Type::Tuple(elems) => elems.iter().all(|e| self.type_supports_hash(e)),
            Type::Array { element, .. } => self.type_supports_hash(element),
            Type::Vector { element, .. } => self.type_supports_hash(element),
            Type::Slice { element, .. } => self.type_supports_hash(element),
            Type::Ref(inner) | Type::MutRef(inner) => self.type_supports_hash(inner),
            // `Vec[T]` hashes by content when `T` does — element-wise, like the
            // `Array`/`Slice` arms above. Without this arm the built-in `Vec`
            // (registered in `env.structs` with no derived traits) falls through
            // to the generic `Named` lookup below and reports `Vec` as un-`Hash`,
            // blocking `Set[Vec[T]]` / `Map[Vec[T], _]`. Codegen's per-element
            // `karac_hash_Vec_<elem>` walks the contents to match (B-2026-06-20-15).
            Type::Named { name, args } if name == "Vec" && args.len() == 1 => {
                self.type_supports_hash(&args[0])
            }
            // A `Str` view hashes as its `String` does: the UTF-8 bytes, then
            // `0xff` (collections.md § Hashing, Lookup by view; B-2026-11-25-28).
            Type::Named { name, .. } if name == "Str" || name == "StringSlice" => true,
            // A derive on a generic type holds for the arguments that have the
            // trait: `#[derive(Hash)] struct Pair[T]` is not `Hash` at `Pair[f64]`
            // (B-2026-10-27-25).
            Type::Named { name, args } => {
                let derived = |d: &std::collections::HashSet<String>| {
                    d.contains("Hash") && args.iter().all(|a| self.type_supports_hash(a))
                };
                if let Some(info) = self.structs.get(name) {
                    derived(&info.derived_traits) || self.has_user_impl_hash(name)
                } else if let Some(info) = self.enums.get(name) {
                    derived(&info.derived_traits) || self.has_user_impl_hash(name)
                } else {
                    true
                }
            }
            Type::Rc(inner) | Type::Arc(inner) => self.type_supports_hash(inner),
            Type::Shared(name) => {
                if let Some(info) = self.structs.get(name) {
                    info.derived_traits.contains("Hash") || self.has_user_impl_hash(name)
                } else if let Some(info) = self.enums.get(name) {
                    info.derived_traits.contains("Hash") || self.has_user_impl_hash(name)
                } else {
                    true
                }
            }
            Type::TypeParam(_) | Type::TypeVar(_) | Type::AssocProjection { .. } | Type::Error => {
                true
            }
            Type::Never => true,
            Type::Function { .. }
            | Type::OnceFunction { .. }
            | Type::Pointer { .. }
            | Type::Weak(_)
            // `impl Trait` existentials only carry the trait surface,
            // not the witness's derive metadata; the derive-matches-bound
            // path is handled directly in `type_satisfies_bound` by the
            // existential-trait-name comparison, not via these helpers.
            | Type::Existential { .. }
            | Type::Dyn { .. } => false,
            // Shape-kinded args are not value types — no derive surface.
            Type::Shape(_) => false,
        }
    }

    /// True if the user has an `impl Ord for Type` registered on the
    /// canonical type name. Sibling to the `derived_traits` check below;
    /// lets a user-supplied `cmp` (which can encode arbitrary order —
    /// reverse, custom tiebreaks, partial-field — that the derive-equivalent
    /// field cascade can't reproduce) count toward the Ord bound at any
    /// consumer site. Scans `env.impls` directly: the impl list is small
    /// (one entry per impl block), and Ord checks aren't a hot path. The
    /// codegen consumer (`emit_sort_by_key_inline_thunk`) consults
    /// `Program.user_ord_typed_exprs` to dispatch to the user's compiled
    /// `Type.cmp` indirectly.
    /// Whether `name` carries a hand-written `impl Hash` (B-2026-08-26-10).
    ///
    /// Counts toward the `Hash` bound for the same reason `has_user_impl_ord`
    /// counts toward `Ord`: both backends now dispatch to the impl, so refusing
    /// it here would reject a program they can both run correctly.
    pub(super) fn has_user_impl_hash(&self, name: &str) -> bool {
        self.impls
            .iter()
            .any(|imp| imp.trait_name.as_deref() == Some("Hash") && imp.target_type == name)
    }

    /// Whether `name` carries a hand-written `impl PartialEq` AND the `impl Eq`
    /// marker.
    ///
    /// BOTH, because that is already the `==` operator's rule — a bare
    /// `impl PartialEq` does not drive `==`, and a container that hashed through
    /// a user impl while comparing structurally would place a key by one rule
    /// and look it up by another.
    pub(super) fn has_user_impl_eq(&self, name: &str) -> bool {
        let has = |t: &str| {
            self.impls
                .iter()
                .any(|imp| imp.trait_name.as_deref() == Some(t) && imp.target_type == name)
        };
        has("PartialEq") && has("Eq")
    }

    pub(super) fn has_user_impl_ord(&self, name: &str) -> bool {
        self.impls
            .iter()
            .any(|imp| imp.trait_name.as_deref() == Some("Ord") && imp.target_type == name)
    }

    /// Whether the ordering operators on `name` route to a hand-written body:
    /// a trait impl of `PartialOrd` or `Ord` supplying `partial_cmp`, `cmp`,
    /// or all four of `lt`/`le`/`gt`/`ge` (see the operator gate,
    /// `ordering_operator_dispatches`, which asks this).
    pub(super) fn user_ordering_dispatches(&self, name: &str) -> bool {
        let has_ordering_impl = self.impls.iter().any(|imp| {
            matches!(imp.trait_name.as_deref(), Some("PartialOrd") | Some("Ord"))
                && imp.target_type == name
        });
        if !has_ordering_impl {
            return false;
        }
        let provides = |m: &str| {
            self.impls.iter().any(|imp| {
                imp.trait_name.is_some() && imp.target_type == name && imp.methods.contains_key(m)
            })
        };
        provides("partial_cmp")
            || provides("cmp")
            || ["lt", "le", "gt", "ge"].iter().all(|m| provides(m))
    }

    /// A `T: PartialOrd` bound: the derive's question
    /// ([`Self::type_supports_partial_ord`]), or a hand-written ordering impl
    /// that calls route to (B-2026-11-26-15). The derive predicate stays as
    /// it is, because a derive over a field of such a type would still order
    /// it field by field; a bound only hands the value to code that compares
    /// it through its operators or `partial_cmp`, which reach the body.
    pub(super) fn satisfies_partial_ord_bound(&self, ty: &Type) -> bool {
        if self.type_supports_partial_ord(ty) {
            return true;
        }
        match ty {
            // The impl's own bounds hold too: `impl[T: Ord] PartialOrd for
            // Pr[T]` orders `Pr[i64]` and not `Pr[f64]`.
            Type::Named { name, args } => {
                self.user_ordering_dispatches(name)
                    && (self.has_applicable_impl("PartialOrd", name, args)
                        || self.has_applicable_impl("Ord", name, args))
            }
            Type::Shared(name) => self.user_ordering_dispatches(name),
            Type::Ref(inner) | Type::MutRef(inner) => self.satisfies_partial_ord_bound(inner),
            Type::Refinement { base, .. } => self.satisfies_partial_ord_bound(base),
            _ => false,
        }
    }

    /// Whether a named aggregate's DERIVED ordering is one both backends can
    /// actually lower — the typechecker's half of the parity rule the two
    /// backends already enforce independently (B-2026-08-27-47).
    ///
    /// A GENERIC aggregate is the case that splits. Its derived comparator has
    /// to order a payload whose type is a type PARAMETER, and neither backend
    /// can do that from the declaration alone: codegen's
    /// `emit_cmp_fn_for_type_expr` refuses a `Path` carrying `generic_args` and
    /// its `emit_cmp_fn_for_enum` reads payload `TypeExpr`s by enum NAME (so it
    /// sees `T`, never the instantiation), and the interpreter's
    /// `aggregate_is_orderable` requires `generic_params.is_empty()` for the
    /// same stated reason. Both therefore reject `<` on `Option[i64]` —
    /// agreeing, so `<` never split.
    ///
    /// `.cmp` DID split, because it never consulted that gate on either side:
    /// the typechecker admitted it here (this arm asked only about the derive,
    /// never about the arguments) and the interpreter answered it straight out
    /// of `value_compare`, which has runtime values and so can order what no
    /// static comparator can. Codegen had nothing to answer with and failed the
    /// build. Measured on the unfixed compiler, all check-green and
    /// interp-green and build-refused: `Option[i64]`, `Result[i64, i64]`, a
    /// user `enum MyOpt[T]`, a user `struct Pair[T]`, and — transitively — a
    /// non-generic `#[derive(Ord)]` struct holding an `Option[i64]` field.
    ///
    /// Rejecting here rather than teaching codegen the instantiation is the
    /// narrow move, and it is the direction this file already takes for exactly
    /// this shape: `type_supports_display` rejects `Type::Unit` so that a
    /// meaningless interpolation cannot render differently per backend. Making
    /// the generic case WORK is a real feature (a monomorphized comparator per
    /// instantiation) and belongs on the roadmap, not in a run-vs-build fix;
    /// when it lands, this gate widens and nothing else has to move.
    ///
    /// A user `impl Ord` always wins, generic or not, and that escape hatch is
    /// load-bearing rather than defensive: a hand-written `impl[T] Ord for
    /// W[T]` is a declared function, so it dispatches through the normal
    /// user-impl path and was measured working on BOTH backends. Gating it out
    /// would break code that compiles today.
    /// A generic struct or enum that derives one of `traits` and has no
    /// hand-written `Ord` (B-2026-10-18-15).
    fn derives_generic(&self, name: &str, traits: &[&str]) -> bool {
        let (params, derived) = match (self.structs.get(name), self.enums.get(name)) {
            (Some(i), _) => (&i.generic_params, &i.derived_traits),
            (None, Some(i)) => (&i.generic_params, &i.derived_traits),
            (None, None) => return false,
        };
        !params.is_empty()
            && traits.iter().any(|t| derived.contains(*t))
            && !self.has_user_impl_ord(name)
    }

    fn derived_ord_is_lowerable(
        &self,
        name: &str,
        generic_params: &[String],
        derived: &std::collections::HashSet<String>,
    ) -> bool {
        if self.has_user_impl_ord(name) {
            return true;
        }
        generic_params.is_empty() && derived.contains("Ord")
    }

    /// The `PartialOrd` twin of [`Self::derived_ord_is_lowerable`], carrying the
    /// same rule for the same reason.
    ///
    /// Kept in step deliberately. `register_ord_orderable_types` admits a
    /// `#[derive(PartialOrd)]` aggregate into codegen's comparator family on
    /// equal footing with `Ord`, so a generic one reaches the identical dead
    /// end; letting only the `Ord` spelling narrow would leave
    /// `#[derive(PartialOrd)] struct S { x: Option[i64] }` accepted at check and
    /// refused at build — the very split being closed, moved one derive over.
    fn derived_partial_ord_is_lowerable(
        &self,
        name: &str,
        generic_params: &[String],
        derived: &std::collections::HashSet<String>,
    ) -> bool {
        if self.has_user_impl_ord(name) {
            return true;
        }
        generic_params.is_empty() && (derived.contains("PartialOrd") || derived.contains("Ord"))
    }

    pub(super) fn type_supports_ord(&self, ty: &Type) -> bool {
        if let Some(ok) = self.distinct_derive_supported(ty, &["Ord"]) {
            return ok;
        }
        match ty {
            Type::Refinement { base, .. } => self.type_supports_ord(base),
            Type::Int(_) | Type::UInt(_) | Type::Bool | Type::Char | Type::Str | Type::Unit => true,
            Type::Float(_) => false,
            Type::Tuple(elems) => elems.iter().all(|e| self.type_supports_ord(e)),
            Type::Array { element, .. } => self.type_supports_ord(element),
            Type::Vector { element, .. } => self.type_supports_ord(element),
            Type::Slice { element, .. } => self.type_supports_ord(element),
            Type::Ref(inner) | Type::MutRef(inner) => self.type_supports_ord(inner),
            Type::Named { name, args } if self.vec_is_ord && (name == "Vec" || name == "VecDeque") && args.len() == 1 => {
                self.type_supports_ord(&args[0])
            }
            // B-2026-10-18-15: the v2 pipeline orders a generic aggregate's
            // derive field by field at each instance, so `Dec[String]` is
            // `Ord` when `String` is (design.md § Derive, "Bounds propagate").
            Type::Named { name, args }
                if self.vec_is_ord && !args.is_empty() && self.derives_generic(name, &["Ord"]) =>
            {
                args.iter().all(|a| self.type_supports_ord(a))
            }
            Type::Named { name, .. } => {
                if let Some(info) = self.structs.get(name) {
                    self.derived_ord_is_lowerable(name, &info.generic_params, &info.derived_traits)
                } else if let Some(info) = self.enums.get(name) {
                    self.derived_ord_is_lowerable(name, &info.generic_params, &info.derived_traits)
                } else {
                    true
                }
            }
            Type::Rc(inner) | Type::Arc(inner) => self.type_supports_ord(inner),
            Type::Shared(name) => {
                if let Some(info) = self.structs.get(name) {
                    self.derived_ord_is_lowerable(name, &info.generic_params, &info.derived_traits)
                } else if let Some(info) = self.enums.get(name) {
                    self.derived_ord_is_lowerable(name, &info.generic_params, &info.derived_traits)
                } else {
                    true
                }
            }
            Type::TypeParam(_) | Type::TypeVar(_) | Type::AssocProjection { .. } | Type::Error => {
                true
            }
            Type::Never => true,
            Type::Function { .. }
            | Type::OnceFunction { .. }
            | Type::Pointer { .. }
            | Type::Weak(_)
            // `impl Trait` existentials only carry the trait surface,
            // not the witness's derive metadata; the derive-matches-bound
            // path is handled directly in `type_satisfies_bound` by the
            // existential-trait-name comparison, not via these helpers.
            | Type::Existential { .. }
            | Type::Dyn { .. } => false,
            // Shape-kinded args are not value types — no derive surface.
            Type::Shape(_) => false,
        }
    }

    /// Check whether a type supports `PartialOrd` (admits NaN for floats).
    pub(super) fn type_supports_partial_ord(&self, ty: &Type) -> bool {
        if let Some(ok) = self.distinct_derive_supported(ty, &["PartialOrd", "Ord"]) {
            return ok;
        }
        match ty {
            Type::Refinement { base, .. } => self.type_supports_partial_ord(base),
            Type::Int(_)
            | Type::UInt(_)
            | Type::Float(_)
            | Type::Bool
            | Type::Char
            | Type::Str
            | Type::Unit => true,
            Type::Tuple(elems) => elems.iter().all(|e| self.type_supports_partial_ord(e)),
            Type::Array { element, .. } => self.type_supports_partial_ord(element),
            Type::Vector { element, .. } => self.type_supports_partial_ord(element),
            Type::Slice { element, .. } => self.type_supports_partial_ord(element),
            Type::Ref(inner) | Type::MutRef(inner) => self.type_supports_partial_ord(inner),
            Type::Named { name, args } if self.vec_is_ord && (name == "Vec" || name == "VecDeque") && args.len() == 1 => {
                self.type_supports_partial_ord(&args[0])
            }
            Type::Named { name, args }
                if self.vec_is_ord
                    && !args.is_empty()
                    && self.derives_generic(name, &["PartialOrd", "Ord"]) =>
            {
                args.iter().all(|a| self.type_supports_partial_ord(a))
            }
            Type::Named { name, .. } => {
                if let Some(info) = self.structs.get(name) {
                    self.derived_partial_ord_is_lowerable(
                        name,
                        &info.generic_params,
                        &info.derived_traits,
                    )
                } else if let Some(info) = self.enums.get(name) {
                    self.derived_partial_ord_is_lowerable(
                        name,
                        &info.generic_params,
                        &info.derived_traits,
                    )
                } else {
                    true
                }
            }
            Type::Rc(inner) | Type::Arc(inner) => self.type_supports_partial_ord(inner),
            Type::Shared(name) => {
                if let Some(info) = self.structs.get(name) {
                    self.derived_partial_ord_is_lowerable(
                        name,
                        &info.generic_params,
                        &info.derived_traits,
                    )
                } else if let Some(info) = self.enums.get(name) {
                    self.derived_partial_ord_is_lowerable(
                        name,
                        &info.generic_params,
                        &info.derived_traits,
                    )
                } else {
                    true
                }
            }
            Type::TypeParam(_) | Type::TypeVar(_) | Type::AssocProjection { .. } | Type::Error => {
                true
            }
            Type::Never => true,
            Type::Function { .. }
            | Type::OnceFunction { .. }
            | Type::Pointer { .. }
            | Type::Weak(_)
            // `impl Trait` existentials only carry the trait surface,
            // not the witness's derive metadata; the derive-matches-bound
            // path is handled directly in `type_satisfies_bound` by the
            // existential-trait-name comparison, not via these helpers.
            | Type::Existential { .. }
            | Type::Dyn { .. } => false,
            // Shape-kinded args are not value types — no derive surface.
            Type::Shape(_) => false,
        }
    }

    /// Check whether a type supports `Clone`. GAT slice 8b
    /// carry-forward (a). All primitives clone trivially; named
    /// types require `#[derive(Clone)]` (`Copy` implies `Clone` because
    /// `extract_derived_traits` closes the derive set over its
    /// dependencies — B-2026-08-17-33 — so this is an invariant by
    /// construction rather than by a separate validation pass). Used by
    /// `type_satisfies_bound` so a `T: Clone` bound discharges
    /// against the derive metadata directly — built-in derive-only
    /// traits aren't registered as impl-table entries, so without
    /// this path a `: Clone` bound would conservatively reject every
    /// concrete RHS at slice 7's `gat_rhs_satisfies_bound`. Mirrors
    /// the field-shape walk in `type_supports_hash` /
    /// `type_supports_display`.
    pub(super) fn type_supports_clone(&self, ty: &Type) -> bool {
        match ty {
            Type::Refinement { base, .. } => self.type_supports_clone(base),
            Type::Int(_)
            | Type::UInt(_)
            | Type::Float(_)
            | Type::Bool
            | Type::Char
            | Type::Str
            | Type::Unit => true,
            Type::Tuple(elems) => elems.iter().all(|e| self.type_supports_clone(e)),
            Type::Array { element, .. } => self.type_supports_clone(element),
            Type::Vector { element, .. } => self.type_supports_clone(element),
            // Slices clone (the slice header is `(ptr, len)` — bitwise copy);
            // the borrowed data is not duplicated.
            Type::Slice { .. } => true,
            Type::Ref(inner) | Type::MutRef(inner) => self.type_supports_clone(inner),
            Type::Named { name, args } => {
                // Built-in collections clone when their type args clone
                // (Option / Result / Vec / Map / Set follow the standard
                // shape). User-defined types require `#[derive(Clone)]`.
                if matches!(
                    name.as_str(),
                    "Option"
                        | "Result"
                        | "Vec"
                        | "VecDeque"
                        | "Map"
                        | "SortedMap"
                        | "Set"
                        | "SortedSet"
                ) {
                    // A table's hasher is a type, not a value it holds.
                    return crate::typechecker::types::element_args(name, args)
                        .iter()
                        .all(|a| self.type_supports_clone(a));
                }
                // Only an impl whose own bounds hold: the library's
                // `impl[I: Clone] Clone for IterTake[I]` makes
                // `v.iter().take(3)` `Clone` and not `v.iter().map(f).take(3)`,
                // whose closure does not clone (B-2026-11-20-90).
                if self.has_applicable_impl("Clone", name, args) {
                    return true;
                }
                // A `shared` type named directly clones as its handle does
                // (the `Type::Shared` arm below): a refcount bump.
                // A derived `Clone` on a generic type is
                // `impl[T: Clone] Clone for Pair[T]` (design.md §9): it holds
                // for the arguments that clone (B-2026-11-07-11).
                let derived = |d: &std::collections::HashSet<String>| {
                    d.contains("Clone") && args.iter().all(|a| self.type_supports_clone(a))
                };
                if let Some(info) = self.structs.get(name) {
                    info.is_shared || derived(&info.derived_traits)
                } else if let Some(info) = self.enums.get(name) {
                    info.is_shared || derived(&info.derived_traits)
                } else if let Some(traits) = self.distinct_types.get(name) {
                    traits.contains("Clone")
                } else {
                    // Unknown nominal — be permissive to avoid noise on
                    // unrelated diagnostics. Slice 7's
                    // gat_rhs_satisfies_bound path tightens to
                    // false-conservative when reaching here from the
                    // bounds path; the impl-site discharge surface
                    // dominates.
                    true
                }
            }
            Type::Rc(inner) | Type::Arc(inner) => self.type_supports_clone(inner),
            // A `shared` handle always clones: `clone` on one is the REFCOUNT
            // BUMP, not a deep copy, which is exactly what
            // `clone_receiver_self_type` already answers for the same type
            // (unconditionally, no derive consulted). This arm used to demand
            // `#[derive(Clone)]` and so disagreed with it: `n.clone()` on a
            // `shared struct` compiled, while `Option[N]` holding the same
            // handle reported `no method 'clone' on type 'Option'`, because the
            // payload check came through here. The two answers have to match —
            // a derive cannot be what decides whether a refcount can be
            // incremented (B-2026-08-27-23).
            Type::Shared(_) => true,
            Type::TypeParam(_) | Type::TypeVar(_) | Type::AssocProjection { .. } | Type::Error => {
                true
            }
            Type::Never => true,
            Type::Function { .. }
            | Type::OnceFunction { .. }
            | Type::Pointer { .. }
            | Type::Weak(_)
            // `impl Trait` existentials only carry the trait surface,
            // not the witness's derive metadata; the derive-matches-bound
            // path is handled directly in `type_satisfies_bound` by the
            // existential-trait-name comparison, not via these helpers.
            | Type::Existential { .. } => false,
            // A `dyn` value clones through its table, which has a `clone`
            // entry when the trait has `Clone` as a supertrait.
            Type::Dyn { trait_name, .. } => self
                .supertrait_closure_traits(trait_name)
                .iter()
                .any(|t| t == "Clone"),
            // Shape-kinded args are not value types — no derive surface.
            Type::Shape(_) => false,
        }
    }

    /// Whether `x.hash(mut h)` on a `ty` is the builtin feed collections.md
    /// § Hashing gives ("What the built-in types feed"): `ty` is a concrete
    /// `Hash` type the MIR builder feeds part by part, with no `hash` method
    /// of its own (B-2026-10-31-25). A type parameter's `hash` comes from its
    /// bound, and a hand-written impl from the impl table.
    pub(super) fn hash_is_builtin(&self, ty: &Type) -> bool {
        let fed = |t: &Type| self.hash_is_builtin(t) || self.hash_has_own(t);
        match ty {
            // `i128` / `u128` feed `write_i128` / `write_u128`
            // (B-2026-11-17-65).
            Type::Int(_) | Type::UInt(_) => true,
            // An integer literal not fixed yet (`(1, 2).hash(mut h)`) is an
            // integer whichever width it settles on.
            Type::TypeVar(id) => matches!(
                super::inference::literal_var_kind(*id),
                Some(super::inference::LiteralKind::Int)
            ),
            Type::Bool | Type::Char | Type::Str | Type::Unit => true,
            Type::Tuple(elems) => elems.iter().all(fed),
            Type::Array { element, .. } | Type::Slice { element, .. } => fed(element),
            Type::Ref(inner) | Type::MutRef(inner) => fed(inner),
            Type::Named { name, args } => {
                let library = matches!(
                    name.as_str(),
                    "String" | "Str" | "StringSlice" | "Vec" | "Option" | "Result"
                );
                let program = self
                    .structs
                    .get(name)
                    .map(|i| &i.derived_traits)
                    .or_else(|| self.enums.get(name).map(|i| &i.derived_traits));
                (library || program.is_some_and(|d| d.contains("Hash")))
                    && !self.has_user_impl_hash(name)
                    && args.iter().all(fed)
                    && self.type_supports_hash(ty)
            }
            _ => false,
        }
    }

    /// Whether `ty` hashes through a `hash` method of its own (a hand-written
    /// `impl Hash`).
    fn hash_has_own(&self, ty: &Type) -> bool {
        match ty {
            Type::Named { name, .. } | Type::Shared(name) => self.has_user_impl_hash(name),
            _ => false,
        }
    }

    /// Whether `x.fmt_debug()` on a `ty` is the builtin `Debug` text: `ty`
    /// is a concrete `Debug` type with no `fmt_debug` method of its own.
    pub(super) fn debug_text_is_builtin(&self, ty: &Type) -> bool {
        match ty {
            Type::TypeParam(_)
            | Type::TypeVar(_)
            | Type::AssocProjection { .. }
            | Type::Error
            | Type::Never => false,
            Type::Named { name, .. } | Type::Shared(name) => {
                (self.structs.contains_key(name)
                    || self.enums.contains_key(name)
                    || matches!(
                        name.as_str(),
                        "Option"
                            | "Result"
                            | "String"
                            | "Vec"
                            | "VecDeque"
                            | "Map"
                            | "SortedMap"
                            | "Set"
                            | "SortedSet"
                    ))
                    && self
                        .find_methods_with_args(name, &[], "fmt_debug")
                        .is_empty()
                    && self.type_supports_debug(ty)
            }
            _ => self.type_supports_debug(ty),
        }
    }

    /// Check whether a type supports `Debug`. GAT slice 8b
    /// carry-forward (a). Mirrors `type_supports_display` (Debug is
    /// the developer-facing dump trait — same surface coverage as
    /// Display for slice 7/8 bound-discharge purposes).
    pub(super) fn type_supports_debug(&self, ty: &Type) -> bool {
        match ty {
            Type::Refinement { base, .. } => self.type_supports_debug(base),
            Type::Int(_)
            | Type::UInt(_)
            | Type::Float(_)
            | Type::Bool
            | Type::Char
            | Type::Str
            | Type::Unit => true,
            Type::Tuple(elems) => elems.iter().all(|e| self.type_supports_debug(e)),
            Type::Array { element, .. } => self.type_supports_debug(element),
            Type::Vector { element, .. } => self.type_supports_debug(element),
            Type::Slice { element, .. } => self.type_supports_debug(element),
            Type::Ref(inner) | Type::MutRef(inner) => self.type_supports_debug(inner),
            Type::Named { name, args } => {
                if matches!(
                    name.as_str(),
                    "Option"
                        | "Result"
                        | "Vec"
                        | "VecDeque"
                        | "Map"
                        | "SortedMap"
                        | "Set"
                        | "SortedSet"
                ) {
                    return crate::typechecker::types::element_args(name, args)
                        .iter()
                        .all(|a| self.type_supports_debug(a));
                }
                if self.has_impl("Debug", name, args) {
                    return true;
                }
                // A derived `Debug` on a generic type is
                // `impl[T: Debug] Debug for W[T]` (design.md §9): it holds
                // for the arguments that are `Debug` (B-2026-11-07-11).
                let derived = |d: &std::collections::HashSet<String>| {
                    d.contains("Debug") && args.iter().all(|a| self.type_supports_debug(a))
                };
                if let Some(info) = self.structs.get(name) {
                    derived(&info.derived_traits)
                } else if let Some(info) = self.enums.get(name) {
                    derived(&info.derived_traits)
                } else if let Some(traits) = self.distinct_types.get(name) {
                    traits.contains("Debug")
                } else {
                    true
                }
            }
            Type::Rc(inner) | Type::Arc(inner) => self.type_supports_debug(inner),
            Type::Shared(name) => {
                if let Some(info) = self.structs.get(name) {
                    info.derived_traits.contains("Debug")
                } else if let Some(info) = self.enums.get(name) {
                    info.derived_traits.contains("Debug")
                } else {
                    true
                }
            }
            Type::TypeParam(_) | Type::TypeVar(_) | Type::AssocProjection { .. } | Type::Error => {
                true
            }
            Type::Never => true,
            Type::Function { .. }
            | Type::OnceFunction { .. }
            | Type::Pointer { .. }
            | Type::Weak(_)
            // `impl Trait` existentials only carry the trait surface,
            // not the witness's derive metadata; the derive-matches-bound
            // path is handled directly in `type_satisfies_bound` by the
            // existential-trait-name comparison, not via these helpers.
            | Type::Existential { .. }
            | Type::Dyn { .. } => false,
            // Shape-kinded args are not value types — no derive surface.
            Type::Shape(_) => false,
        }
    }

    /// Check whether a type is Copy (primitive or derives Copy).
    pub(super) fn is_type_copy(&self, ty: &Type) -> bool {
        match ty {
            Type::Int(_)
            | Type::UInt(_)
            | Type::Float(_)
            | Type::Bool
            | Type::Char
            | Type::Unit
            | Type::Never
            | Type::Error => true,
            // Raw pointers are unconditionally Copy — they carry no
            // ownership and have no destructor, which is exactly why
            // `E_UNION_FIELD_NOT_COPY` tells the user to "hold it behind a
            // raw pointer (`*mut T` / `*const T`)". Without this arm that
            // advice was self-contradictory: `union EpollData { ptr: *mut
            // u8, … }` — the canonical `epoll_data` FFI shape, and the one
            // the union feature exists to express — was rejected by the
            // very rule whose suggested fix it already followed
            // (B-2026-08-12-7).
            Type::Pointer { .. } => true,
            // core-semantics §1.1: `ref T` is `Copy`, whatever `T` is, and
            // `mut ref T` is not. Copying a shared reference gives a second
            // reference to the same place (B-2026-11-22-51).
            Type::Ref(_) => true,
            Type::Tuple(types) => types.iter().all(|t| self.is_type_copy(t)),
            // Array[T, N] is Copy iff T is Copy.
            Type::Array { element, .. } => self.is_type_copy(element),
            Type::Vector { element, .. } => self.is_type_copy(element),
            // Slice[T] is unconditionally Copy; mut Slice[T] is not.
            Type::Slice { mutable, .. } => !mutable,
            Type::Named { name, args } => {
                // Option[T] / Result[T, E] are Copy when all type args are Copy.
                if matches!(name.as_str(), "Option" | "Result") {
                    return args.iter().all(|a| self.is_type_copy(a));
                }
                if let Some(info) = self.structs.get(name) {
                    info.derived_traits.contains("Copy")
                } else if let Some(info) = self.enums.get(name) {
                    info.derived_traits.contains("Copy")
                } else if let Some(traits) = self.distinct_types.get(name) {
                    traits.contains("Copy")
                } else {
                    false
                }
            }
            _ => false,
        }
    }

    /// Whether `ty` has a reachable `default()` — the predicate driving
    /// the `#[derive(Default)]` field check. v1 floor scope: the scalar
    /// primitives (every one has a zero-like value, floats included) plus
    /// any named struct/enum that actually carries a `default` method
    /// (derive-synthesized in [`crate::desugar`] or hand-written). Container
    /// / generic-argument / tuple / ref field types are out of scope and
    /// report cleanly here rather than failing deep in the synthesized
    /// body. Permissive on inference/error types to avoid cascading.
    pub(super) fn type_supports_default(&self, ty: &Type) -> bool {
        match ty {
            Type::Refinement { base, .. } => self.type_supports_default(base),
            Type::Int(_)
            | Type::UInt(_)
            | Type::Float(_)
            | Type::Bool
            | Type::Char
            | Type::Str
            | Type::Unit => true,
            // The standard collections default to empty and `Option` to
            // `None`, whatever their element types (B-2026-10-13-27).
            Type::Named { name, .. } if is_builtin_default_head(name) => true,
            Type::Named { name, .. } | Type::Shared(name) => self.type_has_default_method(name),
            Type::Tuple(parts) => parts.iter().all(|t| self.type_supports_default(t)),
            // `N` defaults of the element, as Rust's `[T; N]: Default`.
            Type::Array { element, .. } => self.type_supports_default(element),
            Type::TypeVar(_) | Type::AssocProjection { .. } | Type::Error => true,
            _ => false,
        }
    }

    /// True when some impl of `name` (inherent or trait) exposes a
    /// `default` associated function. The derive-synthesized inherent
    /// impl is already present in `env.impls` by the time the recursive
    /// derive validator runs, so this answers both the derived and the
    /// hand-written case uniformly.
    fn type_has_default_method(&self, name: &str) -> bool {
        self.impls
            .iter()
            .any(|imp| imp.target_type == name && imp.methods.contains_key("default"))
    }
}

/// A standard type that is `Default` whatever its arguments: the
/// collections (empty) and `Option` (`None`).
pub(super) fn is_builtin_default_head(name: &str) -> bool {
    matches!(
        name,
        "Vec" | "VecDeque" | "Set" | "SortedSet" | "Map" | "SortedMap" | "Option"
    )
}

/// Whether the type's lint overrides include `#[allow(derive_clone_allocates)]`
/// (phase-8-stdlib-floor item 5 suppression). Free fn — no `self` needed.
fn allows_derive_clone_allocates(overrides: &[crate::lints::LintLevelOverride]) -> bool {
    overrides.iter().any(|o| {
        o.lint == "derive_clone_allocates" && matches!(o.level, crate::lints::LintLevel::Allow)
    })
}

/// The type parameters in scope in a distinct type's base: `T` of
/// `distinct type Meters[T] = T`.
fn distinct_scope(d: &crate::ast::DistinctTypeDef) -> Vec<String> {
    d.generic_params
        .iter()
        .flat_map(|g| &g.params)
        .map(|p| p.name.clone())
        .collect()
}

/// A field's `#[serde(...)]` skips: whether it is left out when writing and
/// when reading, and whether it names a `default: expr` for reading.
fn serde_field_skips(attrs: &[Attribute]) -> (bool, bool, bool) {
    let (mut ser, mut de, mut default_expr) = (false, false, false);
    for attr in attrs.iter().filter(|a| a.is_bare("serde")) {
        for arg in &attr.args {
            let bare = match (&arg.name, &arg.value) {
                (
                    None,
                    Some(Expr {
                        kind: ExprKind::Identifier(k),
                        ..
                    }),
                ) => k.as_str(),
                (Some(k), Some(_)) if k == "default" => {
                    default_expr = true;
                    continue;
                }
                _ => continue,
            };
            match bare {
                "skip" => {
                    ser = true;
                    de = true;
                }
                "skip_serializing" => ser = true,
                "skip_deserializing" => de = true,
                _ => {}
            }
        }
    }
    (ser, de, default_expr)
}
