//! Trait and impl-block parsing — marker traits, regular trait
//! definitions, trait aliases, trait methods, impl blocks, and
//! associated-type declarations / bindings.

use crate::ast::*;
use crate::lexer::IdentClass;
use crate::token::{Span, Token};

use super::{FnContext, ParseError, ParseErrorKind};

impl super::Parser {
    // ── Traits ───────────────────────────────────────────────────

    /// Parse `marker trait NAME[GENERICS] [: SUPERTRAITS] [where ...]
    /// (";" | "{" "}")` per syntax.md §3.4 / design.md § Marker Traits.
    /// The body must be empty — methods, associated types, and
    /// associated consts inside the body are rejected with a focused
    /// diagnostic. `body_brace` records whether the user wrote `{ }`
    /// (so the formatter can round-trip) or the canonical `;`.
    pub(super) fn parse_marker_trait(
        &mut self,
        attributes: Vec<Attribute>,
        is_pub: bool,
        is_private: bool,
    ) -> Option<MarkerTraitDef> {
        let start = self.current_span();
        self.expect_kw("marker")?;
        self.expect(&Token::Trait)?;
        let name = self.expect_identifier()?;
        let name_span = self.span_from(&start);
        self.check_ident_class(&name, IdentClass::Type, "marker trait", name_span);
        let doc_comment = self.take_pending_doc();
        let generic_params = self.parse_optional_generic_params();

        let mut supertraits = Vec::new();
        if self.eat(&Token::Colon) {
            loop {
                let bound = self.parse_trait_bound()?;
                supertraits.push(bound);
                if !self.eat(&Token::Plus) {
                    break;
                }
            }
        }

        let where_clause = self.parse_optional_where_clause();

        let body_brace = if self.eat(&Token::LeftBrace) {
            // Empty-brace form `marker trait Foo { }`. Any item inside is
            // rejected with a focused diagnostic; we recover by skipping
            // to the closing brace so the rest of the file parses.
            while !self.check(&Token::RightBrace) && !self.is_at_end() {
                let body_span = self.current_span();
                let (code, msg) = if self.check(&Token::Fn) {
                    (
                        "E_MARKER_TRAIT_HAS_METHOD",
                        "marker traits cannot declare methods; \
                         remove the method or change `marker trait` to `trait`",
                    )
                } else if self.check(&Token::Type) || self.check(&Token::Const) {
                    (
                        "E_MARKER_TRAIT_HAS_ITEM",
                        "marker traits cannot declare associated types or consts; \
                         remove the item or change `marker trait` to `trait`",
                    )
                } else {
                    (
                        "E_MARKER_TRAIT_HAS_ITEM",
                        "marker traits cannot have a body; \
                         remove the item or change `marker trait` to `trait`",
                    )
                };
                self.errors.push(ParseError {
                    kind: crate::parser::ParseErrorKind::Syntax,
                    message: format!("error[{code}]: {msg}"),
                    span: body_span,
                });
                // Skip the offending item whole, so it is reported once: up
                // to its `;`, or through its `{ .. }` body, stopping at the
                // trait's own `}`. Stepping one token at a time reported a
                // "cannot have a body" error at every token of the item.
                let mut depth = 0usize;
                while !self.is_at_end() {
                    if depth == 0 && self.check(&Token::RightBrace) {
                        break;
                    }
                    if depth == 0 && self.check(&Token::Semicolon) {
                        self.advance();
                        break;
                    }
                    if self.check(&Token::LeftBrace) || self.check(&Token::LeftParen) {
                        depth += 1;
                    } else if self.check(&Token::RightBrace) || self.check(&Token::RightParen) {
                        depth = depth.saturating_sub(1);
                        if depth == 0 && self.check(&Token::RightBrace) {
                            self.advance();
                            break;
                        }
                    }
                    self.advance();
                }
            }
            self.expect(&Token::RightBrace)?;
            true
        } else {
            self.expect(&Token::Semicolon)?;
            false
        };

        let deprecation = self.scan_deprecated_attr(&attributes);
        let unstable = self.scan_unstable_attr(&attributes);
        let lint_overrides = self.scan_lint_level_attrs(&attributes);
        Some(MarkerTraitDef {
            span: self.span_from(&start),
            attributes,
            doc_comment,
            is_pub,
            is_private,
            name,
            generic_params,
            supertraits,
            where_clause,
            body_brace,
            deprecation,
            unstable,
            lint_overrides,
        })
    }

