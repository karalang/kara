//! Rich-display rendering for cell results.
//!
//! Tracker entry: `docs/implementation_checklist/phase-5-diagnostics.md`
//! § "Rich display for structs and collections" (P1+ stretch, line 761).
//!
//! Jupyter's `display_data` / `execute_result` messages carry a mime-
//! bundle map (`{ "text/plain": "...", "text/html": "...", ... }`).
//! [`render_display`] converts a runtime [`Value`] into a
//! [`DisplayBundle`] suitable for that map:
//!
//! - `text/plain` is **always** present. Pretty-printed for nested data
//!   (multi-line, indented); atoms and arrays of atoms stay inline.
//! - `text/html` is emitted **only** for arrays of homogeneously-shaped
//!   structs (`Vec[Struct]` / `Slice[Struct]`) — one row per element,
//!   one column per field, columns in alphabetical order so the table
//!   is stable across runs. Unstyled `<table>` markup; Jupyter
//!   frontends apply their own CSS.
//! - `image/png` is reserved for plotting (line 761's third bullet);
//!   deferred to v1.1.x alongside the stdlib plotting work.
//!
//! Reflection is structural: the renderer walks the [`Value`] tree at
//! display time. The tracker's "uses the type registry" phrasing was
//! aspirational — walking the live value gets the same surface (field
//! names, nesting depth) without coupling display to the typechecker's
//! internal data, and works on the tree-walk interpreter today.
//!
//! HashMap iteration order is nondeterministic, so [`Value::Struct`]
//! fields are emitted in alphabetical order in both the plain and the
//! HTML paths. The HTML path additionally requires every row to expose
//! the same field set in the same order, which the sort makes free.
//!
//! Tests live at the bottom of this file (in-module). Integration via
//! the `%show` magic + the Jupyter kernel's `display_data` broadcast
//! is exercised in `tests/repl.rs` and `kernel/src/runtime.rs::tests`.

use std::collections::HashMap;

use crate::interpreter::Value;

/// One cell result rendered into one or more mime types. The order of
/// `mimes` is preserved so callers (and tests) can rely on `text/plain`
/// always appearing first. Jupyter frontends pick the richest mime they
/// understand from the bundle; `text/plain` is the universal fallback.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DisplayBundle {
    pub mimes: Vec<(String, String)>,
}

impl DisplayBundle {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with(mut self, mime: impl Into<String>, body: impl Into<String>) -> Self {
        self.mimes.push((mime.into(), body.into()));
        self
    }

    pub fn get(&self, mime: &str) -> Option<&str> {
        self.mimes
            .iter()
            .find_map(|(m, b)| (m == mime).then_some(b.as_str()))
    }

    pub fn is_empty(&self) -> bool {
        self.mimes.is_empty()
    }

    /// Lossy conversion to a `HashMap` — useful for plumbing into JSON
    /// serialization where the bundle becomes a JSON object.
    pub fn into_map(self) -> HashMap<String, String> {
        self.mimes.into_iter().collect()
    }
}

/// Convert a runtime [`Value`] into a mime bundle suitable for the
/// Jupyter kernel's `display_data` message. Always emits `text/plain`;
/// emits `text/html` when the value is an array of homogeneously-
/// shaped structs.
pub fn render_display(value: &Value) -> DisplayBundle {
    let mut bundle = DisplayBundle::new().with("text/plain", render_text_plain(value));
    if let Some(html) = render_text_html(value) {
        bundle = bundle.with("text/html", html);
    }
    bundle
}

/// Pretty-print a [`Value`] as plain text. Atoms format inline; nested
/// collections (Array / Map / Struct whose elements contain further
/// structure) break across lines with two-space indentation per
/// nesting level. Exposed for the `%show` magic and for tests; the
/// Jupyter kernel reaches it through [`render_display`].
pub fn render_text_plain(value: &Value) -> String {
    let mut out = String::new();
    write_text_plain(&mut out, value, 0);
    out
}

