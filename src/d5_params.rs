//! D5, borrow-by-default parameters (core-semantics.md amendment 8, §4.1).
//!
//! A bare parameter `x: T` borrows (a copy for a `Copy` type) and only
//! `own T` owns; a bare `self` borrows and only `own self` owns. §4.1 calls
//! `ref T` in a parameter "a redundant spelling of bare `T`", so the strict
//! check reads a bare parameter by rewriting it to that spelling before name
//! resolution. Every rule the check already has for a `ref` parameter then
//! applies to it unchanged: a caller's argument does not move, the callee
//! cannot move out of it or write it, and a task or closure that hands it on
//! only borrows it.

use crate::ast::*;
use rustc_hash::{FxHashMap, FxHashSet};

/// The D5 switch, shared by the strict check and the MIR builder: on while
/// `KARAC_D5=1`, until every program has been migrated to `own`.
pub fn on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("KARAC_D5").is_ok_and(|v| v == "1"))
}

const COPY_SCALARS: &[&str] = &[
    "i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64", "u128", "usize", "f16",
    "bf16", "f32", "f64", "bool", "char", "Vector", "Mask",
];

/// Bounds that imply `Copy`: the numeric towers are all scalar.
const COPY_BOUNDS: &[&str] = &["Copy", "Numeric", "Integer", "Float"];

