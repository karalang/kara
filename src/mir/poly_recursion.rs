//! Polymorphic recursion is an error (design.md §8 Monomorphization,
//! B-2026-11-06-12).
//!
//! A generic whose recursive path calls it again at a type argument that
//! strictly contains its own parameter (`deep[T]` calling `deep([x], ..)`
//! at `Vec[T]`) would need an instance per nesting depth, and the builder,
//! which makes one instance per argument tuple, would never finish. The
//! cycle is found here in the call graph, before any instance is made, so
//! the check itself always ends.
//!
//! The graph's nodes are generic parameters: `(f, i)` is the `i`-th
//! positional parameter of `f` (an impl's first). A call in `f`'s body to
//! a generic `g` whose `j`-th type argument holds `f`'s `i`-th parameter
//! `d` levels down gives an edge `(f, i) -> (g, j)` of weight `d` (`T` is
//! 0, `Vec[T]` and `(T, i64)` are 1). A method call that dispatches on a
//! receiver of a parameter's type (`u.grow(n)` with `u: U`) reaches every
//! impl of that method that could take the receiver; the impl's parameter
//! sits `e` levels down in its target (`T` of `impl[T] .. for Vec[T]` is
//! 1), so that edge weighs `d - e`. The weight bounds how much deeper the
//! callee's argument is than the caller's, so the instances are finite
//! exactly when no cycle weighs more than 0; each call of positive weight
//! that closes such a cycle is refused. Recursion at the same parameter
//! (`cnt[T]` calling `cnt(v, ..)`), at an unrelated type, or through a
//! helper that a dispatch peels back (`Tree[T]`'s method passing its
//! children to a `show_all[U]` that calls `show` on each `U`) stays legal.

use rustc_hash::FxHashMap;

use crate::ast::{self, Expr, ExprKind, Function, TypeExpr, TypeKind as AstTy};
use crate::ids::{DefId, NodeId};
use crate::token::Span;
use crate::ty::{Ty, TyCtxt, TyKind};
use crate::typed_hir::{Callee, ResolvedCall};

/// One generic function: its definition, its AST, how many positional
/// generic parameters it has (an impl's included), and, for a method of a
/// generic impl, what the impl is for.
pub struct GenericFn<'a> {
    pub def: DefId,
    pub f: &'a Function,
    pub params: usize,
    /// The impl's own parameters, which come first.
    pub impl_params: usize,
    /// The impl's parameters by name, in order.
    pub impl_names: Vec<&'a str>,
    /// The impl's target as written (`Vec[T]`), and its definition.
    pub target_ty: Option<&'a TypeExpr>,
    pub target: Option<DefId>,
}

/// A growing call on a cycle: where it is, and what to say.
pub struct Refusal {
    pub span: Span,
    pub message: String,
}

struct Edge {
    from: usize,
    to: usize,
    weight: i64,
    span: Span,
    callee: DefId,
    arg: Ty,
    param: Ty,
}

/// What the call graph needs from the typed program.
pub struct Typed<'a> {
    pub calls: &'a FxHashMap<NodeId, ResolvedCall>,
    pub node_types: &'a FxHashMap<NodeId, Ty>,
    pub tcx: &'a TyCtxt,
}

