//! Runtime intrinsic emission: refcounting, scope cleanup, var-tracking
//! registration, and the string-build helpers used by f-strings.
//!
//! Houses `emit_panic`, the RC/Arc alloc/inc/dec primitives
//! (`emit_rc_alloc`, `emit_rc_inc`, `emit_rc_dec`, `emit_arc_inc`,
//! `emit_arc_dec`, `emit_refcount_inc`, `emit_refcount_dec`), the
//! per-variable cleanup-registration helpers
//! (`track_rc_var`, `track_vec_var`, `track_map_var`, `track_enum_var`,
//! `track_struct_var`, `enum_name_for_binding`), the scope-cleanup
//! emission (`emit_scope_cleanup`, `drain_top_frame_with_emit`,
//! `emit_cleanup_action`), and the f-string raw-builder helpers
//! (`emit_string_append_raw`, `compile_fstr_part_to_cstr`).

use crate::ast::*;

use inkwell::types::{BasicTypeEnum, StructType};
use inkwell::values::{BasicValueEnum, FunctionValue, PointerValue};
use inkwell::{AddressSpace, AtomicOrdering, AtomicRMWBinOp, IntPredicate};

use super::state::{CleanupAction, VarSlot};

impl<'ctx> super::Codegen<'ctx> {
    /// Allocate a new RC heap object: `malloc(sizeof(heap_type))`, store refcount = 1.
    /// Returns a pointer to the heap object.
    pub(super) fn emit_panic(&self, message: &str) {
        // design.md § Contracts rule 2: the fault-category prefix is decided at
        // RUNTIME by `karac_runtime_panic_prefix()`, which returns
        // `"contract predicate panicked: "` while a contract predicate is on the
        // stack (a thread-local depth counter set by the enter/exit calls
        // `emit_contract_assert` brackets the predicate's evaluation with) and
        // `""` otherwise. Reading the flag at runtime — rather than baking the
        // prefix in from a compile-time flag — categorizes BOTH the inline case
        // (a bounds / div / unwrap panic lexically inside the predicate) AND the
        // cross-call case (a panic inside a function the predicate calls), which
        // a lexical flag cannot see. The format string is fixed (`panic: %s%s`),
        // so `message` is a `%s` data argument, not the format string — output
        // is byte-identical to the two historical forms `panic: <msg>` and
        // `panic: contract predicate panicked: <msg>`.
        let prefix = self
            .builder
            .build_call(self.karac_runtime_panic_prefix_fn, &[], "panic_prefix")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic();
        let fmt = self
            .builder
            .build_global_string_ptr("panic: %s%s\n\0", "panic_fmt")
            .unwrap();
        let msg = self
            .builder
            .build_global_string_ptr(&format!("{}\0", message), "panic_msg")
            .unwrap();
        self.builder
            .build_call(
                self.printf_fn,
                &[
                    fmt.as_pointer_value().into(),
                    prefix.into(),
                    msg.as_pointer_value().into(),
                ],
                "panic_print",
            )
            .unwrap();
        let exit_code = self.context.i32_type().const_int(1, false);
        self.builder
            .build_call(self.exit_fn, &[exit_code.into()], "")
            .unwrap();
    }

    pub(super) fn emit_rc_alloc(&self, heap_type: StructType<'ctx>) -> PointerValue<'ctx> {
        let size = heap_type.size_of().expect("heap type must be sized");
        let call = self
            .builder
            .build_call(self.malloc_fn, &[size.into()], "rc_alloc")
            .unwrap();
        let ptr = call
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        // Store refcount = 1 at field 0.
        let rc_ptr = self
            .builder
            .build_struct_gep(heap_type, ptr, 0, "rc_ptr")
            .unwrap();
        self.builder
            .build_store(rc_ptr, self.context.i64_type().const_int(1, false))
            .unwrap();
        ptr
    }

    /// Increment the reference count of a shared object.
    pub(super) fn emit_rc_inc(&self, heap_type: StructType<'ctx>, ptr: PointerValue<'ctx>) {
        let rc_ptr = self
            .builder
            .build_struct_gep(heap_type, ptr, 0, "rc_ptr")
            .unwrap();
        let rc = self
            .builder
            .build_load(self.context.i64_type(), rc_ptr, "rc")
            .unwrap()
            .into_int_value();
        let rc_inc = self
            .builder
            .build_int_add(rc, self.context.i64_type().const_int(1, false), "rc_inc")
            .unwrap();
        self.builder.build_store(rc_ptr, rc_inc).unwrap();
    }

    /// Decrement the reference count. If it reaches zero, dispatch to
    /// the per-struct recursive drop fn (`__karac_rc_drop_<Name>`)
    /// when one was lazily synthesized by `track_rc_var` for this
    /// heap type. The drop fn walks each heap-owning field (shared
    /// inner refs, `Option[shared T]` fields, Vec/String data
    /// buffers, Map/Set handles) before `free(ptr)`. Falls back to
    /// plain `free(ptr)` when the struct has no walkable fields
    /// (every field primitive) — `emit_shared_struct_rc_drop_fn`
    /// caches `None` for those, and the reverse-lookup below sees
    /// `Some(None)` and takes the legacy path.
    ///
    /// Resolving heap_type → struct name is done by iterating
    /// `shared_types` (small map; O(n) is fine — measured cost
    /// noise versus a malloc/free pair). A reverse map could be
    /// added if profiles show it.
    pub(super) fn emit_rc_dec(&self, heap_type: StructType<'ctx>, ptr: PointerValue<'ctx>) {
        let rc_ptr = self
            .builder
            .build_struct_gep(heap_type, ptr, 0, "rc_ptr")
            .unwrap();
        let rc = self
            .builder
            .build_load(self.context.i64_type(), rc_ptr, "rc")
            .unwrap()
            .into_int_value();
        let rc_dec = self
            .builder
            .build_int_sub(rc, self.context.i64_type().const_int(1, false), "rc_dec")
            .unwrap();
        self.builder.build_store(rc_ptr, rc_dec).unwrap();

        let is_zero = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                rc_dec,
                self.context.i64_type().const_zero(),
                "rc_is_zero",
            )
            .unwrap();

        let current_fn = self.current_fn.unwrap();
        let free_bb = self.context.append_basic_block(current_fn, "rc_free");
        let done_bb = self.context.append_basic_block(current_fn, "rc_done");

        self.builder
            .build_conditional_branch(is_zero, free_bb, done_bb)
            .unwrap();

        self.builder.position_at_end(free_bb);
        // Dispatch to the per-struct recursive drop fn when one was
        // synthesized for this heap_type. Otherwise plain `free`. The
        // drop fn includes `free(ptr)` after its field walk, so we
        // don't emit a second `free` here.
        let mut dropped = false;
        for (name, info) in &self.shared_types {
            if info.heap_type == heap_type {
                if let Some(Some(drop_fn)) = self.rc_drop_fns.get(name) {
                    self.builder
                        .build_call(*drop_fn, &[ptr.into()], "")
                        .unwrap();
                    dropped = true;
                }
                break;
            }
        }
        if !dropped {
            self.builder
                .build_call(self.free_fn, &[ptr.into()], "")
                .unwrap();
        }
        self.builder.build_unconditional_branch(done_bb).unwrap();

        self.builder.position_at_end(done_bb);
    }

    /// Atomic counterpart to `emit_rc_inc` for `arc_values`-promoted bindings.
    /// `atomicrmw add refcount, 1, seq_cst`. Mirrors the non-atomic helper's
    /// shape exactly — same `struct_gep` to land on the refcount field, same
    /// `+1`-by-i64 — only the load+arith+store sequence changes to a single
    /// `atomicrmw` op. Memory ordering is `SequentiallyConsistent` for v1
    /// (correct, conservative); relaxation to `Monotonic`+`Acquire`/`Release`
    /// per Rust's `Arc` is a future optimization tracked under "out of scope"
    /// in the slice plan. The returned old value is discarded — increments do
    /// not need to observe it (only decrements do, to detect transition to 0).
    pub(super) fn emit_arc_inc(&self, heap_type: StructType<'ctx>, ptr: PointerValue<'ctx>) {
        let rc_ptr = self
            .builder
            .build_struct_gep(heap_type, ptr, 0, "arc_ptr")
            .unwrap();
        let one = self.context.i64_type().const_int(1, false);
        self.builder
            .build_atomicrmw(
                AtomicRMWBinOp::Add,
                rc_ptr,
                one,
                AtomicOrdering::SequentiallyConsistent,
            )
            .unwrap();
    }

    /// Atomic counterpart to `emit_rc_dec`. Uses `atomicrmw sub refcount, 1,
    /// seq_cst`; the returned value is the *previous* refcount, so the
    /// "drop-to-zero" check is `old == 1` (post-decrement value is 0). Same
    /// branch shape as `emit_rc_dec`: a `free_bb` that calls `free(ptr)` and
    /// a `done_bb` join.
    pub(super) fn emit_arc_dec(&self, heap_type: StructType<'ctx>, ptr: PointerValue<'ctx>) {
        let rc_ptr = self
            .builder
            .build_struct_gep(heap_type, ptr, 0, "arc_ptr")
            .unwrap();
        let one = self.context.i64_type().const_int(1, false);
        let old = self
            .builder
            .build_atomicrmw(
                AtomicRMWBinOp::Sub,
                rc_ptr,
                one,
                AtomicOrdering::SequentiallyConsistent,
            )
            .unwrap();

        let is_last = self
            .builder
            .build_int_compare(IntPredicate::EQ, old, one, "arc_is_last")
            .unwrap();

        let current_fn = self.current_fn.unwrap();
        let free_bb = self.context.append_basic_block(current_fn, "arc_free");
        let done_bb = self.context.append_basic_block(current_fn, "arc_done");

        self.builder
            .build_conditional_branch(is_last, free_bb, done_bb)
            .unwrap();

        self.builder.position_at_end(free_bb);
        // Mirror `emit_rc_dec`'s drop-fn dispatch on the atomic
        // path. The drop fn body uses non-atomic field walks
        // internally — the last decrement happens HERE (atomicrmw
        // sub), so once we're inside `free_bb` we hold the unique
        // reference and the walk runs on a non-shared memory view.
        let mut dropped = false;
        for (name, info) in &self.shared_types {
            if info.heap_type == heap_type {
                if let Some(Some(drop_fn)) = self.rc_drop_fns.get(name) {
                    self.builder
                        .build_call(*drop_fn, &[ptr.into()], "")
                        .unwrap();
                    dropped = true;
                }
                break;
            }
        }
        if !dropped {
            self.builder
                .build_call(self.free_fn, &[ptr.into()], "")
                .unwrap();
        }
        self.builder.build_unconditional_branch(done_bb).unwrap();

        self.builder.position_at_end(done_bb);
    }

    /// Dispatch an inc on `name`'s refcount: atomic path when `name` is in
    /// `arc_fallback_fns` for the current function, plain non-atomic otherwise.
    pub(super) fn emit_refcount_inc(
        &self,
        name: &str,
        heap_type: StructType<'ctx>,
        ptr: PointerValue<'ctx>,
    ) {
        if self.is_arc_binding(name) {
            self.emit_arc_inc(heap_type, ptr);
        } else {
            self.emit_rc_inc(heap_type, ptr);
        }
    }

