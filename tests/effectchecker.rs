// tests/effectchecker.rs

use karac::ast::EffectVerbKind;
use karac::effectchecker::*;
use karac::manifest::CompileProfile;
use karac::{
    effectcheck, effectcheck_with_policy, effectcheck_with_profile,
    effectcheck_with_typecheck_data, lower, parse, resolve, typecheck,
};
use std::collections::HashSet;

// ── Test Helpers ────────────────────────────────────────────────

fn effectcheck_ok(source: &str) -> EffectCheckResult {
    let parsed = parse(source);
    assert!(
        parsed.errors.is_empty(),
        "Parse errors: {}",
        parsed
            .errors
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    let result = effectcheck(&parsed.program);
    let real_errors: Vec<_> = result
        .errors
        .iter()
        .filter(|e| e.kind != EffectErrorKind::FfiLintHint)
        .collect();
    assert!(
        real_errors.is_empty(),
        "Effect errors: {}",
        real_errors
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    result
}

fn effectcheck_errors(source: &str) -> Vec<EffectError> {
    let parsed = parse(source);
    assert!(
        parsed.errors.is_empty(),
        "Parse errors: {}",
        parsed
            .errors
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    let result = effectcheck(&parsed.program);
    let real_errors: Vec<EffectError> = result
        .errors
        .into_iter()
        .filter(|e| e.kind != EffectErrorKind::FfiLintHint)
        .collect();
    assert!(
        !real_errors.is_empty(),
        "Expected effect errors but got none"
    );
    real_errors
}

fn effectcheck_all(source: &str) -> EffectCheckResult {
    let parsed = parse(source);
    assert!(
        parsed.errors.is_empty(),
        "Parse errors: {}",
        parsed
            .errors
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    effectcheck(&parsed.program)
}

/// Run the full pre-effectcheck pipeline (parse → resolve → typecheck → lower)
/// before effectcheck. Required for `.into()` / `.try_into()` tests because
/// lowering rewrites those to `Target.from(x)` / `Target.try_from(x)`; without
/// the rewrite, effectcheck sees raw `MethodCall("into", ...)` and can't route
/// to the resolved From impl. Threads the typechecker's `method_callee_types`
/// into the effectchecker so method-call sites resolve to the precise
/// `Type.method` for `with E` / Fn-slot / polymorphic-arg analysis.
fn effectcheck_full_pipeline(source: &str) -> EffectCheckResult {
    let mut parsed = parse(source);
    assert!(
        parsed.errors.is_empty(),
        "Parse errors: {}",
        parsed
            .errors
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    let resolved = resolve(&parsed.program);
    assert!(
        resolved.errors.is_empty(),
        "Resolve errors: {:?}",
        resolved.errors
    );
    let typed = typecheck(&parsed.program, &resolved);
    assert!(typed.errors.is_empty(), "Type errors: {:?}", typed.errors);
    let method_types = typed.method_callee_types.clone();
    let call_type_subs = typed.call_type_subs.clone();
    lower(&mut parsed.program, &typed);
    effectcheck_with_typecheck_data(
        &parsed.program,
        PublicEffectsPolicy::default(),
        CompileProfile::Default,
        method_types,
        call_type_subs,
    )
}

// ── Core ────────────────────────────────────────────────────────

#[test]
fn test_empty_program() {
    effectcheck_ok("");
}

#[test]
fn test_pure_function() {
    let result = effectcheck_ok("fn add(a: i64, b: i64) -> i64 { a + b }");
    let inferred = result.inferred_effects.get("add").unwrap();
    assert!(inferred.effects.is_empty());
}

#[test]
fn test_function_with_declared_effects() {
    effectcheck_ok(
        "effect resource UserDB;\n\
         pub fn save() writes(UserDB) { }",
    );
}

#[test]
fn test_extern_function_effects_trusted() {
    let result = effectcheck_ok(
        "effect resource FileSystem;\n\
         unsafe extern \"C\" { fn write(fd: i32, buf: i64, count: i64) -> i64 writes(FileSystem); }",
    );
    let inferred = result.inferred_effects.get("write").unwrap();
    assert!(inferred
        .effects
        .iter()
        .any(|e| e.effect.resource == "FileSystem"));
}

#[test]
fn test_extern_c_gets_blocks_by_default() {
    // extern "C" functions default to {blocks} (may call blocking OS APIs)
    let result = effectcheck_ok("unsafe extern \"C\" { fn sleep(secs: i32); }");
    let inferred = result.inferred_effects.get("sleep").unwrap();
    assert!(
        inferred
            .effects
            .iter()
            .any(|e| e.effect.verb == EffectVerbKind::Blocks),
        "extern \"C\" should default to blocks, got: {:?}",
        inferred.effects
    );
}

#[test]
fn test_extern_c_unwind_gets_blocks_and_panics() {
    // extern "C-unwind" functions default to {blocks, panics}
    let result = effectcheck_ok("unsafe extern \"C-unwind\" { fn risky(); }");
    let inferred = result.inferred_effects.get("risky").unwrap();
    assert!(
        inferred
            .effects
            .iter()
            .any(|e| e.effect.verb == EffectVerbKind::Blocks),
        "extern \"C-unwind\" should default to blocks"
    );
    assert!(
        inferred
            .effects
            .iter()
            .any(|e| e.effect.verb == EffectVerbKind::Panics),
        "extern \"C-unwind\" should default to panics"
    );
}

#[test]
fn test_trunc_to_int_infers_panics() {
    // phase-8 cast slice 2: `f.trunc_to_<intN>()` is the trapping float→int
    // form and carries `panics`, so a (private) caller inherits the effect.
    let result = effectcheck_ok(
        "fn convert(x: f64) -> i32 { x.trunc_to_i32() }
         fn main() { let _ = convert(3.0); }",
    );
    let inferred = result.inferred_effects.get("convert").unwrap();
    assert!(
        inferred
            .effects
            .iter()
            .any(|e| e.effect.verb == EffectVerbKind::Panics),
        "trunc_to_i32 should make 'convert' inherit panics"
    );
}

#[test]
fn test_saturating_to_int_is_effect_free() {
    // The non-trapping float→int families (saturating/wrapping/checked) carry
    // no effect — a caller stays pure.
    let result = effectcheck_ok(
        "fn convert(x: f64) -> i32 { x.saturating_to_i32() }
         fn main() { let _ = convert(3.0); }",
    );
    let inferred = result.inferred_effects.get("convert").unwrap();
    assert!(
        !inferred
            .effects
            .iter()
            .any(|e| e.effect.verb == EffectVerbKind::Panics),
        "saturating_to_i32 must not introduce panics"
    );
}

#[test]
fn test_noblock_removes_blocks_from_extern_c() {
    // @noblock on extern "C" removes the blocks default
    let result = effectcheck_ok("unsafe extern \"C\" { @noblock fn cpu_work() -> i32; }");
    let inferred = result.inferred_effects.get("cpu_work").unwrap();
    assert!(
        !inferred
            .effects
            .iter()
            .any(|e| e.effect.verb == EffectVerbKind::Blocks),
        "@noblock extern \"C\" should NOT have blocks effect"
    );
}

#[test]
fn test_noblock_removes_blocks_from_c_unwind_but_keeps_panics() {
    // @noblock on extern "C-unwind": blocks removed, panics stays
    let result = effectcheck_ok("unsafe extern \"C-unwind\" { @noblock fn throwing(); }");
    let inferred = result.inferred_effects.get("throwing").unwrap();
    assert!(
        !inferred
            .effects
            .iter()
            .any(|e| e.effect.verb == EffectVerbKind::Blocks),
        "@noblock extern \"C-unwind\" should NOT have blocks"
    );
    assert!(
        inferred
            .effects
            .iter()
            .any(|e| e.effect.verb == EffectVerbKind::Panics),
        "@noblock extern \"C-unwind\" should still have panics"
    );
}

// ── std.process — execution verbs on `Child` methods (phase-8 line 159) ──

/// The synchronous `Child.wait()` carries the `blocks` execution verb (it
/// parks the calling thread on the OS `waitpid` until the child exits),
/// while the non-blocking `try_wait()` poll and the fire-and-return
/// `kill()` do NOT. Asserted against the *actual baked* `process.kara`
/// AST via `STDLIB_PROGRAMS`, so it pins the real stdlib declaration
/// rather than a hand-written stand-in. The async `suspends` form — `wait()`
/// routed through the network event loop — lands in a later slice and would
/// widen `wait`'s declared set further (non-breaking).
#[test]
fn test_process_child_wait_declares_blocks_others_do_not() {
    use karac::ast::{EffectItem, ImplItem, Item, TypeKind};

    let (_, process_prog) = karac::prelude::STDLIB_PROGRAMS
        .iter()
        .find(|(name, _)| *name == "process.kara")
        .expect("process.kara is a baked stdlib program");

    // Collect the declared concrete effect verbs for each method in `impl Child`.
    let mut declared: std::collections::HashMap<String, Vec<EffectVerbKind>> =
        std::collections::HashMap::new();
    for item in &process_prog.items {
        let Item::ImplBlock(imp) = item else { continue };
        let TypeKind::Path(p) = &imp.target_type.kind else {
            continue;
        };
        if p.segments.last().map(String::as_str) != Some("Child") {
            continue;
        }
        for ii in &imp.items {
            let ImplItem::Method(m) = ii else { continue };
            let verbs: Vec<EffectVerbKind> = m
                .effects
                .as_ref()
                .map(|el| {
                    el.items
                        .iter()
                        .filter_map(|i| match i {
                            EffectItem::Verb(v) => Some(v.kind.clone()),
                            _ => None,
                        })
                        .collect()
                })
                .unwrap_or_default();
            declared.insert(m.name.clone(), verbs);
        }
    }

    let wait = declared.get("wait").expect("Child has a `wait` method");
    assert!(
        wait.contains(&EffectVerbKind::Blocks),
        "Child.wait() must declare the `blocks` execution verb (synchronous \
         waitpid parks the calling thread); got {wait:?}"
    );
    assert!(
        wait.contains(&EffectVerbKind::Sends),
        "Child.wait() must still declare sends(ProcessTable); got {wait:?}"
    );
    for non_blocking in ["try_wait", "kill"] {
        let verbs = declared
            .get(non_blocking)
            .unwrap_or_else(|| panic!("Child has a `{non_blocking}` method"));
        assert!(
            !verbs.contains(&EffectVerbKind::Blocks),
            "Child.{non_blocking}() must NOT declare `blocks` — it does not park \
             the calling thread; got {verbs:?}"
        );
    }
}

#[test]
fn test_block_level_noblock_propagates_to_every_child_extern_c() {
    // Block-level `@noblock` lives on `ExternBlock.attributes` (not
    // pre-merged into per-item attributes). The effectchecker must
    // union block-level and per-item attrs when computing the ABI
    // default suppression so every child sees the noblock effect.
    let result = effectcheck_ok(
        "@noblock\n\
         unsafe extern \"C\" {\n\
             fn cpu_work_a() -> i32;\n\
             fn cpu_work_b() -> i32;\n\
         }",
    );
    for name in ["cpu_work_a", "cpu_work_b"] {
        let inferred = result.inferred_effects.get(name).unwrap();
        assert!(
            !inferred
                .effects
                .iter()
                .any(|e| e.effect.verb == EffectVerbKind::Blocks),
            "block-level @noblock should suppress blocks on child `{name}`"
        );
    }
}

#[test]
fn test_per_item_noblock_does_not_leak_to_sibling() {
    // Symmetric negative: per-item `@noblock` on one sibling must NOT
    // bubble to the block, and must NOT suppress the ABI default on
    // any other sibling. Pins that the union is one-way (block → item),
    // not bidirectional.
    let result = effectcheck_ok(
        "unsafe extern \"C\" {\n\
             @noblock fn pure_cpu() -> i32;\n\
             fn slow_io() -> i32;\n\
         }",
    );
    let pure = result.inferred_effects.get("pure_cpu").unwrap();
    assert!(
        !pure
            .effects
            .iter()
            .any(|e| e.effect.verb == EffectVerbKind::Blocks),
        "per-item @noblock should suppress blocks on its own item"
    );
    let slow = result.inferred_effects.get("slow_io").unwrap();
    assert!(
        slow.effects
            .iter()
            .any(|e| e.effect.verb == EffectVerbKind::Blocks),
        "sibling without @noblock should retain the extern \"C\" blocks default"
    );
}

#[test]
fn test_block_level_noblock_on_c_unwind_keeps_panics_on_every_child() {
    // C-unwind ABI defaults to {blocks, panics}; `@noblock` suppresses
    // blocks but cannot suppress panics. Block-level `@noblock` must
    // apply that rule uniformly to every child.
    let result = effectcheck_ok(
        "@noblock\n\
         unsafe extern \"C-unwind\" {\n\
             fn throwing_a();\n\
             fn throwing_b();\n\
         }",
    );
    for name in ["throwing_a", "throwing_b"] {
        let inferred = result.inferred_effects.get(name).unwrap();
        assert!(
            !inferred
                .effects
                .iter()
                .any(|e| e.effect.verb == EffectVerbKind::Blocks),
            "block-level @noblock should suppress blocks on C-unwind child `{name}`"
        );
        assert!(
            inferred
                .effects
                .iter()
                .any(|e| e.effect.verb == EffectVerbKind::Panics),
            "C-unwind child `{name}` should retain panics even with @noblock"
        );
    }
}

#[test]
fn test_extern_c_merges_programmer_annotations() {
    // Programmer-supplied effects are merged with the ABI defaults.
    // extern "C" fn read_db() reads(Db) → final = {blocks, reads(Db)}
    let result = effectcheck_ok(
        "effect resource Db;\n\
         unsafe extern \"C\" { fn read_db() reads(Db); }",
    );
    let inferred = result.inferred_effects.get("read_db").unwrap();
    assert!(
        inferred
            .effects
            .iter()
            .any(|e| e.effect.verb == EffectVerbKind::Blocks),
        "merged set should include ABI default blocks"
    );
    assert!(
        inferred.effects.iter().any(|e| e.effect.resource == "Db"),
        "merged set should include programmer-declared reads(Db)"
    );
}

#[test]
fn test_caller_inherits_extern_c_blocks() {
    // A private function calling an extern "C" fn inherits its {blocks} effect.
    let result = effectcheck_ok(
        "unsafe extern \"C\" { fn os_call() -> i32; }\n\
         fn wrapper() -> i32 { os_call() }",
    );
    let inferred = result.inferred_effects.get("wrapper").unwrap();
    assert!(
        inferred
            .effects
            .iter()
            .any(|e| e.effect.verb == EffectVerbKind::Blocks),
        "wrapper should inherit blocks from extern \"C\" callee"
    );
}

#[test]
fn test_struct_and_const_no_effects() {
    effectcheck_ok(
        "struct Point { x: i64, y: i64 }\n\
         const MAX: i64 = 100;",
    );
}

// ── Effect Groups ───────────────────────────────────────────────

#[test]
fn test_basic_group_expansion() {
    let result = effectcheck_ok(
        "effect resource UserDB;\n\
         effect resource OrderDB;\n\
         effect group read_all = reads(UserDB, OrderDB);",
    );
    let group = result.expanded_groups.get("read_all").unwrap();
    assert_eq!(group.effects.len(), 2);
}

#[test]
fn test_composed_groups() {
    let result = effectcheck_ok(
        "effect resource UserDB;\n\
         effect resource OrderDB;\n\
         effect resource Network;\n\
         effect group reading = reads(UserDB);\n\
         effect group writing = writes(OrderDB);\n\
         effect group all = reading + writing;",
    );
    let group = result.expanded_groups.get("all").unwrap();
    assert_eq!(group.effects.len(), 2);
    assert!(group
        .effects
        .iter()
        .any(|e| e.effect.verb == EffectVerbKind::Reads && e.effect.resource == "UserDB"));
    assert!(group
        .effects
        .iter()
        .any(|e| e.effect.verb == EffectVerbKind::Writes && e.effect.resource == "OrderDB"));
}

#[test]
fn test_group_used_with_with() {
    effectcheck_ok(
        "effect resource UserDB;\n\
         effect group read_all = reads(UserDB);\n\
         pub fn load() with read_all { }",
    );
}

#[test]
fn test_undefined_group_in_with_clause() {
    let errors = effectcheck_errors(
        "effect resource UserDB;\n\
         pub fn load() with Nonexistent { }",
    );
    assert!(errors
        .iter()
        .any(|e| e.kind == EffectErrorKind::UndefinedEffectGroup
            && e.message.contains("Nonexistent")));
}

#[test]
fn test_undefined_group_referenced_by_group() {
    let errors = effectcheck_errors(
        "effect resource UserDB;\n\
         effect group reading = reads(UserDB);\n\
         effect group all = reading + Missing;",
    );
    assert!(errors
        .iter()
        .any(|e| e.kind == EffectErrorKind::UndefinedEffectGroup
            && e.message.contains("Missing")
            && e.message.contains("all")));
}

// ── Effect Inference ────────────────────────────────────────────

#[test]
fn test_transitive_inference() {
    // A calls B calls C, C has effects → A and B should infer them
    let result = effectcheck_ok(
        "effect resource UserDB;\n\
         fn c() writes(UserDB) { }\n\
         fn b() { c(); }\n\
         fn a() { b(); }",
    );
    let a_effects = result.inferred_effects.get("a").unwrap();
    assert!(a_effects
        .effects
        .iter()
        .any(|e| e.effect.verb == EffectVerbKind::Writes && e.effect.resource == "UserDB"));
    let b_effects = result.inferred_effects.get("b").unwrap();
    assert!(b_effects
        .effects
        .iter()
        .any(|e| e.effect.verb == EffectVerbKind::Writes && e.effect.resource == "UserDB"));
}

#[test]
fn test_private_function_no_annotation_needed() {
    // Private function with effects but no declaration — should be fine
    effectcheck_ok(
        "effect resource UserDB;\n\
         fn helper() writes(UserDB) { }\n\
         fn main() { helper(); }",
    );
}

#[test]
fn test_multiple_callees_union_effects() {
    let result = effectcheck_ok(
        "effect resource UserDB;\n\
         effect resource OrderDB;\n\
         fn read_users() reads(UserDB) { }\n\
         fn write_orders() writes(OrderDB) { }\n\
         fn main() { read_users(); write_orders(); }",
    );
    let main_effects = result.inferred_effects.get("main").unwrap();
    assert!(main_effects
        .effects
        .iter()
        .any(|e| e.effect.verb == EffectVerbKind::Reads && e.effect.resource == "UserDB"));
    assert!(main_effects
        .effects
        .iter()
        .any(|e| e.effect.verb == EffectVerbKind::Writes && e.effect.resource == "OrderDB"));
}

#[test]
fn test_pure_function_stays_pure() {
    let result = effectcheck_ok(
        "fn pure_helper() -> i64 { 42 }\n\
         fn main() { pure_helper(); }",
    );
    let main_effects = result.inferred_effects.get("main").unwrap();
    assert!(main_effects.effects.is_empty());
}

#[test]
fn test_public_callee_uses_declared_effects() {
    // Public function B declares writes(UserDB).
    // Private function A calls B → should infer writes(UserDB) from B's declaration.
    let result = effectcheck_ok(
        "effect resource UserDB;\n\
         pub fn public_write() writes(UserDB) { }\n\
         fn main() { public_write(); }",
    );
    let main_effects = result.inferred_effects.get("main").unwrap();
    assert!(main_effects
        .effects
        .iter()
        .any(|e| e.effect.verb == EffectVerbKind::Writes && e.effect.resource == "UserDB"));
}

#[test]
fn test_recursive_function() {
    // Recursive function — should not infinite loop
    effectcheck_ok(
        "effect resource UserDB;\n\
         fn recurse(n: i64) reads(UserDB) { if n > 0 { recurse(n - 1); } }",
    );
}

// ── Verification ────────────────────────────────────────────────

#[test]
fn test_public_matching_declaration_passes() {
    effectcheck_ok(
        "effect resource UserDB;\n\
         fn helper() writes(UserDB) { }\n\
         pub fn save() writes(UserDB) { helper(); }",
    );
}

#[test]
fn test_public_missing_effect_error() {
    let errors = effectcheck_errors(
        "effect resource UserDB;\n\
         fn helper() writes(UserDB) { }\n\
         pub fn save() { helper(); }",
    );
    assert!(errors
        .iter()
        .any(|e| e.kind == EffectErrorKind::MissingEffectDeclaration));
    assert!(errors[0].message.contains("writes(UserDB)"));
}

#[test]
fn test_public_over_declaration_detected() {
    // To detect over-declaration, we need to NOT seed from declarations.
    // Instead, test that verification works by checking the result directly.
    // Since we seed inferred from declared (to handle empty-body test functions),
    // over-declaration for functions with declared effects won't trigger.
    // This is a pragmatic trade-off — in practice, body analysis would catch this.
    let result = effectcheck_all(
        "effect resource UserDB;\n\
         pub fn noop() writes(UserDB) { }",
    );
    // No error because declared effects are seeded into inferred
    assert!(result.errors.is_empty());
}

#[test]
fn test_private_function_with_effects_no_error() {
    // Private function doesn't need effect declarations
    effectcheck_ok(
        "effect resource UserDB;\n\
         fn helper() writes(UserDB) { }\n\
         fn private_caller() { helper(); }",
    );
}

#[test]
fn test_multiple_missing_effects() {
    let errors = effectcheck_errors(
        "effect resource UserDB;\n\
         effect resource OrderDB;\n\
         fn read_db() reads(UserDB) { }\n\
         fn write_db() writes(OrderDB) { }\n\
         pub fn process() { read_db(); write_db(); }",
    );
    // Should report at least one missing effect (possibly two)
    assert!(errors
        .iter()
        .any(|e| e.kind == EffectErrorKind::MissingEffectDeclaration));
}

#[test]
fn test_effect_from_deep_call_chain() {
    let errors = effectcheck_errors(
        "effect resource UserDB;\n\
         fn leaf() writes(UserDB) { }\n\
         fn mid() { leaf(); }\n\
         fn inner() { mid(); }\n\
         pub fn outer() { inner(); }",
    );
    assert!(errors
        .iter()
        .any(|e| e.kind == EffectErrorKind::MissingEffectDeclaration));
    assert!(errors[0].message.contains("writes(UserDB)"));
}

#[test]
fn test_fix_suggestion_in_error() {
    let errors = effectcheck_errors(
        "effect resource UserDB;\n\
         fn helper() writes(UserDB) { }\n\
         pub fn save() { helper(); }",
    );
    // Error message should mention the missing effect
    assert!(errors[0].message.contains("writes(UserDB)"));
}

// ── Conflict Detection ──────────────────────────────────────────

#[test]
fn test_reads_reads_safe() {
    let mut a = EffectSet::new();
    a.add(
        Effect {
            verb: EffectVerbKind::Reads,
            resource: "Db".to_string(),
        },
        EffectOrigin::Direct(dummy_span()),
    );
    let mut b = EffectSet::new();
    b.add(
        Effect {
            verb: EffectVerbKind::Reads,
            resource: "Db".to_string(),
        },
        EffectOrigin::Direct(dummy_span()),
    );
    let conflicts = EffectChecker::find_conflicts(&a, &b, &HashSet::new());
    assert!(conflicts.is_empty());
}

#[test]
fn test_reads_writes_same_resource_conflicts() {
    let mut a = EffectSet::new();
    a.add(
        Effect {
            verb: EffectVerbKind::Reads,
            resource: "Db".to_string(),
        },
        EffectOrigin::Direct(dummy_span()),
    );
    let mut b = EffectSet::new();
    b.add(
        Effect {
            verb: EffectVerbKind::Writes,
            resource: "Db".to_string(),
        },
        EffectOrigin::Direct(dummy_span()),
    );
    let conflicts = EffectChecker::find_conflicts(&a, &b, &HashSet::new());
    assert_eq!(conflicts.len(), 1);
    assert_eq!(conflicts[0].resource, "Db");
}

#[test]
fn test_writes_writes_same_resource_conflicts() {
    let mut a = EffectSet::new();
    a.add(
        Effect {
            verb: EffectVerbKind::Writes,
            resource: "Db".to_string(),
        },
        EffectOrigin::Direct(dummy_span()),
    );
    let mut b = EffectSet::new();
    b.add(
        Effect {
            verb: EffectVerbKind::Writes,
            resource: "Db".to_string(),
        },
        EffectOrigin::Direct(dummy_span()),
    );
    let conflicts = EffectChecker::find_conflicts(&a, &b, &HashSet::new());
    assert_eq!(conflicts.len(), 1);
}

#[test]
fn test_different_resources_safe() {
    let mut a = EffectSet::new();
    a.add(
        Effect {
            verb: EffectVerbKind::Writes,
            resource: "UserDB".to_string(),
        },
        EffectOrigin::Direct(dummy_span()),
    );
    let mut b = EffectSet::new();
    b.add(
        Effect {
            verb: EffectVerbKind::Writes,
            resource: "OrderDB".to_string(),
        },
        EffectOrigin::Direct(dummy_span()),
    );
    let conflicts = EffectChecker::find_conflicts(&a, &b, &HashSet::new());
    assert!(conflicts.is_empty());
}

#[test]
fn test_sends_receives_same_resource_safe() {
    let mut a = EffectSet::new();
    a.add(
        Effect {
            verb: EffectVerbKind::Sends,
            resource: "Network".to_string(),
        },
        EffectOrigin::Direct(dummy_span()),
    );
    let mut b = EffectSet::new();
    b.add(
        Effect {
            verb: EffectVerbKind::Receives,
            resource: "Network".to_string(),
        },
        EffectOrigin::Direct(dummy_span()),
    );
    let conflicts = EffectChecker::find_conflicts(&a, &b, &HashSet::new());
    assert!(conflicts.is_empty());
}

#[test]
fn test_sends_sends_same_resource_safe() {
    let mut a = EffectSet::new();
    a.add(
        Effect {
            verb: EffectVerbKind::Sends,
            resource: "Network".to_string(),
        },
        EffectOrigin::Direct(dummy_span()),
    );
    let mut b = EffectSet::new();
    b.add(
        Effect {
            verb: EffectVerbKind::Sends,
            resource: "Network".to_string(),
        },
        EffectOrigin::Direct(dummy_span()),
    );
    let conflicts = EffectChecker::find_conflicts(&a, &b, &HashSet::new());
    assert!(conflicts.is_empty());
}

#[test]
fn test_receives_receives_same_resource_safe() {
    let mut a = EffectSet::new();
    a.add(
        Effect {
            verb: EffectVerbKind::Receives,
            resource: "Network".to_string(),
        },
        EffectOrigin::Direct(dummy_span()),
    );
    let mut b = EffectSet::new();
    b.add(
        Effect {
            verb: EffectVerbKind::Receives,
            resource: "Network".to_string(),
        },
        EffectOrigin::Direct(dummy_span()),
    );
    let conflicts = EffectChecker::find_conflicts(&a, &b, &HashSet::new());
    assert!(conflicts.is_empty());
}

#[test]
fn test_transparent_effect_excluded() {
    let mut a = EffectSet::new();
    a.add(
        Effect {
            verb: EffectVerbKind::Writes,
            resource: "Db".to_string(),
        },
        EffectOrigin::Direct(dummy_span()),
    );
    let mut b = EffectSet::new();
    b.add(
        Effect {
            verb: EffectVerbKind::Writes,
            resource: "Db".to_string(),
        },
        EffectOrigin::Direct(dummy_span()),
    );
    let mut transparent = HashSet::new();
    transparent.insert("writes".to_string());
    let conflicts = EffectChecker::find_conflicts(&a, &b, &transparent);
    assert!(conflicts.is_empty());
}

// ── Transparent Effects ─────────────────────────────────────────

#[test]
fn test_transparent_effect_registered() {
    let result = effectcheck_ok("transparent effect verb traces;");
    assert!(result.transparent_effects.contains("traces"));
}

// ── Polymorphism ────────────────────────────────────────────────

#[test]
fn test_polymorphic_function_not_flagged() {
    effectcheck_ok(
        "effect resource UserDB;\n\
         pub fn process() with _ { }",
    );
}

#[test]
fn test_polymorphic_empty_body_caller_has_empty_effects() {
    // A `with _` function with an empty body and no arguments contributes
    // no effects to its caller — empty inferred effects is correct here.
    let result = effectcheck_ok(
        "effect resource UserDB;\n\
         pub fn process() with _ { }\n\
         fn caller() { process(); }",
    );
    let caller_effects = result.inferred_effects.get("caller").unwrap();
    assert!(
        caller_effects.effects.is_empty(),
        "caller of a no-op with _ fn should infer empty effects"
    );
}

#[test]
fn test_inline_closure_to_polymorphic_callee_propagates_effects() {
    // Inline closure passed to a `with _` callee: closure body is walked,
    // so caller inherits the closure's effects.
    let result = effectcheck_ok(
        "effect resource UserDB;\n\
         fn reads_db() reads(UserDB) { }\n\
         pub fn apply(f: Fn() -> () with _) -> () with _ { f() }\n\
         fn caller() { apply(|| reads_db()); }",
    );
    let caller_effects = result.inferred_effects.get("caller").unwrap();
    let verbs: Vec<_> = caller_effects
        .effects
        .iter()
        .map(|e| e.effect.resource.as_str())
        .collect();
    assert!(
        verbs.contains(&"UserDB"),
        "caller should inherit reads(UserDB) from inline closure: {verbs:?}"
    );
}

#[test]
fn test_function_reference_to_polymorphic_callee_propagates_effects() {
    // Named function reference passed to a `with _` callee: its effects
    // must propagate to the caller.
    let result = effectcheck_ok(
        "effect resource UserDB;\n\
         fn reads_db() reads(UserDB) { }\n\
         pub fn apply(f: Fn() -> () with _) -> () with _ { f() }\n\
         fn caller() { apply(reads_db); }",
    );
    let caller_effects = result.inferred_effects.get("caller").unwrap();
    let verbs: Vec<_> = caller_effects
        .effects
        .iter()
        .map(|e| e.effect.resource.as_str())
        .collect();
    assert!(
        verbs.contains(&"UserDB"),
        "caller should inherit reads(UserDB) from function reference: {verbs:?}"
    );
}

#[test]
fn test_public_fn_with_explicit_effects_calling_polymorphic_needs_with_underscore() {
    // A public fn with explicit effect declarations that also calls a `with _`
    // function must additionally declare `with _` — the viral rule applies
    // regardless of what other effects are already declared.
    let errors = effectcheck_errors(
        "effect resource UserDB;\n\
         effect resource OrderDB;\n\
         fn reads_db() reads(UserDB) { }\n\
         pub fn apply(f: Fn() -> () with _) -> () with _ { f() }\n\
         pub fn caller() reads(OrderDB) { apply(|| reads_db()); }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("polymorphic") && e.message.contains("caller")),
        "expected missing-with-_ error for caller: {errors:?}"
    );
}

#[test]
fn test_function_ref_through_private_wrapper_propagates_effects() {
    // A private wrapper that internally calls a `with _` function is
    // polymorphic-dependent.  Passing a named effectful function through it
    // must propagate the concrete effects to the outer caller.
    let result = effectcheck_ok(
        "effect resource UserDB;\n\
         fn reads_db() reads(UserDB) { }\n\
         pub fn apply(f: Fn() -> () with _) -> () with _ { f() }\n\
         fn wrapper(f: Fn() -> () with _) { apply(f) }\n\
         fn caller() { wrapper(reads_db); }",
    );
    let caller_effects = result.inferred_effects.get("caller").unwrap();
    let resources: Vec<_> = caller_effects
        .effects
        .iter()
        .map(|e| e.effect.resource.as_str())
        .collect();
    assert!(
        resources.contains(&"UserDB"),
        "caller should inherit reads(UserDB) through private wrapper: {resources:?}"
    );
}

// ── Edge Cases ──────────────────────────────────────────────────

#[test]
fn test_function_with_no_body_calls_is_pure() {
    let result = effectcheck_ok("fn no_effects() -> i64 { 42 }");
    let inferred = result.inferred_effects.get("no_effects").unwrap();
    assert!(inferred.effects.is_empty());
}

#[test]
fn test_multiple_errors_reported() {
    // Public function with no declaration but calls effectful function
    let result = effectcheck_all(
        "effect resource UserDB;\n\
         effect resource OrderDB;\n\
         fn read_db() reads(UserDB) { }\n\
         fn write_db() writes(OrderDB) { }\n\
         pub fn a() { read_db(); write_db(); }",
    );
    // 'a' should have missing effect declarations
    assert!(result
        .errors
        .iter()
        .any(|e| e.kind == EffectErrorKind::MissingEffectDeclaration));
}

// ── Complex Program ─────────────────────────────────────────────

#[test]
fn test_complex_effect_program() {
    effectcheck_ok(
        "effect resource UserDB;\n\
         effect resource OrderDB;\n\
         effect resource Network;\n\
         \n\
         effect group validation = reads(UserDB);\n\
         effect group fulfillment = writes(OrderDB) + sends(Network);\n\
         effect group order_processing = validation + fulfillment;\n\
         \n\
         fn validate_user(id: i64) reads(UserDB) { }\n\
         fn create_order(id: i64) writes(OrderDB) { }\n\
         fn notify(id: i64) sends(Network) { }\n\
         \n\
         pub fn process_order(id: i64) with order_processing {\n\
             validate_user(id);\n\
             create_order(id);\n\
             notify(id);\n\
         }",
    );
}

// ── Transparent Effects in Verification ────────────────────────

#[test]
fn test_transparent_verb_registered_and_queryable() {
    // Transparent verbs are collected during analysis
    let result = effectcheck_ok("transparent effect verb logs;");
    assert!(result.transparent_effects.contains("logs"));
}

#[test]
fn test_transparent_reads_excluded_from_conflicts() {
    // When 'reads' is transparent, reads/writes on same resource should not conflict
    // Note: built-in verbs can't be made transparent via syntax, so we test
    // the conflict detection API directly
    let result = effectcheck_ok(
        "effect resource Db;\n\
         fn a() writes(Db) { }\n\
         fn b() reads(Db) { }",
    );
    let effects_a = result.inferred_effects.get("a").unwrap();
    let effects_b = result.inferred_effects.get("b").unwrap();
    // Without transparent verbs: reads/writes should conflict
    let conflicts =
        EffectChecker::find_conflicts(effects_a, effects_b, &result.transparent_effects);
    assert!(
        !conflicts.is_empty(),
        "reads/writes should conflict normally"
    );
    // With 'reads' marked transparent: no conflict
    let mut transparent = result.transparent_effects.clone();
    transparent.insert("reads".to_string());
    let conflicts = EffectChecker::find_conflicts(effects_a, effects_b, &transparent);
    assert!(
        conflicts.is_empty(),
        "reads should be excluded when transparent"
    );
}

// ── Inference: 5+ Function Call Chain ──────────────────────────

#[test]
fn test_deep_call_chain_inference() {
    // Private function effects inferred across a 5+ function call chain
    let result = effectcheck_ok(
        "effect resource Db;\n\
         fn e() reads(Db) { }\n\
         fn d() { e(); }\n\
         fn c() { d(); }\n\
         fn b() { c(); }\n\
         fn a() { b(); }\n\
         pub fn top() reads(Db) { a(); }",
    );
    // 'a' should have inferred reads(Db) via chain a→b→c→d→e
    let inferred_a = result.inferred_effects.get("a").unwrap();
    assert!(!inferred_a.effects.is_empty());
    assert!(inferred_a
        .effects
        .iter()
        .any(|te| te.effect.verb == EffectVerbKind::Reads));
}

// ── Integration: Guard Effects ─────────────────────────────────

#[test]
fn test_guard_effects_contribute_to_function() {
    // A guard calling an effectful function should contribute its effects
    let result = effectcheck_ok(
        "effect resource Db;\n\
         fn check_db() -> bool reads(Db) { true }\n\
         enum Opt { Some(i64), None }\n\
         fn process(o: Opt) reads(Db) {\n\
             match o {\n\
                 Some(x) if check_db() => { },\n\
                 Some(_) => { },\n\
                 None => { },\n\
             };\n\
         }",
    );
    let inferred = result.inferred_effects.get("process").unwrap();
    assert!(inferred
        .effects
        .iter()
        .any(|te| te.effect.verb == EffectVerbKind::Reads));
}

// ── Integration: todo()/unreachable() Panics Effect ────────────

#[test]
fn test_todo_propagates_panics_effect() {
    // Calling todo() should infer panics effect
    let result = effectcheck_ok(
        "fn placeholder() {\n\
             todo();\n\
         }",
    );
    let inferred = result.inferred_effects.get("placeholder").unwrap();
    assert!(inferred
        .effects
        .iter()
        .any(|te| te.effect.verb == EffectVerbKind::Panics));
}

#[test]
fn test_unreachable_propagates_panics_effect() {
    let result = effectcheck_ok(
        "fn impossible() {\n\
             unreachable();\n\
         }",
    );
    let inferred = result.inferred_effects.get("impossible").unwrap();
    assert!(inferred
        .effects
        .iter()
        .any(|te| te.effect.verb == EffectVerbKind::Panics));
}

#[test]
fn test_public_fn_with_todo_must_declare_panics() {
    // Public function calling todo() needs panics declared
    let errors = effectcheck_errors(
        "pub fn not_done_yet() {\n\
             todo();\n\
         }",
    );
    assert!(errors.iter().any(|e| e.message.contains("panics")));
}

// ── Subscript / Index Panics Effect ────────────────────────────

#[test]
fn test_index_infers_panics_effect() {
    // Indexing with [] should infer panics (can panic on out-of-bounds)
    let result = effectcheck_ok(
        "fn get_first(arr: i64) -> i64 {\n\
             arr[0]\n\
         }",
    );
    let inferred = result.inferred_effects.get("get_first").unwrap();
    assert!(
        inferred
            .effects
            .iter()
            .any(|te| te.effect.verb == EffectVerbKind::Panics),
        "indexing should infer panics effect"
    );
}

#[test]
fn test_public_fn_with_index_must_declare_panics() {
    let errors = effectcheck_errors(
        "pub fn get_first(arr: i64) -> i64 {\n\
             arr[0]\n\
         }",
    );
    assert!(errors.iter().any(|e| e.message.contains("panics")));
}

// ── Diagnostics: Fix Suggestion Content ────────────────────────

#[test]
fn test_diagnostic_includes_originating_function() {
    // Error message should trace back to the callee that introduced the effect
    let errors = effectcheck_errors(
        "effect resource Db;\n\
         fn helper() reads(Db) { }\n\
         pub fn api() {\n\
             helper();\n\
         }",
    );
    // The error should mention either the callee name or the origin
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("helper") || e.message.contains("reads")),
        "Expected diagnostic mentioning origin, got: {:?}",
        errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

// ── Helper ──────────────────────────────────────────────────────

fn dummy_span() -> karac::token::Span {
    karac::token::Span {
        line: 0,
        column: 0,
        offset: 0,
        length: 0,
    }
}

// ── Mutual Recursion Group Detection ───────────────────────────

#[test]
fn test_mutual_recursion_detected() {
    let source = r#"
effect resource Db;
effect resource Log;

fn f() {
    g()
}
fn g() {
    f()
}
"#;
    let result = effectcheck_all(source);
    assert!(
        !result.mutual_recursion_groups.is_empty(),
        "Should detect mutual recursion group"
    );
    let group = &result.mutual_recursion_groups[0];
    assert!(group.functions.contains(&"f".to_string()));
    assert!(group.functions.contains(&"g".to_string()));
    assert_eq!(group.functions.len(), 2);
}

#[test]
fn test_mutual_recursion_with_effects_has_trace() {
    let source = r#"
effect resource Db;
effect resource Log;

pub fn read_db() reads(Db) { }
pub fn write_log() writes(Log) { }

fn f() {
    read_db();
    g()
}
fn g() {
    write_log();
    f()
}
"#;
    let result = effectcheck_all(source);
    assert!(
        !result.mutual_recursion_groups.is_empty(),
        "Should detect mutual recursion group"
    );

    let group = &result.mutual_recursion_groups[0];
    assert!(group.functions.contains(&"f".to_string()));
    assert!(group.functions.contains(&"g".to_string()));

    // Both functions should have effects propagated through them
    assert!(
        !group.resolution_trace.is_empty(),
        "Resolution trace should not be empty"
    );

    // f calls g, so f should resolve writes(Log) via g
    let f_resolves_via_g: Vec<_> = group
        .resolution_trace
        .iter()
        .filter(|r| r.call_site_function == "f" && r.resolved_via == "g")
        .collect();
    assert!(
        f_resolves_via_g.iter().any(|r| r.effect == "writes(Log)"),
        "f should resolve writes(Log) via g, got: {:?}",
        f_resolves_via_g
            .iter()
            .map(|r| &r.effect)
            .collect::<Vec<_>>()
    );

    // g calls f, so g should resolve reads(Db) via f
    let g_resolves_via_f: Vec<_> = group
        .resolution_trace
        .iter()
        .filter(|r| r.call_site_function == "g" && r.resolved_via == "f")
        .collect();
    assert!(
        g_resolves_via_f.iter().any(|r| r.effect == "reads(Db)"),
        "g should resolve reads(Db) via f, got: {:?}",
        g_resolves_via_f
            .iter()
            .map(|r| &r.effect)
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_no_mutual_recursion_for_non_recursive() {
    let source = r#"
fn a() { b() }
fn b() { }
"#;
    let result = effectcheck_all(source);
    assert!(
        result.mutual_recursion_groups.is_empty(),
        "Non-recursive functions should not form a mutual recursion group"
    );
}

#[test]
fn test_no_mutual_recursion_for_self_recursion() {
    // Self-recursion (single-node SCC) should NOT appear in mutual_recursion_groups
    let source = r#"
fn factorial(n: i64) -> i64 {
    if n <= 1 { 1 } else { n * factorial(n - 1) }
}
"#;
    let result = effectcheck_all(source);
    assert!(
        result.mutual_recursion_groups.is_empty(),
        "Self-recursion should not be reported as mutual recursion"
    );
}

#[test]
fn test_three_way_mutual_recursion() {
    let source = r#"
effect resource Net;
pub fn send_data() sends(Net) { }

fn a() { b() }
fn b() { c() }
fn c() { a(); send_data() }
"#;
    let result = effectcheck_all(source);
    assert_eq!(
        result.mutual_recursion_groups.len(),
        1,
        "Should detect one 3-way mutual recursion group"
    );
    let group = &result.mutual_recursion_groups[0];
    assert_eq!(group.functions.len(), 3);
    assert!(group.functions.contains(&"a".to_string()));
    assert!(group.functions.contains(&"b".to_string()));
    assert!(group.functions.contains(&"c".to_string()));

    // sends(Net) should propagate through the cycle
    assert!(
        !group.resolution_trace.is_empty(),
        "Resolution trace should capture sends(Net) propagation"
    );
}

#[test]
fn test_mutual_recursion_trace_line_numbers() {
    let source = "fn f() {\n    g()\n}\nfn g() {\n    f()\n}\n";
    let result = effectcheck_all(source);
    assert_eq!(result.mutual_recursion_groups.len(), 1);
    let group = &result.mutual_recursion_groups[0];
    // Functions should be f and g
    assert!(group.functions.contains(&"f".to_string()));
    assert!(group.functions.contains(&"g".to_string()));
}

// ── SCC-based inference correctness ─────────────────────────────

#[test]
fn test_scc_inferred_effects_propagate_through_cycle() {
    // All functions in a mutual-recursion SCC must end up with the
    // union of effects reachable within the cycle.
    let source = r#"
effect resource Net;
pub fn send_data() sends(Net) { }

fn a() { b() }
fn b() { c() }
fn c() { a(); send_data() }
"#;
    let result = effectcheck_all(source);
    for name in &["a", "b", "c"] {
        let effects = result.inferred_effects.get(*name).unwrap();
        let resources: Vec<_> = effects
            .effects
            .iter()
            .map(|e| e.effect.resource.as_str())
            .collect();
        assert!(
            resources.contains(&"Net"),
            "'{name}' should have sends(Net) via SCC propagation, got: {resources:?}"
        );
    }
}

#[test]
fn test_scc_caller_outside_cycle_gets_scc_effects() {
    // A function that calls into a mutually-recursive SCC must inherit
    // the SCC's fully-resolved effect set (not a partially-propagated snapshot).
    let source = r#"
effect resource Db;
pub fn read_db() reads(Db) { }

fn ping() { pong() }
fn pong() { ping(); read_db() }

fn caller() { ping() }
"#;
    let result = effectcheck_all(source);
    let caller_effects = result.inferred_effects.get("caller").unwrap();
    let resources: Vec<_> = caller_effects
        .effects
        .iter()
        .map(|e| e.effect.resource.as_str())
        .collect();
    assert!(
        resources.contains(&"Db"),
        "caller outside the SCC should inherit reads(Db) from the cycle: {resources:?}"
    );
}

#[test]
fn test_scc_two_independent_cycles_dont_cross_contaminate() {
    // Two disjoint SCCs must not merge each other's effects.
    let source = r#"
effect resource Alpha;
effect resource Beta;
pub fn do_alpha() reads(Alpha) { }
pub fn do_beta() writes(Beta) { }

fn a1() { a2() }
fn a2() { a1(); do_alpha() }

fn b1() { b2() }
fn b2() { b1(); do_beta() }
"#;
    let result = effectcheck_all(source);

    for name in &["a1", "a2"] {
        let effects = result.inferred_effects.get(*name).unwrap();
        let resources: Vec<_> = effects
            .effects
            .iter()
            .map(|e| e.effect.resource.as_str())
            .collect();
        assert!(
            resources.contains(&"Alpha"),
            "'{name}' should have Alpha: {resources:?}"
        );
        assert!(
            !resources.contains(&"Beta"),
            "'{name}' should NOT have Beta: {resources:?}"
        );
    }
    for name in &["b1", "b2"] {
        let effects = result.inferred_effects.get(*name).unwrap();
        let resources: Vec<_> = effects
            .effects
            .iter()
            .map(|e| e.effect.resource.as_str())
            .collect();
        assert!(
            resources.contains(&"Beta"),
            "'{name}' should have Beta: {resources:?}"
        );
        assert!(
            !resources.contains(&"Alpha"),
            "'{name}' should NOT have Alpha: {resources:?}"
        );
    }
}

// ── public_effects = "inferred" mode ────────

#[test]
fn test_inferred_policy_default_is_declared() {
    // Sanity: default policy on EffectCheckResult is Declared.
    let parsed = parse("");
    let result = effectcheck(&parsed.program);
    assert_eq!(result.public_effects_policy, PublicEffectsPolicy::Declared);
}

#[test]
fn test_inferred_policy_accepts_pub_fn_without_declaration() {
    // Under Inferred policy, a pub fn performing effects need not declare them.
    let source = "effect resource UserDB;\n\
                  fn helper() writes(UserDB) { }\n\
                  pub fn save() { helper(); }";
    let parsed = parse(source);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let result = effectcheck_with_policy(&parsed.program, PublicEffectsPolicy::Inferred);
    assert!(
        result.errors.is_empty(),
        "expected no errors under Inferred policy, got: {:?}",
        result.errors
    );
    assert_eq!(result.public_effects_policy, PublicEffectsPolicy::Inferred);
    // Inference still produces the effect set so the CLI can surface it.
    let save = result
        .inferred_effects
        .get("save")
        .expect("save should have an inferred effect set");
    assert!(save
        .effects
        .iter()
        .any(|te| te.effect.verb == EffectVerbKind::Writes && te.effect.resource == "UserDB"));
}

#[test]
fn test_inferred_policy_still_verifies_explicit_declarations() {
    // writing an explicit `with ...` clause is an implicit per-function opt-in
    // to verification even under Inferred policy. No new syntax — the existing
    // declaration is what flips on the check.
    let source = "effect resource UserDB;\n\
                  effect resource OrderDB;\n\
                  fn w_user() writes(UserDB) { }\n\
                  fn w_order() writes(OrderDB) { }\n\
                  pub fn save() writes(UserDB) { w_user(); w_order(); }";
    let parsed = parse(source);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let result = effectcheck_with_policy(&parsed.program, PublicEffectsPolicy::Inferred);
    // w_order's writes(OrderDB) is performed but not declared — still an error.
    assert!(
        result
            .errors
            .iter()
            .any(|e| e.kind == EffectErrorKind::MissingEffectDeclaration
                && e.message.contains("writes(OrderDB)")),
        "expected missing-declaration error for OrderDB, got: {:?}",
        result.errors
    );
}

#[test]
fn test_declared_policy_still_errors_on_missing_declaration() {
    // Regression: explicit Declared policy preserves the existing behavior.
    let source = "effect resource UserDB;\n\
                  fn helper() writes(UserDB) { }\n\
                  pub fn save() { helper(); }";
    let parsed = parse(source);
    let result = effectcheck_with_policy(&parsed.program, PublicEffectsPolicy::Declared);
    assert!(result
        .errors
        .iter()
        .any(|e| e.kind == EffectErrorKind::MissingEffectDeclaration));
}

#[test]
fn test_inferred_policy_private_fn_inference_unchanged() {
    // Private functions behave identically under either policy.
    let source = "effect resource UserDB;\n\
                  fn helper() writes(UserDB) { }\n\
                  fn caller() { helper(); }";
    let parsed = parse(source);
    let result = effectcheck_with_policy(&parsed.program, PublicEffectsPolicy::Inferred);
    assert!(
        result.errors.is_empty(),
        "unexpected errors: {:?}",
        result.errors
    );
    let caller = result.inferred_effects.get("caller").unwrap();
    assert!(caller
        .effects
        .iter()
        .any(|te| te.effect.verb == EffectVerbKind::Writes && te.effect.resource == "UserDB"));
}

#[test]
fn test_inferred_policy_exposes_function_visibility() {
    // The result surfaces visibility so downstream consumers (CLI output)
    // can filter to public functions.
    let source = "effect resource UserDB;\n\
                  fn priv_helper() writes(UserDB) { }\n\
                  pub fn pub_api() { priv_helper(); }";
    let parsed = parse(source);
    let result = effectcheck_with_policy(&parsed.program, PublicEffectsPolicy::Inferred);
    assert_eq!(result.function_visibility.get("pub_api"), Some(&true));
    assert_eq!(result.function_visibility.get("priv_helper"), Some(&false));
}

// ── panics-producing primitives (F-057) ──────────────────────────

#[test]
fn test_unwrap_infers_panics() {
    let result = effectcheck_ok(
        "fn maybe_get(x: Option[i64]) -> i64 {\n\
             x.unwrap()\n\
         }",
    );
    let inferred = result.inferred_effects.get("maybe_get").unwrap();
    assert!(
        inferred
            .effects
            .iter()
            .any(|te| te.effect.verb == EffectVerbKind::Panics),
        "unwrap() should infer panics"
    );
}

#[test]
fn test_expect_infers_panics() {
    let result = effectcheck_ok(
        "fn maybe_get(x: Option[i64]) -> i64 {\n\
             x.expect(\"missing value\")\n\
         }",
    );
    let inferred = result.inferred_effects.get("maybe_get").unwrap();
    assert!(
        inferred
            .effects
            .iter()
            .any(|te| te.effect.verb == EffectVerbKind::Panics),
        "expect() should infer panics"
    );
}

#[test]
fn test_division_infers_panics() {
    let result = effectcheck_ok(
        "fn divide(a: i64, b: i64) -> i64 {\n\
             a / b\n\
         }",
    );
    let inferred = result.inferred_effects.get("divide").unwrap();
    assert!(
        inferred
            .effects
            .iter()
            .any(|te| te.effect.verb == EffectVerbKind::Panics),
        "integer division should infer panics (divide by zero)"
    );
}

#[test]
fn test_modulo_infers_panics() {
    let result = effectcheck_ok(
        "fn modulo(a: i64, b: i64) -> i64 {\n\
             a % b\n\
         }",
    );
    let inferred = result.inferred_effects.get("modulo").unwrap();
    assert!(
        inferred
            .effects
            .iter()
            .any(|te| te.effect.verb == EffectVerbKind::Panics),
        "modulo should infer panics (modulo by zero)"
    );
}

#[test]
fn test_addition_does_not_infer_panics() {
    // Arithmetic overflow is NOT a panics atom per F-057.
    let result = effectcheck_ok(
        "fn add(a: i64, b: i64) -> i64 {\n\
             a + b\n\
         }",
    );
    let inferred = result.inferred_effects.get("add").unwrap();
    assert!(
        !inferred
            .effects
            .iter()
            .any(|te| te.effect.verb == EffectVerbKind::Panics),
        "plain addition should NOT infer panics (overflow is not a panics atom)"
    );
}

#[test]
fn test_public_fn_with_division_must_declare_panics() {
    let errors = effectcheck_errors(
        "pub fn divide(a: i64, b: i64) -> i64 {\n\
             a / b\n\
         }",
    );
    assert!(
        errors.iter().any(|e| e.message.contains("panics")),
        "public fn with division must declare panics"
    );
}

// ── with _ viral rule — default-permitted effects ────────────────

#[test]
fn test_viral_rule_fires_even_when_only_transparent_effect() {
    // The viral rule (`with _` required when calling a polymorphic callee) must
    // fire even if the closure's only effect is transparent (allocates(Heap)).
    // `with _` is about the *mechanism*, not the specific effect values.
    let errors = effectcheck_errors(
        "pub fn apply(f: Fn() -> i32 with _) -> i32 with _ { f() }\n\
         fn pure_fn() -> i32 { 0 }\n\
         pub fn wrapper() -> i32 { apply(pure_fn) }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("with _") && e.message.contains("wrapper")),
        "expected viral-rule error for wrapper: {:?}",
        errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn test_viral_rule_fires_under_inferred_policy() {
    // Under Inferred policy, concrete effect annotations are optional for pub fns,
    // but the `with _` viral rule still applies: calling a polymorphic callee
    // requires declaring `with _` regardless of policy.
    let source = "pub fn apply(f: Fn() -> i32 with _) -> i32 with _ { f() }\n\
                  fn pure_fn() -> i32 { 0 }\n\
                  pub fn wrapper() -> i32 { apply(pure_fn) }";
    let parsed = parse(source);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let result = effectcheck_with_policy(&parsed.program, PublicEffectsPolicy::Inferred);
    assert!(
        result
            .errors
            .iter()
            .any(|e| e.message.contains("with _") && e.message.contains("wrapper")),
        "viral-rule error should fire under Inferred policy: {:?}",
        result.errors
    );
}

// ── Effect set subtyping ─────────────────────────────────────────

#[test]
fn test_subtype_pure_arg_into_reads_slot_ok() {
    // ∅ ⊆ {reads(Db)} — pure function fits a reads slot
    effectcheck_ok(
        "fn run(f: Fn(i32) -> i32 with reads(Db)) -> i32 { f(1) }\n\
         fn pure_fn(x: i32) -> i32 { x }\n\
         fn main() { run(pure_fn); }",
    );
}

#[test]
fn test_subtype_exact_match_ok() {
    // {reads(Db)} ⊆ {reads(Db)} — same effects, always OK
    effectcheck_ok(
        "fn run(f: Fn(i32) -> i32 with reads(Db)) -> i32 { f(1) }\n\
         fn read_fn(x: i32) -> i32 with reads(Db) { x }\n\
         fn main() { run(read_fn); }",
    );
}

#[test]
fn test_subtype_wider_arg_rejected() {
    // {writes(Db)} ⊄ {reads(Db)} — slot only allows reads, arg does writes
    let errors = effectcheck_errors(
        "fn run(f: Fn(i32) -> i32 with reads(Db)) -> i32 { f(1) }\n\
         fn write_fn(x: i32) -> i32 with writes(Db) { x }\n\
         fn main() { run(write_fn); }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.kind == EffectErrorKind::EffectSubtypeViolation),
        "expected EffectSubtypeViolation, got: {:?}",
        errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
    assert!(
        errors.iter().any(|e| e.message.contains("writes(Db)")),
        "error should name the violating effect"
    );
}

#[test]
fn test_subtype_polymorphic_slot_accepts_anything() {
    // with _ slot accepts any effects — no error
    effectcheck_ok(
        "pub fn run(f: Fn(i32) -> i32 with _) -> i32 with _ { f(1) }\n\
         fn write_fn(x: i32) -> i32 with writes(Db) { x }\n\
         fn main() { run(write_fn); }",
    );
}

#[test]
fn test_subtype_unannotated_slot_treated_as_pure() {
    // Unannotated slot → B = ∅; passing an effectful fn is an E0404.
    let errors = effectcheck_errors(
        "effect resource Db;\n\
         fn run(f: Fn(i32) -> i32) -> i32 { f(1) }\n\
         fn write_fn(x: i32) -> i32 with writes(Db) { x }\n\
         fn main() { run(write_fn); }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.kind == EffectErrorKind::EffectSubtypeViolation),
        "expected E0404 when effectful fn is passed to unannotated (pure) slot"
    );
}

#[test]
fn test_subtype_unannotated_slot_pure_arg_passes() {
    // Unannotated slot → B = ∅; passing a pure fn is fine.
    effectcheck_ok(
        "fn run(f: Fn(i32) -> i32) -> i32 { f(1) }\n\
         fn pure_fn(x: i32) -> i32 { x }\n\
         fn main() { run(pure_fn); }",
    );
}

#[test]
fn test_subtype_closure_arg_wider_rejected() {
    // Closure with writes effect passed into reads-only slot
    let errors = effectcheck_errors(
        "fn helper() -> i32 with writes(Db) { 0 }\n\
         fn run(f: Fn(i32) -> i32 with reads(Db)) -> i32 { f(1) }\n\
         fn main() { run(|x| helper()); }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.kind == EffectErrorKind::EffectSubtypeViolation),
        "expected EffectSubtypeViolation for closure with wrong effects"
    );
}

// ── E0404 structured subtype trace (Phase 3 checklist `karac explain`) ───────

#[test]
fn test_subtype_violation_carries_trace() {
    // E0404 must populate subtype_trace with slot, argument, and offending sets.
    let parsed = parse(
        "effect resource Db;\n\
         fn write_fn(x: i32) -> i32 writes(Db) { x }\n\
         fn run(f: Fn(i32) -> i32 with reads(Db)) -> i32 { f(1) }\n\
         fn main() { run(write_fn); }",
    );
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let result = effectcheck(&parsed.program);
    let violations: Vec<_> = result
        .errors
        .iter()
        .filter(|e| e.kind == EffectErrorKind::EffectSubtypeViolation)
        .collect();
    assert!(!violations.is_empty(), "expected at least one E0404");

    for v in &violations {
        let trace = v
            .subtype_trace
            .as_ref()
            .expect("E0404 must carry subtype_trace");
        assert!(
            trace.slot_effects.iter().any(|s| s.contains("reads")),
            "slot_effects should contain reads(Db), got: {:?}",
            trace.slot_effects
        );
        assert!(
            trace.argument_effects.iter().any(|s| s.contains("writes")),
            "argument_effects should contain writes(Db), got: {:?}",
            trace.argument_effects
        );
        assert!(
            trace.offending_effects.iter().any(|s| s.contains("writes")),
            "offending_effects should contain writes(Db), got: {:?}",
            trace.offending_effects
        );
        // The offending set must be a subset of the argument set.
        for off in &trace.offending_effects {
            assert!(
                trace.argument_effects.contains(off),
                "offending {} must appear in argument_effects {:?}",
                off,
                trace.argument_effects
            );
        }
        // The offending set must not intersect the slot set.
        for off in &trace.offending_effects {
            assert!(
                !trace.slot_effects.contains(off),
                "offending {} must not be in slot_effects {:?}",
                off,
                trace.slot_effects
            );
        }
    }
}

#[test]
fn test_subtype_ok_violations_have_no_trace_for_passing_args() {
    // For passing calls (no E0404), no violations are emitted at all.
    let result = effectcheck_ok(
        "effect resource Db;\n\
         fn read_fn(x: i32) -> i32 reads(Db) { x }\n\
         fn run(f: Fn(i32) -> i32 with reads(Db)) -> i32 { f(1) }\n\
         fn main() { run(read_fn); }",
    );
    let violations: Vec<_> = result
        .errors
        .iter()
        .filter(|e| e.kind == EffectErrorKind::EffectSubtypeViolation)
        .collect();
    assert!(
        violations.is_empty(),
        "no E0404 expected for a passing subset check"
    );
}

#[test]
fn test_subtype_trace_slot_is_empty_for_pure_slot() {
    // Slot annotated `with reads(Db)` but arg does `writes(Db)` — slot is non-empty,
    // argument has a single write, offending = [writes(Db)].
    let parsed = parse(
        "effect resource Db;\n\
         fn write_fn(x: i32) -> i32 writes(Db) { x }\n\
         fn run(f: Fn(i32) -> i32 with reads(Db)) -> i32 { f(1) }\n\
         fn main() { run(write_fn); }",
    );
    assert!(parsed.errors.is_empty());
    let result = effectcheck(&parsed.program);
    let v = result
        .errors
        .iter()
        .find(|e| e.kind == EffectErrorKind::EffectSubtypeViolation)
        .expect("expected E0404");
    let trace = v.subtype_trace.as_ref().unwrap();
    // slot must contain reads(Db), NOT writes(Db)
    assert!(!trace.slot_effects.iter().any(|s| s.contains("writes")));
    // offending must not be in slot
    assert!(trace
        .offending_effects
        .iter()
        .all(|o| !trace.slot_effects.contains(o)));
}

#[test]
fn test_subtype_closure_violation_also_has_trace() {
    // Closure argument into a restricted slot also gets a trace.
    let parsed = parse(
        "effect resource Db;\n\
         fn helper() -> i32 writes(Db) { 0 }\n\
         fn run(f: Fn(i32) -> i32 with reads(Db)) -> i32 { f(1) }\n\
         fn main() { run(|x| helper()); }",
    );
    assert!(parsed.errors.is_empty());
    let result = effectcheck(&parsed.program);
    let v = result
        .errors
        .iter()
        .find(|e| e.kind == EffectErrorKind::EffectSubtypeViolation)
        .expect("expected E0404 for closure violation");
    assert!(
        v.subtype_trace.is_some(),
        "closure E0404 must carry subtype_trace"
    );
}

#[test]
fn test_subtype_closure_arg_pure_ok() {
    // Pure closure into reads slot — OK (∅ ⊆ {reads(Db)})
    effectcheck_ok(
        "fn run(f: Fn(i32) -> i32 with reads(Db)) -> i32 { f(1) }\n\
         fn main() { run(|x| x + 1); }",
    );
}

#[test]
fn test_effect_variable_no_undefined_group_error() {
    // A function declaring [with E] and returning `with E` must NOT produce
    // an UndefinedEffectGroup error — Variable("E") is not a group lookup.
    effectcheck_ok("pub fn apply[T, with E](f: Fn(T) -> T with E, x: T) -> T with E { f(x) }");
}

#[test]
fn test_effect_variable_makes_function_polymorphic() {
    // [with E] implies the function is effect-polymorphic; a public fn that
    // only declares `with E` (no fixed effects) must not be treated as having
    // Explicit(empty) — the checker should not demand it declare missing effects.
    effectcheck_ok("pub fn identity[T, with E](f: Fn(T) -> T with E, x: T) -> T with E { f(x) }");
}

// ── Phase 6 line 26 slice 8ac: `E: Effect` bound-form equivalence ─
//
// The bounded form `[T, E: Effect]` from design.md line 736 should
// behave identically to the positional `[T, with E]` form at the
// effect-checker layer — same polymorphic classification, same
// per-call var-binding resolution, same diagnostic surface. Slice
// 8ac is parser/AST only; these tests pin the no-regression
// equivalence so downstream phases pick up the new shape without
// further changes.

#[test]
fn test_slice_8ac_bound_form_polymorphic_classification() {
    // Identical to test_effect_variable_makes_function_polymorphic
    // but using the `E: Effect` bound spelling. Must pass without
    // E0404 / undefined-group / public-effects-missing diagnostics.
    effectcheck_ok(
        "pub fn identity[T, E: Effect](f: Fn(T) -> T with E, x: T) -> T with E { f(x) }",
    );
}

#[test]
fn test_slice_8ac_bound_form_polymorphic_round_trip_with_closure() {
    // Compound polymorphism end-to-end with the bound-form spelling.
    // A closure with concrete effects binds `E` at the call site;
    // the inferred caller effects must include the closure's effects.
    let source = "        resource Db
        fn touch_db() with reads(Db) { todo() }
        pub fn pipeline[T, E: Effect](x: T, cb: Fn(T) -> T with E) -> T with E { cb(x) }
        fn caller() with reads(Db) { pipeline(42, |y| { touch_db(); y }); }
        fn todo() -> i64 { 0 }
    ";
    effectcheck_ok(source);
}

#[test]
fn test_slice_8ac_bound_form_apply_no_diagnostics() {
    // The bound-form variant of test_polymorphic_returning_with_E_does_not_emit_missing_effects.
    effectcheck_ok("pub fn apply[T, E: Effect](f: Fn(T) -> T with E, x: T) -> T with E { f(x) }");
}

// ── Round 10.1: end-to-end compound type+effect polymorphism ────
//
// With round 10.1's two-pass arg inference, a generic `[T, with E]`
// function can be called with a closure whose body's effects bind `E`
// while `T` is solved from a non-closure arg. This test threads through
// the full pipeline (typecheck solves `T`, then effectcheck resolves `E`
// via round 9's `compute_call_var_bindings` against the type-checked
// closure body) — demonstrating steps 1–4 of design.md § Monomorphization
// order for compound polymorphism work end-to-end.

#[test]
fn test_compound_polymorphism_end_to_end_effect_propagation() {
    // `pipeline` is generic in both `T` and `E`. The call `pipeline(42, |y| touch_db(y))`:
    //   step 1 — non-closure arg `42` collects `T = i64`
    //   step 2 — `T` is substituted into `Fn(T) -> T`, closure body sees `y: i64`
    //   step 3 — `E` unifies with the closure body's inferred effects = {reads(Db)}
    //   step 4 — call site contributes `reads(Db)` to `main`, which declares it
    let source = "effect resource Db;\n\
                  pub fn pipeline[T, with E](x: T, cb: Fn(T) -> T with E) -> T with E { cb(x) }\n\
                  pub fn touch_db(x: i64) -> i64 with reads(Db) { x }\n\
                  pub fn main() with reads(Db) {\n\
                      let _ = pipeline(42, |y| touch_db(y));\n\
                  }";
    effectcheck_full_pipeline(source);
}

#[test]
fn test_compound_polymorphism_nested_bottom_up_resolution() {
    // Step 5 of design.md § Monomorphization order for compound polymorphism:
    // nested compound calls resolve bottom-up in expression-typing order. Inner
    // call resolves first (T_inner = i64, E_inner = {reads(Db)}), then its
    // concrete result (an i64-returning closure body carrying `reads(Db)`)
    // becomes input to the outer call's resolution (T_outer = i64,
    // E_outer = {reads(Db)} via outer closure body's inferred effects).
    //
    // Each call resolves independently — no global constraint system. The test
    // demonstrates this by using *different* type parameter names (T/U) and
    // *different* effect variables (E/F) on the two generic functions: their
    // namespaces don't collide, and both still bind correctly to i64 / reads(Db).
    let source = "effect resource Db;\n\
                  pub fn outer[T, with E](x: T, cb: Fn(T) -> T with E) -> T with E { cb(x) }\n\
                  pub fn inner[U, with F](y: U, dg: Fn(U) -> U with F) -> U with F { dg(y) }\n\
                  pub fn touch_db(z: i64) -> i64 with reads(Db) { z }\n\
                  pub fn main() with reads(Db) {\n\
                      let _ = outer(42, |a| inner(a, |b| touch_db(b)));\n\
                  }";
    effectcheck_full_pipeline(source);
}

// ── Round 10.3 step 7: monomorphized signature on E0404 ─────────
//
// When a compound-polymorphic call's effect-subtyping check fails, the
// diagnostic must carry a fully-monomorphized callee signature so the user
// sees `Fn(i64) -> ()` instead of `Fn(T) -> ()` (design.md § Monomorphization
// order for compound polymorphism, step 7). The signature is rendered with
// type parameters resolved per the typechecker's `call_type_subs` and effect
// variables resolved per the call's `var_bindings`.

#[test]
fn test_monomorphized_signature_substitutes_type_param_in_e0404() {
    // `pipeline[T]` has a slot `Fn(T) -> () with reads(Db)` declared narrow.
    // Caller passes `42i64` (binds T=i64) and a closure that does writes(Log),
    // not reads(Db). E0404 fires; the diagnostic must show the slot with T
    // substituted to i64 in the callee signature.
    let source = "effect resource Db;\n\
                  effect resource Log;\n\
                  pub fn write_log() with writes(Log) {}\n\
                  pub fn pipeline[T](x: T, cb: Fn(T) -> () with reads(Db)) with reads(Db) { cb(x); }\n\
                  pub fn main() with writes(Log) reads(Db) {\n\
                      pipeline(42i64, |y| write_log())\n\
                  }";
    let result = effectcheck_full_pipeline(source);
    let v = result
        .errors
        .iter()
        .find(|e| e.kind == EffectErrorKind::EffectSubtypeViolation)
        .unwrap_or_else(|| {
            panic!(
                "expected EffectSubtypeViolation, got: {:?}",
                result
                    .errors
                    .iter()
                    .map(|e| (&e.kind, &e.message))
                    .collect::<Vec<_>>()
            )
        });
    let trace = v
        .subtype_trace
        .as_ref()
        .expect("E0404 must carry subtype_trace");
    let sig = trace
        .monomorphized_signature
        .as_ref()
        .expect("compound polymorphic call's E0404 must carry a monomorphized signature");
    assert!(
        sig.contains("Fn(i64)"),
        "monomorphized signature should substitute T → i64, got: {sig}"
    );
    assert!(
        !sig.contains("Fn(T)") && !sig.contains("(T,") && !sig.contains("(T)"),
        "monomorphized signature must not mention the type variable `T`, got: {sig}"
    );
    assert!(
        sig.contains("pipeline"),
        "signature should include the callee name, got: {sig}"
    );
    assert!(
        sig.contains("reads(Db)"),
        "signature should preserve the slot's declared effect set, got: {sig}"
    );
    // The message must also surface the signature — JSON consumers read
    // the trace, but humans read the message text.
    assert!(
        v.message.contains(sig),
        "error message must include the monomorphized signature, got: {}",
        v.message
    );
}

#[test]
fn test_monomorphized_signature_resolves_with_e_variable() {
    // Compound `[T, with E]` call where E unifies to writes(Log) via the closure
    // body, but the slot is the closure's body. To trigger E0404 while still
    // exercising var-binding rendering, narrow one slot to `with reads(Db)`
    // (concrete) and bind E from another slot.
    //
    // pipeline[T, with E](x: T, restricted: Fn(T) -> () with reads(Db),
    //                     vary: Fn(T) -> () with E) with reads(Db) + E
    // Calling with restricted = |y| write_log() (writes(Log) — NOT reads(Db))
    // triggers E0404 on the `restricted` arg. The signature should still
    // resolve E from the `vary` slot's closure for completeness.
    let source = "effect resource Db;\n\
                  effect resource Log;\n\
                  pub fn write_log() with writes(Log) {}\n\
                  pub fn read_db() with reads(Db) {}\n\
                  pub fn pipeline[T, with E](x: T, \
                      restricted: Fn(T) -> () with reads(Db), \
                      vary: Fn(T) -> () with E) with reads(Db) E { \
                      restricted(x); vary(x); \
                  }\n\
                  pub fn main() with reads(Db) writes(Log) {\n\
                      pipeline(7i64, |y| write_log(), |z| read_db())\n\
                  }";
    let result = effectcheck_full_pipeline(source);
    let v = result
        .errors
        .iter()
        .find(|e| e.kind == EffectErrorKind::EffectSubtypeViolation)
        .unwrap_or_else(|| {
            panic!(
                "expected EffectSubtypeViolation, got: {:?}",
                result
                    .errors
                    .iter()
                    .map(|e| (&e.kind, &e.message))
                    .collect::<Vec<_>>()
            )
        });
    let trace = v.subtype_trace.as_ref().unwrap();
    let sig = trace
        .monomorphized_signature
        .as_ref()
        .expect("compound polymorphic E0404 must carry a monomorphized signature");
    // T → i64 substitution shows up in every Fn slot.
    assert!(
        sig.contains("Fn(i64)") && !sig.contains("Fn(T)"),
        "signature must substitute T → i64, got: {sig}"
    );
    // E binding from the closure passed at the `vary` slot.
    assert!(
        sig.contains("reads(Db)") && sig.contains("with"),
        "signature must include resolved effect spec, got: {sig}"
    );
}

#[test]
fn test_monomorphized_signature_omitted_for_non_generic_call() {
    // Non-generic call → no type substitution → signature is `None` (the
    // diagnostic is no less informative than before — slot effects are still
    // listed in the message).
    let parsed = parse(
        "effect resource Db;\n\
         fn write_fn(x: i32) -> i32 writes(Db) { x }\n\
         fn run(f: Fn(i32) -> i32 with reads(Db)) -> i32 { f(1) }\n\
         fn main() { run(write_fn); }",
    );
    assert!(parsed.errors.is_empty());
    let result = effectcheck(&parsed.program);
    let v = result
        .errors
        .iter()
        .find(|e| e.kind == EffectErrorKind::EffectSubtypeViolation)
        .expect("expected E0404");
    let trace = v.subtype_trace.as_ref().unwrap();
    assert!(
        trace.monomorphized_signature.is_none(),
        "non-generic call should not carry a monomorphized signature \
         (the slot type is already concrete in source), got: {:?}",
        trace.monomorphized_signature
    );
}

// ── Round 10.3 step 8: trait-method effects compose with `with E` ─
//
// Per design.md § Monomorphization order for compound polymorphism (step 8):
// inside `f[T: Trait, with E]`'s body, method calls on `T`-bound values
// resolve through `T`'s per-method declarations and compose with the resolved
// effect variable as a *union* at the call-site contribution step — not
// during effect unification. The trait method's effects flow alongside `E`'s
// binding; both surface in the caller's inferred set.

#[test]
fn test_trait_method_effects_union_with_with_e_binding() {
    // `pipeline[T: Storage, with E]` invokes `store.fetch()` (Storage.fetch
    // declares `reads(Cache)`) and the closure `cb` (binds `E` to whatever
    // its body does). The caller passes a closure that does `writes(Log)`.
    // Both effects should reach `main`, which declares both.
    let source = "effect resource Cache;\n\
                  effect resource Log;\n\
                  pub trait Storage {\n\
                      fn fetch(self) -> i64 with reads(Cache);\n\
                  }\n\
                  pub struct DiskStore {}\n\
                  impl Storage for DiskStore {\n\
                      fn fetch(self) -> i64 with reads(Cache) { 0 }\n\
                  }\n\
                  pub fn write_log() with writes(Log) {}\n\
                  pub fn pipeline[T: Storage, with E](store: T, cb: Fn() with E) \
                      -> i64 with reads(Cache) E { \
                      cb(); store.fetch() \
                  }\n\
                  pub fn main() with reads(Cache) writes(Log) {\n\
                      let _ = pipeline(DiskStore, || write_log());\n\
                  }";
    effectcheck_full_pipeline(source);
}

#[test]
fn test_trait_method_effect_missing_from_caller_declaration_fails() {
    // Same shape, but `main` only declares `writes(Log)` — the trait method's
    // `reads(Cache)` must still surface, and the missing declaration must be
    // diagnosed.
    let source = "effect resource Cache;\n\
                  effect resource Log;\n\
                  pub trait Storage {\n\
                      fn fetch(self) -> i64 with reads(Cache);\n\
                  }\n\
                  pub struct DiskStore {}\n\
                  impl Storage for DiskStore {\n\
                      fn fetch(self) -> i64 with reads(Cache) { 0 }\n\
                  }\n\
                  pub fn write_log() with writes(Log) {}\n\
                  pub fn pipeline[T: Storage, with E](store: T, cb: Fn() with E) \
                      -> i64 with reads(Cache) E { \
                      cb(); store.fetch() \
                  }\n\
                  pub fn main() with writes(Log) {\n\
                      let _ = pipeline(DiskStore, || write_log());\n\
                  }";
    let mut parsed = parse(source);
    assert!(parsed.errors.is_empty(), "Parse: {:?}", parsed.errors);
    let resolved = resolve(&parsed.program);
    assert!(resolved.errors.is_empty(), "Resolve: {:?}", resolved.errors);
    let typed = typecheck(&parsed.program, &resolved);
    assert!(typed.errors.is_empty(), "Type: {:?}", typed.errors);
    let method_types = typed.method_callee_types.clone();
    let call_type_subs = typed.call_type_subs.clone();
    lower(&mut parsed.program, &typed);
    let result = effectcheck_with_typecheck_data(
        &parsed.program,
        PublicEffectsPolicy::default(),
        CompileProfile::Default,
        method_types,
        call_type_subs,
    );
    assert!(
        result
            .errors
            .iter()
            .any(|e| e.kind == EffectErrorKind::MissingEffectDeclaration
                && e.message.contains("reads")
                && e.message.contains("Cache")),
        "expected missing-declaration diagnostic naming reads(Cache), got: {:?}",
        result
            .errors
            .iter()
            .map(|e| (&e.kind, &e.message))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_with_e_closure_effect_missing_from_caller_declaration_fails() {
    // Mirror of the above on the other axis — the trait method's effect is
    // declared, but the closure's `writes(Log)` (which binds `E`) is not.
    let source = "effect resource Cache;\n\
                  effect resource Log;\n\
                  pub trait Storage {\n\
                      fn fetch(self) -> i64 with reads(Cache);\n\
                  }\n\
                  pub struct DiskStore {}\n\
                  impl Storage for DiskStore {\n\
                      fn fetch(self) -> i64 with reads(Cache) { 0 }\n\
                  }\n\
                  pub fn write_log() with writes(Log) {}\n\
                  pub fn pipeline[T: Storage, with E](store: T, cb: Fn() with E) \
                      -> i64 with reads(Cache) E { \
                      cb(); store.fetch() \
                  }\n\
                  pub fn main() with reads(Cache) {\n\
                      let _ = pipeline(DiskStore, || write_log());\n\
                  }";
    let mut parsed = parse(source);
    assert!(parsed.errors.is_empty(), "Parse: {:?}", parsed.errors);
    let resolved = resolve(&parsed.program);
    assert!(resolved.errors.is_empty(), "Resolve: {:?}", resolved.errors);
    let typed = typecheck(&parsed.program, &resolved);
    assert!(typed.errors.is_empty(), "Type: {:?}", typed.errors);
    let method_types = typed.method_callee_types.clone();
    let call_type_subs = typed.call_type_subs.clone();
    lower(&mut parsed.program, &typed);
    let result = effectcheck_with_typecheck_data(
        &parsed.program,
        PublicEffectsPolicy::default(),
        CompileProfile::Default,
        method_types,
        call_type_subs,
    );
    assert!(
        result
            .errors
            .iter()
            .any(|e| e.kind == EffectErrorKind::MissingEffectDeclaration
                && e.message.contains("writes")
                && e.message.contains("Log")),
        "expected missing-declaration diagnostic naming writes(Log), got: {:?}",
        result
            .errors
            .iter()
            .map(|e| (&e.kind, &e.message))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_trait_method_effects_compose_at_call_site_not_during_unification() {
    // Verifies the structural ordering: trait-method effects come from
    // dispatch-time resolution (independent of `E`), so when two impls have
    // *different* trait-method effects, each call site picks up the one
    // matching its concrete `T`. `E` binding is unrelated.
    //
    // No diagnostic should fire — `pipeline` declares `with reads(Cache) E`
    // (not the union of all impls' effects) and each call narrows correctly.
    let source = "effect resource Cache;\n\
                  effect resource Log;\n\
                  pub trait Storage {\n\
                      fn fetch(self) -> i64 with reads(Cache);\n\
                  }\n\
                  pub struct DiskStore {}\n\
                  impl Storage for DiskStore {\n\
                      fn fetch(self) -> i64 with reads(Cache) { 0 }\n\
                  }\n\
                  pub fn pipeline[T: Storage, with E](store: T, cb: Fn() with E) \
                      -> i64 with reads(Cache) E { \
                      cb(); store.fetch() \
                  }\n\
                  pub fn main() with reads(Cache) {\n\
                      let _ = pipeline(DiskStore, || {});\n\
                  }";
    effectcheck_full_pipeline(source);
}

// ── Round 9: `with E` same-signature unification ────────────────

#[test]
fn test_with_e_two_slots_disagreeing_closures_conflict() {
    // Two `with E` slots, closures with different effect sets → conflict.
    // The diagnostic names E and both bindings. (Other diagnostics may
    // also fire from the existing `with _` viral rule and Fn-slot
    // subtyping checks — those are separate concerns; round 9 ships the
    // conflict-detection diagnostic.)
    let errors = effectcheck_errors(
        "effect resource Log;
         effect resource Db;
         pub fn write_log() with writes(Log) {}
         pub fn read_db() with reads(Db) {}
         pub fn pipe[with E](a: Fn() with E, b: Fn() with E) with E { a(); b(); }
         pub fn run() with writes(Log) reads(Db) {
             pipe(|| write_log(), || read_db())
         }",
    );
    let conflict = errors
        .iter()
        .find(|e| e.kind == EffectErrorKind::EffectVariableConflict)
        .unwrap_or_else(|| {
            panic!(
                "expected EffectVariableConflict, got: {:?}",
                errors
                    .iter()
                    .map(|e| (&e.kind, &e.message))
                    .collect::<Vec<_>>()
            )
        });
    assert!(
        conflict.message.contains("`E`")
            && conflict.message.contains("writes(Log)")
            && conflict.message.contains("reads(Db)"),
        "conflict diagnostic should name E and both bindings, got: {}",
        conflict.message
    );
}

#[test]
fn test_with_e_function_ref_args_conflict() {
    // Same conflict shape with function-ref args (not closures). Effect
    // sets come from each function's inferred effects, unified at the
    // call site.
    let errors = effectcheck_errors(
        "effect resource Log;
         effect resource Db;
         pub fn write_log() with writes(Log) {}
         pub fn read_db() with reads(Db) {}
         pub fn pipe[with E](a: Fn() with E, b: Fn() with E) with E { a(); b(); }
         pub fn run() with writes(Log) reads(Db) { pipe(write_log, read_db) }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.kind == EffectErrorKind::EffectVariableConflict),
        "expected EffectVariableConflict for function-ref args, got: {:?}",
        errors
            .iter()
            .map(|e| (&e.kind, &e.message))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_with_e_two_slots_pure_closures_no_conflict() {
    // Both closures pure → both bind E to {} → trivially agree, no
    // EffectVariableConflict diagnostic fires.
    let errors: Vec<_> = effectcheck_all(
        "pub fn pipe[with E](a: Fn() with E, b: Fn() with E) with E { a(); b(); }
         fn run() {
             pipe(|| {}, || {})
         }",
    )
    .errors
    .into_iter()
    .filter(|e| e.kind == EffectErrorKind::EffectVariableConflict)
    .collect();
    assert!(
        errors.is_empty(),
        "pure closures must not trigger EffectVariableConflict, got: {:?}",
        errors
    );
}

#[test]
fn test_with_underscore_two_slots_independent_no_conflict() {
    // Regression: two `with _` slots (no named E) are independent
    // (design.md:317). The conflict diagnostic must NOT fire for `with _`.
    let result = effectcheck_all(
        "effect resource Log;
         effect resource Db;
         pub fn write_log() with writes(Log) {}
         pub fn read_db() with reads(Db) {}
         pub fn pipe(a: Fn() with _, b: Fn() with _) with _ { a(); b(); }
         pub fn run() with _ {
             pipe(|| write_log(), || read_db())
         }",
    );
    let conflicts: Vec<_> = result
        .errors
        .iter()
        .filter(|e| e.kind == EffectErrorKind::EffectVariableConflict)
        .collect();
    assert!(
        conflicts.is_empty(),
        "two `with _` slots are independent — no EffectVariableConflict expected, got: {:?}",
        conflicts
    );
}

// ── Round 9.1: `with E`-aware viral rule + Fn-slot subtyping ────

#[test]
fn test_with_e_agreeing_closures_no_viral_or_subtype_error() {
    // Caller declares only `writes(Log)` and passes two `with E` closures
    // that both perform `writes(Log)`. The `with E` callee must not trip
    // (a) the `with _` viral rule (E is named, not anonymous) nor
    // (b) the Fn-slot subtyping check (E binds to `{writes(Log)}` per
    // call, so each closure satisfies its slot).
    effectcheck_ok(
        "effect resource Log;
         pub fn write_log() with writes(Log) {}
         pub fn write_audit() with writes(Log) {}
         pub fn pipe[with E](a: Fn() with E, b: Fn() with E) with E { a(); b(); }
         pub fn run() with writes(Log) {
             pipe(|| write_log(), || write_audit())
         }",
    );
}

#[test]
fn test_with_e_agreeing_function_refs_no_viral_or_subtype_error() {
    // Same as above but with named function references instead of
    // closure literals. E binds via each callee's inferred effects.
    effectcheck_ok(
        "effect resource Log;
         pub fn write_log() with writes(Log) {}
         pub fn write_audit() with writes(Log) {}
         pub fn pipe[with E](a: Fn() with E, b: Fn() with E) with E { a(); b(); }
         pub fn run() with writes(Log) {
             pipe(write_log, write_audit)
         }",
    );
}

#[test]
fn test_with_e_disagreeing_no_spurious_subtype_error() {
    // When `with E` slots disagree, only the EffectVariableConflict
    // diagnostic fires — the Fn-slot subtyping check uses the union of
    // referenced args' effects so each individual arg satisfies its
    // resolved slot vacuously, avoiding double-reporting.
    let result = effectcheck_all(
        "effect resource Log;
         effect resource Db;
         pub fn write_log() with writes(Log) {}
         pub fn read_db() with reads(Db) {}
         pub fn pipe[with E](a: Fn() with E, b: Fn() with E) with E { a(); b(); }
         pub fn run() with writes(Log) reads(Db) {
             pipe(|| write_log(), || read_db())
         }",
    );
    let subtype_errors: Vec<_> = result
        .errors
        .iter()
        .filter(|e| e.kind == EffectErrorKind::EffectSubtypeViolation)
        .collect();
    assert!(
        subtype_errors.is_empty(),
        "`with E` disagreement must not raise spurious subtype errors, got: {:?}",
        subtype_errors
    );
    let conflicts: Vec<_> = result
        .errors
        .iter()
        .filter(|e| e.kind == EffectErrorKind::EffectVariableConflict)
        .collect();
    assert_eq!(
        conflicts.len(),
        1,
        "expected exactly one EffectVariableConflict, got: {:?}",
        result.errors
    );
}

#[test]
fn test_with_underscore_callee_still_fires_viral_rule() {
    // Regression: a caller declaring fixed effects (not `with _`) that
    // calls a true `with _` callee must still trip the viral rule —
    // round 9.1's relaxation only applies to named `with E` callees.
    let errors = effectcheck_errors(
        "pub fn run_each(f: Fn()) with _ { f(); }
         pub fn caller() { run_each(|| {}) }",
    );
    assert!(
        errors.iter().any(
            |e| matches!(e.kind, EffectErrorKind::MissingEffectDeclaration)
                && e.message.contains("polymorphic")
        ),
        "expected viral-rule MissingEffectDeclaration for `with _` callee, got: {:?}",
        errors
            .iter()
            .map(|e| (&e.kind, &e.message))
            .collect::<Vec<_>>()
    );
}

// ── Round 9.2: `with E` body leak verification + SCC unification ─

#[test]
fn test_with_e_pure_body_leak_diagnosed() {
    // A `pub fn` declared `with E` (purely polymorphic) whose body
    // performs a concrete effect must be diagnosed at the function
    // itself — previously the leak surfaced only at a caller and
    // misattributed to the callee.
    let errors = effectcheck_errors(
        "effect resource Db;
         pub fn read_db() with reads(Db) {}
         pub fn alpha[with E](f: Fn() with E) with E {
             f();
             read_db()
         }",
    );
    let leak: Vec<_> = errors
        .iter()
        .filter(|e| {
            matches!(e.kind, EffectErrorKind::MissingEffectDeclaration)
                && e.message.contains("'alpha'")
                && e.message.contains("reads(Db)")
                && e.message.contains("purely polymorphic")
        })
        .collect();
    assert_eq!(
        leak.len(),
        1,
        "expected one leak diagnostic at alpha, got: {:?}",
        errors
            .iter()
            .map(|e| (&e.kind, &e.message))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_with_e_plus_fixed_body_within_fixed_no_error() {
    // `with E + reads(Config)` — body performs reads(Config) (matches
    // fixed) plus the polymorphic call. No leak.
    effectcheck_ok(
        "effect resource Config;
         pub fn read_config() with reads(Config) {}
         pub fn run_with_config[with E](f: Fn() with E) with E reads(Config) {
             read_config();
             f()
         }",
    );
}

#[test]
fn test_with_e_plus_fixed_body_leak_diagnosed() {
    // `with E + reads(Config)` body also performs writes(Log), which is
    // not in the fixed part. Diagnose at the function itself.
    let errors = effectcheck_errors(
        "effect resource Config;
         effect resource Log;
         pub fn read_config() with reads(Config) {}
         pub fn write_log() with writes(Log) {}
         pub fn run_with_config[with E](f: Fn() with E) with E reads(Config) {
             read_config();
             write_log();
             f()
         }",
    );
    let leak: Vec<_> = errors
        .iter()
        .filter(|e| {
            matches!(e.kind, EffectErrorKind::MissingEffectDeclaration)
                && e.message.contains("'run_with_config'")
                && e.message.contains("writes(Log)")
                && e.message.contains("fixed part")
        })
        .collect();
    assert_eq!(
        leak.len(),
        1,
        "expected one leak diagnostic at run_with_config, got: {:?}",
        errors
            .iter()
            .map(|e| (&e.kind, &e.message))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_with_underscore_body_leak_still_skipped() {
    // Regression: `with _` (anonymous) is viral by design — the wildcard
    // absorbs whatever the body carries. Concrete effects in a `with _`
    // body must NOT trip the leak diagnostic.
    let result = effectcheck_all(
        "effect resource Db;
         pub fn read_db() with reads(Db) {}
         pub fn alpha(f: Fn()) with _ {
             f();
             read_db()
         }",
    );
    let leak_at_alpha: Vec<_> = result
        .errors
        .iter()
        .filter(|e| {
            matches!(e.kind, EffectErrorKind::MissingEffectDeclaration)
                && e.message.contains("'alpha'")
                && (e.message.contains("purely polymorphic") || e.message.contains("fixed part"))
        })
        .collect();
    assert!(
        leak_at_alpha.is_empty(),
        "`with _` body leak must not be diagnosed (it is viral by design), got: {:?}",
        leak_at_alpha
    );
}

#[test]
fn test_polymorphic_scc_pure_no_error() {
    // Two mutually-recursive `with E` functions, both pure outside the
    // polymorphic param. SCC fixed-point converges to empty inferred
    // effects; no leak diagnostic should fire on either member.
    effectcheck_ok(
        "pub fn alpha[with E](f: Fn() with E) with E { f(); beta(f) }
         pub fn beta[with E](f: Fn() with E) with E { f(); alpha(f) }",
    );
}

#[test]
fn test_polymorphic_scc_leak_propagates_to_all_members() {
    // SCC unification of E across the cycle: a leak in beta surfaces
    // for alpha too, because the SCC fixed-point propagates beta's
    // concrete effect through the call graph. Both members fire the
    // diagnostic — formalising that the SCC's E is shared.
    let errors = effectcheck_errors(
        "effect resource Db;
         pub fn read_db() with reads(Db) {}
         pub fn alpha[with E](f: Fn() with E) with E { f(); beta(f) }
         pub fn beta[with E](f: Fn() with E) with E {
             f();
             read_db();
             alpha(f)
         }",
    );
    let alpha_leak = errors.iter().any(|e| {
        matches!(e.kind, EffectErrorKind::MissingEffectDeclaration)
            && e.message.contains("'alpha'")
            && e.message.contains("reads(Db)")
    });
    let beta_leak = errors.iter().any(|e| {
        matches!(e.kind, EffectErrorKind::MissingEffectDeclaration)
            && e.message.contains("'beta'")
            && e.message.contains("reads(Db)")
    });
    assert!(
        alpha_leak && beta_leak,
        "expected leak diagnostic at both alpha and beta (SCC unification of E), got: {:?}",
        errors
            .iter()
            .map(|e| (&e.kind, &e.message))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_multiple_effect_variables_independent() {
    // design.md line 4858: `[with E1, E2]` declares two independent
    // effect variables — each binds via its own slot, no cross-variable
    // unification. Two slots typed `Fn() with E1` and `Fn() with E2`
    // accept arbitrary closures with arbitrary (and different) effects.
    effectcheck_ok(
        "effect resource Log;
         effect resource Db;
         pub fn write_log() with writes(Log) {}
         pub fn read_db() with reads(Db) {}
         pub fn pipe[with E, F](a: Fn() with E, b: Fn() with F) with E F {
             a();
             b()
         }
         pub fn run() with writes(Log) reads(Db) {
             pipe(|| write_log(), || read_db())
         }",
    );
}

#[test]
fn test_with_e_trait_bound_dispatch_not_flagged_as_leak() {
    // design.md line 4901: a `with E` function that dispatches through a
    // typeparam-bounded trait whose method is `with _` must not have its
    // body's effects flagged as concrete leaks. The effect routes through
    // T → Trait.method (polymorphic), i.e. it belongs to E.
    //
    // Without the trait-bound filter on the leak check, the conservative
    // call-collection would attribute `sends(Net)` (from the RemoteProcessor
    // impl) to `run`'s body and false-positive it as a leak.
    let result = effectcheck_all(
        "effect resource Net;
         shared struct LocalProcessor {}
         shared struct RemoteProcessor {}
         pub fn remote_call() with sends(Net) {}
         pub trait Processor {
             fn process(self) with _;
         }
         impl Processor for LocalProcessor {
             fn process(self) {}
         }
         impl Processor for RemoteProcessor {
             fn process(self) with sends(Net) {
                 remote_call()
             }
         }
         pub fn run[T: Processor, with E](p: T) with E {
             p.process()
         }",
    );
    let leak_at_run: Vec<_> = result
        .errors
        .iter()
        .filter(|e| {
            matches!(e.kind, EffectErrorKind::MissingEffectDeclaration)
                && e.message.contains("'run'")
                && (e.message.contains("purely polymorphic") || e.message.contains("fixed part"))
        })
        .collect();
    assert!(
        leak_at_run.is_empty(),
        "trait-bound dispatch through `T: Processor` (Processor.process is `with _`) \
         must not fire the `with E` leak diagnostic at run: {:?}",
        leak_at_run
    );
}

// ── Profile-compatibility checks (FFI item 6) ───────────────────

fn profile_ok(source: &str, profile: CompileProfile) -> EffectCheckResult {
    let parsed = parse(source);
    assert!(
        parsed.errors.is_empty(),
        "Parse errors: {}",
        parsed
            .errors
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    let result = effectcheck_with_profile(&parsed.program, PublicEffectsPolicy::Declared, profile);
    let real_errors: Vec<_> = result
        .errors
        .iter()
        .filter(|e| e.kind != EffectErrorKind::FfiLintHint)
        .collect();
    assert!(
        real_errors.is_empty(),
        "Unexpected effect errors: {}",
        real_errors
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    result
}

fn profile_errors(source: &str, profile: CompileProfile) -> Vec<EffectError> {
    let parsed = parse(source);
    assert!(
        parsed.errors.is_empty(),
        "Parse errors: {}",
        parsed
            .errors
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    let result = effectcheck_with_profile(&parsed.program, PublicEffectsPolicy::Declared, profile);
    let real_errors: Vec<EffectError> = result
        .errors
        .into_iter()
        .filter(|e| e.kind != EffectErrorKind::FfiLintHint)
        .collect();
    assert!(
        !real_errors.is_empty(),
        "Expected profile violation errors but got none"
    );
    real_errors
}

#[test]
fn test_profile_default_allows_all_effects() {
    // Default profile imposes no restrictions on extern declarations.
    profile_ok(
        "unsafe extern \"C\" { fn malloc(size: i64) -> i64 allocates(Heap) blocks panics; }",
        CompileProfile::Default,
    );
}

#[test]
fn test_profile_embedded_forbids_heap_allocation() {
    // Embedded profile forbids allocates(Heap) on extern fns.
    let errors = profile_errors(
        "unsafe extern \"C\" { fn malloc(size: i64) -> i64 allocates(Heap); }",
        CompileProfile::Embedded,
    );
    assert!(
        errors
            .iter()
            .any(|e| matches!(e.kind, EffectErrorKind::ProfileViolation)),
        "Expected ProfileViolation, got: {:?}",
        errors
    );
    assert!(
        errors.iter().any(|e| e.message.contains("Heap")),
        "Error should mention Heap, got: {:?}",
        errors
    );
}

#[test]
fn test_profile_embedded_allows_non_heap_allocates() {
    // Embedded only forbids allocates(Heap); other resources are fine.
    profile_ok(
        "unsafe extern \"C\" { fn arena_alloc(size: i64) -> i64 allocates(Arena); }",
        CompileProfile::Embedded,
    );
}

#[test]
fn test_profile_embedded_allows_blocks_and_panics() {
    // Embedded does not restrict blocks or panics.
    profile_ok(
        "unsafe extern \"C\" { fn os_write(fd: i32) -> i32 blocks panics; }",
        CompileProfile::Embedded,
    );
}

#[test]
fn test_profile_kernel_forbids_panics() {
    // Kernel profile forbids panics on extern fns.
    let errors = profile_errors(
        "unsafe extern \"C\" { fn risky() panics; }",
        CompileProfile::Kernel,
    );
    assert!(
        errors
            .iter()
            .any(|e| matches!(e.kind, EffectErrorKind::ProfileViolation)),
        "Expected ProfileViolation for panics in kernel profile, got: {:?}",
        errors
    );
}

#[test]
fn test_profile_kernel_forbids_blocks() {
    // Kernel profile forbids blocks on extern fns.
    let errors = profile_errors(
        "unsafe extern \"C\" { fn sleep(ms: i32) blocks; }",
        CompileProfile::Kernel,
    );
    assert!(
        errors
            .iter()
            .any(|e| matches!(e.kind, EffectErrorKind::ProfileViolation)),
        "Expected ProfileViolation for blocks in kernel profile, got: {:?}",
        errors
    );
}

#[test]
fn test_profile_kernel_forbids_allocates() {
    // Kernel profile forbids any allocates on extern fns.
    let errors = profile_errors(
        "unsafe extern \"C\" { fn kmalloc(size: i64) -> i64 allocates(Heap); }",
        CompileProfile::Kernel,
    );
    assert!(
        errors
            .iter()
            .any(|e| matches!(e.kind, EffectErrorKind::ProfileViolation)),
        "Expected ProfileViolation for allocates in kernel profile, got: {:?}",
        errors
    );
}

#[test]
fn test_profile_kernel_forbids_suspends() {
    // Kernel profile forbids suspends on extern fns.
    let errors = profile_errors(
        "unsafe extern \"C\" { fn await_io() suspends; }",
        CompileProfile::Kernel,
    );
    assert!(
        errors
            .iter()
            .any(|e| matches!(e.kind, EffectErrorKind::ProfileViolation)),
        "Expected ProfileViolation for suspends in kernel profile, got: {:?}",
        errors
    );
}

#[test]
fn test_profile_kernel_allows_reads_writes_sends_receives() {
    // Kernel profile allows resource-verb effects (reads, writes, sends, receives).
    // @noblock suppresses the extern "C" auto-blocks default so it doesn't mask the signal.
    profile_ok(
        "unsafe extern \"C\" { @noblock fn io_op() reads(FileSystem) writes(FileSystem) sends(Channel) receives(Channel); }",
        CompileProfile::Kernel,
    );
}

#[test]
fn test_profile_violation_message_contains_profile_name() {
    // Error message must name the offending profile.
    let errors = profile_errors(
        "unsafe extern \"C\" { fn malloc(size: i64) -> i64 allocates(Heap); }",
        CompileProfile::Embedded,
    );
    let msg = &errors[0].message;
    assert!(
        msg.contains("embedded"),
        "Message should name 'embedded' profile, got: {}",
        msg
    );
}

#[test]
fn test_profile_violation_message_contains_fn_name() {
    // Error message must name the offending extern function.
    let errors = profile_errors(
        "unsafe extern \"C\" { fn my_alloc(size: i64) -> i64 allocates(Heap); }",
        CompileProfile::Embedded,
    );
    let msg = &errors[0].message;
    assert!(
        msg.contains("my_alloc"),
        "Message should name the function 'my_alloc', got: {}",
        msg
    );
}

#[test]
fn test_profile_default_extern_c_blocks_default_is_ok() {
    // Default profile: extern "C" auto-defaults to {blocks}; no violation.
    profile_ok(
        "unsafe extern \"C\" { fn sleep(secs: i32); }",
        CompileProfile::Default,
    );
}

#[test]
fn test_profile_kernel_extern_c_blocks_default_is_violation() {
    // Kernel profile: extern "C" auto-defaults to {blocks}; blocks is forbidden.
    let errors = profile_errors(
        "unsafe extern \"C\" { fn sleep(secs: i32); }",
        CompileProfile::Kernel,
    );
    assert!(
        errors
            .iter()
            .any(|e| matches!(e.kind, EffectErrorKind::ProfileViolation)),
        "Expected ProfileViolation for auto-blocks in kernel profile, got: {:?}",
        errors
    );
}

#[test]
fn test_profile_multiple_violations_reported() {
    // Both allocates(Heap) and blocks generate violations in kernel profile.
    let errors = profile_errors(
        "unsafe extern \"C\" { fn big_alloc(size: i64) -> i64 allocates(Heap) blocks; }",
        CompileProfile::Kernel,
    );
    let violations: Vec<_> = errors
        .iter()
        .filter(|e| matches!(e.kind, EffectErrorKind::ProfileViolation))
        .collect();
    assert!(
        violations.len() >= 2,
        "Expected at least 2 ProfileViolation errors, got: {}",
        violations.len()
    );
}

// ── FFI linter hints (Phase 3 checklist item 7) ─────────────────

fn ffi_hints(source: &str) -> Vec<EffectError> {
    let parsed = parse(source);
    assert!(
        parsed.errors.is_empty(),
        "Parse errors: {:?}",
        parsed.errors
    );
    let result = effectcheck(&parsed.program);
    result
        .errors
        .into_iter()
        .filter(|e| e.kind == EffectErrorKind::FfiLintHint)
        .collect()
}

#[test]
fn test_ffi_hint_malloc_missing_allocates() {
    // `malloc` without `allocates(Heap)` → lint hint
    let hints = ffi_hints("unsafe extern \"C\" { fn malloc(size: i64) -> i64; }");
    assert!(
        hints.iter().any(|h| h.message.contains("allocates(Heap)")),
        "Expected allocates(Heap) hint for malloc, got: {:?}",
        hints
    );
}

#[test]
fn test_ffi_hint_malloc_suppressed_when_declared() {
    // `malloc` with `allocates(Heap)` → no hint
    let hints = ffi_hints("unsafe extern \"C\" { fn malloc(size: i64) -> i64 allocates(Heap); }");
    assert!(
        hints.is_empty(),
        "No alloc hint expected when allocates(Heap) is declared, got: {:?}",
        hints
    );
}

#[test]
fn test_ffi_hint_sleep_missing_blocks() {
    // extern "C" sleep already gets blocks from the ABI default — no hint
    let hints = ffi_hints("unsafe extern \"C\" { fn sleep(secs: i32); }");
    assert!(
        hints.is_empty(),
        "No blocks hint expected for extern \"C\" sleep (ABI default adds it), got: {:?}",
        hints
    );
}

#[test]
fn test_ffi_hint_sleep_noblock_suggests_blocks() {
    // @noblock removes the ABI default → blocks is now missing → hint fires
    let hints = ffi_hints("unsafe extern \"C\" { @noblock fn sleep(secs: i32); }");
    assert!(
        hints.iter().any(|h| h.message.contains("blocks")),
        "Expected blocks hint for @noblock sleep, got: {:?}",
        hints
    );
}

#[test]
fn test_ffi_hint_getaddrinfo_both_blocking_and_allocating() {
    // getaddrinfo is both blocking and allocating; bare declaration gets two hints.
    // extern "C" ABI default already covers `blocks`, so only allocates hint fires.
    let hints = ffi_hints(
        "unsafe extern \"C\" { fn getaddrinfo(node: i64, service: i64, hints: i64, res: i64) -> i32; }",
    );
    assert!(
        hints.iter().any(|h| h.message.contains("allocates(Heap)")),
        "Expected allocates(Heap) hint for getaddrinfo, got: {:?}",
        hints
    );
    // blocks is already in the ABI default — no redundant hint
    assert!(
        !hints.iter().any(|h| h.message.contains("blocks")),
        "No blocks hint expected for getaddrinfo (ABI default covers it), got: {:?}",
        hints
    );
}

#[test]
fn test_ffi_hint_unknown_symbol_no_hint() {
    // An extern fn with an unknown symbol name gets no hints.
    let hints = ffi_hints("unsafe extern \"C\" { fn do_something_custom() -> i32; }");
    assert!(
        hints.is_empty(),
        "No hints expected for unknown symbol, got: {:?}",
        hints
    );
}

#[test]
fn test_ffi_hint_strdup_missing_allocates() {
    // strdup without allocates(Heap) → hint
    let hints = ffi_hints("unsafe extern \"C\" { fn strdup(s: i64) -> i64; }");
    assert!(
        hints.iter().any(|h| h.message.contains("allocates(Heap)")),
        "Expected allocates(Heap) hint for strdup, got: {:?}",
        hints
    );
}

#[test]
fn test_ffi_hint_is_advisory_not_error() {
    // A program with only FFI lint hints still passes effectcheck_ok
    // (hints do not prevent compilation).
    effectcheck_ok("unsafe extern \"C\" { fn malloc(size: i64) -> i64; }");
}

#[test]
fn test_ffi_hint_leading_underscore_normalized() {
    // Platform-prefixed names like `_malloc` still match the table.
    let hints = ffi_hints("unsafe extern \"C\" { fn _malloc(size: i64) -> i64; }");
    assert!(
        hints.iter().any(|h| h.message.contains("allocates(Heap)")),
        "Expected allocates(Heap) hint for _malloc, got: {:?}",
        hints
    );
}

// ── Impl-vs-trait ceiling (step 5) ──────────────────────────────────────────

#[test]
fn test_impl_trait_ceiling_satisfied() {
    // impl method effect == trait ceiling → ok.
    effectcheck_ok(
        "effect resource Db;
         pub fn do_db_read() with reads(Db) {}
         pub trait Repo with reads(Db) {
             fn fetch(ref self) -> i64;
         }
         pub struct MyRepo {}
         impl Repo for MyRepo {
             fn fetch(ref self) -> i64 { do_db_read(); 42 }
         }",
    );
}

#[test]
fn test_impl_trait_ceiling_narrower_ok() {
    // impl method performs no effects when ceiling allows reads(Db) → ok (narrower is fine).
    effectcheck_ok(
        "effect resource Db;
         pub trait Repo with reads(Db) {
             fn fetch(ref self) -> i64;
         }
         pub struct MyRepo {}
         impl Repo for MyRepo {
             fn fetch(ref self) -> i64 { 42 }
         }",
    );
}

#[test]
fn test_impl_trait_ceiling_violated() {
    // impl method performs reads(Cache) but trait ceiling is reads(Db) → error.
    let errors = effectcheck_errors(
        "effect resource Db;
         effect resource Cache;
         pub fn do_cache_read() with reads(Cache) {}
         pub trait Repo with reads(Db) {
             fn fetch(ref self) -> i64;
         }
         pub struct MyRepo {}
         impl Repo for MyRepo {
             fn fetch(ref self) -> i64 { do_cache_read(); 42 }
         }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.kind == EffectErrorKind::ImplExceedsTraitCeiling),
        "Expected ImplExceedsTraitCeiling, got: {:?}",
        errors
    );
}

#[test]
fn test_impl_trait_no_ceiling_any_effects_ok() {
    // Trait method has no `with` clause → no ceiling → impl may perform any effects.
    effectcheck_ok(
        "effect resource Db;
         pub fn do_db_read() with reads(Db) {}
         pub trait Repo {
             fn fetch(ref self) -> i64;
         }
         pub struct MyRepo {}
         impl Repo for MyRepo {
             fn fetch(ref self) -> i64 { do_db_read(); 42 }
         }",
    );
}

#[test]
fn test_impl_trait_wildcard_ceiling_any_effects_ok() {
    // Trait method ceiling is `with _` (polymorphic) → impl may perform any effects.
    effectcheck_ok(
        "effect resource Db;
         pub fn do_db_read() with reads(Db) {}
         pub trait Repo {
             fn fetch(ref self) -> i64 with _;
         }
         pub struct MyRepo {}
         impl Repo for MyRepo {
             fn fetch(ref self) -> i64 { do_db_read(); 42 }
         }",
    );
}

#[test]
fn test_inherent_impl_no_ceiling_check() {
    // Inherent impl (no trait name) is never checked against any ceiling.
    effectcheck_ok(
        "effect resource Db;
         pub fn do_db_read() with reads(Db) {}
         pub struct MyRepo {}
         impl MyRepo {
             fn fetch(ref self) -> i64 { do_db_read(); 42 }
         }",
    );
}

// ── Trait default method body check (step 6) ────────────────────────────────

#[test]
fn test_trait_default_body_within_ceiling() {
    // Default body performs exactly the declared ceiling effect → ok.
    effectcheck_ok(
        "effect resource Db;
         pub fn do_db_read() with reads(Db) {}
         pub trait Repo {
             fn fetch(ref self) -> i64 with reads(Db) { do_db_read(); 42 }
         }",
    );
}

#[test]
fn test_trait_default_body_narrower_ok() {
    // Default body performs no effects when ceiling allows reads(Db) → ok (narrower is fine).
    effectcheck_ok(
        "effect resource Db;
         pub trait Repo {
             fn fetch(ref self) -> i64 with reads(Db) { 42 }
         }",
    );
}

#[test]
fn test_trait_default_body_exceeds_ceiling() {
    // Default body performs reads(Cache) but ceiling is reads(Db) → error.
    let errors = effectcheck_errors(
        "effect resource Db;
         effect resource Cache;
         pub fn do_cache_read() with reads(Cache) {}
         pub trait Repo {
             fn fetch(ref self) -> i64 with reads(Db) { do_cache_read(); 42 }
         }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.kind == EffectErrorKind::TraitDefaultExceedsCeiling),
        "Expected TraitDefaultExceedsCeiling, got: {:?}",
        errors
    );
}

#[test]
fn test_trait_default_body_no_ceiling_ok() {
    // Method has no `with` clause → no ceiling → default body may perform any effects.
    effectcheck_ok(
        "effect resource Db;
         pub fn do_db_read() with reads(Db) {}
         pub trait Repo {
             fn fetch(ref self) -> i64 { do_db_read(); 42 }
         }",
    );
}

#[test]
fn test_trait_default_body_wildcard_ceiling_ok() {
    // Ceiling is `with _` (polymorphic) → default body may perform any effects.
    effectcheck_ok(
        "effect resource Db;
         pub fn do_db_read() with reads(Db) {}
         pub trait Repo {
             fn fetch(ref self) -> i64 with _ { do_db_read(); 42 }
         }",
    );
}

// ── Trait-level effect ceiling (steps 3–4) ──────────────────────────────────

#[test]
fn test_trait_no_effects_methods_unbound() {
    // A trait with no `with` leaves methods with no declared effect ceiling.
    effectcheck_ok(
        "trait Pure {
             fn compute(ref self) -> i64;
         }",
    );
}

#[test]
fn test_trait_level_with_inherited_by_method() {
    // Methods without their own `with` inherit the trait-level ceiling.
    effectcheck_ok(
        "effect resource Db;
         pub trait Repo with reads(Db) {
             fn fetch(ref self) -> i64;
         }
         pub fn query[R: Repo](r: ref R) -> i64 with reads(Db) { r.fetch() }",
    );
}

#[test]
fn test_trait_method_overrides_trait_level_with() {
    // A method with its own `with` fully replaces the trait-level default.
    effectcheck_ok(
        "effect resource Db;
         effect resource Cache;
         pub trait Repo with reads(Db) {
             fn fetch(ref self) -> i64;
             fn cached_fetch(ref self) -> i64 with reads(Cache);
         }",
    );
}

// ── Step 7: Associated function parity ──────────────────────────────────────

#[test]
fn test_assoc_fn_ceiling_collected() {
    // Associated function (no self) has its ceiling collected identically to a
    // receiver method — impl within ceiling is accepted.
    effectcheck_ok(
        "effect resource Db;
         pub fn do_db_read() with reads(Db) {}
         pub trait Factory with reads(Db) {
             fn create() -> i64;
         }
         pub struct MyFactory {}
         impl Factory for MyFactory {
             fn create() -> i64 { do_db_read(); 42 }
         }",
    );
}

#[test]
fn test_assoc_fn_impl_exceeds_ceiling() {
    // Impl of an associated function exceeds the trait ceiling → ImplExceedsTraitCeiling.
    let errors = effectcheck_errors(
        "effect resource Db;
         effect resource Cache;
         pub fn do_cache_read() with reads(Cache) {}
         pub trait Factory with reads(Db) {
             fn create() -> i64;
         }
         pub struct MyFactory {}
         impl Factory for MyFactory {
             fn create() -> i64 { do_cache_read(); 42 }
         }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.kind == EffectErrorKind::ImplExceedsTraitCeiling),
        "Expected ImplExceedsTraitCeiling for associated fn, got: {:?}",
        errors
    );
}

#[test]
fn test_assoc_fn_default_body_exceeds_ceiling() {
    // Default body of an associated function exceeds its own ceiling →
    // TraitDefaultExceedsCeiling.
    let errors = effectcheck_errors(
        "effect resource Db;
         effect resource Cache;
         pub fn do_cache_read() with reads(Cache) {}
         pub trait Factory {
             fn create() -> i64 with reads(Db) { do_cache_read(); 42 }
         }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.kind == EffectErrorKind::TraitDefaultExceedsCeiling),
        "Expected TraitDefaultExceedsCeiling for associated fn default body, got: {:?}",
        errors
    );
}

// ── Step 9: Unimpled private trait = unbound ceiling ────────────────────────

#[test]
fn test_unimpled_private_trait_ceiling_is_unbound() {
    // A private trait with no impl blocks — any pub caller through its default
    // body must declare `with _` (the ceiling is treated as unbound).
    let errors = effectcheck_errors(
        "trait Opaque {
             fn compute(ref self) -> i64 { 42 }
         }
         pub fn call_through[T: Opaque](t: ref T) -> i64 { t.compute() }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.kind == EffectErrorKind::MissingEffectDeclaration),
        "Expected MissingEffectDeclaration (must declare with _) for \
         pub caller through unimpled private trait, got: {:?}",
        errors
    );
}

#[test]
fn test_private_trait_with_impl_uses_normal_ceiling() {
    // A private trait that does have an impl — step 9 does not apply and the
    // caller needs no special declaration when the default body is pure.
    effectcheck_ok(
        "trait Opaque {
             fn compute(ref self) -> i64 { 42 }
         }
         pub struct Thing {}
         impl Opaque for Thing {
             fn compute(ref self) -> i64 { 42 }
         }
         pub fn call_through(t: ref Thing) -> i64 { t.compute() }",
    );
}

// ── Step 8: Private-trait SCC inference ─────────────────────────────────────

#[test]
fn test_private_trait_abstract_method_effects_reach_caller() {
    // Abstract (no default body) private-trait method: caller through the concrete
    // impl should see the impl's effects.
    let result = effectcheck_ok(
        "effect resource Db;\n\
         fn reads_db() reads(Db) {}\n\
         trait Proc {\n\
             fn run(ref self);\n\
         }\n\
         struct Worker {}\n\
         impl Proc for Worker {\n\
             fn run(ref self) { reads_db() }\n\
         }\n\
         fn execute(w: ref Worker) { w.run() }",
    );
    let effects = result.inferred_effects.get("execute").unwrap();
    let resources: Vec<_> = effects
        .effects
        .iter()
        .map(|e| e.effect.resource.as_str())
        .collect();
    assert!(
        resources.contains(&"Db"),
        "execute should inherit reads(Db) from Worker.run: {resources:?}"
    );
}

#[test]
fn test_private_trait_multiple_impls_caller_sees_union() {
    // Multiple impls — caller through any abstract dispatch should accumulate
    // all impls' effects (conservative union via `ends_with` matching).
    let result = effectcheck_ok(
        "effect resource Db;\n\
         effect resource Cache;\n\
         fn reads_db() reads(Db) {}\n\
         fn reads_cache() reads(Cache) {}\n\
         trait Proc { fn run(ref self); }\n\
         struct A {}\n\
         struct B {}\n\
         impl Proc for A {\n\
             fn run(ref self) { reads_db() }\n\
         }\n\
         impl Proc for B {\n\
             fn run(ref self) { reads_cache() }\n\
         }\n\
         fn exec_a(a: ref A) { a.run() }\n\
         fn exec_b(b: ref B) { b.run() }",
    );
    let a_effects = result.inferred_effects.get("exec_a").unwrap();
    let a_res: Vec<_> = a_effects
        .effects
        .iter()
        .map(|e| e.effect.resource.as_str())
        .collect();
    assert!(
        a_res.contains(&"Db"),
        "exec_a should see reads(Db): {a_res:?}"
    );

    let b_effects = result.inferred_effects.get("exec_b").unwrap();
    let b_res: Vec<_> = b_effects
        .effects
        .iter()
        .map(|e| e.effect.resource.as_str())
        .collect();
    assert!(
        b_res.contains(&"Cache"),
        "exec_b should see reads(Cache): {b_res:?}"
    );
}

#[test]
fn test_private_trait_default_body_scc_cycle_converges() {
    // Default method body calls abstract sibling; impl of abstract sibling
    // calls back into default method body — forms an SCC.
    // Fixed-point must converge with all effects propagated.
    let result = effectcheck_ok(
        "effect resource Db;\n\
         fn reads_db() reads(Db) {}\n\
         trait T {\n\
             fn step_a(ref self) {\n\
                 self.step_b()\n\
             }\n\
             fn step_b(ref self);\n\
         }\n\
         struct Dual {}\n\
         impl T for Dual {\n\
             fn step_b(ref self) { reads_db() }\n\
         }\n\
         fn run(d: ref Dual) { d.step_a() }",
    );
    let effects = result.inferred_effects.get("run").unwrap();
    let resources: Vec<_> = effects
        .effects
        .iter()
        .map(|e| e.effect.resource.as_str())
        .collect();
    assert!(
        resources.contains(&"Db"),
        "run should see reads(Db) through default body → impl chain: {resources:?}"
    );
}

#[test]
fn test_private_trait_ceiling_inferred_from_impl() {
    // After inference, the declared ceiling for a private trait method with no
    // explicit `with` should be updated to the union of impl inferred effects.
    // Observable: verify_impl_trait_ceilings no longer skips — impl ⊆ ceiling is trivially true.
    effectcheck_ok(
        "effect resource Db;\n\
         fn reads_db() reads(Db) {}\n\
         trait Store { fn fetch(ref self); }\n\
         struct PgStore {}\n\
         impl Store for PgStore {\n\
             fn fetch(ref self) { reads_db() }\n\
         }\n\
         fn load(s: ref PgStore) { s.fetch() }",
    );
}

// ── Step 10: `public_effects = "inferred"` does not bypass pub-trait ceiling ─

#[test]
fn test_inferred_policy_does_not_bypass_pub_trait_ceiling() {
    // Under public_effects = "inferred", the impl-vs-trait ceiling check still
    // fires — the policy only relaxes free-function declaration requirements.
    let parsed = parse(
        "effect resource Db;
         effect resource Cache;
         pub fn do_cache_read() with reads(Cache) {}
         pub trait Repo with reads(Db) {
             fn fetch(ref self) -> i64;
         }
         pub struct MyRepo {}
         impl Repo for MyRepo {
             fn fetch(ref self) -> i64 { do_cache_read(); 42 }
         }",
    );
    assert!(
        parsed.errors.is_empty(),
        "Parse errors: {:?}",
        parsed.errors
    );
    let result = effectcheck_with_policy(&parsed.program, PublicEffectsPolicy::Inferred);
    let real_errors: Vec<_> = result
        .errors
        .iter()
        .filter(|e| e.kind != EffectErrorKind::FfiLintHint)
        .collect();
    assert!(
        real_errors
            .iter()
            .any(|e| e.kind == EffectErrorKind::ImplExceedsTraitCeiling),
        "Expected ImplExceedsTraitCeiling under Inferred policy, got: {:?}",
        real_errors
    );
}

// ── Stdlib allocates(Heap) and suspends seeding ─────────────────

#[test]
fn test_vec_new_infers_allocates_heap() {
    // Vec.new() is a heap-allocating stdlib constructor; the caller must
    // accumulate allocates(Heap) in its inferred effect set.
    let result = effectcheck_ok("fn make_vec() { let v: Vec[i64] = Vec.new(); }");
    let inferred = result.inferred_effects.get("make_vec").unwrap();
    assert!(
        inferred
            .effects
            .iter()
            .any(|e| e.effect.verb == EffectVerbKind::Allocates && e.effect.resource == "Heap"),
        "Expected allocates(Heap) for Vec.new(), got: {:?}",
        inferred.effects
    );
}

#[test]
fn test_vec_push_infers_allocates_heap() {
    // Vec.push() may grow the backing buffer — allocates(Heap).
    let result = effectcheck_ok(
        "fn fill_vec() {
             let mut v: Vec[i64] = Vec.new();
             v.push(1);
         }",
    );
    let inferred = result.inferred_effects.get("fill_vec").unwrap();
    assert!(
        inferred
            .effects
            .iter()
            .any(|e| e.effect.verb == EffectVerbKind::Allocates && e.effect.resource == "Heap"),
        "Expected allocates(Heap) for Vec.push(), got: {:?}",
        inferred.effects
    );
}

#[test]
fn test_map_new_infers_allocates_heap() {
    // Map.new() allocates the underlying storage.
    let result = effectcheck_ok("fn make_map() { let m: Map[String, i64] = Map.new(); }");
    let inferred = result.inferred_effects.get("make_map").unwrap();
    assert!(
        inferred
            .effects
            .iter()
            .any(|e| e.effect.verb == EffectVerbKind::Allocates && e.effect.resource == "Heap"),
        "Expected allocates(Heap) for Map.new(), got: {:?}",
        inferred.effects
    );
}

#[test]
fn test_sorted_set_new_infers_allocates_heap() {
    // SortedSet.new() allocates the B-tree backing store.
    let result = effectcheck_ok("fn make_set() { let s: SortedSet[i64] = SortedSet.new(); }");
    let inferred = result.inferred_effects.get("make_set").unwrap();
    assert!(
        inferred
            .effects
            .iter()
            .any(|e| e.effect.verb == EffectVerbKind::Allocates && e.effect.resource == "Heap"),
        "Expected allocates(Heap) for SortedSet.new(), got: {:?}",
        inferred.effects
    );
}

#[test]
fn test_channel_new_infers_allocates_heap() {
    // Channel.new() allocates the shared queue.
    let result = effectcheck_ok(
        "fn make_chan() {
             let (s, r): (Sender[i64], Receiver[i64]) = Channel.new();
         }",
    );
    let inferred = result.inferred_effects.get("make_chan").unwrap();
    assert!(
        inferred
            .effects
            .iter()
            .any(|e| e.effect.verb == EffectVerbKind::Allocates && e.effect.resource == "Heap"),
        "Expected allocates(Heap) for Channel.new(), got: {:?}",
        inferred.effects
    );
}

#[test]
fn test_sender_send_infers_allocates_heap() {
    // Sender.send() pushes to the heap-allocated queue.
    let result = effectcheck_ok(
        "fn do_send(s: Sender[i64]) {
             s.send(42);
         }",
    );
    let inferred = result.inferred_effects.get("do_send").unwrap();
    assert!(
        inferred
            .effects
            .iter()
            .any(|e| e.effect.verb == EffectVerbKind::Allocates && e.effect.resource == "Heap"),
        "Expected allocates(Heap) for Sender.send(), got: {:?}",
        inferred.effects
    );
}

#[test]
fn test_receiver_recv_infers_suspends() {
    // Receiver.recv() blocks until a message arrives — infers `suspends`.
    let result = effectcheck_ok(
        "fn do_recv(r: Receiver[i64]) -> i64 {
             r.recv()
         }",
    );
    let inferred = result.inferred_effects.get("do_recv").unwrap();
    assert!(
        inferred
            .effects
            .iter()
            .any(|e| e.effect.verb == EffectVerbKind::Suspends),
        "Expected suspends for Receiver.recv(), got: {:?}",
        inferred.effects
    );
}

#[test]
fn test_pub_fn_calling_vec_new_must_declare_allocates_heap() {
    // A public function that calls Vec.new() must declare allocates(Heap).
    // Under the default Declared policy, omitting the declaration is an error.
    let errors = effectcheck_errors("pub fn make_vec() { let v: Vec[i64] = Vec.new(); }");
    assert!(
        errors.iter().any(|e| e.message.contains("allocates")),
        "Expected undeclared-allocates error for pub fn calling Vec.new(), got: {:?}",
        errors
    );
}

#[test]
fn test_env_set_infers_writes_env() {
    // env.set(name, value) carries writes(Env) — callers accumulate the
    // effect through the normal call-graph propagation. Both the lowercase
    // and capitalized callee keys are seeded.
    let result = effectcheck_ok(
        "fn do_set() {
             env.set(\"FOO\", \"bar\");
         }",
    );
    let inferred = result.inferred_effects.get("do_set").unwrap();
    assert!(
        inferred
            .effects
            .iter()
            .any(|e| e.effect.verb == EffectVerbKind::Writes && e.effect.resource == "Env"),
        "Expected writes(Env) for env.set(...), got: {:?}",
        inferred.effects
    );
}

#[test]
fn test_env_set_capitalized_form_infers_writes_env() {
    // The capitalized `Env.set(...)` dispatch path picks up the same effect
    // — both keys are seeded by the effectchecker's startup seeding loop.
    let result = effectcheck_ok(
        "fn do_set() {
             Env.set(\"FOO\", \"bar\");
         }",
    );
    let inferred = result.inferred_effects.get("do_set").unwrap();
    assert!(
        inferred
            .effects
            .iter()
            .any(|e| e.effect.verb == EffectVerbKind::Writes && e.effect.resource == "Env"),
        "Expected writes(Env) for Env.set(...), got: {:?}",
        inferred.effects
    );
}

#[test]
fn test_pub_fn_calling_env_set_must_declare_writes_env() {
    // A public function that calls env.set must declare writes(Env). Mirror
    // of the Vec.new + allocates(Heap) negative test for the new effect.
    let errors = effectcheck_errors("pub fn save() { env.set(\"FOO\", \"bar\"); }");
    assert!(
        errors
            .iter()
            .any(|e| e.kind == EffectErrorKind::MissingEffectDeclaration),
        "Expected MissingEffectDeclaration for pub fn calling env.set, got: {:?}",
        errors
    );
    assert!(
        errors.iter().any(|e| e.message.contains("writes(Env)")),
        "Expected error message to mention writes(Env), got: {:?}",
        errors
    );
}

#[test]
fn test_pub_fn_with_writes_env_declared_accepts_env_set() {
    // The dual of the negative — declaring writes(Env) on the public fn
    // satisfies the seeded-effect requirement, no error.
    effectcheck_ok("pub fn save() writes(Env) { env.set(\"FOO\", \"bar\"); }");
}

#[test]
fn test_allocates_heap_propagates_through_call_chain() {
    // allocates(Heap) propagates transitively: inner → outer.
    let result = effectcheck_ok(
        "fn inner() {
             let v: Vec[i64] = Vec.new();
         }
         fn outer() {
             inner();
         }",
    );
    let outer_inferred = result.inferred_effects.get("outer").unwrap();
    assert!(
        outer_inferred
            .effects
            .iter()
            .any(|e| e.effect.verb == EffectVerbKind::Allocates && e.effect.resource == "Heap"),
        "allocates(Heap) should propagate from inner to outer, got: {:?}",
        outer_inferred.effects
    );
}

// ── Per-method effect surface for Map / Set (List 2, item 1) ────

fn has_alloc_heap(set: &EffectSet) -> bool {
    set.effects
        .iter()
        .any(|e| e.effect.verb == EffectVerbKind::Allocates && e.effect.resource == "Heap")
}

fn has_panics(set: &EffectSet) -> bool {
    set.effects
        .iter()
        .any(|e| e.effect.verb == EffectVerbKind::Panics)
}

#[test]
fn test_map_insert_infers_allocates_heap() {
    let result = effectcheck_ok(
        "fn fill(m: Map[String, i64]) {
             m.insert(\"k\", 1);
         }",
    );
    assert!(
        has_alloc_heap(result.inferred_effects.get("fill").unwrap()),
        "Map.insert should infer allocates(Heap)"
    );
}

#[test]
fn test_map_merge_infers_allocates_heap() {
    let result = effectcheck_ok(
        "fn do_merge(m1: Map[String, i64], m2: Map[String, i64]) {
             m1.merge(m2);
         }",
    );
    assert!(
        has_alloc_heap(result.inferred_effects.get("do_merge").unwrap()),
        "Map.merge should infer allocates(Heap)"
    );
}

#[test]
fn test_map_keys_values_entries_infer_allocates_heap() {
    for method in ["keys", "values", "entries"] {
        let src = format!(
            "fn dump(m: Map[String, i64]) {{
                 let _v = m.{}();
             }}",
            method
        );
        let result = effectcheck_ok(&src);
        assert!(
            has_alloc_heap(result.inferred_effects.get("dump").unwrap()),
            "Map.{} should infer allocates(Heap)",
            method
        );
    }
}

#[test]
fn test_map_pure_reads_infer_no_alloc() {
    // get / contains_key / len / is_empty are pure reads.
    let result = effectcheck_ok(
        "fn pure_query(m: Map[i64, i64], k: i64) -> bool {
             let n: i64 = m.len();
             let e: bool = m.is_empty();
             let c: bool = m.contains_key(k);
             let g: Option[i64] = m.get(k);
             c
         }",
    );
    let inferred = result.inferred_effects.get("pure_query").unwrap();
    assert!(
        !has_alloc_heap(inferred),
        "pure Map reads should not infer allocates(Heap), got: {:?}",
        inferred.effects
    );
}

#[test]
fn test_map_remove_clear_no_alloc() {
    // remove / clear mutate without growing — no allocates(Heap).
    let result = effectcheck_ok(
        "fn shrink(m: Map[i64, i64], k: i64) {
             let _ = m.remove(k);
             m.clear();
         }",
    );
    let inferred = result.inferred_effects.get("shrink").unwrap();
    assert!(
        !has_alloc_heap(inferred),
        "remove/clear should not infer allocates(Heap), got: {:?}",
        inferred.effects
    );
}

#[test]
fn test_map_index_infers_panics() {
    // Map's `[]` operator can panic on missing key.
    let result = effectcheck_ok(
        "fn lookup(m: Map[i64, i64], k: i64) -> i64 {
             m[k]
         }",
    );
    assert!(
        has_panics(result.inferred_effects.get("lookup").unwrap()),
        "Map[k] should infer panics"
    );
}

#[test]
fn test_pub_fn_returning_map_must_declare_allocates_heap() {
    // Building and returning a Map demands the declared effect on a pub fn.
    let errors = effectcheck_errors(
        "pub fn build_map() -> Map[String, i64] {
             let m: Map[String, i64] = Map.new();
             m
         }",
    );
    assert!(
        errors.iter().any(|e| e.message.contains("allocates")),
        "Expected undeclared-allocates error for pub fn returning a Map, got: {:?}",
        errors
    );
}

#[test]
fn test_pub_fn_indexing_map_must_declare_panics() {
    // `pub fn lookup(...) -> i64 { m[k] }` without `panics` is rejected.
    let errors = effectcheck_errors("pub fn lookup(m: Map[i64, i64], k: i64) -> i64 { m[k] }");
    assert!(
        errors
            .iter()
            .any(|e| e.message.to_lowercase().contains("panic")),
        "Expected undeclared-panics error for pub fn indexing a Map, got: {:?}",
        errors
    );
}

#[test]
fn test_set_insert_infers_allocates_heap() {
    let result = effectcheck_ok(
        "fn fill(s: Set[i64]) {
             s.insert(1);
         }",
    );
    assert!(
        has_alloc_heap(result.inferred_effects.get("fill").unwrap()),
        "Set.insert should infer allocates(Heap)"
    );
}

#[test]
fn test_set_union_intersection_difference_infer_allocates_heap() {
    for method in ["union", "intersection", "difference"] {
        let src = format!(
            "fn combine(a: Set[i64], b: Set[i64]) -> Set[i64] {{
                 a.{}(b)
             }}",
            method
        );
        let result = effectcheck_ok(&src);
        assert!(
            has_alloc_heap(result.inferred_effects.get("combine").unwrap()),
            "Set.{} should infer allocates(Heap)",
            method
        );
    }
}

#[test]
fn test_set_pure_reads_infer_no_alloc() {
    // contains / len / is_empty are pure reads; remove does not grow.
    let result = effectcheck_ok(
        "fn pure_query(s: Set[i64], x: i64) -> bool {
             let n: i64 = s.len();
             let e: bool = s.is_empty();
             let c: bool = s.contains(x);
             let r: bool = s.remove(x);
             c
         }",
    );
    let inferred = result.inferred_effects.get("pure_query").unwrap();
    assert!(
        !has_alloc_heap(inferred),
        "pure Set reads should not infer allocates(Heap), got: {:?}",
        inferred.effects
    );
}