    /// Top-level dispatch for the `trait` keyword. Reads the trait header
    /// (name + optional generic params), then peeks the next token: `=`
    /// enters the trait-alias path (`trait NAME = bounds;` per v60 item
    /// 40 / design.md § Trait Aliases); anything else falls through to
    /// the regular trait-def path.
    pub(super) fn parse_trait_or_alias(
        &mut self,
        attributes: Vec<Attribute>,
        is_pub: bool,
        is_private: bool,
    ) -> Option<Item> {
        let start = self.current_span();
        self.expect(&Token::Trait)?;
        let name = self.expect_identifier()?;
        let name_span = self.span_from(&start);
        self.check_ident_class(&name, IdentClass::Type, "trait", name_span);
        let doc_comment = self.take_pending_doc();
        self.generic_defaults_ok = true;
        let generic_params = self.parse_optional_generic_params();
        self.generic_defaults_ok = false;

        if self.check(&Token::Equal) {
            return self
                .parse_trait_alias_tail(
                    attributes,
                    is_pub,
                    is_private,
                    name,
                    generic_params,
                    doc_comment,
                    &start,
                )
                .map(Item::TraitAlias);
        }

        // A trait takes no effect variable in v1, but its `[with E]` still
        // scopes over the methods, so the resolver reports the declaration
        // once rather than each `with E` as an undefined name.
        let depth = self.effect_var_stack.len();
        self.push_effect_scope(generic_params.as_ref());
        let def = self.parse_trait_def_tail(
            attributes,
            is_pub,
            is_private,
            name,
            generic_params,
            doc_comment,
            &start,
        );
        self.effect_var_stack.truncate(depth);
        def.map(Item::TraitDef)
    }

    #[allow(clippy::too_many_arguments)]
    fn parse_trait_def_tail(
        &mut self,
        attributes: Vec<Attribute>,
        is_pub: bool,
        is_private: bool,
        name: String,
        generic_params: Option<GenericParams>,
        doc_comment: Option<String>,
        start: &Span,
    ) -> Option<TraitDef> {
        // Optional supertrait list: `trait Foo: Bar + Baz`
        let mut supertraits = Vec::new();
        if self.eat(&Token::Colon) {
            loop {
                let bound = self.parse_trait_bound()?;
                supertraits.push(bound);
                if !self.eat(&Token::Plus) {
                    break;
                }
            }
        }

        // Optional trait-level effect ceiling: `trait Foo with reads(R)`
        let trait_effects = self.parse_optional_effect_list(&[]);

        let where_clause = self.parse_optional_where_clause();

        self.expect(&Token::LeftBrace)?;
        let mut items = Vec::new();
        while !self.check(&Token::RightBrace) && !self.is_at_end() {
            // Collect leading doc comments + attributes for the trait
            // item. The Type-decl branch ignores attributes for now
            // (associated-type attributes aren't part of any current
            // surface); the method branch threads them into
            // `parse_trait_method`.
            self.collect_leading_doc_comments();
            let item_attributes = self.parse_attributes();
            if self.check(&Token::Type) {
                if !item_attributes.is_empty() {
                    // Per design.md, attribute targets on associated
                    // type declarations are not part of the v1 surface
                    // — silently dropping the attributes would mask
                    // user intent. Emit a focused diagnostic; the
                    // parsed item still attaches without the attrs.
                    self.errors.push(ParseError {
                        kind: crate::parser::ParseErrorKind::Syntax,
                        message: "error[E_ATTR_ON_ASSOC_TYPE_DECL]: \
                                  attributes are not supported on \
                                  associated-type declarations in 0.8; \
                                  remove the attribute"
                            .to_string(),
                        span: item_attributes[0].span,
                    });
                }
                let item = self.parse_assoc_type_decl()?;
                items.push(TraitItem::AssocType(Box::new(item)));
            } else {
                // `unsafe fn` in a trait body mirrors the module-scope
                // dispatch in `parse_item`: consume an optional `unsafe`
                // before `fn` and thread it into `parse_trait_method`.
                // `unsafe` followed by anything other than `fn` here is
                // rejected with the same focused diagnostic.
                let is_unsafe = if self.check(&Token::Unsafe) {
                    if self.peek_token_ref_at(1) == &Token::Fn {
                        self.advance(); // consume `unsafe`
                        true
                    } else {
                        self.error(
                            "expected `fn` after `unsafe` in trait body — `unsafe` \
                             may only prefix an `unsafe fn` method declaration here.",
                        );
                        self.advance(); // consume `unsafe` for recovery
                        false
                    }
                } else {
                    false
                };
                let method = self.parse_trait_method(item_attributes, is_unsafe)?;
                items.push(TraitItem::Method(Box::new(method)));
            }
        }
        self.expect(&Token::RightBrace)?;

        let deprecation = self.scan_deprecated_attr(&attributes);
        let unstable = self.scan_unstable_attr(&attributes);
        let lint_overrides = self.scan_lint_level_attrs(&attributes);
        let on_unimplemented = self.scan_on_unimplemented_attr(&attributes);
        Some(TraitDef {
            span: self.span_from(start),
            attributes,
            doc_comment,
            is_pub,
            is_private,
            name,
            generic_params,
            supertraits,
            trait_effects,
            where_clause,
            items,
            stdlib_origin: false,
            deprecation,
            unstable,
            lint_overrides,
            on_unimplemented,
        })
    }

