//! RC-fallback queries — P1.1 catalogue entry, phase-8-stdlib-floor.md
//! § Compiler queries channel. Spec at `docs/design.md § Feature 4
//! Part 4 — RC Fallback with Budget Controls` (P1.1 row of the P1
//! catalogue table).
//!
//! Surfaces [`QueryKind::RcFallbackDecision`]: a binding for which the
//! ownership pass inserted a reference-count fallback (because the value
//! is used after a non-dominating consume) is a site where the author
//! may prefer to restructure for zero-cost single ownership, accept the
//! RC, or forbid it outright. One query per RC-fallback binding, keyed
//! on the enclosing function's def-path. Resolution surface:
//! `#[prefer_rc]` (accept) / `#[no_rc]` (forbid) on the function or the
//! value's type.
//!
//! ### Why this lives outside the ownership pass
//!
//! The ownership pass already records every RC fallback in
//! `OwnershipCheckResult.rc_values` (one [`crate::ownership::RcEntry`]
//! per binding, carrying the trigger and both spans) and silences the
//! perf note for any function annotated `#[allow(rc_fallback)]` via
//! `suppressed_rc_fn_keys`. That is everything needed to reconstruct the
//! decision, so — mirroring the P1.2 [`crate::specialization_queries`]
//! and P1.3 [`crate::codegen_queries`] analyzers — this is a plain-data
//! walk over the finished result rather than a `queries`-field producer
//! threaded into the pass. It runs from the CLI's `query_queries`
//! collator and emits `Vec<CompilerQuery>`.
//!
//! ### Suppression (already-resolved sites)
//!
//! A query is *not* emitted when the decision is already resolved:
//! - the enclosing function carries `#[no_rc]` or `#[prefer_rc]`;
//! - the function carries `#[allow(rc_fallback)]` (its note was
//!   silenced — `suppressed_rc_fn_keys`);
//! - the value's type is a `#[no_rc]` struct.
//!
//! `#[no_rc]` cases double as ownership *errors* at the use site, so
//! suppressing them here keeps the query report aligned with the notes.
//!
//! ### v1 limitation — type-level `#[no_rc]` on locals
//!
//! The `#[no_rc]`-type check keys on `RcEntry.type_name`, which the
//! ownership pass populates for parameter and `self` bindings but not
//! for local `let` bindings. So an RC-promoted *local* of a `#[no_rc]`
//! type is not suppressed by the type check. This is harmless: such a
//! value is already a hard `NoRcViolation` ownership error at the use
//! site, so the author is fixing the error regardless — the redundant
//! query is informational, not a wrong suggestion that survives a clean
//! build. Function-level `#[no_rc]` / `#[prefer_rc]` / `#[allow(rc_fallback)]`
//! suppression is unaffected (it keys on the function, not the binding).

use crate::ast::*;
use crate::def_path::{DefPath, QueryId, SubItemHash};
use crate::ownership::{OwnershipCheckResult, RcEntry};
use crate::queries::{CompilerQuery, Confidence, Phase, QueryKind, QueryOption, ResolutionSurface};
use std::collections::HashSet;

/// Entry point. Emits one [`QueryKind::RcFallbackDecision`] per
/// RC-fallback binding in `ownership.rc_values` whose site is not
/// already resolved (see module doc § Suppression).
pub fn analyze(program: &Program, ownership: &OwnershipCheckResult) -> Vec<CompilerQuery> {
    let resolved_fns = collect_resolved_fns(program);
    let no_rc_types = collect_no_rc_types(program);

    // Flatten to (fn_key, entry) and sort for deterministic output —
    // `rc_values` and its inner maps are `HashMap`s.
    let mut sites: Vec<(&String, &RcEntry)> = Vec::new();
    for (fn_key, bindings) in &ownership.rc_values {
        if resolved_fns.contains(fn_key) || ownership.suppressed_rc_fn_keys.contains(fn_key) {
            continue;
        }
        for entry in bindings.values() {
            if let Some(ty) = &entry.type_name {
                if no_rc_types.contains(ty) {
                    continue; // `#[no_rc]` type — forbidden, not a query
                }
            }
            sites.push((fn_key, entry));
        }
    }
    sites.sort_by(|(ak, ae), (bk, be)| {
        ae.other_use_span
            .offset
            .cmp(&be.other_use_span.offset)
            .then_with(|| ak.cmp(bk))
            .then_with(|| ae.binding.cmp(&be.binding))
    });

    sites
        .into_iter()
        .map(|(fn_key, entry)| {
            let is_arc = ownership
                .arc_values
                .get(fn_key)
                .is_some_and(|s| s.contains(&entry.binding));
            build_rc_fallback_query(&fn_key_to_def_path(fn_key), entry, is_arc)
        })
        .collect()
}

