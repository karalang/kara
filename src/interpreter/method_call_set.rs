//! Set / SortedSet / SortedMap method dispatch — the bodies of the
//! `clear`/`min`/`max`/`union`/`intersection`/`difference` arms (Set/SortedSet)
//! and the `clear`/`min`/`max`/`range`/`floor`/`ceiling` arms (SortedMap, B3)
//! lifted out of `eval_method_call`. Receivers are `Value::Set` /
//! `Value::SortedSet` / `Value::SortedMap` / `Value::Map`.

use std::collections::BTreeMap;

use crate::ast::*;
use crate::token::Span;

use super::value::{EnumData, OrdValue, Value};

/// Wrap an optional payload in the `Option` enum `Value` — `Some(v)` when
/// present, `None` otherwise. Shared by the SortedSet / SortedMap ordered
/// queries (`min` / `max` / `floor` / `ceiling`) that each return `Option[…]`.
fn option_of(payload: Option<Value>) -> Value {
    match payload {
        Some(v) => Value::EnumVariant {
            enum_name: "Option".to_string(),
            variant: "Some".to_string(),
            data: EnumData::Tuple(vec![v]),
        },
        None => Value::EnumVariant {
            enum_name: "Option".to_string(),
            variant: "None".to_string(),
            data: EnumData::Unit,
        },
    }
}

