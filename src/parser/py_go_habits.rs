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

    /// Whether the file binds `range` as a name: `let range`, `let mut
    /// range`, a parameter or field `range:`, a closure's `|range|`, or a
    /// `for range in`.
    fn range_is_a_name(&self) -> bool {
        (0..self.tokens.len()).any(|i| {
            self.ident_at(i) == Some("range")
                && (matches!(self.tok_at(i + 1), Some(Token::Colon | Token::Equal))
                    && !matches!(self.tok_at(i + 2), Some(Token::Equal | Token::Colon))
                    || i > 0
                        && matches!(
                            self.tok_at(i - 1),
                            Some(Token::Let | Token::Mut | Token::Pipe | Token::For)
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
        if self.mend_go_statement_forms() || self.mend_python_tuple_let() {
            return;
        }
        let p = self.pos;
        if self.ident_at(p) == Some("switch")
            && !matches!(
                self.tok_at(p + 1),
                Some(
                    Token::Equal
                        | Token::Dot
                        | Token::LeftParen
                        | Token::Semicolon
                        | Token::RightBrace
                )
            )
            && self.mend_go_switch()
        {
            return;
        }
        // `raise e`, `try { .. } except { .. }`, `with open(p) as f { .. }`:
        // Python's exceptions and context managers have no one-to-one Kāra
        // form, so each is told, with no edit (B-2026-11-20-74).
        // Kāra has its own `try { .. }` block; only one an `except` follows
        // is Python's.
        let python_try = matches!(self.tok_at(p), Some(Token::Try))
            && matches!(self.tok_at(p + 1), Some(Token::LeftBrace))
            && self
                .matching_brace(p + 1)
                .is_some_and(|c| self.ident_at(c + 1) == Some("except"));
        let word = if python_try {
            Some("try".to_string())
        } else {
            self.ident_at(p).map(str::to_string)
        };
        if let Some(word) = word {
            let next_ok = !matches!(
                self.tok_at(p + 1),
                Some(
                    Token::Equal | Token::Dot | Token::LeftParen | Token::Semicolon | Token::Colon
                )
            );
            // `raise` only in Python's shape: `raise "msg"` or
            // `raise ValueError(..)`; `with` only as `with f(..) as x`.
            // Both are soft words, so `with * 2` or `raise + 1` is Kāra.
            let raise_shape = match self.tok_at(p + 1) {
                Some(Token::StringLiteral(_)) => true,
                Some(Token::Identifier { .. }) => {
                    matches!(self.tok_at(p + 2), Some(Token::LeftParen))
                }
                _ => false,
            };
            let with_shape = matches!(self.tok_at(p + 1), Some(Token::Identifier { .. }))
                && matches!(self.tok_at(p + 2), Some(Token::LeftParen))
                && {
                    let mut i = p + 3;
                    let mut depth = 1i32;
                    while depth > 0 {
                        match self.tok_at(i) {
                            Some(Token::LeftParen) => depth += 1,
                            Some(Token::RightParen) => depth -= 1,
                            None => break,
                            _ => {}
                        }
                        i += 1;
                    }
                    depth == 0 && matches!(self.tok_at(i), Some(Token::As))
                };
            let note = match word.as_str() {
                "raise" if raise_shape => Some(
                    "`raise` is Python's: a Kāra function that can fail returns a `Result`, \
                     and fails with `return Err(e)`; `panic(\"..\")` stops the program",
                ),
                "try" if python_try => Some(
                    "`try`/`except` is Python's: Kāra handles a failure where it is returned, \
                     with `match r { Ok(v) => .., Err(e) => .. }`, or passes it on with `?`",
                ),
                "with" if with_shape => Some(
                    "`with` is Python's context manager: a Kāra value is closed when its scope \
                     ends, so `let f = fs.open(p)?;` in a block does what `with` does",
                ),
                _ => None,
            };
            if let Some(note) = note.filter(|_| next_ok) {
                let at = self.span_at(p);
                self.error_at(note, at);
                // Skip the statement: through its `;`, or its blocks
                // (`try {..} except .. {..}`), so nothing cascades.
                let mut i = p;
                let mut depth = 0i32;
                loop {
                    match self.tok_at(i) {
                        Some(Token::LeftBrace) => depth += 1,
                        Some(Token::RightBrace) => {
                            depth -= 1;
                            if depth < 0 {
                                break;
                            }
                            if depth == 0 && self.ident_at(i + 1) != Some("except") {
                                i += 1;
                                break;
                            }
                        }
                        Some(Token::Semicolon) if depth == 0 => {
                            i += 1;
                            break;
                        }
                        Some(Token::EOF) | None => break,
                        _ => {}
                    }
                    i += 1;
                }
                self.tokens.drain(p..i);
                self.tokens.insert(p, Self::synth(Token::LeftBrace, at));
                self.tokens
                    .insert(p + 1, Self::synth(Token::RightBrace, at));
                return;
            }
        }
        // `# text`: Python's line comment (`#[..]` / `#!` are attributes).
        if matches!(self.tok_at(p), Some(Token::Pound))
            && !(matches!(self.tok_at(p + 1), Some(Token::LeftBracket | Token::Bang))
                && self.touching(p, p + 1))
        {
            let at = self.span_at(p);
            self.habit(
                "`#` starts a comment in Python; Kāra's line comment is `//`",
                at,
                vec![Self::edit(at.offset, at.length, "//")],
            );
            let mut end = p;
            while self.tok_at(end).is_some_and(|t| !matches!(t, Token::EOF))
                && self.span_at(end).line == at.line
            {
                end += 1;
            }
            self.tokens.drain(p..end);
            return;
        }
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
            // `v, ok := m[k]`: Go's comma-ok lookup, whose Kāra form depends
            // on what the program does with `ok`.
            let comma_ok = names.len() == 2
                && self.ident_at(at + 3).is_some()
                && matches!(self.tok_at(at + 4), Some(Token::LeftBracket))
                && self.matching_close(at + 4).is_some_and(|c| {
                    matches!(
                        self.tok_at(c + 1),
                        Some(Token::Semicolon | Token::RightBrace)
                    )
                });
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
            if comma_ok {
                self.error_at(
                    "`v, ok := m[k]` is Go's comma-ok lookup: Kāra's lookup returns an \
                     `Option`, so the test is `if let Some(v) = m.get(k) { .. }`",
                    colon,
                );
                return;
            }
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
        if self.mend_go_c_for() || self.mend_go_range_for() {
            return None;
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

    /// Go writes a parameter or a field as `name T`; Kāra as `name: T`
    /// (B-2026-11-20-77). Called where a `:` is due after `name_end`: when a
    /// type starts there instead, on the same line, the `:` is inserted.
    pub(super) fn mend_go_missing_colon(&mut self, name_end: Span, what: &str) {
        if self.check(&Token::Colon) || self.pos == 0 {
            return;
        }
        let at = self.span_at(self.pos);
        if at.line != name_end.line {
            return;
        }
        let type_start = match self.tok_at(self.pos) {
            Some(Token::Identifier { .. } | Token::LeftParen | Token::Ref | Token::Mut) => true,
            // `[]T` / `[N]T`, Go's slice and array types, are not Kāra's;
            // only the `:` is offered, so this is left to the type's own error.
            _ => false,
        };
        if !type_start {
            return;
        }
        let end = name_end.offset + name_end.length;
        self.habit(
            &format!(
                "a {what} is written `name: T` in Kāra; `name T` is Go's order without the `:`"
            ),
            at,
            vec![Self::edit(end, 0, ":")],
        );
        self.tokens.insert(self.pos, Self::synth(Token::Colon, at));
    }

    /// Go's result type follows the parameter list with no arrow:
    /// `fn f(a: i64) i64 { .. }` / `fn f() (i64, i64) { .. }`. Kāra writes
    /// `-> T` (B-2026-11-20-77). Called after the `)`; inserts the `->`.
    pub(super) fn mend_go_result_without_arrow(&mut self, close: Span) {
        if self.is_effect_start() || self.pos >= self.tokens.len() {
            return;
        }
        let at = self.span_at(self.pos);
        if at.line != close.line {
            return;
        }
        let is_type = match self.tok_at(self.pos) {
            Some(Token::LeftParen) => true,
            Some(Token::Identifier { name, .. }) => {
                name.starts_with(|c: char| c.is_ascii_uppercase())
                    || matches!(
                        name.as_str(),
                        "i8" | "i16"
                            | "i32"
                            | "i64"
                            | "i128"
                            | "u8"
                            | "u16"
                            | "u32"
                            | "u64"
                            | "u128"
                            | "isize"
                            | "usize"
                            | "f32"
                            | "f64"
                            | "bool"
                            | "char"
                            | "str"
                            | "string"
                            | "int"
                    )
            }
            _ => false,
        };
        if !is_type {
            return;
        }
        // The body's `{` must follow on the line, so this is a signature.
        let mut i = self.pos;
        let mut depth = 0i32;
        let mut brace = false;
        while let Some(t) = self.tok_at(i) {
            if self.span_at(i).line != at.line {
                break;
            }
            match t {
                Token::LeftParen | Token::LeftBracket => depth += 1,
                Token::RightParen | Token::RightBracket => depth -= 1,
                Token::LeftBrace if depth == 0 => {
                    brace = true;
                    break;
                }
                _ => {}
            }
            i += 1;
        }
        if !brace {
            return;
        }
        self.habit(
            "a function's result type follows `->` in Kāra: `fn f(..) -> T { .. }`; Go \
             writes it after the parameters with no arrow",
            at,
            vec![Self::edit(at.offset, 0, "-> ")],
        );
        self.tokens.insert(self.pos, Self::synth(Token::Arrow, at));
    }

    /// `struct P { x: i64; y: i64 }`: fields are separated by `,` in Kāra
    /// (Go separates them by line, C by `;`). The `;` becomes a `,`.
    pub(super) fn mend_field_semicolon(&mut self) {
        if !self.check(&Token::Semicolon) {
            return;
        }
        let at = self.span_at(self.pos);
        self.habit(
            "struct fields are separated by `,` in Kāra, not `;`",
            at,
            vec![Self::edit(at.offset, at.length, ",")],
        );
        self.tokens[self.pos].token = Token::Comma;
    }

    /// Go's `return a, b` returns two values; Kāra returns the tuple
    /// `(a, b)` (B-2026-11-20-77). Only where the comma cannot be a match
    /// arm's or an argument list's: the statement ends at a `;` or `}` with
    /// no `=>` before it.
    pub(super) fn mend_go_multi_return(
        &mut self,
        first: crate::ast::Expr,
    ) -> Option<crate::ast::Expr> {
        // `Err(r) => return r,` ends a match arm: the comma is the arm's.
        // In `par { return 5, 3 }` the comma ends the branch.
        if !self.check(&Token::Comma)
            || self.in_par_branch
            || matches!(
                self.tok_at(self.pos + 1),
                Some(Token::RightBrace | Token::Semicolon | Token::EOF) | None
            )
        {
            return Some(first);
        }
        let mut depth = 0i32;
        let mut i = self.pos;
        loop {
            match self.tok_at(i) {
                Some(Token::LeftParen | Token::LeftBracket | Token::LeftBrace) => depth += 1,
                Some(Token::RightParen | Token::RightBracket) => depth -= 1,
                Some(Token::RightBrace) if depth == 0 => break,
                Some(Token::RightBrace) => depth -= 1,
                Some(Token::Semicolon) if depth == 0 => break,
                Some(Token::FatArrow) | None | Some(Token::EOF) => return Some(first),
                _ => {}
            }
            if depth < 0 {
                return Some(first);
            }
            i += 1;
        }
        let comma = self.span_at(self.pos);
        let mut items = vec![first];
        while self.eat(&Token::Comma) {
            items.push(self.parse_expression()?);
        }
        let head = items[0].span;
        let last = items[items.len() - 1].span;
        let end = last.offset + last.length;
        self.habit(
            "Kāra returns several values as one tuple: `return (a, b)`; Go's `return a, b` \
             lists them bare",
            comma,
            vec![Self::edit(head.offset, 0, "("), Self::edit(end, 0, ")")],
        );
        Some(crate::ast::Expr {
            id: crate::ids::NodeId::DUMMY,
            span: Span {
                length: end - head.offset,
                ..head
            },
            kind: crate::ast::ExprKind::Tuple(items),
        })
    }

    /// Python's `lambda x, y: x + y` is Kāra's closure `|x, y| x + y`
    /// (B-2026-11-20-73). Rewritten where it starts an operand: plain names
    /// separated by commas, then a `:`.
    pub(super) fn mend_python_lambda(&mut self) {
        let p = self.pos;
        if self.ident_at(p) != Some("lambda") {
            return;
        }
        let mut names = Vec::new();
        let mut at = p + 1;
        while self.ident_at(at).is_some() {
            names.push(at);
            match self.tok_at(at + 1) {
                Some(Token::Comma) => at += 2,
                _ => break,
            }
        }
        let Some(&last) = names.last() else {
            return;
        };
        if !matches!(self.tok_at(last + 1), Some(Token::Colon)) {
            return;
        }
        let kw = self.span_at(p);
        let first = self.span_at(names[0]);
        let last_span = self.span_at(last);
        let colon = self.span_at(last + 1);
        let last_end = last_span.offset + last_span.length;
        self.habit(
            "`lambda` is Python: Kāra writes a closure as `|x| x + 1`",
            kw,
            vec![
                Self::edit(kw.offset, first.offset - kw.offset, "|"),
                Self::edit(last_end, colon.offset + colon.length - last_end, "|"),
            ],
        );
        self.tokens[p].token = Token::Pipe;
        self.tokens[last + 1].token = Token::Pipe;
    }

    /// The source text of `[from, to)`, when the parser has it.
    fn source_between(&self, from: usize, to: usize) -> Option<String> {
        let (src, base) = self.source.as_ref()?;
        src.get(from.checked_sub(*base)?..to.checked_sub(*base)?)
            .map(str::to_string)
    }

    /// The index of the `{` that opens a `for` body starting at `from`,
    /// with the `;` positions met at depth 0 on the way.
    fn header_brace(&self, from: usize) -> Option<(usize, Vec<usize>)> {
        let mut depth = 0i32;
        let mut semis = Vec::new();
        let mut i = from;
        loop {
            match self.tok_at(i)? {
                Token::LeftParen | Token::LeftBracket => depth += 1,
                Token::RightParen | Token::RightBracket => depth -= 1,
                Token::LeftBrace if depth == 0 => return Some((i, semis)),
                Token::Semicolon if depth == 0 => semis.push(i),
                Token::EOF => return None,
                _ => {}
            }
            i += 1;
        }
    }

    /// The index of the `}` matching the `{` at `open`.
    fn matching_brace(&self, open: usize) -> Option<usize> {
        let mut depth = 0i32;
        let mut i = open;
        loop {
            match self.tok_at(i)? {
                Token::LeftBrace => depth += 1,
                Token::RightBrace => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i);
                    }
                }
                Token::EOF => return None,
                _ => {}
            }
            i += 1;
        }
    }

    /// Whether the tokens in `[from, to)` write the name `var` (`var =`,
    /// `var += ..`, `var++`) or call a mutating method on any of `names`.
    fn writes_in(&self, from: usize, to: usize, var: &str, names: &[String]) -> bool {
        (from..to).any(|i| {
            let Some(n) = self.ident_at(i) else {
                return false;
            };
            if n == var
                && matches!(
                    self.tok_at(i + 1),
                    Some(
                        Token::Equal
                            | Token::PlusEqual
                            | Token::MinusEqual
                            | Token::StarEqual
                            | Token::SlashEqual
                            | Token::Plus
                            | Token::Minus
                    )
                )
                && !(matches!(self.tok_at(i + 1), Some(Token::Plus | Token::Minus))
                    && !matches!(self.tok_at(i + 2), Some(Token::Plus | Token::Minus)))
            {
                return true;
            }
            names.iter().any(|m| m == n)
                && (matches!(self.tok_at(i + 1), Some(Token::Equal))
                    || (matches!(self.tok_at(i + 1), Some(Token::Dot))
                        && matches!(
                            self.ident_at(i + 2),
                            Some(
                                "push"
                                    | "pop"
                                    | "insert"
                                    | "remove"
                                    | "clear"
                                    | "truncate"
                                    | "extend"
                                    | "push_back"
                                    | "push_front"
                                    | "pop_back"
                                    | "pop_front"
                                    | "drain"
                                    | "retain"
                                    | "swap_remove"
                            )
                        )))
        })
    }

    /// Go's and C's `for i := a; i < b; i++ { .. }` (and `let mut i = a`,
    /// `i <= b`, `i += 1`) is Kāra's `for i in a..b { .. }` when the body
    /// writes neither `i` nor the bound (B-2026-11-20-78). Any other
    /// three-part header is refused with the note and no edit.
    fn mend_go_c_for(&mut self) -> bool {
        let p = self.pos;
        let Some((brace, semis)) = self.header_brace(p + 1) else {
            return false;
        };
        if semis.len() != 2 {
            return false;
        }
        let at = self.span_at(p);
        let note = "a three-part `for init; cond; step { .. }` is Go's and C's: Kāra counts \
                    with a range, `for i in a..b { .. }` (`a..=b` for `<=`), or loops with \
                    `while`";
        let (s1, s2) = (semis[0], semis[1]);
        // init: `i := A` | `let mut i = A` | `let i = A`
        let (var_at, a_from) = if self.ident_at(p + 1).is_some()
            && matches!(self.tok_at(p + 2), Some(Token::Colon))
            && matches!(self.tok_at(p + 3), Some(Token::Equal))
        {
            (p + 1, p + 4)
        } else if matches!(self.tok_at(p + 1), Some(Token::Let)) {
            let v = if matches!(self.tok_at(p + 2), Some(Token::Mut)) {
                p + 3
            } else {
                p + 2
            };
            if self.ident_at(v).is_none() || !matches!(self.tok_at(v + 1), Some(Token::Equal)) {
                return self.decline_c_for(at, note, brace);
            }
            (v, v + 2)
        } else {
            return self.decline_c_for(at, note, brace);
        };
        let var = self.ident_at(var_at).unwrap_or_default().to_string();
        // cond: `i < B` | `i <= B`
        let inclusive = match (self.ident_at(s1 + 1), self.tok_at(s1 + 2)) {
            (Some(v), Some(Token::LessThan)) if v == var => false,
            (Some(v), Some(Token::LessThanOrEqual)) if v == var => true,
            _ => return self.decline_c_for(at, note, brace),
        };
        let b_from = s1 + 3;
        // step: `i++` | `i += 1`
        let step_ok = self.ident_at(s2 + 1) == Some(var.as_str())
            && match (self.tok_at(s2 + 2), self.tok_at(s2 + 3)) {
                (Some(Token::Plus), Some(Token::Plus)) => s2 + 4 == brace,
                (Some(Token::PlusEqual), Some(Token::Integer(1, None))) => s2 + 4 == brace,
                _ => false,
            };
        if !step_ok || a_from >= s1 || b_from >= s2 {
            return self.decline_c_for(at, note, brace);
        }
        let bound_names: Vec<String> = (b_from..s2)
            .filter_map(|i| self.ident_at(i).map(str::to_string))
            .collect();
        let Some(close) = self.matching_brace(brace) else {
            return false;
        };
        if self.writes_in(brace, close, &var, &bound_names) {
            return self.decline_c_for(at, note, brace);
        }
        let a_span = (self.span_at(a_from).offset, {
            let e = self.span_at(s1 - 1);
            e.offset + e.length
        });
        let b_span = (self.span_at(b_from).offset, {
            let e = self.span_at(s2 - 1);
            e.offset + e.length
        });
        let (Some(a_txt), Some(b_txt)) = (
            self.source_between(a_span.0, a_span.1),
            self.source_between(b_span.0, b_span.1),
        ) else {
            return self.decline_c_for(at, note, brace);
        };
        let dots = if inclusive { "..=" } else { ".." };
        let head_from = at.offset + at.length;
        let head_to = self.span_at(brace).offset;
        self.habit(
            note,
            at,
            vec![Self::edit(
                head_from,
                head_to - head_from,
                &format!(" {var} in {a_txt}{dots}{b_txt} "),
            )],
        );
        // Tokens: `for` var `in` A `..` B `{`.
        let mut head = vec![self.tokens[var_at].clone(), Self::synth(Token::In, at)];
        head.extend(self.tokens[a_from..s1].iter().cloned());
        head.push(Self::synth(
            if inclusive {
                Token::DotDotEq
            } else {
                Token::DotDot
            },
            at,
        ));
        head.extend(self.tokens[b_from..s2].iter().cloned());
        self.tokens.splice(p + 1..brace, head);
        true
    }

    fn decline_c_for(&mut self, at: Span, note: &str, brace: usize) -> bool {
        self.error_at(note, at);
        // Parse on as `loop`: the header is dropped, the body kept.
        self.tokens[self.pos].token = Token::Loop;
        self.tokens.drain(self.pos + 1..brace);
        true
    }

    /// Go's `for i, x := range v` / `for _, x := range v` / `for i := range v`
    /// and `for x in range v` (B-2026-11-20-78).
    fn mend_go_range_for(&mut self) -> bool {
        let p = self.pos;
        let at = self.span_at(p);
        // `for x in range v`: the `range` goes.
        // Only in Go's shape, a collection named after the word, and only
        // where the program binds no `range` of its own: `for d in range {`
        // iterates a binding named `range` (B-2026-11-22-4).
        if self.ident_at(p + 1).is_some()
            && matches!(self.tok_at(p + 2), Some(Token::In))
            && self.ident_at(p + 3) == Some("range")
            && (self.ident_at(p + 4).is_some()
                || (matches!(self.tok_at(p + 4), Some(Token::LeftBracket))
                    && !self.touching(p + 3, p + 4)))
            && !self.range_is_a_name()
        {
            let r = self.span_at(p + 3);
            let next = self.span_at(p + 4);
            self.habit(
                "`range` is Go's: a Kāra `for` loop reads `for x in v`",
                r,
                vec![Self::edit(r.offset, next.offset - r.offset, "")],
            );
            self.tokens.remove(p + 3);
            return true;
        }
        // `for A[, B] := range v {`
        let (names, colon) = if self.ident_at(p + 1).is_some()
            || matches!(self.tok_at(p + 1), Some(Token::Underscore))
        {
            if matches!(self.tok_at(p + 2), Some(Token::Comma))
                && (self.ident_at(p + 3).is_some()
                    || matches!(self.tok_at(p + 3), Some(Token::Underscore)))
            {
                (vec![p + 1, p + 3], p + 4)
            } else {
                (vec![p + 1], p + 2)
            }
        } else {
            return false;
        };
        if !(matches!(self.tok_at(colon), Some(Token::Colon))
            && matches!(self.tok_at(colon + 1), Some(Token::Equal))
            && self.ident_at(colon + 2) == Some("range"))
        {
            return false;
        }
        let Some((brace, semis)) = self.header_brace(colon + 3) else {
            return false;
        };
        if !semis.is_empty() || brace == colon + 3 {
            return false;
        }
        let v_span = (self.span_at(colon + 3).offset, {
            let e = self.span_at(brace - 1);
            e.offset + e.length
        });
        let Some(v_txt) = self.source_between(v_span.0, v_span.1) else {
            return false;
        };
        let name = |i: usize| -> String {
            match self.tok_at(i) {
                Some(Token::Identifier { name, .. }) => name.clone(),
                _ => "_".to_string(),
            }
        };
        let blank = |i: usize| matches!(self.tok_at(i), Some(Token::Underscore));
        let simple_v = brace == colon + 4 && self.ident_at(colon + 3).is_some();
        let v_call = if simple_v {
            v_txt.clone()
        } else {
            format!("({v_txt})")
        };
        let (head, iter_suffix): (String, Option<&str>) = match names.as_slice() {
            // `for i := range v`: Go ranges over the indices.
            [i] => (format!("{} in 0..{v_call}.len()", name(*i)), Some("len")),
            [i, x] if blank(*i) => (format!("{} in {v_txt}", name(*x)), None),
            [i, x] => (
                format!(
                    "({}, {}) in {v_call}.iter().enumerate()",
                    name(*i),
                    name(*x)
                ),
                Some("enumerate"),
            ),
            _ => return false,
        };
        let head_from = at.offset + at.length;
        let head_to = self.span_at(brace).offset;
        self.habit(
            "`for i, x := range v` is Go's: Kāra writes `for (i, x) in v.iter().enumerate()`, \
             `for x in v` for the values alone, and `for i in 0..v.len()` for the indices",
            at,
            vec![Self::edit(
                head_from,
                head_to - head_from,
                &format!(" {head} "),
            )],
        );
        // Re-lex the header and splice it in, with the header's position.
        let mut toks = crate::tokenize(&head);
        toks.retain(|t| !matches!(t.token, Token::EOF));
        for t in &mut toks {
            t.span = Span { length: 0, ..at };
        }
        let _ = iter_suffix;
        self.tokens.splice(p + 1..brace, toks);
        true
    }

    /// Go's `switch x { case 1, 2: .. default: .. }` is Kāra's `match`
    /// (B-2026-11-20-78): each `case` takes literal values, a Go case does
    /// not fall through, and a missing `default` is `_ => {}`. Declined, with
    /// the note, for a tagless `switch`, a non-literal case, or a `break` /
    /// `fallthrough` in a case (a Kāra `break` there would leave the loop).
    fn mend_go_switch(&mut self) -> bool {
        let p = self.pos;
        let at = self.span_at(p);
        let note = "`switch` is Go's: Kāra branches on a value with `match x { 1 | 2 => .., \
                    _ => .. }`";
        let Some((brace, _)) = self.header_brace(p + 1) else {
            return false;
        };
        let Some(close) = self.matching_brace(brace) else {
            return false;
        };
        let decline = |me: &mut Self| {
            me.error_at(note, at);
            // Skip the statement whole: one diagnostic, no cascade.
            me.tokens.drain(p..=close);
            me.tokens.insert(p, Self::synth(Token::LeftBrace, at));
            me.tokens.insert(p + 1, Self::synth(Token::RightBrace, at));
            true
        };
        if brace == p + 1 {
            return decline(self);
        }
        let src = |me: &Self, a: usize, b: usize| -> Option<String> {
            let (x, y) = (me.span_at(a), me.span_at(b));
            me.source_between(x.offset, y.offset + y.length)
        };
        let Some(scrutinee) = src(self, p + 1, brace - 1) else {
            return false;
        };
        // Arms: (values, body token range) in order.
        let mut out = format!("match {scrutinee} {{ ");
        let mut i = brace + 1;
        let mut has_default = false;
        let mut depth = 0i32;
        let mut arm_open = false;
        while i < close {
            let t = self.tok_at(i).cloned();
            let at_arm = depth == 0
                && (self.ident_at(i) == Some("case") || self.ident_at(i) == Some("default"));
            if at_arm {
                if arm_open {
                    out.push_str("} ");
                }
                if self.ident_at(i) == Some("default") {
                    if !matches!(self.tok_at(i + 1), Some(Token::Colon)) {
                        return decline(self);
                    }
                    has_default = true;
                    out.push_str("_ => { ");
                    i += 2;
                } else {
                    let mut vals = Vec::new();
                    let mut j = i + 1;
                    loop {
                        let neg = matches!(self.tok_at(j), Some(Token::Minus));
                        let k = j + usize::from(neg);
                        match self.tok_at(k) {
                            Some(
                                Token::Integer(..)
                                | Token::StringLiteral(_)
                                | Token::True
                                | Token::False
                                | Token::CharLiteral(_),
                            ) => {}
                            _ => return decline(self),
                        }
                        let Some(v) = src(self, j, k) else {
                            return false;
                        };
                        vals.push(v);
                        match self.tok_at(k + 1) {
                            Some(Token::Comma) => j = k + 2,
                            Some(Token::Colon) => {
                                j = k + 2;
                                break;
                            }
                            _ => return decline(self),
                        }
                    }
                    out.push_str(&format!("{} => {{ ", vals.join(" | ")));
                    i = j;
                }
                arm_open = true;
                continue;
            }
            match t {
                Some(Token::LeftBrace | Token::LeftParen | Token::LeftBracket) => depth += 1,
                Some(Token::RightBrace | Token::RightParen | Token::RightBracket) => depth -= 1,
                Some(Token::Break) => return decline(self),
                _ => {}
            }
            if self.ident_at(i) == Some("fallthrough") || !arm_open {
                return decline(self);
            }
            // The arm's text, token by token with the source's spacing.
            let next = if i + 1 < close {
                self.span_at(i + 1).offset
            } else {
                let e = self.span_at(i);
                e.offset + e.length
            };
            let Some(piece) = self.source_between(self.span_at(i).offset, next) else {
                return false;
            };
            out.push_str(&piece);
            i += 1;
        }
        if arm_open {
            out.push_str("} ");
        }
        if !has_default {
            out.push_str("_ => {} ");
        }
        out.push('}');
        let end = {
            let e = self.span_at(close);
            e.offset + e.length
        };
        self.habit(note, at, vec![Self::edit(at.offset, end - at.offset, &out)]);
        let mut toks = crate::tokenize(&out);
        toks.retain(|t| !matches!(t.token, Token::EOF));
        for t in &mut toks {
            t.span = Span { length: 0, ..at };
        }
        self.tokens.splice(p..=close, toks);
        true
    }

    /// `v[a:b]` / `v[:b]` / `v[a:]` (Python's and Go's slice) is Kāra's
    /// `v[a..b]` (B-2026-11-20-75). A negative literal bound counts from the
    /// end in Python: `s[:-4]` is `s[..s.len() - 4]`, written where the
    /// object is a name or a field path. A step (`v[::2]`) is declined.
    pub(super) fn mend_colon_slice(&mut self, object: &crate::ast::Expr) {
        let open = self.pos;
        if !matches!(self.tok_at(open), Some(Token::LeftBracket)) {
            return;
        }
        let mut depth = 0i32;
        let mut colons = Vec::new();
        let mut i = open + 1;
        let close = loop {
            match self.tok_at(i) {
                Some(Token::LeftBracket | Token::LeftParen | Token::LeftBrace) => depth += 1,
                Some(Token::RightBracket) if depth == 0 => break i,
                Some(Token::RightBracket | Token::RightParen | Token::RightBrace) => depth -= 1,
                Some(Token::Colon) if depth == 0 => colons.push(i),
                Some(Token::Comma | Token::Semicolon | Token::EOF) | None if depth == 0 => return,
                _ => {}
            }
            i += 1;
        };
        let note = "`v[a:b]` is Python's and Go's slice: Kāra slices with a range, \
                    `v[a..b]` (`v[..b]`, `v[a..]`), and counts from the end with `v.len()`";
        if colons.is_empty() {
            return;
        }
        if colons.len() > 1 {
            // A step has no slice spelling; the bracket is skipped whole.
            let at = self.span_at(colons[0]);
            self.error_at(note, at);
            self.tokens.drain(open + 1..close);
            self.tokens.insert(open + 1, Self::synth(Token::DotDot, at));
            return;
        }
        let c = colons[0];
        let at = self.span_at(c);
        let place = match &object.kind {
            crate::ast::ExprKind::Identifier(_) | crate::ast::ExprKind::FieldAccess { .. } => {
                self.source_between(object.span.offset, object.span.offset + object.span.length)
            }
            _ => None,
        };
        let mut edits = vec![Self::edit(at.offset, at.length, "..")];
        let mut ok = true;
        // A bound `-k` (a negated integer literal) counts from the end.
        for (from, to) in [(open + 1, c), (c + 1, close)] {
            if to == from + 2
                && matches!(self.tok_at(from), Some(Token::Minus))
                && matches!(self.tok_at(from + 1), Some(Token::Integer(..)))
            {
                match &place {
                    Some(pl) => {
                        let m = self.span_at(from);
                        edits.push(Self::edit(m.offset, m.length, &format!("{pl}.len() - ")));
                    }
                    None => ok = false,
                }
            }
        }
        // The `-` bound is read as written; with no place to name, no edit.
        self.habit(note, at, if ok { edits } else { Vec::new() });
        self.tokens[c].token = Token::DotDot;
    }

    /// Python's list comprehension `[e for x in v]` / `[e for x in v if c]`
    /// is Kāra's `v.iter().map(|x| e).collect()`, with `.filter(|x| c)`
    /// first (B-2026-11-20-75). A nested or multi-`for` comprehension is
    /// declined.
    pub(super) fn mend_python_comprehension(&mut self) {
        let open = self.pos;
        if !matches!(self.tok_at(open), Some(Token::LeftBracket)) {
            return;
        }
        let mut depth = 0i32;
        let (mut for_at, mut in_at, mut if_at) = (None, None, None);
        let mut i = open + 1;
        let close = loop {
            match self.tok_at(i) {
                Some(Token::LeftBracket | Token::LeftParen | Token::LeftBrace) => depth += 1,
                Some(Token::RightBracket) if depth == 0 => break i,
                Some(Token::RightBracket | Token::RightParen | Token::RightBrace) => depth -= 1,
                Some(Token::For) if depth == 0 => {
                    if for_at.is_some() {
                        return;
                    }
                    for_at = Some(i);
                }
                Some(Token::In) if depth == 0 && for_at.is_some() && in_at.is_none() => {
                    in_at = Some(i)
                }
                Some(Token::If) if depth == 0 && in_at.is_some() => if_at = Some(i),
                Some(Token::EOF) | None => return,
                _ => {}
            }
            i += 1;
        };
        let (Some(f), Some(n)) = (for_at, in_at) else {
            return;
        };
        if f == open + 1 || n == f + 1 {
            return;
        }
        let text = |me: &Self, a: usize, b: usize| -> Option<String> {
            if a >= b {
                return None;
            }
            let (x, y) = (me.span_at(a), me.span_at(b - 1));
            me.source_between(x.offset, y.offset + y.length)
        };
        let at = self.span_at(f);
        let note = "a list comprehension is Python's: Kāra builds the list with \
                    `v.iter().map(|x| ..).collect()` (`.filter(|x| ..)` for an `if`)";
        let iter_end = if_at.unwrap_or(close);
        let (Some(e), Some(pat), Some(iter)) = (
            text(self, open + 1, f),
            text(self, f + 1, n),
            text(self, n + 1, iter_end),
        ) else {
            self.error_at(note, at);
            return;
        };
        let cond = match if_at {
            Some(c) => match text(self, c + 1, close) {
                Some(t) => Some(t),
                None => {
                    self.error_at(note, at);
                    return;
                }
            },
            None => None,
        };
        let simple = n + 2 == iter_end && self.ident_at(n + 1).is_some();
        let iter = if simple { iter } else { format!("({iter})") };
        let mut out = format!("{iter}.iter()");
        if let Some(c) = cond {
            out.push_str(&format!(".filter(|{pat}| {c})"));
        }
        out.push_str(&format!(".map(|{pat}| {e}).collect()"));
        let first = self.span_at(open);
        let last = self.span_at(close);
        self.habit(
            note,
            at,
            vec![Self::edit(
                first.offset,
                last.offset + last.length - first.offset,
                &out,
            )],
        );
        let mut toks = crate::tokenize(&out);
        toks.retain(|t| !matches!(t.token, Token::EOF));
        for t in &mut toks {
            t.span = Span { length: 0, ..first };
        }
        self.tokens.splice(open..=close, toks);
    }

    /// The source text of tokens `[a, b)`.
    fn tokens_text(&self, a: usize, b: usize) -> Option<String> {
        if a >= b {
            return None;
        }
        let (x, y) = (self.span_at(a), self.span_at(b - 1));
        if x.length == 0 || y.length == 0 {
            return None;
        }
        self.source_between(x.offset, y.offset + y.length)
    }

    /// Tokens `[from, to]` replaced by `text`, lexed, each spanned at `at`.
    fn splice_text(&mut self, from: usize, to: usize, text: &str, at: Span) {
        let mut toks = crate::tokenize(text);
        toks.retain(|t| !matches!(t.token, Token::EOF));
        for t in &mut toks {
            t.span = Span { length: 0, ..at };
        }
        self.tokens.splice(from..=to, toks);
    }

    /// A refused habit whose tokens `[from, to]` the Kāra `text` replaces,
    /// in the source and in the token stream.
    fn rewrite(&mut self, note: &str, at: Span, from: usize, to: usize, text: &str) {
        let first = self.span_at(from);
        let last = self.span_at(to);
        self.habit(
            note,
            at,
            vec![Self::edit(
                first.offset,
                last.offset + last.length - first.offset,
                text,
            )],
        );
        self.splice_text(from, to, text, first);
    }

    /// The index of the `)` or `]` closing the bracket at `open`.
    fn matching_close(&self, open: usize) -> Option<usize> {
        let mut depth = 0i32;
        let mut i = open;
        loop {
            match self.tok_at(i)? {
                Token::LeftParen | Token::LeftBracket | Token::LeftBrace => depth += 1,
                Token::RightParen | Token::RightBracket | Token::RightBrace => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i);
                    }
                }
                Token::EOF => return None,
                _ => {}
            }
            i += 1;
        }
    }

    /// `k: v, ..` between `from` and `to` (exclusive) as text pairs: a
    /// Python dict's or a Go map literal's entries.
    fn key_value_pairs(&self, from: usize, to: usize) -> Option<Vec<(String, String)>> {
        let mut pairs = Vec::new();
        let mut depth = 0i32;
        let (mut start, mut colon) = (from, None);
        let mut i = from;
        while i <= to {
            let tok = if i == to { None } else { self.tok_at(i) };
            match tok {
                Some(Token::LeftParen | Token::LeftBracket | Token::LeftBrace) => depth += 1,
                Some(Token::RightParen | Token::RightBracket | Token::RightBrace) => depth -= 1,
                Some(Token::Colon) if depth == 0 && colon.is_none() => colon = Some(i),
                Some(Token::Semicolon | Token::EOF) => return None,
                Some(Token::Comma) | None if depth == 0 => {
                    if start < i {
                        let c = colon?;
                        pairs.push((self.tokens_text(start, c)?, self.tokens_text(c + 1, i)?));
                    } else if tok.is_some() {
                        return None;
                    }
                    start = i + 1;
                    colon = None;
                }
                _ => {}
            }
            i += 1;
        }
        Some(pairs)
    }

    /// Go's (and Python's) type names, as Kāra writes them: `int` -> `i64`,
    /// `string` -> `String`, `[]T` -> `Vec[T]`, `map[K]V` -> `Map[K, V]`.
    /// `None` for a type with no one-to-one Kāra form (`chan T`, `*T`).
    fn go_type(text: &str) -> Option<String> {
        let t = text.trim();
        if let Some(rest) = t.strip_prefix("[]") {
            return Some(format!("Vec[{}]", Self::go_type(rest)?));
        }
        if let Some(rest) = t.strip_prefix("map[") {
            let mut depth = 1;
            let close = rest.char_indices().find_map(|(i, c)| {
                match c {
                    '[' => depth += 1,
                    ']' => depth -= 1,
                    _ => {}
                }
                (depth == 0).then_some(i)
            })?;
            let k = Self::go_type(&rest[..close])?;
            let v = Self::go_type(&rest[close + 1..])?;
            return Some(format!("Map[{k}, {v}]"));
        }
        let mapped = match t {
            "int" | "int64" => "i64",
            "int32" | "rune" => {
                if t == "rune" {
                    "char"
                } else {
                    "i32"
                }
            }
            "int16" => "i16",
            "int8" => "i8",
            "uint" | "uint64" => "u64",
            "uint32" => "u32",
            "uint16" => "u16",
            "uint8" | "byte" => "u8",
            "float64" => "f64",
            "float32" => "f32",
            "string" => "String",
            "bool" => "bool",
            _ => "",
        };
        if !mapped.is_empty() {
            return Some(mapped.to_string());
        }
        // A Kāra name as written (`i64`, `String`, a struct of the program).
        (!t.is_empty() && t.chars().all(|c| c.is_alphanumeric() || c == '_')).then(|| t.to_string())
    }

    /// Python's conditional expression `a if c else b`, Kāra's
    /// `if c { a } else { b }` (B-2026-11-20-73). Rewritten where the
    /// expression starts, the whole of it on one line.
    pub(super) fn mend_python_ternary(&mut self) {
        if self.ident_at(1) == Some("__interp__") {
            return;
        }
        let p = self.pos;
        if matches!(
            self.tok_at(p),
            Some(
                Token::Return
                    | Token::Break
                    | Token::Continue
                    | Token::If
                    | Token::Match
                    | Token::Loop
                    | Token::While
                    | Token::For
                    | Token::Let
                    | Token::Pipe
                    | Token::PipePipe
                    | Token::LeftBrace
                    | Token::Else
                    | Token::EOF
            ) | None
        ) {
            return;
        }
        let line = self.span_at(p).line;
        // Scans for `stop` at depth 0, on `line`; `None` at anything that
        // ends the expression first.
        let scan = |me: &Self, from: usize, stop: &Token| -> Option<usize> {
            let mut depth = 0i32;
            let mut i = from;
            loop {
                let t = me.tok_at(i)?;
                if me.span_at(i).line != line {
                    return None;
                }
                match t {
                    Token::LeftParen | Token::LeftBracket => depth += 1,
                    Token::RightParen | Token::RightBracket => {
                        depth -= 1;
                        if depth < 0 {
                            return None;
                        }
                    }
                    t if depth == 0 && t == stop => return Some(i),
                    Token::LeftBrace
                    | Token::RightBrace
                    | Token::Semicolon
                    | Token::Equal
                    | Token::FatArrow
                    | Token::PlusEqual
                    | Token::MinusEqual
                    | Token::StarEqual
                    | Token::SlashEqual
                    | Token::PercentEqual
                    | Token::EOF => return None,
                    Token::Comma if depth == 0 => return None,
                    _ => {}
                }
                i += 1;
            }
        };
        let Some(if_at) = scan(self, p, &Token::If).filter(|&i| i > p) else {
            return;
        };
        let Some(else_at) = scan(self, if_at + 1, &Token::Else).filter(|&i| i > if_at + 1) else {
            return;
        };
        if matches!(
            self.tok_at(else_at + 1),
            Some(Token::LeftBrace | Token::If) | None
        ) {
            return;
        }
        // The else value runs to whatever ends the expression.
        let mut depth = 0i32;
        let mut end = else_at + 1;
        loop {
            match self.tok_at(end) {
                Some(Token::LeftParen | Token::LeftBracket | Token::LeftBrace) => depth += 1,
                Some(Token::RightParen | Token::RightBracket | Token::RightBrace) if depth > 0 => {
                    depth -= 1
                }
                Some(
                    Token::RightParen
                    | Token::RightBracket
                    | Token::RightBrace
                    | Token::Semicolon
                    | Token::Comma
                    | Token::EOF,
                )
                | None => break,
                // A nested conditional is declined: its edit would land in
                // the text this one writes.
                Some(Token::If) if depth == 0 => return,
                _ => {}
            }
            if self.span_at(end).line != line {
                break;
            }
            end += 1;
        }
        let (Some(a), Some(c), Some(b)) = (
            self.tokens_text(p, if_at),
            self.tokens_text(if_at + 1, else_at),
            self.tokens_text(else_at + 1, end),
        ) else {
            return;
        };
        let at = self.span_at(if_at);
        self.rewrite(
            "`a if c else b` is Python's conditional expression: Kāra's `if` is an \
             expression, `if c { a } else { b }`",
            at,
            p,
            end - 1,
            &format!("if {c} {{ {a} }} else {{ {b} }}"),
        );
    }

    /// Python's dict literal `{"a": 1}`, Kāra's `Map.from([("a", 1)])`
    /// (B-2026-11-20-75).
    pub(super) fn mend_python_dict(&mut self) {
        let p = self.pos;
        if self.ident_at(1) == Some("__interp__")
            || !matches!(self.tok_at(p), Some(Token::LeftBrace))
            || !matches!(
                self.tok_at(p + 1),
                Some(Token::StringLiteral(_) | Token::Integer(..))
            )
            || !matches!(self.tok_at(p + 2), Some(Token::Colon))
        {
            return;
        }
        let Some(close) = self.matching_close(p) else {
            return;
        };
        let Some(pairs) = self.key_value_pairs(p + 1, close).filter(|v| !v.is_empty()) else {
            return;
        };
        let items: Vec<String> = pairs.iter().map(|(k, v)| format!("({k}, {v})")).collect();
        let at = self.span_at(p);
        self.rewrite(
            "`{k: v}` is Python's dict literal: Kāra builds a map from its pairs, \
             `Map.from([(k, v), ..])`",
            at,
            p,
            close,
            &format!("Map.from([{}])", items.join(", ")),
        );
    }

    /// Go's value forms where an operand starts (B-2026-11-20-79):
    /// `make(..)`, `[]T{..}`, `map[K]V{..}` and `<-ch`.
    pub(super) fn mend_go_value_forms(&mut self) {
        if self.ident_at(1) == Some("__interp__") {
            return;
        }
        let p = self.pos;
        // `<-rx`: a receive.
        if matches!(self.tok_at(p), Some(Token::LessThan))
            && matches!(self.tok_at(p + 1), Some(Token::Minus))
            && self.touching(p, p + 1)
            && self.ident_at(p + 2).is_some()
        {
            let name = self.ident_at(p + 2).unwrap_or_default().to_string();
            let at = self.span_at(p);
            self.rewrite(
                "`<-rx` is Go's receive: Kāra's is `rx.recv()`, an `Option` that is `None` \
                 once every sender is gone (library/concurrency.md)",
                at,
                p,
                p + 2,
                &format!("{name}.recv()"),
            );
            return;
        }
        // `[]T{a, b}`: a slice literal.
        if matches!(self.tok_at(p), Some(Token::LeftBracket))
            && matches!(self.tok_at(p + 1), Some(Token::RightBracket))
            && self.touching(p, p + 1)
            && self.ident_at(p + 2).is_some()
        {
            let mut brace = p + 2;
            while !matches!(self.tok_at(brace), Some(Token::LeftBrace)) {
                match self.tok_at(brace) {
                    Some(Token::Identifier { .. } | Token::LeftBracket | Token::RightBracket) => {
                        brace += 1
                    }
                    _ => return,
                }
            }
            let Some(close) = self.matching_close(brace) else {
                return;
            };
            let ty = self.tokens_text(p, brace).and_then(|t| Self::go_type(&t));
            let items = self.tokens_text(brace + 1, close);
            let text = match (items, ty) {
                (Some(items), _) => format!("[{}]", items.trim().trim_end_matches(',')),
                (None, Some(ty)) => format!("{ty}.new()"),
                (None, None) => return,
            };
            let at = self.span_at(p);
            self.rewrite(
                "`[]T{..}` is Go's slice literal: Kāra writes the list, `[a, b, c]`",
                at,
                p,
                close,
                &text,
            );
            return;
        }
        // `map[K]V{k: v}`: a map literal.
        if self.ident_at(p) == Some("map") && matches!(self.tok_at(p + 1), Some(Token::LeftBracket))
        {
            let Some(kclose) = self.matching_close(p + 1) else {
                return;
            };
            let mut brace = kclose + 1;
            while !matches!(self.tok_at(brace), Some(Token::LeftBrace)) {
                match self.tok_at(brace) {
                    Some(Token::Identifier { .. } | Token::LeftBracket | Token::RightBracket) => {
                        brace += 1
                    }
                    _ => return,
                }
            }
            let Some(close) = self.matching_close(brace) else {
                return;
            };
            let Some(pairs) = self.key_value_pairs(brace + 1, close) else {
                return;
            };
            let text = if pairs.is_empty() {
                match self.tokens_text(p, brace).and_then(|t| Self::go_type(&t)) {
                    Some(ty) => format!("{ty}.new()"),
                    None => return,
                }
            } else {
                let items: Vec<String> = pairs.iter().map(|(k, v)| format!("({k}, {v})")).collect();
                format!("Map.from([{}])", items.join(", "))
            };
            let at = self.span_at(p);
            self.rewrite(
                "`map[K]V{..}` is Go's map literal: Kāra builds a map from its pairs, \
                 `Map.from([(k, v), ..])`",
                at,
                p,
                close,
                &text,
            );
            return;
        }
        // `make(map[K]V)`, `make([]T, n)`, `make(chan T)`.
        // Only with a Go type first: `make` is a common name for a function.
        let go_type_arg = (self.ident_at(p + 2) == Some("map")
            && matches!(self.tok_at(p + 3), Some(Token::LeftBracket)))
            || (matches!(self.tok_at(p + 2), Some(Token::LeftBracket))
                && matches!(self.tok_at(p + 3), Some(Token::RightBracket))
                && self.touching(p + 2, p + 3)
                && self.ident_at(p + 4).is_some())
            || (self.ident_at(p + 2) == Some("chan") && self.ident_at(p + 3).is_some());
        if self.ident_at(p) == Some("make")
            && matches!(self.tok_at(p + 1), Some(Token::LeftParen))
            && go_type_arg
        {
            let Some(close) = self.matching_close(p + 1) else {
                return;
            };
            let Some(inner) = self.tokens_text(p + 2, close) else {
                return;
            };
            let at = self.span_at(p);
            // The arguments, split at depth-0 commas.
            let mut parts = Vec::new();
            let mut depth = 0i32;
            let mut cur = String::new();
            for ch in inner.chars() {
                match ch {
                    '(' | '[' | '{' => depth += 1,
                    ')' | ']' | '}' => depth -= 1,
                    _ => {}
                }
                if ch == ',' && depth == 0 {
                    parts.push(std::mem::take(&mut cur));
                } else {
                    cur.push(ch);
                }
            }
            parts.push(cur);
            let ty = parts[0].trim().to_string();
            let text = if ty.starts_with("map[") && parts.len() == 1 {
                Self::go_type(&ty).map(|t| format!("{t}.new()"))
            } else if ty.starts_with("[]") && (2..=3).contains(&parts.len()) {
                let zero = match Self::go_type(&ty[2..]).as_deref() {
                    Some("i64") => Some("0"),
                    Some("f64") => Some("0.0"),
                    Some("bool") => Some("false"),
                    Some("String") => Some("String.new()"),
                    _ => None,
                };
                zero.map(|z| format!("Vec.filled({}, {z})", parts[1].trim()))
            } else {
                None
            };
            match text {
                Some(text) => self.rewrite(
                    "`make` is Go's: Kāra builds a map with `Map[K, V].new()` and a list of \
                     `n` zeroes with `Vec.filled(n, 0)`",
                    at,
                    p,
                    close,
                    &text,
                ),
                None if ty.starts_with("chan") => self.error_at(
                    "`make(chan T)` is Go's: a Kāra channel is a sender and a receiver, \
                     `let (tx, rx) = channel(n)`, its capacity `n` required \
                     (library/concurrency.md)",
                    at,
                ),
                None => self.error_at(
                    "`make` is Go's: Kāra builds a map with `Map[K, V].new()`, and a list \
                     with `Vec.filled(n, value)` (the value written out: Kāra has no zero \
                     value for every type)",
                    at,
                ),
            }
        }
    }

    /// Go's statement forms (B-2026-11-20-78, -79): `ch <- v`, `go f(x)`,
    /// `select { case .. }` and `if x := f(); c { .. }`. `true` when the
    /// statement was rewritten or replaced.
    pub(super) fn mend_go_statement_forms(&mut self) -> bool {
        let p = self.pos;
        // `tx <- v`: a send.
        if self.ident_at(p).is_some()
            && matches!(self.tok_at(p + 1), Some(Token::LessThan))
            && matches!(self.tok_at(p + 2), Some(Token::Minus))
            && self.touching(p + 1, p + 2)
        {
            let mut depth = 0i32;
            let mut end = p + 3;
            loop {
                match self.tok_at(end) {
                    Some(Token::LeftParen | Token::LeftBracket | Token::LeftBrace) => depth += 1,
                    Some(Token::RightParen | Token::RightBracket) => depth -= 1,
                    Some(Token::RightBrace) if depth > 0 => depth -= 1,
                    Some(Token::Semicolon | Token::RightBrace | Token::EOF) | None => break,
                    _ => {}
                }
                end += 1;
            }
            let (Some(name), Some(value)) = (
                self.ident_at(p).map(str::to_string),
                self.tokens_text(p + 3, end),
            ) else {
                return false;
            };
            let at = self.span_at(p + 1);
            self.rewrite(
                "`tx <- v` is Go's send: Kāra's is `tx.send(v)` (library/concurrency.md)",
                at,
                p,
                end - 1,
                &format!("{name}.send({value})"),
            );
            return true;
        }
        let word = self.ident_at(p).unwrap_or_default();
        let go_stmt = word == "go"
            && self.ident_at(p + 1).is_some()
            && matches!(self.tok_at(p + 2), Some(Token::LeftParen | Token::Dot));
        let select_stmt = word == "select"
            && matches!(self.tok_at(p + 1), Some(Token::LeftBrace))
            && self.ident_at(p + 2) == Some("case");
        if go_stmt || select_stmt {
            let note = if go_stmt {
                "`go f(x)` is Go's: Kāra starts a task in `par { .. }`, `par for`, or a \
                 `TaskGroup`'s `spawn`, and the group waits for it before it ends \
                 (library/concurrency.md); there is no detached task"
            } else {
                "`select { case .. }` is Go's: Kāra's `select` waits on several channels, \
                 each arm `msg = rx.recv() => handle(msg)`, with an optional `default` \
                 (library/concurrency.md)"
            };
            let at = self.span_at(p);
            self.error_at(note, at);
            // The statement goes, so nothing cascades from it.
            let mut depth = 0i32;
            let mut i = p;
            loop {
                match self.tok_at(i) {
                    Some(Token::LeftParen | Token::LeftBracket | Token::LeftBrace) => depth += 1,
                    Some(Token::RightParen | Token::RightBracket) => depth -= 1,
                    Some(Token::RightBrace) => {
                        depth -= 1;
                        if depth <= 0 {
                            if depth == 0 {
                                i += 1;
                            }
                            break;
                        }
                    }
                    Some(Token::Semicolon) if depth == 0 => {
                        i += 1;
                        break;
                    }
                    Some(Token::EOF) | None => break,
                    _ => {}
                }
                i += 1;
            }
            self.tokens.drain(p..i);
            self.tokens.insert(p, Self::synth(Token::LeftBrace, at));
            self.tokens
                .insert(p + 1, Self::synth(Token::RightBrace, at));
            return true;
        }
        // `if x := f(); c { .. }`: Go's if with an init statement.
        if matches!(self.tok_at(p), Some(Token::If)) {
            let mut names = vec![];
            let mut at = p + 1;
            while self.ident_at(at).is_some() {
                names.push(at);
                if matches!(self.tok_at(at + 1), Some(Token::Comma)) {
                    at += 2;
                } else {
                    break;
                }
            }
            let Some(&last) = names.last() else {
                return false;
            };
            if !(matches!(self.tok_at(last + 1), Some(Token::Colon))
                && matches!(self.tok_at(last + 2), Some(Token::Equal))
                && self.touching(last + 1, last + 2))
            {
                return false;
            }
            let Some((brace, semis)) = self.header_brace(p + 1) else {
                return false;
            };
            let [semi] = semis[..] else {
                return false;
            };
            let at_span = self.span_at(p);
            // Where the if (and its else chain) ends.
            let mut end = self.matching_brace(brace);
            while let Some(e) = end {
                if !matches!(self.tok_at(e + 1), Some(Token::Else)) {
                    break;
                }
                end = if matches!(self.tok_at(e + 2), Some(Token::If)) {
                    self.header_brace(e + 3)
                        .and_then(|(b, _)| self.matching_brace(b))
                } else {
                    self.matching_brace(e + 2)
                };
            }
            let Some(end) = end else {
                return false;
            };
            // A name the rest of the enclosing block reads keeps Go's
            // scope only inside the `if`: moving it out would change what
            // that later name means.
            let name_texts: Vec<String> = names
                .iter()
                .filter_map(|&n| self.ident_at(n).map(str::to_string))
                .collect();
            let mut depth = 0i32;
            let mut i = end + 1;
            let mut used_after = false;
            while let Some(t) = self.tok_at(i) {
                match t {
                    Token::LeftBrace => depth += 1,
                    Token::RightBrace => {
                        depth -= 1;
                        if depth < 0 {
                            break;
                        }
                    }
                    Token::EOF => break,
                    _ => {}
                }
                if self
                    .ident_at(i)
                    .is_some_and(|n| name_texts.iter().any(|x| x == n))
                {
                    used_after = true;
                }
                i += 1;
            }
            let init = self.tokens_text(last + 3, semi);
            let comma_ok = names.len() > 1;
            match init {
                Some(init) if !comma_ok && !used_after => {
                    let name = &name_texts[0];
                    let indent = " ".repeat(at_span.column.saturating_sub(1));
                    let after = self.span_at(semi + 1);
                    self.habit(
                        "`if x := f(); c` is Go's if with an init statement: Kāra binds first, \
                         `let x = f();`, then tests, `if c { .. }`",
                        at_span,
                        vec![
                            Self::edit(
                                at_span.offset,
                                0,
                                &format!("let {name} = {init};\n{indent}"),
                            ),
                            Self::edit(
                                at_span.offset + at_span.length,
                                after.offset - (at_span.offset + at_span.length),
                                " ",
                            ),
                        ],
                    );
                    // Tokens: `let x = init;` then `if c {`.
                    let mut head = vec![
                        Self::synth(Token::Let, at_span),
                        self.tokens[names[0]].clone(),
                        Self::synth(Token::Equal, at_span),
                    ];
                    head.extend(self.tokens[last + 3..=semi].iter().cloned());
                    head.push(self.tokens[p].clone());
                    self.tokens.splice(p..=semi, head);
                }
                _ => {
                    let note = if comma_ok {
                        "`v, ok := m[k]` is Go's comma-ok lookup: Kāra's lookup returns an \
                         `Option`, so the test is `if let Some(v) = m.get(k) { .. }`"
                    } else {
                        "`if x := f(); c` is Go's if with an init statement: Kāra binds first, \
                         `let x = f();`, then tests, `if c { .. }` (here `x` is read after \
                         the `if`, so the binding needs a name of its own)"
                    };
                    self.error_at(note, at_span);
                    self.tokens.drain(p..=end);
                    self.tokens
                        .insert(p, Self::synth(Token::LeftBrace, at_span));
                    self.tokens
                        .insert(p + 1, Self::synth(Token::RightBrace, at_span));
                }
            }
            return true;
        }
        false
    }

    /// Go's item forms (B-2026-11-20-77): `type P struct { .. }` and a
    /// `const ( A = 1; B = 2 )` block.
    pub(super) fn mend_go_item_forms(&mut self) {
        while self.mend_go_file_header() {}
        let p = self.pos;
        if matches!(self.tok_at(p), Some(Token::Type))
            && self.ident_at(p + 1).is_some()
            && matches!(self.tok_at(p + 2), Some(Token::Struct))
            && matches!(self.tok_at(p + 3), Some(Token::LeftBrace))
        {
            let (kw, st) = (self.span_at(p), self.span_at(p + 2));
            let name_text = self.ident_at(p + 1).unwrap_or_default().to_string();
            self.habit(
                "`type P struct { .. }` is Go's: Kāra declares it `struct P { x: T }`",
                kw,
                vec![Self::edit(
                    kw.offset,
                    st.offset + st.length - kw.offset,
                    &format!("struct {name_text}"),
                )],
            );
            self.tokens.remove(p + 2);
            self.tokens[p].token = Token::Struct;
            return;
        }
        if matches!(self.tok_at(p), Some(Token::Const))
            && matches!(self.tok_at(p + 1), Some(Token::LeftParen))
        {
            let at = self.span_at(p);
            let note = "`const ( .. )` is Go's const block: Kāra declares each constant on \
                        its own, `const A: i64 = 1;`, with its type";
            let Some(close) = self.matching_close(p + 1) else {
                return;
            };
            // `NAME = literal` entries, split by `;` or by line.
            let mut lines = Vec::new();
            let mut i = p + 2;
            let mut ok = true;
            while i < close {
                if matches!(self.tok_at(i), Some(Token::Semicolon)) {
                    i += 1;
                    continue;
                }
                let (Some(name), Some(Token::Equal)) =
                    (self.ident_at(i).map(str::to_string), self.tok_at(i + 1))
                else {
                    ok = false;
                    break;
                };
                let ty = match self.tok_at(i + 2) {
                    Some(Token::Integer(_, None)) => "i64",
                    Some(Token::Float(_, None)) => "f64",
                    Some(Token::True | Token::False) => "bool",
                    _ => {
                        ok = false;
                        break;
                    }
                };
                let next = self.tok_at(i + 3);
                let same_line_end = matches!(next, Some(Token::Semicolon))
                    || i + 3 == close
                    || self.span_at(i + 3).line != self.span_at(i).line;
                if !same_line_end {
                    ok = false;
                    break;
                }
                let Some(value) = self.tokens_text(i + 2, i + 3) else {
                    ok = false;
                    break;
                };
                lines.push(format!("const {name}: {ty} = {value};"));
                i += 3;
            }
            if !ok || lines.is_empty() {
                self.error_at(note, at);
                return;
            }
            let text = lines.join("\n");
            self.rewrite(note, at, p, close, &text);
        }
    }

    /// Python's floor division `a // b`, which Kāra lexes as a comment to
    /// the end of the line (B-2026-11-20-73). Found where an operand is
    /// followed by a "comment" that reads as code: it starts with an operand
    /// and holds a `;` or a closing bracket with no opener. Told, and the
    /// line is parsed as `a / b`; the edit to `/` is offered only for two
    /// integer literals, where truncating and flooring agree.
    pub(super) fn mend_floor_division(&mut self) {
        let Some((src, base)) = self.source.clone() else {
            return;
        };
        let mut i = 0;
        while i + 1 < self.tokens.len() {
            let a = self.tokens[i].span;
            let operand = matches!(
                self.tokens[i].token,
                Token::Identifier { .. }
                    | Token::Integer(..)
                    | Token::Float(..)
                    | Token::RightParen
                    | Token::RightBracket
            );
            let a_end = a.offset + a.length;
            let b = self.tokens[i + 1].span;
            if !operand || a.length == 0 || b.offset < a_end {
                i += 1;
                continue;
            }
            let Some(gap) = a_end
                .checked_sub(base)
                .zip(b.offset.checked_sub(base))
                .and_then(|(x, y)| src.get(x..y))
            else {
                i += 1;
                continue;
            };
            let lead = gap.len() - gap.trim_start_matches([' ', '\t']).len();
            let rest = &gap[lead..];
            if !rest.starts_with("//") || rest.starts_with("///") || rest.starts_with("//!") {
                i += 1;
                continue;
            }
            let line_text = rest[2..].split('\n').next().unwrap_or("");
            let mut toks = crate::tokenize(line_text);
            toks.retain(|t| !matches!(t.token, Token::EOF));
            let starts_operand = matches!(
                toks.first().map(|t| &t.token),
                Some(
                    Token::Identifier { .. }
                        | Token::Integer(..)
                        | Token::Float(..)
                        | Token::LeftParen
                )
            );
            let mut depth = 0i32;
            let mut code_like = false;
            for t in &toks {
                match t.token {
                    Token::LeftParen | Token::LeftBracket | Token::LeftBrace => depth += 1,
                    Token::RightParen | Token::RightBracket | Token::RightBrace => {
                        depth -= 1;
                        if depth < 0 {
                            code_like = true;
                        }
                    }
                    Token::Semicolon => code_like = true,
                    _ => {}
                }
            }
            // Prose is not code: two words in a row, or a comment set off
            // by more than one space (`x   // note`), is a comment.
            let prose = toks.windows(2).any(|w| {
                matches!(w[0].token, Token::Identifier { .. } | Token::In)
                    && matches!(w[1].token, Token::Identifier { .. })
            });
            let spaced = lead == 1 && rest[2..].starts_with(' ') && !rest[3..].starts_with(' ');
            if !starts_operand || !code_like || prose || !spaced {
                i += 1;
                continue;
            }
            let slash_at = a_end + lead;
            let at = Span {
                offset: slash_at,
                length: 2,
                line: a.line,
                column: a.column + (slash_at - a.offset),
            };
            let note = "`//` starts a comment in Kāra, so the rest of this line was dropped: \
                        Python's floor division `a // b` is `a / b` here, which truncates \
                        toward zero, so it differs from `//` when one operand is negative";
            let both_literals = matches!(self.tokens[i].token, Token::Integer(..))
                && matches!(toks.first().map(|t| &t.token), Some(Token::Integer(..)));
            if both_literals {
                self.habit(note, at, vec![Self::edit(slash_at, 2, "/")]);
            } else {
                self.error_at(note, at);
            }
            // The line's tokens, at their places after the `//`.
            let text_at = slash_at + 2;
            let col0 = at.column + 2;
            for t in &mut toks {
                t.span.offset += text_at;
                t.span.column = col0 + t.span.column.saturating_sub(1);
                t.span.line = a.line;
            }
            let n = toks.len();
            self.tokens.insert(
                i + 1,
                SpannedToken {
                    token: Token::Slash,
                    span: Span { length: 1, ..at },
                },
            );
            self.tokens.splice(i + 2..i + 2, toks);
            i += 2 + n;
        }
    }

    /// Python's `-> None` result: a Kāra function that returns nothing
    /// writes no result type (B-2026-11-22-5). The `->` and `None` go.
    pub(super) fn mend_python_none_result(&mut self) {
        let p = self.pos;
        if !matches!(self.tok_at(p), Some(Token::Arrow))
            || self.ident_at(p + 1) != Some("None")
            || !(matches!(
                self.tok_at(p + 2),
                Some(Token::LeftBrace | Token::Semicolon)
            ) || matches!(self.ident_at(p + 2), Some("with" | "where")))
            || p == 0
        {
            return;
        }
        // A program's own type named `None` is left alone.
        let declared = (1..self.tokens.len()).any(|i| {
            self.ident_at(i) == Some("None")
                && matches!(
                    self.tok_at(i - 1),
                    Some(Token::Struct | Token::Enum | Token::Type)
                )
        });
        if declared {
            return;
        }
        let before = self.span_at(p - 1);
        let none = self.span_at(p + 1);
        let from = before.offset + before.length;
        let at = self.span_at(p);
        self.habit(
            "`-> None` is Python's: a Kāra function that returns nothing writes no result \
             type, `fn f() { .. }` (its result is `()`)",
            at,
            vec![Self::edit(from, none.offset + none.length - from, "")],
        );
        self.tokens.drain(p..=p + 1);
    }

    /// Python's `let a, b = 1, 2` (a tuple unpacked without parentheses):
    /// Kāra's `let (a, b) = (1, 2)` (B-2026-11-22-5). `true` when mended.
    fn mend_python_tuple_let(&mut self) -> bool {
        let p = self.pos;
        if !matches!(self.tok_at(p), Some(Token::Let)) {
            return false;
        }
        let first = p + 1;
        let mut at = first;
        let mut names = 1;
        while self.ident_at(at).is_some() && matches!(self.tok_at(at + 1), Some(Token::Comma)) {
            at += 2;
            names += 1;
        }
        if names < 2
            || self.ident_at(at).is_none()
            || !matches!(self.tok_at(at + 1), Some(Token::Equal))
        {
            return false;
        }
        let eq = at + 1;
        // The value runs to the statement's end; commas at depth 0 make it
        // a tuple.
        let mut depth = 0i32;
        let mut end = eq + 1;
        let mut commas = false;
        loop {
            match self.tok_at(end) {
                Some(Token::LeftParen | Token::LeftBracket | Token::LeftBrace) => depth += 1,
                Some(Token::RightParen | Token::RightBracket) => depth -= 1,
                Some(Token::RightBrace) if depth > 0 => depth -= 1,
                Some(Token::Comma) if depth == 0 => commas = true,
                Some(Token::Semicolon | Token::RightBrace | Token::EOF) | None => break,
                _ => {}
            }
            if depth < 0 {
                return false;
            }
            end += 1;
        }
        if end == eq + 1 {
            return false;
        }
        let (Some(pat), Some(value)) = (self.tokens_text(first, eq), self.tokens_text(eq + 1, end))
        else {
            return false;
        };
        let value = if commas { format!("({value})") } else { value };
        let at_span = self.span_at(first);
        self.rewrite(
            "Kāra unpacks a tuple inside parentheses: `let (a, b) = (1, 2)`; Python's \
             `a, b = 1, 2` writes it without",
            at_span,
            first,
            end - 1,
            &format!("({pat}) = {value}"),
        );
        true
    }

    /// Go's `package main` and `import "fmt"` at the top of a file
    /// (B-2026-11-22-5): Kāra has no package clause, and its printing needs
    /// no import. `import "fmt"` is deleted only when every `fmt.` call in the
    /// file has its own edit (`fmt.Println(x)` / `fmt.Print(x)`).
    pub(super) fn mend_go_file_header(&mut self) -> bool {
        let p = self.pos;
        let line_end = |me: &Self, last: usize| -> usize {
            // Through the line's end, so the line goes whole.
            match me.tok_at(last + 1) {
                Some(Token::EOF) | None => {
                    let s = me.span_at(last);
                    s.offset + s.length
                }
                _ => me.span_at(last + 1).offset,
            }
        };
        if self.ident_at(p) == Some("package")
            && self.ident_at(p + 1).is_some()
            && self.span_at(p + 1).line == self.span_at(p).line
            && !matches!(
                self.tok_at(p + 2),
                Some(Token::Equal | Token::LeftParen | Token::Dot)
            )
        {
            let at = self.span_at(p);
            let end = line_end(self, p + 1);
            self.habit(
                "`package` is Go's: a Kāra file is its own module and needs no package clause",
                at,
                vec![Self::edit(at.offset, end - at.offset, "")],
            );
            self.tokens.drain(p..=p + 1);
            return true;
        }
        if matches!(self.tok_at(p), Some(Token::Import))
            && matches!(self.tok_at(p + 1), Some(Token::StringLiteral(_)))
        {
            let Some(Token::StringLiteral(pkg)) = self.tok_at(p + 1).cloned() else {
                return false;
            };
            let at = self.span_at(p);
            let mut last = p + 1;
            if matches!(self.tok_at(last + 1), Some(Token::Semicolon)) {
                last += 1;
            }
            // Every `fmt.X(..)` is one the parser mends, so the import is unused.
            let all_mended = pkg == "fmt"
                && (0..self.tokens.len()).all(|i| {
                    if self.ident_at(i) != Some("fmt") || i <= last {
                        return true;
                    }
                    matches!(self.tok_at(i + 1), Some(Token::Dot))
                        && matches!(self.ident_at(i + 2), Some("Println" | "Print"))
                        && matches!(self.tok_at(i + 3), Some(Token::LeftParen))
                        && self.matching_close(i + 3).is_some_and(|c| {
                            c > i + 4
                                && !(i + 4..c).any(|j| matches!(self.tok_at(j), Some(Token::Comma)))
                        })
                });
            let note = format!(
                "`import \"{pkg}\"` is Go's: Kāra imports a module by its path, `import \
                 std.io;`, and printing (`println`) needs no import"
            );
            if all_mended {
                let end = line_end(self, last);
                self.habit(&note, at, vec![Self::edit(at.offset, end - at.offset, "")]);
            } else {
                self.error_at(&note, at);
            }
            self.tokens.drain(p..=last);
            return true;
        }
        false
    }
}
