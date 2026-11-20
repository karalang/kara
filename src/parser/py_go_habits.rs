//! Python's and Go's first-day statement habits, mended where the Kāra form is
//! one-to-one (B-2026-11-20-73, -77, -78): `elif`, `pass`, `is` / `is not`,
//! `x := v`, `var x T = v`, `i++` / `i--`, `for { .. }` and `for cond { .. }`.
//!
//! Each one is refused with its Kāra spelling, carries the edit that writes
//! it, and is rewritten in the token stream so the parse goes on as if the
//! Kāra form had been written: one diagnostic per habit, no cascade.

use crate::resolver::{SpanKey, TextEdit};
use crate::token::{Span, SpannedToken, Token};

use super::Parser;

impl Parser {
    fn ident_at(&self, at: usize) -> Option<&str> {
        match self.tokens.get(at).map(|t| &t.token) {
            Some(Token::Identifier { name, .. }) => Some(name.as_str()),
            _ => None,
        }
    }

    fn tok_at(&self, at: usize) -> Option<&Token> {
        self.tokens.get(at).map(|t| &t.token)
    }

    fn span_at(&self, at: usize) -> Span {
        self.tokens[at].span
    }

    fn touching(&self, a: usize, b: usize) -> bool {
        let (x, y) = (self.span_at(a), self.span_at(b));
        x.offset + x.length == y.offset
    }

    fn habit(&mut self, message: &str, at: Span, edits: Vec<TextEdit>) {
        self.error_at(message, at);
        if !edits.is_empty() {
            self.fix_diffs.insert(SpanKey::from_span(&at), edits);
        }
    }

    fn edit(offset: usize, length: usize, replacement: &str) -> TextEdit {
        TextEdit {
            offset,
            length,
            replacement: replacement.to_string(),
        }
    }

    fn synth(token: Token, at: Span) -> SpannedToken {
        SpannedToken {
            token,
            span: Span { length: 0, ..at },
        }
    }

    /// `elif` where an `else` may follow an `if` block. Rewritten to `else`
    /// `if`; returns whether it was there (the `else` is then consumed).
    pub(super) fn eat_else_or_elif(&mut self) -> bool {
        if self.eat(&Token::Else) {
            return true;
        }
        if self.ident_at(self.pos) != Some("elif")
            || matches!(
                self.tok_at(self.pos + 1),
                Some(Token::Equal | Token::Dot | Token::LeftParen | Token::Semicolon)
            )
        {
            return false;
        }
        let at = self.span_at(self.pos);
        self.habit(
            "`elif` is Python: Kāra writes `else if`",
            at,
            vec![Self::edit(at.offset, at.length, "else if")],
        );
        self.tokens[self.pos].token = Token::If;
        true
    }

    /// `x is None` / `x is not None` (Python's identity test), read as `==`
    /// / `!=`. Returns the operator's token count when the current token
    /// starts one.
    pub(super) fn python_is_operator(&mut self) -> Option<(bool, usize)> {
        if self.ident_at(self.pos) != Some("is") {
            return None;
        }
        // `is` names a value only when an operand follows it.
        let negated = matches!(self.tok_at(self.pos + 1), Some(Token::Not))
            || self.ident_at(self.pos + 1) == Some("not");
        let operand_at = self.pos + 1 + usize::from(negated);
        match self.tok_at(operand_at) {
            Some(
                Token::Identifier { .. }
                | Token::Integer(..)
                | Token::True
                | Token::False
                | Token::StringLiteral(_),
            ) => {}
            _ => return None,
        }
        let first = self.span_at(self.pos);
        let last = self.span_at(operand_at - 1);
        let (sym, word) = if negated {
            ("!=", "is not")
        } else {
            ("==", "is")
        };
        self.habit(
            &format!(
                "`{word}` is Python's identity test: Kāra compares an `Option` with `{sym}` \
                 (`x {sym} None`), or asks `x.is_some()` / `x.is_none()`"
            ),
            first,
            vec![Self::edit(
                first.offset,
                last.offset + last.length - first.offset,
                sym,
            )],
        );
        Some((negated, 1 + usize::from(negated)))
    }

