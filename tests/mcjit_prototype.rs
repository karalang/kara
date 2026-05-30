//! Phase-7 L558 sub-step (a): MCJIT sanity-check prototype tests.
//!
//! Throwaway; gated behind `--features mcjit_prototype` so the normal
//! `cargo test --features llvm` run is unaffected. Removed when the
//! orc2 wrap (L560) lands.
//!
//! **Slice (a) finding.** inkwell 0.9 + LLVM 18 + macOS arm64 MCJIT
//! handles pure-internal modules (arithmetic, control flow, helper-fn
//! calls) correctly but hangs at PC=0 the moment the JITted module
//! calls any external symbol — libc (`malloc`, `free`, `printf`),
//! karac runtime, or other. `add_global_mapping` for libc fns did
//! NOT fix it; `get_function_address` returns a valid non-null
//! address; the JITted code itself jumps to 0 once execution reaches
//! an external call site. Sample confirms (single-stack frame at
//! PC=0x0). This is a known MCJIT/RuntimeDyld weakness on Apple
//! Silicon and is the L558 → L560 transition signal: orc2/LLJIT is
//! structurally necessary, not just "preferred". Apple Silicon is
//! a load-bearing dev platform (user hardware: M5 Pro per
//! `user_hardware`), so the W1 entry on L560 must include a smoke
//! test against this exact code shape (Vec.push and printf) before
//! committing to W2+.

#![cfg(feature = "mcjit_prototype")]

use karac::codegen::jit_run_main;

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
    jit_run_main(&parsed.program, None, None).expect("jit_run_main failed")
}

// ── Slice a.1: pure-internal modules ────────────────────────────────
// JIT executes correctly. Confirms the integration plumbing (Codegen
// → ExecutionEngine → call) works end-to-end for the no-externals
// subset.

#[test]
fn mcjit_runs_empty_main() {
    assert_eq!(jit("fn main() { }"), 0);
}

#[test]
fn mcjit_runs_arithmetic_main() {
    assert_eq!(jit("fn main() { let _x = 2 + 3; }"), 0);
}

#[test]
fn mcjit_runs_control_flow() {
    let src = "fn main() { let mut i: i64 = 0; while i < 10 { i = i + 1; } }";
    assert_eq!(jit(src), 0);
}

#[test]
fn mcjit_runs_internal_function_call() {
    let src = "fn helper(x: i64) -> i64 { x + 1 }\nfn main() { let _y = helper(5); }";
    assert_eq!(jit(src), 0);
}

// ── Slice a.2 + a.3: representative coverage with external calls ────
// Each of these probes one of the surface areas the orc2 wrap will
// need to handle. All currently `#[ignore]`'d because of the MCJIT
// external-relocation hang documented above. The orc2 wrap should
// re-enable them one-by-one as it lands W1–W6 — that's the actual
// shipping vehicle. These tests aren't deleted because they form a
// pre-baked regression battery the orc2 wrap can adopt.

#[test]
#[ignore = "MCJIT/arm64: external call hangs at PC=0; orc2 wrap will re-enable"]
fn mcjit_runs_vec_push_len() {
    // Vec is monomorphized — methods are inlined into the user
    // module — but `.push` still emits direct calls to `@malloc` and
    // `@free` (libc) for the grow path. Those external calls trigger
    // the hang. Smoke test for the orc2 wrap's W2 (multi-module).
    assert_eq!(
        jit("fn main() { let v: Vec[i64] = Vec.new(); v.push(7); }"),
        0
    );
}

#[test]
#[ignore = "MCJIT/arm64: external call hangs at PC=0; orc2 wrap will re-enable"]
fn mcjit_runs_print_int() {
    // `print(42)` → `@printf` libc call. Smallest external-call test.
    // Smoke test for the orc2 wrap's symbol-resolution surface.
    assert_eq!(jit("fn main() { print(42); }"), 0);
}

#[test]
#[ignore = "MCJIT/arm64: external call hangs at PC=0; orc2 wrap will re-enable"]
fn mcjit_runs_map_insert_get() {
    // Map[i64, i64] uses monomorphized symbols emitted into the user
    // module (no `karac_map_*` runtime), but still invokes `@malloc`
    // / `@free` for bucket arrays. Behaves like the Vec case from
    // MCJIT's perspective. Smoke test for the W3 codegen-suite gate.
    let src = "fn main() { let m: Map[i64, i64] = Map.new(); m.insert(1, 2); }";
    assert_eq!(jit(src), 0);
}

#[test]
#[ignore = "MCJIT/arm64: external call hangs at PC=0; orc2 wrap will re-enable"]
fn mcjit_runs_string_format() {
    // String handling lowers to `@snprintf` (libc) + UTF-8 decode runtime
    // helpers. Smoke test for the W3 codegen-suite gate.
    let src = "fn main() { let _s = f\"x = {1 + 2}\"; }";
    assert_eq!(jit(src), 0);
}

#[test]
#[ignore = "MCJIT/arm64: external call + runtime symbol; orc2 wrap will re-enable"]
fn mcjit_runs_par_block() {
    // `par {}` lowers to `karac_par_run` (karac runtime symbol). The
    // orc2 wrap's W1 risk list flagged this explicitly — auto-par
    // codegen must survive JIT load. This is the smoke test for that.
    let src = "fn main() {\n  par {\n    spawn { let _x = 1; }\n    spawn { let _y = 2; }\n  }\n}";
    assert_eq!(jit(src), 0);
}

#[test]
#[ignore = "MCJIT/arm64: external call hangs at PC=0; orc2 wrap will re-enable"]
fn mcjit_runs_result_question_mark() {
    // `?` lowers to runtime `karac_error_trace_push` + control flow.
    // Smoke test for error-return-trace integration under JIT.
    let src = "fn parse_int(_s: i64) -> Result[i64, String] { Ok(42) }\n\
               fn main() -> Result[Unit, String] { let _x = parse_int(0)?; Ok(()) }";
    assert_eq!(jit(src), 0);
}
