//! Editor completion (v1 checklist): what can be written at a cursor.
//! After a `.` on a value, the members its type offers: its fields and the
//! methods that take `self`, the library's included and deprecated ones
//! left out. After `std.`, the library modules and their items. Anywhere
//! else, the locals and parameters in scope there and the buffer's own
//! items. Every answer reads the tables `karac check` builds; nothing runs.

use super::editor_nav::{self, Nav};
use crate::ast::Block;
use crate::ast::{
    Expr, ExprKind, ImplItem, Item, Pattern, PatternKind, Stmt, StmtKind, TraitItem, TraitMethod,
};
use crate::ids::NodeId;
use crate::resolver::SpanKey;
use crate::span_visitor::{walk_item_mut_with, MutVisitor};
use crate::token::Span;
use crate::typechecker::env::ImplInfo;
use crate::typechecker::types::{type_display, Type};

/// What a completion names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionKind {
    Method,
    Field,
    Variable,
    Function,
    Struct,
    Enum,
    Trait,
    Constant,
    TypeAlias,
    Module,
}

/// One completion: the text to insert, what it names, and a one-line
/// detail (a type or a signature) when there is one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionEntry {
    pub label: String,
    pub kind: CompletionKind,
    pub detail: Option<String>,
}

/// Stands in for the member being typed, so a buffer that ends in `x.`
/// still parses and checks.
const HOLE: &str = "__kara_complete";

/// The completions at byte `at` of `source`, sorted by label, each
/// starting with what is already typed of the name there.
pub(crate) fn complete(source: &str, at: usize) -> Vec<CompletionEntry> {
    let mut at = at.min(source.len());
    while !source.is_char_boundary(at) {
        at -= 1;
    }
    let ident = |c: char| c.is_alphanumeric() || c == '_';
    let start = source[..at]
        .char_indices()
        .rev()
        .take_while(|(_, c)| ident(*c))
        .last()
        .map_or(at, |(i, _)| i);
    let prefix = &source[start..at];
    let before = &source[..start];
    let rest = &source[at..];
    let mut out = if before.trim_end().ends_with('.') {
        let dot = before.trim_end().len() - 1;
        match std_path(&source[..dot]) {
            Some(path) => std_members(&path),
            None => members(before, rest),
        }
    } else {
        names(before, rest, start)
    };
    out.retain(|e| e.label.starts_with(prefix) && e.label != HOLE && !e.label.starts_with("__"));
    out.sort_by(|a, b| a.label.cmp(&b.label));
    out.dedup_by(|a, b| a.label == b.label);
    out
}

/// Check `before HOLE rest`, then, when that does not parse, with a `;`
/// after the hole, as a statement left unfinished needs.
fn check_with_hole(before: &str, rest: &str) -> Option<(Nav, usize)> {
    let hole_at = before.len();
    editor_nav::analyze_whole_library(&format!("{before}{HOLE}{rest}"))
        .or_else(|| editor_nav::analyze_whole_library(&format!("{before}{HOLE};{rest}")))
        .map(|nav| (nav, hole_at))
}

/// `std.a.b` as `["std", "a", "b"]` when the text before a `.` ends in a
/// path of plain names that starts at `std`.
fn std_path(text: &str) -> Option<Vec<String>> {
    let mut segs = Vec::new();
    let mut s = text.trim_end();
    loop {
        let name_start = s
            .char_indices()
            .rev()
            .take_while(|(_, c)| c.is_alphanumeric() || *c == '_')
            .last()
            .map(|(i, _)| i)?;
        segs.push(s[name_start..].to_string());
        let left = s[..name_start].trim_end();
        match left.strip_suffix('.') {
            Some(l) => s = l,
            None => {
                let ok = left
                    .chars()
                    .last()
                    .is_none_or(|c| !(c.is_alphanumeric() || c == '_'));
                segs.reverse();
                return (ok && segs[0] == "std").then_some(segs);
            }
        }
    }
}

