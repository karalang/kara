//! `karac query` — effects / concurrency / affected-by / cost envelopes.
//!
//! Extracted verbatim from `cli.rs` (structural-debt extraction, slice 2).

use super::*;

pub(super) fn cmd_query(kind: QueryKind, filename: &str, function: &str) {
    let source = read_source(filename);
    let mut pipeline = Pipeline::new(filename, &source);
    pipeline.resolve();

    if pipeline.has_fatal_errors() {
        print_text_diagnostics(&pipeline);
        process::exit(1);
    }

    match kind {
        QueryKind::Effects => {
            // typecheck + lower BEFORE effectcheck so the effect-inference
            // walker can resolve instance-method callees to their precise
            // `Type.method` key. `Pipeline::effectcheck` sources its
            // `method_callee_types` table from `self.typed`, falling back to
            // an empty map when typecheck didn't run — without this the query
            // under-reports any effect that propagates through an instance
            // method (`c.get(...)` shows no `Network`). Mirrors what `build` /
            // `test` see (they always typecheck first). Phase-8 line 101.
            pipeline.typecheck();
            pipeline.lower();
            pipeline.effectcheck();
            query_effects(&pipeline, function, filename);
        }
        QueryKind::Ownership => {
            pipeline.typecheck();
            pipeline.lower();
            pipeline.ownershipcheck();
            query_ownership(&pipeline, function);
        }
        QueryKind::Concurrency => {
            // Same instance-method-effect-resolution requirement as the
            // Effects arm above — concurrency analysis consumes the
            // effect-check result, so its inputs must come from the
            // typechecked pipeline too (phase-8 line 101).
            pipeline.typecheck();
            pipeline.lower();
            pipeline.effectcheck();
            pipeline.concurrencycheck();
            query_concurrency(&pipeline, function, filename);
        }
        QueryKind::CostSummary => {
            // cost-summary draws from the ownership pass for `rc_ops` and
            // walks the AST directly for `arc_provider_wraps` and
            // `borrow_flag_fields`. It needs typecheck + lower (so the
            // ownership pass sees the same AST every other phase does).
            pipeline.typecheck();
            pipeline.lower();
            pipeline.ownershipcheck();
            query_cost_summary(&pipeline);
        }
        QueryKind::Attributes { tool_prefix } => {
            // Pure AST walk — no further pipeline phases needed beyond
            // the resolve already done above (which gates fatal parse /
            // resolve errors). Tool-namespaced attributes have no
            // semantic effect on later phases, so we can emit a usable
            // result even when typecheck / ownership would have flagged
            // unrelated problems.
            query_attributes(&pipeline, tool_prefix);
        }
        QueryKind::Queries => {
            // Run every phase that may populate `queries`, then fold in
            // the P1.3 codegen analyzer in `query_queries`. The envelope
            // carries the P1.3 catalogue entries (inlining / branch-hint)
            // when they fire; the remaining phase `queries` vecs (P1.1,
            // P1.2, P1.4, P1.6) are still empty, so a program with no
            // hot-looking helper or skewed branch renders `{"queries":[]}`.
            pipeline.typecheck();
            pipeline.lower();
            pipeline.effectcheck();
            pipeline.ownershipcheck();
            pipeline.concurrencycheck();
            query_queries(&pipeline);
        }
        QueryKind::Monomorphization => {
            // Reads from `TypeCheckResult.call_type_subs` +
            // `method_callee_types` for the type tuple, and from
            // `EffectCheckResult.call_effect_subs` for each instance's
            // effective effect set. Effect resolution needs the same
            // typecheck + lower precondition as the Effects/Concurrency
            // arms (so `with E` bindings resolve against the lowered AST
            // the effect checker walks); call_type_subs spans survive
            // lowering, so the type tuple is unaffected.
            pipeline.typecheck();
            pipeline.lower();
            pipeline.effectcheck();
            query_monomorphization(&pipeline);
        }
        QueryKind::AffectedBy {
            target,
            tests_only,
            direction,
        } => {
            // Call-graph query — pure AST walk; resolution and
            // typecheck are not required (the graph is built from
            // the parsed program). Single-file mode infers the
            // test-file flag from the filename suffix per the same
            // `*_test.kara` heuristic the resolver uses.
            let is_test_file = filename.ends_with("_test.kara");
            let graph = crate::call_graph::build(&pipeline.parsed.program, filename, is_test_file);
            query_affected_by(&graph, &target, tests_only, direction, filename);
        }
    }
}

