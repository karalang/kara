//! Iterator-adaptor fusion: the `.iter().map(..).filter(..)…` chain
//! recognizers and their fused single-loop lowerings (collect / fold / sum /
//! count / reduce / for_each / any / all / position / find / partition /
//! last / nth / flat_map / flatten / chunks / windows / zip / cycle / scan).
//!
//! Extracted verbatim from `method_call.rs` (structural-debt second-level
//! split). Sibling `impl<'ctx> super::Codegen<'ctx>` block; moved methods
//! are `pub(super)`.

use super::method_call::*;
use crate::ast::*;

use inkwell::values::BasicValueEnum;

impl<'ctx> super::Codegen<'ctx> {
    /// Lower `<string>.chars().collect()` to the already-supported
    /// `for c in <string>.chars() { v.push(c) }` build and compile that
    /// (B-2026-06-18-1, kata:38). `chars_call` is the `<string>.chars()`
    /// expression (the `collect` receiver), reused verbatim as the loop
    /// iterable. We synthesize the block
    ///
    /// ```text
    /// { let mut __cas_N: Vec[char] = Vec.new();
    ///   for __casc_N in <string>.chars() { __cas_N.push(__casc_N); }
    ///   __cas_N }
    /// ```
    ///
    /// and hand it to `compile_expr`. The `Vec[char]` annotation makes the
    /// let-binding handler register the element type at codegen time (no
    /// typechecker dependency — see `stmts.rs` let lowering), so `push`
    /// dispatches and the result is a usable `Vec[char]`. Reusing the
    /// existing for-chars + push + block-return paths means no new low-level
    /// Vec/iterator codegen, and the block's move-out gives the caller the
    /// freshly built Vec exactly as a `fn() -> Vec[char]` would.
    pub(super) fn compile_chars_collect_to_vec(
        &mut self,
        chars_call: &Expr,
        call_span: &crate::token::Span,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let n = self.indexed_elem_counter;
        self.indexed_elem_counter += 1;
        let vec_name = format!("__cas_{}", n);
        let char_name = format!("__casc_{}", n);
        let sp = *call_span;

        let ident = |name: &str, sp: &crate::token::Span| Expr {
            kind: ExprKind::Identifier(name.to_string()),
            span: *sp,
        };

        // Vec[char] type annotation.
        let char_ty = TypeExpr {
            kind: TypeKind::Path(PathExpr {
                segments: vec!["char".to_string()],
                generic_args: None,
                span: sp,
            }),
            span: sp,
        };
        let vec_char_ty = TypeExpr {
            kind: TypeKind::Path(PathExpr {
                segments: vec!["Vec".to_string()],
                generic_args: Some(vec![GenericArg::Type(char_ty)]),
                span: sp,
            }),
            span: sp,
        };

        // `Vec.new()`
        let vec_new = Expr {
            kind: ExprKind::Call {
                callee: Box::new(Expr {
                    kind: ExprKind::Path {
                        segments: vec!["Vec".to_string(), "new".to_string()],
                        generic_args: None,
                    },
                    span: sp,
                }),
                args: vec![],
            },
            span: sp,
        };

        // `let mut __cas_N: Vec[char] = Vec.new();`
        let let_stmt = Stmt {
            kind: StmtKind::Let {
                is_mut: true,
                pattern: Pattern {
                    kind: PatternKind::Binding(vec_name.clone()),
                    span: sp,
                },
                ty: Some(vec_char_ty),
                value: vec_new,
            },
            span: sp,
        };

        // `__cas_N.push(__casc_N)`
        let push_call = Expr {
            kind: ExprKind::MethodCall {
                object: Box::new(ident(&vec_name, &sp)),
                method: "push".to_string(),
                turbofish: None,
                args: vec![CallArg {
                    label: None,
                    mut_marker: false,
                    mut_marker_span: None,
                    value: ident(&char_name, &sp),
                    span: sp,
                }],
                args_close_span: sp,
            },
            span: sp,
        };

        // `for __casc_N in <string>.chars() { __cas_N.push(__casc_N); }`
        let for_stmt = Stmt {
            kind: StmtKind::Expr(Expr {
                kind: ExprKind::For {
                    label: None,
                    pattern: Pattern {
                        kind: PatternKind::Binding(char_name.clone()),
                        span: sp,
                    },
                    iterable: Box::new(chars_call.clone()),
                    body: Block {
                        stmts: vec![Stmt {
                            kind: StmtKind::Expr(push_call),
                            span: sp,
                        }],
                        final_expr: None,
                        span: sp,
                    },
                    attributes: vec![],
                },
                span: sp,
            }),
            span: sp,
        };

        // `{ <let>; <for>; __cas_N }`
        let block = Expr {
            kind: ExprKind::Block(Block {
                stmts: vec![let_stmt, for_stmt],
                final_expr: Some(Box::new(ident(&vec_name, &sp))),
                span: sp,
            }),
            span: sp,
        };

        self.compile_expr(&block)
    }

    /// Lower `<iter>.map(f)/.filter(p)....collect()` into a materialized
    /// `Vec[U]` by desugaring the adaptor chain to an equivalent `for` loop
    /// that pushes each surviving/transformed element onto a fresh `Vec`
    /// (B-2026-07-03-25). Every construct the desugar produces —
    /// `for x in <src>`, closure-body inlining via `let <param> = <elem>`,
    /// `if <pred> { … }`, `push`, block move-out — is already fully supported,
    /// so no new low-level iterator/`collect` codegen is needed. `collect_recv`
    /// is the outermost `map`/`filter` MethodCall (the `collect` receiver);
    /// `call_span` is the whole `collect()` expression's span, whose
    /// `owned_temp_drops` entry (populated for every `Vec`-typed expr) carries
    /// the collected element type `U`.
    ///
    /// We synthesize, for a `.map(|a| ma)` chain over base `S`:
    ///
    /// ```text
    /// { let mut __icv_N: Vec[U] = Vec.new();
    ///   for __ice_N in S {
    ///     let __icm_N_0 = { let a = __ice_N; ma };
    ///     __icv_N.push(__icm_N_0);
    ///   }
    ///   __icv_N }
    /// ```
    ///
    /// A `filter(|a| pa)` step becomes `if { let a = <cur>; pa } { <rest> }`
    /// wrapping the downstream stages, so a rejected element is simply not
    /// pushed. Each `map` materializes into a fresh `let` so the threaded
    /// "current element" is always a simple identifier (no closure body is
    /// re-evaluated).
    ///
    /// Returns `Ok(None)` — the caller falls through to the dispatch-fail
    /// diagnostic — for any chain this can't faithfully lower: a non-`map`/
    /// `filter` adaptor in the chain (`enumerate`, `zip`, `take`, …), a
    /// `map`/`filter` argument that isn't a single-`Binding`-param closure, no
    /// `map`/`filter` step at all (plain `.iter().collect()`), or a missing/
    /// non-`Vec` recorded output type. Unsupported shapes therefore fail loudly
    /// rather than miscompile.
    /// Split `<src>.map(|x| f"…").<rest>.collect()` at the OUTERMOST non-terminal
    /// f-string map (B-2026-07-04-2 sub-part 3). Returns `Ok(None)` when the
    /// chain has no non-terminal f-string map (the caller then peels normally).
    /// Emitted when it does:
    ///
    /// ```text
    /// { let __ft: Vec[String] = <prefix ending at the f-string map>.collect();
    ///   <rest re-applied to __ft.iter()>.collect() }
    /// ```
    ///
    /// The prefix's f-string map is now the LAST adaptor, so its `.collect()`
    /// takes the leak-clean terminal `push(f"…")` path; the suffix continues
    /// over a plain `Vec[String]` binding. The prefix collect result is always
    /// `Vec[String]` (an f-string yields a `String`), registered under a fresh
    /// `usize::MAX`-based synthetic span; the suffix collect keeps the original
    /// call span (the final result type). Recurses for a nested f-string map in
    /// the prefix.
    pub(super) fn try_split_nonterminal_fstring_map_collect(
        &mut self,
        collect_recv: &Expr,
        call_span: &crate::token::Span,
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        fn body_tail_is_fstring(e: &Expr) -> bool {
            match &e.kind {
                ExprKind::InterpolatedStringLit(_) => true,
                ExprKind::Block(b) => b.final_expr.as_deref().is_some_and(body_tail_is_fstring),
                ExprKind::If {
                    then_block,
                    else_branch,
                    ..
                } => {
                    then_block
                        .final_expr
                        .as_deref()
                        .is_some_and(body_tail_is_fstring)
                        || else_branch.as_deref().is_some_and(body_tail_is_fstring)
                }
                _ => false,
            }
        }
        let is_fstring_map = |e: &Expr| -> bool {
            matches!(&e.kind, ExprKind::MethodCall { method, args, .. }
                if method == "map" && args.len() == 1
                && matches!(&args[0].value.kind, ExprKind::Closure { params, body, .. }
                    if params.len() == 1 && body_tail_is_fstring(body)))
        };
        // Walk outer → inner, recording the adaptors ABOVE the f-string map,
        // until we reach the outermost f-string map (the split point).
        let mut above: Vec<&Expr> = Vec::new();
        let mut cur = collect_recv;
        while !is_fstring_map(cur) {
            match &cur.kind {
                ExprKind::MethodCall { object, .. } => {
                    above.push(cur);
                    cur = object;
                }
                _ => return Ok(None), // no f-string map in the chain
            }
        }
        // Terminal f-string map (nothing above it) already lowers via the normal
        // `push(f"…")` path — no split needed.
        if above.is_empty() {
            return Ok(None);
        }
        let prefix = cur.clone(); // chain rooted AT the f-string map (inclusive)

        let uid = self.indexed_elem_counter;
        self.indexed_elem_counter += 1;
        let sp = *call_span;
        let ft_name = format!("__ft_{}", uid);
        let ident = |name: &str| Expr {
            kind: ExprKind::Identifier(name.to_string()),
            span: sp,
        };
        // `Vec[String]` for the prefix-collect temp.
        let string_te = TypeExpr {
            kind: TypeKind::Path(PathExpr {
                segments: vec!["String".to_string()],
                generic_args: None,
                span: sp,
            }),
            span: sp,
        };
        let vec_string_te = TypeExpr {
            kind: TypeKind::Path(PathExpr {
                segments: vec!["Vec".to_string()],
                generic_args: Some(vec![GenericArg::Type(string_te)]),
                span: sp,
            }),
            span: sp,
        };
        // Synthetic span for the prefix `.collect()` result type (`Vec[String]`).
        let prefix_span = crate::token::Span {
            line: sp.line,
            column: sp.column,
            offset: usize::MAX - (uid as usize) - 1,
            length: 1,
        };
        self.drop_rc.owned_temp_drops.insert(
            (prefix_span.offset, prefix_span.length),
            vec_string_te.clone(),
        );
        // `<prefix>.collect()` at the synthetic span.
        let prefix_collect = Expr {
            kind: ExprKind::MethodCall {
                object: Box::new(prefix),
                method: "collect".to_string(),
                turbofish: None,
                args: vec![],
                args_close_span: prefix_span,
            },
            span: prefix_span,
        };
        let let_ft = Stmt {
            kind: StmtKind::Let {
                is_mut: false,
                pattern: Pattern {
                    kind: PatternKind::Binding(ft_name.clone()),
                    span: sp,
                },
                ty: Some(vec_string_te),
                value: prefix_collect,
            },
            span: sp,
        };
        // Re-apply the ABOVE adaptors to `__ft.iter()` (innermost-above first),
        // then `.collect()` at the ORIGINAL call span (the final result type).
        let mut suffix = Expr {
            kind: ExprKind::MethodCall {
                object: Box::new(ident(&ft_name)),
                method: "iter".to_string(),
                turbofish: None,
                args: vec![],
                args_close_span: sp,
            },
            span: sp,
        };
        for call in above.iter().rev() {
            if let ExprKind::MethodCall {
                method,
                args,
                turbofish,
                args_close_span,
                ..
            } = &call.kind
            {
                suffix = Expr {
                    kind: ExprKind::MethodCall {
                        object: Box::new(suffix),
                        method: method.clone(),
                        turbofish: turbofish.clone(),
                        args: args.clone(),
                        args_close_span: *args_close_span,
                    },
                    span: sp,
                };
            }
        }
        let suffix_collect = Expr {
            kind: ExprKind::MethodCall {
                object: Box::new(suffix),
                method: "collect".to_string(),
                turbofish: None,
                args: vec![],
                args_close_span: sp,
            },
            span: sp,
        };
        let block = Expr {
            kind: ExprKind::Block(Block {
                stmts: vec![let_ft],
                final_expr: Some(Box::new(suffix_collect)),
                span: sp,
            }),
            span: sp,
        };
        Ok(Some(self.compile_expr(&block)?))
    }

