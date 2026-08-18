//! Fresh-temporary receiver type recording — side tables for codegen.
//!
//! Twelfth slice of the `infer_method_call` decomposition
//! ([`docs/spikes/state-decomposition-codegen-methodcall.md`]). Unlike
//! every other extracted family, these blocks mostly do not *type* the
//! call at all: they record the receiver's element or container type in a
//! span-keyed side table (`temp_recv_elem_types`, `temp_recv_mapset_types`)
//! so codegen can reconstruct a temporary's shape later.
//!
//! They exist because a **fresh temporary receiver** — `make_vec().iter()`,
//! `[1, 2, 3].iter()`, `make_map().get(k)` — has no binding whose type
//! codegen can look up. Without the recording, codegen's temp-source
//! for-loop path (`try_compile_for_vec_value`) finds no entry and
//! **silently skips the loop body**: a wrong-answer miscompile rather than
//! an error (B-2026-07-18-39, and the `TupleIndex` sibling that iterated
//! zero times).
//!
//! Covered receiver shapes: a `Call` / `MethodCall` result, a collection
//! literal (for the `iter` / `into_iter` arm only — the read-method arms
//! keep their call-receiver-only behaviour), a `TupleIndex` place, and the
//! `Map` / `Set` fresh-temp forms, which record the whole `Map[K, V]` /
//! `Set[T]` because codegen needs K+V both to redispatch through
//! `compile_map_method` and to classify the handle's `FreeMapHandle` drop.
//!
//! Being recording rather than dispatch, the function returns `Some` only
//! for the one arm that does claim the call; everything else falls
//! through. Its position in the chain is still load-bearing: the records
//! must be written before any later arm consumes them.
//!
//! Lives in a sibling `impl<'a> super::TypeChecker<'a>` block.

use crate::ast::*;
use crate::resolver::SpanKey;
use crate::token::Span;

use super::inference::resolve_type_var_top;
use super::types::Type;

