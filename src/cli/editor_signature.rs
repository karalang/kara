//! Signature help (v1 checklist): inside a call's parentheses, the callee's
//! declaration, the parameter the cursor is on, and each parameter's mode.
//! A bare parameter borrows (core-semantics §4.1), which is what a reader
//! new to Kāra does not expect, so the mode is said for every parameter:
//! borrowed, `own` (the argument moves in) or `mut ref` (the call may
//! write it, and the argument is marked `mut`).
//!
//! Only the body at the cursor is checked ([`super::editor_incremental::focus`]).
//! A builtin type's method that the checker answers itself, with no Kāra
//! declaration (`Vec.push`), has no signature to show.

use super::editor_nav::{self, Nav};
use crate::ast::{Expr, ExprKind, ImplItem, Item, Param, SelfParam, TraitItem, TypeKind};
use crate::ids::NodeId;
use crate::resolver::SpanKey;
use crate::span_visitor::{walk_item_mut_with, MutVisitor};
use crate::token::Span;
use crate::typechecker::types::Type;
use std::collections::HashMap;
use std::sync::OnceLock;

/// How a parameter takes its argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamMode {
    /// A bare parameter or `self`: the call reads the argument, which stays
    /// the caller's (a copy, for a `Copy` type).
    Borrowed,
    /// `own T` / `own self`: the argument moves into the call.
    Owned,
    /// `mut ref T` / `mut ref self` / `mut Slice[T]`: the call may write the
    /// argument, which the call site marks `mut`.
    MutRef,
}

/// One parameter of a [`SignatureHelp`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureParam {
    /// Its byte range in [`SignatureHelp::label`].
    pub start: usize,
    pub end: usize,
    pub mode: ParamMode,
    /// Its `///` doc comment, when it has one.
    pub doc: Option<String>,
}

/// The signature of the call around a cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureHelp {
    /// The declaration on one line: `fn push_all(mut ref self, xs: own Vec[T])`.
    pub label: String,
    /// The parameters, the receiver first when there is one.
    pub params: Vec<SignatureParam>,
    /// The parameter the cursor is on; `None` past the last.
    pub active: Option<usize>,
    /// The function's `///` doc comment.
    pub doc: Option<String>,
}

const NOT_CALLS: &[&str] = &[
    "if", "while", "match", "for", "in", "return", "and", "or", "not", "loop", "else", "with",
    "break", "yield", "await", "fn", "let", "mut", "ref", "own",
];

/// The signature of the call whose parentheses hold byte `at` of `source`.
pub(crate) fn signature_help(source: &str, at: usize) -> Option<SignatureHelp> {
    let mut at = at.min(source.len());
    while !source.is_char_boundary(at) {
        at -= 1;
    }
    let call = open_call(source, at)?;
    // Checked as it is, when it closes the call; then with the call's
    // arguments taken out, which a name being typed in one of them would
    // otherwise stop at the resolver; then, unclosed, closed here, and as a
    // statement.
    let open = call.open;
    let mut texts: Vec<String> = Vec::new();
    match call.close {
        Some(close) => {
            texts.push(source.to_string());
            texts.push(format!("{}{}", &source[..=open], &source[close..]));
        }
        None => {
            for end in [")", ");"] {
                texts.push(format!("{}{end}{}", &source[..=open], &source[at..]));
            }
        }
    }
    let (nav, found) = texts.iter().find_map(|text| {
        let nav = super::editor_complete::analyze_focused(text, open)?;
        let found = find_call(&nav, open)?;
        Some((nav, found))
    })?;
    let (text, _) = nav.text();
    let mut index = call.commas;
    let mut help = match callee(&nav, &found) {
        Some(sig) => {
            let help = render(text, &sig)?;
            if sig.self_param.is_some() && found.receiver {
                index += 1;
            }
            // `name: value` names a parameter after the `;`.
            let arg = source.get(call.arg_start..at).unwrap_or("").trim_start();
            let word: String = arg
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            let rest = arg[word.len()..].trim_start();
            if !word.is_empty() && rest.starts_with(':') && !rest.starts_with("::") {
                let offset = usize::from(sig.self_param.is_some());
                if let Some(i) = sig
                    .params
                    .iter()
                    .position(|p| p.is_named && p.name() == Some(word.as_str()))
                {
                    index = i + offset;
                }
            }
            help
        }
        // No Kāra declaration: a builtin type's method, as the library
        // reference states it, a variant, or a prelude printer.
        None => {
            let (help, has_self) = undeclared(&nav, &found)?;
            if found.receiver && has_self {
                index += 1;
            }
            help
        }
    };
    help.active = (index < help.params.len()).then_some(index);
    Some(help)
}

