//! Import-alias canonicalization for the flattened multi-file unit.
//!
//! `run_multi_file_codegen` concatenates every module's items into one
//! super-program and DROPS the `import` declarations — their effect was
//! already resolved by the per-module passes. That is correct for a plain
//! import: the imported item is physically present in the flat unit under
//! its own name, so a reference to it still binds.
//!
//! It is NOT correct for an ALIASED import. `import doer.{Impl as Widget};`
//! makes `Widget` a name that exists only in the import declaration, so once
//! that declaration is dropped, nothing in the flat unit declares `Widget` —
//! every reference dangles. The flat passes then reject a program the
//! tree-aware per-module passes accepted (B-2026-07-29-14), in three
//! different places depending on what was aliased:
//!
//!   * aliased STRUCT  — `resolve`: `undefined name 'Widget'`
//!   * aliased TRAIT   — `typecheck`: `unknown trait 'D' in inline bound`
//!   * both            — `E0200`: `Widget` does not implement `D`
//!
//! This module rewrites each module's references from the alias to the
//! canonical name before the module's items are appended, which is the
//! honest lowering: after flattening, an alias has no declaration left to
//! point at.
//!
//! Scope and blast radius: [`alias_subst_for_module`] returns an EMPTY map
//! for a module with no aliased imports, and [`rewrite_item`] returns
//! immediately on an empty map. Every program that does not use
//! `import … as …` is therefore byte-identical to before — and every
//! program that does was rejected outright.
//!
//! The walk reuses `desugar`'s substitution helpers (written for trait
//! type-param substitution, and exhaustive over `ExprKind` / `StmtKind` /
//! `TypeKind`) and adds the three slots an alias needs that a type param
//! does not: trait-bound NAMES, `impl Trait for` names, and pattern paths.
//!
//! # Why `dead_code` is allowed without the `llvm` feature
//!
//! Everything here except [`walk_expr_children`] exists to serve
//! `run_multi_file_codegen`, which is `#[cfg(feature = "llvm")]` — there is no
//! flattened multi-file unit to canonicalize when the compiler has no codegen
//! backend, so all 19 rewrite/collect entry points are genuinely unreachable in
//! that cfg. `walk_expr_children` is the exception: `lowering.rs` calls it
//! unconditionally to avoid keeping a third copy of the `ExprKind` recursion,
//! so the module itself cannot be feature-gated.
//!
//! This matters because CI's lint gate is `cargo clippy --all -- -D warnings`
//! with no `--features llvm` (see CLAUDE.md § Commands), so the non-llvm cfg is
//! the one that must stay clean — a bare `--features llvm` clippy run passes
//! here and hides the problem.

#![cfg_attr(not(feature = "llvm"), allow(dead_code))]

use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::desugar::{
    subst_block, subst_expr, subst_leading_type_name, subst_trait_bound, subst_type_expr,
    subst_where_constraint,
};
/// Build the `alias -> canonical name` substitution for one module.
///
/// `local_names` is the set of item names the module declares itself. An
/// alias that collides with a local declaration is SKIPPED: the local
/// declaration is what the module's own references bind to, and rewriting
/// them to the import's target would silently redirect them. (The resolver
/// reports the collision on its own terms; this pass must not paper over
/// it.)
pub(crate) fn alias_subst_for_module(
    imports: &[ImportDecl],
    local_names: &HashSet<String>,
    bound_values: &HashSet<String>,
) -> HashMap<String, TypeExpr> {
    let mut subst = HashMap::new();
    for imp in imports {
        for item in &imp.items {
            let Some(alias) = &item.alias else { continue };
            if alias == &item.name || local_names.contains(alias) || bound_values.contains(alias) {
                continue;
            }
            subst.insert(
                alias.clone(),
                TypeExpr {
                    kind: TypeKind::Path(PathExpr {
                        segments: vec![item.name.clone()],
                        generic_args: None,
                        span: item.span.clone(),
                    }),
                    span: item.span.clone(),
                },
            );
        }
    }
    subst
}