pub(super) fn query_affected_by(
    graph: &crate::call_graph::CallGraph,
    target: &crate::call_graph::TargetSpec,
    tests_only: bool,
    direction: AffectedByDirection,
    filename: &str,
) {
    let seeds = graph.resolve_target(target);
    let input_label = render_target_label(target, filename);
    // Union the per-seed reach sets so a multi-seed target (file or
    // file:range) collapses to a single envelope. De-dup happens via
    // BTreeMap keyed on node `key`.
    let mut callers: std::collections::BTreeMap<String, &crate::call_graph::NodeInfo> =
        std::collections::BTreeMap::new();
    let mut callees: std::collections::BTreeMap<String, &crate::call_graph::NodeInfo> =
        std::collections::BTreeMap::new();
    let mut tests: std::collections::BTreeMap<String, &crate::call_graph::NodeInfo> =
        std::collections::BTreeMap::new();
    for seed in &seeds {
        if matches!(
            direction,
            AffectedByDirection::Callers | AffectedByDirection::All
        ) {
            for n in graph.transitive_callers(seed) {
                callers.insert(n.key.clone(), n);
                if n.is_test {
                    tests.insert(n.key.clone(), n);
                }
            }
        }
        if matches!(
            direction,
            AffectedByDirection::Callees | AffectedByDirection::All
        ) {
            for n in graph.transitive_callees(seed) {
                callees.insert(n.key.clone(), n);
            }
        }
    }
    // `--tests-only` suppresses both callers and callees and emits
    // just the test set. Useful for the test-selection consumer.
    if tests_only {
        let line = render_affected_by_envelope_tests_only(&input_label, &tests);
        println!("{line}");
        return;
    }
    let line = render_affected_by_envelope(&input_label, &callers, &callees, &tests, direction);
    println!("{line}");
}

pub(super) fn render_target_label(
    target: &crate::call_graph::TargetSpec,
    _filename: &str,
) -> String {
    match target {
        crate::call_graph::TargetSpec::File(f) => f.clone(),
        crate::call_graph::TargetSpec::FileRange(f, lo, hi) => {
            if lo == hi {
                format!("{f}:{lo}")
            } else {
                format!("{f}:{lo}-{hi}")
            }
        }
        crate::call_graph::TargetSpec::Function(name) => name.clone(),
    }
}

pub(super) fn render_affected_by_envelope(
    input: &str,
    callers: &std::collections::BTreeMap<String, &crate::call_graph::NodeInfo>,
    callees: &std::collections::BTreeMap<String, &crate::call_graph::NodeInfo>,
    tests: &std::collections::BTreeMap<String, &crate::call_graph::NodeInfo>,
    direction: AffectedByDirection,
) -> String {
    let mut s = String::new();
    s.push('{');
    s.push_str("\"type\":\"affected_by\",");
    write!(s, "\"input\":{}", json_string(input)).unwrap();
    if matches!(
        direction,
        AffectedByDirection::Callers | AffectedByDirection::All
    ) {
        s.push(',');
        write!(s, "\"callers\":{}", render_node_array(callers)).unwrap();
    }
    if matches!(
        direction,
        AffectedByDirection::Callees | AffectedByDirection::All
    ) {
        s.push(',');
        write!(s, "\"callees\":{}", render_node_array(callees)).unwrap();
    }
    if matches!(
        direction,
        AffectedByDirection::Callers | AffectedByDirection::All
    ) {
        s.push(',');
        write!(s, "\"tests\":{}", render_node_array(tests)).unwrap();
    }
    s.push('}');
    s
}

pub(super) fn render_affected_by_envelope_tests_only(
    input: &str,
    tests: &std::collections::BTreeMap<String, &crate::call_graph::NodeInfo>,
) -> String {
    let mut s = String::new();
    s.push('{');
    s.push_str("\"type\":\"affected_by\",");
    write!(s, "\"input\":{}", json_string(input)).unwrap();
    s.push(',');
    write!(s, "\"tests\":{}", render_node_array(tests)).unwrap();
    s.push('}');
    s
}

pub(super) fn render_node_array(
    nodes: &std::collections::BTreeMap<String, &crate::call_graph::NodeInfo>,
) -> String {
    let entries: Vec<String> = nodes
        .values()
        .map(|n| {
            format!(
                "{{\"fn\":{},\"file\":{},\"line\":{}}}",
                json_string(&n.key),
                json_string(&n.file),
                n.line
            )
        })
        .collect();
    format!("[{}]", entries.join(","))
}

