//! Concurrency-lowering state: coroutines, state machines, spawn/task/channel
//! surfaces, auto-par groups, and hot-swap.
//!
//! The per-module caches behind every concurrency lowering `Codegen` performs:
//! the coroutine context and its park/spawn plumbing (`coro.rs`), the
//! network-event-loop state-machine tables, the spawn-site registry and
//! detach flags, channel/task element types, the auto-par branch machinery
//! (`par_blocks.rs`, `disjoint_par.rs` — capture modes, the analyzer's
//! decisions, the branch struct type and counter, the cancel pointer), and
//! the hot-swap slot tables. Extracted from `Codegen` as cluster 11 of the
//! state-decomposition spike; see
//! `docs/spikes/state-decomposition-codegen-methodcall.md`.

use rustc_hash::FxHashMap;
use std::collections::{HashMap, HashSet};

use inkwell::types::{BasicTypeEnum, StructType};
use inkwell::values::{FunctionValue, PointerValue};

use super::coro;
use super::state::SpawnSiteRecord;
use crate::ast::TypeExpr;
use crate::concurrency::FunctionConcurrency;
use crate::ownership::ParCaptureMode;
use crate::resolver::SpanKey;

pub(crate) struct ConcState<'ctx> {
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
    /// B-2026-06-17-2 — set by `compile_stmt` when the statement being lowered
    /// is a discarded `spawn(...)` / `tg.spawn(...)` (a bare expression-
    /// statement or `let _ = …`), whose result `TaskHandle` is never bound or
    /// joined. Consumed (read + cleared) inside `lower_spawn_shared`, which
    /// emits a `karac_runtime_task_detach(handle)` call so the runtime
    /// eager-reaps the handle instead of leaking it. Set false unconditionally
    /// at the top of `compile_stmt` so a prior statement's value never bleeds
    /// into an unrelated spawn lowered as a subexpression.
    pub(crate) pending_spawn_detach: bool,
    /// Per-channel-op MethodCall → element `TypeExpr` side-table — populated
    /// from `Program.channel_elem_types`. Key: `(span.offset, span.length)`
    /// of the `Sender.send` / `Receiver.recv` / `Receiver.try_recv`
    /// MethodCall. Value: the channel element `T`. The channel-op arm of
    /// `compile_method_call` lowers `T` to its LLVM shape to size the
    /// `karac_runtime_channel_*` transfer and shape the recv out slot.
    pub(crate) channel_elem_types: HashMap<(usize, usize), TypeExpr>,
    /// `TaskHandle[T].join()` MethodCall span → result type `T`, from
    /// `Program.task_join_return_types`. The join arm of `compile_method_call`
    /// lowers `T` to its LLVM shape so the cross-task result transfer (and the
    /// join out-slot) is sized for a non-scalar return; absent ⇒ `i64` default.
    pub(crate) task_join_return_types: HashMap<(usize, usize), TypeExpr>,
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
    pub(crate) par_capture_modes: FxHashMap<SpanKey, Vec<(String, ParCaptureMode)>>,
    /// Per-function parallelization decisions populated from `ConcurrencyAnalysis`.
    /// Function name → `FunctionConcurrency` (parallel groups + total stmt count).
    /// Threaded in by `load_concurrency_analysis`; consumed in slice 2 by the
    /// auto-par lowering path that emits `karac_par_run` for inferred groups
    /// outside explicit `par {}` blocks. Empty when no analysis was supplied.
    pub(crate) concurrency_decisions: HashMap<String, FunctionConcurrency>,
    // ── Par block runtime ─────────────────────────────────────────
    /// Monotonic counter used to generate unique par-branch function names.
    /// Also serves as the `SpawnSiteId` for each `par {}` block — the value
    /// at the time `emit_par_run` records a spawn site is the ID written
    /// into the `KARAC_SPAWN_SITES` metadata table (slice 3 of the
    /// Debugger Contract; see `SpawnSiteRecord`).
    pub(crate) par_counter: u32,
    /// Runtime struct `KaracBranch { ptr func, ptr ctx }` — shared across par blocks.
    pub(crate) karac_branch_ty: StructType<'ctx>,
    // ── Debugger contract: SpawnSiteId metadata (slice 3) ─────────
    /// One entry per `par {}` block (explicit or inferred). Populated by
    /// `record_spawn_site`; emitted as the `KARAC_SPAWN_SITES` global by
    /// `emit_spawn_sites_metadata` at the end of compilation. The order
    /// matches `SpawnSiteId` order (entry 0 → ID 0, entry 1 → ID 1, …).
    pub(crate) spawn_sites: Vec<SpawnSiteRecord>,
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
