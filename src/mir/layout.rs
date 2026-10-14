//! The size and alignment of a MIR type: what `T.size_of()` and
//! `T.align_of()` answer (`Rvalue::NullaryOp`), on a 64-bit target.
//!
//! This is the one place the MIR backends get a type's layout from, so the
//! MIR interpreter and the LLVM backend (M2) report the same numbers. The
//! rules are the natural C layout, with no field reordering and one niche
//! (an `Option` of a non-null pointer):
//!
//! - Scalars are their own width (`char` is 4 bytes, `bool` 1, `i128` 16).
//! - A tuple, struct or closure places each part at the next multiple of
//!   its alignment, in order; the whole is padded to its largest alignment.
//! - An enum is a tag (the smallest unsigned integer that counts its
//!   variants) followed by the largest variant laid out as a struct.
//! - A field whose type holds its own ADT again by value is boxed: a
//!   pointer to a malloc'd value ([`boxed_field`]).
//! - An `Option` of a thin pointer that is never null (a `shared` handle,
//!   or a reference to a sized, non-empty type) is that pointer alone, with
//!   null for `None` ([`null_niche`]).
//! - A `shared` handle points at a box: two `u64` counts (strong, then
//!   weak) and then the type's body, laid out as the plain struct or enum.
//! - `String` and `Vec` are `{ptr, len, cap}`; `VecDeque` is a ring,
//!   `{ptr, head, len, cap}`. A slice and a static `str` are two words.
//! - An erased function value is `{code, env, drop}`: `env` points at the
//!   closure's captures (laid out as its MIR closure type) in a malloc'd
//!   block, or is null with no captures, and `drop` is the captures' glue,
//!   or null.
//! - The map and set collections are one word, a handle on a table the
//!   runtime builds; `shared` and `weak` handles, references to sized types
//!   and raw pointers are one word too.
//! - The library cells: `Sender`, `Receiver` and `File` are a one-word
//!   handle, `Atomic[T]` is a `T` held in place, and the rest keep the
//!   fields they declare (one `i64` handle each).

use super::ty::{FloatTy, IntrinsicTy, Ty, TyInterner, TyKind};

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
        TyKind::Str | TyKind::Slice(_) => TWO_WORDS,
        TyKind::Fn { .. } => Layout { size: 24, align: 8 },
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
        TyKind::Adt(_) => match library_cell(tys, t) {
            Some(Cell::Handle) => WORD,
            Some(Cell::Inline(held)) => of(held)?,
            None => adt_body(tys, t, depth)?,
        },
        TyKind::Ref(p) | TyKind::MutRef(p) => match tys.kind(p) {
            TyKind::Str | TyKind::Slice(_) => TWO_WORDS,
            _ => WORD,
        },
        TyKind::Intrinsic(IntrinsicTy::String | IntrinsicTy::Vec(_)) => {
            Layout { size: 24, align: 8 }
        }
        TyKind::Intrinsic(IntrinsicTy::VecDeque(_)) => Layout { size: 32, align: 8 },
        TyKind::Intrinsic(_) | TyKind::Shared(_) | TyKind::Weak(_) | TyKind::RawPtr { .. } => WORD,
        TyKind::Other => return None,
    })
}

/// How a library cell whose declared fields are not its contents is held.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cell {
    /// A one-word runtime handle (`Sender`, `Receiver`, `File`, `CStr`,
    /// `CString`, `BufReader`, `BufWriter`, `LinesIter`).
    Handle,
    /// The held value, in place (`Atomic[T]`, `Mutex[T]`).
    Inline(Ty),
}

/// Whether the plain ADT `t` is a library cell laid out other than by its
/// declared fields.
pub fn library_cell(tys: &TyInterner, t: Ty) -> Option<Cell> {
    let TyKind::Adt(a) = tys.kind(t) else {
        return None;
    };
    match tys.adt(a).name.as_str() {
        // A `CStr` / `CString` is a pointer to NUL-terminated bytes.
        // A `TaskHandle` points at a block holding its task's result.
        // A `LinesIter` is its `BufReader`'s handle, borrowed.
        "Sender" | "Receiver" | "File" | "CStr" | "CString" | "TaskHandle" | "BufReader"
        | "BufWriter" | "LinesIter" => Some(Cell::Handle),
        "Atomic" | "Mutex" => Some(Cell::Inline(tys.tcx().adt_of(t)?.1.first().copied()?)),
        _ => None,
    }
}

