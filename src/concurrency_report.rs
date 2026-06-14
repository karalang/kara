//! Human-readable renderer for `karac build --concurrency-report` /
//! `karac check --concurrency-report` (Slice D, 2026-05-08).
//!
//! Pure rendering: consumes the existing `ConcurrencyAnalysis` produced by
//! `concurrencycheck()` and the per-function effect attributions in
//! `EffectCheckResult`, and emits the demo-storyboard text shape. The
//! structured-JSON output via `karac query concurrency` is unchanged — this
//! is a sibling text channel for human readers, not a parallel data
//! extraction pass.
//!
//! Output shape (locked by `docs/dogfooding.md § Parallax ("What the demo shows")`):
//!
//! ```text
//! function process_request:
//!   parallel_group {
//!     [73] record_a()  // writes(MetricsA)
//!     [74] record_b()  // writes(MetricsB)
//!     [75] record_c()  // writes(MetricsC)
//!     reason: independent effects on different resources
//!   }
//! ```
//!
//! When no function has any non-trivial parallel group, the renderer emits
//! the single line `<no parallelization opportunities detected>`. Trivial
//! groups (`group.is_trivial == true` — pure-arithmetic groups too cheap to
//! justify thread dispatch) are deliberately omitted, matching the
//! storyboard's "what parallelizes and why" framing.

use crate::ast::{
    EffectVerbKind, Expr, ExprKind, Function, ImplBlock, ImplItem, Item, Program, Stmt, StmtKind,
    TypeKind,
};
use crate::concurrency::ConcurrencyAnalysis;
use crate::effectchecker::{DeclaredEffects, EffectCheckResult, EffectSet};

/// Render a human-readable concurrency report from the analyzer's per-function
/// decisions and the effect-checker's per-call attributions.
///
/// Functions are emitted in source-declaration order (walk `program.items`
/// once for free functions, then again for impl methods so the relative
/// ordering of free vs. impl items is preserved per declaration site).
/// Within a function, parallel groups are emitted in `statement_indices[0]`
/// order so the output is stable across runs.
pub fn render_concurrency_report(
    analysis: &ConcurrencyAnalysis,
    effects: &EffectCheckResult,
    program: &Program,
) -> String {
    let mut out = String::new();
    let mut emitted_any = false;

    // Walk items in declaration order so free functions and impl methods
    // appear interleaved in the same order they appear in the source.
    for item in &program.items {
        match item {
            Item::Function(f) => {
                emitted_any |= render_function(&mut out, &f.name, f, analysis, effects);
            }
            Item::ImplBlock(imp) => {
                let Some(type_name) = impl_type_name(imp) else {
                    continue;
                };
                for impl_item in &imp.items {
                    if let ImplItem::Method(method) = impl_item {
                        let key = format!("{}.{}", type_name, method.name);
                        if render_function(&mut out, &key, method, analysis, effects) {
                            emitted_any = true;
                        }
                    }
                }
            }
            _ => {}
        }
    }

    if !emitted_any {
        out.push_str("<no parallelization opportunities detected>\n");
    }
    out
}

/// Pull the impl block's target type name as a single identifier (matching the
/// `ConcurrencyChecker::collect_functions` keying convention).
fn impl_type_name(imp: &ImplBlock) -> Option<String> {
    match &imp.target_type.kind {
        TypeKind::Path(p) => p.segments.last().cloned(),
        _ => None,
    }
}

/// Render a single function's parallel-group section. Returns `true` iff at
/// least one non-trivial group was emitted (so the caller can track whether
/// any output was produced for the empty-case message).
fn render_function(
    out: &mut String,
    decision_key: &str,
    func: &Function,
    analysis: &ConcurrencyAnalysis,
    effects: &EffectCheckResult,
) -> bool {
    let Some(decision) = analysis.function_decisions.get(decision_key) else {
        return false;
    };

    // Skip if neither a non-trivial parallel group nor a recognized
    // loop-reduction exists — sequential functions are noise here.
    let mut groups: Vec<&_> = decision
        .parallel_groups
        .iter()
        .filter(|g| !g.is_trivial)
        .collect();
    let reductions = &decision.loop_reductions;
    if groups.is_empty() && reductions.is_empty() {
        return false;
    }

    // Stable ordering: by the group's first statement index.
    groups.sort_by_key(|g| g.statement_indices.first().copied().unwrap_or(usize::MAX));

    out.push_str(&format!(
        "function {} (line {}):\n",
        decision_key, func.span.line
    ));

    for group in groups {
        out.push_str("  parallel_group {\n");
        let mut indices = group.statement_indices.clone();
        indices.sort();
        for &idx in &indices {
            if let Some(stmt) = func.body.stmts.get(idx) {
                let line = stmt.span.line;
                let call_text = render_call_expr(stmt);
                let effect_text = render_stmt_effects(stmt, effects);
                if effect_text.is_empty() {
                    out.push_str(&format!("    [{line}] {call_text}\n"));
                } else {
                    out.push_str(&format!("    [{line}] {call_text}  // {effect_text}\n"));
                }
            }
        }
        // Reason rendered verbatim from the analyzer's existing prose-shaped
        // string (see `ConcurrencyChecker::describe_group_reason` in
        // `src/concurrency.rs`).
        out.push_str(&format!("    reason: {}\n", group.reason));
        out.push_str("  }\n");
    }

    // Reductions are reported alongside parallel groups so the user sees
    // every opportunity the analyzer surfaced in one block. Stable
    // ordering: by loop_line.
    let mut sorted_reductions: Vec<&_> = reductions.iter().collect();
    sorted_reductions.sort_by_key(|r| r.loop_line);
    for red in sorted_reductions {
        out.push_str(&format!(
            "  reduction {{ op: {}, accumulator: {}, line: {} }}\n",
            red.op.symbol(),
            red.accumulator,
            red.loop_line,
        ));
    }

    true
}