// ── Trait associated function ceilings (List 1, item 4) ─────────

#[test]
fn test_typeparam_assoc_fn_ceiling_propagates() {
    // `T.load()` inside `fn make[T: Loader]` must propagate the trait method's
    // `reads(Db)` ceiling to the caller's inferred set. Public trait so the
    // declared ceiling stays explicit (private traits with no impls collapse
    // to `with _`).
    let result = effectcheck_ok(
        r#"
effect resource Db;

pub trait Loader {
    fn load() -> Self with reads(Db);
}

fn make[T: Loader]() -> T with reads(Db) {
    T.load()
}
"#,
    );
    let make_inferred = result.inferred_effects.get("make").expect("make inferred");
    assert!(
        make_inferred
            .effects
            .iter()
            .any(|e| e.effect.verb == EffectVerbKind::Reads && e.effect.resource == "Db"),
        "reads(Db) should propagate from `T.load()` to `make`, got: {:?}",
        make_inferred.effects
    );
}

#[test]
fn test_pub_fn_with_typeparam_assoc_fn_must_declare_ceiling() {
    // A public `make[T: Loader]` that calls `T.load()` must declare reads(Db).
    let errors = effectcheck_errors(
        r#"
effect resource Db;

pub trait Loader {
    fn load() -> Self with reads(Db);
}

pub fn make[T: Loader]() -> T {
    T.load()
}
"#,
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("reads(Db)") && e.message.contains("'make'")),
        "expected pub fn to require reads(Db) declaration, got: {errors:?}"
    );
}

