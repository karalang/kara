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

use crate::ast::{Block, Expr, ExprKind, Function, Program, StmtKind};
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

// The verb→keyword spelling lives once, in `effect_render`; this wasm-safe
// JSON emitter shares it so the CLI/browser-studio graph stays byte-identical
// to every other rendering. (Kept as a module-local alias so the many call
// sites below read unchanged.)
use crate::effect_render::verb_keyword as effect_verb_str;

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

/// Render a function's recognized `loop_reductions` as a JSON array — the
/// **Tier-2 loop fan-out** decisions (`design.md § 8876` "compute fan-out …
/// associative reductions"), which are a *separate* mechanism from the
/// statement-level `parallel_groups` above and were previously absent from
/// this surface entirely (B-2026-07-29-29).
///
/// Field contract, deliberately named so no field over-promises:
///
/// - `lowering` — what the analysis decided this loop becomes:
///   `"parallel_fanout"` (dispatched across the worker pool) or
///   `"sequential_tabulate"` (the single-threaded push→in-place-store
///   rewrite, which is *not* parallel). `LoopReduction::seq` is the
///   discriminator; conflating the two is what made a recognized-but-
///   sequential loop read as parallelized.
/// - `fanned_out` — the question a caller actually has: does this loop
///   dispatch across the worker pool in the *emitted binary*? Recognition and
///   emission are different answers, and reporting the first as the second was
///   the original defect.
/// - `cost_gate` — which gate decided. `"fanout"` when the loop dispatches, the
///   declining gate's name when it does not (`declined_memory_bound`,
///   `declined_below_cost_threshold`, `declined_variable_k_param_bound`), and
///   `"n/a"` on a sequential-tabulate entry, which has no dispatch to gate.
/// - `reason` — the same decision in prose.
/// - `collect_tabulate` — the exactly-one-unconditional-push shape that
///   licenses in-place slot writes (order-preserving).
///
/// The verdict comes from [`crate::par_cost::fanout_verdict`] — the SAME
/// function codegen calls (B-2026-07-29-33) — so the query cannot drift from
/// the binary. Before that extraction the gates lived in llvm-gated codegen
/// and this surface could only disclaim them (`"deferred_to_codegen"`).
///
/// When the enclosing function's AST is unavailable, or the loop's shape
/// cannot be recovered, `cost_gate` is `"unknown"` and `fanned_out` is `false`
/// rather than a guess.
///
/// ## `fanned_out` is a COMPILE-TIME fact, not a runtime one
///
/// It means codegen emitted a fan-out dispatch for this loop. It does **not**
/// promise the loop runs on multiple threads at runtime, and the gap is real
/// for nested reductions: the runtime caps fan-out depth per thread chain at
/// `KARAC_PAR_MAX_FORK_DEPTH` (default 1, `karac_par_reduce`), so when an
/// outer and an inner reduction both clear the gates, both report
/// `fanned_out: true` while only the OUTERMOST actually forks — every deeper
/// level runs sequentially inline. That cap is what keeps a recursive
/// backtracking search from nesting a parallel region per level and
/// exhausting the stack (B-2026-07-03-14).
///
/// So read this field as "the compiler decided to parallelize here", which is
/// the question the query is for. Whether a *given execution* forks depends on
/// the depth cap and the runtime's own cost gate, neither of which is a
/// compile-time property.
pub(crate) fn loop_reductions_json(
    fc: &FunctionConcurrency,
    func: Option<&Function>,
    program: Option<&Program>,
) -> String {
    let entries: Vec<String> = fc
        .loop_reductions
        .iter()
        .map(|r| {
            // A `seq` entry is the single-threaded push->in-place-store
            // rewrite: no dispatch exists, so no gate applies.
            let (lowering, fanned_out, gate, reason) = if r.seq {
                ("sequential_tabulate", false, "n/a", "lowered inline, single-threaded")
            } else {
                match func.and_then(|f| reduction_loop_verdict(f, program, r)) {
                    Some(v) => (
                        "parallel_fanout",
                        v.is_fanout(),
                        v.tag(),
                        v.reason(),
                    ),
                    None => (
                        "parallel_fanout",
                        false,
                        "unknown",
                        "loop shape not recoverable from the AST at this site",
                    ),
                }
            };
            format!(
                "{{\"statement\":{},\"loop_line\":{},\"accumulator\":{},\"op\":{},\"lowering\":{},\"collect_tabulate\":{},\"fanned_out\":{},\"cost_gate\":{},\"reason\":{}}}",
                r.stmt_index,
                r.loop_line,
                json_string(&r.accumulator),
                json_string(r.op.symbol()),
                json_string(lowering),
                r.collect_tabulate,
                fanned_out,
                json_string(gate),
                json_string(reason),
            )
        })
        .collect();
    format!("[{}]", entries.join(","))
}

