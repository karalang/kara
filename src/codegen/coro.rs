//! LLVM coroutine intrinsic emission for the network-async transform (A2).
//!
//! This is the raw-`llvm-sys` half of the A2 transform
//! (`docs/spikes/network-async-coroutine-transform.md`). inkwell 0.9 *panics*
//! on the LLVM `token` type (`LLVMTokenTypeKind => panic!("FIXME: Unsupported
//! type: Token")` in its `types/enums.rs`), and every coro intrinsic is
//! token-typed (`coro.id -> token`, `coro.begin(token, ptr)`, `coro.suspend(
//! token, i1) -> i8`, `coro.free(token, ptr)`, `coro.end(ptr, i1, token)`), so
//! the scaffolding is emitted via `llvm-sys` FFI **interleaved with the
//! inkwell-built function body** — same module, same builder, same blocks.
//!
//! The bridge is bidirectional and needs no memory round-trip:
//!   * inkwell value → llvm-sys: `AsValueRef::as_value_ref()` /
//!     `Module::as_mut_ptr()` / `Builder::as_mut_ptr()` / `Context::raw()`.
//!   * llvm-sys result → inkwell: inkwell 0.9 exposes `pub unsafe fn
//!     new(LLVMValueRef)` on `IntValue` / `PointerValue` / `FunctionValue`, so
//!     the i8 `coro.suspend` result crosses back into an inkwell
//!     `build_switch` (the resume dispatch) directly.
//!
//! Kāra already depends on `llvm-sys` (same 18.1 pin as inkwell, `prefer-
//! dynamic` so there's a single LLVM copy) and uses it directly in
//! `src/codegen/lljit.rs`; this is the second such interop site.

#![cfg(feature = "llvm")]
// Staged infrastructure: every item below is consumed by the slice-2b
// network-boundary transform (the production caller — see
// docs/spikes/network-async-coroutine-transform.md § 6 "The transform").
// In this slice (2a) the surface is exercised end-to-end by the
// `builder_emitted_coroutine_splits` de-risk test but not yet wired into the
// AOT codegen path, so production cfg sees the `pub(super)` API as unused. The
// `allow` is scoped to this one module and is retired when slice 2b calls it.
#![allow(dead_code)]

use std::ffi::CStr;

use inkwell::attributes::AttributeLoc;
use inkwell::basic_block::BasicBlock;
use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::{Linkage, Module};
use inkwell::values::{AsValueRef, FunctionValue, IntValue, PointerValue};
use inkwell::AddressSpace;

use llvm_sys::core::{
    LLVMAddFunction, LLVMBuildCall2, LLVMConstInt, LLVMConstNull, LLVMConstPointerNull,
    LLVMFunctionType, LLVMGetNamedFunction, LLVMInt1TypeInContext, LLVMInt32TypeInContext,
    LLVMInt64TypeInContext, LLVMInt8TypeInContext, LLVMPointerTypeInContext,
    LLVMTokenTypeInContext, LLVMVoidTypeInContext,
};
use llvm_sys::prelude::{LLVMTypeRef, LLVMValueRef};

/// One declared intrinsic / libc function: the callee value plus its function
/// type (LLVM ≥ opaque-pointer requires the function type at every
/// `LLVMBuildCall2`, it can no longer be recovered from the callee pointer).
#[derive(Clone, Copy)]
struct Declared {
    ty: LLVMTypeRef,
    func: LLVMValueRef,
}

/// The coro + libc intrinsics needed by the network-async transform, declared
/// once per module and reused across every coroutine boundary in it.
///
/// All fields are raw `llvm-sys` refs; construction and every `emit_*` call is
/// `unsafe` (they call into the C API), but the refs are valid for the
/// lifetime of the owning `Module` — the same lifetime invariant inkwell
/// itself relies on.
pub(super) struct CoroIntrinsics {
    // Only the types referenced by the `emit_*` leaves are retained as fields;
    // `i8`/`i64`/`void` are needed solely to build the intrinsic signatures at
    // `declare` time and stay local there.
    token_ty: LLVMTypeRef,
    i1_ty: LLVMTypeRef,
    i32_ty: LLVMTypeRef,
    ptr_ty: LLVMTypeRef,

    coro_id: Declared,
    coro_size_i64: Declared,
    coro_begin: Declared,
    coro_suspend: Declared,
    coro_free: Declared,
    coro_end: Declared,
    // Drive intrinsics (used from the resume shim / call-site ramp, not the
    // coroutine body). These take/return `ptr`/`i1` — no token — but are kept
    // here so the whole coro ABI is declared in one place.
    coro_resume: Declared,
    coro_done: Declared,
    coro_destroy: Declared,
    malloc: Declared,
    free: Declared,
}

