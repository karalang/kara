//! The analysis seam between `karac` and the LSP wire types.
//!
//! Everything here is pure (`&str` in, LSP values out) and has no I/O, so it
//! is unit-testable without standing up a server. The server loop
//! (`main.rs`) is deliberately thin over this module.

use lsp_types::{Diagnostic, DiagnosticSeverity, NumberOrString, Position, Range};
use std::panic::AssertUnwindSafe;

/// Byte-offset → LSP [`Position`] converter for one source document.
///
/// LSP positions are **0-based** `(line, character)` where `character` counts
/// **UTF-16 code units** from the line start — not bytes, not Unicode scalar
/// values. `karac` spans are byte `offset` + `length` (with 1-based
/// line/column for humans, which we do not use here — byte offsets are the
/// unambiguous key). This precomputes the byte offset of each line start once
/// so every span maps in `O(log lines + line length)` and non-ASCII source
/// (Kāra source routinely contains non-ASCII — the language is named `Kāra`)
/// still places squiggles correctly.
pub struct LineIndex<'a> {
    source: &'a str,
    /// Byte offset of the first character of each line. Always starts with 0;
    /// one entry per line.
    line_starts: Vec<usize>,
}

impl<'a> LineIndex<'a> {
    pub fn new(source: &'a str) -> Self {
        let mut line_starts = vec![0usize];
        for (i, b) in source.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(i + 1);
            }
        }
        LineIndex {
            source,
            line_starts,
        }
    }

    /// Map a byte offset (clamped into `[0, len]`) to an LSP position.
    pub fn position(&self, byte_offset: usize) -> Position {
        let offset = byte_offset.min(self.source.len());
        // Largest line whose start is <= offset.
        let line = self.line_starts.partition_point(|&s| s <= offset) - 1;
        let line_start = self.line_starts[line];
        // `offset` may land inside a multi-byte char if a span is malformed;
        // walk only whole chars up to it and stop at the last char boundary
        // <= offset so we never slice mid-codepoint.
        let mut character = 0u32;
        for (i, ch) in self.source[line_start..].char_indices() {
            if line_start + i >= offset {
                break;
            }
            character += ch.len_utf16() as u32;
        }
        Position {
            line: line as u32,
            character,
        }
    }

    /// Map a byte `offset..offset+length` span to an LSP range.
    pub fn range(&self, offset: usize, length: usize) -> Range {
        Range {
            start: self.position(offset),
            end: self.position(offset.saturating_add(length)),
        }
    }
}

/// Run `source` through `karac`'s static-check pipeline and shape the result
/// into LSP diagnostics.
///
/// The analysis is wrapped in [`std::panic::catch_unwind`]: a compiler-phase
/// panic on half-typed source must never take down the language server (the
/// editor would drop the connection and the user would see the whole feature
/// die on one keystroke). On a panic we return no diagnostics for this pass —
/// a stale-but-alive server beats a dead one — and log to stderr.
pub fn diagnostics(source: &str) -> Vec<Diagnostic> {
    let raw = match std::panic::catch_unwind(AssertUnwindSafe(|| karac::check_source(source))) {
        Ok(diags) => diags,
        Err(_) => {
            eprintln!("kara-lsp: analysis panicked; skipping diagnostics for this revision");
            return Vec::new();
        }
    };

    let index = LineIndex::new(source);
    raw.into_iter()
        .map(|d| Diagnostic {
            range: index.range(d.offset, d.length),
            severity: Some(DiagnosticSeverity::ERROR),
            // Carry the producing phase (parse/resolve/typecheck/effect/
            // ownership) as the diagnostic code so an editor can group/filter
            // by it; `source` names the producer for the "Problems" panel.
            code: Some(NumberOrString::String(d.phase.to_string())),
            source: Some("kara".to_string()),
            message: d.message,
            ..Default::default()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_index_ascii_positions() {
        let src = "fn main() {\n    let x = 1;\n}";
        let idx = LineIndex::new(src);
        // offset 0 → line 0, char 0
        assert_eq!(idx.position(0), Position::new(0, 0));
        // the `let` on line 1 starts at byte 16 (after "fn main() {\n" = 12
        // bytes, then 4 spaces = 16)
        assert_eq!(idx.position(16), Position::new(1, 4));
        // the closing brace is on line 2, char 0
        let brace = src.rfind('}').unwrap();
        assert_eq!(idx.position(brace), Position::new(2, 0));
    }

    #[test]
    fn line_index_counts_utf16_units_not_bytes() {
        // `ā` is 2 bytes in UTF-8 but 1 UTF-16 code unit; an emoji outside the
        // BMP is 4 bytes / 2 UTF-16 units. The character column must reflect
        // UTF-16 units so editors place the cursor correctly.
        let src = "let s = \"Kāra🌀\";";
        let idx = LineIndex::new(src);
        // byte offset just past the closing quote
        let after_quote = src.rfind('"').unwrap() + 1;
        let pos = idx.position(after_quote);
        assert_eq!(pos.line, 0);
        // "let s = \"" = 9 chars/units, then K=1, ā=1, r=1, a=1, 🌀=2, "=1
        // → 9 + 1+1+1+1 + 2 + 1 = 16 UTF-16 units
        assert_eq!(pos.character, 16);
    }

    #[test]
    fn line_index_clamps_out_of_range_offset() {
        let src = "abc";
        let idx = LineIndex::new(src);
        // A malformed span past EOF must clamp to the end, not panic.
        assert_eq!(idx.position(999), Position::new(0, 3));
    }

    #[test]
    fn diagnostics_clean_source_is_empty() {
        assert!(diagnostics("fn main() { let x = 1 + 2; }").is_empty());
    }

    #[test]
    fn diagnostics_reports_resolve_error_with_range_and_phase() {
        let diags = diagnostics("fn main() { let _ = undefined_name(); }");
        assert!(!diags.is_empty());
        let d = &diags[0];
        assert_eq!(d.severity, Some(DiagnosticSeverity::ERROR));
        assert_eq!(d.source.as_deref(), Some("kara"));
        assert_eq!(d.code, Some(NumberOrString::String("resolve".to_string())));
        // A non-empty, well-ordered range on line 0.
        assert_eq!(d.range.start.line, 0);
        assert!(d.range.end >= d.range.start);
        assert!(d.range.end.character > d.range.start.character);
    }

    #[test]
    fn diagnostics_reports_parse_error() {
        let diags = diagnostics("fn main() { let = ; }");
        assert!(!diags.is_empty());
        assert!(diags
            .iter()
            .all(|d| d.code == Some(NumberOrString::String("parse".to_string()))));
    }
}
