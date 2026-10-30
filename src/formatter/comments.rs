//! Comment preservation for the canonical formatter (B-2026-10-30-16).
//!
//! The AST keeps no `//` or `/* */` comment, and the printer reads `///` and
//! `//!` text off no node either, so formatting from the tree alone deletes
//! every comment in the file. [`collect_comments`] lifts them out of the
//! source instead: a comment is whatever sits in the gap between two lexer
//! tokens, plus the doc-comment tokens themselves. The printer then writes
//! each one back at the first line boundary it reaches past the comment's
//! offset: before the next item, statement, field, variant, arm or closing
//! brace. A comment that ended a line of code goes back on the end of the
//! line just printed; one on its own line gets its own line at the current
//! indent. Whatever is left is written at the end of the file, so a comment
//! can move (one inside an expression lands after it) but is never lost.

use crate::token::Token;

pub(crate) struct Comment {
    pub(super) start: usize,
    pub(super) text: String,
    /// Nothing but whitespace precedes it on its line.
    pub(super) own_line: bool,
    /// A blank line separates it from what precedes it.
    pub(super) blank_before: bool,
    /// A blank line separates it from what follows it, as under a file's
    /// header comment.
    pub(super) blank_after: bool,
    /// A `//` comment, which runs to the end of its line.
    pub(super) is_line: bool,
}

pub(super) fn collect_comments(source: &str) -> Vec<Comment> {
    let mut out = Vec::new();
    let mut prev_end = 0;
    for tok in crate::tokenize(source) {
        let start = tok.span.offset.min(source.len());
        if start > prev_end {
            scan_gap(source, prev_end, start, &mut out);
        }
        let end = (tok.span.offset + tok.span.length).min(source.len());
        if matches!(tok.token, Token::DocComment(_) | Token::ModuleDocComment(_)) {
            push_comment(source, start, end, true, &mut out);
        }
        prev_end = prev_end.max(end);
        if tok.token == Token::EOF {
            break;
        }
    }
    out
}

/// The comments in `source[lo..hi]`, a gap between two tokens. A gap holds
/// only whitespace and comments, so anything else means the gap was not one
/// (a token whose span does not cover its text) and the scan stops rather
/// than read a `//` inside it as a comment.
fn scan_gap(source: &str, lo: usize, hi: usize, out: &mut Vec<Comment>) {
    let b = source.as_bytes();
    let mut i = lo;
    while i < hi {
        match b[i] {
            b' ' | b'\t' | b'\r' | b'\n' => i += 1,
            b'/' if i + 1 < hi && b[i + 1] == b'/' => {
                let s = i;
                while i < hi && b[i] != b'\n' {
                    i += 1;
                }
                push_comment(source, s, i, true, out);
            }
            b'/' if i + 1 < hi && b[i + 1] == b'*' => {
                let s = i;
                i += 2;
                let mut depth = 1;
                while depth > 0 && i < hi {
                    if b[i] == b'/' && i + 1 < hi && b[i + 1] == b'*' {
                        depth += 1;
                        i += 2;
                    } else if b[i] == b'*' && i + 1 < hi && b[i + 1] == b'/' {
                        depth -= 1;
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
                push_comment(source, s, i, false, out);
            }
            _ => return,
        }
    }
}

fn push_comment(source: &str, s: usize, e: usize, is_line: bool, out: &mut Vec<Comment>) {
    let line_start = source[..s].rfind('\n').map_or(0, |n| n + 1);
    let own_line = source[line_start..s].trim().is_empty();
    let blank_before = own_line && line_start > 0 && {
        let prev_start = source[..line_start - 1].rfind('\n').map_or(0, |n| n + 1);
        prev_start > 0 && source[prev_start..line_start - 1].trim().is_empty()
    };
    let blank_after = own_line && {
        let rest = &source[e..];
        let line_end = rest.find('\n');
        line_end.is_some_and(|n| {
            rest[..n].trim().is_empty() && {
                let next = &rest[n + 1..];
                let next_end = next.find('\n').unwrap_or(next.len());
                next_end < next.len() && next[..next_end].trim().is_empty()
            }
        })
    };
    out.push(Comment {
        start: s,
        text: source[s..e].trim_end().to_string(),
        own_line,
        blank_before,
        blank_after,
        is_line,
    });
}

impl super::Formatter {
    /// True when a comment not yet written starts in `lo..hi`.
    pub(super) fn has_comment_in(&self, lo: usize, hi: usize) -> bool {
        self.comments[self.next_comment..]
            .iter()
            .any(|c| c.start >= lo && c.start < hi)
    }

    /// Write every comment that starts before `offset`. Called where the next
    /// thing printed begins a line, which is the only place a `//` comment
    /// can go without commenting out code; mid-line it does nothing, and the
    /// comments wait for the next line boundary.
    pub(super) fn flush_comments_before(&mut self, offset: usize) {
        self.flush_comments(offset, false);
    }

    /// Write the comments before `offset` that ended a line of code, onto the
    /// line just printed: called before a separator line is written, so a
    /// `} // end` stays on its item rather than moving below the blank line.
    pub(super) fn flush_trailing_comments(&mut self, offset: usize) {
        self.flush_comments(offset, true);
    }

    fn flush_comments(&mut self, offset: usize, trailing_only: bool) {
        while let Some(c) = self.comments.get(self.next_comment) {
            if c.start >= offset || (trailing_only && c.own_line) {
                break;
            }
            if !(self.output.is_empty() || self.output.ends_with('\n')) {
                return;
            }
            let (text, own_line, blank_before, blank_after, is_line) = (
                c.text.clone(),
                c.own_line,
                c.blank_before,
                c.blank_after,
                c.is_line,
            );
            self.next_comment += 1;
            let last_line_empty = self.output.is_empty() || self.output.ends_with("\n\n");
            // A line that already ends in a `//` comment cannot take another
            // comment after it: a block comment there would end up inside it.
            let after_line_comment = self.output.len() == self.line_comment_end;
            if !own_line && !last_line_empty && !after_line_comment {
                self.output.pop();
                self.output.push(' ');
            } else {
                if blank_before && !last_line_empty && !self.output.ends_with("{\n") {
                    self.output.push('\n');
                }
                self.write_indent();
            }
            self.output.push_str(&text);
            self.output.push('\n');
            if is_line {
                self.line_comment_end = self.output.len();
            }
            if own_line && blank_after {
                self.output.push('\n');
            }
        }
    }
}
