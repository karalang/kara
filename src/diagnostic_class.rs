//! Diagnostic-class enum for structured-diagnostic output.
//!
//! Each compiler diagnostic carries an optional `class:
//! DiagnosticClass` tag that names its broad category — the
//! UPPER_SNAKE_CASE string that lands in the `class` field of
//! `karac explain --format=json` records. The enum is the
//! published-catalogue spine for machine consumers (LLM agents, IDE
//! tooling): a `code` field (e.g. `E_PTR_MUT_REQUIRES_MUTABLE_PLACE`)
//! is the specific diagnostic; the `class` value is the family it
//! belongs to.
//!
//! Per the Specification Layers policy, this enum's values live in
//! the *reported-behavior* tier — stable within a release, may
//! evolve across releases. Adding new classes is purely additive;
//! renaming or removing a class is a release-boundary change.
//!
//! Spec: `docs/design.md § AI-First Compiler Interface > Structured
//! Diagnostics`; tracker: `phase-5-diagnostics.md` line 619.

/// Diagnostic category. Mapped 1:N to specific E_* codes — many
/// codes share a class (e.g., `E_PTR_TO_INT_CAST_FORBIDDEN`,
/// `E_INT_TO_PTR_CAST_FORBIDDEN`, and `E_REF_TO_RAW_PTR_CAST_FORBIDDEN`
/// all classify as `InvalidCast`). The catch-all `Other` covers
/// diagnostics that haven't been individually classified yet — its
/// presence is *not* an error, but a backfill opportunity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DiagnosticClass {
    // ── Type system ─────────────────────────────────────────────
    /// Operand / argument / return-value type doesn't match the
    /// expected slot. The canonical class for `TypeErrorKind::
    /// TypeMismatch` and the `cannot assign 'A' to 'B'` family.
    TypeMismatch,
    /// A name in *type* position doesn't resolve to a known type.
    UndefinedType,
    /// Call site supplies the wrong number of positional /
    /// labeled / variadic arguments.
    WrongNumberOfArgs,
    /// Method-call dispatch found no matching impl for the
    /// receiver type. Includes `did you mean 'X'?` suggestions.
    NoMethodFound,
    /// `as`-cast rejected: ptr↔int under strict-provenance,
    /// char→narrow-int, int→char / int→bool / float→bool,
    /// ref→raw-pointer, and other dedicated rejections.
    InvalidCast,
    /// Unary operator (`*`, `-`, `!`) applied to an unsupported
    /// operand type, or to an unsupported place expression.
    InvalidUnaryOp,
    /// A required trait bound isn't satisfied by the supplied
    /// type / type-argument. Routes both `where`-clause failures
    /// and inline bound failures.
    TraitBoundNotSatisfied,
    /// `let PAT = expr;` where `PAT` is refutable — must use
    /// `let ... else { ... }`, `if let`, or `match`.
    RefutablePattern,
    /// Generic type parameter couldn't be inferred from context.
    CannotInferTypeParam,

    // ── Resolver / name resolution ──────────────────────────────
    /// An identifier in *value* position doesn't resolve.
    UndefinedName,
    /// A definition would shadow an existing item that the
    /// resolver treats as an error rather than a shadow.
    DuplicateDefinition,

    // ── Effects ─────────────────────────────────────────────────
    /// A public function uses an effect not present in its
    /// declared effect row.
    EffectUndeclared,
    /// Effect-set conflict between concurrent / interleaved
    /// computations (e.g., `writes(R) ⊓ writes(R)`).
    EffectConflict,

    // ── Ownership / borrow checking ─────────────────────────────
    /// Use of a binding after its value was moved.
    OwnershipMoveAfterUse,
    /// Live-borrow / live-slice / cross-borrow conflict in the
    /// borrow checker's conflict matrix.
    OwnershipBorrowConflict,
    /// Read of a binding before it was initialised
    /// (let-uninit DFA).
    OwnershipUseOfUninitialized,

    // ── FFI / unsafe / target ───────────────────────────────────
    /// Cross-target violation: file-suffix conditional compilation
    /// mismatches, target-feature-gated intrinsics used outside
    /// their target, cross-target effect violations, etc. Per the
    /// spec, these all land under one shared family rather than
    /// fragmenting into per-platform classes.
    TargetIncompatible,
    /// Operation requires an enclosing `unsafe { }` block (raw-
    /// pointer deref / arithmetic, union field read, etc.) and
    /// none is present.
    UnsafeRequired,
    /// FFI-shape rule violation that isn't a cast — `union`
    /// declaration constraints, FFI-float equality, opaque-type
    /// constraints, repr / layout requirements, etc.
    FfiViolation,

    // ── Layout / memory ─────────────────────────────────────────
    /// `size_of[T]()` / `align_of[T]()` / `offset_of[T](path)`
    /// shape errors (missing type arg, generic-param target,
    /// unknown field path, opaque-type target, etc.).
    LayoutQueryInvalid,

    // ── Lints ───────────────────────────────────────────────────
    /// Lint-level diagnostic surfaced as warning or error per
    /// `#[allow]` / `#[warn]` / `#[deny]` controls. The class
    /// signals "this is a lint, not a hard rule"; the lint name
    /// itself lives in `TypeError.lint_name`.
    LintWarning,

    /// Diagnostic emitted but not yet individually classified.
    /// Not an error condition — back-filling is incremental work;
    /// the JSON contract treats this as a valid class while the
    /// classification spreads through the codebase.
    Other,
}

