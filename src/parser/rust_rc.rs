//! Rust's `Rc<RefCell<T>>`, the shape LeetCode's tree and list templates
//! are written in (`Option<Rc<RefCell<TreeNode>>>`). Kāra has no `Rc` or
//! `RefCell`: a `shared struct` is reference counted, and its `mut` fields
//! can be written through any handle (design.md §11), so `Rc<RefCell<T>>`
//! is `T` declared `shared struct`. B-2026-11-08-65.
//!
//! At the `use std::rc::Rc;` / `use std::cell::RefCell;` line the parser
//! reports one diagnostic whose fix deletes the line and, for the first such
//! line, rewrites the program around it in one envelope: each struct `T`
//! named in an `Rc<RefCell<T>>` becomes `shared struct` with `mut` fields,
//! the type becomes `T`, `Rc::new(RefCell::new(x))` / `RefCell::new(x)` /
//! `Rc::clone(&x)` become `x`, and `.borrow()` / `.borrow_mut()` go. A shape
//! the rewrite does not know (`Rc<Vec<T>>`, `Weak`) is left alone, and the
//! resolver's diagnostic for the name points at `shared struct`.
//!
//! Each piece is recognized from the tokens and spelled from the source, so
//! it reads the Rust spelling (`Rc<..>`, `Rc::new`) and the one an earlier
//! `karac fix` round made of it (`Rc[..]`, `Rc.new`) alike: when another fix
//! overlaps the envelope this round, the envelope waits for the next one.

use crate::resolver::TextEdit;
use crate::token::Token;

/// One rewrite: the source bytes `start..end` become `text`, and the walk
/// resumes at token `next`.
struct Piece {
    start: usize,
    end: usize,
    text: String,
    next: usize,
}

impl super::Parser {
    /// `use std::rc::Rc;` or `use std::cell::RefCell;` (also `::{..}` groups
    /// and the `import std.rc.Rc;` an earlier fix made), with the cursor at
    /// the keyword: the diagnostic, its fix, and the statement skipped.
    /// Returns `false`, consuming nothing, for any other statement.
    pub(crate) fn reject_rust_rc_import(&mut self) -> bool {
        let at = self.pos;
        let token = |i: usize| self.tokens.get(i).map(|t| &t.token);
        let ident = |i: usize| match token(i) {
            Some(Token::Identifier { name, .. }) => Some(name.as_str()),
            _ => None,
        };
        let sep = |i: usize| matches!(token(i), Some(Token::ColonColon | Token::Dot));
        if !matches!(ident(at + 1), Some("std" | "core" | "alloc"))
            || !sep(at + 2)
            || !matches!(ident(at + 3), Some("rc" | "cell"))
            || !sep(at + 4)
        {
            return false;
        }
        let mut semi = at + 5;
        loop {
            match token(semi) {
                Some(Token::Semicolon) => break,
                Some(
                    Token::Identifier { .. }
                    | Token::ColonColon
                    | Token::Dot
                    | Token::LeftBrace
                    | Token::RightBrace
                    | Token::Comma,
                ) => semi += 1,
                _ => return false,
            }
        }
        let first = self.tokens[at].span;
        let end = self.tokens[semi].span;
        let whole = crate::token::Span {
            length: end.offset + end.length - first.offset,
            ..first
        };
        let path_start = self.tokens[at + 1].span.offset;
        let path_end = {
            let last = self.tokens[semi - 1].span;
            last.offset + last.length
        };
        let written = self
            .source_at(path_start, path_end - path_start)
            .unwrap_or("std::rc::Rc")
            .to_string();
        let mut edit_length = whole.length;
        if self.source_at(whole.offset + whole.length, 1) == Some("\n") {
            edit_length += 1;
        }
        let mut edits = vec![TextEdit {
            offset: whole.offset,
            length: edit_length,
            replacement: String::new(),
        }];
        let (envelope, wrapped) = if self.rc_envelope_done {
            (Vec::new(), Vec::new())
        } else {
            self.rc_envelope_done = true;
            self.rc_refcell_rewrite()
        };
        let what = if wrapped.is_empty() {
            "delete the line, and declare each type held in an `Rc<RefCell<T>>` as a \
             `shared struct` with `mut` fields"
                .to_string()
        } else {
            let names: Vec<String> = wrapped.iter().map(|n| format!("`{n}`")).collect();
            format!(
                "the fix deletes the line, declares {} `shared struct` with `mut` fields, \
                 writes `Rc<RefCell<T>>` as `T` and `Rc::new(RefCell::new(x))` as `x`, and \
                 drops `.borrow()` / `.borrow_mut()`",
                names.join(", ")
            )
        };
        let message = format!(
            "`{written}` is Rust's standard library -- Kāra has no `Rc` or `RefCell`: a \
             `shared struct` is reference counted, and its `mut` fields can be written \
             through any handle (design.md §11), so `Rc<RefCell<T>>` is a `T` declared \
             `shared struct`; {what}"
        );
        edits.extend(envelope);
        self.error_at(&message, whole);
        self.fix_diffs
            .insert(crate::resolver::SpanKey::from_span(&whole), edits);
        self.pos = semi + 1;
        true
    }

