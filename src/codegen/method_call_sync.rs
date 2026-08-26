//! Synchronization lowerings: `Atomic[T]` methods, mutex storage,
//! `lock {}` blocks, critical sections, and the raw-pointer module calls.
//!
//! Extracted verbatim from `method_call.rs` (structural-debt second-level
//! split). Sibling `impl<'ctx> super::Codegen<'ctx>` block; moved methods
//! are `pub(super)`.

use crate::ast::*;

use inkwell::types::BasicTypeEnum;
use inkwell::values::{BasicValue, BasicValueEnum};
use inkwell::AddressSpace;
use inkwell::AtomicOrdering;
use inkwell::AtomicRMWBinOp;
use inkwell::IntPredicate;

/// Natural alignment (bytes) for an Atomic primitive lowering. LLVM's
/// `load atomic` / `store atomic` require alignment ≥ the type's size
/// in bytes; the v1 Atomic codegen surface admits power-of-two-byte
/// integer widths (i8/i16/i32/i64/usize/i128) per the gate in
/// `compile_atomic_method`. Narrower / non-power-of-two widths (e.g.
/// `i1` from `Atomic[bool]`) are rejected at the dispatch site with a
/// clear diagnostic; the rounding-up branch here is defensive only.
fn atomic_alignment_for(ty: BasicTypeEnum<'_>) -> u32 {
    match ty {
        BasicTypeEnum::IntType(it) => {
            let bits = it.get_bit_width();
            bits.div_ceil(8).max(1)
        }
        _ => 8,
    }
}

impl<'ctx> super::Codegen<'ctx> {
    /// True iff `object` is a receiver shape whose static type is
    /// `Atomic[T]` — either an Identifier `a` (var_type_names registers
    /// "Atomic" via the let-stmt RHS recognizer in `compile_stmt`) or a
    /// FieldAccess `c.field` where `c`'s struct registers `field`'s
    /// declared type as `Atomic` in `struct_field_type_names`.
    /// Companion gate to `compile_atomic_method`.
    pub(super) fn is_atomic_receiver(&self, object: &Expr) -> bool {
        match &object.kind {
            ExprKind::Identifier(name) => {
                matches!(self.var_types.var_type_names.get(name.as_str()), Some(n) if n == "Atomic")
            }
            ExprKind::FieldAccess { object, field } => {
                if let Some(obj_ty) = self.type_name_of_expr(object) {
                    if let Some(field_names) =
                        self.type_decls.struct_field_names.get(obj_ty.as_str())
                    {
                        if let Some(idx) = field_names.iter().position(|n| n == field) {
                            if let Some(field_ty_names) =
                                self.type_decls.struct_field_type_names.get(obj_ty.as_str())
                            {
                                return field_ty_names.get(idx).and_then(|n| n.as_deref())
                                    == Some("Atomic");
                            }
                        }
                    }
                }
                false
            }
            _ => false,
        }
    }

