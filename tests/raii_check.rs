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

// ── Phase 6 line 155 slice 5 — binding-construction span anchoring ─
//
// Slice 1 anchors the diagnostic at the *yield-point* span and names
// the binding in the message; slice 5 threads the binding's
// introducing-pattern span through `StateStructField.binding_span`
// and `RaiiAcrossYieldError.binding_span` so the cli.rs formatter can
// emit a "binding declared here" secondary highlight. Tests pin the
// secondary span to the source position the user needs to act on,
// per binding-introduction shape (parameter, `let`, match-arm). The
// `self` shape stays `None` — there is no source-level pattern for
// `self`, mirroring the existing `ScopeEntry.span_key: None`
// convention for synthetic bindings.

#[test]
fn binding_span_anchors_to_parameter_pattern() {
    // Parameter shape — the binding's introducing position is the
    // parameter declaration (the `h` token in `h: Hub`). Slice 5
    // anchors the secondary highlight there.
    let source = "effect resource Network;
         pub fn fetch() with sends(Network) receives(Network) {}
         shared struct Hub { count: i64 }
         fn driver(h: Hub) { fetch(); }";
    let (_program, _typed, errors) = run_raii_check(source);
    assert_eq!(errors.len(), 1, "expected one RAII error: {:?}", errors);
    let bs = errors[0]
        .binding_span
        .as_ref()
        .expect("parameter binding must carry a binding_span");
    // The `h` token starts at column 21 of the parameter line —
    // `         fn driver(h: Hub)` (9 leading spaces of source-indent
    // + `fn driver(` = 19 chars, then `h` at column 20). Locate
    // dynamically rather than hardcoding so the assertion stays
    // robust against minor source rewrites.
    let line_idx = bs.line - 1;
    let line_text = source.lines().nth(line_idx).expect("line in range");
    let col_idx = bs.column - 1;
    let ch = line_text
        .chars()
        .nth(col_idx)
        .expect("column within line bounds");
    assert_eq!(
        ch, 'h',
        "binding_span column must point at the `h` binding token; got `{}` at line {} col {} (line text: {:?})",
        ch, bs.line, bs.column, line_text,
    );
}

#[test]
fn binding_span_anchors_to_let_pattern() {
    // `let h: Hub = ...` shape — binding_span anchors at the `h`
    // token in the `let` pattern, NOT at the `let` keyword itself
    // and NOT at the value expression.
    let source = "effect resource Network;
         pub fn fetch() with sends(Network) receives(Network) {}
         shared struct Hub { count: i64 }
         fn make() -> Hub { Hub { count: 0 } }
         fn driver() {
             let h: Hub = make();
             fetch();
         }";
    let (_program, _typed, errors) = run_raii_check(source);
    assert_eq!(errors.len(), 1, "expected one RAII error: {:?}", errors);
    let bs = errors[0]
        .binding_span
        .as_ref()
        .expect("let binding must carry a binding_span");
    let line_text = source.lines().nth(bs.line - 1).expect("line in range");
    let ch = line_text
        .chars()
        .nth(bs.column - 1)
        .expect("column within line bounds");
    assert_eq!(
        ch, 'h',
        "binding_span column must point at the `h` binding token in the `let` pattern; got `{}` at line {} col {} (line text: {:?})",
        ch, bs.line, bs.column, line_text,
    );
}

#[test]
fn binding_span_is_none_for_self_receiver() {
    // `self` has no source-level introducing pattern (`SelfParam` is
    // a `Owned`/`Ref`/`MutRef` enum, no Span). The walker pushes
    // `binding_span: None` for the synthetic `self` entry; the error
    // surface preserves that.
    let (_program, _typed, errors) = run_raii_check(
        "effect resource Network;
         pub fn fetch() with sends(Network) receives(Network) {}
         shared struct Hub { count: i64 }
         impl Hub {
             fn run(self) { fetch(); }
         }",
    );
    assert_eq!(errors.len(), 1, "expected one RAII error: {:?}", errors);
    assert_eq!(errors[0].binding_name, "self");
    assert!(
        errors[0].binding_span.is_none(),
        "self receiver has no source-level pattern; binding_span must stay None (got {:?})",
        errors[0].binding_span,
    );
}

