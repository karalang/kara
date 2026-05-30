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

use std::ffi::{c_char, CStr, CString};
use std::ptr;

use llvm_sys::core::{LLVMCreateMemoryBufferWithMemoryRange, LLVMDisposeMessage};
use llvm_sys::error::{LLVMDisposeErrorMessage, LLVMErrorRef, LLVMGetErrorMessage};
use llvm_sys::ir_reader::LLVMParseIRInContext;
use llvm_sys::orc2::lljit::{
    LLVMOrcCreateLLJIT, LLVMOrcCreateLLJITBuilder, LLVMOrcDisposeLLJIT,
    LLVMOrcLLJITAddLLVMIRModule, LLVMOrcLLJITGetGlobalPrefix, LLVMOrcLLJITGetMainJITDylib,
    LLVMOrcLLJITLookup, LLVMOrcLLJITRef,
};
use llvm_sys::orc2::{
    LLVMOrcCreateDynamicLibrarySearchGeneratorForProcess, LLVMOrcCreateNewThreadSafeContext,
    LLVMOrcCreateNewThreadSafeModule, LLVMOrcDefinitionGeneratorRef,
    LLVMOrcDisposeThreadSafeContext, LLVMOrcExecutorAddress, LLVMOrcJITDylibAddGenerator,
    LLVMOrcThreadSafeContextGetContext, LLVMOrcThreadSafeContextRef,
};
use llvm_sys::prelude::{LLVMContextRef, LLVMModuleRef};

/// RAII wrapper around an LLJIT instance + its thread-safe context.
///
/// Both handles are disposed by `Drop`. The engine is keyed against
/// the native target; `Target::initialize_native` is invoked at `new`.
pub struct LLJITEngine {
    ts_ctx: LLVMOrcThreadSafeContextRef,
    jit: LLVMOrcLLJITRef,
}

impl LLJITEngine {
    pub fn new() -> Result<Self, String> {
        // inkwell shares the same llvm-sys library this crate links;
        // calling its target-init keeps a single ownership story for
        // LLVM globals.
        inkwell::targets::Target::initialize_native(
            &inkwell::targets::InitializationConfig::default(),
        )
        .map_err(|e| format!("init native target: {}", e))?;

        unsafe {
            // `LLVMOrcCreateLLJIT` consumes the builder regardless of
            // success/failure — no manual dispose needed after this call.
            let builder = LLVMOrcCreateLLJITBuilder();
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
    pub fn add_ir_module(&self, ir: &str) -> Result<(), String> {
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

            // ThreadSafeModule takes ownership of the module — DO NOT
            // dispose the LLVMModuleRef separately.
            let tsm = LLVMOrcCreateNewThreadSafeModule(module, self.ts_ctx);
            let main_jd = LLVMOrcLLJITGetMainJITDylib(self.jit);
            let err = LLVMOrcLLJITAddLLVMIRModule(self.jit, main_jd, tsm);
            if !err.is_null() {
                return Err(consume_error(err));
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

impl Drop for LLJITEngine {
    fn drop(&mut self) {
        unsafe {
            if !self.jit.is_null() {
                // Returns an error if any module materialization fails
                // during teardown; W1 ignores (we can't propagate from
                // Drop). W2 logs to stderr via a panic-hook-aware path.
                let err = LLVMOrcDisposeLLJIT(self.jit);
                if !err.is_null() {
                    LLVMDisposeErrorMessage(LLVMGetErrorMessage(err));
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
