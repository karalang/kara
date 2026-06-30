//! `Stats.*` free-function statistics — `karac build` (codegen) lowering.
//!
//! The `Stats` namespace (`runtime/stdlib/stats.kara`) exposes eight free
//! functions over `Slice[f64]`: `sum` / `prod` / `mean` / `variance` /
//! `stddev` / `median` → `f64`, and `min` / `max` → `Option[f64]`. The
//! interpreter dispatches them in `eval_stats_fn` (`src/interpreter/helpers.rs`);
//! this module is the AOT twin, intercepted in `compile_call` BEFORE the
//! generic free-function dispatch (the `#[compiler_builtin]` bodies are
//! doc-only placeholders).
//!
//! **Semantics mirrored byte-for-byte from the interpreter** (these differ
//! deliberately from the `Column` stat surface, which uses Bessel `n-1` and
//! skips nulls):
//!   * `variance` / `stddev` are the **population** forms (divide by `n`),
//!     matching `eval_stats_fn`.
//!   * the slice is a borrow with no validity bitmap — every element is read.
//!   * empty input: `sum` → `-0.0` (Rust's float `Sum` identity), `prod` →
//!     `1.0`, `min` / `max` → `None`;
//!     `mean` / `variance` / `stddev` / `median` **trap** (parity with the
//!     interpreter's empty-slice panic).
//!   * `median` of an even count averages the two middle values; the buffer is
//!     copied into a fresh scratch alloc and sorted there (never mutating the
//!     caller's Vec), then freed.
//!
//! The argument compiles to the `{ptr, i64 len, …}` Vec/Slice struct; field 0
//! is the data pointer and field 1 is the length, identical between the 3-word
//! `Vec[T]` and the 2-word `Slice[T]` layouts, so one extract handles either.
//! A fresh owned-temp argument (`Stats.mean(vec![…])`) is freed via
//! `materialize_owned_temp` — without it the early dispatch would skip the
//! generic owned-temp arg loop and leak (the
//! `builtin-method-early-dispatch-skips-owned-temp-arg-free` hazard).

use inkwell::values::{BasicValueEnum, FloatValue, IntValue, PointerValue};
use inkwell::{FloatPredicate, IntPredicate};

use crate::ast::{CallArg, Expr, ExprKind};
use crate::token::Span;

