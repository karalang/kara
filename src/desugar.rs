//! AST-rewriting pre-resolve passes that eliminate sugar so downstream
//! phases only see the canonical form.
//!
//! Today this houses one pass: slice 2 of the `impl Trait` epic —
//! argument-position `impl Trait` desugars to a fresh anonymous generic
//! parameter on the enclosing function. See `docs/design.md § `impl
//! Trait` (Existential Types)` and `phase-5-diagnostics.md` line 395.
//!
//! Pipeline placement: between [`crate::parse`] and [`crate::resolve`].
//! The compilation drivers in `lib.rs` and `cli.rs` invoke
//! [`desugar_program`] on the mutable `Program` before resolution; the
//! formatter path deliberately skips this pass so `impl Trait` round-trips
//! verbatim.

use crate::ast::*;
use crate::token::Span;

/// Run every AST-rewriting pre-resolve pass over `program` in place.
/// Today: argument-position `impl Trait` desugar (slice 2) and
/// parallel/destructuring-assignment desugar.
pub fn desugar_program(program: &mut Program) {
    synthesize_default_impls(program);
    synthesize_trait_default_methods(program);
    propagate_codegen_hints(program);
    desugar_impl_trait_args_in_program(program);
    desugar_multi_assign_in_program(program);
    desugar_multiversion_in_program(program);
}

/// Desugar `#[multiversion(baseline, "avx2", "avx512f")]` free functions into
/// runtime-dispatched multiversioned variants (design.md § Multiversioning >
/// `cpu-baseline` and `#[multiversion]`). For `fn f(a, b) -> R { body }`:
///
///   * each listed feature becomes an `unsafe` clone tagged
///     `#[target_feature(enable = "<feat>")]`, named `f$<feat>`;
///   * a plain (safe) `f$baseline` clone carries the un-widened body;
///   * `f` itself is rewritten into a SAFE thunk that probes
///     `cpu.supports("<feat>")` — last-listed (widest) first — and calls the
///     matching variant in an `unsafe` block, falling back to `f$baseline`.
///
/// Reuses the shipped `#[target_feature]` codegen (per-function `target-features`
/// attribute) and the `cpu.supports` intrinsic — no core-pipeline change; the
/// synthesized variants and thunk are ordinary functions every later phase sees.
/// Scope (enforced in the parser scan; defensively re-checked here): a
/// non-generic free function with simple binding parameters.
fn desugar_multiversion_in_program(program: &mut Program) {
    let mut synthesized: Vec<Item> = Vec::new();
    for item in program.items.iter_mut() {
        let Item::Function(f) = item else { continue };
        let Some(features) = multiversion_feature_list(&f.attributes) else {
            continue;
        };
        if features.is_empty()
            || f.self_param.is_some()
            || f.generic_params.is_some()
            || !f
                .params
                .iter()
                .all(|p| matches!(p.pattern.kind, PatternKind::Binding(_)))
        {
            // Malformed / out-of-scope (parser already reported it) — leave the
            // fn untouched rather than synthesize a broken thunk.
            continue;
        }
        let base = f.name.clone();
        let sp = f.span.clone();
        // Forward each param to the variant by name.
        let fwd: Vec<CallArg> = f
            .params
            .iter()
            .map(|p| {
                let PatternKind::Binding(n) = &p.pattern.kind else {
                    unreachable!("guarded above")
                };
                CallArg {
                    label: None,
                    mut_marker: false,
                    value: mv_ident(n, sp.clone()),
                    span: sp.clone(),
                }
            })
            .collect();

        // Baseline clone: plain (safe), no multiversion attr.
        let mut baseline_fn = f.clone();
        baseline_fn.name = format!("{base}$baseline");
        baseline_fn
            .attributes
            .retain(|a| !a.is_bare("multiversion"));
        synthesized.push(Item::Function(baseline_fn));

        // Per-feature clone: unsafe + `#[target_feature(enable = "<feat>")]`.
        for feat in &features {
            let mut vf = f.clone();
            vf.name = format!("{base}${feat}");
            vf.is_unsafe = true;
            vf.attributes.retain(|a| !a.is_bare("multiversion"));
            vf.attributes.push(mv_target_feature_attr(feat, sp.clone()));
            synthesized.push(Item::Function(vf));
        }

        // Rewrite `f` into the dispatch thunk. Build the nested if-else from the
        // inside out: innermost `else` = the baseline call; each feature (in
        // listed order) wraps the accumulator, so the LAST-listed feature ends
        // up outermost = checked first (list narrowest→widest per the design).
        f.attributes.retain(|a| !a.is_bare("multiversion"));
        let mut acc = mv_call(&format!("{base}$baseline"), &fwd, sp.clone());
        for feat in &features {
            let feat_call = mv_call(&format!("{base}${feat}"), &fwd, sp.clone());
            let unsafe_call = Expr {
                kind: ExprKind::Unsafe(mv_block(feat_call, sp.clone())),
                span: sp.clone(),
            };
            acc = Expr {
                kind: ExprKind::If {
                    condition: Box::new(mv_cpu_supports(feat, sp.clone())),
                    then_block: mv_block(unsafe_call, sp.clone()),
                    else_branch: Some(Box::new(acc)),
                },
                span: sp.clone(),
            };
        }
        f.body = mv_block(acc, sp.clone());
    }
    program.items.extend(synthesized);
}

fn mv_ident(name: &str, span: Span) -> Expr {
    Expr {
        kind: ExprKind::Identifier(name.to_string()),
        span,
    }
}

fn mv_call(name: &str, args: &[CallArg], span: Span) -> Expr {
    Expr {
        kind: ExprKind::Call {
            callee: Box::new(mv_ident(name, span.clone())),
            args: args.to_vec(),
        },
        span,
    }
}

fn mv_block(tail: Expr, span: Span) -> Block {
    Block {
        stmts: Vec::new(),
        final_expr: Some(Box::new(tail)),
        span,
    }
}

fn mv_cpu_supports(feat: &str, span: Span) -> Expr {
    Expr {
        kind: ExprKind::MethodCall {
            object: Box::new(mv_ident("cpu", span.clone())),
            method: "supports".to_string(),
            turbofish: None,
            args: vec![CallArg {
                label: None,
                mut_marker: false,
                value: Expr {
                    kind: ExprKind::StringLit(feat.to_string()),
                    span: span.clone(),
                },
                span: span.clone(),
            }],
            args_close_span: span.clone(),
        },
        span,
    }
}

fn mv_target_feature_attr(feat: &str, span: Span) -> Attribute {
    Attribute {
        span: span.clone(),
        path: vec!["target_feature".to_string()],
        args: vec![AttrArg {
            name: Some("enable".to_string()),
            value: Some(Expr {
                kind: ExprKind::StringLit(feat.to_string()),
                span: span.clone(),
            }),
            span: span.clone(),
        }],
        string_value: None,
    }
}

