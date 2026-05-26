//! Map / Entry / SortedSet / Set method-inference dispatch.
//!
//! Houses the per-method return-type synthesizers for the associative
//! container family: `Map[K,V]`, `Map.Entry[K,V]`, `SortedSet[T]`, and
//! `Set[T]`.

use crate::ast::*;
use crate::token::Span;

use super::types::{type_display, IntSize, Type};
use super::TypeErrorKind;

impl<'a> super::TypeChecker<'a> {
    /// Infer the return type of a method call on `Map[K, V]`.
    /// `key` is K, `val` is V from the receiver's type arguments.
    pub(super) fn infer_map_method(
        &mut self,
        key: &Type,
        val: &Type,
        method: &str,
        args: &[CallArg],
        span: &Span,
    ) -> Type {
        // K: Hash + Eq bound — Map requires the key type to be hashable and equality-comparable.
        if !self.type_supports_hash(key) || !self.type_supports_eq(key) {
            let missing = if !self.type_supports_hash(key) && !self.type_supports_eq(key) {
                "Hash + Eq"
            } else if !self.type_supports_hash(key) {
                "Hash"
            } else {
                "Eq"
            };
            self.type_error(
                format!(
                    "Map[{}, ...]: key type does not implement `{}`; \
                     only hashable equality-comparable types (integers, bool, char, String, \
                     or structs/enums with `#[derive(Hash, Eq)]`) can be Map keys",
                    type_display(key),
                    missing
                ),
                span.clone(),
                TypeErrorKind::TraitBoundNotSatisfied,
            );
        }
        let k = key.clone();
        let v = val.clone();
        let option_v = Type::Named {
            name: "Option".to_string(),
            args: vec![v.clone()],
        };
        let vec_k = Type::Named {
            name: "Vec".to_string(),
            args: vec![k.clone()],
        };
        let vec_v = Type::Named {
            name: "Vec".to_string(),
            args: vec![v.clone()],
        };
        let tuple_kv = Type::Tuple(vec![k.clone(), v.clone()]);
        let vec_kv = Type::Named {
            name: "Vec".to_string(),
            args: vec![tuple_kv],
        };
        let map_kv = Type::Named {
            name: "Map".to_string(),
            args: vec![k.clone(), v.clone()],
        };

        match method {
            "len" => {
                if !args.is_empty() {
                    self.type_error(
                        "Map.len() takes no arguments".to_string(),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                }
                Type::Int(IntSize::I64)
            }
            "is_empty" => {
                if !args.is_empty() {
                    self.type_error(
                        "Map.is_empty() takes no arguments".to_string(),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                }
                Type::Bool
            }
            "contains_key" => {
                for arg in args {
                    let at = self.infer_expr(&arg.value);
                    self.check_assignable(&k, &at, arg.value.span.clone());
                }
                Type::Bool
            }
            "get" => {
                for arg in args {
                    let at = self.infer_expr(&arg.value);
                    self.check_assignable(&k, &at, arg.value.span.clone());
                }
                option_v
            }
            "get_or" => {
                if let Some(key_arg) = args.first() {
                    let kt = self.infer_expr(&key_arg.value);
                    self.check_assignable(&k, &kt, key_arg.value.span.clone());
                }
                if let Some(default_arg) = args.get(1) {
                    let dt = self.infer_expr(&default_arg.value);
                    self.check_assignable(&v, &dt, default_arg.value.span.clone());
                }
                v
            }
            "insert" => {
                if let Some(key_arg) = args.first() {
                    let kt = self.infer_expr(&key_arg.value);
                    self.check_assignable(&k, &kt, key_arg.value.span.clone());
                }
                if let Some(val_arg) = args.get(1) {
                    let vt = self.infer_expr(&val_arg.value);
                    self.check_assignable(&v, &vt, val_arg.value.span.clone());
                }
                option_v
            }
            "remove" => {
                for arg in args {
                    let at = self.infer_expr(&arg.value);
                    self.check_assignable(&k, &at, arg.value.span.clone());
                }
                option_v
            }
            "keys" => {
                if !args.is_empty() {
                    self.type_error(
                        "Map.keys() takes no arguments".to_string(),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                }
                vec_k
            }
            "values" => {
                if !args.is_empty() {
                    self.type_error(
                        "Map.values() takes no arguments".to_string(),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                }
                vec_v
            }
            "entries" => {
                if !args.is_empty() {
                    self.type_error(
                        "Map.entries() takes no arguments".to_string(),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                }
                vec_kv
            }
            "merge" => {
                for arg in args {
                    let at = self.infer_expr(&arg.value);
                    self.check_assignable(&map_kv, &at, arg.value.span.clone());
                }
                map_kv
            }
            "clear" => {
                if !args.is_empty() {
                    self.type_error(
                        "Map.clear() takes no arguments".to_string(),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                }
                Type::Unit
            }
            "entry" => {
                // `entry(key: K) -> Entry[K, V]` — view returned for the given
                // key, occupied or vacant. Drives the in-place insert-or-modify
                // chain (or_insert / or_insert_with / and_modify) via
                // `infer_entry_method`. See design.md § Entry[K, V].
                if args.len() != 1 {
                    self.type_error(
                        format!("Map.entry() expects 1 argument, found {}", args.len()),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                    for arg in args {
                        self.infer_expr(&arg.value);
                    }
                } else {
                    let kt = self.infer_expr(&args[0].value);
                    self.check_assignable(&k, &kt, args[0].value.span.clone());
                }
                Type::Named {
                    name: "Entry".to_string(),
                    args: vec![k.clone(), v.clone()],
                }
            }
            _ => self.require_known_method(
                "Map",
                method,
                &[
                    "clear",
                    "contains_key",
                    "entries",
                    "entry",
                    "get",
                    "get_or",
                    "insert",
                    "is_empty",
                    "keys",
                    "len",
                    "merge",
                    "remove",
                    "values",
                ],
                args,
                span,
            ),
        }
    }

    /// Infer the return type of a method call on `Entry[K, V]`.
    /// Drives the chain produced by `Map.entry(k)` — `or_insert`,
    /// `or_insert_with`, `and_modify`. Effect polymorphism on the closure-
    /// taking forms is handled by the existing closure-effect-propagation
    /// pass in the effect checker; this layer just types the shape.
    pub(super) fn infer_entry_method(
        &mut self,
        key: &Type,
        val: &Type,
        method: &str,
        args: &[CallArg],
        span: &Span,
    ) -> Type {
        let v = val.clone();
        let mut_ref_v = Type::MutRef(Box::new(v.clone()));
        let entry_kv = Type::Named {
            name: "Entry".to_string(),
            args: vec![key.clone(), v.clone()],
        };
        match method {
            "or_insert" => {
                // `or_insert(default: V) -> mut ref V`. Returns a borrow into
                // the map's slot — fresh on Vacant (after writing default),
                // existing on Occupied. Uses `check_expr` (push-down) instead
                // of `infer_expr` (synth-only) so a nested
                // `Vec.new()` / `Vec.with_capacity(n)` / `Vec.filled(n, ..)`
                // default constructor sees the expected value type `V` and
                // can short-circuit on it. Without push-down, the bottom-up
                // `Vec.new()` returns `Vec[?T]`, which the subsequent
                // `check_assignable` can't unify against `Vec[V]` — surfaced
                // 2026-05-25 by kata 3629's
                // `bucket.entry(p).or_insert(Vec.new()).push(j)`.
                if args.len() != 1 {
                    self.type_error(
                        format!("Entry.or_insert() expects 1 argument, found {}", args.len()),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                    for arg in args {
                        self.infer_expr(&arg.value);
                    }
                } else {
                    self.check_expr(&args[0].value, &v);
                }
                mut_ref_v
            }
            "or_insert_with" => {
                // `or_insert_with[with E](f: Fn() -> V with E) -> mut ref V
                // with E`. Closure invoked only on the Vacant arm; effect
                // propagation through `with E` is handled by the effect
                // checker reading the closure's effect set.
                if args.len() != 1 {
                    self.type_error(
                        format!(
                            "Entry.or_insert_with() expects 1 argument, found {}",
                            args.len()
                        ),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                    for arg in args {
                        self.infer_expr(&arg.value);
                    }
                } else {
                    let f_ty = Type::Function {
                        params: vec![],
                        return_type: Box::new(v.clone()),
                    };
                    self.check_expr(&args[0].value, &f_ty);
                }
                mut_ref_v
            }
            "and_modify" => {
                // `and_modify[with E](f: Fn(mut ref V) with E) -> Entry[K, V]
                // with E`. Closure invoked only on Occupied; receives a
                // `mut ref V` to the existing slot. Returns self for
                // chaining (e.g. `.and_modify(...).or_insert(default)`).
                if args.len() != 1 {
                    self.type_error(
                        format!(
                            "Entry.and_modify() expects 1 argument, found {}",
                            args.len()
                        ),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                    for arg in args {
                        self.infer_expr(&arg.value);
                    }
                } else {
                    let f_ty = Type::Function {
                        params: vec![mut_ref_v.clone()],
                        return_type: Box::new(Type::Unit),
                    };
                    self.check_expr(&args[0].value, &f_ty);
                }
                entry_kv
            }
            _ => self.require_known_method(
                "Entry",
                method,
                &["and_modify", "or_insert", "or_insert_with"],
                args,
                span,
            ),
        }
    }

    /// Infer the return type of a method call on `SortedSet[T]`.
    /// `element` is the resolved `T` from the receiver's type arguments.
    /// Called from `infer_method_call` when the object type is
    /// `Type::Named { name: "SortedSet", ... }`.
    pub(super) fn infer_sorted_set_method(
        &mut self,
        element: &Type,
        method: &str,
        args: &[CallArg],
        span: &Span,
    ) -> Type {
        // T: Ord bound — SortedSet requires a total order on its element type.
        if !self.type_supports_ord(element) {
            self.type_error(
                format!(
                    "SortedSet[{}]: element type does not implement `Ord`; \
                     only types with a total order (integers, bool, char, String, \
                     or structs/enums with `#[derive(Ord)]`) can be SortedSet elements",
                    type_display(element)
                ),
                span.clone(),
                TypeErrorKind::TraitBoundNotSatisfied,
            );
        }
        let elem = element.clone();
        let option_elem = Type::Named {
            name: "Option".to_string(),
            args: vec![elem.clone()],
        };
        let sorted_set_elem = Type::Named {
            name: "SortedSet".to_string(),
            args: vec![elem.clone()],
        };

        match method {
            "len" => {
                if !args.is_empty() {
                    self.type_error(
                        "SortedSet.len() takes no arguments".to_string(),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                }
                Type::Int(IntSize::I64)
            }
            "is_empty" => {
                if !args.is_empty() {
                    self.type_error(
                        "SortedSet.is_empty() takes no arguments".to_string(),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                }
                Type::Bool
            }
            "contains" => {
                for arg in args {
                    let at = self.infer_expr(&arg.value);
                    self.check_assignable(&elem, &at, arg.value.span.clone());
                }
                Type::Bool
            }
            "insert" => {
                for arg in args {
                    let at = self.infer_expr(&arg.value);
                    self.check_assignable(&elem, &at, arg.value.span.clone());
                }
                Type::Bool
            }
            "remove" => {
                for arg in args {
                    let at = self.infer_expr(&arg.value);
                    self.check_assignable(&elem, &at, arg.value.span.clone());
                }
                Type::Bool
            }
            "min" | "max" => {
                if !args.is_empty() {
                    self.type_error(
                        format!("SortedSet.{}() takes no arguments", method),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                }
                option_elem
            }
            "union" | "intersection" | "difference" => {
                for arg in args {
                    let at = self.infer_expr(&arg.value);
                    self.check_assignable(&sorted_set_elem, &at, arg.value.span.clone());
                }
                sorted_set_elem
            }
            _ => self.require_known_method(
                "SortedSet",
                method,
                &[
                    "contains",
                    "difference",
                    "insert",
                    "intersection",
                    "is_empty",
                    "len",
                    "max",
                    "min",
                    "remove",
                    "union",
                ],
                args,
                span,
            ),
        }
    }

    /// Infer the return type of a method call on `Set[T: Hash + Eq]`.
    /// Hash set with O(1) average insert/remove/contains. Enforces the
    /// `T: Hash + Eq` bound the same way `Map[K, V]` checks `K: Hash + Eq`.
    pub(super) fn infer_set_method(
        &mut self,
        element: &Type,
        method: &str,
        args: &[CallArg],
        span: &Span,
    ) -> Type {
        // T: Hash + Eq bound
        if !self.type_supports_hash(element) || !self.type_supports_eq(element) {
            self.type_error(
                format!(
                    "Set[{}]: element type does not implement `Hash + Eq`; \
                     only types with a hash (integers, bool, char, String, \
                     or structs/enums with `#[derive(Hash, Eq)]`) can be Set elements",
                    type_display(element)
                ),
                span.clone(),
                TypeErrorKind::TraitBoundNotSatisfied,
            );
        }
        let elem = element.clone();
        let set_elem = Type::Named {
            name: "Set".to_string(),
            args: vec![elem.clone()],
        };

        match method {
            "len" => {
                if !args.is_empty() {
                    self.type_error(
                        "Set.len() takes no arguments".to_string(),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                }
                Type::Int(IntSize::I64)
            }
            "is_empty" => {
                if !args.is_empty() {
                    self.type_error(
                        "Set.is_empty() takes no arguments".to_string(),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                }
                Type::Bool
            }
            "contains" => {
                for arg in args {
                    let at = self.infer_expr(&arg.value);
                    self.check_assignable(&elem, &at, arg.value.span.clone());
                }
                Type::Bool
            }
            "insert" => {
                for arg in args {
                    let at = self.infer_expr(&arg.value);
                    self.check_assignable(&elem, &at, arg.value.span.clone());
                }
                Type::Bool
            }
            "remove" => {
                for arg in args {
                    let at = self.infer_expr(&arg.value);
                    self.check_assignable(&elem, &at, arg.value.span.clone());
                }
                Type::Bool
            }
            "union" | "intersection" | "difference" => {
                for arg in args {
                    let at = self.infer_expr(&arg.value);
                    self.check_assignable(&set_elem, &at, arg.value.span.clone());
                }
                set_elem
            }
            _ => self.require_known_method(
                "Set",
                method,
                &[
                    "contains",
                    "difference",
                    "insert",
                    "intersection",
                    "is_empty",
                    "len",
                    "remove",
                    "union",
                ],
                args,
                span,
            ),
        }
    }
}