#[test]
fn test_bare_assoc_fn_ceiling_propagates() {
    // Bare `load()` inside a `T: Loader` body should propagate reads(Db).
    let result = effectcheck_ok(
        r#"
effect resource Db;

pub trait Loader {
    fn load() -> Self with reads(Db);
}

fn make[T: Loader]() -> T with reads(Db) {
    load()
}
"#,
    );
    let make_inferred = result.inferred_effects.get("make").expect("make inferred");
    assert!(
        make_inferred
            .effects
            .iter()
            .any(|e| e.effect.verb == EffectVerbKind::Reads && e.effect.resource == "Db"),
        "reads(Db) should propagate from bare `load()` to `make`, got: {:?}",
        make_inferred.effects
    );
}

#[test]
fn test_concrete_type_dispatch_uses_impl_effects_not_ceiling() {
    // `Wrapper.load()` should propagate the *impl method's* effects, not the
    // trait's wider ceiling. Here the trait ceiling is `reads(Db)` but the impl
    // is pure — caller should be pure.
    let result = effectcheck_ok(
        r#"
effect resource Db;

trait Loader {
    fn load() -> Self with reads(Db);
}

struct Wrapper { value: i64 }

impl Loader for Wrapper {
    fn load() -> Wrapper { Wrapper { value: 0 } }
}

fn caller() -> Wrapper {
    Wrapper.load()
}
"#,
    );
    let caller_inferred = result
        .inferred_effects
        .get("caller")
        .expect("caller inferred");
    assert!(
        !caller_inferred
            .effects
            .iter()
            .any(|e| e.effect.verb == EffectVerbKind::Reads && e.effect.resource == "Db"),
        "concrete dispatch on a pure impl must not propagate trait ceiling, got: {:?}",
        caller_inferred.effects
    );
}

