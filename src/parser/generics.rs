//! Generics, trait bounds, `where`/`requires`/`ensures` clauses,
//! generic-type-args, and the struct body grammar.
//!
//! Houses `parse_optional_generic_params` / `parse_generic_params`
//! (the `[T, const N: i64, with E]` form), `parse_trait_bound`
//! (the `T: Trait + 'a`-style bound recognizer),
//! `parse_optional_where_clause` (the trailing `where` clause for
//! item signatures), `parse_requires_clauses` /
//! `parse_ensures_clauses` (Hoare-style pre/post conditions),
//! `parse_struct_body` (struct fields + invariants), and
//! `parse_generic_type_args` (the call-site `[T1, T2, …]` shape).
//!
//! Lives in a sibling `impl super::Parser` block.

use crate::ast::*;
use crate::lexer::IdentClass;
use crate::token::Token;

impl super::Parser {
    pub(crate) fn parse_optional_generic_params(&mut self) -> Option<GenericParams> {
        if !self.check(&Token::LeftBracket) {
            return None;
        }
        self.parse_generic_params()
    }

    pub(crate) fn parse_generic_params(&mut self) -> Option<GenericParams> {
        let start = self.current_span();
        self.expect(&Token::LeftBracket)?;

        let mut params = Vec::new();
        let mut effect_params: Vec<EffectParam> = Vec::new();
        // Per design.md (line 4858): type params come first, then `with`
        // introduces effect-variable params. Once we've seen `with`, every
        // subsequent comma-separated identifier is an effect variable —
        // both the `[with E, F]` and `[with E, with F]` spellings are
        // accepted. Phase 6 slice 8ac (design.md line 736) adds the
        // type-param-style `E: Effect` form: a generic-param with a
        // leading `Effect`-bound is reclassified into `effect_params`
        // and may appear at any position in the list (not gated on the
        // sticky `with` mode). The two spellings are equivalent —
        // `[T, E: Effect]` and `[T, with E]` produce the same AST shape
        // modulo the `bounds: vec![Effect]` marker, which downstream
        // phases can consult for future granularity.
        let mut in_effect_params = false;
        loop {
            if self.check(&Token::RightBracket) {
                break;
            }
            // `with E[, F]` — `with` enters effect-vars mode (sticky).
            if self.check(&Token::With) {
                self.advance();
                in_effect_params = true;
            }
            if in_effect_params {
                let ep_start = self.current_span();
                let name = self.expect_identifier()?;
                effect_params.push(EffectParam {
                    name,
                    bounds: Vec::new(),
                    span: self.span_from(&ep_start),
                });
                if !self.eat(&Token::Comma) {
                    break;
                }
                continue;
            }
            let pstart = self.current_span();
            // `...S` — shape-variadic param (syntax.md § GENERIC_PARAM):
            // binds a full dim list (Shape kind). Bounds are not legal on
            // a variadic shape param at v1; the name follows the same
            // Type-class convention as type params.
            if self.check(&Token::DotDotDot) {
                self.advance();
                let name = self.expect_identifier()?;
                let pname_span = self.span_from(&pstart);
                self.check_ident_class(
                    &name,
                    IdentClass::Type,
                    "shape-variadic generic parameter",
                    pname_span,
                );
                params.push(GenericParam {
                    name,
                    bounds: Vec::new(),
                    is_const: false,
                    const_type: None,
                    is_variadic_shape: true,
                    span: self.span_from(&pstart),
                });
                if !self.eat(&Token::Comma) {
                    break;
                }
                continue;
            }
            // Check for const generic: `const N: Type`
            if self.check(&Token::Const) {
                self.advance();
                let name = self.expect_identifier()?;
                let pname_span = self.span_from(&pstart);
                // Const generic params follow the same Type-class convention
                // as type generic params (see design.md § Identifiers and
                // Naming). Single uppercase letters (`N`, `K`) and
                // PascalCase names both classify as Type-class.
                self.check_ident_class(
                    &name,
                    IdentClass::Type,
                    "const generic parameter",
                    pname_span,
                );
                self.expect(&Token::Colon)?;
                let ty = self.parse_type()?;
                params.push(GenericParam {
                    name,
                    bounds: Vec::new(),
                    is_const: true,
                    const_type: Some(ty),
                    is_variadic_shape: false,
                    span: self.span_from(&pstart),
                });
            } else {
                let name = self.expect_identifier()?;
                let pname_span = self.span_from(&pstart);
                self.check_ident_class(
                    &name,
                    IdentClass::Type,
                    "generic type parameter",
                    pname_span,
                );
                let mut bounds = Vec::new();
                if self.eat(&Token::Colon) {
                    loop {
                        let bound = self.parse_trait_bound()?;
                        bounds.push(bound);
                        if !self.eat(&Token::Plus) {
                            break;
                        }
                    }
                }
                // Slice 8ac (phase 6 line 26): a bound list whose first
                // entry is the bare `Effect` trait reclassifies the
                // generic-param into an effect-parameter. The check is
                // purely structural — `Effect` is a built-in marker
                // recognised at parse time, not resolved through scope;
                // `Effect[args]` and multi-segment paths fall through
                // to the type-param arm. Bounds beyond the leading
                // `Effect` (e.g. `Effect + Foo`) are preserved on the
                // AST node for future granularity (design.md line 3150
                // reserves `with E: no writes(R)`-style constraints for
                // Phase 7); v1 ignores them at the effect-checker layer.
                let is_effect_bounded = bounds
                    .first()
                    .map(|b| b.path.len() == 1 && b.path[0] == "Effect" && b.generic_args.is_none())
                    .unwrap_or(false);
                if is_effect_bounded {
                    effect_params.push(EffectParam {
                        name,
                        bounds,
                        span: self.span_from(&pstart),
                    });
                } else {
                    params.push(GenericParam {
                        name,
                        bounds,
                        is_const: false,
                        const_type: None,
                        is_variadic_shape: false,
                        span: self.span_from(&pstart),
                    });
                }
            }
            if !self.eat(&Token::Comma) {
                break;
            }
        }
        self.expect(&Token::RightBracket)?;

        Some(GenericParams {
            params,
            effect_params,
            span: self.span_from(&start),
        })
    }

