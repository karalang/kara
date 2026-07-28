//! Arrow IPC codegen lowering — the `to_arrow_ipc()` surface for `Column`,
//! `DataFrame`, and `Tensor` (phase-11 Arrow IPC codegen twin).
//!
//! Serialization itself lives in the runtime (`runtime/src/arrow_ipc.rs`),
//! behind the opt-in `arrow` feature that produces `libkarac_runtime_arrow.a`;
//! `karac` auto-selects that archive only when the emitted object references a
//! `karac_arrow_*` symbol (`driver.rs § SpecialArchive::Arrow`), so a program
//! that never touches Arrow doesn't carry the arrow-rs dep.
//!
//! Codegen's job is therefore small and uniform across the three receivers:
//! hand the runtime the receiver's control block (plus, where the block isn't
//! self-describing, the element size / kind), then **adopt the returned
//! malloc'd buffer as an owned `Vec[u8]`**. The runtime allocates with
//! `max(len, 1)` so an empty stream is still a unique freeable pointer, and
//! the adoption sets `cap` to match — the `karac_regex_replace_all`
//! convention. From that point the buffer is an ordinary owned Vec and the
//! existing cleanup machinery frees it; nothing Arrow-specific is tracked.
//!
//! Byte-identity with the interpreter is the contract these lowerings serve;
//! the rules that make it hold are documented at `runtime/src/arrow_ipc.rs`'s
//! module header, and `tests/codegen.rs` asserts it end-to-end against an
//! in-process interpreter run.

use inkwell::values::{BasicValueEnum, IntValue, PointerValue};
use inkwell::IntPredicate;

use crate::ast::{CallArg, ExprKind};

