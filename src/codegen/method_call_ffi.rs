//! C-string interop (`CStr` / `CString` / UTF-8 validation) and the
//! ambient-resource method lowerings (Env / Clock / RandomSource / std
//! streams) with their FFI dispatch.
//!
//! Extracted verbatim from `method_call.rs` (structural-debt second-level
//! split). Sibling `impl<'ctx> super::Codegen<'ctx>` block; moved methods
//! are `pub(super)`.

use super::method_call::*;
use crate::ast::*;

use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum};
use inkwell::AddressSpace;
use inkwell::IntPredicate;

impl<'ctx> super::Codegen<'ctx> {
    /// Lower a `CStr` borrowed-surface method (design.md § C-String
    /// Literals). The receiver value is the `{ptr, i64}` slice-struct the
    /// `CStringLit` lowering in `compile_expr` produces: field 0 is the
    /// NUL-terminated rodata pointer, field 1 the source byte count
    /// (excluding the NUL). `as_ptr` is the language's first safe
    /// pointer-producer — it hands out field 0 directly (the FFI/host-fn
    /// handoff per the design's `puts(msg.as_ptr())` example). `as_bytes`
    /// returns the receiver aggregate unchanged: `Slice[u8]` shares the
    /// exact `{ptr, i64}` layout and the NUL stays invisible because the
    /// recorded len excludes it. Args are validated empty by the
    /// typechecker (`infer_cstr_method`), so they're not threaded here.
    pub(super) fn compile_cstr_method(
        &mut self,
        object: &Expr,
        method: &str,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let recv = self.compile_expr(object)?;
        let agg = recv.into_struct_value();
        match method {
            "as_ptr" => Ok(self
                .builder
                .build_extract_value(agg, 0, "cstr.as_ptr")
                .unwrap()),
            "len" => Ok(self
                .builder
                .build_extract_value(agg, 1, "cstr.len")
                .unwrap()),
            "is_empty" => {
                let len = self
                    .builder
                    .build_extract_value(agg, 1, "cstr.len")
                    .unwrap()
                    .into_int_value();
                let zero = self.context.i64_type().const_zero();
                Ok(self
                    .builder
                    .build_int_compare(IntPredicate::EQ, len, zero, "cstr.is_empty")
                    .unwrap()
                    .into())
            }
            "as_bytes" => Ok(recv),
            _ => Err(format!(
                "codegen: no handler for CStr method '{}' (typechecker admits \
                 as_ptr/len/is_empty/as_bytes only — this is a codegen bug)",
                method
            )),
        }
    }

    /// Lower a `CString` borrowed-surface method (design.md § C-String
    /// Literals, "Owning `CString`"). The receiver is the `{ptr, len, cap}`
    /// String-shaped aggregate `to_cstring` produced (field 0 the
    /// NUL-terminated heap pointer, field 1 the source byte count excluding the
    /// NUL, field 2 the capacity `len + 1`). `as_ptr` hands out field 0 (the
    /// FFI handoff); `len` / `is_empty` read field 1. Unlike `CStr.as_bytes`
    /// (whose receiver *is* a 2-word `{ptr, i64}` slice, returned unchanged),
    /// `CString.as_bytes` rebuilds a fresh `Slice[u8]` `{ptr, len}` from fields
    /// 0/1 — the 3-word owning aggregate is not itself slice-shaped. Args are
    /// validated empty by `infer_cstring_method`.
    pub(super) fn compile_cstring_method(
        &mut self,
        object: &Expr,
        method: &str,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let recv = self.compile_expr(object)?;
        let agg = recv.into_struct_value();
        match method {
            "as_ptr" => Ok(self
                .builder
                .build_extract_value(agg, 0, "cstring.as_ptr")
                .unwrap()),
            "len" => Ok(self
                .builder
                .build_extract_value(agg, 1, "cstring.len")
                .unwrap()),
            "is_empty" => {
                let len = self
                    .builder
                    .build_extract_value(agg, 1, "cstring.len")
                    .unwrap()
                    .into_int_value();
                let zero = self.context.i64_type().const_zero();
                Ok(self
                    .builder
                    .build_int_compare(IntPredicate::EQ, len, zero, "cstring.is_empty")
                    .unwrap()
                    .into())
            }
            "as_bytes" => {
                let data = self
                    .builder
                    .build_extract_value(agg, 0, "cstring.ab.ptr")
                    .unwrap();
                let len = self
                    .builder
                    .build_extract_value(agg, 1, "cstring.ab.len")
                    .unwrap();
                let slice_ty = self.slice_struct_type();
                let with_ptr = self
                    .builder
                    .build_insert_value(slice_ty.get_undef(), data, 0, "cstring.ab.p")
                    .unwrap();
                let slice = self
                    .builder
                    .build_insert_value(with_ptr, len, 1, "cstring.ab.l")
                    .unwrap();
                Ok(slice.into_struct_value().into())
            }
            _ => Err(format!(
                "codegen: no handler for CString method '{}' (typechecker admits \
                 as_ptr/len/is_empty/as_bytes only — this is a codegen bug)",
                method
            )),
        }
    }