/// Phase-8 stdlib-floor § Compiler queries channel sub-item 3.
/// Collate every `CompilerQuery` across all phase results plus the
/// P1.3 codegen analyzer (`crate::codegen_queries`) and emit them as
/// a single JSON envelope on stdout. The envelope shape is
/// `{"queries":[…]}`; adding new catalogue entries or phases is
/// non-breaking.
pub(super) fn query_queries(pipeline: &Pipeline) {
    let mut all: Vec<crate::queries::CompilerQuery> = Vec::new();
    if let Some(r) = pipeline.resolved.as_ref() {
        all.extend(r.queries.iter().cloned());
    }
    if let Some(t) = pipeline.typed.as_ref() {
        all.extend(t.queries.iter().cloned());
    }
    if let Some(e) = pipeline.effects.as_ref() {
        all.extend(e.queries.iter().cloned());
    }
    if let Some(o) = pipeline.ownership.as_ref() {
        all.extend(o.queries.iter().cloned());
    }
    if let Some(c) = pipeline.concurrency.as_ref() {
        all.extend(c.queries.iter().cloned());
    }
    // P1.3 codegen queries — plain-data analyzer over the parsed AST.
    // Runs unconditionally; cheap (single AST walk) and doesn't
    // require any later-phase side-tables.
    all.extend(crate::codegen_queries::analyze(&pipeline.parsed.program));

    // P1.2 specialization queries — reads the monomorphization counter,
    // so it needs the typecheck result (the type tuples). Skips silently
    // when typecheck didn't run; `effects` enriches nothing here but is
    // threaded for a uniform analyzer signature.
    if let Some(t) = pipeline.typed.as_ref() {
        all.extend(crate::specialization_queries::analyze(
            &pipeline.parsed.program,
            t,
            pipeline.effects.as_ref(),
        ));
    }

    // P1.1 RC-fallback queries — plain-data walk over the ownership
    // pass's `rc_values`. Skips silently when the ownership pass didn't
    // run.
    if let Some(o) = pipeline.ownership.as_ref() {
        all.extend(crate::rc_fallback_queries::analyze(
            &pipeline.parsed.program,
            o,
        ));
    }

    // P1.6 fork-threshold queries — plain-data walk over the concurrency
    // analysis's per-function parallelization decisions. Skips silently
    // when the concurrency pass didn't run.
    if let Some(c) = pipeline.concurrency.as_ref() {
        all.extend(crate::fork_threshold_queries::analyze(
            &pipeline.parsed.program,
            c,
        ));
    }

    // P1.5 layout-choice queries — plain-data walk over the AST keyed by
    // the typechecker's `expr_types` + `struct_info`. Skips silently when
    // typecheck didn't run.
    if let Some(t) = pipeline.typed.as_ref() {
        all.extend(crate::layout_queries::analyze(&pipeline.parsed.program, t));
    }

    println!("{}", render_queries_envelope(&all, &pipeline.filename));
}

pub(super) fn render_queries_envelope(
    queries: &[crate::queries::CompilerQuery],
    filename: &str,
) -> String {
    if queries.is_empty() {
        return "{\"queries\":[]}".to_string();
    }
    let entries: Vec<String> = queries
        .iter()
        .map(|q| render_compiler_query(q, filename))
        .collect();
    format!("{{\"queries\":[{}]}}", entries.join(","))
}

