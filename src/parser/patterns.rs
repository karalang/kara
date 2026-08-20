//! Pattern parsing — refutable + irrefutable forms for `let`, `match`,
//! function parameters, `if let`, `while let`, and `for` heads.
//!
//! Houses `parse_pattern` (the alternation `p1 | p2 | …` wrapper),
//! `parse_single_pattern` (the big PatternKind dispatch: identifier
//! bindings, literals + ranges, tuple / struct / enum-variant
//! destructuring, slice patterns with `..` rest, wildcards, etc.),
//! plus the pattern-literal helpers (`starts_literal_pattern` /
//! `parse_literal_pattern`) used to disambiguate the range-pattern
//! end position.
//!
//! Lives in a sibling `impl super::Parser` block.

use crate::ast::*;
use crate::lexer::IdentClass;
use crate::token::{IntSuffix, Token};

use super::{starts_upper, ParseError};

impl super::Parser {
    // ── Patterns ─────────────────────────────────────────────────

    // Depth-guarded shell: pattern nesting (tuple / variant destructuring)
    // shares the recursion budget with expressions and types
    // (B-2026-08-16-4).
    pub(crate) fn parse_pattern(&mut self) -> Option<Pattern> {
        if !self.enter_recursion() {
            return None;
        }
        // Red-zone stack growth — see parse_expr_bp_with_ctx for sizing.
        let result =
            stacker::maybe_grow(512 * 1024, 16 * 1024 * 1024, || self.parse_pattern_inner());
        self.exit_recursion();
        result
    }

    fn parse_pattern_inner(&mut self) -> Option<Pattern> {
        let start = self.current_span();
        let first = self.parse_single_pattern()?;

        // Check for or-pattern: A | B | C
        if self.check(&Token::Pipe) {
            let mut alternatives = vec![first];
            while self.eat(&Token::Pipe) {
                alternatives.push(self.parse_single_pattern()?);
            }
            return Some(Pattern {
                kind: PatternKind::Or(alternatives),
                span: self.span_from(&start),
            });
        }

        Some(first)
    }

