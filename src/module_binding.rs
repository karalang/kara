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
//! # Not covered
//!
//! A module-qualified PATTERN path (`connection.Mode.Fast` in match position)
//! does not lex today — the parser caps pattern paths at two segments, so
//! `Mode.Fast` is already the deepest spelling reachable, with or without a
//! module binding. Pattern paths are walked here anyway so the pass is
//! complete the day that cap lifts.

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
    };
    if !binds.is_empty() {
        for item in items.iter_mut() {
            s.item(item);
        }
    }
    s.refs
}

/// The set of names a module must not have a module binding rewritten for:
/// anything it declares as an item, or binds as a value anywhere.
pub(crate) fn shadowed_names(items: &mut [Item]) -> HashSet<String> {
    let mut out = crate::import_alias::declared_names(items);
    out.extend(crate::import_alias::bound_value_names(items));
    out
}

struct Stripper<'a> {
    binds: &'a HashMap<String, Vec<String>>,
    aliases: &'a MemberAliases,
    refs: ModuleRefs,
}

impl Stripper<'_> {
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
        let Some(path) = self.binds.get(&segments[0]).cloned() else {
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
            | Item::UseDecl(_)
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
        self.block(&mut f.body);
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
            self.block(b);
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
            PatternKind::Wildcard
            | PatternKind::Binding(_)
            | PatternKind::Literal(_)
            | PatternKind::RangePattern { .. } => {}
        }
    }

    // ── Statements and blocks ────────────────────────────────────

    fn block(&mut self, b: &mut Block) {
        for stmt in &mut b.stmts {
            self.stmt(stmt);
        }
        if let Some(e) = b.final_expr.as_mut() {
            self.expr(e);
        }
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
            }
            StmtKind::LetUninit { ty, .. } => self.ty(ty),
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
        self.expr_children(e);
        if let Some(kind) = self.rooted_rewrite(e) {
            e.kind = kind;
        }
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
                let path = self.binds.get(recv)?.clone();
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
                let path = self.binds.get(recv)?.clone();
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
            | ExprKind::MultiStringLit(..)
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
                self.block(then_block);
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
                self.block(body);
            }
            ExprKind::For {
                pattern,
                iterable,
                body,
                ..
            } => {
                self.pattern(pattern);
                self.expr(iterable);
                self.block(body);
            }
            ExprKind::Match { scrutinee, arms } => {
                self.expr(scrutinee);
                for arm in arms.iter_mut() {
                    self.pattern(&mut arm.pattern);
                    if let Some(g) = arm.guard.as_mut() {
                        self.expr(g);
                    }
                    self.expr(&mut arm.body);
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
                self.expr(body);
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
            ExprKind::MapLiteral(pairs) => {
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
