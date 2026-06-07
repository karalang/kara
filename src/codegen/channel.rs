//! Channel-end method codegen — `Sender.send` / `Sender.clone` /
//! `Receiver.recv` / `Receiver.try_recv`.
//!
//! `Sender[T]` / `Receiver[T]` lower to the opaque `*mut KaracChannel`
//! pointer returned by `Channel.new()` (see `assoc_call.rs`); the queue and
//! refcount live in `runtime/src/channel.rs`. These methods marshal the
//! type-erased payload across the `karac_runtime_channel_*` FFI: the element
//! type `T` is recovered from the typechecker's `channel_elem_types`
//! side-table (keyed by the MethodCall span), lowered to its LLVM shape, and
//! its store size threaded as the per-call `elem_size`.
//!
//! v1 floor (mirrors the interpreter, non-blocking): `recv` returns the
//! zero-value of `T` on an empty queue (the runtime zero-fills the out slot),
//! so its lowering ignores the FFI presence discriminant; `try_recv` reads
//! the discriminant to build `Some`/`None`.

use crate::ast::*;

use inkwell::values::BasicValueEnum;
use inkwell::IntPredicate;

impl<'ctx> super::Codegen<'ctx> {
    pub(super) fn compile_channel_method(
        &mut self,
        object: &Expr,
        method: &str,
        args: &[CallArg],
        call_span: &crate::token::Span,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        match method {
            "send" => self.compile_channel_send(object, args, call_span),
            "recv" => self.compile_channel_recv(object, call_span),
            "try_recv" => self.compile_channel_try_recv(object, call_span),
            "clone" => self.compile_channel_clone(object),
            // The dispatch gate in `compile_method_call` only routes the four
            // methods above here.
            _ => unreachable!("compile_channel_method: unexpected method `{method}`"),
        }
    }

    /// Resolve the channel element type `T` for a channel-op call site to its
    /// `(LLVM type, store-size-in-bytes)`. The size feeds the type-erased
    /// `karac_runtime_channel_*` `elem_size` parameter. Missing means the
    /// typechecker didn't record the site — a contract break worth surfacing
    /// loudly rather than silently sending zero bytes.
    fn channel_elem_ty_and_size(
        &mut self,
        call_span: &crate::token::Span,
    ) -> Result<(inkwell::types::BasicTypeEnum<'ctx>, u64), String> {
        let te = self
            .channel_elem_types
            .get(&(call_span.offset, call_span.length))
            .cloned()
            .ok_or_else(|| {
                "channel op missing element type — typechecker `infer_channel_method` should \
                 populate `channel_elem_types` at every send/recv/try_recv site"
                    .to_string()
            })?;
        let elem_ty = self.llvm_type_for_type_expr(&te);
        let elem_size = self.ensure_target_data()?.get_store_size(&elem_ty);
        Ok((elem_ty, elem_size))
    }

    /// `tx.send(v)` → spill `v` to a stack slot and `karac_runtime_channel_send`
    /// its `elem_size` bytes onto the queue. Returns Unit.
    fn compile_channel_send(
        &mut self,
        object: &Expr,
        args: &[CallArg],
        call_span: &crate::token::Span,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        if args.len() != 1 {
            return Err("Sender.send expects exactly one argument".to_string());
        }
        let (elem_ty, elem_size) = self.channel_elem_ty_and_size(call_span)?;
        let fn_val = self.current_fn.unwrap();
        // Receiver first (source order), then the value.
        let ch = self.compile_expr(object)?.into_pointer_value();
        let arg_val = self.compile_expr(&args[0].value)?;
        let slot = self.create_entry_alloca(fn_val, "chan.send.val", elem_ty);
        self.builder.build_store(slot, arg_val).unwrap();

        let size_const = self.context.i64_type().const_int(elem_size, false);
        let send_fn = self
            .module
            .get_function("karac_runtime_channel_send")
            .expect("karac_runtime_channel_send declared in Codegen::new");
        self.builder
            .build_call(
                send_fn,
                &[ch.into(), slot.into(), size_const.into()],
                "chan.send",
            )
            .unwrap();
        // Unit return (the i64-zero unit value).
        Ok(self.context.i64_type().const_zero().into())
    }