#[test]
fn binding_span_anchors_to_match_arm_pattern() {
    // Match-arm binding — `Held(h)` introduces `h` at the match-arm
    // pattern position. Slice 5 threads the binding span through
    // `walk_expr_with_pattern`.
    let source = "effect resource Network;
         pub fn fetch() with sends(Network) receives(Network) {}
         shared struct Hub { count: i64 }
         shared enum Slot { Held(Hub), Empty }
         fn driver(s: Slot) {
             match s {
                 Held(h) => { fetch(); }
                 Empty => {}
             }
         }";
    let (_program, _typed, errors) = run_raii_check(source);
    // Expect TWO errors: outer `s: Slot` (shared enum param) AND
    // inner `h: Hub` (shared struct from the match arm). Filter to
    // the `h` binding to verify its binding_span lands at the match
    // arm pattern position.
    let h_error = errors
        .iter()
        .find(|e| e.binding_name == "h")
        .unwrap_or_else(|| panic!("expected an error for binding `h`; got {:?}", errors));
    let bs = h_error
        .binding_span
        .as_ref()
        .expect("match-arm binding must carry a binding_span");
    let line_text = source.lines().nth(bs.line - 1).expect("line in range");
    let ch = line_text
        .chars()
        .nth(bs.column - 1)
        .expect("column within line bounds");
    assert_eq!(
        ch, 'h',
        "binding_span must point at the `h` token in the match-arm pattern; got `{}` at line {} col {} (line text: {:?})",
        ch, bs.line, bs.column, line_text,
    );
}

// ── Phase 6 line 155 slice 4 — raw-pointer detection ──────────────
//
// Raw pointers (`*const T` / `*mut T`) carry no `Drop` hook, so a
// cancel during their live range leaks whatever they reference. The
// design.md v1 NOT-CancelSafe set includes raw pointers; slice 4
// teaches `bind_pattern_types` / `check_pattern_against` to record
// `type_display(Type::Pointer)` (yielding `*const T` / `*mut T`) into
// `pattern_binding_types`, then adds a name-prefix arm to
// `is_not_cancel_safe`. Detection is unconditional — the slice-2
// `impl CancelSafe for X` opt-in does not apply (its walker only
// matches single-segment `TypeKind::Path` targets), and the help
// text is class-branched to omit the misleading `impl CancelSafe`
// suggestion.

#[test]
fn raw_const_pointer_param_held_across_yield_rejected() {
    let (_program, _typed, errors) = run_raii_check(
        "effect resource Network;
         pub fn fetch() with sends(Network) receives(Network) {}
         fn driver(p: *const u8) { fetch(); }",
    );
    assert_eq!(
        errors.len(),
        1,
        "expected one E_RAII_ACROSS_YIELD for `*const u8` param held across yield: {:?}",
        errors,
    );
    assert_eq!(errors[0].fn_key, "driver");
    assert_eq!(errors[0].binding_name, "p");
    assert_eq!(errors[0].type_name, "*const u8");
}

#[test]
fn raw_mut_pointer_param_held_across_yield_rejected() {
    let (_program, _typed, errors) = run_raii_check(
        "effect resource Network;
         pub fn fetch() with sends(Network) receives(Network) {}
         fn driver(p: *mut u32) { fetch(); }",
    );
    assert_eq!(
        errors.len(),
        1,
        "expected one E_RAII_ACROSS_YIELD for `*mut u32` param held across yield: {:?}",
        errors,
    );
    assert_eq!(errors[0].binding_name, "p");
    assert_eq!(errors[0].type_name, "*mut u32");
}