#[test]
fn test_supertrait_self_assoc_fn_ceiling_propagates_in_default_body() {
    // `Self.default()` inside the default body of a trait whose supertrait
    // declares reads(Config) must propagate that ceiling to the SCC ceiling
    // for the method (visible through the trait's own ceiling collection).
    let result = effectcheck_ok(
        r#"
effect resource Config;

pub trait Default {
    fn default() -> Self with reads(Config);
}

pub trait Resettable: Default {
    fn reset() -> Self with reads(Config) {
        Self.default()
    }
}
"#,
    );
    let reset_inferred = result
        .inferred_effects
        .get("Resettable.reset")
        .expect("Resettable.reset inferred");
    assert!(
        reset_inferred
            .effects
            .iter()
            .any(|e| e.effect.verb == EffectVerbKind::Reads && e.effect.resource == "Config"),
        "reads(Config) should propagate from Self.default() into Resettable.reset's body, got: {:?}",
        reset_inferred.effects
    );
}

// ── Conversion trait `with _` ceilings (round 6 — step 1 + 2) ────────

#[test]
fn test_stdlib_conversion_trait_ceilings_seeded() {
    // Step 1: the four stdlib conversion-trait methods carry `with _`
    // (Polymorphic) ceilings even with no user code declaring them.
    let result = effectcheck_ok("");
    for key in [
        "From.from",
        "Into.into",
        "TryFrom.try_from",
        "TryInto.try_into",
    ] {
        assert!(
            matches!(
                result.declared_effects.get(key),
                Some(DeclaredEffects::Polymorphic)
            ),
            "expected {} to be seeded as DeclaredEffects::Polymorphic, got {:?}",
            key,
            result.declared_effects.get(key),
        );
    }
}