    pub(super) fn try_compile_iter_adaptor_collect_to_vec(
        &mut self,
        collect_recv: &Expr,
        call_span: &crate::token::Span,
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        // Walk the chain outer → inner, peeling `map`/`filter` adaptors until we
        // reach the base iterable (`s.iter()`, a range, an array literal, …).
        // `steps` is collected outermost-first, then reversed to application
        // order (base → out).
        enum IterAdaptor {
            Map { param: String, body: Expr },
            Filter { param: String, pred: Expr },
            // Stateful passthrough adaptors — element type is unchanged, so they
            // thread the current element straight through with a pre-loop state
            // variable (a counter or a latch) gating the downstream stages.
            // `Take`/`Skip`/`StepBy` carry an integer *count* expression (bound
            // once before the loop); `TakeWhile`/`SkipWhile`/`Inspect` carry a
            // single-`Binding`-param closure like `filter`/`map`.
            Take { count: Expr },
            Skip { count: Expr },
            StepBy { count: Expr },
            TakeWhile { param: String, pred: Expr },
            SkipWhile { param: String, pred: Expr },
            Inspect { param: String, body: Expr },
            // Element-retyping adaptor: `enumerate()` pairs each element with a
            // running index, changing the element type `T` → `(i64, T)`. No
            // argument.
            Enumerate,
        }

        // B-2026-07-04-2 sub-part 1 (chain): `A.chain(B).collect()` where A and
        // B are each a plain identity SOURCE — a no-arg `.iter()` call or a
        // bounded range — concatenates the two into one Vec. Emit the identity
        // collect loop TWICE into a shared accumulator (`for x in A { acc.push
        // x }; for y in B { acc.push y }`): the same clone semantics as a single
        // identity collect (both borrowed sources survive), applied per source
        // in sequence. This is the sequential, single-loop-per-source multi-
        // source shape — cheap and safe. A side that carries its OWN adaptors
        // (`a.iter().map(f).chain(…)`), a nested chain, or a non-identity source
        // bails to the loud dispatch-fail (never a miscompile); those broaden
        // the surface (per-side pipelines / shared accumulator refactor) and
        // stay OPEN under sub-part 1.
        if let ExprKind::MethodCall {
            object,
            method,
            args,
            ..
        } = &collect_recv.kind
        {
            // B-2026-07-04-2 sub-part 1 (scan): `<src>.scan(init, |acc, x|
            // <body -> Option[(A, U)]>).collect()` threads a running accumulator
            // and collects each `Some` output, stopping on the first `None`.
            // Lowered with `.is_none()` / `.unwrap()` (no Option pattern-match,
            // which would be fragile in post-resolver synthetic AST).
            if method == "scan" && args.len() == 2 {
                if let ExprKind::Closure { params, body, .. } = &args[1].value.kind {
                    if params.len() == 2 {
                        if let (PatternKind::Binding(acc_p), PatternKind::Binding(x_p)) =
                            (&params[0].pattern.kind, &params[1].pattern.kind)
                        {
                            let src_is_identity = matches!(&object.kind,
                                ExprKind::MethodCall { method, args, .. }
                                    if args.is_empty() && (method == "iter" || method == "into_iter"))
                                || matches!(
                                    &object.kind,
                                    ExprKind::Range {
                                        start: Some(_),
                                        end: Some(_),
                                        ..
                                    }
                                );
                            if src_is_identity {
                                if let Some(v) = self.try_compile_scan_collect(
                                    object.as_ref(),
                                    &args[0].value,
                                    acc_p,
                                    x_p,
                                    body,
                                    call_span,
                                )? {
                                    return Ok(Some(v));
                                }
                                return Ok(None);
                            }
                        }
                    }
                }
            }
            if method == "chain" && args.len() == 1 {
                let is_identity_source = |e: &Expr| {
                    matches!(
                        &e.kind,
                        ExprKind::MethodCall { method, args, .. }
                            if args.is_empty() && method == "iter"
                    ) || matches!(
                        &e.kind,
                        ExprKind::Range {
                            start: Some(_),
                            end: Some(_),
                            ..
                        }
                    )
                };
                let src_a = object.as_ref();
                let src_b = &args[0].value;
                if is_identity_source(src_a) && is_identity_source(src_b) {
                    if let Some(v) =
                        self.try_compile_chain_identity_collect(src_a, src_b, call_span)?
                    {
                        return Ok(Some(v));
                    }
                    return Ok(None);
                }
                // Adaptor-CARRYING side(s): `A.iter().map(f).chain(B).collect()`,
                // `A.chain(B.iter().filter(g)).collect()`, etc. Recursively
                // collect each side through the full pipeline and merge into a
                // shared accumulator (B-2026-07-04-2 sub-part 1). Each side must
                // itself be a collectable adaptor chain over an identity base;
                // otherwise bail to the loud dispatch-fail.
                if let Some(v) = self.try_compile_chain_pipeline_collect(src_a, src_b, call_span)? {
                    return Ok(Some(v));
                }
                return Ok(None);
            }
            // B-2026-07-04-2 sub-part 1 (zip): `A.iter().zip(B.iter()).collect()`
            // pairs the two sources element-wise into a `Vec[(EA, EB)]`, stopping
            // at the shorter (min length). Both sides must be a `<indexable>
            // .iter()` so the emitted loop can index the underlying base
            // (`base[i]`); the paired push `acc.push((A[i], B[i]))` clones each
            // element (sources survive). Downstream adaptors after `zip`
            // (`zip().map(…)`) and non-`.iter()` sources bail to the loud
            // dispatch-fail — never a miscompile — and stay OPEN under sub-part 1.
            if method == "zip" && args.len() == 1 {
                let iter_base = |e: &Expr| -> Option<Expr> {
                    match &e.kind {
                        ExprKind::MethodCall {
                            object,
                            method,
                            args,
                            ..
                        } if args.is_empty() && method == "iter" => Some((**object).clone()),
                        _ => None,
                    }
                };
                if let (Some(base_a), Some(base_b)) =
                    (iter_base(object.as_ref()), iter_base(&args[0].value))
                {
                    if let Some(v) =
                        self.try_compile_zip_identity_collect(&base_a, &base_b, call_span)?
                    {
                        return Ok(Some(v));
                    }
                    return Ok(None);
                }
                // Adaptor-CARRYING side(s): `A.iter().map(f).zip(B.iter())
                // .collect()`, `A.iter().zip(B.iter().filter(g)).collect()`, etc.
                // Pre-collect each side to a typed temp, then reuse the identity
                // zip on the two temps (B-2026-07-04-2 sub-part 1).
                if let Some(v) = self.try_compile_zip_pipeline_collect(
                    object.as_ref(),
                    &args[0].value,
                    call_span,
                )? {
                    return Ok(Some(v));
                }
                return Ok(None);
            }
            // B-2026-07-04-2 sub-part 1 (flat_map): `<outer>.flat_map(|p|
            // <inner>).collect()` maps each outer element to an inner iterable
            // and flattens the results into one Vec. Lower to NESTED loops —
            // `for p in <outer> { for x in <inner> { acc.push(x) } }` — reusing
            // the closure param `p` as the outer loop var so the inner iterable
            // `<inner>` (which references `p`) resolves. Iteration-based (not
            // index-based), so `push` clones and it is heap-safe like the other
            // identity collects. Scoped to an identity `<outer>` (`.iter()` /
            // `.into_iter()` / bounded range) and an identity `<inner>` (the
            // closure body is a `.iter()` / `.into_iter()` call or a bounded
            // range); any richer shape (a mapped/filtered inner, a downstream
            // adaptor after flat_map, a multi-param closure) bails to the loud
            // dispatch-fail and stays OPEN under sub-part 1.
            if method == "flat_map" && args.len() == 1 {
                let is_identity_source = |e: &Expr| {
                    matches!(
                        &e.kind,
                        ExprKind::MethodCall { method, args, .. }
                            if args.is_empty() && (method == "iter" || method == "into_iter")
                    ) || matches!(
                        &e.kind,
                        ExprKind::Range {
                            start: Some(_),
                            end: Some(_),
                            ..
                        }
                    )
                };
                if let ExprKind::Closure { params, body, .. } = &args[0].value.kind {
                    if params.len() == 1 {
                        if let PatternKind::Binding(param) = &params[0].pattern.kind {
                            if is_identity_source(object.as_ref()) && is_identity_source(body) {
                                if let Some(v) = self.try_compile_flat_map_collect(
                                    object.as_ref(),
                                    param,
                                    body,
                                    call_span,
                                )? {
                                    return Ok(Some(v));
                                }
                                return Ok(None);
                            }
                            // Adaptor-CARRYING outer with an identity inner that
                            // iterates the PARAM as a container (`param.iter()` /
                            // `param.into_iter()`): pre-collect the outer to a
                            // typed temp, then reuse the identity flat_map. The
                            // outer element type is `Vec[E]` (= the flattened
                            // result type), derivable only for this param-as-
                            // container inner — a range inner (`|p| 0..p`) makes
                            // the outer element a scalar, not derivable, so it
                            // stays gated. B-2026-07-04-2 sub-part 1.
                            let inner_iterates_param = matches!(&body.kind,
                                ExprKind::MethodCall { object, method, args, .. }
                                    if args.is_empty()
                                        && (method == "iter" || method == "into_iter")
                                        && matches!(&object.kind,
                                            ExprKind::Identifier(n) if n == param));
                            if !is_identity_source(object.as_ref()) && inner_iterates_param {
                                if let Some(v) = self.try_compile_flat_map_pipeline_collect(
                                    object.as_ref(),
                                    param,
                                    body,
                                    call_span,
                                )? {
                                    return Ok(Some(v));
                                }
                                return Ok(None);
                            }
                        }
                    }
                }
            }
            // B-2026-07-15-10 (zip→map): `A.iter().zip(B.iter()).map(f).collect()`
            // — a `map` whose base iterable is a `zip`. The general adaptor walk
            // below only accepts a plain-`.iter()`/range base, so a `zip` base
            // bailed loud. Reuse the (now-supported) for-loop over `zip` by
            // rewriting to `{ let mut acc: Vec[R] = Vec.new(); for <p> in <zip>
            // { acc.push(<f body>); } acc }`, binding the map closure's param
            // over the zipped tuple. Gated to a `zip(iter, iter)` base whose two
            // sides are `.iter()`/`.into_iter()` calls (what the for-loop zip
            // arm accepts); any other base falls through to the general walk /
            // loud bail (never a miscompile).
            if method == "map" && args.len() == 1 {
                if let ExprKind::MethodCall {
                    method: zmethod,
                    args: zargs,
                    ..
                } = &object.kind
                {
                    if zmethod == "zip" && zargs.len() == 1 {
                        if let Some(v) = self.try_compile_zip_map_collect(
                            object.as_ref(),
                            &args[0].value,
                            call_span,
                        )? {
                            return Ok(Some(v));
                        }
                    }
                }
            }
            // B-2026-07-04-2 sub-part 1 (cycle+take): `<src>.cycle().take(n)
            // .collect()` repeats the source until `n` elements are collected.
            // A BARE `cycle()` (no bounding `take`) is unbounded and never
            // reaches this branch (it stays a loud dispatch-fail — a
            // non-terminating collect is a semantic non-starter). Only the
            // `cycle().take(n)` shape over an identity source lowers.
            if method == "take" && args.len() == 1 {
                if let ExprKind::MethodCall {
                    object: cyc_recv,
                    method: cyc_method,
                    args: cyc_args,
                    ..
                } = &object.kind
                {
                    if cyc_method == "cycle" && cyc_args.is_empty() {
                        let src_is_identity = matches!(&cyc_recv.kind,
                            ExprKind::MethodCall { method, args, .. }
                                if args.is_empty() && (method == "iter" || method == "into_iter"))
                            || matches!(
                                &cyc_recv.kind,
                                ExprKind::Range {
                                    start: Some(_),
                                    end: Some(_),
                                    ..
                                }
                            );
                        if src_is_identity {
                            if let Some(v) = self.try_compile_cycle_take_collect(
                                cyc_recv,
                                &args[0].value,
                                call_span,
                            )? {
                                return Ok(Some(v));
                            }
                            return Ok(None);
                        }
                    }
                }
            }
            // B-2026-07-04-2 sub-part 1 (chunks/windows): `<base>.iter()
            // .chunks(n).collect()` groups the source into consecutive
            // `Vec[E]` slices of length `n` (last chunk short); `.windows(n)`
            // yields every overlapping length-`n` slice. Lowered with an
            // IN-PLACE fill: push a fresh EMPTY sub-Vec into the accumulator,
            // then `acc[idx].push(base[j])` fills it directly. This avoids the
            // consume-then-reuse loop-local heap binding (`let chunk; …;
            // acc.push(chunk)`) that the ownership checker would RC-fallback —
            // machinery the synthetic AST (generated post-ownership) can't
            // trigger, so that shape double-freed. The only moved binding is the
            // EMPTY sub-Vec (cap=0, nothing to free); the heap elements clone
            // straight into `acc[idx]` via `base[j]`. Gated to a named-Vec
            // `.iter()` base and a positive integer-literal `n`; any other shape
            // bails to the loud dispatch-fail (never a miscompile).
            if (method == "chunks" || method == "windows") && args.len() == 1 {
                let iter_base = |e: &Expr| -> Option<Expr> {
                    match &e.kind {
                        ExprKind::MethodCall {
                            object,
                            method,
                            args,
                            ..
                        } if args.is_empty() && method == "iter" => Some((**object).clone()),
                        _ => None,
                    }
                };
                let n_lit = match &args[0].value.kind {
                    ExprKind::Integer(n, _) if *n > 0 => Some(*n),
                    _ => None,
                };
                if let (Some(base), Some(n)) = (iter_base(object.as_ref()), n_lit) {
                    let base_is_named_vec = matches!(&base.kind, ExprKind::Identifier(nm)
                        if self.var_types.var_elem_type_exprs.contains_key(nm.as_str()));
                    if base_is_named_vec {
                        let overlapping = method == "windows";
                        if let Some(v) = self.try_compile_chunks_windows_collect(
                            &base,
                            n,
                            overlapping,
                            call_span,
                        )? {
                            return Ok(Some(v));
                        }
                        return Ok(None);
                    }
                }
            }
        }

        // B-2026-07-04-2 sub-part 3 (non-terminal f-string map): a `map(|x|
        // f"…")` that is NOT the last adaptor can't materialize into the
        // intermediate `let __icm = f"…"` (it double-frees via the staged
        // f-string accumulator, B-2026-07-03-25). Split the chain AT the
        // outermost such map: collect the prefix (the f-string map is now
        // TERMINAL → the supported `push(f"…")` shape) into a `Vec[String]`
        // temp, then continue the remaining adaptors over `__ft.iter()`. Each
        // split retires one non-terminal f-string map; a nested one inside the
        // prefix recurses. No-op when there is no non-terminal f-string map.
        if let Some(v) = self.try_split_nonterminal_fstring_map_collect(collect_recv, call_span)? {
            return Ok(Some(v));
        }

        let mut steps: Vec<IterAdaptor> = Vec::new();
        let mut cur = collect_recv;
        while let ExprKind::MethodCall {
            object,
            method,
            args,
            ..
        } = &cur.kind
        {
            let step = match method.as_str() {
                // Zero-argument adaptor.
                "enumerate" if args.is_empty() => IterAdaptor::Enumerate,
                // Closure-argument adaptors: the argument is either a
                // single-`Binding`-param closure (inline its body with the param
                // bound to the element) or a NAMED-FUNCTION reference
                // (`.map(double)`, `.filter(is_big)`) — synthesize the wrapping
                // body `<fn>(<param>)` so the lowering is identical to
                // `.map(|x| double(x))` (B-2026-07-04-2 sub-part 2). A multi-param
                // / destructuring closure still returns `Ok(None)` (loud
                // dispatch-fail — the destructuring residual stays open).
                "map" | "filter" | "take_while" | "skip_while" | "inspect" if args.len() == 1 => {
                    let (param, body) = match &args[0].value.kind {
                        ExprKind::Closure { params, body, .. } => {
                            if params.len() != 1 {
                                return Ok(None);
                            }
                            match &params[0].pattern.kind {
                                PatternKind::Binding(param) => (param.clone(), (**body).clone()),
                                // Tuple-destructuring param — e.g.
                                // `enumerate().map(|(i, x)| …)` (B-2026-07-04-2
                                // sub-part 2). Bind a fresh single param to the
                                // element and desugar the destructuring into
                                // leading `let`s in a block body:
                                // `|__dp| { let i = __dp.0; let x = __dp.1; <body> }`.
                                // This reuses the proven single-`Binding`
                                // pipeline verbatim (the element is a tuple, so
                                // `__dp.k` is an ordinary `TupleIndex`), and
                                // normal block scoping handles any shadowing in
                                // the body. Only all-`Binding`/`_` sub-patterns
                                // are lowered; a nested/complex sub-pattern
                                // (`|((a, b), c)|`, a literal, …) bails to the
                                // loud dispatch-fail rather than miscompiling.
                                PatternKind::Tuple(subs) => {
                                    let dp = format!(
                                        "__dp_{}_{}",
                                        self.indexed_elem_counter,
                                        steps.len()
                                    );
                                    let mut stmts = Vec::new();
                                    for (k, sub) in subs.iter().enumerate() {
                                        match &sub.kind {
                                            PatternKind::Wildcard => {}
                                            PatternKind::Binding(name) => {
                                                stmts.push(Stmt {
                                                    kind: StmtKind::Let {
                                                        is_mut: false,
                                                        pattern: Pattern {
                                                            kind: PatternKind::Binding(
                                                                name.clone(),
                                                            ),
                                                            span: sub.span,
                                                        },
                                                        ty: None,
                                                        value: Expr {
                                                            kind: ExprKind::TupleIndex {
                                                                object: Box::new(Expr {
                                                                    kind: ExprKind::Identifier(
                                                                        dp.clone(),
                                                                    ),
                                                                    span: sub.span,
                                                                }),
                                                                index: k as u64,
                                                            },
                                                            span: sub.span,
                                                        },
                                                    },
                                                    span: sub.span,
                                                });
                                            }
                                            _ => return Ok(None),
                                        }
                                    }
                                    let block = Block {
                                        stmts,
                                        final_expr: Some(Box::new((**body).clone())),
                                        span: body.span,
                                    };
                                    (
                                        dp,
                                        Expr {
                                            kind: ExprKind::Block(block),
                                            span: body.span,
                                        },
                                    )
                                }
                                // Wildcard param — `map(|_| 7)` / `filter(|_| ..)`.
                                // The body ignores the element, so bind it to a
                                // fresh throwaway name (the interpreter already
                                // accepts `|_|`; this aligns codegen, B-2026-07-11-19).
                                PatternKind::Wildcard => {
                                    let wname = format!(
                                        "__wild_{}_{}",
                                        self.indexed_elem_counter,
                                        steps.len()
                                    );
                                    (wname, (**body).clone())
                                }
                                _ => return Ok(None),
                            }
                        }
                        // A named-function reference — a bare `Identifier`
                        // (`double`) or a qualified `Path` (`math.sq`). Wrap it in
                        // a fresh single param `p` whose body is `<fn>(p)`. The
                        // synthetic param name is disambiguated by the current
                        // chain depth so multiple named-fn stages don't collide
                        // (the outer `uid` isn't allocated until after the peel).
                        // A non-callable arg still lowers to `<arg>(p)`, which
                        // loud-fails at codegen rather than miscompiling.
                        ExprKind::Identifier(_) | ExprKind::Path { .. } => {
                            let param =
                                format!("__mfp_{}_{}", self.indexed_elem_counter, steps.len());
                            let call = Expr {
                                kind: ExprKind::Call {
                                    callee: Box::new(args[0].value.clone()),
                                    args: vec![CallArg {
                                        label: None,
                                        mut_marker: false,
                                        mut_marker_span: None,
                                        value: Expr {
                                            kind: ExprKind::Identifier(param.clone()),
                                            span: args[0].value.span,
                                        },
                                        span: args[0].value.span,
                                    }],
                                },
                                span: args[0].value.span,
                            };
                            (param, call)
                        }
                        _ => return Ok(None),
                    };
                    match method.as_str() {
                        // B-2026-08-10-18 — same guard as the `fold` / `any` /
                        // `all` terminals: these adaptor bodies are spliced into
                        // the fused loop, so an explicit `return` in one lands in
                        // the enclosing function. Here it leaves a `ret`
                        // mid-block and fails LLVM verification with
                        // "Terminator found in the middle of a basic block",
                        // which says nothing about the cause.
                        "map" | "filter" | "take_while" | "skip_while"
                            if Self::closure_body_has_explicit_return(&body) =>
                        {
                            // B-2026-08-10-18 — register, then fall through to
                            // the normal adaptor construction below.
                            self.register_iter_body_retarget(&body);
                            match method.as_str() {
                                "map" => IterAdaptor::Map { param, body },
                                "filter" => IterAdaptor::Filter { param, pred: body },
                                "take_while" => IterAdaptor::TakeWhile { param, pred: body },
                                _ => IterAdaptor::SkipWhile { param, pred: body },
                            }
                        }
                        "map" => IterAdaptor::Map { param, body },
                        "filter" => IterAdaptor::Filter { param, pred: body },
                        "take_while" => IterAdaptor::TakeWhile { param, pred: body },
                        "skip_while" => IterAdaptor::SkipWhile { param, pred: body },
                        _ => IterAdaptor::Inspect { param, body },
                    }
                }
                // Count-argument adaptors: a single integer expression, bound
                // once before the loop. A closure argument here is malformed for
                // these methods — bail to the diagnostic.
                "take" | "skip" | "step_by" if args.len() == 1 => {
                    if matches!(&args[0].value.kind, ExprKind::Closure { .. }) {
                        return Ok(None);
                    }
                    let count = args[0].value.clone();
                    match method.as_str() {
                        "take" => IterAdaptor::Take { count },
                        "skip" => IterAdaptor::Skip { count },
                        _ => IterAdaptor::StepBy { count },
                    }
                }
                // Any other adaptor (`zip`, `chain`, `flat_map`, `chunks`,
                // `windows`, `scan`, `cycle`, …) is not yet lowered — stop
                // peeling. Whatever remains becomes the `base_iterable`; if it is
                // itself an unhandled iterator method call, the emitted `for … in
                // <base>` loud-fails at codegen rather than miscompiling
                // (B-2026-07-04-2 sub-part 1 residual).
                _ => break,
            };
            steps.push(step);
            cur = object;
        }
        if steps.is_empty() {
            // Identity collect (`<src>.iter().collect()`) with no
            // `map`/`filter`/... adaptor. Inject a synthetic identity
            // `map(|x| x)` so the shared pipeline below lowers it exactly like
            // the verified `<src>.iter().map(|x| x).collect()` shape — a fresh
            // `Vec` of element CLONES (the source is borrowed via `.iter()`, so
            // it survives; both own independent buffers, freed once each).
            // B-2026-07-04-2 sub-part 4.
            //
            // Gated to a recognized iterator SOURCE:
            //   * a no-arg `.iter()` or `.into_iter()` method call. Both CLONE
            //     the element here (the source survives — the ownership checker
            //     treats `<local>.into_iter().collect()` as non-consuming, so
            //     `v.len()` stays valid after, exactly like `.iter()`; the
            //     `for x in <src>.into_iter()` loop already lowers identically
            //     to `.iter()`, control_flow_for.rs). So identity collect over
            //     either is the same clone lowering — B-2026-07-04-2 sub-part 4,
            //     into_iter half. or
            //   * a BOUNDED integer range `a..b` / `a..=b` (`start` and `end`
            //     both present) — `for x in a..b` yields owned POD integers, so
            //     the identity `map(|x| x)` is a plain copy with no source to
            //     alias (B-2026-07-04-2 sub-part 4, range half). An UNBOUNDED
            //     range (`a..`, `..b`) is not collectable and bails.
            // Any other empty-`steps` base (an unhandled adaptor peeled to the
            // `_ => break` arm, a bare iterator variable, …) keeps bailing to
            // the loud dispatch-fail, never a miscompile.
            let is_iter_source = matches!(
                &cur.kind,
                ExprKind::MethodCall { method, args, .. }
                    if args.is_empty() && (method == "iter" || method == "into_iter")
            ) || matches!(
                &cur.kind,
                ExprKind::Range {
                    start: Some(_),
                    end: Some(_),
                    ..
                }
            );
            if !is_iter_source {
                return Ok(None);
            }
            let param = format!("__idc_{}", self.indexed_elem_counter);
            let body = Expr {
                kind: ExprKind::Identifier(param.clone()),
                span: *call_span,
            };
            steps.push(IterAdaptor::Map { param, body });
        }
        steps.reverse();
        let base_iterable = cur.clone();

        // A *non-terminal* `map` whose body evaluates to an f-string must
        // materialize into an intermediate `let` (so the threaded element stays
        // a simple identifier), but `let x = f"…"` routes through the
        // staged-f-string-accumulator path that double-frees once the value is
        // also `push`ed. The terminal `map` dodges this by pushing directly, but
        // a non-terminal one can't. Reject such a chain (loud dispatch-fail)
        // rather than miscompile — a genuinely rare shape (an f-string feeding a
        // further adaptor). `to_string()` / arithmetic bodies are unaffected.
        fn body_tail_is_fstring(e: &Expr) -> bool {
            match &e.kind {
                ExprKind::InterpolatedStringLit(_) => true,
                ExprKind::Block(b) => b.final_expr.as_deref().is_some_and(body_tail_is_fstring),
                ExprKind::If {
                    then_block,
                    else_branch,
                    ..
                } => {
                    then_block
                        .final_expr
                        .as_deref()
                        .is_some_and(body_tail_is_fstring)
                        || else_branch.as_deref().is_some_and(body_tail_is_fstring)
                }
                _ => false,
            }
        }
        for (i, step) in steps.iter().enumerate() {
            let is_terminal = i + 1 == steps.len();
            if !is_terminal {
                if let IterAdaptor::Map { body, .. } = step {
                    if body_tail_is_fstring(body) {
                        return Ok(None);
                    }
                }
            }
        }

        // Output element type: `owned_temp_drops[collect_span]` = `Vec[U]`
        // (the lowering pass records it for every `Vec`-typed expr). Reused
        // verbatim as the accumulator's annotation so `push` lowers `U`.
        let vec_te = match self
            .drop_rc
            .owned_temp_drops
            .get(&(call_span.offset, call_span.length))
        {
            Some(te) => te.clone(),
            None => return Ok(None),
        };
        let is_vec = matches!(
            &vec_te.kind,
            TypeKind::Path(p) if p.segments.last().map(|s| s.as_str()) == Some("Vec")
        );
        if !is_vec {
            return Ok(None);
        }

        // NOTE (B-2026-07-05-1, resolved): a HEAP-bearing `enumerate` tuple
        // `(idx, <heap>)` threaded through downstream stages used to be gated
        // out here whenever a stage would whole-COPY it — a later param stage
        // whose name differs from the tuple binding (`let q = <tuple>`), or a
        // non-terminal `map` returning the whole tuple (`let __icm = <tuple>`).
        // Those copies are now SOUND: the desugar always threads the tuple as a
        // bare identifier (or a re-tuple of its fields), so each `let q = <id>`
        // is an ordinary Vec/String/tuple MOVE that `suppress_source_vec_cleanup
        // _for_arg` / `compile_tuple` retire at the source — no alias, no
        // double-free (verified run==build + LSan-clean across differing-param,
        // non-terminal-map-identity, re-tuple-map, and multi-stage shapes). The
        // gate (and its `copy_free` simulation of the desugar's name threading)
        // is therefore removed; the tuple CONSTRUCTION double-free it referenced
        // (B-2026-07-04-3) stays fixed in `compile_tuple`.

        let uid = self.indexed_elem_counter;
        self.indexed_elem_counter += 1;
        let sp = *call_span;
        let vec_name = format!("__icv_{}", uid);
        // The FIRST param-bearing adaptor's closure param IS the for-loop
        // variable, so the element inherits the source's element type from the
        // for-loop's own binding registration (a synthetic `let param = <elem>`
        // would lose it, breaking method dispatch on a heap element — e.g.
        // `.map(|w| w.len())` over a `Vec[String]`). Count-argument adaptors
        // (`take`/`skip`/`step_by`) have no param, so they thread the loop var
        // through unchanged; a *leading* one therefore does NOT force a
        // synthetic name — the first downstream `map`/`filter`/… still gets the
        // typed loop var directly (its `current` IS its param → the redundant
        // self-binding is elided in `build_body`). A synthetic `__ice_N` is used
        // only when the chain has NO param-bearing stage at all (e.g.
        // `iter().skip(1).take(2).collect()`).
        //
        // The search STOPS at `enumerate` (a retyping stage): a param stage AFTER
        // enumerate binds the `(idx, T)` TUPLE, not the source element, so its
        // param must NOT name the loop var — and it also collides with the
        // enumerate arm's look-ahead tuple binding (both would be that param).
        // So a leading `enumerate` yields a synthetic loop var. `take`/`skip`/
        // `step_by` don't retype, so a param stage past them still binds the
        // source and is honored.
        let elem_name = {
            let mut found = None;
            for s in &steps {
                match s {
                    IterAdaptor::Map { param, .. }
                    | IterAdaptor::Filter { param, .. }
                    | IterAdaptor::TakeWhile { param, .. }
                    | IterAdaptor::SkipWhile { param, .. }
                    | IterAdaptor::Inspect { param, .. } => {
                        found = Some(param.clone());
                        break;
                    }
                    IterAdaptor::Enumerate => break,
                    IterAdaptor::Take { .. }
                    | IterAdaptor::Skip { .. }
                    | IterAdaptor::StepBy { .. } => continue,
                }
            }
            found.unwrap_or_else(|| format!("__ice_{}", uid))
        };

        let ident = |name: &str, sp: &crate::token::Span| Expr {
            kind: ExprKind::Identifier(name.to_string()),
            span: *sp,
        };

        // The accumulator is a plain `Vec.new()` — deliberately NOT pre-sized to
        // `Vec.with_capacity(<src>.len())`. Pre-sizing it was prototyped and
        // measured net-harmful for the collect idiom under glibc (spike
        // collection-capacity-presizing, "Empirical result", 2026-07-09): the
        // desugared loop below already grows via `realloc` with no per-element
        // bounds check, and glibc grows the buffer in place, so the collect is
        // ALREADY within ~15% of hand-tuned `with_capacity` even from `cap 0`
        // (measured 41 ms vs 36 ms on a 2048-elem `Vec[i64]` build). Forcing
        // `with_capacity` bought a modest ~1.16× on POD-element sources but
        // REGRESSED heap-element sources 20–30% — a fresh full-size malloc every
        // iteration lands on cold pages while iterating the larger heap source
        // (its `{ptr,len,cap}` headers) inflates the working set, whereas the
        // grow path reuses the previous iteration's hot buffer. The common
        // `Vec[String].filter().collect()` measured 0.72×. A source-type gate
        // (pre-size scalars only) would recover the POD win but yields an opaque,
        // allocator- and hardware-dependent two-tier performance model — exactly
        // the "unpredictable firing" the spike disqualifies. The manual
        // `Vec.with_capacity` idiom (a reliable ~2× on hand-written counted push
        // loops) and the existing `presize.rs` loop pass cover the cases where
        // pre-sizing genuinely pays; the collect accumulator is not one of them.
        //
        // `let mut __icv_N: Vec[U] = Vec.new();`
        let vec_new = Expr {
            kind: ExprKind::Call {
                callee: Box::new(Expr {
                    kind: ExprKind::Path {
                        segments: vec!["Vec".to_string(), "new".to_string()],
                        generic_args: None,
                    },
                    span: sp,
                }),
                args: vec![],
            },
            span: sp,
        };
        let let_vec = Stmt {
            kind: StmtKind::Let {
                is_mut: true,
                pattern: Pattern {
                    kind: PatternKind::Binding(vec_name.clone()),
                    span: sp,
                },
                ty: Some(vec_te),
                value: vec_new,
            },
            span: sp,
        };

        // Build the for-loop body (base → out), threading the "current element"
        // expression through each stage. Recursion keeps `filter`'s downstream
        // stages nested inside its `if`.
        fn build_body(
            steps: &[IterAdaptor],
            i: usize,
            current: Expr,
            vec_name: &str,
            uid: u32,
            sp: &crate::token::Span,
            ident: &dyn Fn(&str, &crate::token::Span) -> Expr,
        ) -> Vec<Stmt> {
            if i == steps.len() {
                // `__icv_N.push(<current>)`
                let push_call = Expr {
                    kind: ExprKind::MethodCall {
                        object: Box::new(ident(vec_name, sp)),
                        method: "push".to_string(),
                        turbofish: None,
                        args: vec![CallArg {
                            label: None,
                            mut_marker: false,
                            mut_marker_span: None,
                            value: current,
                            span: *sp,
                        }],
                        args_close_span: *sp,
                    },
                    span: *sp,
                };
                return vec![Stmt {
                    kind: StmtKind::Expr(push_call),
                    span: *sp,
                }];
            }
            // AST builders shared by the stateful-adaptor arms below. `st_i` /
            // `stn_i` are the per-stage counter / bound-count names emitted as
            // pre-loop `let`s (see the state-declaration pass in the caller).
            let st_i = format!("__st_{}_{}", uid, i);
            let stn_i = format!("__stn_{}_{}", uid, i);
            let i64_lit = |n: i64| Expr {
                kind: ExprKind::Integer(n, Some(crate::token::IntSuffix::I64)),
                span: *sp,
            };
            let bool_lit_e = |b: bool, sp: &crate::token::Span| Expr {
                kind: ExprKind::Bool(b),
                span: *sp,
            };
            // `break;` — stop the whole for-loop, mirroring the interpreter's
            // `stop`/`drain_source` for an exhausted `take` or a tripped
            // `take_while` (iter_eval.rs). The break sits at the adaptor's
            // position in the chain, so upstream stages have already run for the
            // element that trips it — exactly as under `karac run`.
            let break_stmt = || Stmt {
                kind: StmtKind::Expr(Expr {
                    kind: ExprKind::Break {
                        label: None,
                        value: None,
                    },
                    span: *sp,
                }),
                span: *sp,
            };
            let bin = |op: BinOp, l: Expr, r: Expr| Expr {
                kind: ExprKind::Binary {
                    op,
                    left: Box::new(l),
                    right: Box::new(r),
                },
                span: *sp,
            };
            // `<name> = <value>;`
            let assign = |name: &str, value: Expr| Stmt {
                kind: StmtKind::Assign {
                    target: ident(name, sp),
                    value,
                },
                span: *sp,
            };
            // `if <cond> { <then> } [else { <els> }]` as an expr-statement.
            let if_stmt = |cond: Expr, then: Vec<Stmt>, els: Option<Vec<Stmt>>| Stmt {
                kind: StmtKind::Expr(Expr {
                    kind: ExprKind::If {
                        condition: Box::new(cond),
                        then_block: Block {
                            stmts: then,
                            final_expr: None,
                            span: *sp,
                        },
                        else_branch: els.map(|e| {
                            Box::new(Expr {
                                kind: ExprKind::Block(Block {
                                    stmts: e,
                                    final_expr: None,
                                    span: *sp,
                                }),
                                span: *sp,
                            })
                        }),
                    },
                    span: *sp,
                }),
                span: *sp,
            };
            // Bind a predicate/inspect closure param to `current` and yield the
            // body, eliding the redundant self-binding when `current` already IS
            // the param (mirrors the `filter`/`map` elision so the typed loop var
            // is used directly). Returns the body expr with the param in scope.
            let bind_param_expr = |param: &str, body: &Expr| -> Expr {
                let current_is_param =
                    matches!(&current.kind, ExprKind::Identifier(n) if n == param);
                if current_is_param {
                    body.clone()
                } else {
                    let bind = Stmt {
                        kind: StmtKind::Let {
                            is_mut: false,
                            pattern: Pattern {
                                kind: PatternKind::Binding(param.to_string()),
                                span: *sp,
                            },
                            ty: None,
                            value: current.clone(),
                        },
                        span: *sp,
                    };
                    Expr {
                        kind: ExprKind::Block(Block {
                            stmts: vec![bind],
                            final_expr: Some(Box::new(body.clone())),
                            span: *sp,
                        }),
                        span: *sp,
                    }
                }
            };
            match &steps[i] {
                IterAdaptor::Map { param, body } => {
                    // Compute the transformed value. When `current` already IS
                    // `param` (the base-most stage, whose param is the for-loop
                    // var), the `let param = param` is redundant *and* would strip
                    // the loop var's element type; use the body directly against
                    // the typed param instead.
                    let current_is_param =
                        matches!(&current.kind, ExprKind::Identifier(n) if n == param);
                    let map_value = if current_is_param {
                        body.clone()
                    } else {
                        let bind_param = Stmt {
                            kind: StmtKind::Let {
                                is_mut: false,
                                pattern: Pattern {
                                    kind: PatternKind::Binding(param.clone()),
                                    span: *sp,
                                },
                                ty: None,
                                value: current,
                            },
                            span: *sp,
                        };
                        Expr {
                            kind: ExprKind::Block(Block {
                                stmts: vec![bind_param],
                                final_expr: Some(Box::new(body.clone())),
                                span: *sp,
                            }),
                            span: *sp,
                        }
                    };
                    // Terminal map: push the value directly rather than binding it
                    // to an intermediate `let`. A `let __icm = f"…"` RHS routes
                    // through the staged-f-string-accumulator path, which
                    // double-frees the built String once it's also `push`ed
                    // (B-2026-07-03-25 follow-on); `push(f"…")` is the supported,
                    // leak-clean form (mirrors a hand-written `for … { v.push(f"…")
                    // }`). The base-most param stays typed because `map_value`
                    // references it directly. The caller pre-rejects a
                    // *non-terminal* map with an f-string body (which would still
                    // need the poisoned `let`).
                    if i + 1 == steps.len() {
                        return build_body(steps, i + 1, map_value, vec_name, uid, sp, ident);
                    }
                    // Non-terminal map: materialize into a fresh `let` so the
                    // threaded "current" stays a simple identifier (no downstream
                    // re-evaluation, and a subsequent `filter`'s `let param =
                    // current` binds an identifier, never a heap temp).
                    let synth = format!("__icm_{}_{}", uid, i);
                    let let_synth = Stmt {
                        kind: StmtKind::Let {
                            is_mut: false,
                            pattern: Pattern {
                                kind: PatternKind::Binding(synth.clone()),
                                span: *sp,
                            },
                            ty: None,
                            value: map_value,
                        },
                        span: *sp,
                    };
                    let mut out = vec![let_synth];
                    out.extend(build_body(
                        steps,
                        i + 1,
                        ident(&synth, sp),
                        vec_name,
                        uid,
                        sp,
                        ident,
                    ));
                    out
                }
                IterAdaptor::Filter { param, pred } => {
                    // `if { let <param> = <current>; <pred> } { <rest> }` — with
                    // the same redundant-self-binding elision as `map`: when
                    // `current` IS `param`, evaluate `pred` directly against the
                    // typed loop var. The downstream `current` is unchanged (a
                    // filter is identity on the element it lets through).
                    let current_is_param =
                        matches!(&current.kind, ExprKind::Identifier(n) if n == param);
                    let guard = if current_is_param {
                        pred.clone()
                    } else {
                        let bind_param = Stmt {
                            kind: StmtKind::Let {
                                is_mut: false,
                                pattern: Pattern {
                                    kind: PatternKind::Binding(param.clone()),
                                    span: *sp,
                                },
                                ty: None,
                                value: current.clone(),
                            },
                            span: *sp,
                        };
                        Expr {
                            kind: ExprKind::Block(Block {
                                stmts: vec![bind_param],
                                final_expr: Some(Box::new(pred.clone())),
                                span: *sp,
                            }),
                            span: *sp,
                        }
                    };
                    let then_stmts = build_body(steps, i + 1, current, vec_name, uid, sp, ident);
                    let if_expr = Expr {
                        kind: ExprKind::If {
                            condition: Box::new(guard),
                            then_block: Block {
                                stmts: then_stmts,
                                final_expr: None,
                                span: *sp,
                            },
                            else_branch: None,
                        },
                        span: *sp,
                    };
                    vec![Stmt {
                        kind: StmtKind::Expr(if_expr),
                        span: *sp,
                    }]
                }
                IterAdaptor::Take { .. } => {
                    // `if __st_i >= __stn_i { break }  __st_i = __st_i + 1;
                    //  <rest>` — yield the first `n` elements reaching this stage,
                    // then `break` the loop. Matches the interpreter's `Take`
                    // step (iter_eval.rs): once `remaining == 0` it sets `stop`
                    // and drains the source, so the element that trips exhaustion
                    // has already run every UPSTREAM stage (e.g. a preceding
                    // `inspect`) but no downstream stage — identical here because
                    // the break sits after the upstream stages and before the
                    // rest.
                    let cond = bin(BinOp::GtEq, ident(&st_i, sp), ident(&stn_i, sp));
                    let mut out = vec![if_stmt(cond, vec![break_stmt()], None)];
                    out.push(assign(&st_i, bin(BinOp::Add, ident(&st_i, sp), i64_lit(1))));
                    out.extend(build_body(steps, i + 1, current, vec_name, uid, sp, ident));
                    out
                }
                IterAdaptor::Skip { .. } => {
                    // `if __st_i < __stn_i { __st_i = __st_i + 1 } else { <rest> }`
                    // — swallow the first `n` elements reaching this stage, pass
                    // the rest through.
                    let cond = bin(BinOp::Lt, ident(&st_i, sp), ident(&stn_i, sp));
                    let then = vec![assign(&st_i, bin(BinOp::Add, ident(&st_i, sp), i64_lit(1)))];
                    let els = build_body(steps, i + 1, current, vec_name, uid, sp, ident);
                    vec![if_stmt(cond, then, Some(els))]
                }
                IterAdaptor::StepBy { .. } => {
                    // `if __st_i % __stn_i == 0 { <rest> } __st_i = __st_i + 1;` —
                    // yield elements at positions 0, n, 2n, … (relative to this
                    // stage's input) and advance the counter every element.
                    let modulo = bin(BinOp::Mod, ident(&st_i, sp), ident(&stn_i, sp));
                    let cond = bin(BinOp::Eq, modulo, i64_lit(0));
                    let rest = build_body(steps, i + 1, current, vec_name, uid, sp, ident);
                    vec![
                        if_stmt(cond, rest, None),
                        assign(&st_i, bin(BinOp::Add, ident(&st_i, sp), i64_lit(1))),
                    ]
                }
                IterAdaptor::TakeWhile { param, pred } => {
                    // `if <pred> { <rest> } else { break }` — yield while the
                    // predicate holds; the first `false` breaks the loop. Matches
                    // the interpreter's `TakeWhile` step (iter_eval.rs): the
                    // predicate is evaluated on each element (after upstream
                    // stages) including the first failing one, which then sets
                    // `stop`/drains — so no later element is even pulled. The
                    // `break` gives the same "predicate runs through the first
                    // failure, then iteration stops" shape without a latch.
                    let guard = bind_param_expr(param, pred);
                    let rest = build_body(steps, i + 1, current, vec_name, uid, sp, ident);
                    vec![if_stmt(guard, rest, Some(vec![break_stmt()]))]
                }
                IterAdaptor::SkipWhile { param, pred } => {
                    // `if !(__st_i && <pred>) { __st_i = false; <rest> }` — while
                    // still skipping (`__st_i` true) and the predicate holds, drop
                    // the element; the first non-match latches `__st_i = false`
                    // and passes it plus every subsequent element (the `&&`
                    // short-circuits `<pred>` once skipping stops).
                    let guard = bind_param_expr(param, pred);
                    let and = bin(BinOp::And, ident(&st_i, sp), guard);
                    let cond = Expr {
                        kind: ExprKind::Unary {
                            op: UnaryOp::Not,
                            operand: Box::new(and),
                        },
                        span: *sp,
                    };
                    let mut then = vec![assign(&st_i, bool_lit_e(false, sp))];
                    then.extend(build_body(steps, i + 1, current, vec_name, uid, sp, ident));
                    vec![if_stmt(cond, then, None)]
                }
                IterAdaptor::Inspect { param, body } => {
                    // `{ let param = current; body };  <rest>` — run the closure
                    // for its side effect (value discarded) and pass the element
                    // through unchanged.
                    let side_effect = bind_param_expr(param, body);
                    let mut out = vec![Stmt {
                        kind: StmtKind::Expr(side_effect),
                        span: *sp,
                    }];
                    out.extend(build_body(steps, i + 1, current, vec_name, uid, sp, ident));
                    out
                }
                IterAdaptor::Enumerate => {
                    // `let <tup> = (__st_i, current); __st_i = __st_i + 1;
                    //  <rest(<tup>)>` — pair the element with the CURRENT index,
                    // then advance the counter. Matches the interpreter's
                    // `Enumerate` step (iter_eval.rs): `item = (idx, item); idx +=
                    // 1`. Binding the tuple to a fresh local captures the
                    // pre-increment index and threads a plain identifier
                    // downstream.
                    //
                    // The binding NAME is the FIRST downstream param-bearing
                    // stage's param, so the heap tuple has a SINGLE owning
                    // binding: with a distinct `__ietup` that stage's `let p =
                    // __ietup` would bit-copy (alias) the heap buffer and
                    // double-free (B-2026-07-04-4). The stage then sees `current
                    // == param` and elides its own re-binding (the same
                    // `current_is_param` elision the `map`/`filter` arms use).
                    //
                    // The search skips PAST the value-preserving passthrough
                    // adaptors (`take`/`skip`/`step_by` gate/count but never
                    // rebind the element) so the tuple binds directly to the
                    // param even when a passthrough sits between `enumerate` and
                    // it (`enumerate().take(n).map(|p| …)`, B-2026-07-04-4 case
                    // D) — the passthrough arms thread `current` (= this param
                    // name) through unchanged. When NO param-bearing stage
                    // follows (`take`/`skip`/`step_by` only, or terminal) the
                    // whole tuple is passed through / pushed by MOVE, so the
                    // synthetic `__ietup` is fine. The caller's copy-free gate
                    // rejects the residual whole-tuple-copy shapes (a later param
                    // stage with a DIFFERENT name, a non-terminal whole-tuple
                    // `map`) to the loud dispatch-fail — never a miscompile.
                    let mut downstream_param = None;
                    for s in &steps[i + 1..] {
                        match s {
                            IterAdaptor::Map { param, .. }
                            | IterAdaptor::Filter { param, .. }
                            | IterAdaptor::TakeWhile { param, .. }
                            | IterAdaptor::SkipWhile { param, .. }
                            | IterAdaptor::Inspect { param, .. } => {
                                downstream_param = Some(param.clone());
                                break;
                            }
                            IterAdaptor::Take { .. }
                            | IterAdaptor::Skip { .. }
                            | IterAdaptor::StepBy { .. } => continue,
                            // A nested `enumerate` re-pairs the element; its own
                            // arm binds that tuple. Unreachable in practice.
                            IterAdaptor::Enumerate => break,
                        }
                    }
                    let tup_name =
                        downstream_param.unwrap_or_else(|| format!("__ietup_{}_{}", uid, i));
                    let tuple = Expr {
                        kind: ExprKind::Tuple(vec![ident(&st_i, sp), current]),
                        span: *sp,
                    };
                    let let_tup = Stmt {
                        kind: StmtKind::Let {
                            is_mut: false,
                            pattern: Pattern {
                                kind: PatternKind::Binding(tup_name.clone()),
                                span: *sp,
                            },
                            ty: None,
                            value: tuple,
                        },
                        span: *sp,
                    };
                    let mut out = vec![let_tup];
                    out.push(assign(&st_i, bin(BinOp::Add, ident(&st_i, sp), i64_lit(1))));
                    out.extend(build_body(
                        steps,
                        i + 1,
                        ident(&tup_name, sp),
                        vec_name,
                        uid,
                        sp,
                        ident,
                    ));
                    out
                }
            }
        }

        let for_body = build_body(
            &steps,
            0,
            ident(&elem_name, &sp),
            &vec_name,
            uid,
            &sp,
            &ident,
        );

        // `for __ice_N in <base_iterable> { <for_body> }`
        let for_stmt = Stmt {
            kind: StmtKind::Expr(Expr {
                kind: ExprKind::For {
                    label: None,
                    pattern: Pattern {
                        kind: PatternKind::Binding(elem_name.clone()),
                        span: sp,
                    },
                    iterable: Box::new(base_iterable),
                    body: Block {
                        stmts: for_body,
                        final_expr: None,
                        span: sp,
                    },
                    attributes: vec![],
                },
                span: sp,
            }),
            span: sp,
        };

        // Pre-loop state declarations for the stateful adaptors (counters /
        // latches, keyed by stage index so `build_body` can reference them by
        // the same deterministic name). Emitted after `let_vec`, before the loop.
        let named_ty = |name: &str, sp: &crate::token::Span| TypeExpr {
            kind: TypeKind::Path(PathExpr {
                segments: vec![name.to_string()],
                generic_args: None,
                span: *sp,
            }),
            span: *sp,
        };
        let i64_lit = |n: i64, sp: &crate::token::Span| Expr {
            kind: ExprKind::Integer(n, Some(crate::token::IntSuffix::I64)),
            span: *sp,
        };
        let bool_lit = |b: bool, sp: &crate::token::Span| Expr {
            kind: ExprKind::Bool(b),
            span: *sp,
        };
        let let_stmt =
            |name: &str, is_mut: bool, ty: TypeExpr, value: Expr, sp: &crate::token::Span| Stmt {
                kind: StmtKind::Let {
                    is_mut,
                    pattern: Pattern {
                        kind: PatternKind::Binding(name.to_string()),
                        span: *sp,
                    },
                    ty: Some(ty),
                    value,
                },
                span: *sp,
            };
        let mut state_stmts: Vec<Stmt> = Vec::new();
        for (i, step) in steps.iter().enumerate() {
            match step {
                IterAdaptor::Take { count } | IterAdaptor::Skip { count } => {
                    // `let __stn_N_i: i64 = <count>;` — bind the count once
                    // (a non-trivial count expr must not be re-evaluated per
                    // element) — and `let mut __st_N_i: i64 = 0;` — the counter.
                    state_stmts.push(let_stmt(
                        &format!("__stn_{}_{}", uid, i),
                        false,
                        named_ty("i64", &sp),
                        count.clone(),
                        &sp,
                    ));
                    state_stmts.push(let_stmt(
                        &format!("__st_{}_{}", uid, i),
                        true,
                        named_ty("i64", &sp),
                        i64_lit(0, &sp),
                        &sp,
                    ));
                }
                IterAdaptor::StepBy { count } => {
                    // Same shape as Take/Skip, but the stride is CLAMPED to
                    // ≥ 1 (`let __str = <count>; let __stn = if __str < 1
                    // { 1 } else { __str };`) — the interpreter clamps
                    // (`n.max(1)`, method_call_iter.rs), and the unclamped
                    // `% 0` in the stage body would trap (SIGFPE) instead of
                    // matching the oracle (B-2026-07-14-8, step_by leg).
                    let raw = format!("__str_{}_{}", uid, i);
                    state_stmts.push(let_stmt(
                        &raw,
                        false,
                        named_ty("i64", &sp),
                        count.clone(),
                        &sp,
                    ));
                    let clamped = Expr {
                        kind: ExprKind::If {
                            condition: Box::new(Expr {
                                kind: ExprKind::Binary {
                                    op: BinOp::Lt,
                                    left: Box::new(ident(&raw, &sp)),
                                    right: Box::new(i64_lit(1, &sp)),
                                },
                                span: sp,
                            }),
                            then_block: Block {
                                stmts: Vec::new(),
                                final_expr: Some(Box::new(i64_lit(1, &sp))),
                                span: sp,
                            },
                            else_branch: Some(Box::new(Expr {
                                kind: ExprKind::Block(Block {
                                    stmts: Vec::new(),
                                    final_expr: Some(Box::new(ident(&raw, &sp))),
                                    span: sp,
                                }),
                                span: sp,
                            })),
                        },
                        span: sp,
                    };
                    state_stmts.push(let_stmt(
                        &format!("__stn_{}_{}", uid, i),
                        false,
                        named_ty("i64", &sp),
                        clamped,
                        &sp,
                    ));
                    state_stmts.push(let_stmt(
                        &format!("__st_{}_{}", uid, i),
                        true,
                        named_ty("i64", &sp),
                        i64_lit(0, &sp),
                        &sp,
                    ));
                }
                IterAdaptor::SkipWhile { .. } => {
                    // `let mut __st_N_i: bool = true;` — the "skipping" latch.
                    state_stmts.push(let_stmt(
                        &format!("__st_{}_{}", uid, i),
                        true,
                        named_ty("bool", &sp),
                        bool_lit(true, &sp),
                        &sp,
                    ));
                }
                IterAdaptor::Enumerate => {
                    // `let mut __st_N_i: i64 = 0;` — the running element index.
                    state_stmts.push(let_stmt(
                        &format!("__st_{}_{}", uid, i),
                        true,
                        named_ty("i64", &sp),
                        i64_lit(0, &sp),
                        &sp,
                    ));
                }
                // `TakeWhile` needs no state — it `break`s on the first failing
                // predicate rather than latching (see its `build_body` arm).
                IterAdaptor::Map { .. }
                | IterAdaptor::Filter { .. }
                | IterAdaptor::Inspect { .. }
                | IterAdaptor::TakeWhile { .. } => {}
            }
        }

        // `{ <let_vec>; <state_stmts…>; <for_stmt>; __icv_N }`
        let mut block_stmts = vec![let_vec];
        block_stmts.extend(state_stmts);
        block_stmts.push(for_stmt);
        let block = Expr {
            kind: ExprKind::Block(Block {
                stmts: block_stmts,
                final_expr: Some(Box::new(ident(&vec_name, &sp))),
                span: sp,
            }),
            span: sp,
        };

        Ok(Some(self.compile_expr(&block)?))
    }

