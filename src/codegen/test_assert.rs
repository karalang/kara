//! Codegen lowering for the test-runner builtins `assert`, `assert_eq`,
//! `assert_ne` (prelude names — see `src/prelude.rs`). Until Slice c.1
//! these were interpreter-only — codegen silently dropped the calls
//! (unknown-function fallthrough at `call_dispatch.rs`), which meant
//! AOT-compiled programs ignored failing asserts entirely. This module
//! lowers each call to a typed comparison + a structured-failure-
//! emission + `exit(1)` shape, matching the interpreter's
//! `eval_builtin_assert*` semantics for the v1 supported operand types
//! (int, bool, char, float, String).
//!
//! On failure the lowered code calls `karac_test_record_failure` with
//! `(file, line, col, msg, left_opt, right_opt)` — the runtime writes a
//! `KARAC_TEST_FAILURE {...JSON...}` line to stderr. The runner subprocess
//! introduced in Slice c.3 parses this line into a `TestOutcome`. For
//! plain `assert(cond)` failures the `left` / `right` slots are null.
//! For `assert_eq` / `assert_ne` mismatches the failure path runs each
//! operand through `compile_fstr_part_to_cstr` (same machinery as
//! f-string interpolation) to produce `(ptr, len)` byte slices.
//!
//! Operand types not currently formattable (user structs, enums, Vec /
//! Map / Set) fall through to null `left` / `right`. The runner still
//! sees the `file:line:col` plus the bare "left != right" / "left ==
//! right" message, so the test exits non-zero with a structured failure
//! event — formatting fidelity is the only thing that degrades.
//! Tightening this is a follow-up — see slice (c) wip notes.

use crate::ast::*;

use inkwell::types::BasicTypeEnum;
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum, IntValue, PointerValue};
use inkwell::AddressSpace;

impl<'ctx> super::Codegen<'ctx> {
    /// Lower `assert(cond: bool)`. On `cond == false` calls
    /// `karac_test_record_failure` with `"assertion failed"` + null
    /// operands, then `exit(1)`. Returns the i64-zero unit placeholder
    /// other free-fn dispatches use, with the builder positioned at the
    /// continuation block.
    pub(super) fn compile_assert(
        &mut self,
        args: &[CallArg],
        call_span: &crate::token::Span,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        if args.len() != 1 {
            return Err(format!("assert() expects 1 argument, found {}", args.len()));
        }
        let cond_val = self.compile_expr(&args[0].value)?;
        let cond_i1 = match cond_val {
            BasicValueEnum::IntValue(iv) if iv.get_type().get_bit_width() == 1 => iv,
            _ => {
                return Err("assert() argument must be a bool expression".to_string());
            }
        };

        let cur_fn = self
            .current_fn
            .ok_or_else(|| "compile_assert outside a function context".to_string())?;
        let fail_bb = self.context.append_basic_block(cur_fn, "assert.fail");
        let cont_bb = self.context.append_basic_block(cur_fn, "assert.cont");
        self.builder
            .build_conditional_branch(cond_i1, cont_bb, fail_bb)
            .unwrap();

        // Failure path
        self.builder.position_at_end(fail_bb);
        self.emit_test_record_failure_and_exit(call_span, "assertion failed", None, None);

        // Continuation
        self.builder.position_at_end(cont_bb);
        Ok(self.context.i64_type().const_zero().into())
    }

    /// Lower `assert_eq(l, r)` / `assert_ne(l, r)`. `negate == false` →
    /// `assert_eq`: failure when `l != r`. `negate == true` →
    /// `assert_ne`: failure when `l == r`. The comparison reuses
    /// `compile_binop(Eq, ..)`, which handles primitives (int/bool/
    /// char/float) and the String/Vec 3-field struct layout via
    /// `compile_string_binop`, and user-struct field-by-field equality
    /// via `compile_struct_eq`.
    pub(super) fn compile_assert_eq(
        &mut self,
        args: &[CallArg],
        call_span: &crate::token::Span,
        negate: bool,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let name = if negate { "assert_ne" } else { "assert_eq" };
        if args.len() != 2 {
            return Err(format!(
                "{}() expects 2 arguments, found {}",
                name,
                args.len()
            ));
        }
        let l_val = self.compile_expr(&args[0].value)?;
        let r_val = self.compile_expr(&args[1].value)?;

        let eq_bv = self.compile_binop(&BinOp::Eq, l_val, r_val)?;
        let eq_i1 = eq_bv.into_int_value();

        let cur_fn = self
            .current_fn
            .ok_or_else(|| format!("{} outside a function context", name))?;
        let fail_label = format!("{}.fail", name);
        let cont_label = format!("{}.cont", name);
        let fail_bb = self.context.append_basic_block(cur_fn, &fail_label);
        let cont_bb = self.context.append_basic_block(cur_fn, &cont_label);

        // For `assert_eq`: eq=true → cont, eq=false → fail.
        // For `assert_ne`: eq=true → fail, eq=false → cont.
        let (then_bb, else_bb) = if negate {
            (fail_bb, cont_bb)
        } else {
            (cont_bb, fail_bb)
        };
        self.builder
            .build_conditional_branch(eq_i1, then_bb, else_bb)
            .unwrap();

        // Failure path
        self.builder.position_at_end(fail_bb);
        let left_fmt = self.try_format_assert_operand(l_val, &args[0].value);
        let right_fmt = self.try_format_assert_operand(r_val, &args[1].value);
        let msg = if negate {
            "assertion failed: left == right"
        } else {
            "assertion failed: left != right"
        };
        self.emit_test_record_failure_and_exit(call_span, msg, left_fmt, right_fmt);

        // Continuation
        self.builder.position_at_end(cont_bb);
        Ok(self.context.i64_type().const_zero().into())
    }

