//! Map / Entry / SortedSet / Set method-inference dispatch.
//!
//! Houses the per-method return-type synthesizers for the associative
//! container family: `Map[K,V]`, `Map.Entry[K,V]`, `SortedSet[T]`, and
//! `Set[T]`.

use crate::ast::*;
use crate::token::Span;

use super::inference::{resolve_type_var_top, unify_types};
use super::types::{type_display, IntSize, Type};
use super::TypeErrorKind;

impl<'a> super::TypeChecker<'a> {
    /// Check a `Map`/`SortedMap` key or value argument against the
    /// receiver's slot type, back-propagating the argument type into any
    /// unsolved slot typevar *before* the assignability check.
    ///
    /// `Map.new()` mints the receiver as `Map[?K, ?V]` (two fresh
    /// typevars). Without the `unify_types` step a subsequent `.insert(k,
    /// v)` / `.get(k)` would only `check_assignable(?K, ConcreteK)`, which
    /// reports a spurious `expected '?K', found 'ConcreteK'` mismatch and
    /// leaves the binding's K/V unresolved — so the inferred `Map[?K, ?V]`
    /// later clashes with an annotated `Map[K, V]` field/param at the use
    /// site. Pinning K/V here mirrors how `Vec.new()` + `.push()` pins the
    /// element type (`expr_method_call.rs`). Resolving `slot` after
    /// unification keeps the assignability check from comparing the
    /// now-stale typevar against the (just-pinned) argument type.
    /// Key-slot check for the LOOKUP methods — `get`, `get_or`, `contains_key`,
    /// `remove`. These only COMPARE the key against what the map already holds;
    /// they never store it. That is not a guess: passing an OWNED key to
    /// `m.get(k)` leaves `k` live afterwards today, so the borrow is already
    /// the runtime semantics and only the type check demanded ownership.
    ///
    /// So a `ref K` is a legitimate lookup key, and rejecting it forced a
    /// pointless clone at every call site holding a borrow — `examples/db_pipeline`
    /// has five, all of the shape `fn f(table: ref String)` then
    /// `self.tables.get(table)`, which could not be written at all.
    ///
    /// `insert` and `entry` deliberately keep [`Self::check_map_slot_arg`]:
    /// those STORE the key, so they genuinely need ownership.
    fn check_map_lookup_key_arg(&mut self, slot: &Type, arg: &CallArg) {
        self.check_map_slot_arg_inner(slot, arg, true);
    }

    fn check_map_slot_arg(&mut self, slot: &Type, arg: &CallArg) {
        self.check_map_slot_arg_inner(slot, arg, false);
    }

    /// B-2026-08-08-29 — value-slot check for a `weak V` associative container
    /// (`Map[K, weak V]` / `SortedMap[K, weak V]`). The `Map`-side twin of the
    /// `Vec[weak T].push` gate in `expr_method_call.rs`, and deliberately the
    /// same shape:
    ///
    ///  * it RECORDS the store in `weak_elem_store_sites`, keyed by the value
    ///    argument's span. The interpreter has no static value type to consult,
    ///    so without the record it stores a STRONG handle and a Map-mediated
    ///    cycle stays uncollectable there (B-2026-08-08-14 hit the same wall on
    ///    the `Vec` side);
    ///  * it accepts only a BARE strong handle. The generic weak-FIELD
    ///    coercion in `check_expr` also admits `Option[T]` and `None`, and the
    ///    container store lowers neither — before this gate,
    ///    `m.insert(k, Option.Some(a))` typechecked and stored the Option
    ///    aggregate's first word in a slot the scope-exit drain hands to
    ///    `karac_weak_drop`. Widening this is a codegen change first and a
    ///    typechecker change second.
    fn check_weak_container_slot_arg(&mut self, referent: &Type, arg: &CallArg, recv: &str) {
        self.weak_elem_store_sites
            .insert(crate::resolver::SpanKey::from_span(&arg.value.span));
        let actual = self.infer_expr(&arg.value);
        let ok = matches!(actual, Type::Error | Type::Never)
            || super::types::types_compatible(&actual, referent);
        if !ok {
            self.type_error(
                format!(
                    "cannot insert a value of type '{}' into a `{}[_, weak {}]`; \
                     a container value slot takes a BARE `{}` handle, which is \
                     downgraded on the way in. The `Option[{}]` / `None` forms a \
                     `weak` FIELD accepts are not lowered for a container value \
                     yet — bind the handle first and insert that",
                    type_display(&actual),
                    recv,
                    type_display(referent),
                    type_display(referent),
                    type_display(referent),
                ),
                arg.value.span,
                TypeErrorKind::TypeMismatch,
            );
            self.record_expr_type(&arg.value.span, &Type::Error);
        }
    }

