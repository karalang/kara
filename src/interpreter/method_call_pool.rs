//! Pool[T] method dispatch — `Pool.new` (associated function, fired
//! from `eval_call`'s path-string match for `"Pool.new"`) and the
//! instance methods `pool.acquire` / `pool.release`. Connection
//! values live in the interpreter's `pool_table` between calls; the
//! `Pool[T]` Kāra struct just carries a `handle_id` that keys into
//! the side-table.
//!
//! **Single-threaded semantics.** The tree-walk interpreter has no
//! actual concurrent thread that could free a slot mid-`acquire`,
//! so the spec's "bounded waiters with timeout" collapses to: serve
//! immediately if a slot is available or the pool is below
//! `max_connections` (mint via `create_fn`); otherwise return
//! `PoolError.Timeout` straight away. The `max_waiters` parameter
//! is captured for forward compatibility with a future threaded
//! backend (codegen, runtime) but doesn't gate a waiter queue here.
//!
//! **Drop-releases-automatically — deferred.** The spec's "drop
//! releases automatically" contract needs user-`impl Drop` dispatch
//! in the interpreter, which is not yet wired (per
//! `src/interpreter/eval_stmt.rs::run_cleanup` — `CleanupAction::Drop`
//! is a trace-only no-op until user Drop lands). v1 ships an
//! explicit `pool.release(conn)` method instead; once user Drop
//! dispatches, `PooledConnection`'s drop body can call the same
//! release path and the explicit call becomes optional.

use std::collections::HashMap;

use crate::ast::*;
use crate::token::Span;

use super::exec::ControlFlow;
use super::value::{EnumData, Value};
use super::PoolEntry;

impl<'a> super::Interpreter<'a> {
    /// `Pool.new(create_fn, max_connections, max_waiters) -> Pool[T]`.
    /// Stashes the closure + bounds in `pool_table` keyed by a fresh
    /// monotonic handle id, returns a `Value::Struct` carrying that
    /// handle. Called from `eval_call`'s path-string match.
    pub(super) fn eval_pool_new(&mut self, args: &[CallArg]) -> Option<Value> {
        let create_fn = args.first().map(|a| self.eval_expr_inner(&a.value))?;
        if !matches!(create_fn, Value::Function { .. }) {
            return None;
        }
        let max_connections = args
            .get(1)
            .map(|a| match self.eval_expr_inner(&a.value) {
                Value::Int(i) => i,
                _ => 0,
            })
            .unwrap_or(0);
        let max_waiters = args
            .get(2)
            .map(|a| match self.eval_expr_inner(&a.value) {
                Value::Int(i) => i,
                _ => 0,
            })
            .unwrap_or(0);

        self.pool_handle_counter += 1;
        let handle = self.pool_handle_counter;
        self.pool_table.insert(
            handle,
            PoolEntry {
                create_fn,
                max_connections,
                max_waiters,
                slots: Vec::new(),
                active_count: 0,
                health_check: None,
            },
        );

        let mut pool_fields = HashMap::new();
        pool_fields.insert("handle_id".to_string(), Value::Int(handle));
        Some(Value::Struct {
            name: "Pool".to_string(),
            fields: pool_fields,
        })
    }

    pub(super) fn try_eval_pool_method(
        &mut self,
        method: &str,
        obj: Value,
        args: &[CallArg],
        _span: &Span,
    ) -> Option<Value> {
        match method {
            "acquire" => self.eval_pool_acquire(obj),
            "release" => self.eval_pool_release(obj, args),
            "with_health_check" => self.eval_pool_with_health_check(obj, args),
            _ => None,
        }
    }

    /// `pool.with_health_check(check) -> Pool[T]` — register the `Fn(T) ->
    /// bool` validation hook on the pool entry and return the pool handle
    /// (so it chains off `Pool.new(...)`). `acquire` consults it on every
    /// idle slot it reuses. Re-registering replaces the prior hook.
    fn eval_pool_with_health_check(&mut self, obj: Value, args: &[CallArg]) -> Option<Value> {
        let handle = pool_handle(&obj)?;
        let check = args.first().map(|a| self.eval_expr_inner(&a.value))?;
        if let Some(entry) = self.pool_table.get_mut(&handle) {
            entry.health_check = Some(check);
        }
        // Return the same handle value so the call chains off `Pool.new`.
        Some(obj)
    }

