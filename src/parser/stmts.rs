//! Block / statement parsing.
//!
//! Houses `parse_block` (the brace-delimited block grammar with the
//! statement / final-expression bifurcation), `parse_statement`
//! (the per-stmt dispatch — `let` / `defer` / `errdefer` / expression
//! / return / break / continue / compound-assign / regular assignment),
//! `parse_let_statement` (the `let [mut] pat (: type)? = value;` head),
//! plus the two special-form expressions parsed at statement-prefix
//! position (`providers { … }` and `offset_of!(…)`).
//!
//! Also houses the small statement-classification helpers
//! `is_statement_start`, `is_block_expr`, and `is_block_like_prefix`
//! (the last is a static helper called from exprs.rs).
//!
//! Lives in a sibling `impl super::Parser` block.

use crate::ast::*;
use crate::token::{Span, Token};

use super::ParseError;

impl super::Parser {
    // ── Blocks ───────────────────────────────────────────────────

    pub(crate) fn parse_block(&mut self) -> Option<Block> {
        let start = self.current_span();
        self.expect(&Token::LeftBrace)?;

        let mut stmts = Vec::new();
        let mut final_expr = None;

        while !self.check(&Token::RightBrace) && !self.is_at_end() {
            // `test "name" { … }` is a top-level item only. Catching it
            // here — before falling into the statement / expression
            // dispatch — replaces the generic two-tokens-without-operator
            // parse error with a focused `E_TEST_BLOCK_NOT_TOP_LEVEL`,
            // and lets us skip the misplaced case's body cleanly so
            // surrounding statements still parse. See design.md §
            // Testing and `Item::TestCase`.
            if self.is_test_block_head() {
                self.reject_test_block_in_body();
                continue;
            }
            // Try to parse a statement or final expression
            if self.is_statement_start() {
                match self.parse_statement() {
                    Some(stmt) => stmts.push(stmt),
                    None => {
                        self.synchronize_to_stmt();
                    }
                }
            } else {
                // Try to parse an expression
                match self.parse_expression_stmt() {
                    Some(expr) => {
                        if self.eat(&Token::Semicolon) {
                            // Expression statement
                            stmts.push(Stmt {
                                span: expr.span,
                                kind: StmtKind::Expr(expr),
                            });
                        } else if self.check(&Token::RightBrace) {
                            // Last item in block without semicolon
                            if self.is_block_expr(&expr) {
                                // Block-like expressions (while, for, loop, etc.)
                                // are statements that don't need semicolons
                                stmts.push(Stmt {
                                    span: expr.span,
                                    kind: StmtKind::Expr(expr),
                                });
                            } else {
                                // Value-producing expression (implicit return)
                                final_expr = Some(Box::new(expr));
                            }
                        } else if self.check(&Token::Comma) {
                            // Parallel / destructuring assignment:
                            // `t1, t2, ... = v1, v2, ...;`
                            stmts.push(self.finish_multi_assign(expr)?);
                        } else if self.eat(&Token::Equal) {
                            // Assignment: expr = value
                            let value = self.parse_expression()?;
                            self.expect(&Token::Semicolon)?;
                            let span = expr.span;
                            stmts.push(Stmt {
                                span,
                                kind: StmtKind::Assign {
                                    target: expr,
                                    value,
                                },
                            });
                        } else if let Some(cop) = self.try_compound_op() {
                            // Compound assignment: expr += value
                            let value = self.parse_expression()?;
                            self.expect(&Token::Semicolon)?;
                            let span = expr.span;
                            stmts.push(Stmt {
                                span,
                                kind: StmtKind::CompoundAssign {
                                    target: expr,
                                    op: cop,
                                    value,
                                },
                            });
                        } else if self.is_block_expr(&expr) {
                            // Block-like expressions (if, while, for, loop, match, unsafe)
                            // don't need semicolons when used as statements
                            stmts.push(Stmt {
                                span: expr.span,
                                kind: StmtKind::Expr(expr),
                            });
                        } else {
                            // Expression without semicolon and not at end
                            stmts.push(Stmt {
                                span: expr.span,
                                kind: StmtKind::Expr(expr),
                            });
                        }
                    }
                    None => {
                        self.synchronize_to_stmt();
                    }
                }
            }
        }

        self.expect(&Token::RightBrace)?;

        Some(Block {
            stmts,
            final_expr,
            span: self.span_from(&start),
        })
    }

