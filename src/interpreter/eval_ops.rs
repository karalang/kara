//! Unary / binary / short-circuit / pipe operator evaluation.
//!
//! Houses `eval_unary` (`-x`, `!b`, `~i`), `eval_short_circuit` (`and`
//! / `or` with documented RHS-short-circuit semantics — design.md /
//! roadmap.md), `eval_binary` (the big op-dispatch table for arithmetic
//! / comparison / bitwise / string / shift ops with checked-arithmetic
//! overflow trapping), `eval_pipe` (`a |> f` / `a |> f(args)` /
//! `a |> f(_, args)` desugaring into a synthesized Call), and the
//! shared `record_integer_overflow` helper.
//!
//! Lives in a sibling `impl<'a> super::Interpreter<'a>` block.

use std::sync::{Arc, RwLock};

use crate::ast::*;
use crate::token::Span;

use super::value::narrow_to_i64;
use super::value::{EnumData, TensorElemWidth, Value};

impl<'a> super::Interpreter<'a> {
    // ── Operators ───────────────────────────────────────────────

    pub(crate) fn eval_unary(&mut self, op: &UnaryOp, operand: Value, span: &Span) -> Value {
        let operand_variant = operand.variant_name();
        match (op, operand) {
            // B-2026-08-06-7: `-iN::MIN` does not fit `iN`. `checked_neg`
            // catches it only at the i64 carrier's width, so a NARROW operand
            // (`-(-2147483648i32)`) sailed through with 2147483648 — an
            // out-of-range i32 — while the i64 equivalent trapped correctly.
            // Range-check the result against the DECLARED width exactly as the
            // arithmetic binop arms above do. Codegen mirrors this at the
            // `iN.neg` assoc-call site.
            (UnaryOp::Neg, Value::Int(i)) => Value::Int(match i.checked_neg() {
                Some(v) if !self.narrow_oob(v, span) => v,
                _ => return self.record_integer_overflow(span),
            }),
            (UnaryOp::Neg, Value::Float(f)) => Value::Float(-f),
            // Element-wise tensor negation — fold `-` over each element into a
            // fresh value-semantics tensor (the operand is read, not moved).
            (UnaryOp::Neg, Value::Tensor { dims, data, elem }) => {
                let elems = data.read().unwrap().clone();
                let mut out = Vec::with_capacity(elems.len());
                for x in elems {
                    out.push(self.eval_unary(&UnaryOp::Neg, x, span));
                    if self.pending_cf.is_some() {
                        return Value::Unit;
                    }
                }
                Self::round_tensor_elems(&mut out, elem);
                Value::Tensor {
                    dims,
                    data: Arc::new(RwLock::new(out)),
                    elem,
                }
            }
            // Element-wise column negation — negate each valid slot; null
            // slots stay null. Fresh value-semantics column (operand read).
            (UnaryOp::Neg, Value::Column { data, valid }) => {
                let elems = data.read().unwrap().clone();
                let valids = valid.read().unwrap().clone();
                let mut out = Vec::with_capacity(elems.len());
                for (ok, x) in valids.iter().zip(elems) {
                    if *ok {
                        out.push(self.eval_unary(&UnaryOp::Neg, x, span));
                        if self.pending_cf.is_some() {
                            return Value::Unit;
                        }
                    } else {
                        out.push(Value::Unit);
                    }
                }
                Value::Column {
                    data: Arc::new(RwLock::new(out)),
                    valid: Arc::new(RwLock::new(valids)),
                }
            }
            (UnaryOp::Not, Value::Bool(b)) => Value::Bool(!b),
            (UnaryOp::BitNot, Value::Int(i)) => Value::Int(!i),
            // Integer-lane `Vector[T, N]` complement: `~v` folds `~` over each
            // lane (the typechecker restricts the element to integer lanes).
            (UnaryOp::BitNot, Value::Vector(lanes)) => {
                let out: Vec<Value> = lanes
                    .into_iter()
                    .map(|l| self.eval_unary(&UnaryOp::BitNot, l, span))
                    .collect();
                Value::Vector(out)
            }
            // `*<chain>` where the chain yields a `mut ref V` into a Map slot
            // (`Map.entry(k).or_insert(d)`). Resolve the place-ref to the live
            // slot value. (When the operand is a bound identifier, `Env::get`
            // already resolved it before this point, so only the bare-chain
            // case reaches here as a raw `MapSlotRef`.)
            (UnaryOp::Deref, Value::MapSlotRef { map_var, key }) => {
                self.env.read_map_slot(&map_var, &key)
            }
            // `iter_mut` element ref (B-2026-07-14-10): a bare-chain `*<ref>`
            // reads the live Vec element. (A bound-identifier `*x` is already
            // auto-deref'd by `Env::get` before reaching here.)
            (UnaryOp::Deref, Value::VecSlotRef { storage, index }) => storage
                .read()
                .unwrap()
                .get(index)
                .cloned()
                .unwrap_or(Value::Unit),
            // In the tree-walk interpreter references are passed by value; `*r` is
            // a semantic no-op that returns the underlying value unchanged.
            (UnaryOp::Deref, v) => v,
            // As with `eval_binary`'s fallthrough: only reachable via `karac
            // run`, which executes despite typecheck errors. An illegal operand
            // (e.g. unary `-` on a String) becomes a graceful runtime error
            // rather than an interpreter `unreachable!()` panic.
            _ => self.record_runtime_error(
                format!(
                    "unary operator '{:?}' is not defined for an operand of type '{}' \
                     (this is a type error the typechecker reports as a hard error; \
                     it reached the interpreter only because `karac run` executes despite \
                     typecheck errors)",
                    op, operand_variant
                ),
                span,
            ),
        }
    }

    /// Evaluate `lhs and rhs` / `lhs or rhs` with short-circuit
    /// semantics — RHS is only evaluated when the LHS doesn't already
    /// determine the result, so RHS side-effects (panicking index,
    /// dropped fn call) don't fire when short-circuited.
    pub(crate) fn eval_short_circuit(
        &mut self,
        op: &BinOp,
        left: &Expr,
        right: &Expr,
        span: &Span,
    ) -> Value {
        let lhs_value = self.eval_expr_inner(left);
        // B-2026-08-09-19 — a faulted LHS (index OOB, unwrap of `None`,
        // div-by-zero, …) sets `pending_cf` and yields the `Unit` poison;
        // propagate it instead of asserting it is a Bool. Without this,
        // `if v[3] > 0i64 and true {}` on an empty Vec turned a clean
        // `vec index out of bounds` into an ICE blaming either the
        // typechecker or a wrong-variant codepath, when both were right and
        // the operand had simply already failed.
        //
        // Returning here also means the RHS is NOT evaluated after a faulted
        // LHS, which is the same guarantee the short-circuit itself gives: a
        // panicking index or a side-effecting call on the right must not fire
        // once the left has already gone wrong.
        //
        // `eval_short_circuit` needs its own check rather than inheriting the
        // one B-2026-07-15-7 gave the operand path: that guard lives in the
        // NON-short-circuit `Binary` evaluator, and `and`/`or` are routed away
        // from it precisely so the RHS stays unevaluated — so they never saw
        // it. The row's note that "Binary, Unary and Match all short-circuit
        // on pending_cf" was true of that other evaluator only.
        if self.pending_cf.is_some() {
            return lhs_value;
        }
        let lhs_variant = lhs_value.variant_name();
        let lhs = match lhs_value {
            Value::Bool(b) => b,
            _ => unreachable!(
                "short-circuit `{:?}` LHS at {}:{} was Value::{} not Bool; \
                 either an interpreter codepath produced the wrong variant or \
                 the typechecker accepted a non-Bool operand",
                op, span.line, span.column, lhs_variant
            ),
        };
        match (op, lhs) {
            (BinOp::And, false) => Value::Bool(false),
            (BinOp::Or, true) => Value::Bool(true),
            (BinOp::And, true) | (BinOp::Or, false) => self.eval_expr_inner(right),
            _ => unreachable!("eval_short_circuit only handles And/Or"),
        }
    }

