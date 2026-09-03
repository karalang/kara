//! Vec / VecDeque mutation-method typechecking.
//!
//! Second slice of the `infer_method_call` decomposition
//! ([`docs/spikes/state-decomposition-codegen-methodcall.md`]). Holds the
//! element-slot-checking mutators on the built-in sequence types: `push`,
//! `insert`, `extend` / `extend_from_slice`, `pop` / `pop_back` /
//! `pop_front`, `remove` / `swap_remove`, `get_unchecked`, and `push_back` /
//! `push_front`.
//!
//! These exist as a dedicated surface because `Vec` is a built-in prelude
//! type with no impl block: without them the call falls through to the
//! generic arm and the argument is never checked against the element type
//! (round 12.46 / Step 4 — see the `push` block's own comment).
//!
//! The block order inside `try_vec_mutation_method` is load-bearing —
//! `infer_method_call` is a first-match-wins chain, so these guards keep the
//! exact relative order they had inline, and the function is called from the
//! same position in that chain.
//!
//! Lives in a sibling `impl<'a> super::TypeChecker<'a>` block.

use crate::ast::*;

use super::inference::{resolve_type_var_top, resolve_type_vars, unify_types};
use super::types::{type_display, IntSize, Type};
use super::TypeErrorKind;
use std::collections::HashMap;

