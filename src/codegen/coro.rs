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
// As of slice 2b.3 the production coroutine API (`CoroIntrinsics`,
// `CoroContext`, `emit_coro_ramp`/`emit_coro_park_suspend`/`emit_coro_finish`,
// the resume shim) is wired into the AOT codegen path (functions.rs / tcp.rs /
// call_dispatch.rs). What remains `dead_code` in *production* cfg is the
// slice-2a/2b.2a de-risk scaffolding — `build_demo_coroutine` /
// `build_demo_park_coroutine` and a couple of `CoroIntrinsics` drive leaves —
// which are exercised only by this module's `#[cfg(test)]` unit tests (they
// prove CoroSplit survival in isolation). Those are kept as living regression
// fixtures rather than deleted, so the module-scoped `allow` stays.
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
#[derive(Clone, Copy)]
pub(crate) struct CoroIntrinsics {
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

/// Per-coroutine-function emission context (A2 slice 2b.3). Built by
/// [`Codegen::emit_coro_ramp`] at the top of a coroutine-compiled
/// network-boundary function, stashed in `Codegen.coro_ctx` for the duration of
/// that function's body emission, consulted by
/// [`Codegen::emit_coro_park_suspend`] (the leaf-park coro branch) and the
/// body-return routing, and drained by [`Codegen::emit_coro_finish`] after the
/// body. Carries the live coroutine handle + the shared exit blocks so every
/// park in the body wires its suspend switch to one cleanup / suspend-return /
/// completion target.
///
/// All fields are `Copy` (raw refs + inkwell handle types), so the leaf can copy
/// the whole context out of `self.coro_ctx` and emit through it without holding
/// a borrow on `self`.
#[derive(Clone, Copy)]
pub(crate) struct CoroContext<'ctx> {
    /// The coroutine handle `%hdl` (`coro.begin` result) — goes into each
    /// park's `KaracParkedTask.state` field and drives every `coro.resume`.
    pub hdl: PointerValue<'ctx>,
    /// The `coro.id` token (raw — inkwell can't hold a token), needed by
    /// `coro.free` on the destroy edge.
    pub id: LLVMValueRef,
    /// The per-module coro/libc intrinsic table (declared once, reused).
    pub intr: CoroIntrinsics,
    /// `@__kara_coro_resume` — the parked-task `poll_fn` the dispatcher drives.
    pub shim: FunctionValue<'ctx>,
    /// The frame-resident parked-task record type `{ ptr poll_fn, ptr state,
    /// i64 token }` (first two words are the runtime `KaracParkedTask` ABI;
    /// the third holds the registration token for the resume-edge deregister).
    pub parked_ty: inkwell::types::StructType<'ctx>,
    /// The caller-provided completion slot (the hidden trailing `ptr` param —
    /// see `declare_function`). The caller `park_slot_new`s it, passes it in,
    /// `park_slot_wait`s on it, then frees it; the body `park_slot_signal`s it
    /// just before the final suspend. Spilled into the coro frame by CoroSplit
    /// (live across suspends), but the caller owns the underlying object — the
    /// frame holds only a copy of the pointer, so destroying the frame never
    /// frees the slot.
    pub slot: PointerValue<'ctx>,
    /// Single completion target every body-return routes to: `park_slot_signal(
    /// slot)` + final `coro.suspend(true)` (filled by `emit_coro_finish`).
    pub coro_return_bb: BasicBlock<'ctx>,
    /// Shared destroy edge (`coro.destroy` lands here): `coro.free` then a branch
    /// into `suspend_ret_bb` (the canonical single-`coro.end` shape). The ONLY
    /// place the frame is freed — never on the normal completion path (that
    /// would UAF the dispatcher's post-completion `coro.done`/`coro.destroy`).
    pub cleanup_bb: BasicBlock<'ctx>,
    /// The single shared `coro.end` + `ret hdl`. Reached by every suspend's
    /// `default` (still-suspended) edge AND by `cleanup_bb` (after the frame is
    /// freed). Returns `hdl` — a value, never dereferenced by the caller — so it
    /// is UAF-safe even on the destroy clone where the frame is already gone;
    /// the caller ignores the ramp's return and waits on the slot it passed in.
    pub suspend_ret_bb: BasicBlock<'ctx>,
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
    let cancel = func
        .get_nth_param(1)
        .expect("resume shim cancel param")
        .into_pointer_value();