    /// Whether a struct / enum value opts into the ordered `<` `<=` `>` `>=`
    /// operators: a NON-GENERIC, NON-stdlib user type that derives `Ord` /
    /// `PartialOrd`. This mirrors codegen's `ord_orderable_types` gate (built
    /// from the user program's items) exactly (B-2026-07-03-7) so `karac run`
    /// and `karac build` agree on which aggregate comparisons lower. In
    /// particular BOTH reject the generic prelude enums (`Option`/`Result`,
    /// whose `TypeParam` payload the `karac_cmp` family can't order without the
    /// instantiation) and the non-generic baked prelude enums (`Ordering`,
    /// `MemoryOrdering` — never in the user's `program.items`), keeping parity.
    fn aggregate_is_orderable(&self, v: &Value) -> bool {
        let name = match v {
            Value::Struct { name, .. } => name,
            Value::EnumVariant { enum_name, .. } => enum_name,
            _ => return false,
        };
        let orderable = |generic_params: &[String],
                         derived: &std::collections::HashSet<String>,
                         stdlib: bool| {
            !stdlib
                && generic_params.is_empty()
                && (derived.contains("Ord") || derived.contains("PartialOrd"))
        };
        if let Some(info) = self.typecheck_result.struct_info.get(name) {
            return orderable(
                &info.generic_params,
                &info.derived_traits,
                info.defining_stdlib_origin,
            );
        }
        if let Some(info) = self.typecheck_result.enum_info.get(name) {
            return orderable(
                &info.generic_params,
                &info.derived_traits,
                info.defining_stdlib_origin,
            );
        }
        false
    }

    /// Q4 literal promotion for the tree-walker (B-2026-07-04-12): when one
    /// operand is a *direct, unsuffixed integer literal* (`ExprKind::Integer(_,
    /// None)`) and the other evaluates to a `Float`, promote the literal's
    /// `Value::Int` to `Value::Float` so the following `eval_binary` sees a
    /// homogeneous float pair — matching the typechecker (which re-types the
    /// literal to `f64`) and codegen (which lowers it as `1.0`). Only the
    /// literal side is promoted, and only for the arithmetic / comparison /
    /// equality ops the typechecker itself promotes; a non-literal `Int` operand
    /// (a genuine int/float variable mix — a hard type error since
    /// B-2026-07-04-11) is left untouched so it still errors under `run` rather
    /// than being silently coerced. A float literal is never demoted to an int
    /// (mirroring the typechecker's `can_promote` guard).
    pub(crate) fn promote_int_literal_for_float_peer(
        &self,
        op: &BinOp,
        left: &Expr,
        right: &Expr,
        l: Value,
        r: Value,
    ) -> (Value, Value) {
        let is_promotable = matches!(
            op,
            BinOp::Add
                | BinOp::Sub
                | BinOp::Mul
                | BinOp::Div
                | BinOp::Mod
                | BinOp::Lt
                | BinOp::LtEq
                | BinOp::Gt
                | BinOp::GtEq
                | BinOp::Eq
                | BinOp::NotEq
        );
        if !is_promotable {
            return (l, r);
        }
        let is_unsuffixed_int_lit = |e: &Expr| matches!(&e.kind, ExprKind::Integer(_, None));
        match (&l, &r) {
            (Value::Int(iv), Value::Float(_)) if is_unsuffixed_int_lit(left) => {
                (Value::Float(*iv as f64), r)
            }
            (Value::Float(_), Value::Int(iv)) if is_unsuffixed_int_lit(right) => {
                (l, Value::Float(*iv as f64))
            }
            _ => (l, r),
        }
    }

    /// Re-round a narrow-float binop RESULT to its declared width.
    ///
    /// `Value::Float` is an f64 and carries no width tag, so every `f32` /
    /// `f16` / `bf16` operator computed and stored at f64 precision and kept
    /// bits the compiled backends do not have: `(4000000000u32 as f32) + 1.0`
    /// read 4000000001 under `--interp` and 4000000000 compiled, and a `bf16`
    /// — 8 mantissa bits — diverged on `1.0 + 0.01` (B-2026-08-14-7). The
    /// narrowing CASTS were given this treatment in B-2026-07-22-4 and the
    /// TENSOR element-wise path in B-2026-08-05-31 (`round_tensor_elems`, for
    /// the identical reason); the scalar arithmetic path is the third site and
    /// the last one holding f64 bits in a narrower slot.
    ///
    /// Computing at f64 and rounding once is not an approximation of computing
    /// at the narrow width — it is exact. A single `+`/`-`/`*`/`/` is
    /// correctly rounded when the intermediate carries at least `2p+2` bits,
    /// and f64's 53 clear that for f32 (50), f16 (24) and bf16 (18). Overflow
    /// to infinity survives the second rounding, and an f32-subnormal result
    /// is exact in f64 long before f64 itself underflows.
    ///
    /// A no-op for f64, for a non-float result (every comparison, every
    /// integer op), and for a span the typechecker recorded nothing at.
    pub(super) fn round_float_to_span_width(&self, v: Value, span: &Span) -> Value {
        use crate::typechecker::types::Type;
        let Value::Float(f) = v else {
            return v;
        };
        let key = crate::resolver::SpanKey::from_span(span);
        let Some(ty) = self.typecheck_result.expr_types.get(&key) else {
            return Value::Float(f);
        };
        // Same container peel as `span_int_width`: the element-wise
        // `Column[T] ⊕ x` / `Tensor[T, S] ⊕ x` / `Vector[T, N] ⊕ x` arms
        // recurse per slot with the CONTAINER expression's span, so the
        // narrow element width lives one level in.
        let ty = match ty {
            Type::Named { name, args }
                if (name == "Column" || name == "Tensor") && !args.is_empty() =>
            {
                &args[0]
            }
            Type::Vector { element, .. } => element.as_ref(),
            other => other,
        };
        match ty {
            Type::Float(size) => super::round_float_to_declared_size(f, *size),
            _ => Value::Float(f),
        }
    }

    /// `unsigned_hint` is the WIDTH at which the operands are unsigned, or
    /// `None`. See [`eval_binary_unrounded`]'s parameter doc for why a bool no
    /// longer suffices.
    pub(crate) fn eval_binary(
        &mut self,
        op: &BinOp,
        left: Value,
        right: Value,
        span: &Span,
        unsigned_hint: Option<u32>,
    ) -> Value {
        let v = self.eval_binary_unrounded(op, left, right, span, unsigned_hint);
        self.round_float_to_span_width(v, span)
    }

