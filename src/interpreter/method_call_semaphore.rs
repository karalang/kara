//! `Semaphore` method dispatch — `Semaphore.new` (associated function,
//! fired from `eval_call`'s path-string match for `"Semaphore.new"`) and
//! the `acquire` / `release` instance methods.
//!
//! Storage: `Interpreter.semaphore_table: HashMap<i64, SemEntry>` keyed
//! by `Semaphore.handle_id`. `new` mints an entry; `acquire` decrements
//! `available` when a permit is free; `release` increments it, saturating
//! at the initial budget. The `Semaphore` Kāra struct carries only the
//! `handle_id` that keys into the table.
//!
//! v1 single-threaded semantics (collapsed): the tree-walk interpreter
//! has no peer that could free a permit while a caller waits, so an
//! `acquire` against an exhausted semaphore fails immediately with
//! `SemaphoreError.Timeout` — the same immediate-serve-or-timeout shape
//! `Pool[T]` uses. The parking-with-timeout backend lands with the
//! network event loop.

use std::collections::HashMap;

use crate::ast::*;
use crate::token::Span;

use super::value::{EnumData, Value};
use super::SemEntry;

impl<'a> super::Interpreter<'a> {
    /// `Semaphore.new(permits) -> Semaphore`. Records the permit budget
    /// in `semaphore_table` keyed by a fresh monotonic handle and
    /// returns a `Semaphore { handle_id }`. Called from `eval_call`'s
    /// path-string match. A negative `permits` is clamped to 0 (a
    /// semaphore that never grants).
    pub(super) fn eval_semaphore_new(&mut self, args: &[CallArg]) -> Option<Value> {
        let permits = match args.first().map(|a| self.eval_expr_inner(&a.value))? {
            Value::Int(i) => i.max(0),
            _ => return None,
        };

        self.semaphore_handle_counter += 1;
        let handle = self.semaphore_handle_counter;
        self.semaphore_table.insert(
            handle,
            SemEntry {
                available: permits,
                max: permits,
            },
        );

        let mut fields = HashMap::new();
        fields.insert("handle_id".to_string(), Value::Int(handle));
        Some(Value::Struct {
            name: "Semaphore".to_string(),
            fields,
        })
    }

    pub(super) fn try_eval_semaphore_method(
        &mut self,
        method: &str,
        obj: Value,
        _args: &[CallArg],
        _span: &Span,
    ) -> Option<Value> {
        match method {
            "acquire" => self.eval_semaphore_acquire(obj),
            "release" => self.eval_semaphore_release(obj),
            _ => None,
        }
    }

    fn eval_semaphore_acquire(&mut self, obj: Value) -> Option<Value> {
        let handle = semaphore_handle(&obj)?;
        match self.semaphore_table.get_mut(&handle) {
            Some(entry) if entry.available > 0 => {
                entry.available -= 1;
                Some(result_ok(Value::Unit))
            }
            // Exhausted (or a hand-rolled `Semaphore { handle_id: 0 }`
            // absent from the table): nothing in the single-threaded
            // interpreter can free a permit mid-call, so fail closed.
            _ => Some(result_err(semaphore_error("Timeout"))),
        }
    }

    fn eval_semaphore_release(&mut self, obj: Value) -> Option<Value> {
        let handle = semaphore_handle(&obj)?;
        if let Some(entry) = self.semaphore_table.get_mut(&handle) {
            // Saturate at the initial budget — returning more permits
            // than were taken is a bookkeeping bug, and growing past
            // `max` would inflate the in-flight budget `new` declared.
            if entry.available < entry.max {
                entry.available += 1;
            }
        }
        Some(Value::Unit)
    }
}

// ── Receiver-shape helpers ────────────────────────────────────────

fn semaphore_handle(obj: &Value) -> Option<i64> {
    let Value::Struct { name, fields } = obj else {
        return None;
    };
    if name != "Semaphore" {
        return None;
    }
    match fields.get("handle_id") {
        Some(Value::Int(h)) => Some(*h),
        _ => None,
    }
}

// ── Kāra-value constructors ───────────────────────────────────────

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

fn semaphore_error(variant: &str) -> Value {
    Value::EnumVariant {
        enum_name: "SemaphoreError".to_string(),
        variant: variant.to_string(),
        data: EnumData::Unit,
    }
}