pub(super) fn render_compiler_query(q: &crate::queries::CompilerQuery, filename: &str) -> String {
    use crate::queries::{Confidence, Phase, QueryKind};
    let kind = match q.kind {
        QueryKind::Stub => "stub",
        QueryKind::InliningDecision => "inlining_decision",
        QueryKind::BranchHint => "branch_hint",
        QueryKind::SpecializationDecision => "specialization_decision",
        QueryKind::RcFallbackDecision => "rc_fallback_decision",
        QueryKind::ForkThresholdDecision => "fork_threshold_decision",
        QueryKind::LayoutChoice => "layout_choice",
    };
    let confidence = match q.default_confidence {
        Confidence::Low => "low",
        Confidence::Medium => "medium",
        Confidence::High => "high",
    };
    let origin = q.cross_phase_origin.map(|p| match p {
        Phase::Resolver => "resolver",
        Phase::TypeChecker => "typechecker",
        Phase::EffectChecker => "effectchecker",
        Phase::Ownership => "ownership",
        Phase::Concurrency => "concurrency",
        Phase::Codegen => "codegen",
    });
    let options_json: Vec<String> = q
        .options
        .iter()
        .map(|opt| {
            let note = opt
                .note
                .as_deref()
                .map(|n| format!(",\"note\":\"{}\"", json_escape(n)))
                .unwrap_or_default();
            format!("{{\"label\":\"{}\"{}}}", json_escape(&opt.label), note)
        })
        .collect();
    let resolution_json: Vec<String> = q
        .resolution_surface
        .attributes
        .iter()
        .map(|a| format!("\"{}\"", json_escape(a)))
        .collect();
    let origin_field = origin
        .map(|o| format!(",\"cross_phase_origin\":\"{}\"", o))
        .unwrap_or_default();
    format!(
        "{{\"id\":\"{}\",\"site\":{{\"file\":\"{}\",\"line\":{},\"column\":{},\"offset\":{},\"length\":{}}},\"kind\":\"{}\",\"options\":[{}],\"default\":{},\"default_confidence\":\"{}\",\"resolution_surface\":[{}]{}}}",
        json_escape(&q.id.to_string()),
        json_escape(filename),
        q.site.line,
        q.site.column,
        q.site.offset,
        q.site.length,
        kind,
        options_json.join(","),
        q.default,
        confidence,
        resolution_json.join(","),
        origin_field,
    )
}

pub(super) fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

pub(super) fn query_attributes(pipeline: &Pipeline, tool_prefix: Option<String>) {
    let filter = crate::query_attributes::AttributeQueryFilter {
        tool_prefix: tool_prefix.clone(),
    };
    let records = crate::query_attributes::collect_attributes(&pipeline.parsed.program, &filter);
    println!(
        "{}",
        render_attribute_query_json(&records, &pipeline.filename, tool_prefix.as_deref())
    );
}

pub(super) fn render_attribute_query_json(
    records: &[crate::query_attributes::AttributeQueryRecord],
    filename: &str,
    tool_prefix: Option<&str>,
) -> String {
    let records_json: Vec<String> = records
        .iter()
        .map(|r| render_attribute_record(r, filename))
        .collect();
    let prefix_field = match tool_prefix {
        Some(p) => json_string(p),
        None => "null".to_string(),
    };
    format!(
        "{{\"tool_prefix\":{},\"attributes\":[{}]}}",
        prefix_field,
        records_json.join(","),
    )
}

pub(super) fn render_attribute_record(
    r: &crate::query_attributes::AttributeQueryRecord,
    filename: &str,
) -> String {
    let path = json_string_list(&r.path);
    let args: Vec<String> = r
        .args
        .iter()
        .map(|a| render_attribute_arg(a, filename))
        .collect();
    let span = span_to_json(&r.span, filename);
    format!(
        "{{\"path\":{},\"args\":[{}],\"attached_to\":{},\"span\":{{{}}}}}",
        path,
        args.join(","),
        json_string(&r.attached_to),
        span,
    )
}

pub(super) fn render_attribute_arg(
    a: &crate::query_attributes::AttributeQueryArg,
    filename: &str,
) -> String {
    let name = match &a.name {
        Some(n) => json_string(n),
        None => "null".to_string(),
    };
    let value = match &a.value {
        Some(v) => render_attribute_value(v),
        None => "null".to_string(),
    };
    let span = span_to_json(&a.span, filename);
    format!(
        "{{\"name\":{},\"value\":{},\"span\":{{{}}}}}",
        name, value, span,
    )
}

pub(super) fn render_attribute_value(v: &crate::query_attributes::AttributeQueryValue) -> String {
    use crate::query_attributes::AttributeQueryValue;
    match v {
        AttributeQueryValue::String(s) => {
            format!("{{\"kind\":\"string\",\"value\":{}}}", json_string(s))
        }
        AttributeQueryValue::Int(n) => format!("{{\"kind\":\"int\",\"value\":{}}}", n),
        AttributeQueryValue::Bool(b) => format!("{{\"kind\":\"bool\",\"value\":{}}}", b),
        AttributeQueryValue::Path(p) => {
            format!("{{\"kind\":\"path\",\"value\":{}}}", json_string(p))
        }
        AttributeQueryValue::Other => "{\"kind\":\"expr\"}".to_string(),
    }
}