/// The innermost open `(` before `at` that follows a name.
struct OpenCall {
    open: usize,
    /// The commas between it and `at`, at its own depth.
    commas: usize,
    /// Where the argument at `at` starts.
    arg_start: usize,
    /// Its `)`, when the buffer closes it.
    close: Option<usize>,
}

fn open_call(text: &str, at: usize) -> Option<OpenCall> {
    let b = text.as_bytes();
    // (bracket, offset, commas, last comma)
    let mut stack: Vec<(u8, usize, usize, usize)> = Vec::new();
    let mut i = 0;
    let mut found: Option<(usize, usize, usize, usize)> = None;
    while i < b.len() {
        if i == at && found.is_none() {
            let (kind, open, commas, arg_start) = *stack.last()?;
            if kind != b'(' {
                return None;
            }
            found = Some((open, commas, arg_start, stack.len()));
        }
        match b[i] {
            b'/' if b.get(i + 1) == Some(&b'/') => {
                let end = text[i..].find('\n').map_or(b.len(), |n| i + n);
                if at > i && at <= end && found.is_none() {
                    return None;
                }
                i = end;
                continue;
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                let end = text[i + 2..].find("*/").map_or(b.len(), |n| i + 2 + n + 2);
                if at > i && at < end && found.is_none() {
                    return None;
                }
                i = end;
                continue;
            }
            b'"' => {
                let mut j = i + 1;
                while j < b.len() && b[j] != b'"' {
                    j += if b[j] == b'\\' { 2 } else { 1 };
                }
                let end = (j + 1).min(b.len());
                if at > i && at < end && found.is_none() {
                    return None;
                }
                i = end;
                continue;
            }
            b'\'' => {
                // A char literal: `'x'`, `'\n'`, `'\u{..}'`.
                let close = if b.get(i + 1) == Some(&b'\\') {
                    text[i + 2..]
                        .find('\'')
                        .map(|n| i + 2 + n)
                        .filter(|&c| c - i < 12)
                } else {
                    text[i + 1..]
                        .char_indices()
                        .nth(1)
                        .filter(|(_, c)| *c == '\'')
                        .map(|(n, _)| i + 1 + n)
                };
                if let Some(c) = close {
                    i = c + 1;
                    continue;
                }
            }
            c @ (b'(' | b'[' | b'{') => stack.push((c, i, 0, i + 1)),
            c @ (b')' | b']' | b'}') => {
                if let Some((open, depth)) = found.map(|f| (f.0, f.3)) {
                    if stack.len() == depth && stack.last().map(|s| s.1) == Some(open) {
                        // A `}` or `]` there means the `(` was left open.
                        let close = (c == b')').then_some(i);
                        return call_at(text, open, found?, close);
                    }
                }
                stack.pop();
            }
            b',' => {
                if let Some(top) = stack.last_mut() {
                    top.2 += 1;
                    top.3 = i + 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    if found.is_none() && at == b.len() {
        let (kind, open, commas, arg_start) = *stack.last()?;
        if kind != b'(' {
            return None;
        }
        found = Some((open, commas, arg_start, stack.len()));
    }
    let f = found?;
    call_at(text, f.0, f, None)
}

fn call_at(
    text: &str,
    open: usize,
    f: (usize, usize, usize, usize),
    close: Option<usize>,
) -> Option<OpenCall> {
    // A name, then optionally `[...]` type arguments, then the `(`.
    let b = text.as_bytes();
    let mut j = open;
    if j > 0 && b[j - 1] == b']' {
        let mut depth = 0usize;
        while j > 0 {
            j -= 1;
            match b[j] {
                b']' => depth += 1,
                b'[' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                _ => {}
            }
        }
    }
    let ident = |c: u8| c.is_ascii_alphanumeric() || c == b'_' || c >= 0x80;
    let end = j;
    while j > 0 && ident(b[j - 1]) {
        j -= 1;
    }
    let word = text.get(j..end)?;
    if word.is_empty()
        || word.starts_with(|c: char| c.is_ascii_digit())
        || NOT_CALLS.contains(&word)
    {
        return None;
    }
    // `fn name(` declares.
    let lead = text[..j].trim_end();
    if lead.ends_with("fn") && !lead[..lead.len() - 2].ends_with(|c: char| ident(c as u8)) {
        return None;
    }
    Some(OpenCall {
        open,
        commas: f.1,
        arg_start: f.2,
        close,
    })
}

/// The call whose `(` is at `open`.
struct FoundCall {
    id: NodeId,
    /// The callee of a free call.
    callee: Option<Expr>,
    /// A method call on a value, which passes the receiver as `self`.
    receiver: bool,
    /// A method call's name and receiver.
    method: Option<(String, Expr)>,
}

/// The `(` after the name ending at `from`, past `[...]` type arguments.
fn paren_after(text: &str, from: usize) -> Option<usize> {
    let b = text.as_bytes();
    let skip = |mut i: usize| {
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        i
    };
    let mut i = skip(from);
    if b.get(i) == Some(&b'[') {
        i = matching(text, i)? + 1;
        i = skip(i);
    }
    (b.get(i) == Some(&b'(')).then_some(i)
}

/// Where a free call's callee ends. A path's span runs to the end of the
/// call (`Shape.Rect(1.0, 2.0)`), so it ends after its last segment.
fn callee_end(text: &str, callee: &Expr) -> usize {
    match &callee.kind {
        ExprKind::Path { segments, .. } => {
            let mut at = callee.span.offset;
            for seg in segments {
                match text.get(at..).and_then(|t| t.find(seg.as_str())) {
                    Some(i) => at += i + seg.len(),
                    None => return editor_nav::expr_end(text, callee),
                }
            }
            at
        }
        _ => editor_nav::expr_end(text, callee),
    }
}

/// The bracket closing the one at `open`.
fn matching(text: &str, open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (i, c) in text.as_bytes().iter().enumerate().skip(open) {
        match c {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

fn find_call(nav: &Nav, open: usize) -> Option<FoundCall> {
    struct Find<'a> {
        text: &'a str,
        open: usize,
        hit: Option<FoundCall>,
    }
    impl MutVisitor for Find<'_> {
        fn span(&mut self, _: &mut Span) {}
        fn expr(&mut self, e: &mut Expr) {
            match &e.kind {
                ExprKind::Call { callee, .. } => {
                    if paren_after(self.text, callee_end(self.text, callee)) == Some(self.open) {
                        self.hit = Some(FoundCall {
                            id: e.id,
                            callee: Some((**callee).clone()),
                            receiver: false,
                            method: None,
                        });
                    }
                }
                ExprKind::MethodCall { object, method, .. } => {
                    let Some(name) = editor_nav::member_name_offset(
                        self.text,
                        editor_nav::expr_end(self.text, object),
                        method,
                    ) else {
                        return;
                    };
                    if paren_after(self.text, name + method.len()) == Some(self.open) {
                        // `Type.f(...)` names the type, and passes no receiver.
                        let on_type = matches!(&object.kind,
                            ExprKind::Identifier(n) if n.starts_with(|c: char| c.is_uppercase()))
                            || matches!(&object.kind, ExprKind::Path { .. } | ExprKind::SelfType);
                        self.hit = Some(FoundCall {
                            id: e.id,
                            callee: None,
                            receiver: !on_type,
                            method: Some((method.clone(), (**object).clone())),
                        });
                    }
                }
                _ => {}
            }
        }
    }
    let (text, user_len) = nav.text();
    let mut find = Find {
        text: &text[..user_len],
        open,
        hit: None,
    };
    for item in nav.user_items() {
        if editor_nav::span_contains(item.span(), open) {
            walk_item_mut_with(&mut item.clone(), &mut find);
        }
    }
    find.hit
}

/// A function's or a trait method's declaration, as signature help reads it.
struct FnLike<'a> {
    name: &'a str,
    span: Span,
    self_param: Option<&'a SelfParam>,
    self_is_own: bool,
    self_span: Option<Span>,
    params: &'a [Param],
    /// Where the body starts, when there is one.
    body_start: Option<usize>,
    doc: Option<&'a str>,
}

impl<'a> FnLike<'a> {
    fn of_fn(f: &'a crate::ast::Function) -> Self {
        Self {
            name: &f.name,
            span: f.span,
            self_param: f.self_param.as_ref(),
            self_is_own: f.self_is_own,
            self_span: f.self_span,
            params: &f.params,
            body_start: Some(f.body.span.offset),
            doc: f.doc_comment.as_deref(),
        }
    }

    fn of_trait(m: &'a crate::ast::TraitMethod) -> Self {
        Self {
            name: &m.name,
            span: m.span,
            self_param: m.self_param.as_ref(),
            self_is_own: m.self_is_own,
            self_span: m.self_span,
            params: &m.params,
            body_start: m.body.as_ref().map(|b| b.span.offset),
            doc: m.doc_comment.as_deref(),
        }
    }
}

/// The declaration the call at `found` calls.
fn callee<'a>(nav: &'a Nav, found: &FoundCall) -> Option<FnLike<'a>> {
    let typed = nav.typed();
    if let Some(key) = typed.node_method_callees.get(&found.id) {
        let (ty, method) = key.rsplit_once('.')?;
        return method_decl(nav, ty, method, typed.node_method_impls.get(&found.id));
    }
    let callee = found.callee.as_ref()?;
    // `Type.f(...)` written as a path.
    if let ExprKind::Path { segments, .. } = &callee.kind {
        if let [.., ty, method] = segments.as_slice() {
            if let Some(f) = method_decl(nav, ty, method, None) {
                return Some(f);
            }
        }
    }
    let sym = *nav
        .resolved()
        .resolutions
        .get(&SpanKey::from_span(&callee.span))?;
    let def = nav.resolved().symbol_table.get_symbol(sym).span;

    let (text, _) = nav.text();
    nav.program().items.iter().find_map(|item| match item {
        Item::Function(f)
            if f.span.offset == def.offset
                || editor_nav::fn_name_offset(text, &f.span, &f.name)
                    .is_some_and(|(_, n)| n == def.offset) =>
        {
            Some(FnLike::of_fn(f))
        }
        _ => None,
    })
}

/// `ty.method`'s declaration: in an impl of `ty` (the one the call resolved
/// to, of two), a trait named `ty`, or a trait `ty` implements.
fn method_decl<'a>(
    nav: &'a Nav,
    ty: &str,
    method: &str,
    impl_at: Option<&SpanKey>,
) -> Option<FnLike<'a>> {
    let items = &nav.program().items;
    let head = |ib: &crate::ast::ImplBlock| match &ib.target_type.kind {
        TypeKind::Path(p) => p.segments.last().cloned(),
        _ => None,
    };
    let mut first = None;
    let mut traits: Vec<String> = Vec::new();
    for item in items {
        let Item::ImplBlock(ib) = item else { continue };
        if head(ib).as_deref() != Some(ty) {
            continue;
        }
        if let Some(t) = &ib.trait_name {
            traits.extend(t.segments.last().cloned());
        }
        for it in &ib.items {
            let ImplItem::Method(f) = it else { continue };
            if f.name != method {
                continue;
            }
            if impl_at.is_none_or(|k| k.0 == ib.target_type.span.offset) {
                return Some(FnLike::of_fn(f));
            }
            first.get_or_insert(FnLike::of_fn(f));
        }
    }
    if first.is_some() {
        return first;
    }
    items.iter().find_map(|item| match item {
        Item::TraitDef(td) if td.name == ty || traits.contains(&td.name) => {
            td.items.iter().find_map(|ti| match ti {
                TraitItem::Method(m) if m.name == method => Some(FnLike::of_trait(m)),
                _ => None,
            })
        }
        _ => None,
    })
}