    /// Codegen for `Atomic[T].load(MemoryOrdering.X)` and
    /// `Atomic[T].store(value, MemoryOrdering.X)`. Resolves the
    /// receiver's storage pointer + element LLVM type, parses the
    /// trailing `MemoryOrdering.X` qualified-variant arg into an
    /// `inkwell::AtomicOrdering`, and emits `load atomic` / `store
    /// atomic` against the slot. Supports both Identifier receivers
    /// (`a.load(...)` where `a` is a top-level Atomic[T] binding) and
    /// FieldAccess receivers (`c.field.load(...)` where `c.field` is
    /// an Atomic-typed struct field — the shape the `karac migrate
    /// --atomic` consumer-rewrite emits). The receiver gate runs in
    /// `is_atomic_receiver` upstream.
    pub(super) fn compile_atomic_method(
        &mut self,
        object: &Expr,
        method: &str,
        args: &[CallArg],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let (storage_ptr, elem_ty, inner_is_bool) = self.resolve_atomic_storage(object)?;
        // LLVM requires atomic load/store on a power-of-two-byte
        // integer (i8/i16/i32/i64/i128 plus pointer/float of those
        // widths). Reject narrower / odd-width integers explicitly so
        // the user sees a clear codegen diagnostic rather than an
        // opaque LLVM verifier failure. `Atomic[bool]` is supported
        // via i8 slot-widening (`is_bool_type_expr` arm in
        // `llvm_type_for_type_expr` returns i8, not i1; the load/store
        // arms below trunc/zext at the i1↔i8 boundary).
        if let BasicTypeEnum::IntType(it) = elem_ty {
            let bw = it.get_bit_width();
            if bw < 8 || !bw.is_power_of_two() {
                return Err(format!(
                    "codegen: Atomic[T] requires T to be a power-of-two-byte integer \
                     (i8/i16/i32/i64/i128/usize) or `bool` (widened to i8); \
                     received {}-bit integer.",
                    bw
                ));
            }
        }
        match method {
            "load" => {
                if args.len() != 1 {
                    return Err(format!(
                        "codegen: Atomic.load takes 1 MemoryOrdering argument, got {}",
                        args.len()
                    ));
                }
                let ordering = self.parse_memory_ordering(&args[0].value)?;
                if matches!(
                    ordering,
                    AtomicOrdering::Release | AtomicOrdering::AcquireRelease
                ) {
                    return Err(format!(
                        "codegen: Atomic.load rejects MemoryOrdering.{:?} (LLVM forbids \
                         Release / AcqRel on a load); use Relaxed / Acquire / SeqCst",
                        ordering
                    ));
                }
                let loaded = self
                    .builder
                    .build_load(elem_ty, storage_ptr, "atomic.load")
                    .unwrap();
                let inst = loaded
                    .as_instruction_value()
                    .expect("build_load produces an instruction with an instruction value");
                let align = atomic_alignment_for(elem_ty);
                inst.set_alignment(align).map_err(|e| {
                    format!("codegen: set_alignment failed on atomic load: {:?}", e)
                })?;
                inst.set_atomic_ordering(ordering).map_err(|e| {
                    format!(
                        "codegen: set_atomic_ordering failed on atomic load: {:?}",
                        e
                    )
                })?;
                // Atomic[bool]: the slot is i8 (widened); the surface
                // type the user sees is `bool` (i1). Trunc back to i1
                // so downstream comparison / branch ops see the
                // expected bit width.
                if inner_is_bool {
                    let i8v = loaded.into_int_value();
                    let i1 = self
                        .builder
                        .build_int_truncate(i8v, self.context.bool_type(), "atomic.bool.trunc")
                        .unwrap();
                    return Ok(i1.into());
                }
                Ok(loaded)
            }
            "store" => {
                if args.len() != 2 {
                    return Err(format!(
                        "codegen: Atomic.store takes (value, MemoryOrdering), got {} args",
                        args.len()
                    ));
                }
                let value = self.compile_expr(&args[0].value)?;
                let ordering = self.parse_memory_ordering(&args[1].value)?;
                if matches!(
                    ordering,
                    AtomicOrdering::Acquire | AtomicOrdering::AcquireRelease
                ) {
                    return Err(format!(
                        "codegen: Atomic.store rejects MemoryOrdering.{:?} (LLVM forbids \
                         Acquire / AcqRel on a store); use Relaxed / Release / SeqCst",
                        ordering
                    ));
                }
                // Atomic[bool]: the value coming in is i1, but the slot
                // is i8. Zext at the boundary so the store's value
                // width matches the slot's. The matched trunc on load
                // restores the i1 view above.
                let value = if inner_is_bool {
                    if let BasicValueEnum::IntValue(iv) = value {
                        if iv.get_type().get_bit_width() == 1 {
                            self.builder
                                .build_int_z_extend(iv, self.context.i8_type(), "atomic.bool.zext")
                                .unwrap()
                                .into()
                        } else {
                            value
                        }
                    } else {
                        value
                    }
                } else {
                    value
                };
                let store_inst = self.builder.build_store(storage_ptr, value).unwrap();
                let align = atomic_alignment_for(elem_ty);
                store_inst.set_alignment(align).map_err(|e| {
                    format!("codegen: set_alignment failed on atomic store: {:?}", e)
                })?;
                store_inst.set_atomic_ordering(ordering).map_err(|e| {
                    format!(
                        "codegen: set_atomic_ordering failed on atomic store: {:?}",
                        e
                    )
                })?;
                // Stores return unit — fill the expression slot with the
                // i64-0 placeholder used elsewhere for void returns.
                Ok(self.context.i64_type().const_int(0, false).into())
            }
            // Single-operand read-modify-write ops — all lower to one LLVM
            // `atomicrmw` and return the PREVIOUS value (matching Rust's
            // `Atomic::fetch_*` / `swap`), so e.g. `count.fetch_add(1, ..)` is
            // a race-free increment yielding the pre-increment count. `atomicrmw`
            // accepts any memory ordering (unlike load/store), so no ordering
            // rejection. The arithmetic / bitwise ops are integer-only
            // (`Atomic[bool]` has no arithmetic/bitwise RMW); `swap` (Xchg) is a
            // plain exchange and is the one RMW that also works on `Atomic[bool]`
            // (i8 slot — incoming i1 widened, returned old i8 truncated, same as
            // load/store). `compare_exchange` is a separate slice (two operands,
            // `cmpxchg`, Result-shaped return).
            "fetch_add" | "fetch_sub" | "fetch_and" | "fetch_or" | "fetch_xor" | "swap" => {
                if args.len() != 2 {
                    return Err(format!(
                        "codegen: Atomic.{} takes (value, MemoryOrdering), got {} args",
                        method,
                        args.len()
                    ));
                }
                let is_swap = method == "swap";
                if inner_is_bool && !is_swap {
                    return Err(format!(
                        "codegen: Atomic[bool] does not support {} (no arithmetic/bitwise RMW \
                         on a bool); only `swap` / `load` / `store`",
                        method
                    ));
                }
                let value = self.compile_expr(&args[0].value)?;
                let ordering = self.parse_memory_ordering(&args[1].value)?;
                // Atomic[bool] swap: the slot is i8 but the incoming value is
                // i1 — widen at the boundary (mirrors `store`).
                let value = if inner_is_bool {
                    if let BasicValueEnum::IntValue(iv) = value {
                        if iv.get_type().get_bit_width() == 1 {
                            self.builder
                                .build_int_z_extend(iv, self.context.i8_type(), "atomic.bool.zext")
                                .unwrap()
                                .into()
                        } else {
                            value
                        }
                    } else {
                        value
                    }
                } else {
                    value
                };
                let val_int = match value {
                    BasicValueEnum::IntValue(iv) => iv,
                    _ => {
                        return Err(format!(
                            "codegen: Atomic.{} requires an integer value argument",
                            method
                        ))
                    }
                };
                let op = match method {
                    "fetch_add" => AtomicRMWBinOp::Add,
                    "fetch_sub" => AtomicRMWBinOp::Sub,
                    "fetch_and" => AtomicRMWBinOp::And,
                    "fetch_or" => AtomicRMWBinOp::Or,
                    "fetch_xor" => AtomicRMWBinOp::Xor,
                    "swap" => AtomicRMWBinOp::Xchg,
                    _ => unreachable!("RMW arm gated on the method set above"),
                };
                let old = self
                    .builder
                    .build_atomicrmw(op, storage_ptr, val_int, ordering)
                    .map_err(|e| format!("codegen: build_atomicrmw failed: {:?}", e))?;
                // Atomic[bool] swap: returned old is i8 → trunc to i1 for the
                // surface `bool` view (mirrors `load`). `build_atomicrmw`
                // returns an `IntValue` directly.
                if inner_is_bool {
                    let i1 = self
                        .builder
                        .build_int_truncate(old, self.context.bool_type(), "atomic.bool.trunc")
                        .unwrap();
                    return Ok(i1.into());
                }
                Ok(old.into())
            }
            // `compare_exchange(old, new, success, failure) -> Result[T, T]`
            // (deferred.md § Atomic Operations). Lowers to LLVM `cmpxchg`, which
            // returns a `{ T, i1 }` struct: field 0 is the value loaded from the
            // slot, field 1 is the success flag. The Kāra surface returns
            // `Ok(prev)` on success / `Err(actual)` on failure — both payloads
            // are the loaded value, so the ONLY thing that varies is the tag.
            // Result's tags are `Ok = 1`, `Err = 0`, which is exactly
            // `zext(success_i1)` — so the Result aggregate is built directly with
            // no branch: tag = the success bit, payload word 0 = the loaded
            // value. Integer-only for v1 (`Atomic[bool]` rejected — its i8/i1
            // round-trip through the Result payload is a follow-on).
            "compare_exchange" => {
                if args.len() != 4 {
                    return Err(format!(
                        "codegen: Atomic.compare_exchange takes (old, new, success, failure), \
                         got {} args",
                        args.len()
                    ));
                }
                if inner_is_bool {
                    return Err(
                        "codegen: Atomic[bool].compare_exchange is not supported in v1 \
                         (use `swap` / `load` / `store` for bool flags); CAS on bool is a \
                         tracked follow-on"
                            .to_string(),
                    );
                }
                let expected = self.compile_expr(&args[0].value)?;
                let new_val = self.compile_expr(&args[1].value)?;
                let success_ord = self.parse_memory_ordering(&args[2].value)?;
                let failure_ord = self.parse_memory_ordering(&args[3].value)?;
                // LLVM forbids Release / AcqRel as the *failure* ordering (it is
                // the load-only path — no store happens on failure).
                if matches!(
                    failure_ord,
                    AtomicOrdering::Release | AtomicOrdering::AcquireRelease
                ) {
                    return Err(format!(
                        "codegen: Atomic.compare_exchange rejects MemoryOrdering.{:?} as the \
                         failure ordering (LLVM forbids Release / AcqRel on the no-store path); \
                         use Relaxed / Acquire / SeqCst",
                        failure_ord
                    ));
                }
                let (exp_int, new_int) = match (expected, new_val) {
                    (BasicValueEnum::IntValue(a), BasicValueEnum::IntValue(b)) => (a, b),
                    _ => {
                        return Err(
                            "codegen: Atomic.compare_exchange requires integer old/new values"
                                .to_string(),
                        )
                    }
                };
                let cmpxchg = self
                    .builder
                    .build_cmpxchg(storage_ptr, exp_int, new_int, success_ord, failure_ord)
                    .map_err(|e| format!("codegen: build_cmpxchg failed: {:?}", e))?;
                // `cmpxchg` yields `{ T, i1 }` — extract the loaded value + flag.
                let loaded = self
                    .builder
                    .build_extract_value(cmpxchg, 0, "cas.loaded")
                    .unwrap();
                let success = self
                    .builder
                    .build_extract_value(cmpxchg, 1, "cas.ok")
                    .unwrap()
                    .into_int_value();
                // Build the Result[T, T] aggregate: tag = the success bit
                // (Ok=1 / Err=0), payload word 0 = the loaded value.
                let i64_t = self.context.i64_type();
                let result_layout = self
                    .type_decls
                    .enum_layouts
                    .get("Result")
                    .ok_or_else(|| "codegen: Result enum layout not registered".to_string())?;
                let result_ty = result_layout.llvm_type;
                let payload_words = result_ty.count_fields().saturating_sub(1);
                let tag = self
                    .builder
                    .build_int_z_extend(success, i64_t, "cas.tag")
                    .unwrap();
                let loaded_word = self.coerce_to_i64(loaded)?;
                let mut agg = result_ty.get_undef();
                agg = self
                    .builder
                    .build_insert_value(agg, tag, 0, "cas.res.tag")
                    .unwrap()
                    .into_struct_value();
                agg = self
                    .builder
                    .build_insert_value(agg, loaded_word, 1, "cas.res.val")
                    .unwrap()
                    .into_struct_value();
                // Zero-fill the remaining payload words so the aggregate carries
                // no `undef` past the single value word (Result is sized for its
                // widest payload; a CAS value occupies only word 0).
                for w in 2..=payload_words {
                    agg = self
                        .builder
                        .build_insert_value(agg, i64_t.const_zero(), w, "cas.res.pad")
                        .unwrap()
                        .into_struct_value();
                }
                Ok(agg.into())
            }
            _ => unreachable!(
                "compile_atomic_method gated on method in {{load, store, fetch_add, fetch_sub, \
                 fetch_and, fetch_or, fetch_xor, swap, compare_exchange}}"
            ),
        }
    }

