//! Tests for the monomorphization analyzer (phase-7-codegen.md line
//! 97). Each test parses + typechecks a hand-rolled program, runs the
//! analyzer over the result, and asserts the per-generic / per-
//! instance shape.

use karac::effectchecker::PublicEffectsPolicy;
use karac::manifest::CompileProfile;
use karac::monomorphization::{analyze, MonomorphizationTable};
use karac::{desugar_program, effectcheck_with_typecheck_data, lower, parse, resolve, typecheck};

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
    // Mirror the `karac query monomorphization` pipeline: lower the
    // program, then effect-check it so `call_effect_subs` is populated.
    // call_type_subs spans (recorded pre-lower) survive lowering, so the
    // type tuple still aligns; the effect checker walks the lowered AST.
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
        "a generic with no `with E` variable carries an empty effect set; got {:?}",
        g.instances[0].effects,
    );
}

#[test]
fn compound_polymorphic_call_records_resolved_effect_set() {
    // `run[T, with E]` is generic over `T` (the type tuple) and
    // effect-polymorphic over `E`. The closure passed at the `cb` slot
    // calls `write_log` (`writes(Log)`), so `E` resolves to
    // `{writes(Log)}` — the instance's effective effect set.
    let src = r#"
effect resource Log;
pub fn write_log() with writes(Log) {}
pub fn run[T, with E](x: T, cb: Fn(T) -> () with E) with E { cb(x) }
pub fn main() with writes(Log) {
    run(7i64, |y| write_log())
}
"#;
    let table = analyze_program(src);
    let g = table
        .by_generic
        .iter()
        .find(|g| g.generic == "run")
        .unwrap_or_else(|| panic!("expected `run` generic; got {:?}", table.by_generic));
    assert_eq!(g.instances.len(), 1, "got {:?}", g.instances);
    assert_eq!(g.instances[0].types, vec!["i64".to_string()]);
    assert_eq!(
        g.instances[0].effects,
        vec!["writes(Log)".to_string()],
        "E must resolve to the closure's effect set",
    );
}

#[test]
fn same_types_different_effect_sets_are_distinct_instances() {
    // Two call sites of `run` at the same type (`i64`) but with
    // closures carrying different effects. Per design.md §
    // Monomorphization identity, the resolved effect set is as binding
    // on instance identity as the type tuple, so these are two
    // distinct instances under one generic.
    let src = r#"
effect resource Log;
effect resource Db;
pub fn write_log() with writes(Log) {}
pub fn read_db() with reads(Db) {}
pub fn run[T, with E](x: T, cb: Fn(T) -> () with E) with E { cb(x) }
pub fn main() with writes(Log) reads(Db) {
    run(7i64, |y| write_log());
    run(9i64, |z| read_db())
}
"#;
    let table = analyze_program(src);
    let g = table
        .by_generic
        .iter()
        .find(|g| g.generic == "run")
        .unwrap_or_else(|| panic!("expected `run` generic; got {:?}", table.by_generic));
    assert_eq!(
        g.instances.len(),
        2,
        "same types + differing effect sets must not merge; got {:?}",
        g.instances,
    );
    let mut effect_sets: Vec<Vec<String>> = g.instances.iter().map(|i| i.effects.clone()).collect();
    effect_sets.sort();
    assert_eq!(
        effect_sets,
        vec![
            vec!["reads(Db)".to_string()],
            vec!["writes(Log)".to_string()],
        ],
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
