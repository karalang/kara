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

use super::value::Value;

impl<'a> super::Interpreter<'a> {
    // ── Operators ───────────────────────────────────────────────

    pub(crate) fn eval_unary(&mut self, op: &UnaryOp, operand: Value, span: &Span) -> Value {
        let operand_variant = operand.variant_name();
        match (op, operand) {
            (UnaryOp::Neg, Value::Int(i)) => Value::Int(match i.checked_neg() {
                Some(v) => v,
                None => return self.record_integer_overflow(span),
            }),
            (UnaryOp::Neg, Value::Float(f)) => Value::Float(-f),
            // Element-wise tensor negation — fold `-` over each element into a
            // fresh value-semantics tensor (the operand is read, not moved).
            (UnaryOp::Neg, Value::Tensor { dims, data }) => {
                let elems = data.read().unwrap().clone();
                let mut out = Vec::with_capacity(elems.len());
                for x in elems {
                    out.push(self.eval_unary(&UnaryOp::Neg, x, span));
                    if self.pending_cf.is_some() {
                        return Value::Unit;
                    }
                }
                Value::Tensor {
                    dims,
                    data: Arc::new(RwLock::new(out)),
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

    pub(crate) fn eval_binary(
        &mut self,
        op: &BinOp,
        left: Value,
        right: Value,
        span: &Span,
    ) -> Value {
        let left_variant = left.variant_name();
        let right_variant = right.variant_name();
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
                    .map(|(x, y)| self.eval_binary(op, x, y, span))
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
                },
                Value::Tensor {
                    dims: bd,
                    data: bda,
                },
            ) => self.eval_tensor_tensor_binop(op, &ad, &ada, &bd, &bda, span),
            (_, Value::Tensor { dims, data }, scalar @ (Value::Int(_) | Value::Float(_))) => {
                self.eval_tensor_scalar_binop(op, &dims, &data, scalar, false, span)
            }
            (_, scalar @ (Value::Int(_) | Value::Float(_)), Value::Tensor { dims, data }) => {
                self.eval_tensor_scalar_binop(op, &dims, &data, scalar, true, span)
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

            // Arithmetic (Int). Computed at i64; when the typechecker types
            // the operation as a *narrow* integer (`u8`..`u32`/`i8`..`i32`),
            // the result is range-checked against that width and traps
            // `integer overflow` if it does not fit (design.md § Integer
            // overflow — real fixed-width types). `narrow_oob` is a no-op for
            // i64/u64/usize/isize and non-narrow result types, preserving the
            // existing i64-overflow behavior. Codegen mirrors this in
            // `compile_narrow_int_binop`.
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
                match a.checked_div(b) {
                    Some(v) if !self.narrow_oob(v, span) => Value::Int(v),
                    _ => self.record_integer_overflow(span),
                }
            }
            (BinOp::Mod, Value::Int(a), Value::Int(b)) => {
                if b == 0 {
                    return self.record_runtime_error("division by zero", span);
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
            (BinOp::Shl, Value::Int(a), Value::Int(b)) => Value::Int(a << b),
            (BinOp::Shr, Value::Int(a), Value::Int(b)) => Value::Int(a >> b),

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
                    data.push(self.eval_binary(op, l, r, span));
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
    /// guards of one `RwLock`.
    fn eval_tensor_tensor_binop(
        &mut self,
        op: &BinOp,
        ad: &Arc<Vec<i64>>,
        ada: &Arc<RwLock<Vec<Value>>>,
        bd: &Arc<Vec<i64>>,
        bda: &Arc<RwLock<Vec<Value>>>,
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
        let Some((out, _)) = self.map_binop_slots(op, slots, span) else {
            return Value::Unit;
        };
        Value::Tensor {
            dims: ad.clone(),
            data: Arc::new(RwLock::new(out)),
        }
    }

    /// Element-wise `Tensor ⊕ scalar` (or `scalar ⊕ Tensor` when
    /// `scalar_on_left`). Broadcasts the scalar across every element (with
    /// the int→float promotion of [`Self::broadcast_pair`]).
    fn eval_tensor_scalar_binop(
        &mut self,
        op: &BinOp,
        dims: &Arc<Vec<i64>>,
        data: &Arc<RwLock<Vec<Value>>>,
        scalar: Value,
        scalar_on_left: bool,
        span: &Span,
    ) -> Value {
        let elems = data.read().unwrap().clone();
        let slots = elems
            .into_iter()
            .map(|x| Some(Self::broadcast_pair(x, &scalar, scalar_on_left)))
            .collect();
        let Some((out, _)) = self.map_binop_slots(op, slots, span) else {
            return Value::Unit;
        };
        Value::Tensor {
            dims: dims.clone(),
            data: Arc::new(RwLock::new(out)),
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

    pub(crate) fn eval_pipe(&mut self, left: &Expr, right: &Expr) -> Value {
        match &right.kind {
            // a |> f => f(a)
            ExprKind::Identifier(_) | ExprKind::Path { .. } => {
                let desugared = Expr {
                    span: right.span.clone(),
                    kind: ExprKind::Call {
                        callee: Box::new(right.clone()),
                        args: vec![CallArg {
                            label: None,
                            mut_marker: false,
                            value: left.clone(),
                            span: left.span.clone(),
                        }],
                    },
                };
                self.eval_expr_inner(&desugared)
            }

            // a |> f(args...) => f(a, args...) or f(args with _ replaced)
            ExprKind::Call { callee, args } => {
                let has_placeholder = args
                    .iter()
                    .any(|arg| matches!(arg.value.kind, ExprKind::PipePlaceholder));

                let desugared_args: Vec<CallArg> = if has_placeholder {
                    args.iter()
                        .map(|arg| {
                            if matches!(arg.value.kind, ExprKind::PipePlaceholder) {
                                CallArg {
                                    label: arg.label.clone(),
                                    mut_marker: false,
                                    value: left.clone(),
                                    span: left.span.clone(),
                                }
                            } else {
                                arg.clone()
                            }
                        })
                        .collect()
                } else {
                    let mut new_args = vec![CallArg {
                        label: None,
                        mut_marker: false,
                        value: left.clone(),
                        span: left.span.clone(),
                    }];
                    new_args.extend(args.iter().cloned());
                    new_args
                };

                let desugared = Expr {
                    span: right.span.clone(),
                    kind: ExprKind::Call {
                        callee: callee.clone(),
                        args: desugared_args,
                    },
                };
                self.eval_expr_inner(&desugared)
            }

            _ => unreachable!(
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
    fn narrow_oob(&self, v: i64, span: &Span) -> bool {
        use crate::typechecker::types::{IntSize, Type, UIntSize};
        let key = crate::resolver::SpanKey::from_span(span);
        let Some(ty) = self.typecheck_result.expr_types.get(&key) else {
            return false;
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
        let (lo, hi): (i64, i64) = match ty {
            Type::Int(IntSize::I8) => (-128, 127),
            Type::Int(IntSize::I16) => (-32768, 32767),
            Type::Int(IntSize::I32) => (-2_147_483_648, 2_147_483_647),
            Type::UInt(UIntSize::U8) => (0, 255),
            Type::UInt(UIntSize::U16) => (0, 65_535),
            Type::UInt(UIntSize::U32) => (0, 4_294_967_295),
            _ => return false,
        };
        v < lo || v > hi
    }

    fn record_integer_overflow(&mut self, span: &Span) -> Value {
        self.record_runtime_error("integer overflow", span)
    }
}