/// The modules under `path` and, when `path` is a module, its items.
fn std_members(path: &[String]) -> Vec<CompletionEntry> {
    let mut out = Vec::new();
    let depth = path.len();
    for module in crate::prelude::importable_stdlib_module_names() {
        let segs: Vec<&str> = module.split('.').collect();
        if segs.len() > depth
            && segs[..depth]
                .iter()
                .zip(path)
                .all(|(a, b)| *a == b.as_str())
        {
            out.push(CompletionEntry {
                label: segs[depth].to_string(),
                kind: CompletionKind::Module,
                detail: Some(segs[..=depth].join(".")),
            });
        }
    }
    if let Some((have, _)) = crate::prelude::library_global_module(path) {
        for name in have {
            out.push(CompletionEntry {
                label: name.to_string(),
                kind: if name.starts_with(|c: char| c.is_uppercase()) {
                    CompletionKind::Struct
                } else {
                    CompletionKind::Function
                },
                detail: None,
            });
        }
    }
    for (p, src) in crate::prelude::GATED_STDLIB_SOURCES {
        let same = p.len() == depth && p.iter().zip(path).all(|(a, b)| *a == b.as_str());
        if !same {
            continue;
        }
        let parsed = crate::parse(src);
        for item in &parsed.program.items {
            if let Some((name, kind, true)) = item_name(item) {
                out.push(CompletionEntry {
                    label: name.to_string(),
                    kind,
                    detail: None,
                });
            }
        }
    }
    out
}

/// The fields and `self` methods of the value before the `.`.
fn members(before: &str, rest: &str) -> Vec<CompletionEntry> {
    let Some((nav, hole_at)) = check_with_hole(before, rest) else {
        return Vec::new();
    };
    let Some(ty) = receiver_type(&nav, hole_at) else {
        return Vec::new();
    };
    let Some(head) = member_head(&ty) else {
        return Vec::new();
    };
    let head = head.as_str();
    let typed = nav.typed();
    let mut out = Vec::new();
    if let Some(info) = typed.struct_info.get(head) {
        // A library struct's private fields are not the program's to name.
        let own = nav
            .user_items()
            .any(|it| matches!(it, Item::StructDef(s) if s.name == head));
        for (name, fty, is_pub) in &info.fields {
            if own || *is_pub {
                out.push(CompletionEntry {
                    label: name.clone(),
                    kind: CompletionKind::Field,
                    detail: Some(type_display(fty)),
                });
            }
        }
    }
    // An impl written for one instantiation (`impl ... for Option[Ordering]`)
    // offers its methods on that one only.
    let inner = ty_inner(&ty);
    let args: &[Type] = match &inner {
        Type::Named { args, .. } => args,
        _ => &[],
    };
    let applies = |imp: &&ImplInfo| {
        imp.target_type == head && (imp.target_args.is_empty() || imp.target_args == args)
    };
    for imp in typed.impls.iter().filter(applies) {
        for (name, sig) in &imp.methods {
            if !typed.receiver_methods.contains(&format!("{head}.{name}")) {
                continue;
            }
            let params: Vec<String> = sig
                .param_names
                .iter()
                .zip(&sig.params)
                .map(|(n, t)| match n {
                    Some(n) => format!("{n}: {}", type_display(t)),
                    None => type_display(t),
                })
                .collect();
            let ret = match &sig.return_type {
                Type::Unit => String::new(),
                t => format!(" -> {}", type_display(t)),
            };
            out.push(CompletionEntry {
                label: name.clone(),
                kind: CompletionKind::Method,
                detail: Some(format!("fn {name}({}){ret}", params.join(", "))),
            });
        }
    }
    // A trait's provided methods (an iterator's `map`, `filter`, ...)
    // belong to every type that implements it.
    let traits: Vec<&str> = typed
        .impls
        .iter()
        .filter(applies)
        .filter_map(|imp| imp.trait_name.as_deref())
        .collect();
    let (text, _) = nav.text();
    for item in &nav.program().items {
        let Item::TraitDef(td) = item else { continue };
        if !traits.contains(&td.name.as_str()) {
            continue;
        }
        for ti in &td.items {
            let TraitItem::Method(m) = ti else { continue };
            if m.self_param.is_none() || m.deprecation.is_some() {
                continue;
            }
            out.push(CompletionEntry {
                label: m.name.clone(),
                kind: CompletionKind::Method,
                detail: signature_text(text, m),
            });
        }
    }
    // The builtin types' methods are the checker's own arms, not impl
    // entries; each list is every name its arm answers.
    use crate::typechecker as tc;
    let mut builtin: Vec<&str> = Vec::new();
    match &ty_inner(&ty) {
        Type::Int(_) => {
            builtin.extend(tc::INT_METHODS);
            builtin.extend(tc::SIGNED_INT_METHODS);
        }
        Type::UInt(_) => {
            builtin.extend(tc::INT_METHODS);
            builtin.extend(tc::UNSIGNED_INT_METHODS);
        }
        Type::Float(_) => builtin.extend(tc::FLOAT_METHODS),
        _ => {}
    }
    builtin.extend(match head {
        "Vec" => tc::VEC_METHOD_NAMES,
        "String" => tc::STRING_BUILTIN_METHODS,
        "Slice" => tc::SLICE_BUILTIN_METHODS,
        "Map" => tc::MAP_BUILTIN_METHODS,
        "Set" => tc::SET_BUILTIN_METHODS,
        "SortedMap" => tc::SORTED_MAP_BUILTIN_METHODS,
        "SortedSet" => tc::SORTED_SET_BUILTIN_METHODS,
        "Option" => OPTION_METHODS,
        "Result" => RESULT_METHODS,
        _ => &[],
    });
    // Under the v2 core rules a `Vec` reaches these through `.iter()`
    // (method_iterator_agg.rs); the did-you-mean list keeps them.
    if head == "Vec" {
        builtin.retain(|n| !matches!(*n, "sum" | "product" | "max" | "min"));
    }
    for name in builtin {
        out.push(CompletionEntry {
            label: name.to_string(),
            kind: CompletionKind::Method,
            detail: None,
        });
    }
    out
}