    /// `rx.recv()` → `karac_runtime_channel_recv` into a stack slot, then load
    /// `T`. The presence discriminant is ignored: on an empty queue the
    /// runtime zero-fills the slot, which is the floor's "empty" answer for a
    /// `-> T` result (mirrors the interpreter's `unwrap_or(Unit)`).
    fn compile_channel_recv(
        &mut self,
        object: &Expr,
        call_span: &crate::token::Span,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let (elem_ty, elem_size) = self.channel_elem_ty_and_size(call_span)?;
        let fn_val = self.current_fn.unwrap();
        let ch = self.compile_expr(object)?.into_pointer_value();
        let out = self.create_entry_alloca(fn_val, "chan.recv.out", elem_ty);

        let size_const = self.context.i64_type().const_int(elem_size, false);
        let recv_fn = self
            .module
            .get_function("karac_runtime_channel_recv")
            .expect("karac_runtime_channel_recv declared in Codegen::new");
        self.builder
            .build_call(
                recv_fn,
                &[ch.into(), out.into(), size_const.into()],
                "chan.recv.found",
            )
            .unwrap();
        let val = self
            .builder
            .build_load(elem_ty, out, "chan.recv.val")
            .unwrap();
        Ok(val)
    }

    /// `rx.try_recv()` → `Option[T]`. Same FFI as `recv`, but the presence
    /// discriminant drives a `Some`/`None` build (the Map.get template).
    fn compile_channel_try_recv(
        &mut self,
        object: &Expr,
        call_span: &crate::token::Span,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let (elem_ty, elem_size) = self.channel_elem_ty_and_size(call_span)?;
        let fn_val = self.current_fn.unwrap();
        let ch = self.compile_expr(object)?.into_pointer_value();
        let out = self.create_entry_alloca(fn_val, "chan.tryrecv.out", elem_ty);

        let size_const = self.context.i64_type().const_int(elem_size, false);
        let recv_fn = self
            .module
            .get_function("karac_runtime_channel_recv")
            .expect("karac_runtime_channel_recv declared in Codegen::new");
        let found_i8 = self
            .builder
            .build_call(
                recv_fn,
                &[ch.into(), out.into(), size_const.into()],
                "chan.tryrecv.found",
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();
        let zero_i8 = self.context.i8_type().const_zero();
        let found = self
            .builder
            .build_int_compare(IntPredicate::NE, found_i8, zero_i8, "chan.tryrecv.some")
            .unwrap();

        let some_bb = self
            .context
            .append_basic_block(fn_val, "chan.tryrecv.some.bb");
        let none_bb = self
            .context
            .append_basic_block(fn_val, "chan.tryrecv.none.bb");
        let merge_bb = self
            .context
            .append_basic_block(fn_val, "chan.tryrecv.merge");
        self.builder
            .build_conditional_branch(found, some_bb, none_bb)
            .unwrap();

        // Some — load the value and split into Option's 3 payload words.
        self.builder.position_at_end(some_bb);
        let val = self
            .builder
            .build_load(elem_ty, out, "chan.tryrecv.val")
            .unwrap();
        let some_words = self.coerce_to_payload_words(val, 3)?;
        let some_end_bb = self.builder.get_insert_block().unwrap();
        self.builder.build_unconditional_branch(merge_bb).unwrap();

        // None.
        self.builder.position_at_end(none_bb);
        self.builder.build_unconditional_branch(merge_bb).unwrap();

        // Merge — phi the words and assemble the Option aggregate.
        self.builder.position_at_end(merge_bb);
        let agg =
            self.build_option_some_via_phis(&some_words, some_end_bb, none_bb, "chan.tryrecv");
        Ok(agg)
    }

    /// `tx.clone()` → a second handle to the same channel
    /// (`karac_runtime_channel_clone`: same pointer, refcount++). The new
    /// binding gets its own scope-exit `DropChannelEnd` like any channel end.
    fn compile_channel_clone(&mut self, object: &Expr) -> Result<BasicValueEnum<'ctx>, String> {
        let ch = self.compile_expr(object)?.into_pointer_value();
        let clone_fn = self
            .module
            .get_function("karac_runtime_channel_clone")
            .expect("karac_runtime_channel_clone declared in Codegen::new");
        let new = self
            .builder
            .build_call(clone_fn, &[ch.into()], "chan.clone")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic();
        Ok(new)
    }
}
