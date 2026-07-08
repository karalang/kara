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

// In-process JIT link scaffolding (Linux). `jit_run_main_lljit` runs the
// JIT inside THIS test binary, so its process-symbol-search generator
// resolves `karac_*` runtime FFI via `dlsym` against this executable —
// which on ELF only sees symbols the binary links AND exports into
// `.dynsym`. Two pieces make that work, mirroring `tests/codegen.rs`:
//
//   1. Force-link the runtime so its `karac_*` symbols are actually in
//      the binary (the linker DCEs unreferenced archive members
//      otherwise); `build.rs` then exports them via
//      `--export-dynamic-symbol=karac_*`.
//   2. Provide binary-level stand-ins for the Debugger-Contract globals
//      (`KARAC_SPAWN_SITES*`) that the runtime references as externs the
//      *program* normally defines. JITted user modules carry their own
//      module-local defs; these only satisfy the test binary's own link.
//
// Without these the JIT fails to materialize `main`
// (`Symbols not found: [karac_runtime_*]`) and every test returns empty
// output — green on macOS (Mach-O `dlsym` needs no export flag) but red
// on Linux. See docs/spikes/lljit-productionization.md.
#[no_mangle]
#[allow(non_upper_case_globals)]
pub static KARAC_SPAWN_SITES_ENABLED: u8 = 0;

#[no_mangle]
#[allow(non_upper_case_globals)]
pub static KARAC_SPAWN_SITES_LEN: u32 = 0;

#[no_mangle]
#[allow(non_upper_case_globals)]
pub static KARAC_SPAWN_SITES: KaracSpawnSitesPad = KaracSpawnSitesPad([0; 4]);

#[repr(C, align(8))]
pub struct KaracSpawnSitesPad([u64; 4]);

unsafe impl Sync for KaracSpawnSitesPad {}

#[used]
static _FORCE_LINK_CALL_SITE: fn() -> usize = force_link_karac_runtime;

fn force_link_karac_runtime() -> usize {
    karac_runtime::__preserve_no_mangle_symbols()
}

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

