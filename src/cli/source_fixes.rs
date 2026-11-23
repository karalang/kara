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

/// The label `'old` of the `break` at `span` as `'new`.
fn break_label(src: &[u8], span: Span, old: &str, new: &str) -> Option<Vec<TextEdit>> {
    if !word_at(src, span.offset, "break") {
        return None;
    }
    let tick = trim_fwd(src, span.offset + 5);
    (src.get(tick) == Some(&b'\'') && word_at(src, tick + 1, old))
        .then(|| vec![edit(tick + 1, old.len(), new)])
}

/// Whether `w` is a whole word anywhere in `src[from..to]`.
fn has_word(src: &[u8], from: usize, to: usize, w: &str) -> bool {
    (from..to.min(src.len())).any(|i| word_at(src, i, w))
}

/// The `[..]` generic list of `fn drop` in the impl at `span`, when the
/// body names none of its parameters.
fn drop_generics(src: &[u8], span: Span) -> Option<Vec<TextEdit>> {
    let end = (span.offset + span.length).min(src.len());
    let name = (span.offset..end).find(|&i| {
        word_at(src, i, "drop") && word_ending_at(src, trim_back(src, i)).map(|w| w.1) == Some("fn")
    })?;
    let open = trim_fwd(src, name + 4);
    if src.get(open) != Some(&b'[') {
        return None;
    }
    let close = matching(src, open)?;
    let list = std::str::from_utf8(&src[open + 1..close]).ok()?;
    for param in list.split(',') {
        let param = param.trim();
        let n = param
            .bytes()
            .position(|b| !is_ident(b))
            .unwrap_or(param.len());
        if n == 0 || has_word(src, close + 1, end, &param[..n]) {
            return None;
        }
    }
    Some(vec![edit(open, close + 1 - open, "")])
}

/// Where an argument-position `impl Bound` at `at` sits in its method's
/// signature: the end of the bound, and the place the method's generic
/// parameter goes with the text that opens and closes it there.
struct ImplArg {
    bound_end: usize,
    insert_at: usize,
    open: &'static str,
    close: &'static str,
}

fn impl_arg(src: &[u8], at: usize) -> Option<ImplArg> {
    if !word_at(src, at, "impl") {
        return None;
    }
    let bound = trim_fwd(src, at + 4);
    let bound_end = trim_back(
        src,
        find_top(src, bound, src.len(), |s, i| {
            matches!(s[i], b',' | b')' | b']')
        })?,
    );
    if bound_end <= bound || has_word(src, bound, bound_end, "with") {
        return None;
    }
    // The parameter list's `(`: the first one back from `at` that nothing
    // closes. A string, a comment or a body in between is not a signature.
    let mut depth = 0usize;
    let mut i = at;
    let paren = loop {
        i = i.checked_sub(1)?;
        match src[i] {
            b')' | b']' => depth += 1,
            b'(' if depth == 0 => break i,
            // An unclosed `[` is the generic argument list the `impl`
            // sits in (`Vec[impl T]`).
            b'[' if depth == 0 => {}
            b'(' | b'[' => depth -= 1,
            b'"' | b'{' | b'}' | b';' | b'/' => return None,
            _ => {}
        }
    };
    let before = trim_back(src, paren);
    let is_fn = |name_end: usize| {
        word_ending_at(src, name_end).is_some_and(|(s, w)| {
            !w.is_empty() && word_ending_at(src, trim_back(src, s)).map(|w| w.1) == Some("fn")
        })
    };
    if before > 0 && src[before - 1] == b']' {
        // `fn m[A](..)`: the parameter joins the list.
        let close = before - 1;
        let mut depth = 0usize;
        let mut j = close;
        let open = loop {
            j = j.checked_sub(1)?;
            match src[j] {
                b']' | b')' => depth += 1,
                b'[' if depth == 0 => break j,
                b'[' | b'(' => depth = depth.checked_sub(1)?,
                b'"' | b'{' | b'}' | b';' | b'/' => return None,
                _ => {}
            }
        };
        if !is_fn(trim_back(src, open)) {
            return None;
        }
        let last = trim_back(src, close);
        let (open, insert_at) = match src[last - 1] {
            b'[' => ("", last),
            b',' => (" ", last),
            _ => (", ", last),
        };
        Some(ImplArg {
            bound_end,
            insert_at,
            open,
            close: "",
        })
    } else {
        is_fn(before).then_some(ImplArg {
            bound_end,
            insert_at: before,
            open: "[",
            close: "]",
        })
    }
}