    /// Dispatch a dec on `name`'s refcount: atomic path when `name` is in
    /// `arc_fallback_fns` for the current function, plain non-atomic otherwise.
    pub(super) fn emit_refcount_dec(
        &self,
        name: &str,
        heap_type: StructType<'ctx>,
        ptr: PointerValue<'ctx>,
    ) {
        if self.is_arc_binding(name) {
            self.emit_arc_dec(heap_type, ptr);
        } else {
            self.emit_rc_dec(heap_type, ptr);
        }
    }

    /// Track a shared-type variable for scope-exit rc_dec.
    ///
    /// See `null_init_slot_in_entry_block` for the null-init step that
    /// has to fire AFTER the slot exists in `self.variables` (which
    /// happens at `bind_pattern` time, after this function returns in
    /// the let-stmt flow). The caller in `compile_stmt` re-fetches the
    /// slot after bind_pattern and calls `null_init_slot_in_entry_block`
    /// directly.
    /// Reverse-lookup a shared struct's surface name from its heap
    /// `StructType`. Used by `track_rc_var` / `track_rc_option_var`
    /// to drive the lazy synth of `__karac_rc_drop_<Name>`. O(n) over
    /// `shared_types`; cheap in practice (small number of shared
    /// types per program) and only runs at let-binding time, not on
    /// the hot scope-exit path.
    pub(super) fn struct_name_for_heap_type(&self, heap_type: StructType<'ctx>) -> Option<String> {
        for (name, info) in &self.shared_types {
            if info.heap_type == heap_type {
                return Some(name.clone());
            }
        }
        None
    }

    pub(super) fn track_rc_var(
        &mut self,
        name: &str,
        ptr: PointerValue<'ctx>,
        heap_type: StructType<'ctx>,
    ) {
        // Lazy-synth the recursive drop fn for this shared struct's
        // heap type. Without this, `emit_rc_dec`'s reverse-lookup
        // would never find a registered drop fn and the recursive
        // chain leaks (closes the LeetCode #2 kata bench). The
        // synthesis builds an idempotent fn — repeated `track_rc_var`
        // calls for the same type return the cached entry.
        if let Some(struct_name) = self.struct_name_for_heap_type(heap_type) {
            let _ = self.emit_shared_struct_rc_drop_fn(&struct_name);
        }
        if let Some(frame) = self.scope_cleanup_actions.last_mut() {
            frame.push(CleanupAction::RcDec {
                name: name.to_string(),
                ptr,
                heap_type,
            });
        }
    }

    /// Emit a `store null, slot` at the top of the current function's
    /// entry block (after any allocas, before any body code). Used by
    /// `track_rc_var` to ensure body-local shared-struct slots whose
    /// let-binding may not execute carry a defined null sentinel by the
    /// time scope cleanup runs.
    pub(super) fn null_init_slot_in_entry_block(&self, slot: PointerValue<'ctx>) {
        let Some(fn_val) = self.current_fn else {
            return;
        };
        let Some(entry) = fn_val.get_first_basic_block() else {
            return;
        };
        let b = self.context.create_builder();
        // Position at end of entry block — after any allocas, but
        // before any non-alloca instructions that compile_function
        // emits (parameter copies, RC fallback boxing, etc.). Per LLVM
        // SSA discipline allocas in the entry block precede other ops,
        // so a store at end-of-entry-block runs before the body's
        // first basic-block branch.
        match entry.get_terminator() {
            Some(term) => b.position_before(&term),
            None => b.position_at_end(entry),
        }
        let null = self.context.ptr_type(AddressSpace::default()).const_null();
        let _ = b.build_store(slot, null);
    }

    /// Track an `Option[shared T]` binding for scope-exit rc_dec of its
    /// inner pointer. Mirrors `track_rc_var` but operates on the Option
    /// struct's `{tag, w0, ...}` shape: cleanup loads the tag, branches
    /// on `Some`, and when Some recovers the inner heap pointer from
    /// `w0` (i64 → ptr) before dispatching through `emit_refcount_dec`.
    /// Closes the kata-bench leak: `let out: Option[ShareT] = call();`
    /// (and the same shape via inferred annotation) now drops the
    /// chain's head ref on scope exit. See `CleanupAction::RcDecOption`
    /// for the runtime IR shape.
    pub(super) fn track_rc_option_var(
        &mut self,
        name: &str,
        option_slot: PointerValue<'ctx>,
        option_ty: StructType<'ctx>,
        heap_type: StructType<'ctx>,
    ) {
        // Lazy-synth the recursive drop fn for the inner shared
        // struct's heap type. Same rationale as `track_rc_var`'s
        // synth call; the cleanup arm's `emit_refcount_dec` will
        // dispatch through the cached drop fn for transitive
        // refcount management.
        if let Some(struct_name) = self.struct_name_for_heap_type(heap_type) {
            let _ = self.emit_shared_struct_rc_drop_fn(&struct_name);
        }
        // Record the inner heap layout so the `Assign` arm in
        // `compile_stmt` can perform refcount-aware reassignment of
        // an `Option[shared T]` variable (dec the old inner ptr,
        // inc the new one unless the RHS is a fresh `Some(...)`).
        // Mirrors the plain shared-T Assign arm's behavior, scaled
        // up to the Option-wrapped shape. Without this, a `mut
        // Option[shared T]` binding's reassignment (`next_a =
        // n.next;` in the LeetCode #2 recursive variant) strands
        // the old ref and over-decrements at scope exit, freeing
        // an aliased chain mid-recursion.
        self.var_option_shared_heap
            .insert(name.to_string(), heap_type);
        // Resolve the Some-tag from the seeded Option layout. Defaults
        // to 1 if (impossibly) the table is missing — matches the
        // canonical `seed_builtin_enum_layouts` numbering.
        let some_tag = self
            .enum_layouts
            .get("Option")
            .and_then(|l| l.tags.get("Some").copied())
            .unwrap_or(1);
        if let Some(frame) = self.scope_cleanup_actions.last_mut() {
            frame.push(CleanupAction::RcDecOption {
                name: name.to_string(),
                option_slot,
                option_ty,
                heap_type,
                some_tag,
            });
        }
    }

    /// Zero-init an `Option[T]` slot at the top of the current
    /// function's entry block. Mirrors `null_init_slot_in_entry_block`'s
    /// shape but operates on the full Option struct (`{tag, w0, w1,
    /// w2}`) — `store zeroinitializer`, which puts tag=0 (None) in the
    /// slot. Used by the let-stmt handler for nested-block
    /// `Option[shared T]` lets whose bind_pattern store may not fire
    /// at runtime (loop body skipped, branch not taken); without this,
    /// the cleanup arm reads `undef` as the tag and may dispatch on a
    /// garbage Some-tag path.
    pub(super) fn zero_init_option_slot_in_entry_block(
        &self,
        slot: PointerValue<'ctx>,
        option_ty: StructType<'ctx>,
    ) {
        let Some(fn_val) = self.current_fn else {
            return;
        };
        let Some(entry) = fn_val.get_first_basic_block() else {
            return;
        };
        let b = self.context.create_builder();
        match entry.get_terminator() {
            Some(term) => b.position_before(&term),
            None => b.position_at_end(entry),
        }
        let _ = b.build_store(slot, option_ty.const_zero());
    }

    /// Track a Vec/String alloca for scope-exit buffer free. Pass the
    /// element LLVM type (`vec_elem_types[var_name]`) so the cleanup loop
    /// can recursively drop nested heap-owning element types — critical
    /// for `Vec[Vec[T]]`, `Vec[String]`, `Vec[Map[K, V]]`, etc., where the
    /// outer buffer's free does not reach the inner allocations.
    pub(super) fn track_vec_var(
        &mut self,
        vec_alloca: PointerValue<'ctx>,
        elem_ty: Option<BasicTypeEnum<'ctx>>,
    ) {
        if let Some(frame) = self.scope_cleanup_actions.last_mut() {
            frame.push(CleanupAction::FreeVecBuffer {
                vec_alloca,
                elem_ty,
            });
        }
    }

    /// Register a SoA-laid-out Vec for scope-exit cleanup. Mirrors
    /// `track_vec_var` but emits a `FreeSoaGroups` action whose cleanup
    /// loops over every hot group pointer and (if present) the cold
    /// pointer, GEP'ing against the SoA struct type so the cap-check
    /// reads the actual cap slot (not whichever slot collides with
    /// `vec_struct_type`'s field 2). Without this, an SoA Vec routed
    /// through `track_vec_var(_, None)` leaks every group except `g0`.
    pub(super) fn track_soa_groups(
        &mut self,
        soa_alloca: PointerValue<'ctx>,
        soa_struct_ty: StructType<'ctx>,
        num_hot_groups: u32,
        has_cold: bool,
    ) {
        if let Some(frame) = self.scope_cleanup_actions.last_mut() {
            frame.push(CleanupAction::FreeSoaGroups {
                soa_alloca,
                soa_struct_ty,
                num_hot_groups,
                has_cold,
            });
        }
    }

    /// Emit a runtime zero-store to a Vec/String alloca's `cap` field
    /// (slot index 2 of the `{data, len, cap}` struct). Used to suppress
    /// a queued `FreeVecBuffer` whose buffer ownership has moved to a
    /// different slot — the `cap > 0` guard in `emit_scope_cleanup`'s
    /// `FreeVecBuffer` walker turns the free into a no-op, leaving the
    /// new owner's own cleanup to run once.
    pub(super) fn zero_vec_alloca_cap(&self, vec_alloca: PointerValue<'ctx>) {
        let vec_ty = self.vec_struct_type();
        let i64_t = self.context.i64_type();
        if let Ok(cap_ptr) =
            self.builder
                .build_struct_gep(vec_ty, vec_alloca, 2, "fstr.acc.cap.suppress")
        {
            let _ = self.builder.build_store(cap_ptr, i64_t.const_int(0, false));
        }
    }