/// Materialize trait **default method bodies** into every impl that does not
/// override them, so a default method is callable on an implementor without
/// the impl re-implementing it (B-2026-07-03-8). For `impl Tr for T` where
/// trait `Tr` declares `fn m(self) -> R { <default body> }` and the impl body
/// provides no `m`, this copies `m` (converted from its `TraitMethod` node to
/// the `Function` node an impl method carries) into the impl's items. All
/// downstream phases then see the default exactly as if the user had written
/// it in the impl — which is the one form that already worked end-to-end
/// (typecheck method resolution, `eval_method_call` dispatch, and codegen's
/// `make_impl_method_function` synthesis all key off the impl's item list).
/// `Self` in the copied body/signature resolves to the impl target through the
/// existing impl-method `Self` handling (`current_self_type` in the
/// typechecker, `rewrite_self_in_type_expr` in codegen).
///
/// Scope: only traits declared in the user program are consulted (baked
/// stdlib traits are spliced separately and carry their own default
/// machinery), and only methods with a body are candidates. Overriding impls
/// keep their own method (the `provided` guard). Runs pre-resolve so the
/// synthesized methods are visible to name resolution and every later phase.
///
/// **Generic traits** (`trait Box[T] { fn twice(self) -> T { .. } }`): the
/// copied default's `T` is out of scope in a concrete `impl Box[i64] for W`,
/// so the impl's trait-args are substituted through the copy first
/// (`substitute_trait_params_in_function`) — the trait's declared params zip
/// positionally against `impl Tr[Args]`'s type-args, and every mention of a
/// trait param in the copied method's param/return types, `where` clause, own
/// generic-param bounds, and body type-expressions (`T`-typed locals, casts,
/// `T::assoc()` paths) is rewritten to the concrete arg. A method's own
/// generic params (`fold[A]`) shadow any same-named trait param and are left
/// untouched. Non-generic traits pass through with an empty substitution —
/// byte-identical to the pre-generic behavior (B-2026-07-03-8 / -10).
/// Collect every trait's default-bodied methods (converted to the `Function`
/// shape an impl method carries) from `items`, keyed by trait name, into `out`.
/// Uses `entry().or_insert` so an earlier-collected trait of the same name wins
/// — user-declared traits are passed before the baked stdlib ones so a user
/// trait shadows a same-named stdlib trait.
fn collect_trait_defaults_from_items(
    items: &[Item],
    out: &mut std::collections::HashMap<String, (Vec<String>, Vec<Function>)>,
) {
    for item in items {
        let Item::TraitDef(t) = item else { continue };
        let mut defaults = Vec::new();
        for ti in &t.items {
            if let TraitItem::Method(m) = ti {
                if m.body.is_some() {
                    defaults.push(trait_method_to_function(m, t.stdlib_origin));
                }
            }
        }
        if defaults.is_empty() {
            continue;
        }
        let param_names = t
            .generic_params
            .as_ref()
            .map(|g| g.params.iter().map(|p| p.name.clone()).collect())
            .unwrap_or_default();
        out.entry(t.name.clone()).or_insert((param_names, defaults));
    }
}

fn synthesize_trait_default_methods(program: &mut Program) {
    use std::collections::{HashMap, HashSet};

    // trait name -> (declared generic-param names, default-bodied methods
    // already converted to the `Function` shape an `ImplItem::Method` carries).
    // User-declared traits are collected FIRST so a user trait shadows a
    // same-named baked stdlib trait (`.entry().or_insert`).
    let mut trait_defaults: HashMap<String, (Vec<String>, Vec<Function>)> = HashMap::new();
    collect_trait_defaults_from_items(&program.items, &mut trait_defaults);
    // Baked stdlib traits (`Reduce[T]` etc.) live in `STDLIB_PROGRAMS`, not the
    // user program, so a user `impl Reduce[T] for MyType` can only inherit their
    // default methods if we pull them in here explicitly (S6b-4). The spliced
    // copy is compiled as ordinary user code in the user program (its
    // `stdlib_origin` is cleared below), unlike the never-checked stdlib impl
    // bodies.
    for (_, sp) in crate::prelude::STDLIB_PROGRAMS.iter() {
        collect_trait_defaults_from_items(&sp.items, &mut trait_defaults);
    }
    if trait_defaults.is_empty() {
        return;
    }

    for item in &mut program.items {
        let Item::ImplBlock(imp) = item else { continue };
        // Snapshot the trait's name + type-args, releasing the borrow on
        // `imp.trait_name` before the mutable `imp.items` push below.
        let (trait_name, trait_args) = match &imp.trait_name {
            Some(p) => match p.segments.last() {
                Some(n) => (n.clone(), p.generic_args.clone()),
                None => continue,
            },
            None => continue,
        };
        let Some((param_names, defaults)) = trait_defaults.get(&trait_name) else {
            continue;
        };
        // Positional trait-arg substitution: `impl Tr[i64] for W` binds the
        // trait's declared param -> `i64`. Only `Type` args participate
        // (const/shape trait params carry no type-expr to substitute).
        let mut subst: HashMap<String, TypeExpr> = HashMap::new();
        if let Some(args) = &trait_args {
            for (name, arg) in param_names.iter().zip(args.iter()) {
                if let GenericArg::Type(te) = arg {
                    subst.insert(name.clone(), te.clone());
                }
            }
        }
        let provided: HashSet<String> = imp
            .items
            .iter()
            .filter_map(|it| match it {
                ImplItem::Method(m) => Some(m.name.clone()),
                _ => None,
            })
            .collect();
        for def_fn in defaults {
            if provided.contains(&def_fn.name) {
                continue;
            }
            let mut copy = def_fn.clone();
            // The spliced method is real code in the user program — resolve,
            // typecheck, ownership-check, and codegen must all process it (a
            // stdlib-origin default body would otherwise be skipped like the
            // never-checked baked impl bodies). Clear the flag; it is already
            // false for user-declared traits, so this is a no-op there.
            copy.stdlib_origin = false;
            if !subst.is_empty() {
                substitute_trait_params_in_function(&mut copy, &subst);
            }
            imp.items.push(ImplItem::Method(Box::new(copy)));
        }
    }
}

