//! Rich-display bundles for cell results.
//!
//! Jupyter's `display_data` / `execute_result` messages carry a mime-
//! bundle map (`{ "text/plain": "...", "text/html": "...", ... }`).
//! [`DisplayBundle`] is that map. `%timeline` and `%rc` fill it, and
//! [`render_source`] renders a `%show` value from the source form the MIR
//! interpreter writes it as (the REPL's value snapshots,
//! `crate::mir::interp::REPL_SNAPSHOT_FN`).

use std::collections::HashMap;

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

/// A value's source form (`Row { name: "a", count: 1i64 }`) read as a
/// tree: an atom, or a head and a bracketed list (`Some(…)`, `[…]`,
/// `Row { … }`) whose items may be `label: value` entries (a field, a map
/// entry).
#[derive(Debug, Clone, PartialEq)]
enum Node {
    Atom(String),
    Group {
        head: String,
        open: char,
        items: Vec<Node>,
    },
    Entry(Box<Node>, Box<Node>),
}

fn close_of(open: char) -> char {
    match open {
        '(' => ')',
        '[' => ']',
        _ => '}',
    }
}

struct Reader<'a> {
    chars: std::iter::Peekable<std::str::Chars<'a>>,
}

impl Reader<'_> {
    fn skip_ws(&mut self) {
        while self.chars.peek().is_some_and(|c| c.is_whitespace()) {
            self.chars.next();
        }
    }

    /// A quoted string or char literal, kept as written.
    fn quoted(&mut self, quote: char, out: &mut String) -> Option<()> {
        out.push(self.chars.next()?);
        loop {
            let c = self.chars.next()?;
            out.push(c);
            if c == '\\' {
                out.push(self.chars.next()?);
            } else if c == quote {
                return Some(());
            }
        }
    }

    /// An item: a value, or `value: value`.
    fn item(&mut self) -> Option<Node> {
        let v = self.value()?;
        self.skip_ws();
        if self.chars.peek() == Some(&':') {
            self.chars.next();
            return Some(Node::Entry(Box::new(v), Box::new(self.value()?)));
        }
        Some(v)
    }

    fn value(&mut self) -> Option<Node> {
        self.skip_ws();
        let mut head = String::new();
        while let Some(&c) = self.chars.peek() {
            match c {
                '"' | '\'' => self.quoted(c, &mut head)?,
                '(' | '[' | '{' => {
                    self.chars.next();
                    let mut items = Vec::new();
                    loop {
                        self.skip_ws();
                        match self.chars.peek() {
                            Some(&x) if x == close_of(c) => {
                                self.chars.next();
                                break;
                            }
                            Some(',') => {
                                self.chars.next();
                            }
                            Some(_) => items.push(self.item()?),
                            None => return None,
                        }
                    }
                    return Some(Node::Group {
                        head: head.trim_end().to_string(),
                        open: c,
                        items,
                    });
                }
                ',' | ':' | ')' | ']' | '}' => break,
                _ => {
                    head.push(c);
                    self.chars.next();
                }
            }
        }
        let atom = head.trim().to_string();
        (!atom.is_empty()).then_some(Node::Atom(atom))
    }
}

fn parse_source(source: &str) -> Option<Node> {
    let mut r = Reader {
        chars: source.chars().peekable(),
    };
    let v = r.value()?;
    r.skip_ws();
    r.chars.peek().is_none().then_some(v)
}

/// An atom as it is shown: a number without its type suffix (`3i64` is
/// `3`), anything else as written.
fn shown_atom(a: &str) -> String {
    let suffix =
        regex::Regex::new(r"^(-?[0-9][0-9_.eE+-]*?)(?:[iu](?:8|16|32|64|128|size)|f32|f64)$")
            .expect("valid regex");
    match suffix.captures(a) {
        Some(c) => c[1].to_string(),
        None => a.to_string(),
    }
}

