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
    /// `import std.time.sleep;` (or to nothing when the program does not
    /// sleep: `thread::spawn` has its own diagnostic, and a `TaskGroup`
    /// needs no import), and the statement skipped. Returns `false`,
    /// consuming nothing, for any other statement, and for `std::thread`
    /// when the program reaches through it for anything but `sleep` and
    /// `spawn`.
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
        if !leaves.iter().all(|n| matches!(*n, "sleep" | "spawn")) {
            return false;
        }
        // The module itself: what the program reaches through it. Anything
        // but `sleep` and `spawn` keeps the line for its own diagnostic.
        let reached = if leaves.is_empty() {
            match self.thread_reaches() {
                Some(r) => r,
                None => return false,
            }
        } else {
            leaves.iter().map(|n| n.to_string()).collect()
        };
        let sleeps = reached.iter().any(|n| n == "sleep");
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
        // `thread::spawn`, or nothing at all once a hand edit moved the
        // spawn into a group: `TaskGroup` is in the prelude, so the line
        // goes, or keeps only the pause (B-2026-11-15-152).
        if !sleeps {
            let message = format!(
                "`{written}` is Rust's standard library -- Kāra starts a task in a \
                 `TaskGroup`, which needs no import, and has no `std.thread` module: \
                 delete the line"
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

    /// The names the program reaches through `thread::x` / `thread.x`
    /// outside import lines, or `None` when one is not `sleep` or `spawn`.
    fn thread_reaches(&self) -> Option<Vec<String>> {
        let mut out = Vec::new();
        for (i, w) in self.tokens.windows(3).enumerate() {
            if !matches!(&w[0].token, Token::Identifier { name, .. } if name == "thread")
                || !matches!(w[1].token, Token::ColonColon | Token::Dot)
                || matches!(w[2].token, Token::Semicolon)
            {
                continue;
            }
            // `std::thread::x` in an import line is the line itself.
            if i >= 2
                && matches!(self.tokens[i - 1].token, Token::ColonColon | Token::Dot)
                && matches!(&self.tokens[i - 2].token, Token::Identifier { name, .. } if name == "std" || name == "core")
            {
                continue;
            }
            match &w[2].token {
                Token::Identifier { name, .. } if name == "sleep" || name == "spawn" => {
                    out.push(name.clone())
                }
                _ => return None,
            }
        }
        Some(out)
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

    /// The fix for a `thread::spawn(..)` (path tokens from `at`, the path
    /// spanning `whole`) that starts a statement or a `let`'s initializer in
    /// a function's own body: a group made on the line before, `let group =
    /// TaskGroup.new();`, and the call written `group.spawn(..)`. The group
    /// joins its task when it drops at the end of the body, which a Rust
    /// program that joins its handle has already done; inside a loop or a
    /// nested block it would join each pass, so there is no fix there.
    pub(crate) fn thread_spawn_group_fix(
        &self,
        at: usize,
        whole: crate::token::Span,
    ) -> Option<Vec<TextEdit>> {
        let token = |i: usize| self.tokens.get(i).map(|t| &t.token);
        let ident = |i: usize| match token(i) {
            Some(Token::Identifier { name, .. }) => Some(name.as_str()),
            _ => None,
        };
        // Where the statement starts: the path itself, or `let [mut] h =`.
        let before = |k: usize| at.checked_sub(k).and_then(token);
        let stmt = if matches!(before(1), Some(Token::Equal)) && ident(at.checked_sub(2)?).is_some()
        {
            match (before(3), before(4)) {
                (Some(Token::Let), _) => at - 3,
                (Some(Token::Mut), Some(Token::Let)) => at - 4,
                _ => return None,
            }
        } else if matches!(
            before(1),
            Some(Token::Semicolon | Token::LeftBrace | Token::RightBrace)
        ) {
            at
        } else {
            return None;
        };
        // The innermost block around it, which must be a function's body.
        let mut depth = 0usize;
        let mut open = None;
        for k in (0..stmt).rev() {
            match token(k) {
                Some(Token::RightBrace) => depth += 1,
                Some(Token::LeftBrace) if depth == 0 => {
                    open = Some(k);
                    break;
                }
                Some(Token::LeftBrace) => depth -= 1,
                _ => {}
            }
        }
        let open = open?;
        let mut parens = 0i64;
        let mut is_fn = false;
        for k in (0..open).rev() {
            match token(k) {
                Some(Token::RightParen | Token::RightBracket) => parens += 1,
                Some(Token::LeftParen | Token::LeftBracket) => parens -= 1,
                Some(Token::Semicolon | Token::LeftBrace | Token::RightBrace) if parens == 0 => {
                    break
                }
                Some(Token::Fn) if parens == 0 => {
                    is_fn = true;
                    break;
                }
                _ => {}
            }
        }
        if !is_fn {
            return None;
        }
        let taken = |n: &str| {
            self.tokens
                .iter()
                .any(|t| matches!(&t.token, Token::Identifier { name, .. } if name == n))
        };
        let group = ["group", "tasks", "task_group"]
            .into_iter()
            .find(|n| !taken(n))?;
        let start = self.tokens[stmt].span.offset;
        let (source, base) = self.source.as_ref()?;
        let line = source.get(..start.checked_sub(*base)?)?;
        let indent = &line[line.rfind('\n').map_or(0, |p| p + 1)..];
        let make = if indent.chars().all(char::is_whitespace) {
            format!("let {group} = TaskGroup.new();\n{indent}")
        } else {
            format!("let {group} = TaskGroup.new(); ")
        };
        Some(vec![
            TextEdit {
                offset: start,
                length: 0,
                replacement: make,
            },
            TextEdit {
                offset: whole.offset,
                length: whole.length,
                replacement: format!("{group}.spawn"),
            },
        ])
    }
}
