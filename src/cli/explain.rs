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
        ExplainTarget::Item { file, item } => {
            match super::explain_item::render_to_string(file, item, format) {
                Ok(text) => print!("{text}{}", if text.ends_with('\n') { "" } else { "\n" }),
                Err(e) => {
                    eprintln!("error: {e}");
                    process::exit(1);
                }
            }
        }
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
            if let Some(page) = lint_page(name, format) {
                return Ok(page);
            }
            if let Some(c) = concept_id(name) {
                return Ok(match format {
                    ExplainFormat::Text => render_concept_id_text(c),
                    ExplainFormat::Json => render_concept_id_json(c),
                });
            }
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
        ExplainTarget::Item { file, item } => {
            super::explain_item::render_to_string(file, item, format)
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
    // A lint's name, as `lint_name` and `-A` give it (B-2026-11-16-299).
    if let Some(page) = lint_page(name, format) {
        print!("{page}{}", if page.ends_with('\n') { "" } else { "\n" });
        return;
    }
    // A diagnostic's `concept` id (`ownership/move-semantics`): where the
    // spec says it, and the codes that carry it.
    if let Some(c) = concept_id(name) {
        match format {
            ExplainFormat::Text => print!("{}", render_concept_id_text(c)),
            ExplainFormat::Json => println!("{}", render_concept_id_json(c)),
        }
        return;
    }
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
/// **Scope is by BAND, not by phase.** The table covers every numbered
/// band — parse `E00xx`, resolve `E01xx`, typecheck `E02xx` / `W02xx`,
/// the typecheck lint band `W03xx` / `E03xx` (one code per lint,
/// B-2026-11-16-299), effect `E04xx`, ownership `E05xx` / `N05xx`,
/// provider-escape `E0600`, target/GPU `E08xx` — and the two loose
/// numbers `E0223` / `E0227`, COMPLETELY: every code any emitter mints
/// in them.
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
    ("E0127", res("BindingNameClass", None)),
    // No kind reports this any more: layout blocks are refused as deferred
    // (B-2026-11-16-257); the entry keeps `karac explain E0151` answering.
    ("E0151", res("LayoutReprCOnPubStruct", None)),
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
    ("E0110", res("TraitEffectVarNotAllowed", None)),
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
    ("E0125", res("EffectVarKindMismatch", None)),
    ("E0126", res("EffectArgsMissingInPublicSignature", None)),
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
        ty("InvalidBinaryOp", Some(DiagnosticClass::InvalidBinaryOp)),
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
    ("E0246", ty("OrphanImpl", None)),
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
    ("E_MODULE_LET", ty("ModuleLet", None)),
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
    // B-2026-09-20-59 — `pub main`, `main(x: T)`, or `main[T]()`.
    ("E0286", ty("MainSignature", None)),
    // B-2026-10-04-61 — `f() = v`, `(a, b) = (b, a)`, `a + 1 = v`.
    ("E0287", ty("AssignTargetNotPlace", None)),
    ("E0288", ty("ImplModeStrongerThanTrait", None)),
    ("E0289", ty("MoveOutOfNoMovePlace", None)),
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
    // B-2026-08-26-21 — `let t = v[i]` where the element is not `Copy`.
    // An index expression is a BORROW (`fn index(ref self, ..) -> ref T`),
    // so reading it in value position obeys the `*r` rule: `T: Copy`.
    //
    // E0285, not E0282: this rule was written against a tree where E0282 was
    // free, and a sibling took it for `PreludeShadow` while the patch sat
    // parked. `code_table_has_no_duplicate_rows` caught the collision — the
    // second time a parked diagnostic collided on a number, so pick the code as
    // late as possible, the same discipline the ledger uses for bug ids.
    // B-2026-10-19-61: renamed from E0285 to the code core-semantics.md §3.7
    // gives it.
    (
        "E_INDEX_MOVE_NON_COPY",
        ty("IndexMoveNonCopy", Some(DiagnosticClass::TypeMismatch)),
    ),
    (
        "E0283",
        ty("RefOperandUnsupported", Some(DiagnosticClass::TypeMismatch)),
    ),
    // B-2026-08-26-15 — `LazyLock.new` closure captured a function local.
    ("E0284", ty("LazyLockRuntimeCapture", None)),
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
    ("E0128", ty("BindingNameClass", None)),
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
    (
        "W0282",
        ty("PreludeShadow", Some(DiagnosticClass::LintWarning)),
    ),
    (
        "W0290",
        ty("ReturnClauseOnFn", Some(DiagnosticClass::LintWarning)),
    ),
    (
        "W0291",
        ty("RedundantCast", Some(DiagnosticClass::LintWarning)),
    ),
    ("W0295", ty("StyleLint", Some(DiagnosticClass::LintWarning))),
    (
        "W0296",
        ty("UnusedDocComment", Some(DiagnosticClass::LintWarning)),
    ),
    (
        "W0298",
        ty("AcronymCase", Some(DiagnosticClass::LintWarning)),
    ),
    (
        "W0292",
        ty("LaterTrack", Some(DiagnosticClass::LintWarning)),
    ),
    (
        "W0293",
        ty("RedundantAliasBound", Some(DiagnosticClass::LintWarning)),
    ),
    (
        "W0294",
        ty("WeakReadUnwrap", Some(DiagnosticClass::LintWarning)),
    ),
    (
        "E0294",
        ty("WeakReadUnwrap", Some(DiagnosticClass::LintWarning)),
    ),
    (
        "E0293",
        ty("RedundantAliasBound", Some(DiagnosticClass::LintWarning)),
    ),
    (
        "E0292",
        ty("LaterTrack", Some(DiagnosticClass::LintWarning)),
    ),
    (
        "E0291",
        ty("RedundantCast", Some(DiagnosticClass::LintWarning)),
    ),
    ("E0295", ty("StyleLint", Some(DiagnosticClass::LintWarning))),
    (
        "E0296",
        ty("UnusedDocComment", Some(DiagnosticClass::LintWarning)),
    ),
    (
        "E0298",
        ty("AcronymCase", Some(DiagnosticClass::LintWarning)),
    ),
    (
        "E0297",
        ty(
            "BufferedIoInvalidSource",
            Some(DiagnosticClass::TypeMismatch),
        ),
    ),
    (
        "E0290",
        ty("ReturnClauseOnFn", Some(DiagnosticClass::LintWarning)),
    ),
    (
        "E0282",
        ty("PreludeShadow", Some(DiagnosticClass::LintWarning)),
    ),
    // The catch-all arm of the warning emitter: any `TypeErrorKind` that
    // reaches the warning loop without a code of its own lands here, so this
    // row names the bucket rather than one variant. A warning that deserves
    // its own number should get one — `W0299` showing up in a report is the
    // signal that it has not.
    ("W0299", ty("<unclassified typecheck warning>", None)),
    // One code per lint (B-2026-11-16-299): the typecheck lint band, W03nn
    // warning / E03nn error, in `diag_json::LINT_CODES` order.
    ("W0300", lint_own("redundant_clone")),
    ("E0300", lint_own("redundant_clone")),
    ("W0301", lint_own("unused_mut")),
    ("E0301", lint_own("unused_mut")),
    ("W0302", lint_own("irrefutable_let_pattern")),
    ("E0302", lint_own("irrefutable_let_pattern")),
    ("W0303", lint_own("unreachable_errdefer")),
    ("E0303", lint_own("unreachable_errdefer")),
    ("W0304", lint_own("counter_while_loop")),
    ("E0304", lint_own("counter_while_loop")),
    ("W0305", lint_own("manual_compound_assign")),
    ("E0305", lint_own("manual_compound_assign")),
    ("W0306", lint_own("needless_return")),
    ("E0306", lint_own("needless_return")),
    ("W0307", lint_own("redundant_literal_method")),
    ("E0307", lint_own("redundant_literal_method")),
    ("W0308", lint_own("redundant_ref_param")),
    ("E0308", lint_own("redundant_ref_param")),
    ("W0309", lint_own("redundant_cast")),
    ("E0309", lint_own("redundant_cast")),
    ("W0310", lint_own("lossless_cast")),
    ("E0310", lint_own("lossless_cast")),
    ("W0311", lint_own("f16_software_emulated")),
    ("E0311", lint_own("f16_software_emulated")),
    ("W0312", lint_own("map_value_clone_reinsert")),
    ("E0312", lint_own("map_value_clone_reinsert")),
    ("W0313", lint_own("borrow_projection_copy")),
    ("E0313", lint_own("borrow_projection_copy")),
    ("W0314", lint_own("for_element_drop_copy")),
    ("E0314", lint_own("for_element_drop_copy")),
    ("W0315", lint_own("partial_move_of_drop_enum")),
    ("E0315", lint_own("partial_move_of_drop_enum")),
    ("W0316", lint_own("partial_move_of_drop_struct")),
    ("E0316", lint_own("partial_move_of_drop_struct")),
    ("W0317", lint_own_cp("undocumented_unsafe")),
    ("E0317", lint_own_cp("undocumented_unsafe")),
    ("W0318", lint_own_cp("ffi_float_eq")),
    ("E0318", lint_own_cp("ffi_float_eq")),
    ("W0319", lint_own_cp("ambiguous_not_comparison")),
    ("E0319", lint_own_cp("ambiguous_not_comparison")),
    ("E0801", ty("GpuNotSafe", None)),
    ("E0803", ty("ReprTransparentInvalid", None)),
    ("E0804", ty("DiscriminantInvalid", None)),
    ("E0805", ty("ExternSignatureInvalid", None)),
    ("E0806", ty("ReprArgumentInvalid", None)),
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
    // The whole band: `ParseErrorKind::code` mints exactly these six.
    ("E0001", parse("Syntax")),
    ("E0002", parse("UnexpectedToken")),
    ("E0003", parse("ReservedKeyword")),
    ("E0004", parse("RawIdentNotAllowed")),
    ("E0005", parse("ReservedSyntax")),
    ("E0006", parse("DuplicateLintLevel")),
    // ── the spec's symbolic names ───────────────────────────────
    //
    // design.md and the text renderer name these diagnostics by a symbolic
    // code (`error[E_NOT_CROSS_TASK]: …`) while the JSON record carries the
    // number, so `karac explain` answers both spellings (B-2026-11-05-17).
    ("E_DUPLICATE_LINT_LEVEL", parse("DuplicateLintLevel")),
    ("E_UNKNOWN_ATTRIBUTE", res("UnknownAttribute", None)),
    (
        "E_FORBIDDEN_LINT_ALLOW",
        ty("ForbiddenLintAllow", Some(DiagnosticClass::LintWarning)),
    ),
    (
        "E_EXPECT_ON_UNFULFILLED",
        ty("ExpectOnUnfulfilled", Some(DiagnosticClass::LintWarning)),
    ),
    ("E_NOT_CROSS_TASK", ty("CrossTaskUnsafeCapture", None)),
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
    // B-2026-08-30-29 — `class: None` restates `class_for_effect_error_kind`,
    // which leaves this kind deliberately unclassified: no published
    // DiagnosticClass describes "a destructor that can panic", and the
    // `code_table_class_matches_the_effect_and_ownership_classifiers` test
    // holds the two in lockstep.
    ("E0417", eff("DropBodyMayPanic", None)),
    (
        "E0418",
        eff("ParBranchConflict", Some(DiagnosticClass::EffectConflict)),
    ),
    // B-2026-10-20-7: `E_NO_PROVIDER` (core semantics §14.6). Unclassified
    // for the reason `DropBodyMayPanic` is: no published class describes a
    // call whose resource nothing binds.
    ("E0419", eff("NoProvider", None)),
    // `E_STATEFUL_DEFAULT` (core semantics §14.14), unclassified like
    // `NoProvider`.
    ("E0420", eff("StatefulDefault", None)),
    // `E_PROVIDER_UNSUPPORTED` (core semantics §14.12) and a handle method
    // using an overridden resource (§14.11), unclassified like `NoProvider`.
    ("E0421", eff("ProviderUnsupported", None)),
    ("E0422", eff("HandleUsesOverride", None)),
    // `E_DYN_PROVIDER` (design.md §9), unclassified like `NoProvider`.
    ("E0423", eff("DynProvider", None)),
    // A provider held by a `TaskGroup` task (core semantics §14.9), the
    // tasks' counterpart of `ParBranchConflict`.
    (
        "E0424",
        eff(
            "TaskProviderConflict",
            Some(DiagnosticClass::EffectConflict),
        ),
    ),
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
    (
        "E0513",
        own(
            "MovedFieldRefilledAcrossCall",
            Some(DiagnosticClass::OwnershipMoveAfterUse),
        ),
    ),
    (
        "E0514",
        own(
            "RcFallbackOfDropType",
            Some(DiagnosticClass::OwnershipMoveAfterUse),
        ),
    ),
    (
        "E0515",
        own(
            "MoveOutOfPlace",
            Some(DiagnosticClass::OwnershipMoveAfterUse),
        ),
    ),
    // design.md §10: a cleanup block never captures by move (B-2026-11-06-28).
    (
        "E0522",
        own(
            "MoveInCleanupBlock",
            Some(DiagnosticClass::OwnershipMoveAfterUse),
        ),
    ),
    // v2 core: borrow conflicts the checker finds before MIR borrowck runs
    // under `check`; MIR borrowck uses the same codes.
    (
        "E0516",
        ty(
            "BorrowConflict",
            Some(DiagnosticClass::OwnershipBorrowConflict),
        ),
    ),
    (
        "E0517",
        ty(
            "WriteThroughSharedRef",
            Some(DiagnosticClass::OwnershipBorrowConflict),
        ),
    ),
    // v2 core §9.6: function kinds and the removed capture prefixes.
    (
        "E0518",
        ty("FnKindMismatch", Some(DiagnosticClass::TypeMismatch)),
    ),
    (
        "E0519",
        ty("CapturePrefixRemoved", Some(DiagnosticClass::TypeMismatch)),
    ),
    // v2 core: library items the core removes (§11.7 `collect_all`).
    (
        "E0520",
        ty("RemovedInCore", Some(DiagnosticClass::TypeMismatch)),
    ),
    // v2 core: a variant built with its bare name (B-2026-10-22-46).
    (
        "E0521",
        ty(
            "BareVariantConstructor",
            Some(DiagnosticClass::TypeMismatch),
        ),
    ),
    // B-2026-10-20-8: the TLS server and WebSocket keep their services-track
    // (M4a) placement.
    (
        "E_DEFERRED_M4A",
        ty("DeferredToM4a", Some(DiagnosticClass::TypeMismatch)),
    ),
    // B-2026-10-22-21: features archived with the legacy backend.
    (
        "E_ARCHIVED",
        ty("ArchivedWithLegacy", Some(DiagnosticClass::TypeMismatch)),
    ),
    // B-2026-10-31-23: a later track's feature (struct `invariant`).
    (
        "E_DEFERRED_TRACK",
        ty("DeferredToLaterTrack", Some(DiagnosticClass::TypeMismatch)),
    ),
    // B-2026-11-16-298: deferred.md's effect groups.
    (
        "E_DEFERRED_EFFECT_GROUP",
        ty("DeferredEffectGroup", Some(DiagnosticClass::TypeMismatch)),
    ),
    // B-2026-11-16-257: deferred.md's layout blocks.
    (
        "E_DEFERRED_LAYOUT_BLOCK",
        ty("DeferredLayoutBlock", Some(DiagnosticClass::TypeMismatch)),
    ),
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
    // ── symbolic codes (B-2026-11-13-20) ────────────────────────
    //
    // Every `E_*` code minted in src/ has a row, held there by
    // `every_minted_symbolic_code_is_catalogued`. Front-end codes restate
    // the error kind they are reported under.
    ("E_FN_ANONYMOUS_PARAM", parse("Syntax")),
    ("E_TRAIT_METHOD_ANONYMOUS_PARAM", parse("Syntax")),
    ("E_MARKER_TRAIT_HAS_ITEM", parse("Syntax")),
    ("E_MARKER_TRAIT_HAS_METHOD", parse("Syntax")),
    ("E_TEST_BLOCK_NOT_TOP_LEVEL", parse("Syntax")),
    (
        "E_CODEGEN_HINT_INVALID_POSITION",
        res("CodegenHintInvalidTarget", None),
    ),
    (
        "E_CODEGEN_HINT_ON_EXTERN_DECL",
        res("CodegenHintOnExternDecl", None),
    ),
    (
        "E_QUERY_RESOLUTION_CONFLICT",
        res("QueryResolutionConflict", None),
    ),
    (
        "E_UNION_NON_EXHAUSTIVE_FORBIDDEN",
        res("UnionNonExhaustiveForbidden", None),
    ),
    (
        "E_DEFAULT_ATTRIBUTE_INVALID_POSITION",
        res("DefaultAttributeInvalidPosition", None),
    ),
    (
        "E_DEFAULT_ATTRIBUTE_WITHOUT_DERIVE",
        res("DefaultAttributeWithoutDerive", None),
    ),
    (
        "E_HELPER_ATTRIBUTE_INVALID_POSITION",
        res("HelperAttributeInvalidPosition", None),
    ),
    ("E_PANIC_HANDLER", res("PanicHandlerInvalid", None)),
    ("E_TEST_OUTSIDE_TEST_FILE", res("TestOutsideTestFile", None)),
    (
        "E_MALFORMED_ATTRIBUTE_ARGS",
        res("MalformedAttributeArgs", None),
    ),
    (
        "E_TEST_ATTR_INVALID_TARGET",
        res("TestAttributeInvalidTarget", None),
    ),
    ("E_TEST_REQUIRES", res("TestRequiresInvalid", None)),
    ("E_EXTERN_REF_PARAM", ty("ExternSignatureInvalid", None)),
    ("E_EXTERN_REF_RETURN", ty("ExternSignatureInvalid", None)),
    (
        "E_PTR_CONST_REQUIRES_PLACE",
        ty("InvalidUnaryOp", Some(DiagnosticClass::InvalidUnaryOp)),
    ),
    (
        "E_PTR_MUT_REQUIRES_PLACE",
        ty("InvalidUnaryOp", Some(DiagnosticClass::InvalidUnaryOp)),
    ),
    (
        "E_STDLIB_VARIANCE_IMPLICIT",
        ty("TypeMismatch", Some(DiagnosticClass::TypeMismatch)),
    ),
    (
        "E_VARIANCE_DECLARATION_INCONSISTENT",
        ty("TypeMismatch", Some(DiagnosticClass::TypeMismatch)),
    ),
    (
        "E_NON_EXHAUSTIVE_CROSS_PACKAGE_PATTERN",
        ty("NonExhaustiveCrossPackagePattern", None),
    ),
    (
        "E_SLICE_FROM_TEMPORARY_ESCAPES",
        own("SliceFromTemporaryEscapes", None),
    ),
    (
        "E_SLICE_BORROW_CONFLICT",
        own(
            "SliceBorrowConflict",
            Some(DiagnosticClass::OwnershipBorrowConflict),
        ),
    ),
    (
        "E_CROSS_BORROW_CONFLICT",
        own(
            "CrossBorrowConflict",
            Some(DiagnosticClass::OwnershipBorrowConflict),
        ),
    ),
    (
        "E_CLOSURE_CAPTURE_BORROW_CONFLICT",
        own(
            "ClosureCaptureBorrowConflict",
            Some(DiagnosticClass::OwnershipBorrowConflict),
        ),
    ),
    ("E_RC_BUDGET_EXCEEDED", own("RcBudgetExceeded", None)),
    (
        "E_CONCURRENT_SHARED_STRUCT",
        own("ConcurrentSharedStruct", None),
    ),
    (
        "E_CONCURRENT_PLAIN_STRUCT",
        own("ConcurrentPlainStruct", None),
    ),
    (
        "E_RC_FALLBACK_ALLOCATES_UNDER_FALLIBLE_PROFILE",
        own("RcFallbackAllocatesUnderFallibleProfile", None),
    ),
    (
        "E_EXCLUSIVE_BORROW_ALIASED_ARGS",
        own(
            "ExclusiveBorrowAliasedArgs",
            Some(DiagnosticClass::OwnershipBorrowConflict),
        ),
    ),
    ("E_REQUIRE_SIMD", phased("simd_check", "SimdFinding")),
    ("E_COMPTIME", phased("comptime", "ComptimeError")),
    // ── codes written into the message (B-2026-11-14-10) ────────
    //
    // A message that opens with `error[E_NAME]:` prints that code as its
    // headline. Each row restates the kind the code is reported under; a
    // code minted from two places names the one most sites use.
    (
        "E_AMBIGUOUS_ASSOC_TYPE",
        ty("TypeMismatch", Some(DiagnosticClass::TypeMismatch)),
    ),
    (
        "E_ASSIGN_TARGET_NOT_PLACE",
        ty("AssignTargetNotPlace", None),
    ),
    ("E_ATTRIBUTE_NOT_VALID_ON_STATEMENT", parse("Syntax")),
    ("E_ATTR_ON_ASSOC_TYPE_DECL", parse("Syntax")),
    (
        "E_AT_BINDING_DOUBLE_CONSUME",
        ty(
            "AtBindingDoubleConsume",
            Some(DiagnosticClass::OwnershipBorrowConflict),
        ),
    ),
    (
        "E_BARE_TRAIT_TYPE",
        ty("TypeMismatch", Some(DiagnosticClass::TypeMismatch)),
    ),
    (
        "E_CHAR_AS_NARROW_INT",
        ty("InvalidCast", Some(DiagnosticClass::InvalidCast)),
    ),
    // B-2026-11-15-20: minted in typechecker.rs past its out-of-line
    // `#[cfg(test)] mod tests;`, which the guard below used to stop at.
    (
        "E_INT_AS_BOOL",
        ty("InvalidCast", Some(DiagnosticClass::InvalidCast)),
    ),
    (
        "E_INT_AS_CHAR",
        ty("InvalidCast", Some(DiagnosticClass::InvalidCast)),
    ),
    (
        "E_FLOAT_AS_BOOL",
        ty("InvalidCast", Some(DiagnosticClass::InvalidCast)),
    ),
    (
        "E_INT_TO_PTR_CAST_FORBIDDEN",
        ty("InvalidCast", Some(DiagnosticClass::InvalidCast)),
    ),
    (
        "E_PTR_TO_INT_CAST_FORBIDDEN",
        ty("InvalidCast", Some(DiagnosticClass::InvalidCast)),
    ),
    (
        "E_REF_TO_RAW_PTR_CAST_FORBIDDEN",
        ty("InvalidCast", Some(DiagnosticClass::InvalidCast)),
    ),
    (
        "E_PTR_CAST_REQUIRES_UNSAFE",
        ty("InvalidCast", Some(DiagnosticClass::InvalidCast)),
    ),
    (
        "E_REFINEMENT_CAST_SOURCE_MISMATCH",
        ty("InvalidCast", Some(DiagnosticClass::InvalidCast)),
    ),
    (
        "E_EMPTY_PREFIX_LITERAL_NEEDS_ANNOTATION",
        ty("TypeMismatch", Some(DiagnosticClass::TypeMismatch)),
    ),
    (
        "E_IMPL_TARGET_NOT_DISTINGUISHABLE",
        ty("TypeMismatch", Some(DiagnosticClass::TypeMismatch)),
    ),
    (
        "E_IMPL_TRAIT_ASSOC_MISMATCH",
        ty("TypeMismatch", Some(DiagnosticClass::TypeMismatch)),
    ),
    (
        "E_IMPL_TRAIT_MISSING_BOUND",
        ty("TypeMismatch", Some(DiagnosticClass::TypeMismatch)),
    ),
    (
        "E_SHAPE",
        ty("TypeMismatch", Some(DiagnosticClass::TypeMismatch)),
    ),
    (
        "E_IMPL_TRAIT_MULTIPLE_WITNESSES",
        ty(
            "ImplTraitMultipleWitnesses",
            Some(DiagnosticClass::TypeMismatch),
        ),
    ),
    ("E_MISSING_NON_EXHAUSTIVE", ty("MissingNonExhaustive", None)),
    ("W_DEPENDENCY_YANKED", phased("dep_resolution", "Warning")),
    // design.md §15: a second `type Foo;` naming the same opaque type warns
    // rather than erroring, because the C side has the final word
    // (B-2026-11-16-239).
    (
        "W_OPAQUE_TYPE_REDECLARED",
        res("OpaqueTypeRedeclared", None),
    ),
    (
        "E_STRING_SLICE_NOT_AT_CHAR_BOUNDARY",
        phased("runtime", "StringSliceNotAtCharBoundary"),
    ),
    ("E_CODEGEN_HINT_ON_CLOSURE", parse("Syntax")),
    ("E_COLD_INLINE_ALWAYS_CONFLICT", parse("Syntax")),
    ("E_COMPTIME_ERROR", phased("comptime", "ComptimeError")),
    (
        "E_COMPTIME_ITER_LIMIT_EXCEEDED",
        phased("comptime", "ComptimeError"),
    ),
    (
        "E_COMPTIME_MODULE_AT_RUNTIME",
        phased("comptime", "ComptimeError"),
    ),
    (
        "E_COMPTIME_NON_FOLDABLE_RESULT",
        phased("comptime", "ComptimeError"),
    ),
    ("E_COMPTIME_PANIC", phased("comptime", "ComptimeError")),
    (
        "E_CONSUMED_SELF_IN_ENSURES",
        ty("TypeMismatch", Some(DiagnosticClass::TypeMismatch)),
    ),
    ("E_CONTINUE_LABEL_BLOCK", res("ContinueOnBlockLabel", None)),
    ("E_CONTRACT_IMPURE", eff("ForbiddenEffectInContract", None)),
    (
        "E_DEFAULT_MULTIPLE_VARIANTS",
        ty("TypeMismatch", Some(DiagnosticClass::TypeMismatch)),
    ),
    (
        "E_DEFAULT_NO_VARIANT_MARKED",
        ty("TypeMismatch", Some(DiagnosticClass::TypeMismatch)),
    ),
    (
        "E_DEFAULT_VARIANT_HAS_PAYLOAD",
        ty("TypeMismatch", Some(DiagnosticClass::TypeMismatch)),
    ),
    ("E_DEPRECATED_DUPLICATE", parse("Syntax")),
    ("E_DEPRECATED_FIELD_NOT_STRING", parse("Syntax")),
    ("E_DEPRECATED_ON_FIELD", res("DeprecatedOnField", None)),
    ("E_DEPRECATED_ON_IMPL", res("DeprecatedOnImpl", None)),
    ("E_DEPRECATED_POSITIONAL_ARG", parse("Syntax")),
    ("E_DEPRECATED_UNKNOWN_FIELD", parse("Syntax")),
    (
        "E_DERIVE_DEFAULT_MISSING_FIELD_DEFAULT",
        ty("TypeMismatch", Some(DiagnosticClass::TypeMismatch)),
    ),
    (
        "E_DISCRIMINANT_OUT_OF_RANGE",
        ty("DiscriminantInvalid", None),
    ),
    ("E_DROP_DUPLICATE_IMPL", ty("ConflictingImpl", None)),
    (
        "E_DROP_SIGNATURE_INVALID",
        ty("TypeMismatch", Some(DiagnosticClass::TypeMismatch)),
    ),
    ("E_DUPLICATE_DISCRIMINANT", ty("DiscriminantInvalid", None)),
    (
        "E_DUPLICATE_MODULE_BINDING",
        res(
            "DuplicateDefinition",
            Some(DiagnosticClass::DuplicateDefinition),
        ),
    ),
    (
        "E_DYN_NOT_DISPATCHABLE",
        ty("TypeMismatch", Some(DiagnosticClass::TypeMismatch)),
    ),
    (
        "E_DYN_REPLACE_THROUGH_VIEW",
        ty("InvalidUnaryOp", Some(DiagnosticClass::InvalidUnaryOp)),
    ),
    ("E_DYN_SECOND_TRAIT", parse("Syntax")),
    ("E_EMPTY_UNION", parse("Syntax")),
    (
        "E_ENUM_NESTED_ENUM_PAYLOAD",
        ty("TypeMismatch", Some(DiagnosticClass::TypeMismatch)),
    ),
    (
        "E_ESCAPING_CLOSURE_NOT_YET",
        ty("TypeMismatch", Some(DiagnosticClass::TypeMismatch)),
    ),
    (
        "E_GAT_BOUND_NOT_SATISFIED",
        ty("TypeMismatch", Some(DiagnosticClass::TypeMismatch)),
    ),
    ("E_GAT_EFFECT_PARAM", parse("Syntax")),
    (
        "E_GAT_PARAM_BOUND_NOT_SATISFIED",
        ty("TypeMismatch", Some(DiagnosticClass::TypeMismatch)),
    ),
    (
        "E_GAT_WHERE_CLAUSE_NOT_SATISFIED",
        ty("TypeMismatch", Some(DiagnosticClass::TypeMismatch)),
    ),
    ("E_GPU_ARGS_NOT_PERMITTED", parse("Syntax")),
    (
        "E_GPU_DISPATCH_ARITY",
        ty(
            "WrongNumberOfArgs",
            Some(DiagnosticClass::WrongNumberOfArgs),
        ),
    ),
    (
        "E_GPU_DISPATCH_BUFFER",
        ty("TypeMismatch", Some(DiagnosticClass::TypeMismatch)),
    ),
    (
        "E_GPU_DISPATCH_KERNEL",
        ty("TypeMismatch", Some(DiagnosticClass::TypeMismatch)),
    ),
    (
        "E_GPU_DISPATCH_UNIFORM",
        ty("TypeMismatch", Some(DiagnosticClass::TypeMismatch)),
    ),
    (
        "E_GPU_DOWNLOAD_ARITY",
        ty(
            "WrongNumberOfArgs",
            Some(DiagnosticClass::WrongNumberOfArgs),
        ),
    ),
    (
        "E_GPU_DOWNLOAD_BUFFER",
        ty("TypeMismatch", Some(DiagnosticClass::TypeMismatch)),
    ),
    ("E_GPU_INVALID_TARGET", res("GpuInvalidTarget", None)),
    (
        "E_GPU_REDUCE_ARITY",
        ty(
            "WrongNumberOfArgs",
            Some(DiagnosticClass::WrongNumberOfArgs),
        ),
    ),
    (
        "E_GPU_REDUCE_BUFFER",
        ty("TypeMismatch", Some(DiagnosticClass::TypeMismatch)),
    ),
    (
        "E_GPU_REDUCE_FIELD",
        ty("TypeMismatch", Some(DiagnosticClass::TypeMismatch)),
    ),
    (
        "E_GPU_REDUCE_KERNEL",
        ty("TypeMismatch", Some(DiagnosticClass::TypeMismatch)),
    ),
    (
        "E_GPU_UPLOAD_ARITY",
        ty(
            "WrongNumberOfArgs",
            Some(DiagnosticClass::WrongNumberOfArgs),
        ),
    ),
    (
        "E_GPU_UPLOAD_BUFFER",
        ty("TypeMismatch", Some(DiagnosticClass::TypeMismatch)),
    ),
    (
        "E_IMPL_TRAIT_ALIAS",
        ty("TypeMismatch", Some(DiagnosticClass::TypeMismatch)),
    ),
    ("E_IMPL_TRAIT_IN_NESTED_POSITION", parse("Syntax")),
    ("E_IMPL_TRAIT_IN_TRAIT_METHOD_ARG", parse("Syntax")),
    ("E_INLINE_HINT_CONFLICT", parse("Syntax")),
    (
        "E_INSTALL_BIN_DIR_UNWRITABLE",
        phased("install", "karac install"),
    ),
    ("E_INSTALL_BUILD_FAILED", phased("install", "karac install")),
    (
        "E_INSTALL_BUILD_SPAWN_FAILED",
        phased("install", "karac install"),
    ),
    ("E_INSTALL_COPY_FAILED", phased("install", "karac install")),
    (
        "E_INSTALL_EXE_UNRESOLVABLE",
        phased("install", "karac install"),
    ),
    (
        "E_INSTALL_GIT_UNSUPPORTED",
        phased("install", "karac install"),
    ),
    ("E_INSTALL_HOME_UNSET", phased("install", "karac install")),
    (
        "E_INSTALL_NO_EXECUTABLE",
        phased("install", "karac install"),
    ),
    ("E_INSTALL_PATH_NOT_DIR", phased("install", "karac install")),
    (
        "E_INSTALL_PATH_NOT_FOUND",
        phased("install", "karac install"),
    ),
    (
        "E_INSTALL_REGISTRY_UNSUPPORTED",
        phased("install", "karac install"),
    ),
    ("E_INTERIOR_NUL_IN_C_STRING", parse("Syntax")),
    (
        "E_INVALID_REFINEMENT_PREDICATE",
        ty("InvalidRefinementPredicate", None),
    ),
    (
        "E_LAYOUT_QUERY_TAKES_NO_ARGS",
        ty(
            "WrongNumberOfArgs",
            Some(DiagnosticClass::WrongNumberOfArgs),
        ),
    ),
    (
        "E_LAYOUT_QUERY_TYPE_ARG_REQUIRED",
        ty(
            "WrongNumberOfArgs",
            Some(DiagnosticClass::WrongNumberOfArgs),
        ),
    ),
    (
        "E_LAZYLOCK_RUNTIME_CAPTURE",
        ty("LazyLockRuntimeCapture", None),
    ),
    ("E_LINT_LEVEL_NON_IDENT_ARG", parse("Syntax")),
    ("E_LINT_LEVEL_NO_ARGS", parse("Syntax")),
    ("E_LINT_LEVEL_ON_HARD_RULE", parse("Syntax")),
    ("E_MAIN_ERR_NOT_DISPLAY", ty("MainErrNotDisplay", None)),
    ("E_MAIN_RETURN_TYPE", ty("MainReturnType", None)),
    ("E_MAIN_SIGNATURE", ty("MainSignature", None)),
    (
        "E_MARKER_IMPL_HAS_METHOD",
        ty("TypeMismatch", Some(DiagnosticClass::TypeMismatch)),
    ),
    (
        "E_MODULE_BINDING_EFFECTFUL_INIT",
        ty("ModuleBindingEffectfulInit", None),
    ),
    (
        "E_MODULE_BINDING_HEAP_TYPE",
        ty("ModuleBindingHeapType", None),
    ),
    (
        "E_MODULE_BINDING_NAMING",
        res("UndefinedName", Some(DiagnosticClass::UndefinedName)),
    ),
    ("E_MULTIVERSION_EMPTY", parse("Syntax")),
    ("E_MULTIVERSION_ON_ASSOC", parse("Syntax")),
    (
        "E_NON_CONSTANT_DISCRIMINANT",
        ty("DiscriminantInvalid", None),
    ),
    (
        "E_NON_EXHAUSTIVE_CROSS_PACKAGE_LITERAL",
        ty("NonExhaustiveCrossPackageLiteral", None),
    ),
    (
        "E_NON_EXHAUSTIVE_CROSS_PACKAGE_MATCH",
        ty("NonExhaustiveCrossPackageMatch", None),
    ),
    (
        "E_NON_EXHAUSTIVE_INVALID_TARGET",
        res("NonExhaustiveInvalidTarget", None),
    ),
    ("E_NO_EFFECT_BAD_VERB", parse("Syntax")),
    ("E_NO_EFFECT_EMPTY", parse("Syntax")),
    (
        "E_NO_EFFECT_INVALID_TARGET",
        res("ProfileInvalidTarget", None),
    ),
    ("E_NO_EFFECT_STRING_VALUE", parse("Syntax")),
    ("E_NO_EFFECT_VIOLATED", eff("NoEffectViolated", None)),
    (
        "E_OFFSET_OF_GENERIC_PARAM",
        ty("TypeMismatch", Some(DiagnosticClass::TypeMismatch)),
    ),
    ("E_OFFSET_OF_INVALID_PATH", parse("Syntax")),
    (
        "E_OFFSET_OF_NON_STRUCT_TARGET",
        ty("TypeMismatch", Some(DiagnosticClass::TypeMismatch)),
    ),
    (
        "E_OFFSET_OF_OPAQUE_TYPE",
        ty("TypeMismatch", Some(DiagnosticClass::TypeMismatch)),
    ),
    (
        "E_OFFSET_OF_UNKNOWN_FIELD",
        ty("UndefinedField", Some(DiagnosticClass::TypeMismatch)),
    ),
    (
        "E_OLD_NOT_CLONE",
        ty("TypeMismatch", Some(DiagnosticClass::TypeMismatch)),
    ),
    (
        "E_OLD_OUTSIDE_ENSURES",
        ty("TypeMismatch", Some(DiagnosticClass::TypeMismatch)),
    ),
    (
        "E_OLD_RESULT",
        ty("TypeMismatch", Some(DiagnosticClass::TypeMismatch)),
    ),
    (
        "E_ONCE_CELL_AT_MODULE_SCOPE",
        ty("CrossTaskUnsafeCapture", None),
    ),
    ("E_ONCE_CELL_IN_PAR_TYPE", ty("ParFieldNotConcurrent", None)),
    (
        "E_OPAQUE_TYPE_NO_FIELDS",
        ty("TypeMismatch", Some(DiagnosticClass::TypeMismatch)),
    ),
    (
        "E_OPAQUE_TYPE_NO_INHERENT_OR_TRAIT_IMPLS",
        ty("TypeMismatch", Some(DiagnosticClass::TypeMismatch)),
    ),
    (
        "E_OPAQUE_TYPE_NO_KNOWN_SIZE",
        ty("TypeMismatch", Some(DiagnosticClass::TypeMismatch)),
    ),
    (
        "E_OPAQUE_TYPE_NO_METHODS",
        ty("NoMethodFound", Some(DiagnosticClass::NoMethodFound)),
    ),
    (
        "E_OPAQUE_TYPE_REQUIRES_INDIRECTION",
        ty("TypeMismatch", Some(DiagnosticClass::TypeMismatch)),
    ),
    (
        "E_PARTIAL_EXPLICIT_DISCRIMINANTS",
        ty("DiscriminantInvalid", None),
    ),
    (
        "E_PAYLOAD_DISCRIMINANT_REQUIRES_REPR",
        ty("DiscriminantInvalid", None),
    ),
    (
        "E_PROFILE_INCOMPATIBLE_EFFECT",
        eff("ProfileIncompatibleEffect", None),
    ),
    (
        "E_PROFILE_INVALID_TARGET",
        res("ProfileInvalidTarget", None),
    ),
    ("E_PROFILE_NAMED_ARG", parse("Syntax")),
    ("E_PROFILE_NON_IDENT_ARG", parse("Syntax")),
    ("E_PROFILE_NO_PROFILES", parse("Syntax")),
    ("E_PROFILE_STRING_VALUE", parse("Syntax")),
    (
        "E_PTR_MUT_REQUIRES_MUTABLE_PLACE",
        ty("InvalidUnaryOp", Some(DiagnosticClass::InvalidUnaryOp)),
    ),
    ("E_RANGE_PATTERN_BOUND_NOT_SIMPLE", parse("Syntax")),
    (
        "E_RAW_POINTER_UNRESOLVED_POINTEE",
        ty(
            "CannotInferTypeParam",
            Some(DiagnosticClass::CannotInferTypeParam),
        ),
    ),
    (
        "E_REASSIGN_TO_IMMUTABLE_MODULE_BINDING",
        ty("ReassignToImmutableModuleBinding", None),
    ),
    (
        "E_REFINEMENT_IMPLICIT_NARROWING",
        ty("TypeMismatch", Some(DiagnosticClass::TypeMismatch)),
    ),
    (
        "E_REFINEMENT_PREDICATE_VIOLATION",
        ty("TypeMismatch", Some(DiagnosticClass::TypeMismatch)),
    ),
    (
        "E_REF_OPERAND_UNSUPPORTED",
        ty("RefOperandUnsupported", Some(DiagnosticClass::TypeMismatch)),
    ),
    ("E_REPR_DEFERRED", ty("ReprArgumentInvalid", None)),
    ("E_REPR_INT_ON_STRUCT", ty("ReprArgumentInvalid", None)),
    (
        "E_REPR_TRANSPARENT_ENUM_NOT_SINGLE_VARIANT",
        ty("ReprTransparentInvalid", None),
    ),
    (
        "E_REPR_TRANSPARENT_EXCLUSIVE",
        ty("ReprTransparentInvalid", None),
    ),
    (
        "E_REPR_TRANSPARENT_REQUIRES_FIELD",
        ty("ReprTransparentInvalid", None),
    ),
    (
        "E_REPR_TRANSPARENT_REQUIRES_SINGLE_FIELD",
        ty("ReprTransparentInvalid", None),
    ),
    (
        "E_REPR_TRANSPARENT_REQUIRES_SIZED",
        ty("ReprTransparentInvalid", None),
    ),
    (
        "E_REPR_TRANSPARENT_UNION_FORBIDDEN",
        ty("ReprTransparentInvalid", None),
    ),
    ("E_REPR_UNKNOWN", ty("ReprArgumentInvalid", None)),
    ("E_REST_PATTERN_DUPLICATE", parse("Syntax")),
    ("E_REST_PATTERN_NOT_LAST", parse("Syntax")),
    (
        "E_RPITIT_INCOMPATIBLE_WITH_DYN",
        ty("TypeMismatch", Some(DiagnosticClass::TypeMismatch)),
    ),
    (
        "E_SECRET_TRAIT_FORBIDDEN",
        ty("TypeMismatch", Some(DiagnosticClass::TypeMismatch)),
    ),
    (
        "E_SHARED_FIELD_MOVE",
        ty("TypeMismatch", Some(DiagnosticClass::TypeMismatch)),
    ),
    (
        "E_STRUCT_LITERAL_SPREAD_UNSUPPORTED",
        ty("TypeMismatch", Some(DiagnosticClass::TypeMismatch)),
    ),
    (
        "E_TAIT_NOT_IMPLEMENTED_YET",
        ty("DeferredTait", Some(DiagnosticClass::TypeMismatch)),
    ),
    ("E_TARGET_FEATURE_EMPTY", parse("Syntax")),
    ("E_TARGET_FEATURE_REQUIRES_UNSAFE", parse("Syntax")),
    ("E_TRACK_CALLER_ARGS_NOT_PERMITTED", parse("Syntax")),
    (
        "E_TRACK_CALLER_INVALID_TARGET",
        res("TrackCallerInvalidTarget", None),
    ),
    (
        "E_TRAIT_ALIAS_NOT_IMPLEMENTED_YET",
        ty("DeferredTraitAlias", Some(DiagnosticClass::TypeMismatch)),
    ),
    (
        "E_TRY_BLOCK_NOT_IMPLEMENTED_YET",
        ty("DeferredTryBlock", Some(DiagnosticClass::TypeMismatch)),
    ),
    (
        "E_TYPE_VALUE_AT_RUNTIME",
        ty("TypeMismatch", Some(DiagnosticClass::TypeMismatch)),
    ),
    (
        "E_UNION_BORROW_REQUIRES_UNSAFE",
        ty("TypeMismatch", Some(DiagnosticClass::TypeMismatch)),
    ),
    (
        "E_UNION_DERIVE_FORBIDDEN",
        ty("TypeMismatch", Some(DiagnosticClass::TypeMismatch)),
    ),
    (
        "E_UNION_DROP_FORBIDDEN",
        ty("TypeMismatch", Some(DiagnosticClass::TypeMismatch)),
    ),
    ("E_UNION_FIELD_MUT_FORBIDDEN", parse("Syntax")),
    (
        "E_UNION_FIELD_NOT_COPY",
        ty("TypeMismatch", Some(DiagnosticClass::TypeMismatch)),
    ),
    ("E_UNION_GENERICS_FORBIDDEN", parse("Syntax")),
    (
        "E_UNION_LITERAL_REQUIRES_ONE_FIELD",
        ty("TypeMismatch", Some(DiagnosticClass::TypeMismatch)),
    ),
    (
        "E_UNION_READ_REQUIRES_UNSAFE",
        ty("TypeMismatch", Some(DiagnosticClass::TypeMismatch)),
    ),
    (
        "E_UNION_REQUIRES_REPR",
        ty("TypeMismatch", Some(DiagnosticClass::TypeMismatch)),
    ),
    ("E_UNION_TUPLE_FORBIDDEN", parse("Syntax")),
    ("E_UNION_WHERE_FORBIDDEN", parse("Syntax")),
    (
        "E_UNKNOWN_ASSOC_TYPE_BINDING",
        ty("TypeMismatch", Some(DiagnosticClass::TypeMismatch)),
    ),
    ("E_UNKNOWN_PROFILE", res("UnknownProfile", None)),
    ("E_UNSTABLE_DUPLICATE", parse("Syntax")),
    ("E_UNSTABLE_POSITIONAL_ARG", parse("Syntax")),
    (
        "E_VARIANCE_USER_DECL_NOT_YET",
        ty("TypeMismatch", Some(DiagnosticClass::TypeMismatch)),
    ),
    ("E_VENDOR_COPY_FAILED", phased("vendor", "karac vendor")),
    (
        "E_WHERE_CLAUSE_ASSOC_TYPE_EQ_NOT_SATISFIED",
        ty("TypeMismatch", Some(DiagnosticClass::TypeMismatch)),
    ),
    (
        "E_WHERE_CLAUSE_PROJECTION_BOUND_NOT_SATISFIED",
        ty("TypeMismatch", Some(DiagnosticClass::TypeMismatch)),
    ),
    // The package manager. `phase` is what its JSON record carries.
    (
        "E_WORKSPACE_DEP_NOT_DECLARED",
        phased("dep_resolution", "DepGraphError::WorkspaceDepNotDeclared"),
    ),
    (
        "E_WORKSPACE_DEP_OUTSIDE_WORKSPACE",
        phased(
            "dep_resolution",
            "DepGraphError::WorkspaceDepOutsideWorkspace",
        ),
    ),
    (
        "E_DEPENDENCY_CYCLE",
        phased("dep_resolution", "DepGraphError::DependencyCycle"),
    ),
    (
        "E_PATH_DEP_NOT_FOUND",
        phased("dep_resolution", "DepGraphError::PathDepNotFound"),
    ),
    (
        "E_OFFLINE_VENDOR_ENTRY_MISSING",
        phased("dep_resolution", "DepGraphError::OfflineVendorEntryMissing"),
    ),
    (
        "E_PATH_DEP_MANIFEST_INVALID",
        phased("dep_resolution", "DepGraphError::PathDepManifestInvalid"),
    ),
    (
        "E_REGISTRY_FETCH_FAILED",
        phased("dep_resolution", "DepGraphError::RegistryFetchFailed"),
    ),
    (
        "E_GIT_FETCH_FAILED",
        phased("dep_resolution", "DepGraphError::GitFetchFailed"),
    ),
    (
        "E_GIT_UNAVAILABLE",
        phased("dep_resolution", "GitFetchError::GitUnavailable"),
    ),
    (
        "E_GIT_NO_MANIFEST",
        phased("dep_resolution", "GitFetchError::NoManifest"),
    ),
    (
        "E_REGISTRY_DEP_UNSUPPORTED",
        phased("dep_resolution", "ResolverError::RegistryDepUnsupported"),
    ),
    (
        "E_GIT_DEP_UNSUPPORTED",
        phased("dep_resolution", "ResolverError::GitDepUnsupported"),
    ),
    (
        "E_DEPENDENCY_VERSION_CONFLICT",
        phased("dep_resolution", "ResolverError::VersionConflict"),
    ),
    (
        "E_DEPENDENCY_SOURCE_CONFLICT",
        phased("dep_resolution", "ResolverError::SourceConflict"),
    ),
    (
        "E_TOOLCHAIN_TOO_OLD",
        phased("dep_resolution", "ResolverError::ToolchainTooOld"),
    ),
    (
        "E_DEPENDENCY_UNSATISFIABLE",
        phased("dep_resolution", "ResolverError::UnsatisfiableGraph"),
    ),
    (
        "E_OFFLINE_NO_VENDOR_DIR",
        phased("dep_resolution", "OfflineNoVendorDir"),
    ),
    (
        "E_PROXY_DISABLED",
        phased("dep_resolution", "ProxyClientError::Disabled"),
    ),
    (
        "E_PROXY_UNREACHABLE",
        phased("dep_resolution", "ProxyClientError::Unreachable"),
    ),
    (
        "E_PROXY_UNAUTHORIZED",
        phased("dep_resolution", "ProxyClientError::Unauthorized"),
    ),
    (
        "E_PROXY_PACKAGE_NOT_FOUND",
        phased("dep_resolution", "ProxyClientError::PackageNotFound"),
    ),
    (
        "E_PROXY_VERSION_NOT_FOUND",
        phased("dep_resolution", "ProxyClientError::VersionNotFound"),
    ),
    (
        "E_PROXY_MALFORMED_RESPONSE",
        phased("dep_resolution", "ProxyClientError::MalformedResponse"),
    ),
    (
        "E_PROXY_NOT_IMPLEMENTED",
        phased("dep_resolution", "ProxyClientError::NotImplemented"),
    ),
    (
        "E_REGISTRY_NO_MATCHING_VERSION",
        phased("dep_resolution", "RegistryFetchError::NoMatchingVersion"),
    ),
    (
        "E_REGISTRY_ONLY_YANKED",
        phased("dep_resolution", "RegistryFetchError::OnlyYankedMatches"),
    ),
    (
        "E_REGISTRY_EXTRACT_FAILED",
        phased("dep_resolution", "RegistryExtractError"),
    ),
    (
        "E_TOOLCHAIN_FILE_READ",
        phased("toolchain_pin", "ToolchainError::FileRead"),
    ),
    (
        "E_TOOLCHAIN_INVALID_TOML",
        phased("toolchain_pin", "ToolchainError::InvalidToml"),
    ),
    (
        "E_TOOLCHAIN_MISSING_VERSION",
        phased("toolchain_pin", "ToolchainError::MissingVersion"),
    ),
    (
        "E_TOOLCHAIN_INVALID_VERSION_TYPE",
        phased("toolchain_pin", "ToolchainError::InvalidVersionType"),
    ),
    (
        "E_TOOLCHAIN_INVALID_VERSION_CONSTRAINT",
        phased("toolchain_pin", "ToolchainError::InvalidVersionConstraint"),
    ),
    (
        "E_TOOLCHAIN_INVALID_TARGETS_TYPE",
        phased("toolchain_pin", "ToolchainError::InvalidTargetsType"),
    ),
    (
        "E_TOOLCHAIN_INVALID_TARGET_ENTRY",
        phased("toolchain_pin", "ToolchainError::InvalidTargetEntry"),
    ),
    (
        "E_TOOLCHAIN_EMPTY_TARGET_ENTRY",
        phased("toolchain_pin", "ToolchainError::EmptyTargetEntry"),
    ),
    (
        "E_TOOLCHAIN_VERSION_MISMATCH",
        phased("toolchain_pin", "ToolchainVersionMismatch"),
    ),
    ("E_LOCKFILE_IO", phased("lockfile", "LockfileError::Io")),
    (
        "E_LOCKFILE_PARSE",
        phased("lockfile", "LockfileError::Parse"),
    ),
    (
        "E_LOCKFILE_SCHEMA_VERSION",
        phased("lockfile", "LockfileError::SchemaVersion"),
    ),
    (
        "E_LOCKFILE_INVALID_SOURCE",
        phased("lockfile", "LockfileError::InvalidSource"),
    ),
    (
        "E_LOCKFILE_INVALID_FIELD",
        phased("lockfile", "LockfileError::InvalidField"),
    ),
    (
        "E_CACHE_HOME_UNSET",
        phased("cache", "CacheError::HomeUnset"),
    ),
    (
        "E_CACHE_UNREADABLE",
        phased("cache", "CacheError::Unreadable"),
    ),
    (
        "E_CACHE_MALFORMED_METADATA",
        phased("cache", "CacheError::MalformedMetadata"),
    ),
    (
        "E_CACHE_WRITE_FAILED",
        phased("cache", "CacheError::WriteFailed"),
    ),
    (
        "E_UPDATE_UNKNOWN_PACKAGE",
        phased("update", "UnknownPackage"),
    ),
    ("E_UPDATE_ROOT_PACKAGE", phased("update", "RootPackage")),
    (
        "E_INSTALL_EMPTY_SPEC",
        phased("install", "InstallSpecError::EmptySpec"),
    ),
    (
        "E_INSTALL_MISSING_VALUE",
        phased("install", "InstallSpecError::MissingValue"),
    ),
    (
        "E_INSTALL_EMPTY_VERSION",
        phased("install", "InstallSpecError::EmptyVersion"),
    ),
    (
        "E_INSTALL_INVALID_VERSION",
        phased("install", "InstallSpecError::InvalidVersion"),
    ),
    (
        "E_INSTALL_INVALID_NAME",
        phased("install", "InstallSpecError::InvalidPackageName"),
    ),
    (
        "E_INSTALL_RESERVED_NAME",
        phased("install", "InstallSpecError::ReservedKeyword"),
    ),
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

/// A lint's own code (B-2026-11-16-299, `diag_json::LINT_CODES`), from a
/// typecheck-phase lint: `kind` is the lint's name, what `-A` / `-D` address.
const fn lint_own(kind: &'static str) -> CodeEntry {
    CodeEntry {
        phase: "typecheck",
        kind,
        class: Some(DiagnosticClass::LintWarning),
    }
}

/// [`lint_own`] for a lint the compile path reports (phase `lint`).
const fn lint_own_cp(kind: &'static str) -> CodeEntry {
    lint(kind)
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

/// A row for a phase with no helper of its own: the package manager's
/// (`dep_resolution`, `toolchain_pin`, `lockfile`, `cache`, `update`,
/// `install`, `vendor`), `simd_check` and `comptime`. `phase` is what the emitted
/// record carries; `kind` names the error type or variant that mints the
/// code. None is classified (B-2026-11-13-20).
const fn phased(phase: &'static str, kind: &'static str) -> CodeEntry {
    CodeEntry {
        phase,
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

/// What a code means and how it is fixed, for the codes a newcomer meets
/// whose class page alone says too little (B-2026-11-07-47). Each entry
/// restates the rule from `docs/design.md` or `docs/core-semantics.md`
/// and names the section, so the prose has a source to be checked against.
const CODE_PROSE: &[(&str, &str)] = &[
    (
        "E_STRING_SLICE_NOT_AT_CHAR_BOUNDARY",
        "A runtime error (panic kind `index_out_of_bounds`): a string was sliced \
         by a byte range (`s[a..b]`) whose ends do not fall between UTF-8 \
         characters, so the slice would cut a character in two. The message \
         reads `byte index N is not a char boundary` and names the first end \
         that cuts one; it quotes none of the text (B-2026-11-16-320). Slice at a boundary (`s.char_indices()` gives each \
         character's byte offset), or work in characters with `s.chars()`.",
    ),
    (
        "W_OPAQUE_TYPE_REDECLARED",
        "A warning (design.md §15, Opaque foreign types): two `unsafe extern` \
         blocks declare the same opaque type `type Foo;`. Both must name the \
         same C type, which only the C side can confirm, so the program still \
         compiles and the first declaration is the one used. Keep one \
         declaration and import it where the others were.",
    ),
    (
        "W_DEPENDENCY_YANKED",
        "A warning from the build: `kara.lock` pins a dependency to a version \
         its registry has since yanked. The build still uses it, so the lock \
         stays reproducible, but a yanked release should not be relied on: \
         move to a version the registry still offers and update the lock.",
    ),
    (
        "E0001",
        "The parser could not read this part of the file as Kāra. The message \
         says what went wrong at the marked position. Common causes are an \
         unclosed bracket or string, a missing `;` or `,`, or syntax from \
         another language (Kāra writes generic arguments as `[T]`, not `<T>`). \
         Fix the first syntax error and check again: later ones in the same \
         region often follow from it.",
    ),
    (
        "E0002",
        "The parser found a token the grammar does not allow at this point; \
         the message names what it expected and what it found (for example \
         \"Expected expression, found `;`\"). The mistake is often just before the marked \
         token: a missing operand, an unclosed `(` or `{`, or a missing `=` or \
         `=>`. Fix the first syntax error and check again: later ones in the \
         same region often follow from it.",
    ),
    (
        "E0205",
        "A `match` must be exhaustive: every value of the scrutinee's type must \
         be matched by some arm (design.md § Pattern exhaustiveness). The \
         message lists what is missing. A guarded arm (`P if cond => ...`) does \
         not count toward exhaustiveness, and a match over integers, floats, `char` or \
         strings needs an arm that matches the rest: `_`, a binding, or a \
         half-open range such as `10..`, since the compiler does not check \
         whether a set of literals covers the whole type. On a `Vec`, the slice \
         patterns `[]` and `[x, ..rest]` together cover every length. Add an arm \
         for each missing case, or a `_ => ...` arm when the rest share one \
         behaviour; on an enum, explicit arms keep a variant added later \
         reported here.",
    ),
    ("E0254", CROSS_TASK_PROSE),
    ("E_NOT_CROSS_TASK", CROSS_TASK_PROSE),
    (
        "E0506",
        "A binding declared with `let`, without `mut`, is assigned again. Only \
         a `let mut` binding may be reassigned: change the declaration to \
         `let mut name` (`karac fix` applies this). In a `match`, `if let` or \
         `while let` arm there is no `mut name` pattern; rebind with \
         `let mut x = x;` inside the arm (design.md § Variable binding rules).",
    ),
    (
        "E0509",
        "A reference or view would outlive the place it borrows. Two shapes \
         report it. (1) A function returns a reference or view that does not \
         borrow from a place its signature allows. By the signature rule \
         (core-semantics §5.4), a method's result borrows from `self` only, and \
         any other function's result borrows from its borrowed parameters of a \
         non-`Copy` type, its `mut ref` parameters and its view parameters; a \
         borrow of a local the function owns, or of an `own` parameter, would \
         dangle once it returns. (2) A view is stored in an escaping closure, a \
         `shared` value or a channel (§5.7, §9.3), where its borrow is not \
         kept. Return or store an owned value instead (drop `ref` from the \
         return type, or `.clone()` / `.to_string()` the view), or take the \
         source as a parameter so the result can borrow from it.",
    ),
    (
        "E0510",
        "A value held in a binding declared without `mut` is modified: a field \
         is assigned (`p.x = 2`), or a method that takes `mut ref self` is \
         called on it (`v.push(1)`). A plain struct's fields can be written \
         only when the value is held in a `let mut` binding or reached through \
         `mut ref` (design.md § Structs). Declare the binding `let mut p` \
         (`karac fix` applies this). When the binding is a reference rather \
         than an owner, the rule is E0517.",
    ),
    (
        "E0516",
        "A borrow is used while another access conflicts with it \
         (core-semantics §5.6). While a `mut ref` borrow of a place is live, \
         nothing else may use that place; while a `ref` borrow of it is live, \
         it may not be written, moved, mutably borrowed or dropped. A borrow is \
         live from where it is created to its last use, distinct fields do not \
         overlap, and any index overlaps the whole collection. Finish with one \
         borrow before starting the other, borrow distinct fields, or copy the \
         value you need out first.",
    ),
    (
        "E0517",
        "Nothing is written through a `ref` (core-semantics §5.9). Parameters \
         borrow by default: a bare parameter `u: U` and a `self` receiver are \
         read-only, and so are a `ref` pattern binding, the item of \
         `for x in c`, a `ref` returned by a call, and a local bound to any of \
         these. Such a place may not be assigned or compound-assigned, mutably \
         borrowed (a `mut ref self` method, an argument marked `mut`, \
         `mut ref name`, `mut ref place`), or moved out of. The exception is a \
         `mut` field of a `shared` value. To change the caller's value, declare \
         the parameter `u: mut ref U` and mark each call site `mut` \
         (`bump(mut u)`); iterate with `c.iter_mut()`; or bind with \
         `mut ref name` from a mutable owner. To change a copy of its own \
         instead, take it `own U` or `.clone()` it.",
    ),
    // B-2026-11-16-296: the codes the corpus emits that had no prose of
    // their own. Each has a program in corpus/diagnostics/<code>/ that
    // `tests/diagnostics_corpus.rs` checks still produces it. E0216 is here
    // because its class paragraph described another error (-297).
    (
        "E0003",
        "A word reserved for a later feature (`yield`, `async`, `try`, `box` \
         and the rest of design.md § Keywords, Reserved for future use) was \
         used as a name. Reserving it now lets the feature arrive without \
         breaking code. Rename the item, or keep the name as a raw \
         identifier: `r#yield`.",
    ),
    (
        "E0107",
        "A `break 'label` or `continue 'label` names a label that no \
         enclosing loop or block carries (design.md § Loops and labels). \
         Labels are visible only inside the loop they name, so a typo or a \
         label on a sibling loop both land here. Use one of the enclosing \
         labels; the message suggests the closest one.",
    ),
    (
        "E0112",
        "An `import` names a module that does not exist (design.md § \
         Modules). A single `.kara` file outside a package can import only \
         the library's `std.*` modules; a program of several files is a \
         package, with a `kara.toml` beside its `src/`, and imports its own \
         modules by their path under `src/`.",
    ),
    (
        "E0115",
        "`#[compiler_builtin]` marks library source that is compiled into \
         karac itself; a user program may not use it. Remove the attribute: \
         an ordinary function needs none.",
    ),
    (
        "E0121",
        "A bare `#[name]` attribute that the compiler does not know \
         (design.md § Attributes lists every one). A misspelt attribute would \
         otherwise do nothing silently, so it is an error. Fix the spelling, \
         or write a tool's attribute with its namespace, such as \
         `#[diagnostic::name]`.",
    ),
    (
        "E0127",
        "A local binding is named in Const or Type style (`MAX`, `Total`), \
         but binding names are Value-class: snake_case (design.md § 3, CN-2). \
         The class of a name tells a reader what it is, so the rule is \
         enforced. Rename it as the message suggests (`max`); `karac fix` \
         applies the rename.",
    ),
    (
        "E0128",
        "A name bound by a pattern (a `match` arm, `if let`, a destructuring \
         `let`) is written in Type style (`Radius`), but pattern bindings are \
         Value-class: snake_case (design.md § 3, CN-2). A capitalized name in \
         a pattern reads as a type or variant, so the rule keeps the two \
         apart. Rename the binding (`radius`).",
    ),
    (
        "E0216",
        "In a call to a standard-library method, the labeled arguments \
         (`name: value`) must form one run: an unlabeled argument may not sit \
         between labeled ones (design.md § 7, Named and default parameters). \
         Write the positional arguments first and the labeled ones after \
         them.",
    ),
    (
        "E0231",
        "A trait method's default body performs an effect that the method's \
         declared effect clause does not admit (design.md § Trait methods and \
         generic calls). The clause is the ceiling every implementation, the \
         default included, must stay under, so callers can rely on it. Add \
         the effect to the method's `with` clause, or remove the call from \
         the default body.",
    ),
    (
        "E0240",
        "A type implements the same trait twice (design.md § Coherence). A \
         trait impl is chosen by its header alone, so two impls whose headers \
         can match the same type overlap, whatever their bounds. Keep one \
         impl; if the two differ on purpose, give one of them its own trait \
         or a wrapper type.",
    ),
    (
        "E0242",
        "A `match` reaches into a `#[non_exhaustive]` enum from another \
         package (the library's `DecodeError`, for example) without an arm \
         for variants added later (design.md § `#[non_exhaustive]`). The \
         owning package may add a variant in a minor release, so the match \
         must say what to do with one. Add a wildcard arm for the uncovered \
         pattern the message names.",
    ),
    (
        "E0250",
        "A module-level constant's initializer is not a compile-time constant \
         expression (design.md § Constants). It is evaluated into the binary \
         as data, so it may use literals, other constants, operators, \
         constructors and fixed-size array literals, but not a function call. \
         Compute the value in `main` and pass it down, or use \
         `LazyLock.new(|| ...)` for a value built once on first use.",
    ),
    (
        "E0251",
        "A module-level constant's type holds heap memory (`String`, `Vec`, \
         or a struct with such a field), but a constant lives in the binary \
         as data (design.md § Constants). Use `Str` for constant text and a \
         fixed-size `Array` for a constant list; `karac fix` rewrites \
         `String` to `Str`.",
    ),
    (
        "E0252",
        "An assignment targets a module-level binding, and module-level \
         bindings are constants (design.md § Constants, No mutable globals). \
         Keep the changing value in a local in `main` and pass it to the \
         functions that need it.",
    ),
    (
        "E0263",
        "A range pattern's bound (`lo..=9`) names a local variable, but the \
         bounds of a range pattern must be known at compile time (design.md § \
         Patterns): a literal, or a module-level integer or `char` constant. \
         A pattern never compares against a runtime value; for that, bind the \
         value and test it in a guard: `n if n >= lo => ...`.",
    ),
    (
        "E0267",
        "`main` returns `Result[(), E]` and `E` does not implement `Display` \
         (design.md § The `main` function and exit codes). When `main` \
         returns `Err(e)`, the runtime prints `Error: {e}` and exits 1, so it \
         needs `Display` to print `e`. Add `#[derive(Display)]` to the error \
         type or write `impl Display for` it.",
    ),
    (
        "E0269",
        "A field of a `shared struct` is assigned, but the field is not \
         declared `mut` (design.md § Shared types). A shared value is reached \
         through several handles at once, so the type says which of its \
         fields may change. Declare the field `mut name: T` in the struct.",
    ),
    (
        "E0286",
        "`main` is declared in a form it may not take (design.md § The `main` \
         function and exit codes): it is never `pub` or generic and takes no \
         parameters. Read command-line arguments with `env.args()`.",
    ),
    (
        "E0287",
        "The left side of an assignment is not a place (core-semantics.md § \
         2): only a binding, a field, an index, a tuple element or a `*` \
         dereference names storage that can be written. A function call or \
         other expression produces a temporary value. Assign to a binding \
         that holds the value instead.",
    ),
    (
        "E0288",
        "An impl method takes its receiver or a parameter in a stronger mode \
         than the trait declares (core-semantics.md § 4.7): here `mut ref \
         self` where the trait says `self`. A caller holding only the trait's \
         promise passes a read-only borrow, so an impl may ask for less than \
         the trait, never more. Change the impl to the trait's mode, or \
         change the trait if every implementation must write.",
    ),
    (
        "E0289",
        "A pattern binds a non-`Copy` part of a value whose type has a `Drop` \
         body, which would move the part out (core-semantics.md § 3.7). The \
         `Drop` body runs on the whole value later and expects every field to \
         be there. Bind the part as `ref name` to borrow it, or `.clone()` it \
         inside the arm; `karac fix` inserts the `ref`.",
    ),
    (
        "E0404",
        "A function value passed to a parameter has an effect that the \
         parameter's function type does not admit (design.md § Function \
         values and effects): the slot says `Fn(..) with reads(Ledger)`, and \
         the closure also writes `Stdout`. The callee only promised its own \
         callers the slot's effects. Remove the effect from the closure, or \
         widen the slot's `with` clause.",
    ),
    (
        "E0416",
        "A function under `#[no_effect(...)]` performs the effect it forbids \
         (design.md § Default-permitted effects, `allocates` as a real-time \
         guarantee). The check follows every callee, so a `collect()` or a \
         `Vec` push anywhere below counts. Rewrite the path to avoid the \
         effect, or remove the attribute if the guarantee is not needed.",
    ),
    (
        "E0419",
        "A call needs a provider for a resource that has no default, and \
         nothing supplies one (core-semantics.md § 14.6). `main` takes no \
         hidden provider, so every use of a provider-rooted resource must run \
         inside `with_provider[R](provider, || ...)`, directly or through its \
         callers. Wrap the call, or give the resource a default: `effect \
         resource R: Trait = value;`.",
    ),
    (
        "E0420",
        "A call reaches a `mut ref self` method of a resource whose default \
         provider has fields (core-semantics.md § 14.14). Outside an override \
         that default is one static value every task shares, so writing it \
         would be a global race. Make the method take `self`, or run the call \
         inside `with_provider[R](provider, || ...)`.",
    ),
    (
        "E0511",
        "A `frozen` handle is passed somewhere that could keep it without the \
         `frozen` guarantee (design.md § Frozen handles): a non-`frozen` \
         parameter, a field, or a closure that outlives the call. `frozen` \
         promises that nothing changes the value while the handle lives, so \
         every place that holds it must be `frozen` too. Declare the \
         receiving parameter `frozen`, or clone the value out of the handle.",
    ),
    (
        "E0512",
        "`freeze` or a `frozen` type was applied to a type that is not \
         `shared` (design.md § Frozen handles). Freezing is how a \
         reference-counted `shared` value becomes safe to read from many \
         tasks; a plain struct is already copied or moved, never aliased, so \
         it has nothing to freeze. Declare the type `shared struct`, or drop \
         `freeze`.",
    ),
    (
        "E0600",
        "A closure that uses a provider-rooted resource leaves the \
         `with_provider` scope that supplies it (core-semantics.md § 14.5), \
         by being returned, stored or sent. Outside the scope the provider no \
         longer exists, so the closure could not run. Call the closure inside \
         the scope, or pass the value it produces out instead.",
    ),
    (
        "E0805",
        "A foreign function in an `unsafe extern \"C\"` block has a parameter \
         or return type that has no C ABI form (design.md § 15): a Kāra \
         reference, `String`, `Vec` or other managed type. Foreign signatures \
         use C-shaped types: integers, floats, raw pointers, `#[repr(C)]` \
         structs and opaque `type T;` handles. Pass a raw pointer and hand \
         over the data with `.as_ptr()` at the call site, as the message \
         shows.",
    ),
    (
        "E_COMPTIME",
        "A derive or other compile-time evaluation failed; the message names \
         what it was computing and why (deferred.md § Comptime; the derives \
         run on it). For `#[derive(Message)]`, every field must be a type the \
         format can encode, so a field whose element type does not derive \
         `Message` is refused. Derive the trait on that type too, or change \
         the field.",
    ),
    (
        "E_HELPER_ATTRIBUTE_INVALID_POSITION",
        "A derive helper attribute (`#[serde(...)]` and the like) is on an \
         item that no derive reads (design.md § Attributes, derive helpers): \
         helpers are read only on a struct, an enum, a field or a variant. \
         Elsewhere it would do nothing, so it is an error. Move it to the \
         type or field it configures, or remove it.",
    ),
    (
        "E_MODULE_LET",
        "A module-level `let mut` declares mutable global state, which Kāra \
         does not have (design.md § Constants, No mutable globals). Create \
         the value in `main` and pass it down to the functions that use it, \
         gathering several into a context struct; a module-level value that \
         never changes is a `const`.",
    ),
    (
        "E_REQUIRE_SIMD",
        "A function marked `#[require_simd]` contains a vector operation that \
         would not lower to SIMD instructions on this target (deferred.md § \
         Portable SIMD); the message names the operation and why, such as a \
         lane count that is not a power of two. Change the vector width to \
         one the target supports, or remove the attribute to accept the \
         scalar loop. `--simd-report` shows each operation's lowering.",
    ),
    (
        "E_SLICE_FROM_TEMPORARY_ESCAPES",
        "A slice is taken of a temporary value (an array literal, a \
         function's result) and outlives the statement (core-semantics.md § \
         5.5). The temporary is freed at the end of the statement, so the \
         slice would point at freed memory. Bind the value first (`let all = \
         [10, 20, 30];`), then slice the binding.",
    ),
    (
        "W0299",
        "A typecheck warning that has no code of its own yet; the record's \
         `lint` field and the message name it, and `#[allow(<lint>)]` \
         silences it. The one that reaches this code today is \
         `f16_software_emulated`: `f16` or `bf16` arithmetic that the target \
         computes by widening to `f32` and rounding back. Compute in `f32` \
         and store in the narrow type.",
    ),
    // B-2026-11-16-303, batch 2: the parse and resolve codes no corpus program
    // produced. Each has a program in corpus/diagnostics/<code>/.
    (
        "E0004",
        "`r#` escapes a reserved word so it can be used as a name, but \
         `self`, `Self`, `super` and `crate` are structural markers rather \
         than reservable keywords, and `r#` does not apply to them \
         (design.md § Raw identifiers). Pick another name, such as `this` \
         or `own_self`.",
    ),
    (
        "E0005",
        "The syntax is recognized and reserved for a later release rather \
         than refused as wrong. Here it is an `extern` calling convention: \
         0.8 implements `\"C\"` and `\"C-unwind\"`, and keeps \
         `\"stdcall\"`, `\"fastcall\"`, `\"win64\"` and `\"sysv64\"` for \
         later (deferred.md § Calling conventions). Use `extern \"C\"`, \
         which is the platform's C convention.",
    ),
    (
        "E0006",
        "A lint attribute (`#[allow(...)]`, `#[warn(...)]`, `#[deny(...)]`) \
         names the same lint twice (design.md § Lint levels). The second \
         mention changes nothing and usually marks an editing slip. Name \
         each lint once.",
    ),
    (
        "E0102",
        "A name the language uses for itself was declared as an item or \
         binding: `Fn` names the function type constructor and \
         `split_by_variant` is a keyword inside layout blocks (design.md § \
         Function types and kinds). Rename the item.",
    ),
    (
        "E0106",
        "A layout block names a field that the struct does not have \
         (deferred.md § Layout Blocks). Layout blocks are deferred, so this \
         compiler refuses every one as E_DEFERRED_LAYOUT_BLOCK and no longer \
         produces this code (B-2026-11-16-257). When layout blocks land, each \
         `group` lists fields of the element type, and every field must be \
         one the struct declares.",
    ),
    (
        "E0108",
        "An `impl` of a trait that the compiler supplies itself was written \
         by hand. `Copy` comes only from `#[derive(Copy, Clone)]`, which \
         checks that every field is `Copy` (design.md § `Copy`), and \
         `CrossTask` is computed from the type's fields. Remove the impl \
         and use the derive, or for `CrossTask`, make the type's `shared` \
         parts `sync` types.",
    ),
    (
        "E0109",
        "`impl Into[U] for T` was written by hand, but `Into` is derived \
         from `From` by a blanket impl, so a second path would make \
         `x.into()` ambiguous (design.md § Conversion traits). Write `impl \
         From[T] for U` instead; `x.into()` then works through it.",
    ),
    (
        "E0110",
        "A trait declares an effect variable (`trait Sink[with E]`), which \
         0.8 does not allow (design.md § Effect variables). A trait \
         method's effects are a ceiling its implementations share, so a \
         variable belongs on the method that uses it or on the impl block: \
         `fn put[with E](self, f: Fn() with E) with E`.",
    ),
    (
        "E0114",
        "An effect resource takes a name reserved for the deferred comptime \
         feature, `CompileTimeEnv` or `CompileTimeHeap` (deferred.md § \
         Comptime). Reserving the names now lets that feature arrive \
         without breaking programs. Rename the resource.",
    ),
    (
        "E0116",
        "`continue 'label` names a labeled block rather than a loop \
         (design.md § Loops and labels). A block runs once, so there is no \
         next iteration to continue to. Use `break 'label` to leave the \
         block, or put the label on the enclosing loop.",
    ),
    (
        "E0117",
        "`#[non_exhaustive]` is on an item it does not apply to (design.md \
         § `#[non_exhaustive]`). It marks a `pub struct` or `pub enum` that \
         may gain fields or variants in a minor release, so it means \
         nothing on a function or a private type. Remove the attribute, or \
         move it to the public type.",
    ),
    (
        "E0118",
        "`#[track_caller]` is on something other than a function. The \
         attribute makes a panic inside a function report its caller's \
         location (design.md § 10, Errors and panics), so it applies only \
         to `fn` declarations. Remove it, or move it to the function that \
         panics.",
    ),
    (
        "E0119",
        "`#[deprecated]` is on an `impl` block (design.md § \
         `#[deprecated]`). An impl is not a name callers write, so the \
         warning would have nowhere to land. Mark each method that callers \
         should stop using.",
    ),
    (
        "E0120",
        "`#[deprecated]` is on a struct field, which 0.8 does not support; \
         deprecation is item-level (design.md § `#[deprecated]`). Deprecate \
         the accessor or constructor that callers use, or the whole struct.",
    ),
    (
        "E0122",
        "`#[profile(...)]` is on something other than a function. The \
         attribute asserts that a function's effect set fits a deployment \
         profile (deferred.md § Project Profiles), so it applies only to \
         `fn` declarations. Move it to the functions whose effects matter.",
    ),
    (
        "E0123",
        "`#[profile(...)]` names a profile that does not exist; the message \
         lists the ones that do: `default`, `embedded` and `kernel` \
         (deferred.md § Project Profiles). Fix the spelling.",
    ),
    (
        "E0125",
        "An effect variable is used where a type goes, or a type parameter \
         where an effect set goes (design.md § Effect variables). `[with \
         E]` declares an effect set, which belongs in a `with` clause \
         (`Fn(T) with E`) or a type's effect argument (`S[with E]`); `[T]` \
         declares a type. Declare the parameter with the kind its uses \
         need.",
    ),
    (
        "E0126",
        "A public signature names a type that declares an effect variable \
         (`Bus[with E]`) without spelling its effect set (design.md § \
         Effect variables). Effects are inferred for private code, but a \
         public signature is a contract, so it states them: `Bus[with \
         writes(Log)]`, or `Bus[with E]` with `E` declared on the function.",
    ),
    (
        "E0151",
        "A layout block regroups the fields of a `pub` `#[repr(C)]` struct \
         (deferred.md § Layout Blocks and `#[repr]`). Layout blocks are \
         deferred, so this compiler refuses every one as \
         E_DEFERRED_LAYOUT_BLOCK and no longer produces this code \
         (B-2026-11-16-257). When layout blocks land, `#[repr(C)]` fixes the \
         field order for code across the FFI boundary, so a block could not \
         regroup such a struct.",
    ),
    // B-2026-11-16-303, batch 3: typecheck codes no corpus program produced.
    (
        "E0221",
        "A `pub` function, field or type names a type that is not `pub` \
         (design.md § Visibility). Users of the package could receive a \
         value whose type they cannot name, import or match on. Mark the \
         type `pub`, or keep the item that exposes it internal.",
    ),
    (
        "E0257",
        "A field of a `sync struct` or `sync enum` is declared `mut` \
         (design.md § `sync` types). A `sync` value is shared across tasks \
         and never written field by field through a handle; state that \
         changes lives in `Atomic[T]` and `Mutex[T]` fields, whose methods \
         take a borrowed `self`. Drop the `mut`, and hold changing state in \
         one of those.",
    ),
    (
        "E0258",
        "A method of a `sync` type takes `mut ref self` (design.md § `sync` \
         types). A `sync` value may have holders in many tasks, so \
         exclusive access to it is never available. Take `self`, and change \
         state through the type's `Atomic[T]` methods or a `Mutex[T]` \
         field's lock guard.",
    ),
    (
        "E0266",
        "`main` returns a type the runtime cannot turn into an exit status \
         (design.md § The `main` function and exit codes). It returns `()`, \
         `Result[(), E]` with `E: Display`, or `ExitCode`. To end with a \
         number, return `ExitCode.from(n)`; to report a failure, return \
         `Err(e)`, which prints `Error: {e}` and exits 1.",
    ),
    (
        "E0279",
        "A bare variant name (`Red`) is declared by more than one enum in \
         scope, so the compiler cannot tell which is meant (design.md § \
         Enums). Qualify it with its enum, as the message suggests: \
         `Light.Red`.",
    ),
    // B-2026-11-16-303, batch 4: effect, ownership and repr codes no corpus
    // program produced.
    (
        "E0403",
        "A `with` clause names something that is not an effect: here a bare \
         name such as `with Nonexistent`, where a verb on a resource was \
         meant (design.md § Effect verbs). Effects are written as a verb \
         applied to a resource, `reads(Db)` or `writes(Log)`; effect groups \
         that would let one name stand for several are not in 0.8. Write \
         the verbs out, and declare any resource with `effect resource \
         Name;`.",
    ),
    (
        "E0406",
        "One effect variable is bound to two different effect sets at one \
         call (design.md § Effect variables): `pipe[with E](a: Fn() with E, \
         b: Fn() with E)` called with a closure that writes `Log` and one \
         that reads `Db`. Every slot that names `E` must agree, because `E` \
         is one set. Give the function two variables, `[with E1, E2]`, or \
         make the closures' effects match.",
    ),
    (
        "E0412",
        "A trait method's receiver and its effect clause disagree about a \
         resource (design.md § Effect verbs). A `mut ref self` method on a \
         resource's trait writes the provider, so every call infers \
         `writes(R)`, while the clause promises only `reads(R)`. Take \
         `self`, which borrows, if the method only reads, or declare \
         `writes(R)`.",
    ),
    (
        "E0501",
        "A struct contains itself by value, directly or through other \
         structs (`A` holds a `B`, which holds an `A`), so its size would \
         be infinite. A value type has a fixed size, known at compile time. \
         Break the cycle where it may end: an `Option[A]` field (`b: \
         Option[B]`) or a `Vec[A]`, or make one of the types `shared` so it \
         is held by a reference-counted handle (design.md § Enums, on \
         recursive types).",
    ),
    (
        "E0504",
        "A closure's capture mode does not match what its body does \
         (design.md § Captures): a capture declared `ref` (a `ref || ...` \
         closure, or `ref name` in its capture list) is moved or consumed \
         in the body. A borrowed capture cannot be given away. Drop the \
         `ref` so the closure takes the value (`own`, or a bare capture), \
         or borrow it in the body instead of consuming it.",
    ),
    (
        "E0803",
        "A `#[repr(transparent)]` struct has more than one field with data \
         (design.md § `#[repr(transparent)]`). Transparent means the struct \
         is laid out exactly as its one data field, so the ABI sees that \
         field's type; the other fields must be zero-sized, such as \
         `PhantomData[T]` or `()`. Keep one data field, or use `#[repr(C)]` \
         for a struct with several.",
    ),
    (
        "E0804",
        "An enum's explicit discriminant does not fit the integer type its \
         `#[repr]` names (design.md § 15, Unsafe, FFI and layout control): \
         `High = 300` in a `#[repr(u8)]` enum, whose range is 0 to 255. \
         Choose a wider repr (`#[repr(u16)]`), or a value in range.",
    ),
    (
        "E0806",
        "A `#[repr(...)]` argument is not a layout form (design.md § \
         `#[repr]`: ABI layout). A struct takes `C` or `transparent`; an \
         enum takes `C`, an integer type from `u8` to `u64` or `i8` to \
         `i64`, or `transparent`. Fix the spelling, as the message lists.",
    ),
    // B-2026-11-16-303: E0230 is reachable through the effect checker.
    (
        "E0230",
        "An impl's method performs an effect that its trait method's `with` \
         clause does not admit (design.md § Trait methods and generic \
         calls). The clause is a ceiling every implementation stays under, \
         so that a call through a bound has the clause's effects and no \
         more. Narrow the impl, for instance by moving the extra work out \
         of the method, or widen the trait's clause if every implementation \
         may need it.",
    ),
    // B-2026-11-16-303: codes that need a package of several files;
    // tests/diagnostics_corpus.rs builds each package and checks it.
    (
        "E0111",
        "An item is used from a module that cannot see it (design.md § \
         Visibility). `private` limits an item to the files of its own \
         directory; an item without a marker is visible across the package \
         but not to its users; `pub` is visible everywhere. Mark the item \
         with the visibility its users need, or move the caller next to it.",
    ),
    (
        "E0113",
        "An `import` names an item that the module does not declare, or \
         does not declare for this target (design.md § Imports). The \
         message lists the names the module does have and suggests the \
         closest. Fix the name, or add the item to the module with the \
         visibility the importer needs.",
    ),
    (
        "E0124",
        "A name used in this module is supplied by two wildcard imports \
         (`import alpha.*;` and `import beta.*;` both bring `dup`), so the \
         compiler cannot tell which is meant (design.md § Imports). Two \
         wildcards that overlap are not an error until the name is used. \
         Import the one you mean explicitly, `import alpha.dup;`, or call \
         it by its module, `alpha.dup()`.",
    ),
    (
        "E0223",
        "The modules' imports form a cycle (`a` imports `b`, which imports \
         `a`), which the compiler rejects before name resolution (design.md \
         § Modules, No circular module dependencies). A cycle means neither \
         module can be understood or compiled first. Move the items both \
         depend on into a lower module that both import.",
    ),
    (
        "E0227",
        "A command that works on a package (`karac check` or `karac build` \
         with no file, `karac test`) found no `kara.toml` in the current \
         directory or any parent (design.md § Packages and manifests). Run \
         it from inside the package, create one with `karac init`, or name \
         a single file: `karac check main.kara`.",
    ),
    // B-2026-11-16-303: symbolic codes reported as their own JSON code.
    (
        "E_DEFAULT_ATTRIBUTE_INVALID_POSITION",
        "`#[default]` is on something other than an enum variant (design.md \
         § `Default` and `#[default]`). The attribute picks which unit \
         variant `#[derive(Default)]` returns, so it means nothing on a \
         function, struct or field. Remove it; for a struct, \
         `#[derive(Default)]` builds each field's default.",
    ),
    (
        "E_DEFAULT_ATTRIBUTE_WITHOUT_DERIVE",
        "A variant is marked `#[default]`, but its enum does not derive \
         `Default` (design.md § `Default` and `#[default]`). The marker \
         only tells the derive which variant to return, so without the \
         derive it does nothing. Add `#[derive(Default)]` to the enum, as \
         the message suggests, or remove the marker.",
    ),
    // B-2026-11-16-303: symbolic codes of the attribute, Default-derive, cast
    // and place families, each pinned under corpus/diagnostics.
    (
        "E_AMBIGUOUS_ASSOC_TYPE",
        "A projection `T.Name` names one associated type, but two traits \
         bounding `T` each declare `Name` (`T: Add + Mul` and `T.Output`), \
         so the compiler cannot tell which trait's type is meant (design.md \
         § Associated types). Fix the type on each bound instead, `T: \
         Add[Output = T]`, or, when it must stay open, name it with a \
         generic parameter: `fn f[T: Add[Output = A] + Mul[Output = M], A, \
         M](...)` and write `A` or `M` where `T.Output` was.",
    ),
    (
        "E_ASSIGN_TARGET_NOT_PLACE",
        "The left side of `=` (or of a compound assignment such as `+=`) \
         must name a place that holds a value: a binding, a field `p.x`, an \
         index `v[i]`, a tuple element `t.0`, or a dereference `*r`. A \
         function call, a literal or any other computed value is a \
         temporary with nowhere to store the result. Bind the value first \
         (`let mut x = value(); x = 2;`), or assign to the place the call \
         was meant to reach.",
    ),
    (
        "E_ATTR_ON_ASSOC_TYPE_DECL",
        "An associated-type declaration in a trait (`type Item;`) takes no \
         attributes in 0.8: none of the attributes the language defines \
         applies to it, so one written there would be silently ignored. \
         Remove the attribute; a `///` doc comment above the declaration is \
         the way to document it.",
    ),
    (
        "E_ATTRIBUTE_NOT_VALID_ON_STATEMENT",
        "Inside a function body only the lint-level attributes \
         (`#[allow(...)]`, `#[warn(...)]`, `#[deny(...)]`, \
         `#[expect(...)]`) apply to a statement; they change the lint level \
         for that statement alone. Every other attribute (`#[derive]`, \
         `#[test]`, `#[repr]`, ...) belongs on an item, so move it onto the \
         declaration it was meant for. A loop's own attribute, \
         `#[par_order_free]`, goes directly before `for`, `while` or \
         `loop`.",
    ),
    (
        "E_BARE_TRAIT_TYPE",
        "A trait's name is not a type. A value of some type that implements \
         the trait, chosen at run time, is written `dyn Shape` (design.md § \
         `dyn Trait`); a parameter whose type is known at the call site \
         takes `impl Shape` or a generic `[T: Shape]` and is compiled once \
         per type with no indirection. Pick `dyn` when values of different \
         types share one collection or field, and a generic otherwise.",
    ),
    (
        "E_CHAR_AS_NARROW_INT",
        "`as` from `char` to an integer narrower than 32 bits (`u8`, `u16`, \
         `i8`, `i16`) is refused, because a Unicode scalar value runs to \
         0x10FFFF and the cast would keep only its low bits, which names no \
         meaningful character (design.md § `as` casts). Write `c as u32 as u8` \
         when the low bits are really what you want; for an ASCII literal \
         write the byte literal (`b'#'`), which `karac fix` does for you; \
         for the character's UTF-8 encoding use `c.to_string().bytes()`.",
    ),
    (
        "E_DEFAULT_MULTIPLE_VARIANTS",
        "`#[derive(Default)]` on an enum makes `Mode.default()` return the \
         variant marked `#[default]`, so exactly one variant may carry the \
         marker; here several do, and the compiler will not guess which you \
         meant. Keep the marker on the variant that is the starting state \
         and remove the others.",
    ),
    (
        "E_DEFAULT_NO_VARIANT_MARKED",
        "`#[derive(Default)]` on an enum needs to know which variant \
         `default()` returns, and nothing about an enum makes one variant \
         the obvious start. Mark exactly one field-less variant with \
         `#[default]`; for a struct the derive needs no marker, since each \
         field takes its own type's default.",
    ),
    (
        "E_DEFAULT_VARIANT_HAS_PAYLOAD",
        "The variant marked `#[default]` must have no fields: the derive \
         builds it with no inputs, and choosing a value for a payload \
         (`Busy(0)`? `Busy(-1)`?) is a decision the program should state. \
         Mark a field-less variant instead, or drop the derive and write \
         `impl Mode { fn default() -> Mode { Mode.Busy(0) } }` with the \
         starting value you want.",
    ),
    // B-2026-11-16-303: the deprecation, Drop, discriminant, dyn, contract and
    // pattern families, each pinned under corpus/diagnostics.
    (
        "E_AT_BINDING_DOUBLE_CONSUME",
        "A pattern `whole @ Pair { a, b }` binds the whole value and, \
         inside it, its fields. If both bindings take ownership, the same \
         field would be moved twice: once inside `whole` and once as `a`. \
         Borrow one side (`ref whole @ Pair { a, b }`, or bind the fields \
         with `ref a`), or drop one of the two bindings and read the field \
         from the other.",
    ),
    (
        "E_CONTINUE_LABEL_BLOCK",
        "`continue 'label` starts the next iteration of the labeled loop, \
         and a labeled block (`'outer: { ... }`) has no next iteration. Use \
         `break 'outer` to leave the block, or put the label on a loop if \
         the intent was to continue an outer loop.",
    ),
    (
        "E_DEPRECATED_DUPLICATE",
        "An item carries at most one `#[deprecated]` attribute, since one \
         item has one deprecation (design.md § `#[deprecated]`). Merge the \
         two into one, with `since` and `note` as needed, and remove the \
         other.",
    ),
    (
        "E_DEPRECATED_FIELD_NOT_STRING",
        "The `since` and `note` arguments of `#[deprecated]` are shown to \
         users of the item as written, so each must be a string literal: \
         `#[deprecated(since: \"0.9\", note: \"use `area` instead\")]`. A \
         number, a name or an expression is refused.",
    ),
    (
        "E_DEPRECATED_ON_FIELD",
        "`#[deprecated]` applies to items (functions, methods, types, \
         constants, traits, modules), not to a single struct field, in 0.8. \
         To retire a field, deprecate the accessor or constructor that \
         exposes it, or the struct itself with a note naming the \
         replacement.",
    ),
    (
        "E_DEPRECATED_ON_IMPL",
        "An `impl` block is not something a caller names, so deprecating it \
         would warn nowhere. Put `#[deprecated]` on each method being \
         retired, or on the type itself if the whole type is going away \
         (design.md § `#[deprecated]`).",
    ),
    (
        "E_DEPRECATED_POSITIONAL_ARG",
        "`#[deprecated]` takes only named arguments, so that which string \
         is the version and which is the explanation is never ambiguous: \
         `#[deprecated(note: \"use `next` instead\")]` or \
         `#[deprecated(since: \"0.9\", note: \"...\")]`. A bare \
         `#[deprecated]` with no arguments is also valid.",
    ),
    (
        "E_DEPRECATED_UNKNOWN_FIELD",
        "`#[deprecated]` accepts two arguments, `since` (the version that \
         deprecated the item) and `note` (what to use instead). Any other \
         name, such as `reason`, is refused rather than ignored; most often \
         it should be `note`.",
    ),
    (
        "E_DERIVE_DEFAULT_MISSING_FIELD_DEFAULT",
        "`#[derive(Default)]` on a struct builds each field from its own \
         type's default, so every field's type must implement `Default`. \
         Derive or implement `Default` on the field's type, or write `impl \
         Config { fn default() -> Config { ... } }` by hand and choose that \
         field's starting value there.",
    ),
    (
        "E_DISCRIMINANT_OUT_OF_RANGE",
        "An explicit discriminant must fit in the enum's representation \
         type: under `#[repr(u8)]` the values run from 0 to 255. Choose a \
         value in range, or widen the representation (`#[repr(u16)]`) if \
         the values are fixed by an external format.",
    ),
    (
        "E_DROP_DUPLICATE_IMPL",
        "A type has one destructor. A second `impl Drop for T` would leave \
         it unclear which cleanup runs and in what order, so it is refused. \
         Merge both bodies into the one `drop` method, in the order the \
         cleanup should happen.",
    ),
    (
        "E_DROP_SIGNATURE_INVALID",
        "`Drop` is declared `trait Drop { fn drop(mut ref self); }`: the \
         destructor borrows the value mutably for the moment before its \
         fields are dropped. A `self` (shared borrow) or `own self` (which \
         would drop the value again inside its own destructor) receiver, \
         extra parameters or a return type are refused. Write `fn drop(mut \
         ref self) { ... }`.",
    ),
    (
        "E_DUPLICATE_DISCRIMINANT",
        "Two variants of the enum have the same discriminant value, so \
         converting the value back to a variant could not tell them apart. Give each variant its own value; \
         an implicit discriminant is the previous one plus one, which is \
         often where the collision comes from.",
    ),
    (
        "E_DUPLICATE_LINT_LEVEL",
        "The same lint name appears twice in one lint-level attribute \
         (`#[allow(deprecated, deprecated)]`). The repeat does nothing, and \
         usually means a different lint was meant. Remove the duplicate or \
         replace it with the intended name.",
    ),
    (
        "E_DYN_NOT_DISPATCHABLE",
        "A `dyn Trait` value is called through a table of the trait's \
         methods, so every required method must be callable on a value \
         whose concrete type is unknown. A method with no receiver (`fn \
         make() -> Self`) has no value to dispatch on. Give that method a \
         default body, so it is not required, or keep the concrete type and \
         take a generic `[T: Maker]` instead of `dyn Maker` (design.md § \
         `dyn Trait`).",
    ),
    (
        "E_DYN_SECOND_TRAIT",
        "A `dyn` type names one trait, optionally with the marker \
         `CrossTask` after `+`. Two ordinary traits would need two method \
         tables in one value. Declare a trait with both as supertraits \
         (`trait NamedShape: Shape + Named {}`), implement it, and use `dyn \
         NamedShape` (design.md § `dyn Trait`).",
    ),
    (
        "E_EMPTY_UNION",
        "A union with no fields has no storage and no value can be written \
         to or read from it. Declare at least one field; for a type that \
         can never be constructed, use an enum with no variants or the type `Never`.",
    ),
    (
        "E_FLOAT_AS_BOOL",
        "`as` does not convert a float to `bool`: whether NaN, -0.0 or a \
         tiny denormal counts as true has no single right answer. Write the \
         predicate you mean instead, such as `f != 0.0` or `f > 0.0`.",
    ),
    (
        "E_FN_ANONYMOUS_PARAM",
        "Every function parameter has a name. Write `_: i64` for a \
         parameter the body does not use, or give it a meaningful name. \
         Only a function TYPE (`fn(i64, i64) -> i64`) lists parameter types \
         alone.",
    ),
    // B-2026-11-16-303: impl Trait positions, casts, lint levels, main, marker
    // traits, module constants and #[no_effect], each pinned under
    // corpus/diagnostics.
    (
        "E_IMPL_TRAIT_IN_NESTED_POSITION",
        "Argument-position `impl Trait` stands for one hidden generic \
         parameter, and in 0.8 it may only be a parameter's whole type (`x: \
         impl Display`). Inside another type's arguments (`Vec[impl \
         Display]`, `Option[impl Shape]`) it is refused. Name the parameter \
         instead: `fn first[T: Display](items: Vec[T])` (design.md § `impl \
         Trait`).",
    ),
    (
        "E_IMPL_TRAIT_IN_TRAIT_METHOD_ARG",
        "A trait method's parameter cannot be `impl Trait`: each \
         implementation would need to agree on a hidden generic parameter \
         it never sees declared. Write the generic out on the trait method, \
         `fn log[T: Display](self, msg: T);`, and implementations repeat \
         it. Argument-position `impl Trait` remains fine on free functions \
         and inherent methods (design.md § `impl Trait`).",
    ),
    (
        "E_IMPL_TRAIT_MULTIPLE_WITNESSES",
        "A return type `impl Trait` hides one concrete type that the \
         compiler picks from the body, so every `return` and the tail must \
         produce that same type. Here the branches return different types. \
         Convert them to one type, or, when the type must differ at run \
         time, return `dyn Trait` (design.md § `dyn Trait`), which \
         dispatches through a method table.",
    ),
    (
        "E_INT_AS_BOOL",
        "`as` does not turn an integer into a `bool`: whether only 1, or \
         any non-zero value, means true is a choice the code should state. \
         Write the comparison you mean, usually `n != 0`.",
    ),
    (
        "E_INT_AS_CHAR",
        "Not every integer is a Unicode scalar value: the surrogates 0xD800 \
         to 0xDFFF and anything above 0x10FFFF are not characters, so `as \
         char` from an integer is refused. `char.try_from(n)` returns \
         `Result[char, _]` and lets the program handle the invalid case. A \
         `u8` byte literal (`b'a'`) already has its own type and needs no \
         conversion.",
    ),
    (
        "E_INT_TO_PTR_CAST_FORBIDDEN",
        "A pointer is an address plus the right to access a particular \
         allocation (its provenance), and an integer carries only the \
         address, so `addr as *const T` is refused. To move an existing \
         pointer to another address in the same allocation, use \
         `ptr.with_addr(p, addr)`. To turn back into a pointer an address \
         that was exposed earlier, use `unsafe { ptr.from_exposed(addr) }` \
         with the pointer type annotated.",
    ),
    (
        "E_LAYOUT_QUERY_TAKES_NO_ARGS",
        "The layout queries `T.size_of()` and `T.align_of()` measure the \
         type written as their receiver and take no arguments. Remove the \
         arguments; to measure a value's type, write that type as the \
         receiver (`Point.size_of()`).",
    ),
    (
        "E_LINT_LEVEL_NO_ARGS",
        "A lint-level attribute (`#[allow]`, `#[warn]`, `#[deny]`, \
         `#[expect]`) needs the names of the lints it applies to; with none \
         it would change nothing. Write `#[allow(unused_variables)]`, \
         naming one or more lints.",
    ),
    (
        "E_LINT_LEVEL_NON_IDENT_ARG",
        "A lint-level attribute takes lint names as bare identifiers, \
         `#[allow(deprecated, unused_variables)]`. A string, a number or a \
         `key: value` argument is refused, so a typo cannot hide inside a \
         value that is never checked against the lint list.",
    ),
    (
        "E_MAIN_ERR_NOT_DISPLAY",
        "When `main` returns `Result[(), E]` and the result is `Err(e)`, \
         the program prints `Error: {e}` and exits with status 1, which \
         needs `E: Display` (design.md § The `main` function and exit \
         codes). Add `#[derive(Display)]` or `impl Display for E` to the \
         error type, or map the error to one that has it (often `String`) \
         before returning.",
    ),
    (
        "E_MAIN_RETURN_TYPE",
        "`main` returns one of three things: nothing (`()`), `Result[(), \
         E]` with `E: Display`, or `ExitCode` for an explicit status \
         (design.md § The `main` function and exit codes). An integer \
         return is not a status code in Kāra; return `ExitCode.from(n)` for \
         that.",
    ),
    (
        "E_MAIN_SIGNATURE",
        "`main` takes no parameters (design.md § The `main` function and \
         exit codes). The command line is read with `env.args()` and the \
         environment with `env.var(name)`, from anywhere in the program.",
    ),
    (
        "E_MALFORMED_ATTRIBUTE_ARGS",
        "This attribute's arguments do not have the shape it accepts: \
         `#[cold]` and `#[default]` take none, and `#[inline]` takes none \
         or exactly one of `always` and `never`. The message gives the \
         accepted forms; remove or correct the arguments.",
    ),
    (
        "E_MARKER_IMPL_HAS_METHOD",
        "A marker trait (`marker trait Audited;`) has no methods or \
         associated items: implementing it only records that the type has \
         the property. Its `impl` must therefore have an empty body, `impl \
         Audited for Token {}`. Move the methods to an inherent `impl Token \
         { ... }` or to a trait that declares them (design.md § Marker \
         traits).",
    ),
    (
        "E_MARKER_TRAIT_HAS_ITEM",
        "A marker trait carries no associated types or constants; it only \
         marks types that have a property (design.md § Marker traits). \
         Remove the item, or declare an ordinary `trait` if implementations \
         need to supply it.",
    ),
    (
        "E_MARKER_TRAIT_HAS_METHOD",
        "A marker trait declares no methods; it only marks types that have \
         a property (design.md § Marker traits). Remove the method, or \
         declare an ordinary `trait` if implementations need to provide it.",
    ),
    (
        "E_MODULE_BINDING_EFFECTFUL_INIT",
        "A `const` is evaluated by the compiler and stored in the binary, \
         so its initializer must be a constant expression: literals, \
         arithmetic, comparisons and boolean operations over constants, \
         enum variants, struct and tuple literals of constants, array \
         literals, and other constants (design.md § Constants). A function \
         call is evaluated at run time. Compute the value in `main` and \
         pass it down (module-level cells such as `LazyLock` are deferred).",
    ),
    (
        "E_MODULE_BINDING_HEAP_TYPE",
        "A constant lives in the binary as data, so its type cannot own \
         heap memory: `String`, `Vec` and `Map` allocate when they are \
         built. Use the borrowed form for constant data, `Str` for text and \
         an `Array` for a fixed list (design.md § Constants), and convert \
         at the use site (`GREETING.to_string()`) when an owned value is \
         needed.",
    ),
    (
        "E_MODULE_BINDING_NAMING",
        "A top-level binding in a file that has `fn main` is read as a \
         module-level binding, and module-level names are Const-class \
         (SCREAMING_SNAKE_CASE). Note that a module-level value is declared \
         `const NAME: T = e;`; a module `let` is reported next as \
         E_MODULE_LET, and there are no mutable globals. If the binding was \
         meant to be local, move it into `main`.",
    ),
    (
        "E_NO_EFFECT_BAD_VERB",
        "`#[no_effect(...)]` lists effect verbs, written as in a `with` \
         clause: `allocates(Heap)`, `panics`, `blocks`, `reads(Config)`, \
         `writes(Db)` (design.md § `#[no_effect]`). A word that is not an \
         effect verb is refused so that the guarantee cannot be silently \
         weaker than it reads.",
    ),
    (
        "E_NO_EFFECT_EMPTY",
        "`#[no_effect]` forbids the effects it names, so with no verbs it \
         would forbid nothing while reading like a guarantee. Name at least \
         one: `#[no_effect(allocates(Heap))]`, `#[no_effect(panics)]` \
         (design.md § `#[no_effect]`).",
    ),
    (
        "E_NO_EFFECT_INVALID_TARGET",
        "`#[no_effect(...)]` constrains a function's effect set, so it \
         applies only to a `fn` (free function or method). On a struct, \
         enum, trait or other item there is no effect set to constrain. Put \
         it on the functions that must not have the effect.",
    ),
    (
        "E_NO_EFFECT_STRING_VALUE",
        "`#[no_effect = \"panics\"]` is not a form the attribute takes: its \
         arguments are effect verbs in parentheses, `#[no_effect(panics)]`, \
         so the compiler can check them (design.md § `#[no_effect]`).",
    ),
    (
        "E_NO_EFFECT_VIOLATED",
        "The function declares `#[no_effect(...)]` for an effect its body \
         has, directly or through a function it calls; the message names \
         the operation that brings it in. Remove that operation (for \
         `panics`, use `get` instead of indexing, or handle the \
         `None`/`Err` instead of `unwrap`), or drop the attribute if the \
         guarantee is not meant to hold (design.md § `#[no_effect]`).",
    ),
    (
        "E_NON_CONSTANT_DISCRIMINANT",
        "An explicit enum discriminant is fixed at compile time, so it must \
         be a constant integer expression: an integer literal, arithmetic \
         over literals, or a module-level integer `const`. A function call, \
         a value of another type, an unknown name or constants that refer \
         to each other in a cycle are refused. Declare `const BASE: i64 = \
         1;` and write `Low = BASE`.",
    ),
    (
        "E_NON_EXHAUSTIVE_INVALID_TARGET",
        "`#[non_exhaustive]` lets a public type gain variants or fields \
         later without breaking other packages, so it applies only to a \
         `pub struct` or `pub enum`. On a function, a trait or a private \
         type it has nothing to protect (design.md § `#[non_exhaustive]`).",
    ),
    // B-2026-11-16-303: patterns, discriminants, FFI borrows, attribute
    // validation and test placement, each pinned under corpus/diagnostics.
    (
        "E_DUPLICATE_MODULE_BINDING",
        "Two module-level bindings in one module have the same name, so \
         every use would be ambiguous. Rename one, or delete the duplicate. \
         A module-level value is declared `const NAME: T = e;` (design.md § \
         Constants).",
    ),
    (
        "E_DYN_REPLACE_THROUGH_VIEW",
        "A `mut ref dyn Trait` lets you call `mut ref self` methods on the \
         value it views, but not replace that value with `*s = ...`: the \
         new value may be a different type with a different size, and the \
         view only knows the old one's storage (design.md § `dyn Trait`). \
         Assign to the owned `dyn Trait` binding or field itself, which \
         drops the old value first, or change the value through its \
         methods.",
    ),
    (
        "E_EMPTY_PREFIX_LITERAL_NEEDS_ANNOTATION",
        "An empty collection literal such as `Set[]` or `Map[]` has no \
         elements to infer its element type from, and nothing later in the \
         code fixed it. Annotate the binding (`let s: Set[i64] = Set[];`) \
         or use the constructor with an annotated binding (`let s: Set[i64] \
         = Set.new();`).",
    ),
    (
        "E_EXTERN_REF_PARAM",
        "`ref` and `mut ref` are Kāra borrows, checked by the compiler, and \
         have no C representation, so a foreign function's parameter cannot \
         be one. Use a raw pointer, `*const T` or `*mut T`; for text, take \
         `*const u8` and pass `s.as_ptr()` at the call site. An opaque \
         foreign type is the exception: `ref Foo` is its handle (design.md \
         § 15).",
    ),
    (
        "E_EXTERN_REF_RETURN",
        "A foreign function cannot return a Kāra borrow: nothing on the C \
         side can promise how long the pointee lives, which is what a `ref` \
         claims. Declare the return as a raw pointer, `*const T` or `*mut \
         T`, and read through it inside an `unsafe` block, copying the data \
         into an owned value if it must outlive the call.",
    ),
    (
        "E_IMPL_TARGET_NOT_DISTINGUISHABLE",
        "Several `impl` blocks define the same method for a type, differing \
         only in a const argument (`impl Grid[3]` and another `impl \
         Grid[3]`, or two sizes the dispatcher cannot tell apart). Every \
         call would compile to one function, and the interpreter and the \
         compiled backends could each pick a different body. Give the \
         methods different names, or define one method that branches on the \
         size.",
    ),
    (
        "E_IMPL_TRAIT_MISSING_BOUND",
        "A function declared to return `impl Trait` must return a value \
         whose type implements that trait; here the body's type does not. \
         Implement or derive the trait for the returned type, return a \
         different value, or change the declared return type.",
    ),
    (
        "E_INTERIOR_NUL_IN_C_STRING",
        "A C string ends at its first NUL byte, so a `\\0` inside a \
         `c\"...\"` literal would silently cut it short for every C \
         function that reads it. Remove the escape; to pass bytes that \
         contain zeros, use a byte slice and an explicit length.",
    ),
    (
        "E_LINT_LEVEL_ON_HARD_RULE",
        "Some rules are not lints and cannot be allowed, warned or denied: \
         `unsafe_op_in_unsafe_fn` is one, since an unsafe operation always \
         needs its own `unsafe { ... }` block, even inside an `unsafe fn`. \
         Remove the lint-level attribute and wrap the operation in `unsafe \
         { ... }`, with a `// Safety:` comment saying why it holds.",
    ),
    (
        "E_PANIC_HANDLER",
        "A function marked `#[panic_handler]` replaces the default panic \
         report, and the runtime calls it with one argument, so its \
         signature is fixed: `fn name(info: PanicInfo)`, with no type \
         parameters and no return value (design.md § Panic handler). A \
         program has at most one.",
    ),
    (
        "E_PARTIAL_EXPLICIT_DISCRIMINANTS",
        "An enum either gives every variant an explicit discriminant or \
         none of them. A mix (`Low = 1, Mid, High = 5`) leaves the reader \
         to work out the implicit ones and makes reordering variants \
         silently change values. Write a value on each variant, or remove \
         them all and let declaration order number the variants from 0.",
    ),
    (
        "E_PAYLOAD_DISCRIMINANT_REQUIRES_REPR",
        "An explicit discriminant on an enum whose variants carry data is a \
         promise about the stored tag, and without a layout the tag's \
         location is unspecified, so the value could never be observed. Add \
         `#[repr(u8)]` (or another integer type) or `#[repr(C)]` to fix the \
         layout (design.md § 15), or remove the explicit values.",
    ),
    (
        "E_RANGE_PATTERN_BOUND_NOT_SIMPLE",
        "A range pattern's bounds are fixed when the `match` is compiled, \
         so each must be a literal or the name of a module-level `const`: \
         `0..=9`, `MIN..=MAX`. An expression such as `(1 + 2)` or a call is \
         refused. Declare a `const` for the bound, or use a guard (`n if n \
         <= limit() =>`).",
    ),
    (
        "E_REASSIGN_TO_IMMUTABLE_MODULE_BINDING",
        "A module-level `const` is a value, not a variable: each use builds \
         a fresh copy, so assigning to it would change nothing, and Kāra \
         has no mutable globals (design.md § Constants). To change a copy, \
         bind it first (`let mut v = LIMIT;`). State that changes during \
         the run is created in `main` and passed to the functions that use \
         it.",
    ),
    (
        "E_REST_PATTERN_DUPLICATE",
        "A struct pattern takes at most one `..`, which stands for all the \
         fields the pattern does not name. A second one adds nothing. \
         Remove the extra `..`.",
    ),
    (
        "E_REST_PATTERN_NOT_LAST",
        "In a struct pattern, `..` (the fields not named) comes after every \
         named field: `P { x, .. }`. Move it to the end, or drop the named \
         fields that follow it.",
    ),
    (
        "E_RPITIT_INCOMPATIBLE_WITH_DYN",
        "A trait method that returns `impl Trait` hides a different \
         concrete type in each implementation, so a `dyn` value cannot call \
         it through a single method-table entry. Take the value through a \
         generic parameter (`[T: Source]`) instead of `dyn Source`, or \
         change the method to return a concrete type or a `dyn` value.",
    ),
    (
        "E_SECRET_TRAIT_FORBIDDEN",
        "`Secret[T]` withholds `Display`, `Debug`, `Serialize`, `Eq` and \
         similar traits on purpose, so a secret cannot be printed, logged, \
         serialized or compared in variable time by accident, and an `impl` \
         cannot add them back (docs/library/secret.md). Read the value with \
         `.expose()` where it is really needed, compare with the \
         constant-time `.ct_eq(...)`, and let the built-in drop wipe it.",
    ),
    (
        "E_TEST_BLOCK_NOT_TOP_LEVEL",
        "`test \"name\" { ... }` declares a test case, which is a top-level \
         item in a test file, not a statement (design.md § 14). Inside a \
         function it is refused. Move the block to module scope in a \
         `_test.kara` file, or, for a check that should run with the \
         function, call the assertion directly.",
    ),
    (
        "E_TRACK_CALLER_ARGS_NOT_PERMITTED",
        "`#[track_caller]` has no options: it makes a panic inside the \
         function report its caller's location. Write it bare, \
         `#[track_caller]`.",
    ),
    (
        "E_TRACK_CALLER_INVALID_TARGET",
        "`#[track_caller]` makes a panic inside a function report the \
         location of the call instead (design.md § 10, \"Panics report the \
         caller's location\"), so it applies only to a `fn`. On a struct, \
         enum, trait or impl block there is no call site. Put it on the \
         functions that panic on their arguments.",
    ),
    (
        "E_TRAIT_METHOD_ANONYMOUS_PARAM",
        "Every parameter of a trait method has a name, as in any function. \
         Write `_: f64` for a parameter the declaration does not use, or \
         give it a name that documents it for implementations.",
    ),
    (
        "E_UNKNOWN_ASSOC_TYPE_BINDING",
        "An associated-type binding such as `Iterator[Item = i64]` fixes a \
         type the trait declares. The name given is not one of the trait's \
         associated types, so the binding would constrain nothing; the \
         message lists the ones it has. Correct the name (for `Iterator` it \
         is `Item`).",
    ),
    (
        "E_UNKNOWN_ATTRIBUTE",
        "The compiler does not recognize this attribute name. design.md § \
         Attributes lists the attributes the language defines; an unknown \
         one is refused rather than ignored, so a misspelling \
         (`#[derives]`, `#[must_used]`) cannot silently do nothing. Correct \
         the name or remove the attribute.",
    ),
    (
        "E_UNSTABLE_DUPLICATE",
        "An item carries at most one `#[unstable]` attribute. Keep one, \
         with a `note` if needed, and remove the rest.",
    ),
    (
        "E_UNSTABLE_POSITIONAL_ARG",
        "`#[unstable]` takes only a named `note`: `#[unstable(note: \"may \
         change before 1.0\")]`, or no arguments at all. A positional \
         string is refused so the argument's role is never ambiguous.",
    ),
];

/// `E0254` and `E_NOT_CROSS_TASK` are one rule: the message names the
/// second, `karac check --output=json` reports the first.
const CROSS_TASK_PROSE: &str =
    "A value that is not `CrossTask` would cross a task boundary. Only a \
     `CrossTask` value may be captured by a `par` branch, a `par for` \
     iteration or a `TaskGroup.spawn` closure, be a task's result, be sent \
     on a channel, or be stored in a `sync` field (design.md § CrossTask, \
     core-semantics §6.3). The compiler computes `CrossTask`: a type is \
     `CrossTask` unless it holds, at any depth, a `shared` handle or another \
     task-local type, and the `note:` line shows where. A function value is \
     `CrossTask` only when everything its environment reaches is, and an \
     erased function type (`Fn(A) -> R`) or a `dyn Trait` is not `CrossTask` \
     unless marked `+ CrossTask`. The rule is about the task boundary, not \
     the thread count, so it holds on one worker too. Share state between \
     tasks through a `sync` type holding `Mutex[T]` or `Atomic[T]`, move an \
     owned copy into the task, or mark the function or `dyn` type \
     `+ CrossTask`. `E_NOT_CROSS_TASK` in a message and `E0254` in \
     `--output=json` are the same rule.";

/// The prose for `code`, if it has any.
fn code_prose(code: &str) -> Option<&'static str> {
    CODE_PROSE.iter().find(|(c, _)| *c == code).map(|(_, p)| *p)
}

/// `code` is catalogued: `karac explain` reads `e0500` as `E0500`
/// (B-2026-11-07-47).
pub fn is_catalogued_code(code: &str) -> bool {
    !lookup_code(code).is_empty()
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

/// A catalogued symbolic code (`E_NOT_CROSS_TASK`, `E_MODULE_LET`): `karac
/// explain` reads it as a code, not as the class name its UPPER_SNAKE shape
/// would otherwise make it.
pub fn is_symbolic_code(name: &str) -> bool {
    (name.starts_with("E_") || name.starts_with("W_")) && !lookup_code(name).is_empty()
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
             typecheck E02xx / W02xx, typecheck lints W03xx / E03xx, effect \
             E04xx, ownership E05xx / N05xx, provider-escape E0600, \
             target/GPU E08xx, and the loose E0223 / E0227 — so a code in \
             that shape which lands here is either a typo or from a \
             different compiler version. Symbolic codes \
             (`E_SLICE_BORROW_CONFLICT`) and lint names (`redundant_clone`) \
             explain too, as does a record's class with \
             `karac explain --class=NAME`. Supported classes: {}.",
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

/// A code whose own prose replaces its class paragraph, because the class it
/// is filed under describes a different error (B-2026-11-16-297).
const CLASS_MISFITS: &[&str] = &["E0216"];

/// The paragraph `karac explain` gives for a code that marks a construct
/// outside the language this compiler implements (B-2026-11-16-297).
const SCOPE_PROSE: &str = "This code marks a construct that is not in the language this compiler \
     implements: a feature scheduled for a later track (docs/deferred.md says \
     which), or one removed on purpose. Nothing in the program's types is \
     wrong; the message names the construct and the form to write instead. \
     `karac explain --concept=scope/later-tracks` gives the scope of v1.";

/// A code marks a later-track or removed construct: its kind says so, or its
/// name does (`E_TRY_BLOCK_NOT_IMPLEMENTED_YET`), or it is one of
/// [`DEFERRED_CODES`].
fn is_scope_code(code: &str, entry: &CodeEntry) -> bool {
    concept_for_kind(entry.kind) == Some("scope/later-tracks")
        || code.ends_with("_NOT_IMPLEMENTED_YET")
        || code.ends_with("_NOT_YET")
        || deferred_section(code).is_some()
}

/// Codes that check a deferred feature's syntax, which the front end still
/// parses, and where the spec says so. They print [`SCOPE_PROSE`] and that
/// pointer, and have no corpus pin
/// (review-thread ruling 2026-11-17 on B-2026-11-16-303).
const DEFERRED_CODES: &[(&str, &str)] = &[
    (
        "E0217",
        "The pipe operator is not in the language (design.md § Symbols: \
         punctuation: \"There is no `?.` and no `|>`\"); the parser refuses `|>` \
         as E0001 (B-2026-11-16-341), so this code is no longer produced.",
    ),
    (
        "E0106",
        "Layout blocks are deferred (docs/deferred.md § Layout Blocks); every one is \
         refused as E_DEFERRED_LAYOUT_BLOCK (B-2026-11-16-257).",
    ),
    (
        "E0151",
        "Layout blocks are deferred (docs/deferred.md § Layout Blocks); every one is \
         refused as E_DEFERRED_LAYOUT_BLOCK (B-2026-11-16-257).",
    ),
    (
        "E0260",
        "The `lock` block belongs to a later track (design.md § Keywords lists `lock` \
         among deferred features' words); in 0.8 a `Mutex[T]` is locked with \
         `m.lock()` (design.md § Locks and atomics).",
    ),
    (
        "E0402",
        "Effect groups are deferred (docs/deferred.md § Effect Groups and Composition).",
    ),
    (
        "E0405",
        "Profiles are deferred (docs/deferred.md § Project Profiles).",
    ),
    (
        "E0407",
        "Profiles are deferred (docs/deferred.md § Project Profiles).",
    ),
    (
        "E0409",
        "Module-level mutable state is deferred (docs/deferred.md § Module-Level \
         Mutable State); a module `let mut` is E_MODULE_LET.",
    ),
    (
        "E_CODEGEN_HINT_INVALID_POSITION",
        "The codegen hints `#[inline]`, `#[inline(always)]`, `#[inline(never)]` and `#[cold]` are parsed and checked for placement and conflicts, but not yet lowered to code generation (docs/deferred.md § Codegen Hint Attributes); they do not change what is emitted.",
    ),
    (
        "E_CODEGEN_HINT_ON_CLOSURE",
        "The codegen hints `#[inline]`, `#[inline(always)]`, `#[inline(never)]` and `#[cold]` are parsed and checked for placement and conflicts, but not yet lowered to code generation (docs/deferred.md § Codegen Hint Attributes); they do not change what is emitted.",
    ),
    (
        "E_CODEGEN_HINT_ON_EXTERN_DECL",
        "The codegen hints `#[inline]`, `#[inline(always)]`, `#[inline(never)]` and `#[cold]` are parsed and checked for placement and conflicts, but not yet lowered to code generation (docs/deferred.md § Codegen Hint Attributes); they do not change what is emitted.",
    ),
    (
        "E_COLD_INLINE_ALWAYS_CONFLICT",
        "The codegen hints `#[inline]`, `#[inline(always)]`, `#[inline(never)]` and `#[cold]` are parsed and checked for placement and conflicts, but not yet lowered to code generation (docs/deferred.md § Codegen Hint Attributes); they do not change what is emitted.",
    ),
    (
        "E_INLINE_HINT_CONFLICT",
        "The codegen hints `#[inline]`, `#[inline(always)]`, `#[inline(never)]` and `#[cold]` are parsed and checked for placement and conflicts, but not yet lowered to code generation (docs/deferred.md § Codegen Hint Attributes); they do not change what is emitted.",
    ),
    (
        "E_MULTIVERSION_EMPTY",
        "`#[multiversion]` is deferred (docs/deferred.md § Multiversioning: `cpu-baseline` and `#[multiversion]`); the attribute is parsed and checked but no per-CPU variants are built.",
    ),
    (
        "E_MULTIVERSION_ON_ASSOC",
        "`#[multiversion]` is deferred (docs/deferred.md § Multiversioning: `cpu-baseline` and `#[multiversion]`); the attribute is parsed and checked but no per-CPU variants are built.",
    ),
    (
        "E_IMPL_TRAIT_ALIAS",
        "Trait aliases are deferred (docs/deferred.md § Trait Aliases, \"Implementing trait aliases\"); implement each component trait separately.",
    ),
];

fn deferred_section(code: &str) -> Option<&'static str> {
    DEFERRED_CODES
        .iter()
        .find(|(c, _)| *c == code)
        .map(|(_, section)| *section)
        .or_else(|| family_of(DEFERRED_FAMILIES, code))
}

/// A family of codes: the codes that start with one of `prefixes` or are
/// listed in `codes`, and the sentence pointing at the spec section.
struct CodeFamily {
    prefixes: &'static [&'static str],
    codes: &'static [&'static str],
    pointer: &'static str,
}

fn family_of(families: &[CodeFamily], code: &str) -> Option<&'static str> {
    families
        .iter()
        .find(|f| f.codes.contains(&code) || f.prefixes.iter().any(|p| code.starts_with(p)))
        .map(|f| f.pointer)
}

/// Features deferred.md specifies that nothing in the corpus runs: their
/// codes print [`SCOPE_PROSE`] and the pointer (review-thread ruling
/// 2026-11-17 on B-2026-11-16-303, one rule per family).
const DEFERRED_FAMILIES: &[CodeFamily] = &[
    CodeFamily {
        prefixes: &["E_UNION_"],
        codes: &["E_EMPTY_UNION"],
        pointer: "Unions are deferred (docs/deferred.md § FFI Unions: `union Foo { ... }`).",
    },
    CodeFamily {
        prefixes: &["E_PROFILE_"],
        codes: &[
            "E_UNKNOWN_PROFILE",
            "E_RC_FALLBACK_ALLOCATES_UNDER_FALLIBLE_PROFILE",
        ],
        pointer: "Profiles are deferred (docs/deferred.md § Project Profiles).",
    },
    CodeFamily {
        prefixes: &[],
        codes: &["E_VARIANCE_DECLARATION_INCONSISTENT"],
        pointer: "Declared variance is deferred (docs/deferred.md § Declared Variance Markers).",
    },
];

/// Features deferred.md specifies that the compiler implements and corpus
/// programs run: experimental in 0.8 (Gowtham's decision on contracts
/// 2026-11-17, applied by family). Each deferred.md section says so.
const EXPERIMENTAL_FAMILIES: &[CodeFamily] = &[
    CodeFamily {
        prefixes: &[],
        codes: &[
            "E_CONTRACT_IMPURE",
            "E_CONSUMED_SELF_IN_ENSURES",
            "E_OLD_NOT_CLONE",
            "E_OLD_OUTSIDE_ENSURES",
            "E_OLD_RESULT",
        ],
        pointer: "Contracts (`requires`, `ensures`, `old(...)`); docs/deferred.md § \
                  Contracts (requires / ensures / invariant)",
    },
    CodeFamily {
        prefixes: &["E_COMPTIME_"],
        codes: &["E_TYPE_VALUE_AT_RUNTIME"],
        pointer: "Comptime (`comptime fn`, `comptime` parameters); docs/deferred.md § Comptime",
    },
    CodeFamily {
        prefixes: &["E_OFFSET_OF_"],
        codes: &[],
        pointer: "`offset_of`; docs/deferred.md § Field Offsets: `offset_of[T](field)`",
    },
    CodeFamily {
        prefixes: &["E_ONCE_CELL_"],
        codes: &["E_LAZYLOCK_RUNTIME_CAPTURE"],
        pointer: "Module-level cells (`LazyLock`, `OnceLock`, `OnceCell`); docs/deferred.md \
                  § Module-Level Mutable State",
    },
    CodeFamily {
        prefixes: &["E_GPU_"],
        codes: &["E0800"],
        pointer: "GPU kernels (`#[gpu]`, `gpu.upload`); docs/deferred.md § GPU",
    },
    CodeFamily {
        prefixes: &["E_REFINEMENT_"],
        codes: &["E_INVALID_REFINEMENT_PREDICATE"],
        pointer: "Refinement types (`type Even = i64 where self % 2 == 0`); docs/deferred.md \
                  § Refinement Types",
    },
    CodeFamily {
        prefixes: &["E_GAT_"],
        codes: &[],
        pointer: "Generic associated types; docs/deferred.md § Generic Associated Types (GATs)",
    },
    CodeFamily {
        prefixes: &["E_TARGET_FEATURE_"],
        codes: &[],
        pointer: "`#[target_feature]`; docs/deferred.md § Multiversioning: `cpu-baseline` and \
                  `#[multiversion]`",
    },
];

fn experimental_note(code: &str) -> Option<String> {
    family_of(EXPERIMENTAL_FAMILIES, code).map(|what| {
        let (feature, section) = what.split_once("; ").unwrap_or((what, "docs/deferred.md"));
        format!(
            "This code belongs to an experimental feature, {feature}: it is \
             implemented and runs in 0.8, but it is not part of the v1 language \
             and may change until its track lands ({section}). The message names \
             what is wrong and the form that works."
        )
    })
}

/// Codes no diagnostic emits any more. The number stays reserved so it is
/// never reused for another error; `explain` says it is retired and, where
/// one does, which code reports the case now (review-thread ruling
/// 2026-11-17 on B-2026-11-16-303).
const RETIRED_CODES: &[(&str, &str)] = &[
    ("E0103", "Access to an item the caller cannot see is E0111 PrivateItemAccess."),
    ("E0105", "A variant the enum does not declare is reported as E0236, no associated function of that name on the type."),
    ("E0220", "An unknown suffix on a number literal is a parse error, E0002."),
    ("E0274", "Indexing an iterator is E0275, which suggests `.collect()` into a `Vec`."),
    ("E0277", "`?.` is not in the language: `x?.f` is `(x?).f` (design.md § Results and propagation)."),
    ("E0401", "A public function may declare effects it does not perform; the extra effects are room for the API to grow (design.md § 12, Effects, the declaration paragraph)."),
    ("E0417", "A `Drop` body may panic; the panic ends the process (core-semantics.md § 7.11)."),
    ("E0508", "A closure that would outlive a value it borrows is E0515."),
];

fn retired_note(code: &str) -> Option<&'static str> {
    RETIRED_CODES
        .iter()
        .find(|(c, _)| *c == code)
        .map(|(_, note)| *note)
}

