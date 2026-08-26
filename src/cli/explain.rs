//! Static concept-level explainer pages surfaced by `karac explain`.
//!
//! Each concept page is a `&'static str` rendered verbatim in text
//! mode. The text shape is frozen by tests in `tests/cli.rs` —
//! diagnostic-redirect wording and cross-references must stay aligned
//! with the implementation surface they describe (the ownership
//! checker, `karac query ownership`, and the design.md sections the
//! page cites).
//!
//! Line 619 slice 3 widens the surface from concept-only to
//! concept-or-class lookup and adds `--format=json` for the
//! machine-consumable shape that LLM agents and IDE tooling need.

use crate::cli::{ExplainFormat, ExplainTarget};
use crate::diagnostic_class::DiagnosticClass;
use std::process;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ExplainConcept {
    Closures,
    Operators,
    ModuleState,
    StableHash,
}

impl ExplainConcept {
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "closures" => Some(ExplainConcept::Closures),
            "operators" => Some(ExplainConcept::Operators),
            "module-state" => Some(ExplainConcept::ModuleState),
            "stable-hash" => Some(ExplainConcept::StableHash),
            _ => None,
        }
    }

    pub fn page(self) -> &'static str {
        match self {
            ExplainConcept::Closures => CLOSURES_PAGE,
            ExplainConcept::Operators => OPERATORS_PAGE,
            ExplainConcept::ModuleState => MODULE_STATE_PAGE,
            ExplainConcept::StableHash => STABLE_HASH_PAGE,
        }
    }

    /// Wire-form name for the JSON envelope.
    pub fn as_str(self) -> &'static str {
        match self {
            ExplainConcept::Closures => "closures",
            ExplainConcept::Operators => "operators",
            ExplainConcept::ModuleState => "module-state",
            ExplainConcept::StableHash => "stable-hash",
        }
    }
}

/// Render the requested target in the requested format. Exits the
/// process non-zero if the lookup name is unknown, with a focused
/// hint listing the supported set.
pub fn render(target: &ExplainTarget, format: ExplainFormat) {
    match target {
        ExplainTarget::Concept(name) => render_concept(name, format),
        ExplainTarget::Class(name) => render_class(name, format),
        ExplainTarget::Code(name) => render_code(name, format),
    }
}

/// Like [`render`] but returns the rendered text (or a structured
/// error string) instead of printing + `process::exit`-ing. Used by
/// non-CLI consumers — currently `Session::dispatch_magic` so the
/// Jupyter `%explain` magic surface can route the lookup result into
/// the kernel's `display_data` channel without going through stdout.
pub fn render_to_string(target: &ExplainTarget, format: ExplainFormat) -> Result<String, String> {
    match target {
        ExplainTarget::Concept(name) => {
            let Some(concept) = ExplainConcept::parse(name) else {
                return Err(format!(
                    "unknown concept '{name}'. Supported: {}.",
                    concept_list(),
                ));
            };
            Ok(match format {
                ExplainFormat::Text => concept.page().to_string(),
                ExplainFormat::Json => render_concept_json(concept),
            })
        }
        ExplainTarget::Class(name) => {
            let Some(class) = parse_class_name(name) else {
                return Err(format!(
                    "unknown diagnostic class '{name}'. Supported: {}.",
                    class_list(),
                ));
            };
            Ok(match format {
                ExplainFormat::Text => render_class_text(class),
                ExplainFormat::Json => render_class_json(class),
            })
        }
        ExplainTarget::Code(name) => {
            let entries = lookup_code(name);
            if entries.is_empty() {
                return Err(unknown_code_message(name));
            }
            Ok(match format {
                ExplainFormat::Text => render_code_text(name, entries),
                ExplainFormat::Json => render_code_json(name, entries),
            })
        }
    }
}

fn render_concept(name: &str, format: ExplainFormat) {
    let Some(concept) = ExplainConcept::parse(name) else {
        eprintln!(
            "error: unknown concept '{name}'. Supported: {}.",
            concept_list(),
        );
        process::exit(1);
    };
    match format {
        ExplainFormat::Text => println!("{}", concept.page()),
        ExplainFormat::Json => println!("{}", render_concept_json(concept)),
    }
}

fn render_class(name: &str, format: ExplainFormat) {
    let Some(class) = parse_class_name(name) else {
        eprintln!(
            "error: unknown diagnostic class '{name}'. Supported: {}.",
            class_list(),
        );
        process::exit(1);
    };
    match format {
        ExplainFormat::Text => println!("{}", render_class_text(class)),
        ExplainFormat::Json => println!("{}", render_class_json(class)),
    }
}

fn render_code(name: &str, format: ExplainFormat) {
    let entries = lookup_code(name);
    if entries.is_empty() {
        eprintln!("error: {}", unknown_code_message(name));
        process::exit(1);
    }
    match format {
        ExplainFormat::Text => println!("{}", render_code_text(name, entries)),
        ExplainFormat::Json => println!("{}", render_code_json(name, entries)),
    }
}

/// One row of the diagnostic-code catalogue.
///
/// `kind` is the compiler's own error-kind variant name
/// (`TypeErrorKind::TypeMismatch` → `"TypeMismatch"`), not a
/// human-authored title. That keeps the table factual: it restates the
/// mapping that `collect_diagnostics` already applies rather than
/// inventing a parallel taxonomy that could drift into fiction.
///
/// `class` mirrors `class_for_type_error_kind` and is `None` for the
/// kinds that function leaves unclassified — those emit `"OTHER"` in
/// the JSON `class` field, and `explain` says so rather than printing
/// the vacuous `OTHER` page.
#[derive(Debug, Clone, Copy, PartialEq)]
struct CodeEntry {
    phase: &'static str,
    kind: &'static str,
    class: Option<DiagnosticClass>,
}

