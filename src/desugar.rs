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
    hoist_assoc_bindings_in_program(program);
    synthesize_default_impls(program);
    synthesize_trait_default_methods(program);
    propagate_codegen_hints(program);
    desugar_impl_trait_args_in_program(program);
    desugar_stmt_rewrites_in_program(program);
    desugar_multiversion_in_program(program);
    // Call-site default-parameter fill (B-2026-08-17-19). Runs LAST so calls
    // inside synthesized bodies (trait default methods, multiversion thunks)
    // are filled too.
    crate::default_args::fill_default_args_in_program(program);
    // Struct functional update (B-2026-08-21-18): `P { x: 1, ..base }` becomes
    // the explicit field copies it stands for. Runs LAST so a spread inside a
    // synthesized body (trait default method, multiversion thunk) is expanded
    // too, on the same rule as the default-arg fill above.
    crate::struct_spread::expand_struct_spreads(program);
}

/// Hoist every inline associated-type binding written inside a trait bound
/// (`[I: Iterator[Item = T]]`, `where I: Iterator[Item = T]`) into the
/// equivalent `WhereConstraint::AssocTypeEq` on the enclosing item
/// (B-2026-08-21-9).
///
/// design.md writes the inline form on 43 lines — the Vec/Map/SortedMap
/// stdlib method tables among them — and syntax.md's own prose uses it, but
/// syntax.md's `TRAIT_BOUND` grammar never defined it and the parser
/// implemented the grammar. The WHERE-CLAUSE spelling (`where I.Item = T`)
/// was already accepted and fully wired, so the surface syntax is lowered
/// onto it rather than given semantics of its own: after this pass no
/// `TraitBound::assoc_bindings` is ever non-empty, and every downstream
/// consumer — declaration-site validation, call-site discharge, the
/// resolver, the formatter, the catalog — sees only the constraint form it
/// already handles.
///
/// Runs FIRST in `desugar_program` so the later passes (which synthesize
/// items and rewrite `impl Trait` arguments) operate on already-hoisted
/// input.
fn hoist_assoc_bindings_in_program(program: &mut Program) {
    for item in &mut program.items {
        match item {
            Item::Function(f) => hoist_fn_assoc_bindings(f),
            Item::StructDef(s) => {
                hoist_bounds_into_where(&mut s.generic_params, &mut s.where_clause)
            }
            Item::EnumDef(e) => hoist_bounds_into_where(&mut e.generic_params, &mut e.where_clause),
            Item::TraitDef(t) => {
                hoist_bounds_into_where(&mut t.generic_params, &mut t.where_clause);
                for ti in &mut t.items {
                    if let TraitItem::Method(m) = ti {
                        hoist_bounds_into_where(&mut m.generic_params, &mut m.where_clause);
                    }
                }
            }
            Item::ImplBlock(b) => {
                hoist_bounds_into_where(&mut b.generic_params, &mut b.where_clause);
                for ii in &mut b.items {
                    if let ImplItem::Method(m) = ii {
                        hoist_fn_assoc_bindings(m);
                    }
                }
            }
            _ => {}
        }
    }
}

fn hoist_fn_assoc_bindings(f: &mut Function) {
    hoist_bounds_into_where(&mut f.generic_params, &mut f.where_clause);
}

/// Drain the bindings off every bound in `generic_params` and in the where
/// clause's own `TypeBound`s, appending one `AssocTypeEq` per binding.
///
/// The constraint's `type_name` is the parameter the bound applies to — `I`
/// for `I: Iterator[Item = T]` — which is why this runs here rather than in
/// `parse_trait_bound`: a bound does not know what it bounds.
fn hoist_bounds_into_where(
    generic_params: &mut Option<GenericParams>,
    where_clause: &mut Option<WhereClause>,
) {
    let mut hoisted: Vec<WhereConstraint> = Vec::new();

    if let Some(gp) = generic_params.as_mut() {
        for p in &mut gp.params {
            let owner = p.name.clone();
            for b in &mut p.bounds {
                drain_bound(&owner, b, &mut hoisted);
            }
        }
    }
    if let Some(wc) = where_clause.as_mut() {
        for c in &mut wc.constraints {
            if let WhereConstraint::TypeBound {
                type_name, bounds, ..
            } = c
            {
                let owner = type_name.clone();
                for b in bounds {
                    drain_bound(&owner, b, &mut hoisted);
                }
            }
        }
    }

    if hoisted.is_empty() {
        return;
    }
    match where_clause {
        Some(wc) => wc.constraints.extend(hoisted),
        // No `where` was written: synthesize one spanning the first
        // constraint, so diagnostics still point at real source.
        none => {
            let span = match &hoisted[0] {
                WhereConstraint::AssocTypeEq { span, .. } => *span,
                _ => Span::default(),
            };
            *none = Some(WhereClause {
                constraints: hoisted,
                span,
            });
        }
    }
}

fn drain_bound(owner: &str, bound: &mut TraitBound, out: &mut Vec<WhereConstraint>) {
    for binding in std::mem::take(&mut bound.assoc_bindings) {
        out.push(WhereConstraint::AssocTypeEq {
            type_name: owner.to_string(),
            assoc_name: binding.name,
            ty: binding.ty,
            span: binding.span,
        });
    }
}

/// Where a `#[multiversion]` function lives — decides how the dispatch thunk
/// names its variants at the call site.
#[derive(Clone, Copy)]
enum MvHost {
    /// A module-level free function; variants are called `name$feat(args)`.
    Free,
    /// An impl method with a `self` receiver; variants are called
    /// `self.name$feat(args)` (a method call re-forwarding the receiver).
    SelfMethod,
}

/// Desugar `#[multiversion(baseline, "avx2", "avx512f")]` functions into
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
/// Scope (v1.1): **free functions AND `self`-receiver impl methods**, each
/// generic or not (a generic variant carries its `#[target_feature]` through
/// monomorphization — `declare_mono_function` re-emits the attribute). Params
/// must be simple bindings so the thunk can forward them by name. Associated
/// impl functions (no `self`) are rejected in the parser (`E_MULTIVERSION_ON_ASSOC`)
/// and so never reach this pass carrying the attribute.
fn desugar_multiversion_in_program(program: &mut Program) {
    let mut synthesized_free: Vec<Item> = Vec::new();
    for item in program.items.iter_mut() {
        match item {
            Item::Function(f) => {
                for v in desugar_multiversion_fn(f, MvHost::Free) {
                    synthesized_free.push(Item::Function(v));
                }
            }
            Item::ImplBlock(imp) => {
                let mut synthesized_methods: Vec<ImplItem> = Vec::new();
                for it in imp.items.iter_mut() {
                    if let ImplItem::Method(m) = it {
                        // Only `self`-receiver methods are in scope; a no-`self`
                        // associated fn carrying `#[multiversion]` was rejected
                        // at parse, so there is nothing to act on for it here.
                        if m.self_param.is_some() {
                            for v in desugar_multiversion_fn(m, MvHost::SelfMethod) {
                                synthesized_methods.push(ImplItem::Method(Box::new(v)));
                            }
                        }
                    }
                }
                imp.items.extend(synthesized_methods);
            }
            _ => {}
        }
    }
    program.items.extend(synthesized_free);
}

