//! Tests for the P1.2 specialization-query analyzer
//! (`src/specialization_queries.rs`). Each test runs the full
//! parse → desugar → resolve → typecheck → lower → effectcheck pipeline
//! (so the monomorphization counter the analyzer reads is populated the
//! same way `karac query queries` populates it), then asserts the
//! emitted `SpecializationDecision` queries.

use karac::effectchecker::PublicEffectsPolicy;
use karac::manifest::CompileProfile;
use karac::queries::{CompilerQuery, Confidence, Phase, QueryKind};
use karac::specialization_queries::analyze;
use karac::{desugar_program, effectcheck_with_typecheck_data, lower, parse, resolve, typecheck};

fn analyze_src(src: &str) -> Vec<CompilerQuery> {
    let mut pr = parse(src);
    assert!(pr.errors.is_empty(), "parse errors: {:?}", pr.errors);
    desugar_program(&mut pr.program);
    let resolved = resolve(&pr.program);
    assert!(
        resolved.errors.is_empty(),
        "resolve errors: {:?}",
        resolved.errors
    );
    let tc = typecheck(&pr.program, &resolved);
    let method_types = tc.method_callee_types.clone();
    let call_type_subs = tc.call_type_subs.clone();
    lower(&mut pr.program, &tc);
    let ec = effectcheck_with_typecheck_data(
        &pr.program,
        PublicEffectsPolicy::default(),
        CompileProfile::Default,
        method_types,
        call_type_subs,
    );
    analyze(&pr.program, &tc, Some(&ec))
}

/// A generic free function instantiated at four distinct concrete types
/// (the v1 `SPECIALIZATION_QUERY_MIN_TUPLES` bar). Four distinct
/// suffixed integer literals give four distinct type tuples.
const FOUR_INSTANTIATIONS: &str = r#"
fn identity[T](x: T) -> T { x }
fn main() {
    let _ = identity(1i64);
    let _ = identity(2i32);
    let _ = identity(3u8);
    let _ = identity(4u64);
}
"#;

#[test]
fn specialization_query_fires_for_four_distinct_type_tuples() {
    let queries = analyze_src(FOUR_INSTANTIATIONS);
    let specs: Vec<_> = queries
        .iter()
        .filter(|q| q.kind == QueryKind::SpecializationDecision)
        .collect();
    // Fan-out: ONE query for the generic, not one per instantiation.
    assert_eq!(
        specs.len(),
        1,
        "expected a single fan-out query; got {:?}",
        specs
    );
    let q = specs[0];
    assert_eq!(q.id.def_path.render(), "identity");
    assert_eq!(q.default_confidence, Confidence::Low);
    assert_eq!(q.cross_phase_origin, Some(Phase::TypeChecker));
    assert!(
        q.resolution_surface.attributes == vec!["specialize".to_string()],
        "resolution surface must advertise `specialize`; got {:?}",
        q.resolution_surface.attributes,
    );
}

#[test]
fn specialization_query_folds_every_tuple_into_options() {
    let queries = analyze_src(FOUR_INSTANTIATIONS);
    let q = queries
        .iter()
        .find(|q| q.kind == QueryKind::SpecializationDecision)
        .expect("expected a specialization query");
    // Four per-tuple `specialize_…` options plus the trailing
    // `no_specialize` default.
    assert_eq!(q.options.len(), 5, "got {:?}", q.options);
    let labels: Vec<&str> = q.options.iter().map(|o| o.label.as_str()).collect();
    for ty in ["i64", "i32", "u8", "u64"] {
        assert!(
            labels.contains(&format!("specialize_{ty}").as_str()),
            "missing specialize option for {ty}; got {labels:?}",
        );
    }
    // Default is the last option and is `no_specialize`.
    assert_eq!(q.default, q.options.len() - 1);
    let default_opt = &q.options[q.default];
    assert_eq!(default_opt.label, "no_specialize");
    // The default's note surfaces the full fan-out count even if the
    // per-tuple option list were capped.
    assert!(
        default_opt
            .note
            .as_deref()
            .unwrap_or_default()
            .contains("4 distinct type tuples"),
        "no_specialize note must carry the tuple count; got {:?}",
        default_opt.note,
    );
    // A per-tuple option's note reconstructs the `{T = …}` binding.
    let i64_opt = q
        .options
        .iter()
        .find(|o| o.label == "specialize_i64")
        .unwrap();
    assert!(
        i64_opt
            .note
            .as_deref()
            .unwrap_or_default()
            .contains("T = i64"),
        "per-tuple note must render the binding; got {:?}",
        i64_opt.note,
    );
}

#[test]
fn specialization_query_suppressed_by_specialize_attr() {
    let src = r#"
#[specialize(T = i64)]
fn identity[T](x: T) -> T { x }
fn main() {
    let _ = identity(1i64);
    let _ = identity(2i32);
    let _ = identity(3u8);
    let _ = identity(4u64);
}
"#;
    let queries = analyze_src(src);
    assert!(
        !queries
            .iter()
            .any(|q| q.kind == QueryKind::SpecializationDecision),
        "an annotated generic must not emit a query; got {:?}",
        queries,
    );
}

#[test]
fn specialization_query_does_not_fire_below_threshold() {
    // Three distinct type tuples — under SPECIALIZATION_QUERY_MIN_TUPLES.
    let src = r#"
fn identity[T](x: T) -> T { x }
fn main() {
    let _ = identity(1i64);
    let _ = identity(2i32);
    let _ = identity(3u8);
}
"#;
    let queries = analyze_src(src);
    assert!(
        !queries
            .iter()
            .any(|q| q.kind == QueryKind::SpecializationDecision),
        "three instantiations is below the bar; got {:?}",
        queries,
    );
}

#[test]
fn specialization_query_does_not_fire_for_monomorphic_fn() {
    // Non-generic function called four times — no type fan-out at all.
    let src = r#"
fn inc(x: i64) -> i64 { x + 1 }
fn main() {
    let _ = inc(1i64);
    let _ = inc(2i64);
    let _ = inc(3i64);
    let _ = inc(4i64);
}
"#;
    let queries = analyze_src(src);
    assert!(
        queries.is_empty(),
        "a monomorphic function has no specialization decision; got {:?}",
        queries,
    );
}

#[test]
fn specialization_query_dedups_repeated_type_instantiations() {
    // Many calls but only TWO distinct type tuples — below the bar
    // because the threshold counts distinct tuples, not call sites.
    let src = r#"
fn identity[T](x: T) -> T { x }
fn main() {
    let _ = identity(1i64);
    let _ = identity(2i64);
    let _ = identity(3i64);
    let _ = identity(4i64);
    let _ = identity(5i32);
}
"#;
    let queries = analyze_src(src);
    assert!(
        !queries
            .iter()
            .any(|q| q.kind == QueryKind::SpecializationDecision),
        "two distinct tuples (i64, i32) is below the bar; got {:?}",
        queries,
    );
}