    fn eval_binary_unrounded(
        &mut self,
        op: &BinOp,
        left: Value,
        right: Value,
        span: &Span,
        // Caller-supplied "the operands are unsigned, at THIS width" hint.
        // Needed only for comparison operators, whose *result* type at `span`
        // is `bool` — so `span_unsigned_int_width(span)` can't recover the
        // operand signedness the way it can for arithmetic (whose result type
        // IS the unsigned operand type). Arithmetic / shift callers pass `None`
        // and let the span autodetect below do the work. B-2026-07-04-8; the
        // width replaced a bare bool in B-2026-08-19-23, when `u128` became a
        // second width needing the same reinterpretation and a bool could no
        // longer say which one was meant.
        unsigned_hint: Option<u32>,
    ) -> Value {
        let left_variant = left.variant_name();
        let right_variant = right.variant_name();
        // Unsigned 64-bit model (B-2026-07-04-8): the tree-walker stores every
        // integer width in the i64-carrier `Value::Int`, so a `u64` / `usize`
        // value ≥ 2⁶³ rides as a negative two's-complement i64. When the static
        // type says the operands are unsigned 64-bit, reinterpret the bits as
        // `u64` for the operators that differ (compare / div / rem / shr, and
        // the add/sub/mul overflow boundary) so `karac run` matches codegen's
        // `u{div,rem}` / `ugt` / `lshr` / `uadd.with.overflow` lowering. u8..u32
        // fit i64 non-negatively (signed == unsigned), so only 64-bit needs it.
        match unsigned_hint.or_else(|| self.span_unsigned_int_width(span)) {
            Some(64) => {
                if let (Value::Int(a), Value::Int(b)) = (&left, &right) {
                    if let Some(v) =
                        self.eval_binary_u64(op, narrow_to_i64(*a), narrow_to_i64(*b), span)
                    {
                        return v;
                    }
                }
            }
            // `u128` is the same model one width up: the carrier is signed, so
            // the top half of the range rides as a negative bit pattern and the
            // operators that differ have to read it back as `u128`. Without
            // this the signed arms answered `200e36 > 5` with `false` and
            // trapped `200e36 / 3` as "integer overflow" (B-2026-08-19-23).
            Some(128) => {
                if let (Value::Int(a), Value::Int(b)) = (&left, &right) {
                    if let Some(v) = self.eval_binary_u128(op, *a, *b, span) {
                        return v;
                    }
                }
            }
            _ => {}
        }
        match (op, left, right) {
            // Element-wise SIMD arithmetic on `Vector[T, N]` (design.md
            // § Portable SIMD, slice 1b). Recurse per lane pair so each lane
            // reuses the exact scalar Int/Float semantics (overflow check,
            // div-by-zero). The typechecker guarantees both sides are the same
            // Vector[T, N] and op ∈ {+,-,*,/,%}, and equal lane counts, so the
            // zip is total. Produces a fresh value-semantics Vector.
            (_, Value::Vector(a), Value::Vector(b)) => {
                let lanes: Vec<Value> = a
                    .into_iter()
                    .zip(b)
                    .map(|(x, y)| self.eval_binary(op, x, y, span, None))
                    .collect();
                Value::Vector(lanes)
            }

            // Element-wise arithmetic on `Tensor[T, Shape]` (design.md
            // § Numerical Types). Recurse per element so each element reuses
            // the exact scalar Int/Float semantics (overflow / div-by-zero).
            // Tensor⊕Tensor requires identical shapes — re-checked at runtime
            // because `run_program` bypasses the typechecker. Tensor⊕scalar
            // broadcasts the scalar across every element. The result is a
            // fresh value-semantics tensor; both operands are read, not moved.
            (
                _,
                Value::Tensor {
                    dims: ad,
                    data: ada,
                    elem: ae,
                },
                Value::Tensor {
                    dims: bd,
                    data: bda,
                    ..
                },
            ) => self.eval_tensor_tensor_binop(op, &ad, &ada, &bd, &bda, ae, span),
            (_, Value::Tensor { dims, data, elem }, scalar @ (Value::Int(_) | Value::Float(_))) => {
                self.eval_tensor_scalar_binop(op, &dims, &data, scalar, false, elem, span)
            }
            (_, scalar @ (Value::Int(_) | Value::Float(_)), Value::Tensor { dims, data, elem }) => {
                self.eval_tensor_scalar_binop(op, &dims, &data, scalar, true, elem, span)
            }

            // Element-wise three-valued-logic ops on `Column[T]` (phase-11
            // Arrow). Arithmetic `+ - * /` and comparison `== != < <= > >=`
            // share one mechanism: result validity = AND of the input
            // validities, and each valid slot's value is the recursive scalar
            // `eval_binary` (inheriting overflow / div-by-zero traps). A null
            // slot on either side → a null result slot (never `false` — the
            // 3VL essence). Both operands are read, not moved. Col-col first;
            // then col-scalar / scalar-col broadcast the scalar.
            (
                _,
                Value::Column {
                    data: ad,
                    valid: av,
                },
                Value::Column {
                    data: bd,
                    valid: bv,
                },
            ) => self.eval_column_column_binop(op, &ad, &av, &bd, &bv, span),
            (_, Value::Column { data, valid }, scalar) => {
                self.eval_column_scalar_binop(op, &data, &valid, scalar, false, span)
            }
            (_, scalar, Value::Column { data, valid }) => {
                self.eval_column_scalar_binop(op, &data, &valid, scalar, true, span)
            }

            // Arithmetic (Int). Computed in the i128 CARRIER, then range-checked
            // against the width the typechecker assigned at this span, which
            // traps `integer overflow` if the result does not fit (design.md
            // § Integer overflow — real fixed-width types). Codegen mirrors
            // this in `compile_narrow_int_binop`.
            //
            // EVERY width is checked, i64 included (B-2026-08-19-8 stage 1).
            // It used to be only the narrow ones, because the carrier was
            // itself an i64 and `checked_add` on it caught the 64-bit case for
            // free. Widening the carrier silently removed that — `i64::MAX + 1`
            // simply fits in i128 — so the 64-bit widths are explicit arms in
            // `narrow_oob` now. This is the whole reason the carrier change is
            // not a pure refactor.
            (BinOp::Add, Value::Int(a), Value::Int(b)) => match a.checked_add(b) {
                Some(v) if !self.narrow_oob(v, span) => Value::Int(v),
                _ => self.record_integer_overflow(span),
            },
            (BinOp::Sub, Value::Int(a), Value::Int(b)) => match a.checked_sub(b) {
                Some(v) if !self.narrow_oob(v, span) => Value::Int(v),
                _ => self.record_integer_overflow(span),
            },
            (BinOp::Mul, Value::Int(a), Value::Int(b)) => match a.checked_mul(b) {
                Some(v) if !self.narrow_oob(v, span) => Value::Int(v),
                _ => self.record_integer_overflow(span),
            },
            (BinOp::Div, Value::Int(a), Value::Int(b)) => {
                if b == 0 {
                    return self.record_runtime_error("division by zero", span);
                }
                if self.div_overflows_at_width(a, b, span) {
                    return self.record_integer_overflow(span);
                }
                match a.checked_div(b) {
                    Some(v) if !self.narrow_oob(v, span) => Value::Int(v),
                    _ => self.record_integer_overflow(span),
                }
            }
            (BinOp::Mod, Value::Int(a), Value::Int(b)) => {
                if b == 0 {
                    return self.record_runtime_error("division by zero", span);
                }
                // `%` needs the explicit width check, not just the range
                // check: `MIN % -1` is 0, which fits every width.
                if self.div_overflows_at_width(a, b, span) {
                    return self.record_integer_overflow(span);
                }
                match a.checked_rem(b) {
                    Some(v) if !self.narrow_oob(v, span) => Value::Int(v),
                    _ => self.record_integer_overflow(span),
                }
            }

            // Arithmetic (Float)
            (BinOp::Add, Value::Float(a), Value::Float(b)) => Value::Float(a + b),
            (BinOp::Sub, Value::Float(a), Value::Float(b)) => Value::Float(a - b),
            (BinOp::Mul, Value::Float(a), Value::Float(b)) => Value::Float(a * b),
            (BinOp::Div, Value::Float(a), Value::Float(b)) => Value::Float(a / b),
            (BinOp::Mod, Value::Float(a), Value::Float(b)) => Value::Float(a % b),

            // String Concatenation
            (BinOp::Add, Value::String(a), Value::String(b)) => Value::String(a + &b),

            // Comparison (Int)
            (BinOp::Eq, Value::Int(a), Value::Int(b)) => Value::Bool(a == b),
            (BinOp::NotEq, Value::Int(a), Value::Int(b)) => Value::Bool(a != b),
            (BinOp::Lt, Value::Int(a), Value::Int(b)) => Value::Bool(a < b),
            (BinOp::LtEq, Value::Int(a), Value::Int(b)) => Value::Bool(a <= b),
            (BinOp::Gt, Value::Int(a), Value::Int(b)) => Value::Bool(a > b),
            (BinOp::GtEq, Value::Int(a), Value::Int(b)) => Value::Bool(a >= b),

            // Comparison (Float) - IEEE 754: NaN != NaN
            (BinOp::Eq, Value::Float(a), Value::Float(b)) => Value::Bool(a == b),
            (BinOp::NotEq, Value::Float(a), Value::Float(b)) => Value::Bool(a != b),
            (BinOp::Lt, Value::Float(a), Value::Float(b)) => Value::Bool(a < b),
            (BinOp::LtEq, Value::Float(a), Value::Float(b)) => Value::Bool(a <= b),
            (BinOp::Gt, Value::Float(a), Value::Float(b)) => Value::Bool(a > b),
            (BinOp::GtEq, Value::Float(a), Value::Float(b)) => Value::Bool(a >= b),

            // Comparison (TotalFloat) - total order: NaN == NaN, NaN sorts last
            (BinOp::Eq, Value::TotalFloat64(a), Value::TotalFloat64(b)) => {
                Value::Bool(a.total_cmp(&b).is_eq())
            }
            (BinOp::NotEq, Value::TotalFloat64(a), Value::TotalFloat64(b)) => {
                Value::Bool(!a.total_cmp(&b).is_eq())
            }
            (BinOp::Lt, Value::TotalFloat64(a), Value::TotalFloat64(b)) => {
                Value::Bool(a.total_cmp(&b).is_lt())
            }
            (BinOp::LtEq, Value::TotalFloat64(a), Value::TotalFloat64(b)) => {
                Value::Bool(!a.total_cmp(&b).is_gt())
            }
            (BinOp::Gt, Value::TotalFloat64(a), Value::TotalFloat64(b)) => {
                Value::Bool(a.total_cmp(&b).is_gt())
            }
            (BinOp::GtEq, Value::TotalFloat64(a), Value::TotalFloat64(b)) => {
                Value::Bool(!a.total_cmp(&b).is_lt())
            }
            (BinOp::Eq, Value::TotalFloat32(a), Value::TotalFloat32(b)) => {
                Value::Bool(a.total_cmp(&b).is_eq())
            }
            (BinOp::NotEq, Value::TotalFloat32(a), Value::TotalFloat32(b)) => {
                Value::Bool(!a.total_cmp(&b).is_eq())
            }
            // B-2026-07-22-11: F32 ordering was missing (only Eq/NotEq were
            // present), so `a < b` / `a > b` on an `F32` fell through to the
            // generic op error under `--interp`. Total order, mirroring the
            // `TotalFloat64` arms above.
            (BinOp::Lt, Value::TotalFloat32(a), Value::TotalFloat32(b)) => {
                Value::Bool(a.total_cmp(&b).is_lt())
            }
            (BinOp::LtEq, Value::TotalFloat32(a), Value::TotalFloat32(b)) => {
                Value::Bool(!a.total_cmp(&b).is_gt())
            }
            (BinOp::Gt, Value::TotalFloat32(a), Value::TotalFloat32(b)) => {
                Value::Bool(a.total_cmp(&b).is_gt())
            }
            (BinOp::GtEq, Value::TotalFloat32(a), Value::TotalFloat32(b)) => {
                Value::Bool(!a.total_cmp(&b).is_lt())
            }
            // F16 / Bf16 total-order wrappers — same total ordering as F32/F64.
            (BinOp::Eq, Value::TotalFloat16(a), Value::TotalFloat16(b)) => {
                Value::Bool(a.total_cmp(&b).is_eq())
            }
            (BinOp::NotEq, Value::TotalFloat16(a), Value::TotalFloat16(b)) => {
                Value::Bool(!a.total_cmp(&b).is_eq())
            }
            (BinOp::Lt, Value::TotalFloat16(a), Value::TotalFloat16(b)) => {
                Value::Bool(a.total_cmp(&b).is_lt())
            }
            (BinOp::LtEq, Value::TotalFloat16(a), Value::TotalFloat16(b)) => {
                Value::Bool(!a.total_cmp(&b).is_gt())
            }
            (BinOp::Gt, Value::TotalFloat16(a), Value::TotalFloat16(b)) => {
                Value::Bool(a.total_cmp(&b).is_gt())
            }
            (BinOp::GtEq, Value::TotalFloat16(a), Value::TotalFloat16(b)) => {
                Value::Bool(!a.total_cmp(&b).is_lt())
            }
            (BinOp::Eq, Value::TotalBFloat16(a), Value::TotalBFloat16(b)) => {
                Value::Bool(a.total_cmp(&b).is_eq())
            }
            (BinOp::NotEq, Value::TotalBFloat16(a), Value::TotalBFloat16(b)) => {
                Value::Bool(!a.total_cmp(&b).is_eq())
            }
            (BinOp::Lt, Value::TotalBFloat16(a), Value::TotalBFloat16(b)) => {
                Value::Bool(a.total_cmp(&b).is_lt())
            }
            (BinOp::LtEq, Value::TotalBFloat16(a), Value::TotalBFloat16(b)) => {
                Value::Bool(!a.total_cmp(&b).is_gt())
            }
            (BinOp::Gt, Value::TotalBFloat16(a), Value::TotalBFloat16(b)) => {
                Value::Bool(a.total_cmp(&b).is_gt())
            }
            (BinOp::GtEq, Value::TotalBFloat16(a), Value::TotalBFloat16(b)) => {
                Value::Bool(!a.total_cmp(&b).is_lt())
            }

            // Comparison (String) — lexicographic via Rust's `Ord for String`.
            // Matches the typechecker's builtin Ord registration for `String`
            // (see `register_builtin_impl("Ord", "String", ...)`).
            (BinOp::Eq, Value::String(a), Value::String(b)) => Value::Bool(a == b),
            (BinOp::NotEq, Value::String(a), Value::String(b)) => Value::Bool(a != b),
            (BinOp::Lt, Value::String(a), Value::String(b)) => Value::Bool(a < b),
            (BinOp::LtEq, Value::String(a), Value::String(b)) => Value::Bool(a <= b),
            (BinOp::Gt, Value::String(a), Value::String(b)) => Value::Bool(a > b),
            (BinOp::GtEq, Value::String(a), Value::String(b)) => Value::Bool(a >= b),

            // Comparison (Char) — codepoint order via Rust's `Ord for char`.
            // Matches the typechecker's builtin Ord registration for `char`.
            (BinOp::Eq, Value::Char(a), Value::Char(b)) => Value::Bool(a == b),
            (BinOp::NotEq, Value::Char(a), Value::Char(b)) => Value::Bool(a != b),
            (BinOp::Lt, Value::Char(a), Value::Char(b)) => Value::Bool(a < b),
            (BinOp::LtEq, Value::Char(a), Value::Char(b)) => Value::Bool(a <= b),
            (BinOp::Gt, Value::Char(a), Value::Char(b)) => Value::Bool(a > b),
            (BinOp::GtEq, Value::Char(a), Value::Char(b)) => Value::Bool(a >= b),

            // Logical (Bool)
            (BinOp::And, Value::Bool(a), Value::Bool(b)) => Value::Bool(a && b),
            (BinOp::Or, Value::Bool(a), Value::Bool(b)) => Value::Bool(a || b),
            (BinOp::Eq, Value::Bool(a), Value::Bool(b)) => Value::Bool(a == b),
            (BinOp::NotEq, Value::Bool(a), Value::Bool(b)) => Value::Bool(a != b),
            (BinOp::Lt, Value::Bool(a), Value::Bool(b)) => Value::Bool(!a & b),
            (BinOp::LtEq, Value::Bool(a), Value::Bool(b)) => Value::Bool(a <= b),
            (BinOp::Gt, Value::Bool(a), Value::Bool(b)) => Value::Bool(a & !b),
            (BinOp::GtEq, Value::Bool(a), Value::Bool(b)) => Value::Bool(a >= b),

            // Bitwise (Int)
            (BinOp::BitAnd, Value::Int(a), Value::Int(b)) => Value::Int(a & b),
            (BinOp::BitOr, Value::Int(a), Value::Int(b)) => Value::Int(a | b),
            (BinOp::BitXor, Value::Int(a), Value::Int(b)) => Value::Int(a ^ b),
            // Shifts run at the operand's DECLARED width and trap on an
            // out-of-range amount — design.md § 2142. B-2026-08-06-7.
            (BinOp::Shl, Value::Int(a), Value::Int(b)) => {
                match self.shift_at_width(true, a, b, span) {
                    Some(v) => Value::Int(v),
                    None => self.record_runtime_error("shift amount out of range", span),
                }
            }
            (BinOp::Shr, Value::Int(a), Value::Int(b)) => {
                match self.shift_at_width(false, a, b, span) {
                    Some(v) => Value::Int(v),
                    None => self.record_runtime_error("shift amount out of range", span),
                }
            }

            // Structural equality on aggregates — enum variants and structs.
            // `Value`'s hand-written `PartialEq` already compares these
            // structurally (recursing into payloads/fields, including nested
            // String/Vec/enum values), so `==`/`!=` delegate to it. The
            // typechecker gates these on the operand type deriving `Eq`
            // (a warning otherwise); reaching here means two same-shape
            // aggregates. Without these arms enum/struct `==` fell through to
            // the `unreachable!` below (every enum, incl. Option/Result/
            // Ordering, panicked on `==`).
            (BinOp::Eq, l @ Value::EnumVariant { .. }, r @ Value::EnumVariant { .. }) => {
                Value::Bool(l == r)
            }
            (BinOp::NotEq, l @ Value::EnumVariant { .. }, r @ Value::EnumVariant { .. }) => {
                Value::Bool(l != r)
            }
            (BinOp::Eq, l @ Value::Struct { .. }, r @ Value::Struct { .. }) => Value::Bool(l == r),
            (BinOp::NotEq, l @ Value::Struct { .. }, r @ Value::Struct { .. }) => {
                Value::Bool(l != r)
            }
            // Ordered comparison (`<`, `<=`, `>`, `>=`) on aggregates — struct /
            // enum, by derived-`Ord` DECLARATION order via `value_compare`
            // (B-2026-07-03-7). `value_compare` consults the per-thread
            // `type_order` registry, so the result matches codegen's
            // `karac_cmp_<T>` family and `Vec[Struct].sort()`. The typechecker
            // gates these on the operand deriving `PartialOrd`/`Ord`; reaching
            // here means two same-shape aggregates.
            (
                BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq,
                l @ (Value::Struct { .. } | Value::EnumVariant { .. }),
                r @ (Value::Struct { .. } | Value::EnumVariant { .. }),
            ) if self.aggregate_is_orderable(&l) && self.aggregate_is_orderable(&r) => {
                let ord = super::helpers::value_compare(&l, &r);
                let b = match op {
                    BinOp::Lt => ord.is_lt(),
                    BinOp::LtEq => ord.is_le(),
                    BinOp::Gt => ord.is_gt(),
                    _ => ord.is_ge(),
                };
                Value::Bool(b)
            }
            // `shared struct` equality is structural (design.md § Equality
            // Semantics): `Value`'s `PartialEq` recurses through the inner
            // fields (`Arc::ptr_eq` fast path for identical allocations). The
            // typechecker gates these on the operand deriving `Eq`, same as the
            // plain-struct arms above; without them shared-struct `==` fell
            // through to the `_` runtime-error arm.
            (BinOp::Eq, l @ Value::SharedStruct(_), r @ Value::SharedStruct(_)) => {
                Value::Bool(l == r)
            }
            (BinOp::NotEq, l @ Value::SharedStruct(_), r @ Value::SharedStruct(_)) => {
                Value::Bool(l != r)
            }

            // No valid program reaches here — the typechecker rejects every
            // ill-typed operand combination as a hard error. The one way in is
            // `karac run`, which deliberately demotes typecheck errors to
            // warnings and executes anyway (see `run_program`). On that path an
            // illegal operand (e.g. `String * Int`) must surface as a graceful
            // runtime error, NOT an interpreter `unreachable!()` panic.
            _ => self.record_runtime_error(
                format!(
                    "operator '{:?}' is not defined for operands of type '{}' and '{}' \
                     (this is a type error the typechecker reports as a hard error; \
                     it reached the interpreter only because `karac run` executes despite \
                     typecheck errors)",
                    op, left_variant, right_variant
                ),
                span,
            ),
        }
    }

