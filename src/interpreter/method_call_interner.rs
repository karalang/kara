//! `Interner` method dispatch — `Interner.new` (associated function,
//! fired from `eval_call`'s path-string match for `"Interner.new"`)
//! and the instance methods `interner.intern` / `interner.resolve` /
//! `interner.len`. The interned strings + dedup index live in the
//! interpreter's `interner_table` between calls; the `Interner` Kāra
//! struct just carries a `handle_id` that keys into the side-table.
//!
//! **Dedup semantics.** `intern(s)` returns the existing handle when
//! `s` was already interned (no new storage), otherwise appends `s`
//! to the id→string `Vec`, records `s -> id` in the dedup `HashMap`,
//! and returns the fresh id. `Symbol` is a transparent `i64`, so a
//! handle is just a `Value::Int` at runtime (the distinct type erases
//! to its base, like every other distinct type in the interpreter).
//! `resolve(sym)` reads the `Vec` at the symbol's id.
//!
//! **Interpreter-only.** Mirrors `Pool[T]` / `Arena[T]` — no codegen
//! lowering. The baked `interner.kara` placeholder bodies are never
//! evaluated (this dispatch intercepts before the body runs).

use std::collections::HashMap;

use crate::ast::*;
use crate::token::Span;

use super::value::Value;

impl<'a> super::Interpreter<'a> {
    /// `Interner.new() -> Interner`. Allocates a fresh `(Vec, HashMap)`
    /// pair in `interner_table` keyed by a fresh monotonic handle id,
    /// returns an `Interner { handle_id }`. Called from `eval_call`'s
    /// path-string match.
    pub(super) fn eval_interner_new(&mut self, _args: &[CallArg]) -> Option<Value> {
        self.interner_handle_counter += 1;
        let handle = self.interner_handle_counter;
        self.interner_table
            .insert(handle, (Vec::new(), HashMap::new()));

        let mut fields = HashMap::new();
        fields.insert("handle_id".to_string(), Value::Int(handle));
        Some(Value::Struct {
            name: "Interner".to_string(),
            fields,
        })
    }

    pub(super) fn try_eval_interner_method(
        &mut self,
        method: &str,
        obj: &Value,
        args: &[CallArg],
        _span: &Span,
    ) -> Option<Value> {
        // Borrow + match-on-name: a non-interner receiver returns `None`
        // here without cloning, falling through to the next dispatch
        // guard.
        interner_handle(obj)?;
        match method {
            "intern" => self.eval_interner_intern(obj, args),
            "resolve" => self.eval_interner_resolve(obj, args),
            "len" => self.eval_interner_len(obj),
            _ => None,
        }
    }

    /// `interner.intern(s) -> Symbol` — return `s`'s existing handle if
    /// it was interned before, else store `s` and mint a fresh handle.
    /// `Symbol` erases to its base `i64`, so the handle is a
    /// `Value::Int`.
    fn eval_interner_intern(&mut self, obj: &Value, args: &[CallArg]) -> Option<Value> {
        let handle = interner_handle(obj)?;
        let s = match args.first().map(|a| self.eval_expr_inner(&a.value))? {
            Value::String(s) => s,
            _ => return None,
        };
        let (strings, index) = self.interner_table.get_mut(&handle)?;
        let id = match index.get(&s) {
            Some(existing) => *existing,
            None => {
                let fresh = strings.len() as i64;
                strings.push(s.clone());
                index.insert(s, fresh);
                fresh
            }
        };
        Some(Value::Int(id))
    }

    /// `interner.resolve(sym) -> ref String` — hand back the interned
    /// string at the symbol's id (by clone; the tree-walk interpreter
    /// can't hand out a real borrow — the static type is still `ref
    /// String`). A foreign / out-of-range handle degrades to the empty
    /// string rather than read out of bounds.
    fn eval_interner_resolve(&mut self, obj: &Value, args: &[CallArg]) -> Option<Value> {
        let handle = interner_handle(obj)?;
        let id = match args.first().map(|a| self.eval_expr_inner(&a.value))? {
            Value::Int(id) => id,
            _ => return None,
        };
        let (strings, _) = self.interner_table.get(&handle)?;
        match usize::try_from(id).ok().and_then(|i| strings.get(i)) {
            Some(s) => Some(Value::String(s.clone())),
            None => Some(Value::String(String::new())),
        }
    }

    /// `interner.len() -> i64` — number of distinct strings interned.
    fn eval_interner_len(&mut self, obj: &Value) -> Option<Value> {
        let handle = interner_handle(obj)?;
        let n = self
            .interner_table
            .get(&handle)
            .map(|(strings, _)| strings.len() as i64)
            .unwrap_or(0);
        Some(Value::Int(n))
    }
}

// ── Receiver-shape helper ─────────────────────────────────────────

fn interner_handle(obj: &Value) -> Option<i64> {
    let Value::Struct { name, fields } = obj else {
        return None;
    };
    if name != "Interner" {
        return None;
    }
    match fields.get("handle_id") {
        Some(Value::Int(h)) => Some(*h),
        _ => None,
    }
}