fn collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The declaration `f` on one line, with its parameters' ranges in it.
fn render(text: &str, f: &FnLike) -> Option<SignatureHelp> {
    let (kw, name_at) = editor_nav::fn_name_offset(text, &f.span, f.name)?;
    let open = paren_after(text, name_at + f.name.len())?;
    let close = matching(text, open)?;
    let mut label = collapse(&text[kw..open]);
    label.push('(');
    let mut params = Vec::new();
    let mut push = |label: &mut String, part: String, mode, doc: Option<String>, sep: &str| {
        if !params.is_empty() {
            label.push_str(sep);
        }
        let start = label.len();
        label.push_str(&part);
        params.push(SignatureParam {
            start,
            end: label.len(),
            mode,
            doc,
        });
    };
    if let Some(sp) = f.self_param {
        let part = f
            .self_span
            .and_then(|s| text.get(s.offset..s.offset + s.length))
            .map(collapse)
            .unwrap_or_else(|| "self".to_string());
        let mode = match sp {
            SelfParam::MutRef => ParamMode::MutRef,
            _ if f.self_is_own || part.starts_with("own ") => ParamMode::Owned,
            _ => ParamMode::Borrowed,
        };
        push(&mut label, part, mode, None, ", ");
    }
    let mut named = false;
    for p in f.params {
        let part = collapse(text.get(p.span.offset..p.span.offset + p.span.length)?);
        let ty = part.split_once(':').map_or("", |(_, t)| t.trim_start());
        let mode = if p.is_own || ty.starts_with("own ") {
            ParamMode::Owned
        } else if ty.starts_with("mut ") {
            ParamMode::MutRef
        } else {
            ParamMode::Borrowed
        };
        let sep = if p.is_named && !named { "; " } else { ", " };
        named |= p.is_named;
        push(&mut label, part, mode, p.doc_comment.clone(), sep);
    }
    label.push(')');
    let end = f
        .body_start
        .unwrap_or(f.span.offset + f.span.length)
        .min(text.len());
    if close < end {
        let tail = collapse(&text[close + 1..end]);
        let tail = tail.trim_end_matches(';').trim_end();
        if !tail.is_empty() {
            label.push(' ');
            label.push_str(tail);
        }
    }
    Some(SignatureHelp {
        label,
        params,
        active: None,
        doc: f.doc.map(str::to_string),
    })
}

