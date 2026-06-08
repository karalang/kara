//! `BoundedChannel[T]` AOT codegen — `send` / `recv` lowering.
//!
//! `new` is lowered in `assoc_call.rs`; the runtime FFI lives in
//! `runtime/src/bounded_channel.rs`. This mirrors the unbounded `channel.rs`
//! lowering with three differences: (1) the receiver is a `BoundedChannel {
//! handle_id: i64 }` struct — the runtime `*mut KaracBoundedChannel` is
//! recovered by `inttoptr`ing field 0, not by compiling a raw pointer; (2)
//! `send` returns `Result[Unit, ChannelError]` (built from the runtime's
//! buffered/full discriminant); (3) `recv` is the non-blocking `Option[T]`
//! shape (no Sender/Receiver split, no blocking, no close).
//!
//! Dispatch: `method_call.rs` routes `BoundedChannel.send` / `.recv` here off
//! the `dispatch_key` (recorded by the typechecker's
//! `infer_bounded_channel_method`), ahead of the unbounded-channel
//! `channel_elem_types` gate. `recv` reads its element `T` from
//! `channel_elem_types` (populated for `recv` only) via the shared
//! `channel_elem_ty_and_size`; `send` sizes its payload from the argument.

use inkwell::values::{BasicValueEnum, PointerValue};
use inkwell::{AddressSpace, IntPredicate};

use crate::ast::{CallArg, Expr};

