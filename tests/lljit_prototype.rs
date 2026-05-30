//! Phase-7 L560 W1 acceptance tests: orc2/LLJIT skeleton wrapper.
//!
//! W1 acceptance criterion (strengthened per L558 (a) finding 2026-05-29):
//! the skeleton must round-trip a libc external call on macOS arm64. If
//! the `printf` test below hangs the same way the MCJIT prototype did,
//! halt per the L560 W6 tripwire and surface the Cranelift question
//! before W2 — accumulating effort on a broken JIT foundation is the
//! footgun this gate exists to catch.

#![cfg(feature = "lljit_prototype")]

use karac::codegen::jit_run_main_lljit;

fn jit(src: &str) -> i32 {
    let mut parsed = karac::parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    karac::lower(&mut parsed.program, &typed);
    jit_run_main_lljit(&parsed.program, None, None).expect("jit_run_main_lljit failed")
}

// ── Smoke: pure-internal modules ───────────────────────────────────
// These passed under MCJIT too. Re-test under LLJIT to confirm the
// W1 skeleton doesn't regress the simple cases.

#[test]
fn lljit_runs_empty_main() {
    assert_eq!(jit("fn main() { }"), 0);
}

#[test]
fn lljit_runs_arithmetic_main() {
    assert_eq!(jit("fn main() { let _x = 2 + 3; }"), 0);
}

// ── W1 acceptance: external libc call ───────────────────────────────
// `print(42)` lowers to `@printf`. Under MCJIT this hung at PC=0 — the
// failure mode the L560 W1 gate exists to flush out. If THIS test
// passes, the orc2 + jitlink + DynamicLibrarySearchGenerator wiring
// holds and W1 closes.

#[test]
fn lljit_w1_acceptance_printf_roundtrip() {
    assert_eq!(jit("fn main() { print(42); }"), 0);
}

// ── Post-W1 probe battery ──────────────────────────────────────────
// W2 scope is "lifetime / ownership / multi-module"; these are still
// single-module but exercise the codegen surface areas that hung MCJIT
// (Vec/Map mono dispatch, f-string snprintf path, par-block runtime
// symbol). Passing here means W2's acceptance won't be gated by codegen
// surprises — it'll be focused on the lifetime story it's actually
// supposed to validate. Failing here means W2 starts with a known
// codegen issue to fix.

#[test]
fn lljit_post_w1_vec_push_libc_grow_path() {
    // The exact program that hung MCJIT's prototype — Vec.push grows
    // via `@malloc` / `@free` direct calls. JIT runs the grow path,
    // copies 0 bytes via memcpy (initial len=0), then frees on cleanup.
    assert_eq!(
        jit("fn main() { let v: Vec[i64] = Vec.new(); v.push(7); }"),
        0
    );
}

#[test]
fn lljit_post_w1_control_flow_while() {
    let src = "fn main() { let mut i: i64 = 0; while i < 10 { i = i + 1; } }";
    assert_eq!(jit(src), 0);
}

#[test]
fn lljit_post_w1_internal_function_call() {
    let src = "fn helper(x: i64) -> i64 { x + 1 }\nfn main() { let _y = helper(5); }";
    assert_eq!(jit(src), 0);
}

#[test]
fn lljit_post_w1_fstring_snprintf() {
    // f-string lowers to @snprintf for the integer interpolation.
    let src = "fn main() { let _s = f\"x = {1 + 2}\"; }";
    assert_eq!(jit(src), 0);
}

// ── W2 — lifetime / ownership story ───────────────────────────────
// Direct LLJITEngine access (the high-level `jit_run_main_lljit` builds
// the engine internally for one-shot use; W2 needs longer-lived engines
// to exercise multi-module + ResourceTracker).

use karac::codegen::{compile_to_ir, LLJITEngine};

fn ir(src: &str) -> String {
    let mut parsed = karac::parse(src);
    assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    karac::lower(&mut parsed.program, &typed);
    compile_to_ir(&parsed.program, None, None).expect("compile_to_ir")
}

/// Rename `main` → `<new_name>` in an LLVM-IR text blob. The compiler
/// only emits one `main` definition per program; for W2 multi-module
/// tests we need each module to define its own externally-named entry
/// so symbol-name collisions don't fire.
fn rename_main(ir: &str, new_name: &str) -> String {
    ir.replace("@main(", &format!("@{}(", new_name))
        .replace("@main ", &format!("@{} ", new_name))
}