    pub(crate) fn parse_single_pattern(&mut self) -> Option<Pattern> {
        let start = self.current_span();

        match self.peek_token_ref() {
            Token::Underscore => {
                self.advance();
                Some(Pattern {
                    kind: PatternKind::Wildcard,
                    span: self.span_from(&start),
                })
            }
            // `ref name @ PATTERN` — the only position where `ref` is
            // legal inside a pattern (design.md § @ Bindings, "Explicit
            // `ref` on the `@` binding"). Per-binding `ref` annotations
            // elsewhere don't exist in Kāra — binding modes flow from
            // the scrutinee type (design.md § Match Arm Binding Modes).
            Token::Ref => {
                self.advance();
                let name = self.expect_identifier()?;
                let name_span = self.span_from(&start);
                if !self.eat(&Token::At) {
                    self.errors.push(ParseError {
                        kind: crate::parser::ParseErrorKind::Syntax,
                        message: format!(
                            "'ref' in a pattern is only valid on an '@' binding \
                             ('ref {name} @ PATTERN'); binding modes otherwise \
                             follow the scrutinee type"
                        ),
                        span: name_span,
                    });
                    return None;
                }
                self.check_ident_class(&name, IdentClass::Value, "binding", name_span);
                let sub_pattern = self.parse_single_pattern()?;
                Some(Pattern {
                    kind: PatternKind::AtBinding {
                        name,
                        pattern: Box::new(sub_pattern),
                        by_ref: true,
                    },
                    span: self.span_from(&start),
                })
            }
            // Half-open range patterns with a missing start: `..lit` and `..=lit`.
            // Bare `..` is not a valid pattern (use `_` for wildcard).
            Token::DotDot => {
                self.advance();
                let end = self.parse_range_bound()?;
                Some(Pattern {
                    kind: PatternKind::RangePattern {
                        start: None,
                        end: Some(end),
                        inclusive: false,
                    },
                    span: self.span_from(&start),
                })
            }
            Token::DotDotEq => {
                self.advance();
                let end = self.parse_range_bound()?;
                Some(Pattern {
                    kind: PatternKind::RangePattern {
                        start: None,
                        end: Some(end),
                        inclusive: true,
                    },
                    span: self.span_from(&start),
                })
            }
            Token::True => {
                self.advance();
                Some(Pattern {
                    kind: PatternKind::Literal(LiteralPattern::Bool(true)),
                    span: self.span_from(&start),
                })
            }
            Token::False => {
                self.advance();
                Some(Pattern {
                    kind: PatternKind::Literal(LiteralPattern::Bool(false)),
                    span: self.span_from(&start),
                })
            }
            // B-2026-08-20-1 — an UPPER-HALF unsigned magnitude
            // (`18446744073709551615u64`) arrives as `IntegerOutOfRange`, and
            // the pattern parser had no arm for it, so the whole top half of
            // every unsigned range was unmatchable by literal while the same
            // literal in EXPRESSION position compiled fine. It rides the signed
            // carrier as the identical wrapped bit pattern `parser/exprs.rs`
            // gives it, so pattern and scrutinee compare as the same bits.
            &Token::IntegerOutOfRange(m, sfx) => {
                self.advance();
                let Some(v) = Self::pattern_int_on_carrier(m, sfx) else {
                    self.error("integer literal is out of range for its suffix in a pattern");
                    return None;
                };
                self.finish_integer_pattern(v, sfx, start)
            }
            &Token::Integer(n, sfx) => {
                self.advance();
                self.finish_integer_pattern(n, sfx, start)
            }
            // Byte literals (`b'I'`) are u8 integers — desugar to an
            // integer pattern with a U8 suffix so the whole Integer
            // pattern pipeline (typecheck / codegen / exhaustiveness /
            // ranges) handles them with no new LiteralPattern variant.
            // `b'I'` and `73u8` are then identical in pattern position.
            &Token::ByteLiteral(b) => {
                self.advance();
                let lit = LiteralPattern::Integer(b as i128, Some(IntSuffix::U8));
                // Range pattern: `b'a'..=b'z'` or `b'a'..`
                if self.eat(&Token::DotDotEq) {
                    let end = self.parse_range_bound()?;
                    return Some(Pattern {
                        kind: PatternKind::RangePattern {
                            start: Some(RangeBound::Literal(lit)),
                            end: Some(end),
                            inclusive: true,
                        },
                        span: self.span_from(&start),
                    });
                }
                if self.eat(&Token::DotDot) {
                    let end = if Self::starts_range_bound(self.peek_token_ref()) {
                        Some(self.parse_range_bound()?)
                    } else {
                        None
                    };
                    return Some(Pattern {
                        kind: PatternKind::RangePattern {
                            start: Some(RangeBound::Literal(lit)),
                            end,
                            inclusive: false,
                        },
                        span: self.span_from(&start),
                    });
                }
                Some(Pattern {
                    kind: PatternKind::Literal(lit),
                    span: self.span_from(&start),
                })
            }
            &Token::Float(n, sfx) => {
                self.advance();
                Some(Pattern {
                    kind: PatternKind::Literal(LiteralPattern::Float(n, sfx)),
                    span: self.span_from(&start),
                })
            }
            Token::StringLiteral(s) => {
                let s = s.clone();
                self.advance();
                Some(Pattern {
                    kind: PatternKind::Literal(LiteralPattern::String(s)),
                    span: self.span_from(&start),
                })
            }
            &Token::CharLiteral(c) => {
                self.advance();
                let lit = LiteralPattern::Char(c);
                // Check for range pattern: `'a'..='z'` or `'a'..`
                if self.eat(&Token::DotDotEq) {
                    let end = self.parse_range_bound()?;
                    return Some(Pattern {
                        kind: PatternKind::RangePattern {
                            start: Some(RangeBound::Literal(lit)),
                            end: Some(end),
                            inclusive: true,
                        },
                        span: self.span_from(&start),
                    });
                }
                if self.eat(&Token::DotDot) {
                    // `'a'..'z'` (bounded exclusive) when the next token
                    // is a literal or const path; `'a'..` (half-open) otherwise.
                    let end = if Self::starts_range_bound(self.peek_token_ref()) {
                        Some(self.parse_range_bound()?)
                    } else {
                        None
                    };
                    return Some(Pattern {
                        kind: PatternKind::RangePattern {
                            start: Some(RangeBound::Literal(lit)),
                            end,
                            inclusive: false,
                        },
                        span: self.span_from(&start),
                    });
                }
                Some(Pattern {
                    kind: PatternKind::Literal(lit),
                    span: self.span_from(&start),
                })
            }
            Token::LeftParen => {
                // Parenthesized pattern: a tuple `(A, B, …)`, a 1-tuple `(P,)`,
                // or — with no trailing comma and a single element — a GROUPING
                // `(P)` that simply yields `P`. The grouping form matters for
                // or-patterns: `x @ (1 | 2 | 3)` (and top-level `(1 | 2)`) must
                // parse as the inner `Or`, not a 1-element `Tuple([Or(...)])` —
                // the latter can never match a scalar scrutinee, so codegen fell
                // to its always-true tuple-on-non-aggregate branch (over-match)
                // and the interpreter never matched (under-match). Mirrors Rust:
                // `(P)` is grouping, `(P,)` is a 1-tuple. (B-2026-07-23-19.)
                self.advance();
                let mut patterns = Vec::new();
                let mut trailing_comma = false;
                while !self.check(&Token::RightParen) && !self.is_at_end() {
                    patterns.push(self.parse_pattern()?);
                    if self.eat(&Token::Comma) {
                        // A comma keeps the tuple reading; whether it is a
                        // *trailing* comma is decided by what follows (another
                        // element resets this on the next iteration).
                        trailing_comma = true;
                    } else {
                        trailing_comma = false;
                        break;
                    }
                }
                self.expect(&Token::RightParen)?;
                // `(P)` — exactly one element, no trailing comma — is a grouping.
                if patterns.len() == 1 && !trailing_comma {
                    return patterns.into_iter().next();
                }
                Some(Pattern {
                    kind: PatternKind::Tuple(patterns),
                    span: self.span_from(&start),
                })
            }
            Token::LeftBracket => {
                // Slice/array pattern: `[p1, p2, ..rest, p_n-1, p_n]`.
                // Sub-item 1 of the slice/array-patterns entry (phase 5.2):
                // parser produces the variant; typechecker emits a stub
                // diagnostic until sub-item 2 lands.
                self.advance();
                let mut prefix: Vec<Pattern> = Vec::new();
                let mut suffix: Vec<Pattern> = Vec::new();
                let mut rest: Option<RestPattern> = None;
                while !self.check(&Token::RightBracket) && !self.is_at_end() {
                    if self.check(&Token::DotDot) {
                        let rest_span = self.current_span();
                        self.advance();
                        let new_rest = if let Token::Identifier { .. } = self.peek_token_ref() {
                            let name = self.expect_identifier()?;
                            self.check_ident_class(&name, IdentClass::Value, "binding", rest_span);
                            RestPattern::Bound(name)
                        } else {
                            RestPattern::Ignored
                        };
                        if rest.is_some() {
                            // Recovery: keep the first rest marker; later
                            // elements continue collecting into `suffix`.
                            self.errors.push(ParseError {
                                kind: crate::parser::ParseErrorKind::Syntax,
                                message:
                                    "slice pattern may have at most one `..` marker; remove the extras"
                                        .to_string(),
                                span: rest_span,
                            });
                        } else {
                            rest = Some(new_rest);
                        }
                    } else {
                        let pat = self.parse_pattern()?;
                        if rest.is_none() {
                            prefix.push(pat);
                        } else {
                            suffix.push(pat);
                        }
                    }
                    if !self.eat(&Token::Comma) {
                        break;
                    }
                }
                self.expect(&Token::RightBracket)?;
                Some(Pattern {
                    kind: PatternKind::Slice {
                        prefix,
                        rest,
                        suffix,
                    },
                    span: self.span_from(&start),
                })
            }
            Token::Identifier { .. } => {
                let name = self.expect_identifier()?;

                // Check for @ binding: name @ pattern
                if self.eat(&Token::At) {
                    let name_span = self.span_from(&start);
                    self.check_ident_class(&name, IdentClass::Value, "binding", name_span);
                    let sub_pattern = self.parse_single_pattern()?;
                    return Some(Pattern {
                        kind: PatternKind::AtBinding {
                            name,
                            pattern: Box::new(sub_pattern),
                            by_ref: false,
                        },
                        span: self.span_from(&start),
                    });
                }

                // Range pattern with a const-path START bound:
                // `MAX_AGE..`, `MIN..=MAX`, `LO..hi`. A bare identifier
                // followed by `..`/`..=` is only ever a range start.
                if matches!(self.peek_token_ref(), Token::DotDot | Token::DotDotEq) {
                    let name_span = self.span_from(&start);
                    let inclusive = matches!(self.peek_token_ref(), Token::DotDotEq);
                    self.advance();
                    let start_bound = RangeBound::Path {
                        segments: vec![name],
                        span: name_span,
                    };
                    // `..=` requires an end; `..` accepts an optional end.
                    let end = if inclusive || Self::starts_range_bound(self.peek_token_ref()) {
                        Some(self.parse_range_bound()?)
                    } else {
                        None
                    };
                    return Some(Pattern {
                        kind: PatternKind::RangePattern {
                            start: Some(start_bound),
                            end,
                            inclusive,
                        },
                        span: self.span_from(&start),
                    });
                }

                // Check for struct destructure: Name { ... }
                if self.check(&Token::LeftBrace) {
                    self.advance();
                    let (fields, has_rest) = self.parse_struct_pattern_fields()?;
                    self.expect(&Token::RightBrace)?;
                    Some(Pattern {
                        kind: PatternKind::Struct {
                            path: vec![name],
                            fields,
                            has_rest,
                        },
                        span: self.span_from(&start),
                    })
                }
                // Check for tuple variant: Name(...)
                else if self.check(&Token::LeftParen) {
                    self.advance();
                    let mut patterns = Vec::new();
                    while !self.check(&Token::RightParen) && !self.is_at_end() {
                        patterns.push(self.parse_pattern()?);
                        if !self.eat(&Token::Comma) {
                            break;
                        }
                    }
                    self.expect(&Token::RightParen)?;
                    Some(Pattern {
                        kind: PatternKind::TupleVariant {
                            path: vec![name],
                            patterns,
                        },
                        span: self.span_from(&start),
                    })
                }
                // Check for qualified path: Name.Variant ...
                // Only Type/Const-class idents (uppercase leading) root a path in
                // pattern position; lowercase is always a plain binding.
                else if self.check(&Token::Dot) && starts_upper(&name) {
                    let mut path = vec![name];
                    while self.eat(&Token::Dot) {
                        path.push(self.expect_identifier()?);
                    }
                    // Range pattern with a qualified const-path START bound:
                    // `Limits.HIGH..=other`.
                    if matches!(self.peek_token_ref(), Token::DotDot | Token::DotDotEq) {
                        let path_span = self.span_from(&start);
                        let inclusive = matches!(self.peek_token_ref(), Token::DotDotEq);
                        self.advance();
                        let start_bound = RangeBound::Path {
                            segments: path,
                            span: path_span,
                        };
                        let end = if inclusive || Self::starts_range_bound(self.peek_token_ref()) {
                            Some(self.parse_range_bound()?)
                        } else {
                            None
                        };
                        return Some(Pattern {
                            kind: PatternKind::RangePattern {
                                start: Some(start_bound),
                                end,
                                inclusive,
                            },
                            span: self.span_from(&start),
                        });
                    }
                    // Check for struct or tuple variant
                    if self.check(&Token::LeftBrace) {
                        self.advance();
                        let (fields, has_rest) = self.parse_struct_pattern_fields()?;
                        self.expect(&Token::RightBrace)?;
                        Some(Pattern {
                            kind: PatternKind::Struct {
                                path,
                                fields,
                                has_rest,
                            },
                            span: self.span_from(&start),
                        })
                    } else if self.check(&Token::LeftParen) {
                        self.advance();
                        let mut patterns = Vec::new();
                        while !self.check(&Token::RightParen) && !self.is_at_end() {
                            patterns.push(self.parse_pattern()?);
                            if !self.eat(&Token::Comma) {
                                break;
                            }
                        }
                        self.expect(&Token::RightParen)?;
                        Some(Pattern {
                            kind: PatternKind::TupleVariant { path, patterns },
                            span: self.span_from(&start),
                        })
                    } else {
                        // Just a binding with a qualified name (unit variant)
                        Some(Pattern {
                            kind: PatternKind::Binding(path.join(".")),
                            span: self.span_from(&start),
                        })
                    }
                } else {
                    // Simple binding — may also be a unit variant reference (e.g. `None`
                    // in a match arm). The resolver distinguishes the two cases; skip the
                    // naming check here to avoid false positives on valid variant patterns.
                    Some(Pattern {
                        kind: PatternKind::Binding(name),
                        span: self.span_from(&start),
                    })
                }
            }
            _ => {
                self.error_unexpected_ident("pattern");
                None
            }
        }
    }
    // ── Pattern Helpers ───────────────────────────────────────────