    /// Interpreter twin of codegen's `emit_elementwise_map` loop (S3,
    /// `src/codegen/kernel.rs`): fold each **present** slot through the
    /// scalar `eval_binary` — inheriting the exact scalar semantics (int
    /// overflow trap, div-by-zero trap) and the `pending_cf` early-out — and
    /// stamp a `None` slot (a null under SQL null propagation) with a
    /// never-read `Value::Unit` placeholder + an invalid bit. All four
    /// element-wise binop paths (tensor⊕tensor, tensor⊕scalar, col⊕col,
    /// col⊕scalar) build their slot vector and funnel through here. Returns
    /// `None` when control flow pended mid-loop.
    #[allow(clippy::type_complexity)]
    fn map_binop_slots(
        &mut self,
        op: &BinOp,
        slots: Vec<Option<(Value, Value)>>,
        span: &Span,
    ) -> Option<(Vec<Value>, Vec<bool>)> {
        let mut data = Vec::with_capacity(slots.len());
        let mut valid = Vec::with_capacity(slots.len());
        for slot in slots {
            match slot {
                Some((l, r)) => {
                    data.push(self.eval_binary(op, l, r, span, None));
                    if self.pending_cf.is_some() {
                        return None;
                    }
                    valid.push(true);
                }
                None => {
                    data.push(Value::Unit);
                    valid.push(false);
                }
            }
        }
        Some((data, valid))
    }