/// `Option`'s methods (docs/library/core-types.md), most of them the
/// checker's combinator arms rather than library impls.
/// lsp/tests/completion.rs `every_method_offered_on_a_builtin_type_checks`
/// checks each.
pub(crate) const OPTION_METHODS: &[&str] = &[
    "and",
    "and_then",
    "as_ref",
    "cloned",
    "expect",
    "filter",
    "flatten",
    "get_or_insert",
    "get_or_insert_with",
    "is_none",
    "is_some",
    "is_some_and",
    "map",
    "map_or",
    "map_or_else",
    "ok_or",
    "ok_or_else",
    "or",
    "or_else",
    "replace",
    "take",
    "unwrap",
    "unwrap_or",
    "unwrap_or_default",
    "unwrap_or_else",
    "zip",
];

/// `Result`'s methods (docs/library/core-types.md), as [`OPTION_METHODS`].
pub(crate) const RESULT_METHODS: &[&str] = &[
    "and_then",
    "err",
    "expect",
    "expect_err",
    "is_err",
    "is_ok",
    "map",
    "map_err",
    "map_or",
    "map_or_else",
    "ok",
    "or_else",
    "unwrap",
    "unwrap_err",
    "unwrap_or",
    "unwrap_or_else",
];

/// A trait method's declaration as written, `fn map[...](own self, f: own
/// FnV) -> IterMap[...]`, on one line.
fn signature_text(text: &str, m: &TraitMethod) -> Option<String> {
    let start = m.span.offset;
    let end = m
        .body
        .as_ref()
        .map_or(m.span.offset + m.span.length, |b| b.span.offset);
    let src = text.get(start..end)?;
    let src = &src[src.find("fn ")?..];
    let src = src.trim_end().trim_end_matches(';');
    Some(src.split_whitespace().collect::<Vec<_>>().join(" "))
}

fn ty_inner(ty: &Type) -> Type {
    match ty {
        Type::Ref(t) | Type::MutRef(t) => ty_inner(t),
        t => t.clone(),
    }
}