    pub(crate) fn parse_trait_bound(&mut self) -> Option<TraitBound> {
        let start = self.current_span();
        let path = self.parse_path_segments()?;
        let generic_args = if self.check(&Token::LeftBracket) {
            Some(self.parse_generic_type_args()?)
        } else {
            None
        };
        Some(TraitBound {
            path,
            generic_args,
            span: self.span_from(&start),
        })
    }

    pub(crate) fn parse_optional_where_clause(&mut self) -> Option<WhereClause> {
        if !self.eat(&Token::Where) {
            return None;
        }
        let start = self.current_span();
        let mut constraints = Vec::new();

        loop {
            // Stop at body opener or end-of-item markers
            if self.check(&Token::LeftBrace) || self.check(&Token::Semicolon) || self.is_at_end() {
                break;
            }
            let cstart = self.current_span();
            let saved = self.pos;
            let type_name = match self.expect_identifier() {
                Some(name) => name,
                None => break,
            };

            // Check for projection-based constraints: `T.Assoc[...]` either
            // followed by `=` (associated type equality) or `:` (projection
            // bound, GAT slice 8a — `F.Mapped[i64]: FromIterator[i64]`).
            if self.eat(&Token::Dot) {
                let assoc_name = self.expect_identifier()?;
                let projection_generic_args = if self.check(&Token::LeftBracket) {
                    Some(self.parse_generic_type_args()?)
                } else {
                    None
                };
                if self.eat(&Token::Equal) {
                    if projection_generic_args.is_some() {
                        // `T.Assoc[args] = Type` is not a supported surface
                        // (associated-type-equality binds non-generic
                        // assocs); diagnose and skip.
                        self.error(
                            "associated-type equality (`T.Assoc = Type`) does not \
                             accept generic args on the LHS; use a projection bound \
                             (`T.Assoc[...]: Trait`) instead",
                        );
                        return None;
                    }
                    let ty = self.parse_type()?;
                    constraints.push(WhereConstraint::AssocTypeEq {
                        type_name,
                        assoc_name,
                        ty,
                        span: self.span_from(&cstart),
                    });
                } else if self.eat(&Token::Colon) {
                    // `T.Assoc[...]: Trait + ...` — projection bound.
                    let projection = TypeExpr {
                        kind: TypeKind::Path(PathExpr {
                            segments: vec![type_name, assoc_name],
                            generic_args: projection_generic_args,
                            span: self.span_from(&cstart),
                        }),
                        span: self.span_from(&cstart),
                    };
                    let mut bounds = Vec::new();
                    while let Some(bound) = self.parse_trait_bound() {
                        bounds.push(bound);
                        if !self.eat(&Token::Plus) {
                            break;
                        }
                    }
                    constraints.push(WhereConstraint::ProjectionBound {
                        projection,
                        bounds,
                        span: self.span_from(&cstart),
                    });
                } else {
                    self.error(
                        "expected `=` or `:` after associated-type projection \
                         in where clause",
                    );
                    return None;
                }
            } else if self.eat(&Token::Colon) {
                let mut bounds = Vec::new();
                while let Some(bound) = self.parse_trait_bound() {
                    bounds.push(bound);
                    if !self.eat(&Token::Plus) {
                        break;
                    }
                }
                constraints.push(WhereConstraint::TypeBound {
                    type_name,
                    bounds,
                    span: self.span_from(&cstart),
                });
            } else {
                // Const-predicate fall-through: backtrack to the saved
                // position and parse the constraint as a const expression
                // (e.g. `where N >= 0`, `where M < 4096`). Slice 1 parses;
                // slice 2's evaluator + slice 3's discharge engine consume.
                self.pos = saved;
                let expr = self.parse_expression()?;
                constraints.push(WhereConstraint::ConstPredicate {
                    expr,
                    span: self.span_from(&cstart),
                });
            }

            if !self.eat(&Token::Comma) {
                break;
            }
        }

        Some(WhereClause {
            constraints,
            span: self.span_from(&start),
        })
    }

