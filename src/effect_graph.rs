//! Whole-program effect-graph emission — the data behind Cartographer.
//!
//! This module is **wasm-safe** (no `std::process`, no filesystem) so the
//! same code path serves both the native CLI (`karac query effects
//! <file>` / `query concurrency <file>`) and the browser studio (the
//! `karac-playground` WASM `cartograph` export). The CLI emitters in
//! `src/cli.rs` delegate to the JSON builders here, and [`cartograph_json`]
//! drives the analysis pipeline directly — so every surface produces a
//! byte-identical graph.
//!
//! The graph itself: one node per source-defined function (free fn, impl
//! method, trait default method) carrying its inferred + declared effects,
//! the directed call-graph edges between them, and — from the concurrency
//! pass — each function's parallel bands. Node keys (`fn` / `Type.method`)
//! join 1:1 across the effect and concurrency envelopes and with
//! `karac query affected-by`. See `docs/dogfooding.md` § Cartographer.

use std::fmt::Write as _;

use crate::ast::EffectVerbKind;
use crate::call_graph::CallGraph;
use crate::concurrency::{ConcurrencyAnalysis, FunctionConcurrency, SerializationCause};
use crate::effectchecker::{DeclaredEffects, EffectCheckResult, EffectSet};
use crate::token::Span;

// ── JSON helpers (module-local so this stays free of the CLI layer) ──────────

/// Escape and quote `s` as a JSON string literal.
fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c < '\x20' => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn effect_verb_str(v: &EffectVerbKind) -> &str {
    match v {
        EffectVerbKind::Reads => "reads",
        EffectVerbKind::Writes => "writes",
        EffectVerbKind::Sends => "sends",
        EffectVerbKind::Receives => "receives",
        EffectVerbKind::Allocates => "allocates",
        EffectVerbKind::Panics => "panics",
        EffectVerbKind::Blocks => "blocks",
        EffectVerbKind::Suspends => "suspends",
        EffectVerbKind::UserDefined(s) => s.as_str(),
    }
}

/// Render a source span as a `{"file","line","column"}` JSON object.
/// Mirrors `cli::span_to_json`'s field shape (kept module-local so this
/// stays free of the CLI layer and wasm-safe).
fn span_json(span: &Span, filename: &str) -> String {
    format!(
        "{{\"file\":{},\"line\":{},\"column\":{}}}",
        json_string(filename),
        span.line,
        span.column,
    )
}

/// Render a function's per-statement spans as a JSON array indexed by the
/// same ordinal used in `parallel_groups`/`serialization_points`, so the
/// concurrency surface is self-locating: `statement_spans[i]` locates the
/// statement referenced by ordinal `i`.
pub(crate) fn statement_spans_json(fc: &FunctionConcurrency, filename: &str) -> String {
    let entries: Vec<String> = fc
        .statement_spans
        .iter()
        .map(|s| span_json(s, filename))
        .collect();
    format!("[{}]", entries.join(","))
}

/// Render a [`SerializationCause`] as the structured `serialized_by`
/// object — the machine-readable counterpart to the prose `reason`.
fn serialized_by_json(cause: &SerializationCause) -> String {
    match cause {
        SerializationCause::SeqOrdering => "{\"category\":\"seq_ordering\"}".to_string(),
        SerializationCause::DataDependency { kind, vars } => {
            let vars_json: Vec<String> = vars.iter().map(|v| json_string(v)).collect();
            format!(
                "{{\"category\":\"data_dependency\",\"kind\":{},\"vars\":[{}]}}",
                json_string(kind.as_str()),
                vars_json.join(","),
            )
        }
        SerializationCause::PolymorphicEffect => {
            "{\"category\":\"polymorphic_effect\"}".to_string()
        }
        SerializationCause::EffectConflict { resource, verbs } => {
            format!(
                "{{\"category\":\"effect_conflict\",\"resource\":{},\"verbs\":[{},{}]}}",
                json_string(resource),
                json_string(effect_verb_str(&verbs.0)),
                json_string(effect_verb_str(&verbs.1)),
            )
        }
    }
}

// ── JSON builders (shared with the CLI per-function + whole-program paths) ────

/// Render an `EffectSet` as a JSON array of `{"verb","resource"}` objects.
pub(crate) fn effect_set_json(set: &EffectSet) -> String {
    let list: Vec<String> = set
        .effects
        .iter()
        .map(|te| {
            format!(
                "{{\"verb\":{},\"resource\":{}}}",
                json_string(effect_verb_str(&te.effect.verb)),
                json_string(&te.effect.resource),
            )
        })
        .collect();
    format!("[{}]", list.join(","))
}

/// Render a function's `declared_effects` JSON value: `null` (none /
/// absent), `"polymorphic"`, an explicit array, or the
/// polymorphic-with-fixed object.
pub(crate) fn declared_effects_json(declared: Option<&DeclaredEffects>) -> String {
    match declared {
        Some(DeclaredEffects::Explicit(set)) => effect_set_json(set),
        Some(DeclaredEffects::Polymorphic) => "\"polymorphic\"".to_string(),
        Some(DeclaredEffects::PolymorphicWithFixed(set)) => {
            format!(
                "{{\"polymorphic\":true,\"fixed\":{}}}",
                effect_set_json(set)
            )
        }
        Some(DeclaredEffects::None) | None => "null".to_string(),
    }
}