/// The parser's: an argument-position `impl Trait` in a trait method
/// (`fn log(self, msg: impl Display)`), or one nested in a parameter's
/// type (`xs: Vec[impl Display]`), as the explicit generic parameter its
/// message names (`fn log[T: Display](self, msg: T)`). A function with
/// several has them all in the first one's fix, since they share the one
/// generic list (B-2026-11-21-74).
pub(super) fn complete_parsed(source: &str, p: &mut crate::parser::ParseResult) {
    complete_parse_errors(source, &p.errors, &p.fix_edits, &mut p.fix_diffs);
}

/// [`complete_parsed`] over a parse's parts, as a package's module carries
/// them (`ModuleParseErrors`).
pub(super) fn complete_parse_errors(
    source: &str,
    errors: &[crate::parser::ParseError],
    fix_edits: &rustc_hash::FxHashMap<SpanKey, TextEdit>,
    fix_diffs: &mut rustc_hash::FxHashMap<SpanKey, Vec<TextEdit>>,
) {
    let src = source.as_bytes();
    // Per method, keyed by where its generic parameters go: the diagnostic
    // that owns the fix, the insertion's two ends, and each `impl Bound`
    // with the name it takes.
    struct Method {
        owner: SpanKey,
        site: ImplArg,
        args: Vec<(usize, String, String)>,
    }
    let mut methods: Vec<Method> = Vec::new();
    let mut taken: Vec<String> = Vec::new();
    for e in errors {
        if !e
            .message
            .starts_with("error[E_IMPL_TRAIT_IN_TRAIT_METHOD_ARG]")
            && !e
                .message
                .starts_with("error[E_IMPL_TRAIT_IN_NESTED_POSITION]")
        {
            continue;
        }
        let key = SpanKey::from_span(&e.span);
        if fix_edits.contains_key(&key) || fix_diffs.contains_key(&key) {
            continue;
        }
        let Some(site) = impl_arg(src, e.span.offset) else {
            continue;
        };
        // A name the file does not use anywhere, so it shadows nothing.
        let Some(name) = ["T", "U", "V", "W", "X", "Y", "Z"]
            .into_iter()
            .find(|n| !taken.iter().any(|t| t == n) && !has_word(src, 0, src.len(), n))
        else {
            continue;
        };
        taken.push(name.to_string());
        let Ok(bound) = std::str::from_utf8(&src[trim_fwd(src, e.span.offset + 4)..site.bound_end])
        else {
            continue;
        };
        let arg = (e.span.offset, name.to_string(), bound.to_string());
        match methods
            .iter_mut()
            .find(|m| m.site.insert_at == site.insert_at)
        {
            Some(m) => m.args.push(arg),
            None => methods.push(Method {
                owner: key,
                site,
                args: vec![arg],
            }),
        }
    }
    for m in methods {
        let params: Vec<String> = m.args.iter().map(|(_, n, b)| format!("{n}: {b}")).collect();
        let mut edits = vec![edit(
            m.site.insert_at,
            0,
            &format!("{}{}{}", m.site.open, params.join(", "), m.site.close),
        )];
        for (at, name, bound) in &m.args {
            let end = trim_fwd(src, at + 4) + bound.len();
            edits.push(edit(*at, end - at, name));
        }
        fix_diffs.insert(m.owner, edits);
    }
}

/// The attribute argument that opens with `key` at `at`, value and all
/// (`skip: true`), as the key alone.
fn bare_key(src: &[u8], at: usize, key: &str) -> Option<Vec<TextEdit>> {
    if !word_at(src, at, key) {
        return None;
    }
    let end = trim_back(
        src,
        find_top(src, at, src.len(), |s, i| matches!(s[i], b',' | b')'))?,
    );
    (end > at + key.len()).then(|| vec![edit(at, end - at, key)])
}

/// `size_of[T]()` / `align_of[T]()` at `at` as `to`, the method form its
/// message spells.
fn type_arg_call(src: &[u8], at: usize, to: &str) -> Option<Vec<TextEdit>> {
    let name = ["size_of", "align_of"]
        .into_iter()
        .find(|n| word_at(src, at, n))?;
    let open = at + name.len();
    if src.get(open) != Some(&b'[') {
        return None;
    }
    let args = matching(src, open)? + 1;
    let close = trim_fwd(src, args + 1);
    (src.get(args) == Some(&b'(') && src.get(close) == Some(&b')'))
        .then(|| vec![edit(at, close + 1 - at, to)])
}

/// Whether the expression text `e` needs no parentheses as an operand of
/// `==`: names, paths, calls, indexing, literals.
fn is_postfix_text(src: &[u8], from: usize, to: usize) -> bool {
    let mut i = from;
    while i < to {
        if let Some(next) = skip_literal(src, i) {
            i = next;
        } else if matches!(src[i], b'(' | b'[') {
            match matching(src, i) {
                Some(c) if c < to => i = c + 1,
                _ => return false,
            }
        } else if is_ident(src[i]) || src[i] == b'.' {
            i += 1;
        } else {
            return false;
        }
    }
    to > from
}

