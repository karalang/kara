//! The size and alignment of a MIR type: what `T.size_of()` and
//! `T.align_of()` answer (`Rvalue::NullaryOp`), on a 64-bit target.
//!
//! This is the one place the MIR backends get a type's layout from, so the
//! MIR interpreter and the LLVM backend (M2) report the same numbers. The
//! rules are the natural C layout, with no field reordering and no niche
//! optimization:
//!
//! - Scalars are their own width (`char` is 4 bytes, `bool` 1, `i128` 16).
//! - A tuple, struct or closure places each part at the next multiple of
//!   its alignment, in order; the whole is padded to its largest alignment.
//! - An enum is a tag (the smallest unsigned integer that counts its
//!   variants) followed by the largest variant laid out as a struct.
//! - `String`, `Vec` and `VecDeque` are `{ptr, len, cap}`; a slice, a static
//!   `str` and an erased function value are two words; the map and set
//!   collections, `shared` and `weak` handles, references to sized types and
//!   raw pointers are one word.

use super::ty::{FloatTy, IntrinsicTy, Ty, TyInterner, TyKind};
use crate::ty::TyKind as SharedKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub size: u64,
    pub align: u64,
}

const WORD: Layout = Layout { size: 8, align: 8 };
const TWO_WORDS: Layout = Layout { size: 16, align: 8 };
const ZST: Layout = Layout { size: 0, align: 1 };

fn scalar(bytes: u64) -> Layout {
    Layout {
        size: bytes,
        align: bytes,
    }
}

fn align_up(v: u64, align: u64) -> u64 {
    v.div_ceil(align) * align
}

/// Lay `parts` out in order, starting at offset `start`.
fn record(start: Layout, parts: impl IntoIterator<Item = Layout>) -> Layout {
    let mut size = start.size;
    let mut align = start.align;
    for p in parts {
        size = align_up(size, p.align) + p.size;
        align = align.max(p.align);
    }
    Layout {
        size: align_up(size, align),
        align,
    }
}

/// The layout of `t`, or `None` for a type with no size (a generic
/// parameter, which never reaches a MIR body, or a recursive value type,
/// which the checker rejects).
pub fn layout(tys: &TyInterner, t: Ty) -> Option<Layout> {
    layout_at(tys, t, 0)
}

fn layout_at(tys: &TyInterner, t: Ty, depth: u32) -> Option<Layout> {
    if depth > 64 {
        return None;
    }
    let of = |t: Ty| layout_at(tys, t, depth + 1);
    Some(match tys.kind(t) {
        TyKind::Bool => scalar(1),
        TyKind::Char => scalar(4),
        TyKind::Int(i) => scalar(u64::from(i.bits()) / 8),
        TyKind::Float(FloatTy::F16 | FloatTy::BF16) => scalar(2),
        TyKind::Float(FloatTy::F32) => scalar(4),
        TyKind::Float(FloatTy::F64) => scalar(8),
        TyKind::Unit | TyKind::Never | TyKind::FnDef(_) => ZST,
        TyKind::Str | TyKind::Slice(_) | TyKind::Fn { .. } => TWO_WORDS,
        TyKind::Tuple(ts) | TyKind::Closure(_, ts) => {
            let mut parts = Vec::with_capacity(ts.len());
            for t in ts {
                parts.push(of(t)?);
            }
            record(ZST, parts)
        }
        TyKind::Array(e, n) => {
            let el = of(e)?;
            Layout {
                size: el.size * n,
                align: el.align,
            }
        }
        TyKind::Adt(_) => {
            let (adt, _) = tys.tcx().adt_of(t)?;
            let variant = |v: usize| -> Option<Vec<Layout>> {
                let fields = adt.variants[v].fields.len() as u32;
                let var = adt.is_enum.then_some(v as u32);
                (0..fields).map(|f| of(tys.field_ty(t, var, f)?)).collect()
            };
            if !adt.is_enum {
                return Some(record(ZST, variant(0).unwrap_or_default()));
            }
            let n = adt.variants.len() as u64;
            let tag = match n {
                0 => return Some(ZST),
                1..=0x100 => scalar(1),
                0x101..=0x1_0000 => scalar(2),
                _ => scalar(4),
            };
            let mut whole = tag;
            for v in 0..adt.variants.len() {
                let l = record(tag, variant(v)?);
                whole = Layout {
                    size: whole.size.max(l.size),
                    align: whole.align.max(l.align),
                };
            }
            Layout {
                size: align_up(whole.size, whole.align),
                align: whole.align,
            }
        }
        TyKind::Ref(p) | TyKind::MutRef(p) => match tys.kind(p) {
            TyKind::Str | TyKind::Slice(_) => TWO_WORDS,
            _ => WORD,
        },
        TyKind::Intrinsic(IntrinsicTy::String | IntrinsicTy::Vec(_) | IntrinsicTy::VecDeque(_)) => {
            Layout { size: 24, align: 8 }
        }
        TyKind::Intrinsic(_) | TyKind::Shared(_) | TyKind::Weak(_) => WORD,
        TyKind::Other => match tys.tcx().kind(t) {
            SharedKind::RawPtr { .. } => WORD,
            _ => return None,
        },
    })
}