impl<'a> super::Interpreter<'a> {
    pub(super) fn try_eval_set_method(
        &mut self,
        method: &str,
        object: &Expr,
        obj: &Value,
        args: &[CallArg],
        _span: &Span,
    ) -> Option<Value> {
        match method {
            // B-2026-08-03-2 (class 1) — every entry is destroyed here, so
            // every VALUE's user Drop BODY must run. The arm replaced the
            // receiver with an empty table and never touched the old values,
            // so a Drop-bearing value type lost its destructors silently.
            // Values are snapshotted, the table is emptied, then the bodies
            // fire — no borrow of the receiver is live while a body runs.
            // Codegen twin: the map value-bodies walker now called ahead of
            // the memory work in `Map.clear`.
            "clear" => {
                if let Value::Map(ref entries) = obj {
                    // B-2026-08-26-41 — the KEYS are destroyed here too, so
                    // their bodies are owed exactly as the values' are. Keys
                    // first, matching the binding-death order. Codegen twin:
                    // the `emit_map_key_user_drop_bodies_fn` call in the
                    // `clear` arm of `maps.rs`.
                    let (removed_k, removed_v): (Vec<Value>, Vec<Value>) = {
                        let g = entries.read().unwrap();
                        (
                            g.iter().map(|(k, _)| k.clone()).collect(),
                            g.iter().map(|(_, v)| v.clone()).collect(),
                        )
                    };
                    // Shared storage: clear THIS map rather than rebinding the
                    // name to a fresh one, so a map reached through a field or
                    // an alias is cleared too.
                    entries.write().unwrap().clear();
                    for k in removed_k {
                        self.run_discarded_value_user_drops(k);
                    }
                    for v in removed_v {
                        self.run_discarded_value_user_drops(v);
                    }
                    return Some(Value::Unit);
                }
                if let Value::SortedMap(ref entries) = obj {
                    let removed_k: Vec<Value> = entries.keys().map(|k| k.0.clone()).collect();
                    let removed_v: Vec<Value> = entries.values().cloned().collect();
                    self.write_back_receiver(object, Value::SortedMap(BTreeMap::new()));
                    for k in removed_k {
                        self.run_discarded_value_user_drops(k);
                    }
                    for v in removed_v {
                        self.run_discarded_value_user_drops(v);
                    }
                    return Some(Value::Unit);
                }
                // B-2026-08-12-8 — the SET arms were missing entirely, so
                // `Set.clear()` died with "no interpreter dispatch arm" even
                // though codegen implements it. Registering the method in the
                // typechecker (which is what that row is about) would
                // otherwise have turned a check-time rejection into a
                // run-vs-build split: compiled fine, `karac run --interp`
                // dead at runtime.
                //
                // Same element-drop discipline as the Map arms above
                // (B-2026-08-03-2): every element is destroyed here, so a
                // Drop-bearing element type must run its body. Snapshot, empty
                // the receiver, then fire the bodies, so no borrow of the
                // receiver is live while user code runs.
                //
                // `SortedSet.clear()` is NOT added: the typechecker does not
                // register it either, and codegen's SortedSet dispatch was
                // not confirmed to implement it, so adding an interpreter arm
                // alone would be unreachable code and registering it in the
                // typechecker without checking codegen would trade this gap
                // for a checks-clean-then-fails-at-build one. Noted as an
                // adjacent gap on B-2026-08-12-8 instead.
                if let Value::Set(ref elems) = obj {
                    let removed: Vec<Value> = elems.read().unwrap().items().to_vec();
                    // Clear the shared storage, so an aliased or field-reached
                    // set is cleared too rather than rebound.
                    elems.write().unwrap().clear();
                    for v in removed {
                        self.run_discarded_value_user_drops(v);
                    }
                    return Some(Value::Unit);
                }
            }
            "min" => {
                if let Value::SortedSet(ref set) = obj {
                    return Some(option_of(set.keys().next().map(|k| k.0.clone())));
                }
                if let Value::SortedMap(ref map) = obj {
                    // SortedMap.min() -> Option[(K, V)] — first entry in key order.
                    return Some(option_of(
                        map.iter()
                            .next()
                            .map(|(k, v)| Value::Tuple(vec![k.0.clone(), v.clone()])),
                    ));
                }
            }
            "max" => {
                if let Value::SortedSet(ref set) = obj {
                    return Some(option_of(set.keys().next_back().map(|k| k.0.clone())));
                }
                if let Value::SortedMap(ref map) = obj {
                    // SortedMap.max() -> Option[(K, V)] — last entry in key order.
                    return Some(option_of(
                        map.iter()
                            .next_back()
                            .map(|(k, v)| Value::Tuple(vec![k.0.clone(), v.clone()])),
                    ));
                }
            }
            // SortedMap.range(lo, hi) -> Vec[(K, V)] — entries whose key lies in
            // the INCLUSIVE interval [lo, hi], in ascending key order. An empty
            // or inverted interval yields the empty vec.
            "range" => {
                if let Value::SortedMap(ref map) = obj {
                    let lo = args
                        .first()
                        .map(|a| self.eval_expr_inner(&a.value))
                        .unwrap_or(Value::Unit);
                    let hi = args
                        .get(1)
                        .map(|a| self.eval_expr_inner(&a.value))
                        .unwrap_or(Value::Unit);
                    let (lo, hi) = (OrdValue(lo), OrdValue(hi));
                    let items: Vec<Value> = if lo > hi {
                        Vec::new()
                    } else {
                        map.range(lo..=hi)
                            .map(|(k, v)| Value::Tuple(vec![k.0.clone(), v.clone()]))
                            .collect()
                    };
                    return Some(Value::array_of(items));
                }
            }
            // SortedMap.floor(k) -> Option[(K, V)] — entry with the largest key
            // <= k (the key itself when present). None if every key exceeds k.
            "floor" => {
                if let Value::SortedMap(ref map) = obj {
                    let key = args
                        .first()
                        .map(|a| self.eval_expr_inner(&a.value))
                        .unwrap_or(Value::Unit);
                    return Some(option_of(
                        map.range(..=OrdValue(key))
                            .next_back()
                            .map(|(k, v)| Value::Tuple(vec![k.0.clone(), v.clone()])),
                    ));
                }
            }
            // SortedMap.ceiling(k) -> Option[(K, V)] — entry with the smallest
            // key >= k (the key itself when present). None if every key is below k.
            "ceiling" => {
                if let Value::SortedMap(ref map) = obj {
                    let key = args
                        .first()
                        .map(|a| self.eval_expr_inner(&a.value))
                        .unwrap_or(Value::Unit);
                    return Some(option_of(
                        map.range(OrdValue(key)..)
                            .next()
                            .map(|(k, v)| Value::Tuple(vec![k.0.clone(), v.clone()])),
                    ));
                }
            }
            "union" => {
                let other = args
                    .first()
                    .map(|a| self.eval_expr_inner(&a.value))
                    .unwrap_or(Value::Unit);
                if let (Value::SortedSet(ref a_set), Value::SortedSet(ref b_set)) = (obj, &other) {
                    #[allow(clippy::mutable_key_type)]
                    let mut result = a_set.clone();
                    for k in b_set.keys() {
                        result.insert(k.clone(), ());
                    }
                    return Some(Value::SortedSet(result));
                }
                if let (Value::Set(ref a_set), Value::Set(ref b_set)) = (obj, &other) {
                    let mut result = a_set.read().unwrap().clone();
                    for v in b_set.read().unwrap().iter() {
                        result.insert(v.clone());
                    }
                    return Some(Value::Set(std::sync::Arc::new(std::sync::RwLock::new(
                        result,
                    ))));
                }
            }
            "intersection" => {
                let other = args
                    .first()
                    .map(|a| self.eval_expr_inner(&a.value))
                    .unwrap_or(Value::Unit);
                if let (Value::SortedSet(ref a_set), Value::SortedSet(ref b_set)) = (obj, &other) {
                    #[allow(clippy::mutable_key_type)]
                    let result: BTreeMap<OrdValue, ()> = a_set
                        .iter()
                        .filter(|(k, _)| b_set.contains_key(*k))
                        .map(|(k, v)| (k.clone(), *v))
                        .collect();
                    return Some(Value::SortedSet(result));
                }
                if let (Value::Set(ref a_set), Value::Set(ref b_set)) = (obj, &other) {
                    let b_set = b_set.read().unwrap();
                    let result: Vec<Value> = a_set
                        .read()
                        .unwrap()
                        .iter()
                        .filter(|v| b_set.contains(v))
                        .cloned()
                        .collect();
                    return Some(Value::set_of(result));
                }
            }
            "difference" => {
                let other = args
                    .first()
                    .map(|a| self.eval_expr_inner(&a.value))
                    .unwrap_or(Value::Unit);
                if let (Value::SortedSet(ref a_set), Value::SortedSet(ref b_set)) = (obj, &other) {
                    #[allow(clippy::mutable_key_type)]
                    let result: BTreeMap<OrdValue, ()> = a_set
                        .iter()
                        .filter(|(k, _)| !b_set.contains_key(*k))
                        .map(|(k, v)| (k.clone(), *v))
                        .collect();
                    return Some(Value::SortedSet(result));
                }
                if let (Value::Set(ref a_set), Value::Set(ref b_set)) = (obj, &other) {
                    let b_set = b_set.read().unwrap();
                    let result: Vec<Value> = a_set
                        .read()
                        .unwrap()
                        .iter()
                        .filter(|v| !b_set.contains(v))
                        .cloned()
                        .collect();
                    return Some(Value::set_of(result));
                }
            }
            _ => return None,
        }
        None
    }
}