    /// Parse the tail of `trait NAME[GENERICS] = bound1 + bound2 + ...
    /// [where ...];` after the header (`trait`, name, generics) has been
    /// consumed. Per syntax.md §3.4 `TRAIT_ALIAS_DEF`. An empty bound
    /// list is a parse error; effect-predicate keywords (`reads`,
    /// `panics`, ...) cannot reach the bound parser, so the
    /// `E_EFFECT_IN_TRAIT_ALIAS` diagnostic from design.md is
    /// structurally unreachable here.
    #[allow(clippy::too_many_arguments)]
    fn parse_trait_alias_tail(
        &mut self,
        attributes: Vec<Attribute>,
        is_pub: bool,
        is_private: bool,
        name: String,
        generic_params: Option<GenericParams>,
        doc_comment: Option<String>,
        start: &Span,
    ) -> Option<TraitAliasDef> {
        self.expect(&Token::Equal)?;

        // `trait Foo = ;` — empty bound list rejected at parse with a
        // focused diagnostic.
        if self.check(&Token::Semicolon) || self.at_kw("where") {
            self.error(
                "trait alias requires at least one trait bound on the right-hand side; \
                 write `trait Foo = SomeTrait;` instead of `trait Foo = ;`",
            );
            // Recover by consuming the rest of the form.
            self.parse_optional_where_clause();
            self.eat(&Token::Semicolon);
            return None;
        }

        // Parse the `+`-separated trait bound list. Effect predicates
        // (`reads`, `writes`, `panics`, ...) are keyword tokens at the
        // lexer level and cannot syntactically appear in trait-bound
        // position, so the design.md `E_EFFECT_IN_TRAIT_ALIAS` diagnostic
        // is structurally unreachable here — `parse_trait_bound` would
        // fail to match the keyword token before the alias parser saw
        // it. Effect-group references for the alias body land via the
        // P1 expansion work alongside the broader trait-alias surface.
        let mut bounds = Vec::new();
        loop {
            let bound = self.parse_trait_bound()?;
            bounds.push(bound);
            if !self.eat(&Token::Plus) {
                break;
            }
        }

        let where_clause = self.parse_optional_where_clause();
        self.expect(&Token::Semicolon)?;

        let deprecation = self.scan_deprecated_attr(&attributes);
        let unstable = self.scan_unstable_attr(&attributes);
        let lint_overrides = self.scan_lint_level_attrs(&attributes);
        Some(TraitAliasDef {
            span: self.span_from(start),
            attributes,
            doc_comment,
            is_pub,
            is_private,
            name,
            generic_params,
            bounds,
            where_clause,
            deprecation,
            unstable,
            lint_overrides,
        })
    }

    fn parse_assoc_type_decl(&mut self) -> Option<AssocTypeDecl> {
        let start = self.current_span();
        self.expect(&Token::Type)?;
        let name = self.expect_identifier()?;
        let name_span = self.span_from(&start);
        self.check_ident_class(&name, IdentClass::Type, "associated type", name_span);
        // GAT-shaped declaration `type Name[P1, P2, ...]` — optional
        // generic parameter list. Per design.md § Generic associated
        // types, effect-polymorphic GATs (`type Mapped[U, with E]`) are
        // out of v1 scope; the carrying method takes the `with E`
        // parameter instead. Reject post-parse so the parameter list
        // itself parses (preserving the rest of the trait body).
        let generic_params = self.parse_optional_generic_params();
        self.reject_gat_effect_params(generic_params.as_ref(), "declaration");
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
        let where_clause = self.parse_optional_where_clause();
        self.expect(&Token::Semicolon)?;
        Some(AssocTypeDecl {
            span: self.span_from(&start),
            name,
            generic_params,
            bounds,
            where_clause,
        })
    }