impl CoroIntrinsics {
    /// Declare (idempotently) the coro intrinsics + `malloc`/`free` in
    /// `module`. Re-declaration is avoided via `LLVMGetNamedFunction`, so this
    /// is safe to call more than once per module.
    ///
    /// # Safety
    /// `context` must be the context `module` belongs to; both must outlive the
    /// returned struct.
    pub(super) unsafe fn declare(context: &Context, module: &Module<'_>) -> Self {
        let ctx = context.raw();
        let m = module.as_mut_ptr();

        let token_ty = LLVMTokenTypeInContext(ctx);
        let i1_ty = LLVMInt1TypeInContext(ctx);
        let i8_ty = LLVMInt8TypeInContext(ctx);
        let i32_ty = LLVMInt32TypeInContext(ctx);
        let i64_ty = LLVMInt64TypeInContext(ctx);
        let void_ty = LLVMVoidTypeInContext(ctx);
        // Opaque pointer (addrspace 0) — LLVM 18 default.
        let ptr_ty = LLVMPointerTypeInContext(ctx, 0);

        // Helper: get-or-declare a function by name with the given type.
        let declare_fn = |name: &CStr, ret: LLVMTypeRef, params: &mut [LLVMTypeRef]| -> Declared {
            let ty = LLVMFunctionType(ret, params.as_mut_ptr(), params.len() as u32, 0);
            let existing = LLVMGetNamedFunction(m, name.as_ptr());
            let func = if existing.is_null() {
                LLVMAddFunction(m, name.as_ptr(), ty)
            } else {
                existing
            };
            Declared { ty, func }
        };

        let coro_id = declare_fn(
            c"llvm.coro.id",
            token_ty,
            &mut [i32_ty, ptr_ty, ptr_ty, ptr_ty],
        );
        let coro_size_i64 = declare_fn(c"llvm.coro.size.i64", i64_ty, &mut []);
        let coro_begin = declare_fn(c"llvm.coro.begin", ptr_ty, &mut [token_ty, ptr_ty]);
        let coro_suspend = declare_fn(c"llvm.coro.suspend", i8_ty, &mut [token_ty, i1_ty]);
        let coro_free = declare_fn(c"llvm.coro.free", ptr_ty, &mut [token_ty, ptr_ty]);
        let coro_end = declare_fn(c"llvm.coro.end", i1_ty, &mut [ptr_ty, i1_ty, token_ty]);
        let coro_resume = declare_fn(c"llvm.coro.resume", void_ty, &mut [ptr_ty]);
        let coro_done = declare_fn(c"llvm.coro.done", i1_ty, &mut [ptr_ty]);
        let coro_destroy = declare_fn(c"llvm.coro.destroy", void_ty, &mut [ptr_ty]);
        let malloc = declare_fn(c"malloc", ptr_ty, &mut [i64_ty]);
        let free = declare_fn(c"free", void_ty, &mut [ptr_ty]);

        Self {
            token_ty,
            i1_ty,
            i32_ty,
            ptr_ty,
            coro_id,
            coro_size_i64,
            coro_begin,
            coro_suspend,
            coro_free,
            coro_end,
            coro_resume,
            coro_done,
            coro_destroy,
            malloc,
            free,
        }
    }

    /// `token none` — the only token constant; `Constant::getNullValue(TokenTy)`
    /// returns `ConstantTokenNone`, which is what `LLVMConstNull` calls.
    unsafe fn token_none(&self) -> LLVMValueRef {
        LLVMConstNull(self.token_ty)
    }

    /// Emit a call to a declared function at the builder's current position.
    unsafe fn call(
        &self,
        builder: &Builder<'_>,
        d: Declared,
        args: &mut [LLVMValueRef],
        name: &CStr,
    ) -> LLVMValueRef {
        LLVMBuildCall2(
            builder.as_mut_ptr(),
            d.ty,
            d.func,
            args.as_mut_ptr(),
            args.len() as u32,
            name.as_ptr(),
        )
    }

    // --- The coro scaffolding leaves, each returning the raw result ref. ---

    /// `%id = call token @llvm.coro.id(i32 0, ptr null, ptr null, ptr null)`.
    /// The three `null` args are the promise / coroaddr / fnaddr — unused for
    /// the switched-resume lowering we target.
    unsafe fn coro_id(&self, builder: &Builder<'_>) -> LLVMValueRef {
        let null = LLVMConstPointerNull(self.ptr_ty);
        let zero = LLVMConstInt(self.i32_ty, 0, 0);
        self.call(builder, self.coro_id, &mut [zero, null, null, null], c"id")
    }

