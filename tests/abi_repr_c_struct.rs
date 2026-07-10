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

/// True when the x86-64 **SysV** struct ABI is in effect: either forced via the
/// env or running natively on an x86-64 Linux/macOS/BSD host (the common `cargo
/// test` case — the `codegen-e2e` CI job is x86-64, so these run there for
/// free). Native Windows x86-64 is EXCLUDED (it hits `win_x64_active` instead —
/// its aggregate ABI differs entirely from SysV, so applying SysV assertions
/// there would spuriously fail). Any other forced arch disables the SysV tests.
fn x86_64_active() -> bool {
    match std::env::var("KARAC_FORCE_TARGET_ARCH") {
        Ok(v) => v == "x86_64" || v == "x86-64" || v == "amd64",
        Err(_) => cfg!(target_arch = "x86_64") && !cfg!(target_os = "windows"),
    }
}

/// True when the **Windows x64** (Microsoft x64) struct ABI is in effect:
/// forced via `KARAC_FORCE_TARGET_ARCH=windows_x86_64` (or `win_x86_64` /
/// `x86_64-windows`), or running natively on a Windows x86-64 host. Gates the
/// B-2026-07-09-8 signature-match tests. Identical IR lowers identically
/// through LLVM, so a Linux CI runner with the env forced verifies the
/// coercions match — the Windows CI runner is not required for signature
/// checks (it is for the execution round-trip, tracked separately as
/// Stage 4).
fn win_x64_active() -> bool {
    match std::env::var("KARAC_FORCE_TARGET_ARCH") {
        Ok(v) => {
            v == "windows_x86_64"
                || v == "win_x86_64"
                || v == "x86_64-windows"
                || v == "x86_64_windows"
        }
        Err(_) => cfg!(target_arch = "x86_64") && cfg!(target_os = "windows"),
    }
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

// ── x86-64 SysV (Slice 3c) ──────────────────────────────────────────
//
// x86-64 matches the raw-struct lowering for ≤ 16 B (eightbyte register
// classification, by luck), so only the larger-than-16 B MEMORY case is
// adapted: a `byval` pointer param and an `sret` return. These run natively on
// the x86-64 `codegen-e2e` CI job (no forcing needed).

#[test]
fn x86_64_small_struct_param_stays_raw() {
    // ≤ 16 B: x86-64 keeps the raw struct (matches SysV eightbytes by luck).
    if !x86_64_active() {
        return;
    }
    let src = "#[repr(C)]\npub struct P { a: f64, b: i64 }\n\
               pub extern \"C\" fn probe(s: P) -> f64 { s.a }\n";
    let line = define_line(&ir_for(src), "probe");
    assert!(
        !line.contains("byval") && !line.contains("ptr "),
        "≤16B x86-64 param should stay raw (no byval/ptr):\n{line}"
    );
}

#[test]
fn x86_64_big_struct_param_uses_byval() {
    // clang x86-64: define i64 @f(ptr byval(%struct.P) align 8 %0)
    if !x86_64_active() {
        return;
    }
    let src = "#[repr(C)]\npub struct P { a: i64, b: i64, c: i64 }\n\
               pub extern \"C\" fn sum(s: P) -> i64 { s.a }\n";
    let line = define_line(&ir_for(src), "sum");
    assert!(
        line.contains("byval("),
        "x86-64 >16B param must be `ptr byval(...)`:\n{line}"
    );
}

#[test]
fn x86_64_big_struct_return_uses_sret() {
    // clang x86-64: define void @make(ptr sret(%struct.P) align 8 %0, i64 %1)
    if !x86_64_active() {
        return;
    }
    let src = "#[repr(C)]\npub struct P { a: i64, b: i64, c: i64 }\n\
               pub extern \"C\" fn make(x: i64) -> P { P { a: x, b: x, c: x } }\n";
    let line = define_line(&ir_for(src), "make");
    assert!(
        line.starts_with("define void @make(") && line.contains("sret("),
        "x86-64 >16B return must be void + `sret(...)`:\n{line}"
    );
}

#[test]
fn x86_64_big_struct_combined_sret_then_byval() {
    // clang x86-64: void @rt(ptr sret(%P) %0, ptr byval(%P) %1, i64 %2)
    if !x86_64_active() {
        return;
    }
    let src = "#[repr(C)]\npub struct P { a: i64, b: i64, c: i64 }\n\
               pub extern \"C\" fn rt(s: P, d: i64) -> P { P { a: s.a + d, b: s.b, c: s.c } }\n";
    let line = define_line(&ir_for(src), "rt");
    assert!(
        line.starts_with("define void @rt(ptr sret("),
        "sret result pointer must be first:\n{line}"
    );
    assert!(
        line.contains("byval(") && line.contains("i64 %2"),
        "byval struct param must follow the sret pointer, scalar last:\n{line}"
    );
}

// ── Windows x64 (Microsoft x64, B-2026-07-09-8) ────────────────────
//
// The Microsoft x64 aggregate ABI differs from SysV: an aggregate goes in a
// single integer register ONLY at exact 1/2/4/8-byte POT sizes (no eightbyte
// splitting, no HFA); every other size is passed **by reference** — the caller
// places a copy on the stack and passes a plain `ptr` (NO `byval` attribute —
// the caller-owned-copy semantics don't need LLVM's byval model). Returns
// follow the same POT-≤-8 rule (RAX via coerced `iN`); everything else uses
// `sret`. These tests run under `KARAC_FORCE_TARGET_ARCH=windows_x86_64` on
// any host (identical IR ⇒ identical ABI) or natively on Windows.

fn assert_win_x64_param(struct_def: &str, field_ty: &str, expect_param: &str) {
    if !win_x64_active() {
        eprintln!("skip: Windows x64 struct ABI not active");
        return;
    }
    let src = format!("{struct_def}\npub extern \"C\" fn probe(s: P) -> {field_ty} {{ s.a }}\n");
    let line = define_line(&ir_for(&src), "probe");
    assert!(
        line.contains(expect_param),
        "expected coerced param `{expect_param}` in:\n{line}"
    );
    // Windows never uses byval — the caller allocates the copy and passes
    // its address; a plain `ptr` captures the convention.
    assert!(
        !line.contains("byval("),
        "Windows x64 param must NOT carry `byval` (caller-owned copy convention):\n{line}"
    );
    // The raw struct type must NOT appear as the param.
    assert!(
        !line.contains("%P ") && !line.contains("%struct.P"),
        "param still raw-struct (uncoerced):\n{line}"
    );
}

#[test]
fn win_x64_single_i32_param_coerces_to_i32() {
    // 4-byte aggregate → single integer register (i32).
    assert_win_x64_param("#[repr(C)]\npub struct P { a: i32 }", "i32", "i32");
}

#[test]
fn win_x64_single_i64_param_coerces_to_i64() {
    // 8-byte aggregate → single integer register (i64). Contrast with SysV,
    // where a `{i64,i64}` (16 B) also goes in two regs — Windows would spill
    // that to memory (see the big-struct test below).
    assert_win_x64_param("#[repr(C)]\npub struct P { a: i64 }", "i64", "i64");
}

#[test]
fn win_x64_paired_i32_param_coerces_to_i64() {
    // 8-byte aggregate {i32,i32} → single integer register (i64), like the
    // single-i64 case (raw size, not element structure, drives the coercion).
    assert_win_x64_param("#[repr(C)]\npub struct P { a: i32, b: i32 }", "i32", "i64");
}

#[test]
fn win_x64_16_byte_struct_passed_indirect_ptr() {
    // {i64,i64} = 16 B. On SysV this fits in two regs (raw struct works by
    // luck). On Windows x64 there is no eightbyte splitting: 16 B is > 8,
    // so it goes by reference as a plain `ptr` (no `byval`).
    assert_win_x64_param("#[repr(C)]\npub struct P { a: i64, b: i64 }", "i64", "ptr");
}

#[test]
fn win_x64_big_struct_passed_indirect_ptr() {
    // >16 B aggregate → by reference (plain `ptr`, no `byval`).
    assert_win_x64_param(
        "#[repr(C)]\npub struct P { a: i64, b: i64, c: i64 }",
        "i64",
        "ptr",
    );
}

#[test]
fn win_x64_all_float_struct_treated_as_size_bucket() {
    // Microsoft x64 has NO HFA — a struct of two doubles (16 B) does NOT go
    // in two v-regs like on AArch64; it falls under the > 8-byte rule and
    // is passed by reference. Same treatment as `{i64, i64}` above.
    assert_win_x64_param("#[repr(C)]\npub struct P { a: f64, b: f64 }", "f64", "ptr");
}

fn assert_win_x64_return(struct_def: &str, ctor: &str, expect_ret: &str) {
    if !win_x64_active() {
        return;
    }
    let src = format!("{struct_def}\npub extern \"C\" fn make() -> P {{ {ctor} }}\n");
    let line = define_line(&ir_for(&src), "make");
    let ret = line
        .strip_prefix("define ")
        .and_then(|s| s.split(" @make(").next())
        .unwrap_or("")
        .trim();
    assert_eq!(
        ret, expect_ret,
        "unexpected Windows x64 return type in:\n{line}"
    );
}

#[test]
fn win_x64_single_i64_return_coerces_to_i64() {
    // 8-byte aggregate returns in RAX as `i64`.
    assert_win_x64_return("#[repr(C)]\npub struct P { a: i64 }", "P { a: 42 }", "i64");
}

#[test]
fn win_x64_paired_i32_return_coerces_to_i64() {
    // {i32,i32} = 8 B → `i64` in RAX.
    assert_win_x64_return(
        "#[repr(C)]\npub struct P { a: i32, b: i32 }",
        "P { a: 1, b: 2 }",
        "i64",
    );
}

#[test]
fn win_x64_single_i32_return_coerces_to_i32() {
    // 4-byte aggregate returns in EAX (LLVM `i32`).
    assert_win_x64_return("#[repr(C)]\npub struct P { a: i32 }", "P { a: 7 }", "i32");
}

#[test]
fn win_x64_16_byte_return_uses_sret() {
    // 16 B > 8 → sret (contrast SysV where {i64,i64} returns raw in
    // rax/rdx). Result pointer first, return type void.
    if !win_x64_active() {
        return;
    }
    let src = "#[repr(C)]\npub struct P { a: i64, b: i64 }\n\
               pub extern \"C\" fn make(x: i64) -> P { P { a: x, b: x } }\n";
    let line = define_line(&ir_for(src), "make");
    assert!(
        line.starts_with("define void @make(") && line.contains("sret("),
        "Windows x64 >8B return must be void + `sret(...)`:\n{line}"
    );
}

#[test]
fn win_x64_big_return_uses_sret() {
    // >16 B aggregate return → sret. (Same code path as the 16-B case, but
    // covers the standard "large struct" shape.)
    if !win_x64_active() {
        return;
    }
    let src = "#[repr(C)]\npub struct P { a: i64, b: i64, c: i64 }\n\
               pub extern \"C\" fn make(x: i64) -> P { P { a: x, b: x, c: x } }\n";
    let line = define_line(&ir_for(src), "make");
    assert!(
        line.starts_with("define void @make(") && line.contains("sret("),
        "Windows x64 >8B return must be void + `sret(...)`:\n{line}"
    );
}

#[test]
fn win_x64_big_combined_sret_then_plain_ptr() {
    // Combined sret + indirect param on Windows: the sret pointer is first,
    // the param is a **plain** `ptr` (NOT `ptr byval(...)` like SysV — the
    // Microsoft convention is caller-owned copy passed by address).
    if !win_x64_active() {
        return;
    }
    let src = "#[repr(C)]\npub struct P { a: i64, b: i64, c: i64 }\n\
               pub extern \"C\" fn rt(s: P, d: i64) -> P { P { a: s.a + d, b: s.b, c: s.c } }\n";
    let line = define_line(&ir_for(src), "rt");
    assert!(
        line.starts_with("define void @rt(ptr sret("),
        "sret result pointer must be first:\n{line}"
    );
    assert!(
        !line.contains("byval("),
        "Windows x64 indirect param must NOT be `ptr byval(...)`:\n{line}"
    );
    // The scalar `d: i64` follows as `%2` (0 is sret, 1 is the indirect
    // struct pointer, 2 is the scalar).
    assert!(
        line.contains("i64 %2"),
        "scalar param must follow sret + indirect ptr:\n{line}"
    );
}
