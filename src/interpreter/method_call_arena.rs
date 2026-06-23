//! `Arena[T]` method dispatch — `Arena.new` (associated function,
//! fired from `eval_call`'s path-string match for `"Arena.new"`) and
//! the instance methods `arena.push` / `arena.get` /
//! `arena.high_water_mark` / `arena.rewind_to` / `arena.len`. The
//! bump-allocated values live in the interpreter's `arena_table`
//! between calls; the `Arena[T]` Kāra struct just carries a
//! `handle_id` that keys into the side-table.
//!
//! **Single-lifetime semantics.** Unlike `Pool[T]`, an arena has no
//! per-item release and no generation counter: `push` appends to the
//! backing `Vec<Value>` and returns an `ArenaRef { handle, index }`;
//! `get` reads back the value at that index; every item lives until
//! the whole arena drops. `high_water_mark` / `rewind_to` give
//! snapshot/restore by recording and truncating the vec length.
//!
//! **Interpreter-only.** `std.Arena` mirrors `std.Pool` — no codegen
//! lowering. Generic `T` erases at runtime (a slot is just a
//! `Value`). The baked `arena.kara` placeholder bodies are never
//! evaluated (this dispatch intercepts before the body runs).

use std::collections::HashMap;

use crate::ast::*;
use crate::token::Span;

use super::value::Value;

impl<'a> super::Interpreter<'a> {
    /// `Arena.new() -> Arena[T]`. Allocates a fresh empty backing vec
    /// in `arena_table` keyed by a fresh monotonic handle id, returns
    /// an `Arena { handle_id }`. Called from `eval_call`'s path-string
    /// match.
    pub(super) fn eval_arena_new(&mut self, _args: &[CallArg]) -> Option<Value> {
        self.arena_handle_counter += 1;
        let handle = self.arena_handle_counter;
        self.arena_table.insert(handle, Vec::new());

        let mut fields = HashMap::new();
        fields.insert("handle_id".to_string(), Value::Int(handle));
        Some(Value::Struct {
            name: "Arena".to_string(),
            fields,
        })
    }

    pub(super) fn try_eval_arena_method(
        &mut self,
        method: &str,
        obj: &Value,
        args: &[CallArg],
        _span: &Span,
    ) -> Option<Value> {
        // Borrow + match-on-name: a non-arena receiver (e.g. a `Map`'s
        // own `len`/`get`) returns `None` here without cloning, falling
        // through to the next method-dispatch guard.
        arena_handle(obj)?;
        match method {
            "push" => self.eval_arena_push(obj, args),
            "get" => self.eval_arena_get(obj, args),
            "high_water_mark" => self.eval_arena_high_water_mark(obj),
            "rewind_to" => self.eval_arena_rewind_to(obj, args),
            "len" => self.eval_arena_len(obj),
            _ => None,
        }
    }

    /// `arena.push(value) -> ArenaRef[T]` — bump-allocate `value` into
    /// the backing vec and return a handle carrying its index. A
    /// hand-rolled / closed arena (no table entry) drops the value and
    /// returns an index-0 handle rather than panicking.
    fn eval_arena_push(&mut self, obj: &Value, args: &[CallArg]) -> Option<Value> {
        let handle = arena_handle(obj)?;
        let value = args.first().map(|a| self.eval_expr_inner(&a.value))?;
        let index = if let Some(vec) = self.arena_table.get_mut(&handle) {
            let idx = vec.len() as i64;
            vec.push(value);
            idx
        } else {
            0
        };
        Some(arena_ref(handle, index))
    }

    /// `arena.get(r) -> ref T` — resolve a handle back to its stored
    /// value (returned by clone; the tree-walk interpreter cannot hand
    /// out a real borrow). Cross-arena handles (a ref minted by a
    /// different arena) and dangling handles (an index truncated away
    /// by `rewind_to`) degrade to `Unit` rather than read out of
    /// bounds — the sound debug-mode panic is a subsequent slice.
    fn eval_arena_get(&mut self, obj: &Value, args: &[CallArg]) -> Option<Value> {
        let handle = arena_handle(obj)?;
        let r = args.first().map(|a| self.eval_expr_inner(&a.value))?;
        let (ref_handle, index) = unpack_arena_ref(r)?;
        if ref_handle != handle {
            // Cross-arena get — the ref came from a different arena.
            return Some(Value::Unit);
        }
        let vec = self.arena_table.get(&handle)?;
        match vec.get(index as usize) {
            Some(v) => Some(v.clone()),
            None => Some(Value::Unit),
        }
    }