/// What `karac explain` says a code means, or `None` when there is nothing
/// but the message (B-2026-11-16-296, -297). A class paragraph describes
/// the class as a whole, so it is printed as the code's description only
/// where it fits: a later-track code gets [`SCOPE_PROSE`] instead, a
/// misfit its own prose alone, and a symbolic code with no prose of its own
/// says that the class paragraph is general. `sep` joins two paragraphs.
fn code_description(code: &str, entry: &CodeEntry, sep: &str) -> Option<String> {
    if let Some(text) = lint_code_description(code, sep) {
        return Some(text);
    }
    if let Some(note) = retired_note(code) {
        return Some(format!(
            "This code is retired: no diagnostic emits it any more, and its number \
             stays reserved so it is never reused.{sep}{note}"
        ));
    }
    let prose = code_prose(code);
    if let Some(note) = experimental_note(code) {
        return Some(match prose {
            Some(p) => format!("{note}{sep}{p}"),
            None => note,
        });
    }
    if is_scope_code(code, entry) {
        let scope = match deferred_section(code) {
            Some(pointer) => format!("{SCOPE_PROSE}{sep}{pointer}"),
            None => SCOPE_PROSE.to_string(),
        };
        return Some(match prose {
            Some(p) => format!("{scope}{sep}{p}"),
            None => scope,
        });
    }
    match (entry.class, prose) {
        (Some(_), Some(p)) if CLASS_MISFITS.contains(&code) => Some(p.to_string()),
        (Some(class), Some(p)) => Some(format!("{}{sep}{p}", class_description(class))),
        (Some(class), None) if code.starts_with("E_") || code.starts_with("W_") => Some(format!(
            "This code is filed under the {} class, which gathers many \
             diagnostics; the class's general description follows, and the \
             message is the authoritative account of this one.{sep}{}",
            class.as_str(),
            class_description(class)
        )),
        (Some(class), None) => Some(class_description(class).to_string()),
        (None, Some(p)) => Some(p.to_string()),
        (None, None) => None,
    }
}