    /// Resolve a `lock` place expression to the `(Mutex struct type, pointer to
    /// the aggregate)` pair. Handles the two place shapes: an `Identifier` (a
    /// local / par-captured `Mutex` binding — its `VarSlot` IS the aggregate)
    /// and a `FieldAccess` on a `par` / `shared` struct (a `Mutex` field stored
    /// inline in the heap layout — GEP at `field_idx + 1`, reusing the
    /// shared-field deref the atomic-field path uses).
    pub(super) fn resolve_mutex_storage(
        &mut self,
        mutex: &Expr,
    ) -> Result<
        (
            inkwell::types::StructType<'ctx>,
            inkwell::values::PointerValue<'ctx>,
        ),
        String,
    > {
        match &mutex.kind {
            ExprKind::Identifier(name) => {
                let slot = self.variables.get(name).copied().ok_or_else(|| {
                    format!("codegen: lock target '{}' has no storage slot", name)
                })?;
                // A `ref`/`mut ref Mutex[T]` parameter: the alloca holds a
                // pointer TO the aggregate, and the pointee `{ lockflag, value }`
                // struct type is recorded in `ref_params`. Load through the ref.
                if let Some(&BasicTypeEnum::StructType(st)) = self.borrow_vars.ref_params.get(name)
                {
                    if st.count_fields() == 2 {
                        let agg_ptr = self
                            .builder
                            .build_load(slot.ty, slot.ptr, "mutex.ref.load")
                            .map_err(|e| format!("codegen: lock ref-param load failed: {:?}", e))?
                            .into_pointer_value();
                        return Ok((st, agg_ptr));
                    }
                }
                // A directly-bound (or par-captured) local: the slot IS the
                // aggregate.
                match slot.ty {
                    BasicTypeEnum::StructType(st) if st.count_fields() == 2 => Ok((st, slot.ptr)),
                    other => Err(format!(
                        "codegen: lock target '{}' is not a Mutex[T] (slot type {:?})",
                        name, other
                    )),
                }
            }
            ExprKind::FieldAccess {
                object: inner,
                field,
            } => {
                // `lock self.state` — `self.state` is a `Mutex` field stored
                // inline in the `par`/`shared` struct's heap aggregate
                // `{ i64 refcount, …, { i64 lockflag, T value }, … }`.
                let (type_name, info) = self.shared_type_for_expr(inner).ok_or_else(|| {
                    format!(
                        "codegen: lock field receiver '.{}' is not on a par/shared struct",
                        field
                    )
                })?;
                let idx = self
                    .type_decls
                    .struct_field_names
                    .get(&type_name)
                    .and_then(|names| names.iter().position(|n| n == field))
                    .ok_or_else(|| {
                        format!("codegen: struct '{}' has no field '{}'", type_name, field)
                    })?;
                let heap_ptr = self.compile_expr(inner)?.into_pointer_value();
                let field_ptr = self
                    .builder
                    .build_struct_gep(
                        info.heap_type,
                        heap_ptr,
                        (idx + 1) as u32, // +1: heap index 0 is the refcount
                        "mutex.field.ptr",
                    )
                    .map_err(|e| format!("codegen: lock field gep failed: {:?}", e))?;
                match info.heap_type.get_field_type_at_index((idx + 1) as u32) {
                    Some(BasicTypeEnum::StructType(st)) if st.count_fields() == 2 => {
                        Ok((st, field_ptr))
                    }
                    other => Err(format!(
                        "codegen: lock field '{}.{}' is not a Mutex[T] (field type {:?})",
                        type_name, field, other
                    )),
                }
            }
            other => Err(format!(
                "codegen: unsupported lock place expression {:?}",
                std::mem::discriminant(other)
            )),
        }
    }

