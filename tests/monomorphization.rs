//! Tests for the monomorphization analyzer (phase-7-codegen.md line
//! 97). Each test parses + typechecks a hand-rolled program, runs the
//! analyzer over the result, and asserts the per-generic / per-
//! instance shape.

use karac::monomorphization::{analyze, MonomorphizationTable};
use karac::{desugar_program, parse, resolve, typecheck};

fn analyze_program(src: &str) -> MonomorphizationTable {
    let mut pr = parse(src);
    assert!(
        pr.errors.is_empty(),
        "expected clean parse; got errors: {:?}",
        pr.errors,
    );
    desugar_program(&mut pr.program);
    let resolved = resolve(&pr.program);
    assert!(
        resolved.errors.is_empty(),
        "expected clean resolve; got errors: {:?}",
        resolved.errors,
    );
    let tc = typecheck(&pr.program, &resolved);
    analyze(&pr.program, &tc)
}

#[test]
fn empty_program_has_no_generics() {
    let src = "fn main() {}";
    let table = analyze_program(src);
    assert_eq!(table.generic_count(), 0);
    assert_eq!(table.instance_count(), 0);
}

#[test]
fn non_generic_call_does_not_record_an_instance() {
    // `add` is monomorphic — no type-param substitutions, so the
    // analyzer ignores it entirely (the typechecker's
    // `record_call_type_subs` early-returns on empty solutions, and
    // we filter on a non-empty entry presence).
    let src = r#"
fn add(a: i64, b: i64) -> i64 { a + b }
fn main() {
    let _ = add(1, 2);
}
"#;
    let table = analyze_program(src);
    assert_eq!(
        table.generic_count(),
        0,
        "non-generic call must not appear in the table; got {:?}",
        table.by_generic,
    );
}

#[test]
fn single_generic_with_one_instantiation_records_one_instance() {
    // `identity[T](x: T) -> T` called once at type `i64` — one
    // generic, one instance, types = `["i64"]`.
    let src = r#"
fn identity[T](x: T) -> T { x }
fn main() {
    let _ = identity(7);
}
"#;
    let table = analyze_program(src);
    assert_eq!(table.generic_count(), 1);
    assert_eq!(table.instance_count(), 1);
    let g = &table.by_generic[0];
    assert_eq!(g.generic, "identity");
    assert_eq!(g.instances.len(), 1);
    assert_eq!(g.instances[0].types, vec!["i64".to_string()]);
    assert!(
        g.instances[0].effects.is_empty(),
        "v1 effects slot is always empty; got {:?}",
        g.instances[0].effects,
    );
}

#[test]
fn distinct_type_args_produce_distinct_instances() {
    // Two callers at different types → two instances under one
    // generic.
    let src = r#"
fn identity[T](x: T) -> T { x }
fn main() {
    let _ = identity(7);
    let _ = identity(true);
}
"#;
    let table = analyze_program(src);
    assert_eq!(table.generic_count(), 1);
    assert_eq!(table.instance_count(), 2);
    let g = &table.by_generic[0];
    assert_eq!(g.generic, "identity");
    let mut tys: Vec<String> = g.instances.iter().map(|i| i.types[0].clone()).collect();
    tys.sort();
    assert_eq!(tys, vec!["bool".to_string(), "i64".to_string()]);
}

#[test]
fn same_type_args_at_multiple_sites_collapse_to_one_instance() {
    // Two callers at the same type → one instance. First call-site
    // wins on `site` (lower offset).
    let src = r#"
fn identity[T](x: T) -> T { x }
fn main() {
    let _ = identity(1);
    let _ = identity(2);
}
"#;
    let table = analyze_program(src);
    assert_eq!(table.generic_count(), 1);
    assert_eq!(
        table.instance_count(),
        1,
        "two i64 callers must dedup to one instance; got {:?}",
        table.by_generic,
    );
    let g = &table.by_generic[0];
    assert_eq!(g.instances[0].types, vec!["i64".to_string()]);
}

#[test]
fn multi_param_generic_records_types_alphabetical_by_name() {
    // `pair[A, B](a: A, b: B) -> A` — substitutions are sorted by
    // param-name (A, B) so the `types` list is deterministic across
    // runs.
    let src = r#"
fn pair[A, B](a: A, b: B) -> A { a }
fn main() {
    let _ = pair(1, true);
}
"#;
    let table = analyze_program(src);
    assert_eq!(table.generic_count(), 1);
    let g = &table.by_generic[0];
    assert_eq!(g.generic, "pair");
    assert_eq!(g.instances.len(), 1);
    // A=i64, B=bool → alphabetical (A, B) → ["i64", "bool"].
    assert_eq!(
        g.instances[0].types,
        vec!["i64".to_string(), "bool".to_string()],
    );
}

#[test]
fn site_field_is_the_first_call_offset_when_dedup_collapses() {
    // Two `identity(7)` call sites at the same type tuple — the
    // analyzer keeps the first (lower-offset) call as the site.
    let src = r#"
fn identity[T](x: T) -> T { x }
fn main() {
    let _ = identity(1);
    let _ = identity(2);
}
"#;
    let table = analyze_program(src);
    let g = &table.by_generic[0];
    let first = &g.instances[0];
    assert!(
        first.site.line > 0,
        "expected a real source span on the instance",
    );
    // First `identity(1)` is on the line after `fn main() {`. The
    // assertion is loose so future formatting tweaks don't break the
    // test — what we care about is that *some* span is recorded and
    // it's the earlier one (line is monotonic in source order).
    let later_offset = src.find("identity(2)").expect("fixture has second call");
    assert!(
        first.site.offset < later_offset,
        "expected first call-site offset to be earlier than the second; got first.offset={} later.offset={}",
        first.site.offset,
        later_offset,
    );
}

#[test]
fn generics_sorted_alphabetically_by_name() {
    let src = r#"
fn zeta[T](x: T) -> T { x }
fn alpha[T](x: T) -> T { x }
fn main() {
    let _ = zeta(1);
    let _ = alpha(2);
}
"#;
    let table = analyze_program(src);
    assert_eq!(table.generic_count(), 2);
    assert_eq!(table.by_generic[0].generic, "alpha");
    assert_eq!(table.by_generic[1].generic, "zeta");
}

#[test]
fn analyzer_walks_into_loops_branches_and_nested_blocks() {
    // The walker must visit every expression position; smoke-test
    // that calls inside `for` / `if` / nested block bodies still get
    // recorded.
    let src = r#"
fn identity[T](x: T) -> T { x }
fn main() {
    for i in 0..3 {
        if i > 0 {
            let _ = identity(i);
        }
    }
}
"#;
    let table = analyze_program(src);
    assert_eq!(table.generic_count(), 1);
    assert_eq!(table.instance_count(), 1);
    assert_eq!(table.by_generic[0].generic, "identity");
}
