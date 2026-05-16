//! Map / Map.Entry method dispatch — the bodies of the `get_or`/
//! `keys`/`values`/`entries`/`merge`/`insert`/`remove`/`entry`/
//! `or_insert`/`or_insert_with`/`and_modify` arms lifted out of
//! `eval_method_call`. Receivers are `Value::Map` and the
//! `Value::Entry` cursor returned by `Map::entry()`.

use std::sync::{Arc, Mutex};

use crate::ast::*;
use crate::token::Span;

use super::value::{EnumData, OrdValue, Value};

impl<'a> super::Interpreter<'a> {
    pub(super) fn try_eval_map_method(
        &mut self,
        method: &str,
        object: &Expr,
        obj: Value,
        args: &[CallArg],
        _span: &Span,
    ) -> Option<Value> {
        match method {
            // ── Map[K, V] methods ─────────────────────────────────────────
            "get_or" => {
                if let Value::Map(ref m) = obj {
                    let key = args
                        .first()
                        .map(|a| self.eval_expr_inner(&a.value))
                        .unwrap_or(Value::Unit);
                    let default = args
                        .get(1)
                        .map(|a| self.eval_expr_inner(&a.value))
                        .unwrap_or(Value::Unit);
                    return Some(match m.iter().find(|(k, _)| *k == key) {
                        Some((_, v)) => v.clone(),
                        None => default,
                    });
                }
            }
            "keys" => {
                if let Value::Map(ref m) = obj {
                    return Some(Value::array_of(m.iter().map(|(k, _)| k.clone()).collect()));
                }
            }
            "values" => {
                if let Value::Map(ref m) = obj {
                    return Some(Value::array_of(m.iter().map(|(_, v)| v.clone()).collect()));
                }
            }
            "entries" => {
                if let Value::Map(ref m) = obj {
                    return Some(Value::array_of(
                        m.iter()
                            .map(|(k, v)| Value::Tuple(vec![k.clone(), v.clone()]))
                            .collect(),
                    ));
                }
            }
            "merge" => {
                if let Value::Map(ref base) = obj {
                    let other = args
                        .first()
                        .map(|a| self.eval_expr_inner(&a.value))
                        .unwrap_or(Value::Map(Vec::new()));
                    if let Value::Map(other_entries) = other {
                        let mut result = base.clone();
                        for (k, v) in other_entries {
                            if let Some(entry) = result.iter_mut().find(|(ek, _)| *ek == k) {
                                entry.1 = v;
                            } else {
                                result.push((k, v));
                            }
                        }
                        return Some(Value::Map(result));
                    }
                }
            }

            // ── SortedSet[T: Ord] methods ──────────────────────────────────
            "insert" => {
                let val = args
                    .first()
                    .map(|a| self.eval_expr_inner(&a.value))
                    .unwrap_or(Value::Unit);
                if let Value::Map(mut m) = obj {
                    // Map.insert(key, value) -> Option[V] (old value)
                    let value = args
                        .get(1)
                        .map(|a| self.eval_expr_inner(&a.value))
                        .unwrap_or(Value::Unit);
                    let old = if let Some(entry) = m.iter_mut().find(|(k, _)| *k == val) {
                        let prev = entry.1.clone();
                        entry.1 = value;
                        Value::EnumVariant {
                            enum_name: "Option".to_string(),
                            variant: "Some".to_string(),
                            data: EnumData::Tuple(vec![prev]),
                        }
                    } else {
                        m.push((val, value));
                        Value::EnumVariant {
                            enum_name: "Option".to_string(),
                            variant: "None".to_string(),
                            data: EnumData::Unit,
                        }
                    };
                    if let ExprKind::Identifier(name) = &object.kind {
                        self.env.set(name, Value::Map(m));
                    }
                    return Some(old);
                }
                if let Value::SortedSet(mut set) = obj {
                    let was_absent = set.insert(OrdValue(val), ()).is_none();
                    if let ExprKind::Identifier(name) = &object.kind {
                        self.env.set(name, Value::SortedSet(set));
                    }
                    return Some(Value::Bool(was_absent));
                }
                if let Value::Set(mut set) = obj {
                    let was_absent = !set.contains(&val);
                    if was_absent {
                        set.push(val);
                    }
                    if let ExprKind::Identifier(name) = &object.kind {
                        self.env.set(name, Value::Set(set));
                    }
                    return Some(Value::Bool(was_absent));
                }
            }
            "remove" => {
                let val = args
                    .first()
                    .map(|a| self.eval_expr_inner(&a.value))
                    .unwrap_or(Value::Unit);
                if let Value::Map(mut m) = obj {
                    let old = if let Some(pos) = m.iter().position(|(k, _)| *k == val) {
                        let (_, v) = m.remove(pos);
                        Value::EnumVariant {
                            enum_name: "Option".to_string(),
                            variant: "Some".to_string(),
                            data: EnumData::Tuple(vec![v]),
                        }
                    } else {
                        Value::EnumVariant {
                            enum_name: "Option".to_string(),
                            variant: "None".to_string(),
                            data: EnumData::Unit,
                        }
                    };
                    if let ExprKind::Identifier(name) = &object.kind {
                        self.env.set(name, Value::Map(m));
                    }
                    return Some(old);
                }
                if let Value::SortedSet(mut set) = obj {
                    let was_present = set.remove(&OrdValue(val)).is_some();
                    if let ExprKind::Identifier(name) = &object.kind {
                        self.env.set(name, Value::SortedSet(set));
                    }
                    return Some(Value::Bool(was_present));
                }
                if let Value::Set(mut set) = obj {
                    let was_present = if let Some(pos) = set.iter().position(|x| *x == val) {
                        set.swap_remove(pos);
                        true
                    } else {
                        false
                    };
                    if let ExprKind::Identifier(name) = &object.kind {
                        self.env.set(name, Value::Set(set));
                    }
                    return Some(Value::Bool(was_present));
                }
            }
            // ── Map.entry(k) and the Entry[K, V] method surface ────────────
            //
            // `entry(k)` returns a `Value::Entry` carrying the original Map's
            // binding name (so write-back can target the right slot via
            // `env.set`), the key, and the slot index when the key is
            // already present. The chain methods (`or_insert`,
            // `or_insert_with`, `and_modify`) dispatch on `Value::Entry` and
            // re-fetch the Map from the env each call so any mutation that
            // happened earlier in the chain (or in user code between calls)
            // is visible.
            //
            // The interpreter's `mut ref V` semantics on `or_insert*`'s
            // return are partial: `or_insert` returns the cloned slot value,
            // not a true alias into the map. The fully-aliased form
            // (`m.entry(k).or_insert_with(Vec.new).push(row)` mutating the
            // slot in place) is gated on Subtask 6 (codegen) where mut-ref-V
            // is realised as a raw slot pointer; the typechecker accepts the
            // chain shape regardless. Tests at the interpreter layer verify
            // map state after the chain runs, not the returned-slot ergonomics.
            "entry" => {
                if let Value::Map(ref m) = obj {
                    let key = args
                        .first()
                        .map(|a| self.eval_expr_inner(&a.value))
                        .unwrap_or(Value::Unit);
                    let slot_idx = m.iter().position(|(k, _)| *k == key);
                    let map_var = if let ExprKind::Identifier(name) = &object.kind {
                        Some(name.clone())
                    } else {
                        None
                    };
                    return Some(Value::Entry {
                        map_var,
                        key: Box::new(key),
                        slot_idx,
                    });
                }
            }
            "or_insert" => {
                if let Value::Entry {
                    map_var,
                    key,
                    slot_idx,
                } = obj
                {
                    let default = args
                        .first()
                        .map(|a| self.eval_expr_inner(&a.value))
                        .unwrap_or(Value::Unit);
                    return Some(self.entry_or_insert_value(map_var, *key, slot_idx, default));
                }
            }
            "or_insert_with" => {
                if let Value::Entry {
                    map_var,
                    key,
                    slot_idx,
                } = obj
                {
                    if slot_idx.is_some() {
                        // Occupied — closure not invoked. Pull the existing
                        // slot value out of the live Map (it may have been
                        // mutated by an earlier chain step).
                        if let Some(name) = map_var.as_deref() {
                            if let Some(Value::Map(m)) = self.env.get(name) {
                                if let Some(idx) = slot_idx {
                                    if let Some((_, v)) = m.get(idx) {
                                        return Some(v.clone());
                                    }
                                }
                            }
                        }
                        return Some(Value::Unit);
                    }
                    // Vacant — invoke the no-arg closure to produce the
                    // default value, then insert.
                    let f = args
                        .first()
                        .map(|a| self.eval_expr_inner(&a.value))
                        .unwrap_or(Value::Unit);
                    let default = self.invoke_function_value(f, vec![]);
                    return Some(self.entry_or_insert_value(map_var, *key, slot_idx, default));
                }
            }
            "and_modify" => {
                if let Value::Entry {
                    map_var,
                    key,
                    slot_idx,
                } = obj
                {
                    if let (Some(name), Some(idx)) = (map_var.as_deref(), slot_idx) {
                        // Occupied — invoke closure with a SharedCell aliased
                        // to the slot value so `|v| { v += 1 }` mutates
                        // through. Read the cell back and write the result
                        // into the Map slot.
                        let f = args
                            .first()
                            .map(|a| self.eval_expr_inner(&a.value))
                            .unwrap_or(Value::Unit);
                        if let Some(Value::Map(mut m)) = self.env.get(name) {
                            if let Some((_, slot_v)) = m.get(idx) {
                                let cell = Arc::new(Mutex::new(slot_v.clone()));
                                let _ = self.invoke_function_value(
                                    f,
                                    vec![Value::SharedCell(cell.clone())],
                                );
                                let new_v = cell.lock().unwrap().clone();
                                m[idx].1 = new_v;
                                self.env.set(name, Value::Map(m));
                            }
                        }
                    }
                    // Return self for chaining — vacant case is a no-op pass-
                    // through. slot_idx and key are unchanged in either case.
                    return Some(Value::Entry {
                        map_var,
                        key,
                        slot_idx,
                    });
                }
            }
            _ => return None,
        }
        None
    }
}