/// The growing calls on a cycle of `fns`' call graph, in source order.
pub fn refusals(
    fns: &[GenericFn<'_>],
    typed: &Typed<'_>,
    name: &dyn Fn(DefId) -> String,
) -> Vec<Refusal> {
    let tcx = typed.tcx;
    // Node numbering: each function's parameters in a run.
    let mut base: FxHashMap<DefId, usize> = FxHashMap::default();
    let mut owner: Vec<(DefId, usize)> = Vec::new();
    for g in fns {
        base.insert(g.def, owner.len());
        for i in 0..g.params {
            owner.push((g.def, i));
        }
    }
    let mut edges: Vec<Edge> = Vec::new();
    for g in fns {
        let from_base = base[&g.def];
        let mut visit = |e: &Expr| {
            let resolved = typed.calls.get(&e.id).and_then(|rc| match rc.callee {
                Callee::Def(d) => base.get(&d).map(|&b| (rc, d, b)),
                _ => None,
            });
            if let Some((rc, callee, to_base)) = resolved {
                for (j, arg) in tcx.list(rc.substs).into_iter().enumerate() {
                    for (i, depth) in params_in(tcx, arg) {
                        if (i as usize) < g.params {
                            edges.push(Edge {
                                from: from_base + i as usize,
                                to: to_base + j,
                                weight: depth,
                                span: e.span,
                                callee,
                                arg,
                                param: param_of(tcx, arg, i),
                            });
                        }
                    }
                }
                return;
            }
            // A method the receiver's type picks when the instance is made.
            let ExprKind::MethodCall { object, method, .. } = &e.kind else {
                return;
            };
            let Some(&recv) = typed.node_types.get(&object.id) else {
                return;
            };
            let recv = peel_refs(tcx, recv);
            let held = params_in(tcx, recv);
            if held.is_empty() {
                return;
            }
            let head = match tcx.kind(recv) {
                TyKind::Adt { def, .. } | TyKind::Shared { def, .. } => Some(def),
                _ => None,
            };
            let bare = matches!(tcx.kind(recv), TyKind::Param(_));
            for c in fns {
                if c.impl_params == 0 || c.f.self_param.is_none() || c.f.name != *method {
                    continue;
                }
                let blanket = c.target_ty.is_some_and(|t| bare_param(t, &c.impl_names));
                if !bare && !blanket && (head.is_none() || c.target != head) {
                    continue;
                }
                for (k, n) in c.impl_names.iter().enumerate().take(c.impl_params) {
                    // Unknown depth is taken as 0: a heavier edge, never a
                    // lighter one.
                    let e_k = c.target_ty.and_then(|t| depth_in(t, n)).unwrap_or(0);
                    for &(i, d) in &held {
                        if (i as usize) < g.params {
                            edges.push(Edge {
                                from: from_base + i as usize,
                                to: base[&c.def] + k,
                                weight: d - e_k,
                                span: e.span,
                                callee: c.def,
                                arg: recv,
                                param: param_of(tcx, recv, i),
                            });
                        }
                    }
                }
            }
        };
        crate::span_visitor::for_each_expr_in_block(&g.f.body, &mut visit);
    }
    let comp = components(owner.len(), &edges);
    let mut out: Vec<Refusal> = Vec::new();
    let mut said: Vec<Span> = Vec::new();
    for (n, e) in edges.iter().enumerate() {
        if e.weight <= 0 || comp[e.from] != comp[e.to] || said.contains(&e.span) {
            continue;
        }
        if !closes_heavy_cycle(&edges, &comp, n) {
            continue;
        }
        said.push(e.span);
        let (caller, _) = owner[e.from];
        let callee = name(e.callee);
        let shown = |t: Ty| tcx.display(t, name);
        let what = if caller == e.callee {
            format!(
                "`{callee}` calls itself with `{}` in place of `{}`",
                shown(e.arg),
                shown(e.param)
            )
        } else {
            format!(
                "`{}` calls `{callee}` with `{}`, built from its own `{}`, and \
                 the calls lead back to `{}`",
                name(caller),
                shown(e.arg),
                shown(e.param),
                name(caller)
            )
        };
        out.push(Refusal {
            span: e.span,
            message: format!(
                "polymorphic recursion: {what}, so the argument grows on every round \
                 and would need an instance per depth; recurse at the same type, or \
                 keep the growing values in one type that does not change between \
                 rounds (a `Vec` of them)"
            ),
        });
    }
    out.sort_by_key(|r| r.span.offset);
    out
}

/// Whether edge `n` lies on a cycle of positive weight: the heaviest path
/// from its head back to its tail, within its component, plus its own
/// weight is above 0, or a positive cycle makes that path unbounded.
fn closes_heavy_cycle(edges: &[Edge], comp: &[usize], n: usize) -> bool {
    let e = &edges[n];
    let c = comp[e.from];
    let inside: Vec<&Edge> = edges
        .iter()
        .filter(|x| comp[x.from] == c && comp[x.to] == c)
        .collect();
    let mut best: FxHashMap<usize, i64> = FxHashMap::default();
    best.insert(e.to, 0);
    let nodes = comp.iter().filter(|&&k| k == c).count();
    // Bellman-Ford for the longest path; a relaxation in round `nodes`
    // means a positive cycle.
    for round in 0..=nodes {
        let mut changed = false;
        for x in &inside {
            let Some(&at) = best.get(&x.from) else {
                continue;
            };
            let to = at + x.weight;
            if best.get(&x.to).is_none_or(|&b| to > b) {
                best.insert(x.to, to);
                changed = true;
            }
        }
        if !changed {
            break;
        }
        if round == nodes {
            return true;
        }
    }
    best.get(&e.from).is_some_and(|&b| b + e.weight > 0)
}

fn peel_refs(tcx: &TyCtxt, mut ty: Ty) -> Ty {
    while let TyKind::Ref(t) | TyKind::MutRef(t) = tcx.kind(ty) {
        ty = t;
    }
    ty
}

/// Each `Param` in `ty`, by index, with the deepest level it sits at
/// (`T` is 0, `Vec[T]` 1, `Vec[(T, i64)]` 2).
fn params_in(tcx: &TyCtxt, ty: Ty) -> Vec<(u32, i64)> {
    let mut out: Vec<(u32, i64)> = Vec::new();
    fn go(tcx: &TyCtxt, ty: Ty, depth: i64, out: &mut Vec<(u32, i64)>) {
        if let TyKind::Param(p) = tcx.kind(ty) {
            match out.iter_mut().find(|(i, _)| *i == p.index) {
                Some((_, d)) => *d = (*d).max(depth),
                None => out.push((p.index, depth)),
            }
            return;
        }
        children(tcx, ty, &mut |c| go(tcx, c, depth + 1, out));
    }
    go(tcx, ty, 0, &mut out);
    out
}

/// Whether an impl's target is one of its own parameters (`impl[T] Show
/// for T`).
fn bare_param(t: &TypeExpr, names: &[&str]) -> bool {
    matches!(&t.kind, AstTy::Path(p)
        if p.generic_args.is_none() && p.segments.len() == 1 && names.contains(&p.segments[0].as_str()))
}

/// How deep the parameter `name` sits in the written type `t`, at its
/// shallowest; `None` when `t` does not write it in a shape read here.
fn depth_in(t: &TypeExpr, name: &str) -> Option<i64> {
    let inner = |ts: &mut dyn Iterator<Item = &TypeExpr>| {
        ts.filter_map(|c| depth_in(c, name)).min().map(|d| d + 1)
    };
    match &t.kind {
        AstTy::Path(p) => {
            if p.generic_args.is_none() && p.segments.len() == 1 && p.segments[0] == name {
                return Some(0);
            }
            let args = p.generic_args.as_deref()?;
            inner(&mut args.iter().filter_map(|a| match a {
                ast::GenericArg::Type(te) => Some(te),
                _ => None,
            }))
        }
        AstTy::Tuple(ts) => inner(&mut ts.iter()),
        AstTy::Array { element, .. } => inner(&mut std::iter::once(&**element)),
        AstTy::Ref(x) | AstTy::MutRef(x) => inner(&mut std::iter::once(&**x)),
        _ => None,
    }
}

/// The interned `Param` with index `i` inside `ty`.
fn param_of(tcx: &TyCtxt, ty: Ty, i: u32) -> Ty {
    match tcx.kind(ty) {
        TyKind::Param(p) if p.index == i => ty,
        _ => {
            let mut hit = None;
            children(tcx, ty, &mut |c| {
                if hit.is_none() && mentions(tcx, c, i) {
                    hit = Some(param_of(tcx, c, i));
                }
            });
            hit.unwrap_or(ty)
        }
    }
}

fn mentions(tcx: &TyCtxt, ty: Ty, i: u32) -> bool {
    let mut yes = false;
    tcx.walk(ty, &mut |k| {
        if matches!(k, TyKind::Param(p) if p.index == i) {
            yes = true;
        }
    });
    yes
}

/// `ty`'s immediate component types.
fn children(tcx: &TyCtxt, ty: Ty, f: &mut dyn FnMut(Ty)) {
    match tcx.kind(ty) {
        TyKind::Tuple(list)
        | TyKind::Adt { args: list, .. }
        | TyKind::Opaque { args: list, .. }
        | TyKind::Dyn { args: list, .. }
        | TyKind::Shared { args: list, .. }
        | TyKind::Intrinsic { args: list, .. }
        | TyKind::FnDef { args: list, .. }
        | TyKind::Closure { captures: list, .. } => {
            for t in tcx.list(list) {
                f(t);
            }
        }
        TyKind::Fn { params, ret, .. } => {
            for t in tcx.list(params) {
                f(t);
            }
            f(ret);
        }
        TyKind::Array { elem, .. } | TyKind::Vector { elem, .. } | TyKind::Slice { elem, .. } => {
            f(elem)
        }
        TyKind::Ref(t)
        | TyKind::MutRef(t)
        | TyKind::Weak(t)
        | TyKind::Proj { base: t, .. }
        | TyKind::RawPtr { pointee: t, .. } => f(t),
        _ => {}
    }
}

/// Strongly connected components of the graph (Tarjan, iterative), as a
/// component number per node.
fn components(n: usize, edges: &[Edge]) -> Vec<usize> {
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
    for e in edges {
        adj[e.from].push(e.to);
    }
    const NONE: usize = usize::MAX;
    let mut index = vec![NONE; n];
    let mut low = vec![0usize; n];
    let mut on = vec![false; n];
    let mut comp = vec![NONE; n];
    let mut stack: Vec<usize> = Vec::new();
    let (mut next, mut ncomp) = (0usize, 0usize);
    for root in 0..n {
        if index[root] != NONE {
            continue;
        }
        // (node, next edge to look at)
        let mut work: Vec<(usize, usize)> = vec![(root, 0)];
        index[root] = next;
        low[root] = next;
        next += 1;
        stack.push(root);
        on[root] = true;
        while let Some(&mut (v, ref mut k)) = work.last_mut() {
            if *k < adj[v].len() {
                let w = adj[v][*k];
                *k += 1;
                if index[w] == NONE {
                    index[w] = next;
                    low[w] = next;
                    next += 1;
                    stack.push(w);
                    on[w] = true;
                    work.push((w, 0));
                } else if on[w] {
                    low[v] = low[v].min(index[w]);
                }
                continue;
            }
            work.pop();
            if let Some(&(u, _)) = work.last() {
                low[u] = low[u].min(low[v]);
            }
            if low[v] == index[v] {
                while let Some(w) = stack.pop() {
                    on[w] = false;
                    comp[w] = ncomp;
                    if w == v {
                        break;
                    }
                }
                ncomp += 1;
            }
        }
    }
    comp
}