pub(super) fn query_monomorphization(pipeline: &Pipeline) {
    let tc = match pipeline.typed.as_ref() {
        Some(t) => t,
        None => {
            // Typecheck didn't run (resolve errors short-circuited).
            // Emit an empty envelope so the CLI is still scriptable in
            // that case.
            println!(
                "{{\"scope\":{},\"by_generic\":[],\"totals\":{{\"generic_count\":0,\"instance_count\":0}}}}",
                json_string(&pipeline.filename),
            );
            return;
        }
    };
    let table =
        crate::monomorphization::analyze(&pipeline.parsed.program, tc, pipeline.effects.as_ref());
    println!(
        "{}",
        render_monomorphization_json(&table, &pipeline.filename),
    );
}

pub(super) fn render_monomorphization_json(
    table: &crate::monomorphization::MonomorphizationTable,
    filename: &str,
) -> String {
    let entries: Vec<String> = table
        .by_generic
        .iter()
        .map(|g| {
            let instances: Vec<String> = g
                .instances
                .iter()
                .map(|i| render_monomorphization_instance(i, filename))
                .collect();
            format!(
                "{{\"generic\":{},\"instance_count\":{},\"instances\":[{}]}}",
                json_string(&g.generic),
                g.instances.len(),
                instances.join(","),
            )
        })
        .collect();
    format!(
        "{{\"scope\":{},\"by_generic\":[{}],\"totals\":{{\"generic_count\":{},\"instance_count\":{}}}}}",
        json_string(filename),
        entries.join(","),
        table.generic_count(),
        table.instance_count(),
    )
}

pub(super) fn render_monomorphization_instance(
    instance: &crate::monomorphization::Instance,
    filename: &str,
) -> String {
    let site = format!(
        "{}:{}:{}",
        filename, instance.site.line, instance.site.column
    );
    format!(
        "{{\"types\":{},\"effects\":{},\"site\":{}}}",
        json_string_list(&instance.types),
        json_string_list(&instance.effects),
        json_string(&site),
    )
}

pub(super) fn query_cost_summary(pipeline: &Pipeline) {
    let Some(ownership) = pipeline.ownership.as_ref() else {
        eprintln!("error: ownership pass did not run (earlier phase failed)");
        process::exit(1);
    };
    let summary =
        crate::cost_summary::build(&pipeline.filename, &pipeline.parsed.program, ownership);
    println!("{}", render_cost_summary_json(&summary, &pipeline.filename));
}

pub(super) fn render_cost_summary_json(
    s: &crate::cost_summary::CostSummary,
    filename: &str,
) -> String {
    let totals = format!(
        "{{\"rc_ops\":{{\"count\":{},\"rc\":{},\"arc\":{},\"suppressed\":{}}},\"arc_provider_wraps\":{},\"borrow_flag_fields\":{},\"partition_guard_sites\":{},\"auto_clone_insertions\":{}}}",
        s.totals.rc_ops.count,
        s.totals.rc_ops.rc,
        s.totals.rc_ops.arc,
        s.totals.rc_ops.suppressed,
        s.totals.arc_provider_wraps,
        s.totals.borrow_flag_fields,
        s.totals.partition_guard_sites,
        s.totals.auto_clone_insertions,
    );
    let by_function: Vec<String> = s
        .by_function
        .iter()
        .map(|row| {
            let derivation: Vec<String> = row
                .derivation
                .iter()
                .map(|d| {
                    let site = span_to_json(&d.site, filename);
                    format!(
                        "{{\"reason\":{},\"site\":{{{}}}}}",
                        json_string(&d.reason),
                        site,
                    )
                })
                .collect();
            format!(
                "{{\"function\":{},\"rc_ops\":{},\"rc_ops_suppressed\":{},\"arc_provider_wraps\":{},\"derivation\":[{}]}}",
                json_string(&row.function),
                row.rc_ops,
                row.rc_ops_suppressed,
                row.arc_provider_wraps,
                derivation.join(","),
            )
        })
        .collect();
    let perf_notes: Vec<String> = s
        .perf_notes
        .iter()
        .map(|n| {
            let site = span_to_json(&n.site, filename);
            format!(
                "{{\"code\":{},\"message\":{},\"site\":{{{}}}}}",
                json_string(&n.code),
                json_string(&n.message),
                site,
            )
        })
        .collect();
    format!(
        "{{\"scope\":{},\"totals\":{},\"by_function\":[{}],\"perf_notes\":[{}]}}",
        json_string(&s.scope),
        totals,
        by_function.join(","),
        perf_notes.join(","),
    )
}

