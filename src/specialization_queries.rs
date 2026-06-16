//! Specialization queries — P1.2 catalogue entry, phase-8-stdlib-floor.md
//! § Compiler queries channel. Spec at `docs/design.md § Specification
//! Layers > Compiler Queries` (P1.2 row) + § Generic Monomorphization.
//!
//! Surfaces [`QueryKind::SpecializationDecision`]: a user-defined generic
//! free function that is monomorphized into many distinct concrete type
//! tuples (a *fan-out* — one definition, many emitted bodies) is a site
//! where the author may want to mark a hot tuple for a dedicated
//! specialized body. The query reads the monomorphization counter
//! (`karac query monomorphization` / [`crate::monomorphization::analyze`])
//! and emits one query per qualifying generic, with each concrete tuple
//! folded into the option list. Resolution surface: the `#[specialize]`
//! attribute on the generic's definition.
//!
//! Note this is *not* trait-impl specialization — Kāra permanently
//! rejects overlapping-impl specialization (design.md § "Why not allow
//! specialization"). P1.2 is purely a codegen-shape hint: every generic
//! already monomorphizes to one body per type tuple; the query asks
//! whether the author wants a *dedicated, separately-tuned* body for a
//! specific hot tuple. Like P1.3, v1 only *surfaces* the decision and
//! advertises the attribute; the attribute's codegen semantics are a
//! separate follow-up.
//!
//! ### Why this lives outside the typechecker pass
//!
//! The design names the "typechecker monomorphization counter" as the
//! data source, and that counter (instantiation counts aggregated across
//! call sites) is produced by [`crate::monomorphization::analyze`] *after*
//! typecheck completes — not during the single typechecker pass. So,
//! mirroring the P1.3 [`crate::codegen_queries`] analyzer, this is a
//! plain-data walk run from the CLI's `query_queries` collator rather
//! than a `TypeCheckResult.queries` producer. It consumes `&Program` +
//! `&TypeCheckResult` (+ optional `&EffectCheckResult`) and emits
//! `Vec<CompilerQuery>`.
//!
//! ### v1 limitations
//!
//! - **Free functions only.** `#[specialize(T = i64)]` maps cleanly to a
//!   function's *own* type parameters. Impl methods whose type variables
//!   come from the enclosing `impl[T]` block, and stdlib collection
//!   methods (`Vec.push`) the user cannot annotate, are out of v1 scope —
//!   they have no annotatable definition keyed to the method's own
//!   params. We under-report rather than over-report.
//! - **Counts distinct *type* tuples, not (type, effect) tuples.** The
//!   resolution surface is type-keyed (`#[specialize(T = i64)]`), so the
//!   threshold and the option list dedup on the resolved type tuple; an
//!   effect-polymorphic generic instantiated at one type with several
//!   effect sets counts as one type-specialization candidate.
//! - **Path-form callees are not attributed.** A generic invoked as
//!   `Module::process(...)` keys the monomorphization table under
//!   `"Module::process"`, which won't match a `fn process` definition
//!   collected by bare name. Conservative (under-report), matching the
//!   `codegen_queries` call-site discovery.

use crate::ast::*;
use crate::def_path::{DefPath, QueryId, SubItemHash};
use crate::effectchecker::EffectCheckResult;
use crate::queries::{CompilerQuery, Confidence, Phase, QueryKind, QueryOption, ResolutionSurface};
use crate::token::Span;
use crate::typechecker::TypeCheckResult;
use std::collections::{BTreeSet, HashMap};

/// Minimum number of distinct concrete *type* tuples a generic must be
/// instantiated at before its specialization decision is worth asking.
/// Four is a deliberately conservative "notable fan-out" bar for v1 —
/// below it the monomorphization cost is modest and the report would be
/// noise; the catalogue is informational, so we under-surface rather
/// than spam. (The tracker's worked example is "monomorphized 14×".)
const SPECIALIZATION_QUERY_MIN_TUPLES: usize = 4;

/// Cap on how many per-tuple `specialize_…` options one query lists.
/// A generic instantiated at dozens of tuples would otherwise produce an
/// unwieldy option list; the total count is always surfaced in the
/// `no_specialize` option's note so a cap never hides the fan-out size.
const SPECIALIZATION_QUERY_MAX_TUPLE_OPTIONS: usize = 8;

/// Entry point. Emits one [`QueryKind::SpecializationDecision`] per
/// user-defined generic free function whose monomorphization fan-out
/// meets [`SPECIALIZATION_QUERY_MIN_TUPLES`] and which is not already
/// annotated `#[specialize]`. `ec` is threaded into the monomorphization
/// analyzer only to keep its signature uniform; the type-tuple counts
/// this query reads do not depend on effect resolution.
pub fn analyze(
    program: &Program,
    tc: &TypeCheckResult,
    ec: Option<&EffectCheckResult>,
) -> Vec<CompilerQuery> {
    let table = crate::monomorphization::analyze(program, tc, ec);
    let defs = collect_generic_fn_defs(program);

    let mut queries = Vec::new();
    for record in &table.by_generic {
        let info = match defs.get(&record.generic) {
            Some(info) => info,
            None => continue, // not a user-defined free generic fn
        };
        if info.has_specialize_attr {
            continue; // already resolved at the definition
        }
        // Dedup on the resolved type tuple — the resolution surface is
        // type-keyed, so effect-only variants of one type tuple are a
        // single specialization candidate. `BTreeSet` also sorts.
        let type_tuples: BTreeSet<Vec<String>> =
            record.instances.iter().map(|i| i.types.clone()).collect();
        if type_tuples.len() < SPECIALIZATION_QUERY_MIN_TUPLES {
            continue;
        }
        queries.push(build_specialization_query(
            &record.generic,
            info,
            &type_tuples,
        ));
    }

    // The monomorphization table is sorted by generic name; re-sort the
    // emitted queries by definition-site offset for deterministic,
    // source-order tooling output (matches `codegen_queries`).
    queries.sort_by_key(|q| q.site.offset);
    queries
}

