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
                                span: expr.span.clone(),
                                kind: StmtKind::Expr(expr),
                            });
                        } else if self.check(&Token::RightBrace) {
                            // Last item in block without semicolon
                            if self.is_block_expr(&expr) {
                                // Block-like expressions (while, for, loop, etc.)
                                // are statements that don't need semicolons
                                stmts.push(Stmt {
                                    span: expr.span.clone(),
                                    kind: StmtKind::Expr(expr),
                                });
                            } else {
                                // Value-producing expression (implicit return)
                                final_expr = Some(Box::new(expr));
                            }
                        } else if self.eat(&Token::Equal) {
                            // Assignment: expr = value
                            let value = self.parse_expression()?;
                            self.expect(&Token::Semicolon)?;
                            let span = expr.span.clone();
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
                            let span = expr.span.clone();
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
                                span: expr.span.clone(),
                                kind: StmtKind::Expr(expr),
                            });
                        } else {
                            // Expression without semicolon and not at end
                            stmts.push(Stmt {
                                span: expr.span.clone(),
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
        match self.peek_token() {
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
            match self.peek_token() {
                Token::Dot => {
                    self.advance();
                    match self.peek_token() {
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
        matches!(
            self.peek_token(),
            Token::Let | Token::Defer | Token::ErrDefer
        )
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
            && matches!(self.peek_token_at(1), Token::StringLiteral(_))
            && matches!(self.peek_token_at(2), Token::LeftBrace)
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
            match self.peek_token() {
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
        // Block-like expressions that don't need semicolons when used as statements.
        // These are always treated as statements (not final_expr / implicit return).
        // For `loop` with break-value, use `let x = loop { ... };` pattern.
        matches!(
            &expr.kind,
            ExprKind::While { .. }
                | ExprKind::WhileLet { .. }
                | ExprKind::For { .. }
                | ExprKind::Loop { .. }
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

    fn parse_statement(&mut self) -> Option<Stmt> {
        match self.peek_token() {
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
                    let span = expr.span.clone();
                    Block {
                        stmts: vec![Stmt {
                            span: span.clone(),
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
                    let span = expr.span.clone();
                    Block {
                        stmts: vec![Stmt {
                            span: span.clone(),
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
            _ => {
                let expr = self.parse_expression()?;
                if self.eat(&Token::Equal) {
                    // Assignment
                    let value = self.parse_expression()?;
                    let span = expr.span.clone();
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
                    let span = expr.span.clone();
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
                        span: expr.span.clone(),
                        kind: StmtKind::Expr(expr),
                    })
                }
            }
        }
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
                    message: "uninitialized `let` requires a type annotation; write `let x: T;` (or supply an initializer with `= ...`)"
                        .to_string(),
                    span: self.span_from(&start),
                });
                return None;
            };
            let (name, name_span) = match &pattern.kind {
                PatternKind::Binding(name) => (name.clone(), pattern.span.clone()),
                _ => {
                    self.errors.push(ParseError {
                        message: "uninitialized `let` must bind a single name; destructuring patterns require an initializer"
                            .to_string(),
                        span: pattern.span.clone(),
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
        let value = self.parse_expression()?;

        // let ... else { diverging_block }
        if self.eat(&Token::Else) {
            let else_block = self.parse_block()?;
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
