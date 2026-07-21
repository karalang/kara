//! `OnceLock[T]` / `OnceCell[T]` method codegen — `set` / `get` / `is_set`.
//!
//! Both primitives lower to the opaque `*mut KaracOnce` handle returned by
//! `OnceLock.new()` / `OnceCell.new()` (see `assoc_call.rs`); the write-once
//! cell lives in `runtime/src/once.rs`. Like `channel.rs`, the payload is
//! type-erased: `T` is recovered from `once_var_types` (populated by
//! `register_var_from_type_expr` from the binding's `OnceLock[T]` annotation),
//! lowered to its LLVM shape, and its store size threaded as the per-`set`
//! `value_size`.
//!
//! v1 floor (mirrors the interpreter, `src/interpreter/method_call_once.rs`):
//! - `set(v)` seals the cell, returning `Result[Unit, AlreadySetError[T]]` —
//!   `Ok(())` when this call won, `Err(AlreadySetError { rejected: v })` when
//!   it was already set (the runtime does not copy on failure, so `v`'s words
//!   ride back through the `Err` payload).
//! - `get()` returns `Option[ref T]` — a stable borrow into the sealed value
//!   (`Some`) or `None`, built with the `Map.get` phi shape.
//! - `is_set()` returns `bool`.
//! - `get_or_init(|| ...)` runs the closure once when unset, seals the cell,
//!   and returns the borrow — the closure fires only on the `unset` branch.
//!
//! Element-type support (B-2026-07-12-2, completed): `set`/`get`/`is_set`/
//! `get_or_init` all handle ANY `T` — scalar, `String`/`Vec` (heap-fitting,
//! 3 words), and WIDE `T` (> 3 words: a multi-field struct, a struct with a
//! heap field, a 4+-scalar struct). A wide `T`'s value can't fit the 3-word
//! `Option`/`Result` inline payload area, so `get` heap-BOXES it (a shallow
//! bit-copy behind a pointer, the `Vec.get`/`Map.get` `Option[ref T]`
//! convention — freed box-only for the borrow) and `set`'s
//! `Err(AlreadySetError { rejected })` payload boxes past the 5-word `Result`
//! area; a discarded wide/struct-with-heap rejected value is freed by the
//! `FreeInlineResultPayload` struct-drop arm. `get_or_init` with a HEAP-OWNING
//! `T` returns a BORROWED VIEW — every heap cap in the returned value-copy is
//! zeroed (`zero_heap_caps_in_value`, the Interner/Arena `{ptr,len,cap=0}`
//! convention), so the caller's cap-guarded frees no-op and the cell's
//! `FreeOnceHandle` elem-drop stays the sealed value's single owner; a
//! racing-`set` LOSER's freshly-built value is freed on the `goi.set.lost`
//! arm (nothing else owns it).

use crate::ast::*;

use inkwell::types::{BasicType, BasicTypeEnum};
use inkwell::values::BasicValueEnum;
use inkwell::IntPredicate;

impl<'ctx> super::Codegen<'ctx> {
    /// Lower a `OnceLock`/`OnceCell` method call on a local binding `recv`.
    /// Dispatched from `compile_method_call` gated on `once_var_types`
    /// membership.
    pub(super) fn compile_once_method(
        &mut self,
        recv: &str,
        method: &str,
        args: &[CallArg],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        match method {
            "set" => self.compile_once_set(recv, args),
            "get" => self.compile_once_get(recv),
            "is_set" => self.compile_once_is_set(recv),
            "get_or_init" => self.compile_once_get_or_init(recv, args),
            _ => Err(format!(
                "codegen: unsupported OnceLock/OnceCell method `{method}` (only \
                 set/get/is_set/get_or_init are lowered)"
            )),
        }
    }

