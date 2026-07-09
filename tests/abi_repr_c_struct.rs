//! AArch64 `#[repr(C)]` struct-by-value ABI signature checks (B-2026-07-09-2).
//!
//! Kāra used to emit a raw LLVM struct for a `#[repr(C)]` struct param and rely
//! on the backend default, which matches SysV on x86-64 by luck but not AAPCS
//! on arm64 (silent wrong data — the `codegen-e2e-macos` finding). The fix
//! coerces the param per AAPCS. This suite proves the *emitted IR signature*
//! matches what `clang -target arm64-apple` produces for the same struct — and
//! identical IR lowers identically through LLVM, so a signature match IS an ABI
//! match. It runs without an arm64 machine by forcing the target arch via
//! `KARAC_FORCE_TARGET_ARCH`; the whole binary is invoked with that env set
//! (its own process — no interference with the host-target codegen suite). The
//! `codegen-e2e-macos` CI job is the paired real-execution confirmation.
//!
//! Run: `KARAC_FORCE_TARGET_ARCH=aarch64 cargo test --features llvm --test abi_repr_c_struct`

#![cfg(feature = "llvm")]

use karac::codegen::compile_to_ir;

/// Full front-end + codegen to LLVM IR text (mirrors `tests/codegen.rs::ir_for`).
fn ir_for(src: &str) -> String {
    let mut parsed = karac::parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    karac::lower(&mut parsed.program, &typed);
    compile_to_ir(&parsed.program, None, None).expect("codegen failed")
}

/// The `define ... @<fn>(...)` line for `fn_name` in `ir`.
fn define_line(ir: &str, fn_name: &str) -> String {
    ir.lines()
        .find(|l| l.starts_with("define") && l.contains(&format!("@{fn_name}(")))
        .unwrap_or_else(|| panic!("no define line for `{fn_name}` in:\n{ir}"))
        .to_string()
}

/// True only when the forced arch is actually in effect — otherwise the suite
/// is being run without the env (host x86-64), where no coercion happens and
/// these assertions don't apply. Each test early-returns in that case so a
/// plain `cargo test` doesn't spuriously fail.
fn arm64_forced() -> bool {
    std::env::var("KARAC_FORCE_TARGET_ARCH")
        .map(|v| v == "aarch64" || v == "arm64")
        .unwrap_or(false)
}

fn assert_param_coercion(struct_def: &str, field_ty: &str, expect_param: &str) {
    if !arm64_forced() {
        eprintln!("skip: KARAC_FORCE_TARGET_ARCH not set to aarch64");
        return;
    }
    let src = format!("{struct_def}\npub extern \"C\" fn probe(s: P) -> {field_ty} {{ s.a }}\n");
    let line = define_line(&ir_for(&src), "probe");
    assert!(
        line.contains(expect_param),
        "expected coerced param `{expect_param}` in:\n{line}"
    );
    // The raw struct type must NOT appear as the param (the bug signature).
    assert!(
        !line.contains("%P ") && !line.contains("%struct.P"),
        "param still raw-struct (uncoerced):\n{line}"
    );
}

#[test]
fn mixed_f64_i64_param_coerces_to_2xi64() {
    // clang arm64: define double @f([2 x i64] %0)
    assert_param_coercion(
        "#[repr(C)]\npub struct P { a: f64, b: i64 }",
        "f64",
        "[2 x i64]",
    );
}

#[test]
fn hfa_f64_f64_param_coerces_to_array_double() {
    // clang arm64: define double @f([2 x double] %0)
    assert_param_coercion(
        "#[repr(C)]\npub struct P { a: f64, b: f64 }",
        "f64",
        "[2 x double]",
    );
}

#[test]
fn hfa_f32_f32_param_coerces_to_array_float() {
    // clang arm64: [2 x float]
    assert_param_coercion(
        "#[repr(C)]\npub struct P { a: f32, b: f32 }",
        "f32",
        "[2 x float]",
    );
}

#[test]
fn hfa_single_f64_param_coerces_to_array1_double() {
    // clang arm64: [1 x double]
    assert_param_coercion("#[repr(C)]\npub struct P { a: f64 }", "f64", "[1 x double]");
}

#[test]
fn small_int_struct_param_coerces_to_i64() {
    // clang arm64: {i32,i32} (8 B) -> i64
    assert_param_coercion("#[repr(C)]\npub struct P { a: i32, b: i32 }", "i32", "i64");
}

#[test]
fn single_i32_param_coerces_to_i64() {
    // clang arm64: {i32} (4 B) -> i64 (widened)
    assert_param_coercion("#[repr(C)]\npub struct P { a: i32 }", "i32", "i64");
}