    let builder = context.create_builder();
    let entry = context.append_basic_block(func, "entry");
    let cancel_check_bb = context.append_basic_block(func, "cancel.check");
    let cancel_bb = context.append_basic_block(func, "cancel.teardown");
    let resume_bb = context.append_basic_block(func, "resume");
    let done_bb = context.append_basic_block(func, "done");
    let pending_bb = context.append_basic_block(func, "pending");

    // A2 cooperative cancellation: before resuming, check the cancel flag the
    // dispatcher passes. The runtime always passes a valid pointer today (a
    // process-global never-cancelled flag); per-task routing (so `TaskGroup.
    // cancel()` targets specific coroutines) is the follow-on. A null pointer
    // is still tolerated (treated as not-cancelled) so the shim is robust to
    // any future caller that passes null.
    builder.position_at_end(entry);
    let cancel_is_null = builder
        .build_int_compare(
            inkwell::IntPredicate::EQ,
            cancel,
            ptrt.const_null(),
            "cancel.is_null",
        )
        .unwrap();
    builder
        .build_conditional_branch(cancel_is_null, resume_bb, cancel_check_bb)
        .unwrap();

    // cancel.check: load the flag (a `*const AtomicBool`, i8-wide); branch to
    // teardown when set.
    builder.position_at_end(cancel_check_bb);
    let cancel_val = builder
        .build_load(i8t, cancel, "cancel.flag")
        .unwrap()
        .into_int_value();
    let cancelled = builder
        .build_int_compare(
            inkwell::IntPredicate::NE,
            cancel_val,
            i8t.const_int(0, false),
            "cancel.set",
        )
        .unwrap();
    builder
        .build_conditional_branch(cancelled, cancel_bb, resume_bb)
        .unwrap();

    // cancel.teardown: do NOT resume — destroy the frame. `coro.destroy` runs
    // the destroy clone, which (for a coroutine suspended at a park) executes
    // that park's `kara.coro.destroy.N` edge: deregister the fd, drop the heap
    // locals live across the park (slice 4), `park_slot_signal` the completion
    // slot so the waiter wakes (slice 5c), then `coro.free`. Report Ready (1)
    // so the dispatcher stops driving this task.
    builder.position_at_end(cancel_bb);
    intr.destroy(&builder, handle.as_value_ref());
    builder
        .build_return(Some(&i8t.const_int(1, false)))
        .unwrap();