/// Detect a Vec[Struct] / Slice[Struct] shape and emit an HTML table
/// for it. Returns `None` for any other Value (single struct, mixed-
/// shape array, array of atoms, etc.) — the plain-text rendering is
/// still adequate in those cases. Fields are alphabetically sorted so
/// columns are stable across runs (HashMap iteration order is not).
fn render_text_html(value: &Value) -> Option<String> {
    let items_owned;
    let items: &[Value] = match value {
        Value::Array(rc) => {
            items_owned = rc.read().ok()?.clone();
            &items_owned
        }
        Value::Slice {
            storage,
            start,
            len,
            ..
        } => {
            let read = storage.read().ok()?;
            items_owned = read[*start..*start + *len].to_vec();
            &items_owned
        }
        _ => return None,
    };
    let (_name, fields, rows) = extract_struct_rows(items)?;
    let mut html = String::new();
    html.push_str("<table>\n  <thead>\n    <tr>");
    for f in &fields {
        html.push_str("<th>");
        html.push_str(&escape_html(f));
        html.push_str("</th>");
    }
    html.push_str("</tr>\n  </thead>\n  <tbody>\n");
    for row in &rows {
        html.push_str("    <tr>");
        for cell in row {
            html.push_str("<td>");
            html.push_str(&escape_html(cell));
            html.push_str("</td>");
        }
        html.push_str("</tr>\n");
    }
    html.push_str("  </tbody>\n</table>");
    Some(html)
}

/// Inspect a slice of values; if every element is a `Value::Struct`
/// with the same name and field set, return `(struct_name, sorted_field_
/// names, rows_as_strings)`. The strict shape requirement means a
/// heterogeneous array (`[Point{...}, 42]`) bails out and the caller
/// emits text-plain only.
fn extract_struct_rows(items: &[Value]) -> Option<(String, Vec<String>, Vec<Vec<String>>)> {
    if items.is_empty() {
        return None;
    }
    let (name, mut fields) = match &items[0] {
        Value::Struct { name, fields } => {
            (name.clone(), fields.keys().cloned().collect::<Vec<_>>())
        }
        _ => return None,
    };
    fields.sort();
    let mut rows: Vec<Vec<String>> = Vec::with_capacity(items.len());
    for v in items {
        match v {
            Value::Struct {
                name: n,
                fields: fs,
            } if *n == name && fs.len() == fields.len() => {
                let mut row = Vec::with_capacity(fields.len());
                for k in &fields {
                    let field_v = fs.get(k)?;
                    row.push(render_text_inline(field_v));
                }
                rows.push(row);
            }
            _ => return None,
        }
    }
    Some((name, fields, rows))
}

/// Single-line representation for a table cell. Uses the existing
/// `Display` impl which already produces one-line forms for atoms; for
/// nested structures we still flatten via `Display` because table cells
/// don't accommodate multi-line layout cleanly.
fn render_text_inline(value: &Value) -> String {
    format!("{}", value)
}

fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

fn write_text_plain(out: &mut String, value: &Value, depth: usize) {
    match value {
        Value::Array(rc) => {
            let arr = rc.read().unwrap();
            write_seq_plain(out, &arr, "[", "]", depth);
        }
        Value::Slice {
            storage,
            start,
            len,
            ..
        } => {
            let arr = storage.read().unwrap();
            write_seq_plain(out, &arr[*start..*start + *len], "[", "]", depth);
        }
        Value::Map(entries) => write_map_plain(out, entries, depth),
        Value::Struct { name, fields } => write_struct_plain(out, name, fields, depth),
        Value::Tuple(vals) => {
            // Tuples follow the same multi-line trigger as arrays — if
            // any element contains nested structure, break across lines.
            if vals.iter().any(is_nested) {
                let pad = "  ".repeat(depth);
                let inner = "  ".repeat(depth + 1);
                out.push_str("(\n");
                for (i, v) in vals.iter().enumerate() {
                    out.push_str(&inner);
                    write_text_plain(out, v, depth + 1);
                    if i + 1 < vals.len() {
                        out.push(',');
                    }
                    out.push('\n');
                }
                out.push_str(&pad);
                out.push(')');
            } else {
                out.push_str(&format!("{}", value));
            }
        }
        _ => {
            // Falls through to the existing one-line `Display` impl —
            // atoms, enum variants, functions, channels, etc.
            out.push_str(&format!("{}", value));
        }
    }
}