/// What a lint's own code means (B-2026-11-16-299): the lint's registry
/// entry, how to change its level, and the code it used to share.
fn lint_code_description(code: &str, sep: &str) -> Option<String> {
    let (name, warn, error) = super::diag_json::LINT_CODES
        .iter()
        .find(|(_, w, e)| *w == code || *e == code)?;
    let info = crate::lints::lint_by_name(name)?;
    Some(format!(
        "The `{name}` lint: {}{sep}It is `{}` by default. `{warn}` is the lint \
         warning and `{error}` the same lint made an error by `-D {name}` or \
         `#[deny({name})]`; `-A {name}` or `#[allow({name})]` silences it. Until \
         0.8.2 it shared a code with other diagnostics; that code is the \
         record's `parent_code`, and `lint_name` is `{name}`.",
        info.description,
        info.default_level.as_attr_name(),
    ))
}

/// `karac explain <lint_name>`: the lint's own code page when it has one,
/// else its registry entry (B-2026-11-16-299).
fn lint_page(name: &str, format: ExplainFormat) -> Option<String> {
    let info = crate::lints::lint_by_name(name)?;
    let own = super::diag_json::LINT_CODES
        .iter()
        .find(|(n, _, _)| *n == name)
        .map(|(_, w, _)| *w);
    if let Some(code) = own {
        return Some(match format {
            ExplainFormat::Text => render_code_text(code, lookup_code(code)),
            ExplainFormat::Json => render_code_json(code, lookup_code(code)),
        });
    }
    let level = info.default_level.as_attr_name();
    Some(match format {
        ExplainFormat::Text => format!(
            "karac explain — lint: {name}\n\n{}\n\nIt is `{level}` by default; \
             `-A {name}` / `-W {name}` / `-D {name}`, or `#[allow({name})]` and the \
             other level attributes, change that. A diagnostic of this lint carries \
             `\"lint_name\": \"{name}\"` in `karac check --output=json`, and its \
             `code` is the one to pass to `karac explain` for the rest.\n",
            info.description,
        ),
        ExplainFormat::Json => format!(
            "{{\"kind\":\"lint\",\"name\":\"{name}\",\"default_level\":\"{level}\",\"description\":\"{}\"}}",
            escape_json_string(info.description),
        ),
    })
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
            "\nphase: {}\nkind:  {}\nconcept: {}\n",
            entry.phase,
            entry.kind,
            concept_for(entry.phase, code, None, None)
        ));
        match (entry.class, code_description(code, entry, "\n\n")) {
            (Some(class), Some(text)) => {
                out.push_str(&format!("class: {}\n\n{text}\n", class.as_str()));
            }
            (None, Some(text)) => out.push_str(&format!("class: OTHER\n\n{text}\n")),
            (_, None) => out.push_str(
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
            let description = code_description(code, e, " ").unwrap_or_else(|| {
                "No class page yet; the diagnostic message is authoritative.".to_string()
            });
            format!(
                "{{\"phase\":\"{}\",\"kind\":\"{}\",\"class\":\"{}\",\"concept\":\"{}\",\"description\":\"{}\"}}",
                e.phase,
                e.kind,
                class,
                concept_for(e.phase, code, None, None),
                escape_json_string(&description),
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
        .chain(CONCEPT_IDS.iter().map(|c| c.id))
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
        DiagnosticClass::InvalidBinaryOp,
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
        DiagnosticClass::InvalidBinaryOp => {
            "A binary operator was applied to operands it does not take: `+` on a \
             `String` whose right operand is not a `String` (append `.to_string()` to \
             a `Str`), or arithmetic on a type with no operator impl."
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
             drop `own` from a callee parameter that only reads (a bare parameter \
             borrows), or restructure to avoid the second use."
        }
        DiagnosticClass::OwnershipBorrowConflict => {
            "A live borrow conflicts with a later operation, or a place reached \
             through a `ref` is written. Includes ref-vs-mut-ref overlap, \
             slice-vs-ref overlap, drop-of-borrowed-source, call-site mut-marker \
             requirements that aren't met, and an assignment or `mut` borrow \
             through a borrowed parameter (core-semantics §5.6, §5.9)."
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
struct can each take their own mode; it is not in 0.8.

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

Diagnostic wording the ownership checker emits:

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
    a < b           PartialOrd   T.partial_cmp(a, b).is_lt()
    a <= b          PartialOrd   T.partial_cmp(a, b).is_le()
    a > b           PartialOrd   T.partial_cmp(a, b).is_gt()
    a >= b          PartialOrd   T.partial_cmp(a, b).is_ge()
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

The ordering rows name `partial_cmp` because the operators go through
`PartialOrd`, never `Ord` directly. A PRIMITIVE still uses the direct
`lt`/`le`/`gt`/`ge` the stdlib registers — those bodies exist. A USER
type goes through the comparator its impl supplies, because `trait
PartialOrd` declares `partial_cmp` and nothing else and `trait Ord`
declares `cmp` and nothing else, so there is no `lt` body on such a type
to call:

    impl PartialOrd  ->  T.partial_cmp(a, b).is_lt()   (the specified form)
    impl Ord only    ->  T.cmp(a, b).is_lt()           (equivalent fallback)

An impl that writes all four of `lt`/`le`/`gt`/`ge` itself keeps getting
those called. A PARTIAL set does not: with only `lt` written, taking it
for `<` while `>` went through the comparator would let a disagreeing
pair report both `a < b` and `a > b` true, so the direct form is used
only when all four are present.

`and` / `or` are short-circuit KEYWORDS, not trait-dispatched operators.
They have no trait and cannot be implemented.

Compound assignment desugars through the binary operator: `a += b` means
`a = a + b`, checked under `Add`. There is no `AddAssign` trait in 0.8.
When `a` is a `mut ref T` lvalue the desugar is assign-through —
`*a = *a + b` — so the caller's value is updated rather than the local
name being rebound.

────────────────────────────────────────────────────────────────────
Why your operator call failed
────────────────────────────────────────────────────────────────────

Six rejections cover nearly every case. Each row is the diagnostic you
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
    so it is not the two-Vec join. `append` does not exist either.

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

    A derive is not the only route. Hand-written comparison impls are
    dispatched to, and the derive is the wrong answer when you have one:
    it compares FIELD BY FIELD IN DECLARATION ORDER and never calls your
    body. `==` needs `impl PartialEq` plus the marker `impl Eq for T {}`;
    `< <= > >=` need an `impl Ord` supplying `cmp`. If you wrote only
    `impl PartialOrd`, see rejection 6.

3.  cannot mix integer types 'i32' and 'u32' in arithmetic — they must match
    cannot mix integer and floating-point operands ('i64' and 'f64')

    The narrower operand widens within its kind when that loses nothing:
    `(x: i32) + (y: i64)` is an `i64`, `(u: u8) + (w: i64)` too, and an
    `f32` widens to `f64`. The result is always one of the two operand
    types, so `i32 + u32` and `u64 + i64`, where neither holds every
    value of the other, need an `as` that chooses. Each operation runs in
    its own operands' type: `a_i32 * b_i32 + c_i64` multiplies (and
    traps on overflow) in `i32` before widening. An integer and a float
    never mix in an operator: write `x as f64`, so integer division done
    earlier stays visible. A numeric LITERAL takes the other operand's
    type, so `(x: i32) + 2` is fine.

4.  user-defined `impl Index for T` is not supported in 0.8;
    operator traits are stdlib-only

    Every command dispatches the arithmetic and bitwise
    operators to a user impl: `a + b` on a type with `impl Add` calls its
    `add`, `a += b` means `a = a + b`, and the same holds for `Sub Mul Div
    Rem BitAnd BitOr BitXor Shl Shr`, and for the unary `-a` (`Neg`) and
    `~a` (`Not`), and `c[k]` on a type with `impl Index` / `impl IndexMut`
    is the place their `index` / `index_mut` returns.
    The comparison operators `== != < <= > >=` dispatch to a user impl
    everywhere (see rejections 2 and 6).

5.  unary 'not' requires 'bool', found 'T'
    unary '~' requires an integer or integer-lane Vector type, found 'T'

    Kāra splits the two complements that Rust spells with one `!`, and
    neither accepts the other's operand. `not a` is logical negation and
    takes a `bool`; `~a` is the bitwise complement and takes an integer
    (or an integer-lane `Vector[T, N]`, complementing every lane). Both
    desugar to `Not.not(a)` — the operand type picks the impl. So write
    `~flags` to flip an integer's bits, and `not done` to negate a flag.

6.  ordering operators dispatch through 'cmp', which your
    'impl PartialOrd' and 'impl Ord' for 'T' does not define

    `< <= > >=` need ONE comparator that answers all four. An impl that
    supplies `partial_cmp` or `cmp` has it; an impl that hand-writes only
    SOME of `lt`/`le`/`gt`/`ge` does not, and the operators it left out
    have no body to call. Supplying a comparator is the fix — it covers
    all four by construction:

        impl PartialOrd for T {
            fn partial_cmp(ref self, other: ref T) -> Option[Ordering] { ... }
        }

    Do NOT reach for `#[derive(PartialOrd)]` here. It compiles, and then
    compares your fields in declaration order without ever calling the
    impl you wrote.

    An `impl PartialOrd` with no `impl Ord` is FINE and always was the
    specified shape — it drives the operators on its own. What `Ord`
    cannot do is skip `PartialOrd`: `Ord` requires it, and that is a
    separate rejection.

────────────────────────────────────────────────────────────────────
Which types implement what
────────────────────────────────────────────────────────────────────

  Arithmetic     numeric primitives; `Add` additionally on `String`
                 (`a + b` consumes `a`, borrows `b`, allocates)
  Bitwise        `& | ^ << >>` on the integer primitives only (NOT on
                 `bool` — use the `and` / `or` keywords). `Not` is reached
                 by two operators: `~a` complements an integer (and every
                 lane of an integer-lane Vector), `not a` negates a `bool`.
                 Neither accepts the other's operand type
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

Nothing, as of this build. Every rule above is measured against a
compiled program by tests/cli.rs, and the three divergences this
section used to list are all closed: two were design.md being wrong
about its own compiler (operator-boundary widening, and `Not` being
written `not` rather than `~`), and one was design.md naming
`concat` where only `extend` exists.

If you find a new one, add it here AND pin it to a program in
tests/cli.rs. A bullet claiming something is missing is exactly the
kind of note that stays behind after the gap is closed.

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
What this namespace does NOT give you
────────────────────────────────────────────────────────────────────

`StableHash.siphash24` is the whole namespace. Two things a reader
may arrive looking for are deliberately absent, and design.md § `Hash`
and `Hasher` agrees with this page on both.

    A CRYPTOGRAPHIC HASH.  There is none in the stdlib — no sha256,
    no blake3, no `crypto` namespace. Do NOT reach for `siphash24`
    instead: SipHash is a fast keyed PRF, not a collision-resistant
    hash, and it is unfit for signatures, for deduplication against
    an adversary, or for anything where a chosen collision is a
    problem. This is scheduled rather than undesigned —
    `deferred.md § std.crypto` commits BLAKE3 as the general-hashing
    algorithm; it is not in 0.8. Until then the honest answer is an
    external library.

    A FASTER UNKEYED DIGEST.  `xxh3` was named in an earlier draft of
    the spec and is NOT planned: BLAKE3 above is in the same speed
    class and is collision-resistant too, so a third option that is
    neither DoS-resistant nor collision-resistant would be a choice
    to get wrong for no new capability. Use `siphash24`.
";

const MODULE_STATE_PAGE: &str = "\
karac explain — Module state: bindings, effects, and the alternatives

Source of truth: docs/design.md § Module-Level Bindings. This page is
the concept-level summary; the design.md section is authoritative when
the two disagree.

This is the page the `module_mut_binding` warning points at.

Every command applies the core rules (design.md § Constants), and
there a module-level `let` or `let mut` is refused with E_MODULE_LET:
`const NAME: T = e;` is the only value declared at module scope, which
`karac fix` writes for a `let` that is already constant, and mutable
state is built in `main` and passed down. Module cells (`LazyLock`,
`OnceLock`, `OnceCell`) and `#[thread_local]` are not in 0.8. The rest
of this page describes the module-level bindings design.md specifies
beyond that, which 0.8 refuses.

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

    error[E0408]: module-level let mut 'COUNTER' cannot be written
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

  LazyLock[T]        Write-once, thread-safe, initialized by a
                     closure baked in at construction:
                     `let X: LazyLock[T] = LazyLock.new(|| ...)`,
                     read with `X.get()`. The closure runs on the
                     first `get` and at most once. For values
                     derivable from module constants — compile a
                     regex once, build a lookup table on first use.
                     Surface: `get` alone. Two specified rules are
                     NOT yet enforced: the closure's captures are
                     meant to be restricted to module-level
                     bindings, and first-access effects are meant to
                     be attributed to the calling function.

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

One primitive design.md names in this menu DOES NOT EXIST, and is
left out above rather than listed with a caveat, because a menu is
read as a list of things you can type:

    RwLock[T]     Not a type at all — `undefined type 'RwLock'`.
                  Use `Mutex[T]` at module scope.


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

// ── Concept ids (design.md §17 rule 4, B-2026-11-05-28) ─────────────

/// A concept id and the sections that specify it: `(doc, heading)`, the
/// heading as the document writes it, without its `#`s. design.md's
/// [concept table](../../docs/design.md#concept-ids) lists the same ids
/// against the same sections; `concept_ids_match_the_design_table` holds
/// the two together and resolves every heading.
pub struct ConceptId {
    pub id: &'static str,
    pub sections: &'static [(&'static str, &'static str)],
}

const D: &str = "docs/design.md";
const C: &str = "docs/core-semantics.md";

/// Every concept id a diagnostic carries, in the order design.md lists them.
pub const CONCEPT_IDS: &[ConceptId] = &[
    ConceptId {
        id: "syntax/lexical-structure",
        sections: &[(D, "3. Lexical structure")],
    },
    ConceptId {
        id: "modules/imports",
        sections: &[(D, "Modules"), (D, "Imports")],
    },
    ConceptId {
        id: "modules/constants",
        sections: &[(D, "Constants")],
    },
    ConceptId {
        id: "packages/manifests",
        sections: &[(D, "Packages and manifests")],
    },
    ConceptId {
        id: "names/resolution",
        sections: &[(D, "Imports"), (D, "Visibility")],
    },
    ConceptId {
        id: "names/visibility",
        sections: &[(D, "Visibility")],
    },
    ConceptId {
        id: "types/checking",
        sections: &[(D, "5. Types"), (D, "Type inference")],
    },
    ConceptId {
        id: "types/inference",
        sections: &[(D, "Type inference")],
    },
    ConceptId {
        id: "types/numeric-semantics",
        sections: &[(D, "Numeric semantics")],
    },
    ConceptId {
        id: "types/casts",
        sections: &[(D, "`as` casts")],
    },
    ConceptId {
        id: "types/indexing",
        sections: &[(D, "Indexing"), (D, "`Index` and `IndexMut`")],
    },
    ConceptId {
        id: "patterns/matching",
        sections: &[(D, "Pattern matching")],
    },
    ConceptId {
        id: "patterns/exhaustiveness",
        sections: &[(D, "Pattern exhaustiveness")],
    },
    ConceptId {
        id: "bindings/mutability",
        sections: &[(D, "Variable binding rules"), (D, "Assignment")],
    },
    ConceptId {
        id: "functions/calls",
        sections: &[(D, "Declarations"), (D, "Named and default parameters")],
    },
    ConceptId {
        id: "functions/parameter-modes",
        sections: &[(D, "Parameter modes"), (D, "Call-site `mut` markers")],
    },
    ConceptId {
        id: "functions/closures",
        sections: &[
            (D, "Closures"),
            (D, "Captures"),
            (D, "Function types and kinds"),
            (C, "9. Closures"),
        ],
    },
    ConceptId {
        id: "traits/bounds",
        sections: &[(D, "Bounds and generic bodies"), (D, "Supertraits")],
    },
    ConceptId {
        id: "traits/method-resolution",
        sections: &[(D, "Method resolution")],
    },
    ConceptId {
        id: "traits/coherence",
        sections: &[(D, "Coherence and the orphan rule"), (D, "Operator traits")],
    },
    ConceptId {
        id: "traits/drop",
        sections: &[(D, "`Drop`")],
    },
    ConceptId {
        id: "generics/monomorphization",
        sections: &[(D, "Monomorphization")],
    },
    ConceptId {
        id: "programs/main",
        sections: &[(D, "The `main` function and exit codes")],
    },
    ConceptId {
        id: "ownership/move-semantics",
        sections: &[(D, "The core rules in brief"), (C, "3. Moves")],
    },
    ConceptId {
        id: "ownership/borrowing",
        sections: &[
            (D, "The core rules in brief"),
            (C, "5. References and views (C5)"),
        ],
    },
    ConceptId {
        id: "ownership/initialization",
        sections: &[(D, "Variable binding rules"), (C, "2. Places")],
    },
    ConceptId {
        id: "ownership/sharing",
        sections: &[
            (D, "Shared types"),
            (D, "Frozen handles"),
            (C, "6. Sharing (C7)"),
        ],
    },
    ConceptId {
        id: "items/deprecation",
        sections: &[(D, "`#[deprecated]`")],
    },
    ConceptId {
        id: "items/non-exhaustive",
        sections: &[(D, "`#[non_exhaustive]`")],
    },
    ConceptId {
        id: "items/must-use",
        sections: &[(D, "`#[must_use]`")],
    },
    ConceptId {
        id: "effects/public-function-declaration",
        sections: &[(D, "Inference and declarations")],
    },
    ConceptId {
        id: "effects/inference-boundaries",
        sections: &[(D, "Inference and declarations")],
    },
    ConceptId {
        id: "effects/conflict-rules",
        sections: &[(D, "Conflict rules")],
    },
    ConceptId {
        id: "effects/effect-variables",
        sections: &[(D, "Effect variables")],
    },
    ConceptId {
        id: "effects/trait-methods",
        sections: &[(D, "Trait methods and generic calls")],
    },
    ConceptId {
        id: "effects/providers",
        sections: &[(C, "14. Providers (brought forward from M4a, 2026-10-20)")],
    },
    ConceptId {
        id: "concurrency/task-boundaries",
        sections: &[
            (D, "What may cross a task boundary"),
            (D, "Locks and atomics"),
        ],
    },
    ConceptId {
        id: "unsafe/ffi-and-layout",
        sections: &[(D, "15. Unsafe, FFI and layout control")],
    },
    ConceptId {
        id: "targets/effect-gating",
        sections: &[(D, "Effect-driven target gating")],
    },
    ConceptId {
        id: "targets/cpu-baseline",
        sections: &[(D, "CPU baseline")],
    },
    ConceptId {
        id: "tooling/attributes",
        sections: &[(D, "Attributes")],
    },
    ConceptId {
        id: "tooling/lint-levels",
        sections: &[(D, "Lint levels")],
    },
    ConceptId {
        id: "testing/test-blocks",
        sections: &[(D, "Test blocks")],
    },
    ConceptId {
        id: "compilation/pipeline",
        sections: &[(D, "Compilation model")],
    },
    ConceptId {
        id: "scope/later-tracks",
        sections: &[(D, "v1 scope"), (D, "Tracks")],
    },
];

/// The concept id `name` names, if it is one.
pub fn concept_id(name: &str) -> Option<&'static ConceptId> {
    CONCEPT_IDS.iter().find(|c| c.id == name)
}

/// A heading's anchor, as GitHub writes it: lower case, punctuation other
/// than `-` and `_` dropped, spaces as `-`.
pub fn heading_anchor(heading: &str) -> String {
    heading
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | ' '))
        .map(|c| if c == ' ' { '-' } else { c })
        .collect()
}

