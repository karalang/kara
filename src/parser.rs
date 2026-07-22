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
    /// Span-keyed machine-applicable edits synthesized during parsing (e.g.
    /// delete a stray comma in a comma-separated `with` clause). Consumed by
    /// `collect_diagnostics` (JSON `replacement`) and `cmd_fix`.
    pub fix_edits: std::collections::HashMap<crate::resolver::SpanKey, crate::resolver::TextEdit>,
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

/// FFI calling-convention ABIs the grammar recognizes but reserves for a
/// future release: they parse (freezing the syntax at v1 so a later
/// implementation is not source-breaking) but are rejected with a targeted
/// "reserved" diagnostic distinct from the generic "unknown ABI" error.
/// `extern "C"` / `extern "C-unwind"` are the implemented set. See the
/// Phase-11 embedded/hardware tracker and design.md § FFI.
const RESERVED_FFI_ABIS: &[&str] = &["stdcall", "fastcall", "win64", "sysv64"];

pub struct Parser {
    pub(crate) tokens: Vec<SpannedToken>,
    pub(crate) pos: usize,
    pub(crate) errors: Vec<ParseError>,
    /// Script mode (design.md § Script mode): when `true` (the default —
    /// root source files), top-level statements synthesize a unit
    /// `fn main()`. Item-only contexts — comptime `ast.item` quotes,
    /// `mod`-loaded module files — set this `false` via
    /// [`Parser::items_only`], turning top-level statements into a parse
    /// error instead of a synthesized entry point.
    pub(crate) allow_script_mode: bool,
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
    /// Suppresses the Value-class name lint on a parameter binding while
    /// parsing a `comptime` parameter. A `comptime` param may bind a `Type`
    /// pseudovalue, for which a Type-class name (`comptime T: Type`) is
    /// idiomatic and spec-sanctioned (deferred.md § Comptime, form 3). Set
    /// around the `parse_param_pattern` call for comptime params; the binding
    /// case consults it instead of unconditionally requiring snake_case.
    pub(crate) allow_type_class_param_name: bool,
    /// When set, a `Name { … }` at the current position is NOT treated as a
    /// struct literal — the trailing `{` opens a block/body instead. Set only
    /// while parsing an `if` / `while` / `for` CONDITION (where `Name { body }`
    /// must read as `Name` + the loop/branch body block, e.g. `if DEBUG { … }`),
    /// and cleared again inside any delimited sub-expression (parens, call args,
    /// array literals, a committed struct-literal body) so a struct literal
    /// nested in a condition — `while cond(P { x }) { … }` — still parses. NOT
    /// set for a `match` scrutinee: Kāra allows an unparenthesized struct-literal
    /// scrutinee (`match P { x: 1 } { … }`), which the restriction would break.
    /// See `looks_like_struct_literal`.
    pub(crate) no_struct_literal: bool,
    /// Machine-applicable fix edits synthesized during parsing, keyed by the
    /// span of the diagnostic they resolve. Mirrors ownership.rs's
    /// `error_fix_diffs` side-channel: rather than widen `ParseError` (built
    /// at ~48 sites) with a `replacement` field, edits are hung here and
    /// matched back to their diagnostic by span in `collect_diagnostics`
    /// (JSON `replacement`) and `cmd_fix`. Currently populated only by the
    /// comma-separated-effect-clause recovery in `parse_effect_list`.
    pub(crate) fix_edits:
        std::collections::HashMap<crate::resolver::SpanKey, crate::resolver::TextEdit>,
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
            allow_script_mode: true,
            pending_doc: None,
            fn_context_stack: Vec::new(),
            effect_var_stack: Vec::new(),
            impl_trait_block_stack: Vec::new(),
            allow_type_class_param_name: false,
            no_struct_literal: false,
            fix_edits: std::collections::HashMap::new(),
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

    /// Disable script-mode synthesis for item-only contexts (comptime
    /// `ast.item` quotes, module files). Top-level statements then produce
    /// a parse error rather than a synthesized `fn main()`.
    pub fn items_only(mut self) -> Self {
        self.allow_script_mode = false;
        self
    }