/// Like [`ir`] but forces Level-2 DWARF debug info on (crash-diagnostics
/// Part 2), so the emitted IR carries `!llvm.dbg.cu` / `DISubprogram` /
/// per-instruction `!dbg` locations. Race-free (no `KARAC_DEBUG_INFO`
/// env mutation) so it composes with the parallel test runner.
fn ir_with_dwarf(src: &str) -> String {
    let mut parsed = karac::parse(src);
    assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    karac::lower(&mut parsed.program, &typed);
    karac::codegen::compile_to_ir_with_debug_info(&parsed.program, None, None)
        .expect("compile_to_ir_with_debug_info")
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

// ── W5 — error handling + threading edge cases ────────────────────────
// W5's scope per the L581 milestone list: harden the wrapper against
// failure modes (malformed IR, missing symbols, post-error reuse) and
// prove the thread-safety the type system advertises. The engine holds
// raw LLVM pointers so it is `!Send + !Sync` — concurrency means one
// engine per thread, all racing the process-wide native-target init.

/// W5 error-handling: garbage IR must surface as a clean `Err`, not a
/// panic, abort, or crash. Exercises the `LLVMParseIRInContext` failure
/// path in `parse_ir_into_tsm` (the memory buffer is consumed by the
/// parser even on failure, so this also guards against a double-free /
/// leak on the error edge).
#[test]
fn lljit_w5_malformed_ir_returns_err() {
    let engine = LLJITEngine::new().expect("engine");
    let result = engine.add_ir_module("this is definitely not valid LLVM IR {{{");
    assert!(
        result.is_err(),
        "malformed IR must return Err, got {:?}",
        result
    );
}

/// W5 error-handling: looking up a name that was never defined must
/// return `Err`, not address 0. A 0 address transmuted to a fn pointer
/// and called is a null-deref crash, so a clean error here is
/// load-bearing for the always-JIT dispatch path.
#[test]
fn lljit_w5_lookup_missing_symbol_returns_err() {
    let engine = LLJITEngine::new().expect("engine");
    engine
        .add_ir_module(&ir("fn main() { let _x = 1; }"))
        .expect("add module");
    let result = engine.lookup_address("no_such_symbol_anywhere");
    assert!(
        result.is_err(),
        "missing symbol lookup must return Err, got {:?}",
        result
    );
}

/// W5 error-handling: a failed `add_ir_module` (parse error) must not
/// poison the engine — a subsequent valid module still installs, looks
/// up, and runs. Guards against the LLVM gotcha where a failed parse
/// leaves the shared context in a half-populated state that breaks the
/// next module. If this regresses, `parse_ir_into_tsm` needs to parse
/// into a throwaway context and only graft onto the engine's TS context
/// on success.
#[test]
fn lljit_w5_engine_usable_after_parse_error() {
    let engine = LLJITEngine::new().expect("engine");
    assert!(
        engine.add_ir_module("garbage ir @@@").is_err(),
        "malformed IR should error"
    );
    engine
        .add_ir_module(&ir("fn main() { let _x = 7; }"))
        .expect("valid module installs after a prior parse error");
    let addr = engine.lookup_address("main").expect("main lookup");
    type Fn = unsafe extern "C" fn() -> i32;
    let f: Fn = unsafe { std::mem::transmute(addr as usize) };
    assert_eq!(unsafe { f() }, 0);
}

/// W5 threading: N threads each build + run + drop their own engine
/// concurrently. They all race `LLJITEngine::new`'s native-target init,
/// which `ensure_native_target_initialized`'s `OnceLock` serializes —
/// the regression guard for that fix. Each engine is independent (its
/// own JITDylib), so no cross-thread symbol sharing is involved; this
/// isolates the global-init race. Pre-guard, concurrent
/// `initialize_native` calls mutate LLVM's target registry from multiple
/// threads with no synchronization.
#[test]
fn lljit_w5_concurrent_engines_across_threads() {
    const THREADS: usize = 8;
    // Deterministic compute (sum 0..100 = 4950) so each thread does real
    // JIT work, but `main` still returns 0 — the assertion is "ran
    // cleanly on every thread", which is the no-race contract.
    let ir_text = ir("fn main() { \
        let mut acc: i64 = 0; let mut i: i64 = 0; \
        while i < 100 { acc = acc + i; i = i + 1; } \
       }");

    let handles: Vec<_> = (0..THREADS)
        .map(|t| {
            let ir_text = ir_text.clone();
            std::thread::spawn(move || {
                // Engine built inside the thread — it is `!Send` by
                // design, so ownership never crosses the boundary.
                let engine = LLJITEngine::new().expect("engine");
                engine.add_ir_module(&ir_text).expect("add module");
                let addr = engine.lookup_address("main").expect("main lookup");
                type Fn = unsafe extern "C" fn() -> i32;
                let f: Fn = unsafe { std::mem::transmute(addr as usize) };
                let rc = unsafe { f() };
                assert_eq!(rc, 0, "thread {t} main returned {rc}");
            })
        })
        .collect();

    for h in handles {
        h.join().expect("worker thread panicked");
    }
}

/// A minimal switch-resumed coroutine in the LLVM 18 (opaque-pointer)
/// coroutine ABI. The `@mycoro` ramp is marked `presplitcoroutine`, so it
/// is NOT a valid runnable function until `coro-split` rewrites it — the
/// `llvm.coro.*` intrinsics survive to instruction selection otherwise and
/// the function cannot be materialized. `@malloc` / `@free` resolve via the
/// process-symbol generator like any other libc call.
fn ir_presplit_coroutine() -> &'static str {
    r#"
declare ptr @malloc(i64)
declare void @free(ptr)
declare token @llvm.coro.id(i32, ptr, ptr, ptr)
declare i64 @llvm.coro.size.i64()
declare ptr @llvm.coro.begin(token, ptr)
declare i8 @llvm.coro.suspend(token, i1)
declare ptr @llvm.coro.free(token, ptr)
declare i1 @llvm.coro.end(ptr, i1, token)

define ptr @mycoro() presplitcoroutine {
entry:
  %id = call token @llvm.coro.id(i32 0, ptr null, ptr null, ptr null)
  %size = call i64 @llvm.coro.size.i64()
  %alloc = call ptr @malloc(i64 %size)
  %hdl = call ptr @llvm.coro.begin(token %id, ptr %alloc)
  br label %susp
susp:
  %s = call i8 @llvm.coro.suspend(token none, i1 false)
  switch i8 %s, label %suspend [i8 0, label %resume
                                i8 1, label %cleanup]
resume:
  br label %susp
cleanup:
  %mem = call ptr @llvm.coro.free(token %id, ptr %hdl)
  call void @free(ptr %mem)
  br label %suspend
suspend:
  %u = call i1 @llvm.coro.end(ptr %hdl, i1 false, token none)
  ret ptr %hdl
}
"#
}