/// Substitute trait type-params (`subst`: trait-param-name -> concrete
/// `TypeExpr`) throughout a copied default method — its param types, return
/// type, `where` clause, own generic-param bounds, and body — so a generic
/// trait's default body is a well-formed *concrete* impl method once spliced
/// into `impl Tr[ConcreteArgs] for T`. A method's OWN generic params (e.g.
/// `fold[A]`) shadow any same-named trait param and are excluded while walking
/// that method (B-2026-07-03-10).
fn substitute_trait_params_in_function(
    f: &mut Function,
    subst: &std::collections::HashMap<String, TypeExpr>,
) {
    use std::collections::HashMap;

    // Drop entries shadowed by the method's own generic params.
    let effective: HashMap<String, TypeExpr> = match &f.generic_params {
        Some(g) => {
            let owned: std::collections::HashSet<&str> =
                g.params.iter().map(|p| p.name.as_str()).collect();
            subst
                .iter()
                .filter(|(k, _)| !owned.contains(k.as_str()))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        }
        None => subst.clone(),
    };
    if effective.is_empty() {
        return;
    }

    for p in &mut f.params {
        p.ty = subst_type_expr(&p.ty, &effective);
    }
    if let Some(rt) = f.return_type.take() {
        f.return_type = Some(subst_type_expr(&rt, &effective));
    }
    // A method's own generic-param bounds may reference a trait param in their
    // generic-args (`fold[A: From[T]]`); substitute those, leaving the param
    // names themselves alone.
    if let Some(g) = f.generic_params.as_mut() {
        for gp in &mut g.params {
            for b in &mut gp.bounds {
                subst_trait_bound(b, &effective);
            }
        }
    }
    // `where` constraints subjected on a substituted trait param become
    // concrete after substitution and are redundant on a concrete method —
    // drop them; substitute inside the ones kept (keyed on the method's own
    // params).
    if let Some(w) = f.where_clause.as_mut() {
        w.constraints
            .retain(|c| !where_constraint_subject_is_substituted(c, &effective));
        for c in &mut w.constraints {
            subst_where_constraint(c, &effective);
        }
    }
    subst_block(&mut f.body, &effective);
}

/// Map-keyed twin of `codegen::helpers::rewrite_self_in_type_expr`: replace a
/// bare single-segment type-param reference with its concrete `TypeExpr`,
/// recursing through every compound type form and generic-argument position.
fn subst_type_expr(te: &TypeExpr, subst: &std::collections::HashMap<String, TypeExpr>) -> TypeExpr {
    let kind = match &te.kind {
        TypeKind::Path(p) => {
            if p.segments.len() == 1 && p.generic_args.is_none() {
                if let Some(replacement) = subst.get(&p.segments[0]) {
                    // Substitute the whole node, keeping the reference's span.
                    return TypeExpr {
                        kind: replacement.kind.clone(),
                        span: te.span.clone(),
                    };
                }
            }
            TypeKind::Path(PathExpr {
                segments: p.segments.clone(),
                generic_args: p.generic_args.as_ref().map(|args| {
                    args.iter()
                        .map(|a| match a {
                            GenericArg::Type(t) => GenericArg::Type(subst_type_expr(t, subst)),
                            other => other.clone(),
                        })
                        .collect()
                }),
                span: p.span.clone(),
            })
        }
        TypeKind::Tuple(elems) => {
            TypeKind::Tuple(elems.iter().map(|e| subst_type_expr(e, subst)).collect())
        }
        TypeKind::Array { element, size } => TypeKind::Array {
            element: Box::new(subst_type_expr(element, subst)),
            size: size.clone(),
        },
        TypeKind::Pointer { is_mut, inner } => TypeKind::Pointer {
            is_mut: *is_mut,
            inner: Box::new(subst_type_expr(inner, subst)),
        },
        TypeKind::Ref(inner) => TypeKind::Ref(Box::new(subst_type_expr(inner, subst))),
        TypeKind::MutRef(inner) => TypeKind::MutRef(Box::new(subst_type_expr(inner, subst))),
        TypeKind::MutSlice(inner) => TypeKind::MutSlice(Box::new(subst_type_expr(inner, subst))),
        TypeKind::Weak(inner) => TypeKind::Weak(Box::new(subst_type_expr(inner, subst))),
        TypeKind::FnType {
            params,
            return_type,
            effect_spec,
            is_once,
        } => TypeKind::FnType {
            params: params.iter().map(|p| subst_type_expr(p, subst)).collect(),
            return_type: return_type
                .as_ref()
                .map(|r| Box::new(subst_type_expr(r, subst))),
            effect_spec: effect_spec.clone(),
            is_once: *is_once,
        },
        _ => te.kind.clone(),
    };
    TypeExpr {
        kind,
        span: te.span.clone(),
    }
}

/// Substitute trait params inside a `TraitBound`'s generic-args (the bound's
/// path/name is a trait name, never a type param, so it is left alone).
fn subst_trait_bound(b: &mut TraitBound, subst: &std::collections::HashMap<String, TypeExpr>) {
    if let Some(args) = b.generic_args.as_mut() {
        for a in args.iter_mut() {
            if let GenericArg::Type(t) = a {
                *t = subst_type_expr(t, subst);
            }
        }
    }
}

/// Does a `where` constraint's subject name a substituted trait param? Such a
/// constraint (`where T: Add` with `T -> i64`) is redundant on the concrete
/// synthesized method and is dropped rather than rewritten to `i64: Add`.
fn where_constraint_subject_is_substituted(
    c: &WhereConstraint,
    subst: &std::collections::HashMap<String, TypeExpr>,
) -> bool {
    match c {
        WhereConstraint::TypeBound { type_name, .. }
        | WhereConstraint::AssocTypeEq { type_name, .. } => subst.contains_key(type_name),
        _ => false,
    }
}

/// Substitute trait params inside the `where` constraints kept after the
/// subject-dropped filter (those keyed on the method's own generic params).
fn subst_where_constraint(
    c: &mut WhereConstraint,
    subst: &std::collections::HashMap<String, TypeExpr>,
) {
    match c {
        WhereConstraint::TypeBound { bounds, .. } => {
            for b in bounds.iter_mut() {
                subst_trait_bound(b, subst);
            }
        }
        WhereConstraint::AssocTypeEq { ty, .. } => {
            *ty = subst_type_expr(ty, subst);
        }
        WhereConstraint::ProjectionBound {
            projection, bounds, ..
        } => {
            *projection = subst_type_expr(projection, subst);
            for b in bounds.iter_mut() {
                subst_trait_bound(b, subst);
            }
        }
        WhereConstraint::ConstPredicate { .. } => {}
    }
}

/// Rewrite a leading path segment that names a substituted trait param to the
/// concrete type's leaf name — `T::zero` -> `Cnt::zero`, `T { .. }` -> `Cnt
/// { .. }`. Only fires when the concrete arg is itself a bare single-segment
/// type name (a primitive or plain nominal); a container arg like `Vec[i64]`
/// has no single leaf to graft into a `::`-path, so the segment is left as-is
/// (its generic-args are still substituted by the caller). `qualified_only`
/// skips bare single-segment value paths (an ordinary identifier is never a
/// type param) — set false for type-constructor positions (struct literals).
fn subst_leading_type_name(
    segments: &mut [String],
    subst: &std::collections::HashMap<String, TypeExpr>,
    qualified_only: bool,
) {
    if qualified_only && segments.len() < 2 {
        return;
    }
    let Some(first) = segments.first() else {
        return;
    };
    let Some(replacement) = subst.get(first) else {
        return;
    };
    let TypeKind::Path(p) = &replacement.kind else {
        return;
    };
    if p.segments.len() == 1 && p.generic_args.is_none() {
        segments[0] = p.segments[0].clone();
    }
}

