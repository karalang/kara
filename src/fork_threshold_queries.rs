//! Fork-threshold queries — P1.6 catalogue entry, phase-8-stdlib-floor.md
//! § Compiler queries channel. Spec at `docs/design.md § Specification
//! Layers > Compiler Queries` (P1.6 row of the P1 catalogue table).
//!
//! Surfaces [`QueryKind::ForkThresholdDecision`]: a group of statements
//! the auto-parallelizer decided to **fork** (spawn onto the par
//! runtime) rather than run inline. The cost-model gate that made the
//! call is crude in v1 — a group forks unless it is "trivial" (pure
//! arithmetic / no work-bearing statements). The query surfaces each
//! fork decision and offers `#[fork_at(...)]` so the author can pin or
//! override it with workload knowledge the heuristic lacks.
//!
//! ### Why this lives outside the concurrency pass
//!
//! The concurrency analysis already records every parallelization
//! decision in `ConcurrencyAnalysis.function_decisions` (per function, a
//! list of [`crate::concurrency::ParallelGroup`]s each tagged
//! `is_trivial`). That is everything needed, so — mirroring the
//! P1.1/P1.2/P1.3 analyzers — this is a plain-data walk over the
//! finished result, run from the CLI's `query_queries` collator, rather
//! than a `queries`-field producer threaded into the pass. It consumes
//! `&Program` (to map a group's statement index to a source span) +
//! `&ConcurrencyAnalysis` and emits `Vec<CompilerQuery>`.
//!
//! ### v1 limitation — coarse cost model
//!
//! The catalogue scopes P1.6's cost model as "unspecified for v1": the
//! only fork signal exposed today is the binary `is_trivial` gate
//! (`non_constant_count <= 1`), not a numeric work estimate. So this
//! analyzer surfaces *every* auto-fork decision rather than only the
//! near-threshold ("marginal") ones a real cost model would let it
//! filter to. When a numeric cost model lands, the same query can
//! narrow to forks whose work estimate sits within a band of the
//! threshold. Forked groups are already the non-trivial-work sites
//! (the cheap ones are filtered out as `is_trivial`), so the surface is
//! bounded, not every statement pair.

use crate::ast::*;
use crate::concurrency::ConcurrencyAnalysis;
use crate::def_path::{DefPath, QueryId, SubItemHash};
use crate::queries::{CompilerQuery, Confidence, Phase, QueryKind, QueryOption, ResolutionSurface};
use crate::token::Span;
use std::collections::HashMap;
use std::collections::HashSet;

/// Entry point. Emits one [`QueryKind::ForkThresholdDecision`] per
/// non-trivial parallel group (an auto-fork) in `concurrency`, unless
/// the enclosing function already carries `#[fork_at]`.
pub fn analyze(program: &Program, concurrency: &ConcurrencyAnalysis) -> Vec<CompilerQuery> {
    let suppressed = collect_fork_at_fns(program);
    let bodies = collect_fn_bodies(program);

    let mut queries = Vec::new();
    for (fn_key, fc) in &concurrency.function_decisions {
        if suppressed.contains(fn_key) {
            continue;
        }
        let Some(body) = bodies.get(fn_key.as_str()) else {
            continue; // no findable body (e.g. trait-default keyed elsewhere)
        };
        let def_path = fn_key_to_def_path(fn_key);
        for group in &fc.parallel_groups {
            // `is_trivial` groups are inlined, not forked — no decision
            // to surface. A group must also have >= 2 members to be a
            // real fork (one statement is not parallel).
            if group.is_trivial || group.statement_indices.len() < 2 {
                continue;
            }
            let Some(&first_idx) = group.statement_indices.iter().min() else {
                continue;
            };
            let Some(stmt) = body.stmts.get(first_idx) else {
                continue; // index out of range for this body — skip defensively
            };
            queries.push(build_fork_query(
                &def_path,
                &stmt.span,
                group.statement_indices.len(),
            ));
        }
    }

    queries.sort_by_key(|q| q.site.offset);
    queries
}

