//! How a value of a plain C type crosses an `extern "C"` call by value
//! (design.md §15: a `#[repr(C)]` struct, and a `#[repr(transparent)]`
//! wrapper, in a foreign signature; FF3). One classification, read by the
//! MIR interpreter (`interp::foreign_call`) and the LLVM backend
//! (`backend::mir::foreign`), so the two lanes pass and receive a struct the
//! same way, and the way the platform's C compiler does:
//!
//! - **x86-64 System V** (Linux, macOS): a struct of at most 16 bytes goes in
//!   one register per eightbyte, an SSE one when everything in that eightbyte
//!   is a float, else an integer one; a larger one is copied onto the stack
//!   (`byval`) as an argument and returned through a hidden pointer (`sret`).
//!   A struct whose eightbytes do not all fit in the registers left goes on
//!   the stack whole.
//! - **AArch64** (AAPCS64, and Apple's, which agrees for these): a struct of
//!   one to four members of one float type (an HFA) goes one member per
//!   float register; any other struct of at most 16 bytes in one or two
//!   integer registers; a larger one as a pointer to a copy the caller
//!   makes, and returned through the hidden pointer in `x8`.
//!
//! A struct with a single scalar field (a `#[repr(transparent)]` wrapper, a
//! `distinct type`) crosses exactly as that scalar does, which is also what
//! both ABIs do with a one-field `#[repr(C)]` struct.

use super::layout;
use super::ty::{FloatTy, IntTy, Ty, TyInterner, TyKind};

/// One register's worth of a struct: the bytes `[offset, offset + size)`,
/// in a float register when `float`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Piece {
    pub offset: u64,
    pub size: u64,
    pub float: bool,
}

/// How a struct crosses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pass {
    /// In registers, one per piece.
    Regs(Vec<Piece>),
    /// As a pointer to a copy the caller makes (an AArch64 argument over
    /// 16 bytes).
    Indirect,
    /// On the stack, copied (`byval`; an x86-64 argument over 16 bytes or
    /// one that no longer fits the registers).
    Stack,
    /// A result, through a hidden pointer to the caller's slot (`sret`).
    Sret,
}

/// The C ABIs a struct can cross by value on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Abi {
    SysV,
    Aapcs64,
}

impl Abi {
    /// The ABI of the platform this compiler runs on, which is the one
    /// every native build targets; `None` where no by-value struct crosses
    /// yet (Windows, 32-bit, wasm).
    pub fn host() -> Option<Abi> {
        if cfg!(windows) || crate::target::active_target_is_wasm() {
            None
        } else if cfg!(target_arch = "x86_64") {
            Some(Abi::SysV)
        } else if cfg!(target_arch = "aarch64") {
            Some(Abi::Aapcs64)
        } else {
            None
        }
    }

    /// The argument registers of each kind: integer, float.
    pub fn registers(self) -> (usize, usize) {
        match self {
            Abi::SysV => (6, 8),
            Abi::Aapcs64 => (8, 8),
        }
    }
}

/// A scalar leaf of a struct, at its offset.
#[derive(Debug, Clone, Copy)]
struct Leaf {
    offset: u64,
    size: u64,
    float: Option<FloatTy>,
}

/// Whether `t` may cross an `extern "C"` signature: an integer, `bool`,
/// `char`, `f32`, `f64` or raw pointer, or a struct of those (and of fixed arrays
/// and structs of them) laid out as C lays it out.
pub fn crosses(tys: &TyInterner, t: Ty) -> bool {
    match tys.kind(t) {
        // `char` crosses as `uint32_t` (C's `char32_t`), a Unicode scalar
        // value (review-thread ruling 2026-11-16, B-2026-11-16-249).
        TyKind::Int(_) | TyKind::Bool | TyKind::Char | TyKind::RawPtr { .. } => true,
        TyKind::Float(f) => matches!(f, FloatTy::F32 | FloatTy::F64),
        TyKind::Adt(_) => plain_struct(tys, t) || c_enum_int(tys, t).is_some(),
        _ => false,
    }
}

