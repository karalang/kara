//! `Option` / `Result` combinator typechecking (B-2026-07-14-6).
//!
//! Third slice of the `infer_method_call` decomposition
//! ([`docs/spikes/state-decomposition-codegen-methodcall.md`]). Holds the
//! two adjacent combinator batches:
//!
//! - the **closure-free** subset — `ok` / `err` (`Result` -> `Option`), `or`,
//!   `and`, `ok_or` (`Option` -> `Result`), `flatten`, `take`,
//!   `get_or_insert`;
//! - the **closure-taking** siblings — `unwrap_or_else`, `map_or`,
//!   `map_or_else`, `map_err` (`Result`), `and_then`, `or_else`, `filter`
//!   (`Option`), each of which infers its closure argument, seeds the
//!   closure parameter from the receiver payload (present `T` / error `E`),
//!   reads the closure return, and shapes the result type from it.
//!
//! These are typing arms only: the interpreter arms live in
//! `method_call_optres.rs` and the codegen arms in `calls.rs`. The SOURCE
//! payload type is recorded in `method_unwrap_inner_types` (keyed by call
//! span) so codegen can reconstruct the receiver's payload words, mirroring
//! `map` / `unwrap`.
//!
//! The two batches keep the exact order they had inline, and the function is
//! called from the same position in `infer_method_call`'s first-match-wins
//! chain.
//!
//! Lives in a sibling `impl<'a> super::TypeChecker<'a>` block.

use crate::ast::*;
use crate::token::Span;

use crate::resolver::SpanKey;

use super::inference::resolve_type_var_top;
use super::types::{type_display, Type};
use super::TypeErrorKind;