/// Diagnostic-code catalogue: `code → (phase, kind, class)`.
///
/// **Scope is by BAND, not by phase.** The table covers `E01xx`,
/// `E02xx` / `W02xx`, `E08xx`, and the two loose numbers `E0223` /
/// `E0227` — COMPLETELY, every code any emitter mints in them. The
/// effect (`E04xx`), ownership (`E05xx`) and provider-escape (`E0600`)
/// bands are not catalogued yet; `explain` reports those as
/// uncatalogued rather than guessing.
///
/// The by-band framing is the B-2026-08-20-30 correction. The scope
/// used to be stated by PHASE — "the typecheck and resolve families" —
/// which reads as a promise about numbers, because a number is all a
/// user has when they reach for `karac explain`. The two disagree
/// exactly where a phase mints out of its own band, and several do:
/// `E0230` / `E0231` are effect-checker codes in the typechecker's
/// band, `E0802` is an effect-checker code in the shared target/GPU
/// band, and `E0223` / `E0227` come from phases with no band at all.
/// Each was a code a user could see and `explain` would refuse while
/// its own error message claimed the family was covered.
///
/// COMPLETENESS is the property that matters, not coverage in the
/// abstract: an absent row makes a collision invisible to
/// `code_table_has_no_cross_phase_collisions`, which is exactly how
/// four of the eight B-2026-07-27-14 collisions went unnoticed while
/// the guard reported only four.
/// `code_table_catalogues_every_code_in_a_covered_band` holds the
/// whole covered range at zero gaps, so the next stray fails a test
/// instead of reaching a user.
///
/// **Source of truth.** Every row restates a `match err.kind` arm in
/// `collect_diagnostics` (`src/cli.rs`) crossed with
/// `class_for_type_error_kind` (`src/typechecker.rs`). Those two
/// functions are exhaustive matches, so adding an error kind is a
/// compile error there — but *not* here. The
/// `code_table_class_matches_typechecker` test pins the rows that
/// would otherwise drift silently.
///
/// **One phase per band, mostly.** Each phase allocates from its own
/// numeric band — resolve `E01xx`, typecheck `E02xx`/`W02xx`, effect
/// `E04xx`, ownership `E05xx` — so a code identifies exactly one
/// diagnostic and the `code` field of a structured diagnostic is a
/// usable key on its own. `E08xx` is deliberately shared across phases
/// for target/GPU placement errors, but the numbers within it stay
/// disjoint. Five rows sit outside their minting phase's band and stay
/// there deliberately (see the sections for them below): renumbering a
/// shipping code breaks every consumer keying on it, and the band's
/// one real benefit — no two phases on one number — is delivered by
/// the catalogue row plus the collision guard, not by the number.
///
/// It was not always so: the resolver used to mint thirteen codes into
/// the typechecker's `E02xx` band, eight of which landed on numbers the
/// typechecker had already taken (B-2026-07-27-14). `lookup_code` still
/// returns a `Vec` and the renderer still handles multiple rows —
/// that machinery is what surfaced the collisions in the first place,
/// and `code_table_has_no_cross_phase_collisions` now holds it at zero.
const CODE_TABLE: &[(&str, CodeEntry)] = &[
    // ── resolve ─────────────────────────────────────────────────
    (
        "W0150",
        res("LayoutUnassignedFields", Some(DiagnosticClass::LintWarning)),
    ),
    // B-2026-08-21-2 — the `#[repr(C)]` + layout rule, split by visibility:
    // the private half is the `repr_c_layout_ignored` lint, the `pub` half is
    // the hard FFI-contract error the spec mandates, which takes no
    // suppression.
    (
        "W0151",
        res("LayoutReprCIgnored", Some(DiagnosticClass::LintWarning)),
    ),
    // Class `None`, not `LintWarning`: this is the ERROR half. Classing it as
    // a lint would advertise `LINT_WARNING` for a diagnostic that takes no
    // suppression and is fatal — the same false promise the row it comes from
    // is about.
    ("E0151", res("LayoutReprCOnPubStruct", None)),
    (
        "W0152",
        res("FloatInSerializedType", Some(DiagnosticClass::LintWarning)),
    ),
    (
        "E0100",
        res("UndefinedName", Some(DiagnosticClass::UndefinedName)),
    ),
    (
        "E0101",
        res(
            "DuplicateDefinition",
            Some(DiagnosticClass::DuplicateDefinition),
        ),
    ),
    ("E0102", res("ReservedIdentifier", None)),
    ("E0103", res("PrivateAccess", None)),
    (
        "E0104",
        res("UndefinedType", Some(DiagnosticClass::UndefinedType)),
    ),
    ("E0105", res("UndefinedVariant", None)),
    ("E0106", res("UndefinedField", None)),
    ("E0107", res("UndefinedLabel", None)),
    ("E0108", res("OperatorTraitImplRestricted", None)),
    ("E0109", res("IntoTraitImplNotAllowed", None)),
    ("E0110", res("ImplLevelEffectVarNotAllowed", None)),
    ("E0111", res("PrivateItemAccess", None)),
    ("E0112", res("UnknownModule", None)),
    ("E0113", res("UnknownItemInModule", None)),
    ("E0114", res("ReservedEffectResource", None)),
    ("E0115", res("CompilerBuiltinReserved", None)),
    ("E0116", res("ContinueOnBlockLabel", None)),
    ("E0117", res("NonExhaustiveInvalidTarget", None)),
    ("E0118", res("TrackCallerInvalidTarget", None)),
    ("E0119", res("DeprecatedOnImpl", None)),
    ("E0120", res("DeprecatedOnField", None)),
    ("E0121", res("UnknownAttribute", None)),
    ("E0122", res("ProfileInvalidTarget", None)),
    ("E0123", res("UnknownProfile", None)),
    ("E0124", res("AmbiguousWildcardImport", None)),
    // `E08xx` is the shared target/GPU-placement band — the resolver
    // owns E0800, the typechecker E0801. Shared band, disjoint numbers.
    ("E0800", res("GpuInvalidTarget", None)),
    // ── typecheck ───────────────────────────────────────────────
    (
        "E0200",
        ty("TypeMismatch", Some(DiagnosticClass::TypeMismatch)),
    ),
    (
        "E0201",
        ty("UndefinedField", Some(DiagnosticClass::TypeMismatch)),
    ),
    (
        "E0202",
        ty(
            "WrongNumberOfArgs",
            Some(DiagnosticClass::WrongNumberOfArgs),
        ),
    ),
    (
        "E0203",
        ty("MissingField", Some(DiagnosticClass::TypeMismatch)),
    ),
    (
        "E0204",
        ty("ExtraField", Some(DiagnosticClass::TypeMismatch)),
    ),
    ("E0205", ty("NonExhaustiveMatch", None)),
    (
        "E0206",
        ty("NotCallable", Some(DiagnosticClass::TypeMismatch)),
    ),
    (
        "E0207",
        ty("NotAStruct", Some(DiagnosticClass::TypeMismatch)),
    ),
    (
        "E0208",
        ty("InvalidBinaryOp", Some(DiagnosticClass::InvalidUnaryOp)),
    ),
    (
        "E0209",
        ty("InvalidUnaryOp", Some(DiagnosticClass::InvalidUnaryOp)),
    ),
    (
        "E0210",
        ty("InvalidCast", Some(DiagnosticClass::InvalidCast)),
    ),
    (
        "E0211",
        ty("ConditionNotBool", Some(DiagnosticClass::TypeMismatch)),
    ),
    (
        "E0212",
        ty("BranchTypeMismatch", Some(DiagnosticClass::TypeMismatch)),
    ),
    (
        "E0213",
        ty("ReturnTypeMismatch", Some(DiagnosticClass::TypeMismatch)),
    ),
    (
        "E0214",
        ty("InvalidTupleIndex", Some(DiagnosticClass::TypeMismatch)),
    ),
    (
        "E0215",
        ty("LabelMismatch", Some(DiagnosticClass::TypeMismatch)),
    ),
    (
        "E0216",
        ty("NonContiguousLabels", Some(DiagnosticClass::TypeMismatch)),
    ),
    (
        "E0217",
        ty(
            "InvalidPipePlaceholder",
            Some(DiagnosticClass::InvalidUnaryOp),
        ),
    ),
    (
        "E0218",
        ty(
            "MissingMutMarker",
            Some(DiagnosticClass::OwnershipBorrowConflict),
        ),
    ),
    (
        "E0219",
        ty(
            "InvalidMutMarker",
            Some(DiagnosticClass::OwnershipBorrowConflict),
        ),
    ),
    ("E0220", ty("UnsupportedNumericSuffix", None)),
    ("E0221", ty("PrivateTypeInPublicSignature", None)),
    (
        "E0222",
        ty("RefutablePattern", Some(DiagnosticClass::RefutablePattern)),
    ),
    (
        "E0229",
        ty(
            "MissingSupertrait",
            Some(DiagnosticClass::TraitBoundNotSatisfied),
        ),
    ),
    (
        "E0232",
        ty(
            "TraitBoundNotSatisfied",
            Some(DiagnosticClass::TraitBoundNotSatisfied),
        ),
    ),
    (
        "E0233",
        ty("AmbiguousAssocFn", Some(DiagnosticClass::NoMethodFound)),
    ),
    (
        "E0234",
        ty("CannotInferAssocFn", Some(DiagnosticClass::NoMethodFound)),
    ),
    (
        "E0235",
        ty("OnceFnIntoFnSlot", Some(DiagnosticClass::TypeMismatch)),
    ),
    (
        "E0236",
        ty("NoMethodFound", Some(DiagnosticClass::NoMethodFound)),
    ),
    (
        "W0237",
        ty("UnreachableArm", Some(DiagnosticClass::LintWarning)),
    ),
    (
        "W0238",
        ty(
            "RefinementDomainTooWide",
            Some(DiagnosticClass::LintWarning),
        ),
    ),
    (
        "E0238",
        ty(
            "CannotInferTypeParam",
            Some(DiagnosticClass::CannotInferTypeParam),
        ),
    ),
    (
        "E0239",
        ty("AmbiguousMethod", Some(DiagnosticClass::NoMethodFound)),
    ),
    ("E0240", ty("ConflictingImpl", None)),
    ("E0241", ty("NonExhaustiveCrossPackageLiteral", None)),
    ("E0242", ty("NonExhaustiveCrossPackageMatch", None)),
    ("E0243", ty("NonExhaustiveCrossPackagePattern", None)),
    (
        "W0244",
        ty("UnknownLint", Some(DiagnosticClass::LintWarning)),
    ),
    (
        "E0245",
        ty("Deprecated", Some(DiagnosticClass::LintWarning)),
    ),
    (
        "W0245",
        ty("Deprecated", Some(DiagnosticClass::LintWarning)),
    ),
    ("W0246", ty("MissingNonExhaustive", None)),
    (
        "E0247",
        ty("ForbiddenLintAllow", Some(DiagnosticClass::LintWarning)),
    ),
    (
        "E0248",
        ty("ExpectOnUnfulfilled", Some(DiagnosticClass::LintWarning)),
    ),
    (
        "E0249",
        ty(
            "UnfulfilledLintExpectation",
            Some(DiagnosticClass::LintWarning),
        ),
    ),
    (
        "W0249",
        ty(
            "UnfulfilledLintExpectation",
            Some(DiagnosticClass::LintWarning),
        ),
    ),
    ("E0250", ty("ModuleBindingEffectfulInit", None)),
    ("E0251", ty("ModuleBindingHeapType", None)),
    ("E0252", ty("ReassignToImmutableModuleBinding", None)),
    ("E0253", ty("ScopeLocalEscape", None)),
    ("E0254", ty("CrossTaskUnsafeCapture", None)),
    (
        "E0255",
        ty("UnstableApi", Some(DiagnosticClass::LintWarning)),
    ),
    (
        "W0255",
        ty("UnstableApi", Some(DiagnosticClass::LintWarning)),
    ),
    ("E0256", ty("InvalidRefinementPredicate", None)),
    ("E0257", ty("ParFieldNotConcurrent", None)),
    ("E0258", ty("ParMutSelfReceiver", None)),
    ("E0260", ty("LockTargetNotMutex", None)),
    (
        "E0261",
        ty(
            "AtBindingDoubleConsume",
            Some(DiagnosticClass::OwnershipBorrowConflict),
        ),
    ),
    (
        "E0262",
        ty(
            "TypeAliasBoundNotSatisfied",
            Some(DiagnosticClass::TraitBoundNotSatisfied),
        ),
    ),
    ("E0263", ty("RangePatternBoundNotConst", None)),
    ("E0264", ty("PanickingAllocRejected", None)),
    ("E0265", ty("DeriveCloneAllocates", None)),
    ("E0266", ty("MainReturnType", None)),
    ("E0267", ty("MainErrNotDisplay", None)),
    (
        "E0268",
        ty("StringNotIndexable", Some(DiagnosticClass::TypeMismatch)),
    ),
    (
        "E0274",
        ty("IteratorNotIndexable", Some(DiagnosticClass::TypeMismatch)),
    ),
    (
        "E0275",
        ty("TypeNotIndexable", Some(DiagnosticClass::TypeMismatch)),
    ),
    (
        "E0276",
        ty("NilCoalesceNotWrapped", Some(DiagnosticClass::TypeMismatch)),
    ),
    (
        "E0277",
        ty(
            "OptionalChainNotOption",
            Some(DiagnosticClass::TypeMismatch),
        ),
    ),
    ("E0269", ty("SharedFieldNotMut", None)),
    (
        "E0270",
        ty(
            "AtomicMissingOrdering",
            Some(DiagnosticClass::WrongNumberOfArgs),
        ),
    ),
    (
        "E0271",
        ty(
            "ImplTraitMultipleWitnesses",
            Some(DiagnosticClass::TypeMismatch),
        ),
    ),
    (
        "E0272",
        ty(
            "AtomicInvalidInnerType",
            Some(DiagnosticClass::TypeMismatch),
        ),
    ),
    (
        "E0273",
        ty(
            "PatternScrutineeMismatch",
            Some(DiagnosticClass::TypeMismatch),
        ),
    ),
    ("E0279", ty("AmbiguousBareVariant", None)),
    (
        "W0280",
        ty("RedundantSuffix", Some(DiagnosticClass::LintWarning)),
    ),
    (
        "E0280",
        ty("RedundantSuffix", Some(DiagnosticClass::LintWarning)),
    ),
    (
        "W0281",
        ty("ModuleMutBinding", Some(DiagnosticClass::LintWarning)),
    ),
    (
        "E0281",
        ty("ModuleMutBinding", Some(DiagnosticClass::LintWarning)),
    ),
    // The catch-all arm of the warning emitter: any `TypeErrorKind` that
    // reaches the warning loop without a code of its own lands here, so this
    // row names the bucket rather than one variant. A warning that deserves
    // its own number should get one — `W0299` showing up in a report is the
    // signal that it has not.
    ("W0299", ty("<unclassified typecheck warning>", None)),
    ("E0801", ty("GpuNotSafe", None)),
    ("E0803", ty("ReprTransparentInvalid", None)),
    ("E0804", ty("DiscriminantInvalid", None)),
    ("E0805", ty("ExternSignatureInvalid", None)),
    // ── effect, minted outside the effect band ──────────────────
    //
    // These three are effect-checker diagnostics that do not carry an `E04xx`
    // number. `E0230` / `E0231` are the last of CR-24's allocations into the
    // typechecker's band; `E0802` is deliberate — `E08xx` is the shared
    // target/GPU placement band, and a GPU effect violation belongs there.
    // They are catalogued under the phase that MINTS them, which is what makes
    // `code_table_has_no_cross_phase_collisions` able to see them: an
    // uncatalogued stray is exactly the shape that hid four of the eight
    // B-2026-07-27-14 collisions.
    ("E0230", eff("ImplExceedsTraitCeiling", None)),
    ("E0231", eff("TraitDefaultExceedsCeiling", None)),
    (
        "E0802",
        eff(
            "GpuEffectViolation",
            Some(DiagnosticClass::TargetIncompatible),
        ),
    ),
    // ── module graph / manifest ─────────────────────────────────
    //
    // The two codes minted outside the diagnostic emitter entirely: `E0223` by
    // `print_cycles_text` (`src/cli/build_cmds.rs`) and `E0227` by
    // `ManifestError::code` (`src/manifest.rs`). Both SHIP — a module cycle
    // and a `karac check` run outside a package are ordinary user-facing
    // failures — and both went uncatalogued, so `karac explain` refused two
    // codes it was telling users it covered (B-2026-08-20-30).
    //
    // Their numbers stay where they are. Renumbering into a band that matches
    // the minting phase would mean inventing two new bands for two codes AND
    // breaking two shipping `code` values, and the band buys exactly one thing
    // — collision protection — which the catalogue row and the guards below
    // now provide directly.
    ("E0223", mg("CircularModuleDependency")),
    ("E0227", mf("NotInsideKaraProject")),
    // ── parse ───────────────────────────────────────────────────
    //
    // The whole band: `ParseErrorKind::code` mints exactly these four.
    ("E0001", parse("Syntax")),
    ("E0002", parse("UnexpectedToken")),
    ("E0003", parse("ReservedKeyword")),
    ("E0005", parse("ReservedSyntax")),
    // ── effect ──────────────────────────────────────────────────
    //
    // `L0001` is a NOTE, not an error, and its `L` prefix puts it outside the
    // `E`/`W` shape `unknown_code_message` recognises — so before it was
    // catalogued, `karac explain L0001` did not even say "uncatalogued", it
    // said the argument was not a diagnostic code at all.
    (
        "E0400",
        eff(
            "MissingEffectDeclaration",
            Some(DiagnosticClass::EffectUndeclared),
        ),
    ),
    ("E0401", eff("OverDeclaredEffect", None)),
    ("E0402", eff("CircularEffectGroup", None)),
    ("E0403", eff("UndefinedEffectGroup", None)),
    ("E0404", eff("EffectSubtypeViolation", None)),
    ("E0405", eff("ProfileViolation", None)),
    ("E0406", eff("EffectVariableConflict", None)),
    ("E0407", eff("ProfileIncompatibleEffect", None)),
    (
        "E0408",
        eff(
            "ModuleBindingWriteInPar",
            Some(DiagnosticClass::EffectConflict),
        ),
    ),
    ("E0409", eff("PubFnSyntheticResource", None)),
    ("E0410", eff("ForbiddenEffectInContract", None)),
    (
        "E0411",
        eff(
            "TargetGateViolation",
            Some(DiagnosticClass::TargetIncompatible),
        ),
    ),
    ("E0412", eff("ResourceReceiverContradiction", None)),
    (
        "E0413",
        eff(
            "ExternCUnwindRequiresPanics",
            Some(DiagnosticClass::FfiViolation),
        ),
    ),
    (
        "E0414",
        eff(
            "ExternExportSuspendsUnsupported",
            Some(DiagnosticClass::FfiViolation),
        ),
    ),
    (
        "E0415",
        eff(
            "ExternCUnwindRequiresUnwindProfile",
            Some(DiagnosticClass::FfiViolation),
        ),
    ),
    ("E0416", eff("NoEffectViolated", None)),
    (
        "L0001",
        eff("FfiLintHint", Some(DiagnosticClass::LintWarning)),
    ),
    (
        "L0002",
        eff("MutualRecursionNote", Some(DiagnosticClass::LintWarning)),
    ),
    (
        "L0003",
        eff("PureLoopInPar", Some(DiagnosticClass::LintWarning)),
    ),
    // ── ownership ───────────────────────────────────────────────
    //
    // `N0503` / `N0507` are notes on the same `OwnershipErrorKind` enum as the
    // errors around them, which is why they sit in the ownership band with an
    // `N` prefix rather than in a band of their own.
    //
    // Nine more ownership kinds — the borrow-conflict matrix, the RC budget,
    // the two concurrent-struct rules and the temporary-slice escape — are
    // emitted under SYMBOLIC `E_*` codes and are deliberately outside every
    // numeric band; `explain` still reaches them by class.
    (
        "E0500",
        own("UseAfterMove", Some(DiagnosticClass::OwnershipMoveAfterUse)),
    ),
    ("E0501", own("OwnershipCycle", None)),
    ("E0502", own("NoRcViolation", None)),
    ("N0503", own("RcFallbackNote", None)),
    ("E0504", own("CaptureModeViolation", None)),
    (
        "E0505",
        own(
            "UseOfUninitialized",
            Some(DiagnosticClass::OwnershipUseOfUninitialized),
        ),
    ),
    ("E0506", own("ReassignToImmutable", None)),
    ("N0507", own("UnusedMutCaptureNote", None)),
    ("E0508", own("RefCaptureEscapesScope", None)),
    ("E0509", own("BorrowReturnNotSourcePinned", None)),
    ("E0510", own("MutateImmutableBinding", None)),
    ("E0511", own("FrozenParamEscapes", None)),
    ("E0512", own("FrozenTypeNotFreezable", None)),
    // ── provider escape ─────────────────────────────────────────
    ("E0600", prov("ProviderEscape")),
    // ── lint (non-`TypeErrorKind`) ──────────────────────────────
    //
    // Both halves of each pair are catalogued: the W code is what a default
    // build emits and the E code is what `-D <lint>` escalates to, and it was
    // the ESCALATED code that sent someone to `karac explain` in the first
    // place (B-2026-08-18-17).
    (
        "W0259",
        lint("undocumented_unsafe | unsafe_op_in_unsafe_fn | ffi_float_eq"),
    ),
    (
        "E0259",
        lint("undocumented_unsafe | unsafe_op_in_unsafe_fn | ffi_float_eq"),
    ),
    ("W0278", lint("must_use")),
    ("E0278", lint("must_use")),
];

