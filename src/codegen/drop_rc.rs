//! Drop / clone / RC-fallback state.
//!
//! Eleventh slice of the Phase-2 `Codegen` decomposition
//! ([`docs/spikes/state-decomposition-codegen-methodcall.md`]). This is
//! cluster 13, measured as the **highest-traffic cluster for bug fixes**
//! in the whole struct — it was blocked behind two RC/drop leaks
//! (B-2026-08-15-6 / -7) and taken once those closed.
//!
//! Groups the ownership-lowering state:
//!
//! - the **scope cleanup stack** (`scope_cleanup_actions`) — a stack of
//!   per-scope action lists, pushed and popped by every construct that
//!   opens a scope, which is why so many sibling modules mutate it;
//! - the emitted drop functions, cached per type: enums, structs, user
//!   `Drop` wrappers, aggregates, RC boxes, and the by-name `drop_fn_cache`;
//! - the clone caches (`clone_fn_cache`, `try_clone_fn_cache`);
//! - the **RC fallback** analysis — which functions fall back to RC, the
//!   Arc (cross-thread) variant, the heap struct type per fallback, and
//!   the `rc_elide_ref_params` hint from `ownership.rs` (see
//!   `docs/spikes/rc-elide-ref-params.md`);
//! - the owned-temp drop table and the two inline-payload retain flags.
//!
//! Accessed as `self.drop_rc.<name>` from the sibling `impl Codegen`
//! modules.

use std::collections::{HashMap, HashSet};

use inkwell::types::StructType;
use inkwell::values::{FunctionValue, PointerValue};

use super::state::CleanupAction;
use crate::ast::TypeExpr;