    /// Codegen for `lock <place> [alias] { body }` (design.md § Part 5: Shared
    /// Types, `lock` blocks). `place` names a `Mutex[T]` laid out as
    /// `{ i64 lockflag, T value }` (a local binding or a `par`/`shared` struct
    /// field). Emits a TAS spinlock: acquire by `atomicrmw xchg`-ing the flag to
    /// 1 and spinning until the previous value was 0; expose the value field as a
    /// `mut ref T` binding (the `alias`, or the mutex name itself shadowed for an
    /// `Identifier` place) for the body; release by atomically storing 0.
    /// Straight-line only — the typechecker rejects early exits from the body,
    /// so the single fall-through release is sound.
    pub(super) fn compile_lock_block(
        &mut self,
        mutex: &Expr,
        alias: Option<&str>,
        body: &Block,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let (mutex_struct, base_ptr) = self.resolve_mutex_storage(mutex)?;
        let flag_ptr = self
            .builder
            .build_struct_gep(mutex_struct, base_ptr, 0, "mutex.flag.ptr")
            .map_err(|e| format!("codegen: lock flag gep failed: {:?}", e))?;
        let value_ptr = self
            .builder
            .build_struct_gep(mutex_struct, base_ptr, 1, "mutex.val.ptr")
            .map_err(|e| format!("codegen: lock value gep failed: {:?}", e))?;
        let value_ty = mutex_struct.get_field_type_at_index(1).unwrap();

        let i64_t = self.context.i64_type();
        let current_fn = self.current_fn.unwrap();
        let contended_bb = self
            .context
            .append_basic_block(current_fn, "lock.contended");
        let held_bb = self.context.append_basic_block(current_fn, "lock.held");
        let after_bb = self.context.append_basic_block(current_fn, "lock.after");

        // Acquire — futex 3-state fast path (0 = free, 1 = locked-uncontended,
        // 2 = locked-contended). `cmpxchg(0 -> 1)`: on success we hold the lock
        // with NO runtime call — the uncontended path stays fully inline, at
        // roughly the old spinlock's cost, so this is a pure no-regression win
        // for the common case. On failure (someone else holds it) branch to the
        // contended path, which blocks in the runtime parking lot (marking the
        // flag `2`) instead of burning CPU spinning. Release lives in
        // `CleanupAction::ReleaseMutex` (`runtime.rs`): `xchg(-> 0)` + wake iff
        // the prior state was `2`.
        let cas = self
            .builder
            .build_cmpxchg(
                flag_ptr,
                i64_t.const_zero(),
                i64_t.const_int(1, false),
                AtomicOrdering::SequentiallyConsistent,
                AtomicOrdering::SequentiallyConsistent,
            )
            .map_err(|e| format!("codegen: lock acquire cmpxchg failed: {:?}", e))?;
        let acquired = self
            .builder
            .build_extract_value(cas, 1, "lock.acquired")
            .unwrap()
            .into_int_value();
        self.builder
            .build_conditional_branch(acquired, held_bb, contended_bb)
            .unwrap();

        // Contended — block in the runtime until we hold the lock. The fast
        // cmpxchg already failed; `karac_runtime_mutex_lock` re-tries under a
        // bucketed condvar (Drepper's protocol) and returns holding the lock.
        self.builder.position_at_end(contended_bb);
        let lock_fn = self
            .module
            .get_function("karac_runtime_mutex_lock")
            .expect("karac_runtime_mutex_lock declared in Codegen::new");
        self.builder
            .build_call(lock_fn, &[flag_ptr.into()], "lock.wait")
            .unwrap();
        self.builder.build_unconditional_branch(held_bb).unwrap();

        // Critical section.
        self.builder.position_at_end(held_bb);
        // Bind the body's inner-value name (the alias, or — for an `Identifier`
        // place — the mutex name shadowed) to the value slot: a `mut ref T`
        // whose storage IS the mutex's value field, so the body's reads /
        // writes / field accesses operate in place under the lock. A field
        // place without an alias is rejected by the typechecker.
        let bind_name = match (alias, &mutex.kind) {
            (Some(a), _) => Some(a.to_string()),
            (None, ExprKind::Identifier(n)) => Some(n.clone()),
            (None, _) => None,
        };
        let saved = bind_name
            .as_ref()
            .and_then(|n| self.variables.get(n).copied());
        if let Some(ref name) = bind_name {
            self.variables.insert(
                name.clone(),
                super::VarSlot {
                    ptr: value_ptr,
                    ty: value_ty,
                },
            );
        }
        // Seed a cleanup frame whose bottom action is the lock release, so the
        // release rides the normal scope-cleanup machinery and fires on EVERY
        // exit path — not just the straight-line fall-through. The body's own
        // scope cleanups (Vec frees, RC-decs, drops, user `defer`s) stack ABOVE
        // the release on this frame, so a drain runs them first and releases
        // last (reverse-construction RAII: drop body resources under the lock,
        // then unlock). `flag_ptr` was GEP'd in the lock's entry block, so it
        // dominates every body BB and the re-emitted store at a break/continue/
        // return site is well-formed. This is what retires the `LockEarlyExit`
        // (`E0259`) typechecker rejection — early exits from a lock body are now
        // legal and release the lock on the way out.
        self.drop_rc
            .scope_cleanup_actions
            .push(vec![super::state::CleanupAction::ReleaseMutex { flag_ptr }]);

        let body_val = self.compile_block(body)?;
        // Restore the shadowed binding (mutex name) / drop the alias. This is
        // compile-time `self.variables` bookkeeping and is correct on the
        // early-exit path too (the IR has already branched away; only the
        // symbol table is restored for the code that follows the lock).
        if let Some(ref name) = bind_name {
            match saved {
                Some(s) => {
                    self.variables.insert(name.clone(), s);
                }
                None => {
                    self.variables.remove(name);
                }
            }
        }

        // Drain the release frame. On straight-line fall-through the body block
        // has no terminator, so emit the body cleanups + release here and branch
        // to `after_bb`. On an early exit the body block is already terminated
        // (break/continue ran `emit_scope_cleanup_from`, return ran
        // `emit_scope_cleanup` — both walked this frame and emitted the release
        // before branching), so just pop the now-drained frame. `after_bb` is
        // then dead-but-filled by trailing code / the function epilogue, exactly
        // as `compile_loop`'s exit block is for a no-break loop.
        let body_terminated = self
            .builder
            .get_insert_block()
            .unwrap()
            .get_terminator()
            .is_some();
        if !body_terminated {
            self.drain_top_frame_with_emit();
            self.builder.build_unconditional_branch(after_bb).unwrap();
        } else {
            self.drop_rc.scope_cleanup_actions.pop();
        }
        self.builder.position_at_end(after_bb);

        Ok(body_val.unwrap_or_else(|| i64_t.const_int(0, false).into()))
    }

