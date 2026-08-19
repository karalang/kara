//! Effect-system parsing — effect declarations (`effect resource`,
//! `effect group`, `effect verb`) and effect annotations (`with E`,
//! `effects { ... }` lists on function signatures).

use crate::ast::*;
use crate::lexer::IdentClass;
use crate::token::Span;
use crate::token::Token;

impl super::Parser {
    // ── Effect Declarations ──────────────────────────────────────

    pub(super) fn parse_effect_decl(
        &mut self,
        is_pub: bool,
        is_stable: bool,
        is_transparent: bool,
    ) -> Option<Item> {
        let start = self.current_span();
        self.expect(&Token::Effect)?;

        if is_transparent {
            if !self.eat(&Token::Verb) {
                self.error("Expected 'verb' after 'transparent effect'");
                return None;
            }
            let name = self.expect_identifier()?;
            let name_span = self.span_from(&start);
            self.check_ident_class(&name, IdentClass::Value, "effect verb", name_span);
            self.expect(&Token::Semicolon)?;
            return Some(Item::EffectVerbDecl(EffectVerbDecl {
                span: self.span_from(&start),
                is_pub,
                is_transparent: true,
                verb_name: name,
            }));
        }

        match self.peek_token_ref() {
            Token::Resource => {
                self.advance();
                self.parse_effect_resource_tail(&start)
            }
            Token::Group => {
                self.advance();
                let name = self.expect_identifier()?;
                let name_span = self.span_from(&start);
                self.check_ident_class(&name, IdentClass::Value, "effect group", name_span);
                self.expect(&Token::Equal)?;
                let body = self.parse_effect_group_body()?;
                self.expect(&Token::Semicolon)?;
                Some(Item::EffectGroup(EffectGroupDecl {
                    span: self.span_from(&start),
                    is_pub,
                    is_stable,
                    name,
                    body,
                }))
            }
            Token::Verb => {
                self.advance();
                let name = self.expect_identifier()?;
                let name_span = self.span_from(&start);
                self.check_ident_class(&name, IdentClass::Value, "effect verb", name_span);
                self.expect(&Token::Semicolon)?;
                Some(Item::EffectVerbDecl(EffectVerbDecl {
                    span: self.span_from(&start),
                    is_pub,
                    is_transparent: false,
                    verb_name: name,
                }))
            }
            _ => {
                self.error("Expected 'resource', 'group', or 'verb' after 'effect'");
                None
            }
        }
    }

    /// The body of an `effect resource` declaration, from just after the
    /// `resource` keyword to the terminating `;`.
    ///
    /// ONE implementation for the two spellings — `effect resource R;` and the
    /// bare `resource R;` shorthand — which had duplicate copies that were
    /// already one edit away from drifting apart.
    pub(super) fn parse_effect_resource_tail(&mut self, start: &Span) -> Option<Item> {
        let name = self.expect_identifier()?;
        let name_span = self.span_from(start);
        self.check_ident_class(&name, IdentClass::Type, "effect resource", name_span);
        let key_param = self.parse_optional_resource_key_param();
        let provider_bounds = self.parse_provider_bounds()?;
        self.expect(&Token::Semicolon)?;
        Some(Item::EffectResource(EffectResourceDecl {
            span: self.span_from(start),
            name,
            key_param,
            provider_bounds,
            canonical_host_name: None,
        }))
    }

    /// `: A`, `: Provider[Request]`, or `: A + B + C` after a resource name —
    /// the declaration's provider trait BOUNDS, in source order. Returns an
    /// empty `Vec` for a bare `effect resource R;`.
    ///
    /// The `+`-joined form is design.md:7216, spec'd normatively at :7213
    /// ("Multiple trait bounds are allowed on a resource declaration:") with
    /// semantics at :7217 ("Any provider passed to `with_provider` must
    /// implement all declared bounds plus `Send + Sync`"). Before
    /// B-2026-08-19-3 the parser stopped at the `+` with "Expected Semicolon,
    /// found Plus"; the trailing bounds are carried through every phase rather
    /// than dropped, since accepting syntax whose meaning is ignored would be
    /// worse than not accepting it.
    fn parse_provider_bounds(&mut self) -> Option<Vec<ProviderBound>> {
        let mut bounds = Vec::new();
        if !self.eat(&Token::Colon) {
            return Some(bounds);
        }
        loop {
            let trait_start = self.current_span();
            let name = self.expect_identifier()?;
            let name_span = self.span_from(&trait_start);
            // `: Provider[Request]` — the bound's generic arguments, spelled
            // exactly as any other trait bound spells them, and parsed by the
            // same helper (B-2026-08-18-41). Without this the parser stopped at
            // `[` with "Expected Semicolon, found LeftBracket", so the only way
            // to name a generic provider trait was to drop its argument — which
            // parses and then fails at every call site with "expected 'T'".
            let args = if self.check(&Token::LeftBracket) {
                Some(self.parse_generic_type_args()?)
            } else {
                None
            };
            bounds.push(ProviderBound {
                name,
                name_span,
                args,
            });
            if !self.eat(&Token::Plus) {
                break;
            }
        }
        Some(bounds)
    }

