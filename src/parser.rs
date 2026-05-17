// src/parser.rs

//! Recursive-descent parser for Kāra source code.
//! Produces an AST from a token stream with error recovery and multi-error reporting.

use crate::ast::*;
use crate::lexer::{
    classify_ident, suggest_const_name, suggest_type_name, suggest_value_name, IdentClass,
};
use crate::token::{Span, SpannedToken, Token};

mod attributes;
mod exprs;
mod generics;
mod items;
mod items_effects;
mod items_extern;
mod items_trait;
mod patterns;
mod stmts;
mod types;

// ── Parse Errors ─────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ParseError {
    pub message: String,
    pub span: Span,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}:{}: {}",
            self.span.line, self.span.column, self.message
        )
    }
}

pub struct ParseResult {
    pub program: Program,
    pub errors: Vec<ParseError>,
}

/// Surrounding signature kind for parameter parsing — selects between the
/// trait-method and free-function anonymous-parameter diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FnContext {
    /// `fn` inside a `trait { ... }` body. Drives `E_TRAIT_METHOD_ANONYMOUS_PARAM`.
    TraitMethod,
    /// Free function, impl method, or extern function. Drives
    /// `E_FN_ANONYMOUS_PARAM`. Impl methods reuse the free-function
    /// diagnostic per design.md § Trait method parameter names — required;
    /// the rule only requires *trait declarations* to name parameters,
    /// but the focused diagnostic for free fns helps catch the equivalent
    /// type-only paste typo.
    Function,
}

// `.` is both the path separator and the field/method access operator. The parser
// disambiguates using the case class of the initial identifier: Type- and Const-class
// names (uppercase leading letter) start paths; Value-class names (lowercase) start
// postfix chains. See docs/design.md § Identifiers and Naming.
pub(crate) fn starts_upper(s: &str) -> bool {
    s.chars().next().is_some_and(|c| c.is_ascii_uppercase())
}

// ── Parser ───────────────────────────────────────────────────────

pub struct Parser {
    pub(crate) tokens: Vec<SpannedToken>,
    pub(crate) pos: usize,
    pub(crate) errors: Vec<ParseError>,
    /// Active labels for disambiguating `break label` vs `break value` and
    /// for routing labeled-block label scopes. Each entry carries a
    /// `LabelKind` tag (`Loop` for labeled loops, `Block` for labeled
    /// blocks) — the kind is consulted by the resolver, not the parser,
    /// but the parser tracks it so that `is_known_label` lookups (which
    /// disambiguate `break label expr`) work uniformly across both label
    /// kinds. Pushed at the entry to a labeled construct, popped on exit.
    pub(crate) loop_labels: Vec<(String, LabelKind)>,
    /// Doc-comment text accumulated by the leading-`///` collection at the
    /// top of `parse_item`. Each item-construction site calls
    /// `Self::take_pending_doc` when filling the new node's `doc_comment`
    /// field. Cleared after consumption so a subsequent item without docs
    /// gets `None`.
    pub(crate) pending_doc: Option<String>,
    /// Stack of `FnContext`s for the function-like signature we are currently
    /// parsing parameters for. Drives the anonymous-parameter focused
    /// diagnostic: trait-method bodies emit `E_TRAIT_METHOD_ANONYMOUS_PARAM`
    /// while free / impl / extern function bodies emit
    /// `E_FN_ANONYMOUS_PARAM`. Empty when we are not inside a parameter
    /// list (e.g., parsing a struct field or top-level expression).
    pub(crate) fn_context_stack: Vec<FnContext>,
    /// Stack of effect-variable names declared in the enclosing function /
    /// trait method's `[with E]` generic params. Pushed when entering a
    /// signature with declared effect vars; popped when leaving. Consulted
    /// when parsing nested `Fn(...) with E` types in parameter / return
    /// position so an `E` token resolves to `EffectItem::Variable(E)`
    /// instead of `EffectItem::Group(E)`. Empty top frame means no
    /// effect variables in scope (parser treats all `with X` items as
    /// group references).
    pub(crate) effect_var_stack: Vec<Vec<String>>,
    /// Stack of `impl Trait` rejection reasons for the current
    /// type-expression position. `impl Trait` slice 1: parsing a
    /// `TypeKind::ImplTrait` is only legal in four positions
    /// (argument types, return types, trait-method return types, RHS
    /// of `type` aliases — see design.md § `impl Trait` (Existential
    /// Types)). Every other type-position pushes a `BlockReason` onto
    /// the stack via [`Parser::push_impl_trait_block`] before
    /// descending into [`Parser::parse_type`]; the top-of-stack
    /// reason is consulted in `parse_type` when an `impl` keyword is
    /// observed and produces the corresponding rejection diagnostic
    /// (one of `E_IMPL_TRAIT_IN_NESTED_POSITION`,
    /// `E_IMPL_TRAIT_IN_TRAIT_METHOD_ARG`). Empty stack means the
    /// current position is legal for `impl Trait`.
    pub(crate) impl_trait_block_stack: Vec<ImplTraitBlockReason>,
}

