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
    /// Name the map an entry chain is rooted at, as a PLACE: a plain binding
    /// (`m`) or a dotted field path (`h.buckets`, `self.buckets`).
    ///
    /// B-2026-08-18-34. This used to accept an `ExprKind::Identifier` receiver
    /// only, so `map.entry(k).or_insert(d).push(v)` — the idiomatic append into
    /// a `Map[K, Vec[V]]` — was unusable whenever the map was a struct field,
    /// which is where a multimap normally lives. `Env::map_place_ref` /
    /// `map_place_mut` resolve what this returns.
    fn map_place_string(object: &Expr) -> Option<String> {
        match &object.kind {
            ExprKind::Identifier(name) => Some(name.clone()),
            ExprKind::SelfValue => Some("self".to_string()),
            ExprKind::FieldAccess {
                object: inner,
                field,
            } => Some(format!("{}.{}", Self::map_place_string(inner)?, field)),
            _ => None,
        }
    }

    /// Downgrade a strong handle stored into a `weak` CONTAINER slot
    /// (`Vec[weak T].push`, `Map[K, weak V].insert`, …).
    ///
    /// The interpreter has no static element/value type to consult, so the
    /// typechecker records each such store span in `weak_elem_store_sites` and
    /// this is the one place that consumes it. Storing the strong handle — what
    /// happened before B-2026-08-08-14 for `Vec` and before B-2026-08-08-29 for
    /// `Map` — makes a container-mediated cycle uncollectable here, the exact
    /// interpreter twin of the codegen leak.
    ///
    /// Anything that is not a `SharedStruct` passes through unchanged: the
    /// site set is span-keyed, and a non-shared value in a `weak` slot is
    /// already a typecheck error.
    pub(super) fn downgrade_weak_container_store(&self, arg: &CallArg, val: Value) -> Value {
        match &val {
            Value::SharedStruct(arc)
                if self
                    .typecheck_result
                    .weak_elem_store_sites
                    .contains(&crate::resolver::SpanKey::from_span(&arg.value.span)) =>
            {
                Value::WeakRef(Arc::downgrade(arc))
            }
            _ => val,
        }
    }

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
                if let Value::SortedMap(ref m) = obj {
                    let key = args
                        .first()
                        .map(|a| self.eval_expr_inner(&a.value))
                        .unwrap_or(Value::Unit);
                    let default = args
                        .get(1)
                        .map(|a| self.eval_expr_inner(&a.value))
                        .unwrap_or(Value::Unit);
                    return Some(match m.get(&OrdValue(key)) {
                        Some(v) => v.clone(),
                        None => default,
                    });
                }
            }
            "keys" => {
                if let Value::Map(ref m) = obj {
                    return Some(Value::array_of(m.iter().map(|(k, _)| k.clone()).collect()));
                }
                if let Value::SortedMap(ref m) = obj {
                    return Some(Value::array_of(m.keys().map(|k| k.0.clone()).collect()));
                }
            }
            "values" => {
                if let Value::Map(ref m) = obj {
                    return Some(Value::array_of(m.iter().map(|(_, v)| v.clone()).collect()));
                }
                if let Value::SortedMap(ref m) = obj {
                    return Some(Value::array_of(m.values().cloned().collect()));
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
                if let Value::SortedMap(ref m) = obj {
                    return Some(Value::array_of(
                        m.iter()
                            .map(|(k, v)| Value::Tuple(vec![k.0.clone(), v.clone()]))
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
                if let Value::SortedMap(ref base) = obj {
                    let other = args
                        .first()
                        .map(|a| self.eval_expr_inner(&a.value))
                        .unwrap_or_else(|| Value::SortedMap(std::collections::BTreeMap::new()));
                    if let Value::SortedMap(other_entries) = other {
                        // BTreeMap.insert overwrites — `other`'s value wins on a
                        // key collision, matching Map.merge's last-writer rule.
                        // `OrdValue` keys carry interior mutability via the
                        // value Arc; the BTree never re-hashes on it, so the
                        // mutable-key-type lint is a false positive (same
                        // suppression as SortedSet's set ops).
                        #[allow(clippy::mutable_key_type)]
                        let mut result = base.clone();
                        for (k, v) in other_entries {
                            result.insert(k, v);
                        }
                        return Some(Value::SortedMap(result));
                    }
                }
            }

            // ── SortedSet[T: Ord] methods ──────────────────────────────────
            "insert" => {
                let val = args
                    .first()
                    .map(|a| self.eval_expr_inner(&a.value))
                    .unwrap_or(Value::Unit);
                // Key arg: container walks only (no map walk covers keys —
                // the key source's own body firing once is today's
                // behavior). Value arg: the WHOLE value moves in, own body
                // included; the map's value walk runs it now (codegen twin:
                // `disarm_moved_value_arg_user_drops`).
                if let Some(arg) = args.first() {
                    if let ExprKind::Identifier(n) = &arg.value.kind {
                        let n = n.clone();
                        self.record_container_move_source_name(&n);
                    }
                }
                if args.len() > 1 {
                    self.record_ctor_arg_moves(&args[1..2]);
                    self.record_container_move_sources_in_aggregate_arg(&args[1].value);
                }
                // B-2026-08-02-20 (leg 2) — an aggregate-literal KEY/element
                // arm too (`s.insert(Holder { xs: xs })`): the Set element
                // half owns the sources named in the literal.
                if let Some(arg) = args.first() {
                    let key_expr = arg.value.clone();
                    self.record_container_move_sources_in_aggregate_arg(&key_expr);
                }
                if let Value::Map(mut m) = obj {
                    // Map.insert(key, value) -> Option[V] (old value)
                    let value = args
                        .get(1)
                        .map(|a| {
                            let v = self.eval_expr_inner(&a.value);
                            // B-2026-08-08-29 — a `Map[K, weak V]` insert
                            // DOWNGRADES, the twin of the `Vec[weak T]` push.
                            self.downgrade_weak_container_store(a, v)
                        })
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
                    self.write_back_receiver(object, Value::Map(m));
                    return Some(old);
                }
                if let Value::SortedMap(mut m) = obj {
                    // SortedMap.insert(key, value) -> Option[V] (old value),
                    // mirroring Map.insert. `val` is the already-evaluated key.
                    let value = args
                        .get(1)
                        .map(|a| {
                            let v = self.eval_expr_inner(&a.value);
                            self.downgrade_weak_container_store(a, v)
                        })
                        .unwrap_or(Value::Unit);
                    let old = match m.insert(OrdValue(val), value) {
                        Some(prev) => Value::EnumVariant {
                            enum_name: "Option".to_string(),
                            variant: "Some".to_string(),
                            data: EnumData::Tuple(vec![prev]),
                        },
                        None => Value::EnumVariant {
                            enum_name: "Option".to_string(),
                            variant: "None".to_string(),
                            data: EnumData::Unit,
                        },
                    };
                    self.write_back_receiver(object, Value::SortedMap(m));
                    return Some(old);
                }
                if let Value::SortedSet(mut set) = obj {
                    let was_absent = set.insert(OrdValue(val), ()).is_none();
                    self.write_back_receiver(object, Value::SortedSet(set));
                    return Some(Value::Bool(was_absent));
                }
                if let Value::Set(mut set) = obj {
                    let was_absent = !set.contains(&val);
                    if was_absent {
                        set.push(val);
                    }
                    self.write_back_receiver(object, Value::Set(set));
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
                    self.write_back_receiver(object, Value::Map(m));
                    return Some(old);
                }
                if let Value::SortedMap(mut m) = obj {
                    // SortedMap.remove(key) -> Option[V] (old value), mirroring Map.remove.
                    let old = match m.remove(&OrdValue(val)) {
                        Some(prev) => Value::EnumVariant {
                            enum_name: "Option".to_string(),
                            variant: "Some".to_string(),
                            data: EnumData::Tuple(vec![prev]),
                        },
                        None => Value::EnumVariant {
                            enum_name: "Option".to_string(),
                            variant: "None".to_string(),
                            data: EnumData::Unit,
                        },
                    };
                    self.write_back_receiver(object, Value::SortedMap(m));
                    return Some(old);
                }
                if let Value::SortedSet(mut set) = obj {
                    let was_present = set.remove(&OrdValue(val)).is_some();
                    self.write_back_receiver(object, Value::SortedSet(set));
                    return Some(Value::Bool(was_present));
                }
                if let Value::Set(mut set) = obj {
                    let was_present = if let Some(pos) = set.iter().position(|x| *x == val) {
                        set.swap_remove(pos);
                        true
                    } else {
                        false
                    };
                    self.write_back_receiver(object, Value::Set(set));
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
                    let map_var = Self::map_place_string(object);
                    return Some(Value::Entry {
                        map_var,
                        key: Box::new(key),
                        slot_idx,
                    });
                }
                if let Value::SortedMap(_) = obj {
                    // SortedMap shares Map's entry semantics; its BTreeMap storage
                    // has no positional slot, so `slot_idx` stays `None` and the
                    // chain steps below resolve the slot by KEY (mirrors codegen,
                    // which reuses Map's KaracMap-backed entry chain).
                    let key = args
                        .first()
                        .map(|a| self.eval_expr_inner(&a.value))
                        .unwrap_or(Value::Unit);
                    let map_var = Self::map_place_string(object);
                    return Some(Value::Entry {
                        map_var,
                        key: Box::new(key),
                        slot_idx: None,
                    });
                }
            }
            "or_insert" => {
                if let Value::Entry { map_var, key, .. } = obj {
                    let default = args
                        .first()
                        .map(|a| self.eval_expr_inner(&a.value))
                        .unwrap_or(Value::Unit);
                    // Insert-if-absent, then hand back a `mut ref V` (MapSlotRef)
                    // into the live slot so `*r += 1` / `.push(x)` write through.
                    return Some(self.entry_or_insert_ref(map_var, *key, default));
                }
            }
            "or_insert_with" => {
                if let Value::Entry { map_var, key, .. } = obj {
                    // Occupancy is read from the live map by key — `slot_idx`
                    // may be stale after an earlier chain step mutated the map.
                    let occupied = match map_var.as_deref().and_then(|n| self.env.map_place_ref(n))
                    {
                        Some(Value::Map(pairs)) => pairs.iter().any(|(k, _)| *k == *key),
                        Some(Value::SortedMap(m)) => m.contains_key(&OrdValue((*key).clone())),
                        _ => false,
                    };
                    if occupied {
                        // Occupied — the closure is NOT invoked; return a ref to
                        // the existing slot.
                        return Some(match map_var {
                            Some(name) => Value::MapSlotRef { map_var: name, key },
                            None => Value::Unit,
                        });
                    }
                    // Vacant — invoke the no-arg closure to produce the default
                    // value, insert it, and return a ref to the new slot.
                    let f = args
                        .first()
                        .map(|a| self.eval_expr_inner(&a.value))
                        .unwrap_or(Value::Unit);
                    let default = self.invoke_function_value(f, vec![]);
                    return Some(self.entry_or_insert_ref(map_var, *key, default));
                }
            }
            "and_modify" => {
                if let Value::Entry {
                    map_var,
                    key,
                    slot_idx,
                } = obj
                {
                    if let Some(name) = map_var.as_deref() {
                        // Occupied — invoke closure with a SharedCell aliased
                        // to the slot value so `|v| { v += 1 }` mutates
                        // through. Read the cell back and write the result
                        // into the slot. Map resolves the slot by `slot_idx`;
                        // SortedMap (no positional slot) resolves it by KEY.
                        let f = args
                            .first()
                            .map(|a| self.eval_expr_inner(&a.value))
                            .unwrap_or(Value::Unit);
                        // Resolved as a PLACE, so a map in a struct field
                        // modifies the real map (B-2026-08-18-34). Keyed on the
                        // binding NAME, `h.counts.entry(k).and_modify(f)` found
                        // nothing and silently skipped the closure — the map was
                        // left at its `or_insert` default while codegen applied
                        // the modification, a run-vs-build divergence.
                        match self.env.map_place_ref(name).cloned() {
                            Some(Value::Map(mut m)) => {
                                if let Some(idx) = slot_idx {
                                    if let Some((_, slot_v)) = m.get(idx) {
                                        let cell = Arc::new(Mutex::new(slot_v.clone()));
                                        let _ = self.invoke_function_value(
                                            f,
                                            vec![Value::SharedCell(cell.clone())],
                                        );
                                        let new_v = cell.lock().unwrap().clone();
                                        m[idx].1 = new_v;
                                        if let Some(slot) = self.env.map_place_mut(name) {
                                            *slot = Value::Map(m);
                                        }
                                    }
                                }
                            }
                            Some(Value::SortedMap(mut m)) => {
                                let ok = OrdValue((*key).clone());
                                if let Some(slot_v) = m.get(&ok) {
                                    let cell = Arc::new(Mutex::new(slot_v.clone()));
                                    let _ = self.invoke_function_value(
                                        f,
                                        vec![Value::SharedCell(cell.clone())],
                                    );
                                    let new_v = cell.lock().unwrap().clone();
                                    m.insert(ok, new_v);
                                    if let Some(slot) = self.env.map_place_mut(name) {
                                        *slot = Value::SortedMap(m);
                                    }
                                }
                            }
                            _ => {}
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