    /// Allocate the coro frame: `%hdl = coro.begin(%id, malloc(coro.size))`.
    /// Returns the coroutine handle (an opaque `ptr`).
    unsafe fn begin(&self, builder: &Builder<'_>, id: LLVMValueRef) -> LLVMValueRef {
        let size = self.call(builder, self.coro_size_i64, &mut [], c"size");
        let alloc = self.call(builder, self.malloc, &mut [size], c"alloc");
        self.call(builder, self.coro_begin, &mut [id, alloc], c"hdl")
    }

    /// `%sp = call i8 @llvm.coro.suspend(token none, i1 final)`. The i8 result
    /// drives the resume switch (0 = resume, 1 = destroy, default = suspend).
    unsafe fn suspend(&self, builder: &Builder<'_>, is_final: bool) -> LLVMValueRef {
        let none = self.token_none();
        let final_flag = LLVMConstInt(self.i1_ty, is_final as u64, 0);
        self.call(builder, self.coro_suspend, &mut [none, final_flag], c"sp")
    }

    /// Free the coro frame on the cleanup edge: `free(coro.free(%id, %hdl))`.
    unsafe fn free_frame(&self, builder: &Builder<'_>, id: LLVMValueRef, hdl: LLVMValueRef) {
        let mem = self.call(builder, self.coro_free, &mut [id, hdl], c"mem");
        self.call(builder, self.free, &mut [mem], c"");
    }

    /// `call i1 @llvm.coro.end(ptr %hdl, i1 false, token none)` — marks the
    /// final suspend / return edge.
    unsafe fn end(&self, builder: &Builder<'_>, hdl: LLVMValueRef) {
        let none = self.token_none();
        let unwind = LLVMConstInt(self.i1_ty, 0, 0);
        self.call(builder, self.coro_end, &mut [hdl, unwind, none], c"");
    }

    // --- Drive leaves: called from the resume shim / call-site ramp on a
    //     live handle, NOT from inside the coroutine body. ---

    /// `call void @llvm.coro.resume(ptr %hdl)` — resume a suspended coroutine
    /// to its next suspend point (or completion). CoroSplit lowers this to a
    /// load of the resume-fn pointer from the frame + an indirect call.
    unsafe fn resume(&self, builder: &Builder<'_>, hdl: LLVMValueRef) {
        self.call(builder, self.coro_resume, &mut [hdl], c"");
    }

    /// `%d = call i1 @llvm.coro.done(ptr %hdl)` — true once the coroutine is
    /// suspended at its final suspend point (ready to destroy).
    unsafe fn done(&self, builder: &Builder<'_>, hdl: LLVMValueRef) -> LLVMValueRef {
        self.call(builder, self.coro_done, &mut [hdl], c"done")
    }

    /// `call void @llvm.coro.destroy(ptr %hdl)` — free a completed coroutine's
    /// frame (runs the cleanup path / drops + frees the malloc'd frame).
    unsafe fn destroy(&self, builder: &Builder<'_>, hdl: LLVMValueRef) {
        self.call(builder, self.coro_destroy, &mut [hdl], c"");
    }
}

/// Mark `func` `presplitcoroutine` so LLVM's CoroSplit pass rewrites it into
/// ramp / resume / destroy clones. Without this attribute the coro intrinsics
/// are left in place and the function is a no-op (the bug-C failure mode).
pub(super) fn mark_presplit_coroutine(context: &Context, func: FunctionValue<'_>) {
    let kind = inkwell::attributes::Attribute::get_named_enum_kind_id("presplitcoroutine");
    debug_assert!(
        kind != 0,
        "presplitcoroutine attribute kind-id must resolve"
    );
    let attr = context.create_enum_attribute(kind, 0);
    func.add_attribute(AttributeLoc::Function, attr);
}

/// The name of the runtime-driven coroutine resume shim (see
/// [`emit_coro_resume_shim`]). Stable so the network transform can reference it
/// when building the parked-task record.
pub(super) const CORO_RESUME_SHIM: &str = "__kara_coro_resume";

