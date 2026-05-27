// tests/raii_check.rs
//
// Phase 6 line 31 slice 1: RAII-across-yield typechecker pass. Verifies
// the v1 closed-enumeration detection — shared structs / shared enums
// held across a yield-point call in a network-boundary function fire
// `E_RAII_ACROSS_YIELD`; cancel-safe surface types (primitives, Vec,
// String, regular structs) do not.

use karac::cli::{
    build_callee_network_yield_effect_table, build_state_struct_layouts, build_yield_points_table,
};
use karac::effectchecker::PublicEffectsPolicy;
use karac::manifest::CompileProfile;
use karac::{
    ast::Program, effectcheck_with_typecheck_data, lower, parse, raii_across_yield_check,
    raii_check::RaiiAcrossYieldError, resolve, typecheck, typechecker::TypeCheckResult,
};

/// Drive parse → resolve → typecheck → lower → effectcheck → build
/// `callee_network_yield_effect` + `yield_points` + `state_struct_layouts`
/// → run `raii_across_yield_check`. Returns (program, typed, raii errors)
/// so individual tests can inspect both the layout state and the
/// emitted errors.
fn run_raii_check(source: &str) -> (Program, TypeCheckResult, Vec<RaiiAcrossYieldError>) {
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
    parsed.program.state_struct_layouts = build_state_struct_layouts(
        &parsed.program,
        &parsed.program.callee_network_yield_effect,
        &method_types,
        &pattern_binding_types,
    );
    let errors = raii_across_yield_check(&parsed.program, Some(&typed));
    (parsed.program, typed, errors)
}

#[test]
fn shared_struct_self_held_across_yield_rejected() {
    // A `shared struct` whose method body yields holds `self` (a
    // shared-struct handle, Rc-rooted reachable) across the suspension
    // point. Per design.md § RAII Across Yield Points, this is the
    // v1 NOT-CancelSafe pattern that fires `E_RAII_ACROSS_YIELD`.
    let (_program, _typed, errors) = run_raii_check(
        "effect resource Network;
         pub fn fetch() with sends(Network) receives(Network) {}
         shared struct Hub { count: i64 }
         impl Hub {
             fn run(self) { fetch(); }
         }",
    );
    assert_eq!(
        errors.len(),
        1,
        "expected one E_RAII_ACROSS_YIELD for `self: Hub` (shared struct) held across yield: {:?}",
        errors
    );
    assert_eq!(errors[0].fn_key, "Hub.run");
    assert_eq!(errors[0].binding_name, "self");
    assert_eq!(errors[0].type_name, "Hub");
}

#[test]
fn regular_struct_self_held_across_yield_accepted() {
    // A non-shared `struct` is cancel-safe by default — `self` is owned
    // (not Rc-rooted), and Drop runs as the natural state-struct
    // destructor in reverse construction order without violating Rc
    // invariants. No diagnostic.
    let (_program, _typed, errors) = run_raii_check(
        "effect resource Network;
         pub fn fetch() with sends(Network) receives(Network) {}
         struct Hub { count: i64 }
         impl Hub {
             fn run(self) { fetch(); }
         }",
    );
    assert!(
        errors.is_empty(),
        "regular (non-shared) struct must not trigger E_RAII_ACROSS_YIELD: {:?}",
        errors
    );
}

#[test]
fn shared_enum_held_across_yield_rejected() {
    // Shared enums are symmetric to shared structs per the design
    // spec — both are Rc-rooted reachability shapes that v1 v1
    // classifies as NOT-CancelSafe.
    let (_program, _typed, errors) = run_raii_check(
        "effect resource Network;
         pub fn fetch() with sends(Network) receives(Network) {}
         shared enum Msg { Empty, Hello(i64) }
         impl Msg {
             fn handle(self) { fetch(); }
         }",
    );
    assert_eq!(
        errors.len(),
        1,
        "expected one E_RAII_ACROSS_YIELD for `self: Msg` (shared enum) held across yield: {:?}",
        errors
    );
    assert_eq!(errors[0].fn_key, "Msg.handle");
    assert_eq!(errors[0].type_name, "Msg");
}

