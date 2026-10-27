//! Module-binding imports — `import db.connection;` — desugared to item
//! imports (B-2026-08-20-17).
//!
//! design.md § Module System states the binding rule as UNIFORM across items
//! and sub-modules: "The last segment of each imported path is bound in the
//! current scope … `import db.connection;` binds `connection` as a module
//! reference (reach `Connection` as `connection.Connection`)." The four
//! ITEM-import forms (single item, rename, brace group, `pub import`
//! re-export) all shipped; binding a MODULE bound nothing on any surface —
//! `karac build` and `karac run` both answered `undefined name 'connection',
//! did you mean 'Connection'?` (a Levenshtein hit on a struct inside the very
//! module the user named), while `karac check` passed the same program,
//! because the per-module resolver registers the module binding as a symbol
//! and then nothing consults it.
//!
//! # Why a desugar rather than a module namespace
//!
//! Every downstream pass — resolver scopes, the typechecker's environment,
//! and above all the flattened super-program that project-mode `build` and
//! `run` both compile — works in ONE namespace of bare item names. A real
//! module namespace would have to be threaded through all of them. But the
//! flattening already erases the distinction: after the concat,
//! `db.connection`'s `Connection` IS the flat unit's `Connection`. So
//! `connection.Connection` has exactly one honest lowering — bare
//! `Connection` — which is precisely what an item import already produces.
//!
//! This pass therefore rewrites `<bound>.NAME` to `NAME` throughout the
//! importing module and synthesizes `import db.connection.{NAME, …};` next to
//! the original declaration, listing exactly the names that were reached
//! through it. That buys the whole feature off machinery that already works:
//!
//!   * `E0113 UnknownItemInModule` for `connection.opne(4)` — with the
//!     module's real export list, not a struct-name guess;
//!   * `E0111` / cross-package visibility for a `private` target;
//!   * `karac check`, `karac build` and `karac run` agreeing, since all three
//!     see the same synthesized item imports.
//!
//! The original `import db.connection;` declaration is LEFT IN PLACE. It
//! already resolves (the `binds_submodule` arm of `collect_import`), and
//! keeping it preserves the module-graph edge that `collect_import_edges`
//! draws from it — so cycle detection is unaffected whether or not the
//! binding is ever used.
//!
//! # Shadow guard
//!
//! A rewrite only fires for a bound name the module never declares as an item
//! and never binds as a value ANYWHERE — the same conservative, module-wide
//! test [`crate::import_alias`] applies to aliases, and for the same reason:
//! a local `connection` makes `connection.open()` an ordinary method call,
//! and redirecting one would be a miscompile. Over-approximating only
//! declines the rewrite, which is the pre-existing behaviour.
//!
//! # Patterns
//!
//! A module-qualified pattern path (`connection.Mode.Fast =>`, a struct or
//! tuple variant, a range bound `limits.LOW..=limits.HIGH`) is stripped like
//! the expression it mirrors (B-2026-10-27-24).

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::ast::*;
use crate::token::Span;

/// The module paths reached through a module binding, each with the names
/// referenced through it and the span of the first such reference.
pub(crate) type ModuleRefs = HashMap<Vec<String>, BTreeMap<String, Span>>;

/// The local name a contested member is reached under, keyed by the module it
/// came from and the name it has there — see [`strip_module_bindings`].
pub(crate) type MemberAliases = HashMap<(Vec<String>, String), String>;

/// Rewrite every `<bound>.NAME` reference in `items` to the local name it takes
/// after the desugar: bare `NAME`, or the alias `aliases` gives it.
///
/// `binds` maps a bound module-alias name to the module path it names.
/// Returns, per module path, the names that were actually reached through it —
/// always under the name they have in the DECLARING module, which is what the
/// synthesized import has to name.
pub(crate) fn strip_module_bindings(
    items: &mut [Item],
    binds: &HashMap<String, Vec<String>>,
    aliases: &MemberAliases,
) -> ModuleRefs {
    let mut s = Stripper {
        binds,
        aliases,
        refs: ModuleRefs::new(),
        locals: Vec::new(),
    };
    if !binds.is_empty() {
        for item in items.iter_mut() {
            s.item(item);
        }
    }
    s.refs
}

