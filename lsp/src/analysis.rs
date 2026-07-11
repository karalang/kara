//! The analysis seam between `karac` and the LSP wire types.
//!
//! Everything here is pure (`&str` in, LSP values out) and has no I/O, so it
//! is unit-testable without standing up a server. The server loop
//! (`main.rs`) is deliberately thin over this module.

use lsp_types::{
    Diagnostic, DiagnosticSeverity, DocumentSymbol, Hover, HoverContents, MarkupContent,
    MarkupKind, NumberOrString, Position, Range, SymbolKind, TextEdit,
};
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

    /// Map an LSP [`Position`] (0-based line, UTF-16 character) back to a byte
    /// offset — the inverse of [`Self::position`]. A `character` past the end
    /// of its line clamps to the line end (before the newline); a `line` past
    /// EOF clamps to the document end. Used to turn a hover/definition request
    /// position into the byte offset `karac`'s span-keyed tables expect.
    pub fn offset(&self, pos: Position) -> usize {
        let line = pos.line as usize;
        let Some(&line_start) = self.line_starts.get(line) else {
            return self.source.len();
        };
        let mut utf16 = 0u32;
        for (i, ch) in self.source[line_start..].char_indices() {
            if utf16 >= pos.character || ch == '\n' {
                return line_start + i;
            }
            utf16 += ch.len_utf16() as u32;
        }
        self.source.len()
    }
}

/// Build a hover response for `position` in `text`: the inferred type of the
/// innermost expression under the cursor, rendered as a fenced `kara` code
/// block, ranged to that expression. `None` when nothing typed sits there.
/// `catch_unwind`-guarded for the same reason as [`diagnostics`] — a phase
/// panic on half-typed source must not drop the connection.
pub fn hover(text: &str, position: Position) -> Option<Hover> {
    let index = LineIndex::new(text);
    let offset = index.offset(position);
    let info = match std::panic::catch_unwind(AssertUnwindSafe(|| karac::hover_at(text, offset))) {
        Ok(info) => info?,
        Err(_) => {
            eprintln!("kara-lsp: hover analysis panicked; returning no hover");
            return None;
        }
    };
    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: format!("```kara\n{}\n```", info.type_display),
        }),
        range: Some(index.range(info.span_offset, info.span_length)),
    })
}

/// Resolve the definition range for the reference at `position` (in the same
/// document — single-file today). `None` when nothing navigable sits there
/// (not a resolved reference, or a prelude/builtin with no source location).
/// `catch_unwind`-guarded like [`diagnostics`].
pub fn definition(text: &str, position: Position) -> Option<Range> {
    let index = LineIndex::new(text);
    let offset = index.offset(position);
    let def =
        match std::panic::catch_unwind(AssertUnwindSafe(|| karac::goto_definition(text, offset))) {
            Ok(def) => def?,
            Err(_) => {
                eprintln!("kara-lsp: definition analysis panicked; returning no definition");
                return None;
            }
        };
    Some(index.range(def.span_offset, def.span_length))
}

/// Document outline: one flat [`DocumentSymbol`] per top-level item, in source
/// order. `catch_unwind`-guarded like [`diagnostics`].
pub fn document_symbols(text: &str) -> Vec<DocumentSymbol> {
    let raw = match std::panic::catch_unwind(AssertUnwindSafe(|| karac::document_symbols(text))) {
        Ok(v) => v,
        Err(_) => {
            eprintln!("kara-lsp: document-symbol analysis panicked; returning none");
            return Vec::new();
        }
    };
    let index = LineIndex::new(text);
    raw.into_iter()
        .map(|s| {
            let range = index.range(s.span_offset, s.span_length);
            #[allow(deprecated)] // `deprecated` field is required by the struct
            DocumentSymbol {
                name: s.name,
                detail: None,
                kind: symbol_kind(s.kind),
                tags: None,
                deprecated: None,
                range,
                // Flat outline: no separate name span yet, so the selection
                // range is the whole item (must be ⊆ `range`, which equal is).
                selection_range: range,
                children: None,
            }
        })
        .collect()
}

/// Format the whole document. Returns the edits to apply:
/// - `Some(vec![one full-document replace])` when formatting changes the text;
/// - `Some(vec![])` when it is already formatted (no edits — avoids a needless
///   whole-buffer churn / cursor jump);
/// - `None` when the source doesn't parse (nothing to format) or analysis
///   panics — both map to a null formatting response.
pub fn formatting(text: &str) -> Option<Vec<TextEdit>> {
    let formatted = match std::panic::catch_unwind(AssertUnwindSafe(|| karac::format_source(text)))
    {
        Ok(f) => f?,
        Err(_) => {
            eprintln!("kara-lsp: format analysis panicked; returning no edits");
            return None;
        }
    };
    if formatted == text {
        return Some(Vec::new());
    }
    let index = LineIndex::new(text);
    // A single edit replacing the whole document, from the start to the mapped
    // end position of the original text.
    Some(vec![TextEdit {
        range: Range {
            start: Position::new(0, 0),
            end: index.position(text.len()),
        },
        new_text: formatted,
    }])
}