    /// Lower `A.zip(B).collect()` where EITHER side carries its own adaptors
    /// (`A.iter().map(f).zip(B.iter())`, `A.iter().zip(B.iter().filter(g))`, …)
    /// by pre-collecting each side to a typed temp and reusing the identity zip
    /// on the two temps (B-2026-07-04-2 sub-part 1). Emitted:
    ///
    /// ```text
    /// { let __za: Vec[EA] = <A.collect()>;      // side A via full machinery
    ///   let __zb: Vec[EB] = <B.collect()>;      // side B via full machinery
    ///   __za.iter().zip(__zb.iter()).collect()  // identity zip on the temps
    /// }
    /// ```
    ///
    /// The two sub-`.collect()`s recurse through `compile_method_call`; an
    /// unsupported adaptor on a side bails to the loud dispatch-fail via the
    /// recursive compile. The result type `Vec[(EA, EB)]` is decomposed to type
    /// each side's temp; the sub-collect result types are registered under
    /// fresh synthetic spans (real source offsets are file-bounded, so a
    /// `usize::MAX`-based offset never collides). Both temps are dropped at
    /// block exit; the identity zip index-clones from them, so both original
    /// sources survive and every buffer is owned once. Returns `Ok(None)` if the
    /// result type isn't a recorded `Vec[(EA, EB)]`.
    pub(super) fn try_compile_zip_pipeline_collect(
        &mut self,
        side_a: &Expr,
        side_b: &Expr,
        call_span: &crate::token::Span,
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        let vec_te = match self
            .drop_rc
            .owned_temp_drops
            .get(&(call_span.offset, call_span.length))
        {
            Some(te) => te.clone(),
            None => return Ok(None),
        };
        // Result element must be a 2-tuple `(EA, EB)`.
        let (ea, eb) = match &vec_te.kind {
            TypeKind::Path(p) if p.segments.last().map(|s| s.as_str()) == Some("Vec") => {
                match p.generic_args.as_ref().and_then(|ga| ga.first()) {
                    Some(GenericArg::Type(t)) => match &t.kind {
                        TypeKind::Tuple(elems) if elems.len() == 2 => {
                            (elems[0].clone(), elems[1].clone())
                        }
                        _ => return Ok(None),
                    },
                    _ => return Ok(None),
                }
            }
            _ => return Ok(None),
        };

        let uid = self.indexed_elem_counter;
        self.indexed_elem_counter += 1;
        let sp = *call_span;
        let za = format!("__zpa_{}", uid);
        let zb = format!("__zpb_{}", uid);

        // `Vec[EA]` / `Vec[EB]` type exprs for the side temps.
        let vec_of = |elem: &TypeExpr| TypeExpr {
            kind: TypeKind::Path(PathExpr {
                segments: vec!["Vec".to_string()],
                generic_args: Some(vec![GenericArg::Type(elem.clone())]),
                span: sp,
            }),
            span: sp,
        };
        // Fresh synthetic spans for the two sub-collects; register their result
        // types so the recursive collect lowering resolves them.
        let span_a = crate::token::Span {
            line: sp.line,
            column: sp.column,
            offset: usize::MAX - (uid as usize) * 2 - 1,
            length: 1,
        };
        let span_b = crate::token::Span {
            line: sp.line,
            column: sp.column,
            offset: usize::MAX - (uid as usize) * 2 - 2,
            length: 1,
        };
        self.drop_rc
            .owned_temp_drops
            .insert((span_a.offset, span_a.length), vec_of(&ea));
        self.drop_rc
            .owned_temp_drops
            .insert((span_b.offset, span_b.length), vec_of(&eb));

        let ident = |name: &str| Expr {
            kind: ExprKind::Identifier(name.to_string()),
            span: sp,
        };
        // `<side>.collect()` with the given (synthetic) span.
        let collect_of = |side: &Expr, cspan: &crate::token::Span| Expr {
            kind: ExprKind::MethodCall {
                object: Box::new(side.clone()),
                method: "collect".to_string(),
                turbofish: None,
                args: vec![],
                args_close_span: *cspan,
            },
            span: *cspan,
        };
        let let_side =
            |name: &str, elem: &TypeExpr, side: &Expr, cspan: &crate::token::Span| Stmt {
                kind: StmtKind::Let {
                    is_mut: false,
                    pattern: Pattern {
                        kind: PatternKind::Binding(name.to_string()),
                        span: sp,
                    },
                    ty: Some(vec_of(elem)),
                    value: collect_of(side, cspan),
                },
                span: sp,
            };
        // `<name>.iter()`
        let iter_of = |name: &str| Expr {
            kind: ExprKind::MethodCall {
                object: Box::new(ident(name)),
                method: "iter".to_string(),
                turbofish: None,
                args: vec![],
                args_close_span: sp,
            },
            span: sp,
        };
        // `__za.iter().zip(__zb.iter()).collect()` — identity zip on the temps,
        // typed by the ORIGINAL call span (`Vec[(EA, EB)]`).
        let inner_zip_collect = Expr {
            kind: ExprKind::MethodCall {
                object: Box::new(Expr {
                    kind: ExprKind::MethodCall {
                        object: Box::new(iter_of(&za)),
                        method: "zip".to_string(),
                        turbofish: None,
                        args: vec![CallArg {
                            label: None,
                            mut_marker: false,
                            mut_marker_span: None,
                            value: iter_of(&zb),
                            span: sp,
                        }],
                        args_close_span: sp,
                    },
                    span: sp,
                }),
                method: "collect".to_string(),
                turbofish: None,
                args: vec![],
                args_close_span: sp,
            },
            span: sp,
        };
        let block = Expr {
            kind: ExprKind::Block(Block {
                stmts: vec![
                    let_side(&za, &ea, side_a, &span_a),
                    let_side(&zb, &eb, side_b, &span_b),
                ],
                final_expr: Some(Box::new(inner_zip_collect)),
                span: sp,
            }),
            span: sp,
        };
        Ok(Some(self.compile_expr(&block)?))
    }

    /// Lower `<zip>.map(f).collect()` where `<zip>` is `A.iter().zip(B.iter())`
    /// (B-2026-07-15-10). The map transforms the zipped `(EA, EB)` tuples, so
    /// the collect result is `Vec[R]` (R ≠ a 2-tuple) and
    /// `try_compile_zip_pipeline_collect`'s identity gate rejects it, while the
    /// general adaptor walk rejects a `zip` base. Rewrite to a for-loop over the
    /// zip (the for-loop zip arm binds the closure's param — a single-var tuple
    /// or a 2-sub destructure — to each pair) that pushes the mapped body:
    ///
    /// ```text
    /// { let mut __zmc: Vec[R] = Vec.new();
    ///   for <f.param> in A.iter().zip(B.iter()) { __zmc.push(<f.body>); }
    ///   __zmc }
    /// ```
    ///
    /// `R` comes from the collect's `owned_temp_drops` result type (`Vec[R]`).
    /// The map arg must be a single-param closure; anything else returns `None`
    /// (the caller falls through to the loud dispatch-fail).
    pub(super) fn try_compile_zip_map_collect(
        &mut self,
        zip_recv: &Expr,
        map_closure: &Expr,
        call_span: &crate::token::Span,
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        // Result element type R from `Vec[R]` at the collect span.
        let vec_te = match self
            .drop_rc
            .owned_temp_drops
            .get(&(call_span.offset, call_span.length))
        {
            Some(te) => te.clone(),
            None => return Ok(None),
        };
        let r_elem = match &vec_te.kind {
            TypeKind::Path(p) if p.segments.last().map(|s| s.as_str()) == Some("Vec") => {
                match p.generic_args.as_ref().and_then(|ga| ga.first()) {
                    Some(GenericArg::Type(t)) => t.clone(),
                    _ => return Ok(None),
                }
            }
            _ => return Ok(None),
        };
        // Single-param closure `|p| body` (p is a Binding or a 2-tuple pattern —
        // the for-loop zip arm handles both).
        let (param_pat, body) = match &map_closure.kind {
            ExprKind::Closure { params, body, .. } if params.len() == 1 => {
                (params[0].pattern.clone(), (**body).clone())
            }
            _ => return Ok(None),
        };

        let uid = self.indexed_elem_counter;
        self.indexed_elem_counter += 1;
        let sp = *call_span;
        let acc = format!("__zmc_{}", uid);

        let ident = |name: &str| Expr {
            kind: ExprKind::Identifier(name.to_string()),
            span: sp,
        };
        let vec_r = TypeExpr {
            kind: TypeKind::Path(PathExpr {
                segments: vec!["Vec".to_string()],
                generic_args: Some(vec![GenericArg::Type(r_elem)]),
                span: sp,
            }),
            span: sp,
        };
        // `let mut __zmc: Vec[R] = Vec.new();`
        let let_acc = Stmt {
            kind: StmtKind::Let {
                is_mut: true,
                pattern: Pattern {
                    kind: PatternKind::Binding(acc.clone()),
                    span: sp,
                },
                ty: Some(vec_r),
                value: Expr {
                    kind: ExprKind::Call {
                        callee: Box::new(Expr {
                            kind: ExprKind::Path {
                                segments: vec!["Vec".to_string(), "new".to_string()],
                                generic_args: None,
                            },
                            span: sp,
                        }),
                        args: vec![],
                    },
                    span: sp,
                },
            },
            span: sp,
        };
        // `for <param_pat> in <zip_recv> { __zmc.push(<body>); }`
        let push_stmt = Stmt {
            kind: StmtKind::Expr(Expr {
                kind: ExprKind::MethodCall {
                    object: Box::new(ident(&acc)),
                    method: "push".to_string(),
                    turbofish: None,
                    args: vec![CallArg {
                        label: None,
                        mut_marker: false,
                        mut_marker_span: None,
                        value: body,
                        span: sp,
                    }],
                    args_close_span: sp,
                },
                span: sp,
            }),
            span: sp,
        };
        let for_loop = Stmt {
            kind: StmtKind::Expr(Expr {
                kind: ExprKind::For {
                    label: None,
                    pattern: param_pat,
                    iterable: Box::new(zip_recv.clone()),
                    attributes: Vec::new(),
                    body: Block {
                        stmts: vec![push_stmt],
                        final_expr: None,
                        span: sp,
                    },
                },
                span: sp,
            }),
            span: sp,
        };
        let block = Expr {
            kind: ExprKind::Block(Block {
                stmts: vec![let_acc, for_loop],
                final_expr: Some(Box::new(ident(&acc))),
                span: sp,
            }),
            span: sp,
        };
        Ok(Some(self.compile_expr(&block)?))
    }

    /// Lower `<chain-with-filter_map>.collect()` by rewriting it to a for-loop
    /// collect — `{ let mut __fmc: Vec[U] = Vec.new(); for <v> in <recv> {
    /// __fmc.push(<v>); } __fmc }` — reusing the (working) fused-chain FOR-LOOP
    /// lowering of `filter_map` (B-2026-07-19-14). The older collect engine
    /// (`try_compile_iter_adaptor_collect_to_vec`) has no `filter_map` in its
    /// separate `IterAdaptor` peel, so a `filter_map` chain bails there and
    /// would otherwise hit the loud dispatch-fail; this rewrite catches exactly
    /// those (gated to a chain the shared peel recognizes AND that carries a
    /// `FilterMap` step, so it never shadows the old engine's map/filter path).
    /// Mirrors the `zip→map` collect rewrite above. `Ok(None)` when the result
    /// type isn't a recorded `Vec[U]` or the chain has no `filter_map`.
    pub(super) fn try_compile_filter_map_collect(
        &mut self,
        collect_recv: &Expr,
        call_span: &crate::token::Span,
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        // Only for a chain the fused peel understands that actually contains a
        // `filter_map` step — anything else is left to the existing engines.
        let has_filter_map = match Self::peel_fused_map_filter_chain(collect_recv) {
            Some((_, steps)) => steps
                .iter()
                .any(|(k, _, _)| matches!(k, FusedStepKind::FilterMap)),
            None => false,
        };
        if !has_filter_map {
            return Ok(None);
        }
        // Result element type U from the recorded `Vec[U]` at the collect span.
        let vec_te = match self
            .drop_rc
            .owned_temp_drops
            .get(&(call_span.offset, call_span.length))
        {
            Some(te) => te.clone(),
            None => return Ok(None),
        };
        let u_elem = match &vec_te.kind {
            TypeKind::Path(p) if p.segments.last().map(|s| s.as_str()) == Some("Vec") => {
                match p.generic_args.as_ref().and_then(|ga| ga.first()) {
                    Some(GenericArg::Type(t)) => t.clone(),
                    _ => return Ok(None),
                }
            }
            _ => return Ok(None),
        };

        let uid = self.indexed_elem_counter;
        self.indexed_elem_counter += 1;
        let sp = *call_span;
        let acc = format!("__fmc_{}", uid);
        let elem = format!("__fmce_{}", uid);
        let ident = |name: &str| Expr {
            kind: ExprKind::Identifier(name.to_string()),
            span: sp,
        };
        let vec_u = TypeExpr {
            kind: TypeKind::Path(PathExpr {
                segments: vec!["Vec".to_string()],
                generic_args: Some(vec![GenericArg::Type(u_elem)]),
                span: sp,
            }),
            span: sp,
        };
        // `let mut __fmc: Vec[U] = Vec.new();`
        let let_acc = Stmt {
            kind: StmtKind::Let {
                is_mut: true,
                pattern: Pattern {
                    kind: PatternKind::Binding(acc.clone()),
                    span: sp,
                },
                ty: Some(vec_u),
                value: Expr {
                    kind: ExprKind::Call {
                        callee: Box::new(Expr {
                            kind: ExprKind::Path {
                                segments: vec!["Vec".to_string(), "new".to_string()],
                                generic_args: None,
                            },
                            span: sp,
                        }),
                        args: vec![],
                    },
                    span: sp,
                },
            },
            span: sp,
        };
        // `for <elem> in <recv> { __fmc.push(<elem>); }`
        let push_stmt = Stmt {
            kind: StmtKind::Expr(Expr {
                kind: ExprKind::MethodCall {
                    object: Box::new(ident(&acc)),
                    method: "push".to_string(),
                    turbofish: None,
                    args: vec![CallArg {
                        label: None,
                        mut_marker: false,
                        mut_marker_span: None,
                        value: ident(&elem),
                        span: sp,
                    }],
                    args_close_span: sp,
                },
                span: sp,
            }),
            span: sp,
        };
        let for_loop = Stmt {
            kind: StmtKind::Expr(Expr {
                kind: ExprKind::For {
                    label: None,
                    pattern: Pattern {
                        kind: PatternKind::Binding(elem.clone()),
                        span: sp,
                    },
                    iterable: Box::new(collect_recv.clone()),
                    attributes: Vec::new(),
                    body: Block {
                        stmts: vec![push_stmt],
                        final_expr: None,
                        span: sp,
                    },
                },
                span: sp,
            }),
            span: sp,
        };
        let block = Expr {
            kind: ExprKind::Block(Block {
                stmts: vec![let_acc, for_loop],
                final_expr: Some(Box::new(ident(&acc))),
                span: sp,
            }),
            span: sp,
        };
        Ok(Some(self.compile_expr(&block)?))
    }

    /// Lower `A.chain(B).collect()` where EITHER side carries its own adaptors
    /// (`A.iter().map(f).chain(B).collect()`, `A.chain(B.iter().filter(g))`, …)
    /// by recursively collecting each side through the full pipeline and merging
    /// into a shared accumulator (B-2026-07-04-2 sub-part 1). Emitted:
    ///
    /// ```text
    /// { let mut __chv: Vec[E] = <A.collect()>;      // side A via full machinery
    ///   for __chy in <B.collect()> { __chv.push(__chy); }   // merge B (clones)
    ///   __chv }
    /// ```
    ///
    /// Each side's `.collect()` recurses through `compile_method_call` (identity
    /// sources, map/filter/enumerate/…), so an unsupported adaptor on a side
    /// bails to the loud dispatch-fail via the recursive compile — never a
    /// miscompile. `B.collect()`'s fresh temp is iterated-and-dropped, its
    /// elements cloned into `__chv`, so both sides' sources survive and every
    /// buffer is owned once. Returns `Ok(None)` if the result type isn't a
    /// recorded `Vec[E]`.
    pub(super) fn try_compile_chain_pipeline_collect(
        &mut self,
        src_a: &Expr,
        src_b: &Expr,
        call_span: &crate::token::Span,
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        let vec_te = match self
            .drop_rc
            .owned_temp_drops
            .get(&(call_span.offset, call_span.length))
        {
            Some(te) => te.clone(),
            None => return Ok(None),
        };
        if !matches!(
            &vec_te.kind,
            TypeKind::Path(p) if p.segments.last().map(|s| s.as_str()) == Some("Vec")
        ) {
            return Ok(None);
        }
        let uid = self.indexed_elem_counter;
        self.indexed_elem_counter += 1;
        let sp = *call_span;
        let vec_name = format!("__chpv_{}", uid);
        let loop_var = format!("__chpy_{}", uid);
        let ident = |name: &str| Expr {
            kind: ExprKind::Identifier(name.to_string()),
            span: sp,
        };
        // `<side>.collect()` — recurse through the full collect machinery.
        let collect_of = |side: &Expr| Expr {
            kind: ExprKind::MethodCall {
                object: Box::new(side.clone()),
                method: "collect".to_string(),
                turbofish: None,
                args: vec![],
                args_close_span: sp,
            },
            span: sp,
        };
        // `let mut __chpv: Vec[E] = <A.collect()>;`
        let let_vec = Stmt {
            kind: StmtKind::Let {
                is_mut: true,
                pattern: Pattern {
                    kind: PatternKind::Binding(vec_name.clone()),
                    span: sp,
                },
                ty: Some(vec_te),
                value: collect_of(src_a),
            },
            span: sp,
        };
        // `for __chpy in <B.collect()> { __chpv.push(__chpy); }`
        let merge_loop = Stmt {
            kind: StmtKind::Expr(Expr {
                kind: ExprKind::For {
                    label: None,
                    pattern: Pattern {
                        kind: PatternKind::Binding(loop_var.clone()),
                        span: sp,
                    },
                    iterable: Box::new(collect_of(src_b)),
                    attributes: Vec::new(),
                    body: Block {
                        stmts: vec![Stmt {
                            kind: StmtKind::Expr(Expr {
                                kind: ExprKind::MethodCall {
                                    object: Box::new(ident(&vec_name)),
                                    method: "push".to_string(),
                                    turbofish: None,
                                    args: vec![CallArg {
                                        label: None,
                                        mut_marker: false,
                                        mut_marker_span: None,
                                        value: ident(&loop_var),
                                        span: sp,
                                    }],
                                    args_close_span: sp,
                                },
                                span: sp,
                            }),
                            span: sp,
                        }],
                        final_expr: None,
                        span: sp,
                    },
                },
                span: sp,
            }),
            span: sp,
        };
        let block = Expr {
            kind: ExprKind::Block(Block {
                stmts: vec![let_vec, merge_loop],
                final_expr: Some(Box::new(ident(&vec_name))),
                span: sp,
            }),
            span: sp,
        };
        Ok(Some(self.compile_expr(&block)?))
    }