/// A string literal's text, or `None` for anything else.
fn string_text(a: &str) -> Option<String> {
    let body = a.strip_prefix('"')?.strip_suffix('"')?;
    let mut out = String::new();
    let mut chars = body.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next()? {
            'n' => out.push('\n'),
            't' => out.push('\t'),
            'r' => out.push('\r'),
            '0' => out.push('\0'),
            'u' => {
                let hex: String = chars.by_ref().skip(1).take_while(|&c| c != '}').collect();
                out.push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?);
            }
            other => out.push(other),
        }
    }
    Some(out)
}

/// The order `%show` lists a `Map`'s or `Set`'s elements in: numbers by
/// value, strings by their text, groups element by element. A `Map` walks
/// in a per-process hash order, so the shown order is this one.
fn node_cmp(a: &Node, b: &Node) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (a, b) {
        (Node::Atom(x), Node::Atom(y)) => {
            match (shown_atom(x).parse::<f64>(), shown_atom(y).parse::<f64>()) {
                (Ok(p), Ok(q)) => p.partial_cmp(&q).unwrap_or(Ordering::Equal),
                _ => string_text(x)
                    .unwrap_or(x.clone())
                    .cmp(&string_text(y).unwrap_or(y.clone())),
            }
        }
        (Node::Entry(k, _), Node::Entry(l, _)) => node_cmp(k, l),
        (Node::Group { items: x, .. }, Node::Group { items: y, .. }) => x
            .iter()
            .zip(y)
            .map(|(p, q)| node_cmp(p, q))
            .find(|o| o.is_ne())
            .unwrap_or(x.len().cmp(&y.len())),
        _ => Ordering::Equal,
    }
}

/// `n` with each map's entries and each `Set.from([…])`'s elements in
/// [`node_cmp`] order.
fn sorted(n: Node) -> Node {
    match n {
        Node::Group { head, open, items } => {
            let mut items: Vec<Node> = items.into_iter().map(sorted).collect();
            let map = open == '[' && items.iter().all(|i| matches!(i, Node::Entry(..)));
            if map {
                items.sort_by(node_cmp);
            }
            if head == "Set.from" {
                if let [Node::Group { items: elems, .. }] = items.as_mut_slice() {
                    elems.sort_by(node_cmp);
                }
            }
            Node::Group { head, open, items }
        }
        Node::Entry(k, v) => Node::Entry(Box::new(sorted(*k)), Box::new(sorted(*v))),
        atom => atom,
    }
}

fn has_structure(n: &Node) -> bool {
    match n {
        Node::Atom(_) => false,
        Node::Group { items, .. } => !items.is_empty(),
        Node::Entry(_, v) => has_structure(v),
    }
}

fn group_brackets(head: &str, open: char) -> (String, String) {
    let close = close_of(open);
    match (head.is_empty(), open) {
        (false, '{') => (format!("{head} {{"), "}".to_string()),
        _ => (format!("{head}{open}"), close.to_string()),
    }
}

/// `n` on one line.
fn inline(n: &Node, out: &mut String) {
    match n {
        Node::Atom(a) => out.push_str(&shown_atom(a)),
        Node::Entry(k, v) => {
            inline(k, out);
            out.push_str(": ");
            inline(v, out);
        }
        Node::Group { head, open, items } => {
            let (o, c) = group_brackets(head, *open);
            out.push_str(&o);
            let pad = *open == '{' && !items.is_empty();
            if pad {
                out.push(' ');
            }
            for (i, it) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                inline(it, out);
            }
            // A one-element tuple keeps its comma.
            if head.is_empty() && *open == '(' && items.len() == 1 {
                out.push(',');
            }
            if pad {
                out.push(' ');
            }
            out.push_str(&c);
        }
    }
}

