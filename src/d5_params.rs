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

/// The D5 switch, shared by the strict check and the MIR builder: on, with
/// `KARAC_D5=0` to read bare parameters by value as before the migration to
/// `own T`. The legacy backends never read it.
pub fn on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("KARAC_D5").map_or(true, |v| v != "0"))
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
/// a `shared` or `sync` type, an `Option` or tuple of those: [`copy_like`]),
/// function-typed and `impl Trait` parameters, functions exported to a
/// wasm host (`#[target(...)]`), and the standard library's own bodies.
/// The parameters of function types are rewritten the same way
/// ([`borrow_fn_type_params`]).
///
/// An impl of a user trait borrows wherever the trait's rewrite does,
/// position by position, and every impl, of a user or a standard-library
/// trait (`From`, `Add`, ...), also borrows at its own bare `self` and
/// parameters, since an impl may be weaker than its trait (§4.7).
pub fn borrow_bare_params(program: &mut Program) {
    let mut copy: FxHashSet<String> = COPY_SCALARS.iter().map(|s| s.to_string()).collect();
    for item in &program.items {
        let (name, attrs, shared) = match item {
            // A `sync` (`par`) type is a counted handle, as a `shared` one is.
            Item::StructDef(s) => (&s.name, &s.attributes, s.is_shared || s.is_par),
            Item::EnumDef(e) => (&e.name, &e.attributes, e.is_shared || e.is_par),
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
                    // An impl of a user trait takes the trait's borrows.
                    if let Some(tn) = &trait_name {
                        if let Some((self_borrows, modes)) =
                            traits.get(&(tn.clone(), m.name.clone()))
                        {
                            if *self_borrows && !m.self_is_own {
                                m.self_param = Some(SelfParam::Ref);
                            }
                            apply(&mut m.params, modes);
                        }
                    }
                    // Its own bare `self` and parameters borrow as well, in
                    // a trait impl as in an inherent one: an impl may declare
                    // a weaker mode than its trait, and a call on a concrete
                    // type uses the impl's (core semantics §4.7). The MIR
                    // builder reads an impl's bare receiver and parameters
                    // that way already (`self_mode`, `param_mode_ty`), so
                    // reading the trait's `own` here made the strict check
                    // move what MIR borrows (B-2026-10-14-34).
                    let copy = with_bounds(
                        &copy,
                        &[gp.as_ref(), m.generic_params.as_ref()],
                        &[wc.as_ref(), m.where_clause.as_ref()],
                    );
                    borrow_fn(m, &copy);
                }
            }
            _ => {}
        }
    }
    borrow_fn_type_params(program, &copy);
}

/// A function type's bare parameters borrow as an item's do:
/// `Fn(String) -> i64` reads `Fn(ref String) -> i64`, the type of
/// `fn f(s: String) -> i64` once its parameter borrows, so an item used as
/// a value still has the type written for it. An `own T` parameter
/// (`MutFn(own Self.Item) -> U`) stays owned. Function types in
/// signatures, bounds, fields, aliases and bodies are all rewritten; the
/// standard library's own items are not.
fn borrow_fn_type_params(program: &mut Program, copy: &FxHashSet<String>) {
    for item in &mut program.items {
        let skip = match &*item {
            Item::Function(f) => f.stdlib_origin || exported(&f.attributes),
            Item::TraitDef(t) => t.stdlib_origin,
            Item::StructDef(s) => s.stdlib_origin,
            Item::EnumDef(e) => e.stdlib_origin,
            Item::UnionDef(u) => u.stdlib_origin,
            Item::ImplBlock(imp) => imp
                .items
                .iter()
                .any(|it| matches!(it, ImplItem::Method(m) if m.stdlib_origin)),
            _ => false,
        };
        if skip {
            continue;
        }
        let mut copy = copy.clone();
        copy.extend(item_copy_bounded(item));
        crate::span_visitor::walk_item_mut_with(item, &mut FnTypeParams { copy: &copy });
    }
}

struct FnTypeParams<'a> {
    copy: &'a FxHashSet<String>,
}

impl crate::span_visitor::MutVisitor for FnTypeParams<'_> {
    fn span(&mut self, _: &mut crate::token::Span) {}
    /// A closure's annotated bare parameter borrows as a function's does:
    /// `|w: Vec[i64]|` reads `|w: ref Vec[i64]|`, and only `|w: own T|`
    /// owns (design.md § Function values, "Parameter modes").
    fn expr(&mut self, e: &mut Expr) {
        let ExprKind::Closure { params, .. } = &mut e.kind else {
            return;
        };
        for p in params {
            if let Some(t) = &mut p.ty {
                if !p.is_own && borrows(t, self.copy) {
                    wrap_ref(t);
                }
            }
        }
    }
    fn type_expr(&mut self, t: &mut TypeExpr) {
        let TypeKind::FnType {
            params, own_params, ..
        } = &mut t.kind
        else {
            return;
        };
        for (i, p) in params.iter_mut().enumerate() {
            if !own_params.get(i).copied().unwrap_or(false) && borrows(p, self.copy) {
                wrap_ref(p);
            }
        }
    }
}

/// The type parameters an item's generics, or its methods', bound by `Copy`.
fn item_copy_bounded(item: &Item) -> Vec<String> {
    let mut out = Vec::new();
    let fun = |f: &Function, out: &mut Vec<String>| {
        out.extend(copy_bounded(
            f.generic_params.as_ref(),
            f.where_clause.as_ref(),
        ));
    };
    match item {
        Item::Function(f) => fun(f, &mut out),
        Item::ImplBlock(imp) => {
            out.extend(copy_bounded(
                imp.generic_params.as_ref(),
                imp.where_clause.as_ref(),
            ));
            for it in &imp.items {
                if let ImplItem::Method(m) = it {
                    fun(m, &mut out);
                }
            }
        }
        Item::TraitDef(t) => {
            out.extend(copy_bounded(
                t.generic_params.as_ref(),
                t.where_clause.as_ref(),
            ));
            for it in &t.items {
                if let TraitItem::Method(m) = it {
                    out.extend(copy_bounded(
                        m.generic_params.as_ref(),
                        m.where_clause.as_ref(),
                    ));
                }
            }
            out.push("Self".to_string());
        }
        Item::StructDef(s) => out.extend(copy_bounded(
            s.generic_params.as_ref(),
            s.where_clause.as_ref(),
        )),
        Item::EnumDef(e) => out.extend(copy_bounded(
            e.generic_params.as_ref(),
            e.where_clause.as_ref(),
        )),
        _ => {}
    }
    out
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
        if *borrow && !p.is_own {
            wrap_ref(&mut p.ty);
        }
    }
}

/// `T` becomes `ref T`, keeping `T`'s span: [`implied_borrows`] tells the
/// rewrite's `ref` from a written one by that.
fn wrap_ref(ty: &mut TypeExpr) {
    if matches!(ty.kind, TypeKind::Ref(_) | TypeKind::MutRef(_)) {
        return;
    }
    let span = ty.span;
    let inner = std::mem::replace(
        ty,
        TypeExpr {
            kind: TypeKind::Unit,
            span,
        },
    );
    *ty = TypeExpr {
        span: inner.span,
        kind: TypeKind::Ref(Box::new(inner)),
    };
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