/// Emit (idempotently) `i8 @__kara_coro_resume(ptr handle, ptr cancel)` — the
/// bridge that lets the EXISTING runtime dispatcher drive an LLVM coroutine
/// with no runtime changes.
///
/// Its signature is exactly the runtime `KaracParkedTask.poll_fn` ABI
/// (`unsafe extern "C" fn(*mut c_void state, *const AtomicBool cancel) -> u8`,
/// `runtime/src/event_loop.rs`). So the network transform (slice 2b.2+)
/// registers an fd with `parked = { poll_fn: @__kara_coro_resume, state: <coro
/// handle> }`, and the dispatcher loop (`(task.poll_fn)(task.state, &cancel)`,
/// event_loop.rs ~2992) resumes the coroutine on fd-readiness — the same path
/// it already drives state-machine poll-fns through.
///
/// Body: resume the handle; if the coroutine has reached its final suspend
/// (`coro.done`), destroy the frame and report Ready (1); else report Pending
/// (0) and stay parked for the next readiness wakeup. `cancel` is unused at v1
/// (the runtime passes a process-global never-cancelled flag); per-task cancel
/// routing is later work.
///
/// Internal linkage: the runtime calls it only through the function pointer
/// stored in the parked-task record (address-taken → survives DCE), never by
/// cross-module name.
///
/// # Safety
/// `context` must own `module`.
pub(super) unsafe fn emit_coro_resume_shim<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
) -> FunctionValue<'ctx> {
    if let Some(f) = module.get_function(CORO_RESUME_SHIM) {
        return f;
    }
    let intr = CoroIntrinsics::declare(context, module);

    let i8t = context.i8_type();
    let ptrt = context.ptr_type(AddressSpace::default());
    // i8 (ptr handle, ptr cancel)
    let fn_ty = i8t.fn_type(&[ptrt.into(), ptrt.into()], false);
    let func = module.add_function(CORO_RESUME_SHIM, fn_ty, Some(Linkage::Internal));

    let handle = func
        .get_nth_param(0)
        .expect("resume shim handle param")
        .into_pointer_value();

    let builder = context.create_builder();
    let entry = context.append_basic_block(func, "entry");
    let done_bb = context.append_basic_block(func, "done");
    let pending_bb = context.append_basic_block(func, "pending");

    builder.position_at_end(entry);
    intr.resume(&builder, handle.as_value_ref());
    let done_raw = intr.done(&builder, handle.as_value_ref()); // i1
    let done_flag = IntValue::new(done_raw);
    builder
        .build_conditional_branch(done_flag, done_bb, pending_bb)
        .unwrap();

    // done: free the frame, report Ready (1).
    builder.position_at_end(done_bb);
    intr.destroy(&builder, handle.as_value_ref());
    builder
        .build_return(Some(&i8t.const_int(1, false)))
        .unwrap();

    // pending: still suspended — report Pending (0), stay parked.
    builder.position_at_end(pending_bb);
    builder
        .build_return(Some(&i8t.const_int(0, false)))
        .unwrap();

    func
}

/// Build a minimal valid switched-resume coroutine `@demo_coro` via the
/// inkwell-builder + llvm-sys-intrinsic interleave, exercising the exact bridge
/// the real transform uses.
///
/// This is the **builder-path analogue of the IR-text probe** the slice-0
/// de-risk validated: it proves CoroSplit accepts and splits a coroutine we
/// emit through Kāra's real codegen API (not hand-written IR text). Returns the
/// emitted ramp `FunctionValue`. Mirrors the probe IR preserved in
/// `docs/spikes/network-async-coroutine-transform.md` § Appendix.
///
/// # Safety
/// `context`, `module`, and `builder` must share one LLVM context.
pub(super) unsafe fn build_demo_coroutine<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
) -> FunctionValue<'ctx> {
    let intr = CoroIntrinsics::declare(context, module);

    // `define ptr @demo_coro() presplitcoroutine` — built via llvm-sys so we
    // needn't construct an inkwell opaque-pointer FunctionType, then wrapped
    // back into an inkwell FunctionValue for block/attribute work.
    let fn_ty = LLVMFunctionType(intr.ptr_ty, std::ptr::null_mut(), 0, 0);
    let raw_fn = LLVMAddFunction(module.as_mut_ptr(), c"demo_coro".as_ptr(), fn_ty);
    let func = FunctionValue::new(raw_fn).expect("demo_coro function value");
    mark_presplit_coroutine(context, func);

    let entry = context.append_basic_block(func, "entry");
    let resume: BasicBlock = context.append_basic_block(func, "resume");
    let cleanup = context.append_basic_block(func, "cleanup");
    let suspend = context.append_basic_block(func, "suspend");

    // entry: set up the frame, then suspend.
    builder.position_at_end(entry);
    let id = intr.coro_id(builder);
    let hdl = intr.begin(builder, id);
    let sp_raw = intr.suspend(builder, false);
    // Bridge the i8 coro.suspend result back into an inkwell switch.
    let sp = IntValue::new(sp_raw);
    let i8t = context.i8_type();
    builder
        .build_switch(
            sp,
            suspend,
            &[
                (i8t.const_int(0, false), resume),
                (i8t.const_int(1, false), cleanup),
            ],
        )
        .unwrap();

    // resume: (no work in the minimal probe) fall through to cleanup.
    builder.position_at_end(resume);
    builder.build_unconditional_branch(cleanup).unwrap();

    // cleanup: free the frame, then fall through to the final suspend.
    builder.position_at_end(cleanup);
    intr.free_frame(builder, id, hdl);
    builder.build_unconditional_branch(suspend).unwrap();

    // suspend: final coro.end, return the handle.
    builder.position_at_end(suspend);
    intr.end(builder, hdl);
    let hdl_val = PointerValue::new(hdl);
    builder.build_return(Some(&hdl_val)).unwrap();

    func
}

