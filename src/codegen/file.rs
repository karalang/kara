//! File handle codegen — Phase 8 stdlib `File` slice F4.
//!
//! Lowers `File.open` / `.create` / `.append` constructors (returning
//! `Result[File, IoError]`) and `file.read` / `.write` / `.flush`
//! instance methods (returning `Result[usize, IoError]` and
//! `Result[Unit, IoError]`) to `karac_runtime_file_*` extern calls
//! declared at slice F3.
//!
//! ## KaracIoResult unpacking
//!
//! Every extern call returns a 32-byte `KaracIoResult` struct
//! (see `runtime/src/file.rs` for the layout). The shared helper
//! [`Codegen::lower_kara_io_result`] branches on `error_kind == 0`:
//!
//!   - **Ok arm.** Builds `Result.Ok(value)` where `value` is the
//!     handle pointer (for open-family), byte count (for read/write),
//!     or Unit (for flush). The Ok payload occupies Result field 1
//!     (the first payload word); fields 2–4 are zeroed.
//!
//!   - **Err arm.** Builds `Result.Err(IoError)`. The IoError variant
//!     tag is `error_kind - 1` (runtime tags shift by 1 to reserve 0
//!     for the OK sentinel). For `error_kind 1..=6` (NotFound through
//!     Interrupted) the IoError is a unit variant — all payload words
//!     stay zero. For `error_kind == 7` (Other), the runtime hands us
//!     an owned byte buffer in `error_msg_ptr` / `error_msg_len`; we
//!     stash it as the `String` aggregate `{ptr, len, cap}` (cap =
//!     len) into IoError's 3-word payload, which packs into Result
//!     fields 2/3/4.
//!
//! ## What this module does NOT cover (F4b)
//!
//! Scope-exit cleanup (`FreeFileHandle` CleanupAction +
//! `karac_runtime_file_close` emission). At F4 a `let f = File.open
//! (...); match f { Ok(h) => ... }` chain leaves the file descriptor
//! live until process exit if the user code doesn't manually close
//! it (no direct surface — close happens via Drop only). F4b adds
//! the cleanup action peer to `FreeMapHandle`, with `track_file_var`
//! firing at the pattern-binding site when the bound type is `File`.

use crate::ast::*;

use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum};
use inkwell::IntPredicate;

/// Discriminator for the Ok-arm payload shape. F4's three callsites
/// produce different Ok values from the same KaracIoResult `value`
/// field:
///
///   - `FileHandle` — `value` is the `*mut KaracFile` cast to i64;
///     stored into Result.Ok's payload as-is (it's already an i64
///     word).
///   - `ByteCount` — `value` is a non-negative byte count; stored as
///     i64.
///   - `Unit` — Ok-arm ignores `value`; the payload word is zero.
#[derive(Clone, Copy)]
pub(super) enum FileOkKind {
    FileHandle,
    ByteCount,
    Unit,
}