/// Rewrite one `#[multiversion]` function `f` in place into its dispatch thunk
/// and return the freshly synthesized `$baseline` + per-feature variant clones
/// for the caller to splice next to it (as free items or impl methods per
/// `host`). Returns an empty vec — leaving `f` untouched — for a fn that does
/// not carry the attribute or is a malformed/out-of-scope shape the parser
/// already reported (empty feature list, non-binding params).
fn desugar_multiversion_fn(f: &mut Function, host: MvHost) -> Vec<Function> {
    let Some(features) = multiversion_feature_list(&f.attributes) else {
        return Vec::new();
    };
    if features.is_empty()
        || !f
            .params
            .iter()
            .all(|p| matches!(p.pattern.kind, PatternKind::Binding(_)))
    {
        return Vec::new();
    }
    let base = f.name.clone();
    let sp = f.span;
    // Forward each (non-self) param to the variant by name. `self` is carried
    // implicitly by the `self.name$feat(...)` receiver on the method path.
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
                mut_marker_span: None,
                value: mv_ident(n, sp),
                span: sp,
            }
        })
        .collect();

    let mut synthesized: Vec<Function> = Vec::new();

    // Baseline clone: plain (safe), no multiversion attr.
    let mut baseline_fn = f.clone();
    baseline_fn.name = format!("{base}$baseline");
    baseline_fn
        .attributes
        .retain(|a| !a.is_bare("multiversion"));
    baseline_fn
        .attributes
        .push(mv_allow_undocumented_unsafe_attr(sp));
    synthesized.push(baseline_fn);

    // Per-feature clone: unsafe + `#[target_feature(enable = "<feat>")]`.
    for feat in &features {
        let mut vf = f.clone();
        vf.name = format!("{base}${feat}");
        vf.is_unsafe = true;
        vf.attributes.retain(|a| !a.is_bare("multiversion"));
        vf.attributes.push(mv_target_feature_attr(feat, sp));
        vf.attributes.push(mv_allow_undocumented_unsafe_attr(sp));
        synthesized.push(vf);
    }

    // Rewrite `f` into the dispatch thunk. Build the nested if-else from the
    // inside out: innermost `else` = the baseline call; each feature (in
    // listed order) wraps the accumulator, so the LAST-listed feature ends
    // up outermost = checked first (list narrowest→widest per the design).
    f.attributes.retain(|a| !a.is_bare("multiversion"));
    f.attributes.push(mv_allow_undocumented_unsafe_attr(sp));
    let variant_call = |name: &str| -> Expr {
        match host {
            MvHost::Free => mv_call(name, &fwd, sp),
            MvHost::SelfMethod => mv_self_method_call(name, &fwd, sp),
        }
    };
    let mut acc = variant_call(&format!("{base}$baseline"));
    for (i, feat) in features.iter().enumerate() {
        let feat_call = variant_call(&format!("{base}${feat}"));
        let unsafe_call = Expr {
            kind: ExprKind::Unsafe(mv_block(feat_call, sp)),
            span: sp,
        };
        // Each `cpu.supports(...)` probe gets a DISTINCT span. In a generic body
        // the ownership checker tracks the namespace receiver `cpu` as an ordinary
        // binding, so two probes sharing one span collapse to "value `cpu` moved
        // here, used again here" (a same-place-reused false positive). Perturbing
        // the offset per feature keeps each `cpu` use a distinct program point —
        // exactly what hand-written `cpu.supports` calls on separate source lines
        // already are. No-op for the non-generic case (there `cpu` resolves to the
        // namespace and is never tracked).
        acc = Expr {
            kind: ExprKind::If {
                condition: Box::new(mv_cpu_supports(feat, mv_distinct_span(&sp, i))),
                then_block: mv_block(unsafe_call, sp),
                else_branch: Some(Box::new(acc)),
            },
            span: sp,
        };
    }
    f.body = mv_block(acc, sp);
    synthesized
}

/// A copy of `base` with its `offset`/`column` bumped by `i` so successive
/// synthesized nodes occupy distinct program points (see the ownership-checker
/// note at the `#[multiversion]` dispatch-thunk construction). The perturbation
/// stays within the original function's source range and never surfaces in a
/// diagnostic for correct code.
fn mv_distinct_span(base: &Span, i: usize) -> Span {
    Span {
        line: base.line,
        column: base.column + i,
        offset: base.offset + i,
        length: base.length,
    }
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
            callee: Box::new(mv_ident(name, span)),
            args: args.to_vec(),
        },
        span,
    }
}

/// `self.<method>(args)` — the receiver-forwarding call the method-path dispatch
/// thunk uses to reach a sibling `$baseline` / `$<feat>` variant method. `self`
/// (the thunk's own receiver, in whatever mode the method declared) is passed
/// implicitly as the call's object; only the non-self params are forwarded.
fn mv_self_method_call(method: &str, args: &[CallArg], span: Span) -> Expr {
    Expr {
        kind: ExprKind::MethodCall {
            object: Box::new(Expr {
                kind: ExprKind::SelfValue,
                span,
            }),
            method: method.to_string(),
            turbofish: None,
            args: args.to_vec(),
            args_close_span: span,
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
            object: Box::new(mv_ident("cpu", span)),
            method: "supports".to_string(),
            turbofish: None,
            args: vec![CallArg {
                label: None,
                mut_marker: false,
                mut_marker_span: None,
                value: Expr {
                    kind: ExprKind::StringLit(feat.to_string()),
                    span,
                },
                span,
            }],
            args_close_span: span,
        },
        span,
    }
}

