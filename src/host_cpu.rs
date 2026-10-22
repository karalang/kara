//! The host CPU-feature probe behind `cpu.supports(name)`, shared by the
//! legacy interpreter and the MIR interpreter: the tree-walk twin of the
//! runtime's `karac_cpu_supports` (`runtime/src/cpu.rs`).

/// Whether the host CPU has the feature `name`.
/// Deliberately mirrors `karac_cpu_supports`'s recognised-name set per architecture,
/// so `karac run --interp` and `karac build`/JIT report the same features on the
/// machine they run on. An unknown name is `false`.
#[cfg(target_arch = "x86_64")]
pub fn host_cpu_supports(name: &str) -> bool {
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
pub fn host_cpu_supports(name: &str) -> bool {
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
pub fn host_cpu_supports(_name: &str) -> bool {
    false
}
