//! Level 2 crash diagnostics — Part 2: DWARF debug-info emission.
//!
//! Part 1 (`emit_panic` printing `panic at <file>:<line>:<col> in <fn>`)
//! delivers the user-facing headline with compile-time-constant operands and
//! zero binary-size cost. Part 2 (this module) emits *actual* DWARF via
//! inkwell's `DebugInfoBuilder` so `gdb` / `lldb` users get symbolic
//! backtraces — most valuable on the JIT path (REPL/test), where ORC's GDB
//! JIT interface registers the in-process frames and binary size is moot.
//!
//! **Gated, default-off.** DWARF `.debug_*` sections would otherwise regress
//! the Phase 1–3 AOT binary-size floor on ELF targets, so emission is enabled
//! only when the `KARAC_DEBUG_INFO` env var is set (the `-g` / `--debug-info`
//! build flag wires to the same gate). When off, `Codegen::debug_info` is
//! `None` and every hook here is a cheap early-return — the default codegen
//! path is byte-for-byte unchanged.
//!
//! **Verifier safety — the load-bearing invariant.** LLVM's verifier rejects a
//! module ("!dbg attachment points at wrong subprogram for function") if an
//! instruction carries a `!dbg` whose scope does not chain to the
//! `DISubprogram` of the function the instruction lives in. Kāra emits LLVM
//! functions through many paths — `compile_function`, but also closures,
//! drop/clone glue, `Display` impls, synthesized test/`main` wrappers,
//! par-branch thunks — and they nest (a closure body compiles while its parent
//! function's `compile_function` is still on the stack). Tracking a "current
//! subprogram" as mutable state desyncs from the builder's actual insert
//! position on every nested return and produces exactly that verifier error.
//!
//! So `di_set_location` does NOT trust tracked state: it reads the subprogram
//! back from the function the builder is *currently* emitting into
//! (`FunctionValue::get_subprogram`). If that function has a subprogram, the
//! location is stamped in its scope; if it has none (synthetic glue with no
//! `di_enter_function` call), the location is *unset* so the instruction
//! carries no `!dbg` (always valid). This is correct by construction
//! regardless of nesting or which path emitted the code, and needs no
//! save/restore stack.
//!
//! `finalize()` MUST run after all functions are emitted and before any
//! verification / object emission (the verifier validates debug metadata);
//! `Codegen::di_finalize` is called right before the module-level `verify()`.

#![cfg(feature = "llvm")]

use inkwell::debug_info::{
    AsDIScope, DIFile, DIFlags, DIFlagsConstants, DWARFEmissionKind, DWARFSourceLanguage,
    DebugInfoBuilder,
};
use inkwell::module::{FlagBehavior, Module};
use inkwell::values::FunctionValue;

/// DWARF "Debug Info Version" — LLVM's current metadata schema version. Stamped
/// as a module flag; without it the backend silently drops all debug metadata.
const DEBUG_METADATA_VERSION: u64 = 3;

/// Per-module debug-info state. Created once (when the gate is on and a source
/// filename is available) at the start of `compile_program`; lives for the
/// whole module emission. Holds the `DebugInfoBuilder` and the file scope
/// reused as the parent for every `DISubprogram`. No "current function" is
/// tracked — the scope for each location is derived from the LLVM function the
/// builder is emitting into (see module docs).
pub(crate) struct DebugInfo<'ctx> {
    pub(crate) builder: DebugInfoBuilder<'ctx>,
    /// File scope — parent of every `DISubprogram` and scope of each subroutine
    /// type. Single-file for v1 (one compile unit per `.kara` file; multi-file
    /// DWARF is a follow-up).
    pub(crate) file: DIFile<'ctx>,
}

impl<'ctx> DebugInfo<'ctx> {
    /// Build the per-module debug-info scaffolding: set the "Debug Info Version"
    /// module flag, create the `DebugInfoBuilder` + compile unit, stash the file
    /// scope. `filename` is split into directory + basename so DWARF consumers
    /// resolve the source path correctly.
    pub(crate) fn new(
        context: &'ctx inkwell::context::Context,
        module: &Module<'ctx>,
        filename: &str,
    ) -> Self {
        module.add_basic_value_flag(
            "Debug Info Version",
            FlagBehavior::Warning,
            context.i32_type().const_int(DEBUG_METADATA_VERSION, false),
        );

        let (dir, base) = split_dir_base(filename);
        let (builder, compile_unit) = module.create_debug_info_builder(
            /* allow_unresolved */ true,
            DWARFSourceLanguage::C,
            base,
            dir,
            "karac",
            /* is_optimized */ false,
            /* flags */ "",
            /* runtime_ver */ 0,
            /* split_name */ "",
            DWARFEmissionKind::Full,
            /* dwo_id */ 0,
            /* split_debug_inlining */ false,
            /* debug_info_for_profiling */ false,
            /* sysroot */ "",
            /* sdk */ "",
        );
        let file = compile_unit.get_file();
        Self { builder, file }
    }
}

/// Split a path into `(directory, basename)` for `create_debug_info_builder` /
/// `create_file`. A bare filename gets `"."` as its directory. Dependency-free
/// (no `std::path`) so both halves are plain `&str` slices of the input.
fn split_dir_base(path: &str) -> (&str, &str) {
    match path.rfind('/') {
        Some(idx) => {
            let dir = if idx == 0 { "/" } else { &path[..idx] };
            (dir, &path[idx + 1..])
        }
        None => (".", path),
    }
}