    /// Lower `<src>.cycle().take(n).collect()` — repeat the identity source
    /// until `n` elements are collected (B-2026-07-04-2 sub-part 1). Emitted:
    ///
    /// ```text
    /// { let mut __cyv: Vec[E] = Vec.new();
    ///   let __cyn = <n>;
    ///   let mut __cyc = 0;
    ///   while __cyc < __cyn {
    ///     let __cystart = __cyc;
    ///     for __cyx in <src> {
    ///       if __cyc >= __cyn { break; }
    ///       __cyv.push(__cyx);
    ///       __cyc = __cyc + 1;
    ///     }
    ///     if __cyc == __cystart { break; }   // empty source → stop (no infinite loop)
    ///   }
    ///   __cyv }
    /// ```
    ///
    /// Each `for __cyx in <src>` over the borrowed source clones on `push`, so
    /// the source survives and the accumulator owns independent copies. The
    /// empty-source guard prevents a non-terminating loop when `<src>` yields
    /// nothing. Returns `Ok(None)` if the result type isn't a recorded `Vec[E]`.
    pub(super) fn try_compile_cycle_take_collect(
        &mut self,
        src: &Expr,
        n: &Expr,
        call_span: &crate::token::Span,
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        let vec_te = match self
            .drop_rc
            .owned_temp_drops
            .get(&(call_span.offset, call_span.length))
        {
            Some(te) => te.clone(),
            None => return Ok(None),
        };
        if !matches!(
            &vec_te.kind,
            TypeKind::Path(p) if p.segments.last().map(|s| s.as_str()) == Some("Vec")
        ) {
            return Ok(None);
        }
        let uid = self.indexed_elem_counter;
        self.indexed_elem_counter += 1;
        let sp = *call_span;
        let vname = format!("__cyv_{}", uid);
        let nname = format!("__cyn_{}", uid);
        let cname = format!("__cyc_{}", uid);
        let sname = format!("__cystart_{}", uid);
        let xname = format!("__cyx_{}", uid);
        let ident = |name: &str| Expr {
            kind: ExprKind::Identifier(name.to_string()),
            span: sp,
        };
        let i64_lit = |v: i64| Expr {
            kind: ExprKind::Integer(v, Some(crate::token::IntSuffix::I64)),
            span: sp,
        };
        let bin = |op: BinOp, l: Expr, r: Expr| Expr {
            kind: ExprKind::Binary {
                op,
                left: Box::new(l),
                right: Box::new(r),
            },
            span: sp,
        };
        let let_stmt = |is_mut: bool, name: &str, ty: Option<TypeExpr>, value: Expr| Stmt {
            kind: StmtKind::Let {
                is_mut,
                pattern: Pattern {
                    kind: PatternKind::Binding(name.to_string()),
                    span: sp,
                },
                ty,
                value,
            },
            span: sp,
        };
        let assign = |name: &str, value: Expr| Stmt {
            kind: StmtKind::Assign {
                target: ident(name),
                value,
            },
            span: sp,
        };
        let break_stmt = || Stmt {
            kind: StmtKind::Expr(Expr {
                kind: ExprKind::Break {
                    label: None,
                    value: None,
                },
                span: sp,
            }),
            span: sp,
        };
        let if_break = |cond: Expr| Stmt {
            kind: StmtKind::Expr(Expr {
                kind: ExprKind::If {
                    condition: Box::new(cond),
                    then_block: Block {
                        stmts: vec![break_stmt()],
                        final_expr: None,
                        span: sp,
                    },
                    else_branch: None,
                },
                span: sp,
            }),
            span: sp,
        };
        let vec_new = Expr {
            kind: ExprKind::Call {
                callee: Box::new(Expr {
                    kind: ExprKind::Path {
                        segments: vec!["Vec".to_string(), "new".to_string()],
                        generic_args: None,
                    },
                    span: sp,
                }),
                args: vec![],
            },
            span: sp,
        };
        // Inner for-loop body.
        let for_body = vec![
            if_break(bin(BinOp::GtEq, ident(&cname), ident(&nname))),
            Stmt {
                kind: StmtKind::Expr(Expr {
                    kind: ExprKind::MethodCall {
                        object: Box::new(ident(&vname)),
                        method: "push".to_string(),
                        turbofish: None,
                        args: vec![CallArg {
                            label: None,
                            mut_marker: false,
                            mut_marker_span: None,
                            value: ident(&xname),
                            span: sp,
                        }],
                        args_close_span: sp,
                    },
                    span: sp,
                }),
                span: sp,
            },
            assign(&cname, bin(BinOp::Add, ident(&cname), i64_lit(1))),
        ];
        let for_loop = Stmt {
            kind: StmtKind::Expr(Expr {
                kind: ExprKind::For {
                    label: None,
                    pattern: Pattern {
                        kind: PatternKind::Binding(xname.clone()),
                        span: sp,
                    },
                    iterable: Box::new(src.clone()),
                    attributes: Vec::new(),
                    body: Block {
                        stmts: for_body,
                        final_expr: None,
                        span: sp,
                    },
                },
                span: sp,
            }),
            span: sp,
        };
        // Outer while body.
        let while_body = vec![
            let_stmt(false, &sname, None, ident(&cname)),
            for_loop,
            if_break(bin(BinOp::Eq, ident(&cname), ident(&sname))),
        ];
        let while_loop = Stmt {
            kind: StmtKind::Expr(Expr {
                kind: ExprKind::While {
                    label: None,
                    condition: Box::new(bin(BinOp::Lt, ident(&cname), ident(&nname))),
                    body: Block {
                        stmts: while_body,
                        final_expr: None,
                        span: sp,
                    },
                    attributes: Vec::new(),
                },
                span: sp,
            }),
            span: sp,
        };
        let block = Expr {
            kind: ExprKind::Block(Block {
                stmts: vec![
                    let_stmt(true, &vname, Some(vec_te), vec_new),
                    let_stmt(false, &nname, None, n.clone()),
                    let_stmt(true, &cname, None, i64_lit(0)),
                    while_loop,
                ],
                final_expr: Some(Box::new(ident(&vname))),
                span: sp,
            }),
            span: sp,
        };
        Ok(Some(self.compile_expr(&block)?))
    }