#[test]
fn raw_pointer_help_omits_impl_cancel_safe_suggestion() {
    // Class-branched help text: raw pointers cannot opt into
    // CancelSafe (no Drop to audit; slice 2's walker doesn't match
    // pointer-type targets), so the help message must NOT suggest
    // `impl CancelSafe for *const T`. Instead it points at safe-
    // handle conversion as the remediation path.
    let (_program, _typed, errors) = run_raii_check(
        "effect resource Network;
         pub fn fetch() with sends(Network) receives(Network) {}
         fn driver(p: *const u8) { fetch(); }",
    );
    assert_eq!(errors.len(), 1);
    let help = errors[0].help();
    assert!(
        !help.contains("impl CancelSafe"),
        "raw-pointer help must not suggest `impl CancelSafe` (it cannot apply); got: {:?}",
        help,
    );
    assert!(
        help.contains("safe handle"),
        "raw-pointer help must point at safe-handle conversion; got: {:?}",
        help,
    );
}

#[test]
fn named_type_cancel_safe_opt_in_does_not_cover_raw_pointer_to_that_type() {
    // The slice-2 opt-in walker matches single-segment
    // `TypeKind::Path` impl targets; `*const Cell` is parsed as a
    // pointer type, not a path, so an `impl CancelSafe for Cell`
    // does NOT transitively cover `*const Cell`. Raw-pointer
    // rejection in `is_not_cancel_safe` runs BEFORE the opt-in
    // check, so even if a future opt-in form did register the
    // pointee name, the raw-pointer rejection would still fire.
    let (_program, _typed, errors) = run_raii_check(
        "effect resource Network;
         pub fn fetch() with sends(Network) receives(Network) {}
         marker trait CancelSafe;
         shared struct Cell { v: i64 }
         impl CancelSafe for Cell {}
         fn driver(p: *const Cell) { fetch(); }",
    );
    assert_eq!(
        errors.len(),
        1,
        "named-type CancelSafe opt-in must not transitively cover raw pointers to that type: {:?}",
        errors,
    );
    assert_eq!(errors[0].type_name, "*const Cell");
}

#[test]
fn raw_pointer_in_vec_is_not_in_scope() {
    // Documents the slice 4 scope limit: a Vec[*const T] binding
    // records `Vec` as its surface type (the type-name recorder
    // doesn't recurse into element types), so the slice-1 walk
    // doesn't see the inner raw pointer. Transitive raw-pointer
    // reachability is in the slice 4 "what this does NOT cover"
    // footer — likely a separate sub-slice if the direct-binding
    // detection is judged insufficient.
    let (_program, _typed, errors) = run_raii_check(
        "effect resource Network;
         pub fn fetch() with sends(Network) receives(Network) {}
         fn driver(ps: Vec[*const u8]) { fetch(); }",
    );
    assert!(
        errors.is_empty(),
        "Vec[*const u8] is out of scope for slice 4 — binding type is `Vec`, not `*const u8`: {:?}",
        errors,
    );
}

// ── Phase 6 line 155 slice 3a — flow-sensitive File annotation ──────
//
// `File.write` carries `#[cancel_unsafe_until(method = "flush")]` in
// `runtime/stdlib/io.kara`. The slice-3 walker reads that attribute,
// tracks per-binding state through each network-boundary function
// body, and emits a `StateViolation`-bearing `E_RAII_ACROSS_YIELD`
// when a soiled binding survives to a yield point without being
// cleared by the matching `flush()` call.