/// Render a function's `disjoint_write_loops` as a JSON array — the **indexed
/// write** fan-out shape (`out[f(i)] = ...`), which is neither a statement-level
/// `parallel_group` nor a `loop_reduction`, and so was invisible in this
/// surface until the disjointness proof landed.
///
/// Field contract, again named so no field over-promises:
///
/// - `loop_var` — the candidate parallel dimension.
/// - `disjoint_writes` — did the per-iteration footprint proof discharge? True
///   means no two iterations of `loop_var` can write the same slot in any
///   listed target.
/// - `gate` — `"proven"`, or the machine tag of the obligation that failed
///   (`indirect_index`, `footprint_overlap`, `reads_written_target`, …). This
///   field is the point of the surface: "the compiler silently didn't
///   parallelize" is the failure mode a queryable decline replaces.
/// - `reason` — the same decision in prose. On a proof it names the interval,
///   e.g. ``iteration `dy` writes `out` only within [dy * (4 * dw), (dy + 1) *
///   (4 * dw))``.
/// - `targets` — per written collection, its `stride` and `base`, so a reader
///   can check the interval the compiler believes in against the one they meant.
///
/// - `fanned_out` — does the emitted binary dispatch this loop across the
///   worker pool? A proof is necessary but not sufficient: the same cost model
///   the reduction path uses still decides whether dispatch pays. Recognition
///   and emission are different questions, and reporting the first as the
///   second is the defect B-2026-07-29-29 was filed for.
/// - `cost_gate` — `"fanout"` when it dispatches, the declining gate's name
///   when the proof held but the cost model said no
///   (`declined_memory_bound`, `declined_below_cost_threshold`,
///   `declined_variable_k_param_bound`), and `"n/a"` when the proof itself
///   declined — there is no dispatch to gate.
///
/// The verdict comes from [`crate::par_cost::fanout_verdict`], the SAME
/// function `codegen/disjoint_par.rs` calls, so the query cannot drift from the
/// binary.
///
/// ## `fanned_out` is a COMPILE-TIME fact
///
/// It means codegen emitted a fan-out dispatch. Whether a given execution
/// actually forks additionally depends on the runtime's own cost gate and the
/// per-thread fork-depth cap (`KARAC_PAR_MAX_FORK_DEPTH`) — neither a
/// compile-time property. Same reading as `loop_reductions`' field of the same
/// name.
pub(crate) fn disjoint_write_loops_json(
    fc: &FunctionConcurrency,
    func: Option<&Function>,
    program: Option<&Program>,
) -> String {
    let entries: Vec<String> = fc
        .disjoint_write_loops
        .iter()
        .map(|d| {
            let targets: Vec<String> = d
                .targets
                .iter()
                .map(|t| {
                    format!(
                        "{{\"name\":{},\"stride\":{},\"base\":{},\"writes\":{}}}",
                        json_string(&t.target),
                        json_string(&t.stride),
                        json_string(&t.base),
                        t.writes,
                    )
                })
                .collect();
            // A declined proof never reaches the cost model, so it reports
            // `n/a` rather than a gate name it did not run.
            let (fanned_out, cost_gate) = if !d.proven() {
                (false, "n/a")
            } else {
                match func.and_then(|f| disjoint_loop_verdict(f, program, d)) {
                    Some(v) => (v.is_fanout(), v.tag()),
                    None => (false, "unknown"),
                }
            };
            format!(
                "{{\"statement\":{},\"loop_line\":{},\"loop_var\":{},\"disjoint_writes\":{},\"gate\":{},\"fanned_out\":{},\"cost_gate\":{},\"targets\":[{}],\"reason\":{}}}",
                d.stmt_index,
                d.loop_line,
                json_string(&d.loop_var),
                d.proven(),
                json_string(d.tag()),
                fanned_out,
                json_string(cost_gate),
                targets.join(","),
                json_string(&d.reason),
            )
        })
        .collect();
    format!("[{}]", entries.join(","))
}