    /// Emit an eager free of a Vec/String slot's heap buffer, guarded on
    /// `cap > 0`. Used at move-overwrite sites where the slot is about to
    /// be reassigned to a new heap buffer — without this, the prior
    /// buffer leaks (the slot loses its only reference before scope-exit
    /// cleanup can reach it). Mirrors the runtime shape of `FreeVecBuffer`
    /// for the eager-free position. `cap = 0` slots (string literals,
    /// already-transferred sources) skip the free, preserving the static-
    /// vs-heap invariant the scope walker also relies on.
    ///
    /// **Outer-buffer free only** — does NOT walk inner elements when the
    /// element type is itself heap-owning. The eager-free site sits in
    /// the middle of a user's control flow, so inner heap-owning elements
    /// may already be co-owned by other live bindings (`let x = vec[i]`
    /// shapes that haven't gone out of scope yet, sibling aliases mid-
    /// loop, etc.). Walking the inner buffers here races with the per-
    /// alias scope-exit cleanup the let-binding registered at its own
    /// site — a double-free that hangs in macOS malloc. The scope-exit
    /// `FreeVecBuffer` cleanup walker IS safe to do the recursive walk
    /// because it runs at function exit when every per-alias cleanup has
    /// already drained.
    ///
    /// Result: outer-buffer leak is closed, inner heap-owned elements
    /// are still freed via their existing per-alias scope-exit cleanup
    /// (e.g., the `let prefix = out[i]` body in kata-17 frees each
    /// indexed String at end-of-iter; the leak there was the outer
    /// {ptr,len,cap} array per BFS step). Workloads that move-overwrite
    /// without per-element aliases keep their existing scope-exit
    /// recursive drop unchanged.
    pub(super) fn emit_free_vec_buffer_if_owned(&mut self, vec_alloca: PointerValue<'ctx>) {
        let vec_ty = self.vec_struct_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();
        let fn_val = match self.current_fn {
            Some(f) => f,
            None => return,
        };
        let data_ptr = match self
            .builder
            .build_struct_gep(vec_ty, vec_alloca, 0, "ov.data.pp")
        {
            Ok(p) => p,
            Err(_) => return,
        };
        let cap_ptr = match self
            .builder
            .build_struct_gep(vec_ty, vec_alloca, 2, "ov.cap.pp")
        {
            Ok(p) => p,
            Err(_) => return,
        };
        let data = self
            .builder
            .build_load(ptr_ty, data_ptr, "ov.data")
            .unwrap()
            .into_pointer_value();
        let cap = self
            .builder
            .build_load(i64_t, cap_ptr, "ov.cap")
            .unwrap()
            .into_int_value();
        let zero = i64_t.const_int(0, false);
        let owned = self
            .builder
            .build_int_compare(IntPredicate::UGT, cap, zero, "ov.owned")
            .unwrap();
        let free_bb = self.context.append_basic_block(fn_val, "ov.free");
        let after_bb = self.context.append_basic_block(fn_val, "ov.after");
        self.builder
            .build_conditional_branch(owned, free_bb, after_bb)
            .unwrap();
        self.builder.position_at_end(free_bb);
        self.builder
            .build_call(self.free_fn, &[data.into()], "")
            .unwrap();
        self.builder.build_unconditional_branch(after_bb).unwrap();
        self.builder.position_at_end(after_bb);
    }

    /// Track a Map / Set alloca for scope-exit free. `key_is_vec` /
    /// `val_is_vec` tell the cleanup whether each side follows the
    /// Vec/String `{ptr, len, cap}` layout and therefore needs per-entry
    /// buffer release before the bucket storage is deallocated. Both
    /// false → plain `karac_map_free`. Either true → routes through
    /// `karac_map_free_with_drop_vec(handle, key_is_vec, val_is_vec)`
    /// so the per-entry walk runs.
    ///
    /// `val_shared_heap_type = Some(heap_ty)` triggers the codegen-side
    /// per-bucket rc_dec walk for shared-struct / shared-enum values
    /// (the runtime helper can't decrement refcounts itself — it's
    /// type-erased and doesn't know V's heap layout). Closes the
    /// `Map[K, shared T]` leak (2026-05-16): values previously
    /// stranded their refcount when the Map went out of scope.
    /// `key_shared_heap_type` is the symmetric K-side gate — fires
    /// the same walk against the key half of each occupied bucket
    /// (`Map[shared K, V]` / `Set[shared T]`).
    pub(super) fn track_map_var(
        &mut self,
        map_alloca: PointerValue<'ctx>,
        key_is_vec: bool,
        val_is_vec: bool,
        val_shared_heap_type: Option<StructType<'ctx>>,
        key_shared_heap_type: Option<StructType<'ctx>>,
    ) {
        if let Some(frame) = self.scope_cleanup_actions.last_mut() {
            frame.push(CleanupAction::FreeMapHandle {
                map_alloca,
                key_is_vec,
                val_is_vec,
                val_shared_heap_type,
                key_shared_heap_type,
            });
        }
    }

    /// Phase 8 `File` handle slice F4b: register a File-typed binding
    /// for scope-exit close. Pushed at the pattern-binding site in
    /// `pattern_binding.rs` when `type_name == "File"` fires the
    /// int→ptr re-typing arm. The drain emits
    /// `karac_runtime_file_close(load(file_alloca))` on exit.
    pub(super) fn track_file_var(&mut self, file_alloca: PointerValue<'ctx>) {
        if let Some(frame) = self.scope_cleanup_actions.last_mut() {
            frame.push(CleanupAction::FreeFileHandle { file_alloca });
        }
    }

    /// Phase 7.2 Slice DP — resolve a let-binding's surface enum name
    /// from the let-statement's annotation and RHS shape, for the
    /// `track_enum_var` registration site. Tries in order:
    ///
    /// 1. Existing `var_type_names` entry — populated by the upstream
    ///    type-hint pass when an explicit `let e: E = ...;` annotation
    ///    is present, or when an Identifier-RHS aliases a previously-
    ///    typed binding.
    /// 2. RHS = bare `Variant(args)` (`ExprKind::Call` with an Identifier
    ///    callee whose name matches a known variant) — walk `enum_layouts`
    ///    for the enum that owns that variant. Single-variant collisions
    ///    across enums are rare in practice and are tolerated by taking
    ///    the first match.
    /// 3. RHS = qualified `Enum.Variant(args)` (`ExprKind::Call` with a
    ///    Path-based callee whose first segment matches a known enum) —
    ///    use the first-segment name directly.
    /// 4. RHS = qualified `Enum.assoc_fn(args)` returning a value of the
    ///    enum's LLVM struct type — match by LLVM-struct-identity reverse-
    ///    lookup against `enum_layouts` (the same shape the existing
    ///    user-struct fallback at the let-site uses for structs).
    ///
    /// Returns `None` when the binding's surface type isn't a known
    /// value-type enum; the cleanup hook then becomes a no-op for that
    /// binding (matches v1 conservative behavior — no spurious cleanup).
    pub(super) fn enum_name_for_binding(
        &self,
        var_name: &str,
        value: &Expr,
        ty: Option<&TypeExpr>,
    ) -> Option<String> {
        // (1) Existing var_type_names entry pointing at a known enum.
        if let Some(n) = self.var_type_names.get(var_name) {
            if self.enum_layouts.contains_key(n) {
                return Some(n.clone());
            }
        }
        // Explicit annotation.
        if let Some(t) = ty {
            if let TypeKind::Path(p) = &t.kind {
                if let Some(seg) = p.segments.last() {
                    if self.enum_layouts.contains_key(seg) {
                        return Some(seg.clone());
                    }
                }
            }
        }
        // (2) / (3) Inspect the RHS Call shape.
        if let ExprKind::Call { callee, .. } = &value.kind {
            match &callee.kind {
                ExprKind::Identifier(n) => {
                    // Bare-name variant constructor. Prefer user-declared
                    // enums over seeded built-ins (Option / Result / Json
                    // / TcpError) when the variant name collides — same
                    // disambiguation as `try_compile_enum_variant`. Without
                    // this preference, HashMap iteration order picks a
                    // seeded enum's layout non-deterministically for a
                    // user-defined variant with the same name.
                    let mut user_match: Option<String> = None;
                    let mut seed_match: Option<String> = None;
                    for (en, layout) in &self.enum_layouts {
                        if layout.tags.contains_key(n) {
                            if self.seeded_enum_names.contains(en) {
                                seed_match.get_or_insert_with(|| en.clone());
                            } else {
                                user_match.get_or_insert_with(|| en.clone());
                            }
                        }
                    }
                    if let Some(name) = user_match.or(seed_match) {
                        return Some(name);
                    }
                }
                ExprKind::Path { segments, .. } => {
                    if let Some(first) = segments.first() {
                        if self.enum_layouts.contains_key(first) {
                            return Some(first.clone());
                        }
                    }
                }
                _ => {}
            }
        }
        None
    }

    /// Phase 7.2 Slice DP — register a value-type enum alloca for
    /// scope-exit drop-function invocation. Per design lock DP1, the
    /// registration site is at let-binding time (not inside
    /// `try_compile_enum_variant` — the variant constructor returns a
    /// `BasicValueEnum` aggregate before the alloca exists; the alloca
    /// is created by `bind_pattern_values`). Per DP3, `is_shared` enums
    /// are filtered upstream — RC inc/dec via `track_rc_var` handles
    /// their cleanup through refcount semantics. Per DP4, the
    /// scope-exit drain emits a single `call drop_fn(alloca)` for the
    /// `EnumDrop` action; move-suppression for caller→callee passing
    /// is implicit in the existing convention that function parameters
    /// don't register `track_enum_var` (mirrors how Vec/String params
    /// don't register `track_vec_var` — only the let-binding site
    /// owns cleanup, so the param is a stranded view of the same
    /// payload words and no double-free can occur).
    pub(super) fn track_enum_var(&mut self, enum_name: &str, enum_alloca: PointerValue<'ctx>) {
        // DP3 carve-out: shared enums use the RC-pointer cleanup path
        // (refcount-driven free in `emit_rc_dec`). The drop-switch
        // machinery is for value-type enums only.
        let is_shared = self
            .enum_layouts
            .get(enum_name)
            .map(|l| l.is_shared)
            .unwrap_or(false);
        if is_shared {
            return;
        }
        // Skip enums with no heap-bearing payload anywhere — emitting
        // a no-op drop call would just bloat IR. The drop-fn helper
        // returns `None` when every variant's `field_drop_kinds` is
        // entirely `EnumDropKind::None`.
        let drop_fn = match self.emit_enum_drop_switch(enum_name) {
            Some(f) => f,
            None => return,
        };
        if let Some(frame) = self.scope_cleanup_actions.last_mut() {
            frame.push(CleanupAction::EnumDrop {
                enum_alloca,
                drop_fn,
            });
        }
    }

    /// Track a non-shared struct alloca for scope-exit drop-fn invocation.
    /// Mirrors `track_enum_var` but for struct types. The per-struct drop
    /// fn is lazily synthesized by `emit_struct_drop_synthesis`; if the
    /// struct has no heap-owning fields (every field is primitive / Slice
    /// / Ref / etc.) the synthesis returns `None` and we skip registration
    /// — there's nothing to drop. Shared structs use the RC machinery
    /// (`track_rc_var` / `emit_refcount_dec`) and are also filtered out by
    /// `emit_struct_drop_synthesis`.
    ///
    /// Closes the 2026-05-14 leak class for `struct Holder { v: Vec[i64] }`
    /// / `struct Cache { entries: Map[String, V] }` / `Vec[Container]`
    /// (slice γ of the recursive-drop work). Without this, a let-binding
    /// of a struct value never drops its Vec/Map/Set field contents on
    /// scope exit — only the struct's own inline storage (the
    /// `{ptr, len, cap}` field for a Vec field) was released, the actual
    /// heap-allocated backing buffer leaked.
    pub(super) fn track_struct_var(
        &mut self,
        struct_name: &str,
        struct_alloca: PointerValue<'ctx>,
    ) {
        let drop_fn = match self.emit_struct_drop_synthesis(struct_name) {
            Some(f) => f,
            None => return,
        };
        if let Some(frame) = self.scope_cleanup_actions.last_mut() {
            frame.push(CleanupAction::StructDrop {
                struct_alloca,
                drop_fn,
            });
        }
    }