/// Every name `items` binds as a VALUE anywhere — parameters, `let`
/// patterns, closure params, match / if-let / while-let / for patterns.
///
/// The second half of [`alias_subst_for_module`]'s shadow guard, and what
/// makes rewriting a bare `Identifier` safe: if the module never binds the
/// alias name as a value, an identifier spelling it cannot be a local.
///
/// Conservative on purpose — a binding ANYWHERE in the module disables that
/// alias's rewrite for the whole module. Over-approximating costs nothing
/// (the program was rejected outright before) and can never redirect a
/// variable reference, which is the failure mode that matters.
///
/// Takes `&mut` only to reuse the mutable child walk; it mutates nothing.
pub(crate) fn bound_value_names(items: &mut [Item]) -> HashSet<String> {
    let mut out = HashSet::new();
    for item in items {
        match item {
            Item::Function(f) => collect_fn_bindings(f, &mut out),
            Item::ImplBlock(b) => {
                for ii in &mut b.items {
                    if let ImplItem::Method(m) = ii {
                        collect_fn_bindings(m, &mut out);
                    }
                }
            }
            Item::TraitDef(t) => {
                for ti in &mut t.items {
                    if let TraitItem::Method(m) = ti {
                        for p in &m.params {
                            out.extend(p.pattern.binding_names());
                        }
                        if let Some(b) = m.body.as_mut() {
                            collect_block_bindings(b, &mut out);
                        }
                    }
                }
            }
            Item::TestCase(t) => collect_block_bindings(&mut t.body, &mut out),
            Item::ConstDecl(c) => {
                out.insert(c.name.clone());
                collect_expr_bindings(&mut c.value, &mut out);
            }
            Item::ModuleBinding(m) => {
                out.insert(m.name.clone());
                collect_expr_bindings(&mut m.value, &mut out);
            }
            _ => {}
        }
    }
    out
}

fn collect_fn_bindings(f: &mut Function, out: &mut HashSet<String>) {
    for p in &f.params {
        out.extend(p.pattern.binding_names());
    }
    collect_block_bindings(&mut f.body, out);
}

fn collect_block_bindings(b: &mut Block, out: &mut HashSet<String>) {
    for stmt in &mut b.stmts {
        match &mut stmt.kind {
            StmtKind::Let { pattern, value, .. } => {
                out.extend(pattern.binding_names());
                collect_expr_bindings(value, out);
            }
            StmtKind::LetElse {
                pattern,
                value,
                else_block,
                ..
            } => {
                out.extend(pattern.binding_names());
                collect_expr_bindings(value, out);
                collect_block_bindings(else_block, out);
            }
            StmtKind::LetUninit { name, .. } => {
                out.insert(name.clone());
            }
            StmtKind::Expr(e) => collect_expr_bindings(e, out),
            StmtKind::Assign { target, value, .. }
            | StmtKind::CompoundAssign { target, value, .. } => {
                collect_expr_bindings(target, out);
                collect_expr_bindings(value, out);
            }
            StmtKind::MultiAssign {
                targets, values, ..
            } => {
                for e in targets.iter_mut().chain(values.iter_mut()) {
                    collect_expr_bindings(e, out);
                }
            }
            StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
                collect_block_bindings(body, out)
            }
        }
    }
    if let Some(e) = b.final_expr.as_mut() {
        collect_expr_bindings(e, out);
    }
}

fn collect_expr_bindings(e: &mut Expr, out: &mut HashSet<String>) {
    match &mut e.kind {
        ExprKind::Match { scrutinee, arms } => {
            collect_expr_bindings(scrutinee, out);
            for arm in arms.iter_mut() {
                out.extend(arm.pattern.binding_names());
                if let Some(g) = arm.guard.as_mut() {
                    collect_expr_bindings(g, out);
                }
                collect_expr_bindings(&mut arm.body, out);
            }
        }
        ExprKind::IfLet {
            pattern,
            value,
            then_block,
            else_branch,
        } => {
            out.extend(pattern.binding_names());
            collect_expr_bindings(value, out);
            collect_block_bindings(then_block, out);
            if let Some(b) = else_branch.as_mut() {
                collect_expr_bindings(b, out);
            }
        }
        ExprKind::WhileLet {
            pattern,
            value,
            body,
            ..
        } => {
            out.extend(pattern.binding_names());
            collect_expr_bindings(value, out);
            collect_block_bindings(body, out);
        }
        ExprKind::For {
            pattern,
            iterable,
            body,
            ..
        } => {
            out.extend(pattern.binding_names());
            collect_expr_bindings(iterable, out);
            collect_block_bindings(body, out);
        }
        ExprKind::Closure { params, body, .. } => {
            for p in params.iter() {
                out.extend(p.pattern.binding_names());
            }
            collect_expr_bindings(body, out);
        }
        // Both child callbacks need `out`; share it through a cell so the
        // closures don't each claim unique access.
        _ => {
            let cell = std::cell::RefCell::new(&mut *out);
            walk_expr_children(
                e,
                &mut |c| {
                    let mut g = cell.borrow_mut();
                    collect_expr_bindings(c, &mut g);
                },
                &mut |b| {
                    let mut g = cell.borrow_mut();
                    collect_block_bindings(b, &mut g);
                },
            );
        }
    }
}

