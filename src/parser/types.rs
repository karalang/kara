//! Type-expression parsing.
//!
//! Houses `parse_type` (the big TypeKind dispatch covering primitives,
//! generics, references, slices, options, results, function types
//! `Fn(args) -> ret with E`, tuple types, path types, etc.) and
//! `parse_path_type` (the `Foo.Bar[T1, T2]` PathExpr form used inside
//! type position).
//!
//! Lives in a sibling `impl super::Parser` block.

use crate::ast::*;
use crate::parser::ImplTraitBlockReason;
use crate::token::Token;

impl super::Parser {
    // ── Types ────────────────────────────────────────────────────

    /// Whether `tok` can START a type expression. Used only to disambiguate
    /// the contextual `frozen` mode from an ordinary identifier, so it lists
    /// the type-leading tokens rather than trying to be a full FIRST set:
    /// `frozen Node` is the mode, a lone `frozen` is a path.
    fn token_begins_type(tok: &Token) -> bool {
        matches!(
            tok,
            Token::Identifier { .. }
                | Token::Ref
                | Token::Mut
                | Token::LeftParen
                | Token::LeftBracket
                | Token::Star
        )
    }

    // Depth-guarded shell: type nesting (`Vec[Vec[…]]`, tuple types) shares
    // the recursion budget with expressions and patterns (B-2026-08-16-4).
    pub(crate) fn parse_type(&mut self) -> Option<TypeExpr> {
        if !self.enter_recursion() {
            return None;
        }
        // Red-zone stack growth — see parse_expr_bp_with_ctx for sizing.
        let result = stacker::maybe_grow(512 * 1024, 16 * 1024 * 1024, || self.parse_type_inner());
        self.exit_recursion();
        result
    }

