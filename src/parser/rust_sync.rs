//! Rust's `std::sync::atomic` and `Arc`, the shape a Rust-trained author
//! writes shared server state in: `use std::sync::atomic::{AtomicI64,
//! Ordering};` with `AtomicI64::new(0)` and `c.fetch_add(1,
//! Ordering::SeqCst)`, and `Arc::new(Mutex::new(s))` with a `let srv =
//! server.clone();` handed to a task.
//!
//! Kāra's atomics are one generic type, `Atomic[T]` in `std.sync`, with no
//! ordering argument (library/concurrency.md § Atomics); the deprecated
//! trailing ordering already has its own fix. Kāra has no `Arc`: a task of a
//! `TaskGroup` borrows from the scope that made the group, so the shared
//! value is the value itself and a handle to it is `ref value`.
//!
//! At the import line the parser reports one diagnostic whose fix rewrites
//! the line and, in one envelope, every use in the file: `AtomicI64`
//! becomes `Atomic[i64]` (as a type and as a constructor's receiver alike),
//! `Arc<T>` becomes `T` (a struct's field `ref T`, unless the file fills it
//! with an `Arc::new(..)`), `Arc::new(x)` becomes `x`, and `Arc::clone(&x)`
//! or `x.clone()` on a name bound to an `Arc` becomes `ref x`.

use crate::resolver::TextEdit;
use crate::token::Token;

/// The `Atomic[T]` a Rust atomic type is.
fn atomic_inner(name: &str) -> Option<&'static str> {
    Some(match name {
        "AtomicBool" => "bool",
        "AtomicI32" => "i32",
        "AtomicI64" => "i64",
        "AtomicU32" => "u32",
        "AtomicU64" => "u64",
        // Kāra writes a Rust `usize` count as `i64`, and an atomic holds no
        // `usize`.
        "AtomicUsize" | "AtomicIsize" => "i64",
        _ => return None,
    })
}

impl super::Parser {
    /// `use std::sync::atomic::{AtomicI64, Ordering};` (also a single leaf,
    /// and the `import std.sync.atomic..` an earlier fix made), with the
    /// cursor at the keyword: when every name is an atomic `Atomic[T]`
    /// covers or `Ordering`, the diagnostic, its fix, the statement skipped,
    /// and `true`. `false`, consuming nothing, for anything else.
    pub(crate) fn reject_rust_atomic_import(&mut self) -> bool {
        let at = self.pos;
        let token = |i: usize| self.tokens.get(i).map(|t| &t.token);
        let ident = |i: usize| match token(i) {
            Some(Token::Identifier { name, .. }) => Some(name.as_str()),
            _ => None,
        };
        let sep = |i: usize| matches!(token(i), Some(Token::ColonColon | Token::Dot));
        if !matches!(ident(at + 1), Some("std" | "core"))
            || !sep(at + 2)
            || ident(at + 3) != Some("sync")
            || !sep(at + 4)
            || ident(at + 5) != Some("atomic")
            || !sep(at + 6)
        {
            return false;
        }
        let mut names: Vec<String> = Vec::new();
        let semi = if ident(at + 7).is_some() && matches!(token(at + 8), Some(Token::Semicolon)) {
            names.push(ident(at + 7).unwrap_or_default().to_string());
            at + 8
        } else if matches!(token(at + 7), Some(Token::LeftBrace)) {
            let mut i = at + 8;
            loop {
                let Some(name) = ident(i) else {
                    return false;
                };
                names.push(name.to_string());
                i += 1;
                match token(i) {
                    Some(Token::Comma) if matches!(token(i + 1), Some(Token::RightBrace)) => {
                        i += 1;
                        break;
                    }
                    Some(Token::Comma) => i += 1,
                    Some(Token::RightBrace) => break,
                    _ => return false,
                }
            }
            if !matches!(token(i + 1), Some(Token::Semicolon)) {
                return false;
            }
            i + 1
        } else {
            return false;
        };
        if !names
            .iter()
            .all(|n| n == "Ordering" || atomic_inner(n).is_some())
        {
            return false;
        }
        let first = self.tokens[at].span;
        let end = self.tokens[semi].span;
        let whole = crate::token::Span {
            length: end.offset + end.length - first.offset,
            ..first
        };
        let atomics: Vec<&String> = names.iter().filter(|n| *n != "Ordering").collect();
        let mut edits = vec![if atomics.is_empty() {
            let length = self.rust_use_line_length(whole);
            TextEdit {
                offset: whole.offset,
                length,
                replacement: String::new(),
            }
        } else {
            TextEdit {
                offset: whole.offset,
                length: whole.length,
                replacement: "import std.sync.{Atomic};".to_string(),
            }
        }];
        // Every use of an imported atomic's name, after the line itself.
        for t in &self.tokens[semi + 1..] {
            if let Token::Identifier { name, .. } = &t.token {
                if let (true, Some(inner)) = (names.contains(name), atomic_inner(name)) {
                    edits.push(TextEdit {
                        offset: t.span.offset,
                        length: t.span.length,
                        replacement: format!("Atomic[{inner}]"),
                    });
                }
            }
        }
        let shown: Vec<String> = atomics
            .iter()
            .map(|n| format!("`{n}` is `Atomic[{}]`", atomic_inner(n).unwrap_or("T")))
            .collect();
        let message = format!(
            "`std::sync::atomic` is Rust's -- Kāra's atomics are one type, `Atomic[T]` in \
             `std.sync`, and every operation is sequentially consistent with no ordering \
             argument (library/concurrency.md § Atomics){}{}",
            if shown.is_empty() { "" } else { ": " },
            shown.join(", ")
        );
        self.error_at(&message, whole);
        self.fix_diffs
            .insert(crate::resolver::SpanKey::from_span(&whole), edits);
        self.pos = semi + 1;
        true
    }

