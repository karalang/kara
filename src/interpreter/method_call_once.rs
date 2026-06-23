//! `OnceLock[T]` / `OnceCell[T]` method dispatch — the associated
//! `OnceLock.new` / `OnceCell.new` (fired from `eval_call`'s
//! path-string match) and the instance methods `set` / `get` /
//! `get_or_init` / `is_set`. The cell's slot lives in the
//! interpreter's `once_table` between calls; the Kāra value carries
//! only a `handle_id` that keys into the side-table.
//!
//! **Write-once semantics.** A cell starts empty (`None`). `set(v)`
//! fills it and returns `Ok(Unit)` on the first call; a later `set`
//! leaves the slot untouched and returns `Err(AlreadySetError {
//! rejected: v })`. `get()` reads the slot as `Option[ref T]`.
//! `get_or_init(f)` returns the filled value, running `f` to fill the
//! cell first only when empty — so the init closure fires at most
//! once. `is_set()` reports whether the slot is filled.
//!
//! **One table for both types.** The cross-task vs. single-task split
//! between `OnceLock` and `OnceCell` is enforced entirely at
//! typecheck time (three structural rules); the tree-walk interpreter
//! is single-threaded, so the runtime storage is identical for both —
//! a single `once_table: HashMap<i64, Option<Value>>`. The receiver's
//! struct name (`OnceLock` / `OnceCell`) only matters for dispatch
//! recognition, not for storage.
//!
//! **Interpreter-only.** Mirrors `Pool[T]` / `Arena[T]` / `Interner` —
//! no codegen lowering. The baked `once.kara` placeholder bodies are
//! never evaluated (this dispatch intercepts before the body runs).

use std::collections::HashMap;

use crate::ast::*;
use crate::token::Span;

use super::value::{EnumData, Value};

impl<'a> super::Interpreter<'a> {
    /// `OnceLock.new() -> OnceLock[T]` / `OnceCell.new() -> OnceCell[T]`.
    /// Allocate a fresh empty slot in `once_table` keyed by a fresh
    /// monotonic handle id, return a `<type_name> { handle_id }`.
    /// Called from `eval_call`'s path-string match with the concrete
    /// cell type name.
    pub(super) fn eval_once_new(&mut self, type_name: &str) -> Option<Value> {
        self.once_handle_counter += 1;
        let handle = self.once_handle_counter;
        self.once_table.insert(handle, None);

        let mut fields = HashMap::new();
        fields.insert("handle_id".to_string(), Value::Int(handle));
        Some(Value::Struct {
            name: type_name.to_string(),
            fields,
        })
    }

    pub(super) fn try_eval_once_method(
        &mut self,
        method: &str,
        obj: &Value,
        args: &[CallArg],
        span: &Span,
    ) -> Option<Value> {
        // Borrow + match-on-name: a non-cell receiver returns `None`
        // here without cloning, falling through to the next dispatch
        // guard.
        once_handle(obj)?;
        match method {
            "set" => self.eval_once_set(obj, args),
            "get" => self.eval_once_get(obj),
            "get_or_init" => self.eval_once_get_or_init(obj, args, span),
            "is_set" => self.eval_once_is_set(obj),
            _ => None,
        }
    }

    /// `cell.set(value) -> Result[Unit, AlreadySetError[T]]` — fill the
    /// cell on first call (`Ok(Unit)`), reject every later call handing
    /// the value back (`Err(AlreadySetError { rejected: value })`). A
    /// hand-rolled `handle_id: 0` literal (no table entry) degrades to
    /// `Ok(Unit)` without storing rather than crashing.
    fn eval_once_set(&mut self, obj: &Value, args: &[CallArg]) -> Option<Value> {
        let handle = once_handle(obj)?;
        let value = args.first().map(|a| self.eval_expr_inner(&a.value))?;
        if self.check_cf() {
            return Some(Value::Unit);
        }
        match self.once_table.get_mut(&handle) {
            Some(slot) => {
                if slot.is_some() {
                    Some(result_err(already_set_error(value)))
                } else {
                    *slot = Some(value);
                    Some(result_ok(Value::Unit))
                }
            }
            // Bypassed `OnceLock.new` (handle 0 / foreign handle): no
            // backing slot to fill — degrade to a successful no-op.
            None => Some(result_ok(Value::Unit)),
        }
    }

    /// `cell.get() -> Option[ref T]` — `Some(v)` once filled, `None`
    /// before (and for a foreign / hand-rolled handle).
    fn eval_once_get(&mut self, obj: &Value) -> Option<Value> {
        let handle = once_handle(obj)?;
        match self.once_table.get(&handle) {
            Some(Some(v)) => Some(option_some(v.clone())),
            _ => Some(option_none()),
        }
    }

    /// `cell.get_or_init(init) -> ref T` — return the filled value,
    /// running `init` to fill the cell first only when empty. The
    /// closure fires at most once: a `get_or_init` on an already-filled
    /// cell returns the cached value without evaluating `init`.
    fn eval_once_get_or_init(
        &mut self,
        obj: &Value,
        args: &[CallArg],
        span: &Span,
    ) -> Option<Value> {
        let handle = once_handle(obj)?;
        // Already filled → return the cached value, do NOT run `init`.
        if let Some(Some(v)) = self.once_table.get(&handle) {
            return Some(v.clone());
        }
        // Empty → evaluate + invoke the init closure, then store its
        // result. The closure's own effects are attributed to the
        // caller by the effect walk; here we just run it.
        let closure = args.first().map(|a| self.eval_expr_inner(&a.value))?;
        if self.check_cf() {
            return Some(Value::Unit);
        }
        let produced = self.invoke_zero_arg_closure(closure, span);
        if self.check_cf() {
            return Some(produced);
        }
        if let Some(slot) = self.once_table.get_mut(&handle) {
            *slot = Some(produced.clone());
        }
        Some(produced)
    }

    /// `cell.is_set() -> bool` — whether the slot is filled.
    fn eval_once_is_set(&mut self, obj: &Value) -> Option<Value> {
        let handle = once_handle(obj)?;
        let filled = matches!(self.once_table.get(&handle), Some(Some(_)));
        Some(Value::Bool(filled))
    }
}

// ── Receiver-shape + value helpers ────────────────────────────────

/// `Some(handle_id)` when `obj` is an `OnceLock` / `OnceCell` struct
/// carrying an integer `handle_id`; `None` otherwise (so a non-cell
/// receiver falls through to the next dispatch guard).
fn once_handle(obj: &Value) -> Option<i64> {
    let Value::Struct { name, fields } = obj else {
        return None;
    };
    if name != "OnceLock" && name != "OnceCell" {
        return None;
    }
    match fields.get("handle_id") {
        Some(Value::Int(h)) => Some(*h),
        _ => None,
    }
}

fn already_set_error(rejected: Value) -> Value {
    let mut fields = HashMap::new();
    fields.insert("rejected".to_string(), rejected);
    Value::Struct {
        name: "AlreadySetError".to_string(),
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

fn option_some(v: Value) -> Value {
    Value::EnumVariant {
        enum_name: "Option".to_string(),
        variant: "Some".to_string(),
        data: EnumData::Tuple(vec![v]),
    }
}

fn option_none() -> Value {
    Value::EnumVariant {
        enum_name: "Option".to_string(),
        variant: "None".to_string(),
        data: EnumData::Unit,
    }
}