    /// Order a broadcast pair by `scalar_on_left`, promoting an int scalar to
    /// float when the element is float — the Q4 literal-promotion case
    /// (`t + 2` on a float tensor): codegen sees a float literal via
    /// lowering's rewrite, and this keeps the interpreter byte-for-byte in
    /// step.
    fn broadcast_pair(x: Value, scalar: &Value, scalar_on_left: bool) -> (Value, Value) {
        let s = match (&x, scalar) {
            (Value::Float(_), Value::Int(i)) => Value::Float(*i as f64),
            _ => scalar.clone(),
        };
        if scalar_on_left {
            (s, x)
        } else {
            (x, s)
        }
    }

    /// Element-wise `Tensor ⊕ Tensor`. Runtime shape-equality re-check (the
    /// `run_program` bypass), then a fresh tensor whose elements are the
    /// per-position scalar results. Both buffers are cloned out before the
    /// loop so `a + a` (an aliased data `Arc`) can't deadlock on two read
    /// B-2026-08-05-31 — round every float slot to the tensor's element width.
    /// The interpreter stores all floats as f64, so an `f32` tensor has to be
    /// re-rounded after each element-wise op or its results drift away from
    /// codegen's packed f32 buffer (`0.1 * 3` differed in the 8th digit).
    /// A no-op for `F64` and for non-float slots.
    fn round_tensor_elems(elems: &mut [Value], elem: TensorElemWidth) {
        if elem == TensorElemWidth::F64 {
            return;
        }
        for v in elems.iter_mut() {
            if let Value::Float(f) = v {
                *v = Value::Float(elem.round(*f));
            }
        }
    }

    /// guards of one `RwLock`.
    #[allow(clippy::too_many_arguments)]
    fn eval_tensor_tensor_binop(
        &mut self,
        op: &BinOp,
        ad: &Arc<Vec<i64>>,
        ada: &Arc<RwLock<Vec<Value>>>,
        bd: &Arc<Vec<i64>>,
        bda: &Arc<RwLock<Vec<Value>>>,
        elem: TensorElemWidth,
        span: &Span,
    ) -> Value {
        if ad.as_ref() != bd.as_ref() {
            return self.record_runtime_error(
                format!(
                    "tensor shape mismatch in element-wise operator: {:?} vs {:?} \
                     (element-wise tensor arithmetic requires identical shapes)",
                    ad.as_ref(),
                    bd.as_ref()
                ),
                span,
            );
        }
        let a = ada.read().unwrap().clone();
        let b = bda.read().unwrap().clone();
        let slots = a.into_iter().zip(b).map(Some).collect();
        let Some((mut out, _)) = self.map_binop_slots(op, slots, span) else {
            return Value::Unit;
        };
        Self::round_tensor_elems(&mut out, elem);
        Value::Tensor {
            dims: ad.clone(),
            data: Arc::new(RwLock::new(out)),
            elem,
        }
    }

    /// Element-wise `Tensor ⊕ scalar` (or `scalar ⊕ Tensor` when
    /// `scalar_on_left`). Broadcasts the scalar across every element (with
    /// the int→float promotion of [`Self::broadcast_pair`]).
    #[allow(clippy::too_many_arguments)]
    fn eval_tensor_scalar_binop(
        &mut self,
        op: &BinOp,
        dims: &Arc<Vec<i64>>,
        data: &Arc<RwLock<Vec<Value>>>,
        scalar: Value,
        scalar_on_left: bool,
        elem: TensorElemWidth,
        span: &Span,
    ) -> Value {
        let elems = data.read().unwrap().clone();
        let slots = elems
            .into_iter()
            .map(|x| Some(Self::broadcast_pair(x, &scalar, scalar_on_left)))
            .collect();
        let Some((mut out, _)) = self.map_binop_slots(op, slots, span) else {
            return Value::Unit;
        };
        Self::round_tensor_elems(&mut out, elem);
        Value::Tensor {
            dims: dims.clone(),
            data: Arc::new(RwLock::new(out)),
            elem,
        }
    }