/// The `ref_eq(a, b)` call around the argument at `arg` as `a == b`.
fn ref_eq_as_eq(src: &[u8], arg: Span) -> Option<Vec<TextEdit>> {
    // The call's `(`: the first one back from the argument nothing closes.
    let mut depth = 0usize;
    let mut i = arg.offset;
    let open = loop {
        i = i.checked_sub(1)?;
        match src[i] {
            b')' | b']' => depth += 1,
            b'(' if depth == 0 => break i,
            b'(' | b'[' => depth = depth.checked_sub(1)?,
            b'"' | b'{' | b'}' | b';' => return None,
            _ => {}
        }
    };
    let (name, word) = word_ending_at(src, open)?;
    if word != "ref_eq" || (name > 0 && src[name - 1] == b'.') {
        return None;
    }
    let close = matching(src, open)?;
    let comma = find_top(src, open + 1, close, |s, i| s[i] == b',')?;
    if find_top(src, comma + 1, close, |s, i| s[i] == b',').is_some() {
        return None;
    }
    let side = |from: usize, to: usize| {
        let (from, to) = (trim_fwd(src, from), trim_back(src, to));
        let text = std::str::from_utf8(src.get(from..to)?).ok()?;
        Some(if is_postfix_text(src, from, to) {
            text.to_string()
        } else {
            format!("({text})")
        })
    };
    let eq = format!("{} == {}", side(open + 1, comma)?, side(comma + 1, close)?);
    // Alone in a condition, an argument, an initializer or a statement the
    // comparison stands as written; under an operator it keeps parentheses.
    let before = trim_back(src, name);
    let after = trim_fwd(src, close + 1);
    let opens = before == 0
        || matches!(src[before - 1], b'(' | b'{' | b',' | b';' | b'[')
        || (src[before - 1] == b'='
            && before >= 2
            && !matches!(src[before - 2], b'=' | b'!' | b'<' | b'>'))
        || word_ending_at(src, before)
            .is_some_and(|(_, w)| matches!(w, "if" | "while" | "return" | "in"));
    let closes = src
        .get(after)
        .is_none_or(|b| matches!(b, b')' | b'{' | b'}' | b',' | b';' | b']'));
    let text = if opens && closes {
        eq
    } else {
        format!("({eq})")
    };
    Some(vec![edit(name, close + 1 - name, &text)])
}

/// The item that starts at `at`, through its `;` or its closing `}`, with
/// the rest of its line.
fn item_extent(src: &[u8], at: usize) -> Option<(usize, usize)> {
    let stop = find_top(src, at, src.len(), |s, i| matches!(s[i], b';' | b'{'))?;
    let mut end = if src[stop] == b'{' {
        matching(src, stop)? + 1
    } else {
        stop + 1
    };
    while src.get(end).is_some_and(|b| matches!(b, b' ' | b'\t')) {
        end += 1;
    }
    if src.get(end) == Some(&b'\n') {
        end += 1;
    }
    let mut start = at;
    while start > 0 && matches!(src[start - 1], b' ' | b'\t') {
        start -= 1;
    }
    (start == 0 || src[start - 1] == b'\n').then_some((start, end))
}

/// Every whole-word `name` in `src` outside `skip` and outside strings and
/// comments.
fn uses_of(src: &[u8], name: &str, skip: (usize, usize)) -> Vec<usize> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < src.len() {
        if let Some(next) = skip_literal(src, i) {
            i = next;
            continue;
        }
        if (i < skip.0 || i >= skip.1) && word_at(src, i, name) {
            out.push(i);
            i += name.len();
        } else {
            i += 1;
        }
    }
    out
}

/// The trait alias declared at `span` (`trait Numeric = Copy + Clone;`)
/// deleted, and `bounds` written at each use of its name. Every use must be
/// a bound (after `:`, `+`, `impl` or `dyn`, and not the trait of an impl
/// block), or nothing is offered.
fn inline_trait_alias(src: &[u8], span: Span, name: &str, bounds: &str) -> Option<Vec<TextEdit>> {
    let (start, end) = item_extent(src, span.offset)?;
    let decl = std::str::from_utf8(&src[start..end]).ok()?;
    // `trait Name = ..;` with no parameters of its own.
    let head = decl.split('=').next()?;
    if !decl.trim_end().ends_with(';') || head.contains('[') || !decl.contains('=') {
        return None;
    }
    let mut edits = vec![edit(start, end - start, "")];
    for at in uses_of(src, name, (start, end)) {
        let before = trim_back(src, at);
        let bound = before > 0 && matches!(src[before - 1], b':' | b'+')
            || word_ending_at(src, before).is_some_and(|(_, w)| matches!(w, "impl" | "dyn"));
        let after = trim_fwd(src, at + name.len());
        // `impl Numeric for W` implements it; a bound list cannot stand
        // there.
        if !bound || src.get(after) == Some(&b'[') || word_at(src, after, "for") {
            return None;
        }
        edits.push(edit(at, name.len(), bounds));
    }
    Some(edits)
}