fn write_seq_plain(out: &mut String, items: &[Value], open: &str, close: &str, depth: usize) {
    if items.iter().any(is_nested) {
        let pad = "  ".repeat(depth);
        let inner = "  ".repeat(depth + 1);
        out.push_str(open);
        out.push('\n');
        for (i, v) in items.iter().enumerate() {
            out.push_str(&inner);
            write_text_plain(out, v, depth + 1);
            if i + 1 < items.len() {
                out.push(',');
            }
            out.push('\n');
        }
        out.push_str(&pad);
        out.push_str(close);
    } else {
        out.push_str(open);
        for (i, v) in items.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            out.push_str(&format!("{}", v));
        }
        out.push_str(close);
    }
}

fn write_map_plain(out: &mut String, entries: &[(Value, Value)], depth: usize) {
    if entries.iter().any(|(_, v)| is_nested(v)) {
        let pad = "  ".repeat(depth);
        let inner = "  ".repeat(depth + 1);
        out.push_str("{\n");
        for (i, (k, v)) in entries.iter().enumerate() {
            out.push_str(&inner);
            out.push_str(&format!("{}: ", k));
            write_text_plain(out, v, depth + 1);
            if i + 1 < entries.len() {
                out.push(',');
            }
            out.push('\n');
        }
        out.push_str(&pad);
        out.push('}');
    } else {
        out.push('{');
        for (i, (k, v)) in entries.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            out.push_str(&format!("{}: {}", k, v));
        }
        out.push('}');
    }
}

fn write_struct_plain(out: &mut String, name: &str, fields: &HashMap<String, Value>, depth: usize) {
    let mut keys: Vec<&String> = fields.keys().collect();
    keys.sort();
    if keys.iter().any(|k| is_nested(&fields[*k])) {
        let pad = "  ".repeat(depth);
        let inner = "  ".repeat(depth + 1);
        out.push_str(name);
        out.push_str(" {\n");
        for (i, k) in keys.iter().enumerate() {
            let v = &fields[*k];
            out.push_str(&inner);
            out.push_str(k);
            out.push_str(": ");
            write_text_plain(out, v, depth + 1);
            if i + 1 < keys.len() {
                out.push(',');
            }
            out.push('\n');
        }
        out.push_str(&pad);
        out.push('}');
    } else {
        out.push_str(name);
        out.push_str(" { ");
        for (i, k) in keys.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            out.push_str(k);
            out.push_str(": ");
            out.push_str(&format!("{}", &fields[*k]));
        }
        out.push_str(" }");
    }
}