    /// Format an assert operand for the runtime failure record. Returns
    /// `(ptr, len)` for v1-supported types (int, bool, char, float, and
    /// the String 3-field struct layout). Returns `None` for everything
    /// else (multi-field user structs, enums, Vec / Map / Set, opaque
    /// pointers) — the failure record then carries `null` in that slot
    /// and the runner surfaces the bare message. `compile_fstr_part_to_cstr`
    /// would panic on non-String StructValues at its `extract_value` /
    /// `into_pointer_value` step, so the struct arm narrows to the
    /// 3-field `{ ptr, i64, i64 }` shape before delegating.
    fn try_format_assert_operand(
        &mut self,
        val: BasicValueEnum<'ctx>,
        src: &Expr,
    ) -> Option<(PointerValue<'ctx>, IntValue<'ctx>)> {
        match val {
            BasicValueEnum::IntValue(_) | BasicValueEnum::FloatValue(_) => {
                Some(self.compile_fstr_part_to_cstr(val, src))
            }
            BasicValueEnum::StructValue(sv) => {
                let st = sv.get_type();
                if st.count_fields() != 3 {
                    return None;
                }
                let f0 = st.get_field_type_at_index(0)?;
                let f1 = st.get_field_type_at_index(1)?;
                let is_ptr_field = matches!(f0, BasicTypeEnum::PointerType(_));
                let is_i64_field = matches!(
                    f1,
                    BasicTypeEnum::IntType(it) if it.get_bit_width() == 64
                );
                if is_ptr_field && is_i64_field {
                    Some(self.compile_fstr_part_to_cstr(val, src))
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// Emit the `karac_test_record_failure(...)` + `exit(1)` + `unreachable`
    /// triad on the current insertion point. `left` / `right` are
    /// `Some((ptr, len))` when the corresponding operand was formattable
    /// (see `try_format_assert_operand`), `None` for null slots. Caller
    /// re-positions the builder to a continuation block afterwards.
    fn emit_test_record_failure_and_exit(
        &mut self,
        call_span: &crate::token::Span,
        msg: &str,
        left: Option<(PointerValue<'ctx>, IntValue<'ctx>)>,
        right: Option<(PointerValue<'ctx>, IntValue<'ctx>)>,
    ) {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_ty = self.context.i64_type();
        let i32_ty = self.context.i32_type();

        // `#[track_caller]` slice 6: when the enclosing fn is `#[track_caller]`,
        // redirect the reported assert location to the received caller location
        // (the same redirect `emit_panic` performs for the other panic-emitters,
        // so an assert inside a `#[track_caller]` wrapper blames the wrapper's
        // caller for consistency with unwrap/index/div). The caller-location
        // file is a NUL-terminated C string (built that way in `compile_call`),
        // so its byte length comes from a runtime `strlen` rather than a
        // compile-time constant. Outside a `#[track_caller]` fn
        // (`current_fn_caller_loc == None`) this path is byte-identical to
        // before — the whole test-harness surface is unaffected.
        let (file_ptr, file_len, line, col) = match self.current_fn_caller_loc {
            Some((cf_file, cf_line, cf_col)) => {
                let strlen_fn = self
                    .module
                    .get_function("strlen")
                    .expect("strlen declared in Codegen::new");
                let len = self
                    .builder
                    .build_call(strlen_fn, &[cf_file.into()], "tc.assert_file_len")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_int_value();
                (cf_file, len, cf_line, cf_col)
            }
            None => {
                let (fp, fl) = match self.ensure_source_filename_global() {
                    Some((p, len)) => (p, i64_ty.const_int(len, false)),
                    None => (ptr_ty.const_null(), i64_ty.const_int(0, false)),
                };
                (
                    fp,
                    fl,
                    i32_ty.const_int(call_span.line as u64, false),
                    i32_ty.const_int(call_span.column as u64, false),
                )
            }
        };

        // Materialize the message as a deduped global string. The
        // runtime reads `msg_len` bytes, so we pass the exact byte length
        // and let the trailing NUL `build_global_string_ptr` adds sit
        // unused.
        let msg_global = self
            .builder
            .build_global_string_ptr(msg, "karac.assert_msg")
            .unwrap();
        let msg_ptr = msg_global.as_pointer_value();
        let msg_len = i64_ty.const_int(msg.len() as u64, false);

        let (left_ptr, left_len) = match left {
            Some((p, len)) => (p, len),
            None => (ptr_ty.const_null(), i64_ty.const_zero()),
        };
        let (right_ptr, right_len) = match right {
            Some((p, len)) => (p, len),
            None => (ptr_ty.const_null(), i64_ty.const_zero()),
        };

        self.builder
            .build_call(
                self.karac_test_record_failure_fn,
                &[
                    BasicMetadataValueEnum::from(file_ptr),
                    file_len.into(),
                    line.into(),
                    col.into(),
                    BasicMetadataValueEnum::from(msg_ptr),
                    msg_len.into(),
                    BasicMetadataValueEnum::from(left_ptr),
                    left_len.into(),
                    BasicMetadataValueEnum::from(right_ptr),
                    right_len.into(),
                ],
                "assert.record",
            )
            .unwrap();

        let exit_code = i32_ty.const_int(1, false);
        self.builder
            .build_call(self.exit_fn, &[exit_code.into()], "")
            .unwrap();
        self.builder.build_unreachable().unwrap();
    }
}