impl<'ctx> super::Codegen<'ctx> {
    /// Intercept `Stats.<method>(slice)`. Returns `Ok(None)` for any callee
    /// that is not a recognized `Stats` free function so `compile_call` falls
    /// through to its normal dispatch.
    pub(super) fn try_compile_stats_call(
        &mut self,
        callee: &Expr,
        args: &[CallArg],
        _call_span: &Span,
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        let method = match &callee.kind {
            ExprKind::Path { segments, .. } if segments.len() == 2 && segments[0] == "Stats" => {
                segments[1].as_str()
            }
            _ => return Ok(None),
        };
        if !matches!(
            method,
            "sum" | "prod" | "mean" | "variance" | "stddev" | "median" | "min" | "max"
        ) {
            return Ok(None);
        }

        let arg = args
            .first()
            .ok_or_else(|| format!("Stats.{method} expects one slice argument"))?;
        let val = self.compile_expr(&arg.value)?;
        let sv = val.into_struct_value();
        // Read the data pointer (field 0) and length (field 1) via scalar
        // `struct_gep` loads off a spill alloca — NOT `extractvalue` on a
        // 24-byte aggregate `load`. The aggregate-load + `extractvalue`
        // pattern mis-lowers the pointer field to null under ASan on
        // arm64-Linux (the value read as 0 → every reduction returned 0,
        // while the scalar-`struct_gep` index path read the same buffer
        // correctly). Spilling and reading the fields with 8-byte scalar
        // loads matches the proven index/`len()` path on every target.
        let vec_ty = sv.get_type();
        let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        let i64_t = self.context.i64_type();
        let spill = self.builder.build_alloca(vec_ty, "stats.arg").unwrap();
        self.builder.build_store(spill, sv).unwrap();
        let data_field = self
            .builder
            .build_struct_gep(vec_ty, spill, 0, "stats.data.p")
            .unwrap();
        let data = self
            .builder
            .build_load(ptr_ty, data_field, "stats.data")
            .unwrap()
            .into_pointer_value();
        let len_field = self
            .builder
            .build_struct_gep(vec_ty, spill, 1, "stats.len.p")
            .unwrap();
        let len = self
            .builder
            .build_load(i64_t, len_field, "stats.len")
            .unwrap()
            .into_int_value();

        // Free a fresh owned-temp Vec argument (`Stats.sum(vec![…])`). An
        // identifier binding's own scope drop already covers it, so only
        // fresh temps / collection literals are materialized; the helper
        // self-guards on the Vec/String LLVM shape.
        let is_fresh_temp = self.expr_yields_fresh_owned_temp(&arg.value)
            || matches!(&arg.value.kind, ExprKind::PrefixCollectionLiteral { .. });
        if is_fresh_temp && self.llvm_ty_is_vec_struct(val.get_type()) {
            self.materialize_owned_temp(val, (arg.value.span.offset, arg.value.span.length));
        }

        let result = match method {
            "sum" => self.stats_fold(data, len, false).into(),
            "prod" => self.stats_fold(data, len, true).into(),
            "mean" => self.stats_mean(data, len)?.into(),
            "variance" => self.stats_variance(data, len)?.into(),
            "stddev" => {
                let var = self.stats_variance(data, len)?;
                self.column_sqrt_f64(var).into()
            }
            "median" => self.stats_median(data, len)?.into(),
            "min" => self.stats_minmax(data, len, false),
            "max" => self.stats_minmax(data, len, true),
            _ => unreachable!(),
        };
        Ok(Some(result))
    }