const fn ty(kind: &'static str, class: Option<DiagnosticClass>) -> CodeEntry {
    CodeEntry {
        phase: "typecheck",
        kind,
        class,
    }
}

/// A row for a lint that is NOT a `TypeErrorKind` (B-2026-08-18-28).
///
/// `ty` / `res` cannot express these: their `kind` restates a variant of an
/// exhaustive compiler enum, and `must_use` and the `lint_entries_for_compile_path`
/// family live outside both. That is exactly why they were absent from the
/// catalogue while the lints registered AS `TypeErrorKind`s (`Deprecated`
/// E0245, `UnstableApi` E0255) were present — the table had no row shape for
/// them, so nobody noticed the omission.
///
/// `phase: "lint"` matches the `phase` these diagnostics carry in the JSON
/// feed, so the catalogue and the emitted record agree on the field an agent
/// loop reads.
///
/// `kind` names the LINT(S) the code covers rather than a single enum variant,
/// because a lint code may be shared: `lint_name` is the discriminator (it is
/// what `-A` / `-D` address), while the code identifies the family. See
/// B-2026-08-18-17's note on whether that sharing should persist.
const fn lint(kind: &'static str) -> CodeEntry {
    CodeEntry {
        phase: "lint",
        kind,
        class: Some(DiagnosticClass::LintWarning),
    }
}

/// An effect-checker row. Effect diagnostics carry `class: None` in the JSON
/// feed (`diag_json.rs` sets it literally), so the catalogue does too — the
/// `EFFECT_UNDECLARED` / `EFFECT_CONFLICT` classes exist in
/// [`DiagnosticClass`] but nothing applies them yet, and a row that claimed
/// otherwise would disagree with the record an agent actually reads.
const fn eff(kind: &'static str, class: Option<DiagnosticClass>) -> CodeEntry {
    CodeEntry {
        phase: "effect",
        kind,
        class,
    }
}

/// An ownership row. Same rule as [`eff`]: `class` restates
/// `class_for_ownership_error_kind`, which is `None` for most kinds.
const fn own(kind: &'static str, class: Option<DiagnosticClass>) -> CodeEntry {
    CodeEntry {
        phase: "ownership",
        kind,
        class,
    }
}

/// A parse row. The parser owns `E00xx`, below the resolver's `E01xx`, and
/// mints exactly four codes from `ParseErrorKind::code`. None is classified —
/// `class` is a semantic taxonomy and a syntax error has no semantics yet.
const fn parse(kind: &'static str) -> CodeEntry {
    CodeEntry {
        phase: "parse",
        kind,
        class: None,
    }
}

/// The provider-escape phase, which mints exactly one code: `E0600`.
/// `phase: "provider_escape"` matches the emitted record.
const fn prov(kind: &'static str) -> CodeEntry {
    CodeEntry {
        phase: "provider_escape",
        kind,
        class: None,
    }
}

/// The module-graph phase, which mints exactly one code: `E0223`, from
/// `print_cycles_text`. `phase: "module_graph"` matches the `phase` field the
/// JSON emitter writes alongside it.
const fn mg(kind: &'static str) -> CodeEntry {
    CodeEntry {
        phase: "module_graph",
        kind,
        class: None,
    }
}

/// The manifest phase, which mints exactly one code: `E0227`, from
/// `ManifestError::code`. `phase: "manifest"` matches the emitted record.
const fn mf(kind: &'static str) -> CodeEntry {
    CodeEntry {
        phase: "manifest",
        kind,
        class: None,
    }
}

const fn res(kind: &'static str, class: Option<DiagnosticClass>) -> CodeEntry {
    CodeEntry {
        phase: "resolve",
        kind,
        class,
    }
}

/// All catalogue rows for `code`, in table order. Returns more than
/// one row only for the cross-phase collisions documented on
/// [`CODE_TABLE`].
fn lookup_code(code: &str) -> Vec<CodeEntry> {
    CODE_TABLE
        .iter()
        .filter(|(c, _)| *c == code)
        .map(|(_, e)| *e)
        .collect()
}

fn unknown_code_message(code: &str) -> String {
    // Same shape rule as `classify_explain_name` — any uppercase letter then
    // digits, so the `N05xx` notes and `L0001` are recognised as codes rather
    // than reported as non-codes.
    let looks_like_a_code = {
        let mut chars = code.chars();
        matches!(chars.next(), Some(c) if c.is_ascii_uppercase())
            && code.len() > 1
            && chars.all(|c| c.is_ascii_digit())
    };
    if looks_like_a_code {
        format!(
            "'{code}' is not a diagnostic code this compiler mints. Every \
             NUMBERED code does have an entry — parse E00xx, resolve E01xx, \
             typecheck E02xx / W02xx, effect E04xx, ownership E05xx / N05xx, \
             provider-escape E0600, target/GPU E08xx, and the loose E0223 / \
             E0227 — so a code in that shape which lands here is either a \
             typo or from a different compiler version. Some diagnostics \
             carry a SYMBOLIC code instead (`E_SLICE_BORROW_CONFLICT` and \
             the rest of the borrow-conflict family); look those up by class \
             — `karac check --output=json` reports one in each record's \
             `class` field, and `karac explain --class=NAME` explains it. \
             Supported classes: {}.",
            class_list(),
        )
    } else {
        format!(
            "'{code}' is not a diagnostic code (expected the `E0200` / `W0244` \
             form that `karac check --output=json` reports in each record's \
             `code` field)."
        )
    }
}

fn render_code_text(code: &str, entries: Vec<CodeEntry>) -> String {
    let mut out = format!("karac explain — diagnostic code: {code}\n");
    if entries.len() > 1 {
        out.push_str(
            "\nNOTE: this code is currently minted by more than one phase for \
             unrelated errors. Use the `phase` field of the diagnostic to tell \
             which one you hit.\n",
        );
    }
    for entry in &entries {
        out.push_str(&format!(
            "\nphase: {}\nkind:  {}\n",
            entry.phase, entry.kind
        ));
        match entry.class {
            Some(class) => out.push_str(&format!(
                "class: {}\n\n{}\n",
                class.as_str(),
                class_description(class)
            )),
            None => out.push_str(
                "class: OTHER — this diagnostic has no class page yet. Its JSON \
                 record reports `\"class\": \"OTHER\"`; the message text is the \
                 authoritative description.\n",
            ),
        }
    }
    out
}

