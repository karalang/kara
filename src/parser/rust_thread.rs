//! Rust's `std::thread` for a pause (B-2026-11-11-73): `use std::thread;`
//! with `thread::sleep(d)`, `use std::thread::sleep;`, and a bare
//! `std::thread::sleep(d)`. Kāra's pause is `std.time.sleep(d)`, over the
//! same `Duration`, and `std.thread` is not a module, so the import line
//! becomes `import std.time.sleep;` and the path loses its module (in
//! `rewrite_rust_path`, which takes the import edit from here when no
//! import line is there to rewrite).

use crate::resolver::TextEdit;
use crate::token::Token;

impl super::Parser {
    /// `use std::thread;` / `use std::thread::sleep;` (also
    /// `::{sleep}`, and the `import std.thread;` an earlier fix made), with
    /// the cursor at the keyword: the diagnostic, its fix to
    /// `import std.time.sleep;`, and the statement skipped. Returns `false`,
    /// consuming nothing, for any other statement, and for `std::thread`
    /// when the program reaches through it for anything but `sleep`
    /// (`thread::spawn` has its own diagnostic, and the line stays for it).
    pub(crate) fn reject_rust_thread_import(&mut self) -> bool {
        let at = self.pos;
        let token = |i: usize| self.tokens.get(i).map(|t| &t.token);
        let ident = |i: usize| match token(i) {
            Some(Token::Identifier { name, .. }) => Some(name.as_str()),
            _ => None,
        };
        let sep = |i: usize| matches!(token(i), Some(Token::ColonColon | Token::Dot));
        if !matches!(ident(at + 1), Some("std" | "core"))
            || !sep(at + 2)
            || ident(at + 3) != Some("thread")
        {
            return false;
        }
        // The names after `thread`, up to the `;`.
        let mut leaves: Vec<&str> = Vec::new();
        let mut semi = at + 4;
        loop {
            match token(semi) {
                Some(Token::Semicolon) => break,
                Some(Token::Identifier { name, .. }) => leaves.push(name.as_str()),
                Some(Token::ColonColon | Token::Dot | Token::LeftBrace | Token::RightBrace) => {}
                Some(Token::Comma) => {}
                _ => return false,
            }
            semi += 1;
        }
        if !leaves.iter().all(|n| *n == "sleep") {
            return false;
        }
        // The module itself: every `thread::x` / `thread.x` in the program
        // must be `sleep`.
        if leaves.is_empty() && self.thread_reaches_beyond_sleep() {
            return false;
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
            .unwrap_or("std::thread")
            .to_string();
        let message = format!(
            "`{written}` is Rust's standard library -- Kāra's pause is `std.time.sleep`, \
             which takes the same `Duration`: `import std.time.sleep;`"
        );
        self.error_at(&message, whole);
        self.fix_diffs.insert(
            crate::resolver::SpanKey::from_span(&whole),
            vec![TextEdit {
                offset: whole.offset,
                length: whole.length,
                replacement: "import std.time.sleep;".to_string(),
            }],
        );
        self.pos = semi + 1;
        true
    }

    /// Whether some `thread::x` / `thread.x` in the program names
    /// anything but `sleep`.
    fn thread_reaches_beyond_sleep(&self) -> bool {
        self.tokens.windows(3).any(|w| {
            matches!(&w[0].token, Token::Identifier { name, .. } if name == "thread")
                && matches!(w[1].token, Token::ColonColon | Token::Dot)
                && !matches!(&w[2].token, Token::Identifier { name, .. } if name == "sleep")
                && !matches!(&w[2].token, Token::Semicolon)
        })
    }

    /// The edit that imports `std.time.sleep` for a `thread::sleep` path:
    /// `None` when an import line of `std::thread` (which the fix above
    /// rewrites to that import) or of `sleep` is already there.
    pub(super) fn std_time_sleep_import(&self) -> Option<TextEdit> {
        let token = |i: usize| self.tokens.get(i).map(|t| &t.token);
        let ident = |i: usize| match token(i) {
            Some(Token::Identifier { name, .. }) => Some(name.as_str()),
            _ => None,
        };
        let mut any_import = false;
        for i in 0..self.tokens.len() {
            let keyword = matches!(token(i), Some(Token::Import))
                || (ident(i) == Some("use") && ident(i + 1).is_some());
            if !keyword {
                continue;
            }
            any_import = true;
            // The statement names `thread` or `sleep` before its `;`.
            let mut j = i + 1;
            while let Some(t) = token(j) {
                match t {
                    Token::Semicolon => break,
                    Token::Identifier { name, .. } if name == "thread" || name == "sleep" => {
                        return None;
                    }
                    _ => j += 1,
                }
            }
        }
        let (source, base) = self.source.as_ref()?;
        let at = crate::library_imports::above_first_item(source);
        let replacement = if any_import {
            "import std.time.sleep;\n"
        } else {
            "import std.time.sleep;\n\n"
        };
        Some(TextEdit {
            offset: at + base,
            length: 0,
            replacement: replacement.to_string(),
        })
    }
}