    fn parse_type_inner(&mut self) -> Option<TypeExpr> {
        let start = self.current_span();

        // One-shot: take the permission and clear it, so ONLY the outermost
        // `parse_type` of a parameter can accept `frozen`. Clearing it inside
        // the `frozen` branch alone was not enough — generic arguments are
        // parsed by a different descent, so `Vec[frozen N]` kept the flag lit
        // and was wrongly accepted. Taking it here covers every nested position
        // by construction rather than by enumerating them.
        let frozen_ok = std::mem::replace(&mut self.frozen_ok, false);

        // `frozen Type` — B-2026-08-01-33 mechanism 3, stage 1.
        //
        // A CONTEXTUAL keyword, not a reserved word: `frozen` stays a legal
        // identifier everywhere else, so adding the mode cannot break an
        // existing program that uses the name. The guard is the same shape the
        // `mut Slice[T]` arm below already uses — recognise it only when the
        // NEXT token can begin a type, so a bare `frozen` used as a type name
        // still parses as the path it always did.
        // THE MODE LEAVES THE TYPE TREE HERE, and that is the point. It is not
        // discarded — `parse_param` picks it up off `frozen_consumed` and
        // records it on [`Param::is_frozen`], where every later phase can see
        // it. What it does NOT do is travel inside the `TypeExpr`.
        //
        // `TypeKind::Frozen` exists and every walk in the compiler handles it,
        // but nothing constructs it, because a mode inside the type tree means
        // every phase that unwraps `Ref | MutRef` must learn a third form.
        // Trying that first turned up four rounds of such sites in codegen
        // alone (`call_dispatch`, `functions`, `mono`, then the param
        // type-name registry) with no reason to believe the fourth was the
        // last, and each one is a place a later phase could disagree about what
        // `frozen` means. A bit on the parameter cannot disagree with itself,
        // and it keeps the mode away from codegen by construction — codegen
        // will learn which values are non-counting through the plain-data
        // `elidable_ref_params` hint channel, per the codegen-containment
        // invariant. The variant stays for stage 2, when widening `frozen`
        // past parameter position will genuinely need a type-level mode.
        if let Token::Identifier { name, .. } = self.peek_token_ref() {
            if name == "frozen" && Self::token_begins_type(self.peek_token_ref_at(1)) {
                let kw_span = self.current_span();
                self.advance();
                if frozen_ok {
                    self.frozen_consumed = true;
                } else {
                    self.error_at(
                        "`frozen` is only supported on a Kara function's parameter \
                         type (stage 1); it is not yet accepted on a `let` \
                         annotation, a struct field, a return type, inside a \
                         generic argument, or on a foreign-import (`extern`) \
                         parameter",
                        kw_span,
                    );
                }
                // Parsing continues through the inner type either way, so one
                // misplaced keyword does not cascade into unrelated errors.
                // The flag was already taken above, so a nested `frozen`
                // (`frozen frozen T`) is rejected on the inner occurrence.
                let inner = self.parse_type()?;
                if !frozen_ok {
                    // Already reported; hand back the bare type so the rest of
                    // the signature keeps its shape.
                    return Some(inner);
                }
                // `frozen T` LOWERS TO A BORROW — `ref T` — and this is the
                // whole of stage 1's RC suppression.
                //
                // The design calls a `frozen` value "non-owning, non-counting".
                // Non-owning is exactly what `ref` already means, and codegen
                // emits no retain/release for a borrow because the caller keeps
                // ownership. Measured: an owned `shared` pass-through emits
                // rc_inc=4/rc_dec=9, the same chain through `ref` emits 0/0 —
                // and it still emits 0/0 with `KARAC_RC_ELIDE_REF_PARAMS=0`, so
                // that zero is the BORROW CONVENTION, not the rc-elide pass.
                //
                // Modes are declared at the signature (CLAUDE.md: body-level
                // ownership analysis "is not a signature-derivation
                // mechanism"), so a mode that wants borrow semantics has to say
                // so in the declared type. Inferring `Ref` into `param_modes`
                // is not enough and was measured not to be: the mode was
                // already `Ref` there while codegen still emitted the owned
                // traffic.
                //
                // Note what does NOT happen here: `TypeKind::Frozen` is still
                // never constructed. Codegen sees `Ref`, a form it already
                // handles on a shipped, ASAN-verified path, so this adds no
                // unwrap site anywhere. The frozen-ness itself stays on
                // `Param::is_frozen`, which is what the escape checker, the
                // freeze-site check, and `par` admission read.
                return Some(TypeExpr {
                    kind: TypeKind::Ref(Box::new(inner)),
                    span: self.span_from(&start),
                });
            }
        }

        match self.peek_token_ref() {
            // ref Type
            Token::Ref => {
                self.advance();
                let inner = self.parse_type()?;
                Some(TypeExpr {
                    kind: TypeKind::Ref(Box::new(inner)),
                    span: self.span_from(&start),
                })
            }
            // mut ref Type  |  mut Slice[T]
            Token::Mut => {
                self.advance();
                // `mut Slice[T]` — mutable slice view (no `ref` keyword).
                if let Token::Identifier { name, .. } = self.peek_token_ref() {
                    if name == "Slice" {
                        // Parse the Slice path as a normal type, then strip
                        // down to its element and re-wrap as MutSlice.
                        let slice_ty = self.parse_type()?;
                        let element = match slice_ty.kind {
                            TypeKind::Path(ref path)
                                if path.segments.len() == 1 && path.segments[0] == "Slice" =>
                            {
                                match &path.generic_args {
                                    Some(args) if args.len() == 1 => match &args[0] {
                                        crate::ast::GenericArg::Type(t) => t.clone(),
                                        _ => {
                                            self.error(
                                                "mut Slice[T] requires a type argument, found const",
                                            );
                                            return None;
                                        }
                                    },
                                    _ => {
                                        self.error(
                                            "mut Slice[T] requires exactly one type argument",
                                        );
                                        return None;
                                    }
                                }
                            }
                            _ => {
                                self.error("expected Slice[T] after `mut`");
                                return None;
                            }
                        };
                        return Some(TypeExpr {
                            kind: TypeKind::MutSlice(Box::new(element)),
                            span: self.span_from(&start),
                        });
                    }
                }
                // Otherwise `mut ref T`.
                self.expect(&Token::Ref)?;
                let inner = self.parse_type()?;
                Some(TypeExpr {
                    kind: TypeKind::MutRef(Box::new(inner)),
                    span: self.span_from(&start),
                })
            }
            // weak Type
            Token::Weak => {
                self.advance();
                let inner = self.parse_type()?;
                Some(TypeExpr {
                    kind: TypeKind::Weak(Box::new(inner)),
                    span: self.span_from(&start),
                })
            }
            // *const T or *mut T
            Token::Star => {
                self.advance();
                let is_mut = if self.eat(&Token::Mut) {
                    true
                } else {
                    // expect "const" as identifier
                    match self.peek_token_ref() {
                        Token::Const => {
                            self.advance();
                            false
                        }
                        _ => {
                            self.error("Expected 'const' or 'mut' after '*' in pointer type");
                            return None;
                        }
                    }
                };
                let inner = self.parse_type()?;
                Some(TypeExpr {
                    kind: TypeKind::Pointer {
                        is_mut,
                        inner: Box::new(inner),
                    },
                    span: self.span_from(&start),
                })
            }
            // () unit type or (A, B) tuple type
            Token::LeftParen => {
                self.advance();
                if self.eat(&Token::RightParen) {
                    return Some(TypeExpr {
                        kind: TypeKind::Unit,
                        span: self.span_from(&start),
                    });
                }
                let first = self.parse_type()?;
                if self.eat(&Token::Comma) {
                    // Tuple type
                    let mut types = vec![first];
                    while !self.check(&Token::RightParen) && !self.is_at_end() {
                        types.push(self.parse_type()?);
                        if !self.eat(&Token::Comma) {
                            break;
                        }
                    }
                    self.expect(&Token::RightParen)?;
                    Some(TypeExpr {
                        kind: TypeKind::Tuple(types),
                        span: self.span_from(&start),
                    })
                } else {
                    // Parenthesized type
                    self.expect(&Token::RightParen)?;
                    Some(first)
                }
            }
            // `impl Trait[GenericArgs] [with EFFECT_LIST]` — existential
            // / argument-sugar type marker (see design.md § `impl Trait`
            // (Existential Types) and phase-5-diagnostics.md line 391).
            //
            // Slice 1: parser surface + AST node only. The four legal
            // positions (argument-type, return-type, trait-method-
            // return, RHS of `type` aliases) all reach this arm through
            // their normal `parse_type` path; the two illegal positions
            // (`Vec[impl T]`-style nested generic-args, and trait-method
            // argument-type position) are guarded by the
            // `impl_trait_block_stack` — see `parse_generic_type_args`
            // and `parse_param` for the matching push sites.
            //
            // Surface form is `impl <TraitPath>[<args>] [with E]`. The
            // trait path is parsed via `parse_path_type` so multi-
            // segment paths (`std.iter.Iterator`) and generic args
            // (`Iterator[Item = i64]`) are handled uniformly with
            // regular path types.
            // `dyn TraitPath[GENERIC_ARGS]` — trait-object type marker.
            // The general `dyn Trait` feature is P1-deferred per
            // design.md § Polymorphism; the parser surface lands today
            // only so the `impl Trait` epic's slice-5 check
            // (RPITIT-blocks-dyn) has a syntactic target. The
            // typechecker emits one of two focused diagnostics on
            // lowering (`E_RPITIT_INCOMPATIBLE_WITH_DYN` /
            // `E_DYN_TRAIT_NOT_IMPLEMENTED_YET`); the parser builds the
            // `TypeKind::Dyn` node regardless so error recovery stays
            // clean.
            Token::Dyn => {
                let dyn_kw_span = self.current_span();
                self.advance(); // consume `dyn`
                let (trait_path, assoc_bindings) = self.parse_trait_path_with_bindings()?;
                let (segments, args_opt) = (trait_path.segments, trait_path.generic_args);
                let args: Vec<GenericArg> = args_opt.unwrap_or_default();
                let trait_path_clean = PathExpr {
                    segments,
                    generic_args: None,
                    span: self.span_from(&dyn_kw_span),
                };
                let span = self.span_from(&start);
                Some(TypeExpr {
                    kind: TypeKind::Dyn {
                        trait_path: trait_path_clean,
                        args,
                        assoc_bindings,
                        span,
                    },
                    span,
                })
            }
            Token::Impl => {
                let impl_kw_span = self.current_span();
                self.advance(); // consume `impl`

                // Position rejection. We still parse the body of the
                // `impl Trait` expression after emitting the
                // diagnostic — error recovery: producing a
                // `TypeKind::Error` here would cascade into a noisy
                // "expected type" downstream, while producing a real
                // `ImplTrait` lets the rest of the signature parse
                // cleanly and gives the user one focused diagnostic
                // rather than a cluster. The diagnostic is anchored
                // at the `impl` keyword's span.
                if let Some(reason) = self.current_impl_trait_block() {
                    let msg = match reason {
                        ImplTraitBlockReason::NestedGenericArg => {
                            "error[E_IMPL_TRAIT_IN_NESTED_POSITION]: `impl Trait` is not \
                             permitted inside a nested generic-argument position at v1; \
                             introduce an explicit generic parameter on the enclosing \
                             function (e.g. `fn f[T: Trait](x: Vec[T])`) instead. \
                             Deep-position `impl Trait` is post-v1 — see design.md \
                             § `impl Trait` (Existential Types)."
                                .to_string()
                        }
                        ImplTraitBlockReason::TraitMethodArg => {
                            "error[E_IMPL_TRAIT_IN_TRAIT_METHOD_ARG]: `impl Trait` is not \
                             permitted in trait-method argument position; use the explicit \
                             generic form `fn method[T: Trait](x: T)` on the trait method \
                             declaration instead. The compiler restricts argument-position \
                             `impl Trait` to free functions and impl-block methods — see \
                             design.md § `impl Trait` (Existential Types)."
                                .to_string()
                        }
                    };
                    self.errors.push(crate::parser::ParseError {
                        kind: crate::parser::ParseErrorKind::Syntax,
                        message: msg,
                        span: impl_kw_span,
                    });
                }

                // Trait path + generic args (parsed uniformly with
                // regular path types), plus any inline associated-type
                // bindings — `impl Iterator[Item = i64]` (B-2026-08-22-4).
                let (trait_path, assoc_bindings) = self.parse_trait_path_with_bindings()?;
                let (segments, args_opt) = (trait_path.segments, trait_path.generic_args);
                let args: Vec<GenericArg> = args_opt.unwrap_or_default();

                // Rebuild a clean PathExpr (without the generic_args
                // — those live in the ImplTrait variant's `args`
                // field, not nested under the path).
                let trait_path_clean = PathExpr {
                    segments,
                    generic_args: None,
                    span: self.span_from(&impl_kw_span),
                };

                // Optional `with EFFECT_LIST` clause. Mirrors the
                // `FnType` arm above — `parse_effect_list` itself
                // consumes the `with` keyword, so we peek-only and
                // dispatch on the token immediately following `with`.
                let use_effects = if self.check(&Token::With) {
                    let saved = self.pos;
                    if let Some(token) = self.tokens.get(self.pos + 1) {
                        if matches!(token.token, Token::Underscore) {
                            // `impl Trait with _` — anonymous-
                            // polymorphic use-effect ceiling. Same
                            // shape as `Fn(...) with _`.
                            self.advance(); // with
                            self.advance(); // _
                            Some(EffectList {
                                items: vec![EffectItem::Polymorphic],
                                span: self.span_from(&impl_kw_span),
                            })
                        } else {
                            let effect_vars: Vec<String> = self.current_effect_vars().to_vec();
                            // Nested `Fn(...) with E` type: a trailing comma is
                            // a legitimate param/list separator, not a stray
                            // effect-clause comma — recovery off.
                            match self.parse_effect_list(&effect_vars, false) {
                                Some(effects) => Some(effects),
                                None => {
                                    self.pos = saved;
                                    return None;
                                }
                            }
                        }
                    } else {
                        None
                    }
                } else {
                    None
                };

                let span = self.span_from(&start);
                Some(TypeExpr {
                    kind: TypeKind::ImplTrait {
                        trait_path: trait_path_clean,
                        args,
                        assoc_bindings,
                        use_effects,
                        span,
                    },
                    span,
                })
            }
            // Fn(T) -> U with _ — and the once-callable variant `OnceFn(...)`
            // (round 12.46, Step 4). Both share the same AST shape; the
            // `is_once` flag distinguishes them so `lower_type_expr` can emit
            // `Type::OnceFunction` for the OnceFn form.
            Token::Identifier { ref name, .. } if name == "Fn" || name == "OnceFn" => {
                let is_once = name == "OnceFn";
                self.advance();
                self.expect(&Token::LeftParen)?;
                let mut params = Vec::new();
                while !self.check(&Token::RightParen) && !self.is_at_end() {
                    params.push(self.parse_type()?);
                    if !self.eat(&Token::Comma) {
                        break;
                    }
                }
                self.expect(&Token::RightParen)?;

                let return_type = if self.eat(&Token::Arrow) {
                    Some(Box::new(self.parse_type()?))
                } else {
                    None
                };

                let effect_spec = if self.check(&Token::With) {
                    // Peek-only: `parse_effect_list` consumes the `with`
                    // keyword itself and then handles `_` / verbs / named
                    // effect variables / group names uniformly. Pre-
                    // consuming would force the call into the without-
                    // `with`-prefix branch, which only handles bare verbs
                    // and silently drops named variables (latent bug fixed
                    // here as part of round 9).
                    let saved = self.pos;
                    if let Some(token) = self.tokens.get(self.pos + 1) {
                        if matches!(token.token, Token::Underscore) {
                            self.advance(); // with
                            self.advance(); // _
                            Some(EffectSpec::Polymorphic)
                        } else {
                            let effect_vars: Vec<String> = self.current_effect_vars().to_vec();
                            // Nested `Fn(...) with E` type: trailing comma is a
                            // legitimate separator — recovery off (see above).
                            match self.parse_effect_list(&effect_vars, false) {
                                Some(effects) => Some(EffectSpec::Specific(effects)),
                                None => {
                                    self.pos = saved;
                                    return None;
                                }
                            }
                        }
                    } else {
                        None
                    }
                } else {
                    None
                };

                Some(TypeExpr {
                    kind: TypeKind::FnType {
                        params,
                        return_type,
                        effect_spec,
                        is_once,
                    },
                    span: self.span_from(&start),
                })
            }
            // Path type: ident[::ident]*[<T, U>]
            Token::Identifier { .. } | Token::SelfType => {
                let path = self.parse_path_type()?;
                Some(TypeExpr {
                    kind: TypeKind::Path(path),
                    span: self.span_from(&start),
                })
            }
            _ => {
                self.error(&format!("Expected type, found {:?}", self.peek_token()));
                None
            }
        }
    }