/// Single-file mode: `import std.path;` binds `path` to the library module
/// (design.md § Imports), which has no module value in a single file — the
/// gated library modules are spliced in item by item. So the binding is
/// desugared exactly as project mode desugars one (see the module docs):
/// `path.parent(p)` becomes `parent(p)`, and the binding becomes
/// `import std.path.{parent};`, which the gated splice then expands
/// (B-2026-10-15-105).
///
/// Unlike project mode, which declines a binding whose name is bound as a
/// value ANYWHERE in the module, the walk here is scope-aware: a
/// `let path = ..` shadows the module only where it is in scope, which is the
/// common shape in a program that uses `std.path` at all. A member name that
/// the program also declares, binds, or imports takes an alias instead of its
/// bare name, as in project mode ([`crate::module_rename::qualified_name`]).
///
/// A binding whose own name is an item of the program is left alone, so the
/// resolver still reports it.
pub fn expand_single_file_std_module_bindings(program: &mut Program) {
    let declared = crate::import_alias::declared_names(&program.items);
    let mut binds: HashMap<String, Vec<String>> = HashMap::new();
    // Bound module path -> the declaration that bound it (directly, or as the
    // head of a chain like `web.net.f()`).
    let mut origin: HashMap<Vec<String>, usize> = HashMap::new();
    for (idx, item) in program.items.iter().enumerate() {
        let Item::Import(imp) = item else { continue };
        if imp.is_wildcard || imp.path.first().map(String::as_str) != Some("std") {
            continue;
        }
        for it in &imp.items {
            let mut full = imp.path.clone();
            full.push(it.name.clone());
            let bound = it.alias.clone().unwrap_or_else(|| it.name.clone());
            if crate::prelude::is_gated_stdlib_module(&full) && !declared.contains(&bound) {
                origin.insert(full.clone(), idx);
                binds.insert(bound, full);
            }
        }
    }
    if binds.is_empty() {
        return;
    }

    let pristine = program.items.clone();
    let no_aliases = MemberAliases::new();
    let mut refs = single_file_fixpoint(&mut program.items, &binds, &no_aliases, &mut origin);

    // A member is contested when its bare name is spoken for elsewhere.
    let mut spoken: HashSet<String> = shadowed_names(&mut program.items);
    let has_wildcard = program
        .items
        .iter()
        .any(|it| matches!(it, Item::Import(imp) if imp.is_wildcard));
    // `import std.log.{Level};` beside `import std.log;` names the same
    // item `log.Level` reaches, so it contests nothing.
    let mut same: HashSet<(Vec<String>, String)> = HashSet::new();
    for it in &program.items {
        let Item::Import(imp) = it else { continue };
        for ii in &imp.items {
            spoken.insert(ii.alias.clone().unwrap_or_else(|| ii.name.clone()));
            if ii.alias.is_none() {
                same.insert((imp.path.clone(), ii.name.clone()));
            }
        }
    }
    let mut providers: HashMap<String, usize> = HashMap::new();
    for names in refs.values() {
        for name in names.keys() {
            *providers.entry(name.clone()).or_default() += 1;
        }
    }
    let mut taken = spoken.clone();
    taken.extend(providers.keys().cloned());
    let mut aliases = MemberAliases::new();
    let mut paths: Vec<&Vec<String>> = refs.keys().collect();
    paths.sort();
    for path in paths {
        for name in refs[path].keys() {
            let contested = spoken.contains(name) && !same.contains(&(path.clone(), name.clone()));
            if has_wildcard || providers[name] > 1 || contested {
                let alias = crate::module_rename::qualified_name(name, path, &mut taken);
                aliases.insert((path.clone(), name.clone()), alias);
            }
        }
    }
    if !aliases.is_empty() {
        program.items = pristine;
        refs = single_file_fixpoint(&mut program.items, &binds, &aliases, &mut origin);
    }

    // Each binding becomes the item import of what was reached through it; a
    // binding nothing reached (unused, or shadowed at every use) binds nothing.
    let bound_names: HashSet<&String> = binds.keys().collect();
    let mut spliced: Vec<(usize, Item)> = Vec::new();
    for (path, names) in refs {
        let Some(&idx) = origin.get(&path) else {
            continue;
        };
        let Item::Import(at) = &program.items[idx] else {
            continue;
        };
        let span = at.span;
        let items = names
            .into_iter()
            .map(|(name, span)| {
                let alias = aliases.get(&(path.clone(), name.clone())).cloned();
                ImportItem { span, name, alias }
            })
            .collect();
        spliced.push((
            idx,
            Item::Import(ImportDecl {
                span,
                is_pub: false,
                is_wildcard: false,
                path_spans: vec![span; path.len()],
                path,
                items,
            }),
        ));
    }
    for item in program.items.iter_mut() {
        let Item::Import(imp) = item else { continue };
        if imp.is_wildcard || imp.path.first().map(String::as_str) != Some("std") {
            continue;
        }
        let path = imp.path.clone();
        imp.items.retain(|it| {
            let mut full = path.clone();
            full.push(it.name.clone());
            let bound = it.alias.as_ref().unwrap_or(&it.name);
            !(bound_names.contains(bound) && binds.get(bound) == Some(&full))
        });
    }
    spliced.sort_by_key(|a| std::cmp::Reverse(a.0));
    for (idx, item) in spliced {
        program.items.insert(idx + 1, item);
    }
    // A declaration left with no items is dropped by the gated splice.
}