impl<'ctx> super::Codegen<'ctx> {
    /// Initialize debug info if the gate (`KARAC_DEBUG_INFO`) is on and a source
    /// filename was threaded in. Called once at the top of `compile_program`.
    /// No-op (leaves `debug_info = None`) otherwise — the default path.
    pub(super) fn di_init(&mut self) {
        if self.debug_info.is_some() {
            return; // defensive against a double-call
        }
        if !read_debug_info_env() {
            return;
        }
        let Some(filename) = self.source_filename.clone() else {
            // No filename → no DWARF file to attach to. Part 1's panic-location
            // gate has the same prerequisite; stay consistent and skip.
            return;
        };
        self.debug_info = Some(DebugInfo::new(self.context, &self.module, &filename));
    }

    /// Force debug-info initialization regardless of the `KARAC_DEBUG_INFO`
    /// env gate, given a source filename has been set. Mirrors the race-free
    /// `set_strip_contracts` / `compile_to_ir_with_contracts_stripped` pattern:
    /// tests (and the `--debug-info` CLI flag's race-free path) enable DWARF
    /// without mutating process-global env, which would otherwise race parallel
    /// tests. Because `di_init` early-returns when `debug_info.is_some()`, a
    /// caller that runs this before `compile_program` keeps the forced builder.
    /// No-op if no source filename was threaded in (nothing to attach DWARF to).
    pub(super) fn force_debug_info(&mut self) {
        if self.debug_info.is_some() {
            return;
        }
        if let Some(filename) = self.source_filename.clone() {
            self.debug_info = Some(DebugInfo::new(self.context, &self.module, &filename));
        }
    }

    /// Create a `DISubprogram` for `fn_val` (named `name`, starting at source
    /// `line`) and attach it to the LLVM function, then seed an entry location.
    /// Called from `compile_function` right after the entry block is positioned.
    /// No-op when debug info is disabled.
    pub(super) fn di_enter_function(&mut self, fn_val: FunctionValue<'ctx>, name: &str, line: u32) {
        let Some(di) = self.debug_info.as_ref() else {
            return;
        };
        // A `void(void)` subroutine type is a valid minimal signature for
        // line-table debug info; parameter/local-variable DWARF is a Level-3 /
        // future concern. The verifier only requires the subprogram's type be a
        // subroutine type, not that it match the LLVM signature.
        let subroutine_ty = di.builder.create_subroutine_type(
            di.file,
            /* return type */ None,
            /* parameter types */ &[],
            DIFlags::PUBLIC,
        );
        let subprogram = di.builder.create_function(
            di.file.as_debug_info_scope(),
            name,
            /* linkage_name */ None,
            di.file,
            line,
            subroutine_ty,
            /* is_local_to_unit */ false,
            /* is_definition */ true,
            /* scope_line */ line,
            DIFlags::PUBLIC,
            /* is_optimized */ false,
        );
        fn_val.set_subprogram(subprogram);
        // Seed a location at the function's first line so prologue instructions
        // (allocas, param stores) carry a valid in-scope `!dbg`. `di_set_location`
        // re-derives the scope from `fn_val`, which now has the subprogram.
        self.di_set_location(line, 1);
    }

    /// Stamp the builder's current debug location from a 1-indexed
    /// `(line, column)`, deriving the scope from the `DISubprogram` of the
    /// function the builder is currently emitting into (ground truth — see the
    /// verifier-safety note in the module docs). If that function has no
    /// subprogram (synthetic glue), the location is *unset* so its instructions
    /// carry no `!dbg`. No-op when debug info is disabled.
    pub(super) fn di_set_location(&self, line: u32, column: u32) {
        let Some(di) = self.debug_info.as_ref() else {
            return;
        };
        let cur_fn = self
            .builder
            .get_insert_block()
            .and_then(|bb| bb.get_parent());
        let Some(sp) = cur_fn.and_then(|f| f.get_subprogram()) else {
            // No insert block, or the current function has no subprogram
            // (synthetic glue) — never stamp a foreign scope; drop any location.
            self.builder.unset_current_debug_location();
            return;
        };
        let loc = di.builder.create_debug_location(
            self.context,
            line,
            column,
            sp.as_debug_info_scope(),
            None,
        );
        self.builder.set_current_debug_location(loc);
    }

    /// Finalize debug info: resolve all temporaries. MUST run after every
    /// function is emitted and before verification / object emission. No-op
    /// when disabled.
    pub(super) fn di_finalize(&self) {
        if let Some(di) = self.debug_info.as_ref() {
            di.builder.finalize();
        }
    }
}

/// Read the `KARAC_DEBUG_INFO` gate. Unset / `"0"` → off (the default); any
/// other value → on. Mirrors the other codegen env gates (`read_auto_par_env`
/// etc.). The `-g` / `--debug-info` CLI flag sets this var for its build.
pub(super) fn read_debug_info_env() -> bool {
    matches!(std::env::var("KARAC_DEBUG_INFO"), Ok(v) if v != "0")
}