    /// Phase 7 user-`impl Drop` dispatch Prereq.3 — track a struct
    /// alloca for scope-exit invocation of its `karac_drop_<Type>`
    /// wrapper. Used in place of `track_struct_var` when the binding's
    /// type has a user-defined `impl Drop` — the wrapper's body already
    /// invokes the existing `__karac_drop_struct_<Type>` synthesiser
    /// internally after running the user body, so registering both
    /// would double-cleanup the fields. Returns `()` either way; falls
    /// through to no-op (no action pushed) when the wrapper isn't in
    /// the cache (shouldn't happen — `emit_user_drop_wrappers` runs
    /// before the function-body compile pass).
    pub(super) fn track_user_drop_var(
        &mut self,
        type_name: &str,
        binding_name: &str,
        binding_ptr: PointerValue<'ctx>,
    ) {
        let drop_fn = match self.user_drop_wrapper_fns.get(type_name) {
            Some(f) => *f,
            None => return,
        };
        if let Some(frame) = self.scope_cleanup_actions.last_mut() {
            frame.push(CleanupAction::UserDrop {
                binding_name: binding_name.to_string(),
                binding_ptr,
                drop_fn,
            });
        }
    }

    /// Move-suppression for user-Drop bindings — remove the
    /// `CleanupAction::UserDrop` entry for `name` from the cleanup
    /// stack so it does NOT fire at scope exit. Used at `let g = f;`
    /// (RHS is an Identifier) when `f`'s value is moved into `g`;
    /// without suppression both bindings would drop the same logical
    /// value, double-closing fds / double-dropping resources. Walks
    /// all frames (inner-most first) so the suppression works even
    /// for moves out of nested scopes — though the v1 caller in
    /// `stmts.rs` only ever suppresses within the current frame
    /// because that's where the source binding lives.
    pub(super) fn suppress_user_drop_for_var(&mut self, name: &str) {
        for frame in self.scope_cleanup_actions.iter_mut().rev() {
            frame.retain(|action| match action {
                CleanupAction::UserDrop { binding_name, .. } => binding_name != name,
                _ => true,
            });
        }
    }

    /// Emit all cleanup actions registered across all scope frames (for function exit).
    /// Iterates frames in reverse (innermost first) and within each frame in reverse
    /// push order (LIFO). LIFO is mandatory for user `defer` per design.md § Drop
    /// ordering within a branch ("last declared, first drained"); compiler-internal
    /// cleanup variants (RcDec, FreeVecBuffer, FreeMapHandle, EnumDrop, StructDrop,
    /// RcDecOption) each touch independent allocations and commute, so reversing
    /// their order is a no-op for correctness.
    ///
    /// **Normal-exit path.** `UserErrDefer` actions are skipped here — they
    /// fire only on error-exit paths (`?`-propagation, explicit `return
    /// Err(...)` / `return None`). Error-exit dispatch goes through
    /// `emit_scope_cleanup_for_error_path` instead, which runs errdefers
    /// in phase 1 before reaching this same drop+defer drain in phase 2.
    pub(super) fn emit_scope_cleanup(&mut self) {
        let vec_ty = self.vec_struct_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();
        let fn_val = self.current_fn.unwrap();

        for frame_idx in (0..self.scope_cleanup_actions.len()).rev() {
            let n = self.scope_cleanup_actions[frame_idx].len();
            for action_idx in (0..n).rev() {
                if matches!(
                    &self.scope_cleanup_actions[frame_idx][action_idx],
                    CleanupAction::UserErrDefer { .. }
                ) {
                    continue;
                }
                self.emit_cleanup_action_at(frame_idx, action_idx, fn_val, vec_ty, ptr_ty, i64_t);
            }
        }
    }

    /// Emit the heap-local drops for a coroutine **destroy/cancel edge** (A2
    /// slice 4 — `docs/spikes/network-async-coroutine-transform.md` § 7 slice
    /// 4). Called from `emit_coro_park_suspend`'s per-park destroy block, where
    /// the live `scope_cleanup_actions` stack is exactly the set of locals live
    /// across that suspend — so a coroutine destroyed *while parked here* frees
    /// exactly the heap a mid-flight cancel would otherwise leak (Vec read
    /// buffers, String, Map/file handles, RC-fallback boxes, struct/enum drops,
    /// user `Drop` impls).
    ///
    /// Mirrors [`Self::emit_scope_cleanup`]'s whole-stack LIFO drain, with two
    /// deliberate differences:
    ///   * It skips **both** `UserDefer` and `UserErrDefer`. These are
    ///     scope-exit *control-flow* constructs, not heap ownership; their
    ///     bodies must not be re-compiled onto every park's destroy edge, and
    ///     defer-on-cancel semantics are out of scope until the slice-5 cancel
    ///     work wires a real teardown trigger. (`emit_scope_cleanup` runs
    ///     `UserDefer` on the normal-completion path; that asymmetry is the
    ///     documented v1 limitation — a destroyed-mid-flight coroutine drops its
    ///     heap but does not run user `defer` blocks.)
    ///   * The frame is **not** freed here — the shared `cleanup_bb`
    ///     (`coro.free`) the destroy block branches into does that. This only
    ///     runs the Kāra-level drops.
    ///
    /// Each remaining action goes through the same `emit_cleanup_action_at` the
    /// normal path uses, so it inherits the null-guards / conditional-init
    /// handling (e.g. `RcDec`'s null-sentinel skip) verbatim. The
    /// completion-path drops and these destroy-edge drops are on mutually
    /// exclusive control-flow paths (a coroutine either runs to completion —
    /// body-end `emit_scope_cleanup` drops, then parks at the final suspend
    /// whose destroy edge is free-only — or is destroyed at a park, reaching
    /// this drain), so no value is dropped twice.
    pub(super) fn emit_coro_destroy_edge_drops(&mut self) {
        let vec_ty = self.vec_struct_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();
        let fn_val = self.current_fn.unwrap();

        for frame_idx in (0..self.scope_cleanup_actions.len()).rev() {
            let n = self.scope_cleanup_actions[frame_idx].len();
            for action_idx in (0..n).rev() {
                if matches!(
                    &self.scope_cleanup_actions[frame_idx][action_idx],
                    CleanupAction::UserDefer(_) | CleanupAction::UserErrDefer { .. }
                ) {
                    continue;
                }
                self.emit_cleanup_action_at(frame_idx, action_idx, fn_val, vec_ty, ptr_ty, i64_t);
            }
        }
    }

    /// Error-exit drain. Per design.md § *Drop ordering within a branch*,
    /// when control exits a scope via an error path (the `?` operator's
    /// Err-propagation branch, an explicit `return Err(...)` or `return
    /// None`), the unified cleanup stack drains in two phases:
    ///
    /// 1. **Phase 1: errdefers.** Every `UserErrDefer` action runs first,
    ///    in reverse declaration order (LIFO), per frame innermost-first.
    /// 2. **Phase 2: drops + defers.** Every other cleanup variant (the
    ///    compiler-internal drops + `UserDefer`) drains in the same
    ///    program-order LIFO `emit_scope_cleanup` uses on normal exit.
    ///
    /// Per-frame interleave (phase 1 then phase 2 within each frame,
    /// innermost frame first) mirrors the interpreter's `run_cleanup`
    /// shape (`src/interpreter/eval_stmt.rs:364-408`): each scope drains
    /// its own errdefers before its own drops, and outer scopes drain in
    /// turn when the error bubbles out. The action stack still excludes
    /// the binding form `errdefer(e) { ... }` per slice 2 — slice 4 will
    /// lift the gate in `compile_stmt` and add the bind-payload step here.
    pub(super) fn emit_scope_cleanup_for_error_path(&mut self) {
        let vec_ty = self.vec_struct_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();
        let fn_val = self.current_fn.unwrap();

        for frame_idx in (0..self.scope_cleanup_actions.len()).rev() {
            let n = self.scope_cleanup_actions[frame_idx].len();
            // Phase 1: errdefers LIFO within this frame.
            for action_idx in (0..n).rev() {
                if matches!(
                    &self.scope_cleanup_actions[frame_idx][action_idx],
                    CleanupAction::UserErrDefer { .. }
                ) {
                    self.emit_cleanup_action_at(
                        frame_idx, action_idx, fn_val, vec_ty, ptr_ty, i64_t,
                    );
                }
            }
            // Phase 2: non-errdefer actions LIFO within this frame.
            for action_idx in (0..n).rev() {
                if matches!(
                    &self.scope_cleanup_actions[frame_idx][action_idx],
                    CleanupAction::UserErrDefer { .. }
                ) {
                    continue;
                }
                self.emit_cleanup_action_at(frame_idx, action_idx, fn_val, vec_ty, ptr_ty, i64_t);
            }
        }
    }

    /// Drain the topmost `scope_cleanup_actions` frame: emit cleanup IR for
    /// every action it holds (in reverse push order — LIFO), then pop the
    /// frame. Used by `compile_match` to fire match-arm-scoped cleanups
    /// (let-bindings inside the arm body, plus the match-arm pattern binding
    /// itself) at end-of-arm instead of end-of-function — without this the
    /// alloca reuse across match-arm iterations leaks all but the last bound
    /// value.
    ///
    /// Caller is responsible for ensuring the basic-block insertion point is
    /// somewhere meaningful (i.e. the arm-body's end before the merge branch).
    /// No-op if the cleanup stack is empty.
    ///
    /// **Normal-exit semantics.** `UserErrDefer` actions in the frame are
    /// skipped — this is a normal-fall-through drain, the error-path drain
    /// goes through `emit_scope_cleanup_for_error_path` instead. The skipped
    /// errdefers are dropped along with the frame on pop, so a block that
    /// registers an `errdefer` but exits normally never fires it.
    pub(super) fn drain_top_frame_with_emit(&mut self) {
        if self.scope_cleanup_actions.is_empty() {
            return;
        }
        let vec_ty = self.vec_struct_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();
        let fn_val = self.current_fn.unwrap();
        let top_idx = self.scope_cleanup_actions.len() - 1;
        let n = self.scope_cleanup_actions[top_idx].len();
        for action_idx in (0..n).rev() {
            if matches!(
                &self.scope_cleanup_actions[top_idx][action_idx],
                CleanupAction::UserErrDefer { .. }
            ) {
                continue;
            }
            self.emit_cleanup_action_at(top_idx, action_idx, fn_val, vec_ty, ptr_ty, i64_t);
        }
        self.scope_cleanup_actions.pop();
    }

