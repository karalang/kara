//! Phase-7 L560 W3.4 — subprocess helper that runs a karac-emitted
//! LLVM IR module through `LLJITEngine` and exits with the JIT'd
//! `main`'s return code.
//!
//! Used by `tests/codegen.rs::jit_dispatch` to route the codegen E2E
//! suite (~543 tests) through LLJIT without losing test-runner
//! isolation. When the JIT'd program calls `emit_panic`'s `exit(1)`
//! (runtime bounds check, map-miss, slice OOB, `?` Err return-and-
//! abort), the process termination only affects this child — `cargo
//! test`'s runner stays alive, `Command::output` captures stdout +
//! stderr + the non-zero exit code, exactly mirroring the AOT
//! object+link+spawn semantics.
//!
//! Usage: `karac_jit_runner <ir-path>`
//!
//! Exit codes:
//!   - `0..=N` — whatever the JIT'd `main` returned (0 = success,
//!     1 = `emit_panic`'s `exit(1)`, other = explicit user return).
//!   - `2` — helper setup failure (could not read IR, JIT init or
//!     `main` lookup failed). Diagnostic to stderr. Mirrors the AOT
//!     path's link-fail -> `None` shape semantically: callers treat
//!     this as "JIT couldn't even start," not as a JIT'd-program
//!     assertion result.
//!
//! Inside `cargo test`, the test binary locates this helper via
//! `env!("CARGO_BIN_EXE_karac_jit_runner")` — cargo guarantees it's
//! built before tests run when both share the same workspace.

use std::process::ExitCode;

use karac::codegen::LLJITEngine;

// ── KARAC_SPAWN_SITES stand-ins ──────────────────────────────────────
// Mirror of the test-binary stand-ins in `tests/codegen.rs` and
// `tests/lljit_e2e.rs`: the runtime crate declares these as `extern`
// under `#[cfg(not(test))]`, so the AOT user-binary path resolves
// them against codegen-emitted globals. JIT'd code emits its own
// per-module copies inside its JITDylib — the helper binary still
// needs satisfiers for the static rlib link of `karac-runtime`.
// `_ENABLED = 0` keeps `karac_runtime_has_debug_metadata` short-
// circuiting; `_LEN = 0` keeps the (unused) iteration paths no-op.
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

fn main() -> ExitCode {
    // Belt-and-suspenders: ensure the runtime's `#[no_mangle]` symbol
    // graph is materialized in the process symbol table before the
    // JIT's process-symbol-search generator runs `dlsym`.
    let _ = force_link_karac_runtime();

    let mut args = std::env::args();
    let _prog = args.next();
    let ir_path = match args.next() {
        Some(p) => p,
        None => {
            eprintln!("karac_jit_runner: missing IR path argv[1]");
            return ExitCode::from(2);
        }
    };

    let ir = match std::fs::read_to_string(&ir_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("karac_jit_runner: read IR {ir_path}: {e}");
            return ExitCode::from(2);
        }
    };

    let engine = match LLJITEngine::new() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("karac_jit_runner: LLJITEngine::new: {e}");
            return ExitCode::from(2);
        }
    };

    if let Err(e) = engine.add_ir_module(&ir) {
        eprintln!("karac_jit_runner: add_ir_module: {e}");
        return ExitCode::from(2);
    }

    let addr = match engine.lookup_address("main") {
        Ok(a) => a,
        Err(e) => {
            eprintln!("karac_jit_runner: lookup_address(\"main\"): {e}");
            return ExitCode::from(2);
        }
    };

    // SAFETY: `addr` is the JIT-resolved address of an LLVM-emitted
    // function with C ABI signature `fn() -> i32` (the Kāra entry
    // shape per `functions.rs`). The engine outlives this call.
    let rc: i32 = unsafe {
        type MainFn = unsafe extern "C" fn() -> i32;
        let main_fn: MainFn = std::mem::transmute(addr as usize);
        main_fn()
    };

    // ExitCode is u8; clamp negative or out-of-range i32 values to
    // 255 so a non-zero exit still signals failure to the parent.
    // The runtime's `emit_panic` uses `exit(1)`, which exits the
    // process before this branch is reached; this path only fires
    // when `main` returns normally with a value.
    let code: u8 = if (0..=255).contains(&rc) {
        rc as u8
    } else {
        255
    };
    ExitCode::from(code)
}