/// The `effect group` declared at `span` deleted, and its effects written
/// out at each use of its name in a `with` clause. A use anywhere else, or
/// a group whose own list names another group's `+`-joined form the clause
/// cannot take, withholds the fix.
fn inline_effect_group(src: &[u8], span: Span, name: &str) -> Option<Vec<TextEdit>> {
    let (start, end) = item_extent(src, span.offset)?;
    let decl = std::str::from_utf8(&src[start..end]).ok()?;
    let (_, list) = decl.trim_end().strip_suffix(';')?.split_once('=')?;
    // `a + b(..)` in the declaration is `a b(..)` in a clause.
    let mut effects = Vec::new();
    let bytes = list.as_bytes();
    let mut from = 0;
    while from < bytes.len() {
        let to = find_top(bytes, from, bytes.len(), |s, i| s[i] == b'+').unwrap_or(bytes.len());
        let one = list[from..to].trim();
        if one.is_empty() {
            return None;
        }
        effects.push(one);
        from = to + 1;
    }
    let effects = effects.join(" ");
    let mut edits = vec![edit(start, end - start, "")];
    for at in uses_of(src, name, (start, end)) {
        // Back over the clause's other effects to its `with`.
        let mut i = at;
        let in_clause = loop {
            i = trim_back(src, i);
            if i > 0 && src[i - 1] == b')' {
                let mut depth = 0usize;
                loop {
                    i = i.checked_sub(1)?;
                    match src[i] {
                        b')' => depth += 1,
                        b'(' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                continue;
            }
            match word_ending_at(src, i) {
                Some((_, "with")) => break true,
                Some((s, w)) if !w.is_empty() => i = s,
                _ => break false,
            }
        };
        if !in_clause {
            return None;
        }
        edits.push(edit(at, name.len(), &effects));
    }
    Some(edits)
}

/// The method at `span`, which its trait does not declare, moved out of
/// the `impl Trait for Type` block into an `impl Type` block after it.
fn move_to_inherent_impl(src: &[u8], span: Span) -> Option<Vec<TextEdit>> {
    let (start, end) = item_extent(src, span.offset)?;
    if src[end - 1] != b'\n' || !src[..end].trim_ascii_end().ends_with(b"}") {
        return None;
    }
    // The enclosing block's `{`: the first one back that nothing closes.
    let mut depth = 0usize;
    let mut i = start;
    let open = loop {
        i = i.checked_sub(1)?;
        match src[i] {
            b'}' => depth += 1,
            b'{' if depth == 0 => break i,
            b'{' => depth -= 1,
            _ => {}
        }
    };
    let close = matching(src, open)?;
    // Its header, `impl Trait for Type {`, alone on its line and with no
    // parameters or `where` of its own to carry over.
    let line = src[..open]
        .iter()
        .rposition(|b| *b == b'\n')
        .map_or(0, |n| n + 1);
    let header = std::str::from_utf8(&src[line..open]).ok()?;
    let rest = header.strip_prefix("impl ")?;
    let (_, target) = rest.split_once(" for ")?;
    let target = target.trim();
    if target.is_empty()
        || header.contains(" where ")
        || !target.bytes().all(|b| is_ident(b) || b == b'.')
        || src.get(close + 1) != Some(&b'\n')
    {
        return None;
    }
    let method = std::str::from_utf8(&src[start..end]).ok()?;
    Some(vec![
        edit(start, end - start, ""),
        edit(close + 2, 0, &format!("\nimpl {target} {{\n{method}}}\n")),
    ])
}

/// Where the line holding `at` starts.
fn line_start(src: &[u8], at: usize) -> usize {
    src[..at]
        .iter()
        .rposition(|b| *b == b'\n')
        .map_or(0, |n| n + 1)
}

/// The codegen hint at `at`, written on an `impl` block, moved onto each
/// of the block's methods.
fn hint_onto_methods(src: &[u8], at: usize) -> Option<Vec<TextEdit>> {
    if src.get(at..at + 2) != Some(b"#[") || line_start(src, at) != at {
        return None;
    }
    let attr_end = matching(src, at + 1)? + 1;
    let attr = std::str::from_utf8(&src[at..attr_end]).ok()?;
    if src.get(attr_end) != Some(&b'\n') || !word_at(src, attr_end + 1, "impl") {
        return None;
    }
    let open = find_top(src, attr_end, src.len(), |s, i| matches!(s[i], b'{' | b';'))?;
    if src[open] != b'{' {
        return None;
    }
    let close = matching(src, open)?;
    let mut edits = vec![edit(at, attr_end + 1 - at, "")];
    let mut i = open + 1;
    while i < close {
        if let Some(next) = skip_literal(src, i) {
            i = next;
        } else if src[i] == b'{' {
            i = matching(src, i)? + 1;
        } else if word_at(src, i, "fn") {
            // The hint goes on a line of its own above the method's, at
            // the method's indentation.
            let line = line_start(src, i);
            let indent = src[line..i]
                .iter()
                .take_while(|b| matches!(b, b' ' | b'\t'))
                .count();
            let indent = std::str::from_utf8(&src[line..line + indent]).ok()?;
            edits.push(edit(line, 0, &format!("{indent}{attr}\n")));
            i = signature_end(src, i)?;
        } else {
            i += 1;
        }
    }
    (edits.len() > 1).then_some(edits)
}

/// `let s = <temporary><slice>;` at `value` (the initializer) with the
/// temporary bound to a local of its own on the line before, and the slice
/// taken from that.
fn hoist_sliced_temporary(
    src: &[u8],
    value: usize,
    taken: &mut Vec<&'static str>,
) -> Option<Vec<TextEdit>> {
    let semi = find_top(src, value, src.len(), |s, i| matches!(s[i], b';' | b'{'))?;
    if src[semi] != b';' {
        return None;
    }
    let end = trim_back(src, semi);
    let text = std::str::from_utf8(&src[value..end]).ok()?;
    // The slice is the initializer's last postfix: `.as_slice()`,
    // `.as_slice_mut()`, or a `[a..b]` index.
    let (source_end, keyword) = if let Some(rest) = text.strip_suffix(".as_slice()") {
        (value + rest.len(), "let")
    } else if let Some(rest) = text.strip_suffix(".as_slice_mut()") {
        (value + rest.len(), "let mut")
    } else if text.ends_with(']') {
        let mut depth = 0usize;
        let mut i = end;
        loop {
            i = i.checked_sub(1).filter(|i| *i > value)?;
            match src[i] {
                b']' | b')' => depth += 1,
                b'[' | b'(' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                b'"' => return None,
                _ => {}
            }
        }
        (trim_back(src, i), "let")
    } else {
        return None;
    };
    // The statement is a `let` that opens its line.
    let line = line_start(src, value);
    let head = std::str::from_utf8(&src[line..value]).ok()?;
    let indent = &head[..head.len() - head.trim_start().len()];
    if source_end <= value
        || !head.trim_start().starts_with("let ")
        || !head.trim_end().ends_with('=')
    {
        return None;
    }
    // A name the file does not use, nor an earlier fix of this pass.
    let name = ["backing", "backing_2", "backing_3", "backing_4"]
        .into_iter()
        .find(|n| !taken.contains(n) && !has_word(src, 0, src.len(), n))?;
    taken.push(name);
    let source = std::str::from_utf8(&src[value..source_end]).ok()?;
    Some(vec![
        edit(line, 0, &format!("{indent}{keyword} {name} = {source};\n")),
        edit(value, source_end - value, name),
    ])
}

/// The call of a removed encoding namespace at `at` (`Base64.encode(b)`)
/// as the `std.encoding` function that replaces it, with its import. Each
/// call's fix carries the import it needs, so any one of them leaves a
/// program that resolves; two that write the same import are applied a
/// pass apart, and the second finds the name there.
fn removed_encoding_call(src: &[u8], at: usize, ty: &str, method: &str) -> Option<Vec<TextEdit>> {
    // The function, what its argument gains, and what the call gains.
    let (name, arg_suffix, call_suffix) = match (ty, method) {
        ("Base64", "encode") => ("base64_encode", "", ""),
        ("Base64", "encode_url_safe") => ("base64url_encode", "", ""),
        ("Base64", "decode") => ("base64_decode", "", ""),
        ("Hex", "encode") => ("hex_encode", "", ""),
        ("Hex", "encode_upper") => ("hex_encode", "", ".to_uppercase()"),
        ("Hex", "decode") => ("hex_decode", "", ""),
        ("Url", "encode") => ("percent_encode", ".bytes()", ""),
        _ => return None,
    };
    if !word_at(src, at, ty) || src.get(at + ty.len()) != Some(&b'.') {
        return None;
    }
    let m = at + ty.len() + 1;
    let open = m + method.len();
    if !word_at(src, m, method) || src.get(open) != Some(&b'(') {
        return None;
    }
    let close = matching(src, open)?;
    let (arg, arg_end) = (trim_fwd(src, open + 1), trim_back(src, close));
    if arg >= arg_end || find_top(src, arg, arg_end, |s, i| s[i] == b',').is_some() {
        return None;
    }
    let mut edits = vec![edit(at, open - at, name)];
    if !arg_suffix.is_empty() {
        if is_postfix_text(src, arg, arg_end) {
            edits.push(edit(arg_end, 0, arg_suffix));
        } else {
            edits.push(edit(arg, 0, "("));
            edits.push(edit(arg_end, 0, &format!("){arg_suffix}")));
        }
    }
    if !call_suffix.is_empty() {
        edits.push(edit(close + 1, 0, call_suffix));
    }
    // The import: the name joins `import std.encoding.{..};` where the
    // file has one, else a line of its own after the last import.
    const HEAD: &[u8] = b"import std.encoding.{";
    let mut last_import_end = 0;
    let mut i = 0;
    while i < src.len() {
        if let Some(next) = skip_literal(src, i) {
            i = next;
            continue;
        }
        if line_start(src, i) == i && src[i..].starts_with(b"import ") {
            let semi = find_top(src, i, src.len(), |s, i| s[i] == b';')?;
            if src[i..].starts_with(HEAD) {
                let brace = i + HEAD.len() - 1;
                let end = (brace..semi).find(|&j| src[j] == b'}')?;
                if !has_word(src, brace, end, name) {
                    let last = trim_back(src, end);
                    let sep = if src[last - 1] == b'{' { "" } else { ", " };
                    edits.push(edit(last, 0, &format!("{sep}{name}")));
                }
                return Some(edits);
            }
            last_import_end = semi + 1;
            i = semi + 1;
            continue;
        }
        i += 1;
    }
    let line = format!("import std.encoding.{{{name}}};\n");
    if last_import_end > 0 {
        if src.get(last_import_end) != Some(&b'\n') {
            return None;
        }
        edits.push(edit(last_import_end + 1, 0, &line));
    } else {
        edits.push(edit(0, 0, &format!("{line}\n")));
    }
    Some(edits)
}

/// The `(` that opens the list `at` sits in, and how many top-level
/// commas lie between the two: the first `(` back from `at` that nothing
/// closes, with no body, statement end or string on the way.
fn enclosing_paren(src: &[u8], at: usize) -> Option<(usize, usize)> {
    let mut depth = 0usize;
    let mut commas = 0;
    let mut i = at;
    loop {
        i = i.checked_sub(1)?;
        match src[i] {
            b')' | b']' => depth += 1,
            b'(' if depth == 0 => return Some((i, commas)),
            b'(' | b'[' => depth = depth.checked_sub(1)?,
            b',' if depth == 0 => commas += 1,
            b'"' | b'{' | b'}' | b';' => return None,
            _ => {}
        }
    }
}

/// The `index`th top-level argument of the list that opens at `open`.
fn nth_argument(src: &[u8], open: usize, index: usize) -> Option<(usize, usize)> {
    let close = matching(src, open)?;
    let mut from = open + 1;
    for _ in 0..index {
        from = find_top(src, from, close, |s, i| s[i] == b',')? + 1;
    }
    let to = find_top(src, from, close, |s, i| s[i] == b',').unwrap_or(close);
    let (from, to) = (trim_fwd(src, from), trim_back(src, to));
    (to > from).then_some((from, to))
}

/// A foreign import's parameter whose type at `ty` does not cross to C
/// (`ref CStr`, `String`) as `*const u8`, with `.as_ptr()` on that argument
/// of every call of `name`. A use of `name` that is not a call withholds it.
fn extern_param_as_ptr(src: &[u8], ty: usize, name: &str) -> Option<Vec<TextEdit>> {
    let (open, index) = enclosing_paren(src, ty)?;
    let (decl, word) = word_ending_at(src, trim_back(src, open))?;
    let ty_end = trim_back(
        src,
        find_top(src, ty, src.len(), |s, i| matches!(s[i], b',' | b')'))?,
    );
    if word != name || ty_end <= ty {
        return None;
    }
    let mut edits = vec![edit(ty, ty_end - ty, "*const u8")];
    for at in uses_of(src, name, (decl, decl + name.len())) {
        let call = at + name.len();
        if src.get(call) != Some(&b'(') || (at > 0 && src[at - 1] == b'.') {
            return None;
        }
        let (from, to) = nth_argument(src, call, index)?;
        if is_postfix_text(src, from, to) {
            edits.push(edit(to, 0, ".as_ptr()"));
        } else {
            edits.push(edit(from, 0, "("));
            edits.push(edit(to, 0, ").as_ptr()"));
        }
    }
    Some(edits)
}

/// `frozen` on the parameter of the free function that receives the
/// argument at `arg`: the one `fn` of that name in the file.
fn frozen_receiving_param(src: &[u8], arg: usize) -> Option<Vec<TextEdit>> {
    let (open, index) = enclosing_paren(src, arg)?;
    let (name_at, name) = word_ending_at(src, open)?;
    if name_at > 0 && src[name_at - 1] == b'.' {
        return None;
    }
    let mut decls = uses_of(src, name, (0, 0))
        .into_iter()
        .filter(|&at| word_ending_at(src, trim_back(src, at)).map(|w| w.1) == Some("fn"));
    let decl = decls.next()?;
    if decls.next().is_some() {
        return None;
    }
    let mut list = decl + name.len();
    if src.get(list) == Some(&b'[') {
        list = matching(src, list)? + 1;
    }
    if src.get(list) != Some(&b'(') {
        return None;
    }
    let (from, to) = nth_argument(src, list, index)?;
    // `name: T`, with no mode of its own.
    let colon = find_top(src, from, to, |s, i| s[i] == b':')?;
    let ty = trim_fwd(src, colon + 1);
    let plain = !["frozen", "own", "mut", "ref"]
        .iter()
        .any(|w| word_at(src, ty, w))
        && word_ending_at(src, trim_back(src, colon)).is_some_and(|(s, _)| s == from);
    plain.then(|| vec![edit(ty, 0, "frozen ")])
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
    if let Some(call) = between(m, "`", "` is removed: encodings are the free functions of") {
        let (ty, method) = call.split_once('.')?;
        return removed_encoding_call(src, e.span.offset, ty, method);
    }
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
    if m.contains("hand over the data pointer at the call site with `.as_ptr()`")
        || m.contains("take `*const u8` and pass `s.as_ptr()`")
    {
        let name = between(m, "the foreign import '", "'")?;
        return extern_param_as_ptr(src, e.span.offset, name);
    }
    if let Some(to) = between(
        m,
        "has no call-site type arguments (design.md §8): write `",
        "()`",
    ) {
        return type_arg_call(src, e.span.offset, &format!("{to}()"));
    }
    if m.contains("`ref_eq` compares reference identity of `shared` values only") {
        return ref_eq_as_eq(src, e.span);
    }
    if let (Some(name), Some(bounds)) = (
        between(
            m,
            "trait alias `",
            "` is not available: trait aliases are deferred",
        ),
        between(m, "where the alias would be used: `", "`"),
    ) {
        return inline_trait_alias(src, e.span, name, bounds);
    }
    if let Some(name) = between(
        m,
        "`effect group ",
        "` is not available: effect groups are deferred",
    ) {
        return inline_effect_group(src, e.span, name);
    }
    if m.contains("holds only the trait's items; move it to an inherent `impl ") {
        return move_to_inherent_impl(src, e.span);
    }
    if m.ends_with("— the `drop` method must not declare its own generic parameters") {
        return drop_generics(src, e.span);
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
    complete_typed_at(source, 0, errors, diffs);
}

/// [`complete_typed`] over one module of a package, whose spans sit `base`
/// bytes past its file's start (`Module::span_base`): the edits are offsets
/// into `source`, the module's own file, which is what a reader of the
/// JSON applies them to, and the keys stay the diagnostics' spans.
pub(super) fn complete_typed_at(
    source: &str,
    base: usize,
    errors: &mut [TypeError],
    diffs: &mut rustc_hash::FxHashMap<SpanKey, Vec<TextEdit>>,
) {
    let src = source.as_bytes();
    // A slot's `Fn` as `MutFn` is the typechecker's, from the type's span
    // alone: the text there must be `Fn(`, or the edit is dropped
    // (B-2026-11-21-74).
    for e in errors.iter_mut() {
        if e.message.ends_with("declare the slot `MutFn(..)`")
            && e.fix_it.as_ref().is_some_and(|f| {
                src.get(f.span.offset..f.span.offset + 3) != Some(b"Fn(")
                    || (f.span.offset > 0 && is_ident(src[f.span.offset - 1]))
            })
        {
            e.fix_it = None;
        }
    }
    for e in errors.iter_mut() {
        if e.fix_it.is_some()
            || e.span.offset < base
            || e.span.offset - base + e.span.length > src.len()
            || diffs.contains_key(&SpanKey::from_span(&e.span))
        {
            continue;
        }
        let mut local = e.clone();
        local.span.offset -= base;
        let Some(mut edits) = typed_fix(src, &local) else {
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
    complete_resolved_at(source, 0, &mut r.errors, &mut r.error_fix_diffs);
}

/// [`complete_resolved`] over one module of a package; `base` as in
/// [`complete_typed_at`].
pub(super) fn complete_resolved_at(
    source: &str,
    base: usize,
    errors: &mut [crate::resolver::ResolveError],
    diffs: &mut rustc_hash::FxHashMap<SpanKey, Vec<TextEdit>>,
) {
    let src = source.as_bytes();
    // A binding's rename (E0127) is the resolver's, from spans alone: every
    // edit must cover the old name in the text, or none is kept
    // (B-2026-11-21-74).
    for e in errors.iter_mut() {
        if e.kind != crate::resolver::ResolveErrorKind::BindingNameClass {
            continue;
        }
        let Some(old) = between(&e.message, "`", "` is ") else {
            continue;
        };
        // The edits are offsets into `source` already (only the spans sit
        // `base` past it).
        let covers = |t: &TextEdit| src.get(t.offset..t.offset + t.length) == Some(old.as_bytes());
        let key = SpanKey::from_span(&e.span);
        if e.replacement.as_deref().is_some_and(|t| !covers(t)) {
            e.replacement = None;
        }
        if diffs.get(&key).is_some_and(|v| !v.iter().all(covers)) {
            diffs.remove(&key);
        }
    }
    let mut fixes = Vec::new();
    for e in errors.iter() {
        if e.replacement.is_some()
            || e.span.offset < base
            || e.span.offset - base + e.span.length > src.len()
            || diffs.contains_key(&SpanKey::from_span(&e.span))
        {
            continue;
        }
        let local = Span {
            offset: e.span.offset - base,
            ..e.span
        };
        let m = e.message.as_str();
        let edits = if let Some(target) = between(m, "a manual `impl Copy for ", "` is not allowed")
        {
            remove_impl_block(src, local).and_then(|mut del| {
                let target = target.split('[').next().unwrap_or(target);
                let add = add_derive(src, "struct", target, &["Copy", "Clone"])
                    .or_else(|| add_derive(src, "enum", target, &["Copy", "Clone"]))?;
                del.extend(add);
                Some(del)
            })
        } else if m.contains("` is not allowed: `CrossTask` is computed by the compiler") {
            remove_impl_block(src, local)
        } else if let Some(name) = between(
            m,
            "requires the enclosing enum `",
            "` to carry `#[derive(Default)]`",
        ) {
            add_derive(src, "enum", name, &["Default"])
        } else if m.contains("accepts no arguments — write it as a bare") {
            bare_attribute(src, local)
        } else if m.contains("is not valid on impl block; codegen hints attach to named functions")
        {
            hint_onto_methods(src, local.offset)
        } else if let Some(key) = between(m, "`serde(", ")` takes no value; write `") {
            bare_key(src, local.offset, key)
        } else if let (Some(old), Some(new)) = (
            between(m, "undefined loop label `", "`"),
            between(m, ", did you mean `", "`?"),
        ) {
            break_label(src, local, old, new)
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
            if let Some(e) = errors
                .iter_mut()
                .find(|e| e.span == span && e.replacement.is_none())
            {
                e.replacement = Some(Box::new(t));
            }
        } else {
            diffs.insert(SpanKey::from_span(&span), edits);
        }
    }
}

/// The ownership checker's: a closure's capture prefix that its body
/// contradicts, `frozen` on a `sync struct`, which adds nothing, and a
/// slice of a temporary bound past its statement.
pub(super) fn complete_ownership(source: &str, o: &mut crate::ownership::OwnershipCheckResult) {
    let src = source.as_bytes();
    let mut taken = Vec::new();
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
        } else if m.starts_with("`frozen` parameter `")
            && m.ends_with("` is passed to a non-`frozen` slot")
        {
            frozen_receiving_param(src, e.span.offset)
        } else if m == "slice from temporary value escapes the enclosing statement"
            && !o.error_fix_diffs.contains_key(&SpanKey::from_span(&e.span))
        {
            hoist_sliced_temporary(src, e.span.offset, &mut taken)
        } else {
            None
        };
        if let Some(mut edits) = fix {
            if edits.len() == 1 {
                e.replacement = Some(Box::new(edits.remove(0)));
            } else {
                o.error_fix_diffs.insert(SpanKey::from_span(&e.span), edits);
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