    // resume: drive the coroutine to its next suspend (or completion).
    builder.position_at_end(resume_bb);
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

/// Build a **park-shaped** switched-resume coroutine `@demo_park_coro(i32 fd)`
/// that mirrors the production network-leaf lowering shape (slice 2b.2), so the
/// load-bearing question can be answered before wiring into the real compile
/// path: **does the frame-resident parked-task slot survive CoroSplit with a
/// stable address, and does the post-park syscall land on the resume edge (not
/// get dropped — the bug-C failure mode)?**
///
/// The emitted shape matches `emit_state_machine_invocation_for_park_on_fd`
/// (`tcp.rs`) but with the thread-block swapped for a real suspend:
///
/// ```text
/// entry:   id/begin; alloca parked{poll_fn,state,token};
///          parked = {@__kara_coro_resume, hdl};
///          token = register_fd(fd, dir, &parked);   // dispatcher reads &parked
///          token -> parked.token;                   // (while suspended)
///          suspend; switch [0->resume, 1->cleanup] default suspend
/// resume:  deregister(fd, load parked.token);       // reload FORCES the slot
///          n = syscall(fd);                          // into the coro frame —
///          br cleanup                                // the post-park work
/// cleanup: free_frame; br suspend
/// suspend: coro.end; ret hdl
/// ```
///
/// The `parked.token` reload on the resume edge is what makes the slot live
/// across the suspend, so CoroSplit promotes it into the frame and the address
/// register_fd captured stays valid for the dispatcher to dereference during
/// suspension — exactly the lifetime contract the production leaf needs. The
/// extern `__demo_register_fd`/`_deregister_fd`/`_syscall` stand in for
/// `karac_runtime_event_loop_register_fd` / `_deregister_fd` /
/// `karac_runtime_tcp_read`; this validates the emission *shape*, not the FFI.
///
/// # Safety
/// `context`, `module`, and `builder` must share one LLVM context.
pub(super) unsafe fn build_demo_park_coroutine<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
) -> FunctionValue<'ctx> {
    let intr = CoroIntrinsics::declare(context, module);
    let shim = emit_coro_resume_shim(context, module);

    let i8t = context.i8_type();
    let i32t = context.i32_type();
    let i64t = context.i64_type();
    let ptrt = context.ptr_type(AddressSpace::default());

    // Extern stubs standing in for the runtime FFI the production leaf calls.
    let register_fd = module.add_function(
        "__demo_register_fd",
        i64t.fn_type(&[i32t.into(), i8t.into(), ptrt.into()], false),
        None,
    );
    let deregister_fd = module.add_function(
        "__demo_deregister_fd",
        context
            .void_type()
            .fn_type(&[i32t.into(), i64t.into()], false),
        None,
    );
    let syscall = module.add_function("__demo_syscall", i64t.fn_type(&[i32t.into()], false), None);

    // `define ptr @demo_park_coro(i32 %fd) presplitcoroutine`.
    let func = module.add_function("demo_park_coro", ptrt.fn_type(&[i32t.into()], false), None);
    mark_presplit_coroutine(context, func);
    let fd = func
        .get_nth_param(0)
        .expect("demo_park_coro fd param")
        .into_int_value();

    // Demo parked-task slot: `{ptr poll_fn, ptr state, i64 token}`. The first
    // two words are the runtime `KaracParkedTask` ABI; the third holds the
    // registration token (production keeps it in a state-struct field).
    let parked_ty = context.struct_type(&[ptrt.into(), ptrt.into(), i64t.into()], false);

    let entry = context.append_basic_block(func, "entry");
    let resume = context.append_basic_block(func, "resume");
    let cleanup = context.append_basic_block(func, "cleanup");
    let suspend = context.append_basic_block(func, "suspend");

    // entry: set up the frame, build the parked record, register, suspend.
    builder.position_at_end(entry);
    let id = intr.coro_id(builder);
    let hdl = intr.begin(builder, id);
    let hdl_pv = PointerValue::new(hdl);

    let slot = builder.build_alloca(parked_ty, "parked").unwrap();
    let poll_fn_field = builder
        .build_struct_gep(parked_ty, slot, 0, "parked.poll_fn")
        .unwrap();
    builder
        .build_store(poll_fn_field, shim.as_global_value().as_pointer_value())
        .unwrap();
    let state_field = builder
        .build_struct_gep(parked_ty, slot, 1, "parked.state")
        .unwrap();
    builder.build_store(state_field, hdl_pv).unwrap();

    let dir = i8t.const_int(0, false);
    let token = builder
        .build_call(register_fd, &[fd.into(), dir.into(), slot.into()], "token")
        .unwrap()
        .try_as_basic_value()
        .unwrap_basic()
        .into_int_value();
    let token_field = builder
        .build_struct_gep(parked_ty, slot, 2, "parked.token")
        .unwrap();
    builder.build_store(token_field, token).unwrap();

    let sp = IntValue::new(intr.suspend(builder, false));
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

    // resume: reload the token (this cross-suspend use forces the slot into the
    // frame), deregister, run the post-park syscall, then clean up.
    builder.position_at_end(resume);
    let token_field2 = builder
        .build_struct_gep(parked_ty, slot, 2, "parked.token.reload")
        .unwrap();
    let token2 = builder
        .build_load(i64t, token_field2, "token.val")
        .unwrap()
        .into_int_value();
    builder
        .build_call(deregister_fd, &[fd.into(), token2.into()], "")
        .unwrap();
    builder.build_call(syscall, &[fd.into()], "n").unwrap();
    builder.build_unconditional_branch(cleanup).unwrap();

    // cleanup: free the frame, then fall through to the final suspend.
    builder.position_at_end(cleanup);
    intr.free_frame(builder, id, hdl);
    builder.build_unconditional_branch(suspend).unwrap();

    // suspend: final coro.end, return the handle.
    builder.position_at_end(suspend);
    intr.end(builder, hdl);
    builder.build_return(Some(&hdl_pv)).unwrap();

    func
}