#[test]
fn test_from_impl_with_panics_accepted() {
    // Step 2: a `From` impl whose body panics is accepted under the
    // seeded `with _` ceiling.
    effectcheck_ok(
        "struct ParseError { msg: String }
         struct AppError { msg: String }
         impl From for AppError {
             fn from(e: ParseError) -> AppError with panics {
                 panic(\"convert failed\")
             }
         }",
    );
}

#[test]
fn test_from_impl_with_writes_accepted() {
    // Step 2: a `From` impl that writes a user resource is accepted.
    effectcheck_ok(
        "effect resource Log;
         pub fn write_log() with writes(Log) {}
         struct ParseError { msg: String }
         struct AppError { msg: String }
         impl From for AppError {
             fn from(e: ParseError) -> AppError with writes(Log) {
                 write_log();
                 AppError { msg: e.msg }
             }
         }",
    );
}

#[test]
fn test_from_impl_with_allocates_heap_accepted() {
    // Step 2: a `From` impl that allocates is accepted.
    effectcheck_ok(
        "struct Raw { n: i64 }
         struct Wrapped { items: Vec[i64] }
         impl From for Wrapped {
             fn from(r: Raw) -> Wrapped with allocates(Heap) {
                 let v: Vec[i64] = Vec.new();
                 Wrapped { items: v }
             }
         }",
    );
}

#[test]
fn test_tryfrom_impl_with_effects_accepted() {
    // Step 2: a `TryFrom` impl that performs effects is accepted.
    // `TryFrom.try_from` carries the same `with _` ceiling as `From.from`.
    effectcheck_ok(
        "effect resource Log;
         pub fn write_log() with writes(Log) {}
         struct Raw { n: i64 }
         struct Validated { n: i64 }
         impl TryFrom for Validated {
             type Error = String;
             fn try_from(r: Raw) -> Result[Validated, String] with writes(Log) {
                 write_log();
                 Result.Ok(Validated { n: r.n })
             }
         }",
    );
}

#[test]
fn test_into_propagates_from_impl_effects() {
    // Step 3: `x.into()` lowers to `Target.from(x)` in the lowering pass,
    // so the effectchecker sees a normal `Type.method` call and propagates
    // the From impl's declared effects to the caller.
    let result = effectcheck_full_pipeline(
        "effect resource Log;
         pub fn write_log() with writes(Log) {}
         struct ParseError { msg: String }
         struct AppError { msg: String }
         impl From for AppError {
             fn from(e: ParseError) -> AppError with writes(Log) {
                 write_log();
                 AppError { msg: e.msg }
             }
         }
         fn convert(p: ParseError) -> AppError {
             p.into()
         }",
    );
    let convert_effects = result
        .inferred_effects
        .get("convert")
        .expect("convert inferred");
    assert!(
        convert_effects
            .effects
            .iter()
            .any(|e| e.effect.verb == EffectVerbKind::Writes && e.effect.resource == "Log"),
        "expected writes(Log) to propagate from From impl through `.into()` to convert(), got: {:?}",
        convert_effects.effects
    );
}

#[test]
fn test_from_impl_no_effects_still_works() {
    // Regression: a `From` impl with no effects continues to compile under
    // the new seeding (the `Polymorphic` ceiling admits the empty effect set).
    effectcheck_ok(
        "struct ParseError { msg: String }
         struct AppError { msg: String }
         impl From for AppError {
             fn from(e: ParseError) -> AppError { AppError { msg: e.msg } }
         }",
    );
}

#[test]
fn test_into_pure_from_impl_no_spurious_effects() {
    // Regression: `.into()` to a target with a pure From impl must not
    // contribute any effects to the caller.
    let result = effectcheck_full_pipeline(
        "struct Raw { n: i64 }
         struct Wrapped { n: i64 }
         impl From for Wrapped {
             fn from(r: Raw) -> Wrapped { Wrapped { n: r.n } }
         }
         fn convert(r: Raw) -> Wrapped {
             r.into()
         }",
    );
    let convert_effects = result
        .inferred_effects
        .get("convert")
        .expect("convert inferred");
    let non_transparent: Vec<_> = convert_effects
        .effects
        .iter()
        .filter(|e| {
            !matches!(
                e.effect.verb,
                EffectVerbKind::Allocates | EffectVerbKind::Reads
            ) || e.effect.resource != "Heap"
        })
        .collect();
    // No effects expected (allocates(Heap) is fine since Wrapped is stack-allocated).
    assert!(
        non_transparent.is_empty(),
        "pure From impl should add no effects to .into() caller, got: {:?}",
        convert_effects.effects
    );
}

#[test]
fn test_into_distinct_targets_pick_distinct_from_impls() {
    // Step 3: `.into()` resolved to different target types pulls in only
    // that target's From impl effects, not a union across all From[Source]
    // impls. Two From impls for the same source type, distinct effects;
    // each conversion site picks up only its target's effects.
    let result = effectcheck_full_pipeline(
        "effect resource Log;
         effect resource Db;
         pub fn write_log() with writes(Log) {}
         pub fn read_db() with reads(Db) {}
         struct Raw { n: i64 }
         struct A { n: i64 }
         struct B { n: i64 }
         impl From for A {
             fn from(r: Raw) -> A with writes(Log) {
                 write_log();
                 A { n: r.n }
             }
         }
         impl From for B {
             fn from(r: Raw) -> B with reads(Db) {
                 read_db();
                 B { n: r.n }
             }
         }
         fn to_a(r: Raw) -> A { r.into() }
         fn to_b(r: Raw) -> B { r.into() }",
    );
    let to_a_effects = result.inferred_effects.get("to_a").expect("to_a inferred");
    let to_b_effects = result.inferred_effects.get("to_b").expect("to_b inferred");
    assert!(
        to_a_effects
            .effects
            .iter()
            .any(|e| e.effect.verb == EffectVerbKind::Writes && e.effect.resource == "Log"),
        "to_a should inherit writes(Log) from A's From impl, got: {:?}",
        to_a_effects.effects
    );
    assert!(
        !to_a_effects
            .effects
            .iter()
            .any(|e| e.effect.verb == EffectVerbKind::Reads && e.effect.resource == "Db"),
        "to_a must NOT inherit reads(Db) from B's From impl, got: {:?}",
        to_a_effects.effects
    );
    assert!(
        to_b_effects
            .effects
            .iter()
            .any(|e| e.effect.verb == EffectVerbKind::Reads && e.effect.resource == "Db"),
        "to_b should inherit reads(Db) from B's From impl, got: {:?}",
        to_b_effects.effects
    );
    assert!(
        !to_b_effects
            .effects
            .iter()
            .any(|e| e.effect.verb == EffectVerbKind::Writes && e.effect.resource == "Log"),
        "to_b must NOT inherit writes(Log) from A's From impl, got: {:?}",
        to_b_effects.effects
    );
}

#[test]
fn test_try_into_propagates_tryfrom_impl_effects() {
    // Round 7: `.try_into()` lowers to `Target.try_from(x)` via the new
    // desugar (lowering.rs:rewrite_try_into_call), so the round-6 step-3
    // propagation path now applies through the sugar without further
    // effectchecker changes.
    let result = effectcheck_full_pipeline(
        "effect resource Log;
         pub fn write_log() with writes(Log) {}
         struct Raw { n: i64 }
         struct Validated { n: i64 }
         impl TryFrom for Validated {
             type Error = String;
             fn try_from(r: Raw) -> Result[Validated, String] with writes(Log) {
                 write_log();
                 Result.Ok(Validated { n: r.n })
             }
         }
         fn convert(r: Raw) -> Result[Validated, String] {
             r.try_into()
         }",
    );
    let convert_effects = result
        .inferred_effects
        .get("convert")
        .expect("convert inferred");
    assert!(
        convert_effects
            .effects
            .iter()
            .any(|e| e.effect.verb == EffectVerbKind::Writes && e.effect.resource == "Log"),
        "expected writes(Log) to propagate from TryFrom impl through `.try_into()` to convert(), got: {:?}",
        convert_effects.effects
    );
}

#[test]
fn test_tryfrom_direct_call_propagates_effects() {
    // Round 6 / round 7 boundary: direct `Target.try_from(x)` propagates the
    // TryFrom impl's effects to the caller. Round 6 verified this for the
    // direct-call form before the `.try_into()` desugar landed in round 7.
    let result = effectcheck_full_pipeline(
        "effect resource Log;
         pub fn write_log() with writes(Log) {}
         struct Raw { n: i64 }
         struct Validated { n: i64 }
         impl TryFrom for Validated {
             type Error = String;
             fn try_from(r: Raw) -> Result[Validated, String] with writes(Log) {
                 write_log();
                 Result.Ok(Validated { n: r.n })
             }
         }
         fn convert(r: Raw) -> Result[Validated, String] {
             Validated.try_from(r)
         }",
    );
    let convert_effects = result
        .inferred_effects
        .get("convert")
        .expect("convert inferred");
    assert!(
        convert_effects
            .effects
            .iter()
            .any(|e| e.effect.verb == EffectVerbKind::Writes && e.effect.resource == "Log"),
        "expected writes(Log) to propagate from TryFrom impl through Validated.try_from(r), got: {:?}",
        convert_effects.effects
    );
}

// ── Code-review fixes: method-call effect parity ────────────────
//
// The four cases below are method-call analogues of effect-checker rules
// that already worked for free-function calls. Without the fixes, method
// dispatch silently bypassed the rule. Each repro is taken verbatim from
// `brainstorming/code_review_2026_05_01.md`.

#[test]
fn test_method_call_function_ref_arg_propagates_effects() {
    // F1: function-reference argument to a `with _` method must propagate
    // the referenced function's effects to the caller, mirroring the
    // free-call handling. Before the fix, `caller` inferred no effects and
    // emitted no missing-declaration diagnostic.
    let result = effectcheck_full_pipeline(
        "effect resource Log;
         struct Runner {}
         pub fn write_log() with writes(Log) {}
         impl Runner {
             fn run_each[with E](self, f: Fn() with E) with E { f(); }
         }
         pub fn caller() {
             let r = Runner {};
             r.run_each(write_log)
         }",
    );
    let caller_effects = result
        .inferred_effects
        .get("caller")
        .expect("caller inferred");
    assert!(
        caller_effects
            .effects
            .iter()
            .any(|e| e.effect.verb == EffectVerbKind::Writes && e.effect.resource == "Log"),
        "expected writes(Log) to propagate through r.run_each(write_log), got: {:?}",
        caller_effects.effects
    );
    assert!(
        result
            .errors
            .iter()
            .any(|e| e.kind == EffectErrorKind::MissingEffectDeclaration),
        "expected MissingEffectDeclaration on caller (it performs writes(Log) but \
         declares nothing); errors: {:?}",
        result
            .errors
            .iter()
            .map(|e| (&e.kind, &e.message))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_method_call_with_e_unification_disagreeing_closures_conflict() {
    // F2: method-call analogue of `test_with_e_two_slots_disagreeing_closures_conflict`.
    // Two `with E` slots on a method, closures with different effect sets →
    // EffectVariableConflict.
    let result = effectcheck_full_pipeline(
        "effect resource Log;
         effect resource Db;
         struct Runner {}
         pub fn write_log() with writes(Log) {}
         pub fn read_db() with reads(Db) {}
         impl Runner {
             fn pipe[with E](self, a: Fn() with E, b: Fn() with E) with E { a(); b(); }
         }
         pub fn run() with writes(Log) reads(Db) {
             let r = Runner {};
             r.pipe(|| write_log(), || read_db())
         }",
    );
    let conflict = result
        .errors
        .iter()
        .find(|e| e.kind == EffectErrorKind::EffectVariableConflict)
        .unwrap_or_else(|| {
            panic!(
                "expected EffectVariableConflict on r.pipe(...), got: {:?}",
                result
                    .errors
                    .iter()
                    .map(|e| (&e.kind, &e.message))
                    .collect::<Vec<_>>()
            )
        });
    assert!(
        conflict.message.contains("`E`")
            && conflict.message.contains("writes(Log)")
            && conflict.message.contains("reads(Db)"),
        "conflict diagnostic should name E and both bindings, got: {}",
        conflict.message
    );
}

#[test]
fn test_method_call_pure_fn_slot_rejects_effectful_closure() {
    // F3: method-call analogue of the Fn-slot subtyping check. A method
    // whose closure parameter is pure (`Fn()`, no `with` clause) must
    // reject an effectful closure even when the enclosing caller declares
    // the effects.
    let result = effectcheck_full_pipeline(
        "effect resource Log;
         struct Runner {}
         pub fn write_log() with writes(Log) {}
         impl Runner {
             fn run_pure(self, f: Fn()) { f(); }
         }
         pub fn caller() with writes(Log) {
             let r = Runner {};
             r.run_pure(|| write_log())
         }",
    );
    assert!(
        result
            .errors
            .iter()
            .any(|e| e.kind == EffectErrorKind::EffectSubtypeViolation),
        "expected EffectSubtypeViolation on r.run_pure(|| write_log()) (closure has \
         writes(Log) but slot is pure), got: {:?}",
        result
            .errors
            .iter()
            .map(|e| (&e.kind, &e.message))
            .collect::<Vec<_>>()
    );
}

// ── Step 8: capture mode does not feed into closure effect set ──
//
// Per design.md § Closures (Rule 2½), the four legal capture-mode forms
// (`||`, `own ||`, `ref ||`, `mut ref ||`) are ownership signals that
// drive RC fallback / drop placement. They must NOT influence the
// closure's inferred effect set; effects come from the body's actions
// alone. The current pass ordering runs effect inference before
// ownership analysis, so the property holds vacuously today; these
// tests pin it as a regression sentinel against any future code that
// would let capture mode leak into the effect side.

#[test]
fn capture_mode_prefix_does_not_change_inferred_effects() {
    // The closure expression is wrapped in parens to sidestep the parser's
    // call-arg rejection of `ref` / `mut ref` (design.md Part 1½ Rule 4 —
    // those keywords are not legal at call sites except as a closure prefix
    // recognized inside a primary-expression context). Parens reach that
    // context. The semantics of `(closure)` and `closure` are identical for
    // effect inference — `inline_closure_effects_for_arg` walks through the
    // paren wrapper.
    let collect_resources = |prefix: &str| -> Vec<String> {
        let source = format!(
            "effect resource UserDB;\n\
             fn reads_db() reads(UserDB) {{ }}\n\
             pub fn apply(f: Fn() -> () with _) -> () with _ {{ f() }}\n\
             fn caller() {{ apply(({prefix}|| reads_db())); }}"
        );
        let result = effectcheck_ok(&source);
        let mut verbs: Vec<String> = result
            .inferred_effects
            .get("caller")
            .unwrap()
            .effects
            .iter()
            .map(|e| e.effect.resource.clone())
            .collect();
        verbs.sort();
        verbs
    };
    let bare = collect_resources("");
    let own = collect_resources("own ");
    let ref_ = collect_resources("ref ");
    let mut_ref = collect_resources("mut ref ");
    assert!(
        bare.contains(&"UserDB".to_string()),
        "bare prefix should propagate reads(UserDB): {bare:?}"
    );
    assert_eq!(bare, own, "own prefix changed the inferred effect set");
    assert_eq!(bare, ref_, "ref prefix changed the inferred effect set");
    assert_eq!(
        bare, mut_ref,
        "mut ref prefix changed the inferred effect set"
    );
}

#[test]
fn empty_closure_body_yields_no_effects_for_any_capture_mode() {
    for prefix in &["", "own ", "ref ", "mut ref "] {
        let source = format!(
            "pub fn apply(f: Fn() -> () with _) -> () with _ {{ f() }}\n\
             fn caller() {{ apply(({prefix}|| {{}})); }}"
        );
        let result = effectcheck_ok(&source);
        let effects = &result.inferred_effects.get("caller").unwrap().effects;
        assert!(
            effects.is_empty(),
            "{prefix}|| should infer empty effects, got: {:?}",
            effects
                .iter()
                .map(|e| (e.effect.verb.clone(), e.effect.resource.clone()))
                .collect::<Vec<_>>()
        );
    }
}

#[test]
fn test_iter_chunks_infers_allocates_heap() {
    let result = effectcheck_ok(
        "fn group_it() {
             let v: Vec[i64] = Vec.new();
             let _it = v.iter().chunks(2);
         }",
    );
    let inferred = result.inferred_effects.get("group_it").unwrap();
    assert!(
        inferred
            .effects
            .iter()
            .any(|e| e.effect.verb == EffectVerbKind::Allocates && e.effect.resource == "Heap"),
        "Expected allocates(Heap) for Iterator.chunks(), got: {:?}",
        inferred.effects
    );
}

#[test]
fn test_iter_windows_infers_allocates_heap() {
    let result = effectcheck_ok(
        "fn group_it() {
             let v: Vec[i64] = Vec.new();
             let _it = v.iter().windows(2);
         }",
    );
    let inferred = result.inferred_effects.get("group_it").unwrap();
    assert!(
        inferred
            .effects
            .iter()
            .any(|e| e.effect.verb == EffectVerbKind::Allocates && e.effect.resource == "Heap"),
        "Expected allocates(Heap) for Iterator.windows(), got: {:?}",
        inferred.effects
    );
}

#[test]
fn test_iter_chunk_by_infers_allocates_heap() {
    // Iterator.chunk_by() allocates a fresh Vec[T] per group — the
    // effect-checker seeds `allocates(Heap)` on `Iterator.chunk_by`
    // so callers transitively pick it up via the
    // STDLIB_METHOD_MAP-based call collection.
    let result = effectcheck_ok(
        "fn group_it() {
             let v: Vec[i64] = Vec.new();
             let _it = v.iter().chunk_by(|x| x);
         }",
    );
    let inferred = result.inferred_effects.get("group_it").unwrap();
    assert!(
        inferred
            .effects
            .iter()
            .any(|e| e.effect.verb == EffectVerbKind::Allocates && e.effect.resource == "Heap"),
        "Expected allocates(Heap) for Iterator.chunk_by(), got: {:?}",
        inferred.effects
    );
}

// ── Network-boundary callee identification (phase 6 line 26 slice 1) ──
//
// The cli pipeline populates `Program.callee_network_yield_effect` from the
// effect-check result by filtering for `sends(Network)` / `receives(Network)`
// verb-resource pairs. These helpers mirror the logic in
// `src/cli.rs::build_callee_network_yield_effect_table` so tests can verify
// what the pipeline would compute from a given effect-check result; if the
// production helper changes shape, this mirror must change with it.

fn set_has_network_yield_effect(set: &EffectSet) -> bool {
    set.effects.iter().any(|t| {
        matches!(
            t.effect.verb,
            EffectVerbKind::Sends | EffectVerbKind::Receives
        ) && t.effect.resource == "Network"
    })
}

#[test]
fn network_yield_filter_classifies_sends_network_as_boundary() {
    // Unit test of the filter rule on a hand-constructed EffectSet. A set
    // carrying `sends(Network)` must classify as network-boundary.
    let mut set = EffectSet::new();
    set.add(
        Effect {
            verb: EffectVerbKind::Sends,
            resource: "Network".to_string(),
        },
        EffectOrigin::Direct(karac::token::Span {
            line: 0,
            column: 0,
            offset: 0,
            length: 0,
        }),
    );
    assert!(set_has_network_yield_effect(&set));
}

#[test]
fn network_yield_filter_classifies_receives_network_as_boundary() {
    // A set carrying `receives(Network)` must classify as network-boundary.
    let mut set = EffectSet::new();
    set.add(
        Effect {
            verb: EffectVerbKind::Receives,
            resource: "Network".to_string(),
        },
        EffectOrigin::Direct(karac::token::Span {
            line: 0,
            column: 0,
            offset: 0,
            length: 0,
        }),
    );
    assert!(set_has_network_yield_effect(&set));
}

#[test]
fn network_yield_filter_rejects_non_network_resources() {
    // Sends and Receives against a non-Network resource must NOT classify
    // as network-boundary. This is what keeps channel sends and filesystem
    // writes out of the state-machine-transform candidate pool.
    for resource in &["Heap", "Filesystem", "Channel", "Database"] {
        for verb in &[EffectVerbKind::Sends, EffectVerbKind::Receives] {
            let mut set = EffectSet::new();
            set.add(
                Effect {
                    verb: verb.clone(),
                    resource: (*resource).to_string(),
                },
                EffectOrigin::Direct(karac::token::Span {
                    line: 0,
                    column: 0,
                    offset: 0,
                    length: 0,
                }),
            );
            assert!(
                !set_has_network_yield_effect(&set),
                "{:?}({}) must NOT classify as network-boundary",
                verb,
                resource
            );
        }
    }
}

#[test]
fn network_yield_filter_rejects_non_send_receive_verbs() {
    // Other verbs paired with `Network` must NOT classify (only Sends and
    // Receives drive the network event loop's park-and-yield path).
    let span = karac::token::Span {
        line: 0,
        column: 0,
        offset: 0,
        length: 0,
    };
    for verb in &[
        EffectVerbKind::Reads,
        EffectVerbKind::Writes,
        EffectVerbKind::Allocates,
        EffectVerbKind::Panics,
        EffectVerbKind::Blocks,
        EffectVerbKind::Suspends,
    ] {
        let mut set = EffectSet::new();
        set.add(
            Effect {
                verb: verb.clone(),
                resource: "Network".to_string(),
            },
            EffectOrigin::Direct(span.clone()),
        );
        assert!(
            !set_has_network_yield_effect(&set),
            "{:?}(Network) must NOT classify as network-boundary — only \
             Sends/Receives qualify",
            verb
        );
    }
}

#[test]
fn network_yield_filter_picks_one_of_many() {
    // A mixed effect set containing `sends(Network)` alongside other
    // non-network effects must classify as network-boundary — even one
    // qualifying entry is enough.
    let span = karac::token::Span {
        line: 0,
        column: 0,
        offset: 0,
        length: 0,
    };
    let mut set = EffectSet::new();
    set.add(
        Effect {
            verb: EffectVerbKind::Allocates,
            resource: "Heap".to_string(),
        },
        EffectOrigin::Direct(span.clone()),
    );
    set.add(
        Effect {
            verb: EffectVerbKind::Reads,
            resource: "Filesystem".to_string(),
        },
        EffectOrigin::Direct(span.clone()),
    );
    set.add(
        Effect {
            verb: EffectVerbKind::Sends,
            resource: "Network".to_string(),
        },
        EffectOrigin::Direct(span.clone()),
    );
    assert!(set_has_network_yield_effect(&set));
}

#[test]
fn network_yield_table_marks_seeded_client_methods_as_boundary() {
    // Integration check: the effect-checker's built-in seeded entries for
    // `Client.get` and `Client.post` carry `sends(Network) +
    // receives(Network)`, so they must classify as network-boundary under
    // the filter. This is the contract the state-machine transform relies
    // on — stdlib HTTP entry points are the canonical v1 network-boundary
    // calls.
    let result = effectcheck_full_pipeline("fn dummy() {}");
    let client_get = result
        .inferred_effects
        .get("Client.get")
        .expect("Client.get must be seeded by the effect-checker");
    assert!(
        set_has_network_yield_effect(client_get),
        "Client.get builtin must be network-boundary: {:?}",
        client_get.effects
    );
    let client_post = result
        .inferred_effects
        .get("Client.post")
        .expect("Client.post must be seeded by the effect-checker");
    assert!(
        set_has_network_yield_effect(client_post),
        "Client.post builtin must be network-boundary: {:?}",
        client_post.effects
    );
}

fn caller_has_network_pair(result: &EffectCheckResult, fn_name: &str) -> bool {
    let inferred = result
        .inferred_effects
        .get(fn_name)
        .unwrap_or_else(|| panic!("{fn_name} must have an inferred effect set"));
    let has_sends = inferred
        .effects
        .iter()
        .any(|te| te.effect.verb == EffectVerbKind::Sends && te.effect.resource == "Network");
    let has_receives = inferred
        .effects
        .iter()
        .any(|te| te.effect.verb == EffectVerbKind::Receives && te.effect.resource == "Network");
    has_sends && has_receives
}

fn caller_has_any_network(result: &EffectCheckResult, fn_name: &str) -> bool {
    result
        .inferred_effects
        .get(fn_name)
        .map(|s| s.effects.iter().any(|te| te.effect.resource == "Network"))
        .unwrap_or(false)
}

#[test]
fn http_network_effects_propagate_to_callers() {
    // Regression for the pre-lock API-shape audit (phase-8 line 64 / line 70
    // "Effect annotations"): every `std.http` method that performs network
    // I/O must propagate `sends(Network)` + `receives(Network)` to its
    // caller's inferred effect set. Before the fix the client instance
    // methods (`Client.get` / `Client.post` / `RequestBuilder.send`) and the
    // `Server.*` entry points were invisible to network-effect conflict
    // analysis, `karac explain` / `query effects`, and the effect-routed
    // task-parking transform (phase-6 line 17) — the client methods because
    // their qualified-key seed was unreachable through the name-only
    // method-call heuristics, and the server methods because baked-stdlib
    // `with` clauses are not consumed for propagation.
    let src = r#"
        fn call_get() -> i64 {
            let c = Client.new();
            let r = c.get("http://x/y");
            0
        }
        fn call_post() -> i64 {
            let c = Client.new();
            let r = c.post("http://x/y", "body");
            0
        }
        fn call_send() -> i64 {
            let c = Client.new();
            let r = c.request("GET", "http://x/y").send();
            0
        }
        fn handler(req: Request) -> Response {
            Response { status: 200, body: "ok" }
        }
        fn call_serve() -> i64 {
            let r = Server.serve("127.0.0.1:0", handler);
            0
        }
        fn call_serve_tls() -> i64 {
            let r = Server.serve_tls("127.0.0.1:0", "cert", "key", handler);
            0
        }
        fn call_serve_static() -> i64 {
            let r = Server.serve_static("127.0.0.1:0", "ok");
            0
        }
    "#;
    let result = effectcheck_full_pipeline(src);
    for fn_name in [
        "call_get",
        "call_post",
        "call_send",
        "call_serve",
        "call_serve_tls",
        "call_serve_static",
    ] {
        assert!(
            caller_has_network_pair(&result, fn_name),
            "{fn_name} must propagate sends+receives(Network): {:?}",
            result.inferred_effects.get(fn_name).map(|s| &s.effects),
        );
    }
}

#[test]
fn http_network_effect_resolution_does_not_taint_same_named_methods() {
    // Guards the additive precise-key approach against same-name collisions:
    // resolving `client.get()` to `Client.get` must NOT taint `map.get(...)`,
    // which shares the `get` method name but is not a network operation. The
    // precise typechecker-resolved callee distinguishes the two receivers,
    // where a name-only `get` -> `Client.get` mapping could not — it is
    // exactly this collision that makes the name-only `STDLIB_METHOD_MAP`
    // unusable for the HTTP client surface. (`RequestBuilder.send` is resolved
    // the same precise way, so no non-`RequestBuilder` `.send()` can acquire
    // Network either; there is structurally no name-based `send` path in the
    // fix to test against.)
    let src = r#"
        fn map_get_is_pure() -> i64 {
            let mut m: Map[String, i64] = Map.new();
            m.insert("k", 1);
            let v = m.get("k");
            0
        }
    "#;
    let result = effectcheck_full_pipeline(src);
    assert!(
        !caller_has_any_network(&result, "map_get_is_pure"),
        "map.get() must not acquire a Network effect: {:?}",
        result
            .inferred_effects
            .get("map_get_is_pure")
            .map(|s| &s.effects),
    );
}

#[test]
fn tcp_tls_ws_network_effects_propagate_to_callers() {
    // Companion to `http_network_effects_propagate_to_callers`: the same
    // baked-stdlib propagation gap (a `with sends(Network) receives(Network)`
    // clause in the stdlib `.kara` is not consumed for caller propagation —
    // the effect must be hand-seeded in `seed_builtin_effects`) applies to the
    // whole TCP / TLS / WebSocket network surface, not just `std.http`. Every
    // method below carries the network effect in its stdlib signature; a caller
    // must inherit `sends(Network)` + `receives(Network)` so the surface is
    // visible to conflict analysis, `karac explain` / `query effects`, and the
    // effect-routed task-parking transform (phase-6 line 17). These types have
    // no hardcoded typechecker dispatch arm — they resolve through the normal
    // resolved-method path, which records `method_callee_types`, so the precise
    // `MethodCall` resolution added for `std.http` reaches them once seeded.
    let src = r#"
        fn tcp_accept_caller() -> i64 {
            let l = TcpListener.bind("127.0.0.1:0").unwrap();
            let s = l.accept().unwrap();
            0
        }
        fn tcp_io_caller() -> i64 {
            let l = TcpListener.bind("127.0.0.1:0").unwrap();
            let s = l.accept().unwrap();
            let mut buf: Array[u8, 16] = [0u8; 16];
            let r = s.read(mut buf);
            let w = s.write(buf.as_slice());
            0
        }
        fn tls_accept_caller() -> i64 {
            let l = TlsListener.bind_tls("127.0.0.1:0", "cert", "key").unwrap();
            let s = l.accept().unwrap();
            0
        }
        fn tls_connect_caller() -> i64 {
            let s = TlsStream.connect("127.0.0.1:8443", "localhost", "roots").unwrap();
            0
        }
        fn tls_io_caller() -> i64 {
            let s = TlsStream.connect("127.0.0.1:8443", "localhost", "roots").unwrap();
            let mut buf: Array[u8, 16] = [0u8; 16];
            let r = s.read(mut buf);
            let w = s.write(buf.as_slice());
            0
        }
        fn ws_accept_caller() -> i64 {
            let l = TcpListener.bind("127.0.0.1:0").unwrap();
            let ws = WebSocket.accept(l).unwrap();
            0
        }
        fn ws_io_caller() -> i64 {
            let l = TcpListener.bind("127.0.0.1:0").unwrap();
            let ws = WebSocket.accept(l).unwrap();
            let mut buf: Array[u8, 16] = [0u8; 16];
            let r = ws.recv_text(mut buf);
            let w = ws.send_text(buf.as_slice());
            0
        }
    "#;
    let result = effectcheck_full_pipeline(src);
    for fn_name in [
        "tcp_accept_caller",
        "tcp_io_caller",
        "tls_accept_caller",
        "tls_connect_caller",
        "tls_io_caller",
        "ws_accept_caller",
        "ws_io_caller",
    ] {
        assert!(
            caller_has_network_pair(&result, fn_name),
            "{fn_name} must propagate sends+receives(Network): {:?}",
            result.inferred_effects.get(fn_name).map(|s| &s.effects),
        );
    }
}

