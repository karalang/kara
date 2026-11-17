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
}