    /// Lower `String.to_cstring(ref self) -> Result[CString, NulError]`
    /// (design.md § C-String Literals). The receiver `{ptr, len, cap}` is only
    /// READ (its bytes are copied into a fresh NUL-terminated buffer), so the
    /// caller's `String` keeps its own scope-exit drop — no ownership transfer,
    /// mirroring `CStr.to_string`. The runtime extern
    /// `karac_runtime_string_to_cstring` scans for an interior NUL and either
    /// writes an owning `CString` (`{ptr, len, cap=len+1}`) into an out-slot and
    /// returns `true`, or returns `false` (interior NUL found). Codegen owns the
    /// enum-tag assignment: `Result.Ok(CString)` on success, else
    /// `Result.Err(NulError.InteriorNul)`. Structural twin of
    /// `build_utf8_validated_result`.
    pub(super) fn compile_string_to_cstring(
        &mut self,
        object: &Expr,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let recv = self.compile_expr(object)?;
        let agg = recv.into_struct_value();
        let data_ptr = self
            .builder
            .build_extract_value(agg, 0, "tocstr.ptr")
            .unwrap()
            .into_pointer_value();
        let data_len = self
            .builder
            .build_extract_value(agg, 1, "tocstr.len")
            .unwrap()
            .into_int_value();

        let cstring_ty = self.vec_struct_type();

        let fn_val = self
            .current_fn
            .ok_or_else(|| "codegen: String.to_cstring called outside a function".to_string())?;
        let out_cstr = self.create_entry_alloca(fn_val, "tocstr.out", cstring_ty.into());

        let f = self
            .module
            .get_function("karac_runtime_string_to_cstring")
            .expect("karac_runtime_string_to_cstring declared in Codegen::new");
        let ok = self
            .builder
            .build_call(
                f,
                &[data_ptr.into(), data_len.into(), out_cstr.into()],
                "tocstr.ok",
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();

        // Result llvm-type copied out before any `&mut self` enum builder call.
        let result_ty = self
            .type_decls
            .enum_layouts
            .get("Result")
            .map(|l| l.llvm_type)
            .ok_or_else(|| "codegen: Result enum layout missing (codegen bug)".to_string())?;

        let ok_bb = self.context.append_basic_block(fn_val, "tocstr.okbb");
        let err_bb = self.context.append_basic_block(fn_val, "tocstr.errbb");
        let merge_bb = self.context.append_basic_block(fn_val, "tocstr.merge");
        self.builder
            .build_conditional_branch(ok, ok_bb, err_bb)
            .unwrap();

        // Ok arm: Result.Ok(<owning CString the runtime wrote into out_cstr>).
        // The Result payload words reinterpret the 3-word CString inline, exactly
        // as the `Result[String, Utf8Error]` Ok arm reinterprets a String.
        self.builder.position_at_end(ok_bb);
        let cstr_val = self
            .builder
            .build_load(cstring_ty, out_cstr, "tocstr.load")
            .unwrap();
        let ok_val = self.build_nonshared_enum_value("Result", "Ok", &[cstr_val])?;
        self.builder.build_unconditional_branch(merge_bb).unwrap();
        let ok_end = self.builder.get_insert_block().unwrap();

        // Err arm: Result.Err(NulError.InteriorNul) — the only failure the
        // runtime signals (`ok == false` ⇔ interior NUL).
        self.builder.position_at_end(err_bb);
        let nul_err = self.build_nonshared_enum_value("NulError", "InteriorNul", &[])?;
        let err_val = self.build_nonshared_enum_value("Result", "Err", &[nul_err])?;
        self.builder.build_unconditional_branch(merge_bb).unwrap();
        let err_end = self.builder.get_insert_block().unwrap();

        self.builder.position_at_end(merge_bb);
        let phi = self.builder.build_phi(result_ty, "tocstr.result").unwrap();
        phi.add_incoming(&[(&ok_val, ok_end), (&err_val, err_end)]);
        Ok(phi.as_basic_value())
    }

    /// Lower `CStr.to_string() -> Result[String, Utf8Error]` (phase-12
    /// Cluster 2). The receiver is the `{ptr, i64}` slice-struct (field 0 the
    /// NUL-terminated bytes, field 1 the source length). The runtime extern
    /// `karac_runtime_cstr_to_string` validates UTF-8 and either writes a heap
    /// `String` (`{ptr,len,cap}`) into an out-slot and returns `true`, or
    /// writes the `Utf8Error` variant discriminant (0 = InvalidByte,
    /// 1 = IncompleteSequence) into a second out-slot and returns `false`.
    /// Codegen owns the enum-tag assignment: it builds `Result.Ok(String)` on
    /// success and, on failure, *selects* the `Utf8Error` variant tag from the
    /// runtime discriminant before wrapping it in `Result.Err`. Structural twin
    /// of the `env.var -> Result[String, VarError]` lowering above.
    pub(super) fn compile_cstr_to_string(
        &mut self,
        object: &Expr,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let recv = self.compile_expr(object)?;
        let agg = recv.into_struct_value();
        let data_ptr = self
            .builder
            .build_extract_value(agg, 0, "cstr.ts.ptr")
            .unwrap()
            .into_pointer_value();
        let data_len = self
            .builder
            .build_extract_value(agg, 1, "cstr.ts.len")
            .unwrap()
            .into_int_value();
        self.build_utf8_validated_result(data_ptr, data_len)
    }

    /// `String.from_utf8(bytes: Vec[u8]) -> Result[String, Utf8Error]` — the
    /// UTF-8-validating String constructor (interpreter parity in
    /// `eval_call.rs`). Extracts the input `Vec`'s `{data, len}` (fields 0/1 of
    /// the `{data, len, cap}` aggregate) and delegates to the shared
    /// `build_utf8_validated_result`. The bytes are validated and COPIED into a
    /// fresh heap String (the consume-by-copy convention `Vec.push(param)`
    /// uses), so the input `Vec`'s own scope-exit drop frees its buffer — no
    /// move/ownership transfer needed. Was interpreter-only (B-2026-06-18-11);
    /// this wires the codegen path so `match String.from_utf8(v) { Ok(s) => …,
    /// Err(_) => … }` builds (the Relay slice-4 request-line parse).
    pub(super) fn compile_string_from_utf8(
        &mut self,
        arg: &Expr,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let vec_val = self.compile_expr(arg)?;
        let agg = vec_val.into_struct_value();
        let data_ptr = self
            .builder
            .build_extract_value(agg, 0, "fu8.data")
            .unwrap()
            .into_pointer_value();
        let data_len = self
            .builder
            .build_extract_value(agg, 1, "fu8.len")
            .unwrap()
            .into_int_value();
        // B-2026-08-14-20 — a FRESH OWNED `Vec[u8]` TEMPORARY argument
        // (`String.from_utf8(mk_bytes())`, `String.from_utf8(s.bytes().to_vec())`)
        // has no binding to carry a scope-exit drop, and this path only reads
        // the range — `karac_runtime_cstr_to_string` copies the bytes into a
        // fresh String — so nothing ever freed the argument's buffer. Track it
        // at the enclosing frame's exit, after the pointer and length are
        // already extracted.
        //
        // Both gates are load-bearing. The SHAPE gate keeps the borrow-shaped
        // arguments out: `s.bytes()` and `v.as_slice()` are 2-field `{ptr,len}`
        // views into a buffer somebody else owns, and freeing one would take
        // the source's storage with it. The FRESH-TEMP gate keeps NAMED
        // arguments out: `String.from_utf8(v)` must leave `v` its own drop.
        // (`FreeVecBuffer`'s `cap > 0` guard is a third backstop, for a
        // Vec-shaped borrow that reports itself owned.)
        if self.llvm_ty_is_vec_struct(vec_val.get_type()) && self.expr_yields_fresh_owned_temp(arg)
        {
            let fn_val = self
                .current_fn
                .ok_or_else(|| "codegen: String.from_utf8 called outside a function".to_string())?;
            let slot = self.create_entry_alloca(fn_val, "fu8.tmp", vec_val.get_type());
            self.builder.build_store(slot, vec_val).unwrap();
            self.track_vec_var(slot, Some(self.context.i8_type().into()));
        }
        self.build_utf8_validated_result(data_ptr, data_len)
    }

    /// Shared core of `CStr.to_string()` and `String.from_utf8(Vec[u8])`:
    /// given a `(data_ptr, data_len)` byte range, validate UTF-8 via
    /// `karac_runtime_cstr_to_string` (which COPIES the bytes into a fresh heap
    /// String on success) and build `Result[String, Utf8Error]` — `Ok(String)`
    /// on valid UTF-8, else `Err(Utf8Error.{InvalidByte | IncompleteSequence})`
    /// selected from the runtime discriminant. The range is only READ (the
    /// runtime copies), so the caller's source buffer keeps its own scope-exit
    /// drop — no ownership transfer.
    pub(super) fn build_utf8_validated_result(
        &mut self,
        data_ptr: inkwell::values::PointerValue<'ctx>,
        data_len: inkwell::values::IntValue<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let i64_t = self.context.i64_type();
        let i8_t = self.context.i8_type();
        let str_ty = self.vec_struct_type();

        let fn_val = self
            .current_fn
            .ok_or_else(|| "codegen: CStr.to_string called outside a function".to_string())?;
        let out_str = self.create_entry_alloca(fn_val, "cstr.ts.outstr", str_ty.into());
        let out_err = self.create_entry_alloca(fn_val, "cstr.ts.outerr", i8_t.into());

        let f = self
            .module
            .get_function("karac_runtime_cstr_to_string")
            .expect("karac_runtime_cstr_to_string declared in Codegen::new");
        let ok = self
            .builder
            .build_call(
                f,
                &[
                    data_ptr.into(),
                    data_len.into(),
                    out_str.into(),
                    out_err.into(),
                ],
                "cstr.ts.ok",
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();

        // Copy out the Result llvm-type and the two Utf8Error variant tags
        // before any `&mut self` call (drops the `enum_layouts` borrow).
        let result_ty = self
            .type_decls
            .enum_layouts
            .get("Result")
            .map(|l| l.llvm_type)
            .ok_or_else(|| "codegen: Result enum layout missing (codegen bug)".to_string())?;
        let (tag_invalid, tag_incomplete) = {
            let utf8 = self
                .type_decls
                .enum_layouts
                .get("Utf8Error")
                .ok_or_else(|| {
                    "codegen: Utf8Error enum layout missing (codegen bug)".to_string()
                })?;
            let inv = *utf8.tags.get("InvalidByte").ok_or_else(|| {
                "codegen: Utf8Error.InvalidByte missing (codegen bug)".to_string()
            })?;
            let inc = *utf8.tags.get("IncompleteSequence").ok_or_else(|| {
                "codegen: Utf8Error.IncompleteSequence missing (codegen bug)".to_string()
            })?;
            (inv, inc)
        };

        let ok_bb = self.context.append_basic_block(fn_val, "cstr.ts.ok_bb");
        let err_bb = self.context.append_basic_block(fn_val, "cstr.ts.err_bb");
        let merge_bb = self.context.append_basic_block(fn_val, "cstr.ts.merge");
        self.builder
            .build_conditional_branch(ok, ok_bb, err_bb)
            .unwrap();

        // Ok arm: Result.Ok(<heap String the runtime wrote into out_str>).
        self.builder.position_at_end(ok_bb);
        let string_val = self
            .builder
            .build_load(str_ty, out_str, "cstr.ts.str")
            .unwrap();
        let ok_val = self.build_nonshared_enum_value("Result", "Ok", &[string_val])?;
        self.builder.build_unconditional_branch(merge_bb).unwrap();
        let ok_end = self.builder.get_insert_block().unwrap();

        // Err arm: Result.Err(Utf8Error.<runtime-selected variant>). Both
        // candidate variants are unit-payload, so building a base aggregate for
        // one and overwriting its tag word yields the other with no extra block.
        self.builder.position_at_end(err_bb);
        let err_tag = self
            .builder
            .build_load(i8_t, out_err, "cstr.ts.errtag")
            .unwrap()
            .into_int_value();
        let is_invalid = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                err_tag,
                i8_t.const_zero(),
                "cstr.ts.is_invalid",
            )
            .unwrap();
        let sel_tag = self
            .builder
            .build_select(
                is_invalid,
                i64_t.const_int(tag_invalid, false),
                i64_t.const_int(tag_incomplete, false),
                "cstr.ts.errsel",
            )
            .unwrap()
            .into_int_value();
        let base_err = self
            .build_nonshared_enum_value("Utf8Error", "InvalidByte", &[])?
            .into_struct_value();
        let utf8_err = self
            .builder
            .build_insert_value(base_err, sel_tag, 0, "cstr.ts.utf8err")
            .unwrap()
            .into_struct_value();
        let err_val = self.build_nonshared_enum_value("Result", "Err", &[utf8_err.into()])?;
        self.builder.build_unconditional_branch(merge_bb).unwrap();
        let err_end = self.builder.get_insert_block().unwrap();

        self.builder.position_at_end(merge_bb);
        let phi = self.builder.build_phi(result_ty, "cstr.ts.result").unwrap();
        phi.add_incoming(&[(&ok_val, ok_end), (&err_val, err_end)]);
        Ok(phi.as_basic_value())
    }

    /// Lower `CStr.to_string_slice() -> Result[StringSlice, Utf8Error]` — the
    /// zero-copy sibling of `to_string`. The receiver is the `{ptr, i64}`
    /// slice-struct (field 0 the NUL-terminated bytes, field 1 the source
    /// length). Instead of copying into an owning `String`, on valid UTF-8 it
    /// returns a borrowed `StringSlice` VIEW over the SAME bytes (design.md
    /// § StringSlice: a borrowed window, no allocation).
    pub(super) fn compile_cstr_to_string_slice(
        &mut self,
        object: &Expr,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let recv = self.compile_expr(object)?;
        let agg = recv.into_struct_value();
        let data_ptr = self
            .builder
            .build_extract_value(agg, 0, "cstr.tss.ptr")
            .unwrap()
            .into_pointer_value();
        let data_len = self
            .builder
            .build_extract_value(agg, 1, "cstr.tss.len")
            .unwrap()
            .into_int_value();
        self.build_utf8_validated_slice_result(data_ptr, data_len)
    }

    /// Borrowed-view sibling of `build_utf8_validated_result` (backs
    /// `CStr.to_string_slice()`): validate `(data_ptr, data_len)` as UTF-8 via
    /// the NON-copying `karac_runtime_utf8_validate` and build
    /// `Result[StringSlice, Utf8Error]` — `Ok(StringSlice { ptr: data_ptr,
    /// len: data_len, cap: 0 })` (a VIEW over the input, not a copy) on valid
    /// UTF-8, else the same `Err(Utf8Error.{InvalidByte | IncompleteSequence})`
    /// selected from the runtime discriminant. `StringSlice` shares the
    /// `vec_struct_type` LLVM layout with `String`, so the enum layout is
    /// identical to the owning `to_string()` path; the `cap == 0` field is what
    /// keeps the view from being freed at scope exit (the drop path's
    /// static/borrowed guard), so the input bytes (rodata for a `c"..."`
    /// literal, caller-owned for a `from_ptr` receiver) are only READ.
    pub(super) fn build_utf8_validated_slice_result(
        &mut self,
        data_ptr: inkwell::values::PointerValue<'ctx>,
        data_len: inkwell::values::IntValue<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let i64_t = self.context.i64_type();
        let i8_t = self.context.i8_type();
        // `StringSlice` lowers to the same `{ptr, i64, i64}` shape as `String`.
        let slice_ty = self.vec_struct_type();

        let fn_val = self
            .current_fn
            .ok_or_else(|| "codegen: CStr.to_string_slice called outside a function".to_string())?;
        let out_err = self.create_entry_alloca(fn_val, "cstr.tss.outerr", i8_t.into());

        let f = self
            .module
            .get_function("karac_runtime_utf8_validate")
            .expect("karac_runtime_utf8_validate declared in Codegen::new");
        let ok = self
            .builder
            .build_call(
                f,
                &[data_ptr.into(), data_len.into(), out_err.into()],
                "cstr.tss.ok",
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();

        // Copy out the Result llvm-type and the two Utf8Error variant tags
        // before any `&mut self` call (drops the `enum_layouts` borrow).
        let result_ty = self
            .type_decls
            .enum_layouts
            .get("Result")
            .map(|l| l.llvm_type)
            .ok_or_else(|| "codegen: Result enum layout missing (codegen bug)".to_string())?;
        let (tag_invalid, tag_incomplete) = {
            let utf8 = self
                .type_decls
                .enum_layouts
                .get("Utf8Error")
                .ok_or_else(|| {
                    "codegen: Utf8Error enum layout missing (codegen bug)".to_string()
                })?;
            let inv = *utf8.tags.get("InvalidByte").ok_or_else(|| {
                "codegen: Utf8Error.InvalidByte missing (codegen bug)".to_string()
            })?;
            let inc = *utf8.tags.get("IncompleteSequence").ok_or_else(|| {
                "codegen: Utf8Error.IncompleteSequence missing (codegen bug)".to_string()
            })?;
            (inv, inc)
        };

        let ok_bb = self.context.append_basic_block(fn_val, "cstr.tss.ok_bb");
        let err_bb = self.context.append_basic_block(fn_val, "cstr.tss.err_bb");
        let merge_bb = self.context.append_basic_block(fn_val, "cstr.tss.merge");
        self.builder
            .build_conditional_branch(ok, ok_bb, err_bb)
            .unwrap();

        // Ok arm: Result.Ok(StringSlice { data_ptr, data_len, cap: 0 }) — a
        // borrowed view; cap == 0 keeps the drop path from freeing it.
        self.builder.position_at_end(ok_bb);
        let view0 = self
            .builder
            .build_insert_value(slice_ty.const_zero(), data_ptr, 0, "cstr.tss.v0")
            .unwrap()
            .into_struct_value();
        let view1 = self
            .builder
            .build_insert_value(view0, data_len, 1, "cstr.tss.v1")
            .unwrap()
            .into_struct_value();
        let view = self
            .builder
            .build_insert_value(view1, i64_t.const_zero(), 2, "cstr.tss.v2")
            .unwrap()
            .into_struct_value();
        let ok_val = self.build_nonshared_enum_value("Result", "Ok", &[view.into()])?;
        self.builder.build_unconditional_branch(merge_bb).unwrap();
        let ok_end = self.builder.get_insert_block().unwrap();

        // Err arm: Result.Err(Utf8Error.<runtime-selected variant>) — identical
        // to the owning `to_string()` path.
        self.builder.position_at_end(err_bb);
        let err_tag = self
            .builder
            .build_load(i8_t, out_err, "cstr.tss.errtag")
            .unwrap()
            .into_int_value();
        let is_invalid = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                err_tag,
                i8_t.const_zero(),
                "cstr.tss.is_invalid",
            )
            .unwrap();
        let sel_tag = self
            .builder
            .build_select(
                is_invalid,
                i64_t.const_int(tag_invalid, false),
                i64_t.const_int(tag_incomplete, false),
                "cstr.tss.errsel",
            )
            .unwrap()
            .into_int_value();
        let base_err = self
            .build_nonshared_enum_value("Utf8Error", "InvalidByte", &[])?
            .into_struct_value();
        let utf8_err = self
            .builder
            .build_insert_value(base_err, sel_tag, 0, "cstr.tss.utf8err")
            .unwrap()
            .into_struct_value();
        let err_val = self.build_nonshared_enum_value("Result", "Err", &[utf8_err.into()])?;
        self.builder.build_unconditional_branch(merge_bb).unwrap();
        let err_end = self.builder.get_insert_block().unwrap();

        self.builder.position_at_end(merge_bb);
        let phi = self
            .builder
            .build_phi(result_ty, "cstr.tss.result")
            .unwrap();
        phi.add_incoming(&[(&ok_val, ok_end), (&err_val, err_end)]);
        Ok(phi.as_basic_value())
    }

    /// Lower an ambient built-in resource method (`env.set`, `clock.now`).
    ///
    /// A `with_provider[R]` override of an ambient resource is pushed onto
    /// the runtime provider stack (see `compile_with_provider_ambient`), so
    /// the override is visible across function-call boundaries — including
    /// the `karac test` synthesized-main path, which wraps a *call* to the
    /// test fn. When an override vtable for this resource exists in the
    /// module, emit a runtime branch: consult `karac_provider_lookup`, and
    /// if an override frame is active, dispatch through its vtable;
    /// otherwise fall to the builtin FFI default. When no override vtable
    /// exists (no `with_provider[R]` in the module), no override can be
    /// active, so skip the branch and emit the FFI default directly.
    pub(super) fn compile_ambient_resource_method(
        &mut self,
        resource: &str,
        method: &str,
        args: &[CallArg],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        // Compile args ONCE — they must not be re-evaluated across the
        // override / default branches (side effects would double-run).
        let arg_vals: Vec<BasicValueEnum<'ctx>> = args
            .iter()
            .map(|a| self.compile_expr(&a.value))
            .collect::<Result<_, _>>()?;

        // Runtime override dispatch is possible only when (a) this method
        // has a canonical vtable slot and (b) some override vtable for this
        // resource was emitted in the module. Otherwise no override can be
        // active at runtime — emit the FFI default directly.
        if let Some(method_idx) = ambient_method_index(resource, method) {
            if let Some(fn_type) = self.ambient_override_fn_type(resource, method) {
                return self.compile_ambient_dispatch_branch(
                    resource, method, method_idx, fn_type, &arg_vals,
                );
            }
        } else if self.ambient_override_fn_type(resource, method).is_some() {
            // The method has NO `AMBIENT_RESOURCE_METHODS` vtable slot, yet a
            // `with_provider[<resource>]` override in this module supplies an
            // impl of it (its `@<Type>.<method>` symbol exists). With no slot
            // there is no runtime dispatch branch, so falling through to the
            // builtin FFI default would SILENTLY ignore the override and
            // diverge from the interpreter. Error loudly instead. Every
            // ambient method that has both an FFI default and override support
            // is listed in `AMBIENT_RESOURCE_METHODS` (so it takes the branch
            // above) — reaching here means a method gained an override impl
            // before earning a slot; add it to the table to lift this.
            return Err(format!(
                "codegen: a `with_provider[{resource}]` override supplies `{method}`, but \
                 ambient overrides of `{resource}.{method}` are not yet lowered (the method has \
                 no vtable slot, so the override would be silently ignored). Run this program \
                 with `karac run` (interpreter), or drop the override of `{method}`. Tracked in \
                 docs/implementation_checklist/phase-7-codegen.md."
            ));
        }
        self.compile_ambient_ffi(resource, method, &arg_vals)
    }

    /// Emit the runtime override-vs-default branch for an ambient method
    /// call whose resource has an override vtable in this module:
    /// ```text
    ///   {data, vt} = karac_provider_lookup(<resource_id>)
    ///   br (data != null), %override, %default
    /// override: fn = vt[<method_idx>]; r1 = call fn(self=data, args...)
    /// default:  r2 = <ambient FFI default>
    /// merge:    phi <ret> [r1, override], [r2, default]
    /// ```
    /// The merge phi takes the method's real return type, read off the
    /// FFI-default value (`default_val.get_type()`): i64 for the scalar /
    /// unit-placeholder methods (`Clock.now`, `RandomSource.next_u64`,
    /// `Env.set`, `Stdout/Stderr.*`), the `Vec` struct for `Env.args`, the
    /// `Result` enum for `Env.var` / `Stdin.*` / `FileSystem.*`. The
    /// override arm and the default arm both lower the same Kāra signature,
    /// so they produce the identical LLVM type (aggregates return by value —
    /// no sret), and a void-returning override yields the same i64-0
    /// placeholder the unit FFI default does. A null fn-ptr slot (override
    /// implements only some methods) would null-deref in the override arm —
    /// but the override arm is only taken when a frame is active, and an
    /// active provider must implement every method the body calls (the
    /// interpreter errors otherwise — `resource_method.rs`, no per-method
    /// fallback), so the slot for a called method is non-null.
    pub(super) fn compile_ambient_dispatch_branch(
        &mut self,
        resource: &str,
        method: &str,
        method_idx: usize,
        fn_type: inkwell::types::FunctionType<'ctx>,
        arg_vals: &[BasicValueEnum<'ctx>],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let resource_id = *self
            .provider_state
            .provider_resource_ids
            .get(resource)
            .ok_or_else(|| {
                format!("codegen: ambient resource '{resource}' has no minted ID (codegen bug)")
            })?;
        let i32_t = self.context.i32_type();
        let i64_t = self.context.i64_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let fn_val = self
            .current_fn
            .ok_or_else(|| "ambient dispatch: no current function".to_string())?;

        // Runtime lookup → {data, vtable}.
        let id_v = i32_t.const_int(resource_id as u64, false);
        let lookup_sv = self
            .builder
            .build_call(
                self.runtime_fns.karac_provider_lookup_fn,
                &[id_v.into()],
                "amb.lookup",
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_struct_value();
        let data_ptr = self
            .builder
            .build_extract_value(lookup_sv, 0, "amb.data")
            .unwrap()
            .into_pointer_value();
        let vtable_ptr = self
            .builder
            .build_extract_value(lookup_sv, 1, "amb.vt")
            .unwrap()
            .into_pointer_value();
        let is_present = self
            .builder
            .build_is_not_null(data_ptr, "amb.present")
            .unwrap();

        let override_bb = self.context.append_basic_block(fn_val, "amb.override");
        let default_bb = self.context.append_basic_block(fn_val, "amb.default");
        let merge_bb = self.context.append_basic_block(fn_val, "amb.merge");
        self.builder
            .build_conditional_branch(is_present, override_bb, default_bb)
            .unwrap();

        // override arm: indirect call through the vtable slot.
        self.builder.position_at_end(override_bb);
        let idx_v = i32_t.const_int(method_idx as u64, false);
        let fn_slot = unsafe {
            self.builder
                .build_gep(ptr_ty, vtable_ptr, &[idx_v], "amb.fn.slot")
                .unwrap()
        };
        let fn_ptr = self
            .builder
            .build_load(ptr_ty, fn_slot, "amb.fn")
            .unwrap()
            .into_pointer_value();
        // self-arg lowering mirrors `try_compile_provider_dispatch`: ptr
        // for `ref/mut ref/shared self`, loaded struct for owned `self`.
        let self_param_ty = fn_type
            .get_param_types()
            .into_iter()
            .next()
            .ok_or_else(|| {
                format!("ambient dispatch: override method `{resource}.{method}` has no self param")
            })?;
        let self_arg: BasicMetadataValueEnum<'ctx> = match self_param_ty {
            inkwell::types::BasicMetadataTypeEnum::PointerType(_) => {
                BasicMetadataValueEnum::from(data_ptr)
            }
            inkwell::types::BasicMetadataTypeEnum::StructType(st) => {
                let loaded = self
                    .builder
                    .build_load(st, data_ptr, "amb.self.owned")
                    .unwrap();
                BasicMetadataValueEnum::from(loaded)
            }
            other => {
                return Err(format!(
                    "ambient dispatch: unexpected self-param lowering `{other:?}` for `{resource}.{method}`"
                ));
            }
        };
        let mut call_args: Vec<BasicMetadataValueEnum<'ctx>> = vec![self_arg];
        for v in arg_vals {
            call_args.push(BasicMetadataValueEnum::from(*v));
        }
        let override_call = self
            .builder
            .build_indirect_call(fn_type, fn_ptr, &call_args, "amb.call")
            .unwrap();
        let override_val: BasicValueEnum<'ctx> =
            if override_call.try_as_basic_value().is_instruction() {
                i64_t.const_int(0, false).into()
            } else {
                override_call.try_as_basic_value().unwrap_basic()
            };
        self.builder.build_unconditional_branch(merge_bb).unwrap();
        let override_end = self.builder.get_insert_block().unwrap();

        // default arm: the builtin FFI default.
        self.builder.position_at_end(default_bb);
        let default_val = self.compile_ambient_ffi(resource, method, arg_vals)?;
        self.builder.build_unconditional_branch(merge_bb).unwrap();
        let default_end = self.builder.get_insert_block().unwrap();

        // merge: phi the two results at the method's real return type. Both
        // arms lower the same Kāra signature, so their LLVM types match; a
        // void override reuses the unit i64-0 placeholder (= `default_val`).
        self.builder.position_at_end(merge_bb);
        let phi = self
            .builder
            .build_phi(default_val.get_type(), "amb.result")
            .unwrap();
        phi.add_incoming(&[(&override_val, override_end), (&default_val, default_end)]);
        Ok(phi.as_basic_value())
    }

    /// The builtin-FFI default lowering for an ambient method (the codegen
    /// counterpart of the interpreter's
    /// `dispatch_builtin_resource_method_with_values`). Takes already-
    /// compiled arg values so it can serve both the no-override fast path
    /// and the default arm of `compile_ambient_dispatch_branch` without
    /// re-evaluating args. Only the resource/method pairs the runtime backs
    /// are lowered; others error naming the gap rather than miscompiling.
    pub(super) fn compile_ambient_ffi(
        &mut self,
        resource: &str,
        method: &str,
        arg_vals: &[BasicValueEnum<'ctx>],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let i64_t = self.context.i64_type();
        let ptr_t = self.context.ptr_type(AddressSpace::default());
        match (resource, method) {
            ("Env", "set") => {
                if arg_vals.len() != 2 {
                    return Err(format!(
                        "codegen: env.set expects 2 arguments, found {}",
                        arg_vals.len()
                    ));
                }
                let (name_ptr, name_len) = self.extract_string_ptr_len(arg_vals[0], "env.set.name");
                let (val_ptr, val_len) = self.extract_string_ptr_len(arg_vals[1], "env.set.val");
                let fn_val = match self.module.get_function("karac_runtime_env_set") {
                    Some(f) => f,
                    None => {
                        let fn_ty = self.context.void_type().fn_type(
                            &[ptr_t.into(), i64_t.into(), ptr_t.into(), i64_t.into()],
                            false,
                        );
                        self.module
                            .add_function("karac_runtime_env_set", fn_ty, None)
                    }
                };
                self.builder
                    .build_call(
                        fn_val,
                        &[
                            name_ptr.into(),
                            name_len.into(),
                            val_ptr.into(),
                            val_len.into(),
                        ],
                        "env.set",
                    )
                    .unwrap();
                // `env.set` returns Unit → the i64-0 void-return placeholder.
                Ok(i64_t.const_int(0, false).into())
            }
            ("Clock", "now") => {
                if !arg_vals.is_empty() {
                    return Err(format!(
                        "codegen: clock.now expects 0 arguments, found {}",
                        arg_vals.len()
                    ));
                }
                let fn_val = match self.module.get_function("karac_runtime_clock_now") {
                    Some(f) => f,
                    None => {
                        let fn_ty = i64_t.fn_type(&[], false);
                        self.module
                            .add_function("karac_runtime_clock_now", fn_ty, None)
                    }
                };
                let call = self.builder.build_call(fn_val, &[], "clock.now").unwrap();
                Ok(call.try_as_basic_value().unwrap_basic())
            }
            ("RandomSource", "next_u64") => {
                if !arg_vals.is_empty() {
                    return Err(format!(
                        "codegen: rand.next_u64 expects 0 arguments, found {}",
                        arg_vals.len()
                    ));
                }
                let fn_val = match self.module.get_function("karac_runtime_rand_next_u64") {
                    Some(f) => f,
                    None => {
                        let fn_ty = i64_t.fn_type(&[], false);
                        self.module
                            .add_function("karac_runtime_rand_next_u64", fn_ty, None)
                    }
                };
                let call = self
                    .builder
                    .build_call(fn_val, &[], "rand.next_u64")
                    .unwrap();
                Ok(call.try_as_basic_value().unwrap_basic())
            }
            ("Env", "args") => {
                if !arg_vals.is_empty() {
                    return Err(format!(
                        "codegen: env.args expects 0 arguments, found {}",
                        arg_vals.len()
                    ));
                }
                // `env.args() -> Vec[String]` — first aggregate-returning
                // ambient method. Out-pointer ABI: alloca a `{ptr, i64, i64}`
                // Vec slot, hand its address to the runtime fn (which
                // heap-allocates the element buffer + each String in Kāra
                // shape so scope-exit cleanup frees them), then load the Vec
                // value. Mirrors the `Runtime.list_par_blocks` lowering.
                let vec_ty = self.vec_struct_type();
                let fn_val = self
                    .current_fn
                    .ok_or_else(|| "codegen: env.args called outside a function".to_string())?;
                let slot = self.create_entry_alloca(fn_val, "env.args.slot", vec_ty.into());
                let f = match self.module.get_function("karac_runtime_env_args_into") {
                    Some(f) => f,
                    None => {
                        let fn_ty = self.context.void_type().fn_type(&[ptr_t.into()], false);
                        self.module
                            .add_function("karac_runtime_env_args_into", fn_ty, None)
                    }
                };
                self.builder
                    .build_call(f, &[slot.into()], "env.args.fill")
                    .unwrap();
                let value = self
                    .builder
                    .build_load(vec_ty, slot, "env.args.val")
                    .unwrap();
                Ok(value)
            }
            ("Env", "var") => {
                if arg_vals.len() != 1 {
                    return Err(format!(
                        "codegen: env.var expects 1 argument, found {}",
                        arg_vals.len()
                    ));
                }
                // `env.var(name) -> Result[String, VarError]`. The runtime FFI
                // does the OS read + heap String copy and returns `found:i1`,
                // writing the String into an out-slot; codegen builds the
                // Result enum here — `Ok(string)` on found, `Err(VarError
                // .NotPresent)` on miss — so all enum-layout knowledge stays
                // on the codegen side (codegen-containment). String shares the
                // `{ptr, i64, i64}` shape with Vec, so `vec_struct_type()` is
                // the out-slot type.
                let (name_ptr, name_len) = self.extract_string_ptr_len(arg_vals[0], "env.var.name");
                let str_ty = self.vec_struct_type();
                let fn_val = self
                    .current_fn
                    .ok_or_else(|| "codegen: env.var called outside a function".to_string())?;
                let out_slot = self.create_entry_alloca(fn_val, "env.var.out", str_ty.into());
                let f = match self.module.get_function("karac_runtime_env_var") {
                    Some(f) => f,
                    None => {
                        let fn_ty = self
                            .context
                            .bool_type()
                            .fn_type(&[ptr_t.into(), i64_t.into(), ptr_t.into()], false);
                        self.module
                            .add_function("karac_runtime_env_var", fn_ty, None)
                    }
                };
                let found = self
                    .builder
                    .build_call(
                        f,
                        &[name_ptr.into(), name_len.into(), out_slot.into()],
                        "env.var.found",
                    )
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_int_value();

                let result_ty = self
                    .type_decls
                    .enum_layouts
                    .get("Result")
                    .map(|l| l.llvm_type)
                    .ok_or_else(|| {
                        "codegen: Result enum layout missing (codegen bug)".to_string()
                    })?;

                let found_bb = self.context.append_basic_block(fn_val, "env.var.found_bb");
                let notfound_bb = self
                    .context
                    .append_basic_block(fn_val, "env.var.notfound_bb");
                let merge_bb = self.context.append_basic_block(fn_val, "env.var.merge");
                self.builder
                    .build_conditional_branch(found, found_bb, notfound_bb)
                    .unwrap();

                // found arm: Result.Ok(<heap String the FFI wrote>).
                self.builder.position_at_end(found_bb);
                let string_val = self
                    .builder
                    .build_load(str_ty, out_slot, "env.var.str")
                    .unwrap();
                let ok_val = self.build_nonshared_enum_value("Result", "Ok", &[string_val])?;
                self.builder.build_unconditional_branch(merge_bb).unwrap();
                let found_end = self.builder.get_insert_block().unwrap();

                // miss arm: Result.Err(VarError.NotPresent).
                self.builder.position_at_end(notfound_bb);
                let varerr = self.build_nonshared_enum_value("VarError", "NotPresent", &[])?;
                let err_val = self.build_nonshared_enum_value("Result", "Err", &[varerr])?;
                self.builder.build_unconditional_branch(merge_bb).unwrap();
                let notfound_end = self.builder.get_insert_block().unwrap();

                self.builder.position_at_end(merge_bb);
                let phi = self.builder.build_phi(result_ty, "env.var.result").unwrap();
                phi.add_incoming(&[(&ok_val, found_end), (&err_val, notfound_end)]);
                Ok(phi.as_basic_value())
            }
            ("Stdin", "read_line") | ("Stdin", "read_to_string") => {
                if !arg_vals.is_empty() {
                    return Err(format!(
                        "codegen: stdin.{method} expects 0 arguments, found {}",
                        arg_vals.len()
                    ));
                }
                // `stdin.read_line()` / `read_to_string()` -> Result[String,
                // IoError]. Same `KaracIoResult` out-param ABI + String-payload
                // unpack as `FileSystem.read_to_string`: alloca the 32-byte
                // result slot, call the runtime fn, then `lower_kara_io_result`
                // builds `Result.Ok(string)` (error_kind == 0) or
                // `Result.Err(IoError)` (variant from the runtime's error_kind),
                // so all IoError-layout knowledge stays in the shared file-IO
                // lowering rather than being duplicated here.
                let symbol = if method == "read_line" {
                    "karac_runtime_stdin_read_line"
                } else {
                    "karac_runtime_stdin_read_to_string"
                };
                let io_ty = self.kara_io_result_type();
                let fn_val = self
                    .current_fn
                    .ok_or_else(|| format!("codegen: stdin.{method} called outside a function"))?;
                let slot = self.create_entry_alloca(fn_val, "stdin.read.slot", io_ty.into());
                let f = match self.module.get_function(symbol) {
                    Some(f) => f,
                    None => {
                        let fn_ty = self.context.void_type().fn_type(&[ptr_t.into()], false);
                        self.module.add_function(symbol, fn_ty, None)
                    }
                };
                self.builder
                    .build_call(f, &[slot.into()], "stdin.read.call")
                    .unwrap();
                self.lower_kara_io_result(slot, super::file::FileOkKind::StringPayload)
            }
            ("Stdout", "print")
            | ("Stdout", "println")
            | ("Stderr", "print")
            | ("Stderr", "println") => {
                if arg_vals.len() != 1 {
                    return Err(format!(
                        "codegen: {resource}.{method} expects 1 argument, found {}",
                        arg_vals.len()
                    ));
                }
                let to_stderr = resource == "Stderr";
                let newline = method == "println";
                self.emit_console_str_write(arg_vals[0], to_stderr, newline)?;
                // Returns Unit → the i64-0 void-return placeholder.
                Ok(i64_t.const_int(0, false).into())
            }
            ("Stdout", "flush") | ("Stderr", "flush") => {
                if !arg_vals.is_empty() {
                    return Err(format!(
                        "codegen: {resource}.flush expects 0 arguments, found {}",
                        arg_vals.len()
                    ));
                }
                // `fflush(NULL)` flushes every open output stream — portable
                // (POSIX), and crucially flushes the libc stdout buffer that
                // `printf` (free `print`/`println` and `Stdout.*`) writes
                // into. `Stderr.*` goes to fd 2 unbuffered via `dprintf`, so
                // its flush is a no-op, but `fflush(NULL)` covers both
                // uniformly. No FILE*-global access needed (the `stdout` /
                // `__stderrp` symbol differs across libc).
                let fflush = match self.module.get_function("fflush") {
                    Some(f) => f,
                    None => {
                        let ty = self.context.i32_type().fn_type(&[ptr_t.into()], false);
                        self.module.add_function("fflush", ty, None)
                    }
                };
                self.builder
                    .build_call(fflush, &[ptr_t.const_null().into()], "fflush")
                    .unwrap();
                Ok(i64_t.const_int(0, false).into())
            }
            ("FileSystem", "read_to_string") => {
                // Lowercase `fs.read_to_string(path)`. The capitalized
                // `FileSystem.read_to_string` is lowered on the associated-call
                // path (`assoc_call.rs` → `compile_file_read_to_string`); the
                // ambient-alias path arrives here with the path already
                // compiled, so route to the value-core variant.
                if arg_vals.len() != 1 {
                    return Err(format!(
                        "codegen: fs.read_to_string expects 1 argument, found {}",
                        arg_vals.len()
                    ));
                }
                self.compile_file_read_to_string_val(arg_vals[0])
            }
            ("FileSystem", "read_lines") => {
                // Lowercase `fs.read_lines(path)`. Capitalized form is lowered
                // via `assoc_call.rs` → `compile_fs_read_lines`; here the path
                // is pre-compiled, so route to the value-core variant. B-38.
                if arg_vals.len() != 1 {
                    return Err(format!(
                        "codegen: fs.read_lines expects 1 argument, found {}",
                        arg_vals.len()
                    ));
                }
                self.compile_fs_read_lines_val(arg_vals[0])
            }
            ("FileSystem", "write") => {
                // Lowercase `fs.write(path, contents)`. Capitalized form is
                // lowered via `assoc_call.rs` → `compile_fs_write`; here both
                // args are pre-compiled, so use the value-core variant.
                if arg_vals.len() != 2 {
                    return Err(format!(
                        "codegen: fs.write expects 2 arguments, found {}",
                        arg_vals.len()
                    ));
                }
                self.compile_fs_write_vals(arg_vals[0], arg_vals[1])
            }
            _ => Err(format!(
                "codegen: ambient resource method '{}.{}' is not yet lowered \
                 (interpreter-only); add a runtime FFI + an arm in \
                 `compile_ambient_ffi`",
                resource, method
            )),
        }
    }

    /// Emit a console write of a Kāra `String` value to stdout or stderr,
    /// optionally with a trailing newline. Backs the `Stdout.{print,println}`
    /// / `Stderr.{print,println}` ambient methods (L646 slice 4b).
    ///
    /// **Stdout** reuses `self.runtime_fns.printf_fn` — the SAME libc `printf` / stdout
    /// buffer the free `print`/`println` builtins use (`compile_print`), so a
    /// program mixing `println(x)` and `Stdout.println(y)` never interleaves
    /// out of order. **Stderr** writes to fd 2 via POSIX `dprintf`, avoiding
    /// the non-portable `stderr` / `__stderrp` FILE*-global; fd 2 is
    /// unbuffered. Both use `%.*s` with the explicit length (field 1) so a
    /// non-NUL-terminated heap `String` is read exactly `len` bytes —
    /// identical to `compile_print`'s String-value arm (which documents the
    /// ASan heap-overflow that a bare `%s` would cause).
    pub(super) fn emit_console_str_write(
        &mut self,
        str_val: BasicValueEnum<'ctx>,
        to_stderr: bool,
        newline: bool,
    ) -> Result<(), String> {
        if !str_val.is_struct_value() {
            return Err(format!(
                "codegen: console write expects a String value, got {str_val:?}"
            ));
        }
        let sv = str_val.into_struct_value();
        let str_ptr = self
            .builder
            .build_extract_value(sv, 0, "con.str.ptr")
            .unwrap()
            .into_pointer_value();
        let str_len = self
            .builder
            .build_extract_value(sv, 1, "con.str.len")
            .unwrap()
            .into_int_value();
        let nl = if newline { "\n" } else { "" };
        // NUL-safe `fwrite` to the stdout / stderr `FILE*` (L5) — the old
        // `printf`/`dprintf("%.*s")` form truncated a String at an interior
        // NUL. stderr's `FILE*` is unbuffered by default, preserving the
        // immediate-flush semantics the prior `dprintf(fd 2)` had.
        self.emit_nul_safe_write(str_ptr, str_len, nl, to_stderr);
        Ok(())
    }
}