    /// The envelope that writes every `Arc` in the file away: `Arc<T>` as
    /// `T`, `Arc::new(x)` as `x`, `Arc::clone(&x)` and `x.clone()` (for a
    /// name bound to an `Arc`) as `ref x`.
    pub(crate) fn rust_arc_rewrite(&self) -> Vec<TextEdit> {
        let token = |k: usize| self.tokens.get(k).map(|t| &t.token);
        let ident = |k: usize| match token(k) {
            Some(Token::Identifier { name, .. }) => Some(name.as_str()),
            _ => None,
        };
        let is_arc = |k: usize| ident(k) == Some("Arc");
        let opens_type = |k: usize| matches!(token(k), Some(Token::LessThan | Token::LeftBracket));
        let arc_call = |k: usize, method: &str| {
            is_arc(k)
                && matches!(token(k + 1), Some(Token::ColonColon | Token::Dot))
                && ident(k + 2) == Some(method)
                && matches!(token(k + 3), Some(Token::LeftParen))
        };
        // The names bound to an `Arc`: `let s = Arc::new(..)`, `s: Arc<..>`.
        let mut names: Vec<String> = Vec::new();
        for k in 0..self.tokens.len() {
            let Some(name) = ident(k) else { continue };
            let bound = (matches!(token(k + 1), Some(Token::Equal))
                && matches!(
                    k.checked_sub(1).and_then(token),
                    Some(Token::Let | Token::Mut)
                )
                && arc_call(k + 2, "new"))
                || (matches!(token(k + 1), Some(Token::Colon))
                    && is_arc(k + 2)
                    && opens_type(k + 3));
            if bound && !names.iter().any(|n| n == name) {
                names.push(name.to_string());
            }
        }
        let span = |k: usize| self.tokens[k].span;
        let delete = |from: usize, to: usize| TextEdit {
            offset: from,
            length: to - from,
            replacement: String::new(),
        };
        // The fields an `Arc::new(..)` initializes in a struct literal
        // (`state: Arc::new(..)`): the struct owns that value, so the field
        // keeps it.
        let mut owning_fields: Vec<&str> = Vec::new();
        for k in 0..self.tokens.len() {
            if let Some(name) = ident(k) {
                if matches!(token(k + 1), Some(Token::Colon)) && arc_call(k + 2, "new") {
                    owning_fields.push(name);
                }
            }
        }
        let mut edits = Vec::new();
        let mut k = 0;
        while k < self.tokens.len() {
            // `Arc<T>` / `Arc[T]`: the head and its closing bracket go. A
            // struct's field shares a value someone else made, which a
            // task borrows from the scope of its group: the field is
            // `ref T` (B-2026-11-27-41), unless the file fills it with an
            // `Arc::new(..)` of its own.
            if is_arc(k) && opens_type(k + 1) {
                let field = self
                    .struct_field_name_before(k)
                    .filter(|f| !owning_fields.contains(&f.as_str()));
                let mut depth: i64 = 0;
                let mut j = k + 1;
                let mut close = None;
                while let Some(t) = token(j) {
                    match t {
                        Token::LessThan | Token::LeftBracket => depth += 1,
                        Token::GreaterThan | Token::RightBracket => {
                            depth -= 1;
                            if depth == 0 {
                                close = Some((span(j).offset, 1));
                            }
                        }
                        // `>>` closes two; the `Arc`'s is the half that
                        // takes the depth to zero.
                        Token::GreaterGreater => {
                            if depth <= 2 {
                                close = Some((span(j).offset + depth as usize - 1, 1));
                            }
                            depth -= 2;
                        }
                        Token::Semicolon | Token::LeftBrace | Token::EOF => break,
                        _ => {}
                    }
                    if depth <= 0 {
                        break;
                    }
                    j += 1;
                }
                if let Some((offset, length)) = close {
                    edits.push(TextEdit {
                        offset: span(k).offset,
                        length: span(k + 2).offset - span(k).offset,
                        replacement: if field.is_some() {
                            "ref ".to_string()
                        } else {
                            String::new()
                        },
                    });
                    edits.push(TextEdit {
                        offset,
                        length,
                        replacement: String::new(),
                    });
                }
                k += 2;
                continue;
            }
            // `Arc::new(x)` -> `x`; `Arc::clone(&x)` -> `ref x`.
            if arc_call(k, "new") || arc_call(k, "clone") {
                if let Some(close) = self.matching_paren(k + 3) {
                    let clone = ident(k + 2) == Some("clone");
                    let mut lo = k + 4;
                    let amp = clone && matches!(token(lo), Some(Token::Amp));
                    let already_ref = clone && matches!(token(lo), Some(Token::Ref));
                    if amp {
                        lo += 1;
                    }
                    if lo < close {
                        edits.push(TextEdit {
                            offset: span(k).offset,
                            length: span(lo).offset - span(k).offset,
                            replacement: if clone && !already_ref {
                                "ref ".to_string()
                            } else {
                                String::new()
                            },
                        });
                        edits.push(delete(span(close).offset, span(close).offset + 1));
                    }
                }
                k += 4;
                continue;
            }
            // `srv.clone()` on a name bound to an `Arc` -> `ref srv`.
            if let Some(name) = ident(k) {
                let receiver = !matches!(k.checked_sub(1).and_then(token), Some(Token::Dot));
                if receiver
                    && names.iter().any(|n| n == name)
                    && matches!(token(k + 1), Some(Token::Dot))
                    && ident(k + 2) == Some("clone")
                    && matches!(token(k + 3), Some(Token::LeftParen))
                    && matches!(token(k + 4), Some(Token::RightParen))
                {
                    let end = span(k + 4);
                    edits.push(TextEdit {
                        offset: span(k).offset,
                        length: end.offset + end.length - span(k).offset,
                        replacement: format!("ref {name}"),
                    });
                    k += 5;
                    continue;
                }
            }
            k += 1;
        }
        edits
    }