impl<'a> super::TypeChecker<'a> {
    /// Type an `Option` / `Result` combinator call.
    ///
    /// Returns `Some(ty)` when this surface claims `method` (including
    /// `Some(Type::Error)` when it claims the name but the call is
    /// ill-formed and a diagnostic has been emitted), and `None` when the
    /// name belongs to some later link in the `infer_method_call` chain.
    pub(super) fn try_option_result_combinator(
        &mut self,
        method: &str,
        args: &[CallArg],
        span: &Span,
        args_close_span: &Span,
        obj_ty: &Type,
    ) -> Option<Type> {
        // ── Option / Result combinators, non-closure batch (B-2026-07-14-6) ──
        // A family of standard combinators the typechecker previously rejected
        // (no dedicated arm → `no method 'X' on Option/Result`) and which had no
        // runtime dispatch in either backend. Modelled here so they type
        // correctly; interpreter arms live in `method_call_optres.rs`, codegen
        // arms in `calls.rs`. The SOURCE payload type is recorded in
        // `method_unwrap_inner_types` (keyed by the call span) so codegen can
        // reconstruct the receiver's payload words, mirroring `map`/`unwrap`.
        // This batch is the CLOSURE-FREE subset: `ok`/`err` (Result→Option),
        // `or` (passthrough), `ok_or` (Option→Result), `flatten` (Option
        // un-nest). The closure-taking combinators are a separate arm below.
        if matches!(
            method,
            "ok" | "err" | "or" | "and" | "ok_or" | "flatten" | "take" | "get_or_insert"
        ) {
            let optres = |ty: &Type| -> Option<(bool, Type, Option<Type>)> {
                match ty {
                    Type::Named { name, args } if name == "Option" && args.len() == 1 => {
                        Some((false, args[0].clone(), None))
                    }
                    Type::Named { name, args } if name == "Result" && args.len() == 2 => {
                        Some((true, args[0].clone(), Some(args[1].clone())))
                    }
                    _ => None,
                }
            };
            let recv = match obj_ty {
                Type::Ref(i) | Type::MutRef(i) => optres(i),
                other => optres(other),
            };
            if let Some((is_result, t_ty, e_ty)) = recv {
                let opt = |payload: Type| Type::Named {
                    name: "Option".to_string(),
                    args: vec![payload],
                };
                let record_src = |s: &mut Self, ty: &Type| {
                    let resolved = resolve_type_var_top(ty, &s.env.substitutions);
                    s.method_unwrap_inner_types.insert(
                        SpanKey::for_method_call(span, args_close_span),
                        Self::type_to_type_expr(&resolved),
                    );
                };
                let result = match method {
                    // `Result[T, E].ok() -> Option[T]` / `.err() -> Option[E]`.
                    "ok" | "err" if is_result => {
                        if !args.is_empty() {
                            self.type_error(
                                format!("Result.{method} takes no arguments"),
                                span.clone(),
                                TypeErrorKind::WrongNumberOfArgs,
                            );
                        }
                        let payload = if method == "ok" {
                            t_ty.clone()
                        } else {
                            e_ty.clone().unwrap_or(Type::Error)
                        };
                        record_src(self, &payload);
                        Some(opt(resolve_type_var_top(&payload, &self.env.substitutions)))
                    }
                    // `Option[T].or(alt: Option[T]) -> Option[T]` /
                    // `Result[T,E].or(alt: Result[T,F]) -> Result[T,F]` — eager
                    // alternative, returned when the receiver is absent/err.
                    // `and` is the dual: `Option[T].and(other: Option[U]) ->
                    // Option[U]` / `Result[T,E].and(other: Result[U,E]) ->
                    // Result[U,E]` — the eager `other`, returned when the
                    // receiver is PRESENT (else the absent receiver passes
                    // through). Both take the argument's type as the result
                    // (its payload governs the present/other branch), kept
                    // permissive like `unwrap_or`.
                    "or" | "and" => {
                        let arg_ty = args
                            .first()
                            .map(|a| self.infer_expr(&a.value))
                            .unwrap_or(Type::Error);
                        record_src(self, &t_ty);
                        Some(resolve_type_var_top(&arg_ty, &self.env.substitutions))
                    }
                    // `Option[T].ok_or(err: E) -> Result[T, E]` — eager error.
                    "ok_or" if !is_result => {
                        let err_ty = args
                            .first()
                            .map(|a| self.infer_expr(&a.value))
                            .unwrap_or(Type::Error);
                        record_src(self, &t_ty);
                        Some(Type::Named {
                            name: "Result".to_string(),
                            args: vec![
                                resolve_type_var_top(&t_ty, &self.env.substitutions),
                                resolve_type_var_top(&err_ty, &self.env.substitutions),
                            ],
                        })
                    }
                    // `Option[Option[U]].flatten() -> Option[U]`.
                    "flatten" if !is_result => {
                        if !args.is_empty() {
                            self.type_error(
                                "Option.flatten takes no arguments".to_string(),
                                span.clone(),
                                TypeErrorKind::WrongNumberOfArgs,
                            );
                        }
                        let inner = resolve_type_var_top(&t_ty, &self.env.substitutions);
                        match &inner {
                            Type::Named { name, args } if name == "Option" && args.len() == 1 => {
                                record_src(self, &inner);
                                Some(opt(args[0].clone()))
                            }
                            _ => {
                                self.type_error(
                                    format!(
                                        "Option.flatten requires an `Option[Option[T]]` \
                                         receiver, found `Option[{}]`",
                                        type_display(&inner)
                                    ),
                                    span.clone(),
                                    TypeErrorKind::TypeMismatch,
                                );
                                Some(Type::Error)
                            }
                        }
                    }
                    // `Option[T].take() -> Option[T]` — MUTATING: returns the
                    // receiver's current value and leaves `None` in its place.
                    // Receiver-mutation is seeded in the effectchecker builtin
                    // table (`Option.take`) so the auto-par write-dependency
                    // gate serializes it against sibling reads (the
                    // B-2026-07-14-17 standing rule for in-place mutators).
                    "take" if !is_result => {
                        if !args.is_empty() {
                            self.type_error(
                                "Option.take takes no arguments".to_string(),
                                span.clone(),
                                TypeErrorKind::WrongNumberOfArgs,
                            );
                        }
                        record_src(self, &t_ty);
                        Some(opt(resolve_type_var_top(&t_ty, &self.env.substitutions)))
                    }
                    // `Option[T].get_or_insert(v: T) -> T` — MUTATING: inserts
                    // `Some(v)` when the receiver is `None`, then yields the
                    // (now guaranteed-present) payload. Kāra models the result
                    // BY VALUE (a copy of the payload), not Rust's `&mut T` —
                    // a mut-ref result needs place-ref machinery deferred with
                    // the rest of that surface. Mutation is seeded in the
                    // effectchecker table (`Option.get_or_insert`).
                    "get_or_insert" if !is_result => {
                        if let Some(a) = args.first() {
                            let at = self.infer_expr(&a.value);
                            self.check_assignable(&t_ty, &at, a.value.span.clone());
                        } else {
                            self.type_error(
                                "Option.get_or_insert expects 1 argument".to_string(),
                                span.clone(),
                                TypeErrorKind::WrongNumberOfArgs,
                            );
                        }
                        record_src(self, &t_ty);
                        Some(resolve_type_var_top(&t_ty, &self.env.substitutions))
                    }
                    _ => None,
                };
                if let Some(result) = result {
                    self.record_expr_type(span, &result);
                    return Some(result);
                }
            }
        }

        // ── Option / Result combinators, CLOSURE batch (B-2026-07-14-6) ──────
        // The closure-taking siblings of the non-closure block above:
        // `unwrap_or_else`, `map_or`, `map_or_else`, `map_err` (Result),
        // `and_then`, `or_else`, `filter` (Option). Each infers its closure
        // argument, seeds the closure's parameter from the receiver's payload
        // (present `T` / error `E`), reads the closure's return, and shapes the
        // result type accordingly. The SOURCE payload `T` is recorded in
        // `method_unwrap_inner_types` for codegen payload reconstruction (as
        // `map` does). Interpreter arms in `method_call_optres.rs`, codegen in
        // `calls.rs`.
        if matches!(
            method,
            "unwrap_or_else"
                | "map_or"
                | "map_or_else"
                | "map_err"
                | "and_then"
                | "or_else"
                | "filter"
        ) {
            let optres = |ty: &Type| -> Option<(bool, Type, Option<Type>)> {
                match ty {
                    Type::Named { name, args } if name == "Option" && args.len() == 1 => {
                        Some((false, args[0].clone(), None))
                    }
                    Type::Named { name, args } if name == "Result" && args.len() == 2 => {
                        Some((true, args[0].clone(), Some(args[1].clone())))
                    }
                    _ => None,
                }
            };
            let recv = match obj_ty {
                Type::Ref(i) | Type::MutRef(i) => optres(i),
                other => optres(other),
            };
            if let Some((is_result, t_ty, e_ty)) = recv {
                let e_ty = e_ty.unwrap_or(Type::Error);
                // Two closure-checking strategies:
                //  - `check_closure` (used when the closure's RETURN is already
                //    known — `filter`'s `bool`, `unwrap_or_else`'s payload `T`):
                //    `check_expr` against a fully-concrete `Fn(params) -> ret`
                //    SEEDS the closure's parameter, so an un-annotated `|x| x > 3`
                //    predicate type-checks against `T` (the `infer_expr`-then-
                //    unify order left `x` unsolved and `x > 3` failed as "cannot
                //    compare '?T' and 'i64'").
                //  - `infer_closure_ret` (used when the return is UNKNOWN —
                //    `map_or`/`map_err`/`and_then`/…): infer the closure, unify
                //    its first param with the seed, read the resolved return.
                //    Same limitation `map` has: an un-annotated param is only
                //    inferred for a body that unifies it (arithmetic), not a bare
                //    comparison — annotate `|x: T|` for those.
                let check_closure = |s: &mut Self, arg: &CallArg, params: Vec<Type>, ret: Type| {
                    let f_ty = Type::Function {
                        params,
                        return_type: Box::new(ret),
                    };
                    s.check_expr(&arg.value, &f_ty);
                };
                let infer_closure_ret =
                    |s: &mut Self, arg: &CallArg, seed: Option<&Type>| -> Type {
                        // B-2026-07-15-16: publish the param seed for the
                        // closure's un-annotated param, then infer the body
                        // FREELY (no return-type expectation). Seeding-then-
                        // free-infer binds a `?T` param to the receiver's payload
                        // up front — so `r.and_then(|x| x > 0)` /
                        // `v.retain(|x| x != 3)` stop failing "cannot compare
                        // '?T0' and 'i64'" — while a wrapper-returning body
                        // (`Ok(..)` / `Some(..)`) still infers its own payload and
                        // the enclosing context binds the rest. (Check-mode with a
                        // fresh return var leaves a bare constructor body
                        // un-adoptable.) An explicit param annotation wins in the
                        // synth-mode closure arm. No seed (a zero-param
                        // absent-branch closure, `Option.or_else(|| …)`) → the
                        // seed insert is skipped and the body infers as before.
                        if let (ExprKind::Closure { .. }, Some(seed)) = (&arg.value.kind, seed) {
                            s.closure_param_seeds
                                .insert(SpanKey::from_span(&arg.value.span), vec![seed.clone()]);
                        }
                        let f_actual = s.infer_expr(&arg.value);
                        let f_resolved = resolve_type_var_top(&f_actual, &s.env.substitutions);
                        match &f_resolved {
                            Type::Function {
                                params: _,
                                return_type,
                            }
                            | Type::OnceFunction {
                                params: _,
                                return_type,
                            } => resolve_type_var_top(return_type, &s.env.substitutions),
                            _ => Type::Error,
                        }
                    };
                // Record the payload type codegen reconstructs. `map_err` maps
                // over the `Err` payload (`Ok` passes through untouched), so it
                // records `E`; every other combinator reconstructs the present
                // payload `T`.
                let recorded_payload = if method == "map_err" {
                    resolve_type_var_top(&e_ty, &self.env.substitutions)
                } else {
                    resolve_type_var_top(&t_ty, &self.env.substitutions)
                };
                self.method_unwrap_inner_types.insert(
                    SpanKey::for_method_call(span, args_close_span),
                    Self::type_to_type_expr(&recorded_payload),
                );
                // The RESULT forms of the absent-closure combinators pass the
                // `Err` value `e` to that closure, so codegen additionally needs
                // `E` — recorded in the sibling table (the present-payload slot
                // above already holds `T` for these methods).
                if is_result && matches!(method, "unwrap_or_else" | "map_or_else" | "or_else") {
                    let e_resolved = resolve_type_var_top(&e_ty, &self.env.substitutions);
                    self.method_unwrap_err_types.insert(
                        SpanKey::for_method_call(span, args_close_span),
                        Self::type_to_type_expr(&e_resolved),
                    );
                }
                let opt = |payload: Type| Type::Named {
                    name: "Option".to_string(),
                    args: vec![payload],
                };
                // The closure's param list for the ABSENT branch (`unwrap_or_else`
                // / `map_or_else` default / `or_else`): none for Option, the error
                // `E` for Result.
                let absent_params = || {
                    if is_result {
                        vec![e_ty.clone()]
                    } else {
                        vec![]
                    }
                };
                let result: Option<Type> = match method {
                    // `unwrap_or_else(f)` — present payload, else `f()`/`f(e)`. → T.
                    // Return is the known payload `T`, so `check_closure` seeds
                    // the absent-branch param (`E` for Result) precisely.
                    "unwrap_or_else" => {
                        let t = resolve_type_var_top(&t_ty, &self.env.substitutions);
                        if let Some(a) = args.first() {
                            check_closure(self, a, absent_params(), t.clone());
                        }
                        Some(t)
                    }
                    // `map_or(default, f)` — `f(T)` if present, else `default`. → U.
                    "map_or" => {
                        let default_ty = args
                            .first()
                            .map(|a| self.infer_expr(&a.value))
                            .unwrap_or(Type::Error);
                        let r = args
                            .get(1)
                            .map(|a| infer_closure_ret(self, a, Some(&t_ty)))
                            .unwrap_or(default_ty);
                        Some(resolve_type_var_top(&r, &self.env.substitutions))
                    }
                    // `map_or_else(default_fn, f)` — `f(T)` if present, else
                    // `default_fn()`/`default_fn(e)`. → U (the mapper's return).
                    "map_or_else" => {
                        let r = args
                            .get(1)
                            .map(|a| infer_closure_ret(self, a, Some(&t_ty)))
                            .unwrap_or(Type::Error);
                        let r = resolve_type_var_top(&r, &self.env.substitutions);
                        // Seed the default_fn against the now-known result type.
                        if let Some(a) = args.first() {
                            check_closure(self, a, absent_params(), r.clone());
                        }
                        Some(r)
                    }
                    // `Result[T,E].map_err(f)` — `Err(f(e))`; `Ok` passes through.
                    // → `Result[T, F]`.
                    "map_err" if is_result => {
                        let f_ret = args
                            .first()
                            .map(|a| infer_closure_ret(self, a, Some(&e_ty)))
                            .unwrap_or(Type::Error);
                        Some(Type::Named {
                            name: "Result".to_string(),
                            args: vec![
                                resolve_type_var_top(&t_ty, &self.env.substitutions),
                                resolve_type_var_top(&f_ret, &self.env.substitutions),
                            ],
                        })
                    }
                    // `and_then(f)` — `f(T)` (itself an Option/Result) if present,
                    // else the absent receiver. → the closure's return type.
                    "and_then" => {
                        let r = args
                            .first()
                            .map(|a| infer_closure_ret(self, a, Some(&t_ty)))
                            .unwrap_or(Type::Error);
                        Some(resolve_type_var_top(&r, &self.env.substitutions))
                    }
                    // `or_else(f)` — present receiver, else `f()`/`f(e)` (itself
                    // an Option/Result). → the closure's return type.
                    "or_else" => {
                        let r = args
                            .first()
                            .map(|a| infer_closure_ret(self, a, absent_params().first()))
                            .unwrap_or_else(|| obj_ty.clone());
                        Some(resolve_type_var_top(&r, &self.env.substitutions))
                    }
                    // `Option[T].filter(pred)` — `Some(x)` kept iff `pred(x)`,
                    // else `None`. → `Option[T]`. Return is `bool`, so
                    // `check_closure` seeds `pred`'s param as `T`.
                    "filter" if !is_result => {
                        if let Some(a) = args.first() {
                            check_closure(self, a, vec![t_ty.clone()], Type::Bool);
                        }
                        Some(opt(resolve_type_var_top(&t_ty, &self.env.substitutions)))
                    }
                    _ => None,
                };
                if let Some(result) = result {
                    self.record_expr_type(span, &result);
                    return Some(result);
                }
            }
        }
        None
    }
}
