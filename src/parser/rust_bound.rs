//! Rust's `std::ops::Bound` for a neighbour lookup in a `BTreeMap`:
//! `m.range((Bound::Excluded(k), Bound::Unbounded)).next()`, the least key
//! after `k`, and its three siblings. Kāra's `SortedMap` names the four
//! directly, `higher(k)`, `ceiling(k)`, `lower(k)` and `floor(k)`
//! (library/collections.md), and `std.ops` is not a module. At the
//! `use std::ops::Bound;` line the parser reports one diagnostic whose fix
//! deletes the line and, in one envelope, writes each such lookup as its
//! method. When the file uses `Bound` in any other shape the line is left to
//! the resolver, since deleting it would leave those uses unknown
//! (B-2026-11-19-82).

use crate::resolver::TextEdit;
use crate::token::Token;

/// One bound of a lookup: its kind, the key's token range, the `Bound`
/// names it holds, and the token after it.
type OneBound = (&'static str, Option<(usize, usize)>, usize, usize);

/// One lookup: the edit's start and end offsets, and its text.
struct Lookup {
    start: usize,
    end: usize,
    text: String,
}

impl super::Parser {
    /// `use std::ops::Bound;` (also `::{Bound}`, and the `import std.ops..`
    /// an earlier fix made), with the cursor at the keyword: when every use
    /// of `Bound` in the file is a lookup the envelope rewrites, the
    /// diagnostic, its fix, the statement skipped, and `true`. `false`,
    /// consuming nothing, for anything else.
    pub(crate) fn reject_rust_bound_import(&mut self) -> bool {
        let at = self.pos;
        let token = |i: usize| self.tokens.get(i).map(|t| &t.token);
        let ident = |i: usize| match token(i) {
            Some(Token::Identifier { name, .. }) => Some(name.as_str()),
            _ => None,
        };
        let sep = |i: usize| matches!(token(i), Some(Token::ColonColon | Token::Dot));
        if !matches!(ident(at + 1), Some("std" | "core"))
            || !sep(at + 2)
            || ident(at + 3) != Some("ops")
            || !sep(at + 4)
        {
            return false;
        }
        let semi = match (token(at + 5), token(at + 6), token(at + 7), token(at + 8)) {
            (Some(Token::Identifier { name, .. }), Some(Token::Semicolon), _, _)
                if name == "Bound" =>
            {
                at + 6
            }
            (
                Some(Token::LeftBrace),
                Some(Token::Identifier { name, .. }),
                Some(Token::RightBrace),
                Some(Token::Semicolon),
            ) if name == "Bound" => at + 8,
            _ => return false,
        };
        // Every `Bound` after the line must sit in a lookup.
        let mut lookups = Vec::new();
        let mut covered = 0usize;
        let mut uses = 0usize;
        for i in semi + 1..self.tokens.len() {
            if ident(i) == Some("Bound") {
                uses += 1;
            }
            if matches!(token(i), Some(Token::Dot)) && ident(i + 1) == Some("range") {
                if let Some((lookup, bounds)) = self.bound_lookup_at(i + 1) {
                    lookups.push(lookup);
                    covered += bounds;
                }
            }
        }
        if covered != uses {
            return false;
        }
        let first = self.tokens[at].span;
        let end = self.tokens[semi].span;
        let whole = crate::token::Span {
            length: end.offset + end.length - first.offset,
            ..first
        };
        let length = self.rust_use_line_length(whole);
        let mut edits = vec![TextEdit {
            offset: whole.offset,
            length,
            replacement: String::new(),
        }];
        edits.extend(lookups.into_iter().map(|l| TextEdit {
            offset: l.start,
            length: l.end - l.start,
            replacement: l.text,
        }));
        self.error_at(
            "`std::ops::Bound` is Rust's -- Kāra's `SortedMap` names each neighbour lookup: \
             `range((Excluded(k), Unbounded)).next()` is `higher(k)`, `(Included(k), \
             Unbounded)` `ceiling(k)`, and `range((Unbounded, Excluded(k))).next_back()` is \
             `lower(k)`, `(Unbounded, Included(k))` `floor(k)`",
            whole,
        );
        self.fix_diffs
            .insert(crate::resolver::SpanKey::from_span(&whole), edits);
        self.pos = semi + 1;
        true
    }

    /// `range((B, B)).next()` / `.next_back()` with `range` at token `r`:
    /// the lookup it is, and how many `Bound` names it holds.
    fn bound_lookup_at(&self, r: usize) -> Option<(Lookup, usize)> {
        let token = |i: usize| self.tokens.get(i).map(|t| &t.token);
        let ident = |i: usize| match token(i) {
            Some(Token::Identifier { name, .. }) => Some(name.as_str()),
            _ => None,
        };
        if !matches!(token(r + 1), Some(Token::LeftParen))
            || !matches!(token(r + 2), Some(Token::LeftParen))
        {
            return None;
        }
        // One bound from token `i`.
        let bound = |i: usize| -> Option<OneBound> {
            let (i, named) = if ident(i) == Some("Bound")
                && matches!(token(i + 1), Some(Token::ColonColon | Token::Dot))
            {
                (i + 2, 1)
            } else {
                (i, 0)
            };
            match ident(i)? {
                "Unbounded" => Some(("Unbounded", None, named, i + 1)),
                name @ ("Excluded" | "Included") => {
                    let kind = if name == "Excluded" {
                        "Excluded"
                    } else {
                        "Included"
                    };
                    let close = self.matching_paren(i + 1)?;
                    (close > i + 2).then_some((kind, Some((i + 2, close)), named, close + 1))
                }
                _ => None,
            }
        };
        let (lo, lo_key, lo_named, after_lo) = bound(r + 3)?;
        if !matches!(token(after_lo), Some(Token::Comma)) {
            return None;
        }
        let (hi, hi_key, hi_named, after_hi) = bound(after_lo + 1)?;
        if !matches!(token(after_hi), Some(Token::RightParen))
            || !matches!(token(after_hi + 1), Some(Token::RightParen))
            || !matches!(token(after_hi + 2), Some(Token::Dot))
            || !matches!(token(after_hi + 4), Some(Token::LeftParen))
            || !matches!(token(after_hi + 5), Some(Token::RightParen))
        {
            return None;
        }
        let step = ident(after_hi + 3)?;
        let (method, key) = match (lo, hi, step) {
            ("Excluded", "Unbounded", "next") => ("higher", lo_key?),
            ("Included", "Unbounded", "next") => ("ceiling", lo_key?),
            ("Unbounded", "Excluded", "next_back") => ("lower", hi_key?),
            ("Unbounded", "Included", "next_back") => ("floor", hi_key?),
            _ => return None,
        };
        let (source, base) = self.source.as_ref()?;
        let from = self.tokens[key.0].span.offset;
        let last = self.tokens[key.1 - 1].span;
        let arg = source.get(from.checked_sub(*base)?..last.offset + last.length - base)?;
        let end = self.tokens[after_hi + 5].span;
        Some((
            Lookup {
                start: self.tokens[r].span.offset,
                end: end.offset + end.length,
                text: format!("{method}({arg})"),
            },
            lo_named + hi_named,
        ))
    }
}