/// `#[allow(undocumented_unsafe)]` — suppresses the `undocumented_unsafe` lint
/// on the compiler-synthesized multiversion family. The per-feature variants are
/// `unsafe fn`s (form-3 doc lint) and the dispatch thunk wraps each variant call
/// in a synthesized `unsafe { }` block (form-1 block lint); neither is
/// user-authored, so a "missing `# Safety`" warning on them is un-actionable
/// noise. Placed on every clone + the thunk. (A user's own `unsafe { }` inside a
/// `#[multiversion]` body is thereby also un-linted — an accepted trade for a hot
/// kernel whose whole point is the unsafe SIMD path.)
fn mv_allow_undocumented_unsafe_attr(span: Span) -> Attribute {
    Attribute {
        span,
        path: vec!["allow".to_string()],
        args: vec![AttrArg {
            name: None,
            value: Some(Expr {
                kind: ExprKind::Identifier("undocumented_unsafe".to_string()),
                span,
            }),
            span,
        }],
        string_value: None,
        effect_args: Vec::new(),
    }
}

fn mv_target_feature_attr(feat: &str, span: Span) -> Attribute {
    Attribute {
        span,
        path: vec!["target_feature".to_string()],
        args: vec![AttrArg {
            name: Some("enable".to_string()),
            value: Some(Expr {
                kind: ExprKind::StringLit(feat.to_string()),
                span,
            }),
            span,
        }],
        string_value: None,
        effect_args: Vec::new(),
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
pub(crate) fn subst_type_expr(
    te: &TypeExpr,
    subst: &std::collections::HashMap<String, TypeExpr>,
) -> TypeExpr {
    let kind = match &te.kind {
        TypeKind::Path(p) => {
            if p.segments.len() == 1 && p.generic_args.is_none() {
                if let Some(replacement) = subst.get(&p.segments[0]) {
                    // Substitute the whole node, keeping the reference's span.
                    return TypeExpr {
                        kind: replacement.kind.clone(),
                        span: te.span,
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
                span: p.span,
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
        span: te.span,
    }
}

/// Substitute trait params inside a `TraitBound`'s generic-args (the bound's
/// path/name is a trait name, never a type param, so it is left alone).
pub(crate) fn subst_trait_bound(
    b: &mut TraitBound,
    subst: &std::collections::HashMap<String, TypeExpr>,
) {
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
pub(crate) fn subst_where_constraint(
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
pub(crate) fn subst_leading_type_name(
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

pub(crate) fn subst_block(block: &mut Block, subst: &std::collections::HashMap<String, TypeExpr>) {
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
pub(crate) fn subst_expr(expr: &mut Expr, subst: &std::collections::HashMap<String, TypeExpr>) {
    match &mut expr.kind {
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
        span: m.span,
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
        // Trait methods carry no `frozen` receiver (stage 2.7 is impl-only).
        self_is_frozen: false,
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
        no_effect: Vec::new(),
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
                    synthesized.push(make_default_impl(&s.name, body, s.span));
                }
            }
            Item::EnumDef(e)
                if e.generic_params.is_none()
                    && derives_default(&e.attributes)
                    && !has_user_default.contains(&e.name) =>
            {
                if let Some(body) = enum_default_body(e) {
                    synthesized.push(make_default_impl(&e.name, body, e.span));
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
    let span = ty.span;
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
                span,
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
            span: f.span,
        });
    }
    Some(Expr {
        kind: ExprKind::StructLiteral {
            path: vec![s.name.clone()],
            fields,
            spread: None,
        },
        span: s.span,
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
        span: e.span,
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
            span,
        }),
        span,
    };
    let func = Function {
        span,
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
        self_is_frozen: false,
        return_type: Some(ret_ty.clone()),
        effects: None,
        requires: Vec::new(),
        ensures: Vec::new(),
        where_clause: None,
        body: Block {
            stmts: Vec::new(),
            final_expr: Some(Box::new(body)),
            span,
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
        no_effect: Vec::new(),
        abi: None,
    };
    Item::ImplBlock(ImplBlock {
        span,
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

/// The statement-level AST rewrites, sharing one walk of every block in the
/// program (they are independent, and two walkers would have to be kept in
/// sync with `ExprKind` forever):
///
///   * parallel / destructuring assignment (`a, b = b, a`), and
///   * `collect()` into a non-`Vec` `FromIterator` target (B-2026-08-17-36).
fn desugar_stmt_rewrites_in_program(program: &mut Program) {
    let sigs = collect_arg_sigs(program);
    for item in &mut program.items {
        match item {
            Item::Function(f) => {
                let ret = f.return_type.clone();
                let params = std::mem::take(&mut f.params);
                walk_fn_body(&mut f.body, ret.as_ref(), &params, &sigs);
                f.params = params;
            }
            Item::ImplBlock(imp) => {
                for it in &mut imp.items {
                    if let ImplItem::Method(m) = it {
                        let ret = m.return_type.clone();
                        let params = std::mem::take(&mut m.params);
                        walk_fn_body(&mut m.body, ret.as_ref(), &params, &sigs);
                        m.params = params;
                    }
                }
            }
            Item::TraitDef(t) => {
                for it in &mut t.items {
                    if let TraitItem::Method(m) = it {
                        let ret = m.return_type.clone();
                        let params = std::mem::take(&mut m.params);
                        if let Some(body) = &mut m.body {
                            walk_fn_body(body, ret.as_ref(), &params, &sigs);
                        }
                        m.params = params;
                    }
                }
            }
            Item::TestCase(tc) => walk_fn_body(&mut tc.body, None, &[], &sigs),
            Item::ConstDecl(c) => {
                // Not a function body: no parameters, and no local bindings to
                // stand down for. A fresh context with no collected names is
                // exactly right, and the arg-position pass is skipped because
                // `walk_fn_body`'s two-pass driver is what enables it.
                let mut cx = WalkCx::collecting();
                walk_expr(&mut c.value, None, &mut cx);
            }
            _ => {}
        }
    }
}

// ── Argument-position `collect()` target (B-2026-08-18-27) ──────

/// Top-level fn name → one entry per parameter: `Some(ty)` when that
/// parameter's declared type is a `collect()` target this pass can build,
/// `None` otherwise. Only functions with at least one `Some` are present, so a
/// lookup miss is the overwhelmingly common case and costs one hash.
type ArgSigs = std::collections::HashMap<String, Vec<Option<TypeExpr>>>;

/// Build [`ArgSigs`] from the program's top-level functions.
///
/// GENERIC FUNCTIONS ARE EXCLUDED WHOLESALE. The rewrite pastes the parameter's
/// written type into the caller as a `let` annotation, so a parameter spelled
/// `Set[T]` would land `let mut __dst: Set[T]` in a scope where `T` is not
/// bound — turning a working call into a resolve error. Discriminating "`Set[T]`
/// mentions a type parameter" from "`Set[i64]` happens to sit in a generic fn"
/// is a type walk this pass does not need: excluding every generic fn costs
/// only a missed rewrite, which is the pre-existing behaviour.
///
/// A DUPLICATED NAME POISONS THE ENTRY. Two same-named top-level fns are a
/// resolve error, but this pass runs before resolve and must not pick one.
fn collect_arg_sigs(program: &Program) -> ArgSigs {
    let mut sigs: ArgSigs = std::collections::HashMap::new();
    let mut poisoned: std::collections::HashSet<String> = std::collections::HashSet::new();
    for item in &program.items {
        let Item::Function(f) = item else {
            continue;
        };
        if f.generic_params.is_some() || f.self_param.is_some() || f.is_comptime {
            poisoned.insert(f.name.clone());
            continue;
        }
        let params: Vec<Option<TypeExpr>> = f
            .params
            .iter()
            .map(|p| collect_target_of(&p.ty).map(|_| p.ty.clone()))
            .collect();
        if sigs.insert(f.name.clone(), params).is_some() {
            poisoned.insert(f.name.clone());
        }
    }
    for name in poisoned {
        sigs.remove(&name);
    }
    sigs.retain(|_, params| params.iter().any(Option::is_some));
    sigs
}

/// State threaded through one function body's walk.
///
/// The walk runs TWICE over each body. Pass 1 (`args: None`) performs the
/// existing `let`/`return` rewrites and records every name the body binds. Pass
/// 2 (`args: Some(..)`) performs the argument-position rewrite, consulting the
/// names pass 1 collected.
///
/// TWO PASSES OF THE SAME TRAVERSAL, rather than a separate binding-collector,
/// is the load-bearing choice. A shadowing local can appear anywhere in the
/// body — including textually AFTER the call — so the decision needs the whole
/// body's binding set before any rewrite fires. A second traversal written by
/// hand would be a second place to forget an AST form; reusing this one means
/// any expression the rewrite can reach is an expression the collector already
/// reached. Both `walk_expr` matches are exhaustive over `ExprKind`, so a new
/// variant breaks the build rather than silently escaping either pass.
///
/// The `let`/`return` rewrites re-run on pass 2 and are idempotent: once a
/// value has become the accumulate-into-`T` `Block`, `desugar_collect_target`
/// no longer matches it.
struct WalkCx<'a> {
    /// Every name bound by a pattern anywhere in this body, plus its
    /// parameters. Complete before pass 2 begins.
    bound: std::collections::HashSet<String>,
    /// `None` on pass 1 (collect only), `Some` on pass 2 (rewrite).
    args: Option<&'a ArgSigs>,
    /// Did pass 1 see a `collect()` call anywhere in this body? Pass 2 has
    /// nothing to do otherwise, and most bodies contain no `collect()` at all —
    /// so without this the second traversal would be pure overhead on nearly
    /// every function, given that a plain `String` parameter is enough to put a
    /// function in `ArgSigs`.
    saw_collect: bool,
}

impl WalkCx<'_> {
    fn collecting() -> Self {
        WalkCx {
            bound: std::collections::HashSet::new(),
            args: None,
            saw_collect: false,
        }
    }

    fn bind_pattern(&mut self, p: &Pattern) {
        collect_binding_names(p, &mut self.bound);
    }
}

/// Every name a pattern binds. Exhaustive by construction — no catch-all arm,
/// so a new `PatternKind` breaks the build. `Slice` matters here and is the
/// reason [`crate::cfg::pattern_bindings`] is not reused: that helper's `_`
/// arm silently drops `[first, .., last]` and `[head, ..rest]` bindings, and a
/// dropped binding here would be a WRONG REWRITE rather than a missed one.
fn collect_binding_names(p: &Pattern, out: &mut std::collections::HashSet<String>) {
    match &p.kind {
        PatternKind::Binding(name) => {
            out.insert(name.clone());
        }
        PatternKind::AtBinding { name, pattern, .. } => {
            out.insert(name.clone());
            collect_binding_names(pattern, out);
        }
        PatternKind::Struct { fields, .. } => {
            for f in fields {
                match &f.pattern {
                    Some(sub) => collect_binding_names(sub, out),
                    // Shorthand `Foo { x }` binds the field name itself.
                    None => {
                        out.insert(f.name.clone());
                    }
                }
            }
        }
        PatternKind::Tuple(ps) | PatternKind::TupleVariant { patterns: ps, .. } => {
            for sub in ps {
                collect_binding_names(sub, out);
            }
        }
        PatternKind::Or(alts) => {
            for sub in alts {
                collect_binding_names(sub, out);
            }
        }
        PatternKind::Slice {
            prefix,
            rest,
            suffix,
        } => {
            for sub in prefix.iter().chain(suffix.iter()) {
                collect_binding_names(sub, out);
            }
            if let Some(RestPattern::Bound(name)) = rest {
                out.insert(name.clone());
            }
        }
        PatternKind::Wildcard | PatternKind::Literal(_) | PatternKind::RangePattern { .. } => {}
    }
}

/// The argument-position half of the rewrite, applied at a `Call` on pass 2.
///
/// WHY THE CALLEE MUST NOT BE A LOCALLY BOUND NAME: a `let f = |v| ...` closure
/// shadows a top-level `fn f` and is callable exactly the same way (verified —
/// the language allows it). Rewriting the argument against the top-level
/// signature would then aim at the wrong type and turn a program that compiles
/// today into a typecheck error. `cx.bound` is the whole body's binding set, so
/// any such shadow — before or after the call — stands the rewrite down.
///
/// The remaining conservative gates: only a bare `Identifier` callee (a path
/// call may cross modules, whose signatures this pass cannot see), only
/// all-positional arguments at exact arity (labels and defaults change which
/// parameter an argument lands on), and only the parameters `collect_arg_sigs`
/// already screened.
fn rewrite_call_arg_collects(
    callee: &Expr,
    args: &mut [CallArg],
    sigs: &ArgSigs,
    bound: &std::collections::HashSet<String>,
) {
    let ExprKind::Identifier(name) = &callee.kind else {
        return;
    };
    if bound.contains(name) {
        return;
    }
    let Some(param_tys) = sigs.get(name) else {
        return;
    };
    if param_tys.len() != args.len() || args.iter().any(|a| a.label.is_some()) {
        return;
    }
    for (arg, param_ty) in args.iter_mut().zip(param_tys) {
        if let Some(ty) = param_ty {
            let synth_base = arg.value.span;
            desugar_collect_target_at(ty, &mut arg.value, synth_base);
        }
    }
}

/// A FUNCTION BODY, whose trailing expression is the implicit return value.
///
/// B-2026-08-18-18 — split from `walk_block` because only this block's tail is
/// a return position. A nested block's tail is that block's value, and while
/// some of those are transitively returned too (an `if` in tail position), the
/// analysis to tell which is real tail-position reasoning. Restricting the
/// rewrite to an explicit `return` and the function body's own tail keeps the
/// pass syntactic and is what the two spellings in the row's repro use; the
/// rest still fails loudly at typecheck rather than silently building a `Vec`.
fn walk_fn_body(block: &mut Block, ret: Option<&TypeExpr>, params: &[Param], sigs: &ArgSigs) {
    let mut cx = WalkCx::collecting();
    for p in params {
        cx.bind_pattern(&p.pattern);
    }
    walk_block(block, ret, &mut cx);
    if let (Some(tail), Some(ty)) = (block.final_expr.as_mut(), ret) {
        desugar_collect_target(ty, tail);
    }

    // Pass 2 — argument position (B-2026-08-18-27). Skipped entirely when no
    // top-level fn in the program takes a `collect()`-able parameter, which is
    // the usual case.
    if sigs.is_empty() || !cx.saw_collect {
        return;
    }
    cx.args = Some(sigs);
    walk_block(block, ret, &mut cx);
}

fn walk_block(block: &mut Block, ret: Option<&TypeExpr>, cx: &mut WalkCx) {
    for stmt in &mut block.stmts {
        walk_stmt(stmt, ret, cx);
    }
    if let Some(e) = &mut block.final_expr {
        walk_expr(e, ret, cx);
    }
}

fn walk_stmt(stmt: &mut Stmt, ret: Option<&TypeExpr>, cx: &mut WalkCx) {
    match &mut stmt.kind {
        StmtKind::Let {
            pattern, ty, value, ..
        } => {
            cx.bind_pattern(pattern);
            // Recurse FIRST so a nested `let` inside the value (a closure body,
            // a block expr) is rewritten before this one wraps the value.
            walk_expr(value, ret, cx);
            if let Some(ty) = ty {
                desugar_collect_target(ty, value);
            }
        }
        StmtKind::LetUninit { name, .. } => {
            cx.bound.insert(name.clone());
        }
        StmtKind::LetElse {
            pattern,
            value,
            else_block,
            ..
        } => {
            cx.bind_pattern(pattern);
            walk_expr(value, ret, cx);
            walk_block(else_block, ret, cx);
        }
        StmtKind::Defer { body } => walk_block(body, ret, cx),
        StmtKind::ErrDefer { binding, body } => {
            if let Some(name) = binding {
                cx.bound.insert(name.clone());
            }
            walk_block(body, ret, cx);
        }
        StmtKind::Assign { target, value } => {
            walk_expr(target, ret, cx);
            walk_expr(value, ret, cx);
        }
        StmtKind::CompoundAssign { target, value, .. } => {
            walk_expr(target, ret, cx);
            walk_expr(value, ret, cx);
        }
        StmtKind::Expr(e) => walk_expr(e, ret, cx),
        StmtKind::MultiAssign { .. } => {
            let span = stmt.span;
            let placeholder = StmtKind::Expr(Expr {
                kind: ExprKind::Error,
                span,
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
                walk_expr(t, ret, cx);
            }
            for v in values.iter_mut() {
                walk_expr(v, ret, cx);
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
        let vspan = value.span;
        temp_names.push(name.clone());
        stmts.push(Stmt {
            span: vspan,
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
        let tspan = target.span;
        stmts.push(Stmt {
            span: tspan,
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
            span,
        }),
        span,
    })
}

fn walk_expr(expr: &mut Expr, ret: Option<&TypeExpr>, cx: &mut WalkCx) {
    match &mut expr.kind {
        // Leaves — no sub-expressions or blocks.
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
                    walk_expr(e, ret, cx);
                }
            }
        }
        ExprKind::Binary { left, right, .. }
        | ExprKind::NilCoalesce { left, right }
        | ExprKind::Pipe { left, right } => {
            walk_expr(left, ret, cx);
            walk_expr(right, ret, cx);
        }
        ExprKind::Unary { operand, .. } => walk_expr(operand, ret, cx),
        ExprKind::Question(e) => walk_expr(e, ret, cx),
        ExprKind::OptionalChain { object, args, .. } => {
            walk_expr(object, ret, cx);
            if let Some(args) = args {
                for a in args.iter_mut() {
                    walk_expr(&mut a.value, ret, cx);
                }
            }
        }
        ExprKind::Call { callee, args } => {
            walk_expr(callee, ret, cx);
            for a in args.iter_mut() {
                walk_expr(&mut a.value, ret, cx);
            }
            // B-2026-08-18-27 — `f(<chain>.collect())` against a non-`Vec`
            // parameter. Pass 2 only: the decision needs the whole body's
            // binding set, which pass 1 is what collects.
            if let Some(sigs) = cx.args {
                rewrite_call_arg_collects(callee, args, sigs, &cx.bound);
            }
        }
        ExprKind::MethodCall {
            object,
            method,
            args,
            ..
        } => {
            if method == "collect" {
                cx.saw_collect = true;
            }
            walk_expr(object, ret, cx);
            for a in args.iter_mut() {
                walk_expr(&mut a.value, ret, cx);
            }
        }
        ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
            walk_expr(object, ret, cx)
        }
        ExprKind::Index { object, index } => {
            walk_expr(object, ret, cx);
            walk_expr(index, ret, cx);
        }
        ExprKind::Block(b) | ExprKind::Comptime(b) => walk_block(b, ret, cx),
        ExprKind::If {
            condition,
            then_block,
            else_branch,
        } => {
            walk_expr(condition, ret, cx);
            walk_block(then_block, ret, cx);
            if let Some(e) = else_branch {
                walk_expr(e, ret, cx);
            }
        }
        ExprKind::IfLet {
            pattern,
            value,
            then_block,
            else_branch,
        } => {
            cx.bind_pattern(pattern);
            walk_expr(value, ret, cx);
            walk_block(then_block, ret, cx);
            if let Some(e) = else_branch {
                walk_expr(e, ret, cx);
            }
        }
        ExprKind::Match { scrutinee, arms } => {
            walk_expr(scrutinee, ret, cx);
            for arm in arms.iter_mut() {
                cx.bind_pattern(&arm.pattern);
                if let Some(g) = &mut arm.guard {
                    walk_expr(g, ret, cx);
                }
                walk_expr(&mut arm.body, ret, cx);
            }
        }
        ExprKind::While {
            condition, body, ..
        } => {
            walk_expr(condition, ret, cx);
            walk_block(body, ret, cx);
        }
        ExprKind::WhileLet {
            pattern,
            value,
            body,
            ..
        } => {
            cx.bind_pattern(pattern);
            walk_expr(value, ret, cx);
            walk_block(body, ret, cx);
        }
        ExprKind::For {
            pattern,
            iterable,
            body,
            ..
        } => {
            cx.bind_pattern(pattern);
            walk_expr(iterable, ret, cx);
            walk_block(body, ret, cx);
        }
        ExprKind::Loop { body, .. } => walk_block(body, ret, cx),
        ExprKind::LabeledBlock { body, .. } => walk_block(body, ret, cx),
        // A `return` inside a closure returns from the CLOSURE, not the
        // enclosing fn, so the fn's declared return type must NOT reach here —
        // rewriting against it would target the wrong type exactly where it
        // looks like it works. Closures declare no return type of their own, so
        // there is nothing to substitute: the target is simply dropped.
        ExprKind::Closure { params, body, .. } => {
            for cp in params.iter() {
                cx.bind_pattern(&cp.pattern);
            }
            walk_expr(body, None, cx);
        }
        ExprKind::Return(opt) => {
            if let Some(e) = opt {
                walk_expr(e, ret, cx);
                // B-2026-08-18-18 — `return <chain>.collect()` against a
                // declared non-`Vec` return type, the sibling of the
                // annotated-`let` form. Same syntactic rewrite, same
                // by-construction backend parity; the only new input is which
                // type to aim at, and `ret` is `None` inside a closure.
                if let Some(ty) = ret {
                    desugar_collect_target(ty, e);
                }
            }
        }
        ExprKind::Break { value, .. } => {
            if let Some(e) = value {
                walk_expr(e, ret, cx);
            }
        }
        ExprKind::Tuple(items)
        | ExprKind::ArrayLiteral(items)
        | ExprKind::PrefixCollectionLiteral { items, .. } => {
            for e in items.iter_mut() {
                walk_expr(e, ret, cx);
            }
        }
        ExprKind::RepeatLiteral { value, count, .. } => {
            walk_expr(value, ret, cx);
            walk_expr(count, ret, cx);
        }
        ExprKind::MapLiteral(pairs) => {
            for (k, v) in pairs.iter_mut() {
                walk_expr(k, ret, cx);
                walk_expr(v, ret, cx);
            }
        }
        ExprKind::StructLiteral { fields, spread, .. } => {
            for f in fields.iter_mut() {
                walk_expr(&mut f.value, ret, cx);
            }
            if let Some(s) = spread {
                walk_expr(s, ret, cx);
            }
        }
        ExprKind::Cast { expr: e, .. } => walk_expr(e, ret, cx),
        ExprKind::Range { start, end, .. } => {
            if let Some(s) = start {
                walk_expr(s, ret, cx);
            }
            if let Some(e) = end {
                walk_expr(e, ret, cx);
            }
        }
        ExprKind::Unsafe(b) | ExprKind::Try(b) | ExprKind::Seq(b) | ExprKind::Par(b) => {
            walk_block(b, ret, cx)
        }
        ExprKind::Lock { mutex, body, .. } => {
            walk_expr(mutex, ret, cx);
            walk_block(body, ret, cx);
        }
        ExprKind::Providers { bindings, body } => {
            for bnd in bindings.iter_mut() {
                walk_expr(&mut bnd.value, ret, cx);
            }
            walk_block(body, ret, cx);
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
    let mut hoisted: Vec<WhereConstraint> = Vec::new();
    let mut counter = 0usize;
    for param in &mut f.params {
        let TypeKind::ImplTrait {
            trait_path,
            args,
            assoc_bindings,
            span: impl_trait_span,
            ..
        } = &param.ty.kind
        else {
            continue;
        };

        let synthetic_name = format!("T_impl_arg_{counter}");
        counter += 1;

        // An inline associated-type binding on an ARGUMENT-position
        // `impl Trait` is the bound-position case wearing different syntax:
        // the desugar is about to give the parameter a name, and a binding on
        // a named parameter is exactly `WhereConstraint::AssocTypeEq`
        // (B-2026-08-22-4). Emitted directly rather than left on the synthetic
        // bound because `hoist_assoc_bindings_in_program` already ran — it is
        // the FIRST pass in `desugar_program`, precisely so the later
        // synthesizing passes see hoisted input.
        //
        // Return-position `impl Trait` is NOT covered here: it has no name to
        // constrain, so its bindings ride the existential itself through
        // `lower_type_expr` into `Type::Existential::assoc_bindings`.
        for binding in assoc_bindings {
            hoisted.push(WhereConstraint::AssocTypeEq {
                type_name: synthetic_name.clone(),
                assoc_name: binding.name.clone(),
                ty: binding.ty.clone(),
                span: binding.span,
            });
        }

        let bound = TraitBound {
            path: trait_path.segments.clone(),
            generic_args: if args.is_empty() {
                None
            } else {
                Some(args.clone())
            },
            // Drained into `hoisted` above, in the one form the rest of the
            // compiler understands.
            assoc_bindings: Vec::new(),
            span: *impl_trait_span,
        };
        synthetic_params.push(GenericParam {
            name: synthetic_name.clone(),
            bounds: vec![bound],
            is_const: false,
            const_type: None,
            variance: Variance::Invariant,
            variance_span: None,
            is_variadic_shape: false,
            span: *impl_trait_span,
        });

        let original_span = param.ty.span;
        param.ty = TypeExpr {
            kind: TypeKind::Path(PathExpr {
                segments: vec![synthetic_name],
                generic_args: None,
                span: original_span,
            }),
            span: original_span,
        };
    }

    if synthetic_params.is_empty() {
        return;
    }

    if !hoisted.is_empty() {
        match &mut f.where_clause {
            Some(wc) => wc.constraints.extend(hoisted),
            none => {
                let span = match &hoisted[0] {
                    WhereConstraint::AssocTypeEq { span, .. } => *span,
                    _ => Span::default(),
                };
                *none = Some(WhereClause {
                    constraints: hoisted,
                    span,
                });
            }
        }
    }

    match &mut f.generic_params {
        Some(existing) => existing.params.extend(synthetic_params),
        None => {
            let span = synthetic_params
                .first()
                .map(|p| p.span)
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

// ── `collect()` into a non-`Vec` FromIterator target (B-2026-08-17-36) ──────

/// The non-`Vec` `collect()` targets design.md § Iterator Adaptors promises:
/// "Every standard collection (`Vec`, `Map`, `Set`, `VecDeque`, `TreeMap`,
/// `String`) implements `FromIterator` for its natural element type."
///
/// `TreeMap` is the sixth and has no arm here: it cannot be NAMED yet
/// (B-2026-08-17-38), so a `TreeMap` annotation fails before this pass can see
/// it. When that row lands, `TreeMap` joins `Map` below with the same body.
#[derive(Clone, Copy)]
enum CollectTarget {
    Str,
    Set,
    VecDeque,
    Map,
}

/// Recognize a supported `collect()` target from a `let`'s type annotation.
/// Arity is part of the match so a malformed `Set[K, V]` falls through to the
/// normal (unchanged) typecheck error rather than desugaring into nonsense.
fn collect_target_of(ty: &TypeExpr) -> Option<CollectTarget> {
    let TypeKind::Path(p) = &ty.kind else {
        return None;
    };
    if p.segments.len() != 1 {
        return None;
    }
    let nargs = p.generic_args.as_ref().map_or(0, |a| a.len());
    match (p.segments[0].as_str(), nargs) {
        ("String", 0) => Some(CollectTarget::Str),
        ("Set", 1) => Some(CollectTarget::Set),
        ("VecDeque", 1) => Some(CollectTarget::VecDeque),
        ("Map", 2) => Some(CollectTarget::Map),
        _ => None,
    }
}

/// Synthesized-node span: zero LENGTH at a distinct offset.
///
/// Length zero is the load-bearing half. A `MethodCall`'s `Expr.span` covers
/// only its RECEIVER, so every node of `v.iter().map(f).collect()` already
/// shares one `SpanKey` — the base identifier's. Handing a synthesized node any
/// non-zero length near that offset would risk landing on a real node's key and
/// silently overwriting its recorded type (the collision class B-2026-08-18-7
/// and B-2026-08-18-9 were both about). Real nodes always span at least one
/// character, so a zero-length span cannot collide with one, and distinct `i`
/// keeps the synthesized nodes distinct from each other.
///
/// The offset stays inside the file so diagnostic rendering (which slices
/// `source[offset .. offset + length]`) stays in range and yields "".
fn collect_synth_span(base: &Span, i: usize) -> Span {
    Span {
        line: base.line,
        column: base.column,
        offset: base.offset + i,
        length: 0,
    }
}

fn collect_ident(name: &str, span: Span) -> Expr {
    Expr {
        kind: ExprKind::Identifier(name.to_string()),
        span,
    }
}

fn collect_arg(value: Expr) -> CallArg {
    let span = value.span;
    CallArg {
        label: None,
        mut_marker: false,
        mut_marker_span: None,
        value,
        span,
    }
}

fn collect_method_call(object: Expr, method: &str, args: Vec<CallArg>, span: Span) -> Expr {
    Expr {
        kind: ExprKind::MethodCall {
            object: Box::new(object),
            method: method.to_string(),
            turbofish: None,
            args,
            args_close_span: span,
        },
        span,
    }
}

/// Methods that produce or transform an iterator, so a `collect()` on top of
/// one is the `FromIterator` terminal rather than a same-named user method.
/// Deliberately a name whitelist: this pass runs BEFORE typecheck and so has no
/// types to consult, and being wrong in the permissive direction here would
/// rewrite a user's own `collect()`.
const ITER_CHAIN_METHODS: &[&str] = &[
    // sources
    "iter",
    "iter_mut",
    "into_iter",
    "chars",
    "bytes",
    "lines",
    "values",
    "keys",
    "entries",
    "drain",
    "windows",
    "chunks",
    "split",
    "splitn",
    "char_indices",
    // adaptors
    "chain",
    "chunk_by",
    "cycle",
    "enumerate",
    "filter",
    "filter_map",
    "flat_map",
    "flatten",
    "inspect",
    "map",
    "peekable",
    "rev",
    "scan",
    "step_by",
    "zip",
    "take",
    "take_while",
    "skip",
    "skip_while",
    "copied",
    "cloned",
    "dedup",
    "unique",
    "sorted",
];

/// Is `e` the receiver of a `collect()` that is genuinely an iterator? A range
/// (`(0..n).collect()`) is one directly; otherwise the receiver must itself be
/// a call to a known iterator source or adaptor.
fn is_iterator_chain(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Range { .. } => true,
        ExprKind::MethodCall { method, .. } => ITER_CHAIN_METHODS.contains(&method.as_str()),
        _ => false,
    }
}

/// Rewrite `let x: T = <chain>.collect();` for a non-`Vec` target `T` into the
/// accumulate-into-`T` block the language already supports:
///
/// ```text
/// let x: Set[i64] = {
///     let __karac_collect_src_N = <chain>.collect();      // still Vec[E]
///     let mut __karac_collect_dst_N: Set[i64] = Set.new();
///     for __karac_collect_it_N in __karac_collect_src_N {
///         __karac_collect_dst_N.insert(__karac_collect_it_N);
///     }
///     __karac_collect_dst_N
/// };
/// ```
///
/// WHY A PRE-TYPECHECK DESUGAR rather than a `FromIterator` target inferred in
/// the typechecker: every phase downstream — typecheck, effects, ownership, the
/// interpreter, and codegen — then sees ordinary, already-supported code, so
/// `--interp` and both compiled backends agree BY CONSTRUCTION instead of by
/// three parallel implementations kept in sync. It also means no new span-keyed
/// side table, which is the machinery that made this class of feature expensive
/// before.
///
/// The `String` arm appends `it.to_string()` rather than branching on the
/// element type, because a pre-typecheck pass cannot know whether the element
/// is a `char` (`s.chars()....collect()`) or a `String`
/// (`v.iter().map(|s| s.to_uppercase()).collect()`) — and `push_str` accepts
/// the `to_string()` of both, while `+` accepts neither uniformly. The extra
/// `to_string()` on an already-`String` element is a copy; correctness and
/// backend parity are worth it here, and a later type-directed pass can drop it.
fn desugar_collect_target(ty: &TypeExpr, value: &mut Expr) {
    desugar_collect_target_at(ty, value, ty.span);
}

/// [`desugar_collect_target`] with the base for the SYNTHESIZED nodes' spans
/// given separately from the target type.
///
/// The two coincide for a `let` annotation and a `return` type — the written
/// type sits at a unique place in the source, so `ty.span` distinguishes one
/// rewrite from every other. In ARGUMENT position it does not: the target type
/// is the CALLEE'S PARAMETER, one span shared by every call site of that
/// function. Deriving the synthetic spans from it would give two call sites the
/// same `SpanKey`s, and a `String` target makes that a real miscompile rather
/// than a tidiness issue — `s.chars()...collect()` at one site and
/// `v.iter().map(to_upper)...collect()` at another record `char` and `String`
/// for the same element key, last write wins. Callers in argument position pass
/// the ARGUMENT's own span instead, which is unique per call.
///
/// The BLOCK keeps `ty.span` in both cases: no expression is recorded at a type
/// annotation's span, and two call sites recording the same target type under
/// one key is a harmless agreement rather than a collision.
fn desugar_collect_target_at(ty: &TypeExpr, value: &mut Expr, synth_base: Span) {
    let Some(target) = collect_target_of(ty) else {
        return;
    };
    // Only a bare zero-arg `collect()` whose RECEIVER is recognizably an
    // iterator. `collect` is an ordinary method name, so a user type may define
    // its own returning one of these very targets — rewriting that would
    // silently re-iterate a finished collection instead of building one. Gating
    // on the receiver keeps this closed: an unrecognized chain is left exactly
    // as it is today (the annotation still fails to typecheck), which is a safe
    // failure rather than a wrong one.
    match &value.kind {
        ExprKind::MethodCall {
            method,
            args,
            object,
            ..
        } if method == "collect" && args.is_empty() && is_iterator_chain(object) => {}
        _ => return,
    }

    let base = synth_base;
    let src_name = format!("__karac_collect_src_{}", base.offset);
    let dst_name = format!("__karac_collect_dst_{}", base.offset);
    let it_name = format!("__karac_collect_it_{}", base.offset);

    // The block itself takes the ANNOTATION's span: it is the node whose type
    // is the target, so a diagnostic about the target points at the written
    // target, and no EXPRESSION is recorded at a type annotation's span.
    let block_span = ty.span;
    let s1 = collect_synth_span(&base, 1);
    let s2 = collect_synth_span(&base, 2);
    let s3 = collect_synth_span(&base, 3);
    let s4 = collect_synth_span(&base, 4);
    let s5 = collect_synth_span(&base, 5);
    let s6 = collect_synth_span(&base, 6);
    let s7 = collect_synth_span(&base, 7);
    let s8 = collect_synth_span(&base, 8);
    let s9 = collect_synth_span(&base, 9);

    // `let __src = <the original collect call>;` — untouched, so the chain keeps
    // its own spans and its `Vec[E]` typing.
    let placeholder = Expr {
        kind: ExprKind::Error,
        span: value.span,
    };
    let original = std::mem::replace(value, placeholder);
    let src_stmt = Stmt {
        span: s1,
        kind: StmtKind::Let {
            is_mut: false,
            pattern: Pattern {
                kind: PatternKind::Binding(src_name.clone()),
                span: s1,
            },
            ty: None,
            value: original,
        },
    };

    // `let mut __dst: T = <ctor>;`
    let ctor = match target {
        CollectTarget::Str => Expr {
            kind: ExprKind::StringLit(String::new()),
            span: s2,
        },
        CollectTarget::Set | CollectTarget::VecDeque | CollectTarget::Map => {
            let coll = match target {
                CollectTarget::Set => "Set",
                CollectTarget::VecDeque => "VecDeque",
                _ => "Map",
            };
            Expr {
                kind: ExprKind::Call {
                    callee: Box::new(Expr {
                        kind: ExprKind::Path {
                            segments: vec![coll.to_string(), "new".to_string()],
                            generic_args: None,
                        },
                        span: s2,
                    }),
                    args: Vec::new(),
                },
                span: s2,
            }
        }
    };
    let dst_stmt = Stmt {
        span: s3,
        kind: StmtKind::Let {
            is_mut: true,
            pattern: Pattern {
                kind: PatternKind::Binding(dst_name.clone()),
                span: s3,
            },
            ty: Some(ty.clone()),
            value: ctor,
        },
    };

    // The per-element append.
    let append = match target {
        CollectTarget::Str => {
            let as_string =
                collect_method_call(collect_ident(&it_name, s4), "to_string", Vec::new(), s5);
            collect_method_call(
                collect_ident(&dst_name, s6),
                "push_str",
                vec![collect_arg(as_string)],
                s7,
            )
        }
        CollectTarget::Set => collect_method_call(
            collect_ident(&dst_name, s6),
            "insert",
            vec![collect_arg(collect_ident(&it_name, s4))],
            s7,
        ),
        CollectTarget::VecDeque => collect_method_call(
            collect_ident(&dst_name, s6),
            "push_back",
            vec![collect_arg(collect_ident(&it_name, s4))],
            s7,
        ),
        CollectTarget::Map => {
            // The element is the `(K, V)` pair every `Map` FromIterator takes.
            let k = Expr {
                kind: ExprKind::TupleIndex {
                    object: Box::new(collect_ident(&it_name, s4)),
                    index: 0,
                },
                span: s4,
            };
            let v = Expr {
                kind: ExprKind::TupleIndex {
                    object: Box::new(collect_ident(&it_name, s5)),
                    index: 1,
                },
                span: s5,
            };
            collect_method_call(
                collect_ident(&dst_name, s6),
                "insert",
                vec![collect_arg(k), collect_arg(v)],
                s7,
            )
        }
    };

    let for_stmt = Stmt {
        span: s8,
        kind: StmtKind::Expr(Expr {
            kind: ExprKind::For {
                label: None,
                pattern: Pattern {
                    kind: PatternKind::Binding(it_name),
                    span: s4,
                },
                iterable: Box::new(collect_ident(&src_name, s1)),
                body: Block {
                    stmts: vec![Stmt {
                        span: s7,
                        kind: StmtKind::Expr(append),
                    }],
                    final_expr: None,
                    span: s8,
                },
                attributes: Vec::new(),
            },
            span: s8,
        }),
    };

    *value = Expr {
        kind: ExprKind::Block(Block {
            stmts: vec![src_stmt, dst_stmt, for_stmt],
            // `s9`, NOT `s6` — the block's tail must not share a span with the
            // `__dst` receiver inside the loop. When it did, the ownership CFG
            // saw the tail's MOVE and the loop body's USE at one `SpanKey` and
            // could not order them, so every `collect()` into a non-`Vec`
            // target reported `perf[rc-fallback]: RC fallback inserted for
            // '__karac_collect_dst_N' (direct re-use after consume)` — a note
            // naming a synthesized binding the user never wrote, attached to an
            // RC box the code does not need. The hand-written equivalent of
            // this same block checks clean, which is what identified the shared
            // span rather than the shape as the cause.
            final_expr: Some(Box::new(collect_ident(&dst_name, s9))),
            span: block_span,
        }),
        span: block_span,
    };
}