/// The name the receiver's methods are listed under: a named type's own,
/// `String` for a string, `Slice` for a slice, a number's type name.
fn member_head(ty: &Type) -> Option<String> {
    match ty {
        Type::Ref(t) | Type::MutRef(t) => member_head(t),
        Type::Str => Some("String".to_string()),
        Type::Slice { .. } => Some("Slice".to_string()),
        Type::Int(_) | Type::UInt(_) | Type::Float(_) | Type::Bool | Type::Char => {
            Some(type_display(ty))
        }
        _ => editor_nav::type_head(ty).map(str::to_string),
    }
}

/// The type of the receiver of the member access whose name is the hole.
fn receiver_type(nav: &Nav, hole_at: usize) -> Option<Type> {
    struct Find<'a> {
        text: &'a str,
        hole_at: usize,
        hit: Option<(NodeId, Span)>,
    }
    impl MutVisitor for Find<'_> {
        fn span(&mut self, _: &mut Span) {}
        fn expr(&mut self, e: &mut Expr) {
            let (object, name) = match &e.kind {
                ExprKind::MethodCall { object, method, .. } => (object, method),
                ExprKind::FieldAccess { object, field } => (object, field),
                _ => return,
            };
            if name != HOLE {
                return;
            }
            let at = editor_nav::member_name_offset(
                self.text,
                editor_nav::expr_end(self.text, object),
                name,
            );
            if at == Some(self.hole_at) {
                self.hit = Some((object.id, object.span));
            }
        }
    }
    let (text, user_len) = nav.text();
    let mut find = Find {
        text: &text[..user_len],
        hole_at,
        hit: None,
    };
    for item in nav.user_items() {
        if editor_nav::span_contains(item.span(), hole_at) {
            walk_item_mut_with(&mut item.clone(), &mut find);
        }
    }
    let (id, span) = find.hit?;
    let typed = nav.typed();
    typed
        .node_types
        .get(&id)
        .map(|(t, _)| t)
        .or_else(|| typed.expr_types.get(&SpanKey::from_span(&span)))
        .cloned()
}

/// A top-level item's name and kind, and whether it is `pub`.
fn item_name(item: &Item) -> Option<(&str, CompletionKind, bool)> {
    Some(match item {
        Item::Function(f) => (f.name.as_str(), CompletionKind::Function, f.is_pub),
        Item::StructDef(s) => (s.name.as_str(), CompletionKind::Struct, s.is_pub),
        Item::EnumDef(e) => (e.name.as_str(), CompletionKind::Enum, e.is_pub),
        Item::TraitDef(t) => (t.name.as_str(), CompletionKind::Trait, t.is_pub),
        Item::ConstDecl(c) => (c.name.as_str(), CompletionKind::Constant, c.is_pub),
        Item::TypeAlias(a) => (a.name.as_str(), CompletionKind::TypeAlias, a.is_pub),
        _ => return None,
    })
}

