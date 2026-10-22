//! The coroutine-lowering pass string for a module, which the JIT
//! (`lljit.rs`) and legacy codegen's optimization pipeline both run
//! (moved out of `codegen/coro.rs` for B-2026-10-22-24).

/// The coroutine-lowering pipeline, run at EVERY opt level by both the AOT
/// driver (`driver::apply_optimization_passes`) and the JIT
/// (`lljit::run_coro_passes`) — see [`coro_lowering_pipeline`].
const CORO_LOWERING_PASSES: &str = "coro-early,coro-split,coro-cleanup";

/// [`CORO_LOWERING_PASSES`] preceded by `globaldce`, for a module that holds a
/// `presplitcoroutine` function.
///
/// B-2026-09-26-44 — CoroSplit is a CGSCC pass, and the lazy call graph it
/// walks never visits an INTERNAL function nothing reaches. So a
/// network-boundary fn that no code calls stayed unsplit, its `llvm.coro.*`
/// intrinsics reached instruction selection, and the build aborted (`Cannot
/// select: intrinsic %llvm.coro.size`, or `Do not know how to promote this
/// operator's operand!`). `-O1`/`-O2` never saw it only because their pipeline
/// deletes the dead function after the split; `-O0` and the JIT run no such
/// pipeline. Deleting dead internal functions FIRST leaves CoroSplit only
/// coroutines it will visit. Gated on a coroutine being present so a module
/// without one keeps its `-O0` shape exactly.
const CORO_LOWERING_PASSES_WITH_DCE: &str = "globaldce,coro-early,coro-split,coro-cleanup";

/// The pass string to lower coroutines in `module` — see
/// [`CORO_LOWERING_PASSES_WITH_DCE`] for why it depends on the module.
///
/// # Safety
///
/// `module` must be a valid, live `LLVMModuleRef`.
pub unsafe fn coro_lowering_pipeline(module: llvm_sys::prelude::LLVMModuleRef) -> &'static str {
    use llvm_sys::core::{
        LLVMGetEnumAttributeAtIndex, LLVMGetEnumAttributeKindForName, LLVMGetFirstFunction,
        LLVMGetNextFunction,
    };
    use llvm_sys::LLVMAttributeFunctionIndex;
    let name = "presplitcoroutine";
    let kind = unsafe { LLVMGetEnumAttributeKindForName(name.as_ptr().cast(), name.len()) };
    let mut f = unsafe { LLVMGetFirstFunction(module) };
    while !f.is_null() {
        if !unsafe { LLVMGetEnumAttributeAtIndex(f, LLVMAttributeFunctionIndex, kind) }.is_null() {
            return CORO_LOWERING_PASSES_WITH_DCE;
        }
        f = unsafe { LLVMGetNextFunction(f) };
    }
    CORO_LOWERING_PASSES
}
