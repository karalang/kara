//! Rust's I/O traits and `std::net::Shutdown` (B-2026-11-16-170): `use
//! std::io::{Read, Write};` and `use std::net::{TcpStream, Shutdown};`,
//! with `stream.shutdown(Shutdown::Write)`. Kāra's `read`, `write_all` and
//! `read_line` are methods of the handle itself, with no trait to import,
//! and a `TcpStream` closes its write half with `shutdown_write()`. The
//! Rust-only names leave the import (the whole line when nothing else is
//! on it), and the call becomes `shutdown_write()`.

use crate::resolver::TextEdit;
use crate::token::{Span, Token};

/// The names of `std::<module>` that are Rust's alone, for the modules Kāra
/// also has.
fn rust_only(module: &str, name: &str) -> bool {
    match module {
        "io" => matches!(name, "Read" | "Write" | "BufRead" | "Seek"),
        "net" => name == "Shutdown",
        _ => false,
    }
}

/// Why `name` goes, for the diagnostic.
fn why(module: &str) -> &'static str {
    if module == "net" {
        "Kāra's `TcpStream` closes its write half with `shutdown_write()`, which takes no \
         argument"
    } else {
        "Kāra's `read`, `write_all` and `read_line` are methods of the handle itself, with no \
         trait to import"
    }
}

impl super::Parser {
    /// `use std::io::{Read, Write};` / `import std.net.{TcpStream,
    /// Shutdown};` (also a single leaf), with the cursor at the keyword.
    /// When every name is Rust's alone: one diagnostic whose fix deletes
    /// the line, the statement skipped, and `true`. When only some are:
    /// a diagnostic for each run of them, whose fix deletes the run, the
    /// names recorded so the import drops them, and `false` with nothing
    /// consumed, so the import parses as usual. `false` for anything else.
    pub(crate) fn reject_rust_io_trait_import(&mut self) -> bool {
        let at = self.pos;
        let token = |i: usize| self.tokens.get(i).map(|t| &t.token);
        let ident = |i: usize| match token(i) {
            Some(Token::Identifier { name, .. }) => Some(name.as_str()),
            _ => None,
        };
        let sep = |i: usize| matches!(token(i), Some(Token::ColonColon | Token::Dot));
        let module = match ident(at + 3) {
            Some(m @ ("io" | "net")) => m.to_string(),
            _ => return false,
        };
        if ident(at + 1) != Some("std") || !sep(at + 2) || !sep(at + 4) {
            return false;
        }
        // The leaves: one name, or a flat `{..}` group of plain names.
        let mut leaves: Vec<usize> = Vec::new();
        let semi = if ident(at + 5).is_some() && matches!(token(at + 6), Some(Token::Semicolon)) {
            leaves.push(at + 5);
            at + 6
        } else if matches!(token(at + 5), Some(Token::LeftBrace)) {
            let mut i = at + 6;
            loop {
                if ident(i).is_none() {
                    return false;
                }
                leaves.push(i);
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
        let bad: Vec<bool> = leaves
            .iter()
            .map(|&i| rust_only(&module, ident(i).unwrap_or_default()))
            .collect();
        if !bad.iter().any(|b| *b) {
            return false;
        }
        let names_at: Vec<String> = leaves
            .iter()
            .map(|&i| ident(i).unwrap_or_default().to_string())
            .collect();
        let spans_at: Vec<Span> = leaves.iter().map(|&i| self.tokens[i].span).collect();
        let span = |i: usize| self.tokens[i].span;
        if bad.iter().all(|b| *b) {
            let first = span(at);
            let end = span(semi);
            let whole = Span {
                length: end.offset + end.length - first.offset,
                ..first
            };
            let path_start = span(at + 1).offset;
            let last = span(semi - 1);
            let written = self
                .source_at(path_start, last.offset + last.length - path_start)
                .unwrap_or("std::io")
                .to_string();
            let message = format!(
                "`{written}` is Rust's standard library -- {}: delete the line",
                why(&module)
            );
            self.error_at(&message, whole);
            let length = self.rust_use_line_length(whole);
            self.fix_diffs.insert(
                crate::resolver::SpanKey::from_span(&whole),
                vec![TextEdit {
                    offset: whole.offset,
                    length,
                    replacement: String::new(),
                }],
            );
            self.pos = semi + 1;
            return true;
        }
        // Each run of Rust-only names goes with the separator after it, or,
        // at the end of the list, the one before it.
        let mut k = 0;
        while k < leaves.len() {
            if !bad[k] {
                k += 1;
                continue;
            }
            let mut j = k;
            while j + 1 < leaves.len() && bad[j + 1] {
                j += 1;
            }
            let (from, to) = if j + 1 < leaves.len() {
                (spans_at[k].offset, spans_at[j + 1].offset)
            } else {
                let prev = spans_at[k - 1];
                let last = spans_at[j];
                (prev.offset + prev.length, last.offset + last.length)
            };
            let names: Vec<String> = (k..=j).map(|n| format!("`{}`", names_at[n])).collect();
            let first = spans_at[k];
            let last = spans_at[j];
            let at_names = Span {
                length: last.offset + last.length - first.offset,
                ..first
            };
            let message = format!(
                "{} {} Rust's alone -- {}: delete {}",
                names.join(", "),
                if k == j { "is" } else { "are" },
                why(&module),
                if k == j { "it" } else { "them" }
            );
            self.error_at(&message, at_names);
            self.fix_diffs.insert(
                crate::resolver::SpanKey::from_span(&at_names),
                vec![TextEdit {
                    offset: from,
                    length: to - from,
                    replacement: String::new(),
                }],
            );
            self.rust_only_import_leaves
                .extend(spans_at[k..=j].iter().map(|s| s.offset));
            k = j + 1;
        }
        false
    }

    /// `stream.shutdown(Shutdown::Write)` (or `Shutdown.Write`), with the
    /// cursor just past the method name `shutdown`: the diagnostic, its fix
    /// to `shutdown_write()`, and `true`, so the caller drops the argument
    /// and calls `shutdown_write`. The `::` becomes a `.` so the path's own
    /// rewrite does not overlap the fix.
    pub(crate) fn reject_rust_shutdown_call(&mut self, name_span: Span) -> bool {
        let at = self.pos;
        let token = |i: usize| self.tokens.get(i).map(|t| &t.token);
        let is = |i: usize, s: &str| matches!(token(i), Some(Token::Identifier { name, .. }) if name == s);
        if !matches!(token(at), Some(Token::LeftParen))
            || !is(at + 1, "Shutdown")
            || !matches!(token(at + 2), Some(Token::ColonColon | Token::Dot))
            || !is(at + 3, "Write")
            || !matches!(token(at + 4), Some(Token::RightParen))
        {
            return false;
        }
        let close = self.tokens[at + 4].span;
        let whole = Span {
            length: close.offset + close.length - name_span.offset,
            ..name_span
        };
        self.error_at(
            "`shutdown(Shutdown::Write)` is Rust's -- Kāra's `TcpStream` closes its write half \
             with `shutdown_write()`, which takes no argument",
            whole,
        );
        self.fix_diffs.insert(
            crate::resolver::SpanKey::from_span(&whole),
            vec![TextEdit {
                offset: whole.offset,
                length: whole.length,
                replacement: "shutdown_write()".to_string(),
            }],
        );
        self.tokens[at + 2].token = Token::Dot;
        true
    }
}
