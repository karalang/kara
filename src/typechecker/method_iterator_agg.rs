//! Iterator, collection-aggregation and atomic method typechecking.
//!
//! Eighth slice of the `infer_method_call` decomposition
//! ([`docs/spikes/state-decomposition-codegen-methodcall.md`]). Three
//! adjacent built-in surfaces that share a shape — the receiver is a
//! built-in collection or atomic with no user `impl` block, so without a
//! dedicated arm the call falls through to the silent `Type::Error` arm:
//!
//! - **iteration** — `iter` / `iter_mut` / `into_iter` producing an
//!   `Iterator[Item = T]`, and `clone()` on the collection types
//!   (`Vec[T]`, `String`, `Map[K, V]`, …);
//! - **aggregation** — `sum` / `product` / `max` / `min` over an iterator
//!   or sequence, `join` / `concat` over string sequences (concat being
//!   join with the empty separator), and the `StdinLines` / `LinesIter`
//!   line-iterator surface;
//! - **atomics** — `compare_exchange`, `load` / `store` and the
//!   `fetch_*` read-modify-write family, whose inner type is read off the
//!   receiver.
//!
//! The block order is load-bearing — `infer_method_call` is a
//! first-match-wins chain, so these guards keep the exact relative order
//! they had inline, and the function is called from the same position in
//! that chain.
//!
//! Lives in a sibling `impl<'a> super::TypeChecker<'a>` block.

use crate::ast::*;
use crate::resolver::SpanKey;
use crate::token::Span;

use super::types::{iterator_item_type_for, Type};
use super::TypeErrorKind;

