//! Editor navigation the resolver cannot answer on its own (B-2026-11-07-45):
//! go-to-definition on a method call's or a field access's name, which the
//! typechecker resolves, and hover on a `let` binding's own name, which is a
//! pattern rather than an expression. Both read the tables `karac check`
//! builds for the buffer, with the Kāra library appended, so a library
//! method's definition is found in the library source it was checked from.

use super::Pipeline;
use crate::ast::{Expr, ExprKind, ImplItem, Item, Pattern, PatternKind, TypeKind};
use crate::ids::NodeId;
use crate::resolver::SpanKey;
use crate::span_visitor::{walk_item_mut_with, MutVisitor};
use crate::token::Span;
use crate::typechecker::types::Type;

/// One buffer checked as `karac check` checks it, kept for a position query.
pub(crate) struct Nav {
    pipeline: Pipeline,
    /// The checked text: the buffer, then the library.
    text: String,
    /// Where the library starts in `text`.
    user_len: usize,
}

/// Where a definition is: in the buffer, or in one file of the library
/// source, with the offsets in that file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NavSite {
    Buffer {
        offset: usize,
        length: usize,
    },
    Library {
        file: &'static str,
        text: &'static str,
        offset: usize,
        length: usize,
    },
}

/// A member name under the cursor and what it names.
pub(crate) struct Member {
    /// The member name's own span in the buffer.
    pub(crate) name_offset: usize,
    pub(crate) name_length: usize,
    /// Where it is defined, when that is found.
    pub(crate) def: Option<NavSite>,
    /// For a method: its `Type.method` key and its declaration's header
    /// (`fn greet(self) -> String`).
    pub(crate) method: Option<(String, Option<String>)>,
}

/// Check `source` for a position query; `None` when it does not parse.
pub(crate) fn analyze(source: &str) -> Option<Nav> {
    nav_of(super::editor::checked(source), source)
}

/// [`analyze`] against the whole library, for completion.
pub(crate) fn analyze_whole_library(source: &str) -> Option<Nav> {
    nav_of(super::editor::checked_whole_library(source), source)
}

/// The whole library, checked once a process against an empty program:
/// its types, impls and traits, for completion to offer what a buffer has
/// not named yet without checking the whole library with every buffer
/// (B-2026-11-09-28).
pub(crate) fn whole_library() -> Option<&'static Nav> {
    static WHOLE: std::sync::OnceLock<Option<Nav>> = std::sync::OnceLock::new();
    WHOLE
        .get_or_init(|| analyze_whole_library("fn main() {}\n"))
        .as_ref()
}

fn nav_of(pipeline: super::Pipeline, source: &str) -> Option<Nav> {
    pipeline.typed.as_ref()?;
    pipeline.resolved.as_ref()?;
    pipeline.effects.as_ref()?;
    let text = pipeline
        .source
        .clone()
        .unwrap_or_else(|| source.to_string());
    let user_len = pipeline.library_len.unwrap_or(text.len()).min(text.len());
    Some(Nav {
        pipeline,
        text,
        user_len,
    })
}

impl Nav {
    pub(crate) fn resolved(&self) -> &crate::resolver::ResolveResult {
        self.pipeline.resolved.as_ref().expect("checked in analyze")
    }

    pub(crate) fn typed(&self) -> &crate::typechecker::TypeCheckResult {
        self.pipeline.typed.as_ref().expect("checked in analyze")
    }

    pub(crate) fn effects(&self) -> &crate::effectchecker::EffectCheckResult {
        self.pipeline.effects.as_ref().expect("checked in analyze")
    }

    /// The checked program: the buffer's items, then the library's.
    pub(crate) fn program(&self) -> &crate::ast::Program {
        &self.pipeline.parsed.program
    }

    /// The checked text (the buffer, then the library) and where the
    /// library starts in it.
    pub(crate) fn text(&self) -> (&str, usize) {
        (&self.text, self.user_len)
    }

    pub(crate) fn user_items(&self) -> impl Iterator<Item = &Item> {
        let user_len = self.user_len;
        self.pipeline
            .parsed
            .program
            .items
            .iter()
            .filter(move |item| item.span().offset < user_len)
    }

