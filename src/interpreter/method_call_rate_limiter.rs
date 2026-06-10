//! `RateLimiter` method dispatch — `RateLimiter.new_token_bucket`
//! (associated function, fired from `eval_call`'s path-string match) and
//! the `try_acquire` instance method.
//!
//! Storage: `Interpreter.rate_limiter_table: HashMap<i64,
//! RateLimiterEntry>` keyed by `RateLimiter.handle_id`. Each entry holds
//! the bucket config (`rate` tokens/sec, `capacity`) plus a per-key
//! `TokenBucket { tokens, last }`. `try_acquire` lazily refills the key's
//! bucket from the elapsed monotonic time, then consumes a token if one
//! is available.
//!
//! v1 ships the synchronous, non-blocking `try_acquire` only; the
//! waiting `acquire(key) -> impl Future` form is event-loop dependent
//! (carved as a follow-on). Time comes from `std::time::Instant` — a
//! monotonic clock read directly in the intrinsic, the same way the
//! `Clock`/`RandomSource` ambient resources read `SystemTime` in
//! `resource_method.rs`.

use std::time::Instant;

use crate::ast::*;
use crate::token::Span;

use super::value::Value;
use super::{RateLimiterEntry, TokenBucket};

impl<'a> super::Interpreter<'a> {
    /// `RateLimiter.new_token_bucket(rate, capacity) -> RateLimiter`.
    /// Records the bucket config in `rate_limiter_table` keyed by a fresh
    /// monotonic handle and returns a `RateLimiter { handle_id }`. `rate`
    /// and `capacity` are clamped to be non-negative (a 0-rate / 0-cap
    /// limiter simply never grants).
    pub(super) fn eval_rate_limiter_new(&mut self, args: &[CallArg]) -> Option<Value> {
        let rate = match args.first().map(|a| self.eval_expr_inner(&a.value))? {
            Value::Int(i) => i.max(0) as f64,
            _ => return None,
        };
        let capacity = match args.get(1).map(|a| self.eval_expr_inner(&a.value))? {
            Value::Int(i) => i.max(0) as f64,
            _ => return None,
        };

        self.rate_limiter_handle_counter += 1;
        let handle = self.rate_limiter_handle_counter;
        self.rate_limiter_table.insert(
            handle,
            RateLimiterEntry {
                rate,
                capacity,
                buckets: std::collections::HashMap::new(),
            },
        );

        let mut fields = std::collections::HashMap::new();
        fields.insert("handle_id".to_string(), Value::Int(handle));
        Some(Value::Struct {
            name: "RateLimiter".to_string(),
            fields,
        })
    }

    pub(super) fn try_eval_rate_limiter_method(
        &mut self,
        method: &str,
        obj: &Value,
        args: &[CallArg],
        _span: &Span,
    ) -> Option<Value> {
        match method {
            "try_acquire" => self.eval_rate_limiter_try_acquire(obj, args),
            _ => None,
        }
    }

    fn eval_rate_limiter_try_acquire(&mut self, obj: &Value, args: &[CallArg]) -> Option<Value> {
        let handle = rate_limiter_handle(obj)?;
        let key = match args.first().map(|a| self.eval_expr_inner(&a.value))? {
            Value::String(s) => s,
            _ => return None,
        };

        let now = Instant::now();
        let Some(entry) = self.rate_limiter_table.get_mut(&handle) else {
            // Hand-rolled `RateLimiter { handle_id: 0 }` with no table
            // entry — fail closed (limited).
            return Some(Value::Bool(false));
        };

        let rate = entry.rate;
        let capacity = entry.capacity;
        // A fresh key starts with a full bucket — the first `capacity`
        // grants are a burst, then grants are paced by `rate`.
        let bucket = entry.buckets.entry(key).or_insert(TokenBucket {
            tokens: capacity,
            last: now,
        });

        // Lazy refill: add `elapsed * rate` tokens, capped at capacity.
        let elapsed = now.saturating_duration_since(bucket.last).as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * rate).min(capacity);
        bucket.last = now;

        let granted = bucket.tokens >= 1.0;
        if granted {
            bucket.tokens -= 1.0;
        }
        Some(Value::Bool(granted))
    }
}

fn rate_limiter_handle(obj: &Value) -> Option<i64> {
    let Value::Struct { name, fields } = obj else {
        return None;
    };
    if name != "RateLimiter" {
        return None;
    }
    match fields.get("handle_id") {
        Some(Value::Int(h)) => Some(*h),
        _ => None,
    }
}