/// Function keys (`fn_name` / `Type.method`) whose definition already
/// carries `#[no_rc]` or `#[prefer_rc]` — the P1.1 resolution surface.
/// Mirrors the collection in `ownership::enforce_no_rc_attrs` so the
/// query report agrees with the pass's own `#[no_rc]` handling.
fn collect_resolved_fns(program: &Program) -> HashSet<String> {
    let mut out = HashSet::new();
    for item in &program.items {
        match item {
            Item::Function(f) if has_rc_resolution_attr(&f.attributes) => {
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
                        if has_rc_resolution_attr(&m.attributes) {
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

/// Names of `#[no_rc]` struct types — a value of such a type can never
/// be RC'd, so the decision is resolved at the type, not the use site.
fn collect_no_rc_types(program: &Program) -> HashSet<String> {
    program
        .items
        .iter()
        .filter_map(|item| match item {
            Item::StructDef(s) if s.no_rc => Some(s.name.clone()),
            _ => None,
        })
        .collect()
}

fn has_rc_resolution_attr(attrs: &[Attribute]) -> bool {
    attrs
        .iter()
        .any(|a| a.path.len() == 1 && (a.path[0] == "no_rc" || a.path[0] == "prefer_rc"))
}

/// `"Type.method"` → `DefPath::new(["Type", "method"])`; a bare name →
/// `DefPath::item(name)`. Kāra identifiers contain no `.`, so a single
/// split is correct.
fn fn_key_to_def_path(fn_key: &str) -> DefPath {
    match fn_key.split_once('.') {
        Some((ty, method)) => DefPath::new(vec![ty.to_string(), method.to_string()]),
        None => DefPath::item(fn_key.to_string()),
    }
}

fn build_rc_fallback_query(def_path: &DefPath, entry: &RcEntry, is_arc: bool) -> CompilerQuery {
    let kind_word = if is_arc { "Arc-shared" } else { "Rc-shared" };
    CompilerQuery {
        // One query per binding; the later use-site offset disambiguates
        // multiple RC fallbacks within one function (`SubItemHash` is a
        // per-compile site index, matching `codegen_queries`).
        id: QueryId {
            def_path: def_path.clone(),
            sub_item_hash: SubItemHash(entry.other_use_span.offset as u64),
        },
        site: entry.other_use_span.clone(),
        kind: QueryKind::RcFallbackDecision,
        options: vec![
            QueryOption {
                label: "keep_rc".to_string(),
                note: Some(format!(
                    "`{}` is {} here ({}); accept the reference-counting overhead",
                    entry.binding,
                    kind_word,
                    entry.trigger.label(),
                )),
            },
            QueryOption {
                label: "prefer_rc".to_string(),
                note: Some(
                    "confirm the RC fallback is intended with `#[prefer_rc]` (silences this query)"
                        .to_string(),
                ),
            },
            QueryOption {
                label: "no_rc".to_string(),
                note: Some(
                    "forbid RC with `#[no_rc]` — requires restructuring to single ownership \
                     (clone or borrow), else it becomes a compile error here"
                        .to_string(),
                ),
            },
        ],
        // The compiler already inserted RC, so its standing pick is
        // `keep_rc`. `Medium` (not `Low`): the fallback is a deterministic
        // dataflow result, not a heuristic guess — but it carries a real
        // perf cost the author may want to design away, so it is still
        // worth surfacing (above the `High` "barely worth a look" band).
        default: 0,
        default_confidence: Confidence::Medium,
        resolution_surface: ResolutionSurface {
            attributes: vec!["no_rc".to_string(), "prefer_rc".to_string()],
        },
        cross_phase_origin: Some(Phase::Ownership),
    }
}