/// The layout of the body of the struct or enum `t`, which is a plain ADT
/// or a `shared` one (whose handle points at a box holding this body).
fn adt_body(tys: &TyInterner, t: Ty, depth: u32) -> Option<Layout> {
    let of = |t: Ty| layout_at(tys, t, depth + 1);
    if null_niche(tys, t).is_some() {
        return Some(WORD);
    }
    let (adt, _) = tys.tcx().adt_of(t)?;
    let variant = |v: usize| -> Option<Vec<Layout>> {
        let fields = adt.variants[v].fields.len() as u32;
        let var = adt.is_enum.then_some(v as u32);
        (0..fields)
            .map(|f| {
                if boxed_field(tys, t, var, f) {
                    Some(WORD)
                } else {
                    of(tys.field_ty(t, var, f)?)
                }
            })
            .collect()
    };
    if !adt.is_enum {
        return Some(record(ZST, variant(0).unwrap_or_default()));
    }
    let Some(tag) = enum_tag(tys, t) else {
        return Some(ZST);
    };
    let mut whole = tag;
    for v in 0..adt.variants.len() {
        let l = record(tag, variant(v)?);
        whole = Layout {
            size: whole.size.max(l.size),
            align: whole.align.max(l.align),
        };
    }
    Some(Layout {
        size: align_up(whole.size, whole.align),
        align: whole.align,
    })
}

/// The tag of the enum `t` (plain or `shared`): the smallest unsigned
/// integer that counts its variants, at offset 0, holding the variant's
/// index. `None` for a struct or an enum with no variants.
pub fn enum_tag(tys: &TyInterner, t: Ty) -> Option<Layout> {
    let (adt, _) = tys.tcx().adt_of(t)?;
    if !adt.is_enum {
        return None;
    }
    Some(match adt.variants.len() as u64 {
        0 => return None,
        1..=0x100 => scalar(1),
        0x101..=0x1_0000 => scalar(2),
        _ => scalar(4),
    })
}

/// The bytes before a `shared` box's body: the strong and the weak count.
pub const SHARED_COUNTS: u64 = 16;

/// The layout of the body of the ADT `t` (plain or `shared`): for a plain
/// ADT this is `layout(t)`; for a `shared` one it is what the box holds
/// after its counts.
pub fn body_layout(tys: &TyInterner, t: Ty) -> Option<Layout> {
    adt_body(tys, t, 0)
}

/// Where a `shared` box keeps the body of `t`: after the counts, aligned.
pub fn shared_body_offset(tys: &TyInterner, t: Ty) -> Option<u64> {
    Some(align_up(SHARED_COUNTS, body_layout(tys, t)?.align))
}

/// The offset of field `f` within a value of `t` (seen as `variant` when
/// `t` is an enum): a tuple element, a closure capture, or a struct or
/// enum field. For a `shared` type it is the offset within the box's body
/// (add [`shared_body_offset`] for the offset within the box).
pub fn field_offset(tys: &TyInterner, t: Ty, variant: Option<u32>, f: u32) -> Option<u64> {
    if let Some((_, some)) = null_niche(tys, t) {
        return (variant == Some(some) && f == 0).then_some(0);
    }
    let (start, count) = match tys.kind(t) {
        TyKind::Tuple(ts) | TyKind::Closure(_, ts) if variant.is_none() => (ZST, ts.len()),
        TyKind::Adt(_) | TyKind::Shared(_) => {
            let (adt, _) = tys.tcx().adt_of(t)?;
            let v = match (adt.is_enum, variant) {
                (false, None) => 0,
                (true, Some(v)) => v as usize,
                _ => return None,
            };
            let start = if adt.is_enum { enum_tag(tys, t)? } else { ZST };
            (start, adt.variants.get(v)?.fields.len())
        }
        _ => return None,
    };
    if f as usize >= count {
        return None;
    }
    let mut at = start.size;
    for i in 0..=f {
        let l = if boxed_field(tys, t, variant, i) {
            WORD
        } else {
            layout(tys, tys.field_ty(t, variant, i)?)?
        };
        at = align_up(at, l.align);
        if i < f {
            at += l.size;
        }
    }
    Some(at)
}

