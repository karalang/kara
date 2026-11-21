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
        if self.ident_at(p + 1).is_some()
            && matches!(self.tok_at(p + 2), Some(Token::In))
            && self.ident_at(p + 3) == Some("range")
            && !matches!(self.tok_at(p + 4), Some(Token::LeftParen))
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
}
