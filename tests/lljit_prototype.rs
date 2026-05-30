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