    /// Reject effect-parameter clauses on a GAT's generic-parameter
    /// list. The carrying *method* takes the `with E` parameter, not
    /// the associated type itself — see design.md § GATs "Out of v1
    /// scope" bullet. Emits one diagnostic per offending effect
    /// parameter, anchored at the generic-params span (the parser
    /// doesn't retain per-effect-param spans). Suggestion text steers
    /// the author at the carrying-method form.
    fn reject_gat_effect_params(&mut self, gp: Option<&GenericParams>, role: &str) {
        let Some(gp) = gp else { return };
        if gp.effect_params.is_empty() {
            return;
        }
        let joined = gp
            .effect_params
            .iter()
            .map(|ep| ep.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        self.errors.push(ParseError {
            kind: crate::parser::ParseErrorKind::Syntax,
            message: format!(
                "error[E_GAT_EFFECT_PARAM]: effect parameter `with {joined}` is not \
                 permitted on a generic associated type {role}; GATs are over types \
                 only — move the `with {joined}` clause to the carrying method's \
                 generic-parameter list instead",
            ),
            span: gp.span,
        });
    }

    fn parse_trait_method(
        &mut self,
        attributes: Vec<Attribute>,
        is_unsafe: bool,
    ) -> Option<TraitMethod> {
        let start = self.current_span();
        // Take the item-level doc *before* descending into the
        // signature so per-param doc collection doesn't overwrite it.
        let doc_comment = self.take_pending_doc();
        self.expect(&Token::Fn)?;
        let name = self.expect_method_name()?;
        let name_span = self.span_from(&start);
        self.check_ident_class(&name, IdentClass::Value, "fn", name_span);
        let generic_params = self.parse_optional_generic_params();
        let effect_vars = self.push_effect_scope(generic_params.as_ref());

        self.expect(&Token::LeftParen)?;
        self.fn_context_stack.push(FnContext::TraitMethod);
        // `impl Trait` slice 1: argument-position `impl Trait` is
        // rejected in trait method declarations (use the explicit
        // `[T: Trait]` form there instead — design.md § `impl Trait`).
        // Push the block reason for the parameter-list parse so a
        // `fn m(x: impl T)` inside `trait { ... }` produces
        // `E_IMPL_TRAIT_IN_TRAIT_METHOD_ARG`. Return-type position is
        // parsed *after* the matching pop below, so RPITIT
        // (`fn m() -> impl T`) keeps working.
        self.push_impl_trait_block(crate::parser::ImplTraitBlockReason::TraitMethodArg);
        self.own_self_consumed = false;
        let m0 = self.pattern_muts.len();
        let (self_param, self_span, params) = self.parse_fn_params()?;
        // `(mut a, b): (i64, i64)` (B-2026-11-04-76): fixed in the body.
        let muts = self.take_pattern_muts(m0);
        let self_is_own = std::mem::take(&mut self.own_self_consumed);
        self.pop_impl_trait_block();
        self.fn_context_stack.pop();
        self.expect(&Token::RightParen)?;

        let return_type = if self.eat(&Token::Arrow) {
            Some(self.parse_return_type()?)
        } else {
            None
        };

        let effects = self.parse_optional_effect_list(&effect_vars);
        let requires = self.parse_requires_clauses();
        let ensures = self.parse_ensures_clauses();
        let where_clause = self.parse_optional_where_clause();

        // Default method body or required method (semicolon)
        let body = if self.peek_token_ref() == &Token::LeftBrace {
            let block = self.parse_block()?;
            self.fix_pattern_muts(muts, block.span, true);
            Some(block)
        } else {
            self.expect(&Token::Semicolon)?;
            None
        };
        self.effect_var_stack.pop();

        // Capture `#[track_caller]` and `#[deprecated]` from the
        // attributes attached at the dispatcher level — the same
        // helpers that `parse_function` uses. The track_caller
        // helper additionally emits `E_TRACK_CALLER_ARGS_NOT_PERMITTED`
        // for malformed args; the deprecated helper emits the four
        // `E_DEPRECATED_*` diagnostics for malformed forms.
        let is_track_caller = self.scan_track_caller_attr(&attributes);
        let is_gpu = self.scan_gpu_attr(&attributes);
        let (inline_hint, is_cold) = self.scan_codegen_hint_attrs(&attributes);
        let deprecation = self.scan_deprecated_attr(&attributes);
        let unstable = self.scan_unstable_attr(&attributes);

        Some(TraitMethod {
            span: self.span_from(&start),
            attributes,
            doc_comment,
            is_unsafe,
            name,
            generic_params,
            self_param,
            self_is_own,
            self_span,
            params,
            return_type,
            effects,
            requires,
            ensures,
            where_clause,
            body,
            deprecation,
            unstable,
            is_track_caller,
            is_gpu,
            inline_hint,
            is_cold,
        })
    }

    // ── Impl Blocks ──────────────────────────────────────────────

    /// Consume and reject an effect clause written on an impl HEADER.
    ///
    /// Recognized rather than merely refused: the clause is parsed with the
    /// real effect-list grammar (so `with panics`, `with writes(Log)` and
    /// `with E` all land here identically), then reported as
    /// [`ParseErrorKind::ReservedSyntax`] — design.md reserves the position
    /// for possible future relaxation, so tooling should read this as "not
    /// yet", not "nonsense".
    ///
    /// The edit deletes the clause because a header clause has no meaning to
    /// preserve: the effects belong on the impl's methods, which is where the
    /// spec's own example already carries them.
    fn reject_impl_header_effect_clause(&mut self, effect_vars: &[String], is_trait_impl: bool) {
        let with_idx = self.pos;
        let span = self.current_span();
        // Parse for RECOVERY, not for a value — a malformed clause has already
        // reported its own errors and `self.pos` still advanced past it.
        let _ = self.parse_effect_list(effect_vars, true);
        self.record_token_range_deletion(with_idx, self.pos, &span);
        // An INHERENT impl has no trait to carry `with _`, so naming one would
        // send the reader looking for something that does not exist.
        let remedy = if is_trait_impl {
            "declare the trait method `with _` and put the effects on the impl's \
             own methods instead"
        } else {
            "put the effects on the impl's own methods instead"
        };
        self.error_at_kind(
            ParseErrorKind::ReservedSyntax,
            &format!("an effect clause on an impl header is not valid syntax — {remedy}"),
            span,
        );
    }

    /// An `impl[with E]`'s effect variables are in scope in its header and
    /// its methods (design.md § Effect variables); the scope closes however
    /// the block's parse ends.
    pub(super) fn parse_impl_block(&mut self, attributes: Vec<Attribute>) -> Option<ImplBlock> {
        let depth = self.effect_var_stack.len();
        let saved_self = self.impl_self_name.take();
        let block = self.parse_impl_block_scoped(attributes);
        self.impl_self_name = saved_self;
        self.effect_var_stack.truncate(depth);
        block
    }

    fn parse_impl_block_scoped(&mut self, attributes: Vec<Attribute>) -> Option<ImplBlock> {
        // A `///` run before `impl` documents nothing (design.md §3); left
        // pending, it used to attach to the block's first undocumented method.
        self.discard_pending_doc(super::DOC_IMPL);
        let start = self.current_span();
        self.expect(&Token::Impl)?;

        let generic_params = self.parse_optional_generic_params();
        self.push_effect_scope(generic_params.as_ref());

        // `impl fmt::Display for T`: Rust's formatting trait, whose `fmt`
        // method is rewritten below (B-2026-11-15-71).
        let rust_fmt = self.rust_fmt_trait_header();
        // Parse the type or trait name
        let first_type = self.parse_type()?;

        // Check if this is `impl Trait for Type`
        let (trait_name, target_type) = if self.eat(&Token::For) {
            let path = match &first_type.kind {
                TypeKind::Path(p) => p.clone(),
                _ => {
                    self.error("Expected trait name before 'for'");
                    return None;
                }
            };
            let target = self.parse_type()?;
            (Some(path), target)
        } else {
            (None, first_type)
        };
        if let TypeKind::Path(p) = &target_type.kind {
            self.impl_self_name = p.segments.last().cloned();
        }

        let where_clause = self.parse_optional_where_clause();
        // `impl T for U with <effects> { … }` — recognized, then rejected.
        // design.md § From/Into settles this in the negative ("No impl-level
        // effect variables in v1"): effect polymorphism on a trait method is
        // spelled `with _` on the TRAIT, and the impl block carries nothing.
        // The resolver already says exactly that for the effect-VARIABLE
        // spelling (`impl[T, with E] …`); this is the concrete-clause spelling
        // of the same mistake, and it used to fall through to the
        // `expect(LeftBrace)` below as a bare "Expected LeftBrace, found With"
        // — plus a CASCADED "Expected expression, found RightBrace" once the
        // block's tail desynchronized. Consuming the clause is what removes
        // the cascade.
        if self.at_kw("with") {
            let effect_vars: Vec<String> = generic_params
                .as_ref()
                .map(|g| g.effect_params.iter().map(|ep| ep.name.clone()).collect())
                .unwrap_or_default();
            self.reject_impl_header_effect_clause(&effect_vars, trait_name.is_some());
        }

        self.expect(&Token::LeftBrace)?;
        let mut items = Vec::new();
        while !self.check(&Token::RightBrace) && !self.is_at_end() {
            // Collect leading `///` doc comments for the impl item before its
            // attributes — mirrors `parse_item` and the trait-block loop.
            // `parse_function` drains the pending doc via `take_pending_doc`
            // onto `Function.doc_comment`; without this, a doc comment before
            // an impl method was a hard parse error ("Expected Fn, found
            // DocComment") and impl methods (unlike trait methods) could not
            // be documented at all.
            self.collect_leading_doc_comments();
            let attrs = self.parse_attributes();
            if self.check(&Token::Type) {
                let binding = self.parse_assoc_type_binding()?;
                items.push(ImplItem::AssocType(Box::new(binding)));
            } else {
                let is_pub = self.eat_pub();
                let is_private = if !is_pub {
                    self.eat(&Token::Private)
                } else {
                    false
                };
                // `comptime fn` inside an impl block mirrors the module-scope
                // dispatch in `parse_item`: consume an optional `comptime`
                // (outermost, before any `unsafe`) and thread it into
                // `parse_function`. `comptime` not followed by `fn` /
                // `unsafe fn` is rejected with a focused diagnostic.
                let is_comptime = if self.check(&Token::Comptime) {
                    let nxt = self.peek_token_at(1);
                    if nxt == Token::Fn
                        || (nxt == Token::Unsafe && self.peek_token_ref_at(2) == &Token::Fn)
                    {
                        self.advance(); // consume `comptime`
                        true
                    } else {
                        self.error(
                            "expected `fn` after `comptime` in impl block — `comptime` \
                             may only prefix a `comptime fn` method declaration here.",
                        );
                        self.advance(); // consume `comptime` for recovery
                        false
                    }
                } else {
                    false
                };
                // `unsafe fn` inside an impl block mirrors the module-scope
                // dispatch in `parse_item`: consume an optional `unsafe`
                // before `fn` and thread it into `parse_function`. `unsafe`
                // followed by anything other than `fn` inside an impl body
                // is rejected with the same focused diagnostic. There is no
                // `unsafe impl` syntax at v1.
                let is_unsafe = if self.check(&Token::Unsafe) {
                    if self.peek_token_ref_at(1) == &Token::Fn {
                        self.advance(); // consume `unsafe`
                        true
                    } else {
                        self.error(
                            "expected `fn` after `unsafe` in impl block — `unsafe` \
                             may only prefix an `unsafe fn` method declaration here.",
                        );
                        self.advance(); // consume `unsafe` for recovery
                        false
                    }
                } else {
                    false
                };
                if let Some(kind) = rust_fmt {
                    if self.check(&Token::Fn)
                        && matches!(self.peek_token_ref_at(1), Token::Identifier { name, .. } if name == "fmt")
                        && self.peek_token_ref_at(2) == &Token::LeftParen
                        && self.rust_fmt_method(kind)
                    {
                        continue;
                    }
                }
                // A method or associated function may be named by any keyword.
                self.member_fn_name = true;
                let method =
                    self.parse_function(attrs, is_pub, is_private, is_unsafe, is_comptime)?;
                // `#[multiversion]` dispatch on an impl item is supported only on
                // a `self`-receiver method (the thunk forwards via
                // `self.variant(...)`); an associated function (no `self`) has no
                // receiver to forward and is out of scope at v1.1. The non-empty
                // feature-list shape was already checked in `parse_function`.
                if method.self_param.is_none() {
                    if let Some(mv) = method.attributes.iter().find(|a| a.is_bare("multiversion")) {
                        if !crate::ast::multiversion_feature_list(&method.attributes)
                            .unwrap_or_default()
                            .is_empty()
                        {
                            self.errors.push(super::ParseError {
                                kind: crate::parser::ParseErrorKind::Syntax,
                                message: "error[E_MULTIVERSION_ON_ASSOC]: `#[multiversion]` on an \
                                          impl item is supported only on a method with a `self` \
                                          receiver, not an associated function"
                                    .to_string(),
                                span: mv.span,
                            });
                        }
                    }
                }
                items.push(ImplItem::Method(Box::new(method)));
            }
        }
        self.expect(&Token::RightBrace)?;

        let lint_overrides = self.scan_lint_level_attrs(&attributes);
        let do_not_recommend = self.scan_do_not_recommend_attr(&attributes);
        Some(ImplBlock {
            span: self.span_from(&start),
            attributes,
            generic_params,
            trait_name,
            target_type,
            where_clause,
            items,
            lint_overrides,
            do_not_recommend,
        })
    }

    /// At an impl header: Rust's `fmt::Display` / `std::fmt::Debug` (or
    /// the bare name, after `use std::fmt::Display;`) before `for`. Kāra's
    /// `Display` and `Debug` are in the prelude, so a module path is
    /// reported, its fix deletes it, and its tokens go so the parse goes on
    /// as the fixed text would. Returns the trait, so the block's Rust
    /// `fmt` method can be rewritten to Kāra's.
    fn rust_fmt_trait_header(&mut self) -> Option<&'static str> {
        let at = self.pos;
        let ident = |i: usize| match self.tokens.get(i).map(|t| &t.token) {
            Some(Token::Identifier { name, .. }) => Some(name.clone()),
            _ => None,
        };
        let mut names = vec![ident(at)?];
        let mut last = at;
        while matches!(
            self.tokens.get(last + 1).map(|t| &t.token),
            Some(Token::ColonColon)
        ) {
            names.push(ident(last + 2)?);
            last += 2;
        }
        let kind = match names[names.len() - 1].as_str() {
            "Display" => "Display",
            "Debug" => "Debug",
            _ => return None,
        };
        let prefix: Vec<&str> = names[..names.len() - 1]
            .iter()
            .map(|s| s.as_str())
            .collect();
        if !matches!(
            prefix.as_slice(),
            [] | ["fmt"] | ["std", "fmt"] | ["core", "fmt"]
        ) || !matches!(
            self.tokens.get(last + 1).map(|t| &t.token),
            Some(Token::For)
        ) {
            return None;
        }
        if !prefix.is_empty() {
            let first = self.tokens[at].span;
            let end = self.tokens[last].span;
            let whole = Span {
                length: end.offset + end.length - first.offset,
                ..first
            };
            self.error_at(
                &format!(
                    "`{}` is Rust's formatting trait -- Kāra's `{kind}` is in the prelude: \
                     write `{kind}`",
                    names.join("::")
                ),
                whole,
            );
            self.fix_edits.insert(
                crate::resolver::SpanKey::from_span(&whole),
                crate::resolver::TextEdit {
                    offset: first.offset,
                    length: end.offset - first.offset,
                    replacement: String::new(),
                },
            );
            self.tokens.drain(at..last);
        }
        Some(kind)
    }

