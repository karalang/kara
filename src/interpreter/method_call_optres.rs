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
        // The receiver place-expr. No longer needed for atomic mutation —
        // `Value::Atomic` is now an `Arc<Mutex<…>>` shared cell mutated in
        // place under lock, so there is no write-back to the env slot/field.
        _object: &Expr,
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
                    // Ordering argument accepted but ignored — the `Mutex`
                    // already serialises every op, which is stronger than any
                    // requested ordering.
                    return Some(cell.lock().unwrap().clone());
                }
            }
            "store" => {
                if let Value::Atomic(cell) = obj {
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