/// Rewrite every bare parameter of a user function, method and trait method
/// to `ref T`, and every bare `self` to `ref self`. Left alone: `own`
/// parameters, ones that already name a borrow or a view (`ref T`,
/// `mut ref T`, `Slice[T]`, `mut Slice[T]`, `Str`), parameters whose type is
/// copied or is a counted handle (a scalar, a `Copy` type or type parameter,
/// a `shared` type, an `Option` or tuple of those: [`copy_like`]),
/// function-typed and `impl Trait` parameters, functions exported to a
/// wasm host (`#[target(...)]`), and the standard library's own bodies.
///
/// A trait method's parameters take the modes the trait declares, so an
/// impl of a user trait follows the trait's rewrite position by position,
/// and an impl of a standard-library trait (`From`, `Add`, ...) keeps the
/// modes the library spells, as the library is not rewritten.
pub fn borrow_bare_params(program: &mut Program) {
    let mut copy: FxHashSet<String> = COPY_SCALARS.iter().map(|s| s.to_string()).collect();
    for item in &program.items {
        let (name, attrs, shared) = match item {
            Item::StructDef(s) => (&s.name, &s.attributes, s.is_shared),
            Item::EnumDef(e) => (&e.name, &e.attributes, e.is_shared),
            _ => continue,
        };
        if shared || crate::typechecker::extract_derived_traits(attrs).contains("Copy") {
            copy.insert(name.clone());
        }
    }
    // Per user trait method: whether `self` and each parameter borrow.
    let mut traits: FxHashMap<(String, String), (bool, Vec<bool>)> = FxHashMap::default();
    for item in &mut program.items {
        let Item::TraitDef(t) = item else { continue };
        if t.stdlib_origin {
            continue;
        }
        let outer = copy_bounded(t.generic_params.as_ref(), t.where_clause.as_ref());
        for it in &mut t.items {
            if let TraitItem::Method(m) = it {
                let mut copy = copy.clone();
                copy.extend(outer.iter().cloned());
                copy.extend(copy_bounded(
                    m.generic_params.as_ref(),
                    m.where_clause.as_ref(),
                ));
                copy.insert("Self".to_string());
                let self_borrows = matches!(m.self_param, Some(SelfParam::Owned)) && !m.self_is_own;
                let modes: Vec<bool> = m
                    .params
                    .iter()
                    .map(|p| !p.is_own && borrows(&p.ty, &copy))
                    .collect();
                if self_borrows {
                    m.self_param = Some(SelfParam::Ref);
                }
                apply(&mut m.params, &modes);
                traits.insert((t.name.clone(), m.name.clone()), (self_borrows, modes));
            }
        }
    }
    for item in &mut program.items {
        match item {
            Item::Function(f) if !f.stdlib_origin && !exported(&f.attributes) => {
                let copy = with_bounds(
                    &copy,
                    &[f.generic_params.as_ref()],
                    &[f.where_clause.as_ref()],
                );
                borrow_fn(f, &copy);
            }
            Item::ImplBlock(imp) => {
                let trait_name = imp
                    .trait_name
                    .as_ref()
                    .and_then(|p| p.segments.last().cloned());
                let gp = imp.generic_params.clone();
                let wc = imp.where_clause.clone();
                for it in &mut imp.items {
                    let ImplItem::Method(m) = it else { continue };
                    if m.stdlib_origin {
                        continue;
                    }
                    match &trait_name {
                        // An impl of a user trait takes the trait's modes.
                        Some(tn) => {
                            if let Some((self_borrows, modes)) =
                                traits.get(&(tn.clone(), m.name.clone()))
                            {
                                if *self_borrows && !m.self_is_own {
                                    m.self_param = Some(SelfParam::Ref);
                                }
                                apply(&mut m.params, modes);
                            }
                        }
                        None => {
                            let copy = with_bounds(
                                &copy,
                                &[gp.as_ref(), m.generic_params.as_ref()],
                                &[wc.as_ref(), m.where_clause.as_ref()],
                            );
                            borrow_fn(m, &copy);
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

fn borrow_fn(f: &mut Function, copy: &FxHashSet<String>) {
    if matches!(f.self_param, Some(SelfParam::Owned)) && !f.self_is_own {
        f.self_param = Some(SelfParam::Ref);
    }
    let modes: Vec<bool> = f
        .params
        .iter()
        .map(|p| !p.is_own && borrows(&p.ty, copy))
        .collect();
    apply(&mut f.params, &modes);
}

/// A function handed to a wasm host, or run as a GPU kernel, takes its
/// arguments by value.
fn exported(attrs: &[Attribute]) -> bool {
    attrs
        .iter()
        .any(|a| a.is_bare("target") || a.is_bare("gpu"))
}

/// `copy` plus the type parameters these generics bound by `Copy`.
fn with_bounds(
    copy: &FxHashSet<String>,
    gps: &[Option<&GenericParams>],
    wcs: &[Option<&WhereClause>],
) -> FxHashSet<String> {
    let mut out = copy.clone();
    for (gp, wc) in gps.iter().zip(wcs) {
        out.extend(copy_bounded(*gp, *wc));
    }
    out
}

fn copy_bounded(gp: Option<&GenericParams>, wc: Option<&WhereClause>) -> Vec<String> {
    let is_copy = |b: &TraitBound| {
        b.path
            .last()
            .is_some_and(|n| COPY_BOUNDS.contains(&n.as_str()))
    };
    let mut out: Vec<String> = gp
        .iter()
        .flat_map(|g| &g.params)
        .filter(|p| p.bounds.iter().any(is_copy))
        .map(|p| p.name.clone())
        .collect();
    for c in wc.iter().flat_map(|w| &w.constraints) {
        if let WhereConstraint::TypeBound {
            type_name, bounds, ..
        } = c
        {
            if bounds.iter().any(is_copy) {
                out.push(type_name.clone());
            }
        }
    }
    out
}

fn apply(params: &mut [Param], modes: &[bool]) {
    for (p, borrow) in params.iter_mut().zip(modes) {
        if !*borrow || p.is_own || matches!(p.ty.kind, TypeKind::Ref(_) | TypeKind::MutRef(_)) {
            continue;
        }
        let span = p.ty.span;
        let inner = std::mem::replace(
            &mut p.ty,
            TypeExpr {
                kind: TypeKind::Unit,
                span,
            },
        );
        p.ty = TypeExpr {
            span: inner.span,
            kind: TypeKind::Ref(Box::new(inner)),
        };
    }
}

/// Whether a bare parameter of this type becomes a borrow.
fn borrows(ty: &TypeExpr, copy: &FxHashSet<String>) -> bool {
    match &ty.kind {
        TypeKind::Path(path) => {
            let last = path.segments.last().map(String::as_str).unwrap_or("");
            !matches!(last, "Str" | "StringSlice" | "Slice") && !copy_like(ty, copy)
        }
        TypeKind::Tuple(elems) => !elems.is_empty() && !copy_like(ty, copy),
        TypeKind::Array { .. } => !copy_like(ty, copy),
        _ => false,
    }
}

/// A type whose every part is copied (a scalar, a `Copy` type) or is a
/// counted handle (a `shared` type): `Option[TreeNode]`, `(i64, Node)`.
/// A borrow of one reads exactly what a copy does, and a store of it takes
/// a copy or counts the handle (§6.1), so it stays bare.
fn copy_like(ty: &TypeExpr, copy: &FxHashSet<String>) -> bool {
    match &ty.kind {
        TypeKind::Path(path) => {
            let last = path.segments.last().map(String::as_str).unwrap_or("");
            let args: Vec<&TypeExpr> = path
                .generic_args
                .iter()
                .flatten()
                .filter_map(|a| match a {
                    GenericArg::Type(t) => Some(t),
                    _ => None,
                })
                .collect();
            if copy.contains(last) {
                return true;
            }
            matches!(last, "Option" | "Result")
                && !args.is_empty()
                && args.iter().all(|t| copy_like(t, copy))
        }
        TypeKind::Tuple(elems) => elems.iter().all(|t| copy_like(t, copy)),
        TypeKind::Array { element, .. } => copy_like(element, copy),
        TypeKind::Unit => true,
        _ => false,
    }
}

/// The parameters of `f` (and `self`) that borrow only because the rewrite
/// above made them, each with the point where `own ` makes it owned. The
/// rewrite gives the `ref` it adds the span of the type it wraps, and a
/// receiver it rewrites keeps the span of the bare `self`, which is how a
/// written `ref T` or `ref self` is told apart.
pub fn implied_borrows(f: &Function) -> FxHashMap<String, crate::token::Span> {
    let at = |s: crate::token::Span| crate::token::Span { length: 0, ..s };
    let mut out = FxHashMap::default();
    for p in &f.params {
        if let TypeKind::Ref(inner) = &p.ty.kind {
            if inner.span == p.ty.span {
                if let Some(n) = p.name() {
                    out.insert(n.to_string(), at(p.ty.span));
                }
            }
        }
    }
    if let (Some(SelfParam::Ref), Some(sp)) = (&f.self_param, f.self_span) {
        if sp.length == "self".len() {
            out.insert("self".to_string(), at(sp));
        }
    }
    out
}