    /// Parse a literal for use in range patterns (integer or char).
    /// True when `tok` starts a literal pattern (integer or char). Used
    /// by the range-pattern parser to disambiguate the bounded-exclusive
    /// form `lo..hi` from the half-open form `lo..` — only the former
    /// has a literal in end position.
    fn starts_literal_pattern(tok: &Token) -> bool {
        matches!(
            tok,
            Token::Integer(..)
                | Token::IntegerOutOfRange(..)
                | Token::CharLiteral(_)
                | Token::ByteLiteral(_)
        )
    }

    /// True when `tok` can begin a range-pattern bound — a literal (above)
    /// or an identifier rooting a const path (`MAX_AGE`, `Limits.HIGH`).
    /// Used to disambiguate the half-open form `lo..` from the bounded
    /// form `lo..hi` when `hi` may be a const path, not just a literal.
    fn starts_range_bound(tok: &Token) -> bool {
        Self::starts_literal_pattern(tok) || matches!(tok, Token::Identifier { .. })
    }

    /// Parse one range-pattern bound: a literal or a path to a
    /// module-level const (`MIN_AGE`, `Limits.HIGH`). Anything else is
    /// rejected with `E_RANGE_PATTERN_BOUND_NOT_SIMPLE` (slice 7 — the
    /// grammar limits the shape upfront; const resolution + ordering /
    /// type checks happen at typecheck). The path's const-ness is NOT
    /// verified here — a non-const path resolves to
    /// `E_RANGE_PATTERN_BOUND_NOT_CONST` at typecheck.
    fn parse_range_bound(&mut self) -> Option<RangeBound> {
        let bound_start = self.current_span();
        if let Token::Identifier { .. } = self.peek_token_ref() {
            let name = self.expect_identifier()?;
            let mut segments = vec![name];
            while self.eat(&Token::Dot) {
                segments.push(self.expect_identifier()?);
            }
            return Some(RangeBound::Path {
                segments,
                span: self.span_from(&bound_start),
            });
        }
        if Self::starts_literal_pattern(self.peek_token_ref()) {
            return Some(RangeBound::Literal(self.parse_literal_pattern()?));
        }
        self.errors.push(ParseError {
            kind: crate::parser::ParseErrorKind::Syntax,
            message: "error[E_RANGE_PATTERN_BOUND_NOT_SIMPLE]: range pattern bound must be \
                      a literal or a path to a module-level const; arbitrary expressions are \
                      not accepted at pattern position"
                .to_string(),
            span: bound_start,
        });
        None
    }