impl<'a> super::TypeChecker<'a> {
    /// Type a mutation method on a `Vec` / `VecDeque` receiver.
    ///
    /// Returns `Some(ty)` when this surface claims `method` (including
    /// `Some(Type::Error)` when it claims the name but the call is
    /// ill-formed and a diagnostic has been emitted), and `None` when the
    /// name belongs to some later link in the `infer_method_call` chain.
    pub(super) fn try_vec_mutation_method(
        &mut self,
        method: &str,
        args: &[CallArg],
        obj_ty: &Type,
    ) -> Option<Type> {
        // `Vec[T].push(item: T)` slot check (round 12.46 / Step 4). Vec is a
        // built-in prelude type with no impl block, so without this dispatch
        // `push` falls through to the silent `Type::Error` arm below and the
        // argument never gets checked against the element type. Routing the
        // single argument through `check_assignable(element, arg_ty, span)`
        // means a once-callable closure value flowing into a `Vec[Fn(...)]`
        // element slot triggers `OnceFnIntoFnSlot` via the same path Step 3
        // wired for parameter slots. Other Vec methods continue through the
        // historical fall-through to preserve existing test behavior — Step 5
        // can promote them when needed.
        if method == "push" && args.len() == 1 {
            let element_ty = match obj_ty {
                Type::Named { name, args } if name == "Vec" && args.len() == 1 => {
                    Some(args[0].clone())
                }
                Type::Ref(inner) | Type::MutRef(inner) => match inner.as_ref() {
                    Type::Named { name, args } if name == "Vec" && args.len() == 1 => {
                        Some(args[0].clone())
                    }
                    _ => None,
                },
                _ => None,
            };
            if let Some(elem) = element_ty {
                // B-2026-08-08-5 — a `weak T` ELEMENT. The downgrade coercion
                // lives in `check_expr`, keyed on the EXPECTED type, so a site
                // that infers-then-compares never reaches it: that is why
                // `Vec[weak N].push(strong)` reported `expected 'weak N', found
                // 'N'` while the field store `c.parent = p` accepted the same
                // value.
                //
                // NARROWER THAN THE FIELD RULE, ON PURPOSE. A `weak` FIELD also
                // accepts `Option[T]` and `None`; the container store lowers
                // only a BARE strong handle, and admitting the `Option` form
                // here compiled a program that SEGFAULTED at runtime — the
                // typechecker must not admit what codegen does not implement,
                // which is the whole reason this is a hand-written gate rather
                // than a call to `check_expr`. Widening it is a codegen change
                // first and a typechecker change second.
                if let Type::Weak(referent) = &elem {
                    // B-2026-08-08-14 — record the store for the interpreter,
                    // which has no static element type of its own to consult.
                    // Keyed by the ARGUMENT's span rather than the call's: a
                    // method call shares its receiver's span here, so the call
                    // span is not unique to this push.
                    self.weak_elem_store_sites
                        .insert(crate::resolver::SpanKey::from_span(&args[0].value.span));
                    let actual = self.infer_expr(&args[0].value);
                    let ok = matches!(actual, Type::Error | Type::Never)
                        || super::types::types_compatible(&actual, referent);
                    if !ok {
                        self.type_error(
                            format!(
                                "cannot push a value of type '{}' into a `Vec[weak {}]`; \
                                 a container element takes a BARE `{}` handle, which is \
                                 downgraded on the way in. The `Option[{}]` / `None` forms a \
                                 `weak` FIELD accepts are not lowered for a container element \
                                 yet — bind the handle first and push that",
                                type_display(&actual),
                                type_display(referent),
                                type_display(referent),
                                type_display(referent),
                            ),
                            args[0].value.span,
                            TypeErrorKind::TypeMismatch,
                        );
                        self.record_expr_type(&args[0].value.span, &Type::Error);
                        return Some(Type::Unit);
                    }
                    self.record_expr_type(&args[0].value.span, &elem);
                    return Some(Type::Unit);
                }
                let arg_ty = self.infer_expr(&args[0].value);
                // B-2026-08-08-2 — a `frozen` parameter reads as `ref T`; the
                // ownership pass decides whether this store is legal.
                let arg_ty = self.deref_frozen_param_arg(&args[0].value, arg_ty, &elem);
                // B-2026-08-14-1 — reject an implicit NARROWING into the
                // element slot. This gate is a hand-written slot check rather
                // than a `check_expr` (see the `weak` comment above for why),
                // and the narrowing rule rode on `check_expr`, so `Vec[u8]`
                // accepted a `300i64` with no diagnostic — while `Vec`'s OWN
                // index-assign, `v[i] = nsrc`, rejected it. Placed after the
                // `frozen` deref so a `ref T` argument compares peeled, which
                // is what the gate's own peel expects.
                self.check_int_widening_coercion(&args[0].value, &elem, &arg_ty);
                // B-2026-08-14-12 — the float-narrowing sibling. `push`
                // INFERS its argument and never reaches `check_expr`, so
                // without this the one position that escaped leg 1's
                // re-record would escape the gate too.
                self.check_float_narrowing_coercion(&args[0].value, &elem, &arg_ty);
                // B-2026-08-14-6 — the int-to-float sibling of the gate above.
                // This site hand-rolls its slot check rather than routing
                // through `check_expr`, so the generic recording boundary never
                // sees it and the interpreter left an `Int` in a `Vec[f64]`.
                self.record_float_coercion(&args[0].value, &elem, &arg_ty);
                // B-2026-09-03-20 — and the own-`Drop` partial-move sibling,
                // which escapes for the same structural reason those three do:
                // this site hand-rolls its slot check instead of routing
                // through `check_expr`, so every rule that hangs off a value
                // position has to be re-hung here by hand. `v.push(w.r)`
                // measured two `R` bodies with the second diverging
                // run-vs-build.
                self.warn_partial_move_of_drop_struct(&args[0].value, &elem);
                // Unify so an unsolved element typevar bound to the
                // receiver (e.g. `let mut v = Vec.new(); v.push(x);`)
                // gets pinned to the first push's value type. Otherwise
                // the binding's `pattern_binding_inner_types` entry
                // stays unresolved and codegen registers `i64` instead
                // of the right LLVM element type.
                unify_types(
                    &elem,
                    &arg_ty,
                    &mut self.env.substitutions,
                    &mut self.env.const_substitutions,
                );
                // Resolve BOTH sides through the substitutions the unify
                // just populated, so the assignability check doesn't
                // compare against a stale typevar. `resolve_type_var_top`
                // only reaches the TOP level — enough for the scalar case
                // (`let mut v = Vec.new(); v.push(5)` pins the receiver's
                // element var), but NOT for an empty container constructor
                // in argument position: `out.push(Vec.new())` with
                // `out: Vec[Vec[i64]]` unifies `Vec[i64]` with `Vec[?T0]`
                // (binding `?T0 = i64`), yet the arg's NESTED `?T0` stayed
                // unresolved and reported a spurious `Vec[i64]` vs
                // `Vec[?T0]` mismatch (B-2026-07-11-10). Deep-resolving the
                // arg makes the empty-constructor push infer without the
                // `let empty: Vec[i64] = Vec.new()` annotation.
                let no_names = HashMap::new();
                let no_const_names = HashMap::new();
                let resolved_elem = resolve_type_vars(
                    &elem,
                    &self.env.substitutions,
                    &no_names,
                    &self.env.const_substitutions,
                    &no_const_names,
                );
                let resolved_arg = resolve_type_vars(
                    &arg_ty,
                    &self.env.substitutions,
                    &no_names,
                    &self.env.const_substitutions,
                    &no_const_names,
                );
                self.check_assignable(&resolved_elem, &resolved_arg, args[0].value.span);
                // B-2026-08-02-12 — the arg was INFERRED, so an inference-
                // driven collection constructor (`v.push(Map.new())`) recorded
                // `Map[?K, ?V]` in `expr_types` BEFORE the unify above bound
                // the vars; lowering renders those vars as `Error` TypeExprs
                // and codegen's expression-position `Map.new()` handle
                // construction then built the handle with an i64 key default —
                // wrong key size/hash for a `Map[String, _]` (silent lookup
                // MISses + a key-buffer leak at map free). Re-record the
                // ctor arg's entry with the RESOLVED type so the span-keyed
                // table codegen consults carries the concrete K/V.
                self.rerecord_resolved_ctor_arg(&args[0].value, &resolved_arg);
                // B-2026-08-14-11 — the pushed ARGUMENT is inferred here, not
                // checked, so `check_expr`'s narrow-float literal re-record
                // never sees it: `Vec[f32].push(0.1)` left the literal typed
                // `f64` and the interpreter stored the full double. Same
                // re-record, applied where the element type is in hand.
                self.record_narrow_float_literal(&args[0].value, &resolved_elem);
                return Some(Type::Unit);
            }
        }
        // `Vec[T].insert(idx: i64, value: T) -> ()` — shift the tail up and
        // place `value` at `idx` (`idx == len` appends). Sibling of `push`
        // (same element-var unification so `let mut v = Vec.new(); v.insert(0,
        // x)` pins the element type) and `remove` (arg 0 is the i64 index).
        //
        // (See `rerecord_resolved_ctor_arg` in the push arm above for why the
        // ctor-shaped value arg's expr-type entry is re-recorded post-unify.)
        if method == "insert" && args.len() == 2 {
            let element_ty = match obj_ty {
                Type::Named { name, args }
                    if (name == "Vec" || name == "VecDeque") && args.len() == 1 =>
                {
                    Some(args[0].clone())
                }
                Type::Ref(inner) | Type::MutRef(inner) => match inner.as_ref() {
                    Type::Named { name, args }
                        if (name == "Vec" || name == "VecDeque") && args.len() == 1 =>
                    {
                        Some(args[0].clone())
                    }
                    _ => None,
                },
                _ => None,
            };
            if let Some(elem) = element_ty {
                let idx_ty = self.infer_expr(&args[0].value);
                self.check_assignable(&Type::Int(IntSize::I64), &idx_ty, args[0].value.span);
                let val_ty = self.infer_expr(&args[1].value);
                unify_types(
                    &elem,
                    &val_ty,
                    &mut self.env.substitutions,
                    &mut self.env.const_substitutions,
                );
                let no_names = HashMap::new();
                let no_const_names = HashMap::new();
                let resolved_elem = resolve_type_vars(
                    &elem,
                    &self.env.substitutions,
                    &no_names,
                    &self.env.const_substitutions,
                    &no_const_names,
                );
                let resolved_arg = resolve_type_vars(
                    &val_ty,
                    &self.env.substitutions,
                    &no_names,
                    &self.env.const_substitutions,
                    &no_const_names,
                );
                self.check_assignable(&resolved_elem, &resolved_arg, args[1].value.span);
                // Ctor-shaped value arg: re-record resolved (B-2026-08-02-12,
                // see the push arm).
                self.rerecord_resolved_ctor_arg(&args[1].value, &resolved_arg);
                return Some(Type::Unit);
            }
        }

        // `Vec[T].extend_from_slice(other)` — `other` may be
        // `Slice[T]`, `Vec[T]`, or `Array[T, N]`. We unify the
        // receiver's element type with the source's element type so
        // that an unsolved typevar on the receiver (e.g. `let mut v =
        // Vec.new(); v.extend_from_slice(other);`) gets pinned to the
        // source's element type, mirroring `push`'s behavior.
        if matches!(method, "extend_from_slice" | "extend") && args.len() == 1 {
            let element_ty = match obj_ty {
                Type::Named { name, args } if name == "Vec" && args.len() == 1 => {
                    Some(args[0].clone())
                }
                Type::Ref(inner) | Type::MutRef(inner) => match inner.as_ref() {
                    Type::Named { name, args } if name == "Vec" && args.len() == 1 => {
                        Some(args[0].clone())
                    }
                    _ => None,
                },
                _ => None,
            };
            if let Some(elem) = element_ty {
                let arg_ty = self.infer_expr(&args[0].value);
                // Peel one layer of Ref/MutRef from the source — the
                // arg may arrive as `ref Slice[T]` / `ref Vec[T]` /
                // `mut Slice[T]` depending on the call site.
                let arg_inner = match &arg_ty {
                    Type::Ref(inner) | Type::MutRef(inner) => (**inner).clone(),
                    other => other.clone(),
                };
                let src_elem = match &arg_inner {
                    Type::Named { name, args }
                        if (name == "Slice" || name == "Vec") && args.len() == 1 =>
                    {
                        Some(args[0].clone())
                    }
                    // A structural `Type::Slice { element }` source — what
                    // `String.bytes()` / `Vec.as_slice()` / a `v.slice(a, b)`
                    // view produce (the byte-slice shape, distinct from the
                    // `Type::Named { name: "Slice" }` spelling). Both backends
                    // already accept a 2-field slice source
                    // (`vec_method.rs`); without this arm the call fell through
                    // to the silent prelude Error-typing (part of
                    // B-2026-07-17-12) — e.g. `buf.extend_from_slice(s.bytes())`
                    // typed as `Type::Error` instead of `()`.
                    Type::Slice { element, .. } => Some((**element).clone()),
                    Type::Array { element, .. } => Some((**element).clone()),
                    _ => None,
                };
                if let Some(src) = src_elem {
                    unify_types(
                        &elem,
                        &src,
                        &mut self.env.substitutions,
                        &mut self.env.const_substitutions,
                    );
                    let resolved_elem = resolve_type_var_top(&elem, &self.env.substitutions);
                    let resolved_src = resolve_type_var_top(&src, &self.env.substitutions);
                    self.check_assignable(&resolved_elem, &resolved_src, args[0].value.span);
                    return Some(Type::Unit);
                }
            }
        }

        // `Vec[T].pop()` / `Vec[T].pop_back()` and `VecDeque[T]`'s
        // `pop_front` / `pop_back` all return `Option[T]` per design.md.
        // The codegen-side pop arm builds an `Option[T]` aggregate via
        // multi-word payload words (commit 76263d1); without the
        // typechecker recording the return type, an unannotated
        // `match q.pop_front() { Some(node) => ... }` infers scrutinee
        // type `Error` and pattern bindings lose their tuple types,
        // breaking the `Some(node) => let (a, b) = node` shape's
        // tuple-binding reconstitution in codegen.
        if matches!(method, "pop" | "pop_back" | "pop_front") && args.is_empty() {
            let element_ty = match obj_ty {
                Type::Named { name, args }
                    if (name == "Vec" || name == "VecDeque") && args.len() == 1 =>
                {
                    Some(args[0].clone())
                }
                Type::Ref(inner) | Type::MutRef(inner) => match inner.as_ref() {
                    Type::Named { name, args }
                        if (name == "Vec" || name == "VecDeque") && args.len() == 1 =>
                    {
                        Some(args[0].clone())
                    }
                    _ => None,
                },
                _ => None,
            };
            if let Some(elem) = element_ty {
                // Resolve typevars so a `let mut q = VecDeque.new(); q.push(x);
                // let _ = q.pop_front();` round-trips the element type — without
                // this, `?T` solved by `push` stays unresolved in the
                // `Option[?T]` return, and downstream `Some(x)` bindings lose
                // the surface type they need for codegen routing.
                let resolved = resolve_type_var_top(&elem, &self.env.substitutions);
                return Some(Type::Named {
                    name: "Option".to_string(),
                    args: vec![resolved],
                });
            }
        }

        // `Vec[T].remove(idx: i64) -> T` — remove the element at `idx`,
        // shift the tail down by one, return the removed value. v1
        // matches Rust's contract: idx out-of-bounds is UB (no bounds
        // check, no graceful Option). Callers ensure idx < len (the
        // backend TODO API kata's DELETE handler at
        // `kara-katas/backend/todo-api/main.kara` finds the index via
        // `find_index_by_id` first, then removes — the index is
        // known-good at the call). Mirrors the pop_front shape but
        // at an arbitrary index instead of 0.
        if matches!(method, "remove" | "swap_remove") && args.len() == 1 {
            let element_ty = match obj_ty {
                Type::Named { name, args }
                    if (name == "Vec" || name == "VecDeque") && args.len() == 1 =>
                {
                    Some(args[0].clone())
                }
                Type::Ref(inner) | Type::MutRef(inner) => match inner.as_ref() {
                    Type::Named { name, args }
                        if (name == "Vec" || name == "VecDeque") && args.len() == 1 =>
                    {
                        Some(args[0].clone())
                    }
                    _ => None,
                },
                _ => None,
            };
            if let Some(elem) = element_ty {
                let arg_ty = self.infer_expr(&args[0].value);
                self.check_assignable(&Type::Int(IntSize::I64), &arg_ty, args[0].value.span);
                return Some(resolve_type_var_top(&elem, &self.env.substitutions));
            }
        }

        // `Vec[T].get_unchecked(i: i64) -> T` — unsafe direct-index read.
        // Skips the bounds check that `vec[i]` and `Vec.get(i)` emit; UB on
        // out-of-range index. Must be called inside `unsafe { ... }`; the
        // enforcement is hardcoded in `unsafe_lint::build_unsafe_fn_registry`
        // (the built-in equivalent of marking an impl-method `unsafe fn`).
        // Counterpart to the deferred `Slice.get_unchecked` plan at
        // `phase-7-codegen.md:481`; surfaced as the perf lever for the
        // bounds-check tax measured on kata #5 (see `wip-kata5-perf.md`).
        if method == "get_unchecked" && args.len() == 1 {
            // Bare `Slice[T]` receivers dispatch earlier via
            // `infer_slice_method`; this arm covers `Vec[T]` and
            // `ref`/`mut ref` of Vec/Slice.
            // Accepts `Vec[T]` and `Slice[T]` (and `ref`/`mut ref` of either),
            // returning `T` by value — sound for the Copy element types hot
            // scanners use (i64/u8). The `Slice.get_unchecked` escape mirrors
            // the landed `Vec.get_unchecked` so a `Slice[T]` / `mut Slice[T]`
            // param can skip the bounds check the source-level dominator pass
            // can't reach (e.g. KMP's `needle[j]`, where `j` rewinds via the
            // LPS table — provably in-range, not compiler-provable). See
            // phase-7-codegen.md § BCE table-range tier.
            // `Slice[T]` reaches here as either `Type::Slice { element }` (slice
            // expressions / coercions) or `Type::Named { name: "Slice" }`
            // (declared params) — match both, plus `Vec[T]`, through one
            // optional layer of `ref`/`mut ref`.
            fn get_unchecked_elem(t: &Type) -> Option<Type> {
                match t {
                    Type::Named { name, args }
                        if (name == "Vec" || name == "Slice") && args.len() == 1 =>
                    {
                        Some(args[0].clone())
                    }
                    Type::Slice { element, .. } => Some((**element).clone()),
                    _ => None,
                }
            }
            let element_ty = match obj_ty {
                Type::Ref(inner) | Type::MutRef(inner) => get_unchecked_elem(inner.as_ref()),
                other => get_unchecked_elem(other),
            };
            if let Some(elem) = element_ty {
                let arg_ty = self.infer_expr(&args[0].value);
                self.check_assignable(&Type::Int(IntSize::I64), &arg_ty, args[0].value.span);
                return Some(resolve_type_var_top(&elem, &self.env.substitutions));
            }
        }

        // `VecDeque[T].push_back(item)` / `push_front(item)` — slot
        // check sibling to `Vec.push`. Returns `Type::Unit`.
        if matches!(method, "push_back" | "push_front") && args.len() == 1 {
            let element_ty = match obj_ty {
                Type::Named { name, args }
                    if (name == "Vec" || name == "VecDeque") && args.len() == 1 =>
                {
                    Some(args[0].clone())
                }
                Type::Ref(inner) | Type::MutRef(inner) => match inner.as_ref() {
                    Type::Named { name, args }
                        if (name == "Vec" || name == "VecDeque") && args.len() == 1 =>
                    {
                        Some(args[0].clone())
                    }
                    _ => None,
                },
                _ => None,
            };
            if let Some(elem) = element_ty {
                let arg_ty = self.infer_expr(&args[0].value);
                // Mirror the unification in the `Vec.push` arm above so an
                // unsolved receiver-element typevar gets pinned to the first
                // pushed value type.
                unify_types(
                    &elem,
                    &arg_ty,
                    &mut self.env.substitutions,
                    &mut self.env.const_substitutions,
                );
                let resolved_elem = resolve_type_var_top(&elem, &self.env.substitutions);
                // Deep-resolve the ARG side too before the assignability
                // check — `dq.push_back(Map.new())` unifies the ctor's fresh
                // `?K/?V` against the element type, but the recorded
                // `arg_ty` snapshot still carries the vars and was rejected
                // as `expected 'Map<String, i64>', found 'Map<?T0, ?T1>'`
                // (the push arm's B-2026-07-11-10 fix, mirrored here as part
                // of B-2026-08-02-12).
                let no_names = HashMap::new();
                let no_const_names = HashMap::new();
                let deep_resolved_arg = resolve_type_vars(
                    &arg_ty,
                    &self.env.substitutions,
                    &no_names,
                    &self.env.const_substitutions,
                    &no_const_names,
                );
                self.check_assignable(&resolved_elem, &deep_resolved_arg, args[0].value.span);
                // Ctor-shaped arg: re-record resolved (B-2026-08-02-12, see
                // the push arm).
                self.rerecord_resolved_ctor_arg(&args[0].value, &deep_resolved_arg);
                return Some(Type::Unit);
            }
        }
        None
    }
}