/// The integer a C-like `#[repr(intN)]` or `#[repr(C)]` enum crosses as:
/// its C tag, which holds the declared discriminant (design.md §15,
/// B-2026-11-16-233).
pub fn c_enum_int(tys: &TyInterner, t: Ty) -> Option<Ty> {
    if !matches!(tys.kind(t), TyKind::Adt(_)) {
        return None;
    }
    let (adt, _) = tys.tcx().adt_of(t)?;
    if !adt.is_enum {
        return None;
    }
    let c = tys.tcx().c_tag(adt.def)?;
    let it = match (c.bytes, c.signed) {
        (1, true) => IntTy::I8,
        (1, false) => IntTy::U8,
        (2, true) => IntTy::I16,
        (2, false) => IntTy::U16,
        (8, true) => IntTy::I64,
        (8, false) => IntTy::U64,
        (_, true) => IntTy::I32,
        (_, false) => IntTy::U32,
    };
    Some(tys.intern(TyKind::Int(it)))
}

/// A non-generic-cell, non-enum struct whose every field is plain data.
pub fn plain_struct(tys: &TyInterner, t: Ty) -> bool {
    if layout::library_cell(tys, t).is_some() {
        return false;
    }
    let Some((adt, _)) = tys.tcx().adt_of(t) else {
        return false;
    };
    if adt.is_enum {
        return false;
    }
    let n = adt.variants[0].fields.len() as u32;
    (0..n).all(|f| {
        !layout::boxed_field(tys, t, None, f)
            && tys
                .field_ty(t, None, f)
                .is_some_and(|ft| field_plain(tys, ft))
    })
}

fn field_plain(tys: &TyInterner, t: Ty) -> bool {
    match tys.kind(t) {
        TyKind::Array(e, _) => field_plain(tys, e),
        _ => crosses(tys, t),
    }
}

/// The one scalar field a struct crosses as, when it has exactly one and
/// it is a scalar (through nested one-field structs).
pub fn single_scalar(tys: &TyInterner, t: Ty) -> Option<Ty> {
    let mut t = t;
    loop {
        match tys.kind(t) {
            TyKind::Adt(_) => {
                let (adt, _) = tys.tcx().adt_of(t)?;
                if adt.is_enum || adt.variants[0].fields.len() != 1 {
                    return None;
                }
                t = tys.field_ty(t, None, 0)?;
            }
            TyKind::Int(_)
            | TyKind::Bool
            | TyKind::Char
            | TyKind::RawPtr { .. }
            | TyKind::Float(_) => return Some(t),
            _ => return None,
        }
    }
}

fn leaves(tys: &TyInterner, t: Ty, base: u64, out: &mut Vec<Leaf>) -> Option<()> {
    match tys.kind(t) {
        TyKind::Float(f) => out.push(Leaf {
            offset: base,
            size: layout::layout(tys, t)?.size,
            float: Some(f),
        }),
        TyKind::Int(_) | TyKind::Bool | TyKind::Char | TyKind::RawPtr { .. } => out.push(Leaf {
            offset: base,
            size: layout::layout(tys, t)?.size,
            float: None,
        }),
        TyKind::Adt(_) if c_enum_int(tys, t).is_some() => out.push(Leaf {
            offset: base,
            size: layout::layout(tys, t)?.size,
            float: None,
        }),
        TyKind::Array(e, n) => {
            let size = layout::layout(tys, e)?.size;
            for i in 0..n {
                leaves(tys, e, base + i * size, out)?;
            }
        }
        TyKind::Adt(_) => {
            let (adt, _) = tys.tcx().adt_of(t)?;
            for f in 0..adt.variants[0].fields.len() as u32 {
                let at = layout::field_offset(tys, t, None, f)?;
                leaves(tys, tys.field_ty(t, None, f)?, base + at, out)?;
            }
        }
        _ => return None,
    }
    Some(())
}