/// Regression for phase-7-codegen.md L591: the JIT path must run the coro
/// correctness passes before installing a module. Without the
/// `coro-early,coro-split,coro-cleanup` run inside `parse_ir_into_tsm`, the
/// `presplitcoroutine` ramp keeps its un-lowered `llvm.coro.*` intrinsics
/// and LLJIT cannot materialize it — `lookup_address` (which forces
/// materialization) fails. With the fix, CoroSplit turns the ramp into an
/// ordinary function that returns a non-null coroutine handle.
///
/// (The suspended frame is intentionally leaked — resuming/destroying it
/// would require calling the ABI resume/destroy clones by raw offset, which
/// adds no coverage of the seam under test. lljit_prototype tests are not
/// run under ASAN.)
#[test]
fn lljit_coro_split_runs_on_jit_path() {
    let engine = LLJITEngine::new().expect("engine");
    engine
        .add_ir_module(ir_presplit_coroutine())
        .expect("add coroutine module (coro passes must run during install)");
    let addr = engine
        .lookup_address("mycoro")
        .expect("lookup mycoro — fails if coro-split did not run (intrinsics unlowered)");
    let ramp: extern "C" fn() -> *mut std::ffi::c_void = unsafe { std::mem::transmute(addr) };
    let handle = ramp();
    assert!(
        !handle.is_null(),
        "split coroutine ramp must return a non-null frame handle"
    );
}

/// Slice 3 — Level-2 DWARF debug info is **preserved through the JIT lane**.
///
/// The "wrapping done" criterion (phase-7-codegen.md L706) is "full codegen
/// E2E passes via JIT path *with Level 2 DWARF preserved*". DWARF emission
/// (`src/codegen/debug_info.rs`) is designed for the JIT lane in particular
/// (ORC's GDB JIT interface registers the in-process frames), but nothing
/// pinned that the metadata actually survives the JIT pipeline — the parsed
/// module goes through a data-layout/triple restamp + the `coro-*` pass run
/// (`run_coro_passes`) before LLJIT compiles it, either of which could drop
/// or invalidate `!dbg` metadata (CoroSplit clones functions; a mis-scoped
/// `DISubprogram` on a clone is a hard verifier error). This asserts, for a
/// spread of representative shapes, that the DWARF-carrying IR both (a)
/// genuinely carries the debug metadata (non-vacuity) and (b) still
/// materializes + runs `main` cleanly through `LLJITEngine`.
#[test]
fn lljit_dwarf_debug_info_preserved_through_jit() {
    // A spread across the codegen paths debug info attaches to: a bare
    // main, an internal-fn call (two `DISubprogram`s), a loop (multiple
    // `!dbg` line locations), and an external libc call via `print`.
    let cases = [
        "fn main() { let _x = 2 + 3; }",
        "fn helper(x: i64) -> i64 { x + 1 }\nfn main() { let _y = helper(5); }",
        "fn main() { let mut i: i64 = 0; while i < 10 { i = i + 1; } }",
        "fn main() { print(42); }",
    ];
    for src in cases {
        let dwarf_ir = ir_with_dwarf(src);
        // (a) Non-vacuity: the IR really carries DWARF. Without these the
        // test would pass on a debug-info-stripped module and prove nothing.
        assert!(
            dwarf_ir.contains("!llvm.dbg.cu"),
            "expected a DWARF compile unit in the IR for {src:?}"
        );
        assert!(
            dwarf_ir.contains("DISubprogram"),
            "expected a per-function DISubprogram in the IR for {src:?}"
        );
        assert!(
            dwarf_ir.contains("!DILocation"),
            "expected per-instruction !dbg locations in the IR for {src:?}"
        );
        // (b) The DWARF-carrying module survives parse → restamp → coro
        // passes → LLJIT compile and runs `main` to a clean exit. A dropped
        // or mis-scoped `!dbg` would surface here as an `add_ir_module` /
        // `lookup_address` `Err`, not a silent pass.
        let engine = LLJITEngine::new().expect("engine");
        engine
            .add_ir_module(&dwarf_ir)
            .unwrap_or_else(|e| panic!("add DWARF module for {src:?}: {e}"));
        let addr = engine
            .lookup_address("main")
            .unwrap_or_else(|e| panic!("lookup main for {src:?}: {e}"));
        type MainFn = unsafe extern "C" fn() -> i32;
        let main_fn: MainFn = unsafe { std::mem::transmute(addr as usize) };
        assert_eq!(
            unsafe { main_fn() },
            0,
            "DWARF-compiled `main` must run to a clean exit for {src:?}"
        );
    }
}