/// Map `karac`'s coarse kind slug to the LSP symbol kind.
fn symbol_kind(slug: &str) -> SymbolKind {
    match slug {
        "function" => SymbolKind::FUNCTION,
        "struct" => SymbolKind::STRUCT,
        "enum" => SymbolKind::ENUM,
        "interface" => SymbolKind::INTERFACE,
        "constant" => SymbolKind::CONSTANT,
        "variable" => SymbolKind::VARIABLE,
        "class" => SymbolKind::CLASS,
        "namespace" => SymbolKind::NAMESPACE,
        _ => SymbolKind::OBJECT,
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

    #[test]
    fn offset_is_inverse_of_position() {
        let src = "fn main() {\n    let x = 1;\n}";
        let idx = LineIndex::new(src);
        for probe in [0usize, 4, 16, 20, src.len()] {
            let pos = idx.position(probe);
            assert_eq!(idx.offset(pos), probe, "roundtrip failed at {probe}");
        }
    }

    #[test]
    fn offset_counts_utf16_and_clamps() {
        let src = "let s = \"Kāra\";";
        let idx = LineIndex::new(src);
        // position (line 0, char 9) is just after the opening quote (byte 9,
        // since "let s = \"" is 9 ASCII bytes).
        assert_eq!(idx.offset(Position::new(0, 9)), 9);
        // A character past the line end clamps to the line's end (EOF here).
        assert_eq!(idx.offset(Position::new(0, 999)), src.len());
        // A line past EOF clamps to the document end.
        assert_eq!(idx.offset(Position::new(50, 0)), src.len());
    }

    #[test]
    fn hover_reports_type_as_kara_code_block() {
        let src = "fn f(a: i64) -> i64 { a }";
        let idx = LineIndex::new(src);
        let body_a = src.rfind('a').unwrap();
        let pos = idx.position(body_a);
        let h = hover(src, pos).expect("expected a hover");
        match h.contents {
            HoverContents::Markup(m) => {
                assert_eq!(m.kind, MarkupKind::Markdown);
                assert_eq!(m.value, "```kara\ni64\n```");
            }
            other => panic!("expected markup hover, got {other:?}"),
        }
        // Ranged to the single-char `a` under the cursor.
        let r = h.range.expect("hover should carry a range");
        assert_eq!(r.start, pos);
    }

    #[test]
    fn hover_none_on_keyword() {
        assert!(hover("fn f() {}", Position::new(0, 0)).is_none());
    }

    #[test]
    fn definition_ranges_to_the_definition() {
        let src = "fn helper() -> i64 { 1 }\nfn main() { let _ = helper(); }";
        let idx = LineIndex::new(src);
        // the call `helper` is on line 1; find its position
        let call = src.rfind("helper").unwrap();
        let range = definition(src, idx.position(call)).expect("expected a definition range");
        // definition is the item on line 0.
        assert_eq!(range.start, Position::new(0, 0));
        assert!(range.end > range.start);
    }

    #[test]
    fn definition_none_on_prelude() {
        let src = "fn main() { println(\"hi\"); }";
        let idx = LineIndex::new(src);
        let call = src.find("println").unwrap();
        assert!(definition(src, idx.position(call)).is_none());
    }

    #[test]
    fn formatting_replaces_whole_document() {
        let messy = "fn   main( ){let x=1+2;}";
        let edits = formatting(messy).expect("expected format edits");
        assert_eq!(edits.len(), 1, "one full-document replace edit");
        let e = &edits[0];
        assert_eq!(e.range.start, Position::new(0, 0));
        // The replacement is the (changed) formatted text.
        assert_ne!(e.new_text, messy);
        assert!(e.new_text.contains("fn main"));
    }

    #[test]
    fn formatting_already_formatted_yields_no_edits() {
        // Format once, then formatting the result must produce zero edits.
        let formatted = karac::format_source("fn   main(){ }").unwrap();
        assert_eq!(
            formatting(&formatted),
            Some(Vec::new()),
            "already-formatted source needs no edits"
        );
    }

    #[test]
    fn formatting_none_on_parse_error() {
        assert!(formatting("fn main( {").is_none());
    }

    #[test]
    fn document_symbols_maps_kinds() {
        let src = "struct Point { x: i64 }\nfn area() -> i64 { 0 }\nenum Color { Red }";
        let syms = document_symbols(src);
        let got: Vec<(&str, SymbolKind)> = syms.iter().map(|s| (s.name.as_str(), s.kind)).collect();
        assert_eq!(
            got,
            vec![
                ("Point", SymbolKind::STRUCT),
                ("area", SymbolKind::FUNCTION),
                ("Color", SymbolKind::ENUM),
            ]
        );
        // Each symbol's selection range is within its range.
        for s in &syms {
            assert!(s.selection_range.start >= s.range.start);
            assert!(s.selection_range.end <= s.range.end);
        }
    }
}
