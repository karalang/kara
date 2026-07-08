//! Phase-7 L560 W1: minimal orc2/LLJIT wrapper.
//!
//! Calls `llvm-sys::orc2` directly because inkwell 0.9 does not expose
//! ORC v2 / LLJIT. The surface here is the smallest one that lets us
//! round-trip a Kāra module through the JIT — create engine, add IR
//! module, look up symbol, dispose. W2+ add multi-module support,
//! lifetime/threading correctness, full codegen-suite integration.
//!
//! This is on the `lljit_prototype` cargo feature path and is not
//! compiled into the normal `karac` binary until W6 closes.

#![cfg(all(feature = "llvm", feature = "lljit_prototype"))]

use std::ffi::{c_char, c_void, CStr, CString};
use std::ptr;
use std::sync::OnceLock;

use llvm_sys::core::{
    LLVMCreateMemoryBufferWithMemoryRange, LLVMDisposeMessage, LLVMSetDataLayout, LLVMSetTarget,
};
use llvm_sys::error::{LLVMDisposeErrorMessage, LLVMErrorRef, LLVMGetErrorMessage};
use llvm_sys::execution_engine::LLVMCreateGDBRegistrationListener;
use llvm_sys::ir_reader::LLVMParseIRInContext;
use llvm_sys::orc2::ee::{
    LLVMOrcCreateRTDyldObjectLinkingLayerWithSectionMemoryManager,
    LLVMOrcRTDyldObjectLinkingLayerRegisterJITEventListener,
};
use llvm_sys::orc2::lljit::{
    LLVMOrcCreateLLJIT, LLVMOrcCreateLLJITBuilder, LLVMOrcDisposeLLJIT,
    LLVMOrcLLJITAddLLVMIRModule, LLVMOrcLLJITAddLLVMIRModuleWithRT,
    LLVMOrcLLJITBuilderSetObjectLinkingLayerCreator, LLVMOrcLLJITGetDataLayoutStr,
    LLVMOrcLLJITGetGlobalPrefix, LLVMOrcLLJITGetMainJITDylib, LLVMOrcLLJITGetTripleString,
    LLVMOrcLLJITLookup, LLVMOrcLLJITRef,
};
use llvm_sys::orc2::LLVMOrcThreadSafeModuleRef;
use llvm_sys::orc2::{
    LLVMOrcCreateDynamicLibrarySearchGeneratorForProcess, LLVMOrcCreateNewThreadSafeContext,
    LLVMOrcCreateNewThreadSafeModule, LLVMOrcDefinitionGeneratorRef,
    LLVMOrcDisposeThreadSafeContext, LLVMOrcExecutionSessionRef, LLVMOrcExecutorAddress,
    LLVMOrcJITDylibAddGenerator, LLVMOrcJITDylibCreateResourceTracker, LLVMOrcObjectLayerRef,
    LLVMOrcReleaseResourceTracker, LLVMOrcResourceTrackerRef, LLVMOrcResourceTrackerRemove,
    LLVMOrcThreadSafeContextGetContext, LLVMOrcThreadSafeContextRef,
};
use llvm_sys::prelude::{LLVMContextRef, LLVMModuleRef};
use llvm_sys::target_machine::{
    LLVMCodeGenOptLevel, LLVMCodeModel, LLVMCreateTargetMachine, LLVMDisposeTargetMachine,
    LLVMGetTargetFromTriple, LLVMRelocMode, LLVMTargetMachineRef, LLVMTargetRef,
};
use llvm_sys::transforms::pass_builder::{
    LLVMCreatePassBuilderOptions, LLVMDisposePassBuilderOptions, LLVMRunPasses,
};

/// Initialize the native target exactly once, process-wide.
///
/// The native-target init touches LLVM global registries that are not
/// safe to mutate from multiple threads concurrently; `OnceLock` both
/// serializes the first call and caches its outcome so subsequent
/// engine creations (on any thread) are a cheap clone of the result.
fn ensure_native_target_initialized() -> Result<(), String> {
    static INIT: OnceLock<Result<(), String>> = OnceLock::new();
    INIT.get_or_init(|| {
        inkwell::targets::Target::initialize_native(
            &inkwell::targets::InitializationConfig::default(),
        )
        .map_err(|e| format!("init native target: {}", e))
    })
    .clone()
}

