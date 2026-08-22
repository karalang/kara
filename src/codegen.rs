//! LLVM code generation for Kāra programs.
//!
//! Compiles the AST to LLVM IR, then to native object files.
//! Uses the `inkwell` crate for LLVM bindings.

use rustc_hash::{FxHashMap, FxHashSet};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::rc::Rc;

use accel::Accel;
use bce_state::BceState;
use borrow_vars::BorrowVars;
use closure_state::ClosureState;
use conc_state::ConcState;
use contract_state::ContractState;
use display::Display;
use drop_rc::DropRc;
use fn_ctx::FnCtx;
use fn_sig::FnSig;
use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::Linkage;
use inkwell::module::Module;
use inkwell::targets::{FileType, TargetData};
use inkwell::types::{BasicMetadataTypeEnum, BasicTypeEnum};
use inkwell::values::{BasicValue, BasicValueEnum, FunctionValue, PointerValue};
use inkwell::AddressSpace;
use mapset::MapSet;
use mod_bindings::ModBindings;
use mono_state::MonoState;
use pattern_state::PatternState;
use payload_vars::PayloadVars;
use provider_state::ProviderState;
use rc_elision::RcElision;
use runtime_fns::RuntimeFns;
use span_tables::SpanTables;
use target_abi::TargetAbi;
use tracing::Tracing;
use type_decls::TypeDecls;
use var_types::VarTypes;

use crate::ast::*;
use crate::concurrency::{ConcurrencyAnalysis, FunctionConcurrency};
use crate::ownership::OwnershipCheckResult;
use crate::resolver::SpanKey;
use crate::token::Span;

mod accel;
mod accum_overflow;
mod arrow;
mod ascii_const_chars;
mod assoc_call;
mod backpressure;
mod bce_interproc;
mod bce_length_pin;
mod bce_state;
mod borrow_elision;
mod borrow_vars;
mod bounded_channel;
mod cabi;
mod call_dispatch;
mod calls;
mod channel;
mod chars_bailout;
mod clone_drop;
mod closure_state;
mod closures;
mod collections;
mod column;
mod conc_state;
mod consume_class;
mod contract_state;
mod control_flow;
mod control_flow_bce;
mod control_flow_for;
mod control_flow_match;
mod control_flow_slice;
mod coro;
mod dataframe;
mod debug_info;
mod declarations;
mod disjoint_par;
mod display;
pub(crate) mod driver;
pub mod drop_obs;
mod drop_rc;
mod entry_chains;
mod expr_ops;
mod exprs;
mod file;
mod fn_ctx;
mod fn_sig;
mod functions;
mod helpers;
mod http;
mod json;
mod kernel;
mod lazyframe;
#[cfg(feature = "llvm")]
mod lljit;
mod popcount_idiom;
#[cfg(feature = "llvm")]
pub use lljit::{LLJITEngine, ResourceTracker};
mod arena;
mod contracts;
mod interner;
mod maps;
mod mapset;
mod method_call;
mod method_call_ffi;
mod method_call_iter;
mod method_call_sync;
mod method_call_vector;
mod mod_bindings;
mod module_bindings;
mod mono;
mod mono_state;
mod once;
mod par_blocks;
mod param_own;
mod pattern_binding;
mod pattern_state;
mod payload_vars;
mod pool;
mod process;
mod provider;
mod provider_state;
mod rc_elision;
mod reduce;
mod refinement;
mod runtime;
mod runtime_fns;
mod shadow;
mod slice_alias;
mod span_tables;
mod sso;
mod state;
mod stats;
mod stmts;
mod synth;
mod synth_display;
mod synth_drop;
mod target_abi;
mod task_group;
mod tcp;
mod tensor;
mod test_assert;
mod tls;
mod tracing;
mod type_decls;
mod types_lowering;
mod var_types;
mod vec_method;

use driver::{
    apply_optimization_passes, create_target_machine, read_auto_par_env,
    read_runtime_debug_metadata_env, read_strip_contracts_env, read_strip_error_trace_env,
};
pub use driver::{
    link_executable, link_executable_exports, link_executable_with_sanitizer, link_native_library,
    link_wasm_executable_threaded, pin_runtime_debug_metadata, print_target_cpu_listing,
    validate_target_cpu, validate_target_features, NativeLibKind, RuntimeDebugMetadataPin,
};
use helpers::{
    impl_target_name, make_generic_impl_method_function, make_impl_method_function,
    method_is_compiler_builtin, method_self_is_value,
};
use state::{EnumLayout, LayoutId, SpawnSiteRecord, VarSlot};

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
    cg.fn_sig.declare_only_fns = declare_only_fns.clone();
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
#[cfg(feature = "llvm")]
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
    // `typecheck_stdlib_module` (not `typecheck`) disables the #34 collision-skip
    // — this module IS a stdlib module, so its own types must not skip the
    // injected prelude copy of themselves.
    let tc = crate::typecheck_stdlib_module(&parsed.program, &resolve);
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

static PROTOBUF_LOWERED_PROGRAM: std::sync::LazyLock<Program> = std::sync::LazyLock::new(|| {
    lower_stdlib_source("protobuf", include_str!("../runtime/stdlib/protobuf.kara"))
});

/// The lowered `std.protobuf` program — its pure-Kāra `ProtoBuf.*` encoder
/// namespace fns and `ProtoReader` methods, which a `#[derive(Message)]`-
/// generated `encode`/`decode` body calls into (B-2026-07-08-15 Layer 2).
/// Designed interpreter-only originally; compiling its bodies here is what lets
/// a derived Message round-trip under codegen / the JIT-default `karac run`.
/// The `comptime fn derive_message` / `proto_*` items are skipped by the
/// compile pass (comptime-only, per Layer 3); only the runtime `ProtoBuf` /
/// `ProtoReader` impl bodies are emitted.
fn protobuf_stdlib_program() -> &'static Program {
    &PROTOBUF_LOWERED_PROGRAM
}

static MEM_LOWERED_PROGRAM: std::sync::LazyLock<Program> = std::sync::LazyLock::new(|| {
    lower_stdlib_source("mem", include_str!("../runtime/stdlib/mem.kara"))
});

/// The lowered `std.mem` program. `swap`/`replace` are `#[compiler_builtin]`
/// (hand-rolled call-site intercepts in `call_dispatch.rs`, never compiled as
/// bodies), so the ONLY thing this contributes to codegen is the real generic
/// free fn `take[T: Default]` — seeded into `generic_fns` by the generic-fn
/// seeding loop so a `take(&mut x)` call site monomorphizes its
/// `replace(dest, T.default())` body per concrete `T`. Always present (like
/// `ordering`); `take`'s mono is emitted only on demand, so an unused `std.mem`
/// costs nothing in the output.
fn mem_stdlib_program() -> &'static Program {
    &MEM_LOWERED_PROGRAM
}

static REGEX_LOWERED_PROGRAM: std::sync::LazyLock<Program> = std::sync::LazyLock::new(|| {
    lower_stdlib_source("regex", include_str!("../runtime/stdlib/regex.kara"))
});

static POOL_LOWERED_PROGRAM: std::sync::LazyLock<Program> = std::sync::LazyLock::new(|| {
    lower_stdlib_source("pool", include_str!("../runtime/stdlib/pool.kara"))
});

/// The lowered `std` `Pool` program. Every `Pool` / `PooledConnection` method
/// is `#[compiler_builtin]` — `Pool.new` is an `assoc_call.rs` intercept,
/// `acquire`/`release` and the two Drops are hand-lowered in
/// `src/codegen/pool.rs` to `karac_runtime_pool_*` externs — so, like `regex`,
/// NO method body is compiled from here. Its sole contribution is the `Pool` /
/// `PooledConnection[T]` STRUCT LAYOUTS and the `PoolError` ENUM LAYOUT: without
/// them `acquire`'s `Result[PooledConnection[T], PoolError]` return can't build
/// the generic `PooledConnection { pool_handle_id, conn_id, val: T }` aggregate.
/// Always present (like `regex`) — the layouts cost nothing when unused and a
/// `Pool`-redefining user program skips the module via `user_redefines_stdlib_type`.
fn pool_stdlib_program() -> &'static Program {
    &POOL_LOWERED_PROGRAM
}

static PROCESS_LOWERED_PROGRAM: std::sync::LazyLock<Program> = std::sync::LazyLock::new(|| {
    lower_stdlib_source("process", include_str!("../runtime/stdlib/process.kara"))
});

/// The lowered `std.process` program. Contributes the `Command` /
/// `Child` / `ChildStdout` / `ChildStderr` / `ChildStdin` / `EnvVar` /
/// `ExitStatus` struct layouts, the `Stdio` enum layout, and the real
/// Kāra bodies of the pure builder methods (`Command.new` / `.arg` /
/// `.env` / `.stdin` / `.stdout` / `.stderr`). The OS-touching methods
/// (`spawn`, the `Child`/stream methods) are `#[compiler_builtin]` —
/// hand-lowered in `src/codegen/process.rs` to `karac_runtime_process_*`
/// externs — so they are skipped by the body-compile pass. Always
/// present (like `ordering`): the builder bodies don't call each other,
/// so the zero-use fixpoint prune drops them from process-free binaries.
fn process_stdlib_program() -> &'static Program {
    &PROCESS_LOWERED_PROGRAM
}

/// The lowered `std` regex program. Every `Regex` method is
/// `#[compiler_builtin]` — `compile` / `is_match` are hand-rolled codegen
/// intercepts (`assoc_call.rs` / `method_call.rs`), and `find` / `find_all` /
/// `replace_all` stay interp-only (they fail loud under codegen, unchanged) —
/// so, like `mem`, NO method body is compiled from here. Its sole contribution
/// is the `Regex` / `RegexError` / `Match` STRUCT LAYOUTS: without them a
/// `Regex` value collapses to a single i64 and the
/// `Regex.compile(...).unwrap().is_match(...)` round-trip loses the pattern
/// String (B-2026-07-14-19). Always present (like `mem`) — the layouts cost
/// nothing when unused and a `Regex`-redefining user program skips the module
/// via `user_redefines_stdlib_type`.
fn regex_stdlib_program() -> &'static Program {
    &REGEX_LOWERED_PROGRAM
}

/// True when `user` references the `std.protobuf` runtime surface — i.e. it
/// carries a `#[derive(Message)]` on some struct/enum. That derive is the sole
/// entry point to protobuf: its expansion (already run by codegen time, per
/// B-2026-07-08-15 Layer 1) splices `encode`/`decode` bodies that call into
/// `ProtoBuf.*` / `ProtoReader`, and there is no user-facing manual API.
///
/// Why protobuf needs a usage gate when `ordering` does not: the zero-use
/// fixpoint prune in [`Codegen::compile_stdlib_program_method_bodies`] cannot
/// collect a *mutually-recursive* dead group — encode↔nested-encode and the
/// reader's recursive descent keep each other's use counts nonzero, so none
/// of them ever reaches zero uses and the whole encoder tree (with its checked-
/// arithmetic `with.overflow` intrinsics) survives into a protobuf-free binary.
/// `ordering`'s `is_lt`/`is_le`/… bodies don't call each other, so they prune
/// clean and need no gate. Gating protobuf on actual use keeps protobuf-free
/// IR lean and the IR-shape codegen tests valid.
fn program_uses_protobuf(user: &Program) -> bool {
    user.items.iter().any(|item| {
        let attrs = match item {
            Item::StructDef(s) => &s.attributes,
            Item::EnumDef(e) => &e.attributes,
            _ => return false,
        };
        crate::comptime::ordered_derived_traits(attrs)
            .iter()
            .any(|t| t == "Message")
    })
}

/// The baked stdlib modules whose real (non-`#[compiler_builtin]`) impl
/// bodies codegen compiles via the generalized stdlib-body passes, beyond
/// the special-cased `tracing` program. Phase-7 line 889 grows this list
/// one module at a time as each module's bodies are verified to lower.
///
/// `ordering` is always present (its bodies prune to nothing when unused);
/// `protobuf` is included only when the user program uses it (see
/// [`program_uses_protobuf`] for why it can't rely on the prune). The three
/// consuming passes (generic-fn declare, layout/signature declare, body
/// compile) MUST all call this with the same `user` program so a module is
/// declared iff its bodies are compiled.
fn compiled_stdlib_programs(user: &Program) -> Vec<&'static Program> {
    let mut programs = vec![
        ordering_stdlib_program(),
        mem_stdlib_program(),
        regex_stdlib_program(),
        process_stdlib_program(),
        pool_stdlib_program(),
    ];
    if program_uses_protobuf(user) {
        programs.push(protobuf_stdlib_program());
    }
    programs
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
    cg.load_deque_head_locals(program);
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
    cg.load_deque_head_locals(program);
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
    cg.load_deque_head_locals(program);
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
    cg.load_deque_head_locals(program);
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
    cg.load_deque_head_locals(program);
    cg.load_concurrency_analysis(concurrency);
    cg.set_coro_enabled(true);
    cg.compile_program(program)?;

    let target_machine = create_target_machine()?;
    apply_optimization_passes(&cg.module, &target_machine, cg.bce.binsearch_assume_emitted)?;
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
    cg.load_deque_head_locals(program);
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
    cg.load_deque_head_locals(program);
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
    apply_optimization_passes(&cg.module, &target_machine, cg.bce.binsearch_assume_emitted)?;
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
    cg.load_deque_head_locals(program);
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

    apply_optimization_passes(&cg.module, &target_machine, cg.bce.binsearch_assume_emitted)?;
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
    cg.load_deque_head_locals(program);
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
    cg.load_deque_head_locals(program);
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
#[cfg(feature = "llvm")]
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

/// Alias-scope context for a sequential-tabulate loop (reduce.rs).
/// Built once per lowering site; `compile_vec_index` consults it to tag
/// element loads. Both maps hold READY-TO-ATTACH metadata lists:
/// `alias_scope[var]` = the single-scope list naming var's own scope,
/// `noalias[var]` = the list of scopes var's loads are asserted disjoint
/// from (the tabulate output scope). Scope validity is bounded by
/// `llvm.experimental.noalias.scope.decl` calls in the loop preheader —
/// required for soundness because an OUTER loop may swap which buffer a
/// binding holds between inner-loop executions (the LBM grid↔next swap);
/// per-execution scopes make cross-execution accesses unrelated.
pub(crate) struct TabulateAliasScopes<'ctx> {
    /// The function whose preheader declared these scopes. Tags apply
    /// only while compiling inside it (nested function emission — par
    /// workers, closures — must not inherit another function's scopes).
    pub(crate) fn_key: inkwell::values::FunctionValue<'ctx>,
    pub(crate) alias_scope: std::collections::HashMap<String, inkwell::values::MetadataValue<'ctx>>,
    pub(crate) noalias: std::collections::HashMap<String, inkwell::values::MetadataValue<'ctx>>,
}

pub(super) struct Codegen<'ctx> {
    /// The hasher currently being synthesized FOR — the `Map[K, V, H]` /
    /// `Set[T, H]` selector (B-2026-08-21-6). Set around the hash-function
    /// synthesis at each container-construction site and restored after.
    ///
    /// A field rather than a parameter because the five `emit_hash_fn_for_*`
    /// synthesizers recurse into one another (a tuple key hashes its fields, a
    /// `Vec[T]` key hashes its elements), so a parameter would have to be
    /// threaded through every one of them and every recursive call to reach
    /// the single leaf that consumes it — `emit_hash_bytes_call`. It also
    /// participates in the emitted symbol NAME, so a program using both
    /// hashers on the same key type gets two distinct cached functions rather
    /// than whichever one was synthesized first.
    pub(crate) hash_hasher: crate::hasher_kind::HasherKind,
    /// The program's `Map[K, V, H]` / `Set[T, H]` selectors, keyed by the
    /// container path's span — a clone of [`crate::ast::Program::container_hashers`],
    /// loaded at the top of `compile_program` because `Codegen::new` has no
    /// program in hand. Empty for every program that never names a hasher.
    pub(crate) container_hashers:
        rustc_hash::FxHashMap<crate::resolver::SpanKey, crate::hasher_kind::HasherKind>,
    /// `impl BuildHasher for B { type Hasher = S; … }` → `B` ⇒ `S`
    /// (B-2026-08-22-6). The trailing slot of `Map[K, V, B]` names the BUILDER,
    /// but `write` / `finish` live on the per-hash STATE type, so
    /// `emit_hash_bytes_call`'s user arm needs this hop to find the two symbols
    /// it calls. Loaded alongside `container_hashers` at the top of
    /// `compile_program`, and for the same reason: it is a plain-data reading
    /// of the AST, which `Codegen::new` does not have.
    pub(crate) user_hasher_states: rustc_hash::FxHashMap<String, String>,
    pub(crate) mod_bindings: ModBindings<'ctx>,
    pub(crate) borrow_vars: BorrowVars<'ctx>,
    pub(crate) span_tables: SpanTables,
    pub(crate) rc_elision: RcElision,
    pub(crate) type_decls: TypeDecls<'ctx>,
    pub(crate) fn_sig: FnSig<'ctx>,
    pub(crate) payload_vars: PayloadVars<'ctx>,
    pub(crate) closure_state: ClosureState<'ctx>,
    pub(crate) var_types: VarTypes<'ctx>,
    pub(crate) conc: ConcState<'ctx>,
    pub(crate) bce: BceState,
    pub(crate) fn_ctx: FnCtx<'ctx>,
    pub(crate) drop_rc: DropRc<'ctx>,
    pub(crate) pattern_state: PatternState<'ctx>,
    pub(crate) mono_state: MonoState<'ctx>,
    pub(crate) mapset: MapSet<'ctx>,
    pub(crate) accel: Accel<'ctx>,
    pub(crate) contract_state: ContractState<'ctx>,
    pub(crate) tracing: Tracing,
    pub(crate) display: Display<'ctx>,
    pub(crate) provider_state: ProviderState<'ctx>,
    pub(crate) target_abi: TargetAbi<'ctx>,
    /// Cached declarations of the C / `karac_*` runtime functions this
    /// module calls. Grouped out of `Codegen` as the first Phase-2
    /// decomposition slice — see
    /// [`docs/spikes/state-decomposition-codegen-methodcall.md`]. Every
    /// entry is declared once by `Codegen::new` and only ever read
    /// afterwards, which is what makes the group safe to move wholesale.
    pub(crate) runtime_fns: RuntimeFns<'ctx>,
    pub(crate) context: &'ctx Context,
    pub(crate) module: Module<'ctx>,
    pub(crate) builder: Builder<'ctx>,
    /// Maps variable name → (alloca pointer, value type).
    pub(crate) variables: HashMap<String, VarSlot<'ctx>>,
    /// B-2026-08-12-27 — span of a heap FIELD read off a Vec element
    /// (`ps[0].word`) → the alloca holding the deep CLONE emitted for it.
    ///
    /// The read used to hand back a shallow alias of the container's buffer, so
    /// every owning destination ended up sharing one pointer with the element
    /// and both freed it (eight measured double frees), while the `let` shape
    /// papered over it by cap-zeroing the SOURCE — a move, which `karac check`
    /// and the interpreter both contradict. `clone_vec_elem_heap_field_read`
    /// now clones at the read, so the destination and the element own separate
    /// buffers, exactly as the WHOLE-element read has always done
    /// (`clone_owned_vec_index_element`).
    ///
    /// The clone gets its own scope cleanup so a NON-consuming read
    /// (`ps[0].word.len()`, `ps[0].word + "!"`) does not leak — those are
    /// common and were clean before. This map is how a CONSUMING destination
    /// takes it over instead: keyed by the read's span, so
    /// `suppress_source_vec_cleanup_for_arg_ex` (and the `let` site) can zero
    /// the clone's `cap` and leave the destination sole owner. Span-keyed
    /// rather than name-keyed because the clone is anonymous — there is no
    /// binding to look up.
    pub(crate) vec_elem_field_clone_slots:
        std::collections::HashMap<(usize, usize), PointerValue<'ctx>>,
    /// B-2026-08-12-33 — the same clones as `vec_elem_field_clone_slots`, in
    /// EMISSION ORDER, so a later consumer can ask "was this span cloned
    /// *during this evaluation*" rather than "has it ever been cloned".
    ///
    /// The displaced-element drop needs the ordered form, and the map cannot
    /// answer it. An index-assign's guard clears an RHS that reaches the
    /// container only through values the emitted code actually deep-cloned; a
    /// map hit proves a clone happened at that SPAN at some point in the
    /// module, which is a different claim once the same source expression
    /// compiles twice (two monomorphs of one generic body) and the clone's
    /// type-driven gates decide differently in each. The stale hit would
    /// authorize freeing a buffer the second context still aliases — a double
    /// free, which is the one direction this family refuses to trade for a
    /// leak. Marking the log's length before the RHS is compiled and looking
    /// only past that mark makes the evidence local to the statement.
    ///
    /// Append-only for the life of the module; entries are never removed, and
    /// the log is read by index, so it costs one `usize` at each read site.
    pub(crate) vec_elem_field_clone_log: Vec<(usize, usize)>,
    /// B-2026-08-13-11 — set only while `maybe_defensive_copy_return_value` is
    /// running, so the shared helper can tell an ARGUMENT position from a RETURN
    /// one.
    ///
    /// The field-rooted Vec-element clone this row added belongs to the argument
    /// half only. In return position the same read is ALREADY reconciled — the
    /// three ASAN fixtures that pin it (`fn at(ref self, i) -> T { self.xs[i] }`
    /// and two siblings) went from clean to 72–78 B of leak when the clone fired
    /// there too, because the return path clones again and this one is left with
    /// no owner. One flag beats threading a bool through ~25 argument call sites
    /// to reach the one caller that needs to say no.
    pub(crate) in_return_defensive_copy: bool,
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
    pub(crate) current_fn: Option<FunctionValue<'ctx>>,
    /// The libc `FILE*` globals for stdout / stderr, used as the `fwrite`
    /// stream argument. The symbol name is platform-specific (`__stdoutp` /
    /// `__stderrp` on Apple, `stdout` / `stderr` elsewhere, incl. wasi-libc).
    pub(crate) stdout_global: inkwell::values::GlobalValue<'ctx>,
    pub(crate) stderr_global: inkwell::values::GlobalValue<'ctx>,
    /// Associated-type bindings from CONCRETE (non-generic) impl blocks,
    /// keyed by `(target_type_name, assoc_type_name)` → the bound `TypeExpr`.
    /// Populated once in `compile_program` from every `impl <Trait> for T {
    /// type Assoc = <ty>; … }`. Consulted by `llvm_type_for_type_expr` to
    /// resolve an associated-type PROJECTION (`C.Item`) that appears in a
    /// generic fn's signature: inside a monomorph the leading segment `C`
    /// resolves to its concrete type name via `type_subst_names`, and this
    /// table maps `(concrete, "Item")` → the bound type. Without it a
    /// `fn get[C: Container](c: C) -> C.Item` mono lowered its return type to
    /// the `i64`/`{}` default and failed module verification against the body's
    /// real (concrete) return value. Generic-impl bindings (`impl[T] … for
    /// Box[T] { type Item = T }`, where the RHS references the impl's params)
    /// are a follow-on — only concrete bindings are recorded here.
    pub(crate) assoc_type_bindings: HashMap<(String, String), crate::ast::TypeExpr>,
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
    /// Names of user struct/enum types whose `karac_cmp_<T>` ordering fn is
    /// mid-emission, so a self-referential field (`S { next: Vec[S] }`) that
    /// recurses back into the same type returns `None` (unorderable — the sort
    /// call site errors loudly) instead of infinitely recursing at compile
    /// time. See `emit_cmp_fn_for_struct` / `emit_cmp_fn_for_enum`.
    pub(crate) cmp_fn_in_progress: std::collections::HashSet<String>,
    /// Per-layout-monomorphization slice 3 — the active *return* layout of the
    /// monomorph currently being compiled (`Aos` outside a return-SoA mono).
    /// Saved/restored around the mono body like `layout_subst`; read by
    /// `declare_mono_function` to lower the LLVM return type to the SoA struct
    /// and by `compile_mono_function` to seed the returned local(s) as `Soa`
    /// (`docs/spikes/per-layout-monomorphization.md` §4.5).
    pub(crate) return_layout: LayoutId,
    /// Backward-inference one-shot: the receiving binding's layout, set by the
    /// SoA `let <name> = <call>()` arm just before the call's RHS is compiled
    /// and consumed (`take`n) at the top of `compile_call`, so the callee is
    /// monomorphized to *return* the receiving binding's physical layout
    /// (`init_grid()`-shape returns, spike §4.2 backward). `None` for every
    /// other call.
    pub(crate) pending_return_layout: Option<LayoutId>,
    /// Monotonic counter for synthesized identifier names emitted by the
    /// indexed-receiver method-dispatch lowering (`__indexed_elem_<n>`).
    /// Each call site that lowers an `Index { object, index }` receiver
    /// allocates one synth name, registers it in the variable + element-type
    /// registries pointing into the outer container's storage, dispatches the
    /// method through the existing identifier path, and cleans up after.
    pub(crate) indexed_elem_counter: u32,
    /// Monotonic id for the synthetic pattern-binding spans `?.` lowering
    /// mints. A chained `a?.b?.c` gives BOTH `OptionalChain` nodes the
    /// receiver's span, so a span-derived key collides and the outer chain's
    /// binding type overwrites the inner's — measured as "cannot resolve field
    /// 'city'" on design.md's own three-level example (B-2026-08-17-28).
    pub(crate) optional_chain_counter: u32,
    /// One-shot signal from an `Iterator.rev()` chain lowering (B-2026-07-18-41
    /// codegen leg): when set, the NEXT base-source for-loop (`compile_for_vec_var`
    /// / range / temp-Vec) iterates in REVERSE order and clears the flag. Set by
    /// the `compile_method_call` / `compile_for` rev guards after stripping the
    /// `.rev()` node and just before re-dispatching the stripped chain; consumed
    /// by the base iteration the terminal desugars to. Only ever set when the
    /// chain is reverse-SAFE (order-independent map/filter/inspect steps over a
    /// reverse-iterable Vec/array/range base), so the consuming arm is guaranteed
    /// reverse-aware — never a silent forward iteration.
    pub(crate) pending_reverse_iter: bool,
    /// B-2026-07-12-24 (residual): value-spans (offset,length) of `let`
    /// bindings in the CURRENTLY-compiled function whose binding name NEVER
    /// ESCAPES — every use is a direct `match` scrutinee, or the binding is
    /// unused. For a `Result[shared]` such binding it is safe to register a
    /// scope-exit `RcDecOption` (via `track_rc_result_var`): a non-escaping
    /// binding is consumed in place and never handed to another owner, so the
    /// dec can never double-free. Recomputed per function in `compile_function`
    /// by `crate::result_escape::nonescaping_let_value_spans` (a compiler-
    /// exhaustive AST walk — a new AST node breaks its build rather than
    /// silently missing a move-out). Escaping user bindings stay leaking (the
    /// documented residual) rather than risk a double-free.
    pub(crate) result_shared_nonescaping_let_spans: std::collections::HashSet<(usize, usize)>,
    /// B-2026-07-12-24 (residual, consuming-call leg): names of the CURRENTLY-
    /// compiled function's PARAMETERS that never escape (used only as a `match`
    /// scrutinee, or unused). An OWNED `Result[shared]` such param owns the
    /// caller's transferred `+1` and takes a scope-exit `RcDecOption` — closing
    /// the `eat(d)` by-value-consuming-call leak. A forwarded param (passed on
    /// to another consuming call) escapes → left out → the terminal consumer's
    /// dec stays the only one, so the chain self-balances with no call-site
    /// suppression. Recomputed per function in `compile_function` by
    /// `crate::result_escape::nonescaping_param_names`.
    pub(crate) result_shared_nonescaping_param_names: std::collections::HashSet<String>,
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
    /// B-2026-08-06-15 — the value-position block just compiled handed out a
    /// direct `shared` field as its tail (`let x = { let b = mk(); b.v };`), and
    /// `suppress_block_tail_cleanup` null-stored the source, TRANSFERRING the
    /// owner's single ref to the escaping value.
    ///
    /// The consumer must therefore NOT take a receive-inc: it already owns that
    /// ref, and inc'ing leaves the count at 1 forever (measured 32 B leaked per
    /// evaluation). It cannot be decided by `rhs_yields_fresh_ref`, which runs
    /// BEFORE the RHS is compiled — the block's inner binding is not in scope
    /// yet, so the field's type cannot be resolved there. Recorded at the point
    /// the transfer actually happens and consumed at the let site, the same
    /// shape as `last_fstr_acc` above.
    ///
    /// Cleared before each let RHS is compiled and again once consumed, so it
    /// only ever describes the block immediately to its left.
    pub(crate) block_tail_shared_transfer: bool,
    /// B-2026-07-22-2 — the most recent FRESH call-result struct temp
    /// materialized by a field access in expression position
    /// (`println(mk().s)` / `take(mk().s)` / `mkv().v.len()`):
    /// `(slot, struct_name, accessed_field, object_span_key)`. The temp's
    /// struct drop is registered at the access; the extracted field value is
    /// a BORROW for read consumers, and a MOVE consumer (let / assign /
    /// return / fn tail) zeroes the accessed field's heap in the slot via
    /// `consume_freshtemp_field_move` (matched on field name + object span
    /// so a stale entry can never disarm an unrelated statement's temp).
    pub(crate) freshtemp_field_access_slot:
        Option<(PointerValue<'ctx>, String, String, (usize, usize))>,
    /// Per-function scoped-alias metadata for slice parameters (alias-metadata
    /// slice 4). Keyed by param binding name → the `!alias.scope` / `!noalias`
    /// nodes attached to the element load/store in `compile_slice_index` /
    /// `_store`. Rebuilt at each function/mono entry by `build_slice_alias_scopes`
    /// (empty unless the function has ≥1 exclusive `mut Slice` param and ≥2 slice
    /// params); a name is dropped on any re-registration
    /// (`register_var_from_type_expr`) so a shadowing local loses the metadata.
    pub(crate) slice_alias_md: HashMap<String, slice_alias::SliceAliasMd>,
    /// LLJIT Slice 6c prerequisite (B-2026-07-08-5 fix): the index sub-pattern
    /// of a `for (i, v) in xs.iter().enumerate()` loop, threaded from
    /// `compile_for`'s `.enumerate()` arm into the underlying container loop
    /// (`compile_for_{vec,slice,array}_var`). Those loops already carry the
    /// storage index as their induction variable — which is exactly the
    /// enumerate index — so they `take()` this and bind it to the loop's `cur`
    /// alongside the element. `take()` (not clone) so a NESTED loop inside the
    /// enumerate body doesn't inherit it. Before this, `.enumerate()` fell
    /// through `compile_for`'s dispatch to the silent skip-body arm, so every
    /// enumerate loop body was a no-op under codegen (outer mutations lost) —
    /// interpreter-vs-codegen divergence, e.g. two_sum printing "No solution".
    pub(crate) enumerate_index_pattern: Option<crate::ast::Pattern>,
    /// B-2026-08-10-18 — spans of fused-iterator CLOSURE BODIES whose
    /// `return` must be retargeted to the body's own value.
    ///
    /// These emitters splice the closure body into a synthesized loop and hand
    /// the whole loop to the ordinary compiler, so they never own the body's
    /// `compile_expr` call and cannot wrap it the way the sort-comparator
    /// emitter does (B-2026-08-10-16). Keying on the body's SPAN lets
    /// `compile_expr` recognise it wherever the splice put it and scope the
    /// retarget to exactly that sub-expression — the merge block then lands at
    /// the body's continuation, which is what makes `return` mean "this
    /// element's value" rather than "exit the loop" (or, before this, "return
    /// from the enclosing function").
    pub(crate) iter_body_retarget_spans: std::collections::HashSet<(usize, usize)>,
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
    /// Subset of `boxed_export_names` whose box is a Slice-2a tagged-union
    /// `#[repr(C)]` enum (`{ i64 tag, i64 w0 }`), not a `{data,len,cap}` Vec
    /// box. The distinction is load-bearing at destructor-emit time: the
    /// Vec-box destructor frees an inner `data` buffer, which would
    /// catastrophically misinterpret an enum's payload word as a pointer —
    /// the enum box owns nothing but itself, so its `karac_free_<fn>` only
    /// frees the box.
    pub(crate) boxed_enum_export_names: std::collections::HashSet<String>,
    /// True only while compiling the RHS of a `let <name> = <ref-returning
    /// call>` — the one caller context that binds the borrow as a ref-local
    /// (deref on use). Outside it, a call to a borrow-returning function is
    /// rejected by `compile_call` rather than silently miscompiled (the
    /// returned `ptr` would be mishandled as a value). Direct use of a
    /// borrow-returning call result is a tracked Tier-1.5 follow-on
    /// (B-2026-06-07-5).
    pub(crate) compiling_ref_return_let_rhs: bool,
    /// Set by the `StmtKind::Let` arm around its `bind_pattern` call for a
    /// type-changing shadow (`let s = "x"; let s = s.len();`). The Let arm
    /// manages the rebound name's per-variable sidecar metadata itself via
    /// the take/restore dance in `shadow.rs` (it must keep the OLD class
    /// tags live while the RHS may still reference the old binding, then
    /// install pure-NEW tags before the bind). `bind_pattern`'s own
    /// rebind-purge would wipe those just-installed NEW tags, so it skips
    /// the purge while this flag is set. For-loop / match-arm / destructure
    /// callers leave it `false` — they re-register the new binding's
    /// metadata *after* `bind_pattern`, so the purge there is exactly right.
    pub(crate) suppress_shadow_metadata_purge: bool,
    /// B-2026-07-18-2 — for-loop STRICT-SHARED copy-support mode for
    /// `field_copy_supported`: a DIRECT bare-`shared` field becomes supported
    /// (the move-out "copy" is an rc-INC via `deep_copy_rc_inc_bare_shared`,
    /// symmetric with the element drain's rc-DEC), while `Option`/`Result`
    /// fields become UNSUPPORTED — a shared-bearing struct's drain skips
    /// Option fields (the synthesis gate sees it as non-copy-supported), so a
    /// registered element's aliased Option leaf would lose its `!view_src`
    /// leaf-cleanup and leak (caught by
    /// `asan_vec_of_struct_shared_and_option_field_consumed_no_leak`). The
    /// for-loop registration gate ORs this strict variant with the classic
    /// predicate, so legacy-supported shapes are untouched. Consulted ONLY in
    /// `field_copy_supported`; false everywhere else.
    pub(crate) copy_support_for_loop_shared_mode: bool,

    /// Materialized iterator bindings (B-2026-07-11-19): a `let it =
    /// <iter-chain>` whose RHS is a fusable iterator chain (`v.iter()...`, a
    /// range) is NOT codegen'd as a value (codegen has no runtime iterator);
    /// instead the binding name maps to the (already-inlined) chain expr, and
    /// each `it.<adaptor|terminal>(..)` use substitutes the chain as the
    /// receiver so the existing fused terminals handle it. Not scope-tracked —
    /// a later same-named binding overwrites; a non-iterator `let` never
    /// registers here.
    pub(crate) iter_let_bindings: HashMap<String, Expr>,
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
    /// Inner `T` of each entry in [`Self::user_ref_method_names`], by method
    /// name. Declaration-derived, so unlike the span-keyed
    /// `ref_return_inner_types` it survives the parser's chained-call span
    /// aliasing — see the population site (B-2026-07-29-12).
    pub(crate) user_ref_method_inner: std::collections::HashMap<String, TypeExpr>,
    /// Tuple ELEMENT indices moved out of a let-bound tuple (`let x = t.0`),
    /// per variable. The element's body now belongs to the destination, so the
    /// tuple's `__karac_dropelems_tuple_*` walk must skip it — without the mask
    /// the source fired a SECOND body over the cap-zeroed slot (`drop 1 ` with
    /// an empty name), while the interpreter fired a full duplicate. Accumulated
    /// across move-outs so `let a = t.0; let b = t.1;` masks both. Cleared per
    /// function alongside `tuple_var_elem_tes` (B-2026-08-03-3).
    pub(crate) tuple_moved_elem_bodies: HashMap<String, std::collections::HashSet<u32>>,
    /// The synthesized `void __karac_static_init()` function, declared
    /// in `declare_module_bindings` when `map_set_module_inits` is
    /// non-empty so `main`'s entry can emit a forward `call` to it, and
    /// filled in at `finalize_module_binding_static_init` once all type
    /// metadata is available. `None` when no Map/Set module binding
    /// exists.
    pub(crate) static_init_fn: Option<FunctionValue<'ctx>>,
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
    /// B-2026-08-01-33 mechanism 2 — `shared` type names the ownership pass
    /// promoted to atomic refcounting. Read by `heap_type_uses_atomic_rc`, the
    /// funnel all four refcount dispatchers share. Empty unless a multi-branch
    /// capture was admitted, so nothing changes by default.
    pub(crate) atomic_promoted_types: HashSet<String>,
    /// B-2026-08-01-33 mechanism 3, stage 2.5 — initializer spans of the `let`
    /// bindings the ownership pass admitted as non-counting aliases of a place
    /// rooted at a `frozen` parameter. A hit means: skip the `Vec`-element
    /// clone, skip the receive-inc, and register NO scope-exit cleanup — the
    /// caller's value stays the sole owner. Empty unless the program uses the
    /// `frozen` mode.
    pub(crate) frozen_alias_bindings: FxHashSet<SpanKey>,
    /// B-2026-08-01-33 mechanism 3, stage 3c — initializer spans of the `let`s
    /// the ownership pass proved to be FROZEN-ELEMENT CONTAINERS. A hit at a
    /// `let` means the binding's name joins `frozen_elem_vec_owners`, which
    /// drives two suppressions that are a matched PAIR: no retain at
    /// `c.push(x)`, and no per-element release in the scope-exit drop (the
    /// buffer is still freed). Empty unless the program uses the `frozen` mode.
    pub(crate) frozen_element_containers: FxHashSet<SpanKey>,
    /// Stage 3c, per function — the NAMES of the containers above, resolved at
    /// each `let` and consulted at the push site and the cleanup registration.
    /// Reset per function, because the hint set is span-keyed and
    /// program-wide while these names are not unique across functions.
    pub(crate) frozen_elem_vec_owners: HashSet<String>,
    /// Level 2 crash diagnostics — Part 2: DWARF debug-info state. `Some` only
    /// when `KARAC_DEBUG_INFO` is on AND a source filename is threaded in;
    /// `None` (the default) makes every `di_*` hook a cheap early-return so the
    /// standard codegen path is byte-for-byte unchanged. See `debug_info.rs`.
    pub(crate) debug_info: Option<debug_info::DebugInfo<'ctx>>,
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
    /// Active sequential-tabulate alias-scope context (reduce.rs §
    /// seq tabulate). While `Some` AND `current_fn` matches the stored
    /// function, `compile_vec_index` tags element loads of the listed
    /// Vec variables with `!alias.scope`/`!noalias` metadata asserting
    /// disjointness from the tabulate output buffer — the
    /// ownership-derived guarantee (two distinct owned Vec locals never
    /// share storage) that lets LLVM's loop vectorizer skip its runtime
    /// memchecks (which false-conflict on exactly-adjacent buffers; see
    /// the phase-10 CPU-codegen forensics entry). The function key
    /// prevents scope leakage into nested function emission (par
    /// workers, closures): scopes are declared via
    /// `llvm.experimental.noalias.scope.decl` in THIS function's loop
    /// preheader and are only sound within it.
    pub(crate) tabulate_alias_scopes: Option<TabulateAliasScopes<'ctx>>,
    /// True while `declare_stdlib_program` is walking a baked `STDLIB_PROGRAMS`
    /// tree rather than the user's. Those `StructDef`s are parsed straight from
    /// stdlib source and never had `stdlib_origin` flipped (only the
    /// `synthetic_*` clone paths do that), so without this flag the
    /// prelude-shadowing detector sees the stdlib's own `Regex` / `Match` and
    /// flags the stdlib against itself — refusing every legitimate regex
    /// program. See B-2026-08-02-13.
    pub(crate) declaring_stdlib_program: bool,
    /// HTTP handler ABI trampoline (2026-05-09): cache of per-handler-fn
    /// `extern "C"` shims. Key is the user handler's mangled fn name (e.g.
    /// `"handle"`); value is the synthesized shim function. Sharing the
    /// shim across multiple `Server.serve(handler)` calls in one program
    /// avoids redundant emission and keeps the IR stable. Pinned by
    /// `tests/codegen.rs::test_server_serve_handler_shim_caches`.
    pub(crate) http_shim_cache: HashMap<String, FunctionValue<'ctx>>,
    /// Lazily-initialized `TargetData` consumed by the layout-introspection
    /// intrinsics (`align_of[T]()`, `offset_of[T](field)`). Constructed
    /// via `create_target_machine().get_target_data()` on first use; the
    /// rest of codegen never reads it. Held as `Option` because the
    /// host-target initialization pulls in `Target::initialize_native`,
    /// which we want to avoid in the (common) path where no layout
    /// intrinsic is invoked.
    pub(crate) target_data: Option<TargetData>,
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
}

