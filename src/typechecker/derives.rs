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
use super::{extract_derived_traits, TypeErrorKind};

impl<'a> super::TypeChecker<'a> {
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
        if let Type::Named { name, .. } = ty {
            if let Some(traits) = self.env.distinct_types.get(name) {
                return Some(wanted.iter().any(|w| traits.contains(*w)));
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
            Type::Named { name, args } => {
                // A user-provided `impl Eq for Name` is sufficient — the
                // lowering pass dispatches `==`/`!=` through it. Falls back
                // to `#[derive(Eq)]`/`#[derive(PartialEq)]` when no impl is
                // registered (e.g. for compiler-provided structural eq on
                // built-in enums like `Option`/`Result`).
                if self.env.has_impl("Eq", name, args) {
                    return true;
                }
                if let Some(info) = self.env.structs.get(name) {
                    info.derived_traits.contains("Eq") || info.derived_traits.contains("PartialEq")
                } else if let Some(info) = self.env.enums.get(name) {
                    info.derived_traits.contains("Eq") || info.derived_traits.contains("PartialEq")
                } else {
                    true
                }
            }
            Type::Rc(inner) | Type::Arc(inner) => self.type_supports_partial_eq(inner),
            Type::Shared(name) => {
                if self.env.has_impl("Eq", name, &[]) {
                    return true;
                }
                if let Some(info) = self.env.structs.get(name) {
                    info.derived_traits.contains("Eq") || info.derived_traits.contains("PartialEq")
                } else if let Some(info) = self.env.enums.get(name) {
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
            | Type::Existential { .. } => false,
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
            Type::Named { name, args } if name == "Vec" && args.len() == 1 => {
                self.type_supports_eq(&args[0])
            }
            Type::Named { name, .. } => {
                if let Some(info) = self.env.structs.get(name) {
                    info.derived_traits.contains("Eq")
                } else if let Some(info) = self.env.enums.get(name) {
                    info.derived_traits.contains("Eq")
                } else {
                    // Unknown type — permissive to avoid cascading errors
                    // when the resolver has already flagged it.
                    true
                }
            }
            Type::Rc(inner) | Type::Arc(inner) => self.type_supports_eq(inner),
            Type::Shared(name) => {
                if let Some(info) = self.env.structs.get(name) {
                    info.derived_traits.contains("Eq")
                } else if let Some(info) = self.env.enums.get(name) {
                    info.derived_traits.contains("Eq")
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
            Type::Named { name, .. } => {
                if let Some(info) = self.env.structs.get(name) {
                    info.derived_traits.contains("Hash")
                } else if let Some(info) = self.env.enums.get(name) {
                    info.derived_traits.contains("Hash")
                } else {
                    true
                }
            }
            Type::Rc(inner) | Type::Arc(inner) => self.type_supports_hash(inner),
            Type::Shared(name) => {
                if let Some(info) = self.env.structs.get(name) {
                    info.derived_traits.contains("Hash")
                } else if let Some(info) = self.env.enums.get(name) {
                    info.derived_traits.contains("Hash")
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
    pub(super) fn has_user_impl_ord(&self, name: &str) -> bool {
        self.env
            .impls
            .iter()
            .any(|imp| imp.trait_name.as_deref() == Some("Ord") && imp.target_type == name)
    }

    /// Check whether a type supports total `Ord`. Floats do not (see Eq).
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
            Type::Named { name, .. } => {
                if let Some(info) = self.env.structs.get(name) {
                    info.derived_traits.contains("Ord") || self.has_user_impl_ord(name)
                } else if let Some(info) = self.env.enums.get(name) {
                    info.derived_traits.contains("Ord") || self.has_user_impl_ord(name)
                } else {
                    true
                }
            }
            Type::Rc(inner) | Type::Arc(inner) => self.type_supports_ord(inner),
            Type::Shared(name) => {
                if let Some(info) = self.env.structs.get(name) {
                    info.derived_traits.contains("Ord") || self.has_user_impl_ord(name)
                } else if let Some(info) = self.env.enums.get(name) {
                    info.derived_traits.contains("Ord") || self.has_user_impl_ord(name)
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

    /// Check whether a type implements `Display`.
    /// All primitives support Display. Built-in containers (Vec, Map, SortedSet,
    /// Option, Result) support Display when their type arguments do.
    /// Named user types require `#[derive(Display)]`.
    pub(super) fn type_supports_display(&self, ty: &Type) -> bool {
        if let Some(ok) = self.distinct_derive_supported(ty, &["Display"]) {
            return ok;
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
            Type::Named { name, args } => match name.as_str() {
                "Vec" | "Option" | "SortedSet" | "Set" if args.len() == 1 => {
                    self.type_supports_display(&args[0])
                }
                "Map" | "SortedMap" | "Result" if args.len() == 2 => {
                    self.type_supports_display(&args[0]) && self.type_supports_display(&args[1])
                }
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
            Type::Named { name, .. } => {
                if let Some(info) = self.env.structs.get(name) {
                    info.derived_traits.contains("PartialOrd")
                        || info.derived_traits.contains("Ord")
                } else if let Some(info) = self.env.enums.get(name) {
                    info.derived_traits.contains("PartialOrd")
                        || info.derived_traits.contains("Ord")
                } else {
                    true
                }
            }
            Type::Rc(inner) | Type::Arc(inner) => self.type_supports_partial_ord(inner),
            Type::Shared(name) => {
                if let Some(info) = self.env.structs.get(name) {
                    info.derived_traits.contains("PartialOrd")
                        || info.derived_traits.contains("Ord")
                } else if let Some(info) = self.env.enums.get(name) {
                    info.derived_traits.contains("PartialOrd")
                        || info.derived_traits.contains("Ord")
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

    /// Check whether a type supports `Clone`. GAT slice 8b
    /// carry-forward (a). All primitives clone trivially; named
    /// types require `#[derive(Clone)]` (Copy implies Clone by the
    /// existing `validate_copy_implies_clone` rule, so this is
    /// already an invariant in practice). Used by
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
                    return args.iter().all(|a| self.type_supports_clone(a));
                }
                if self.env.has_impl("Clone", name, args) {
                    return true;
                }
                if let Some(info) = self.env.structs.get(name) {
                    info.derived_traits.contains("Clone")
                } else if let Some(info) = self.env.enums.get(name) {
                    info.derived_traits.contains("Clone")
                } else if let Some(traits) = self.env.distinct_types.get(name) {
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
            Type::Shared(name) => {
                if let Some(info) = self.env.structs.get(name) {
                    info.derived_traits.contains("Clone")
                } else if let Some(info) = self.env.enums.get(name) {
                    info.derived_traits.contains("Clone")
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
                    return args.iter().all(|a| self.type_supports_debug(a));
                }
                if self.env.has_impl("Debug", name, args) {
                    return true;
                }
                if let Some(info) = self.env.structs.get(name) {
                    info.derived_traits.contains("Debug")
                } else if let Some(info) = self.env.enums.get(name) {
                    info.derived_traits.contains("Debug")
                } else if let Some(traits) = self.env.distinct_types.get(name) {
                    traits.contains("Debug")
                } else {
                    true
                }
            }
            Type::Rc(inner) | Type::Arc(inner) => self.type_supports_debug(inner),
            Type::Shared(name) => {
                if let Some(info) = self.env.structs.get(name) {
                    info.derived_traits.contains("Debug")
                } else if let Some(info) = self.env.enums.get(name) {
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
            | Type::Existential { .. } => false,
            // Shape-kinded args are not value types — no derive surface.
            Type::Shape(_) => false,
        }
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
                if let Some(info) = self.env.structs.get(name) {
                    info.derived_traits.contains("Copy")
                } else if let Some(info) = self.env.enums.get(name) {
                    info.derived_traits.contains("Copy")
                } else if let Some(traits) = self.env.distinct_types.get(name) {
                    traits.contains("Copy")
                } else {
                    false
                }
            }
            _ => false,
        }
    }

    /// Validate that #[derive(Copy)] structs/enums have all-Copy fields, and
    /// that distinct types with #[derive(Copy)] have a Copy base type.
    pub(super) fn validate_derive_copy(&mut self) {
        self.validate_derived_trait("Copy", |this, ty| this.is_type_copy(ty));
        // Check distinct types: base type must be Copy.
        let distinct_items: Vec<_> = self
            .program
            .items
            .iter()
            .filter_map(|item| {
                if let Item::DistinctType(d) = item {
                    let traits = extract_derived_traits(&d.attributes);
                    if traits.contains("Copy") {
                        return Some((d.name.clone(), d.span.clone(), d.base_type.clone()));
                    }
                }
                None
            })
            .collect();
        for (name, span, base_ty_expr) in distinct_items {
            let base_ty = self.lower_type_expr(&base_ty_expr, &[]);
            if !self.is_type_copy(&base_ty) {
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

    /// Validate that every type deriving Copy also derives Clone.
    pub(super) fn validate_copy_implies_clone(&mut self) {
        let items: Vec<_> = self.program.items.clone();
        for item in &items {
            match item {
                Item::StructDef(s) => {
                    let traits = extract_derived_traits(&s.attributes);
                    if traits.contains("Copy") && !traits.contains("Clone") {
                        self.type_error(
                            format!(
                                "struct '{}' derives Copy but not Clone; Copy requires Clone",
                                s.name
                            ),
                            s.span.clone(),
                            TypeErrorKind::TypeMismatch,
                        );
                    }
                }
                Item::EnumDef(e) => {
                    let traits = extract_derived_traits(&e.attributes);
                    if traits.contains("Copy") && !traits.contains("Clone") {
                        self.type_error(
                            format!(
                                "enum '{}' derives Copy but not Clone; Copy requires Clone",
                                e.name
                            ),
                            e.span.clone(),
                            TypeErrorKind::TypeMismatch,
                        );
                    }
                }
                Item::DistinctType(d) => {
                    let traits = extract_derived_traits(&d.attributes);
                    if traits.contains("Copy") && !traits.contains("Clone") {
                        self.type_error(
                            format!(
                                "distinct type '{}' derives Copy but not Clone; Copy requires Clone",
                                d.name
                            ),
                            d.span.clone(),
                            TypeErrorKind::TypeMismatch,
                        );
                    }
                }
                _ => {}
            }
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
        self.validate_derive_default();
        self.validate_derive_display_on_enums();
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
                Item::StructDef(s) if s.name == name => Some(s.span.clone()),
                _ => None,
            });
            let Some(struct_span) = struct_span else {
                continue;
            };
            for (field_name, field_ty, _) in &info.fields {
                if !self.type_supports_default(field_ty) {
                    self.type_error(
                        format!(
                            "error[E_DERIVE_DEFAULT_MISSING_FIELD_DEFAULT]: cannot \
                             #[derive(Default)] for '{}' — field '{}' of type '{}' does not \
                             implement 'Default'",
                            name,
                            field_name,
                            type_display(field_ty)
                        ),
                        struct_span.clone(),
                        TypeErrorKind::TypeMismatch,
                    );
                }
            }
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
                        e.span.clone(),
                        TypeErrorKind::TypeMismatch,
                    );
                }
                [only] => {
                    if !matches!(only.kind, VariantKind::Unit) {
                        self.type_error(
                            format!(
                                "error[E_DEFAULT_VARIANT_HAS_PAYLOAD]: #[default] on enum \
                                 variant '{}' requires a field-less variant; either declare \
                                 the marked variant as '{}' (no fields) or write a manual \
                                 'impl {} {{ fn default() -> {} {{ ... }} }}' that constructs \
                                 the desired starting state",
                                only.name, only.name, e.name, e.name
                            ),
                            only.span.clone(),
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
                        e.span.clone(),
                        TypeErrorKind::TypeMismatch,
                    );
                }
            }
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
            Type::Named { name, .. } | Type::Shared(name) => self.type_has_default_method(name),
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
        self.env
            .impls
            .iter()
            .any(|imp| imp.target_type == name && imp.methods.contains_key("default"))
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
        // Collect enum names for the carve-out check. `shared` enums
        // are heap-allocated via RC, so a payload field of type
        // `SharedFoo` is a single pointer word and is allowed.
        let value_enum_names: std::collections::HashSet<String> = self
            .program
            .items
            .iter()
            .filter_map(|item| match item {
                Item::EnumDef(e) if !e.is_shared => Some(e.name.clone()),
                _ => None,
            })
            .collect();

        // Walk every enum variant and inspect its payload field types.
        // The payload field's `TypeExpr` -> head segment is the
        // user-visible type name; if that name is a value enum, emit
        // the diagnostic. We only flag the *direct* head; recursion
        // through `Vec[Inner]` etc. is intentionally allowed
        // (CP5 carve-out is about size-recursion, not name presence).
        let items: Vec<_> = self.program.items.clone();
        for item in &items {
            if let Item::EnumDef(e) = item {
                if e.is_shared {
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
                                             v1 only supports up to one level of enum nesting; \
                                             either flatten the variant, mark the inner enum as \
                                             `shared` (RC pointer), or wrap it in a `Vec` / \
                                             collection layer",
                                            e.name, variant.name, head
                                        ),
                                        variant.span.clone(),
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

    /// Validate `#[derive(Arithmetic)]` usage:
    /// - Reject on struct/enum (must use manual impls).
    /// - Reject on distinct types whose base type is non-numeric.
    pub(super) fn validate_derive_arithmetic(&mut self) {
        let items: Vec<_> = self.program.items.clone();
        for item in &items {
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
                        s.span.clone(),
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
                        e.span.clone(),
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
                            d.span.clone(),
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
                        return Some(s.span.clone());
                    }
                }
                None
            });
            let Some(struct_span) = struct_span else {
                continue;
            };
            for (field_name, field_ty, _) in &info.fields {
                if !supports(self, field_ty) {
                    let span = struct_span.clone();
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
                        return Some(e.span.clone());
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
                        enum_span.clone(),
                        TypeErrorKind::TypeMismatch,
                    );
                }
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
        let items: Vec<_> = self.program.items.clone();
        for item in &items {
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
                span.clone(),
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

/// Whether the type's lint overrides include `#[allow(derive_clone_allocates)]`
/// (phase-8-stdlib-floor item 5 suppression). Free fn — no `self` needed.
fn allows_derive_clone_allocates(overrides: &[crate::lints::LintLevelOverride]) -> bool {
    overrides.iter().any(|o| {
        o.lint == "derive_clone_allocates" && matches!(o.level, crate::lints::LintLevel::Allow)
    })
}