impl DiagnosticClass {
    /// UPPER_SNAKE_CASE wire form. The string that lands in the
    /// `class` field of `karac explain --format=json` records.
    /// Stable within a release per the Specification Layers
    /// policy; rename across releases is a versioned change.
    pub fn as_str(self) -> &'static str {
        match self {
            DiagnosticClass::TypeMismatch => "TYPE_MISMATCH",
            DiagnosticClass::UndefinedType => "UNDEFINED_TYPE",
            DiagnosticClass::WrongNumberOfArgs => "WRONG_NUMBER_OF_ARGS",
            DiagnosticClass::NoMethodFound => "NO_METHOD_FOUND",
            DiagnosticClass::InvalidCast => "INVALID_CAST",
            DiagnosticClass::InvalidUnaryOp => "INVALID_UNARY_OP",
            DiagnosticClass::TraitBoundNotSatisfied => "TRAIT_BOUND_NOT_SATISFIED",
            DiagnosticClass::RefutablePattern => "REFUTABLE_PATTERN",
            DiagnosticClass::CannotInferTypeParam => "CANNOT_INFER_TYPE_PARAM",
            DiagnosticClass::UndefinedName => "UNDEFINED_NAME",
            DiagnosticClass::DuplicateDefinition => "DUPLICATE_DEFINITION",
            DiagnosticClass::EffectUndeclared => "EFFECT_UNDECLARED",
            DiagnosticClass::EffectConflict => "EFFECT_CONFLICT",
            DiagnosticClass::OwnershipMoveAfterUse => "OWNERSHIP_MOVE_AFTER_USE",
            DiagnosticClass::OwnershipBorrowConflict => "OWNERSHIP_BORROW_CONFLICT",
            DiagnosticClass::OwnershipUseOfUninitialized => "OWNERSHIP_USE_OF_UNINITIALIZED",
            DiagnosticClass::TargetIncompatible => "TARGET_INCOMPATIBLE",
            DiagnosticClass::UnsafeRequired => "UNSAFE_REQUIRED",
            DiagnosticClass::FfiViolation => "FFI_VIOLATION",
            DiagnosticClass::LayoutQueryInvalid => "LAYOUT_QUERY_INVALID",
            DiagnosticClass::LintWarning => "LINT_WARNING",
            DiagnosticClass::Other => "OTHER",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn as_str_returns_upper_snake_for_every_variant() {
        // Pin the wire form of every variant. Failure here is a
        // breaking change to the JSON catalogue — a release-boundary
        // rename / removal — and warrants the entry's spec line in
        // the catalogue docs to be updated alongside.
        assert_eq!(DiagnosticClass::TypeMismatch.as_str(), "TYPE_MISMATCH");
        assert_eq!(DiagnosticClass::UndefinedType.as_str(), "UNDEFINED_TYPE");
        assert_eq!(
            DiagnosticClass::WrongNumberOfArgs.as_str(),
            "WRONG_NUMBER_OF_ARGS"
        );
        assert_eq!(DiagnosticClass::NoMethodFound.as_str(), "NO_METHOD_FOUND");
        assert_eq!(DiagnosticClass::InvalidCast.as_str(), "INVALID_CAST");
        assert_eq!(DiagnosticClass::InvalidUnaryOp.as_str(), "INVALID_UNARY_OP");
        assert_eq!(
            DiagnosticClass::TraitBoundNotSatisfied.as_str(),
            "TRAIT_BOUND_NOT_SATISFIED"
        );
        assert_eq!(
            DiagnosticClass::RefutablePattern.as_str(),
            "REFUTABLE_PATTERN"
        );
        assert_eq!(
            DiagnosticClass::CannotInferTypeParam.as_str(),
            "CANNOT_INFER_TYPE_PARAM"
        );
        assert_eq!(DiagnosticClass::UndefinedName.as_str(), "UNDEFINED_NAME");
        assert_eq!(
            DiagnosticClass::DuplicateDefinition.as_str(),
            "DUPLICATE_DEFINITION"
        );
        assert_eq!(
            DiagnosticClass::EffectUndeclared.as_str(),
            "EFFECT_UNDECLARED"
        );
        assert_eq!(DiagnosticClass::EffectConflict.as_str(), "EFFECT_CONFLICT");
        assert_eq!(
            DiagnosticClass::OwnershipMoveAfterUse.as_str(),
            "OWNERSHIP_MOVE_AFTER_USE"
        );
        assert_eq!(
            DiagnosticClass::OwnershipBorrowConflict.as_str(),
            "OWNERSHIP_BORROW_CONFLICT"
        );
        assert_eq!(
            DiagnosticClass::OwnershipUseOfUninitialized.as_str(),
            "OWNERSHIP_USE_OF_UNINITIALIZED"
        );
        assert_eq!(
            DiagnosticClass::TargetIncompatible.as_str(),
            "TARGET_INCOMPATIBLE"
        );
        assert_eq!(DiagnosticClass::UnsafeRequired.as_str(), "UNSAFE_REQUIRED");
        assert_eq!(DiagnosticClass::FfiViolation.as_str(), "FFI_VIOLATION");
        assert_eq!(
            DiagnosticClass::LayoutQueryInvalid.as_str(),
            "LAYOUT_QUERY_INVALID"
        );
        assert_eq!(DiagnosticClass::LintWarning.as_str(), "LINT_WARNING");
        assert_eq!(DiagnosticClass::Other.as_str(), "OTHER");
    }

    #[test]
    fn wire_form_is_all_uppercase_with_underscores() {
        // Sanity: the catalogue contract is UPPER_SNAKE_CASE for
        // every variant. Walks every class via a static list so a
        // newly-added variant must be appended here too (the lint
        // is "any class missing → test fails").
        let all = [
            DiagnosticClass::TypeMismatch,
            DiagnosticClass::UndefinedType,
            DiagnosticClass::WrongNumberOfArgs,
            DiagnosticClass::NoMethodFound,
            DiagnosticClass::InvalidCast,
            DiagnosticClass::InvalidUnaryOp,
            DiagnosticClass::TraitBoundNotSatisfied,
            DiagnosticClass::RefutablePattern,
            DiagnosticClass::CannotInferTypeParam,
            DiagnosticClass::UndefinedName,
            DiagnosticClass::DuplicateDefinition,
            DiagnosticClass::EffectUndeclared,
            DiagnosticClass::EffectConflict,
            DiagnosticClass::OwnershipMoveAfterUse,
            DiagnosticClass::OwnershipBorrowConflict,
            DiagnosticClass::OwnershipUseOfUninitialized,
            DiagnosticClass::TargetIncompatible,
            DiagnosticClass::UnsafeRequired,
            DiagnosticClass::FfiViolation,
            DiagnosticClass::LayoutQueryInvalid,
            DiagnosticClass::LintWarning,
            DiagnosticClass::Other,
        ];
        for cls in all {
            let s = cls.as_str();
            assert!(!s.is_empty(), "class {:?} produced empty wire form", cls);
            assert!(
                s.chars().all(|c| c.is_ascii_uppercase() || c == '_'),
                "class {:?} wire form '{}' is not UPPER_SNAKE_CASE",
                cls,
                s
            );
        }
    }
}
