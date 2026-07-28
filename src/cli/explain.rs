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
}

impl ExplainConcept {
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "closures" => Some(ExplainConcept::Closures),
            _ => None,
        }
    }

    pub fn page(self) -> &'static str {
        match self {
            ExplainConcept::Closures => CLOSURES_PAGE,
        }
    }

    /// Wire-form name for the JSON envelope.
    pub fn as_str(self) -> &'static str {
        match self {
            ExplainConcept::Closures => "closures",
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
/// **Scope.** Covers the two families whose codes an agent loop
/// actually sees on a failing `karac check`: `typecheck` (the `E02xx`
/// / `W02xx` band plus the `E08xx` target-gate strays) and `resolve`
/// (`E01xx` plus its `E022x`–`E024x` strays). Codes minted by the
/// parse / effect / ownership phases are not here yet; `explain`
/// reports them as uncatalogued rather than guessing.
///
/// **Source of truth.** Every row restates a `match err.kind` arm in
/// `collect_diagnostics` (`src/cli.rs`) crossed with
/// `class_for_type_error_kind` (`src/typechecker.rs`). Those two
/// functions are exhaustive matches, so adding an error kind is a
/// compile error there — but *not* here. The
/// `code_table_class_matches_typechecker` test pins the rows that
/// would otherwise drift silently.
///
/// **Collisions are real and intentional to surface.** `E0222`,
/// `E0238`, `E0239`, and `E0240` are each minted by *both* the
/// resolver and the typechecker for unrelated errors, so a lookup can
/// return more than one row. That is a pre-existing defect in the code
/// allocation (see the bug ledger); rendering every match is the
/// honest response until the codes are split.
const CODE_TABLE: &[(&str, CodeEntry)] = &[
    // ── resolve ─────────────────────────────────────────────────
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
    ("E0222", res("PrivateItemAccess", None)),
    ("E0224", res("UnknownModule", None)),
    ("E0225", res("UnknownItemInModule", None)),
    ("E0228", res("ReservedEffectResource", None)),
    ("E0237", res("CompilerBuiltinReserved", None)),
    ("E0238", res("ContinueOnBlockLabel", None)),
    ("E0239", res("NonExhaustiveInvalidTarget", None)),
    ("E0240", res("TrackCallerInvalidTarget", None)),
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
    ("E0250", ty("ModuleBindingEffectfulInit", None)),
    ("E0251", ty("ModuleBindingHeapType", None)),
    ("E0252", ty("ReassignToImmutableModuleBinding", None)),
    ("E0253", ty("ScopeLocalEscape", None)),
    ("E0254", ty("CrossTaskUnsafeCapture", None)),
    (
        "E0255",
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
    ("E0801", ty("GpuNotSafe", None)),
    ("E0803", ty("ReprTransparentInvalid", None)),
    ("E0804", ty("DiscriminantInvalid", None)),
];

const fn ty(kind: &'static str, class: Option<DiagnosticClass>) -> CodeEntry {
    CodeEntry {
        phase: "typecheck",
        kind,
        class,
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
    let looks_like_a_code = {
        let mut chars = code.chars();
        matches!(chars.next(), Some('E') | Some('W')) && chars.all(|c| c.is_ascii_digit())
    };
    if looks_like_a_code {
        format!(
            "diagnostic code '{code}' is not in the catalogue yet. \
             `karac explain` covers the resolve and typecheck families \
             (E01xx, E02xx / W02xx, E08xx); codes minted by the parse, \
             effect, and ownership phases are not catalogued. Look the \
             diagnostic up by class instead — `karac check --output=json` \
             reports one in each record's `class` field, and \
             `karac explain --class=NAME` explains it. Supported classes: {}.",
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

fn concept_list() -> String {
    // Single concept today; the list shape future-proofs against
    // additional pages without rewriting the dispatch surface.
    "closures".to_string()
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::typechecker::TypeErrorKind as K;

    /// The four codes that two phases both mint. This test exists to
    /// make the collision *visible* rather than to bless it: if a
    /// future commit splits the codes apart, this test fails and the
    /// fix is to shrink the expected set (and drop the collision note
    /// from `render_code_text`). If a NEW collision appears, it fails
    /// too — which is the point, since a silently reused code makes
    /// `explain` and every agent consuming the `code` field ambiguous.
    #[test]
    fn code_table_collisions_are_the_documented_set() {
        let mut collided: Vec<&str> = CODE_TABLE
            .iter()
            .filter(|(code, _)| lookup_code(code).len() > 1)
            .map(|(code, _)| *code)
            .collect();
        collided.sort_unstable();
        collided.dedup();
        assert_eq!(
            collided,
            vec!["E0222", "E0238", "E0239", "E0240"],
            "diagnostic-code collisions changed; see CODE_TABLE's doc comment"
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

    #[test]
    fn collision_render_names_every_phase() {
        let entries = lookup_code("E0222");
        assert_eq!(entries.len(), 2);
        let text = render_code_text("E0222", entries);
        assert!(text.contains("more than one phase"));
        assert!(text.contains("PrivateItemAccess"));
        assert!(text.contains("RefutablePattern"));
    }

    #[test]
    fn uncatalogued_code_points_at_the_class_surface() {
        assert!(lookup_code("E0001").is_empty());
        let msg = unknown_code_message("E0001");
        assert!(msg.contains("not in the catalogue yet"));
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