/// Render a function's `parallel_groups` as a JSON array of
/// `{"statements":[…],"reason":…}` objects.
pub(crate) fn parallel_groups_json(fc: &FunctionConcurrency) -> String {
    let group_entries: Vec<String> = fc
        .parallel_groups
        .iter()
        .map(|g| {
            let indices: Vec<String> = g.statement_indices.iter().map(|i| i.to_string()).collect();
            format!(
                "{{\"statements\":[{}],\"reason\":{}}}",
                indices.join(","),
                json_string(&g.reason),
            )
        })
        .collect();
    format!("[{}]", group_entries.join(","))
}

/// Render a function's `serialization_points` as a JSON array of
/// `{"statements":[…],"reason":…,"resource":…,"blocking_callees":[…]}`
/// objects — the inverse of `parallel_groups`. Inverting `blocking_callees`
/// across functions yields the "which callers does this function block"
/// attribution view.
pub(crate) fn serialization_points_json(fc: &FunctionConcurrency) -> String {
    let entries: Vec<String> = fc
        .serialization_points
        .iter()
        .map(|sp| {
            let indices: Vec<String> = sp.statement_indices.iter().map(|i| i.to_string()).collect();
            let callees: Vec<String> = sp.blocking_callees.iter().map(|c| json_string(c)).collect();
            format!(
                "{{\"statements\":[{}],\"reason\":{},\"resource\":{},\"blocking_callees\":[{}],\"serialized_by\":{}}}",
                indices.join(","),
                json_string(&sp.reason),
                json_string(&sp.resource),
                callees.join(","),
                serialized_by_json(&sp.cause),
            )
        })
        .collect();
    format!("[{}]", entries.join(","))
}

/// Render a function's `reorder_opportunities` as a JSON array of
/// `{"statements":[i,j],"movable_statement":m,"reason":…}` objects — the
/// deterministic "a legal reorder would expose more parallelism here"
/// advisory. `statements` are independent ordinals (index into
/// `statement_spans`); `movable_statement` is the one that can slide
/// adjacent to its partner.
pub(crate) fn reorder_opportunities_json(fc: &FunctionConcurrency) -> String {
    let entries: Vec<String> = fc
        .reorder_opportunities
        .iter()
        .map(|op| {
            let indices: Vec<String> = op.statement_indices.iter().map(|i| i.to_string()).collect();
            format!(
                "{{\"statements\":[{}],\"movable_statement\":{},\"reason\":{}}}",
                indices.join(","),
                op.movable_statement,
                json_string(&op.reason),
            )
        })
        .collect();
    format!("[{}]", entries.join(","))
}

/// Build the whole-program effect-graph JSON envelope: effect-annotated
/// nodes (one per source function) plus the directed call-graph edges.
pub(crate) fn build_effect_graph_json(
    effects: &EffectCheckResult,
    graph: &CallGraph,
    scope: &str,
) -> String {
    let fn_entries: Vec<String> = graph
        .nodes
        .iter()
        .map(|(key, node)| {
            let inferred_str = effects
                .inferred_effects
                .get(key)
                .map(effect_set_json)
                .unwrap_or_else(|| "[]".to_string());
            format!(
                "{{\"function\":{},\"line\":{},\"is_test\":{},\"inferred_effects\":{},\"declared_effects\":{}}}",
                json_string(key),
                node.line,
                node.is_test,
                inferred_str,
                declared_effects_json(effects.declared_effects.get(key)),
            )
        })
        .collect();

    let mut edges: Vec<String> = Vec::new();
    for (caller, callees) in &graph.forward {
        for callee in callees {
            edges.push(format!(
                "{{\"caller\":{},\"callee\":{}}}",
                json_string(caller),
                json_string(callee),
            ));
        }
    }

    format!(
        "{{\"scope\":{},\"functions\":[{}],\"calls\":[{}]}}",
        json_string(scope),
        fn_entries.join(","),
        edges.join(","),
    )
}

/// Build the whole-program concurrency JSON envelope: one entry per
/// analyzed function (in call-graph key order) with its statement count
/// and parallel bands.
pub(crate) fn build_concurrency_graph_json(
    analysis: &ConcurrencyAnalysis,
    graph: &CallGraph,
    scope: &str,
) -> String {
    let fn_entries: Vec<String> = graph
        .nodes
        .iter()
        .filter_map(|(key, node)| {
            analysis.function_decisions.get(key).map(|fc| {
                format!(
                    "{{\"function\":{},\"line\":{},\"total_statements\":{},\"statement_spans\":{},\"parallel_groups\":{},\"serialization_points\":{},\"reorder_opportunities\":{}}}",
                    json_string(key),
                    node.line,
                    fc.total_statements,
                    statement_spans_json(fc, scope),
                    parallel_groups_json(fc),
                    serialization_points_json(fc),
                    reorder_opportunities_json(fc),
                )
            })
        })
        .collect();

    format!(
        "{{\"scope\":{},\"functions\":[{}]}}",
        json_string(scope),
        fn_entries.join(","),
    )
}