impl<'a> super::TypeChecker<'a> {
    /// Type an iterator, collection-aggregation or atomic method call.
    ///
    /// Returns `Some(ty)` when this surface claims `method` (including
    /// `Some(Type::Error)` when it claims the name but the call is
    /// ill-formed and a diagnostic has been emitted), and `None` when the
    /// name belongs to some later link in the `infer_method_call` chain.
    pub(super) fn try_iterator_agg_atomic_method(
        &mut self,
        object: &Expr,
        method: &str,
        args: &[CallArg],
        span: &Span,
        obj_ty: &Type,
    ) -> Option<Type> {
        // Iterator-source methods: `iter()` / `into_iter()` on any iterable
        // collection produce an `Iterator[Item = T]` value. Handled here in
        // one place so per-collection method handlers don't have to repeat
        // the registration. The borrow-vs-consume distinction between
        // `iter()` and `into_iter()` is a typechecker concern in design.md
        // but immaterial at this layer — both return the same Iterator type.
        // See `wip-list2.md` § Iterator trait — full adaptor surface.
        // B-2026-08-12-8 — `iter_mut()` yields `mut ref T` so `for x in
        // xs.iter_mut() { *x = ... }` writes back in place. Both backends
        // already implement it (codegen `control_flow_for.rs`, interpreter
        // `eval_expr.rs`, each special-casing the for-loop position) and the
        // lowering pass already documents this as "the explicit `.iter_mut()`
        // path (`for x in xs.iter_mut()` → `mut ref T`)" — only the
        // typechecker never listed the method, so `karac check` rejected every
        // program using it and its E2E test passed solely because the harness
        // discarded typecheck errors (B-2026-08-11-34).
        //
        // Typed for every element type, matching the INTERPRETER's support.
        // Codegen handles scalar elements and bails LOUD with an `--interp`
        // pointer for heap elements / destructuring patterns, which is the
        // codebase's normal posture for a partially-lowered construct — unlike
        // the silent nothing this used to be.
        if method == "iter_mut" {
            if let Some(item_ty) = iterator_item_type_for(obj_ty) {
                if !args.is_empty() {
                    self.type_error(
                        "'iter_mut' takes no arguments".to_string(),
                        *span,
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                    for arg in args {
                        self.infer_expr(&arg.value);
                    }
                }
                return Some(Type::Named {
                    name: "Iterator".to_string(),
                    args: vec![Type::MutRef(Box::new(item_ty))],
                });
            }
        }
        if method == "iter" || method == "into_iter" {
            if let Some(item_ty) = iterator_item_type_for(obj_ty) {
                if !args.is_empty() {
                    self.type_error(
                        format!("'{}' takes no arguments", method),
                        *span,
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                    for arg in args {
                        self.infer_expr(&arg.value);
                    }
                }
                return Some(Type::Named {
                    name: "Iterator".to_string(),
                    args: vec![item_ty],
                });
            }
        }

        // `clone()` on collection types — `Vec[T]`, `String`, `Map[K, V]`,
        // `Set[T]`, `SortedSet[T]`, `Array[T, N]` all implement Clone per
        // design.md § Iteration line 1692. Returns `Self`. The `T: Clone`
        // bound on element types is enforced via the existing trait-bound
        // checking; primitives and String satisfy it trivially. The
        // canonical bullet lives in `phase-8-stdlib-floor.md` (search
        // `Clone trait surface for collections`).
        //
        // B-2026-07-29-27 / B-2026-07-29-31 — routed through the env-aware
        // `clone_receiver_self_type` rather than the pure free fn
        // `clone_self_type_for`, so `Option[T]` and user types carrying
        // `#[derive(Clone)]` (plus a `T: Clone`-bounded generic param) get the
        // callable method their satisfiable bound already implied. The free fn
        // is still the first thing consulted inside, so the collection surface
        // is byte-for-byte unchanged. `Result` is deliberately still rejected —
        // see the note in `clone_receiver_self_type`.
        if method == "clone" {
            if let Some(self_ty) = self.clone_receiver_self_type(obj_ty) {
                if !args.is_empty() {
                    self.type_error(
                        "clone() takes no arguments".to_string(),
                        *span,
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                    for arg in args {
                        self.infer_expr(&arg.value);
                    }
                }
                return Some(self_ty);
            }
        }

        // Iterator method dispatch — `Iterator[Item = T].next()` and the
        // adaptor surface (added in subtask 3+). Keyed on the receiver's
        // outer Type::Named name; the Item type is at args[0].
        // `Range` / `RangeInclusive` are also Iterators (matches Rust),
        // routed through the same dispatch so `(0..10).step_by(2)` works
        // without a redundant `.iter()` call.
        if let Type::Named {
            name,
            args: type_args,
        } = obj_ty
        {
            if name == "Iterator"
                || name == "Peekable"
                || name == "Range"
                || name == "RangeInclusive"
            {
                let item_ty = type_args.first().cloned().unwrap_or(Type::Error);
                let is_peekable = name == "Peekable";
                return Some(self.infer_iterator_method(&item_ty, method, args, span, is_peekable));
            }
        }

        // Direct iterator TERMINALS on an iterable collection receiver —
        // `v.sum()` / `v.product()` / `v.max()` / `v.min()` without the
        // `.iter()` hop (B-2026-07-16-14). Pre-fix these fell through to the
        // silent unknown-method leniency (`Type::Error`, which unifies with
        // anything), so `karac check` passed programs every backend then
        // trapped on — a check/execution hole on the exact shapes LLM authors
        // write constantly. Route them through `infer_iterator_method` as if
        // `.iter()` were present: the terminal's span-keyed metadata
        // (`iter_terminal_elem_types` etc.) records against THIS call's span,
        // and the lowering desugar (`src/lowering.rs`) rewrites the AST to the
        // canonical `.iter().<terminal>()` chain the backends implement.
        // Scoped to the no-closure numeric/ordering terminals; `join`/`concat`
        // are Vec[String]-receiver METHODS handled in their own arm (they
        // never had an Iterator form).
        // Narrowed to Vec/VecDeque receivers: SortedMap/SortedSet (and Map)
        // have their OWN min/max surfaces (Option[(K, V)] pairs, sorted-order
        // first/last) with dedicated typing + lowering — routing them here
        // regressed test_sorted_map_min_max_return_option_pair on the first
        // battery run.
        if matches!(method, "sum" | "product" | "max" | "min") {
            let vec_like_item = match obj_ty {
                Type::Named { name, args: targs }
                    if matches!(name.as_str(), "Vec" | "VecDeque") && targs.len() == 1 =>
                {
                    Some(targs[0].clone())
                }
                Type::Ref(inner) | Type::MutRef(inner) => match inner.as_ref() {
                    Type::Named { name, args: targs }
                        if matches!(name.as_str(), "Vec" | "VecDeque") && targs.len() == 1 =>
                    {
                        Some(targs[0].clone())
                    }
                    _ => None,
                },
                _ => None,
            };
            if let Some(item_ty) = vec_like_item {
                // B-2026-08-11-19: tell the lowering desugar, from here, that
                // THIS call is a direct-on-collection terminal. It used to
                // re-derive that by reading `method_callee_types[span]` back —
                // but `span` is the receiver's span (see this function's
                // `args_close_span` doc), so every call in a chain shares one
                // key and the last write wins. `xs.max().unwrap().to_string()`
                // therefore recorded `i64.to_string` over `Vec.max`, the gate
                // saw a non-Vec head, no `.iter()` was inserted, and both
                // backends reported the raw `max` as an unsupported method.
                // Keyed on the closing paren, which is a leaf span no outer
                // expression aliases.
                self.direct_iter_terminals
                    .insert(SpanKey::from_span(&object.span));
                return Some(self.infer_iterator_method(&item_ty, method, args, span, false));
            }
        }

        // `Vec[String].join(sep) -> String` / `.concat() -> String` — the
        // string-collection terminals (B-2026-07-16-14's other half). These
        // are collection METHODS (no Iterator form): join places `sep`
        // between every adjacent pair (positionally — an empty first element
        // still gets a separator after it), concat is join with the empty
        // separator. Non-String elements are rejected here so the
        // check/execution contract holds (pre-fix these fell into the same
        // silent Type::Error leniency as the terminals above). VecDeque is
        // included — same layout, same runtime walk.
        if matches!(method, "join" | "concat") {
            let elem_is_str = match obj_ty {
                Type::Named { name, args: targs }
                    if matches!(name.as_str(), "Vec" | "VecDeque") && targs.len() == 1 =>
                {
                    Some(matches!(targs[0], Type::Str))
                }
                Type::Ref(inner) | Type::MutRef(inner) => match inner.as_ref() {
                    Type::Named { name, args: targs }
                        if matches!(name.as_str(), "Vec" | "VecDeque") && targs.len() == 1 =>
                    {
                        Some(matches!(targs[0], Type::Str))
                    }
                    _ => None,
                },
                _ => None,
            };
            if let Some(is_str) = elem_is_str {
                if !is_str {
                    self.type_error(
                        format!("Vec.{method}() requires String elements"),
                        *span,
                        TypeErrorKind::TypeMismatch,
                    );
                    for arg in args {
                        self.infer_expr(&arg.value);
                    }
                    return Some(Type::Error);
                }
                let expected_args = usize::from(method == "join");
                if args.len() != expected_args {
                    self.type_error(
                        format!(
                            "Vec.{method}() expects {expected_args} argument(s), found {}",
                            args.len()
                        ),
                        *span,
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                    for arg in args {
                        self.infer_expr(&arg.value);
                    }
                    return Some(Type::Str);
                }
                if method == "join" {
                    let sep_ty = self.infer_expr(&args[0].value);
                    self.check_assignable(&Type::Str, &sep_ty, args[0].value.span);
                }
                return Some(Type::Str);
            }
        }

        // `StdinLines` (`stdin.lines()`) and `LinesIter` (`BufReader.lines()`)
        // are opaque line-iterator markers with NO surface methods — iteration
        // is via `for line in <iter>` only (the drain/codegen loop pulls one
        // line per turn). Reject ANY method call on them LOUDLY: an adaptor
        // (`.map()`/`.filter()`/…) or terminal is not wired into the for-loop
        // materialization, so without this it either falls through to a silent
        // zero-iteration no-op (`LinesIter`) or an unhelpful generic "no method"
        // (`StdinLines`) — B-2026-07-11-34. Direct for-loop iteration does not
        // go through method dispatch, so it is unaffected.
        if let Type::Named { name, .. } = obj_ty {
            if name == "StdinLines" || name == "LinesIter" {
                self.type_error(
                    format!(
                        "`.{method}()` is not available on `{name}` — line iterators support no \
                         adaptors/terminals at v1; iterate directly with `for line in <iter>` and \
                         filter/map inside the loop body (each item is `Result[String, IoError]`)"
                    ),
                    *span,
                    TypeErrorKind::NoMethodFound,
                );
                for arg in args {
                    self.infer_expr(&arg.value);
                }
                return Some(Type::Error);
            }
        }

        // `Atomic[T].compare_exchange(old, new, success, failure) -> Result[T, T]`
        // (deferred.md § Atomic Operations, line 311). Special-cased because its
        // Result-shaped return must be visible to the typechecker so the caller
        // can `match` / `.is_ok()` on the outcome. The other atomic methods
        // (`load` / `store` / `fetch_*` / `swap`) are codegen-only and fall
        // through to the silent `Type::Error` arm below — their inner-type
        // return isn't modeled here, which is harmless because `Type::Error`
        // is universally assignable. `compare_exchange` can't ride that path:
        // a `Result`-typed scrutinee is needed for exhaustive matching.
        // Returns `Ok(prev)` on a successful swap, `Err(actual)` otherwise —
        // both payloads are `T`, hence `Result[T, T]`.
        if method == "compare_exchange" {
            let inner = match obj_ty {
                Type::Named { name, args } if name == "Atomic" && args.len() == 1 => {
                    Some(args[0].clone())
                }
                Type::Ref(b) | Type::MutRef(b) => match b.as_ref() {
                    Type::Named { name, args } if name == "Atomic" && args.len() == 1 => {
                        Some(args[0].clone())
                    }
                    _ => None,
                },
                _ => None,
            };
            if let Some(inner) = inner {
                if args.len() != 4 {
                    self.type_error(
                        format!(
                            "Atomic.compare_exchange expects (old, new, success: MemoryOrdering, \
                             failure: MemoryOrdering) — 4 arguments, found {}",
                            args.len()
                        ),
                        *span,
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                    for arg in args {
                        self.infer_expr(&arg.value);
                    }
                } else {
                    // old / new must be assignable to the atomic's inner type T;
                    // the two ordering args are inferred for recording (their
                    // `MemoryOrdering.X` shape is validated at codegen).
                    let old_ty = self.infer_expr(&args[0].value);
                    self.check_assignable(&inner, &old_ty, args[0].value.span);
                    let new_ty = self.infer_expr(&args[1].value);
                    self.check_assignable(&inner, &new_ty, args[1].value.span);
                    self.infer_expr(&args[2].value);
                    self.infer_expr(&args[3].value);
                }
                let result_ty = Type::Named {
                    name: "Result".to_string(),
                    args: vec![inner.clone(), inner],
                };
                self.record_expr_type(span, &result_ty);
                return Some(result_ty);
            }
        }

        // `Atomic[T]` load / store / read-modify-write ops — each takes an
        // explicit `MemoryOrdering` argument and has NO implicit-ordering
        // overload (deferred.md § Atomic Operations, lines 339–345):
        //   `load(ord) -> T`, `store(val, ord)`, and the RMW family
        //   `fetch_add` / `fetch_sub` / `fetch_and` / `fetch_or` /
        //   `fetch_xor` / `swap` — all `(val, ord) -> T`.
        // Without this arm these fell through to the silent `Type::Error`
        // catch-all below: arity went unchecked, so the implicit-ordering form
        // (`c.fetch_add(1)`) passed typecheck and ran fine under the
        // interpreter (which ignores the ordering) while codegen rejected it —
        // a run/build divergence (B-2026-06-30-5). Requiring the ordering here,
        // with a run-fatal `AtomicMissingOrdering`, makes `run` and `build`
        // agree: both reject the implicit form. Modeling the real return type
        // (`T` for load/RMW, `Unit` for store) also replaces the
        // universally-assignable `Type::Error` with the correct type. The
        // receiver gate (`Atomic[T]`, possibly behind a borrow) leaves the
        // same-named Vec/Slice `swap(i, j)` method untouched — it falls through
        // to its own handling below. `compare_exchange` (4 args, `Result`-typed)
        // is handled separately above. The ordering arg's `MemoryOrdering.X`
        // shape is validated at codegen.
        if matches!(
            method,
            "load"
                | "store"
                | "fetch_add"
                | "fetch_sub"
                | "fetch_and"
                | "fetch_or"
                | "fetch_xor"
                | "swap"
        ) {
            let inner = match obj_ty {
                Type::Named { name, args } if name == "Atomic" && args.len() == 1 => {
                    Some(args[0].clone())
                }
                Type::Ref(b) | Type::MutRef(b) => match b.as_ref() {
                    Type::Named { name, args } if name == "Atomic" && args.len() == 1 => {
                        Some(args[0].clone())
                    }
                    _ => None,
                },
                _ => None,
            };
            if let Some(inner) = inner {
                // `load` takes (ordering); `store` and every RMW op take
                // (value, ordering). Both forms require the trailing ordering.
                let want = if method == "load" { 1 } else { 2 };
                if args.len() != want {
                    let shape = if method == "load" {
                        "(ordering: MemoryOrdering)"
                    } else {
                        "(value, ordering: MemoryOrdering)"
                    };
                    self.type_error(
                        format!(
                            "Atomic.{method} takes {shape} — {want} argument{}; every atomic \
                             operation requires an explicit MemoryOrdering (there is no \
                             implicit-ordering form), found {}",
                            if want == 1 { "" } else { "s" },
                            args.len()
                        ),
                        *span,
                        TypeErrorKind::AtomicMissingOrdering,
                    );
                    for arg in args {
                        self.infer_expr(&arg.value);
                    }
                } else if method == "load" {
                    // The single argument is the ordering — inferred for
                    // recording; its `MemoryOrdering.X` shape is a codegen check.
                    self.infer_expr(&args[0].value);
                } else {
                    // store / RMW: the leading value must be assignable to the
                    // atomic's inner type `T`; the trailing ordering is inferred.
                    // (`swap` accepts any `T`, including `Atomic[bool]`.)
                    let val_ty = self.infer_expr(&args[0].value);
                    self.check_assignable(&inner, &val_ty, args[0].value.span);
                    self.infer_expr(&args[1].value);
                }
                let ret = if method == "store" { Type::Unit } else { inner };
                self.record_expr_type(span, &ret);
                return Some(ret);
            }
        }
        None
    }
}