/// The concept a diagnostic carries, from what its record says about it:
/// the phase that emitted it, its code, its class and its lint. The code's
/// catalogue row names the compiler's own error kind, which is the finest
/// key; the class and then the phase decide the rest, so every diagnostic
/// has one (design.md §17 rule 4: required on errors and warnings).
pub fn concept_for(
    phase: &str,
    code: &str,
    class: Option<&str>,
    lint_name: Option<&str>,
) -> &'static str {
    if let Some(lint) = lint_name {
        match lint {
            "deprecated" => return "items/deprecation",
            "must_use" => return "items/must-use",
            "undocumented_unsafe" | "unsafe_op_in_unsafe_fn" | "ffi_float_eq" => {
                return "unsafe/ffi-and-layout"
            }
            _ => {}
        }
    }
    let row = CODE_TABLE
        .iter()
        .filter(|(c, _)| *c == code)
        .map(|(_, e)| e)
        .find(|e| e.phase == phase)
        .or_else(|| CODE_TABLE.iter().find(|(c, _)| *c == code).map(|(_, e)| e));
    if let Some(concept) = row.and_then(|e| concept_for_kind(e.kind)) {
        return concept;
    }
    let class = class
        .and_then(parse_class_name)
        .or_else(|| row.and_then(|e| e.class));
    if let Some(concept) = class.and_then(concept_for_class) {
        return concept;
    }
    match phase {
        "parse" | "lex" => "syntax/lexical-structure",
        "resolve" => "names/resolution",
        "module_graph" => "modules/imports",
        "manifest" | "walker" | "dep_resolution" | "toolchain_pin" | "lockfile" | "update"
        | "cache" | "install" | "vendor" => "packages/manifests",
        "effect" | "effects" => "effects/inference-boundaries",
        "ownership" => "ownership/move-semantics",
        "provider_escape" => "effects/providers",
        "simd_check" => "targets/cpu-baseline",
        "comptime" => "modules/constants",
        "monomorphization-budget" => "generics/monomorphization",
        "lint" => "tooling/lint-levels",
        "test" => "testing/test-blocks",
        "typecheck" => "types/checking",
        _ => "compilation/pipeline",
    }
}

