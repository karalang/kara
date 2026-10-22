//! The MIR backend and what it needs from LLVM: lowering MIR to LLVM IR
//! (`mir`), building objects and binaries from it (`mir_aot`, `mir_asan`),
//! target machines and the linker (`driver`), the JIT (`lljit`) and finding
//! its runner (`runner`). Nothing here reaches into legacy codegen
//! (`crate::codegen`), so that legacy can be deleted while `build --mir`,
//! `__mir-llvm-run` and `__mir-llvm-asan` keep building (M3 item 5,
//! B-2026-10-22-24). Legacy re-exports what it shares from here.

#[cfg(feature = "llvm")]
pub mod coro_passes;
#[cfg(feature = "llvm")]
pub mod driver;
#[cfg(feature = "llvm")]
pub mod lljit;
#[cfg(feature = "llvm")]
pub mod mir;
#[cfg(feature = "llvm")]
pub mod mir_aot;
#[cfg(feature = "llvm")]
pub mod mir_asan;
pub mod runner;

/// Read the `KARAC_DEBUG_INFO` gate. Unset / `"0"` → off (the default); any
/// other value → on. The `-g` / `--debug-info` CLI flag sets this var for its
/// build.
#[cfg(feature = "llvm")]
pub fn read_debug_info_env() -> bool {
    matches!(std::env::var("KARAC_DEBUG_INFO"), Ok(v) if v != "0")
}