fn subst_block(block: &mut Block, subst: &std::collections::HashMap<String, TypeExpr>) {
    for stmt in &mut block.stmts {
        subst_stmt(stmt, subst);
    }
    if let Some(e) = &mut block.final_expr {
        subst_expr(e, subst);
    }
}

fn subst_stmt(stmt: &mut Stmt, subst: &std::collections::HashMap<String, TypeExpr>) {
    match &mut stmt.kind {
        StmtKind::Let { ty, value, .. } => {
            if let Some(t) = ty.as_mut() {
                *t = subst_type_expr(t, subst);
            }
            subst_expr(value, subst);
        }
        StmtKind::LetUninit { ty, .. } => {
            *ty = subst_type_expr(ty, subst);
        }
        StmtKind::LetElse {
            ty,
            value,
            else_block,
            ..
        } => {
            if let Some(t) = ty.as_mut() {
                *t = subst_type_expr(t, subst);
            }
            subst_expr(value, subst);
            subst_block(else_block, subst);
        }
        StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => subst_block(body, subst),
        StmtKind::Assign { target, value } => {
            subst_expr(target, subst);
            subst_expr(value, subst);
        }
        StmtKind::CompoundAssign { target, value, .. } => {
            subst_expr(target, subst);
            subst_expr(value, subst);
        }
        StmtKind::MultiAssign { targets, values } => {
            // Not yet desugared at this pass (multi-assign runs later); walk
            // both sides so any `T`-typed cast/annotation inside is rewritten.
            for t in targets.iter_mut() {
                subst_expr(t, subst);
            }
            for v in values.iter_mut() {
                subst_expr(v, subst);
            }
        }
        StmtKind::Expr(e) => subst_expr(e, subst),
    }
}

/// Substitute trait params through every type-expression and type-naming path
/// segment reachable from `expr`, recursing into all sub-expressions. Mirrors
/// `walk_expr`'s variant coverage; the type-bearing arms (`Path`, `Cast`,
/// `OffsetOf`, `MethodCall` turbofish, `Closure` param annotations,
/// `StructLiteral` path) additionally rewrite their type positions.
fn subst_expr(expr: &mut Expr, subst: &std::collections::HashMap<String, TypeExpr>) {
    match &mut expr.kind {
        ExprKind::Integer(..)
        | ExprKind::Float(..)
        | ExprKind::CharLit(..)
        | ExprKind::ByteLit(..)
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
            subst_leading_type_name(segments, subst, /* qualified_only */ true);
            if let Some(args) = generic_args.as_mut() {
                for a in args.iter_mut() {
                    if let GenericArg::Type(t) = a {
                        *t = subst_type_expr(t, subst);
                    }
                }
            }
        }
        ExprKind::OffsetOf { ty, .. } => {
            *ty = subst_type_expr(ty, subst);
        }
        ExprKind::Cast { expr: e, ty } => {
            subst_expr(e, subst);
            *ty = subst_type_expr(ty, subst);
        }
        ExprKind::InterpolatedStringLit(parts) => {
            for part in parts.iter_mut() {
                if let ParsedInterpolationPart::Expr(e, _) = part {
                    subst_expr(e, subst);
                }
            }
        }
        ExprKind::Binary { left, right, .. }
        | ExprKind::NilCoalesce { left, right }
        | ExprKind::Pipe { left, right } => {
            subst_expr(left, subst);
            subst_expr(right, subst);
        }
        ExprKind::Unary { operand, .. } => subst_expr(operand, subst),
        ExprKind::Question(e) => subst_expr(e, subst),
        ExprKind::OptionalChain { object, args, .. } => {
            subst_expr(object, subst);
            if let Some(args) = args {
                for a in args.iter_mut() {
                    subst_expr(&mut a.value, subst);
                }
            }
        }
        ExprKind::Call { callee, args } => {
            subst_expr(callee, subst);
            for a in args.iter_mut() {
                subst_expr(&mut a.value, subst);
            }
        }
        ExprKind::MethodCall {
            object,
            turbofish,
            args,
            ..
        } => {
            subst_expr(object, subst);
            if let Some(tf) = turbofish.as_mut() {
                for t in tf.iter_mut() {
                    *t = subst_type_expr(t, subst);
                }
            }
            for a in args.iter_mut() {
                subst_expr(&mut a.value, subst);
            }
        }
        ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
            subst_expr(object, subst)
        }
        ExprKind::Index { object, index } => {
            subst_expr(object, subst);
            subst_expr(index, subst);
        }
        ExprKind::Block(b) | ExprKind::Comptime(b) => subst_block(b, subst),
        ExprKind::If {
            condition,
            then_block,
            else_branch,
        } => {
            subst_expr(condition, subst);
            subst_block(then_block, subst);
            if let Some(e) = else_branch {
                subst_expr(e, subst);
            }
        }
        ExprKind::IfLet {
            value,
            then_block,
            else_branch,
            ..
        } => {
            subst_expr(value, subst);
            subst_block(then_block, subst);
            if let Some(e) = else_branch {
                subst_expr(e, subst);
            }
        }
        ExprKind::Match { scrutinee, arms } => {
            subst_expr(scrutinee, subst);
            for arm in arms.iter_mut() {
                if let Some(g) = &mut arm.guard {
                    subst_expr(g, subst);
                }
                subst_expr(&mut arm.body, subst);
            }
        }
        ExprKind::While {
            condition, body, ..
        } => {
            subst_expr(condition, subst);
            subst_block(body, subst);
        }
        ExprKind::WhileLet { value, body, .. } => {
            subst_expr(value, subst);
            subst_block(body, subst);
        }
        ExprKind::For { iterable, body, .. } => {
            subst_expr(iterable, subst);
            subst_block(body, subst);
        }
        ExprKind::Loop { body, .. } => subst_block(body, subst),
        ExprKind::LabeledBlock { body, .. } => subst_block(body, subst),
        ExprKind::Closure { params, body, .. } => {
            for p in params.iter_mut() {
                if let Some(t) = p.ty.as_mut() {
                    *t = subst_type_expr(t, subst);
                }
            }
            subst_expr(body, subst);
        }
        ExprKind::Return(opt) => {
            if let Some(e) = opt {
                subst_expr(e, subst);
            }
        }
        ExprKind::Break { value, .. } => {
            if let Some(e) = value {
                subst_expr(e, subst);
            }
        }
        ExprKind::Tuple(items)
        | ExprKind::ArrayLiteral(items)
        | ExprKind::PrefixCollectionLiteral { items, .. } => {
            for e in items.iter_mut() {
                subst_expr(e, subst);
            }
        }
        ExprKind::RepeatLiteral { value, count, .. } => {
            subst_expr(value, subst);
            subst_expr(count, subst);
        }
        ExprKind::MapLiteral(pairs) => {
            for (k, v) in pairs.iter_mut() {
                subst_expr(k, subst);
                subst_expr(v, subst);
            }
        }
        ExprKind::StructLiteral {
            path,
            fields,
            spread,
        } => {
            // A struct-literal path is a type-constructor position, so a bare
            // single-segment `T { .. }` is a type param too (qualified_only =
            // false).
            subst_leading_type_name(path, subst, /* qualified_only */ false);
            for f in fields.iter_mut() {
                subst_expr(&mut f.value, subst);
            }
            if let Some(s) = spread {
                subst_expr(s, subst);
            }
        }
        ExprKind::Range { start, end, .. } => {
            if let Some(s) = start {
                subst_expr(s, subst);
            }
            if let Some(e) = end {
                subst_expr(e, subst);
            }
        }
        ExprKind::Unsafe(b) | ExprKind::Try(b) | ExprKind::Seq(b) | ExprKind::Par(b) => {
            subst_block(b, subst)
        }
        ExprKind::Lock { mutex, body, .. } => {
            subst_expr(mutex, subst);
            subst_block(body, subst);
        }
        ExprKind::Providers { bindings, body } => {
            for bnd in bindings.iter_mut() {
                subst_expr(&mut bnd.value, subst);
            }
            subst_block(body, subst);
        }
    }
}