/// The concept of a catalogued error kind, where the kind is finer than its
/// class (`BorrowConflict` is classed a type mismatch).
fn concept_for_kind(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "Syntax" | "UnexpectedToken" | "ReservedKeyword" | "RawIdentNotAllowed"
        | "ReservedSyntax" | "ReservedIdentifier" | "BindingNameClass" | "UnusedDocComment"
        | "AcronymCase" => "syntax/lexical-structure",
        "DuplicateLintLevel"
        | "UnknownLint"
        | "ForbiddenLintAllow"
        | "ExpectOnUnfulfilled"
        | "UnfulfilledLintExpectation" => "tooling/lint-levels",
        "UnknownAttribute"
        | "TrackCallerInvalidTarget"
        | "ProfileInvalidTarget"
        | "UnknownProfile"
        | "CompilerBuiltinReserved" => "tooling/attributes",
        "CircularModuleDependency"
        | "UnknownModule"
        | "UnknownItemInModule"
        | "AmbiguousWildcardImport" => "modules/imports",
        "NotInsideKaraProject" => "packages/manifests",
        "PrivateAccess" | "PrivateItemAccess" | "PrivateTypeInPublicSignature" => {
            "names/visibility"
        }
        "ModuleBindingEffectfulInit"
        | "ModuleBindingHeapType"
        | "ModuleLet"
        | "ReassignToImmutableModuleBinding"
        | "ModuleMutBinding"
        | "LazyLockRuntimeCapture" => "modules/constants",
        "Deprecated" | "DeprecatedOnImpl" | "DeprecatedOnField" => "items/deprecation",
        "NonExhaustiveInvalidTarget"
        | "NonExhaustiveCrossPackageLiteral"
        | "NonExhaustiveCrossPackageMatch"
        | "NonExhaustiveCrossPackagePattern"
        | "MissingNonExhaustive" => "items/non-exhaustive",
        "NonExhaustiveMatch" | "RefutablePattern" | "UnreachableArm" => "patterns/exhaustiveness",
        "PatternScrutineeMismatch"
        | "RangePatternBoundNotConst"
        | "AtBindingDoubleConsume"
        | "AmbiguousBareVariant"
        | "BareVariantConstructor" => "patterns/matching",
        "InvalidCast" | "RedundantCast" => "types/casts",
        "UnsupportedNumericSuffix" | "RedundantSuffix" => "types/numeric-semantics",
        "StringNotIndexable" | "IteratorNotIndexable" | "TypeNotIndexable" => "types/indexing",
        "CannotInferTypeParam" | "CannotInferAssocFn" => "types/inference",
        "OperatorTraitImplRestricted"
        | "IntoTraitImplNotAllowed"
        | "ConflictingImpl"
        | "OrphanImpl" => "traits/coherence",
        "DropBodyMayPanic" => "traits/drop",
        "MainReturnType" | "MainErrNotDisplay" | "MainSignature" => "programs/main",
        "ReassignToImmutable"
        | "MutateImmutableBinding"
        | "SharedFieldNotMut"
        | "AssignTargetNotPlace" => "bindings/mutability",
        "MissingMutMarker" | "InvalidMutMarker" | "ImplModeStrongerThanTrait" => {
            "functions/parameter-modes"
        }
        "FnKindMismatch"
        | "CapturePrefixRemoved"
        | "CaptureModeViolation"
        | "RefCaptureEscapesScope"
        | "UnusedMutCaptureNote"
        | "OnceFnIntoFnSlot" => "functions/closures",
        "UseAfterMove"
        | "MoveOutOfPlace"
        | "MovedFieldRefilledAcrossCall"
        | "MoveInCleanupBlock"
        | "MoveOutOfNoMovePlace"
        | "IndexMoveNonCopy" => "ownership/move-semantics",
        "BorrowConflict"
        | "WriteThroughSharedRef"
        | "BorrowReturnNotSourcePinned"
        | "RefOperandUnsupported" => "ownership/borrowing",
        "UseOfUninitialized" => "ownership/initialization",
        "OwnershipCycle"
        | "NoRcViolation"
        | "RcFallbackNote"
        | "RcFallbackOfDropType"
        | "FrozenParamEscapes"
        | "FrozenTypeNotFreezable" => "ownership/sharing",
        "MissingEffectDeclaration" | "OverDeclaredEffect" | "PubFnSyntheticResource" => {
            "effects/public-function-declaration"
        }
        "TraitEffectVarNotAllowed"
        | "EffectVarKindMismatch"
        | "EffectArgsMissingInPublicSignature"
        | "EffectVariableConflict" => "effects/effect-variables",
        "ImplExceedsTraitCeiling" | "TraitDefaultExceedsCeiling" => "effects/trait-methods",
        "NoProvider"
        | "StatefulDefault"
        | "ProviderUnsupported"
        | "HandleUsesOverride"
        | "DynProvider"
        | "TaskProviderConflict"
        | "ProviderEscape" => "effects/providers",
        "ParFieldNotConcurrent"
        | "ParMutSelfReceiver"
        | "CrossTaskUnsafeCapture"
        | "LockTargetNotMutex"
        | "ScopeLocalEscape"
        | "AtomicMissingOrdering"
        | "AtomicInvalidInnerType" => "concurrency/task-boundaries",
        "GpuInvalidTarget"
        | "GpuNotSafe"
        | "GpuEffectViolation"
        | "TargetGateViolation"
        | "ProfileViolation"
        | "ProfileIncompatibleEffect" => "targets/effect-gating",
        "OpaqueTypeRedeclared"
        | "ReprTransparentInvalid"
        | "DiscriminantInvalid"
        | "ExternSignatureInvalid"
        | "ReprArgumentInvalid"
        | "FfiLintHint" => "unsafe/ffi-and-layout",
        "RemovedInCore"
        | "DeferredToM4a"
        | "ArchivedWithLegacy"
        | "DeferredToLaterTrack"
        | "LaterTrack"
        | "DeferredTryBlock"
        | "DeferredTraitAlias"
        | "DeferredTait"
        | "DeferredEffectGroup"
        | "DeferredLayoutBlock" => "scope/later-tracks",
        _ => return None,
    })
}