/// Definition metadata for a user-defined generic free function.
struct GenericFnInfo {
    def_path: DefPath,
    span: Span,
    /// Type-parameter names sorted alphabetically — the same order
    /// `monomorphization::ordered_types` emits the resolved types in, so
    /// `zip(param_names, tuple)` reconstructs each `{T = i64}` binding.
    sorted_param_names: Vec<String>,
    has_specialize_attr: bool,
}

/// Collect every `pub`/private generic free function keyed by the bare
/// name the monomorphization table attributes its instances to. A
/// function qualifies only if it declares at least one *type* parameter
/// (effect-only `with E` generics have no type tuple to specialize on).
fn collect_generic_fn_defs(program: &Program) -> HashMap<String, GenericFnInfo> {
    let mut out = HashMap::new();
    for item in &program.items {
        let Item::Function(f) = item else {
            continue;
        };
        let Some(generics) = &f.generic_params else {
            continue;
        };
        if generics.params.is_empty() {
            continue; // effect-only generic — nothing type-keyed to specialize
        }
        let mut sorted_param_names: Vec<String> =
            generics.params.iter().map(|p| p.name.clone()).collect();
        sorted_param_names.sort();
        out.insert(
            f.name.clone(),
            GenericFnInfo {
                def_path: DefPath::item(f.name.clone()),
                span: f.span.clone(),
                sorted_param_names,
                has_specialize_attr: has_specialize_attr(&f.attributes),
            },
        );
    }
    out
}

/// True when any attribute on the item is a bare `#[specialize]` /
/// `#[specialize(...)]` — the P1.2 resolution surface. Argument shape is
/// not inspected (any `specialize` annotation resolves the query at the
/// definition, exactly as `#[inline]` resolves the P1.3 inlining query).
fn has_specialize_attr(attrs: &[Attribute]) -> bool {
    attrs
        .iter()
        .any(|a| a.path.len() == 1 && a.path[0] == "specialize")
}

fn build_specialization_query(
    generic: &str,
    info: &GenericFnInfo,
    type_tuples: &BTreeSet<Vec<String>>,
) -> CompilerQuery {
    let total = type_tuples.len();
    let mut options: Vec<QueryOption> = type_tuples
        .iter()
        .take(SPECIALIZATION_QUERY_MAX_TUPLE_OPTIONS)
        .map(|tuple| {
            let binding = render_binding(&info.sorted_param_names, tuple);
            QueryOption {
                label: format!("specialize_{}", tuple.join("_")),
                note: Some(format!(
                    "emit a dedicated specialized body for {{{}}}",
                    binding
                )),
            }
        })
        .collect();

    // Default: leave every instantiation as a standard monomorphization.
    // Its note always carries the full fan-out count, so the
    // `SPECIALIZATION_QUERY_MAX_TUPLE_OPTIONS` cap never hides how many
    // tuples there are.
    options.push(QueryOption {
        label: "no_specialize".to_string(),
        note: Some(format!(
            "`{}` monomorphizes to {} distinct type tuples; leave all as standard monomorphizations",
            generic, total
        )),
    });
    let default = options.len() - 1;

    CompilerQuery {
        // Fan-out query: one stable id per generic *definition*, with the
        // many monomorphizations folded into `options`. This is the P1.2
        // stress test of the P0 identity scheme over multi-instance items
        // (phase-8-stdlib-floor.md) — `SubItemHash::ROOT` because the
        // decision site is the item itself, not a sub-expression.
        id: QueryId {
            def_path: info.def_path.clone(),
            sub_item_hash: SubItemHash::ROOT,
        },
        site: info.span.clone(),
        kind: QueryKind::SpecializationDecision,
        options,
        default,
        default_confidence: Confidence::Low,
        resolution_surface: ResolutionSurface {
            attributes: vec!["specialize".to_string()],
        },
        cross_phase_origin: Some(Phase::TypeChecker),
    }
}

/// Render a `{T = i64, U = bool}`-style binding by zipping the
/// alphabetically-sorted type-parameter names with the resolved type
/// tuple (which `monomorphization` emits in that same order). If the
/// arities disagree (defensive — shouldn't happen for a well-formed
/// instantiation), fall back to the bare tuple so the note stays useful.
fn render_binding(sorted_param_names: &[String], tuple: &[String]) -> String {
    if sorted_param_names.len() == tuple.len() {
        sorted_param_names
            .iter()
            .zip(tuple)
            .map(|(name, ty)| format!("{} = {}", name, ty))
            .collect::<Vec<_>>()
            .join(", ")
    } else {
        tuple.join(", ")
    }
}