/// A value is "nested" iff it contains structure that benefits from
/// multi-line layout. Atoms and bare-collections-of-atoms stay inline.
fn is_nested(value: &Value) -> bool {
    matches!(
        value,
        Value::Array(_)
            | Value::Slice { .. }
            | Value::Map(_)
            | Value::Struct { .. }
            | Value::SharedStruct(_)
            | Value::Tuple(_)
    )
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, RwLock};

    use super::*;

    fn s(name: &str, fields: &[(&str, Value)]) -> Value {
        let mut map = HashMap::new();
        for (k, v) in fields {
            map.insert((*k).to_string(), v.clone());
        }
        Value::Struct {
            name: name.to_string(),
            fields: map,
        }
    }

    fn arr(items: Vec<Value>) -> Value {
        Value::Array(Arc::new(RwLock::new(items)))
    }

    #[test]
    fn atom_emits_text_plain_only() {
        let b = render_display(&Value::Int(42));
        assert_eq!(b.mimes.len(), 1);
        assert_eq!(b.get("text/plain"), Some("42"));
        assert!(b.get("text/html").is_none());
    }

    #[test]
    fn string_emits_text_plain_only() {
        let b = render_display(&Value::String("hi".to_string()));
        assert_eq!(b.get("text/plain"), Some("hi"));
        assert!(b.get("text/html").is_none());
    }

    #[test]
    fn empty_array_emits_text_plain_only() {
        let b = render_display(&arr(vec![]));
        assert_eq!(b.get("text/plain"), Some("[]"));
        assert!(b.get("text/html").is_none());
    }

    #[test]
    fn array_of_ints_stays_inline() {
        let b = render_display(&arr(vec![Value::Int(1), Value::Int(2), Value::Int(3)]));
        assert_eq!(b.get("text/plain"), Some("[1, 2, 3]"));
        assert!(b.get("text/html").is_none());
    }

    #[test]
    fn vec_struct_emits_html_table() {
        let val = arr(vec![
            s("Point", &[("x", Value::Int(1)), ("y", Value::Int(2))]),
            s("Point", &[("x", Value::Int(3)), ("y", Value::Int(4))]),
        ]);
        let b = render_display(&val);
        let html = b.get("text/html").expect("vec[Struct] must emit text/html");
        // Headers in alphabetical order.
        assert!(html.contains("<th>x</th><th>y</th>"), "headers: {html}");
        // First row reads 1 / 2 in x / y order.
        assert!(html.contains("<td>1</td><td>2</td>"), "row 1: {html}");
        assert!(html.contains("<td>3</td><td>4</td>"), "row 2: {html}");
        assert!(html.starts_with("<table>"), "wrapper: {html}");
        assert!(html.contains("<thead>"));
        assert!(html.contains("<tbody>"));
    }

    #[test]
    fn vec_struct_text_plain_pretty_prints_multiline() {
        let val = arr(vec![
            s("Point", &[("x", Value::Int(1)), ("y", Value::Int(2))]),
            s("Point", &[("x", Value::Int(3)), ("y", Value::Int(4))]),
        ]);
        let plain = render_text_plain(&val);
        // Structs are nested data, so the array goes multi-line.
        assert!(plain.starts_with("[\n"), "should multi-line: {plain:?}");
        assert!(plain.contains("Point { x: 1, y: 2 }"), "row 1: {plain:?}");
        assert!(plain.contains("Point { x: 3, y: 4 }"), "row 2: {plain:?}");
        assert!(plain.ends_with("]"));
    }

    #[test]
    fn vec_struct_html_escapes_special_chars() {
        let val = arr(vec![s(
            "Row",
            &[
                ("name", Value::String("<script>".to_string())),
                ("note", Value::String("a & b".to_string())),
            ],
        )]);
        let html = render_display(&val).get("text/html").unwrap().to_string();
        assert!(html.contains("&lt;script&gt;"), "lt/gt: {html}");
        assert!(html.contains("a &amp; b"), "amp: {html}");
        assert!(!html.contains("<script>"), "raw: {html}");
    }

    #[test]
    fn mixed_shape_array_no_html() {
        let val = arr(vec![
            s("Point", &[("x", Value::Int(1)), ("y", Value::Int(2))]),
            Value::Int(99),
        ]);
        assert!(render_display(&val).get("text/html").is_none());
    }

    #[test]
    fn different_struct_names_no_html() {
        let val = arr(vec![
            s("A", &[("x", Value::Int(1))]),
            s("B", &[("x", Value::Int(2))]),
        ]);
        assert!(render_display(&val).get("text/html").is_none());
    }

    #[test]
    fn struct_with_missing_field_no_html() {
        let val = arr(vec![
            s("R", &[("x", Value::Int(1)), ("y", Value::Int(2))]),
            s("R", &[("x", Value::Int(3))]),
        ]);
        assert!(render_display(&val).get("text/html").is_none());
    }

    #[test]
    fn single_struct_no_html() {
        let val = s("Point", &[("x", Value::Int(1)), ("y", Value::Int(2))]);
        let b = render_display(&val);
        assert!(b.get("text/html").is_none());
        assert_eq!(b.get("text/plain"), Some("Point { x: 1, y: 2 }"));
    }

    #[test]
    fn nested_struct_pretty_prints() {
        let inner = arr(vec![Value::Int(1), Value::Int(2)]);
        let outer = s("Holder", &[("items", inner)]);
        let plain = render_text_plain(&outer);
        assert!(plain.starts_with("Holder {\n"), "multi-line: {plain}");
        assert!(plain.contains("items: [1, 2]"), "inner inline: {plain}");
        assert!(plain.ends_with("}"));
    }

    #[test]
    fn map_with_atom_values_inline() {
        let m = Value::Map(vec![
            (Value::String("a".to_string()), Value::Int(1)),
            (Value::String("b".to_string()), Value::Int(2)),
        ]);
        let plain = render_text_plain(&m);
        assert_eq!(plain, "{a: 1, b: 2}");
    }

    #[test]
    fn map_with_nested_values_multiline() {
        let m = Value::Map(vec![(
            Value::String("xs".to_string()),
            arr(vec![Value::Int(1), Value::Int(2)]),
        )]);
        let plain = render_text_plain(&m);
        assert!(plain.starts_with("{\n"), "multi-line: {plain}");
        assert!(plain.contains("xs: [1, 2]"), "nested inline: {plain}");
    }

    #[test]
    fn tuple_with_nested_multiline() {
        let t = Value::Tuple(vec![Value::Int(1), arr(vec![Value::Int(2), Value::Int(3)])]);
        let plain = render_text_plain(&t);
        assert!(plain.starts_with("(\n"), "multi-line: {plain}");
        assert!(plain.contains("1"));
        assert!(plain.contains("[2, 3]"));
    }

    #[test]
    fn tuple_atoms_inline() {
        let t = Value::Tuple(vec![Value::Int(1), Value::Int(2)]);
        let plain = render_text_plain(&t);
        assert_eq!(plain, "(1, 2)");
    }

    #[test]
    fn slice_of_struct_emits_html() {
        let storage = Arc::new(RwLock::new(vec![
            s("R", &[("x", Value::Int(1))]),
            s("R", &[("x", Value::Int(2))]),
            s("R", &[("x", Value::Int(3))]),
        ]));
        let slice = Value::Slice {
            storage,
            start: 1,
            len: 2,
            mutable: false,
        };
        let b = render_display(&slice);
        let html = b.get("text/html").expect("slice[Struct] also emits html");
        assert!(html.contains("<td>2</td>"));
        assert!(html.contains("<td>3</td>"));
        assert!(!html.contains("<td>1</td>"), "slice should skip element 0");
    }

    #[test]
    fn html_columns_sorted_alphabetically() {
        // Insertion order varies because HashMap, so spell-check by
        // confirming alphabetical column order.
        let val = arr(vec![s(
            "R",
            &[
                ("zeta", Value::Int(1)),
                ("alpha", Value::Int(2)),
                ("middle", Value::Int(3)),
            ],
        )]);
        let html = render_display(&val).get("text/html").unwrap().to_string();
        let alpha_pos = html.find("<th>alpha</th>").unwrap();
        let middle_pos = html.find("<th>middle</th>").unwrap();
        let zeta_pos = html.find("<th>zeta</th>").unwrap();
        assert!(
            alpha_pos < middle_pos && middle_pos < zeta_pos,
            "alphabetical: {html}"
        );
    }

    #[test]
    fn bundle_into_map_round_trip() {
        let b = DisplayBundle::new()
            .with("text/plain", "hello")
            .with("text/html", "<b>hi</b>");
        let m = b.into_map();
        assert_eq!(m.get("text/plain").map(String::as_str), Some("hello"));
        assert_eq!(m.get("text/html").map(String::as_str), Some("<b>hi</b>"));
        assert_eq!(m.len(), 2);
    }

    #[test]
    fn bundle_get_returns_none_for_unknown_mime() {
        let b = render_display(&Value::Int(1));
        assert!(b.get("image/png").is_none());
        assert!(b.get("application/json").is_none());
    }
}