/// The concept of a diagnostic class, for a kind [`concept_for_kind`] leaves
/// to it.
fn concept_for_class(class: DiagnosticClass) -> Option<&'static str> {
    use DiagnosticClass as K;
    Some(match class {
        K::TypeMismatch | K::UndefinedType | K::InvalidUnaryOp | K::InvalidBinaryOp => {
            "types/checking"
        }
        K::WrongNumberOfArgs => "functions/calls",
        K::NoMethodFound => "traits/method-resolution",
        K::InvalidCast => "types/casts",
        K::TraitBoundNotSatisfied => "traits/bounds",
        K::RefutablePattern => "patterns/exhaustiveness",
        K::CannotInferTypeParam => "types/inference",
        K::UndefinedName | K::DuplicateDefinition => "names/resolution",
        K::EffectUndeclared => "effects/public-function-declaration",
        K::EffectConflict => "effects/conflict-rules",
        K::OwnershipMoveAfterUse => "ownership/move-semantics",
        K::OwnershipBorrowConflict => "ownership/borrowing",
        K::OwnershipUseOfUninitialized => "ownership/initialization",
        K::TargetIncompatible => "targets/effect-gating",
        K::UnsafeRequired | K::FfiViolation | K::LayoutQueryInvalid => "unsafe/ffi-and-layout",
        K::LintWarning => "tooling/lint-levels",
        K::Other => return None,
    })
}