    /// Element-wise `Column ⊕ Column` with SQL null propagation (phase-11
    /// Arrow). Lengths must match (re-checked at runtime — `run_program`
    /// bypasses the typechecker). Each output slot is valid iff *both* inputs
    /// are valid; a valid slot recurses through the scalar `eval_binary`
    /// (inheriting overflow / div-by-zero traps), a null slot holds a
    /// never-read placeholder. Works for arithmetic (→ values) and
    /// comparison (→ bools) identically — the op decides the per-element type.
    fn eval_column_column_binop(
        &mut self,
        op: &BinOp,
        ad: &Arc<RwLock<Vec<Value>>>,
        av: &Arc<RwLock<Vec<bool>>>,
        bd: &Arc<RwLock<Vec<Value>>>,
        bv: &Arc<RwLock<Vec<bool>>>,
        span: &Span,
    ) -> Value {
        let a = ad.read().unwrap().clone();
        let b = bd.read().unwrap().clone();
        let avalid = av.read().unwrap().clone();
        let bvalid = bv.read().unwrap().clone();
        if avalid.len() != bvalid.len() {
            return self.record_runtime_error(
                format!(
                    "column length mismatch in element-wise operator: {} vs {} \
                     (element-wise column ops require equal lengths)",
                    avalid.len(),
                    bvalid.len()
                ),
                span,
            );
        }
        let slots = a
            .into_iter()
            .zip(b)
            .zip(avalid.iter())
            .zip(bvalid.iter())
            .map(|(((x, y), &ok_a), &ok_b)| (ok_a && ok_b).then_some((x, y)))
            .collect();
        let Some((out_data, out_valid)) = self.map_binop_slots(op, slots, span) else {
            return Value::Unit;
        };
        Value::Column {
            data: Arc::new(RwLock::new(out_data)),
            valid: Arc::new(RwLock::new(out_valid)),
        }
    }

    /// Element-wise `Column ⊕ scalar` (or `scalar ⊕ Column` when
    /// `scalar_on_left`) with null propagation. Valid slots compute against
    /// the broadcast scalar (with the int→float promotion of
    /// [`Self::broadcast_pair`], mirroring the Tensor scalar path); null
    /// slots stay null.
    fn eval_column_scalar_binop(
        &mut self,
        op: &BinOp,
        data: &Arc<RwLock<Vec<Value>>>,
        valid: &Arc<RwLock<Vec<bool>>>,
        scalar: Value,
        scalar_on_left: bool,
        span: &Span,
    ) -> Value {
        let elems = data.read().unwrap().clone();
        let valids = valid.read().unwrap().clone();
        let slots = valids
            .iter()
            .zip(elems)
            .map(|(&ok, x)| ok.then(|| Self::broadcast_pair(x, &scalar, scalar_on_left)))
            .collect();
        let Some((out_data, _)) = self.map_binop_slots(op, slots, span) else {
            return Value::Unit;
        };
        Value::Column {
            data: Arc::new(RwLock::new(out_data)),
            valid: Arc::new(RwLock::new(valids)),
        }
    }

    /// Project `member` out of an optional chain's unwrapped payload — a
    /// field read when `a?.f`, a real method call when `a?.m(args)`.
    ///
    /// B-2026-08-17-28 — the method form used to be a FIELD lookup with the
    /// argument list thrown away, which found nothing and produced `Unit`.
    /// Routing it through `eval_method_call` is what makes `c?.label()` mean
    /// the same call as `c.label()`; binding the payload to a scope-local
    /// synthetic name is how an already-evaluated `Value` is handed to an
    /// evaluator whose interface takes an `Expr`. The name is not a valid
    /// Kara identifier, so it cannot shadow anything the author wrote, and
    /// the scope is popped before returning.
    pub(crate) fn optional_chain_project(
        &mut self,
        payload: Value,
        member: &str,
        args: &Option<Vec<CallArg>>,
        expr: &Expr,
    ) -> Value {
        let Some(call_args) = args else {
            // Field form: read it straight off the struct.
            return match payload {
                Value::Struct { fields, .. } => fields.get(member).cloned().unwrap_or(Value::Unit),
                _ => Value::Unit,
            };
        };

        const RECV: &str = "__optional_chain_recv";
        self.env.push_scope();
        self.env.define(RECV.to_string(), payload);
        let recv_expr = Expr {
            span: expr.span,
            kind: ExprKind::Identifier(RECV.to_string()),
        };
        let out = self.eval_method_call(&recv_expr, member, call_args, &expr.span, &expr.span);
        self.env.pop_scope();
        out
    }

    /// Wrap an optional chain's projected value back into the chain's
    /// `Option`, FLATTENING when the projection was already one.
    ///
    /// `a?.f` yields `Option[U]`, never `Option[Option[U]]` — that is what
    /// lets the next `?.` in `a?.f?.g` project from the payload instead of
    /// from a wrapper. Without it the spec's own `user.address?.city?.name`
    /// could not work at all (B-2026-08-17-28).
    pub(crate) fn rewrap_optional_chain(projected: Value) -> Value {
        if matches!(&projected, Value::EnumVariant { enum_name, .. } if enum_name == "Option") {
            return projected;
        }
        Value::EnumVariant {
            enum_name: "Option".to_string(),
            variant: "Some".to_string(),
            data: EnumData::Tuple(vec![projected]),
        }
    }

    /// The `[lo, hi)` bounds a `Range` VALUE stands for, or `None` if the
    /// value is not one.
    ///
    /// B-2026-08-18-3 — a range value is modelled as an eagerly materialized
    /// `Value::Iterator`, so it carries its elements rather than its bounds.
    /// The index path needs bounds, and the elements determine them exactly:
    /// the typechecker admits only a contiguous ascending range as an index
    /// (a stepped or filtered iterator is rejected with a span), so the first
    /// remaining element is the start and one past the last is the end.
    ///
    /// `cursor` is respected, so a partially consumed range slices from where
    /// it now stands rather than from where it began. An exhausted or empty
    /// range yields an empty half-open pair, which slices to nothing — the
    /// right answer for `3..3` and for a descending `3..1` alike.
    pub(crate) fn range_value_bounds(v: &Value) -> Option<(i64, i64)> {
        let Value::Iterator {
            source: super::value::IteratorSource::Eager { items, cursor },
            steps,
        } = v
        else {
            return None;
        };
        // An adaptor chain (`.map`, `.filter`, …) no longer stands for a
        // contiguous span, and the typechecker rejects such a value in index
        // position anyway — decline rather than invent bounds for it.
        if !steps.is_empty() {
            return None;
        }
        let rest = items.get(*cursor..)?;
        let mut bounds: Option<(i64, i64)> = None;
        for item in rest {
            let Value::Int(n) = item else { return None };
            bounds = Some(match bounds {
                None => (narrow_to_i64(*n), narrow_to_i64(*n + 1)),
                Some((lo, _)) => (lo, narrow_to_i64(*n + 1)),
            });
        }
        Some(bounds.unwrap_or((0, 0)))
    }

    pub(crate) fn eval_pipe(&mut self, left: &Expr, right: &Expr, span: &Span) -> Value {
        // Shared with the typechecker and codegen (B-2026-08-17-25) — see
        // `ast::desugar_pipe`. The synthesized call carries the PIPE's span,
        // which is where the typechecker recorded this call's facts; the
        // hand-rolled copy this replaced built it at `right.span` instead.
        match desugar_pipe(left, right, *span) {
            Some(desugared) => self.eval_expr_inner(&desugared),
            None => unreachable!(
                "invalid pipe right-hand side at {}:{}; should be caught by parser/typechecker",
                right.span.line, right.span.column
            ),
        }
    }

    /// True when the i64 result `v` does not fit the *narrow* integer type
    /// the typechecker assigned to the expression at `span` (`u8`..`u32` /
    /// `i8`..`i32`). A no-op (false) for `i64`/`u64`/`usize`/`isize`, non-
    /// narrow, and untyped spans — so only genuinely narrow-typed arithmetic
    /// is range-checked. Codegen mirrors this in `compile_narrow_int_binop`.
    pub(super) fn narrow_oob(&self, v: i128, span: &Span) -> bool {
        let (lo, hi) = self.span_int_bounds(span);
        v < lo || v > hi
    }