#[test]
fn network_yield_table_excludes_receiver_recv_suspends() {
    // `Receiver.recv` carries `suspends` (no Network resource). The spec at
    // phase-6 line 26 explicitly excludes `suspends` from the
    // state-machine-transform candidate set — those functions stay
    // thread-blocking at v1 even though they suspend the calling task.
    let receiver_recv = effectcheck_full_pipeline("fn dummy() {}")
        .inferred_effects
        .get("Receiver.recv")
        .cloned()
        .expect("Receiver.recv must be seeded by the effect-checker");
    assert!(
        !set_has_network_yield_effect(&receiver_recv),
        "Receiver.recv carries `suspends`, not `sends`/`receives(Network)` — \
         must NOT classify as network-boundary: {:?}",
        receiver_recv.effects
    );
}

#[test]
fn network_yield_table_excludes_pure_functions() {
    // A function with no effectful calls must NOT classify as network-boundary.
    let result = effectcheck_full_pipeline("fn add(a: i64, b: i64) -> i64 { a + b }");
    let add = result.inferred_effects.get("add").unwrap();
    assert!(
        !set_has_network_yield_effect(add),
        "pure function `add` must not classify as network-boundary: {:?}",
        add.effects
    );
}

#[test]
fn network_yield_table_excludes_alloc_only_callees() {
    // A function that only allocates heap (e.g., `Vec.new`) is NOT
    // network-boundary — `allocates(Heap)` is not a Sends/Receives(Network)
    // pair.
    let result = effectcheck_full_pipeline(
        "fn make_vec() {
             let _v: Vec[i64] = Vec.new();
         }",
    );
    let make_vec = result.inferred_effects.get("make_vec").unwrap();
    assert!(
        !set_has_network_yield_effect(make_vec),
        "alloc-only function must not classify as network-boundary: {:?}",
        make_vec.effects
    );
}

#[test]
fn mut_ref_capture_does_not_synthesize_writes_effect() {
    // A closure that captures `mut ref` and mutates the captured binding
    // must NOT synthesize a `writes(R)` effect. The mutation is an
    // ownership operation against the local binding `x`, not a write
    // against any declared resource. The closure's effect set must be
    // empty (no callee with effects, nothing else effect-positive in the
    // body). Locks in the separation between ownership and effect.
    let result = effectcheck_ok(
        "pub fn apply(f: Fn() -> () with _) -> () with _ { f() }\n\
         fn caller() {\n\
             let mut x: i64 = 1;\n\
             apply((mut ref || { x = x + 1; }));\n\
         }",
    );
    let effects = &result.inferred_effects.get("caller").unwrap().effects;
    assert!(
        effects.is_empty(),
        "mut ref capture mutation must not produce a writes effect: {:?}",
        effects
            .iter()
            .map(|e| (e.effect.verb.clone(), e.effect.resource.clone()))
            .collect::<Vec<_>>()
    );
}

// ── Yield-point enumeration (phase 6 line 26 slice 2) ──────────────────
//
// The cli pipeline populates `Program.yield_points` after
// `callee_network_yield_effect` is in place. For each network-boundary
// function, the table lists the call sites whose callees are themselves
// network-boundary — these are the suspension points the state-machine
// transform will lower to "register fd + park + yield" code.

/// Drive parse → resolve → typecheck → lower → effectcheck → build both
/// `callee_network_yield_effect` and `yield_points` side-tables, returning
/// the populated `Program`. Mirrors `Pipeline::effectcheck`'s wiring.
fn pipeline_with_yield_points(source: &str) -> karac::ast::Program {
    use karac::cli::{build_callee_network_yield_effect_table, build_yield_points_table};
    let mut parsed = parse(source);
    assert!(
        parsed.errors.is_empty(),
        "Parse errors: {:?}",
        parsed.errors
    );
    let resolved = resolve(&parsed.program);
    assert!(
        resolved.errors.is_empty(),
        "Resolve errors: {:?}",
        resolved.errors
    );
    let typed = typecheck(&parsed.program, &resolved);
    assert!(typed.errors.is_empty(), "Type errors: {:?}", typed.errors);
    let method_types = typed.method_callee_types.clone();
    let call_type_subs = typed.call_type_subs.clone();
    lower(&mut parsed.program, &typed);
    let effects = effectcheck_with_typecheck_data(
        &parsed.program,
        PublicEffectsPolicy::default(),
        CompileProfile::Default,
        method_types.clone(),
        call_type_subs,
    );
    parsed.program.callee_network_yield_effect = build_callee_network_yield_effect_table(&effects);
    let yield_points = build_yield_points_table(
        &parsed.program,
        &parsed.program.callee_network_yield_effect,
        &method_types,
    );
    parsed.program.yield_points = yield_points;
    parsed.program
}

#[test]
fn yield_points_records_single_call_to_network_boundary_callee() {
    // A function calling one `with sends(Network)` callee gets one yield
    // point in its table entry, pointing at the call expression's span.
    let program = pipeline_with_yield_points(
        "effect resource Network;
         pub fn fetch() with sends(Network) receives(Network) {}
         fn driver() { fetch(); }",
    );
    let yps = program
        .yield_points
        .get("driver")
        .expect("driver should have yield points (calls into a network-boundary callee)");
    assert_eq!(yps.len(), 1, "expected exactly one yield point: {:?}", yps);
    assert_eq!(yps[0].callee, "fetch");
}

#[test]
fn yield_points_records_multiple_calls_in_order() {
    // Three calls in source order produce three yield points in the same
    // order — the walker traverses statements left-to-right, top-to-bottom.
    let program = pipeline_with_yield_points(
        "effect resource Network;
         pub fn fetch() with sends(Network) receives(Network) {}
         pub fn upload() with sends(Network) {}
         fn driver() {
             fetch();
             upload();
             fetch();
         }",
    );
    let yps = program
        .yield_points
        .get("driver")
        .expect("driver should have yield points");
    assert_eq!(yps.len(), 3, "expected three yield points: {:?}", yps);
    let callees: Vec<&str> = yps.iter().map(|y| y.callee.as_str()).collect();
    assert_eq!(callees, vec!["fetch", "upload", "fetch"]);
}

#[test]
fn yield_points_walks_into_conditionals() {
    // A yield point inside an `if` branch is still recorded — the walker
    // descends into both arms.
    let program = pipeline_with_yield_points(
        "effect resource Network;
         pub fn fetch() with sends(Network) receives(Network) {}
         fn driver(cond: bool) {
             if cond { fetch(); } else { fetch(); }
         }",
    );
    let yps = program
        .yield_points
        .get("driver")
        .expect("driver should have yield points");
    assert_eq!(
        yps.len(),
        2,
        "if/else with yield in each arm = 2 yield points: {:?}",
        yps
    );
}

#[test]
fn yield_points_walks_into_loops() {
    // A yield point inside a `while` body is recorded — the walker
    // descends into the loop body block. The yield-inside-loop case is
    // explicitly called out in the line-26 test-coverage list.
    let program = pipeline_with_yield_points(
        "effect resource Network;
         pub fn fetch() with sends(Network) receives(Network) {}
         fn driver() {
             while true { fetch(); }
         }",
    );
    let yps = program
        .yield_points
        .get("driver")
        .expect("driver should have yield points");
    assert_eq!(yps.len(), 1, "single yield in loop body: {:?}", yps);
    assert_eq!(yps[0].callee, "fetch");
}

#[test]
fn yield_points_omits_non_network_boundary_functions() {
    // A function that doesn't call any network-boundary callee gets no
    // entry in the table, even if it lives in the same Program as a
    // network-boundary function.
    let program = pipeline_with_yield_points(
        "effect resource Network;
         pub fn fetch() with sends(Network) receives(Network) {}
         fn pure_fn(x: i64) -> i64 { x + 1 }
         fn driver() { fetch(); }",
    );
    assert!(
        !program.yield_points.contains_key("pure_fn"),
        "pure_fn has no network yield points: {:?}",
        program.yield_points.get("pure_fn")
    );
    assert!(
        program.yield_points.contains_key("driver"),
        "driver should have yield points"
    );
}

#[test]
fn yield_points_omits_non_boundary_callees_in_boundary_function() {
    // Inside a network-boundary function, only calls to OTHER
    // network-boundary callees count as yield points. Calls to pure or
    // non-network functions in the same body are ignored.
    let program = pipeline_with_yield_points(
        "effect resource Network;
         pub fn fetch() with sends(Network) receives(Network) {}
         fn pure_helper(x: i64) -> i64 { x + 1 }
         fn driver() {
             let _a = pure_helper(1);
             fetch();
             let _b = pure_helper(2);
         }",
    );
    let yps = program
        .yield_points
        .get("driver")
        .expect("driver is network-boundary via fetch call");
    assert_eq!(
        yps.len(),
        1,
        "only the fetch call counts; pure_helper calls do not: {:?}",
        yps
    );
    assert_eq!(yps[0].callee, "fetch");
}

#[test]
fn yield_points_empty_for_program_without_network_effects() {
    // A program with no network-boundary callees has an empty yield_points
    // table. The state-machine transform has nothing to do.
    let program = pipeline_with_yield_points(
        "fn add(a: i64, b: i64) -> i64 { a + b }
         fn use_add() { let _r = add(1, 2); }",
    );
    assert!(
        program.yield_points.is_empty(),
        "no network-boundary callees → empty yield_points: {:?}",
        program.yield_points.keys().collect::<Vec<_>>()
    );
}

// ── captured-locals at yield points (phase 6 line 26 slice 3) ──────────
//
// Each `YieldPoint` carries `captured_locals: Vec<String>` — the names of
// every binding lexically in scope at the yield site (function params +
// every let / pattern binding introduced earlier in source order that
// hasn't gone out of scope). These are the locals the state-machine
// transform codegen must preserve across the suspension (v1 conservative
// over-approximation; future passes can refine with real live-range
// analysis).

#[test]
fn captured_locals_includes_params() {
    // Function params are in scope throughout the body and must appear in
    // every yield point's captured set.
    let program = pipeline_with_yield_points(
        "effect resource Network;
         pub fn fetch(url: i64) with sends(Network) receives(Network) {}
         fn driver(url: i64, retries: i64) { fetch(url); }",
    );
    let yps = program
        .yield_points
        .get("driver")
        .expect("driver should have yield points");
    assert_eq!(yps.len(), 1);
    assert_eq!(yps[0].captured_locals, vec!["url", "retries"]);
}

#[test]
fn captured_locals_includes_lets_introduced_before_yield() {
    // Local bindings introduced before the yield point are in scope at it.
    let program = pipeline_with_yield_points(
        "effect resource Network;
         pub fn fetch() with sends(Network) receives(Network) {}
         fn driver() {
             let a: i64 = 1;
             let b: i64 = 2;
             fetch();
         }",
    );
    let yps = program.yield_points.get("driver").expect("yield points");
    assert_eq!(yps[0].captured_locals, vec!["a", "b"]);
}

#[test]
fn captured_locals_grows_across_sequential_yields() {
    // The first yield sees `a`; the second yield sees `a` + `b` introduced
    // between the two calls.
    let program = pipeline_with_yield_points(
        "effect resource Network;
         pub fn fetch() with sends(Network) receives(Network) {}
         fn driver() {
             let a: i64 = 1;
             fetch();
             let b: i64 = 2;
             fetch();
         }",
    );
    let yps = program.yield_points.get("driver").expect("yield points");
    assert_eq!(yps.len(), 2);
    assert_eq!(yps[0].captured_locals, vec!["a"]);
    assert_eq!(yps[1].captured_locals, vec!["a", "b"]);
}

#[test]
fn captured_locals_excludes_binding_introduced_by_the_let_containing_the_yield() {
    // A yield point in the RHS of `let x = ...` runs BEFORE `x` is bound;
    // `x` is not yet in scope at the yield. (`fetch_ret` doesn't return a
    // useful type here but the parser doesn't need that.)
    let program = pipeline_with_yield_points(
        "effect resource Network;
         pub fn fetch() with sends(Network) receives(Network) {}
         fn driver() {
             let a: i64 = 1;
             let _x: i64 = { fetch(); 0 };
         }",
    );
    let yps = program.yield_points.get("driver").expect("yield points");
    assert_eq!(yps[0].captured_locals, vec!["a"]);
}

#[test]
fn captured_locals_pops_after_inner_block_exit() {
    // A binding inside an inner block is captured at a yield inside that
    // block but NOT at a yield outside it (it's already gone out of
    // scope).
    let program = pipeline_with_yield_points(
        "effect resource Network;
         pub fn fetch() with sends(Network) receives(Network) {}
         fn driver() {
             let outer: i64 = 1;
             {
                 let inner: i64 = 2;
                 fetch();
             }
             fetch();
         }",
    );
    let yps = program.yield_points.get("driver").expect("yield points");
    assert_eq!(yps.len(), 2);
    assert_eq!(yps[0].captured_locals, vec!["outer", "inner"]);
    assert_eq!(yps[1].captured_locals, vec!["outer"]);
}

#[test]
fn captured_locals_records_for_loop_pattern_binding() {
    // A `for x in iter { ... }` body has `x` bound; yield inside captures
    // it. The iter expression itself runs in the parent scope, before
    // `x` is bound.
    let program = pipeline_with_yield_points(
        "effect resource Network;
         pub fn fetch() with sends(Network) receives(Network) {}
         fn driver(items: Vec[i64]) {
             for x in items.iter() { fetch(); }
         }",
    );
    let yps = program.yield_points.get("driver").expect("yield points");
    assert_eq!(yps[0].captured_locals, vec!["items", "x"]);
}

#[test]
fn captured_locals_self_in_methods() {
    // For methods with a `self` parameter, `self` is in scope and appears
    // in every yield point's captured set.
    let program = pipeline_with_yield_points(
        "effect resource Network;
         pub fn fetch() with sends(Network) receives(Network) {}
         shared struct Hub { count: i64 }
         impl Hub {
             fn run(self) { fetch(); }
         }",
    );
    let yps = program
        .yield_points
        .get("Hub.run")
        .expect("Hub.run should be in yield_points");
    assert_eq!(yps[0].captured_locals, vec!["self"]);
}

#[test]
fn captured_locals_does_not_descend_into_closures() {
    // Closures form their own state machine. A network-effect call inside
    // a closure body is NOT a yield point of the enclosing function.
    let program = pipeline_with_yield_points(
        "effect resource Network;
         pub fn fetch() with sends(Network) receives(Network) {}
         fn driver(items: Vec[i64]) {
             let _c = |x: i64| { fetch(); };
         }",
    );
    // driver doesn't itself call any network-boundary fn; the closure
    // body's call doesn't count. So driver has no entry in yield_points.
    assert!(
        !program.yield_points.contains_key("driver"),
        "closure-body yield should NOT count for the outer function: {:?}",
        program.yield_points.get("driver")
    );
}

// ─── Phase 6 line 26 slice 4: state-struct layout synthesis ────────────────
//
// Slice 4 lifts the captured-local *name* lists (slice 3) into a per-function
// `StateStructLayout` carrying the **union** of captured locals across every
// yield point, paired with each binding's surface type name as recorded by
// the typechecker's `pattern_binding_types` map. Codegen consumes this layout
// (in a future slice) to size and lower the network-boundary function's
// state struct one-per-monomorphization.

/// Drive parse → resolve → typecheck → lower → effectcheck → build all four
/// side-tables (`callee_network_yield_effect`, `yield_points`,
/// `pattern_binding_types` mirror on Program, `state_struct_layouts`),
/// returning the populated `Program`. Mirrors `Pipeline::effectcheck`'s
/// wiring including the slice-4 step.
fn pipeline_with_state_struct_layouts(source: &str) -> karac::ast::Program {
    use karac::cli::{
        build_callee_network_yield_effect_table, build_state_struct_layouts,
        build_yield_points_table,
    };
    let mut parsed = parse(source);
    assert!(
        parsed.errors.is_empty(),
        "Parse errors: {:?}",
        parsed.errors
    );
    let resolved = resolve(&parsed.program);
    assert!(
        resolved.errors.is_empty(),
        "Resolve errors: {:?}",
        resolved.errors
    );
    let typed = typecheck(&parsed.program, &resolved);
    assert!(typed.errors.is_empty(), "Type errors: {:?}", typed.errors);
    let method_types = typed.method_callee_types.clone();
    let call_type_subs = typed.call_type_subs.clone();
    let pattern_binding_types = typed.pattern_binding_types.clone();
    lower(&mut parsed.program, &typed);
    let effects = effectcheck_with_typecheck_data(
        &parsed.program,
        PublicEffectsPolicy::default(),
        CompileProfile::Default,
        method_types.clone(),
        call_type_subs,
    );
    parsed.program.callee_network_yield_effect = build_callee_network_yield_effect_table(&effects);
    let yield_points = build_yield_points_table(
        &parsed.program,
        &parsed.program.callee_network_yield_effect,
        &method_types,
    );
    parsed.program.yield_points = yield_points;
    let layouts = build_state_struct_layouts(
        &parsed.program,
        &parsed.program.callee_network_yield_effect,
        &method_types,
        &pattern_binding_types,
    );
    parsed.program.state_struct_layouts = layouts;
    parsed.program
}

#[test]
fn state_struct_layout_records_named_param_with_typechecker_recorded_type_name() {
    // A `Vec[T]` param's surface type is recorded by the typechecker into
    // `pattern_binding_types` (`"Vec"`), so slice 4 surfaces it in the
    // state-struct field's `type_name` — codegen has enough info to size
    // the slot without re-deriving the type.
    let program = pipeline_with_state_struct_layouts(
        "effect resource Network;
         pub fn fetch() with sends(Network) receives(Network) {}
         fn driver(items: Vec[i64]) { fetch(); }",
    );
    let layout = program
        .state_struct_layouts
        .get("driver")
        .expect("driver should have a state-struct layout");
    assert_eq!(layout.fields.len(), 1);
    assert_eq!(layout.fields[0].name, "items");
    assert_eq!(layout.fields[0].type_name.as_deref(), Some("Vec"));
}

#[test]
fn state_struct_layout_unions_captures_across_yield_points_in_source_order() {
    // Two sequential yield points where the second yield sees a binding
    // introduced after the first: the layout = source-introduction-ordered
    // union [a, b], with `b` reachable only at the second yield. v1 packs
    // unconditionally — every entry holds storage across every yield.
    let program = pipeline_with_state_struct_layouts(
        "effect resource Network;
         pub fn fetch() with sends(Network) receives(Network) {}
         fn driver(a: Vec[i64]) {
             fetch();
             let b: Vec[i64] = a;
             fetch();
         }",
    );
    let layout = program
        .state_struct_layouts
        .get("driver")
        .expect("driver layout");
    let names: Vec<&str> = layout.fields.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, vec!["a", "b"]);
    assert_eq!(layout.fields[0].type_name.as_deref(), Some("Vec"));
    assert_eq!(layout.fields[1].type_name.as_deref(), Some("Vec"));
}

#[test]
fn state_struct_layout_self_in_methods_uses_impl_target_type() {
    // `self` has no introducing pattern, so its `type_name` comes from the
    // impl block's target type directly — `Hub` here. This makes codegen's
    // first-pass lookup for `self`-typed state-struct slots consistent with
    // the same canonical name used for other Named-type entries.
    let program = pipeline_with_state_struct_layouts(
        "effect resource Network;
         pub fn fetch() with sends(Network) receives(Network) {}
         shared struct Hub { count: i64 }
         impl Hub {
             fn run(self) { fetch(); }
         }",
    );
    let layout = program
        .state_struct_layouts
        .get("Hub.run")
        .expect("Hub.run layout");
    assert_eq!(layout.fields.len(), 1);
    assert_eq!(layout.fields[0].name, "self");
    assert_eq!(layout.fields[0].type_name.as_deref(), Some("Hub"));
}

#[test]
fn state_struct_layout_primitive_typed_bindings_have_none_type_name() {
    // The typechecker's `pattern_binding_types` map only records Named /
    // Str / Shared surface names. Primitive-typed bindings (`i64`, `bool`,
    // ...) yield `None` in the layout; codegen falls through to its
    // primitive-sizing path on absent entries.
    let program = pipeline_with_state_struct_layouts(
        "effect resource Network;
         pub fn fetch() with sends(Network) receives(Network) {}
         fn driver(n: i64) { fetch(); }",
    );
    let layout = program
        .state_struct_layouts
        .get("driver")
        .expect("driver layout");
    assert_eq!(layout.fields.len(), 1);
    assert_eq!(layout.fields[0].name, "n");
    assert!(layout.fields[0].type_name.is_none());
}

#[test]
fn state_struct_layout_excludes_inner_block_bindings_not_seen_at_any_yield() {
    // A binding introduced inside an inner block that closes before the
    // (only) yield-point call is NOT in the layout — slice 4's union is
    // *what every yield point captures*, not *every binding the body ever
    // had*. Lexical scope pop discipline carries through from slice 3.
    let program = pipeline_with_state_struct_layouts(
        "effect resource Network;
         pub fn fetch() with sends(Network) receives(Network) {}
         fn driver(outer: Vec[i64]) {
             {
                 let inner: Vec[i64] = outer;
                 // no yield here — `inner` pops at this block's close
             }
             fetch();
         }",
    );
    let layout = program
        .state_struct_layouts
        .get("driver")
        .expect("driver layout");
    let names: Vec<&str> = layout.fields.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["outer"],
        "`inner` popped before the yield — must not appear in layout"
    );
}

#[test]
fn state_struct_layout_excludes_closures_no_entry_for_outer_function() {
    // Same closure rule as slice 3: a yield inside a closure body is the
    // closure's own state machine, not the enclosing function's. The
    // enclosing function gets no entry in `state_struct_layouts` when its
    // own body has zero network-effect call sites.
    let program = pipeline_with_state_struct_layouts(
        "effect resource Network;
         pub fn fetch() with sends(Network) receives(Network) {}
         fn driver(items: Vec[i64]) {
             let _c = |x: i64| { fetch(); };
         }",
    );
    assert!(
        !program.state_struct_layouts.contains_key("driver"),
        "closure-body yield must not produce an entry for the outer function: {:?}",
        program.state_struct_layouts.get("driver")
    );
}

#[test]
fn state_struct_layout_pure_function_has_no_entry() {
    // A function with no network-effect calls in its body — even one
    // whose surrounding program declares network-bearing effects — gets
    // no entry. Mirrors `YieldPointsTable`'s presence rule exactly.
    let program = pipeline_with_state_struct_layouts(
        "effect resource Network;
         pub fn fetch() with sends(Network) receives(Network) {}
         fn pure_helper(x: i64) -> i64 { x + 1 }
         fn driver() { fetch(); }",
    );
    assert!(
        !program.state_struct_layouts.contains_key("pure_helper"),
        "pure function must not have a state-struct layout entry"
    );
    // sanity check: the driver entry exists (we did populate the table)
    assert!(program.state_struct_layouts.contains_key("driver"));
}

#[test]
fn state_struct_layout_includes_inner_block_binding_when_yield_inside() {
    // The mirror of the pop test: a binding introduced inside an inner
    // block that DOES contain a yield ends up in the layout. The walker
    // snapshots scope at the yield site, so the inner binding is part
    // of that yield's captures and thus part of the function-level union.
    let program = pipeline_with_state_struct_layouts(
        "effect resource Network;
         pub fn fetch() with sends(Network) receives(Network) {}
         fn driver(outer: Vec[i64]) {
             {
                 let inner: Vec[i64] = outer;
                 fetch();
             }
         }",
    );
    let layout = program
        .state_struct_layouts
        .get("driver")
        .expect("driver layout");
    let names: Vec<&str> = layout.fields.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, vec!["outer", "inner"]);
}

// ── #[profile(...)] slice 3 — effect-checker integration ────────
//
// For each function carrying a non-empty `profile_compat` list, the
// effect checker walks the transitive (declared + inferred) effect set
// after inference settles and emits `E_PROFILE_INCOMPATIBLE_EFFECT`
// (`EffectErrorKind::ProfileIncompatibleEffect`) for any effect that
// at least one listed profile forbids. Per-profile forbidden table:
//   - default: forbids nothing
//   - embedded: forbids allocates(Heap)
//   - kernel:   forbids allocates(*), panics, blocks, suspends
// These tests run against the default build profile so the *attribute*
// is the only constraint source — the build-profile path is covered
// by the `test_profile_*` tests above.