/// LLJIT object-linking-layer creator that builds an **RTDyld** layer and
/// registers the process-wide **GDB JIT-registration listener** on it, so a
/// `gdb`/`lldb` attached to the JIT process can symbolize JIT'd frames from the
/// DWARF the module carries (crash-diagnostics Part 2 — the debugger *bonus*).
///
/// Why go through a creator callback instead of registering on
/// `LLVMOrcLLJITGetObjLinkingLayer(jit)` after the fact: the C API's
/// `LLVMOrcRTDyldObjectLinkingLayerRegisterJITEventListener` does an
/// **unchecked `unwrap<RTDyldObjectLinkingLayer>`** on the layer pointer.
/// If LLVM's LLJIT had defaulted this target to a JITLink object layer
/// (`ObjectLinkingLayer`, which has no `registerJITEventListener`), calling it
/// on that pointer is a type-confused cast → memory corruption. Constructing
/// the RTDyld layer *ourselves* here makes the layer type known-good by
/// construction, so the register call is always sound. The GDB listener is a
/// process-wide singleton (`LLVMCreateGDBRegistrationListener` returns the same
/// static every call), so one per engine is fine.
///
/// Best-effort: if either handle comes back null we return the layer without
/// registering rather than abort — a missing debugger integration must never
/// break JIT execution (the DWARF is still emitted + preserved regardless).
extern "C" fn rtdyld_layer_with_gdb_listener(
    _ctx: *mut c_void,
    es: LLVMOrcExecutionSessionRef,
    _triple: *const c_char,
) -> LLVMOrcObjectLayerRef {
    unsafe {
        let layer = LLVMOrcCreateRTDyldObjectLinkingLayerWithSectionMemoryManager(es);
        if layer.is_null() {
            return layer;
        }
        let listener = LLVMCreateGDBRegistrationListener();
        if !listener.is_null() {
            LLVMOrcRTDyldObjectLinkingLayerRegisterJITEventListener(layer, listener);
        }
        layer
    }
}

/// RAII wrapper around an LLJIT instance + its thread-safe context.
///
/// Both handles are disposed by `Drop`. The engine is keyed against
/// the native target; `Target::initialize_native` is invoked at `new`.
///
/// Holds raw LLVM pointers, so it is intentionally neither `Send` nor
/// `Sync` — an engine is owned and used by a single thread. Concurrency
/// is supported by giving each thread its own engine (the native-target
/// init they share is funnelled through [`ensure_native_target_initialized`]);
/// the JIT'd code may itself spawn threads (e.g. `par {}` blocks), which
/// is independent of engine ownership because worker threads run compiled
/// function pointers and never touch the engine handle.
pub struct LLJITEngine {
    ts_ctx: LLVMOrcThreadSafeContextRef,
    jit: LLVMOrcLLJITRef,
}

