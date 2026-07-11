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
//!
//! Deferred: `get_or_init` (needs closure-in-codegen), module-level `OnceLock`
//! globals (static-init prologue), and heap-`T` move-suppression on `set`.

use crate::ast::*;

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
            "get_or_init" => Err(
                "codegen: `OnceLock`/`OnceCell`.get_or_init(...) is not yet supported under \
                 `karac build` (the closure-init form is interpreter-only at v1; a tracked \
                 follow-on). Run it with `karac run --interp`, or use `if not c.is_set() { \
                 c.set(init()); } c.get()` for a build-compatible shape."
                    .to_string(),
            ),
            _ => Err(format!(
                "codegen: unsupported OnceLock/OnceCell method `{method}` (only \
                 set/get/is_set are lowered)"
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
}
