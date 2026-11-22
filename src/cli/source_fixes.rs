//! Fixes read off the program's text for diagnostics whose help names one
//! determined edit but whose emitting site has no place to compute it: the
//! site knows the span and the remedy, and the source says where the edit
//! goes (the `else` of an irrefutable `if let`, the `pub` before `main`,
//! the `#[derive(..)]` line above a union). Each runs after its phase, over
//! the diagnostics that carry no fix of their own, and on the program's
//! own text only, never the library text appended after it; a fixer that
//! cannot place its edit exactly gives none (the karac fix coverage audit,
//! 2026-11-21, B-2026-11-21-72).

use crate::resolver::{SpanKey, TextEdit};
use crate::token::Span;
use crate::typechecker::{FixIt, TypeError};

fn edit(offset: usize, length: usize, replacement: &str) -> TextEdit {
    TextEdit {
        offset,
        length,
        replacement: replacement.to_string(),
    }
}

fn is_ident(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// `i` moved back over the whitespace before it.
fn trim_back(src: &[u8], mut i: usize) -> usize {
    while i > 0 && src[i - 1].is_ascii_whitespace() {
        i -= 1;
    }
    i
}

/// `i` moved forward over the whitespace at it.
fn trim_fwd(src: &[u8], mut i: usize) -> usize {
    while i < src.len() && src[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

/// The word that ends at `end` (exclusive), with where it starts.
fn word_ending_at(src: &[u8], end: usize) -> Option<(usize, &str)> {
    let mut s = end;
    while s > 0 && is_ident(src[s - 1]) {
        s -= 1;
    }
    (s < end).then(|| (s, std::str::from_utf8(&src[s..end]).unwrap_or("")))
}

/// Whether the word `w` starts at `i` and is a whole word there.
fn word_at(src: &[u8], i: usize, w: &str) -> bool {
    src.get(i..i + w.len()) == Some(w.as_bytes())
        && (i == 0 || !is_ident(src[i - 1]))
        && src.get(i + w.len()).is_none_or(|b| !is_ident(*b))
}

/// Past a string, character or comment starting at `i`, or `None` when
/// none starts there.
fn skip_literal(src: &[u8], i: usize) -> Option<usize> {
    match src[i] {
        b'"' => {
            let mut j = i + 1;
            while j < src.len() && src[j] != b'"' {
                j += if src[j] == b'\\' { 2 } else { 1 };
            }
            Some((j + 1).min(src.len()))
        }
        b'\'' if src.get(i + 2) == Some(&b'\'') || src.get(i + 1) == Some(&b'\\') => {
            let mut j = i + 1;
            while j < src.len() && src[j] != b'\'' {
                j += if src[j] == b'\\' { 2 } else { 1 };
            }
            Some((j + 1).min(src.len()))
        }
        b'/' if src.get(i + 1) == Some(&b'/') => {
            let mut j = i;
            while j < src.len() && src[j] != b'\n' {
                j += 1;
            }
            Some(j)
        }
        b'/' if src.get(i + 1) == Some(&b'*') => {
            let mut j = i + 2;
            while j + 1 < src.len() && !(src[j] == b'*' && src[j + 1] == b'/') {
                j += 1;
            }
            Some((j + 2).min(src.len()))
        }
        _ => None,
    }
}

/// The bracket closing the one at `open`.
fn matching(src: &[u8], open: usize) -> Option<usize> {
    let (o, c) = match src.get(open)? {
        b'(' => (b'(', b')'),
        b'[' => (b'[', b']'),
        b'{' => (b'{', b'}'),
        _ => return None,
    };
    let mut depth = 0usize;
    let mut i = open;
    while i < src.len() {
        if let Some(next) = skip_literal(src, i) {
            i = next;
            continue;
        }
        if src[i] == o {
            depth += 1;
        } else if src[i] == c {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

/// The first position in `from..to` outside brackets, strings and comments
/// where `hit` holds.
fn find_top(
    src: &[u8],
    from: usize,
    to: usize,
    hit: impl Fn(&[u8], usize) -> bool,
) -> Option<usize> {
    let mut i = from;
    while i < to.min(src.len()) {
        if let Some(next) = skip_literal(src, i) {
            i = next;
            continue;
        }
        if hit(src, i) {
            return Some(i);
        }
        if matches!(src[i], b'(' | b'[') {
            i = matching(src, i)? + 1;
            continue;
        }
        i += 1;
    }
    None
}

/// A `=` at `i` that is an assignment, not part of `==`, `!=`, `<=`, `>=`
/// or `=>`.
fn lone_eq(src: &[u8], i: usize) -> bool {
    src[i] == b'='
        && src.get(i + 1).is_none_or(|b| !matches!(b, b'=' | b'>'))
        && (i == 0 || !matches!(src[i - 1], b'=' | b'!' | b'<' | b'>'))
}

/// The end of the `else` chain after the block that closes at `close`, or
/// `None` when no `else` follows.
fn else_chain_end(src: &[u8], close: usize) -> Option<usize> {
    let mut end = None;
    let mut at = close + 1;
    loop {
        let e = trim_fwd(src, at);
        if !word_at(src, e, "else") {
            return end;
        }
        let open = find_top(src, e + 4, src.len(), |s, i| s[i] == b'{')?;
        let c = matching(src, open)?;
        end = Some(c + 1);
        at = c + 1;
    }
}

/// An irrefutable `if let P = e { .. } else { .. }` as `{ let P = e; .. }`,
/// `while let P = e { .. }` as `loop { let P = e; .. }`, and `let P = e
/// else { .. };` as `let P = e;`. `pat` is the pattern's span.
fn irrefutable_let(src: &[u8], pat: Span, form: &str) -> Option<Vec<TextEdit>> {
    let (let_at, w) = word_ending_at(src, trim_back(src, pat.offset))?;
    if w != "let" {
        return None;
    }
    let eq = find_top(src, pat.offset, src.len(), lone_eq)?;
    if form == "else" {
        let else_at = find_top(src, eq + 1, src.len(), |s, i| {
            word_at(s, i, "else") || s[i] == b';'
        })?;
        if !word_at(src, else_at, "else") {
            return None;
        }
        let open = find_top(src, else_at + 4, src.len(), |s, i| s[i] == b'{')?;
        let close = matching(src, open)?;
        let from = trim_back(src, else_at);
        return Some(vec![edit(from, close + 1 - from, "")]);
    }
    let (kw_at, kw) = word_ending_at(src, trim_back(src, let_at))?;
    if kw != form {
        return None;
    }
    let open = find_top(src, eq + 1, src.len(), |s, i| s[i] == b'{')?;
    let close = matching(src, open)?;
    let scrutinee = std::str::from_utf8(&src[trim_fwd(src, eq + 1)..trim_back(src, open)]).ok()?;
    let pattern = std::str::from_utf8(&src[pat.offset..trim_back(src, eq)]).ok()?;
    let head = if form == "if" { "{" } else { "loop {" };
    let mut edits = vec![edit(
        kw_at,
        open + 1 - kw_at,
        &format!("{head} let {pattern} = {scrutinee};"),
    )];
    if form == "if" {
        if let Some(end) = else_chain_end(src, close) {
            edits.push(edit(close + 1, end - (close + 1), ""));
        }
    }
    Some(edits)
}

/// A doc comment at `span` as a plain one: `///` as `//`, `/**` as `/*`.
fn plain_comment(src: &[u8], span: Span) -> Option<Vec<TextEdit>> {
    let at = span.offset;
    if src[at..].starts_with(b"///") {
        Some(vec![edit(at, 3, "//")])
    } else if src[at..].starts_with(b"/**") {
        Some(vec![edit(at, 3, "/*")])
    } else {
        None
    }
}

/// The bound `param: bound` in the generic list of the alias at `span`.
fn remove_alias_bound(src: &[u8], span: Span, param: &str, bound: &str) -> Option<Vec<TextEdit>> {
    let open = find_top(src, span.offset, src.len(), |s, i| {
        matches!(s[i], b'[' | b'=')
    })?;
    if src[open] != b'[' {
        return None;
    }
    let close = matching(src, open)?;
    let mut i = open + 1;
    while i < close {
        let p = trim_fwd(src, i);
        let end = find_top(src, p, close, |s, j| s[j] == b',').unwrap_or(close);
        if word_at(src, p, param) {
            let colon = trim_fwd(src, p + param.len());
            if src.get(colon) != Some(&b':') {
                return None;
            }
            let written = std::str::from_utf8(&src[colon + 1..end]).ok()?;
            let norm = |t: &str| t.split_whitespace().collect::<String>();
            if norm(written) != norm(bound) {
                return None;
            }
            let from = p + param.len();
            return Some(vec![edit(from, trim_back(src, end) - from, "")]);
        }
        i = end + 1;
    }
    None
}

/// `x as bool`, whose operand is at `span`, as `x != 0`, parenthesised
/// unless what surrounds it already delimits it.
fn int_as_bool(src: &[u8], span: Span) -> Option<Vec<TextEdit>> {
    let operand_end = span.offset + span.length;
    let as_at = trim_fwd(src, operand_end);
    if !word_at(src, as_at, "as") {
        return None;
    }
    let bool_at = trim_fwd(src, as_at + 2);
    if !word_at(src, bool_at, "bool") {
        return None;
    }
    let end = bool_at + 4;
    let before = trim_back(src, span.offset);
    let after = trim_fwd(src, end);
    let open_ok = before == 0
        || matches!(src[before - 1], b'(' | b',' | b'{' | b';')
        || (src[before - 1] == b'='
            && !matches!(
                src.get(before.wrapping_sub(2)),
                Some(b'=' | b'!' | b'<' | b'>')
            ))
        || word_ending_at(src, before)
            .is_some_and(|(_, w)| matches!(w, "if" | "while" | "return" | "and" | "or"));
    let close_ok = after >= src.len()
        || matches!(src[after], b')' | b',' | b';' | b'{' | b'}' | b']')
        || word_at(src, after, "and")
        || word_at(src, after, "or");
    let mut edits = vec![edit(operand_end, end - operand_end, " != 0")];
    if !(open_ok && close_ok) {
        edits.push(edit(span.offset, 0, "("));
        edits.push(edit(end, 0, ")"));
    }
    Some(edits)
}

/// `pub` before the item whose keyword is at `at`, with the blank after it.
fn remove_pub(src: &[u8], at: usize) -> Option<Vec<TextEdit>> {
    let (p, w) = word_ending_at(src, trim_back(src, at))?;
    (w == "pub").then(|| vec![edit(p, at - p, "")])
}

/// `, ..base` in the struct literal at `span`.
fn remove_spread(src: &[u8], span: Span) -> Option<Vec<TextEdit>> {
    let open = find_top(src, span.offset, src.len(), |s, i| s[i] == b'{')?;
    let close = matching(src, open)?;
    let dots = find_top(src, open + 1, close, |s, i| s.get(i..i + 2) == Some(b".."))?;
    let before = trim_back(src, dots);
    let from = if src.get(before.wrapping_sub(1)) == Some(&b',') {
        trim_back(src, before - 1)
    } else {
        before
    };
    let to = trim_back(src, close);
    (from > open).then(|| vec![edit(from, to - from, "")])
}

/// The end of the signature of the item whose keyword is at `at`: its
/// body's `{` or its `;`.
fn signature_end(src: &[u8], at: usize) -> Option<usize> {
    find_top(src, at, src.len(), |s, i| matches!(s[i], b'{' | b';'))
}

/// `from` in the signature at `at` as `to`.
fn replace_in_signature(src: &[u8], at: usize, from: &str, to: &str) -> Option<Vec<TextEdit>> {
    let end = signature_end(src, at)?;
    let text = std::str::from_utf8(&src[at..end]).ok()?;
    let i = at + text.find(from)?;
    word_at(src, i, from.split(' ').next()?).then(|| vec![edit(i, from.len(), to)])
}

/// Every `from` in the item from `at` to its first top-level `;`, newline
/// or `,`, as `to`.
fn replace_word_in_item(src: &[u8], at: usize, from: &str, to: &str) -> Option<Vec<TextEdit>> {
    let stop = find_top(src, at, src.len(), |s, i| {
        matches!(s[i], b';' | b'\n' | b',')
    })
    .unwrap_or(src.len());
    let edits: Vec<TextEdit> = (at..stop)
        .filter(|&i| word_at(src, i, from))
        .map(|i| edit(i, from.len(), to))
        .collect();
    (!edits.is_empty()).then_some(edits)
}

/// Where the line of the declaration `kind name` starts, and its indent.
fn item_line(src: &[u8], kind: &str, name: &str) -> Option<(usize, String)> {
    let text = std::str::from_utf8(src).ok()?;
    let mut at = None;
    let mut search = 0;
    while let Some(p) = text[search..].find(kind) {
        let i = search + p;
        search = i + kind.len();
        if !word_at(src, i, kind) {
            continue;
        }
        let n = trim_fwd(src, i + kind.len());
        if word_at(src, n, name) {
            at = Some(i);
            break;
        }
    }
    let at = at?;
    let line_start = text[..at].rfind('\n').map_or(0, |p| p + 1);
    let indent: String = text[line_start..]
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect();
    Some((line_start, indent))
}

/// The attribute lines right above the line starting at `line_start`, each
/// as (start, trimmed text), nearest first.
fn attribute_lines(text: &str, line_start: usize) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let mut l = line_start;
    while l > 0 {
        let prev = text[..l - 1].rfind('\n').map_or(0, |p| p + 1);
        let line = text[prev..l - 1].trim();
        if !line.starts_with("#[") {
            break;
        }
        out.push((prev, line));
        l = prev;
    }
    out
}

/// The line `#[derive(..)]` above the item `kind name`, given `traits`, or
/// those traits added to the item's own `derive`.
fn add_derive(src: &[u8], kind: &str, name: &str, traits: &[&str]) -> Option<Vec<TextEdit>> {
    let text = std::str::from_utf8(src).ok()?;
    let (line_start, indent) = item_line(src, kind, name)?;
    for (prev, line) in attribute_lines(text, line_start) {
        if let Some(d) = line.strip_prefix("#[derive(") {
            let present: Vec<&str> = d.trim_end_matches(")]").split(',').map(str::trim).collect();
            let missing: Vec<&str> = traits
                .iter()
                .copied()
                .filter(|t| !present.contains(t))
                .collect();
            if missing.is_empty() {
                return None;
            }
            let close = prev + text[prev..].find(")]")?;
            return Some(vec![edit(close, 0, &format!(", {}", missing.join(", ")))]);
        }
    }
    Some(vec![edit(
        line_start,
        0,
        &format!("{indent}#[derive({})]\n", traits.join(", ")),
    )])
}

/// `#[repr(C)]` above the union `name`, unless it already carries a `repr`.
fn add_repr_c(src: &[u8], name: &str) -> Option<Vec<TextEdit>> {
    let text = std::str::from_utf8(src).ok()?;
    let (line_start, indent) = item_line(src, "union", name)?;
    if attribute_lines(text, line_start)
        .iter()
        .any(|(_, l)| l.starts_with("#[repr"))
    {
        return None;
    }
    Some(vec![edit(line_start, 0, &format!("{indent}#[repr(C)]\n"))])
}

/// The `#[derive(..)]` line above the item `kind name`.
fn remove_derive_line(src: &[u8], kind: &str, name: &str) -> Option<Vec<TextEdit>> {
    let text = std::str::from_utf8(src).ok()?;
    let (line_start, _) = item_line(src, kind, name)?;
    let (prev, _) = attribute_lines(text, line_start)
        .into_iter()
        .find(|(_, l)| l.starts_with("#[derive(") && l.ends_with(")]"))?;
    let end = prev + text[prev..].find('\n')? + 1;
    Some(vec![edit(prev, end - prev, "")])
}

/// The `impl .. { .. }` block at `span`, deleted with the line it leaves
/// blank.
fn remove_impl_block(src: &[u8], span: Span) -> Option<Vec<TextEdit>> {
    let text = std::str::from_utf8(src).ok()?;
    let start = if word_at(src, span.offset, "impl") {
        span.offset
    } else {
        text[..span.offset].rfind("impl")?
    };
    if !word_at(src, start, "impl") {
        return None;
    }
    let open = find_top(src, start, src.len(), |s, i| s[i] == b'{')?;
    let close = matching(src, open)?;
    let line_start = text[..start].rfind('\n').map_or(0, |p| p + 1);
    let from = if text[line_start..start].trim().is_empty() {
        line_start
    } else {
        start
    };
    let mut to = close + 1;
    if src.get(to) == Some(&b'\n') {
        to += 1;
        // and the blank line that separated it from the next item
        if src.get(to) == Some(&b'\n') && from == line_start && from > 0 && src[from - 1] == b'\n' {
            to += 1;
        }
    }
    // Nothing but blank after it: keep that tail, which a file without a
    // final newline does not have.
    if src[close + 1..].iter().all(u8::is_ascii_whitespace) {
        to = close + 1;
    }
    Some(vec![edit(from, to - from, "")])
}

/// The `( .. )` after the attribute name at `span` (`#[cold(always)]`).
fn bare_attribute(src: &[u8], span: Span) -> Option<Vec<TextEdit>> {
    let name = if src[span.offset..].starts_with(b"#[") {
        span.offset + 2
    } else {
        span.offset
    };
    let open = find_top(src, name, src.len(), |s, i| matches!(s[i], b'(' | b']'))?;
    if src[open] != b'(' {
        return None;
    }
    let close = matching(src, open)?;
    (src.get(trim_fwd(src, close + 1)) == Some(&b']'))
        .then(|| vec![edit(open, close + 1 - open, "")])
}

/// The `?? fallback` of the `??` expression at `span`.
fn drop_fallback(src: &[u8], span: Span) -> Option<Vec<TextEdit>> {
    let end = (span.offset + span.length).min(src.len());
    let qq = find_top(src, span.offset, end, |s, i| s.get(i..i + 2) == Some(b"??"))?;
    let from = trim_back(src, qq);
    (from > span.offset).then(|| vec![edit(from, end - from, "")])
}

/// The keyword `from` at `at` as `to`.
fn replace_keyword(src: &[u8], at: usize, from: &str, to: &str) -> Option<Vec<TextEdit>> {
    word_at(src, at, from).then(|| vec![edit(at, from.len(), to)])
}

/// `x as R` whose operand is at `span` as `(x as Base) as R`.
fn cast_through_base(src: &[u8], span: Span, base: &str) -> Option<Vec<TextEdit>> {
    let end = span.offset + span.length;
    if !word_at(src, trim_fwd(src, end), "as") {
        return None;
    }
    Some(vec![
        edit(span.offset, 0, "("),
        edit(end, 0, &format!(" as {base})")),
    ])
}

/// The capture prefix (`mut ref`, `ref`) before the closure at `span`.
fn closure_prefix(src: &[u8], span: Span, prefix: &str) -> Option<Vec<TextEdit>> {
    let bar = if src.get(span.offset) == Some(&b'|') {
        span.offset
    } else {
        find_top(src, span.offset, src.len(), |s, i| s[i] == b'|')?
    };
    let words: Vec<&str> = prefix.split(' ').collect();
    let mut at = trim_back(src, bar);
    for w in words.iter().rev() {
        let (s, got) = word_ending_at(src, at)?;
        if got != *w {
            return None;
        }
        at = trim_back(src, s);
        if w == &words[0] {
            return Some(vec![edit(s, bar - s, "")]);
        }
    }
    None
}

/// `frozen ` in the signature of the method at `at`.
fn drop_frozen(src: &[u8], at: usize) -> Option<Vec<TextEdit>> {
    let end = signature_end(src, at)?;
    let i = (at..end).find(|&i| word_at(src, i, "frozen"))?;
    Some(vec![edit(i, trim_fwd(src, i + 6) - i, "")])
}

/// The quoted text after `after` up to `until` in `message`.
fn between<'m>(message: &'m str, after: &str, until: &str) -> Option<&'m str> {
    let s = message.find(after)? + after.len();
    let e = message[s..].find(until)? + s;
    Some(&message[s..e])
}

/// A fix for one typecheck diagnostic, from its message and the source.
fn typed_fix(src: &[u8], e: &TypeError) -> Option<Vec<TextEdit>> {
    let m = e.message.as_str();
    let lint = e.lint_name.as_deref();
    if lint == Some("irrefutable_let_pattern") {
        let form = if m.contains("irrefutable `if let`") {
            "if"
        } else if m.contains("irrefutable `while let`") {
            "while"
        } else {
            "else"
        };
        return irrefutable_let(src, e.span, form);
    }
    if lint == Some("unused_doc_comment") {
        return plain_comment(src, e.span);
    }
    if lint == Some("redundant_alias_bound") {
        let bound = between(m, "bound '", "' on alias")?;
        let (param, t) = bound.split_once(": ")?;
        return remove_alias_bound(src, e.span, param, t);
    }
    if lint == Some("unreachable_errdefer")
        || (m.contains("so the errdefer never runs") && m.contains("use `defer`"))
    {
        return replace_keyword(src, e.span.offset, "errdefer", "defer");
    }
    if m.contains("for the explicit non-zero check") {
        return int_as_bool(src, e.span);
    }
    if m.contains("`main()` is never `pub`") {
        return remove_pub(src, e.span.offset);
    }
    if m.contains("has a spread base (`..base`) with no effect") && m.contains("drop `..base`") {
        return remove_spread(src, e.span);
    }
    if m.contains("cannot take a `mut ref self` receiver: methods of a sync type") {
        return replace_in_signature(src, e.span.offset, "mut ref self", "self");
    }
    if m.contains("Replace `OnceCell` with `OnceLock`") {
        return replace_word_in_item(src, e.span.offset, "OnceCell", "OnceLock");
    }
    if m.contains("an unwrapped value needs no fallback — drop the '?? ...'") {
        return drop_fallback(src, e.span);
    }
    if let Some(name) = between(m, "union `", "` is missing the required `#[repr(C)]`") {
        return add_repr_c(src, name);
    }
    if let Some(name) = between(m, "` on union `", "` — derives are not supported on union") {
        return remove_derive_line(src, "union", name);
    }
    if m.contains("cannot implement `Drop` for union") {
        return remove_impl_block(src, e.span);
    }
    if let Some(base) = between(
        m,
        "Convert to the base first, then assert: `(x as ",
        ") as ",
    ) {
        return cast_through_base(src, e.span, base);
    }
    None
}

/// The typecheck diagnostics with a mechanical fix their help names and no
/// fix of their own get it: a single edit as `fix_it`, several in
/// `error_fix_diffs`.
pub(super) fn complete_typed(
    source: &str,
    errors: &mut [TypeError],
    diffs: &mut rustc_hash::FxHashMap<SpanKey, Vec<TextEdit>>,
) {
    let src = source.as_bytes();
    for e in errors.iter_mut() {
        if e.fix_it.is_some()
            || e.span.offset + e.span.length > src.len()
            || diffs.contains_key(&SpanKey::from_span(&e.span))
        {
            continue;
        }
        let Some(mut edits) = typed_fix(src, e) else {
            continue;
        };
        if edits.len() == 1 {
            let t = edits.remove(0);
            e.fix_it = Some(FixIt {
                span: Span {
                    offset: t.offset,
                    length: t.length,
                    ..e.span
                },
                replacement: t.replacement,
            });
        } else {
            diffs.insert(SpanKey::from_span(&e.span), edits);
        }
    }
}

/// The resolver's: `impl Copy for P` (the impl removed, the derive added),
/// `impl CrossTask for P` (removed), `#[default]` without the enum's
/// `derive(Default)`, an attribute given arguments it does not take.
pub(super) fn complete_resolved(source: &str, r: &mut crate::resolver::ResolveResult) {
    let src = source.as_bytes();
    let mut fixes = Vec::new();
    for e in &r.errors {
        if e.replacement.is_some()
            || e.span.offset + e.span.length > src.len()
            || r.error_fix_diffs.contains_key(&SpanKey::from_span(&e.span))
        {
            continue;
        }
        let m = e.message.as_str();
        let edits = if let Some(target) = between(m, "a manual `impl Copy for ", "` is not allowed")
        {
            remove_impl_block(src, e.span).and_then(|mut del| {
                let target = target.split('[').next().unwrap_or(target);
                let add = add_derive(src, "struct", target, &["Copy", "Clone"])
                    .or_else(|| add_derive(src, "enum", target, &["Copy", "Clone"]))?;
                del.extend(add);
                Some(del)
            })
        } else if m.contains("` is not allowed: `CrossTask` is computed by the compiler") {
            remove_impl_block(src, e.span)
        } else if let Some(name) = between(
            m,
            "requires the enclosing enum `",
            "` to carry `#[derive(Default)]`",
        ) {
            add_derive(src, "enum", name, &["Default"])
        } else if m.contains("accepts no arguments — write it as a bare") {
            bare_attribute(src, e.span)
        } else {
            None
        };
        if let Some(edits) = edits {
            fixes.push((e.span, edits));
        }
    }
    for (span, mut edits) in fixes {
        if edits.len() == 1 {
            let t = edits.remove(0);
            if let Some(e) = r
                .errors
                .iter_mut()
                .find(|e| e.span == span && e.replacement.is_none())
            {
                e.replacement = Some(Box::new(t));
            }
        } else {
            r.error_fix_diffs.insert(SpanKey::from_span(&span), edits);
        }
    }
}

/// The ownership checker's: a closure's capture prefix that its body
/// contradicts, and `frozen` on a `sync struct`, which adds nothing.
pub(super) fn complete_ownership(source: &str, o: &mut crate::ownership::OwnershipCheckResult) {
    let src = source.as_bytes();
    for e in o.errors.iter_mut() {
        if e.replacement.is_some() || e.span.offset + e.span.length > src.len() {
            continue;
        }
        let m = e.message.as_str();
        let fix = if m.contains("declared `mut ref` but consumed in closure body")
            && m.contains("drop the `mut ref` prefix")
        {
            closure_prefix(src, e.span, "mut ref")
        } else if m.contains("declared `ref` but consumed in closure body")
            && m.contains("drop the `ref` prefix")
        {
            closure_prefix(src, e.span, "ref")
        } else if m.contains("is a `sync struct`, so `frozen` adds nothing") {
            drop_frozen(src, e.span.offset)
        } else {
            None
        };
        if let Some(mut edits) = fix {
            if edits.len() == 1 {
                e.replacement = Some(Box::new(edits.remove(0)));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span_of(src: &str, needle: &str) -> Span {
        Span {
            offset: src.find(needle).unwrap(),
            length: needle.len(),
            line: 0,
            column: 0,
        }
    }

    fn apply(src: &str, mut edits: Vec<TextEdit>) -> String {
        edits.sort_by_key(|e| std::cmp::Reverse(e.offset));
        let mut out = src.to_string();
        for e in edits {
            out.replace_range(e.offset..e.offset + e.length, &e.replacement);
        }
        out
    }

    #[test]
    fn an_irrefutable_if_let_becomes_a_block_without_its_else() {
        let src = "if let (a, b) = t {\n    f(a);\n} else {\n    g();\n}";
        let s = span_of(src, "(a, b)");
        assert_eq!(
            apply(src, irrefutable_let(src.as_bytes(), s, "if").unwrap()),
            "{ let (a, b) = t;\n    f(a);\n}"
        );
    }

    #[test]
    fn an_irrefutable_while_let_becomes_a_loop() {
        let src = "while let (a, b) = (n, 1) {\n    n = a + b;\n}";
        let s = span_of(src, "(a, b)");
        assert_eq!(
            apply(src, irrefutable_let(src.as_bytes(), s, "while").unwrap()),
            "loop { let (a, b) = (n, 1);\n    n = a + b;\n}"
        );
    }

    #[test]
    fn an_irrefutable_let_else_loses_its_else() {
        let src = "let (a, b) = (1, 2) else {\n    return;\n};";
        let s = span_of(src, "(a, b)");
        assert_eq!(
            apply(src, irrefutable_let(src.as_bytes(), s, "else").unwrap()),
            "let (a, b) = (1, 2);"
        );
    }

    #[test]
    fn an_int_as_bool_is_parenthesised_only_where_it_must_be() {
        let src = "if n as bool { }";
        let s = Span {
            length: 1,
            ..span_of(src, "n ")
        };
        assert_eq!(
            apply(src, int_as_bool(src.as_bytes(), s).unwrap()),
            "if n != 0 { }"
        );
        let src = "let b = z as bool == false;";
        let s = Span {
            length: 1,
            ..span_of(src, "z ")
        };
        assert_eq!(
            apply(src, int_as_bool(src.as_bytes(), s).unwrap()),
            "let b = (z != 0) == false;"
        );
    }

    #[test]
    fn a_spread_with_nothing_to_copy_is_dropped() {
        let src = "let p = P { x: 2, ..b };";
        let s = span_of(src, "P {");
        assert_eq!(
            apply(src, remove_spread(src.as_bytes(), s).unwrap()),
            "let p = P { x: 2 };"
        );
    }

    #[test]
    fn a_derive_merges_into_the_items_own() {
        let src = "#[derive(Debug)]\nstruct P { x: i64 }\n";
        assert_eq!(
            apply(
                src,
                add_derive(src.as_bytes(), "struct", "P", &["Copy", "Clone"]).unwrap()
            ),
            "#[derive(Debug, Copy, Clone)]\nstruct P { x: i64 }\n"
        );
        let src = "#[derive(Debug)]\nenum Mode { A }\n\nstruct P { x: i64 }\n";
        assert_eq!(
            apply(
                src,
                add_derive(src.as_bytes(), "struct", "P", &["Copy", "Clone"]).unwrap()
            ),
            "#[derive(Debug)]\nenum Mode { A }\n\n#[derive(Copy, Clone)]\nstruct P { x: i64 }\n"
        );
    }

    #[test]
    fn an_alias_bound_is_removed_and_only_the_one_named() {
        let src = "type Ranked[T: Ord, U] = SortedMap[T, U];";
        let s = span_of(src, "type");
        assert_eq!(
            apply(
                src,
                remove_alias_bound(src.as_bytes(), s, "T", "Ord").unwrap()
            ),
            "type Ranked[T, U] = SortedMap[T, U];"
        );
        assert!(remove_alias_bound(src.as_bytes(), s, "T", "Hash").is_none());
    }

    #[test]
    fn a_closure_prefix_and_a_frozen_receiver_are_dropped() {
        let src = "let f = mut ref || { let _ = o; };";
        let s = span_of(src, "||");
        assert_eq!(
            apply(src, closure_prefix(src.as_bytes(), s, "mut ref").unwrap()),
            "let f = || { let _ = o; };"
        );
        let src = "fn m(frozen self) -> i64 { self.v }";
        assert_eq!(
            apply(src, drop_frozen(src.as_bytes(), 0).unwrap()),
            "fn m(self) -> i64 { self.v }"
        );
    }

    #[test]
    fn an_impl_block_that_ends_the_file_keeps_the_tail() {
        let src = "union U { a: i32 }\nimpl Drop for U { fn drop(mut ref self) {} }\n";
        let s = span_of(src, "impl");
        assert_eq!(
            apply(src, remove_impl_block(src.as_bytes(), s).unwrap()),
            "union U { a: i32 }\n\n"
        );
    }
}