    fn eval_pool_acquire(&mut self, obj: Value) -> Option<Value> {
        let handle = pool_handle(&obj)?;

        // Fast path: reuse an idle slot from `slots`, validated by the
        // health-check hook if one is registered. Pop slots until we find a
        // healthy one (hand it back without consulting `create_fn`) or run
        // out (fall through to the mint-or-fail branch). An unhealthy slot
        // is evicted: the connection is destroyed, so `active_count` drops
        // by one — that's what lets the mint path below find room to create
        // a fresh replacement even when the pool was at its `max_connections`
        // cap (the evict-on-error pattern).
        loop {
            let (popped, health_check) = {
                let Some(entry) = self.pool_table.get_mut(&handle) else {
                    return Some(result_err(pool_error("PoolClosed")));
                };
                (entry.slots.pop(), entry.health_check.clone())
            };
            let Some(val) = popped else {
                // No idle slots left — fall through to mint-or-fail.
                break;
            };
            let Some(check) = health_check else {
                // No hook → hand the idle slot straight back.
                return Some(result_ok(pooled_connection(handle, val)));
            };
            // Validate on a clone so the slot value survives a healthy
            // verdict. `invoke_function_value` returns `Unit` on a
            // non-callable hook; treat any non-`bool` result as healthy so a
            // malformed hook never silently evicts.
            let healthy = matches!(
                self.invoke_function_value(check, vec![val.clone()]),
                Value::Bool(true) | Value::Unit
            );
            if healthy {
                return Some(result_ok(pooled_connection(handle, val)));
            }
            // Unhealthy: evict (drop `val`) and decrement `active_count`.
            if let Some(entry) = self.pool_table.get_mut(&handle) {
                if entry.active_count > 0 {
                    entry.active_count -= 1;
                }
            }
            // Loop to try the next idle slot, or fall through to mint.
        }

        // Decide whether to mint a fresh slot. Snapshot the gate
        // values so we can drop the borrow before invoking
        // `create_fn` (which re-enters the interpreter and may
        // touch `pool_table` itself).
        let (under_cap, create_fn) = {
            let entry = self.pool_table.get(&handle)?;
            (
                entry.active_count < entry.max_connections,
                entry.create_fn.clone(),
            )
        };
        if !under_cap {
            // Spec: bounded waiters with timeout. Single-threaded
            // interpreter has no peer to free a slot, so an
            // at-cap acquire fails immediately.
            return Some(result_err(pool_error("Timeout")));
        }

        let minted = self.invoke_pool_create_fn(create_fn)?;
        // Re-borrow after the user closure may have mutated state.
        if let Some(entry) = self.pool_table.get_mut(&handle) {
            entry.active_count += 1;
            Some(result_ok(pooled_connection(handle, minted)))
        } else {
            // Closure deleted the pool out from under us — surface
            // as PoolClosed rather than panic.
            Some(result_err(pool_error("PoolClosed")))
        }
    }

    fn eval_pool_release(&mut self, obj: Value, args: &[CallArg]) -> Option<Value> {
        let pool_handle = pool_handle(&obj)?;
        let conn_val = args.first().map(|a| self.eval_expr_inner(&a.value))?;
        let (conn_pool_handle, val) = unpack_pooled_connection(conn_val)?;
        if conn_pool_handle != pool_handle {
            // Cross-pool release — user passed a connection minted
            // by a different pool. Drop the value silently rather
            // than corrupting the target pool's bookkeeping.
            return Some(Value::Unit);
        }
        if let Some(entry) = self.pool_table.get_mut(&pool_handle) {
            entry.slots.push(val);
        }
        Some(Value::Unit)
    }

    /// Invoke a zero-arg `Value::Function` and return its result.
    /// Mirrors the closure-call shape in
    /// `eval_call.rs::invoke_zero_arg_closure` so any future change
    /// to closure invocation semantics flows through both sites.
    /// Pool-specific because the existing helper is private to
    /// `eval_call.rs` and panics on non-Function — here we'd rather
    /// surface a clean `None` (acquire then returns `PoolClosed`)
    /// than abort if the typechecker ever lets a non-callable
    /// through the `create_fn` slot.
    fn invoke_pool_create_fn(&mut self, func: Value) -> Option<Value> {
        let Value::Function {
            body, closure_env, ..
        } = func
        else {
            return None;
        };
        self.env.push_scope();
        if let Some(captured) = closure_env {
            for (k, v) in captured {
                self.env.define(k, v);
            }
        }
        let result = self.eval_block_inner(&body);
        self.env.pop_scope();
        Some(match result {
            Ok(v) => v,
            Err(ControlFlow::Return(v)) => v,
            Err(cf) => self.set_cf(cf),
        })
    }
}

// ── Receiver-shape helpers ────────────────────────────────────────

fn pool_handle(obj: &Value) -> Option<i64> {
    let Value::Struct { name, fields } = obj else {
        return None;
    };
    if name != "Pool" {
        return None;
    }
    match fields.get("handle_id") {
        Some(Value::Int(h)) => Some(*h),
        _ => None,
    }
}

fn unpack_pooled_connection(obj: Value) -> Option<(i64, Value)> {
    let Value::Struct { name, mut fields } = obj else {
        return None;
    };
    if name != "PooledConnection" {
        return None;
    }
    let handle = match fields.get("pool_handle_id") {
        Some(Value::Int(h)) => *h,
        _ => return None,
    };
    let val = fields.remove("val")?;
    Some((handle, val))
}

// ── Kāra-value constructors ───────────────────────────────────────

fn pooled_connection(pool_handle: i64, val: Value) -> Value {
    let mut fields = HashMap::new();
    fields.insert("pool_handle_id".to_string(), Value::Int(pool_handle));
    fields.insert("val".to_string(), val);
    Value::Struct {
        name: "PooledConnection".to_string(),
        fields,
    }
}

fn result_ok(v: Value) -> Value {
    Value::EnumVariant {
        enum_name: "Result".to_string(),
        variant: "Ok".to_string(),
        data: EnumData::Tuple(vec![v]),
    }
}

fn result_err(v: Value) -> Value {
    Value::EnumVariant {
        enum_name: "Result".to_string(),
        variant: "Err".to_string(),
        data: EnumData::Tuple(vec![v]),
    }
}

fn pool_error(variant: &str) -> Value {
    Value::EnumVariant {
        enum_name: "PoolError".to_string(),
        variant: variant.to_string(),
        data: EnumData::Unit,
    }
}
