//! `BoundedChannel[T]` method dispatch — `BoundedChannel.new`
//! (associated function, fired from `eval_call`'s path-string match) and
//! the `send` / `recv` instance methods.
//!
//! Storage: `Interpreter.bounded_channel_table: HashMap<i64,
//! BoundedChannelEntry>` keyed by `BoundedChannel.handle_id`. Each entry
//! holds the `capacity` bound, whether a full `send` fails fast, and the
//! live `VecDeque<Value>` buffer (T erases at runtime, like `Pool[T]`).
//!
//! v1 single-threaded semantics (collapsed): no peer can drain the buffer
//! while a sender waits, so `OnFull::Block` behaves like `FailFast` — a
//! full `send` returns `Err(ChannelError.Full)` rather than parking, and
//! `recv` returns `None` on an empty buffer rather than blocking. The
//! real parking-on-full lands with the network event loop.

use std::collections::{HashMap, VecDeque};

use crate::ast::*;
use crate::token::Span;

use super::value::{EnumData, Value};
use super::BoundedChannelEntry;

impl<'a> super::Interpreter<'a> {
    /// `BoundedChannel.new(capacity, on_full) -> BoundedChannel[T]`.
    /// Records the bound + on-full policy in `bounded_channel_table`
    /// keyed by a fresh monotonic handle. `capacity` is clamped to be
    /// non-negative (a 0-capacity channel always fails `send`).
    pub(super) fn eval_bounded_channel_new(&mut self, args: &[CallArg]) -> Option<Value> {
        let capacity = match args.first().map(|a| self.eval_expr_inner(&a.value))? {
            Value::Int(i) => i.max(0),
            _ => return None,
        };
        // `on_full` is an `OnFull` enum; `FailFast` and (v1-collapsed)
        // `Block` both fail a full send, so only `FailFast` needs to be
        // distinguished from a hypothetical future non-failing mode.
        let fail_fast = match args.get(1).map(|a| self.eval_expr_inner(&a.value))? {
            Value::EnumVariant {
                enum_name, variant, ..
            } if enum_name == "OnFull" => variant == "FailFast" || variant == "Block",
            // Absent / wrong-typed policy: default to fail-fast (the
            // safe, non-parking behavior).
            _ => true,
        };

        self.bounded_channel_handle_counter += 1;
        let handle = self.bounded_channel_handle_counter;
        self.bounded_channel_table.insert(
            handle,
            BoundedChannelEntry {
                capacity,
                fail_fast,
                queue: VecDeque::new(),
            },
        );

        let mut fields = HashMap::new();
        fields.insert("handle_id".to_string(), Value::Int(handle));
        Some(Value::Struct {
            name: "BoundedChannel".to_string(),
            fields,
        })
    }

    pub(super) fn try_eval_bounded_channel_method(
        &mut self,
        method: &str,
        obj: Value,
        args: &[CallArg],
        _span: &Span,
    ) -> Option<Value> {
        match method {
            "send" => self.eval_bounded_channel_send(obj, args),
            "recv" => self.eval_bounded_channel_recv(obj),
            _ => None,
        }
    }

    fn eval_bounded_channel_send(&mut self, obj: Value, args: &[CallArg]) -> Option<Value> {
        let handle = bounded_channel_handle(&obj)?;
        let value = args.first().map(|a| self.eval_expr_inner(&a.value))?;
        match self.bounded_channel_table.get_mut(&handle) {
            Some(entry) if (entry.queue.len() as i64) < entry.capacity => {
                entry.queue.push_back(value);
                Some(result_ok(Value::Unit))
            }
            // Full buffer (or a hand-rolled `handle_id: 0` absent from
            // the table): fail closed. `Block` can't park in v1.
            _ => Some(result_err(channel_error("Full"))),
        }
    }

    fn eval_bounded_channel_recv(&mut self, obj: Value) -> Option<Value> {
        let handle = bounded_channel_handle(&obj)?;
        let popped = self
            .bounded_channel_table
            .get_mut(&handle)
            .and_then(|entry| entry.queue.pop_front());
        Some(match popped {
            Some(v) => option_some(v),
            None => option_none(),
        })
    }
}

fn bounded_channel_handle(obj: &Value) -> Option<i64> {
    let Value::Struct { name, fields } = obj else {
        return None;
    };
    if name != "BoundedChannel" {
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

fn channel_error(variant: &str) -> Value {
    Value::EnumVariant {
        enum_name: "ChannelError".to_string(),
        variant: variant.to_string(),
        data: EnumData::Unit,
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