/// Locate the loop a `DisjointWriteLoop` came from and run the fan-out cost
/// gates over it — the same gates, through the same entry point, that
/// `codegen/disjoint_par.rs` applies.
///
/// Matched on the loop expression's **byte span**, not its line: a nested loop
/// written on its parent's line shares the parent's line and statement index,
/// and handing it the parent's tag is the miscompile
/// `DisjointWriteLoop::loop_span` documents. The query has no reason to be
/// looser than codegen about which loop a tag names.
fn disjoint_loop_verdict(
    func: &Function,
    program: Option<&Program>,
    d: &crate::concurrency::DisjointWriteLoop,
) -> Option<crate::par_cost::FanoutVerdict> {
    let (parent, idx, loop_expr) = find_loop_by_span(&func.body, &d.loop_span)?;
    let shape = crate::par_cost::extract_loop_shape(parent, idx, loop_expr)?;
    let params: Vec<&str> = func.params.iter().filter_map(|p| p.name()).collect();
    let refs_param = expr_mentions_any_name(&shape.end_expr, &params)
        || shape
            .lo_expr
            .as_ref()
            .is_some_and(|e| expr_mentions_any_name(e, &params));
    Some(crate::par_cost::fanout_verdict_indexed_writes(
        &shape.body,
        &shape.end_expr,
        shape.lo_expr.as_ref(),
        program,
        refs_param,
    ))
}

/// Span-exact sibling of [`find_loop_by_line`].
fn find_loop_by_span<'a>(block: &'a Block, span: &Span) -> Option<(&'a Block, usize, &'a Expr)> {
    for (i, stmt) in block.stmts.iter().enumerate() {
        let StmtKind::Expr(e) = &stmt.kind else {
            continue;
        };
        if !matches!(
            e.kind,
            ExprKind::For { .. } | ExprKind::While { .. } | ExprKind::Loop { .. }
        ) {
            continue;
        }
        if e.span.offset == span.offset && e.span.length == span.length {
            return Some((block, i, e));
        }
        let inner = match &e.kind {
            ExprKind::For { body, .. }
            | ExprKind::While { body, .. }
            | ExprKind::Loop { body, .. } => Some(body),
            _ => None,
        };
        if let Some(b) = inner {
            if let Some(hit) = find_loop_by_span(b, span) {
                return Some(hit);
            }
        }
    }
    None
}