impl<'ctx> super::Codegen<'ctx> {
    /// Unpack a `KaracIoResult` stored in `slot` (an alloca address)
    /// into a Kāra `Result[T, IoError]` LLVM aggregate value. Shared
    /// by every File F4 dispatch arm (constructors + read/write/flush).
    ///
    /// The runtime extern call writes its KaracIoResult into the
    /// caller-allocated slot (out-param ABI; see F2/F3 design fork
    /// note). This helper loads the four fields back via GEPs and
    /// branches on `error_kind == 0` to build the Result aggregate.
    pub(super) fn lower_kara_io_result(
        &mut self,
        slot: inkwell::values::PointerValue<'ctx>,
        ok_kind: FileOkKind,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let i64_ty = self.context.i64_type();
        let i32_ty = self.context.i32_type();
        let zero_i64 = i64_ty.const_int(0, false);
        let zero_i32 = i32_ty.const_int(0, false);
        let io_ty = self.kara_io_result_type();

        // Load each KaracIoResult field via GEPs against the slot.
        // Field 2 (`_pad`) is skipped; codegen ignores padding.
        let value_ptr = self
            .builder
            .build_struct_gep(io_ty, slot, 0, "io.value.ptr")
            .unwrap();
        let value_i64 = self
            .builder
            .build_load(i64_ty, value_ptr, "io.value")
            .unwrap()
            .into_int_value();
        let kind_ptr = self
            .builder
            .build_struct_gep(io_ty, slot, 1, "io.kind.ptr")
            .unwrap();
        let error_kind = self
            .builder
            .build_load(i32_ty, kind_ptr, "io.kind")
            .unwrap()
            .into_int_value();
        let msg_ptr_ptr = self
            .builder
            .build_struct_gep(io_ty, slot, 3, "io.msg.ptr.ptr")
            .unwrap();
        let msg_ptr = self
            .builder
            .build_load(
                self.context.ptr_type(inkwell::AddressSpace::default()),
                msg_ptr_ptr,
                "io.msg.ptr",
            )
            .unwrap()
            .into_pointer_value();
        let msg_len_ptr = self
            .builder
            .build_struct_gep(io_ty, slot, 4, "io.msg.len.ptr")
            .unwrap();
        let msg_len = self
            .builder
            .build_load(i64_ty, msg_len_ptr, "io.msg.len")
            .unwrap()
            .into_int_value();

        // Look up the Result enum layout. Result is hand-seeded at
        // module init with a 4-payload-word shape, so the IoError
        // value (also 4 words including its tag) packs in directly.
        let result_layout = self
            .enum_layouts
            .get("Result")
            .ok_or_else(|| "Result layout not registered before File codegen".to_string())?;
        let result_ty = result_layout.llvm_type;
        let total_fields = result_ty.count_fields() as u64;

        let fn_val = self
            .current_fn
            .ok_or_else(|| "File codegen called outside fn".to_string())?;
        let result_slot = self.create_entry_alloca(fn_val, "file.result", result_ty.into());

        let is_ok = self
            .builder
            .build_int_compare(IntPredicate::EQ, error_kind, zero_i32, "io.is_ok")
            .unwrap();
        let ok_bb = self.context.append_basic_block(fn_val, "file.ok");
        let err_bb = self.context.append_basic_block(fn_val, "file.err");
        let cont_bb = self.context.append_basic_block(fn_val, "file.cont");
        self.builder
            .build_conditional_branch(is_ok, ok_bb, err_bb)
            .unwrap();

        // ── Ok arm ────────────────────────────────────────────────
        // Result.Ok tag = 1 (matches the seeded `Ok=1` layout in
        // `seed_builtin_enum_layouts`).
        self.builder.position_at_end(ok_bb);
        let ok_tag = i64_ty.const_int(1, false);
        let tag_ptr_ok = self
            .builder
            .build_struct_gep(result_ty, result_slot, 0, "ok.tag")
            .unwrap();
        self.builder.build_store(tag_ptr_ok, ok_tag).unwrap();

        // Ok payload word 0 (Result field 1) — shape depends on `ok_kind`.
        let ok_payload_w0 = match ok_kind {
            FileOkKind::FileHandle | FileOkKind::ByteCount => value_i64,
            FileOkKind::Unit => zero_i64,
        };
        if total_fields > 1 {
            let p1 = self
                .builder
                .build_struct_gep(result_ty, result_slot, 1, "ok.w0")
                .unwrap();
            self.builder.build_store(p1, ok_payload_w0).unwrap();
        }
        // Zero the remaining payload words.
        for w in 2..total_fields {
            let elem_ptr = self
                .builder
                .build_struct_gep(result_ty, result_slot, w as u32, &format!("ok.w{w}"))
                .unwrap();
            self.builder.build_store(elem_ptr, zero_i64).unwrap();
        }
        self.builder.build_unconditional_branch(cont_bb).unwrap();

        // ── Err arm ───────────────────────────────────────────────
        // Result.Err tag = 0. IoError variant tag = error_kind - 1
        // (runtime tags shift by 1 to reserve 0 for OK).
        self.builder.position_at_end(err_bb);
        let err_tag = zero_i64;
        let tag_ptr_err = self
            .builder
            .build_struct_gep(result_ty, result_slot, 0, "err.tag")
            .unwrap();
        self.builder.build_store(tag_ptr_err, err_tag).unwrap();

        // IoError tag (Result field 1) = (error_kind as i64) - 1.
        let err_kind_i64 = self
            .builder
            .build_int_z_extend(error_kind, i64_ty, "io.kind.i64")
            .unwrap();
        let one_i64 = i64_ty.const_int(1, false);
        let io_tag = self
            .builder
            .build_int_sub(err_kind_i64, one_i64, "io.variant.tag")
            .unwrap();
        if total_fields > 1 {
            let p1 = self
                .builder
                .build_struct_gep(result_ty, result_slot, 1, "err.io.tag")
                .unwrap();
            self.builder.build_store(p1, io_tag).unwrap();
        }

        // IoError payload words (Result fields 2/3/4) carry the
        // String aggregate for `Other(String)`; zero otherwise. Branch
        // on whether `msg_ptr` is non-null — non-null only when
        // error_kind == 7 (Other). Storing zero for the other
        // variants keeps the unit-variant invariant (all-zero payload).
        let msg_ptr_int = self
            .builder
            .build_ptr_to_int(msg_ptr, i64_ty, "err.msg.ptr.i64")
            .unwrap();
        if total_fields > 2 {
            let p2 = self
                .builder
                .build_struct_gep(result_ty, result_slot, 2, "err.io.w0")
                .unwrap();
            self.builder.build_store(p2, msg_ptr_int).unwrap();
        }
        if total_fields > 3 {
            let p3 = self
                .builder
                .build_struct_gep(result_ty, result_slot, 3, "err.io.w1")
                .unwrap();
            self.builder.build_store(p3, msg_len).unwrap();
        }
        if total_fields > 4 {
            // cap = len (the runtime allocated exactly len bytes).
            let p4 = self
                .builder
                .build_struct_gep(result_ty, result_slot, 4, "err.io.w2")
                .unwrap();
            self.builder.build_store(p4, msg_len).unwrap();
        }
        self.builder.build_unconditional_branch(cont_bb).unwrap();

        // ── Cont ──────────────────────────────────────────────────
        self.builder.position_at_end(cont_bb);
        let result = self
            .builder
            .build_load(result_ty, result_slot, "file.result.val")
            .unwrap();
        Ok(result)
    }