/// `n` as `text/plain`: on one line when its items are all atoms, and
/// otherwise one item to a line, indented two spaces a level.
fn plain(n: &Node, depth: usize, out: &mut String) {
    match n {
        Node::Entry(k, v) => {
            inline(k, out);
            out.push_str(": ");
            plain(v, depth, out);
        }
        // A wrapper of one value (`Some(…)`, `Set.from([…])`) wraps it as
        // it is shown.
        Node::Group { head, open, items }
            if !head.is_empty() && *open == '(' && items.len() == 1 =>
        {
            out.push_str(head);
            out.push('(');
            plain(&items[0], depth, out);
            out.push(')');
        }
        Node::Group { head, open, items } if items.iter().any(has_structure) => {
            let (o, c) = group_brackets(head, *open);
            out.push_str(&o);
            out.push('\n');
            for (i, it) in items.iter().enumerate() {
                out.push_str(&"  ".repeat(depth + 1));
                plain(it, depth + 1, out);
                if i + 1 < items.len() || (head.is_empty() && *open == '(' && items.len() == 1) {
                    out.push(',');
                }
                out.push('\n');
            }
            out.push_str(&"  ".repeat(depth));
            out.push_str(&c);
        }
        _ => inline(n, out),
    }
}

/// A `text/html` table for a list of structs of one type, one row per
/// element and one column per field, the columns in alphabetical order;
/// `None` for any other value.
fn html_table(n: &Node) -> Option<String> {
    let Node::Group {
        open: '[', items, ..
    } = n
    else {
        return None;
    };
    let fields_of = |it: &Node| -> Option<(String, Vec<(String, Node)>)> {
        let Node::Group {
            head,
            open: '{',
            items,
        } = it
        else {
            return None;
        };
        let mut fields = Vec::new();
        for f in items {
            let Node::Entry(k, v) = f else { return None };
            let Node::Atom(k) = k.as_ref() else {
                return None;
            };
            fields.push((k.clone(), v.as_ref().clone()));
        }
        fields.sort_by(|a, b| a.0.cmp(&b.0));
        Some((head.clone(), fields))
    };
    let rows: Vec<(String, Vec<(String, Node)>)> =
        items.iter().map(fields_of).collect::<Option<_>>()?;
    let (head, first) = rows.first()?;
    let names: Vec<&String> = first.iter().map(|(k, _)| k).collect();
    if rows
        .iter()
        .any(|(h, fs)| h != head || fs.iter().map(|(k, _)| k).ne(names.iter().copied()))
    {
        return None;
    }
    let mut html = String::from("<table>\n  <thead>\n    <tr>");
    for k in &names {
        html.push_str(&format!("<th>{}</th>", escape_html(k)));
    }
    html.push_str("</tr>\n  </thead>\n  <tbody>\n");
    for (_, fs) in &rows {
        html.push_str("    <tr>");
        for (_, v) in fs {
            let cell = match v {
                Node::Atom(a) => string_text(a).unwrap_or_else(|| shown_atom(a)),
                other => {
                    let mut s = String::new();
                    inline(other, &mut s);
                    s
                }
            };
            html.push_str(&format!("<td>{}</td>", escape_html(&cell)));
        }
        html.push_str("</tr>\n");
    }
    html.push_str("  </tbody>\n</table>");
    Some(html)
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

/// The `%show` display of a value written as `source`: `text/plain`
/// always, and `text/html` for a list of structs of one type. `None` when
/// `source` is not a value's source form.
pub(crate) fn render_source(source: &str) -> Option<DisplayBundle> {
    let n = sorted(parse_source(source)?);
    let mut text = String::new();
    plain(&n, 0, &mut text);
    let mut bundle = DisplayBundle::new().with("text/plain", text);
    if let Some(html) = html_table(&n) {
        bundle = bundle.with("text/html", html);
    }
    Some(bundle)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain_of(source: &str) -> String {
        render_source(source)
            .and_then(|b| b.get("text/plain").map(str::to_string))
            .unwrap_or_else(|| panic!("no rendering of {source}"))
    }

    #[test]
    fn show_renders_atoms_without_suffixes() {
        assert_eq!(plain_of("3i64"), "3");
        assert_eq!(plain_of("-7i32"), "-7");
        assert_eq!(plain_of("1.5f64"), "1.5");
        assert_eq!(plain_of("1.0e300f64"), "1.0e300");
        assert_eq!(plain_of("255u8"), "255");
        assert_eq!(plain_of("true"), "true");
        assert_eq!(plain_of("'x'"), "'x'");
        assert_eq!(plain_of("\"a, b: [c]\""), "\"a, b: [c]\"");
        assert_eq!(plain_of("()"), "()");
        assert!(render_source("3i64").unwrap().get("text/html").is_none());
    }

    #[test]
    fn show_renders_flat_collections_inline() {
        assert_eq!(plain_of("[1i64, 2i64, 3i64]"), "[1, 2, 3]");
        assert_eq!(plain_of("(1i64,)"), "(1,)");
        assert_eq!(plain_of("(1i64, \"a\")"), "(1, \"a\")");
        assert_eq!(plain_of("Vec.new()"), "Vec.new()");
        assert_eq!(plain_of("Some(5i64)"), "Some(5)");
        assert_eq!(plain_of("Shape.Unit"), "Shape.Unit");
        assert_eq!(plain_of("P { x: 1i64, y: -2i64 }"), "P { x: 1, y: -2 }");
    }

    #[test]
    fn show_breaks_nested_values_across_lines() {
        assert_eq!(
            plain_of("Line { a: P { x: 1i64, y: 2i64 }, n: 3i64 }"),
            "Line {\n  a: P { x: 1, y: 2 },\n  n: 3\n}"
        );
        assert_eq!(plain_of("[[1i64], [2i64, 3i64]]"), "[\n  [1],\n  [2, 3]\n]");
        assert_eq!(plain_of("([1i64],)"), "(\n  [1],\n)");
        assert_eq!(
            plain_of("Some(Line { a: P { x: 1i64 }, n: 3i64 })"),
            "Some(Line {\n  a: P { x: 1 },\n  n: 3\n})"
        );
    }

    #[test]
    fn show_orders_map_and_set_elements() {
        assert_eq!(
            plain_of("[10i64: \"b\", 9i64: \"a\", 100i64: \"c\"]"),
            "[9: \"a\", 10: \"b\", 100: \"c\"]"
        );
        assert_eq!(
            plain_of("Set.from([3i64, 1i64, 2i64])"),
            "Set.from([1, 2, 3])"
        );
        assert_eq!(
            plain_of("[\"k\": [2i64, 1i64]]"),
            "[\n  \"k\": [2, 1]\n]",
            "a list's order is its own"
        );
    }

    #[test]
    fn show_renders_a_list_of_structs_as_a_table() {
        let b = render_source(
            "[Row { name: \"a<b\", count: 1i64 }, Row { name: \"it's\", count: 2i64 }]",
        )
        .unwrap();
        let html = b.get("text/html").expect("a table");
        assert!(html.contains("<th>count</th><th>name</th>"), "{html}");
        assert!(html.contains("<td>1</td><td>a&lt;b</td>"), "{html}");
        assert!(html.contains("<td>2</td><td>it&#39;s</td>"), "{html}");
        assert_eq!(b.mimes[0].0, "text/plain");
    }

    #[test]
    fn show_has_no_table_for_mixed_or_single_structs() {
        let mixed = "[A { x: 1i64 }, B { x: 2i64 }]";
        assert!(render_source(mixed).unwrap().get("text/html").is_none());
        let fields = "[A { x: 1i64 }, A { y: 2i64 }]";
        assert!(render_source(fields).unwrap().get("text/html").is_none());
        let single = "A { x: 1i64 }";
        assert!(render_source(single).unwrap().get("text/html").is_none());
        assert!(render_source("[1i64]").unwrap().get("text/html").is_none());
    }

    #[test]
    fn show_refuses_what_is_not_a_source_form() {
        assert!(render_source("[1i64").is_none());
        assert!(render_source("").is_none());
    }
}