// ── Production coroutine emission (A2 slice 2b.3) ─────────────────────────────
//
// Three Codegen methods turn a network-boundary function into a switched-resume
// coroutine driven by the existing dispatcher (see
// docs/spikes/network-async-coroutine-transform.md § 6¾ "Drive-model
// correction"):
//   * `emit_coro_ramp`    — entry prologue: coro.id/begin + completion slot +
//                           the shared exit blocks; returns the CoroContext.
//   * `emit_coro_park_suspend` — one park → suspend (called from the tcp.rs
//                           leaf when self.coro_ctx is Some).
//   * `emit_coro_finish`  — fills the shared exit blocks after the body.
//
// The frame-lifetime topology is the load-bearing, correctness-sensitive part:
// the frame is freed ONLY on the destroy edge (`cleanup_bb`, reached via
// `coro.destroy`), never on the normal-completion path — a coroutine that
// self-frees on completion UAFs the dispatcher's post-completion
// `coro.done`/`coro.destroy`. Normal completion routes through a *final*
// `coro.suspend(true)` and leaves the frame alive for the shim to destroy.
impl<'ctx> super::Codegen<'ctx> {
    /// Emit the coroutine ramp prologue at the current builder position — the
    /// function's `entry` block, before param allocas. Declares the coro
    /// intrinsics + resume shim (both idempotent per module), marks `fn_val`
    /// `presplitcoroutine`, emits `coro.id`/`coro.begin`, allocates the caller's
    /// completion slot, and appends the three shared exit blocks (filled later
    /// by [`Self::emit_coro_finish`]). Stores the populated context in
    /// `self.coro_ctx` and resets the per-function park counter; also returns it.
    pub(super) fn emit_coro_ramp(
        &mut self,
        fn_val: FunctionValue<'ctx>,
        slot: PointerValue<'ctx>,
    ) -> CoroContext<'ctx> {
        let intr = unsafe { CoroIntrinsics::declare(self.context, &self.module) };
        let shim = unsafe { emit_coro_resume_shim(self.context, &self.module) };
        mark_presplit_coroutine(self.context, fn_val);

        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_ty = self.context.i64_type();
        let parked_ty = self
            .context
            .struct_type(&[ptr_ty.into(), ptr_ty.into(), i64_ty.into()], false);

        // coro.id / coro.begin at the top of entry — the frame allocation.
        // `slot` is the caller-provided completion slot (the hidden trailing
        // `ptr` param — see `declare_function`): the body signals it at
        // completion and the caller `park_slot_wait`s on it. Passing it in
        // (rather than the ramp `park_slot_new`-ing + returning it) keeps the
        // single canonical `coro.end` UAF-safe — the ramp returns `hdl` (a value,
        // safe to return even on the freed-frame destroy edge), never a
        // frame-resident value.
        let id = unsafe { intr.coro_id(&self.builder) };
        let hdl = unsafe { intr.begin(&self.builder, id) };
        let hdl_pv = unsafe { PointerValue::new(hdl) };

        // Shared exit blocks — appended now (so parks can target them), filled
        // by emit_coro_finish after the body.
        let coro_return_bb = self.context.append_basic_block(fn_val, "kara.coro.return");
        let cleanup_bb = self.context.append_basic_block(fn_val, "kara.coro.cleanup");
        let suspend_ret_bb = self
            .context
            .append_basic_block(fn_val, "kara.coro.suspend_ret");