/// Apply the malloc-family allocator attributes to an alloc/realloc wrapper
/// *declaration* (`karac_alloc_fallible` / `karac_alloc_or_panic` /
/// `karac_realloc_or_panic`), so LLVM stops treating each opaque extern as a
/// clobber-everything barrier — the alloc-side twin of the free-family set on
/// `karac_free_buf` (B-2026-07-17-9 / phase-10 line 284). Modeling the call's
/// real memory effects (`memory(inaccessiblemem: readwrite)` — a plain alloc
/// touches only allocator-internal state) lets DSE / store-forwarding / LICM
/// run across the alloc, recovering ~2.4× on a dead-buffer churn loop on top of
/// the free-side win (the "elide the stores into them" half of the entry).
///
/// **`noalias`-return and `allocsize` are DELIBERATELY NOT applied.** `noalias`
/// on the return is what LLVM additionally requires to REMOVE a dead allocation
/// (the malloc+free-PAIR elision), but it is UNSOUND under Kāra's codegen — the
/// large-buffer recycling cache hands back recently-freed addresses and the
/// move/aliasing lowering keeps multiple SSA pointers to one buffer, both of
/// which violate the noalias-return precondition. Applying it miscompiled 15
/// E2E programs (bounds-elision, Vec sort/retain/with-capacity-grow, enum
/// moves) into wrong output / empty output. `allocsize` buys nothing without
/// the removal it would enable and feeds LLVM's object-size / bounds reasoning,
/// so it is left off too. See phase-10 line 284 for the bisect.
///
/// `allockind_bits`: `Alloc`(1) for the byte-size wrappers, `Realloc`(2) for the
/// resizer — each OR'd with `Uninitialized`(8) because the buffers are
/// malloc-backed, NOT zeroed (the `Zeroed` bit is DELIBERATELY absent so LLVM
/// never folds a read-before-write to 0). `argmem_rw`: true only for realloc (it
/// reads the old buffer); a plain alloc touches only allocator-internal
/// (inaccessible) memory. `willreturn`: false for the `_or_panic` variants (they
/// abort on OOM). `realloc_ptr_param`: marks the resized allocation —
/// `allockind` `Realloc` is inert without it, exactly like free's `allocptr`.
fn apply_alloc_family_attrs(
    context: &Context,
    fn_val: FunctionValue<'_>,
    allockind_bits: u64,
    willreturn: bool,
    argmem_rw: bool,
    realloc_ptr_param: Option<u32>,
) {
    use inkwell::attributes::{Attribute, AttributeLoc};
    let enum_attr = |name: &str, val: u64| {
        context.create_enum_attribute(Attribute::get_named_enum_kind_id(name), val)
    };
    // memory(inaccessiblemem: readwrite [, argmem: readwrite]) — 2 bits per
    // location (argmem = bits[1:0], inaccessiblemem = bits[3:2], ModRef = 0b11);
    // matches the free-side `0b1111` for argmem+inaccessible readwrite.
    let memory = if argmem_rw { 0b1111 } else { 0b1100 };
    fn_val.add_attribute(AttributeLoc::Function, enum_attr("memory", memory));
    fn_val.add_attribute(
        AttributeLoc::Function,
        enum_attr("allockind", allockind_bits),
    );
    fn_val.add_attribute(AttributeLoc::Function, enum_attr("nounwind", 0));
    fn_val.add_attribute(AttributeLoc::Function, enum_attr("mustprogress", 0));
    if willreturn {
        fn_val.add_attribute(AttributeLoc::Function, enum_attr("willreturn", 0));
    }
    fn_val.add_attribute(
        AttributeLoc::Function,
        context.create_string_attribute("alloc-family", "malloc"),
    );
    if let Some(p) = realloc_ptr_param {
        fn_val.add_attribute(AttributeLoc::Param(p), enum_attr("allocptr", 0));
    }
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
        let mut init_target_data = None;
        if let Ok(tm) = create_target_machine() {
            module.set_triple(&tm.get_triple());
            module.set_data_layout(&tm.get_target_data().get_data_layout());
            // Capture the TargetData now so `&self` emitters (the scope-exit
            // cleanup drain's recycling-hint math) can read it immutably —
            // `ensure_target_data` stays as the lazy path for the cold start
            // where the machine couldn't be created here.
            init_target_data = Some(tm.get_target_data());
        }
        let builder = context.create_builder();

        let i32_type = context.i32_type();
        let i64_type = context.i64_type();
        let ptr_type = context.ptr_type(AddressSpace::default());
        let printf_type = i32_type.fn_type(&[BasicMetadataTypeEnum::from(ptr_type)], true);
        let printf_fn = module.add_function("printf", printf_type, None);

        // `size_t` is i32 on wasm32 (wasi-libc) and i64 natively. Used by the
        // size-bearing libc decls below (`snprintf`, `fwrite`, `malloc`). Even
        // though `snprintf` is varargs, its FIXED `size_t n` param must match
        // libc's width exactly or wasm-ld replaces the call with a trapping
        // `signature_mismatch:snprintf` stub (B-2026-06-14-15: numeric f-strings
        // aborted on every wasm target — `i64` here mismatched wasi-libc's i32).
        let size_t_type = if crate::target::active_target_is_wasm() {
            i32_type
        } else {
            i64_type
        };

        // `int snprintf(char* buf, size_t n, const char* fmt, ...)` for f-string
        // integer/float → string conversion.
        let snprintf_type = i32_type.fn_type(
            &[
                BasicMetadataTypeEnum::from(ptr_type),
                BasicMetadataTypeEnum::from(size_t_type),
                BasicMetadataTypeEnum::from(ptr_type),
            ],
            true,
        );
        let snprintf_fn = module.add_function("snprintf", snprintf_type, Some(Linkage::External));

        // `void karac_runtime_write_console(ptr data, size_t len, ptr stream)`
        // — the auto-par ordered-output console chokepoint (runtime/src/lib.rs),
        // the sole console-write primitive emitted now (it replaced the inline
        // `fwrite`/`printf` calls so a capture can intercept every print). `len`
        // is `size_t`-width to match the runtime's `usize` param (i32 on wasm32,
        // i64 native), the same width discipline `snprintf` above uses; a
        // mismatch traps on wasm. `stream` is the loaded `stdout`/`stderr`
        // `FILE*` global, forwarded so the no-capture path can `fwrite` to it.
        let write_console_type = context.void_type().fn_type(
            &[
                BasicMetadataTypeEnum::from(ptr_type),
                BasicMetadataTypeEnum::from(size_t_type),
                BasicMetadataTypeEnum::from(ptr_type),
            ],
            false,
        );
        // Runtime ordered-output console chokepoint (capture-capable; declared
        // so the finalized wrapper can call it). Codegen routes every write
        // through the internal `__karac_write_console` wrapper below, whose body
        // — defined in `finalize_write_console_wrapper` once all function bodies
        // are in — calls this runtime chokepoint ONLY when the module uses
        // parallelism (a `karac_par_run` / `karac_par_reduce` site exists;
        // `karac_par_run` is the sole `OutputCapture` installer). A non-parallel
        // binary's wrapper does a lean direct `fwrite`, so it references neither
        // the chokepoint nor the `OutputCapture` machinery and AOT `-dead_strip`s
        // them — restoring the lean binary-size floor `1a401c7b` regressed
        // ~17 KiB (B-2026-06-15-2).
        module.add_function(
            "karac_runtime_write_console",
            write_console_type,
            Some(Linkage::External),
        );
        let write_console_fn = module.add_function(
            "__karac_write_console",
            write_console_type,
            Some(Linkage::Internal),
        );
        // B-2026-07-30-9 — the line-atomic sibling. Body is emitted by
        // `finalize_write_console_line_wrapper`, which must run AFTER
        // `finalize_write_console_wrapper` so the callee already has one.
        let write_console_line_type = {
            let size_t = if crate::target::active_target_is_wasm() {
                context.i32_type()
            } else {
                context.i64_type()
            };
            let ptr_ty = context.ptr_type(AddressSpace::default());
            context.void_type().fn_type(
                &[
                    ptr_ty.into(),
                    size_t.into(),
                    ptr_ty.into(),
                    size_t.into(),
                    ptr_ty.into(),
                ],
                false,
            )
        };
        let write_console_line_fn = module.add_function(
            "__karac_write_console_line",
            write_console_line_type,
            Some(Linkage::Internal),
        );

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
        // `void karac_free_buf(u8* data, size_t bytes_hint)` — the `bytes_hint`
        // is a Rust `usize`, so it must be the target's pointer-width int
        // (`size_t_type`: i32 on wasm32, i64 on 64-bit native). Hardcoding i64
        // here matched native by accident but produced a `(i32-ptr, i64)`
        // import against wasi-libc's `(i32, i32)` definition on wasm32 — wasm-ld
        // can't reconcile that, so it emits a trapping `signature_mismatch:
        // karac_free_buf` stub, and every wasm program that frees a heap buffer
        // traps at runtime (`unreachable`). Native never sees it (usize=i64),
        // which is why it survived until an in-browser heap-freeing program
        // (Prism) exercised the path. Same fix the sibling size-taking externs
        // (snprintf, alloc) already carry via `size_t_type`.
        let free_buf_type = context.void_type().fn_type(
            &[
                BasicMetadataTypeEnum::from(ptr_type),
                BasicMetadataTypeEnum::from(size_t_type),
            ],
            false,
        );
        let free_buf_fn =
            module.add_function("karac_free_buf", free_buf_type, Some(Linkage::External));
        // LLVM knows libc `free`'s semantics BY NAME (TargetLibraryInfo); an
        // opaque replacement is a clobber-everything call that kills store
        // forwarding / LICM / dead-heap elimination around every cleanup
        // drain (measured +55% instructions on a String-churn loop). Declare
        // the same contract explicitly: touches only its pointee + allocator
        // internals, always returns, never unwinds, and is the malloc
        // family's free (`allockind("free")` value = AllocFnKind::Free).
        {
            use inkwell::attributes::{Attribute, AttributeLoc};
            // memory(argmem: readwrite, inaccessiblemem: readwrite) —
            // 2 bits per location: argmem=0b11, inaccessible=0b11<<2.
            let memory_kind = Attribute::get_named_enum_kind_id("memory");
            let allockind_kind = Attribute::get_named_enum_kind_id("allockind");
            for attr in [
                context.create_enum_attribute(memory_kind, 0b1111),
                context.create_enum_attribute(allockind_kind, 1 << 2),
                context.create_enum_attribute(Attribute::get_named_enum_kind_id("nounwind"), 0),
                context.create_enum_attribute(Attribute::get_named_enum_kind_id("willreturn"), 0),
                context.create_enum_attribute(Attribute::get_named_enum_kind_id("mustprogress"), 0),
            ] {
                free_buf_fn.add_attribute(AttributeLoc::Function, attr);
            }
            free_buf_fn.add_attribute(
                AttributeLoc::Function,
                context.create_string_attribute("alloc-family", "malloc"),
            );
            // `allockind("free")` is inert without `allocptr` marking WHICH
            // argument is the freed allocation — this is what lets DSE kill
            // stores into (and then the allocation of) a buffer whose only
            // remaining use is this call, exactly as it does for libc free.
            for param_attr in ["allocptr", "nocapture", "noundef"] {
                free_buf_fn.add_attribute(
                    AttributeLoc::Param(0),
                    context.create_enum_attribute(Attribute::get_named_enum_kind_id(param_attr), 0),
                );
            }
        }
        // Alloc-side twin of the free-family attributes above (phase-10 line
        // 284): `Alloc | Uninitialized` (0b1001), touches only allocator-internal
        // memory. `_fallible` always returns (null on OOM → `willreturn`);
        // `_or_panic` aborts on OOM (no `willreturn`).
        apply_alloc_family_attrs(context, alloc_fallible_fn, 0b1001, true, false, None);
        apply_alloc_family_attrs(context, alloc_or_panic_fn, 0b1001, false, false, None);

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
        // B-2026-08-17-14 — the depth-guarded AUTO-PAR sibling: same ABI,
        // but a call reached from inside a par worker runs its branches
        // inline instead of convoying the already-saturated pool. Explicit
        // `par {}` keeps `karac_par_run` (siblings may rendezvous).
        let karac_par_run_auto_fn = module.add_function(
            "karac_par_run_auto",
            karac_par_run_type,
            Some(Linkage::External),
        );

        // Auto-par reduction-lowering runtime entry (slice 3a, 2026-05-19).
        // `karac_par_reduce(*const KaracReduceDescriptor, *mut u8 out_slot,
        // u32 spawn_site_id) -> ()` — see runtime/src/lib.rs's
        // `karac_par_reduce` doc-comment for the ABI shape. Declared
        // alongside `karac_par_run` so future slices (3b, the actual
        // lowering of recognized reductions into a fan-out + serial-combine
        // call) can route through `self.runtime_fns.karac_par_reduce_fn` without
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
        // WebSocket-upgrade variant (phase-8 line 170): serve_http's shape
        // plus a second `extern "C" fn(i64 fd)` handler slot for the
        // upgraded-connection callback. Backs
        // `Server.serve_ws(addr, handler, ws_handler)`.
        let karac_runtime_serve_ws_type = context.i32_type().fn_type(
            &[
                ptr_type.into(), // addr_cstr
                ptr_type.into(), // http handler shim fn-ptr
                ptr_type.into(), // ws handler shim fn-ptr
                ptr_type.into(), // bound_port_out
            ],
            false,
        );
        let _karac_runtime_serve_ws_fn = module.add_function(
            "karac_runtime_serve_ws",
            karac_runtime_serve_ws_type,
            Some(Linkage::External),
        );
        // TLS twin: `serve_https`'s cert/key params + the ws handler slot.
        let karac_runtime_serve_ws_tls_type = context.i32_type().fn_type(
            &[
                ptr_type.into(), // addr_cstr
                ptr_type.into(), // cert_pem
                i64_type.into(), // cert_len
                ptr_type.into(), // key_pem
                i64_type.into(), // key_len
                ptr_type.into(), // http handler shim fn-ptr
                ptr_type.into(), // ws handler shim fn-ptr
                ptr_type.into(), // bound_port_out
            ],
            false,
        );
        let _karac_runtime_serve_ws_tls_fn = module.add_function(
            "karac_runtime_serve_ws_tls",
            karac_runtime_serve_ws_tls_type,
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
        // `std.process` runtime externs (`runtime/src/process.rs`,
        // phase-8 P1 codegen leg). Same KaracIoResult out-param ABI as
        // the file family. `spawn` takes the program String as a
        // *descriptor pointer* (SSO-safe) plus the raw `Vec[String]` /
        // `Vec[EnvVar]` buffer pointers + element counts (the runtime
        // strides the buffers natively — no codegen-side loops) and the
        // three `Stdio` enum tags.
        let process_spawn_type = file_call_void_type.fn_type(
            &[
                ptr_type.into(), // out: *mut KaracIoResult
                ptr_type.into(), // prog: *const RuntimeKaracString
                ptr_type.into(), // args data: *const RuntimeKaracString
                i64_type.into(), // args len
                ptr_type.into(), // env data: *const KaracEnvVarView
                i64_type.into(), // env len
                i64_type.into(), // stdin tag
                i64_type.into(), // stdout tag
                i64_type.into(), // stderr tag
            ],
            false,
        );
        module.add_function(
            "karac_runtime_process_spawn",
            process_spawn_type,
            Some(Linkage::External),
        );
        // wait / try_wait / kill: (out, pid) -> void.
        let process_pid_type =
            file_call_void_type.fn_type(&[ptr_type.into(), i64_type.into()], false);
        for sym in [
            "karac_runtime_process_wait",
            "karac_runtime_process_try_wait",
            "karac_runtime_process_kill",
        ] {
            module.add_function(sym, process_pid_type, Some(Linkage::External));
        }
        // take_stream: (pid, which) -> i64 (pid when taken, 0 → None).
        let process_take_type = i64_type.fn_type(&[i64_type.into(), i64_type.into()], false);
        module.add_function(
            "karac_runtime_process_take_stream",
            process_take_type,
            Some(Linkage::External),
        );
        // read_to_string: (out, pid, which) -> void (StringPayload Ok).
        let process_read_type = file_call_void_type
            .fn_type(&[ptr_type.into(), i64_type.into(), i64_type.into()], false);
        module.add_function(
            "karac_runtime_process_read_to_string",
            process_read_type,
            Some(Linkage::External),
        );
        // stdin_write: (out, pid, data: *const RuntimeKaracString) -> void.
        let process_write_type = file_call_void_type
            .fn_type(&[ptr_type.into(), i64_type.into(), ptr_type.into()], false);
        module.add_function(
            "karac_runtime_process_stdin_write",
            process_write_type,
            Some(Linkage::External),
        );
        // stdin_close: (pid) -> void.
        let process_close_type = file_call_void_type.fn_type(&[i64_type.into()], false);
        module.add_function(
            "karac_runtime_process_stdin_close",
            process_close_type,
            Some(Linkage::External),
        );
        // f-string format-spec runtime formatter (`runtime/src/fmt.rs`) — the
        // shared-renderer path for binary / center-align / custom-fill specs
        // `snprintf` can't express. Each takes the raw spec bytes + the value +
        // a caller output buffer, returns bytes written. Sizes are `i64` on
        // every target (the Rust ABI uses `i64`, so no wasm size-switching).
        let fmt_int_type = i64_type.fn_type(
            &[
                ptr_type.into(), // spec_ptr
                i64_type.into(), // spec_len
                i64_type.into(), // value
                i32_type.into(), // is_unsigned
                ptr_type.into(), // out_buf
                i64_type.into(), // out_cap
            ],
            false,
        );
        module.add_function(
            "karac_runtime_fmt_int",
            fmt_int_type,
            Some(Linkage::External),
        );
        let fmt_float_type = i64_type.fn_type(
            &[
                ptr_type.into(),
                i64_type.into(),
                context.f64_type().into(), // value
                ptr_type.into(),
                i64_type.into(),
            ],
            false,
        );
        module.add_function(
            "karac_runtime_fmt_float",
            fmt_float_type,
            Some(Linkage::External),
        );
        let fmt_str_type = i64_type.fn_type(
            &[
                ptr_type.into(), // spec_ptr
                i64_type.into(), // spec_len
                ptr_type.into(), // s_ptr
                i64_type.into(), // s_len
                ptr_type.into(), // out_buf
                i64_type.into(), // out_cap
            ],
            false,
        );
        module.add_function(
            "karac_runtime_fmt_str",
            fmt_str_type,
            Some(Linkage::External),
        );
        // `df.write_csv(path)` — serialize a DataFrame control block to a
        // CSV file in the runtime (walks the fixed entry/Column layouts;
        // phase-11 CSV leg): (out, df_ctrl, path_ptr, path_len) -> void.
        // Unit Ok payload, same KaracIoResult ABI as fs_write.
        let df_write_csv_type = file_call_void_type.fn_type(
            &[
                ptr_type.into(),
                ptr_type.into(),
                ptr_type.into(),
                i64_type.into(),
            ],
            false,
        );
        module.add_function(
            "karac_runtime_df_write_csv",
            df_write_csv_type,
            Some(Linkage::External),
        );
        // `DataFrame.read_csv(path)` — parse a CSV file into a fresh
        // DataFrame control-block graph built runtime-side (phase-11 CSV
        // leg): (out_io, out_df, path_ptr, path_len) -> void. Two
        // out-params like fs_read_lines; the Ok payload is the df control
        // pointer written through out_df.
        let df_read_csv_type = file_call_void_type.fn_type(
            &[
                ptr_type.into(),
                ptr_type.into(),
                ptr_type.into(),
                i64_type.into(),
            ],
            false,
        );
        module.add_function(
            "karac_runtime_df_read_csv",
            df_read_csv_type,
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
        // Durability: (out, handle) -> void, same shape as flush but a
        // real fsync/fdatasync rather than a userspace-buffer push.
        module.add_function(
            "karac_runtime_file_sync_all",
            file_flush_type,
            Some(Linkage::External),
        );
        module.add_function(
            "karac_runtime_file_sync_data",
            file_flush_type,
            Some(Linkage::External),
        );
        // Seek: (out, handle, whence: u8, offset: i64) -> void
        // (B-2026-08-10-3). The runtime symbol has existed since the
        // File slice shipped — exported early precisely so adding the
        // surface needs no runtime rebuild — and is already on the
        // `__preserve_no_mangle_symbols` keep-list, so the JIT runner
        // resolves it too.
        let file_seek_type = file_call_void_type.fn_type(
            &[
                ptr_type.into(),
                ptr_type.into(),
                context.i8_type().into(),
                i64_type.into(),
            ],
            false,
        );
        module.add_function(
            "karac_runtime_file_seek",
            file_seek_type,
            Some(Linkage::External),
        );
        // `FileSystem.read_lines(path)` — one-shot whole-file read split
        // into a `Vec[String]` of lines: (out_io, out_vec, path_ptr,
        // path_len) -> void. Two out-params: the KaracIoResult (Ok/Err
        // status) *and* a KaracVec of RuntimeKaracString elements. The Ok
        // payload is the Vec (built into the KaracVec slot); the Err path
        // leaves the vec empty and fills the IoError through KaracIoResult.
        // B-2026-07-11-38.
        let fs_read_lines_type = file_call_void_type.fn_type(
            &[
                ptr_type.into(),
                ptr_type.into(),
                ptr_type.into(),
                i64_type.into(),
            ],
            false,
        );
        module.add_function(
            "karac_runtime_fs_read_lines",
            fs_read_lines_type,
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
        // Regex FFI (B-2026-07-14-19) — the AOT backend for
        // `runtime/stdlib/regex.kara`'s `#[compiler_builtin]` `Regex.compile` /
        // `is_match`, resolved from the opt-in `libkarac_runtime_regex.a`
        // (`karac` selects it on any `karac_regex_*` reference).
        //   `u8 karac_regex_validate(*const u8 pat, i64 pat_len)` — 1 if the
        //    pattern compiles, backing `Regex.compile`'s Ok/Err decision.
        module.add_function(
            "karac_regex_validate",
            context
                .i8_type()
                .fn_type(&[ptr_type.into(), i64_type.into()], false),
            Some(Linkage::External),
        );
        //   `u8 karac_regex_is_match(*const u8 pat, i64 pat_len, *const u8 s,
        //    i64 s_len)` — 1 if `pat` matches anywhere in `s`; backs `is_match`.
        module.add_function(
            "karac_regex_is_match",
            context.i8_type().fn_type(
                &[
                    ptr_type.into(),
                    i64_type.into(),
                    ptr_type.into(),
                    i64_type.into(),
                ],
                false,
            ),
            Some(Linkage::External),
        );
        //   `u8 karac_regex_find(*const u8 pat, i64 pat_len, *const u8 s,
        //    i64 s_len, *mut i64 out_start, *mut i64 out_end)` — 1 (writing the
        //    leftmost match's byte offsets) or 0; backs `find`'s `Option[Match]`.
        module.add_function(
            "karac_regex_find",
            context.i8_type().fn_type(
                &[
                    ptr_type.into(),
                    i64_type.into(),
                    ptr_type.into(),
                    i64_type.into(),
                    ptr_type.into(),
                    ptr_type.into(),
                ],
                false,
            ),
            Some(Linkage::External),
        );
        //   `*mut i64 karac_regex_find_all(*const u8 pat, i64 pat_len,
        //    *const u8 s, i64 s_len, *mut i64 out_count)` — a malloc'd
        //    `[start0,end0,…]` offset array (caller frees) + count; backs
        //    `find_all`'s `Vec[Match]`.
        module.add_function(
            "karac_regex_find_all",
            ptr_type.fn_type(
                &[
                    ptr_type.into(),
                    i64_type.into(),
                    ptr_type.into(),
                    i64_type.into(),
                    ptr_type.into(),
                ],
                false,
            ),
            Some(Linkage::External),
        );
        //   `*mut u8 karac_arrow_column_to_ipc(*const u8 col_ctrl, i64 elem_size,
        //    i64 kind, *mut i64 out_len)` — serialize a Column to an Arrow IPC
        //    stream; returns a malloc'd buffer (caller owns as a `Vec[u8]`) +
        //    byte length, the `karac_regex_replace_all` convention. Resolved
        //    only by the opt-in `libkarac_runtime_arrow.a`, which `karac`
        //    auto-selects when this symbol is referenced
        //    (`driver.rs § SpecialArchive::Arrow`).
        module.add_function(
            "karac_arrow_column_to_ipc",
            ptr_type.fn_type(
                &[
                    ptr_type.into(),
                    i64_type.into(),
                    i64_type.into(),
                    ptr_type.into(),
                ],
                false,
            ),
            Some(Linkage::External),
        );
        //   `*mut u8 karac_arrow_dataframe_to_ipc(*const u8 df_ctrl,
        //    *mut i64 out_len)` — the N-field batch. Takes no element
        //    description: each stride-40 DataFrame entry already carries its
        //    own `elem_size` / `kind`.
        module.add_function(
            "karac_arrow_dataframe_to_ipc",
            ptr_type.fn_type(&[ptr_type.into(), ptr_type.into()], false),
            Some(Linkage::External),
        );
        //   `*mut u8 karac_arrow_column_from_ipc(*const u8 bytes, i64 len,
        //    i64 elem_size, i64 kind)` — the read direction: parse a stream and
        //    BUILD a Column control block at the declared element type (the
        //    element type lives in the Kāra type, not in the stream, so it has
        //    to cross). Returns NULL on a malformed stream / a non-converting
        //    element type — never a partial graph; codegen guards on null.
        module.add_function(
            "karac_arrow_column_from_ipc",
            ptr_type.fn_type(
                &[
                    ptr_type.into(),
                    i64_type.into(),
                    i64_type.into(),
                    i64_type.into(),
                ],
                false,
            ),
            Some(Linkage::External),
        );
        //   `*mut u8 karac_arrow_dataframe_from_ipc(*const u8 bytes, i64 len)`
        //    — builds the whole frame graph. No element description crosses:
        //    a `DataFrame` is not generic, so each column's representation is
        //    derived from its Arrow type. NULL on failure, as above.
        module.add_function(
            "karac_arrow_dataframe_from_ipc",
            ptr_type.fn_type(&[ptr_type.into(), i64_type.into()], false),
            Some(Linkage::External),
        );
        //   `*mut u8 karac_arrow_tensor_from_ipc(*const u8 bytes, i64 len,
        //    i64 elem_size, i64 kind, i64 want_rank, *const i64 want_dims)` —
        //    builds the `[rank][dims][data]` block. `want_dims` is the declared
        //    shape, one i64 per axis with -1 for a `?` axis, so the runtime can
        //    reject a mismatched stream BEFORE allocating. NULL on failure.
        module.add_function(
            "karac_arrow_tensor_from_ipc",
            ptr_type.fn_type(
                &[
                    ptr_type.into(),
                    i64_type.into(),
                    i64_type.into(),
                    i64_type.into(),
                    i64_type.into(),
                    ptr_type.into(),
                ],
                false,
            ),
            Some(Linkage::External),
        );
        //   `*mut u8 karac_arrow_tensor_to_ipc(*const u8 t_ptr, i64 elem_size,
        //    i64 kind, *mut i64 out_len)` — the `arrow.fixed_shape_tensor`
        //    extension. Rank and dims come from the tensor block's own header,
        //    so only the element description crosses the boundary.
        module.add_function(
            "karac_arrow_tensor_to_ipc",
            ptr_type.fn_type(
                &[
                    ptr_type.into(),
                    i64_type.into(),
                    i64_type.into(),
                    ptr_type.into(),
                ],
                false,
            ),
            Some(Linkage::External),
        );
        //   `*mut u8 karac_regex_replace_all(*const u8 pat, i64 pat_len,
        //    *const u8 s, i64 s_len, *const u8 repl, i64 repl_len,
        //    *mut i64 out_len)` — a malloc'd result buffer (caller owns as an
        //    owned `String`) + byte length; backs `replace_all`.
        module.add_function(
            "karac_regex_replace_all",
            ptr_type.fn_type(
                &[
                    ptr_type.into(),
                    i64_type.into(),
                    ptr_type.into(),
                    i64_type.into(),
                    ptr_type.into(),
                    i64_type.into(),
                    ptr_type.into(),
                ],
                false,
            ),
            Some(Linkage::External),
        );
        // LazyFrame runtime engine (phase-11 LazyDataFrame codegen twin,
        // `runtime/src/lazy.rs`). Handles are `Arc::into_raw` pointers:
        // every ARGUMENT position borrows (the runtime clones internally),
        // every constructor/builder returns a fresh +1 handle that codegen
        // stores in an alloca and releases once at the producing scope
        // (`ReleaseLazyExpr` / `ReleaseLazyPlan` cleanup actions), with
        // `_retain` bumped only on a value escaping through a user-fn
        // return. Lowered in `src/codegen/lazyframe.rs`.
        //   `*const ExprNode karac_lazy_expr_col(*const u8 name, i64 len)`
        module.add_function(
            "karac_lazy_expr_col",
            ptr_type.fn_type(&[ptr_type.into(), i64_type.into()], false),
            Some(Linkage::External),
        );
        //   `*const ExprNode karac_lazy_expr_lit_int(i64 v)`
        module.add_function(
            "karac_lazy_expr_lit_int",
            ptr_type.fn_type(&[i64_type.into()], false),
            Some(Linkage::External),
        );
        //   `*const ExprNode karac_lazy_expr_lit_float(f64 v)`
        module.add_function(
            "karac_lazy_expr_lit_float",
            ptr_type.fn_type(&[context.f64_type().into()], false),
            Some(Linkage::External),
        );
        //   `*const ExprNode karac_lazy_expr_lit_str(*const u8 s, i64 len)`
        module.add_function(
            "karac_lazy_expr_lit_str",
            ptr_type.fn_type(&[ptr_type.into(), i64_type.into()], false),
            Some(Linkage::External),
        );
        //   `*const ExprNode karac_lazy_expr_lit_bool(i8 v)`
        module.add_function(
            "karac_lazy_expr_lit_bool",
            ptr_type.fn_type(&[context.i8_type().into()], false),
            Some(Linkage::External),
        );
        //   `*const ExprNode karac_lazy_expr_cmp(i64 op, *const ExprNode lhs,
        //    *const ExprNode rhs)` — op: 0=gt 1=ge 2=lt 3=le 4=eq 5=ne.
        module.add_function(
            "karac_lazy_expr_cmp",
            ptr_type.fn_type(&[i64_type.into(), ptr_type.into(), ptr_type.into()], false),
            Some(Linkage::External),
        );
        //   `*const ExprNode karac_lazy_expr_bool(i64 op, *const ExprNode a,
        //    *const ExprNode b)` — op: 0=and_ 1=or_.
        module.add_function(
            "karac_lazy_expr_bool",
            ptr_type.fn_type(&[i64_type.into(), ptr_type.into(), ptr_type.into()], false),
            Some(Linkage::External),
        );
        //   `*const ExprNode karac_lazy_expr_not(*const ExprNode x)`
        module.add_function(
            "karac_lazy_expr_not",
            ptr_type.fn_type(&[ptr_type.into()], false),
            Some(Linkage::External),
        );
        //   `*const ExprNode karac_lazy_expr_arith(i64 op, *const ExprNode l,
        //    *const ExprNode r)` — op: 0=add 1=sub 2=mul 3=div.
        module.add_function(
            "karac_lazy_expr_arith",
            ptr_type.fn_type(&[i64_type.into(), ptr_type.into(), ptr_type.into()], false),
            Some(Linkage::External),
        );
        //   `void karac_lazy_expr_retain(*const ExprNode x)` /
        //   `void karac_lazy_expr_release(*const ExprNode x)`
        module.add_function(
            "karac_lazy_expr_retain",
            context.void_type().fn_type(&[ptr_type.into()], false),
            Some(Linkage::External),
        );
        module.add_function(
            "karac_lazy_expr_release",
            context.void_type().fn_type(&[ptr_type.into()], false),
            Some(Linkage::External),
        );
        //   `*const LazyPlan karac_lazy_new(*const u8 df_ctrl)` — BORROWS the
        //    DataFrame control block; deep-copies the frame.
        module.add_function(
            "karac_lazy_new",
            ptr_type.fn_type(&[ptr_type.into()], false),
            Some(Linkage::External),
        );
        //   `*const LazyPlan karac_lazy_select(*const LazyPlan plan,
        //    *const u8 elems, i64 count)` — elems is the `Vec[String]` DATA
        //    pointer (contiguous 24-byte {ptr,len,cap} String aggregates).
        module.add_function(
            "karac_lazy_select",
            ptr_type.fn_type(&[ptr_type.into(), ptr_type.into(), i64_type.into()], false),
            Some(Linkage::External),
        );
        //   `*const LazyPlan karac_lazy_limit(*const LazyPlan plan, i64 n)`
        module.add_function(
            "karac_lazy_limit",
            ptr_type.fn_type(&[ptr_type.into(), i64_type.into()], false),
            Some(Linkage::External),
        );
        //   `*const LazyPlan karac_lazy_filter(*const LazyPlan plan,
        //    *const ExprNode pred)`
        module.add_function(
            "karac_lazy_filter",
            ptr_type.fn_type(&[ptr_type.into(), ptr_type.into()], false),
            Some(Linkage::External),
        );
        //   `void karac_lazy_retain(*const LazyPlan plan)` /
        //   `void karac_lazy_release(*const LazyPlan plan)`
        module.add_function(
            "karac_lazy_retain",
            context.void_type().fn_type(&[ptr_type.into()], false),
            Some(Linkage::External),
        );
        module.add_function(
            "karac_lazy_release",
            context.void_type().fn_type(&[ptr_type.into()], false),
            Some(Linkage::External),
        );
        //   `*mut u8 karac_lazy_explain(*const LazyPlan plan, *mut i64
        //    out_len)` — a malloc'd UTF-8 buffer, always max(len, 1) bytes
        //    (the `karac_regex_replace_all` adoption convention).
        module.add_function(
            "karac_lazy_explain",
            ptr_type.fn_type(&[ptr_type.into(), ptr_type.into()], false),
            Some(Linkage::External),
        );
        //   `*mut u8 karac_lazy_collect(*const LazyPlan plan)` — a fresh
        //    malloc-compatible DataFrame control block, freeable by the
        //    ordinary FreeDataFrame path.
        module.add_function(
            "karac_lazy_collect",
            ptr_type.fn_type(&[ptr_type.into()], false),
            Some(Linkage::External),
        );
        //   `*const ExprNode karac_lazy_expr_desc(*const ExprNode x)` — the
        //    descending sort-key marker.
        module.add_function(
            "karac_lazy_expr_desc",
            ptr_type.fn_type(&[ptr_type.into()], false),
            Some(Linkage::External),
        );
        //   `*const ExprNode karac_lazy_expr_agg(i64 op, *const ExprNode arg)`
        //    — op: 0=count 1=sum 2=mean 3=min 4=max.
        module.add_function(
            "karac_lazy_expr_agg",
            ptr_type.fn_type(&[i64_type.into(), ptr_type.into()], false),
            Some(Linkage::External),
        );
        //   `*const ExprNode karac_lazy_expr_alias(*const ExprNode expr,
        //    *const u8 name, i64 name_len)` — output-name override.
        module.add_function(
            "karac_lazy_expr_alias",
            ptr_type.fn_type(&[ptr_type.into(), ptr_type.into(), i64_type.into()], false),
            Some(Linkage::External),
        );
        //   `*const LazyPlan karac_lazy_sort(*const LazyPlan plan,
        //    *const u8 keys, i64 count)` — keys is a `Vec[LazyExpr]` DATA
        //    pointer: packed 8-byte handle words (`{ handle_id: i64 }` POD).
        module.add_function(
            "karac_lazy_sort",
            ptr_type.fn_type(&[ptr_type.into(), ptr_type.into(), i64_type.into()], false),
            Some(Linkage::External),
        );
        //   `*const LazyGb karac_lazy_group_by(*const LazyPlan plan,
        //    *const u8 keys, i64 count)` — a NEW handle type (plan + pending
        //    keys), released via `karac_lazy_gb_release`.
        module.add_function(
            "karac_lazy_group_by",
            ptr_type.fn_type(&[ptr_type.into(), ptr_type.into(), i64_type.into()], false),
            Some(Linkage::External),
        );
        //   `*const LazyPlan karac_lazy_agg(*const LazyGb gb, *const u8 aggs,
        //    i64 count)` — completes the grouping into a GroupBy plan step.
        module.add_function(
            "karac_lazy_agg",
            ptr_type.fn_type(&[ptr_type.into(), ptr_type.into(), i64_type.into()], false),
            Some(Linkage::External),
        );
        //   `void karac_lazy_gb_retain(*const LazyGb gb)` /
        //   `void karac_lazy_gb_release(*const LazyGb gb)`
        module.add_function(
            "karac_lazy_gb_retain",
            context.void_type().fn_type(&[ptr_type.into()], false),
            Some(Linkage::External),
        );
        module.add_function(
            "karac_lazy_gb_release",
            context.void_type().fn_type(&[ptr_type.into()], false),
            Some(Linkage::External),
        );
        //   `*const LazyPlan karac_lazy_join(*const LazyPlan plan,
        //    *const LazyPlan other, *const u8 on, i64 count)` — other is a
        //    BORROWED plan handle (stored as the nested right sub-plan); on
        //    is a `Vec[String]` DATA pointer (24-byte String aggregates).
        module.add_function(
            "karac_lazy_join",
            ptr_type.fn_type(
                &[
                    ptr_type.into(),
                    ptr_type.into(),
                    ptr_type.into(),
                    i64_type.into(),
                ],
                false,
            ),
            Some(Linkage::External),
        );
        //   `*const LazyPlan karac_lazy_with_columns(*const LazyPlan plan,
        //    *const u8 exprs, i64 count)` — exprs is a `Vec[LazyExpr]` DATA
        //    pointer (packed handle words).
        module.add_function(
            "karac_lazy_with_columns",
            ptr_type.fn_type(&[ptr_type.into(), ptr_type.into(), i64_type.into()], false),
            Some(Linkage::External),
        );
        // Unicode `char` classification predicates (phase-12 #13): `char`
        // lowers to `i32`, so each takes the codepoint as `i32` and returns
        // `i8` (0/1). Backs `char.is_alphabetic()` / `is_numeric()` /
        // `is_alphanumeric()` / `is_whitespace()` / `is_uppercase()` /
        // `is_lowercase()` — the Unicode-aware companions of the inlined
        // `u8.is_ascii_*` byte predicates.
        let char_pred_type = context
            .i8_type()
            .fn_type(&[context.i32_type().into()], false);
        for name in [
            "karac_runtime_char_is_alphabetic",
            "karac_runtime_char_is_numeric",
            "karac_runtime_char_is_alphanumeric",
            "karac_runtime_char_is_whitespace",
            "karac_runtime_char_is_uppercase",
            "karac_runtime_char_is_lowercase",
        ] {
            module.add_function(name, char_pred_type, Some(Linkage::External));
        }
        // Unicode case folding (B-2026-08-12-25): `char.to_lowercase()` /
        // `to_uppercase()` — codepoint in, codepoint out (i32 → i32, unlike the
        // i8-returning predicates above). Needs the runtime's Unicode tables for
        // the same reason the predicates do; the inlined `to_ascii_*case`
        // arithmetic covers only `a`..`z` / `A`..`Z`.
        let char_fold_type = context
            .i32_type()
            .fn_type(&[context.i32_type().into()], false);
        for name in [
            "karac_runtime_char_to_lowercase",
            "karac_runtime_char_to_uppercase",
        ] {
            module.add_function(name, char_fold_type, Some(Linkage::External));
        }
        // `i64 karac_runtime_string_char_count(*const u8 ptr, i64 len)` — O(n)
        // Unicode scalar count, backing `s.char_count()`. And
        // `i8 karac_runtime_string_char_at(*const u8 ptr, i64 len, i64 idx,
        //  *mut u32 out_cp)` — writes the idx-th scalar through `out_cp` and
        // returns 1 in range / 0 past the end, backing `s.char_at(i)`'s
        // `Option[char]`. Both fetched by name in `compile_vec_method`.
        module.add_function(
            "karac_runtime_string_char_count",
            context
                .i64_type()
                .fn_type(&[ptr_type.into(), context.i64_type().into()], false),
            Some(Linkage::External),
        );
        module.add_function(
            "karac_runtime_string_char_at",
            context.i8_type().fn_type(
                &[
                    ptr_type.into(),
                    context.i64_type().into(),
                    context.i64_type().into(),
                    ptr_type.into(),
                ],
                false,
            ),
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
        // `karac_runtime_utf8_validate(data: *const u8, len: usize,
        //  out_err: *mut u8) -> bool`. Backs `CStr.to_string_slice() ->
        //  Result[StringSlice, Utf8Error]`: the zero-copy sibling of
        //  `cstr_to_string` — validates UTF-8 WITHOUT allocating, so codegen
        //  builds a borrowed `{ptr, len, cap=0}` view on Ok. Same Utf8Error
        //  discriminant on Err.
        let utf8_validate_type = context
            .bool_type()
            .fn_type(&[ptr_type.into(), i64_type.into(), ptr_type.into()], false);
        let _karac_runtime_utf8_validate_fn = module.add_function(
            "karac_runtime_utf8_validate",
            utf8_validate_type,
            Some(Linkage::External),
        );
        // `karac_runtime_string_to_cstring(data: *const u8, len: usize,
        //  out_cstr: *mut CString) -> bool`. Backs `String.to_cstring() ->
        //  Result[CString, NulError]` (design.md § C-String Literals): scans the
        //  bytes for an interior NUL; on none, allocates a `len + 1` buffer,
        //  copies the bytes + appends a NUL, writes the owning `{ptr, len,
        //  cap=len+1}` CString into `out_cstr`, and returns `true`; on an interior
        //  NUL returns `false` (codegen builds `Err(NulError.InteriorNul)`).
        let string_to_cstring_type = context
            .bool_type()
            .fn_type(&[ptr_type.into(), i64_type.into(), ptr_type.into()], false);
        let _karac_runtime_string_to_cstring_fn = module.add_function(
            "karac_runtime_string_to_cstring",
            string_to_cstring_type,
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
                i64_type.into(),          // raw_fd (i64 fd ABI — Windows IOCP prep)
                context.i8_type().into(), // direction (0=Read, 1=Write, 2=ReadWrite)
                ptr_type.into(),          // parked task pointer (opaque)
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
                i64_type.into(),          // raw_fd (i64 fd ABI — Windows IOCP prep)
                context.i8_type().into(), // direction
                ptr_type.into(),          // parked task pointer (opaque)
                ptr_type.into(),          // cancel: *const AtomicBool (null = none)
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
        // Return stays i32 (0/-1 status); raw_fd param widens to i64.
        let deregister_fd_ty = context
            .i32_type()
            .fn_type(&[i64_type.into(), i64_type.into()], false);
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
        // `karac_runtime_park_slot_store_result(slot: ptr, src: ptr, size: i64)`
        // / `_load_result(slot: ptr, dst: ptr, size: i64)` — carry a non-unit
        // coroutine's return value across the inline ramp+wait boundary
        // (B-2026-06-19). The coroutine body stores its `size`-byte return into
        // the slot at `coro_return` before signalling; the inline-drive caller
        // loads it back after `park_slot_wait` and before `park_slot_free`.
        let park_slot_result_ty = context
            .void_type()
            .fn_type(&[ptr_type.into(), ptr_type.into(), i64_type.into()], false);
        module.add_function(
            "karac_runtime_park_slot_store_result",
            park_slot_result_ty,
            Some(Linkage::External),
        );
        module.add_function(
            "karac_runtime_park_slot_load_result",
            park_slot_result_ty,
            Some(Linkage::External),
        );

        // ── stdlib TcpListener codegen-side wiring (Phase 6 line 17) ──────
        //
        // `karac_runtime_tcp_bind(addr_ptr: *const u8, addr_len: i64) -> i32`
        // — backs `TcpListener.bind(addr: String) -> TcpListener`. Returns
        // the listener fd; -1 on UTF-8 / parse / bind failure. Prints
        // `BOUND_PORT=<n>` to stdout when the requested address ends in
        // `:0` (ephemeral-port convention).
        // Returns the listener fd (or negative error code) — widened to i64.
        let tcp_bind_ty = i64_type.fn_type(&[ptr_type.into(), i64_type.into()], false);
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
        // Returns the connected socket fd (or negative error code) — i64.
        let tcp_connect_ty = i64_type.fn_type(&[ptr_type.into(), i64_type.into()], false);
        module.add_function(
            "karac_runtime_tcp_connect",
            tcp_connect_ty,
            Some(Linkage::External),
        );
        // Parked (non-blocking) connect pair — `connect_start(addr_ptr, len) ->
        // fd` begins a non-blocking connect; codegen parks on the fd's WRITE
        // readiness; `connect_finish(fd) -> fd` reads SO_ERROR (fd on success,
        // -1 closing the socket on failure). Backs the parked lowering of
        // `TcpStream.connect` so a coroutine handler's upstream connect SUSPENDS
        // instead of blocking the reactor thread.
        let tcp_connect_start_ty = i64_type.fn_type(&[ptr_type.into(), i64_type.into()], false);
        module.add_function(
            "karac_runtime_tcp_connect_start",
            tcp_connect_start_ty,
            Some(Linkage::External),
        );
        let tcp_connect_finish_ty = i64_type.fn_type(&[i64_type.into()], false);
        module.add_function(
            "karac_runtime_tcp_connect_finish",
            tcp_connect_finish_ty,
            Some(Linkage::External),
        );
        // `karac_runtime_tcp_try_clone(fd: i64) -> i64` — backs
        // `TcpStream.try_clone(ref self) -> Result[TcpStream, TcpError]`.
        // `dup(2)`s the socket fd so a connection can be split into two
        // owned handles (read-half + write-half) for a full-duplex splice.
        // Returns the new fd, or -1 on failure. No parking (a pure syscall,
        // like connect/bind).
        // `karac_runtime_tcp_shutdown(fd: i64, how: i64) -> i32` — backs
        // `TcpStream.shutdown_write(ref self) -> Result[Unit, TcpError]`
        // (called with how=1=Write). Half-closes a socket direction so a
        // proxy can propagate EOF across a full-duplex splice. 0 ok / -1 err.
        let tcp_shutdown_ty = i32_type.fn_type(&[i64_type.into(), i64_type.into()], false);
        module.add_function(
            "karac_runtime_tcp_shutdown",
            tcp_shutdown_ty,
            Some(Linkage::External),
        );
        let tcp_try_clone_ty = i64_type.fn_type(&[i64_type.into()], false);
        module.add_function(
            "karac_runtime_tcp_try_clone",
            tcp_try_clone_ty,
            Some(Linkage::External),
        );
        // `karac_runtime_tcp_accept(listener_fd: i32) -> i32` — backs
        // the *raw* accept(2) inside `TcpListener.accept`'s codegen
        // lowering. Caller (codegen) is responsible for parking via
        // `karac_park_on_fd` BEFORE invoking this — the FFI itself is
        // pure-syscall (no event-loop interaction). Returns the new
        // connection fd; -1 on failure (incl. EAGAIN, which signals
        // a missed-wakeup bug).
        // listener_fd param + connection-fd return both widen to i64.
        let tcp_accept_ty = i64_type.fn_type(&[i64_type.into()], false);
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
        // stream_fd param widens to i64; byte-count return already i64.
        let tcp_read_ty = context
            .i64_type()
            .fn_type(&[i64_type.into(), ptr_type.into(), i64_type.into()], false);
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
        // stream_fd param widens to i64; byte-count return already i64.
        let tcp_write_ty = context
            .i64_type()
            .fn_type(&[i64_type.into(), ptr_type.into(), i64_type.into()], false);
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
        // Return stays i32 (0/-1 status); fd param widens to i64.
        let tcp_close_ty = context.i32_type().fn_type(&[i64_type.into()], false);
        module.add_function(
            "karac_runtime_tcp_close",
            tcp_close_ty,
            Some(Linkage::External),
        );

        // Critical sections (design.md § Critical sections). `acquire()`
        // returns an i64 restore token (`compile_critical_section_acquire`);
        // the hand-rolled `@CriticalSectionGuard.drop` body
        // (`emit_critical_section_drop_body`) hands that token back to
        // `release`. On a hosted target the runtime maintains a balanced
        // nesting depth; on an MCU it masks/restores interrupts.
        let cs_acquire_ty = i64_type.fn_type(&[], false);
        module.add_function(
            "karac_critical_section_acquire",
            cs_acquire_ty,
            Some(Linkage::External),
        );
        let cs_release_ty = context.void_type().fn_type(&[i64_type.into()], false);
        module.add_function(
            "karac_critical_section_release",
            cs_release_ty,
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
        // Returns the listener fd (or negative error code) — widened to i64.
        let tls_listener_bind_ty =
            i64_type.fn_type(&[ptr_type.into(), i64_type.into(), ptr_type.into()], false);
        module.add_function(
            "karac_runtime_tls_listener_bind",
            tls_listener_bind_ty,
            Some(Linkage::External),
        );
        // `karac_runtime_tls_accept(listener_fd, config) -> i32` —
        // raw accept(2) + synchronous rustls handshake; registers a
        // `TlsSession` in the per-fd registry on success. Returns the
        // connection fd or -1.
        // listener_fd param + connection-fd return both widen to i64.
        let tls_accept_ty = i64_type.fn_type(&[i64_type.into(), ptr_type.into()], false);
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
        // Returns the connection fd (or -1) — widened to i64.
        let tls_client_connect_ty = i64_type.fn_type(
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
        // fd param widens to i64; byte-count return already i64.
        let tls_read_ty = context
            .i64_type()
            .fn_type(&[i64_type.into(), ptr_type.into(), i64_type.into()], false);
        module.add_function(
            "karac_runtime_tls_read",
            tls_read_ty,
            Some(Linkage::External),
        );
        // fd param widens to i64; byte-count return already i64.
        let tls_write_ty = context
            .i64_type()
            .fn_type(&[i64_type.into(), ptr_type.into(), i64_type.into()], false);
        module.add_function(
            "karac_runtime_tls_write",
            tls_write_ty,
            Some(Linkage::External),
        );
        // `karac_runtime_tls_close(fd) -> i32` — remove the session
        // entry from the per-fd registry and close the underlying TCP
        // fd. Same `-1` no-op shape as `karac_runtime_tcp_close`.
        // Return stays i32 (0/-1 status); fd param widens to i64.
        let tls_close_ty = context.i32_type().fn_type(&[i64_type.into()], false);
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

        // `karac_runtime_task_detach(handle: ptr)` — B-2026-06-17-2. Mark a
        // spawn handle detached (its call-site `TaskHandle` is discarded, never
        // bound/joined) so the runtime eager-reaps it instead of leaking: a
        // free-spawn handle self-reaps on completion, a `tg.spawn` child is
        // reaped by the group's register-time sweep. Emitted by
        // `lower_spawn_shared` when `pending_spawn_detach` is set.
        let task_detach_ty = context.void_type().fn_type(&[ptr_type.into()], false);
        module.add_function(
            "karac_runtime_task_detach",
            task_detach_ty,
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

        // `karac_runtime_channel_new_bounded(cap: i64) -> ptr` — the
        // capacity-bounded sibling backing `Channel.bounded(cap)`
        // (B-2026-08-22-16). Same refcount-2 contract; the bound makes a full
        // `send` apply backpressure instead of growing the queue.
        let channel_new_bounded_ty = ptr_type.fn_type(&[i64_type.into()], false);
        module.add_function(
            "karac_runtime_channel_new_bounded",
            channel_new_bounded_ty,
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

        // `karac_runtime_channel_set_elem_drop(ch: ptr, drop_fn: ptr)` — record
        // the element's `karac_drop_<T>` fn so the channel destructor frees any
        // heap payload SENT but never RECEIVED (B-2026-07-13-17). Emitted at
        // each heap-payload `send`; a scalar-payload channel never calls it.
        let channel_set_drop_ty = context
            .void_type()
            .fn_type(&[ptr_type.into(), ptr_type.into()], false);
        module.add_function(
            "karac_runtime_channel_set_elem_drop",
            channel_set_drop_ty,
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

        // `karac_runtime_channel_try_send(ch: ptr, val_ptr: ptr,
        // elem_size: u64) -> u8` — the NON-PANICKING send (B-2026-08-22-21).
        // Same shape as `send` but with a status return: 1 sent, 0 full,
        // 2 no live receiver. It reuses `channel_recv_ty` because the two
        // signatures coincide (three args, `u8` back) — the direction the
        // pointer is read in is the caller's business, not the type's.
        module.add_function(
            "karac_runtime_channel_try_send",
            channel_recv_ty,
            Some(Linkage::External),
        );

        // Write-once cell runtime (`runtime/src/once.rs`), backing
        // `OnceLock[T]` / `OnceCell[T]` (compiled into every archive — a cell
        // behind a lock has no scheduler dependency). The opaque
        // `*mut KaracOnce` handle is stored directly in the binding's slot.
        // `value_size` is threaded per `set` (type-erased, like the channel).
        //
        // `karac_runtime_once_new() -> ptr` — fresh empty cell.
        let once_new_ty = ptr_type.fn_type(&[], false);
        module.add_function(
            "karac_runtime_once_new",
            once_new_ty,
            Some(Linkage::External),
        );
        // `karac_runtime_once_set(cell, src_ptr, value_size) -> u8` — 1 = this
        // call sealed the cell, 0 = already set (caller keeps its value for the
        // `AlreadySetError` arm).
        let once_set_ty = context
            .i8_type()
            .fn_type(&[ptr_type.into(), ptr_type.into(), i64_type.into()], false);
        module.add_function(
            "karac_runtime_once_set",
            once_set_ty,
            Some(Linkage::External),
        );
        // `karac_runtime_once_get(cell) -> ptr` — stable borrow into the sealed
        // value, or null when unset. Codegen wraps non-null as `Some(ref T)`.
        let once_get_ty = ptr_type.fn_type(&[ptr_type.into()], false);
        module.add_function(
            "karac_runtime_once_get",
            once_get_ty,
            Some(Linkage::External),
        );
        // `karac_runtime_once_is_set(cell) -> u8`.
        let once_is_set_ty = context.i8_type().fn_type(&[ptr_type.into()], false);
        module.add_function(
            "karac_runtime_once_is_set",
            once_is_set_ty,
            Some(Linkage::External),
        );
        // `karac_runtime_once_free(cell)` — scope-exit free for a local
        // binding (`FreeOnceHandle`). Null is a no-op.
        let once_free_ty = context.void_type().fn_type(&[ptr_type.into()], false);
        module.add_function(
            "karac_runtime_once_free",
            once_free_ty,
            Some(Linkage::External),
        );

        // String-interner runtime (`runtime/src/interner.rs`), backing
        // `Symbol` + `Interner` (compiled into every archive — a byte-string
        // table behind a lock has no scheduler dependency). The opaque
        // `*mut KaracInterner` handle is stored directly in the binding's
        // slot; `Symbol` erases to a bare `i64` id.
        //
        // `karac_runtime_interner_new() -> ptr` — fresh empty interner.
        let interner_new_ty = ptr_type.fn_type(&[], false);
        module.add_function(
            "karac_runtime_interner_new",
            interner_new_ty,
            Some(Linkage::External),
        );
        // `karac_runtime_interner_intern(interner, bytes_ptr, len) -> i64` —
        // existing id on a dedup hit, else copies the bytes and mints the
        // next sequential id.
        let interner_intern_ty =
            i64_type.fn_type(&[ptr_type.into(), ptr_type.into(), i64_type.into()], false);
        module.add_function(
            "karac_runtime_interner_intern",
            interner_intern_ty,
            Some(Linkage::External),
        );
        // `karac_runtime_interner_resolve(interner, id, out_len) -> ptr` —
        // stable borrow into the interned bytes (length via out-param);
        // out-of-range degrades to the empty string. Codegen wraps the pair
        // as a `cap = 0` (never-freed) String view.
        let interner_resolve_ty =
            ptr_type.fn_type(&[ptr_type.into(), i64_type.into(), ptr_type.into()], false);
        module.add_function(
            "karac_runtime_interner_resolve",
            interner_resolve_ty,
            Some(Linkage::External),
        );
        // `karac_runtime_interner_len(interner) -> i64`.
        let interner_len_ty = i64_type.fn_type(&[ptr_type.into()], false);
        module.add_function(
            "karac_runtime_interner_len",
            interner_len_ty,
            Some(Linkage::External),
        );
        // `karac_runtime_interner_free(interner)` — scope-exit free for a
        // local binding (`FreeInternerHandle`). Null is a no-op.
        let interner_free_ty = context.void_type().fn_type(&[ptr_type.into()], false);
        module.add_function(
            "karac_runtime_interner_free",
            interner_free_ty,
            Some(Linkage::External),
        );

        // Arena runtime (`runtime/src/arena.rs`), backing `Arena[T]` +
        // `ArenaRef[T]` (compiled into every archive — a blob table behind a
        // lock has no scheduler dependency). The opaque `*mut KaracArena`
        // handle is stored directly in the binding's slot; `ArenaRef[T]`
        // erases to a bare `i64` index and `ArenaCheckpoint` to an `i64`
        // mark. Elements are raw byte blobs; codegen owns the per-`T`
        // interpretation (`src/codegen/arena.rs`).
        //
        // `karac_runtime_arena_new() -> ptr` — fresh empty arena.
        let arena_new_ty = ptr_type.fn_type(&[], false);
        module.add_function(
            "karac_runtime_arena_new",
            arena_new_ty,
            Some(Linkage::External),
        );
        // `karac_runtime_arena_push(arena, blob_ptr, len) -> i64` — copies
        // the blob, returns the dense index (the `ArenaRef`).
        let arena_push_ty =
            i64_type.fn_type(&[ptr_type.into(), ptr_type.into(), i64_type.into()], false);
        module.add_function(
            "karac_runtime_arena_push",
            arena_push_ty,
            Some(Linkage::External),
        );
        // `karac_runtime_arena_get(arena, idx, out_len) -> ptr` — stable
        // borrow into the stored blob (length via out-param); out-of-range
        // degrades to empty. Codegen wraps String elements as a `cap = 0`
        // (never-freed) String view.
        let arena_get_ty =
            ptr_type.fn_type(&[ptr_type.into(), i64_type.into(), ptr_type.into()], false);
        module.add_function(
            "karac_runtime_arena_get",
            arena_get_ty,
            Some(Linkage::External),
        );
        // `karac_runtime_arena_get_copy(arena, idx, dst, dst_len) -> i64` —
        // copy-out for by-value element kinds; zero-fills `dst` on degrade
        // so the subsequent load is always defined.
        let arena_get_copy_ty = i64_type.fn_type(
            &[
                ptr_type.into(),
                i64_type.into(),
                ptr_type.into(),
                i64_type.into(),
            ],
            false,
        );
        module.add_function(
            "karac_runtime_arena_get_copy",
            arena_get_copy_ty,
            Some(Linkage::External),
        );
        // `karac_runtime_arena_len(arena) -> i64` (also serves
        // `high_water_mark` — a checkpoint IS the current length).
        let arena_len_ty = i64_type.fn_type(&[ptr_type.into()], false);
        module.add_function(
            "karac_runtime_arena_len",
            arena_len_ty,
            Some(Linkage::External),
        );
        // `karac_runtime_arena_rewind(arena, mark)` — truncate to the
        // checkpoint mark (clamped by the runtime).
        let arena_rewind_ty = context
            .void_type()
            .fn_type(&[ptr_type.into(), i64_type.into()], false);
        module.add_function(
            "karac_runtime_arena_rewind",
            arena_rewind_ty,
            Some(Linkage::External),
        );
        // `karac_runtime_arena_free(arena)` — scope-exit free for a local
        // binding (`FreeArenaHandle`). Null is a no-op.
        let arena_free_ty = context.void_type().fn_type(&[ptr_type.into()], false);
        module.add_function(
            "karac_runtime_arena_free",
            arena_free_ty,
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

        // Backpressure primitives (`runtime/src/semaphore.rs`,
        // `runtime/src/rate_limiter.rs`), backing `Semaphore` / `RateLimiter`.
        // Same single-owner opaque-handle shape as bounded_channel: the
        // `*mut Karac{Semaphore,RateLimiter}` round-trips through the `i64
        // handle_id` field (ptrtoint at `new`, inttoptr at op/drop).
        //
        // Semaphore: `_new(permits: i64) -> ptr`; `_acquire(sem, timeout: i64)
        // -> u8` (1 = permit taken → `Ok(())`, 0 = exhausted → `Err(Timeout)`;
        // timeout ignored in v1); `_release(sem)`; `_drop(sem)`.
        let sem_new_ty = ptr_type.fn_type(&[i64_type.into()], false);
        module.add_function(
            "karac_runtime_semaphore_new",
            sem_new_ty,
            Some(Linkage::External),
        );
        let sem_acquire_ty = context
            .i8_type()
            .fn_type(&[ptr_type.into(), i64_type.into()], false);
        module.add_function(
            "karac_runtime_semaphore_acquire",
            sem_acquire_ty,
            Some(Linkage::External),
        );
        let sem_void_ptr_ty = context.void_type().fn_type(&[ptr_type.into()], false);
        module.add_function(
            "karac_runtime_semaphore_release",
            sem_void_ptr_ty,
            Some(Linkage::External),
        );
        module.add_function(
            "karac_runtime_semaphore_drop",
            sem_void_ptr_ty,
            Some(Linkage::External),
        );
        // RateLimiter: `_new(rate: i64, capacity: i64) -> ptr`;
        // `_try_acquire(rl, key_ptr, key_len: i64) -> u8` (1 = token taken →
        // `true`, 0 = limited → `false`); `_drop(rl)`.
        let rl_new_ty = ptr_type.fn_type(&[i64_type.into(), i64_type.into()], false);
        module.add_function(
            "karac_runtime_rate_limiter_new",
            rl_new_ty,
            Some(Linkage::External),
        );
        let rl_try_ty = context
            .i8_type()
            .fn_type(&[ptr_type.into(), ptr_type.into(), i64_type.into()], false);
        module.add_function(
            "karac_runtime_rate_limiter_try_acquire",
            rl_try_ty,
            Some(Linkage::External),
        );
        module.add_function(
            "karac_runtime_rate_limiter_drop",
            sem_void_ptr_ty,
            Some(Linkage::External),
        );

        // Connection pool (`runtime/src/pool.rs`). The `*mut KaracPool`
        // round-trips through the `Pool { handle_id: i64 }` field.
        // `_new(fn_ptr: i64, env_ptr: i64, elem_size: i64, max_conn: i64,
        // max_waiters: i64) -> ptr` stores the create-fn fat pointer + bounds;
        // `_begin_acquire(pool, out_val: ptr, out_conn_id: ptr, out_fn_ptr: ptr,
        // out_env_ptr: ptr) -> i32` decides idle/mint/at-cap/closed (codegen does
        // the mint's closure call); `_release(pool, conn_id: i64, val: ptr)`
        // returns a slot (idempotent on conn_id); `_drop(pool)` frees it.
        let pool_new_ty = ptr_type.fn_type(
            &[
                i64_type.into(),
                i64_type.into(),
                i64_type.into(),
                i64_type.into(),
                i64_type.into(),
            ],
            false,
        );
        module.add_function(
            "karac_runtime_pool_new",
            pool_new_ty,
            Some(Linkage::External),
        );
        let pool_begin_ty = context.i32_type().fn_type(
            &[
                ptr_type.into(),
                ptr_type.into(),
                ptr_type.into(),
                ptr_type.into(),
                ptr_type.into(),
            ],
            false,
        );
        module.add_function(
            "karac_runtime_pool_begin_acquire",
            pool_begin_ty,
            Some(Linkage::External),
        );
        let pool_release_ty = context
            .void_type()
            .fn_type(&[ptr_type.into(), i64_type.into(), ptr_type.into()], false);
        module.add_function(
            "karac_runtime_pool_release",
            pool_release_ty,
            Some(Linkage::External),
        );
        module.add_function(
            "karac_runtime_pool_drop",
            sem_void_ptr_ty,
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
        let ws_send_text_ty = context
            .i64_type()
            .fn_type(&[i64_type.into(), ptr_type.into(), i64_type.into()], false);
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
        let ws_recv_text_ty = context
            .i64_type()
            .fn_type(&[i64_type.into(), ptr_type.into(), i64_type.into()], false);
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
        // listener_fd param + connection-fd return both widen to i64.
        let ws_accept_ty = i64_type.fn_type(&[i64_type.into()], false);
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
        // listener_fd param + connection-fd return both widen to i64.
        let ws_accept_tls_ty = i64_type.fn_type(&[i64_type.into(), ptr_type.into()], false);
        module.add_function(
            "karac_runtime_ws_accept_tls",
            ws_accept_tls_ty,
            Some(Linkage::External),
        );

        // Phase 6 line 17 slice 9e.3 — binary frame FFIs. Same ABI
        // as the text framing FFIs from slice 9e.1 (3-arg `(fd,
        // ptr, len)` returning i64); the runtime helper switches
        // on the opcode bit (0x2 vs 0x1) internally.
        let ws_send_binary_ty = context
            .i64_type()
            .fn_type(&[i64_type.into(), ptr_type.into(), i64_type.into()], false);
        module.add_function(
            "karac_runtime_ws_send_binary",
            ws_send_binary_ty,
            Some(Linkage::External),
        );
        let ws_recv_binary_ty = context
            .i64_type()
            .fn_type(&[i64_type.into(), ptr_type.into(), i64_type.into()], false);
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
        let ws_send_text_masked_ty = context
            .i64_type()
            .fn_type(&[i64_type.into(), ptr_type.into(), i64_type.into()], false);
        module.add_function(
            "karac_runtime_ws_send_text_masked",
            ws_send_text_masked_ty,
            Some(Linkage::External),
        );
        let ws_send_binary_masked_ty = context
            .i64_type()
            .fn_type(&[i64_type.into(), ptr_type.into(), i64_type.into()], false);
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
        // B-2026-07-30-15 — exact-i64 sibling of make_number.
        let json_make_int_ty = ptr_type.fn_type(&[i64_type.into()], false);
        module.add_function(
            "karac_runtime_json_make_int",
            json_make_int_ty,
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

        // karac_map_free_with_val_drop_fn(map: ptr, drop_key: i32,
        // val_drop_fn: ptr) -> void — slice 3r (deferred gap (d)): runs a
        // synthesized `karac_drop_<T>(ptr)` on every live entry's VALUE blob
        // in place before deallocating the bucket storage. Selected when the
        // value type owns heap beyond the one-level `{ptr,len,cap}` overlay
        // (`Map[K, Holder]`, `Map[K, Map[J, W]]`, `Map[K, Vec[String]]`).
        // The key side keeps the flag contract (keys are Hash-constrained to
        // scalar / Vec/String shapes).
        let map_free_with_val_drop_fn_ty = context
            .void_type()
            .fn_type(&[ptr_md, i32_ty, ptr_md], false);
        let karac_map_free_with_val_drop_fn_fn = module.add_function(
            "karac_map_free_with_val_drop_fn",
            map_free_with_val_drop_fn_ty,
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

        // karac_map_try_insert(map, key, val, out_old_val, out_failed_bytes) -> i32
        // Fallible sibling of insert_old (Map/Set/SortedSet.try_insert, phase-8
        // item 8): 0 = fresh (Ok(None)), 1 = updated (Ok(Some(old))), 2 = OOM
        // (Err(AllocError.OutOfMemory{*out_failed_bytes}), map unchanged).
        let map_try_insert_ty = context
            .i32_type()
            .fn_type(&[ptr_md, ptr_md, ptr_md, ptr_md, ptr_md], false);
        let karac_map_try_insert_fn = module.add_function(
            "karac_map_try_insert",
            map_try_insert_ty,
            Some(Linkage::External),
        );

        // karac_map_get(map: ptr, key: ptr, out_val: ptr) -> i1
        let map_get_ty = context
            .bool_type()
            .fn_type(&[ptr_md, ptr_md, ptr_md], false);
        let karac_map_get_fn =
            module.add_function("karac_map_get", map_get_ty, Some(Linkage::External));

        // karac_map_remove_old(map: ptr, key: ptr, out_old_val: ptr, drop_key: i32) -> i1
        // The value is moved out via out_old_val (caller owns it), so only the
        // bucket's STORED key is freed; `drop_key` (nonzero = heap key) gates
        // that free — the tombstone would otherwise orphan it.
        let i32_md: BasicMetadataTypeEnum = context.i32_type().into();
        let map_remove_old_ty = context
            .bool_type()
            .fn_type(&[ptr_md, ptr_md, ptr_md, i32_md], false);
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

        // karac_map_clear_with_val_drop_fn(map, drop_key: i32, val_drop_fn: ptr)
        // — the clear sibling of `karac_map_free_with_val_drop_fn` (slice 3r).
        let karac_map_clear_with_val_drop_fn_fn = module.add_function(
            "karac_map_clear_with_val_drop_fn",
            map_free_with_val_drop_fn_ty,
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

        // karac_map_sorted_keys(map: ptr, out_len: ptr, cmp_fn: ptr) -> ptr
        // Returns a malloc'd, ascending-sorted copy of the map's keys (SortedSet
        // / SortedMap ordered iteration + min/max). Caller frees the buffer.
        let map_sorted_keys_ty = ptr_type.fn_type(&[ptr_md, ptr_md, ptr_md], false);
        let karac_map_sorted_keys_fn = module.add_function(
            "karac_map_sorted_keys",
            map_sorted_keys_ty,
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

        // Allocating String→String transforms (full Unicode, matching the
        // interpreter). `(data: ptr, len: i64, out_len: ptr) -> ptr`: returns a
        // fresh NUL-terminated buffer, writes the result byte length to `out_len`.
        let string_xform_ty = ptr_type.fn_type(&[ptr_md, i64_ty, ptr_md], false);
        // `karac_hash_bytes(ptr, len) -> u64` — SipHash-1-3 under the
        // per-process seed (B-2026-08-21-6). Every emitted per-type `hash_fn`
        // bottoms out here, so the compiled backends and the interpreter share
        // one implementation. In EVERY archive, not an opt-in one: every
        // `Map`/`Set` needs it.
        let hash_bytes_ty = i64_type.fn_type(&[ptr_md, i64_ty], false);
        module.add_function("karac_hash_bytes", hash_bytes_ty, Some(Linkage::External));
        // The `Map[K, V, FxBuildHasher]` sibling — unseeded FxHash. Declared
        // unconditionally like its default twin; LLVM drops the declaration
        // when nothing calls it, so a program that never names the selector
        // emits exactly what it emitted before the parameter existed.
        module.add_function(
            "karac_hash_bytes_fx",
            hash_bytes_ty,
            Some(Linkage::External),
        );

        // `String.normalize(form)` — the same shape plus an i32 form selector
        // (design.md § Strings, Equality; B-2026-08-20-41). Resolved from the
        // opt-in `libkarac_runtime_unicode.a`, which `karac` selects on any
        // `karac_unicode_*` reference (`driver.rs § SpecialArchive::Unicode`),
        // so the ICU normalization tables stay out of every other binary.
        //   `ptr karac_unicode_normalize(*const u8 data, i64 len, i32 form,
        //                                *mut i64 out_len)`
        let string_normalize_ty =
            ptr_type.fn_type(&[ptr_md, i64_ty, i32_type.into(), ptr_md], false);
        let karac_unicode_normalize_fn = module.add_function(
            "karac_unicode_normalize",
            string_normalize_ty,
            Some(Linkage::External),
        );
        let karac_string_to_lowercase_fn = module.add_function(
            "karac_string_to_lowercase",
            string_xform_ty,
            Some(Linkage::External),
        );
        let karac_string_to_uppercase_fn = module.add_function(
            "karac_string_to_uppercase",
            string_xform_ty,
            Some(Linkage::External),
        );
        let karac_string_trim_fn = module.add_function(
            "karac_string_trim",
            string_xform_ty,
            Some(Linkage::External),
        );
        let karac_string_trim_start_fn = module.add_function(
            "karac_string_trim_start",
            string_xform_ty,
            Some(Linkage::External),
        );
        let karac_string_trim_end_fn = module.add_function(
            "karac_string_trim_end",
            string_xform_ty,
            Some(Linkage::External),
        );
        let karac_string_sorted_fn = module.add_function(
            "karac_string_sorted",
            string_xform_ty,
            Some(Linkage::External),
        );
        // karac_string_replace(data, len, from, from_len, to, to_len, out_len) -> ptr
        let string_replace_ty = ptr_type.fn_type(
            &[ptr_md, i64_ty, ptr_md, i64_ty, ptr_md, i64_ty, ptr_md],
            false,
        );
        let karac_string_replace_fn = module.add_function(
            "karac_string_replace",
            string_replace_ty,
            Some(Linkage::External),
        );
        // karac_string_replacen(data, len, from, from_len, to, to_len, n, out_len) -> ptr
        let string_replacen_ty = ptr_type.fn_type(
            &[
                ptr_md, i64_ty, ptr_md, i64_ty, ptr_md, i64_ty, i64_ty, ptr_md,
            ],
            false,
        );
        let karac_string_replacen_fn = module.add_function(
            "karac_string_replacen",
            string_replacen_ty,
            Some(Linkage::External),
        );
        // karac_string_strip_{prefix,suffix}(data, len, p, p_len, out_len,
        // out_matched: *mut i32) -> ptr. Fetched by name at the use site
        // (vec_method.rs) — no cached field. `out_matched` distinguishes a
        // matched empty remainder (Some("")) from a no-match (None).
        // karac_string_join(parts: *const {ptr,len,cap}, count, sep, sep_len,
        // out_len) -> ptr — Vec[String].join/concat (B-2026-07-16-14). Walks
        // the vector's element triples read-only; fetched by name at the use
        // site (vec_method.rs) — no cached field.
        let string_join_ty = ptr_type.fn_type(&[ptr_md, i64_ty, ptr_md, i64_ty, ptr_md], false);
        module.add_function("karac_string_join", string_join_ty, Some(Linkage::External));
        let string_strip_ty =
            ptr_type.fn_type(&[ptr_md, i64_ty, ptr_md, i64_ty, ptr_md, ptr_md], false);
        module.add_function(
            "karac_string_strip_prefix",
            string_strip_ty,
            Some(Linkage::External),
        );
        module.add_function(
            "karac_string_strip_suffix",
            string_strip_ty,
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
            hash_hasher: crate::hasher_kind::HasherKind::default(),
            container_hashers: rustc_hash::FxHashMap::default(),
            user_hasher_states: rustc_hash::FxHashMap::default(),
            context,
            module,
            builder,
            variables: HashMap::new(),
            contract_state: ContractState {
                secret_type_is_stdlib: false,
                refinement_bases: HashMap::new(),
                refinement_generic_params: HashMap::new(),
                distinct_bases: HashMap::new(),
                refinement_predicates: HashMap::new(),
                current_contract_ensures: Vec::new(),
                current_contract_result_type: None,
                contract_old_snapshots: FxHashMap::default(),
                current_method_invariants: Vec::new(),
                constructor_invariant_self_type: None,
                strip_contracts: read_strip_contracts_env(),
                secret_inner_types: HashMap::new(),
            },
            var_types: VarTypes {
                var_type_names: HashMap::new(),
                tuple_var_elem_type_names: HashMap::new(),
                tuple_var_elem_type_exprs: HashMap::new(),
                int_const_locals: std::collections::HashMap::new(),
                vec_elem_types: HashMap::new(),
                pending_let_elem_type: None,
                pending_let_elem_type_expr: None,
                pending_let_tuple_te: None,
                slice_elem_types: HashMap::new(),
                binding_layouts: HashMap::new(),
                tuple_var_elem_tes: HashMap::new(),
                optres_var_payload_tes: HashMap::new(),
                var_elem_type_exprs: HashMap::new(),
                array_elem_type_exprs: HashMap::new(),
                closure_ret_vec_te: HashMap::new(),
                once_var_types: HashMap::new(),
                interner_vars: std::collections::HashSet::new(),
                arena_vars: HashMap::new(),
                arena_checkpoint_owner: HashMap::new(),
                var_option_payload_te: HashMap::new(),
                var_result_payload_te: HashMap::new(),
                string_vars: HashSet::new(),
                ascii_const_string_lets: HashMap::new(),
                cstr_vars: HashSet::new(),
                range_let_bindings: HashMap::new(),
            },
            payload_vars: PayloadVars {
                passthrough_owner_alias: std::collections::HashMap::new(),
                boxed_passthrough_owner_alias: std::collections::HashMap::new(),
                inline_option_payload_vars: std::collections::HashSet::new(),
                inline_result_payload_vars: std::collections::HashSet::new(),
                inline_option_map_payload_vars: std::collections::HashSet::new(),
                inline_option_agg_payload_vars: std::collections::HashSet::new(),
                boxed_enum_payload_vars: std::collections::HashSet::new(),
                boxed_moved_in_vars: std::collections::HashSet::new(),
                boxed_struct_payload_vars: std::collections::HashSet::new(),
                nested_boxed_payload_vars: std::collections::HashSet::new(),
                struct_field_boxed_payload_vars: std::collections::HashSet::new(),
                nested_boxed_passthrough_owner_alias: std::collections::HashMap::new(),
                plain_alias_bases: HashMap::new(),
                plain_alias_generic_params: HashMap::new(),
                shared_enum_payload_view_vars: std::collections::HashMap::new(),
                boxed_optres_payload_view_vars: HashMap::new(),
                deboxed_payload_box_ptrs: HashMap::new(),
                deferred_payload_box_ptrs: HashMap::new(),
                pending_box_field_zeroes: HashMap::new(),
                param_view_locals: HashSet::new(),
            },
            drop_rc: DropRc {
                inline_optres_retained_sources: std::collections::HashSet::new(),
                scope_cleanup_actions: Vec::new(),
                deep_copy_rc_inc_bare_shared: false,
                enum_drop_fns: HashMap::new(),
                struct_drop_fns: HashMap::new(),
                user_drop_wrapper_fns: HashMap::new(),
                rc_drop_fns: HashMap::new(),
                owned_temp_drops: HashMap::new(),
                rc_fallback_fns: HashMap::new(),
                rc_elide_ref_params: HashMap::new(),
                arc_fallback_fns: HashMap::new(),
                rc_fallback_heap_types: HashMap::new(),
                rc_fallback_box_drop_fns: Vec::new(),
                aggregate_drop_fns: Vec::new(),
                clone_fn_cache: HashMap::new(),
                try_clone_fn_cache: HashMap::new(),
                drop_fn_cache: HashMap::new(),
            },
            vec_elem_field_clone_slots: std::collections::HashMap::new(),
            vec_elem_field_clone_log: Vec::new(),
            in_return_defensive_copy: false,
            tracing: Tracing {
                strip_error_trace: read_strip_error_trace_env(),
                runtime_panic_prefix_needed: true,
                panic_site_counter: std::cell::Cell::new(0),
                current_span: None,
            },
            runtime_fns: RuntimeFns {
                karac_runtime_enter_predicate_fn,
                karac_runtime_exit_predicate_fn,
                karac_runtime_panic_prefix_fn,
                printf_fn,
                snprintf_fn,
                write_console_fn,
                write_console_line_fn,
                malloc_fn,
                alloc_fallible_fn,
                alloc_or_panic_fn,
                free_fn,
                free_buf_fn,
                exit_fn,
                memcmp_fn,
                sched_yield_fn,
                karac_par_run_fn,
                karac_par_run_auto_fn,
                karac_par_reduce_fn,
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
                karac_map_new_fn,
                karac_map_free_fn,
                karac_map_free_with_drop_vec_fn,
                karac_map_free_with_val_drop_fn_fn,
                karac_map_insert_old_fn,
                karac_map_try_insert_fn,
                karac_map_insert_borrowed_str_old_fn,
                karac_map_get_fn,
                karac_map_remove_old_fn,
                karac_map_contains_fn,
                karac_map_len_fn,
                karac_map_clear_fn,
                karac_map_clear_with_drop_vec_fn,
                karac_map_clear_with_val_drop_fn_fn,
                karac_map_iter_new_fn,
                karac_map_sorted_keys_fn,
                karac_map_iter_next_fn,
                karac_map_iter_free_fn,
                karac_string_decode_char_fn,
                karac_string_encode_char_fn,
                karac_map_entry_fn,
                karac_map_lookup_slot_fn,
                karac_string_clone_fn,
                karac_string_slice_fn,
                karac_string_slice_borrow_fn,
                karac_unicode_normalize_fn,
                karac_string_to_lowercase_fn,
                karac_string_to_uppercase_fn,
                karac_string_trim_fn,
                karac_string_trim_start_fn,
                karac_string_trim_end_fn,
                karac_string_sorted_fn,
                karac_string_replace_fn,
                karac_string_replacen_fn,
                karac_error_trace_push_fn,
                karac_error_trace_clear_fn,
                karac_test_record_failure_fn,
            },
            atomic_var_inner_is_bool: HashSet::new(),
            current_fn: None,
            stdout_global,
            stderr_global,
            type_decls: TypeDecls {
                struct_types: HashMap::new(),
                shared_type_names: std::collections::HashSet::new(),
                declared_generic_param_names: std::collections::HashSet::new(),
                struct_field_names: HashMap::new(),
                struct_field_type_names: HashMap::new(),
                struct_field_type_exprs: HashMap::new(),
                ord_orderable_types: std::collections::HashSet::new(),
                struct_generic_params: HashMap::new(),
                shared_type_decl_names: std::collections::HashSet::new(),
                union_types: HashMap::new(),
                union_field_types: HashMap::new(),
                enum_layouts: HashMap::new(),
                enum_unit_variants: HashMap::new(),
                enum_discriminants: crate::ast::EnumDiscriminantTable::default(),
                seeded_enum_names: HashSet::new(),
                shared_types: HashMap::new(),
                enum_inst_type_exprs: HashMap::new(),
                enum_inst_var_types: HashMap::new(),
                struct_moved_field_bodies: HashMap::new(),
                user_shadowed_prelude_types: std::collections::HashSet::new(),
            },
            assoc_type_bindings: HashMap::new(),
            conc: ConcState {
                state_struct_types: HashMap::new(),
                state_machine_poll_fns: HashMap::new(),
                state_machine_state_constructors: HashMap::new(),
                state_machine_state_destructors: HashMap::new(),
                state_machine_return_types: HashMap::new(),
                pending_spawn_detach: false,
                channel_elem_types: HashMap::new(),
                task_join_return_types: HashMap::new(),
                branch_cancel_ptr: None,
                par_capture_modes: FxHashMap::default(),
                concurrency_decisions: HashMap::new(),
                par_counter: 0,
                karac_branch_ty,
                spawn_sites: Vec::new(),
                auto_par_disabled: !read_auto_par_env() || crate::target::active_target_is_wasm(),
                hot_swap_enabled: false,
                hot_swap_slots: HashMap::new(),
                hot_swap_fns: Vec::new(),
                coro_enabled: false,
                coro_fn_keys: HashSet::new(),
                coro_ctx: None,
                coro_park_counter: 0,
                coro_spawn_slot: None,
            },
            program_snapshot: None,
            cmp_fn_in_progress: std::collections::HashSet::new(),
            display: Display {
                baked_display_enum_names: HashSet::new(),
                display_option_result_types: HashMap::new(),
                display_tuple_types: HashMap::new(),
                display_generic_enum_types: HashMap::new(),
                display_vec_types: HashMap::new(),
                display_map_types: HashMap::new(),
                display_set_types: HashMap::new(),
                display_sorted_collection_spans: std::collections::HashSet::new(),
                display_fn_cache: HashMap::new(),
            },
            fn_ctx: FnCtx {
                loop_stack: Vec::new(),
                current_fn_heap_closure_spans: std::collections::HashSet::new(),
                current_fn_param_names: HashSet::new(),
                tail_ret_inner: None,
                return_retargets: Vec::new(),
                current_fn_err_payload_ty: None,
                current_fn_returns_ref: false,
                current_fn_boxes_return: false,
                current_fn_name: String::new(),
                current_fn_caller_loc: None,
                current_fn_arm64_return_coercion: None,
                current_fn_sret_param: None,
            },
            mono_state: MonoState {
                generic_fns: HashMap::new(),
                generated_monos: HashSet::new(),
                type_subst: HashMap::new(),
                type_subst_names: HashMap::new(),
                type_subst_type_exprs: HashMap::new(),
                const_subst: HashMap::new(),
                layout_subst: HashMap::new(),
                mono_handle_param_infos: HashMap::new(),
                mono_payload_binding_type_exprs: HashMap::new(),
            },
            fn_sig: FnSig {
                fn_asts: HashMap::new(),
                fn_param_slice_elem: HashMap::new(),
                fn_param_ref: HashMap::new(),
                fn_param_mut_ref: HashMap::new(),
                fn_param_tensor_info: HashMap::new(),
                extern_link_names: HashMap::new(),
                fn_return_type_names: HashMap::new(),
                fn_return_type_exprs: HashMap::new(),
                fn_ref_return_inner: HashMap::new(),
                fn_return_option_inner_shared: HashMap::new(),
                fn_body_ident_mention_offsets: HashMap::new(),
                callee_effectful: HashMap::new(),
                heuristic_inline_hints: std::collections::HashMap::new(),
                track_caller_fns: std::collections::HashSet::new(),
                declare_only_fns: std::collections::HashSet::new(),
            },
            return_layout: LayoutId::Aos,
            pending_return_layout: None,
            closure_state: ClosureState {
                closure_counter: 0,
                closure_fn_types: HashMap::new(),
                escape: crate::closure_escape::EscapeAnalysis::default(),
                heap_env_closure_vars: std::collections::HashSet::new(),
                heap_env_owner_fields: std::collections::HashMap::new(),
                pending_closure_fn_type: None,
                pending_closure_param_hints: None,
                closure_capture_paths: FxHashMap::default(),
            },
            indexed_elem_counter: 0,
            optional_chain_counter: 0,
            pending_reverse_iter: false,
            result_shared_nonescaping_let_spans: std::collections::HashSet::new(),
            result_shared_nonescaping_param_names: std::collections::HashSet::new(),
            mapset: MapSet {
                pending_map_insert_old_dec: false,
                map_tag_override: match std::env::var("KARAC_MAP_TAG").as_deref() {
                    Ok("0") => Some(false),
                    Ok("1") => Some(true),
                    _ => None,
                },
                map_lookup_probe: match std::env::var("KARAC_MAP_PROBE").as_deref() {
                    Ok("unbounded") => mono::MapLookupProbe::Unbounded,
                    Ok("slotwalk") => mono::MapLookupProbe::SlotWalk,
                    _ => mono::MapLookupProbe::Bounded,
                },
                temp_recv_mapset_types: HashMap::new(),
                map_val_bodies_tes: HashMap::new(),
                deque_head_locals: HashMap::new(),
                deque_head_slots: HashMap::new(),
                map_key_types: HashMap::new(),
                map_val_types: HashMap::new(),
                map_key_type_names: HashMap::new(),
                map_key_type_exprs: HashMap::new(),
                map_hashers: HashMap::new(),
                set_elem_types: HashMap::new(),
                sorted_collection_vars: std::collections::HashSet::new(),
                set_elem_type_names: HashMap::new(),
                set_elem_type_exprs: HashMap::new(),
                map_mono_methods: HashMap::new(),
            },
            last_fstr_acc: None,
            block_tail_shared_transfer: false,
            freshtemp_field_access_slot: None,
            bce: BceState {
                len_alias: HashMap::new(),
                asserted_index_bounds: Vec::new(),
                pending_vec_len_pins: FxHashMap::default(),
                descending_skips: FxHashMap::default(),
                interproc_conv_skips: HashMap::new(),
                converging_skips: FxHashMap::default(),
                vec_len_pins: Vec::new(),
                binsearch_guard_stack: Vec::new(),
                binsearch_assume_emitted: false,
                check_free_accum_sites: std::collections::HashSet::new(),
                elide_next_add_overflow_check: false,
                elide_proven_index_add_overflow: std::env::var("KARAC_BCE_OVF_SKIP").as_deref()
                    != Ok("0"),
            },
            slice_alias_md: HashMap::new(),
            borrow_vars: BorrowVars {
                ref_params: HashMap::new(),
                signature_ref_params: std::collections::HashSet::new(),
                entry_slot_ref_vars: HashMap::new(),
                owned_vecstr_params: HashSet::new(),
                for_loop_borrow_vars: HashSet::new(),
                borrow_accessor_let_payload: std::collections::HashMap::new(),
                for_loop_owned_agg_vars: HashSet::new(),
                borrowed_agg_payload_struct_vars: HashSet::new(),
                owned_struct_params: HashSet::new(),
                var_option_shared_heap: HashMap::new(),
                ref_option_shared_heap: HashMap::new(),
            },
            enumerate_index_pattern: None,
            target_abi: TargetAbi {
                fn_niche_abi: HashMap::new(),
                boxed_export_names: std::collections::HashSet::new(),
                headerless_fns: HashMap::new(),
                headerless_type_candidates: HashMap::new(),
                headerless_reshaper_dummies: HashMap::new(),
                headerless_types: HashSet::new(),
                target_is_aarch64: !crate::target::active_target_is_wasm()
                    && driver::native_target_is_aarch64(),
                target_is_x86_64: !crate::target::active_target_is_wasm()
                    && driver::native_target_is_x86_64(),
                target_is_windows_x86_64: !crate::target::active_target_is_wasm()
                    && driver::native_target_is_windows_x86_64(),
                arm64_coerced_struct_params: HashMap::new(),
                indirect_struct_params: HashMap::new(),
                abi_adapted_export_names: std::collections::HashSet::new(),
                arm64_coerced_struct_returns: HashMap::new(),
                sret_struct_returns: HashMap::new(),
            },
            pattern_state: PatternState {
                discarded_branch_spans: FxHashSet::default(),
                pattern_binding_is_borrow: false,
                pattern_binding_source_retains_inline_payload: false,
                pattern_binding_scrutinee_is_elidable_param: false,
                pattern_binding_scrutinee_is_option_result: false,
                pattern_binding_scrutinee_is_fresh_owning_temp: false,
                pattern_binding_arm_only_borrows: false,
                pattern_binding_scrutinee_is_owned_param: false,
                pattern_binding_scrutinee_payload_bodies_src: None,
                pattern_binding_scrutinee_optres_slot: None,
                pattern_binding_scrutinee_optres_area: 0,
                pattern_binding_scrutinee_is_shared_enum: false,
                match_scrutinee_enum_hint: None,
                pattern_binding_types: HashMap::new(),
                pattern_binding_inner_types: HashMap::new(),
                pattern_binding_borrow_modes: HashMap::new(),
                current_variant_payload_bindings: HashSet::new(),
            },
            accel: Accel {
                soa_layouts: HashMap::new(),
                soa_return_locals: std::collections::HashSet::new(),
                soa_drop_fns: HashMap::new(),
                gpu_dispatch_wgsl: HashMap::new(),
                gpu_resident_field: HashMap::new(),
                gpu_reduce_int_elems: HashMap::new(),
                tensor_typed_exprs: HashMap::new(),
                tensor_index_recv_types: HashMap::new(),
                tensor_var_infos: HashMap::new(),
                pending_let_tensor_info: None,
                column_typed_exprs: HashMap::new(),
                column_var_infos: HashMap::new(),
                pending_let_column_info: None,
                dataframe_var_infos: std::collections::HashSet::new(),
                gpu_buffer_elem_structs: HashMap::new(),
                gpu_buffer_vars: HashSet::new(),
            },
            iter_body_retarget_spans: std::collections::HashSet::new(),
            pending_errdefer_payload: None,
            main_result_err_te: None,
            main_returns_exitcode: false,
            boxed_enum_export_names: std::collections::HashSet::new(),
            compiling_ref_return_let_rhs: false,
            suppress_shadow_metadata_purge: false,
            copy_support_for_loop_shared_mode: false,
            span_tables: SpanTables {
                question_conversions: HashMap::new(),
                question_ok_payload_types: HashMap::new(),
                wp_result_types: HashMap::new(),
                method_callee_types: HashMap::new(),
                impl_dispatch_names: crate::impl_dispatch::ImplDispatchNames::default(),
                method_impl_dispatch: HashMap::new(),
                call_effect_subs: crate::ast::CallEffectSubsTable::new(),
                method_unwrap_inner_types: HashMap::new(),
                optional_chain_lowering: HashMap::new(),
                method_unwrap_err_types: HashMap::new(),
                temp_recv_elem_types: HashMap::new(),
                temp_recv_len_elem_types: HashMap::new(),
                iter_terminal_elem_types: HashMap::new(),
                iter_terminal_acc_types: HashMap::new(),
                stats_elem_types: HashMap::new(),
                string_typed_exprs: HashSet::new(),
                borrow_vec_typed_exprs: HashSet::new(),
                iterator_typed_exprs: HashSet::new(),
                fn_value_typed_exprs: HashMap::new(),
                call_type_subs: HashMap::new(),
                call_type_subs_mangle: HashMap::new(),
                index_recv_vec_types: HashMap::new(),
                unsigned_vector_exprs: HashSet::new(),
                unsigned_int_exprs: HashSet::new(),
                cast_source_unsigned: HashSet::new(),
                vector_method_call_spans: HashSet::new(),
                expr_struct_type_names: HashMap::new(),
                user_ord_typed_exprs: HashMap::new(),
                raw_pointer_pointee_types: HashMap::new(),
                concrete_named_type_exprs: HashMap::new(),
                vec_index_borrow_spans: FxHashSet::default(),
                vec_index_cloned_sites: FxHashSet::default(),
                uam_consume_sites: std::collections::HashSet::new(),
                uam_copied_sites: std::collections::HashSet::new(),
            },
            iter_let_bindings: HashMap::new(),
            ref_return_inner_types: HashMap::new(),
            user_ref_method_names: std::collections::HashSet::new(),
            user_ref_method_inner: std::collections::HashMap::new(),
            tuple_moved_elem_bodies: HashMap::new(),
            mod_bindings: ModBindings {
                consts: HashMap::new(),
                module_bindings: HashMap::new(),
                map_set_module_inits: Vec::new(),
                once_module_inits: Vec::new(),
                computed_module_inits: Vec::new(),
                module_binding_types: std::collections::HashMap::new(),
            },
            static_init_fn: None,
            source_filename: None,
            source_filename_global: None,
            source_text: None,
            used_symbols: Vec::new(),
            used_data_globals: Vec::new(),
            rc_elision: RcElision {
                elided_bindings: HashMap::new(),
                elided_cluster_roots: HashMap::new(),
                elided_b2_bindings: HashMap::new(),
                adopted_cluster_roots: HashMap::new(),
                weak_targeted_types: HashSet::new(),
                conditional_adopted_roots: HashMap::new(),
                borrowed_param_skips: HashMap::new(),
            },
            atomic_promoted_types: HashSet::new(),
            frozen_alias_bindings: FxHashSet::default(),
            frozen_element_containers: FxHashSet::default(),
            frozen_elem_vec_owners: HashSet::new(),
            debug_info: None,
            runtime_debug_metadata_enabled: read_runtime_debug_metadata_env(),
            // Env gate OR wasm target — see the field doc-comment
            // (auto-par fan-out is pure overhead on a single-threaded
            // target; phase-10 sequential default).
            tabulate_alias_scopes: None,
            provider_state: ProviderState {
                provider_resource_ids: HashMap::new(),
                provider_resource_traits: HashMap::new(),
                provider_trait_methods: HashMap::new(),
                user_ambient_resource_methods: HashMap::new(),
                provider_vtables: HashMap::new(),
                provider_frame_ty,
            },
            declaring_stdlib_program: false,
            http_shim_cache: HashMap::new(),
            target_data: init_target_data,
            main_symbol_override: None,
            force_external_linkage: false,
            snapshot_capture: HashMap::new(),
            snapshot_replay: HashMap::new(),
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
            self.drop_rc.rc_fallback_fns.insert(fn_name.clone(), names);
        }
        for (fn_name, arc_set) in &ow.arc_values {
            self.drop_rc
                .arc_fallback_fns
                .insert(fn_name.clone(), arc_set.clone());
        }
        // B-2026-08-01-33 mechanism 2 — types promoted to atomic RC so a
        // multi-branch `par {}` capture could be admitted.
        self.atomic_promoted_types
            .extend(ow.atomic_promoted_types.iter().cloned());
        // B-2026-08-01-33 mechanism 3, stage 2.5 — `let` bindings admitted as
        // non-counting aliases of a frozen place. Consumed by the let-stmt
        // shared arm; empty unless the program uses the `frozen` mode.
        self.frozen_alias_bindings
            .extend(ow.frozen_alias_bindings.iter().cloned());
        // B-2026-08-01-33 mechanism 3, stage 3c — the frozen-element container
        // `let`s. See the field's doc for why its two consumers are a pair.
        self.frozen_element_containers
            .extend(ow.frozen_element_containers.iter().cloned());
        // RC-elide-ref (env `KARAC_RC_ELIDE_REF_PARAMS`): consume the ownership
        // pass's *sound* elidability set — `Ref` params that no call site
        // passes a fresh rvalue and whose function never escapes as a value
        // (`crate::rc_elide::safe_elidable_ref_params`). `borrowed_arg_skip` /
        // `borrowed_param_dec_skip` then treat each as a pure balanced borrow
        // (no caller arg inc, no callee exit dec). Empty unless the flag is set
        // — the ownership pass gates the walk — so nothing changes by default.
        // B-2026-08-10-21 — the `UseAfterMove` defensive-copy sites.
        self.span_tables
            .uam_consume_sites
            .extend(ow.use_after_move_consume_sites.iter().copied());
        for (fn_name, recs) in &ow.elidable_ref_params {
            self.drop_rc
                .rc_elide_ref_params
                .insert(fn_name.clone(), recs.clone());
        }
        // RC elision phase A: per-fn elided-binding sets. Consulted by
        // the let-stmt shared arm via `is_elided_binding`.
        for (fn_name, names) in &ow.elided_bindings {
            self.rc_elision
                .elided_bindings
                .insert(fn_name.clone(), names.clone());
        }
        // RC elision phase B1: cluster roots → free-walk cleanup.
        // Phase B2: role records for displacement-free clusters.
        for (fn_name, clusters) in &ow.elided_clusters {
            let entry = self
                .rc_elision
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
                        &mut self.rc_elision.conditional_adopted_roots
                    } else {
                        &mut self.rc_elision.adopted_cluster_roots
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
                        .rc_elision
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
                let b2_entry = self
                    .rc_elision
                    .elided_b2_bindings
                    .entry(fn_name.clone())
                    .or_default();
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
                    self.target_abi
                        .headerless_fns
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
            self.closure_state
                .closure_capture_paths
                .insert(*k, v.clone());
        }
        // L227: per-par-block capture modes. Drives the `SharedRc` arm
        // in `emit_par_branch_fn`'s capture-unpack loop (atomic rc_inc
        // in the branch prologue + `track_rc_var` registration). Names
        // absent from the inner Vec fall through to `Copy` semantics
        // (today's behavior).
        for (k, v) in &ow.par_capture_modes {
            self.conc.par_capture_modes.insert(*k, v.clone());
        }
        // Phase C2b: headerless-T candidates (reconciled in
        // `compile_program` once coro keys + struct layouts exist).
        for (t, v) in &ow.headerless_types {
            self.target_abi
                .headerless_type_candidates
                .insert(t.clone(), v.clone());
        }
        // Headerless reshaper fns → dummy sentinel binding (single-node
        // free at scope exit).
        for (fn_key, dummy) in &ow.headerless_reshaper_dummies {
            self.target_abi
                .headerless_reshaper_dummies
                .insert(fn_key.clone(), dummy.clone());
        }
    }

    /// Load the head-index deque eligibility set (`crate::deque_head`).
    ///
    /// Computed from the AST, plain data — no LLVM type crosses the boundary,
    /// so codegen containment holds (CLAUDE.md § Codegen architecture).
    fn load_deque_head_locals(&mut self, program: &crate::ast::Program) {
        self.mapset.deque_head_locals = crate::deque_head::eligible_deque_locals(program);
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
        self.conc.hot_swap_enabled = enabled;
    }

    /// Override the contract-stripping decision (design.md § Contracts:
    /// "stripped in release"). `true` elides all `requires` / `ensures` /
    /// `old(...)` / `invariant` emission; `false` keeps them. The default
    /// comes from `KARAC_STRIP_CONTRACTS` at construction; this setter lets
    /// the release-build path and IR tests force the decision without relying
    /// on the process-global env var.
    pub(crate) fn set_strip_contracts(&mut self, strip: bool) {
        self.contract_state.strip_contracts = strip;
    }

    /// Override the `?`-error-return-trace stripping decision (peer to
    /// `set_strip_contracts`). `true` elides the `karac_error_trace_push` /
    /// `_clear` instrumentation; `false` keeps it. Default from
    /// `KARAC_STRIP_ERROR_TRACE`; the `release` build path forces it on.
    pub(crate) fn set_strip_error_trace(&mut self, strip: bool) {
        self.tracing.strip_error_trace = strip;
    }

    /// Enable the A2 slice 2b.3 coroutine compilation path (default off). When
    /// set before `compile_program`, network-boundary functions compile as LLVM
    /// coroutines with the dispatcher-driven slot-wait drive instead of the
    /// degenerate `emit_state_machine_poll_fn_for_key` body-splitter. Race-free
    /// (no process-global env), mirroring `set_strip_contracts`. See
    /// docs/spikes/network-async-coroutine-transform.md § 6¾.
    pub(crate) fn set_coro_enabled(&mut self, enabled: bool) {
        self.conc.coro_enabled = enabled;
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
        self.conc.auto_par_disabled =
            !read_auto_par_env() || (crate::target::active_target_is_wasm() && !threaded);
    }

    /// Whether `fn_key` is compiled as a coroutine this run (A2 slice 2b.3) —
    /// i.e. `coro_enabled` and the key is a non-generic network-boundary
    /// function (`coro_fn_keys`, populated in `compile_program`). The single
    /// predicate behind the three coupled coroutine toggles: `ptr` return type
    /// in `declare_function`, poll-fn skip in `emit_state_machine_poll_fns`, and
    /// the slot-wait call-site drive.
    pub(crate) fn is_coroutine_compiled(&self, fn_key: &str) -> bool {
        self.conc.coro_enabled && self.conc.coro_fn_keys.contains(fn_key)
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
        let id = self.conc.par_counter;
        self.conc.par_counter += 1;
        let (line, col) = match self.source_text.as_deref() {
            Some(src) => {
                let (l, c) = crate::byte_offset_to_line_col(src, span.offset);
                (l as u32, c as u32)
            }
            None => (span.line as u32, span.column as u32),
        };
        let file = self.source_filename.clone().unwrap_or_default();
        self.conc.spawn_sites.push(SpawnSiteRecord {
            id,
            file,
            line,
            col,
            worker_count,
        });
        id
    }

    fn is_rc_fallback_binding(&self, name: &str) -> bool {
        self.drop_rc
            .rc_fallback_fns
            .get(&self.fn_ctx.current_fn_name)
            .is_some_and(|set| set.contains(name))
    }

    /// True iff `name` is an RC-elided shared binding in the current
    /// function (ownership phase-A elision). The let-site routes these
    /// to `track_elided_shared_var` (unconditional free) instead of
    /// `track_rc_var`. Nested-fn compiles (closures, par branches)
    /// never see elided names — the analysis blocks any candidate
    /// mentioned inside those regions.
    fn is_elided_binding(&self, name: &str) -> bool {
        self.rc_elision
            .elided_bindings
            .get(&self.fn_ctx.current_fn_name)
            .is_some_and(|set| set.contains(name))
    }

    /// True iff `name` is a deque local of the current function that the
    /// eligibility analysis cleared for the head-index lowering AND whose head
    /// alloca has been materialized. Both halves matter: the analysis runs over
    /// top-level functions only, so a closure or par-branch compile can carry
    /// the same binding name without an entry in `deque_head_slots`, and the
    /// method arms must fall back to the memmove lowering there.
    pub(super) fn is_head_index_deque(&self, name: &str) -> bool {
        // The slot must live in the function whose body is being emitted
        // RIGHT NOW (B-2026-07-31-35). The name-keyed table plus
        // `current_fn_name` is not enough: out-of-line emitters — par-branch
        // functions, closures, reduction/disjoint-write workers, sort
        // comparators — compile statements into a different LLVM function
        // without rebinding `current_fn_name`, and a head-aware method arm
        // emitted there would reference an alloca from another function
        // ("Instruction does not dominate all uses"). Comparing the alloca's
        // parent function against `current_fn` makes every such lane fall
        // back to the memmove lowering structurally, whatever emitter it is.
        self.mapset.deque_head_slots.get(name).is_some_and(|slot| {
            let parent_fn = slot
                .as_instruction_value()
                .and_then(|inst| inst.get_parent())
                .and_then(|bb| bb.get_parent());
            parent_fn.is_some() && parent_fn == self.current_fn
        }) && self
            .mapset
            .deque_head_locals
            .get(&self.fn_ctx.current_fn_name)
            .is_some_and(|set| set.contains(name))
    }

    /// The `head` alloca for an eligible deque local, or `None` when the
    /// binding is not on the head-index path.
    pub(super) fn deque_head_slot(&self, name: &str) -> Option<PointerValue<'ctx>> {
        if self.is_head_index_deque(name) {
            self.mapset.deque_head_slots.get(name).copied()
        } else {
            None
        }
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
        self.rc_elision
            .elided_cluster_roots
            .get(&self.fn_ctx.current_fn_name)
            .and_then(|m| m.get(name))
            .cloned()
    }

    /// Phase C1c adopted-root lookup for the current function:
    /// `(member type, link user-field index)` when `name` is an
    /// adopted cluster root (an `Option[shared T]` builder-call result
    /// whose scope-exit cleanup is the option-guarded free-walk).
    fn adopted_root_info(&self, name: &str) -> Option<(String, usize)> {
        if let Some(info) = self
            .rc_elision
            .adopted_cluster_roots
            .get(&self.fn_ctx.current_fn_name)
            .and_then(|m| m.get(name))
        {
            return Some(info.clone());
        }
        // C2b conditional families (sanctioned-arg users): active only
        // under the reconciled headerless set — otherwise the binding
        // falls back to full RC and the ordinary arg-inc / exit-dec
        // balance applies.
        self.rc_elision
            .conditional_adopted_roots
            .get(&self.fn_ctx.current_fn_name)
            .and_then(|m| m.get(name))
            .filter(|(t, _)| self.target_abi.headerless_types.contains(t))
            .cloned()
    }

    /// Phase C2b: skip the call-site `Option[shared T]` arg inc when
    /// the callee's param at `position` is a borrowed-family param of
    /// a reconciled headerless type (the callee's exit dec is skipped
    /// symmetrically — see `compile_function`'s param registration).
    fn borrowed_arg_skip(&self, callee: &str, position: usize) -> bool {
        self.rc_elision.borrowed_param_skips.get(callee).is_some_and(|recs| {
            recs.iter()
                .any(|(_, pos, t)| *pos == position && self.target_abi.headerless_types.contains(t))
        })
        // PROTOTYPE RC-elide-ref: the callee's param at `position` was
        // classified `ref`/borrow by the ownership pass → skip the
        // call-site arg inc (the callee's exit dec is skipped
        // symmetrically in `borrowed_param_dec_skip`).
        || self
            .drop_rc
                .rc_elide_ref_params
            .get(callee)
            .is_some_and(|recs| recs.iter().any(|(_, pos)| *pos == position))
    }

    /// Phase C2b: the callee-side half — `param_name` of the CURRENT fn
    /// is a borrowed-family param of a reconciled headerless type, so
    /// its exit `RcDecOption` is skipped (no caller inc ever happened).
    fn borrowed_param_dec_skip(&self, param_name: &str) -> bool {
        self.rc_elision.borrowed_param_skips
            .get(&self.fn_ctx.current_fn_name)
            .is_some_and(|recs| {
                recs.iter()
                    .any(|(n, _, t)| n == param_name && self.target_abi.headerless_types.contains(t))
            })
        // PROTOTYPE RC-elide-ref: the current fn's `param_name` was
        // classified `ref`/borrow → skip its exit dec (the caller
        // skipped the arg inc symmetrically in `borrowed_arg_skip`).
        || self
            .drop_rc
                .rc_elide_ref_params
            .get(&self.fn_ctx.current_fn_name)
            .is_some_and(|recs| recs.iter().any(|(n, _)| n == param_name))
    }

    /// Phase-B2 role lookup for the current function.
    fn b2_binding(&self, name: &str) -> Option<&state::B2Binding> {
        self.rc_elision
            .elided_b2_bindings
            .get(&self.fn_ctx.current_fn_name)
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
        if self.target_abi.headerless_types.contains(type_name) {
            return true;
        }
        let Some(link_idx) = self
            .target_abi
            .headerless_fns
            .get(&self.fn_ctx.current_fn_name)
            .and_then(|m| m.get(type_name))
        else {
            return false;
        };
        if self.is_coroutine_compiled(&self.fn_ctx.current_fn_name) {
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
        // Weak-targeted types carry a two-word `{ strong, weak, fields… }`
        // control header, so user field 0 is at heap index 2. Checked FIRST:
        // such a type is force-headed (never headerless), and this is the one
        // place the base shift is expressed. Everything that routes field GEPs
        // through this funnel picks it up for free. (`docs/spikes/weak-refs.md`.)
        if self
            .type_decls
            .shared_types
            .get(type_name)
            .is_some_and(|i| i.has_weak_header)
        {
            (heap_type, 2)
        } else if self.headerless_here(type_name) {
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
        self.drop_rc
            .arc_fallback_fns
            .get(&self.fn_ctx.current_fn_name)
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
            self.conc
                .concurrency_decisions
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
        if self.conc.concurrency_decisions.is_empty() {
            return None;
        }
        self.conc
            .concurrency_decisions
            .get(&self.fn_ctx.current_fn_name)
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
        loop_line: usize,
    ) -> Option<&crate::concurrency::LoopReduction> {
        // Matched on (stmt_index, loop_line), not index alone: since the
        // analyzer recurses into nested blocks (2026-07-15), `stmt_index`
        // is an index within the loop's OWN block, and equal indices
        // recur across sibling/nested blocks — the source line is what
        // makes the pair unique per loop.
        let decision = self
            .conc
            .concurrency_decisions
            .get(&self.fn_ctx.current_fn_name)?;
        decision
            .loop_reductions
            .iter()
            .find(|r| r.stmt_index == stmt_index && r.loop_line == loop_line)
    }

    /// Sibling of [`Self::loop_reduction_for_stmt`] for the indexed-write
    /// fan-out shape — but keyed on the loop expression's **byte span**, not
    /// `(stmt_index, loop_line)`.
    ///
    /// The stricter key is load-bearing, not tidiness. A nested loop written on
    /// its parent's source line shares both the parent's statement index (0,
    /// within the parent's own body block) and its line; codegen compiles the
    /// parent's body inside the fan-out worker, meets the inner loop there, and
    /// a line-keyed lookup hands it the parent's tag. The result is a second
    /// fan-out over a loop nothing proved disjoint — see
    /// `DisjointWriteLoop::loop_span` for the concrete same-slot race that
    /// produces. `stmt_index` is kept in the match as a cheap first filter.
    pub(crate) fn disjoint_write_loop_for_stmt(
        &self,
        stmt_index: usize,
        loop_span: &crate::token::Span,
    ) -> Option<&crate::concurrency::DisjointWriteLoop> {
        let decision = self
            .conc
            .concurrency_decisions
            .get(&self.fn_ctx.current_fn_name)?;
        decision.disjoint_write_loops.iter().find(|d| {
            d.stmt_index == stmt_index
                && d.loop_span.offset == loop_span.offset
                && d.loop_span.length == loop_span.length
        })
    }

    // ── Program / function compilation ───────────────────────────

    /// Populate the type-alias (refinement + plain) and distinct-type base
    /// maps from the user program (plus baked-stdlib distinct types). Called
    /// early in `compile_program`, *before* struct/enum layouts are built, so
    /// a field whose type names a refinement (`type Email = String where …`),
    /// a plain alias (`type Name = String;`), or a distinct type resolves to
    /// the base's layout while the aggregate is lowered — not after, where
    /// the name would hit the `i64` unknown-name fall-through and mis-size
    /// the field.
    fn populate_type_alias_bases(&mut self, program: &Program) {
        // Refinement type aliases (`type Email = String where …`): record
        // each one's base `TypeExpr` so type lowering resolves the
        // refinement to its base's layout (phase-9 step 4). A refinement
        // carries no runtime tag — it is layout-identical to its base.
        for item in &program.items {
            if let Item::TypeAlias(t) = item {
                if let Some(pred) = &t.refinement {
                    self.contract_state
                        .refinement_bases
                        .insert(t.name.clone(), t.ty.clone());
                    self.contract_state
                        .refinement_predicates
                        .insert(t.name.clone(), pred.clone());
                    // Generic refinement (`type NonEmpty[T] = Vec[T] where …`):
                    // remember the param names so a use at concrete arity
                    // substitutes the right element type into the base.
                    if let Some(gp) = &t.generic_params {
                        self.contract_state.refinement_generic_params.insert(
                            t.name.clone(),
                            gp.params.iter().map(|p| p.name.clone()).collect(),
                        );
                    }
                } else if !matches!(t.ty.kind, TypeKind::ImplTrait { .. }) {
                    // Plain type alias (`type Name = String;`) — the
                    // `where`-free arm, B-2026-07-30-7. Fully transparent:
                    // the typechecker lowers it straight to its base
                    // (`env_add_type_alias`), so codegen must too or the
                    // name hits the `i64` unknown-name fall-through and
                    // mis-sizes every param / return / field / binding
                    // written against a non-`i64`-shaped base.
                    //
                    // TAIT aliases (`type X = impl Trait;`) are excluded: the
                    // RHS has no layout to project (`impl Trait` is still
                    // `E_TAIT_NOT_IMPLEMENTED_YET` at witness-required use
                    // sites), so peeling to it buys nothing over the
                    // fall-through and would only obscure the diagnostic.
                    self.payload_vars
                        .plain_alias_bases
                        .insert(t.name.clone(), t.ty.clone());
                    // Generic alias (`type Plain[T] = Vec[T];`): remember the
                    // param names so a use at concrete arity substitutes the
                    // right element type into the base — same reason as the
                    // refinement arm above.
                    if let Some(gp) = &t.generic_params {
                        self.payload_vars.plain_alias_generic_params.insert(
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
                self.contract_state
                    .distinct_bases
                    .insert(d.name.clone(), d.base_type.clone());
                // Combined `distinct type T = Base where pred`: register the
                // predicate so the `T(value)` constructor emits the runtime
                // assertion via `emit_refinement_assert`. Keyed by the
                // distinct name, parallel to refinements.
                if let Some(pred) = &d.refinement {
                    self.contract_state
                        .refinement_predicates
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
                    self.contract_state
                        .distinct_bases
                        .entry(d.name.clone())
                        .or_insert_with(|| d.base_type.clone());
                    if let Some(pred) = &d.refinement {
                        self.contract_state
                            .refinement_predicates
                            .entry(d.name.clone())
                            .or_insert_with(|| pred.clone());
                    }
                }
            }
        }

        self.prune_cyclic_type_aliases();
    }

    /// Drop every type-alias entry that sits on a base-name CYCLE, so the
    /// alias-peeling recursions (`llvm_type_for_type_expr` →
    /// `resolve_type_alias_te` → itself, and `register_var_from_type_expr`'s
    /// nested-alias recursion) are guaranteed to terminate.
    ///
    /// Nothing upstream rejects `type A = A;`, `type A = B; type B = A;`, or
    /// the shadowing form `type Vec = Vec[i64];` — the typechecker lowers
    /// such an alias to an opaque named type and moves on. A pruned entry
    /// falls back to the pre-existing unknown-name behavior (the `i64`
    /// fall-through), which is wrong but bounded; the alternative is a
    /// compiler stack overflow on nonsense input. Well-formed programs never
    /// reach the prune list, so this costs one bounded walk per alias at
    /// startup and nothing at lowering time.
    fn prune_cyclic_type_aliases(&mut self) {
        fn head_name(te: &TypeExpr) -> Option<&String> {
            match &te.kind {
                TypeKind::Path(p) => p.segments.first(),
                _ => None,
            }
        }
        let cyclic: Vec<String> = self
            .contract_state
            .refinement_bases
            .keys()
            .chain(self.payload_vars.plain_alias_bases.keys())
            .filter(|name| {
                let mut cur = (*name).clone();
                let mut seen = std::collections::HashSet::new();
                seen.insert(cur.clone());
                loop {
                    let Some(base) = self
                        .contract_state
                        .refinement_bases
                        .get(&cur)
                        .or_else(|| self.payload_vars.plain_alias_bases.get(&cur))
                    else {
                        return false;
                    };
                    let Some(next) = head_name(base) else {
                        return false;
                    };
                    if !seen.insert(next.clone()) {
                        return true;
                    }
                    cur = next.clone();
                }
            })
            .cloned()
            .collect();
        for name in cyclic {
            self.contract_state.refinement_bases.remove(&name);
            self.contract_state.refinement_generic_params.remove(&name);
            self.payload_vars.plain_alias_bases.remove(&name);
            self.payload_vars.plain_alias_generic_params.remove(&name);
        }
    }

    /// Populate `assoc_type_bindings` from every CONCRETE (non-generic) impl
    /// block's associated-type bindings — `impl <Trait> for T { type Assoc =
    /// <ty>; … }` → `(T, "Assoc") -> <ty>`. Called early in `compile_program`
    /// (before any monomorph is declared) so `llvm_type_for_type_expr` can
    /// resolve an associated-type projection (`C.Item`) in a generic fn's
    /// signature. Only NON-generic impls are recorded: a generic impl's binding
    /// RHS may reference the impl's own params (`impl[T] … for Box[T] { type
    /// Item = T }`), which would need per-instantiation substitution — a
    /// follow-on. Baked-stdlib collections seed the typechecker's equivalent
    /// table (`impl_assoc_types`) but are handled by codegen's own container
    /// lowering, so they need no entry here.
    fn populate_assoc_type_bindings(&mut self, program: &Program) {
        for item in &program.items {
            if let Item::ImplBlock(imp) = item {
                if imp.generic_params.is_some() {
                    continue;
                }
                let Some(target) = crate::codegen::helpers::impl_target_name(&imp.target_type)
                else {
                    continue;
                };
                for impl_item in &imp.items {
                    if let ImplItem::AssocType(binding) = impl_item {
                        // Skip GAT bindings (`type Mapped[U] = …`) — their RHS
                        // is parameterized and needs the projection's own args
                        // to resolve; the non-parameterized case is what the
                        // generic-fn projection return type needs.
                        if binding.generic_params.is_some() {
                            continue;
                        }
                        self.assoc_type_bindings
                            .insert((target.clone(), binding.name.clone()), binding.ty.clone());
                    }
                }
            }
        }
    }

    fn compile_program(&mut self, program: &Program) -> Result<(), String> {
        // B-2026-08-21-6 — the parser deleted each `Map[K, V, H]` hasher
        // argument and left the choice here, keyed by the container path's
        // span. Every `Codegen::new` entry funnels through this method, so one
        // load covers the AOT, JIT, REPL-cell and test-module paths alike.
        self.container_hashers.extend(
            program
                .container_hashers
                .iter()
                .map(|(k, v)| (*k, v.clone())),
        );
        // B-2026-08-22-6 — and the builder → per-hash-state hop a USER hasher
        // needs, read off the same AST in the same place.
        self.collect_user_hasher_states(program);
        // B-2026-08-07-10 — `KARAC_TEXT_PAD=<bytes>`: a filler function ahead
        // of the program's own code, so a measurement can move `main` (and the
        // hot loop inside it) by a CHOSEN number of bytes while every
        // instruction of the program stays identical.
        //
        // WHY A SECOND LEVER, next to `KARAC_FN_ALIGN`. That one can only
        // align, which quantises placement instead of setting it: on kata:170
        // the whole 4..1024 sweep reaches three distinct 64-byte residues out
        // of sixteen, and cannot reproduce the 152-byte shift this row
        // bisected (fast side at residue 8, slow side at 32). Placement is the
        // independent variable the row is about, so it needs a lever that
        // varies it continuously. The row names this experiment outright —
        // "that needs either an LLVM alignment flag or a padding experiment,
        // neither of which was run".
        //
        // Emitted here, at the top of program lowering, because the backend
        // emits functions in module order: added after `main` it would shift
        // nothing. Never called, so the bytes' decoding is irrelevant;
        // `.space` is zeros. Pinned into `@llvm.used` so `-dead_strip` keeps
        // it — the row records that a plain dead function is eliminated and is
        // therefore NOT a placement lever. Off unless set, so no default build
        // changes.
        Self::apply_llvm_cl_args();
        self.emit_text_pad()?;
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
        self.tracing.runtime_panic_prefix_needed = self.main_symbol_override.is_some()
            || (!self.contract_state.strip_contracts
                && contracts::program_declares_contracts(program));
        // Eagerly cache the host `TargetData` up front (phase-10 line 282): the
        // `&self` drop-synthesis paths read `self.target_data` to size the
        // `karac_free_buf` recycling hint (`cap × elem_abi_size`) for a
        // multi-byte-element Vec FIELD, and a `None` there would silently
        // under-hint back to `cap × 1`. Ignore a target-machine failure — the
        // hint falls back to `1` (a sound under-hint, never a correctness issue).
        let _ = self.ensure_target_data();
        // Interprocedural bounds preconditions (bce_interproc.rs,
        // B-2026-08-05-6). Whole-PROGRAM analysis — it needs every call site of
        // a candidate callee — so it runs here rather than per function, and
        // `compile_function` only merges its own entry. Both kill switches
        // apply: its own, and the converging-skip switch it feeds.
        self.bce.interproc_conv_skips = if std::env::var("KARAC_BCE_INTERPROC").as_deref()
            == Ok("0")
            || std::env::var("KARAC_BCE_CONV_SKIP").as_deref() == Ok("0")
        {
            HashMap::new()
        } else {
            crate::codegen::bce_interproc::compute_interproc_converging_skips(program)
        };
        // Level 2 crash diagnostics — Part 2: stand up DWARF debug-info state
        // before any function compiles (no-op unless KARAC_DEBUG_INFO is set and
        // a source filename was threaded in via set_source_filename, which runs
        // before compile_program).
        self.di_init();
        // ownership-model-mechanization Slice 4 (structural): when
        // `KARAC_ORACLE_DROP_CHECK` is set, run the ownership oracle on this
        // (lowered) program and arm the drop recorder, so `compile_program` can
        // verify at the end that codegen's emitted cleanup covers the oracle's
        // drop schedule on every function — the invariant the structural fix
        // makes hold by construction, checked here first. Off by default (one
        // env probe); it yields to an external arming (the differential harness)
        // rather than fight it, so it never perturbs a real build or the fuzzer.
        // The oracle runs on codegen's own lowered tree — validated to agree
        // with codegen's emitted drops on the fuzzer corpus (0 divergences),
        // which is why no surface tree needs threading in.
        let mut oracle_drop_schedule = if std::env::var_os("KARAC_ORACLE_DROP_CHECK").is_some()
            && !crate::codegen::drop_obs::armed()
        {
            crate::codegen::drop_obs::begin();
            Some(crate::ownership_oracle::analyze(program))
        } else {
            None
        };
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
        // Associated-type bindings from concrete impls — so an
        // associated-type projection (`C.Item`) in a generic fn's signature
        // resolves to the concrete bound type during monomorphization.
        self.populate_assoc_type_bindings(program);
        // Two-pass struct declaration with `declare_enums` interleaved, so a
        // struct field that names a user enum lowers at the enum's real
        // tagged-union shape instead of collapsing to the `i64` fall-through
        // (the self-hosting `enum-in-struct-field` blocker). (1) register
        // struct field metadata (AST, no LLVM types); (2) build enum layouts —
        // their payload sizing recurses through that struct metadata, not the
        // not-yet-built struct LLVM types; (3) build struct LLVM types, now
        // that `enum_layouts` is populated. See
        // `declarations.rs::register_struct_metadata` for the cycle rationale.
        // `KARAC_STRICT_TYPE_LOWERING` support only — see the field doc.
        // Cheap one-pass scan; the set is never consulted unless the lever
        // is on. EVERY baked stdlib program is scanned, not just the ones this
        // compilation pulls in: most of the generic params a real compilation
        // lowers without a substitution come from there (`T` fires on
        // `hello world` otherwise), and the usage-gated set misses the
        // PRELUDE-baked declarations specifically — `Result[T, E]`'s own `E`
        // reaches the default on any program calling a Result combinator, and
        // read as a real unknown type until this scanned the full set.
        self.collect_declared_generic_param_names(program);
        for (_, tp) in crate::prelude::STDLIB_PROGRAMS.iter() {
            self.collect_declared_generic_param_names(tp);
        }
        for (_, tp) in crate::prelude::GATED_STDLIB_PROGRAMS.iter() {
            self.collect_declared_generic_param_names(tp);
        }
        // Name-only `shared`/`par` set, before `declare_enums` — see the field doc.
        self.collect_shared_type_names(program);
        self.register_struct_metadata(program);
        // AFTER `register_struct_metadata`, because this seed measures the
        // program's channel element types — including user structs — to size
        // its payload area. See the fn's own doc comment.
        self.seed_send_error_enum_layout(program);
        self.register_ord_orderable_types(program);
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
        if self.conc.coro_enabled {
            // `Server.serve_ws` ws-handler exclusion (phase-8 line 170): a fn
            // passed as the third arg of `Server.serve_ws(addr, handler,
            // ws_handler)` is invoked through the runtime's `extern "C"
            // fn(i64)` callback slot on a DEDICATED blocking thread — the
            // same posture as `main`'s top-level accept. It cannot be a
            // caller-driven coroutine ramp (the FFI slot is a plain void
            // call), and its network leaf ops (`recv_text` parks) are exactly
            // the thread-block park path. Collect the names before the key
            // population below so they stay on the non-coro path.
            let ws_handler_names = collect_serve_ws_handler_names(program);
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
                if key != "main"
                    && !ws_handler_names.contains(key)
                    && !declarations::is_generic_fn_key(program, key)
                {
                    self.conc.coro_fn_keys.insert(key.clone());
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
            .target_abi
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
            // Force-headed: a `weak`-targeted type MUST keep its `{ strong, weak
            // }` control header (a headerless node has no count word to answer
            // "is the target alive?"), so it can never be headerless. The
            // whole-program weak-target set is computed in `build_struct_types`,
            // which runs before this reconcile. (`docs/spikes/weak-refs.md`.)
            if self.rc_elision.weak_targeted_types.contains(&t) {
                continue;
            }
            self.target_abi.headerless_types.insert(t);
        }
        // Slice 8v Phase 2: snapshot the whole `Program` as `Rc<Program>`
        // so the per-mono state-machine emission path triggered from
        // `compile_generic_call` can access layouts / yield points /
        // function ASTs without `&Program` being plumbed through
        // `compile_expr` → `compile_call` → `compile_generic_call`.
        // Cheap `Rc` clones flow to per-mono callers as they fire.
        self.program_snapshot = Some(Rc::new(program.clone()));
        // Collect SoA `layout` blocks BEFORE the state-machine emission below:
        // a persisted local that is SoA (a `layout`-named `Vec[E]` carried
        // across a suspend, e.g. the browser render loop's `grid`) must size its
        // state-struct field as the 4-field SoA struct, not the AoS
        // `{ptr,len,cap}` Vec — and `emit_state_struct_types` /
        // `emit_state_machine_state_{constructors,destructors}` consult
        // `soa_layouts` to do so. Collected here too (kept below for the
        // non-state-machine path's idempotent re-collect is harmless); without
        // this the catalogue was empty during state-struct emission and an SoA
        // carried-grid coroutine mis-sized its frame slot.
        self.collect_soa_layouts(program);
        // Pre-populate `fn_asts` BEFORE the state-machine emission below. The
        // canonical population is in the declare loop (further down), but the
        // state-machine passes — `emit_state_machine_state_{constructors,
        // destructors}` — compile suspending-function bodies (the poll-fn), and
        // those bodies' `let g = builder()` SoA-return inference consults
        // `fn_asts` via `let_rhs_calls_layout_returning_fn`. With `fn_asts` still
        // empty there, a `let grid = init_grid()` in the browser render loop was
        // NOT recognized as a layout-returning call, so it kept the AoS
        // `{ptr,len,cap}` slot while `seed_binding_site_layout` had already
        // seeded the binding SoA — the call dispatch then routed `substep` to its
        // SoA monomorph and passed the 3-field header into a 4-field param (LLVM
        // signature mismatch). Seeding the AST map first makes the inference
        // consistent across the state-machine body compiles and the normal pass.
        // The later canonical insert is idempotent (same clones).
        for item in &program.items {
            if let Item::Function(f) = item {
                if f.generic_params.is_none() {
                    self.fn_sig.fn_asts.insert(f.name.clone(), f.clone());
                }
            }
        }
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
        self.span_tables.question_conversions = program.question_conversions.clone();
        // Side-table set by `lowering::lower_program`: each `?` site's unwrapped
        // Ok/Some payload type, so `reconstruct_question_ok_payload` rebuilds a
        // multi-word payload of any shape (B-2026-07-13-19).
        self.span_tables.question_ok_payload_types = program.question_ok_payload_types.clone();
        self.span_tables.wp_result_types = program.wp_result_types.clone();

        // Side-table set by the cli pipeline after effectcheck: per-callee
        // boolean indicating whether the callee carries any observable
        // effect (reads/writes/sends/receives). Read by
        // `emit_branch_cancel_check` to skip the cancel atomic load when the
        // callee is provably non-observable.
        self.fn_sig.callee_effectful = program.callee_effectful.clone();

        // Side-table set by `lowering::lower_program`: each `MethodCall`
        // expression's span maps to the canonical `Type.method` callee key.
        // Read by `compile_method_call` so the par-branch cancel-check
        // narrowing applies to instance methods, not just free-function
        // and `Type.assoc` calls.
        self.span_tables.method_callee_types = program.method_callee_types.clone();
        self.span_tables.impl_dispatch_names =
            crate::impl_dispatch::collect_impl_dispatch_names(program);
        self.span_tables.method_impl_dispatch = program.method_impl_dispatch.clone();

        // Side-table set by `lowering::lower_program` from
        // `TypeCheckResult.expr_types`: the spans of every `Type::Str`
        // expression. `emit_sort_by_key_inline_thunk` consults this set
        // to dispatch String keys to the `karac_string_cmp` arm — the
        // LLVM struct shape is identical to `Vec[u8]` and a few other
        // 3-word types, so the value alone can't distinguish them.
        self.span_tables.string_typed_exprs = program.string_typed_exprs.clone();
        self.span_tables.borrow_vec_typed_exprs = program.borrow_vec_typed_exprs.clone();
        self.span_tables.iterator_typed_exprs = program.iterator_typed_exprs.clone();
        self.span_tables.fn_value_typed_exprs = program.fn_value_typed_exprs.clone();
        // Per-generic-call-site resolved type-arg substitution — lets
        // `compile_generic_call` bind container element type params the
        // LLVM-type inference can't (B-2026-07-02-41).
        self.span_tables.call_type_subs = program.call_type_subs.clone();
        // Sibling: element-aware mono-mangle tokens (B-2026-07-11-35), so
        // `compile_generic_call` gives a distinct symbol to each builtin-
        // collection whole-type-param instantiation sharing the `{ptr,i64,i64}`
        // LLVM shape.
        self.span_tables.call_type_subs_mangle = program.call_type_subs_mangle.clone();
        // Sibling: per-span Tensor element-type + static-dims info for
        // construction / let-registration / indexing dispatch (see
        // `src/codegen/tensor.rs`).
        self.accel.tensor_typed_exprs = program.tensor_typed_exprs.clone();
        self.accel.tensor_index_recv_types = program.tensor_index_recv_types.clone();
        self.span_tables.index_recv_vec_types = program.index_recv_vec_types.clone();
        // Sibling: per-span Column element-type info for construction /
        // let-registration / indexing dispatch (see `src/codegen/column.rs`).
        self.accel.column_typed_exprs = program.column_typed_exprs.clone();
        // Sibling: spans of unsigned-element vector expressions, so the SIMD
        // `reduce_min/max` codegen picks `ult`/`ugt` over the signed default.
        self.span_tables.unsigned_vector_exprs = program.unsigned_vector_exprs.clone();
        self.span_tables.unsigned_int_exprs = program.unsigned_int_exprs.clone();
        self.span_tables.cast_source_unsigned = program.cast_source_unsigned.clone();
        self.span_tables.vector_method_call_spans = program.vector_method_call_spans.clone();
        // Sibling to `string_typed_exprs` for `Type::Named` struct
        // expressions. Maps span → struct name. `emit_sort_by_key_inline_thunk`
        // consults this to dispatch struct-typed keys (e.g.
        // `sort_by_key(|item| item)` where `item: MyStruct`) to a
        // field-aware lex cascade that picks the right per-field
        // comparator via `self.type_decls.struct_field_type_names[struct_name]`.
        self.span_tables.expr_struct_type_names = program.expr_struct_type_names.clone();
        // Sibling map for spans whose struct type has a user `impl Ord`.
        // `emit_sort_by_key_inline_thunk` consults it before the derive
        // cascade to dispatch to the user's compiled `Type.cmp` via
        // direct call.
        self.span_tables.user_ord_typed_exprs = program.user_ord_typed_exprs.clone();

        // Surface TypeExpr per heap-owning temporary expression. Keyed by
        // span; `materialize_owned_temp` consults it to scope-drop unnamed
        // Vec/String (with element type), Map/Set handles, and RC boxes.
        self.drop_rc.owned_temp_drops = program.owned_temp_drops.clone();

        // Pointee TypeExpr per raw-pointer-typed expression. The unary-deref
        // arm keys this by operand span to load through `*const T` / `*mut T`.
        self.span_tables.raw_pointer_pointee_types = program.raw_pointer_pointee_types.clone();

        // Fully-instantiated TypeExpr per generic Named instantiation
        // expression (`Option[String]`, `Result[i64, AllocError]`, …). Keyed
        // by span; `compile_enum_eq` consults it to recover the concrete type
        // argument a generic heap-payload enum's variant was instantiated
        // with, so `Some(String)` compares by content not pointer word.
        self.type_decls.enum_inst_type_exprs = program.enum_inst_type_exprs.clone();
        self.mod_bindings.module_binding_types = program.module_binding_types.clone();
        self.span_tables.concrete_named_type_exprs = program.concrete_named_type_exprs.clone();

        // Phase 6 line 26 slice 8ab: snapshot the per-call effect-
        // variable substitution table. Slice 8y (entry 32) reads
        // this in `compile_generic_call` to gate per-mono state-
        // machine emission on whether the resolved per-call effects
        // include any network-yield verb.
        self.span_tables.call_effect_subs = program.call_effect_subs.clone();

        // Side-table set by `lowering::lower_program`: each
        // `unwrap`/`expect`/`is_*` MethodCall on `Option[T]` or `Result[T, E]`
        // maps to the inner `TypeExpr`. Read by the codegen `unwrap` arm
        // to know how to reconstitute the payload back to a value of T.
        self.span_tables.method_unwrap_inner_types = program.method_unwrap_inner_types.clone();
        self.span_tables.optional_chain_lowering = program.optional_chain_lowering.clone();
        self.span_tables.method_unwrap_err_types = program.method_unwrap_err_types.clone();
        self.span_tables.temp_recv_elem_types = program.temp_recv_elem_types.clone();
        // Keyed by enum NAME, so it lives with the other type declarations
        // rather than in the span tables (B-2026-08-21-10).
        self.type_decls.enum_discriminants = program.enum_discriminants.clone();
        self.mapset.temp_recv_mapset_types = program.temp_recv_mapset_types.clone();
        self.span_tables.temp_recv_len_elem_types = program.temp_recv_len_elem_types.clone();
        self.span_tables.iter_terminal_elem_types = program.iter_terminal_elem_types.clone();
        self.span_tables.iter_terminal_acc_types = program.iter_terminal_acc_types.clone();
        self.conc.channel_elem_types = program.channel_elem_types.clone();
        self.span_tables.stats_elem_types = program.stats_elem_types.clone();
        self.accel.gpu_dispatch_wgsl = program.gpu_dispatch_wgsl.clone();
        self.accel.gpu_resident_field = program.gpu_resident_field.clone();
        self.accel.gpu_reduce_int_elems = program.gpu_reduce_int_elems.clone();
        self.conc.task_join_return_types = program.task_join_return_types.clone();
        self.ref_return_inner_types = program.ref_return_inner_types.clone();
        self.contract_state.secret_inner_types = program.secret_inner_types.clone();
        self.display.display_option_result_types = program.display_option_result_types.clone();
        self.display.display_tuple_types = program.display_tuple_types.clone();
        self.display.display_generic_enum_types = program.display_generic_enum_types.clone();
        self.display.display_vec_types = program.display_vec_types.clone();
        self.display.display_map_types = program.display_map_types.clone();
        self.display.display_set_types = program.display_set_types.clone();
        self.display.display_sorted_collection_spans =
            program.display_sorted_collection_spans.clone();
        // Bare names of user impl methods that return a borrow — gates the
        // method-ref caller path away from builtin ref-returning methods.
        for item in &program.items {
            if let Item::ImplBlock(imp) = item {
                for impl_item in &imp.items {
                    if let ImplItem::Method(m) = impl_item {
                        if let Some(TypeKind::Ref(inner) | TypeKind::MutRef(inner)) =
                            m.return_type.as_ref().map(|t| &t.kind)
                        {
                            self.user_ref_method_names.insert(m.name.clone());
                            // B-2026-07-29-12: the borrow's INNER type, by
                            // method name. The span-keyed
                            // `ref_return_inner_types` cannot serve a CHAINED
                            // receiver (`h.view().is_empty()`): the parser
                            // gives both calls one span, and the outer call's
                            // `bool` result overwrites the inner call's
                            // `ref Vec[i64]` in `expr_types`, which is what
                            // that table is derived from. The declaration is
                            // immune — it is read off the impl item here, not
                            // off an inferred expression type.
                            self.user_ref_method_inner
                                .insert(m.name.clone(), (**inner).clone());
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
        self.pattern_state.pattern_binding_types = program.pattern_binding_types.clone();

        // Side-table set by `lowering::lower_program`: each pattern-
        // binding's span maps to its inner element TypeExpr for `Vec[T]` /
        // `Slice[T]` bindings only. Read by `bind_pattern_values` to
        // populate `vec_elem_types` / `slice_elem_types` under the
        // binding's variable name so direct method dispatch on the
        // binding (`xs.len()`, `xs[0]`, `xs.push(...)`) routes through
        // the right element-typed path. PB sibling slice (2026-05-09).
        self.pattern_state.pattern_binding_inner_types =
            program.pattern_binding_inner_types.clone();

        // Side-table set by `lowering::lower_program`: each pattern-
        // binding's span maps to its borrow form (`Ref` / `MutRef`) when
        // the enclosing match scrutinee is `ref T` / `mut ref T`. Owned
        // bindings are absent. Read by `bind_pattern_values` (Binding
        // arm) to wrap the value-typed leaf alloca in a ref-shim so call
        // sites that take a `ref T` / `mut ref T` parameter receive the
        // right ABI shape — slice 3a, 2026-05-14.
        self.pattern_state.pattern_binding_borrow_modes =
            program.pattern_binding_borrow_modes.clone();

        // Top-level `const NAME: T = value` collection. References from
        // function bodies (parsed as `ExprKind::Identifier(name)` for bare
        // uses) look up this map and re-compile the stored expression at
        // each use site; LLVM folds the resulting constant arithmetic.
        // Must precede function compilation so forward references work.
        for item in &program.items {
            if let Item::ConstDecl(c) = item {
                self.mod_bindings
                    .consts
                    .insert(c.name.clone(), c.value.clone());
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
                self.provider_state
                    .provider_resource_ids
                    .insert(decl.name.clone(), next_resource_id);
                next_resource_id += 1;
                if !decl.provider_bounds.is_empty() {
                    self.provider_state.provider_resource_traits.insert(
                        decl.name.clone(),
                        decl.provider_bounds
                            .iter()
                            .map(|b| b.name.clone())
                            .collect(),
                    );
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
            self.provider_state
                .provider_resource_ids
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
                self.provider_state
                    .provider_trait_methods
                    .insert(t.name.clone(), methods);
            }
        }

        // Record boxed-return exports (Slice 4 Path B) up front so
        // `compile_call` can reject an internal Kāra call to one before any
        // body is compiled. A Slice-2a tagged-union `#[repr(C)]` enum return
        // is boxed the same way (its LLVM signature returns `ptr`), so it goes
        // in `boxed_export_names` too (internal-call rejection) *and* in
        // `boxed_enum_export_names` (marks the box as enum-shaped — a distinct,
        // buffer-free destructor).
        for item in &program.items {
            if let Item::Function(f) = item {
                if crate::cheader::boxed_return_of(f).is_some() {
                    self.target_abi.boxed_export_names.insert(f.name.clone());
                } else if crate::cheader::export_return_is_boxed_enum(f, program) {
                    self.target_abi.boxed_export_names.insert(f.name.clone());
                    self.boxed_enum_export_names.insert(f.name.clone());
                }
            }
        }

        // Compiler-driven inline hints (phase-11 Codegen Optimization): decide,
        // per concrete user function with no explicit `#[inline]`, whether to
        // attach a heuristic `inlinehint` / `noinline`. Computed once here (a
        // whole-program size + call-site census) so `emit_codegen_hint_attrs`
        // can consult it during the per-function declaration pass below.
        self.fn_sig.heuristic_inline_hints = crate::inline_hints::compute(program);

        // First pass: register generic functions for on-demand monomorphization;
        // declare concrete (non-generic) functions for forward-call support.
        for item in &program.items {
            if let Item::Function(f) = item {
                // `comptime fn`s (including `#[derive(X)]`'s `derive_x`) run only
                // at compile time — the comptime fold evaluates them via the
                // interpreter and splices their results; their bodies (reflection
                // calls like `T.name()`, `ast.item(..)`) have no runtime lowering
                // and must NOT reach codegen, which would fail dispatch ("no
                // handler for method 'name' on variable 'T'"). Skip declaring +
                // compiling them entirely (B-2026-07-08-15 Layer 3).
                if f.is_comptime {
                    continue;
                }
                if f.generic_params.is_some() {
                    self.mono_state
                        .generic_fns
                        .insert(f.name.clone(), f.clone());
                    // Register the CONCRETE return-type name (if any) so code
                    // that consults `fn_return_type_names` — the print
                    // signedness check (`expr_is_unsigned_int`), call-result var
                    // typing — works for a generic fn with a non-generic return
                    // (`gwrap[T](x: T) -> u8` printed `255u8` as `-1` because the
                    // Call arm found no entry and defaulted to signed). A generic
                    // return (`-> T`, where `T` is one of the fn's own params) has
                    // no static name — skip it. B-2026-07-03-N.
                    if let Some(TypeKind::Path(path)) = f.return_type.as_ref().map(|t| &t.kind) {
                        if let Some(seg) = path.segments.first() {
                            let is_generic_param = f
                                .generic_params
                                .as_ref()
                                .is_some_and(|gp| gp.params.iter().any(|p| &p.name == seg));
                            if !is_generic_param {
                                self.fn_sig
                                    .fn_return_type_names
                                    .insert(f.name.clone(), seg.clone());
                            }
                        }
                    }
                } else {
                    self.declare_function(f)?;
                    // Retain the AST for on-demand per-layout monomorphization
                    // (slice 2): a SoA argument at a call site compiles a layout
                    // specialization of this body. The all-`Aos` body is the one
                    // just declared and compiled in the normal pass.
                    self.fn_sig.fn_asts.insert(f.name.clone(), f.clone());
                }
            }
        }

        // Seed baked-stdlib generic FREE functions into `generic_fns` so a
        // bare call from user code (`min(a, b)`, `clamp(v, lo, hi)` — roadmap
        // Phase 8 § std.cmp) monomorphizes on demand through the same path as
        // a user generic fn. Unlike stdlib impl methods (declared by
        // `declare_stdlib_program`), free fns never reach that pass, and the
        // user-program loop above only sees `program.items`. `#[compiler_builtin]`
        // free fns (`spawn`, `size_of`, `with_span`, …) are skipped — they have
        // placeholder bodies and dedicated call-site intercepts.
        //
        // The user program wins any name collision. A user `fn max(...)` that is
        // NON-generic is `declare_function`'d (never in `generic_fns`), so a
        // plain `or_insert` would still add the stdlib generic `max` — and the
        // call-site generic-dispatch check (`call_dispatch.rs`) fires BEFORE the
        // concrete-call path, mis-routing `max(data)` to the stdlib body
        // (`examples/array_basics.kara` defines its own `fn max(a: Array[...])`).
        // So skip any name the user program itself defines as a free function,
        // generic or not. Gated on `user_redefines_stdlib_type` too, so a program
        // that shadows a module's type (and thus has that whole module skipped at
        // declare/compile) does not get its free fns either — keeping the three
        // passes in lockstep.
        let user_fn_names: std::collections::HashSet<&str> = program
            .items
            .iter()
            .filter_map(|it| match it {
                Item::Function(f) => Some(f.name.as_str()),
                _ => None,
            })
            .collect();
        for tp in compiled_stdlib_programs(program) {
            if user_redefines_stdlib_type(program, tp) {
                continue;
            }
            for item in &tp.items {
                if let Item::Function(f) = item {
                    if f.generic_params.is_some()
                        && !f.attributes.iter().any(|a| a.is_bare("compiler_builtin"))
                        && !user_fn_names.contains(f.name.as_str())
                    {
                        self.mono_state
                            .generic_fns
                            .entry(f.name.clone())
                            .or_insert_with(|| f.clone());
                    }
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
                        // B-2026-08-13-8 — the name this method's SYMBOL
                        // takes. Normally the head (`Vec`); the qualified
                        // segment (`Vec[i64]`) when another impl in this program
                        // defines the same method on another instantiation of
                        // the same head. Emitting both as `Vec.describe` made
                        // LLVM rename the second and `get_function` hand the
                        // FIRST to every receiver — while the interpreter, over
                        // the same erased key, handed out the LAST.
                        //
                        // Only the SYMBOL moves. `type_name` stays the head
                        // below, because it also types the synthesized `self`
                        // param and resolves `-> Self`; a qualified string in
                        // either position would be a path segment naming no type.
                        let dispatch_head = crate::impl_dispatch::impl_dispatch_segment(
                            &imp.target_type,
                            &self.span_tables.impl_dispatch_names,
                        )
                        .unwrap_or_else(|| type_name.clone());
                        // A method is monomorphized on demand (registered in
                        // `generic_fns`, NOT eagerly `declare_function`'d) when
                        // it is generic via its OWN params (B-2026-07-03-15) OR
                        // via the IMPL's params (`impl[T] Box[T]`,
                        // B-2026-07-03-23 layer 4). The impl-generic case must
                        // route through the mono pipeline so the method is
                        // compiled with `self` typed at the RECEIVER's struct
                        // instantiation (`Box[f64]` → `{double}`); declaring the
                        // bare `Box.method` here would give it the all-`i64`
                        // default and the non-generic dispatch arm would find
                        // that wrong version first.
                        let impl_is_generic = imp.generic_params.is_some();
                        for impl_item in &imp.items {
                            if let ImplItem::Method(method) = impl_item {
                                if method.generic_params.is_some() || impl_is_generic {
                                    // Register generic impl/trait methods for
                                    // on-demand monomorphization at the call
                                    // site — mirrors the free-fn `generic_fns`
                                    // registration above. Keyed by the same
                                    // `Type.method` name a call site forms; the
                                    // synth prepends `self` (ref/owned) as param
                                    // 0 so the mono pipeline
                                    // (`compile_generic_call`) treats it exactly
                                    // like a generic free fn. Before this the
                                    // method was skipped entirely, so a call
                                    // `o.wrap[A](..)` fell through to the "no
                                    // handler for method" codegen error even
                                    // though `karac run` executed it correctly
                                    // (B-2026-07-03-15). For a method on a
                                    // GENERIC impl, `make_generic_impl_method_function`
                                    // types `self` as the impl's target expr
                                    // (`Box[T]`) and adds the impl's params to
                                    // the method's generic-param axis so the
                                    // receiver's instantiation binds `T`
                                    // (B-2026-07-03-23 layer 4). `.or_insert_with`
                                    // dedups across the value-self / ref-self
                                    // two-pass.
                                    let qualified = format!("{}.{}", type_name, method.name);
                                    self.mono_state.generic_fns.entry(qualified).or_insert_with(
                                        || {
                                            if impl_is_generic {
                                                make_generic_impl_method_function(imp, method)
                                            } else {
                                                make_impl_method_function(
                                                    &type_name,
                                                    method,
                                                    &imp.target_type,
                                                )
                                            }
                                        },
                                    );
                                    continue;
                                }
                                if method_self_is_value(method) != value_self_pass {
                                    continue;
                                }
                                let qualified = format!("{}.{}", dispatch_head, method.name);
                                if !declared_impl_methods.insert(qualified.clone()) {
                                    continue;
                                }
                                let mut synth =
                                    make_impl_method_function(&type_name, method, &imp.target_type);
                                // Carry the disambiguated symbol (see
                                // `dispatch_head`); a no-op unless this impl is
                                // in a colliding group.
                                synth.name.clone_from(&qualified);
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
        for tp in compiled_stdlib_programs(program) {
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
        // Heap-closure-env epic (B-2026-06-22-2): identify the functions whose
        // return value IS or OWNS a heap-env closure (directly / in a struct /
        // tuple / array / `Vec[Fn]`), so a `let r = <call to such a fn>`
        // binding registers the matching `FreeClosureEnv` cleanup(s). The four
        // producer-set fixpoints (base, then relay-of-aggregate, then
        // relay-of-container, then relay-of-Vec — order documented in
        // `EscapeAnalysis::compute`) live in `crate::closure_escape`, the
        // plain-AST module the `escaping_closure` check lint shares
        // (B-2026-08-16-13). `fn_asts` is fully populated by now — both
        // insert sites precede this snapshot.
        self.closure_state.escape =
            crate::closure_escape::EscapeAnalysis::compute(&self.fn_sig.fn_asts);
        for item in &program.items {
            if let Item::Function(f) = item {
                // Comptime-only fn — never emitted (B-2026-07-08-15 Layer 3);
                // see the declare pass above.
                if f.is_comptime {
                    continue;
                }
                if f.generic_params.is_none() {
                    if self.fn_sig.declare_only_fns.contains(&f.name) {
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
                        // B-2026-08-13-8 — the name this method's SYMBOL
                        // takes. Normally the head (`Vec`); the qualified
                        // segment (`Vec[i64]`) when another impl in this program
                        // defines the same method on another instantiation of
                        // the same head. Emitting both as `Vec.describe` made
                        // LLVM rename the second and `get_function` hand the
                        // FIRST to every receiver — while the interpreter, over
                        // the same erased key, handed out the LAST.
                        //
                        // Only the SYMBOL moves. `type_name` stays the head
                        // below, because it also types the synthesized `self`
                        // param and resolves `-> Self`; a qualified string in
                        // either position would be a path segment naming no type.
                        let dispatch_head = crate::impl_dispatch::impl_dispatch_segment(
                            &imp.target_type,
                            &self.span_tables.impl_dispatch_names,
                        )
                        .unwrap_or_else(|| type_name.clone());
                        // A method that is generic via its own params OR via the
                        // impl's params is compiled on demand by the mono
                        // pipeline (`compile_generic_call`), not eagerly here —
                        // the declaration pass registered it in `generic_fns`
                        // rather than `declare_function`'ing a bare version
                        // (B-2026-07-03-23 layer 4). Skip both.
                        let impl_is_generic = imp.generic_params.is_some();
                        for impl_item in &imp.items {
                            if let ImplItem::Method(method) = impl_item {
                                if method.generic_params.is_some() || impl_is_generic {
                                    continue;
                                }
                                if method_self_is_value(method) != value_self_pass {
                                    continue;
                                }
                                let qualified = format!("{}.{}", dispatch_head, method.name);
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
                                if self.fn_sig.declare_only_fns.contains(&qualified) {
                                    continue;
                                }
                                let mut synth =
                                    make_impl_method_function(&type_name, method, &imp.target_type);
                                synth.name.clone_from(&qualified);
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
        for tp in compiled_stdlib_programs(program) {
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
        // Fill the `__karac_static_init` body now that all function
        // bodies are compiled and every struct/enum type is registered.
        // `main`'s entry already emitted the forward `call` to it.
        self.finalize_module_binding_static_init();

        // C-ABI auto-destructors for boxed-return exports (additive-interop
        // Slice 4 Path B): one `karac_free_<name>(handle)` per
        // `pub extern "C" fn` returning `Vec[scalar]` / `String`, freeing
        // the buffer + the heap box. Emitted after all bodies so the box
        // shape is settled.
        self.emit_export_destructors(program)?;

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

        // Define the `__karac_write_console` wrapper body now that every
        // function — user + on-demand stdlib + wasm shims — is in, so its
        // `karac_par_run` / `karac_par_reduce` use-check is final (B-2026-06-15-2).
        self.finalize_write_console_wrapper();
        // Strictly after the above — it calls that wrapper (B-2026-07-30-9).
        self.finalize_write_console_line_wrapper();

        // Slice 4 structural self-check: every user + stdlib function is now
        // compiled, so the recorder holds codegen's full emitted-drop set.
        // Compare it against the oracle schedule armed at entry. Warn-only (a
        // diagnostic, not a build gate): closure / `spawn` / `par` captures are
        // the oracle's §7 conservative edge and may warn benignly.
        if let Some(schedule) = oracle_drop_schedule.take() {
            let recs = crate::codegen::drop_obs::take();
            self.verify_oracle_drop_coverage(program, &schedule, &recs);
        }

        // Level 2 crash diagnostics — Part 2: finalize DWARF debug info BEFORE
        // verify. The verifier validates debug metadata, and unresolved
        // temporaries / a missing finalize would make it reject the module.
        // No-op unless debug info is enabled.
        self.di_finalize();

        // B-2026-08-07-10 — `KARAC_FN_ALIGN=<bytes>`: force a minimum
        // alignment on every DEFINED function in the module.
        //
        // A MEASUREMENT LEVER, not a default. That row bisected a sticky 1.09x
        // regression on kata:170 to a commit with identical instruction count
        // AND identical binary size on both sides — the only difference being
        // that a new helper emitted ahead of `main` shifted it by 152 bytes.
        // The carrier is therefore code placement relative to fetch and
        // branch-predictor structures, and the row's stated next step is to
        // force alignment behind a lever and re-measure before deciding
        // whether alignment is worth its size cost corpus-wide.
        //
        // Applied here rather than at each function's creation so it covers
        // user, stdlib and shim functions uniformly, and after every one of
        // them exists. Declarations (no body) are skipped: alignment on an
        // external symbol is meaningless and LLVM would carry it into the
        // reference. Off unless the variable is set, so no default build
        // changes; a non-numeric or zero value is ignored rather than
        // diagnosed, matching the other `KARAC_*` levers.
        if let Some(align) = std::env::var("KARAC_FN_ALIGN")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .filter(|a| *a > 0 && a.is_power_of_two())
        {
            let mut f = self.module.get_first_function();
            while let Some(fv) = f {
                if fv.count_basic_blocks() > 0 {
                    fv.as_global_value().set_alignment(align);
                }
                f = fv.get_next_function();
            }
        }

        self.module.verify().map_err(|e| {
            // A verifier failure is otherwise a one-line ICE with no module to
            // inspect. `KARAC_DUMP_IR_ON_VERIFY_FAIL=<path>` writes the full
            // IR so cross-function references (the "Instruction does not
            // dominate all uses" class, e.g. B-2026-07-31-35) can be located.
            if let Ok(path) = std::env::var("KARAC_DUMP_IR_ON_VERIFY_FAIL") {
                let _ = std::fs::write(&path, self.module.print_to_string().to_string());
            }
            format!("Module verification failed: {}", e)
        })
    }

    /// Slice 4 structural self-check (`KARAC_ORACLE_DROP_CHECK`): report any
    /// place the ownership oracle schedules a drop for that codegen emitted no
    /// cleanup action for — a missing drop (leak), the exact class the
    /// structural fix must make impossible. Warn-only: this is the inline form
    /// of the `drop_fuzz --differential` gate, run on whatever real program is
    /// being compiled rather than the fuzzer corpus. Parameters are excluded
    /// (an owned heap param is freed caller-side, not at the callee — see
    /// `drop_differential`'s rule 2); closure / `spawn` / `par` captures are the
    /// oracle's §7 conservative edge and may warn benignly. Only the
    /// missing-drop direction is meaningful here — an extra codegen drop is not
    /// emit-time distinguishable from a runtime-guarded no-op.
    fn verify_oracle_drop_coverage(
        &self,
        program: &Program,
        schedule: &crate::ownership_oracle::OracleResult,
        recs: &[crate::codegen::drop_obs::DropRecord],
    ) {
        use std::collections::{HashMap, HashSet};

        let params = crate::drop_differential::param_names_by_function(program);
        let mut emitted: HashMap<&str, HashSet<&str>> = HashMap::new();
        for r in recs {
            emitted
                .entry(r.function.as_str())
                .or_default()
                .insert(r.place.as_str());
        }

        let mut missing = 0usize;
        for f in &schedule.functions {
            let no_params = HashSet::new();
            let fn_params = params.get(&f.function).unwrap_or(&no_params);
            let em = emitted.get(f.function.as_str());
            for d in &f.drops {
                if fn_params.contains(&d.place) {
                    continue;
                }
                if !em.is_some_and(|s| s.contains(d.place.as_str())) {
                    missing += 1;
                    eprintln!(
                        "karac[oracle-drop-check]: fn `{}` — oracle schedules a drop for `{}` \
                         ({}) that codegen emitted no cleanup for (possible leak, or a §7 \
                         closure/spawn/par capture edge)",
                        f.function, d.place, d.ty
                    );
                }
            }
        }
        if missing == 0 {
            eprintln!(
                "karac[oracle-drop-check]: OK — codegen's emitted cleanup covers the oracle's \
                 local drop schedule on every function"
            );
        } else {
            eprintln!("karac[oracle-drop-check]: {missing} uncovered scheduled drop(s)");
        }
    }

    /// Define the body of the internal `__karac_write_console` wrapper that
    /// every console write routes through (`emit_nul_safe_write` /
    /// `compile_print`). It calls the capture-capable runtime
    /// `karac_runtime_write_console` ONLY when the module emits a
    /// `karac_par_run` / `karac_par_reduce` call — `karac_par_run` is the sole
    /// installer of an `OutputCapture`, so its presence is exactly when a
    /// par-branch write must be captured and replayed in source order.
    /// Otherwise the wrapper does a lean libc `fwrite(data, 1, len, stream)`
    /// directly, so a non-parallel binary references neither the runtime
    /// chokepoint nor the `OutputCapture` machinery it transitively pulls, and
    /// AOT `-dead_strip`s the whole lot — restoring the lean binary-size floor
    /// `1a401c7b`'s blanket routing regressed by ~17 KiB on every output-bearing
    /// Emit a C-ABI auto-destructor for every boxed-return export
    /// (additive-interop Slice 4 Path B). See [`emit_one_export_destructor`].
    fn emit_export_destructors(&mut self, program: &Program) -> Result<(), String> {
        // `(fn_name, elems_need_drop, is_plain_box)`. A Vec/String box
        // (`boxed_return_of`) owns an inner `data` buffer to free (and, when
        // nested, per-element buffers). A Slice-2a tagged-union `#[repr(C)]`
        // enum box (`is_plain_box`) owns nothing but itself — its destructor
        // must NOT run the vec-buffer free, which would misread the payload
        // word as a `data` pointer and free garbage.
        let mut targets: Vec<(String, bool, bool)> = Vec::new();
        for it in &program.items {
            if let Item::Function(f) = it {
                if crate::cheader::boxed_return_of(f).is_some() {
                    targets.push((
                        f.name.clone(),
                        crate::cheader::boxed_return_elements_need_drop(f),
                        false,
                    ));
                } else if crate::cheader::export_return_is_boxed_enum(f, program) {
                    targets.push((f.name.clone(), false, true));
                }
            }
        }
        for (name, elems_need_drop, is_plain_box) in targets {
            self.emit_one_export_destructor(&name, elems_need_drop, is_plain_box);
        }
        Ok(())
    }

    /// `void karac_free_<fn>(handle)` — the auto-emitted destructor for a
    /// boxed-return export (Slice 4 Path B). The handle is the heap box
    /// holding the returned `{data,len,cap}` value. When `elems_need_drop`
    /// (a `Vec[String]` / `Vec[Vec[scalar]]` return, the Path-B follow-on),
    /// first walk `data[0..len]` freeing each element's own `{ptr,len,cap}`
    /// buffer; then free the outer buffer (`emit_free_vec_buffer_if_owned`)
    /// and the box. Null-guarded: `karac_free_<fn>(NULL)` is a no-op.
    ///
    /// `is_plain_box` (a Slice-2a tagged-union `#[repr(C)]` enum box) means the
    /// box owns nothing but itself — a `{ i64 tag, i64 w0 }` value with only
    /// scalar payloads. It frees the box directly and MUST skip the vec-buffer
    /// free, which would read the payload word as a `data` pointer and free it.
    fn emit_one_export_destructor(
        &mut self,
        fn_name: &str,
        elems_need_drop: bool,
        is_plain_box: bool,
    ) {
        use inkwell::module::Linkage;
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let void_ty = self.context.void_type();
        let free_name = format!("karac_free_{fn_name}");
        if self.module.get_function(&free_name).is_some() {
            return;
        }
        let fn_ty = void_ty.fn_type(&[ptr_ty.into()], false);
        let dtor = self
            .module
            .add_function(&free_name, fn_ty, Some(Linkage::External));

        let saved_bb = self.builder.get_insert_block();
        let saved_fn = self.current_fn;
        self.current_fn = Some(dtor);

        let entry = self.context.append_basic_block(dtor, "entry");
        let free_bb = self.context.append_basic_block(dtor, "free");
        let ret_bb = self.context.append_basic_block(dtor, "ret");
        self.builder.position_at_end(entry);
        let handle = dtor.get_nth_param(0).unwrap().into_pointer_value();
        // Null guard: skip all frees for a NULL handle.
        let is_null = self.builder.build_is_null(handle, "kfree.isnull").unwrap();
        self.builder
            .build_conditional_branch(is_null, ret_bb, free_bb)
            .unwrap();

        self.builder.position_at_end(free_bb);
        if is_plain_box {
            // Slice-2a tagged-union enum box: scalar payloads own no heap, so
            // there is no inner buffer to free — freeing the box is the whole
            // cleanup. Emphatically NOT `emit_free_vec_buffer_if_owned` (that
            // would treat the payload word as a `data` pointer).
        } else {
            // Nested return (`Vec[String]` / `Vec[Vec[scalar]]`): each element
            // is a `{ptr,len,cap}` aggregate — free each element's buffer
            // before the outer buffer, else the inner buffers leak.
            if elems_need_drop {
                self.emit_boxed_elems_drop_loop(handle);
            }
            // The box points to a `{data,len,cap}` value; free its owned
            // buffer (element size unknown at this generic handle path —
            // hint 0, the runtime asks the allocator).
            self.emit_free_vec_buffer_if_owned(handle, 0);
        }
        // Then free the box allocation itself.
        self.builder
            .build_call(self.runtime_fns.free_fn, &[handle.into()], "")
            .unwrap();
        self.builder.build_unconditional_branch(ret_bb).unwrap();

        self.builder.position_at_end(ret_bb);
        self.builder.build_return(None).unwrap();

        self.current_fn = saved_fn;
        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
    }

    /// Free the per-element `{ptr,len,cap}` buffers of a boxed nested Vec
    /// (`Vec[String]` / `Vec[Vec[scalar]]`) — `for i in 0..len { free
    /// data[i].buffer }`. `box_ptr` points to the outer `{data,len,cap}`;
    /// `data` (field 0) is the element array, `len` (field 1) the count.
    /// Each element is one `vec_struct_type` (24 B), so `data[i]` is a
    /// pointer to that element's own `{ptr,len,cap}` — reuse
    /// `emit_free_vec_buffer_if_owned` on it. Must run before the outer
    /// buffer is freed. One nesting level (the boxable set stops there).
    fn emit_boxed_elems_drop_loop(&mut self, box_ptr: PointerValue<'ctx>) {
        let vec_ty = self.vec_struct_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();
        let dtor = self.current_fn.unwrap();

        let data_pp = self
            .builder
            .build_struct_gep(vec_ty, box_ptr, 0, "kfree.data.pp")
            .unwrap();
        let data = self
            .builder
            .build_load(ptr_ty, data_pp, "kfree.data")
            .unwrap()
            .into_pointer_value();
        let len_pp = self
            .builder
            .build_struct_gep(vec_ty, box_ptr, 1, "kfree.len.pp")
            .unwrap();
        let len = self
            .builder
            .build_load(i64_t, len_pp, "kfree.len")
            .unwrap()
            .into_int_value();

        let idx_slot = self.builder.build_alloca(i64_t, "kfree.i").unwrap();
        self.builder
            .build_store(idx_slot, i64_t.const_zero())
            .unwrap();

        let check_bb = self.context.append_basic_block(dtor, "kfree.loop.check");
        let body_bb = self.context.append_basic_block(dtor, "kfree.loop.body");
        let after_bb = self.context.append_basic_block(dtor, "kfree.loop.after");
        self.builder.build_unconditional_branch(check_bb).unwrap();

        self.builder.position_at_end(check_bb);
        let i = self
            .builder
            .build_load(i64_t, idx_slot, "kfree.i.cur")
            .unwrap()
            .into_int_value();
        let cond = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SLT, i, len, "kfree.i.lt")
            .unwrap();
        self.builder
            .build_conditional_branch(cond, body_bb, after_bb)
            .unwrap();

        self.builder.position_at_end(body_bb);
        // `elem_ptr = &data[i]` (element stride = vec_struct_type size).
        let elem_ptr = unsafe {
            self.builder
                .build_in_bounds_gep(vec_ty, data, &[i], "kfree.elem")
                .unwrap()
        };
        // Free this element's own owned buffer (inner element size unknown
        // at this generic handle path — hint 0).
        self.emit_free_vec_buffer_if_owned(elem_ptr, 0);
        let next = self
            .builder
            .build_int_add(i, i64_t.const_int(1, false), "kfree.i.next")
            .unwrap();
        self.builder.build_store(idx_slot, next).unwrap();
        self.builder.build_unconditional_branch(check_bb).unwrap();

        self.builder.position_at_end(after_bb);
    }

    /// compute binary (B-2026-06-15-2). Idempotent; must run after all function
    /// bodies are compiled so the par use-check sees every site.
    fn finalize_write_console_wrapper(&mut self) {
        let wrapper = self.runtime_fns.write_console_fn;
        if wrapper.get_first_basic_block().is_some() {
            return;
        }
        let par_used = |name: &str| -> bool {
            self.module.get_function(name).is_some_and(|f| {
                inkwell::values::BasicValue::get_first_use(&f.as_global_value().as_pointer_value())
                    .is_some()
            })
        };
        // B-2026-08-17-14 — `karac_par_run_auto` is a par dispatch like the
        // other two: miss it here and every println in an auto-par program
        // lowers to raw fwrite, so concurrent branches race the fd instead
        // of capturing for ordered replay (measured: the map/set ASAN
        // fixtures' output interleaved non-deterministically).
        let needs_capture = par_used("karac_par_run")
            || par_used("karac_par_run_auto")
            || par_used("karac_par_reduce");

        let saved = self.builder.get_insert_block();
        let entry = self.context.append_basic_block(wrapper, "entry");
        self.builder.position_at_end(entry);
        let data = wrapper.get_nth_param(0).unwrap();
        let len = wrapper.get_nth_param(1).unwrap();
        let stream = wrapper.get_nth_param(2).unwrap();

        if needs_capture {
            let rt = self
                .module
                .get_function("karac_runtime_write_console")
                .expect("runtime write_console declared at setup");
            self.builder
                .build_call(rt, &[data.into(), len.into(), stream.into()], "")
                .unwrap();
        } else {
            // Lean path: `fwrite(data, 1, len, stream)` — `size` = 1,
            // `nmemb` = `len`, matching the chokepoint's fast path byte-for-byte.
            let size_t = len.into_int_value().get_type();
            let ptr_ty = self.context.ptr_type(AddressSpace::default());
            let fwrite_ty = size_t.fn_type(
                &[ptr_ty.into(), size_t.into(), size_t.into(), ptr_ty.into()],
                false,
            );
            let fwrite = self
                .module
                .get_function("fwrite")
                .unwrap_or_else(|| self.module.add_function("fwrite", fwrite_ty, None));
            let one = size_t.const_int(1, false);
            self.builder
                .build_call(
                    fwrite,
                    &[data.into(), one.into(), len.into(), stream.into()],
                    "",
                )
                .unwrap();
        }
        self.builder.build_return(None).unwrap();
        if let Some(bb) = saved {
            self.builder.position_at_end(bb);
        }
    }

    /// B-2026-07-30-9 — emit the body of `__karac_write_console_line`: stage
    /// `data[0..len]` and `nl[0..nl_len]` into one stack buffer and make a
    /// SINGLE `__karac_write_console` call, so a `println` reaches the OS as one
    /// write.
    ///
    /// The bug: `emit_nul_safe_write` wrote the payload, then the newline, as
    /// two calls. Whatever serializes a write — glibc's per-`FILE` lock inside
    /// `fwrite` — is released between them, so two `spawn`ed tasks printing
    /// concurrently interleaved as payload-A, payload-B, newline-A, newline-B:
    /// `12\n\n` where the program says `1\n2\n`. Measured on a five-task probe:
    /// 14 garbled runs in 60, down to 0 in 60.
    ///
    /// WHY THIS IS IN CODEGEN AND NOT THE RUNTIME, which is the non-obvious
    /// part. Giving `karac_runtime_write_console` a `trailing_newline` flag
    /// looks smaller and does not work: `finalize_write_console_wrapper` above
    /// inlines `fwrite` DIRECTLY whenever the program uses no `par`, so the
    /// runtime chokepoint is never called on exactly the `spawn`-only programs
    /// that flake. Staging here sits in front of both arms and fixes both.
    ///
    /// A fixed 4 KiB uninitialised alloca, not an allocation: this runs on every
    /// `println`, and a malloc/free pair per line would be a real cost on the
    /// print-heavy paths. One function-scoped slot is reused by every call.
    /// A line that does not fit falls back to the original two writes — still
    /// correct, just not atomic, and >4 KiB single-line output is not the
    /// interleaving case anyone hits.
    ///
    /// Must run AFTER `finalize_write_console_wrapper`: it calls that wrapper,
    /// whose own body decides inline-`fwrite` vs capture based on whether `par`
    /// is used anywhere, and that decision has to be made first.
    fn finalize_write_console_line_wrapper(&mut self) {
        let wrapper = self.runtime_fns.write_console_line_fn;
        if wrapper.get_first_basic_block().is_some() {
            return;
        }
        const STAGE_CAP: u64 = 4096;

        let saved = self.builder.get_insert_block();
        let entry = self.context.append_basic_block(wrapper, "entry");
        let fits_bb = self.context.append_basic_block(wrapper, "fits");
        let split_bb = self.context.append_basic_block(wrapper, "split");

        self.builder.position_at_end(entry);
        let data = wrapper.get_nth_param(0).unwrap().into_pointer_value();
        let len = wrapper.get_nth_param(1).unwrap().into_int_value();
        let nl = wrapper.get_nth_param(2).unwrap().into_pointer_value();
        let nl_len = wrapper.get_nth_param(3).unwrap().into_int_value();
        let stream = wrapper.get_nth_param(4).unwrap();

        let size_t = len.get_type();
        let total = self
            .builder
            .build_int_add(len, nl_len, "wcl.total")
            .unwrap();
        let cap = size_t.const_int(STAGE_CAP, false);
        let fits = self
            .builder
            .build_int_compare(inkwell::IntPredicate::ULE, total, cap, "wcl.fits")
            .unwrap();
        self.builder
            .build_conditional_branch(fits, fits_bb, split_bb)
            .unwrap();

        // Staged: memcpy payload, memcpy newline, one write.
        self.builder.position_at_end(fits_bb);
        let buf = self
            .builder
            .build_alloca(
                self.context.i8_type().array_type(STAGE_CAP as u32),
                "wcl.buf",
            )
            .unwrap();
        self.builder.build_memcpy(buf, 1, data, 1, len).unwrap();
        let tail = unsafe {
            self.builder
                .build_in_bounds_gep(self.context.i8_type(), buf, &[len], "wcl.tail")
                .unwrap()
        };
        self.builder.build_memcpy(tail, 1, nl, 1, nl_len).unwrap();
        self.builder
            .build_call(
                self.runtime_fns.write_console_fn,
                &[buf.into(), total.into(), stream.into()],
                "",
            )
            .unwrap();
        self.builder.build_return(None).unwrap();

        // Oversized: the original two writes. Not line-atomic, but correct.
        self.builder.position_at_end(split_bb);
        self.builder
            .build_call(
                self.runtime_fns.write_console_fn,
                &[data.into(), len.into(), stream.into()],
                "",
            )
            .unwrap();
        self.builder
            .build_call(
                self.runtime_fns.write_console_fn,
                &[nl.into(), nl_len.into(), stream.into()],
                "",
            )
            .unwrap();
        self.builder.build_return(None).unwrap();

        if let Some(bb) = saved {
            self.builder.position_at_end(bb);
        }
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
        let prev_declaring_stdlib = self.declaring_stdlib_program;
        self.declaring_stdlib_program = true;
        let r = self.declare_stdlib_program_inner(tp);
        self.declaring_stdlib_program = prev_declaring_stdlib;
        r
    }

    fn declare_stdlib_program_inner(&mut self, tp: &Program) -> Result<(), String> {
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
                                let synth =
                                    make_impl_method_function(&type_name, method, &imp.target_type);
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
        let mut t_question_ok_payload_types = tp.question_ok_payload_types.clone();
        let mut t_wp_result_types = tp.wp_result_types.clone();
        let mut t_callee_effectful = tp.callee_effectful.clone();
        let mut t_method_callee_types = tp.method_callee_types.clone();
        let mut t_string_typed_exprs = tp.string_typed_exprs.clone();
        let mut t_borrow_vec_typed_exprs = tp.borrow_vec_typed_exprs.clone();
        let mut t_unsigned_vector_exprs = tp.unsigned_vector_exprs.clone();
        let mut t_unsigned_int_exprs = tp.unsigned_int_exprs.clone();
        let mut t_cast_source_unsigned = tp.cast_source_unsigned.clone();
        let mut t_vector_method_call_spans = tp.vector_method_call_spans.clone();
        let mut t_expr_struct_type_names = tp.expr_struct_type_names.clone();
        let mut t_user_ord_typed_exprs = tp.user_ord_typed_exprs.clone();
        let mut t_owned_temp_drops = tp.owned_temp_drops.clone();
        let mut t_raw_pointer_pointee_types = tp.raw_pointer_pointee_types.clone();
        let mut t_enum_inst_type_exprs = tp.enum_inst_type_exprs.clone();
        let mut t_call_effect_subs = tp.call_effect_subs.clone();
        let mut t_method_unwrap_inner_types = tp.method_unwrap_inner_types.clone();
        let mut t_method_unwrap_err_types = tp.method_unwrap_err_types.clone();
        let mut t_temp_recv_elem_types = tp.temp_recv_elem_types.clone();
        let mut t_index_recv_vec_types = tp.index_recv_vec_types.clone();
        let mut t_temp_recv_mapset_types = tp.temp_recv_mapset_types.clone();
        let mut t_temp_recv_len_elem_types = tp.temp_recv_len_elem_types.clone();
        let mut t_channel_elem_types = tp.channel_elem_types.clone();
        let mut t_ref_return_inner_types = tp.ref_return_inner_types.clone();
        let mut t_secret_inner_types = tp.secret_inner_types.clone();
        let mut t_display_option_result_types = tp.display_option_result_types.clone();
        let mut t_display_tuple_types = tp.display_tuple_types.clone();
        let mut t_display_generic_enum_types = tp.display_generic_enum_types.clone();
        let mut t_display_vec_types = tp.display_vec_types.clone();
        let mut t_pattern_binding_types = tp.pattern_binding_types.clone();
        let mut t_pattern_binding_inner_types = tp.pattern_binding_inner_types.clone();
        let mut t_pattern_binding_borrow_modes = tp.pattern_binding_borrow_modes.clone();
        macro_rules! swap_all {
            () => {{
                std::mem::swap(
                    &mut self.span_tables.question_conversions,
                    &mut t_question_conversions,
                );
                std::mem::swap(
                    &mut self.span_tables.question_ok_payload_types,
                    &mut t_question_ok_payload_types,
                );
                std::mem::swap(
                    &mut self.span_tables.wp_result_types,
                    &mut t_wp_result_types,
                );
                std::mem::swap(&mut self.fn_sig.callee_effectful, &mut t_callee_effectful);
                std::mem::swap(
                    &mut self.span_tables.method_callee_types,
                    &mut t_method_callee_types,
                );
                std::mem::swap(
                    &mut self.span_tables.string_typed_exprs,
                    &mut t_string_typed_exprs,
                );
                std::mem::swap(
                    &mut self.span_tables.borrow_vec_typed_exprs,
                    &mut t_borrow_vec_typed_exprs,
                );
                std::mem::swap(
                    &mut self.span_tables.unsigned_vector_exprs,
                    &mut t_unsigned_vector_exprs,
                );
                std::mem::swap(
                    &mut self.span_tables.unsigned_int_exprs,
                    &mut t_unsigned_int_exprs,
                );
                std::mem::swap(
                    &mut self.span_tables.cast_source_unsigned,
                    &mut t_cast_source_unsigned,
                );
                std::mem::swap(
                    &mut self.span_tables.vector_method_call_spans,
                    &mut t_vector_method_call_spans,
                );
                std::mem::swap(
                    &mut self.span_tables.expr_struct_type_names,
                    &mut t_expr_struct_type_names,
                );
                std::mem::swap(
                    &mut self.span_tables.user_ord_typed_exprs,
                    &mut t_user_ord_typed_exprs,
                );
                std::mem::swap(&mut self.drop_rc.owned_temp_drops, &mut t_owned_temp_drops);
                std::mem::swap(
                    &mut self.span_tables.raw_pointer_pointee_types,
                    &mut t_raw_pointer_pointee_types,
                );
                std::mem::swap(
                    &mut self.type_decls.enum_inst_type_exprs,
                    &mut t_enum_inst_type_exprs,
                );
                std::mem::swap(
                    &mut self.span_tables.call_effect_subs,
                    &mut t_call_effect_subs,
                );
                std::mem::swap(
                    &mut self.span_tables.method_unwrap_inner_types,
                    &mut t_method_unwrap_inner_types,
                );
                std::mem::swap(
                    &mut self.span_tables.method_unwrap_err_types,
                    &mut t_method_unwrap_err_types,
                );
                std::mem::swap(
                    &mut self.span_tables.temp_recv_elem_types,
                    &mut t_temp_recv_elem_types,
                );
                std::mem::swap(
                    &mut self.span_tables.index_recv_vec_types,
                    &mut t_index_recv_vec_types,
                );
                std::mem::swap(
                    &mut self.mapset.temp_recv_mapset_types,
                    &mut t_temp_recv_mapset_types,
                );
                std::mem::swap(
                    &mut self.span_tables.temp_recv_len_elem_types,
                    &mut t_temp_recv_len_elem_types,
                );
                std::mem::swap(&mut self.conc.channel_elem_types, &mut t_channel_elem_types);
                std::mem::swap(
                    &mut self.ref_return_inner_types,
                    &mut t_ref_return_inner_types,
                );
                std::mem::swap(
                    &mut self.contract_state.secret_inner_types,
                    &mut t_secret_inner_types,
                );
                std::mem::swap(
                    &mut self.display.display_option_result_types,
                    &mut t_display_option_result_types,
                );
                std::mem::swap(
                    &mut self.display.display_tuple_types,
                    &mut t_display_tuple_types,
                );
                std::mem::swap(
                    &mut self.display.display_generic_enum_types,
                    &mut t_display_generic_enum_types,
                );
                std::mem::swap(
                    &mut self.display.display_vec_types,
                    &mut t_display_vec_types,
                );
                std::mem::swap(
                    &mut self.pattern_state.pattern_binding_types,
                    &mut t_pattern_binding_types,
                );
                std::mem::swap(
                    &mut self.pattern_state.pattern_binding_inner_types,
                    &mut t_pattern_binding_inner_types,
                );
                std::mem::swap(
                    &mut self.pattern_state.pattern_binding_borrow_modes,
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
                                if self.fn_sig.declare_only_fns.contains(&qualified) {
                                    continue;
                                }
                                let synth =
                                    make_impl_method_function(&type_name, method, &imp.target_type);
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
        if !self.conc.hot_swap_enabled || self.conc.hot_swap_fns.is_empty() {
            return;
        }
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let n = self.conc.hot_swap_fns.len() as u32;
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
        if !self.conc.hot_swap_enabled || self.conc.hot_swap_fns.is_empty() {
            return;
        }
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i32_ty = self.context.i32_type();
        let i64_ty = self.context.i64_type();
        let n = self.conc.hot_swap_fns.len() as u32;
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
        for (slot, fn_val) in self.conc.hot_swap_fns.clone() {
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

    /// B-2026-08-07-10 — `KARAC_LLVM_ARGS="<flags>"`: forward raw LLVM
    /// `cl::opt` flags into the process-global option registry, the way
    /// clang's `-mllvm` does.
    ///
    /// A MEASUREMENT LEVER. Backend knobs that exist only as `cl::opt` — the
    /// block/loop alignment family this row needs
    /// (`-align-all-nofallthru-blocks=<log2>`) among them — are unreachable
    /// from the IR-building API that codegen otherwise uses, so without this
    /// the question "does aligning the hot loop remove the placement penalty"
    /// cannot be asked at all.
    ///
    /// Deliberately unvalidated input: LLVM prints its own diagnostic and
    /// exits the process on an unknown flag. That is acceptable for a lever
    /// nothing sets by default and is the same contract clang offers for
    /// `-mllvm`. `Once` because the registry rejects a second parse.
    fn apply_llvm_cl_args() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            let Ok(raw) = std::env::var("KARAC_LLVM_ARGS") else {
                return;
            };
            let args: Vec<std::ffi::CString> = std::iter::once("karac")
                .chain(raw.split_whitespace())
                .filter_map(|s| std::ffi::CString::new(s).ok())
                .collect();
            let ptrs: Vec<*const std::ffi::c_char> = args.iter().map(|c| c.as_ptr()).collect();
            unsafe {
                llvm_sys::support::LLVMParseCommandLineOptions(
                    ptrs.len() as i32,
                    ptrs.as_ptr(),
                    std::ptr::null(),
                );
            }
        });
    }

    /// B-2026-08-07-10 — emit the `KARAC_TEXT_PAD` filler function.
    ///
    /// A MEASUREMENT LEVER, not a default; see the call site in
    /// `compile_program` for why placement needs a continuous lever and not
    /// just `KARAC_FN_ALIGN`'s quantised one. A non-numeric, zero, or absent
    /// value is a no-op rather than a diagnostic, matching the other `KARAC_*`
    /// levers.
    ///
    /// The body is one side-effecting `.space <n>, 0` inline-asm blob and a
    /// `ret`. Inline asm because it is the only construct whose emitted byte
    /// count the optimizer cannot change: any IR-level filler (dead
    /// arithmetic, an unused array) is exactly what LLVM exists to delete, and
    /// the row already recorded that a plain dead function gets eliminated.
    fn emit_text_pad(&mut self) -> Result<(), String> {
        let Some(bytes) = std::env::var("KARAC_TEXT_PAD")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .filter(|n| *n > 0)
        else {
            return Ok(());
        };
        let fn_ty = self.context.void_type().fn_type(&[], false);
        let pad = self.module.add_function("__karac_text_pad", fn_ty, None);
        // B-2026-08-08-26 — being FIRST IN MODULE ORDER is not enough on ELF.
        // The pad is added before any user function (see the `compile_program`
        // call site), which is what makes it precede `main` on Mach-O, where the
        // row's measurements were taken. On Linux it did not: `main` sat at
        // 0x1280 and the pad at 0x1300, so `KARAC_TEXT_PAD=16` and `=80`
        // produced byte-identical `main` addresses and the lever measured
        // NOTHING while still looking like it worked (the filler is present, the
        // right size, and pinned into `@llvm.used`).
        //
        // The cause is GNU ld's built-in linker script, which places
        // `*(.text.startup)` AHEAD of `*(.text .text.*)` — and LLVM emits `main`
        // into `.text.startup`. Module order never got a say: the pad's
        // `.text.<name>` section was in a later output-section group no matter
        // when it was added. Putting the pad in `.text.startup` too puts both in
        // one group, where input order does decide, so the pad lands first and
        // `main` shifts by exactly the requested bytes (measured: 0x12a0 →
        // 0x12e0 for 16 → 80).
        //
        // Linux-only: Mach-O has no `.text.startup` and already orders this
        // correctly, so naming an ELF section there would be wrong.
        if cfg!(target_os = "linux") {
            pad.as_global_value().set_section(Some(".text.startup"));
        }
        let entry = self.context.append_basic_block(pad, "entry");
        let saved = self.builder.get_insert_block();
        self.builder.position_at_end(entry);
        let asm = self.context.create_inline_asm(
            fn_ty,
            format!(".space {bytes}, 0"),
            String::new(),
            true,  // side effects — never optimize the filler away
            false, // no stack alignment
            None,  // default dialect
            false, // cannot throw
        );
        self.builder
            .build_indirect_call(fn_ty, asm, &[], "")
            .map_err(|e| format!("codegen: KARAC_TEXT_PAD asm call failed: {e}"))?;
        self.builder
            .build_return(None)
            .map_err(|e| format!("codegen: KARAC_TEXT_PAD return failed: {e}"))?;
        if let Some(bb) = saved {
            self.builder.position_at_end(bb);
        }
        self.used_symbols.push(pad);
        Ok(())
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
            self.conc.spawn_sites.len() as u32
        } else {
            0
        };

        // Build the per-filename `i8`-array globals (one per distinct
        // file path) and remember each as a pointer-to-first-byte.
        let mut file_globals: HashMap<String, PointerValue<'ctx>> = HashMap::new();
        if emit_entries {
            for record in &self.conc.spawn_sites {
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
            self.conc
                .spawn_sites
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
            if let Some(&heap_type) = self.drop_rc.rc_fallback_heap_types.get(name) {
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
            if let Some(&inner_ty) = self.borrow_vars.ref_params.get(name) {
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
            if let Some(&heap_type) = self.drop_rc.rc_fallback_heap_types.get(name) {
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
            if self.borrow_vars.ref_params.contains_key(name) {
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
        self.mod_bindings
            .module_bindings
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

/// Collect the names of free fns passed as the WS-handler (third) argument of
/// any `Server.serve_ws(addr, handler, ws_handler)` / `Server.serve_ws_tls(addr,
/// cert, key, handler, ws_handler)` call in the program —
/// both the `MethodCall`-on-`Server` and `Call(Path([Server, serve_ws]))`
/// shapes. These fns are invoked through the runtime's `extern "C" fn(i64)`
/// callback on a dedicated blocking thread, so they are excluded from
/// `coro_fn_keys` (like `main`) and compile on the thread-block park path. A
/// call shape the walk misses fails LOUDLY downstream (module verification —
/// arity mismatch at the shim call), never silently.
fn collect_serve_ws_handler_names(program: &Program) -> std::collections::HashSet<String> {
    use std::collections::HashSet;
    fn scan_expr(e: &Expr, out: &mut HashSet<String>) {
        // Record a serve_ws third-arg identifier at this node, then recurse.
        match &e.kind {
            ExprKind::MethodCall {
                object,
                method,
                args,
                ..
            } => {
                if matches!(&object.kind, ExprKind::Identifier(n) if n == "Server") {
                    // `serve_ws(addr, handler, ws_handler)` → arg 2;
                    // `serve_ws_tls(addr, cert, key, handler, ws_handler)` → arg 4.
                    let ws_arg_idx = match method.as_str() {
                        "serve_ws" => Some(2),
                        "serve_ws_tls" => Some(4),
                        _ => None,
                    };
                    if let Some(idx) = ws_arg_idx {
                        if let Some(arg) = args.get(idx) {
                            if let ExprKind::Identifier(n) = &arg.value.kind {
                                out.insert(n.clone());
                            }
                        }
                    }
                }
                scan_expr(object, out);
                for a in args {
                    scan_expr(&a.value, out);
                }
            }
            ExprKind::Call { callee, args } => {
                if let ExprKind::Path { segments, .. } = &callee.kind {
                    if segments.len() == 2 && segments[0] == "Server" {
                        let ws_arg_idx = match segments[1].as_str() {
                            "serve_ws" => Some(2),
                            "serve_ws_tls" => Some(4),
                            _ => None,
                        };
                        if let Some(idx) = ws_arg_idx {
                            if let Some(arg) = args.get(idx) {
                                if let ExprKind::Identifier(n) = &arg.value.kind {
                                    out.insert(n.clone());
                                }
                            }
                        }
                    }
                }
                scan_expr(callee, out);
                for a in args {
                    scan_expr(&a.value, out);
                }
            }
            ExprKind::Block(b) | ExprKind::Comptime(b) | ExprKind::LabeledBlock { body: b, .. } => {
                scan_block(b, out)
            }
            ExprKind::If {
                condition,
                then_block,
                else_branch,
            } => {
                scan_expr(condition, out);
                scan_block(then_block, out);
                if let Some(e2) = else_branch {
                    scan_expr(e2, out);
                }
            }
            ExprKind::IfLet {
                value,
                then_block,
                else_branch,
                ..
            } => {
                scan_expr(value, out);
                scan_block(then_block, out);
                if let Some(e2) = else_branch {
                    scan_expr(e2, out);
                }
            }
            ExprKind::Match { scrutinee, arms } => {
                scan_expr(scrutinee, out);
                for arm in arms {
                    if let Some(g) = &arm.guard {
                        scan_expr(g, out);
                    }
                    scan_expr(&arm.body, out);
                }
            }
            ExprKind::While {
                condition, body, ..
            } => {
                scan_expr(condition, out);
                scan_block(body, out);
            }
            ExprKind::WhileLet { value, body, .. } => {
                scan_expr(value, out);
                scan_block(body, out);
            }
            ExprKind::For { iterable, body, .. } => {
                scan_expr(iterable, out);
                scan_block(body, out);
            }
            ExprKind::Loop { body, .. } => scan_block(body, out),
            ExprKind::Closure { body, .. } => scan_expr(body, out),
            ExprKind::Return(Some(inner)) => scan_expr(inner, out),
            ExprKind::Binary { left, right, .. } => {
                scan_expr(left, out);
                scan_expr(right, out);
            }
            ExprKind::Unary { operand, .. } => scan_expr(operand, out),
            ExprKind::Question(inner)
            | ExprKind::FieldAccess { object: inner, .. }
            | ExprKind::TupleIndex { object: inner, .. } => scan_expr(inner, out),
            ExprKind::OptionalChain { object, args, .. } => {
                scan_expr(object, out);
                if let Some(args) = args {
                    for a in args {
                        scan_expr(&a.value, out);
                    }
                }
            }
            ExprKind::NilCoalesce { left, right } => {
                scan_expr(left, out);
                scan_expr(right, out);
            }
            ExprKind::Index { object, index } => {
                scan_expr(object, out);
                scan_expr(index, out);
            }
            ExprKind::Tuple(items) | ExprKind::ArrayLiteral(items) => {
                for it in items {
                    scan_expr(it, out);
                }
            }
            _ => {}
        }
    }
    fn scan_block(b: &Block, out: &mut HashSet<String>) {
        for s in &b.stmts {
            match &s.kind {
                StmtKind::Let { value, .. } => scan_expr(value, out),
                StmtKind::LetElse {
                    value, else_block, ..
                } => {
                    scan_expr(value, out);
                    scan_block(else_block, out);
                }
                StmtKind::Assign { target, value } => {
                    scan_expr(target, out);
                    scan_expr(value, out);
                }
                StmtKind::CompoundAssign { target, value, .. } => {
                    scan_expr(target, out);
                    scan_expr(value, out);
                }
                StmtKind::Expr(e) => scan_expr(e, out),
                StmtKind::Defer { body: b2 } | StmtKind::ErrDefer { body: b2, .. } => {
                    scan_block(b2, out)
                }
                _ => {}
            }
        }
        if let Some(fe) = &b.final_expr {
            scan_expr(fe, out);
        }
    }
    let mut out = HashSet::new();
    for item in &program.items {
        if let Item::Function(f) = item {
            scan_block(&f.body, &mut out);
        }
    }
    out
}