    /// `arena.high_water_mark() -> ArenaCheckpoint` — record the
    /// arena's current length for a later `rewind_to`.
    fn eval_arena_high_water_mark(&mut self, obj: &Value) -> Option<Value> {
        let handle = arena_handle(obj)?;
        let mark = self
            .arena_table
            .get(&handle)
            .map(|v| v.len() as i64)
            .unwrap_or(0);
        Some(arena_checkpoint(handle, mark))
    }

    /// `arena.rewind_to(cp)` — truncate the backing vec back to the
    /// recorded checkpoint length, dropping every item pushed since. A
    /// checkpoint minted by a different arena is ignored; a stale mark
    /// is clamped to `[0, len]`.
    fn eval_arena_rewind_to(&mut self, obj: &Value, args: &[CallArg]) -> Option<Value> {
        let handle = arena_handle(obj)?;
        let cp = args.first().map(|a| self.eval_expr_inner(&a.value))?;
        let (cp_handle, mark) = unpack_arena_checkpoint(cp)?;
        if cp_handle == handle {
            if let Some(vec) = self.arena_table.get_mut(&handle) {
                let target = (mark.max(0) as usize).min(vec.len());
                vec.truncate(target);
            }
        }
        Some(Value::Unit)
    }

    /// `arena.len() -> i64` — number of items currently live.
    fn eval_arena_len(&mut self, obj: &Value) -> Option<Value> {
        let handle = arena_handle(obj)?;
        let n = self
            .arena_table
            .get(&handle)
            .map(|v| v.len() as i64)
            .unwrap_or(0);
        Some(Value::Int(n))
    }
}

// ── Receiver-shape helpers ────────────────────────────────────────

fn arena_handle(obj: &Value) -> Option<i64> {
    let Value::Struct { name, fields } = obj else {
        return None;
    };
    if name != "Arena" {
        return None;
    }
    match fields.get("handle_id") {
        Some(Value::Int(h)) => Some(*h),
        _ => None,
    }
}

fn unpack_arena_ref(obj: Value) -> Option<(i64, i64)> {
    let Value::Struct { name, fields } = obj else {
        return None;
    };
    if name != "ArenaRef" {
        return None;
    }
    let handle = match fields.get("arena_handle_id") {
        Some(Value::Int(h)) => *h,
        _ => return None,
    };
    let index = match fields.get("index") {
        Some(Value::Int(i)) => *i,
        _ => return None,
    };
    Some((handle, index))
}

fn unpack_arena_checkpoint(obj: Value) -> Option<(i64, i64)> {
    let Value::Struct { name, fields } = obj else {
        return None;
    };
    if name != "ArenaCheckpoint" {
        return None;
    }
    let handle = match fields.get("arena_handle_id") {
        Some(Value::Int(h)) => *h,
        _ => return None,
    };
    let mark = match fields.get("mark") {
        Some(Value::Int(m)) => *m,
        _ => return None,
    };
    Some((handle, mark))
}

// ── Kāra-value constructors ───────────────────────────────────────

fn arena_ref(handle: i64, index: i64) -> Value {
    let mut fields = HashMap::new();
    fields.insert("arena_handle_id".to_string(), Value::Int(handle));
    fields.insert("index".to_string(), Value::Int(index));
    Value::Struct {
        name: "ArenaRef".to_string(),
        fields,
    }
}

fn arena_checkpoint(handle: i64, mark: i64) -> Value {
    let mut fields = HashMap::new();
    fields.insert("arena_handle_id".to_string(), Value::Int(handle));
    fields.insert("mark".to_string(), Value::Int(mark));
    Value::Struct {
        name: "ArenaCheckpoint".to_string(),
        fields,
    }
}