    /// Recover the (storage pointer, element LLVM type) pair for an
    /// `Atomic[T]` receiver. Identifier path reads from `variables`;
    /// FieldAccess path GEPs to the struct field. Element type is the
    /// LLVM type of the inner primitive (Atomic[T] is laid out
    /// transparently as T — see `llvm_type_for_type_expr`'s Atomic
    /// arm).
    pub(super) fn resolve_atomic_storage(
        &mut self,
        object: &Expr,
    ) -> Result<
        (
            inkwell::values::PointerValue<'ctx>,
            BasicTypeEnum<'ctx>,
            bool,
        ),
        String,
    > {
        match &object.kind {
            ExprKind::Identifier(name) => {
                let Some(slot) = self.variables.get(name.as_str()).copied() else {
                    // MODULE-LEVEL `let COUNTER: Atomic[i64] = Atomic.new(0)` —
                    // the storage is an LLVM global, not a local alloca, and
                    // this lookup only ever consulted `variables`. So the
                    // module-scope Atomic that the `module_mut_binding` warning
                    // recommends as the FIRST alternative to `let mut` checked
                    // and interpreted fine, then failed both compiled backends
                    // with "has no slot" (B-2026-08-26-17). The global IS the
                    // storage — same posture `get_data_ptr` documents for the
                    // Vec/Map/Set method paths — so it substitutes for the
                    // alloca directly.
                    if let Some(info) = self.mod_bindings.module_bindings.get(name.as_str()) {
                        let is_bool = self.atomic_var_inner_is_bool.contains(name.as_str());
                        return Ok((info.global.as_pointer_value(), info.llvm_ty, is_bool));
                    }
                    return Err(format!("codegen: Atomic receiver '{}' has no slot", name));
                };
                let is_bool = self.atomic_var_inner_is_bool.contains(name.as_str());
                // A `ref Atomic[T]` / `mut ref Atomic[T]` PARAMETER: its alloca
                // holds a POINTER to the caller's atomic storage (recorded in
                // `ref_params`, "alloca holds a pointer-to-data" — functions.rs),
                // so the atomic op must operate through the LOADED pointer, not
                // the alloca itself (which holds the address, not the value).
                // Without the deref, `fn bump(c: ref Atomic[i64]) {
                // c.fetch_add(1, ..) }` RMW'd the pointer-holding slot and the
                // caller's cell never changed (interp counted, build stayed 0) —
                // a run-vs-build divergence with no `par` involved
                // (B-2026-07-18-30). `ref_params[name]` is the atomic's VALUE
                // type (the peeled inner), which is the correct `elem_ty`.
                if let Some(&inner_ty) = self.borrow_vars.ref_params.get(name.as_str()) {
                    let ptr_ty = self.context.ptr_type(AddressSpace::default());
                    let storage_ptr = self
                        .builder
                        .build_load(ptr_ty, slot.ptr, "atomic.ref.deref")
                        .unwrap()
                        .into_pointer_value();
                    return Ok((storage_ptr, inner_ty, is_bool));
                }
                Ok((slot.ptr, slot.ty, is_bool))
            }
            ExprKind::FieldAccess {
                object: inner,
                field,
            } => {
                // `shared`/`par` struct field receiver — e.g. `self.count.load(..)`
                // on a `par struct Counter { count: Atomic[i64] }`. These live in
                // `shared_types` (heap layout `{ i64 refcount, fields... }`), NOT
                // `struct_types`, so the plain path below would error with "no LLVM
                // type". Reuse the proven shared field-read deref: `compile_expr(inner)`
                // yields the heap pointer (handling the `ref self` ptr-to-heap-ptr
                // load), then GEP at `idx + 1` (index 0 is the refcount) into the
                // heap type. The field slot IS the transparent `Atomic[T]` = `T`
                // storage the atomic load/store operates on. Mirrors the shared
                // field-read path in `expr_ops.rs::compile_field_access`.
                if let Some((type_name, info)) = self.shared_type_for_expr(inner) {
                    if !info.is_enum {
                        if let Some(idx) = self
                            .type_decls
                            .struct_field_names
                            .get(&type_name)
                            .and_then(|names| names.iter().position(|n| n == field))
                        {
                            let heap_ptr = self.compile_expr(inner)?.into_pointer_value();
                            let field_ptr = self
                                .builder
                                .build_struct_gep(
                                    info.heap_type,
                                    heap_ptr,
                                    (idx + 1) as u32,
                                    "atomic.sh_field.ptr",
                                )
                                .map_err(|e| format!("codegen: struct_gep failed: {:?}", e))?;
                            let elem_ty = info
                                .heap_type
                                .get_field_type_at_index((idx + 1) as u32)
                                .ok_or_else(|| {
                                    format!(
                                        "codegen: shared/par struct '{}' field {} out of range",
                                        type_name, idx
                                    )
                                })?;
                            let inner_is_bool = self
                                .type_decls
                                .struct_field_type_exprs
                                .get(&type_name)
                                .and_then(|fields| fields.get(idx))
                                .map(super::types_lowering::is_atomic_bool_type_expr)
                                .unwrap_or(false);
                            return Ok((field_ptr, elem_ty, inner_is_bool));
                        }
                    }
                }
                let obj_ty_name = self.type_name_of_expr(inner).ok_or_else(|| {
                    format!(
                        "codegen: Atomic field receiver '.{}' has unknown object type",
                        field
                    )
                })?;
                let field_names = self
                    .type_decls
                    .struct_field_names
                    .get(obj_ty_name.as_str())
                    .cloned()
                    .ok_or_else(|| {
                        format!("codegen: struct '{}' has no registered fields", obj_ty_name)
                    })?;
                let idx = field_names.iter().position(|n| n == field).ok_or_else(|| {
                    format!("codegen: struct '{}' has no field '{}'", obj_ty_name, field)
                })? as u32;
                let struct_ty = *self
                    .type_decls
                    .struct_types
                    .get(obj_ty_name.as_str())
                    .ok_or_else(|| {
                        format!(
                            "codegen: struct '{}' has no LLVM type (shared structs not \
                             supported as Atomic field receivers)",
                            obj_ty_name
                        )
                    })?;
                let inner_name = if let ExprKind::Identifier(n) = &inner.kind {
                    n.clone()
                } else {
                    return Err(format!(
                        "codegen: Atomic FieldAccess receiver must be `<identifier>.{}` \
                         in v1 (got nested receiver)",
                        field
                    ));
                };
                let base_ptr = self.get_data_ptr(&inner_name).ok_or_else(|| {
                    format!(
                        "codegen: Atomic field receiver base '{}' has no storage ptr",
                        inner_name
                    )
                })?;
                let field_ptr = self
                    .builder
                    .build_struct_gep(struct_ty, base_ptr, idx, "atomic.field.ptr")
                    .map_err(|e| format!("codegen: struct_gep failed: {:?}", e))?;
                let elem_ty = struct_ty.get_field_type_at_index(idx).ok_or_else(|| {
                    format!(
                        "codegen: struct '{}' field {} index out of range",
                        obj_ty_name, idx
                    )
                })?;
                // Inner-is-bool detection for struct fields reads the
                // full per-field TypeExpr registered at struct
                // declaration time. Fields ALWAYS carry their
                // annotation (declaration syntax requires it), so this
                // path is exact — no missing-info fallback needed.
                let inner_is_bool = self
                    .type_decls
                    .struct_field_type_exprs
                    .get(obj_ty_name.as_str())
                    .and_then(|fields| fields.get(idx as usize))
                    .map(super::types_lowering::is_atomic_bool_type_expr)
                    .unwrap_or(false);
                Ok((field_ptr, elem_ty, inner_is_bool))
            }
            _ => Err(format!(
                "codegen: Atomic method receiver shape {:?} not supported in v1",
                std::mem::discriminant(&object.kind)
            )),
        }
    }

