//! Tests for the P1.5 layout-choice query analyzer
//! (`src/layout_queries.rs`). Each test runs parse → resolve →
//! typecheck (the data the analyzer reads), then asserts the emitted
//! `LayoutChoice` queries.

use karac::layout_queries::analyze;
use karac::queries::{CompilerQuery, Confidence, Phase, QueryKind};
use karac::{parse, resolve, typecheck};

fn analyze_src(src: &str) -> Vec<CompilerQuery> {
    let pr = parse(src);
    assert!(pr.errors.is_empty(), "parse errors: {:?}", pr.errors);
    let resolved = resolve(&pr.program);
    assert!(
        resolved.errors.is_empty(),
        "resolve errors: {:?}",
        resolved.errors
    );
    let tc = typecheck(&pr.program, &resolved);
    assert!(tc.errors.is_empty(), "type errors: {:?}", tc.errors);
    analyze(&pr.program, &tc)
}

#[test]
fn layout_query_fires_for_strict_subset_field_access() {
    // The loop reads only `e.x` — 1 of `Entity`'s 3 fields — so a SoA
    // `layout` block grouping the hot field would improve locality.
    let src = r#"
struct Entity { x: f64, y: f64, hp: i64 }
fn sum_x(entities: Vec[Entity]) -> f64 {
    let mut total: f64 = 0.0;
    for e in entities {
        total = total + e.x;
    }
    total
}
"#;
    let queries = analyze_src(src);
    let layout: Vec<_> = queries
        .iter()
        .filter(|q| q.kind == QueryKind::LayoutChoice)
        .collect();
    assert_eq!(
        layout.len(),
        1,
        "expected one layout query; got {:?}",
        layout
    );
    let q = layout[0];
    assert_eq!(q.id.def_path.render(), "sum_x");
    assert_eq!(q.cross_phase_origin, Some(Phase::Codegen));
    assert_eq!(q.default_confidence, Confidence::Low);
    assert_eq!(q.default, 0);
    let labels: Vec<&str> = q.options.iter().map(|o| o.label.as_str()).collect();
    assert_eq!(labels, vec!["keep_aos", "group_hot_fields"]);
    // Resolution is the `layout` block syntax, not an attribute.
    assert!(q.resolution_surface.attributes.is_empty());
    let note = q.options[0].note.as_deref().unwrap_or_default();
    assert!(
        note.contains("1 of 3 fields") && note.contains('x'),
        "keep_aos note must report the subset; got {note:?}",
    );
}

#[test]
fn layout_query_does_not_fire_when_all_fields_read() {
    // Reading every field has no struct-of-arrays win — each cache line
    // is fully used either way.
    let src = r#"
struct Entity { x: f64, y: f64, hp: i64 }
fn use_all(entities: Vec[Entity]) -> f64 {
    let mut total: f64 = 0.0;
    for e in entities {
        total = total + e.x + e.y;
        let _ = e.hp;
    }
    total
}
"#;
    let queries = analyze_src(src);
    assert!(
        !queries.iter().any(|q| q.kind == QueryKind::LayoutChoice),
        "reading all fields is not a SoA candidate; got {queries:?}",
    );
}

#[test]
fn layout_query_does_not_fire_for_single_field_struct() {
    // A one-field struct has no grouping choice.
    let src = r#"
struct One { v: i64 }
fn f(items: Vec[One]) -> i64 {
    let mut total: i64 = 0;
    for e in items {
        total = total + e.v;
    }
    total
}
"#;
    let queries = analyze_src(src);
    assert!(
        !queries.iter().any(|q| q.kind == QueryKind::LayoutChoice),
        "a single-field struct has nothing to group; got {queries:?}",
    );
}

#[test]
fn layout_query_suppressed_by_existing_layout_block() {
    // `Entity` already has a `layout` block — the decision is resolved.
    let src = r#"
struct Entity { x: f64, y: f64, hp: i64 }
layout entities: Vec[Entity] {
    group physics { x, y }
    group combat { hp }
}
fn sum_x(es: Vec[Entity]) -> f64 {
    let mut total: f64 = 0.0;
    for e in es {
        total = total + e.x;
    }
    total
}
"#;
    let queries = analyze_src(src);
    assert!(
        !queries.iter().any(|q| q.kind == QueryKind::LayoutChoice),
        "a struct with a layout block emits no query; got {queries:?}",
    );
}

#[test]
fn layout_query_does_not_fire_for_non_struct_collection() {
    // `Vec[i64]` has no fields to group.
    let src = r#"
fn sum(nums: Vec[i64]) -> i64 {
    let mut total: i64 = 0;
    for n in nums {
        total = total + n;
    }
    total
}
"#;
    let queries = analyze_src(src);
    assert!(
        queries.is_empty(),
        "a primitive-element collection emits no query; got {queries:?}",
    );
}