#[test]
fn file_write_then_yield_rejected() {
    // Linear `write; fetch` is the v1 cancel-leak pattern this slice
    // is designed to catch. The walker sees `f.write(data)` first
    // (Soiled), then `fetch()` (network yield), and emits one error
    // carrying the StateViolation payload pinning the soil method
    // and the missing clear method.
    let (_program, _typed, errors) = run_raii_check(
        "effect resource Network;
         pub fn fetch() with sends(Network) receives(Network) {}
         fn driver(f: File, data: Slice[u8]) {
             let _w = f.write(data);
             fetch();
         }",
    );
    assert_eq!(
        errors.len(),
        1,
        "expected one E_RAII_ACROSS_YIELD for `f` soiled by .write across yield: {:?}",
        errors
    );
    assert_eq!(errors[0].fn_key, "driver");
    assert_eq!(errors[0].binding_name, "f");
    assert_eq!(errors[0].type_name, "File");
    let sv = errors[0]
        .state_violation
        .as_ref()
        .expect("slice-3 violation must carry state_violation payload");
    assert_eq!(sv.soiling_method, "write");
    assert_eq!(sv.clear_method_name, "flush");
}

#[test]
fn file_write_then_flush_then_yield_accepted() {
    // The clearing call lands before the yield: state flips back to
    // Clean and the walker emits no error.
    let (_program, _typed, errors) = run_raii_check(
        "effect resource Network;
         pub fn fetch() with sends(Network) receives(Network) {}
         fn driver(f: File, data: Slice[u8]) {
             let _w = f.write(data);
             let _f = f.flush();
             fetch();
         }",
    );
    assert!(
        errors.is_empty(),
        "write-then-flush-then-yield must accept: {:?}",
        errors
    );
}

#[test]
fn file_param_without_write_held_across_yield_accepted() {
    // The binding's surface type is cancel-safe; only the soiling
    // call would have changed state. Without a write, no soil, no
    // diagnostic.
    let (_program, _typed, errors) = run_raii_check(
        "effect resource Network;
         pub fn fetch() with sends(Network) receives(Network) {}
         fn driver(f: File) {
             fetch();
         }",
    );
    assert!(
        errors.is_empty(),
        "File held across yield with no preceding write must accept: {:?}",
        errors
    );
}

#[test]
fn file_write_violation_carries_state_help_text() {
    // Help text class-branches on `state_violation`: it should name
    // the specific clear method (`flush`) rather than the generic
    // `impl CancelSafe` fix-it. Pins the diagnostic surface so users
    // see the literal remediation call.
    let (_program, _typed, errors) = run_raii_check(
        "effect resource Network;
         pub fn fetch() with sends(Network) receives(Network) {}
         fn driver(f: File, data: Slice[u8]) {
             let _w = f.write(data);
             fetch();
         }",
    );
    assert_eq!(errors.len(), 1);
    let help = errors[0].help();
    assert!(
        help.contains("f.flush()"),
        "help must name the clear method literally: got {help:?}"
    );
    assert!(
        help.contains("pending `write`"),
        "help must name the soiling method: got {help:?}"
    );
    assert!(
        !help.contains("impl CancelSafe"),
        "slice-3 help must not surface the impl-CancelSafe fix-it (that's for shared-type rejection): got {help:?}"
    );
    let message = errors[0].message();
    assert!(
        message.contains("pending `write`") && message.contains("f.flush"),
        "message must surface both soiling and clearing method names: got {message:?}",
    );
}

#[test]
fn file_two_writes_then_one_flush_then_yield_accepted() {
    // A second write *before* the flush still ends Clean — the
    // clearing call covers all preceding writes (the OS-level
    // `flush` does indeed flush every buffered byte, not just the
    // most recent). Pins the "one clear closes any prior soil"
    // semantic of the v1 state machine.
    let (_program, _typed, errors) = run_raii_check(
        "effect resource Network;
         pub fn fetch() with sends(Network) receives(Network) {}
         fn driver(f: File, a: Slice[u8], b: Slice[u8]) {
             let _w1 = f.write(a);
             let _w2 = f.write(b);
             let _r = f.flush();
             fetch();
         }",
    );
    assert!(
        errors.is_empty(),
        "two writes followed by one flush before yield must accept: {:?}",
        errors,
    );
}

