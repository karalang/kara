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
            "clear" => {
                if let Value::Map(_) = obj {
                    if let ExprKind::Identifier(name) = &object.kind {
                        self.env.set(name, Value::Map(Vec::new()));
                    }
                    return Some(Value::Unit);
                }
                if let Value::SortedMap(_) = obj {
                    if let ExprKind::Identifier(name) = &object.kind {
                        self.env.set(name, Value::SortedMap(BTreeMap::new()));
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
                    for (k, _v) in b_set.iter() {
                        result.insert(k.clone(), ());
                    }
                    return Some(Value::SortedSet(result));
                }
                if let (Value::Set(ref a_set), Value::Set(ref b_set)) = (obj, &other) {
                    let mut result = a_set.clone();
                    for v in b_set {
                        if !result.contains(v) {
                            result.push(v.clone());
                        }
                    }
                    return Some(Value::Set(result));
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
                    let result: Vec<Value> = a_set
                        .iter()
                        .filter(|v| b_set.contains(v))
                        .cloned()
                        .collect();
                    return Some(Value::Set(result));
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
                    let result: Vec<Value> = a_set
                        .iter()
                        .filter(|v| !b_set.contains(v))
                        .cloned()
                        .collect();
                    return Some(Value::Set(result));
                }
            }
            _ => return None,
        }
        None
    }
}