    /// At `fn fmt(` in an `impl Display` / `impl Debug` block: Rust's
    /// `fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result`. Kāra's
    /// `Display` is `fn to_string(self) -> String` and its `Debug` is
    /// `fn fmt_debug(self) -> String` (design.md § Display and Debug), so
    /// the signature is rewritten, and each `write!(f, ..)` whose result is
    /// the method's (no `?` after it) becomes the `format!(..)` the next
    /// pass turns into an f-string, and `f.write_str("..")` the string. A
    /// body that uses the formatter any other way is reported with no fix.
    /// The method's tokens are skipped either way: its Rust types
    /// (`fmt::Formatter<'_>`) would only cascade (B-2026-11-15-71).
    fn rust_fmt_method(&mut self, kind: &str) -> bool {
        let toks = &self.tokens;
        let matching = |open: usize| -> Option<usize> {
            let mut depth = 0usize;
            for (i, t) in toks.iter().enumerate().skip(open) {
                match t.token {
                    Token::LeftParen | Token::LeftBracket | Token::LeftBrace => depth += 1,
                    Token::RightParen | Token::RightBracket | Token::RightBrace => {
                        depth = depth.checked_sub(1)?;
                        if depth == 0 {
                            return Some(i);
                        }
                    }
                    _ => {}
                }
            }
            None
        };
        let name_at = self.pos + 1;
        let open = self.pos + 2;
        let Some(close) = matching(open) else {
            return false;
        };
        let Some(body_open) =
            (close + 1..toks.len()).find(|&i| matches!(toks[i].token, Token::LeftBrace))
        else {
            return false;
        };
        let Some(body_close) = matching(body_open) else {
            return false;
        };
        let tok = |i: usize| toks.get(i).map(|t| &t.token);
        let is_ident = |i: usize, want: &str| matches!(tok(i), Some(Token::Identifier { name, .. }) if name == want);
        // `&self, f: ..` (or `self, f: ..`): the formatter's name.
        let mut p = open + 1;
        if matches!(tok(p), Some(Token::Amp)) {
            p += 1;
        }
        let formatter = match (tok(p), tok(p + 1), tok(p + 2), tok(p + 3)) {
            (
                Some(Token::SelfValue),
                Some(Token::Comma),
                Some(Token::Identifier { name, .. }),
                Some(Token::Colon),
            ) => Some(name.clone()),
            _ => None,
        };
        // A `fn fmt(self)` with no formatter is not Rust's method: it is
        // parsed as written.
        let Some(formatter) = formatter else {
            return false;
        };
        let new_name = if kind == "Display" {
            "to_string"
        } else {
            "fmt_debug"
        };
        let name_span = toks[name_at].span;
        let sig_end = toks[body_open - 1].span;
        let mut edits = vec![crate::resolver::TextEdit {
            offset: name_span.offset,
            length: sig_end.offset + sig_end.length - name_span.offset,
            replacement: format!("{new_name}(self) -> String"),
        }];
        let mut fixable = true;
        {
            let f = &formatter;
            for i in body_open + 1..body_close {
                if !is_ident(i, f) {
                    continue;
                }
                let write = i >= 3
                    && is_ident(i - 3, "write")
                    && matches!(tok(i - 2), Some(Token::Bang))
                    && matches!(tok(i - 1), Some(Token::LeftParen))
                    && matches!(tok(i + 1), Some(Token::Comma));
                if write {
                    let returned = matching(i - 1)
                        .is_some_and(|c| !matches!(tok(c + 1), Some(Token::Question)));
                    fixable &= returned;
                    let from = toks[i - 3].span.offset;
                    edits.push(crate::resolver::TextEdit {
                        offset: from,
                        length: toks[i + 2].span.offset - from,
                        replacement: "format!(".to_string(),
                    });
                    continue;
                }
                let write_str = matches!(tok(i + 1), Some(Token::Dot))
                    && is_ident(i + 2, "write_str")
                    && matches!(tok(i + 3), Some(Token::LeftParen))
                    && matches!(tok(i + 4), Some(Token::StringLiteral(_)))
                    && matches!(tok(i + 5), Some(Token::RightParen))
                    && !matches!(tok(i + 6), Some(Token::Question));
                if write_str {
                    let from = toks[i].span.offset;
                    let lit = toks[i + 4].span;
                    let end = toks[i + 5].span;
                    edits.push(crate::resolver::TextEdit {
                        offset: from,
                        length: lit.offset - from,
                        replacement: String::new(),
                    });
                    edits.push(crate::resolver::TextEdit {
                        offset: end.offset,
                        length: end.length,
                        replacement: ".to_string()".to_string(),
                    });
                    continue;
                }
                fixable = false;
            }
        }
        let message = format!(
            "`fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result` is Rust's -- Kāra's \
             `{kind}` is `fn {new_name}(self) -> String`, which returns the text: write \
             `fn {new_name}(self) -> String {{ f\"..\" }}`{}",
            if fixable {
                ""
            } else {
                ", building the text where the body writes to the formatter"
            }
        );
        self.error_at(&message, name_span);
        if fixable {
            self.fix_diffs
                .insert(crate::resolver::SpanKey::from_span(&name_span), edits);
        }
        self.pos = body_close + 1;
        true
    }

    fn parse_assoc_type_binding(&mut self) -> Option<AssocTypeBinding> {
        let start = self.current_span();
        self.expect(&Token::Type)?;
        let name = self.expect_identifier()?;
        // GAT binding `type Name[P1, P2, ...] = TypeExpr` mirrors the
        // declaration's parameter-list shape. Same effect-polymorphism
        // rejection rule as the trait-side declaration.
        let generic_params = self.parse_optional_generic_params();
        self.reject_gat_effect_params(generic_params.as_ref(), "binding");
        self.expect(&Token::Equal)?;
        let ty = self.parse_type()?;
        let where_clause = self.parse_optional_where_clause();
        self.expect(&Token::Semicolon)?;
        Some(AssocTypeBinding {
            span: self.span_from(&start),
            name,
            generic_params,
            ty,
            where_clause,
        })
    }
}