    /// The envelope's edits, and the struct names it declares `shared`.
    fn rc_refcell_rewrite(&self) -> (Vec<TextEdit>, Vec<String>) {
        let Some((source, base)) = self.source.as_ref() else {
            return (Vec::new(), Vec::new());
        };
        let (source, base): (&str, usize) = (source, *base);
        // The structs held in an `Rc<RefCell<T>>` anywhere in the file.
        let mut wrapped: Vec<String> = Vec::new();
        for i in 0..self.tokens.len() {
            if let Some((_, name)) = self.rc_type_at(i, source, base) {
                if !wrapped.contains(&name) {
                    wrapped.push(name);
                }
            }
        }
        let mut edits = Vec::new();
        let mut i = 0;
        while i < self.tokens.len() {
            if let Some(more) = self.shared_struct_edits(i, &wrapped) {
                edits.extend(more);
                i += 1;
                continue;
            }
            match self.rc_piece_at(i, source, base) {
                Some(piece) => {
                    edits.push(TextEdit {
                        offset: piece.start,
                        length: piece.end - piece.start,
                        replacement: piece.text,
                    });
                    i = piece.next;
                }
                None => i += 1,
            }
        }
        // Only the structs this file declares are rewritten; say only those.
        wrapped.retain(|n| {
            self.tokens.windows(2).any(|w| {
                matches!(w[0].token, Token::Struct)
                    && matches!(&w[1].token, Token::Identifier { name, .. } if name == n)
            })
        });
        (edits, wrapped)
    }

    /// `Rc<RefCell<T>>` (or `Rc[RefCell[T]]`) starting at token `i`: the end
    /// offset of the type and `T`, a plain name.
    fn rc_type_at(&self, i: usize, source: &str, base: usize) -> Option<(usize, String)> {
        let t = self.tokens.get(i)?;
        if !matches!(&t.token, Token::Identifier { name, .. } if name == "Rc") {
            return None;
        }
        let bytes = source.as_bytes();
        let mut at = (t.span.offset + t.span.length).checked_sub(base)?;
        let skip = |at: &mut usize| {
            while *at < bytes.len() && bytes[*at].is_ascii_whitespace() {
                *at += 1;
            }
        };
        let open = |at: &mut usize| -> bool {
            skip(at);
            if *at < bytes.len() && (bytes[*at] == b'<' || bytes[*at] == b'[') {
                *at += 1;
                true
            } else {
                false
            }
        };
        let close = |at: &mut usize| -> bool {
            skip(at);
            if *at < bytes.len() && (bytes[*at] == b'>' || bytes[*at] == b']') {
                *at += 1;
                true
            } else {
                false
            }
        };
        let word = |at: &mut usize| -> String {
            skip(at);
            let start = *at;
            while *at < bytes.len() && (bytes[*at].is_ascii_alphanumeric() || bytes[*at] == b'_') {
                *at += 1;
            }
            source[start..*at].to_string()
        };
        if !open(&mut at) || word(&mut at) != "RefCell" || !open(&mut at) {
            return None;
        }
        let name = word(&mut at);
        if name.is_empty() || !name.starts_with(|c: char| c.is_ascii_uppercase()) {
            return None;
        }
        if !close(&mut at) || !close(&mut at) {
            return None;
        }
        Some((at + base, name))
    }