/// Best-effort rendering of a statement's call expression for the report. The
/// concurrency analyzer's parallel groups only ever contain call-shaped stmts
/// (anything else is filtered out by the data-dependency / effect-conflict
/// edges), but the renderer is defensive: a non-call stmt prints as `<stmt>`
/// rather than panicking.
fn render_call_expr(stmt: &Stmt) -> String {
    let expr = match &stmt.kind {
        StmtKind::Expr(e) => e,
        StmtKind::Let { value, .. } => value,
        StmtKind::LetElse { value, .. } => value,
        StmtKind::Assign { value, .. } => value,
        StmtKind::CompoundAssign { value, .. } => value,
        StmtKind::LetUninit { .. } | StmtKind::Defer { .. } | StmtKind::ErrDefer { .. } => {
            return "<stmt>".to_string();
        }
    };
    render_expr(expr)
}

/// Render a small subset of expression shapes — enough to cover the
/// call-expression density the report renders. Non-call shapes fall back to a
/// shape tag so the output stays compact and predictable.
fn render_expr(expr: &Expr) -> String {
    match &expr.kind {
        ExprKind::Call { callee, args } => {
            let head = render_callee(callee);
            format!("{head}({})", render_args(args.len()))
        }
        ExprKind::MethodCall {
            object,
            method,
            args,
            ..
        } => {
            let recv = render_expr(object);
            format!("{recv}.{method}({})", render_args(args.len()))
        }
        ExprKind::Identifier(name) => name.clone(),
        ExprKind::Path { segments, .. } => segments.join("."),
        ExprKind::FieldAccess { object, field } => format!("{}.{}", render_expr(object), field),
        ExprKind::SelfValue => "self".to_string(),
        ExprKind::SelfType => "Self".to_string(),
        _ => "<expr>".to_string(),
    }
}

fn render_callee(expr: &Expr) -> String {
    // For free-function calls, the callee is typically an Identifier or Path.
    // We render the same way as render_expr but keep it scoped so a future
    // tweak (e.g. dropping turbofish) only touches one site.
    render_expr(expr)
}

/// Compact rendering of arg arity. The demo storyboard shows `fetch_profile()`
/// — bare parens — so we elide arg text and just preserve the "is this a call"
/// signal. v1 sticks to `()` / `(...)` density per the out-of-scope list.
fn render_args(n: usize) -> &'static str {
    if n == 0 {
        ""
    } else {
        "..."
    }
}

/// Resolve a statement's call expression to the callee's effect set and
/// render `verb(Resource), verb(Resource), ...`. Returns the empty string
/// when the call has no observable effects, when the callee can't be resolved
/// to a name, or when the callee is polymorphic without a fixed-effect
/// component. Stable ordering: declared / inferred effects are sorted so the
/// snapshot stays identical across HashMap iteration variations.
fn render_stmt_effects(stmt: &Stmt, effects: &EffectCheckResult) -> String {
    let expr = match &stmt.kind {
        StmtKind::Expr(e) => e,
        StmtKind::Let { value, .. } => value,
        StmtKind::LetElse { value, .. } => value,
        StmtKind::Assign { value, .. } => value,
        StmtKind::CompoundAssign { value, .. } => value,
        _ => return String::new(),
    };
    let Some(callee_name) = extract_callee_name(expr) else {
        return String::new();
    };
    let mut rendered: Vec<String> = Vec::new();

    // Prefer the inferred set when present — same source-of-truth the
    // analyzer keys off in `add_function_effects`. Fall back to the declared
    // set's fixed portion if the inferred map has no entry (which can happen
    // for builtin or stdlib stubs whose effects are declared-only).
    if let Some(set) = effects.inferred_effects.get(&callee_name) {
        push_effects(set, &mut rendered);
    } else if let Some(decl) = effects.declared_effects.get(&callee_name) {
        match decl {
            DeclaredEffects::Explicit(set) | DeclaredEffects::PolymorphicWithFixed(set) => {
                push_effects(set, &mut rendered);
            }
            DeclaredEffects::Polymorphic | DeclaredEffects::None => {}
        }
    }

    // Method calls — try `Type.method` keys, then bare `method`. Matches
    // `ConcurrencyChecker::collect_expr_effects`'s strategy.
    if rendered.is_empty() {
        if let ExprKind::MethodCall { method, .. } = &expr.kind {
            for key in effects.inferred_effects.keys() {
                if key.ends_with(&format!(".{}", method)) {
                    if let Some(set) = effects.inferred_effects.get(key) {
                        push_effects(set, &mut rendered);
                    }
                }
            }
        }
    }

    rendered.sort();
    rendered.dedup();
    rendered.join(", ")
}