        let ctx = CoroContext {
            hdl: hdl_pv,
            id,
            intr,
            shim,
            parked_ty,
            slot,
            coro_return_bb,
            cleanup_bb,
            suspend_ret_bb,
        };
        self.coro_ctx = Some(ctx);
        self.coro_park_counter = 0;
        ctx
    }

    /// Emit one network park as a coroutine suspend at the current builder
    /// position. Called from `tcp.rs`'s
    /// `emit_state_machine_invocation_for_park_on_fd` when `self.coro_ctx` is
    /// `Some`. Builds a frame-resident parked record `{ @__kara_coro_resume,
    /// hdl, token }`, ensures the dispatcher is running, registers the fd,
    /// `coro.suspend`s, and wires the suspend switch to the shared exit blocks
    /// plus a fresh per-park resume block. On return the builder is positioned
    /// at that resume block, *after* the deregister — so the caller's post-park
    /// syscall (`karac_runtime_tcp_read`/`accept`/`write`) lands on the resume
    /// edge verbatim (the bug-C fix).
    pub(super) fn emit_coro_park_suspend(
        &mut self,
        fd: IntValue<'ctx>,
        direction: IntValue<'ctx>,
        ctx: &CoroContext<'ctx>,
    ) {
        let i8_ty = self.context.i8_type();
        let i64_ty = self.context.i64_type();

        let n = self.coro_park_counter;
        self.coro_park_counter += 1;
        let fn_val = self
            .builder
            .get_insert_block()
            .and_then(|b| b.get_parent())
            .expect("emit_coro_park_suspend inside a function context");

        // Frame-resident parked record: { poll_fn = @__kara_coro_resume,
        // state = hdl, token }. CoroSplit lifts it into the coro frame because
        // of the cross-suspend token reload on the resume edge below.
        let parked = self
            .builder
            .build_alloca(ctx.parked_ty, &format!("kara.coro.parked.{n}"))
            .unwrap();
        let poll_fn_field = self
            .builder
            .build_struct_gep(ctx.parked_ty, parked, 0, "kara.coro.parked.poll_fn")
            .unwrap();
        self.builder
            .build_store(poll_fn_field, ctx.shim.as_global_value().as_pointer_value())
            .unwrap();
        let state_field = self
            .builder
            .build_struct_gep(ctx.parked_ty, parked, 1, "kara.coro.parked.state")
            .unwrap();
        self.builder.build_store(state_field, ctx.hdl).unwrap();

        // Ensure the dispatcher is up before we register (idempotent bootstrap;
        // the degenerate poll-fn path does the same in its state_0).
        let start_disp = self
            .module
            .get_function("karac_runtime_scheduler_start_dispatcher")
            .expect("karac_runtime_scheduler_start_dispatcher declared in Codegen::new");
        self.builder.build_call(start_disp, &[], "").unwrap();

        let register_fd = self
            .module
            .get_function("karac_runtime_event_loop_register_fd")
            .expect("karac_runtime_event_loop_register_fd declared in Codegen::new");
        let token = self
            .builder
            .build_call(
                register_fd,
                &[fd.into(), direction.into(), parked.into()],
                "kara.coro.token",
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();
        let token_field = self
            .builder
            .build_struct_gep(ctx.parked_ty, parked, 2, "kara.coro.parked.token")
            .unwrap();
        self.builder.build_store(token_field, token).unwrap();

        // Active-span preservation across the suspend (phase-8 line 153 Phase 2).
        // The ambient active span is a per-*thread* TLS register; a coroutine can
        // resume on a different dispatcher worker than the one it parked on, so
        // the resuming thread's register reflects whatever it last ran, not this
        // coroutine's pre-suspend span. Snapshot it into a frame-resident slot
        // here and restore it on every post-suspend edge (resume + destroy). The
        // cross-suspend load below forces CoroSplit to spill this alloca into the
        // coro frame — the same residency mechanism the token relies on. The two
        // `karac_tracing_*` accessors are unconditional runtime externs (declared
        // in `Codegen::new`, always present in the archive), so this is safe even
        // for a program that never touches `std.tracing`.
        let active_span_slot = self
            .builder
            .build_alloca(i64_ty, &format!("kara.coro.active_span.{n}"))
            .unwrap();
        let active_span_snap = self
            .builder
            .build_call(
                self.karac_tracing_get_active_span_fn,
                &[],
                "kara.coro.active_span.snap",
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();
        self.builder
            .build_store(active_span_slot, active_span_snap)
            .unwrap();

        // Suspend; switch to the shared suspend-return (default), a fresh resume
        // block (case 0), and a fresh per-park DESTROY block (case 1). The
        // destroy edge is the cancel/teardown path for a coroutine suspended
        // *at this park*: case 1 cannot share the final-suspend's free-only
        // `ctx.cleanup_bb` directly, because here the fd is still registered and
        // the heap locals live across this park are still owned by the frame —
        // both must be torn down before the frame is freed (A2 slice 4).
        let sp = unsafe { IntValue::new(ctx.intr.suspend(&self.builder, false)) };
        let resume_bb = self
            .context
            .append_basic_block(fn_val, &format!("kara.coro.resume.{n}"));
        let destroy_bb = self
            .context
            .append_basic_block(fn_val, &format!("kara.coro.destroy.{n}"));
        self.builder
            .build_switch(
                sp,
                ctx.suspend_ret_bb,
                &[
                    (i8_ty.const_int(0, false), resume_bb),
                    (i8_ty.const_int(1, false), destroy_bb),
                ],
            )
            .unwrap();

        let deregister_fd = self
            .module
            .get_function("karac_runtime_event_loop_deregister_fd")
            .expect("karac_runtime_event_loop_deregister_fd declared in Codegen::new");

        // Destroy edge: the coroutine is being torn down while parked here
        // (cooperative cancellation — the resume shim sees the cancel flag set
        // and `coro.destroy`s instead of resuming, landing on THIS edge). Order:
        //   1. deregister the fd — the event loop still holds a pointer to the
        //      frame-resident parked record; freeing the frame without this
        //      dangles it (the dispatcher would deref freed memory on the next
        //      readiness poll). MUST precede the frame free.
        //   2. drop the heap locals live across this park — the set a cancel
        //      here would otherwise leak. CoroSplit spills each to the frame
        //      because they are used on this (post-suspend) edge.
        //   3. `park_slot_signal` the completion slot (slice 5c) — a cancelled
        //      coroutine never reaches its normal `coro_return` signal, so
        //      without this the waiter (`park_slot_wait` inline, or the
        //      spawn-coro join) HANGS forever. The completion and cancel paths
        //      are mutually exclusive (the coroutine either runs to completion
        //      and signals from `coro_return`, or is destroyed at a park and
        //      signals here), and `park_slot_signal` is idempotent, so there is
        //      no double-signal. `slot` is the last use of `ctx.slot` here — the
        //      woken waiter may free the slot immediately after, and the rest of
        //      this edge (deregister/drops/free) never touches it again.
        //   4. branch to the shared `cleanup_bb`, which `coro.free`s the frame.
        // Deregister-vs-drops-vs-signal order is immaterial (independent); the
        // frame free strictly follows all three.
        self.builder.position_at_end(destroy_bb);
        // Restore the pre-suspend active span first, so any user-`Drop` run by
        // `emit_coro_destroy_edge_drops` below logs under the coroutine's span
        // rather than the resuming worker's.
        self.emit_coro_restore_active_span(active_span_slot, i64_ty);
        let token_field_d = self
            .builder
            .build_struct_gep(ctx.parked_ty, parked, 2, "kara.coro.parked.token.destroy")
            .unwrap();
        let token_d = self
            .builder
            .build_load(i64_ty, token_field_d, "kara.coro.token.destroy.val")
            .unwrap()
            .into_int_value();
        self.builder
            .build_call(deregister_fd, &[fd.into(), token_d.into()], "")
            .unwrap();
        self.emit_coro_destroy_edge_drops();
        let signal_fn = self
            .module
            .get_function("karac_runtime_park_slot_signal")
            .expect("karac_runtime_park_slot_signal declared in Codegen::new");
        self.builder
            .build_call(signal_fn, &[ctx.slot.into()], "")
            .unwrap();
        self.builder
            .build_unconditional_branch(ctx.cleanup_bb)
            .unwrap();

        // Resume edge: reload the token (forces frame residency), deregister,
        // then leave the builder here so the post-park syscall lands on it.
        self.builder.position_at_end(resume_bb);
        let token_field2 = self
            .builder
            .build_struct_gep(ctx.parked_ty, parked, 2, "kara.coro.parked.token.reload")
            .unwrap();
        let token2 = self
            .builder
            .build_load(i64_ty, token_field2, "kara.coro.token.val")
            .unwrap()
            .into_int_value();
        self.builder
            .build_call(deregister_fd, &[fd.into(), token2.into()], "")
            .unwrap();
        // Restore the pre-suspend active span on the resume edge. The builder
        // stays here afterward, so the caller's post-park syscall still lands on
        // this block verbatim — the restore is just prepended to it.
        self.emit_coro_restore_active_span(active_span_slot, i64_ty);
    }

    /// Reload the frame-spilled pre-suspend active span from `slot` and reinstall
    /// it into the per-thread TLS register via `karac_tracing_set_active_span`.
    /// Shared by both post-suspend edges of [`Self::emit_coro_park_suspend`].
    fn emit_coro_restore_active_span(
        &self,
        slot: PointerValue<'ctx>,
        i64_ty: inkwell::types::IntType<'ctx>,
    ) {
        let saved = self
            .builder
            .build_load(i64_ty, slot, "kara.coro.active_span.restore")
            .unwrap()
            .into_int_value();
        self.builder
            .build_call(self.karac_tracing_set_active_span_fn, &[saved.into()], "")
            .unwrap();
    }

    /// Fill the three shared exit blocks after the function body is emitted
    /// (the body's returns have all branched to `ctx.coro_return_bb`).
    ///
    /// Topology (see §6¾ for the lifetime rationale):
    ///   * `coro_return`: `park_slot_signal(slot)` then the **final**
    ///     `coro.suspend(true)`. The dispatcher's last resume sees `coro.done`
    ///     and `coro.destroy`s (→ `cleanup`); `default` (still suspended) →
    ///     `suspend_ret`; the post-final resume edge is unreachable.
    ///   * `cleanup` (destroy edge): `coro.free` + `coro.end` + `ret null`. The
    ///     ONLY free site, and it must NOT `ret slot` — the frame (and the
    ///     frame-spilled `slot`) is gone by the return.
    ///   * `suspend_ret` (suspend-return edge, frame alive): `coro.end` +
    ///     `ret slot`. In the ramp this returns `slot` to the original caller;
    ///     in resume clones it returns the frame-spilled `slot` (dispatcher
    ///     ignores it).
    pub(super) fn emit_coro_finish(&mut self, ctx: &CoroContext<'ctx>) {
        let i8_ty = self.context.i8_type();
        let fn_val = ctx
            .coro_return_bb
            .get_parent()
            .expect("coro_return_bb has a parent function");

        // coro_return: signal completion, then the final suspend.
        self.builder.position_at_end(ctx.coro_return_bb);
        let signal = self
            .module
            .get_function("karac_runtime_park_slot_signal")
            .expect("karac_runtime_park_slot_signal declared in Codegen::new");
        self.builder
            .build_call(signal, &[ctx.slot.into()], "")
            .unwrap();
        let spf = unsafe { IntValue::new(ctx.intr.suspend(&self.builder, true)) };
        let after_final = self
            .context
            .append_basic_block(fn_val, "kara.coro.after_final");
        self.builder
            .build_switch(
                spf,
                ctx.suspend_ret_bb,
                &[
                    (i8_ty.const_int(0, false), after_final),
                    (i8_ty.const_int(1, false), ctx.cleanup_bb),
                ],
            )
            .unwrap();
        // A final suspend is never resumed (case 0) — only destroyed.
        self.builder.position_at_end(after_final);
        self.builder.build_unreachable().unwrap();

        // cleanup (destroy edge): free the frame — the ONLY free site — then
        // fall through to the single shared `coro.end` in suspend_ret (the
        // canonical shape: exactly one fallthrough `coro.end`). `coro.end` after
        // `coro.free` is a no-op marker on the being-destroyed frame.
        self.builder.position_at_end(ctx.cleanup_bb);
        unsafe {
            ctx.intr
                .free_frame(&self.builder, ctx.id, ctx.hdl.as_value_ref());
        }
        self.builder
            .build_unconditional_branch(ctx.suspend_ret_bb)
            .unwrap();

        // suspend_ret: the single `coro.end`, then `ret hdl`. Returning `hdl`
        // (the coro handle — a value, never dereferenced by the caller) is
        // UAF-safe on every clone, including the destroy clone where the frame
        // is already freed; the caller ignores the ramp's return and instead
        // waits on the completion slot it passed in. (Earlier `ret slot` was a
        // UAF: `slot` is frame-resident, loaded after `coro.free` on the destroy
        // edge.)
        self.builder.position_at_end(ctx.suspend_ret_bb);
        unsafe {
            ctx.intr.end(&self.builder, ctx.hdl.as_value_ref());
        }
        self.builder.build_return(Some(&ctx.hdl)).unwrap();
    }
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

    /// The **park-shaped** coroutine (the production leaf shape, slice 2b.2)
    /// splits correctly: the frame-resident parked-task slot survives with a
    /// stable address (no `alloca` left in the ramp — CoroSplit lifted it into
    /// the frame), the registration stays in the ramp, and the post-park
    /// syscall lands in the `.resume` clone rather than being dropped (the
    /// bug-C failure mode). This is the slice-2b.2 emission de-risk.
    #[test]
    fn park_shaped_coroutine_splits_with_frame_resident_slot() {
        let context = Context::create();
        let module = context.create_module("coro_park");
        let builder = context.create_builder();

        unsafe {
            build_demo_park_coroutine(&context, &module, &builder);
        }

        module
            .verify()
            .unwrap_or_else(|e| panic!("pre-split module invalid: {}", e.to_string()));

        let tm = crate::codegen::driver::create_target_machine()
            .expect("create target machine for coro park");
        let opts = inkwell::passes::PassBuilderOptions::create();
        module
            .run_passes("coro-early,coro-split,coro-cleanup", &tm, opts)
            .unwrap_or_else(|e| panic!("coro pipeline failed: {}", e.to_string()));

        // The state machine was generated.
        let ramp = module
            .get_function("demo_park_coro")
            .expect("ramp survives split");
        let resume_clone = module
            .get_function("demo_park_coro.resume")
            .expect("CoroSplit must emit demo_park_coro.resume");

        let ramp_ir = ramp.print_to_string().to_string();
        let resume_ir = resume_clone.print_to_string().to_string();

        // (1) The park registration stays in the ramp — the dispatcher gets a
        //     pointer into the (now frame-resident) parked slot.
        assert!(
            ramp_ir.contains("@__demo_register_fd"),
            "register_fd must stay in the ramp; ramp IR:\n{}",
            ramp_ir
        );
        // (2) Frame residency — the load-bearing invariant. The pointer
        //     register_fd captures (and the dispatcher dereferences while the
        //     coroutine is suspended) must be a GEP into the coro frame, not a
        //     ramp-local stack alloca that would dangle the moment the ramp
        //     returns. (CoroSplit may leave a *dead* `alloca` behind that the
        //     full opt pipeline's DCE strips — so we check what register_fd
        //     actually receives, not the mere presence of `alloca`.)
        let call_line = ramp_ir
            .lines()
            .find(|l| l.contains("@__demo_register_fd"))
            .expect("register_fd call must be in the ramp");
        let parked_arg = call_line
            .rsplit("ptr ")
            .next()
            .and_then(|s| s.split(')').next())
            .map(str::trim)
            .expect("register_fd parked pointer arg");
        let def_line = ramp_ir
            .lines()
            .find(|l| l.trim_start().starts_with(&format!("{parked_arg} = ")))
            .unwrap_or_else(|| panic!("no definition for {parked_arg}:\n{}", ramp_ir));
        assert!(
            def_line.contains("getelementptr") && def_line.contains("demo_park_coro.Frame"),
            "register_fd's parked pointer ({parked_arg}) must be a coro-frame GEP \
             (stable address the dispatcher can deref while suspended), got:\n  {}\nramp IR:\n{}",
            def_line.trim(),
            ramp_ir
        );
        // (3) Anti-bug-C: the post-park syscall landed on the resume edge, not
        //     dropped. The degenerate body-splitter this replaces drops exactly
        //     this work.
        assert!(
            resume_ir.contains("@__demo_syscall"),
            "post-park syscall must execute in the .resume clone; resume IR:\n{}",
            resume_ir
        );

        module
            .verify()
            .unwrap_or_else(|e| panic!("post-split module invalid: {}", e.to_string()));
    }
}