/// How the struct `t` crosses as an argument (`result` false) or a result,
/// with `free` the integer and float argument registers still unused (an
/// argument that does not fit them goes on the stack whole). `None` for a
/// type no ABI here passes by value.
pub fn classify(
    tys: &TyInterner,
    abi: Abi,
    t: Ty,
    result: bool,
    free: (usize, usize),
) -> Option<Pass> {
    let size = layout::layout(tys, t)?.size;
    let mut ls = Vec::new();
    leaves(tys, t, 0, &mut ls)?;
    let big = if result {
        Pass::Sret
    } else if abi == Abi::SysV {
        Pass::Stack
    } else {
        Pass::Indirect
    };
    if size == 0 {
        return None;
    }
    let pieces = match abi {
        Abi::SysV => {
            if size > 16 {
                return Some(big);
            }
            (0..size.div_ceil(8))
                .map(|i| {
                    let lo = i * 8;
                    let hi = (lo + 8).min(size);
                    let float = ls
                        .iter()
                        .filter(|l| l.offset < hi && l.offset + l.size > lo)
                        .all(|l| l.float.is_some());
                    Piece {
                        offset: lo,
                        size: hi - lo,
                        float,
                    }
                })
                .collect::<Vec<_>>()
        }
        Abi::Aapcs64 => {
            let hfa = ls.len() <= 4
                && ls
                    .first()
                    .and_then(|l| l.float)
                    .is_some_and(|f| ls.iter().all(|l| l.float == Some(f)));
            if hfa {
                ls.iter()
                    .map(|l| Piece {
                        offset: l.offset,
                        size: l.size,
                        float: true,
                    })
                    .collect()
            } else if size > 16 {
                return Some(big);
            } else {
                (0..size.div_ceil(8))
                    .map(|i| Piece {
                        offset: i * 8,
                        size: (size - i * 8).min(8),
                        float: false,
                    })
                    .collect()
            }
        }
    };
    if !result {
        let ints = pieces.iter().filter(|p| !p.float).count();
        let floats = pieces.len() - ints;
        if ints > free.0 || floats > free.1 {
            return Some(match abi {
                Abi::SysV => Pass::Stack,
                // AArch64 copies a small struct onto the stack too, which
                // the backend's consecutive-register arrays already do.
                Abi::Aapcs64 => Pass::Regs(pieces),
            });
        }
    }
    Some(Pass::Regs(pieces))
}

#[cfg(test)]
mod tests {
    use super::super::ty::{AdtDef, IntTy, VariantDef};
    use super::*;
    use crate::ids::DefId;

    fn p(offset: u64, size: u64, float: bool) -> Piece {
        Piece {
            offset,
            size,
            float,
        }
    }

    /// The classification of a `{ a: A, b: B }` argument.
    fn two(tys: &TyInterner, def: u32, a: Ty, b: Ty, abi: Abi) -> Option<Pass> {
        let id = tys.add_adt(AdtDef {
            def: DefId(def),
            name: format!("S{def}"),
            is_enum: false,
            variants: vec![VariantDef {
                name: format!("S{def}"),
                fields: vec![("a".into(), a), ("b".into(), b)],
            }],
            has_drop_impl: false,
            is_copy: true,
            user: true,
            sync: false,
        });
        let t = tys.intern(TyKind::Adt(id));
        classify(tys, abi, t, false, abi.registers())
    }

    #[test]
    fn c_abi_sysv_eightbytes() {
        let tys = TyInterner::new();
        let i32t = tys.int(IntTy::I32);
        let i64t = tys.int(IntTy::I64);
        let f32t = tys.intern(TyKind::Float(FloatTy::F32));
        let f64t = tys.intern(TyKind::Float(FloatTy::F64));
        let sysv = Abi::SysV;
        assert_eq!(
            two(&tys, 900, i32t, i32t, sysv),
            Some(Pass::Regs(vec![p(0, 8, false)]))
        );
        assert_eq!(
            two(&tys, 901, i64t, i64t, sysv),
            Some(Pass::Regs(vec![p(0, 8, false), p(8, 8, false)]))
        );
        assert_eq!(
            two(&tys, 902, f64t, i32t, sysv),
            Some(Pass::Regs(vec![p(0, 8, true), p(8, 8, false)]))
        );
        assert_eq!(
            two(&tys, 903, f32t, f32t, sysv),
            Some(Pass::Regs(vec![p(0, 8, true)]))
        );
        assert_eq!(
            two(&tys, 904, f32t, i32t, sysv),
            Some(Pass::Regs(vec![p(0, 8, false)]))
        );
    }

    #[test]
    fn c_abi_aapcs64_hfa_and_words() {
        let tys = TyInterner::new();
        let i32t = tys.int(IntTy::I32);
        let f32t = tys.intern(TyKind::Float(FloatTy::F32));
        let f64t = tys.intern(TyKind::Float(FloatTy::F64));
        let a64 = Abi::Aapcs64;
        assert_eq!(
            two(&tys, 910, f32t, f32t, a64),
            Some(Pass::Regs(vec![p(0, 4, true), p(4, 4, true)]))
        );
        assert_eq!(
            two(&tys, 911, f64t, f64t, a64),
            Some(Pass::Regs(vec![p(0, 8, true), p(8, 8, true)]))
        );
        assert_eq!(
            two(&tys, 912, f32t, i32t, a64),
            Some(Pass::Regs(vec![p(0, 8, false)]))
        );
    }
}