/// The library reference's pages that list the builtin types' methods.
const LIBRARY_PAGES: &[&str] = &[
    include_str!("../../docs/library/collections.md"),
    include_str!("../../docs/library/strings.md"),
    include_str!("../../docs/library/core-types.md"),
    include_str!("../../docs/library/iterators.md"),
];

/// `(type, method)` to its signature and notes, from the method tables of
/// [`LIBRARY_PAGES`]: a row `` | `push` | `fn push(mut ref self, val: own T)` | Appends | ``
/// under a heading that names its types (`` ## `Vec[T]` ``,
/// `` ## Reading: `Str` and `String` ``).
fn library_methods() -> &'static HashMap<(String, String), (String, String)> {
    static TABLE: OnceLock<HashMap<(String, String), (String, String)>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut out = HashMap::new();
        for page in LIBRARY_PAGES {
            let mut types: Vec<String> = Vec::new();
            for line in page.lines() {
                if let Some(h) = line.strip_prefix("## ") {
                    types = h
                        .split('`')
                        .skip(1)
                        .step_by(2)
                        .map(|t| t.split('[').next().unwrap_or(t).trim().to_string())
                        .filter(|t| t.starts_with(|c: char| c.is_ascii_uppercase()))
                        .collect();
                    continue;
                }
                let cells: Vec<&str> = line.split(" | ").collect();
                let [first, sig, rest @ ..] = cells.as_slice() else {
                    continue;
                };
                let Some(name) = first.strip_prefix("| `").and_then(|n| n.strip_suffix('`')) else {
                    continue;
                };
                // The first of `fn a(..)` / `fn a(..)`.
                let Some(sig) = sig
                    .strip_prefix("`fn ")
                    .and_then(|s| s.split('`').next())
                    .map(|s| format!("fn {s}"))
                else {
                    continue;
                };
                let notes = rest.join(" | ");
                let notes = notes.trim_end_matches('|').trim().to_string();
                for t in &types {
                    out.entry((t.clone(), name.to_string()))
                        .or_insert_with(|| (sig.clone(), notes.clone()));
                }
            }
        }
        out
    })
}