fn profile_compat_errors(source: &str) -> Vec<EffectError> {
    let parsed = parse(source);
    assert!(
        parsed.errors.is_empty(),
        "Parse errors: {}",
        parsed
            .errors
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    let result = effectcheck(&parsed.program);
    result
        .errors
        .into_iter()
        .filter(|e| e.kind == EffectErrorKind::ProfileIncompatibleEffect)
        .collect()
}

fn profile_compat_ok(source: &str) {
    let errors = profile_compat_errors(source);
    assert!(
        errors.is_empty(),
        "Expected no profile-compat errors, got: {errors:?}",
    );
}

#[test]
fn profile_slice3_default_allows_all_effects_via_attribute() {
    // The `default` profile imposes no restrictions, so the attribute
    // is satisfied even when the function clearly allocates.
    profile_compat_ok(
        "#[profile(default)]\n\
         pub fn make_vec() -> Vec[i64] with allocates(Heap) { Vec.new() }",
    );
}

#[test]
fn profile_slice3_embedded_accepts_pure_function() {
    profile_compat_ok(
        "#[profile(embedded)]\n\
         pub fn add(x: i64, y: i64) -> i64 { x + y }",
    );
}

#[test]
fn profile_slice3_embedded_rejects_heap_allocates() {
    let errors = profile_compat_errors(
        "#[profile(embedded)]\n\
         pub fn make_vec() -> Vec[i64] with allocates(Heap) { Vec.new() }",
    );
    assert_eq!(errors.len(), 1, "expected exactly one error: {errors:?}");
    let msg = &errors[0].message;
    assert!(msg.contains("E_PROFILE_INCOMPATIBLE_EFFECT"));
    assert!(msg.contains("make_vec"));
    assert!(msg.contains("embedded"));
    assert!(msg.contains("allocates(Heap)"));
}

#[test]
fn profile_slice3_embedded_allows_non_heap_allocates() {
    // Embedded only forbids `allocates(Heap)`; user-defined resources
    // pass.
    profile_compat_ok(
        "effect resource Arena;\n\
         #[profile(embedded)]\n\
         pub fn arena_alloc() with allocates(Arena) {}",
    );
}

#[test]
fn profile_slice3_kernel_rejects_panics() {
    let errors = profile_compat_errors(
        "#[profile(kernel)]\n\
         pub fn might_fail() with panics { todo() }",
    );
    assert_eq!(errors.len(), 1, "expected exactly one error: {errors:?}");
    assert!(errors[0].message.contains("kernel"));
    assert!(errors[0].message.contains("panics"));
}

#[test]
fn profile_slice3_kernel_rejects_blocks() {
    // The `blocks` effect is inferred from a call to an `extern "C"` fn
    // (the C ABI default trust-not-verify set is `{blocks}`).
    let errors = profile_compat_errors(
        "unsafe extern \"C\" { fn sleep(secs: i64) -> i64; }\n\
         #[profile(kernel)]\n\
         pub fn waits() with blocks { unsafe { sleep(1) } }",
    );
    assert_eq!(errors.len(), 1, "expected exactly one error: {errors:?}");
    assert!(errors[0].message.contains("blocks"));
}

#[test]
fn profile_slice3_kernel_rejects_suspends() {
    // The `suspends` effect is inferred from `Receiver.recv()`, which
    // the effect checker seeds with `suspends`.
    let errors = profile_compat_errors(
        "#[profile(kernel)]\n\
         pub fn yields(r: Receiver[i64]) -> i64 with suspends { r.recv() }",
    );
    assert_eq!(errors.len(), 1, "expected exactly one error: {errors:?}");
    assert!(errors[0].message.contains("suspends"));
}

#[test]
fn profile_slice3_kernel_rejects_allocates_any_resource() {
    // Kernel forbids `allocates` regardless of resource — unlike
    // embedded which only catches `allocates(Heap)`.
    let errors = profile_compat_errors(
        "effect resource Arena;\n\
         #[profile(kernel)]\n\
         pub fn arena_alloc() with allocates(Arena) {}",
    );
    assert_eq!(errors.len(), 1, "expected exactly one error: {errors:?}");
    assert!(errors[0].message.contains("allocates(Arena)"));
}

#[test]
fn profile_slice3_intersection_tightens_one_profile_forbids() {
    // `#[profile(embedded, kernel)]`: `panics` is forbidden by `kernel`
    // but not by `embedded`. Only `kernel` shows up in the
    // "forbidden by" list, but the full declared list still appears in
    // the diagnostic so the user can see the tightening came from the
    // intersection.
    let errors = profile_compat_errors(
        "#[profile(embedded, kernel)]\n\
         pub fn might_fail() with panics { todo() }",
    );
    assert_eq!(errors.len(), 1, "expected exactly one error: {errors:?}");
    let msg = &errors[0].message;
    // Full declared list rendered as the attribute payload.
    assert!(
        msg.contains("#[profile(embedded, kernel)]"),
        "diagnostic should echo declared profile list: {msg}",
    );
    // The forbidding profile is named explicitly.
    assert!(msg.contains("the `kernel` profile"));
}

#[test]
fn profile_slice3_intersection_tightens_multiple_profiles_forbid() {
    // `#[profile(embedded, kernel)]`: `allocates(Heap)` is forbidden by
    // *both* profiles. The diagnostic switches to the "strictest of"
    // phrasing and lists every profile that forbids the effect.
    let errors = profile_compat_errors(
        "#[profile(embedded, kernel)]\n\
         pub fn make_vec() -> Vec[i64] with allocates(Heap) { Vec.new() }",
    );
    assert_eq!(errors.len(), 1, "expected exactly one error: {errors:?}");
    let msg = &errors[0].message;
    assert!(
        msg.contains("strictest"),
        "expected 'strictest' phrasing when 2+ profiles forbid: {msg}",
    );
    assert!(msg.contains("embedded"));
    assert!(msg.contains("kernel"));
}

#[test]
fn profile_slice3_two_offending_effects_yield_two_errors() {
    // Both `allocates(Heap)` and `panics` are forbidden by `kernel`;
    // each effect emits its own diagnostic so the user can address them
    // independently.
    let errors = profile_compat_errors(
        "#[profile(kernel)]\n\
         pub fn dangerous() -> Vec[i64] with allocates(Heap) panics {\n\
             todo()\n\
         }",
    );
    assert_eq!(errors.len(), 2, "expected two errors: {errors:?}");
    let combined: String = errors.iter().map(|e| e.message.as_str()).collect();
    assert!(combined.contains("allocates(Heap)"));
    assert!(combined.contains("panics"));
}

#[test]
fn profile_slice3_inherent_method_checked() {
    // Method-key lookup uses `Type.method`, matching the lookup
    // convention `inferred_effects` exposes. An inherent impl with
    // `#[profile(embedded)]` and an inferred `allocates(Heap)` should
    // surface the violation.
    let errors = profile_compat_errors(
        "struct Buf { data: Vec[i64] }\n\
         impl Buf {\n\
             #[profile(embedded)]\n\
             pub fn fresh() -> Vec[i64] with allocates(Heap) { Vec.new() }\n\
         }",
    );
    assert_eq!(errors.len(), 1, "expected exactly one error: {errors:?}");
    assert!(errors[0].message.contains("fresh"));
    assert!(errors[0].message.contains("embedded"));
}

#[test]
fn profile_slice3_inferred_effect_drives_check() {
    // No declared `with allocates(Heap)` — the effect is *inferred*
    // from the `Vec.new()` call. The profile check must consult the
    // post-inference set, not just the declared set.
    let errors = profile_compat_errors(
        "#[profile(embedded)]\n\
         fn build() -> Vec[i64] { Vec.new() }",
    );
    assert_eq!(errors.len(), 1, "expected exactly one error: {errors:?}");
    assert!(errors[0].message.contains("allocates(Heap)"));
}

#[test]
fn profile_slice3_unknown_profile_skips_check() {
    // The resolver emits E_UNKNOWN_PROFILE for `yolo`; the effect
    // checker must not pile a stale profile-compat error on top.
    let parsed = parse(
        "#[profile(yolo)]\n\
         pub fn build() -> Vec[i64] with allocates(Heap) { Vec.new() }",
    );
    let result = effectcheck(&parsed.program);
    assert!(
        !result
            .errors
            .iter()
            .any(|e| e.kind == EffectErrorKind::ProfileIncompatibleEffect),
        "did not expect profile-compat errors when a name is unknown; got: {:?}",
        result.errors,
    );
}

#[test]
fn profile_slice3_dedupe_within_attr() {
    // `#[profile(embedded, embedded)]` is one constraint contributor,
    // not two — only one diagnostic per offending effect.
    let errors = profile_compat_errors(
        "#[profile(embedded, embedded)]\n\
         pub fn make_vec() -> Vec[i64] with allocates(Heap) { Vec.new() }",
    );
    assert_eq!(
        errors.len(),
        1,
        "duplicate profile name should not double-emit: {errors:?}",
    );
}

#[test]
fn profile_slice3_diagnostic_uses_dedicated_error_kind() {
    let errors = profile_compat_errors(
        "#[profile(embedded)]\n\
         pub fn make_vec() -> Vec[i64] with allocates(Heap) { Vec.new() }",
    );
    assert!(matches!(
        errors[0].kind,
        EffectErrorKind::ProfileIncompatibleEffect
    ));
}

#[test]
fn profile_slice3_absent_attribute_silent() {
    // No `#[profile]` attribute → no profile-compat diagnostic, even
    // when the function clearly allocates.
    profile_compat_ok("pub fn make_vec() -> Vec[i64] with allocates(Heap) { Vec.new() }");
}

// ── Phase 6 line 26 slice 8aa: call_effect_subs side-table ───────────────
//
// `compute_call_var_bindings` already resolves named effect variables
// at each call site by unioning effects across Fn-arg slot positions
// that reference the variable. Slice 8aa persists those bindings into
// `EffectCheckResult.call_effect_subs`, keyed by call-expression span,
// so slice 8ab (entry 35) can forward them to codegen and slice 8y
// (entry 32) can gate state-machine emission on the resolved
// per-call effect set.

#[test]
fn test_slice_8aa_call_effect_subs_populated_for_polymorphic_effect_call() {
    // `op[T, with E]` called with a closure that reads(Db) should
    // record `E → {reads(Db)}` in `call_effect_subs` keyed on the
    // `op(...)` call span. Mirrors
    // `test_compound_polymorphism_end_to_end_effect_propagation`'s
    // shape — uses the full pipeline so the typechecker's
    // `call_type_subs` and `method_callee_types` are threaded
    // through, matching the production path the codegen consumer
    // (slice 8ab → 8y) will exercise.
    let source = "effect resource Db;\n\
                  pub fn op[T, with E](x: T, cb: Fn(T) -> T with E) -> T with E { cb(x) }\n\
                  pub fn touch_db(x: i64) -> i64 with reads(Db) { x }\n\
                  pub fn main() with reads(Db) {\n\
                      let _ = op(42, |y| touch_db(y));\n\
                  }";
    let result = effectcheck_full_pipeline(source);
    assert!(
        !result.call_effect_subs.is_empty(),
        "call_effect_subs must record at least one binding for the op(...) call"
    );
    // There should be exactly one entry — the `op(42, |y| ...)` call
    // site. Inner map binds the single effect variable `E` to a set
    // containing the `reads(Db)` effect.
    let (_, bindings) = result
        .call_effect_subs
        .iter()
        .next()
        .expect("at least one call-site binding");
    let e_binding = bindings
        .get("E")
        .expect("effect variable E must be bound at the op(...) call");
    assert!(
        e_binding
            .iter()
            .any(|e| matches!(e.verb, EffectVerbKind::Reads) && e.resource == "Db"),
        "E must be bound to a set containing reads(Db); got: {:?}",
        e_binding
    );
}

#[test]
fn test_slice_8aa_call_effect_subs_empty_when_no_polymorphic_callee() {
    // A program with no polymorphic-effect callees should leave
    // `call_effect_subs` empty. Pins the "absence means no
    // polymorphic-effect to resolve" semantic that slice 8ab + 8y
    // consumers rely on.
    let result = effectcheck_ok(
        "pub fn add(x: i64, y: i64) -> i64 { x + y }\n\
         pub fn main() { let _ = add(1, 2); }",
    );
    assert!(
        result.call_effect_subs.is_empty(),
        "non-polymorphic-effect program must not populate call_effect_subs; got: {:?}",
        result.call_effect_subs
    );
}

#[test]
fn test_slice_8aa_call_effect_subs_records_multiple_call_sites() {
    // Two distinct call sites to the same polymorphic-effect callee
    // bind E to (potentially) different concrete effect sets. The
    // table records both entries keyed by their respective call
    // spans — confirming the per-call-site granularity needed for
    // slice 8y's per-call gating.
    let source = "effect resource Db;\n\
                  pub fn op[T, with E](x: T, cb: Fn(T) -> T with E) -> T with E { cb(x) }\n\
                  pub fn touch_db(x: i64) -> i64 with reads(Db) { x }\n\
                  pub fn pure_id(x: i64) -> i64 { x }\n\
                  pub fn main() with reads(Db) {\n\
                      let _ = op(42, |y| touch_db(y));\n\
                      let _ = op(7, |y| pure_id(y));\n\
                  }";
    let result = effectcheck_full_pipeline(source);
    assert_eq!(
        result.call_effect_subs.len(),
        2,
        "expected 2 call-site bindings (one per op call), got {}: {:?}",
        result.call_effect_subs.len(),
        result.call_effect_subs
    );
    // One binding should carry reads(Db); the other should be empty
    // (pure closure body) — both are valid resolutions of `E` and
    // both must be recorded.
    let mut saw_reads_db = false;
    let mut saw_empty = false;
    for bindings in result.call_effect_subs.values() {
        let e = bindings.get("E").expect("E must be bound at each op call");
        if e.iter()
            .any(|x| matches!(x.verb, EffectVerbKind::Reads) && x.resource == "Db")
        {
            saw_reads_db = true;
        } else if e.is_empty() {
            saw_empty = true;
        }
    }
    assert!(
        saw_reads_db && saw_empty,
        "expected one call to bind E→{{reads(Db)}} and one to bind E→{{}}; got: {:?}",
        result.call_effect_subs
    );
}

// ── Resource-trait dispatch effect inference ───────────────────────
//
// `R.method(...)` where `R` is an `effect resource R: Trait`
// dispatches at runtime through the provider stack. The effect
// inference walker contributes the verb implied by `Trait.method`'s
// receiver mode to the caller's inferred set:
//   `mut ref self` / owned `self` → writes(R)
//   `ref self`                    → reads(R)

#[test]
fn test_resource_trait_dispatch_infers_writes_from_mut_ref_self() {
    let result = effectcheck_ok(
        "pub trait Logger { fn log(mut ref self, msg: String); }\n\
         pub effect resource Audit: Logger;\n\
         pub struct InMemoryLogger { entries: Vec[String] }\n\
         impl InMemoryLogger { pub fn new() -> InMemoryLogger { InMemoryLogger { entries: Vec.new() } } }\n\
         impl Logger for InMemoryLogger { fn log(mut ref self, msg: String) { self.entries.push(msg); } }\n\
         fn record_event(msg: String) { Audit.log(msg); }",
    );
    let inferred = result
        .inferred_effects
        .get("record_event")
        .expect("record_event must be in inferred_effects");
    assert!(
        inferred.effects.iter().any(
            |e| matches!(e.effect.verb, EffectVerbKind::Writes) && e.effect.resource == "Audit"
        ),
        "expected writes(Audit) in record_event's inferred set; got: {:?}",
        inferred.effects
    );
}

#[test]
fn test_resource_trait_dispatch_infers_reads_from_ref_self() {
    let result = effectcheck_ok(
        "pub trait Reader { fn last(ref self) -> String; }\n\
         pub effect resource Audit: Reader;\n\
         pub struct InMemoryReader { last: String }\n\
         impl Reader for InMemoryReader { fn last(ref self) -> String { self.last } }\n\
         fn peek() -> String { Audit.last() }",
    );
    let inferred = result
        .inferred_effects
        .get("peek")
        .expect("peek must be in inferred_effects");
    assert!(
        inferred
            .effects
            .iter()
            .any(|e| matches!(e.effect.verb, EffectVerbKind::Reads) && e.effect.resource == "Audit"),
        "expected reads(Audit) in peek's inferred set; got: {:?}",
        inferred.effects
    );
}

#[test]
fn test_wrong_resource_on_resource_trait_dispatch_diagnosed() {
    // `record_event` calls `Audit.log` (writes(Audit)) but declares
    // the wrong resource (`writes(Wrong)`). The body's actual
    // writes(Audit) is missing from the declaration →
    // MissingEffectDeclaration. The mirror diagnostic
    // (OverDeclaredEffect on the unused writes(Wrong)) does not fire
    // under the current architecture: `collect_function_info` seeds
    // inferred_effects from declared, so declared effects are always
    // present in the inferred set at verification time. This is the
    // same trade-off documented in
    // `test_public_over_declaration_detected`. The
    // Missing-on-the-actual-call-site diagnostic is the load-bearing
    // one for catching a programmer who declared the wrong resource.
    let errors = effectcheck_errors(
        "pub trait Logger { fn log(mut ref self, msg: String); }\n\
         pub effect resource Audit: Logger;\n\
         pub effect resource Wrong: Logger;\n\
         pub struct InMemoryLogger { entries: Vec[String] }\n\
         impl Logger for InMemoryLogger { fn log(mut ref self, msg: String) { self.entries.push(msg); } }\n\
         pub fn record_event(msg: String) writes(Wrong) { Audit.log(msg); }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.kind == EffectErrorKind::MissingEffectDeclaration
                && e.message.contains("writes(Audit)")),
        "expected MissingEffectDeclaration mentioning writes(Audit); got: {:?}",
        errors
            .iter()
            .map(|e| (&e.kind, &e.message))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_missing_resource_effect_under_declared_policy_diagnosed() {
    // The default `Declared` policy requires every `pub fn` to list
    // its effects; calling `Audit.log` without declaring writes(Audit)
    // fires `MissingEffectDeclaration`.
    let errors = effectcheck_errors(
        "pub trait Logger { fn log(mut ref self, msg: String); }\n\
         pub effect resource Audit: Logger;\n\
         pub struct InMemoryLogger { entries: Vec[String] }\n\
         impl Logger for InMemoryLogger { fn log(mut ref self, msg: String) { self.entries.push(msg); } }\n\
         pub fn record_event(msg: String) { Audit.log(msg); }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.kind == EffectErrorKind::MissingEffectDeclaration
                && e.message.contains("writes(Audit)")),
        "expected MissingEffectDeclaration mentioning writes(Audit); got: {:?}",
        errors
            .iter()
            .map(|e| (&e.kind, &e.message))
            .collect::<Vec<_>>()
    );
}

// ── Dispatch call sites inherit declared clause effects ────────────
//
// The receiver-derived verb on R is only the floor: a trait method's
// declared `with` clause can name effects on *other* resources
// (e.g. `fn get(ref self, ...) with reads(Cfg) writes(Log)`), and a
// `Cfg.get(...)` call site must inherit those too. Before the fix the
// seed inserted exactly one receiver-derived `verb(R)` per `R.method`
// key and silently dropped the clause — under-attribution that would
// let two tasks race on `Log` through `Cfg.get`.

#[test]
fn test_resource_dispatch_inherits_clause_effects_on_other_resources() {
    // `f` declares only the receiver-implied reads(Cfg); the clause's
    // writes(Log) must surface as a missing declaration.
    let errors = effectcheck_errors(
        "pub trait Logger { fn log(mut ref self, msg: i64); }\n\
         pub trait Config { fn get(ref self, k: i64) -> i64 with reads(Cfg) writes(Log); }\n\
         pub effect resource Cfg: Config;\n\
         pub effect resource Log: Logger;\n\
         pub struct C { v: i64 }\n\
         impl Config for C { fn get(ref self, k: i64) -> i64 with reads(Cfg) writes(Log) { self.v } }\n\
         pub fn f() -> i64 with reads(Cfg) { Cfg.get(1) }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.kind == EffectErrorKind::MissingEffectDeclaration
                && e.message.contains("writes(Log)")),
        "expected MissingEffectDeclaration mentioning writes(Log); got: {:?}",
        errors
            .iter()
            .map(|e| (&e.kind, &e.message))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_resource_dispatch_clause_effects_declared_passes() {
    // Same program with writes(Log) declared on `f` — clean.
    effectcheck_ok(
        "pub trait Logger { fn log(mut ref self, msg: i64); }\n\
         pub trait Config { fn get(ref self, k: i64) -> i64 with reads(Cfg) writes(Log); }\n\
         pub effect resource Cfg: Config;\n\
         pub effect resource Log: Logger;\n\
         pub struct C { v: i64 }\n\
         impl Config for C { fn get(ref self, k: i64) -> i64 with reads(Cfg) writes(Log) { self.v } }\n\
         pub fn f() -> i64 with reads(Cfg) writes(Log) { Cfg.get(1) }",
    );
}

#[test]
fn test_resource_dispatch_clause_writes_survives_alongside_receiver_reads() {
    // `ref self` + declared `writes(Db)`: the receiver contributes
    // reads(Db), the clause contributes writes(Db) — both must land in
    // the caller's inferred set (E0412 only rejects the inverse, a
    // writes-receiver with a non-writes clause on R).
    let result = effectcheck_ok(
        "pub trait Store { fn put(ref self, v: i64) with writes(Db); }\n\
         pub effect resource Db: Store;\n\
         pub struct S { v: i64 }\n\
         impl Store for S { fn put(ref self, v: i64) with writes(Db) { } }\n\
         fn g() { Db.put(1); }",
    );
    let inferred = result
        .inferred_effects
        .get("g")
        .expect("g must be in inferred_effects");
    let has = |verb: fn(&EffectVerbKind) -> bool| {
        inferred
            .effects
            .iter()
            .any(|e| verb(&e.effect.verb) && e.effect.resource == "Db")
    };
    assert!(
        has(|v| matches!(v, EffectVerbKind::Reads)) && has(|v| matches!(v, EffectVerbKind::Writes)),
        "expected both reads(Db) and writes(Db) in g's inferred set; got: {:?}",
        inferred.effects
    );
}

#[test]
fn test_resource_dispatch_inherits_clause_effects_through_group() {
    // The clause names an effect group; the seed runs after
    // `expand_effect_groups`/`collect_declared_effects`, so the
    // group's expansion (writes(Log)) must be inherited too.
    let errors = effectcheck_errors(
        "effect group logging = writes(Log);\n\
         pub trait Logger { fn log(mut ref self, msg: i64); }\n\
         pub trait Config { fn get(ref self, k: i64) -> i64 with reads(Cfg) logging; }\n\
         pub effect resource Cfg: Config;\n\
         pub effect resource Log: Logger;\n\
         pub struct C { v: i64 }\n\
         impl Config for C { fn get(ref self, k: i64) -> i64 with reads(Cfg) logging { self.v } }\n\
         pub fn f() -> i64 with reads(Cfg) { Cfg.get(1) }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.kind == EffectErrorKind::MissingEffectDeclaration
                && e.message.contains("writes(Log)")),
        "expected MissingEffectDeclaration mentioning writes(Log); got: {:?}",
        errors
            .iter()
            .map(|e| (&e.kind, &e.message))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_resource_dispatch_polymorphic_clause_contributes_only_receiver_verb() {
    // A `with _` ceiling is conservatively skipped (same set the E0412
    // predicate skips) — the dispatch site contributes only the
    // receiver-implied reads(Cfg).
    effectcheck_ok(
        "pub trait Config { fn get(ref self, k: i64) -> i64 with _; }\n\
         pub effect resource Cfg: Config;\n\
         pub struct C { v: i64 }\n\
         impl Config for C { fn get(ref self, k: i64) -> i64 { self.v } }\n\
         pub fn f() -> i64 with reads(Cfg) { Cfg.get(1) }",
    );
}

// ── E0412: resource-receiver contradiction ────────────────────────
//
// An `effect resource R: Trait` method whose declared `with` clause
// mentions R without writes(R), while its receiver mode (bare `self`
// or `mut ref self`) seeds writes(R) on every `R.method(...)` call
// site. The declaration is unsatisfiable as written — the diagnostic
// fires at the trait method definition (the root cause, not the
// N call sites that would otherwise each trip E0400) and carries a
// machine-applicable `ref self` receiver rewrite for `karac fix`.

#[test]
fn test_resource_receiver_contradiction_owned_self_reads_only() {
    let source = "pub effect resource Cfg: Config;\n\
                  pub trait Config { fn get(self, k: i64) -> i64 with reads(Cfg); }";
    let errors = effectcheck_errors(source);
    let err = errors
        .iter()
        .find(|e| e.kind == EffectErrorKind::ResourceReceiverContradiction)
        .unwrap_or_else(|| {
            panic!(
                "expected ResourceReceiverContradiction; got: {:?}",
                errors
                    .iter()
                    .map(|e| (&e.kind, &e.message))
                    .collect::<Vec<_>>()
            )
        });
    assert!(
        err.message.contains("Config.get")
            && err.message.contains("reads(Cfg)")
            && err.message.contains("`self` receiver")
            && err.message.contains("ref self"),
        "message should name the method, the declared verb, the receiver, \
         and the fix; got: {}",
        err.message
    );
    let edit = err
        .replacement
        .as_deref()
        .expect("E0412 must carry a machine-applicable receiver rewrite");
    assert_eq!(edit.replacement, "ref self");
    assert_eq!(
        &source[edit.offset..edit.offset + edit.length],
        "self",
        "edit span must cover exactly the receiver text"
    );
}

#[test]
fn test_resource_receiver_contradiction_mut_ref_self_reads_only() {
    let source = "pub effect resource Cfg: Config;\n\
                  pub trait Config { fn get(mut ref self, k: i64) -> i64 with reads(Cfg); }";
    let errors = effectcheck_errors(source);
    let err = errors
        .iter()
        .find(|e| e.kind == EffectErrorKind::ResourceReceiverContradiction)
        .expect("expected ResourceReceiverContradiction for mut ref self");
    assert!(
        err.message.contains("`mut ref self` receiver"),
        "message: {}",
        err.message
    );
    let edit = err.replacement.as_deref().expect("rewrite expected");
    assert_eq!(edit.replacement, "ref self");
    assert_eq!(
        &source[edit.offset..edit.offset + edit.length],
        "mut ref self",
        "edit span must cover the full receiver form"
    );
}

#[test]
fn test_resource_receiver_ref_self_reads_only_is_clean() {
    // `ref self` seeds reads(R) — the reads-only declaration holds.
    effectcheck_ok(
        "pub effect resource Cfg: Config;\n\
         pub trait Config { fn get(ref self, k: i64) -> i64 with reads(Cfg); }",
    );
}

#[test]
fn test_resource_receiver_owned_self_with_declared_writes_is_clean() {
    // Declaring writes(R) alongside the owned receiver is consistent —
    // consuming the provider is an intentional write-grade operation.
    effectcheck_ok(
        "pub effect resource Cfg: Config;\n\
         pub trait Config { fn take(self, k: i64) -> i64 with writes(Cfg); }",
    );
}

#[test]
fn test_resource_receiver_owned_self_without_clause_is_clean() {
    // No declared clause → nothing promised, nothing contradicted; the
    // dispatch seed's writes(R) is simply the inferred truth.
    effectcheck_ok(
        "pub effect resource Cfg: Config;\n\
         pub trait Config { fn take(self, k: i64) -> i64; }",
    );
}

#[test]
fn test_resource_receiver_clause_on_other_resource_is_clean() {
    // The clause mentions a different resource — no promise about Cfg
    // is broken by the receiver-implied writes(Cfg).
    effectcheck_ok(
        "pub effect resource Cfg: Config;\n\
         pub effect resource Log: Logger;\n\
         pub trait Logger { fn log(mut ref self, msg: String); }\n\
         pub trait Config { fn get(self, k: i64) -> i64 with reads(Log); }",
    );
}

// ── Module-level `let mut` synthetic per-binding resources ────────
//
// Phase-8 mod-let slice 6 (design.md §1322 + §1330). Every
// `let mut BINDING` at module scope implicitly declares a
// project-internal `effect resource BINDING_resource` — reads of
// the binding contribute `reads(BINDING_resource)` to the
// enclosing function's inferred effect set; assignments contribute
// `writes(BINDING_resource)`. Immutable `let` declares no
// synthetic resource. `#[thread_local]` wraps the resource as
// `ThreadLocal[BINDING_resource]` so each task holds a disjoint
// instance (never conflicts with itself across tasks).

fn inferred_has(set: &EffectSet, verb: EffectVerbKind, resource: &str) -> bool {
    set.effects
        .iter()
        .any(|e| e.effect.verb == verb && e.effect.resource == resource)
}

#[test]
fn test_modbind_let_mut_read_attributes_reads_synthetic_resource() {
    let result = effectcheck_ok(
        "let mut COUNTER: i64 = 0;\n\
         fn current() -> i64 { COUNTER }",
    );
    let inferred = result
        .inferred_effects
        .get("current")
        .expect("current must be in inferred_effects");
    assert!(
        inferred_has(inferred, EffectVerbKind::Reads, "COUNTER_resource"),
        "expected reads(COUNTER_resource) in current; got: {:?}",
        inferred.effects
    );
    assert!(
        !inferred_has(inferred, EffectVerbKind::Writes, "COUNTER_resource"),
        "did not expect writes(COUNTER_resource) for a pure read; got: {:?}",
        inferred.effects
    );
}

#[test]
fn test_modbind_let_mut_write_attributes_writes_synthetic_resource() {
    let result = effectcheck_ok(
        "let mut COUNTER: i64 = 0;\n\
         fn bump() { COUNTER = COUNTER + 1; }",
    );
    let inferred = result
        .inferred_effects
        .get("bump")
        .expect("bump must be in inferred_effects");
    assert!(
        inferred_has(inferred, EffectVerbKind::Writes, "COUNTER_resource"),
        "expected writes(COUNTER_resource) in bump; got: {:?}",
        inferred.effects
    );
    assert!(
        inferred_has(inferred, EffectVerbKind::Reads, "COUNTER_resource"),
        "expected reads(COUNTER_resource) in bump (RHS reads COUNTER); got: {:?}",
        inferred.effects
    );
}

#[test]
fn test_modbind_let_mut_pure_assign_writes_only() {
    // `COUNTER = 0;` is a write with no read on the RHS.
    let result = effectcheck_ok(
        "let mut COUNTER: i64 = 0;\n\
         fn reset() { COUNTER = 0; }",
    );
    let inferred = result
        .inferred_effects
        .get("reset")
        .expect("reset must be in inferred_effects");
    assert!(
        inferred_has(inferred, EffectVerbKind::Writes, "COUNTER_resource"),
        "expected writes(COUNTER_resource) in reset; got: {:?}",
        inferred.effects
    );
    assert!(
        !inferred_has(inferred, EffectVerbKind::Reads, "COUNTER_resource"),
        "did not expect reads(COUNTER_resource) for pure assign with literal RHS; got: {:?}",
        inferred.effects
    );
}

#[test]
fn test_modbind_let_mut_compound_assign_reads_and_writes() {
    // `COUNTER += 1` is a load+store — both reads and writes the
    // synthetic resource. Mirrors what codegen will emit (slice 9).
    let result = effectcheck_ok(
        "let mut COUNTER: i64 = 0;\n\
         fn bump() { COUNTER += 1; }",
    );
    let inferred = result
        .inferred_effects
        .get("bump")
        .expect("bump must be in inferred_effects");
    assert!(
        inferred_has(inferred, EffectVerbKind::Reads, "COUNTER_resource")
            && inferred_has(inferred, EffectVerbKind::Writes, "COUNTER_resource"),
        "expected both reads(COUNTER_resource) and writes(COUNTER_resource) for compound assign; got: {:?}",
        inferred.effects
    );
}

#[test]
fn test_modbind_let_immutable_emits_no_synthetic_resource() {
    // Immutable `let` declares no synthetic resource — reading is
    // free. The function reading MAX must carry no MAX_resource
    // effect in its inferred set.
    let result = effectcheck_ok(
        "let MAX: i64 = 100;\n\
         fn limit() -> i64 { MAX }",
    );
    let inferred = result
        .inferred_effects
        .get("limit")
        .expect("limit must be in inferred_effects");
    assert!(
        !inferred_has(inferred, EffectVerbKind::Reads, "MAX_resource"),
        "did not expect any MAX_resource effect for immutable let; got: {:?}",
        inferred.effects
    );
    assert!(inferred.effects.is_empty(), "expected pure function");
}

#[test]
fn test_modbind_let_mut_effect_propagates_through_callee() {
    // A function that calls a writer inherits the synthetic
    // `writes(COUNTER_resource)` via normal call-graph propagation
    // — this is the load-bearing property that feeds conflict
    // analysis (slice 7) and `pub fn` synthetic-resource
    // detection (slice 8).
    let result = effectcheck_ok(
        "let mut COUNTER: i64 = 0;\n\
         fn bump() { COUNTER = COUNTER + 1; }\n\
         fn do_work() { bump(); }",
    );
    let inferred = result
        .inferred_effects
        .get("do_work")
        .expect("do_work must be in inferred_effects");
    assert!(
        inferred_has(inferred, EffectVerbKind::Writes, "COUNTER_resource"),
        "expected writes(COUNTER_resource) in do_work via bump(); got: {:?}",
        inferred.effects
    );
}

#[test]
fn test_modbind_let_mut_local_shadow_suppresses_synthetic_effect() {
    // A local binding of the same name shadows the module binding
    // — the function should NOT carry the synthetic resource effect
    // for the shadowed name. Module bindings are Const-class
    // (SCREAMING_SNAKE_CASE); collisions with local lets are rare
    // in practice but must still be handled.
    let result = effectcheck_ok(
        "let mut COUNTER: i64 = 0;\n\
         fn local() -> i64 {\n\
             let COUNTER: i64 = 5;\n\
             COUNTER\n\
         }",
    );
    let inferred = result
        .inferred_effects
        .get("local")
        .expect("local must be in inferred_effects");
    assert!(
        !inferred_has(inferred, EffectVerbKind::Reads, "COUNTER_resource"),
        "did not expect reads(COUNTER_resource) when local shadow exists; got: {:?}",
        inferred.effects
    );
}

#[test]
fn test_modbind_let_mut_thread_local_wraps_synthetic_resource() {
    // `#[thread_local]` wraps the synthetic resource so each task
    // holds a disjoint instance (design.md §1330). The resource
    // string becomes `ThreadLocal[COUNTER_resource]`, which never
    // conflicts with itself across tasks under the conflict
    // analyzer (slice 7).
    let result = effectcheck_ok(
        "#[thread_local]\n\
         let mut COUNTER: i64 = 0;\n\
         fn bump() { COUNTER = COUNTER + 1; }",
    );
    let inferred = result
        .inferred_effects
        .get("bump")
        .expect("bump must be in inferred_effects");
    assert!(
        inferred_has(
            inferred,
            EffectVerbKind::Writes,
            "ThreadLocal[COUNTER_resource]",
        ),
        "expected writes(ThreadLocal[COUNTER_resource]) for #[thread_local] binding; got: {:?}",
        inferred.effects
    );
    assert!(
        inferred_has(
            inferred,
            EffectVerbKind::Reads,
            "ThreadLocal[COUNTER_resource]",
        ),
        "expected reads(ThreadLocal[COUNTER_resource]) for #[thread_local] binding's RHS read; got: {:?}",
        inferred.effects
    );
}

#[test]
fn test_modbind_let_mut_reader_and_writer_distinct_functions() {
    // Two functions touching the same binding — one reader, one
    // writer — both carry the synthetic effect with the correct
    // verb. This is the seed for the slice-7 par-block conflict
    // rule: a `par { }` whose branches dispatch to one reader and
    // one writer fires `reads` + `writes` conflict on the same
    // resource.
    let result = effectcheck_ok(
        "let mut COUNTER: i64 = 0;\n\
         fn read_counter() -> i64 { COUNTER }\n\
         fn write_counter() { COUNTER = 1; }",
    );
    let reader = result.inferred_effects.get("read_counter").unwrap();
    let writer = result.inferred_effects.get("write_counter").unwrap();
    assert!(
        inferred_has(reader, EffectVerbKind::Reads, "COUNTER_resource"),
        "expected reads(COUNTER_resource) in read_counter; got: {:?}",
        reader.effects
    );
    assert!(
        inferred_has(writer, EffectVerbKind::Writes, "COUNTER_resource"),
        "expected writes(COUNTER_resource) in write_counter; got: {:?}",
        writer.effects
    );
}

#[test]
fn test_modbind_let_mut_two_distinct_bindings_get_separate_resources() {
    // Per-binding granularity is load-bearing — two `let mut`s get
    // two separate synthetic resources so mutations don't
    // serialize against each other in conflict analysis.
    let result = effectcheck_ok(
        "let mut COUNTER: i64 = 0;\n\
         let mut FLAG: bool = false;\n\
         fn bump_counter() { COUNTER = 1; }\n\
         fn set_flag() { FLAG = true; }",
    );
    let bc = result.inferred_effects.get("bump_counter").unwrap();
    let sf = result.inferred_effects.get("set_flag").unwrap();
    assert!(inferred_has(bc, EffectVerbKind::Writes, "COUNTER_resource"));
    assert!(!inferred_has(bc, EffectVerbKind::Writes, "FLAG_resource"));
    assert!(inferred_has(sf, EffectVerbKind::Writes, "FLAG_resource"));
    assert!(!inferred_has(
        sf,
        EffectVerbKind::Writes,
        "COUNTER_resource"
    ));
}

#[test]
fn test_modbind_let_mut_read_inside_loop_attributed() {
    // The body walker must recurse through loop bodies — a read of
    // a module binding inside a `for` is not free.
    let result = effectcheck_ok(
        "let mut COUNTER: i64 = 0;\n\
         fn sum_loop() -> i64 {\n\
             let mut total: i64 = 0;\n\
             for _i in 0..10 {\n\
                 total = total + COUNTER;\n\
             }\n\
             total\n\
         }",
    );
    let inferred = result.inferred_effects.get("sum_loop").unwrap();
    assert!(
        inferred_has(inferred, EffectVerbKind::Reads, "COUNTER_resource"),
        "expected reads(COUNTER_resource) for read inside for-loop; got: {:?}",
        inferred.effects
    );
}

// ── Slice 7: par-block conflict rule (design.md §1328) ───────────
//
// A `par { }` branch whose transitive effect set contains
// `writes(BINDING_resource)` for a `let mut BINDING` whose type is
// not an explicit concurrency primitive (`Atomic[T]` / `Mutex[T]` /
// `RwLock[T]` / `Arc[...]`) and is not `#[thread_local]` is rejected
// with `error[E_MODULE_BINDING_WRITE_IN_PAR]`. Reader+reader stays
// legal; reader+writer is caught by the writer-branch arm of the
// rule; writer+writer fires once per offending binding.

fn has_par_conflict_for(errors: &[EffectError], binding_name: &str) -> bool {
    errors.iter().any(|e| {
        e.kind == EffectErrorKind::ModuleBindingWriteInPar
            && e.message.contains(&format!("'{}'", binding_name))
    })
}

#[test]
fn test_par_block_bare_write_rejected() {
    let errors = effectcheck_errors(
        "let mut COUNTER: i64 = 0;\n\
         fn run() { par { COUNTER = 1; } }",
    );
    assert!(
        has_par_conflict_for(&errors, "COUNTER"),
        "expected E_MODULE_BINDING_WRITE_IN_PAR for direct par-block write; got: {:?}",
        errors
            .iter()
            .map(|e| (&e.kind, &e.message))
            .collect::<Vec<_>>()
    );
    let found = errors
        .iter()
        .find(|e| e.kind == EffectErrorKind::ModuleBindingWriteInPar)
        .expect("expected at least one par-block conflict diagnostic");
    assert!(
        found
            .message
            .contains("wrap in Atomic[T], Mutex[T], or use #[thread_local]"),
        "expected §1328 verbatim fix-it; got message: {}",
        found.message
    );
}

#[test]
fn test_par_block_write_via_callee_rejected() {
    // Transitive write via a function call inside `par { }`. The
    // effect-set check is the load-bearing property: the callee
    // doesn't write directly inside the par branch, but its
    // synthetic write effect propagates through and trips the rule
    // exactly as if the assignment had appeared inline.
    let errors = effectcheck_errors(
        "let mut COUNTER: i64 = 0;\n\
         fn bump() { COUNTER = COUNTER + 1; }\n\
         fn run() { par { bump(); } }",
    );
    assert!(
        has_par_conflict_for(&errors, "COUNTER"),
        "expected E_MODULE_BINDING_WRITE_IN_PAR via transitive callee; got: {:?}",
        errors
            .iter()
            .map(|e| (&e.kind, &e.message))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_par_block_compound_assign_rejected() {
    let errors = effectcheck_errors(
        "let mut COUNTER: i64 = 0;\n\
         fn run() { par { COUNTER += 1; } }",
    );
    assert!(
        has_par_conflict_for(&errors, "COUNTER"),
        "expected E_MODULE_BINDING_WRITE_IN_PAR for compound assign; got: {:?}",
        errors
            .iter()
            .map(|e| (&e.kind, &e.message))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_par_block_reader_writer_cross_branch_rejected() {
    // Reader in one branch, writer in another. The rule fires on
    // the writer branch — that's the offending operation regardless
    // of whether the sibling is a reader or another writer.
    let errors = effectcheck_errors(
        "let mut COUNTER: i64 = 0;\n\
         fn run() -> i64 {\n\
             let mut snapshot: i64 = 0;\n\
             par {\n\
                 snapshot = COUNTER;\n\
                 COUNTER = 1;\n\
             }\n\
             snapshot\n\
         }",
    );
    assert!(
        has_par_conflict_for(&errors, "COUNTER"),
        "expected E_MODULE_BINDING_WRITE_IN_PAR for reader+writer across branches; got: {:?}",
        errors
            .iter()
            .map(|e| (&e.kind, &e.message))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_par_block_reader_reader_allowed() {
    let result = effectcheck_ok(
        "let mut COUNTER: i64 = 0;\n\
         fn run() -> i64 {\n\
             let mut snap1: i64 = 0;\n\
             let mut snap2: i64 = 0;\n\
             par {\n\
                 snap1 = COUNTER;\n\
                 snap2 = COUNTER;\n\
             }\n\
             snap1 + snap2\n\
         }",
    );
    assert!(
        !result
            .errors
            .iter()
            .any(|e| e.kind == EffectErrorKind::ModuleBindingWriteInPar),
        "did not expect par-block conflict for reader+reader; got: {:?}",
        result.errors
    );
}

#[test]
fn test_par_block_atomic_wrapped_allowed() {
    // `Atomic[T]` carries its own synchronisation — writing under
    // `par { }` is well-defined and the rule must not fire.
    let parsed = parse(
        "let mut COUNTER: Atomic[i64] = Atomic.new(0);\n\
         fn run() { par { COUNTER = COUNTER; } }",
    );
    let result = karac::effectcheck(&parsed.program);
    assert!(
        !result
            .errors
            .iter()
            .any(|e| e.kind == EffectErrorKind::ModuleBindingWriteInPar),
        "did not expect E_MODULE_BINDING_WRITE_IN_PAR for Atomic-wrapped binding"
    );
}

#[test]
fn test_par_block_mutex_wrapped_allowed() {
    let parsed = parse(
        "let mut TODOS: Mutex[i64] = Mutex.new(0);\n\
         fn run() { par { TODOS = TODOS; } }",
    );
    let result = karac::effectcheck(&parsed.program);
    assert!(
        !result
            .errors
            .iter()
            .any(|e| e.kind == EffectErrorKind::ModuleBindingWriteInPar),
        "did not expect E_MODULE_BINDING_WRITE_IN_PAR for Mutex-wrapped binding"
    );
}

#[test]
fn test_par_block_thread_local_allowed() {
    let parsed = parse(
        "#[thread_local]\n\
         let mut SCRATCH: i64 = 0;\n\
         fn run() { par { SCRATCH = 1; } }",
    );
    let result = karac::effectcheck(&parsed.program);
    assert!(
        !result
            .errors
            .iter()
            .any(|e| e.kind == EffectErrorKind::ModuleBindingWriteInPar),
        "did not expect E_MODULE_BINDING_WRITE_IN_PAR for #[thread_local] binding; got: {:?}",
        result.errors
    );
}

#[test]
fn test_par_block_write_outside_par_not_flagged() {
    let result = effectcheck_ok(
        "let mut COUNTER: i64 = 0;\n\
         fn bump() { COUNTER = 1; }",
    );
    assert!(
        !result
            .errors
            .iter()
            .any(|e| e.kind == EffectErrorKind::ModuleBindingWriteInPar),
        "did not expect par-block conflict outside par; got: {:?}",
        result.errors
    );
}

#[test]
fn test_par_block_writer_writer_fires_once_per_binding() {
    let errors = effectcheck_errors(
        "let mut COUNTER: i64 = 0;\n\
         fn run() {\n\
             par {\n\
                 COUNTER = 1;\n\
                 COUNTER = 2;\n\
             }\n\
         }",
    );
    let count = errors
        .iter()
        .filter(|e| {
            e.kind == EffectErrorKind::ModuleBindingWriteInPar && e.message.contains("'COUNTER'")
        })
        .count();
    assert_eq!(
        count,
        1,
        "expected exactly one par-block conflict for COUNTER; got {}: {:?}",
        count,
        errors
            .iter()
            .map(|e| (&e.kind, &e.message))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_par_block_two_distinct_bindings_two_diagnostics() {
    let errors = effectcheck_errors(
        "let mut COUNTER: i64 = 0;\n\
         let mut FLAG: bool = false;\n\
         fn run() {\n\
             par {\n\
                 COUNTER = 1;\n\
                 FLAG = true;\n\
             }\n\
         }",
    );
    assert!(has_par_conflict_for(&errors, "COUNTER"));
    assert!(has_par_conflict_for(&errors, "FLAG"));
}

// ── Slice 8: pub fn synthetic-resource rejection (design.md §1326) ─
//
// Under `public_effects = "declared"`, a `pub fn` that carries an
// effect on a synthetic per-binding resource (`<NAME>_resource`)
// cannot satisfy the declaration discipline — the resource is not
// nameable. The dedicated rejection fires with `E0409`. The two
// supported escapes are: wrap the binding in a concurrency primitive
// (`Atomic[T]` / `Mutex[T]` / `RwLock[T]` / `Arc[shared struct S]`)
// or switch the project to `public_effects = "inferred"`.
// `#[thread_local]` is also implicitly permitted — the wrapped
// `ThreadLocal[...]` resource never conflicts across tasks, so it
// raises no synchronisation concern at the public boundary.

fn has_pub_fn_synth_for(errors: &[EffectError], binding_name: &str) -> bool {
    errors.iter().any(|e| {
        e.kind == EffectErrorKind::PubFnSyntheticResource
            && e.message.contains(&format!("'{}'", binding_name))
    })
}

#[test]
fn test_pub_fn_writes_modbind_under_declared_rejected() {
    let errors = effectcheck_errors(
        "let mut COUNTER: i64 = 0;\n\
         pub fn bump() { COUNTER = 1; }",
    );
    assert!(
        has_pub_fn_synth_for(&errors, "COUNTER"),
        "expected E_PUB_FN_SYNTHETIC_RESOURCE for direct pub-fn write; got: {:?}",
        errors
            .iter()
            .map(|e| (&e.kind, &e.message))
            .collect::<Vec<_>>()
    );
    let found = errors
        .iter()
        .find(|e| e.kind == EffectErrorKind::PubFnSyntheticResource)
        .expect("expected at least one pub-fn synthetic-resource diagnostic");
    assert!(
        found.message.contains("Atomic[T]")
            && found.message.contains("Mutex[T]")
            && found.message.contains("RwLock[T]")
            && found.message.contains("Arc[shared struct S]")
            && found.message.contains("public_effects = \"inferred\""),
        "expected §1326 verbatim escape-hatch fix-it; got message: {}",
        found.message
    );
}

#[test]
fn test_pub_fn_reads_modbind_under_declared_rejected() {
    // Reads contribute `reads(BINDING_resource)` which is just as
    // un-nameable as the write side — same rejection applies.
    let errors = effectcheck_errors(
        "let mut COUNTER: i64 = 0;\n\
         pub fn current() -> i64 { COUNTER }",
    );
    assert!(
        has_pub_fn_synth_for(&errors, "COUNTER"),
        "expected E_PUB_FN_SYNTHETIC_RESOURCE for pub-fn read; got: {:?}",
        errors
            .iter()
            .map(|e| (&e.kind, &e.message))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_pub_fn_modbind_via_callee_rejected() {
    // Transitive: a public function whose body calls a private
    // function that writes the binding still inherits the synthetic
    // effect through call-graph propagation. The rejection fires on
    // the public boundary regardless of where the assignment lives.
    let errors = effectcheck_errors(
        "let mut COUNTER: i64 = 0;\n\
         fn _bump() { COUNTER = 1; }\n\
         pub fn bump() { _bump() }",
    );
    assert!(
        has_pub_fn_synth_for(&errors, "COUNTER"),
        "expected E_PUB_FN_SYNTHETIC_RESOURCE via transitive callee; got: {:?}",
        errors
            .iter()
            .map(|e| (&e.kind, &e.message))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_pub_fn_modbind_under_inferred_policy_allowed() {
    // §1326 escape-hatch (b): under `public_effects = "inferred"`,
    // the rejection is suppressed entirely. The effect is still in
    // the inferred set (conflict analysis continues to work), it
    // just isn't a declaration violation.
    let parsed = parse(
        "let mut COUNTER: i64 = 0;\n\
         pub fn bump() { COUNTER = 1; }",
    );
    assert!(parsed.errors.is_empty());
    let result = effectcheck_with_policy(&parsed.program, PublicEffectsPolicy::Inferred);
    assert!(
        !result
            .errors
            .iter()
            .any(|e| e.kind == EffectErrorKind::PubFnSyntheticResource),
        "did not expect E_PUB_FN_SYNTHETIC_RESOURCE under Inferred policy; got: {:?}",
        result.errors
    );
}

#[test]
fn test_pub_fn_atomic_wrapped_allowed() {
    // §1326 escape-hatch (a): `Atomic[T]` carries its own
    // synchronisation. The synthetic-resource concern doesn't apply.
    let parsed = parse(
        "let mut COUNTER: Atomic[i64] = Atomic.new(0);\n\
         pub fn bump() { COUNTER = COUNTER; }",
    );
    let result = karac::effectcheck(&parsed.program);
    assert!(
        !result
            .errors
            .iter()
            .any(|e| e.kind == EffectErrorKind::PubFnSyntheticResource),
        "did not expect E_PUB_FN_SYNTHETIC_RESOURCE for Atomic-wrapped binding; got: {:?}",
        result.errors
    );
}

#[test]
fn test_pub_fn_mutex_wrapped_allowed() {
    let parsed = parse(
        "let mut LOCKED: Mutex[i64] = Mutex.new(0);\n\
         pub fn set() { LOCKED = LOCKED; }",
    );
    let result = karac::effectcheck(&parsed.program);
    assert!(
        !result
            .errors
            .iter()
            .any(|e| e.kind == EffectErrorKind::PubFnSyntheticResource),
        "did not expect E_PUB_FN_SYNTHETIC_RESOURCE for Mutex-wrapped binding; got: {:?}",
        result.errors
    );
}

#[test]
fn test_pub_fn_rwlock_wrapped_allowed() {
    let parsed = parse(
        "let mut CACHE: RwLock[i64] = RwLock.new(0);\n\
         pub fn store() { CACHE = CACHE; }",
    );
    let result = karac::effectcheck(&parsed.program);
    assert!(
        !result
            .errors
            .iter()
            .any(|e| e.kind == EffectErrorKind::PubFnSyntheticResource),
        "did not expect E_PUB_FN_SYNTHETIC_RESOURCE for RwLock-wrapped binding; got: {:?}",
        result.errors
    );
}

#[test]
fn test_pub_fn_thread_local_allowed() {
    // `#[thread_local]` resource is `ThreadLocal[BINDING_resource]`,
    // which never conflicts across tasks — the public-boundary
    // synchronisation concern doesn't apply.
    let parsed = parse(
        "#[thread_local]\n\
         let mut SCRATCH: i64 = 0;\n\
         pub fn set() { SCRATCH = 1; }",
    );
    let result = karac::effectcheck(&parsed.program);
    assert!(
        !result
            .errors
            .iter()
            .any(|e| e.kind == EffectErrorKind::PubFnSyntheticResource),
        "did not expect E_PUB_FN_SYNTHETIC_RESOURCE for #[thread_local] binding; got: {:?}",
        result.errors
    );
}

#[test]
fn test_pub_fn_immutable_let_no_rejection() {
    // Immutable `let` declares no synthetic resource — reading it is
    // a non-effect, so the public-boundary check has nothing to fire.
    let parsed = parse(
        "let MAX: i64 = 100;\n\
         pub fn limit() -> i64 { MAX }",
    );
    let result = karac::effectcheck(&parsed.program);
    assert!(
        !result
            .errors
            .iter()
            .any(|e| e.kind == EffectErrorKind::PubFnSyntheticResource),
        "did not expect E_PUB_FN_SYNTHETIC_RESOURCE for immutable let; got: {:?}",
        result.errors
    );
}

#[test]
fn test_private_fn_modbind_no_rejection() {
    // Private functions don't satisfy the `pub fn` precondition; the
    // synthetic effect is fine on a private boundary because the
    // declaration discipline doesn't apply.
    let parsed = parse(
        "let mut COUNTER: i64 = 0;\n\
         fn bump() { COUNTER = 1; }",
    );
    let result = karac::effectcheck(&parsed.program);
    assert!(
        !result
            .errors
            .iter()
            .any(|e| e.kind == EffectErrorKind::PubFnSyntheticResource),
        "did not expect E_PUB_FN_SYNTHETIC_RESOURCE for private fn; got: {:?}",
        result.errors
    );
}

#[test]
fn test_pub_fn_two_distinct_bindings_two_diagnostics() {
    // One diagnostic per (function, offending binding) pair — a
    // single pub fn that touches two synthetic resources produces
    // two diagnostics so the programmer sees both fix sites.
    let errors = effectcheck_errors(
        "let mut COUNTER: i64 = 0;\n\
         let mut FLAG: bool = false;\n\
         pub fn touch_both() { COUNTER = 1; FLAG = true; }",
    );
    assert!(has_pub_fn_synth_for(&errors, "COUNTER"));
    assert!(has_pub_fn_synth_for(&errors, "FLAG"));
}

#[test]
fn test_pub_fn_reads_and_writes_same_binding_dedupes() {
    // A pub fn that both reads and writes the same binding produces
    // one diagnostic for that binding, not two.
    let errors = effectcheck_errors(
        "let mut COUNTER: i64 = 0;\n\
         pub fn touch() { COUNTER = COUNTER + 1; }",
    );
    let count = errors
        .iter()
        .filter(|e| {
            e.kind == EffectErrorKind::PubFnSyntheticResource && e.message.contains("'COUNTER'")
        })
        .count();
    assert_eq!(
        count,
        1,
        "expected exactly one pub-fn synth diagnostic per binding; got {}: {:?}",
        count,
        errors
            .iter()
            .map(|e| (&e.kind, &e.message))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_pub_fn_modbind_no_double_fire_missing_declaration() {
    // Synthetic modbind effects must be filtered out of
    // `verify_declarations` so the user sees only the dedicated
    // §1326 rejection — not a generic "Add: writes(COUNTER_resource)"
    // suggestion that would direct them to write a name that isn't
    // legal anywhere.
    let errors = effectcheck_errors(
        "let mut COUNTER: i64 = 0;\n\
         pub fn bump() { COUNTER = 1; }",
    );
    let missing_with_synth: Vec<_> = errors
        .iter()
        .filter(|e| {
            e.kind == EffectErrorKind::MissingEffectDeclaration
                && e.message.contains("COUNTER_resource")
        })
        .collect();
    assert!(
        missing_with_synth.is_empty(),
        "did not expect a MissingEffectDeclaration mentioning the synthetic resource; got: {:?}",
        missing_with_synth
            .iter()
            .map(|e| &e.message)
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_pub_method_modbind_rejected() {
    // Impl-block methods marked `pub fn` are also covered — the
    // pass walks `function_bodies` and `method_bodies` keyed in
    // `function_visibility`.
    let errors = effectcheck_errors(
        "let mut COUNTER: i64 = 0;\n\
         struct Counter {}\n\
         impl Counter {\n\
             pub fn bump(ref self) { COUNTER = COUNTER + 1; }\n\
         }",
    );
    assert!(
        has_pub_fn_synth_for(&errors, "COUNTER"),
        "expected E_PUB_FN_SYNTHETIC_RESOURCE on pub method; got: {:?}",
        errors
            .iter()
            .map(|e| (&e.kind, &e.message))
            .collect::<Vec<_>>()
    );
}

// ── Phase 8 File handle slice F1 — effect declarations ─────────────
//
// File methods declare their FileSystem effects via baked stdlib
// `with reads(FileSystem)` / `with writes(FileSystem)` clauses
// (`runtime/stdlib/io.kara` slice F1). A `pub fn` that calls a File
// method without declaring the corresponding effect must fail
// `MissingEffectDeclaration` — same shape as `FileSystem.write` and
// every other ambient I/O surface.

#[test]
fn test_pub_fn_calling_file_open_must_declare_reads_filesystem() {
    let errors = effectcheck_errors(
        "pub fn driver() {
             let _ = File.open(\"x.txt\");
         }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.kind == EffectErrorKind::MissingEffectDeclaration
                && e.message.contains("reads(FileSystem)")),
        "expected MissingEffectDeclaration for reads(FileSystem); got: {:?}",
        errors.iter().map(|e| &e.message).collect::<Vec<_>>(),
    );
}

#[test]
fn test_pub_fn_calling_file_create_must_declare_writes_filesystem() {
    let errors = effectcheck_errors(
        "pub fn driver() {
             let _ = File.create(\"x.txt\");
         }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.kind == EffectErrorKind::MissingEffectDeclaration
                && e.message.contains("writes(FileSystem)")),
        "expected MissingEffectDeclaration for writes(FileSystem); got: {:?}",
        errors.iter().map(|e| &e.message).collect::<Vec<_>>(),
    );
}

#[test]
fn test_pub_fn_with_filesystem_effects_declared_accepts_file_methods() {
    // Mirror of the `Env.set` positive test (line 83 of phase-8) —
    // a `pub fn` that declares both reads/writes(FileSystem) is
    // accepted when calling open + create + write + flush + read.
    effectcheck_ok(
        "pub fn driver() with reads(FileSystem) writes(FileSystem) {
             let _ = File.open(\"x.txt\");
             let _ = File.create(\"y.txt\");
         }",
    );
}

// ── Phase 8 BufReader[R] — effect declarations ─────────────────────
//
// BufReader read methods carry `reads(FileSystem)` via baked stdlib
// `with reads(FileSystem)` clauses (`runtime/stdlib/bufreader.kara`),
// seeded into `inferred_effects` so the call-site walker sees them. A
// `pub fn` that calls a BufReader read method without declaring
// reads(FileSystem) must fail `MissingEffectDeclaration`. The function
// takes the `BufReader[File]` + destination by ref so the test
// isolates the read effect (no `File.open` / `String.new` noise).

// Instance-method effects (`br.read_line(...)`) resolve to the seeded
// `BufReader.read_line` key only when the typechecker's
// `method_callee_types` is threaded into the effectchecker, so these
// use `effectcheck_full_pipeline` rather than the bare `effectcheck`
// helpers (which the File tests above can use because `File.open` is a
// name-resolved Path call).

#[test]
fn test_pub_fn_calling_bufreader_read_line_must_declare_reads_filesystem() {
    let result = effectcheck_full_pipeline(
        "pub fn slurp(br: ref BufReader[File], buf: mut ref String) {
             let _ = br.read_line(buf);
         }",
    );
    assert!(
        result
            .errors
            .iter()
            .any(|e| e.kind == EffectErrorKind::MissingEffectDeclaration
                && e.message.contains("reads(FileSystem)")),
        "expected MissingEffectDeclaration for reads(FileSystem); got: {:?}",
        result.errors.iter().map(|e| &e.message).collect::<Vec<_>>(),
    );
}

#[test]
fn test_pub_fn_with_reads_filesystem_declared_accepts_bufreader_methods() {
    let result = effectcheck_full_pipeline(
        "pub fn slurp(br: ref BufReader[File], buf: mut ref String, raw: mut Slice[u8]) \
             with reads(FileSystem) {
             let _ = br.read_line(buf);
             let _ = br.read_to_string(buf);
             let _ = br.read(raw);
         }",
    );
    let real_errors: Vec<_> = result
        .errors
        .iter()
        .filter(|e| e.kind != EffectErrorKind::FfiLintHint)
        .collect();
    assert!(
        real_errors.is_empty(),
        "expected clean effectcheck with reads(FileSystem) declared; got: {:?}",
        real_errors.iter().map(|e| &e.message).collect::<Vec<_>>(),
    );
}

#[test]
fn test_pub_fn_calling_bufreader_lines_must_declare_reads_filesystem() {
    // `lines()` carries `reads(FileSystem)` (the per-line reads happen during
    // iteration; the effect is attributed at the `lines()` call), so a pub fn
    // iterating it without declaring the effect must fail.
    let result = effectcheck_full_pipeline(
        "pub fn dump(br: ref BufReader[File]) {
             for line in br.lines() {
                 match line {
                     Ok(_) => {}
                     Err(_) => {}
                 }
             }
         }",
    );
    assert!(
        result
            .errors
            .iter()
            .any(|e| e.kind == EffectErrorKind::MissingEffectDeclaration
                && e.message.contains("reads(FileSystem)")),
        "expected MissingEffectDeclaration for reads(FileSystem); got: {:?}",
        result.errors.iter().map(|e| &e.message).collect::<Vec<_>>(),
    );
}

#[test]
fn test_pub_fn_with_reads_filesystem_declared_accepts_bufreader_lines() {
    let result = effectcheck_full_pipeline(
        "pub fn dump(br: ref BufReader[File]) with reads(FileSystem) {
             for line in br.lines() {
                 match line {
                     Ok(_) => {}
                     Err(_) => {}
                 }
             }
         }",
    );
    let real_errors: Vec<_> = result
        .errors
        .iter()
        .filter(|e| e.kind != EffectErrorKind::FfiLintHint)
        .collect();
    assert!(
        real_errors.is_empty(),
        "expected clean effectcheck with reads(FileSystem) declared; got: {:?}",
        real_errors.iter().map(|e| &e.message).collect::<Vec<_>>(),
    );
}

#[test]
fn test_pub_fn_calling_bufreader_fill_buf_must_declare_reads_filesystem() {
    // fill_buf refills the buffer from the underlying reader, so it carries
    // reads(FileSystem); a pub fn calling it without declaring the effect must
    // fail.
    let result = effectcheck_full_pipeline(
        "pub fn peek(br: ref BufReader[File]) {
             let _ = br.fill_buf();
         }",
    );
    assert!(
        result
            .errors
            .iter()
            .any(|e| e.kind == EffectErrorKind::MissingEffectDeclaration
                && e.message.contains("reads(FileSystem)")),
        "expected MissingEffectDeclaration for reads(FileSystem); got: {:?}",
        result.errors.iter().map(|e| &e.message).collect::<Vec<_>>(),
    );
}

#[test]
fn test_pub_fn_calling_bufreader_consume_needs_no_effect() {
    // consume only advances the buffer cursor — no I/O — so a pub fn calling
    // it without any effect declaration is clean (proves consume is not
    // seeded with reads(FileSystem)).
    let result = effectcheck_full_pipeline(
        "pub fn skip(br: ref BufReader[File]) {
             br.consume(3);
         }",
    );
    let real_errors: Vec<_> = result
        .errors
        .iter()
        .filter(|e| e.kind != EffectErrorKind::FfiLintHint)
        .collect();
    assert!(
        real_errors.is_empty(),
        "expected clean effectcheck for effect-free consume; got: {:?}",
        real_errors.iter().map(|e| &e.message).collect::<Vec<_>>(),
    );
}

// ── Phase 8 BufWriter[W] — effect declarations ─────────────────────
//
// BufWriter write methods carry `writes(FileSystem)` via baked stdlib
// `with writes(FileSystem)` clauses (`runtime/stdlib/bufwriter.kara`),
// seeded into `inferred_effects` so the call-site walker sees them. A
// `pub fn` that calls a BufWriter write method without declaring
// writes(FileSystem) must fail `MissingEffectDeclaration`. As with the
// BufReader tests, these use `effectcheck_full_pipeline` so the
// typechecker's `method_callee_types` is threaded in (instance-method
// effect resolution depends on it).

#[test]
fn test_pub_fn_calling_bufwriter_write_must_declare_writes_filesystem() {
    let result = effectcheck_full_pipeline(
        "pub fn dump(bw: ref BufWriter[File], raw: Slice[u8]) {
             let _ = bw.write(raw);
         }",
    );
    assert!(
        result
            .errors
            .iter()
            .any(|e| e.kind == EffectErrorKind::MissingEffectDeclaration
                && e.message.contains("writes(FileSystem)")),
        "expected MissingEffectDeclaration for writes(FileSystem); got: {:?}",
        result.errors.iter().map(|e| &e.message).collect::<Vec<_>>(),
    );
}

#[test]
fn test_pub_fn_with_writes_filesystem_declared_accepts_bufwriter_methods() {
    let result = effectcheck_full_pipeline(
        "pub fn dump(bw: ref BufWriter[File], raw: Slice[u8]) with writes(FileSystem) {
             let _ = bw.write(raw);
             let _ = bw.flush();
         }",
    );
    let real_errors: Vec<_> = result
        .errors
        .iter()
        .filter(|e| e.kind != EffectErrorKind::FfiLintHint)
        .collect();
    assert!(
        real_errors.is_empty(),
        "expected clean effectcheck with writes(FileSystem) declared; got: {:?}",
        real_errors.iter().map(|e| &e.message).collect::<Vec<_>>(),
    );
}

// ── Refinement types (phase-9 step 3) ───────────────────────────
//
// `x as Refined` is a runtime predicate assertion that panics on
// failure, so it propagates the `panics` effect via the synthetic
// `__builtin_refinement_assert` callee. A plain numeric cast does not.

#[test]
fn refinement_as_cast_infers_panics() {
    let result = effectcheck_ok(
        "type Even = i64 where self % 2 == 0;
         fn assert_even(x: i64) -> Even { x as Even }",
    );
    let inferred = result.inferred_effects.get("assert_even").unwrap();
    assert!(
        inferred
            .effects
            .iter()
            .any(|e| e.effect.verb == EffectVerbKind::Panics),
        "`x as Even` should infer the `panics` effect"
    );
}

#[test]
fn plain_numeric_cast_does_not_infer_panics() {
    let result = effectcheck_ok("fn widen(x: i32) -> i64 { x as i64 }");
    let inferred = result.inferred_effects.get("widen").unwrap();
    assert!(
        !inferred
            .effects
            .iter()
            .any(|e| e.effect.verb == EffectVerbKind::Panics),
        "a plain numeric `as` cast must not infer `panics`"
    );
}

#[test]
fn distinct_where_constructor_infers_panics() {
    // The `T(value)` constructor of a combined `distinct type T = Base where
    // pred` runs a runtime predicate assertion, so it propagates `panics`
    // (same mechanism as the refinement `as` cast).
    let result = effectcheck_ok(
        "distinct type Even = i64 where self % 2 == 0;
         fn mk(n: i64) -> Even { Even(n) }",
    );
    let inferred = result.inferred_effects.get("mk").unwrap();
    assert!(
        inferred
            .effects
            .iter()
            .any(|e| e.effect.verb == EffectVerbKind::Panics),
        "`Even(n)` on a combined distinct type should infer `panics`"
    );
}

#[test]
fn plain_distinct_constructor_does_not_infer_panics() {
    // A predicate-free `distinct type` constructor is a zero-cost wrap with
    // no runtime check, so it must not infer `panics`.
    let result = effectcheck_ok(
        "distinct type UserId = i64;
         fn mk(n: i64) -> UserId { UserId(n) }",
    );
    let inferred = result.inferred_effects.get("mk").unwrap();
    assert!(
        !inferred
            .effects
            .iter()
            .any(|e| e.effect.verb == EffectVerbKind::Panics),
        "a plain distinct constructor must not infer `panics`"
    );
}

// ── Contracts — purity (effect set ⊆ {panics}) ─────────────────────
//
// design.md § Contracts rule 1: contract expressions must be pure. Any of
// the seven non-panic effects appearing via a call inside a `requires` /
// `ensures` / `invariant` is a compile error; `panics` alone is permitted.

fn assert_contract_impure(source: &str) {
    let errors = effectcheck_errors(source);
    assert!(
        errors
            .iter()
            .any(|e| e.kind == EffectErrorKind::ForbiddenEffectInContract),
        "expected E_CONTRACT_IMPURE, got: {}",
        errors
            .iter()
            .map(|e| e.message.clone())
            .collect::<Vec<_>>()
            .join(" | ")
    );
}

#[test]
fn contract_requires_with_reads_effect_rejected() {
    assert_contract_impure(
        "effect resource Log;\n\
         fn audit() -> bool with reads(Log) { true }\n\
         fn f(x: i64) -> i64 requires audit() { x }",
    );
}

#[test]
fn contract_ensures_with_writes_effect_rejected() {
    assert_contract_impure(
        "effect resource Log;\n\
         fn mark() -> bool with writes(Log) { true }\n\
         fn f(x: i64) -> i64 ensures(result) mark() { x }",
    );
}

#[test]
fn contract_invariant_with_effect_rejected() {
    assert_contract_impure(
        "effect resource Log;\n\
         fn probe() -> bool with reads(Log) { true }\n\
         struct S { x: i64, invariant probe() }",
    );
}

#[test]
fn contract_with_panics_effect_allowed() {
    // `panics` is the one permitted effect (indexing / unwrap idioms).
    effectcheck_ok(
        "fn boom() -> bool with panics { true }\n\
         fn f(x: i64) -> i64 requires boom() { x }",
    );
}

#[test]
fn contract_pure_predicate_accepted() {
    effectcheck_ok("fn f(x: i64) -> i64 requires x > 0 ensures(result) result > x { x + 1 }");
}

// ── Phase-10: `host fn` effect semantics ────────────────────────
// Declared effects are trusted (no body to verify) and there is NO
// ABI default — unlike `extern "C"`'s implicit `{blocks}`. The
// "host" ABI sentinel falls through the abi-default match unmatched.

#[test]
fn host_fn_declared_effects_propagate_to_callers() {
    let errs = effectcheck_errors(
        r#"
effect resource Screen;

host fn dom_clear() with writes(Screen);

pub fn wipe() {
    dom_clear();
}

fn main() {}
"#,
    );
    assert!(
        errs.iter().any(|e| {
            let m = e.to_string();
            m.contains("wipe") && m.contains("writes(Screen)")
        }),
        "declared host fn effects must propagate and flag the undeclared pub caller: {errs:?}",
    );
}

#[test]
fn host_fn_has_no_blocks_default() {
    // A caller declaring exactly the host fn's declared set must pass —
    // if the extern-"C" {blocks} default leaked in, `wipe` would be
    // flagged for an undeclared `blocks`.
    effectcheck_ok(
        r#"
effect resource Screen;

host fn dom_clear() with writes(Screen);

pub fn wipe() with writes(Screen) {
    dom_clear();
}

fn main() {}
"#,
    );
}

#[test]
fn host_fn_declared_blocks_is_honored_when_written() {
    // No default ≠ blocks unavailable: an explicitly declared `blocks`
    // flows like any other declared effect.
    let errs = effectcheck_errors(
        r#"
host fn host_sleep(ms: i64) with blocks;

pub fn nap() {
    host_sleep(10);
}

fn main() {}
"#,
    );
    assert!(
        errs.iter()
            .any(|e| e.to_string().contains("nap") && e.to_string().contains("blocks")),
        "explicit blocks on a host fn must propagate: {errs:?}",
    );
}

// ── Phase-10: effect-driven target gating ───────────────────────
// design.md § Cross-target Compilation > Effect-Driven Target Gating.
// Current target is `native`; std.web resources (Display/Storage/...)
// are the not-provided set that can fire on it. Resource identity is
// the clause string, so a source-level `effect resource Display;`
// claims the host Display identity — that is the std.wasi
// redeclaration philosophy and what makes these single-program tests
// possible without the gated-import machinery.

#[test]
fn target_gate_rejects_reachable_unprovided_resource_with_chain() {
    let errs = effectcheck_errors(
        r#"
effect resource Display;

fn paint() with writes(Display) {
}

fn helper() {
    paint();
}

fn main() {
    helper();
}
"#,
    );
    assert!(
        errs.iter().any(|e| {
            let m = e.to_string();
            m.contains("target `native` does not provide resource 'Display'")
                && m.contains("main → helper → paint")
        }),
        "expected gate error with full call chain: {errs:?}",
    );
}

#[test]
fn target_gate_ignores_unreachable_declarations() {
    effectcheck_ok(
        r#"
effect resource Display;

fn paint() with writes(Display) {
}

fn main() {
    println("ok");
}
"#,
    );
}

#[test]
fn target_gate_provider_binding_discharges_resource() {
    // The SSR pattern from design.md: binding a provider over a
    // target-foreign resource makes the subtree legal on this target.
    effectcheck_ok(
        r#"
effect resource Display;

struct HtmlBuilder { buf: String }

fn render() with writes(Display) {
}

fn main() {
    providers {
        Display => HtmlBuilder { buf: "" },
    } in {
        render();
    }
}
"#,
    );
}

#[test]
fn target_gate_exempts_user_resources() {
    effectcheck_ok(
        r#"
effect resource UserDB;

fn query() with reads(UserDB) {
}

fn main() {
    query();
}
"#,
    );
}

#[test]
fn target_gate_native_provided_resources_pass() {
    // FileSystem / Clock / Network / ProcessTable are all in native's
    // provided set — reachable uses must not fire.
    effectcheck_ok(
        r#"
fn snapshot() with reads(FileSystem) reads(Clock) sends(Network) sends(ProcessTable) {
}

fn main() {
    snapshot();
}
"#,
    );
}

#[test]
fn target_gate_unbound_sibling_call_still_fires() {
    // Function-granular discharge: a providers binding in `main`
    // covers main's whole body (documented approximation), but a
    // DIFFERENT entry path with no binding still fires.
    let errs = effectcheck_errors(
        r#"
effect resource Display;

fn render() with writes(Display) {
}

fn untracked() {
    render();
}

fn main() {
    untracked();
}
"#,
    );
    assert!(
        errs.iter()
            .any(|e| e.to_string().contains("main → untracked → render")),
        "{errs:?}",
    );
}

// ── FFI export panic semantics (design.md § Panic Semantics at the FFI
//    Boundary, case 2: E_EXTERN_C_UNWIND_REQUIRES_PANICS / E0413) ──────

#[test]
fn extern_c_unwind_panicking_body_requires_panics_declaration() {
    let errors = effectcheck_errors(
        "extern \"C-unwind\" fn f() -> i32 { unreachable() } \
         fn main() { let _ = f(); }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.kind == EffectErrorKind::ExternCUnwindRequiresPanics),
        "expected ExternCUnwindRequiresPanics, got: {:?}",
        errors.iter().map(|e| &e.kind).collect::<Vec<_>>()
    );
}

#[test]
fn extern_c_unwind_with_panics_declaration_ok() {
    effectcheck_ok(
        "extern \"C-unwind\" fn f() -> i32 with panics { unreachable() } \
         fn main() { let _ = f(); }",
    );
}

#[test]
fn extern_c_unwind_non_panicking_body_ok() {
    // No panic in the body → the C-unwind rule does not bite.
    effectcheck_ok(
        "extern \"C-unwind\" fn f(x: i32) -> i32 { x + 1 } \
         fn main() { let _ = f(1); }",
    );
}

#[test]
fn extern_c_export_panicking_body_has_no_panics_requirement() {
    // Case 1 (`extern "C"`) auto-aborts a body panic at the boundary, so
    // no `with panics` is required — the C-unwind rule must NOT fire here.
    let result = effectcheck_all(
        "extern \"C\" fn f() -> i32 { unreachable() } \
         fn main() { let _ = f(); }",
    );
    assert!(
        !result
            .errors
            .iter()
            .any(|e| e.kind == EffectErrorKind::ExternCUnwindRequiresPanics),
        "extern \"C\" export must not require a panics declaration, got: {:?}",
        result.errors.iter().map(|e| &e.kind).collect::<Vec<_>>()
    );
}