    /// For `struct T {` with `T` in `wrapped`: `shared ` ahead of `struct`
    /// and `mut ` ahead of each field not already `mut`.
    fn shared_struct_edits(&self, i: usize, wrapped: &[String]) -> Option<Vec<TextEdit>> {
        let token = |k: usize| self.tokens.get(k).map(|t| &t.token);
        if !matches!(token(i), Some(Token::Struct)) {
            return None;
        }
        let Some(Token::Identifier { name, .. }) = token(i + 1) else {
            return None;
        };
        if !wrapped.contains(name) || !matches!(token(i + 2), Some(Token::LeftBrace)) {
            return None;
        }
        let mut edits = Vec::new();
        let shared_already = i > 0
            && matches!(token(i - 1), Some(Token::Identifier { name, .. }) if name == "shared");
        if !shared_already {
            edits.push(TextEdit {
                offset: self.tokens[i].span.offset,
                length: 0,
                replacement: "shared ".to_string(),
            });
        }
        let mut depth = 0usize;
        let mut k = i + 2;
        while let Some(t) = token(k) {
            match t {
                Token::LeftBrace | Token::LeftParen | Token::LeftBracket => depth += 1,
                Token::RightBrace | Token::RightParen | Token::RightBracket => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                Token::Identifier { .. }
                    if depth == 1
                        && matches!(token(k + 1), Some(Token::Colon))
                        && matches!(
                            token(k - 1),
                            Some(
                                Token::LeftBrace
                                    | Token::Comma
                                    | Token::Pub
                                    | Token::Private
                                    | Token::RightBracket
                            )
                        ) =>
                {
                    edits.push(TextEdit {
                        offset: self.tokens[k].span.offset,
                        length: 0,
                        replacement: "mut ".to_string(),
                    });
                }
                _ => {}
            }
            k += 1;
        }
        Some(edits)
    }

    /// The rewrite that starts at token `i`, if any: an `Rc<RefCell<T>>`
    /// type, an `Rc::new(..)` / `RefCell::new(..)` / `Rc::clone(&..)` call,
    /// or a `.borrow()` / `.borrow_mut()`.
    fn rc_piece_at(&self, i: usize, source: &str, base: usize) -> Option<Piece> {
        let token = |k: usize| self.tokens.get(k).map(|t| &t.token);
        let ident = |k: usize| match token(k) {
            Some(Token::Identifier { name, .. }) => Some(name.as_str()),
            _ => None,
        };
        let start = self.tokens.get(i)?.span.offset;
        if let Some((end, name)) = self.rc_type_at(i, source, base) {
            let mut next = i + 1;
            while self.tokens.get(next).is_some_and(|t| t.span.offset < end) {
                next += 1;
            }
            return Some(Piece {
                start,
                end,
                text: name,
                next,
            });
        }
        // `Rc::new(x)`, `RefCell::new(x)`, `Rc::clone(&x)`
        let wrapper = matches!(ident(i), Some("Rc" | "RefCell"))
            && matches!(token(i + 1), Some(Token::ColonColon | Token::Dot))
            && matches!(ident(i + 2), Some("new" | "clone"))
            && matches!(token(i + 3), Some(Token::LeftParen));
        if wrapper {
            let close = self.matching_paren(i + 3)?;
            let mut lo = i + 4;
            if ident(i + 2) == Some("clone") && matches!(token(lo), Some(Token::Amp)) {
                lo += 1;
            }
            if lo >= close {
                return None;
            }
            let end_tok = self.tokens[close].span;
            return Some(Piece {
                start,
                end: end_tok.offset + end_tok.length,
                text: self.rc_rewrite_text(lo, close, source, base)?,
                next: close + 1,
            });
        }
        // `.borrow()` / `.borrow_mut()`
        if matches!(token(i), Some(Token::Dot))
            && matches!(ident(i + 1), Some("borrow" | "borrow_mut"))
            && matches!(token(i + 2), Some(Token::LeftParen))
            && matches!(token(i + 3), Some(Token::RightParen))
        {
            let end_tok = self.tokens[i + 3].span;
            return Some(Piece {
                start,
                end: end_tok.offset + end_tok.length,
                text: String::new(),
                next: i + 4,
            });
        }
        None
    }

    /// The source of tokens `lo..hi` with every rewrite inside it applied.
    fn rc_rewrite_text(&self, lo: usize, hi: usize, source: &str, base: usize) -> Option<String> {
        let mut out = String::new();
        let mut cursor = self.tokens.get(lo)?.span.offset;
        let end = {
            let last = self.tokens.get(hi - 1)?.span;
            last.offset + last.length
        };
        let mut i = lo;
        while i < hi {
            match self.rc_piece_at(i, source, base) {
                Some(piece) if piece.next <= hi => {
                    out.push_str(source.get(cursor - base..piece.start - base)?);
                    out.push_str(&piece.text);
                    cursor = piece.end;
                    i = piece.next;
                }
                _ => i += 1,
            }
        }
        out.push_str(source.get(cursor - base..end - base)?);
        Some(out)
    }

    /// The `)` that closes the `(` at token `open`.
    fn matching_paren(&self, open: usize) -> Option<usize> {
        let mut depth = 0usize;
        for k in open..self.tokens.len() {
            match self.tokens[k].token {
                Token::LeftParen => depth += 1,
                Token::RightParen => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(k);
                    }
                }
                _ => {}
            }
        }
        None
    }
}