    /// Parse the field list of a struct pattern between `{` and `}`.
    /// The caller consumes the opening `{`; this helper stops at the
    /// closing `}` without consuming it. Returns the field patterns
    /// plus a `has_rest` flag set to `true` when a `..` rest marker
    /// appears in the field list.
    ///
    /// Grammar accepted:
    ///   `{ field (, field)* (, ..)? ,? }`
    ///   `{ .. }`
    ///   `{ }`
    ///
    /// The `..` may only appear once and must be the last item before
    /// `}` (Rust's rule; the spec follows). A bare `..` in struct
    /// pattern is the canonical "I don't care about other fields"
    /// shape; combined with field patterns it means "match these
    /// fields, ignore the rest". A `..` followed by another field
    /// emits `E_REST_PATTERN_NOT_LAST`.
    fn parse_struct_pattern_fields(&mut self) -> Option<(Vec<FieldPattern>, bool)> {
        let mut fields = Vec::new();
        let mut has_rest = false;
        while !self.check(&Token::RightBrace) && !self.is_at_end() {
            if self.check(&Token::DotDot) {
                let dotdot_span = self.current_span();
                self.advance();
                if has_rest {
                    self.errors.push(super::ParseError {
                        kind: crate::parser::ParseErrorKind::Syntax,
                        message: "error[E_REST_PATTERN_DUPLICATE]: \
                                  `..` rest-pattern appears more than once in \
                                  the same struct pattern — only one is permitted"
                            .to_string(),
                        span: dotdot_span,
                    });
                }
                has_rest = true;
                // Optional trailing comma is fine; another field after
                // the `..` is not.
                if self.eat(&Token::Comma) && !self.check(&Token::RightBrace) {
                    self.errors.push(super::ParseError {
                        kind: crate::parser::ParseErrorKind::Syntax,
                        message: "error[E_REST_PATTERN_NOT_LAST]: \
                                  `..` rest-pattern must appear last in the \
                                  struct pattern's field list — move it after \
                                  every named field, or drop the named fields \
                                  that follow it"
                            .to_string(),
                        span: self.current_span(),
                    });
                    // Continue parsing to surface follow-on errors
                    // rather than bailing immediately.
                }
                continue;
            }
            let fs = self.current_span();
            let field_name = self.expect_identifier()?;
            let pattern = if self.eat(&Token::Colon) {
                Some(self.parse_pattern()?)
            } else {
                None
            };
            fields.push(FieldPattern {
                name: field_name,
                pattern,
                span: self.span_from(&fs),
            });
            if !self.eat(&Token::Comma) {
                break;
            }
        }
        Some((fields, has_rest))
    }