/// A signature written as text, `fn push(mut ref self, val: own T)`, with
/// its parameters' ranges and modes, and whether it takes a receiver.
fn render_text(sig: &str, doc: Option<String>, all_owned: bool) -> Option<(SignatureHelp, bool)> {
    let name_end = sig.find(['(', '[']).filter(|&i| i > 0)?;
    let open = paren_after(sig, name_end)?;
    let close = matching(sig, open)?;
    let mut params = Vec::new();
    let mut has_self = false;
    let mut depth = 0usize;
    let mut start = open + 1;
    let b = sig.as_bytes();
    for i in open + 1..=close {
        match b[i] {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' if i < close => depth -= 1,
            b',' | b';' | b')' if depth == 0 => {
                let part = &sig[start..i];
                let lead = part.len() - part.trim_start().len();
                let part = part.trim();
                if !part.is_empty() {
                    let is_self =
                        params.is_empty() && part.split_whitespace().last() == Some("self");
                    has_self |= is_self;
                    let mode = if all_owned {
                        ParamMode::Owned
                    } else {
                        text_mode(part, is_self)
                    };
                    params.push(SignatureParam {
                        start: start + lead,
                        end: start + lead + part.len(),
                        mode,
                        doc: None,
                    });
                }
                start = i + 1;
            }
            _ => {}
        }
    }
    Some((
        SignatureHelp {
            label: sig.to_string(),
            params,
            active: None,
            doc,
        },
        has_self,
    ))
}