impl<'ctx> super::Codegen<'ctx> {
    /// Recover the runtime `*mut KaracBoundedChannel` from a `BoundedChannel {
    /// handle_id: i64 }` receiver: compile the struct, extract field 0, and
    /// `inttoptr`. Mirrors `TaskHandle.join`'s `task_id`→pointer recovery.
    fn bounded_channel_handle_ptr(&mut self, object: &Expr) -> Result<PointerValue<'ctx>, String> {
        let recv = self.compile_expr(object)?.into_struct_value();
        let handle = self
            .builder
            .build_extract_value(recv, 0, "bch.handle")
            .unwrap()
            .into_int_value();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        Ok(self
            .builder
            .build_int_to_ptr(handle, ptr_ty, "bch.ptr")
            .unwrap())
    }

    /// `bc.send(value)` → `Result[Unit, ChannelError]`. Spills the value to a
    /// stack slot, calls `karac_runtime_bounded_channel_send` (returns 1 when
    /// buffered, 0 when the buffer is at capacity), and builds `Ok(())` vs
    /// `Err(ChannelError.Full)` from the discriminant. `T`'s size comes from
    /// the argument value, so no `channel_elem_types` entry is needed here.
    pub(super) fn compile_bounded_channel_send(
        &mut self,
        object: &Expr,
        args: &[CallArg],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        if args.len() != 1 {
            return Err("BoundedChannel.send expects exactly one argument".to_string());
        }
        let fn_val = self.current_fn.unwrap();
        // Receiver first (source order), then the value.
        let ch = self.bounded_channel_handle_ptr(object)?;
        let arg_val = self.compile_expr(&args[0].value)?;
        let elem_ty = arg_val.get_type();
        let elem_size = self.ensure_target_data()?.get_store_size(&elem_ty);
        let slot = self.create_entry_alloca(fn_val, "bch.send.val", elem_ty);
        self.builder.build_store(slot, arg_val).unwrap();

        let size_const = self.context.i64_type().const_int(elem_size, false);
        let send_fn = self
            .module
            .get_function("karac_runtime_bounded_channel_send")
            .expect("karac_runtime_bounded_channel_send declared in Codegen::new");
        let stored_i8 = self
            .builder
            .build_call(
                send_fn,
                &[ch.into(), slot.into(), size_const.into()],
                "bch.send.stored",
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();
        let zero_i8 = self.context.i8_type().const_zero();
        let stored = self
            .builder
            .build_int_compare(IntPredicate::NE, stored_i8, zero_i8, "bch.send.ok")
            .unwrap();

        let ok_bb = self.context.append_basic_block(fn_val, "bch.send.ok.bb");
        let full_bb = self.context.append_basic_block(fn_val, "bch.send.full.bb");
        let merge_bb = self.context.append_basic_block(fn_val, "bch.send.merge");
        self.builder
            .build_conditional_branch(stored, ok_bb, full_bb)
            .unwrap();

        // Buffered → `Ok(())` (Unit payload is the i64-zero unit value).
        self.builder.position_at_end(ok_bb);
        let unit_val = self.context.i64_type().const_zero().into();
        let ok_val = self.build_nonshared_enum_value("Result", "Ok", &[unit_val])?;
        let ok_end = self.builder.get_insert_block().unwrap();
        self.builder.build_unconditional_branch(merge_bb).unwrap();

        // Full → `Err(ChannelError.Full)`.
        self.builder.position_at_end(full_bb);
        let full_inner = self.build_nonshared_enum_value("ChannelError", "Full", &[])?;
        let err_val = self.build_nonshared_enum_value("Result", "Err", &[full_inner])?;
        let full_end = self.builder.get_insert_block().unwrap();
        self.builder.build_unconditional_branch(merge_bb).unwrap();

        // Merge — phi the two `Result` aggregates (same enum llvm_type).
        self.builder.position_at_end(merge_bb);
        let phi = self
            .builder
            .build_phi(ok_val.get_type(), "bch.send.result")
            .unwrap();
        phi.add_incoming(&[(&ok_val, ok_end), (&err_val, full_end)]);
        Ok(phi.as_basic_value())
    }

    /// `bc.recv()` → `Option[T]`. Non-blocking: calls
    /// `karac_runtime_bounded_channel_recv` (1 with a value, 0 on empty) and
    /// builds `Some`/`None` from the discriminant — identical shape to the
    /// unbounded `try_recv` lowering.
    pub(super) fn compile_bounded_channel_recv(
        &mut self,
        object: &Expr,
        call_span: &crate::token::Span,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let (elem_ty, elem_size) = self.channel_elem_ty_and_size(call_span)?;
        let fn_val = self.current_fn.unwrap();
        let ch = self.bounded_channel_handle_ptr(object)?;
        let out = self.create_entry_alloca(fn_val, "bch.recv.out", elem_ty);

        let size_const = self.context.i64_type().const_int(elem_size, false);
        let recv_fn = self
            .module
            .get_function("karac_runtime_bounded_channel_recv")
            .expect("karac_runtime_bounded_channel_recv declared in Codegen::new");
        let found_i8 = self
            .builder
            .build_call(
                recv_fn,
                &[ch.into(), out.into(), size_const.into()],
                "bch.recv.found",
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();
        let zero_i8 = self.context.i8_type().const_zero();
        let found = self
            .builder
            .build_int_compare(IntPredicate::NE, found_i8, zero_i8, "bch.recv.some")
            .unwrap();

        let some_bb = self.context.append_basic_block(fn_val, "bch.recv.some.bb");
        let none_bb = self.context.append_basic_block(fn_val, "bch.recv.none.bb");
        let merge_bb = self.context.append_basic_block(fn_val, "bch.recv.merge");
        self.builder
            .build_conditional_branch(found, some_bb, none_bb)
            .unwrap();

        // Some — load the value and split into Option's 3 payload words.
        self.builder.position_at_end(some_bb);
        let val = self
            .builder
            .build_load(elem_ty, out, "bch.recv.val")
            .unwrap();
        let some_words = self.coerce_to_payload_words(val, 3)?;
        let some_end_bb = self.builder.get_insert_block().unwrap();
        self.builder.build_unconditional_branch(merge_bb).unwrap();

        // None.
        self.builder.position_at_end(none_bb);
        self.builder.build_unconditional_branch(merge_bb).unwrap();

        // Merge — phi the words and assemble the Option aggregate.
        self.builder.position_at_end(merge_bb);
        let agg = self.build_option_some_via_phis(&some_words, some_end_bb, none_bb, "bch.recv");
        Ok(agg)
    }
}