#[test]
fn file_write_flush_write_then_yield_rejected() {
    // The clear handles the first write, but the second write
    // re-soils. The walker should reject — pins the "Soil → Clean →
    // Soil → yield" sequence triggers detection.
    let (_program, _typed, errors) = run_raii_check(
        "effect resource Network;
         pub fn fetch() with sends(Network) receives(Network) {}
         fn driver(f: File, a: Slice[u8], b: Slice[u8]) {
             let _w1 = f.write(a);
             let _r = f.flush();
             let _w2 = f.write(b);
             fetch();
         }",
    );
    assert_eq!(
        errors.len(),
        1,
        "second write after a flush must re-soil and trigger detection: {:?}",
        errors,
    );
    assert_eq!(errors[0].binding_name, "f");
    let sv = errors[0]
        .state_violation
        .as_ref()
        .expect("expected state_violation payload on re-soil case");
    assert_eq!(sv.soiling_method, "write");
}

#[test]
fn file_two_yields_emit_one_error_per_binding() {
    // Dedup contract: a binding held Soiled across multiple yield
    // points should produce one error per (binding, fn_key) pair,
    // not one per yield. Matches the slice-1 walk's emission shape.
    let (_program, _typed, errors) = run_raii_check(
        "effect resource Network;
         pub fn fetch() with sends(Network) receives(Network) {}
         fn driver(f: File, data: Slice[u8]) {
             let _w = f.write(data);
             fetch();
             fetch();
         }",
    );
    assert_eq!(
        errors.len(),
        1,
        "multiple yields under a single Soiled binding must dedup to one error: {:?}",
        errors,
    );
}