impl LLJITEngine {
    pub fn new() -> Result<Self, String> {
        // inkwell shares the same llvm-sys library this crate links;
        // calling its target-init keeps a single ownership story for
        // LLVM globals.
        //
        // W5 (threading): `Target::initialize_native` mutates LLVM's
        // global target registries. Calling it from two threads at once
        // (e.g. each thread building its own engine) races that global
        // state. Funnel it through a `OnceLock` so the init runs exactly
        // once process-wide regardless of how many engines are created
        // concurrently; later callers see the cached result. `get_or_init`
        // blocks competing threads until the first init completes, which
        // is the serialization the underlying C API needs.
        ensure_native_target_initialized()?;

        unsafe {
            // `LLVMOrcCreateLLJIT` consumes the builder regardless of
            // success/failure — no manual dispose needed after this call.
            let builder = LLVMOrcCreateLLJITBuilder();
            // Install our own RTDyld object-linking layer that registers the
            // GDB JIT listener (crash-diagnostics Part 2 — the gdb/lldb bonus
            // for DWARF-carrying modules). Setting a creator also pins the
            // layer type to RTDyld, which is what makes the listener
            // registration sound — see `rtdyld_layer_with_gdb_listener`. The
            // builder passes the callback its own `ExecutionSession` at LLJIT
            // construction time.
            LLVMOrcLLJITBuilderSetObjectLinkingLayerCreator(
                builder,
                rtdyld_layer_with_gdb_listener,
                ptr::null_mut(),
            );
            let mut jit: LLVMOrcLLJITRef = ptr::null_mut();
            let err = LLVMOrcCreateLLJIT(&mut jit, builder);
            if !err.is_null() {
                return Err(consume_error(err));
            }

            // Wire the process-symbol search generator into the main
            // JITDylib so libc and runtime symbols (linked into the
            // calling process) resolve at lookup time. This is the
            // load-bearing piece per the L558 (a) finding: external
            // symbols are exactly where MCJIT failed, and this is how
            // LLJIT lets them succeed.
            let prefix = LLVMOrcLLJITGetGlobalPrefix(jit);
            let main_jd = LLVMOrcLLJITGetMainJITDylib(jit);
            let mut dg: LLVMOrcDefinitionGeneratorRef = ptr::null_mut();
            let err = LLVMOrcCreateDynamicLibrarySearchGeneratorForProcess(
                &mut dg,
                prefix,
                None,
                ptr::null_mut(),
            );
            if !err.is_null() {
                LLVMOrcDisposeLLJIT(jit);
                return Err(consume_error(err));
            }
            LLVMOrcJITDylibAddGenerator(main_jd, dg);

            let ts_ctx = LLVMOrcCreateNewThreadSafeContext();
            Ok(Self { ts_ctx, jit })
        }
    }

    /// Underlying `LLVMContextRef` from the engine's thread-safe context.
    /// Use this when building modules destined for `add_ir_module`.
    fn context_ref(&self) -> LLVMContextRef {
        unsafe { LLVMOrcThreadSafeContextGetContext(self.ts_ctx) }
    }

    /// Parse an LLVM-IR text blob into a module under the engine's
    /// thread-safe context, wrap it in a `ThreadSafeModule`, and add it
    /// to the main JITDylib. The text is copied by `LLVMParseIRInContext`,
    /// so the caller's `&str` is free to drop afterwards.
    ///
    /// Multiple calls are allowed — each module's symbols become visible
    /// in the main JITDylib alongside any previously added. Symbol name
    /// collisions surface as `Lookup` errors per LLVM's symbol-resolution
    /// rules (duplicate definition).
    pub fn add_ir_module(&self, ir: &str) -> Result<(), String> {
        let tsm = self.parse_ir_into_tsm(ir)?;
        unsafe {
            let main_jd = LLVMOrcLLJITGetMainJITDylib(self.jit);
            let err = LLVMOrcLLJITAddLLVMIRModule(self.jit, main_jd, tsm);
            if !err.is_null() {
                return Err(consume_error(err));
            }
            Ok(())
        }
    }