    /// `sum` (seed `0.0`, add) / `prod` (seed `1.0`, multiply) over the whole
    /// contiguous `f64` buffer. Empty input yields the seed (parity with the
    /// interpreter — no trap).
    fn stats_fold(
        &self,
        data: PointerValue<'ctx>,
        len: IntValue<'ctx>,
        is_mul: bool,
    ) -> FloatValue<'ctx> {
        let i64_t = self.context.i64_type();
        let f64_t = self.context.f64_type();
        let fn_val = self.current_fn.expect("stats fold in function");
        let acc = self.builder.build_alloca(f64_t, "stats.fold.acc").unwrap();
        // Additive seed is NEGATIVE zero to match the interpreter's
        // `xs.iter().sum::<f64>()` (Rust's float `Sum` identity is `-0.0`):
        // an empty `Stats.sum` prints `-0`, and `-0.0 + x == x` leaves every
        // non-empty result unchanged.
        let seed = if is_mul {
            f64_t.const_float(1.0)
        } else {
            f64_t.const_float(-0.0)
        };
        self.builder.build_store(acc, seed).unwrap();
        let i = self.builder.build_alloca(i64_t, "stats.fold.i").unwrap();
        self.builder.build_store(i, i64_t.const_zero()).unwrap();
        let h = self.context.append_basic_block(fn_val, "stats.fold.h");
        let b = self.context.append_basic_block(fn_val, "stats.fold.b");
        let e = self.context.append_basic_block(fn_val, "stats.fold.e");
        self.builder.build_unconditional_branch(h).unwrap();
        self.builder.position_at_end(h);
        let iv = self
            .builder
            .build_load(i64_t, i, "stats.fold.iv")
            .unwrap()
            .into_int_value();
        let more = self
            .builder
            .build_int_compare(IntPredicate::ULT, iv, len, "stats.fold.more")
            .unwrap();
        self.builder.build_conditional_branch(more, b, e).unwrap();
        self.builder.position_at_end(b);
        let x = self.stats_load(data, iv);
        let cur = self
            .builder
            .build_load(f64_t, acc, "stats.fold.cur")
            .unwrap()
            .into_float_value();
        let next = if is_mul {
            self.builder
                .build_float_mul(cur, x, "stats.fold.mul")
                .unwrap()
        } else {
            self.builder
                .build_float_add(cur, x, "stats.fold.add")
                .unwrap()
        };
        self.builder.build_store(acc, next).unwrap();
        self.builder
            .build_store(
                i,
                self.builder
                    .build_int_add(iv, i64_t.const_int(1, false), "stats.fold.i2")
                    .unwrap(),
            )
            .unwrap();
        self.builder.build_unconditional_branch(h).unwrap();
        self.builder.position_at_end(e);
        self.builder
            .build_load(f64_t, acc, "stats.fold.out")
            .unwrap()
            .into_float_value()
    }

    /// `mean` = `sum / n`; empty input traps.
    fn stats_mean(
        &mut self,
        data: PointerValue<'ctx>,
        len: IntValue<'ctx>,
    ) -> Result<FloatValue<'ctx>, String> {
        let i64_t = self.context.i64_type();
        let f64_t = self.context.f64_type();
        let nonempty = self
            .builder
            .build_int_compare(IntPredicate::UGT, len, i64_t.const_zero(), "stats.mean.ne")
            .unwrap();
        self.emit_column_guard(nonempty, "Stats.mean() called on empty slice")?;
        let sum = self.stats_fold(data, len, false);
        let nf = self
            .builder
            .build_unsigned_int_to_float(len, f64_t, "stats.mean.nf")
            .unwrap();
        Ok(self.builder.build_float_div(sum, nf, "stats.mean").unwrap())
    }

    /// Population `variance` = `Σ(xᵢ − mean)² / n`; empty input traps.
    fn stats_variance(
        &mut self,
        data: PointerValue<'ctx>,
        len: IntValue<'ctx>,
    ) -> Result<FloatValue<'ctx>, String> {
        let i64_t = self.context.i64_type();
        let f64_t = self.context.f64_type();
        let nonempty = self
            .builder
            .build_int_compare(IntPredicate::UGT, len, i64_t.const_zero(), "stats.var.ne")
            .unwrap();
        self.emit_column_guard(nonempty, "Stats.variance() called on empty slice")?;
        let mean = self.stats_mean(data, len)?;
        let fn_val = self.current_fn.expect("stats variance in function");
        let acc = self.builder.build_alloca(f64_t, "stats.var.acc").unwrap();
        self.builder.build_store(acc, f64_t.const_zero()).unwrap();
        let i = self.builder.build_alloca(i64_t, "stats.var.i").unwrap();
        self.builder.build_store(i, i64_t.const_zero()).unwrap();
        let h = self.context.append_basic_block(fn_val, "stats.var.h");
        let b = self.context.append_basic_block(fn_val, "stats.var.b");
        let e = self.context.append_basic_block(fn_val, "stats.var.e");
        self.builder.build_unconditional_branch(h).unwrap();
        self.builder.position_at_end(h);
        let iv = self
            .builder
            .build_load(i64_t, i, "stats.var.iv")
            .unwrap()
            .into_int_value();
        let more = self
            .builder
            .build_int_compare(IntPredicate::ULT, iv, len, "stats.var.more")
            .unwrap();
        self.builder.build_conditional_branch(more, b, e).unwrap();
        self.builder.position_at_end(b);
        let x = self.stats_load(data, iv);
        let d = self
            .builder
            .build_float_sub(x, mean, "stats.var.d")
            .unwrap();
        let sq = self.builder.build_float_mul(d, d, "stats.var.sq").unwrap();
        let cur = self
            .builder
            .build_load(f64_t, acc, "stats.var.cur")
            .unwrap()
            .into_float_value();
        self.builder
            .build_store(
                acc,
                self.builder
                    .build_float_add(cur, sq, "stats.var.a2")
                    .unwrap(),
            )
            .unwrap();
        self.builder
            .build_store(
                i,
                self.builder
                    .build_int_add(iv, i64_t.const_int(1, false), "stats.var.i2")
                    .unwrap(),
            )
            .unwrap();
        self.builder.build_unconditional_branch(h).unwrap();
        self.builder.position_at_end(e);
        let total = self
            .builder
            .build_load(f64_t, acc, "stats.var.total")
            .unwrap()
            .into_float_value();
        let nf = self
            .builder
            .build_unsigned_int_to_float(len, f64_t, "stats.var.nf")
            .unwrap();
        Ok(self
            .builder
            .build_float_div(total, nf, "stats.var")
            .unwrap())
    }

    /// `median` — copy the buffer into a fresh scratch alloc, sort it, take the
    /// middle (or the mean of the two middles for an even count), then free the
    /// scratch. Never mutates the caller's slice. Empty input traps.
    fn stats_median(
        &mut self,
        data: PointerValue<'ctx>,
        len: IntValue<'ctx>,
    ) -> Result<FloatValue<'ctx>, String> {
        let i64_t = self.context.i64_type();
        let f64_t = self.context.f64_type();
        let nonempty = self
            .builder
            .build_int_compare(IntPredicate::UGT, len, i64_t.const_zero(), "stats.med.ne")
            .unwrap();
        self.emit_column_guard(nonempty, "Stats.median() called on empty slice")?;

        let nbytes = self
            .builder
            .build_int_mul(len, i64_t.const_int(8, false), "stats.med.nb")
            .unwrap();
        let buf = self
            .builder
            .build_call(self.malloc_fn, &[nbytes.into()], "stats.med.buf")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        // memcpy the f64 data into the scratch (so the source Vec is untouched).
        self.builder
            .build_memcpy(buf, 8, data, 8, nbytes)
            .map_err(|e| format!("stats median memcpy failed: {e:?}"))?;
        self.column_sort_f64_inplace(buf, len);

        // mid = len / 2; even → (buf[mid-1] + buf[mid]) / 2, odd → buf[mid].
        let mid = self
            .builder
            .build_int_unsigned_div(len, i64_t.const_int(2, false), "stats.med.mid")
            .unwrap();
        let rem = self
            .builder
            .build_int_unsigned_rem(len, i64_t.const_int(2, false), "stats.med.rem")
            .unwrap();
        let is_odd = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                rem,
                i64_t.const_int(1, false),
                "stats.med.odd",
            )
            .unwrap();
        let hi = self.stats_load(buf, mid);
        let lo_idx = self
            .builder
            .build_int_sub(mid, i64_t.const_int(1, false), "stats.med.loi")
            .unwrap();
        let lo = self.stats_load(buf, lo_idx);
        let avg = {
            let s = self.builder.build_float_add(lo, hi, "stats.med.s").unwrap();
            self.builder
                .build_float_div(s, f64_t.const_float(2.0), "stats.med.avg")
                .unwrap()
        };
        let median = self
            .builder
            .build_select(is_odd, hi, avg, "stats.med.sel")
            .unwrap()
            .into_float_value();
        self.builder
            .build_call(self.free_fn, &[buf.into()], "stats.med.free")
            .unwrap();
        Ok(median)
    }

    /// `min` / `max` → `Option[f64]`. Empty input is `None`; otherwise fold the
    /// buffer (seeded with element 0, matching the interpreter's `reduce`).
    fn stats_minmax(
        &self,
        data: PointerValue<'ctx>,
        len: IntValue<'ctx>,
        is_max: bool,
    ) -> BasicValueEnum<'ctx> {
        let i64_t = self.context.i64_type();
        let f64_t = self.context.f64_type();
        let fn_val = self.current_fn.expect("stats minmax in function");
        let nonempty = self
            .builder
            .build_int_compare(IntPredicate::UGT, len, i64_t.const_zero(), "stats.mm.ne")
            .unwrap();
        let some_bb = self.context.append_basic_block(fn_val, "stats.mm.some");
        let none_bb = self.context.append_basic_block(fn_val, "stats.mm.none");
        let merge_bb = self.context.append_basic_block(fn_val, "stats.mm.merge");
        self.builder
            .build_conditional_branch(nonempty, some_bb, none_bb)
            .unwrap();

        // Non-empty: seed = data[0], fold from index 1.
        self.builder.position_at_end(some_bb);
        let acc = self.builder.build_alloca(f64_t, "stats.mm.acc").unwrap();
        let seed = self.stats_load(data, i64_t.const_zero());
        self.builder.build_store(acc, seed).unwrap();
        let i = self.builder.build_alloca(i64_t, "stats.mm.i").unwrap();
        self.builder
            .build_store(i, i64_t.const_int(1, false))
            .unwrap();
        let h = self.context.append_basic_block(fn_val, "stats.mm.h");
        let b = self.context.append_basic_block(fn_val, "stats.mm.b");
        let e = self.context.append_basic_block(fn_val, "stats.mm.e");
        self.builder.build_unconditional_branch(h).unwrap();
        self.builder.position_at_end(h);
        let iv = self
            .builder
            .build_load(i64_t, i, "stats.mm.iv")
            .unwrap()
            .into_int_value();
        let more = self
            .builder
            .build_int_compare(IntPredicate::ULT, iv, len, "stats.mm.more")
            .unwrap();
        self.builder.build_conditional_branch(more, b, e).unwrap();
        self.builder.position_at_end(b);
        let x = self.stats_load(data, iv);
        let cur = self
            .builder
            .build_load(f64_t, acc, "stats.mm.cur")
            .unwrap()
            .into_float_value();
        // `x < cur` (min) / `x > cur` (max) → take x, matching `f64::min`/`max`
        // (a NaN comparison is false, so the accumulator is retained).
        let pred = if is_max {
            FloatPredicate::OGT
        } else {
            FloatPredicate::OLT
        };
        let take = self
            .builder
            .build_float_compare(pred, x, cur, "stats.mm.take")
            .unwrap();
        let next = self
            .builder
            .build_select(take, x, cur, "stats.mm.next")
            .unwrap()
            .into_float_value();
        self.builder.build_store(acc, next).unwrap();
        self.builder
            .build_store(
                i,
                self.builder
                    .build_int_add(iv, i64_t.const_int(1, false), "stats.mm.i2")
                    .unwrap(),
            )
            .unwrap();
        self.builder.build_unconditional_branch(h).unwrap();
        self.builder.position_at_end(e);
        let result = self
            .builder
            .build_load(f64_t, acc, "stats.mm.res")
            .unwrap()
            .into_float_value();
        let word = self
            .builder
            .build_bit_cast(result, i64_t, "stats.mm.word")
            .unwrap()
            .into_int_value();
        let some_end_bb = self.builder.get_insert_block().unwrap();
        self.builder.build_unconditional_branch(merge_bb).unwrap();

        self.builder.position_at_end(none_bb);
        self.builder.build_unconditional_branch(merge_bb).unwrap();

        self.builder.position_at_end(merge_bb);
        self.build_option_some_via_phis(&[word], some_end_bb, none_bb, "stats.mm")
    }

    /// Load `data[i]` as `f64` from a contiguous slice/Vec buffer.
    fn stats_load(&self, data: PointerValue<'ctx>, i: IntValue<'ctx>) -> FloatValue<'ctx> {
        let f64_t = self.context.f64_type();
        let slot = unsafe {
            self.builder
                .build_gep(f64_t, data, &[i], "stats.slot")
                .unwrap()
        };
        self.builder
            .build_load(f64_t, slot, "stats.elem")
            .unwrap()
            .into_float_value()
    }
}