    /// When the type at token `k` is a named field's type in a `struct`'s
    /// braces (`name: Arc<..>`, `pub name: ..`), the field's name.
    fn struct_field_name_before(&self, k: usize) -> Option<String> {
        let token = |i: usize| self.tokens.get(i).map(|t| &t.token);
        if !matches!(token(k.checked_sub(1)?), Some(Token::Colon)) {
            return None;
        }
        let name = match token(k.checked_sub(2)?) {
            Some(Token::Identifier { name, .. }) => name.clone(),
            _ => return None,
        };
        let mut before = k.checked_sub(3)?;
        if matches!(token(before), Some(Token::Pub)) {
            before = before.checked_sub(1)?;
        }
        if !matches!(token(before), Some(Token::LeftBrace | Token::Comma)) {
            return None;
        }
        // The `{` that opens the braces holding the field.
        let mut depth = 0usize;
        let mut open = None;
        for i in (0..=before).rev() {
            match token(i) {
                Some(Token::RightBrace | Token::RightParen | Token::RightBracket) => depth += 1,
                Some(Token::LeftBrace) if depth == 0 => {
                    open = Some(i);
                    break;
                }
                Some(Token::LeftBrace | Token::LeftParen | Token::LeftBracket) => {
                    depth = depth.checked_sub(1)?
                }
                _ => {}
            }
        }
        // `struct Name {` or `struct Name[T] {`.
        let mut i = open?.checked_sub(1)?;
        if matches!(token(i), Some(Token::RightBracket | Token::GreaterThan)) {
            let mut d = 0usize;
            loop {
                match token(i) {
                    Some(Token::RightBracket | Token::GreaterThan) => d += 1,
                    Some(Token::LeftBracket | Token::LessThan) => {
                        d -= 1;
                        if d == 0 {
                            break;
                        }
                    }
                    None => return None,
                    _ => {}
                }
                i = i.checked_sub(1)?;
            }
            i = i.checked_sub(1)?;
        }
        if !matches!(token(i), Some(Token::Identifier { .. })) {
            return None;
        }
        matches!(token(i.checked_sub(1)?), Some(Token::Struct)).then_some(name)
    }
}