    /// Whether the file uses `pass` as a name anywhere outside a lone
    /// `pass` statement (`let pass = ..`, a parameter, `pass.len()`), in
    /// which case `pass` is never read as Python's empty statement.
    fn pass_is_a_name(&self) -> bool {
        (0..self.tokens.len()).any(|i| {
            self.ident_at(i) == Some("pass")
                && !(i > 0
                    && matches!(
                        self.tok_at(i - 1),
                        Some(Token::LeftBrace | Token::Semicolon | Token::RightBrace)
                    )
                    && matches!(
                        self.tok_at(i + 1),
                        Some(Token::RightBrace | Token::Semicolon)
                    ))
        })
    }

    /// Statement-start habits, rewritten in place before the statement is
    /// parsed.
    pub(super) fn mend_statement_habits(&mut self) {
        // An f-string hole is re-parsed as `fn __interp__() { <hole>; }`: its
        // text is an expression, never a statement written by hand, and
        // its names are bound outside it (`f"{pass}"`).
        if self.ident_at(1) == Some("__interp__") {
            return;
        }
        let p = self.pos;
        // `pass`: the empty statement. Its Kāra form is nothing at all.
        if self.ident_at(p) == Some("pass")
            && !self.pass_is_a_name()
            && matches!(
                self.tok_at(p + 1),
                Some(Token::RightBrace | Token::Semicolon)
            )
        {
            let at = self.span_at(p);
            let mut len = at.length;
            if matches!(self.tok_at(p + 1), Some(Token::Semicolon)) && self.touching(p, p + 1) {
                len += 1;
            }
            self.habit(
                "`pass` is Python's empty statement: a Kāra block may be empty, `{}`, so \
                 the `pass` goes",
                at,
                vec![Self::edit(at.offset, len, "")],
            );
            // Parsed as an empty block, `{}`, which does nothing.
            self.tokens[p].token = Token::LeftBrace;
            self.tokens
                .insert(p + 1, Self::synth(Token::RightBrace, at));
            return;
        }
        // `var x T = v` / `var x: T = v` / `var x = v` (Go).
        if self.ident_at(p) == Some("var") && self.ident_at(p + 1).is_some() {
            let at = self.span_at(p);
            let name = self.span_at(p + 1);
            let mut edits = vec![Self::edit(at.offset, at.length, "let mut")];
            self.tokens[p].token = Token::Let;
            self.tokens.insert(p + 1, Self::synth(Token::Mut, at));
            // A type written straight after the name takes a `:`.
            if !matches!(
                self.tok_at(p + 3),
                Some(Token::Colon | Token::Equal | Token::Semicolon)
            ) {
                edits.push(Self::edit(name.offset + name.length, 0, ":"));
                self.tokens.insert(p + 3, Self::synth(Token::Colon, name));
            }
            self.habit(
                "`var` is Go: Kāra binds a variable with `let mut x: T = v` (`let` alone \
                 when it is never reassigned), the type after a `:`",
                at,
                edits,
            );
            return;
        }
        // `x := v` and `a, b := f()` (Go's short declaration).
        let mut names = vec![p];
        let mut at = p;
        while self.ident_at(at).is_some() {
            match self.tok_at(at + 1) {
                Some(Token::Comma) if self.ident_at(at + 2).is_some() => {
                    at += 2;
                    names.push(at);
                }
                _ => break,
            }
        }
        if self.ident_at(p).is_some()
            && matches!(self.tok_at(at + 1), Some(Token::Colon))
            && matches!(self.tok_at(at + 2), Some(Token::Equal))
            && self.touching(at + 1, at + 2)
        {
            let start = self.span_at(p);
            let colon = self.span_at(at + 1);
            let tuple = names.len() > 1;
            let mut edits = vec![Self::edit(
                start.offset,
                0,
                if tuple { "let (" } else { "let " },
            )];
            if tuple {
                // `r :=` -> `r) =`: the `)` closes on the last name.
                let last = self.span_at(at);
                let end = last.offset + last.length;
                edits.push(Self::edit(end, colon.offset + 2 - end, ") ="));
            } else {
                edits.push(Self::edit(colon.offset, 2, "="));
            }
            // Tokens: `let` [`(`] names [`)`] `=`.
            self.tokens[at + 1].token = if tuple {
                Token::RightParen
            } else {
                Token::Equal
            };
            if tuple {
                self.tokens[at + 2].token = Token::Equal;
            } else {
                self.tokens.remove(at + 2);
            }
            if tuple {
                self.tokens.insert(p, Self::synth(Token::LeftParen, start));
            }
            self.tokens.insert(p, Self::synth(Token::Let, start));
            self.habit(
                "`:=` is Go's short declaration: Kāra binds with `let x = v` (`let (a, b) = \
                 f()` for several values, `let mut` for one that is reassigned)",
                colon,
                edits,
            );
            return;
        }
        // `i++;` / `i--;` (Go, C).
        if self.ident_at(p).is_some() {
            let op = match (self.tok_at(p + 1), self.tok_at(p + 2)) {
                (Some(Token::Plus), Some(Token::Plus)) => Some(("++", "+= 1", Token::PlusEqual)),
                (Some(Token::Minus), Some(Token::Minus)) => Some(("--", "-= 1", Token::MinusEqual)),
                _ => None,
            };
            if let Some((sym, kara, tok)) = op {
                if self.touching(p + 1, p + 2)
                    && matches!(
                        self.tok_at(p + 3),
                        Some(Token::Semicolon | Token::RightBrace)
                    )
                {
                    let at = self.span_at(p + 1);
                    let name = self.span_at(p);
                    self.habit(
                        &format!("`{sym}` is not Kāra: a counter steps with `x {kara}`"),
                        at,
                        vec![Self::edit(
                            name.offset + name.length,
                            at.length + 1,
                            &format!(" {kara}"),
                        )],
                    );
                    self.tokens[p + 1].token = tok;
                    self.tokens[p + 2].token = Token::Integer(1, None);
                }
            }
        }
    }