    /// Allocate a fresh `KaracIoResult` slot in the function entry
    /// block. Caller uses the returned pointer as the `out` first-arg
    /// when calling a `karac_runtime_file_*` extern, then passes the
    /// same pointer to `lower_kara_io_result` to load + unpack the
    /// result. Allocating in the entry block avoids stack-growth-in-a-
    /// loop pathology (mirrors the convention `create_entry_alloca`
    /// follows everywhere).
    fn alloca_io_result_slot(&mut self) -> Result<inkwell::values::PointerValue<'ctx>, String> {
        let fn_val = self
            .current_fn
            .ok_or_else(|| "File codegen called outside fn".to_string())?;
        let io_ty = self.kara_io_result_type();
        Ok(self.create_entry_alloca(fn_val, "file.io.slot", io_ty.into()))
    }

    /// Compile `File.open(path)` / `.create(path)` / `.append(path)`.
    /// `runtime_sym` selects the corresponding extern; the result
    /// shape is the same `Result[File, IoError]` for all three.
    pub(super) fn compile_file_constructor(
        &mut self,
        runtime_sym: &str,
        path_arg: &Expr,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let path_val = self.compile_expr(path_arg)?;
        let path_sv = path_val.into_struct_value();
        let path_ptr = self
            .builder
            .build_extract_value(path_sv, 0, "path.ptr")
            .unwrap()
            .into_pointer_value();
        let path_len = self
            .builder
            .build_extract_value(path_sv, 1, "path.len")
            .unwrap()
            .into_int_value();
        let slot = self.alloca_io_result_slot()?;
        let f = self
            .module
            .get_function(runtime_sym)
            .unwrap_or_else(|| panic!("{runtime_sym} declared in Codegen::new"));
        self.builder
            .build_call(
                f,
                &[
                    BasicMetadataValueEnum::PointerValue(slot),
                    BasicMetadataValueEnum::PointerValue(path_ptr),
                    BasicMetadataValueEnum::IntValue(path_len),
                ],
                "file.ctor.call",
            )
            .unwrap();
        self.lower_kara_io_result(slot, FileOkKind::FileHandle)
    }

    /// Compile `file.read(buf)` — reads up to `buf.len()` bytes into
    /// `buf`'s backing storage. Returns `Result[usize, IoError]` with
    /// the byte count (0 = clean EOF).
    pub(super) fn compile_file_read(
        &mut self,
        self_val: BasicValueEnum<'ctx>,
        buf_val: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let handle = self_val.into_pointer_value();
        let buf_sv = buf_val.into_struct_value();
        let buf_ptr = self
            .builder
            .build_extract_value(buf_sv, 0, "buf.ptr")
            .unwrap()
            .into_pointer_value();
        let buf_len = self
            .builder
            .build_extract_value(buf_sv, 1, "buf.len")
            .unwrap()
            .into_int_value();
        let slot = self.alloca_io_result_slot()?;
        let f = self
            .module
            .get_function("karac_runtime_file_read")
            .expect("karac_runtime_file_read declared in Codegen::new");
        self.builder
            .build_call(
                f,
                &[
                    BasicMetadataValueEnum::PointerValue(slot),
                    BasicMetadataValueEnum::PointerValue(handle),
                    BasicMetadataValueEnum::PointerValue(buf_ptr),
                    BasicMetadataValueEnum::IntValue(buf_len),
                ],
                "file.read.call",
            )
            .unwrap();
        self.lower_kara_io_result(slot, FileOkKind::ByteCount)
    }

    /// Compile `file.write(buf)` — writes `buf.len()` bytes from
    /// `buf`'s backing storage. Returns `Result[usize, IoError]`.
    pub(super) fn compile_file_write(
        &mut self,
        self_val: BasicValueEnum<'ctx>,
        buf_val: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let handle = self_val.into_pointer_value();
        let buf_sv = buf_val.into_struct_value();
        let buf_ptr = self
            .builder
            .build_extract_value(buf_sv, 0, "buf.ptr")
            .unwrap()
            .into_pointer_value();
        let buf_len = self
            .builder
            .build_extract_value(buf_sv, 1, "buf.len")
            .unwrap()
            .into_int_value();
        let slot = self.alloca_io_result_slot()?;
        let f = self
            .module
            .get_function("karac_runtime_file_write")
            .expect("karac_runtime_file_write declared in Codegen::new");
        self.builder
            .build_call(
                f,
                &[
                    BasicMetadataValueEnum::PointerValue(slot),
                    BasicMetadataValueEnum::PointerValue(handle),
                    BasicMetadataValueEnum::PointerValue(buf_ptr),
                    BasicMetadataValueEnum::IntValue(buf_len),
                ],
                "file.write.call",
            )
            .unwrap();
        self.lower_kara_io_result(slot, FileOkKind::ByteCount)
    }

    /// Compile `file.flush()` — flushes the file's write buffer.
    /// Returns `Result[Unit, IoError]`.
    pub(super) fn compile_file_flush(
        &mut self,
        self_val: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let handle = self_val.into_pointer_value();
        let slot = self.alloca_io_result_slot()?;
        let f = self
            .module
            .get_function("karac_runtime_file_flush")
            .expect("karac_runtime_file_flush declared in Codegen::new");
        self.builder
            .build_call(
                f,
                &[
                    BasicMetadataValueEnum::PointerValue(slot),
                    BasicMetadataValueEnum::PointerValue(handle),
                ],
                "file.flush.call",
            )
            .unwrap();
        self.lower_kara_io_result(slot, FileOkKind::Unit)
    }
}