    /// One top-level SCRIPT statement (script mode, design.md § Script mode):
    /// mirrors `parse_block`'s statement dispatch — `let`/`defer`/... via
    /// `parse_statement`, otherwise an expression statement with the same
    /// semicolon / assignment / compound-assignment / block-like handling —
    /// but terminated by EOF/next-item instead of `}` and with no
    /// final-expression semantics (the synthesized `fn main()` is unit; a
    /// trailing value expression is just a statement).
    pub(crate) fn parse_top_level_script_stmt(&mut self) -> Option<Stmt> {
        if self.is_statement_start() {
            return self.parse_statement();
        }
        let expr = self.parse_expression_stmt()?;
        if self.eat(&Token::Semicolon) {
            return Some(Stmt {
                span: expr.span,
                kind: StmtKind::Expr(expr),
            });
        }
        if self.check(&Token::Comma) {
            return self.finish_multi_assign(expr);
        }
        if self.eat(&Token::Equal) {
            let value = self.parse_expression()?;
            self.expect(&Token::Semicolon)?;
            let span = expr.span;
            return Some(Stmt {
                span,
                kind: StmtKind::Assign {
                    target: expr,
                    value,
                },
            });
        }
        if let Some(cop) = self.try_compound_op() {
            let value = self.parse_expression()?;
            self.expect(&Token::Semicolon)?;
            let span = expr.span;
            return Some(Stmt {
                span,
                kind: StmtKind::CompoundAssign {
                    target: expr,
                    op: cop,
                    value,
                },
            });
        }
        // Block-like expression (`if`/`while`/`match`/...) or a trailing
        // expression without a semicolon — both are statements here.
        Some(Stmt {
            span: expr.span,
            kind: StmtKind::Expr(expr),
        })
    }

    /// Parse a `providers { R => e, ... } in { body }` expression.
    /// Caller positions at the `providers` keyword. Resource keys are
    /// bare identifiers (Type-class; the case-class check is a later
    /// pass). Trailing comma is accepted. Empty binding lists are
    /// rejected — an empty `providers { } in { body }` is semantically
    /// equivalent to just `body` and almost certainly a typo.
    /// Parse `providers { R => e, ... } in { body }` — the keyword
    /// `providers` is contextual and has already been consumed by the
    /// caller (`parse_identifier_expr`'s "providers"-name dispatch).
    /// `start` is the span of the consumed keyword for fidelity in the
    /// resulting Expr.
    pub(crate) fn parse_providers_block(&mut self, start: Span) -> Option<Expr> {
        self.expect(&Token::LeftBrace)?;

        let mut bindings: Vec<crate::ast::ProviderBinding> = Vec::new();
        while !self.check(&Token::RightBrace) && !self.is_at_end() {
            let resource_span = self.current_span();
            let resource = self.expect_identifier()?;
            self.expect(&Token::FatArrow)?;
            let value = self.parse_expression()?;
            bindings.push(crate::ast::ProviderBinding {
                resource,
                resource_span,
                value,
            });
            if !self.eat(&Token::Comma) {
                break;
            }
        }
        self.expect(&Token::RightBrace)?;

        if bindings.is_empty() {
            self.error("`providers { ... } in { ... }` requires at least one binding");
            return None;
        }

        self.expect(&Token::In)?;
        let body = self.parse_block()?;

        Some(Expr {
            span: self.span_from(&start),
            kind: ExprKind::Providers { bindings, body },
        })
    }