/// Names declared by `items` — the shadowing guard for
/// [`alias_subst_for_module`].
pub(crate) fn declared_names(items: &[Item]) -> HashSet<String> {
    let mut out = HashSet::new();
    for item in items {
        match item {
            Item::Function(f) => out.insert(f.name.clone()),
            Item::StructDef(s) => out.insert(s.name.clone()),
            Item::UnionDef(u) => out.insert(u.name.clone()),
            Item::EnumDef(e) => out.insert(e.name.clone()),
            Item::TraitDef(t) => out.insert(t.name.clone()),
            Item::TraitAlias(t) => out.insert(t.name.clone()),
            Item::MarkerTrait(t) => out.insert(t.name.clone()),
            Item::ConstDecl(c) => out.insert(c.name.clone()),
            _ => false,
        };
    }
    out
}

/// Rewrite every aliased reference in `item` to its canonical name.
pub(crate) fn rewrite_item(item: &mut Item, subst: &HashMap<String, TypeExpr>) {
    if subst.is_empty() {
        return;
    }
    match item {
        Item::Function(f) => rewrite_function(f, subst),
        Item::StructDef(s) => {
            rewrite_generic_params(s.generic_params.as_mut(), subst);
            rewrite_where(s.where_clause.as_mut(), subst);
            for field in &mut s.fields {
                field.ty = subst_type_expr(&field.ty, subst);
            }
            for inv in s.invariants.iter_mut().chain(s.impl_invariants.iter_mut()) {
                rewrite_expr(inv, subst);
            }
        }
        Item::UnionDef(u) => {
            for field in &mut u.fields {
                field.ty = subst_type_expr(&field.ty, subst);
            }
        }
        Item::EnumDef(e) => {
            rewrite_generic_params(e.generic_params.as_mut(), subst);
            rewrite_where(e.where_clause.as_mut(), subst);
            for v in &mut e.variants {
                match &mut v.kind {
                    VariantKind::Unit => {}
                    VariantKind::Tuple(tys) => {
                        for t in tys.iter_mut() {
                            *t = subst_type_expr(t, subst);
                        }
                    }
                    VariantKind::Struct(fields) => {
                        for f in fields.iter_mut() {
                            f.ty = subst_type_expr(&f.ty, subst);
                        }
                    }
                }
                if let Some(d) = v.discriminant.as_mut() {
                    rewrite_expr(d, subst);
                }
            }
        }
        Item::TraitDef(t) => {
            rewrite_generic_params(t.generic_params.as_mut(), subst);
            rewrite_where(t.where_clause.as_mut(), subst);
            for sup in &mut t.supertraits {
                rewrite_bound(sup, subst);
            }
            for ti in &mut t.items {
                if let TraitItem::Method(m) = ti {
                    rewrite_trait_method(m, subst);
                }
            }
        }
        Item::MarkerTrait(t) => {
            rewrite_generic_params(t.generic_params.as_mut(), subst);
            for sup in &mut t.supertraits {
                rewrite_bound(sup, subst);
            }
        }
        Item::TraitAlias(t) => {
            rewrite_generic_params(t.generic_params.as_mut(), subst);
            for b in &mut t.bounds {
                rewrite_bound(b, subst);
            }
        }
        Item::ImplBlock(b) => {
            rewrite_generic_params(b.generic_params.as_mut(), subst);
            rewrite_where(b.where_clause.as_mut(), subst);
            // `impl D for Widget` — BOTH halves can be aliased.
            if let Some(tn) = b.trait_name.as_mut() {
                rewrite_path_segments(&mut tn.segments, subst);
            }
            b.target_type = subst_type_expr(&b.target_type, subst);
            for ii in &mut b.items {
                match ii {
                    ImplItem::Method(m) => rewrite_function(m, subst),
                    ImplItem::AssocType(a) => {
                        a.ty = subst_type_expr(&a.ty, subst);
                    }
                }
            }
        }
        Item::ConstDecl(c) => {
            c.ty = subst_type_expr(&c.ty, subst);
            rewrite_expr(&mut c.value, subst);
        }
        Item::ModuleBinding(m) => {
            if let Some(t) = m.ty.as_mut() {
                *t = subst_type_expr(t, subst);
            }
            rewrite_expr(&mut m.value, subst);
        }
        Item::TestCase(t) => rewrite_block(&mut t.body, subst),
        // No type or path reference an alias can reach.
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
        | Item::TypeAlias(_)
        | Item::DistinctType(_) => {}
    }
}

