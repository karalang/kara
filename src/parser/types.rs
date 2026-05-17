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

    pub(crate) fn parse_type(&mut self) -> Option<TypeExpr> {
        let start = self.current_span();

        match self.peek_token() {
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
                if let Token::Identifier { name, .. } = self.peek_token() {
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
                    match self.peek_token() {
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
                let trait_path = self.parse_path_type()?;
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
                        span: span.clone(),
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
                        message: msg,
                        span: impl_kw_span.clone(),
                    });
                }

                // Trait path + generic args (parsed uniformly with
                // regular path types).
                let trait_path = self.parse_path_type()?;
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
                            match self.parse_effect_list(&effect_vars) {
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
                        use_effects,
                        span: span.clone(),
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
                            match self.parse_effect_list(&effect_vars) {
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

    pub(crate) fn parse_path_type(&mut self) -> Option<PathExpr> {
        let start = self.current_span();
        let segments = self.parse_path_segments()?;

        // Check for generic args [T, U] — unambiguous in type position
        let generic_args = if self.check(&Token::LeftBracket) {
            Some(self.parse_generic_type_args()?)
        } else {
            None
        };

        Some(PathExpr {
            segments,
            generic_args,
            span: self.span_from(&start),
        })
    }
}