pub(super) fn query_effects(pipeline: &Pipeline, function: &str, filename: &str) {
    let effects = pipeline.effects.as_ref().unwrap();

    // Whole-program mode: an empty `function` (a bare `<file>.kara`
    // target) emits every function's effects plus the call-graph edges
    // — the effect-graph artifact Cartographer consumes.
    if function.is_empty() {
        query_effects_whole_program(pipeline, effects, filename);
        return;
    }

    let inferred = effects.inferred_effects.get(function);
    let declared = effects.declared_effects.get(function);

    if inferred.is_none() && declared.is_none() {
        eprintln!("error: function '{function}' not found");
        process::exit(1);
    }

    let inferred_str = inferred
        .map(crate::effect_graph::effect_set_json)
        .unwrap_or_else(|| "[]".to_string());

    println!(
        "{{\"function\":{},\"inferred_effects\":{},\"declared_effects\":{}}}",
        json_string(function),
        inferred_str,
        crate::effect_graph::declared_effects_json(declared),
    );
}

/// Whole-program effect graph: one node per source-defined function
/// (free fn, impl method, trait default method) carrying its inferred +
/// declared effects, plus the directed call-graph edges between them.
/// Delegates to the wasm-safe [`crate::effect_graph`] builder so the CLI
/// and the browser studio emit a byte-identical graph.
pub(super) fn query_effects_whole_program(
    pipeline: &Pipeline,
    effects: &EffectCheckResult,
    filename: &str,
) {
    let is_test_file = filename.ends_with("_test.kara");
    let graph = crate::call_graph::build(&pipeline.parsed.program, filename, is_test_file);
    println!(
        "{}",
        crate::effect_graph::build_effect_graph_json(effects, &graph, filename)
    );
}

