//! Phase-7 L558 sub-step (a): MCJIT sanity-check prototype tests.
//!
//! Throwaway; gated behind `--features mcjit_prototype` so the normal
//! `cargo test --features llvm` run is unaffected. Removed when the
//! orc2 wrap (L560) lands.

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

/// Slice a.1 — the minimum sanity check: a runtime-free `main` round-trips
/// through MCJIT and returns the C-ABI exit code (0 on success). No Vec /
/// Map / print / par-block / `?` — those land in slice a.2 + a.3 once
/// runtime symbol resolution is wired up.
#[test]
fn mcjit_runs_empty_main() {
    let code = jit("fn main() { }");
    assert_eq!(code, 0, "empty main should exit 0");
}

#[test]
fn mcjit_runs_arithmetic_main() {
    // No print, no heap — just exercises that the compiled body executes
    // without crashing. Return value is still the LLVM `main` i32 exit
    // code (0), not the let-binding's value.
    let code = jit("fn main() { let _x = 2 + 3; }");
    assert_eq!(code, 0);
}
