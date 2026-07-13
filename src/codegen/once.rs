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
//!   Scalar `T` only (an aggregate closure return uses the deferred sret ABI).
//!
//! Element-type support: scalar and small all-scalar-struct `T` work for
//! `set`/`get`/`is_set`; `get_or_init` additionally requires a single-scalar
//! `T`. Heap-owning / wide `T` (and a non-scalar `get_or_init`) are loud-gated
//! with a `--interp` hint — tracked as B-2026-07-12-2.

use crate::ast::*;

use inkwell::types::BasicType;
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

    /// Loud-gate the element types the v1 `set`/`get` codegen can't handle
    /// correctly yet: (1) a **heap-owning** `T` (`String`, `Vec[_]`, a struct/
    /// enum with a heap field) — `set` moves it into the cell but the
    /// rejected-value (`AlreadySetError`) path leaks it, and only the
    /// success-path element-drop is wired; (2) a **wide** `T` whose word count
    /// exceeds the `Option`/`Result` inline payload area (>3 words) — `get`'s
    /// `Option[ref T]` construction has no boxing path and would overflow the
    /// payload. Both need the deferred heap-`T` slice (move-suppression +
    /// rejected-value drop + payload boxing). Scalars and small all-scalar
    /// structs pass. Fail loudly (with a `--interp` hint) rather than emit a
    /// leak or a payload-overflow miscompile.
    fn reject_unsupported_once_elem(&mut self, recv: &str, method: &str) -> Result<(), String> {
        let te = self
            .once_var_types
            .get(recv)
            .map(|(te, _)| te.clone())
            .ok_or_else(|| format!("OnceLock/OnceCell binding '{recv}' missing element type"))?;
        let elem_ty = self.llvm_type_for_type_expr(&te);
        let wide = Self::llvm_type_word_count(elem_ty) > 3;
        // heap-but-FITTING `T` (`String` / `Vec[_]` / a small single-heap-field
        // struct, <=3 words) is now supported: recovery is sound (B-2026-07-12-27)
        // and the success-path element drop + rejected-value drop are wired below.
        // A WIDE `T` (>3 words) still overflows the `Option`/`Result` inline
        // payload area with no boxing path (gap 3) — keep it loud-gated.
        if wide {
            return Err(format!(
                "codegen: `OnceLock`/`OnceCell`.{method}` with an element type wider than the \
                 3-word inline payload is not yet supported under `karac build` (the wide-`T` \
                 payload-boxing slice is a tracked follow-on, B-2026-07-12-2 gap 3). Scalar, \
                 `String`/`Vec`, and small heap-fitting structs work; for a wide `T`, run with \
                 `karac run --interp` (or `KARAC_RUN_JIT=0`)."
            ));
        }
        Ok(())
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
        self.reject_unsupported_once_elem(recv, "set")?;
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
        self.reject_unsupported_once_elem(recv, "get")?;
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
        let num_words = (Self::llvm_type_word_count(elem_ty)).max(1);
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
    /// precedent — the borrow is a no-op auto-deref for the heap-free `T` this
    /// lowering supports). Heap/wide `T` is loud-gated like `set`/`get`.
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
        self.reject_unsupported_once_elem(recv, "get_or_init")?;
        let (elem_ty, size) = self.once_elem_ty_and_size(recv)?;
        // The closure ABI returns a multi-word AGGREGATE via sret (a hidden
        // out-pointer), not the direct-return this lowering assumes — so
        // `get_or_init` supports only a single-scalar `T` (`i64`/`f64`/`bool`/…)
        // at v1, the common lazy-compute-a-number/handle case. An aggregate `T`
        // (even a heap-free struct that `set`/`get` handle) is loud-gated here;
        // the sret closure-return path is part of the deferred heap-/wide-`T`
        // slice (B-2026-07-12-2). `set`/`get` don't invoke a closure, so they
        // accept small structs; only `get_or_init` needs this narrower gate.
        if !elem_ty.is_int_type() && !elem_ty.is_float_type() && !elem_ty.is_pointer_type() {
            return Err(
                "codegen: `OnceLock`/`OnceCell`.get_or_init(...) with a non-scalar element type \
                 is not yet supported under `karac build` (the closure returns the value by \
                 aggregate/sret, a deferred ABI — B-2026-07-12-2). Use a scalar `T`, or `if not \
                 c.is_set() { c.set(init()); } c.get()`, or run with `karac run --interp`."
                    .to_string(),
            );
        }
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
        self.builder
            .build_call(
                set_fn,
                &[handle.into(), val_slot.into(), size_const.into()],
                "goi.set",
            )
            .unwrap();
        self.builder.build_unconditional_branch(cont_bb).unwrap();

        // ── cont: the cell is now sealed on both paths — load + return it ───
        // A concurrent racing `set` between our `is_set` check and our `set`
        // would win, and `once_get` returns the winner's value (the losing
        // `init_val` is dropped by the caller's normal temp cleanup) — the
        // race-safe "one winner" semantics the spec requires.
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
        Ok(result)
    }
}