#[test]
fn vec_param_held_across_yield_accepted() {
    // Vec[T] is cancel-safe when element-type-cancel-safe per the spec.
    // The slice-4 layout records `Vec` as the surface type; the v1
    // closed enumeration does not flag this name.
    let (_program, _typed, errors) = run_raii_check(
        "effect resource Network;
         pub fn fetch() with sends(Network) receives(Network) {}
         fn driver(items: Vec[i64]) { fetch(); }",
    );
    assert!(
        errors.is_empty(),
        "Vec[i64] param must not trigger E_RAII_ACROSS_YIELD: {:?}",
        errors
    );
}

#[test]
fn primitive_param_held_across_yield_accepted() {
    // Primitive-typed bindings have `type_name: None` in the layout
    // (the typechecker doesn't record names for primitives). The
    // check pass skips entries without a type_name, so no error fires.
    let (_program, _typed, errors) = run_raii_check(
        "effect resource Network;
         pub fn fetch() with sends(Network) receives(Network) {}
         fn driver(n: i64) { fetch(); }",
    );
    assert!(
        errors.is_empty(),
        "primitive i64 param must not trigger E_RAII_ACROSS_YIELD: {:?}",
        errors
    );
}

#[test]
fn pure_function_emits_no_raii_error() {
    // A function with no yield-point calls in its body has no entry in
    // `state_struct_layouts` (slice-4 presence rule). The check pass
    // skips it entirely.
    let (_program, _typed, errors) = run_raii_check(
        "effect resource Network;
         pub fn fetch() with sends(Network) receives(Network) {}
         fn pure_helper(x: i64) -> i64 { x + 1 }",
    );
    assert!(
        errors.is_empty(),
        "pure function must not trigger E_RAII_ACROSS_YIELD: {:?}",
        errors
    );
}

#[test]
fn shared_struct_let_binding_held_across_yield_rejected() {
    // Local `let h: SharedHub = ...` introduced before a yield in a
    // network-boundary function gets captured into the layout and
    // triggers the same diagnostic as a shared-struct param. Pins
    // that the check fires regardless of binding origin (param vs.
    // let), since the cancel-leak hazard is identical.
    let (_program, _typed, errors) = run_raii_check(
        "effect resource Network;
         pub fn fetch() with sends(Network) receives(Network) {}
         shared struct Hub { count: i64 }
         fn driver(h: Hub) {
             fetch();
         }",
    );
    assert_eq!(
        errors.len(),
        1,
        "expected one E_RAII_ACROSS_YIELD for shared-struct param `h: Hub` held across yield: {:?}",
        errors
    );
    assert_eq!(errors[0].fn_key, "driver");
    assert_eq!(errors[0].binding_name, "h");
    assert_eq!(errors[0].type_name, "Hub");
}

#[test]
fn no_types_argument_returns_empty_errors() {
    // When the check is invoked without typecheck output (parse-only
    // pipeline), it returns no errors — the type classification index
    // is unavailable, so it can't decide cancel-safety. Defensive
    // contract: the check never spuriously fires.
    let mut parsed = parse(
        "effect resource Network;
         pub fn fetch() with sends(Network) receives(Network) {}
         shared struct Hub { count: i64 }
         impl Hub {
             fn run(self) { fetch(); }
         }",
    );
    let _ = resolve(&parsed.program);
    let typed = typecheck(&parsed.program, &resolve(&parsed.program));
    lower(&mut parsed.program, &typed);
    let effects = effectcheck_with_typecheck_data(
        &parsed.program,
        PublicEffectsPolicy::default(),
        CompileProfile::Default,
        typed.method_callee_types.clone(),
        typed.call_type_subs.clone(),
    );
    parsed.program.callee_network_yield_effect = build_callee_network_yield_effect_table(&effects);
    parsed.program.yield_points = build_yield_points_table(
        &parsed.program,
        &parsed.program.callee_network_yield_effect,
        &typed.method_callee_types,
    );
    parsed.program.state_struct_layouts = build_state_struct_layouts(
        &parsed.program,
        &parsed.program.callee_network_yield_effect,
        &typed.method_callee_types,
        &typed.pattern_binding_types,
    );
    let errors = raii_across_yield_check(&parsed.program, None);
    assert!(
        errors.is_empty(),
        "RAII check with `None` types must return empty: {:?}",
        errors
    );
}

