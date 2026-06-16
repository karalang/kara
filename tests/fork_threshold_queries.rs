//! Tests for the P1.6 fork-threshold query analyzer
//! (`src/fork_threshold_queries.rs`). Each test runs
//! parse → resolve → typecheck → lower → effectcheck → concurrency
//! (the order the CLI pipeline uses before concurrencycheck), then
//! asserts the emitted `ForkThresholdDecision` queries.

use karac::fork_threshold_queries::analyze;
use karac::queries::{CompilerQuery, Confidence, Phase, QueryKind};
use karac::{concurrency_analyze, effectcheck, lower, parse, resolve, typecheck};

fn analyze_src(src: &str) -> Vec<CompilerQuery> {
    let mut pr = parse(src);
    assert!(pr.errors.is_empty(), "parse errors: {:?}", pr.errors);
    let resolved = resolve(&pr.program);
    assert!(
        resolved.errors.is_empty(),
        "resolve errors: {:?}",
        resolved.errors
    );
    let tc = typecheck(&pr.program, &resolved);
    lower(&mut pr.program, &tc);
    let effects = effectcheck(&pr.program);
    let analysis = concurrency_analyze(&pr.program, &effects);
    analyze(&pr.program, &analysis)
}

/// Two effectful calls on independent resources — real structural
/// parallelism the cost-model gate does NOT mark trivial, so the
/// auto-parallelizer forks the group.
const FORK_FIXTURE: &str = r#"
effect resource R1;
effect resource R2;
fn w1() writes(R1) {}
fn w2() writes(R2) {}
fn main() {
    w1();
    w2();
}
"#;

#[test]
fn fork_query_fires_for_an_auto_forked_group() {
    let queries = analyze_src(FORK_FIXTURE);
    let forks: Vec<_> = queries
        .iter()
        .filter(|q| q.kind == QueryKind::ForkThresholdDecision)
        .collect();
    assert_eq!(forks.len(), 1, "expected one fork query; got {:?}", forks);
    let q = forks[0];
    assert_eq!(q.id.def_path.render(), "main");
    assert_eq!(q.cross_phase_origin, Some(Phase::Concurrency));
    assert_eq!(q.default_confidence, Confidence::Low);
    assert_eq!(q.default, 0);
    let labels: Vec<&str> = q.options.iter().map(|o| o.label.as_str()).collect();
    assert_eq!(labels, vec!["keep_auto", "pin_fork", "keep_sequential"]);
    assert_eq!(q.resolution_surface.attributes, vec!["fork_at".to_string()]);
    // keep_auto note names the group size (2 statements).
    let note = q.options[0].note.as_deref().unwrap_or_default();
    assert!(
        note.contains("2 statements"),
        "keep_auto note must name the group size; got {note:?}",
    );
}

#[test]
fn fork_query_suppressed_by_fork_at_on_function() {
    let src = FORK_FIXTURE.replace("fn main() {", "#[fork_at]\nfn main() {");
    let queries = analyze_src(&src);
    assert!(
        !queries
            .iter()
            .any(|q| q.kind == QueryKind::ForkThresholdDecision),
        "a `#[fork_at]`-annotated function emits no query; got {queries:?}",
    );
}

#[test]
fn fork_query_does_not_fire_for_a_trivial_group() {
    // Two independent but pure (constant-cost) statements: the cost-model
    // gate marks the group trivial (inlined, not forked) → no query.
    let src = r#"
fn main() {
    let x: i64 = 1 + 2;
    let y: i64 = 3 + 4;
    let _ = x + y;
}
"#;
    let queries = analyze_src(src);
    assert!(
        !queries
            .iter()
            .any(|q| q.kind == QueryKind::ForkThresholdDecision),
        "a trivial (inlined) group emits no fork query; got {queries:?}",
    );
}

#[test]
fn fork_query_does_not_fire_without_parallelism() {
    // A strict data dependency (`y` reads `x`) serializes the two
    // statements — no parallel group at all, so no fork query.
    let src = r#"
fn main() {
    let x: i64 = 1;
    let y: i64 = x + 1;
    let _ = y;
}
"#;
    let queries = analyze_src(src);
    assert!(
        queries.is_empty(),
        "a serialized body emits no fork query; got {queries:?}",
    );
}
