//! Codegen side of the Reduce/ElementwiseMap/ElementwiseOrd unification
//! (`docs/spikes/reduce-elementwise-trait-unification.md`). This module owns
//! the shared LLVM emitters that the `Tensor`, `Column`, and `Stats.*`
//! reductions funnel through, keyed on the same backend-agnostic
//! [`crate::reduce_kernel::ReduceOp`] vocabulary the interpreter twin uses
//! (S0). It is the codegen counterpart of `src/reduce_kernel.rs`.
//!
//! The three surfaces share one index-fold skeleton; the axes that genuinely
//! differ — the element source ([`ContainerAccess`], incl. the optional Arrow
//! validity `bitmap`), the element kind, and per-surface knobs (seed, empty
//! policy, result wrapping) — are parameters, not forks. **S1 migrates the
//! `sum`/`prod`/`mean` fold family and the `min`/`max` ordering family of all
//! three surfaces:** `Stats` + `Tensor` (dense, `bitmap: None`) and `Column`
//! (Arrow-nullable, `bitmap: Some` → the `*_gated` variants that fold valid
//! slots only and guard the all-null case in-emitter). **S2 adds the
//! f64-accumulator family** ([`emit_sum_f64_and_count`] +
//! [`emit_variance_from`]): `Column.mean`/`var`/`std` (sample, ÷ n−1) and
//! `Stats.variance`/`stddev` (population, ÷ n) fold their overflow-safe `f64`
//! sum through one dense-or-gated pass and share the `Var { bessel }` divisor
//! knob. **S3 adds the element-wise map family** ([`emit_elementwise_map`]):
//! Tensor `⊕`/`-t` (dense) and Column `⊕`/`-c` (validity-gated with SQL null
//! propagation) share one map skeleton, parameterized on the second operand
//! ([`MapOther`]: container / broadcast scalar / none) and the per-element op
//! ([`MapKernelOp`]: `compile_binop_typed`, or `Neg` = IEEE `fneg` / checked
//! int `0 - x` matching the interpreter's `eval_unary`). **S4 adds the
//! ordering family** ([`emit_sort_scratch`] + [`emit_reduce_argminmax`]): one
//! insertion-sort skeleton, keyed by [`SortKey`] (`Value` f64 sort vs
//! `IndexInto` stable argsort), behind `Stats.sort`/`median`/`percentile`/
//! `argsort`, `Column.median`/`quantile`, and the `DataFrame.describe`
//! quartiles; plus the first-occurrence argmin/argmax compare-select loop.
//! **S5 adds the non-f64 element axis for `Stats`**: the typechecker's
//! `infer_stats_call` records the slice element (`i64` | `f64`) in
//! `stats_elem_types`, and the `Stats.*` paths instantiate these same
//! emitters at that element type — int folds/compares stay exact
//! ([`SortKey::IntValue`]/[`SortKey::IndexIntoInt`] compare at signed i64),
//! float statistics promote through `emit_sum_f64_and_count`.
//!
//! **Byte-identical.** The emitters here reduce to the exact instructions the
//! hand-rolled loops emitted (`compile_binop_typed` lowers f64 `Add`/`Mul` to
//! `build_float_add`/`build_float_mul`, and `to_float` on an f64 is the
//! identity), so migrated surfaces keep byte-identical program output — proved
//! by the run-vs-build oracle.

use inkwell::types::BasicTypeEnum;
use inkwell::values::{BasicValueEnum, FloatValue, IntValue, PointerValue};
use inkwell::{FloatPredicate, IntPredicate};

use crate::ast::{BinOp, ClosureParam, Expr, PatternKind};
use crate::reduce_kernel::ReduceOp;

use super::state::VarSlot;

/// How a reduction reads its elements. Element `i` is `data[i]` at LLVM type
/// `elem` over `[0, len)`. The three surfaces differ only in `bitmap`:
///   * `None` — a flat, contiguous, non-nullable buffer (`Stats`' `Slice[f64]`
///     and the `Tensor` C-order data block); every slot is read.
///   * `Some(bm)` — the `Column` Apache-Arrow validity bitmap; only slots whose
///     bit is set are folded (nulls skipped, the SQL/pandas posture), and the
///     valid count drives the empty guard / `mean` divisor.
pub(super) struct ContainerAccess<'ctx> {
    /// Base pointer of the element buffer.
    pub data: PointerValue<'ctx>,
    /// Number of elements.
    pub len: IntValue<'ctx>,
    /// LLVM type of one element (`f64` for `Stats`; the tensor's / column's `T`).
    pub elem: BasicTypeEnum<'ctx>,
    /// Whether integer elements are unsigned (drives the fold's overflow
    /// semantics through `compile_binop_typed`). Ignored for float elements.
    pub unsigned: bool,
    /// The Arrow validity bitmap (`Column`), or `None` for a dense buffer
    /// (`Stats`/`Tensor`).
    pub bitmap: Option<PointerValue<'ctx>>,
}

/// The second operand of an element-wise map (S3).
pub(super) enum MapOther<'ctx> {
    /// A second container operand (tensor⊕tensor / col⊕col), loaded at its
    /// own element type each iteration.
    Access(ContainerAccess<'ctx>),
    /// A broadcast scalar; `on_left` puts it on the operator's left
    /// (`2 - t` / `2 - c`).
    Scalar {
        value: BasicValueEnum<'ctx>,
        on_left: bool,
    },
    /// No second operand (unary negation).
    Unary,
}

/// The per-element operation of an element-wise map (S3).
pub(super) enum MapKernelOp<'a> {
    /// A scalar binary op through `compile_binop_typed` — inherits the exact
    /// scalar semantics (int overflow trap, div-by-zero trap, signedness).
    Binop(&'a BinOp),
    /// Element negation with the scalar `-x` semantics (the interpreter's
    /// `eval_unary`): a true IEEE `fneg` for floats (`-0.0` for `0.0` — NOT
    /// `0.0 - x`, which loses the signed zero) and a **checked** `0 - x` for
    /// ints (traps on `MIN`, like `checked_neg`). Fixed B-2026-07-01-1/-2:
    /// Tensor `-t` used `fsub 0.0, x` (`+0.0` for `0.0`) and Column `-c`
    /// used a bare wrapping `ineg` (silent `i64::MIN` wrap) — both diverged
    /// from `karac run` at exactly those edges.
    Neg,
    /// An inline closure `|x| <body>` (S6c-2, `Column.map` / `Tensor.map`):
    /// the single parameter binds to the current element and the body is
    /// compiled in place, its value written to the destination slot — the
    /// same inline-body strategy as `Column.fold` (the native backend can't
    /// thread a closure *value*). Only the inline-literal closure form reaches
    /// here; a closure-valued local / named fn is rejected at the call site.
    Closure {
        params: &'a [ClosureParam],
        body: &'a Expr,
    },
}

/// Where an element-wise map writes (S3): the result buffer, its element
/// type (computed elements are coerced to it), and the result validity
/// bitmap for the gated (`Column`) form.
pub(super) struct MapDest<'ctx> {
    pub data: PointerValue<'ctx>,
    pub elem: BasicTypeEnum<'ctx>,
    pub bitmap: Option<PointerValue<'ctx>>,
}