    /// The tail shared by both integer-literal pattern arms: a bare literal,
    /// or the start of a `lo..=hi` / `lo..hi` / `lo..` range.
    fn finish_integer_pattern(
        &mut self,
        v: i128,
        sfx: Option<IntSuffix>,
        start: crate::token::Span,
    ) -> Option<Pattern> {
        let lit = LiteralPattern::Integer(v, sfx);
        if self.eat(&Token::DotDotEq) {
            let end = self.parse_range_bound()?;
            return Some(Pattern {
                kind: PatternKind::RangePattern {
                    start: Some(RangeBound::Literal(lit)),
                    end: Some(end),
                    inclusive: true,
                },
                span: self.span_from(&start),
            });
        }
        if self.eat(&Token::DotDot) {
            // `lo..hi` (bounded exclusive) when the next token is a literal or
            // const path; `lo..` (half-open) otherwise.
            let end = if Self::starts_range_bound(self.peek_token_ref()) {
                Some(self.parse_range_bound()?)
            } else {
                None
            };
            return Some(Pattern {
                kind: PatternKind::RangePattern {
                    start: Some(RangeBound::Literal(lit)),
                    end,
                    inclusive: false,
                },
                span: self.span_from(&start),
            });
        }
        Some(Pattern {
            kind: PatternKind::Literal(lit),
            span: self.span_from(&start),
        })
    }