    /// Would a division-family op on `(a, b)` overflow the DECLARED width?
    ///
    /// `MIN / -1` overflows every signed width, and for `%` / `rem_euclid` the
    /// RESULT is `0` — which fits — so range-checking the result cannot see it
    /// (B-2026-08-19-8 stage 1). While the carrier was an i64 this was free:
    /// `checked_rem` on the carrier returned `None` because the intermediate
    /// division overflowed the carrier itself. A wider carrier computes it
    /// happily, so the case is explicit now. Matches Rust's
    /// `i64::checked_rem` / `checked_rem_euclid`, and codegen's `sdiv`/`srem`
    /// trap pair.
    pub(super) fn div_overflows_at_width(&self, a: i128, b: i128, span: &Span) -> bool {
        b == -1 && a == self.span_int_bounds(span).0
    }

    /// Inclusive `(min, max)` of the integer type the typechecker assigned at
    /// `span`, defaulting to i64's range. See [`Self::narrow_oob`].
    fn span_int_bounds(&self, span: &Span) -> (i128, i128) {
        use crate::typechecker::types::{IntSize, Type, UIntSize};
        let key = crate::resolver::SpanKey::from_span(span);
        // A span with no recorded type degrades to the i64 default rather than
        // to "no check" (B-2026-08-19-8 stage 1). `karac run` populates
        // `expr_types` sparsely, and while the carrier WAS an i64 those spans
        // still trapped, because the carrier overflowed on its own. Returning
        // false here now would silently drop overflow detection for exactly
        // the spans the typechecker did not annotate.
        let Some(ty) = self.typecheck_result.expr_types.get(&key) else {
            return (i64::MIN as i128, i64::MAX as i128);
        };
        // B-2026-07-01-3: element-wise `Column[T] ⊕ x` / `Tensor[T, S] ⊕ x`
        // recurses through the scalar arms with the CONTAINER expression's
        // span, whose recorded type is `Column[i32]`-shaped — peel down to
        // the element so a narrow-element container op range-checks exactly
        // like the scalar op (codegen already traps at the element's LLVM
        // width; the interpreter silently produced out-of-range values).
        let ty = match ty {
            Type::Named { name, args }
                if (name == "Column" || name == "Tensor") && !args.is_empty() =>
            {
                &args[0]
            }
            other => other,
        };
        match ty {
            Type::Int(IntSize::I8) => (-128, 127),
            Type::Int(IntSize::I16) => (-32768, 32767),
            Type::Int(IntSize::I32) => (-2_147_483_648, 2_147_483_647),
            Type::UInt(UIntSize::U8) => (0, 255),
            Type::UInt(UIntSize::U16) => (0, 65_535),
            Type::UInt(UIntSize::U32) => (0, 4_294_967_295),
            // The 64-bit widths, which the i64 carrier used to check for free.
            Type::Int(IntSize::I64) => (i64::MIN as i128, i64::MAX as i128),
            Type::UInt(UIntSize::U64) | Type::UInt(UIntSize::Usize) => (0, u64::MAX as i128),
            // The carrier is SIGNED (`Value::Int(i128)`), so `u128`'s usable
            // ceiling is `i128::MAX`: the upper half of the range is stored as
            // a negative bit pattern that this check would reject and that
            // `println` renders with its signed reading. Tracked separately as
            // B-2026-08-19-23 — it needs the carrier to grow an unsigned half,
            // not a wider bound here.
            Type::Int(IntSize::I128) => (i128::MIN, i128::MAX),
            Type::UInt(UIntSize::U128) => (0, i128::MAX),
            // Anything else (a generic param, a named type, a float result
            // type) keeps the i64 default rather than skipping the check, for
            // the same reason the missing-type branch above does.
            _ => (i64::MIN as i128, i64::MAX as i128),
        }
    }

    /// The DECLARED bit width of the integer type the typechecker assigned at
    /// `span`, and whether it is unsigned — `(64, _)` for `i64`/`u64`/`usize`/
    /// `isize`, for a non-integer type, and for an untyped span (`karac run`
    /// populates `expr_types` sparsely, so degrading to the i64 default keeps
    /// it graceful, exactly as [`Self::narrow_oob`] does). B-2026-08-06-7.
    ///
    /// Shifts need the width that `narrow_oob` only needs a range for, so this
    /// is deliberately a sibling rather than a rewrite of it: `narrow_oob`
    /// answers "does this result fit", this answers "how wide is the type",
    /// and the shift arms need the second question answered even for i64,
    /// where the first is always yes.
    fn span_int_width(&self, span: &Span) -> (u32, bool) {
        use crate::typechecker::types::{IntSize, Type, UIntSize};
        let key = crate::resolver::SpanKey::from_span(span);
        let Some(ty) = self.typecheck_result.expr_types.get(&key) else {
            return (64, false);
        };
        // Same container peel as `narrow_oob`, for the element-wise
        // `Column[T] ⊕ x` / `Tensor[T, S] ⊕ x` arms that recurse with the
        // CONTAINER expression's span (B-2026-07-01-3).
        let ty = match ty {
            Type::Named { name, args }
                if (name == "Column" || name == "Tensor") && !args.is_empty() =>
            {
                &args[0]
            }
            other => other,
        };
        match ty {
            Type::Int(IntSize::I8) => (8, false),
            Type::Int(IntSize::I16) => (16, false),
            Type::Int(IntSize::I32) => (32, false),
            Type::UInt(UIntSize::U8) => (8, true),
            Type::UInt(UIntSize::U16) => (16, true),
            Type::UInt(UIntSize::U32) => (32, true),
            Type::UInt(UIntSize::U64) | Type::UInt(UIntSize::Usize) => (64, true),
            // The 128-bit widths were missing, so every consumer of this
            // function saw them as signed 64. The visible consequence was the
            // SHIFT range check: `1i128 << 100` is legal, but `100 >= 64`
            // rejected it as "shift amount out of range" on both backends
            // (B-2026-08-19-23).
            Type::Int(IntSize::I128) => (128, false),
            Type::UInt(UIntSize::U128) => (128, true),
            _ => (64, false),
        }
    }

    /// `a << b` / `a >> b` at the operand's DECLARED width — design.md § 2142
    /// ("Shift by the bit width or more traps"; `(x: i32) << 31` is legal
    /// "regardless of whether it flips the sign bit"). B-2026-08-06-7.
    ///
    /// `Err` carries the already-recorded error value for an out-of-range
    /// amount, so the caller simply propagates it.
    ///
    /// Before this, both arms were a bare Rust `a << b` / `a >> b` on the
    /// i64 carrier. That is TWO defects: a shift amount >= 64 panicked the
    /// interpreter process outright ("attempt to shift left with overflow"),
    /// and a narrow shift computed at i64 so `1i32 << 31` yielded 2147483648
    /// — a value an `i32` cannot represent — instead of the specified
    /// -2147483648. Codegen had the matching pair, the first leg as LLVM
    /// poison.
    fn shift_at_width(&self, left: bool, a: i128, b: i128, span: &Span) -> Option<i128> {
        let (bits, is_unsigned) = self.span_int_width(span);
        if b < 0 || b >= bits as i128 {
            return None;
        }
        let sh = b as u32;
        // Compute on the u128 carrier and re-narrow, so `<<` is a bit shift at
        // the declared width (bits shifted past it are dropped, and the
        // declared width's sign bit is whatever landed there). Truncation is
        // what makes the result representable in the declared type — the
        // invariant every other narrow arm maintains.
        //
        // The carrier here is `u128` rather than `u64` because a 128-bit shift
        // has to be computable at all: at `u64` every amount ≥ 64 either
        // overflowed the shift or was rejected outright, so `1i128 << 100` was
        // unreachable (B-2026-08-19-23).
        let raw = if left {
            (a as u128) << sh
        } else if is_unsigned {
            (a as u128) >> sh
        } else {
            // Arithmetic shift: sign-extend from the DECLARED width first, so
            // a narrow negative value smears its own sign bit rather than the
            // carrier's.
            let sext = if bits == 128 {
                a
            } else {
                (a << (128 - bits)) >> (128 - bits)
            };
            (sext >> sh) as u128
        };
        Some(Self::truncate_to_width(raw, bits, is_unsigned))
    }