/// Convert a default-bodied `TraitMethod` into the `Function` node an impl
/// method carries. Mirrors the synthesis in `TypeChecker::check_trait_def`
/// but preserves the codegen-relevant markers (`unsafe`, `#[track_caller]`,
/// inline/cold/gpu hints, deprecation/unstable, attributes) so a synthesized
/// default behaves like a hand-written impl method. Only called for methods
/// whose `body` is `Some`.
fn trait_method_to_function(m: &TraitMethod, stdlib_origin: bool) -> Function {
    Function {
        span: m.span.clone(),
        attributes: m.attributes.clone(),
        doc_comment: m.doc_comment.clone(),
        is_pub: false,
        is_private: false,
        is_unsafe: m.is_unsafe,
        is_comptime: false,
        name: m.name.clone(),
        generic_params: m.generic_params.clone(),
        params: m.params.clone(),
        self_param: m.self_param.clone(),
        return_type: m.return_type.clone(),
        effects: m.effects.clone(),
        requires: m.requires.clone(),
        ensures: m.ensures.clone(),
        where_clause: m.where_clause.clone(),
        body: m.body.clone().expect("caller guards on body.is_some()"),
        stdlib_origin,
        deprecation: m.deprecation.clone(),
        unstable: m.unstable.clone(),
        is_track_caller: m.is_track_caller,
        inline_hint: m.inline_hint,
        is_cold: m.is_cold,
        is_gpu: m.is_gpu,
        lint_overrides: Vec::new(),
        profile_compat: Vec::new(),
        abi: None,
    }
}

// ── Codegen-hint trait → impl propagation ────────────────────────
//
// A codegen-hint attribute (`#[inline]` / `#[inline(always)]` /
// `#[inline(never)]` / `#[cold]`) on a trait *method declaration*
// applies to every impl of that method unless the impl carries its own
// override — last-writer-wins, paralleling `#[track_caller]` (design.md
// § Codegen Hint Attributes > "Where they may appear"). The two axes
// (inline / cold) propagate independently: an impl that sets only its
// own `#[inline(never)]` still inherits the trait's `#[cold]`.
//
// Trait resolution at this pre-resolve stage is by name only — the last
// segment of the impl's `trait_name` path matched against `TraitDef`s in
// the same program. That covers same-program trait + impl (the common
// case and the v1 floor); cross-package trait hints are not propagated
// here (additive-later, alongside cross-package IR inlining).
fn propagate_codegen_hints(program: &mut Program) {
    use std::collections::HashMap;

    // trait name → (method name → (inline_hint, is_cold)), only for
    // trait methods that actually carry a hint.
    let mut trait_hints: HashMap<String, HashMap<String, (Option<InlineHint>, bool)>> =
        HashMap::new();
    for item in &program.items {
        if let Item::TraitDef(t) = item {
            for ti in &t.items {
                if let TraitItem::Method(m) = ti {
                    if m.inline_hint.is_some() || m.is_cold {
                        trait_hints
                            .entry(t.name.clone())
                            .or_default()
                            .insert(m.name.clone(), (m.inline_hint, m.is_cold));
                    }
                }
            }
        }
    }
    if trait_hints.is_empty() {
        return;
    }

    for item in &mut program.items {
        let Item::ImplBlock(imp) = item else { continue };
        let Some(trait_path) = &imp.trait_name else {
            continue;
        };
        let Some(trait_name) = trait_path.segments.last() else {
            continue;
        };
        let Some(methods) = trait_hints.get(trait_name) else {
            continue;
        };
        for ii in &mut imp.items {
            if let ImplItem::Method(m) = ii {
                if let Some(&(hint, cold)) = methods.get(&m.name) {
                    if m.inline_hint.is_none() {
                        m.inline_hint = hint;
                    }
                    if !m.is_cold {
                        m.is_cold = cold;
                    }
                }
            }
        }
    }
}