    /// Load the opaque `*mut KaracOnce` handle from the binding's slot.
    fn load_once_handle(
        &mut self,
        recv: &str,
    ) -> Result<inkwell::values::PointerValue<'ctx>, String> {
        let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        let slot = self
            .get_data_ptr(recv)
            .ok_or_else(|| format!("unknown OnceLock/OnceCell binding '{recv}'"))?;
        Ok(self
            .builder
            .build_load(ptr_ty, slot, "once.handle")
            .unwrap()
            .into_pointer_value())
    }

    /// `T`'s LLVM type + store size, from the binding's recorded element type.
    fn once_elem_ty_and_size(
        &mut self,
        recv: &str,
    ) -> Result<(inkwell::types::BasicTypeEnum<'ctx>, u64), String> {
        let te = self
            .once_var_types
            .get(recv)
            .map(|(te, _)| te.clone())
            .ok_or_else(|| format!("OnceLock/OnceCell binding '{recv}' missing element type"))?;
        let elem_ty = self.llvm_type_for_type_expr(&te);
        let size = self.ensure_target_data()?.get_store_size(&elem_ty);
        Ok((elem_ty, size))
    }

    /// `cell.is_set() -> bool`. The runtime returns `u8` (0/1); codegen rides
    /// that as the bool value directly, mirroring `Map.contains_key`.
    fn compile_once_is_set(&mut self, recv: &str) -> Result<BasicValueEnum<'ctx>, String> {
        let handle = self.load_once_handle(recv)?;
        let f = self
            .module
            .get_function("karac_runtime_once_is_set")
            .expect("karac_runtime_once_is_set declared in Codegen::new");
        let raw = self
            .builder
            .build_call(f, &[handle.into()], "once.is_set")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();
        // Runtime returns `u8` (0/1); codegen's `bool` is `i1`. Compare `!= 0`
        // so the value is a proper `i1` for `if`/`match`/Display.
        let zero = raw.get_type().const_zero();
        let as_bool = self
            .builder
            .build_int_compare(IntPredicate::NE, raw, zero, "once.is_set.bool")
            .unwrap();
        Ok(as_bool.into())
    }

    /// `cell.set(v) -> Result[Unit, AlreadySetError[T]]`. Spill `v` to a stack
    /// slot, `karac_runtime_once_set` copies `value_size` bytes into the cell
    /// on the winning call. Build `Ok(())` when won (1), `Err(AlreadySetError {
    /// rejected: v })` when already set (0). `AlreadySetError { rejected: T }`
    /// is a single-field struct, so its word layout equals `T`'s — `v`'s words
    /// fill the `Err` payload directly.
    fn compile_once_set(
        &mut self,
        recv: &str,
        args: &[CallArg],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        if args.len() != 1 {
            return Err(format!(
                "codegen: OnceLock/OnceCell.set expects 1 argument, found {}",
                args.len()
            ));
        }
        let (elem_ty, size) = self.once_elem_ty_and_size(recv)?;
        let handle = self.load_once_handle(recv)?;
        let fn_val = self.current_fn.unwrap();
        let val = self.compile_expr(&args[0].value)?;
        // Move-suppress the source (B-2026-07-12-2 gap 1): `set(v)` transfers
        // `v`'s buffer to EITHER the cell (win — freed by `FreeOnceHandle`'s
        // elem_drop) OR the returned `Err(AlreadySetError { rejected: v })`
        // payload (lose — freed by that payload's drop). In BOTH cases the source
        // binding must not also free it. A named `String`/`Vec` source has its
        // `cap` zeroed so its scope-exit `FreeVecBuffer` no-ops (else double-free
        // with the cell's elem_drop); a fresh-temp source (`"x".to_string()`) is
        // a no-op here (nothing else owns it — the cell/Err payload is the sole
        // owner). The loaded `val` SSA keeps its real `cap`, so the Err payload
        // still carries a live buffer. Mirrors the `Vec.push` arg suppression.
        self.suppress_source_vec_cleanup_for_arg(&args[0].value);
        let val_slot = self.create_entry_alloca(fn_val, "once.set.val", elem_ty);
        self.builder.build_store(val_slot, val).unwrap();

        let size_const = self.context.i64_type().const_int(size, false);
        let set_fn = self
            .module
            .get_function("karac_runtime_once_set")
            .expect("karac_runtime_once_set declared in Codegen::new");
        let won = self
            .builder
            .build_call(
                set_fn,
                &[handle.into(), val_slot.into(), size_const.into()],
                "once.set.won",
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();

        // Branch on won == 1. Result layout: Ok tag 1, Err tag 0.
        let result_ty = self
            .enum_layouts
            .get("Result")
            .ok_or_else(|| "Result layout not registered before OnceLock codegen".to_string())?
            .llvm_type;
        let result_slot = self.create_entry_alloca(fn_val, "once.set.result", result_ty.into());

        let zero_i8 = won.get_type().const_zero();
        let is_won = self
            .builder
            .build_int_compare(IntPredicate::NE, won, zero_i8, "once.set.is_won")
            .unwrap();
        let ok_bb = self.context.append_basic_block(fn_val, "once.set.ok");
        let err_bb = self.context.append_basic_block(fn_val, "once.set.err");
        let cont_bb = self.context.append_basic_block(fn_val, "once.set.cont");
        self.builder
            .build_conditional_branch(is_won, ok_bb, err_bb)
            .unwrap();

        // Ok(()) — Unit payload.
        self.builder.position_at_end(ok_bb);
        let unit = self.context.i64_type().const_zero().into();
        let ok_agg = self.build_nonshared_enum_value("Result", "Ok", &[unit])?;
        self.builder
            .build_store(result_slot, ok_agg.into_struct_value())
            .unwrap();
        self.builder.build_unconditional_branch(cont_bb).unwrap();

        // Err(AlreadySetError { rejected: v }) — v's words are the Err payload.
        self.builder.position_at_end(err_bb);
        let err_agg = self.build_nonshared_enum_value("Result", "Err", &[val])?;
        self.builder
            .build_store(result_slot, err_agg.into_struct_value())
            .unwrap();
        self.builder.build_unconditional_branch(cont_bb).unwrap();

        self.builder.position_at_end(cont_bb);
        let result = self
            .builder
            .build_load(result_ty, result_slot, "once.set.result.val")
            .unwrap();
        Ok(result)
    }

    /// `cell.get() -> Option[ref T]`. `karac_runtime_once_get` returns a stable
    /// borrow into the sealed value or null. Non-null → `Some(<T loaded>)`,
    /// null → `None` — the `Map.get` alias-into-container phi shape.
    fn compile_once_get(&mut self, recv: &str) -> Result<BasicValueEnum<'ctx>, String> {
        let (elem_ty, _size) = self.once_elem_ty_and_size(recv)?;
        let handle = self.load_once_handle(recv)?;
        let fn_val = self.current_fn.unwrap();
        let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());

        let get_fn = self
            .module
            .get_function("karac_runtime_once_get")
            .expect("karac_runtime_once_get declared in Codegen::new");
        let got = self
            .builder
            .build_call(get_fn, &[handle.into()], "once.get.ptr")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();

        let is_null = self.builder.build_is_null(got, "once.get.is_null").unwrap();
        let some_bb = self.context.append_basic_block(fn_val, "once.get.some");
        let none_bb = self.context.append_basic_block(fn_val, "once.get.none");
        let merge_bb = self.context.append_basic_block(fn_val, "once.get.merge");
        self.builder
            .build_conditional_branch(is_null, none_bb, some_bb)
            .unwrap();

        // Some — load T through the borrow and split into payload words.
        self.builder.position_at_end(some_bb);
        let loaded = self
            .builder
            .build_load(elem_ty, got, "once.get.val")
            .unwrap();
        // Cap the requested word count at the `Option` inline payload area
        // (3 words). A FITTING `T` (scalar / `String` / `Vec` / small struct,
        // `<= 3` words) fills the payload inline as before. A WIDE `T` (`> 3`
        // words — a struct with a `String` field, a `4+`-scalar struct) would
        // overflow `build_option_some_via_phis` (which inserts one word per
        // element into the fixed 3-word area and PANICS past field 3), so we
        // hand `coerce_to_payload_words` the AREA, not the full width: it then
        // heap-BOXES the value (a shallow bit-copy, ptr in word 0) — exactly
        // the `Vec.get`/`Map.get` `Option[ref T]` convention for wide elements
        // (collections.rs). `reconstruct_payload_value` deboxes on the mirror
        // predicate (`want > field_words.len()`), and because `get` is a BORROW
        // call (`scrutinee_is_borrow_call`), the consumer takes no arm-drop and
        // `track_freshtemp_boxed_enum_scrutinee` runs a box-ONLY free — the box
        // copy's interior heap (aliasing the sealed value) is left to the cell's
        // `FreeOnceHandle` elem-drop, so no leak and no double-free
        // (B-2026-07-12-2 gap 3).
        const OPTION_PAYLOAD_WORDS: usize = 3;
        let num_words = Self::llvm_type_word_count(elem_ty).clamp(1, OPTION_PAYLOAD_WORDS);
        let some_words = self.coerce_to_payload_words(loaded, num_words)?;
        let some_end_bb = self.builder.get_insert_block().unwrap();
        self.builder.build_unconditional_branch(merge_bb).unwrap();

        // None.
        self.builder.position_at_end(none_bb);
        self.builder.build_unconditional_branch(merge_bb).unwrap();

        self.builder.position_at_end(merge_bb);
        let _ = ptr_ty; // ptr_ty reserved for the ref-payload variant (heap-T follow-on).
        let agg = self.build_option_some_via_phis(&some_words, some_end_bb, none_bb, "once.opt");
        Ok(agg)
    }

    /// `cell.get_or_init(init: || -> T) -> ref T`. If the cell is unset, run the
    /// `init` closure once, `set` the cell with its result, then return the
    /// borrow; if already set, return the existing value without invoking the
    /// closure. The closure fires at most once (only on the `unset` branch).
    /// The returned `ref T` is represented as the loaded `T` value (the `get`
    /// precedent) — and for a HEAP-OWNING `T` (`String`/`Vec`, or a struct with
    /// a heap field) as a BORROWED VIEW of that value: every heap cap zeroed,
    /// so the caller's cap-guarded frees no-op against the cell's sealed heap
    /// (the B-2026-07-12-2 follow-on that used to loud-gate here). A losing
    /// racing `set` frees the loser's freshly-built heap value in place.
    fn compile_once_get_or_init(
        &mut self,
        recv: &str,
        args: &[CallArg],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        if args.len() != 1 {
            return Err(format!(
                "codegen: OnceLock/OnceCell.get_or_init expects 1 argument (a closure), found {}",
                args.len()
            ));
        }
        let elem_te = self
            .once_var_types
            .get(recv)
            .map(|(te, _)| te.clone())
            .ok_or_else(|| format!("OnceLock/OnceCell binding '{recv}' missing element type"))?;
        // A HEAP-OWNING element (`String`/`Vec`, or a struct with a heap
        // field) returns a BORROWED VIEW: the loaded value-copy aliases the
        // cell's sealed heap, so every heap cap in the returned value is
        // zeroed (`zero_heap_caps_in_value`) — the `{ptr, len, cap = 0}`
        // static-buffer convention (Interner `resolve` / Arena `get`). All
        // downstream frees are cap-guarded (`FreeVecBuffer`, the struct-drop
        // walk), so the caller's binding no-ops at scope exit and the cell's
        // `FreeOnceHandle` elem-drop stays the single owner. This closes the
        // B-2026-07-12-2 follow-on that previously loud-gated heap `T` here.
        let elem_is_heap = self.type_expr_has_drop_heap(&elem_te);
        let (elem_ty, size) = self.once_elem_ty_and_size(recv)?;
        let handle = self.load_once_handle(recv)?;
        let fn_val = self.current_fn.unwrap();
        let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());

        // Branch on the current `is_set` state — run the closure only when unset.
        let is_set_fn = self
            .module
            .get_function("karac_runtime_once_is_set")
            .expect("karac_runtime_once_is_set declared in Codegen::new");
        let is_set_raw = self
            .builder
            .build_call(is_set_fn, &[handle.into()], "goi.is_set")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();
        let zero = is_set_raw.get_type().const_zero();
        let is_set = self
            .builder
            .build_int_compare(IntPredicate::NE, is_set_raw, zero, "goi.is_set.b")
            .unwrap();
        let init_bb = self.context.append_basic_block(fn_val, "goi.init");
        let cont_bb = self.context.append_basic_block(fn_val, "goi.cont");
        self.builder
            .build_conditional_branch(is_set, cont_bb, init_bb)
            .unwrap();

        // ── init: compile + invoke the closure, then seal the cell ──────────
        self.builder.position_at_end(init_bb);
        let fat = self.compile_expr(&args[0].value)?;
        let fat_sv = fat.into_struct_value();
        let clo_fn_ptr = self
            .builder
            .build_extract_value(fat_sv, 0, "goi.clo.fn")
            .unwrap()
            .into_pointer_value();
        let clo_env_ptr = self
            .builder
            .build_extract_value(fat_sv, 1, "goi.clo.env")
            .unwrap()
            .into_pointer_value();
        // Closure ABI is `T(ptr env)` (see par_blocks trampoline).
        let closure_fn_ty = elem_ty.fn_type(&[ptr_ty.into()], false);
        let init_val = self
            .builder
            .build_indirect_call(
                closure_fn_ty,
                clo_fn_ptr,
                &[clo_env_ptr.into()],
                "goi.invoke",
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic();
        let val_slot = self.create_entry_alloca(fn_val, "goi.val", elem_ty);
        self.builder.build_store(val_slot, init_val).unwrap();
        let size_const = self.context.i64_type().const_int(size, false);
        let set_fn = self
            .module
            .get_function("karac_runtime_once_set")
            .expect("karac_runtime_once_set declared in Codegen::new");
        let set_won = self
            .builder
            .build_call(
                set_fn,
                &[handle.into(), val_slot.into(), size_const.into()],
                "goi.set",
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();
        // Racing-`set` loser cleanup (heap `T` only): if another task sealed
        // the cell between our `is_set` check and our `set`, the runtime
        // rejected our freshly-built `init_val` — nothing else owns its heap,
        // so free its buffers here (cap-guarded, so a heap-free shape emits
        // nothing). Single-threaded programs never take this branch; without
        // it a lost race leaks the loser's value.
        if elem_is_heap {
            let lost_bb = self.context.append_basic_block(fn_val, "goi.set.lost");
            let sealed_bb = self.context.append_basic_block(fn_val, "goi.set.sealed");
            let zero_i8 = set_won.get_type().const_zero();
            let won_b = self
                .builder
                .build_int_compare(IntPredicate::NE, set_won, zero_i8, "goi.set.won.b")
                .unwrap();
            self.builder
                .build_conditional_branch(won_b, sealed_bb, lost_bb)
                .unwrap();
            self.builder.position_at_end(lost_bb);
            self.free_heap_value_at(val_slot, elem_ty);
            self.builder.build_unconditional_branch(sealed_bb).unwrap();
            self.builder.position_at_end(sealed_bb);
        }
        self.builder.build_unconditional_branch(cont_bb).unwrap();

        // ── cont: the cell is now sealed on both paths — load + return it ───
        // A concurrent racing `set` between our `is_set` check and our `set`
        // would win, and `once_get` returns the winner's value (a heap loser's
        // buffers are freed on the `goi.set.lost` arm above; a scalar loser
        // needs no cleanup) — the race-safe "one winner" semantics the spec
        // requires.
        self.builder.position_at_end(cont_bb);
        let get_fn = self
            .module
            .get_function("karac_runtime_once_get")
            .expect("karac_runtime_once_get declared in Codegen::new");
        let vptr = self
            .builder
            .build_call(get_fn, &[handle.into()], "goi.get")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        let result = self
            .builder
            .build_load(elem_ty, vptr, "goi.result")
            .unwrap();
        // Heap `T`: hand back a BORROWED VIEW — every heap cap in the value
        // copy zeroed, so cap-guarded downstream frees no-op and the sealed
        // value's single owner stays the cell (see the gate-removal note
        // above).
        if elem_is_heap {
            return Ok(self.zero_heap_caps_in_value(result));
        }
        Ok(result)
    }

    /// Return `val` with the `cap` word of every String/Vec (`{ptr,len,cap}`)
    /// field zeroed, recursing into nested struct/tuple fields — the value
    /// becomes a borrowed `cap = 0` view whose cap-guarded frees all no-op.
    /// Fields are recognized structurally (`== vec_struct_type()`), the same
    /// signal `aggregate_has_heap_field` / `FreeVecBuffer`'s recursive element
    /// drop use. Non-aggregate values pass through unchanged.
    pub(super) fn zero_heap_caps_in_value(
        &self,
        val: BasicValueEnum<'ctx>,
    ) -> BasicValueEnum<'ctx> {
        let BasicValueEnum::StructValue(sv) = val else {
            return val;
        };
        let vec_ty = self.vec_struct_type();
        let zero = self.context.i64_type().const_zero();
        if sv.get_type() == vec_ty {
            return self
                .builder
                .build_insert_value(sv, zero, 2, "goi.cap0")
                .unwrap()
                .into_struct_value()
                .into();
        }
        let st = sv.get_type();
        let mut out = sv;
        for i in 0..st.count_fields() {
            let Some(BasicTypeEnum::StructType(field_st)) = st.get_field_type_at_index(i) else {
                continue;
            };
            let field_is_heapish = field_st == vec_ty || self.aggregate_has_heap_field(field_st);
            if !field_is_heapish {
                continue;
            }
            let field = self
                .builder
                .build_extract_value(out, i, "goi.cap0.f")
                .unwrap();
            let zeroed = self.zero_heap_caps_in_value(field);
            out = self
                .builder
                .build_insert_value(out, zeroed, i, "goi.cap0.iv")
                .unwrap()
                .into_struct_value();
        }
        out.into()
    }

    /// Emit cap-guarded frees for the heap buffers of the value stored at
    /// `slot` (typed `elem_ty`) — a bare String/Vec frees its own buffer; a
    /// struct/tuple walks its fields via `emit_aggregate_heap_field_frees`.
    /// Scalars and heap-free aggregates emit nothing.
    fn free_heap_value_at(
        &mut self,
        slot: inkwell::values::PointerValue<'ctx>,
        elem_ty: inkwell::types::BasicTypeEnum<'ctx>,
    ) {
        let vec_ty = self.vec_struct_type();
        match elem_ty {
            BasicTypeEnum::StructType(st) if st == vec_ty => {
                // Wrap the bare triple as a 1-field aggregate walk: reuse the
                // same cap-guarded free by treating `slot` as the field ptr.
                // `emit_aggregate_heap_field_frees` GEPs fields off a struct
                // type, so for the bare case emit the guarded free inline.
                let i64_t = self.context.i64_type();
                let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                let fn_val = self.current_fn.unwrap();
                let data_pp = self
                    .builder
                    .build_struct_gep(vec_ty, slot, 0, "goi.lost.data.pp")
                    .unwrap();
                let data = self
                    .builder
                    .build_load(ptr_ty, data_pp, "goi.lost.data")
                    .unwrap()
                    .into_pointer_value();
                let cap_pp = self
                    .builder
                    .build_struct_gep(vec_ty, slot, 2, "goi.lost.cap.pp")
                    .unwrap();
                let cap = self
                    .builder
                    .build_load(i64_t, cap_pp, "goi.lost.cap")
                    .unwrap()
                    .into_int_value();
                let has_cap = self
                    .builder
                    .build_int_compare(
                        IntPredicate::SGT,
                        cap,
                        i64_t.const_zero(),
                        "goi.lost.has_cap",
                    )
                    .unwrap();
                let free_bb = self.context.append_basic_block(fn_val, "goi.lost.free");
                let done_bb = self.context.append_basic_block(fn_val, "goi.lost.done");
                self.builder
                    .build_conditional_branch(has_cap, free_bb, done_bb)
                    .unwrap();
                self.builder.position_at_end(free_bb);
                let free_fn = self
                    .module
                    .get_function("free")
                    .expect("free declared in Codegen::new");
                self.builder
                    .build_call(free_fn, &[data.into()], "")
                    .unwrap();
                self.builder.build_unconditional_branch(done_bb).unwrap();
                self.builder.position_at_end(done_bb);
            }
            BasicTypeEnum::StructType(st) => {
                self.emit_aggregate_heap_field_frees(slot, st);
            }
            _ => {}
        }
    }
}