    /// Parse the contextual special form `offset_of[T](field.path)`.
    /// `T` is a regular `TypeExpr` (so `offset_of[Vec[i64]](len)` works
    /// transparently); the paren contents are an identifier-only path
    /// `IDENT (. IDENT)*`. Any non-identifier expression form in the
    /// path position emits a focused diagnostic and returns `None`.
    /// See design.md § Field Offsets for the spec; the typechecker
    /// validates the path against `T`'s field set.
    pub(crate) fn parse_offset_of_special_form(&mut self, start: Span) -> Option<Expr> {
        self.expect(&Token::LeftBracket)?;
        let ty = self.parse_type()?;
        self.expect(&Token::RightBracket)?;
        self.expect(&Token::LeftParen)?;

        let mut field_path: Vec<String> = Vec::new();
        match self.peek_token_ref() {
            Token::Identifier { .. } => {
                field_path.push(self.expect_identifier()?);
            }
            _ => {
                self.error(
                    "error[E_OFFSET_OF_INVALID_PATH]: offset_of accepts a field-name path \
                     (e.g. `offset_of[T](field)` or `offset_of[T](inner.y)`); expression \
                     forms (literals, calls, indexing, dereferences) are not legal here",
                );
                return None;
            }
        }
        loop {
            match self.peek_token_ref() {
                Token::Dot => {
                    self.advance();
                    match self.peek_token_ref() {
                        Token::Identifier { .. } => {
                            field_path.push(self.expect_identifier()?);
                        }
                        _ => {
                            self.error(
                                "error[E_OFFSET_OF_INVALID_PATH]: each segment of the offset_of \
                                 field path must be a bare identifier; indexing, method calls, \
                                 and dereferences are not legal here",
                            );
                            return None;
                        }
                    }
                }
                Token::RightParen => break,
                // `field[0]` (indexing), `field()` (call), `*field` (deref),
                // and any other expression-form continuation are rejected
                // with a focused diagnostic. The generic "Expected
                // RightParen" message would point at the wrong intent.
                _ => {
                    self.error(
                        "error[E_OFFSET_OF_INVALID_PATH]: offset_of accepts a field-name path \
                         (e.g. `offset_of[T](field)` or `offset_of[T](inner.y)`); indexing, \
                         method calls, dereferences, and other expression forms are not legal \
                         here",
                    );
                    return None;
                }
            }
        }
        self.expect(&Token::RightParen)?;

        Some(Expr {
            span: self.span_from(&start),
            kind: ExprKind::OffsetOf { ty, field_path },
        })
    }

    fn is_statement_start(&self) -> bool {
        if matches!(self.peek_token_ref(), Token::Pound) {
            // An attribute block introduces a STATEMENT unless what follows it
            // is a loop, which keeps its existing expression-position path
            // (`#[par_order_free]`, and a trailing `for` needs no `;` — routing
            // it through the statement parser would demand one). Scanning past
            // the block is the only way to tell, so scan.
            return !matches!(
                self.token_after_attribute_block(),
                Token::While | Token::For | Token::Loop
            );
        }
        matches!(
            self.peek_token(),
            Token::Let | Token::Defer | Token::ErrDefer
        )
    }

    /// The first token after a run of `#[ … ]` attribute blocks at the cursor,
    /// found by counting bracket depth. Returns [`Token::EOF`] if the run is
    /// unterminated — a malformed attribute is the attribute parser's error to
    /// report, not this predicate's.
    fn token_after_attribute_block(&self) -> Token {
        let mut i = 0usize;
        loop {
            if !matches!(self.peek_token_ref_at(i), Token::Pound) {
                return self.peek_token_ref_at(i).clone();
            }
            i += 1; // past `#`
            if matches!(self.peek_token_ref_at(i), Token::Bang) {
                i += 1; // an inner attribute `#![ … ]`
            }
            if !matches!(self.peek_token_ref_at(i), Token::LeftBracket) {
                return self.peek_token_ref_at(i).clone();
            }
            let mut depth = 0usize;
            loop {
                match self.peek_token_ref_at(i) {
                    Token::LeftBracket => depth += 1,
                    Token::RightBracket => {
                        depth -= 1;
                        if depth == 0 {
                            i += 1;
                            break;
                        }
                    }
                    Token::EOF => return Token::EOF,
                    _ => {}
                }
                i += 1;
            }
        }
    }