    // ── Contracts ─────────────────────────────────────────────────

    pub(crate) fn parse_requires_clauses(&mut self) -> Vec<Expr> {
        let mut clauses = Vec::new();
        while self.eat(&Token::Requires) {
            if let Some(expr) = self.parse_expression() {
                clauses.push(expr);
            }
        }
        clauses
    }

    pub(crate) fn parse_ensures_clauses(&mut self) -> Vec<EnsuresClause> {
        let mut clauses = Vec::new();
        while self.eat(&Token::Ensures) {
            let start = self.current_span();
            // Result-binding syntax. Two accepted forms:
            //   `ensures(result) <postcond>` — the design syntax (design.md
            //     § Contracts). Recognized by `( IDENT )` immediately after
            //     `ensures` that is NOT followed by `{` (which would mean the
            //     parens are just grouping a bare-bool predicate before the
            //     function body block).
            //   `ensures |result| <postcond>` — closure-style pipes.
            let param = if self.peek_token() == Token::LeftParen
                && matches!(self.peek_token_at(1), Token::Identifier { .. })
                && self.peek_token_at(2) == Token::RightParen
                && self.peek_token_at(3) != Token::LeftBrace
            {
                self.eat(&Token::LeftParen);
                let name = self.expect_identifier();
                self.expect(&Token::RightParen);
                name
            } else if self.eat(&Token::Pipe) {
                let name = self.expect_identifier();
                self.expect(&Token::Pipe);
                name
            } else {
                None
            };
            if let Some(body) = self.parse_expression() {
                clauses.push(EnsuresClause {
                    param,
                    body,
                    span: self.span_from(&start),
                });
            }
        }
        clauses
    }