/// Drop, clone and RC-fallback lowering state.
pub(crate) struct DropRc<'ctx> {
    /// B-2026-08-08-25 leg 1 — sources whose inline `Option`/`Result` payload a
    /// read-only match LEFT WITH THEM (`scrutinee_is_readonly_inline_optres_local`
    /// classified the match as a borrow, so no arm binding took ownership).
    ///
    /// Exists to keep the one-owner invariant across a CHAIN.
    /// `suppress_inline_option_result_binding_move` disarms a source binding
    /// when a consuming combinator took its payload — and for
    /// `<src>.map(f).unwrap_or(d)` it resolves through the map to `<src>`
    /// (B-2026-08-07-3), on the premise that the map's arm already owns the
    /// buffer. Once the arm is classified as a borrow that premise is false:
    /// the arm frees nothing, so disarming the source too leaves NOBODY, and
    /// the payload leaks once per evaluation (measured on
    /// `opt.map(|xs| xs.len()).unwrap_or(0)` — 32 bytes an iteration, invisible
    /// at the default opt level because a `.len()`-only Vec is a dead
    /// allocation LLVM deletes outright).
    pub(crate) inline_optres_retained_sources: std::collections::HashSet<String>,
    /// Per-scope cleanup stack.  Each inner `Vec` is one scope frame; entries
    /// are emitted in reverse-push order at scope exit (innermost first).
    pub(crate) scope_cleanup_actions: Vec<Vec<CleanupAction<'ctx>>>,
    /// B-2026-08-28-51 — spans (`(offset, length)`, the shape every other span
    /// table uses) of expressions known to sit in an ESCAPING position: one
    /// whose value is handed to an owner rather than discarded. Seeded at the
    /// three escaping sites — a function body's tail, a `return` operand, a
    /// `let` initializer — and extended lazily through branch structure, so
    /// each arm tail of an escaping `if` / `match` / block is escaping too.
    ///
    /// The interpreter keeps the identical set by the identical rule
    /// (`Interpreter::note_escaping_site`), which is what makes the two
    /// backends agree here by construction rather than by convention.
    ///
    /// Not cleared, including per function, matching the interpreter — see the
    /// caveat on `Interpreter::cond_move_escaping_sites` for why that symmetry
    /// is worth more than the smaller collision window clearing would buy on
    /// this side alone.
    pub(crate) cond_move_escaping_sites: std::collections::HashSet<(usize, usize)>,
    /// B-2026-08-28-51 — per-binding CONDITIONAL-MOVE drop flags: an `i1`
    /// alloca that is `true` while the binding still owns its value and
    /// `false` once a branch arm has moved it out.
    ///
    /// This is the runtime bit the row says both backends lack. A value moved
    /// on SOME paths and dead on others cannot be resolved statically, and the
    /// two existing channels guess in opposite directions: `merge_outer_states`
    /// re-marks the place `Owned` and leans on a cap/null guard that protects
    /// MEMORY but cannot stop a user `Drop` BODY running twice, while the
    /// move-suppression family removes the action on ALL paths and under-fires.
    ///
    /// Allocated LAZILY, at the arm tail that moves the binding, because that
    /// is the first point the shape is known — the `let` that registered the
    /// action was compiled earlier. The alloca and its `true` init go in the
    /// function's ENTRY block, which dominates every path, so the `false`
    /// store in the arm's own basic block is the only path-dependent write.
    ///
    /// Scoped deliberately narrowly: only a binding actually reached as an
    /// escaping branch-arm tail gets a flag, so the action stays armed for
    /// everything else and `has_armed_user_drop` /
    /// `has_armed_own_user_drop` / `has_armed_container_elem_bodies` answer
    /// exactly as they did before. That is what keeps this slice off the
    /// predicate question the row warns about.
    pub(crate) cond_move_drop_flags: HashMap<String, PointerValue<'ctx>>,
    /// B-2026-08-30-28 — the parameters whose user `Drop` BODY is owned by a
    /// per-path flag rather than by a static decision, so a later move site
    /// must NOT retract their action.
    ///
    /// `compile_function`'s conditional-store registration arms a bodies-only
    /// drop for a parameter stored into an outliving place on SOME path, and
    /// `arm_conditional_store_flag` disarms it on the path that stored. The
    /// store site then reaches `suppress_user_drop_for_var`, whose removal is
    /// all-paths — it would delete the very action the flag exists to schedule,
    /// putting the shape straight back to a lost body. This set is what lets
    /// that one removal decline.
    ///
    /// A SET, not a reuse of `cond_move_drop_flags`: that map also holds the
    /// conditional-RETURN bindings, whose ownership story is settled by
    /// B-2026-08-28-51/-65 and which have always been safe to retract. Keying
    /// the decline on this set alone means the only actions protected are the
    /// ones this row's registration created, so no pre-existing retraction
    /// changes behaviour.
    pub(crate) cond_store_flag_params: std::collections::HashSet<String>,
    /// B-2026-08-26-30 — slots already zero-initialized at their alloca by
    /// `zero_init_tracked_vec_slot`. Purely a de-duplicator: a slot tracked
    /// more than once would otherwise collect one identical `{null, 0, 0}`
    /// store per registration. The repeats are harmless (same value, same
    /// dominating position, folded by LLVM) but they are IR noise, and this
    /// keeps the emitted entry block readable when a slot is re-tracked in a
    /// loop.
    pub(crate) zero_inited_vec_slots: rustc_hash::FxHashSet<PointerValue<'ctx>>,
    /// B-2026-07-10-4 — when set, the deep-copy field walker
    /// (`deep_copy_one_aggregate_field` / `deep_copy_vec_aggregate_elements_in_place`)
    /// additionally rc-INCs a bare `shared` handle it would otherwise leave shallow:
    /// a directly-nested `shared` field, and each element of a `Vec[shared]`. A
    /// copy-supported struct can carry such a handle BURIED inside a `Vec[struct]`
    /// element or nested struct (`FnDefNode.params[].ty`, `FnDefNode.body`,
    /// `EnumDefNode.variants[].fields`) — the entry-copy duplicated the buffers but
    /// shared the boxes without a refcount bump, while the combined struct-drop
    /// rc-DECs them per element, so the caller's retained original and the callee's
    /// copy both dec → double-free (the self-hosted item parser's `render_*` nodes).
    /// Set only around `make_aggregate_param_callee_owned`'s deep-copy so the copy
    /// stays symmetric with that drop; false elsewhere. (An earlier global attempt
    /// leaked because the drop side hadn't yet been reconciled — it since was, so
    /// the entry-copy inc is now balanced.)
    pub(crate) deep_copy_rc_inc_bare_shared: bool,
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
    /// B-2026-08-29-15 — the bare-identifier TARGET of the assignment
    /// statement currently being compiled, if any (`e = pass(e);` records
    /// `"e"`). Reset at the top of every `compile_stmt`, so it never outlives
    /// the statement that set it.
    ///
    /// Read by `suppress_user_drop_body_keeping_memory` to decline a
    /// self-assignment: there the callee's result is stored straight back into
    /// the argument's own slot, so that binding does NOT die at the call — it
    /// goes on to own the returned value and needs its cleanup action intact.
    /// Retracting it stranded the value with no owner at all (measured:
    /// `let mut e = mk_loud(7); e = pass(e);` lost `loud drop` / `drop 7 l7`
    /// entirely, and the enum spelling aborted before flushing any output).
    ///
    /// A stale value could only make the suppression MORE conservative — a
    /// kept double body, never a lost one — but the per-statement reset means
    /// there is none.
    pub(crate) assign_ident_target: Option<String>,
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
    /// Surface `TypeExpr` per heap-owning *temporary* expression —
    /// populated from `Program.owned_temp_drops` (set by the lowering pass
    /// from `TypeCheckResult.expr_types`). `materialize_owned_temp` keys
    /// this by the producing expression's span to reconstruct an unnamed
    /// temporary's scope-exit cleanup (Vec element type / Map key-val
    /// classification / RC heap layout). See
    /// `docs/spikes/general-owned-temp-tracking.md` (slice 2).
    pub(crate) owned_temp_drops: HashMap<(usize, usize), TypeExpr>,
    // ── RC-fallback bindings ──────────────────────────────────────
    /// Per-function RC-fallback binding names populated from `OwnershipCheckResult`.
    /// Function name → set of binding names that need heap-boxing + refcount.
    pub(crate) rc_fallback_fns: HashMap<String, HashSet<String>>,
    /// RC-elide-ref (env `KARAC_RC_ELIDE_REF_PARAMS`, default ON; opt out `=0`): per-fn
    /// `(param name, position)` of every `ref`-mode `shared`/`Option[shared]`
    /// parameter proven **sound to RC-elide** by
    /// [`crate::rc_elide::safe_elidable_ref_params`] — a private, directly-called
    /// function whose every call passes this param a *projection* of a named
    /// binding (a borrow), used only in place (consumed-in-place per
    /// `result_escape`), with a scalar return and no `mut ref` params (no
    /// resource escapes). ORed into the same call-site arg-inc skip (by
    /// position) and callee-side exit-dec skip (by name) as
    /// `borrowed_param_skips`, WITHOUT the `headerless_types` guard: the
    /// LSan-verified C2b borrow path (no arg inc, no source transfer/consume, no
    /// callee exit dec — a pure balanced borrow). Verified flag-on == flag-off on
    /// the full Linux LSan suite. Empty unless the env flag is set. See
    /// `docs/spikes/rc-elide-ref-params.md`.
    pub(crate) rc_elide_ref_params: HashMap<String, Vec<(String, usize)>>,
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
}