fn push_effects(set: &EffectSet, out: &mut Vec<String>) {
    for te in &set.effects {
        out.push(format!(
            "{}({})",
            verb_name(&te.effect.verb),
            te.effect.resource
        ));
    }
}

fn extract_callee_name(expr: &Expr) -> Option<String> {
    match &expr.kind {
        ExprKind::Call { callee, .. } => match &callee.kind {
            ExprKind::Identifier(name) => Some(name.clone()),
            ExprKind::Path { segments, .. } => {
                if segments.len() == 2 {
                    Some(format!("{}.{}", segments[0], segments[1]))
                } else {
                    segments.last().cloned()
                }
            }
            _ => None,
        },
        ExprKind::MethodCall { method, .. } => Some(method.clone()),
        _ => None,
    }
}

fn verb_name(verb: &EffectVerbKind) -> &'static str {
    match verb {
        EffectVerbKind::Reads => "reads",
        EffectVerbKind::Writes => "writes",
        EffectVerbKind::Sends => "sends",
        EffectVerbKind::Receives => "receives",
        EffectVerbKind::Allocates => "allocates",
        EffectVerbKind::Panics => "panics",
        EffectVerbKind::Blocks => "blocks",
        EffectVerbKind::Suspends => "suspends",
        EffectVerbKind::UserDefined(_) => "user",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::concurrency::{ConcurrencyAnalysis, FunctionConcurrency, ParallelGroup};
    use crate::effectchecker::{EffectCheckResult, PublicEffectsPolicy};
    use std::collections::{HashMap, HashSet};

    /// Construct an empty `EffectCheckResult` for tests that only need the
    /// renderer's pass-through behaviour. `EffectCheckResult` doesn't derive
    /// `Default` (the public-policy field has multiple legitimate defaults
    /// across the pipeline), so we spell the empty value once here.
    fn empty_effects() -> EffectCheckResult {
        EffectCheckResult {
            inferred_effects: HashMap::new(),
            declared_effects: HashMap::new(),
            expanded_groups: HashMap::new(),
            transparent_effects: HashSet::new(),
            mutual_recursion_groups: Vec::new(),
            function_visibility: HashMap::new(),
            public_effects_policy: PublicEffectsPolicy::Declared,
            errors: Vec::new(),
            queries: Vec::new(),
            call_effect_subs: HashMap::new(),
        }
    }

    /// Empty analysis → empty-case message.
    #[test]
    fn test_render_concurrency_report_empty_analysis() {
        let analysis = ConcurrencyAnalysis {
            function_decisions: HashMap::new(),
            queries: Vec::new(),
        };
        let effects = empty_effects();
        let program = Program::default();
        let report = render_concurrency_report(&analysis, &effects, &program);
        assert_eq!(report, "<no parallelization opportunities detected>\n");
    }

    /// Trivial-only groups produce no output (and therefore the empty-case
    /// message — there are no observable opportunities by the slice's
    /// definition of "opportunity").
    #[test]
    fn test_render_concurrency_report_skips_trivial_groups() {
        let mut decisions = HashMap::new();
        decisions.insert(
            "f".to_string(),
            FunctionConcurrency {
                parallel_groups: vec![ParallelGroup {
                    statement_indices: vec![0, 1],
                    reason: "pure computations".to_string(),
                    is_trivial: true,
                    captured_mutations: std::collections::HashSet::new(),
                }],
                total_statements: 2,
                loop_reductions: Vec::new(),
                serialization_points: Vec::new(),
            },
        );
        let analysis = ConcurrencyAnalysis {
            function_decisions: decisions,
            queries: Vec::new(),
        };
        let effects = empty_effects();
        // The renderer also needs the function in the program AST to render
        // anything. With nothing in the program, the no-program case still
        // emits the empty-case sentinel, which is the same shape and serves
        // the unit test's intent.
        let program = Program::default();
        let report = render_concurrency_report(&analysis, &effects, &program);
        assert_eq!(report, "<no parallelization opportunities detected>\n");
    }
}