    /// Reinterpret the low `bits` of a u128 carrier as a value of the declared
    /// integer type, returned in the i128 carrier every `Value::Int` uses:
    /// sign-extended for a signed type, zero-extended for an unsigned one.
    /// A 128-bit width is already the carrier's own shape and passes through.
    fn truncate_to_width(raw: u128, bits: u32, is_unsigned: bool) -> i128 {
        // At 128 bits the pattern IS the carrier, signed or not.
        if bits >= 128 {
            return raw as i128;
        }
        // At 64 the carrier encoding predates the i128 widening and is
        // load-bearing: a `u64` at or above 2^63 is stored WRAPPED — the i64
        // reinterpretation, sign-extended into the carrier — which is what
        // `eval_binary_u64`'s `a as u64`, `literal_as_i128`'s read-back, and
        // the `%llu` display path all assume. Zero-extending it here instead
        // put a positive 2^63 in the carrier and the next `narrow_to_i64` on
        // the u64 path panicked (caught by `test_interp_u64_*`). Only the
        // 128-bit arm above is new; every width below behaves as it did.
        if bits >= 64 {
            return ((raw as u64) as i64) as i128;
        }
        let masked = raw & ((1u128 << bits) - 1);
        if is_unsigned {
            masked as i128
        } else {
            ((masked << (128 - bits)) as i128) >> (128 - bits)
        }
    }

    /// The WIDTH at which `span`'s type is unsigned, or `None` when it is
    /// signed / not an integer / unrecorded. `Some(64)` for `u64` / `usize`,
    /// `Some(128)` for `u128`.
    ///
    /// These are the two widths whose top half does not fit the signed carrier,
    /// so they are the two that need the operators that differ — compare, div,
    /// rem, `>>`, and the add/sub/mul overflow boundary — reinterpreted at
    /// their own width. `u8`..`u32` fit non-negatively (signed == unsigned), so
    /// they need nothing, which is why they are absent rather than forgotten.
    ///
    /// This replaced a bool-valued `span_type_is_unsigned64`, whose every
    /// consumer then read the carrier back at 64 bits. That was invisible while
    /// `u64` was the only unsigned width whose top half did not fit the
    /// carrier; `u128` is a second, and reading one at 64 bits keeps the low
    /// half — `u128::MAX` printed `-1`, sorted first, and compared as negative
    /// (B-2026-08-19-23).
    pub(crate) fn span_unsigned_int_width(&self, span: &Span) -> Option<u32> {
        let key = crate::resolver::SpanKey::from_span(span);
        let ty = self.typecheck_result.expr_types.get(&key)?;
        Self::type_unsigned_int_width(ty)
    }

    /// The type-level predicate behind [`span_unsigned_int_width`], peeling one
    /// container layer so a `Vec[u64]` / `Column[u128]` / … element counts.
    pub(crate) fn type_unsigned_int_width(ty: &crate::typechecker::types::Type) -> Option<u32> {
        use crate::typechecker::types::{Type, UIntSize};
        fn width(t: &Type) -> Option<u32> {
            match t {
                Type::UInt(UIntSize::U64) | Type::UInt(UIntSize::Usize) => Some(64),
                Type::UInt(UIntSize::U128) => Some(128),
                _ => None,
            }
        }
        match ty {
            Type::Array { element, .. }
            | Type::Vector { element, .. }
            | Type::Slice { element, .. } => width(element),
            Type::Named { name, args }
                if (name == "Vec"
                    || name == "Column"
                    || name == "Tensor"
                    || name == "Set"
                    || name == "SortedSet")
                    && !args.is_empty() =>
            {
                width(&args[0])
            }
            other => width(other),
        }
    }

    /// Evaluate the integer operators whose result differs between signed i64
    /// and unsigned u64 semantics, reinterpreting the i64-carrier bits `a` / `b`
    /// as `u64`. Returns `None` for operators that are bit-identical across
    /// signedness (`==` `!=` `&` `|` `^` `<<`) so the caller falls through to
    /// the shared signed arms. B-2026-07-04-8; mirrors codegen's unsigned
    /// lowering (`u{div,rem}`, `ugt`/`uge`/`ult`/`ule`, `lshr`,
    /// `uadd/usub/umul.with.overflow`).
    fn eval_binary_u64(&mut self, op: &BinOp, a: i64, b: i64, span: &Span) -> Option<Value> {
        let (ua, ub) = (a as u64, b as u64);
        let v: u64 = match op {
            // Overflow traps at the u64 boundary — codegen uses the
            // `u{add,sub,mul}.with.overflow` intrinsics (emit_checked_int_arith,
            // `is_unsigned = true`), so a sum ≥ 2⁶³ that overflows i64 but fits
            // u64 must NOT false-trap (the signed arms' `checked_*` would).
            BinOp::Add => match ua.checked_add(ub) {
                Some(v) => v,
                None => return Some(self.record_integer_overflow(span)),
            },
            BinOp::Sub => match ua.checked_sub(ub) {
                Some(v) => v,
                None => return Some(self.record_integer_overflow(span)),
            },
            BinOp::Mul => match ua.checked_mul(ub) {
                Some(v) => v,
                None => return Some(self.record_integer_overflow(span)),
            },
            BinOp::Div => {
                if ub == 0 {
                    return Some(self.record_runtime_error("division by zero", span));
                }
                ua / ub
            }
            BinOp::Mod => {
                if ub == 0 {
                    return Some(self.record_runtime_error("division by zero", span));
                }
                ua % ub
            }
            // Logical (zero-filling) right shift — codegen lowers u64 `>>` to
            // `lshr`; the signed arm's `a >> b` is an arithmetic (sign-
            // extending) shift and would smear the high bit. Shift amount `b`
            // mirrors the signed arm's raw form.
            BinOp::Shr => ua >> b,
            BinOp::Lt => return Some(Value::Bool(ua < ub)),
            BinOp::LtEq => return Some(Value::Bool(ua <= ub)),
            BinOp::Gt => return Some(Value::Bool(ua > ub)),
            BinOp::GtEq => return Some(Value::Bool(ua >= ub)),
            _ => return None,
        };
        Some(Value::Int((v as i64).into()))
    }

    /// The `u128` sibling of [`eval_binary_u64`] — same model, same operator
    /// set, one width up. `Value::Int`'s `i128` carrier holds a `u128` as its
    /// two's-complement bit pattern, so every operator whose answer depends on
    /// signedness has to reinterpret: the ordered comparisons, division and
    /// remainder, the logical right shift, and the add/sub/mul overflow
    /// boundary (a sum past `i128::MAX` that still fits `u128` must not
    /// false-trap the way the signed `checked_*` arms would).
    fn eval_binary_u128(&mut self, op: &BinOp, a: i128, b: i128, span: &Span) -> Option<Value> {
        let (ua, ub) = (a as u128, b as u128);
        let v: u128 = match op {
            BinOp::Add => match ua.checked_add(ub) {
                Some(v) => v,
                None => return Some(self.record_integer_overflow(span)),
            },
            BinOp::Sub => match ua.checked_sub(ub) {
                Some(v) => v,
                None => return Some(self.record_integer_overflow(span)),
            },
            BinOp::Mul => match ua.checked_mul(ub) {
                Some(v) => v,
                None => return Some(self.record_integer_overflow(span)),
            },
            BinOp::Div => {
                if ub == 0 {
                    return Some(self.record_runtime_error("division by zero", span));
                }
                ua / ub
            }
            BinOp::Mod => {
                if ub == 0 {
                    return Some(self.record_runtime_error("division by zero", span));
                }
                ua % ub
            }
            // Logical (zero-filling) right shift, matching codegen's `lshr`.
            // The amount is range-checked by the shift arm in
            // `eval_binary_unrounded`, which runs before this dispatch only for
            // the signed path — so guard here too rather than shifting by an
            // out-of-range amount (`u128 >> 128` is UB in Rust).
            BinOp::Shr => {
                if !(0..128).contains(&b) {
                    return Some(self.record_runtime_error("shift amount out of range", span));
                }
                ua >> b
            }
            BinOp::Lt => return Some(Value::Bool(ua < ub)),
            BinOp::LtEq => return Some(Value::Bool(ua <= ub)),
            BinOp::Gt => return Some(Value::Bool(ua > ub)),
            BinOp::GtEq => return Some(Value::Bool(ua >= ub)),
            _ => return None,
        };
        Some(Value::Int(v as i128))
    }

    pub(super) fn record_integer_overflow(&mut self, span: &Span) -> Value {
        self.record_runtime_error("integer overflow", span)
    }
}