    /// The method-call or field-access name under `at`, if any.
    pub(crate) fn member_at(&self, at: usize) -> Option<Member> {
        struct Find<'a> {
            text: &'a str,
            at: usize,
            hit: Option<(bool, String, NodeId, Box<Expr>, usize)>,
        }
        impl MutVisitor for Find<'_> {
            fn span(&mut self, _: &mut Span) {}
            fn expr(&mut self, e: &mut Expr) {
                let (object, name, is_method) = match &e.kind {
                    ExprKind::MethodCall { object, method, .. } => (object, method, true),
                    ExprKind::FieldAccess { object, field } => (object, field, false),
                    _ => return,
                };
                let Some(off) = member_name_offset(self.text, expr_end(self.text, object), name)
                else {
                    return;
                };
                if self.at >= off && self.at < off + name.len() {
                    self.hit = Some((is_method, name.clone(), e.id, object.clone(), off));
                }
            }
        }
        let mut find = Find {
            text: &self.text[..self.user_len],
            at,
            hit: None,
        };
        for item in self.user_items() {
            if !span_contains(item.span(), at) {
                continue;
            }
            walk_item_mut_with(&mut item.clone(), &mut find);
        }
        let (is_method, name, id, object, name_offset) = find.hit?;
        let typed = self.typed();
        if is_method {
            let key = typed.node_method_callees.get(&id).cloned();
            let found = key.as_deref().and_then(|k| {
                let (ty, method) = k.rsplit_once('.')?;
                self.method_def(ty, method, typed.node_method_impls.get(&id))
            });
            let (def, header) = match found {
                Some((site, header)) => (Some(site), Some(header)),
                None => (None, None),
            };
            Some(Member {
                name_offset,
                name_length: name.len(),
                def,
                method: key.map(|k| (k, header)),
            })
        } else {
            let ty = typed
                .node_types
                .get(&object.id)
                .map(|(t, _)| t)
                .or_else(|| typed.expr_types.get(&SpanKey::from_span(&object.span)))?;
            let def = self.field_def(type_head(ty)?, &name);
            Some(Member {
                name_offset,
                name_length: name.len(),
                def,
                method: None,
            })
        }
    }

    /// The type of the `let` (or other pattern) binding whose name is under
    /// `at`, with the name's span.
    pub(crate) fn binding_at(&self, at: usize) -> Option<(&Type, usize, usize)> {
        struct Find {
            at: usize,
            hit: Option<(NodeId, Span)>,
        }
        impl MutVisitor for Find {
            fn span(&mut self, _: &mut Span) {}
            fn pattern(&mut self, p: &mut Pattern) {
                if matches!(p.kind, PatternKind::Binding(_))
                    && span_contains(&p.span, self.at)
                    && self.hit.is_none_or(|(_, s)| p.span.length < s.length)
                {
                    self.hit = Some((p.id, p.span));
                }
            }
        }
        let mut find = Find { at, hit: None };
        for item in self.user_items() {
            if span_contains(item.span(), at) {
                walk_item_mut_with(&mut item.clone(), &mut find);
            }
        }
        let (id, span) = find.hit?;
        let (ty, _) = self.typed().node_types.get(&id)?;
        Some((ty, span.offset, span.length))
    }

    /// The definition of `ty.method`, and the declaration's header.
    fn method_def(
        &self,
        ty: &str,
        method: &str,
        impl_at: Option<&SpanKey>,
    ) -> Option<(NavSite, String)> {
        let mut found = None;
        for item in &self.pipeline.parsed.program.items {
            let Item::ImplBlock(ib) = item else {
                continue;
            };
            let TypeKind::Path(p) = &ib.target_type.kind else {
                continue;
            };
            if p.segments.last().map(String::as_str) != Some(ty) {
                continue;
            }
            for it in &ib.items {
                let ImplItem::Method(f) = it else { continue };
                if f.name != method {
                    continue;
                }
                let Some((fn_kw, name_off)) = fn_name_offset(&self.text, &f.span, &f.name) else {
                    continue;
                };
                let header_end = f.body.span.offset.max(fn_kw).min(self.text.len());
                let header = self.text[fn_kw..header_end]
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ");
                let hit = (self.site(name_off, f.name.len())?, header);
                // Of two impls of the method, the one the call resolved to.
                let chosen = impl_at.is_none_or(|k| k.0 == ib.target_type.span.offset);
                if chosen {
                    return Some(hit);
                }
                found.get_or_insert(hit);
            }
        }
        found
    }

    /// The definition of field `field` of struct `ty`.
    fn field_def(&self, ty: &str, field: &str) -> Option<NavSite> {
        self.pipeline
            .parsed
            .program
            .items
            .iter()
            .find_map(|item| match item {
                Item::StructDef(s) if s.name == ty => s
                    .fields
                    .iter()
                    .find(|f| f.name == field)
                    .map(|f| f.name_span),
                _ => None,
            })
            .and_then(|s| self.site(s.offset, s.length))
    }

    /// `offset` in the checked text as a buffer or library-file position.
    /// `None` past the always-appended library sources (the baked modules
    /// after them are rewritten as they are appended).
    fn site(&self, offset: usize, length: usize) -> Option<NavSite> {
        if offset < self.user_len {
            return Some(NavSite::Buffer { offset, length });
        }
        let mut start = self.user_len;
        for (file, text) in crate::prelude::LIBRARY_SOURCES {
            let begin = start + 1;
            if offset >= begin && offset + length <= begin + text.len() {
                return Some(NavSite::Library {
                    file,
                    text,
                    offset: offset - begin,
                    length,
                });
            }
            start = begin + text.len();
        }
        None
    }
}