    /// Add an IR module under a fresh `ResourceTracker`. The returned
    /// tracker lets the caller bulk-remove just this module's symbols
    /// later, without touching the rest of the JITDylib. This is the
    /// load-bearing mechanism for `karac repl` cell shadowing — a
    /// re-declared name removes the prior cell's tracker, then a new
    /// cell with the same name installs under a fresh tracker.
    ///
    /// The tracker borrows `&self` so it cannot outlive the engine.
    /// Holding the tracker is safe across multiple JIT lookups; calling
    /// `.remove()` invalidates any function pointers obtained from
    /// symbols defined by this module — that's caller responsibility.
    pub fn add_ir_module_with_tracker(&self, ir: &str) -> Result<ResourceTracker<'_>, String> {
        let tsm = self.parse_ir_into_tsm(ir)?;
        unsafe {
            let main_jd = LLVMOrcLLJITGetMainJITDylib(self.jit);
            let rt = LLVMOrcJITDylibCreateResourceTracker(main_jd);
            let err = LLVMOrcLLJITAddLLVMIRModuleWithRT(self.jit, rt, tsm);
            if !err.is_null() {
                LLVMOrcReleaseResourceTracker(rt);
                return Err(consume_error(err));
            }
            Ok(ResourceTracker {
                raw: rt,
                _engine: std::marker::PhantomData,
            })
        }
    }

    /// Shared IR-text → `ThreadSafeModule` step used by both
    /// `add_ir_module` and `add_ir_module_with_tracker`. The TSM is the
    /// boundary: once handed to `LLVMOrcLLJIT*Add*`, ownership transfers
    /// to the JIT and the caller must not dispose the underlying Module.
    fn parse_ir_into_tsm(&self, ir: &str) -> Result<LLVMOrcThreadSafeModuleRef, String> {
        // LLVM's IR parser needs a nul-terminated buffer when
        // `RequiresNullTerminator = 1`; passing a fresh CString is the
        // simplest way to satisfy it without juggling the contract.
        let ir_cstr = CString::new(ir).map_err(|e| format!("ir cstring: {}", e))?;
        let name_cstr = CString::new("karac_jit").unwrap();

        unsafe {
            // `RequiresNullTerminator = 1` is correct here — the CString
            // above guarantees nul-termination at `ir.len()`. LLVM takes
            // ownership of the memory buffer through to the parser.
            let buf = LLVMCreateMemoryBufferWithMemoryRange(
                ir_cstr.as_ptr(),
                ir.len(),
                name_cstr.as_ptr(),
                1,
            );

            let ctx = self.context_ref();
            let mut module: LLVMModuleRef = ptr::null_mut();
            let mut errmsg: *mut c_char = ptr::null_mut();
            // Returns 0 on success; non-zero means parse error and the
            // memory buffer has been consumed. On success, `module` owns
            // the parsed IR; we hand that ownership to TSM below.
            let failed = LLVMParseIRInContext(ctx, buf, &mut module, &mut errmsg);
            if failed != 0 {
                let msg = if errmsg.is_null() {
                    "LLVMParseIRInContext failed (no message)".to_string()
                } else {
                    let s = CStr::from_ptr(errmsg).to_string_lossy().into_owned();
                    LLVMDisposeMessage(errmsg);
                    s
                };
                return Err(msg);
            }

            // Align the module's data layout and target triple with the
            // LLJIT's. AOT codegen never sets these on the module (the
            // AOT TargetMachine provides them at object-write time), so
            // the parsed module here has empty / module-default layout.
            // LLJIT requires the module to match its own
            // JITTargetMachineBuilder layout — mismatch shows up as
            // wrong struct field offsets and segfaults / aborts at
            // runtime (observed on `enum E { V(Vec[i64]) }` + match
            // destructure: free() got an offset-into-the-stack pointer
            // instead of the Vec's heap data pointer). Fix: ask LLJIT
            // for its layout + triple and stamp them on the module
            // before TSM wrap.
            let dl_str = LLVMOrcLLJITGetDataLayoutStr(self.jit);
            if !dl_str.is_null() {
                LLVMSetDataLayout(module, dl_str);
            }
            let triple_str = LLVMOrcLLJITGetTripleString(self.jit);
            if !triple_str.is_null() {
                LLVMSetTarget(module, triple_str);
            }

            // Run the coroutine-lowering pipeline before handing the module
            // to LLJIT. CoroSplit is a *correctness* pass, not an
            // optimization: a `presplitcoroutine` function (the A2
            // network-async transform) is not a valid runnable function
            // until split into its ramp/resume/destroy clones — the
            // `llvm.coro.*` intrinsics are otherwise left unlowered and the
            // JIT would materialize a no-op task (the bug-C failure class).
            // The AOT path runs this inside `driver::apply_optimization_passes`;
            // the JIT path bypasses that driver entirely and hands raw IR
            // straight to `LLVMOrcLLJITAddLLVMIRModule`, so the same pass must
            // run here. For the non-coroutine modules that are everything
            // today this is a pure no-op — the coro passes only touch
            // `presplitcoroutine` functions. A pass-run failure would
            // otherwise be silent (un-split coroutine → no-op), so it is
            // surfaced as a hard `Err`. See `phase-7-codegen.md` L591 +
            // `driver.rs`.
            self.run_coro_passes(module)?;

            // ThreadSafeModule takes ownership of the module — DO NOT
            // dispose the LLVMModuleRef separately.
            Ok(LLVMOrcCreateNewThreadSafeModule(module, self.ts_ctx))
        }
    }

    /// Run the LLVM coroutine-lowering pipeline (`coro-early`, `coro-split`,
    /// `coro-cleanup`) on a freshly parsed module before it is added to the
    /// JIT. Kept in lockstep with the AOT pipeline in
    /// `driver::apply_optimization_passes` — same pass string, run
    /// unconditionally because coroutine splitting is a correctness
    /// requirement, not an optimization.
    ///
    /// `LLVMRunPasses` needs a non-null `TargetMachine`; the JIT path has no
    /// inkwell `TargetMachine` handy (the LLJIT owns its own internally and
    /// does not expose it through the C API), so build a throwaway one from
    /// the engine's own triple. The coro passes are target-independent
    /// transforms, so a generic machine on the right triple is sufficient;
    /// it is disposed before returning.
    ///
    /// `module` must be a valid `LLVMModuleRef` owned by this engine's
    /// context and not yet handed to a `ThreadSafeModule` — the only caller
    /// is `parse_ir_into_tsm`, which upholds that. FFI is localized to the
    /// `unsafe` block, mirroring `lookup_address` / `parse_ir_into_tsm`.
    fn run_coro_passes(&self, module: LLVMModuleRef) -> Result<(), String> {
        unsafe {
            let triple = LLVMOrcLLJITGetTripleString(self.jit);
            if triple.is_null() {
                return Err("LLJIT returned a null triple; cannot build a \
                            target machine for the coro pass run"
                    .to_string());
            }

            let mut target: LLVMTargetRef = ptr::null_mut();
            let mut err_msg: *mut c_char = ptr::null_mut();
            if LLVMGetTargetFromTriple(triple, &mut target, &mut err_msg) != 0 {
                let msg = if err_msg.is_null() {
                    "LLVMGetTargetFromTriple failed (no message)".to_string()
                } else {
                    let s = CStr::from_ptr(err_msg).to_string_lossy().into_owned();
                    LLVMDisposeMessage(err_msg);
                    s
                };
                return Err(format!("coro pass target lookup failed: {msg}"));
            }

            // Empty CPU + features: coro lowering is target-independent, so
            // the generic baseline for the triple is fine. PIC/Default mirror
            // the AOT machine's reloc/code-model so the throwaway machine
            // cannot disagree with the module's stamped layout.
            let empty = CString::new("").unwrap();
            let tm: LLVMTargetMachineRef = LLVMCreateTargetMachine(
                target,
                triple,
                empty.as_ptr(),
                empty.as_ptr(),
                LLVMCodeGenOptLevel::LLVMCodeGenLevelDefault,
                LLVMRelocMode::LLVMRelocPIC,
                LLVMCodeModel::LLVMCodeModelDefault,
            );
            if tm.is_null() {
                return Err("LLVMCreateTargetMachine returned null; cannot run \
                            the coro pass pipeline"
                    .to_string());
            }

            let passes = CString::new("coro-early,coro-split,coro-cleanup").unwrap();
            let opts = LLVMCreatePassBuilderOptions();
            let run_err = LLVMRunPasses(module, passes.as_ptr(), tm, opts);
            LLVMDisposePassBuilderOptions(opts);
            LLVMDisposeTargetMachine(tm);

            if !run_err.is_null() {
                return Err(format!(
                    "LLVM coroutine lowering passes failed on the JIT path: {}. \
                     This is a correctness pass (CoroSplit) — it cannot be \
                     skipped for `presplitcoroutine` functions.",
                    consume_error(run_err)
                ));
            }
            Ok(())
        }
    }

    /// Resolve `name` to an executor address. For W1 this is invoked on
    /// `main` only; W2+ extends to per-function lookup for the always-JIT
    /// codepath.
    pub fn lookup_address(&self, name: &str) -> Result<u64, String> {
        let cname = CString::new(name).map_err(|e| format!("lookup cstring: {}", e))?;
        unsafe {
            let mut addr: LLVMOrcExecutorAddress = 0;
            let err = LLVMOrcLLJITLookup(self.jit, &mut addr, cname.as_ptr());
            if !err.is_null() {
                return Err(consume_error(err));
            }
            Ok(addr)
        }
    }
}