    /// `test "..." { … }` head — module-scope item shape encountered
    /// inside a function body. Matches the same 3-token lookahead as
    /// the dispatcher in `parse_item`, so the rejection path here
    /// triggers exactly when the equivalent top-level form would
    /// produce an `Item::TestCase`.
    fn is_test_block_head(&self) -> bool {
        let Token::Identifier { name, .. } = self.peek_token() else {
            return false;
        };
        name == "test"
            && matches!(self.peek_token_ref_at(1), Token::StringLiteral(_))
            && matches!(self.peek_token_ref_at(2), Token::LeftBrace)
    }

    /// Emit `E_TEST_BLOCK_NOT_TOP_LEVEL` for a misplaced
    /// `test "name" { body }` and consume through the matching `}`
    /// so the enclosing block can keep parsing. The case body is
    /// dropped — slice 1 deliberately doesn't try to "rescue" it,
    /// since the misplacement signals either (a) a typo where the
    /// programmer meant a free-standing `assert_eq` call or (b) a
    /// case that was supposed to live at module scope; in both
    /// situations preserving the body adds noise downstream.
    fn reject_test_block_in_body(&mut self) {
        let start = self.current_span();
        // Consume `test`, the string literal, and `{`.
        self.advance();
        self.advance();
        self.advance();
        // Skip through to the matching `}` while balancing braces.
        let mut depth: usize = 1;
        while depth > 0 && !self.is_at_end() {
            match self.peek_token_ref() {
                Token::LeftBrace => {
                    depth += 1;
                    self.advance();
                }
                Token::RightBrace => {
                    depth -= 1;
                    self.advance();
                }
                _ => {
                    self.advance();
                }
            }
        }
        self.errors.push(super::ParseError {
            kind: crate::parser::ParseErrorKind::Syntax,
            message: "`test \"name\" { body }` declares a top-level test \
                      case and may not appear inside a function body. \
                      Move the case to module scope (any `_test.kara` \
                      file) or, if you meant a runtime assertion, drop \
                      the `test \"...\"` wrapper and call the assertion \
                      directly. (`E_TEST_BLOCK_NOT_TOP_LEVEL`)"
                .to_string(),
            span: self.span_from(&start),
        });
    }

    fn is_block_expr(&self, expr: &Expr) -> bool {
        // Block-like expressions that don't need semicolons when used as
        // statements, and are never a block's `final_expr`.
        //
        // `While` / `WhileLet` / `For` belong here because design.md gives
        // them type `()` unconditionally — there is no value to carry out, so
        // treating a trailing one as a statement loses nothing.
        //
        // `Loop` does NOT (B-2026-08-24-10). design.md § `loop` type
        // inference makes `loop` an EXPRESSION whose type is the LUB of its
        // `break` values, so a trailing `loop` is the block's value like any
        // other tail expression. Listing it here is what made
        // `fn pick() -> i64 { loop { break 30 } }` fall off the end of its own
        // body and yield `()` — and, because the typechecker then called the
        // loop `Never`, made codegen emit `unreachable` at the loop exit so
        // LLVM deleted the exit edge and the compiled binary hung.
        matches!(
            &expr.kind,
            ExprKind::While { .. } | ExprKind::WhileLet { .. } | ExprKind::For { .. }
        )
    }

    // Block-like expressions that, in statement context, terminate the
    // current statement at their closing `}`. The next token — even one
    // normally accepted as a postfix operator (`[`, `(`, `.`, `?`, `?.`)
    // — starts a fresh statement.
    //
    // Required so that `while cond { ... }` followed by `[1, 2]` on the
    // next line parses as two statements rather than as
    // `(while cond {...})[1, 2]`. To apply postfix to a block-like
    // expression in statement context, parenthesize:
    // `(if cond { v1 } else { v2 }).method()`.
    pub(crate) fn is_block_like_prefix(expr: &Expr) -> bool {
        matches!(
            &expr.kind,
            ExprKind::If { .. }
                | ExprKind::IfLet { .. }
                | ExprKind::Match { .. }
                | ExprKind::While { .. }
                | ExprKind::WhileLet { .. }
                | ExprKind::For { .. }
                | ExprKind::Loop { .. }
                | ExprKind::Block(_)
                | ExprKind::Unsafe(_)
                | ExprKind::Seq(_)
                | ExprKind::Par(_)
                | ExprKind::Lock { .. }
                | ExprKind::Providers { .. }
        )
    }

