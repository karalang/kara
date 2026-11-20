//! Rust's turbofish after a function name: `with_provider::<Clock, _>(p, ||
//! ..)`, `parse_as::<i64>(s)`. Kāra has no call-site type arguments
//! (design.md § Generics), and `with_provider` names its resource in
//! brackets, `with_provider[Clock](p, || ..)`. The `::` used to surface as
//! "Expected `;`, found `::`" with no fix. Now the list is named and
//! rewritten: for `with_provider`, to the bracketed resource (its `_` holes
//! dropped); for any other function, deleted, so the arguments infer it.

use crate::resolver::{SpanKey, TextEdit};
use crate::token::{Span, SpannedToken, Token};

impl super::Parser {
    /// Called with the function name consumed and the cursor at the `::`.
    /// When a `::<..>(` follows: the diagnostic, its fix, and the tokens
    /// rewritten as the fixed text reads, so the call parses on.
    pub(crate) fn reject_function_turbofish(&mut self, name: &str) {
        let at = self.pos;
        // `HashSet::<i64>::new()`: the list belongs to the type, and Kāra
        // keeps it in brackets (B-2026-11-19-89).
        if crate::parser::starts_upper(name) && at > 0 {
            let start = self.tokens[at - 1].span;
            if let Some((edits, args, method)) = self.rewrite_type_turbofish(at) {
                let end = self.tokens[method].span;
                let whole = Span {
                    length: end.offset + end.length - start.offset,
                    ..start
                };
                let member = match &self.tokens[method].token {
                    Token::Identifier { name, .. } => name.clone(),
                    _ => String::new(),
                };
                self.error_at(
                    &format!(
                        "`::<..>` after a type is Rust's turbofish -- Kāra names a type's \
                         arguments in brackets: `{name}[{args}].{member}`"
                    ),
                    whole,
                );
                self.fix_diffs.insert(SpanKey::from_span(&whole), edits);
                return;
            }
        }
        let token = |i: usize| self.tokens.get(i).map(|t| &t.token);
        if !matches!(token(at), Some(Token::ColonColon))
            || !matches!(token(at + 1), Some(Token::LessThan))
        {
            return;
        }
        // The matching `>`: one counter for both bracket kinds, as the list
        // is a type and only nests properly; `>>` closes two.
        let mut depth: i64 = 0;
        let mut i = at + 1;
        // Each top-level argument, as a token range.
        let mut args: Vec<(usize, usize)> = Vec::new();
        let mut arg_start = at + 2;
        loop {
            match token(i) {
                Some(Token::LessThan | Token::LeftBracket | Token::LeftParen) => depth += 1,
                Some(Token::GreaterThan | Token::RightBracket | Token::RightParen) => depth -= 1,
                Some(Token::GreaterGreater) => depth -= 2,
                Some(Token::Comma) if depth == 1 => {
                    args.push((arg_start, i));
                    arg_start = i + 1;
                }
                Some(Token::EOF | Token::Semicolon | Token::LeftBrace | Token::RightBrace)
                | None => return,
                _ => {}
            }
            if depth <= 0 {
                break;
            }
            i += 1;
        }
        if depth < 0 {
            return;
        }
        let close = i;
        // `None::<Rc<RefCell<TreeNode>>>`: Rust's typed `None` (kata
        // 1660's template). Kāra names the type on the enum,
        // `Option[TreeNode].None` (design.md § Generics, B-2026-09-13-25).
        if name == "None" && !matches!(token(close + 1), Some(Token::LeftParen)) {
            self.reject_none_turbofish(at, close);
            return;
        }
        if !matches!(token(close + 1), Some(Token::LeftParen)) {
            return;
        }
        if arg_start < close {
            args.push((arg_start, close));
        }
        let kept: Vec<(usize, usize)> = args
            .into_iter()
            .filter(|&(s, e)| !(e == s + 1 && matches!(token(s), Some(Token::Underscore))))
            .collect();
        let first = self.tokens[at].span;
        let last = self.tokens[close].span;
        let whole = Span {
            length: last.offset + last.length - first.offset,
            ..first
        };
        let resource = match kept.as_slice() {
            [(s, e)] if name == "with_provider" && *e == s + 1 => match token(*s) {
                Some(Token::Identifier { name, .. }) => Some((name.clone(), *s)),
                _ => None,
            },
            _ => None,
        };
        if let Some((resource, ident)) = resource {
            self.error_at(
                &format!(
                    "`::<..>` after `with_provider` is Rust's turbofish -- Kāra names the \
                     resource in brackets: `with_provider[{resource}](provider, || ..)`"
                ),
                whole,
            );
            self.fix_diffs.insert(
                SpanKey::from_span(&whole),
                vec![TextEdit {
                    offset: whole.offset,
                    length: whole.length,
                    replacement: format!("[{resource}]"),
                }],
            );
            let ident = self.tokens[ident].clone();
            let bracket = |token: Token, span: Span| SpannedToken { token, span };
            self.tokens.splice(
                at..=close,
                [
                    bracket(Token::LeftBracket, first),
                    ident,
                    bracket(Token::RightBracket, last),
                ],
            );
            return;
        }
        self.error_at(
            "`::<..>` after a function name is Rust's turbofish; Kāra has no call-site type \
             arguments -- remove it, and the arguments or the binding's annotation name the \
             types",
            whole,
        );
        self.fix_diffs.insert(
            SpanKey::from_span(&whole),
            vec![TextEdit {
                offset: whole.offset,
                length: whole.length,
                replacement: String::new(),
            }],
        );
        self.tokens.drain(at..=close);
    }
    /// `None::<T>` with the `None` consumed, `at` the `::` and `close` the
    /// list's `>`: the diagnostic, with the fix `Option[T].None` when the
    /// list is one type, and the list dropped so `None` parses on.
    fn reject_none_turbofish(&mut self, at: usize, close: usize) {
        let name = self.tokens[at - 1].span;
        let last = self.tokens[close].span;
        let whole = Span {
            length: last.offset + last.length - name.offset,
            ..name
        };
        let first = self.tokens[at + 2].span;
        let ty = self.source.as_ref().and_then(|(source, base)| {
            let text =
                source.get(first.offset.checked_sub(*base)?..last.offset.checked_sub(*base)?)?;
            let one_type = !text.trim().is_empty()
                && text.trim() != "_"
                && !text.contains(['\n', '/', '"', ',']);
            one_type.then(|| text.trim().replace('<', "[").replace('>', "]"))
        });
        match ty {
            Some(ty) => {
                self.error_at(
                    &format!(
                        "`None::<..>` is Rust's turbofish; Kāra names the type on the enum: \
                         `Option[{ty}].None`"
                    ),
                    whole,
                );
                self.fix_diffs.insert(
                    SpanKey::from_span(&whole),
                    vec![TextEdit {
                        offset: whole.offset,
                        length: whole.length,
                        replacement: format!("Option[{ty}].None"),
                    }],
                );
            }
            None => self.error_at(
                "`None::<..>` is Rust's turbofish; Kāra names the type on the enum: \
                 `Option[T].None`",
                whole,
            ),
        }
        self.tokens.drain(at..=close);
    }