/// Module-scoped handle for removing a module's resources from the JIT.
///
/// Obtained via [`LLJITEngine::add_ir_module_with_tracker`]. Borrows the
/// engine so it cannot outlive the engine that owns it.
///
/// Two lifecycle terminators:
/// - `.remove()` — tear down the module's resources NOW. Function pointers
///   previously obtained from this module's symbols are invalidated.
///   Idempotent at the C-API level but no-op on the Rust side after the
///   first call; subsequent `remove()` returns Ok.
/// - `drop` — releases the C++ refcount on the tracker. Does **not**
///   implicitly remove resources; if the caller wants module unloading
///   they must call `.remove()` explicitly. Dropping without remove leaves
///   the module materialized for the engine's full lifetime.
///
/// This split mirrors LLVM's `LLVMOrcResourceTrackerRemove` vs
/// `LLVMOrcReleaseResourceTracker` separation — remove tears down,
/// release ref-counts.
pub struct ResourceTracker<'engine> {
    raw: LLVMOrcResourceTrackerRef,
    _engine: std::marker::PhantomData<&'engine LLJITEngine>,
}

impl ResourceTracker<'_> {
    /// Tear down the module's resources. Function pointers from this
    /// module's symbols are invalidated after this returns.
    pub fn remove(&self) -> Result<(), String> {
        unsafe {
            let err = LLVMOrcResourceTrackerRemove(self.raw);
            if !err.is_null() {
                return Err(consume_error(err));
            }
        }
        Ok(())
    }
}