    /// Lower `<src>.scan(init, |acc, x| <body → Option[(A, U)]>).collect()`
    /// (B-2026-07-04-2 sub-part 1). Emitted:
    ///
    /// ```text
    /// { let mut __scv: Vec[U] = Vec.new();
    ///   let mut __sacc = <init>;
    ///   for <x_p> in <src> {
    ///     let <acc_p> = __sacc;
    ///     let __sr = <body>;                 // Option[(A, U)]
    ///     if __sr.is_none() { break; }
    ///     let __st = __sr.unwrap();           // (A, U)
    ///     __sacc = __st.0;                    // next accumulator
    ///     __scv.push(__st.1);                 // output
    ///   }
    ///   __scv }
    /// ```
    ///
    /// The x-param is the for-loop variable (so it inherits the source's element
    /// type); the acc-param binds the running accumulator each iteration. `None`
    /// stops the scan (mirroring the interpreter). Uses `.is_none()`/`.unwrap()`
    /// rather than an `Option` pattern-match — a match's `None` arm parses as
    /// `Binding("None")` which post-resolver synthetic AST would treat as a
    /// catch-all. Returns `Ok(None)` if the result type isn't a recorded
    /// `Vec[U]`.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn try_compile_scan_collect(
        &mut self,
        src: &Expr,
        init: &Expr,
        acc_p: &str,
        x_p: &str,
        body: &Expr,
        call_span: &crate::token::Span,
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        let vec_te = match self
            .drop_rc
            .owned_temp_drops
            .get(&(call_span.offset, call_span.length))
        {
            Some(te) => te.clone(),
            None => return Ok(None),
        };
        if !matches!(
            &vec_te.kind,
            TypeKind::Path(p) if p.segments.last().map(|s| s.as_str()) == Some("Vec")
        ) {
            return Ok(None);
        }
        let uid = self.indexed_elem_counter;
        self.indexed_elem_counter += 1;
        let sp = *call_span;
        let vname = format!("__scv_{}", uid);
        let accname = format!("__sacc_{}", uid);
        let tname = format!("__st_{}", uid);
        let ident = |name: &str| Expr {
            kind: ExprKind::Identifier(name.to_string()),
            span: sp,
        };
        let let_stmt = |is_mut: bool, name: &str, ty: Option<TypeExpr>, value: Expr| Stmt {
            kind: StmtKind::Let {
                is_mut,
                pattern: Pattern {
                    kind: PatternKind::Binding(name.to_string()),
                    span: sp,
                },
                ty,
                value,
            },
            span: sp,
        };
        let assign = |name: &str, value: Expr| Stmt {
            kind: StmtKind::Assign {
                target: ident(name),
                value,
            },
            span: sp,
        };
        // Extract the inner tuple of a direct `Some(<tuple>)` body. A body that
        // conditionally returns `None` (or isn't a direct `Some(...)`) bails to
        // the loud dispatch-fail — extracting the tuple sidesteps needing the
        // `Option`'s type name (synthetic AST has no typechecker record for a
        // fresh `let __sr = <body>`, so `.is_none()`/`.unwrap()` wouldn't
        // dispatch). The common `|acc, x| Some((new, out))` shape is covered.
        let callee_is_some = |callee: &Expr| -> bool {
            match &callee.kind {
                ExprKind::Identifier(n) => n == "Some",
                ExprKind::Path { segments, .. } => {
                    segments.last().map(|s| s.as_str()) == Some("Some")
                }
                _ => false,
            }
        };
        let inner_tuple = match &body.kind {
            ExprKind::Call { callee, args } if args.len() == 1 && callee_is_some(callee) => {
                args[0].value.clone()
            }
            _ => return Ok(None),
        };
        let tuple_idx = |recv: Expr, idx: u64| Expr {
            kind: ExprKind::TupleIndex {
                object: Box::new(recv),
                index: idx,
            },
            span: sp,
        };
        let push_out = Stmt {
            kind: StmtKind::Expr(Expr {
                kind: ExprKind::MethodCall {
                    object: Box::new(ident(&vname)),
                    method: "push".to_string(),
                    turbofish: None,
                    args: vec![CallArg {
                        label: None,
                        mut_marker: false,
                        mut_marker_span: None,
                        value: tuple_idx(ident(&tname), 1),
                        span: sp,
                    }],
                    args_close_span: sp,
                },
                span: sp,
            }),
            span: sp,
        };
        let for_body = vec![
            let_stmt(false, acc_p, None, ident(&accname)),
            let_stmt(false, &tname, None, inner_tuple),
            assign(&accname, tuple_idx(ident(&tname), 0)),
            push_out,
        ];
        let for_loop = Stmt {
            kind: StmtKind::Expr(Expr {
                kind: ExprKind::For {
                    label: None,
                    pattern: Pattern {
                        kind: PatternKind::Binding(x_p.to_string()),
                        span: sp,
                    },
                    iterable: Box::new(src.clone()),
                    attributes: Vec::new(),
                    body: Block {
                        stmts: for_body,
                        final_expr: None,
                        span: sp,
                    },
                },
                span: sp,
            }),
            span: sp,
        };
        let vec_new = Expr {
            kind: ExprKind::Call {
                callee: Box::new(Expr {
                    kind: ExprKind::Path {
                        segments: vec!["Vec".to_string(), "new".to_string()],
                        generic_args: None,
                    },
                    span: sp,
                }),
                args: vec![],
            },
            span: sp,
        };
        let block = Expr {
            kind: ExprKind::Block(Block {
                stmts: vec![
                    let_stmt(true, &vname, Some(vec_te), vec_new),
                    let_stmt(true, &accname, None, init.clone()),
                    for_loop,
                ],
                final_expr: Some(Box::new(ident(&vname))),
                span: sp,
            }),
            span: sp,
        };
        Ok(Some(self.compile_expr(&block)?))
    }

    /// Peel `map`/`filter`/`take_while`/`skip_while`/`take`/`skip`/`step_by`/
    /// `inspect` adaptors off a fused iterator chain, returning the base
    /// source and the adaptors in source order — the shared front half of the
    /// `fold` terminal (B-2026-07-11-17), the `for`-over-chain desugar
    /// (B-2026-07-11-18), and the other fused terminals. Each step is
    /// `(kind, closure_param, body_or_pred_or_count)`.
    ///
    /// The base must be a source the `for`-loop already iterates CORRECTLY on its
    /// own (an identity iterator source or a range); anything else — a plain
    /// collection value, or an unrecognized adaptor MethodCall
    /// (`enumerate`/`zip`/`flat_map`/…) that the `for` lowering silently iterates
    /// zero times — returns `None` so the caller fails closed rather than emit a
    /// wrong-answer loop. A non-single-`Binding` adaptor closure also returns
    /// `None`. The base-source requirement is also what keeps the peel safe
    /// against NON-iterator methods that share a name (`Option.take()` takes no
    /// args and breaks the walk; a 1-arg `take` on a user type leaves a base
    /// that fails the source check).
    #[allow(clippy::type_complexity)]
    pub(super) fn peel_fused_map_filter_chain(recv: &Expr) -> Option<(&Expr, Vec<FusedChainStep>)> {
        let mut steps: Vec<FusedChainStep> = Vec::new();
        let mut base = recv;
        while let ExprKind::MethodCall {
            object,
            method,
            args,
            ..
        } = &base.kind
        {
            // `peekable()` in a CHAIN position is a pure identity — the
            // `Peekable` wrapper only changes behavior through `.peek()`
            // calls on a materialized iterator binding, which a fused chain
            // never has (B-2026-07-14-8, peekable leg). Peel it off with no
            // step.
            if method == "peekable" && args.is_empty() {
                base = object;
                continue;
            }
            let kind = match method.as_str() {
                "map" => FusedStepKind::Map,
                "filter" => FusedStepKind::Filter,
                "filter_map" => FusedStepKind::FilterMap,
                "take_while" => FusedStepKind::TakeWhile,
                "skip_while" => FusedStepKind::SkipWhile,
                "take" => FusedStepKind::Take,
                "skip" => FusedStepKind::Skip,
                "step_by" => FusedStepKind::StepBy,
                "inspect" => FusedStepKind::Inspect,
                _ => break,
            };
            if args.len() != 1 {
                break;
            }
            if matches!(
                kind,
                FusedStepKind::Take | FusedStepKind::Skip | FusedStepKind::StepBy
            ) {
                // Count-argument adaptors: a single integer expression (bound
                // once, pre-loop). A closure argument here is malformed for
                // these methods — fail closed.
                if matches!(&args[0].value.kind, ExprKind::Closure { .. }) {
                    return None;
                }
                steps.push((kind, String::new(), args[0].value.clone()));
                base = object;
                continue;
            }
            let ExprKind::Closure { params, body, .. } = &args[0].value.kind else {
                return None;
            };
            if params.len() != 1 {
                return None;
            }
            // A wildcard adaptor param (`map(|_| ..)`) binds to a fresh throwaway
            // name (the interpreter already accepts it, B-2026-07-11-19); a
            // destructuring/complex param bails (fail closed).
            let param = match &params[0].pattern.kind {
                PatternKind::Binding(param) => param.clone(),
                PatternKind::Wildcard => format!("__pw_{}", steps.len()),
                _ => return None,
            };
            steps.push((kind, param, (**body).clone()));
            base = object;
        }
        steps.reverse(); // outermost-peeled → source order

        let base_ok = match &base.kind {
            ExprKind::MethodCall { method, args, .. } => {
                (args.is_empty()
                    && matches!(
                        method.as_str(),
                        "iter" | "iter_mut" | "into_iter" | "chars" | "bytes" | "keys" | "values"
                    ))
                    || Self::peel_base_is_structural_adaptor(base)
            }
            ExprKind::Range { .. } => true,
            _ => false,
        };
        if !base_ok {
            return None;
        }
        Some((base, steps))
    }

    /// True iff `base` is a STRUCTURAL-adaptor chain the for-loop lowers via
    /// its own desugar (flat_map / cycle / scan / windows / chunks), making it
    /// a valid fused-chain BASE: the fused desugar emits `for <elem> in
    /// <base>` and `compile_for` recursion handles it (B-2026-07-14-8,
    /// post-structural chaining). Composition is sound because the flat_map /
    /// cycle desugars RETARGET unlabeled breaks in their body onto their outer
    /// label — a downstream `take`/`take_while` step's synthesized `break`
    /// therefore exits the whole structure, and the fused steps' hoisted state
    /// (skip_while latches, take/step_by counters) lives OUTSIDE the emitted
    /// `for`, persisting across cycle restarts / flat_map batches exactly like
    /// the interpreter's post-adaptor step list. Shape gates only — a base
    /// that passes here but whose own desugar declines (e.g. heap-element
    /// windows over a non-named source) fails LOUD via the adaptor bail,
    /// never silently.
    pub(super) fn peel_base_is_structural_adaptor(base: &Expr) -> bool {
        let ExprKind::MethodCall {
            object,
            method,
            args,
            ..
        } = &base.kind
        else {
            return false;
        };
        match method.as_str() {
            "flat_map" => Self::for_loop_iterates_flat_map(base),
            "flatten" if args.is_empty() => Self::for_loop_iterates_flatten(base),
            "cycle" if args.is_empty() => Self::peel_fused_map_filter_chain(object).is_some(),
            "scan" if args.len() == 2 => {
                let ExprKind::Closure { params, body, .. } = &args[1].value.kind else {
                    return false;
                };
                params.len() == 2
                    && params
                        .iter()
                        .all(|p| matches!(&p.pattern.kind, PatternKind::Binding(_)))
                    && matches!(
                        &body.kind,
                        ExprKind::Call { callee, args: cargs } if cargs.len() == 1
                            && matches!(
                                &callee.kind,
                                ExprKind::Identifier(n) if n == "Some"
                            )
                    )
                    && Self::peel_fused_map_filter_chain(object).is_some()
            }
            "windows" | "chunks" if args.len() == 1 => {
                !matches!(&args[0].value.kind, ExprKind::Closure { .. })
                    && Self::peel_fused_map_filter_chain(object).is_some()
            }
            "chunk_by" if args.len() == 1 => {
                matches!(
                    &args[0].value.kind,
                    ExprKind::Closure { params, .. }
                        if params.len() == 1
                            && matches!(&params[0].pattern.kind, PatternKind::Binding(_))
                ) && Self::peel_fused_map_filter_chain(object).is_some()
            }
            _ => false,
        }
    }

    /// Materialized-iterator substitution (B-2026-07-11-19): if `recv`'s base
    /// receiver is a name bound by a recorded `let it = <iter-chain>`, return a
    /// copy of `recv` with that base replaced by the recorded chain expr (so
    /// `it.map(f).fold(..)` becomes `v.iter().map(f).fold(..)`, which the fused
    /// terminals/adaptors handle). Returns `None` when no recorded name appears
    /// at the base — the caller then compiles `recv` unchanged.
    pub(super) fn substitute_iter_let_receiver(&self, recv: &Expr) -> Option<Expr> {
        match &recv.kind {
            ExprKind::Identifier(name) => self.iter_let_bindings.get(name).cloned(),
            ExprKind::MethodCall {
                object,
                method,
                turbofish,
                args,
                args_close_span,
            } => {
                let new_object = self.substitute_iter_let_receiver(object)?;
                Some(Expr {
                    kind: ExprKind::MethodCall {
                        object: Box::new(new_object),
                        method: method.clone(),
                        turbofish: turbofish.clone(),
                        args: args.clone(),
                        args_close_span: *args_close_span,
                    },
                    span: recv.span,
                })
            }
            _ => None,
        }
    }

    /// True iff `e` is a fusable iterator chain whose base is a genuine
    /// `.iter()` / `.iter_mut()` / `.into_iter()` (possibly under `map`/`filter`
    /// adaptors) — the shape a `let it = <e>` binding can be materialized-inlined
    /// instead of codegen'd as a (nonexistent) runtime iterator value
    /// (B-2026-07-11-19). Deliberately NARROWER than `peel`'s base set: `chars` /
    /// `bytes` / `keys` / `values` are excluded because codegen materializes them
    /// to an INDEXABLE collection (`let b = s.bytes(); b[0]`), and a bare `Range`
    /// is excluded because `let r = a..b; for i in r` is the dominant use and
    /// ranges compile to real values — inlining either would break a
    /// non-iterator use. An `X.iter()` binding, by contrast, is `Iterator[T]`:
    /// its only uses are adaptor/terminal method calls (handled by
    /// `substitute_iter_let_receiver`) and `for` loops (handled in `compile_for`).
    pub(super) fn is_materializable_iter_chain(&self, e: &Expr) -> bool {
        // Sound gate: the expr must be typed `Iterator` by the typechecker. This
        // is what distinguishes `Vec.iter()` (a real iterator) from an `.iter()`
        // that returns a collection (`Column.iter() -> Vec[Option[T]]`) — the
        // latter is never in `iterator_typed_exprs`, so it is left to compile as
        // the value it is. The span is the RHS expr's own span (preserved across
        // `substitute_iter_let_receiver`, which clones the original span).
        if !self
            .span_tables
            .iterator_typed_exprs
            .contains(&(e.span.offset, e.span.length))
        {
            return false;
        }
        let Some((base, _steps)) = Self::peel_fused_map_filter_chain(e) else {
            return false;
        };
        matches!(
            &base.kind,
            ExprKind::MethodCall { method, .. }
                if matches!(method.as_str(), "iter" | "iter_mut" | "into_iter")
        )
    }

    /// The bound name for a single iterator-terminal closure param, synthesizing
    /// a fresh throwaway for a `_` wildcard (the interpreter accepts `|_|` /
    /// `|a, _|` — e.g. the `fold(0, |a, _| a + 1)` count idiom; this aligns
    /// codegen, B-2026-07-11-19). Returns `None` for a destructuring / complex
    /// param so the caller fails closed. The wildcard seed is fixed per role and
    /// only ever names an UNREFERENCED binding (the body ignores a `_` param), and
    /// each terminal desugars into its own block scope, so a fixed name cannot
    /// collide across sites.
    pub(super) fn closure_param_name(pat: &Pattern, wildcard_seed: &str) -> Option<String> {
        match &pat.kind {
            PatternKind::Binding(n) => Some(n.clone()),
            PatternKind::Wildcard => Some(wildcard_seed.to_string()),
            _ => None,
        }
    }

    /// Deterministic, collision-safe synthetic span for a `filter_map` step's
    /// `Some(<payload>)` match-arm binding, derived from the closure body's
    /// span. Placed in a high reserved offset region (well away from any real
    /// source offset AND from `reduce`'s `usize::MAX - uid` region) so
    /// `pattern_binding_types` keys don't collide; unique per `filter_map` step
    /// because every step's closure body has a distinct source offset. Both the
    /// `build_fused_chain_body` `FilterMap` arm (which stamps the binding with
    /// this span) and the typechecker's `filter_map` arm (`stdlib_iter.rs`,
    /// which registers the payload's surface type at it via
    /// `record_pattern_binding_surface_types`) MUST derive the span the same
    /// way — the formula is duplicated there and kept in lockstep by comment.
    pub(super) fn filter_map_bind_span(body_span: &crate::token::Span) -> crate::token::Span {
        crate::token::Span {
            line: body_span.line,
            column: body_span.column,
            offset: usize::MAX / 2 - body_span.offset,
            length: 1,
        }
    }

    /// Thread the "current element" expression through a peeled fused chain
    /// (source order), emitting `if <pred> { … }` for a filter, a
    /// `let <param> = <current>`-bound body for a map, an
    /// `if <pred> { … } else { break }` for a take_while, and a latch-gated
    /// `if !<flag> && <pred> {} else { <flag> = true; … }` for a skip_while —
    /// with the collect engine's bind-or-elide (when `current` already IS the
    /// stage param, use the body directly to keep the loop var's element
    /// type). At the terminal the caller's `sink` turns the fully-adapted
    /// element into the loop-body statements (a `push` for collect, an
    /// accumulate for fold, the user's body for `for`). Shared by the fused
    /// terminals and the `for`-over-chain desugar.
    ///
    /// `sw_prefix` names the skip_while latch flags (`{sw_prefix}{step-index}`);
    /// the CALLER must hoist the matching `let mut <flag> = false;` decls
    /// (`fused_chain_prelude`) BEFORE the loop. `break` inside a
    /// take_while step targets the enclosing synthesized/user `for`, which is
    /// exactly the "iteration ends here" semantics — in a terminal desugar the
    /// accumulator block continues after the loop.
    pub(super) fn build_fused_chain_body(
        steps: &[FusedChainStep],
        i: usize,
        current: Expr,
        sink: &dyn Fn(Expr) -> Vec<Stmt>,
        sw_prefix: &str,
        sp: &crate::token::Span,
    ) -> Vec<Stmt> {
        if i == steps.len() {
            return sink(current);
        }
        let let_bind = |name: &str, value: Expr| Stmt {
            kind: StmtKind::Let {
                is_mut: false,
                pattern: Pattern {
                    kind: PatternKind::Binding(name.to_string()),
                    span: *sp,
                },
                ty: None,
                value,
            },
            span: *sp,
        };
        let ident = |name: &str| Expr {
            kind: ExprKind::Identifier(name.to_string()),
            span: *sp,
        };
        let current_is = |name: &str| matches!(&current.kind, ExprKind::Identifier(n) if n == name);
        let (kind, param, body) = &steps[i];
        let bind_or_use = |expr: &Expr| -> Expr {
            if current_is(param) {
                expr.clone()
            } else {
                Expr {
                    kind: ExprKind::Block(Block {
                        stmts: vec![let_bind(param, current.clone())],
                        final_expr: Some(Box::new(expr.clone())),
                        span: *sp,
                    }),
                    span: *sp,
                }
            }
        };
        let if_stmt =
            |condition: Expr, then_stmts: Vec<Stmt>, else_stmts: Option<Vec<Stmt>>| Stmt {
                kind: StmtKind::Expr(Expr {
                    kind: ExprKind::If {
                        condition: Box::new(condition),
                        then_block: Block {
                            stmts: then_stmts,
                            final_expr: None,
                            span: *sp,
                        },
                        else_branch: else_stmts.map(|s| {
                            Box::new(Expr {
                                kind: ExprKind::Block(Block {
                                    stmts: s,
                                    final_expr: None,
                                    span: *sp,
                                }),
                                span: *sp,
                            })
                        }),
                    },
                    span: *sp,
                }),
                span: *sp,
            };
        match kind {
            FusedStepKind::Filter => {
                // `if <pred> { <rest> }` — filter is identity on the element it
                // lets through, so `current` is unchanged downstream.
                let guard = bind_or_use(body);
                let then_stmts =
                    Self::build_fused_chain_body(steps, i + 1, current, sink, sw_prefix, sp);
                vec![if_stmt(guard, then_stmts, None)]
            }
            FusedStepKind::Map => {
                // Map: the transformed value becomes the next stage's element.
                let map_value = bind_or_use(body);
                Self::build_fused_chain_body(steps, i + 1, map_value, sink, sw_prefix, sp)
            }
            FusedStepKind::FilterMap => {
                // `filter_map(f: Fn(T) -> Option[U])` — apply `f` to the current
                // element and `match` its `Option[U]`: a `Some(v)` feeds `v` to
                // the rest of the chain, a `None` drops the element (map+filter
                // fusion). Lowered as a synthesized `match`, reusing the proven
                // Option-match codegen. The `Some(<fresh>)` payload binding gets
                // a DETERMINISTIC unique span (`filter_map_bind_span`, derived
                // from the closure body's span) whose surface payload type the
                // typechecker's `filter_map` arm (`stdlib_iter.rs`) pre-registers
                // in `pattern_binding_types` at the SAME span — so
                // `reconstruct_payload_value` materializes a heap `U`
                // (`String`/`Vec`) or a narrow / float scalar correctly instead
                // of the raw-i64 default (the same span-registration trick
                // `reduce` uses, B-2026-07-17-11).
                let opt_expr = bind_or_use(body);
                let fresh = format!("{sw_prefix}fm{i}");
                let bind_span = Self::filter_map_bind_span(&body.span);
                let then_stmts =
                    Self::build_fused_chain_body(steps, i + 1, ident(&fresh), sink, sw_prefix, sp);
                let block_of = |stmts: Vec<Stmt>| Expr {
                    kind: ExprKind::Block(Block {
                        stmts,
                        final_expr: None,
                        span: *sp,
                    }),
                    span: *sp,
                };
                let match_expr = Expr {
                    kind: ExprKind::Match {
                        scrutinee: Box::new(opt_expr),
                        arms: vec![
                            MatchArm {
                                pattern: Pattern {
                                    kind: PatternKind::TupleVariant {
                                        path: vec!["Some".to_string()],
                                        patterns: vec![Pattern {
                                            kind: PatternKind::Binding(fresh.clone()),
                                            span: bind_span,
                                        }],
                                    },
                                    span: *sp,
                                },
                                guard: None,
                                body: block_of(then_stmts),
                                span: *sp,
                            },
                            MatchArm {
                                pattern: Pattern {
                                    kind: PatternKind::Binding("None".to_string()),
                                    span: *sp,
                                },
                                guard: None,
                                body: block_of(Vec::new()),
                                span: *sp,
                            },
                        ],
                    },
                    span: *sp,
                };
                vec![Stmt {
                    kind: StmtKind::Expr(match_expr),
                    span: *sp,
                }]
            }
            FusedStepKind::TakeWhile => {
                // `if <pred> { <rest> } else { break }` — the first failing
                // element ends the whole loop; elements that pass flow through
                // unchanged (identity).
                let guard = bind_or_use(body);
                let then_stmts =
                    Self::build_fused_chain_body(steps, i + 1, current, sink, sw_prefix, sp);
                let brk = Stmt {
                    kind: StmtKind::Expr(Expr {
                        kind: ExprKind::Break {
                            label: None,
                            value: None,
                        },
                        span: *sp,
                    }),
                    span: *sp,
                };
                vec![if_stmt(guard, then_stmts, Some(vec![brk]))]
            }
            FusedStepKind::SkipWhile => {
                // `if !<flag> && <pred> {} else { <flag> = true; <rest> }` —
                // while the latch is unset and the predicate holds, drop the
                // element; the first failing element sets the latch and every
                // element from there on flows through (the `&&` short-circuit
                // means the predicate is never re-evaluated once latched,
                // matching skip_while's contract). The `<flag> = true`
                // re-assignment after latching is idempotent.
                let flag = format!("{sw_prefix}{i}");
                let guard = bind_or_use(body);
                let cond = Expr {
                    kind: ExprKind::Binary {
                        op: BinOp::And,
                        left: Box::new(Expr {
                            kind: ExprKind::Unary {
                                op: UnaryOp::Not,
                                operand: Box::new(ident(&flag)),
                            },
                            span: *sp,
                        }),
                        right: Box::new(guard),
                    },
                    span: *sp,
                };
                let mut else_stmts = vec![Stmt {
                    kind: StmtKind::Assign {
                        target: ident(&flag),
                        value: Expr {
                            kind: ExprKind::Bool(true),
                            span: *sp,
                        },
                    },
                    span: *sp,
                }];
                else_stmts.extend(Self::build_fused_chain_body(
                    steps,
                    i + 1,
                    current,
                    sink,
                    sw_prefix,
                    sp,
                ));
                vec![if_stmt(cond, Vec::new(), Some(else_stmts))]
            }
            FusedStepKind::Take => {
                // `if <cnt> >= <n> { break }  <cnt> = <cnt> + 1;  <rest>` —
                // pass the first `n` elements reaching this stage, then end
                // the loop (mirrors the collect engine's Take arm; a negative
                // count means `0 >= n` on the first element — yields nothing,
                // matching the interpreter's clamp-to-0).
                let cnt = format!("{sw_prefix}{i}");
                let n = format!("{sw_prefix}n{i}");
                let exhausted = Expr {
                    kind: ExprKind::Binary {
                        op: BinOp::GtEq,
                        left: Box::new(ident(&cnt)),
                        right: Box::new(ident(&n)),
                    },
                    span: *sp,
                };
                let brk = Stmt {
                    kind: StmtKind::Expr(Expr {
                        kind: ExprKind::Break {
                            label: None,
                            value: None,
                        },
                        span: *sp,
                    }),
                    span: *sp,
                };
                let mut out = vec![
                    if_stmt(exhausted, vec![brk], None),
                    Self::fused_counter_incr(&cnt, sp),
                ];
                out.extend(Self::build_fused_chain_body(
                    steps,
                    i + 1,
                    current,
                    sink,
                    sw_prefix,
                    sp,
                ));
                out
            }
            FusedStepKind::Skip => {
                // `if <cnt> < <n> { <cnt> = <cnt> + 1 } else { <rest> }` —
                // swallow the first `n` elements reaching this stage, pass the
                // rest through (a negative count means `0 < n` is false — skips
                // nothing, matching the interpreter's clamp-to-0).
                let cnt = format!("{sw_prefix}{i}");
                let n = format!("{sw_prefix}n{i}");
                let still_skipping = Expr {
                    kind: ExprKind::Binary {
                        op: BinOp::Lt,
                        left: Box::new(ident(&cnt)),
                        right: Box::new(ident(&n)),
                    },
                    span: *sp,
                };
                let then_stmts = vec![Self::fused_counter_incr(&cnt, sp)];
                let els = Self::build_fused_chain_body(steps, i + 1, current, sink, sw_prefix, sp);
                vec![if_stmt(still_skipping, then_stmts, Some(els))]
            }
            FusedStepKind::StepBy => {
                // `if <cnt> % <n> == 0 { <rest> }  <cnt> = <cnt> + 1;` — yield
                // elements at positions 0, n, 2n, … relative to this stage's
                // input, advancing the counter on every element. `<n>` is the
                // pre-clamped stride from the prelude (`max(count, 1)`,
                // matching the interpreter — a raw `% 0` would trap).
                let cnt = format!("{sw_prefix}{i}");
                let n = format!("{sw_prefix}n{i}");
                let modulo = Expr {
                    kind: ExprKind::Binary {
                        op: BinOp::Mod,
                        left: Box::new(ident(&cnt)),
                        right: Box::new(ident(&n)),
                    },
                    span: *sp,
                };
                let on_stride = Expr {
                    kind: ExprKind::Binary {
                        op: BinOp::Eq,
                        left: Box::new(modulo),
                        right: Box::new(Self::fused_i64_lit(0, sp)),
                    },
                    span: *sp,
                };
                let rest = Self::build_fused_chain_body(steps, i + 1, current, sink, sw_prefix, sp);
                vec![
                    if_stmt(on_stride, rest, None),
                    Self::fused_counter_incr(&cnt, sp),
                ]
            }
            FusedStepKind::Inspect => {
                // `{ let <param> = <current>; <body> };  <rest>` — run the
                // closure for its side effect (value discarded), element
                // passes through unchanged.
                let side_effect = bind_or_use(body);
                let mut out = vec![Stmt {
                    kind: StmtKind::Expr(side_effect),
                    span: *sp,
                }];
                out.extend(Self::build_fused_chain_body(
                    steps,
                    i + 1,
                    current,
                    sink,
                    sw_prefix,
                    sp,
                ));
                out
            }
        }
    }

    /// `<name> = <name> + 1;` — the shared per-element counter advance for the
    /// count-adaptor fused steps.
    pub(super) fn fused_counter_incr(name: &str, sp: &crate::token::Span) -> Stmt {
        let ident = |n: &str| Expr {
            kind: ExprKind::Identifier(n.to_string()),
            span: *sp,
        };
        Stmt {
            kind: StmtKind::Assign {
                target: ident(name),
                value: Expr {
                    kind: ExprKind::Binary {
                        op: BinOp::Add,
                        left: Box::new(ident(name)),
                        right: Box::new(Self::fused_i64_lit(1, sp)),
                    },
                    span: *sp,
                },
            },
            span: *sp,
        }
    }

    /// An `i64`-suffixed integer literal for synthesized fused-chain AST.
    pub(super) fn fused_i64_lit(n: i64, sp: &crate::token::Span) -> Expr {
        Expr {
            kind: ExprKind::Integer(n, Some(crate::token::IntSuffix::I64)),
            span: *sp,
        }
    }

    /// Hoisted pre-loop state declarations for a peeled fused chain — one
    /// group per stateful step, named off `{sw_prefix}{step-index}` to match
    /// `build_fused_chain_body`'s references. Every fused-terminal /
    /// for-desugar caller emits these BEFORE its synthesized loop; empty when
    /// the chain has no stateful step.
    ///
    /// - `skip_while` at step i: `let mut {p}{i} = false;` (the latch flag).
    /// - `take`/`skip` at step i: `let {p}n{i}: i64 = <count>;` (bound once —
    ///   a non-trivial count expr must not re-evaluate per element) and
    ///   `let mut {p}{i}: i64 = 0;` (the counter).
    /// - `step_by` at step i: same shape, but the stride is clamped to ≥ 1
    ///   (`let {p}r{i} = <count>; let {p}n{i} = if {p}r{i} < 1 { 1 } else
    ///   { {p}r{i} };`) — the interpreter clamps (`n.max(1)`), and an
    ///   unclamped `% 0` would trap.
    pub(super) fn fused_chain_prelude(
        steps: &[FusedChainStep],
        sw_prefix: &str,
        sp: &crate::token::Span,
    ) -> Vec<Stmt> {
        let i64_ty = || TypeExpr {
            kind: TypeKind::Path(PathExpr {
                segments: vec!["i64".to_string()],
                generic_args: None,
                span: *sp,
            }),
            span: *sp,
        };
        let let_stmt = |name: String, is_mut: bool, ty: Option<TypeExpr>, value: Expr| Stmt {
            kind: StmtKind::Let {
                is_mut,
                pattern: Pattern {
                    kind: PatternKind::Binding(name),
                    span: *sp,
                },
                ty,
                value,
            },
            span: *sp,
        };
        let ident = |name: &str| Expr {
            kind: ExprKind::Identifier(name.to_string()),
            span: *sp,
        };
        let mut out = Vec::new();
        for (i, (kind, _, count)) in steps.iter().enumerate() {
            match kind {
                FusedStepKind::SkipWhile => {
                    out.push(let_stmt(
                        format!("{sw_prefix}{i}"),
                        true,
                        None,
                        Expr {
                            kind: ExprKind::Bool(false),
                            span: *sp,
                        },
                    ));
                }
                FusedStepKind::Take | FusedStepKind::Skip => {
                    out.push(let_stmt(
                        format!("{sw_prefix}n{i}"),
                        false,
                        Some(i64_ty()),
                        count.clone(),
                    ));
                    out.push(let_stmt(
                        format!("{sw_prefix}{i}"),
                        true,
                        Some(i64_ty()),
                        Self::fused_i64_lit(0, sp),
                    ));
                }
                FusedStepKind::StepBy => {
                    let raw = format!("{sw_prefix}r{i}");
                    out.push(let_stmt(raw.clone(), false, Some(i64_ty()), count.clone()));
                    let clamped = Expr {
                        kind: ExprKind::If {
                            condition: Box::new(Expr {
                                kind: ExprKind::Binary {
                                    op: BinOp::Lt,
                                    left: Box::new(ident(&raw)),
                                    right: Box::new(Self::fused_i64_lit(1, sp)),
                                },
                                span: *sp,
                            }),
                            then_block: Block {
                                stmts: Vec::new(),
                                final_expr: Some(Box::new(Self::fused_i64_lit(1, sp))),
                                span: *sp,
                            },
                            else_branch: Some(Box::new(Expr {
                                kind: ExprKind::Block(Block {
                                    stmts: Vec::new(),
                                    final_expr: Some(Box::new(ident(&raw))),
                                    span: *sp,
                                }),
                                span: *sp,
                            })),
                        },
                        span: *sp,
                    };
                    out.push(let_stmt(
                        format!("{sw_prefix}n{i}"),
                        false,
                        Some(i64_ty()),
                        clamped,
                    ));
                    out.push(let_stmt(
                        format!("{sw_prefix}{i}"),
                        true,
                        Some(i64_ty()),
                        Self::fused_i64_lit(0, sp),
                    ));
                }
                FusedStepKind::Map
                | FusedStepKind::Filter
                | FusedStepKind::FilterMap
                | FusedStepKind::TakeWhile
                | FusedStepKind::Inspect => {}
            }
        }
        out
    }

    /// Lower `<src>.iter().{map|filter}*.fold(init, |acc, x| body)` — a
    /// sequential `fold` terminal on a fused iterator chain — into a synthetic
    /// accumulator loop, mirroring the `collect` desugar's map/filter threading
    /// but with an accumulate sink instead of a `push` (B-2026-07-11-17).
    ///
    /// The `collect` engine (`try_compile_iter_adaptor_collect_to_vec`) is the
    /// only iterator terminal codegen supported; `fold` fell through to the loud
    /// "no handler for method 'fold' on non-identifier receiver" dispatch error
    /// even though the interpreter runs it. Rather than materialize an
    /// intermediate `Vec` (the fused chain's element type isn't recoverable from
    /// codegen's lowering-derived side tables for a *synthetic* `collect`), this
    /// peels the `map`/`filter` adaptors off the receiver down to a base source
    /// the `for`-loop already iterates correctly (`X.iter()` / a plain
    /// collection / a range — NOT another adaptor chain, which the `for` lowering
    /// silently mis-iterates), and emits:
    ///
    /// ```text
    /// { let mut __facc = <init>;
    ///   for <elem> in <base> {
    ///       <filter as `if <pred> { … }`, map as `let <p> = <body>`>
    ///       let <acc_p> = __facc; __facc = <fold_body>;
    ///   }
    ///   __facc }
    /// ```
    ///
    /// Fails closed (`Ok(None)` → the loud dispatch error, never a silent wrong
    /// answer) for any shape it does not fully understand: a non-`map`/`filter`
    /// adaptor in the chain (`enumerate`/`take`/`zip`/…), a non-single-`Binding`
    /// closure, or a base that is itself an unrecognized adaptor MethodCall.
    /// B-2026-08-10-18 — does this closure body contain an explicit `return`?
    ///
    /// The fused iterator emitters SPLICE a closure body into a synthesized
    /// loop that is then compiled in the ENCLOSING function, so a `return` in
    /// it lowers to a real function return: `fold` exited `main` on the first
    /// element (exit 0, remaining output gone), and `map` / `filter` / `any` /
    /// `all` / `retain` left a `ret` mid-block and failed LLVM verification.
    ///
    /// Rather than teach every splice site to retarget the return — which
    /// needs the body's compile call to be owned by the emitter, and these
    /// hand it to the loop compiler embedded in a larger AST — the fast paths
    /// DECLINE this shape and fall back to the general closure lowering, where
    /// the body is a real function body and `return` means what it says.
    ///
    /// Conservative by construction: a false positive costs one fused-pipeline
    /// optimization on a rare shape, a false negative is a silent miscompile.
    /// Reuses the BCE walker rather than a hand-rolled match for that reason —
    /// a bespoke walker that missed an `ExprKind` would fail in the dangerous
    /// direction.
    /// B-2026-08-10-18 — fail the build with an actionable message when a
    /// fused-iterator closure body contains an explicit `return`.
    ///
    /// This is a GUARD, not the fix. The fused emitters are the only
    /// implementation of these terminals — declining them does not fall back
    /// to a general closure lowering, it falls through to
    /// "no handler for method '<m>' on non-identifier receiver … this is a
    /// codegen bug", which sends the reader after the wrong thing entirely.
    ///
    /// What it buys is the `fold` arm, which did not fail at all: the spliced
    /// `return` exited the ENCLOSING function on the first element, so the
    /// program stopped early and still exited 0. Trading a silently truncated
    /// program for a named limitation is worth doing on its own, ahead of the
    /// restructuring the real fix needs (see the row: the emitters splice the
    /// body into a synthesized loop they do not own the compile of, so
    /// B-2026-08-10-16's `ReturnRetarget` cannot be scoped to the body here).
    /// B-2026-08-10-18 — mark a fused-iterator closure body so `compile_expr`
    /// retargets its `return`s to the body's own value.
    ///
    /// A no-op unless the body actually contains one, so the fused pipelines
    /// keep their existing lowering untouched in the overwhelmingly common
    /// case and only bodies that need the retarget pay for it.
    pub(super) fn register_iter_body_retarget(&mut self, body: &Expr) {
        if Self::closure_body_has_explicit_return(body) {
            self.iter_body_retarget_spans
                .insert((body.span.offset, body.span.length));
        }
    }

    pub(super) fn closure_body_has_explicit_return(body: &Expr) -> bool {
        fn walk(e: &Expr) -> bool {
            if matches!(e.kind, ExprKind::Return(_)) {
                return true;
            }
            // A NESTED closure is opaque: a `return` inside it belongs to
            // THAT closure and is compiled as a real closure body, where it
            // works. Descending would reject `xs.iter().map(|n| apply(|m| {
            // … return m; }, n))`, which builds and runs correctly today —
            // measured, after an earlier version of this guard broke it.
            if matches!(e.kind, ExprKind::Closure { .. }) {
                return false;
            }
            !super::bce_length_pin::expr_children_all(e, |c| !walk(c))
        }
        walk(body)
    }

    pub(super) fn try_compile_iter_chain_fold(
        &mut self,
        recv: &Expr,
        init: &Expr,
        acc_p: &str,
        x_p: &str,
        fold_body: &Expr,
        call_span: &crate::token::Span,
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        let (base, steps) = match Self::peel_fused_map_filter_chain(recv) {
            Some(x) => x,
            // A flat_map-terminated receiver is not a fused step, but the
            // synthesized `for <elem> in <recv>` iterates it via the
            // nested-loop desugar (compile_for's flat_map arm) — treat it as
            // a zero-step base (B-2026-07-14-8, flat_map terminals).
            None if Self::for_loop_iterates_flat_map(recv) => (recv, Vec::new()),
            None => return Ok(None),
        };

        let uid = self.indexed_elem_counter;
        self.indexed_elem_counter += 1;
        let sp = *call_span;
        // Loop var: the first adaptor's param keeps the source element typed by
        // the for-loop binding (same reason the collect engine reuses it); with
        // no adaptors the fold element param IS the source element.
        let elem_name = steps
            .iter()
            .find(|(_, p, _)| !p.is_empty())
            .map(|(_, p, _)| p.clone())
            .unwrap_or_else(|| x_p.to_string());

        // Accumulator type annotation (B-2026-07-13-18). The synthetic
        // `let mut <acc> = init` reuses the fold-call span, so the typechecker
        // recorded no `pattern_binding_types` for it — without an explicit
        // annotation a HEAP accumulator (`String`/`Vec`) never registers in
        // `vec_elem_types`/`string_vars`, so the Assign arm's move-machinery
        // (eager-free of the old buffer + f-string staging-slot suppression) is
        // skipped and the accumulator buffer double-frees. Stamp the
        // typechecker-recorded accumulator `TypeExpr` as the annotation so the
        // Let arm's explicit-annotation path registers it exactly as a
        // hand-written `let mut acc: String = …` would. `is_some()` iff the
        // accumulator is heap (`String`/`Vec`); a scalar fold (`fold(0, |a,x|
        // a+x)`) needs no tracking and stays on the un-annotated path.
        let acc_ty_ann = self
            .span_tables
            .iter_terminal_acc_types
            .get(&(call_span.offset, call_span.length))
            .filter(|te| self.is_string_type_expr(te) || self.extract_vec_elem_type(te).is_some())
            .cloned();

        // For a HEAP accumulator, use the closure's `acc` PARAM directly as the
        // accumulator variable — the self-referential `acc = fold_body` shape a
        // hand-written loop uses, which the Assign move-machinery handles
        // cleanly. The alternative (a fresh `__facc` bound via `let acc =
        // __facc`) makes that intermediate `let` a MOVE of the now-tracked
        // accumulator, which zeroes `__facc`'s cap and defeats the eager-free —
        // leaking every intermediate buffer (B-2026-07-13-18, desugar variant b).
        // Using `acc` directly is only sound when it can't collide with a name
        // the receiver introduces: a free/param identifier in `base` or an
        // adaptor closure (which becomes the loop var / an inner element bind and
        // would shadow the accumulator inside the loop body). Scalar accumulators
        // never double-free, so they stay on the proven fresh-name path.
        let acc_collides = || {
            let mut r = std::collections::HashSet::new();
            let mut d = std::collections::HashSet::new();
            self.refs_in_expr(recv, &mut r, &mut d);
            r.contains(acc_p) || d.contains(acc_p) || steps.iter().any(|(_, p, _)| p == acc_p)
        };
        let use_direct_acc = acc_ty_ann.is_some() && !acc_collides();
        // A heap accumulator that CAN'T use the self-referential shape (its `acc`
        // param name collides with a receiver name) has no clean codegen lowering
        // — defer to the interpreter (loud `--interp` fallback) rather than emit
        // the leaking/double-freeing fresh-name form. Rare and pathological.
        if acc_ty_ann.is_some() && !use_direct_acc {
            return Ok(None);
        }
        let accname = if use_direct_acc {
            acc_p.to_string()
        } else {
            format!("__facc_{}", uid)
        };

        let ident = |name: &str| Expr {
            kind: ExprKind::Identifier(name.to_string()),
            span: sp,
        };

        // Accumulate sink: bind the fold element param to the fully-adapted
        // element (elide a redundant self-bind), bind the acc param to the
        // running accumulator (unless the accumulator IS that param already —
        // the direct-acc heap path), then reassign it to the fold body's value.
        let sink = |current: Expr| -> Vec<Stmt> {
            let let_bind = |name: &str, value: Expr| Stmt {
                kind: StmtKind::Let {
                    is_mut: false,
                    pattern: Pattern {
                        kind: PatternKind::Binding(name.to_string()),
                        span: sp,
                    },
                    ty: None,
                    value,
                },
                span: sp,
            };
            let current_is_x = matches!(&current.kind, ExprKind::Identifier(n) if n == x_p);
            let mut out = Vec::new();
            if !current_is_x {
                out.push(let_bind(x_p, current));
            }
            if accname != acc_p {
                out.push(let_bind(acc_p, ident(&accname)));
            }
            out.push(Stmt {
                kind: StmtKind::Assign {
                    target: ident(&accname),
                    value: fold_body.clone(),
                },
                span: sp,
            });
            out
        };
        let sw_prefix = format!("__swf_{uid}_");
        let for_body =
            Self::build_fused_chain_body(&steps, 0, ident(&elem_name), &sink, &sw_prefix, &sp);
        let for_loop = Stmt {
            kind: StmtKind::Expr(Expr {
                kind: ExprKind::For {
                    label: None,
                    pattern: Pattern {
                        kind: PatternKind::Binding(elem_name),
                        span: sp,
                    },
                    iterable: Box::new(base.clone()),
                    attributes: Vec::new(),
                    body: Block {
                        stmts: for_body,
                        final_expr: None,
                        span: sp,
                    },
                },
                span: sp,
            }),
            span: sp,
        };
        let mut block_stmts = vec![Stmt {
            kind: StmtKind::Let {
                is_mut: true,
                pattern: Pattern {
                    kind: PatternKind::Binding(accname.clone()),
                    span: sp,
                },
                ty: acc_ty_ann,
                value: init.clone(),
            },
            span: sp,
        }];
        block_stmts.extend(Self::fused_chain_prelude(&steps, &sw_prefix, &sp));
        block_stmts.push(for_loop);
        let block = Expr {
            kind: ExprKind::Block(Block {
                stmts: block_stmts,
                final_expr: Some(Box::new(ident(&accname))),
                span: sp,
            }),
            span: sp,
        };
        Ok(Some(self.compile_expr(&block)?))
    }

    /// Lower `<src>.iter().{map|filter}*.sum()` — the numeric-accumulation
    /// terminal on a fused iterator chain (B-2026-07-11-19). Desugars to the
    /// `fold` engine with a synthesized `(0 as <elem>)` init and an `acc + x`
    /// body, so the whole shape reuses the shared map/filter fusion. The element
    /// type is the one the typechecker recorded at this MethodCall span
    /// (`iter_terminal_elem_types`); without it — or when `fold`'s peel rejects
    /// the chain — this fails closed (`Ok(None)` → the loud dispatch error),
    /// never a silent wrong answer.
    pub(super) fn try_compile_iter_chain_sum_product(
        &mut self,
        recv: &Expr,
        is_product: bool,
        call_span: &crate::token::Span,
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        let Some(elem_te) = self
            .span_tables
            .iter_terminal_elem_types
            .get(&(call_span.offset, call_span.length))
            .cloned()
        else {
            return Ok(None);
        };
        let sp = *call_span;
        self.indexed_elem_counter += 1;
        let uid = self.indexed_elem_counter;
        let acc_p = format!("__sum_acc_{}", uid);
        let x_p = format!("__sum_x_{}", uid);
        let ident = |name: &str| Expr {
            kind: ExprKind::Identifier(name.to_string()),
            span: sp,
        };
        // `(0 as <elem>)` / `(1 as <elem>)` — a width-correct additive/
        // multiplicative identity for any numeric element type (i8..i64 /
        // isize, u8..u64 / usize, f32 / f64) without spelling a per-type
        // literal suffix (`IntSuffix` has no isize/usize spelling).
        let seed = Expr {
            kind: ExprKind::Cast {
                expr: Box::new(Expr {
                    kind: ExprKind::Integer(if is_product { 1 } else { 0 }, None),
                    span: sp,
                }),
                ty: elem_te,
            },
            span: sp,
        };
        let fold_body = Expr {
            kind: ExprKind::Binary {
                op: if is_product { BinOp::Mul } else { BinOp::Add },
                left: Box::new(ident(&acc_p)),
                right: Box::new(ident(&x_p)),
            },
            span: sp,
        };
        self.try_compile_iter_chain_fold(recv, &seed, &acc_p, &x_p, &fold_body, call_span)
    }

    /// `<iter-chain>.count() -> i64` — the element-count terminal on a fused
    /// iterator chain (B-2026-07-11-19). Desugars to `fold(0, |acc, _x| acc + 1)`
    /// over the peeled base, so every `filter`/`map` adaptor is applied and the
    /// count reflects the post-adaptor element count. The element param is bound
    /// but unused; the accumulator is a plain `i64` (0 → +1), so — unlike `sum`
    /// — no element `TypeExpr` is needed. Fails closed (`Ok(None)`) when the
    /// chain shape isn't one `peel_fused_map_filter_chain` understands, leaving
    /// the materialized-collection `len`/`count` intercept to service it.
    pub(super) fn try_compile_iter_chain_count(
        &mut self,
        recv: &Expr,
        call_span: &crate::token::Span,
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        let sp = *call_span;
        self.indexed_elem_counter += 1;
        let uid = self.indexed_elem_counter;
        let acc_p = format!("__cnt_acc_{}", uid);
        let x_p = format!("__cnt_x_{}", uid);
        let zero = Expr {
            kind: ExprKind::Integer(0, None),
            span: sp,
        };
        let fold_body = Expr {
            kind: ExprKind::Binary {
                op: BinOp::Add,
                left: Box::new(Expr {
                    kind: ExprKind::Identifier(acc_p.clone()),
                    span: sp,
                }),
                right: Box::new(Expr {
                    kind: ExprKind::Integer(1, None),
                    span: sp,
                }),
            },
            span: sp,
        };
        self.try_compile_iter_chain_fold(recv, &zero, &acc_p, &x_p, &fold_body, call_span)
    }

    /// Lower `<src>.iter().{map|filter}*.reduce(|acc, x| body)` — the
    /// `Option[A]`-returning fold terminal (B-2026-07-11-19). Desugars to
    ///
    /// ```text
    /// { let mut __racc: Option[A] = None;
    ///   for <elem> in <base> {
    ///       <adapters>; let <x_p> = <adapted>;
    ///       __racc = match __racc {
    ///           None => Some(<x_p>),
    ///           Some(<acc_p>) => Some(<body>),
    ///       };
    ///   }
    ///   __racc }
    /// ```
    ///
    /// The type-erased 4-word Option layout lets the synthetic `Some(...)` /
    /// `None` construction (`coerce_to_payload_words`) and the tag-dispatched
    /// match (which recognizes a bare `None` binding as a unit variant by tag)
    /// work with no typecheck pass over these nodes. The `Option[A]` annotation
    /// on the accumulator supplies the element type A for the `Some(acc)` payload
    /// binding. Fails closed (`Ok(None)`) when the element type wasn't recorded
    /// or the chain shape isn't one the shared peel understands.
    pub(super) fn try_compile_iter_chain_reduce(
        &mut self,
        recv: &Expr,
        acc_p: &str,
        x_p: &str,
        reduce_body: &Expr,
        call_span: &crate::token::Span,
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        let Some(elem_te) = self
            .span_tables
            .iter_terminal_elem_types
            .get(&(call_span.offset, call_span.length))
            .cloned()
        else {
            return Ok(None);
        };
        // Gate to trivially-copyable (scalar) elements. A heap element (String /
        // Vec / struct) would double-free in the synthetic `Some(acc) =>
        // Some(f(acc, x))` match: the extracted payload is consumed by `f` AND
        // the old accumulator's copy is dropped. Getting that rc-accounting
        // right for arbitrary payloads is the deferred piece — non-Copy elements
        // fall through to the loud `--interp` deferral (the interpreter runs
        // them correctly). Scalar reduce (the common numeric case) is exact.
        //
        // B-2026-08-11-15: the total-order float wrappers ride this path too.
        // `is_trivially_copyable_te` is a PRIMITIVE-name list shared by 36 call
        // sites, so it is not widened — a `{ float }` wrapper is trivially
        // copyable in the sense THIS gate cares about (no heap payload to
        // double-free in the synthetic match), but that is not the same
        // question the other callers ask, and answering it for them here would
        // be a silent change to every one of those sites.
        if !super::vec_method::is_trivially_copyable_te(&elem_te)
            && !Self::is_total_float_wrapper_te(&elem_te)
        {
            return Ok(None);
        }
        let (base, steps) = match Self::peel_fused_map_filter_chain(recv) {
            Some(x) => x,
            // A flat_map-terminated receiver is not a fused step, but the
            // synthesized `for <elem> in <recv>` iterates it via the
            // nested-loop desugar (compile_for's flat_map arm) — treat it as
            // a zero-step base (B-2026-07-14-8, flat_map terminals).
            None if Self::for_loop_iterates_flat_map(recv) => (recv, Vec::new()),
            None => return Ok(None),
        };
        self.indexed_elem_counter += 1;
        let uid = self.indexed_elem_counter;
        let sp = *call_span;
        let raccname = format!("__racc_{}", uid);
        // B-2026-07-17-11: the synthesized `Some(<acc>)` match-arm binding is
        // compiled WITHOUT a typecheck pass, so codegen's payload
        // reconstruction (`reconstruct_payload_value`) has no
        // `pattern_binding_types` entry for it and falls to the raw-i64
        // default — correct for an i64 element, but a FLOAT element then reads
        // the payload word (the float's bit pattern) via `sitofp` (garbage),
        // and a NARROW-INT element (`u8`/`i32`/…) never truncates (`200u8`
        // read back as `-56`). Give the binding a unique synthetic span (well
        // outside any real source offset, distinct per reduce via `uid`) and
        // register the element's surface name there, so the float-bitcast /
        // int-truncation arms fire exactly as they do for a typechecked
        // `match`. Elements are already gated to trivially-copyable scalars,
        // so the last path segment is the surface name (`f64`, `u8`, …);
        // registering `i64`/`u64` is a harmless no-op (word passes through).
        let acc_bind_span = crate::token::Span {
            line: sp.line,
            column: sp.column,
            offset: usize::MAX - uid as usize,
            length: 1,
        };
        let acc_surface_name = match &elem_te.kind {
            TypeKind::Path(p) if p.generic_args.is_none() => p.segments.last().cloned(),
            _ => None,
        };
        if let Some(name) = acc_surface_name {
            self.pattern_state
                .pattern_binding_types
                .insert((acc_bind_span.offset, acc_bind_span.length), name);
        }
        let elem_name = steps
            .iter()
            .find(|(_, p, _)| !p.is_empty())
            .map(|(_, p, _)| p.clone())
            .unwrap_or_else(|| x_p.to_string());
        let ident = |name: &str| Expr {
            kind: ExprKind::Identifier(name.to_string()),
            span: sp,
        };
        // `Option[<elem>]`
        let opt_te = TypeExpr {
            kind: TypeKind::Path(PathExpr {
                segments: vec!["Option".to_string()],
                generic_args: Some(vec![GenericArg::Type(elem_te)]),
                span: sp,
            }),
            span: sp,
        };
        // `Some(<e>)` — the ctor callee is a bare `Identifier` (the form the
        // parser produces and codegen's enum-variant-call recognition expects).
        let some_of = |e: Expr| Expr {
            kind: ExprKind::Call {
                callee: Box::new(Expr {
                    kind: ExprKind::Identifier("Some".to_string()),
                    span: sp,
                }),
                args: vec![CallArg {
                    label: None,
                    mut_marker: false,
                    mut_marker_span: None,
                    value: e,
                    span: sp,
                }],
            },
            span: sp,
        };
        let let_bind = |name: &str, value: Expr| Stmt {
            kind: StmtKind::Let {
                is_mut: false,
                pattern: Pattern {
                    kind: PatternKind::Binding(name.to_string()),
                    span: sp,
                },
                ty: None,
                value,
            },
            span: sp,
        };
        // Sink: bind x_p to the fully-adapted element, then fold `__racc` via a
        // match — seed with `Some(x)` on the first (None) element, else combine.
        let sink = |current: Expr| -> Vec<Stmt> {
            let mut out = Vec::new();
            let current_is_x = matches!(&current.kind, ExprKind::Identifier(n) if n == x_p);
            if !current_is_x {
                out.push(let_bind(x_p, current));
            }
            let match_expr = Expr {
                kind: ExprKind::Match {
                    scrutinee: Box::new(ident(&raccname)),
                    arms: vec![
                        MatchArm {
                            pattern: Pattern {
                                kind: PatternKind::Binding("None".to_string()),
                                span: sp,
                            },
                            guard: None,
                            body: some_of(ident(x_p)),
                            span: sp,
                        },
                        MatchArm {
                            pattern: Pattern {
                                kind: PatternKind::TupleVariant {
                                    path: vec!["Some".to_string()],
                                    patterns: vec![Pattern {
                                        kind: PatternKind::Binding(acc_p.to_string()),
                                        // Unique span registered in
                                        // `pattern_binding_types` above so the
                                        // payload reconstruction knows the
                                        // element's real scalar type
                                        // (B-2026-07-17-11).
                                        span: acc_bind_span,
                                    }],
                                },
                                span: sp,
                            },
                            guard: None,
                            body: some_of(reduce_body.clone()),
                            span: sp,
                        },
                    ],
                },
                span: sp,
            };
            out.push(Stmt {
                kind: StmtKind::Assign {
                    target: ident(&raccname),
                    value: match_expr,
                },
                span: sp,
            });
            out
        };
        let sw_prefix = format!("__swr_{uid}_");
        let for_body =
            Self::build_fused_chain_body(&steps, 0, ident(&elem_name), &sink, &sw_prefix, &sp);
        let for_loop = Stmt {
            kind: StmtKind::Expr(Expr {
                kind: ExprKind::For {
                    label: None,
                    pattern: Pattern {
                        kind: PatternKind::Binding(elem_name),
                        span: sp,
                    },
                    iterable: Box::new(base.clone()),
                    attributes: Vec::new(),
                    body: Block {
                        stmts: for_body,
                        final_expr: None,
                        span: sp,
                    },
                },
                span: sp,
            }),
            span: sp,
        };
        let mut block_stmts = vec![Stmt {
            kind: StmtKind::Let {
                is_mut: true,
                pattern: Pattern {
                    kind: PatternKind::Binding(raccname.clone()),
                    span: sp,
                },
                ty: Some(opt_te),
                value: ident("None"),
            },
            span: sp,
        }];
        block_stmts.extend(Self::fused_chain_prelude(&steps, &sw_prefix, &sp));
        block_stmts.push(for_loop);
        let block = Expr {
            kind: ExprKind::Block(Block {
                stmts: block_stmts,
                final_expr: Some(Box::new(ident(&raccname))),
                span: sp,
            }),
            span: sp,
        };
        Ok(Some(self.compile_expr(&block)?))
    }

    /// Lower `<src>.iter().{map|filter}*.for_each(|x| body)` — the side-effecting
    /// terminal on a fused iterator chain (B-2026-07-11-19). Desugars to a `for`
    /// loop over the peeled base whose body binds the closure param to the
    /// fully-adapted element and runs the closure body for its side effects,
    /// yielding unit. The body INLINES (no closure value is built), so a
    /// capture-mutating body propagates just like `fold`/`any`/`all`. Fails
    /// closed (`Ok(None)` → the loud dispatch error) for a chain shape the shared
    /// peel rejects.
    pub(super) fn try_compile_iter_chain_for_each(
        &mut self,
        recv: &Expr,
        param: &str,
        body: &Expr,
        call_span: &crate::token::Span,
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        let (base, steps) = match Self::peel_fused_map_filter_chain(recv) {
            Some(x) => x,
            // A flat_map-terminated receiver is not a fused step, but the
            // synthesized `for <elem> in <recv>` iterates it via the
            // nested-loop desugar (compile_for's flat_map arm) — treat it as
            // a zero-step base (B-2026-07-14-8, flat_map terminals).
            None if Self::for_loop_iterates_flat_map(recv) => (recv, Vec::new()),
            None => return Ok(None),
        };
        self.indexed_elem_counter += 1;
        let uid = self.indexed_elem_counter;
        let sp = *call_span;
        let elem_name = steps
            .iter()
            .find(|(_, p, _)| !p.is_empty())
            .map(|(_, p, _)| p.clone())
            .unwrap_or_else(|| param.to_string());
        let ident = |name: &str| Expr {
            kind: ExprKind::Identifier(name.to_string()),
            span: sp,
        };

        // Sink: bind the closure param to the fully-adapted element (elide a
        // redundant self-bind), then run the closure body as a statement.
        let sink = |current: Expr| -> Vec<Stmt> {
            let mut out = Vec::new();
            let current_is_param = matches!(&current.kind, ExprKind::Identifier(n) if n == param);
            if !current_is_param {
                out.push(Stmt {
                    kind: StmtKind::Let {
                        is_mut: false,
                        pattern: Pattern {
                            kind: PatternKind::Binding(param.to_string()),
                            span: sp,
                        },
                        ty: None,
                        value: current,
                    },
                    span: sp,
                });
            }
            out.push(Stmt {
                kind: StmtKind::Expr(body.clone()),
                span: sp,
            });
            out
        };
        let sw_prefix = format!("__swe_{uid}_");
        let for_body =
            Self::build_fused_chain_body(&steps, 0, ident(&elem_name), &sink, &sw_prefix, &sp);
        let for_loop = Expr {
            kind: ExprKind::For {
                label: None,
                pattern: Pattern {
                    kind: PatternKind::Binding(elem_name),
                    span: sp,
                },
                iterable: Box::new(base.clone()),
                attributes: Vec::new(),
                body: Block {
                    stmts: for_body,
                    final_expr: None,
                    span: sp,
                },
            },
            span: sp,
        };
        // The terminal yields unit — run the `for` loop as a statement.
        let mut block_stmts = Self::fused_chain_prelude(&steps, &sw_prefix, &sp);
        block_stmts.push(Stmt {
            kind: StmtKind::Expr(for_loop),
            span: sp,
        });
        let block = Expr {
            kind: ExprKind::Block(Block {
                stmts: block_stmts,
                final_expr: None,
                span: sp,
            }),
            span: sp,
        };
        Ok(Some(self.compile_expr(&block)?))
    }

    /// Lower `<src>.iter().{map|filter}*.any(|x| pred)` / `.all(|x| pred)` — the
    /// short-circuit boolean terminals on a fused iterator chain
    /// (B-2026-07-11-19). The typechecker and interpreter already accept them;
    /// only codegen lacked a terminal, so the chain fell through to the loud
    /// "no handler for method 'any'/'all'" dispatch error.
    ///
    /// Reuses the shared map/filter fusion (`peel_fused_map_filter_chain` +
    /// `build_fused_chain_body`) with a short-circuit sink: a boolean result
    /// seeded `false` (`any`) / `true` (`all`), flipped and `break`-ed the first
    /// time the predicate decides the answer. Emits
    ///
    /// ```text
    /// { let mut __aa = <false|true>;
    ///   for <elem> in <base> {
    ///       <adapters>;
    ///       any:  if <pred> { __aa = true;  break; }
    ///       all:  if <pred> {} else { __aa = false; break; }
    ///   }
    ///   __aa }
    /// ```
    ///
    /// Fails closed (`Ok(None)` → the loud dispatch error) for any chain shape
    /// the shared peel rejects.
    pub(super) fn try_compile_iter_chain_any_all(
        &mut self,
        recv: &Expr,
        is_any: bool,
        param: &str,
        pred: &Expr,
        call_span: &crate::token::Span,
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        let (base, steps) = match Self::peel_fused_map_filter_chain(recv) {
            Some(x) => x,
            // A flat_map-terminated receiver is not a fused step, but the
            // synthesized `for <elem> in <recv>` iterates it via the
            // nested-loop desugar (compile_for's flat_map arm) — treat it as
            // a zero-step base (B-2026-07-14-8, flat_map terminals).
            None if Self::for_loop_iterates_flat_map(recv) => (recv, Vec::new()),
            None => return Ok(None),
        };

        self.indexed_elem_counter += 1;
        let uid = self.indexed_elem_counter;
        let sp = *call_span;
        let resname = format!("__aa_{}", uid);
        let elem_name = steps
            .iter()
            .find(|(_, p, _)| !p.is_empty())
            .map(|(_, p, _)| p.clone())
            .unwrap_or_else(|| param.to_string());
        let ident = |name: &str| Expr {
            kind: ExprKind::Identifier(name.to_string()),
            span: sp,
        };
        let bool_lit = |b: bool| Expr {
            kind: ExprKind::Bool(b),
            span: sp,
        };

        // Short-circuit sink: bind the predicate param to the fully-adapted
        // element (elide a redundant self-bind), then on the deciding outcome set
        // the result and `break`.
        let sink = |current: Expr| -> Vec<Stmt> {
            let current_is_param = matches!(&current.kind, ExprKind::Identifier(n) if n == param);
            let guard = if current_is_param {
                pred.clone()
            } else {
                Expr {
                    kind: ExprKind::Block(Block {
                        stmts: vec![Stmt {
                            kind: StmtKind::Let {
                                is_mut: false,
                                pattern: Pattern {
                                    kind: PatternKind::Binding(param.to_string()),
                                    span: sp,
                                },
                                ty: None,
                                value: current,
                            },
                            span: sp,
                        }],
                        final_expr: Some(Box::new(pred.clone())),
                        span: sp,
                    }),
                    span: sp,
                }
            };
            // `__aa = <is_any>; break;` — the deciding outcome.
            let decide = vec![
                Stmt {
                    kind: StmtKind::Assign {
                        target: ident(&resname),
                        value: bool_lit(is_any),
                    },
                    span: sp,
                },
                Stmt {
                    kind: StmtKind::Expr(Expr {
                        kind: ExprKind::Break {
                            label: None,
                            value: None,
                        },
                        span: sp,
                    }),
                    span: sp,
                },
            ];
            // `any`: decide when the predicate holds (then-branch). `all`: decide
            // when it FAILS (else-branch), leaving the then-branch empty.
            let (then_stmts, else_stmts) = if is_any {
                (decide, None)
            } else {
                (Vec::new(), Some(decide))
            };
            vec![Stmt {
                kind: StmtKind::Expr(Expr {
                    kind: ExprKind::If {
                        condition: Box::new(guard),
                        then_block: Block {
                            stmts: then_stmts,
                            final_expr: None,
                            span: sp,
                        },
                        else_branch: else_stmts.map(|s| {
                            Box::new(Expr {
                                kind: ExprKind::Block(Block {
                                    stmts: s,
                                    final_expr: None,
                                    span: sp,
                                }),
                                span: sp,
                            })
                        }),
                    },
                    span: sp,
                }),
                span: sp,
            }]
        };
        let sw_prefix = format!("__swa_{uid}_");
        let for_body =
            Self::build_fused_chain_body(&steps, 0, ident(&elem_name), &sink, &sw_prefix, &sp);
        let for_loop = Stmt {
            kind: StmtKind::Expr(Expr {
                kind: ExprKind::For {
                    label: None,
                    pattern: Pattern {
                        kind: PatternKind::Binding(elem_name),
                        span: sp,
                    },
                    iterable: Box::new(base.clone()),
                    attributes: Vec::new(),
                    body: Block {
                        stmts: for_body,
                        final_expr: None,
                        span: sp,
                    },
                },
                span: sp,
            }),
            span: sp,
        };
        // Seed: `any` starts false, `all` starts true.
        let mut block_stmts = vec![Stmt {
            kind: StmtKind::Let {
                is_mut: true,
                pattern: Pattern {
                    kind: PatternKind::Binding(resname.clone()),
                    span: sp,
                },
                ty: None,
                value: bool_lit(!is_any),
            },
            span: sp,
        }];
        block_stmts.extend(Self::fused_chain_prelude(&steps, &sw_prefix, &sp));
        block_stmts.push(for_loop);
        let block = Expr {
            kind: ExprKind::Block(Block {
                stmts: block_stmts,
                final_expr: Some(Box::new(ident(&resname))),
                span: sp,
            }),
            span: sp,
        };
        Ok(Some(self.compile_expr(&block)?))
    }

    /// `<iter-chain>.position(|x| pred) -> Option[i64]` — the 0-based index of
    /// the first YIELDED element the predicate holds for, or `None`. Desugars to
    /// a fused for-loop with a running index and a short-circuit `break`:
    ///
    /// ```text
    /// { let mut __pos: Option[i64] = None; let mut __idx: i64 = 0;
    ///   for <elem> in <base> { <steps>
    ///       if <pred> { __pos = Some(__idx); break; }
    ///       __idx = __idx + 1; }
    ///   __pos }
    /// ```
    ///
    /// The index counts POST-adaptor elements (each sink invocation). `Ok(None)`
    /// (loud deferral) for a chain shape the fused peel doesn't understand.
    pub(super) fn try_compile_iter_chain_position(
        &mut self,
        recv: &Expr,
        param: &str,
        pred: &Expr,
        call_span: &crate::token::Span,
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        let (base, steps) = match Self::peel_fused_map_filter_chain(recv) {
            Some(x) => x,
            None if Self::for_loop_iterates_flat_map(recv) => (recv, Vec::new()),
            None => return Ok(None),
        };
        self.indexed_elem_counter += 1;
        let uid = self.indexed_elem_counter;
        let sp = *call_span;
        let posname = format!("__pos_{}", uid);
        let idxname = format!("__idx_{}", uid);
        let elem_name = steps
            .iter()
            .find(|(_, p, _)| !p.is_empty())
            .map(|(_, p, _)| p.clone())
            .unwrap_or_else(|| param.to_string());
        let ident = |name: &str| Expr {
            kind: ExprKind::Identifier(name.to_string()),
            span: sp,
        };
        // `Some(<idx>)` — the ctor callee is a bare `Identifier`, the form the
        // parser produces and codegen's enum-variant-call recognition expects.
        let some_of = |e: Expr| Expr {
            kind: ExprKind::Call {
                callee: Box::new(Expr {
                    kind: ExprKind::Identifier("Some".to_string()),
                    span: sp,
                }),
                args: vec![CallArg {
                    label: None,
                    mut_marker: false,
                    mut_marker_span: None,
                    value: e,
                    span: sp,
                }],
            },
            span: sp,
        };
        // Short-circuit sink: bind the pred param to the fully-adapted element
        // (elide a redundant self-bind), then `if pred { __pos = Some(__idx);
        // break }`, then `__idx += 1` (skipped on the break path).
        let sink = |current: Expr| -> Vec<Stmt> {
            let current_is_param = matches!(&current.kind, ExprKind::Identifier(n) if n == param);
            let guard = if current_is_param {
                pred.clone()
            } else {
                Expr {
                    kind: ExprKind::Block(Block {
                        stmts: vec![Stmt {
                            kind: StmtKind::Let {
                                is_mut: false,
                                pattern: Pattern {
                                    kind: PatternKind::Binding(param.to_string()),
                                    span: sp,
                                },
                                ty: None,
                                value: current,
                            },
                            span: sp,
                        }],
                        final_expr: Some(Box::new(pred.clone())),
                        span: sp,
                    }),
                    span: sp,
                }
            };
            let decide = vec![
                Stmt {
                    kind: StmtKind::Assign {
                        target: ident(&posname),
                        value: some_of(ident(&idxname)),
                    },
                    span: sp,
                },
                Stmt {
                    kind: StmtKind::Expr(Expr {
                        kind: ExprKind::Break {
                            label: None,
                            value: None,
                        },
                        span: sp,
                    }),
                    span: sp,
                },
            ];
            vec![
                Stmt {
                    kind: StmtKind::Expr(Expr {
                        kind: ExprKind::If {
                            condition: Box::new(guard),
                            then_block: Block {
                                stmts: decide,
                                final_expr: None,
                                span: sp,
                            },
                            else_branch: None,
                        },
                        span: sp,
                    }),
                    span: sp,
                },
                Stmt {
                    kind: StmtKind::Assign {
                        target: ident(&idxname),
                        value: Expr {
                            kind: ExprKind::Binary {
                                op: BinOp::Add,
                                left: Box::new(ident(&idxname)),
                                right: Box::new(Expr {
                                    kind: ExprKind::Integer(1, None),
                                    span: sp,
                                }),
                            },
                            span: sp,
                        },
                    },
                    span: sp,
                },
            ]
        };
        let sw_prefix = format!("__swp_{uid}_");
        let for_body =
            Self::build_fused_chain_body(&steps, 0, ident(&elem_name), &sink, &sw_prefix, &sp);
        let for_loop = Stmt {
            kind: StmtKind::Expr(Expr {
                kind: ExprKind::For {
                    label: None,
                    pattern: Pattern {
                        kind: PatternKind::Binding(elem_name),
                        span: sp,
                    },
                    iterable: Box::new(base.clone()),
                    attributes: Vec::new(),
                    body: Block {
                        stmts: for_body,
                        final_expr: None,
                        span: sp,
                    },
                },
                span: sp,
            }),
            span: sp,
        };
        let i64_te = TypeExpr {
            kind: TypeKind::Path(PathExpr {
                segments: vec!["i64".to_string()],
                generic_args: None,
                span: sp,
            }),
            span: sp,
        };
        let opt_i64_te = TypeExpr {
            kind: TypeKind::Path(PathExpr {
                segments: vec!["Option".to_string()],
                generic_args: Some(vec![GenericArg::Type(i64_te.clone())]),
                span: sp,
            }),
            span: sp,
        };
        let mut block_stmts = vec![
            Stmt {
                kind: StmtKind::Let {
                    is_mut: true,
                    pattern: Pattern {
                        kind: PatternKind::Binding(posname.clone()),
                        span: sp,
                    },
                    ty: Some(opt_i64_te),
                    value: ident("None"),
                },
                span: sp,
            },
            Stmt {
                kind: StmtKind::Let {
                    is_mut: true,
                    pattern: Pattern {
                        kind: PatternKind::Binding(idxname.clone()),
                        span: sp,
                    },
                    ty: Some(i64_te),
                    value: Expr {
                        kind: ExprKind::Integer(0, None),
                        span: sp,
                    },
                },
                span: sp,
            },
        ];
        block_stmts.extend(Self::fused_chain_prelude(&steps, &sw_prefix, &sp));
        block_stmts.push(for_loop);
        let block = Expr {
            kind: ExprKind::Block(Block {
                stmts: block_stmts,
                final_expr: Some(Box::new(ident(&posname))),
                span: sp,
            }),
            span: sp,
        };
        Ok(Some(self.compile_expr(&block)?))
    }

    /// `<iter-chain>.find(|x| pred) -> Option[T]` — the first YIELDED element the
    /// predicate holds for, or `None`. Desugars like `position` but stores the
    /// ELEMENT (not its index):
    ///
    /// ```text
    /// { let mut __find: Option[T] = None;
    ///   for <elem> in <base> { <steps>
    ///       let <param> = <adapted-elem>;
    ///       if <pred> { __find = Some(<param>); break; } }
    ///   __find }
    /// ```
    ///
    /// SCALAR payloads only (gated via `iter_terminal_elem_types` +
    /// `is_trivially_copyable_te`): a heap element `Some(elem)` would alias the
    /// borrowed source buffer and double-free (the reduce/max heap deferral).
    /// `Ok(None)` (loud `--interp` deferral) for a heap element or an
    /// unrecorded / unpeelable chain.
    pub(super) fn try_compile_iter_chain_find(
        &mut self,
        recv: &Expr,
        param: &str,
        pred: &Expr,
        call_span: &crate::token::Span,
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        let Some(elem_te) = self
            .span_tables
            .iter_terminal_elem_types
            .get(&(call_span.offset, call_span.length))
            .cloned()
        else {
            return Ok(None);
        };
        if !super::vec_method::is_trivially_copyable_te(&elem_te) {
            return Ok(None);
        }
        let (base, steps) = match Self::peel_fused_map_filter_chain(recv) {
            Some(x) => x,
            None if Self::for_loop_iterates_flat_map(recv) => (recv, Vec::new()),
            None => return Ok(None),
        };
        self.indexed_elem_counter += 1;
        let uid = self.indexed_elem_counter;
        let sp = *call_span;
        let findname = format!("__find_{}", uid);
        let elem_name = steps
            .iter()
            .find(|(_, p, _)| !p.is_empty())
            .map(|(_, p, _)| p.clone())
            .unwrap_or_else(|| param.to_string());
        let ident = |name: &str| Expr {
            kind: ExprKind::Identifier(name.to_string()),
            span: sp,
        };
        let some_of = |e: Expr| Expr {
            kind: ExprKind::Call {
                callee: Box::new(Expr {
                    kind: ExprKind::Identifier("Some".to_string()),
                    span: sp,
                }),
                args: vec![CallArg {
                    label: None,
                    mut_marker: false,
                    mut_marker_span: None,
                    value: e,
                    span: sp,
                }],
            },
            span: sp,
        };
        // Sink: bind `param` to the fully-adapted element (so both the pred and
        // the `Some(param)` payload reference it), then `if pred { __find =
        // Some(param); break }`.
        let sink = |current: Expr| -> Vec<Stmt> {
            let current_is_param = matches!(&current.kind, ExprKind::Identifier(n) if n == param);
            let mut out = Vec::new();
            if !current_is_param {
                out.push(Stmt {
                    kind: StmtKind::Let {
                        is_mut: false,
                        pattern: Pattern {
                            kind: PatternKind::Binding(param.to_string()),
                            span: sp,
                        },
                        ty: None,
                        value: current,
                    },
                    span: sp,
                });
            }
            let decide = vec![
                Stmt {
                    kind: StmtKind::Assign {
                        target: ident(&findname),
                        value: some_of(ident(param)),
                    },
                    span: sp,
                },
                Stmt {
                    kind: StmtKind::Expr(Expr {
                        kind: ExprKind::Break {
                            label: None,
                            value: None,
                        },
                        span: sp,
                    }),
                    span: sp,
                },
            ];
            out.push(Stmt {
                kind: StmtKind::Expr(Expr {
                    kind: ExprKind::If {
                        condition: Box::new(pred.clone()),
                        then_block: Block {
                            stmts: decide,
                            final_expr: None,
                            span: sp,
                        },
                        else_branch: None,
                    },
                    span: sp,
                }),
                span: sp,
            });
            out
        };
        let sw_prefix = format!("__swf_{uid}_");
        let for_body =
            Self::build_fused_chain_body(&steps, 0, ident(&elem_name), &sink, &sw_prefix, &sp);
        let for_loop = Stmt {
            kind: StmtKind::Expr(Expr {
                kind: ExprKind::For {
                    label: None,
                    pattern: Pattern {
                        kind: PatternKind::Binding(elem_name),
                        span: sp,
                    },
                    iterable: Box::new(base.clone()),
                    attributes: Vec::new(),
                    body: Block {
                        stmts: for_body,
                        final_expr: None,
                        span: sp,
                    },
                },
                span: sp,
            }),
            span: sp,
        };
        let opt_te = TypeExpr {
            kind: TypeKind::Path(PathExpr {
                segments: vec!["Option".to_string()],
                generic_args: Some(vec![GenericArg::Type(elem_te)]),
                span: sp,
            }),
            span: sp,
        };
        let mut block_stmts = vec![Stmt {
            kind: StmtKind::Let {
                is_mut: true,
                pattern: Pattern {
                    kind: PatternKind::Binding(findname.clone()),
                    span: sp,
                },
                ty: Some(opt_te),
                value: ident("None"),
            },
            span: sp,
        }];
        block_stmts.extend(Self::fused_chain_prelude(&steps, &sw_prefix, &sp));
        block_stmts.push(for_loop);
        let block = Expr {
            kind: ExprKind::Block(Block {
                stmts: block_stmts,
                final_expr: Some(Box::new(ident(&findname))),
                span: sp,
            }),
            span: sp,
        };
        Ok(Some(self.compile_expr(&block)?))
    }

    /// `<iter-chain>.find_map(|x| <Option-expr>) -> Option[U]` — the first
    /// `Some(u)` the closure produces over the fused chain, or `None` (map+find
    /// fusion). Desugars like `find`, but the sink is a synthesized
    /// `match <closure-body> { Some(<v>) => { __fm = Some(<v>); break }, None =>
    /// {} }` (reusing the proven Option-match codegen, exactly like the
    /// `filter_map` FOR-LOOP step) instead of an `if <pred>`. The synthesized
    /// `Some(<v>)` payload binding is stamped with `filter_map_bind_span` (the
    /// closure body's span) whose surface payload type the typechecker's
    /// `find_map` arm pre-registered in `pattern_binding_types` at the SAME span,
    /// so `reconstruct_payload_value` sizes a narrow/float `U` correctly.
    /// Trivially-copyable payload `U` only (`iter_terminal_elem_types` holds U);
    /// a heap `U` returns `Ok(None)` -> the caller's loud `--interp` bail.
    pub(super) fn try_compile_iter_chain_find_map(
        &mut self,
        recv: &Expr,
        param: &str,
        body: &Expr,
        call_span: &crate::token::Span,
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        // `iter_terminal_elem_types` holds the *payload* type `U` for find_map
        // (the typechecker registers `new_item` there, not the source element).
        let Some(u_te) = self
            .span_tables
            .iter_terminal_elem_types
            .get(&(call_span.offset, call_span.length))
            .cloned()
        else {
            return Ok(None);
        };
        if !super::vec_method::is_trivially_copyable_te(&u_te) {
            return Ok(None);
        }
        let (base, steps) = match Self::peel_fused_map_filter_chain(recv) {
            Some(x) => x,
            None if Self::for_loop_iterates_flat_map(recv) => (recv, Vec::new()),
            None => return Ok(None),
        };
        self.indexed_elem_counter += 1;
        let uid = self.indexed_elem_counter;
        let sp = *call_span;
        let findname = format!("__findm_{}", uid);
        let elem_name = steps
            .iter()
            .find(|(_, p, _)| !p.is_empty())
            .map(|(_, p, _)| p.clone())
            .unwrap_or_else(|| param.to_string());
        let ident = |name: &str| Expr {
            kind: ExprKind::Identifier(name.to_string()),
            span: sp,
        };
        let some_of = |e: Expr| Expr {
            kind: ExprKind::Call {
                callee: Box::new(Expr {
                    kind: ExprKind::Identifier("Some".to_string()),
                    span: sp,
                }),
                args: vec![CallArg {
                    label: None,
                    mut_marker: false,
                    mut_marker_span: None,
                    value: e,
                    span: sp,
                }],
            },
            span: sp,
        };
        // Sink: bind `param` to the fully-adapted element (so the closure body
        // references it), then `match <body> { Some(<fresh>) => { __fm =
        // Some(<fresh>); break }, None => {} }`.
        let fresh = format!("__fmv_{uid}");
        let bind_span = Self::filter_map_bind_span(&body.span);
        let sink = |current: Expr| -> Vec<Stmt> {
            let current_is_param = matches!(&current.kind, ExprKind::Identifier(n) if n == param);
            let mut out = Vec::new();
            if !current_is_param {
                out.push(Stmt {
                    kind: StmtKind::Let {
                        is_mut: false,
                        pattern: Pattern {
                            kind: PatternKind::Binding(param.to_string()),
                            span: sp,
                        },
                        ty: None,
                        value: current,
                    },
                    span: sp,
                });
            }
            let some_arm_body = Expr {
                kind: ExprKind::Block(Block {
                    stmts: vec![
                        Stmt {
                            kind: StmtKind::Assign {
                                target: ident(&findname),
                                value: some_of(ident(&fresh)),
                            },
                            span: sp,
                        },
                        Stmt {
                            kind: StmtKind::Expr(Expr {
                                kind: ExprKind::Break {
                                    label: None,
                                    value: None,
                                },
                                span: sp,
                            }),
                            span: sp,
                        },
                    ],
                    final_expr: None,
                    span: sp,
                }),
                span: sp,
            };
            let none_arm_body = Expr {
                kind: ExprKind::Block(Block {
                    stmts: Vec::new(),
                    final_expr: None,
                    span: sp,
                }),
                span: sp,
            };
            out.push(Stmt {
                kind: StmtKind::Expr(Expr {
                    kind: ExprKind::Match {
                        scrutinee: Box::new(body.clone()),
                        arms: vec![
                            MatchArm {
                                pattern: Pattern {
                                    kind: PatternKind::TupleVariant {
                                        path: vec!["Some".to_string()],
                                        patterns: vec![Pattern {
                                            kind: PatternKind::Binding(fresh.clone()),
                                            span: bind_span,
                                        }],
                                    },
                                    span: sp,
                                },
                                guard: None,
                                body: some_arm_body,
                                span: sp,
                            },
                            MatchArm {
                                pattern: Pattern {
                                    kind: PatternKind::Binding("None".to_string()),
                                    span: sp,
                                },
                                guard: None,
                                body: none_arm_body,
                                span: sp,
                            },
                        ],
                    },
                    span: sp,
                }),
                span: sp,
            });
            out
        };
        let sw_prefix = format!("__swfm_{uid}_");
        let for_body =
            Self::build_fused_chain_body(&steps, 0, ident(&elem_name), &sink, &sw_prefix, &sp);
        let for_loop = Stmt {
            kind: StmtKind::Expr(Expr {
                kind: ExprKind::For {
                    label: None,
                    pattern: Pattern {
                        kind: PatternKind::Binding(elem_name),
                        span: sp,
                    },
                    iterable: Box::new(base.clone()),
                    attributes: Vec::new(),
                    body: Block {
                        stmts: for_body,
                        final_expr: None,
                        span: sp,
                    },
                },
                span: sp,
            }),
            span: sp,
        };
        let opt_te = TypeExpr {
            kind: TypeKind::Path(PathExpr {
                segments: vec!["Option".to_string()],
                generic_args: Some(vec![GenericArg::Type(u_te)]),
                span: sp,
            }),
            span: sp,
        };
        let mut block_stmts = vec![Stmt {
            kind: StmtKind::Let {
                is_mut: true,
                pattern: Pattern {
                    kind: PatternKind::Binding(findname.clone()),
                    span: sp,
                },
                ty: Some(opt_te),
                value: ident("None"),
            },
            span: sp,
        }];
        block_stmts.extend(Self::fused_chain_prelude(&steps, &sw_prefix, &sp));
        block_stmts.push(for_loop);
        let block = Expr {
            kind: ExprKind::Block(Block {
                stmts: block_stmts,
                final_expr: Some(Box::new(ident(&findname))),
                span: sp,
            }),
            span: sp,
        };
        Ok(Some(self.compile_expr(&block)?))
    }

    /// `<iter-chain>.partition(|x| pred) -> (Vec[T], Vec[T])` — eager terminal
    /// splitting the fused-chain elements into (matches, non-matches). Desugars
    /// to `{ let mut __pt: Vec[T] = Vec.new(); let mut __pf: Vec[T] = Vec.new();
    /// <prelude>; for <e> in <base> { <chain>; if <pred> { __pt.push(x) } else {
    /// __pf.push(x) } } (__pt, __pf) }` — reusing the fused for-loop lowering and
    /// the (verified) block-returns-a-tuple-of-owned-Vecs path. Trivially-
    /// copyable element `T` only (`iter_terminal_elem_types` holds T); a heap `T`
    /// returns `Ok(None)` -> the caller's loud `--interp` bail (each element
    /// would need a clone into one target Vec).
    pub(super) fn try_compile_iter_chain_partition(
        &mut self,
        recv: &Expr,
        param: &str,
        pred: &Expr,
        call_span: &crate::token::Span,
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        let Some(elem_te) = self
            .span_tables
            .iter_terminal_elem_types
            .get(&(call_span.offset, call_span.length))
            .cloned()
        else {
            return Ok(None);
        };
        // A heap element `T` (String / Vec) is supported by CLONING the pushed
        // element (`param.clone()`) so the owning target Vec doesn't alias the
        // borrowed source element (a shallow push would double-free at scope
        // exit). A trivially-copyable `T` pushes the bare `param` (the proven
        // scalar path — no clone needed). Unlike find_map's owned-payload case,
        // partition's element comes borrowed from the source, so the clone is
        // what makes it sound.
        let elem_is_heap = !super::vec_method::is_trivially_copyable_te(&elem_te);
        let (base, steps) = match Self::peel_fused_map_filter_chain(recv) {
            Some(x) => x,
            None if Self::for_loop_iterates_flat_map(recv) => (recv, Vec::new()),
            None => return Ok(None),
        };
        self.indexed_elem_counter += 1;
        let uid = self.indexed_elem_counter;
        let sp = *call_span;
        let pt = format!("__pt_{}", uid);
        let pf = format!("__pf_{}", uid);
        let elem_name = steps
            .iter()
            .find(|(_, p, _)| !p.is_empty())
            .map(|(_, p, _)| p.clone())
            .unwrap_or_else(|| param.to_string());
        let ident = |name: &str| Expr {
            kind: ExprKind::Identifier(name.to_string()),
            span: sp,
        };
        let push_to = |vec_name: &str, val: Expr| -> Stmt {
            Stmt {
                kind: StmtKind::Expr(Expr {
                    kind: ExprKind::MethodCall {
                        object: Box::new(ident(vec_name)),
                        method: "push".to_string(),
                        turbofish: None,
                        args: vec![CallArg {
                            label: None,
                            mut_marker: false,
                            mut_marker_span: None,
                            value: val,
                            span: sp,
                        }],
                        args_close_span: sp,
                    },
                    span: sp,
                }),
                span: sp,
            }
        };
        // The value pushed into whichever partition Vec: `param` for a scalar
        // element (copy), `param.clone()` for a heap element (deep copy so the
        // target Vec owns an independent buffer; the borrowed source keeps its own
        // — no double-free).
        let pushed = |param: &str| -> Expr {
            if elem_is_heap {
                Expr {
                    kind: ExprKind::MethodCall {
                        object: Box::new(ident(param)),
                        method: "clone".to_string(),
                        turbofish: None,
                        args: vec![],
                        args_close_span: sp,
                    },
                    span: sp,
                }
            } else {
                ident(param)
            }
        };
        // Sink: bind `param` to the fully-adapted element (so both the pred and
        // the pushed value reference it), then `if pred { __pt.push(param[.clone()])
        // } else { __pf.push(param[.clone()]) }`.
        let sink = |current: Expr| -> Vec<Stmt> {
            let current_is_param = matches!(&current.kind, ExprKind::Identifier(n) if n == param);
            let mut out = Vec::new();
            if !current_is_param {
                out.push(Stmt {
                    kind: StmtKind::Let {
                        is_mut: false,
                        pattern: Pattern {
                            kind: PatternKind::Binding(param.to_string()),
                            span: sp,
                        },
                        ty: None,
                        value: current,
                    },
                    span: sp,
                });
            }
            out.push(Stmt {
                kind: StmtKind::Expr(Expr {
                    kind: ExprKind::If {
                        condition: Box::new(pred.clone()),
                        then_block: Block {
                            stmts: vec![push_to(&pt, pushed(param))],
                            final_expr: None,
                            span: sp,
                        },
                        else_branch: Some(Box::new(Expr {
                            kind: ExprKind::Block(Block {
                                stmts: vec![push_to(&pf, pushed(param))],
                                final_expr: None,
                                span: sp,
                            }),
                            span: sp,
                        })),
                    },
                    span: sp,
                }),
                span: sp,
            });
            out
        };
        let sw_prefix = format!("__swpt_{uid}_");
        let for_body =
            Self::build_fused_chain_body(&steps, 0, ident(&elem_name), &sink, &sw_prefix, &sp);
        let for_loop = Stmt {
            kind: StmtKind::Expr(Expr {
                kind: ExprKind::For {
                    label: None,
                    pattern: Pattern {
                        kind: PatternKind::Binding(elem_name),
                        span: sp,
                    },
                    iterable: Box::new(base.clone()),
                    attributes: Vec::new(),
                    body: Block {
                        stmts: for_body,
                        final_expr: None,
                        span: sp,
                    },
                },
                span: sp,
            }),
            span: sp,
        };
        let vec_te = |elem: TypeExpr| TypeExpr {
            kind: TypeKind::Path(PathExpr {
                segments: vec!["Vec".to_string()],
                generic_args: Some(vec![GenericArg::Type(elem)]),
                span: sp,
            }),
            span: sp,
        };
        let new_vec = || Expr {
            kind: ExprKind::Call {
                callee: Box::new(Expr {
                    kind: ExprKind::Path {
                        segments: vec!["Vec".to_string(), "new".to_string()],
                        generic_args: None,
                    },
                    span: sp,
                }),
                args: vec![],
            },
            span: sp,
        };
        let let_vec = |name: &str, elem: TypeExpr| Stmt {
            kind: StmtKind::Let {
                is_mut: true,
                pattern: Pattern {
                    kind: PatternKind::Binding(name.to_string()),
                    span: sp,
                },
                ty: Some(vec_te(elem)),
                value: new_vec(),
            },
            span: sp,
        };
        let mut block_stmts = vec![let_vec(&pt, elem_te.clone()), let_vec(&pf, elem_te.clone())];
        block_stmts.extend(Self::fused_chain_prelude(&steps, &sw_prefix, &sp));
        block_stmts.push(for_loop);
        let tuple = Expr {
            kind: ExprKind::Tuple(vec![ident(&pt), ident(&pf)]),
            span: sp,
        };
        let block = Expr {
            kind: ExprKind::Block(Block {
                stmts: block_stmts,
                final_expr: Some(Box::new(tuple)),
                span: sp,
            }),
            span: sp,
        };
        Ok(Some(self.compile_expr(&block)?))
    }

    /// `<iter-chain>.last() -> Option[T]` (`nth_arg` = None) and
    /// `<iter-chain>.nth(n) -> Option[T]` (`nth_arg` = Some(n)). Desugar like
    /// `find` but store the element unconditionally (last, no break) or at the
    /// n-th yield (nth, break). SCALAR payloads only (heap defers, like `find`).
    pub(super) fn try_compile_iter_chain_last_nth(
        &mut self,
        recv: &Expr,
        nth_arg: Option<&Expr>,
        call_span: &crate::token::Span,
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        let Some(elem_te) = self
            .span_tables
            .iter_terminal_elem_types
            .get(&(call_span.offset, call_span.length))
            .cloned()
        else {
            return Ok(None);
        };
        if !super::vec_method::is_trivially_copyable_te(&elem_te) {
            return Ok(None);
        }
        let (base, steps) = match Self::peel_fused_map_filter_chain(recv) {
            Some(x) => x,
            None if Self::for_loop_iterates_flat_map(recv) => (recv, Vec::new()),
            None => return Ok(None),
        };
        self.indexed_elem_counter += 1;
        let uid = self.indexed_elem_counter;
        let sp = *call_span;
        let resname = format!("__ln_{}", uid);
        let idxname = format!("__lni_{}", uid);
        let nbname = format!("__lnb_{}", uid);
        let is_nth = nth_arg.is_some();
        let elem_name = steps
            .iter()
            .find(|(_, p, _)| !p.is_empty())
            .map(|(_, p, _)| p.clone())
            .unwrap_or_else(|| format!("__lne_{}", uid));
        let ident = |name: &str| Expr {
            kind: ExprKind::Identifier(name.to_string()),
            span: sp,
        };
        let some_of = |e: Expr| Expr {
            kind: ExprKind::Call {
                callee: Box::new(Expr {
                    kind: ExprKind::Identifier("Some".to_string()),
                    span: sp,
                }),
                args: vec![CallArg {
                    label: None,
                    mut_marker: false,
                    mut_marker_span: None,
                    value: e,
                    span: sp,
                }],
            },
            span: sp,
        };
        let sink = |current: Expr| -> Vec<Stmt> {
            let assign = Stmt {
                kind: StmtKind::Assign {
                    target: ident(&resname),
                    value: some_of(current),
                },
                span: sp,
            };
            if !is_nth {
                return vec![assign];
            }
            let decide = vec![
                assign,
                Stmt {
                    kind: StmtKind::Expr(Expr {
                        kind: ExprKind::Break {
                            label: None,
                            value: None,
                        },
                        span: sp,
                    }),
                    span: sp,
                },
            ];
            vec![
                Stmt {
                    kind: StmtKind::Expr(Expr {
                        kind: ExprKind::If {
                            condition: Box::new(Expr {
                                kind: ExprKind::Binary {
                                    op: BinOp::Eq,
                                    left: Box::new(ident(&idxname)),
                                    right: Box::new(ident(&nbname)),
                                },
                                span: sp,
                            }),
                            then_block: Block {
                                stmts: decide,
                                final_expr: None,
                                span: sp,
                            },
                            else_branch: None,
                        },
                        span: sp,
                    }),
                    span: sp,
                },
                Stmt {
                    kind: StmtKind::Assign {
                        target: ident(&idxname),
                        value: Expr {
                            kind: ExprKind::Binary {
                                op: BinOp::Add,
                                left: Box::new(ident(&idxname)),
                                right: Box::new(Expr {
                                    kind: ExprKind::Integer(1, None),
                                    span: sp,
                                }),
                            },
                            span: sp,
                        },
                    },
                    span: sp,
                },
            ]
        };
        let sw_prefix = format!("__swln_{uid}_");
        let for_body =
            Self::build_fused_chain_body(&steps, 0, ident(&elem_name), &sink, &sw_prefix, &sp);
        let for_loop = Stmt {
            kind: StmtKind::Expr(Expr {
                kind: ExprKind::For {
                    label: None,
                    pattern: Pattern {
                        kind: PatternKind::Binding(elem_name),
                        span: sp,
                    },
                    iterable: Box::new(base.clone()),
                    attributes: Vec::new(),
                    body: Block {
                        stmts: for_body,
                        final_expr: None,
                        span: sp,
                    },
                },
                span: sp,
            }),
            span: sp,
        };
        let i64_te = TypeExpr {
            kind: TypeKind::Path(PathExpr {
                segments: vec!["i64".to_string()],
                generic_args: None,
                span: sp,
            }),
            span: sp,
        };
        let opt_te = TypeExpr {
            kind: TypeKind::Path(PathExpr {
                segments: vec!["Option".to_string()],
                generic_args: Some(vec![GenericArg::Type(elem_te)]),
                span: sp,
            }),
            span: sp,
        };
        let mut block_stmts = vec![Stmt {
            kind: StmtKind::Let {
                is_mut: true,
                pattern: Pattern {
                    kind: PatternKind::Binding(resname.clone()),
                    span: sp,
                },
                ty: Some(opt_te),
                value: ident("None"),
            },
            span: sp,
        }];
        if is_nth {
            block_stmts.push(Stmt {
                kind: StmtKind::Let {
                    is_mut: false,
                    pattern: Pattern {
                        kind: PatternKind::Binding(nbname.clone()),
                        span: sp,
                    },
                    ty: Some(i64_te.clone()),
                    value: nth_arg.unwrap().clone(),
                },
                span: sp,
            });
            block_stmts.push(Stmt {
                kind: StmtKind::Let {
                    is_mut: true,
                    pattern: Pattern {
                        kind: PatternKind::Binding(idxname.clone()),
                        span: sp,
                    },
                    ty: Some(i64_te),
                    value: Expr {
                        kind: ExprKind::Integer(0, None),
                        span: sp,
                    },
                },
                span: sp,
            });
        }
        block_stmts.extend(Self::fused_chain_prelude(&steps, &sw_prefix, &sp));
        block_stmts.push(for_loop);
        let block = Expr {
            kind: ExprKind::Block(Block {
                stmts: block_stmts,
                final_expr: Some(Box::new(ident(&resname))),
                span: sp,
            }),
            span: sp,
        };
        Ok(Some(self.compile_expr(&block)?))
    }

    /// Lower `for <pat> in <src>.iter().{map|filter}+ { <body> }` — a `for` loop
    /// whose iterable is a fused iterator chain (B-2026-07-11-18).
    ///
    /// `compile_for` has explicit peel arms for identity sources (`.iter()` /
    /// `.chars()` / ranges / `.enumerate()`), but a `.map(..)`/`.filter(..)`
    /// adaptor iterable had NO arm and fell through to the dispatcher's silent
    /// `_ =>` — the loop body ran ZERO times, a SILENT wrong-answer miscompile
    /// (the interpreter iterated correctly). This routes such an iterable through
    /// the same map/filter fusion as the `fold` terminal, with the USER'S loop
    /// body as the sink: it peels the adaptors down to a base source the `for`
    /// loop iterates correctly and emits
    ///
    /// ```text
    /// for <elem> in <base> {
    ///     <filter as `if <pred> { … }`, map as `let <p> = <body>`>
    ///     let <pat> = <adapted-element>;  <user body>
    /// }
    /// ```
    ///
    /// Returns `Ok(None)` (so `compile_for`'s existing arms / fallthrough handle
    /// it) when the iterable is NOT a `map`/`filter` chain — an empty adaptor
    /// list (a bare `.iter()` etc. an existing arm already handles), or a shape
    /// `peel_fused_map_filter_chain` rejects.
    pub(super) fn try_compile_for_iter_chain(
        &mut self,
        label: Option<&str>,
        pattern: &Pattern,
        iterable: &Expr,
        body: &Block,
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        let Some((base, steps)) = Self::peel_fused_map_filter_chain(iterable) else {
            return Ok(None);
        };
        // No adaptors → a bare source `compile_for` already iterates correctly;
        // let its existing arms handle it (this desugar is only for map/filter).
        if steps.is_empty() {
            return Ok(None);
        }

        self.indexed_elem_counter += 1;
        let uid = self.indexed_elem_counter;
        let sp = iterable.span;
        // The loop var is the first PARAM-BEARING adaptor's param (count
        // adaptors have none), keeping the source element typed by the
        // `for`-loop binding (as the collect/fold desugars do). A pure
        // count-adaptor chain (`xs.iter().skip(1).take(2)`) has no closure
        // param; use the user's own binding name if the pattern is a plain
        // binding (the sink then elides the redundant re-bind), else a
        // synthetic name.
        let elem_name = steps
            .iter()
            .find(|(_, p, _)| !p.is_empty())
            .map(|(_, p, _)| p.clone())
            .or_else(|| match &pattern.kind {
                PatternKind::Binding(n) => Some(n.clone()),
                _ => None,
            })
            .unwrap_or_else(|| format!("__fle_{uid}"));
        let ident = |name: &str| Expr {
            kind: ExprKind::Identifier(name.to_string()),
            span: sp,
        };

        // Sink: bind the USER pattern to the fully-adapted element (eliding a
        // redundant self-bind when the pattern is a plain identifier equal to the
        // current element), then inline the user's loop body.
        let sink = |current: Expr| -> Vec<Stmt> {
            let mut out = Vec::new();
            let elide = matches!(
                (&pattern.kind, &current.kind),
                (PatternKind::Binding(pn), ExprKind::Identifier(cn)) if pn == cn
            );
            if !elide {
                out.push(Stmt {
                    kind: StmtKind::Let {
                        is_mut: false,
                        pattern: pattern.clone(),
                        ty: None,
                        value: current,
                    },
                    span: sp,
                });
            }
            out.extend(body.stmts.iter().cloned());
            if let Some(fe) = &body.final_expr {
                out.push(Stmt {
                    kind: StmtKind::Expr((**fe).clone()),
                    span: sp,
                });
            }
            out
        };
        let sw_prefix = format!("__swl_{uid}_");
        let for_body =
            Self::build_fused_chain_body(&steps, 0, ident(&elem_name), &sink, &sw_prefix, &sp);
        let for_loop = Expr {
            kind: ExprKind::For {
                label: label.map(|s| s.to_string()),
                pattern: Pattern {
                    kind: PatternKind::Binding(elem_name),
                    span: sp,
                },
                iterable: Box::new(base.clone()),
                attributes: Vec::new(),
                body: Block {
                    stmts: for_body,
                    final_expr: None,
                    span: sp,
                },
            },
            span: sp,
        };
        // A `skip_while` step needs its latch flag(s) declared before the loop
        // — wrap in a block; the user's `label` stays on the inner `for`, so
        // labeled `break`/`continue` in the body are unaffected. Chains
        // without `skip_while` compile the bare `for` exactly as before.
        let prelude = Self::fused_chain_prelude(&steps, &sw_prefix, &sp);
        if prelude.is_empty() {
            return Ok(Some(self.compile_expr(&for_loop)?));
        }
        let mut block_stmts = prelude;
        block_stmts.push(Stmt {
            kind: StmtKind::Expr(for_loop),
            span: sp,
        });
        let block = Expr {
            kind: ExprKind::Block(Block {
                stmts: block_stmts,
                final_expr: None,
                span: sp,
            }),
            span: sp,
        };
        Ok(Some(self.compile_expr(&block)?))
    }

    /// Lower `A.chain(B).collect()` for two plain identity SOURCES (`.iter()` /
    /// bounded range) into a single Vec by emitting the identity-collect loop
    /// once per source into a shared accumulator (B-2026-07-04-2 sub-part 1).
    /// Returns `Ok(None)` if the result type isn't a recorded `Vec[T]` (the
    /// caller then falls through to the loud dispatch-fail). The emitted block
    /// is:
    ///
    /// ```text
    /// { let mut __chv: Vec[T] = Vec.new();
    ///   for __ch0 in <A> { __chv.push(__ch0); }
    ///   for __ch1 in <B> { __chv.push(__ch1); }
    ///   __chv }
    /// ```
    ///
    /// Each `for x in <src>` over a borrowed source clones the element on
    /// `push` (the exact single-source identity-collect semantics), so both
    /// sources survive and the accumulator owns independent copies.
    pub(super) fn try_compile_chain_identity_collect(
        &mut self,
        src_a: &Expr,
        src_b: &Expr,
        call_span: &crate::token::Span,
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        let vec_te = match self
            .drop_rc
            .owned_temp_drops
            .get(&(call_span.offset, call_span.length))
        {
            Some(te) => te.clone(),
            None => return Ok(None),
        };
        if !matches!(
            &vec_te.kind,
            TypeKind::Path(p) if p.segments.last().map(|s| s.as_str()) == Some("Vec")
        ) {
            return Ok(None);
        }

        let uid = self.indexed_elem_counter;
        self.indexed_elem_counter += 1;
        let sp = *call_span;
        let vec_name = format!("__chv_{}", uid);
        let ident = |name: &str| Expr {
            kind: ExprKind::Identifier(name.to_string()),
            span: sp,
        };

        // `let mut __chv: Vec[T] = Vec.new();`
        let vec_new = Expr {
            kind: ExprKind::Call {
                callee: Box::new(Expr {
                    kind: ExprKind::Path {
                        segments: vec!["Vec".to_string(), "new".to_string()],
                        generic_args: None,
                    },
                    span: sp,
                }),
                args: vec![],
            },
            span: sp,
        };
        let let_vec = Stmt {
            kind: StmtKind::Let {
                is_mut: true,
                pattern: Pattern {
                    kind: PatternKind::Binding(vec_name.clone()),
                    span: sp,
                },
                ty: Some(vec_te),
                value: vec_new,
            },
            span: sp,
        };

        // `for <loop_var> in <src> { __chv.push(<loop_var>); }`
        let for_loop = |loop_var: &str, src: &Expr| Stmt {
            kind: StmtKind::Expr(Expr {
                kind: ExprKind::For {
                    label: None,
                    pattern: Pattern {
                        kind: PatternKind::Binding(loop_var.to_string()),
                        span: sp,
                    },
                    iterable: Box::new(src.clone()),
                    attributes: Vec::new(),
                    body: Block {
                        stmts: vec![Stmt {
                            kind: StmtKind::Expr(Expr {
                                kind: ExprKind::MethodCall {
                                    object: Box::new(ident(&vec_name)),
                                    method: "push".to_string(),
                                    turbofish: None,
                                    args: vec![CallArg {
                                        label: None,
                                        mut_marker: false,
                                        mut_marker_span: None,
                                        value: ident(loop_var),
                                        span: sp,
                                    }],
                                    args_close_span: sp,
                                },
                                span: sp,
                            }),
                            span: sp,
                        }],
                        final_expr: None,
                        span: sp,
                    },
                },
                span: sp,
            }),
            span: sp,
        };

        let loop_a = for_loop(&format!("__ch0_{}", uid), src_a);
        let loop_b = for_loop(&format!("__ch1_{}", uid), src_b);

        let block = Expr {
            kind: ExprKind::Block(Block {
                stmts: vec![let_vec, loop_a, loop_b],
                final_expr: Some(Box::new(ident(&vec_name))),
                span: sp,
            }),
            span: sp,
        };
        Ok(Some(self.compile_expr(&block)?))
    }

    /// Lower `<outer>.flat_map(|param| <inner>).collect()` into a flat Vec via
    /// nested loops (B-2026-07-04-2 sub-part 1). Returns `Ok(None)` if the
    /// result type isn't a recorded `Vec[T]`. The emitted block is:
    ///
    /// ```text
    /// { let mut __fmv: Vec[T] = Vec.new();
    ///   for <param> in <outer> {
    ///     for __fm in <inner> { __fmv.push(__fm); }
    ///   }
    ///   __fmv }
    /// ```
    ///
    /// The closure param IS the outer loop var, so the inner iterable `<inner>`
    /// (the closure body, which references `param`) resolves. Iteration-based —
    /// each `push` clones, so the source survives and the accumulator owns
    /// independent copies (heap-safe, like the other identity collects).
    pub(super) fn try_compile_flat_map_collect(
        &mut self,
        outer: &Expr,
        param: &str,
        inner: &Expr,
        call_span: &crate::token::Span,
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        let vec_te = match self
            .drop_rc
            .owned_temp_drops
            .get(&(call_span.offset, call_span.length))
        {
            Some(te) => te.clone(),
            None => return Ok(None),
        };
        if !matches!(
            &vec_te.kind,
            TypeKind::Path(p) if p.segments.last().map(|s| s.as_str()) == Some("Vec")
        ) {
            return Ok(None);
        }

        let uid = self.indexed_elem_counter;
        self.indexed_elem_counter += 1;
        let sp = *call_span;
        let vec_name = format!("__fmv_{}", uid);
        let inner_var = format!("__fm_{}", uid);
        let ident = |name: &str| Expr {
            kind: ExprKind::Identifier(name.to_string()),
            span: sp,
        };
        let for_loop = |var: &str, iterable: Expr, body: Vec<Stmt>| Stmt {
            kind: StmtKind::Expr(Expr {
                kind: ExprKind::For {
                    label: None,
                    pattern: Pattern {
                        kind: PatternKind::Binding(var.to_string()),
                        span: sp,
                    },
                    iterable: Box::new(iterable),
                    attributes: Vec::new(),
                    body: Block {
                        stmts: body,
                        final_expr: None,
                        span: sp,
                    },
                },
                span: sp,
            }),
            span: sp,
        };

        let vec_new = Expr {
            kind: ExprKind::Call {
                callee: Box::new(Expr {
                    kind: ExprKind::Path {
                        segments: vec!["Vec".to_string(), "new".to_string()],
                        generic_args: None,
                    },
                    span: sp,
                }),
                args: vec![],
            },
            span: sp,
        };
        let let_vec = Stmt {
            kind: StmtKind::Let {
                is_mut: true,
                pattern: Pattern {
                    kind: PatternKind::Binding(vec_name.clone()),
                    span: sp,
                },
                ty: Some(vec_te),
                value: vec_new,
            },
            span: sp,
        };

        // Inner: `for __fm in <inner> { __fmv.push(__fm); }`
        let push_inner = Stmt {
            kind: StmtKind::Expr(Expr {
                kind: ExprKind::MethodCall {
                    object: Box::new(ident(&vec_name)),
                    method: "push".to_string(),
                    turbofish: None,
                    args: vec![CallArg {
                        label: None,
                        mut_marker: false,
                        mut_marker_span: None,
                        value: ident(&inner_var),
                        span: sp,
                    }],
                    args_close_span: sp,
                },
                span: sp,
            }),
            span: sp,
        };
        let inner_loop = for_loop(&inner_var, inner.clone(), vec![push_inner]);
        // Outer: `for <param> in <outer> { <inner_loop> }`
        let outer_loop = for_loop(param, outer.clone(), vec![inner_loop]);

        let block = Expr {
            kind: ExprKind::Block(Block {
                stmts: vec![let_vec, outer_loop],
                final_expr: Some(Box::new(ident(&vec_name))),
                span: sp,
            }),
            span: sp,
        };
        Ok(Some(self.compile_expr(&block)?))
    }

    /// Lower `<recv>.flatten().collect()` (B-2026-07-19-12 slice 3) into a single
    /// accumulating loop over the flatten chain:
    ///
    /// ```text
    /// { let mut __flc: Vec[E] = Vec.new();
    ///   for __fli in <recv>.flatten() { __flc.push(__fli); }
    ///   __flc }
    /// ```
    ///
    /// The `for __fli in <recv>.flatten()` loop routes through the flatten
    /// nested-loop desugar (`try_compile_for_flatten`, slice 2), so this reuses
    /// all of that shape's coverage. `flatten_recv` is the `flatten()` MethodCall
    /// (the collect receiver); the result element type comes from the
    /// typechecker-recorded `owned_temp_drops` entry at the collect span. Returns
    /// `Ok(None)` (→ the general collect engine / loud bail) when that type isn't
    /// a recorded `Vec[E]`.
    pub(super) fn try_compile_flatten_collect(
        &mut self,
        flatten_recv: &Expr,
        call_span: &crate::token::Span,
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        let vec_te = match self
            .drop_rc
            .owned_temp_drops
            .get(&(call_span.offset, call_span.length))
        {
            Some(te) => te.clone(),
            None => return Ok(None),
        };
        if !matches!(
            &vec_te.kind,
            TypeKind::Path(p) if p.segments.last().map(|s| s.as_str()) == Some("Vec")
        ) {
            return Ok(None);
        }

        let uid = self.indexed_elem_counter;
        self.indexed_elem_counter += 1;
        let sp = *call_span;
        let vec_name = format!("__flc_{}", uid);
        let elem_var = format!("__fli_{}", uid);
        let ident = |name: &str| Expr {
            kind: ExprKind::Identifier(name.to_string()),
            span: sp,
        };

        let vec_new = Expr {
            kind: ExprKind::Call {
                callee: Box::new(Expr {
                    kind: ExprKind::Path {
                        segments: vec!["Vec".to_string(), "new".to_string()],
                        generic_args: None,
                    },
                    span: sp,
                }),
                args: vec![],
            },
            span: sp,
        };
        let let_vec = Stmt {
            kind: StmtKind::Let {
                is_mut: true,
                pattern: Pattern {
                    kind: PatternKind::Binding(vec_name.clone()),
                    span: sp,
                },
                ty: Some(vec_te),
                value: vec_new,
            },
            span: sp,
        };
        // `__flc.push(__fli)`
        let push = Stmt {
            kind: StmtKind::Expr(Expr {
                kind: ExprKind::MethodCall {
                    object: Box::new(ident(&vec_name)),
                    method: "push".to_string(),
                    turbofish: None,
                    args: vec![CallArg {
                        label: None,
                        mut_marker: false,
                        mut_marker_span: None,
                        value: ident(&elem_var),
                        span: sp,
                    }],
                    args_close_span: sp,
                },
                span: sp,
            }),
            span: sp,
        };
        // `for __fli in <recv>.flatten() { __flc.push(__fli); }`
        let for_loop = Stmt {
            kind: StmtKind::Expr(Expr {
                kind: ExprKind::For {
                    label: None,
                    pattern: Pattern {
                        kind: PatternKind::Binding(elem_var),
                        span: sp,
                    },
                    iterable: Box::new(flatten_recv.clone()),
                    attributes: Vec::new(),
                    body: Block {
                        stmts: vec![push],
                        final_expr: None,
                        span: sp,
                    },
                },
                span: sp,
            }),
            span: sp,
        };
        let block = Expr {
            kind: ExprKind::Block(Block {
                stmts: vec![let_vec, for_loop],
                final_expr: Some(Box::new(ident(&vec_name))),
                span: sp,
            }),
            span: sp,
        };
        Ok(Some(self.compile_expr(&block)?))
    }

    /// Lower `<outer>.flat_map(|param| param.iter()).collect()` where `<outer>`
    /// carries its OWN adaptors (`A.iter().map(f).flat_map(|p| p.iter())`) by
    /// pre-collecting the outer to a typed temp and reusing the identity
    /// flat_map (B-2026-07-04-2 sub-part 1). Emitted:
    ///
    /// ```text
    /// { let __fo: Vec[Vec[E]] = <outer.collect()>;          // Vec[EO], EO = Vec[E]
    ///   __fo.iter().flat_map(|param| param.iter()).collect() }
    /// ```
    ///
    /// Gated (by the caller) to an inner that iterates the param as a container
    /// (`param.iter()` / `param.into_iter()`), so the outer element type is
    /// `Vec[E]` — the flattened result type — and the temp is `Vec[Vec[E]]`,
    /// registered under a fresh `usize::MAX`-based synthetic span. The
    /// outer's `.collect()` recurses through the full pipeline (an unsupported
    /// outer adaptor bails via the recursive compile). Returns `Ok(None)` if the
    /// result type isn't a recorded `Vec[E]`.
    pub(super) fn try_compile_flat_map_pipeline_collect(
        &mut self,
        outer: &Expr,
        param: &str,
        inner: &Expr,
        call_span: &crate::token::Span,
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        let result_te = match self
            .drop_rc
            .owned_temp_drops
            .get(&(call_span.offset, call_span.length))
        {
            Some(te) => te.clone(),
            None => return Ok(None),
        };
        if !matches!(
            &result_te.kind,
            TypeKind::Path(p) if p.segments.last().map(|s| s.as_str()) == Some("Vec")
        ) {
            return Ok(None);
        }

        let uid = self.indexed_elem_counter;
        self.indexed_elem_counter += 1;
        let sp = *call_span;
        let fo_name = format!("__fo_{}", uid);
        let ident = |name: &str| Expr {
            kind: ExprKind::Identifier(name.to_string()),
            span: sp,
        };
        // The outer element type EO = Vec[E] = the flattened result type, so the
        // pre-collected temp is `Vec[EO]` = `Vec[Vec[E]]`.
        let temp_te = TypeExpr {
            kind: TypeKind::Path(PathExpr {
                segments: vec!["Vec".to_string()],
                generic_args: Some(vec![GenericArg::Type(result_te)]),
                span: sp,
            }),
            span: sp,
        };
        // Synthetic span for the outer `.collect()` result type (`Vec[Vec[E]]`).
        let outer_span = crate::token::Span {
            line: sp.line,
            column: sp.column,
            offset: usize::MAX - (uid as usize) - 1,
            length: 1,
        };
        self.drop_rc
            .owned_temp_drops
            .insert((outer_span.offset, outer_span.length), temp_te.clone());
        let outer_collect = Expr {
            kind: ExprKind::MethodCall {
                object: Box::new(outer.clone()),
                method: "collect".to_string(),
                turbofish: None,
                args: vec![],
                args_close_span: outer_span,
            },
            span: outer_span,
        };
        let let_fo = Stmt {
            kind: StmtKind::Let {
                is_mut: false,
                pattern: Pattern {
                    kind: PatternKind::Binding(fo_name.clone()),
                    span: sp,
                },
                ty: Some(temp_te),
                value: outer_collect,
            },
            span: sp,
        };
        // `__fo.iter().flat_map(|param| <inner>).collect()` — identity outer +
        // identity inner, typed by the ORIGINAL call span (`Vec[E]`).
        let fo_iter = Expr {
            kind: ExprKind::MethodCall {
                object: Box::new(ident(&fo_name)),
                method: "iter".to_string(),
                turbofish: None,
                args: vec![],
                args_close_span: sp,
            },
            span: sp,
        };
        let closure = Expr {
            kind: ExprKind::Closure {
                params: vec![ClosureParam {
                    pattern: Pattern {
                        kind: PatternKind::Binding(param.to_string()),
                        span: sp,
                    },
                    ty: None,
                    span: sp,
                }],
                capture_mode: None,
                prefix_span: None,
                body: Box::new(inner.clone()),
            },
            span: sp,
        };
        let flat_map_collect = Expr {
            kind: ExprKind::MethodCall {
                object: Box::new(Expr {
                    kind: ExprKind::MethodCall {
                        object: Box::new(fo_iter),
                        method: "flat_map".to_string(),
                        turbofish: None,
                        args: vec![CallArg {
                            label: None,
                            mut_marker: false,
                            mut_marker_span: None,
                            value: closure,
                            span: sp,
                        }],
                        args_close_span: sp,
                    },
                    span: sp,
                }),
                method: "collect".to_string(),
                turbofish: None,
                args: vec![],
                args_close_span: sp,
            },
            span: sp,
        };
        let block = Expr {
            kind: ExprKind::Block(Block {
                stmts: vec![let_fo],
                final_expr: Some(Box::new(flat_map_collect)),
                span: sp,
            }),
            span: sp,
        };
        Ok(Some(self.compile_expr(&block)?))
    }

    /// Lower `<base>.iter().chunks(n).collect()` (`overlapping == false`) or
    /// `.windows(n).collect()` (`overlapping == true`) into a `Vec[Vec[E]]`
    /// with an IN-PLACE fill. `base` is a named `Vec[E]`; `n > 0`. Emitted:
    ///
    /// ```text
    /// { let mut __ckv: Vec[Vec[E]] = Vec.new();
    ///   let __ckn = base.len();
    ///   let mut __cks = 0;
    ///   while __cks < __ckn {                     // windows: while __cks + n <= __ckn
    ///     let mut __cke0: Vec[E] = Vec.new();     // EMPTY (cap=0, safe to move)
    ///     __ckv.push(__cke0);
    ///     let __ckx = __ckv.len() - 1;
    ///     let mut __ckj = __cks;
    ///     let __cke = __cks + n;
    ///     while __ckj < __cke and __ckj < __ckn {
    ///       __ckv[__ckx].push(base[__ckj]);       // index-read deep-clones E, in place
    ///       __ckj = __ckj + 1;
    ///     }
    ///     __cks = __cks + step;                    // chunks: n, windows: 1
    ///   }
    ///   __ckv }
    /// ```
    ///
    /// The IN-PLACE fill is what makes this sound: the only moved binding is the
    /// EMPTY `__cke0` (nothing heap to double-free), and each heap element is
    /// cloned straight into `__ckv[__ckx]` — no consume-then-reuse loop-local
    /// heap binding for the synthetic AST to mishandle (that shape needs the
    /// ownership checker's RC fallback, which post-ownership codegen can't
    /// emit). `base` survives intact. Returns `Ok(None)` if the result type
    /// isn't a recorded `Vec[Vec[E]]`. B-2026-07-04-2 sub-part 1.
    pub(super) fn try_compile_chunks_windows_collect(
        &mut self,
        base: &Expr,
        n: i64,
        overlapping: bool,
        call_span: &crate::token::Span,
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        let outer_te = match self
            .drop_rc
            .owned_temp_drops
            .get(&(call_span.offset, call_span.length))
        {
            Some(te) => te.clone(),
            None => return Ok(None),
        };
        // Outer must be `Vec[<inner>]`; inner is the per-chunk `Vec[E]`.
        let inner_te = match &outer_te.kind {
            TypeKind::Path(p) if p.segments.last().map(|s| s.as_str()) == Some("Vec") => {
                match p.generic_args.as_ref().and_then(|ga| ga.first()) {
                    Some(GenericArg::Type(t)) => t.clone(),
                    _ => return Ok(None),
                }
            }
            _ => return Ok(None),
        };

        let uid = self.indexed_elem_counter;
        self.indexed_elem_counter += 1;
        let sp = *call_span;
        let outv = format!("__ckv_{}", uid);
        let lenv = format!("__ckn_{}", uid);
        let startv = format!("__cks_{}", uid);
        let chunkv = format!("__ckc_{}", uid);
        let jv = format!("__ckj_{}", uid);
        let endv = format!("__cke_{}", uid);

        let ident = |name: &str| Expr {
            kind: ExprKind::Identifier(name.to_string()),
            span: sp,
        };
        let i64_lit = |v: i64| Expr {
            kind: ExprKind::Integer(v, Some(crate::token::IntSuffix::I64)),
            span: sp,
        };
        let bin = |op: BinOp, l: Expr, r: Expr| Expr {
            kind: ExprKind::Binary {
                op,
                left: Box::new(l),
                right: Box::new(r),
            },
            span: sp,
        };
        let let_stmt = |is_mut: bool, name: &str, ty: Option<TypeExpr>, value: Expr| Stmt {
            kind: StmtKind::Let {
                is_mut,
                pattern: Pattern {
                    kind: PatternKind::Binding(name.to_string()),
                    span: sp,
                },
                ty,
                value,
            },
            span: sp,
        };
        let assign = |name: &str, value: Expr| Stmt {
            kind: StmtKind::Assign {
                target: ident(name),
                value,
            },
            span: sp,
        };
        let vec_new = || Expr {
            kind: ExprKind::Call {
                callee: Box::new(Expr {
                    kind: ExprKind::Path {
                        segments: vec!["Vec".to_string(), "new".to_string()],
                        generic_args: None,
                    },
                    span: sp,
                }),
                args: vec![],
            },
            span: sp,
        };
        let len_of = |e: Expr| Expr {
            kind: ExprKind::MethodCall {
                object: Box::new(e),
                method: "len".to_string(),
                turbofish: None,
                args: vec![],
                args_close_span: sp,
            },
            span: sp,
        };
        // `<recv>.push(<val>)` where recv is an arbitrary place expression.
        let push_to = |recv: Expr, val: Expr| Stmt {
            kind: StmtKind::Expr(Expr {
                kind: ExprKind::MethodCall {
                    object: Box::new(recv),
                    method: "push".to_string(),
                    turbofish: None,
                    args: vec![CallArg {
                        label: None,
                        mut_marker: false,
                        mut_marker_span: None,
                        value: val,
                        span: sp,
                    }],
                    args_close_span: sp,
                },
                span: sp,
            }),
            span: sp,
        };
        let while_stmt = |cond: Expr, body: Vec<Stmt>| Stmt {
            kind: StmtKind::Expr(Expr {
                kind: ExprKind::While {
                    label: None,
                    condition: Box::new(cond),
                    body: Block {
                        stmts: body,
                        final_expr: None,
                        span: sp,
                    },
                    attributes: Vec::new(),
                },
                span: sp,
            }),
            span: sp,
        };

        // Chunk builder: an inline BLOCK expr whose tail-return is a FRESH
        // per-chunk Vec — `{ let mut __ckc = Vec.new(); <fill>; __ckc }`. This
        // is the `mk()`-fresh-temp pattern inlined: the block value is a
        // tail-returned fresh Vec consumed by `__ckv.push(…)`, NOT a
        // consume-then-reuse loop-local binding (which would need the ownership
        // RC fallback the synthetic AST can't emit) and NOT an in-place fill of
        // a growing accumulator element (which double-freed on realloc). Each
        // `base[__ckj]` deep-clones (the heap-index-read fix), so `base`
        // survives and every clone is owned once by the result.
        let base_index = Expr {
            kind: ExprKind::Index {
                object: Box::new(base.clone()),
                index: Box::new(ident(&jv)),
            },
            span: sp,
        };
        let inner_body = vec![
            push_to(ident(&chunkv), base_index),
            assign(&jv, bin(BinOp::Add, ident(&jv), i64_lit(1))),
        ];
        let inner_cond = bin(
            BinOp::And,
            bin(BinOp::Lt, ident(&jv), ident(&endv)),
            bin(BinOp::Lt, ident(&jv), ident(&lenv)),
        );
        let chunk_block = Expr {
            kind: ExprKind::Block(Block {
                stmts: vec![
                    let_stmt(true, &chunkv, Some(inner_te.clone()), vec_new()),
                    let_stmt(false, &jv, None, ident(&startv)),
                    let_stmt(
                        false,
                        &endv,
                        None,
                        bin(BinOp::Add, ident(&startv), i64_lit(n)),
                    ),
                    while_stmt(inner_cond, inner_body),
                ],
                final_expr: Some(Box::new(ident(&chunkv))),
                span: sp,
            }),
            span: sp,
        };
        // Outer-loop body: push the freshly-built chunk, advance the start.
        let step = if overlapping { 1 } else { n };
        let outer_body = vec![
            push_to(ident(&outv), chunk_block),
            assign(&startv, bin(BinOp::Add, ident(&startv), i64_lit(step))),
        ];
        // Outer condition: chunks stop when start >= len; windows need a FULL
        // length-`n` window, so stop when start + n > len (i.e. start <= len-n).
        let outer_cond = if overlapping {
            bin(
                BinOp::LtEq,
                bin(BinOp::Add, ident(&startv), i64_lit(n)),
                ident(&lenv),
            )
        } else {
            bin(BinOp::Lt, ident(&startv), ident(&lenv))
        };

        let stmts = vec![
            let_stmt(true, &outv, Some(outer_te.clone()), vec_new()),
            let_stmt(false, &lenv, None, len_of(base.clone())),
            let_stmt(true, &startv, None, i64_lit(0)),
            while_stmt(outer_cond, outer_body),
        ];
        let block = Expr {
            kind: ExprKind::Block(Block {
                stmts,
                final_expr: Some(Box::new(ident(&outv))),
                span: sp,
            }),
            span: sp,
        };
        Ok(Some(self.compile_expr(&block)?))
    }

    /// Lower `A.iter().zip(B.iter()).collect()` into a `Vec[(EA, EB)]` by
    /// pairing the two indexable bases element-wise up to the shorter length
    /// (B-2026-07-04-2 sub-part 1). Returns `Ok(None)` if the result type isn't
    /// a recorded `Vec[T]`. The emitted block is:
    ///
    /// ```text
    /// { let mut __zv: Vec[(EA, EB)] = Vec.new();
    ///   let __zna = A.len();  let __znb = B.len();
    ///   let mut __zi = 0;
    ///   while __zi < __zna && __zi < __znb {
    ///     __zv.push((A[__zi], B[__zi]));
    ///     __zi = __zi + 1;
    ///   }
    ///   __zv }
    /// ```
    ///
    /// `A[i]` / `B[i]` clone the indexed element, so both borrowed sources
    /// survive and the accumulator owns independent copies.
    pub(super) fn try_compile_zip_identity_collect(
        &mut self,
        base_a: &Expr,
        base_b: &Expr,
        call_span: &crate::token::Span,
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        let vec_te = match self
            .drop_rc
            .owned_temp_drops
            .get(&(call_span.offset, call_span.length))
        {
            Some(te) => te.clone(),
            None => return Ok(None),
        };
        let elem_te = match &vec_te.kind {
            TypeKind::Path(p) if p.segments.last().map(|s| s.as_str()) == Some("Vec") => {
                p.generic_args.as_ref().and_then(|ga| match ga.first() {
                    Some(GenericArg::Type(t)) => Some(t.clone()),
                    _ => None,
                })
            }
            _ => return Ok(None),
        };
        // Heap-bearing paired tuples (`(String, String)`, `(String, i64)`, …)
        // are sound now that the pushed tuple `(A[i], B[i])` deep-clones each
        // named-Vec heap index-read (`compile_tuple` →
        // `maybe_defensive_copy_param_arg` → `clone_owned_vec_index_element`),
        // so the sources survive and the collect result owns independent
        // buffers (B-2026-07-04-2 heap-zip leg). The clone fires ONLY for a
        // named-Vec identifier base; a non-identifier base (e.g. a fresh-temp
        // `foo().iter()`) whose element is heap would still alias, so require
        // both bases to be clone-eligible named Vecs before admitting a heap
        // element — otherwise keep bailing to the loud dispatch-fail (never a
        // miscompile). A fully-POD tuple needs no clone and admits any base.
        // `te_owns_option_heap_payload` closes `type_expr_has_drop_heap`'s
        // Option blind spot: an `Option[String]`-bearing element is NOT POD
        // (its drop frees the `Some` payload), so it must take the
        // clone-eligible-gated path, not the admit-any-base one.
        let elem_is_pod = match &elem_te {
            Some(te) => !self.type_expr_has_drop_heap(te) && !self.te_owns_option_heap_payload(te),
            None => return Ok(None),
        };
        let base_is_named_vec = |cg: &Self, base: &Expr| {
            matches!(&base.kind, ExprKind::Identifier(n)
                if cg.var_types.var_elem_type_exprs.contains_key(n.as_str()))
        };
        let heap_bases_clone_eligible =
            base_is_named_vec(self, base_a) && base_is_named_vec(self, base_b);
        if !(elem_is_pod || heap_bases_clone_eligible) {
            return Ok(None);
        }

        let uid = self.indexed_elem_counter;
        self.indexed_elem_counter += 1;
        let sp = *call_span;
        let vec_name = format!("__zv_{}", uid);
        let na_name = format!("__zna_{}", uid);
        let nb_name = format!("__znb_{}", uid);
        let i_name = format!("__zi_{}", uid);
        let ident = |name: &str| Expr {
            kind: ExprKind::Identifier(name.to_string()),
            span: sp,
        };
        let i64_lit = |n: i64| Expr {
            kind: ExprKind::Integer(n, Some(crate::token::IntSuffix::I64)),
            span: sp,
        };
        let bin = |op: BinOp, l: Expr, r: Expr| Expr {
            kind: ExprKind::Binary {
                op,
                left: Box::new(l),
                right: Box::new(r),
            },
            span: sp,
        };
        let let_stmt = |is_mut: bool, name: &str, ty: Option<TypeExpr>, value: Expr| Stmt {
            kind: StmtKind::Let {
                is_mut,
                pattern: Pattern {
                    kind: PatternKind::Binding(name.to_string()),
                    span: sp,
                },
                ty,
                value,
            },
            span: sp,
        };
        // `<base>.len()`
        let len_of = |base: &Expr| Expr {
            kind: ExprKind::MethodCall {
                object: Box::new(base.clone()),
                method: "len".to_string(),
                turbofish: None,
                args: vec![],
                args_close_span: sp,
            },
            span: sp,
        };
        // `<base>[<i>]`
        let index_of = |base: &Expr| Expr {
            kind: ExprKind::Index {
                object: Box::new(base.clone()),
                index: Box::new(ident(&i_name)),
            },
            span: sp,
        };

        let vec_new = Expr {
            kind: ExprKind::Call {
                callee: Box::new(Expr {
                    kind: ExprKind::Path {
                        segments: vec!["Vec".to_string(), "new".to_string()],
                        generic_args: None,
                    },
                    span: sp,
                }),
                args: vec![],
            },
            span: sp,
        };

        // Loop body: `__zv.push((A[__zi], B[__zi])); __zi = __zi + 1;`
        let push_pair = Stmt {
            kind: StmtKind::Expr(Expr {
                kind: ExprKind::MethodCall {
                    object: Box::new(ident(&vec_name)),
                    method: "push".to_string(),
                    turbofish: None,
                    args: vec![CallArg {
                        label: None,
                        mut_marker: false,
                        mut_marker_span: None,
                        value: Expr {
                            kind: ExprKind::Tuple(vec![index_of(base_a), index_of(base_b)]),
                            span: sp,
                        },
                        span: sp,
                    }],
                    args_close_span: sp,
                },
                span: sp,
            }),
            span: sp,
        };
        let incr = Stmt {
            kind: StmtKind::Assign {
                target: ident(&i_name),
                value: bin(BinOp::Add, ident(&i_name), i64_lit(1)),
            },
            span: sp,
        };
        // `while __zi < __zna && __zi < __znb { … }`
        let while_cond = bin(
            BinOp::And,
            bin(BinOp::Lt, ident(&i_name), ident(&na_name)),
            bin(BinOp::Lt, ident(&i_name), ident(&nb_name)),
        );
        let while_stmt = Stmt {
            kind: StmtKind::Expr(Expr {
                kind: ExprKind::While {
                    label: None,
                    condition: Box::new(while_cond),
                    body: Block {
                        stmts: vec![push_pair, incr],
                        final_expr: None,
                        span: sp,
                    },
                    attributes: Vec::new(),
                },
                span: sp,
            }),
            span: sp,
        };

        let block = Expr {
            kind: ExprKind::Block(Block {
                stmts: vec![
                    let_stmt(true, &vec_name, Some(vec_te), vec_new),
                    let_stmt(false, &na_name, None, len_of(base_a)),
                    let_stmt(false, &nb_name, None, len_of(base_b)),
                    let_stmt(true, &i_name, None, i64_lit(0)),
                    while_stmt,
                ],
                final_expr: Some(Box::new(ident(&vec_name))),
                span: sp,
            }),
            span: sp,
        };
        Ok(Some(self.compile_expr(&block)?))
    }
}