/// The locals and parameters in scope at `at`, then the buffer's items.
fn names(before: &str, rest: &str, at: usize) -> Vec<CompletionEntry> {
    // The name being typed is left out, so it cannot fail to resolve; a
    // `0` stands in where the buffer needs an operand there (`a + |`).
    let nav = editor_nav::analyze(&format!("{before}{rest}"))
        .or_else(|| editor_nav::analyze(&format!("{before}0{rest}")))
        .or_else(|| editor_nav::analyze(&format!("{before}0;{rest}")));
    let Some(nav) = nav else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let typed = nav.typed();
    let mut scope = Scope {
        at,
        visible: Vec::new(),
        bindings: Vec::new(),
    };
    for item in nav.user_items() {
        if !editor_nav::span_contains(item.span(), at) {
            continue;
        }
        match item {
            Item::Function(f) if editor_nav::span_contains(&f.body.span, at) => {
                scope
                    .visible
                    .extend(f.params.iter().map(|p| p.pattern.span));
            }
            Item::ImplBlock(ib) => {
                for it in &ib.items {
                    if let ImplItem::Method(f) = it {
                        if editor_nav::span_contains(&f.body.span, at) {
                            scope
                                .visible
                                .extend(f.params.iter().map(|p| p.pattern.span));
                            if f.self_param.is_some() {
                                out.push(CompletionEntry {
                                    label: "self".to_string(),
                                    kind: CompletionKind::Variable,
                                    detail: None,
                                });
                            }
                        }
                    }
                }
            }
            _ => {}
        }
        walk_item_mut_with(&mut item.clone(), &mut scope);
    }
    // The innermost binding of a name shadows the others: the last one
    // declared before the cursor.
    let mut seen = std::collections::HashSet::new();
    let mut bindings = scope.bindings;
    bindings.sort_by_key(|(_, _, span)| std::cmp::Reverse(span.offset));
    for (name, id, span) in bindings {
        let inside = scope
            .visible
            .iter()
            .any(|v| span.offset >= v.offset && span.offset + span.length <= v.offset + v.length);
        if !inside || span.offset >= at || !seen.insert(name.clone()) {
            continue;
        }
        out.push(CompletionEntry {
            label: name,
            kind: CompletionKind::Variable,
            detail: typed.node_types.get(&id).map(|(t, _)| type_display(t)),
        });
    }
    for item in nav.user_items() {
        if let Some((name, kind, _)) = item_name(item) {
            if seen.contains(name) {
                continue;
            }
            let detail = match item {
                Item::Function(f) => {
                    let (text, _) = nav.text();
                    editor_nav::fn_name_offset(text, &f.span, &f.name).map(|(kw, _)| {
                        let end = f.body.span.offset.max(kw).min(text.len());
                        text[kw..end]
                            .split_whitespace()
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                }
                _ => None,
            };
            out.push(CompletionEntry {
                label: name.to_string(),
                kind,
                detail,
            });
        }
    }
    out
}

/// Which bindings are in scope at `at`: every binding pattern, and the
/// pattern spans whose bindings are visible there.
struct Scope {
    at: usize,
    /// Spans of the patterns whose bindings `at` can see.
    visible: Vec<Span>,
    /// Every binding: its name, its node and its span.
    bindings: Vec<(String, NodeId, Span)>,
}

impl Scope {
    fn sees(&self, span: &Span) -> bool {
        editor_nav::span_contains(span, self.at)
    }
}

impl MutVisitor for Scope {
    fn span(&mut self, _: &mut Span) {}

    fn block(&mut self, b: &mut Block) {
        if !self.sees(&b.span) {
            return;
        }
        for s in &b.stmts {
            if s.span.offset + s.span.length > self.at {
                break;
            }
            if let Some(p) = let_pattern(s) {
                self.visible.push(p);
            }
        }
    }

    fn expr(&mut self, e: &mut Expr) {
        match &e.kind {
            ExprKind::For { pattern, body, .. } | ExprKind::WhileLet { pattern, body, .. }
                if self.sees(&body.span) =>
            {
                self.visible.push(pattern.span);
            }
            ExprKind::IfLet {
                pattern,
                then_block,
                ..
            } if self.sees(&then_block.span) => {
                self.visible.push(pattern.span);
            }
            ExprKind::Match { arms, .. } => {
                for arm in arms {
                    if self.sees(&arm.span) {
                        self.visible.push(arm.pattern.span);
                    }
                }
            }
            ExprKind::Closure { params, body, .. } if self.sees(&body.span) => {
                self.visible.extend(params.iter().map(|p| p.pattern.span));
            }
            _ => {}
        }
    }

    fn pattern(&mut self, p: &mut Pattern) {
        let name = match &p.kind {
            PatternKind::Binding(name) | PatternKind::AtBinding { name, .. } => name,
            _ => return,
        };
        self.bindings.push((name.clone(), p.id, p.span));
    }
}

/// The pattern a `let` statement binds.
fn let_pattern(s: &Stmt) -> Option<Span> {
    match &s.kind {
        StmtKind::Let { pattern, .. } | StmtKind::LetElse { pattern, .. } => Some(pattern.span),
        _ => None,
    }
}