impl<'ctx> super::Codegen<'ctx> {
    /// Call a `karac_arrow_*_to_ipc` entrypoint and adopt its malloc'd result
    /// as an owned `Vec[u8]`. `args` are the entrypoint's leading arguments;
    /// the `out_len` slot is appended here, since every entrypoint takes it
    /// last.
    fn arrow_call_and_adopt(
        &mut self,
        fn_name: &str,
        args: &[BasicValueEnum<'ctx>],
        label: &str,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let i64_t = self.context.i64_type();
        let fn_val = self.current_fn.unwrap();
        let len_slot = self.create_entry_alloca(fn_val, &format!("{label}.len"), i64_t.into());
        self.builder
            .build_store(len_slot, i64_t.const_zero())
            .unwrap();

        let callee = self
            .module
            .get_function(fn_name)
            .unwrap_or_else(|| panic!("{fn_name} declared in Codegen::new"));
        let mut call_args: Vec<inkwell::values::BasicMetadataValueEnum<'ctx>> =
            args.iter().map(|a| (*a).into()).collect();
        call_args.push(len_slot.into());
        let ptr = self
            .builder
            .build_call(callee, &call_args, &format!("{label}.ptr"))
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        let len = self
            .builder
            .build_load(i64_t, len_slot, &format!("{label}.len.v"))
            .unwrap()
            .into_int_value();
        // cap = max(len, 1) — the runtime allocated max(len, 1) bytes.
        let len_pos = self
            .builder
            .build_int_compare(
                IntPredicate::UGT,
                len,
                i64_t.const_zero(),
                &format!("{label}.pos"),
            )
            .unwrap();
        let cap: IntValue<'ctx> = self
            .builder
            .build_select(
                len_pos,
                len,
                i64_t.const_int(1, false),
                &format!("{label}.cap"),
            )
            .unwrap()
            .into_int_value();
        Ok(self.build_vec_value(ptr, len, cap))
    }

    /// `col.to_arrow_ipc() -> Vec[u8]`. `elem_size` + `kind` travel alongside
    /// the control block because a bare Column control block, unlike a
    /// DataFrame entry, carries no element tag — `kind` uses the same encoding
    /// as the DataFrame entry field, so the runtime's decode table is shared.
    pub(super) fn compile_arrow_column_to_ipc(
        &mut self,
        control: PointerValue<'ctx>,
        elem_size: u64,
        kind: u64,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let i64_t = self.context.i64_type();
        self.arrow_call_and_adopt(
            "karac_arrow_column_to_ipc",
            &[
                control.into(),
                i64_t.const_int(elem_size, false).into(),
                i64_t.const_int(kind, false).into(),
            ],
            "arrow.col",
        )
    }

    /// `df.to_arrow_ipc() -> Vec[u8]`. Nothing but the control block crosses:
    /// each stride-40 entry already carries its own `elem_size` / `kind`.
    pub(super) fn compile_arrow_dataframe_to_ipc(
        &mut self,
        control: PointerValue<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        self.arrow_call_and_adopt(
            "karac_arrow_dataframe_to_ipc",
            &[control.into()],
            "arrow.df",
        )
    }

    /// `t.to_arrow_ipc() -> Vec[u8]`. Rank and dims come from the tensor
    /// block's own `[rank][dims][data]` header, which is authoritative at
    /// runtime, so only the element description crosses — this works
    /// uniformly for static, `?`-bearing, and splice-generic receivers, like
    /// `shape()` / `rank()`.
    pub(super) fn compile_arrow_tensor_to_ipc(
        &mut self,
        t_ptr: PointerValue<'ctx>,
        elem_size: u64,
        kind: u64,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let i64_t = self.context.i64_type();
        self.arrow_call_and_adopt(
            "karac_arrow_tensor_to_ipc",
            &[
                t_ptr.into(),
                i64_t.const_int(elem_size, false).into(),
                i64_t.const_int(kind, false).into(),
            ],
            "arrow.t",
        )
    }

    // ── Read direction (`from_arrow_ipc`) ────────────────────────────

    /// Compile the `bytes: Vec[u8]` argument of a `from_arrow_ipc` call to its
    /// `(data, len)` parts, plus the capacity of a TEMPORARY argument that the
    /// caller must free — see `arrow_free_temp_bytes`.
    ///
    /// The runtime only READS the buffer — it builds a fresh control-block
    /// graph and never adopts the input — so ownership follows the
    /// `Tensor.zeros(dims)` rule: an identifier argument is left to its own
    /// scope cleanup, while a temporary (`Column.from_arrow_ipc(c.to_arrow_ipc())`,
    /// the round-trip shape this surface exists for) has no other owner and
    /// must be freed at this call. Getting that backwards is a leak on one
    /// side and a double-free on the other.
    ///
    /// The capacity comes back rather than the free being emitted here because
    /// the free must land AFTER the runtime call reads the buffer. Emitting it
    /// at extraction time hands the runtime a dangling pointer — which is
    /// exactly the failure this signature exists to make unrepresentable.
    fn arrow_bytes_parts(
        &mut self,
        args: &[CallArg],
        what: &str,
    ) -> Result<(PointerValue<'ctx>, IntValue<'ctx>, Option<IntValue<'ctx>>), String> {
        let arg = args
            .first()
            .map(|a| &a.value)
            .ok_or_else(|| format!("{what}: missing bytes argument"))?;
        let v = self.compile_expr(arg)?;
        let s = v.into_struct_value();
        let data = self
            .builder
            .build_extract_value(s, 0, "arrow.in.data")
            .unwrap()
            .into_pointer_value();
        let len = self
            .builder
            .build_extract_value(s, 1, "arrow.in.len")
            .unwrap()
            .into_int_value();
        let temp_cap = (!matches!(arg.kind, ExprKind::Identifier(_))).then(|| {
            self.builder
                .build_extract_value(s, 2, "arrow.in.cap")
                .unwrap()
                .into_int_value()
        });
        Ok((data, len, temp_cap))
    }

    /// Free a temporary `bytes` argument once the runtime call has consumed
    /// it. Must be emitted after the call and before the null guard — the
    /// guard's failure arm is `unreachable`, so a free placed after it would
    /// be skipped on the path that still reaches the join.
    fn arrow_free_temp_bytes(
        &mut self,
        data: PointerValue<'ctx>,
        temp_cap: Option<IntValue<'ctx>>,
    ) {
        if let Some(cap) = temp_cap {
            // `Vec[u8]` — element ABI size 1.
            self.emit_free_if_cap_positive(data, cap, 1);
        }
    }

    /// Guard a `from_arrow_ipc` result: the runtime returns NULL for a
    /// malformed stream, an unsupported element type, or values that do not
    /// convert to the declared type, and never a partially-built graph.
    ///
    /// The message is static because codegen panics carry compile-time
    /// constants only (`emit_panic`) — the same tradeoff `Regex.compile`'s
    /// AOT Err message makes. The panic's source location is what identifies
    /// *which* call failed.
    fn arrow_guard_non_null(
        &mut self,
        ptr: PointerValue<'ctx>,
        message: &str,
    ) -> Result<(), String> {
        let ok = self
            .builder
            .build_int_compare(
                IntPredicate::NE,
                self.builder
                    .build_ptr_to_int(ptr, self.context.i64_type(), "arrow.rd.pi")
                    .unwrap(),
                self.context.i64_type().const_zero(),
                "arrow.rd.ok",
            )
            .unwrap();
        self.emit_tensor_guard(ok, message)
    }

    /// `Column.from_arrow_ipc(bytes) -> Column[T]`. The runtime builds the
    /// whole control block at the call site's declared `(elem_size, kind)` —
    /// codegen contributes only the type description, since a `Column`'s
    /// element type lives in the Kāra type and not in the stream.
    pub(super) fn compile_arrow_column_from_ipc(
        &mut self,
        args: &[CallArg],
        elem_size: u64,
        kind: u64,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let i64_t = self.context.i64_type();
        let (data, len, temp_cap) = self.arrow_bytes_parts(args, "Column.from_arrow_ipc")?;
        let callee = self
            .module
            .get_function("karac_arrow_column_from_ipc")
            .expect("karac_arrow_column_from_ipc declared in Codegen::new");
        let ctrl = self
            .builder
            .build_call(
                callee,
                &[
                    data.into(),
                    len.into(),
                    i64_t.const_int(elem_size, false).into(),
                    i64_t.const_int(kind, false).into(),
                ],
                "arrow.col.rd",
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        self.arrow_free_temp_bytes(data, temp_cap);
        self.arrow_guard_non_null(
            ctrl,
            "Column.from_arrow_ipc: the byte buffer is not a readable Arrow IPC \
             stream, or its column's element type does not convert to the \
             column's declared type (String and bool convert only to themselves)",
        )?;
        Ok(ctrl.into())
    }

    /// `DataFrame.from_arrow_ipc(bytes) -> DataFrame`. Nothing but the buffer
    /// crosses: a `DataFrame` is not generic, so each column's representation
    /// is derived from its Arrow type on the runtime side — which is also why
    /// this leg can only fail on a malformed stream or an unsupported field
    /// type, never on a conversion.
    pub(super) fn compile_arrow_dataframe_from_ipc(
        &mut self,
        args: &[CallArg],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let (data, len, temp_cap) = self.arrow_bytes_parts(args, "DataFrame.from_arrow_ipc")?;
        let callee = self
            .module
            .get_function("karac_arrow_dataframe_from_ipc")
            .expect("karac_arrow_dataframe_from_ipc declared in Codegen::new");
        let ctrl = self
            .builder
            .build_call(callee, &[data.into(), len.into()], "arrow.df.rd")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        self.arrow_free_temp_bytes(data, temp_cap);
        self.arrow_guard_non_null(
            ctrl,
            "DataFrame.from_arrow_ipc: the byte buffer is not a readable Arrow IPC \
             stream, or a field's element type is outside the supported set \
             (Int64/Int32, Float64/Float32, Utf8/LargeUtf8, Boolean)",
        )?;
        Ok(ctrl.into())
    }
}