pub(crate) fn span_contains(span: &Span, at: usize) -> bool {
    at >= span.offset && at < span.offset.saturating_add(span.length)
}

/// The name a type is declared under: `User` for `User`, `ref User`,
/// `Rc[User]` or a `shared` `User`.
pub(crate) fn type_head(ty: &Type) -> Option<&str> {
    match ty {
        Type::Named { name, .. } | Type::Shared(name) => Some(name),
        Type::Ref(t) | Type::MutRef(t) | Type::Rc(t) | Type::Arc(t) | Type::Weak(t) => type_head(t),
        _ => None,
    }
}

/// Where `e` ends in the source. A method call's span covers only its
/// receiver, so its end is its closing `)`; a field access ends at its name.
pub(crate) fn expr_end(text: &str, e: &Expr) -> usize {
    match &e.kind {
        ExprKind::MethodCall {
            args_close_span, ..
        } if args_close_span.length > 0 => args_close_span.offset + args_close_span.length,
        ExprKind::FieldAccess { object, field } => {
            member_name_offset(text, expr_end(text, object), field)
                .map_or(e.span.offset + e.span.length, |o| o + field.len())
        }
        _ => e.span.offset + e.span.length,
    }
}

/// The offset of `name` in `.name` / `?.name` right after `from`.
pub(crate) fn member_name_offset(text: &str, from: usize, name: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let skip_ws = |mut i: usize| {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        i
    };
    let mut i = skip_ws(from);
    if bytes.get(i) == Some(&b'?') {
        i += 1;
    }
    if bytes.get(i) != Some(&b'.') {
        return None;
    }
    let i = skip_ws(i + 1);
    let end = i + name.len();
    let ident = |b: Option<&u8>| b.is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_');
    (text.get(i..end) == Some(name) && !ident(bytes.get(end))).then_some(i)
}

/// The `fn` keyword's offset and the name's in the declaration at `span`.
pub(crate) fn fn_name_offset(text: &str, span: &Span, name: &str) -> Option<(usize, usize)> {
    let end = (span.offset + span.length).min(text.len());
    let decl = text.get(span.offset..end)?;
    let ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut from = 0;
    while let Some(k) = decl[from..].find("fn") {
        let kw = from + k;
        let bytes = decl.as_bytes();
        let before_ok = kw == 0 || !ident(bytes[kw - 1]);
        let after = kw + 2;
        if before_ok && bytes.get(after).is_some_and(|b| b.is_ascii_whitespace()) {
            let mut i = after;
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            if decl[i..].starts_with(name) && !bytes.get(i + name.len()).is_some_and(|b| ident(*b))
            {
                return Some((span.offset + kw, span.offset + i));
            }
        }
        from = kw + 2;
    }
    None
}