/// Reason an `impl Trait` type expression is rejected at the current
/// parser position. Encoded as an enum (rather than a single
/// `disallow_impl_trait: bool` flag) so the rejection diagnostic can
/// route to the position-specific error code + suggestion text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ImplTraitBlockReason {
    /// `impl Trait` inside a nested generic-argument list — e.g.
    /// `Vec[impl T]`. Rejected with `E_IMPL_TRAIT_IN_NESTED_POSITION`
    /// per the design.md spec: "`Vec[impl T]` is rejected at v1 with
    /// a diagnostic suggesting an explicit generic parameter — `impl
    /// Trait` deep in nested type positions is post-v1".
    NestedGenericArg,
    /// `impl Trait` in an argument-type position of a trait method
    /// declaration. Rejected with `E_IMPL_TRAIT_IN_TRAIT_METHOD_ARG`
    /// per design.md: "The compiler does not allow argument-position
    /// `impl Trait` in trait methods (use the explicit generic form
    /// there instead)." Argument-position `impl Trait` in free
    /// functions and impl-block methods stays legal (slice 2 desugars
    /// those to anonymous generic parameters).
    TraitMethodArg,
}

impl Parser {
    pub fn new(tokens: Vec<SpannedToken>) -> Self {
        Parser {
            tokens,
            pos: 0,
            loop_labels: Vec::new(),
            errors: Vec::new(),
            pending_doc: None,
            fn_context_stack: Vec::new(),
            effect_var_stack: Vec::new(),
            impl_trait_block_stack: Vec::new(),
        }
    }

    /// Push an `impl Trait` rejection reason — see
    /// [`ImplTraitBlockReason`]. Caller is responsible for the
    /// matching [`Parser::pop_impl_trait_block`] after the
    /// `parse_type` (or `parse_generic_type_args`) call returns.
    pub(crate) fn push_impl_trait_block(&mut self, reason: ImplTraitBlockReason) {
        self.impl_trait_block_stack.push(reason);
    }

    pub(crate) fn pop_impl_trait_block(&mut self) {
        self.impl_trait_block_stack.pop();
    }

    /// Returns the current top-of-stack rejection reason, if any.
    pub(crate) fn current_impl_trait_block(&self) -> Option<ImplTraitBlockReason> {
        self.impl_trait_block_stack.last().copied()
    }