    // ── Statements ───────────────────────────────────────────────

    pub(crate) fn parse_statement(&mut self) -> Option<Stmt> {
        match self.peek_token_ref() {
            Token::Let => self.parse_let_statement(),
            Token::Defer => {
                let start = self.current_span();
                self.advance();
                let body = if self.check(&Token::LeftBrace) {
                    self.parse_block()?
                } else {
                    // defer expr;
                    let expr = self.parse_expression()?;
                    self.expect(&Token::Semicolon)?;
                    let span = expr.span;
                    Block {
                        stmts: vec![Stmt {
                            span,
                            kind: StmtKind::Expr(expr),
                        }],
                        final_expr: None,
                        span,
                    }
                };
                Some(Stmt {
                    span: self.span_from(&start),
                    kind: StmtKind::Defer { body },
                })
            }
            Token::ErrDefer => {
                let start = self.current_span();
                self.advance();
                // errdefer(e) { ... } — paren-delimited binding
                let binding = if self.check(&Token::LeftParen) {
                    self.advance();
                    let name = self.expect_identifier()?;
                    self.expect(&Token::RightParen)?;
                    Some(name)
                } else {
                    None
                };
                let body = if self.check(&Token::LeftBrace) {
                    self.parse_block()?
                } else {
                    // errdefer expr;
                    let expr = self.parse_expression()?;
                    self.expect(&Token::Semicolon)?;
                    let span = expr.span;
                    Block {
                        stmts: vec![Stmt {
                            span,
                            kind: StmtKind::Expr(expr),
                        }],
                        final_expr: None,
                        span,
                    }
                };
                Some(Stmt {
                    span: self.span_from(&start),
                    kind: StmtKind::ErrDefer { binding, body },
                })
            }
            // Statement-position attributes. `#[allow(lint)] stmt;` and
            // `#[expect(lint)] let x = …;` are documented lint surface —
            // design.md § Lint Level Attributes, and § Module-Level Bindings
            // calls `#[allow(module_mut_binding)]` "per-binding, not
            // module-wide" — and used to be a parse error, because the only
            // attribute handling below statement level lived in the EXPRESSION
            // prefix parser and accepted nothing but a loop
            // (B-2026-08-21-12). Loops still go there; everything else is
            // handled here.
            Token::Pound => self.parse_attributed_statement(),
            _ => self.parse_expression_statement(),
        }
    }

    /// The tail of [`Self::parse_statement`] — an expression, optionally
    /// followed by `=` / a compound operator / a comma list, then `;`.
    /// Extracted so the attribute arm can rewind into it when the attributes
    /// turn out to belong to a loop.
    fn parse_expression_statement(&mut self) -> Option<Stmt> {
        {
            {
                let expr = self.parse_expression()?;
                if self.check(&Token::Comma) {
                    // Parallel / destructuring assignment:
                    // `t1, t2, ... = v1, v2, ...;`
                    self.finish_multi_assign(expr)
                } else if self.eat(&Token::Equal) {
                    // Assignment
                    let value = self.parse_expression()?;
                    let span = expr.span;
                    self.expect(&Token::Semicolon)?;
                    Some(Stmt {
                        span,
                        kind: StmtKind::Assign {
                            target: expr,
                            value,
                        },
                    })
                } else if let Some(cop) = self.try_compound_op() {
                    // Compound assignment
                    let value = self.parse_expression()?;
                    let span = expr.span;
                    self.expect(&Token::Semicolon)?;
                    Some(Stmt {
                        span,
                        kind: StmtKind::CompoundAssign {
                            target: expr,
                            op: cop,
                            value,
                        },
                    })
                } else {
                    self.expect(&Token::Semicolon)?;
                    Some(Stmt {
                        span: expr.span,
                        kind: StmtKind::Expr(expr),
                    })
                }
            }
        }
    }