    /// Dispatch one cleanup action by `(frame_idx, action_idx)` indices into
    /// `scope_cleanup_actions`. Uses indices rather than a borrowed reference
    /// so user-defer dispatch (`UserDefer(Block)` / `UserErrDefer { .. }`)
    /// can release the borrow, clone the body, and then call `compile_block`
    /// under `&mut self`. Compiler-internal variants take the existing
    /// `&self` `emit_cleanup_action` fast path.
    fn emit_cleanup_action_at(
        &mut self,
        frame_idx: usize,
        action_idx: usize,
        fn_val: FunctionValue<'ctx>,
        vec_ty: StructType<'ctx>,
        ptr_ty: inkwell::types::PointerType<'ctx>,
        i64_t: inkwell::types::IntType<'ctx>,
    ) {
        // Slice 4 (Phase 7 § *defer / errdefer codegen*): split the
        // body extraction so the errdefer binding name can be threaded
        // through to the bind-then-emit dispatch below. `UserDefer` has
        // no binding; `UserErrDefer` carries `Option<String>` — `None`
        // is slice 2's no-binding form (no extra setup), `Some(name)`
        // is slice 4's binding form (allocate, store staged payload,
        // register in `variables`, emit, restore).
        let body_and_binding = match &self.scope_cleanup_actions[frame_idx][action_idx] {
            CleanupAction::UserDefer(block) => Some((block.clone(), None)),
            CleanupAction::UserErrDefer { binding, body } => Some((body.clone(), binding.clone())),
            _ => None,
        };
        if let Some((block, binding)) = body_and_binding {
            // Slice 4: bind the staged Err payload into the body's
            // scope when this is a binding-form errdefer. The payload
            // was staged into `self.pending_errdefer_payload` by the
            // error-exit site (`compile_question`'s `fail_bb`,
            // `ExprKind::Return(Err(...))`, or `compile_function`'s
            // tail `Err(...)` emitter) immediately before
            // `emit_scope_cleanup_for_error_path` ran. Allocate an
            // entry-block alloca of the payload's LLVM type, store
            // the staged value, save the prior `variables[name]` (if
            // any) for restoration after the body emits, then insert
            // the new slot so the body's compile_expr reads of `e`
            // resolve to a fresh load of the bound payload.
            //
            // When the binding is present but no payload is staged
            // (`pending_errdefer_payload` is `None`), the body still
            // emits — without the binding — so an `errdefer(e)` that
            // never sees a runtime error path stays consistent with
            // the no-binding form's drain semantics. In practice all
            // three error-exit sites stage before calling the
            // error-path drain, so the unstaged case is unreachable
            // from a well-formed program; the conservative branch
            // here keeps emission non-fatal.
            let saved_binding: Option<(String, Option<VarSlot<'ctx>>)> =
                if let Some(name) = &binding {
                    if let Some(payload) = self.pending_errdefer_payload {
                        let payload_ty = payload.get_type();
                        let alloca = self.create_entry_alloca(fn_val, name, payload_ty);
                        self.builder.build_store(alloca, payload).unwrap();
                        let prior = self.variables.get(name).copied();
                        self.variables.insert(
                            name.clone(),
                            VarSlot {
                                ptr: alloca,
                                ty: payload_ty,
                            },
                        );
                        Some((name.clone(), prior))
                    } else {
                        None
                    }
                } else {
                    None
                };
            // Slice 1.5: route the defer body through the frame-pushing
            // variant so a nested `defer` inside this body scopes to the
            // defer body itself (drains at end-of-defer-body) instead of
            // bubbling up to the enclosing scope's frame. Also gives the
            // defer body the same runtime-reachability shape as a naked
            // block: a `defer` inside an `if false { ... }` nested in
            // here never fires. The errdefer body (slice 2) reuses this
            // same path so a `defer` inside an errdefer body scopes the
            // same way.
            let _ = self.compile_block_with_frame(&block);
            // Restore any prior binding the errdefer's `e` shadowed.
            // Removing the slot rather than leaving it in `variables`
            // is required: the alloca is live only for the duration of
            // this body's compile, and a subsequent unrelated reference
            // to the same name (in a later errdefer body or the same
            // body re-entered) must not pick up a stale slot.
            if let Some((name, prior)) = saved_binding {
                match prior {
                    Some(slot) => {
                        self.variables.insert(name, slot);
                    }
                    None => {
                        self.variables.remove(&name);
                    }
                }
            }
            return;
        }
        let action_ref = &self.scope_cleanup_actions[frame_idx][action_idx];
        self.emit_cleanup_action(action_ref, fn_val, vec_ty, ptr_ty, i64_t);
    }

    /// Per-action cleanup IR emitter. Extracted from `emit_scope_cleanup` so
    /// the same code path serves both whole-stack drain (function-end /
    /// early-return cleanup) and top-frame drain (per-match-arm cleanup at
    /// `drain_top_frame_with_emit`). Signature takes pre-computed type
    /// handles so the caller hoists them out of inner loops.
    pub(super) fn emit_cleanup_action(
        &self,
        action: &CleanupAction<'ctx>,
        fn_val: FunctionValue<'ctx>,
        vec_ty: StructType<'ctx>,
        ptr_ty: inkwell::types::PointerType<'ctx>,
        i64_t: inkwell::types::IntType<'ctx>,
    ) {
        match action {
            CleanupAction::RcDec {
                name,
                ptr,
                heap_type,
            } => {
                let current_ptr = if let Some(slot) = self.variables.get(name) {
                    self.builder
                        .build_load(ptr_ty, slot.ptr, &format!("{}_rc_cleanup", name))
                        .unwrap()
                        .into_pointer_value()
                } else {
                    *ptr
                };
                // Null-guard the dec: body-local shared-struct slots
                // whose let-binding never executed (the enclosing loop
                // body or conditional branch was skipped) carry a
                // null sentinel — `track_rc_var` emits a `store null`
                // at function entry. Without the guard, the dec
                // dereferences null (or stale memory) and hangs in
                // macOS malloc's bookkeeping pages. Skip when null;
                // otherwise dispatch through `emit_refcount_dec` as
                // before.
                let null = ptr_ty.const_null();
                let is_null = self
                    .builder
                    .build_int_compare(IntPredicate::EQ, current_ptr, null, "rc_is_null")
                    .unwrap();
                let skip_bb = self.context.append_basic_block(fn_val, "rc_cleanup_skip");
                let do_bb = self.context.append_basic_block(fn_val, "rc_cleanup_do");
                let join_bb = self.context.append_basic_block(fn_val, "rc_cleanup_join");
                self.builder
                    .build_conditional_branch(is_null, skip_bb, do_bb)
                    .unwrap();
                self.builder.position_at_end(do_bb);
                self.emit_refcount_dec(name, *heap_type, current_ptr);
                self.builder.build_unconditional_branch(join_bb).unwrap();
                self.builder.position_at_end(skip_bb);
                self.builder.build_unconditional_branch(join_bb).unwrap();
                self.builder.position_at_end(join_bb);
            }
            CleanupAction::FreeVecBuffer {
                vec_alloca,
                elem_ty,
            } => {
                let cap_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, *vec_alloca, 2, "cleanup.cap.ptr")
                    .unwrap();
                let cap = self
                    .builder
                    .build_load(i64_t, cap_ptr, "cleanup.cap")
                    .unwrap()
                    .into_int_value();
                let zero = i64_t.const_int(0, false);
                let is_heap = self
                    .builder
                    .build_int_compare(IntPredicate::UGT, cap, zero, "is_heap")
                    .unwrap();
                let free_bb = self.context.append_basic_block(fn_val, "cleanup.free");
                let skip_bb = self.context.append_basic_block(fn_val, "cleanup.skip");
                self.builder
                    .build_conditional_branch(is_heap, free_bb, skip_bb)
                    .unwrap();
                self.builder.position_at_end(free_bb);
                let data_ptr_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, *vec_alloca, 0, "cleanup.data.ptr")
                    .unwrap();
                let data = self
                    .builder
                    .build_load(ptr_ty, data_ptr_ptr, "cleanup.data")
                    .unwrap()
                    .into_pointer_value();

                // Recursive-drop fast path: when the element type is
                // itself a Vec/String struct, each live element owns
                // a separate data buffer. Iterate `len` elements and
                // free each one's `data` pointer before releasing
                // the outer buffer; otherwise those inner buffers
                // leak. Closes the 2026-05-13 cumulative-retention
                // bug measured on LeetCode #3629 bfs_sieve, where
                // `Vec[Vec[i64]]` leaked ~32 MB per `min_jumps`
                // call. One-level recursion handles the bench
                // workloads and the documented common case
                // (`Vec[Vec[T]]`, `Vec[String]`); deeper nesting
                // (`Vec[Vec[Vec[T]]]`) still leaks the innermost
                // buffers — tracked as a follow-up in `deferred.md`
                // § *Recursive Drop for Heap-Owned Collection
                // Elements > deeper-nesting limitation*.
                if let Some(et) = elem_ty {
                    if self.llvm_ty_is_vec_struct(*et) {
                        let len_ptr = self
                            .builder
                            .build_struct_gep(vec_ty, *vec_alloca, 1, "cleanup.len.ptr")
                            .unwrap();
                        let len = self
                            .builder
                            .build_load(i64_t, len_ptr, "cleanup.len")
                            .unwrap()
                            .into_int_value();
                        let counter =
                            self.create_entry_alloca(fn_val, "cleanup.drop.i", i64_t.into());
                        self.builder.build_store(counter, zero).unwrap();
                        let drop_cond_bb =
                            self.context.append_basic_block(fn_val, "cleanup.drop.cond");
                        let drop_body_bb =
                            self.context.append_basic_block(fn_val, "cleanup.drop.body");
                        let drop_after_bb = self
                            .context
                            .append_basic_block(fn_val, "cleanup.drop.after");
                        self.builder
                            .build_unconditional_branch(drop_cond_bb)
                            .unwrap();

                        self.builder.position_at_end(drop_cond_bb);
                        let cur = self
                            .builder
                            .build_load(i64_t, counter, "cleanup.drop.cur")
                            .unwrap()
                            .into_int_value();
                        let lt = self
                            .builder
                            .build_int_compare(IntPredicate::ULT, cur, len, "cleanup.drop.lt")
                            .unwrap();
                        self.builder
                            .build_conditional_branch(lt, drop_body_bb, drop_after_bb)
                            .unwrap();

                        self.builder.position_at_end(drop_body_bb);
                        // Each element is a Vec struct `{ptr, len,
                        // cap}` at `data + i * sizeof(VecStruct)`.
                        // Check inner cap > 0, then free inner ptr.
                        let inner_struct_ptr = unsafe {
                            self.builder
                                .build_gep(
                                    self.vec_struct_type(),
                                    data,
                                    &[cur],
                                    "cleanup.drop.elem",
                                )
                                .unwrap()
                        };
                        let inner_cap_ptr = self
                            .builder
                            .build_struct_gep(
                                self.vec_struct_type(),
                                inner_struct_ptr,
                                2,
                                "cleanup.drop.inner.cap.ptr",
                            )
                            .unwrap();
                        let inner_cap = self
                            .builder
                            .build_load(i64_t, inner_cap_ptr, "cleanup.drop.inner.cap")
                            .unwrap()
                            .into_int_value();
                        let inner_is_heap = self
                            .builder
                            .build_int_compare(
                                IntPredicate::UGT,
                                inner_cap,
                                zero,
                                "cleanup.drop.inner.is_heap",
                            )
                            .unwrap();
                        let inner_free_bb = self
                            .context
                            .append_basic_block(fn_val, "cleanup.drop.inner.free");
                        let inner_skip_bb = self
                            .context
                            .append_basic_block(fn_val, "cleanup.drop.inner.skip");
                        self.builder
                            .build_conditional_branch(inner_is_heap, inner_free_bb, inner_skip_bb)
                            .unwrap();

                        self.builder.position_at_end(inner_free_bb);
                        let inner_data_ptr = self
                            .builder
                            .build_struct_gep(
                                self.vec_struct_type(),
                                inner_struct_ptr,
                                0,
                                "cleanup.drop.inner.data.ptr",
                            )
                            .unwrap();
                        let inner_data = self
                            .builder
                            .build_load(ptr_ty, inner_data_ptr, "cleanup.drop.inner.data")
                            .unwrap()
                            .into_pointer_value();
                        self.builder
                            .build_call(self.free_fn, &[inner_data.into()], "")
                            .unwrap();
                        self.builder
                            .build_unconditional_branch(inner_skip_bb)
                            .unwrap();

                        self.builder.position_at_end(inner_skip_bb);
                        let one = i64_t.const_int(1, false);
                        let next = self
                            .builder
                            .build_int_add(cur, one, "cleanup.drop.next")
                            .unwrap();
                        self.builder.build_store(counter, next).unwrap();
                        self.builder
                            .build_unconditional_branch(drop_cond_bb)
                            .unwrap();

                        self.builder.position_at_end(drop_after_bb);
                    }
                }

                self.builder
                    .build_call(self.free_fn, &[data.into()], "")
                    .unwrap();
                self.builder.build_unconditional_branch(skip_bb).unwrap();
                self.builder.position_at_end(skip_bb);
            }
            CleanupAction::FreeSoaGroups {
                soa_alloca,
                soa_struct_ty,
                num_hot_groups,
                has_cold,
            } => {
                // cap > 0 ⇒ groups were allocated. Read cap via the SoA
                // struct type so the GEP lands on the actual cap slot
                // (last field), not whichever slot collides with the
                // plain Vec `{ptr,len,cap}` layout's field 2.
                let cap_idx = *num_hot_groups + if *has_cold { 1 } else { 0 } + 1;
                let cap_ptr = self
                    .builder
                    .build_struct_gep(*soa_struct_ty, *soa_alloca, cap_idx, "soa.cleanup.cap.ptr")
                    .unwrap();
                let cap = self
                    .builder
                    .build_load(i64_t, cap_ptr, "soa.cleanup.cap")
                    .unwrap()
                    .into_int_value();
                let zero = i64_t.const_int(0, false);
                let is_heap = self
                    .builder
                    .build_int_compare(IntPredicate::UGT, cap, zero, "soa.cleanup.is_heap")
                    .unwrap();
                let free_bb = self.context.append_basic_block(fn_val, "soa.cleanup.free");
                let skip_bb = self.context.append_basic_block(fn_val, "soa.cleanup.skip");
                self.builder
                    .build_conditional_branch(is_heap, free_bb, skip_bb)
                    .unwrap();

                self.builder.position_at_end(free_bb);
                // Free each hot group buffer in declaration order, then the
                // cold buffer if present. Each group is its own malloc
                // (see `compile_soa_method`'s push-grow loop); a single
                // `free(g0)` leaks the rest.
                let total_ptrs = *num_hot_groups + if *has_cold { 1 } else { 0 };
                for gi in 0..total_ptrs {
                    let grp_ptr_ptr = self
                        .builder
                        .build_struct_gep(
                            *soa_struct_ty,
                            *soa_alloca,
                            gi,
                            &format!("soa.cleanup.g{}.ptr", gi),
                        )
                        .unwrap();
                    let grp_ptr = self
                        .builder
                        .build_load(ptr_ty, grp_ptr_ptr, &format!("soa.cleanup.g{}.buf", gi))
                        .unwrap()
                        .into_pointer_value();
                    self.builder
                        .build_call(self.free_fn, &[grp_ptr.into()], "")
                        .unwrap();
                }
                self.builder.build_unconditional_branch(skip_bb).unwrap();
                self.builder.position_at_end(skip_bb);
            }
            CleanupAction::FreeMapHandle {
                map_alloca,
                key_is_vec,
                val_is_vec,
                val_shared_heap_type,
                key_shared_heap_type,
            } => {
                let handle = self
                    .builder
                    .build_load(ptr_ty, *map_alloca, "cleanup.map.handle")
                    .unwrap()
                    .into_pointer_value();
                // Shared-half rc_dec walks MUST run before the runtime
                // helper releases the bucket storage — they read each
                // live slot's bytes from `kv[]`. Closes the `Map[K,
                // shared T]` leak (2026-05-16) on the value side, and
                // the `Map[shared K, V]` / `Set[shared T]` leak on the
                // key side. Both fire when both K and V are shared.
                // Type-erased runtime can't decrement refcounts itself
                // because it doesn't know each half's heap layout;
                // codegen does, so the dec is open-coded per-
                // instantiation against the matching
                // `SharedTypeInfo.heap_type`.
                if let Some(heap_ty) = val_shared_heap_type {
                    self.emit_map_shared_half_rc_dec_walk(handle, *heap_ty, true);
                }
                if let Some(heap_ty) = key_shared_heap_type {
                    self.emit_map_shared_half_rc_dec_walk(handle, *heap_ty, false);
                }
                // When either the key or value type follows the Vec/String
                // `{ptr, len, cap}` layout, route through the recursive-
                // drop runtime helper so each live entry's heap content
                // is freed before the bucket array is deallocated. Plain
                // `karac_map_free` is correct only when both sides own
                // no heap. Closes the 2026-05-13 bucket leak (LeetCode
                // #3629 `Map[i64, Vec[i64]]`) and the 2026-05-14
                // `Set[String]` / `Map[String, V]` leaks (slice α /
                // β of the recursive-drop work).
                if *key_is_vec || *val_is_vec {
                    let i32_t = self.context.i32_type();
                    let key_flag = i32_t.const_int(if *key_is_vec { 1 } else { 0 }, false);
                    let val_flag = i32_t.const_int(if *val_is_vec { 1 } else { 0 }, false);
                    self.builder
                        .build_call(
                            self.karac_map_free_with_drop_vec_fn,
                            &[handle.into(), key_flag.into(), val_flag.into()],
                            "",
                        )
                        .unwrap();
                } else {
                    self.builder
                        .build_call(self.karac_map_free_fn, &[handle.into()], "")
                        .unwrap();
                }
            }
            // Phase 8 `File` handle slice F4b — close the file fd at
            // scope exit. Load the handle from its alloca, hand it to
            // `karac_runtime_file_close` which reconstructs the Box
            // and drops it (releasing the OS fd via std::fs::File's
            // own Drop). Null-handle is a no-op on the runtime side.
            CleanupAction::FreeFileHandle { file_alloca } => {
                let handle = self
                    .builder
                    .build_load(ptr_ty, *file_alloca, "cleanup.file.handle")
                    .unwrap()
                    .into_pointer_value();
                let close_fn = self
                    .module
                    .get_function("karac_runtime_file_close")
                    .expect("karac_runtime_file_close declared in Codegen::new");
                self.builder
                    .build_call(close_fn, &[handle.into()], "")
                    .unwrap();
            }
            // Phase 7.2 Slice DP — invoke the per-enum drop
            // function on the alloca. The drop fn takes a
            // pointer to the enum struct and walks the tag-
            // switch / per-variant cleanup BBs internally.
            CleanupAction::EnumDrop {
                enum_alloca,
                drop_fn,
            } => {
                self.builder
                    .build_call(*drop_fn, &[(*enum_alloca).into()], "")
                    .unwrap();
            }
            CleanupAction::StructDrop {
                struct_alloca,
                drop_fn,
            } => {
                self.builder
                    .build_call(*drop_fn, &[(*struct_alloca).into()], "")
                    .unwrap();
            }
            // Phase 7 user-`impl Drop` dispatch Prereq.3 — invoke the
            // per-type wrapper `karac_drop_<Type>` on the binding. The
            // wrapper internally calls the user-defined `<Type>.drop`
            // method body, then (when the type has heap-owning fields)
            // hands off to the existing `__karac_drop_struct_<Type>`
            // field cleanup synthesiser. Registration at let-binding
            // time is mutually exclusive with `StructDrop`, so this
            // path is the unique field-cleanup invocation for types
            // with a user Drop impl.
            CleanupAction::UserDrop {
                binding_name: _,
                binding_ptr,
                drop_fn,
            } => {
                self.builder
                    .build_call(*drop_fn, &[(*binding_ptr).into()], "")
                    .unwrap();
            }
            // `Option[shared T]` binding — load the tag, branch on
            // Some, recover the inner pointer from word 0, dispatch
            // through `emit_refcount_dec`. None side is a no-op (no
            // inner heap allocation to release). Mirrors the `RcDec`
            // arm's reload-from-slot discipline so a reassignment of
            // the binding is observed at scope exit; mirrors the
            // null-guard shape but on the tag instead of a pointer
            // (`tag == None` is the "skip" path here).
            CleanupAction::RcDecOption {
                name,
                option_slot,
                option_ty,
                heap_type,
                some_tag,
            } => {
                // GEP to tag (field 0), load, compare with Some-tag.
                let tag_ptr = self
                    .builder
                    .build_struct_gep(
                        *option_ty,
                        *option_slot,
                        0,
                        &format!("{}_opt_tag_ptr", name),
                    )
                    .unwrap();
                let tag = self
                    .builder
                    .build_load(i64_t, tag_ptr, &format!("{}_opt_tag", name))
                    .unwrap()
                    .into_int_value();
                let some_tag_const = i64_t.const_int(*some_tag, false);
                let is_some = self
                    .builder
                    .build_int_compare(
                        IntPredicate::EQ,
                        tag,
                        some_tag_const,
                        &format!("{}_opt_is_some", name),
                    )
                    .unwrap();
                let do_bb = self.context.append_basic_block(fn_val, "opt_rc_cleanup_do");
                let skip_bb = self
                    .context
                    .append_basic_block(fn_val, "opt_rc_cleanup_skip");
                let join_bb = self
                    .context
                    .append_basic_block(fn_val, "opt_rc_cleanup_join");
                self.builder
                    .build_conditional_branch(is_some, do_bb, skip_bb)
                    .unwrap();
                // Some-side: load w0 (field 1) as i64, int_to_ptr,
                // dec. The Some-side inner pointer can itself be null
                // in malformed-IR cases — defensive null-skip mirrors
                // the `RcDec` arm so a hypothetical future codegen
                // shape that stores a sentinel-null doesn't crash the
                // dec. The common case (a real Some(ptr) payload) has
                // a non-null pointer.
                self.builder.position_at_end(do_bb);
                let w0_ptr = self
                    .builder
                    .build_struct_gep(*option_ty, *option_slot, 1, &format!("{}_opt_w0_ptr", name))
                    .unwrap();
                let w0 = self
                    .builder
                    .build_load(i64_t, w0_ptr, &format!("{}_opt_w0", name))
                    .unwrap()
                    .into_int_value();
                let inner_ptr = self
                    .builder
                    .build_int_to_ptr(w0, ptr_ty, &format!("{}_opt_inner_ptr", name))
                    .unwrap();
                let inner_null = ptr_ty.const_null();
                let inner_is_null = self
                    .builder
                    .build_int_compare(
                        IntPredicate::EQ,
                        inner_ptr,
                        inner_null,
                        &format!("{}_opt_inner_is_null", name),
                    )
                    .unwrap();
                let inner_do_bb = self
                    .context
                    .append_basic_block(fn_val, "opt_rc_cleanup_inner_do");
                let inner_skip_bb = self
                    .context
                    .append_basic_block(fn_val, "opt_rc_cleanup_inner_skip");
                self.builder
                    .build_conditional_branch(inner_is_null, inner_skip_bb, inner_do_bb)
                    .unwrap();
                self.builder.position_at_end(inner_do_bb);
                self.emit_refcount_dec(name, *heap_type, inner_ptr);
                self.builder
                    .build_unconditional_branch(inner_skip_bb)
                    .unwrap();
                self.builder.position_at_end(inner_skip_bb);
                self.builder.build_unconditional_branch(join_bb).unwrap();
                self.builder.position_at_end(skip_bb);
                self.builder.build_unconditional_branch(join_bb).unwrap();
                self.builder.position_at_end(join_bb);
            }
            CleanupAction::UserDefer(_) => {
                // Routed through `emit_cleanup_action_at` instead — user-defer
                // bodies require `&mut self` to compile a Block, while this
                // function is `&self`. The indirection at the drain sites
                // (`emit_scope_cleanup` / `drain_top_frame_with_emit`) splits
                // the UserDefer case out before reaching this match.
                unreachable!(
                    "CleanupAction::UserDefer must be dispatched via emit_cleanup_action_at"
                );
            }
            CleanupAction::UserErrDefer { .. } => {
                // Routed through `emit_cleanup_action_at` instead — same
                // shape as UserDefer (the errdefer body needs `&mut self`
                // to compile a Block). On normal-exit drains
                // (`emit_scope_cleanup` / `drain_top_frame_with_emit`)
                // errdefers are filtered out before reaching this match;
                // on error-exit drains (`emit_scope_cleanup_for_error_path`)
                // errdefers are routed via `emit_cleanup_action_at` in
                // phase 1. Reaching this arm means the cleanup-action
                // index walked an errdefer slot on a normal-exit path,
                // which is a routing bug.
                unreachable!(
                    "CleanupAction::UserErrDefer must be dispatched via emit_cleanup_action_at on an error-exit path"
                );
            }
        }
    }

    /// Walk every live bucket of `map_handle` and emit `rc_dec` on
    /// one half of the slot — value when `is_val == true`, key when
    /// `is_val == false`. Used by `FreeMapHandle` cleanup when the
    /// corresponding side is a shared struct / shared enum — the
    /// type-erased runtime (`karac_map_free_with_drop_vec`) only
    /// knows the Vec/String `{ptr, len, cap}` layout, so per-K / per-V
    /// refcount decrements have to be open-coded at the cleanup site
    /// against the matching `SharedTypeInfo.heap_type`. Mirrors the
    /// bucket-walk shape in `karac_map_free_with_drop_vec`
    /// (`runtime/src/map.rs`): for each `slot in 0..capacity`, check
    /// `status[slot] == OCCUPIED`, then load the half's pointer from
    /// `kv[slot*stride + offset]` (`offset = 0` for key, `key_size`
    /// for val) and rc_dec it.
    ///
    /// **Layout dependence.** Reads `capacity`, `status`, `kv`,
    /// `key_size`, `val_size` from the runtime's `#[repr(C)]`
    /// `KaracMap` at the offsets pinned by the runtime-side
    /// `karac_map_field_offsets_match_codegen` unit test. `key_size`
    /// and `val_size` are loaded at runtime (not const-folded from
    /// K/V LLVM widths) so the walk stays agnostic of K's / V's
    /// exact representation — the `kv` byte array's stride is
    /// `(key_size + val_size)` bytes, with the val half starting
    /// at `+key_size` and the key half at `+0`.
    ///
    /// **Concurrency.** The walk uses `emit_rc_dec` (non-atomic)
    /// rather than `emit_arc_dec`. Maps are local to a single thread
    /// (`unsafe impl Send for KaracMap`), and the cleanup runs on
    /// the thread that owns the Map, so non-atomic is correct here.
    /// If a future change shares Maps across threads via Arc, this
    /// callsite needs the atomic dispatch — same shape as the
    /// `emit_refcount_dec` decision in `RcDec` cleanup, but the
    /// map's keys / values aren't named bindings, so the
    /// `is_arc_binding` check has no anchor; an explicit `is_arc`
    /// flag on `FreeMapHandle` would be the path then.
    pub(super) fn emit_map_shared_half_rc_dec_walk(
        &self,
        map_handle: PointerValue<'ctx>,
        heap_type: StructType<'ctx>,
        is_val: bool,
    ) {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i8_t = self.context.i8_type();
        let i64_t = self.context.i64_type();
        let fn_val = self.current_fn.unwrap();

        // Runtime layout offsets (pinned by
        // `karac_map_field_offsets_match_codegen`):
        //   0..8   status   *u8
        //   8..16  kv       *u8
        //   16..24 capacity usize
        //   24..32 len      usize
        //   32..40 tombstones usize
        //   40..48 key_size usize
        //   48..56 val_size usize
        const STATUS_OFFSET: u64 = 0;
        const KV_OFFSET: u64 = 8;
        const CAPACITY_OFFSET: u64 = 16;
        const KEY_SIZE_OFFSET: u64 = 40;
        const VAL_SIZE_OFFSET: u64 = 48;
        const BUCKET_OCCUPIED: u64 = 1;

        // Null guard — the registration site stores a fresh
        // `karac_map_new` handle which is non-null, but defensive
        // null-skip matches the runtime helper's first check
        // (`if map.is_null() { return; }`) so the cleanup is
        // robust against any future code path that might leave
        // the alloca uninitialized.
        let is_null = self
            .builder
            .build_is_null(map_handle, "cleanup.map.shared.is_null")
            .unwrap();
        let null_skip_bb = self
            .context
            .append_basic_block(fn_val, "cleanup.map.shared.null.skip");
        let walk_entry_bb = self
            .context
            .append_basic_block(fn_val, "cleanup.map.shared.walk.entry");
        self.builder
            .build_conditional_branch(is_null, null_skip_bb, walk_entry_bb)
            .unwrap();

        // ── walk.entry: load capacity, status, kv, key_size ─────
        self.builder.position_at_end(walk_entry_bb);
        let cap_p = unsafe {
            self.builder
                .build_in_bounds_gep(
                    i8_t,
                    map_handle,
                    &[i64_t.const_int(CAPACITY_OFFSET, false)],
                    "cleanup.map.shared.cap.p",
                )
                .unwrap()
        };
        let capacity = self
            .builder
            .build_load(i64_t, cap_p, "cleanup.map.shared.cap")
            .unwrap()
            .into_int_value();
        let status_pp = unsafe {
            self.builder
                .build_in_bounds_gep(
                    i8_t,
                    map_handle,
                    &[i64_t.const_int(STATUS_OFFSET, false)],
                    "cleanup.map.shared.status.pp",
                )
                .unwrap()
        };
        let status_ptr = self
            .builder
            .build_load(ptr_ty, status_pp, "cleanup.map.shared.status")
            .unwrap()
            .into_pointer_value();
        let kv_pp = unsafe {
            self.builder
                .build_in_bounds_gep(
                    i8_t,
                    map_handle,
                    &[i64_t.const_int(KV_OFFSET, false)],
                    "cleanup.map.shared.kv.pp",
                )
                .unwrap()
        };
        let kv_ptr = self
            .builder
            .build_load(ptr_ty, kv_pp, "cleanup.map.shared.kv")
            .unwrap()
            .into_pointer_value();
        let key_size_p = unsafe {
            self.builder
                .build_in_bounds_gep(
                    i8_t,
                    map_handle,
                    &[i64_t.const_int(KEY_SIZE_OFFSET, false)],
                    "cleanup.map.shared.ks.p",
                )
                .unwrap()
        };
        let key_size = self
            .builder
            .build_load(i64_t, key_size_p, "cleanup.map.shared.ks")
            .unwrap()
            .into_int_value();
        let val_size_p = unsafe {
            self.builder
                .build_in_bounds_gep(
                    i8_t,
                    map_handle,
                    &[i64_t.const_int(VAL_SIZE_OFFSET, false)],
                    "cleanup.map.shared.vs.p",
                )
                .unwrap()
        };
        let val_size = self
            .builder
            .build_load(i64_t, val_size_p, "cleanup.map.shared.vs")
            .unwrap()
            .into_int_value();
        let stride = self
            .builder
            .build_int_add(key_size, val_size, "cleanup.map.shared.stride")
            .unwrap();

        // Loop counter alloca'd in entry block.
        let counter = self.create_entry_alloca(fn_val, "cleanup.map.shared.i", i64_t.into());
        self.builder
            .build_store(counter, i64_t.const_zero())
            .unwrap();

        let cond_bb = self
            .context
            .append_basic_block(fn_val, "cleanup.map.shared.loop.cond");
        let body_bb = self
            .context
            .append_basic_block(fn_val, "cleanup.map.shared.loop.body");
        let occupied_bb = self
            .context
            .append_basic_block(fn_val, "cleanup.map.shared.loop.occupied");
        let next_bb = self
            .context
            .append_basic_block(fn_val, "cleanup.map.shared.loop.next");
        let exit_bb = self
            .context
            .append_basic_block(fn_val, "cleanup.map.shared.loop.exit");

        self.builder.build_unconditional_branch(cond_bb).unwrap();

        // ── loop.cond: i < capacity? ──────────────────────────────
        self.builder.position_at_end(cond_bb);
        let i_val = self
            .builder
            .build_load(i64_t, counter, "cleanup.map.shared.i.cur")
            .unwrap()
            .into_int_value();
        let cont = self
            .builder
            .build_int_compare(
                IntPredicate::ULT,
                i_val,
                capacity,
                "cleanup.map.shared.cont",
            )
            .unwrap();
        self.builder
            .build_conditional_branch(cont, body_bb, exit_bb)
            .unwrap();

        // ── loop.body: load status[i], occupied? ──────────────────
        self.builder.position_at_end(body_bb);
        let status_slot_p = unsafe {
            self.builder
                .build_in_bounds_gep(
                    i8_t,
                    status_ptr,
                    &[i_val],
                    "cleanup.map.shared.status.slot.p",
                )
                .unwrap()
        };
        let status_byte = self
            .builder
            .build_load(i8_t, status_slot_p, "cleanup.map.shared.status.byte")
            .unwrap()
            .into_int_value();
        let is_occupied = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                status_byte,
                i8_t.const_int(BUCKET_OCCUPIED, false),
                "cleanup.map.shared.is_occupied",
            )
            .unwrap();
        self.builder
            .build_conditional_branch(is_occupied, occupied_bb, next_bb)
            .unwrap();

        // ── loop.occupied: rc_dec value pointer ───────────────────
        self.builder.position_at_end(occupied_bb);
        let slot_off = self
            .builder
            .build_int_mul(i_val, stride, "cleanup.map.shared.slot.off")
            .unwrap();
        let slot_kv_p = unsafe {
            self.builder
                .build_in_bounds_gep(i8_t, kv_ptr, &[slot_off], "cleanup.map.shared.slot.kv.p")
                .unwrap()
        };
        // Key half lives at offset 0 within the bucket (`slot_kv_p`);
        // value half lives at `+key_size`. Both are pointer-sized on
        // shared types (rc-managed heap-pointer values are 8 bytes
        // on 64-bit).
        let half_ptr_p = if is_val {
            unsafe {
                self.builder
                    .build_in_bounds_gep(
                        i8_t,
                        slot_kv_p,
                        &[key_size],
                        "cleanup.map.shared.slot.val.p",
                    )
                    .unwrap()
            }
        } else {
            slot_kv_p
        };
        let half_ptr = self
            .builder
            .build_load(
                ptr_ty,
                half_ptr_p,
                if is_val {
                    "cleanup.map.shared.val.ptr"
                } else {
                    "cleanup.map.shared.key.ptr"
                },
            )
            .unwrap()
            .into_pointer_value();
        self.emit_rc_dec(heap_type, half_ptr);
        self.builder.build_unconditional_branch(next_bb).unwrap();

        // ── loop.next: i++, branch back to cond ──────────────────
        self.builder.position_at_end(next_bb);
        let i_next = self
            .builder
            .build_int_add(
                i_val,
                i64_t.const_int(1, false),
                "cleanup.map.shared.i.next",
            )
            .unwrap();
        self.builder.build_store(counter, i_next).unwrap();
        self.builder.build_unconditional_branch(cond_bb).unwrap();

        // ── loop.exit: fall through to null.skip via uncond jump ─
        self.builder.position_at_end(exit_bb);
        self.builder
            .build_unconditional_branch(null_skip_bb)
            .unwrap();

        // Continuation point — both the null-guard and the loop
        // funnel here so the caller can continue emitting the
        // `karac_map_free*` runtime call after this helper returns.
        self.builder.position_at_end(null_skip_bb);
    }

    // ── F-string helpers ──────────────────────────────────────────

    /// Append `src_len` bytes from `src_ptr` to the String (Vec<u8>) alloca at
    /// `dest_alloca`, growing the buffer if necessary.  Mirrors the inline
    /// `push_str` logic in `compile_vec_method`.
    pub(super) fn emit_string_append_raw(
        &mut self,
        dest_alloca: PointerValue<'ctx>,
        src_ptr: PointerValue<'ctx>,
        src_len: inkwell::values::IntValue<'ctx>,
    ) {
        let vec_ty = self.vec_struct_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();
        let fn_val = self.current_fn.unwrap();

        let data_ptr_ptr = self
            .builder
            .build_struct_gep(vec_ty, dest_alloca, 0, "fsa.data.pp")
            .unwrap();
        let len_ptr = self
            .builder
            .build_struct_gep(vec_ty, dest_alloca, 1, "fsa.len.ptr")
            .unwrap();
        let cap_ptr = self
            .builder
            .build_struct_gep(vec_ty, dest_alloca, 2, "fsa.cap.ptr")
            .unwrap();

        let data = self
            .builder
            .build_load(ptr_ty, data_ptr_ptr, "fsa.data")
            .unwrap()
            .into_pointer_value();
        let len = self
            .builder
            .build_load(i64_t, len_ptr, "fsa.len")
            .unwrap()
            .into_int_value();
        let cap = self
            .builder
            .build_load(i64_t, cap_ptr, "fsa.cap")
            .unwrap()
            .into_int_value();

        let new_len = self
            .builder
            .build_int_add(len, src_len, "fsa.new_len")
            .unwrap();

        // Grow if new_len > cap.
        let grow_bb = self.context.append_basic_block(fn_val, "fsa.grow");
        let copy_bb = self.context.append_basic_block(fn_val, "fsa.copy");
        let needs_grow = self
            .builder
            .build_int_compare(IntPredicate::UGT, new_len, cap, "fsa.needs_grow")
            .unwrap();
        self.builder
            .build_conditional_branch(needs_grow, grow_bb, copy_bb)
            .unwrap();

        // Grow path: compute new_cap, malloc, memcpy old data, free old, update alloca.
        self.builder.position_at_end(grow_bb);
        let two = i64_t.const_int(2, false);
        let four = i64_t.const_int(4, false);
        let doubled = self.builder.build_int_mul(cap, two, "fsa.doubled").unwrap();
        let cmp1 = self
            .builder
            .build_int_compare(IntPredicate::UGT, doubled, four, "fsa.cmp1")
            .unwrap();
        let growth_min = self
            .builder
            .build_select(cmp1, doubled, four, "fsa.gmin")
            .unwrap()
            .into_int_value();
        let cmp2 = self
            .builder
            .build_int_compare(IntPredicate::UGT, new_len, growth_min, "fsa.cmp2")
            .unwrap();
        let new_cap = self
            .builder
            .build_select(cmp2, new_len, growth_min, "fsa.new_cap")
            .unwrap()
            .into_int_value();
        let new_buf = self
            .builder
            .build_call(self.malloc_fn, &[new_cap.into()], "fsa.new_buf")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        // Copy existing data into new buffer (memcpy with len=0 is safe per C spec).
        self.builder.build_memcpy(new_buf, 1, data, 1, len).unwrap();
        // Free old heap buffer (free(null) is a no-op per C spec).
        self.builder
            .build_call(self.free_fn, &[data.into()], "")
            .unwrap();
        // Update data pointer and cap in the alloca.
        self.builder.build_store(data_ptr_ptr, new_buf).unwrap();
        self.builder.build_store(cap_ptr, new_cap).unwrap();
        self.builder.build_unconditional_branch(copy_bb).unwrap();

        // Copy path: reload cur data (updated by grow, or unchanged), memcpy src.
        self.builder.position_at_end(copy_bb);
        let cur_data = self
            .builder
            .build_load(ptr_ty, data_ptr_ptr, "fsa.cur_data")
            .unwrap()
            .into_pointer_value();
        let i8_ty = self.context.i8_type();
        let dest = unsafe {
            self.builder
                .build_gep(i8_ty, cur_data, &[len], "fsa.dest")
                .unwrap()
        };
        self.builder
            .build_memcpy(dest, 1, src_ptr, 1, src_len)
            .unwrap();
        self.builder.build_store(len_ptr, new_len).unwrap();
    }

    /// Convert a compiled value to `(raw_ptr, byte_len)` for f-string interpolation.
    /// Dispatches on the LLVM type so callers don't need to track the Kāra type name.
    ///
    /// - `String` (3-field struct) → extract (data_ptr, len)
    /// - `bool` (i1) → global "true"/"false" literal
    /// - float (f32/f64) → snprintf "%g" into a 64-byte stack buffer
    /// - integer → snprintf "%lld" / "%llu" into a 64-byte stack buffer
    ///
    /// `source_expr` carries the originating Kāra expression so the integer
    /// arm can pick signed/unsigned widening via `expr_is_unsigned_int` —
    /// mirrors the fix in `compile_print` (2026-05-19). Pre-fix this arm
    /// passed narrow ints (e.g. `i32`) raw to `%lld`, which printf reads as
    /// 64 bits and produces the unsigned reinterpretation on negatives
    /// (`i32 -123` → `4294967173` inside an f-string).
    pub(super) fn compile_fstr_part_to_cstr(
        &mut self,
        val: BasicValueEnum<'ctx>,
        source_expr: &Expr,
    ) -> (PointerValue<'ctx>, inkwell::values::IntValue<'ctx>) {
        let i64_t = self.context.i64_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let fn_val = self.current_fn.unwrap();

        match val {
            BasicValueEnum::StructValue(sv) => {
                // Treat as String: field 0 = ptr, field 1 = len.
                let ptr = self
                    .builder
                    .build_extract_value(sv, 0, "fst.ptr")
                    .unwrap()
                    .into_pointer_value();
                let len = self
                    .builder
                    .build_extract_value(sv, 1, "fst.len")
                    .unwrap()
                    .into_int_value();
                (ptr, len)
            }
            BasicValueEnum::IntValue(iv) if iv.get_type().get_bit_width() == 1 => {
                // bool
                let true_str = self
                    .builder
                    .build_global_string_ptr("true", "fst.true")
                    .unwrap();
                let false_str = self
                    .builder
                    .build_global_string_ptr("false", "fst.false")
                    .unwrap();
                let four = i64_t.const_int(4, false);
                let five = i64_t.const_int(5, false);
                let ptr = self
                    .builder
                    .build_select(
                        iv,
                        true_str.as_pointer_value(),
                        false_str.as_pointer_value(),
                        "fst.bptr",
                    )
                    .unwrap()
                    .into_pointer_value();
                let len = self
                    .builder
                    .build_select(iv, four, five, "fst.blen")
                    .unwrap()
                    .into_int_value();
                (ptr, len)
            }
            _ => {
                // Integer or float: use snprintf into a 64-byte stack buffer.
                let buf_size = i64_t.const_int(64, false);
                let buf = self.create_entry_alloca(
                    fn_val,
                    "fst.buf",
                    self.context.i8_type().array_type(64).into(),
                );
                let buf_ptr = self
                    .builder
                    .build_pointer_cast(buf, ptr_ty, "fst.buf_ptr")
                    .unwrap();
                let is_float = matches!(val, BasicValueEnum::FloatValue(_));
                // Widen narrower ints to i64 before snprintf's varargs slot —
                // sext for signed, zext for unsigned. Mirrors `compile_print`
                // (control_flow.rs ~258-285): without this, a negative i32 in
                // an f-string renders as its unsigned reinterpretation
                // (`-123` → `4294967173`) because printf reads 64 bits and
                // the high bits are LLVM's zero pad.
                let is_unsigned_int = !is_float && self.expr_is_unsigned_int(source_expr);
                let arg_val: BasicValueEnum<'ctx> = if let BasicValueEnum::IntValue(iv) = val {
                    let bits = iv.get_type().get_bit_width();
                    if bits < 64 {
                        let widened = if is_unsigned_int {
                            self.builder
                                .build_int_z_extend(iv, i64_t, "fst.zext")
                                .unwrap()
                        } else {
                            self.builder
                                .build_int_s_extend(iv, i64_t, "fst.sext")
                                .unwrap()
                        };
                        widened.into()
                    } else {
                        val
                    }
                } else {
                    val
                };
                let fmt_str = if is_float {
                    self.builder
                        .build_global_string_ptr("%g", "fst.fmt_f")
                        .unwrap()
                        .as_pointer_value()
                } else if is_unsigned_int {
                    self.builder
                        .build_global_string_ptr("%llu", "fst.fmt_u")
                        .unwrap()
                        .as_pointer_value()
                } else {
                    self.builder
                        .build_global_string_ptr("%lld", "fst.fmt_i")
                        .unwrap()
                        .as_pointer_value()
                };
                let written = self
                    .builder
                    .build_call(
                        self.snprintf_fn,
                        &[
                            buf_ptr.into(),
                            buf_size.into(),
                            fmt_str.into(),
                            arg_val.into(),
                        ],
                        "fst.written",
                    )
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_int_value();
                let len = self
                    .builder
                    .build_int_z_extend(written, i64_t, "fst.len")
                    .unwrap();
                (buf_ptr, len)
            }
        }
    }

    /// Encode an i32 codepoint as 1–4 UTF-8 bytes in a 4-byte stack alloca;
    /// return `(buf_ptr, byte_len_i64)`. Used by the print and f-string
    /// char-arms to render a `char` as the glyph rather than the integer
    /// codepoint. Delegates the encoding logic to the runtime helper
    /// `karac_string_encode_char` to keep the lowered IR small (one call
    /// per print, vs. the ~30-instruction inline branch ladder).
    pub(super) fn emit_codepoint_to_utf8(
        &self,
        cp: inkwell::values::IntValue<'ctx>,
    ) -> (PointerValue<'ctx>, inkwell::values::IntValue<'ctx>) {
        let fn_val = self.current_fn.unwrap();
        let i8_t = self.context.i8_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());

        let buf = self.create_entry_alloca(fn_val, "u8.buf", i8_t.array_type(4).into());
        let buf_ptr = self
            .builder
            .build_pointer_cast(buf, ptr_ty, "u8.buf.ptr")
            .unwrap();
        let len = self
            .builder
            .build_call(
                self.karac_string_encode_char_fn,
                &[cp.into(), buf_ptr.into()],
                "u8.enc",
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();
        (buf_ptr, len)
    }
}