    /// `allow_borrowed` peels ONE `ref` / `mut ref` level off the argument
    /// before unification. With it `false` this is byte-identical to the
    /// original helper, so every value slot behaves exactly as before.
    fn check_map_slot_arg_inner(&mut self, slot: &Type, arg: &CallArg, allow_borrowed: bool) {
        // Two directions, chosen by whether the slot is already concrete:
        //  * CONCRETE slot (`Map[K, Vec[i64]]` → slot `Vec[i64]`): push it as
        //    the EXPECTED type via `check_expr` so a type-inferred constructor
        //    argument resolves its element against the slot — `m.get_or(k,
        //    Vec.new())` / `m.insert(k, Vec.new())` pins `Vec[i64]` instead of
        //    leaving `Vec[?T]` and erroring (B-2026-07-18-17).
        //  * UNRESOLVED slot (`Map.new()` → bare `?V`, or a generic-body `V`):
        //    `check_expr` against a bare typevar/typeparam would `check_assignable`
        //    it and spuriously report `expected '?V', found '…'`, so infer the
        //    arg plainly and let the `unify_types` back-propagation below pin the
        //    slot from the arg (this helper's original job).
        let pre_slot = resolve_type_var_top(slot, &self.env.substitutions);
        // A borrowed key cannot be pushed through `check_expr` against the
        // concrete slot — that is precisely the check that rejects `ref String`
        // against `String` — so infer it plainly and peel below.
        let arg_ty = if allow_borrowed || matches!(pre_slot, Type::TypeVar(_) | Type::TypeParam(_))
        {
            self.infer_expr(&arg.value)
        } else {
            self.check_expr(&arg.value, &pre_slot)
        };
        let arg_ty = if allow_borrowed {
            match &arg_ty {
                Type::Ref(inner) | Type::MutRef(inner) => (**inner).clone(),
                _ => arg_ty,
            }
        } else {
            arg_ty
        };
        unify_types(
            slot,
            &arg_ty,
            &mut self.env.substitutions,
            &mut self.env.const_substitutions,
        );
        let resolved_slot = resolve_type_var_top(slot, &self.env.substitutions);
        self.check_assignable(&resolved_slot, &arg_ty, arg.value.span);
    }

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
                *span,
                TypeErrorKind::TraitBoundNotSatisfied,
            );
        }
        let k = key.clone();
        let v = val.clone();
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
                        *span,
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                }
                Type::Int(IntSize::I64)
            }
            "is_empty" => {
                if !args.is_empty() {
                    self.type_error(
                        "Map.is_empty() takes no arguments".to_string(),
                        *span,
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                }
                Type::Bool
            }
            "contains_key" => {
                for arg in args {
                    self.check_map_lookup_key_arg(&k, arg);
                }
                Type::Bool
            }
            "get" => {
                for arg in args {
                    self.check_map_lookup_key_arg(&k, arg);
                }
                // Re-read V through the substitutions: a sibling `insert`
                // may have pinned it, and even a lone `get(k)` on a fresh
                // `Map.new()` should surface the resolved value type.
                let resolved_v = resolve_type_var_top(&v, &self.env.substitutions);
                // B-2026-08-09-2 — a `weak V` VALUE read is an UPGRADE, the twin
                // of the `Vec[weak T]` element read (`exprs.rs`, the
                // `!index_is_lhs` arm) and of the `weak` FIELD read. Without it
                // the `Some` payload was a bare `weak V`, every field access
                // through the binding was rejected ("no field 'v' on type 'this
                // type'"), and the container was store-only: fine for
                // cycle-breaking back-edges, which exist not to be traversed,
                // useless for a parent-pointer walk.
                //
                // ONE Option, not `Option[Option[V]]`. A missing key and a dead
                // referent both yield `None`, and collapsing them is the honest
                // shape rather than a shortcut: a referent that has been
                // released IS a key whose value is gone, and no caller can act
                // on the difference — the entry it would ask about no longer
                // refers to anything.
                let payload = match &resolved_v {
                    Type::Weak(referent) => (**referent).clone(),
                    _ => resolved_v,
                };
                Type::Named {
                    name: "Option".to_string(),
                    args: vec![payload],
                }
            }
            "get_or" => {
                if let Some(key_arg) = args.first() {
                    self.check_map_lookup_key_arg(&k, key_arg);
                }
                if let Some(default_arg) = args.get(1) {
                    self.check_map_slot_arg(&v, default_arg);
                }
                resolve_type_var_top(&v, &self.env.substitutions)
            }
            "insert" => {
                if let Some(key_arg) = args.first() {
                    self.check_map_slot_arg(&k, key_arg);
                }
                if let Some(val_arg) = args.get(1) {
                    // B-2026-08-08-29 — a `weak V` VALUE slot takes the
                    // container gate, not the generic weak-FIELD coercion in
                    // `check_expr`. Two reasons, both measured:
                    //
                    //  * the store site has to be recorded for the interpreter,
                    //    which has no static value type of its own to consult
                    //    (the `Vec[weak T]` twin, B-2026-08-08-14);
                    //  * the field coercion also admits `Option[T]` and `None`,
                    //    and the container store lowers only a BARE strong
                    //    handle. `m.insert(k, Option.Some(a))` compiled and put
                    //    a mangled word in the bucket -- a word the scope-exit
                    //    weak drain would then hand to `karac_weak_drop`.
                    //    Exactly the narrowing `Vec[weak T].push` already
                    //    applies, for exactly the same reason: the typechecker
                    //    must not admit what codegen does not implement.
                    let slot = resolve_type_var_top(&v, &self.env.substitutions);
                    if let Type::Weak(referent) = &slot {
                        self.check_weak_container_slot_arg(referent, val_arg, "Map");
                    } else {
                        self.check_map_slot_arg(&v, val_arg);
                    }
                }
                let resolved_v = resolve_type_var_top(&v, &self.env.substitutions);
                Type::Named {
                    name: "Option".to_string(),
                    args: vec![resolved_v],
                }
            }
            "remove" => {
                for arg in args {
                    self.check_map_lookup_key_arg(&k, arg);
                }
                let resolved_v = resolve_type_var_top(&v, &self.env.substitutions);
                Type::Named {
                    name: "Option".to_string(),
                    args: vec![resolved_v],
                }
            }
            "keys" => {
                if !args.is_empty() {
                    self.type_error(
                        "Map.keys() takes no arguments".to_string(),
                        *span,
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                }
                vec_k
            }
            "values" => {
                if !args.is_empty() {
                    self.type_error(
                        "Map.values() takes no arguments".to_string(),
                        *span,
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                }
                vec_v
            }
            "entries" => {
                if !args.is_empty() {
                    self.type_error(
                        "Map.entries() takes no arguments".to_string(),
                        *span,
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                }
                vec_kv
            }
            "merge" => {
                for arg in args {
                    let at = self.infer_expr(&arg.value);
                    self.check_assignable(&map_kv, &at, arg.value.span);
                }
                map_kv
            }
            "clear" => {
                if !args.is_empty() {
                    self.type_error(
                        "Map.clear() takes no arguments".to_string(),
                        *span,
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
                        *span,
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                    for arg in args {
                        self.infer_expr(&arg.value);
                    }
                } else {
                    let kt = self.infer_expr(&args[0].value);
                    self.check_assignable(&k, &kt, args[0].value.span);
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
                        *span,
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
                        *span,
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
                        *span,
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
                *span,
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
                        *span,
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                }
                Type::Int(IntSize::I64)
            }
            "is_empty" => {
                if !args.is_empty() {
                    self.type_error(
                        "SortedSet.is_empty() takes no arguments".to_string(),
                        *span,
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                }
                Type::Bool
            }
            // B-2026-08-14-1 — `check_assignable` alone admits an implicit
            // NARROWING into the element slot, which the language refuses
            // everywhere else it is checked. These infer-then-compare arms
            // never reached `check_int_widening_coercion`, so `Set[u8]` took a
            // `300i64` with no diagnostic; the compiled build then truncated to
            // 44 while the interpreter stored 300, leaving a `Set[u8]` holding
            // a value its element type cannot represent. The `Map` sibling
            // routes its slot args through `check_expr`, which is why
            // `Map.insert` rejected the same shape all along. Applied to
            // `SortedSet` too — same arms, same hole, and the sweep only
            // probed the hashed one.
            // `insert` STORES its argument, so its spec really is `val: T` and
            // an owned value is required. `contains` and `remove` are PROBES —
            // design.md gives both `val: ref T` — so they are split out below.
            "insert" => {
                for arg in args {
                    let at = self.infer_expr(&arg.value);
                    self.check_int_widening_coercion(&arg.value, &elem, &at);
                    // B-2026-08-14-12 — the float-narrowing sibling.
                    self.check_float_narrowing_coercion(&arg.value, &elem, &at);
                    self.check_assignable(&elem, &at, arg.value.span);
                }
                Type::Bool
            }
            // B-2026-08-19-21 — the missed siblings of B-2026-08-15-22. That
            // bug fixed `Vec`/`VecDeque`'s `contains` to accept a BORROWED
            // needle; the set arms kept comparing the raw type and so rejected
            // `s.contains(w)` for a `w: ref String` with "expected 'String',
            // found 'ref String'" — against a spec that says `val: ref T`.
            // `remove` rides along: it uses the needle to find a slot and drops
            // it, never storing it, so design.md gives it `val: ref T` too.
            "contains" | "remove" => {
                for arg in args {
                    let at = self.infer_expr(&arg.value);
                    // The numeric-coercion checks keep the RAW type, so a
                    // borrowed numeric needle behaves exactly as before and no
                    // coercion is recorded against a reference.
                    self.check_int_widening_coercion(&arg.value, &elem, &at);
                    // B-2026-08-14-12 — the float-narrowing sibling.
                    self.check_float_narrowing_coercion(&arg.value, &elem, &at);
                    let probe_at = crate::typechecker::peel_probe_ref(&at);
                    self.check_assignable(&elem, &probe_at, arg.value.span);
                }
                Type::Bool
            }
            "min" | "max" => {
                if !args.is_empty() {
                    self.type_error(
                        format!("SortedSet.{}() takes no arguments", method),
                        *span,
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                }
                option_elem
            }
            "union" | "intersection" | "difference" => {
                for arg in args {
                    let at = self.infer_expr(&arg.value);
                    self.check_assignable(&sorted_set_elem, &at, arg.value.span);
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

    /// Infer the return type of a method call on `SortedMap[K, V]`.
    /// `key` is K, `val` is V from the receiver's type arguments. Called from
    /// `infer_method_call` when the object type is
    /// `Type::Named { name: "SortedMap", ... }`. The key→value sibling of
    /// `SortedSet`: core map surface plus the ordered queries
    /// (`min` / `max` / `range` / `floor` / `ceiling`).
    pub(super) fn infer_sorted_map_method(
        &mut self,
        key: &Type,
        val: &Type,
        method: &str,
        args: &[CallArg],
        span: &Span,
    ) -> Type {
        // K: Ord bound — SortedMap requires a total order on its key type.
        if !self.type_supports_ord(key) {
            self.type_error(
                format!(
                    "SortedMap[{}, ...]: key type does not implement `Ord`; \
                     only types with a total order (integers, bool, char, String, \
                     or structs/enums with `#[derive(Ord)]`) can be SortedMap keys",
                    type_display(key)
                ),
                *span,
                TypeErrorKind::TraitBoundNotSatisfied,
            );
        }
        let k = key.clone();
        let v = val.clone();
        let tuple_kv = Type::Tuple(vec![k.clone(), v.clone()]);
        let option_kv = Type::Named {
            name: "Option".to_string(),
            args: vec![tuple_kv.clone()],
        };
        let vec_k = Type::Named {
            name: "Vec".to_string(),
            args: vec![k.clone()],
        };
        let vec_v = Type::Named {
            name: "Vec".to_string(),
            args: vec![v.clone()],
        };
        let vec_kv = Type::Named {
            name: "Vec".to_string(),
            args: vec![tuple_kv],
        };
        let sorted_map_kv = Type::Named {
            name: "SortedMap".to_string(),
            args: vec![k.clone(), v.clone()],
        };

        match method {
            "len" => {
                if !args.is_empty() {
                    self.type_error(
                        "SortedMap.len() takes no arguments".to_string(),
                        *span,
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                }
                Type::Int(IntSize::I64)
            }
            "is_empty" => {
                if !args.is_empty() {
                    self.type_error(
                        "SortedMap.is_empty() takes no arguments".to_string(),
                        *span,
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                }
                Type::Bool
            }
            "contains_key" => {
                for arg in args {
                    self.check_map_lookup_key_arg(&k, arg);
                }
                Type::Bool
            }
            "get" => {
                for arg in args {
                    self.check_map_lookup_key_arg(&k, arg);
                }
                // Re-read V through the substitutions: a sibling `insert`
                // may have pinned it, and even a lone `get(k)` on a fresh
                // `Map.new()` should surface the resolved value type.
                let resolved_v = resolve_type_var_top(&v, &self.env.substitutions);
                // B-2026-08-09-2 — a `weak V` VALUE read is an UPGRADE, the twin
                // of the `Vec[weak T]` element read (`exprs.rs`, the
                // `!index_is_lhs` arm) and of the `weak` FIELD read. Without it
                // the `Some` payload was a bare `weak V`, every field access
                // through the binding was rejected ("no field 'v' on type 'this
                // type'"), and the container was store-only: fine for
                // cycle-breaking back-edges, which exist not to be traversed,
                // useless for a parent-pointer walk.
                //
                // ONE Option, not `Option[Option[V]]`. A missing key and a dead
                // referent both yield `None`, and collapsing them is the honest
                // shape rather than a shortcut: a referent that has been
                // released IS a key whose value is gone, and no caller can act
                // on the difference — the entry it would ask about no longer
                // refers to anything.
                let payload = match &resolved_v {
                    Type::Weak(referent) => (**referent).clone(),
                    _ => resolved_v,
                };
                Type::Named {
                    name: "Option".to_string(),
                    args: vec![payload],
                }
            }
            "get_or" => {
                if let Some(key_arg) = args.first() {
                    self.check_map_lookup_key_arg(&k, key_arg);
                }
                if let Some(default_arg) = args.get(1) {
                    self.check_map_slot_arg(&v, default_arg);
                }
                resolve_type_var_top(&v, &self.env.substitutions)
            }
            "insert" => {
                if let Some(key_arg) = args.first() {
                    self.check_map_slot_arg(&k, key_arg);
                }
                if let Some(val_arg) = args.get(1) {
                    // B-2026-08-08-29 — a `weak V` VALUE slot takes the
                    // container gate, not the generic weak-FIELD coercion in
                    // `check_expr`. Two reasons, both measured:
                    //
                    //  * the store site has to be recorded for the interpreter,
                    //    which has no static value type of its own to consult
                    //    (the `Vec[weak T]` twin, B-2026-08-08-14);
                    //  * the field coercion also admits `Option[T]` and `None`,
                    //    and the container store lowers only a BARE strong
                    //    handle. `m.insert(k, Option.Some(a))` compiled and put
                    //    a mangled word in the bucket -- a word the scope-exit
                    //    weak drain would then hand to `karac_weak_drop`.
                    //    Exactly the narrowing `Vec[weak T].push` already
                    //    applies, for exactly the same reason: the typechecker
                    //    must not admit what codegen does not implement.
                    let slot = resolve_type_var_top(&v, &self.env.substitutions);
                    if let Type::Weak(referent) = &slot {
                        self.check_weak_container_slot_arg(referent, val_arg, "SortedMap");
                    } else {
                        self.check_map_slot_arg(&v, val_arg);
                    }
                }
                let resolved_v = resolve_type_var_top(&v, &self.env.substitutions);
                Type::Named {
                    name: "Option".to_string(),
                    args: vec![resolved_v],
                }
            }
            "remove" => {
                for arg in args {
                    self.check_map_lookup_key_arg(&k, arg);
                }
                let resolved_v = resolve_type_var_top(&v, &self.env.substitutions);
                Type::Named {
                    name: "Option".to_string(),
                    args: vec![resolved_v],
                }
            }
            "keys" => {
                if !args.is_empty() {
                    self.type_error(
                        "SortedMap.keys() takes no arguments".to_string(),
                        *span,
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                }
                vec_k
            }
            "values" => {
                if !args.is_empty() {
                    self.type_error(
                        "SortedMap.values() takes no arguments".to_string(),
                        *span,
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                }
                vec_v
            }
            "entries" => {
                if !args.is_empty() {
                    self.type_error(
                        "SortedMap.entries() takes no arguments".to_string(),
                        *span,
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                }
                vec_kv
            }
            "merge" => {
                for arg in args {
                    let at = self.infer_expr(&arg.value);
                    self.check_assignable(&sorted_map_kv, &at, arg.value.span);
                }
                sorted_map_kv
            }
            "clear" => {
                if !args.is_empty() {
                    self.type_error(
                        "SortedMap.clear() takes no arguments".to_string(),
                        *span,
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                }
                Type::Unit
            }
            "min" | "max" => {
                if !args.is_empty() {
                    self.type_error(
                        format!("SortedMap.{}() takes no arguments", method),
                        *span,
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                }
                option_kv
            }
            "floor" | "ceiling" => {
                if args.len() != 1 {
                    self.type_error(
                        format!(
                            "SortedMap.{}() expects 1 argument, found {}",
                            method,
                            args.len()
                        ),
                        *span,
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                }
                for arg in args {
                    let at = self.infer_expr(&arg.value);
                    self.check_assignable(&k, &at, arg.value.span);
                }
                option_kv
            }
            "range" => {
                if args.len() != 2 {
                    self.type_error(
                        format!(
                            "SortedMap.range() expects 2 arguments, found {}",
                            args.len()
                        ),
                        *span,
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                }
                for arg in args {
                    let at = self.infer_expr(&arg.value);
                    self.check_assignable(&k, &at, arg.value.span);
                }
                vec_kv
            }
            "entry" => {
                // `entry(key: K) -> Entry[K, V]` — the same insert-or-modify view
                // as `Map.entry` (SortedMap shares Map's `KaracMap` storage;
                // `entry` mutates that storage, which is order-independent, so the
                // ascending-iteration wrapper is unaffected). Drives the
                // or_insert / or_insert_with / and_modify chain via
                // `infer_entry_method`, exactly as the Map path does.
                if args.len() != 1 {
                    self.type_error(
                        format!("SortedMap.entry() expects 1 argument, found {}", args.len()),
                        *span,
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                    for arg in args {
                        self.infer_expr(&arg.value);
                    }
                } else {
                    self.check_map_slot_arg(&k, &args[0]);
                }
                Type::Named {
                    name: "Entry".to_string(),
                    args: vec![k.clone(), v.clone()],
                }
            }
            _ => self.require_known_method(
                "SortedMap",
                method,
                &[
                    "ceiling",
                    "clear",
                    "contains_key",
                    "entries",
                    "entry",
                    "floor",
                    "get",
                    "get_or",
                    "insert",
                    "is_empty",
                    "keys",
                    "len",
                    "max",
                    "merge",
                    "min",
                    "range",
                    "remove",
                    "values",
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
                *span,
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
                        *span,
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                }
                Type::Int(IntSize::I64)
            }
            "is_empty" => {
                if !args.is_empty() {
                    self.type_error(
                        "Set.is_empty() takes no arguments".to_string(),
                        *span,
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                }
                Type::Bool
            }
            // B-2026-08-14-1 — `check_assignable` alone admits an implicit
            // NARROWING into the element slot, which the language refuses
            // everywhere else it is checked. These infer-then-compare arms
            // never reached `check_int_widening_coercion`, so `Set[u8]` took a
            // `300i64` with no diagnostic; the compiled build then truncated to
            // 44 while the interpreter stored 300, leaving a `Set[u8]` holding
            // a value its element type cannot represent. The `Map` sibling
            // routes its slot args through `check_expr`, which is why
            // `Map.insert` rejected the same shape all along. Applied to
            // `SortedSet` too — same arms, same hole, and the sweep only
            // probed the hashed one.
            // `insert` STORES its argument, so its spec really is `val: T` and
            // an owned value is required. `contains` and `remove` are PROBES —
            // design.md gives both `val: ref T` — so they are split out below.
            "insert" => {
                for arg in args {
                    let at = self.infer_expr(&arg.value);
                    self.check_int_widening_coercion(&arg.value, &elem, &at);
                    // B-2026-08-14-12 — the float-narrowing sibling.
                    self.check_float_narrowing_coercion(&arg.value, &elem, &at);
                    self.check_assignable(&elem, &at, arg.value.span);
                }
                Type::Bool
            }
            // B-2026-08-19-21 — the missed siblings of B-2026-08-15-22. That
            // bug fixed `Vec`/`VecDeque`'s `contains` to accept a BORROWED
            // needle; the set arms kept comparing the raw type and so rejected
            // `s.contains(w)` for a `w: ref String` with "expected 'String',
            // found 'ref String'" — against a spec that says `val: ref T`.
            // `remove` rides along: it uses the needle to find a slot and drops
            // it, never storing it, so design.md gives it `val: ref T` too.
            "contains" | "remove" => {
                for arg in args {
                    let at = self.infer_expr(&arg.value);
                    // The numeric-coercion checks keep the RAW type, so a
                    // borrowed numeric needle behaves exactly as before and no
                    // coercion is recorded against a reference.
                    self.check_int_widening_coercion(&arg.value, &elem, &at);
                    // B-2026-08-14-12 — the float-narrowing sibling.
                    self.check_float_narrowing_coercion(&arg.value, &elem, &at);
                    let probe_at = crate::typechecker::peel_probe_ref(&at);
                    self.check_assignable(&elem, &probe_at, arg.value.span);
                }
                Type::Bool
            }
            "union" | "intersection" | "difference" => {
                for arg in args {
                    let at = self.infer_expr(&arg.value);
                    self.check_assignable(&set_elem, &at, arg.value.span);
                }
                set_elem
            }
            // B-2026-08-12-8 — codegen has implemented `Set.clear()` all along
            // (collections.rs) and an E2E test covered it, but the typechecker
            // never listed it, so `karac check` rejected every program that
            // used it and the test only passed because the harness discarded
            // typecheck errors (B-2026-08-11-34).
            "clear" => {
                self.expect_no_args("Set.clear", args, span);
                Type::Unit
            }
            _ => self.require_known_method(
                "Set",
                method,
                &[
                    "clear",
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
