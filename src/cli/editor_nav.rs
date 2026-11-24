//! Editor navigation the resolver cannot answer on its own (B-2026-11-07-45):
//! go-to-definition on a method call's or a field access's name, which the
//! typechecker resolves, and hover on a `let` binding's own name, which is a
//! pattern rather than an expression. Both read the tables `karac check`
//! builds for the buffer, with the Kāra library appended, so a library
//! method's definition is found in the library source it was checked from.

use crate::ast::{Expr, ExprKind, ImplItem, Item, Pattern, PatternKind, TypeKind};
use crate::ids::NodeId;
use crate::resolver::SpanKey;
use crate::span_visitor::{walk_item_mut_with, MutVisitor};
use crate::token::Span;
use crate::typechecker::types::Type;

/// One buffer checked as `karac check` checks it, kept for a position query.
pub(crate) struct Nav {
    program: crate::ast::Program,
    resolved: crate::resolver::ResolveResult,
    typed: crate::typechecker::TypeCheckResult,
    effects: crate::effectchecker::EffectCheckResult,
    /// The checked text: the buffer, then (in a package) the other modules,
    /// then the library.
    text: String,
    /// Where the buffer ends in `text`.
    user_len: usize,
    /// Where the library starts in `text`.
    lib_start: usize,
    /// The package's other modules: each one's file, and where its text is
    /// in `text` (B-2026-11-17-71).
    modules: Vec<(std::path::PathBuf, usize, usize)>,
    /// The program's types the check renamed apart from the compiler's
    /// (`(new, old)`), which a type it shows names as written.
    renames: Vec<(String, String)>,
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
    /// In another module of the buffer's package, with the offsets in its
    /// file (B-2026-11-17-71).
    Module {
        file: std::path::PathBuf,
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

/// A struct field named in the buffer, as [`Nav::field_sites`] finds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FieldSite {
    /// The struct's name.
    pub(crate) ty: String,
    pub(crate) field: String,
    /// The name's span; a `ref name` pattern field, whose name is its
    /// binding's, has length 0 at the field's start.
    pub(crate) offset: usize,
    pub(crate) length: usize,
    /// `P { x }`: the field and its value or binding in one name.
    pub(crate) shorthand: bool,
}

std::thread_local! {
    /// The file the buffer being queried is, while [`with_path`] runs.
    static BUFFER_PATH: std::cell::RefCell<Option<std::path::PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

/// Run `f` with the buffer known to be the file at `path`, so every
/// [`analyze`] inside checks it as a module of its package.
pub(crate) fn with_path<R>(path: &std::path::Path, f: impl FnOnce() -> R) -> R {
    struct Reset(Option<std::path::PathBuf>);
    impl Drop for Reset {
        fn drop(&mut self) {
            BUFFER_PATH.with(|p| *p.borrow_mut() = self.0.take());
        }
    }
    let _reset = Reset(BUFFER_PATH.with(|p| p.borrow_mut().replace(path.to_path_buf())));
    f()
}

/// Check `source` for a position query; `None` when it does not parse.
/// Under [`with_path`], a module of a package is checked against the
/// package's other modules, so the names it imports resolve
/// (B-2026-11-17-71).
pub(crate) fn analyze(source: &str) -> Option<std::rc::Rc<Nav>> {
    let path = BUFFER_PATH.with(|p| p.borrow().clone());
    analyze_at(source, path.as_deref())
}

/// [`analyze`] for the buffer of the file at `path`: a module of a package
/// with other modules is checked against them; any other file alone.
pub(crate) fn analyze_at(source: &str, path: Option<&std::path::Path>) -> Option<std::rc::Rc<Nav>> {
    // Outside a package the file is checked alone, as its text says.
    let path = path
        .filter(|p| super::run_check_cmds::package_root_of_member(&p.to_string_lossy()).is_some());
    let key =
        |(p, t, _): &(Option<std::path::PathBuf>, String, _)| p.as_deref() == path && t == source;
    if let Some(hit) = RECENT.with(|r| {
        r.borrow()
            .iter()
            .find(|e| key(e))
            .map(|(_, _, nav)| nav.clone())
    }) {
        return hit;
    }
    let nav = analyze_uncached(source, path).map(std::rc::Rc::new);
    keep(path, source, nav.clone());
    nav
}

fn keep(path: Option<&std::path::Path>, source: &str, nav: Option<std::rc::Rc<Nav>>) {
    RECENT.with(|r| {
        let mut r = r.borrow_mut();
        if r.len() >= RECENT_KEPT {
            r.remove(0);
        }
        r.push((path.map(|p| p.to_path_buf()), source.to_string(), nav));
    });
}

/// Keep the full check of `source` that diagnostics just made, for the
/// position queries on the same text that follow (B-2026-11-21-90).
pub(super) fn remember(source: &str, pipeline: super::Pipeline) {
    let nav = nav_of(pipeline, source).map(std::rc::Rc::new);
    keep(None, source, nav);
}

/// How many checked buffers [`analyze_at`] keeps: a revision's hover,
/// definition and signature help share one, and completion checks a few
/// variants of it with a hole at the cursor.
const RECENT_KEPT: usize = 6;

std::thread_local! {
    /// The buffers [`analyze_at`] checked last, oldest first, by file and
    /// text. A package module's check also reads its other modules from
    /// disk, so [`forget_checked_buffers`] drops them all when anything may
    /// have changed (B-2026-11-21-90).
    #[allow(clippy::type_complexity)]
    static RECENT: std::cell::RefCell<
        Vec<(Option<std::path::PathBuf>, String, Option<std::rc::Rc<Nav>>)>,
    > = const { std::cell::RefCell::new(Vec::new()) };
}

/// Drop what [`analyze_at`] keeps: a language server calls it when a
/// document opens, changes, is saved or closes, so a check that read
/// another module's old text is not answered again.
pub(crate) fn forget_checked_buffers() {
    RECENT.with(|r| r.borrow_mut().clear());
}

fn analyze_uncached(source: &str, path: Option<&std::path::Path>) -> Option<Nav> {
    if let Some(t) = path.and_then(|p| super::build_cmds::editor_module_tables(p, source)) {
        return Some(Nav {
            program: t.program,
            resolved: t.resolved,
            typed: t.typed,
            effects: t.effects,
            text: t.text,
            user_len: t.user_len,
            lib_start: t.lib_start,
            modules: t.modules,
            renames: Vec::new(),
        });
    }
    nav_of(super::editor::checked_for_nav(source), source)
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

fn nav_of(mut pipeline: super::Pipeline, source: &str) -> Option<Nav> {
    let typed = pipeline.typed.take()?;
    let resolved = pipeline.resolved.take()?;
    let effects = pipeline.effects.take()?;
    let text = pipeline.source.take().unwrap_or_else(|| source.to_string());
    let user_len = pipeline.library_len.unwrap_or(text.len()).min(text.len());
    Some(Nav {
        program: std::mem::take(&mut pipeline.parsed.program),
        resolved,
        typed,
        effects,
        text,
        user_len,
        lib_start: user_len,
        modules: Vec::new(),
        renames: std::mem::take(&mut pipeline.renames),
    })
}

impl Nav {
    /// `ty` as the program writes it (B-2026-11-20-58).
    pub(crate) fn shown(&self, ty: &Type) -> String {
        self.unrename(crate::typechecker::types::type_display(ty))
    }

    /// [`super::unrename`] with this check's renames.
    pub(crate) fn unrename(&self, shown: String) -> String {
        super::unrename(&self.renames, shown)
    }

    pub(crate) fn resolved(&self) -> &crate::resolver::ResolveResult {
        &self.resolved
    }

    pub(crate) fn typed(&self) -> &crate::typechecker::TypeCheckResult {
        &self.typed
    }

    pub(crate) fn effects(&self) -> &crate::effectchecker::EffectCheckResult {
        &self.effects
    }

    /// The checked program: the buffer's items, then (in a package) the
    /// other modules', then the library's.
    pub(crate) fn program(&self) -> &crate::ast::Program {
        &self.program
    }

    /// Whether the buffer was checked as a module of its package, beside
    /// other modules (B-2026-11-17-71).
    pub(crate) fn in_package(&self) -> bool {
        !self.modules.is_empty()
    }

    /// The definition of the name under `at` the resolver resolved: in the
    /// buffer, another module of the package or the library.
    pub(crate) fn resolved_def(&self, at: usize) -> Option<NavSite> {
        let resolved = self.resolved();
        let mut best: Option<(usize, crate::resolver::SymbolId)> = None;
        for (key, sym) in &resolved.resolutions {
            if at < key.0 || at >= key.0.saturating_add(key.1) {
                continue;
            }
            if best.is_none_or(|(len, _)| key.1 < len) {
                best = Some((key.1, *sym));
            }
        }
        let sym = resolved.symbol_table.get_symbol(best?.1);
        let span = sym.span;
        if span.length == 0 {
            return None;
        }
        let span = self.follow_import(span, &sym.name);
        self.site(span.offset, span.length)
    }

    /// The declaration of the symbol `name` declared at `span`: a name the
    /// buffer imports is declared at its import, and its declaration is
    /// the exporting module's (B-2026-11-17-71); any other, `span`.
    pub(crate) fn follow_import(&self, span: Span, name: &str) -> Span {
        let at_import = span.offset < self.user_len
            && self.user_items().any(|item| {
                matches!(item, Item::Import(_)) && span_contains(item.span(), span.offset)
            });
        if !at_import {
            return span;
        }
        self.exported_decl(name)
            .map_or(span, |(offset, length)| Span {
                offset,
                length,
                ..span
            })
    }

    /// The name of the `pub` item `name` another module of the package
    /// declares, in the checked text.
    fn exported_decl(&self, name: &str) -> Option<(usize, usize)> {
        let ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
        self.program.items.iter().find_map(|item| {
            let (decl, is_pub) = match item {
                Item::Function(f) => (&f.name, f.is_pub),
                Item::StructDef(d) => (&d.name, d.is_pub),
                Item::EnumDef(d) => (&d.name, d.is_pub),
                Item::TraitDef(d) => (&d.name, d.is_pub),
                _ => return None,
            };
            let span = item.span();
            if decl != name
                || !is_pub
                || !self
                    .modules
                    .iter()
                    .any(|(_, base, len)| span.offset >= *base && span.offset < base + len)
            {
                return None;
            }
            // The name's first whole-word occurrence after the keyword.
            let end = (span.offset + span.length).min(self.text.len());
            let text = self.text.get(span.offset..end)?;
            let bytes = text.as_bytes();
            let mut from = 0;
            while let Some(k) = text[from..].find(name) {
                let at = from + k;
                let before = at == 0 || !ident(bytes[at - 1]);
                let after = !bytes.get(at + name.len()).is_some_and(|b| ident(*b));
                if before && after && at > 0 {
                    return Some((span.offset + at, name.len()));
                }
                from = at + name.len();
            }
            None
        })
    }

    /// The checked text (the buffer, then the library) and where the
    /// buffer ends in it.
    pub(crate) fn text(&self) -> (&str, usize) {
        (&self.text, self.user_len)
    }

    pub(crate) fn user_items(&self) -> impl Iterator<Item = &Item> {
        let user_len = self.user_len;
        self.program
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

    /// Every place the buffer names a struct's field: its declaration, a
    /// field access (`p.x`, `self.x`), a struct literal's field and a
    /// struct pattern's, each as `(struct, field)` and the name's span.
    /// A shorthand (`P { x }`) is the field and the value or binding at
    /// once, so it is marked: renaming the field writes `P { z: x }`.
    pub(crate) fn field_sites(&self) -> Vec<FieldSite> {
        struct Walk<'a> {
            nav: &'a Nav,
            text: &'a str,
            out: Vec<FieldSite>,
        }
        impl Walk<'_> {
            fn push(&mut self, ty: &str, field: &str, offset: usize, shorthand: bool) {
                self.out.push(FieldSite {
                    ty: ty.to_string(),
                    field: field.to_string(),
                    offset,
                    length: field.len(),
                    shorthand,
                });
            }
            /// The field name at the start of a field's span, `None` for
            /// `ref name` in a pattern, whose name follows the mode.
            fn name_at(&self, span: &Span, field: &str) -> Option<usize> {
                let ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80;
                let end = span.offset + field.len();
                (self.text.get(span.offset..end) == Some(field)
                    && !self.text.as_bytes().get(end).is_some_and(|b| ident(*b)))
                .then_some(span.offset)
            }
        }
        impl MutVisitor for Walk<'_> {
            fn span(&mut self, _: &mut Span) {}
            fn expr(&mut self, e: &mut Expr) {
                match &e.kind {
                    ExprKind::FieldAccess { object, field } => {
                        let typed = self.nav.typed();
                        let ty = typed
                            .node_types
                            .get(&object.id)
                            .map(|(t, _)| t)
                            .or_else(|| typed.expr_types.get(&SpanKey::from_span(&object.span)));
                        let Some(ty) = ty.and_then(type_head) else {
                            return;
                        };
                        let from = expr_end(self.text, object);
                        if let Some(off) = member_name_offset(self.text, from, field) {
                            let ty = ty.to_string();
                            self.push(&ty, field, off, false);
                        }
                    }
                    ExprKind::StructLiteral { path, fields, .. } => {
                        let Some(ty) = path.last() else { return };
                        for f in fields {
                            if let Some(off) = self.name_at(&f.span, &f.name) {
                                self.push(ty, &f.name, off, f.shorthand);
                            }
                        }
                    }
                    _ => {}
                }
            }
            fn pattern(&mut self, p: &mut Pattern) {
                let PatternKind::Struct { path, fields, .. } = &p.kind else {
                    return;
                };
                let Some(ty) = path.last() else { return };
                for f in fields {
                    match self.name_at(&f.span, &f.name) {
                        Some(off) => self.push(ty, &f.name, off, f.pattern.is_none()),
                        // `ref name`: the field is named by its binding.
                        None => self.out.push(FieldSite {
                            ty: ty.to_string(),
                            field: f.name.clone(),
                            offset: f.span.offset,
                            length: 0,
                            shorthand: true,
                        }),
                    }
                }
            }
        }
        let mut walk = Walk {
            nav: self,
            text: &self.text[..self.user_len],
            out: Vec::new(),
        };
        for item in self.user_items() {
            if let Item::StructDef(s) = item {
                for f in &s.fields {
                    walk.push(&s.name, &f.name, f.name_span.offset, false);
                }
            }
            walk_item_mut_with(&mut item.clone(), &mut walk);
        }
        let mut out = walk.out;
        out.sort_by_key(|f| (f.offset, f.length));
        out.dedup_by(|a, b| a.offset == b.offset && a.length == b.length);
        out
    }

    /// The `let` (or other pattern) binding whose name is under `at`: its
    /// type, its name's span, whether it is a `let` of a closure that writes
    /// a capture, a `MutFn` (§9.6) the type does not tell from an `Fn`
    /// (B-2026-11-20-58), and whether it is a view of a place the function
    /// does not own, a `ref` its type does not say (B-2026-11-20-60).
    pub(crate) fn binding_at(&self, at: usize) -> Option<(&Type, usize, usize, bool, bool)> {
        struct Find {
            at: usize,
            hit: Option<(NodeId, Span, String)>,
            /// `let` pattern offset → its closure value's offset.
            closure_lets: Vec<(usize, usize)>,
        }
        impl MutVisitor for Find {
            fn span(&mut self, _: &mut Span) {}
            fn pattern(&mut self, p: &mut Pattern) {
                if let PatternKind::Binding(name) = &p.kind {
                    if span_contains(&p.span, self.at)
                        && self
                            .hit
                            .as_ref()
                            .is_none_or(|(_, s, _)| p.span.length < s.length)
                    {
                        self.hit = Some((p.id, p.span, name.clone()));
                    }
                }
            }
            fn stmt(&mut self, s: &mut crate::ast::Stmt) {
                if let crate::ast::StmtKind::Let { pattern, value, .. } = &s.kind {
                    if matches!(value.kind, crate::ast::ExprKind::Closure { .. }) {
                        self.closure_lets
                            .push((pattern.span.offset, value.span.offset));
                    }
                }
            }
        }
        let mut find = Find {
            at,
            hit: None,
            closure_lets: Vec::new(),
        };
        for item in self.user_items() {
            if span_contains(item.span(), at) {
                walk_item_mut_with(&mut item.clone(), &mut find);
            }
        }
        let (id, span, name) = find.hit?;
        let (ty, _) = self.typed().node_types.get(&id)?;
        let mut_fn = find
            .closure_lets
            .iter()
            .any(|(p, c)| *p == span.offset && self.typed().mut_fn_closures.contains(c));
        let view = self.typed().core_view_lets.iter().any(|(s, n)| {
            *n == name && s.offset <= span.offset && span.offset < s.offset + s.length
        });
        Some((ty, span.offset, span.length, mut_fn, view))
    }

    /// A top-level function named at `at`, by a reference the resolver
    /// resolved or by its own declaration's name: the function's name (its
    /// effect key), its declaration's header as written, and the span of
    /// the name under the cursor. A hover shows the header, as it does a
    /// method's, because the function's `Type` cannot say `escaping` or
    /// `MutFn` and has no parameter names (B-2026-11-24-6).
    pub(crate) fn fn_decl_at(&self, at: usize) -> Option<(String, String, usize, usize)> {
        let header_of = |f: &crate::ast::Function| -> Option<(usize, usize, String)> {
            let (fn_kw, name_off) = fn_name_offset(&self.text, &f.span, &f.name)?;
            let header_end = f.body.span.offset.max(fn_kw).min(self.text.len());
            let header = self
                .text
                .get(fn_kw..header_end)?
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            Some((name_off, f.name.len(), header))
        };
        let fns = || {
            self.program.items.iter().filter_map(|item| match item {
                Item::Function(f) => Some(f),
                _ => None,
            })
        };
        // The declaration's own name.
        for f in fns() {
            if !span_contains(&f.span, at) {
                continue;
            }
            if let Some((name_off, len, header)) = header_of(f) {
                if name_off <= at && at < name_off + len {
                    return Some((f.name.clone(), header, name_off, len));
                }
            }
        }
        // A reference the resolver resolved to a top-level function.
        let resolved = self.resolved();
        let mut best: Option<(usize, usize, crate::resolver::SymbolId)> = None;
        for (key, sym) in &resolved.resolutions {
            if at < key.0 || at >= key.0.saturating_add(key.1) {
                continue;
            }
            if best.is_none_or(|(_, len, _)| key.1 < len) {
                best = Some((key.0, key.1, *sym));
            }
        }
        let (ref_off, ref_len, sym) = best?;
        let sym = resolved.symbol_table.get_symbol(sym);
        if sym.span.length == 0 {
            return None;
        }
        let decl = self.follow_import(sym.span, &sym.name);
        let f = fns().find(|f| f.name == sym.name && span_contains(&f.span, decl.offset))?;
        let (_, _, header) = header_of(f)?;
        Some((f.name.clone(), header, ref_off, ref_len))
    }

    /// The definition of `ty.method`, and the declaration's header.
    fn method_def(
        &self,
        ty: &str,
        method: &str,
        impl_at: Option<&SpanKey>,
    ) -> Option<(NavSite, String)> {
        let mut found = None;
        for item in &self.program.items {
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
        self.program
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
    pub(crate) fn site(&self, offset: usize, length: usize) -> Option<NavSite> {
        if offset < self.user_len {
            return Some(NavSite::Buffer { offset, length });
        }
        for (file, base, len) in &self.modules {
            if offset >= *base && offset + length <= base + len {
                return Some(NavSite::Module {
                    file: file.clone(),
                    offset: offset - base,
                    length,
                });
            }
        }
        let mut start = self.lib_start;
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
