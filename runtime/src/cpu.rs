//! CPU feature detection — the runtime feature-probe behind the `cpu.supports`
//! intrinsic and the `#[multiversion]` dispatch (design.md § Multiversioning >
//! `cpu-baseline` and `#[multiversion]`). Wraps std's *cached*
//! `is_*_feature_detected!` macros, so after the first CPUID (x86) / HWCAP
//! (aarch64) probe every subsequent query is a cheap cached-bool read — which is
//! what makes a per-call multiversion dispatch acceptable without a separate
//! resolve-once cache in codegen.

/// Return 1 iff the running CPU supports the named feature, else 0. `name` is a
/// UTF-8 byte range (`ptr` + `len`, NOT NUL-terminated) — the shape codegen
/// already produces for string literals. An unknown name, a name not meaningful
/// on the running architecture, a null pointer, or invalid UTF-8 all return 0
/// (conservative: a caller must always have a baseline fallback path).
///
/// # Safety
/// `name_ptr` must point to `name_len` readable bytes (or be null). Called only
/// by compiler-emitted code that passes a valid string-literal slice.
#[no_mangle]
pub extern "C" fn karac_cpu_supports(name_ptr: *const u8, name_len: usize) -> i32 {
    if name_ptr.is_null() {
        return 0;
    }
    // Safety: the caller (codegen) passes a valid `{ptr,len}` string literal.
    let bytes = unsafe { std::slice::from_raw_parts(name_ptr, name_len) };
    let Ok(name) = std::str::from_utf8(bytes) else {
        return 0;
    };
    cpu_feature_detected(name) as i32
}

/// Per-architecture feature probe. The recognised names mirror the levels the
/// `cpu-baseline` knob and `#[target_feature]` accept (design.md §
/// Multiversioning table); an unrecognised name is `false` rather than a panic
/// so a stale/typo'd feature degrades to the baseline path.
#[cfg(target_arch = "x86_64")]
fn cpu_feature_detected(name: &str) -> bool {
    match name {
        "sse4.2" => std::is_x86_feature_detected!("sse4.2"),
        "avx" => std::is_x86_feature_detected!("avx"),
        "avx2" => std::is_x86_feature_detected!("avx2"),
        "fma" => std::is_x86_feature_detected!("fma"),
        "bmi1" => std::is_x86_feature_detected!("bmi1"),
        "bmi2" => std::is_x86_feature_detected!("bmi2"),
        "avx512f" => std::is_x86_feature_detected!("avx512f"),
        "avx512bw" => std::is_x86_feature_detected!("avx512bw"),
        "avx512vl" => std::is_x86_feature_detected!("avx512vl"),
        "avx512dq" => std::is_x86_feature_detected!("avx512dq"),
        "avx512cd" => std::is_x86_feature_detected!("avx512cd"),
        _ => false,
    }
}

#[cfg(target_arch = "aarch64")]
fn cpu_feature_detected(name: &str) -> bool {
    match name {
        "neon" => std::arch::is_aarch64_feature_detected!("neon"),
        "dotprod" => std::arch::is_aarch64_feature_detected!("dotprod"),
        "fp16" => std::arch::is_aarch64_feature_detected!("fp16"),
        "sve" => std::arch::is_aarch64_feature_detected!("sve"),
        "sve2" => std::arch::is_aarch64_feature_detected!("sve2"),
        "i8mm" => std::arch::is_aarch64_feature_detected!("i8mm"),
        "bf16" => std::arch::is_aarch64_feature_detected!("bf16"),
        _ => false,
    }
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
fn cpu_feature_detected(_name: &str) -> bool {
    // wasm and other targets have no runtime CPU-feature dispatch surface.
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_and_invalid_return_zero() {
        assert_eq!(karac_cpu_supports(std::ptr::null(), 0), 0);
        // An unknown feature name is always 0.
        let unknown = b"no-such-feature";
        assert_eq!(karac_cpu_supports(unknown.as_ptr(), unknown.len()), 0);
    }

    #[test]
    fn known_feature_matches_std_detection() {
        // On x86-64, `sse4.2` is present on every CPU this test suite runs on
        // (x86-64-v2+); assert the wrapper agrees with the std macro directly.
        #[cfg(target_arch = "x86_64")]
        {
            let name = b"avx2";
            let via_wrapper = karac_cpu_supports(name.as_ptr(), name.len()) != 0;
            assert_eq!(via_wrapper, std::is_x86_feature_detected!("avx2"));
        }
        #[cfg(target_arch = "aarch64")]
        {
            let name = b"neon";
            let via_wrapper = karac_cpu_supports(name.as_ptr(), name.len()) != 0;
            assert_eq!(via_wrapper, std::arch::is_aarch64_feature_detected!("neon"));
        }
    }
}