/// Find the AST of the function a concurrency decision is keyed under.
///
/// Mirrors `ConcurrencyChecker::collect_functions`' keying convention exactly:
/// a free function is keyed by its bare name, an impl method by
/// `Type.method`. Keeping the two in step matters — a mismatch here silently
/// degrades the query to `cost_gate: "unknown"` rather than failing loudly.
pub(crate) fn function_by_decision_key<'a>(
    program: &'a Program,
    key: &str,
) -> Option<&'a Function> {
    for item in &program.items {
        match item {
            crate::ast::Item::Function(f) if f.name == key => return Some(f),
            crate::ast::Item::ImplBlock(imp) => {
                let type_name = match &imp.target_type.kind {
                    crate::ast::TypeKind::Path(p) => p.segments.last().cloned(),
                    _ => None,
                };
                let Some(type_name) = type_name else { continue };
                for it in &imp.items {
                    if let crate::ast::ImplItem::Method(m) = it {
                        if format!("{type_name}.{}", m.name) == key {
                            return Some(m);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    None
}

/// Locate the loop this `LoopReduction` came from and run the real fan-out
/// gates over it.
///
/// Loops are matched by SOURCE LINE, not by `stmt_index`: the analyzer
/// recurses into nested blocks, so `stmt_index` is an index within the loop's
/// own enclosing block and equal indices recur across sibling and nested
/// blocks. `codegen` keys the same lookup on the `(stmt_index, loop_line)`
/// pair for exactly this reason.
fn reduction_loop_verdict(
    func: &Function,
    program: Option<&Program>,
    r: &crate::concurrency::LoopReduction,
) -> Option<crate::par_cost::FanoutVerdict> {
    let (parent, idx, loop_expr) = find_loop_by_line(&func.body, r.loop_line)?;
    let shape = crate::par_cost::extract_loop_shape(parent, idx, loop_expr)?;
    // The variable-K floor only fires when the trip-count bound references a
    // parameter of the ENCLOSING function — the reusable-helper shape.
    let params: Vec<&str> = func.params.iter().filter_map(|p| p.name()).collect();
    let refs_param = expr_mentions_any_name(&shape.end_expr, &params)
        || shape
            .lo_expr
            .as_ref()
            .is_some_and(|e| expr_mentions_any_name(e, &params));
    Some(crate::par_cost::fanout_verdict(
        &shape.body,
        &shape.end_expr,
        shape.lo_expr.as_ref(),
        program,
        refs_param,
    ))
}

/// Depth-first search for a `for`/`while`/`loop` expression on `line`,
/// returning its enclosing block and index within it (what
/// `extract_loop_shape` needs to find a preceding `let mut k = lo;`).
fn find_loop_by_line(block: &Block, line: usize) -> Option<(&Block, usize, &Expr)> {
    for (i, stmt) in block.stmts.iter().enumerate() {
        let StmtKind::Expr(e) = &stmt.kind else {
            continue;
        };
        if matches!(
            e.kind,
            ExprKind::For { .. } | ExprKind::While { .. } | ExprKind::Loop { .. }
        ) {
            if e.span.line == line {
                return Some((block, i, e));
            }
            // Recurse into the loop body for nested reductions.
            let inner = match &e.kind {
                ExprKind::For { body, .. }
                | ExprKind::While { body, .. }
                | ExprKind::Loop { body, .. } => Some(body),
                _ => None,
            };
            if let Some(b) = inner {
                if let Some(hit) = find_loop_by_line(b, line) {
                    return Some(hit);
                }
            }
        }
    }
    None
}

/// Whether `expr` mentions any of `names` as a bare identifier.
fn expr_mentions_any_name(expr: &Expr, names: &[&str]) -> bool {
    let mut found = false;
    walk_expr_idents(expr, &mut |n| {
        if names.contains(&n) {
            found = true;
        }
    });
    found
}

fn walk_expr_idents(expr: &Expr, f: &mut impl FnMut(&str)) {
    match &expr.kind {
        ExprKind::Identifier(n) => f(n),
        ExprKind::Binary { left, right, .. } => {
            walk_expr_idents(left, f);
            walk_expr_idents(right, f);
        }
        ExprKind::Unary { operand, .. } => walk_expr_idents(operand, f),
        ExprKind::Call { callee, args } => {
            walk_expr_idents(callee, f);
            for a in args {
                walk_expr_idents(&a.value, f);
            }
        }
        ExprKind::MethodCall { object, args, .. } => {
            walk_expr_idents(object, f);
            for a in args {
                walk_expr_idents(&a.value, f);
            }
        }
        ExprKind::FieldAccess { object, .. } => walk_expr_idents(object, f),
        ExprKind::Index { object, index } => {
            walk_expr_idents(object, f);
            walk_expr_idents(index, f);
        }
        ExprKind::Cast { expr: inner, .. } => walk_expr_idents(inner, f),
        _ => {}
    }
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
    program: Option<&Program>,
) -> String {
    let fn_entries: Vec<String> = graph
        .nodes
        .iter()
        .filter_map(|(key, node)| {
            analysis.function_decisions.get(key).map(|fc| {
                format!(
                    "{{\"function\":{},\"line\":{},\"total_statements\":{},\"statement_spans\":{},\"parallel_groups\":{},\"loop_reductions\":{},\"disjoint_write_loops\":{},\"serialization_points\":{},\"reorder_opportunities\":{}}}",
                    json_string(key),
                    node.line,
                    fc.total_statements,
                    statement_spans_json(fc, scope),
                    parallel_groups_json(fc),
                    loop_reductions_json(
                        fc,
                        program.and_then(|p| function_by_decision_key(p, key)),
                        program,
                    ),
                    disjoint_write_loops_json(
                        fc,
                        program.and_then(|p| function_by_decision_key(p, key)),
                        program,
                    ),
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
        concurrency_json: build_concurrency_graph_json(
            &analysis,
            &graph,
            scope,
            Some(&parsed.program),
        ),
        diagnostics,
    }
}