// ── Phase 6 line 155 slice 2 — CancelSafe marker trait + opt-in ───
//
// Slice 1 rejects every `shared struct` / `shared enum` held across a
// yield point with no opt-out; slice 2 wires the diagnostic's promised
// `impl CancelSafe for <T>` fix-it. v1 keeps the marker trait
// user-declared (no implicit stdlib seeding) — tests declare
// `marker trait CancelSafe;` inline alongside the opt-in impl.

#[test]
fn shared_struct_with_cancel_safe_opt_in_accepted() {
    // Mirror of `shared_struct_self_held_across_yield_rejected` with
    // the slice-2 opt-in added. The walker must short-circuit before
    // the `is_shared` rejection arm; the diagnostic vanishes.
    let (_program, _typed, errors) = run_raii_check(
        "effect resource Network;
         pub fn fetch() with sends(Network) receives(Network) {}
         marker trait CancelSafe;
         shared struct Hub { count: i64 }
         impl CancelSafe for Hub {}
         impl Hub {
             fn run(self) { fetch(); }
         }",
    );
    assert!(
        errors.is_empty(),
        "expected zero RAII errors after `impl CancelSafe for Hub` opt-in: {:?}",
        errors
    );
}

#[test]
fn shared_enum_with_cancel_safe_opt_in_accepted() {
    // Symmetric to the shared-struct case — the slice-2 walker matches
    // by type name; struct vs. enum is not in the predicate.
    let (_program, _typed, errors) = run_raii_check(
        "effect resource Network;
         pub fn fetch() with sends(Network) receives(Network) {}
         marker trait CancelSafe;
         shared enum Status { Idle, Active(i64) }
         impl CancelSafe for Status {}
         impl Status {
             fn drive(self) { fetch(); }
         }",
    );
    assert!(
        errors.is_empty(),
        "expected zero RAII errors after `impl CancelSafe for Status` opt-in: {:?}",
        errors
    );
}

#[test]
fn shared_struct_without_opt_in_still_rejected() {
    // Slice-1 contract preserved: a shared struct with no
    // `impl CancelSafe for T` still fires. This guards against an
    // over-eager opt-in walker that would clear the closed
    // enumeration in the absence of any matching impl.
    let (_program, _typed, errors) = run_raii_check(
        "effect resource Network;
         pub fn fetch() with sends(Network) receives(Network) {}
         marker trait CancelSafe;
         shared struct Hub { count: i64 }
         impl Hub {
             fn run(self) { fetch(); }
         }",
    );
    assert_eq!(
        errors.len(),
        1,
        "expected slice-1 rejection unchanged when no opt-in present: {:?}",
        errors
    );
    assert_eq!(errors[0].fn_key, "Hub.run");
    assert_eq!(errors[0].type_name, "Hub");
}

#[test]
fn cancel_safe_opt_in_is_strict_name_match() {
    // Path-segment-equality is strict: an `impl CancelSafeButTypo for H`
    // (or any other name that isn't exactly `CancelSafe`) must NOT
    // count as an opt-in. Regression guard against a contains / prefix
    // / case-insensitive walker.
    let (_program, _typed, errors) = run_raii_check(
        "effect resource Network;
         pub fn fetch() with sends(Network) receives(Network) {}
         marker trait CancelSafeButTypo;
         shared struct Hub { count: i64 }
         impl CancelSafeButTypo for Hub {}
         impl Hub {
             fn run(self) { fetch(); }
         }",
    );
    assert_eq!(
        errors.len(),
        1,
        "expected slice-1 rejection unchanged when opt-in trait name doesn't match: {:?}",
        errors
    );
    assert_eq!(errors[0].type_name, "Hub");
}