/// For an `Option` stored as a bare pointer, the variant indices of its
/// `None` and `Some`: the payload is a `shared` handle or a reference to a
/// sized type that is not empty, so it is never null, and a null pointer
/// is `None`. The logical tag is still [`enum_tag`]'s byte; readers and
/// writers of the tag compare and store the pointer instead.
pub fn null_niche(tys: &TyInterner, t: Ty) -> Option<(u32, u32)> {
    let TyKind::Adt(a) = tys.kind(t) else {
        return None;
    };
    let adt = tys.adt(a);
    if adt.name != "Option" || adt.variants.len() != 2 {
        return None;
    }
    let some = adt.variants.iter().position(|v| v.name == "Some")? as u32;
    let none = 1 - some;
    if adt.variants[some as usize].fields.len() != 1
        || !adt.variants[none as usize].fields.is_empty()
    {
        return None;
    }
    let payload = tys.field_ty(t, Some(some), 0)?;
    let ok = match tys.kind(payload) {
        TyKind::Shared(_) => true,
        TyKind::Ref(p) | TyKind::MutRef(p) => {
            !matches!(tys.kind(p), TyKind::Str | TyKind::Slice(_))
                && !may_be_empty(tys, p, &mut Vec::new())
        }
        _ => false,
    };
    ok.then_some((none, some))
}

/// Whether `t` may have size 0 (a reference to it then need not point at
/// anything, and may be null). Answered from the type's shape rather than
/// its layout, which asks [`null_niche`] in turn; a type met again on the
/// way counts as possibly empty.
fn may_be_empty(tys: &TyInterner, t: Ty, seen: &mut Vec<Ty>) -> bool {
    if seen.contains(&t) {
        return true;
    }
    seen.push(t);
    match tys.kind(t) {
        TyKind::Unit | TyKind::Never | TyKind::FnDef(_) | TyKind::Other => true,
        TyKind::Tuple(ts) | TyKind::Closure(_, ts) => {
            ts.into_iter().all(|t| may_be_empty(tys, t, seen))
        }
        TyKind::Array(el, n) => n == 0 || may_be_empty(tys, el, seen),
        TyKind::Adt(_) => match library_cell(tys, t) {
            Some(Cell::Handle) => false,
            Some(Cell::Inline(h)) => may_be_empty(tys, h, seen),
            None => {
                let Some((adt, _)) = tys.tcx().adt_of(t) else {
                    return true;
                };
                if adt.is_enum {
                    // Any variant gives the enum a tag byte.
                    return adt.variants.is_empty();
                }
                (0..adt.variants[0].fields.len() as u32).all(|f| {
                    !boxed_field(tys, t, None, f)
                        && tys
                            .field_ty(t, None, f)
                            .is_none_or(|ft| may_be_empty(tys, ft, seen))
                })
            }
        },
        _ => false,
    }
}

/// Whether field `f` of the plain ADT `t` (seen as `variant` for an enum)
/// is held in a box of its own: a pointer to a malloc'd value of the
/// field's type. A field is boxed when its type holds `t` again by value
/// (`struct Node { next: Option[Node] }`, `enum Expr { Add(Expr, Expr) }`),
/// which would otherwise have no size. `Option` and `Result` never box
/// their payloads: the user type they hold boxes its own field instead.
pub fn boxed_field(tys: &TyInterner, t: Ty, variant: Option<u32>, f: u32) -> bool {
    let TyKind::Adt(a) = tys.kind(t) else {
        return false;
    };
    if matches!(tys.adt(a).name.as_str(), "Option" | "Result") || library_cell(tys, t).is_some() {
        return false;
    }
    let Some(ft) = tys.field_ty(t, variant, f) else {
        return false;
    };
    let mut seen = Vec::new();
    holds(tys, ft, t, &mut seen)
}

/// Whether a value of `x` holds a `target` in place, through fields,
/// tuple and array elements and captures (never through a pointer).
fn holds(tys: &TyInterner, x: Ty, target: Ty, seen: &mut Vec<Ty>) -> bool {
    if x == target {
        return true;
    }
    if seen.contains(&x) {
        return false;
    }
    seen.push(x);
    match tys.kind(x) {
        TyKind::Tuple(ts) | TyKind::Closure(_, ts) => {
            ts.into_iter().any(|t| holds(tys, t, target, seen))
        }
        TyKind::Array(e, _) => holds(tys, e, target, seen),
        TyKind::Adt(_) => match library_cell(tys, x) {
            Some(Cell::Handle) => false,
            Some(Cell::Inline(h)) => holds(tys, h, target, seen),
            None => {
                let Some((adt, _)) = tys.tcx().adt_of(x) else {
                    return false;
                };
                let adt = adt.clone();
                (0..adt.variants.len()).any(|v| {
                    let var = adt.is_enum.then_some(v as u32);
                    (0..adt.variants[v].fields.len() as u32)
                        .filter_map(|f| tys.field_ty(x, var, f))
                        .any(|ft| holds(tys, ft, target, seen))
                })
            }
        },
        _ => false,
    }
}