// ── `#[derive(Default)]` → synthetic `default()` assoc fn ────────
//
// `#[derive(Default)] struct Config { ... }` does not, on its own, give
// the type a `Config.default()` associated function — the dispatch
// machinery for `Type.default()` only fires against a real `default`
// method in an impl block. This pass closes that gap by synthesizing an
// inherent impl:
//
//     impl Config { fn default() -> Config { Config { f1: <d1>, ... } } }
//
// where each field initializer `<di>` is the field type's "zero-like"
// value — `0` / `0.0` / `false` / `""` / `'\0'` for primitives, and a
// recursive `FieldType.default()` call for a nested user type that also
// carries a `default` (derive-synthesized or hand-written). Because the
// synthesized body is built entirely from ordinary struct/enum-literal
// and literal AST, every downstream phase (typecheck, interpreter,
// codegen) handles it through already-tested paths — no per-backend
// special-casing of `default`. Spec: book appendix C (`Default`):
// "calls `.default()` on each field in declaration order and constructs
// the struct. For enums, the `#[default]`-marked variant is used" — a
// `#[derive(Default)]` enum must mark exactly one field-less variant
// with `#[default]` (enforced by the typechecker's
// `validate_derive_default`); the synthesized body is `Enum.Variant`.
//
// Scope (v1 floor): primitives + nested user types. Generic types and
// container/generic-argument field types (`Vec[T]`, `Option[T]`, tuples,
// refs, arrays, …) are out of scope here — the pass declines to
// synthesize for them, and the typechecker's `validate_derive_default`
// emits the clean "field ... is not Default" diagnostic instead.
fn synthesize_default_impls(program: &mut Program) {
    use std::collections::HashSet;

    // Names that will have a callable `default` — a non-generic
    // struct/enum carrying `#[derive(Default)]`, or any type with a
    // hand-written `default` method in an impl block. A nested field of
    // such a type lowers to `FieldType.default()`; anything else is not
    // (yet) defaultable and blocks synthesis for the enclosing type.
    let mut defaultable: HashSet<String> = HashSet::new();
    for item in &program.items {
        match item {
            Item::StructDef(s) if s.generic_params.is_none() && derives_default(&s.attributes) => {
                defaultable.insert(s.name.clone());
            }
            Item::EnumDef(e) if e.generic_params.is_none() && derives_default(&e.attributes) => {
                defaultable.insert(e.name.clone());
            }
            Item::ImplBlock(imp) => {
                let provides_default = imp
                    .items
                    .iter()
                    .any(|it| matches!(it, ImplItem::Method(m) if m.name == "default"));
                if provides_default {
                    if let Some(name) = type_leaf_name(&imp.target_type) {
                        defaultable.insert(name);
                    }
                }
            }
            _ => {}
        }
    }

    // Types that already have a hand-written `default` — never
    // double-define (the user's impl wins; deriving on top is their
    // call to make, and a redundant synthesized fn would collide).
    let mut has_user_default: HashSet<String> = HashSet::new();
    for item in &program.items {
        if let Item::ImplBlock(imp) = item {
            let provides_default = imp
                .items
                .iter()
                .any(|it| matches!(it, ImplItem::Method(m) if m.name == "default"));
            if provides_default {
                if let Some(name) = type_leaf_name(&imp.target_type) {
                    has_user_default.insert(name);
                }
            }
        }
    }

    let mut synthesized: Vec<Item> = Vec::new();
    for item in &program.items {
        match item {
            Item::StructDef(s)
                if s.generic_params.is_none()
                    && derives_default(&s.attributes)
                    && !has_user_default.contains(&s.name) =>
            {
                if let Some(body) = struct_default_body(s, &defaultable) {
                    synthesized.push(make_default_impl(&s.name, body, s.span.clone()));
                }
            }
            Item::EnumDef(e)
                if e.generic_params.is_none()
                    && derives_default(&e.attributes)
                    && !has_user_default.contains(&e.name) =>
            {
                if let Some(body) = enum_default_body(e) {
                    synthesized.push(make_default_impl(&e.name, body, e.span.clone()));
                }
            }
            _ => {}
        }
    }
    program.items.extend(synthesized);
}

fn derives_default(attributes: &[Attribute]) -> bool {
    crate::typechecker::extract_derived_traits(attributes).contains("Default")
}

/// Leaf type name of a single-segment, non-generic path type — the only
/// shape `default()` synthesis recognizes. `None` for tuples, refs,
/// arrays, generic-argument types, and multi-segment paths.
fn type_leaf_name(ty: &TypeExpr) -> Option<String> {
    if let TypeKind::Path(p) = &ty.kind {
        if p.segments.len() == 1 && p.generic_args.is_none() {
            return Some(p.segments[0].clone());
        }
    }
    None
}

/// The default initializer expression for a field of type `ty`, or
/// `None` when the type is outside this pass's v1 scope (containers,
/// generics, tuples, refs, or a named type with no reachable `default`).
fn default_field_expr(
    ty: &TypeExpr,
    defaultable: &std::collections::HashSet<String>,
) -> Option<Expr> {
    let span = ty.span.clone();
    let name = type_leaf_name(ty)?;
    let kind = match name.as_str() {
        "i8" | "i16" | "i32" | "i64" | "i128" | "isize" | "u8" | "u16" | "u32" | "u64" | "u128"
        | "usize" => ExprKind::Integer(0, None),
        "f32" | "f64" => ExprKind::Float(0.0, None),
        "bool" => ExprKind::Bool(false),
        "char" => ExprKind::CharLit('\0'),
        "String" => ExprKind::StringLit(String::new()),
        other if defaultable.contains(other) => ExprKind::Call {
            callee: Box::new(Expr {
                kind: ExprKind::Path {
                    segments: vec![other.to_string(), "default".to_string()],
                    generic_args: None,
                },
                span: span.clone(),
            }),
            args: Vec::new(),
        },
        _ => return None,
    };
    Some(Expr { kind, span })
}

/// `Name { f1: <d1>, ... }` literal for a derive-Default struct, or
/// `None` when any field is out of scope.
fn struct_default_body(
    s: &StructDef,
    defaultable: &std::collections::HashSet<String>,
) -> Option<Expr> {
    let mut fields = Vec::with_capacity(s.fields.len());
    for f in &s.fields {
        let value = default_field_expr(&f.ty, defaultable)?;
        fields.push(FieldInit {
            name: f.name.clone(),
            value,
            shorthand: false,
            span: f.span.clone(),
        });
    }
    Some(Expr {
        kind: ExprKind::StructLiteral {
            path: vec![s.name.clone()],
            fields,
            spread: None,
        },
        span: s.span.clone(),
    })
}

/// Default literal for a derive-Default enum: the unique `#[default]`-
/// marked, field-less variant, lowered to `Enum.Variant`. `None` when
/// the marker rule is not satisfied (zero or multiple markers, or the
/// marked variant carries a payload) — the typechecker's
/// `validate_derive_default` emits the focused diagnostic for each of
/// those cases, so declining here just suppresses a redundant
/// synthesized impl, never a silent acceptance.
fn enum_default_body(e: &EnumDef) -> Option<Expr> {
    let mut marked = e
        .variants
        .iter()
        .filter(|v| v.attributes.iter().any(|a| a.is_bare("default")));
    let variant = marked.next()?;
    // More than one marker — ambiguous, decline (typechecker reports).
    if marked.next().is_some() {
        return None;
    }
    // The marked variant must be field-less; a payload default is a
    // typechecker error, not a synthesizable body.
    if !matches!(variant.kind, VariantKind::Unit) {
        return None;
    }
    Some(Expr {
        kind: ExprKind::Path {
            segments: vec![e.name.clone(), variant.name.clone()],
            generic_args: None,
        },
        span: e.span.clone(),
    })
}

/// Wrap a `default()` body expression in an inherent
/// `impl Name { fn default() -> Name { <body> } }`. Non-`pub` so its
/// effects are *inferred* (a `pub` fn would have to declare them, and a
/// `String`-field default touches the allocator); this matches the
/// single-program v1 scope where `Name.default()` is called in-crate.
fn make_default_impl(type_name: &str, body: Expr, span: Span) -> Item {
    let ret_ty = TypeExpr {
        kind: TypeKind::Path(PathExpr {
            segments: vec![type_name.to_string()],
            generic_args: None,
            span: span.clone(),
        }),
        span: span.clone(),
    };
    let func = Function {
        span: span.clone(),
        attributes: Vec::new(),
        doc_comment: None,
        is_pub: false,
        is_private: false,
        is_unsafe: false,
        is_comptime: false,
        name: "default".to_string(),
        generic_params: None,
        params: Vec::new(),
        self_param: None,
        return_type: Some(ret_ty.clone()),
        effects: None,
        requires: Vec::new(),
        ensures: Vec::new(),
        where_clause: None,
        body: Block {
            stmts: Vec::new(),
            final_expr: Some(Box::new(body)),
            span: span.clone(),
        },
        stdlib_origin: false,
        deprecation: None,
        unstable: None,
        is_track_caller: false,
        is_gpu: false,
        inline_hint: None,
        is_cold: false,
        lint_overrides: Vec::new(),
        profile_compat: Vec::new(),
        abi: None,
    };
    Item::ImplBlock(ImplBlock {
        span: span.clone(),
        attributes: Vec::new(),
        generic_params: None,
        trait_name: None,
        target_type: ret_ty,
        where_clause: None,
        items: vec![ImplItem::Method(Box::new(func))],
        lint_overrides: Vec::new(),
        do_not_recommend: false,
    })
}

