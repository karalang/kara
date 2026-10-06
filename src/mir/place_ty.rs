//! Typing of places: the type a projection path ends at.

use super::syntax::{Body, Place, ProjElem};
use super::ty::{IntTy, Ty, TyInterner, TyKind};

/// The type of a place, plus the variant it is viewed as after a
/// `Downcast` (only meaningful until the following `Field`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaceTy {
    pub ty: Ty,
    pub variant: Option<u32>,
}

/// Computes the type of `place`, or explains why its projection path is
/// ill-typed.
pub fn place_ty(body: &Body, tys: &TyInterner, place: &Place) -> Result<PlaceTy, String> {
    let Some(decl) = body.locals.get(place.local.index()) else {
        return Err(format!("{} is not a local of this body", place.local));
    };
    let mut cur = PlaceTy {
        ty: decl.ty,
        variant: None,
    };
    for elem in &place.projection {
        cur = project(body, tys, cur, elem)?;
    }
    Ok(cur)
}

fn project(
    body: &Body,
    tys: &TyInterner,
    cur: PlaceTy,
    elem: &ProjElem,
) -> Result<PlaceTy, String> {
    let kind = tys.kind(cur.ty);
    match *elem {
        ProjElem::Field(f, declared) => {
            if let (TyKind::Adt(a) | TyKind::Shared(a), None) = (kind, cur.variant) {
                if tys.adt(*a).is_enum {
                    return Err(format!(
                        "field {} of enum {} without a downcast",
                        f.0,
                        tys.display(cur.ty)
                    ));
                }
            }
            let actual = tys
                .field_ty(cur.ty, cur.variant, f.0)
                .ok_or_else(|| format!("{} has no field {}", tys.display(cur.ty), f.0))?;
            if actual != declared {
                return Err(format!(
                    "field {} of {} has type {}, but the projection says {}",
                    f.0,
                    tys.display(cur.ty),
                    tys.display(actual),
                    tys.display(declared)
                ));
            }
            Ok(PlaceTy {
                ty: actual,
                variant: None,
            })
        }
        ProjElem::Downcast(v) => match kind {
            TyKind::Adt(a) | TyKind::Shared(a) if tys.adt(*a).is_enum && cur.variant.is_none() => {
                if v.index() >= tys.adt(*a).variants.len() {
                    return Err(format!("{} has no variant {}", tys.display(cur.ty), v.0));
                }
                Ok(PlaceTy {
                    ty: cur.ty,
                    variant: Some(v.0),
                })
            }
            _ => Err(format!("downcast of non-enum {}", tys.display(cur.ty))),
        },
        ProjElem::Deref => match kind {
            TyKind::Ref(t) | TyKind::MutRef(t) => Ok(PlaceTy {
                ty: *t,
                variant: None,
            }),
            _ => Err(format!("deref of non-reference {}", tys.display(cur.ty))),
        },
        ProjElem::Index(idx) => {
            let idx_ty = body
                .locals
                .get(idx.index())
                .ok_or_else(|| format!("index {idx} is not a local of this body"))?
                .ty;
            if *tys.kind(idx_ty) != TyKind::Int(IntTy::Usize) {
                return Err(format!(
                    "index {idx} has type {}, not usize",
                    tys.display(idx_ty)
                ));
            }
            element(tys, cur)
        }
        ProjElem::ConstIndex(i) => {
            if let TyKind::Array(_, n) = kind {
                if i >= *n {
                    return Err(format!(
                        "constant index {i} out of bounds for {}",
                        tys.display(cur.ty)
                    ));
                }
            }
            element(tys, cur)
        }
    }
}

fn element(tys: &TyInterner, cur: PlaceTy) -> Result<PlaceTy, String> {
    match tys.kind(cur.ty) {
        TyKind::Array(e, _) | TyKind::Slice(e) => Ok(PlaceTy {
            ty: *e,
            variant: None,
        }),
        _ => Err(format!("index into non-array {}", tys.display(cur.ty))),
    }
}

/// Does the path to `place` pass through a `shared` handle? Moving out of
/// such a place is forbidden (core semantics §3.7).
pub fn passes_through_shared(body: &Body, tys: &TyInterner, place: &Place) -> bool {
    let mut cur = PlaceTy {
        ty: body.locals[place.local.index()].ty,
        variant: None,
    };
    for elem in &place.projection {
        if matches!(tys.kind(cur.ty), TyKind::Shared(_)) {
            return true;
        }
        match project(body, tys, cur, elem) {
            Ok(next) => cur = next,
            Err(_) => return false,
        }
    }
    false
}