#[cfg(test)]
mod tests {
    use super::*;
    use inkwell::context::Context;
    use inkwell::values::AnyValue;

    /// The builder-path coroutine emission survives `coro-early,coro-split,
    /// coro-cleanup`: CoroSplit produces the `.resume` clone, proving the
    /// llvm-sys ⇄ inkwell interleave emits a coroutine the real (slice-1-wired)
    /// pipeline accepts. This is the slice-2a de-risk gate.
    #[test]
    fn builder_emitted_coroutine_splits() {
        let context = Context::create();
        let module = context.create_module("coro_probe");
        let builder = context.create_builder();

        unsafe {
            build_demo_coroutine(&context, &module, &builder);
        }

        // Pre-split: the module must verify (well-formed coroutine).
        module
            .verify()
            .unwrap_or_else(|e| panic!("pre-split module invalid: {}", e.to_string()));

        // Run the coro pipeline through the same target machine the driver uses.
        let tm = crate::codegen::driver::create_target_machine()
            .expect("create target machine for coro probe");
        let opts = inkwell::passes::PassBuilderOptions::create();
        module
            .run_passes("coro-early,coro-split,coro-cleanup", &tm, opts)
            .unwrap_or_else(|e| panic!("coro pipeline failed: {}", e.to_string()));

        // CoroSplit names the resume clone `<fn>.resume`. Its presence is the
        // concrete proof the state machine was generated.
        assert!(
            module.get_function("demo_coro.resume").is_some(),
            "CoroSplit did not emit demo_coro.resume; module after passes:\n{}",
            module.print_to_string().to_string()
        );

        // Post-split the module must still verify.
        module
            .verify()
            .unwrap_or_else(|e| panic!("post-split module invalid: {}", e.to_string()));
    }

    /// The resume shim (`__kara_coro_resume`) lowers cleanly through the coro
    /// pipeline alongside a real coroutine: `coro.resume`/`coro.done`/
    /// `coro.destroy` are rewritten by CoroCleanup into frame accesses, the
    /// shim survives, and the module re-verifies. This is the slice-2b.1 gate —
    /// the drive bridge that plugs a coroutine into the existing dispatcher.
    #[test]
    fn resume_shim_lowers_alongside_coroutine() {
        let context = Context::create();
        let module = context.create_module("coro_drive");
        let builder = context.create_builder();

        unsafe {
            build_demo_coroutine(&context, &module, &builder);
            emit_coro_resume_shim(&context, &module);
        }

        module
            .verify()
            .unwrap_or_else(|e| panic!("pre-split module invalid: {}", e.to_string()));

        let tm = crate::codegen::driver::create_target_machine()
            .expect("create target machine for coro drive");
        let opts = inkwell::passes::PassBuilderOptions::create();
        module
            .run_passes("coro-early,coro-split,coro-cleanup", &tm, opts)
            .unwrap_or_else(|e| panic!("coro pipeline failed: {}", e.to_string()));

        // The shim survives lowering, and the coroutine still split.
        let shim = module
            .get_function(CORO_RESUME_SHIM)
            .expect("resume shim must survive the coro pipeline");
        assert!(
            module.get_function("demo_coro.resume").is_some(),
            "CoroSplit did not emit demo_coro.resume"
        );
        // The coro.resume/done/destroy intrinsics must be fully lowered away in
        // the shim (CoroCleanup replaces them with frame loads + indirect
        // calls) — no leftover `@llvm.coro.*` calls remain.
        let shim_ir = shim.print_to_string().to_string();
        assert!(
            !shim_ir.contains("@llvm.coro."),
            "resume shim still has un-lowered coro intrinsics:\n{}",
            shim_ir
        );

        module
            .verify()
            .unwrap_or_else(|e| panic!("post-split module invalid: {}", e.to_string()));
    }
}