    /// Parse a statement introduced by an attribute block.
    ///
    /// Only LINT-LEVEL attributes (`allow` / `warn` / `deny` / `expect`) are
    /// meaningful here; they are scanned into
    /// [`crate::ast::Program::stmt_lint_overrides`], keyed by the statement's
    /// own span, and the typechecker pushes them as a frame around that one
    /// statement. Any other attribute in this position is rejected by name
    /// rather than silently dropped — a `#[inline]` on a statement means the
    /// writer expected something to happen, and nothing would.
    ///
    /// A loop keeps its existing expression-position path: `#[par_order_free]`
    /// and the loop-attribute surface are parsed there, so when the attribute
    /// block turns out to precede `while` / `for` / `loop` this rewinds and
    /// lets the expression parser read the same attributes again.
    fn parse_attributed_statement(&mut self) -> Option<Stmt> {
        let saved = self.pos;
        let attributes = self.parse_attributes();
        // Rewind into the expression path for anything it already diagnoses
        // better than a generic statement rejection would: a loop (which owns
        // `#[par_order_free]` and needs no `;`), a MISPLACED loop attribute
        // (`#[par_order_free] if …` keeps "expected `while`, `for`, or `loop`
        // after attribute block"), and a codegen hint (which owns
        // `E_CODEGEN_HINT_ON_CLOSURE`). Duplicating those messages here would
        // be two places to keep in step.
        let defer_to_expression_path = matches!(
            self.peek_token_ref(),
            Token::While | Token::For | Token::Loop
        ) || attributes
            .iter()
            .any(|a| a.is_par_order_free() || a.codegen_hint_name().is_some());
        if defer_to_expression_path {
            self.pos = saved;
            return self.parse_expression_statement();
        }

        let overrides = self.scan_lint_level_attrs(&attributes);
        for attr in &attributes {
            let is_lint_level = attr.path.len() == 1
                && crate::lints::LintLevel::from_attr_name(&attr.path[0]).is_some();
            if !is_lint_level {
                let name = attr.path.join("::");
                self.error_at(
                    &format!(
                        "error[E_ATTRIBUTE_NOT_VALID_ON_STATEMENT]: `#[{name}]` cannot \
                         apply to a statement — only the lint-level attributes \
                         `#[allow]`, `#[warn]`, `#[deny]` and `#[expect]` are \
                         recognised here. Move it onto the enclosing item if it \
                         belongs there."
                    ),
                    attr.span,
                );
            }
        }

        let stmt = self.parse_statement()?;
        if !overrides.is_empty() {
            self.stmt_lint_overrides
                .insert(crate::resolver::SpanKey::from_span(&stmt.span), overrides);
        }
        Some(stmt)
    }

    /// Finish a parallel / destructuring assignment
    /// `t1, t2, ... = v1, v2, ...;`, entered with the first target `first`
    /// already parsed and the cursor sitting on the first comma.
    ///
    /// Produces a [`StmtKind::MultiAssign`] node. The [`crate::desugar`] pass
    /// rewrites it (before resolve) into a block-expr of `let`-temps + single
    /// `Assign`s that evaluates every right-hand value left-to-right before
    /// writing any target — so `a, b = b, a` swaps. Keeping the surface node
    /// lets the formatter round-trip the comma syntax verbatim.
    fn finish_multi_assign(&mut self, first: Expr) -> Option<Stmt> {
        let start = first.span;
        let mut targets = vec![first];
        while self.eat(&Token::Comma) {
            targets.push(self.parse_expression()?);
        }
        self.expect(&Token::Equal)?;
        let mut values = vec![self.parse_expression()?];
        while self.eat(&Token::Comma) {
            values.push(self.parse_expression()?);
        }
        self.expect(&Token::Semicolon)?;
        let span = self.span_from(&start);
        if targets.len() != values.len() {
            self.error_at(
                &format!(
                    "parallel assignment has {} target(s) but {} value(s); both sides must list the same number of elements",
                    targets.len(),
                    values.len()
                ),
                span,
            );
            return None;
        }
        Some(Stmt {
            span,
            kind: StmtKind::MultiAssign { targets, values },
        })
    }