fn render_code_json(code: &str, entries: Vec<CodeEntry>) -> String {
    let rows = entries
        .iter()
        .map(|e| {
            let class = e.class.map(|c| c.as_str()).unwrap_or("OTHER");
            let description = e
                .class
                .map(class_description)
                .unwrap_or("No class page yet; the diagnostic message is authoritative.");
            format!(
                "{{\"phase\":\"{}\",\"kind\":\"{}\",\"class\":\"{}\",\"description\":\"{}\"}}",
                e.phase,
                e.kind,
                class,
                escape_json_string(description),
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!("{{\"kind\":\"diagnostic_code\",\"code\":\"{code}\",\"entries\":[{rows}]}}")
}

/// Every concept name `--concept=` accepts, rendered for the
/// unknown-name hint.
///
/// Derived from [`ALL_CONCEPTS`] rather than hand-listed, so a new page
/// cannot be added to the enum and forgotten here — which is precisely
/// how the hint would start lying about what is available.
fn concept_list() -> String {
    ALL_CONCEPTS
        .iter()
        .map(|c| c.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// The concept pages, in the order the unknown-name hint lists them.
const ALL_CONCEPTS: &[ExplainConcept] = &[
    ExplainConcept::Closures,
    ExplainConcept::Operators,
    ExplainConcept::ModuleState,
    ExplainConcept::StableHash,
];

fn parse_class_name(name: &str) -> Option<DiagnosticClass> {
    all_classes()
        .iter()
        .find(|&&cls| cls.as_str() == name)
        .copied()
}

fn all_classes() -> &'static [DiagnosticClass] {
    &[
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
    ]
}

fn class_list() -> String {
    all_classes()
        .iter()
        .map(|c| c.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

fn render_class_text(class: DiagnosticClass) -> String {
    format!(
        "karac explain — diagnostic class: {}\n\n{}\n",
        class.as_str(),
        class_description(class)
    )
}

/// Render a diagnostic-class entry as a JSON object. The envelope
/// is deliberately small at slice 3 — future slices (4: typed
/// expected/got, 5: fixes) extend the shape; consumers should treat
/// unknown keys as forward-compatibility growth, not breakage. The
/// `class` field's value is the same UPPER_SNAKE wire form returned
/// by `DiagnosticClass::as_str()` and embedded in build-time
/// diagnostic records.
fn render_class_json(class: DiagnosticClass) -> String {
    // Hand-rolled JSON keeps the dep surface small and matches the
    // existing emitter style in `src/cli.rs` (no serde wrapper for
    // this surface). String escapes: `\` and `"` only — the
    // descriptions are ASCII without control chars.
    let class_str = class.as_str();
    let description = class_description(class);
    let description_escaped = escape_json_string(description);
    format!(
        "{{\"kind\":\"diagnostic_class\",\"class\":\"{}\",\"description\":\"{}\"}}",
        class_str, description_escaped
    )
}

fn render_concept_json(concept: ExplainConcept) -> String {
    // The concept body is multi-line static text — escape for JSON
    // embedding so consumers receive a single JSON record.
    let body_escaped = escape_json_string(concept.page());
    format!(
        "{{\"kind\":\"concept\",\"concept\":\"{}\",\"body\":\"{}\"}}",
        concept.as_str(),
        body_escaped
    )
}

fn escape_json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

/// Prose description for each diagnostic class. Single source of
/// truth for both text and JSON output. Keep concise — these are
/// catalogue entries, not concept pages.
fn class_description(class: DiagnosticClass) -> &'static str {
    match class {
        DiagnosticClass::TypeMismatch => {
            "Operand, argument, or return-value type doesn't match the expected slot. \
             Covers assignment type mismatch, branch-arm mismatch, return-type mismatch, \
             missing or extra struct fields, non-callable invocation targets, and the \
             once-fn-into-fn-slot narrowing failure."
        }
        DiagnosticClass::UndefinedType => {
            "A name in type position doesn't resolve to a known type. Triggered by \
             unknown type names in annotations, generic arguments, parameter types, and \
             return types."
        }
        DiagnosticClass::WrongNumberOfArgs => {
            "A call site supplies the wrong number of positional, labeled, or variadic \
             arguments for the callee's signature."
        }
        DiagnosticClass::NoMethodFound => {
            "Method-call dispatch found no matching impl for the receiver type. May \
             carry a `did you mean 'X'?` suggestion when an edit-distance-≤2 candidate \
             exists. Also covers ambiguous-method and cannot-infer-assoc-fn failures."
        }
        DiagnosticClass::InvalidCast => {
            "An `as` cast was rejected. Covers strict-provenance ptr↔int casts \
             (`ptr.addr` / `ptr.with_addr` instead), char→narrow-int (use `as u32 as \
             T`), int→char (use `char.try_from`), int→bool / float→bool (use explicit \
             predicates), and reference→raw-pointer (use `ptr.const` / `ptr.mut`)."
        }
        DiagnosticClass::InvalidUnaryOp => {
            "A unary or pipe-style operator was applied to an unsupported operand. \
             Includes raw-pointer place-form rejections (`ptr.const(non-place)`), \
             deref of non-pointer values, and pipe-placeholder misuse."
        }
        DiagnosticClass::TraitBoundNotSatisfied => {
            "A required trait bound isn't satisfied by the supplied type. Routes \
             inline bound failures (`T: Ord`) and `where`-clause failures, plus \
             missing-supertrait diagnostics on impl blocks."
        }
        DiagnosticClass::RefutablePattern => {
            "A `let PAT = expr;` binding uses a refutable pattern — one that may not \
             match every value of the bound type. Use `let ... else { ... }`, \
             `if let`, or `match` instead."
        }
        DiagnosticClass::CannotInferTypeParam => {
            "A generic type parameter couldn't be inferred from the surrounding \
             context. Add a turbofish annotation (`f[T](...)`) or a binding-type \
             annotation that pins the parameter."
        }
        DiagnosticClass::UndefinedName => {
            "An identifier in value position doesn't resolve to a binding, function, \
             constant, or import."
        }
        DiagnosticClass::DuplicateDefinition => {
            "A definition would shadow an existing item that the resolver treats as \
             an error rather than a shadow (e.g., duplicate function / type / impl \
             names in the same scope)."
        }
        DiagnosticClass::EffectUndeclared => {
            "A public function uses an effect that isn't listed in its declared \
             effect row. Add the effect to the signature, or wrap the call in a \
             handler that discharges it."
        }
        DiagnosticClass::EffectConflict => {
            "Two concurrent or interleaved computations have conflicting effects \
             (e.g., parallel `writes(R)` against the same resource)."
        }
        DiagnosticClass::OwnershipMoveAfterUse => {
            "A binding was used after its value was moved. Clone before the move, \
             borrow with `ref` / `mut ref`, or restructure to avoid the second use."
        }
        DiagnosticClass::OwnershipBorrowConflict => {
            "A live borrow conflicts with a later operation. Includes ref-vs-mut-ref \
             overlap, slice-vs-ref overlap, drop-of-borrowed-source, and call-site \
             mut-marker requirements that aren't met."
        }
        DiagnosticClass::OwnershipUseOfUninitialized => {
            "A binding was read before it was initialised. The let-uninit DFA tracks \
             initialisation through later assignments; this fires when a read \
             precedes the first assignment."
        }
        DiagnosticClass::TargetIncompatible => {
            "A cross-target violation: file-suffix conditional compilation \
             mismatch, target-feature-gated intrinsic used outside its target, or \
             cross-target effect violation. Shared family — all target-incompatibility \
             classes route here."
        }
        DiagnosticClass::UnsafeRequired => {
            "An operation requires an enclosing `unsafe { }` block (raw-pointer \
             deref, raw-pointer arithmetic, union field read, etc.) and none is \
             present."
        }
        DiagnosticClass::FfiViolation => {
            "An FFI-shape rule was violated: invalid union declaration (missing \
             `#[repr(C)]`, non-Copy field, `Drop` impl), FFI-float equality without \
             tolerance, opaque-type constraint violation, or repr/layout mismatch."
        }
        DiagnosticClass::LayoutQueryInvalid => {
            "A `size_of[T]()` / `align_of[T]()` / `offset_of[T](path)` call has the \
             wrong shape — missing type argument, generic-parameter target, unknown \
             field path, opaque-type target, or non-struct target."
        }
        DiagnosticClass::LintWarning => {
            "A lint-level diagnostic surfaced as warning or error per `#[allow]` / \
             `#[warn]` / `#[deny]` controls. The specific lint name lives in the \
             diagnostic record's `lint_name` field; this class tag signals the \
             diagnostic came from the lint machinery rather than a hard rule."
        }
        DiagnosticClass::Other => {
            "Diagnostic emitted but not yet individually classified. Backfill is \
             incremental work; the JSON contract treats this as a valid class while \
             the classification spreads through the codebase."
        }
    }
}

/// Concept page for `karac explain --concept=closures`. Describes
/// Rule 2 first-use capture-mode inference, the three explicit
/// prefixes (`own` / `ref` / `mut ref`), the K2 conflict table with
/// the exact diagnostic-redirect wording the ownership checker emits,
/// the outer-scope routing rule for `own`-captured roots, and the
/// per-function inspection surface (`karac query ownership <fn>`).
///
/// Cross-references the disjoint-capture (Rule 2¼) extension — see
/// `docs/implementation_checklist/phase-5-diagnostics.md` § Disjoint
/// closure capture; once that lands, the per-name inference described
/// here generalises to per-path uniformly through the same
/// `closure_captures` registry without rewriting this page.
const CLOSURES_PAGE: &str = "\
karac explain — Closures: parameter modes, capture, and escape

Source of truth: docs/design.md § Closures: parameter modes, capture,
and escape > Rule 2 / Rule 2½. This page is the concept-level summary;
the design.md section is authoritative when the two disagree.

────────────────────────────────────────────────────────────────────
Bare form: |x| body — Rule 2 first-use inference
────────────────────────────────────────────────────────────────────

A bare closure runs a per-captured-name scan over the body and picks
the weakest mode that satisfies the body's first classifying use:

    first use is a read     → capture is taken by `ref`
    first use is a mutate   → capture is taken by `mut ref`
    first use is a consume  → capture is taken by `own` (moved in)

The closure does whatever the body demands, no more. Modes form an
ordering — `ref < mut ref < own` — and the inferred mode is the
minimum that satisfies the body.

Granularity is per-capture-name today: field projections under the
same root binding (e.g. `o.x` and `o.y`) collapse to one entry for
the root `o`. The disjoint-capture extension (Rule 2¼) will refine
this to per-path so two closures over different fields of the same
struct can each take their own mode — see
phase-5-diagnostics.md § \"Disjoint closure capture\".

────────────────────────────────────────────────────────────────────
Outer-scope routing for `own`-captured roots
────────────────────────────────────────────────────────────────────

When a bare body consumes a captured root, the root is classified
`own` and moved into the closure. A *use of the same binding after
the closure expression* is not a use-after-move error — it routes
through Part 4's RC fallback (RcTrigger::ClosureCaptureWithOuterUse),
tentatively promoting the binding to `Rc`. This is the spec's
\"(ii) Escape with outer use of a capture\" path: the closure creation
and the outer use compose under the existing RC dataflow pass rather
than a closure-specific borrow rule.

────────────────────────────────────────────────────────────────────
Explicit prefixes: own | ref | mut ref
────────────────────────────────────────────────────────────────────

Three optional keywords on a closure expression *pin* every captured
path to a single declared mode regardless of what first-use inference
would pick:

    |x| body          // bare — per-capture inference (Rule 2)
    own |x| body      // every capture is by value (consume — moved)
    ref |x| body      // every capture is by reference (read-only)
    mut ref |x| body  // every capture is by mutable reference

The prefix is *per closure*, not per capture. Per-name override
syntax is deferred — the per-closure form composes forward without
breakage if real programs surface the need.

`move |x|` is rejected with a focused diagnostic redirecting to
`own |x|` (Kāra uses the `own` keyword; see design.md § Reserved
keywords).

────────────────────────────────────────────────────────────────────
K2 conflict table — declared mode is the floor
────────────────────────────────────────────────────────────────────

When a prefix is present, body usage must satisfy the declared mode
but may be weaker. Stronger usage than declared is a compile error
at the closure expression site, naming the capture and the offending
use's line.

    Declared    Body usage          Result
    ────────    ───────────────     ───────────────────────────────
    own         reads only          OK — \"capture for ownership
                                         extension\" idiom
    own         mutates             OK
    own         consumes            OK
    ref         reads only          OK
    ref         mutates             ERROR (escalation)
    ref         consumes            ERROR — see [K2-ref-consume]
    mut ref     reads only          OK — perf note
                                         [unused-mut-capture]
    mut ref     mutates             OK
    mut ref     consumes            ERROR — see [K2-mut-ref-consume]

The bare form has no row in this table — its body-usage row *is*
the inference rule and there is nothing to conflict against.

Diagnostic wording the ownership checker emits (pinned by
slice 1 of phase-5-diagnostics.md § Closure default capture mode):

  [K2-ref-consume]
    capture `x` declared `ref` but consumed in closure body at
    line N — drop the `ref` prefix (use `own` or bare) or remove the consume

  [K2-mut-ref-consume]
    capture `x` declared `mut ref` but consumed in closure body at
    line N — drop the `mut ref` prefix and use `own`

  [unused-mut-capture]
    perf[unused-mut-capture]: capture `x` declared `mut ref` but never
    mutated — consider `ref` (machine-applicable rewrite when the
    prefix span is recorded)

────────────────────────────────────────────────────────────────────
When to use which form
────────────────────────────────────────────────────────────────────

  • Use bare `|x|` when the body is short, the closure stays inside
    its creation scope, and refactoring fragility is not a concern.
    First-use inference is locally fragile — reordering body lines
    or adding an early `.clone()` can flip a capture from consume
    to read, which changes RC decisions in the *enclosing* function.

  • Use an explicit prefix (`own` / `ref` / `mut ref`) when the
    closure escapes (return, store, send across a channel) and the
    captures' fates need to be visible at the closure expression
    site so a benign body refactor cannot silently alter the
    surrounding ownership analysis.

────────────────────────────────────────────────────────────────────
Inspecting inferred capture modes
────────────────────────────────────────────────────────────────────

Per-function inferred capture modes are exposed by

    karac query ownership <file>.<function>

Each closure in the function shows as a JSON entry with `parameters`
(one record per parameter, `{name, mode}`) and `captures` (the same
shape per captured root binding), each tagged with the closure's
source `line` / `column`. The `mode` field is one of `own` / `ref`
/ `mut_ref` and reflects either the prefix-declared mode (if a
prefix is present) or the Rule 2 inferred mode (if bare).

Sample shape:

    {
      \"function\": \"main\",
      \"closures\": [
        {
          \"line\": 7, \"column\": 19,
          \"parameters\": [{\"name\": \"x\", \"mode\": \"ref\"}],
          \"captures\":   [{\"name\": \"o\", \"mode\": \"own\"}]
        }
      ]
    }
";

const OPERATORS_PAGE: &str = "\
karac explain — Operators: trait dispatch, desugaring, and why a call fails

Source of truth: docs/design.md § Operator Traits and § Index / IndexMut.
This page documents what the compiler ENFORCES TODAY; where that differs
from the spec, the divergence is called out in the last section rather
than papered over, because the point of this page is to tell you why a
real operator call was rejected.

────────────────────────────────────────────────────────────────────
The desugaring table
────────────────────────────────────────────────────────────────────

Operators are trait-dispatched; there is no parser-level operator table
for built-in types. After type checking, the lowering phase rewrites the
AST node to a trait call with the span preserved.

    Operator        Trait        Desugars to
    ─────────────   ──────────   ─────────────────────────────────
    a + b           Add          Add.add(a, b)
    a - b           Sub          Sub.sub(a, b)
    a * b           Mul          Mul.mul(a, b)
    a / b           Div          Div.div(a, b)
    a % b           Rem          Rem.rem(a, b)
    -a              Neg          Neg.neg(a)
    a == b          PartialEq    PartialEq.eq(ref a, ref b)
    a != b          PartialEq    not PartialEq.eq(ref a, ref b)
    a < b           PartialOrd   partial_cmp(ref a, ref b).is_lt()
    a <= b          PartialOrd   partial_cmp(ref a, ref b).is_le()
    a > b           PartialOrd   partial_cmp(ref a, ref b).is_gt()
    a >= b          PartialOrd   partial_cmp(ref a, ref b).is_ge()
    a & b           BitAnd       BitAnd.bitand(a, b)
    a | b           BitOr        BitOr.bitor(a, b)
    a ^ b           BitXor       BitXor.bitxor(a, b)
    a << b          Shl          Shl.shl(a, b)
    a >> b          Shr          Shr.shr(a, b)
    not a  (bool)   Not          Not.not(a)
    c[key]          Index        Index.index(ref c, key)
    c[i, j]         Index        Index.index(ref c, (i, j))
    c[key] = v      IndexMut     *IndexMut.index_mut(mut ref c, key) = v

Arithmetic and bitwise traits take `self` (owned) because they produce a
new value. Comparison traits take `ref self` — comparing two values never
consumes them.

`and` / `or` are short-circuit KEYWORDS, not trait-dispatched operators.
They have no trait and cannot be implemented.

Compound assignment desugars through the binary operator: `a += b` means
`a = a + b`, checked under `Add`. There is no `AddAssign` trait in v1.
When `a` is a `mut ref T` lvalue the desugar is assign-through —
`*a = *a + b` — so the caller's value is updated rather than the local
name being rebound.

────────────────────────────────────────────────────────────────────
Why your operator call failed
────────────────────────────────────────────────────────────────────

Four rejections cover nearly every case. Each row is the diagnostic you
will actually see, followed by the fix.

1.  type 'T' does not implement trait Add

    `+ - * / %` are accepted only on the numeric primitives (i8..i64,
    u8..u64, f32, f64, usize, isize) and on `String` for `+`. A struct,
    enum, Vec, or distinct type reaches this error, which names the trait
    the operator desugars to (Add, Sub, Mul, Div, Rem).

    For a Vec, there is deliberately no `impl Add` — the ambiguity
    between concatenation and elementwise addition is why the language
    makes you name the method: use `a.extend(b)` to append b's elements
    to a. Note that `Vec.concat()` is a DIFFERENT operation — it takes no
    argument and joins a `Vec[String]`'s own elements into one String —
    so it is not the two-Vec join, despite design.md's wording.

    For a `distinct` type the diagnostic says: add #[derive(Arithmetic)]
    to 'T' to use arithmetic operators between two 'T' values, or unwrap
    explicitly. That derive enables the operators between two values of
    the SAME distinct type.
    Cross-type arithmetic stays an error by design — that is what
    `distinct` is for. Without the derive, unwrap: `FloorNum(a.raw() + 1)`.

2.  type 'T' does not implement PartialEq;
      add #[derive(PartialEq)] to use == or !=
    type 'T' does not implement PartialOrd;
      add #[derive(PartialOrd)] to use <, <=, >, or >=

    Add the derive the message names. These are the PARTIAL traits on
    purpose: the desugaring runs through them, and each is sufficient on
    its own. `Eq` and `Ord` are the total traits — deriving them also
    works, but they are strictly more than the operator needs, and `Eq`
    is not derivable at all on a type with an `f32`/`f64` field, since
    `NaN != NaN` breaks reflexivity.

3.  cannot mix integer types 'i32' and 'i64' in arithmetic — they must match
    cannot mix integer and floating-point operands ('i64' and 'f64')

    Operands must have the SAME type. Cast the narrower one explicitly:
    `(x as i64) + y`. There is no implicit widening at operator
    boundaries today — see the divergence section.

4.  user-defined `impl Add for T` is not supported in v1;
    operator traits are stdlib-only

    v1 implements the operator traits for stdlib types only. The trait
    names, signatures, and desugaring are already what user impls will
    use, so lifting this is a non-breaking, additive change. Until then,
    write a named method.

────────────────────────────────────────────────────────────────────
Which types implement what
────────────────────────────────────────────────────────────────────

  Arithmetic     numeric primitives; `Add` additionally on `String`
                 (`a + b` consumes `a`, borrows `b`, allocates)
  Bitwise        integer primitives; `not` on `bool`
  PartialEq      every primitive, both string types, and lifted through
                 Vec / Option / Result / tuples when elements qualify
  Eq             the same minus `f32` / `f64` — IEEE NaN != NaN breaks
                 reflexivity, so floats are PartialEq but not Eq
  PartialOrd     every primitive including floats, both string types,
                 lifted through Vec / Option / Result / tuples
  Ord            the same minus `f32` / `f64` — NaN is incomparable.
                 The `F32` / `F64` total-order wrappers implement both
                 Eq and Ord, treating NaN == NaN and sorting NaN last
  Index          Vec, Array, Slice, Map; range indexing `c[a..b]`
                 yields `Slice[T]`

Indexing panics on an invalid index rather than returning `Result`,
contributing a `panics` effect to the calling function. When bounds are
uncertain use `.get(idx)`, which returns `Option[ref T]`.

────────────────────────────────────────────────────────────────────
Where the implementation differs from design.md today
────────────────────────────────────────────────────────────────────

Measured, not inferred. Each is tracked; see docs/bug-ledger.jsonl.

  • Missing-impl diagnostics speak OPERATOR language, not trait
    language. design.md specifies `vec1 + vec2` should report \"type
    Vec[T] does not implement trait Add\" and point at `.concat` /
    `.extend`; it currently reports \"arithmetic operator requires
    numeric type, found 'Vec[i64]'\" and names no trait or method.

  • Implicit lossless widening at operator boundaries is NOT
    implemented. design.md says `(x: i32) + (y: i64)` is valid and
    widens to `i64`; the compiler rejects it and asks for a cast.

  • Bitwise `Not` on integer primitives is not available — `not` on an
    integer reports \"unary 'not' requires 'bool'\".

  • design.md offers `vec.concat(other)` as one of the two redirects for
    `vec1 + vec2`, but `Vec.concat()` here is the zero-argument
    `Vec[String]` join, not a two-Vec concatenation. The diagnostic names
    only `extend`, which is the one that exists.
";

const STABLE_HASH_PAGE: &str = "\
karac explain — Stable hashing: digests that outlive the process

Source of truth: docs/design.md § `Hash` and `Hasher`, stability
policy. This page is the concept-level summary; the design.md section
is authoritative when the two disagree.

This is the page a `Hash` reached for the wrong reason points at.

────────────────────────────────────────────────────────────────────
Why `Hash` cannot give you a stable digest
────────────────────────────────────────────────────────────────────

`Map` and `Set` hash through SipHash-1-3 under a key drawn from a
random source once per process. Two runs of the same binary, over the
same input, produce different digests — and therefore different
iteration orders. That is deliberate: the common attack on a map keyed
by request data is hash flooding, and a key the attacker cannot
predict is the defence.

So a `Hash` digest is explicitly NOT stable across runs, NOT stable
across Kara versions, and NOT stable across targets. Nothing stops you
writing one to disk. Everything stops that from working.

────────────────────────────────────────────────────────────────────
What to use instead
────────────────────────────────────────────────────────────────────

    StableHash.siphash24(bytes, k0, k1) -> u64

SipHash-2-4 over `bytes` under the 128-bit key `(k0, k1)`. It reads no
process state, so the same bytes under the same key give the same
`u64` across runs, across machines, across targets, and across Kara
versions — the four axes `Hash` disclaims, in that order.

    let id = StableHash.siphash24(payload.bytes(), 0, 0);

The first parameter is `Slice[u8]`, so a `Vec[u8]`, a `[u8]` literal,
`s.bytes()` and an existing slice all pass straight in.

Reach for it whenever the digest outlives the process that took it:

    content addressing    the digest IS the name of the thing
    on-disk indexes       written by one run, read by the next
    snapshot tests        a fixture that must not churn
    distributed sharding  two machines must agree on the owner

2-4 rather than the `Map` default's 1-3 because 2-4 is the round count
the SipHash paper specifies, and therefore the one another language's
`siphash24` computes for the same input. Interoperability is the whole
point, which is why the algorithm is named in the function rather than
left to the compiler to choose.

────────────────────────────────────────────────────────────────────
The key, and why you have to write it
────────────────────────────────────────────────────────────────────

`(0, 0)` is a perfectly good key for a content address and is what
most callers want. It is required rather than defaulted because
changing the key later changes every digest you have ever stored —
a fact that belongs where the digest is taken, not in a default nobody
reads.

Use a non-zero key when the input is adversarial AND the digest must
still be stable. Pick one, keep it secret, and store it outside the
source.

────────────────────────────────────────────────────────────────────
Two things that look like the answer and are not
────────────────────────────────────────────────────────────────────

    KARAC_HASH_SEED     Pins the per-process key so a run reproduces
                        exactly. A testing and debugging affordance:
                        a published key is the same as no key, which
                        is precisely the DoS resistance the random
                        default exists to provide. Never set it in a
                        deployment, and never let a digest that
                        leaves the process depend on it.

    Map[K, V,           Unkeyed, so iteration order IS stable across
    FxBuildHasher]      runs of one binary. Still not a stable
                        DIGEST: the algorithm is a `Map`
                        implementation detail free to change between
                        Kara versions, and it is floodable by anyone
                        who can read the compiler's source.

────────────────────────────────────────────────────────────────────
Where the implementation differs from design.md today
────────────────────────────────────────────────────────────────────

design.md's stability paragraph names three things. One of them
ships, and the spec spells its path Rust-style (`hash::stable::`);
Kara has no `::` separator, so the real spelling is the namespace
below.

    StableHash.siphash24  ships — this page.

    hash.stable.xxh3      DOES NOT EXIST. There is no faster unkeyed
                          stable digest; use `siphash24`.

    the `crypto` module   DOES NOT EXIST. There is no cryptographic
                          hash in the stdlib. Do NOT substitute
                          `siphash24` for one: SipHash is a fast
                          keyed PRF, not a collision-resistant hash,
                          and it is unfit for signatures, for
                          deduplication against an adversary, or for
                          anything where a chosen collision is a
                          problem.

Tracked as B-2026-08-26-2 in docs/bug-ledger.jsonl.
";

const MODULE_STATE_PAGE: &str = "\
karac explain — Module state: bindings, effects, and the alternatives

Source of truth: docs/design.md § Module-Level Bindings. This page is
the concept-level summary; the design.md section is authoritative when
the two disagree.

This is the page the `module_mut_binding` warning points at.

────────────────────────────────────────────────────────────────────
The compile-time initializer rule
────────────────────────────────────────────────────────────────────

A `.kara` file may declare `let` and `let mut` bindings at file scope.
Their initializers must be compile-time constant expressions. Kāra has
no module initialization: no code runs before `main`, so there is no
init-order problem, no unattributed startup effects, and no lazy
first-access semantics. Module-level bindings are constant data in the
binary.

Allowed: literals; arithmetic, comparison and boolean operations on
constants; enum variant constructors with constant arguments; struct
and tuple literals over constants; array and repeat literals; and
references to other module-level bindings.

Rejected, each with its own error code:

    E_MODULE_BINDING_EFFECTFUL_INIT
        the initializer is a call, a closure, or anything carrying an
        effect. `let CONFIG: i64 = load();` is rejected even when
        `load` only reads — an effect at module scope has no function
        to attribute it to and no caller to catch its failure.

    E_MODULE_BINDING_HEAP_TYPE
        the binding's type needs runtime heap allocation. `String` is
        the common case: use `StringSlice`, which borrows the binary's
        read-only data segment at no cost. A string literal at module
        scope IS a `StringSlice` — the function-body default of
        `String` does not apply here.

    E_MODULE_BINDING_NAMING
        the binding name is Type-class (e.g. `G`, `Config`). Module
        bindings use SCREAMING_SNAKE_CASE.

────────────────────────────────────────────────────────────────────
`let mut` and the synthetic per-binding resource
────────────────────────────────────────────────────────────────────

Every module-level `let mut BINDING` implicitly declares a synthetic
effect resource. Reading it contributes `reads(BINDING_resource)` to
the reading function's inferred effect set; assigning contributes
`writes(BINDING_resource)`. This feeds conflict analysis directly: two
readers never conflict, a reader and a writer always do, two writers
always do.

The resource is per-binding on purpose. A single global bucket would
force every module-level mutation to serialize against every other,
which defeats the point. It is not exportable and cannot be named in
user code — it exists only for conflict analysis.

Only module-level `let mut` gets one. Inside a function body, reads and
writes to struct fields and Vec elements do not contribute named
resources; the ownership system's aliasing analysis governs those.

────────────────────────────────────────────────────────────────────
The `par { }` rule
────────────────────────────────────────────────────────────────────

A `par { }` branch or `spawn()`-ed task whose TRANSITIVE effect set
contains `writes(BINDING_resource)` is a compile error:

    error[effect]: module-level let mut 'COUNTER' cannot be written
    from inside par { } — wrap in Atomic[T], Mutex[T], or use
    #[thread_local] for per-task state (binding declared at line 1)

The check is effect-set-based, not syntactic — calling a helper that
carries the effect is caught exactly as if the assignment were inline.
A reader branch beside a writer branch is the same error. Two branches
that both only read are fine.

The conflict analysis would already have serialized these branches,
but serializing inside `par { }` is almost never what was meant, so
the compiler upgrades the conflict to an error.

────────────────────────────────────────────────────────────────────
The alternatives menu
────────────────────────────────────────────────────────────────────

What the `module_mut_binding` warning is steering you toward, and what
each one is actually for:

  Context struct     Build the value in `main` and pass it down. The
                     default answer for loaded config, database pools,
                     compiled regexes — anything needing runtime init.

  Atomic[T]          Lock-free scalar. Module scope OK.

  Mutex[T]           Mutual exclusion around a value. Module scope OK.

  #[thread_local]    Per-task disjoint copies of a `let mut`. Effect
                     becomes writes(ThreadLocal[BINDING_resource]),
                     which never conflicts with itself across tasks,
                     so it is legal inside `par { }`. Initializer must
                     still be compile-time constant.

  OnceLock[T]        Write-once, thread-safe, set explicitly at
                     runtime. For values depending on input the
                     closure cannot see at module-load time — CLI
                     flags, env vars, config files. `main` calls
                     `set` once; the rest of the program calls `get`.
                     Surface: `new`, `set`, `get`, `get_or_init`,
                     `is_set`.

  OnceCell[T]        The single-task sibling of `OnceLock`, for
                     struct-field memoization. REJECTED at module
                     scope in every profile
                     (E_ONCE_CELL_AT_MODULE_SCOPE) — module bindings
                     are visible to every task, and `OnceCell` carries
                     no synchronization.

Two primitives design.md names in this menu DO NOT EXIST, and are
left out above rather than listed with a caveat, because a menu is
read as a list of things you can type:

    RwLock[T]     Not a type at all — `undefined type 'RwLock'`.
                  Use `Mutex[T]` at module scope.

    LazyLock[T]   Passes `karac check` in the module-binding form
                  design.md shows, then fails on EVERY execution
                  backend: the interpreter has no evaluation rule for
                  `LazyLock.new`, and both `karac run` and
                  `karac build` reject `.get()` with a codegen error.
                  Use `OnceLock[T]` and `set` it from `main`.

There is no `static mut`. The absence of a \"raw mutable global,
caller's responsibility to synchronize\" path is deliberate.

────────────────────────────────────────────────────────────────────
Suppressing the warning
────────────────────────────────────────────────────────────────────

    #[allow(module_mut_binding)]
    let mut COUNTER: i64 = 0;

The suppression is per-binding, not module-wide, so each site opts in
explicitly. The attribute attaches to the binding name the diagnostic
underlines.

────────────────────────────────────────────────────────────────────
Profile gating
────────────────────────────────────────────────────────────────────

    Profile              Module-level `let mut`
    ──────────────────   ──────────────────────────────────────────
    lib (default)        Warning — module_mut_binding
    embedded             Permitted (MMIO, DMA, static buffers)
    app                  Specified as a hard error; NOT IMPLEMENTED
    gpu                  Specified as a hard error; NOT IMPLEMENTED

The warning fires only under the default profile, matching the table's
`lib` row. `embedded` permits the form outright — that is where the
feature's justification lives — so firing there would be a lint beyond
its documented trigger. The `app` and `gpu` rows specify a hard error
rather than a lint, and no such profile variant exists in
`CompileProfile` yet, so those rows are left unimplemented rather than
approximated by the warning.
";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::typechecker::TypeErrorKind as K;

    /// No code may be minted by two phases (B-2026-07-27-14). A
    /// diagnostic's `code` is the stable short key every agent loop,
    /// IDE, and docs index reads; the moment two phases share a number
    /// it stops identifying anything on its own.
    #[test]
    fn code_table_has_no_cross_phase_collisions() {
        let mut collided: Vec<&str> = CODE_TABLE
            .iter()
            .filter(|(code, _)| {
                let rows = lookup_code(code);
                rows.iter().map(|e| e.phase).collect::<Vec<_>>().len() > 1
            })
            .map(|(code, _)| *code)
            .collect();
        collided.sort_unstable();
        collided.dedup();
        assert!(
            collided.is_empty(),
            "these codes are minted by more than one phase: {collided:?}. \
             Each phase owns a band (resolve E01xx, typecheck E02xx/W02xx, \
             ownership E05xx); allocate from the emitting phase's band."
        );
    }

    /// Every numeric `(phase, code)` pair the emitter actually mints,
    /// read out of the diagnostic emitter's own `match err.kind` arms
    /// (`src/cli/diag_json.rs` since the cli extraction; before that,
    /// `src/cli.rs` itself — the scan moved with the arms).
    ///
    /// Scanning the source is deliberate. `ResolveErrorKind` /
    /// `TypeErrorKind` cannot be enumerated at runtime, so any
    /// hand-maintained copy of the mapping is one more thing that can
    /// drift — and drift is the whole defect here: the collision guard
    /// below can only see codes it is told about, so a stale copy
    /// reports "no collisions" while real ones sit in the emitter.
    /// That is exactly what happened before B-2026-07-27-14 was fixed
    /// (`CODE_TABLE` stopped at E0240, hiding four of the eight).
    fn emitted_codes() -> Vec<(&'static str, &'static str)> {
        let src = include_str!("diag_json.rs");
        let mut out = Vec::new();
        for line in src.lines() {
            let line = line.trim();
            let phase = if line.contains("ResolveErrorKind::") {
                "resolve"
            } else if line.contains("TypeErrorKind::") {
                "typecheck"
            } else {
                continue;
            };
            let Some((_, rest)) = line.split_once("=> \"") else {
                continue;
            };
            let Some((code, _)) = rest.split_once('"') else {
                continue;
            };
            // Symbolic codes (E_UNKNOWN_ATTRIBUTE, …) are outside the
            // numeric bands by design.
            if code.len() == 5
                && (code.starts_with('E') || code.starts_with('W'))
                && code[1..].chars().all(|c| c.is_ascii_digit())
            {
                out.push((phase, code));
            }
        }
        assert!(
            out.len() > 80,
            "the emitter scan found only {} arms — the match-arm shape in \
             src/cli.rs changed and this guard has gone blind",
            out.len()
        );
        out
    }

    /// The emitter itself must mint no code from two phases. This is
    /// the real invariant; the `CODE_TABLE` check below is downstream
    /// of it.
    #[test]
    fn emitter_mints_no_code_from_two_phases() {
        let mut collided: Vec<&str> = Vec::new();
        for (_, code) in emitted_codes() {
            let phases: Vec<&str> = emitted_codes()
                .into_iter()
                .filter(|(_, c)| *c == code)
                .map(|(p, _)| p)
                .collect();
            if phases.iter().any(|p| *p != phases[0]) {
                collided.push(code);
            }
        }
        collided.sort_unstable();
        collided.dedup();
        assert!(
            collided.is_empty(),
            "these codes are minted by more than one phase: {collided:?}. \
             Each phase owns a band (resolve E01xx, typecheck E02xx/W02xx, \
             ownership E05xx); allocate from the emitting phase's band."
        );
    }

    /// The resolver allocates only from its own `E01xx` band (plus the
    /// deliberately shared `E08xx` target band and symbolic codes).
    /// Pins the band split so a new resolver diagnostic cannot drift
    /// back into the typechecker's `E02xx` and re-open the collision.
    #[test]
    fn resolver_numeric_codes_live_in_the_resolve_band() {
        let mut strays: Vec<&str> = emitted_codes()
            .into_iter()
            .filter(|(phase, code)| {
                // `W01xx` is the resolver's WARNING half of the same 01xx
                // numeric band (B-2026-08-21-2 follow-up): the band exists to
                // stop numeric collisions with the typechecker's E02xx, and
                // the prefix carries severity, not ownership. Before
                // `layout_unassigned_fields` the resolver minted only errors,
                // so this check could assume an `E` prefix.
                *phase == "resolve"
                    && !code.starts_with("E01")
                    && !code.starts_with("E08")
                    && !code.starts_with("W01")
            })
            .map(|(_, code)| code)
            .collect();
        strays.sort_unstable();
        strays.dedup();
        assert!(
            strays.is_empty(),
            "resolver codes outside the E01xx band: {strays:?}. \
             The typechecker owns E02xx — allocating there collides."
        );
    }

    /// Every numeric resolver code the emitter mints must be
    /// catalogued in [`CODE_TABLE`], so `karac explain <code>` can
    /// answer for all of them and the collision guard can see them.
    #[test]
    fn code_table_catalogues_every_resolver_code() {
        let mut missing: Vec<&str> = emitted_codes()
            .into_iter()
            .filter(|(phase, code)| {
                *phase == "resolve"
                    && !CODE_TABLE
                        .iter()
                        .any(|(c, e)| c == code && e.phase == "resolve")
            })
            .map(|(_, code)| code)
            .collect();
        missing.sort_unstable();
        missing.dedup();
        assert!(
            missing.is_empty(),
            "resolver codes missing from CODE_TABLE: {missing:?}. \
             An absent row hides collisions from \
             emitter_mints_no_code_from_two_phases."
        );
    }

    /// Every numeric code the diagnostic emitter mints, regardless of phase.
    ///
    /// There is no band list to check against. [`CODE_TABLE`] claims the
    /// WHOLE numeric space since B-2026-08-20-31, so every numbered code the
    /// emitter mints must have a row, full stop — nothing to keep in sync.
    /// Symbolic codes (`E_SLICE_BORROW_CONFLICT`, `E_UNKNOWN_ATTRIBUTE`, …)
    /// are outside the numbering by design and are reached by class instead.
    ///
    /// [`emitted_codes`] above cannot serve here: it keys off the
    /// `ResolveErrorKind::` / `TypeErrorKind::` text on the line, so it sees
    /// nothing minted from an `EffectErrorKind` arm or from a bare `_ =>`
    /// fallthrough — which is precisely where four of this guard's findings
    /// were hiding (`E0230`, `E0231`, `E0802`, `W0299`).
    fn emitted_numeric_codes() -> Vec<&'static str> {
        const SRC: &str = include_str!("diag_json.rs");
        // Byte-indexed on purpose: the file has multi-byte characters in its
        // prose, and a char-boundary slice over a five-ASCII-byte window is
        // both slower and a panic waiting to happen.
        let b = SRC.as_bytes();
        let mut out = Vec::new();
        for i in 0..b.len().saturating_sub(6) {
            if b[i] != b'"' || b[i + 6] != b'"' {
                continue;
            }
            // Any uppercase prefix, not just `E`/`W`: the ownership notes are
            // `N05xx` and the FFI lint hint is `L0001`, and a scan that only
            // knew the two error prefixes would have skipped all three
            // silently — the exact failure mode this guard exists to prevent.
            if !b[i + 1].is_ascii_uppercase() {
                continue;
            }
            if !b[i + 2..i + 6].iter().all(|c| c.is_ascii_digit()) {
                continue;
            }
            out.push(&SRC[i + 1..i + 6]);
        }
        out.sort_unstable();
        out.dedup();
        assert!(
            out.len() > 100,
            "the literal scan found only {} codes — the emitter's shape \
             changed and this guard has gone blind",
            out.len()
        );
        out
    }

    /// A band the catalogue claims must have NO uncatalogued members
    /// (B-2026-08-20-30).
    ///
    /// `code_table_catalogues_every_resolver_code` pinned this for `resolve`
    /// only, so the typecheck band — the one `explain`'s own error message
    /// named first — drifted eight codes out of date without a failing test:
    /// `E0230`, `E0231` (effect-minted, in the typechecker's band), `E0279`,
    /// `W0245`, `W0249`, `W0255`, `W0299`, and `E0802` in the shared
    /// target/GPU band. Every one of them was a code a user could be handed
    /// and `karac explain` would refuse.
    ///
    /// Deliberately keyed on the BAND rather than the phase. A user reaching
    /// for `explain` has a number, not a phase, and the phase framing is what
    /// let an effect code inside `E02xx` look out of scope.
    #[test]
    fn code_table_catalogues_every_code_in_a_covered_band() {
        let mut missing: Vec<&str> = emitted_numeric_codes()
            .into_iter()
            .filter(|code| !CODE_TABLE.iter().any(|(c, _)| c == code))
            .collect();
        missing.sort_unstable();
        assert!(
            missing.is_empty(),
            "numbered codes the emitter mints with no CODE_TABLE row: \
             {missing:?}. Add the row under the phase that MINTS the code, \
             which need not be the owner of the band the number sits in."
        );
    }

    /// The two codes minted outside the diagnostic emitter — `E0223` from
    /// `print_cycles_text`, `E0227` from `ManifestError::code` — are invisible
    /// to the scan above, so pin them directly: catalogued under the minting
    /// phase, and still minted at the site the row names. Renumbering either
    /// mint site without moving its catalogue row fails here.
    #[test]
    fn the_two_out_of_emitter_codes_stay_catalogued_at_their_mint_sites() {
        for (code, phase, src, what) in [
            (
                "E0223",
                "module_graph",
                include_str!("build_cmds.rs"),
                "print_cycles_text",
            ),
            (
                "E0227",
                "manifest",
                include_str!("../manifest.rs"),
                "ManifestError::code",
            ),
        ] {
            assert!(
                CODE_TABLE
                    .iter()
                    .any(|(c, e)| *c == code && e.phase == phase),
                "{code} is not catalogued under phase `{phase}`"
            );
            assert!(
                src.contains(code),
                "{what} no longer mints {code} — move its CODE_TABLE row to \
                 match, or drop it"
            );
        }
    }

    /// The parser mints its codes through `ParseErrorKind::code`, so no
    /// literal reaches `diag_json.rs` and the scan above cannot see them.
    /// Enumerate the enum instead — and assert the count, because an added
    /// variant is a compile error in `code()` but would be a SILENT omission
    /// from a hand-written list here.
    #[test]
    fn code_table_catalogues_every_parse_code() {
        use crate::parser::ParseErrorKind as P;
        let all = [
            P::Syntax,
            P::UnexpectedToken,
            P::ReservedKeyword,
            P::ReservedSyntax,
        ];
        assert_eq!(
            all.len(),
            4,
            "ParseErrorKind gained a variant — add it here and to CODE_TABLE"
        );
        for kind in all {
            let code = kind.code();
            assert!(
                CODE_TABLE
                    .iter()
                    .any(|(c, e)| *c == code && e.phase == "parse"),
                "parse code {code} ({kind:?}) has no CODE_TABLE row"
            );
        }
    }

    /// The effect and ownership rows must restate
    /// `class_for_effect_error_kind` / `class_for_ownership_error_kind`, the
    /// same contract `code_table_class_matches_typechecker` holds for the
    /// typecheck rows. Without it the catalogue could claim a class the
    /// emitted record does not carry — and `class` is the field machine
    /// consumers route on, so the two disagreeing is worse than either being
    /// coarse (B-2026-08-20-31).
    #[test]
    fn code_table_class_matches_the_effect_and_ownership_classifiers() {
        use crate::effectchecker::EffectErrorKind as E;
        use crate::ownership::OwnershipErrorKind as O;

        let effects: &[(&str, E)] = &[
            ("E0400", E::MissingEffectDeclaration),
            ("E0401", E::OverDeclaredEffect),
            ("E0408", E::ModuleBindingWriteInPar),
            ("E0411", E::TargetGateViolation),
            ("E0413", E::ExternCUnwindRequiresPanics),
            ("E0416", E::NoEffectViolated),
            ("L0001", E::FfiLintHint),
            ("L0002", E::MutualRecursionNote),
            ("L0003", E::PureLoopInPar),
            ("E0802", E::GpuEffectViolation),
            ("E0230", E::ImplExceedsTraitCeiling),
        ];
        for (code, kind) in effects {
            let want = crate::effectchecker::class_for_effect_error_kind(kind);
            let row = CODE_TABLE
                .iter()
                .find(|(c, e)| c == code && e.phase == "effect")
                .unwrap_or_else(|| panic!("{code} has no effect row"));
            assert_eq!(row.1.class, want, "{code} class");
        }

        let ownerships: &[(&str, O)] = &[
            ("E0500", O::UseAfterMove),
            ("E0501", O::OwnershipCycle),
            ("N0503", O::RcFallbackNote),
            ("E0505", O::UseOfUninitialized),
            ("E0508", O::RefCaptureEscapesScope),
            ("E0512", O::FrozenTypeNotFreezable),
        ];
        for (code, kind) in ownerships {
            let want = crate::ownership::class_for_ownership_error_kind(kind);
            let row = CODE_TABLE
                .iter()
                .find(|(c, e)| c == code && e.phase == "ownership")
                .unwrap_or_else(|| panic!("{code} has no ownership row"));
            assert_eq!(row.1.class, want, "{code} class");
        }
    }

    /// design.md source, for the code-assignment lint below.
    const DESIGN_MD: &str = include_str!("../../docs/design.md");

    /// Codes design.md names that [`CODE_TABLE`] does not carry, each with the
    /// phase that mints it.
    ///
    /// EMPTY, and that is the goal state rather than a gap: a row here is a
    /// code the spec names and the catalogue cannot answer for, which is a
    /// defect waiting to be noticed. `E0223` and `E0227` sat here until
    /// B-2026-08-20-30 catalogued them under their minting phases
    /// (`module_graph` / `manifest`) instead of renumbering them.
    ///
    /// The list stays as a declared escape hatch. A code can legitimately be
    /// written into design.md before its diagnostic ships, and reserving it
    /// here — with the phase that will mint it — is better than either
    /// inventing a `CODE_TABLE` row for a diagnostic that does not exist or
    /// letting the lint below fail with no way to say "not yet."
    const SPEC_RESERVED: &[(&str, &str, &str)] = &[];

    /// Codes design.md names ONLY to record that they were withdrawn. The prose
    /// has to keep the number — that is what stops it being re-allocated by
    /// someone who reads the section and sees a gap — so the lint has to know
    /// the difference between a live assignment and an epitaph.
    const SPEC_RETIRED: &[(&str, &str)] = &[
        ("E0226", "ConflictingPlatformModule"),
        // B-2026-08-20-25: the condition it named cannot arise — the walker
        // keeps at most one file per (module path, target).
    ];

    /// Every `E0NNN Name` pair design.md writes, in document order. A bare
    /// `E0NNN` with no name after it is skipped — those are JSON samples and
    /// prose references, not assignments.
    fn design_md_named_codes() -> Vec<(String, String)> {
        let mut out = Vec::new();
        let bytes = DESIGN_MD.as_bytes();
        let mut i = 0;
        while let Some(hit) = DESIGN_MD[i..].find("E0") {
            let start = i + hit;
            i = start + 2;
            let digits: String = DESIGN_MD[i..].chars().take(3).collect();
            if digits.len() != 3 || !digits.chars().all(|c| c.is_ascii_digit()) {
                continue;
            }
            // Must be a whole token: `aE0123` and `E01234` are not codes.
            if start > 0 && (bytes[start - 1] as char).is_alphanumeric() {
                continue;
            }
            let after = start + 5;
            if DESIGN_MD[after..].starts_with(|c: char| c.is_ascii_digit()) {
                continue;
            }
            let name: String = DESIGN_MD[after..]
                .trim_start_matches(' ')
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            // A name is UpperCamel and follows on the same line; anything else
            // (prose, a table cell, the end of a backticked span) is not one.
            if name.len() > 1 && name.starts_with(|c: char| c.is_ascii_uppercase()) {
                out.push((format!("E0{digits}"), name));
            }
        }
        out
    }

    /// design.md names diagnostic codes in prose, and those numbers are what a
    /// reader — human, or the Mend loop's LLM authoring against the spec —
    /// implements. Nothing tied them to the catalogue, so a code could be
    /// written into the spec in the wrong phase's band and sit there until
    /// someone tried to build it.
    ///
    /// That is not hypothetical. CR-24 allocated the whole module-system family
    /// out of the typechecker's `E02xx` band; B-2026-07-27-14 moved the three
    /// that had collided into `E01xx` and left the rest, so design.md went on
    /// naming `E0226 ConflictingPlatformModule` — a check the typechecker would
    /// never mint — until B-2026-08-20-25 established it could not fire at all
    /// and struck it. Two of that family are still out of band and shipping.
    /// This is the guard that makes the next one a test failure rather than a
    /// discovery.
    ///
    /// Every `E0NNN Name` the spec writes must therefore match its `CODE_TABLE`
    /// row, or be listed in [`SPEC_RESERVED`] with the phase that mints it, or
    /// in [`SPEC_RETIRED`] as a withdrawn number the prose still records.
    #[test]
    fn design_md_code_assignments_match_the_catalogue() {
        let mut wrong_kind: Vec<String> = Vec::new();
        let mut uncatalogued: Vec<String> = Vec::new();
        let mut out_of_band: Vec<String> = Vec::new();

        for (code, name) in design_md_named_codes() {
            if let Some(entry) = CODE_TABLE.iter().find(|(c, _)| *c == code).map(|(_, e)| e) {
                if entry.kind != name {
                    wrong_kind.push(format!(
                        "{code}: design.md says `{name}`, CODE_TABLE says `{}`",
                        entry.kind
                    ));
                }
                continue;
            }
            if SPEC_RETIRED.iter().any(|(c, n)| *c == code && *n == name) {
                continue;
            }
            let Some((_, _, phase)) = SPEC_RESERVED.iter().find(|(c, _, _)| *c == code) else {
                uncatalogued.push(format!("{code} {name}"));
                continue;
            };
            // A reserved code still has to sit in the band of the phase that
            // mints it. Only `resolve` has a band to check against here — the
            // two CLI-level phases have none yet (see SPEC_RESERVED).
            if *phase == "resolve" && !code.starts_with("E01") && !code.starts_with("W01") {
                out_of_band.push(format!("{code} {name} (minted by resolve)"));
            }
        }

        for list in [&mut wrong_kind, &mut uncatalogued, &mut out_of_band] {
            list.sort();
            list.dedup(); // a code named in several places reports once
        }
        assert!(
            wrong_kind.is_empty(),
            "design.md and CODE_TABLE disagree about what a code IS: {wrong_kind:?}"
        );
        assert!(
            uncatalogued.is_empty(),
            "design.md names diagnostic codes that are neither catalogued nor \
             listed in SPEC_RESERVED / SPEC_RETIRED: {uncatalogued:?}. Add the \
             row to CODE_TABLE when the diagnostic ships, to SPEC_RESERVED with \
             the phase that mints it, or to SPEC_RETIRED if the number was \
             withdrawn."
        );
        assert!(
            out_of_band.is_empty(),
            "design.md allocates a code outside its minting phase's band: \
             {out_of_band:?}. Each phase owns a band (resolve E01xx, typecheck \
             E02xx/W02xx, ownership E05xx)."
        );
    }

    /// The lint above is only worth anything if the extractor actually finds the
    /// spec's assignments — one that silently matched nothing would make all
    /// three of its assertions vacuously true. Pin the set.
    #[test]
    fn design_md_named_codes_finds_the_module_system_family() {
        let found = design_md_named_codes();
        for (code, name) in [
            ("E0111", "PrivateItemAccess"),
            ("E0112", "UnknownModule"),
            ("E0113", "UnknownItemInModule"),
            ("E0124", "AmbiguousWildcardImport"),
            ("E0221", "PrivateTypeInPublicSignature"),
            ("E0223", "CircularModuleDependency"),
            ("E0227", "NotInsideKaraProject"),
        ] {
            assert!(
                found.iter().any(|(c, n)| c == code && n == name),
                "design.md names `{code} {name}` but the extractor missed it"
            );
        }
        // A bare code with no name — design.md's JSON sample carries
        // `"code":"E0099"` — must NOT be read as an assignment.
        assert!(
            !found.iter().any(|(c, _)| c == "E0099"),
            "a bare code with no name was read as an assignment"
        );
    }

    /// Every [`SPEC_RESERVED`] / [`SPEC_RETIRED`] row must still be named in
    /// design.md — otherwise the list rots into a reservation for a code the
    /// spec no longer mentions.
    #[test]
    fn spec_reserved_and_retired_rows_are_still_in_design_md() {
        let named = design_md_named_codes();
        let mut stale: Vec<&str> = SPEC_RESERVED
            .iter()
            .filter(|(code, name, _)| !named.iter().any(|(c, n)| c == code && n == name))
            .map(|(code, _, _)| *code)
            .collect();
        stale.extend(
            SPEC_RETIRED
                .iter()
                .filter(|(code, name)| !named.iter().any(|(c, n)| c == code && n == name))
                .map(|(code, _)| *code),
        );
        assert!(
            stale.is_empty(),
            "SPEC_RESERVED / SPEC_RETIRED rows design.md no longer names: {stale:?}"
        );
    }

    /// No `(code, phase)` pair may appear twice — that would be a
    /// straight copy-paste error rather than a cross-phase collision.
    #[test]
    fn code_table_has_no_duplicate_rows() {
        let mut seen: Vec<(&str, &str)> = CODE_TABLE
            .iter()
            .map(|(code, entry)| (*code, entry.phase))
            .collect();
        let before = seen.len();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(
            before,
            seen.len(),
            "duplicate (code, phase) row in CODE_TABLE"
        );
    }

    /// A LINT's diagnostic code must not be a code the table assigns to a
    /// DIFFERENT PHASE.
    ///
    /// `code_table_has_no_duplicate_rows` above cannot see this: the lint codes
    /// are string literals in `diag_json.rs`'s emitters and never enter
    /// `CODE_TABLE`, so a lint could — and did — ship on a number the table
    /// already owned. `must_use` escalated by `-D must_use` emitted `E0250`,
    /// which `karac explain E0250` answers for the typecheck
    /// `ModuleBindingEffectfulInit` (B-2026-08-18-17).
    ///
    /// Scans the emitter's source for the one line shape every lint entry uses
    /// — `code: if is_error { "Ennnn" } else { "Wnnnn" },` — and asserts
    /// neither half is claimed by `typecheck` or `resolve`. A lint that lands
    /// on a taken number fails here rather than at whichever user runs
    /// `karac explain` on it.
    ///
    /// B-2026-08-18-28 RELAXED THIS FROM "absent" TO "not another phase's".
    /// The original form asserted lint codes appear NOWHERE in the table,
    /// which made the -17 guard and the -28 fix mutually exclusive: the
    /// catalogue could not list a lint code without failing the test meant to
    /// protect it. "No other phase owns this number" is the invariant -17
    /// actually wanted; "the table has never heard of it" was a proxy that
    /// happened to hold only while lints were uncatalogued.
    /// `every_lint_code_is_catalogued` below now pins the other direction, so
    /// relaxing this one loses no coverage.
    #[test]
    fn lint_codes_do_not_collide_with_the_code_table() {
        const DIAG_JSON_SRC: &str = include_str!("diag_json.rs");
        let mut checked = 0usize;
        for line in DIAG_JSON_SRC.lines() {
            let line = line.trim();
            let Some(rest) = line.strip_prefix("code: if is_error {") else {
                continue;
            };
            // `"Ennnn" } else { "Wnnnn" },`
            let codes: Vec<&str> = rest
                .split('"')
                .skip(1)
                .step_by(2)
                .filter(|s| s.len() == 5)
                .collect();
            assert_eq!(
                codes.len(),
                2,
                "unrecognized lint-code line shape (update this scanner): {line}"
            );
            for code in codes {
                checked += 1;
                let foreign: Vec<&str> = lookup_code(code)
                    .iter()
                    .map(|e| e.phase)
                    .filter(|p| *p != "lint")
                    .collect();
                assert!(
                    foreign.is_empty(),
                    "lint code {code} is already assigned in CODE_TABLE to {foreign:?} — \
                     pick a free number for the lint (see B-2026-08-18-17)"
                );
            }
        }
        // The scanner silently passing because it matched nothing is the one
        // way this test could rot into a no-op.
        assert!(
            checked >= 2,
            "expected to find at least one lint code pair to check, found {checked}"
        );
    }

    /// Every lint code the emitter mints must BE in the catalogue
    /// (B-2026-08-18-28).
    ///
    /// The sibling test above pins that a lint code takes nobody else's
    /// number; this pins that it has a number anyone can look up. Both were
    /// needed: `karac explain E0278` and `E0259` answered "not in the
    /// catalogue yet" for codes the compiler actively emits under
    /// `-D <lint>`, because `CODE_TABLE`'s rows were all built from
    /// `TypeErrorKind` and there was no row shape for a lint that is not one.
    /// The lints that ARE `TypeErrorKind`s (`Deprecated` E0245, `UnstableApi`
    /// E0255) were catalogued all along, which is why the hole was easy to
    /// miss by inspection.
    ///
    /// Scans the same emitter line shape, so a lint added later with a fresh
    /// code pair fails here until it is catalogued.
    #[test]
    fn every_lint_code_is_catalogued() {
        const DIAG_JSON_SRC: &str = include_str!("diag_json.rs");
        let mut checked = 0usize;
        for line in DIAG_JSON_SRC.lines() {
            let Some(rest) = line.trim().strip_prefix("code: if is_error {") else {
                continue;
            };
            let codes: Vec<&str> = rest
                .split('"')
                .skip(1)
                .step_by(2)
                .filter(|s| s.len() == 5)
                .collect();
            for code in codes {
                checked += 1;
                let rows = lookup_code(code);
                assert!(
                    rows.iter().any(|e| e.phase == "lint"),
                    "lint code {code} is emitted but has no `lint` row in CODE_TABLE — \
                     `karac explain {code}` would answer \"not in the catalogue yet\" \
                     for a code the compiler mints (see B-2026-08-18-28)"
                );
            }
        }
        assert!(
            checked >= 2,
            "expected to find at least one lint code pair to check, found {checked}"
        );
    }

    /// The table's `class` column must agree with the typechecker's
    /// own `class_for_type_error_kind` — that function is the emitter's
    /// source of truth for the JSON `class` field, so any disagreement
    /// means `explain` describes a diagnostic differently from how the
    /// compiler reports it.
    ///
    /// Covers every typecheck row whose kind carries a class, plus a
    /// sample of the unclassified ones. `class_for_type_error_kind` is
    /// an exhaustive match, so a newly added kind breaks *it* at compile
    /// time; this test catches the case where the kind is classified
    /// there but the table here was not updated to match.
    #[test]
    fn code_table_class_matches_typechecker() {
        let pairs: &[(&str, K)] = &[
            ("E0200", K::TypeMismatch),
            ("E0201", K::UndefinedField),
            ("E0202", K::WrongNumberOfArgs),
            ("E0203", K::MissingField),
            ("E0204", K::ExtraField),
            ("E0205", K::NonExhaustiveMatch),
            ("E0206", K::NotCallable),
            ("E0207", K::NotAStruct),
            ("E0208", K::InvalidBinaryOp),
            ("E0209", K::InvalidUnaryOp),
            ("E0210", K::InvalidCast),
            ("E0211", K::ConditionNotBool),
            ("E0212", K::BranchTypeMismatch),
            ("E0213", K::ReturnTypeMismatch),
            ("E0214", K::InvalidTupleIndex),
            ("E0215", K::LabelMismatch),
            ("E0216", K::NonContiguousLabels),
            ("E0217", K::InvalidPipePlaceholder),
            ("E0218", K::MissingMutMarker),
            ("E0219", K::InvalidMutMarker),
            ("E0220", K::UnsupportedNumericSuffix),
            ("E0221", K::PrivateTypeInPublicSignature),
            ("E0222", K::RefutablePattern),
            ("E0229", K::MissingSupertrait),
            ("E0232", K::TraitBoundNotSatisfied),
            ("E0233", K::AmbiguousAssocFn),
            ("E0234", K::CannotInferAssocFn),
            ("E0235", K::OnceFnIntoFnSlot),
            ("E0236", K::NoMethodFound),
            ("W0237", K::UnreachableArm),
            ("W0238", K::RefinementDomainTooWide),
            ("E0238", K::CannotInferTypeParam),
            ("E0239", K::AmbiguousMethod),
            ("E0240", K::ConflictingImpl),
            ("W0244", K::UnknownLint),
            ("E0245", K::Deprecated),
            ("E0247", K::ForbiddenLintAllow),
            ("E0248", K::ExpectOnUnfulfilled),
            ("E0249", K::UnfulfilledLintExpectation),
            ("E0255", K::UnstableApi),
            ("E0261", K::AtBindingDoubleConsume),
            ("E0262", K::TypeAliasBoundNotSatisfied),
            ("E0268", K::StringNotIndexable),
            ("E0274", K::IteratorNotIndexable),
            ("E0275", K::TypeNotIndexable),
            ("E0276", K::NilCoalesceNotWrapped),
            ("E0277", K::OptionalChainNotOption),
            ("E0270", K::AtomicMissingOrdering),
            ("E0271", K::ImplTraitMultipleWitnesses),
            ("E0272", K::AtomicInvalidInnerType),
            ("E0273", K::PatternScrutineeMismatch),
            ("E0801", K::GpuNotSafe),
        ];
        for (code, kind) in pairs {
            let row = lookup_code(code)
                .into_iter()
                .find(|e| e.phase == "typecheck")
                .unwrap_or_else(|| panic!("{code} missing a typecheck row in CODE_TABLE"));
            assert_eq!(
                row.class,
                crate::typechecker::class_for_type_error_kind(kind),
                "{code} ({}) disagrees with class_for_type_error_kind",
                row.kind,
            );
        }
    }

    #[test]
    fn known_code_resolves_to_its_class() {
        let entries = lookup_code("E0200");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].phase, "typecheck");
        assert_eq!(entries[0].class, Some(DiagnosticClass::TypeMismatch));
        let text = render_code_text("E0200", entries);
        assert!(text.contains("diagnostic code: E0200"));
        assert!(text.contains("TYPE_MISMATCH"));
    }

    /// A code with no class must say so plainly instead of printing
    /// the vacuous `OTHER` page as though it were an explanation.
    #[test]
    fn unclassified_code_says_so_rather_than_printing_other() {
        let entries = lookup_code("E0250");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].class, None);
        let text = render_code_text("E0250", entries);
        assert!(text.contains("ModuleBindingEffectfulInit"));
        assert!(text.contains("no class page yet"));
    }

    /// The multi-row renderer is kept even though no code collides
    /// today (B-2026-07-27-14 split the bands). It is the safety net:
    /// if a future commit re-introduces a shared code, `explain` still
    /// tells the truth — names every phase and points at the `phase`
    /// field — instead of silently showing whichever row sorts first.
    /// Fed a synthetic pair rather than a real lookup, precisely
    /// because a real collision must no longer exist.
    #[test]
    fn collision_render_names_every_phase() {
        let entries = vec![res("PrivateItemAccess", None), ty("RefutablePattern", None)];
        let text = render_code_text("E0222", entries);
        assert!(text.contains("more than one phase"));
        assert!(text.contains("PrivateItemAccess"));
        assert!(text.contains("RefutablePattern"));
    }

    /// The former collision codes now resolve to exactly one phase.
    #[test]
    fn former_collision_codes_resolve_to_one_phase_each() {
        for (code, phase, kind) in [
            ("E0222", "typecheck", "RefutablePattern"),
            ("E0238", "typecheck", "CannotInferTypeParam"),
            ("E0239", "typecheck", "AmbiguousMethod"),
            ("E0240", "typecheck", "ConflictingImpl"),
            ("E0241", "typecheck", "NonExhaustiveCrossPackageLiteral"),
            ("E0242", "typecheck", "NonExhaustiveCrossPackageMatch"),
            ("E0243", "typecheck", "NonExhaustiveCrossPackagePattern"),
            ("E0245", "typecheck", "Deprecated"),
            ("E0111", "resolve", "PrivateItemAccess"),
            ("E0116", "resolve", "ContinueOnBlockLabel"),
            ("E0117", "resolve", "NonExhaustiveInvalidTarget"),
            ("E0118", "resolve", "TrackCallerInvalidTarget"),
            ("E0119", "resolve", "DeprecatedOnImpl"),
            ("E0120", "resolve", "DeprecatedOnField"),
            ("E0121", "resolve", "UnknownAttribute"),
            ("E0123", "resolve", "UnknownProfile"),
        ] {
            let rows = lookup_code(code);
            assert_eq!(rows.len(), 1, "{code} should resolve to one row");
            assert_eq!(rows[0].phase, phase, "{code} phase");
            assert_eq!(rows[0].kind, kind, "{code} kind");
        }
    }

    /// `E0001` used to be the stock example of an uncatalogued code — it is
    /// the parser's `Syntax` error, and the parse band was outside the
    /// catalogue's scope. B-2026-08-20-31 closed the numbering, so the
    /// example had to become a number nothing mints. The message for one of
    /// those still has to point at the class surface, because the symbolic
    /// `E_*` codes genuinely are only reachable that way.
    #[test]
    fn uncatalogued_code_points_at_the_class_surface() {
        assert!(
            !lookup_code("E0001").is_empty(),
            "E0001 is the parser's Syntax error and is catalogued"
        );
        assert!(lookup_code("E0997").is_empty());
        let msg = unknown_code_message("E0997");
        assert!(msg.contains("not a diagnostic code this compiler mints"));
        assert!(msg.contains("--class"));
    }

    #[test]
    fn non_code_token_is_rejected_as_such() {
        let msg = unknown_code_message("banana");
        assert!(msg.contains("is not a diagnostic code"));
    }

    #[test]
    fn code_json_envelope_is_well_formed() {
        let json = render_code_json("E0200", lookup_code("E0200"));
        assert!(json.starts_with("{\"kind\":\"diagnostic_code\",\"code\":\"E0200\""));
        assert!(json.contains("\"class\":\"TYPE_MISMATCH\""));
        assert!(json.ends_with("]}"));
    }
}
