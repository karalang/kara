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
}
