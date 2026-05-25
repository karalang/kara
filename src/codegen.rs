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
use crate::ownership::{CapturePath, OwnershipCheckResult, OwnershipMode};
use crate::resolver::SpanKey;
use crate::token::Span;

mod assoc_call;
mod call_dispatch;
mod calls;
mod clone_drop;
mod closures;
mod collections;
mod control_flow;
mod control_flow_bce;
mod control_flow_for;
mod control_flow_match;
mod control_flow_slice;
mod declarations;
mod driver;
mod entry_chains;
mod expr_ops;
mod exprs;
mod functions;
mod helpers;
mod http;
mod json;
mod maps;
mod method_call;
mod module_bindings;
mod mono;
mod par_blocks;
mod pattern_binding;
mod provider;
mod reduce;
mod runtime;
mod state;
mod stmts;
mod synth;
mod synth_display;
mod synth_drop;
mod types_lowering;
mod vec_method;

use driver::{
    apply_optimization_passes, create_target_machine, read_auto_par_env,
    read_runtime_debug_metadata_env,
};
pub use driver::{link_executable, link_executable_with_sanitizer};
use helpers::{impl_target_name, make_impl_method_function, method_self_is_value};
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

/// Compile a Kāra program to a native object file.
pub fn compile_to_object(
    program: &Program,
    output_path: &str,
    ownership: Option<&OwnershipCheckResult>,
    concurrency: Option<&ConcurrencyAnalysis>,
) -> Result<(), String> {
    compile_to_object_with_options(program, output_path, ownership, concurrency, None, None)
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
    )
}