    pub(crate) fn parse_struct_body(&mut self) -> Option<(Vec<StructField>, Vec<Expr>, Vec<Expr>)> {
        let mut fields = Vec::new();
        let mut invariants = Vec::new();
        let mut impl_invariants = Vec::new();

        while !self.check(&Token::RightBrace) && !self.is_at_end() {
            self.collect_leading_doc_comments();
            // `impl invariant <expr>` — checked at *every* method exit (pub
            // and private), unlike the plain `invariant` form below
            // (design.md § Contracts — `impl invariant`).
            if self.check(&Token::Impl) && self.peek_token_at(1) == Token::Invariant {
                self.advance(); // impl
                self.advance(); // invariant
                let _ = self.take_pending_doc();
                if let Some(expr) = self.parse_expression() {
                    impl_invariants.push(expr);
                }
                continue;
            }
            // Check for invariant
            if self.eat(&Token::Invariant) {
                // Doc comments don't attach to invariants — drop any
                // accumulated text so it doesn't bleed into the next field.
                let _ = self.take_pending_doc();
                if let Some(expr) = self.parse_expression() {
                    invariants.push(expr);
                }
                // Optional trailing comma/semicolon after invariant — not required
                continue;
            }

            // Otherwise, parse a struct field
            let start = self.current_span();
            let attributes = self.parse_attributes();
            let is_pub = self.eat(&Token::Pub);
            let mut_keyword_span = if self.check(&Token::Mut) {
                let s = self.current_span();
                self.advance();
                Some(s)
            } else {
                None
            };
            let is_mut = mut_keyword_span.is_some();
            // Field-modifier `weak` per design.md § Shared Types — Weak
            // references. `mut weak field: T` and `weak field: T` are
            // both legal; the modifier wraps the parsed field type in
            // `TypeKind::Weak`. This is sugar — the type-position form
            // `field: weak T` is also accepted via `parse_type`.
            let weak_modifier_span = if matches!(self.peek_token(), Token::Weak) {
                let span = self.current_span();
                self.advance();
                Some(span)
            } else {
                None
            };
            let name_token_span = self.current_span();
            let name = self.expect_identifier()?;
            let name_span_from_start = self.span_from(&start);
            self.check_ident_class(
                &name,
                IdentClass::Value,
                "struct field",
                name_span_from_start,
            );
            self.expect(&Token::Colon)?;
            let inner_ty = self.parse_type()?;
            let ty = if let Some(span) = weak_modifier_span {
                TypeExpr {
                    kind: TypeKind::Weak(Box::new(inner_ty)),
                    span,
                }
            } else {
                inner_ty
            };
            let doc_comment = self.take_pending_doc();
            fields.push(StructField {
                span: self.span_from(&start),
                attributes,
                doc_comment,
                is_pub,
                is_mut,
                mut_keyword_span,
                name,
                name_span: name_token_span,
                ty,
            });
            if !self.eat(&Token::Comma) {
                break;
            }
        }

        Some((fields, invariants, impl_invariants))
    }

    pub(crate) fn parse_generic_type_args(&mut self) -> Option<Vec<GenericArg>> {
        self.expect(&Token::LeftBracket)?;
        let mut args = Vec::new();
        loop {
            if self.check(&Token::RightBracket) {
                break;
            }
            // Shape literal args: a `[` starting a generic argument is a
            // SHAPE_LIT (`Tensor[f64, [3, 4, ?]]`) — no other generic-arg
            // form begins with `[` (syntax.md § GENERIC_ARG). Dims are
            // const expressions, `?` dynamic-dim markers, or `...IDENT`
            // variadic splices; shape literals do not nest.
            if self.check(&Token::LeftBracket) {
                let lit = self.parse_shape_literal()?;
                args.push(GenericArg::Shape(lit));
                if !self.eat(&Token::Comma) {
                    break;
                }
                continue;
            }
            // Const expression args: integer literals, negative integers, bool literals,
            // character literals, and `Identifier OP ...` shapes (e.g. `Array[T, N + 1]`)
            // where the operator following the identifier disambiguates a const-arg
            // expression from a type-arg. Plain `Identifier` (no trailing op)
            // continues to parse as a type — `Vec[Map[K, V]]` and similar
            // type-position shapes are preserved.
            let is_const_arg_expr = match self.peek_token() {
                Token::Integer(_, _)
                | Token::True
                | Token::False
                | Token::Minus
                | Token::CharLiteral(_) => true,
                Token::Identifier { .. } => {
                    let next = self.tokens.get(self.pos + 1).map(|t| &t.token);
                    matches!(
                        next,
                        Some(
                            Token::Plus
                                | Token::Minus
                                | Token::Star
                                | Token::Slash
                                | Token::Percent
                                | Token::Caret
                                | Token::Amp
                                | Token::Pipe
                                | Token::LessLess
                                | Token::GreaterGreater
                                | Token::EqualEqual
                                | Token::BangEqual
                                | Token::LessThanOrEqual
                                | Token::GreaterThanOrEqual
                        )
                    )
                }
                _ => false,
            };
            if is_const_arg_expr {
                let expr = self.parse_expression()?;
                args.push(GenericArg::Const(expr));
            } else {
                // `impl Trait` slice 1: nested generic-arg positions
                // (e.g., `Vec[impl T]`) are rejected at v1 per
                // design.md § `impl Trait` — push a
                // `NestedGenericArg` block reason for the duration
                // of this argument's `parse_type` call so a top-of-
                // generic-args `impl Trait` produces
                // `E_IMPL_TRAIT_IN_NESTED_POSITION` (see the
                // matching arm in `parse_type`).
                self.push_impl_trait_block(crate::parser::ImplTraitBlockReason::NestedGenericArg);
                let ty = self.parse_type();
                self.pop_impl_trait_block();
                args.push(GenericArg::Type(ty?));
            }
            if !self.eat(&Token::Comma) {
                break;
            }
        }
        self.expect(&Token::RightBracket)?;
        Some(args)
    }