    /// `parse_path_type` for a TRAIT path — the `Src[Item = i64]` after an
    /// `impl` / `dyn` keyword — returning the positional args on the path and
    /// the inline associated-type bindings separately.
    ///
    /// `impl Trait` and `dyn Trait` name a TRAIT, not a type, so their bracket
    /// list is the same grammar a trait bound's is: `IDENT = TYPE` inside it
    /// binds an associated type rather than passing a positional argument
    /// (syntax.md § BOUND_ARG). Ordinary path types keep the flag off, so
    /// `Vec[Item = i64]` stays the parse error it should be.
    ///
    /// Mirrors `parse_trait_bound`'s save/enable/drain dance exactly, and for
    /// the same reason: the shared arg parser collects bindings into
    /// `pending_assoc_bindings`, and the saved/restored pair keeps a nested
    /// occurrence from inheriting or clobbering an outer one's.
    fn parse_trait_path_with_bindings(&mut self) -> Option<(PathExpr, Vec<AssocBinding>)> {
        let saved_allowed = self.assoc_bindings_allowed;
        let saved_pending = std::mem::take(&mut self.pending_assoc_bindings);
        self.assoc_bindings_allowed = true;
        let path = self.parse_path_type();
        self.assoc_bindings_allowed = saved_allowed;
        let bindings = std::mem::replace(&mut self.pending_assoc_bindings, saved_pending);
        Some((path?, bindings))
    }