// ── parallel / destructuring assignment desugar ─────────────────
//
// `t1, ..., tn = v1, ..., vn;` (parsed as `StmtKind::MultiAssign`) is rewritten
// into a block-expr statement that binds every right-hand value to a fresh
// temporary (left to right) and then writes each target from its temporary:
//
//     { let _t0 = v0; ...; let _tn = vn; target0 = _t0; ...; targetn = _tn; }
//
// Evaluating all values before writing any target is what gives `a, b = b, a`
// its swap semantics. After this pass no `StmtKind::MultiAssign` remains, so
// every phase from the resolver onward treats it as ordinary `let`/`Assign`
// nodes. The formatter skips this pass, so it still sees — and round-trips —
// the surface node.

fn desugar_multi_assign_in_program(program: &mut Program) {
    for item in &mut program.items {
        match item {
            Item::Function(f) => walk_block(&mut f.body),
            Item::ImplBlock(imp) => {
                for it in &mut imp.items {
                    if let ImplItem::Method(m) = it {
                        walk_block(&mut m.body);
                    }
                }
            }
            Item::TraitDef(t) => {
                for it in &mut t.items {
                    if let TraitItem::Method(m) = it {
                        if let Some(body) = &mut m.body {
                            walk_block(body);
                        }
                    }
                }
            }
            Item::TestCase(tc) => walk_block(&mut tc.body),
            Item::ConstDecl(c) => walk_expr(&mut c.value),
            _ => {}
        }
    }
}

fn walk_block(block: &mut Block) {
    for stmt in &mut block.stmts {
        walk_stmt(stmt);
    }
    if let Some(e) = &mut block.final_expr {
        walk_expr(e);
    }
}

fn walk_stmt(stmt: &mut Stmt) {
    match &mut stmt.kind {
        StmtKind::Let { value, .. } => walk_expr(value),
        StmtKind::LetUninit { .. } => {}
        StmtKind::LetElse {
            value, else_block, ..
        } => {
            walk_expr(value);
            walk_block(else_block);
        }
        StmtKind::Defer { body } => walk_block(body),
        StmtKind::ErrDefer { body, .. } => walk_block(body),
        StmtKind::Assign { target, value } => {
            walk_expr(target);
            walk_expr(value);
        }
        StmtKind::CompoundAssign { target, value, .. } => {
            walk_expr(target);
            walk_expr(value);
        }
        StmtKind::Expr(e) => walk_expr(e),
        StmtKind::MultiAssign { .. } => {
            let span = stmt.span.clone();
            let placeholder = StmtKind::Expr(Expr {
                kind: ExprKind::Error,
                span: span.clone(),
            });
            let StmtKind::MultiAssign {
                mut targets,
                mut values,
            } = std::mem::replace(&mut stmt.kind, placeholder)
            else {
                unreachable!("matched MultiAssign above")
            };
            // Operands may themselves contain nested blocks (e.g. a block-expr
            // value) that hold further multi-assigns — recurse before expanding.
            for t in targets.iter_mut() {
                walk_expr(t);
            }
            for v in values.iter_mut() {
                walk_expr(v);
            }
            stmt.kind = expand_multi_assign(targets, values, span);
        }
    }
}

/// Build the block-expr `StmtKind` a parallel assignment lowers to. The
/// temporaries carry a `__karac_pa_<offset>_<i>` name that user code cannot
/// collide with and live only inside the synthesized block's scope.
fn expand_multi_assign(targets: Vec<Expr>, values: Vec<Expr>, span: Span) -> StmtKind {
    let n = targets.len();
    let mut stmts: Vec<Stmt> = Vec::with_capacity(n * 2);
    let mut temp_names: Vec<String> = Vec::with_capacity(n);
    for (i, value) in values.into_iter().enumerate() {
        let name = format!("__karac_pa_{}_{}", span.offset, i);
        let vspan = value.span.clone();
        temp_names.push(name.clone());
        stmts.push(Stmt {
            span: vspan.clone(),
            kind: StmtKind::Let {
                is_mut: false,
                pattern: Pattern {
                    kind: PatternKind::Binding(name),
                    span: vspan,
                },
                ty: None,
                value,
            },
        });
    }
    for (target, name) in targets.into_iter().zip(temp_names) {
        let tspan = target.span.clone();
        stmts.push(Stmt {
            span: tspan.clone(),
            kind: StmtKind::Assign {
                target,
                value: Expr {
                    kind: ExprKind::Identifier(name),
                    span: tspan,
                },
            },
        });
    }
    StmtKind::Expr(Expr {
        kind: ExprKind::Block(Block {
            stmts,
            final_expr: None,
            span: span.clone(),
        }),
        span,
    })
}

