//! Every place the buffer names a method or an enum variant, for references
//! and rename (B-2026-11-19-121). The resolver binds names it can see on its
//! own (locals, functions, types); a method is bound by the typechecker
//! (`x.m()` names the method the receiver's type has) and a variant by its
//! enum, so neither had a site past its declaration.
//!
//! A site's key names the member it is: `m:Owner.name` for a method, where
//! the owner is the trait that declares it when it is a trait's method (the
//! trait's declaration, every impl's, and every call are one member) and the
//! type otherwise; `v:Enum.Variant` for a variant; and `e:Enum` for the enum
//! named as a variant's qualifier in a pattern, which the resolver does not
//! record for a unit variant (`Slot.N =>`).

use std::collections::{HashMap, HashSet};

use super::editor_nav::{expr_end, fn_name_offset, member_name_offset, Nav};
use crate::ast::{Expr, ExprKind, ImplItem, Item, Pattern, PatternKind, TraitItem, TypeKind};
use crate::span_visitor::{walk_item_mut_with, MutVisitor};
use crate::token::Span;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MemberSite {
    pub(crate) key: String,
    pub(crate) offset: usize,
    pub(crate) length: usize,
    pub(crate) declaration: bool,
    /// A method an impl of a trait the buffer does not declare (`drop` in
    /// `impl Drop for R`) must keep its name, which the trait fixes.
    pub(crate) locked: Option<String>,
}

/// The buffer's own declarations the sites are keyed against.
struct Decls {
    /// Each enum's variants.
    enums: HashMap<String, Vec<String>>,
    /// Each trait's methods.
    traits: HashMap<String, Vec<String>>,
    /// `(type, method)` → the trait whose impl for the type declares it.
    trait_impls: HashMap<(String, String), String>,
    /// `(type, method)` of an inherent impl.
    inherent: HashSet<(String, String)>,
}

impl Decls {
    /// The key of `ty.method`, a call's or a path's.
    fn method_key(&self, ty: &str, method: &str) -> String {
        if self
            .traits
            .get(ty)
            .is_some_and(|ms| ms.iter().any(|m| m == method))
        {
            return format!("m:{ty}.{method}");
        }
        if self
            .inherent
            .contains(&(ty.to_string(), method.to_string()))
        {
            return format!("m:{ty}.{method}");
        }
        if let Some(t) = self.trait_impls.get(&(ty.to_string(), method.to_string())) {
            return format!("m:{t}.{method}");
        }
        // A generic receiver (`t.go()` with `t: T`, `T: Tr`): the one trait
        // of the buffer's that declares the method.
        let owners: Vec<&String> = self
            .traits
            .iter()
            .filter(|(_, ms)| ms.iter().any(|m| m == method))
            .map(|(t, _)| t)
            .collect();
        match owners.as_slice() {
            [t] if !self.enums.contains_key(ty) => format!("m:{t}.{method}"),
            _ => format!("m:{ty}.{method}"),
        }
    }
}

fn impl_target(ib: &crate::ast::ImplBlock) -> Option<String> {
    match &ib.target_type.kind {
        TypeKind::Path(p) => p.segments.last().cloned(),
        _ => None,
    }
}

/// The last whole-word `name` in `text[span]`, as an absolute offset.
fn last_word(text: &str, span: &Span, name: &str) -> Option<usize> {
    let ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80;
    let s = text.get(span.offset..(span.offset + span.length).min(text.len()))?;
    let b = s.as_bytes();
    s.match_indices(name)
        .filter(|(i, _)| {
            (*i == 0 || !ident(b[i - 1]))
                && (i + name.len() == b.len() || !ident(b[i + name.len()]))
        })
        .map(|(i, _)| span.offset + i)
        .last()
}