    fn current_effect_vars(&self) -> &[String] {
        self.effect_var_stack
            .last()
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Drain pending leading `///` doc-comment text. Returns `None` when no
    /// doc comments are attached. Called from each item-construction site.
    fn take_pending_doc(&mut self) -> Option<String> {
        self.pending_doc.take()
    }

    /// Consume any leading `Token::DocComment` tokens at the current
    /// position and accumulate their text (newline-joined) into
    /// `pending_doc`. Idempotent — safe to call when no doc comments
    /// follow. Stops at the first non-doc-comment token.
    fn collect_leading_doc_comments(&mut self) {
        let mut buf: Option<String> = None;
        while let Token::DocComment(text) = self.peek_token() {
            let line = text.clone();
            self.advance();
            match &mut buf {
                Some(s) => {
                    s.push('\n');
                    s.push_str(&line);
                }
                None => buf = Some(line),
            }
        }
        if buf.is_some() {
            self.pending_doc = buf;
        }
    }

    pub fn parse(mut self) -> ParseResult {
        let program = self.parse_program();
        ParseResult {
            program,
            errors: self.errors,
        }
    }

    // ── Program ──────────────────────────────────────────────────

    fn parse_program(&mut self) -> Program {
        // Consume any leading `//!` module-level doc comments. They may
        // only appear before the first item; subsequent `//!` lines are
        // a parse error, but for the v1 MVP we silently treat trailing
        // `//!` as a regular line comment via the lexer's normal path.
        let mut module_doc_lines: Vec<String> = Vec::new();
        while let Token::ModuleDocComment(text) = self.peek_token() {
            module_doc_lines.push(text.clone());
            self.advance();
        }
        let module_doc_comment = if module_doc_lines.is_empty() {
            None
        } else {
            Some(module_doc_lines.join("\n"))
        };

        let mut items = Vec::new();
        while !self.is_at_end() {
            match self.parse_item() {
                Some(item) => items.push(item),
                None => {
                    // Error recovery: skip to next item-starting token
                    self.synchronize_to_item();
                }
            }
        }
        Program {
            items,
            module_doc_comment,
            ..Program::default()
        }
    }

    // ── Path Helpers ─────────────────────────────────────────────

    fn parse_path_segments(&mut self) -> Option<Vec<String>> {
        let mut segments = Vec::new();

        match self.peek_token() {
            Token::Identifier { .. } => segments.push(self.expect_identifier()?),
            Token::SelfType => {
                self.advance();
                segments.push("Self".to_string());
            }
            _ => {
                self.error("Expected identifier");
                return None;
            }
        }

        while self.eat(&Token::Dot) {
            segments.push(self.expect_identifier()?);
        }
        Some(segments)
    }

    // ── Token Helpers ────────────────────────────────────────────

    fn peek_token(&self) -> Token {
        if self.pos < self.tokens.len() {
            self.tokens[self.pos].token.clone()
        } else {
            Token::EOF
        }
    }

    fn peek_token_at(&self, offset: usize) -> Token {
        let idx = self.pos + offset;
        if idx < self.tokens.len() {
            self.tokens[idx].token.clone()
        } else {
            Token::EOF
        }
    }

    fn current_span(&self) -> Span {
        if self.pos < self.tokens.len() {
            self.tokens[self.pos].span.clone()
        } else if !self.tokens.is_empty() {
            self.tokens.last().unwrap().span.clone()
        } else {
            Span {
                line: 1,
                column: 1,
                offset: 0,
                length: 0,
            }
        }
    }

    fn advance(&mut self) -> &SpannedToken {
        let tok = &self.tokens[self.pos];
        if self.pos < self.tokens.len() {
            self.pos += 1;
        }
        tok
    }

    fn check(&self, expected: &Token) -> bool {
        std::mem::discriminant(&self.peek_token()) == std::mem::discriminant(expected)
    }

    fn eat(&mut self, expected: &Token) -> bool {
        if self.check(expected) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, expected: &Token) -> Option<()> {
        if self.check(expected) {
            self.advance();
            Some(())
        } else {
            self.error(&format!(
                "Expected {:?}, found {:?}",
                expected,
                self.peek_token()
            ));
            None
        }
    }

    fn expect_identifier(&mut self) -> Option<String> {
        match self.peek_token() {
            Token::Identifier { name, .. } => {
                self.advance();
                Some(name)
            }
            _ => {
                self.error(&format!(
                    "Expected identifier, found {:?}",
                    self.peek_token()
                ));
                None
            }
        }
    }

    /// Like [`expect_identifier`] but accepts a small set of contextual
    /// keywords as identifier-equivalents in attribute-arg position. The
    /// design uses `requires` (already a contract keyword) and `ensures`
    /// inside `#[test(...)]`-style attributes; treating them as identifiers
    /// only here keeps the contract grammar untouched while letting
    /// design-conformant attribute syntax parse without quoting.
    /// Like [`expect_identifier`] but accepts the logical-operator
    /// keywords `not` / `and` / `or` as identifier-equivalents. Used
    /// at trait-method-name and impl-method-name positions so the
    /// stdlib can declare `trait Not { fn not(self) -> Self }` —
    /// design.md's bitwise-Not trait — without a separate raw-ident
    /// syntax. The lexer eagerly tokenizes these as `Token::Not` /
    /// `Token::And` / `Token::Or` regardless of context, so a
    /// targeted post-parse escape is the cleanest fix. Mirrors
    /// `expect_attr_arg_name`'s treatment of `requires` / `ensures`.
    fn expect_method_name(&mut self) -> Option<String> {
        match self.peek_token() {
            Token::Identifier { name, .. } => {
                self.advance();
                Some(name)
            }
            Token::Not => {
                self.advance();
                Some("not".to_string())
            }
            Token::And => {
                self.advance();
                Some("and".to_string())
            }
            Token::Or => {
                self.advance();
                Some("or".to_string())
            }
            _ => {
                self.error(&format!(
                    "Expected method name, found {:?}",
                    self.peek_token()
                ));
                None
            }
        }
    }

    fn expect_attr_arg_name(&mut self) -> Option<String> {
        match self.peek_token() {
            Token::Identifier { name, .. } => {
                self.advance();
                Some(name)
            }
            Token::Requires => {
                self.advance();
                Some("requires".to_string())
            }
            Token::Ensures => {
                self.advance();
                Some("ensures".to_string())
            }
            _ => {
                self.error(&format!(
                    "Expected attribute argument name, found {:?}",
                    self.peek_token()
                ));
                None
            }
        }
    }

    fn is_at_end(&self) -> bool {
        self.pos >= self.tokens.len() || self.peek_token() == Token::EOF
    }

    /// Lookahead used by `parse_attribute` to decide whether the current
    /// attribute-arg position starts a named (`ident = value` /
    /// `ident: value`) or positional (bare expression) argument. A named
    /// arg is an identifier-shaped token whose immediate successor is
    /// `=` or `:`; everything else — including a bare identifier followed
    /// by `,` / `)` / `.` — is treated as the start of an expression.
    /// The contract keywords `requires` / `ensures` are treated as
    /// identifiers here to match `expect_attr_arg_name`.
    fn is_named_attr_arg_head(&self) -> bool {
        if self.pos + 1 >= self.tokens.len() {
            return false;
        }
        let is_name_token = matches!(
            self.tokens[self.pos].token,
            Token::Identifier { .. } | Token::Requires | Token::Ensures
        );
        if !is_name_token {
            return false;
        }
        matches!(self.tokens[self.pos + 1].token, Token::Equal | Token::Colon)
    }

    fn span_from(&self, start: &Span) -> Span {
        let end = if self.pos > 0 {
            &self.tokens[self.pos - 1].span
        } else {
            start
        };
        Span {
            line: start.line,
            column: start.column,
            offset: start.offset,
            length: (end.offset + end.length).saturating_sub(start.offset),
        }
    }

    // ── Error Recovery ───────────────────────────────────────────

    fn error(&mut self, message: &str) {
        let span = self.current_span();
        self.errors.push(ParseError {
            message: message.to_string(),
            span,
        });
    }

    /// Emit a non-fatal diagnostic if `name` does not have the expected
    /// `IdentClass`. The `context` string is the declaration kind (e.g.
    /// `"struct"`, `"fn"`, `"const"`). The diagnostic includes a rename
    /// suggestion.
    fn check_ident_class(&mut self, name: &str, expected: IdentClass, context: &str, span: Span) {
        let actual = classify_ident(name);
        if actual == expected {
            return;
        }
        let (expected_desc, suggestion) = match expected {
            IdentClass::Type => ("Type-class (PascalCase)", suggest_type_name(name)),
            IdentClass::Value => ("Value-class (snake_case)", suggest_value_name(name)),
            IdentClass::Const => (
                "Const-class (SCREAMING_SNAKE_CASE)",
                suggest_const_name(name),
            ),
        };
        let actual_desc = match actual {
            IdentClass::Type => "Type-class",
            IdentClass::Value => "Value-class",
            IdentClass::Const => "Const-class",
        };
        self.errors.push(ParseError {
            message: format!(
                "`{name}` is {actual_desc} but {context} names must be {expected_desc}; consider renaming to `{suggestion}`"
            ),
            span,
        });
    }

    fn synchronize_to_item(&mut self) {
        while !self.is_at_end() {
            match self.peek_token() {
                Token::Fn
                | Token::Struct
                | Token::Enum
                | Token::Trait
                | Token::Impl
                | Token::Effect
                | Token::Transparent
                | Token::Layout
                | Token::Mod
                | Token::Use
                | Token::Import
                | Token::Const
                | Token::Alias
                | Token::Independent
                | Token::Extern
                | Token::Type
                | Token::Distinct
                | Token::Pub
                | Token::Stable
                | Token::Pound => return,
                _ => {
                    self.advance();
                }
            }
        }
    }

    fn synchronize_to_stmt(&mut self) {
        while !self.is_at_end() {
            match self.peek_token() {
                Token::Semicolon => {
                    self.advance();
                    return;
                }
                Token::RightBrace => return,
                Token::Let
                | Token::Return
                | Token::If
                | Token::While
                | Token::For
                | Token::Loop
                | Token::Match
                | Token::Break
                | Token::Continue => return,
                _ => {
                    self.advance();
                }
            }
        }
    }
}

// ── Diagnostic helpers ──────────────────────────────────────────

/// Render a `TypeExpr` back to a compact source-style string for inclusion
/// in parser diagnostics (e.g., the `_: <type>` fix-it on
/// `E_FN_ANONYMOUS_PARAM`). Covers every `TypeKind` variant the parser
/// can build; not byte-for-byte identical to the original source, but
/// produces a copy-pasteable surface form.
pub(crate) fn render_type_for_diagnostic(ty: &TypeExpr) -> String {
    let mut out = String::new();
    write_type_for_diagnostic(ty, &mut out);
    out
}

fn write_type_for_diagnostic(ty: &TypeExpr, out: &mut String) {
    match &ty.kind {
        TypeKind::Path(path) => {
            for (i, seg) in path.segments.iter().enumerate() {
                if i > 0 {
                    out.push('.');
                }
                out.push_str(seg);
            }
            if let Some(args) = &path.generic_args {
                if !args.is_empty() {
                    out.push('[');
                    for (i, arg) in args.iter().enumerate() {
                        if i > 0 {
                            out.push_str(", ");
                        }
                        match arg {
                            crate::ast::GenericArg::Type(t) => write_type_for_diagnostic(t, out),
                            crate::ast::GenericArg::Const(_) => out.push('_'),
                        }
                    }
                    out.push(']');
                }
            }
        }
        TypeKind::Tuple(types) => {
            out.push('(');
            for (i, t) in types.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_type_for_diagnostic(t, out);
            }
            out.push(')');
        }
        TypeKind::Array { element, .. } => {
            out.push('[');
            write_type_for_diagnostic(element, out);
            out.push_str("; _]");
        }
        TypeKind::Pointer { is_mut, inner } => {
            out.push('*');
            out.push_str(if *is_mut { "mut " } else { "const " });
            write_type_for_diagnostic(inner, out);
        }
        TypeKind::FnType {
            params,
            return_type,
            is_once,
            ..
        } => {
            out.push_str(if *is_once { "OnceFn(" } else { "Fn(" });
            for (i, p) in params.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_type_for_diagnostic(p, out);
            }
            out.push(')');
            if let Some(rt) = return_type {
                out.push_str(" -> ");
                write_type_for_diagnostic(rt, out);
            }
        }
        TypeKind::Ref(inner) => {
            out.push_str("ref ");
            write_type_for_diagnostic(inner, out);
        }
        TypeKind::MutRef(inner) => {
            out.push_str("mut ref ");
            write_type_for_diagnostic(inner, out);
        }
        TypeKind::MutSlice(element) => {
            out.push_str("mut Slice[");
            write_type_for_diagnostic(element, out);
            out.push(']');
        }
        TypeKind::Weak(inner) => {
            out.push_str("weak ");
            write_type_for_diagnostic(inner, out);
        }
        // `impl Trait` slice 1 stub: render the surface form for the
        // anonymous-parameter `_: <type>` fix-it suggestion. The full
        // existential-effect rendering is unimportant for the
        // diagnostic surface — we use the bare `impl TraitPath[args]`
        // form (no `with` clause), since the `with` half is rarely
        // load-bearing in a parameter-type-paste typo.
        TypeKind::ImplTrait {
            trait_path, args, ..
        } => {
            out.push_str("impl ");
            for (i, seg) in trait_path.segments.iter().enumerate() {
                if i > 0 {
                    out.push('.');
                }
                out.push_str(seg);
            }
            if !args.is_empty() {
                out.push('[');
                for (i, arg) in args.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    match arg {
                        crate::ast::GenericArg::Type(t) => write_type_for_diagnostic(t, out),
                        crate::ast::GenericArg::Const(_) => out.push('_'),
                    }
                }
                out.push(']');
            }
        }
        TypeKind::Unit => out.push_str("()"),
        TypeKind::Error => out.push('_'),
    }
}