impl Drop for ResourceTracker<'_> {
    fn drop(&mut self) {
        // Release the refcount; does NOT remove resources. If the
        // caller wanted removal, they called `.remove()` already.
        unsafe { LLVMOrcReleaseResourceTracker(self.raw) };
    }
}

impl Drop for LLJITEngine {
    fn drop(&mut self) {
        unsafe {
            if !self.jit.is_null() {
                // Returns an error if any module materialization fails
                // during teardown. We can't propagate a `Result` from
                // `Drop`, but swallowing the failure silently hides real
                // problems (W5 error-handling: teardown failures must be
                // observable), so surface it on stderr. `consume_error`
                // reads the message and frees both the error and message.
                let err = LLVMOrcDisposeLLJIT(self.jit);
                if !err.is_null() {
                    eprintln!("LLJITEngine::drop: dispose error: {}", consume_error(err));
                }
            }
            if !self.ts_ctx.is_null() {
                LLVMOrcDisposeThreadSafeContext(self.ts_ctx);
            }
        }
    }
}

/// Consumes an `LLVMErrorRef`, returning its message as an owned `String`.
/// The error reference is invalidated by the underlying `LLVMGetErrorMessage`
/// call — do not use `err` after this returns.
unsafe fn consume_error(err: LLVMErrorRef) -> String {
    let msg_ptr = unsafe { LLVMGetErrorMessage(err) };
    if msg_ptr.is_null() {
        return "llvm error (no message)".to_string();
    }
    let msg = unsafe { CStr::from_ptr(msg_ptr) }
        .to_string_lossy()
        .into_owned();
    unsafe { LLVMDisposeErrorMessage(msg_ptr) };
    msg
}