/// The catalogued codes whose diagnostics carry `concept`.
fn codes_of_concept(concept: &str) -> Vec<&'static str> {
    let mut codes: Vec<&'static str> = CODE_TABLE
        .iter()
        .filter(|(code, e)| concept_for(e.phase, code, None, None) == concept)
        .map(|(code, _)| *code)
        .collect();
    codes.dedup();
    codes
}

fn render_concept_id_text(c: &ConceptId) -> String {
    let mut out = format!("karac explain — concept: {}\n\nSpecified in:\n", c.id);
    for (doc, heading) in c.sections {
        out.push_str(&format!(
            "  {doc}#{}  ({heading})\n",
            heading_anchor(heading)
        ));
    }
    let codes = codes_of_concept(c.id);
    if !codes.is_empty() {
        out.push_str(&format!("\nDiagnostic codes: {}\n", codes.join(", ")));
    }
    out
}

fn render_concept_id_json(c: &ConceptId) -> String {
    let sections = c
        .sections
        .iter()
        .map(|(doc, heading)| {
            format!(
                "{{\"file\":\"{doc}\",\"section\":\"{}\",\"anchor\":\"{}\"}}",
                escape_json_string(heading),
                heading_anchor(heading)
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let codes = codes_of_concept(c.id)
        .iter()
        .map(|c| format!("\"{c}\""))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{{\"kind\":\"concept_id\",\"concept\":\"{}\",\"sections\":[{sections}],\"codes\":[{codes}]}}",
        c.id
    )
}

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
    /// Codes the spec assigns to one rule that two phases enforce. Each is
    /// the SAME diagnostic, so `karac explain` keeps its one row; the guard
    /// below still refuses any other code minted twice.
    const SPEC_SHARED_CODES: &[(&str, &str)] = &[(
        "E0111",
        "design.md §4: direct access to an invisible item is `E0111 \
         PrivateItemAccess`. The resolver refuses an item; the typechecker \
         refuses a field, since only it knows the struct a field belongs to \
         (B-2026-11-04-46).",
    )];

    #[test]
    fn emitter_mints_no_code_from_two_phases() {
        let mut collided: Vec<&str> = Vec::new();
        for (_, code) in emitted_codes() {
            if SPEC_SHARED_CODES.iter().any(|(c, _)| *c == code) {
                continue;
            }
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
        // An exemption that no longer applies is a stale one: each shared
        // code must still be minted by two phases.
        for (code, why) in SPEC_SHARED_CODES {
            let mut phases: Vec<&str> = emitted_codes()
                .into_iter()
                .filter(|(_, c)| c == code)
                .map(|(p, _)| p)
                .collect();
            phases.sort_unstable();
            phases.dedup();
            assert!(
                phases.len() > 1,
                "{code} is no longer shared ({why}): {phases:?}"
            );
        }
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
                // the prefix carries severity, not ownership.
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
    /// are outside the numbering by design; their own guard is
    /// [`every_minted_symbolic_code_is_catalogued`].
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
            P::RawIdentNotAllowed,
            P::ReservedSyntax,
            P::DuplicateLintLevel,
        ];
        assert_eq!(
            all.len(),
            6,
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
            ("E0417", E::DropBodyMayPanic),
            ("E0418", E::ParBranchConflict),
            ("E0419", E::NoProvider),
            ("E0420", E::StatefulDefault),
            ("E0421", E::ProviderUnsupported),
            ("E0422", E::HandleUsesOverride),
            ("E0423", E::DynProvider),
            ("E0424", E::TaskProviderConflict),
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
            ("E0513", O::MovedFieldRefilledAcrossCall),
            ("E0514", O::RcFallbackOfDropType),
            ("E0515", O::MoveOutOfPlace),
            ("E0522", O::MoveInCleanupBlock),
            (
                "E_SLICE_FROM_TEMPORARY_ESCAPES",
                O::SliceFromTemporaryEscapes,
            ),
            ("E_CROSS_BORROW_CONFLICT", O::CrossBorrowConflict),
            (
                "E_CLOSURE_CAPTURE_BORROW_CONFLICT",
                O::ClosureCaptureBorrowConflict,
            ),
            (
                "E_CONCURRENT_SHARED_STRUCT",
                O::ConcurrentSharedStruct {
                    type_name: String::new(),
                    binding: String::new(),
                },
            ),
            (
                "E_RC_FALLBACK_ALLOCATES_UNDER_FALLIBLE_PROFILE",
                O::RcFallbackAllocatesUnderFallibleProfile,
            ),
            (
                "E_EXCLUSIVE_BORROW_ALIASED_ARGS",
                O::ExclusiveBorrowAliasedArgs,
            ),
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
            ("E0283", K::RefOperandUnsupported),
            ("E0276", K::NilCoalesceNotWrapped),
            ("E0277", K::OptionalChainNotOption),
            ("E0270", K::AtomicMissingOrdering),
            ("E0271", K::ImplTraitMultipleWitnesses),
            ("E0272", K::AtomicInvalidInnerType),
            ("E0273", K::PatternScrutineeMismatch),
            ("E0801", K::GpuNotSafe),
            ("E0516", K::BorrowConflict),
            ("E0517", K::WriteThroughSharedRef),
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
    /// the vacuous `OTHER` page as though it were an explanation. E0250
    /// was the example until B-2026-11-16-296 gave it prose, so the test
    /// takes any unclassified code that has none.
    #[test]
    fn unclassified_code_says_so_rather_than_printing_other() {
        let (code, entry) = CODE_TABLE
            .iter()
            .find(|(c, e)| {
                e.class.is_none()
                    && code_prose(c).is_none()
                    && !is_scope_code(c, e)
                    && retired_note(c).is_none()
                    && experimental_note(c).is_none()
            })
            .expect("an unclassified code with no prose");
        let text = render_code_text(code, vec![*entry]);
        assert!(text.contains(entry.kind), "{text}");
        assert!(text.contains("no class page yet"), "{text}");
        let e0250 = render_code_text("E0250", lookup_code("E0250"));
        assert!(e0250.contains("ModuleBindingEffectfulInit"), "{e0250}");
        assert!(!e0250.contains("no class page yet"), "{e0250}");
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

    const CORE_SEMANTICS_MD: &str = include_str!("../../docs/core-semantics.md");

    /// design.md's concept table lists exactly [`CONCEPT_IDS`], in order,
    /// each against the sections the code names, and every section is a
    /// heading of its document (B-2026-11-05-28).
    #[test]
    fn concept_ids_match_the_design_table() {
        let table = &DESIGN_MD[DESIGN_MD.find("#### Concept ids").unwrap()..];
        let table = &table[..table.find("#### Error return traces").unwrap()];
        let rows: Vec<(&str, &str)> = table
            .lines()
            .filter_map(|l| l.strip_prefix("| `"))
            .filter_map(|l| l.split_once("` | "))
            .filter(|(_, sections)| {
                !sections.starts_with("Retired") && !sections.starts_with("Deferred")
            })
            .collect();
        let ids: Vec<&str> = rows.iter().map(|(id, _)| *id).collect();
        let want: Vec<&str> = CONCEPT_IDS.iter().map(|c| c.id).collect();
        assert_eq!(
            ids, want,
            "design.md's concept table and CONCEPT_IDS differ"
        );
        let anchors = |doc: &str| -> Vec<String> {
            doc.lines()
                .filter(|l| l.starts_with('#'))
                .map(|l| heading_anchor(l.trim_start_matches('#').trim()))
                .collect()
        };
        let (design, core) = (anchors(DESIGN_MD), anchors(CORE_SEMANTICS_MD));
        for (c, (_, cell)) in CONCEPT_IDS.iter().zip(&rows) {
            for (doc, heading) in c.sections {
                let anchor = heading_anchor(heading);
                let (headings, link) = if *doc == D {
                    (&design, format!("(#{anchor})"))
                } else {
                    (&core, format!("(core-semantics.md#{anchor})"))
                };
                assert!(
                    headings.contains(&anchor),
                    "{}: no heading `{heading}` in {doc}",
                    c.id
                );
                assert!(
                    cell.contains(&link),
                    "{}: the table does not link {link}",
                    c.id
                );
            }
        }
    }

    /// Every catalogued code has a concept from the table, and the spec's
    /// own examples get the ids it shows.
    #[test]
    fn every_catalogued_code_has_a_concept() {
        for (code, e) in CODE_TABLE {
            let c = concept_for(e.phase, code, None, None);
            assert!(concept_id(c).is_some(), "{code}: {c} is not a concept id");
        }
        let at = |phase, code| concept_for(phase, code, None, None);
        assert_eq!(at("ownership", "E0500"), "ownership/move-semantics");
        assert_eq!(at("effect", "E0400"), "effects/public-function-declaration");
        // The kind is finer than the class: E0516 is classed a mismatch.
        assert_eq!(at("typecheck", "E0516"), "ownership/borrowing");
        // An uncatalogued symbolic code goes by its class, then its phase.
        assert_eq!(
            concept_for(
                "ownership",
                "E_SLICE_BORROW_CONFLICT",
                Some("OWNERSHIP_BORROW_CONFLICT"),
                None
            ),
            "ownership/borrowing"
        );
        assert_eq!(
            concept_for("manifest", "E0999", None, None),
            "packages/manifests"
        );
        assert_eq!(
            concept_for("typecheck", "W0245", None, Some("deprecated")),
            "items/deprecation"
        );
        for phase in ["no-such-phase", "build", "test"] {
            assert!(concept_id(concept_for(phase, "", None, None)).is_some());
        }
    }

    /// The `E_*` codes the compiler's own code mints: every `"E_NAME"`
    /// string literal under src/, and every `error[E_NAME]` a message opens
    /// with (B-2026-11-14-10), outside comments and `#[cfg(test)]` modules
    /// (whose fixtures, `E_TEST` and the like, are not codes).
    fn minted_symbolic_codes() -> Vec<(String, String)> {
        fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            for entry in std::fs::read_dir(dir).unwrap().flatten() {
                let p = entry.path();
                if p.is_dir() {
                    walk(&p, out);
                } else if p.extension().is_some_and(|e| e == "rs") {
                    out.push(p);
                }
            }
        }
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        walk(&root, &mut files);
        // The runtime mints the codes of its own panics (`runtime error:
        // E_STRING_SLICE_NOT_AT_CHAR_BOUNDARY: ..`).
        let runtime = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("runtime/src");
        walk(&runtime, &mut files);
        let mut out = Vec::new();
        for path in files {
            let text = std::fs::read_to_string(&path).unwrap();
            // Cut at the first INLINE test module, `mod tests {`. An
            // out-of-line one, `#[cfg(test)] mod tests;`, holds nothing of
            // this file, and cutting there once left 9000 lines of
            // typechecker.rs unscanned (B-2026-11-15-20).
            let mut end = text.len();
            let mut at = 0;
            while let Some(hit) = text[at..].find("#[cfg(test)]") {
                let start = at + hit;
                let rest = text[start + "#[cfg(test)]".len()..].trim_start();
                let rest = rest.strip_prefix("pub ").unwrap_or(rest);
                if let Some(m) = rest.strip_prefix("mod ") {
                    let after = m.trim_start_matches(|c: char| c.is_alphanumeric() || c == '_');
                    if after.trim_start().starts_with('{') {
                        end = start;
                        break;
                    }
                }
                at = start + 1;
            }
            let file = path
                .strip_prefix(&root)
                .or_else(|_| path.strip_prefix(&runtime))
                .unwrap()
                .display()
                .to_string();
            for line in text[..end].lines() {
                if line.trim_start().starts_with("//") {
                    continue;
                }
                for (open, close) in [
                    ("\"E_", b'"'),
                    ("error[E_", b']'),
                    ("\"W_", b'"'),
                    ("warning[W_", b']'),
                    ("runtime error: E_", b':'),
                    ("\"E_", b':'),
                ] {
                    let b = line.as_bytes();
                    let mut at = 0;
                    while let Some(hit) = line[at..].find(open) {
                        let name = at + hit + open.len() - 2;
                        let name_end = (name + 2..b.len())
                            .find(|&j| {
                                !(b[j].is_ascii_uppercase()
                                    || b[j].is_ascii_digit()
                                    || b[j] == b'_')
                            })
                            .unwrap_or(b.len());
                        if name_end > name + 2
                            && name_end < b.len()
                            && b[name_end] == close
                            && b[name_end - 1] != b'_'
                        {
                            out.push((line[name..name_end].to_string(), file.clone()));
                        }
                        at = name_end;
                    }
                }
            }
        }
        out
    }

    /// design.md §2: every code is catalogued for `karac explain`. A symbolic
    /// code minted anywhere in src/ without a [`CODE_TABLE`] row fails here
    /// (B-2026-11-13-20: 84 of them had none, and `explain` refused each;
    /// B-2026-11-14-10: 166 more written only into their messages).
    #[test]
    fn every_minted_symbolic_code_is_catalogued() {
        let minted = minted_symbolic_codes();
        assert!(
            minted.len() >= 250,
            "the scan found only {} minted codes; it is not seeing src/",
            minted.len()
        );
        let mut missing: Vec<String> = minted
            .into_iter()
            .filter(|(code, _)| lookup_code(code).is_empty())
            .map(|(code, file)| format!("{code} ({file})"))
            .collect();
        missing.sort();
        missing.dedup();
        assert!(
            missing.is_empty(),
            "symbolic codes minted in src/ with no CODE_TABLE row: {missing:?}"
        );
        for code in [
            "E_DEPENDENCY_CYCLE",
            "E_CROSS_BORROW_CONFLICT",
            "E_TEST_BLOCK_NOT_TOP_LEVEL",
            "E_CHAR_AS_NARROW_INT",
            "E_INSTALL_HOME_UNSET",
            // Past typechecker.rs's out-of-line test module, a warning, and
            // a runtime panic's code (B-2026-11-15-20).
            "E_INT_AS_BOOL",
            "E_SHAPE",
            "W_DEPENDENCY_YANKED",
            "E_STRING_SLICE_NOT_AT_CHAR_BOUNDARY",
        ] {
            assert!(is_symbolic_code(code), "{code}");
        }
    }

    /// The codes a newcomer meets explain themselves: none falls back to the
    /// "no class page yet" stub, and E0517 is explained as the borrow rule it
    /// is, not as a type mismatch (B-2026-11-07-47).
    #[test]
    fn newcomer_codes_have_prose() {
        for code in [
            "E0001",
            "E0002",
            "E0205",
            "E0254",
            "E0506",
            "E0509",
            "E0510",
            "E_NOT_CROSS_TASK",
            "E0516",
            "E0517",
        ] {
            let entries = lookup_code(code);
            assert!(!entries.is_empty(), "{code} is not catalogued");
            let text = render_code_text(code, entries.clone());
            assert!(
                !text.contains("no class page yet"),
                "{code} prints the stub: {text}"
            );
            assert!(text.contains(code_prose(code).unwrap()), "{code}: {text}");
            let json = render_code_json(code, entries);
            assert!(!json.contains("No class page yet"), "{code}: {json}");
        }
        let text = render_code_text("E0517", lookup_code("E0517"));
        assert!(text.contains("class: OWNERSHIP_BORROW_CONFLICT"), "{text}");
        assert!(!text.contains("TYPE_MISMATCH"), "{text}");
        assert!(text.contains("mut ref"), "{text}");
    }
}