/// The first whole-word `name` in `text[span]`, as an absolute offset.
fn first_word(text: &str, span: &Span, name: &str) -> Option<usize> {
    let ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80;
    let s = text.get(span.offset..(span.offset + span.length).min(text.len()))?;
    let b = s.as_bytes();
    s.match_indices(name)
        .find(|(i, _)| {
            (*i == 0 || !ident(b[i - 1]))
                && (i + name.len() == b.len() || !ident(b[i + name.len()]))
        })
        .map(|(i, _)| span.offset + i)
}

impl Nav {
    pub(crate) fn member_sites(&self) -> Vec<MemberSite> {
        let (text, user_len) = self.text();
        let text = &text[..user_len];
        let mut decls = Decls {
            enums: HashMap::new(),
            traits: HashMap::new(),
            trait_impls: HashMap::new(),
            inherent: HashSet::new(),
        };
        let mut out: Vec<MemberSite> = Vec::new();
        let push = |out: &mut Vec<MemberSite>, key: String, offset: usize, len: usize, decl| {
            out.push(MemberSite {
                key,
                offset,
                length: len,
                declaration: decl,
                locked: None,
            })
        };
        for item in self.user_items() {
            match item {
                Item::EnumDef(e) => {
                    decls.enums.insert(
                        e.name.clone(),
                        e.variants.iter().map(|v| v.name.clone()).collect(),
                    );
                    for v in &e.variants {
                        if let Some(at) = first_word(text, &v.span, &v.name) {
                            push(
                                &mut out,
                                format!("v:{}.{}", e.name, v.name),
                                at,
                                v.name.len(),
                                true,
                            );
                        }
                    }
                }
                Item::TraitDef(td) => {
                    let mut names = Vec::new();
                    for ti in &td.items {
                        let TraitItem::Method(m) = ti else { continue };
                        names.push(m.name.clone());
                        if let Some((_, at)) = fn_name_offset(text, &m.span, &m.name) {
                            push(
                                &mut out,
                                format!("m:{}.{}", td.name, m.name),
                                at,
                                m.name.len(),
                                true,
                            );
                        }
                    }
                    decls.traits.insert(td.name.clone(), names);
                }
                _ => {}
            }
        }
        for item in self.user_items() {
            let Item::ImplBlock(ib) = item else { continue };
            let Some(ty) = impl_target(ib) else { continue };
            let tr = ib
                .trait_name
                .as_ref()
                .and_then(|p| p.segments.last().cloned());
            for it in &ib.items {
                let ImplItem::Method(f) = it else { continue };
                let key = match &tr {
                    Some(t) => {
                        decls
                            .trait_impls
                            .insert((ty.clone(), f.name.clone()), t.clone());
                        format!("m:{t}.{}", f.name)
                    }
                    None => {
                        decls.inherent.insert((ty.clone(), f.name.clone()));
                        format!("m:{ty}.{}", f.name)
                    }
                };
                if let Some((_, at)) = fn_name_offset(text, &f.span, &f.name) {
                    push(&mut out, key, at, f.name.len(), true);
                    if let Some(t) = tr.as_ref().filter(|t| !decls.traits.contains_key(*t)) {
                        if let Some(last) = out.last_mut() {
                            last.locked = Some(t.clone());
                        }
                    }
                }
            }
        }

        struct Walk<'a> {
            nav: &'a Nav,
            text: &'a str,
            decls: &'a Decls,
            out: Vec<MemberSite>,
        }
        impl Walk<'_> {
            fn site(&mut self, key: String, offset: usize, length: usize) {
                self.out.push(MemberSite {
                    key,
                    offset,
                    length,
                    declaration: false,
                    locked: None,
                });
            }
            /// `Enum.Variant` (or `Type.method`) written as a path in `span`.
            fn path(&mut self, path: &[String], span: &Span, pattern: bool) {
                let [.., owner, name] = path else { return };
                if self
                    .decls
                    .enums
                    .get(owner)
                    .is_some_and(|vs| vs.iter().any(|v| v == name))
                {
                    if let Some(at) = last_word(self.text, span, name) {
                        self.site(format!("v:{owner}.{name}"), at, name.len());
                    }
                    if pattern {
                        if let Some(at) = first_word(self.text, span, owner) {
                            self.site(format!("e:{owner}"), at, owner.len());
                        }
                    }
                } else if !pattern
                    && (self.decls.inherent.contains(&(owner.clone(), name.clone()))
                        || self
                            .decls
                            .traits
                            .get(owner)
                            .is_some_and(|ms| ms.contains(name))
                        || self
                            .decls
                            .trait_impls
                            .contains_key(&(owner.clone(), name.clone())))
                {
                    if let Some(at) = last_word(self.text, span, name) {
                        let key = self.decls.method_key(owner, name);
                        self.site(key, at, name.len());
                    }
                }
            }
        }
        impl MutVisitor for Walk<'_> {
            fn span(&mut self, _: &mut Span) {}
            fn expr(&mut self, e: &mut Expr) {
                match &e.kind {
                    ExprKind::MethodCall { object, method, .. } => {
                        let typed = self.nav.typed();
                        let name_at =
                            member_name_offset(self.text, expr_end(self.text, object), method);
                        let Some(callee) = typed.node_method_callees.get(&e.id) else {
                            // `Enum.Variant(x)` read as a call on the enum's
                            // name.
                            if let (ExprKind::Identifier(owner), Some(at)) = (&object.kind, name_at)
                            {
                                let path = [owner.clone(), method.clone()];
                                let span = Span {
                                    length: at + method.len() - object.span.offset,
                                    ..object.span
                                };
                                self.path(&path, &span, false);
                            }
                            return;
                        };
                        let Some((ty, _)) = callee.rsplit_once('.') else {
                            return;
                        };
                        let ty = ty.rsplit('.').next().unwrap_or(ty);
                        let ty = ty.split('[').next().unwrap_or(ty);
                        if let Some(at) = name_at {
                            let key = self.decls.method_key(ty, method);
                            self.site(key, at, method.len());
                        }
                    }
                    ExprKind::Path { segments, .. } => {
                        let segments = segments.clone();
                        self.path(&segments, &e.span, false);
                    }
                    ExprKind::FieldAccess { object, field } => {
                        // `Enum.Unit` read as a field of the enum's name.
                        if let ExprKind::Identifier(owner) = &object.kind {
                            let path = [owner.clone(), field.clone()];
                            if let Some(at) =
                                member_name_offset(self.text, expr_end(self.text, object), field)
                            {
                                let span = Span {
                                    offset: object.span.offset,
                                    length: at + field.len() - object.span.offset,
                                    ..object.span
                                };
                                self.path(&path, &span, false);
                            }
                        }
                    }
                    ExprKind::StructLiteral { path, .. } => {
                        let path = path.clone();
                        self.path(&path, &e.span, false);
                    }
                    _ => {}
                }
            }
            fn pattern(&mut self, p: &mut Pattern) {
                let path: Vec<String> = match &p.kind {
                    PatternKind::TupleVariant { path, .. } | PatternKind::Struct { path, .. } => {
                        path.clone()
                    }
                    PatternKind::Binding(name) if name.contains('.') => {
                        name.split('.').map(str::to_string).collect()
                    }
                    _ => return,
                };
                self.path(&path, &p.span, true);
            }
        }
        let mut walk = Walk {
            nav: self,
            text,
            decls: &decls,
            out,
        };
        for item in self.user_items() {
            walk_item_mut_with(&mut item.clone(), &mut walk);
        }
        let mut out = walk.out;
        out.sort_by(|a, b| {
            (a.offset, a.length, !a.declaration).cmp(&(b.offset, b.length, !b.declaration))
        });
        out.dedup_by(|a, b| a.offset == b.offset && a.length == b.length && a.key == b.key);
        out
    }
}
