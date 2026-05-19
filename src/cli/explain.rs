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
