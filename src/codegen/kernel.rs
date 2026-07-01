//! Codegen side of the Reduce/ElementwiseMap/ElementwiseOrd unification
//! (`docs/spikes/reduce-elementwise-trait-unification.md`). This module owns
//! the shared LLVM emitters that the `Tensor`, `Column`, and `Stats.*`
//! reductions funnel through, keyed on the same backend-agnostic
//! [`crate::reduce_kernel::ReduceOp`] vocabulary the interpreter twin uses
//! (S0). It is the codegen counterpart of `src/reduce_kernel.rs`.
//!
//! The three surfaces share one index-fold skeleton; the axes that genuinely
//! differ — the element source ([`ContainerAccess`]), the element kind, and
//! per-surface knobs (seed, empty policy, result wrapping) — are parameters,
//! not forks. **S1 migrates the contiguous no-validity fold family
//! (`sum`/`prod`/`mean`) of `Stats` and `Tensor`;** the Arrow-nullable
//! validity gate (`Column`), the `min`/`max` ordering family, and the
//! non-f64 `ElemKind` axis land in later sub-slices.
//!
//! **Byte-identical.** The emitters here reduce to the exact instructions the
//! hand-rolled loops emitted (`compile_binop_typed` lowers f64 `Add`/`Mul` to
//! `build_float_add`/`build_float_mul`, and `to_float` on an f64 is the
//! identity), so migrated surfaces keep byte-identical program output — proved
//! by the run-vs-build oracle.

use inkwell::types::BasicTypeEnum;
use inkwell::values::{BasicValueEnum, IntValue, PointerValue};
use inkwell::IntPredicate;

use crate::ast::BinOp;
use crate::reduce_kernel::ReduceOp;

/// How a reduction reads its elements. One flat, contiguous, non-nullable
/// numeric buffer — the `Stats` `Slice[f64]` and the `Tensor` C-order data
/// block. Element `i` is `data[i]` at LLVM type `elem`; there is no validity
/// bitmap (the `Column` Arrow-nullable form is a later slice).
pub(super) struct ContainerAccess<'ctx> {
    /// Base pointer of the element buffer.
    pub data: PointerValue<'ctx>,
    /// Number of elements.
    pub len: IntValue<'ctx>,
    /// LLVM type of one element (`f64` for `Stats`; the tensor's `T`).
    pub elem: BasicTypeEnum<'ctx>,
    /// Whether integer elements are unsigned (drives the fold's overflow
    /// semantics through `compile_binop_typed`). Ignored for float elements.
    pub unsigned: bool,
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
}