    pub(crate) fn parse_path_type(&mut self) -> Option<PathExpr> {
        let start = self.current_span();
        let segments = self.parse_path_segments()?;

        // Check for generic args [T, U] — unambiguous in type position
        let mut generic_args = if self.check(&Token::LeftBracket) {
            Some(self.parse_generic_type_args()?)
        } else {
            None
        };

        let span = self.span_from(&start);
        self.take_container_hasher_arg(&segments, &mut generic_args, span);

        Some(PathExpr {
            segments,
            generic_args,
            span,
        })
    }

    /// Remove a `Map[K, V, H]` / `Set[T, H]` trailing hasher argument from the
    /// path's generic arguments and record it on the parser (B-2026-08-21-6,
    /// B-2026-08-22-6).
    ///
    /// See [`crate::ast::Program::container_hashers`] for why the argument is
    /// deleted here rather than left in the tree. In one sentence: every later
    /// phase recognizes a `Map` by "head name plus exactly two arguments", so
    /// an extra one stops the type from being a `Map` to any of them.
    ///
    /// WHY THIS TAKES AN UNRECOGNIZED NAME TOO. Before user hashers, only the
    /// two compiler-known selectors were removed and everything else fell
    /// through to `take_hasher_type_arg`, whose whitelist reported it. A user
    /// hasher is any type that `impl BuildHasher for`, and the parser has no
    /// impl table — it runs before resolution — so it cannot tell
    /// `MyBuildHasher` from `i64`. It therefore removes ANY single trailing
    /// path argument and records the name; validation moves one phase later to
    /// `TypeChecker::check_recorded_container_hasher`, which does have the
    /// impl table
    /// and reports `Map[K, V, i64]` there instead. The alternative — leaving
    /// unknown names in place — would make a legitimate `Map[K, V,
    /// MyBuildHasher]` stop being a `Map` to every later phase, which is the
    /// exact failure this deletion exists to avoid.
    ///
    /// Still removes ONLY a bare path in exactly the trailing position, so
    /// `Map[K, V, H, X]`, `Map[K, V, Vec[u8]]` and `SortedMap[K, V, H]` all keep
    /// their arguments and reach `take_hasher_type_arg`, which reports them.
    fn take_container_hasher_arg(
        &mut self,
        segments: &[String],
        generic_args: &mut Option<Vec<GenericArg>>,
        span: crate::token::Span,
    ) {
        // Single-segment only: `Map` / `Set` are prelude types written bare, so
        // a qualified `some.module.Map` is somebody ELSE's type and its third
        // argument is not a hasher. Falling through leaves the argument in
        // place, which is the safe direction — the typechecker then reports it
        // rather than this quietly eating it.
        if segments.len() != 1 {
            return;
        }
        let base = match segments[0].as_str() {
            "Map" => 2,
            "Set" => 1,
            _ => return,
        };
        let Some(args) = generic_args.as_mut() else {
            return;
        };
        if args.len() != base + 1 {
            return;
        }
        let GenericArg::Type(te) = &args[base] else {
            return;
        };
        let TypeKind::Path(p) = &te.kind else {
            return;
        };
        if p.generic_args.is_some() {
            return;
        }
        let kind = match p.segments.last().map(String::as_str) {
            Some("FxBuildHasher") => crate::hasher_kind::HasherKind::Fx,
            Some("SipHash13BuildHasher") => crate::hasher_kind::HasherKind::SipHash13,
            // Anything else is a candidate user hasher. Keyed on the LAST
            // segment so a qualified `my.hashers.MyBuildHasher` records the
            // same name the impl table is keyed by.
            Some(name) => crate::hasher_kind::HasherKind::User(name.to_string()),
            None => return,
        };
        args.truncate(base);
        self.container_hashers
            .insert(crate::resolver::SpanKey::from_span(&span), kind);
    }
}