fn rewrite_function(f: &mut Function, subst: &HashMap<String, TypeExpr>) {
    rewrite_generic_params(f.generic_params.as_mut(), subst);
    rewrite_where(f.where_clause.as_mut(), subst);
    for p in &mut f.params {
        p.ty = subst_type_expr(&p.ty, subst);
    }
    if let Some(rt) = f.return_type.take() {
        f.return_type = Some(subst_type_expr(&rt, subst));
    }
    rewrite_block(&mut f.body, subst);
}

fn rewrite_trait_method(m: &mut TraitMethod, subst: &HashMap<String, TypeExpr>) {
    rewrite_generic_params(m.generic_params.as_mut(), subst);
    rewrite_where(m.where_clause.as_mut(), subst);
    for p in &mut m.params {
        p.ty = subst_type_expr(&p.ty, subst);
    }
    if let Some(rt) = m.return_type.take() {
        m.return_type = Some(subst_type_expr(&rt, subst));
    }
    if let Some(b) = m.body.as_mut() {
        rewrite_block(b, subst);
    }
}

fn rewrite_generic_params(g: Option<&mut GenericParams>, subst: &HashMap<String, TypeExpr>) {
    let Some(g) = g else { return };
    for gp in &mut g.params {
        for b in &mut gp.bounds {
            rewrite_bound(b, subst);
        }
        if let Some(ct) = gp.const_type.as_mut() {
            *ct = subst_type_expr(ct, subst);
        }
    }
}

fn rewrite_where(w: Option<&mut WhereClause>, subst: &HashMap<String, TypeExpr>) {
    let Some(w) = w else { return };
    for c in &mut w.constraints {
        // Generic ARGUMENTS + projected types via desugar's helper …
        subst_where_constraint(c, subst);
        // … then the bound NAMES, which it deliberately leaves alone.
        match c {
            WhereConstraint::TypeBound { bounds, .. }
            | WhereConstraint::ProjectionBound { bounds, .. } => {
                for b in bounds.iter_mut() {
                    rewrite_path_segments(&mut b.path, subst);
                }
            }
            WhereConstraint::AssocTypeEq { .. } | WhereConstraint::ConstPredicate { .. } => {}
        }
    }
}

/// A trait bound needs BOTH halves rewritten. `desugar`'s
/// `subst_trait_bound` only touches the generic ARGUMENTS, because a type
/// param can never be the trait itself — an import alias can
/// (`import m.{Doer as D}` + `T: D`), so the bound's own path is rewritten
/// here too.
fn rewrite_bound(b: &mut TraitBound, subst: &HashMap<String, TypeExpr>) {
    subst_trait_bound(b, subst);
    rewrite_path_segments(&mut b.path, subst);
}

/// Rewrite a leading path segment naming an aliased item. Unlike
/// `subst_leading_type_name`'s type-param use, a BARE single segment is a
/// legitimate alias reference (`D`, `Widget`), so there is no
/// `qualified_only` gate.
fn rewrite_path_segments(segments: &mut [String], subst: &HashMap<String, TypeExpr>) {
    subst_leading_type_name(segments, subst, /* qualified_only */ false);
}

fn rewrite_block(block: &mut Block, subst: &HashMap<String, TypeExpr>) {
    subst_block(block, subst);
    // `subst_block` covers types and expressions but not PATTERNS — a type
    // param cannot appear in one, an aliased enum/struct name can
    // (`match e { Widget.Some(x) => … }`).
    for stmt in &mut block.stmts {
        rewrite_stmt_patterns(stmt, subst);
    }
    if let Some(e) = block.final_expr.as_mut() {
        rewrite_expr_patterns(e, subst);
    }
}

fn rewrite_expr(e: &mut Expr, subst: &HashMap<String, TypeExpr>) {
    subst_expr(e, subst);
    rewrite_expr_patterns(e, subst);
}