    fn parse_let_statement(&mut self) -> Option<Stmt> {
        let start = self.current_span();
        self.expect(&Token::Let)?;

        let is_mut = self.eat(&Token::Mut);
        let pattern = self.parse_pattern()?;

        let ty = if self.eat(&Token::Colon) {
            Some(self.parse_type()?)
        } else {
            None
        };

        // Uninitialized form: `let pat: T;` (no `=` initializer).
        // Requires a type annotation (no RHS to infer from) and a single-name
        // pattern (destructuring needs a value). Definite-assignment analysis
        // tracks initialization through later assignments.
        if self.check(&Token::Semicolon) {
            self.advance();
            let Some(ty) = ty else {
                self.errors.push(ParseError {
                    kind: crate::parser::ParseErrorKind::Syntax,
                    message: "uninitialized `let` requires a type annotation; write `let x: T;` (or supply an initializer with `= ...`)"
                        .to_string(),
                    span: self.span_from(&start),
                });
                return None;
            };
            let (name, name_span) = match &pattern.kind {
                PatternKind::Binding(name) => (name.clone(), pattern.span),
                _ => {
                    self.errors.push(ParseError {
                        kind: crate::parser::ParseErrorKind::Syntax,
                        message: "uninitialized `let` must bind a single name; destructuring patterns require an initializer"
                            .to_string(),
                        span: pattern.span,
                    });
                    return None;
                }
            };
            return Some(Stmt {
                span: self.span_from(&start),
                kind: StmtKind::LetUninit {
                    is_mut,
                    name,
                    name_span,
                    ty,
                },
            });
        }

        self.expect(&Token::Equal)?;
        // `freeze <place>` — B-2026-08-01-33 mechanism 3, stage 3. A
        // CONTEXTUAL keyword recognized in exactly one position (a `let`
        // initializer), so `freeze` stays a legal identifier and an existing
        // program using the name cannot break. Only when the NEXT token can
        // begin a place, mirroring the `frozen T` guard.
        let freeze_kw = matches!(self.peek_token_ref(), Token::Identifier { name, .. } if name == "freeze")
            && matches!(
                self.peek_token_at(1),
                Token::Identifier { .. } | Token::SelfValue
            );
        if freeze_kw {
            self.advance();
        }
        let value = self.parse_expression()?;
        if freeze_kw {
            // Recorded against the FROZEN PLACE's span, which is the span the
            // ownership pass and codegen both already key on for an alias
            // binding — so the statement reuses that channel end to end.
            self.freeze_spans
                .insert(crate::resolver::SpanKey::from_span(&value.span));
        }

        // let ... else { diverging_block }
        if self.eat(&Token::Else) {
            let else_block = self.parse_block()?;
            // The trailing `;` is OPTIONAL, and that is a deliberate
            // under-commitment rather than the obvious reading of the grammar.
            //
            // syntax.md § Statements writes it as required —
            //   LET_ELSE_STATEMENT = "let" [ "mut" ] PATTERN "=" EXPR "else" BLOCK ";"
            // — and design.md's `if let` / `let...else` section ends every
            // example with `};`. The parser used to stop at the closing brace,
            // so the spelling BOTH documents use failed with `Expected
            // expression, found Semicolon` while the undocumented one compiled:
            // the feature was implemented and unreachable as written down
            // (B-2026-08-21-12, found by the design.md conformance sweep).
            //
            // Requiring it would match the grammar exactly, and it would also
            // reject the form this compiler's own tests are written in
            // (`tests/interpreter.rs::test_let_else_match_and_diverge`,
            // `tests/codegen.rs::test_e2e_let_else_binds_then_else_diverges`).
            // That is a language decision about whether a block-tailed binding
            // takes a terminator, not a defect, so it is left to the spec owner:
            // accepting both spellings removes the divergence without deciding
            // it. Tightening to required is a one-word change here plus those
            // two tests.
            self.eat(&Token::Semicolon);
            return Some(Stmt {
                span: self.span_from(&start),
                kind: StmtKind::LetElse {
                    pattern,
                    ty,
                    value,
                    else_block,
                },
            });
        }

        self.expect(&Token::Semicolon)?;

        Some(Stmt {
            span: self.span_from(&start),
            kind: StmtKind::Let {
                is_mut,
                pattern,
                ty,
                value,
            },
        })
    }
}