    /// Parse a shape literal in generic-argument position:
    /// `"[" SHAPE_ELEM { "," SHAPE_ELEM } "]"` where a SHAPE_ELEM is a
    /// const expression (`3`, `N`), a `?` dynamic-dim marker, or a
    /// `...IDENT` variadic splice (syntax.md § SHAPE_LIT). Caller has
    /// checked (not consumed) the leading `[`. Shape literals require at
    /// least one dim and never nest.
    pub(crate) fn parse_shape_literal(&mut self) -> Option<ShapeLit> {
        let start = self.current_span();
        self.expect(&Token::LeftBracket)?;
        let mut dims = Vec::new();
        loop {
            if self.check(&Token::RightBracket) {
                break;
            }
            match self.peek_token() {
                // `?` — dynamic dim. Inside a shape literal the token is a
                // dim marker, not the expression-level try operator
                // (context disambiguates; syntax.md §5.21 is unaffected).
                Token::Question => {
                    let span = self.current_span();
                    self.advance();
                    dims.push(ShapeDim::Dynamic { span });
                }
                // `...IDENT` — variadic shape splice.
                Token::DotDotDot => {
                    let splice_start = self.current_span();
                    self.advance();
                    match self.peek_token() {
                        Token::Identifier { name, .. } => {
                            let name = name.clone();
                            self.advance();
                            dims.push(ShapeDim::Splice {
                                name,
                                span: self.span_from(&splice_start),
                            });
                        }
                        _ => {
                            self.error(
                                "expected identifier after `...` in shape literal — a variadic \
                                 splice names a Shape-kinded parameter, e.g. `[...S, M]`",
                            );
                            return None;
                        }
                    }
                }
                // Nested shape literal — explicitly rejected per grammar.
                Token::LeftBracket => {
                    self.error(
                        "shape literals do not nest — a dim is a const expression, `?`, or a \
                         `...IDENT` splice, never another shape literal",
                    );
                    return None;
                }
                // Const-expression dim: integer literal, Dim-kinded param
                // name, module-level constant, or (parsed-but-deferred)
                // arithmetic like `N + 1`.
                _ => {
                    let expr = self.parse_expression()?;
                    dims.push(ShapeDim::Const(Box::new(expr)));
                }
            }
            if !self.eat(&Token::Comma) {
                break;
            }
        }
        if dims.is_empty() {
            self.error(
                "shape literal requires at least one dimension — `[]` is not a valid shape \
                 (a rank-0 scalar is just `T`)",
            );
            return None;
        }
        self.expect(&Token::RightBracket)?;
        Some(ShapeLit {
            dims,
            span: self.span_from(&start),
        })
    }
}