    /// Go's `for { .. }` (Kāra's `loop`) and `for cond { .. }` (Kāra's
    /// `while`): the `for` token is rewritten, and the caller parses the
    /// loop it now names. Returns the keyword it became.
    pub(super) fn mend_go_for(&mut self) -> Option<Token> {
        let p = self.pos;
        if !matches!(self.tok_at(p), Some(Token::For)) {
            return None;
        }
        let at = self.span_at(p);
        if matches!(self.tok_at(p + 1), Some(Token::LeftBrace)) {
            self.habit(
                "`for { .. }` is Go's endless loop: Kāra writes `loop { .. }`",
                at,
                vec![Self::edit(at.offset, at.length, "loop")],
            );
            self.tokens[p].token = Token::Loop;
            return Some(Token::Loop);
        }
        // `for cond {`: no `in`, `;` or `:` before the body's brace.
        let mut depth = 0i32;
        let mut i = p + 1;
        let mut saw = false;
        while let Some(t) = self.tok_at(i) {
            match t {
                Token::LeftParen | Token::LeftBracket => depth += 1,
                Token::RightParen | Token::RightBracket => depth -= 1,
                Token::LeftBrace if depth == 0 => break,
                Token::In | Token::Semicolon | Token::Colon | Token::Comma if depth == 0 => {
                    return None
                }
                Token::EOF => return None,
                _ => {}
            }
            saw = true;
            i += 1;
        }
        if !saw || self.span_at(i).line != at.line {
            return None;
        }
        self.habit(
            "`for cond { .. }` is Go's conditional loop: Kāra writes `while cond { .. }`",
            at,
            vec![Self::edit(at.offset, at.length, "while")],
        );
        self.tokens[p].token = Token::While;
        Some(Token::While)
    }
}