/// A parameter's mode from how it is written: `own T` / `own self`,
/// `mut ref T` / `mut Slice[T]` / `mut ref self`, or bare.
fn text_mode(part: &str, is_self: bool) -> ParamMode {
    let ty = if is_self {
        part.trim_end_matches("self").trim()
    } else {
        part.split_once(':').map_or("", |(_, t)| t.trim_start())
    };
    if ty.starts_with("own") {
        ParamMode::Owned
    } else if ty.starts_with("mut") {
        ParamMode::MutRef
    } else {
        ParamMode::Borrowed
    }
}

/// The name a type's methods are listed under in the library reference.
fn listed_head(ty: &Type) -> Option<String> {
    match ty {
        Type::Ref(t) | Type::MutRef(t) => listed_head(t),
        Type::Str => Some("Str".to_string()),
        Type::Slice { .. } => Some("Slice".to_string()),
        _ => editor_nav::type_head(ty).map(str::to_string),
    }
}

/// A call whose callee has no Kāra declaration.
fn undeclared(nav: &Nav, found: &FoundCall) -> Option<(SignatureHelp, bool)> {
    let typed = nav.typed();
    if let Some((method, object)) = &found.method {
        let head = match typed.node_method_callees.get(&found.id) {
            Some(key) => key.rsplit_once('.').map(|(t, _)| t.to_string()),
            None => match &object.kind {
                ExprKind::Identifier(n) if !found.receiver => Some(n.clone()),
                _ => typed
                    .node_types
                    .get(&object.id)
                    .map(|(t, _)| t)
                    .or_else(|| typed.expr_types.get(&SpanKey::from_span(&object.span)))
                    .and_then(listed_head),
            },
        }?;
        // A `Vec` has every `Slice` method, a `String` every `Str` one.
        let also: &[&str] = match head.as_str() {
            "Vec" | "Array" => &["Slice"],
            "String" => &["Str"],
            "Str" => &["String"],
            _ => &[],
        };
        let table = library_methods();
        let listed = std::iter::once(head.as_str())
            .chain(also.iter().copied())
            .find_map(|t| table.get(&(t.to_string(), method.clone())));
        return match listed {
            Some((sig, notes)) => {
                let doc = (!notes.is_empty()).then(|| notes.clone());
                render_text(sig, doc, false)
            }
            // `Shape.Circle(r)`.
            None => variant(nav, Some(&head), method),
        };
    }
    let callee = found.callee.as_ref()?;
    let name = match &callee.kind {
        ExprKind::Identifier(n) => n.clone(),
        ExprKind::Path { segments, .. } => {
            // `Vec.new()`, `String.with_capacity(n)`: a builtin type's
            // function, as the library reference lists it.
            if let [.., ty, f] = segments.as_slice() {
                if let Some((sig, notes)) = library_methods().get(&(ty.clone(), f.clone())) {
                    let doc = (!notes.is_empty()).then(|| notes.clone());
                    return render_text(sig, doc, false);
                }
                return variant(nav, Some(ty), f);
            }
            segments.last()?.clone()
        }
        // `Shape.Circle(r)`.
        ExprKind::FieldAccess { object, field } => {
            let ExprKind::Identifier(of) = &object.kind else {
                return None;
            };
            return variant(nav, Some(of), field);
        }
        _ => return None,
    };
    // The prelude's variants and printers (docs/library/README.md § The
    // prelude).
    let builtin = match name.as_str() {
        "Some" => Some(("Some(value: own T)", None)),
        "Ok" => Some(("Ok(value: own T)", None)),
        "Err" => Some(("Err(error: own E)", None)),
        "print" | "println" | "eprintln" => Some((
            "",
            Some("Takes any `Display` value; `print` and `println` write `Stdout`, `eprintln` `Stderr`."),
        )),
        _ => None,
    };
    if let Some((sig, doc)) = builtin {
        let sig = if sig.is_empty() {
            format!("fn {name}(value: impl Display)")
        } else {
            sig.to_string()
        };
        let owned = !name.ends_with("print") && !name.ends_with("println");
        return render_text(&sig, doc.map(str::to_string), owned);
    }
    variant(nav, None, &name)
}

/// Tuple variant `name` of enum `of` (of any enum, when `None`) the program
/// or the library declares.
fn variant(nav: &Nav, of: Option<&str>, name: &str) -> Option<(SignatureHelp, bool)> {
    let (text, _) = nav.text();
    nav.program().items.iter().find_map(|item| {
        let Item::EnumDef(e) = item else { return None };
        if of.is_some_and(|o| o != e.name) {
            return None;
        }
        let v = e.variants.iter().find(|v| v.name == name)?;
        let crate::ast::VariantKind::Tuple(fields) = &v.kind else {
            return None;
        };
        let parts: Vec<String> = fields
            .iter()
            .map(|t| {
                collapse(
                    text.get(t.span.offset..t.span.offset + t.span.length)
                        .unwrap_or("_"),
                )
            })
            .collect();
        let sig = format!("{}.{}({})", e.name, v.name, parts.join(", "));
        render_text(&sig, None, true)
    })
}
