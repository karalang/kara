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

#[test]
fn big_nonhfa_param_passed_indirect_ptr() {
    // clang arm64: a > 16 B non-HFA struct is passed indirectly — the caller
    // allocates a copy and passes a `ptr`:
    //   define i64 @take(ptr nocapture noundef readonly %0)
    // (Slice 3a). We match the *type* (`ptr`); attributes are opt hints.
    assert_param_coercion(
        "#[repr(C)]\npub struct P { a: i64, b: i64, c: i64 }",
        "i64",
        "ptr",
    );
}

// ── Returns (Slice 2) ───────────────────────────────────────────────

/// Assert the `define <ret> @make(...)` return type for a fn returning `P`.
/// `expect_ret` is the exact token clang emits for the arm64 return.
fn assert_return_type(struct_def: &str, ctor: &str, expect_ret: &str) {
    if !arm64_forced() {
        eprintln!("skip: KARAC_FORCE_TARGET_ARCH not set to aarch64");
        return;
    }
    let src = format!("{struct_def}\npub extern \"C\" fn make() -> P {{ {ctor} }}\n");
    let line = define_line(&ir_for(&src), "make");
    let ret = line
        .strip_prefix("define ")
        .and_then(|s| s.split(" @make(").next())
        .unwrap_or("")
        .trim();
    assert_eq!(ret, expect_ret, "unexpected arm64 return type in:\n{line}");
}

#[test]
fn mixed_f64_i64_return_coerces_to_2xi64() {
    // clang arm64: define [2 x i64] @make()
    assert_return_type(
        "#[repr(C)]\npub struct P { a: f64, b: i64 }",
        "P { a: 1.5, b: 3 }",
        "[2 x i64]",
    );
}

#[test]
fn small_int_struct_return_coerces_to_i64() {
    // clang arm64: {i32,i32} (8 B) -> i64
    assert_return_type(
        "#[repr(C)]\npub struct P { a: i32, b: i32 }",
        "P { a: 1, b: 2 }",
        "i64",
    );
}

#[test]
fn big_nonhfa_return_uses_sret() {
    // clang arm64: a > 16 B non-HFA struct returns via sret — the fn returns
    // `void` and takes a leading `ptr sret(%struct.P)` result pointer (x8):
    //   define void @make(ptr sret(%struct.P) align 8 %0, i64 %1)
    // (Slice 3b). Assert both the void return AND the sret param.
    if !arm64_forced() {
        return;
    }
    let src = "#[repr(C)]\npub struct P { a: i64, b: i64, c: i64 }\n\
               pub extern \"C\" fn make(x: i64) -> P { P { a: x, b: x, c: x } }\n";
    let line = define_line(&ir_for(src), "make");
    assert!(
        line.starts_with("define void @make("),
        "sret return should make the fn `void`:\n{line}"
    );
    assert!(
        line.contains("sret("),
        "sret return should carry a leading `sret(...)` result param:\n{line}"
    );
}

#[test]
fn big_nonhfa_param_and_sret_return_combined() {
    // clang arm64: a fn taking a > 16 B struct by value AND returning one has
    // the sret pointer FIRST, then the indirect param pointer:
    //   define void @rt(ptr sret(%P) %0, ptr %1, i64 %2)
    // Proves the param-index +1 shift lands the Kāra params correctly.
    if !arm64_forced() {
        return;
    }
    let src = "#[repr(C)]\npub struct P { a: i64, b: i64, c: i64 }\n\
               pub extern \"C\" fn rt(s: P, d: i64) -> P { P { a: s.a + d, b: s.b, c: s.c } }\n";
    let line = define_line(&ir_for(src), "rt");
    assert!(
        line.starts_with("define void @rt(ptr sret("),
        "sret result pointer must be the FIRST param:\n{line}"
    );
    // Two `ptr` params (sret + indirect struct) then the scalar.
    assert!(
        line.matches("ptr ").count() >= 2 && line.contains("i64 %2"),
        "indirect param should follow the sret pointer, scalar last:\n{line}"
    );
}

#[test]
fn hfa_return_stays_raw_struct() {
    // clang arm64 returns an HFA as the raw struct (v-regs), NOT [2 x i64].
    // Kāra's raw `{ double, double }` return lowers to v0/v1 identically.
    if !arm64_forced() {
        return;
    }
    let src = "#[repr(C)]\npub struct P { a: f64, b: f64 }\n\
               pub extern \"C\" fn make() -> P { P { a: 1.0, b: 2.0 } }\n";
    let line = define_line(&ir_for(src), "make");
    assert!(
        line.contains("double, double") && !line.contains("[2 x i64]"),
        "HFA return should stay a raw 2-double struct:\n{line}"
    );
}