    /// Parse the canonical `MemoryOrdering.X` qualified-variant
    /// expression into an `inkwell::AtomicOrdering`. Mirrors the
    /// interpreter's `MemoryOrdering` qualified-variant recognizer at
    /// `src/interpreter/eval_call.rs:474+`. The Kāra surface spelling
    /// for `Relaxed` maps to LLVM's `Monotonic`; all others map by
    /// name.
    pub(super) fn parse_memory_ordering(&self, expr: &Expr) -> Result<AtomicOrdering, String> {
        if let ExprKind::Path { segments, .. } = &expr.kind {
            if segments.len() == 2 && segments[0] == "MemoryOrdering" {
                return match segments[1].as_str() {
                    "Relaxed" => Ok(AtomicOrdering::Monotonic),
                    "Acquire" => Ok(AtomicOrdering::Acquire),
                    "Release" => Ok(AtomicOrdering::Release),
                    "AcqRel" => Ok(AtomicOrdering::AcquireRelease),
                    "SeqCst" => Ok(AtomicOrdering::SequentiallyConsistent),
                    other => Err(format!(
                        "codegen: unknown MemoryOrdering variant '{}'",
                        other
                    )),
                };
            }
        }
        Err(
            "codegen: Atomic.load / .store ordering arg must be a MemoryOrdering.X variant literal"
                .to_string(),
        )
    }