    /// `[user_id: i64]` after a resource name — the PARTITION KEY of a
    /// parameterized resource (design.md § Parameterized Resources,
    /// B-2026-08-18-41).
    ///
    /// This slot used to go to `parse_optional_generic_params`, so the spec's
    /// own example failed with "`user_id` is Value-class but generic type
    /// parameters must be Type-class" — a naming-convention complaint about a
    /// declaration that was never generic. Nothing downstream ever read those
    /// generic params, no spec text declares a generic effect resource, and the
    /// key is the only `[...]` written after a resource name anywhere in
    /// design.md, so the slot is the key's outright.
    fn parse_optional_resource_key_param(&mut self) -> Option<ResourceKeyParam> {
        if !self.check(&Token::LeftBracket) {
            return None;
        }
        let open = self.current_span();
        self.advance();
        let name_start = self.current_span();
        let name = self.expect_identifier()?;
        let name_span = self.span_from(&name_start);
        // Value-class, like any binding: `[user_id: i64]`, never `[T: i64]`.
        // Getting this backwards is exactly the error the old generic-param
        // path produced, so it is worth naming the right class here.
        self.check_ident_class(&name, IdentClass::Value, "resource key", name_span);
        self.expect(&Token::Colon)?;
        let ty = self.parse_type()?;
        self.expect(&Token::RightBracket)?;
        Some(ResourceKeyParam {
            name,
            name_span,
            ty,
            span: self.span_from(&open),
        })
    }

    fn parse_effect_group_body(&mut self) -> Option<Vec<EffectGroupTerm>> {
        let mut terms = Vec::new();
        loop {
            if let Some(verb) = self.try_parse_effect_verb() {
                terms.push(EffectGroupTerm::Verb(verb));
            } else if let Token::Identifier { .. } = self.peek_token_ref() {
                let name = self.expect_identifier()?;
                terms.push(EffectGroupTerm::GroupRef(name));
            } else {
                break;
            }

            if !self.eat(&Token::Plus) {
                break;
            }
        }
        Some(terms)
    }

    // ── Effect Annotations ───────────────────────────────────────

    pub(super) fn parse_optional_effect_list(
        &mut self,
        effect_vars: &[String],
    ) -> Option<EffectList> {
        // Effects start with an effect verb keyword or `with`
        if !self.is_effect_start() {
            return None;
        }
        // Signature-position effect clause: a `{`/`;`/end follows, so a comma
        // between effect items is always a mistake — recovery on.
        self.parse_effect_list(effect_vars, true)
    }

    fn is_effect_start(&self) -> bool {
        matches!(
            self.peek_token(),
            Token::Reads
                | Token::Writes
                | Token::Sends
                | Token::Receives
                | Token::Allocates
                | Token::Panics
                | Token::Blocks
                | Token::Suspends
                | Token::With
        )
    }

