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
            "sum"
                | "prod"
                | "mean"
                | "variance"
                | "stddev"
                | "median"
                | "min"
                | "max"
                | "percentile"
                | "argmin"
                | "argmax"
                | "sort"
                | "argsort"
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
            "percentile" => {
                let p_arg = args
                    .get(1)
                    .ok_or_else(|| "Stats.percentile expects (slice, p)".to_string())?;
                let p_val = self.compile_expr(&p_arg.value)?;
                let p = match p_val {
                    BasicValueEnum::FloatValue(f) => f,
                    BasicValueEnum::IntValue(iv) => self
                        .builder
                        .build_signed_int_to_float(iv, self.context.f64_type(), "stats.pct.pf")
                        .unwrap(),
                    _ => return Err("Stats.percentile p must be numeric".to_string()),
                };
                self.stats_percentile(data, len, p)?.into()
            }
            "argmin" => self.stats_argminmax(data, len, false),
            "argmax" => self.stats_argminmax(data, len, true),
            "sort" => self.stats_sort(data, len)?,
            "argsort" => self.stats_argsort(data, len)?,
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

    /// `percentile(p)` — NumPy `np.percentile` convention: `p ∈ [0, 100]`,
    /// linear interpolation between ranks (`median ≡ percentile(50)`). Empty
    /// slice or out-of-range `p` traps. Copies the buffer into a fresh scratch,
    /// sorts it (never mutating the caller's Vec), interpolates, then frees it.
    fn stats_percentile(
        &mut self,
        data: PointerValue<'ctx>,
        len: IntValue<'ctx>,
        p: FloatValue<'ctx>,
    ) -> Result<FloatValue<'ctx>, String> {
        let i64_t = self.context.i64_type();
        let f64_t = self.context.f64_type();
        let nonempty = self
            .builder
            .build_int_compare(IntPredicate::UGT, len, i64_t.const_zero(), "stats.pct.ne")
            .unwrap();
        self.emit_column_guard(nonempty, "Stats.percentile() called on empty slice")?;
        let ge0 = self
            .builder
            .build_float_compare(FloatPredicate::OGE, p, f64_t.const_zero(), "stats.pct.ge0")
            .unwrap();
        let le100 = self
            .builder
            .build_float_compare(
                FloatPredicate::OLE,
                p,
                f64_t.const_float(100.0),
                "stats.pct.le100",
            )
            .unwrap();
        let inrange = self
            .builder
            .build_and(ge0, le100, "stats.pct.inrange")
            .unwrap();
        self.emit_column_guard(inrange, "Stats.percentile() p must be in [0, 100]")?;

        let nbytes = self
            .builder
            .build_int_mul(len, i64_t.const_int(8, false), "stats.pct.nb")
            .unwrap();
        let buf = self
            .builder
            .build_call(self.malloc_fn, &[nbytes.into()], "stats.pct.buf")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        self.builder
            .build_memcpy(buf, 8, data, 8, nbytes)
            .map_err(|e| format!("stats percentile memcpy failed: {e:?}"))?;
        self.column_sort_f64_inplace(buf, len);

        // pos = (p / 100) · (n - 1); lo = ⌊pos⌋ (fptoui, pos ≥ 0);
        // hi = (lo+1 < n) ? lo+1 : lo; result = buf[lo] + frac·(buf[hi]-buf[lo]).
        let nf = self
            .builder
            .build_unsigned_int_to_float(len, f64_t, "stats.pct.nf")
            .unwrap();
        let nm1 = self
            .builder
            .build_float_sub(nf, f64_t.const_float(1.0), "stats.pct.nm1")
            .unwrap();
        let frac100 = self
            .builder
            .build_float_div(p, f64_t.const_float(100.0), "stats.pct.f100")
            .unwrap();
        let pos = self
            .builder
            .build_float_mul(frac100, nm1, "stats.pct.pos")
            .unwrap();
        let lo = self
            .builder
            .build_float_to_unsigned_int(pos, i64_t, "stats.pct.lo")
            .unwrap();
        let lo1 = self
            .builder
            .build_int_add(lo, i64_t.const_int(1, false), "stats.pct.lo1")
            .unwrap();
        let lt = self
            .builder
            .build_int_compare(IntPredicate::ULT, lo1, len, "stats.pct.lt")
            .unwrap();
        let hi = self
            .builder
            .build_select(lt, lo1, lo, "stats.pct.hi")
            .unwrap()
            .into_int_value();
        let lof = self
            .builder
            .build_unsigned_int_to_float(lo, f64_t, "stats.pct.lof")
            .unwrap();
        let fr = self
            .builder
            .build_float_sub(pos, lof, "stats.pct.fr")
            .unwrap();
        let blo = self.stats_load(buf, lo);
        let bhi = self.stats_load(buf, hi);
        let diff = self
            .builder
            .build_float_sub(bhi, blo, "stats.pct.diff")
            .unwrap();
        let scaled = self
            .builder
            .build_float_mul(fr, diff, "stats.pct.scaled")
            .unwrap();
        let res = self
            .builder
            .build_float_add(blo, scaled, "stats.pct.res")
            .unwrap();
        self.builder
            .build_call(self.free_fn, &[buf.into()], "stats.pct.free")
            .unwrap();
        Ok(res)
    }

    /// `argmin` / `argmax` → `Option[i64]`: the index of the FIRST min / max,
    /// or `None` on an empty slice (mirroring `min`/`max`). Strict `<` / `>`
    /// keeps the first occurrence (a later equal value never displaces it).
    fn stats_argminmax(
        &self,
        data: PointerValue<'ctx>,
        len: IntValue<'ctx>,
        is_max: bool,
    ) -> BasicValueEnum<'ctx> {
        let i64_t = self.context.i64_type();
        let fn_val = self.current_fn.expect("stats argminmax in function");
        let nonempty = self
            .builder
            .build_int_compare(IntPredicate::UGT, len, i64_t.const_zero(), "stats.am.ne")
            .unwrap();
        let some_bb = self.context.append_basic_block(fn_val, "stats.am.some");
        let none_bb = self.context.append_basic_block(fn_val, "stats.am.none");
        let merge_bb = self.context.append_basic_block(fn_val, "stats.am.merge");
        self.builder
            .build_conditional_branch(nonempty, some_bb, none_bb)
            .unwrap();

        self.builder.position_at_end(some_bb);
        let bi = self.builder.build_alloca(i64_t, "stats.am.bi").unwrap();
        self.builder.build_store(bi, i64_t.const_zero()).unwrap();
        let i = self.builder.build_alloca(i64_t, "stats.am.i").unwrap();
        self.builder
            .build_store(i, i64_t.const_int(1, false))
            .unwrap();
        let h = self.context.append_basic_block(fn_val, "stats.am.h");
        let b = self.context.append_basic_block(fn_val, "stats.am.b");
        let e = self.context.append_basic_block(fn_val, "stats.am.e");
        self.builder.build_unconditional_branch(h).unwrap();
        self.builder.position_at_end(h);
        let iv = self
            .builder
            .build_load(i64_t, i, "stats.am.iv")
            .unwrap()
            .into_int_value();
        let more = self
            .builder
            .build_int_compare(IntPredicate::ULT, iv, len, "stats.am.more")
            .unwrap();
        self.builder.build_conditional_branch(more, b, e).unwrap();
        self.builder.position_at_end(b);
        let x = self.stats_load(data, iv);
        let biv = self
            .builder
            .build_load(i64_t, bi, "stats.am.biv")
            .unwrap()
            .into_int_value();
        let bx = self.stats_load(data, biv);
        let pred = if is_max {
            FloatPredicate::OGT
        } else {
            FloatPredicate::OLT
        };
        let take = self
            .builder
            .build_float_compare(pred, x, bx, "stats.am.take")
            .unwrap();
        let newbi = self
            .builder
            .build_select(take, iv, biv, "stats.am.newbi")
            .unwrap()
            .into_int_value();
        self.builder.build_store(bi, newbi).unwrap();
        self.builder
            .build_store(
                i,
                self.builder
                    .build_int_add(iv, i64_t.const_int(1, false), "stats.am.i2")
                    .unwrap(),
            )
            .unwrap();
        self.builder.build_unconditional_branch(h).unwrap();
        self.builder.position_at_end(e);
        let result_idx = self
            .builder
            .build_load(i64_t, bi, "stats.am.res")
            .unwrap()
            .into_int_value();
        let some_end_bb = self.builder.get_insert_block().unwrap();
        self.builder.build_unconditional_branch(merge_bb).unwrap();

        self.builder.position_at_end(none_bb);
        self.builder.build_unconditional_branch(merge_bb).unwrap();

        self.builder.position_at_end(merge_bb);
        // Payload word IS the i64 index — no bit-cast needed.
        self.build_option_some_via_phis(&[result_idx], some_end_bb, none_bb, "stats.am")
    }

    /// `sort` → a fresh ascending `Vec[f64]`. Mallocs a buffer, `memcpy`s the
    /// data in (the source slice is borrowed and unchanged), sorts in place,
    /// and returns the owned `{ptr, len, len}` Vec — the binding site frees it.
    fn stats_sort(
        &self,
        data: PointerValue<'ctx>,
        len: IntValue<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let i64_t = self.context.i64_type();
        let nbytes = self
            .builder
            .build_int_mul(len, i64_t.const_int(8, false), "stats.srt.nb")
            .unwrap();
        let buf = self
            .builder
            .build_call(self.malloc_fn, &[nbytes.into()], "stats.srt.buf")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        self.builder
            .build_memcpy(buf, 8, data, 8, nbytes)
            .map_err(|e| format!("stats sort memcpy failed: {e:?}"))?;
        self.column_sort_f64_inplace(buf, len);
        Ok(self.stats_build_vec(buf, len))
    }

    /// `argsort` → a fresh `Vec[i64]` of the indices that sort the slice
    /// ascending (stable — strict `>` in the inner compare leaves equal keys in
    /// input order). Mallocs an i64 index buffer initialized to `0..n`, then
    /// insertion-sorts the indices keyed by `data[idx]`. Returns the owned Vec.
    fn stats_argsort(
        &self,
        data: PointerValue<'ctx>,
        len: IntValue<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let i64_t = self.context.i64_type();
        let fn_val = self.current_fn.expect("stats argsort in function");
        let nbytes = self
            .builder
            .build_int_mul(len, i64_t.const_int(8, false), "stats.as.nb")
            .unwrap();
        let buf = self
            .builder
            .build_call(self.malloc_fn, &[nbytes.into()], "stats.as.buf")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();

        // Initialize buf[i] = i.
        let ii = self.builder.build_alloca(i64_t, "stats.as.ii").unwrap();
        self.builder.build_store(ii, i64_t.const_zero()).unwrap();
        let ih = self.context.append_basic_block(fn_val, "stats.as.ih");
        let ib = self.context.append_basic_block(fn_val, "stats.as.ib");
        let ie = self.context.append_basic_block(fn_val, "stats.as.ie");
        self.builder.build_unconditional_branch(ih).unwrap();
        self.builder.position_at_end(ih);
        let iiv = self
            .builder
            .build_load(i64_t, ii, "stats.as.iiv")
            .unwrap()
            .into_int_value();
        let imore = self
            .builder
            .build_int_compare(IntPredicate::ULT, iiv, len, "stats.as.imore")
            .unwrap();
        self.builder
            .build_conditional_branch(imore, ib, ie)
            .unwrap();
        self.builder.position_at_end(ib);
        let islot = unsafe {
            self.builder
                .build_gep(i64_t, buf, &[iiv], "stats.as.islot")
                .unwrap()
        };
        self.builder.build_store(islot, iiv).unwrap();
        self.builder
            .build_store(
                ii,
                self.builder
                    .build_int_add(iiv, i64_t.const_int(1, false), "stats.as.ii2")
                    .unwrap(),
            )
            .unwrap();
        self.builder.build_unconditional_branch(ih).unwrap();
        self.builder.position_at_end(ie);

        // Insertion sort the indices keyed by data[idx] (stable via `>`).
        let si = self.builder.build_alloca(i64_t, "stats.as.si").unwrap();
        let sj = self.builder.build_alloca(i64_t, "stats.as.sj").unwrap();
        let key = self.builder.build_alloca(i64_t, "stats.as.key").unwrap();
        self.builder
            .build_store(si, i64_t.const_int(1, false))
            .unwrap();
        let oh = self.context.append_basic_block(fn_val, "stats.as.oh");
        let ob = self.context.append_basic_block(fn_val, "stats.as.ob");
        let inh = self.context.append_basic_block(fn_val, "stats.as.inh");
        let ick = self.context.append_basic_block(fn_val, "stats.as.ick");
        let ish = self.context.append_basic_block(fn_val, "stats.as.ish");
        let ipl = self.context.append_basic_block(fn_val, "stats.as.ipl");
        let oc = self.context.append_basic_block(fn_val, "stats.as.oc");
        let oe = self.context.append_basic_block(fn_val, "stats.as.oe");
        self.builder.build_unconditional_branch(oh).unwrap();
        self.builder.position_at_end(oh);
        let siv = self
            .builder
            .build_load(i64_t, si, "stats.as.siv")
            .unwrap()
            .into_int_value();
        let omore = self
            .builder
            .build_int_compare(IntPredicate::ULT, siv, len, "stats.as.omore")
            .unwrap();
        self.builder
            .build_conditional_branch(omore, ob, oe)
            .unwrap();
        self.builder.position_at_end(ob);
        let keyslot = unsafe {
            self.builder
                .build_gep(i64_t, buf, &[siv], "stats.as.keyslot")
                .unwrap()
        };
        let keyv = self
            .builder
            .build_load(i64_t, keyslot, "stats.as.keyv")
            .unwrap()
            .into_int_value();
        self.builder.build_store(key, keyv).unwrap();
        self.builder
            .build_store(
                sj,
                self.builder
                    .build_int_sub(siv, i64_t.const_int(1, false), "stats.as.sj0")
                    .unwrap(),
            )
            .unwrap();
        self.builder.build_unconditional_branch(inh).unwrap();
        self.builder.position_at_end(inh);
        let sjv = self
            .builder
            .build_load(i64_t, sj, "stats.as.sjv")
            .unwrap()
            .into_int_value();
        let ge0 = self
            .builder
            .build_int_compare(IntPredicate::SGE, sjv, i64_t.const_zero(), "stats.as.ge0")
            .unwrap();
        self.builder
            .build_conditional_branch(ge0, ick, ipl)
            .unwrap();
        self.builder.position_at_end(ick);
        // data[buf[sj]] vs data[key]
        let bjslot = unsafe {
            self.builder
                .build_gep(i64_t, buf, &[sjv], "stats.as.bjslot")
                .unwrap()
        };
        let bj_idx = self
            .builder
            .build_load(i64_t, bjslot, "stats.as.bjidx")
            .unwrap()
            .into_int_value();
        let bj_val = self.stats_load(data, bj_idx);
        let keyc = self
            .builder
            .build_load(i64_t, key, "stats.as.keyc")
            .unwrap()
            .into_int_value();
        let key_val = self.stats_load(data, keyc);
        let gt = self
            .builder
            .build_float_compare(FloatPredicate::OGT, bj_val, key_val, "stats.as.gt")
            .unwrap();
        self.builder.build_conditional_branch(gt, ish, ipl).unwrap();
        self.builder.position_at_end(ish);
        let sjp1 = self
            .builder
            .build_int_add(sjv, i64_t.const_int(1, false), "stats.as.sjp1")
            .unwrap();
        let dst = unsafe {
            self.builder
                .build_gep(i64_t, buf, &[sjp1], "stats.as.dst")
                .unwrap()
        };
        self.builder.build_store(dst, bj_idx).unwrap();
        self.builder
            .build_store(
                sj,
                self.builder
                    .build_int_sub(sjv, i64_t.const_int(1, false), "stats.as.sjm1")
                    .unwrap(),
            )
            .unwrap();
        self.builder.build_unconditional_branch(inh).unwrap();
        self.builder.position_at_end(ipl);
        let sjv2 = self
            .builder
            .build_load(i64_t, sj, "stats.as.sjv2")
            .unwrap()
            .into_int_value();
        let place = self
            .builder
            .build_int_add(sjv2, i64_t.const_int(1, false), "stats.as.place")
            .unwrap();
        let pslot = unsafe {
            self.builder
                .build_gep(i64_t, buf, &[place], "stats.as.pslot")
                .unwrap()
        };
        let keyf = self
            .builder
            .build_load(i64_t, key, "stats.as.keyf")
            .unwrap()
            .into_int_value();
        self.builder.build_store(pslot, keyf).unwrap();
        self.builder.build_unconditional_branch(oc).unwrap();
        self.builder.position_at_end(oc);
        self.builder
            .build_store(
                si,
                self.builder
                    .build_int_add(siv, i64_t.const_int(1, false), "stats.as.si2")
                    .unwrap(),
            )
            .unwrap();
        self.builder.build_unconditional_branch(oh).unwrap();
        self.builder.position_at_end(oe);
        Ok(self.stats_build_vec(buf, len))
    }

    /// Build an owned `Vec` value `{ buf, len, len }` (cap == len) from a
    /// malloc'd buffer. The let-binding's `Vec` cleanup frees `buf`.
    fn stats_build_vec(
        &self,
        buf: PointerValue<'ctx>,
        len: IntValue<'ctx>,
    ) -> BasicValueEnum<'ctx> {
        let vec_ty = self.vec_struct_type();
        let agg = vec_ty.const_zero();
        let agg = self
            .builder
            .build_insert_value(agg, buf, 0, "stats.vec.ptr")
            .unwrap()
            .into_struct_value();
        let agg = self
            .builder
            .build_insert_value(agg, len, 1, "stats.vec.len")
            .unwrap()
            .into_struct_value();
        let agg = self
            .builder
            .build_insert_value(agg, len, 2, "stats.vec.cap")
            .unwrap()
            .into_struct_value();
        agg.into()
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