/// Variant of [`compile_to_object_with_options`] that accepts the
/// phase-7 line-5 `--enable-hot-swap` flag. See
/// [`compile_to_ir_with_hot_swap`] for the codegen contract.
pub fn compile_to_object_with_hot_swap(
    program: &Program,
    output_path: &str,
    ownership: Option<&OwnershipCheckResult>,
    concurrency: Option<&ConcurrencyAnalysis>,
    source_filename: Option<&str>,
    source_text: Option<&str>,
    enable_hot_swap: bool,
) -> Result<(), String> {
    let context = Context::create();
    let mut cg = Codegen::new(&context, "karac_module");
    cg.load_rc_fallback(ownership);
    cg.load_concurrency_analysis(concurrency);
    cg.set_source_filename(source_filename);
    cg.set_source_text(source_text);
    cg.set_hot_swap_enabled(enable_hot_swap);
    cg.compile_program(program)?;

    let target_machine = create_target_machine()?;
    apply_optimization_passes(&cg.module, &target_machine)?;
    target_machine
        .write_to_file(&cg.module, FileType::Object, Path::new(output_path))
        .map_err(|e| format!("Failed to write object file: {}", e))
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
    pub(crate) current_fn: Option<FunctionValue<'ctx>>,
    pub(crate) printf_fn: FunctionValue<'ctx>,
    /// `int snprintf(char* buf, size_t n, const char* fmt, ...)` — used by f-string
    /// codegen to convert integers and floats to their decimal string forms.
    pub(crate) snprintf_fn: FunctionValue<'ctx>,
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
    /// SoA layout metadata (layout name → SoaLayout).
    pub(crate) soa_layouts: HashMap<String, SoaLayout>,
    /// Function parameter ref-ness (function name → vec of is_ref per param).
    pub(crate) fn_param_ref: HashMap<String, Vec<bool>>,
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
    /// Per-scope cleanup stack.  Each inner `Vec` is one scope frame; entries
    /// are emitted in reverse-push order at scope exit (innermost first).
    pub(crate) scope_cleanup_actions: Vec<Vec<CleanupAction<'ctx>>>,
    /// Set by `compile_match` when the scrutinee is a borrow-returning
    /// call (`Map.get`, `Vec.first`, ...) — used by `bind_pattern_values`
    /// to suppress `track_vec_var` for the bound name, since the payload
    /// aliases the container's storage and the container's own cleanup
    /// already covers the buffer.
    pub(crate) pattern_binding_is_borrow: bool,
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
    /// Per-function parallelization decisions populated from `ConcurrencyAnalysis`.
    /// Function name → `FunctionConcurrency` (parallel groups + total stmt count).
    /// Threaded in by `load_concurrency_analysis`; consumed in slice 2 by the
    /// auto-par lowering path that emits `karac_par_run` for inferred groups
    /// outside explicit `par {}` blocks. Empty when no analysis was supplied.
    pub(crate) concurrency_decisions: HashMap<String, FunctionConcurrency>,
    /// Name of the function currently being compiled (for rc_fallback_fns lookup).
    pub(crate) current_fn_name: String,
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
    pub(crate) karac_map_get_fn: FunctionValue<'ctx>,
    pub(crate) karac_map_remove_old_fn: FunctionValue<'ctx>,
    pub(crate) karac_map_contains_fn: FunctionValue<'ctx>,
    pub(crate) karac_map_len_fn: FunctionValue<'ctx>,
    pub(crate) karac_map_clear_fn: FunctionValue<'ctx>,
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
    /// Per-type clone function cache. Keyed on the canonical mangled type
    /// name (`display_mangle_te`). Each emitted fn has signature
    /// `void karac_clone_<typename>(*const T src, *mut T dst)` — caller
    /// provides both source and destination addresses, callee writes the
    /// cloned value into the destination slot. Mirror of `display_fn_cache`.
    pub(crate) clone_fn_cache: HashMap<String, FunctionValue<'ctx>>,
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

        // Declare malloc and free for RC heap allocation.
        let malloc_type = ptr_type.fn_type(&[BasicMetadataTypeEnum::from(i64_type)], false);
        let malloc_fn = module.add_function("malloc", malloc_type, Some(Linkage::External));
        let free_type = context
            .void_type()
            .fn_type(&[BasicMetadataTypeEnum::from(ptr_type)], false);
        let free_fn = module.add_function("free", free_type, Some(Linkage::External));

        let exit_type = context
            .void_type()
            .fn_type(&[BasicMetadataTypeEnum::from(i32_type)], false);
        let exit_fn = module.add_function("exit", exit_type, Some(Linkage::External));

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

        Codegen {
            context,
            module,
            builder,
            variables: HashMap::new(),
            var_type_names: HashMap::new(),
            current_fn: None,
            printf_fn,
            snprintf_fn,
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
            fn_param_ref: HashMap::new(),
            fn_return_type_names: HashMap::new(),
            fn_return_option_inner_shared: HashMap::new(),
            var_option_shared_heap: HashMap::new(),
            soa_layouts: HashMap::new(),
            scope_cleanup_actions: Vec::new(),
            pattern_binding_is_borrow: false,
            enum_drop_fns: HashMap::new(),
            struct_drop_fns: HashMap::new(),
            rc_drop_fns: HashMap::new(),
            question_conversions: HashMap::new(),
            callee_effectful: HashMap::new(),
            method_callee_types: HashMap::new(),
            call_effect_subs: crate::ast::CallEffectSubsTable::new(),
            method_unwrap_inner_types: HashMap::new(),
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
            arc_fallback_fns: HashMap::new(),
            rc_fallback_heap_types: HashMap::new(),
            closure_capture_paths: HashMap::new(),
            concurrency_decisions: HashMap::new(),
            current_fn_name: String::new(),
            par_counter: 0,
            karac_branch_ty,
            karac_par_run_fn,
            karac_par_reduce_fn,
            spawn_sites: Vec::new(),
            runtime_debug_metadata_enabled: read_runtime_debug_metadata_env(),
            auto_par_disabled: !read_auto_par_env(),
            provider_resource_ids: HashMap::new(),
            provider_resource_traits: HashMap::new(),
            provider_trait_methods: HashMap::new(),
            provider_vtables: HashMap::new(),
            karac_provider_push_fn,
            karac_provider_pop_fn,
            karac_provider_lookup_fn,
            karac_provider_get_stack_head_fn,
            karac_provider_set_stack_head_fn,
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
            http_shim_cache: HashMap::new(),
            karac_map_new_fn,
            karac_map_free_fn,
            karac_map_free_with_drop_vec_fn,
            karac_map_insert_old_fn,
            karac_map_get_fn,
            karac_map_remove_old_fn,
            karac_map_contains_fn,
            karac_map_len_fn,
            karac_map_clear_fn,
            karac_map_iter_new_fn,
            karac_map_iter_next_fn,
            karac_map_iter_free_fn,
            karac_string_decode_char_fn,
            karac_string_encode_char_fn,
            karac_map_entry_fn,
            karac_map_lookup_slot_fn,
            karac_string_clone_fn,
            clone_fn_cache: HashMap::new(),
            drop_fn_cache: HashMap::new(),
            map_mono_methods: HashMap::new(),
            display_fn_cache: HashMap::new(),
            karac_error_trace_push_fn,
            karac_error_trace_clear_fn,
            target_data: None,
            hot_swap_enabled: false,
            hot_swap_slots: HashMap::new(),
            hot_swap_fns: Vec::new(),
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
        // Disjoint-capture slice 4: per-closure capture-path mode set
        // (slice 2 output). Drives the per-path env-struct layout in
        // `compile_closure` when the closure expression's `SpanKey` is
        // present in this map; absent → per-name fallback.
        for (k, v) in &ow.closure_capture_path_modes {
            self.closure_capture_paths.insert(*k, v.clone());
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

    fn compile_program(&mut self, program: &Program) -> Result<(), String> {
        // Seed `Option` / `Result` layouts before walking struct fields so
        // a `shared struct N { mut left: Option[N] }` declaration's field-
        // type lowering finds the `{i64 tag, i64 payload}` layout via
        // `llvm_type_for_name("Option")` and embeds a 2-word slot in the
        // heap struct, rather than collapsing the field to the default
        // `i64` and losing the payload word.
        self.seed_builtin_enum_layouts();
        self.declare_structs(program);
        // Phase 5 line 569 slice 4: lower `#[repr(C)] union Foo { ... }`
        // declarations to LLVM storage types so `size_of[Foo]` /
        // `align_of[Foo]` resolve correctly and union literals /
        // field accesses can target the storage struct downstream.
        // Runs after `declare_structs` so a union field whose type
        // names a user struct resolves to the right LLVM aggregate
        // when computing primary-field alignment.
        self.declare_unions(program);
        self.declare_enums(program);
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

        // Theme 6: emit static vtables for impls of provider traits.
        // Runs after impl methods are *declared* (their fn-ptrs become
        // vtable entries) but BEFORE function bodies are compiled — body
        // compilation may include `with_provider[R]` call sites that
        // need the vtable global to already exist in `provider_vtables`
        // for the lookup at sub-step 3 lowering time. Bodies don't need
        // to be compiled yet because the vtable only references fn-ptr
        // symbols which were established by `declare_function`.
        self.emit_provider_vtables(program);

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

        // Second pass: compile concrete functions (generic ones are compiled lazily).
        for item in &program.items {
            if let Item::Function(f) = item {
                if f.generic_params.is_none() {
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
                                if !compiled_impl_methods.insert(qualified) {
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

        self.emit_jit_template_section();
        self.emit_llvm_used();
        self.emit_spawn_sites_metadata();
        self.finalize_hot_swap_table();

        self.module
            .verify()
            .map_err(|e| format!("Module verification failed: {}", e))
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
    /// `__KARA,__jittmpl` (segment 6 chars, section 8 chars) on Apple
    /// targets. The platform branch reflects the karac binary's host
    /// triple (matches `create_target_machine` which uses the default
    /// triple); cross-compile to a non-host object format would need
    /// to widen this surface.
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
            "__KARA,__jittmpl"
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

    fn param_name(&self, param: &Param) -> String {
        match &param.pattern.kind {
            PatternKind::Binding(name) => name.clone(),
            _ => "_".to_string(),
        }
    }
}