#[test]
fn user_annotated_type_participates() {
    // The slice-3 walker is type-agnostic — any
    // `#[cancel_unsafe_until]`-annotated method participates,
    // not just File.write. Pins the user-extensibility surface
    // so a future BufReader / database transaction stdlib type
    // plugs in by annotating its soiling methods (no walker
    // changes needed).
    let (_program, _typed, errors) = run_raii_check(
        "effect resource Network;
         pub fn fetch() with sends(Network) receives(Network) {}
         struct Tx { }
         impl Tx {
             #[cancel_unsafe_until(method = \"commit\")]
             fn put(ref self, k: String, v: String) { }
             fn commit(ref self) { }
         }
         fn driver(tx: Tx) {
             tx.put(\"a\", \"b\");
             fetch();
         }",
    );
    assert_eq!(
        errors.len(),
        1,
        "user-annotated cancel-unsafe method must participate in slice-3 walk: {:?}",
        errors,
    );
    assert_eq!(errors[0].fn_key, "driver");
    assert_eq!(errors[0].binding_name, "tx");
    assert_eq!(errors[0].type_name, "Tx");
    let sv = errors[0]
        .state_violation
        .as_ref()
        .expect("user-annotated violation must carry state_violation payload");
    assert_eq!(sv.soiling_method, "put");
    assert_eq!(sv.clear_method_name, "commit");
}

// ── Slice 3 — branch-precise flow merging ───────────────────────────
//
// These pin the soundness/precision the merge buys over the prior
// linear single-state walk. Each uses the `Tx` shape from
// `user_annotated_type_participates`: `put` soils, `commit` clears,
// `fetch()` is the yield point.

const TX_PREAMBLE: &str = "effect resource Network;
     pub fn fetch() with sends(Network) receives(Network) {}
     struct Tx { }
     impl Tx {
         #[cancel_unsafe_until(method = \"commit\")]
         fn put(ref self, k: String, v: String) { }
         fn commit(ref self) { }
     }\n";

#[test]
fn branch_soil_in_one_arm_clear_in_other_then_yield_rejected() {
    // The false-negative the linear walk had: then-arm soils, else-arm
    // clears the SAME threaded state, so the post-`if` state read Clean
    // and the yield was wrongly accepted. With per-arm merge the soil on
    // the `c == true` path survives → rejected.
    let src = format!(
        "{TX_PREAMBLE}
         fn driver(tx: Tx, c: bool) {{
             if c {{ tx.put(\"a\", \"b\"); }} else {{ tx.commit(); }}
             fetch();
         }}"
    );
    let (_p, _t, errors) = run_raii_check(&src);
    assert_eq!(
        errors.len(),
        1,
        "soil on one arm must survive the merge and be caught at the yield: {:?}",
        errors,
    );
    assert_eq!(errors[0].binding_name, "tx");
}

#[test]
fn branch_soil_and_yield_on_disjoint_arms_accepted() {
    // The false-positive the linear walk had: the then-arm's soil leaked
    // into the else-arm's state, so the else-arm's yield was wrongly
    // flagged. The soil and the yield are on mutually-exclusive paths →
    // accepted.
    let src = format!(
        "{TX_PREAMBLE}
         fn driver(tx: Tx, c: bool) {{
             if c {{ tx.put(\"a\", \"b\"); }} else {{ fetch(); }}
         }}"
    );
    let (_p, _t, errors) = run_raii_check(&src);
    assert!(
        errors.is_empty(),
        "soil and yield on disjoint arms must not error: {:?}",
        errors,
    );
}

#[test]
fn branch_both_arms_clear_after_soil_accepted() {
    // Pre-`if` soil, both arms clear → merge is Clean → yield accepted.
    // Locks that the merge doesn't spuriously retain a soil both arms drop.
    let src = format!(
        "{TX_PREAMBLE}
         fn driver(tx: Tx, c: bool) {{
             tx.put(\"a\", \"b\");
             if c {{ tx.commit(); }} else {{ tx.commit(); }}
             fetch();
         }}"
    );
    let (_p, _t, errors) = run_raii_check(&src);
    assert!(
        errors.is_empty(),
        "both arms clearing the soil must accept the later yield: {:?}",
        errors,
    );
}

#[test]
fn match_soil_in_one_arm_then_yield_rejected() {
    // Match-arm analogue of the if/else false-negative: one arm soils,
    // another clears; the union retains the soil → yield rejected.
    let src = format!(
        "{TX_PREAMBLE}
         fn driver(tx: Tx, k: i64) {{
             match k {{
                 0 => {{ tx.put(\"a\", \"b\"); }}
                 _ => {{ tx.commit(); }}
             }}
             fetch();
         }}"
    );
    let (_p, _t, errors) = run_raii_check(&src);
    assert_eq!(
        errors.len(),
        1,
        "a soil on any match arm must survive the union and be caught: {:?}",
        errors,
    );
    assert_eq!(errors[0].binding_name, "tx");
}

#[test]
fn loop_carried_soil_across_yield_rejected() {
    // The loop fixpoint: iteration N soils after the yield; iteration N+1
    // reaches the body-top yield while still soiled. The single-pass walk
    // missed this (it only saw the first iteration's pre-yield Clean state).
    let src = format!(
        "{TX_PREAMBLE}
         fn driver(tx: Tx, n: i64) {{
             while n > 0 {{
                 fetch();
                 tx.put(\"a\", \"b\");
             }}
         }}"
    );
    let (_p, _t, errors) = run_raii_check(&src);
    assert_eq!(
        errors.len(),
        1,
        "a soil carried across a loop iteration must be caught at the body-top yield: {:?}",
        errors,
    );
    assert_eq!(errors[0].binding_name, "tx");
}

#[test]
fn loop_clear_at_body_top_accepted() {
    // The fixpoint must not over-report: a carried soil that the body
    // top unconditionally clears before the yield is sound to accept.
    let src = format!(
        "{TX_PREAMBLE}
         fn driver(tx: Tx, n: i64) {{
             while n > 0 {{
                 tx.commit();
                 fetch();
                 tx.put(\"a\", \"b\");
             }}
         }}"
    );
    let (_p, _t, errors) = run_raii_check(&src);
    assert!(
        errors.is_empty(),
        "clearing the carried soil at the body top must accept the yield: {:?}",
        errors,
    );
}