/// [`strip_module_bindings`] to a fixpoint over the gated library modules:
/// `web.net.fetch(u)` strips to `net.fetch(u)`, which is itself a binding.
fn single_file_fixpoint(
    items: &mut [Item],
    binds: &HashMap<String, Vec<String>>,
    aliases: &MemberAliases,
    origin: &mut HashMap<Vec<String>, usize>,
) -> ModuleRefs {
    let mut refs = ModuleRefs::new();
    let mut round = binds.clone();
    while !round.is_empty() {
        let found = strip_module_bindings(items, &round, aliases);
        let mut next: HashMap<String, Vec<String>> = HashMap::new();
        for (path, names) in found {
            for (name, span) in names {
                let mut full = path.clone();
                full.push(name.clone());
                if crate::prelude::is_gated_stdlib_module(&full) && !binds.contains_key(&name) {
                    if let Some(&idx) = origin.get(&path) {
                        origin.entry(full.clone()).or_insert(idx);
                        next.insert(name, full);
                        continue;
                    }
                }
                refs.entry(path.clone())
                    .or_default()
                    .entry(name)
                    .or_insert(span);
            }
        }
        round = next;
    }
    refs
}

/// The set of names a module must not have a module binding rewritten for:
/// anything it declares as an item, or binds as a value anywhere.
pub(crate) fn shadowed_names(items: &mut [Item]) -> HashSet<String> {
    let mut out = crate::import_alias::declared_names(items);
    out.extend(crate::import_alias::bound_value_names(items));
    out
}

/// `Type.m(args)` / `Type.X` in the shape the parser gives them when written
/// bare — a call of the two-segment path — once the walk has stripped the
/// module off `<bound>.Type.m(args)`, which parsed as a method call on a field.
fn type_member(e: &mut Expr) -> Option<ExprKind> {
    let is_type = |n: &str| n.chars().next().is_some_and(char::is_uppercase);
    match &mut e.kind {
        ExprKind::MethodCall {
            object,
            method,
            turbofish,
            args,
            ..
        } => {
            let ExprKind::Identifier(ty) = &object.kind else {
                return None;
            };
            if !is_type(ty) {
                return None;
            }
            let segments = vec![ty.clone(), method.clone()];
            let generic_args = turbofish
                .take()
                .map(|tf| tf.into_iter().map(GenericArg::Type).collect());
            Some(ExprKind::Call {
                callee: Box::new(Expr {
                    id: crate::ids::NodeId::DUMMY,
                    kind: ExprKind::Path {
                        segments,
                        generic_args,
                    },
                    span: object.span,
                }),
                args: std::mem::take(args),
            })
        }
        ExprKind::FieldAccess { object, field } => {
            let ExprKind::Identifier(ty) = &object.kind else {
                return None;
            };
            if !is_type(ty) {
                return None;
            }
            Some(ExprKind::Path {
                segments: vec![ty.clone(), field.clone()],
                generic_args: None,
            })
        }
        _ => None,
    }
}

struct Stripper<'a> {
    binds: &'a HashMap<String, Vec<String>>,
    aliases: &'a MemberAliases,
    refs: ModuleRefs,
    /// The value names bound by the scopes enclosing the walk, innermost
    /// last. A `<bound>.x` whose root is one of them reaches a local, not the
    /// module. Project mode never needs it (its bindings are already dropped
    /// for any name bound as a value anywhere, see [`shadowed_names`]); the
    /// single-file desugar ([`expand_single_file_std_module_bindings`]) does.
    locals: Vec<Vec<String>>,
}