/// W2.1 finding (2026-05-29): direct multi-module coexistence in the
/// main JITDylib fails. Every karac-compiled module emits the same
/// runtime-contract globals — `KARAC_SPAWN_SITES`, `KARAC_SPAWN_SITES_LEN`,
/// `KARAC_SPAWN_SITES_ENABLED` per the Debugger Contract (slice 3, see
/// `runtime/src/lib.rs` and `phase-6-runtime.md`). When module B installs
/// on top of module A, LLJIT rejects with a duplicate-definition error.
///
/// This is correct semantics — two definitions of the same symbol in
/// one symbol table genuinely collide. The two viable v1 patterns:
///
/// 1. **REPL cell shadowing** — each module installs under its own
///    `ResourceTracker`; the prior tracker's `.remove()` is called
///    before the next installs. See `lljit_w2_many_trackers_one_engine`.
///
/// 2. **Per-module JITDylib isolation** (W3+ work) — each module
///    installs into its own JITDylib, with the main JD as a linked
///    parent for libc/runtime resolution. Requires `LLVMOrcExecutionSessionCreateJITDylib`
///    plus `ExecutionSession::lookup` (the LLJIT C API only exposes
///    a main-JD lookup). Out of scope for W2.
///
/// This test pins down the limit so the next-pass surface design is
/// honest about which use case is currently supported.
#[test]
fn lljit_w2_finding_same_jd_collides_on_runtime_globals() {
    let ir_a = rename_main(&ir("fn main() { let _x = 1; }"), "m_first");
    let ir_b = rename_main(&ir("fn main() { let _y = 2; }"), "m_second");

    let engine = LLJITEngine::new().expect("engine");
    engine
        .add_ir_module(&ir_a)
        .expect("first module installs cleanly");
    let result = engine.add_ir_module(&ir_b);

    let err = result.expect_err("second module must fail on shared globals");
    assert!(
        err.contains("Duplicate definition"),
        "expected duplicate-definition error, got: {}",
        err
    );
    // The specific symbol name varies by which global the symbol-table
    // walker encounters first (`karac_jit_template_manifest`,
    // `KARAC_SPAWN_SITES_LEN`, `kara.string_table`, …); the constraint
    // is at the symbol-table level, not any one symbol. Documenting in
    // the test name + above docstring rather than asserting on a name
    // that drifts when codegen tables get reordered.
}

/// W2.2 — `add_ir_module_with_tracker` + `tracker.remove()` makes the
/// symbol unresolvable afterwards. Models the REPL shadowing flow:
/// cell N defines `compute`, cell N+1 re-defines `compute` — the
/// cell-N tracker is removed before cell N+1's module installs.
#[test]
fn lljit_w2_resource_tracker_removes_module() {
    let ir_v1 = rename_main(&ir("fn main() { let _v1 = 1; }"), "compute");
    let engine = LLJITEngine::new().expect("engine");
    let tracker = engine.add_ir_module_with_tracker(&ir_v1).expect("add v1");

    // Look up first — `compute` is reachable.
    let addr_before = engine.lookup_address("compute").expect("v1 lookup");
    assert_ne!(addr_before, 0);

    // Remove the module's resources via the tracker.
    tracker.remove().expect("tracker.remove");

    // After removal, the symbol must be gone — lookup fails.
    let after = engine.lookup_address("compute");
    assert!(
        after.is_err(),
        "compute should be unresolvable after tracker.remove; got {:?}",
        after
    );

    // Now install a fresh `compute` (cell N+1 case). New address must
    // differ from the removed-but-stale `addr_before`.
    let ir_v2 = rename_main(&ir("fn main() { let _v2 = 999; }"), "compute");
    let _tracker2 = engine.add_ir_module_with_tracker(&ir_v2).expect("add v2");
    let addr_after = engine.lookup_address("compute").expect("v2 lookup");
    assert_ne!(addr_after, 0);
    // Addresses might happen to collide if allocator reuses memory, so
    // we don't strictly require `assert_ne!(addr_before, addr_after)` —
    // the contract is "v1 is unresolvable, v2 is fresh", which the
    // previous asserts already verified.
}

/// W2.3 — Stress: many engine lifecycles in a single test process
/// without OOM/panic. Catches gross Drop hygiene issues (double-free,
/// leaked TS contexts) since each engine allocates LLVM machinery.
#[test]
fn lljit_w2_many_engine_lifecycles() {
    const ITERATIONS: usize = 100;
    let src = "fn main() { let _x = 41 + 1; }";
    let ir_text = ir(src);
    for _ in 0..ITERATIONS {
        let engine = LLJITEngine::new().expect("engine");
        engine.add_ir_module(&ir_text).expect("add module");
        let addr = engine.lookup_address("main").expect("main lookup");
        type Fn = unsafe extern "C" fn() -> i32;
        let main_fn: Fn = unsafe { std::mem::transmute(addr as usize) };
        assert_eq!(unsafe { main_fn() }, 0);
        // `engine` drops at end of iteration — engine teardown +
        // TS context dispose run here. If Drop misbehaves we'll see
        // it after ~100 iterations.
    }
}

/// W2.3-companion — Many trackers in a single engine. Same shape as
/// W2.3 but exercises the per-module tracker path, which has more
/// teardown steps (Remove + Release per tracker).
#[test]
fn lljit_w2_many_trackers_one_engine() {
    const ITERATIONS: usize = 100;
    let engine = LLJITEngine::new().expect("engine");
    for i in 0..ITERATIONS {
        let name = format!("cell_{}", i);
        let module_ir = rename_main(&ir("fn main() { let _x = 1; }"), &name);
        let tracker = engine
            .add_ir_module_with_tracker(&module_ir)
            .expect("add tracked");
        let addr = engine.lookup_address(&name).expect("lookup");
        type Fn = unsafe extern "C" fn() -> i32;
        let f: Fn = unsafe { std::mem::transmute(addr as usize) };
        assert_eq!(unsafe { f() }, 0);
        tracker.remove().expect("tracker.remove");
    }
}