fn rewrite_stmt_patterns(stmt: &mut Stmt, subst: &HashMap<String, TypeExpr>) {
    match &mut stmt.kind {
        StmtKind::Let { pattern, value, .. } => {
            rewrite_pattern(pattern, subst);
            rewrite_expr_patterns(value, subst);
        }
        StmtKind::LetElse {
            pattern,
            value,
            else_block,
            ..
        } => {
            rewrite_pattern(pattern, subst);
            rewrite_expr_patterns(value, subst);
            rewrite_block_patterns(else_block, subst);
        }
        StmtKind::Expr(e) => rewrite_expr_patterns(e, subst),
        StmtKind::Assign { target, value, .. } | StmtKind::CompoundAssign { target, value, .. } => {
            rewrite_expr_patterns(target, subst);
            rewrite_expr_patterns(value, subst);
        }
        StmtKind::MultiAssign {
            targets, values, ..
        } => {
            for t in targets.iter_mut().chain(values.iter_mut()) {
                rewrite_expr_patterns(t, subst);
            }
        }
        StmtKind::Defer { body } => rewrite_block_patterns(body, subst),
        StmtKind::ErrDefer { body, .. } => rewrite_block_patterns(body, subst),
        StmtKind::LetUninit { .. } => {}
    }
}

fn rewrite_block_patterns(block: &mut Block, subst: &HashMap<String, TypeExpr>) {
    for stmt in &mut block.stmts {
        rewrite_stmt_patterns(stmt, subst);
    }
    if let Some(e) = block.final_expr.as_mut() {
        rewrite_expr_patterns(e, subst);
    }
}

/// Walk to every pattern reachable from `e`. Only the pattern-bearing
/// expression forms need naming here; everything else recurses through the
/// generic child walk below.
fn rewrite_expr_patterns(e: &mut Expr, subst: &HashMap<String, TypeExpr>) {
    // A bare `Identifier` naming an aliased item — the shape a FUNCTION alias
    // takes (`import m.{mk as build};` + `build(9)`), which `subst_expr`
    // leaves alone because for its original trait-type-param use an
    // identifier is always a value, never a type (B-2026-07-29-23).
    //
    // Safe unconditionally HERE because `alias_subst_for_module` has already
    // dropped any alias that the module binds as a value anywhere — see
    // `bound_value_names`. So an identifier matching a live alias cannot be a
    // local variable, parameter, closure param, or pattern binding.
    if let ExprKind::Identifier(n) = &mut e.kind {
        if let Some(TypeExpr {
            kind: TypeKind::Path(p),
            ..
        }) = subst.get(n.as_str())
        {
            if p.segments.len() == 1 && p.generic_args.is_none() {
                *n = p.segments[0].clone();
            }
        }
        return;
    }
    match &mut e.kind {
        ExprKind::Match { scrutinee, arms } => {
            rewrite_expr_patterns(scrutinee, subst);
            for arm in arms.iter_mut() {
                rewrite_pattern(&mut arm.pattern, subst);
                if let Some(g) = arm.guard.as_mut() {
                    rewrite_expr_patterns(g, subst);
                }
                rewrite_expr_patterns(&mut arm.body, subst);
            }
        }
        ExprKind::IfLet {
            pattern,
            value,
            then_block,
            else_branch,
        } => {
            rewrite_pattern(pattern, subst);
            rewrite_expr_patterns(value, subst);
            rewrite_block_patterns(then_block, subst);
            if let Some(b) = else_branch.as_mut() {
                rewrite_expr_patterns(b, subst);
            }
        }
        ExprKind::WhileLet {
            pattern,
            value,
            body,
            ..
        } => {
            rewrite_pattern(pattern, subst);
            rewrite_expr_patterns(value, subst);
            rewrite_block_patterns(body, subst);
        }
        ExprKind::For {
            pattern,
            iterable,
            body,
            ..
        } => {
            rewrite_pattern(pattern, subst);
            rewrite_expr_patterns(iterable, subst);
            rewrite_block_patterns(body, subst);
        }
        _ => walk_expr_children(e, &mut |c| rewrite_expr_patterns(c, subst), &mut |b| {
            rewrite_block_patterns(b, subst)
        }),
    }
}