impl Stripper<'_> {
    /// The module path `name` is bound to, unless a local shadows it here.
    fn bound_module(&self, name: &str) -> Option<Vec<String>> {
        if self
            .locals
            .iter()
            .any(|scope| scope.iter().any(|n| n == name))
        {
            return None;
        }
        self.binds.get(name).cloned()
    }

    fn push_scope(&mut self, names: Vec<String>) {
        self.locals.push(names);
    }

    fn pop_scope(&mut self) {
        self.locals.pop();
    }

    /// Bind `names` in the innermost scope, for a `let` that binds them from
    /// its statement onward.
    fn bind_here(&mut self, names: Vec<String>) {
        match self.locals.last_mut() {
            Some(scope) => scope.extend(names),
            None => self.locals.push(names),
        }
    }

    /// Record that `name` was reached through the module at `path`.
    fn note(&mut self, path: &[String], name: &str, span: Span) {
        self.refs
            .entry(path.to_vec())
            .or_default()
            .entry(name.to_string())
            .or_insert(span);
    }

    /// Record the reference and answer with the local name it becomes — bare
    /// `name`, or the alias minted for it because the bare name is contested.
    fn resolved(&mut self, path: &[String], name: &str, span: Span) -> String {
        self.note(path, name, span);
        self.aliases
            .get(&(path.to_vec(), name.to_string()))
            .cloned()
            .unwrap_or_else(|| name.to_string())
    }

    /// Drop a leading segment naming a bound module: `["connection",
    /// "Connection"]` → `["Connection"]`. A single-segment path is left
    /// alone — a bare `connection` names no item.
    fn segments(&mut self, segments: &mut Vec<String>, span: Span) {
        if segments.len() < 2 {
            return;
        }
        let Some(path) = self.bound_module(&segments[0]) else {
            return;
        };
        segments.remove(0);
        let bare = segments[0].clone();
        segments[0] = self.resolved(&path, &bare, span);
    }

    // ── Items ────────────────────────────────────────────────────

    fn item(&mut self, item: &mut Item) {
        match item {
            Item::Function(f) => self.function(f),
            Item::StructDef(s) => {
                self.generics(s.generic_params.as_mut());
                self.where_clause(s.where_clause.as_mut());
                for field in &mut s.fields {
                    self.ty(&mut field.ty);
                }
                for inv in s.invariants.iter_mut().chain(s.impl_invariants.iter_mut()) {
                    self.expr(inv);
                }
            }
            Item::UnionDef(u) => {
                for field in &mut u.fields {
                    self.ty(&mut field.ty);
                }
            }
            Item::EnumDef(e) => {
                self.generics(e.generic_params.as_mut());
                self.where_clause(e.where_clause.as_mut());
                for v in &mut e.variants {
                    match &mut v.kind {
                        VariantKind::Unit => {}
                        VariantKind::Tuple(tys) => {
                            for t in tys.iter_mut() {
                                self.ty(t);
                            }
                        }
                        VariantKind::Struct(fields) => {
                            for f in fields.iter_mut() {
                                self.ty(&mut f.ty);
                            }
                        }
                    }
                    if let Some(d) = v.discriminant.as_mut() {
                        self.expr(d);
                    }
                }
            }
            Item::TraitDef(t) => {
                self.generics(t.generic_params.as_mut());
                self.where_clause(t.where_clause.as_mut());
                for sup in &mut t.supertraits {
                    self.bound(sup);
                }
                for ti in &mut t.items {
                    if let TraitItem::Method(m) = ti {
                        self.trait_method(m);
                    }
                }
            }
            Item::MarkerTrait(t) => {
                self.generics(t.generic_params.as_mut());
                for sup in &mut t.supertraits {
                    self.bound(sup);
                }
            }
            Item::TraitAlias(t) => {
                self.generics(t.generic_params.as_mut());
                for b in &mut t.bounds {
                    self.bound(b);
                }
            }
            Item::ImplBlock(b) => {
                self.generics(b.generic_params.as_mut());
                self.where_clause(b.where_clause.as_mut());
                if let Some(tn) = b.trait_name.as_mut() {
                    let span = tn.span;
                    self.segments(&mut tn.segments, span);
                }
                self.ty(&mut b.target_type);
                for ii in &mut b.items {
                    match ii {
                        ImplItem::Method(m) => self.function(m),
                        ImplItem::AssocType(a) => self.ty(&mut a.ty),
                    }
                }
            }
            Item::ConstDecl(c) => {
                self.ty(&mut c.ty);
                self.expr(&mut c.value);
            }
            Item::ModuleBinding(m) => {
                if let Some(t) = m.ty.as_mut() {
                    self.ty(t);
                }
                self.expr(&mut m.value);
            }
            Item::TypeAlias(a) => {
                self.generics(a.generic_params.as_mut());
                self.ty(&mut a.ty);
                if let Some(r) = a.refinement.as_mut() {
                    self.expr(r);
                }
            }
            Item::TestCase(t) => self.block(&mut t.body),
            // No type or path a module binding can reach.
            Item::EffectResource(_)
            | Item::EffectGroup(_)
            | Item::EffectVerbDecl(_)
            | Item::LayoutDef(_)
            | Item::Import(_)
            | Item::AliasDecl(_)
            | Item::IndependentDecl(_)
            | Item::ExternFunction(_)
            | Item::ExternBlock(_)
            | Item::DistinctType(_) => {}
        }
    }

    fn function(&mut self, f: &mut Function) {
        self.generics(f.generic_params.as_mut());
        self.where_clause(f.where_clause.as_mut());
        for p in &mut f.params {
            self.ty(&mut p.ty);
        }
        if let Some(rt) = f.return_type.as_mut() {
            self.ty(rt);
        }
        let params = f.params.iter().flat_map(|p| p.pattern.binding_names());
        self.push_scope(params.collect());
        self.block(&mut f.body);
        self.pop_scope();
    }

    fn trait_method(&mut self, m: &mut TraitMethod) {
        self.generics(m.generic_params.as_mut());
        self.where_clause(m.where_clause.as_mut());
        for p in &mut m.params {
            self.ty(&mut p.ty);
        }
        if let Some(rt) = m.return_type.as_mut() {
            self.ty(rt);
        }
        if let Some(b) = m.body.as_mut() {
            let params = m.params.iter().flat_map(|p| p.pattern.binding_names());
            self.push_scope(params.collect());
            self.block(b);
            self.pop_scope();
        }
    }

    fn generics(&mut self, g: Option<&mut GenericParams>) {
        let Some(g) = g else { return };
        for gp in &mut g.params {
            for b in &mut gp.bounds {
                self.bound(b);
            }
            if let Some(ct) = gp.const_type.as_mut() {
                self.ty(ct);
            }
        }
    }

    fn where_clause(&mut self, w: Option<&mut WhereClause>) {
        let Some(w) = w else { return };
        for c in &mut w.constraints {
            match c {
                WhereConstraint::TypeBound { bounds, .. } => {
                    for b in bounds.iter_mut() {
                        self.bound(b);
                    }
                }
                WhereConstraint::ProjectionBound {
                    projection, bounds, ..
                } => {
                    self.ty(projection);
                    for b in bounds.iter_mut() {
                        self.bound(b);
                    }
                }
                WhereConstraint::AssocTypeEq { ty, .. } => self.ty(ty),
                WhereConstraint::ConstPredicate { .. } => {}
            }
        }
    }

    fn bound(&mut self, b: &mut TraitBound) {
        let span = b.span;
        self.segments(&mut b.path, span);
        if let Some(args) = b.generic_args.as_mut() {
            for a in args.iter_mut() {
                self.generic_arg(a);
            }
        }
        // Inline associated-type bindings are still on the bound here: this
        // pass runs during module loading, BEFORE `desugar_program` hoists
        // them onto the where clause. Their RHS can name an imported type
        // (`I: Src[Item = other.Thing]`), so it needs the same rewrite.
        for binding in b.assoc_bindings.iter_mut() {
            self.ty(&mut binding.ty);
        }
    }

    fn generic_arg(&mut self, a: &mut GenericArg) {
        match a {
            GenericArg::Type(t) => self.ty(t),
            GenericArg::Const(e) => self.expr(e),
            _ => {}
        }
    }

    // ── Types ────────────────────────────────────────────────────

    fn ty(&mut self, t: &mut TypeExpr) {
        match &mut t.kind {
            TypeKind::Path(p) => {
                let span = p.span;
                self.segments(&mut p.segments, span);
                if let Some(args) = p.generic_args.as_mut() {
                    for a in args.iter_mut() {
                        self.generic_arg(a);
                    }
                }
            }
            TypeKind::Tuple(elems) => {
                for e in elems.iter_mut() {
                    self.ty(e);
                }
            }
            TypeKind::Array { element, size } => {
                self.ty(element);
                self.expr(size);
            }
            TypeKind::Pointer { inner, .. }
            | TypeKind::Ref(inner)
            | TypeKind::MutRef(inner)
            | TypeKind::Frozen(inner)
            | TypeKind::MutSlice(inner)
            | TypeKind::Weak(inner) => self.ty(inner),
            TypeKind::FnType {
                params,
                return_type,
                ..
            } => {
                for p in params.iter_mut() {
                    self.ty(p);
                }
                if let Some(r) = return_type.as_mut() {
                    self.ty(r);
                }
            }
            TypeKind::ImplTrait {
                trait_path,
                args,
                assoc_bindings,
                ..
            }
            | TypeKind::Dyn {
                trait_path,
                args,
                assoc_bindings,
                ..
            } => {
                let span = trait_path.span;
                self.segments(&mut trait_path.segments, span);
                for a in args.iter_mut() {
                    self.generic_arg(a);
                }
                // An inline binding's RHS is an ordinary type and can name an
                // imported one — `impl Src[Item = other.Thing]`. Rewriting the
                // path list without it would leave that reference unbound.
                for b in assoc_bindings.iter_mut() {
                    self.ty(&mut b.ty);
                }
            }
            TypeKind::Unit | TypeKind::Error => {}
        }
    }

    // ── Patterns ─────────────────────────────────────────────────

    fn pattern(&mut self, p: &mut Pattern) {
        let span = p.span;
        match &mut p.kind {
            PatternKind::Struct { path, fields, .. } => {
                self.segments(path, span);
                for f in fields.iter_mut() {
                    if let Some(sub) = f.pattern.as_mut() {
                        self.pattern(sub);
                    }
                }
            }
            PatternKind::TupleVariant { path, patterns } => {
                self.segments(path, span);
                for sub in patterns.iter_mut() {
                    self.pattern(sub);
                }
            }
            PatternKind::Tuple(items) | PatternKind::Or(items) => {
                for sub in items.iter_mut() {
                    self.pattern(sub);
                }
            }
            PatternKind::Slice { prefix, suffix, .. } => {
                for sub in prefix.iter_mut().chain(suffix.iter_mut()) {
                    self.pattern(sub);
                }
            }
            PatternKind::AtBinding { pattern, .. } => self.pattern(pattern),
            // A qualified unit variant or const is stored joined
            // (`log.Level.Debug`); a plain binding has no `.`.
            PatternKind::Binding(name) if name.contains('.') => {
                let mut segments: Vec<String> = name.split('.').map(str::to_string).collect();
                self.segments(&mut segments, span);
                *name = segments.join(".");
            }
            PatternKind::RangePattern { start, end, .. } => {
                for bound in [start, end].into_iter().flatten() {
                    if let RangeBound::Path { segments, span } = bound {
                        let span = *span;
                        self.segments(segments, span);
                    }
                }
            }
            PatternKind::Wildcard | PatternKind::Binding(_) | PatternKind::Literal(_) => {}
        }
    }

    // ── Statements and blocks ────────────────────────────────────

    fn block(&mut self, b: &mut Block) {
        self.push_scope(Vec::new());
        for stmt in &mut b.stmts {
            self.stmt(stmt);
        }
        if let Some(e) = b.final_expr.as_mut() {
            self.expr(e);
        }
        self.pop_scope();
    }

    fn stmt(&mut self, stmt: &mut Stmt) {
        match &mut stmt.kind {
            StmtKind::Let {
                pattern, ty, value, ..
            } => {
                self.pattern(pattern);
                if let Some(t) = ty.as_mut() {
                    self.ty(t);
                }
                self.expr(value);
                self.bind_here(pattern.binding_names());
            }
            StmtKind::LetUninit { name, ty, .. } => {
                self.ty(ty);
                self.bind_here(vec![name.clone()]);
            }
            StmtKind::LetElse {
                pattern,
                ty,
                value,
                else_block,
                ..
            } => {
                self.pattern(pattern);
                if let Some(t) = ty.as_mut() {
                    self.ty(t);
                }
                self.expr(value);
                self.block(else_block);
                self.bind_here(pattern.binding_names());
            }
            StmtKind::Defer { body } => self.block(body),
            StmtKind::ErrDefer { body, .. } => self.block(body),
            StmtKind::Assign { target, value } | StmtKind::CompoundAssign { target, value, .. } => {
                self.expr(target);
                self.expr(value);
            }
            StmtKind::MultiAssign { targets, values } => {
                for e in targets.iter_mut().chain(values.iter_mut()) {
                    self.expr(e);
                }
            }
            StmtKind::Expr(e) => self.expr(e),
        }
    }

    // ── Expressions ──────────────────────────────────────────────

    fn expr(&mut self, e: &mut Expr) {
        // Children FIRST, then the rewrite at this node. A chain reaches the
        // binding only through its innermost link, so `db.connection.open(5)`
        // needs `db.connection` collapsed to `connection` before the outer
        // method call can see a module-rooted receiver. Walking the receiver
        // first costs nothing — a bare `Identifier` has no children and is
        // never rewritten on its own.
        let type_rooted = self.reaches_through_binding(e);
        self.expr_children(e);
        if let Some(kind) = self.rooted_rewrite(e) {
            e.kind = kind;
        } else if type_rooted {
            if let Some(kind) = type_member(e) {
                e.kind = kind;
            }
        }
    }

    /// `<bound>.Type.m(..)` / `<bound>.Type.X`: a member of a type reached
    /// through a binding, which the walk turns into `Type.m(..)` / `Type.X`.
    fn reaches_through_binding(&self, e: &Expr) -> bool {
        let (ExprKind::MethodCall { object, .. } | ExprKind::FieldAccess { object, .. }) = &e.kind
        else {
            return false;
        };
        let ExprKind::FieldAccess { object: root, .. } = &object.kind else {
            return false;
        };
        matches!(&root.kind, ExprKind::Identifier(r) if self.bound_module(r).is_some())
    }

    /// `<bound>.NAME` and `<bound>.NAME(args)` → `NAME` / `NAME(args)`.
    /// `None` when this node is not rooted at a module binding.
    fn rooted_rewrite(&mut self, e: &mut Expr) -> Option<ExprKind> {
        match &mut e.kind {
            ExprKind::MethodCall {
                object,
                method,
                turbofish,
                args,
                ..
            } => {
                let ExprKind::Identifier(recv) = &object.kind else {
                    return None;
                };
                let path = self.bound_module(recv)?;
                let local = self.resolved(&path, method, object.span);
                let callee_kind = match turbofish.take() {
                    Some(tf) => ExprKind::Path {
                        segments: vec![local],
                        generic_args: Some(tf.into_iter().map(GenericArg::Type).collect()),
                    },
                    None => ExprKind::Identifier(local),
                };
                Some(ExprKind::Call {
                    callee: Box::new(Expr {
                        id: crate::ids::NodeId::DUMMY,
                        kind: callee_kind,
                        span: object.span,
                    }),
                    args: std::mem::take(args),
                })
            }
            ExprKind::FieldAccess { object, field } => {
                let ExprKind::Identifier(recv) = &object.kind else {
                    return None;
                };
                let path = self.bound_module(recv)?;
                let local = self.resolved(&path, field, object.span);
                Some(ExprKind::Identifier(local))
            }
            _ => None,
        }
    }

    fn expr_children(&mut self, e: &mut Expr) {
        let span = e.span;
        match &mut e.kind {
            ExprKind::Integer(..)
            | ExprKind::Float(..)
            | ExprKind::CharLit(..)
            | ExprKind::ByteLit(..)
            | ExprKind::ByteStringLit(..)
            | ExprKind::StringLit(..)
            | ExprKind::CStringLit { .. }
            | ExprKind::Bool(..)
            | ExprKind::Identifier(..)
            | ExprKind::SelfValue
            | ExprKind::SelfType
            | ExprKind::PipePlaceholder
            | ExprKind::Continue { .. }
            | ExprKind::Error => {}
            ExprKind::Path {
                segments,
                generic_args,
            } => {
                self.segments(segments, span);
                if let Some(args) = generic_args.as_mut() {
                    for a in args.iter_mut() {
                        self.generic_arg(a);
                    }
                }
            }
            ExprKind::OffsetOf { ty, .. } => self.ty(ty),
            ExprKind::InterpolatedStringLit(parts) => {
                for p in parts.iter_mut() {
                    if let ParsedInterpolationPart::Expr(inner, _) = p {
                        self.expr(inner);
                    }
                }
            }
            ExprKind::Block(b)
            | ExprKind::Comptime(b)
            | ExprKind::Par(b)
            | ExprKind::Seq(b)
            | ExprKind::Try(b)
            | ExprKind::Unsafe(b)
            | ExprKind::LabeledBlock { body: b, .. }
            | ExprKind::Loop { body: b, .. } => self.block(b),
            ExprKind::Lock { mutex, body, .. } => {
                self.expr(mutex);
                self.block(body);
            }
            ExprKind::If {
                condition,
                then_block,
                else_branch,
            } => {
                self.expr(condition);
                self.block(then_block);
                if let Some(x) = else_branch.as_mut() {
                    self.expr(x);
                }
            }
            ExprKind::IfLet {
                pattern,
                value,
                then_block,
                else_branch,
                ..
            } => {
                self.pattern(pattern);
                self.expr(value);
                self.push_scope(pattern.binding_names());
                self.block(then_block);
                self.pop_scope();
                if let Some(x) = else_branch.as_mut() {
                    self.expr(x);
                }
            }
            ExprKind::While {
                condition, body, ..
            } => {
                self.expr(condition);
                self.block(body);
            }
            ExprKind::WhileLet {
                pattern,
                value,
                body,
                ..
            } => {
                self.pattern(pattern);
                self.expr(value);
                self.push_scope(pattern.binding_names());
                self.block(body);
                self.pop_scope();
            }
            ExprKind::For {
                pattern,
                iterable,
                body,
                ..
            } => {
                self.pattern(pattern);
                self.expr(iterable);
                self.push_scope(pattern.binding_names());
                self.block(body);
                self.pop_scope();
            }
            ExprKind::Match { scrutinee, arms } => {
                self.expr(scrutinee);
                for arm in arms.iter_mut() {
                    self.pattern(&mut arm.pattern);
                    self.push_scope(arm.pattern.binding_names());
                    if let Some(g) = arm.guard.as_mut() {
                        self.expr(g);
                    }
                    self.expr(&mut arm.body);
                    self.pop_scope();
                }
            }
            ExprKind::MethodCall {
                object,
                turbofish,
                args,
                ..
            } => {
                self.expr(object);
                if let Some(tf) = turbofish.as_mut() {
                    for t in tf.iter_mut() {
                        self.ty(t);
                    }
                }
                for a in args.iter_mut() {
                    self.expr(&mut a.value);
                }
            }
            ExprKind::Call { callee, args } => {
                self.expr(callee);
                for a in args.iter_mut() {
                    self.expr(&mut a.value);
                }
            }
            ExprKind::OptionalChain { object, args, .. } => {
                self.expr(object);
                if let Some(args) = args.as_mut() {
                    for a in args.iter_mut() {
                        self.expr(&mut a.value);
                    }
                }
            }
            ExprKind::Index { object, index } => {
                self.expr(object);
                self.expr(index);
            }
            ExprKind::Binary { left, right, .. }
            | ExprKind::NilCoalesce { left, right }
            | ExprKind::Pipe { left, right } => {
                self.expr(left);
                self.expr(right);
            }
            ExprKind::Unary { operand, .. } => self.expr(operand),
            ExprKind::Question(inner) => self.expr(inner),
            ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
                self.expr(object)
            }
            ExprKind::Cast { expr: inner, ty } => {
                self.expr(inner);
                self.ty(ty);
            }
            ExprKind::Closure { params, body, .. } => {
                for p in params.iter_mut() {
                    self.pattern(&mut p.pattern);
                    if let Some(t) = p.ty.as_mut() {
                        self.ty(t);
                    }
                }
                let names = params.iter().flat_map(|p| p.pattern.binding_names());
                self.push_scope(names.collect());
                self.expr(body);
                self.pop_scope();
            }
            ExprKind::Return(opt) | ExprKind::Break { value: opt, .. } => {
                if let Some(inner) = opt.as_mut() {
                    self.expr(inner);
                }
            }
            ExprKind::Tuple(items)
            | ExprKind::ArrayLiteral(items)
            | ExprKind::PrefixCollectionLiteral { items, .. } => {
                for x in items.iter_mut() {
                    self.expr(x);
                }
            }
            ExprKind::RepeatLiteral { value, count, .. } => {
                self.expr(value);
                self.expr(count);
            }
            ExprKind::MapLiteral { entries: pairs, .. } => {
                for (k, v) in pairs.iter_mut() {
                    self.expr(k);
                    self.expr(v);
                }
            }
            ExprKind::StructLiteral {
                path,
                fields,
                spread,
                ..
            } => {
                self.segments(path, span);
                for f in fields.iter_mut() {
                    self.expr(&mut f.value);
                }
                if let Some(s) = spread.as_mut() {
                    self.expr(s);
                }
            }
            ExprKind::Range { start, end, .. } => {
                if let Some(x) = start.as_mut() {
                    self.expr(x);
                }
                if let Some(x) = end.as_mut() {
                    self.expr(x);
                }
            }
            ExprKind::Providers { bindings, body } => {
                for b in bindings.iter_mut() {
                    self.expr(&mut b.value);
                }
                self.block(body);
            }
        }
    }
}