    /// An out-of-range UNSIGNED magnitude on the `i64` carrier that
    /// `LiteralPattern::Integer` provides. Widths of 64 bits or narrower wrap
    /// to the same bit pattern `parser/exprs.rs` produces in expression
    /// position, so a pattern and its scrutinee compare as identical bits. A
    /// 128-bit suffix has no room here and returns `None`.
    fn pattern_int_on_carrier(m: u128, sfx: Option<IntSuffix>) -> Option<i128> {
        // ≤64-bit unsigned: the magnitude rides the carrier as its wrapped
        // 64-bit two's-complement pattern, which is how the same literal is
        // encoded in expression position and at runtime, so pattern and
        // scrutinee compare as identical bits (B-2026-08-20-1).
        if matches!(
            sfx,
            Some(IntSuffix::U8)
                | Some(IntSuffix::U16)
                | Some(IntSuffix::U32)
                | Some(IntSuffix::U64)
                | Some(IntSuffix::Usize)
        ) {
            if let Ok(u) = u64::try_from(m) {
                return Some((u as i64) as i128);
            }
        }
        // 128-bit: the same rule one width up. `i128` takes the magnitude
        // positively; `u128` wraps its top half into the signed carrier, which
        // is exactly what `parser/exprs.rs` does for the expression spelling
        // (B-2026-08-19-23) — the two encodings have to agree or a pattern
        // would never match its own value (B-2026-08-20-4).
        if matches!(sfx, Some(IntSuffix::I128)) {
            return i128::try_from(m).ok();
        }
        if matches!(sfx, Some(IntSuffix::U128)) {
            return Some(m as i128);
        }
        None
    }

    pub(crate) fn parse_literal_pattern(&mut self) -> Option<LiteralPattern> {
        match *self.peek_token_ref() {
            Token::Integer(n, sfx) => {
                self.advance();
                Some(LiteralPattern::Integer(n, sfx))
            }
            Token::IntegerOutOfRange(m, sfx) => {
                self.advance();
                Some(LiteralPattern::Integer(
                    Self::pattern_int_on_carrier(m, sfx)?,
                    sfx,
                ))
            }
            Token::CharLiteral(c) => {
                self.advance();
                Some(LiteralPattern::Char(c))
            }
            Token::ByteLiteral(b) => {
                self.advance();
                Some(LiteralPattern::Integer(b as i128, Some(IntSuffix::U8)))
            }
            _ => {
                self.error("Expected integer or character literal in range pattern");
                None
            }
        }
    }
}