/// How the shared scratch sort keys its elements (S4; i64 forms S5).
pub(super) enum SortKey<'ctx> {
    /// The buffer elements ARE the `f64` sort keys — a value sort
    /// (`Stats.sort`/`median`/`percentile`, `Column.median`/`quantile`,
    /// `DataFrame.describe` quartiles).
    Value,
    /// The buffer elements are `i64` sort keys compared at exact integer
    /// precision (signed `>`) — the `Slice[i64]` value sort (S5); no lossy
    /// float round-trip above 2⁵³.
    IntValue,
    /// The buffer holds `i64` indices into this `f64` data pointer; an
    /// element's key is `data[idx]` (`Stats.argsort`). Stable — the strict
    /// `>` inner compare leaves equal keys in input order.
    IndexInto(PointerValue<'ctx>),
    /// The buffer holds `i64` indices into this `i64` data pointer; keys
    /// compare at exact integer precision (the `Slice[i64]` argsort, S5).
    IndexIntoInt(PointerValue<'ctx>),
}

impl<'ctx> super::Codegen<'ctx> {
    /// Load element `i` from a contiguous access — `data[i]` at `elem`. This
    /// is exactly the `stats_load` / tensor `load_at` GEP-then-load, so it is
    /// byte-identical to the code it replaces.
    fn access_load(
        &self,
        access: &ContainerAccess<'ctx>,
        i: IntValue<'ctx>,
    ) -> BasicValueEnum<'ctx> {
        let slot = unsafe {
            self.builder
                .build_gep(access.elem, access.data, &[i], "kern.slot")
                .unwrap()
        };
        self.builder
            .build_load(access.elem, slot, "kern.elem")
            .unwrap()
    }

    /// The shared fold reduction over a contiguous access: `sum`/`prod`/`mean`.
    ///
    /// Seeds `acc` with `seed` (the caller picks the per-surface identity —
    /// `Stats.sum` seeds `-0.0` to match Rust's float `Sum`, `Tensor.sum`
    /// seeds `0`), then folds every element left-to-right through
    /// `compile_binop_typed` (`Add` for `Sum`/`Mean`, `Mul` for `Prod`) — so
    /// integer folds inherit the overflow trap and float folds lower to
    /// `fadd`/`fmul`. `Mean` divides the accumulated sum by the element count
    /// as `f64` and returns the quotient; `Sum`/`Prod` return the bare
    /// accumulator (element-typed).
    ///
    /// The **empty policy stays at the call site**: `Sum`/`Prod` return the
    /// seed on an empty buffer (no trap), while `Mean` (and every surface that
    /// traps on empty) guards emptiness with its own message/mechanism
    /// *before* calling this — the `Mean` division here assumes a guarded
    /// non-empty `len`.
    pub(super) fn emit_reduce_fold(
        &mut self,
        access: &ContainerAccess<'ctx>,
        op: ReduceOp,
        seed: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        // Arrow-nullable receiver (`Column`) — the validity-gated variant folds
        // valid slots only and guards the empty-valid-set case in-emitter.
        if let Some(bitmap) = access.bitmap {
            return self.emit_reduce_fold_gated(access, bitmap, op, seed);
        }
        let fold_op = match op {
            ReduceOp::Sum | ReduceOp::Mean => BinOp::Add,
            ReduceOp::Prod => BinOp::Mul,
            other => {
                return Err(format!(
                    "emit_reduce_fold: unsupported op {other:?} (fold family is Sum/Prod/Mean)"
                ))
            }
        };
        let i64_t = self.context.i64_type();
        let fn_val = self
            .current_fn
            .ok_or_else(|| "reduce fold outside function".to_string())?;

        let acc = self
            .builder
            .build_alloca(access.elem, "kern.fold.acc")
            .unwrap();
        self.builder.build_store(acc, seed).unwrap();
        let i = self.builder.build_alloca(i64_t, "kern.fold.i").unwrap();
        self.builder.build_store(i, i64_t.const_zero()).unwrap();

        let head = self.context.append_basic_block(fn_val, "kern.fold.head");
        let body = self.context.append_basic_block(fn_val, "kern.fold.body");
        let exit = self.context.append_basic_block(fn_val, "kern.fold.exit");
        self.builder.build_unconditional_branch(head).unwrap();

        self.builder.position_at_end(head);
        let iv = self
            .builder
            .build_load(i64_t, i, "kern.fold.iv")
            .unwrap()
            .into_int_value();
        let more = self
            .builder
            .build_int_compare(IntPredicate::ULT, iv, access.len, "kern.fold.more")
            .unwrap();
        self.builder
            .build_conditional_branch(more, body, exit)
            .unwrap();

        self.builder.position_at_end(body);
        let x = self.access_load(access, iv);
        let cur = self
            .builder
            .build_load(access.elem, acc, "kern.fold.cur")
            .unwrap();
        let next = self.compile_binop_typed(&fold_op, cur, x, access.unsigned)?;
        self.builder.build_store(acc, next).unwrap();
        let i2 = self
            .builder
            .build_int_add(iv, i64_t.const_int(1, false), "kern.fold.i2")
            .unwrap();
        self.builder.build_store(i, i2).unwrap();
        self.builder.build_unconditional_branch(head).unwrap();

        self.builder.position_at_end(exit);
        let total = self
            .builder
            .build_load(access.elem, acc, "kern.fold.out")
            .unwrap();

        if matches!(op, ReduceOp::Mean) {
            let f64_t = self.context.f64_type();
            let sum_f = self.to_float(total)?;
            // Promote a narrow float accumulator (an `f32` tensor sums in `f32`)
            // to f64 before the divide — otherwise `fdiv f32, f64` is a
            // type-mismatch that fails module verification, and `mean` declares
            // an f64 result regardless. An integer accumulator is already f64
            // via `to_float`'s `sitofp`.
            let sum_f = if sum_f.get_type() == self.context.f32_type() {
                self.builder
                    .build_float_ext(sum_f, f64_t, "kern.mean.sumf64")
                    .unwrap()
            } else {
                sum_f
            };
            let nf = self
                .builder
                .build_unsigned_int_to_float(access.len, f64_t, "kern.mean.nf")
                .unwrap();
            Ok(self
                .builder
                .build_float_div(sum_f, nf, "kern.mean")
                .unwrap()
                .into())
        } else {
            Ok(total)
        }
    }

    /// The shared `min`/`max` reduction over a **non-empty** contiguous access
    /// — the caller guards emptiness first (`Tensor` traps, `Stats` wraps the
    /// result in `Option` with a `None` arm for the empty case). Seeds `acc`
    /// with element 0 and folds from index 1, taking the strictly smaller
    /// (`min`) / larger (`max`) element via compare-select. A NaN comparison is
    /// false, so NaN neither displaces the accumulator nor is taken — the
    /// scalar `<`/`>` posture matching `f64::min`/`max` and the interpreter.
    /// Returns the bare element-typed extreme.
    ///
    /// For an Arrow-nullable receiver (`Column`) the validity-gated variant is
    /// used instead: it can't seed with element 0 (which may be null), so it
    /// seeds on the first valid slot via a `seeded` flag and guards the
    /// all-null case in-emitter.
    pub(super) fn emit_reduce_minmax(
        &mut self,
        access: &ContainerAccess<'ctx>,
        is_max: bool,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        if let Some(bitmap) = access.bitmap {
            return self.emit_reduce_minmax_gated(access, bitmap, is_max);
        }
        let i64_t = self.context.i64_type();
        let fn_val = self
            .current_fn
            .ok_or_else(|| "reduce minmax outside function".to_string())?;
        let is_float = access.elem.is_float_type();

        let acc = self
            .builder
            .build_alloca(access.elem, "kern.mm.acc")
            .unwrap();
        let seed = self.access_load(access, i64_t.const_zero());
        self.builder.build_store(acc, seed).unwrap();
        let i = self.builder.build_alloca(i64_t, "kern.mm.i").unwrap();
        self.builder
            .build_store(i, i64_t.const_int(1, false))
            .unwrap();

        let head = self.context.append_basic_block(fn_val, "kern.mm.head");
        let body = self.context.append_basic_block(fn_val, "kern.mm.body");
        let exit = self.context.append_basic_block(fn_val, "kern.mm.exit");
        self.builder.build_unconditional_branch(head).unwrap();

        self.builder.position_at_end(head);
        let iv = self
            .builder
            .build_load(i64_t, i, "kern.mm.iv")
            .unwrap()
            .into_int_value();
        let more = self
            .builder
            .build_int_compare(IntPredicate::ULT, iv, access.len, "kern.mm.more")
            .unwrap();
        self.builder
            .build_conditional_branch(more, body, exit)
            .unwrap();

        self.builder.position_at_end(body);
        let x = self.access_load(access, iv);
        let cur = self
            .builder
            .build_load(access.elem, acc, "kern.mm.cur")
            .unwrap();
        // `x ⋖ cur` → take x. Float uses ordered predicates (NaN → false);
        // int uses the signedness-correct predicate.
        let take = if is_float {
            let pred = if is_max {
                FloatPredicate::OGT
            } else {
                FloatPredicate::OLT
            };
            self.builder
                .build_float_compare(
                    pred,
                    x.into_float_value(),
                    cur.into_float_value(),
                    "kern.mm.cmp",
                )
                .unwrap()
        } else {
            let pred = match (is_max, access.unsigned) {
                (false, false) => IntPredicate::SLT,
                (false, true) => IntPredicate::ULT,
                (true, false) => IntPredicate::SGT,
                (true, true) => IntPredicate::UGT,
            };
            self.builder
                .build_int_compare(
                    pred,
                    x.into_int_value(),
                    cur.into_int_value(),
                    "kern.mm.cmp",
                )
                .unwrap()
        };
        let next = self
            .builder
            .build_select(take, x, cur, "kern.mm.sel")
            .unwrap();
        self.builder.build_store(acc, next).unwrap();
        let i2 = self
            .builder
            .build_int_add(iv, i64_t.const_int(1, false), "kern.mm.i2")
            .unwrap();
        self.builder.build_store(i, i2).unwrap();
        self.builder.build_unconditional_branch(head).unwrap();

        self.builder.position_at_end(exit);
        Ok(self
            .builder
            .build_load(access.elem, acc, "kern.mm.out")
            .unwrap())
    }

    /// Validity-gated fold (`Column.sum`): fold the valid slots only, tracking
    /// the valid count, then guard the empty-valid-set case in-emitter (SQL/
    /// pandas skip-nulls posture). Element-typed accumulator, bare-`T` result.
    /// `Column.mean`/`var`/`std` accumulate in `f64` and keep their own path.
    fn emit_reduce_fold_gated(
        &mut self,
        access: &ContainerAccess<'ctx>,
        bitmap: PointerValue<'ctx>,
        op: ReduceOp,
        seed: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let (fold_op, method) = match op {
            ReduceOp::Sum => (BinOp::Add, "sum"),
            ReduceOp::Prod => (BinOp::Mul, "prod"),
            other => {
                return Err(format!(
                    "emit_reduce_fold_gated: unsupported op {other:?} (element-typed gated fold \
                     is Sum/Prod; mean/var/std accumulate in f64)"
                ))
            }
        };
        let i64_t = self.context.i64_type();
        let fn_val = self
            .current_fn
            .ok_or_else(|| "gated reduce fold outside function".to_string())?;

        let acc = self
            .builder
            .build_alloca(access.elem, "kern.gf.acc")
            .unwrap();
        self.builder.build_store(acc, seed).unwrap();
        let idx = self.builder.build_alloca(i64_t, "kern.gf.i").unwrap();
        self.builder.build_store(idx, i64_t.const_zero()).unwrap();
        let cnt = self.builder.build_alloca(i64_t, "kern.gf.cnt").unwrap();
        self.builder.build_store(cnt, i64_t.const_zero()).unwrap();

        let head = self.context.append_basic_block(fn_val, "kern.gf.head");
        let body = self.context.append_basic_block(fn_val, "kern.gf.body");
        let add = self.context.append_basic_block(fn_val, "kern.gf.add");
        let cont = self.context.append_basic_block(fn_val, "kern.gf.cont");
        let exit = self.context.append_basic_block(fn_val, "kern.gf.exit");
        self.builder.build_unconditional_branch(head).unwrap();

        self.builder.position_at_end(head);
        let iv = self
            .builder
            .build_load(i64_t, idx, "kern.gf.iv")
            .unwrap()
            .into_int_value();
        let more = self
            .builder
            .build_int_compare(IntPredicate::ULT, iv, access.len, "kern.gf.more")
            .unwrap();
        self.builder
            .build_conditional_branch(more, body, exit)
            .unwrap();

        self.builder.position_at_end(body);
        let valid = self.column_load_valid_bit(bitmap, iv);
        self.builder
            .build_conditional_branch(valid, add, cont)
            .unwrap();

        self.builder.position_at_end(add);
        let x = self.access_load(access, iv);
        let a = self
            .builder
            .build_load(access.elem, acc, "kern.gf.a")
            .unwrap();
        let a2 = self.compile_binop_typed(&fold_op, a, x, access.unsigned)?;
        self.builder.build_store(acc, a2).unwrap();
        let c = self
            .builder
            .build_load(i64_t, cnt, "kern.gf.c")
            .unwrap()
            .into_int_value();
        let c2 = self
            .builder
            .build_int_add(c, i64_t.const_int(1, false), "kern.gf.c2")
            .unwrap();
        self.builder.build_store(cnt, c2).unwrap();
        self.builder.build_unconditional_branch(cont).unwrap();

        self.builder.position_at_end(cont);
        let next = self
            .builder
            .build_int_add(iv, i64_t.const_int(1, false), "kern.gf.next")
            .unwrap();
        self.builder.build_store(idx, next).unwrap();
        self.builder.build_unconditional_branch(head).unwrap();

        self.builder.position_at_end(exit);
        let cnt_v = self
            .builder
            .build_load(i64_t, cnt, "kern.gf.cntv")
            .unwrap()
            .into_int_value();
        let ok = self
            .builder
            .build_int_compare(IntPredicate::UGT, cnt_v, i64_t.const_zero(), "kern.gf.ok")
            .unwrap();
        self.emit_column_guard(
            ok,
            &format!("cannot compute `{method}` on a column with no valid values"),
        )?;
        Ok(self
            .builder
            .build_load(access.elem, acc, "kern.gf.result")
            .unwrap())
    }

    /// Validity-gated `min`/`max` (`Column.min`/`max`): seed on the first valid
    /// slot via a `seeded` flag (nulls may precede it, so element 0 can't seed),
    /// take the strictly smaller/larger valid element via compare-select, and
    /// guard the all-null case in-emitter. Bare-`T` result.
    fn emit_reduce_minmax_gated(
        &mut self,
        access: &ContainerAccess<'ctx>,
        bitmap: PointerValue<'ctx>,
        is_max: bool,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let i64_t = self.context.i64_type();
        let bool_t = self.context.bool_type();
        let fn_val = self
            .current_fn
            .ok_or_else(|| "gated reduce minmax outside function".to_string())?;
        // Dummy seed (overwritten on the first valid slot via the `seeded`
        // flag); `const_zero` matches `column_zero_elem`.
        let zero = match access.elem {
            BasicTypeEnum::FloatType(ft) => ft.const_zero().into(),
            BasicTypeEnum::IntType(it) => it.const_zero().into(),
            other => other.const_zero(),
        };

        let idx = self.builder.build_alloca(i64_t, "kern.gm.i").unwrap();
        let acc = self
            .builder
            .build_alloca(access.elem, "kern.gm.acc")
            .unwrap();
        let seeded = self.builder.build_alloca(bool_t, "kern.gm.seeded").unwrap();
        self.builder.build_store(idx, i64_t.const_zero()).unwrap();
        self.builder.build_store(acc, zero).unwrap();
        self.builder
            .build_store(seeded, bool_t.const_zero())
            .unwrap();

        let head = self.context.append_basic_block(fn_val, "kern.gm.head");
        let body = self.context.append_basic_block(fn_val, "kern.gm.body");
        let upd = self.context.append_basic_block(fn_val, "kern.gm.upd");
        let cont = self.context.append_basic_block(fn_val, "kern.gm.cont");
        let exit = self.context.append_basic_block(fn_val, "kern.gm.exit");
        self.builder.build_unconditional_branch(head).unwrap();

        self.builder.position_at_end(head);
        let iv = self
            .builder
            .build_load(i64_t, idx, "kern.gm.iv")
            .unwrap()
            .into_int_value();
        let more = self
            .builder
            .build_int_compare(IntPredicate::ULT, iv, access.len, "kern.gm.more")
            .unwrap();
        self.builder
            .build_conditional_branch(more, body, exit)
            .unwrap();

        self.builder.position_at_end(body);
        let valid = self.column_load_valid_bit(bitmap, iv);
        self.builder
            .build_conditional_branch(valid, upd, cont)
            .unwrap();

        self.builder.position_at_end(upd);
        let x = self.access_load(access, iv);
        let cur = self
            .builder
            .build_load(access.elem, acc, "kern.gm.cur")
            .unwrap();
        let s = self
            .builder
            .build_load(bool_t, seeded, "kern.gm.s")
            .unwrap()
            .into_int_value();
        // Strict compare `x ⋖ cur`; take unconditionally when not yet seeded.
        let cmp_op = if is_max { BinOp::Gt } else { BinOp::Lt };
        let cmp = self
            .compile_binop_typed(&cmp_op, x, cur, access.unsigned)?
            .into_int_value();
        let not_seeded = self.builder.build_not(s, "kern.gm.ns").unwrap();
        let take = self
            .builder
            .build_or(not_seeded, cmp, "kern.gm.take")
            .unwrap();
        let newacc = self
            .builder
            .build_select(take, x, cur, "kern.gm.new")
            .unwrap();
        self.builder.build_store(acc, newacc).unwrap();
        self.builder
            .build_store(seeded, bool_t.const_int(1, false))
            .unwrap();
        self.builder.build_unconditional_branch(cont).unwrap();

        self.builder.position_at_end(cont);
        let next = self
            .builder
            .build_int_add(iv, i64_t.const_int(1, false), "kern.gm.next")
            .unwrap();
        self.builder.build_store(idx, next).unwrap();
        self.builder.build_unconditional_branch(head).unwrap();

        self.builder.position_at_end(exit);
        let s = self
            .builder
            .build_load(bool_t, seeded, "kern.gm.sf")
            .unwrap()
            .into_int_value();
        let method = if is_max { "max" } else { "min" };
        self.emit_column_guard(
            s,
            &format!("cannot compute `{method}` on a column with no valid values"),
        )?;
        Ok(self
            .builder
            .build_load(access.elem, acc, "kern.gm.result")
            .unwrap())
    }

    /// The shared element-wise map loop (S3) — one pass writing
    /// `dest[i] = op(lhs[i], other[i])` over `[0, lhs.len)`. One skeleton
    /// behind Tensor `⊕`/`-t` (dense) and Column `⊕`/`-c` (Arrow-nullable);
    /// the genuinely-different axes are parameters:
    ///   * **validity** — gated iff any operand access carries a `bitmap`.
    ///     A result slot is valid iff **all** gated operands are valid at
    ///     `i` (SQL null propagation): the bit-AND is stamped into
    ///     `dest.bitmap`, then only the valid branch computes — so a null
    ///     slot's placeholder never trips a div-by-zero / overflow trap —
    ///     and the invalid branch stores a zero placeholder (never read;
    ///     the bitmap masks it). Dense mode has no validity state at all.
    ///   * **the second operand** ([`MapOther`]) — container / broadcast
    ///     scalar (`on_left` for `2 - t`) / none.
    ///   * **the per-element op** ([`MapKernelOp`]) — `Binop` via
    ///     `compile_binop_typed`, or `Neg` with the scalar `-x` semantics
    ///     (IEEE `fneg` for floats, checked `0 - x` for ints) that both
    ///     Tensor `-t` and Column `-c` route through.
    ///
    /// The computed element is coerced to `dest.elem` via
    /// `coerce_scalar_to_type` (identity when the types already match —
    /// every Tensor case). Allocation, shape/length guards, and fresh-temp
    /// frees stay at the call sites.
    pub(super) fn emit_elementwise_map(
        &mut self,
        lhs: &ContainerAccess<'ctx>,
        other: &MapOther<'ctx>,
        op: &MapKernelOp<'_>,
        dest: &MapDest<'ctx>,
    ) -> Result<(), String> {
        let i64_t = self.context.i64_type();
        let fn_val = self
            .current_fn
            .ok_or_else(|| "elementwise map outside function".to_string())?;
        let gated =
            lhs.bitmap.is_some() || matches!(other, MapOther::Access(a) if a.bitmap.is_some());
        if gated && dest.bitmap.is_none() {
            return Err("elementwise map: gated operands need a dest bitmap".to_string());
        }

        let idx = self.builder.build_alloca(i64_t, "kern.map.i").unwrap();
        self.builder.build_store(idx, i64_t.const_zero()).unwrap();
        let head = self.context.append_basic_block(fn_val, "kern.map.head");
        let body = self.context.append_basic_block(fn_val, "kern.map.body");
        let comp = self.context.append_basic_block(fn_val, "kern.map.comp");
        // `skip` exists only in gated mode (the null-slot placeholder arm).
        let skip = if gated {
            Some(self.context.append_basic_block(fn_val, "kern.map.skip"))
        } else {
            None
        };
        let cont = self.context.append_basic_block(fn_val, "kern.map.cont");
        let exit = self.context.append_basic_block(fn_val, "kern.map.exit");
        self.builder.build_unconditional_branch(head).unwrap();

        self.builder.position_at_end(head);
        let iv = self
            .builder
            .build_load(i64_t, idx, "kern.map.iv")
            .unwrap()
            .into_int_value();
        let more = self
            .builder
            .build_int_compare(IntPredicate::ULT, iv, lhs.len, "kern.map.more")
            .unwrap();
        self.builder
            .build_conditional_branch(more, body, exit)
            .unwrap();

        self.builder.position_at_end(body);
        if gated {
            // Valid iff every gated operand is valid at `i` — AND the bits,
            // stamp the result bitmap, branch compute-vs-placeholder.
            let mut valid: Option<IntValue<'ctx>> = None;
            if let Some(bm) = lhs.bitmap {
                valid = Some(self.column_load_valid_bit(bm, iv));
            }
            if let MapOther::Access(a) = other {
                if let Some(bm) = a.bitmap {
                    let v = self.column_load_valid_bit(bm, iv);
                    valid = Some(match valid {
                        Some(prev) => self.builder.build_and(prev, v, "kern.map.both").unwrap(),
                        None => v,
                    });
                }
            }
            let valid = valid.expect("gated implies at least one bitmap");
            self.column_write_bit_runtime(dest.bitmap.unwrap(), iv, valid);
            self.builder
                .build_conditional_branch(valid, comp, skip.unwrap())
                .unwrap();
        } else {
            self.builder.build_unconditional_branch(comp).unwrap();
        }

        self.builder.position_at_end(comp);
        let a = self.access_load(lhs, iv);
        let r = match (op, other) {
            (MapKernelOp::Binop(bin), MapOther::Access(acc)) => {
                let b = self.access_load(acc, iv);
                self.compile_binop_typed(bin, a, b, lhs.unsigned)?
            }
            (MapKernelOp::Binop(bin), MapOther::Scalar { value, on_left }) => {
                let (l, r) = if *on_left { (*value, a) } else { (a, *value) };
                self.compile_binop_typed(bin, l, r, lhs.unsigned)?
            }
            (MapKernelOp::Binop(_), MapOther::Unary) => {
                return Err("elementwise map: binop needs a second operand".to_string())
            }
            (MapKernelOp::Neg, MapOther::Unary) => match a {
                // True IEEE negation — `-0.0` for `0.0` (a `0.0 - x` would
                // lose the signed zero; B-2026-07-01-1).
                BasicValueEnum::FloatValue(fv) => self
                    .builder
                    .build_float_neg(fv, "kern.map.fneg")
                    .unwrap()
                    .into(),
                // Checked `0 - x` — traps on `MIN` like the interpreter's
                // `checked_neg` (a bare `ineg` silently wraps;
                // B-2026-07-01-2).
                BasicValueEnum::IntValue(int_v) => {
                    let zero: BasicValueEnum<'ctx> = int_v.get_type().const_zero().into();
                    self.compile_binop_typed(&BinOp::Sub, zero, a, lhs.unsigned)?
                }
                other_v => other_v,
            },
            (MapKernelOp::Neg, _) => return Err("elementwise map: neg is unary".to_string()),
            (MapKernelOp::Closure { params, body }, MapOther::Unary) => {
                // Bind the closure's single param to the current element, then
                // compile the body in place (captures resolve through the
                // enclosing scope). Save/restore any shadowed outer binding so
                // the loop's own scope stays contained — mirrors `Column.fold`.
                let pname = match &params[0].pattern.kind {
                    PatternKind::Binding(n) => n.clone(),
                    _ => "_map_p0".to_string(),
                };
                let saved = self.variables.get(&pname).copied();
                let param_slot = self.create_entry_alloca(fn_val, &pname, lhs.elem);
                self.builder.build_store(param_slot, a).unwrap();
                self.variables.insert(
                    pname.clone(),
                    VarSlot {
                        ptr: param_slot,
                        ty: lhs.elem,
                    },
                );
                let result = self.compile_expr(body)?;
                match saved {
                    Some(s) => {
                        self.variables.insert(pname.clone(), s);
                    }
                    None => {
                        self.variables.remove(&pname);
                    }
                }
                result
            }
            (MapKernelOp::Closure { params, body }, MapOther::Access(acc)) => {
                // The BINARY closure form (`zip_with(other, |a, b| ...)`): bind
                // param 0 to this container's element and param 1 to the other
                // container's element at the same index, then inline the body
                // (captures resolve through the enclosing scope). Save/restore
                // both shadowed outer bindings — same inline-body strategy as
                // the unary `map` above.
                let b = self.access_load(acc, iv);
                let p0 = match &params[0].pattern.kind {
                    PatternKind::Binding(n) => n.clone(),
                    _ => "_zip_p0".to_string(),
                };
                let p1 = match &params[1].pattern.kind {
                    PatternKind::Binding(n) => n.clone(),
                    _ => "_zip_p1".to_string(),
                };
                let saved0 = self.variables.get(&p0).copied();
                let saved1 = self.variables.get(&p1).copied();
                let slot0 = self.create_entry_alloca(fn_val, &p0, lhs.elem);
                self.builder.build_store(slot0, a).unwrap();
                self.variables.insert(
                    p0.clone(),
                    VarSlot {
                        ptr: slot0,
                        ty: lhs.elem,
                    },
                );
                let slot1 = self.create_entry_alloca(fn_val, &p1, acc.elem);
                self.builder.build_store(slot1, b).unwrap();
                self.variables.insert(
                    p1.clone(),
                    VarSlot {
                        ptr: slot1,
                        ty: acc.elem,
                    },
                );
                let result = self.compile_expr(body)?;
                match saved1 {
                    Some(s) => {
                        self.variables.insert(p1.clone(), s);
                    }
                    None => {
                        self.variables.remove(&p1);
                    }
                }
                match saved0 {
                    Some(s) => {
                        self.variables.insert(p0.clone(), s);
                    }
                    None => {
                        self.variables.remove(&p0);
                    }
                }
                result
            }
            (MapKernelOp::Closure { .. }, MapOther::Scalar { .. }) => {
                return Err(
                    "elementwise map: a closure map takes no broadcast scalar operand".to_string(),
                )
            }
        };
        let r = self.coerce_scalar_to_type(r, dest.elem);
        let rp = unsafe {
            self.builder
                .build_gep(dest.elem, dest.data, &[iv], "kern.map.rp")
                .unwrap()
        };
        self.builder.build_store(rp, r).unwrap();
        self.builder.build_unconditional_branch(cont).unwrap();

        if let Some(skip_bb) = skip {
            // Null slot: zero placeholder (matches `column_zero_elem`).
            self.builder.position_at_end(skip_bb);
            let zero: BasicValueEnum<'ctx> = match dest.elem {
                BasicTypeEnum::FloatType(ft) => ft.const_zero().into(),
                BasicTypeEnum::IntType(it) => it.const_zero().into(),
                other_t => other_t.const_zero(),
            };
            let zp = unsafe {
                self.builder
                    .build_gep(dest.elem, dest.data, &[iv], "kern.map.zp")
                    .unwrap()
            };
            self.builder.build_store(zp, zero).unwrap();
            self.builder.build_unconditional_branch(cont).unwrap();
        }

        self.builder.position_at_end(cont);
        let next = self
            .builder
            .build_int_add(iv, i64_t.const_int(1, false), "kern.map.next")
            .unwrap();
        self.builder.build_store(idx, next).unwrap();
        self.builder.build_unconditional_branch(head).unwrap();

        self.builder.position_at_end(exit);
        Ok(())
    }

    /// One pass over `access` accumulating `(Σ x as f64, count)`. A dense
    /// access (`bitmap: None`) sums every slot and `count == len`; a gated
    /// access (`bitmap: Some`) sums only valid slots (`count` = #valid). Each
    /// element widens to `f64` via
    /// [`column_elem_to_f64`](super::Codegen::column_elem_to_f64) — the
    /// identity on `f64`, a signed/unsigned int→f64 conversion otherwise. This
    /// is the overflow-safe first pass shared by `Column.mean`/`var`/`std` and
    /// `Stats.variance`/`stddev`: the `f64` accumulator can't overflow the way
    /// an element-typed integer fold would. The **empty policy stays at the
    /// call site** — each surface guards its own minimum count (`Stats` `n ≥ 1`,
    /// `Column.mean` `n ≥ 1`, `Column.var`/`std` `n ≥ 2`) with its own message
    /// against the returned `count`.
    pub(super) fn emit_sum_f64_and_count(
        &mut self,
        access: &ContainerAccess<'ctx>,
    ) -> Result<(FloatValue<'ctx>, IntValue<'ctx>), String> {
        let i64_t = self.context.i64_type();
        let f64_t = self.context.f64_type();
        let fn_val = self
            .current_fn
            .ok_or_else(|| "f64-sum outside function".to_string())?;

        let idx = self.builder.build_alloca(i64_t, "kern.fs.i").unwrap();
        let acc = self.builder.build_alloca(f64_t, "kern.fs.acc").unwrap();
        let cnt = self.builder.build_alloca(i64_t, "kern.fs.cnt").unwrap();
        self.builder.build_store(idx, i64_t.const_zero()).unwrap();
        self.builder.build_store(acc, f64_t.const_zero()).unwrap();
        self.builder.build_store(cnt, i64_t.const_zero()).unwrap();

        let head = self.context.append_basic_block(fn_val, "kern.fs.head");
        let body = self.context.append_basic_block(fn_val, "kern.fs.body");
        let add = self.context.append_basic_block(fn_val, "kern.fs.add");
        let cont = self.context.append_basic_block(fn_val, "kern.fs.cont");
        let exit = self.context.append_basic_block(fn_val, "kern.fs.exit");
        self.builder.build_unconditional_branch(head).unwrap();

        self.builder.position_at_end(head);
        let iv = self
            .builder
            .build_load(i64_t, idx, "kern.fs.iv")
            .unwrap()
            .into_int_value();
        let more = self
            .builder
            .build_int_compare(IntPredicate::ULT, iv, access.len, "kern.fs.more")
            .unwrap();
        self.builder
            .build_conditional_branch(more, body, exit)
            .unwrap();

        // Dense: every slot contributes. Gated: only valid slots — nulls skip
        // straight to `cont` without touching the accumulator or count.
        self.builder.position_at_end(body);
        match access.bitmap {
            Some(bitmap) => {
                let valid = self.column_load_valid_bit(bitmap, iv);
                self.builder
                    .build_conditional_branch(valid, add, cont)
                    .unwrap();
            }
            None => {
                self.builder.build_unconditional_branch(add).unwrap();
            }
        }

        self.builder.position_at_end(add);
        let x = self.access_load(access, iv);
        let xf = self.column_elem_to_f64(x, access.unsigned);
        let a = self
            .builder
            .build_load(f64_t, acc, "kern.fs.a")
            .unwrap()
            .into_float_value();
        let a2 = self.builder.build_float_add(a, xf, "kern.fs.a2").unwrap();
        self.builder.build_store(acc, a2).unwrap();
        let c = self
            .builder
            .build_load(i64_t, cnt, "kern.fs.c")
            .unwrap()
            .into_int_value();
        let c2 = self
            .builder
            .build_int_add(c, i64_t.const_int(1, false), "kern.fs.c2")
            .unwrap();
        self.builder.build_store(cnt, c2).unwrap();
        self.builder.build_unconditional_branch(cont).unwrap();

        self.builder.position_at_end(cont);
        let next = self
            .builder
            .build_int_add(iv, i64_t.const_int(1, false), "kern.fs.next")
            .unwrap();
        self.builder.build_store(idx, next).unwrap();
        self.builder.build_unconditional_branch(head).unwrap();

        self.builder.position_at_end(exit);
        let sum = self
            .builder
            .build_load(f64_t, acc, "kern.fs.sum")
            .unwrap()
            .into_float_value();
        let count = self
            .builder
            .build_load(i64_t, cnt, "kern.fs.count")
            .unwrap()
            .into_int_value();
        Ok((sum, count))
    }

    /// Variance from a precomputed `(sum, count)` first pass (from
    /// [`emit_sum_f64_and_count`](Self::emit_sum_f64_and_count)). Computes
    /// `mean = sum / count`, sums the squared deviations `Σ (x − mean)²` over
    /// the access in a second pass (dense: every slot; gated: valid slots
    /// only), then divides by the Bessel-adjusted denominator — `count − 1`
    /// when `bessel` (the **sample** form, `Column.var`/`std`), `count`
    /// otherwise (the **population** form, `Stats.variance`/`stddev`). The
    /// caller has already guarded the minimum count against `count`; `std`
    /// callers `sqrt` the returned variance.
    pub(super) fn emit_variance_from(
        &mut self,
        access: &ContainerAccess<'ctx>,
        sum: FloatValue<'ctx>,
        count: IntValue<'ctx>,
        bessel: bool,
    ) -> Result<FloatValue<'ctx>, String> {
        let i64_t = self.context.i64_type();
        let f64_t = self.context.f64_type();
        let fn_val = self
            .current_fn
            .ok_or_else(|| "variance outside function".to_string())?;
        let cntf = self
            .builder
            .build_unsigned_int_to_float(count, f64_t, "kern.var.cntf")
            .unwrap();
        let mean = self
            .builder
            .build_float_div(sum, cntf, "kern.var.mean")
            .unwrap();

        // Pass 2 — Σ (x − mean)² over the dense-or-gated access.
        let idx = self.builder.build_alloca(i64_t, "kern.var.i").unwrap();
        let ss = self.builder.build_alloca(f64_t, "kern.var.ss").unwrap();
        self.builder.build_store(idx, i64_t.const_zero()).unwrap();
        self.builder.build_store(ss, f64_t.const_zero()).unwrap();

        let head = self.context.append_basic_block(fn_val, "kern.var.head");
        let body = self.context.append_basic_block(fn_val, "kern.var.body");
        let add = self.context.append_basic_block(fn_val, "kern.var.add");
        let cont = self.context.append_basic_block(fn_val, "kern.var.cont");
        let exit = self.context.append_basic_block(fn_val, "kern.var.exit");
        self.builder.build_unconditional_branch(head).unwrap();

        self.builder.position_at_end(head);
        let iv = self
            .builder
            .build_load(i64_t, idx, "kern.var.iv")
            .unwrap()
            .into_int_value();
        let more = self
            .builder
            .build_int_compare(IntPredicate::ULT, iv, access.len, "kern.var.more")
            .unwrap();
        self.builder
            .build_conditional_branch(more, body, exit)
            .unwrap();

        self.builder.position_at_end(body);
        match access.bitmap {
            Some(bitmap) => {
                let valid = self.column_load_valid_bit(bitmap, iv);
                self.builder
                    .build_conditional_branch(valid, add, cont)
                    .unwrap();
            }
            None => {
                self.builder.build_unconditional_branch(add).unwrap();
            }
        }

        self.builder.position_at_end(add);
        let x = self.access_load(access, iv);
        let xf = self.column_elem_to_f64(x, access.unsigned);
        let d = self
            .builder
            .build_float_sub(xf, mean, "kern.var.d")
            .unwrap();
        let d2 = self.builder.build_float_mul(d, d, "kern.var.d2").unwrap();
        let s = self
            .builder
            .build_load(f64_t, ss, "kern.var.s")
            .unwrap()
            .into_float_value();
        let s2 = self.builder.build_float_add(s, d2, "kern.var.s2").unwrap();
        self.builder.build_store(ss, s2).unwrap();
        self.builder.build_unconditional_branch(cont).unwrap();

        self.builder.position_at_end(cont);
        let next = self
            .builder
            .build_int_add(iv, i64_t.const_int(1, false), "kern.var.next")
            .unwrap();
        self.builder.build_store(idx, next).unwrap();
        self.builder.build_unconditional_branch(head).unwrap();

        self.builder.position_at_end(exit);
        let ss_v = self
            .builder
            .build_load(f64_t, ss, "kern.var.ssv")
            .unwrap()
            .into_float_value();
        // Population divides by `count`; sample (Bessel) by `count − 1`. The
        // population branch keeps `cntf` unmodified so `Stats.variance` stays
        // byte-identical (no dead `− 0.0`).
        let denom = if bessel {
            self.builder
                .build_float_sub(cntf, f64_t.const_float(1.0), "kern.var.denom")
                .unwrap()
        } else {
            cntf
        };
        Ok(self
            .builder
            .build_float_div(ss_v, denom, "kern.var.out")
            .unwrap())
    }

    /// The shared in-place ascending insertion sort over a scratch buffer of
    /// `n` elements (S4) — the one sort loop behind every ordering op:
    /// `Stats.sort`/`median`/`percentile`/`argsort`, `Column.median`/
    /// `quantile`, and the `DataFrame.describe` quartiles. [`SortKey`] picks
    /// the element/key relationship: `Value` sorts an `f64` buffer by its own
    /// elements; `IndexInto(data)` sorts an `i64` index buffer keyed by
    /// `data[idx]` (argsort — stable, since the strict `>` never shifts an
    /// equal key). NaN keys follow `fcmp ogt`: a NaN never shifts a smaller
    /// element, so NaNs settle at the front — the scalar-comparison posture
    /// (NaN unordered); quantiles over NaN-bearing data are undefined anyway.
    ///
    /// Insertion sort:
    /// `for si in 1..n { key = buf[si]; sj = si-1;`
    /// `  while sj >= 0 && key_of(buf[sj]) > key_of(key) { buf[sj+1] = buf[sj]; sj-- }`
    /// `  buf[sj+1] = key }`
    pub(super) fn emit_sort_scratch(
        &self,
        buf: PointerValue<'ctx>,
        n: IntValue<'ctx>,
        key: &SortKey<'ctx>,
    ) {
        let i64_t = self.context.i64_type();
        let f64_t = self.context.f64_type();
        let fn_val = self.current_fn.expect("scratch sort in function");
        // Value sort moves f64/i64 elements; argsort moves i64 indices.
        let elem_t: BasicTypeEnum<'ctx> = match key {
            SortKey::Value => f64_t.into(),
            SortKey::IntValue | SortKey::IndexInto(_) | SortKey::IndexIntoInt(_) => i64_t.into(),
        };
        // The sort key of a loaded element (f64 for the float forms, i64 for
        // the exact-integer forms).
        let key_of = |el: BasicValueEnum<'ctx>, nm: &str| -> BasicValueEnum<'ctx> {
            match key {
                SortKey::Value | SortKey::IntValue => el,
                SortKey::IndexInto(data) => {
                    let slot = unsafe {
                        self.builder
                            .build_gep(f64_t, *data, &[el.into_int_value()], nm)
                            .unwrap()
                    };
                    self.builder.build_load(f64_t, slot, nm).unwrap()
                }
                SortKey::IndexIntoInt(data) => {
                    let slot = unsafe {
                        self.builder
                            .build_gep(i64_t, *data, &[el.into_int_value()], nm)
                            .unwrap()
                    };
                    self.builder.build_load(i64_t, slot, nm).unwrap()
                }
            }
        };
        // Strict `key(a) > key(b)` — ordered `fcmp ogt` for float keys (NaN
        // settles to the front), signed `icmp sgt` for exact-integer keys.
        let key_gt = |a: BasicValueEnum<'ctx>, b: BasicValueEnum<'ctx>| -> IntValue<'ctx> {
            match key {
                SortKey::Value | SortKey::IndexInto(_) => self
                    .builder
                    .build_float_compare(
                        FloatPredicate::OGT,
                        a.into_float_value(),
                        b.into_float_value(),
                        "kern.is.gt",
                    )
                    .unwrap(),
                SortKey::IntValue | SortKey::IndexIntoInt(_) => self
                    .builder
                    .build_int_compare(
                        IntPredicate::SGT,
                        a.into_int_value(),
                        b.into_int_value(),
                        "kern.is.gt",
                    )
                    .unwrap(),
            }
        };

        let si = self.builder.build_alloca(i64_t, "kern.is.si").unwrap();
        let sj = self.builder.build_alloca(i64_t, "kern.is.sj").unwrap();
        let key_a = self.builder.build_alloca(elem_t, "kern.is.key").unwrap();
        self.builder
            .build_store(si, i64_t.const_int(1, false))
            .unwrap();
        let oh = self.context.append_basic_block(fn_val, "kern.is.ohead");
        let ob = self.context.append_basic_block(fn_val, "kern.is.obody");
        let ih = self.context.append_basic_block(fn_val, "kern.is.ihead");
        let ick = self.context.append_basic_block(fn_val, "kern.is.icheck");
        let ish = self.context.append_basic_block(fn_val, "kern.is.ishift");
        let ipl = self.context.append_basic_block(fn_val, "kern.is.iplace");
        let oc = self.context.append_basic_block(fn_val, "kern.is.ocont");
        let oe = self.context.append_basic_block(fn_val, "kern.is.oexit");
        self.builder.build_unconditional_branch(oh).unwrap();

        self.builder.position_at_end(oh);
        let siv = self
            .builder
            .build_load(i64_t, si, "kern.is.siv")
            .unwrap()
            .into_int_value();
        let omore = self
            .builder
            .build_int_compare(IntPredicate::ULT, siv, n, "kern.is.omore")
            .unwrap();
        self.builder
            .build_conditional_branch(omore, ob, oe)
            .unwrap();

        self.builder.position_at_end(ob);
        let key_slot = unsafe {
            self.builder
                .build_gep(elem_t, buf, &[siv], "kern.is.keyslot")
                .unwrap()
        };
        let key_v = self
            .builder
            .build_load(elem_t, key_slot, "kern.is.keyv")
            .unwrap();
        self.builder.build_store(key_a, key_v).unwrap();
        self.builder
            .build_store(
                sj,
                self.builder
                    .build_int_sub(siv, i64_t.const_int(1, false), "kern.is.sj0")
                    .unwrap(),
            )
            .unwrap();
        self.builder.build_unconditional_branch(ih).unwrap();

        self.builder.position_at_end(ih);
        let sjv = self
            .builder
            .build_load(i64_t, sj, "kern.is.sjv")
            .unwrap()
            .into_int_value();
        // Signed `sj >= 0` (short-circuits before the buf[sj] read).
        let ge0 = self
            .builder
            .build_int_compare(IntPredicate::SGE, sjv, i64_t.const_zero(), "kern.is.ge0")
            .unwrap();
        self.builder
            .build_conditional_branch(ge0, ick, ipl)
            .unwrap();

        self.builder.position_at_end(ick);
        let bj_slot = unsafe {
            self.builder
                .build_gep(elem_t, buf, &[sjv], "kern.is.bjslot")
                .unwrap()
        };
        let bj = self
            .builder
            .build_load(elem_t, bj_slot, "kern.is.bj")
            .unwrap();
        let bj_key = key_of(bj, "kern.is.bjkey");
        let key_cur = self
            .builder
            .build_load(elem_t, key_a, "kern.is.keycur")
            .unwrap();
        let cur_key = key_of(key_cur, "kern.is.curkey");
        let gt = key_gt(bj_key, cur_key);
        self.builder.build_conditional_branch(gt, ish, ipl).unwrap();

        self.builder.position_at_end(ish);
        // buf[sj+1] = buf[sj]
        let sjp1 = self
            .builder
            .build_int_add(sjv, i64_t.const_int(1, false), "kern.is.sjp1")
            .unwrap();
        let dst = unsafe {
            self.builder
                .build_gep(elem_t, buf, &[sjp1], "kern.is.dst")
                .unwrap()
        };
        self.builder.build_store(dst, bj).unwrap();
        self.builder
            .build_store(
                sj,
                self.builder
                    .build_int_sub(sjv, i64_t.const_int(1, false), "kern.is.sjdec")
                    .unwrap(),
            )
            .unwrap();
        self.builder.build_unconditional_branch(ih).unwrap();

        self.builder.position_at_end(ipl);
        // buf[sj+1] = key (sj holds the final resting slot minus one).
        let sjv2 = self
            .builder
            .build_load(i64_t, sj, "kern.is.sjv2")
            .unwrap()
            .into_int_value();
        let placep1 = self
            .builder
            .build_int_add(sjv2, i64_t.const_int(1, false), "kern.is.placep1")
            .unwrap();
        let pslot = unsafe {
            self.builder
                .build_gep(elem_t, buf, &[placep1], "kern.is.pslot")
                .unwrap()
        };
        let key_final = self
            .builder
            .build_load(elem_t, key_a, "kern.is.keyf")
            .unwrap();
        self.builder.build_store(pslot, key_final).unwrap();
        self.builder.build_unconditional_branch(oc).unwrap();

        self.builder.position_at_end(oc);
        self.builder
            .build_store(
                si,
                self.builder
                    .build_int_add(siv, i64_t.const_int(1, false), "kern.is.sinext")
                    .unwrap(),
            )
            .unwrap();
        self.builder.build_unconditional_branch(oh).unwrap();

        self.builder.position_at_end(oe);
    }

    /// Classify a scratch-sortable element for the *widened* sort path (S6c
    /// follow-on: `sorted`/`argsort` beyond i64/f64). `Ok(true)` — an integer
    /// key (sorted/compared at signed i64); `Ok(false)` — a float key (f64).
    /// i64(signed)/f64 pass through directly; `i8`/`i16`/`i32` widen by
    /// sign-extension, `u8`/`u16`/`u32` by zero-extension, `f32` by `fpext` —
    /// all lossless into the 8-byte scratch slot, so the `karac build` result
    /// matches `karac run`. **u64** (unsigned 64-bit) is the sole rejection:
    /// this shared scratch sort ([`emit_sort_scratch`]) compares integer keys as
    /// SIGNED (`SGT`), misordering values ≥ 2⁶³. The interpreter now HAS a real
    /// u64 model (bug-ledger B-2026-07-04-8, fixed), so `run` sorts these
    /// Columns / Tensors correctly and the old "enabling `build` would diverge
    /// from `run`" blocker is gone — but threading the unsigned `UGT` scratch
    /// compare through [`SortKey`] (so `Column[u64]`/`Tensor[u64]`
    /// `sorted`/`argsort` match) is a tracked follow-on: the kernel is shared
    /// with the stats median/percentile path, so it wants its own reviewed slice
    /// (bug-ledger B-2026-07-07-2). Until then a u64 element sort is rejected
    /// LOUDLY rather than silently mis-sorted. The sibling `Vec[u64].sort()`
    /// (its own default-order thunk, not this kernel) is already
    /// unsigned-correct. A non-numeric element is a typechecker-caught
    /// impossibility here, rejected defensively.
    pub(super) fn sort_key_is_int(
        &self,
        elem: BasicTypeEnum<'ctx>,
        unsigned: bool,
        method: &str,
        container: &str,
    ) -> Result<bool, String> {
        match elem {
            BasicTypeEnum::IntType(it) if it.get_bit_width() == 64 && unsigned => Err(format!(
                "{container}.{method} under the native backend (`karac build`) does \
                 not yet support u64 element {container}s — the shared scratch sort \
                 compares integer keys as signed i64, which misorders values ≥ 2^63. \
                 The interpreter now has a real u64 model (`karac run` sorts these \
                 correctly — bug-ledger B-2026-07-04-8, fixed), so this is no longer \
                 gated on `run`; wiring the unsigned `UGT` scratch compare through \
                 `SortKey` (the kernel is shared with the stats median/percentile \
                 path) is a tracked follow-on — see bug-ledger B-2026-07-07-2. \
                 `Vec[u64].sort()` is already unsigned-correct under `build`."
            )),
            BasicTypeEnum::IntType(_) => Ok(true),
            BasicTypeEnum::FloatType(_) => Ok(false),
            _ => Err(format!(
                "{container}.{method} under the native backend (`karac build`) \
                 requires a numeric element type."
            )),
        }
    }

    /// Widen a loaded element into its 8-byte scratch-sort key: signed ints
    /// `sext` to i64, unsigned ints `zext` to i64, `f32` `fpext` to f64;
    /// i64/f64 pass through unchanged. Inverse of [`sort_narrow_value`].
    pub(super) fn sort_widen_value(
        &self,
        v: BasicValueEnum<'ctx>,
        elem: BasicTypeEnum<'ctx>,
        unsigned: bool,
    ) -> BasicValueEnum<'ctx> {
        let i64_t = self.context.i64_type();
        let f64_t = self.context.f64_type();
        match elem {
            BasicTypeEnum::IntType(it) if it.get_bit_width() == 64 => v,
            BasicTypeEnum::IntType(_) if unsigned => self
                .builder
                .build_int_z_extend(v.into_int_value(), i64_t, "sort.zext")
                .unwrap()
                .into(),
            BasicTypeEnum::IntType(_) => self
                .builder
                .build_int_s_extend(v.into_int_value(), i64_t, "sort.sext")
                .unwrap()
                .into(),
            BasicTypeEnum::FloatType(ft) if ft.get_bit_width() == 64 => v,
            BasicTypeEnum::FloatType(_) => self
                .builder
                .build_float_ext(v.into_float_value(), f64_t, "sort.fpext")
                .unwrap()
                .into(),
            _ => v,
        }
    }

    /// Inverse of [`sort_widen_value`] for a `sorted() -> Vec[T]` result:
    /// narrow an 8-byte sorted key back to the element width (`trunc` for
    /// narrow ints, `fptrunc` for `f32`); i64/f64 pass through. The widen was
    /// lossless, so the round-trip is exact.
    fn sort_narrow_value(
        &self,
        v: BasicValueEnum<'ctx>,
        elem: BasicTypeEnum<'ctx>,
    ) -> BasicValueEnum<'ctx> {
        match elem {
            BasicTypeEnum::IntType(it) if it.get_bit_width() != 64 => self
                .builder
                .build_int_truncate(v.into_int_value(), it, "sort.trunc")
                .unwrap()
                .into(),
            BasicTypeEnum::FloatType(ft) if ft.get_bit_width() != 64 => self
                .builder
                .build_float_trunc(v.into_float_value(), ft, "sort.fptrunc")
                .unwrap()
                .into(),
            _ => v,
        }
    }

    /// True when `elem` occupies a full 8-byte scratch slot with no widening
    /// (`i64`/`f64`) — the sort operates on it in place; a narrow int / `f32`
    /// is widened on the way in and narrowed on the way out.
    pub(super) fn sort_elem_is_wide(&self, elem: BasicTypeEnum<'ctx>) -> bool {
        matches!(elem, BasicTypeEnum::IntType(it) if it.get_bit_width() == 64)
            || matches!(elem, BasicTypeEnum::FloatType(ft) if ft.get_bit_width() == 64)
    }

    /// Build a `sorted() -> Vec[T]` value from a **sorted** 8-byte key buffer
    /// `buf8` of `k` keys. For an 8-byte element the buffer *is* the Vec
    /// storage (stolen via [`stats_build_vec`]); for a narrow int / `f32` it
    /// mallocs a fresh `k * sizeof(T)` buffer, narrows each key back to `elem`
    /// ([`sort_narrow_value`]), frees `buf8`, and builds the Vec over the
    /// narrow buffer. Either way the binding site owns and frees the storage
    /// backing the returned Vec.
    pub(super) fn sort_build_vec_from_keys(
        &self,
        buf8: PointerValue<'ctx>,
        k: IntValue<'ctx>,
        elem: BasicTypeEnum<'ctx>,
    ) -> BasicValueEnum<'ctx> {
        if self.sort_elem_is_wide(elem) {
            return self.stats_build_vec(buf8, k);
        }
        let i64_t = self.context.i64_type();
        let f64_t = self.context.f64_type();
        let fn_val = self.current_fn.expect("sort narrow-back in function");
        let key_t: BasicTypeEnum<'ctx> = if elem.is_int_type() {
            i64_t.into()
        } else {
            f64_t.into()
        };
        let esize = match elem {
            BasicTypeEnum::IntType(it) => (it.get_bit_width() / 8) as u64,
            BasicTypeEnum::FloatType(ft) => (ft.get_bit_width() / 8) as u64,
            _ => 8,
        };
        let nbytes = self
            .builder
            .build_int_mul(k, i64_t.const_int(esize, false), "sort.nb.bytes")
            .unwrap();
        let nbuf = self
            .builder
            .build_call(self.malloc_fn, &[nbytes.into()], "sort.nb.buf")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();

        let j = self.builder.build_alloca(i64_t, "sort.nb.j").unwrap();
        self.builder.build_store(j, i64_t.const_zero()).unwrap();
        let head = self.context.append_basic_block(fn_val, "sort.nb.head");
        let body = self.context.append_basic_block(fn_val, "sort.nb.body");
        let exit = self.context.append_basic_block(fn_val, "sort.nb.exit");
        self.builder.build_unconditional_branch(head).unwrap();

        self.builder.position_at_end(head);
        let jv = self
            .builder
            .build_load(i64_t, j, "sort.nb.jv")
            .unwrap()
            .into_int_value();
        let more = self
            .builder
            .build_int_compare(IntPredicate::ULT, jv, k, "sort.nb.more")
            .unwrap();
        self.builder
            .build_conditional_branch(more, body, exit)
            .unwrap();

        self.builder.position_at_end(body);
        let src = unsafe {
            self.builder
                .build_gep(key_t, buf8, &[jv], "sort.nb.src")
                .unwrap()
        };
        let keyv = self.builder.build_load(key_t, src, "sort.nb.key").unwrap();
        let narrowed = self.sort_narrow_value(keyv, elem);
        let dst = unsafe {
            self.builder
                .build_gep(elem, nbuf, &[jv], "sort.nb.dst")
                .unwrap()
        };
        self.builder.build_store(dst, narrowed).unwrap();
        self.builder
            .build_store(
                j,
                self.builder
                    .build_int_add(jv, i64_t.const_int(1, false), "sort.nb.j2")
                    .unwrap(),
            )
            .unwrap();
        self.builder.build_unconditional_branch(head).unwrap();

        self.builder.position_at_end(exit);
        self.builder
            .build_call(self.free_fn, &[buf8.into()], "sort.nb.free")
            .unwrap();
        self.stats_build_vec(nbuf, k)
    }

    /// Materialize an 8-byte-per-element *widened* copy of a contiguous `data`
    /// buffer of `count` elements at `elem` — the key array an `argsort` over a
    /// narrow int / `f32` container keys into (`IndexIntoInt`/`IndexInto`); the
    /// caller frees it after the sort. Only needed when the element is NOT
    /// already 8-byte ([`sort_elem_is_wide`]); the wide case keys directly into
    /// the live data with no copy.
    pub(super) fn sort_widen_data_buffer(
        &self,
        data: PointerValue<'ctx>,
        count: IntValue<'ctx>,
        elem: BasicTypeEnum<'ctx>,
        unsigned: bool,
    ) -> PointerValue<'ctx> {
        let i64_t = self.context.i64_type();
        let f64_t = self.context.f64_type();
        let fn_val = self.current_fn.expect("sort widen-data in function");
        let key_t: BasicTypeEnum<'ctx> = if elem.is_int_type() {
            i64_t.into()
        } else {
            f64_t.into()
        };
        let nbytes = self
            .builder
            .build_int_mul(count, i64_t.const_int(8, false), "sort.wd.bytes")
            .unwrap();
        let wbuf = self
            .builder
            .build_call(self.malloc_fn, &[nbytes.into()], "sort.wd.buf")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();

        let j = self.builder.build_alloca(i64_t, "sort.wd.j").unwrap();
        self.builder.build_store(j, i64_t.const_zero()).unwrap();
        let head = self.context.append_basic_block(fn_val, "sort.wd.head");
        let body = self.context.append_basic_block(fn_val, "sort.wd.body");
        let exit = self.context.append_basic_block(fn_val, "sort.wd.exit");
        self.builder.build_unconditional_branch(head).unwrap();

        self.builder.position_at_end(head);
        let jv = self
            .builder
            .build_load(i64_t, j, "sort.wd.jv")
            .unwrap()
            .into_int_value();
        let more = self
            .builder
            .build_int_compare(IntPredicate::ULT, jv, count, "sort.wd.more")
            .unwrap();
        self.builder
            .build_conditional_branch(more, body, exit)
            .unwrap();

        self.builder.position_at_end(body);
        let src = unsafe {
            self.builder
                .build_gep(elem, data, &[jv], "sort.wd.src")
                .unwrap()
        };
        let raw = self.builder.build_load(elem, src, "sort.wd.raw").unwrap();
        let widened = self.sort_widen_value(raw, elem, unsigned);
        let dst = unsafe {
            self.builder
                .build_gep(key_t, wbuf, &[jv], "sort.wd.dst")
                .unwrap()
        };
        self.builder.build_store(dst, widened).unwrap();
        self.builder
            .build_store(
                j,
                self.builder
                    .build_int_add(jv, i64_t.const_int(1, false), "sort.wd.j2")
                    .unwrap(),
            )
            .unwrap();
        self.builder.build_unconditional_branch(head).unwrap();

        self.builder.position_at_end(exit);
        wbuf
    }

    /// The shared first-occurrence `argmin`/`argmax` over a **non-empty**
    /// dense access (S4) — the index of the first smallest/largest element.
    /// Tracks the best index, re-reading `data[best]` each iteration for the
    /// compare; the strict `<`/`>` keeps the first occurrence (a later equal
    /// value never displaces it), and a NaN comparison is false, so NaN
    /// neither takes nor blocks the slot beyond position 0. The caller
    /// guards emptiness (`Stats` wraps in `Option` with a `None` arm). A
    /// gated (`Column`) form has no surface today — `Err` until one does.
    pub(super) fn emit_reduce_argminmax(
        &mut self,
        access: &ContainerAccess<'ctx>,
        is_max: bool,
    ) -> Result<IntValue<'ctx>, String> {
        if access.bitmap.is_some() {
            return Err("emit_reduce_argminmax: no validity-gated surface exists yet".to_string());
        }
        let i64_t = self.context.i64_type();
        let fn_val = self
            .current_fn
            .ok_or_else(|| "reduce argminmax outside function".to_string())?;
        let is_float = access.elem.is_float_type();

        let bi = self.builder.build_alloca(i64_t, "kern.am.bi").unwrap();
        self.builder.build_store(bi, i64_t.const_zero()).unwrap();
        let i = self.builder.build_alloca(i64_t, "kern.am.i").unwrap();
        self.builder
            .build_store(i, i64_t.const_int(1, false))
            .unwrap();

        let head = self.context.append_basic_block(fn_val, "kern.am.head");
        let body = self.context.append_basic_block(fn_val, "kern.am.body");
        let exit = self.context.append_basic_block(fn_val, "kern.am.exit");
        self.builder.build_unconditional_branch(head).unwrap();

        self.builder.position_at_end(head);
        let iv = self
            .builder
            .build_load(i64_t, i, "kern.am.iv")
            .unwrap()
            .into_int_value();
        let more = self
            .builder
            .build_int_compare(IntPredicate::ULT, iv, access.len, "kern.am.more")
            .unwrap();
        self.builder
            .build_conditional_branch(more, body, exit)
            .unwrap();

        self.builder.position_at_end(body);
        let x = self.access_load(access, iv);
        let biv = self
            .builder
            .build_load(i64_t, bi, "kern.am.biv")
            .unwrap()
            .into_int_value();
        let bx = self.access_load(access, biv);
        // `x ⋖ data[best]` → take i. Float uses ordered predicates (NaN →
        // false); int uses the signedness-correct predicate.
        let take = if is_float {
            let pred = if is_max {
                FloatPredicate::OGT
            } else {
                FloatPredicate::OLT
            };
            self.builder
                .build_float_compare(
                    pred,
                    x.into_float_value(),
                    bx.into_float_value(),
                    "kern.am.take",
                )
                .unwrap()
        } else {
            let pred = match (is_max, access.unsigned) {
                (false, false) => IntPredicate::SLT,
                (false, true) => IntPredicate::ULT,
                (true, false) => IntPredicate::SGT,
                (true, true) => IntPredicate::UGT,
            };
            self.builder
                .build_int_compare(
                    pred,
                    x.into_int_value(),
                    bx.into_int_value(),
                    "kern.am.take",
                )
                .unwrap()
        };
        let newbi = self
            .builder
            .build_select(take, iv, biv, "kern.am.newbi")
            .unwrap()
            .into_int_value();
        self.builder.build_store(bi, newbi).unwrap();
        self.builder
            .build_store(
                i,
                self.builder
                    .build_int_add(iv, i64_t.const_int(1, false), "kern.am.i2")
                    .unwrap(),
            )
            .unwrap();
        self.builder.build_unconditional_branch(head).unwrap();

        self.builder.position_at_end(exit);
        Ok(self
            .builder
            .build_load(i64_t, bi, "kern.am.res")
            .unwrap()
            .into_int_value())
    }

    /// The validity-gated sibling of [`emit_reduce_argminmax`] (S6c) — the
    /// first-occurrence `argmin`/`argmax` over a `Column`'s valid slots, used
    /// by the `ElementwiseOrd` surface. Null slots are skipped in the compare
    /// but the tracked/returned index is the ORIGINAL slot (`Series.idxmin`
    /// semantics). Returns `(seeded, best)`: `seeded` (an `i1`) is `false` iff
    /// the column is empty / all-null — the caller wraps `best` in
    /// `Some`/`None` on it. Like the dense form the strict `<`/`>` keeps the
    /// first occurrence, and `data[best]` is re-read each iteration for the
    /// compare (the pre-seed read of `data[0]` is a safe in-bounds placeholder
    /// read whose result is masked by the `seeded` OR).
    pub(super) fn emit_reduce_argminmax_gated(
        &mut self,
        access: &ContainerAccess<'ctx>,
        bitmap: PointerValue<'ctx>,
        is_max: bool,
    ) -> Result<(IntValue<'ctx>, IntValue<'ctx>), String> {
        let i64_t = self.context.i64_type();
        let bool_t = self.context.bool_type();
        let fn_val = self
            .current_fn
            .ok_or_else(|| "gated reduce argminmax outside function".to_string())?;

        let idx = self.builder.build_alloca(i64_t, "kern.gam.i").unwrap();
        let best = self.builder.build_alloca(i64_t, "kern.gam.best").unwrap();
        let seeded = self
            .builder
            .build_alloca(bool_t, "kern.gam.seeded")
            .unwrap();
        self.builder.build_store(idx, i64_t.const_zero()).unwrap();
        self.builder.build_store(best, i64_t.const_zero()).unwrap();
        self.builder
            .build_store(seeded, bool_t.const_zero())
            .unwrap();

        let head = self.context.append_basic_block(fn_val, "kern.gam.head");
        let body = self.context.append_basic_block(fn_val, "kern.gam.body");
        let upd = self.context.append_basic_block(fn_val, "kern.gam.upd");
        let cont = self.context.append_basic_block(fn_val, "kern.gam.cont");
        let exit = self.context.append_basic_block(fn_val, "kern.gam.exit");
        self.builder.build_unconditional_branch(head).unwrap();

        self.builder.position_at_end(head);
        let iv = self
            .builder
            .build_load(i64_t, idx, "kern.gam.iv")
            .unwrap()
            .into_int_value();
        let more = self
            .builder
            .build_int_compare(IntPredicate::ULT, iv, access.len, "kern.gam.more")
            .unwrap();
        self.builder
            .build_conditional_branch(more, body, exit)
            .unwrap();

        self.builder.position_at_end(body);
        let valid = self.column_load_valid_bit(bitmap, iv);
        self.builder
            .build_conditional_branch(valid, upd, cont)
            .unwrap();

        self.builder.position_at_end(upd);
        let x = self.access_load(access, iv);
        let bestv = self
            .builder
            .build_load(i64_t, best, "kern.gam.bestv")
            .unwrap()
            .into_int_value();
        let bx = self.access_load(access, bestv);
        let s = self
            .builder
            .build_load(bool_t, seeded, "kern.gam.s")
            .unwrap()
            .into_int_value();
        // `x ⋖ data[best]`; take unconditionally when not yet seeded.
        let cmp_op = if is_max { BinOp::Gt } else { BinOp::Lt };
        let cmp = self
            .compile_binop_typed(&cmp_op, x, bx, access.unsigned)?
            .into_int_value();
        let not_seeded = self.builder.build_not(s, "kern.gam.ns").unwrap();
        let take = self
            .builder
            .build_or(not_seeded, cmp, "kern.gam.take")
            .unwrap();
        let newbest = self
            .builder
            .build_select(take, iv, bestv, "kern.gam.newbest")
            .unwrap()
            .into_int_value();
        self.builder.build_store(best, newbest).unwrap();
        self.builder
            .build_store(seeded, bool_t.const_int(1, false))
            .unwrap();
        self.builder.build_unconditional_branch(cont).unwrap();

        self.builder.position_at_end(cont);
        let next = self
            .builder
            .build_int_add(iv, i64_t.const_int(1, false), "kern.gam.next")
            .unwrap();
        self.builder.build_store(idx, next).unwrap();
        self.builder.build_unconditional_branch(head).unwrap();

        self.builder.position_at_end(exit);
        let seeded_v = self
            .builder
            .build_load(bool_t, seeded, "kern.gam.sf")
            .unwrap()
            .into_int_value();
        let best_v = self
            .builder
            .build_load(i64_t, best, "kern.gam.bf")
            .unwrap()
            .into_int_value();
        Ok((seeded_v, best_v))
    }
}