impl<'a> super::TypeChecker<'a> {
    /// Record fresh-temporary receiver types for codegen, and type the one
    /// arm of this surface that claims its call.
    ///
    /// Returns `Some(ty)` only for that arm; `None` otherwise, leaving the
    /// call to later links in the `infer_method_call` chain.
    pub(super) fn record_temp_receiver_types(
        &mut self,
        object: &Expr,
        method: &str,
        args: &[CallArg],
        span: &Span,
        args_close_span: &Span,
        obj_ty: &Type,
    ) -> Option<Type> {
        // General owned-temp tracking, slice 3b — element-type-aware read
        // methods (`get`/`first`/`last`/`get_unchecked`/`contains`) on a
        // FRESH-TEMP (non-identifier) `Vec`/`VecDeque` receiver
        // (`make_vec().get(0)`). Codegen materializes the temp into a synthetic
        // local and re-dispatches through `compile_vec_method`, which needs the
        // receiver's ELEMENT type to shape the `Option[T]` payload — but it
        // cannot recover it from `expr_types` because `MethodCall.span ==
        // receiver.span` holds the method's *result* type (`Option[T]`), not the
        // receiver's `Vec[T]`. Record the element `TypeExpr` here (where
        // `obj_ty` is the receiver type), keyed by the call span — the same
        // collision dodge `method_unwrap_inner_types` / `method_callee_types`
        // use. Gated to `Call`/`MethodCall` receivers — the fresh-temp shapes
        // codegen's `expr_yields_fresh_owned_temp` recognizes; a place-expression
        // receiver (identifier / field / index) is owned elsewhere and routes
        // through the named-binding dispatch.
        //
        // Element scope: SCALAR elements service all five read methods — a
        // scalar element owns no nested heap, so the single outer
        // `FreeVecBuffer` is the complete, double-free-free drop. STRING
        // elements (slice 3b-heap) service the borrow-returning
        // `get`/`first`/`last` plus `contains`:
        //   - `get`/`first`/`last` return `Option[ref String]` aliasing an
        //     element inside the soon-freed temp buffer, but
        //     `scrutinee_is_borrow_call` (receiver-shape-agnostic — it keys off
        //     the *method*, not the object) already suppresses the `Some(s)`
        //     arm binding's independent drop, and the `FreeVecBuffer` vec-struct
        //     recursion frees each per-element String buffer, so the borrow is
        //     the sole reader of storage freed exactly once at frame exit.
        //   - `contains` returns `bool` — no borrow escapes, so there is no
        //     aliasing/suppression obligation at all; it only needs the receiver
        //     temp per-element freed, which the same `FreeVecBuffer` recursion
        //     does. The compared arg is borrowed, not consumed (the named
        //     `Vec[String].contains` path already does element `==` via memcmp
        //     without freeing the arg); a *fresh-owned* arg (`contains(make_str())`)
        //     is the separate 3b-c operand-temp leak, out of scope here.
        // `get_unchecked` (bare `ref String` via a let-binding suppression path
        // that doesn't cover builtin methods, and it needs an `unsafe` block)
        // stays scalar-only — a distinct follow-on. Other heap elements
        // (`Vec[T]`, user struct/enum, Map/Set) need element-drop threading
        // (`elem_agg_drop`) the helper doesn't carry — also follow-ons.
        let recv_is_call = matches!(
            &object.kind,
            ExprKind::Call { .. } | ExprKind::MethodCall { .. }
        );
        // A fresh COLLECTION-LITERAL temp (`vec![1,2,3].iter()`, `[…].iter()`)
        // is neither a Call nor a MethodCall, so the block below never recorded
        // its element type — and codegen's temp-source for-loop / reduce-terminal
        // path (`try_compile_for_vec_value`) then found no `temp_recv_elem_types`
        // entry and SILENTLY skipped the loop body (output 0 vs the interpreter;
        // B-2026-07-18-39). Accept a collection-literal receiver too, but ONLY
        // for the `iter`/`into_iter` arm — the read-method arms (`get`/`first`/
        // `last`/`contains`) keep their call-receiver-only behavior unchanged.
        let recv_is_coll_lit = matches!(
            &object.kind,
            ExprKind::PrefixCollectionLiteral { .. } | ExprKind::ArrayLiteral(_)
        );
        if recv_is_call || recv_is_coll_lit {
            let elem = match obj_ty {
                Type::Named { name, args }
                    if (name == "Vec" || name == "VecDeque") && args.len() == 1 =>
                {
                    Some(args[0].clone())
                }
                _ => None,
            };
            if let Some(elem) = elem {
                let resolved = resolve_type_var_top(&elem, &self.env.substitutions);
                let is_scalar = matches!(
                    resolved,
                    Type::Int(_) | Type::UInt(_) | Type::Float(_) | Type::Bool | Type::Char
                );
                // An owned `String` element resolves to `Type::Str` here (the
                // checker's owned-string representation); a `Type::Named` form
                // is accepted too for robustness. Both `type_to_type_expr` to a
                // `str`/`String` `TypeExpr` that `llvm_type_for_type_expr`
                // lowers to `vec_struct_type`, so the `FreeVecBuffer` vec-struct
                // recursion per-element frees each element's buffer.
                let is_string = matches!(&resolved, Type::Str)
                    || matches!(
                        &resolved,
                        Type::Named { name, args } if name == "String" && args.is_empty()
                    );
                // A one-level nested `Vec[scalar]` / `VecDeque[scalar]` element
                // (`Vec[Vec[i64]]` — matrices, adjacency lists). The element is a
                // `vec_struct_type`, so the `FreeVecBuffer` vec-struct recursion
                // (the documented `Vec[Vec[T]]` one-level path) per-element frees
                // each inner POD buffer, and `get`/`first`/`last` return
                // `Option[ref Vec[scalar]]` — a borrow `scrutinee_is_borrow_call`
                // suppresses. INNER must be scalar: a `Vec[Vec[String]]` would
                // leak the innermost String buffers (two-level nesting exceeds the
                // one-level recursion) — excluded. `contains` (Vec content-eq) and
                // `get_unchecked` stay out for nested Vec.
                let is_pod_vec = matches!(
                    &resolved,
                    Type::Named { name, args }
                        if (name == "Vec" || name == "VecDeque")
                            && args.len() == 1
                            && matches!(
                                resolve_type_var_top(&args[0], &self.env.substitutions),
                                Type::Int(_)
                                    | Type::UInt(_)
                                    | Type::Float(_)
                                    | Type::Bool
                                    | Type::Char
                            )
                );
                // A user-defined STRUCT element (`Vec[Rec]`, `Rec` carrying a
                // `String`/`Vec`/`shared` field). Unlike scalar/String/nested-Vec
                // — whose element either has no destructor or reuses the
                // `vec_struct_type` recursion — a struct element needs its
                // synthesized per-element `__karac_drop_<S>` threaded into the
                // `FreeVecBuffer` (codegen's `vec_elem_agg_drop_for_type_expr` +
                // `track_vec_of_aggs_var`). `get`/`first`/`last` return
                // `Option[ref Rec]`, a borrow `scrutinee_is_borrow_call`
                // suppresses, so each element's heap fields are freed once at
                // frame exit while the borrow reads it. A user ENUM element
                // (`Vec[Tok]`, `Tok` a variant carrying a `String`/`Vec`/shared
                // payload) rides the SAME machinery:
                // `vec_elem_agg_drop_for_type_expr` already routes a non-shared
                // enum to `emit_enum_drop_switch` (and a `shared enum` to a
                // per-element rc-dec), so the per-element drop is threaded
                // identically — no new codegen mechanism. Still excluded:
                // `contains` (enum content-eq) and `get_unchecked`.
                let is_user_struct = matches!(
                    &resolved,
                    Type::Named { name, args } if args.is_empty() && self.env.structs.contains_key(name)
                );
                let is_user_enum = matches!(
                    &resolved,
                    Type::Named { name, args } if args.is_empty() && self.env.enums.contains_key(name)
                );
                // A `shared struct` / `shared enum` element (`Vec[Node]`).
                // Reference-semantic, so the element slot is an 8-byte RC
                // handle, not an inline aggregate — its per-element drop is the
                // rc-DEC `vec_elem_agg_drop_for_type_expr` already synthesizes
                // (`emit_vec_elem_rc_dec_fn`). A shared type resolves to
                // `Type::Shared`, NOT `Type::Named`, so neither predicate above
                // sees it and the `len`-family arm skipped recording — leaving
                // codegen's intercept to fall back to an outer-buffer-only free
                // that released none of the references the temp's clone took
                // (B-2026-08-15-14).
                let is_shared_agg = matches!(&resolved, Type::Shared(_));
                let record = (recv_is_call
                    && ((is_scalar
                        && matches!(
                            method,
                            "get" | "first" | "last" | "get_unchecked" | "contains"
                        ))
                        || (is_string && matches!(method, "get" | "first" | "last" | "contains"))
                        || (is_pod_vec && matches!(method, "get" | "first" | "last"))
                        || (is_user_struct && matches!(method, "get" | "first" | "last"))
                        || (is_user_enum && matches!(method, "get" | "first" | "last"))))
                    // `for x in make_vec().iter()` / `.into_iter()` — a fresh-temp
                    // receiver iterated in a for-loop. The element type drives the
                    // same materialize-iterate-drop path as the read methods, but
                    // here the for-loop peels `.iter()` and recurses on the
                    // receiver: at the collided MethodCall span `expr_types` holds
                    // `Iterator[T]` (clobbering the receiver's `Vec[T]`), so
                    // `owned_temp_drops` has no entry and the loop body is silently
                    // skipped (output 0 vs the interpreter). Recording the element
                    // span-keyed lets codegen reconstruct `Vec[elem]`. Every
                    // element shape above is supported (scalar/String/POD-Vec/user
                    // struct/user enum) — the for-loop reuses the read-method
                    // cleanup threading verbatim.
                    || ((is_scalar
                        || is_string
                        || is_pod_vec
                        || is_user_struct
                        || is_user_enum)
                        && matches!(method, "iter" | "into_iter"));
                if record {
                    let te = Self::type_to_type_expr(&resolved);
                    self.temp_recv_elem_types
                        .insert(SpanKey::from_span(span), te);
                }
                // `mk().len()` / `Env.args().is_empty()` — the element-agnostic
                // read terminals on a fresh-temp receiver (B-2026-07-31-43).
                // Codegen's `len`/`is_empty`/`count` intercept materializes the
                // receiver and drop-tracks it, but the element type it wants
                // from `owned_temp_drops` is span-clobbered (the parser gives a
                // MethodCall its receiver's span, so the chain's outermost
                // scalar result evicts the receiver's `Vec[T]` from
                // `expr_types`) — the track degrades to an outer-buffer-only
                // free and every heap-bearing element leaks. Record the element
                // type in a DEDICATED table (not `temp_recv_elem_types`: at a
                // collided span a chain's `first` record describes a DIFFERENT
                // receiver than its `len` record — see the field doc). Only
                // heap-bearing elements are recorded; a scalar element needs no
                // walk, so its absence keeps today's complete outer-buffer
                // free.
                if recv_is_call
                    && (is_string || is_pod_vec || is_user_struct || is_user_enum || is_shared_agg)
                    && matches!(method, "len" | "is_empty" | "count")
                {
                    let te = Self::type_to_type_expr(&resolved);
                    self.temp_recv_len_elem_types
                        .insert(SpanKey::from_span(span), te);
                }
            }
        }

        // `for x in t.0.iter()` / a fused terminal over a tuple-element `Vec`
        // (`t.0.iter().fold(..)`) — the `TupleIndex` sibling of the fresh-temp
        // recording above. Codegen has a place-based tuple-iter lowering
        // (`try_compile_for_tuple_index_iter`) that GEPs into the tuple to
        // reach the element's `{ptr,len,cap}` storage, but it needs the
        // element's full `TypeExpr` (the per-var tuple name registry is lossy —
        // it drops the generic args and isn't populated for an inferred `let t
        // = f()` binding). The parser sets the `.iter()` MethodCall span equal
        // to its receiver (`t.0`) span, so recording the element type keyed by
        // `span` lands it exactly where codegen reads it
        // (`temp_recv_elem_types[tuple_index.span]`). WITHOUT this, a
        // tuple-element `Vec` iterated in a for-loop (or any fused terminal that
        // desugars to that for-loop — `fold`/`sum`/…) fell through codegen's
        // for-loop dispatch and iterated ZERO times: a silent wrong-answer
        // miscompile (the interpreter iterated the real elements). `Vec` /
        // `VecDeque` receivers only — a `Slice` tuple element has a distinct
        // `{ptr,len}` header the reconstruct-as-`Vec` helper doesn't model.
        if matches!(&object.kind, ExprKind::TupleIndex { .. })
            && matches!(method, "iter" | "into_iter")
        {
            if let Type::Named { name, args } = obj_ty {
                if (name == "Vec" || name == "VecDeque") && args.len() == 1 {
                    let resolved = resolve_type_var_top(&args[0], &self.env.substitutions);
                    let te = Self::type_to_type_expr(&resolved);
                    self.temp_recv_elem_types
                        .insert(SpanKey::from_span(span), te);
                }
            }
        }

        // Sibling of the Vec block above for `Map`/`Set` fresh-temp receivers
        // (`make_map().get(k)`, `make_set().contains(x)`): record the receiver's
        // whole `Map[K,V]` / `Set[T]` type — codegen needs K+V to redispatch
        // through `compile_map_method` and to classify the handle's
        // `FreeMapHandle` drop, so a single element type doesn't suffice. Same
        // `Call`/`MethodCall` fresh-temp gate. Scalar K/V/elem only: `Map.get`
        // returns `Option[ref V]` (a borrow the receiver-shape-agnostic
        // `scrutinee_is_borrow_call` already suppresses), and a scalar V owns no
        // nested heap, so the single `FreeMapHandle` is the complete drop;
        // `contains_key`/`contains` return `bool` (no borrow). Heap K/V (per-entry
        // String/Vec drop) is a follow-on.
        if matches!(
            &object.kind,
            ExprKind::Call { .. } | ExprKind::MethodCall { .. }
        ) {
            let subs = &self.env.substitutions;
            // Scalar OR owned `String` (which resolves to `Type::Str` here, as in
            // the Vec[String] slice). A String K/V makes the handle's
            // `FreeMapHandle` per-entry drop the element buffers
            // (`map_temp_cleanup_parts` classifies `key_is_vec`/`val_is_vec` from
            // the type), and a `Map[_, String].get` returns `Option[ref String]`
            // whose arm binding is suppressed by `scrutinee_is_borrow_call` — the
            // same single-free shape the `Vec[String]` slice established. Other
            // heap K/V (`Vec[T]`, user struct/enum, nested Map) are excluded —
            // they need element-drop threading the helper doesn't carry.
            let is_scalar_or_string = |t: &Type| {
                let r = resolve_type_var_top(t, subs);
                matches!(
                    r,
                    Type::Int(_)
                        | Type::UInt(_)
                        | Type::Float(_)
                        | Type::Bool
                        | Type::Char
                        | Type::Str
                ) || matches!(&r, Type::Named { name, args } if name == "String" && args.is_empty())
            };
            // `iter` is recorded for the for-loop temp path (`for (k, v) in
            // make_map().iter()`): the for-loop peels `.iter()`, recurses on the
            // receiver, and codegen's `try_compile_for_mapset_value` reconstructs
            // the handle from this side-table (the collided `.iter()` span holds
            // `Iterator[(K,V)]` in `expr_types`, so `owned_temp_drops` misses).
            // Same scalar/String K/V constraint as `get` — the `FreeMapHandle`
            // per-entry drop only frees scalar/String entries.
            //
            // `keys` / `values` / `entries` materialize a fresh `Vec[K]` /
            // `Vec[V]` / `Vec[(K,V)]` and take the same fresh-temp Map path
            // (codegen re-dispatches through `compile_map_method` →
            // `compile_map_keys_values_entries`, which CLONES each scalar/String
            // element into the result Vec, so freeing the map handle afterward —
            // `track_map_var` — never dangles the returned Vec). The returned Vec
            // is owned by the enclosing binding / for-loop like any collection
            // method result — `entries` needs no extra tuple-element handling
            // here: its `Vec[(K,V)]` result drop is the SAME machinery the
            // named-map `let es: Vec[(i64,String)] = m.entries()` path already
            // uses; only the Map RECEIVER temp needs this side-table so codegen
            // recognizes the fresh-temp shape at all.
            let record = match obj_ty {
                Type::Named { name, args }
                    if name == "Map"
                        && args.len() == 2
                        && matches!(
                            method,
                            "get"
                                | "contains_key"
                                | "iter"
                                | "keys"
                                | "values"
                                | "entries"
                                // B-2026-08-18-26 — `len` and `is_empty` were
                                // absent here, so no side-table entry was
                                // recorded and codegen's fresh-temp Map/Set
                                // path declined. The receiver then fell through
                                // to a lowering that reads the HANDLE (a plain
                                // pointer) as though it were an inline
                                // aggregate: `mk_map().len()` printed 152 for a
                                // two-entry map and `is_empty()` printed raw
                                // bytes, while `--interp` was correct. A silent
                                // wrong answer, not a build error — six such
                                // calls in one program segfaulted.
                                //
                                // The methods that WORKED were exactly the ones
                                // on this list, which is what identified it.
                                | "len"
                                | "is_empty"
                        )
                        && is_scalar_or_string(&args[0])
                        && is_scalar_or_string(&args[1]) =>
                {
                    Some(Type::Named {
                        name: "Map".to_string(),
                        args: vec![
                            resolve_type_var_top(&args[0], subs),
                            resolve_type_var_top(&args[1], subs),
                        ],
                    })
                }
                Type::Named { name, args }
                    if name == "Set"
                        && args.len() == 1
                        // B-2026-08-18-26 — the `Set` half of the same gap.
                        && matches!(method, "contains" | "iter" | "len" | "is_empty")
                        && is_scalar_or_string(&args[0]) =>
                {
                    Some(Type::Named {
                        name: "Set".to_string(),
                        args: vec![resolve_type_var_top(&args[0], subs)],
                    })
                }
                _ => None,
            };
            if let Some(resolved_recv) = record {
                let te = Self::type_to_type_expr(&resolved_recv);
                self.temp_recv_mapset_types
                    .insert(SpanKey::from_span(span), te);
            }
        }

        // Option/Result unwrap-family side-table: record the inner `T` /
        // success-`T` so codegen's `compile_method_call` arm for
        // `unwrap`/`expect`/`is_*`/`unwrap_or` knows the LLVM shape of the
        // value to reconstitute from the Option/Result payload words. Sibling
        // to `method_callee_types`; mirrors the per-MethodCall-span keying so
        // the lookup at codegen time is O(1). The `is_*` arms record T for
        // uniformity even though codegen only consumes the tag.
        if matches!(
            method,
            "unwrap"
                | "expect"
                | "is_some"
                | "is_none"
                | "is_ok"
                | "is_err"
                | "unwrap_or"
                | "unwrap_err"
                | "expect_err"
        ) {
            // `unwrap_or(default)` eagerly evaluates its fallback — infer it
            // here (where `args` is still the method-call arg list, before the
            // `Type::Named { args }` binding below shadows it) so the default's
            // sub-expressions are typed for codegen. Kept permissive (no hard
            // unify with `T`) to avoid a 722-style over-strict rejection of a
            // coercible default; codegen width-coerces an int default to `T`.
            if method == "unwrap_or" {
                if let Some(a) = args.first() {
                    let _ = self.infer_expr(&a.value);
                }
            }
            let receiver_named = match obj_ty {
                Type::Named { .. } => Some(obj_ty),
                Type::Ref(inner) | Type::MutRef(inner) => match inner.as_ref() {
                    Type::Named { .. } => Some(inner.as_ref()),
                    _ => None,
                },
                _ => None,
            };
            if let Some(Type::Named { name, args }) = receiver_named {
                // `unwrap_err` / `expect_err` extract the ERR payload of a
                // `Result[T, E]`, so their reconstituted inner type is `E` (the
                // SECOND type arg), not `T`. Every other family member (incl. the
                // uniform `is_*` recording) uses the first arg. `_err` is not a
                // valid method on `Option` (no Err half).
                let inner_ty = if matches!(method, "unwrap_err" | "expect_err") {
                    if name == "Result" {
                        args.get(1).cloned()
                    } else {
                        None
                    }
                } else {
                    match name.as_str() {
                        "Option" | "Result" => args.first().cloned(),
                        _ => None,
                    }
                };
                if let Some(inner_ty) = inner_ty {
                    let resolved = resolve_type_var_top(&inner_ty, &self.env.substitutions);
                    let te = Self::type_to_type_expr(&resolved);
                    self.method_unwrap_inner_types
                        .insert(SpanKey::for_method_call(span, args_close_span), te);
                    // `Result[T, E].unwrap_or(d)` DISCARDS the `Err` payload on
                    // the absent path — the default becomes the result and `E`
                    // is dropped on the floor. A heap `E` (`Result[_, String]`)
                    // therefore needs a free emitted there, and codegen can only
                    // reconstruct it from the payload words if it knows `E`.
                    // Record it in the sibling table. (The closure combinators
                    // `unwrap_or_else`/`map_or_else`/`or_else` populate the same
                    // table further down for a different reason — they FEED `e`
                    // to the absent closure rather than discarding it.)
                    // B-2026-08-05-9.
                    if method == "unwrap_or" && name == "Result" {
                        if let Some(e_ty) = args.get(1).cloned() {
                            let e_resolved = resolve_type_var_top(&e_ty, &self.env.substitutions);
                            self.method_unwrap_err_types.insert(
                                SpanKey::for_method_call(span, args_close_span),
                                Self::type_to_type_expr(&e_resolved),
                            );
                        }
                    }
                    // Surface a proper return type so the binding gets the
                    // right Type rather than falling through to the
                    // prelude-permissive `Type::Error`. Without this,
                    // `let x = m.get(k).unwrap()` binds `x: Type::Error`,
                    // which breaks downstream `x.field` / `x.method(...)`
                    // resolution (field-access dispatch keys off
                    // `var_type_names` populated from `pattern_binding_types`).
                    return Some(match method {
                        "unwrap" | "expect" | "unwrap_or" | "unwrap_err" | "expect_err" => resolved,
                        "is_some" | "is_none" | "is_ok" | "is_err" => Type::Bool,
                        _ => unreachable!(),
                    });
                }
            }
        }
        None
    }
}