    /// Resolve a place expression to its in-memory address for
    /// `ptr.const(place)` / `ptr.mut(place)`. Mirrors the typechecker's
    /// structural place-validator (`is_place_expression` in
    /// `expr_method_call.rs`, which accepts a binding / `self` / field
    /// access / tuple index / index / dereference chain). Unlike the
    /// match-suppression `field_chain_place_ptr`, the root binding is
    /// resolved through `get_data_ptr`, so a chain rooted at a `ref` /
    /// `mut ref` parameter or an RC-promoted binding yields the correct
    /// pointee address — not the address of the slot that *holds* the
    /// pointer. Returns `None` for a shape it can't resolve (a
    /// call-rooted base, an unknown struct type, a non-simple Vec index),
    /// so the `ptr.const` / `ptr.mut` dispatch falls through to the
    /// status-quo diagnostic rather than emit a wrong address.
    pub(super) fn ptr_place_addr(
        &mut self,
        place: &Expr,
    ) -> Option<inkwell::values::PointerValue<'ctx>> {
        match &place.kind {
            ExprKind::Identifier(name) => self.get_data_ptr(name),
            ExprKind::SelfValue => self.get_data_ptr("self"),
            ExprKind::FieldAccess { object, field } => {
                let base_ptr = self.ptr_place_addr(object)?;
                let obj_ty = self.place_chain_type_name(object)?;
                let st = *self.type_decls.struct_types.get(obj_ty.as_str())?;
                let idx = self
                    .type_decls
                    .struct_field_names
                    .get(obj_ty.as_str())?
                    .iter()
                    .position(|n| n == field)? as u32;
                self.builder
                    .build_struct_gep(st, base_ptr, idx, "ptr.place.field")
                    .ok()
            }
            ExprKind::TupleIndex { object, index } => {
                let base_ptr = self.ptr_place_addr(object)?;
                let tuple_ty = self.place_chain_aggregate_llvm_type(object)?;
                self.builder
                    .build_struct_gep(tuple_ty, base_ptr, *index as u32, "ptr.place.tupidx")
                    .ok()
            }
            ExprKind::Index { object, index } => {
                let ExprKind::Identifier(vec_var) = &object.kind else {
                    return None;
                };
                // Restricted to a plain (non-array-slot) Vec variable indexed
                // by a side-effect-free index — `vec_index_elem_ptr` re-evaluates
                // the index to recompute the element pointer, and a pure index
                // makes that re-eval a no-op. Mirrors `field_chain_place_ptr`.
                if !self.var_types.vec_elem_types.contains_key(vec_var.as_str())
                    || !matches!(index.kind, ExprKind::Identifier(_) | ExprKind::Integer(..))
                {
                    return None;
                }
                let slot_is_array = self
                    .variables
                    .get(vec_var.as_str())
                    .is_some_and(|s| matches!(s.ty, BasicTypeEnum::ArrayType(_)));
                if slot_is_array {
                    return None;
                }
                let vec_var = vec_var.clone();
                self.vec_index_elem_ptr(&vec_var, index).ok()
            }
            ExprKind::Unary {
                op: UnaryOp::Deref,
                operand,
            } => {
                // The operand's *value* is the address; reseat through
                // `inttoptr` if it still flows as an integer.
                let v = self.compile_expr(operand).ok()?;
                match v {
                    BasicValueEnum::PointerValue(pv) => Some(pv),
                    BasicValueEnum::IntValue(iv) => self
                        .builder
                        .build_int_to_ptr(
                            iv,
                            self.context.ptr_type(AddressSpace::default()),
                            "ptr.place.deref",
                        )
                        .ok(),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// Slice 3 of the strict-provenance work (line 511). Lower one of
    /// the seven `ptr.*` module functions to its LLVM cast counterpart.
    /// Returns `Ok(None)` for an unknown method so the caller's
    /// fall-through diagnostic stays in place; the typechecker has
    /// already accepted only the seven valid names so reaching `None`
    /// here means a real codegen bug rather than a user error.
    ///
    /// **ABI note.** The current codegen lowers `*const T` / `*mut T`
    /// to LLVM `i64` at function-signature and binding-slot boundaries
    /// (see `llvm_type_for_type_expr` — raw pointer kinds fall through
    /// to the `i64` default). Under that ABI all four ptr↔int casts in
    /// the strict-provenance API are *identity at the LLVM level*: the
    /// address bits already round-trip losslessly through the i64 slot
    /// that holds the raw pointer. The pragmatic lowering here mirrors
    /// that — emit a no-op (when both sides are already i64) or a
    /// `ptrtoint` (when the receiver happens to flow as an LLVM
    /// pointer-typed SSA, which can happen for some intermediate
    /// values). The provenance-preserving lowering the spec describes
    /// (`ptrtoint`+`!provenance.preserve` markers; `inttoptr` with
    /// `noalias` invalidation for the `expose` family) requires
    /// raw-pointer-typed LLVM slots end-to-end — that uplift is
    /// tracked as a follow-up. Tests in `tests/codegen.rs` pin the
    /// runtime round-trip; the IR-shape pins live alongside.
    /// Lower `critical_section.acquire()` to a call to
    /// `karac_critical_section_acquire() -> i64` (declared in `Codegen::new`)
    /// and return the i64 restore token as the guard value. `CriticalSectionGuard`
    /// is a single-`i64`-field stdlib struct represented as its bare word, so
    /// the token IS the guard: the let-binding stores it in an i64 slot that
    /// the scope-exit `@CriticalSectionGuard.drop` (`emit_critical_section_drop_body`)
    /// GEPs back as `{i64}` field 0 to hand to `release`. RAII drop fires
    /// because the typechecker labels the binding `CriticalSectionGuard`
    /// (`pattern_binding_types` → `var_type_names`) and `drop_method_keys`
    /// carries the type — no per-site drop wiring needed here.
    pub(super) fn compile_critical_section_acquire(
        &mut self,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let acquire_fn = self
            .module
            .get_function("karac_critical_section_acquire")
            .expect("karac_critical_section_acquire declared in Codegen::new");
        let token = self
            .builder
            .build_call(acquire_fn, &[], "critical_section.acquire")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic();
        Ok(token)
    }

    pub(super) fn compile_ptr_module_call(
        &mut self,
        method: &str,
        args: &[CallArg],
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        let i64_ty = self.context.i64_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        // Raw pointers lower to genuine LLVM `ptr` since the CStr/as_ptr
        // slice lifted `TypeKind::Pointer` off the historical i64
        // fall-through (see `llvm_type_for_type_expr`) — the "deferred
        // refinement" the original i64-ABI lowering here anticipated.
        // ptr→usize ops emit `ptrtoint`, usize→ptr ops emit `inttoptr`,
        // exactly the spec's provenance story (design.md § Pointer
        // Provenance; the `!provenance` metadata refinement remains
        // open). The two coercion helpers absorb either value shape so
        // intermediate results that still flow as integers (e.g. a
        // usize-typed local) compose with pointer-typed params.
        let to_i64 =
            |this: &mut Self, v: BasicValueEnum<'ctx>, label: &str| -> BasicValueEnum<'ctx> {
                match v {
                    BasicValueEnum::PointerValue(pv) => this
                        .builder
                        .build_ptr_to_int(pv, i64_ty, label)
                        .unwrap()
                        .into(),
                    BasicValueEnum::IntValue(_) => v,
                    _ => v,
                }
            };
        let to_ptr =
            |this: &mut Self, v: BasicValueEnum<'ctx>, label: &str| -> BasicValueEnum<'ctx> {
                match v {
                    BasicValueEnum::IntValue(iv) => this
                        .builder
                        .build_int_to_ptr(iv, ptr_ty, label)
                        .unwrap()
                        .into(),
                    BasicValueEnum::PointerValue(_) => v,
                    _ => v,
                }
            };
        match method {
            // p: *_ T -> usize  (ptr.addr / ptr.expose / ptr.expose_mut)
            "addr" | "expose" | "expose_mut" if args.len() == 1 => {
                let p = self.compile_expr(&args[0].value)?;
                let label = match method {
                    "addr" => "ptr.addr",
                    "expose" => "ptr.expose",
                    _ => "ptr.expose_mut",
                };
                Ok(Some(to_i64(self, p, label)))
            }
            // (p: *_ T, addr: usize) -> *_ T  (ptr.with_addr / ptr.with_addr_mut)
            //
            // Compile the first arg for side effects only — a
            // provenance-aware lowering would consult `p`'s
            // `!provenance` metadata to reseat the address bits; until
            // that metadata lands, the result is just `addr` reseated
            // into a pointer via `inttoptr`.
            "with_addr" | "with_addr_mut" if args.len() == 2 => {
                let _ = self.compile_expr(&args[0].value)?;
                let a = self.compile_expr(&args[1].value)?;
                let label = if method == "with_addr" {
                    "ptr.with_addr"
                } else {
                    "ptr.with_addr_mut"
                };
                Ok(Some(to_ptr(self, a, label)))
            }
            // addr: usize -> *_ T  (ptr.from_exposed / ptr.from_exposed_mut)
            "from_exposed" | "from_exposed_mut" if args.len() == 1 => {
                let a = self.compile_expr(&args[0].value)?;
                let label = if method == "from_exposed" {
                    "ptr.from_exposed"
                } else {
                    "ptr.from_exposed_mut"
                };
                Ok(Some(to_ptr(self, a, label)))
            }
            // (field_ptr: *_ F, offset: usize) -> *_ T
            //   (ptr.container_of / ptr.container_of_mut)
            //
            // Intrusive-DS pointer recovery — subtract the field
            // offset from the field-pointer's address bits. The
            // provenance-preserving lowering the spec describes is
            // `field_ptr.with_addr(field_ptr.addr() - offset)`, which
            // is exactly the `ptrtoint` → integer subtract → `inttoptr`
            // sequence emitted here.
            "container_of" | "container_of_mut" if args.len() == 2 => {
                let field_ptr_val = self.compile_expr(&args[0].value)?;
                let offset_val = self.compile_expr(&args[1].value)?;
                let label = if method == "container_of" {
                    "ptr.container_of"
                } else {
                    "ptr.container_of_mut"
                };
                let field_ptr_i64 = to_i64(self, field_ptr_val, &format!("{label}.fp"));
                let offset_i64 = to_i64(self, offset_val, &format!("{label}.off"));
                let result = self
                    .builder
                    .build_int_sub(
                        field_ptr_i64.into_int_value(),
                        offset_i64.into_int_value(),
                        &format!("{label}.bits"),
                    )
                    .unwrap();
                Ok(Some(to_ptr(self, result.into(), label)))
            }
            // `ptr.const(place)` / `ptr.mut(place)` — raw pointer
            // construction from a place expression (typechecker
            // place-validator gate is upstream — design.md § Raw
            // Pointer Construction, v60 item 19). The result is the
            // place's storage address as a genuine `ptr` value.
            // `ptr_place_addr` resolves the full place grammar the
            // typechecker accepts — binding / `self` / field access /
            // tuple index / Vec index / dereference chains — rooting
            // through `get_data_ptr` so a `ref` / `mut ref` / RC-promoted
            // root yields the pointee address, not the slot address.
            // `const` and `mut` share one path: the address is identical
            // (LLVM `ptr` is unqualified); mutability is the typechecker's
            // concern. `None` for an unresolvable shape (call-rooted base,
            // unknown struct type) falls through to the generic
            // method-call diagnostic rather than emit a wrong address.
            "const" | "mut" if args.len() == 1 => match self.ptr_place_addr(&args[0].value) {
                Some(ptr) => Ok(Some(ptr.into())),
                None => Ok(None),
            },
            // `ptr.null[T]()` / `ptr.null_mut[T]()` -> the all-zeroes
            // pointer (LLVM `ptr null`). The two methods differ only
            // in their typechecker-reported return type (`*const T`
            // vs `*mut T`); the codegen value is identical.
            "null" | "null_mut" if args.is_empty() => Ok(Some(ptr_ty.const_null().into())),
            // `ptr.dangling[T]()` / `ptr.dangling_mut[T]()` -> a
            // non-null pointer aligned to T's natural alignment, *not*
            // dereferenceable. Spec: design.md § Raw Pointer
            // Construction (v60 item 19); mirrors Rust's
            // `NonNull::dangling` (= `align_of::<T>() as *const T`).
            //
            // T-aware lowering would consult the type argument and
            // emit `align_of[T]`. The type argument is not threaded to
            // this hook, so v1 emits a fixed alignment of 8 (the max
            // alignment of any built-in primitive on a 64-bit target —
            // correct for every T whose alignment is <= 8, conservative
            // for over-aligned SIMD / `#[repr(align(N))]` types),
            // reseated into a `ptr` via constant `inttoptr`. The actual
            // deref of a dangling pointer is unsafe and *always* UB; the
            // only observable property is non-null + alignment, both of
            // which hold here. Tracker: phase-5-diagnostics line 573.
            "dangling" | "dangling_mut" if args.is_empty() => Ok(Some(
                i64_ty.const_int(8, false).const_to_pointer(ptr_ty).into(),
            )),
            // `ptr.is_null[T](p)` -> `p == 0` as bool (i1). The
            // typechecker reports the result as `Type::Bool`; codegen
            // returns an i1 matching how the BinOp::Eq path produces
            // bool values (`build_int_compare(EQ, ...)`).
            "is_null" if args.len() == 1 => {
                let p = self.compile_expr(&args[0].value)?;
                let p_i64 = to_i64(self, p, "ptr.is_null.p");
                let zero = i64_ty.const_zero();
                let result = self
                    .builder
                    .build_int_compare(
                        IntPredicate::EQ,
                        p_i64.into_int_value(),
                        zero,
                        "ptr.is_null",
                    )
                    .unwrap();
                Ok(Some(result.into()))
            }
            _ => Ok(None),
        }
    }
}
