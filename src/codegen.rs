//! LLVM code generation for Kāra programs.
//!
//! Compiles the AST to LLVM IR, then to native object files.
//! Uses the `inkwell` crate for LLVM bindings.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::rc::Rc;

use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::Linkage;
use inkwell::module::Module;
use inkwell::targets::{FileType, TargetData};
use inkwell::types::{BasicMetadataTypeEnum, BasicTypeEnum, FunctionType, StructType};
use inkwell::values::{BasicValueEnum, FunctionValue, GlobalValue, PointerValue};
use inkwell::AddressSpace;

use crate::ast::*;
use crate::concurrency::{ConcurrencyAnalysis, FunctionConcurrency};
use crate::ownership::{CapturePath, OwnershipCheckResult, OwnershipMode, ParCaptureMode};
use crate::resolver::SpanKey;
use crate::token::Span;

mod assoc_call;
mod bounded_channel;
mod cabi;
mod call_dispatch;
mod calls;
mod channel;
mod clone_drop;
mod closures;
mod collections;
mod control_flow;
mod control_flow_bce;
mod control_flow_for;
mod control_flow_match;
mod control_flow_slice;
mod coro;
mod debug_info;
mod declarations;
mod driver;
mod entry_chains;
mod expr_ops;
mod exprs;
mod file;
mod functions;
mod helpers;
mod http;
mod json;
#[cfg(all(feature = "llvm", feature = "lljit_prototype"))]
mod lljit;
#[cfg(all(feature = "llvm", feature = "lljit_prototype"))]
pub use lljit::{LLJITEngine, ResourceTracker};
mod contracts;
mod maps;
mod method_call;
mod module_bindings;
mod mono;
mod par_blocks;
mod param_own;
mod pattern_binding;
mod provider;
mod reduce;
mod refinement;
mod runtime;
mod state;
mod stmts;
mod synth;
mod synth_display;
mod synth_drop;
mod task_group;
mod tcp;
mod tensor;
mod test_assert;
mod tls;
mod types_lowering;
mod vec_method;

use driver::{
    apply_optimization_passes, create_target_machine, read_auto_par_env,
    read_runtime_debug_metadata_env, read_strip_contracts_env, read_strip_error_trace_env,
};
pub use driver::{
    link_executable, link_executable_exports, link_executable_with_sanitizer,
    link_wasm_executable_threaded, print_target_cpu_listing, validate_target_cpu,
    validate_target_features,
};
use helpers::{
    impl_target_name, make_impl_method_function, method_is_compiler_builtin, method_self_is_value,
};
use state::{
    AssertedIndexBound, CleanupAction, EnumLayout, LoopFrame, MapMonoMethods, SharedTypeInfo,
    SoaLayout, SpawnSiteRecord, VarSlot,
};

// ── Public API ─────────────────────────────────────────────────

/// Compile a Kāra program to LLVM IR text (for debugging/testing).
pub fn compile_to_ir(
    program: &Program,
    ownership: Option<&OwnershipCheckResult>,
    concurrency: Option<&ConcurrencyAnalysis>,
) -> Result<String, String> {
    compile_to_ir_with_options(program, ownership, concurrency, None, None)
}

/// Like [`compile_to_ir`] but accepts optional source-filename and source-text
/// strings used by side features:
///   - `source_filename` is materialized as a deduped global string and
///     passed to `karac_error_trace_push(...)` at `?` failure sites so
///     error-return traces print as `<file>:<line>:<col>`. `None` preserves
///     the MVP fallback `<line>:<col>` output.
///   - `source_text` is consumed by the SpawnSiteId metadata table emission
///     (Debugger Contract slice 3) to resolve `par {}` byte offsets to
///     `(line, col)` for the `KARAC_SPAWN_SITES` global. `None` records
///     `(0, 0)` for each site — the table still emits, just without
///     source-position fidelity.
pub fn compile_to_ir_with_options(
    program: &Program,
    ownership: Option<&OwnershipCheckResult>,
    concurrency: Option<&ConcurrencyAnalysis>,
    source_filename: Option<&str>,
    source_text: Option<&str>,
) -> Result<String, String> {
    compile_to_ir_with_hot_swap(
        program,
        ownership,
        concurrency,
        source_filename,
        source_text,
        false,
    )
}

/// Slice c-repl.B.5.1: types eligible for REPL value-snapshotting.
/// A top-level immutable `let name = expr` binding whose Kāra type
/// lowers to one of these forms can have its bound value stashed in
/// an LLVM global at first emission and replayed (via a load from
/// that global, skipping the original RHS) on every subsequent cell.
/// The `Prim` in the name is historical (B.5.2 extends the set to
/// String — see the `String` variant below); the name is kept to
/// avoid churning the public surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotPrimKind {
    /// Kāra `i64`.
    I64,
    /// Kāra `f64`.
    F64,
    /// Kāra `bool` — lowered as i1 in slot, but the global stashes
    /// as i8 so the storage width is portable.
    Bool,
    /// Kāra `Char` — lowered as i32 (Unicode scalar value).
    Char,
    /// Slice c-repl.B.5.2: Kāra `String` — lowered as the standard
    /// `{ i8*, i64, i64 }` (ptr, len, cap) struct. The global holds
    /// the same triple; capture transfers buffer ownership to the
    /// global by zeroing the let slot's cap (so the queued
    /// `FreeVecBuffer` cleanup no-ops at scope exit) and replay
    /// loads the triple into a fresh slot whose own cleanup is
    /// skipped. Mut String bindings fall through to pass-through —
    /// same-cell `push_str` after capture would otherwise leave the
    /// global pointing at a freed buffer.
    String,
    /// Slice c-repl.B.5.3: Kāra `Vec[T]` for primitive `T`. Storage
    /// layout matches String (`{ ptr, len, cap }` — the same
    /// `vec_struct_type()` shape every Kāra Vec uses); the variant
    /// carries the element kind so the replay path can re-register
    /// `vec_elem_types[name]` with the right LLVM type for downstream
    /// method/index dispatch. Capture transfers buffer ownership to
    /// the global by zeroing the let slot's cap (suppressing
    /// `FreeVecBuffer`), same shape as String. Mut Vec bindings fall
    /// through to pass-through — same-cell `xs.push(…)` after
    /// capture would realloc and diverge from the snapshot global.
    /// Aggregate-element Vecs (`Vec[String]`, `Vec[<user struct>]`)
    /// are out of scope for v1: the shallow `{ ptr, len, cap }`
    /// transfer leaves the inner heap pointers shared between the
    /// global and the source cell's element drops; the design needs
    /// per-element retain/release or a deeper deferral story.
    Vec(VecElemKind),
    /// Slice c-repl.B.5.3b: Kāra `Map[K, V]` for primitive K and V.
    /// Storage is a single opaque handle pointer (i8*) — the Map's
    /// state lives behind that pointer, managed by the `karac_map_*`
    /// runtime fns. Capture transfers handle ownership to the global
    /// by storing the loaded pointer + nulling the let slot's
    /// pointer; `karac_map_free` and `karac_map_free_with_drop_vec`
    /// are both null-safe (early-return on null), so the scope-exit
    /// `FreeMapHandle` cleanup becomes a no-op for the captured
    /// slot. Replay loads the handle into a fresh slot and re-
    /// registers `map_key_types[name]` / `map_val_types[name]` /
    /// `map_key_type_names[name]` so downstream method dispatch
    /// (`m.get(k)`, `m.insert(k, v)`) routes through the Map
    /// surface unchanged. Mut Map bindings fall through to pass-
    /// through — same-cell `m.insert(…)` after capture would mutate
    /// the now-snapshot-owned handle, and cell N+1 would observe
    /// the post-mutation state, diverging from the interpreter's
    /// snapshot semantic. Aggregate-key / aggregate-value /
    /// shared-K / shared-V Map shapes are out of scope for v1 —
    /// the runtime's per-entry buffer-drop walk + the codegen-side
    /// shared rc-dec walk both need per-entry retain/release that
    /// the shallow handle transfer can't carry.
    Map { key: VecElemKind, val: VecElemKind },
    /// Slice c-repl.B.5.3c: Kāra `Set[T]` for primitive `T`. Set lowers
    /// to `Map[T, ()]` at codegen (`karac_map_new(elem_size, 0, …)` —
    /// `val_size = 0` collapses the value slot inside the bucket
    /// layout) and reuses the Map runtime end-to-end — same opaque
    /// handle storage, same `karac_map_free` cleanup, same
    /// `FreeMapHandle` `CleanupAction`. Capture/replay mirror
    /// `Map { … }` exactly; the variant only carries the element kind
    /// so replay can re-register `set_elem_types[name]` /
    /// `set_elem_type_names[name]` for downstream method dispatch
    /// (`s.contains(x)`, `s.insert(x)`). Mut Set bindings fall through
    /// to pass-through, same as Map. Aggregate-element sets
    /// (`Set[String]`, `Set[<user struct>]`) and `Set[shared T]` are
    /// out of scope for v1 — they need the per-entry retain/release
    /// walk the shallow handle transfer can't carry, same as the
    /// aggregate Map cases.
    Set(VecElemKind),
}

/// Slice c-repl.B.5.3: Vec element kinds eligible for the v1 snapshot
/// port. Limited to primitives that round-trip cleanly through one
/// `{ ptr, len, cap }` triple — i.e., the element drop is a no-op so
/// the global can take buffer ownership without leaving dangling
/// per-element references. `Vec[String]` and `Vec[<user struct>]`
/// need per-element ref/drop accounting and are deferred.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VecElemKind {
    /// `Vec[i64]`.
    I64,
    /// `Vec[f64]`.
    F64,
    /// `Vec[bool]` — elements stored as i1 in the buffer (Kāra's
    /// codegen lowers Vec[bool] elements to i8 in the heap buffer for
    /// portable storage; the snapshot global doesn't see the buffer's
    /// internal layout, only the triple).
    Bool,
    /// `Vec[char]` — elements stored as i32 (Unicode scalar value).
    Char,
}

/// Slice c-repl.B.4: REPL-cell codegen entry for the JIT path.
///
/// `declare_only_fns`: names of free functions whose bodies are
///   already installed in the JIT's JITDylib by a prior cell.
///   Codegen emits `declare i64 @<name>(...)` for these (signature
///   only, no body), so the JIT linker resolves calls to them
///   against the previously-installed definition. Functions absent
///   from this set lower with their full body, as usual.
///
/// `main_symbol`: the LLVM symbol the AST's `fn main()` should be
///   registered under. The REPL JIT path passes
///   `cell_main_<cell_id>` so each cell's main coexists in the
///   JITDylib without colliding. The i32-return + special return-
///   zero arm still fires (the AST-side check on `func.name == "main"`
///   is unchanged); only the LLVM symbol changes.
///
/// Returns the textual LLVM IR for the new cell. The caller frames
/// it onto the runner's stdin via the slice-B.A protocol.
pub fn compile_to_ir_for_repl_cell(
    program: &Program,
    declare_only_fns: &std::collections::HashSet<String>,
    main_symbol: &str,
) -> Result<String, String> {
    compile_to_ir_for_repl_cell_with_snapshots(
        program,
        declare_only_fns,
        main_symbol,
        &HashMap::new(),
        &HashMap::new(),
    )
}

/// Slice c-repl.B.5.1: extended variant of
/// [`compile_to_ir_for_repl_cell`] that threads two snapshot sets
/// through to the codegen pass.
///
/// `snapshot_capture`: top-level `let <name> = <expr>` bindings in
///   the current cell whose post-bind value should be stored to an
///   externally-visible LLVM global `__karac_repl_snapshot_<name>`.
///   The original RHS still runs (this is the binding's first
///   evaluation in the session). Subsequent cells will discover
///   the global via `snapshot_replay`.
///
/// `snapshot_replay`: top-level `let <name> = <expr>` bindings
///   whose RHS should be SKIPPED in this cell's codegen — the
///   bound value is loaded from `__karac_repl_snapshot_<name>`
///   (declared external in this module) instead. The synthetic
///   source still carries the let stmt so resolver/typechecker
///   accept downstream references to the binding.
///
/// Mutual exclusion: a name appears in at most one of the two maps
/// per cell; replay wins when the parent's set-builder sees both
/// possible (the binding was both replayed AND newly defined in
/// the same cell, which Kāra's resolver rejects anyway).
///
/// The original [`compile_to_ir_for_repl_cell`] entry delegates here
/// with empty snapshot maps; non-REPL callers don't need to know
/// this variant exists.
pub fn compile_to_ir_for_repl_cell_with_snapshots(
    program: &Program,
    declare_only_fns: &std::collections::HashSet<String>,
    main_symbol: &str,
    snapshot_capture: &HashMap<String, SnapshotPrimKind>,
    snapshot_replay: &HashMap<String, SnapshotPrimKind>,
) -> Result<String, String> {
    let context = Context::create();
    let mut cg = Codegen::new(&context, "karac_repl_cell");
    cg.declare_only_fns = declare_only_fns.clone();
    cg.main_symbol_override = Some(main_symbol.to_string());
    cg.snapshot_capture = snapshot_capture.clone();
    cg.snapshot_replay = snapshot_replay.clone();
    cg.compile_program(program)?;
    Ok(cg.module.print_to_string().to_string())
}

/// `karac test` persistent shared-module codegen. Emits all the source
/// module's items + the Debugger-Contract globals (no `main_symbol_override`,
/// so the globals are NOT suppressed — this module owns them for the
/// session), but forces every top-level fn / impl method to `External`
/// linkage so the per-test `main` modules' declare-only references resolve
/// to them across the shared JITDylib. Installed once in the runner via the
/// `module` command and referenced declare-only by every per-test `main`.
pub fn compile_to_ir_for_test_module(
    program: &Program,
    source_filename: Option<&str>,
) -> Result<String, String> {
    let context = Context::create();
    let mut cg = Codegen::new(&context, "karac_test_module");
    cg.force_external_linkage = true;
    cg.source_filename = source_filename.map(|s| s.to_string());
    cg.compile_program(program)?;
    Ok(cg.module.print_to_string().to_string())
}

/// Resolve an `impl` block's target type to its bare type name (the
/// last path segment), or `None` for non-path targets. Public wrapper
/// over the codegen-internal `impl_target_name` so the REPL's
/// declare-only bookkeeping (slice c-repl.B.4 impl-method extension)
/// can compute the `Type.method` LLVM symbol keys exactly as
/// `compile_program` does, keeping the two in lockstep.
///
/// Gated on `lljit_prototype` — its only caller is `run_cell_via_jit`,
/// which lives behind the same feature; without the gate the function
/// is dead code under a plain `--features llvm` build.
#[cfg(feature = "lljit_prototype")]
pub(crate) fn impl_target_name_for_repl(target: &crate::ast::TypeExpr) -> Option<String> {
    helpers::impl_target_name(target)
}

/// The fully-lowered `std.tracing` baked-stdlib program — parsed,
/// desugared, resolved, type-checked, and lowered, so it carries the
/// span-keyed side tables (`pattern_binding_types`,
/// `method_callee_types`, …) that codegen's body lowering consumes.
///
/// **Why a dedicated lowered copy.** Unlike the rest of the stdlib (whose
/// codegen-reachable methods are `#[compiler_builtin]` + hand-rolled
/// lowerings), the tracing methods are real Kāra source, so the
/// maintainable codegen is to compile that source. But codegen body
/// lowering is driven by typechecker side tables keyed by source span,
/// and the baked stdlib is only ever *signature*-registered — its bodies
/// are never type-checked, so those tables are empty for it (a
/// `let mut x = self.fields; x.push(..)` body can't find that `x` is a
/// `Vec`). Running the normal pipeline over `tracing.kara` in isolation
/// populates them (verified: the source type-checks clean standalone).
/// [`Codegen::compile_tracing_stdlib_methods`] swaps these tables in
/// while it emits the bodies; since the tracing AST carries `tracing.kara`
/// spans and the user program is never active during that window, the
/// swap is collision-free (no span re-basing needed).
/// Parse → desugar → resolve → typecheck → lower one baked stdlib `.kara`
/// source into a `Program` whose impl-method bodies codegen can compile
/// (phase-7 line 889). The lowering pass populates the span-keyed side
/// tables (`string_typed_exprs`, `method_callee_types`, …) that the
/// body-emission pass swaps in. Each stdlib module resolves/typechecks
/// standalone (the prelude is always in scope), so no cross-module link is
/// needed for self-contained modules like `ordering` / `tracing`.
fn lower_stdlib_source(module: &str, src: &str) -> Program {
    let mut parsed = crate::parse(src);
    debug_assert!(
        parsed.errors.is_empty(),
        "{module}.kara failed to parse for codegen lowering: {:?}",
        parsed.errors
    );
    crate::desugar_program(&mut parsed.program);
    let resolve = crate::resolve(&parsed.program);
    let tc = crate::typecheck(&parsed.program, &resolve);
    let mut program = parsed.program;
    crate::lower(&mut program, &tc);
    program
}

static TRACING_LOWERED_PROGRAM: std::sync::LazyLock<Program> = std::sync::LazyLock::new(|| {
    lower_stdlib_source("tracing", include_str!("../runtime/stdlib/tracing.kara"))
});

/// The lowered `std.tracing` program codegen compiles its impl bodies
/// from. See [`TRACING_LOWERED_PROGRAM`] and
/// [`Codegen::declare_stdlib_program`].
fn tracing_stdlib_program() -> &'static Program {
    &TRACING_LOWERED_PROGRAM
}

static ORDERING_LOWERED_PROGRAM: std::sync::LazyLock<Program> = std::sync::LazyLock::new(|| {
    lower_stdlib_source("ordering", include_str!("../runtime/stdlib/ordering.kara"))
});

/// The lowered `std` `Ordering` program — first non-`#[compiler_builtin]`
/// stdlib module compiled through the generalized [`Codegen::declare_stdlib_program`]
/// / [`Codegen::compile_stdlib_program`] passes (phase-7 line 889 slice 1).
/// `Ordering`'s `is_lt`/`is_le`/`is_gt`/`is_ge`/`is_eq` are concrete,
/// non-generic `match self` bodies — pure general lowerings, no hand-rolled
/// codegen.
fn ordering_stdlib_program() -> &'static Program {
    &ORDERING_LOWERED_PROGRAM
}

/// The baked stdlib modules whose real (non-`#[compiler_builtin]`) impl
/// bodies codegen compiles via the generalized stdlib-body passes, beyond
/// the special-cased `tracing` program. Phase-7 line 889 grows this list
/// one module at a time as each module's bodies are verified to lower.
fn compiled_stdlib_programs() -> [&'static Program; 1] {
    [ordering_stdlib_program()]
}

/// A real-source stdlib module (`std.tracing`, `Ordering`) is SKIPPED at
/// codegen — neither layout-declared nor body-compiled — when the user
/// program defines a struct/enum whose name collides with a type that module
/// exports. codegen's `struct_types` / `enum_layouts` are flat name-keyed
/// maps, so declaring the stdlib type would overwrite the user's same-named
/// type (or vice-versa) and the user's literals/returns would build against
/// the WRONG layout and fail module verification — self-hosting blocker #6:
/// the lexer's `struct Span { line, column, offset, length }` collided with
/// `std.tracing`'s `struct Span { name, span_id, parent_id, fields }`.
///
/// Skipping the WHOLE module (not just the colliding type) is required for
/// soundness: the module's own method bodies reference its types through the
/// same shared maps, so declaring its `Span` while the user's is live would
/// miscompile those bodies. A program that redefines a module's public type
/// name therefore cannot also use that module — an unambiguous, acceptable
/// trade (you redefined its surface), and an unused module's bodies are
/// dead-stripped at link anyway, so there's no size cost to keeping it when
/// there's no collision. The module surface is derived from its own items,
/// so this stays correct as the baked stdlib grows. The declare-pass and the
/// body-compile pass MUST gate on this identically (declaring a module whose
/// bodies are skipped would leave undefined method symbols, and vice-versa).
fn user_redefines_stdlib_type(user: &Program, stdlib: &Program) -> bool {
    fn type_name(item: &Item) -> Option<&str> {
        match item {
            Item::StructDef(s) => Some(s.name.as_str()),
            Item::EnumDef(e) => Some(e.name.as_str()),
            _ => None,
        }
    }
    let user_types: std::collections::HashSet<&str> =
        user.items.iter().filter_map(type_name).collect();
    stdlib
        .items
        .iter()
        .filter_map(type_name)
        .any(|n| user_types.contains(n))
}

/// Variant of [`compile_to_ir_with_options`] that accepts the
/// phase-7 line-5 `--enable-hot-swap` flag. When `true`, the codegen
/// emits PLT-style indirection through `@karac_hotswap_table` for every
/// call to a user-defined `pub fn` (extern-public module symbol);
/// internal calls stay direct. The table + an initializer ctor are
/// emitted at module finalize; v1 ships the table populated with
/// direct pointers, so the perf delta is the load+indirect-call cost
/// per call site. Provides the artifact-format reservation for the
/// post-v1 continuous-PGO + shared-object reload story.
pub fn compile_to_ir_with_hot_swap(
    program: &Program,
    ownership: Option<&OwnershipCheckResult>,
    concurrency: Option<&ConcurrencyAnalysis>,
    source_filename: Option<&str>,
    source_text: Option<&str>,
    enable_hot_swap: bool,
) -> Result<String, String> {
    let context = Context::create();
    let mut cg = Codegen::new(&context, "karac_module");
    cg.load_rc_fallback(ownership);
    cg.load_concurrency_analysis(concurrency);
    cg.set_source_filename(source_filename);
    cg.set_source_text(source_text);
    cg.set_hot_swap_enabled(enable_hot_swap);
    cg.compile_program(program)?;
    Ok(cg.module.print_to_string().to_string())
}

/// Compile to textual LLVM IR with contract machinery **stripped** (design.md
/// § Contracts: "stripped in release"). Equivalent to forcing
/// `KARAC_STRIP_CONTRACTS=1` for this one compile, but via an explicit setter
/// so the decision is race-free (no process-global env mutation) — used by the
/// release-build path and by the IR-contrast tests. `requires` / `ensures` /
/// `old(...)` / `invariant` asserts are not emitted.
pub fn compile_to_ir_with_contracts_stripped(
    program: &Program,
    ownership: Option<&OwnershipCheckResult>,
    concurrency: Option<&ConcurrencyAnalysis>,
) -> Result<String, String> {
    let context = Context::create();
    let mut cg = Codegen::new(&context, "karac_module");
    cg.load_rc_fallback(ownership);
    cg.load_concurrency_analysis(concurrency);
    cg.set_strip_contracts(true);
    cg.compile_program(program)?;
    Ok(cg.module.print_to_string().to_string())
}

/// Compile to textual LLVM IR with Level 2 **DWARF debug info forced on**
/// (crash-diagnostics Part 2), regardless of the `KARAC_DEBUG_INFO` env gate.
/// Race-free counterpart used by tests + the `--debug-info` CLI path — mirrors
/// `compile_to_ir_with_contracts_stripped` so enabling DWARF in one test does
/// not perturb process-global env shared with parallel tests. The emitted IR
/// carries `!llvm.dbg.cu` / `DICompileUnit` / per-function `DISubprogram` and
/// per-instruction `!dbg` locations. Uses a fixed `debug.kara` source filename
/// so debug info attaches to a `DIFile` (DWARF needs a file to anchor to).
pub fn compile_to_ir_with_debug_info(
    program: &Program,
    ownership: Option<&OwnershipCheckResult>,
    concurrency: Option<&ConcurrencyAnalysis>,
) -> Result<String, String> {
    let context = Context::create();
    let mut cg = Codegen::new(&context, "karac_module");
    cg.load_rc_fallback(ownership);
    cg.load_concurrency_analysis(concurrency);
    cg.set_source_filename(Some("debug.kara"));
    cg.force_debug_info();
    cg.compile_program(program)?;
    Ok(cg.module.print_to_string().to_string())
}

/// Like [`compile_to_ir_with_options`] but forces the `?`-error-return-trace
/// instrumentation off (the `release` strip, as if `KARAC_STRIP_ERROR_TRACE=1`)
/// via an explicit setter so the decision is race-free. No `karac_error_trace
/// _push` / `_clear` calls are emitted at `?` sites. Used by the IR-contrast
/// tests; the `release` build path strips this alongside contracts.
pub fn compile_to_ir_with_error_trace_stripped(
    program: &Program,
    ownership: Option<&OwnershipCheckResult>,
    concurrency: Option<&ConcurrencyAnalysis>,
) -> Result<String, String> {
    let context = Context::create();
    let mut cg = Codegen::new(&context, "karac_module");
    cg.load_rc_fallback(ownership);
    cg.load_concurrency_analysis(concurrency);
    cg.set_strip_error_trace(true);
    cg.compile_program(program)?;
    Ok(cg.module.print_to_string().to_string())
}

/// Compile a Kāra program to a native object file.
pub fn compile_to_object(
    program: &Program,
    output_path: &str,
    ownership: Option<&OwnershipCheckResult>,
    concurrency: Option<&ConcurrencyAnalysis>,
) -> Result<(), String> {
    compile_to_object_with_options(program, output_path, ownership, concurrency, None, None)
}

/// Compile to a native object with the A2 slice 2b.3 **coroutine path enabled**
/// ([`Codegen::set_coro_enabled`]): network-boundary free functions compile as
/// LLVM coroutines driven by the runtime dispatcher (register fd + `coro.suspend`
/// per park; the caller waits on a `karac_runtime_park_slot`), instead of the
/// degenerate `emit_state_machine_poll_fn_for_key` body-splitter. The program
/// must already carry `state_struct_layouts` / `yield_points` /
/// `callee_network_yield_effect` (populated by the effectcheck +
/// `build_state_struct_layouts` pipeline). Opt-in until the flip-the-default
/// slice; today only the E2E test reaches for it. See
/// docs/spikes/network-async-coroutine-transform.md § 6¾.
pub fn compile_to_object_with_coro(
    program: &Program,
    output_path: &str,
    ownership: Option<&OwnershipCheckResult>,
    concurrency: Option<&ConcurrencyAnalysis>,
) -> Result<(), String> {
    let context = Context::create();
    let mut cg = Codegen::new(&context, "karac_module");
    cg.load_rc_fallback(ownership);
    cg.load_concurrency_analysis(concurrency);
    cg.set_coro_enabled(true);
    cg.compile_program(program)?;

    let target_machine = create_target_machine()?;
    apply_optimization_passes(&cg.module, &target_machine)?;
    target_machine
        .write_to_file(&cg.module, FileType::Object, Path::new(output_path))
        .map_err(|e| format!("Failed to write object file: {}", e))
}

/// Like [`compile_to_object_with_coro`] but returns the textual LLVM IR **after
/// the coroutine lowering passes** (`coro-early,coro-split,coro-cleanup`) have
/// run, so the CoroSplit-generated `.resume` / `.destroy` / `.cleanup` clones
/// are present for structural inspection. The general optimization pipeline is
/// deliberately NOT run, keeping the clones close to what CoroSplit emits.
///
/// Used by the A2 slice-4 destroy-edge drop test
/// (`tests/coro_e2e.rs`): it asserts the `.destroy` clone of a coroutine that
/// holds a heap local across a park frees that heap on the cancel/teardown edge
/// — the path a future slice-5 cancel trigger exercises at runtime, and the one
/// that would otherwise leak. See
/// docs/spikes/network-async-coroutine-transform.md § 7 slice 4.
pub fn compile_to_ir_with_coro_split(
    program: &Program,
    ownership: Option<&OwnershipCheckResult>,
    concurrency: Option<&ConcurrencyAnalysis>,
) -> Result<String, String> {
    let context = Context::create();
    let mut cg = Codegen::new(&context, "karac_module");
    cg.load_rc_fallback(ownership);
    cg.load_concurrency_analysis(concurrency);
    cg.set_coro_enabled(true);
    cg.compile_program(program)?;

    let target_machine = create_target_machine()?;
    let opts = inkwell::passes::PassBuilderOptions::create();
    cg.module
        .run_passes("coro-early,coro-split,coro-cleanup", &target_machine, opts)
        .map_err(|e| format!("coro pipeline failed: {}", e))?;
    Ok(cg.module.print_to_string().to_string())
}

/// Like [`compile_to_object`] but accepts optional source-filename and
/// source-text strings; see [`compile_to_ir_with_options`] for the
/// rationale and how each is consumed.
pub fn compile_to_object_with_options(
    program: &Program,
    output_path: &str,
    ownership: Option<&OwnershipCheckResult>,
    concurrency: Option<&ConcurrencyAnalysis>,
    source_filename: Option<&str>,
    source_text: Option<&str>,
) -> Result<(), String> {
    compile_to_object_with_hot_swap(
        program,
        output_path,
        ownership,
        concurrency,
        source_filename,
        source_text,
        false,
        false,
        // coro_enabled: the test-facing object path stays on the legacy
        // degenerate state-machine path (its IR tests assert that shape);
        // the CLI build/run paths pass `true`. Convergence is a later slice.
        false,
    )
}

/// Variant of [`compile_to_object_with_options`] that accepts the
/// phase-7 line-5 `--enable-hot-swap` flag and the phase-9 `--release`
/// contract-stripping flag. See [`compile_to_ir_with_hot_swap`] for the
/// hot-swap codegen contract and [`compile_to_ir_with_contracts_stripped`]
/// for the stripping semantics.
///
/// `release` carries `karac build --release`: when `true` it strips **all**
/// debug-only runtime instrumentation for this compile — contracts (via
/// [`Codegen::set_strip_contracts`]) and the `?`-error-return-trace (via
/// [`Codegen::set_strip_error_trace`]). When `false` each field keeps the
/// env-derived default (`KARAC_STRIP_CONTRACTS` / `KARAC_STRIP_ERROR_TRACE`)
/// that `Codegen::new` already applied — so the flag and the env knobs compose
/// with OR semantics and a bare build never *un*-strips an env-requested strip.
// Two build-toggle bools (hot-swap, release-strip) on top of the source/
// ownership/concurrency context push this to 8 params. A bundling options
// struct would ripple through every call site for no readability win at this
// thin public-API boundary; the `enable_hot_swap` / `release` names are
// self-documenting at the (few) call sites. Matches `cmd_build`'s allow.
///
/// `coro_enabled` carries the A2 coroutine network-async transform: when `true`
/// (the CLI `karac build` / `karac run` path), network-boundary fns compile as
/// dispatcher-driven LLVM coroutines (`Codegen::set_coro_enabled`) — the bug-C
/// fix, so a spawned per-connection handler actually executes + parks. The
/// test-facing `compile_to_object` chain passes `false` to keep the legacy
/// degenerate state-machine poll-fn path under test (the convergence — flip
/// every object path on + retire the degenerate emitter + migrate its IR tests
/// — is a follow-on slice). See docs/spikes/network-async-coroutine-transform.md.
#[allow(clippy::too_many_arguments)]
pub fn compile_to_object_with_hot_swap(
    program: &Program,
    output_path: &str,
    ownership: Option<&OwnershipCheckResult>,
    concurrency: Option<&ConcurrencyAnalysis>,
    source_filename: Option<&str>,
    source_text: Option<&str>,
    enable_hot_swap: bool,
    release: bool,
    coro_enabled: bool,
) -> Result<(), String> {
    let context = Context::create();
    let mut cg = Codegen::new(&context, "karac_module");
    cg.load_rc_fallback(ownership);
    cg.load_concurrency_analysis(concurrency);
    cg.set_source_filename(source_filename);
    cg.set_source_text(source_text);
    cg.set_hot_swap_enabled(enable_hot_swap);
    if release {
        cg.set_strip_contracts(true);
        cg.set_strip_error_trace(true);
    }
    cg.set_coro_enabled(coro_enabled);
    cg.compile_program(program)?;

    let target_machine = create_target_machine()?;
    apply_optimization_passes(&cg.module, &target_machine)?;
    target_machine
        .write_to_file(&cg.module, FileType::Object, Path::new(output_path))
        .map_err(|e| format!("Failed to write object file: {}", e))
}

/// Compile the **threaded pass** of a `--features wasm-threads` build to
/// a wasm32-wasip1-threads object (phase-10 "WASM concurrency lowering —
/// `--features wasm-threads` opt-in"). The dual-artifact sibling of the
/// sequential pass's [`compile_to_object_with_hot_swap`] call: the
/// front-end ran once; this pass re-emits with
///
/// - the real concurrency analysis (auto-par re-enabled via
///   [`Codegen::set_wasm_threaded_pass`] — the threaded module has a
///   worker pool, so fan-outs pay off there),
/// - the threaded target machine (`wasm32-wasip1-threads` triple +
///   `+atomics,+bulk-memory,+mutable-globals` — wasm-ld refuses
///   `--shared-memory` against an object without them), whose triple +
///   datalayout re-pin the module after `Codegen::new`'s
///   active-target-keyed default (datalayout is identical across the
///   two wasm triples; the re-pin keeps the emitted IR honest).
///
/// No hot-swap parameter: `--enable-hot-swap` is rejected on every wasm
/// target before codegen. `coro` stays on for parity with the
/// sequential pass's CLI call (network-boundary fns can't exist on wasm
/// — E0411 gates them — so it's inert either way).
pub fn compile_to_object_wasm_threaded(
    program: &Program,
    output_path: &str,
    ownership: Option<&OwnershipCheckResult>,
    concurrency: Option<&ConcurrencyAnalysis>,
    source_filename: Option<&str>,
    source_text: Option<&str>,
    release: bool,
) -> Result<(), String> {
    let context = Context::create();
    let target_machine = driver::create_target_machine_threaded()?;
    let mut cg = Codegen::new(&context, "karac_module");
    cg.module.set_triple(&target_machine.get_triple());
    cg.module
        .set_data_layout(&target_machine.get_target_data().get_data_layout());
    cg.load_rc_fallback(ownership);
    cg.load_concurrency_analysis(concurrency);
    cg.set_source_filename(source_filename);
    cg.set_source_text(source_text);
    if release {
        cg.set_strip_contracts(true);
        cg.set_strip_error_trace(true);
    }
    cg.set_coro_enabled(true);
    cg.set_wasm_threaded_pass(true);
    cg.compile_program(program)?;

    apply_optimization_passes(&cg.module, &target_machine)?;
    target_machine
        .write_to_file(&cg.module, FileType::Object, Path::new(output_path))
        .map_err(|e| format!("Failed to write object file: {}", e))
}

/// IR-text sibling of [`compile_to_object_wasm_threaded`] for the
/// wasm-target IR pins in `tests/wasm_codegen.rs` (and debugging): same
/// threaded-pass configuration, returns the textual IR instead of
/// writing an object. The threaded-pass selection is parameter-passed
/// (a `Codegen` setter), never a process-global — so this can share a
/// test binary with sequential-pass pins without racing them.
pub fn compile_to_ir_wasm_threaded(
    program: &Program,
    ownership: Option<&OwnershipCheckResult>,
    concurrency: Option<&ConcurrencyAnalysis>,
) -> Result<String, String> {
    let context = Context::create();
    let target_machine = driver::create_target_machine_threaded()?;
    let mut cg = Codegen::new(&context, "karac_module");
    cg.module.set_triple(&target_machine.get_triple());
    cg.module
        .set_data_layout(&target_machine.get_target_data().get_data_layout());
    cg.load_rc_fallback(ownership);
    cg.load_concurrency_analysis(concurrency);
    cg.set_wasm_threaded_pass(true);
    cg.compile_program(program)?;
    Ok(cg.module.print_to_string().to_string())
}

/// Phase-7 L558 sub-step (a): MCJIT sanity-check prototype.
///
/// Compile `program` through the existing codegen pipeline, load the
/// resulting module into inkwell's MCJIT `ExecutionEngine`, look up
/// `main`, and invoke it. Returns the i32 exit code that the user's
/// `main` produced (LLVM-side `main` always returns i32 per
/// `functions.rs:61`).
///
/// **Throwaway prototype, not a shipping vehicle.** The orc2/LLJIT
/// wrap lives at phase-7 L560 and that entry's W1–W6 milestones are
/// what eventually backs `karac repl` / `karac test`. This entry
/// only validates that LLVM-JIT round-trips a Kāra module at all
/// before the orc2 effort starts.
#[cfg(feature = "mcjit_prototype")]
pub fn jit_run_main(
    program: &Program,
    ownership: Option<&OwnershipCheckResult>,
    concurrency: Option<&ConcurrencyAnalysis>,
) -> Result<i32, String> {
    use inkwell::targets::{InitializationConfig, Target};
    use inkwell::OptimizationLevel;

    Target::initialize_native(&InitializationConfig::default())
        .map_err(|e| format!("Failed to initialize native target: {}", e))?;

    let context = Context::create();
    let mut cg = Codegen::new(&context, "karac_module");
    cg.load_rc_fallback(ownership);
    cg.load_concurrency_analysis(concurrency);
    cg.compile_program(program)?;

    let engine = cg
        .module
        .create_jit_execution_engine(OptimizationLevel::None)
        .map_err(|e| format!("Failed to create JIT engine: {}", e))?;

    // Slice a.2 — explicit symbol bindings for libc functions our IR
    // calls. MCJIT on macOS arm64 (LLVM 18 + inkwell 0.9) fails to
    // resolve external symbols via the default RTDyldMemoryManager;
    // unresolved calls land at PC=0 and the thread hangs (sample
    // confirms — see wip-always-jit.md § slice (a) close-out).
    // `add_global_mapping` bypasses RTDyld's lookup by binding LLVM
    // FunctionValues to in-process addresses directly.
    jit_bind_libc_symbols(&engine, &cg.module);

    // LLVM `main` signature is `i32 ()` — see `functions.rs:61`.
    //
    // Bypass `engine.get_function`'s `JitFunction` wrapper — its
    // `transmute_copy` path appears to lose the address on this inkwell
    // 0.9 / LLVM 18 / arm64-darwin combination (jit_probe_main_address
    // shows `get_function_address` returns a valid `0x...`, but calling
    // through `JitFunction.call()` lands at PC=0). Direct transmute of
    // the usize → fn pointer skips the wrapper.
    // Bypass `engine.get_function`'s `JitFunction` wrapper. On
    // macOS arm64 / inkwell 0.9 / LLVM 18, modules that call any
    // external symbol (malloc, free, printf, runtime fns) JIT to a
    // valid `get_function_address` but the JITted code jumps to PC=0
    // shortly after entry — sample-confirmed; see wip-always-jit.md
    // § slice (a) close-out. Pure-internal modules (arithmetic, control
    // flow, helper-fn calls) run correctly through this path.
    let addr = engine
        .get_function_address("main")
        .map_err(|e| format!("Failed to look up main: {}", e))?;
    type MainFn = unsafe extern "C" fn() -> i32;
    let main_fn: MainFn = unsafe { std::mem::transmute(addr) };
    let result = unsafe { main_fn() };
    Ok(result)
}

/// Phase-7 L560 W1: orc2/LLJIT round-trip of a Kāra program.
///
/// Same compiler pipeline as `compile_to_ir`, but instead of returning
/// the IR text, parses it back into an LLJIT-owned `Module`, looks up
/// `main`, and invokes it. Returns the i32 exit code that the user's
/// `main` produced.
///
/// W1 acceptance criterion (per L558 (a) finding): must round-trip a
/// `printf` call on macOS arm64. If this entry hangs on a printf-bearing
/// test, halt and revisit the v2 Cranelift question before W2+.
#[cfg(feature = "lljit_prototype")]
pub fn jit_run_main_lljit(
    program: &Program,
    ownership: Option<&OwnershipCheckResult>,
    concurrency: Option<&ConcurrencyAnalysis>,
) -> Result<i32, String> {
    let ir = compile_to_ir(program, ownership, concurrency)?;
    let engine = lljit::LLJITEngine::new()?;
    engine.add_ir_module(&ir)?;
    let addr = engine.lookup_address("main")?;
    // LLVM `main` signature is `i32 ()` — see `functions.rs:61`.
    type MainFn = unsafe extern "C" fn() -> i32;
    let main_fn: MainFn = unsafe { std::mem::transmute(addr as usize) };
    let result = unsafe { main_fn() };
    Ok(result)
}

/// Bind every libc symbol that karac codegen may emit `declare` lines for
/// to its in-process address. Skips any symbol the current module doesn't
/// reference — `module.get_function(name)` returns `None` for symbols not
/// declared, and we just move on.
#[cfg(feature = "mcjit_prototype")]
fn jit_bind_libc_symbols(
    engine: &inkwell::execution_engine::ExecutionEngine<'_>,
    module: &Module<'_>,
) {
    extern "C" {
        fn malloc(size: usize) -> *mut std::ffi::c_void;
        fn free(ptr: *mut std::ffi::c_void);
        fn printf(fmt: *const i8, ...) -> i32;
        fn snprintf(s: *mut i8, n: usize, fmt: *const i8, ...) -> i32;
        fn strlen(s: *const i8) -> usize;
        fn memcmp(a: *const std::ffi::c_void, b: *const std::ffi::c_void, n: usize) -> i32;
        fn strcmp(a: *const i8, b: *const i8) -> i32;
        fn exit(status: i32);
    }
    let bindings: &[(&str, usize)] = &[
        ("malloc", malloc as *const () as usize),
        ("free", free as *const () as usize),
        ("printf", printf as *const () as usize),
        ("snprintf", snprintf as *const () as usize),
        ("strlen", strlen as *const () as usize),
        ("memcmp", memcmp as *const () as usize),
        ("strcmp", strcmp as *const () as usize),
        ("exit", exit as *const () as usize),
    ];
    for (name, addr) in bindings {
        if let Some(fv) = module.get_function(name) {
            engine.add_global_mapping(&fv, *addr);
        }
    }
}

// ── Codegen ────────────────────────────────────────────────────

pub(super) struct Codegen<'ctx> {
    pub(crate) context: &'ctx Context,
    pub(crate) module: Module<'ctx>,
    pub(crate) builder: Builder<'ctx>,
    /// Maps variable name → (alloca pointer, value type).
    pub(crate) variables: HashMap<String, VarSlot<'ctx>>,
    /// Maps variable name → Kāra type name (for struct/enum field resolution).
    pub(crate) var_type_names: HashMap<String, String>,
    /// Per-element type names of a let-bound TUPLE binding (`let t = (i, Inner
    /// { .. })` → `[None, Some("Inner")]`), so a struct-field access through a
    /// tuple element (`t.1.name`) can resolve the element's struct type.
    /// `type_name_of_expr` is structural (the parser shares spans across
    /// chained postfix, so a span-keyed expr-type lookup can't distinguish
    /// `t` / `t.1` / `t.1.name`); this records the element types at the
    /// binding site from the annotation or the RHS tuple literal
    /// (B-2026-06-11-6). `None` for a non-struct element (primitive / nested
    /// tuple / unresolved RHS — those don't field-access into a struct).
    pub(crate) tuple_var_elem_type_names: HashMap<String, Vec<Option<String>>>,
    /// Names of `Option[T]` bindings that registered a
    /// `CleanupAction::FreeInlineOptionPayload` (T is an inline heap
    /// `String`/`Vec`). A `match`/`if let` arm that binds the `Some`
    /// payload out of such a variable must zero the variable's `cap` word
    /// (option field 3) so the scope-exit free skips — the bound payload's
    /// own cleanup frees it once. Without this gate the suppression can't
    /// tell `Option[String]` (cap at w2, must suppress) from `Option[i64]`
    /// (no heap payload, nothing to suppress): the `Option` layout is
    /// type-erased. See B-2026-06-10-6.
    pub(crate) inline_option_payload_vars: std::collections::HashSet<String>,
    /// `Result[T, E]` sibling of `inline_option_payload_vars` — names of
    /// `Result` bindings that registered a `FreeInlineResultPayload` (the Ok
    /// and/or Err half is an inline heap `String`/`Vec`). A `match`/`if let`
    /// arm binding the `Ok`/`Err` payload out zeros the variable's `cap`
    /// word so the scope-exit free skips (the bound payload frees it once).
    /// See B-2026-06-10-6's Result follow-on.
    pub(crate) inline_result_payload_vars: std::collections::HashSet<String>,
    /// `Option[Map]`/`Option[Set]` sibling — names of `Option` bindings that
    /// registered a `FreeInlineOptionMapPayload`. A `match`/`if let` arm
    /// binding the `Some` payload out sets the source tag to `None` (no `cap`
    /// word to zero, unlike the Vec case) so the scope-exit free skips. See
    /// B-2026-06-10-6's `Option[Map]` follow-on.
    pub(crate) inline_option_map_payload_vars: std::collections::HashSet<String>,
    /// Refinement type alias name → its base `TypeExpr` (`type Email =
    /// String where …` → the `String` type expr). Populated from the
    /// program's `Item::TypeAlias`es that carry a `where` predicate.
    /// Consulted by `llvm_type_for_type_expr` / `llvm_type_for_name` so a
    /// refinement lowers to its *base*'s layout — without this a refinement
    /// over a non-`i64` base would hit the `i64` fall-through default and
    /// silently mis-size the slot (phase-9 step 4). A refinement is
    /// layout-identical to its base (no runtime tag), so this is a pure
    /// alias resolution.
    pub(crate) refinement_bases: HashMap<String, crate::ast::TypeExpr>,
    /// Refinement type alias name → the ordered names of its generic
    /// parameters (`type NonEmpty[T] = Vec[T] where …` → `["T"]`). Parallel
    /// to `refinement_bases`, which stores only the *uninstantiated* base
    /// (`Vec[T]`). When a refinement alias is used at a concrete arity
    /// (`NonEmpty[EnrichedRow]`), `resolve_refinement_alias_te` zips these
    /// param names against the use-site generic args and substitutes them
    /// into the base so the binding registers as `Vec[EnrichedRow]` (correct
    /// element type), not `Vec[T]` (which would mis-size the element as the
    /// `i64` unknown-name fall-through). Empty for non-generic refinements.
    pub(crate) refinement_generic_params: HashMap<String, Vec<String>>,
    /// Distinct-type name → its base `TypeExpr` (`distinct type UserId = i64`
    /// → the `i64` type expression). A distinct type is layout-identical to
    /// its base (zero-cost wrapper, no runtime tag), so codegen lowers it to
    /// the base's LLVM layout — consulted ONLY at the pure-layout sites
    /// (`llvm_type_for_type_expr`, `llvm_type_for_name`), NOT in
    /// `refinement_base_name`: unlike a refinement, a distinct type keeps its
    /// own name for value-level method dispatch (no base-method deref).
    /// Populated from `Item::DistinctType`. design.md § Distinct Types.
    pub(crate) distinct_bases: HashMap<String, crate::ast::TypeExpr>,
    /// Refinement name → its predicate `Expr` (`type Even = i64 where
    /// self % 2 == 0` → the `self % 2 == 0` expression). Populated from
    /// `Item::TypeAlias.refinement`, parallel to `refinement_bases`. Drives
    /// the runtime predicate check emitted at `x as Refined` cast sites and
    /// `Refined.try_from(x)` calls (phase-9 step 5c): the predicate is
    /// compiled with `self` bound to the candidate value, then branched on.
    pub(crate) refinement_predicates: HashMap<String, crate::ast::Expr>,
    /// The `ensures` clauses of the function currently being compiled
    /// (design.md § Contracts). Set at `compile_function` entry, cleared at
    /// exit; consumed by `emit_ensures_checks`, which is emitted inline
    /// before each `ret` (the tail return + every explicit `return`).
    pub(crate) current_contract_ensures: Vec<crate::ast::EnsuresClause>,
    /// The return `TypeExpr` of the function currently being compiled, set
    /// alongside `current_contract_ensures`. `emit_ensures_checks` uses it to
    /// register the `result` binding's type (via `register_var_from_type_expr`)
    /// so a `result.field` access inside an `ensures` clause resolves the
    /// struct field index — without it, field access on `result` can't find
    /// the struct name and reads the wrong slot (the `ensures(result)
    /// result.q == old(...)` codegen bug surfaced by the Weave dogfood).
    /// `None` for a `()`-returning function or when contracts are stripped.
    pub(crate) current_contract_result_type: Option<crate::ast::TypeExpr>,
    /// `old(arg)` pre-state snapshots for the current function, captured at
    /// entry and keyed by the arg expression's span. Read back by the
    /// `old(...)` interception in `compile_call` when emitting the
    /// postcondition (design.md § Contracts rule 4).
    pub(crate) contract_old_snapshots: HashMap<SpanKey, inkwell::values::BasicValueEnum<'ctx>>,
    /// Struct/impl `invariant` predicates that must hold at every exit of the
    /// method currently being compiled (design.md § Contracts rule 3). Set at
    /// `compile_function` entry for impl-method functions — `impl invariant`
    /// always, plain `invariant` only when the method is `pub` — and cleared at
    /// exit. Consumed by `emit_invariant_checks`, emitted inline before each
    /// `ret` (same exit points as `ensures`), with `self` already bound as the
    /// method's first parameter. Empty for free functions and non-pub methods
    /// of invariant-free structs.
    pub(crate) current_method_invariants: Vec<crate::ast::Expr>,
    /// `Some(type_name)` when the function currently being compiled is a
    /// *constructor* — a `pub` associated function returning `Self`/the type,
    /// which has no `self` parameter (design.md § Contracts: "Constructors ...
    /// also check the invariant at their return point"). When set,
    /// `emit_invariant_checks` binds the *return value* as `self` before
    /// evaluating `current_method_invariants`, since the construction boundary
    /// has no receiver. `None` for methods (where `self` is parameter 0) and
    /// for free / non-constructor associated functions. Set at
    /// `compile_function` entry, cleared at exit.
    pub(crate) constructor_invariant_self_type: Option<String>,
    /// When `true`, all contract machinery is elided from the emitted module
    /// (design.md § Contracts: "stripped in release"): `requires` / `ensures`
    /// checks, `old(...)` pre-state capture, and struct/impl `invariant`
    /// checks are not emitted, paying zero runtime cost. Defaults from
    /// `read_strip_contracts_env` (`KARAC_STRIP_CONTRACTS`) at construction;
    /// `set_strip_contracts` overrides it (used by the release-build path and
    /// by IR tests that must force the decision without touching global env).
    /// The gate lives at the three contract *setup* sites in
    /// `compile_function` — suppressing setup makes every downstream emit site
    /// a natural no-op, and `old(...)` (which lives only inside `ensures`
    /// bodies) is never reached because those bodies aren't compiled.
    pub(crate) strip_contracts: bool,
    /// When `true`, the `?`-error-return-trace instrumentation is elided: no
    /// `karac_error_trace_push` at `?` failure sites, no `karac_error_trace_clear`
    /// on the success path. The trace is a debug-only diagnostic, so a release
    /// build pays zero `?`-site cost (peer to `strip_contracts`). Defaults from
    /// `read_strip_error_trace_env` (`KARAC_STRIP_ERROR_TRACE`) at construction;
    /// `set_strip_error_trace` overrides it (the `release` build path forces it
    /// on alongside contract stripping). The gate lives at the two emission
    /// sites in `compile_expr`'s `?` lowering.
    pub(crate) strip_error_trace: bool,
    /// Runtime contract-predicate-context FFI (design.md § Contracts rule 2).
    /// `emit_contract_assert` brackets a predicate's *runtime* evaluation with
    /// `karac_runtime_enter_predicate()` / `karac_runtime_exit_predicate()` (a
    /// thread-local depth counter in the runtime), and `emit_panic` reads
    /// `karac_runtime_panic_prefix()` to choose its fault category. A panic that
    /// fires while the depth is non-zero — whether an inline bounds/div/unwrap
    /// check lexically inside the predicate (`requires v[i] >= 0`) OR a panic
    /// inside a function the predicate transitively *calls* — is the distinct
    /// `contract predicate panicked: <msg>` fault, not `contract violated`
    /// (reserved for the predicate evaluating to `false`, where the depth is
    /// back to 0). The runtime flag subsumes the prior compile-time flag: it
    /// sees cross-call panics a lexical flag cannot, matching the interpreter's
    /// global `pending_cf` behavior. The depth is a counter, not a bool, so a
    /// predicate that calls a function with its own contract nests correctly.
    pub(crate) karac_runtime_enter_predicate_fn: FunctionValue<'ctx>,
    pub(crate) karac_runtime_exit_predicate_fn: FunctionValue<'ctx>,
    pub(crate) karac_runtime_panic_prefix_fn: FunctionValue<'ctx>,
    /// Whether `emit_panic` must read the fault-category prefix from the
    /// runtime (`karac_runtime_panic_prefix()`) rather than folding it to the
    /// static `""`. Set at the top of `compile_program`: `true` when the
    /// program declares any contract (`requires` / `ensures` / `invariant`,
    /// scanned across free fns, impl methods, trait methods, and struct
    /// invariants by `program_declares_contracts`) and contracts aren't
    /// stripped, or when compiling a REPL cell module (`main_symbol_override`
    /// set — a cell can call contracted functions JIT'd from earlier cells,
    /// which this module's item scan can't see; per-test `main` modules ride
    /// the same entry point and signal). When `false`, no predicate bracket
    /// can ever run in-process, the depth counter is statically 0, and the
    /// prefix is always `""` — `emit_panic` skips the runtime call, so (a)
    /// the `karac_runtime_panic_prefix` symbol and the writable thread-local
    /// `__DATA` page it drags into the link dead-strip from contract-free
    /// binaries (+16 KiB per binary), and (b) panic landing pads stay
    /// static-string leaves instead of blocks with a live call (the
    /// unconditional call regressed a bounds-check-hot loop 1.34× —
    /// kata-5 longest-palindromic-substring, 2026-06-05). Defaults `true`
    /// (conservative: any path that bypasses `compile_program` keeps the
    /// always-correct runtime read).
    pub(crate) runtime_panic_prefix_needed: bool,
    /// Monotonic counter naming the per-site outlined panic bodies
    /// (`__karac_panic_site_<n>`) `emit_panic` creates — see its doc for why
    /// panic bodies are outlined. `Cell` because `emit_panic` is `&self`.
    pub(crate) panic_site_counter: std::cell::Cell<u32>,
    /// Set of top-level Atomic[T]-typed bindings whose inner T is `bool`.
    /// The slot itself is widened to `i8` (LLVM atomics reject `i1`); this
    /// set drives the `.load` trunc-to-i1 and `.store` zext-to-i8 wrapping
    /// in `compile_atomic_method`. Populated at let-stmt time when either
    /// (a) the explicit annotation is `Atomic[bool]`, or (b) the RHS is
    /// `Atomic.new(<bool literal>)`. The struct-field equivalent is read
    /// from `struct_field_type_exprs` at the FieldAccess receiver site —
    /// fields always have full annotations recorded there, so no parallel
    /// table is needed for the struct case.
    pub(crate) atomic_var_inner_is_bool: HashSet<String>,
    /// A2 slice 2b.3 gate. When `true`, network-boundary functions (keys in
    /// `coro_fn_keys`) compile as LLVM coroutines (ramp + `coro.suspend` parks +
    /// dispatcher-driven slot-wait drive) instead of the degenerate
    /// `emit_state_machine_poll_fn_for_key` body-splitter. Default `false` (set
    /// via [`Codegen::set_coro_enabled`]) so the existing poll-fn / drive tests
    /// stay green; the new coroutine path is opt-in until the flip-the-default +
    /// delete-degenerate-path slice. See
    /// docs/spikes/network-async-coroutine-transform.md § 6¾.
    pub(crate) coro_enabled: bool,
    /// The network-boundary function keys compiled as coroutines this run
    /// (populated from `program.state_struct_layouts`, minus generics, only when
    /// `coro_enabled`). Read by `declare_function` (→ `ptr` return type),
    /// `emit_state_machine_poll_fns` (→ skip the degenerate poll-fn), and the
    /// call-site intercepts (→ slot-wait drive instead of the poll-loop).
    pub(crate) coro_fn_keys: HashSet<String>,
    /// Set by `emit_coro_ramp` for the duration of a coroutine-compiled
    /// function's body emission; consulted by the tcp.rs leaf-park branch and
    /// the body-return routing; drained (`None`) at the top of every
    /// `compile_function`. `Some` ⇒ "currently emitting inside a coroutine".
    pub(crate) coro_ctx: Option<coro::CoroContext<'ctx>>,
    /// Per-coroutine-function counter for unique park resume-block names; reset
    /// by `emit_coro_ramp`, bumped by each `emit_coro_park_suspend`.
    pub(crate) coro_park_counter: u32,
    /// A2 slice 5a — non-blocking spawn drive. Set to `Some(slot)` only while
    /// compiling a `__spawn_coro_wrap_N` wrapper body (task_group.rs): the
    /// `is_coroutine_compiled` call-site intercept then emits `ramp(args,
    /// slot)` and returns **without** `park_slot_new`/`wait`/`free` — the
    /// runtime owns the slot and binds it to the `TaskHandle`, so the wrapper
    /// ramps and returns, freeing the worker. `None` (the default) is the
    /// inline blocking drive (allocate slot, ramp, wait, free).
    pub(crate) coro_spawn_slot: Option<PointerValue<'ctx>>,
    pub(crate) current_fn: Option<FunctionValue<'ctx>>,
    pub(crate) printf_fn: FunctionValue<'ctx>,
    /// `int snprintf(char* buf, size_t n, const char* fmt, ...)` — used by f-string
    /// codegen to convert integers and floats to their decimal string forms.
    pub(crate) snprintf_fn: FunctionValue<'ctx>,
    /// `size_t fwrite(const void* ptr, size_t size, size_t nmemb, FILE* stream)` —
    /// the NUL-safe string-print primitive (L5). Unlike `printf("%.*s")`, which
    /// stops at the first interior NUL even with a precision, `fwrite` writes
    /// exactly `len` bytes. It shares libc's stdio buffer with the `printf`
    /// int/bool paths, so output ordering across mixed prints is preserved.
    pub(crate) fwrite_fn: FunctionValue<'ctx>,
    /// The libc `FILE*` globals for stdout / stderr, used as the `fwrite`
    /// stream argument. The symbol name is platform-specific (`__stdoutp` /
    /// `__stderrp` on Apple, `stdout` / `stderr` elsewhere, incl. wasi-libc).
    pub(crate) stdout_global: inkwell::values::GlobalValue<'ctx>,
    pub(crate) stderr_global: inkwell::values::GlobalValue<'ctx>,
    /// LLVM struct types for Kāra structs (struct name → LLVM type).
    pub(crate) struct_types: HashMap<String, StructType<'ctx>>,
    /// State-struct LLVM types for the network-event-loop state-machine
    /// transform (phase 6 line 26). Key: network-boundary function key
    /// (`name` for free fns, `Type.method` for impl methods — same shape
    /// as `Program.state_struct_layouts`). Value: `%kara.state.<fn_key>`
    /// LLVM struct type with field 0 = `i32` yield-point tag, fields 1..n
    /// = one slot per captured local from the function's `StateStructLayout`
    /// (sized via the typechecker-recorded `type_name` through the
    /// existing `llvm_type_for_name` API; `None` type names fall back to
    /// `i64`). Populated by `emit_state_struct_types` immediately after
    /// `declare_enums`, before any function-body lowering — so the
    /// slice-6+ state-machine transform passes can look up the struct
    /// type at body-rewrite time. Empty when no network-boundary
    /// functions exist (the common case for non-network programs).
    pub(crate) state_struct_types: HashMap<String, StructType<'ctx>>,
    /// State-machine poll functions for the network-event-loop transform
    /// (phase 6 line 26 slice 6). Key: same function key shape as
    /// `state_struct_types` (`name` / `Type.method`). Value:
    /// `define internal i8 @__kara_poll_<fn_key>(ptr %state, ptr %cancel)`
    /// FunctionValue carrying the poll-fn ABI per `KaracParkedTask.poll_fn`
    /// (state-struct pointer + cancel `AtomicBool` pointer; returns the
    /// `KaracPollResult` discriminant `0=Pending / 1=Ready / 2=Err`).
    /// Slice 6 ships only the **stub body** (loads the tag via GEP into
    /// the state struct's field 0, unconditionally returns Pending) —
    /// the actual switch-on-tag dispatch + per-yield-arm lowering land
    /// in subsequent sub-slices. The stub already makes the ABI concrete
    /// in the IR so caller-side allocate-state-struct-then-invoke-poll
    /// work in slice 7+ can wire against a stable signature. Populated
    /// by `emit_state_machine_poll_fns` immediately after
    /// `emit_state_struct_types`. Empty when no network-boundary
    /// functions exist.
    pub(crate) state_machine_poll_fns: HashMap<String, FunctionValue<'ctx>>,
    /// State-struct constructor helpers (phase 6 line 26 slice 8c). Key:
    /// same function key shape as `state_struct_types`. Value:
    /// `define internal ptr @__kara_state_new_<fn_key>()` — a no-arg
    /// helper that `malloc`s a fresh state struct of the right size,
    /// initializes the i32 yield-point tag (field 0) to 0 so the next
    /// poll-fn invocation routes to the entry arm `state_0`, and
    /// returns the heap pointer. Slice 8d's caller-side wiring replaces
    /// each direct call to a network-boundary fn with a call to this
    /// constructor followed by an initial poll-fn invocation; future
    /// slices add the loop-until-Ready and the `free` of the state
    /// struct when the caller observes Ready/Err. Populated by
    /// `emit_state_machine_state_constructors` after the poll-fn pass.
    /// Empty when no network-boundary functions exist.
    pub(crate) state_machine_state_constructors: HashMap<String, FunctionValue<'ctx>>,
    /// State-struct destructor helpers (phase 6 line 26 slice 8u). Key:
    /// same function key shape as `state_struct_types`. Value:
    /// `define internal void @__kara_state_drop_<fn_key>(ptr %state)` —
    /// walks the captured-local fields and frees any heap-bearing ones
    /// (Vec/String `cap > 0 ? free(data)` pattern; shared-struct fields
    /// `emit_refcount_dec` against the slot's loaded handle). The
    /// state struct's own heap allocation is the *caller's* responsibility
    /// to `free` after invoking the destructor — matches the constructor's
    /// caller-allocates / caller-frees discipline (slice 8c). Slice 8u
    /// ships the destructor as the unified unwind primitive both the
    /// future `?`-error-propagation path (post-yield arm prologue when
    /// the resumed call returned `Err`) and the cooperative-cancel path
    /// (poll-fn's per-arm `*cancel == true` check) will share — neither
    /// use site lands in slice 8u itself. Empty when no network-boundary
    /// functions exist; also empty when none of the captured-local fields
    /// has a heap-bearing type (the destructor would have an empty body
    /// — skipped to avoid IR bloat).
    pub(crate) state_machine_state_destructors: HashMap<String, FunctionValue<'ctx>>,
    /// Slice 8v Phase 2: cached `Program` snapshot used by the per-mono
    /// state-machine emission path. `compile_generic_call` (in
    /// `src/codegen/mono.rs`) needs access to the polymorphic
    /// function's state-struct layout, yield points, and AST body to
    /// emit the four per-mono state-machine helpers (state-struct
    /// LLVM type, poll-fn, constructor, destructor) at the mangled
    /// key. The existing call-site plumbing for `compile_generic_call`
    /// doesn't thread `&Program` through `compile_expr` →
    /// `compile_call` → `compile_generic_call`, so we snapshot a full
    /// `Rc<Program>` clone at the top of `compile_program` and
    /// dispense cheap `Rc` clones to per-mono callers as they fire.
    /// The wrapping `Rc` avoids borrow-conflict pain when the per-mono
    /// path needs both `&self` (for the snapshot read) and `&mut self`
    /// (for the per-key emission helpers). Always populated for the
    /// duration of `compile_program`; left `None` outside that scope.
    pub(crate) program_snapshot: Option<Rc<Program>>,
    /// Non-unit return-type marker for network-boundary functions
    /// (phase 6 line 26 slice 8i). Key: same function key shape as
    /// `state_struct_types`. Value: the LLVM type of the function's
    /// return value (slice-8i v1 records `i64` only; unit-returning
    /// fns have no entry; other types are deferred to a follow-on
    /// slice and also have no entry). When an entry exists:
    /// - The state struct (slice 5) gains a terminal field of this
    ///   type appended after the captured-local fields;
    /// - The poll-fn's terminal arm (slice 8b) writes a placeholder
    ///   into the terminal field ahead of returning Ready (the
    ///   actual user-level return-expression value lands later when
    ///   body-splitting completes for non-trivial bodies);
    /// - Caller-side intercepts (slices 8d / 8g) load the terminal
    ///   field after the `kara.poll_done` block and use the loaded
    ///   value as the call's return value, replacing the unconditional
    ///   `i64 0` from earlier slices.
    ///
    /// Absent entries preserve the v1 unit-return behavior.
    pub(crate) state_machine_return_types: HashMap<String, BasicTypeEnum<'ctx>>,
    /// Field names in declaration order (struct name → field names).
    pub(crate) struct_field_names: HashMap<String, Vec<String>>,
    /// Field type-names in declaration order (struct name → per-field
    /// user-type name, or `None` if the field's declared type isn't a
    /// path / isn't a known user struct). Used to recover the inner
    /// type of chained field accesses (`o.inner.name` requires knowing
    /// the type of `o.inner` to resolve `name`'s field index in
    /// `compile_field_access` / `field_index_for`).
    pub(crate) struct_field_type_names: HashMap<String, Vec<Option<String>>>,
    /// Full per-field `TypeExpr` in declaration order (struct name →
    /// field TypeExprs). Carries the generic args that
    /// `struct_field_type_names` discards (`Vec[Node]` vs just `"Vec"`),
    /// which the field-receiver method dispatch path needs to populate
    /// the synth's element-type side-tables via
    /// `register_var_from_type_expr`. Populated alongside
    /// `struct_field_type_names` in `declare_structs`.
    pub(crate) struct_field_type_exprs: HashMap<String, Vec<crate::ast::TypeExpr>>,
    /// FFI union storage types (union name → LLVM struct type used as
    /// the storage blob). Phase 5 slice 4. The storage struct is sized
    /// to `max(field_sizes)` and aligned to `max(field_aligns)` per the
    /// `#[repr(C)] union Foo { ... }` lowering rule: its single LLVM
    /// field is the union-field with the largest alignment (tie-break
    /// preferring the largest size), followed by a `[k x i8]` padding
    /// tail when that field's size is smaller than the full union size.
    /// Populated by `declare_unions` after `declare_structs`. Read by
    /// `llvm_type_for_name` (so `size_of[Foo]` / `align_of[Foo]` work
    /// for free) and by the union-literal / union-field-access codegen
    /// in `compile_struct_init` / `compile_field_access`.
    pub(crate) union_types: HashMap<String, inkwell::types::StructType<'ctx>>,
    /// Per-union field declarations in source order (union name →
    /// (field_name, field_llvm_type)). Used by union-literal codegen
    /// to look up the destination LLVM type when storing through the
    /// alloca, and by union-field-access codegen to bitcast the read
    /// pointer to the field's LLVM type before loading. Populated
    /// alongside `union_types`.
    pub(crate) union_field_types: HashMap<String, Vec<(String, BasicTypeEnum<'ctx>)>>,
    /// Enum layouts for tagged-union codegen (enum name → layout).
    pub(crate) enum_layouts: HashMap<String, EnumLayout<'ctx>>,
    /// All-unit (no payload), non-shared user enums → variant names in tag
    /// order. Drives codegen `Display` for enums (subtask 5): such an enum
    /// renders as the bare variant name, selected on the tag. Payload-bearing
    /// enums are absent (their Display codegen is a tracked follow-on).
    pub(crate) enum_unit_variants: HashMap<String, Vec<String>>,
    /// Names of enums seeded by `seed_builtin_enum_layouts` (`Option`,
    /// `Result`, `Json`, `TcpError`, …) — used by the variant-name →
    /// enum-name disambiguation in `try_compile_enum_variant` /
    /// `infer_enum_from_value` to prefer user-declared enums when a
    /// variant name appears in both. Without this set, HashMap iteration
    /// order non-deterministically picks a seeded layout for a
    /// user-defined variant with the same name (e.g. `MyIoErr.Other`
    /// vs `TcpError.Other`), producing a wrong-shape value at the
    /// constructor site and emitting `unreachable` for downstream
    /// dispatch — surfaced 2026-05-25 by the codegen suite's
    /// intermittent hang investigation.
    pub(crate) seeded_enum_names: HashSet<String>,
    /// Nested loop stack — innermost frame is last.
    pub(crate) loop_stack: Vec<LoopFrame<'ctx>>,
    // ── Generic monomorphization ──────────────────────────────────
    /// Generic function AST nodes keyed by name. Not compiled until instantiated.
    pub(crate) generic_fns: HashMap<String, Function>,
    /// Already-generated monomorphizations (mangled name → done). Prevents duplicate codegen.
    pub(crate) generated_monos: HashSet<String>,
    /// Active type-parameter substitution during a monomorphization pass.
    /// Maps generic param name (e.g. `"T"`) → concrete LLVM type.
    pub(crate) type_subst: HashMap<String, BasicTypeEnum<'ctx>>,
    /// Active const-parameter substitution during a monomorphization
    /// pass (const generics slice 4). Maps const-generic param name
    /// (e.g. `"N"`) → its bound `ConstValue`. Used by
    /// `compile_expr ExprKind::Identifier` to lower const-param
    /// references in generic bodies to LLVM constants of the matching
    /// width via `compile_primitive_const`, and by `Array[T, N]`
    /// element-size extraction sites to recover the size from a
    /// const-param reference. Slice 1b populates this map during
    /// `compile_generic_call`'s mango-key mango step; slice 4
    /// extends the save/restore around `compile_mono_function` so the
    /// body lowering sees the same bindings.
    pub(crate) const_subst: HashMap<String, crate::prelude::ConstValue>,
    // ── Closure compilation ────────────────────────────────────────
    /// Monotonic counter used to generate unique closure function names.
    pub(crate) closure_counter: u32,
    /// Monotonic counter for synthesized identifier names emitted by the
    /// indexed-receiver method-dispatch lowering (`__indexed_elem_<n>`).
    /// Each call site that lowers an `Index { object, index }` receiver
    /// allocates one synth name, registers it in the variable + element-type
    /// registries pointing into the outer container's storage, dispatches the
    /// method through the existing identifier path, and cleans up after.
    pub(crate) indexed_elem_counter: u32,
    /// Maps local variable names that hold closure fat-pointers to their LLVM function type.
    /// Required for indirect calls: `build_indirect_call` needs the callee's function type.
    pub(crate) closure_fn_types: HashMap<String, FunctionType<'ctx>>,
    /// Staging slot — set by `compile_closure` so the surrounding `let` binding can record
    /// the function type under the newly bound name.
    pub(crate) pending_closure_fn_type: Option<FunctionType<'ctx>>,
    /// Staging slot — caller-supplied LLVM types for a closure's parameters,
    /// consulted by `compile_closure` when the source has no type annotation
    /// to refine. Used by `Vec.sort_by` to push the element type into
    /// `|a, b|` closures so tuple receivers don't collapse to bare `i64`.
    /// Taken once and cleared on entry to `compile_closure`.
    pub(crate) pending_closure_param_hints: Option<Vec<BasicTypeEnum<'ctx>>>,
    /// Staging slot — set by `compile_stmt`'s Let / Expr arms when the
    /// surrounding statement discards the `Option[V]` result of a
    /// `Map.insert(k, v)` call (i.e. `let _ = m.insert(...)` or a bare
    /// `m.insert(...)` statement). `compile_map_method`'s `insert` arm
    /// reads + clears this flag to decide whether to emit a follow-up
    /// `rc_dec` on the displaced shared value (the `Some(old)` payload
    /// that no one will hold the +1 of). Without the dec the prior
    /// bucket value's refcount stays >0 on every overwrite and the
    /// shared object leaks. When the result *is* bound (`let prev =
    /// m.insert(...)`), the caller's scope-exit cleanup on `prev`
    /// handles the +1; the discard path is the only one that needs
    /// the receive-site dec.
    pub(crate) pending_map_insert_old_dec: bool,
    /// Staging slot — set by `compile_expr`'s `InterpolatedStringLit` arm
    /// to the f-string's accumulator alloca. The Let / Assign handlers
    /// consume it when the RHS is an f-string AND the LHS is a tracked
    /// Vec/String slot, zeroing the acc's `cap` so its scope-exit
    /// `FreeVecBuffer` cleanup no-ops — the buffer is now owned by the
    /// LHS slot (whose own queued cleanup will free it once). Without
    /// this transfer both cleanups fire on the same heap pointer and
    /// macOS malloc's double-free path hangs in `malloc_printf`.
    /// Symmetric to `suppress_source_vec_cleanup_for_arg`'s
    /// cap-zeroing for Identifier RHS aliasing.
    pub(crate) last_fstr_acc: Option<PointerValue<'ctx>>,
    // ── Shared types (RC) ─────────────────────────────────────────
    /// Shared type metadata (struct/enum name → heap layout info).
    pub(crate) shared_types: HashMap<String, SharedTypeInfo<'ctx>>,
    /// malloc function for heap allocation.
    pub(crate) malloc_fn: FunctionValue<'ctx>,
    /// `karac_alloc_fallible(size) -> ptr` — non-null on success, null on OOM
    /// (phase-8-stdlib-floor item 8). The `try_*` collection companions call
    /// this and branch on null to build `Result.Err(AllocError)`.
    pub(crate) alloc_fallible_fn: FunctionValue<'ctx>,
    /// `karac_alloc_or_panic(size) -> ptr` — the panicking counterpart that
    /// aborts on OOM instead of returning null. The panicking collection
    /// methods (`Vec.with_capacity`, `Vec.from_slice`, grow paths) route
    /// through it so OOM is a clean abort, not a null-deref segfault.
    pub(crate) alloc_or_panic_fn: FunctionValue<'ctx>,
    /// free function for heap deallocation.
    pub(crate) free_fn: FunctionValue<'ctx>,
    /// exit function for runtime panics.
    pub(crate) exit_fn: FunctionValue<'ctx>,
    /// memcmp for string comparison.
    pub(crate) memcmp_fn: FunctionValue<'ctx>,
    /// `int sched_yield(void)` — POSIX thread-yield primitive. Phase 6
    /// line 26 slice 8e wires this into the caller-side network-boundary
    /// intercept's Pending path so the parent thread cooperatively
    /// yields to the OS scheduler / dispatcher between poll-fn
    /// invocations instead of busy-looping. Linked from libc (same
    /// path as malloc / free). Windows IOCP support (line 17 sub-item 7)
    /// will need a `SwitchToThread` analog; v1 targets Linux / macOS
    /// where sched_yield is available.
    pub(crate) sched_yield_fn: FunctionValue<'ctx>,
    /// Local bindings that alias `vec_var.len()` — populated at let-sites of
    /// the form `let n = v.len()` where `v` is a Vec identifier in scope.
    /// Consulted by the bounds-check-elision pass when parsing while-guard
    /// predicates of form `idx < n`: resolving `n` back to `v.len()` lets
    /// the elision recognize `idx < v.len()` and skip the upper-half of
    /// `compile_vec_index`'s bounds check on a matching `v[idx]` site.
    /// Cleared / replaced as bindings shadow; the simple HashMap shape is
    /// load-bearing because tracked Vec names don't shadow each other in
    /// practice — refine to scope-keyed if a counter-example surfaces.
    pub(crate) len_alias: HashMap<String, String>,
    /// Asserted bounds in the current emission scope — facts established
    /// by a dominating `while`-guard or `for`-range that the bounds-check
    /// emission can rely on. Each entry asserts one half of a Vec-index
    /// safety fact; `compile_vec_index` consults this stack at the
    /// indexing site and elides the matching half of the bounds check.
    /// The stack discipline (push on body-entry, pop on body-exit) maps
    /// directly onto the source-level lexical scope of the guard.
    pub(crate) asserted_index_bounds: Vec<AssertedIndexBound>,
    /// Per-variable Vec element type tracking (variable name → element LLVM type).
    pub(crate) vec_elem_types: HashMap<String, BasicTypeEnum<'ctx>>,
    /// Element type for the let-binding currently being compiled, threaded
    /// through `compile_expr(rhs)` so zero-arg `Vec.with_capacity(n)` can
    /// recover `T` from the annotation. Set just before compiling the let's
    /// RHS, cleared just after. Read by `Vec.with_capacity` in
    /// `compile_assoc_call`.
    pub(crate) pending_let_elem_type: Option<BasicTypeEnum<'ctx>>,
    /// Per-variable Slice element type tracking (variable name → element LLVM type).
    /// Entries only exist for values whose LLVM representation is the
    /// 2-field slice struct `{ptr, i64}`; used to dispatch indexing and
    /// iteration lowering.
    pub(crate) slice_elem_types: HashMap<String, BasicTypeEnum<'ctx>>,
    /// Variables that are ref parameters (name → inner LLVM type for dereferencing).
    pub(crate) ref_params: HashMap<String, BasicTypeEnum<'ctx>>,
    /// Owned (bare `String` / `Vec[T]`, non-ref) parameters of the
    /// function currently being compiled. The call ABI passes these
    /// `{data, len, cap}` headers by value while the CALLER retains the
    /// buffer's scope-exit free (no ownership transfer at the call
    /// boundary today), so any consume site inside the callee that
    /// RETAINS the value beyond the call — `Vec.push(param)`,
    /// `return param` — must deep-copy the buffer instead of aliasing
    /// it. Without the copy, the caller's free leaves the retained
    /// alias dangling (kata-22 backtracking: `out.push(cur)` at the
    /// recursion base case; `fn id(s: String) -> String { s }`).
    /// Cleared per-function alongside `ref_params`.
    pub(crate) owned_vecstr_params: HashSet<String>,
    /// Owned (bare, non-ref) **struct** params with at least one heap
    /// (`Vec`/`String`) field. Same copy-model rationale as
    /// `owned_vecstr_params`, one level in: a by-value struct param is a
    /// shallow copy whose heap-field buffers alias the caller's, but the
    /// caller retains and frees them. So moving a heap field OUT
    /// (`let inner = h.v`) into an owned local that the callee then frees
    /// double-frees against the caller's struct-drop. The let-FieldAccess
    /// lowering deep-copies such a field's buffer so the moved-out local is
    /// independent (B-2026-06-10-2). Cleared per-function alongside
    /// `ref_params`.
    pub(crate) owned_struct_params: HashSet<String>,
    /// SoA layout metadata (layout name → SoaLayout).
    pub(crate) soa_layouts: HashMap<String, SoaLayout>,
    /// Function parameter ref-ness (function name → vec of is_ref per param).
    pub(crate) fn_param_ref: HashMap<String, Vec<bool>>,
    /// `unsafe extern` imports that carry `#[link_name("symbol")]`: maps the
    /// Kāra fn identifier → the foreign symbol it actually binds. The import
    /// is registered in the LLVM module under the *symbol* name, so call
    /// sites must translate the Kāra name through this map before
    /// `module.get_function(...)` (an LLVM function's name *is* its symbol).
    /// Empty unless a program uses `#[link_name]`; the common case keeps the
    /// Kāra name and never touches this map. Lets a snake_case Kāra fn bind a
    /// PascalCase C symbol — the LLVM-C self-hosting binding's requirement
    /// (`docs/spikes/self-hosting-llvm-c-ffi.md`).
    pub(crate) extern_link_names: HashMap<String, String>,
    /// Function parameter slice element type (function name → per-param
    /// Some(elem_ty) if that param is Slice[T] / mut Slice[T], else None).
    /// Used at call sites to emit Array → Slice and Vec → Slice coercions.
    pub(crate) fn_param_slice_elem: HashMap<String, Vec<Option<BasicTypeEnum<'ctx>>>>,
    /// Function return-type name (function name → user-type name of the
    /// declared return type, if it is a bare `Path` to a known struct /
    /// enum). Used by `compile_field_access` to recover the static type
    /// of a call-chain field-access object (`helper().val`) when the
    /// callee returns a shared struct — without this, the field path
    /// falls through to the generic `StructValue` extract and silently
    /// loads `i64 0`. See bug #8 (call-chain field access on
    /// shared-struct return).
    pub(crate) fn_return_type_names: HashMap<String, String>,
    /// Function-name → inner `TypeExpr` of a borrow return (`-> ref T` /
    /// `-> mut ref T` ⇒ inner `T`). Lets the caller learn that a call
    /// result is a borrow so it can bind it as a ref-local (deref on use
    /// via `ref_params`) rather than treating the returned `ptr` as a
    /// value — the caller half of B-2026-06-07-5. Populated by
    /// `declare_function`.
    pub(crate) fn_ref_return_inner: HashMap<String, TypeExpr>,
    /// Function-name → inner-shared-name when the function returns
    /// `Option[shared T]`. Populated by `declare_function` from the
    /// return type's `Option[T]` generic arg when T is a known shared
    /// type. Read by the let-stmt handler's `Option[shared T]`
    /// detection to register an `RcDecOption` cleanup for untyped
    /// bindings whose RHS is a call (`let out = add_two_numbers(...)`).
    /// Closes the kata-bench retention gap (2026-05-17) for the
    /// inferred-annotation shape; the explicit-annotation shape
    /// (`let out: Option[ListNode] = ...`) reads the inner directly
    /// off the surface `TypeExpr`.
    pub(crate) fn_return_option_inner_shared: HashMap<String, String>,
    /// Function-name → full return `TypeExpr`. Populated by
    /// `declare_function`. Read by the let-stmt handler's oversized-enum
    /// boxing path (`boxed_enum_payload_variants`) for an *untyped* let whose
    /// RHS is a direct call (`let o = make_opt()`): the box drop needs the
    /// generic arg `T` of `Option[T]` / `Result[T, E]` to decide boxing and
    /// name the inner struct, which `fn_return_type_names` (bare segment only)
    /// can't supply. The annotated shape reads `T` off the `let`'s `ty`.
    /// docs/spikes/oversized-enum-payload.md §3.
    pub(crate) fn_return_type_exprs: HashMap<String, TypeExpr>,
    /// Niche-ABI record per function (wip-shared-struct-codegen-followups
    /// Slice 1). A function whose signature mentions `Option[shared T]`
    /// in return and/or parameter position is declared with a single
    /// nullable `ptr` (null = None, non-null = Some) at those positions
    /// instead of the conventional 4-i64 Option enum struct — closing
    /// the field-niche/call-ABI asymmetry and skipping the sret
    /// round-trip on every call. The function *body* still works on the
    /// conventional 4-word shape: `compile_function` unpacks niche
    /// params at entry, the return sites pack at `ret`, and
    /// `compile_call` packs args / unpacks the result, so every other
    /// codegen path (refcounting, pattern matching, RC-fallback
    /// analysis) is shape-blind to the ABI. Keyed by LLVM symbol name;
    /// names absent from the map (impl methods, closures, generic
    /// monos, coroutine ramps, extern decls) keep the conventional ABI.
    /// Eligibility is decided once in `declare_function`.
    pub(crate) fn_niche_abi: HashMap<String, state::NicheAbi>,
    /// Per-binding inner-shared-heap layout for `Option[shared T]`
    /// variables. Populated by `track_rc_option_var` at let-binding
    /// time; read by the `Assign` arm so reassignment of a tracked
    /// Option[shared T] binding adjusts refcounts symmetrically to
    /// the plain shared-T arm (dec old inner pointer, inc new inner
    /// pointer unless RHS is a fresh `Some(...)` literal). Without
    /// this, `next_a = n.next;` (LeetCode #2 recursive variant)
    /// stranded the old inner ref and over-decremented at scope
    /// exit, freeing a still-aliased chain.
    pub(crate) var_option_shared_heap: HashMap<String, StructType<'ctx>>,
    /// Flow-sensitive tail-return context for `Option[shared T]` returns.
    /// `Some(inner_heap)` means "the expression about to be compiled at a
    /// block's final-expr position is in function-tail-return position, and
    /// the function returns `Option[shared T]` whose inner heap layout is
    /// this". Threaded by `compile_function` → `compile_block` (final expr) →
    /// `compile_if_let` / `compile_match` (each branch's final expr), and
    /// CLEARED while compiling block statements so a non-tail `if let` in
    /// statement position never picks it up. When a tail leaf is a bare
    /// `Option[shared]` binding (`l1` / `l2`), `compile_block` inc's its inner
    /// in that branch's own block — the per-branch compensation that lets a
    /// function MIX `Some(<alias>)` tails (which need no inc) with bare-arg
    /// returns (which do) without the over/under-count a single merge-block
    /// inc would cause. See docs/implementation_checklist/phase-7-codegen.md.
    pub(crate) tail_ret_inner: Option<StructType<'ctx>>,
    /// Per-scope cleanup stack.  Each inner `Vec` is one scope frame; entries
    /// are emitted in reverse-push order at scope exit (innermost first).
    pub(crate) scope_cleanup_actions: Vec<Vec<CleanupAction<'ctx>>>,
    /// Phase 7 § *defer / errdefer codegen* slice 4. Staging slot for the
    /// about-to-be-returned Err payload, set by each error-exit site
    /// (`compile_question`'s `fail_bb`, `ExprKind::Return(Err(...))`, and
    /// the function-tail `Err(...)` emitter) immediately before calling
    /// `emit_scope_cleanup_for_error_path`, and cleared on return. Read
    /// by `emit_cleanup_action_at`'s `UserErrDefer { binding: Some(name),
    /// .. }` arm: allocates an entry alloca of the payload's LLVM type,
    /// stores the staged value, and registers `name` in `self.variables`
    /// for the duration of the body's `compile_block_with_frame` call.
    /// `None` means no payload is currently staged — only the no-binding
    /// form errdefer can fire (the binding form is gated on `is_some`).
    pub(crate) pending_errdefer_payload: Option<inkwell::values::BasicValueEnum<'ctx>>,
    /// Phase 7 § *defer / errdefer codegen* slice 4 follow-up (a) —
    /// wider-E payload reconstruction at the `?` site (2026-05-26).
    /// Source-level LLVM type of the current function's `Result[T, E]`
    /// Err arm — recorded at `compile_function` entry by walking
    /// `func.return_type` for the `Result[T, E]` shape and lowering E
    /// via `llvm_type_for_type_expr`. Read by `compile_question`'s
    /// `fail_bb` to call `rebuild_value_from_payload_words` against
    /// the result struct's payload words (w0/w1/w2 at fields 1/2/3),
    /// staging the source-typed value rather than the i64-coerced
    /// `w0` slice 4 originally used. `None` means the current function
    /// doesn't return `Result[T, E]` (or doesn't return at all) — the
    /// `?` site falls back to staging bare `w0` as i64 in that case.
    pub(crate) current_fn_err_payload_ty: Option<inkwell::types::BasicTypeEnum<'ctx>>,
    /// Set while compiling `main` when its declared return type is
    /// `Result[(), E]` — holds E's source `TypeExpr` (the error type). The
    /// LLVM `main` is the C entry (`i32`), so every Result-returning site —
    /// the tail, an explicit `return Ok/Err`, and a `?` early-return on the
    /// error path — must adapt the Result to a process exit code rather than
    /// `ret` the `{tag, ...}` aggregate (which fails module verification
    /// against the `i32` signature, B-2026-06-12-9). Per design.md § Entry
    /// Point, `Ok(())` exits 0 and `Err(e)` prints `Error: {e}\n` to stderr
    /// (via E's `Display`) then exits 1. The `TypeExpr` (not just an LLVM
    /// type) is needed so `emit_display_fn_for_type_expr` can synthesize the
    /// error's Display rendering. `None` for `fn main()` / `fn main() ->
    /// ExitCode` / any non-`main` function.
    pub(crate) main_result_err_te: Option<crate::ast::TypeExpr>,
    /// True while compiling `fn main() -> ExitCode` (Phase-8 entry-point
    /// contract Slice B). `main`'s LLVM signature is the C entry `i32`,
    /// and `ExitCode` is `distinct type = i32`, so the body's tail value
    /// IS the i32 exit code — the tail-return site `ret`s it (coerced to
    /// i32) rather than discarding it and returning `0` (the plain
    /// `fn main()` posture). Mutually exclusive with `main_result_err_te`.
    /// `false` for `fn main()` / `fn main() -> Result[(), E]` / non-`main`.
    pub(crate) main_returns_exitcode: bool,
    /// True while compiling a function whose declared return type is a
    /// borrow (`-> ref T` / `-> mut ref T`). The LLVM signature returns a
    /// thin `ptr`, so the tail / explicit-`return` sites must emit the
    /// ADDRESS of the borrow source (a `ref` param or a field reached
    /// through one) via `compile_ref_return_ptr`, not the materialized
    /// value — see `B-2026-06-07-5` (returned-borrow codegen). Set per
    /// function in `compile_function`.
    pub(crate) current_fn_returns_ref: bool,
    /// True only while compiling the RHS of a `let <name> = <ref-returning
    /// call>` — the one caller context that binds the borrow as a ref-local
    /// (deref on use). Outside it, a call to a borrow-returning function is
    /// rejected by `compile_call` rather than silently miscompiled (the
    /// returned `ptr` would be mishandled as a value). Direct use of a
    /// borrow-returning call result is a tracked Tier-1.5 follow-on
    /// (B-2026-06-07-5).
    pub(crate) compiling_ref_return_let_rhs: bool,
    /// Set by `compile_match` when the scrutinee is a borrow-returning
    /// call (`Map.get`, `Vec.first`, ...) — used by `bind_pattern_values`
    /// to suppress `track_vec_var` for the bound name, since the payload
    /// aliases the container's storage and the container's own cleanup
    /// already covers the buffer.
    pub(crate) pattern_binding_is_borrow: bool,
    /// Set by `compile_match` when the scrutinee enum is the type-erased
    /// `Option` / `Result` (B-2026-06-13-13 residual A). Their inline / boxed
    /// payloads are owned by the dedicated `FreeInlineOptionPayload` /
    /// boxed-scrutinee cleanup, NOT a per-field `EnumDrop`, so a pattern-bound
    /// struct payload (`Some(h)`) must NOT get a `track_struct_var` — that would
    /// double-free against the Option's own free. Gates the user-struct arm of
    /// the pattern-binding struct-drop registration.
    pub(crate) pattern_binding_scrutinee_is_option_result: bool,
    /// Phase 7.2 Slice DP — per-enum drop function cache (enum name →
    /// `__karac_drop_<EnumName>` `FunctionValue`). Lazily populated by
    /// `emit_enum_drop_switch` on first registration of a value-type
    /// enum binding via `track_enum_var`. One drop fn per enum type;
    /// reused across all registration sites for that type. Mirrors the
    /// existing `display_fn_cache` / `clone_fn_cache` lazy-synth pattern.
    pub(crate) enum_drop_fns: HashMap<String, FunctionValue<'ctx>>,
    /// Per-struct lazy drop-fn cache (struct name → `__karac_drop_struct_<Name>`
    /// `FunctionValue`). Lazily populated by `emit_struct_drop_synthesis` on
    /// first registration of a non-shared struct binding via `track_struct_var`.
    /// One drop fn per struct type; reused across registration sites. Mirrors
    /// `enum_drop_fns`. The drop fn walks fields and frees Vec/String data
    /// buffers + invokes `karac_map_free` on Map/Set handle fields. Structs
    /// with no heap-owning fields don't get an entry (the synthesis fn returns
    /// `None`) and don't reach `CleanupAction::StructDrop`.
    pub(crate) struct_drop_fns: HashMap<String, FunctionValue<'ctx>>,
    /// Per-user-type lazy drop-wrapper cache (type name →
    /// `karac_drop_<Type>` `FunctionValue`). Populated by
    /// `emit_user_drop_wrappers` for every type in
    /// `program.drop_method_keys` — i.e., every user type with a
    /// validated `impl Drop`. The wrapper invokes the user-defined
    /// `Type.drop` body and then hands off to the existing field-cleanup
    /// synthesizer (`emit_struct_drop_synthesis`) when the type has
    /// heap-owning fields. Prereq.2 of the user-`impl Drop` dispatch
    /// slice (`docs/implementation_checklist/phase-7-codegen.md`).
    /// Consumed by Prereq.3's scope-exit lowering pass via
    /// `module.get_function("karac_drop_<Type>")`.
    pub(crate) user_drop_wrapper_fns: HashMap<String, FunctionValue<'ctx>>,
    /// Per-shared-struct lazy drop-fn cache (shared-struct name →
    /// `__karac_rc_drop_<Name>` `FunctionValue`, or `None` when the
    /// struct has no heap-owning fields and `emit_rc_dec` can fall
    /// through to plain `free(ptr)`). Lazily populated by
    /// `emit_shared_struct_rc_drop_fn` on first registration of a
    /// shared-struct binding via `track_rc_var` / `track_rc_option_var`,
    /// or recursively from another struct's drop body when it
    /// encounters a shared-typed field. The drop fn walks each field
    /// of the shared struct's heap layout and, before `free(ptr)`,
    /// dispatches the appropriate cleanup per field type:
    ///   - Shared struct field → recursive `__karac_rc_drop_<Name>`
    ///     call (dec inner refcount; if it hits zero, transitively
    ///     drop the inner's chain).
    ///   - `Option[shared T]` field → tag-switch; on Some, dec the
    ///     inner shared pointer.
    ///   - Vec / String field → `cap > 0 ? free(data)` (same shape
    ///     as `CleanupAction::FreeVecBuffer`).
    ///   - Map / Set handle field → `karac_map_free*` (mirrors
    ///     `StructDrop`'s field walk).
    ///
    /// `None`-cached entries mean "no walk needed" — the drop fn isn't
    /// emitted and `emit_rc_dec` proceeds with the legacy plain-`free`
    /// path. Closes the recursive-drop gap for shared-struct chains
    /// (LeetCode #2 kata bench, 2026-05-17): without this, freeing
    /// the chain's head leaked every transitive `next: Option[ListNode]`
    /// because the dec→free path ignored field-bound shared refs.
    pub(crate) rc_drop_fns: HashMap<String, Option<FunctionValue<'ctx>>>,
    /// Cross-error-type conversion targets at `?` sites — populated from
    /// `Program.question_conversions` (set by the lowering pass from the
    /// typechecker's `question_conversions` map). Key: `(span.offset,
    /// span.length)` of the `?` expression. Value: target type name (e.g.
    /// `"AppError"`). When present, `compile_question` emits `Target.from(e)`
    /// against the inner err payload before the propagation early-return.
    pub(crate) question_conversions: HashMap<(usize, usize), String>,
    /// Per-callee effectfulness side-table — populated from
    /// `Program.callee_effectful` (set by the cli pipeline after effectcheck).
    /// Key: callable's canonical name (free fn `name`, assoc/method
    /// `Type.method`). Value: `true` iff the callee carries any of
    /// `reads`/`writes`/`sends`/`receives`. Read by `emit_branch_cancel_check`
    /// to skip the cooperative cancel atomic load when we can prove the
    /// callee is non-observably-effectful. Absent callees are treated as
    /// potentially effectful (fall back to the conservative MVP behavior).
    pub(crate) callee_effectful: HashMap<String, bool>,
    /// Per-method-call → `Type.method` callee key side-table — populated
    /// from `Program.method_callee_types` (set by the lowering pass from
    /// `TypeCheckResult.expr_types`). Key: `(span.offset, span.length)` of
    /// the `MethodCall` expression. Value: canonical `Type.method` string
    /// usable as a lookup into `callee_effectful`. Lets
    /// `compile_method_call` apply the same narrowing that `compile_call`
    /// applies to free-function and `Type.assoc` calls.
    pub(crate) method_callee_types: HashMap<(usize, usize), String>,
    /// Phase 6 line 26 slice 8ab: per-call-site effect-variable
    /// substitutions, snapshotted from `Program.call_effect_subs`
    /// (which `cli.rs::Pipeline` populates from
    /// `EffectCheckResult.call_effect_subs` via
    /// `build_call_effect_subs_table`). Slice 8y (entry 32) reads
    /// this in `compile_generic_call` to gate per-mono state-machine
    /// emission on whether the resolved per-call effects include any
    /// network-yield verb. Empty when effectcheck didn't run or no
    /// polymorphic-effect callees exist.
    pub(crate) call_effect_subs: crate::ast::CallEffectSubsTable,
    /// Per-`unwrap`/`expect`/`is_*` MethodCall → inner `TypeExpr` side-
    /// table — populated from `Program.method_unwrap_inner_types` (set by
    /// the lowering pass from `TypeCheckResult.method_unwrap_inner_types`).
    /// Key: `(span.offset, span.length)` of the MethodCall expression.
    /// Value: the `T` of `Option[T]` (or success-`T` of `Result[T, E]`).
    /// Codegen's `unwrap` arm uses this to lower the inner type to its
    /// LLVM shape and reconstitute the payload words back to a value.
    pub(crate) method_unwrap_inner_types: HashMap<(usize, usize), TypeExpr>,
    /// Per-channel-op MethodCall → element `TypeExpr` side-table — populated
    /// from `Program.channel_elem_types`. Key: `(span.offset, span.length)`
    /// of the `Sender.send` / `Receiver.recv` / `Receiver.try_recv`
    /// MethodCall. Value: the channel element `T`. The channel-op arm of
    /// `compile_method_call` lowers `T` to its LLVM shape to size the
    /// `karac_runtime_channel_*` transfer and shape the recv out slot.
    pub(crate) channel_elem_types: HashMap<(usize, usize), TypeExpr>,
    /// Inner type of every borrow-typed (`ref T`) expression, keyed by span
    /// — populated from `Program.ref_return_inner_types`. Lets the `let` arm
    /// recognise that a method-call RHS (`let n = u.name()`) returns a
    /// borrow and bind `n` as a ref-local. Method-ref half of
    /// B-2026-06-07-5 (free-fn calls use `fn_ref_return_inner`).
    pub(crate) ref_return_inner_types: HashMap<(usize, usize), TypeExpr>,
    /// Bare names of USER-defined impl methods whose declared return type is
    /// a borrow (`-> ref T`). Gates the method-ref caller path (let-bind +
    /// direct-use rejection) so it fires ONLY for user accessors — builtin
    /// ref-returning methods (`Map.or_insert`, `Vec.get`, …) are never in
    /// a user impl block and so keep their dedicated codegen. B-2026-06-07-5.
    pub(crate) user_ref_method_names: std::collections::HashSet<String>,
    /// Set of `(span.offset, span.length)` keys for every expression whose
    /// Kāra type is `String`. Populated from `Program.string_typed_exprs`
    /// (which the lowering pass derives from `TypeCheckResult.expr_types`).
    /// Lets codegen distinguish `String` from `Vec[T]` and other 3-word
    /// `{ptr, i64, i64}` types whose LLVM struct shape is identical.
    /// First consumer: `emit_sort_by_key_inline_thunk`'s String-key
    /// dispatch arm — `String` and `Vec[u8]` are indistinguishable from
    /// the LLVM value alone, so the span-set is what tells them apart.
    pub(crate) string_typed_exprs: HashSet<(usize, usize)>,
    /// Per-expression Tensor type info (element TypeExpr + static dims),
    /// keyed by `(span.offset, span.length)`. Populated from
    /// `Program.tensor_typed_exprs` (lowering pass, from
    /// `TypeCheckResult.expr_types`). Consumed at `Tensor.from(...)`
    /// construction sites, unannotated tensor let-bindings, and indexing
    /// dispatch. See `src/codegen/tensor.rs` for the value layout this
    /// drives.
    pub(crate) tensor_typed_exprs: HashMap<(usize, usize), crate::ast::TensorTypeInfo>,
    /// Per-binding Tensor registration: element LLVM type + static dims
    /// (`Some(n)` = concrete literal usable for stride folding /
    /// bounds-check elision; `None` = read the dim from the value's
    /// runtime header). Populated by `register_var_from_type_expr`'s
    /// Tensor arm (annotations, params, for-bindings) and the let-path
    /// side-table fallback for unannotated bindings. Consulted by
    /// `compile_index` / `compile_index_store` / method dispatch.
    pub(crate) tensor_var_infos: HashMap<String, state::TensorVarInfo<'ctx>>,
    /// Expected-type threading for `Tensor.zeros` / `ones` / `full` —
    /// these constructors can't recover the element type or rank from
    /// their `dims: Vec[i64]` argument, so the let-binding path stashes
    /// the destination binding's registered `TensorVarInfo` here before
    /// compiling the RHS (the exact `pending_let_elem_type` mechanism
    /// `Vec.with_capacity` uses). `Tensor.from` never needs it (dims and
    /// element type both come from the literal).
    pub(crate) pending_let_tensor_info: Option<state::TensorVarInfo<'ctx>>,
    /// Set of `(span.offset, span.length)` keys for every expression whose
    /// Kāra type is a `Vector[T, N]` with an unsigned-integer element.
    /// Populated from `Program.unsigned_vector_exprs`. The LLVM `<N x iX>`
    /// lane type is signless, so `compile_vector_method`'s `reduce_min`/
    /// `reduce_max` arm consults this (keyed by the receiver-vector span)
    /// to pick the unsigned compare predicate (`ult`/`ugt`) over the signed
    /// default. Shared infra for the slice-3 mask comparisons.
    pub(crate) unsigned_vector_exprs: HashSet<(usize, usize)>,
    /// Sibling to `string_typed_exprs`: for every expression whose Kāra
    /// type is a `Named` struct, the canonical struct name. Populated
    /// from `Program.expr_struct_type_names`. Lets codegen recover the
    /// source-level struct identity from a value alone — the LLVM struct
    /// type doesn't carry the name back — so `emit_sort_by_key_inline_thunk`
    /// can look up per-field type names via `struct_field_type_names` and
    /// dispatch the right per-field comparator (int / String) when the
    /// key is a struct with mixed-type fields.
    pub(crate) expr_struct_type_names: HashMap<(usize, usize), String>,
    /// Sibling to `expr_struct_type_names`: for every expression whose
    /// Kāra type is a struct with a user-supplied `impl Ord for T`, maps
    /// span → canonical `"Type.cmp"` callee key. Populated from
    /// `Program.user_ord_typed_exprs`. `emit_sort_by_key_inline_thunk`
    /// consults this map before the field-aware cascade so the user's
    /// `cmp` runs instead of a synthesized derive-equivalent lex compare.
    pub(crate) user_ord_typed_exprs: HashMap<(usize, usize), String>,
    /// Surface `TypeExpr` per heap-owning *temporary* expression —
    /// populated from `Program.owned_temp_drops` (set by the lowering pass
    /// from `TypeCheckResult.expr_types`). `materialize_owned_temp` keys
    /// this by the producing expression's span to reconstruct an unnamed
    /// temporary's scope-exit cleanup (Vec element type / Map key-val
    /// classification / RC heap layout). See
    /// `docs/spikes/general-owned-temp-tracking.md` (slice 2).
    pub(crate) owned_temp_drops: HashMap<(usize, usize), TypeExpr>,
    /// Pointee surface `TypeExpr` per raw-pointer-typed (`*const T` / `*mut T`)
    /// expression, keyed by span — populated from
    /// `Program.raw_pointer_pointee_types`. The unary-deref arm keys this by the
    /// operand span to `load` through a raw pointer (whose value is the address)
    /// instead of returning the address; references are absent and take the
    /// pass-through path.
    pub(crate) raw_pointer_pointee_types: HashMap<(usize, usize), TypeExpr>,
    /// Fully-instantiated surface `TypeExpr` per *generic* `Named`
    /// instantiation expression (`Option[String]`, `Result[i64, AllocError]`,
    /// generic user enums) — populated from `Program.enum_inst_type_exprs`
    /// (set by the lowering pass from `TypeCheckResult.expr_types`). Keyed by
    /// the expression's `(span.offset, span.length)`. `compile_enum_eq` uses
    /// it to recover the concrete type argument a generic enum's variant
    /// payload was instantiated with (the `[String]` that `var_type_names`'
    /// bare `"Option"` loses), so a `Some(String)` payload compares by content
    /// rather than by pointer word. A missing entry degrades to the word-wise
    /// path (sound for scalar/unit enums), never a miscompile.
    pub(crate) enum_inst_type_exprs: HashMap<(usize, usize), TypeExpr>,
    /// Instantiated generic-enum type per *local variable / parameter* name
    /// (`opt` → `Option[String]`). Populated during codegen traversal at let
    /// and parameter binding sites (cleared per function, like
    /// `var_type_names`), so heap-payload enum `==` (`compile_enum_eq`) can
    /// resolve a variable operand's type argument by **name** — collision-free,
    /// unlike `enum_inst_type_exprs`, whose span keys collide across f-string
    /// interpolations (every interp expr is re-parsed under a fixed-length
    /// `fn __interp__() { … }` wrapper). The span-keyed table remains the source
    /// at the reliable, absolute-spanned binding sites; this name map is the
    /// reliable lookup at use sites.
    pub(crate) enum_inst_var_types: HashMap<String, TypeExpr>,
    /// Per-pattern-binding surface type table — populated from
    /// `Program.pattern_binding_types` (set by the lowering pass from
    /// `TypeCheckResult.pattern_binding_types`). Key: pattern's
    /// `(span.offset, span.length)`. Value: canonical type name (e.g.
    /// `"MyError"`). Used in `bind_pattern_values` to reconstitute struct
    /// payloads from the i64 word when the surface binding type is a
    /// struct, so subsequent `.field` access dispatches through the right
    /// struct shape.
    pub(crate) pattern_binding_types: HashMap<(usize, usize), String>,
    /// Sibling to `pattern_binding_types` carrying the inner element
    /// `TypeExpr` for `Vec[T]` / `Slice[T]` pattern bindings only. Populated
    /// from `Program.pattern_binding_inner_types`. Read by
    /// `bind_pattern_values` to lower the inner element type to a
    /// `BasicTypeEnum` (via `llvm_type_for_type_expr`) and register it
    /// under the binding's variable name in `vec_elem_types` /
    /// `slice_elem_types`, so subsequent method-dispatch (`xs.len()` /
    /// `xs[0]` / `xs.push(...)`) on a pattern-bound `Vec` / `Slice` payload
    /// routes through the right element-typed path. PB sibling slice
    /// (2026-05-09).
    pub(crate) pattern_binding_inner_types: HashMap<(usize, usize), TypeExpr>,
    /// Per-leaf-binding borrow mode populated from
    /// `Program.pattern_binding_borrow_modes`. Consumed by
    /// `bind_pattern_values` (Binding arm) to wrap a value-typed leaf
    /// binding in a ref-shim — an extra `ptr` alloca holding the value
    /// alloca's address, registered in `ref_params` — so call sites
    /// expecting `ref T` / `mut ref T` receive the right ABI shape.
    /// Empty for owned bindings. Slice 3a, 2026-05-14.
    pub(crate) pattern_binding_borrow_modes:
        HashMap<(usize, usize), crate::ast::PatternBindingBorrow>,
    /// Top-level `const NAME: T = value` declarations, populated by
    /// `compile_program` from `Item::ConstDecl` items before any function
    /// body is compiled. Key: const name. Value: the const's value
    /// expression. References to a const inside function bodies (parsed as
    /// `ExprKind::Identifier(name)` for bare uses) re-compile this stored
    /// expression at the use site, leaving constant folding to LLVM.
    /// Cycles are precluded upstream by the typechecker's const-evaluation
    /// pass (`check_const_decl`).
    pub(crate) consts: HashMap<String, Expr>,
    /// Module-level `let` / `let mut` bindings — slice 9 of the
    /// phase-8 module-let work (design.md §1278-1330). Populated by
    /// `declare_module_bindings` before any function body is
    /// compiled. Identifier loads in function bodies short-circuit
    /// to a real LLVM `load` from the global via
    /// `try_load_module_binding`; assignments / compound-assigns
    /// route through `try_store_module_binding`. Distinct from
    /// `consts`, which inlines the value expression at each use site
    /// — module bindings need real LLVM globals so `let mut`
    /// mutation is observable across functions and `#[thread_local]`
    /// gets the per-task disjoint instance.
    pub(crate) module_bindings: HashMap<String, module_bindings::ModuleBindingInfo<'ctx>>,
    /// Source filename threaded in from the CLI (`compile_to_object_with_options`
    /// / `compile_to_ir_with_options`). When `Some`, `emit_error_trace_push`
    /// emits a deduped global string and passes its `(ptr, len)` to the runtime
    /// so error-return traces print as `<file>:<line>:<col>`. `None` preserves
    /// the original `<line>:<col>` MVP output for callers that don't supply a
    /// filename (most tests, ad-hoc IR dumps).
    pub(crate) source_filename: Option<String>,
    /// Memoized `(ptr, len)` for the global string materialized from
    /// `source_filename`. Populated lazily on first `?` site so we don't add
    /// an unused global to programs with no `?` propagation. Cleared on each
    /// `compile_program` entry alongside the other side-tables.
    pub(crate) source_filename_global: Option<(PointerValue<'ctx>, u64)>,
    /// Source text threaded in from the CLI (`compile_to_object_with_options`
    /// / `compile_to_ir_with_options` via `set_source_text`). When `Some`,
    /// `record_spawn_site` resolves each `par {}` block's byte offset to
    /// `(line, col)` via `crate::byte_offset_to_line_col`. When `None`,
    /// recorded entries fall back to `(0, 0)` — the metadata table still
    /// emits, just without source-position fidelity (most tests and ad-hoc
    /// IR dumps don't supply source text, and the `(line, col)` fields are
    /// strictly for the slice 5 / debugger surface).
    pub(crate) source_text: Option<String>,
    /// Symbols carrying `#[used]` collected during declaration. After the
    /// program is fully lowered, `emit_llvm_used` materializes them into the
    /// special `@llvm.used` appending-linkage global so the linker preserves
    /// each symbol even when nothing else references it. Order is preserved
    /// for stable IR output (helps snapshot tests and diffs).
    pub(crate) used_symbols: Vec<FunctionValue<'ctx>>,
    /// Data-global counterpart to `used_symbols` — globals (not
    /// functions) that need to land in `@llvm.used` so the linker
    /// preserves them across `--gc-sections` / `-dead_strip`. v1
    /// consumer: the `.kara_jit_template` manifest emitted by
    /// `emit_jit_template_section` (phase-7 line 14).
    pub(crate) used_data_globals: Vec<inkwell::values::GlobalValue<'ctx>>,
    /// When compiling a par-branch function body, holds the LLVM pointer
    /// to the runtime's `AtomicBool` cancel flag (the second parameter
    /// passed by `karac_par_run`). `compile_call` reads this to emit a
    /// cooperative cancel check before each call site, implementing
    /// mid-branch cooperative cancellation per `design.md § Effect-boundary
    /// cooperative cancellation`. Conservatively fires before ANY call —
    /// the spec narrows the requirement to effectful calls (reads/writes/
    /// sends/receives) but the over-approximation is sound and avoids
    /// threading the `EffectCheckResult` through codegen for v1.
    /// `None` outside par branches.
    pub(crate) branch_cancel_ptr: Option<PointerValue<'ctx>>,
    // ── RC-fallback bindings ──────────────────────────────────────
    /// Per-function RC-fallback binding names populated from `OwnershipCheckResult`.
    /// Function name → set of binding names that need heap-boxing + refcount.
    pub(crate) rc_fallback_fns: HashMap<String, HashSet<String>>,
    /// RC elision phase A (`src/ownership/elision.rs`; design record in
    /// phase-7-codegen.md): per-function sets of shared bindings whose
    /// refcount provably never exceeds 1. The let-site queues a
    /// `FreeSharedElided` cleanup (unconditional null-guarded free)
    /// instead of `RcDec` for these. Keyed by fn key (bare name /
    /// `Type.method`), matching `current_fn_name`.
    pub(crate) elided_bindings: HashMap<String, HashSet<String>>,
    /// Phase B1 cluster roots: fn key → root binding → (member struct
    /// name, link user-field index). The let-site swaps the root's
    /// cleanup for `FreeClusterWalk`. Cursors and fresh nodes keep
    /// their standard cleanups (drop-side-only consumption).
    pub(crate) elided_cluster_roots:
        HashMap<String, HashMap<String, (String, usize, crate::ownership::ReturnedChain)>>,
    /// Phase B2 build-side elision: fn key → cluster binding →
    /// role/cluster record. Populated only for clusters whose analysis
    /// `b2` flag is set (displacement-free canonical shapes). Consulted
    /// by the let-site shared/option arms, both Assign arms, and the
    /// dedicated link-store fast path.
    pub(crate) elided_b2_bindings: HashMap<String, HashMap<String, state::B2Binding>>,
    /// Phase D headerless cluster density: fn key → member type name →
    /// link user-field index, for clusters whose analysis `headerless`
    /// flag is set (b2 + dual type-purity gate — see
    /// `ElidedCluster::headerless`). Within such a fn, every value of
    /// the member type is provably a cluster member, so the heap
    /// layout is keyed per `(fn, type)`: allocation drops the 8-byte
    /// rc header (`emit_headerless_alloc`), and every member-field GEP
    /// routes through `shared_gep_layout` to pick the headerless twin
    /// struct type at field base 0 instead of `heap_type` at base one.
    /// The link index rides along for the lazy niche-shape check in
    /// `headerless_here` (a non-niche link would make the free-walk's
    /// RcDec fallback reachable — structurally excluded by demoting).
    pub(crate) headerless_fns: HashMap<String, HashMap<String, usize>>,
    /// Phase C1c caller adoption: fn key → adopted root binding →
    /// (member type, link user-field index), for clusters whose
    /// analysis `adopted` flag is set. The root is an `Option[shared
    /// T]` binding born from a fresh-return builder call; its let-site
    /// queues a `FreeClusterWalkOption` cleanup instead of the
    /// `RcDecOption` dec-walk (and skips `var_option_shared_heap`
    /// registration — adopted roots are never reassigned, the analysis
    /// poisons that). Kept separate from `elided_cluster_roots` so the
    /// literal-cluster let-site/transfer paths never see adopted roots.
    pub(crate) adopted_cluster_roots: HashMap<String, HashMap<String, (String, usize)>>,
    /// Phase C2b: ANALYSIS-side headerless-T candidates — member type →
    /// (link index, touching fn keys). Reconciled into
    /// `headerless_types` in `compile_program` once coroutine keys and
    /// struct layouts exist (a coro toucher or a non-niche link drops
    /// the type; every consumer keys on the reconciled set, so a drop
    /// deactivates the whole composition coherently).
    pub(crate) headerless_type_candidates: HashMap<String, (usize, Vec<String>)>,
    /// Phase C2b: the FINAL program-wide headerless set. A member type
    /// in here has no rc word anywhere — `headerless_here` answers true
    /// in every fn, builders allocate via `emit_headerless_alloc`, the
    /// borrowed-param exit decs and call-site arg incs are skipped, and
    /// the arg-sanctioned adopted families activate.
    pub(crate) headerless_types: HashSet<String>,
    /// Phase C2b: adopted families that used the sanctioned-arg channel
    /// — active ONLY when their member type is in `headerless_types`
    /// (otherwise the binding falls back to full RC and the ordinary
    /// arg-inc / exit-dec balance applies).
    pub(crate) conditional_adopted_roots: HashMap<String, HashMap<String, (String, usize)>>,
    /// Phase C2b: borrowed-param records per fn — (param name, position,
    /// member type). Drives the callee-side exit-dec skip (by name, in
    /// `compile_function`) and the call-site arg-inc skip (by position,
    /// in the direct-call arg loop) — both gated on `headerless_types`.
    pub(crate) borrowed_param_skips: HashMap<String, Vec<(String, usize, String)>>,
    /// Per-function Arc-promoted binding names — the subset of `rc_fallback_fns`
    /// flagged by the ownership pass as crossing a `par {}` thread boundary.
    /// Inc/dec on these bindings emits atomic LLVM operations (`atomicrmw add` /
    /// `atomicrmw sub`, `SeqCst`); the rest stay on plain non-atomic load+arith+store.
    /// Allocation site is unchanged — the heap layout `{ refcount: i64, payload: T }`
    /// is identical for both flavors.
    pub(crate) arc_fallback_fns: HashMap<String, HashSet<String>>,
    /// Heap struct type for each active RC-fallback binding in the current function.
    /// Cleared at each `compile_function` call. Key: binding name.
    pub(crate) rc_fallback_heap_types: HashMap<String, StructType<'ctx>>,
    /// Synthesized "free the boxed value's heap fields" fn per RC-fallback
    /// box heap type (`{i64 rc, value}`). When a non-shared aggregate
    /// (tuple / struct with String/Vec fields) is RC-fallback-boxed, the box
    /// free at `rc == 0` must recurse into the boxed value's heap fields
    /// before releasing the box — otherwise those buffers leak
    /// (B-2026-06-10-8). The fn takes the box pointer, GEPs to the value
    /// field, and emits a `cap`-guarded `free` for every `{ptr,len,cap}`
    /// (String/Vec) field, recursing into nested aggregates; it does NOT
    /// free the box itself (`emit_rc_dec`'s fallback `free` does that after).
    /// Keyed on the box heap type (module-stable, embeds the value type), so
    /// bindings of the same boxed type share one fn. Module-level cache like
    /// `drop_fn_cache` — not cleared per function. A `Vec` with linear
    /// `StructType`-equality lookup (LLVM `StructType` is `PartialEq` but not
    /// `Hash`/`Eq`, so it can't key a `HashMap`); the box-type count per
    /// program is tiny, and `emit_rc_dec` already scans `shared_types` the
    /// same way.
    pub(crate) rc_fallback_box_drop_fns: Vec<(StructType<'ctx>, FunctionValue<'ctx>)>,
    /// Synthesized "free this aggregate's heap fields" drop fns for ANONYMOUS
    /// aggregates — a let-bound tuple (`let t = (i, f"x")`) the named-struct
    /// `track_struct_var` / `struct_drop_fns` path can't reach (a tuple has no
    /// type name). Body is `emit_aggregate_heap_field_frees`. Keyed on the
    /// aggregate LLVM type; same `Vec` + linear `StructType`-equality lookup
    /// rationale as `rc_fallback_box_drop_fns` (`StructType` isn't `Hash`).
    /// Registered as a `CleanupAction::StructDrop` by `track_tuple_var`
    /// (B-2026-06-11-4 part a).
    pub(crate) aggregate_drop_fns: Vec<(StructType<'ctx>, FunctionValue<'ctx>)>,
    /// Per-closure capture path modes sourced from
    /// `OwnershipCheckResult::closure_capture_path_modes` — line 353
    /// phase-5 checklist disjoint-capture slice 4. When a closure
    /// expression's `SpanKey` lives in this map, `compile_closure` lays
    /// the env struct out with one slot per captured `CapturePath` (each
    /// sized to the leaf type at the projection chain end) and stitches
    /// the leaf values back into a fresh root alloca in the synthesized
    /// closure body. Absent → fall back to the per-name
    /// `collect_closure_free_vars` layout (preserves the `compile_to_ir`
    /// path and any codegen-only tests that don't run the ownership
    /// pass).
    pub(crate) closure_capture_paths: HashMap<SpanKey, Vec<(CapturePath, OwnershipMode)>>,
    /// Per-`par {}` block capture modes — phase-7 L227. Threaded from
    /// `OwnershipCheckResult::par_capture_modes`. Keyed by the par
    /// expression's `SpanKey`. Consumed in `emit_par_branch_fn`'s
    /// capture-unpack loop: a `(name, ParCaptureMode::SharedRc)`
    /// entry triggers atomic rc_inc + `track_rc_var` registration so
    /// the branch's scope-exit cleanup decs the heap pointer. Names
    /// absent from this map (or par blocks absent from the outer
    /// map) fall through to today's by-value-through-env copy
    /// behavior. Empty when codegen runs without an ownership pass
    /// (e.g. `compile_to_ir` invoked without an `OwnershipCheckResult`).
    pub(crate) par_capture_modes: HashMap<SpanKey, Vec<(String, ParCaptureMode)>>,
    /// Per-function parallelization decisions populated from `ConcurrencyAnalysis`.
    /// Function name → `FunctionConcurrency` (parallel groups + total stmt count).
    /// Threaded in by `load_concurrency_analysis`; consumed in slice 2 by the
    /// auto-par lowering path that emits `karac_par_run` for inferred groups
    /// outside explicit `par {}` blocks. Empty when no analysis was supplied.
    pub(crate) concurrency_decisions: HashMap<String, FunctionConcurrency>,
    /// Name of the function currently being compiled (for rc_fallback_fns lookup).
    pub(crate) current_fn_name: String,
    /// Source span of the expression currently being compiled. Set at the top
    /// of `compile_expr`; read by `emit_panic` for Level 2 crash diagnostics
    /// (design.md § Crash diagnostics) — `panic at <file>:<line>:<col> in
    /// <fn>: <msg>`. `Span` already carries 1-indexed `line`/`column`, so no
    /// byte-offset resolution is needed. `None` until the first expression is
    /// compiled (synthetic panics with no originating expression fall back to
    /// the bare `panic: <msg>` form).
    pub(crate) current_span: Option<crate::token::Span>,
    /// Level 2 crash diagnostics — Part 2: DWARF debug-info state. `Some` only
    /// when `KARAC_DEBUG_INFO` is on AND a source filename is threaded in;
    /// `None` (the default) makes every `di_*` hook a cheap early-return so the
    /// standard codegen path is byte-for-byte unchanged. See `debug_info.rs`.
    pub(crate) debug_info: Option<debug_info::DebugInfo<'ctx>>,
    // ── Par block runtime ─────────────────────────────────────────
    /// Monotonic counter used to generate unique par-branch function names.
    /// Also serves as the `SpawnSiteId` for each `par {}` block — the value
    /// at the time `emit_par_run` records a spawn site is the ID written
    /// into the `KARAC_SPAWN_SITES` metadata table (slice 3 of the
    /// Debugger Contract; see `SpawnSiteRecord`).
    pub(crate) par_counter: u32,
    /// Runtime struct `KaracBranch { ptr func, ptr ctx }` — shared across par blocks.
    pub(crate) karac_branch_ty: StructType<'ctx>,
    /// Runtime entry point `void karac_par_run(const KaracBranch*, usize)`.
    pub(crate) karac_par_run_fn: FunctionValue<'ctx>,
    /// Runtime entry point `void karac_par_reduce(*const KaracReduceDescriptor,
    /// *mut u8 out_slot, u32 spawn_site_id)`. Declared in slice 3a, called
    /// from slice 3b's `src/codegen/reduce.rs::emit_reduce_call`. See
    /// `runtime/src/lib.rs`'s `karac_par_reduce` for the ABI.
    pub(crate) karac_par_reduce_fn: FunctionValue<'ctx>,
    // ── Debugger contract: SpawnSiteId metadata (slice 3) ─────────
    /// One entry per `par {}` block (explicit or inferred). Populated by
    /// `record_spawn_site`; emitted as the `KARAC_SPAWN_SITES` global by
    /// `emit_spawn_sites_metadata` at the end of compilation. The order
    /// matches `SpawnSiteId` order (entry 0 → ID 0, entry 1 → ID 1, …).
    pub(crate) spawn_sites: Vec<SpawnSiteRecord>,
    /// Whether `KARAC_SPAWN_SITES` and friends emit populated. Driven by
    /// the `KARAC_RUNTIME_DEBUG_METADATA` env var read at `Codegen::new`
    /// time:
    ///
    /// - `Ok("0")` → false (gate explicitly off).
    /// - `Ok(_)`   → true.
    /// - `Err(_)`  → true (dev default).
    ///
    /// Slice 3 ships dev-default-on with env-var override only; profile-
    /// aware defaults (release / embedded / `isr` → off) and the TOML
    /// config home for the knob land in Phase 8.5 Track 2 (Build &
    /// Dependency Tooling). When the gate is off, all three globals
    /// (`KARAC_SPAWN_SITES`, `_LEN`, `_ENABLED`) still emit so slice 5's
    /// runtime API can read through the same symbols regardless of build
    /// mode and degrade cleanly — `_LEN` is zero, the array has zero
    /// entries, and `_ENABLED` is false. ID minting is unaffected so
    /// `__par_branch_<id>_<i>` symbol names stay stable across the
    /// gate-on / gate-off boundary. See `phase-8-stdlib-floor.md`
    /// § Auto-Concurrency Codegen — Debugger Contract slice 3.
    pub(crate) runtime_debug_metadata_enabled: bool,
    /// Slice 6 (Parallax-lite workload) — when true,
    /// `compile_function_body` skips its parallel-group dispatch path
    /// entirely and falls through to plain sequential `compile_block`,
    /// disabling auto-par codegen. Read once from the `KARAC_AUTO_PAR`
    /// env var at `Codegen` construction (see `read_auto_par_env`); the
    /// default is `false` (auto-par on). Used to support side-by-side
    /// wall-clock benchmarking of auto-par vs sequential codegen on the
    /// same workload without changing source. The user-facing
    /// `--sequential` CLI flag is a Phase 8.5 Track 2 deliverable; in
    /// v1, `KARAC_AUTO_PAR=0` is the only way to flip the gate. See
    /// `phase-8-stdlib-floor.md` § "Auto-Concurrency Codegen —
    /// Parallax-lite Workload".
    ///
    /// Also forced on for wasm targets (phase-10 "WASM concurrency
    /// lowering — sequential default"): the target is single-threaded,
    /// so an auto-par fan-out is pure overhead (branch-fn synthesis +
    /// runtime dispatch) with no parallelism to buy — the untransformed
    /// sequential program *is* the sequential lowering. Explicit
    /// `par {}` blocks are unaffected: they still lower through
    /// `karac_par_run` (sequential in the wasm runtime archive) so
    /// their cancellation/result-slot semantics are preserved.
    pub(crate) auto_par_disabled: bool,
    // ── Theme 6: `with_provider[R]` trait-method dispatch ──────────
    /// Resource name → stable u32 ID assigned at codegen init from the
    /// declaration order of `Item::EffectResource` items. The same
    /// integer flows through to runtime calls (`karac_provider_push`,
    /// `karac_provider_lookup`); the runtime is name-agnostic.
    pub(crate) provider_resource_ids: HashMap<String, u32>,
    /// Resource name → trait name for resources declared as
    /// `effect resource R: T`. Used to (1) drive vtable emission for
    /// the impls of `T` and (2) resolve method indices at `R.method(...)`
    /// call sites.
    pub(crate) provider_resource_traits: HashMap<String, String>,
    /// Trait name → ordered method-name list (source-declaration order
    /// from the `trait T { ... }` block). Vtables for `impl T for U`
    /// store fn ptrs in this same order; method dispatch resolves the
    /// vtable index by `position()` against this list.
    pub(crate) provider_trait_methods: HashMap<String, Vec<String>>,
    /// Trait-less *user* effect resource (`effect resource R;`, no `: T`)
    /// → ordered method-name list, derived from the override type's
    /// inherent-impl method order during the eager ambient-vtable pre-pass
    /// (`emit_ambient_provider_vtables`). A trait-less resource has no trait
    /// to pin a canonical method order, so it is keyed by *resource* (the
    /// call site `R.method(...)` knows R but not the override type U) and
    /// plays the same role `provider_trait_methods` plays for trait-ful
    /// resources: vtable layout + dispatch index. Distinct from
    /// `prelude::AMBIENT_RESOURCE_METHODS` (prelude resources like `Clock`
    /// keep their hardcoded order + FFI default); membership here is the
    /// discriminator that routes a trait-less resource through the
    /// always-override runtime dispatch (no FFI default) in
    /// `try_compile_provider_dispatch`.
    pub(crate) user_ambient_resource_methods: HashMap<String, Vec<String>>,
    /// (impl-target type name, trait name) → emitted vtable global.
    /// Populated after impl method declarations run in `compile_program`.
    pub(crate) provider_vtables: HashMap<(String, String), GlobalValue<'ctx>>,
    /// Runtime extern: `karac_provider_push(frame_ptr, resource_id, data_ptr, vtable_ptr)`.
    /// Consumed by `with_provider[R]` lowering (sub-step 3).
    pub(crate) karac_provider_push_fn: FunctionValue<'ctx>,
    /// Runtime extern: `karac_provider_pop()`. Consumed by `with_provider[R]`
    /// lowering (sub-step 3) for the matching pop on body exit.
    pub(crate) karac_provider_pop_fn: FunctionValue<'ctx>,
    /// Runtime extern: `karac_provider_lookup(resource_id) -> ProviderLookupResult`.
    /// Consumed by `R.method(...)` dispatch (sub-step 4) to find the
    /// active provider's data pointer and vtable.
    pub(crate) karac_provider_lookup_fn: FunctionValue<'ctx>,
    /// Runtime extern: `karac_provider_get_stack_head() -> *const ProviderFrame`.
    /// Consumed by par-block lowering (sub-step 5) to snapshot the
    /// calling thread's stack head into the par-block env-struct.
    pub(crate) karac_provider_get_stack_head_fn: FunctionValue<'ctx>,
    /// Runtime extern: `karac_provider_set_stack_head(head)`. Consumed
    /// by par-branch fn prologues (sub-step 5) to seed each worker
    /// thread's TLS from the env-struct snapshot, so providers in
    /// scope at the par-block site stay visible inside spawned branches.
    pub(crate) karac_provider_set_stack_head_fn: FunctionValue<'ctx>,
    /// Runtime extern: `karac_tracing_get_active_span() -> i64` (phase-8
    /// line 153). Consumed by the `tracing_active_span()` builtin (which
    /// `Log.*` / `LogEvent` use to auto-stamp the ambient span) and by
    /// the `with_span` lowering to snapshot the prior active span.
    pub(crate) karac_tracing_get_active_span_fn: FunctionValue<'ctx>,
    /// Runtime extern: `karac_tracing_set_active_span(i64)` (phase-8 line
    /// 153). Consumed by the `with_span(span, ||body)` lowering to install
    /// the body's active span and restore the prior one on exit, and by
    /// par-branch prologues to inherit the parent's active span.
    pub(crate) karac_tracing_set_active_span_fn: FunctionValue<'ctx>,
    /// Runtime extern: `karac_tracing_get_min_level() -> i64` (phase-8 line
    /// 156, codegen half). The `tracing_level_enabled(rank)` builtin lowers
    /// to `rank >= this`, so a compiled `Log.*` honors `Log.set_min_level`.
    pub(crate) karac_tracing_get_min_level_fn: FunctionValue<'ctx>,
    /// Runtime extern: `karac_tracing_set_min_level(i64)` (phase-8 line
    /// 156). The `tracing_set_min_level(rank)` builtin (called from
    /// `Log.set_min_level`'s lowered body) writes the process-global level.
    pub(crate) karac_tracing_set_min_level_fn: FunctionValue<'ctx>,
    /// Runtime extern: `karac_tracing_set_exporter(*const u8, *const u8)`
    /// (phase-8 line 156). The `tracing_set_exporter(e)` builtin registers
    /// the heap-leaked exporter value + its `export_event` fn-ptr here.
    pub(crate) karac_tracing_set_exporter_fn: FunctionValue<'ctx>,
    /// Runtime extern: `karac_tracing_get_exporter_data() -> *const u8`
    /// (phase-8 line 156). The `tracing_emit_event` lowering branches on
    /// this (null → default `StdoutExporter`, else indirect-dispatch).
    pub(crate) karac_tracing_get_exporter_data_fn: FunctionValue<'ctx>,
    /// Runtime extern: `karac_tracing_get_exporter_fn() -> *const u8`
    /// (phase-8 line 156). The registered sink's `export_event` fn-ptr, used
    /// by the `tracing_emit_event` lowering for the indirect call.
    pub(crate) karac_tracing_get_exporter_fn_fn: FunctionValue<'ctx>,
    /// Runtime extern: `karac_tracing_reset()` (phase-8 line 156). Clears
    /// the min level and registered sink; `Log.reset`'s body lowers to it.
    pub(crate) karac_tracing_reset_fn: FunctionValue<'ctx>,
    /// LLVM struct type for `ProviderFrame { prev, resource_id, data, vtable }`
    /// — `#[repr(C)]` matches `runtime/src/lib.rs::ProviderFrame`. Consumed
    /// at `with_provider[R]` lowering sites for the alloca'd frame storage
    /// (sub-step 3); declared here so the type is established alongside
    /// the runtime extern declarations.
    pub(crate) provider_frame_ty: StructType<'ctx>,
    /// LLVM struct type for `ProviderLookupResult { data, vtable }` —
    /// matches the runtime's `#[repr(C)]` shape. Used once at codegen
    /// init to type the `karac_provider_lookup` extern's return; after
    /// that the call's return type carries the shape implicitly so
    /// extractvalue at sub-step 4 dispatch sites doesn't need to look
    /// it up here. Field retained as ABI documentation for future
    /// readers and as the canonical anchor if `ProviderLookupResult`'s
    /// shape ever changes.
    #[allow(dead_code)]
    pub(crate) provider_lookup_result_ty: StructType<'ctx>,
    // ── Map runtime ───────────────────────────────────────────────
    /// Per-variable Map key LLVM type (variable name → K LLVM type).
    pub(crate) map_key_types: HashMap<String, BasicTypeEnum<'ctx>>,
    /// Per-variable Map value LLVM type (variable name → V LLVM type).
    pub(crate) map_val_types: HashMap<String, BasicTypeEnum<'ctx>>,
    /// Per-variable Map key type name string (e.g. "i64", "String") for hash/eq fn selection.
    pub(crate) map_key_type_names: HashMap<String, String>,
    /// Per-variable element-`TypeExpr` side-table for collection variables —
    /// the *element* of a Vec/Slice/Array, or the *value* of a Map. Used by
    /// `compile_for_*_var` so for-loop bindings inherit the right side-table
    /// registrations (`vec_elem_types`, `slice_elem_types`, `map_*_types`)
    /// when the element is itself a Vec/String/Slice/Map. Without this,
    /// LLVM-type-only tracking can't distinguish `Vec[String]` from
    /// `Vec[Vec[T]]` (both store `vec_struct_type` as the element LLVM type).
    pub(crate) var_elem_type_exprs: HashMap<String, TypeExpr>,
    /// Per-Map-variable key-`TypeExpr` side-table (parallels
    /// `var_elem_type_exprs` for the key slot). Used by `compile_for_map_var`
    /// to register the per-iteration `k` binding when iterating with a tuple
    /// pattern `for (k, v) in m`.
    pub(crate) map_key_type_exprs: HashMap<String, TypeExpr>,
    /// Per-variable Set element LLVM type (variable name → T LLVM type).
    /// Mirrors `map_key_types` — `Set[T]` lowers to `Map[T, ()]` at codegen,
    /// reusing the `karac_map_*` C runtime, but the surface type identity is
    /// kept distinct so codegen can pick the right method dispatch and the
    /// Display fn can pick the `Set{...}` brace style.
    pub(crate) set_elem_types: HashMap<String, BasicTypeEnum<'ctx>>,
    /// Per-variable Set element type name string (e.g. `"i64"`, `"String"`)
    /// for hash/eq fn selection. Mirrors `map_key_type_names`.
    pub(crate) set_elem_type_names: HashMap<String, String>,
    /// Per-variable Set element-`TypeExpr` side-table. Mirrors
    /// `map_key_type_exprs` and is consulted alongside it by Set-aware paths
    /// (`compile_for_set_var`, Set Display fn) so compound element types
    /// (`Set[(i64, String)]`, `Set[Vec[T]]`) compose through the
    /// TypeExpr-aware hash/eq/Display paths.
    pub(crate) set_elem_type_exprs: HashMap<String, TypeExpr>,
    /// Variables whose surface type is `String`. Disambiguates Strings from
    /// `Vec[u8]` at iteration time — both share the `{ptr, i64, i64}`
    /// physical layout and are both registered in `vec_elem_types` with
    /// element-LLVM-type `i8`, so the for-loop dispatcher otherwise can't
    /// tell which iteration shape to emit. `for c in s` and `for c in
    /// s.chars()` on a String iterate per Unicode scalar value via the
    /// `karac_string_decode_char` runtime helper; `for b in v` on a
    /// `Vec[u8]` iterates per byte. Populated alongside the existing
    /// `vec_elem_types` insertion at every String-registration site.
    pub(crate) string_vars: HashSet<String>,
    /// Variables whose surface type is `ref CStr` (the `c"..."` literal
    /// type — design.md § C-String Literals). Physically a `{ptr, i64}`
    /// slice-struct value: the NUL-terminated rodata pointer plus the
    /// source byte count (excluding the NUL), which is what makes `len()`
    /// O(1) per the design. Kept separate from `slice_elem_types` so the
    /// CStr method surface (`as_ptr` / `as_bytes` / `len` / `is_empty`,
    /// dispatched in `compile_cstr_method`) doesn't leak onto real
    /// slices and vice versa. Populated by the `let` RHS/annotation
    /// heuristics (stmts.rs) and `register_var_from_type_expr` (params).
    pub(crate) cstr_vars: HashSet<String>,
    /// HTTP handler ABI trampoline (2026-05-09): cache of per-handler-fn
    /// `extern "C"` shims. Key is the user handler's mangled fn name (e.g.
    /// `"handle"`); value is the synthesized shim function. Sharing the
    /// shim across multiple `Server.serve(handler)` calls in one program
    /// avoids redundant emission and keeps the IR stable. Pinned by
    /// `tests/codegen.rs::test_server_serve_handler_shim_caches`.
    pub(crate) http_shim_cache: HashMap<String, FunctionValue<'ctx>>,
    pub(crate) karac_map_new_fn: FunctionValue<'ctx>,
    pub(crate) karac_map_free_fn: FunctionValue<'ctx>,
    /// `karac_map_free_with_drop_vec(map: ptr, drop_key: i32, drop_val: i32)`
    /// — `karac_map_free` variant that recursively drops per-entry
    /// Vec/String content before deallocating the bucket storage.
    /// `drop_key != 0` releases each live entry's key data buffer when
    /// the key follows the `{ptr, len, cap}` layout; `drop_val != 0`
    /// does the same for the value. Selected by the `FreeMapHandle`
    /// cleanup arm whenever either flag is set. Replaces the narrower
    /// `karac_map_free_with_val_drop_vec` (val-only) helper that
    /// shipped 2026-05-13.
    ///
    /// Closes leaks for `Set[Vec[T]]` / `Set[String]` (key drop only),
    /// `Map[String, V]` / `Map[Vec[T], V]` (key drop only),
    /// `Map[String, Vec[U]]` / `Map[Vec[T], Vec[U]]` (both flags). The
    /// primitive-only `Map[i64, i64]` case stays on plain
    /// `karac_map_free` for zero overhead.
    pub(crate) karac_map_free_with_drop_vec_fn: FunctionValue<'ctx>,
    pub(crate) karac_map_insert_old_fn: FunctionValue<'ctx>,
    /// Borrowed-String-key insert: deep-copies the key only on a fresh
    /// insertion, so a slice-into-source key (`m.insert(s[a..b], v)`)
    /// allocates once per distinct key instead of once per call.
    pub(crate) karac_map_insert_borrowed_str_old_fn: FunctionValue<'ctx>,
    pub(crate) karac_map_get_fn: FunctionValue<'ctx>,
    pub(crate) karac_map_remove_old_fn: FunctionValue<'ctx>,
    pub(crate) karac_map_contains_fn: FunctionValue<'ctx>,
    pub(crate) karac_map_len_fn: FunctionValue<'ctx>,
    pub(crate) karac_map_clear_fn: FunctionValue<'ctx>,
    /// `karac_map_clear_with_drop_vec(map, drop_key, drop_val)` — clear that
    /// frees heap key/value buffers first (peer of
    /// `karac_map_free_with_drop_vec`); selected for heap-keyed/valued maps.
    pub(crate) karac_map_clear_with_drop_vec_fn: FunctionValue<'ctx>,
    pub(crate) karac_map_iter_new_fn: FunctionValue<'ctx>,
    pub(crate) karac_map_iter_next_fn: FunctionValue<'ctx>,
    pub(crate) karac_map_iter_free_fn: FunctionValue<'ctx>,
    /// `i64 karac_string_decode_char(*const u8 data, i64 len, i64 byte_offset, *mut u32 out_cp)`.
    /// Returns the byte offset after the decoded char and writes the
    /// codepoint through the out-param. Drives `for c in s` / `for c in
    /// s.chars()` lowering — see `compile_for_string_chars`.
    pub(crate) karac_string_decode_char_fn: FunctionValue<'ctx>,
    /// `i64 karac_string_encode_char(u32 cp, *mut u8 out)`. Writes 1–4
    /// UTF-8 bytes for the codepoint through `out`, returns the byte
    /// count. Peer of `karac_string_decode_char_fn`; used by the print
    /// and f-string char arms to render the glyph rather than the
    /// integer codepoint. See `emit_codepoint_to_utf8`.
    pub(crate) karac_string_encode_char_fn: FunctionValue<'ctx>,
    /// `karac_map_entry(map: ptr, key: ptr, out_slot_ptr: ptr) -> i1` —
    /// probe-and-insert-on-vacant. Used by entry chains whose terminal is
    /// `or_insert` / `or_insert_with` — codegen will write a default through
    /// the slot when occupied=false, so the runtime claims the bucket up
    /// front.
    pub(crate) karac_map_entry_fn: FunctionValue<'ctx>,
    /// `karac_map_lookup_slot(map: ptr, key: ptr, out_slot_ptr: ptr) -> i1`
    /// — read-only variant used by entry chains whose terminal is
    /// `and_modify`. The closure runs only when occupied=true; nothing is
    /// inserted on the Vacant path.
    pub(crate) karac_map_lookup_slot_fn: FunctionValue<'ctx>,
    /// `karac_string_clone(src: ptr, dst: ptr) -> void` — runtime helper
    /// for the codegen-emitted String case in `emit_clone_fn_for_type_expr`.
    /// Allocates a fresh buffer, copies len bytes, writes the new
    /// `{data, len, cap}` to `dst`. Static-literal sources (cap = 0) get
    /// a heap-owned copy so scope-exit cleanup fires; source untouched.
    pub(crate) karac_string_clone_fn: FunctionValue<'ctx>,
    pub(crate) karac_string_slice_fn: FunctionValue<'ctx>,
    /// `karac_string_slice_borrow(data, len, start, end) -> ptr` — validating,
    /// non-allocating slice; returns `data + start`. Backs borrowed
    /// `{ptr, len, cap=0}` String views used as non-retained map keys.
    pub(crate) karac_string_slice_borrow_fn: FunctionValue<'ctx>,
    /// Per-type clone function cache. Keyed on the canonical mangled type
    /// name (`display_mangle_te`). Each emitted fn has signature
    /// `void karac_clone_<typename>(*const T src, *mut T dst)` — caller
    /// provides both source and destination addresses, callee writes the
    /// cloned value into the destination slot. Mirror of `display_fn_cache`.
    pub(crate) clone_fn_cache: HashMap<String, FunctionValue<'ctx>>,
    /// Per-type *fallible* clone function cache. Keyed by the canonical
    /// type name (same scheme as `clone_fn_cache`). Each emitted fn has
    /// signature `i1 karac_try_clone_<typename>(*const T, *mut T, *mut i64)`:
    /// it clones `src` into `dst` using `karac_alloc_fallible`, returns
    /// `true` on success, or `false` on the first allocation failure after
    /// freeing any partially-cloned heap (so the caller leaks nothing) and
    /// storing the failed allocation's byte count through the third
    /// out-parameter. Backs `try_clone` codegen (phase-8-stdlib-floor item 8);
    /// mirror of `clone_fn_cache`. Map/Set element shapes are NOT emitted
    /// here — those need a fallible `karac_map_*` runtime API (item 8,
    /// `try_insert` blocker) and are rejected at the dispatch guard before
    /// any IR is emitted.
    pub(crate) try_clone_fn_cache: HashMap<String, FunctionValue<'ctx>>,
    /// Per-type Drop function cache. Keyed by the canonical type name
    /// (e.g. `"i64"`, `"String"`, `"Vec_i64"`, `"Map_String_i64"`). Each
    /// emitted fn has signature `void karac_drop_<typename>(*mut T)` and
    /// releases any heap owned by the value (for primitives: no-op; for
    /// String: free the data buffer if cap > 0; for Vec: per-element drop
    /// then free; for tuple: per-field drop; for Map/Set: delegate to the
    /// existing `karac_map_free*` runtime as a placeholder pending the
    /// monomorphized Map layout in Slice 1+). Mirror of `clone_fn_cache`.
    /// See [`wip-monomorphized-collections.md`](../docs/implementation_checklist/wip-monomorphized-collections.md) §3.3.
    ///
    /// `#[allow(dead_code)]` until Slice 1 lands the first production
    /// consumer (monomorphized `Map[i64, i64]` drop, per
    /// [`phase-7-codegen.md`](../docs/implementation_checklist/phase-7-codegen.md)
    /// "Monomorphized collections" entry). The framework is foundation;
    /// it has no production caller until the consumer lands.
    #[allow(dead_code)]
    pub(crate) drop_fn_cache: HashMap<String, FunctionValue<'ctx>>,
    /// Per-(K, V) cache of monomorphized `Map[K, V]` method symbols.
    /// Keyed by the mangled `"{key_mangle}_{val_mangle}"` token (e.g.
    /// `"i64_i64"`) produced by `mono_map_cache_key`. Lazily populated
    /// by `get_or_emit_map_mono_methods` on the first request for a
    /// given K/V tuple. Per-method `FunctionValue`s have `LinkOnceODR`
    /// linkage so cross-crate / cross-TU duplicates collapse at link
    /// time (locked design § 3.2). Slice 1 ships `Map[i64, i64]` only;
    /// the gating predicate `should_use_mono_map_for` returns `false`
    /// for every other K/V tuple, leaving them on the erased fallback
    /// per § 3.6.
    pub(crate) map_mono_methods: HashMap<String, MapMonoMethods<'ctx>>,
    /// Per-type Display function cache. Keyed by the canonical type name
    /// (e.g. `"i64"`, `"String"`, `"Vec_i64"`, `"Map_String_i64"`). Each
    /// emitted fn has signature `void karac_display_<typename>(ptr)` and
    /// writes characters to stdout via `printf` with no trailing newline.
    /// The pointer-by-reference convention is uniform across every type so
    /// callers don't need per-type calling conventions; primitives load the
    /// value, structs extract fields, opaque ptrs load the handle.
    ///
    /// `dead_code` is allowed because subtasks 1+2 of the Display canonical
    /// bullet ship the machinery + primitive Display fns ahead of subtasks
    /// 3-7 which add the callers (Vec/Map/Set/Tuple Display fns + the
    /// `compile_print` integration). Remove the allow when subtask 7 lands.
    #[allow(dead_code)]
    pub(crate) display_fn_cache: HashMap<String, FunctionValue<'ctx>>,
    // ── Error return trace runtime ────────────────────────────────
    /// `void karac_error_trace_push(ptr file, i64 file_len, i32 line, i32 col)`.
    /// Called by `compile_question` at each `?` failure block before
    /// `emit_scope_cleanup`. The runtime maintains a thread-local depth-64
    /// ring buffer; an atexit handler prints it to stderr at program exit.
    pub(crate) karac_error_trace_push_fn: FunctionValue<'ctx>,
    /// `void karac_error_trace_clear()`. Emitted at every `?` success site
    /// so a recovered earlier propagation doesn't leak frames into a later
    /// failure.
    pub(crate) karac_error_trace_clear_fn: FunctionValue<'ctx>,
    /// `void karac_test_record_failure(ptr file, i64 file_len, i32 line, i32 col,
    /// ptr msg, i64 msg_len, ptr left, i64 left_len, ptr right, i64 right_len)`.
    /// Lowered `assert` / `assert_eq` / `assert_ne` failure path calls this then
    /// `exit(1)`. The runtime writes a `KARAC_TEST_FAILURE {...JSON...}` line to
    /// stderr; `cmd_test` (Slice c.3) parses the line into a `TestOutcome`.
    pub(crate) karac_test_record_failure_fn: FunctionValue<'ctx>,
    /// Lazily-initialized `TargetData` consumed by the layout-introspection
    /// intrinsics (`align_of[T]()`, `offset_of[T](field)`). Constructed
    /// via `create_target_machine().get_target_data()` on first use; the
    /// rest of codegen never reads it. Held as `Option` because the
    /// host-target initialization pulls in `Target::initialize_native`,
    /// which we want to avoid in the (common) path where no layout
    /// intrinsic is invoked.
    pub(crate) target_data: Option<TargetData>,
    // ── Hot-swap codegen (phase-7 line 5) ─────────────────────────
    /// Set by `compile_to_*_with_hot_swap` from the CLI's
    /// `--enable-hot-swap` flag. When `true`, every call to a
    /// user-defined `pub fn` (extern-public module symbol) is emitted
    /// as a load-from-table + indirect-call shape so post-v1 reload
    /// can replace the table entry without recompiling callers. Off by
    /// default; the artifact-format reservation is per `deferred.md
    /// § Continuous PGO with Shared-Object Hot-Swap`.
    pub(crate) hot_swap_enabled: bool,
    /// Slice c-repl.B.4: free-fn names whose bodies should NOT be
    /// emitted in this module — only the LLVM `declare` (signature
    /// without body) is emitted, so the JIT resolves calls to these
    /// names against a previously-installed module in the same
    /// JITDylib. Used by `karac repl`'s cross-cell amortization
    /// pipeline so cell N+1 doesn't re-emit cell N's items. Empty
    /// in every other codegen entry point.
    pub(crate) declare_only_fns: std::collections::HashSet<String>,
    /// Slice c-repl.B.4: when `Some(name)`, the AST function whose
    /// `func.name == "main"` is registered in LLVM under `name`
    /// instead of the literal `main` symbol. The i32-return
    /// special-case still fires (so the runner's transmute to
    /// `unsafe extern "C" fn() -> i32` stays sound); only the
    /// emitted symbol changes. Used by the REPL JIT path so cell N's
    /// main and cell N+1's main don't collide in the same JITDylib.
    /// `None` everywhere else preserves the standalone-binary
    /// `int main(void)` shape.
    pub(crate) main_symbol_override: Option<String>,
    /// Force `External` linkage on every top-level user fn / impl method,
    /// independent of `main_symbol_override`. Used by the `karac test`
    /// persistent-module path: that module carries the Debugger-Contract
    /// globals (so it must NOT set `main_symbol_override`, which suppresses
    /// them) yet its functions must be cross-module-visible so the per-test
    /// `main` modules' declare-only references resolve to them in the shared
    /// JITDylib. `false` everywhere else preserves the pub/FFI-vs-Internal
    /// split (so the inliner can elide private fns in AOT / one-shot builds).
    pub(crate) force_external_linkage: bool,
    /// Slice c-repl.B.5.1: REPL value-snapshot capture set. Maps a
    /// top-level `let <name> = <expr>` binding name (where `<name>`
    /// is a single-binding pattern) to the primitive type its RHS
    /// evaluates to. Codegen emits the let body unchanged AND a
    /// post-bind store of the bound value to an LLVM global named
    /// `__karac_repl_snapshot_<name>`. Subsequent cells in the same
    /// REPL session can replay the value from that global rather
    /// than re-evaluating the original RHS — important when the RHS
    /// has side effects (`let log = read_file("big.json")` should
    /// not reread the file on every cell that uses `log`). Empty
    /// in every non-REPL codegen entry. Mutually exclusive with
    /// `snapshot_replay` per binding name (the parent assembles the
    /// two sets so they never overlap; replay always wins).
    pub(crate) snapshot_capture: HashMap<String, SnapshotPrimKind>,
    /// Slice c-repl.B.5.1: REPL value-snapshot replay set. Maps a
    /// top-level `let <name> = <expr>` binding name to its primitive
    /// type. When the codegen pass encounters such a binding, it
    /// SKIPS the original RHS, emits a load from the matching
    /// `__karac_repl_snapshot_<name>` global (declared as external
    /// in this module since the previous cell defined it), and
    /// binds the loaded value to the pattern. The synthetic source
    /// still carries the original `let <name> = <expr>` text — the
    /// resolver / typechecker need it to typecheck downstream uses
    /// — but codegen never lowers the original `<expr>`.
    pub(crate) snapshot_replay: HashMap<String, SnapshotPrimKind>,
    /// Per-pub-fn slot index in `@karac_hotswap_table`, populated as
    /// pub function declarations are emitted. The slot list is also
    /// kept ordered in `hot_swap_fns` so the module-init ctor can
    /// store function pointers in the matching order.
    pub(crate) hot_swap_slots: HashMap<String, u32>,
    /// Ordered list of `(slot_index, function_value)` for every
    /// pub-fn definition that received an indirection slot. The
    /// finalize step emits a ctor that writes each function's address
    /// into its slot in the table.
    pub(crate) hot_swap_fns: Vec<(u32, FunctionValue<'ctx>)>,
}

impl<'ctx> Codegen<'ctx> {
    fn new(context: &'ctx Context, module_name: &str) -> Self {
        let module = context.create_module(module_name);
        // Pin the module's data layout + triple to the actual target BEFORE any
        // type is built. Without this the module carries LLVM's *empty* default
        // data layout (`i64:32` packing), under which `llvm.coro.size.i64` folds
        // the coroutine frame's `sizeof` — but the AOT object backend lays the
        // frame out under the REAL target layout (`i64:64`, 8-byte alignment),
        // which is strictly larger. For a coro frame that ends in a small field
        // after a large one (e.g. the network handler's `[4096 x i8]` recv
        // buffer followed by the i2 suspend-index), the empty-layout size is up
        // to 8 bytes short, so `malloc(coro.size)` under-allocates and the
        // trailing suspend-index store lands one past the heap block — a
        // heap-buffer-overflow that glibc traps as `corrupted size vs.
        // prev_size` / `double free` (silent on macOS's allocator, caught by
        // ASAN/valgrind on any OS). Setting the layout makes `coro.size` and the
        // backend agree. Best-effort: if the target machine can't be created we
        // leave the default layout (non-coro modules are unaffected).
        if let Ok(tm) = create_target_machine() {
            module.set_triple(&tm.get_triple());
            module.set_data_layout(&tm.get_target_data().get_data_layout());
        }
        let builder = context.create_builder();

        let i32_type = context.i32_type();
        let i64_type = context.i64_type();
        let ptr_type = context.ptr_type(AddressSpace::default());
        let printf_type = i32_type.fn_type(&[BasicMetadataTypeEnum::from(ptr_type)], true);
        let printf_fn = module.add_function("printf", printf_type, None);

        // `int snprintf(char* buf, size_t n, const char* fmt, ...)` for f-string
        // integer/float → string conversion.
        let snprintf_type = i32_type.fn_type(
            &[
                BasicMetadataTypeEnum::from(ptr_type),
                BasicMetadataTypeEnum::from(i64_type),
                BasicMetadataTypeEnum::from(ptr_type),
            ],
            true,
        );
        let snprintf_fn = module.add_function("snprintf", snprintf_type, Some(Linkage::External));

        // `size_t fwrite(const void* ptr, size_t size, size_t nmemb, FILE* stream)`
        // — the NUL-safe print primitive (L5). `printf("%.*s")` truncates a
        // length-prefixed String at an interior NUL; `fwrite` writes exactly
        // `len` bytes. `FILE*` is opaque, so it lowers to `ptr`. `fwrite` is
        // NOT varargs, so its signature must match libc EXACTLY or wasm traps
        // the call (`signature_mismatch:fwrite`): `size_t` is i32 on wasm32
        // (wasi-libc) and i64 natively — `emit_nul_safe_write` normalizes the
        // length to this width at every call site. (Same size_t-width concern
        // the `malloc` shim comment below addresses.)
        let size_t_type = if crate::target::active_target_is_wasm() {
            i32_type
        } else {
            i64_type
        };
        let fwrite_type = size_t_type.fn_type(
            &[
                BasicMetadataTypeEnum::from(ptr_type),
                BasicMetadataTypeEnum::from(size_t_type),
                BasicMetadataTypeEnum::from(size_t_type),
                BasicMetadataTypeEnum::from(ptr_type),
            ],
            false,
        );
        let fwrite_fn = module.add_function("fwrite", fwrite_type, Some(Linkage::External));

        // The libc `FILE*` globals for stdout / stderr, used as the `fwrite`
        // stream. The symbol differs by platform: `__stdoutp` / `__stderrp` on
        // Apple, `stdout` / `stderr` on glibc and wasi-libc. The active target
        // (not the host) decides — a wasm cross-build wants the unprefixed
        // names even on an Apple host; a native build's target IS the host.
        let (stdout_sym, stderr_sym) = if crate::target::active_target_is_wasm() {
            ("stdout", "stderr")
        } else if cfg!(target_vendor = "apple") {
            ("__stdoutp", "__stderrp")
        } else {
            ("stdout", "stderr")
        };
        let stdout_global = module.add_global(ptr_type, None, stdout_sym);
        stdout_global.set_linkage(Linkage::External);
        let stderr_global = module.add_global(ptr_type, None, stderr_sym);
        stderr_global.set_linkage(Linkage::External);

        // Declare malloc and free for RC heap allocation. On wasm32 the
        // libc `malloc` takes `size_t` = i32, and wasm traps signature
        // mismatches at the call — so the wasm runtime archive exports a
        // 64-bit-size shim (`__karac_malloc64`, see
        // `runtime/src/wasm_alloc.rs`) and codegen declares THAT under the
        // i64 signature every call site already passes. `free(ptr)` needs
        // no shim: pointers lower to the right width per the datalayout.
        let malloc_type = ptr_type.fn_type(&[BasicMetadataTypeEnum::from(i64_type)], false);
        let malloc_fn = module.add_function(
            crate::codegen::driver::c_malloc_symbol(),
            malloc_type,
            Some(Linkage::External),
        );
        // Fallible / panicking allocation wrappers (phase-8-stdlib-floor item 8).
        // Same `ptr fn(i64)` signature as `malloc`. `karac_alloc_fallible`
        // returns null on OOM (the `try_*` companions branch on it);
        // `karac_alloc_or_panic` aborts on OOM (the panicking collection
        // methods route through it). On wasm these resolve to the i64 shims
        // (`__karac_alloc_*64`) rather than the `usize`-param runtime wrappers —
        // wasm32 `size_t` is i32 and a direct i64 call traps a signature
        // mismatch, exactly like `malloc` → `__karac_malloc64` (B-2026-06-12-1).
        let alloc_fallible_fn = module.add_function(
            crate::codegen::driver::c_alloc_fallible_symbol(),
            malloc_type,
            Some(Linkage::External),
        );
        let alloc_or_panic_fn = module.add_function(
            crate::codegen::driver::c_alloc_or_panic_symbol(),
            malloc_type,
            Some(Linkage::External),
        );
        let free_type = context
            .void_type()
            .fn_type(&[BasicMetadataTypeEnum::from(ptr_type)], false);
        let free_fn = module.add_function("free", free_type, Some(Linkage::External));

        let exit_type = context
            .void_type()
            .fn_type(&[BasicMetadataTypeEnum::from(i32_type)], false);
        let exit_fn = module.add_function("exit", exit_type, Some(Linkage::External));

        // Contract-predicate-context FFI (design.md § Contracts rule 2). The
        // enter/exit pair drives a thread-local depth counter in the runtime;
        // `karac_runtime_panic_prefix() -> *const c_char` returns the panic
        // message infix (`"contract predicate panicked: "` while a predicate is
        // on the stack, else `""`). See the field docs on `Codegen`.
        let pred_ctx_type = context.void_type().fn_type(&[], false);
        let karac_runtime_enter_predicate_fn = module.add_function(
            "karac_runtime_enter_predicate",
            pred_ctx_type,
            Some(Linkage::External),
        );
        let karac_runtime_exit_predicate_fn = module.add_function(
            "karac_runtime_exit_predicate",
            pred_ctx_type,
            Some(Linkage::External),
        );
        let panic_prefix_type = ptr_type.fn_type(&[], false);
        let karac_runtime_panic_prefix_fn = module.add_function(
            "karac_runtime_panic_prefix",
            panic_prefix_type,
            Some(Linkage::External),
        );

        let memcmp_type = i32_type.fn_type(
            &[
                BasicMetadataTypeEnum::from(ptr_type),
                BasicMetadataTypeEnum::from(ptr_type),
                BasicMetadataTypeEnum::from(i64_type),
            ],
            false,
        );
        let memcmp_fn = module.add_function("memcmp", memcmp_type, Some(Linkage::External));

        // `int sched_yield(void)` — POSIX cooperative thread yield. Wired
        // into the caller-side network-boundary intercept's Pending path
        // (phase 6 line 26 slice 8e) so the parent thread yields the
        // OS scheduler quantum between poll-fn invocations rather than
        // busy-looping. Linked from libc — same path as malloc / free.
        let sched_yield_type = i32_type.fn_type(&[], false);
        let sched_yield_fn =
            module.add_function("sched_yield", sched_yield_type, Some(Linkage::External));

        // Declare runtime types and entry points for par blocks.
        // `KaracBranch { func: ptr, ctx: ptr }` matches the #[repr(C)] struct in
        // runtime/src/lib.rs. karac_par_run(branches, count, spawn_site_id)
        // spawns one thread per branch and joins all before returning. The
        // `spawn_site_id` argument (Debugger Contract slice 4) identifies the
        // par site for `KaracFrame` metadata; the runtime ignores it when
        // `KARAC_RUNTIME_DEBUG_METADATA=0`.
        let karac_branch_ty = context.struct_type(&[ptr_type.into(), ptr_type.into()], false);
        let karac_par_run_type = context.void_type().fn_type(
            &[
                BasicMetadataTypeEnum::from(ptr_type),
                BasicMetadataTypeEnum::from(i64_type),
                BasicMetadataTypeEnum::from(i32_type),
                // parent_cancel: enclosing branch's cancel flag for the
                // nested-cancellation cascade (phase-6 line 475); null at the
                // top level. See `emit_par_run`'s call site.
                BasicMetadataTypeEnum::from(ptr_type),
            ],
            false,
        );
        let karac_par_run_fn =
            module.add_function("karac_par_run", karac_par_run_type, Some(Linkage::External));

        // Auto-par reduction-lowering runtime entry (slice 3a, 2026-05-19).
        // `karac_par_reduce(*const KaracReduceDescriptor, *mut u8 out_slot,
        // u32 spawn_site_id) -> ()` — see runtime/src/lib.rs's
        // `karac_par_reduce` doc-comment for the ABI shape. Declared
        // alongside `karac_par_run` so future slices (3b, the actual
        // lowering of recognized reductions into a fan-out + serial-combine
        // call) can route through `self.karac_par_reduce_fn` without
        // touching this declaration site again. The slice-3a wiring proves
        // the extern is linkable; slice 3b populates the call sites.
        let karac_par_reduce_type = context.void_type().fn_type(
            &[
                BasicMetadataTypeEnum::from(ptr_type), // descriptor: *const KaracReduceDescriptor
                BasicMetadataTypeEnum::from(ptr_type), // out_slot: *mut u8
                BasicMetadataTypeEnum::from(i32_type), // spawn_site_id: u32
            ],
            false,
        );
        let karac_par_reduce_fn = module.add_function(
            "karac_par_reduce",
            karac_par_reduce_type,
            Some(Linkage::External),
        );

        // ── Theme 6: provider stack ABI ──────────────────────────────────
        //
        // Mirrors `runtime/src/lib.rs::ProviderFrame` and
        // `ProviderLookupResult` `#[repr(C)]` layouts. `ProviderFrame` is
        // alloca'd at each `with_provider[R](...)` site; the runtime only
        // updates head pointers, so the storage shape needs to match the
        // runtime's reads but isn't owned by the runtime.
        let provider_frame_ty = context.struct_type(
            &[
                ptr_type.into(), // prev: *const ProviderFrame
                i32_type.into(), // resource_id: u32
                ptr_type.into(), // provider_data_ptr: *const u8
                ptr_type.into(), // vtable_ptr: *const VTable
            ],
            false,
        );
        let provider_lookup_result_ty = context.struct_type(
            &[
                ptr_type.into(), // data: *const u8
                ptr_type.into(), // vtable: *const VTable
            ],
            false,
        );
        let karac_provider_push_type = context.void_type().fn_type(
            &[
                ptr_type.into(), // frame: *mut ProviderFrame
                i32_type.into(), // resource_id: u32
                ptr_type.into(), // provider_data: *const u8
                ptr_type.into(), // vtable: *const VTable
            ],
            false,
        );
        let karac_provider_push_fn = module.add_function(
            "karac_provider_push",
            karac_provider_push_type,
            Some(Linkage::External),
        );
        let karac_provider_pop_type = context.void_type().fn_type(&[], false);
        let karac_provider_pop_fn = module.add_function(
            "karac_provider_pop",
            karac_provider_pop_type,
            Some(Linkage::External),
        );
        let karac_provider_lookup_type =
            provider_lookup_result_ty.fn_type(&[i32_type.into()], false);
        let karac_provider_lookup_fn = module.add_function(
            "karac_provider_lookup",
            karac_provider_lookup_type,
            Some(Linkage::External),
        );
        // Sub-step 5 (par-block inheritance): get/set the per-thread head
        // pointer so par-branch worker tasks can inherit the parent
        // thread's provider stack via the env-struct snapshot mechanism.
        let karac_provider_get_stack_head_type = ptr_type.fn_type(&[], false);
        let karac_provider_get_stack_head_fn = module.add_function(
            "karac_provider_get_stack_head",
            karac_provider_get_stack_head_type,
            Some(Linkage::External),
        );
        let karac_provider_set_stack_head_type =
            context.void_type().fn_type(&[ptr_type.into()], false);
        let karac_provider_set_stack_head_fn = module.add_function(
            "karac_provider_set_stack_head",
            karac_provider_set_stack_head_type,
            Some(Linkage::External),
        );
        // Phase-8 line 153 (active-span propagation): get/set the
        // per-thread active span id. `with_span` snapshots+installs+restores
        // it; `tracing_active_span()` reads it for `Log.*` auto-stamping;
        // par-branch prologues set it from the env-struct snapshot.
        let karac_tracing_get_active_span_type = i64_type.fn_type(&[], false);
        let karac_tracing_get_active_span_fn = module.add_function(
            "karac_tracing_get_active_span",
            karac_tracing_get_active_span_type,
            Some(Linkage::External),
        );
        let karac_tracing_set_active_span_type =
            context.void_type().fn_type(&[i64_type.into()], false);
        let karac_tracing_set_active_span_fn = module.add_function(
            "karac_tracing_set_active_span",
            karac_tracing_set_active_span_type,
            Some(Linkage::External),
        );
        // Phase-8 line 156 (configurable ambient exporter, codegen half):
        // the process-global min-level + registered-sink accessors. All
        // unconditional externs (present in lean + full archives) — safe
        // for programs that never touch `std.tracing`, exactly like the
        // active-span pair above. `tracing_level_enabled` reads the level;
        // `tracing_set_min_level`/`tracing_reset` write config; `set_exporter`
        // registers a sink; `tracing_emit_event` reads the sink pair.
        let karac_tracing_get_min_level_type = i64_type.fn_type(&[], false);
        let karac_tracing_get_min_level_fn = module.add_function(
            "karac_tracing_get_min_level",
            karac_tracing_get_min_level_type,
            Some(Linkage::External),
        );
        let karac_tracing_set_min_level_type =
            context.void_type().fn_type(&[i64_type.into()], false);
        let karac_tracing_set_min_level_fn = module.add_function(
            "karac_tracing_set_min_level",
            karac_tracing_set_min_level_type,
            Some(Linkage::External),
        );
        let karac_tracing_set_exporter_type = context
            .void_type()
            .fn_type(&[ptr_type.into(), ptr_type.into()], false);
        let karac_tracing_set_exporter_fn = module.add_function(
            "karac_tracing_set_exporter",
            karac_tracing_set_exporter_type,
            Some(Linkage::External),
        );
        let karac_tracing_get_exporter_data_type = ptr_type.fn_type(&[], false);
        let karac_tracing_get_exporter_data_fn = module.add_function(
            "karac_tracing_get_exporter_data",
            karac_tracing_get_exporter_data_type,
            Some(Linkage::External),
        );
        let karac_tracing_get_exporter_fn_type = ptr_type.fn_type(&[], false);
        let karac_tracing_get_exporter_fn_fn = module.add_function(
            "karac_tracing_get_exporter_fn",
            karac_tracing_get_exporter_fn_type,
            Some(Linkage::External),
        );
        let karac_tracing_reset_type = context.void_type().fn_type(&[], false);
        let karac_tracing_reset_fn = module.add_function(
            "karac_tracing_reset",
            karac_tracing_reset_type,
            Some(Linkage::External),
        );

        // ── Debugger Contract slice 5: `std.runtime` introspection ──
        //
        // Two extern declarations consumed by `compile_assoc_call`'s
        // arms for `Runtime.has_debug_metadata()` and
        // `Runtime.list_par_blocks()`. The third API
        // (`Runtime.list_tasks()`) always returns the empty Vec value
        // in v1, so it has no runtime-side dispatch — the call lowers
        // to a `Vec.new()`-shaped value directly.
        //
        // `karac_runtime_has_debug_metadata() -> bool` reads the
        // `KARAC_SPAWN_SITES_ENABLED` global emitted by slice 3.
        // `karac_runtime_list_par_blocks_into(out: *mut KaracVec)` writes
        // a freshly-materialized `Vec[ParBlockInfo]` `{data, len, cap}`
        // descriptor into the slot at `out`. Slice 5 takes the
        // runtime-side full Vec materialization path (hard-stop trigger 3
        // fallback) — runtime knows Kāra Vec + String layout (already
        // documented in `clone.rs::karac_string_clone`) and the
        // `KaracParBlockInfo` `#[repr(C)]` matches what user-side codegen
        // would produce for the baked-stdlib `ParBlockInfo` struct.
        let karac_runtime_has_debug_metadata_type = context.bool_type().fn_type(&[], false);
        let _karac_runtime_has_debug_metadata_fn = module.add_function(
            "karac_runtime_has_debug_metadata",
            karac_runtime_has_debug_metadata_type,
            Some(Linkage::External),
        );
        let karac_runtime_list_par_blocks_into_type = context
            .void_type()
            .fn_type(&[BasicMetadataTypeEnum::from(ptr_type)], false);
        let _karac_runtime_list_par_blocks_into_fn = module.add_function(
            "karac_runtime_list_par_blocks_into",
            karac_runtime_list_par_blocks_into_type,
            Some(Linkage::External),
        );

        // ── Slice B: HTTP server FFI surface (minimal `std.http`) ────
        //
        // `karac_runtime_serve_http_static(addr: *const c_char, body: *const u8,
        // body_len: usize) -> i32` — bind on `addr`, serve every
        // request with a 200/JSON response carrying `body` as the
        // payload. v1's smoke handler. Real handler-fn-ptr dispatch
        // lands in a follow-up; see the Slice B close-out under
        // `wip-list1.md`.
        //
        // Return code: 0 on graceful shutdown (currently unreachable —
        // the accept loop runs forever); non-zero on bind / runtime-
        // construction failure. Codegen translates the return into a
        // `Result[Unit, HttpError]` per the Kāra-side
        // `Server.serve_static` signature.
        let karac_runtime_serve_http_static_type = context
            .i32_type()
            .fn_type(&[ptr_type.into(), ptr_type.into(), i64_type.into()], false);
        let _karac_runtime_serve_http_static_fn = module.add_function(
            "karac_runtime_serve_http_static",
            karac_runtime_serve_http_static_type,
            Some(Linkage::External),
        );

        // Slice B follow-up (2026-05-09): full handler-dispatch entry.
        // `karac_runtime_serve_http(addr_cstr: *const c_char, handler:
        // extern "C" fn(*const KaracHttpRequest, *mut KaracHttpResponse),
        // bound_port_out: *mut u16) -> i32`. v1 always passes a null
        // `bound_port_out` — the smoke test reads the port from the
        // runtime's `BOUND_PORT=<n>\n` stdout line per Slice B's
        // convention. The handler is a free-fn pointer (sub-step (b) of
        // the Slice B follow-up); closures with captures are rejected
        // upstream with `E_CLOSURE_AS_FN_PTR_NOT_YET`. The LLVM
        // function-pointer types of the user handler and this extern's
        // `handler` slot don't have to match structurally — LLVM
        // function-pointer-typed parameters are just `ptr`s at the
        // indirect-call boundary; the runtime invokes the handler with
        // the documented `extern "C"` signature regardless of the user
        // handler's lowered Kāra signature.
        let karac_runtime_serve_http_type = context.i32_type().fn_type(
            &[
                ptr_type.into(), // addr_cstr
                ptr_type.into(), // handler fn-ptr (just `ptr` at LLVM level)
                ptr_type.into(), // bound_port_out
            ],
            false,
        );
        let _karac_runtime_serve_http_fn = module.add_function(
            "karac_runtime_serve_http",
            karac_runtime_serve_http_type,
            Some(Linkage::External),
        );
        // HTTPS variant: same shape as `karac_runtime_serve_http` plus
        // inline cert + key PEM byte slices (`ptr` + `i64` len each).
        // Terminates TLS via `tokio_rustls::TlsAcceptor` in front of
        // hyper. Backs `Server.serve_tls(addr, cert_pem, key_pem,
        // handler)`.
        let karac_runtime_serve_https_type = context.i32_type().fn_type(
            &[
                ptr_type.into(), // addr_cstr
                ptr_type.into(), // cert_pem
                i64_type.into(), // cert_len
                ptr_type.into(), // key_pem
                i64_type.into(), // key_len
                ptr_type.into(), // handler fn-ptr
                ptr_type.into(), // bound_port_out
            ],
            false,
        );
        let _karac_runtime_serve_https_fn = module.add_function(
            "karac_runtime_serve_https",
            karac_runtime_serve_https_type,
            Some(Linkage::External),
        );

        // HTTP handler ABI trampoline (2026-05-09): per-request runtime
        // externs invoked from the Kāra-side `Request.path()` / `.method()`
        // methods and from the per-handler shim that decomposes the user-
        // returned `Response` into the FFI `KaracHttpResponse` slot.
        //
        // - `karac_runtime_http_request_path(*const KaracHttpRequest)
        //    -> *const c_char` — null-terminated UTF-8 path. Lifetime tied
        //   to the request struct (dropped after the handler returns), so
        //   `Request.path()` copies into a fresh Kāra String per call (F2).
        // - `karac_runtime_http_request_method(...)` — same shape, returns
        //   the HTTP method verb.
        // - `karac_runtime_http_response_set_status(*mut KaracHttpResponse,
        //    u16)` — write the status code.
        // - `karac_runtime_http_response_set_body(*mut KaracHttpResponse,
        //    *const u8, usize)` — copy a byte buffer into a fresh
        //   runtime-owned response body.
        let request_path_type = ptr_type.fn_type(&[ptr_type.into()], false);
        let _karac_runtime_http_request_path_fn = module.add_function(
            "karac_runtime_http_request_path",
            request_path_type,
            Some(Linkage::External),
        );
        let request_method_type = ptr_type.fn_type(&[ptr_type.into()], false);
        let _karac_runtime_http_request_method_fn = module.add_function(
            "karac_runtime_http_request_method",
            request_method_type,
            Some(Linkage::External),
        );
        // `karac_runtime_http_request_body_ptr` returns the raw byte
        // pointer (not null-terminated); paired with
        // `karac_runtime_http_request_body_len`, the Kāra-side
        // `Request.body()` allocates a fresh String per call so the
        // resulting value owns its buffer beyond the request lifetime.
        let request_body_ptr_type = ptr_type.fn_type(&[ptr_type.into()], false);
        let _karac_runtime_http_request_body_ptr_fn = module.add_function(
            "karac_runtime_http_request_body_ptr",
            request_body_ptr_type,
            Some(Linkage::External),
        );
        let request_body_len_type = context.i64_type().fn_type(&[ptr_type.into()], false);
        let _karac_runtime_http_request_body_len_fn = module.add_function(
            "karac_runtime_http_request_body_len",
            request_body_len_type,
            Some(Linkage::External),
        );
        // `karac_runtime_http_request_header(*const KaracHttpRequest,
        //  *const u8, usize) -> *const c_char` — case-insensitive header
        // lookup. Returns null on miss; on hit, returns a runtime-owned
        // null-terminated UTF-8 pointer (valid for the duration of the
        // handler call). `Request.header(name)` copies the bytes into a
        // fresh Kāra String per call so the resulting `Option[String]`
        // outlives the request struct.
        let request_header_type =
            ptr_type.fn_type(&[ptr_type.into(), ptr_type.into(), i64_type.into()], false);
        let _karac_runtime_http_request_header_fn = module.add_function(
            "karac_runtime_http_request_header",
            request_header_type,
            Some(Linkage::External),
        );
        // Full-map iteration surface for `Request.headers()` /
        // `Request.query()` (Phase 8 line 13). Both return
        // `Vec[(String, String)]`; codegen's `compile_request_pairs`
        // drives a counted loop over these indexed accessors, copying
        // each borrowed `*const c_char` into a fresh owned Kāra String
        // (same per-call ownership contract as `header(name)`):
        //   `*_count(*const KaracHttpRequest) -> usize` (loop bound)
        //   `*_key_at` / `*_val_at(*const KaracHttpRequest, usize idx)
        //    -> *const c_char` (null on out-of-range; runtime-owned on
        //    hit). Query keys/vals are percent-decoded runtime-side.
        let request_count_type = i64_type.fn_type(&[ptr_type.into()], false);
        let request_at_type = ptr_type.fn_type(&[ptr_type.into(), i64_type.into()], false);
        for name in [
            "karac_runtime_http_request_headers_count",
            "karac_runtime_http_request_query_count",
        ] {
            module.add_function(name, request_count_type, Some(Linkage::External));
        }
        for name in [
            "karac_runtime_http_request_header_key_at",
            "karac_runtime_http_request_header_val_at",
            "karac_runtime_http_request_query_key_at",
            "karac_runtime_http_request_query_val_at",
        ] {
            module.add_function(name, request_at_type, Some(Linkage::External));
        }
        // Phase 8 `File` handle slice F3/F4: extern declarations for
        // the `runtime/src/file.rs` ABI surface. Each open/read/write/
        // flush entry point writes its `KaracIoResult` (32 bytes; see
        // `runtime/src/file.rs::KaracIoResult`) into a caller-provided
        // `*mut KaracIoResult` out-param rather than returning the
        // struct by value. The struct exceeds the 16-byte register-
        // return threshold on every supported target (System V x86_64
        // / AAPCS AArch64), so a by-value return would route through
        // sret-via-hidden-pointer (SystemV) or x8-indirect (AAPCS) —
        // platform-specific ABIs that require matching `sret` / `byval`
        // attributes on the LLVM side. F4's first cut tried the
        // by-value shape and produced a corrupted-stack hang at the
        // call boundary. The out-param shape is ABI-trivial. F4's
        // method codegen allocas a stack slot, passes its address as
        // the first arg, then loads the result fields from the slot.
        let file_call_void_type = context.void_type();
        // Open-family: (out: *mut KaracIoResult, path_ptr: *const u8, path_len: i64) -> void.
        let file_open_type = file_call_void_type
            .fn_type(&[ptr_type.into(), ptr_type.into(), i64_type.into()], false);
        for sym in [
            "karac_runtime_file_open",
            "karac_runtime_file_create",
            "karac_runtime_file_append",
            // `FileSystem.read_to_string(path)` — same (out, path_ptr,
            // path_len) shape as the open-family; returns its String
            // payload through the KaracIoResult buffer fields.
            "karac_runtime_file_read_to_string",
        ] {
            module.add_function(sym, file_open_type, Some(Linkage::External));
        }
        // Read / write: (out, handle, buf_ptr, buf_len) -> void.
        let file_rw_type = file_call_void_type.fn_type(
            &[
                ptr_type.into(),
                ptr_type.into(),
                ptr_type.into(),
                i64_type.into(),
            ],
            false,
        );
        for sym in ["karac_runtime_file_read", "karac_runtime_file_write"] {
            module.add_function(sym, file_rw_type, Some(Linkage::External));
        }
        // `FileSystem.write(path, contents)` — one-shot whole-file write:
        // (out, path_ptr, path_len, contents_ptr, contents_len) -> void.
        // No live handle (open+write+close in one runtime call), Unit Ok
        // payload. L646 slice 4.
        let fs_write_type = file_call_void_type.fn_type(
            &[
                ptr_type.into(),
                ptr_type.into(),
                i64_type.into(),
                ptr_type.into(),
                i64_type.into(),
            ],
            false,
        );
        module.add_function(
            "karac_runtime_fs_write",
            fs_write_type,
            Some(Linkage::External),
        );
        // Flush: (out, handle) -> void.
        let file_flush_type =
            file_call_void_type.fn_type(&[ptr_type.into(), ptr_type.into()], false);
        module.add_function(
            "karac_runtime_file_flush",
            file_flush_type,
            Some(Linkage::External),
        );
        // Close: (handle) -> void. Called by F4b's FreeFileHandle
        // cleanup action at scope exit.
        let file_close_type = context.void_type().fn_type(&[ptr_type.into()], false);
        module.add_function(
            "karac_runtime_file_close",
            file_close_type,
            Some(Linkage::External),
        );

        // `karac_runtime_parse_i64(data: *const u8, len: usize, out: *mut i64) -> u8`.
        // Returns 1 on success (with the parsed value at `*out`), 0 on
        // failure. Backs `i64.parse(s: String) -> Option[i64]` and the
        // narrower integer-type parse methods (which all currently lower
        // to i64 at the Value layer).
        let parse_i64_type = context
            .i8_type()
            .fn_type(&[ptr_type.into(), i64_type.into(), ptr_type.into()], false);
        let _karac_runtime_parse_i64_fn = module.add_function(
            "karac_runtime_parse_i64",
            parse_i64_type,
            Some(Linkage::External),
        );
        // `karac_runtime_parse_i64_radix(data: *const u8, len: usize,
        //  radix: u32, out: *mut i64) -> u8`. Backs `i64.from_str_radix(s,
        //  radix)` — the self-hosting lexer's hex/binary/octal literal path.
        let parse_i64_radix_type = context.i8_type().fn_type(
            &[
                ptr_type.into(),
                i64_type.into(),
                context.i32_type().into(),
                ptr_type.into(),
            ],
            false,
        );
        let _karac_runtime_parse_i64_radix_fn = module.add_function(
            "karac_runtime_parse_i64_radix",
            parse_i64_radix_type,
            Some(Linkage::External),
        );
        // `karac_runtime_parse_f64(data: *const u8, len: usize, out: *mut f64)
        //  -> u8`. Backs `f64.parse(s) -> Option[f64]` — the self-hosting
        //  lexer's float-literal path.
        let parse_f64_type = context
            .i8_type()
            .fn_type(&[ptr_type.into(), i64_type.into(), ptr_type.into()], false);
        let _karac_runtime_parse_f64_fn = module.add_function(
            "karac_runtime_parse_f64",
            parse_f64_type,
            Some(Linkage::External),
        );
        // `karac_runtime_cstr_to_string(data: *const u8, len: usize,
        //  out_str: *mut String, out_err: *mut u8) -> bool`. Backs
        //  `CStr.to_string() -> Result[String, Utf8Error]` (phase-12 Cluster 2):
        //  validates UTF-8, writes a heap String into `out_str` on Ok, or the
        //  Utf8Error variant tag into `out_err` on Err. Codegen builds the
        //  Result enum from the discriminant (enum-layout knowledge stays here).
        let cstr_to_string_type = context.bool_type().fn_type(
            &[
                ptr_type.into(),
                i64_type.into(),
                ptr_type.into(),
                ptr_type.into(),
            ],
            false,
        );
        let _karac_runtime_cstr_to_string_fn = module.add_function(
            "karac_runtime_cstr_to_string",
            cstr_to_string_type,
            Some(Linkage::External),
        );
        let response_set_status_type = context
            .void_type()
            .fn_type(&[ptr_type.into(), context.i16_type().into()], false);
        let _karac_runtime_http_response_set_status_fn = module.add_function(
            "karac_runtime_http_response_set_status",
            response_set_status_type,
            Some(Linkage::External),
        );
        let response_set_body_type = context
            .void_type()
            .fn_type(&[ptr_type.into(), ptr_type.into(), i64_type.into()], false);
        let _karac_runtime_http_response_set_body_fn = module.add_function(
            "karac_runtime_http_response_set_body",
            response_set_body_type,
            Some(Linkage::External),
        );
        // Phase-8 line 14 — `karac_runtime_http_response_set_header(
        //   *mut KaracHttpResponse, *const u8 key, usize key_len,
        //   *const u8 val, usize val_len) -> void`. Called per
        // `(key, value)` pair from the handler shim when the user's
        // `Response` carries a third `headers: Vec[(String, String)]`
        // field. The runtime accumulates these into a thread-local
        // staged Vec that `serve_request` drains into hyper's response
        // builder after the handler returns.
        let response_set_header_type = context.void_type().fn_type(
            &[
                ptr_type.into(), // *mut KaracHttpResponse
                ptr_type.into(), // key_ptr
                i64_type.into(), // key_len
                ptr_type.into(), // val_ptr
                i64_type.into(), // val_len
            ],
            false,
        );
        let _karac_runtime_http_response_set_header_fn = module.add_function(
            "karac_runtime_http_response_set_header",
            response_set_header_type,
            Some(Linkage::External),
        );
        // Phase-8 line 17 slice 2 — `karac_runtime_http_client_get(
        //   url_ptr, url_len, *mut i64 status, *mut *mut u8 body_ptr,
        //   *mut i64 body_len, *mut *mut u8 err_ptr, *mut i64 err_len)
        //   -> void`. Backs compiled-mode `Client.get(url)`. Out-params
        // resolve to Result[Response, HttpError] via status > 0 vs
        // status == 0 discrimination; ownership of body_ptr / err_ptr
        // transfers to the caller (libc::malloc'd, freed via the Kāra
        // String's Drop → C `free(data)`).
        let http_client_get_type = context.void_type().fn_type(
            &[
                ptr_type.into(), // url_ptr
                i64_type.into(), // url_len
                ptr_type.into(), // out_status: *mut i64
                ptr_type.into(), // out_body_ptr: *mut *mut u8
                ptr_type.into(), // out_body_len: *mut i64
                ptr_type.into(), // out_err_ptr: *mut *mut u8
                ptr_type.into(), // out_err_len: *mut i64
                ptr_type.into(), // out_headers_handle: *mut i64 (phase-8 line 39)
            ],
            false,
        );
        let _karac_runtime_http_client_get_fn = module.add_function(
            "karac_runtime_http_client_get",
            http_client_get_type,
            Some(Linkage::External),
        );
        // POST variant — same out-param shape with two additional
        // input params for the request body (`body_ptr`, `body_len`).
        let http_client_post_type = context.void_type().fn_type(
            &[
                ptr_type.into(), // url_ptr
                i64_type.into(), // url_len
                ptr_type.into(), // body_ptr
                i64_type.into(), // body_len
                ptr_type.into(), // out_status
                ptr_type.into(), // out_body_ptr
                ptr_type.into(), // out_body_len
                ptr_type.into(), // out_err_ptr
                ptr_type.into(), // out_err_len
                ptr_type.into(), // out_headers_handle: *mut i64 (phase-8 line 39)
            ],
            false,
        );
        let _karac_runtime_http_client_post_fn = module.add_function(
            "karac_runtime_http_client_post",
            http_client_post_type,
            Some(Linkage::External),
        );
        // Phase-8 line 39 — `karac_runtime_http_response_header(i64 handle,
        //   *const u8 name, usize name_len) -> *const c_char`. Looks up a
        // response header (case-insensitive, RFC 7230 §3.2) in the
        // runtime's `HTTP_RESPONSE_HEADERS` side-table keyed by the handle
        // the client FFI minted into `Response`'s hidden `headers` field.
        // Returns null on miss / unknown handle; on hit returns a
        // runtime-owned null-terminated pointer that `compile_response_header`
        // copies into a fresh Kāra String wrapped in `Option[String]`.
        // Response-side mirror of `karac_runtime_http_request_header`.
        let response_header_type =
            ptr_type.fn_type(&[i64_type.into(), ptr_type.into(), i64_type.into()], false);
        let _karac_runtime_http_response_header_fn = module.add_function(
            "karac_runtime_http_response_header",
            response_header_type,
            Some(Linkage::External),
        );
        // Phase-8 line 39 follow-up — `Response.headers()` full-map
        // iteration accessors over the same side-table handle:
        //   `_response_headers_count(i64 handle) -> i64` (loop bound)
        //   `_response_header_{key,val}_at(i64 handle, i64 idx)
        //    -> *const c_char` (null on unknown handle / OOR; runtime-owned
        //    on hit). `compile_response_pairs` drives a counted loop over
        //    these, copying each borrowed cstring into a fresh owned String.
        let response_headers_count_type = i64_type.fn_type(&[i64_type.into()], false);
        let _karac_runtime_http_response_headers_count_fn = module.add_function(
            "karac_runtime_http_response_headers_count",
            response_headers_count_type,
            Some(Linkage::External),
        );
        let response_header_at_type = ptr_type.fn_type(&[i64_type.into(), i64_type.into()], false);
        for name in [
            "karac_runtime_http_response_header_key_at",
            "karac_runtime_http_response_header_val_at",
        ] {
            module.add_function(name, response_header_at_type, Some(Linkage::External));
        }
        // Phase-8 line 39 follow-up — `karac_runtime_http_response_headers_free(
        //   i64 handle) -> void`. Releases the side-table entry; called by
        // the synthesized `Response` Drop at scope exit (see
        // `emit_struct_drop_synthesis`'s `HttpHandleFree` field arm).
        // Idempotent: handle 0 / unknown is a no-op, so a move-suppressed
        // (zeroed) handle or a double-drop is harmless.
        let response_headers_free_type = context.void_type().fn_type(&[i64_type.into()], false);
        let _karac_runtime_http_response_headers_free_fn = module.add_function(
            "karac_runtime_http_response_headers_free",
            response_headers_free_type,
            Some(Linkage::External),
        );
        // Phase-8 line 24 — chained-builder request descriptor FFI.
        // `_builder_new(method_ptr, method_len, url_ptr, url_len) ->
        // i64`. Returns an opaque positive handle indexing the
        // runtime-side `HTTP_BUILDERS` map; `0` signals an alloc
        // failure (downstream `_builder_*` calls treat it as a
        // missing entry and degrade gracefully).
        let http_builder_new_type = i64_type.fn_type(
            &[
                ptr_type.into(), // method_ptr
                i64_type.into(), // method_len
                ptr_type.into(), // url_ptr
                i64_type.into(), // url_len
            ],
            false,
        );
        module.add_function(
            "karac_runtime_http_builder_new",
            http_builder_new_type,
            Some(Linkage::External),
        );
        // `_builder_add_header(handle, key_ptr, key_len, val_ptr,
        // val_len) -> void`. Appends to the builder's header list;
        // unknown handle is a no-op.
        let http_builder_add_header_type = context.void_type().fn_type(
            &[
                i64_type.into(), // handle
                ptr_type.into(), // key_ptr
                i64_type.into(), // key_len
                ptr_type.into(), // val_ptr
                i64_type.into(), // val_len
            ],
            false,
        );
        module.add_function(
            "karac_runtime_http_builder_add_header",
            http_builder_add_header_type,
            Some(Linkage::External),
        );
        // `_builder_set_body(handle, body_ptr, body_len) -> void`.
        // Replaces the builder's request body bytes.
        let http_builder_set_body_type = context.void_type().fn_type(
            &[
                i64_type.into(), // handle
                ptr_type.into(), // body_ptr
                i64_type.into(), // body_len
            ],
            false,
        );
        module.add_function(
            "karac_runtime_http_builder_set_body",
            http_builder_set_body_type,
            Some(Linkage::External),
        );
        // `_builder_set_timeout(handle, ms) -> void`. Configures the
        // builder's request deadline; `ms <= 0` disables timeout.
        let http_builder_set_timeout_type = context
            .void_type()
            .fn_type(&[i64_type.into(), i64_type.into()], false);
        module.add_function(
            "karac_runtime_http_builder_set_timeout",
            http_builder_set_timeout_type,
            Some(Linkage::External),
        );
        // Phase-8 line 39 follow-up — `karac_runtime_http_builder_free(
        //   i64 handle) -> void`. Releases the `HTTP_BUILDERS` entry;
        // called by the synthesized `RequestBuilder` Drop at scope exit so
        // an abandoned (never-sent) builder doesn't leak. Idempotent on
        // 0 / unknown / already-sent.
        let http_builder_free_type = context.void_type().fn_type(&[i64_type.into()], false);
        module.add_function(
            "karac_runtime_http_builder_free",
            http_builder_free_type,
            Some(Linkage::External),
        );
        // `_builder_send(handle, *mut i64 status, *mut *mut u8 body_ptr,
        //   *mut i64 body_len, *mut *mut u8 err_ptr, *mut i64 err_len)
        //   -> void`. Drives the configured request and drops the
        // entry. Same out-param shape and Ok/Err discriminant as
        // `_client_get`.
        let http_builder_send_type = context.void_type().fn_type(
            &[
                i64_type.into(), // handle
                ptr_type.into(), // out_status
                ptr_type.into(), // out_body_ptr
                ptr_type.into(), // out_body_len
                ptr_type.into(), // out_err_ptr
                ptr_type.into(), // out_err_len
                ptr_type.into(), // out_headers_handle: *mut i64 (phase-8 line 39)
            ],
            false,
        );
        module.add_function(
            "karac_runtime_http_builder_send",
            http_builder_send_type,
            Some(Linkage::External),
        );
        let strlen_type = i64_type.fn_type(&[ptr_type.into()], false);
        if module.get_function("strlen").is_none() {
            module.add_function("strlen", strlen_type, Some(Linkage::External));
        }

        // ── Phase 6 line 17 slice 6 — network-event-loop FFI surface ──────
        //
        // Externs consumed by the `karac_park_on_fd` leaf-primitive poll
        // function (emitted in `emit_state_machine_poll_fn_for_key`'s
        // special-case branch). The runtime side lives in
        // `runtime/src/event_loop.rs`; signatures pinned by the
        // `karac_park_on_fd_*_signature_pinned` tests in the codegen
        // module test block.
        let register_fd_ty = i64_type.fn_type(
            &[
                context.i32_type().into(), // raw_fd
                context.i8_type().into(),  // direction (0=Read, 1=Write, 2=ReadWrite)
                ptr_type.into(),           // parked task pointer (opaque)
            ],
            false,
        );
        module.add_function(
            "karac_runtime_event_loop_register_fd",
            register_fd_ty,
            Some(Linkage::External),
        );
        // `register_fd_cancel(raw_fd, dir, parked, cancel) -> token` — the
        // coroutine park-suspend variant that binds a per-task cancel flag on
        // the registration (slice 5c), so the dispatcher / cancel-sweep hand
        // the coroutine its own cooperative-cancellation flag.
        let register_fd_cancel_ty = i64_type.fn_type(
            &[
                context.i32_type().into(), // raw_fd
                context.i8_type().into(),  // direction
                ptr_type.into(),           // parked task pointer (opaque)
                ptr_type.into(),           // cancel: *const AtomicBool (null = none)
            ],
            false,
        );
        module.add_function(
            "karac_runtime_event_loop_register_fd_cancel",
            register_fd_cancel_ty,
            Some(Linkage::External),
        );
        // `take_wakeups(out_buf, max_wakeups, timeout_nanos) -> count`.
        // `karac_park_on_fd`'s state_1 calls this with `timeout_nanos = -1`
        // to block until any wakeup arrives. v1 single-task model: the
        // only fd registered is ours, so any wakeup is ours.
        let take_wakeups_ty = i64_type.fn_type(
            &[
                ptr_type.into(), // out_buf: *mut KaracWakeup
                i64_type.into(), // max_wakeups
                i64_type.into(), // timeout_nanos (-1 = block)
            ],
            false,
        );
        module.add_function(
            "karac_runtime_event_loop_take_wakeups",
            take_wakeups_ty,
            Some(Linkage::External),
        );
        // Idempotent bootstrap. `karac_park_on_fd`'s state_0 calls this
        // before `register_fd` so the background poller is guaranteed to
        // be running before any fd is registered. The runtime is the
        // authority on idempotency — second call returns 0 without
        // re-spawning.
        let start_bg_ty = context.i32_type().fn_type(&[], false);
        module.add_function(
            "karac_runtime_event_loop_start_background_thread",
            start_bg_ty,
            Some(Linkage::External),
        );

        // ── Async-scheduler integration (Phase 6 line 170 slice 2/3) ──────
        //
        // The dispatcher-yield model: the leaf park's `state_0` registers
        // the fd and returns Pending; the *caller* blocks on a per-park
        // `KaracParkSlot`; the *dispatcher* thread runs `state_1` when the
        // fd actually fires (routed by the wakeup's `parked` pointer) and
        // signals that slot. This replaces the pre-slice-2 model where
        // `state_1` blocked on the unfiltered global `take_wakeups` — two
        // concurrently-parked tasks stole each other's wakeups (the
        // accept-loop-wedges-at-1 P0 blocker).
        //
        // `karac_runtime_scheduler_start_dispatcher() -> i32` — idempotent
        // bootstrap; auto-starts the background poller. Replaces the bare
        // `start_background_thread` call at park bootstrap so wakeups are
        // routed to the correct task rather than drained globally.
        let start_dispatcher_ty = context.i32_type().fn_type(&[], false);
        module.add_function(
            "karac_runtime_scheduler_start_dispatcher",
            start_dispatcher_ty,
            Some(Linkage::External),
        );
        // `karac_runtime_event_loop_deregister_fd(raw_fd: i32, token: u64)
        // -> i32` — the caller deregisters its fd after the park completes
        // (one-shot), so a re-registered fd in a subsequent loop iteration
        // gets a fresh token and the event loop doesn't keep reporting a
        // stale registration.
        let deregister_fd_ty = context
            .i32_type()
            .fn_type(&[context.i32_type().into(), i64_type.into()], false);
        module.add_function(
            "karac_runtime_event_loop_deregister_fd",
            deregister_fd_ty,
            Some(Linkage::External),
        );
        // ── Async-sleep timer (phase-5 auto-par divergence A2a-2.2) ───────
        //
        // `karac_runtime_event_loop_register_timer(duration_nanos: u64,
        // parked: *mut c_void, cancel: *const AtomicBool) -> u64` — the
        // timer-axis sibling of `register_fd`: no fd, no `epoll_ctl`, just a
        // deadline on the reactor's min-heap (A2a-1). On expiry the poller
        // surfaces a `Wakeup{parked}` that the dispatcher routes to the
        // parked poll-fn's `state_1`. `sleep_ms`'s park-on-timer state
        // machine (`emit_state_machine_invocation_for_park_on_timer`) is the
        // sole emitter. The dispatcher claims the registration itself
        // (`take_registration_with_cancel`), so — unlike the fd path, which
        // must `epoll_ctl(DEL)` via `deregister_fd` — the timer caller needs
        // no post-wait cleanup call.
        let register_timer_ty = i64_type.fn_type(
            &[
                i64_type.into(), // duration_nanos (u64)
                ptr_type.into(), // parked task pointer (opaque)
                ptr_type.into(), // cancel: *const AtomicBool (null = none)
            ],
            false,
        );
        module.add_function(
            "karac_runtime_event_loop_register_timer",
            register_timer_ty,
            Some(Linkage::External),
        );
        // `karac_runtime_event_loop_cancel_timer(token: u64) -> i32` — claims
        // a not-yet-fired timer registration (the cooperative-cancel path,
        // A2b). Declared here for completeness; the non-cancellable
        // `sleep_ms` lowering never calls it.
        let cancel_timer_ty = context.i32_type().fn_type(&[i64_type.into()], false);
        module.add_function(
            "karac_runtime_event_loop_cancel_timer",
            cancel_timer_ty,
            Some(Linkage::External),
        );
        // Per-park completion slot. `new` allocates; `wait` blocks the
        // caller until the dispatcher signals readiness; `signal` is called
        // by the leaf poll-fn's `state_1` on the dispatcher thread; `free`
        // releases the slot after `wait` returns.
        let park_slot_new_ty = ptr_type.fn_type(&[], false);
        module.add_function(
            "karac_runtime_park_slot_new",
            park_slot_new_ty,
            Some(Linkage::External),
        );
        let park_slot_unary_ty = context.void_type().fn_type(&[ptr_type.into()], false);
        module.add_function(
            "karac_runtime_park_slot_wait",
            park_slot_unary_ty,
            Some(Linkage::External),
        );
        module.add_function(
            "karac_runtime_park_slot_signal",
            park_slot_unary_ty,
            Some(Linkage::External),
        );
        module.add_function(
            "karac_runtime_park_slot_free",
            park_slot_unary_ty,
            Some(Linkage::External),
        );
        // `karac_runtime_park_slot_cancel_ptr(slot: ptr) -> ptr` — reads the
        // slot's bound per-task cancel flag (null if unbound). The coroutine
        // park-suspend copies the result into the parked record's `cancel`
        // field so the dispatcher / cancel-sweep hand the coroutine its own
        // cancellation flag (slice 5c).
        let park_slot_cancel_ptr_ty = ptr_type.fn_type(&[ptr_type.into()], false);
        module.add_function(
            "karac_runtime_park_slot_cancel_ptr",
            park_slot_cancel_ptr_ty,
            Some(Linkage::External),
        );

        // ── stdlib TcpListener codegen-side wiring (Phase 6 line 17) ──────
        //
        // `karac_runtime_tcp_bind(addr_ptr: *const u8, addr_len: i64) -> i32`
        // — backs `TcpListener.bind(addr: String) -> TcpListener`. Returns
        // the listener fd; -1 on UTF-8 / parse / bind failure. Prints
        // `BOUND_PORT=<n>` to stdout when the requested address ends in
        // `:0` (ephemeral-port convention).
        let tcp_bind_ty = context
            .i32_type()
            .fn_type(&[ptr_type.into(), i64_type.into()], false);
        module.add_function(
            "karac_runtime_tcp_bind",
            tcp_bind_ty,
            Some(Linkage::External),
        );
        // `karac_runtime_tcp_connect(addr_ptr: *const u8, addr_len: i64)
        // -> i32` — backs `TcpStream.connect(addr: String) ->
        // Result[TcpStream, TcpError]`, the plain-TCP client primitive.
        // Same signature shape as `karac_runtime_tcp_bind`; returns the
        // connected socket fd, -1 on UTF-8 / parse / connect failure.
        let tcp_connect_ty = context
            .i32_type()
            .fn_type(&[ptr_type.into(), i64_type.into()], false);
        module.add_function(
            "karac_runtime_tcp_connect",
            tcp_connect_ty,
            Some(Linkage::External),
        );
        // `karac_runtime_tcp_accept(listener_fd: i32) -> i32` — backs
        // the *raw* accept(2) inside `TcpListener.accept`'s codegen
        // lowering. Caller (codegen) is responsible for parking via
        // `karac_park_on_fd` BEFORE invoking this — the FFI itself is
        // pure-syscall (no event-loop interaction). Returns the new
        // connection fd; -1 on failure (incl. EAGAIN, which signals
        // a missed-wakeup bug).
        let tcp_accept_ty = context
            .i32_type()
            .fn_type(&[context.i32_type().into()], false);
        module.add_function(
            "karac_runtime_tcp_accept",
            tcp_accept_ty,
            Some(Linkage::External),
        );
        // `karac_runtime_tcp_read(stream_fd: i32, buf_ptr: *mut u8,
        // buf_len: i64) -> i64` — backs the *raw* read(2) inside
        // `TcpStream.read`'s codegen lowering. Caller (codegen) is
        // responsible for parking via `karac_park_on_fd(stream_fd, 0)`
        // BEFORE invoking this. Returns byte count read; 0 on EOF;
        // -1 on error.
        let tcp_read_ty = context.i64_type().fn_type(
            &[context.i32_type().into(), ptr_type.into(), i64_type.into()],
            false,
        );
        module.add_function(
            "karac_runtime_tcp_read",
            tcp_read_ty,
            Some(Linkage::External),
        );
        // `karac_runtime_tcp_write(stream_fd: i32, buf_ptr: *const u8,
        // buf_len: i64) -> i64` — backs the *raw* write(2) inside
        // `TcpStream.write`'s codegen lowering. Caller (codegen) is
        // responsible for parking via `karac_park_on_fd(stream_fd, 1)`
        // BEFORE invoking this. Returns byte count written; -1 on
        // error.
        let tcp_write_ty = context.i64_type().fn_type(
            &[context.i32_type().into(), ptr_type.into(), i64_type.into()],
            false,
        );
        module.add_function(
            "karac_runtime_tcp_write",
            tcp_write_ty,
            Some(Linkage::External),
        );

        // `karac_runtime_tcp_close(fd: i32) -> i32` — backs the
        // hand-rolled `@TcpStream.drop` / `@TcpListener.drop` LLVM
        // bodies emitted by `emit_hardcoded_stdlib_drop_bodies`.
        // Closes the kernel-side socket; a `-1` fd is a no-op.
        // Phase 6 line 17 slice 9d.
        let tcp_close_ty = context
            .i32_type()
            .fn_type(&[context.i32_type().into()], false);
        module.add_function(
            "karac_runtime_tcp_close",
            tcp_close_ty,
            Some(Linkage::External),
        );

        // ── Phase 6 line 236 slice 2 — TLS / HTTPS server-side stdlib FFI.
        //
        // Six external symbols mirroring slice 1's `runtime/src/tls.rs`
        // surface. Codegen-side lowerings live in `src/codegen/tls.rs`.
        //
        // `karac_runtime_tls_config_new(cert_pem, cert_len, key_pem,
        // key_len) -> *mut TlsConfig` — parse PEM bytes via rustls-pemfile,
        // build a `rustls::ServerConfig`, return an opaque heap pointer.
        // Null on parse / build failure. Freed via _tls_config_free.
        let tls_config_new_ty = ptr_type.fn_type(
            &[
                ptr_type.into(),
                i64_type.into(),
                ptr_type.into(),
                i64_type.into(),
            ],
            false,
        );
        module.add_function(
            "karac_runtime_tls_config_new",
            tls_config_new_ty,
            Some(Linkage::External),
        );
        // `karac_runtime_tls_config_free(*mut TlsConfig)` — drop a
        // previously-allocated config. Idempotent for null.
        let tls_config_free_ty = context.void_type().fn_type(&[ptr_type.into()], false);
        module.add_function(
            "karac_runtime_tls_config_free",
            tls_config_free_ty,
            Some(Linkage::External),
        );
        // `karac_runtime_tls_listener_bind(addr_ptr, addr_len, config)
        // -> i32` — TCP bind that also keeps the config available for
        // accept time. v1 delegates to `karac_runtime_tcp_bind`; the
        // config pointer is forwarded by the kara struct rather than
        // stored runtime-side. Same `:0` BOUND_PORT convention as TCP.
        let tls_listener_bind_ty = context
            .i32_type()
            .fn_type(&[ptr_type.into(), i64_type.into(), ptr_type.into()], false);
        module.add_function(
            "karac_runtime_tls_listener_bind",
            tls_listener_bind_ty,
            Some(Linkage::External),
        );
        // `karac_runtime_tls_accept(listener_fd, config) -> i32` —
        // raw accept(2) + synchronous rustls handshake; registers a
        // `TlsSession` in the per-fd registry on success. Returns the
        // connection fd or -1.
        let tls_accept_ty = context
            .i32_type()
            .fn_type(&[context.i32_type().into(), ptr_type.into()], false);
        module.add_function(
            "karac_runtime_tls_accept",
            tls_accept_ty,
            Some(Linkage::External),
        );
        // Phase-8 line 22 — `karac_runtime_tls_client_connect(addr_ptr,
        // addr_len, server_name_ptr, server_name_len, roots_pem_ptr,
        // roots_pem_len) -> i32`. Client-side mirror of `_tls_accept`:
        // build a `ClientConfig` from `roots_pem`, TCP connect, sync
        // handshake against `server_name`, register session in the
        // shared per-fd map (`Connection::Client` variant). Returns the
        // connection fd or -1. Backs `TlsStream.connect`.
        let tls_client_connect_ty = context.i32_type().fn_type(
            &[
                ptr_type.into(), // addr_ptr
                i64_type.into(), // addr_len
                ptr_type.into(), // server_name_ptr
                i64_type.into(), // server_name_len
                ptr_type.into(), // roots_pem_ptr
                i64_type.into(), // roots_pem_len
            ],
            false,
        );
        module.add_function(
            "karac_runtime_tls_client_connect",
            tls_client_connect_ty,
            Some(Linkage::External),
        );
        // `karac_runtime_tls_read(fd, buf_ptr, buf_len) -> i64` /
        // `karac_runtime_tls_write(fd, buf_ptr, buf_len) -> i64` — pump
        // rustls's inbound / outbound packet processors. Caller parks
        // via `karac_park_on_fd(fd, dir)` BEFORE invoking. Same
        // negative-errno return convention as the TCP siblings; -1 for
        // non-syscall errors (protocol failure, session-lookup miss).
        let tls_read_ty = context.i64_type().fn_type(
            &[context.i32_type().into(), ptr_type.into(), i64_type.into()],
            false,
        );
        module.add_function(
            "karac_runtime_tls_read",
            tls_read_ty,
            Some(Linkage::External),
        );
        let tls_write_ty = context.i64_type().fn_type(
            &[context.i32_type().into(), ptr_type.into(), i64_type.into()],
            false,
        );
        module.add_function(
            "karac_runtime_tls_write",
            tls_write_ty,
            Some(Linkage::External),
        );
        // `karac_runtime_tls_close(fd) -> i32` — remove the session
        // entry from the per-fd registry and close the underlying TCP
        // fd. Same `-1` no-op shape as `karac_runtime_tcp_close`.
        let tls_close_ty = context
            .i32_type()
            .fn_type(&[context.i32_type().into()], false);
        module.add_function(
            "karac_runtime_tls_close",
            tls_close_ty,
            Some(Linkage::External),
        );

        // Phase 6 line 218 slice 4 — spawn / TaskHandle.join / handle-free FFI.
        //
        // `karac_runtime_spawn(fn_ptr: ptr, env: ptr, result_size: usize,
        // result_align: usize) -> ptr` — submit a fresh closure-task to
        // the global worker pool. The `fn_ptr` is a codegen-synthesized
        // `extern "C" fn(env, result_out, cancel)` wrapper that reads
        // captures from `env`, runs the closure body, memcpys the
        // T-typed return value into `*result_out`. The returned pointer
        // is the runtime-side `KaracTaskHandle` — codegen casts it to
        // i64 and stores into the `TaskHandle.task_id` field. See
        // `runtime/src/scheduler.rs` slice 3.
        let usize_ty = if std::mem::size_of::<usize>() == 8 {
            context.i64_type()
        } else {
            context.i32_type()
        };
        let spawn_ty = ptr_type.fn_type(
            &[
                ptr_type.into(), // fn_ptr (SpawnFn)
                ptr_type.into(), // env
                usize_ty.into(), // result_size
                usize_ty.into(), // result_align
            ],
            false,
        );
        module.add_function("karac_runtime_spawn", spawn_ty, Some(Linkage::External));

        // A2 slice 5a — `karac_runtime_spawn_coro(wrap_fn: ptr, env: ptr)
        // -> ptr` — density-optimal non-blocking coroutine spawn. `wrap_fn`
        // is a codegen-synthesized `extern "C" fn(env, *KaracParkSlot,
        // cancel)` that unpacks `env` → args and calls the coroutine *ramp*
        // with the bound slot (register fd + suspend + return). The worker
        // is freed the moment the ramp suspends; the dispatcher drives the
        // parked coroutine, whose body signals the slot at completion. The
        // returned handle's `karac_runtime_task_join` waits on that slot.
        // See runtime/src/scheduler.rs + spike § 6⅞.
        let spawn_coro_ty = ptr_type.fn_type(
            &[
                ptr_type.into(), // wrap_fn (CoroSpawnFn)
                ptr_type.into(), // env
            ],
            false,
        );
        module.add_function(
            "karac_runtime_spawn_coro",
            spawn_coro_ty,
            Some(Linkage::External),
        );

        // `karac_runtime_task_join(handle: ptr, out_slot: ptr) -> u8`
        // — block until the task reaches a terminal state, memcpy the
        // result into `*out_slot` on COMPLETED, free the handle, return
        // a `TASK_STATE_*` discriminant (1 = COMPLETED, 2 = PANICKED,
        // 3 = CANCELLED).
        let task_join_ty = context
            .i8_type()
            .fn_type(&[ptr_type.into(), ptr_type.into()], false);
        module.add_function(
            "karac_runtime_task_join",
            task_join_ty,
            Some(Linkage::External),
        );

        // `karac_runtime_task_handle_free(handle: ptr)` — release a
        // handle without joining. Used by TaskGroup-side cleanup (slice
        // 5) for unjoined handles. Caller must ensure the task has
        // reached a terminal state before calling.
        let task_handle_free_ty = context.void_type().fn_type(&[ptr_type.into()], false);
        module.add_function(
            "karac_runtime_task_handle_free",
            task_handle_free_ty,
            Some(Linkage::External),
        );

        // `karac_runtime_task_state(handle: ptr) -> u8` — non-blocking
        // peek at the task's lifecycle state. Used by TaskGroup.drop's
        // poll-before-free path (slice 5) and by tests.
        let task_state_ty = context.i8_type().fn_type(&[ptr_type.into()], false);
        module.add_function(
            "karac_runtime_task_state",
            task_state_ty,
            Some(Linkage::External),
        );

        // Phase 6 line 218 slice 5 — TaskGroup container FFI.
        //
        // `karac_runtime_taskgroup_new() -> ptr` — allocate a fresh
        // group; address is stored as `i64` in `TaskGroup.id`.
        let taskgroup_new_ty = ptr_type.fn_type(&[], false);
        module.add_function(
            "karac_runtime_taskgroup_new",
            taskgroup_new_ty,
            Some(Linkage::External),
        );

        // `karac_runtime_taskgroup_register(group: ptr, child: ptr)`
        // — register a freshly spawned child handle with the group.
        // Codegen emits this immediately after `karac_runtime_spawn`
        // returns inside the `tg.spawn(closure)` lowering path.
        let taskgroup_register_ty = context
            .void_type()
            .fn_type(&[ptr_type.into(), ptr_type.into()], false);
        module.add_function(
            "karac_runtime_taskgroup_register",
            taskgroup_register_ty,
            Some(Linkage::External),
        );

        // `karac_runtime_taskgroup_join_and_free(group: ptr)` — block
        // until every registered child reaches a terminal state, then
        // free the group. Invoked from the hand-rolled `@TaskGroup.drop`
        // LLVM body emitted by `emit_hardcoded_stdlib_drop_bodies`.
        let taskgroup_join_ty = context.void_type().fn_type(&[ptr_type.into()], false);
        module.add_function(
            "karac_runtime_taskgroup_join_and_free",
            taskgroup_join_ty,
            Some(Linkage::External),
        );

        // `karac_runtime_taskgroup_cancel(group: ptr)` — A2 slice 5b-1.
        // Flips every registered child's per-task cancel flag. Backs the
        // user-callable `TaskGroup.cancel()` method. Inert until the
        // dispatcher routes the per-task flag to parked coroutines (slice 5c).
        let taskgroup_cancel_ty = context.void_type().fn_type(&[ptr_type.into()], false);
        module.add_function(
            "karac_runtime_taskgroup_cancel",
            taskgroup_cancel_ty,
            Some(Linkage::External),
        );

        // Phase 6 "Channel AOT codegen lowering" — `Channel[T]` runtime FFI.
        // The type-erased queue lives in `runtime/src/channel.rs` (compiled
        // into every archive — a queue has no scheduler dependency). Both
        // channel ends (`Sender`/`Receiver`) lower to the opaque
        // `*mut KaracChannel` these return/consume. `elem_size` is `u64`
        // (ABI-identical on wasm32 + native — the `__karac_malloc64` size_t
        // discipline) and is threaded per send/recv call: the element type is
        // statically known at each op site (the typed receiver) but NOT at
        // `Channel.new()`, so `channel_new` itself is type-agnostic.
        //
        // `karac_runtime_channel_new() -> ptr` — fresh channel, refcount 2
        // (the Sender + Receiver of one `Channel.new()`).
        let channel_new_ty = ptr_type.fn_type(&[], false);
        module.add_function(
            "karac_runtime_channel_new",
            channel_new_ty,
            Some(Linkage::External),
        );

        // `karac_runtime_channel_clone(ch: ptr) -> ptr` — backs
        // `Sender.clone()`: same pointer, sender + total count++.
        let channel_clone_ty = ptr_type.fn_type(&[ptr_type.into()], false);
        module.add_function(
            "karac_runtime_channel_clone",
            channel_clone_ty,
            Some(Linkage::External),
        );

        // `karac_runtime_channel_drop_sender(ch: ptr)` /
        // `karac_runtime_channel_drop_receiver(ch: ptr)` — scope-exit drop,
        // split by end so the last `Sender` drop can *close* the channel
        // (waking blocked receivers). Both release one `total` reference
        // (free at zero). Emitted by `DropChannelEnd { is_sender }` keyed off
        // the binding's `Sender`/`Receiver` surface type.
        let channel_drop_ty = context.void_type().fn_type(&[ptr_type.into()], false);
        module.add_function(
            "karac_runtime_channel_drop_sender",
            channel_drop_ty,
            Some(Linkage::External),
        );
        module.add_function(
            "karac_runtime_channel_drop_receiver",
            channel_drop_ty,
            Some(Linkage::External),
        );

        // `karac_runtime_channel_send(ch: ptr, val_ptr: ptr, elem_size: u64)`
        // — copy `elem_size` bytes from `*val_ptr` into the queue.
        // `Sender.send`.
        let channel_send_ty = context
            .void_type()
            .fn_type(&[ptr_type.into(), ptr_type.into(), i64_type.into()], false);
        module.add_function(
            "karac_runtime_channel_send",
            channel_send_ty,
            Some(Linkage::External),
        );

        // `karac_runtime_channel_recv(ch: ptr, out_ptr: ptr, elem_size: u64)
        // -> u8` — **blocking** receive (parks while empty + open on
        // threads-targets; non-blocking on sequential wasm). Returns 1 with a
        // value, 0 on closed-empty (out slot zero-filled). `recv` ignores the
        // discriminant (result is `T`, the 0 case is the zero-value answer).
        // `karac_runtime_channel_try_recv` has the same signature but is
        // **non-blocking** on every target — `try_recv` builds `Some`/`None`
        // from its discriminant.
        let channel_recv_ty = context
            .i8_type()
            .fn_type(&[ptr_type.into(), ptr_type.into(), i64_type.into()], false);
        module.add_function(
            "karac_runtime_channel_recv",
            channel_recv_ty,
            Some(Linkage::External),
        );
        module.add_function(
            "karac_runtime_channel_try_recv",
            channel_recv_ty,
            Some(Linkage::External),
        );

        // Bounded-channel runtime (`runtime/src/bounded_channel.rs`), backing
        // `BoundedChannel[T]` (also compiled into every archive — a bounded
        // queue has no scheduler dependency). The opaque
        // `*mut KaracBoundedChannel` round-trips through the `i64 handle_id`
        // field of the `BoundedChannel { handle_id }` struct (ptrtoint at
        // `new`, inttoptr at send/recv/drop). `elem_size` is `u64`, threaded
        // per send/recv like the unbounded channel. No clone / Sender split:
        // single-owner, freed by `_drop` at scope exit.
        //
        // `karac_runtime_bounded_channel_new(capacity: i64, on_full: u8) -> ptr`
        // — fresh bounded queue (capacity clamped >= 0; `on_full` accepted for
        // forward-compat but v1-collapsed to fail-fast).
        let bch_new_ty = ptr_type.fn_type(&[i64_type.into(), context.i8_type().into()], false);
        module.add_function(
            "karac_runtime_bounded_channel_new",
            bch_new_ty,
            Some(Linkage::External),
        );
        // `_send(ch, val_ptr, elem_size) -> u8` (1 = buffered → `Ok(())`,
        // 0 = full → `Err(ChannelError.Full)`) and `_recv(ch, out_ptr,
        // elem_size) -> u8` (1 = `Some`, 0 = `None`); both non-blocking.
        let bch_op_ty = context
            .i8_type()
            .fn_type(&[ptr_type.into(), ptr_type.into(), i64_type.into()], false);
        module.add_function(
            "karac_runtime_bounded_channel_send",
            bch_op_ty,
            Some(Linkage::External),
        );
        module.add_function(
            "karac_runtime_bounded_channel_recv",
            bch_op_ty,
            Some(Linkage::External),
        );
        // `_drop(ch)` — single-owner free at scope exit (the `BoundedChannel`
        // Drop lowering).
        let bch_drop_ty = context.void_type().fn_type(&[ptr_type.into()], false);
        module.add_function(
            "karac_runtime_bounded_channel_drop",
            bch_drop_ty,
            Some(Linkage::External),
        );

        // Blocking-mutex slow path (`runtime/src/mutex.rs`). Both take the
        // `*mut i64` lock-flag word (field 0 of the `{ i64 lockflag, T value }`
        // Mutex aggregate). `_mutex_lock` blocks until it acquires (called only
        // when `compile_lock_block`'s inline `cmpxchg(0->1)` fast path failed);
        // `_mutex_unlock_wake` wakes a parked waiter (called by the
        // `ReleaseMutex` cleanup action only when its `xchg(->0)` saw state 2).
        let mutex_op_ty = context.void_type().fn_type(&[ptr_type.into()], false);
        module.add_function(
            "karac_runtime_mutex_lock",
            mutex_op_ty,
            Some(Linkage::External),
        );
        module.add_function(
            "karac_runtime_mutex_unlock_wake",
            mutex_op_ty,
            Some(Linkage::External),
        );

        // `karac_runtime_ws_send_text(fd: i32, msg_ptr: *const u8,
        // msg_len: i64) -> i64` — backs the encode + write step
        // inside `WebSocket.send_text`'s codegen lowering. Caller
        // (codegen) is responsible for parking via
        // `karac_park_on_fd(fd, 1)` BEFORE invoking this. Returns
        // payload byte count on success (== msg_len); -1 on error.
        // Phase 6 line 17 slice 9e.1.
        let ws_send_text_ty = context.i64_type().fn_type(
            &[context.i32_type().into(), ptr_type.into(), i64_type.into()],
            false,
        );
        module.add_function(
            "karac_runtime_ws_send_text",
            ws_send_text_ty,
            Some(Linkage::External),
        );

        // `karac_runtime_ws_recv_text(fd: i32, out_ptr: *mut u8,
        // out_max_len: i64) -> i64` — backs the read + decode +
        // unmask step inside `WebSocket.recv_text`'s codegen
        // lowering. Caller (codegen) is responsible for parking via
        // `karac_park_on_fd(fd, 0)` BEFORE invoking this. Returns
        // payload byte count on success; 0 on graceful EOF;
        // -1 on protocol error / IO error / oversize payload.
        // Phase 6 line 17 slice 9e.1.
        let ws_recv_text_ty = context.i64_type().fn_type(
            &[context.i32_type().into(), ptr_type.into(), i64_type.into()],
            false,
        );
        module.add_function(
            "karac_runtime_ws_recv_text",
            ws_recv_text_ty,
            Some(Linkage::External),
        );

        // `karac_runtime_ws_accept(listener_fd: i32) -> i32` —
        // backs the accept + HTTP-upgrade step inside
        // `WebSocket.accept`'s codegen lowering. Caller (codegen)
        // is responsible for parking via
        // `karac_park_on_fd(listener_fd, 0)` BEFORE invoking this.
        // Returns the upgraded connection fd on success, -1 on
        // any failure (accept error, IO error, missing
        // Sec-WebSocket-Key, response write error).
        // Phase 6 line 17 slice 9e.2.
        let ws_accept_ty = context
            .i32_type()
            .fn_type(&[context.i32_type().into()], false);
        module.add_function(
            "karac_runtime_ws_accept",
            ws_accept_ty,
            Some(Linkage::External),
        );

        // Phase 6 line 236 slice 3 — `karac_runtime_ws_accept_tls(
        // listener_fd: i32, config: *mut TlsConfig) -> i32`. Same
        // shape as `ws_accept` but the conn is TLS-wrapped: TCP
        // accept → rustls handshake → register session in TLS
        // SESSIONS registry → HTTP upgrade exchange over TLS →
        // return the connection fd. Subsequent `ws_recv_text` /
        // `ws_send_text` calls auto-dispatch through TLS once the
        // session is registered (their FFI bodies check `tls::
        // lookup_session(fd)`).
        let ws_accept_tls_ty = context
            .i32_type()
            .fn_type(&[context.i32_type().into(), ptr_type.into()], false);
        module.add_function(
            "karac_runtime_ws_accept_tls",
            ws_accept_tls_ty,
            Some(Linkage::External),
        );

        // Phase 6 line 17 slice 9e.3 — binary frame FFIs. Same ABI
        // as the text framing FFIs from slice 9e.1 (3-arg `(fd,
        // ptr, len)` returning i64); the runtime helper switches
        // on the opcode bit (0x2 vs 0x1) internally.
        let ws_send_binary_ty = context.i64_type().fn_type(
            &[context.i32_type().into(), ptr_type.into(), i64_type.into()],
            false,
        );
        module.add_function(
            "karac_runtime_ws_send_binary",
            ws_send_binary_ty,
            Some(Linkage::External),
        );
        let ws_recv_binary_ty = context.i64_type().fn_type(
            &[context.i32_type().into(), ptr_type.into(), i64_type.into()],
            false,
        );
        module.add_function(
            "karac_runtime_ws_recv_binary",
            ws_recv_binary_ty,
            Some(Linkage::External),
        );

        // Phase 6 line 17 slice 9e.4 — client-side masked send FFIs.
        // Same ABI as the unmasked text/binary send FFIs (3-arg
        // `(fd, ptr, len)` returning i64); the runtime helper
        // generates the mask key per-call and writes a MASK=1
        // frame.
        let ws_send_text_masked_ty = context.i64_type().fn_type(
            &[context.i32_type().into(), ptr_type.into(), i64_type.into()],
            false,
        );
        module.add_function(
            "karac_runtime_ws_send_text_masked",
            ws_send_text_masked_ty,
            Some(Linkage::External),
        );
        let ws_send_binary_masked_ty = context.i64_type().fn_type(
            &[context.i32_type().into(), ptr_type.into(), i64_type.into()],
            false,
        );
        module.add_function(
            "karac_runtime_ws_send_binary_masked",
            ws_send_binary_masked_ty,
            Some(Linkage::External),
        );

        // ── std.json codegen-side wiring (phase-8 line 435 slice 1) ──────
        //
        // Per-variant FFI constructors invoked by the synthesized
        // `__karac_json_kara_to_ffi` walker in `compile_json_lower_helper`,
        // plus the `stringify` / `free_value` / `free_string` consumers
        // called by `compile_method_call`'s Json arm. See
        // `runtime/src/lib.rs::karac_runtime_json_*` for the matching
        // ownership rules.
        let json_make_null_ty = ptr_type.fn_type(&[], false);
        module.add_function(
            "karac_runtime_json_make_null",
            json_make_null_ty,
            Some(Linkage::External),
        );
        let i8_ty = context.i8_type();
        let json_make_bool_ty = ptr_type.fn_type(&[i8_ty.into()], false);
        module.add_function(
            "karac_runtime_json_make_bool",
            json_make_bool_ty,
            Some(Linkage::External),
        );
        let f64_ty = context.f64_type();
        let json_make_number_ty = ptr_type.fn_type(&[f64_ty.into()], false);
        module.add_function(
            "karac_runtime_json_make_number",
            json_make_number_ty,
            Some(Linkage::External),
        );
        let json_make_string_ty = ptr_type.fn_type(&[ptr_type.into(), i64_type.into()], false);
        module.add_function(
            "karac_runtime_json_make_string",
            json_make_string_ty,
            Some(Linkage::External),
        );
        let json_alloc_items_buf_ty = ptr_type.fn_type(&[i64_type.into()], false);
        module.add_function(
            "karac_runtime_json_alloc_items_buf",
            json_alloc_items_buf_ty,
            Some(Linkage::External),
        );
        let json_alloc_keys_buf_ty = ptr_type.fn_type(&[i64_type.into()], false);
        module.add_function(
            "karac_runtime_json_alloc_keys_buf",
            json_alloc_keys_buf_ty,
            Some(Linkage::External),
        );
        let json_alloc_key_ty = ptr_type.fn_type(&[ptr_type.into(), i64_type.into()], false);
        module.add_function(
            "karac_runtime_json_alloc_key",
            json_alloc_key_ty,
            Some(Linkage::External),
        );
        let json_make_array_ty = ptr_type.fn_type(&[ptr_type.into(), i64_type.into()], false);
        module.add_function(
            "karac_runtime_json_make_array",
            json_make_array_ty,
            Some(Linkage::External),
        );
        let json_make_object_ty =
            ptr_type.fn_type(&[ptr_type.into(), ptr_type.into(), i64_type.into()], false);
        module.add_function(
            "karac_runtime_json_make_object",
            json_make_object_ty,
            Some(Linkage::External),
        );
        let json_stringify_ty = ptr_type.fn_type(&[ptr_type.into()], false);
        module.add_function(
            "karac_runtime_json_stringify",
            json_stringify_ty,
            Some(Linkage::External),
        );
        let json_free_value_ty = context.void_type().fn_type(&[ptr_type.into()], false);
        module.add_function(
            "karac_runtime_json_free_value",
            json_free_value_ty,
            Some(Linkage::External),
        );
        let json_free_string_ty = context.void_type().fn_type(&[ptr_type.into()], false);
        module.add_function(
            "karac_runtime_json_free_string",
            json_free_string_ty,
            Some(Linkage::External),
        );

        // karac_runtime_json_parse(input: *const c_char, error_out: *mut KaracJsonError)
        //   -> *mut KaracJsonValue. Returns null on parse error with the
        // KaracJsonError struct populated; caller owns the FFI tree and
        // (on null return) the message string. Phase-8 line 435 slice 2.
        let json_parse_ty = ptr_type.fn_type(&[ptr_type.into(), ptr_type.into()], false);
        module.add_function(
            "karac_runtime_json_parse",
            json_parse_ty,
            Some(Linkage::External),
        );

        // ── Map runtime extern declarations ──────────────────────────────
        // All map methods use opaque ptr for the map handle and key/value
        // pointers. Sizes and fn-pointers are passed as i64 / ptr.
        let i64_ty: BasicMetadataTypeEnum = i64_type.into();
        let ptr_md: BasicMetadataTypeEnum = ptr_type.into();

        // karac_map_new(key_size: i64, val_size: i64, hash_fn: ptr, eq_fn: ptr) -> ptr
        let map_new_ty = ptr_type.fn_type(&[i64_ty, i64_ty, ptr_md, ptr_md], false);
        let karac_map_new_fn =
            module.add_function("karac_map_new", map_new_ty, Some(Linkage::External));

        // karac_map_free(map: ptr) -> void
        let map_free_ty = context.void_type().fn_type(&[ptr_md], false);
        let karac_map_free_fn =
            module.add_function("karac_map_free", map_free_ty, Some(Linkage::External));

        // karac_map_free_with_drop_vec(map: ptr, drop_key: i32, drop_val: i32) -> void —
        // generalized variant: walks live buckets and frees per-entry key
        // and/or value data pointers (when the respective flag is set
        // and the field's `cap > 0`) before deallocating the bucket
        // storage. Selected when either side of `Map[K, V]` / `Set[T]`
        // follows the Vec/String `{ptr, len, cap}` layout. The i32 flags
        // are codegen-set: nonzero means "drop this side".
        let i32_ty: BasicMetadataTypeEnum = context.i32_type().into();
        let map_free_with_drop_ty = context
            .void_type()
            .fn_type(&[ptr_md, i32_ty, i32_ty], false);
        let karac_map_free_with_drop_vec_fn = module.add_function(
            "karac_map_free_with_drop_vec",
            map_free_with_drop_ty,
            Some(Linkage::External),
        );

        // karac_map_insert_old(map: ptr, key: ptr, val: ptr, out_old_val: ptr) -> i1
        let map_insert_old_ty = context
            .bool_type()
            .fn_type(&[ptr_md, ptr_md, ptr_md, ptr_md], false);
        let karac_map_insert_old_fn = module.add_function(
            "karac_map_insert_old",
            map_insert_old_ty,
            Some(Linkage::External),
        );

        // karac_map_insert_borrowed_str_old(map, key, val, out_old_val) -> i1
        // Same signature as insert_old; deep-copies a borrowed String key on a
        // fresh insertion (zero-alloc on an existing key).
        let karac_map_insert_borrowed_str_old_fn = module.add_function(
            "karac_map_insert_borrowed_str_old",
            map_insert_old_ty,
            Some(Linkage::External),
        );

        // karac_map_get(map: ptr, key: ptr, out_val: ptr) -> i1
        let map_get_ty = context
            .bool_type()
            .fn_type(&[ptr_md, ptr_md, ptr_md], false);
        let karac_map_get_fn =
            module.add_function("karac_map_get", map_get_ty, Some(Linkage::External));

        // karac_map_remove_old(map: ptr, key: ptr, out_old_val: ptr) -> i1
        let map_remove_old_ty = context
            .bool_type()
            .fn_type(&[ptr_md, ptr_md, ptr_md], false);
        let karac_map_remove_old_fn = module.add_function(
            "karac_map_remove_old",
            map_remove_old_ty,
            Some(Linkage::External),
        );

        // karac_map_contains(map: ptr, key: ptr) -> i1
        let map_contains_ty = context.bool_type().fn_type(&[ptr_md, ptr_md], false);
        let karac_map_contains_fn = module.add_function(
            "karac_map_contains",
            map_contains_ty,
            Some(Linkage::External),
        );

        // karac_map_len(map: ptr) -> i64
        let map_len_ty = i64_type.fn_type(&[ptr_md], false);
        let karac_map_len_fn =
            module.add_function("karac_map_len", map_len_ty, Some(Linkage::External));

        // karac_map_clear(map: ptr) -> void
        let map_clear_ty = context.void_type().fn_type(&[ptr_md], false);
        let karac_map_clear_fn =
            module.add_function("karac_map_clear", map_clear_ty, Some(Linkage::External));

        // karac_map_clear_with_drop_vec(map: ptr, drop_key: i32, drop_val: i32) -> void
        // In-place clear that frees per-entry heap key/value buffers first.
        let karac_map_clear_with_drop_vec_fn = module.add_function(
            "karac_map_clear_with_drop_vec",
            map_free_with_drop_ty,
            Some(Linkage::External),
        );

        // karac_map_iter_new(map: ptr) -> ptr
        let map_iter_new_ty = ptr_type.fn_type(&[ptr_md], false);
        let karac_map_iter_new_fn = module.add_function(
            "karac_map_iter_new",
            map_iter_new_ty,
            Some(Linkage::External),
        );

        // karac_map_iter_next(iter: ptr, out_key: ptr, out_val: ptr) -> i1
        let map_iter_next_ty = context
            .bool_type()
            .fn_type(&[ptr_md, ptr_md, ptr_md], false);
        let karac_map_iter_next_fn = module.add_function(
            "karac_map_iter_next",
            map_iter_next_ty,
            Some(Linkage::External),
        );

        // karac_map_iter_free(iter: ptr) -> void
        let map_iter_free_ty = context.void_type().fn_type(&[ptr_md], false);
        let karac_map_iter_free_fn = module.add_function(
            "karac_map_iter_free",
            map_iter_free_ty,
            Some(Linkage::External),
        );

        // karac_string_decode_char(data: ptr, len: i64, byte_offset: i64,
        //                          out_codepoint: ptr) -> i64
        // Drives `for c in s` / `for c in s.chars()` lowering. Returns the
        // byte offset after the decoded char; writes the codepoint to
        // `out_codepoint`.
        let i64_md: BasicMetadataTypeEnum = context.i64_type().into();
        let string_decode_char_ty = context
            .i64_type()
            .fn_type(&[ptr_md, i64_md, i64_md, ptr_md], false);
        let karac_string_decode_char_fn = module.add_function(
            "karac_string_decode_char",
            string_decode_char_ty,
            Some(Linkage::External),
        );

        // karac_string_encode_char(cp: u32, out: ptr) -> i64
        // Encodes a Unicode scalar value as 1–4 UTF-8 bytes through `out`,
        // returns the byte count. Drives the `println(c)` / `f"{c}"` glyph
        // rendering — see `emit_codepoint_to_utf8`.
        let u32_md: BasicMetadataTypeEnum = context.i32_type().into();
        let string_encode_char_ty = context.i64_type().fn_type(&[u32_md, ptr_md], false);
        let karac_string_encode_char_fn = module.add_function(
            "karac_string_encode_char",
            string_encode_char_ty,
            Some(Linkage::External),
        );

        // karac_map_entry(map: ptr, key: ptr, out_slot_ptr: ptr) -> i1
        let map_entry_ty = context
            .bool_type()
            .fn_type(&[ptr_md, ptr_md, ptr_md], false);
        let karac_map_entry_fn =
            module.add_function("karac_map_entry", map_entry_ty, Some(Linkage::External));

        // karac_map_lookup_slot(map: ptr, key: ptr, out_slot_ptr: ptr) -> i1
        let map_lookup_slot_ty = context
            .bool_type()
            .fn_type(&[ptr_md, ptr_md, ptr_md], false);
        let karac_map_lookup_slot_fn = module.add_function(
            "karac_map_lookup_slot",
            map_lookup_slot_ty,
            Some(Linkage::External),
        );

        // karac_string_clone(src: ptr, dst: ptr) -> void
        let string_clone_ty = context.void_type().fn_type(&[ptr_md, ptr_md], false);
        let karac_string_clone_fn = module.add_function(
            "karac_string_clone",
            string_clone_ty,
            Some(Linkage::External),
        );

        // karac_string_slice(data: ptr, len: i64, start: i64, end: i64) -> ptr
        // Returns a fresh heap buffer holding `data[start..end]` (NUL-
        // terminated); null for an empty slice. Validates bounds + UTF-8
        // char boundaries, exit(1) on violation. See `runtime/src/clone.rs`.
        let string_slice_ty = ptr_type.fn_type(&[ptr_md, i64_ty, i64_ty, i64_ty], false);
        let karac_string_slice_fn = module.add_function(
            "karac_string_slice",
            string_slice_ty,
            Some(Linkage::External),
        );

        // karac_string_slice_borrow(data, len, start, end) -> ptr
        // Validates identically to karac_string_slice but returns a pointer
        // *into* the source (`data + start`) without allocating — backs a
        // borrowed `{ptr, len, cap=0}` String view used as a non-retained map
        // key. See `runtime/src/clone.rs`.
        let karac_string_slice_borrow_fn = module.add_function(
            "karac_string_slice_borrow",
            string_slice_ty,
            Some(Linkage::External),
        );

        // ── Error return trace runtime ────────────────────────────────
        // karac_error_trace_push(file_ptr: ptr, file_len: i64, line: i32, col: i32) -> void
        let i32_ty = context.i32_type();
        let trace_push_ty = context.void_type().fn_type(
            &[
                ptr_md,
                i64_ty,
                BasicMetadataTypeEnum::from(i32_ty),
                BasicMetadataTypeEnum::from(i32_ty),
            ],
            false,
        );
        let karac_error_trace_push_fn = module.add_function(
            "karac_error_trace_push",
            trace_push_ty,
            Some(Linkage::External),
        );
        // karac_error_trace_clear() -> void
        let trace_clear_ty = context.void_type().fn_type(&[], false);
        let karac_error_trace_clear_fn = module.add_function(
            "karac_error_trace_clear",
            trace_clear_ty,
            Some(Linkage::External),
        );

        // ── Test-runner outcome bridge (Slice c.1) ─────────────────
        // void karac_test_record_failure(
        //     ptr file_ptr, i64 file_len,
        //     i32 line, i32 col,
        //     ptr msg_ptr,  i64 msg_len,
        //     ptr left_ptr,  i64 left_len,
        //     ptr right_ptr, i64 right_len,
        // )
        let test_record_failure_ty = context.void_type().fn_type(
            &[
                ptr_md,
                i64_ty,
                BasicMetadataTypeEnum::from(i32_ty),
                BasicMetadataTypeEnum::from(i32_ty),
                ptr_md,
                i64_ty,
                ptr_md,
                i64_ty,
                ptr_md,
                i64_ty,
            ],
            false,
        );
        let karac_test_record_failure_fn = module.add_function(
            "karac_test_record_failure",
            test_record_failure_ty,
            Some(Linkage::External),
        );

        Codegen {
            context,
            module,
            builder,
            variables: HashMap::new(),
            var_type_names: HashMap::new(),
            tuple_var_elem_type_names: HashMap::new(),
            inline_option_payload_vars: std::collections::HashSet::new(),
            inline_result_payload_vars: std::collections::HashSet::new(),
            inline_option_map_payload_vars: std::collections::HashSet::new(),
            refinement_bases: HashMap::new(),
            refinement_generic_params: HashMap::new(),
            distinct_bases: HashMap::new(),
            refinement_predicates: HashMap::new(),
            current_contract_ensures: Vec::new(),
            current_contract_result_type: None,
            contract_old_snapshots: HashMap::new(),
            current_method_invariants: Vec::new(),
            constructor_invariant_self_type: None,
            strip_contracts: read_strip_contracts_env(),
            strip_error_trace: read_strip_error_trace_env(),
            karac_runtime_enter_predicate_fn,
            karac_runtime_exit_predicate_fn,
            karac_runtime_panic_prefix_fn,
            runtime_panic_prefix_needed: true,
            panic_site_counter: std::cell::Cell::new(0),
            atomic_var_inner_is_bool: HashSet::new(),
            current_fn: None,
            printf_fn,
            snprintf_fn,
            fwrite_fn,
            stdout_global,
            stderr_global,
            struct_types: HashMap::new(),
            state_struct_types: HashMap::new(),
            state_machine_poll_fns: HashMap::new(),
            state_machine_state_constructors: HashMap::new(),
            state_machine_state_destructors: HashMap::new(),
            program_snapshot: None,
            state_machine_return_types: HashMap::new(),
            struct_field_names: HashMap::new(),
            struct_field_type_names: HashMap::new(),
            struct_field_type_exprs: HashMap::new(),
            union_types: HashMap::new(),
            union_field_types: HashMap::new(),
            enum_layouts: HashMap::new(),
            enum_unit_variants: HashMap::new(),
            seeded_enum_names: HashSet::new(),
            loop_stack: Vec::new(),
            generic_fns: HashMap::new(),
            generated_monos: HashSet::new(),
            type_subst: HashMap::new(),
            const_subst: HashMap::new(),
            closure_counter: 0,
            indexed_elem_counter: 0,
            closure_fn_types: HashMap::new(),
            pending_closure_fn_type: None,
            pending_closure_param_hints: None,
            pending_map_insert_old_dec: false,
            last_fstr_acc: None,
            shared_types: HashMap::new(),
            malloc_fn,
            alloc_fallible_fn,
            alloc_or_panic_fn,
            free_fn,
            exit_fn,
            memcmp_fn,
            sched_yield_fn,
            len_alias: HashMap::new(),
            asserted_index_bounds: Vec::new(),
            vec_elem_types: HashMap::new(),
            pending_let_elem_type: None,
            slice_elem_types: HashMap::new(),
            fn_param_slice_elem: HashMap::new(),
            ref_params: HashMap::new(),
            owned_vecstr_params: HashSet::new(),
            owned_struct_params: HashSet::new(),
            fn_param_ref: HashMap::new(),
            extern_link_names: HashMap::new(),
            fn_return_type_names: HashMap::new(),
            fn_return_type_exprs: HashMap::new(),
            fn_ref_return_inner: HashMap::new(),
            fn_return_option_inner_shared: HashMap::new(),
            fn_niche_abi: HashMap::new(),
            var_option_shared_heap: HashMap::new(),
            tail_ret_inner: None,
            soa_layouts: HashMap::new(),
            scope_cleanup_actions: Vec::new(),
            pending_errdefer_payload: None,
            current_fn_err_payload_ty: None,
            main_result_err_te: None,
            main_returns_exitcode: false,
            current_fn_returns_ref: false,
            compiling_ref_return_let_rhs: false,
            pattern_binding_is_borrow: false,
            pattern_binding_scrutinee_is_option_result: false,
            enum_drop_fns: HashMap::new(),
            struct_drop_fns: HashMap::new(),
            user_drop_wrapper_fns: HashMap::new(),
            rc_drop_fns: HashMap::new(),
            question_conversions: HashMap::new(),
            callee_effectful: HashMap::new(),
            method_callee_types: HashMap::new(),
            call_effect_subs: crate::ast::CallEffectSubsTable::new(),
            method_unwrap_inner_types: HashMap::new(),
            channel_elem_types: HashMap::new(),
            ref_return_inner_types: HashMap::new(),
            user_ref_method_names: std::collections::HashSet::new(),
            string_typed_exprs: HashSet::new(),
            tensor_typed_exprs: HashMap::new(),
            tensor_var_infos: HashMap::new(),
            pending_let_tensor_info: None,
            unsigned_vector_exprs: HashSet::new(),
            expr_struct_type_names: HashMap::new(),
            user_ord_typed_exprs: HashMap::new(),
            owned_temp_drops: HashMap::new(),
            raw_pointer_pointee_types: HashMap::new(),
            enum_inst_type_exprs: HashMap::new(),
            enum_inst_var_types: HashMap::new(),
            pattern_binding_types: HashMap::new(),
            pattern_binding_inner_types: HashMap::new(),
            pattern_binding_borrow_modes: HashMap::new(),
            consts: HashMap::new(),
            module_bindings: HashMap::new(),
            source_filename: None,
            source_filename_global: None,
            source_text: None,
            used_symbols: Vec::new(),
            used_data_globals: Vec::new(),
            branch_cancel_ptr: None,
            rc_fallback_fns: HashMap::new(),
            elided_bindings: HashMap::new(),
            elided_cluster_roots: HashMap::new(),
            elided_b2_bindings: HashMap::new(),
            headerless_fns: HashMap::new(),
            adopted_cluster_roots: HashMap::new(),
            headerless_type_candidates: HashMap::new(),
            headerless_types: HashSet::new(),
            conditional_adopted_roots: HashMap::new(),
            borrowed_param_skips: HashMap::new(),
            arc_fallback_fns: HashMap::new(),
            rc_fallback_heap_types: HashMap::new(),
            rc_fallback_box_drop_fns: Vec::new(),
            aggregate_drop_fns: Vec::new(),
            closure_capture_paths: HashMap::new(),
            par_capture_modes: HashMap::new(),
            concurrency_decisions: HashMap::new(),
            current_fn_name: String::new(),
            current_span: None,
            debug_info: None,
            par_counter: 0,
            karac_branch_ty,
            karac_par_run_fn,
            karac_par_reduce_fn,
            spawn_sites: Vec::new(),
            runtime_debug_metadata_enabled: read_runtime_debug_metadata_env(),
            // Env gate OR wasm target — see the field doc-comment
            // (auto-par fan-out is pure overhead on a single-threaded
            // target; phase-10 sequential default).
            auto_par_disabled: !read_auto_par_env() || crate::target::active_target_is_wasm(),
            provider_resource_ids: HashMap::new(),
            provider_resource_traits: HashMap::new(),
            provider_trait_methods: HashMap::new(),
            user_ambient_resource_methods: HashMap::new(),
            provider_vtables: HashMap::new(),
            karac_provider_push_fn,
            karac_provider_pop_fn,
            karac_provider_lookup_fn,
            karac_provider_get_stack_head_fn,
            karac_provider_set_stack_head_fn,
            karac_tracing_get_active_span_fn,
            karac_tracing_set_active_span_fn,
            karac_tracing_get_min_level_fn,
            karac_tracing_set_min_level_fn,
            karac_tracing_set_exporter_fn,
            karac_tracing_get_exporter_data_fn,
            karac_tracing_get_exporter_fn_fn,
            karac_tracing_reset_fn,
            provider_frame_ty,
            provider_lookup_result_ty,
            map_key_types: HashMap::new(),
            map_val_types: HashMap::new(),
            map_key_type_names: HashMap::new(),
            var_elem_type_exprs: HashMap::new(),
            map_key_type_exprs: HashMap::new(),
            set_elem_types: HashMap::new(),
            set_elem_type_names: HashMap::new(),
            set_elem_type_exprs: HashMap::new(),
            string_vars: HashSet::new(),
            cstr_vars: HashSet::new(),
            http_shim_cache: HashMap::new(),
            karac_map_new_fn,
            karac_map_free_fn,
            karac_map_free_with_drop_vec_fn,
            karac_map_insert_old_fn,
            karac_map_insert_borrowed_str_old_fn,
            karac_map_get_fn,
            karac_map_remove_old_fn,
            karac_map_contains_fn,
            karac_map_len_fn,
            karac_map_clear_fn,
            karac_map_clear_with_drop_vec_fn,
            karac_map_iter_new_fn,
            karac_map_iter_next_fn,
            karac_map_iter_free_fn,
            karac_string_decode_char_fn,
            karac_string_encode_char_fn,
            karac_map_entry_fn,
            karac_map_lookup_slot_fn,
            karac_string_clone_fn,
            karac_string_slice_fn,
            karac_string_slice_borrow_fn,
            clone_fn_cache: HashMap::new(),
            try_clone_fn_cache: HashMap::new(),
            drop_fn_cache: HashMap::new(),
            map_mono_methods: HashMap::new(),
            display_fn_cache: HashMap::new(),
            karac_error_trace_push_fn,
            karac_error_trace_clear_fn,
            karac_test_record_failure_fn,
            target_data: None,
            hot_swap_enabled: false,
            declare_only_fns: std::collections::HashSet::new(),
            main_symbol_override: None,
            force_external_linkage: false,
            snapshot_capture: HashMap::new(),
            snapshot_replay: HashMap::new(),
            hot_swap_slots: HashMap::new(),
            hot_swap_fns: Vec::new(),
            coro_enabled: false,
            coro_fn_keys: HashSet::new(),
            coro_ctx: None,
            coro_park_counter: 0,
            coro_spawn_slot: None,
        }
    }

    /// Lazily build (and cache) the host-target `TargetData` used by the
    /// layout-introspection intrinsics. Most modules never invoke
    /// `align_of[T]()` / `offset_of[T](field)`, so we pay the
    /// `Target::initialize_native` + `create_target_machine` cost only
    /// when the first such intrinsic is lowered.
    fn ensure_target_data(&mut self) -> Result<&TargetData, String> {
        if self.target_data.is_none() {
            let tm = create_target_machine()?;
            self.target_data = Some(tm.get_target_data());
        }
        Ok(self.target_data.as_ref().unwrap())
    }

    /// Populate RC-fallback data from an ownership check result.
    ///
    /// Two side-tables land here:
    /// * `rc_fallback_fns` — every binding the dataflow flagged for heap-boxing
    ///   + refcount.
    /// * `arc_fallback_fns` — the subset of those that also cross a `par {}`
    ///   thread boundary (Phase 2 promotion). Codegen routes inc/dec on the
    ///   subset to the atomic path (`atomicrmw add` / `atomicrmw sub`,
    ///   `SeqCst`); the rest stay on plain non-atomic ops. Allocation is
    ///   identical for both — the heap shape is `{ refcount: i64, payload: T }`
    ///   regardless of flavor and the initial `refcount = 1` store happens
    ///   before the value is shared.
    fn load_rc_fallback(&mut self, ownership: Option<&OwnershipCheckResult>) {
        let Some(ow) = ownership else { return };
        for (fn_name, rc_map) in &ow.rc_values {
            let names: HashSet<String> = rc_map.keys().cloned().collect();
            self.rc_fallback_fns.insert(fn_name.clone(), names);
        }
        for (fn_name, arc_set) in &ow.arc_values {
            self.arc_fallback_fns
                .insert(fn_name.clone(), arc_set.clone());
        }
        // RC elision phase A: per-fn elided-binding sets. Consulted by
        // the let-stmt shared arm via `is_elided_binding`.
        for (fn_name, names) in &ow.elided_bindings {
            self.elided_bindings.insert(fn_name.clone(), names.clone());
        }
        // RC elision phase B1: cluster roots → free-walk cleanup.
        // Phase B2: role records for displacement-free clusters.
        for (fn_name, clusters) in &ow.elided_clusters {
            let entry = self
                .elided_cluster_roots
                .entry(fn_name.clone())
                .or_default();
            for c in clusters {
                if c.adopted {
                    // Phase C1c: adopted roots live in their own map —
                    // the literal-cluster let-site / tail-transfer
                    // paths must never see them (the root is Option-
                    // typed, not a bare member literal). C2b: families
                    // that used the sanctioned-arg channel are
                    // CONDITIONAL — consulted only when the member
                    // type survives the headerless reconcile.
                    let target = if c.arg_sanctioned {
                        &mut self.conditional_adopted_roots
                    } else {
                        &mut self.adopted_cluster_roots
                    };
                    target
                        .entry(fn_name.clone())
                        .or_default()
                        .insert(c.root.clone(), (c.member_type.clone(), c.link_field_index));
                    continue;
                }
                // Phase C2a: borrowed-param families have NO root
                // cleanup of their own (the params keep the balanced
                // entry/exit ownership) — only their walk cursors take
                // the count-skip roles below. C2b records the params
                // for the conditional residual-count skips.
                if c.borrowed {
                    let recs = self
                        .borrowed_param_skips
                        .entry(fn_name.clone())
                        .or_default();
                    for (pname, pos) in &c.borrowed_params {
                        recs.push((pname.clone(), *pos, c.member_type.clone()));
                    }
                    continue;
                }
                entry.insert(
                    c.root.clone(),
                    (c.member_type.clone(), c.link_field_index, c.returned),
                );
            }
            for c in clusters {
                if !c.b2 {
                    continue;
                }
                let b2_entry = self.elided_b2_bindings.entry(fn_name.clone()).or_default();
                let mk = |role: state::B2Role| state::B2Binding {
                    role,
                    member_type: c.member_type.clone(),
                    link_field_index: c.link_field_index,
                };
                // Adopted roots are Option-typed call results, not
                // literal roots — their cleanup dispatch happens via
                // `adopted_root_info`; only their cursors take the
                // count-free roles.
                if !c.adopted && !c.borrowed {
                    b2_entry.insert(c.root.clone(), mk(state::B2Role::Root));
                }
                for n in &c.fresh_linked {
                    b2_entry.insert(n.clone(), mk(state::B2Role::Fresh));
                }
                for n in &c.bare_cursors {
                    b2_entry.insert(n.clone(), mk(state::B2Role::BareCursor));
                }
                for n in &c.option_cursors {
                    b2_entry.insert(n.clone(), mk(state::B2Role::OptionCursor));
                }
            }
            // Phase D: headerless member layout for this (fn, type).
            for c in clusters {
                if c.headerless {
                    self.headerless_fns
                        .entry(fn_name.clone())
                        .or_default()
                        .insert(c.member_type.clone(), c.link_field_index);
                }
            }
        }
        // Disjoint-capture slice 4: per-closure capture-path mode set
        // (slice 2 output). Drives the per-path env-struct layout in
        // `compile_closure` when the closure expression's `SpanKey` is
        // present in this map; absent → per-name fallback.
        for (k, v) in &ow.closure_capture_path_modes {
            self.closure_capture_paths.insert(*k, v.clone());
        }
        // L227: per-par-block capture modes. Drives the `SharedRc` arm
        // in `emit_par_branch_fn`'s capture-unpack loop (atomic rc_inc
        // in the branch prologue + `track_rc_var` registration). Names
        // absent from the inner Vec fall through to `Copy` semantics
        // (today's behavior).
        for (k, v) in &ow.par_capture_modes {
            self.par_capture_modes.insert(*k, v.clone());
        }
        // Phase C2b: headerless-T candidates (reconciled in
        // `compile_program` once coro keys + struct layouts exist).
        for (t, v) in &ow.headerless_types {
            self.headerless_type_candidates.insert(t.clone(), v.clone());
        }
    }

    /// Set the source filename used for `karac_error_trace_push` calls at
    /// `?` failure sites. See the field doc on `source_filename`.
    fn set_source_filename(&mut self, filename: Option<&str>) {
        self.source_filename = filename.map(|s| s.to_string());
    }

    /// Set the source text used by `record_spawn_site` to resolve byte
    /// offsets to `(line, col)` for the `KARAC_SPAWN_SITES` metadata
    /// table (Debugger Contract slice 3). Mirrors `set_source_filename`.
    fn set_source_text(&mut self, text: Option<&str>) {
        self.source_text = text.map(|s| s.to_string());
    }

    /// Set the phase-7 line-5 `--enable-hot-swap` flag. When `true`,
    /// pub-fn declarations register a slot in `@karac_hotswap_table`
    /// during emission, and call sites to those callees are lowered as
    /// load + indirect call. See [`compile_to_object_with_hot_swap`].
    fn set_hot_swap_enabled(&mut self, enabled: bool) {
        self.hot_swap_enabled = enabled;
    }

    /// Override the contract-stripping decision (design.md § Contracts:
    /// "stripped in release"). `true` elides all `requires` / `ensures` /
    /// `old(...)` / `invariant` emission; `false` keeps them. The default
    /// comes from `KARAC_STRIP_CONTRACTS` at construction; this setter lets
    /// the release-build path and IR tests force the decision without relying
    /// on the process-global env var.
    pub(crate) fn set_strip_contracts(&mut self, strip: bool) {
        self.strip_contracts = strip;
    }

    /// Override the `?`-error-return-trace stripping decision (peer to
    /// `set_strip_contracts`). `true` elides the `karac_error_trace_push` /
    /// `_clear` instrumentation; `false` keeps it. Default from
    /// `KARAC_STRIP_ERROR_TRACE`; the `release` build path forces it on.
    pub(crate) fn set_strip_error_trace(&mut self, strip: bool) {
        self.strip_error_trace = strip;
    }

    /// Enable the A2 slice 2b.3 coroutine compilation path (default off). When
    /// set before `compile_program`, network-boundary functions compile as LLVM
    /// coroutines with the dispatcher-driven slot-wait drive instead of the
    /// degenerate `emit_state_machine_poll_fn_for_key` body-splitter. Race-free
    /// (no process-global env), mirroring `set_strip_contracts`. See
    /// docs/spikes/network-async-coroutine-transform.md § 6¾.
    pub(crate) fn set_coro_enabled(&mut self, enabled: bool) {
        self.coro_enabled = enabled;
    }

    /// Mark this compile as the **threaded pass** of a `--features
    /// wasm-threads` dual-artifact build (phase-10 wasm-threads entry):
    /// re-derives `auto_par_disabled` with the wasm disable lifted —
    /// the threaded module has a real worker pool, so auto-par fan-outs
    /// are re-enabled there (the env gate still applies). Deliberately
    /// a plain-data setter, NOT a process-global: one build process
    /// runs codegen twice (sequential pass then threaded pass), and the
    /// sequential pass must keep today's wasm-disabled derivation —
    /// flipping a global between passes would also retroactively skew
    /// every other `active_target_is_wasm()`-keyed read. Race-free,
    /// mirroring `set_strip_contracts`. Must run before
    /// `compile_program` (it only re-derives the construction-time
    /// field; nothing reads `auto_par_disabled` earlier).
    pub(crate) fn set_wasm_threaded_pass(&mut self, threaded: bool) {
        self.auto_par_disabled =
            !read_auto_par_env() || (crate::target::active_target_is_wasm() && !threaded);
    }

    /// Whether `fn_key` is compiled as a coroutine this run (A2 slice 2b.3) —
    /// i.e. `coro_enabled` and the key is a non-generic network-boundary
    /// function (`coro_fn_keys`, populated in `compile_program`). The single
    /// predicate behind the three coupled coroutine toggles: `ptr` return type
    /// in `declare_function`, poll-fn skip in `emit_state_machine_poll_fns`, and
    /// the slot-wait call-site drive.
    pub(crate) fn is_coroutine_compiled(&self, fn_key: &str) -> bool {
        self.coro_enabled && self.coro_fn_keys.contains(fn_key)
    }

    /// Mint a fresh `SpawnSiteId` and record a `SpawnSiteRecord` for the
    /// par block at `span` with `worker_count` static branches. Returns
    /// the assigned ID. The ID is the value of `par_counter` at entry —
    /// using a single counter for both par-branch function naming and
    /// SpawnSiteId minting keeps `__par_branch_<id>_<i>` and the metadata
    /// table in lockstep.
    ///
    /// Recording happens unconditionally (regardless of
    /// `runtime_debug_metadata_enabled`) so the IDs are stable across
    /// the gate-on / gate-off boundary; the gate decides only whether
    /// the emitted globals are populated. See `Codegen::spawn_sites` and
    /// the slice 3 plan in `phase-8-stdlib-floor.md`.
    fn record_spawn_site(&mut self, span: &Span, worker_count: Option<u32>) -> u32 {
        let id = self.par_counter;
        self.par_counter += 1;
        let (line, col) = match self.source_text.as_deref() {
            Some(src) => {
                let (l, c) = crate::byte_offset_to_line_col(src, span.offset);
                (l as u32, c as u32)
            }
            None => (span.line as u32, span.column as u32),
        };
        let file = self.source_filename.clone().unwrap_or_default();
        self.spawn_sites.push(SpawnSiteRecord {
            id,
            file,
            line,
            col,
            worker_count,
        });
        id
    }

    fn is_rc_fallback_binding(&self, name: &str) -> bool {
        self.rc_fallback_fns
            .get(&self.current_fn_name)
            .is_some_and(|set| set.contains(name))
    }

    /// True iff `name` is an RC-elided shared binding in the current
    /// function (ownership phase-A elision). The let-site routes these
    /// to `track_elided_shared_var` (unconditional free) instead of
    /// `track_rc_var`. Nested-fn compiles (closures, par branches)
    /// never see elided names — the analysis blocks any candidate
    /// mentioned inside those regions.
    fn is_elided_binding(&self, name: &str) -> bool {
        self.elided_bindings
            .get(&self.current_fn_name)
            .is_some_and(|set| set.contains(name))
    }

    /// Phase-B1 cluster-root lookup for the current function: returns
    /// (member type name, link user-field index, C1b return mode) when
    /// `name` is a cluster root. Mode `No` → the cleanup takes the
    /// free-walk; `RootLink` → root-only free (the chain transfers out
    /// through the sanctioned tail link read); `SomeRoot` → no cleanup
    /// at all (the whole cluster transfers to the caller).
    fn cluster_root_info(
        &self,
        name: &str,
    ) -> Option<(String, usize, crate::ownership::ReturnedChain)> {
        self.elided_cluster_roots
            .get(&self.current_fn_name)
            .and_then(|m| m.get(name))
            .cloned()
    }

    /// Phase C1c adopted-root lookup for the current function:
    /// `(member type, link user-field index)` when `name` is an
    /// adopted cluster root (an `Option[shared T]` builder-call result
    /// whose scope-exit cleanup is the option-guarded free-walk).
    fn adopted_root_info(&self, name: &str) -> Option<(String, usize)> {
        if let Some(info) = self
            .adopted_cluster_roots
            .get(&self.current_fn_name)
            .and_then(|m| m.get(name))
        {
            return Some(info.clone());
        }
        // C2b conditional families (sanctioned-arg users): active only
        // under the reconciled headerless set — otherwise the binding
        // falls back to full RC and the ordinary arg-inc / exit-dec
        // balance applies.
        self.conditional_adopted_roots
            .get(&self.current_fn_name)
            .and_then(|m| m.get(name))
            .filter(|(t, _)| self.headerless_types.contains(t))
            .cloned()
    }

    /// Phase C2b: skip the call-site `Option[shared T]` arg inc when
    /// the callee's param at `position` is a borrowed-family param of
    /// a reconciled headerless type (the callee's exit dec is skipped
    /// symmetrically — see `compile_function`'s param registration).
    fn borrowed_arg_skip(&self, callee: &str, position: usize) -> bool {
        self.borrowed_param_skips.get(callee).is_some_and(|recs| {
            recs.iter()
                .any(|(_, pos, t)| *pos == position && self.headerless_types.contains(t))
        })
    }

    /// Phase C2b: the callee-side half — `param_name` of the CURRENT fn
    /// is a borrowed-family param of a reconciled headerless type, so
    /// its exit `RcDecOption` is skipped (no caller inc ever happened).
    fn borrowed_param_dec_skip(&self, param_name: &str) -> bool {
        self.borrowed_param_skips
            .get(&self.current_fn_name)
            .is_some_and(|recs| {
                recs.iter()
                    .any(|(n, _, t)| n == param_name && self.headerless_types.contains(t))
            })
    }

    /// Phase-B2 role lookup for the current function.
    fn b2_binding(&self, name: &str) -> Option<&state::B2Binding> {
        self.elided_b2_bindings
            .get(&self.current_fn_name)
            .and_then(|m| m.get(name))
    }

    /// Phase D: true when values of `type_name` use the headerless
    /// layout in the current function. Two lazy demotions on top of
    /// the analysis flag: coroutine fns (their bodies re-emit under
    /// ramp/resume splitting — untested layout territory) and a
    /// non-niche link slot (would make the free-walk's RcDec fallback
    /// reachable against a header that does not exist).
    pub(crate) fn headerless_here(&self, type_name: &str) -> bool {
        // Phase C2b: program-wide headerless types answer true in
        // EVERY fn — the reconcile already excluded coroutine touchers
        // and non-niche links, and layout uniformity is the invariant
        // (a per-fn demotion here would mix layouts on one object).
        if self.headerless_types.contains(type_name) {
            return true;
        }
        let Some(link_idx) = self
            .headerless_fns
            .get(&self.current_fn_name)
            .and_then(|m| m.get(type_name))
        else {
            return false;
        };
        if self.is_coroutine_compiled(&self.current_fn_name) {
            return false;
        }
        self.niche_field_inner_heap_type(type_name, *link_idx)
            .is_some()
    }

    /// Phase D layout resolution for shared-struct member-field access:
    /// `(struct type to GEP with, heap index base for user field 0)`.
    /// Headered: `(heap_type, 1)` — index 0 is the rc word. Headerless:
    /// `(twin, 0)` where the twin is `heap_type` minus the rc slot
    /// (anonymous struct types are uniqued by LLVM, so rebuilding per
    /// call site is free). Every site that GEPs / allocs member fields
    /// MUST route through this helper — the centralization is what
    /// keeps the two layouts from ever mixing on one object. The only
    /// deliberate exceptions are the `sh_call_` / `sh_idx_` field
    /// paths (call-result and collection-element receivers): those
    /// receiver shapes are structurally impossible for a headerless
    /// type (the purity gate excludes calls returning the type and any
    /// collection mention), and a headered GEP is correct for every
    /// value that CAN reach them.
    pub(crate) fn shared_gep_layout(
        &self,
        type_name: &str,
        heap_type: inkwell::types::StructType<'ctx>,
    ) -> (inkwell::types::StructType<'ctx>, u32) {
        if self.headerless_here(type_name) {
            let fields: Vec<inkwell::types::BasicTypeEnum<'ctx>> =
                heap_type.get_field_types().into_iter().skip(1).collect();
            (self.context.struct_type(&fields, false), 0)
        } else {
            (heap_type, 1)
        }
    }

    /// True when `name` is a non-owning B2 binding (fresh node or
    /// cursor) — no count ops, no cleanup registration.
    fn b2_skips_counts(&self, name: &str) -> bool {
        self.b2_binding(name).is_some_and(|b| {
            matches!(
                b.role,
                state::B2Role::Fresh | state::B2Role::BareCursor | state::B2Role::OptionCursor
            )
        })
    }

    /// True iff `name` was promoted to Arc in the current function — i.e. it
    /// lives in the `arc_values` subset for this function key. Inc/dec on
    /// such bindings must use the atomic path.
    fn is_arc_binding(&self, name: &str) -> bool {
        self.arc_fallback_fns
            .get(&self.current_fn_name)
            .is_some_and(|set| set.contains(name))
    }

    /// Populate concurrency-analysis data from a `ConcurrencyAnalysis` result.
    ///
    /// Mirrors `load_rc_fallback`: walks `analysis.function_decisions` and
    /// clones each entry into `concurrency_decisions`. The plumbing is
    /// behavior-neutral — slice 2 will consume the loaded decisions to emit
    /// `karac_par_run` for compiler-inferred parallel groups outside
    /// explicit `par {}` blocks. `None` is a no-op (the existing `par`
    /// codegen path stays in effect).
    fn load_concurrency_analysis(&mut self, analysis: Option<&ConcurrencyAnalysis>) {
        let Some(an) = analysis else { return };
        for (fn_name, decision) in &an.function_decisions {
            self.concurrency_decisions
                .insert(fn_name.clone(), decision.clone());
        }
    }

    /// Look up the parallelization decision for the function currently being
    /// compiled. Returns `None` when no concurrency analysis was threaded in
    /// (the legacy entry-point path) or when the current function isn't
    /// keyed in the analysis (e.g. compiler-synthesized helpers). Slice 2
    /// consumes this in `compile_function_body` to decide whether to emit
    /// `karac_par_run` for compiler-inferred parallel groups outside
    /// explicit `par {}` blocks.
    fn parallel_groups_for_current_fn(&self) -> Option<&FunctionConcurrency> {
        if self.concurrency_decisions.is_empty() {
            return None;
        }
        self.concurrency_decisions.get(&self.current_fn_name)
    }

    /// Look up the recognized reduction (if any) for the loop statement at
    /// `stmt_index` in the current function's body. Slice 3a-only utility
    /// — the call sites that consume the returned `LoopReduction` to emit
    /// a `karac_par_reduce` lowering land in slice 3b. Returns `None`
    /// when no concurrency analysis was threaded in, when the function
    /// has no recognized reductions, or when `stmt_index` isn't the
    /// index of a recognized loop. The analyzer (slice 1, `src/concurrency.
    /// rs`) only emits reductions for top-level loop statements, so this
    /// lookup is by stmt index in `func.body.stmts`.
    pub(crate) fn loop_reduction_for_stmt(
        &self,
        stmt_index: usize,
    ) -> Option<&crate::concurrency::LoopReduction> {
        let decision = self.concurrency_decisions.get(&self.current_fn_name)?;
        decision
            .loop_reductions
            .iter()
            .find(|r| r.stmt_index == stmt_index)
    }

    // ── Program / function compilation ───────────────────────────

    /// Populate the refinement-alias and distinct-type base maps from the
    /// user program (plus baked-stdlib distinct types). Called early in
    /// `compile_program`, *before* struct/enum layouts are built, so a field
    /// whose type names a refinement (`type Email = String where …`) or a
    /// distinct type resolves to the base's layout while the aggregate is
    /// lowered — not after, where the name would hit the `i64` unknown-name
    /// fall-through and mis-size the field.
    fn populate_type_alias_bases(&mut self, program: &Program) {
        // Refinement type aliases (`type Email = String where …`): record
        // each one's base `TypeExpr` so type lowering resolves the
        // refinement to its base's layout (phase-9 step 4). A refinement
        // carries no runtime tag — it is layout-identical to its base.
        for item in &program.items {
            if let Item::TypeAlias(t) = item {
                if let Some(pred) = &t.refinement {
                    self.refinement_bases.insert(t.name.clone(), t.ty.clone());
                    self.refinement_predicates
                        .insert(t.name.clone(), pred.clone());
                    // Generic refinement (`type NonEmpty[T] = Vec[T] where …`):
                    // remember the param names so a use at concrete arity
                    // substitutes the right element type into the base.
                    if let Some(gp) = &t.generic_params {
                        self.refinement_generic_params.insert(
                            t.name.clone(),
                            gp.params.iter().map(|p| p.name.clone()).collect(),
                        );
                    }
                }
            }
        }

        // Distinct types (`distinct type UserId = i64`): record each one's
        // base `TypeExpr` so type lowering resolves the distinct type to its
        // base's layout (zero-cost wrapper, no runtime tag). Unlike a
        // refinement, this base is consulted only for layout — a distinct
        // type keeps its own name for value-level dispatch. design.md
        // § Distinct Types (Newtypes).
        for item in &program.items {
            if let Item::DistinctType(d) = item {
                self.distinct_bases
                    .insert(d.name.clone(), d.base_type.clone());
                // Combined `distinct type T = Base where pred`: register the
                // predicate so the `T(value)` constructor emits the runtime
                // assertion via `emit_refinement_assert`. Keyed by the
                // distinct name, parallel to refinements.
                if let Some(pred) = &d.refinement {
                    self.refinement_predicates
                        .insert(d.name.clone(), pred.clone());
                }
            }
        }
        // Baked-stdlib `distinct type`s (e.g. `ExitCode` — Phase-8
        // entry-point contract Slice B). The user `program` carries only
        // user items, so a stdlib distinct type's `T(value)` constructor
        // (`ExitCode(code)`) and its bare-name layout (`-> ExitCode`
        // lowering to its i32 base) would otherwise be unrecognized. User
        // entries win on collision (registered first; `entry().or_insert`).
        for (_, sp) in crate::prelude::STDLIB_PROGRAMS.iter() {
            for item in &sp.items {
                if let Item::DistinctType(d) = item {
                    self.distinct_bases
                        .entry(d.name.clone())
                        .or_insert_with(|| d.base_type.clone());
                    if let Some(pred) = &d.refinement {
                        self.refinement_predicates
                            .entry(d.name.clone())
                            .or_insert_with(|| pred.clone());
                    }
                }
            }
        }
    }

    fn compile_program(&mut self, program: &Program) -> Result<(), String> {
        // Decide whether `emit_panic` needs the runtime fault-category prefix
        // before ANY function compiles — the first panic site bakes the
        // decision in. Contract-free programs (the overwhelmingly common
        // case) fold the prefix static, dead-stripping the
        // `karac_runtime_panic_prefix` thread-local's __DATA page and keeping
        // panic landing pads leaf blocks; see the field doc on
        // `runtime_panic_prefix_needed` for the measured costs this avoids.
        // REPL cell modules (`main_symbol_override` set) always keep the
        // runtime read: a cell can call contracted functions JIT'd from
        // earlier cells, which this module's item scan cannot see.
        self.runtime_panic_prefix_needed = self.main_symbol_override.is_some()
            || (!self.strip_contracts && contracts::program_declares_contracts(program));
        // Level 2 crash diagnostics — Part 2: stand up DWARF debug-info state
        // before any function compiles (no-op unless KARAC_DEBUG_INFO is set and
        // a source filename was threaded in via set_source_filename, which runs
        // before compile_program).
        self.di_init();
        // Seed `Option` / `Result` layouts before walking struct fields so
        // a `shared struct N { mut left: Option[N] }` declaration's field-
        // type lowering finds the `{i64 tag, i64 payload}` layout via
        // `llvm_type_for_name("Option")` and embeds a 2-word slot in the
        // heap struct, rather than collapsing the field to the default
        // `i64` and losing the payload word.
        self.seed_builtin_enum_layouts();
        // Seed baked stdlib struct types (`Client`, `Response`,
        // `HttpError`) so pattern_payload_word_count + field-access
        // GEPs find the right LLVM shape. Same rationale as the enum
        // seeding above. Must run before declare_structs so a user
        // program with `struct Response { ... }` (unlikely but legal)
        // can override the seeded shape.
        self.seed_builtin_struct_types();
        // Refinement-alias / distinct-type base maps MUST be populated before
        // struct + enum layouts are built: a field whose type names a
        // refinement (`email: BoundedText`) or distinct type lowers via
        // `llvm_type_for_type_expr`, which consults `refinement_bases` /
        // `distinct_bases` to reach the base's real layout. Run too late and
        // those names hit the `i64` unknown-name fall-through, mis-sizing
        // every refinement-typed field (the construction-vs-layout type
        // mismatch surfaced by the Weave dogfood: `{i64,i64,i64}` slots fed a
        // `String`/`f64`/`i64` row).
        self.populate_type_alias_bases(program);
        // Two-pass struct declaration with `declare_enums` interleaved, so a
        // struct field that names a user enum lowers at the enum's real
        // tagged-union shape instead of collapsing to the `i64` fall-through
        // (the self-hosting `enum-in-struct-field` blocker). (1) register
        // struct field metadata (AST, no LLVM types); (2) build enum layouts —
        // their payload sizing recurses through that struct metadata, not the
        // not-yet-built struct LLVM types; (3) build struct LLVM types, now
        // that `enum_layouts` is populated. See
        // `declarations.rs::register_struct_metadata` for the cycle rationale.
        self.register_struct_metadata(program);
        self.declare_enums(program);
        self.build_struct_types(program);
        // Phase 5 line 569 slice 4: lower `#[repr(C)] union Foo { ... }`
        // declarations to LLVM storage types so `size_of[Foo]` /
        // `align_of[Foo]` resolve correctly and union literals /
        // field accesses can target the storage struct downstream.
        // Runs after `build_struct_types` so a union field whose type
        // names a user struct resolves to the right LLVM aggregate
        // when computing primary-field alignment.
        self.declare_unions(program);
        // A2 slice 2b.3: when the coroutine path is enabled, record which
        // network-boundary keys compile as coroutines — every
        // `state_struct_layouts` key that isn't generic (per-mono generic
        // poll-fns are emitted at `compile_generic_call` time and stay on the
        // degenerate path for this slice). `KARAC_PARK_ON_FD` is the leaf
        // primitive and never lands in `state_struct_layouts`, so it's
        // naturally excluded. This must run before `declare_function` so the
        // `ptr`-return signature toggle sees the right set. Drives all three
        // coupled toggles via `is_coroutine_compiled`.
        if self.coro_enabled {
            for key in program.state_struct_layouts.keys() {
                // `main` is the C-ABI `i32 ()` entry point — it can't be a
                // caller-driven coroutine ramp (and isn't called by anyone), so
                // it stays on the existing thread-block park path even if it
                // parks (e.g. a top-level `accept`).
                //
                // Both free fns and `Type.method` keys (dotted) are eligible (A2
                // slice 2b.4(b) wired the method-call intercept's receiver-as-
                // self ramp-drive). Generics stay on the per-mono degenerate
                // path. `KARAC_PARK_ON_FD` is the leaf primitive and never lands
                // in `state_struct_layouts`.
                if key != "main" && !declarations::is_generic_fn_key(program, key) {
                    self.coro_fn_keys.insert(key.clone());
                }
            }
        }
        // Phase C2b reconcile: a headerless-T candidate survives iff
        // none of its touching fns compiles as a coroutine (frame
        // layout authority differs) and the link slot is niched (the
        // free-walks' dec fallback must stay unreachable). Runs here —
        // after `declare_structs` (niche tables) and the coro-key
        // population above, before any function body compiles.
        let candidates: Vec<(String, (usize, Vec<String>))> = self
            .headerless_type_candidates
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        for (t, (link_idx, fns)) in candidates {
            if fns.iter().any(|k| self.is_coroutine_compiled(k)) {
                continue;
            }
            if self.niche_field_inner_heap_type(&t, link_idx).is_none() {
                continue;
            }
            self.headerless_types.insert(t);
        }
        // Slice 8v Phase 2: snapshot the whole `Program` as `Rc<Program>`
        // so the per-mono state-machine emission path triggered from
        // `compile_generic_call` can access layouts / yield points /
        // function ASTs without `&Program` being plumbed through
        // `compile_expr` → `compile_call` → `compile_generic_call`.
        // Cheap `Rc` clones flow to per-mono callers as they fire.
        self.program_snapshot = Some(Rc::new(program.clone()));
        // Phase 6 line 26 slice 5: emit one `%kara.state.<fn_key>` LLVM
        // struct per entry in `program.state_struct_layouts` (populated
        // by the cli pipeline from slice 4). Must precede function-body
        // lowering so the slice-6+ state-machine transform pass can look
        // up the struct type at body-rewrite time. Empty when no
        // network-boundary functions exist (the common case).
        self.emit_state_struct_types(program);
        // Phase-5 auto-par divergence (A2a-2.2): emit the async-sleep timer
        // primitive family (state-struct + poll-fn + constructor) in one
        // shot — self-contained (calls only runtime FFIs), so unlike the fd
        // family it needs no split across the constructor / poll-fn passes.
        // `sleep_ms` call sites compose with it via
        // `emit_state_machine_invocation_for_park_on_timer`.
        self.emit_park_on_timer_family();
        // Phase 6 line 26 slice 8c: emit a state-struct constructor
        // helper per state-struct entry. Caller-side wiring in
        // slice 8d+ replaces direct calls to network-boundary fns
        // with a `__kara_state_new_<fn_key>` invocation + initial
        // poll-fn invocation.
        self.emit_state_machine_state_constructors(program);
        // Phase 6 line 26 slice 8u: emit a state-struct destructor
        // helper per state-struct entry with at least one heap-bearing
        // captured-local (Vec/String, shared struct). The destructor is
        // the unified unwind primitive the future `?`-Err-propagation
        // and cooperative-cancel use sites both invoke; slice 8u lands
        // the primitive only, not the use sites.
        self.emit_state_machine_state_destructors(program);
        self.collect_soa_layouts(program);
        self.declare_extern_functions(program)?;

        // Side-table set by `lowering::lower_program`: at each `?` site that
        // requires `From`-based error conversion, the target type name is
        // recorded so `compile_question` can emit `Target.from(e)` ahead of
        // the early-return.
        self.question_conversions = program.question_conversions.clone();

        // Side-table set by the cli pipeline after effectcheck: per-callee
        // boolean indicating whether the callee carries any observable
        // effect (reads/writes/sends/receives). Read by
        // `emit_branch_cancel_check` to skip the cancel atomic load when the
        // callee is provably non-observable.
        self.callee_effectful = program.callee_effectful.clone();

        // Side-table set by `lowering::lower_program`: each `MethodCall`
        // expression's span maps to the canonical `Type.method` callee key.
        // Read by `compile_method_call` so the par-branch cancel-check
        // narrowing applies to instance methods, not just free-function
        // and `Type.assoc` calls.
        self.method_callee_types = program.method_callee_types.clone();

        // Side-table set by `lowering::lower_program` from
        // `TypeCheckResult.expr_types`: the spans of every `Type::Str`
        // expression. `emit_sort_by_key_inline_thunk` consults this set
        // to dispatch String keys to the `karac_string_cmp` arm — the
        // LLVM struct shape is identical to `Vec[u8]` and a few other
        // 3-word types, so the value alone can't distinguish them.
        self.string_typed_exprs = program.string_typed_exprs.clone();
        // Sibling: per-span Tensor element-type + static-dims info for
        // construction / let-registration / indexing dispatch (see
        // `src/codegen/tensor.rs`).
        self.tensor_typed_exprs = program.tensor_typed_exprs.clone();
        // Sibling: spans of unsigned-element vector expressions, so the SIMD
        // `reduce_min/max` codegen picks `ult`/`ugt` over the signed default.
        self.unsigned_vector_exprs = program.unsigned_vector_exprs.clone();
        // Sibling to `string_typed_exprs` for `Type::Named` struct
        // expressions. Maps span → struct name. `emit_sort_by_key_inline_thunk`
        // consults this to dispatch struct-typed keys (e.g.
        // `sort_by_key(|item| item)` where `item: MyStruct`) to a
        // field-aware lex cascade that picks the right per-field
        // comparator via `self.struct_field_type_names[struct_name]`.
        self.expr_struct_type_names = program.expr_struct_type_names.clone();
        // Sibling map for spans whose struct type has a user `impl Ord`.
        // `emit_sort_by_key_inline_thunk` consults it before the derive
        // cascade to dispatch to the user's compiled `Type.cmp` via
        // direct call.
        self.user_ord_typed_exprs = program.user_ord_typed_exprs.clone();

        // Surface TypeExpr per heap-owning temporary expression. Keyed by
        // span; `materialize_owned_temp` consults it to scope-drop unnamed
        // Vec/String (with element type), Map/Set handles, and RC boxes.
        self.owned_temp_drops = program.owned_temp_drops.clone();

        // Pointee TypeExpr per raw-pointer-typed expression. The unary-deref
        // arm keys this by operand span to load through `*const T` / `*mut T`.
        self.raw_pointer_pointee_types = program.raw_pointer_pointee_types.clone();

        // Fully-instantiated TypeExpr per generic Named instantiation
        // expression (`Option[String]`, `Result[i64, AllocError]`, …). Keyed
        // by span; `compile_enum_eq` consults it to recover the concrete type
        // argument a generic heap-payload enum's variant was instantiated
        // with, so `Some(String)` compares by content not pointer word.
        self.enum_inst_type_exprs = program.enum_inst_type_exprs.clone();

        // Phase 6 line 26 slice 8ab: snapshot the per-call effect-
        // variable substitution table. Slice 8y (entry 32) reads
        // this in `compile_generic_call` to gate per-mono state-
        // machine emission on whether the resolved per-call effects
        // include any network-yield verb.
        self.call_effect_subs = program.call_effect_subs.clone();

        // Side-table set by `lowering::lower_program`: each
        // `unwrap`/`expect`/`is_*` MethodCall on `Option[T]` or `Result[T, E]`
        // maps to the inner `TypeExpr`. Read by the codegen `unwrap` arm
        // to know how to reconstitute the payload back to a value of T.
        self.method_unwrap_inner_types = program.method_unwrap_inner_types.clone();
        self.channel_elem_types = program.channel_elem_types.clone();
        self.ref_return_inner_types = program.ref_return_inner_types.clone();
        // Bare names of user impl methods that return a borrow — gates the
        // method-ref caller path away from builtin ref-returning methods.
        for item in &program.items {
            if let Item::ImplBlock(imp) = item {
                for impl_item in &imp.items {
                    if let ImplItem::Method(m) = impl_item {
                        if matches!(
                            m.return_type.as_ref().map(|t| &t.kind),
                            Some(TypeKind::Ref(_) | TypeKind::MutRef(_))
                        ) {
                            self.user_ref_method_names.insert(m.name.clone());
                        }
                    }
                }
            }
        }

        // Side-table set by `lowering::lower_program`: each pattern-
        // binding's span maps to its surface type name. Read by
        // `bind_pattern_values` to reconstitute struct payloads from the
        // i64 word at match-arm bind sites — so `Err(e) => e.field` works
        // when the variant payload is a struct.
        self.pattern_binding_types = program.pattern_binding_types.clone();

        // Side-table set by `lowering::lower_program`: each pattern-
        // binding's span maps to its inner element TypeExpr for `Vec[T]` /
        // `Slice[T]` bindings only. Read by `bind_pattern_values` to
        // populate `vec_elem_types` / `slice_elem_types` under the
        // binding's variable name so direct method dispatch on the
        // binding (`xs.len()`, `xs[0]`, `xs.push(...)`) routes through
        // the right element-typed path. PB sibling slice (2026-05-09).
        self.pattern_binding_inner_types = program.pattern_binding_inner_types.clone();

        // Side-table set by `lowering::lower_program`: each pattern-
        // binding's span maps to its borrow form (`Ref` / `MutRef`) when
        // the enclosing match scrutinee is `ref T` / `mut ref T`. Owned
        // bindings are absent. Read by `bind_pattern_values` (Binding
        // arm) to wrap the value-typed leaf alloca in a ref-shim so call
        // sites that take a `ref T` / `mut ref T` parameter receive the
        // right ABI shape — slice 3a, 2026-05-14.
        self.pattern_binding_borrow_modes = program.pattern_binding_borrow_modes.clone();

        // Top-level `const NAME: T = value` collection. References from
        // function bodies (parsed as `ExprKind::Identifier(name)` for bare
        // uses) look up this map and re-compile the stored expression at
        // each use site; LLVM folds the resulting constant arithmetic.
        // Must precede function compilation so forward references work.
        for item in &program.items {
            if let Item::ConstDecl(c) = item {
                self.consts.insert(c.name.clone(), c.value.clone());
            }
        }

        // NOTE: refinement-alias / distinct-type base population was moved
        // up to before `build_struct_types` (right after
        // `seed_builtin_struct_types`) — a struct/enum field whose type names
        // a refinement (`email: BoundedText`) or distinct type must resolve
        // to the base's layout *while the aggregate is being lowered*, not
        // after. See `populate_type_alias_bases`.

        // Slice 9 of phase-8 module-let work — emit one LLVM global per
        // `Item::ModuleBinding`. Must precede function compilation so
        // forward references from any function body resolve through
        // `try_load_module_binding`. Distinct from `consts`: bindings
        // become real LLVM globals (mutable for `let mut`,
        // `thread_local` for `#[thread_local]`), not inlined values.
        self.declare_module_bindings(program);

        // Theme 6: assign stable u32 IDs to `effect resource R[: T]`
        // declarations + capture each provider trait's method-declaration
        // order. The IDs flow through to `karac_provider_push` /
        // `karac_provider_lookup` calls; method order pins the vtable
        // layout. Must precede impl-method declaration so vtable emission
        // (after impl declarations) can read these tables.
        let mut next_resource_id: u32 = 0;
        for item in &program.items {
            if let Item::EffectResource(decl) = item {
                self.provider_resource_ids
                    .insert(decl.name.clone(), next_resource_id);
                next_resource_id += 1;
                if let Some(trait_name) = &decl.provider_trait {
                    self.provider_resource_traits
                        .insert(decl.name.clone(), trait_name.clone());
                }
            }
        }
        // Mint stable IDs for ambient prelude resources (`Clock`, `Env`, …)
        // so `with_provider`-ambient overrides push/lookup on the same
        // runtime provider stack as user resources (cross-boundary
        // dispatch — `compile_with_provider_ambient` /
        // `try_compile_ambient_dispatch`). Most ambient resources have no
        // `Item::EffectResource` declaration in any path (the prelude only
        // registers them by name); `Network` / `ProcessTable` DO declare
        // one and already have an ID — `or_insert_with` skips those. IDs
        // continue past the user range so they never collide.
        for (resource, _methods) in crate::prelude::AMBIENT_RESOURCE_METHODS {
            self.provider_resource_ids
                .entry(resource.to_string())
                .or_insert_with(|| {
                    let id = next_resource_id;
                    next_resource_id += 1;
                    id
                });
        }
        for item in &program.items {
            if let Item::TraitDef(t) = item {
                let methods: Vec<String> = t
                    .items
                    .iter()
                    .filter_map(|ti| match ti {
                        TraitItem::Method(m) => Some(m.name.clone()),
                        TraitItem::AssocType(_) => None,
                    })
                    .collect();
                self.provider_trait_methods.insert(t.name.clone(), methods);
            }
        }

        // First pass: register generic functions for on-demand monomorphization;
        // declare concrete (non-generic) functions for forward-call support.
        for item in &program.items {
            if let Item::Function(f) = item {
                if f.generic_params.is_some() {
                    self.generic_fns.insert(f.name.clone(), f.clone());
                } else {
                    self.declare_function(f)?;
                }
            }
        }

        // Declare user impl-block methods as LLVM functions named
        // "Type.method". Self-taking methods get `self` prepended as a
        // normal owned parameter (`ref self`/`mut ref self` deferred; all
        // current use sites — Eq/Ord/constructors — take `self` or no
        // receiver). Generic methods are deferred along with generic-fn
        // monomorphization.
        //
        // Duplicate impls (e.g. `impl PartialEq for Point { fn eq(ref self,
        // ref Point) }` and `impl Eq for Point { fn eq(self, Point) }` —
        // both legal in Kāra because `Eq` is a non-marker compat shim per
        // `runtime/stdlib/eq.kara`) collide on the LLVM symbol
        // `@Type.method`. We pick the value-self variant when there's a
        // choice: the lowering pass at `lowering.rs:543` dispatches `==`
        // via the `Eq` trait and emits `Type.eq(a, b)` with value-typed
        // arguments, so the function signature must accept values. A
        // ref-self body wouldn't compile correctly today anyway (deferred
        // — see comment above and `var_type_names` not being populated for
        // ref-typed params in `compile_function`). Two-pass iteration:
        // value-self impls first, then ref-self impls of the same method
        // are skipped as duplicates.
        let mut declared_impl_methods: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        for value_self_pass in [true, false] {
            for item in &program.items {
                if let Item::ImplBlock(imp) = item {
                    if let Some(type_name) = impl_target_name(&imp.target_type) {
                        for impl_item in &imp.items {
                            if let ImplItem::Method(method) = impl_item {
                                if method.generic_params.is_some() {
                                    continue;
                                }
                                if method_self_is_value(method) != value_self_pass {
                                    continue;
                                }
                                let qualified = format!("{}.{}", type_name, method.name);
                                if !declared_impl_methods.insert(qualified) {
                                    continue;
                                }
                                let synth = make_impl_method_function(&type_name, method);
                                self.declare_function(&synth)?;
                            }
                        }
                    }
                }
            }
        }

        // Bring the baked `std.tracing` surface into codegen (struct
        // layouts + impl-method *declarations*). Must run after the user
        // impl-declaration loop above and before user bodies compile, so
        // a `tracer.export_event(...)` / `LogEvent.info(...)` call site in
        // a user body resolves its `Type.method` symbol. Bodies are
        // compiled by the sibling pass after the user impl-body loop.
        // Skip a real-source stdlib module whose type name the user redefines
        // (flat-map collision — see `user_redefines_stdlib_type`). Gated
        // identically here and at the body-compile pass below.
        if !user_redefines_stdlib_type(program, tracing_stdlib_program()) {
            self.declare_stdlib_program(tracing_stdlib_program())?;
        }
        // 889 slice 1: declare the other compiled stdlib modules' layouts +
        // non-builtin impl-method signatures so user-body call sites resolve
        // their `Type.method` symbols (e.g. `ordering_value.is_lt()`).
        for tp in compiled_stdlib_programs() {
            if !user_redefines_stdlib_type(program, tp) {
                self.declare_stdlib_program(tp)?;
            }
        }

        // Theme 6: emit static vtables for impls of provider traits.
        // Runs after impl methods are *declared* (their fn-ptrs become
        // vtable entries) but BEFORE function bodies are compiled — body
        // compilation may include `with_provider[R]` call sites that
        // need the vtable global to already exist in `provider_vtables`
        // for the lookup at sub-step 3 lowering time. Bodies don't need
        // to be compiled yet because the vtable only references fn-ptr
        // symbols which were established by `declare_function`.
        self.emit_provider_vtables(program);
        // Ambient analog: emit override vtables for `with_provider[Clock]`
        // etc. eagerly too, so a cross-boundary ambient call (compiled
        // before the `with_provider` site — e.g. the test fn vs the
        // synthesized `main`) sees the vtable when deciding to emit its
        // runtime-dispatch branch.
        self.emit_ambient_provider_vtables(program);

        // Phase-7 line 5 sub-item 1 — emit the hot-swap indirection
        // table global so call-site lowering in the body pass can GEP
        // into it. The populator ctor is emitted at finalize. No-op
        // when --enable-hot-swap is off.
        self.pre_emit_hot_swap_table();

        // Phase 6 line 26 slice 6 + 8h: emit the state-machine poll
        // function per state-struct entry. Runs HERE (after user
        // function declarations) rather than early in the type-emission
        // block because slice-8h body-splitting needs to reference
        // user functions in the per-arm void-call emissions —
        // `module.get_function("pure_helper")` requires that callee
        // to be declared first. The declaration-only piece could in
        // principle land earlier (declarations don't need user-fn
        // symbols), but keeping declaration + body in one pass at
        // this point is simpler than splitting the emit function
        // across two compile_program phases, and the caller-side
        // intercept (slice 8d / 8g) only reads `state_machine_poll_fns`
        // during user body compilation that runs immediately after
        // this — so the side-table is populated in time.
        self.emit_state_machine_poll_fns(program);

        // Phase 6 line 17 slice 9d — hand-roll bodies for stdlib
        // `<Type>.drop` symbols that the existing impl-method pass
        // can't reach (stdlib impls live outside `program.items`). For
        // `TcpListener` / `TcpStream` this emits a `call
        // @karac_runtime_tcp_close(self.fd); ret void` body, mirroring
        // the always-emitted pattern from `karac_park_on_fd`. Must run
        // BEFORE `emit_user_drop_wrappers` below so the wrapper synth's
        // `module.get_function("<Type>.drop")` lookup succeeds.
        self.emit_hardcoded_stdlib_drop_bodies(program);

        // Phase 7 user-`impl Drop` dispatch — synthesize the per-type
        // `karac_drop_<Type>` wrapper for each entry in
        // `program.drop_method_keys`. Must run AFTER impl-method
        // declarations (so the wrapper body's `call @<Type>.drop` finds
        // the symbol via `module.get_function`) AND BEFORE the
        // concrete-function body pass below — Prereq.3's
        // `track_user_drop_var` reads `user_drop_wrapper_fns` at
        // let-binding time during body compile, so the cache must be
        // populated by then. The wrapper body's `call @<Type>.drop`
        // references the impl-method symbol but does not need its
        // body to be compiled (Prereq.2 finding); the impl-method body
        // pass at the bottom of this function compiles user-side bodies
        // in their own pass.
        self.emit_user_drop_wrappers(program);

        // Second pass: compile concrete functions (generic ones are compiled lazily).
        // Slice c-repl.B.4: when `declare_only_fns` contains the fn's name,
        // skip body emission. The first-pass `declare_function` already
        // registered an LLVM `declare i64 @<name>(...)` for the signature;
        // leaving it body-less lets the JIT linker resolve the symbol
        // against an earlier-installed module in the same JITDylib. Used
        // by the REPL JIT path so cell N+1 doesn't re-emit cell N's items.
        for item in &program.items {
            if let Item::Function(f) = item {
                if f.generic_params.is_none() {
                    if self.declare_only_fns.contains(&f.name) {
                        continue;
                    }
                    self.compile_function(f)?;
                }
            }
        }

        // Compile user impl-block method bodies. Mirror the declaration
        // pass's value-self-first ordering so the body that gets compiled
        // is the same one whose signature was declared.
        let mut compiled_impl_methods: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        for value_self_pass in [true, false] {
            for item in &program.items {
                if let Item::ImplBlock(imp) = item {
                    if let Some(type_name) = impl_target_name(&imp.target_type) {
                        for impl_item in &imp.items {
                            if let ImplItem::Method(method) = impl_item {
                                if method.generic_params.is_some() {
                                    continue;
                                }
                                if method_self_is_value(method) != value_self_pass {
                                    continue;
                                }
                                let qualified = format!("{}.{}", type_name, method.name);
                                if !compiled_impl_methods.insert(qualified.clone()) {
                                    continue;
                                }
                                // Slice c-repl.B.4 (impl-method extension):
                                // a prior cell already installed this method
                                // body in the runner's JITDylib. The synth
                                // fn's LLVM symbol is exactly `qualified`
                                // (`Type.method` — see
                                // `make_impl_method_function`), so the same
                                // declare-only set used for free functions
                                // applies. Skip the body; the first-pass
                                // `declare_function` already emitted the
                                // body-less `declare`, and the JIT linker
                                // resolves call sites against the earlier
                                // install. Without this, re-running a cell
                                // under an active provider re-emits
                                // `FakeClock.now` and trips
                                // `add_ir_module: Duplicate definition of
                                // symbol`.
                                if self.declare_only_fns.contains(&qualified) {
                                    continue;
                                }
                                let synth = make_impl_method_function(&type_name, method);
                                self.compile_function(&synth)?;
                            }
                        }
                    }
                }
            }
        }

        // Compile the baked `std.tracing` impl-method bodies whose
        // signatures were declared above. Mirrors the user impl-body
        // pass; the bodies use only general lowerings.
        // Mirror the declare-pass gate exactly: a module skipped above was
        // never declared, so its bodies must be skipped too (and vice-versa).
        if !user_redefines_stdlib_type(program, tracing_stdlib_program()) {
            self.compile_stdlib_program(tracing_stdlib_program())?;
        }
        // 889 slice 1: compile the other stdlib modules' real impl bodies
        // (declared above). Each runs with its own span tables swapped in and
        // prunes its own zero-use functions, so an ordering-free binary stays
        // lean.
        for tp in compiled_stdlib_programs() {
            if !user_redefines_stdlib_type(program, tp) {
                self.compile_stdlib_program(tp)?;
            }
        }

        // Slice c-repl.B.4: when this codegen pass is producing a
        // REPL cell module (signaled by `main_symbol_override`),
        // suppress the Debugger-Contract globals
        // (`karac_jit_template_manifest`, `KARAC_SPAWN_SITES*`).
        // Every karac-emitted module declares the same names, and
        // the REPL JIT keeps prior cells' modules alive in the
        // JITDylib so cell N+1's globals would trip duplicate-
        // symbol install errors. The first cell's globals stay
        // visible to the runtime's introspection reads (the runner
        // captures their addresses on the first install via
        // `karac_runtime_init_jit_spawn_sites`); subsequent cells'
        // par-block introspection sees the first cell's table,
        // which is a known limitation until per-cell JITDylib
        // isolation lands.
        if self.main_symbol_override.is_none() {
            self.emit_jit_template_section();
        }
        self.emit_llvm_used();
        if self.main_symbol_override.is_none() {
            self.emit_spawn_sites_metadata();
        }
        self.finalize_hot_swap_table();

        // Phase-10 WASM build path: wasi-libc's `crt1-command.o` enters at
        // `_start → __main_void`; libc's own (weak, arg-gathering)
        // `__main_void` chains to `__main_argc_argv`, a symbol clang mints
        // when compiling C `main` — karac's entry is the literal `main`,
        // so that chain would end in an undefined-symbol link error.
        // Defining `__main_void` ourselves keeps libc's member from being
        // extracted at all: the shim just tail-calls `main()`.
        self.emit_wasm_entry_shim()?;

        // Phase-10 WASM entry-point discovery (sub-slice D): on a
        // component build, rename scalar exports to their kebab WIT name
        // and emit canonical-ABI trampolines for record-returning exports.
        self.emit_wasm_component_export_surface(program)?;

        // Level 2 crash diagnostics — Part 2: finalize DWARF debug info BEFORE
        // verify. The verifier validates debug metadata, and unresolved
        // temporaries / a missing finalize would make it reject the module.
        // No-op unless debug info is enabled.
        self.di_finalize();

        self.module
            .verify()
            .map_err(|e| format!("Module verification failed: {}", e))
    }

    /// WASM entry-point shim (`--target=wasm_wasi` / `wasm_browser` —
    /// both wasip1 command modules): `i32 @__main_void()` that calls
    /// `i32 @main()`. No-op on every other target, and when no `main`
    /// exists in the module (library-shaped programs / REPL cells
    /// — `main_symbol_override` renames the entry, and a wasm REPL JIT
    /// doesn't exist, so the literal-`main` lookup is the right key).
    fn emit_wasm_entry_shim(&mut self) -> Result<(), String> {
        if !crate::target::active_target_is_wasm() {
            return Ok(());
        }
        let Some(main_fn) = self.module.get_function("main") else {
            return Ok(());
        };
        let i32_type = self.context.i32_type();
        let shim = self
            .module
            .add_function("__main_void", i32_type.fn_type(&[], false), None);
        let entry = self.context.append_basic_block(shim, "entry");
        self.builder.position_at_end(entry);
        let ret = self
            .builder
            .build_call(main_fn, &[], "main_ret")
            .map_err(|e| format!("wasm entry shim: {e}"))?
            .try_as_basic_value()
            .unwrap_basic();
        self.builder
            .build_return(Some(&ret))
            .map_err(|e| format!("wasm entry shim: {e}"))?;
        Ok(())
    }

    /// Bring one baked stdlib `Program`'s surface into codegen — struct +
    /// enum layouts + non-`#[compiler_builtin]` impl-method *declarations*
    /// (phase-7 line 889). Used for `std.tracing` and the modules in
    /// [`compiled_stdlib_programs`] (`ordering`, …).
    ///
    /// Stdlib types/impls live in `STDLIB_PROGRAMS`, which codegen does NOT
    /// walk by default (`declarations.rs` § "items reach the typechecker via
    /// `STDLIB_PROGRAMS` but do NOT reach codegen"). Without this pass a
    /// compiled binary saw their struct/enum layouts as the i64 default and
    /// any real-bodied method dispatch (`tracer.export_event(...)`,
    /// `ordering_value.is_lt()`) fell through with "no handler for method".
    /// Unlike the TCP/TLS/WS stdlib (whose methods are `#[compiler_builtin]`
    /// with hand-rolled lowerings — skipped here), these are real Kāra bodies,
    /// so the maintainable path is to compile that real source: declare the
    /// layouts through the normal `declare_structs` / `declare_enums`
    /// side-table populators (no IR), then declare every concrete non-builtin
    /// impl method so dispatch's `module.get_function("Type.method")` lookup
    /// resolves. Bodies land in [`Self::compile_stdlib_program`].
    ///
    /// Mirrors the value-self-first two-pass dedup of the user impl
    /// declaration loop in `compile_program`, kept identical so the two
    /// stay in lockstep.
    fn declare_stdlib_program(&mut self, tp: &Program) -> Result<(), String> {
        // Layouts + field/variant side tables (struct_types / struct_field_* /
        // enum layouts), no IR — so literals, field access, `match` on a
        // stdlib enum, and aggregate fields all lower at the right shape.
        // `declare_enums` is the addition over the original tracing-only pass
        // (tracing has no enums; `Ordering` does). Same metadata→enums→types
        // ordering as `compile_program` so a stdlib struct field that names a
        // stdlib enum resolves at the enum's tagged-union shape, not `i64`.
        self.register_struct_metadata(tp);
        self.declare_enums(tp);
        self.build_struct_types(tp);
        let mut declared: std::collections::HashSet<String> = std::collections::HashSet::new();
        for value_self_pass in [true, false] {
            for item in &tp.items {
                if let Item::ImplBlock(imp) = item {
                    if let Some(type_name) = impl_target_name(&imp.target_type) {
                        for impl_item in &imp.items {
                            if let ImplItem::Method(method) = impl_item {
                                if method.generic_params.is_some() {
                                    continue;
                                }
                                // 889: `#[compiler_builtin]` methods have
                                // hand-rolled codegen lowerings (their Kāra
                                // bodies are stubs); never declare/compile
                                // them here. No-op for tracing (its builtins
                                // are free fns) and `Ordering` (no builtins).
                                if method_is_compiler_builtin(method) {
                                    continue;
                                }
                                if method_self_is_value(method) != value_self_pass {
                                    continue;
                                }
                                let qualified = format!("{}.{}", type_name, method.name);
                                if !declared.insert(qualified) {
                                    continue;
                                }
                                let synth = make_impl_method_function(&type_name, method);
                                self.declare_function(&synth)?;
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Compile one baked stdlib `Program`'s non-builtin impl-method bodies,
    /// declared by [`Self::declare_stdlib_program`] (phase-7 line 889). Runs
    /// after the user impl-body pass; the bodies must use only general
    /// lowerings (no module-specific codegen) — tracing's do (struct
    /// construction, `Vec.new`/`push`, f-strings, `for`, String `+`,
    /// `println`); `Ordering`'s are `match self` → bool. The program's
    /// span-keyed side tables are swapped in for the duration so the bodies'
    /// `<module>.kara` spans resolve. The `declare_only_fns` guard mirrors
    /// the user loop's REPL-cell de-dup.
    fn compile_stdlib_program(&mut self, tp: &Program) -> Result<(), String> {
        // Swap in the stdlib program's span-keyed side tables for the
        // duration of body emission. The bodies carry `tracing.kara`
        // spans, so they only hit these tables; the user program's tables
        // are restored before this returns. Name-keyed state
        // (`struct_types`, `vec_elem_types`, …) is shared and stays put —
        // tracing struct layouts were already merged by the declaration
        // pass. Swap ALL program-derived span tables (not just the few the
        // current bodies touch) so a future tracing-body edit that leans
        // on, say, `method_unwrap_inner_types` doesn't silently miscompile.
        //
        // `std::mem::swap` needs an lvalue on both sides; stage the
        // tracing-side clones into owned locals, `swap_all!` to install
        // them, emit the bodies, then `swap_all!` again to restore.
        let mut t_question_conversions = tp.question_conversions.clone();
        let mut t_callee_effectful = tp.callee_effectful.clone();
        let mut t_method_callee_types = tp.method_callee_types.clone();
        let mut t_string_typed_exprs = tp.string_typed_exprs.clone();
        let mut t_unsigned_vector_exprs = tp.unsigned_vector_exprs.clone();
        let mut t_expr_struct_type_names = tp.expr_struct_type_names.clone();
        let mut t_user_ord_typed_exprs = tp.user_ord_typed_exprs.clone();
        let mut t_owned_temp_drops = tp.owned_temp_drops.clone();
        let mut t_raw_pointer_pointee_types = tp.raw_pointer_pointee_types.clone();
        let mut t_enum_inst_type_exprs = tp.enum_inst_type_exprs.clone();
        let mut t_call_effect_subs = tp.call_effect_subs.clone();
        let mut t_method_unwrap_inner_types = tp.method_unwrap_inner_types.clone();
        let mut t_channel_elem_types = tp.channel_elem_types.clone();
        let mut t_ref_return_inner_types = tp.ref_return_inner_types.clone();
        let mut t_pattern_binding_types = tp.pattern_binding_types.clone();
        let mut t_pattern_binding_inner_types = tp.pattern_binding_inner_types.clone();
        let mut t_pattern_binding_borrow_modes = tp.pattern_binding_borrow_modes.clone();
        macro_rules! swap_all {
            () => {{
                std::mem::swap(&mut self.question_conversions, &mut t_question_conversions);
                std::mem::swap(&mut self.callee_effectful, &mut t_callee_effectful);
                std::mem::swap(&mut self.method_callee_types, &mut t_method_callee_types);
                std::mem::swap(&mut self.string_typed_exprs, &mut t_string_typed_exprs);
                std::mem::swap(
                    &mut self.unsigned_vector_exprs,
                    &mut t_unsigned_vector_exprs,
                );
                std::mem::swap(
                    &mut self.expr_struct_type_names,
                    &mut t_expr_struct_type_names,
                );
                std::mem::swap(&mut self.user_ord_typed_exprs, &mut t_user_ord_typed_exprs);
                std::mem::swap(&mut self.owned_temp_drops, &mut t_owned_temp_drops);
                std::mem::swap(
                    &mut self.raw_pointer_pointee_types,
                    &mut t_raw_pointer_pointee_types,
                );
                std::mem::swap(&mut self.enum_inst_type_exprs, &mut t_enum_inst_type_exprs);
                std::mem::swap(&mut self.call_effect_subs, &mut t_call_effect_subs);
                std::mem::swap(
                    &mut self.method_unwrap_inner_types,
                    &mut t_method_unwrap_inner_types,
                );
                std::mem::swap(&mut self.channel_elem_types, &mut t_channel_elem_types);
                std::mem::swap(
                    &mut self.ref_return_inner_types,
                    &mut t_ref_return_inner_types,
                );
                std::mem::swap(
                    &mut self.pattern_binding_types,
                    &mut t_pattern_binding_types,
                );
                std::mem::swap(
                    &mut self.pattern_binding_inner_types,
                    &mut t_pattern_binding_inner_types,
                );
                std::mem::swap(
                    &mut self.pattern_binding_borrow_modes,
                    &mut t_pattern_binding_borrow_modes,
                );
            }};
        }
        swap_all!();
        let result = self.compile_stdlib_program_method_bodies(tp);
        swap_all!(); // restore the user program's tables
        result
    }

    /// Inner body-emission loop for [`Self::compile_tracing_stdlib_methods`],
    /// run with the tracing program's span tables swapped in.
    ///
    /// Two phases:
    ///
    /// 1. **Compile every tracing method body.** No usage gate here —
    ///    tracing methods call each other (`Log.info` → `StdoutExporter.
    ///    export_event` → `LogEvent.info`), so a callee's only use may be a
    ///    caller whose body hasn't been emitted yet; gating during this
    ///    pass would delete the callee before its caller's call site
    ///    exists.
    /// 2. **Prune unused tracing functions to a fixpoint.** Once every body
    ///    is emitted, every real call site exists, so a tracing function
    ///    with zero uses is genuinely dead — delete it. Deleting one can
    ///    orphan another (an unused `Log.info` was the only caller of
    ///    `export_event`), so loop until a full scan deletes nothing. This
    ///    keeps tracing-free binaries lean (no dead `Vec`/f-string
    ///    machinery) and the IR-shape codegen tests valid.
    fn compile_stdlib_program_method_bodies(&mut self, tp: &Program) -> Result<(), String> {
        // Compiling the tracing bodies repositions `self.builder` into the
        // last tracing function, and the phase-2 prune may then *delete*
        // that function — leaving the builder on a freed block. Downstream
        // passes (e.g. `finalize_hot_swap_table`'s `get_insert_block`)
        // assume a live insert position, so snapshot it now and restore it
        // before returning. The saved block belongs to the user-side
        // function the impl-body pass left off in; the prune never touches
        // user functions, so it stays valid.
        let saved_block = self.builder.get_insert_block();
        let mut compiled: std::collections::HashSet<String> = std::collections::HashSet::new();
        for value_self_pass in [true, false] {
            for item in &tp.items {
                if let Item::ImplBlock(imp) = item {
                    if let Some(type_name) = impl_target_name(&imp.target_type) {
                        for impl_item in &imp.items {
                            if let ImplItem::Method(method) = impl_item {
                                if method.generic_params.is_some() {
                                    continue;
                                }
                                // 889: skip `#[compiler_builtin]` — bodies are
                                // stubs; codegen has hand-rolled lowerings.
                                if method_is_compiler_builtin(method) {
                                    continue;
                                }
                                if method_self_is_value(method) != value_self_pass {
                                    continue;
                                }
                                let qualified = format!("{}.{}", type_name, method.name);
                                if !compiled.insert(qualified.clone()) {
                                    continue;
                                }
                                if self.declare_only_fns.contains(&qualified) {
                                    continue;
                                }
                                let synth = make_impl_method_function(&type_name, method);
                                self.compile_function(&synth)?;
                            }
                        }
                    }
                }
            }
        }
        // Phase 2: fixpoint prune of zero-use tracing functions.
        loop {
            let mut deleted_any = false;
            for qualified in &compiled {
                if let Some(fv) = self.module.get_function(qualified) {
                    // `get_first_use` lives on the `BasicValue` trait, which
                    // `FunctionValue` doesn't impl — route through the
                    // function's global-value pointer, which does.
                    if inkwell::values::BasicValue::get_first_use(
                        &fv.as_global_value().as_pointer_value(),
                    )
                    .is_none()
                    {
                        // SAFETY: no uses (checked above), so deleting the
                        // function cannot dangle a call site.
                        unsafe { fv.delete() };
                        deleted_any = true;
                    }
                }
            }
            if !deleted_any {
                break;
            }
        }
        // Restore the pre-pass insert position (see the snapshot above).
        if let Some(bb) = saved_block {
            self.builder.position_at_end(bb);
        }
        Ok(())
    }

    /// Phase-7 line 5 sub-item 1 — emit the hot-swap table global with
    /// `zeroinitializer` so call sites lowered during function body
    /// compilation can GEP+load it. The populator ctor is emitted
    /// later in `finalize_hot_swap_table`, after the function bodies
    /// have closed (so the cursor isn't yanked out from under them).
    ///
    /// Called between the function-declaration pass and the function-
    /// body pass in `compile_program`. No-op when `hot_swap_enabled`
    /// is `false` or no pub-fn declarations were registered.
    pub(crate) fn pre_emit_hot_swap_table(&mut self) {
        if !self.hot_swap_enabled || self.hot_swap_fns.is_empty() {
            return;
        }
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let n = self.hot_swap_fns.len() as u32;
        let arr_ty = ptr_ty.array_type(n);
        let table = self.module.add_global(arr_ty, None, "karac_hotswap_table");
        table.set_initializer(&arr_ty.const_zero());
        table.set_linkage(inkwell::module::Linkage::External);
    }

    /// Finalize phase-7 line 5 sub-item 1. Emits the populator ctor
    /// (`@__karac_init_hot_swap_table`) and registers it in
    /// `@llvm.global_ctors` so each slot is initialized before `main`.
    ///
    /// No-op when `hot_swap_enabled` is `false` or no pub-fn
    /// declarations were registered. The v1 binary stores direct
    /// function addresses so dispatch behavior is unchanged; the
    /// indirection only exists to make post-v1 reload non-breaking.
    fn finalize_hot_swap_table(&mut self) {
        if !self.hot_swap_enabled || self.hot_swap_fns.is_empty() {
            return;
        }
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i32_ty = self.context.i32_type();
        let i64_ty = self.context.i64_type();
        let n = self.hot_swap_fns.len() as u32;
        let arr_ty = ptr_ty.array_type(n);
        let table = self
            .module
            .get_global("karac_hotswap_table")
            .expect("pre_emit_hot_swap_table must run before finalize");

        // Populator ctor.
        let void_ty = self.context.void_type();
        let ctor_ty = void_ty.fn_type(&[], false);
        let ctor = self.module.add_function(
            "__karac_init_hot_swap_table",
            ctor_ty,
            Some(inkwell::module::Linkage::Internal),
        );
        let entry = self.context.append_basic_block(ctor, "entry");
        let prev = self.builder.get_insert_block();
        self.builder.position_at_end(entry);
        for (slot, fn_val) in self.hot_swap_fns.clone() {
            let fn_ptr = fn_val.as_global_value().as_pointer_value();
            let gep = unsafe {
                self.builder.build_in_bounds_gep(
                    arr_ty,
                    table.as_pointer_value(),
                    &[
                        i64_ty.const_int(0, false),
                        i64_ty.const_int(slot as u64, false),
                    ],
                    &format!("hotswap_slot_{slot}"),
                )
            }
            .unwrap();
            self.builder.build_store(gep, fn_ptr).unwrap();
        }
        self.builder.build_return(None).unwrap();
        if let Some(bb) = prev {
            self.builder.position_at_end(bb);
        }

        // Register the ctor in `@llvm.global_ctors`. Standard layout:
        // appending-linkage `[N x { i32 priority, ptr fn, ptr data }]`.
        // Priority 65535 is the LLVM default ("run last"); we don't need
        // an earlier slot — the table is consulted from inside main /
        // user code, never from another ctor.
        let entry_ty = self
            .context
            .struct_type(&[i32_ty.into(), ptr_ty.into(), ptr_ty.into()], false);
        let arr_one_ty = entry_ty.array_type(1);
        let global_ctors = self
            .module
            .add_global(arr_one_ty, None, "llvm.global_ctors");
        let priority = i32_ty.const_int(65535, false);
        let ctor_ptr = ctor.as_global_value().as_pointer_value();
        let null_data = ptr_ty.const_null();
        let entry_val =
            entry_ty.const_named_struct(&[priority.into(), ctor_ptr.into(), null_data.into()]);
        let arr_val = entry_ty.const_array(&[entry_val]);
        global_ctors.set_initializer(&arr_val);
        global_ctors.set_linkage(inkwell::module::Linkage::Appending);
    }

    /// Phase-7 line 14 — emit the `.kara_jit_template` section
    /// containing a 4-byte version manifest. The section is reserved
    /// at v1 freeze so post-v1 runtime monomorphization JIT (see
    /// `deferred.md § Runtime Monomorphization JIT`) can fill in real
    /// bitcode payloads without breaking the AOT binary format —
    /// existing v1 binaries already carry a `version=0 / empty` slot
    /// that v2+ readers recognize and ignore.
    ///
    /// Layout — 4 bytes total:
    /// `[version: u8, reserved: u8, reserved: u8, reserved: u8]`.
    /// v1 ships `[0x00, 0x00, 0x00, 0x00]` (version 0 = empty). v2+
    /// picks any payload shape under version 1+ without breaking v1
    /// readers (which only know version 0 = empty; any unknown
    /// version is ignored).
    ///
    /// The global is `External`-linkage with a stable symbol name
    /// (`karac_jit_template_manifest`) so v2+ tooling can find it
    /// either by section or by symbol. It registers in
    /// `used_data_globals` so `emit_llvm_used` pins it into
    /// `@llvm.used` and the linker can't strip it under
    /// `--gc-sections` / `-dead_strip`.
    ///
    /// **Section name handling.** ELF accepts `.kara_jit_template`
    /// verbatim; Mach-O caps section names at 16 chars and uses a
    /// `__SEGMENT,__SECTION` form, so the codegen picks
    /// `__TEXT,__jittmpl` (segment 5 chars, section 8 chars) on Apple
    /// targets. The platform branch reflects the karac binary's host
    /// triple (matches `create_target_machine` which uses the default
    /// triple); cross-compile to a non-host object format would need
    /// to widen this surface.
    ///
    /// **Why `__TEXT` and not a fresh `__KARA` segment.** Mach-O
    /// segments are page-aligned in the file, so a fresh segment for a
    /// 4-byte payload still costs one full page — 16 KiB on macOS,
    /// paid by every kara binary forever. Parking the manifest inside
    /// `__TEXT` (which every binary already carries with room to
    /// spare) reclaims those 16 KiB at zero functional cost: the
    /// section identifier stays `__jittmpl`, JIT-discovery readers
    /// still find the symbol by `nm` or by walking `__TEXT`'s
    /// sections. When post-v1 JIT actually ships, *that* karac version
    /// adds whatever segment shape the JIT-payload needs at the
    /// emission site — additive segment additions don't break v1
    /// readers, and anyone running post-v1 JIT necessarily has
    /// post-v1 karac and is rebuilding their `.kara` sources anyway
    /// (no mainstream toolchain promises in-place binary augmentation
    /// across compiler upgrades). The original v1 ship (`82d53e5`,
    /// 2026-05-18) parked the manifest in `__KARA` to pre-reserve
    /// segment shape — measurement against kata-88's bench (2026-05-25)
    /// caught the 16-KiB-per-binary tax and the segment promise was
    /// re-scoped to "ships when JIT does."
    ///
    /// **Target gating.** v1 emits unconditionally — the 4-byte
    /// marker has no measurable cost regardless of profile. The
    /// entry's "v1 simply does not emit for embedded/wasm" guidance
    /// was a future-proofing precaution against bitcode payload; for
    /// the empty-manifest case the marker is harmless on any target.
    /// The hard-error for actual bitcode payload lives at the v2
    /// emission site (where it can refuse to populate the section
    /// based on profile).
    fn emit_jit_template_section(&mut self) {
        let i8_ty = self.context.i8_type();
        let manifest_ty = i8_ty.array_type(4);
        let manifest = self
            .module
            .add_global(manifest_ty, None, "karac_jit_template_manifest");
        // Version 0 = empty; the trailing three bytes are reserved for
        // future use and stay 0 in v1.
        let bytes = [
            i8_ty.const_int(0, false),
            i8_ty.const_int(0, false),
            i8_ty.const_int(0, false),
            i8_ty.const_int(0, false),
        ];
        manifest.set_initializer(&i8_ty.const_array(&bytes));
        manifest.set_linkage(inkwell::module::Linkage::External);
        let section_name = if cfg!(target_vendor = "apple") {
            "__TEXT,__jittmpl"
        } else {
            ".kara_jit_template"
        };
        manifest.set_section(Some(section_name));
        self.used_data_globals.push(manifest);
    }

    /// Materialize the special `@llvm.used` global from `used_symbols`.
    /// Standard LLVM convention: an `appending`-linkage array of pointers
    /// in section `llvm.metadata`. The linker treats every entry as
    /// implicitly referenced and preserves it across dead-code elimination.
    /// No-op when no `#[used]` symbols were declared.
    fn emit_llvm_used(&mut self) {
        if self.used_symbols.is_empty() && self.used_data_globals.is_empty() {
            return;
        }
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let total = (self.used_symbols.len() + self.used_data_globals.len()) as u32;
        let arr_ty = ptr_ty.array_type(total);
        let global = self.module.add_global(arr_ty, None, "llvm.used");
        let mut entries: Vec<inkwell::values::PointerValue<'ctx>> = self
            .used_symbols
            .iter()
            .map(|f| f.as_global_value().as_pointer_value())
            .collect();
        entries.extend(self.used_data_globals.iter().map(|g| g.as_pointer_value()));
        let init = ptr_ty.const_array(&entries);
        global.set_initializer(&init);
        global.set_linkage(inkwell::module::Linkage::Appending);
        global.set_section(Some("llvm.metadata"));
    }

    /// Emit the three module-scope globals that make up the SpawnSiteId
    /// metadata table — the foundation of the four-piece Debugger
    /// Contract specified in `design.md § AI-First Compiler Interface
    /// > Debugger Contract`. Slice 3 of the contract; consumed by
    /// slices 4 + 5 and the `std.panic` crash report's
    /// `parallel_context` field.
    ///
    /// Globals (external linkage; names are exact — runtime APIs and
    /// any external debugger tooling key on them verbatim):
    ///
    /// - `KARAC_SPAWN_SITES_ENABLED: i1` — `true` iff
    ///   `runtime_debug_metadata_enabled`. `std.runtime`'s
    ///   `has_debug_metadata()` (slice 5) reads this.
    /// - `KARAC_SPAWN_SITES_LEN: i32` — 0 when the gate is off,
    ///   `spawn_sites.len()` otherwise.
    /// - `KARAC_SPAWN_SITES: [N x SpawnSiteEntry]` where
    ///   `SpawnSiteEntry = { i32 id, ptr file_cstr, i32 line,
    ///   i32 col, i32 worker_count, i32 reserved }`.
    ///   The trailing `reserved` field is intentionally future-additive
    ///   (per `design.md § Debugger Contract > Stability`); it lets
    ///   future fields land within a major version without breaking
    ///   ABI. When the gate is off, `N == 0`.
    ///
    /// Per-filename `i8`-array globals are deduped by file path so a
    /// program with many `par {}` blocks in the same file emits only
    /// one filename string.
    fn emit_spawn_sites_metadata(&mut self) {
        let i32_ty = self.context.i32_type();
        let i8_ty = self.context.i8_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let bool_ty = self.context.bool_type();

        // Entry struct layout: { i32 id, ptr file_cstr, i32 line,
        //                        i32 col, i32 worker_count, i32 reserved }
        let entry_ty = self.context.struct_type(
            &[
                i32_ty.into(),
                ptr_ty.into(),
                i32_ty.into(),
                i32_ty.into(),
                i32_ty.into(),
                i32_ty.into(),
            ],
            false,
        );

        // Decide what the table contains. When the gate is off, emit
        // an empty array and a length of zero; the runtime API in
        // slice 5 reads through the same symbols regardless and
        // degrades cleanly (`list_par_blocks() == []`,
        // `has_debug_metadata() == false`).
        let emit_entries = self.runtime_debug_metadata_enabled;
        let len_value = if emit_entries {
            self.spawn_sites.len() as u32
        } else {
            0
        };

        // Build the per-filename `i8`-array globals (one per distinct
        // file path) and remember each as a pointer-to-first-byte.
        let mut file_globals: HashMap<String, PointerValue<'ctx>> = HashMap::new();
        if emit_entries {
            for record in &self.spawn_sites {
                if file_globals.contains_key(&record.file) {
                    continue;
                }
                // Null-terminated; use `const_string(.., true)` to
                // append the trailing NUL automatically.
                let cstr = self.context.const_string(record.file.as_bytes(), true);
                let arr_ty = i8_ty.array_type(cstr.get_type().len());
                let g = self.module.add_global(
                    arr_ty,
                    None,
                    &format!("karac.spawn_site_file.{}", file_globals.len()),
                );
                g.set_initializer(&cstr);
                g.set_linkage(Linkage::Private);
                g.set_constant(true);
                file_globals.insert(record.file.clone(), g.as_pointer_value());
            }
        }

        // Construct the array initializer.
        let entries_init: Vec<_> = if emit_entries {
            self.spawn_sites
                .iter()
                .map(|r| {
                    let file_ptr = file_globals
                        .get(&r.file)
                        .copied()
                        .unwrap_or_else(|| ptr_ty.const_null());
                    let id_v = i32_ty.const_int(r.id as u64, false);
                    let line_v = i32_ty.const_int(r.line as u64, false);
                    let col_v = i32_ty.const_int(r.col as u64, false);
                    let wc_v =
                        i32_ty.const_int(r.worker_count.map(|w| w as u64).unwrap_or(0), false);
                    let reserved_v = i32_ty.const_zero();
                    entry_ty.const_named_struct(&[
                        id_v.into(),
                        file_ptr.into(),
                        line_v.into(),
                        col_v.into(),
                        wc_v.into(),
                        reserved_v.into(),
                    ])
                })
                .collect()
        } else {
            Vec::new()
        };

        let arr_ty = entry_ty.array_type(entries_init.len() as u32);
        let arr_global = self.module.add_global(arr_ty, None, "KARAC_SPAWN_SITES");
        arr_global.set_initializer(&entry_ty.const_array(&entries_init));
        arr_global.set_linkage(Linkage::External);
        arr_global.set_constant(true);

        let len_global = self
            .module
            .add_global(i32_ty, None, "KARAC_SPAWN_SITES_LEN");
        len_global.set_initializer(&i32_ty.const_int(len_value as u64, false));
        len_global.set_linkage(Linkage::External);
        len_global.set_constant(true);

        let enabled_global = self
            .module
            .add_global(bool_ty, None, "KARAC_SPAWN_SITES_ENABLED");
        enabled_global.set_initializer(&bool_ty.const_int(
            if self.runtime_debug_metadata_enabled {
                1
            } else {
                0
            },
            false,
        ));
        enabled_global.set_linkage(Linkage::External);
        enabled_global.set_constant(true);
    }

    fn load_variable(&self, name: &str) -> Result<BasicValueEnum<'ctx>, String> {
        if let Some(slot) = self.variables.get(name) {
            // RC-fallback: the alloca holds a heap ptr → {i64 rc, T value}; load T from field 1.
            if let Some(&heap_type) = self.rc_fallback_heap_types.get(name) {
                let ptr_ty = self.context.ptr_type(AddressSpace::default());
                let heap_ptr = self
                    .builder
                    .build_load(ptr_ty, slot.ptr, &format!("{}.rcptr", name))
                    .unwrap()
                    .into_pointer_value();
                let val_ty = heap_type.get_field_type_at_index(1).unwrap();
                let val_field = self
                    .builder
                    .build_struct_gep(heap_type, heap_ptr, 1, &format!("{}.rcval", name))
                    .unwrap();
                return Ok(self.builder.build_load(val_ty, val_field, name).unwrap());
            }
            let val = self.builder.build_load(slot.ty, slot.ptr, name).unwrap();
            // For ref params, the loaded value is a pointer — dereference to get the actual value.
            if let Some(&inner_ty) = self.ref_params.get(name) {
                let ptr = val.into_pointer_value();
                Ok(self
                    .builder
                    .build_load(inner_ty, ptr, &format!("{}.deref", name))
                    .unwrap())
            } else {
                Ok(val)
            }
        } else {
            Err(format!("Undefined variable '{}'", name))
        }
    }

    /// For a variable that may be a ref param, return a pointer to the underlying data.
    /// Owned: returns the alloca directly (alloca holds the struct).
    /// Ref: loads the pointer from alloca then returns it (pointer to the caller's struct).
    /// Module binding: returns the global's pointer (the global IS the storage,
    /// equivalent to an owned alloca for dispatch-shape purposes — used by the
    /// Vec/Map/Set method-call paths to dispatch on a global like `TODOS`).
    fn get_data_ptr(&self, name: &str) -> Option<PointerValue<'ctx>> {
        if let Some(slot) = self.variables.get(name) {
            // RC-fallback: the alloca holds a heap ptr → `{ i64 rc, T value }`;
            // the data lives at field 1 (offset 8 past the refcount header).
            // Mirror `load_variable`'s RC-aware read, but return the *pointer*
            // to the value (the callee at a `ref`/`mut ref` arg site expects a
            // place), not the loaded value. Without this, a `ref`-arg of a
            // genuinely RC-promoted binding receives the box's header address
            // (the refcount slot) instead of the value pointer — the callee
            // then reads/writes the refcount or zeroes the box ptr through a
            // field write, and a later use derefs `null + 8` (B-2026-06-13-1).
            // Checked before `ref_params` to match `load_variable`'s ordering;
            // an RC-promoted binding is owned, never itself a ref param.
            if let Some(&heap_type) = self.rc_fallback_heap_types.get(name) {
                let ptr_ty = self.context.ptr_type(AddressSpace::default());
                let heap_ptr = self
                    .builder
                    .build_load(ptr_ty, slot.ptr, &format!("{}.rcptr", name))
                    .unwrap()
                    .into_pointer_value();
                let val_field = self
                    .builder
                    .build_struct_gep(heap_type, heap_ptr, 1, &format!("{}.rcdata", name))
                    .unwrap();
                return Some(val_field);
            }
            if self.ref_params.contains_key(name) {
                // Ref param: alloca holds a ptr → load it.
                let ptr_ty = self.context.ptr_type(AddressSpace::default());
                return Some(
                    self.builder
                        .build_load(ptr_ty, slot.ptr, &format!("{}.ref.ptr", name))
                        .unwrap()
                        .into_pointer_value(),
                );
            }
            // Owned: alloca IS the struct pointer.
            return Some(slot.ptr);
        }
        // Module-binding fall-back. The global's pointer is the data
        // pointer — the slice-10 codegen surface registers
        // `vec_elem_types` / `map_key_types` / etc. for module bindings
        // via `reseed_module_binding_side_tables`, so the dispatch
        // tables key correctly on the binding's name; this helper
        // supplies the matching data-pointer.
        self.module_bindings
            .get(name)
            .map(|info| info.global.as_pointer_value())
    }

    fn create_entry_alloca(
        &self,
        fn_val: FunctionValue<'ctx>,
        name: &str,
        ty: BasicTypeEnum<'ctx>,
    ) -> PointerValue<'ctx> {
        let entry = fn_val.get_first_basic_block().unwrap();
        let b = self.context.create_builder();
        match entry.get_first_instruction() {
            Some(inst) => b.position_before(&inst),
            None => b.position_at_end(entry),
        }
        b.build_alloca(ty, name).unwrap()
    }

    /// Zero-initialize a `{ptr, len, cap}` String/Vec alloca to `{null, 0, 0}`
    /// **at the entry block**, right after its alloca instruction — not at the
    /// current builder position.
    ///
    /// Used for f-string accumulators (and any String/Vec temporary whose
    /// alloca is hoisted to entry but whose value-initializing stores are
    /// emitted at the expression site). If that expression sits inside a
    /// conditionally-executed block (a `for` body, an `if` arm) that never
    /// runs, the alloca is left holding uninitialized stack — and the
    /// unconditional scope-exit cleanup then reads a garbage `cap`, frees a
    /// garbage pointer, and corrupts the heap (the f-string-in-a-loop
    /// double-free, surfaced by `std.tracing`'s exporter bodies). Emitting the
    /// `{null, 0, 0}` store at entry guarantees the cap is `0` on the
    /// never-executed path, so the `cap > 0` free guard skips it. The
    /// expression site keeps its own re-init (a loop body re-evaluates the
    /// f-string each iteration and must start from empty).
    fn zero_init_str_acc_at_entry(&self, acc: PointerValue<'ctx>) {
        let vec_ty = self.vec_struct_type();
        let i64_t = self.context.i64_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let acc_inst = match acc.as_instruction() {
            Some(inst) => inst,
            None => return,
        };
        let b = self.context.create_builder();
        // The alloca was inserted as the entry block's first instruction
        // (`create_entry_alloca` positions before the prior first instruction),
        // so its next instruction is a safe, dominating insertion point that
        // precedes the block's terminator.
        match acc_inst.get_next_instruction() {
            Some(next) => b.position_before(&next),
            None => b.position_at_end(acc_inst.get_parent().unwrap()),
        }
        let data_pp = b
            .build_struct_gep(vec_ty, acc, 0, "fstr.init.data")
            .unwrap();
        let len_p = b.build_struct_gep(vec_ty, acc, 1, "fstr.init.len").unwrap();
        let cap_p = b.build_struct_gep(vec_ty, acc, 2, "fstr.init.cap").unwrap();
        b.build_store(data_pp, ptr_ty.const_null()).unwrap();
        b.build_store(len_p, i64_t.const_int(0, false)).unwrap();
        b.build_store(cap_p, i64_t.const_int(0, false)).unwrap();
    }

    fn param_name(&self, param: &Param) -> String {
        match &param.pattern.kind {
            PatternKind::Binding(name) => name.clone(),
            _ => "_".to_string(),
        }
    }
}
