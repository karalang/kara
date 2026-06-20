//! Expression evaluation: the big `eval_expr_inner` match on `ExprKind`.
//!
//! Handles literals, operators, identifiers/control flow, calls,
//! collection literals, comprehensions, struct/enum literals, match
//! expressions, loops, closures, and pipe expressions. Receiver-shape
//! method dispatch lives in `method_call.rs`; iterator-source stepping
//! lives in `iter_eval.rs`.
//!
//! Lives in a sibling `impl<'a> super::Interpreter<'a>` block.

use std::collections::HashSet;
use std::sync::Arc;

use crate::ast::*;
use crate::resolver::SpanKey;

use super::exec::{add_pattern_bindings, collect_free_idents_expr, ControlFlow};
use super::value::{primitive_const_to_value, EnumData, IteratorSource, Value};

impl<'a> super::Interpreter<'a> {
    pub(crate) fn eval_expr_inner(&mut self, expr: &Expr) -> Value {
        // If a control flow signal is pending, short-circuit
        if self.check_cf() {
            return Value::Unit;
        }
        match &expr.kind {
            // Literals
            ExprKind::Integer(i, _) => Value::Int(*i),
            ExprKind::Float(f, _) => Value::Float(*f),
            ExprKind::Bool(b) => Value::Bool(*b),
            ExprKind::CharLit(c) => Value::Char(*c),
            // `b'X'` evaluates as a u8 via the shared `Value::Int(i64)` carrier
            // (the typechecker has already classified the value as u8).
            ExprKind::ByteLit(b) => Value::Int(i64::from(*b)),
            ExprKind::StringLit(s) => Value::String(s.clone()),
            ExprKind::MultiStringLit(s) => Value::String(s.clone()),
            // `c"..."` — bytes exclude the trailing NUL (a codegen-level
            // artifact; design.md § C-String Literals pins `len()` to the
            // source byte count). See `Value::CStr`'s doc for the
            // representation rationale.
            ExprKind::CStringLit { bytes, .. } => Value::CStr(std::sync::Arc::new(bytes.clone())),
            ExprKind::InterpolatedStringLit(parts) => {
                let mut result = String::new();
                for part in parts {
                    match part {
                        crate::ast::ParsedInterpolationPart::Text(t) => result.push_str(t),
                        crate::ast::ParsedInterpolationPart::Expr(e) => {
                            // Render each interpolation through the unified
                            // `to_string` dispatch (one evaluation of `e`,
                            // matching codegen's `f"{e}"` → `e.to_string()`
                            // desugar) so a user `impl Display` takes effect
                            // exactly as an explicit `.to_string()` would;
                            // built-in types fall through to `display_render`
                            // inside that dispatch. GAP-W4.
                            match self.eval_method_call(e, "to_string", &[], &expr.span, &expr.span)
                            {
                                Value::String(s) => result.push_str(&s),
                                other => result.push_str(&self.display_render(&other)),
                            }
                        }
                    }
                }
                Value::String(result)
            }

            // Operators
            ExprKind::Binary { op, left, right } => {
                // Short-circuit `and`/`or` are documented design intent
                // (roadmap.md:425, 429) — RHS only evaluates when the
                // LHS doesn't already determine the result. Routed
                // through a helper to keep `eval_expr_inner`'s debug-
                // mode stack frame from bloating recursive callers.
                if matches!(op, BinOp::And | BinOp::Or) {
                    return self.eval_short_circuit(op, left, right, &expr.span);
                }
                let l = self.eval_expr_inner(left);
                // A faulted operand (index OOB, div-by-zero, unwrap of None,
                // …) sets `pending_cf` and yields a poison value; don't run
                // the op on it (which would hit `eval_binary`'s unreachable
                // for the wrong variant) — propagate the fault instead.
                if self.pending_cf.is_some() {
                    return l;
                }
                let r = self.eval_expr_inner(right);
                if self.pending_cf.is_some() {
                    return r;
                }
                self.eval_binary(op, l, r, &expr.span)
            }
            ExprKind::Unary { op, operand } => {
                let val = self.eval_expr_inner(operand);
                if self.pending_cf.is_some() {
                    return val;
                }
                self.eval_unary(op, val, &expr.span)
            }

            ExprKind::Identifier(name) => self.env.get(name).unwrap_or_else(|| {
                unreachable!(
                    "variable '{}' not found at {}:{}; should be caught by resolver",
                    name, expr.span.line, expr.span.column
                )
            }),

            ExprKind::Path { segments, .. } => {
                let full = segments.join(".");
                if let Some(v) = self.env.get(&full) {
                    return v;
                }
                // `ExitCode.SUCCESS` / `ExitCode.FAILURE` (Phase-8
                // entry-point contract Slice B). `ExitCode` is a
                // zero-cost `distinct type = i32`, so the constant is a
                // plain `Value::Int(0 / 1)`. Parsed as a 2-segment Path
                // (sibling to the typechecker / codegen intercepts).
                if segments.len() == 2 {
                    if let Some(code) =
                        crate::prelude::lookup_exitcode_const(&segments[0], &segments[1])
                    {
                        return Value::Int(code as i64);
                    }
                }
                // Type-parameter dispatch: `T.method` where `T` is bound to a
                // concrete type at the current call frame's substitution
                // stack. Look up `<concrete>.method` instead.
                if segments.len() == 2 {
                    if let Some(concrete) = self.resolve_type_param(&segments[0]) {
                        let key = format!("{}.{}", concrete, segments[1]);
                        if let Some(v) = self.env.get(&key) {
                            return v;
                        }
                    }
                }
                // Try just the last segment (enum variant, etc.)
                let last = segments.last().cloned().unwrap_or_default();
                if let Some(v) = self.env.get(&last) {
                    return v;
                }
                // A path that survived resolve + typecheck but has no
                // evaluation rule here — historically a special-cased
                // stdlib path (e.g. `String.new` before it was wired)
                // falling through the `eval_call` match. Degrade to a
                // structured runtime error instead of panicking so the
                // next unwired path fails with a span-carrying
                // diagnostic, per the no-panic diagnostics standard.
                self.record_runtime_error(
                    format!(
                        "internal: path '{full}' has no interpreter evaluation rule \
                         (accepted by the typechecker but not wired in the tree-walk \
                         interpreter) — please report this"
                    ),
                    &expr.span,
                )
            }

            ExprKind::SelfValue => self.env.get("self").unwrap_or_else(|| {
                unreachable!(
                    "'self' not found at {}:{}; should be caught by resolver",
                    expr.span.line, expr.span.column
                )
            }),

            ExprKind::Block(block) => match self.eval_block_inner(block) {
                Ok(v) => v,
                Err(cf) => self.set_cf(cf),
            },

            // Tuple
            ExprKind::Tuple(exprs) => {
                let vals: Vec<Value> = exprs.iter().map(|e| self.eval_expr_inner(e)).collect();
                Value::Tuple(vals)
            }

            // Array literal — synthesis mode produces Vec[T] in the type system;
            // both Array and Vec are represented as Value::Array at runtime.
            ExprKind::ArrayLiteral(elements) => {
                let vals: Vec<Value> = elements.iter().map(|e| self.eval_expr_inner(e)).collect();
                Value::array_of(vals)
            }

            // Prefix collection literal: `Vec[e1, e2, ...]` / `Array[e1, ...]`
            ExprKind::PrefixCollectionLiteral { items, .. } => {
                let vals: Vec<Value> = items.iter().map(|e| self.eval_expr_inner(e)).collect();
                Value::array_of(vals)
            }

            // Repeat literal: `[v; n]` / `Vec[v; n]` / `Array[v; n]`. Value
            // is evaluated once; the resulting `n` clones share the value's
            // structure (consistent with Rust's `[v; n]` semantics).
            ExprKind::RepeatLiteral { value, count, .. } => {
                let v = self.eval_expr_inner(value);
                let n = match self.eval_expr_inner(count) {
                    Value::Int(n) if n >= 0 => n as usize,
                    _ => 0,
                };
                Value::array_of(vec![v; n])
            }

            // Map literal
            ExprKind::MapLiteral(entries) => {
                let vals: Vec<(Value, Value)> = entries
                    .iter()
                    .map(|(k, v)| (self.eval_expr_inner(k), self.eval_expr_inner(v)))
                    .collect();
                Value::Map(vals)
            }

            // Struct literal
            ExprKind::StructLiteral {
                path,
                fields,
                spread,
            } => self.eval_struct_literal(path, fields, spread.as_deref()),

            // Field access
            ExprKind::FieldAccess { object, field } => {
                // Primitive-type associated constants — `i64.MAX` /
                // `f64.INFINITY` / etc. parse as `FieldAccess(Identifier("i64"),
                // "MAX")`. Intercept before `eval_expr_inner(object)` would
                // panic on the bare primitive identifier (which has no
                // env binding). Falls through to the normal field-access
                // path when the lookup misses (so a regular struct field
                // with the same shape still resolves correctly).
                if let ExprKind::Identifier(name) = &object.kind {
                    if let Some(cv) = crate::prelude::lookup_primitive_const(name, field) {
                        return primitive_const_to_value(cv);
                    }
                    // `ExitCode.SUCCESS` / `ExitCode.FAILURE` (Phase-8
                    // entry-point contract Slice B). The `ExitCode`
                    // distinct type is zero-cost — its runtime value IS
                    // the underlying `i32` — so the constant is a plain
                    // `Value::Int(0 / 1)`. Mirrors the typechecker /
                    // codegen sibling intercepts.
                    if let Some(code) = crate::prelude::lookup_exitcode_const(name, field) {
                        return Value::Int(code as i64);
                    }
                }
                let obj = self.eval_expr_inner(object);
                self.read_field(obj, field, &expr.span)
            }

            // Tuple index
            ExprKind::TupleIndex { object, index } => {
                let obj = self.eval_expr_inner(object);
                let obj_variant = obj.variant_name();
                match obj {
                    Value::Tuple(vals) => vals.get(*index as usize).cloned().unwrap_or_else(|| {
                        unreachable!(
                            "tuple index {} out of bounds (len {}) at {}:{}; \
                             either the typechecker missed an out-of-range index \
                             or an interpreter codepath produced a shorter tuple than the static type",
                            *index, vals.len(), expr.span.line, expr.span.column
                        )
                    }),
                    _ => unreachable!(
                        "tuple index on Value::{} at {}:{}; \
                         either an interpreter codepath produced a non-Tuple where one was expected \
                         or the typechecker accepted tuple indexing on a non-tuple",
                        obj_variant, expr.span.line, expr.span.column
                    ),
                }
            }

            // Array/map index
            ExprKind::Index { object, index } => {
                // Range indexing: `v[a..b]` — produce a Slice[T] (interpreter
                // models this as a Value::Array copy of the sub-range; the
                // type-erased interpreter doesn't distinguish slice vs. array
                // at runtime). Mutation through a mutable slice in the
                // interpreter does not propagate back to the source — the
                // compiled codegen has full aliasing semantics.
                if let ExprKind::Range {
                    start,
                    end,
                    inclusive,
                } = &index.kind
                {
                    let obj = self.eval_expr_inner(object);
                    // Evaluate optional bounds; absent start defaults to 0,
                    // absent end is resolved after we know the array length.
                    let start_i = if let Some(s) = start {
                        let v = self.eval_expr_inner(s);
                        let v_variant = v.variant_name();
                        match v {
                            Value::Int(n) if n >= 0 => n as usize,
                            Value::Int(n) => {
                                return self.record_runtime_error(
                                    format!("range start must be non-negative, got {}", n),
                                    &expr.span,
                                );
                            }
                            _ => unreachable!(
                                "range start at {}:{} was Value::{} not Int; \
                                 either an interpreter codepath produced the wrong variant \
                                 or the typechecker accepted a non-integer range start",
                                expr.span.line, expr.span.column, v_variant
                            ),
                        }
                    } else {
                        0
                    };
                    let obj_variant = obj.variant_name();
                    let (storage, source_len) = match &obj {
                        Value::Array(rc) => (rc.clone(), rc.read().unwrap().len()),
                        Value::Slice {
                            storage,
                            start,
                            len,
                            ..
                        } => {
                            // Re-slicing — produce a window into the same
                            // storage with offset adjustment.
                            let raw_end = if let Some(e) = end {
                                let v = self.eval_expr_inner(e);
                                let v_variant = v.variant_name();
                                match v {
                                    Value::Int(n) if n >= 0 => n as usize,
                                    Value::Int(n) => {
                                        return self.record_runtime_error(
                                            format!("range end must be non-negative, got {}", n),
                                            &expr.span,
                                        );
                                    }
                                    _ => unreachable!(
                                        "range end at {}:{} was Value::{} not Int; \
                                         either an interpreter codepath produced the wrong variant \
                                         or the typechecker accepted a non-integer range end",
                                        expr.span.line, expr.span.column, v_variant
                                    ),
                                }
                            } else {
                                *len
                            };
                            let end_i = if *inclusive { raw_end + 1 } else { raw_end };
                            if start_i > end_i || end_i > *len {
                                return self.record_runtime_error(
                                    format!(
                                        "slice bounds {}..{} out of range (len {})",
                                        start_i, end_i, len,
                                    ),
                                    &expr.span,
                                );
                            }
                            return Value::Slice {
                                storage: storage.clone(),
                                start: start + start_i,
                                len: end_i - start_i,
                                mutable: false,
                            };
                        }
                        Value::String(s) => {
                            // `s[a..b]` → a fresh substring `String` (not a
                            // Slice). Byte-offset range with UTF-8
                            // char-boundary validation: a non-boundary index
                            // is a runtime panic
                            // (`E_STRING_SLICE_NOT_AT_CHAR_BOUNDARY`), mirroring
                            // Rust's `&s[a..b]`. `a..=b` includes byte `b`
                            // (so the boundary check applies to `b + 1`).
                            let byte_len = s.len();
                            let raw_end = if let Some(e) = end {
                                let v = self.eval_expr_inner(e);
                                let v_variant = v.variant_name();
                                match v {
                                    Value::Int(n) if n >= 0 => n as usize,
                                    Value::Int(n) => {
                                        return self.record_runtime_error(
                                            format!("range end must be non-negative, got {}", n),
                                            &expr.span,
                                        );
                                    }
                                    _ => unreachable!(
                                        "range end at {}:{} was Value::{} not Int; \
                                         either an interpreter codepath produced the wrong variant \
                                         or the typechecker accepted a non-integer range end",
                                        expr.span.line, expr.span.column, v_variant
                                    ),
                                }
                            } else {
                                byte_len
                            };
                            let end_i = if *inclusive { raw_end + 1 } else { raw_end };
                            if start_i > end_i || end_i > byte_len {
                                return self.record_runtime_error(
                                    format!(
                                        "string slice bounds {}..{} out of range (len {})",
                                        start_i, end_i, byte_len,
                                    ),
                                    &expr.span,
                                );
                            }
                            if !s.is_char_boundary(start_i) || !s.is_char_boundary(end_i) {
                                return self.record_runtime_error(
                                    format!(
                                        "E_STRING_SLICE_NOT_AT_CHAR_BOUNDARY: byte range \
                                         {}..{} does not fall on UTF-8 char boundaries of {:?}",
                                        start_i, end_i, s,
                                    ),
                                    &expr.span,
                                );
                            }
                            return Value::String(s[start_i..end_i].to_string());
                        }
                        _ => unreachable!(
                            "range-indexing on Value::{} at {}:{}; \
                             either an interpreter codepath produced a non-array/non-slice value \
                             or the typechecker accepted range-indexing on an unindexable type",
                            obj_variant, expr.span.line, expr.span.column
                        ),
                    };
                    let raw_end = if let Some(e) = end {
                        let v = self.eval_expr_inner(e);
                        let v_variant = v.variant_name();
                        match v {
                            Value::Int(n) if n >= 0 => n as usize,
                            Value::Int(n) => {
                                return self.record_runtime_error(
                                    format!("range end must be non-negative, got {}", n),
                                    &expr.span,
                                );
                            }
                            _ => unreachable!(
                                "range end at {}:{} was Value::{} not Int; \
                                 either an interpreter codepath produced the wrong variant \
                                 or the typechecker accepted a non-integer range end",
                                expr.span.line, expr.span.column, v_variant
                            ),
                        }
                    } else {
                        source_len
                    };
                    let end_i = if *inclusive { raw_end + 1 } else { raw_end };
                    if start_i > end_i || end_i > source_len {
                        return self.record_runtime_error(
                            format!(
                                "slice bounds {}..{} out of range (len {})",
                                start_i, end_i, source_len,
                            ),
                            &expr.span,
                        );
                    }
                    return Value::Slice {
                        storage,
                        start: start_i,
                        len: end_i - start_i,
                        mutable: false,
                    };
                }
                let obj = self.eval_expr_inner(object);
                let idx = self.eval_expr_inner(index);
                // Phase-11 Tensor multi-dim indexing — `t[i, j, k]`
                // arrives as a tuple index (parser desugar); rank-1
                // accepts a bare Int. Row-major offset + runtime bounds
                // checks via `tensor_offset`.
                if let Value::Tensor { dims, data } = &obj {
                    let Some(components) =
                        crate::interpreter::method_call_tensor::index_components(&idx)
                    else {
                        return self.record_runtime_error(
                            format!(
                                "tensor index must be integers (one per dim), got {}",
                                idx.variant_name()
                            ),
                            &expr.span,
                        );
                    };
                    return match crate::interpreter::method_call_tensor::tensor_offset(
                        dims,
                        &components,
                    ) {
                        Ok(off) => data.read().unwrap()[off].clone(),
                        Err(msg) => self.record_runtime_error(msg, &expr.span),
                    };
                }
                match (&obj, &idx) {
                    (Value::Array(rc), Value::Int(i)) => {
                        let i = *i as usize;
                        let vals = rc.read().unwrap();
                        let len = vals.len();
                        vals.get(i).cloned().unwrap_or_else(|| {
                            self.record_runtime_error(
                                format!("index {} out of bounds (len {})", i, len),
                                &expr.span,
                            )
                        })
                    }
                    (
                        Value::Slice {
                            storage,
                            start,
                            len,
                            ..
                        },
                        Value::Int(i),
                    ) => {
                        let i = *i as usize;
                        if i >= *len {
                            return self.record_runtime_error(
                                format!("index {} out of bounds (len {})", i, len),
                                &expr.span,
                            );
                        }
                        let vals = storage.read().unwrap();
                        vals[start + i].clone()
                    }
                    // `Vector[T, N]` lane read `v[i] -> T` (design.md § Portable
                    // SIMD, slice 1b). Value semantics — returns the lane by
                    // clone; out-of-range panics, matching the codegen
                    // bounds-check.
                    (Value::Vector(lanes), Value::Int(i)) => {
                        let i = *i as usize;
                        let len = lanes.len();
                        lanes.get(i).cloned().unwrap_or_else(|| {
                            self.record_runtime_error(
                                format!("vector lane index {} out of bounds (lanes {})", i, len),
                                &expr.span,
                            )
                        })
                    }
                    _ => unreachable!(
                        "index expression at {}:{}: obj=Value::{}, index=Value::{}; \
                         either an interpreter codepath produced wrong variants \
                         (e.g. a no-op cast left a non-Int where the typechecker blessed an Int) \
                         or the typechecker accepted an unindexable operand pair",
                        expr.span.line,
                        expr.span.column,
                        obj.variant_name(),
                        idx.variant_name()
                    ),
                }
            }

            // Function calls
            ExprKind::Call { callee, args } => self.eval_call(callee, args, &expr.span),

            // Method calls
            ExprKind::MethodCall {
                object,
                method,
                args,
                args_close_span,
                ..
            } => self.eval_method_call(object, method, args, &expr.span, args_close_span),

            // If/else
            ExprKind::If {
                condition,
                then_block,
                else_branch,
            } => {
                let cond = self.eval_expr_inner(condition);
                if self.is_truthy(&cond) {
                    match self.eval_block_inner(then_block) {
                        Ok(v) => v,
                        Err(cf) => self.set_cf(cf),
                    }
                } else if let Some(ref else_expr) = else_branch {
                    self.eval_expr_inner(else_expr)
                } else {
                    Value::Unit
                }
            }

            // If-let
            ExprKind::IfLet {
                pattern,
                value,
                then_block,
                else_branch,
            } => {
                let val = self.eval_expr_inner(value);
                if self.try_match_pattern(pattern, &val) {
                    self.env.push_scope();
                    self.bind_pattern(pattern, val);
                    let result = self.eval_block_inner(then_block);
                    self.env.pop_scope();
                    match result {
                        Ok(v) => v,
                        Err(cf) => self.set_cf(cf),
                    }
                } else if let Some(ref else_expr) = else_branch {
                    self.eval_expr_inner(else_expr)
                } else {
                    Value::Unit
                }
            }

            // Match
            ExprKind::Match { scrutinee, arms } => {
                let val = self.eval_expr_inner(scrutinee);
                self.eval_match(&val, arms, &expr.span)
            }

            // While loop
            ExprKind::While {
                condition,
                body,
                label,
                ..
            } => {
                loop {
                    let cond = self.eval_expr_inner(condition);
                    if self.check_cf() || !self.is_truthy(&cond) {
                        break;
                    }
                    match self.eval_block_inner(body) {
                        Ok(_) => {}
                        Err(ControlFlow::Break {
                            label: ref bl,
                            value: ref v,
                        }) => {
                            if bl.is_none() || bl.as_deref() == label.as_deref() {
                                return v.clone().unwrap_or(Value::Unit);
                            } else {
                                return self.set_cf(ControlFlow::Break {
                                    label: bl.clone(),
                                    value: v.clone(),
                                });
                            }
                        }
                        Err(ControlFlow::Continue { label: ref cl }) => {
                            if cl.is_none() || cl.as_deref() == label.as_deref() {
                                continue;
                            } else {
                                return self.set_cf(ControlFlow::Continue { label: cl.clone() });
                            }
                        }
                        Err(cf) => return self.set_cf(cf),
                    }
                }
                Value::Unit
            }

            // While-let loop — re-evaluate the scrutinee each iteration and
            // test it against the pattern (the loop condition). On a match,
            // bind the pattern's names in a per-iteration scope and run the
            // body; on a non-match, exit. Mirrors the `For` loop's scope +
            // break/continue handling (pop_scope on every exit path).
            ExprKind::WhileLet {
                pattern,
                value,
                body,
                label,
                ..
            } => {
                loop {
                    let val = self.eval_expr_inner(value);
                    // A control-flow signal raised while evaluating the
                    // scrutinee (e.g. a `?` early-return) propagates — stop.
                    if self.check_cf() {
                        break;
                    }
                    if !self.try_match_pattern(pattern, &val) {
                        break;
                    }
                    self.env.push_scope();
                    self.bind_pattern(pattern, val);
                    match self.eval_block_inner(body) {
                        Ok(_) => {}
                        Err(ControlFlow::Break {
                            label: ref bl,
                            value: ref v,
                        }) => {
                            self.env.pop_scope();
                            if bl.is_none() || bl.as_deref() == label.as_deref() {
                                return v.clone().unwrap_or(Value::Unit);
                            } else {
                                return self.set_cf(ControlFlow::Break {
                                    label: bl.clone(),
                                    value: v.clone(),
                                });
                            }
                        }
                        Err(ControlFlow::Continue { label: ref cl }) => {
                            self.env.pop_scope();
                            if cl.is_none() || cl.as_deref() == label.as_deref() {
                                continue;
                            } else {
                                return self.set_cf(ControlFlow::Continue { label: cl.clone() });
                            }
                        }
                        Err(cf) => {
                            self.env.pop_scope();
                            return self.set_cf(cf);
                        }
                    }
                    self.env.pop_scope();
                }
                Value::Unit
            }

            // For loop
            ExprKind::For {
                pattern,
                iterable,
                body,
                label,
                ..
            } => {
                let iter_val = self.eval_expr_inner(iterable);
                let items = match iter_val {
                    Value::Array(rc) => match Arc::try_unwrap(rc) {
                        Ok(cell) => cell.into_inner().unwrap(),
                        Err(rc) => rc.read().unwrap().clone(),
                    },
                    Value::Slice {
                        storage,
                        start,
                        len,
                        ..
                    } => storage.read().unwrap()[start..start + len].to_vec(),
                    Value::Tuple(v) => v,
                    // SortedSet iterates in ascending key order
                    Value::SortedSet(s) => s.into_keys().map(|k| k.0).collect(),
                    // Set iterates in insertion order
                    Value::Set(s) => s,
                    // Map iterates as (key, value) tuples in insertion order
                    Value::Map(m) => m
                        .into_iter()
                        .map(|(k, v)| Value::Tuple(vec![k, v]))
                        .collect(),
                    // String iterates per Unicode scalar value, matching the
                    // canonical `s.chars()` surface — design.md § Character
                    // type (line 2299) pins `for c in s` and `s.chars()` as
                    // semantic peers.
                    Value::String(s) => s.chars().map(Value::Char).collect(),
                    // Iterator: drain via repeated `iterator_step` so adaptor
                    // closures (Map / Filter / future) fire per element. The
                    // for-loop walks the resulting Vec uniformly with the
                    // raw-collection arms above.
                    iter @ Value::Iterator { .. } => {
                        let mut it = iter;
                        let mut drained = Vec::new();
                        while let Some(v) = self.iterator_step(&mut it) {
                            drained.push(v);
                        }
                        drained
                    }
                    // `LinesIter[R]` (from `BufReader.lines()`) — drain the
                    // shared reader one line at a time, yielding
                    // `Result[String, IoError]` per line: `Ok(line)` with the
                    // trailing `\n` / `\r\n` stripped (matching Rust's
                    // `BufRead::lines`), `Err` once on a mid-stream read error
                    // (then terminate), EOF terminates. Eager-materialized like
                    // the other iterables above; the shared BufReader is left
                    // at EOF afterward.
                    Value::LinesIter(rc) => {
                        use std::io::BufRead;
                        let mut guard = rc.lock().unwrap();
                        let mut drained = Vec::new();
                        loop {
                            let mut line = String::new();
                            match guard.read_line(&mut line) {
                                Ok(0) => break,
                                Ok(_) => {
                                    if line.ends_with('\n') {
                                        line.pop();
                                        if line.ends_with('\r') {
                                            line.pop();
                                        }
                                    }
                                    drained.push(super::helpers::io_ok(Value::String(line)));
                                }
                                Err(e) => {
                                    drained.push(super::helpers::io_err_value(
                                        super::helpers::io_error_from_std(&e),
                                    ));
                                    break;
                                }
                            }
                        }
                        drained
                    }
                    _ => vec![iter_val],
                };
                for item in items {
                    self.env.push_scope();
                    self.bind_pattern(pattern, item);
                    match self.eval_block_inner(body) {
                        Ok(_) => {}
                        Err(ControlFlow::Break {
                            label: ref bl,
                            value: ref v,
                        }) => {
                            self.env.pop_scope();
                            if bl.is_none() || bl.as_deref() == label.as_deref() {
                                return v.clone().unwrap_or(Value::Unit);
                            } else {
                                return self.set_cf(ControlFlow::Break {
                                    label: bl.clone(),
                                    value: v.clone(),
                                });
                            }
                        }
                        Err(ControlFlow::Continue { label: ref cl }) => {
                            self.env.pop_scope();
                            if cl.is_none() || cl.as_deref() == label.as_deref() {
                                continue;
                            } else {
                                return self.set_cf(ControlFlow::Continue { label: cl.clone() });
                            }
                        }
                        Err(cf) => {
                            self.env.pop_scope();
                            return self.set_cf(cf);
                        }
                    }
                    self.env.pop_scope();
                }
                Value::Unit
            }

            // Loop
            ExprKind::Loop { body, label, .. } => loop {
                match self.eval_block_inner(body) {
                    Ok(_) => {}
                    Err(ControlFlow::Break {
                        label: ref bl,
                        value: ref v,
                    }) => {
                        if bl.is_none() || bl.as_deref() == label.as_deref() {
                            return v.clone().unwrap_or(Value::Unit);
                        } else {
                            return self.set_cf(ControlFlow::Break {
                                label: bl.clone(),
                                value: v.clone(),
                            });
                        }
                    }
                    Err(ControlFlow::Continue { label: ref cl }) => {
                        if cl.is_none() || cl.as_deref() == label.as_deref() {
                            continue;
                        } else {
                            return self.set_cf(ControlFlow::Continue { label: cl.clone() });
                        }
                    }
                    Err(cf) => return self.set_cf(cf),
                }
            },

            // Return
            ExprKind::Return(val) => {
                let v = val
                    .as_ref()
                    .map(|e| self.eval_expr_inner(e))
                    .unwrap_or(Value::Unit);
                self.set_cf(ControlFlow::Return(v))
            }

            // Break
            ExprKind::Break { label, value } => {
                let v = value.as_ref().map(|e| self.eval_expr_inner(e));
                self.set_cf(ControlFlow::Break {
                    label: label.clone(),
                    value: v,
                })
            }

            // Continue
            ExprKind::Continue { label } => self.set_cf(ControlFlow::Continue {
                label: label.clone(),
            }),

            // Closure
            ExprKind::Closure {
                params,
                capture_mode,
                prefix_span: _,
                body,
            } => {
                // For `mut ref |...|` closures, promote each captured outer
                // binding's slot to a `Value::SharedCell` so mutations made
                // inside the body propagate back to the outer binding and
                // are visible to subsequent invocations of the closure.
                if matches!(capture_mode, Some(CaptureMode::MutRef)) {
                    let mut bound: HashSet<String> = HashSet::new();
                    for p in params {
                        add_pattern_bindings(&p.pattern, &mut bound);
                    }
                    let mut idents: Vec<String> = Vec::new();
                    collect_free_idents_expr(body, &mut bound, &mut idents);
                    for name in idents {
                        // Skip globals (functions, enum variants, type ctors,
                        // etc.) — they live in scope[0] and never need to
                        // alias back through a cell.
                        if self
                            .env
                            .scopes
                            .first()
                            .is_some_and(|s| s.contains_key(&name))
                        {
                            continue;
                        }
                        let _ = self.env.wrap_capture(&name);
                    }
                }
                let captured = self.env.snapshot();
                let closure_body = Block {
                    stmts: Vec::new(),
                    final_expr: Some(Box::new(body.as_ref().clone())),
                    span: body.span.clone(),
                };
                Value::Function {
                    name: "<closure>".to_string(),
                    param_patterns: params.iter().map(|p| p.pattern.clone()).collect(),
                    param_defaults: params.iter().map(|_| None).collect(),
                    body: closure_body,
                    closure_env: Some(captured),
                }
            }

            // Cast — runtime conversion driven by the surface target type.
            // Numeric ↔ numeric, bool → int, and char → wide-int are the
            // shapes the typechecker accepts (see `check_cast_pair`); the
            // interpreter mirrors them here so `c as i32` produces the
            // codepoint as an integer rather than leaving a `Value::Char`
            // that downstream arithmetic would mis-type.
            ExprKind::Cast { expr: inner, ty } => {
                let val = self.eval_expr_inner(inner);
                let target_name = match &ty.kind {
                    crate::ast::TypeKind::Path(p) => p.segments.last().cloned().unwrap_or_default(),
                    _ => String::new(),
                };
                // `x as Refined` enforces the refinement predicate at runtime
                // (phase-9 step 5b): the value is cast to the refinement's
                // base representation, then the predicate is checked with
                // `self` bound to it. A false predicate is a contract
                // violation — abort with a `contract`-style fault. On success
                // the (layout-identical) base value flows through unchanged.
                if let Some(pred) = self.refinement_predicate(&target_name) {
                    let base = self
                        .refinement_base_name(&target_name)
                        .unwrap_or_else(|| target_name.clone());
                    let casted = cast_value(val, &base);
                    match self.eval_refinement_predicate(&pred, casted.clone()) {
                        Some(true) => return casted,
                        _ => {
                            return self.record_runtime_error(
                                format!(
                                    "contract violated: value does not satisfy refinement `{target_name}`"
                                ),
                                &expr.span,
                            )
                        }
                    }
                }
                cast_value(val, &target_name)
            }

            // Range — evaluates to a `Value::Iterator` for bounded ranges
            // (so `(0..10).step_by(2)` and the rest of the adaptor surface
            // dispatch through the same path as `xs.iter()`), or a runtime
            // error for unbounded forms used as values. The for-loop iterable
            // path drains `Value::Iterator` via `iterator_step` (see the
            // `ExprKind::For` arm above), so `for x in 0..n { ... }` keeps
            // working unchanged.
            ExprKind::Range {
                start,
                end,
                inclusive,
            } => {
                let s = start.as_deref().map(|e| self.eval_expr_inner(e));
                let e = end.as_deref().map(|e| self.eval_expr_inner(e));
                let s_variant = s.as_ref().map(|v| v.variant_name()).unwrap_or("None");
                let e_variant = e.as_ref().map(|v| v.variant_name()).unwrap_or("None");
                match (s, e) {
                    (Some(Value::Int(a)), Some(Value::Int(b))) => {
                        let items: Vec<Value> = if *inclusive {
                            (a..=b).map(Value::Int).collect()
                        } else {
                            (a..b).map(Value::Int).collect()
                        };
                        Value::Iterator {
                            source: IteratorSource::Eager { items, cursor: 0 },
                            steps: Vec::new(),
                        }
                    }
                    (None, None) => {
                        // RangeFull used as a value — only valid as a slice index
                        self.record_runtime_error(
                            "RangeFull (..) cannot be used as a standalone value".to_string(),
                            &expr.span,
                        )
                    }
                    (Some(_), None) | (None, Some(_)) => {
                        // RangeFrom / RangeTo used as a value outside of index context
                        self.record_runtime_error(
                            "unbounded ranges cannot be used as standalone values".to_string(),
                            &expr.span,
                        )
                    }
                    _ => unreachable!(
                        "range bounds at {}:{}: start=Value::{}, end=Value::{}; \
                         either an interpreter codepath produced wrong variants \
                         or the typechecker accepted non-integer range bounds",
                        expr.span.line, expr.span.column, s_variant, e_variant
                    ),
                }
            }

            // Pipe
            ExprKind::Pipe { left, right } => self.eval_pipe(left, right),

            // Question mark (? operator)
            // On Err(e) → return Err(e) from enclosing function
            // On Ok(v) → unwrap to v
            // On None → return None from enclosing function
            ExprKind::Question(inner) => {
                let val = self.eval_expr_inner(inner);
                match &val {
                    Value::EnumVariant {
                        variant,
                        data: EnumData::Tuple(vals),
                        ..
                    } if variant == "Ok" => {
                        self.clear_error_trace();
                        vals.first().cloned().unwrap_or(Value::Unit)
                    }
                    Value::EnumVariant { variant, .. } if variant == "Err" => {
                        // Record trace frame before propagating
                        self.push_error_trace(expr.span.line, expr.span.column);
                        // Cross-error conversion: typechecker recorded a target
                        // type at this `?` span if `From` conversion is needed.
                        let span_key = SpanKey::from_span(&expr.span);
                        let propagated = if let Some(target) = self
                            .typecheck_result
                            .question_conversions
                            .get(&span_key)
                            .cloned()
                        {
                            let inner_err = match &val {
                                Value::EnumVariant {
                                    data: EnumData::Tuple(vs),
                                    ..
                                } => vs.first().cloned().unwrap_or(Value::Unit),
                                _ => Value::Unit,
                            };
                            let converted =
                                self.call_function(&format!("{}.from", target), &[inner_err]);
                            Value::EnumVariant {
                                enum_name: "Result".to_string(),
                                variant: "Err".to_string(),
                                data: EnumData::Tuple(vec![converted]),
                            }
                        } else {
                            val
                        };
                        // Propagate Err by returning from enclosing function
                        self.set_cf(ControlFlow::Return(propagated))
                    }
                    Value::EnumVariant {
                        variant,
                        data: EnumData::Tuple(vals),
                        ..
                    } if variant == "Some" => {
                        self.clear_error_trace();
                        vals.first().cloned().unwrap_or(Value::Unit)
                    }
                    Value::EnumVariant {
                        variant,
                        data: EnumData::Unit,
                        ..
                    } if variant == "None" => {
                        // Record trace frame before propagating
                        self.push_error_trace(expr.span.line, expr.span.column);
                        self.set_cf(ControlFlow::Return(val))
                    }
                    // Not a Result/Option — pass through
                    _ => val,
                }
            }

            // Optional chaining (?.)
            ExprKind::OptionalChain {
                object,
                field_or_method: field,
                args: _,
            } => {
                let obj = self.eval_expr_inner(object);
                match &obj {
                    Value::EnumVariant {
                        variant,
                        data: EnumData::Unit,
                        ..
                    } if variant == "None" => {
                        obj // propagate None
                    }
                    Value::EnumVariant {
                        variant,
                        data: EnumData::Tuple(vals),
                        ..
                    } if variant == "Some" => {
                        let inner = vals.first().cloned().unwrap_or(Value::Unit);
                        match inner {
                            Value::Struct { fields, .. } => {
                                let val = fields.get(field).cloned().unwrap_or(Value::Unit);
                                Value::EnumVariant {
                                    enum_name: "Option".to_string(),
                                    variant: "Some".to_string(),
                                    data: EnumData::Tuple(vec![val]),
                                }
                            }
                            _ => Value::EnumVariant {
                                enum_name: "Option".to_string(),
                                variant: "None".to_string(),
                                data: EnumData::Unit,
                            },
                        }
                    }
                    _ => {
                        // Not an Option, just do field access
                        match obj {
                            Value::Struct { fields, .. } => {
                                fields.get(field).cloned().unwrap_or(Value::Unit)
                            }
                            _ => Value::Unit,
                        }
                    }
                }
            }

            // NilCoalesce (??)
            ExprKind::NilCoalesce { left, right } => {
                let l = self.eval_expr_inner(left);
                match &l {
                    Value::EnumVariant {
                        variant,
                        data: EnumData::Unit,
                        ..
                    } if variant == "None" => self.eval_expr_inner(right),
                    _ => l,
                }
            }

            ExprKind::Unsafe(block) => match self.eval_block_inner(block) {
                Ok(v) => v,
                Err(cf) => self.set_cf(cf),
            },

            ExprKind::Try(block) => {
                // v1 stub — typechecker rejects every `try { ... }` use
                // with E_TRY_BLOCK_NOT_IMPLEMENTED_YET; the interpreter
                // never sees a valid try block in a typechecker-clean
                // program. We still walk the body for any debug-mode use
                // that bypasses the typechecker so the form has a defined
                // shape until P1 ships ?-retargeting.
                match self.eval_block_inner(block) {
                    Ok(v) => v,
                    Err(cf) => self.set_cf(cf),
                }
            }

            ExprKind::Seq(block) => match self.eval_block_inner(block) {
                Ok(v) => v,
                Err(cf) => self.set_cf(cf),
            },

            ExprKind::Par(block) => {
                if self.sequential_mode {
                    // In sequential mode, par {} is just a regular block
                    match self.eval_block_inner(block) {
                        Ok(v) => v,
                        Err(cf) => self.set_cf(cf),
                    }
                } else {
                    match self.eval_par_block(block) {
                        Ok(v) => v,
                        Err(cf) => self.set_cf(cf),
                    }
                }
            }

            ExprKind::Providers { bindings, body } => self.eval_providers_block(bindings, body),

            // LBC4 — `label: { body }`. Routes the existing
            // `ControlFlow::Break { label, value }` signal: a `break label
            // expr` inside the body matches by label, returns the value
            // (or `Value::Unit` when bare `break label`); any non-matching
            // control-flow signal (outer-label break, return, cancel,
            // exit, runtime error) propagates unchanged. No new
            // `ControlFlow` variants needed.
            ExprKind::LabeledBlock { label, body, .. } => match self.eval_block_inner(body) {
                Ok(v) => v,
                Err(ControlFlow::Break {
                    label: Some(ref l),
                    ref value,
                }) if l == label => value.clone().unwrap_or(Value::Unit),
                Err(cf) => self.set_cf(cf),
            },

            // `lock m [alias] { body }` — the tree-walk interpreter is
            // single-threaded, so the lock is a no-op for synchronization. Bind
            // the mutex's inner value as the alias (or the mutex name itself,
            // shadowed), evaluate the body, then write the (possibly mutated)
            // value back into the mutex cell. The body is straight-line (the
            // typechecker rejects early exits), so no control-flow unwinding of
            // the write-back is needed.
            ExprKind::Lock { mutex, alias, body } => {
                // Resolve the place (`m` or `self.state`) to its shared
                // `Arc<Mutex<…>>` cell. The `Arc` clone shares the *same* cell
                // a par-struct field holds across `par {}` branch env clones,
                // so the lock below is genuinely mutually exclusive between
                // branches (the prior `Box<Value>` copy raced).
                let cell = match self.eval_expr_inner(mutex) {
                    Value::Mutex(c) => c,
                    // Defensive — typechecker guarantees a Mutex place; wrap a
                    // stray value in a throwaway cell so the body still runs.
                    other => std::sync::Arc::new(std::sync::Mutex::new(other)),
                };
                // The inner-value binding name: the alias, or (for an
                // `Identifier` place) the mutex name itself. A field place
                // without an alias is rejected by the typechecker.
                let bind_name = alias.clone().or_else(|| match &mutex.kind {
                    ExprKind::Identifier(n) => Some(n.clone()),
                    _ => None,
                });
                // Hold the real lock for the whole body — this is what
                // serialises concurrent `par {}` branches. `guard` borrows the
                // local `cell` (an `Arc` clone), not `self`, so it can live
                // across the `self.eval_block_inner(body)` call. Re-locking the
                // same mutex inside the body deadlocks, matching codegen's real
                // spinlock (std `Mutex` is not re-entrant).
                let mut guard = cell.lock().unwrap();
                let inner = guard.clone();
                self.env.push_scope();
                if let Some(ref name) = bind_name {
                    self.env.define(name.clone(), inner);
                }
                let cf_result = self.eval_block_inner(body);
                // Read the (possibly mutated) bound value back before popping
                // the scope, then write it through the guard — the shared cell
                // is the single source of truth, so there is no separate
                // write-back to the env slot / field (the place already holds
                // the same `Arc`). Persist on early exit too, so mutations made
                // before a `return` / `break` are not lost.
                let new_inner = bind_name
                    .as_ref()
                    .and_then(|n| self.env.get(n))
                    .unwrap_or(Value::Unit);
                self.env.pop_scope();
                *guard = new_inner;
                drop(guard);
                match cf_result {
                    Ok(v) => v,
                    Err(cf) => self.set_cf(cf),
                }
            }

            ExprKind::SelfType | ExprKind::PipePlaceholder | ExprKind::Error => Value::Unit,

            _ => todo!(
                "Interpreter: unhandled expr {:?}",
                std::mem::discriminant(&expr.kind)
            ),
        }
    }
}