/// Recurse into every sub-expression / sub-block of `e`.
///
/// Deliberately conservative: an `ExprKind` this does not name simply has
/// its patterns left alone, which is the pre-existing behaviour, not a
/// regression. The forms that CAN carry a pattern are all handled by the
/// caller above; this only has to reach them.
pub(crate) fn walk_expr_children(
    e: &mut Expr,
    on_expr: &mut impl FnMut(&mut Expr),
    on_block: &mut impl FnMut(&mut Block),
) {
    match &mut e.kind {
        ExprKind::Binary { left, right, .. }
        | ExprKind::NilCoalesce { left, right }
        | ExprKind::Pipe { left, right } => {
            on_expr(left);
            on_expr(right);
        }
        ExprKind::Unary { operand, .. } => on_expr(operand),
        ExprKind::Question(x) | ExprKind::Cast { expr: x, .. } => on_expr(x),
        ExprKind::OptionalChain { object, args, .. } => {
            on_expr(object);
            if let Some(args) = args {
                for a in args.iter_mut() {
                    on_expr(&mut a.value);
                }
            }
        }
        ExprKind::Call { callee, args } => {
            on_expr(callee);
            for a in args.iter_mut() {
                on_expr(&mut a.value);
            }
        }
        ExprKind::MethodCall { object, args, .. } => {
            on_expr(object);
            for a in args.iter_mut() {
                on_expr(&mut a.value);
            }
        }
        ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
            on_expr(object)
        }
        ExprKind::Index { object, index } => {
            on_expr(object);
            on_expr(index);
        }
        ExprKind::If {
            condition,
            then_block,
            else_branch,
        } => {
            on_expr(condition);
            on_block(then_block);
            if let Some(b) = else_branch.as_mut() {
                on_expr(b);
            }
        }
        ExprKind::While {
            condition, body, ..
        } => {
            on_expr(condition);
            on_block(body);
        }
        ExprKind::Block(b)
        | ExprKind::Comptime(b)
        | ExprKind::Unsafe(b)
        | ExprKind::Try(b)
        | ExprKind::Seq(b)
        | ExprKind::Par(b)
        | ExprKind::Loop { body: b, .. }
        | ExprKind::LabeledBlock { body: b, .. } => on_block(b),
        ExprKind::Closure { body, .. } => on_expr(body),
        ExprKind::Return(opt) | ExprKind::Break { value: opt, .. } => {
            if let Some(x) = opt.as_mut() {
                on_expr(x);
            }
        }
        ExprKind::Tuple(items)
        | ExprKind::ArrayLiteral(items)
        | ExprKind::PrefixCollectionLiteral { items, .. } => {
            for x in items.iter_mut() {
                on_expr(x);
            }
        }
        ExprKind::RepeatLiteral { value, count, .. } => {
            on_expr(value);
            on_expr(count);
        }
        ExprKind::MapLiteral(pairs) => {
            for (k, v) in pairs.iter_mut() {
                on_expr(k);
                on_expr(v);
            }
        }
        ExprKind::StructLiteral { fields, spread, .. } => {
            for f in fields.iter_mut() {
                on_expr(&mut f.value);
            }
            if let Some(s) = spread.as_mut() {
                on_expr(s);
            }
        }
        ExprKind::Range { start, end, .. } => {
            if let Some(x) = start.as_mut() {
                on_expr(x);
            }
            if let Some(x) = end.as_mut() {
                on_expr(x);
            }
        }
        ExprKind::Lock { mutex, body, .. } => {
            on_expr(mutex);
            on_block(body);
        }
        ExprKind::Providers { bindings, body } => {
            for b in bindings.iter_mut() {
                on_expr(&mut b.value);
            }
            on_block(body);
        }
        ExprKind::InterpolatedStringLit(parts) => {
            for part in parts.iter_mut() {
                if let ParsedInterpolationPart::Expr(x, _) = part {
                    on_expr(x);
                }
            }
        }
        _ => {}
    }
}

fn rewrite_pattern(p: &mut Pattern, subst: &HashMap<String, TypeExpr>) {
    match &mut p.kind {
        PatternKind::TupleVariant { path, patterns } => {
            rewrite_path_segments(path, subst);
            for f in patterns.iter_mut() {
                rewrite_pattern(f, subst);
            }
        }
        PatternKind::Struct { path, fields, .. } => {
            rewrite_path_segments(path, subst);
            for f in fields.iter_mut() {
                if let Some(p) = f.pattern.as_mut() {
                    rewrite_pattern(p, subst);
                }
            }
        }
        PatternKind::Tuple(items) | PatternKind::Or(items) => {
            for f in items.iter_mut() {
                rewrite_pattern(f, subst);
            }
        }
        PatternKind::Slice { prefix, suffix, .. } => {
            for f in prefix.iter_mut().chain(suffix.iter_mut()) {
                rewrite_pattern(f, subst);
            }
        }
        PatternKind::AtBinding { pattern, .. } => rewrite_pattern(pattern, subst),
        _ => {}
    }
}
