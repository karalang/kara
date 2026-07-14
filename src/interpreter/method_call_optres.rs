//! Option / Result / Atomic method dispatch — the bodies of the
//! `unwrap`/`expect`/`is_some`/`is_none`/`is_ok`/`is_err`/`load`/
//! `store` arms lifted out of `eval_method_call`.

use crate::ast::*;
use crate::token::Span;

use super::value::{EnumData, Value};

impl<'a> super::Interpreter<'a> {
    pub(super) fn try_eval_option_result_method(
        &mut self,
        method: &str,
        // The receiver place-expr. Not needed for atomic mutation
        // (`Value::Atomic` is an `Arc<Mutex<…>>` shared cell mutated in place
        // under lock), but `Option.take` writes `None` back through it.
        object: &Expr,
        obj: &Value,
        args: &[CallArg],
        span: &Span,
    ) -> Option<Value> {
        match method {
            "unwrap" => {
                return Some(match obj {
                    Value::EnumVariant {
                        variant,
                        data: EnumData::Tuple(vals),
                        ..
                    } if variant == "Ok" || variant == "Some" => {
                        vals.first().cloned().unwrap_or(Value::Unit)
                    }
                    Value::EnumVariant { variant, .. } if variant == "Err" || variant == "None" => {
                        return Some(self.record_runtime_error(
                            format!("called unwrap() on {}", variant),
                            span,
                        ));
                    }
                    other => other.clone(),
                });
            }
            "expect" => {
                let msg = if let Some(arg) = args.first() {
                    match self.eval_expr_inner(&arg.value) {
                        Value::String(s) => s,
                        v => format!("{}", v),
                    }
                } else {
                    String::new()
                };
                return Some(match obj {
                    Value::EnumVariant {
                        variant,
                        data: EnumData::Tuple(vals),
                        ..
                    } if variant == "Ok" || variant == "Some" => {
                        vals.first().cloned().unwrap_or(Value::Unit)
                    }
                    Value::EnumVariant { variant, .. } if variant == "Err" || variant == "None" => {
                        return Some(self.record_runtime_error(
                            if msg.is_empty() {
                                format!("expect() called on {}", variant)
                            } else {
                                format!("{}: {}", msg, variant)
                            },
                            span,
                        ));
                    }
                    other => other.clone(),
                });
            }
            "unwrap_err" => {
                return Some(match obj {
                    Value::EnumVariant {
                        variant,
                        data: EnumData::Tuple(vals),
                        ..
                    } if variant == "Err" => vals.first().cloned().unwrap_or(Value::Unit),
                    Value::EnumVariant { variant, .. } if variant == "Ok" => {
                        return Some(self.record_runtime_error(
                            format!("called unwrap_err() on {}", variant),
                            span,
                        ));
                    }
                    other => other.clone(),
                });
            }
            "expect_err" => {
                let msg = if let Some(arg) = args.first() {
                    match self.eval_expr_inner(&arg.value) {
                        Value::String(s) => s,
                        v => format!("{}", v),
                    }
                } else {
                    String::new()
                };
                return Some(match obj {
                    Value::EnumVariant {
                        variant,
                        data: EnumData::Tuple(vals),
                        ..
                    } if variant == "Err" => vals.first().cloned().unwrap_or(Value::Unit),
                    Value::EnumVariant { variant, .. } if variant == "Ok" => {
                        return Some(self.record_runtime_error(
                            if msg.is_empty() {
                                format!("expect_err() called on {}", variant)
                            } else {
                                format!("{}: {}", msg, variant)
                            },
                            span,
                        ));
                    }
                    other => other.clone(),
                });
            }
            "unwrap_or" => {
                // `unwrap_or(default)` — eager fallback (the arg is always
                // evaluated, matching Rust semantics, unlike `unwrap_or_else`).
                // Present (`Some`/`Ok`) yields the payload; absent (`None`/
                // `Err`) yields the default.
                let default = args
                    .first()
                    .map(|a| self.eval_expr_inner(&a.value))
                    .unwrap_or(Value::Unit);
                return Some(match obj {
                    Value::EnumVariant {
                        variant,
                        data: EnumData::Tuple(vals),
                        ..
                    } if variant == "Ok" || variant == "Some" => {
                        vals.first().cloned().unwrap_or(default)
                    }
                    _ => default,
                });
            }
            "map" => {
                // `Option[T].map(f)` / `Result[T, E].map(f)`: apply `f` to the
                // present payload (`Some`/`Ok`) and re-wrap in the SAME variant;
                // an absent receiver (`None`/`Err`) passes through unchanged.
                // `f` is a fn-reference or closure argument that evaluates to a
                // `Value::Function`; `invoke_function_value` runs it over the
                // unwrapped payload. design.md documents `.map` on Result as
                // intended (`self.fetch_profile(user.id).map(Response.ok)`);
                // the typechecker already types this call (B-2026-07-12-11).
                let f = self.eval_expr_inner(&args[0].value);
                return Some(match obj {
                    Value::EnumVariant {
                        enum_name,
                        variant,
                        data: EnumData::Tuple(vals),
                    } if variant == "Some" || variant == "Ok" => {
                        let payload = vals.first().cloned().unwrap_or(Value::Unit);
                        let mapped = self.invoke_function_value(f, vec![payload]);
                        Value::EnumVariant {
                            enum_name: enum_name.clone(),
                            variant: variant.clone(),
                            data: EnumData::Tuple(vec![mapped]),
                        }
                    }
                    // `None` / `Err(e)` — unchanged (the mapper never runs).
                    other => other.clone(),
                });
            }
            // ── Option / Result combinators, non-closure batch (B-2026-07-14-6) ──
            // Each arm is GATED on the receiver being the matching Option/Result
            // variant shape and otherwise falls through (reaches the trailing
            // `None`), so a user type with an identically-named method still
            // dispatches through the normal path — this handler runs for every
            // receiver, not just Option/Result.
            "ok" | "err" => {
                // `Result[T,E].ok() -> Option[T]`; `.err() -> Option[E]`.
                if let Value::EnumVariant {
                    variant,
                    data: EnumData::Tuple(vals),
                    ..
                } = obj
                {
                    if variant == "Ok" || variant == "Err" {
                        let want = if method == "ok" { "Ok" } else { "Err" };
                        return Some(if variant == want {
                            Value::EnumVariant {
                                enum_name: "Option".to_string(),
                                variant: "Some".to_string(),
                                data: EnumData::Tuple(vec![vals
                                    .first()
                                    .cloned()
                                    .unwrap_or(Value::Unit)]),
                            }
                        } else {
                            Value::EnumVariant {
                                enum_name: "Option".to_string(),
                                variant: "None".to_string(),
                                data: EnumData::Unit,
                            }
                        });
                    }
                }
            }
            "or" | "and" => {
                // `Option[T].or(alt)` / `Result[T,E].or(alt)` — the receiver when
                // present (`Some`/`Ok`), else the eagerly-evaluated alternative.
                // `and` is the dual: the eagerly-evaluated `other` when the
                // receiver is present, else the (absent) receiver passes through.
                if let Value::EnumVariant { variant, .. } = obj {
                    if matches!(variant.as_str(), "Some" | "None" | "Ok" | "Err") {
                        let arg = args
                            .first()
                            .map(|a| self.eval_expr_inner(&a.value))
                            .unwrap_or(Value::Unit);
                        let present = variant == "Some" || variant == "Ok";
                        let take_arg = if method == "or" { !present } else { present };
                        return Some(if take_arg { arg } else { obj.clone() });
                    }
                }
            }
            "ok_or" => {
                // `Option[T].ok_or(e) -> Result[T, E]`: `Some(x)`→`Ok(x)`,
                // `None`→`Err(e)` (the error arg is eagerly evaluated).
                if let Value::EnumVariant { variant, data, .. } = obj {
                    if variant == "Some" {
                        let payload = match data {
                            EnumData::Tuple(vals) => vals.first().cloned().unwrap_or(Value::Unit),
                            _ => Value::Unit,
                        };
                        return Some(Value::EnumVariant {
                            enum_name: "Result".to_string(),
                            variant: "Ok".to_string(),
                            data: EnumData::Tuple(vec![payload]),
                        });
                    } else if variant == "None" {
                        let e = args
                            .first()
                            .map(|a| self.eval_expr_inner(&a.value))
                            .unwrap_or(Value::Unit);
                        return Some(Value::EnumVariant {
                            enum_name: "Result".to_string(),
                            variant: "Err".to_string(),
                            data: EnumData::Tuple(vec![e]),
                        });
                    }
                }
            }
            "take" => {
                // `Option[T].take() -> Option[T]` — MUTATING: yields the
                // receiver's current value and writes `None` back into the
                // receiver's PLACE (`assign_to_place` handles identifier /
                // field / index / deref roots). Gated on the receiver being an
                // Option variant so a user type's `take` still routes normally.
                if let Value::EnumVariant { variant, .. } = obj {
                    if variant == "Some" || variant == "None" {
                        let taken = obj.clone();
                        let none = Value::EnumVariant {
                            enum_name: "Option".to_string(),
                            variant: "None".to_string(),
                            data: EnumData::Unit,
                        };
                        self.assign_to_place(object, none);
                        return Some(taken);
                    }
                }
            }
            "get_or_insert" => {
                // `Option[T].get_or_insert(v) -> T` — MUTATING: `Some(x)` yields
                // `x`; `None` writes `Some(v)` back into the receiver's place
                // and yields `v`. Result modeled BY VALUE (see the typechecker
                // arm). Gated on the receiver being an Option variant.
                if let Value::EnumVariant { variant, data, .. } = obj {
                    if variant == "Some" {
                        if let EnumData::Tuple(vals) = data {
                            return Some(vals.first().cloned().unwrap_or(Value::Unit));
                        }
                    } else if variant == "None" {
                        let v = args
                            .first()
                            .map(|a| self.eval_expr_inner(&a.value))
                            .unwrap_or(Value::Unit);
                        let some = Value::EnumVariant {
                            enum_name: "Option".to_string(),
                            variant: "Some".to_string(),
                            data: EnumData::Tuple(vec![v.clone()]),
                        };
                        self.assign_to_place(object, some);
                        return Some(v);
                    }
                }
            }
            "flatten" => {
                // `Option[Option[T]].flatten() -> Option[T]`: `Some(inner)`→
                // `inner`, `None`→`None`.
                if let Value::EnumVariant { variant, data, .. } = obj {
                    if variant == "Some" {
                        if let EnumData::Tuple(vals) = data {
                            return Some(vals.first().cloned().unwrap_or(Value::EnumVariant {
                                enum_name: "Option".to_string(),
                                variant: "None".to_string(),
                                data: EnumData::Unit,
                            }));
                        }
                    } else if variant == "None" {
                        return Some(obj.clone());
                    }
                }
            }
            // ── Option / Result combinators, CLOSURE batch (B-2026-07-14-6) ──
            // Each is gated on the receiver being the matching Option/Result
            // variant, else falls through (preserving user methods).
            "unwrap_or_else" => {
                // present → payload; absent → `f()` (Option) / `f(e)` (Result).
                if let Value::EnumVariant { variant, data, .. } = obj {
                    if matches!(variant.as_str(), "Some" | "Ok" | "None" | "Err") {
                        let present = variant == "Some" || variant == "Ok";
                        let payload = match data {
                            EnumData::Tuple(vals) => vals.first().cloned(),
                            _ => None,
                        };
                        if present {
                            return Some(payload.unwrap_or(Value::Unit));
                        }
                        let f = self.eval_expr_inner(&args[0].value);
                        let call_args = if variant == "Err" {
                            vec![payload.unwrap_or(Value::Unit)]
                        } else {
                            vec![]
                        };
                        return Some(self.invoke_function_value(f, call_args));
                    }
                }
            }
            "map_or" => {
                // present → `f(payload)`; absent → eager `default` (arg 0).
                if let Value::EnumVariant { variant, data, .. } = obj {
                    if matches!(variant.as_str(), "Some" | "Ok" | "None" | "Err") {
                        let present = variant == "Some" || variant == "Ok";
                        if present {
                            let payload = match data {
                                EnumData::Tuple(vals) => {
                                    vals.first().cloned().unwrap_or(Value::Unit)
                                }
                                _ => Value::Unit,
                            };
                            let f = self.eval_expr_inner(&args[1].value);
                            return Some(self.invoke_function_value(f, vec![payload]));
                        }
                        return Some(self.eval_expr_inner(&args[0].value));
                    }
                }
            }
            "map_or_else" => {
                // present → `f(payload)` (arg 1); absent → `default_fn()`
                // (Option) / `default_fn(e)` (Result) (arg 0).
                if let Value::EnumVariant { variant, data, .. } = obj {
                    if matches!(variant.as_str(), "Some" | "Ok" | "None" | "Err") {
                        let present = variant == "Some" || variant == "Ok";
                        let payload = match data {
                            EnumData::Tuple(vals) => vals.first().cloned().unwrap_or(Value::Unit),
                            _ => Value::Unit,
                        };
                        if present {
                            let f = self.eval_expr_inner(&args[1].value);
                            return Some(self.invoke_function_value(f, vec![payload]));
                        }
                        let d = self.eval_expr_inner(&args[0].value);
                        let call_args = if variant == "Err" {
                            vec![payload]
                        } else {
                            vec![]
                        };
                        return Some(self.invoke_function_value(d, call_args));
                    }
                }
            }
            "map_err" => {
                // `Ok` passes through; `Err(e)` → `Err(f(e))`.
                if let Value::EnumVariant {
                    enum_name,
                    variant,
                    data: EnumData::Tuple(vals),
                } = obj
                {
                    if variant == "Ok" {
                        return Some(obj.clone());
                    } else if variant == "Err" {
                        let e = vals.first().cloned().unwrap_or(Value::Unit);
                        let f = self.eval_expr_inner(&args[0].value);
                        let mapped = self.invoke_function_value(f, vec![e]);
                        return Some(Value::EnumVariant {
                            enum_name: enum_name.clone(),
                            variant: "Err".to_string(),
                            data: EnumData::Tuple(vec![mapped]),
                        });
                    }
                }
            }
            "and_then" => {
                // present → `f(payload)` (itself an Option/Result); absent → self.
                if let Value::EnumVariant { variant, data, .. } = obj {
                    if matches!(variant.as_str(), "Some" | "Ok" | "None" | "Err") {
                        let present = variant == "Some" || variant == "Ok";
                        if !present {
                            return Some(obj.clone());
                        }
                        let payload = match data {
                            EnumData::Tuple(vals) => vals.first().cloned().unwrap_or(Value::Unit),
                            _ => Value::Unit,
                        };
                        let f = self.eval_expr_inner(&args[0].value);
                        return Some(self.invoke_function_value(f, vec![payload]));
                    }
                }
            }
            "or_else" => {
                // present → self; absent → `f()` (Option) / `f(e)` (Result),
                // itself an Option/Result.
                if let Value::EnumVariant { variant, data, .. } = obj {
                    if matches!(variant.as_str(), "Some" | "Ok" | "None" | "Err") {
                        let present = variant == "Some" || variant == "Ok";
                        if present {
                            return Some(obj.clone());
                        }
                        let f = self.eval_expr_inner(&args[0].value);
                        let call_args = if variant == "Err" {
                            let e = match data {
                                EnumData::Tuple(vals) => {
                                    vals.first().cloned().unwrap_or(Value::Unit)
                                }
                                _ => Value::Unit,
                            };
                            vec![e]
                        } else {
                            vec![]
                        };
                        return Some(self.invoke_function_value(f, call_args));
                    }
                }
            }
            "filter" => {
                // `Some(x)` kept iff `pred(x)`, else `None`; `None` → `None`.
                if let Value::EnumVariant { variant, data, .. } = obj {
                    if variant == "Some" {
                        let payload = match data {
                            EnumData::Tuple(vals) => vals.first().cloned().unwrap_or(Value::Unit),
                            _ => Value::Unit,
                        };
                        let pred = self.eval_expr_inner(&args[0].value);
                        let keep = matches!(
                            self.invoke_function_value(pred, vec![payload.clone()]),
                            Value::Bool(true)
                        );
                        return Some(if keep {
                            Value::EnumVariant {
                                enum_name: "Option".to_string(),
                                variant: "Some".to_string(),
                                data: EnumData::Tuple(vec![payload]),
                            }
                        } else {
                            Value::EnumVariant {
                                enum_name: "Option".to_string(),
                                variant: "None".to_string(),
                                data: EnumData::Unit,
                            }
                        });
                    } else if variant == "None" {
                        return Some(obj.clone());
                    }
                }
            }
            "is_some" => {
                return Some(match obj {
                    Value::EnumVariant { variant, .. } if variant == "Some" => Value::Bool(true),
                    Value::EnumVariant { variant, .. } if variant == "None" => Value::Bool(false),
                    _ => Value::Bool(true),
                });
            }
            "is_none" => {
                return Some(match obj {
                    Value::EnumVariant { variant, .. } if variant == "None" => Value::Bool(true),
                    _ => Value::Bool(false),
                });
            }
            "is_ok" => {
                return Some(match obj {
                    Value::EnumVariant { variant, .. } if variant == "Ok" => Value::Bool(true),
                    _ => Value::Bool(false),
                });
            }
            "is_err" => {
                return Some(match obj {
                    Value::EnumVariant { variant, .. } if variant == "Err" => Value::Bool(true),
                    _ => Value::Bool(false),
                });
            }
            "load" => {
                if let Value::Atomic(cell) = obj {
                    // Backstop for the run/build divergence (B-2026-06-30-5):
                    // codegen rejects the implicit-ordering form, so the
                    // interpreter must too, or `karac run` would silently
                    // accept a program `karac build` refuses. The typechecker
                    // (run-fatal `AtomicMissingOrdering`) catches most call
                    // shapes earlier, but a field access through a `ref`/`mut
                    // ref` struct param types as `Type::Error` (fields.rs), so
                    // those slip past typecheck and land here — this guard is
                    // what keeps them consistent.
                    if args.len() != 1 {
                        return Some(
                            self.record_runtime_error(
                                "Atomic.load requires an explicit MemoryOrdering argument \
                             (there is no implicit-ordering form)"
                                    .to_string(),
                                span,
                            ),
                        );
                    }
                    // Ordering argument accepted but ignored — the `Mutex`
                    // already serialises every op, which is stronger than any
                    // requested ordering.
                    return Some(cell.lock().unwrap().clone());
                }
            }
            "store" => {
                if let Value::Atomic(cell) = obj {
                    // Arity backstop — see the `load` arm above (B-2026-06-30-5).
                    if args.len() != 2 {
                        return Some(
                            self.record_runtime_error(
                                "Atomic.store requires (value, MemoryOrdering) — the \
                             MemoryOrdering argument is not optional"
                                    .to_string(),
                                span,
                            ),
                        );
                    }
                    // Evaluate the argument *before* taking the lock: the
                    // interpreter could otherwise re-enter and touch the same
                    // atomic, and `std::sync::Mutex` is not re-entrant.
                    let val = if let Some(arg) = args.first() {
                        self.eval_expr_inner(&arg.value)
                    } else {
                        Value::Unit
                    };
                    *cell.lock().unwrap() = val;
                    return Some(Value::Unit);
                }
            }
            // Single-operand read-modify-write ops — return the PREVIOUS value
            // (matching the codegen / Rust semantics). The whole read-update-
            // write happens under the cell's `Mutex`, so concurrent `par {}`
            // branches sharing the same atomic serialise correctly (the prior
            // `Box<Value>` cell raced — torn reads surfaced as
            // `method '…' not found on type 'unknown'` panics and lost
            // updates). The mutation lands regardless of receiver shape
            // (identifier or `self.field`) because the `Arc` cell is shared,
            // not written back to a place — fixing the old field-receiver
            // limitation. Arithmetic/bitwise ops are integer-only; `swap`
            // exchanges any value (incl. `Atomic[bool]`). The ordering arg is
            // accepted and ignored.
            "fetch_add" | "fetch_sub" | "fetch_and" | "fetch_or" | "fetch_xor" | "swap" => {
                if let Value::Atomic(cell) = obj {
                    // Arity backstop — see the `load` arm above (B-2026-06-30-5).
                    // Gated on `Value::Atomic`, so the same-named Vec/Slice
                    // `swap(i, j)` (a non-atomic receiver) is untouched.
                    if args.len() != 2 {
                        return Some(self.record_runtime_error(
                            format!(
                                "Atomic.{method} requires (value, MemoryOrdering) — the \
                                 MemoryOrdering argument is not optional"
                            ),
                            span,
                        ));
                    }
                    // Eval the operand before locking (re-entrancy guard, as in
                    // `store`).
                    let arg_val = args
                        .first()
                        .map(|a| self.eval_expr_inner(&a.value))
                        .unwrap_or(Value::Unit);
                    let mut guard = cell.lock().unwrap();
                    let old = guard.clone();
                    let new = if method == "swap" {
                        Some(arg_val)
                    } else if let (Value::Int(o), Value::Int(d)) = (&old, &arg_val) {
                        Some(Value::Int(match method {
                            "fetch_add" => o + d,
                            "fetch_sub" => o - d,
                            "fetch_and" => o & d,
                            "fetch_or" => o | d,
                            "fetch_xor" => o ^ d,
                            _ => unreachable!("RMW arm gated on the method set above"),
                        }))
                    } else {
                        None
                    };
                    if let Some(new) = new {
                        *guard = new;
                    }
                    return Some(old);
                }
            }
            // `compare_exchange(old, new, success, failure) -> Result[T, T]` —
            // CAS. If the current value equals `old`, store `new` and return
            // `Ok(prev)`; otherwise leave it and return `Err(actual)`. Both
            // payloads are the loaded value. The compare-and-store runs under
            // the cell's `Mutex` so it is genuinely atomic across branches;
            // orderings ignored.
            "compare_exchange" => {
                if let Value::Atomic(cell) = obj {
                    // Arity backstop — see the `load` arm above (B-2026-06-30-5).
                    // CAS takes (old, new, success: MemoryOrdering, failure:
                    // MemoryOrdering); both orderings are required.
                    if args.len() != 4 {
                        return Some(
                            self.record_runtime_error(
                                "Atomic.compare_exchange requires (old, new, success: \
                             MemoryOrdering, failure: MemoryOrdering) — both ordering \
                             arguments are required"
                                    .to_string(),
                                span,
                            ),
                        );
                    }
                    // Eval both operands before locking (re-entrancy guard).
                    // `new` is evaluated unconditionally — these are value
                    // arguments per the CAS signature, so this matches Rust
                    // and avoids running user code under the lock.
                    let expected = args
                        .first()
                        .map(|a| self.eval_expr_inner(&a.value))
                        .unwrap_or(Value::Unit);
                    let new = args
                        .get(1)
                        .map(|a| self.eval_expr_inner(&a.value))
                        .unwrap_or(Value::Unit);
                    let mut guard = cell.lock().unwrap();
                    let current = guard.clone();
                    let swapped = current == expected;
                    if swapped {
                        *guard = new;
                    }
                    drop(guard);
                    return Some(Value::EnumVariant {
                        enum_name: "Result".to_string(),
                        variant: if swapped { "Ok" } else { "Err" }.to_string(),
                        data: EnumData::Tuple(vec![current]),
                    });
                }
            }
            _ => return None,
        }
        None
    }
}