/// Function keys (`fn_name` / `Type.method`) whose definition carries
/// `#[fork_at]` — the P1.6 resolution surface. An annotated function has
/// already resolved the decision, so its forks emit no query.
fn collect_fork_at_fns(program: &Program) -> HashSet<String> {
    let mut out = HashSet::new();
    for item in &program.items {
        match item {
            Item::Function(f) if has_fork_at_attr(&f.attributes) => {
                out.insert(f.name.clone());
            }
            Item::ImplBlock(imp) => {
                let TypeKind::Path(p) = &imp.target_type.kind else {
                    continue;
                };
                let Some(target) = p.segments.last() else {
                    continue;
                };
                for it in &imp.items {
                    if let ImplItem::Method(m) = it {
                        if has_fork_at_attr(&m.attributes) {
                            out.insert(format!("{}.{}", target, m.name));
                        }
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Map each function key to its body block, so a group's statement index
/// can be resolved to a source span. Mirrors the keying that
/// `concurrency::analyze` uses to populate `function_decisions`.
fn collect_fn_bodies(program: &Program) -> HashMap<String, &Block> {
    let mut out = HashMap::new();
    for item in &program.items {
        match item {
            Item::Function(f) => {
                out.insert(f.name.clone(), &f.body);
            }
            Item::ImplBlock(imp) => {
                let TypeKind::Path(p) = &imp.target_type.kind else {
                    continue;
                };
                let Some(target) = p.segments.last() else {
                    continue;
                };
                for it in &imp.items {
                    if let ImplItem::Method(m) = it {
                        out.insert(format!("{}.{}", target, m.name), &m.body);
                    }
                }
            }
            _ => {}
        }
    }
    out
}

fn has_fork_at_attr(attrs: &[Attribute]) -> bool {
    attrs
        .iter()
        .any(|a| a.path.len() == 1 && a.path[0] == "fork_at")
}

/// `"Type.method"` → `DefPath::new(["Type", "method"])`; bare name →
/// `DefPath::item(name)`.
fn fn_key_to_def_path(fn_key: &str) -> DefPath {
    match fn_key.split_once('.') {
        Some((ty, method)) => DefPath::new(vec![ty.to_string(), method.to_string()]),
        None => DefPath::item(fn_key.to_string()),
    }
}

fn build_fork_query(def_path: &DefPath, site: &Span, group_size: usize) -> CompilerQuery {
    CompilerQuery {
        // One query per forked group; the group's first-statement offset
        // disambiguates multiple forks within one function.
        id: QueryId {
            def_path: def_path.clone(),
            sub_item_hash: SubItemHash(site.offset as u64),
        },
        site: site.clone(),
        kind: QueryKind::ForkThresholdDecision,
        options: vec![
            QueryOption {
                label: "keep_auto".to_string(),
                note: Some(format!(
                    "the auto-parallelizer forks these {} statements; keep the cost-model decision",
                    group_size,
                )),
            },
            QueryOption {
                label: "pin_fork".to_string(),
                note: Some(
                    "lock the fork in with `#[fork_at]` so the group parallelizes regardless of \
                     future cost-model tuning"
                        .to_string(),
                ),
            },
            QueryOption {
                label: "keep_sequential".to_string(),
                note: Some(
                    "raise the fork threshold via `#[fork_at]` so this group runs sequentially \
                     (e.g. when the per-spawn cost outweighs the work)"
                        .to_string(),
                ),
            },
        ],
        // The compiler's standing pick is to keep its auto decision.
        default: 0,
        // `Low`: the v1 fork gate is a crude `is_trivial` heuristic (the
        // real cost model is unspecified for v1), so the decision is
        // genuinely worth the author's confirmation.
        default_confidence: Confidence::Low,
        resolution_surface: ResolutionSurface {
            attributes: vec!["fork_at".to_string()],
        },
        cross_phase_origin: Some(Phase::Concurrency),
    }
}