fn walk_expr(expr: &mut Expr) {
    match &mut expr.kind {
        // Leaves — no sub-expressions or blocks.
        ExprKind::Integer(..)
        | ExprKind::Float(..)
        | ExprKind::CharLit(..)
        | ExprKind::ByteLit(..)
        | ExprKind::StringLit(..)
        | ExprKind::MultiStringLit(..)
        | ExprKind::CStringLit { .. }
        | ExprKind::Bool(..)
        | ExprKind::Identifier(..)
        | ExprKind::Path { .. }
        | ExprKind::SelfValue
        | ExprKind::SelfType
        | ExprKind::PipePlaceholder
        | ExprKind::Continue { .. }
        | ExprKind::OffsetOf { .. }
        | ExprKind::Error => {}

        ExprKind::InterpolatedStringLit(parts) => {
            for part in parts.iter_mut() {
                if let ParsedInterpolationPart::Expr(e, _) = part {
                    walk_expr(e);
                }
            }
        }
        ExprKind::Binary { left, right, .. }
        | ExprKind::NilCoalesce { left, right }
        | ExprKind::Pipe { left, right } => {
            walk_expr(left);
            walk_expr(right);
        }
        ExprKind::Unary { operand, .. } => walk_expr(operand),
        ExprKind::Question(e) => walk_expr(e),
        ExprKind::OptionalChain { object, args, .. } => {
            walk_expr(object);
            if let Some(args) = args {
                for a in args.iter_mut() {
                    walk_expr(&mut a.value);
                }
            }
        }
        ExprKind::Call { callee, args } => {
            walk_expr(callee);
            for a in args.iter_mut() {
                walk_expr(&mut a.value);
            }
        }
        ExprKind::MethodCall { object, args, .. } => {
            walk_expr(object);
            for a in args.iter_mut() {
                walk_expr(&mut a.value);
            }
        }
        ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
            walk_expr(object)
        }
        ExprKind::Index { object, index } => {
            walk_expr(object);
            walk_expr(index);
        }
        ExprKind::Block(b) | ExprKind::Comptime(b) => walk_block(b),
        ExprKind::If {
            condition,
            then_block,
            else_branch,
        } => {
            walk_expr(condition);
            walk_block(then_block);
            if let Some(e) = else_branch {
                walk_expr(e);
            }
        }
        ExprKind::IfLet {
            value,
            then_block,
            else_branch,
            ..
        } => {
            walk_expr(value);
            walk_block(then_block);
            if let Some(e) = else_branch {
                walk_expr(e);
            }
        }
        ExprKind::Match { scrutinee, arms } => {
            walk_expr(scrutinee);
            for arm in arms.iter_mut() {
                if let Some(g) = &mut arm.guard {
                    walk_expr(g);
                }
                walk_expr(&mut arm.body);
            }
        }
        ExprKind::While {
            condition, body, ..
        } => {
            walk_expr(condition);
            walk_block(body);
        }
        ExprKind::WhileLet { value, body, .. } => {
            walk_expr(value);
            walk_block(body);
        }
        ExprKind::For { iterable, body, .. } => {
            walk_expr(iterable);
            walk_block(body);
        }
        ExprKind::Loop { body, .. } => walk_block(body),
        ExprKind::LabeledBlock { body, .. } => walk_block(body),
        ExprKind::Closure { body, .. } => walk_expr(body),
        ExprKind::Return(opt) => {
            if let Some(e) = opt {
                walk_expr(e);
            }
        }
        ExprKind::Break { value, .. } => {
            if let Some(e) = value {
                walk_expr(e);
            }
        }
        ExprKind::Tuple(items)
        | ExprKind::ArrayLiteral(items)
        | ExprKind::PrefixCollectionLiteral { items, .. } => {
            for e in items.iter_mut() {
                walk_expr(e);
            }
        }
        ExprKind::RepeatLiteral { value, count, .. } => {
            walk_expr(value);
            walk_expr(count);
        }
        ExprKind::MapLiteral(pairs) => {
            for (k, v) in pairs.iter_mut() {
                walk_expr(k);
                walk_expr(v);
            }
        }
        ExprKind::StructLiteral { fields, spread, .. } => {
            for f in fields.iter_mut() {
                walk_expr(&mut f.value);
            }
            if let Some(s) = spread {
                walk_expr(s);
            }
        }
        ExprKind::Cast { expr: e, .. } => walk_expr(e),
        ExprKind::Range { start, end, .. } => {
            if let Some(s) = start {
                walk_expr(s);
            }
            if let Some(e) = end {
                walk_expr(e);
            }
        }
        ExprKind::Unsafe(b) | ExprKind::Try(b) | ExprKind::Seq(b) | ExprKind::Par(b) => {
            walk_block(b)
        }
        ExprKind::Lock { mutex, body, .. } => {
            walk_expr(mutex);
            walk_block(body);
        }
        ExprKind::Providers { bindings, body } => {
            for bnd in bindings.iter_mut() {
                walk_expr(&mut bnd.value);
            }
            walk_block(body);
        }
    }
}

// ── `impl Trait` argument-position desugar ──────────────────────

fn desugar_impl_trait_args_in_program(program: &mut Program) {
    for item in &mut program.items {
        match item {
            Item::Function(f) => desugar_impl_trait_args_in_function(f),
            Item::ImplBlock(imp) => {
                for it in &mut imp.items {
                    if let ImplItem::Method(method) = it {
                        desugar_impl_trait_args_in_function(method);
                    }
                }
            }
            _ => {}
        }
    }
}

/// Rewrite every top-level `TypeKind::ImplTrait` on `f.params[i].ty` into a
/// `TypeKind::Path` reference to a freshly synthesized anonymous generic
/// parameter `T_impl_arg_N`, and append that parameter (with the original
/// trait as its bound) to `f.generic_params`. Per-occurrence: two
/// `impl T` parameters produce two distinct synthetic params so the
/// typechecker never unifies them.
///
/// Only top-level argument-position occurrences are desugared. Return-position
/// `impl Trait` (slice 3) and TAIT-RHS `impl Trait` (slice 6) are intentionally
/// left intact so the typechecker's slice-1 stub continues to surface them.
/// Nested-through-generic-args (`Vec[impl T]`) and trait-method argument
/// position were already rejected at parse (slice 1), so they never reach
/// this pass.
///
/// `use_effects` on argument-position `impl Trait` is dropped: per the parent
/// spec the argument-position desugar produces "the same bounds (no
/// existential, no special handling downstream)" — the `with E'` clause is
/// meaningful only on return-position existentials, where slice 3 + Phase 8
/// pick it up.
fn desugar_impl_trait_args_in_function(f: &mut Function) {
    let mut synthetic_params: Vec<GenericParam> = Vec::new();
    let mut counter = 0usize;
    for param in &mut f.params {
        let TypeKind::ImplTrait {
            trait_path,
            args,
            span: impl_trait_span,
            ..
        } = &param.ty.kind
        else {
            continue;
        };

        let synthetic_name = format!("T_impl_arg_{counter}");
        counter += 1;

        let bound = TraitBound {
            path: trait_path.segments.clone(),
            generic_args: if args.is_empty() {
                None
            } else {
                Some(args.clone())
            },
            span: impl_trait_span.clone(),
        };
        synthetic_params.push(GenericParam {
            name: synthetic_name.clone(),
            bounds: vec![bound],
            is_const: false,
            const_type: None,
            variance: Variance::Invariant,
            variance_span: None,
            is_variadic_shape: false,
            span: impl_trait_span.clone(),
        });

        let original_span = param.ty.span.clone();
        param.ty = TypeExpr {
            kind: TypeKind::Path(PathExpr {
                segments: vec![synthetic_name],
                generic_args: None,
                span: original_span.clone(),
            }),
            span: original_span,
        };
    }

    if synthetic_params.is_empty() {
        return;
    }

    match &mut f.generic_params {
        Some(existing) => existing.params.extend(synthetic_params),
        None => {
            let span = synthetic_params
                .first()
                .map(|p| p.span.clone())
                .unwrap_or_else(|| Span {
                    line: 0,
                    column: 0,
                    offset: 0,
                    length: 0,
                });
            f.generic_params = Some(GenericParams {
                params: synthetic_params,
                effect_params: Vec::new(),
                span,
            });
        }
    }
}