    pub(crate) fn parse_effect_list(
        &mut self,
        effect_vars: &[String],
        recover_stray_comma: bool,
    ) -> Option<EffectList> {
        let start = self.current_span();
        let mut items = Vec::new();

        // Handle `with` keyword
        if self.eat(&Token::With) {
            // with _ (anonymous polymorphic)
            if self.eat(&Token::Underscore) {
                return Some(EffectList {
                    items: vec![EffectItem::Polymorphic],
                    span: self.span_from(&start),
                });
            }

            // Parse space-separated effect items: verbs, effect variables, or group names
            loop {
                // Try verb first (reads, writes, blocks, user-defined, etc.)
                if let Some(verb) = self.try_parse_effect_verb() {
                    items.push(EffectItem::Verb(verb));
                } else if let Token::Identifier { .. } = self.peek_token_ref() {
                    let name = self.expect_identifier()?;
                    // Named effect variable declared in the function's [with E] params
                    // takes precedence over effect group lookup.
                    if effect_vars.contains(&name) {
                        items.push(EffectItem::Variable(name));
                    } else {
                        items.push(EffectItem::Group(name));
                    }
                } else if recover_stray_comma && self.check(&Token::Comma) && !items.is_empty() {
                    // Common LLM/porting habit: comma-separating effect items
                    // (`with reads(A), reads(B)`). Kāra effect clauses are
                    // space-separated; the stray comma otherwise surfaces as a
                    // confusing "Expected LeftBrace, found Comma" at the caller.
                    // Emit a focused, machine-applicable diagnostic and recover
                    // by consuming the comma so the rest of the clause parses
                    // (each stray comma gets its own delete edit).
                    let comma_span = self.current_span();
                    self.error_at(
                        "effect items are space-separated, not comma-separated; remove the `,`",
                        comma_span,
                    );
                    self.fix_edits.insert(
                        crate::resolver::SpanKey::from_span(&comma_span),
                        crate::resolver::TextEdit {
                            offset: comma_span.offset,
                            length: comma_span.length,
                            replacement: String::new(),
                        },
                    );
                    self.advance();
                } else {
                    break;
                }
            }

            if items.is_empty() {
                self.error("Expected effect verb, effect variable, or group name after 'with'");
                return None;
            }

            return Some(EffectList {
                items,
                span: self.span_from(&start),
            });
        }

        // Parse verb-based effect list (without `with` prefix — for effect groups in decls)
        while let Some(verb) = self.try_parse_effect_verb() {
            items.push(EffectItem::Verb(verb));
        }

        if items.is_empty() {
            None
        } else {
            Some(EffectList {
                items,
                span: self.span_from(&start),
            })
        }
    }

    /// `pub(crate)` for the `#[no_effect(...)]` attribute parser, which needs
    /// exactly this grammar: the attribute forbids the same verbs a `with`
    /// clause declares, so sharing one parser is what keeps the two spellings
    /// from drifting.
    pub(crate) fn try_parse_effect_verb(&mut self) -> Option<EffectVerb> {
        let start = self.current_span();
        let kind = match self.peek_token_ref() {
            Token::Reads => {
                self.advance();
                EffectVerbKind::Reads
            }
            Token::Writes => {
                self.advance();
                EffectVerbKind::Writes
            }
            Token::Sends => {
                self.advance();
                EffectVerbKind::Sends
            }
            Token::Receives => {
                self.advance();
                EffectVerbKind::Receives
            }
            Token::Allocates => {
                self.advance();
                EffectVerbKind::Allocates
            }
            Token::Panics => {
                self.advance();
                return Some(EffectVerb {
                    kind: EffectVerbKind::Panics,
                    resources: Vec::new(),
                    span: self.span_from(&start),
                });
            }
            Token::Blocks => {
                self.advance();
                EffectVerbKind::Blocks
            }
            Token::Suspends => {
                self.advance();
                EffectVerbKind::Suspends
            }
            Token::Identifier { name, .. } if self.peek_ahead_is_left_paren() => {
                let name = name.clone();
                self.advance();
                EffectVerbKind::UserDefined(name)
            }
            _ => return None,
        };

        // Blocks and suspends may appear without resources (like panics)
        if !self.check(&Token::LeftParen) {
            return Some(EffectVerb {
                kind,
                resources: Vec::new(),
                span: self.span_from(&start),
            });
        }

        self.expect(&Token::LeftParen)?;
        let mut resources = Vec::new();
        loop {
            let res = self.parse_resource()?;
            resources.push(res);
            if !self.eat(&Token::Comma) {
                break;
            }
            if self.check(&Token::RightParen) {
                break;
            }
        }
        self.expect(&Token::RightParen)?;

        Some(EffectVerb {
            kind,
            resources,
            span: self.span_from(&start),
        })
    }

    /// Check if the token after the current one is a left paren (used for user-defined verb lookahead)
    fn peek_ahead_is_left_paren(&self) -> bool {
        if self.pos + 1 < self.tokens.len() {
            self.tokens[self.pos + 1].token == Token::LeftParen
        } else {
            false
        }
    }

    fn parse_resource(&mut self) -> Option<Resource> {
        let start = self.current_span();
        let path = self.parse_path_segments()?;

        let param = if self.eat(&Token::LeftBracket) {
            let expr = self.parse_expression()?;
            self.expect(&Token::RightBracket)?;
            Some(Box::new(expr))
        } else {
            None
        };

        Some(Resource {
            path,
            param,
            span: self.span_from(&start),
        })
    }
}