pub(super) fn query_ownership(pipeline: &Pipeline, function: &str) {
    let ownership = pipeline.ownership.as_ref().unwrap();

    match ownership.param_modes.get(function) {
        Some(params) => {
            let param_entries: Vec<String> = params
                .iter()
                .map(|(name, mode)| {
                    let repr = ownership
                        .representations
                        .get(&format!("{}.{}", function, name))
                        .cloned()
                        .unwrap_or_else(|| match mode {
                            crate::ownership::OwnershipMode::Own => "owned (stack)".to_string(),
                            _ => "ref (borrow)".to_string(),
                        });
                    format!(
                        "{{\"name\":{},\"mode\":{},\"representation\":{}}}",
                        json_string(name),
                        json_string(ownership_mode_str(mode)),
                        json_string(&repr),
                    )
                })
                .collect();
            let rc_entries: Vec<String> = ownership
                .rc_values
                .get(function)
                .map(|m| {
                    let mut v: Vec<&crate::ownership::RcEntry> = m.values().collect();
                    v.sort_by(|a, b| a.binding.cmp(&b.binding));
                    v.into_iter()
                        .map(|e| {
                            let arc = ownership
                                .arc_values
                                .get(function)
                                .is_some_and(|s| s.contains(&e.binding));
                            let kind = if arc { "Arc" } else { "Rc" };
                            format!(
                                "{{\"binding\":{},\"kind\":{},\"trigger\":{},\"consume_line\":{},\"other_use_line\":{}}}",
                                json_string(&e.binding),
                                json_string(kind),
                                json_string(rc_trigger_str(&e.trigger)),
                                e.consume_span.line,
                                e.other_use_span.line,
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();

            // Round 12.25: closures created inside `function` are
            // surfaced as a `"closures"` array. Each entry carries
            // the closure expression's source location plus the
            // round-12.23 inferred parameter modes and round-12.24
            // captures. Sorted by (line, column) for deterministic
            // output.
            let mut closures_to_emit: Vec<(&crate::resolver::SpanKey, &crate::token::Span)> =
                ownership
                    .closure_function
                    .iter()
                    .filter(|(_, fn_key)| fn_key.as_str() == function)
                    .filter_map(|(k, _)| ownership.closure_spans.get(k).map(|sp| (k, sp)))
                    .collect();
            closures_to_emit.sort_by_key(|(_, sp)| (sp.line, sp.column));
            let closure_entries: Vec<String> = closures_to_emit
                .iter()
                .map(|(key, span)| {
                    let params_json: Vec<String> = ownership
                        .closure_param_modes
                        .get(key)
                        .map(|ms| {
                            ms.iter()
                                .map(|(name, mode)| {
                                    format!(
                                        "{{\"name\":{},\"mode\":{}}}",
                                        json_string(name),
                                        json_string(ownership_mode_str(mode)),
                                    )
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    let captures_json: Vec<String> = ownership
                        .closure_captures
                        .get(key)
                        .map(|cs| {
                            cs.iter()
                                .map(|(name, mode)| {
                                    format!(
                                        "{{\"name\":{},\"mode\":{}}}",
                                        json_string(name),
                                        json_string(ownership_mode_str(mode)),
                                    )
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    format!(
                        "{{\"line\":{},\"column\":{},\"parameters\":[{}],\"captures\":[{}]}}",
                        span.line,
                        span.column,
                        params_json.join(","),
                        captures_json.join(","),
                    )
                })
                .collect();
            println!(
                "{{\"function\":{},\"parameters\":[{}],\"rc_values\":[{}],\"closures\":[{}]}}",
                json_string(function),
                param_entries.join(","),
                rc_entries.join(","),
                closure_entries.join(","),
            );
        }
        None => {
            eprintln!("error: function '{function}' not found");
            process::exit(1);
        }
    }
}

pub(super) fn rc_trigger_str(t: &crate::ownership::RcTrigger) -> &'static str {
    match t {
        crate::ownership::RcTrigger::DirectReuseAfterConsume => "direct_reuse_after_consume",
        crate::ownership::RcTrigger::ClosureCaptureWithOuterUse => "closure_capture_with_outer_use",
        crate::ownership::RcTrigger::ContainerStoreWithSubsequentUse => {
            "container_store_with_subsequent_use"
        }
    }
}

pub(super) fn query_concurrency(pipeline: &Pipeline, function: &str, filename: &str) {
    let analysis = pipeline.concurrency.as_ref().unwrap();

    // Whole-program mode: an empty `function` (a bare `<file>.kara`
    // target) emits the parallel-band decision for every analyzed
    // function — the concurrency layer of Cartographer's effect graph.
    if function.is_empty() {
        query_concurrency_whole_program(pipeline, analysis, filename);
        return;
    }

    match analysis.function_decisions.get(function) {
        Some(fc) => {
            println!(
                "{{\"function\":{},\"total_statements\":{},\"statement_spans\":{},\"parallel_groups\":{},\"loop_reductions\":{},\"declined_par_loops\":{},\"disjoint_write_loops\":{},\"serialization_points\":{},\"reorder_opportunities\":{}}}",
                json_string(function),
                fc.total_statements,
                crate::effect_graph::statement_spans_json(fc, filename),
                crate::effect_graph::parallel_groups_json(fc),
                crate::effect_graph::loop_reductions_json(
                    fc,
                    crate::effect_graph::function_by_decision_key(
                        &pipeline.parsed.program,
                        function,
                    ),
                    Some(&pipeline.parsed.program),
                    function,
                ),
                crate::effect_graph::declined_par_loops_json(fc),
                crate::effect_graph::disjoint_write_loops_json(
                    fc,
                    crate::effect_graph::function_by_decision_key(
                        &pipeline.parsed.program,
                        function,
                    ),
                    Some(&pipeline.parsed.program),
                    function,
                ),
                crate::effect_graph::serialization_points_json(fc),
                crate::effect_graph::reorder_opportunities_json(fc),
            );
        }
        None => {
            eprintln!("error: function '{function}' not found");
            process::exit(1);
        }
    }
}

/// Whole-program concurrency report: one entry per analyzed source
/// function (in call-graph key order), carrying its statement count and
/// parallel groups. Function keys join with the effect-graph nodes from
/// `query effects <file>`, so a consumer can overlay parallel bands onto
/// the effect graph.
pub(super) fn query_concurrency_whole_program(
    pipeline: &Pipeline,
    analysis: &ConcurrencyAnalysis,
    filename: &str,
) {
    let is_test_file = filename.ends_with("_test.kara");
    let graph = crate::call_graph::build(&pipeline.parsed.program, filename, is_test_file);
    println!(
        "{}",
        crate::effect_graph::build_concurrency_graph_json(
            analysis,
            &graph,
            filename,
            Some(&pipeline.parsed.program),
        )
    );
}