// ── Library / WASM entry point ────────────────────────────────────────────────

/// One diagnostic for the Cartographer live editor — same shape as
/// `PlaygroundDiagnostic`, surfaced so the browser studio can decorate
/// the editor with the type / effect errors the compiler found while the
/// user edits.
pub struct CartographDiagnostic {
    pub phase: &'static str,
    pub message: String,
    pub line: usize,
    pub column: usize,
    pub offset: usize,
    pub length: usize,
}

/// Result of [`cartograph_json`]: the two whole-program graph envelopes
/// (effects + concurrency, byte-identical to the CLI `query effects` /
/// `query concurrency` output) plus any diagnostics. On a fatal
/// parse/resolve error `ok` is false and the JSON strings are empty — the
/// caller keeps its last good graph and renders the diagnostics.
pub struct CartographResult {
    pub ok: bool,
    pub effects_json: String,
    pub concurrency_json: String,
    pub diagnostics: Vec<CartographDiagnostic>,
}

fn push_diag(
    out: &mut Vec<CartographDiagnostic>,
    phase: &'static str,
    message: &str,
    span: &crate::token::Span,
) {
    out.push(CartographDiagnostic {
        phase,
        message: message.to_string(),
        line: span.line,
        column: span.column,
        offset: span.offset,
        length: span.length,
    });
}

/// Whole-program effect + concurrency graph for `source`, as the two JSON
/// envelopes the CLI `query effects <file>` / `query concurrency <file>`
/// commands emit. This is the library entry point the `karac-playground`
/// WASM `cartograph` export wraps for the Cartographer browser studio.
///
/// The analysis mirrors the CLI query path: parse → desugar → resolve →
/// typecheck → lower → effect-check (with the typechecker's
/// `method_callee_types`, so effects propagating through method calls
/// resolve precisely) → concurrency. `concurrency_analyze` depends only
/// on `(program, effects)` — not on the codegen-oriented program tables
/// the CLI `Pipeline` also populates — so the graph is identical to the
/// CLI's (pinned by `tests/cli.rs::test_cartograph_json_matches_cli_query_output`).
///
/// `scope` is the logical file name stamped into the `scope` field and
/// used for the `_test.kara` test-node heuristic. Never panics: a fatal
/// parse/resolve error returns `ok:false` with diagnostics and empty
/// envelopes; typecheck/effect errors are non-fatal (the graph still
/// builds, mirroring the CLI query) and are surfaced in `diagnostics`.
pub fn cartograph_json(source: &str, scope: &str) -> CartographResult {
    let mut diagnostics = Vec::new();

    let mut parsed = crate::parse(source);
    if !parsed.errors.is_empty() {
        for e in &parsed.errors {
            push_diag(&mut diagnostics, "parse", &e.message, &e.span);
        }
        return CartographResult {
            ok: false,
            effects_json: String::new(),
            concurrency_json: String::new(),
            diagnostics,
        };
    }

    crate::desugar_program(&mut parsed.program);

    let resolved = crate::resolve(&parsed.program);
    if !resolved.errors.is_empty() {
        for e in &resolved.errors {
            push_diag(&mut diagnostics, "resolve", &e.message, &e.span);
        }
        return CartographResult {
            ok: false,
            effects_json: String::new(),
            concurrency_json: String::new(),
            diagnostics,
        };
    }

    let typed = crate::typecheck(&parsed.program, &resolved);
    for e in &typed.errors {
        push_diag(&mut diagnostics, "typecheck", &e.message, &e.span);
    }

    crate::lower(&mut parsed.program, &typed);

    // Thread the typechecker's method-callee resolution so effects that
    // propagate through method calls (`obj.m()`) surface — the same data
    // the CLI `Pipeline::effectcheck` threads; without it a method-routed
    // `reads(R)` would be invisible.
    let effects = crate::effectcheck_with_typecheck_data(
        &parsed.program,
        crate::effectchecker::PublicEffectsPolicy::default(),
        crate::manifest::ProfileConfig::default(),
        typed.method_callee_types.clone(),
        typed.call_type_subs.clone(),
    );
    for e in &effects.errors {
        push_diag(&mut diagnostics, "effect", &e.message, &e.span);
    }

    let analysis = crate::concurrency_analyze(&parsed.program, &effects);

    let is_test_file = scope.ends_with("_test.kara");
    let graph = crate::call_graph::build(&parsed.program, scope, is_test_file);

    CartographResult {
        ok: true,
        effects_json: build_effect_graph_json(&effects, &graph, scope),
        concurrency_json: build_concurrency_graph_json(&analysis, &graph, scope),
        diagnostics,
    }
}