    /// Rust's turbofish on a TYPE in an expression: `HashSet::<char>::new()`,
    /// where `colons` is the index of the `::` before the `<`. Kāra names the
    /// type's arguments in brackets, `HashSet[char].new()` (design.md
    /// § Generics), so the list is kept, not dropped (B-2026-11-19-89).
    /// When a `::<..>::name` follows, answers the edits that write
    /// `[..].` in place of `::<..>::`, the bracketed list's text, and the
    /// index of the method name, after rewriting the tokens to read as the
    /// fixed text does. Answers `None`, touching nothing, for any other shape
    /// or when the source text is not at hand.
    pub(crate) fn rewrite_type_turbofish(
        &mut self,
        colons: usize,
    ) -> Option<(Vec<TextEdit>, String, usize)> {
        let token = |i: usize| self.tokens.get(i).map(|t| &t.token);
        if !matches!(token(colons), Some(Token::ColonColon))
            || !matches!(token(colons + 1), Some(Token::LessThan))
        {
            return None;
        }
        let mut depth: i64 = 0;
        let mut i = colons + 1;
        loop {
            match token(i) {
                Some(Token::LessThan | Token::LeftBracket | Token::LeftParen) => depth += 1,
                Some(Token::GreaterThan | Token::RightBracket | Token::RightParen) => depth -= 1,
                Some(Token::GreaterGreater) => depth -= 2,
                Some(Token::EOF | Token::Semicolon | Token::LeftBrace | Token::RightBrace)
                | None => return None,
                _ => {}
            }
            if depth <= 0 {
                break;
            }
            i += 1;
        }
        let close = i;
        if depth != 0
            || close == colons + 2
            || !matches!(token(close + 1), Some(Token::ColonColon))
            || !matches!(token(close + 2), Some(Token::Identifier { .. }))
        {
            return None;
        }
        let open = self.tokens[colons + 1].span;
        let shut = self.tokens[close].span;
        let after = self.tokens[close + 1].span;
        let nested_close = matches!(token(close), Some(Token::GreaterGreater));
        let args = self.source.as_ref().and_then(|(source, base)| {
            let from = (open.offset + open.length).checked_sub(*base)?;
            // A `>>` closes the list and the last argument's own list.
            let to = (shut.offset + usize::from(nested_close)).checked_sub(*base)?;
            let text = source.get(from..to)?.trim();
            (!text.is_empty() && !text.contains(['\n', '/', '"']))
                .then(|| text.replace('<', "[").replace('>', "]"))
        })?;
        let first = self.tokens[colons].span;
        let edit = TextEdit {
            offset: first.offset,
            length: after.offset + after.length - first.offset,
            replacement: format!("[{args}]."),
        };
        let at = |token: Token, span: Span| SpannedToken { token, span };
        let mut inner: Vec<SpannedToken> = Vec::new();
        for t in &self.tokens[colons + 2..close] {
            match t.token {
                Token::LessThan => inner.push(at(Token::LeftBracket, t.span)),
                Token::GreaterThan => inner.push(at(Token::RightBracket, t.span)),
                Token::GreaterGreater => {
                    inner.push(at(Token::RightBracket, t.span));
                    inner.push(at(Token::RightBracket, t.span));
                }
                _ => inner.push(t.clone()),
            }
        }
        if nested_close {
            inner.push(at(Token::RightBracket, shut));
        }
        let mut with = vec![at(Token::LeftBracket, open)];
        with.extend(inner);
        with.push(at(Token::RightBracket, shut));
        with.push(at(Token::Dot, after));
        let added = with.len();
        self.tokens.splice(colons..=close + 1, with);
        Some((vec![edit], args, colons + added))
    }
}