    pub fn parse(mut self) -> ParseResult {
        let program = self.parse_program();
        ParseResult {
            program,
            errors: self.errors,
            fix_edits: self.fix_edits,
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

        // Phase-7 line 43 — module-level inner attributes
        // (`#![rc_budget(max: N)]`, etc.). Must appear before any
        // top-level item; subsequent outer attributes (`#[...]`)
        // belong to the first item that follows.
        let inner_attrs = self.parse_inner_attributes();

        let mut items = Vec::new();
        // Script mode (design.md § Script mode, phase-8 Q7): top-level
        // STATEMENTS — expression statements, assignments, Value-class
        // (lowercase) `let`s — collect here in file order and become the
        // body of a synthesized `fn main()` after the loop. Items hoist to
        // module scope exactly as before. Previously these statements were
        // SILENTLY DROPPED (`parse_item` returned `None` with no error and
        // `synchronize_to_item` skipped them), so a main-less script
        // "passed" `karac check` and did nothing under `run --interp`.
        let mut script_stmts: Vec<Stmt> = Vec::new();
        while !self.is_at_end() {
            let errs_before = self.errors.len();
            match self.parse_item() {
                Some(item) => items.push(item),
                None if self.errors.len() == errs_before && !self.is_at_end() => {
                    // Non-item head with no diagnostic — a top-level
                    // statement. Parse it as one; a statement parse that
                    // ALSO fails falls back to the item-level recovery so
                    // the loop always makes progress.
                    let pos_before = self.pos;
                    match self.parse_top_level_script_stmt() {
                        Some(stmt) => script_stmts.push(stmt),
                        None => {
                            if self.pos == pos_before {
                                self.synchronize_to_item();
                            }
                        }
                    }
                }
                None => {
                    // Error recovery: skip to next item-starting token
                    self.synchronize_to_item();
                }
            }
        }
        if !script_stmts.is_empty() && !self.allow_script_mode {
            // Item-only context (`ast.item` quote, module file): top-level
            // statements are an error here, never a synthesized main.
            self.error_at(
                "top-level statements are not allowed in this context — only item \
                 declarations (fn, struct, enum, trait, impl, use); script mode \
                 applies only to a root source file",
                script_stmts[0].span.clone(),
            );
        } else if !script_stmts.is_empty() {
            // Ambiguity rule (design.md § Script mode > Rejecting ambiguous
            // files): top-level statements + an explicit `fn main` is a
            // compile error with one obvious fix in each direction.
            let explicit_main = items.iter().find_map(|it| match it {
                Item::Function(f) if f.name == "main" => Some(f.span.clone()),
                _ => None,
            });
            let first_span = script_stmts[0].span.clone();
            if let Some(main_span) = explicit_main {
                self.error_at(
                    &format!(
                        "file contains both top-level statements and an explicit `fn main()` \
                         (defined at line {}); move the statements into `main`, or remove the \
                         explicit `main` to use script mode",
                        main_span.line
                    ),
                    first_span,
                );
            } else {
                // Synthesize `fn main()` wrapping the statements in file
                // order. Unit return (the REPL cell-wrapper precedent and
                // the entry-point contract's `()` arm); the design doc's
                // `Result[Unit, Error]` signature upgrade is deferred until
                // a catch-all `Error` type exists in v1 — until then a
                // top-level `?` reports the ordinary ?-in-unit-fn
                // diagnostic. Effects are inferred exactly as for a
                // user-written private `fn main()`.
                let last_span = script_stmts.last().map(|s| s.span.clone()).unwrap();
                let body_span = Span {
                    line: first_span.line,
                    column: first_span.column,
                    offset: first_span.offset,
                    length: (last_span.offset + last_span.length).saturating_sub(first_span.offset),
                };
                items.push(Item::Function(Function {
                    span: first_span.clone(),
                    attributes: Vec::new(),
                    doc_comment: None,
                    is_pub: false,
                    is_private: false,
                    is_unsafe: false,
                    is_comptime: false,
                    name: "main".to_string(),
                    generic_params: None,
                    params: Vec::new(),
                    self_param: None,
                    return_type: None,
                    effects: None,
                    requires: Vec::new(),
                    ensures: Vec::new(),
                    where_clause: None,
                    body: Block {
                        stmts: script_stmts,
                        final_expr: None,
                        span: body_span,
                    },
                    stdlib_origin: false,
                    deprecation: None,
                    unstable: None,
                    is_track_caller: false,
                    inline_hint: None,
                    is_cold: false,
                    is_gpu: false,
                    lint_overrides: Vec::new(),
                    profile_compat: Vec::new(),
                    abi: None,
                }));
            }
        }
        Program {
            items,
            module_doc_comment,
            inner_attrs,
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
                let msg = self.unexpected_ident_msg("identifier");
                self.error(&msg);
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

    /// Build the "unexpected token" message for a position that expected an
    /// identifier / pattern / expression. When the offending token is a reserved
    /// keyword (`group`, `type`, `match`, …) the message names the keyword and
    /// says it is reserved, instead of printing the token's internal Rust `Debug`
    /// name — e.g. `let mut group = …` now reports `'group' is a reserved keyword
    /// and cannot be used as an identifier` rather than `Expected pattern, found
    /// Group` (B-2026-07-08-13). `expected` is the noun used in the non-keyword
    /// fallback ("identifier" / "pattern" / "expression").
    fn unexpected_ident_msg(&self, expected: &str) -> String {
        let tok = self.peek_token();
        match tok.keyword_spelling() {
            Some(kw) => {
                format!("'{kw}' is a reserved keyword and cannot be used as an identifier")
            }
            None => format!("Expected {expected}, found {tok:?}"),
        }
    }

    /// Like [`error`], but anchors the diagnostic at an explicit `span`
    /// rather than the current token — for cases where the offending
    /// construct was already consumed (e.g. a bad `extern "ABI"` string).
    fn error_at(&mut self, message: &str, span: Span) {
        self.errors.push(ParseError {
            message: message.to_string(),
            span,
        });
    }

    /// If `abi` names one of the calling conventions reserved-but-unimplemented
    /// at v1, emit the targeted "reserved" diagnostic at `span` and return
    /// `true`; otherwise return `false` so the caller applies its own
    /// valid/unknown-ABI handling. Recognizing the syntax now (rather than
    /// lumping it into the generic "unknown ABI" error) freezes it at v1 so a
    /// future implementation is not source-breaking — the same reservation
    /// posture as the `f16` / `bf16` numeric keywords. See the Phase-11
    /// embedded/hardware tracker and design.md § FFI.
    fn reserved_abi_diagnostic(&mut self, abi: &str, span: Span) -> bool {
        if RESERVED_FFI_ABIS.contains(&abi) {
            self.error_at(
                &format!(
                    "the `\"{abi}\"` calling convention is reserved for a future release \
                     and cannot be used yet — at v1 an `extern` ABI may only be `\"C\"` or \
                     `\"C-unwind\"`. (`\"stdcall\"`, `\"fastcall\"`, `\"win64\"`, `\"sysv64\"` \
                     are recognized but unimplemented; see design.md § FFI.)"
                ),
                span,
            );
            true
        } else {
            false
        }
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
                | Token::Union
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
                            crate::ast::GenericArg::Shape(s) => {
                                out.push('[');
                                for (j, _) in s.dims.iter().enumerate() {
                                    if j > 0 {
                                        out.push_str(", ");
                                    }
                                    out.push('_');
                                }
                                out.push(']');
                            }
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
                        crate::ast::GenericArg::Shape(s) => {
                            out.push('[');
                            for (j, _) in s.dims.iter().enumerate() {
                                if j > 0 {
                                    out.push_str(", ");
                                }
                                out.push('_');
                            }
                            out.push(']');
                        }
                    }
                }
                out.push(']');
            }
        }
        // `dyn Trait` slice 5 stub: render the surface form so the
        // anonymous-parameter `_: <type>` fix-it covers `dyn Trait`
        // signatures uniformly with the `impl Trait` shape above.
        TypeKind::Dyn {
            trait_path, args, ..
        } => {
            out.push_str("dyn ");
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
                        crate::ast::GenericArg::Shape(s) => {
                            out.push('[');
                            for (j, _) in s.dims.iter().enumerate() {
                                if j > 0 {
                                    out.push_str(", ");
                                }
                                out.push('_');
                            }
                            out.push(']');
                        }
                    }
                }
                out.push(']');
            }
        }
        TypeKind::Unit => out.push_str("()"),
        TypeKind::Error => out.push('_'),
    }
}