/// Apply the surface-level `as`-cast at runtime. Mirrors the cast pairs the
/// typechecker accepts in `check_cast_pair`: numeric↔numeric (int / float),
/// bool→int, and char→wide-int (i32/i64/u32/u64). Narrow integer targets
/// are masked (e.g. `1000i64 as i8` keeps the low 8 bits, matching the
/// LLVM `trunc` codegen). Unknown / unsupported target names pass the
/// value through — the typechecker has already rejected genuine
/// mis-casts; this guard just prevents an interpreter panic when the
/// AST shape carries something the runtime doesn't recognize.
pub(super) fn cast_value(val: Value, target: &str) -> Value {
    let int_from = |i: i64| -> Value {
        match target {
            "i8" => Value::Int(i as i8 as i64),
            "i16" => Value::Int(i as i16 as i64),
            "i32" => Value::Int(i as i32 as i64),
            "i64" | "isize" | "int" => Value::Int(i),
            "u8" => Value::Int((i as u8) as i64),
            "u16" => Value::Int((i as u16) as i64),
            "u32" => Value::Int((i as u32) as i64),
            "u64" | "usize" | "uint" => Value::Int(i),
            "f32" => Value::Float(i as f32 as f64),
            "f64" | "float" => Value::Float(i as f64),
            "bool" => Value::Bool(i != 0),
            _ => Value::Int(i),
        }
    };
    let float_from = |f: f64| -> Value {
        match target {
            "f32" => Value::Float(f as f32 as f64),
            "f64" | "float" => Value::Float(f),
            "i8" => Value::Int(f as i8 as i64),
            "i16" => Value::Int(f as i16 as i64),
            "i32" => Value::Int(f as i32 as i64),
            "i64" | "isize" | "int" => Value::Int(f as i64),
            "u8" => Value::Int((f as u8) as i64),
            "u16" => Value::Int((f as u16) as i64),
            "u32" => Value::Int((f as u32) as i64),
            "u64" | "usize" | "uint" => Value::Int(f as u64 as i64),
            _ => Value::Float(f),
        }
    };
    match val {
        Value::Int(i) => int_from(i),
        Value::Float(f) => float_from(f),
        Value::Bool(b) => int_from(b as i64),
        Value::Char(c) => int_from(c as u32 as i64),
        other => other,
    }
}
