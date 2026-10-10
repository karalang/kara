//! MIR types: the shared interned [`crate::ty::Ty`], seen through MIR's own
//! vocabulary.
//!
//! The handles and their storage are the shared [`TyCtxt`]'s, so a type the
//! MIR builder takes from typed HIR is the same handle here. ADTs are
//! registered by [`DefId`] ([`crate::ty::AdtDef`]); an [`AdtId`] is that
//! DefId's index. MIR is built after monomorphisation, so the types MIR
//! works with are concrete (no `Param`): an ADT instance is `(def, args)`,
//! and [`TyInterner::field_ty`] instantiates field types with the args.
//!
//! [`TyKind`] here is a VIEW, built on demand by [`TyInterner::kind`]: it
//! names the integer widths MIR distinguishes as one [`IntTy`], spells
//! ADTs by [`AdtId`], and carries tuple elements and captures as vectors.
//! Interning a view converts it back. An `Adt(id)` view stands for the
//! instance with no arguments; types with arguments come from typed HIR or
//! from [`TyInterner::tcx`] directly.

use std::rc::Rc;

use crate::ids::DefId;
pub use crate::ty::{AdtDef, IntrinsicKind, Ty, TyCtxt, VariantDef};
use crate::ty::{ArrayLen, TyKind as SharedKind};
use crate::typechecker::types::{FloatSize, IntSize, UIntSize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IntTy {
    I8,
    I16,
    I32,
    I64,
    I128,
    Isize,
    U8,
    U16,
    U32,
    U64,
    U128,
    Usize,
}

impl IntTy {
    pub fn name(self) -> &'static str {
        match self {
            IntTy::I8 => "i8",
            IntTy::I16 => "i16",
            IntTy::I32 => "i32",
            IntTy::I64 => "i64",
            IntTy::I128 => "i128",
            IntTy::Isize => "isize",
            IntTy::U8 => "u8",
            IntTy::U16 => "u16",
            IntTy::U32 => "u32",
            IntTy::U64 => "u64",
            IntTy::U128 => "u128",
            IntTy::Usize => "usize",
        }
    }

    pub fn bits(self) -> u32 {
        match self {
            IntTy::I8 | IntTy::U8 => 8,
            IntTy::I16 | IntTy::U16 => 16,
            IntTy::I32 | IntTy::U32 => 32,
            IntTy::I64 | IntTy::U64 | IntTy::Isize | IntTy::Usize => 64,
            IntTy::I128 | IntTy::U128 => 128,
        }
    }

    pub fn signed(self) -> bool {
        matches!(
            self,
            IntTy::I8 | IntTy::I16 | IntTy::I32 | IntTy::I64 | IntTy::I128 | IntTy::Isize
        )
    }
}

impl IntTy {
    fn to_shared(self) -> SharedKind {
        match self {
            IntTy::I8 => SharedKind::Int(IntSize::I8),
            IntTy::I16 => SharedKind::Int(IntSize::I16),
            IntTy::I32 => SharedKind::Int(IntSize::I32),
            IntTy::I64 => SharedKind::Int(IntSize::I64),
            IntTy::I128 => SharedKind::Int(IntSize::I128),
            IntTy::Isize => SharedKind::Int(IntSize::Isize),
            IntTy::U8 => SharedKind::UInt(UIntSize::U8),
            IntTy::U16 => SharedKind::UInt(UIntSize::U16),
            IntTy::U32 => SharedKind::UInt(UIntSize::U32),
            IntTy::U64 => SharedKind::UInt(UIntSize::U64),
            IntTy::U128 => SharedKind::UInt(UIntSize::U128),
            IntTy::Usize => SharedKind::UInt(UIntSize::Usize),
        }
    }

    fn from_shared(k: SharedKind) -> Option<IntTy> {
        Some(match k {
            SharedKind::Int(IntSize::I8) => IntTy::I8,
            SharedKind::Int(IntSize::I16) => IntTy::I16,
            SharedKind::Int(IntSize::I32) => IntTy::I32,
            SharedKind::Int(IntSize::I64) => IntTy::I64,
            SharedKind::Int(IntSize::I128) => IntTy::I128,
            SharedKind::Int(IntSize::Isize) => IntTy::Isize,
            SharedKind::UInt(UIntSize::U8) => IntTy::U8,
            SharedKind::UInt(UIntSize::U16) => IntTy::U16,
            SharedKind::UInt(UIntSize::U32) => IntTy::U32,
            SharedKind::UInt(UIntSize::U64) => IntTy::U64,
            SharedKind::UInt(UIntSize::U128) => IntTy::U128,
            SharedKind::UInt(UIntSize::Usize) => IntTy::Usize,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FloatTy {
    F16,
    BF16,
    F32,
    F64,
}

impl FloatTy {
    pub fn name(self) -> &'static str {
        match self {
            FloatTy::F16 => "f16",
            FloatTy::BF16 => "bf16",
            FloatTy::F32 => "f32",
            FloatTy::F64 => "f64",
        }
    }

    /// `x` rounded to this type's precision (to nearest, ties to even),
    /// as the compiled backends round it. `f16` goes through `f32` first,
    /// as the legacy interpreter does; `bf16` drops the low half of the
    /// `f32` bits.
    pub fn round(self, x: f64) -> f64 {
        match self {
            FloatTy::F16 => f16_bits_to_f64(f32_to_f16_bits(x as f32)),
            FloatTy::BF16 => {
                let f = x as f32;
                let bits = f.to_bits();
                let out: u16 = if f.is_nan() {
                    ((bits | 0x0040_0000) >> 16) as u16
                } else {
                    let lsb = (bits >> 16) & 1;
                    (bits.wrapping_add(0x7FFF + lsb) >> 16) as u16
                };
                f32::from_bits((out as u32) << 16) as f64
            }
            FloatTy::F32 => x as f32 as f64,
            FloatTy::F64 => x,
        }
    }
}

// The f16 conversions, copied from the legacy interpreter
// (`interpreter/eval_expr.rs`), which is frozen and goes away at M3.

/// f32 → f16 bit pattern with round-to-nearest-even, handling
/// Inf/NaN/overflow/subnormals/underflow.
fn f32_to_f16_bits(x: f32) -> u16 {
    let b = x.to_bits();
    let sign = ((b >> 16) & 0x8000) as u16;
    let exp = ((b >> 23) & 0xFF) as i32;
    let frac = b & 0x007F_FFFF;
    if exp == 0xFF {
        // Inf / NaN (quiet the NaN, keep the top payload bits)
        return if frac != 0 {
            sign | 0x7E00 | ((frac >> 13) as u16)
        } else {
            sign | 0x7C00
        };
    }
    if exp == 0 && frac == 0 {
        return sign;
    }
    let e = exp - 127;
    if e >= 16 {
        return sign | 0x7C00; // ≥ 2^16 → Inf
    }
    if e >= -14 {
        // Normal f16 range: 24-bit significand → 11 bits, RNE on the cut.
        let mant = 0x0080_0000 | frac;
        let base = mant >> 13;
        let rem = mant & 0x1FFF;
        let mut r = base;
        if rem > 0x1000 || (rem == 0x1000 && (base & 1) == 1) {
            r += 1;
        }
        let mut ee = (e + 15) as u32;
        let mut mm = r & 0x3FF;
        if r == 0x800 {
            // Mantissa carry (0x7FF+1): bump the exponent.
            ee += 1;
            mm = 0;
        }
        if ee >= 31 {
            return sign | 0x7C00; // rounded past 65504 → Inf
        }
        return sign | ((ee as u16) << 10) | (mm as u16);
    }
    if e < -25 {
        return sign; // below half the smallest subnormal → ±0
    }
    // Subnormal f16: shift the significand into the subnormal position,
    // RNE on the cut. (`r` reaching 0x400 IS the smallest normal — the
    // bit pattern composes correctly.)
    let mant = 0x0080_0000u32 | frac;
    let shift = (13 + (-14 - e)) as u32;
    let base = mant >> shift;
    let rem = mant & ((1u32 << shift) - 1);
    let half = 1u32 << (shift - 1);
    let mut r = base as u16;
    if rem > half || (rem == half && (r & 1) == 1) {
        r += 1;
    }
    sign | r
}

/// f16 bit pattern → f64 (exact — every f16 is an f64).
fn f16_bits_to_f64(h: u16) -> f64 {
    let neg = h & 0x8000 != 0;
    let exp = ((h >> 10) & 0x1F) as i32;
    let mant = (h & 0x3FF) as f64;
    let v = if exp == 31 {
        if mant != 0.0 {
            f64::NAN
        } else {
            f64::INFINITY
        }
    } else if exp == 0 {
        mant * (2.0f64).powi(-24)
    } else {
        (1.0 + mant / 1024.0) * (2.0f64).powi(exp - 15)
    };
    if neg {
        -v
    } else {
        v
    }
}

/// An ADT's index: the [`DefId`] it is registered under.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AdtId(pub u32);

impl AdtId {
    pub fn def(self) -> DefId {
        DefId(self.0)
    }
}

/// MIR's view of a type. See the module docs.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TyKind {
    Bool,
    Char,
    Int(IntTy),
    Float(FloatTy),
    Unit,
    Never,
    /// A static string slice (`"..."` literal).
    Str,
    Tuple(Vec<Ty>),
    Array(Ty, u64),
    Slice(Ty),
    /// A user struct or enum.
    Adt(AdtId),
    /// A `shared struct` / `shared enum` handle (reference counted).
    Shared(AdtId),
    /// A `weak` handle (core semantics §6.5): the `shared` type it points
    /// at. It keeps the object's memory but not the object alive, is
    /// move-only, and is read through an `Upgrade` cast.
    Weak(Ty),
    Ref(Ty),
    MutRef(Ty),
    /// A raw pointer (`*const T`, `*mut T`): `Copy`, holds no loan, and is
    /// read or written through only in `unsafe` code.
    RawPtr {
        pointee: Ty,
        mutable: bool,
    },
    /// A library collection the MIR interpreter implements natively
    /// (`String`, `Vec[T]`, `Map[K, V]`, ...): §8 open question 2.
    Intrinsic(IntrinsicTy),
    /// A function item; its value is zero-sized.
    FnDef(DefId),
    /// A closure; its fields are the captures, in capture order.
    Closure(DefId, Vec<Ty>),
    /// An erased function value (`Fn(A) -> R`, `MutFn`, `OnceFn`): a
    /// closure or function item whose type is forgotten. It is move-only,
    /// owns its closure's captures, and has no parts MIR can project.
    Fn {
        params: Vec<Ty>,
        ret: Ty,
        kind: FnKind,
    },
    /// A shared kind MIR has no view of (a generic parameter, a function
    /// pointer, a raw pointer, ...): never valid in a MIR body.
    Other,
}

/// How an erased function value may be called (core semantics §9.6):
/// through `ref`, through `mut ref`, or once by value. Each kind accepts
/// the ones before it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum FnKind {
    Fn,
    MutFn,
    OnceFn,
}

impl FnKind {
    pub fn name(self) -> &'static str {
        match self {
            FnKind::Fn => "Fn",
            FnKind::MutFn => "MutFn",
            FnKind::OnceFn => "OnceFn",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum IntrinsicTy {
    String,
    Vec(Ty),
    Map(Ty, Ty),
    Set(Ty),
    VecDeque(Ty),
    /// A map kept in key order.
    SortedMap(Ty, Ty),
    /// A set kept in key order.
    SortedSet(Ty),
}

/// MIR's handle on the shared type context.
#[derive(Default)]
pub struct TyInterner {
    tcx: TyCtxt,
}

impl std::fmt::Debug for TyInterner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TyInterner")
    }
}

impl TyInterner {
    pub fn new() -> Self {
        Self::default()
    }

    /// Build MIR over an existing context, such as typed HIR's.
    pub fn from_tcx(tcx: TyCtxt) -> Self {
        TyInterner { tcx }
    }

    /// The shared context underneath.
    pub fn tcx(&self) -> &TyCtxt {
        &self.tcx
    }

    pub fn intern(&self, kind: TyKind) -> Ty {
        let tcx = &self.tcx;
        let none = || tcx.empty_list();
        let shared = match kind {
            TyKind::Bool => SharedKind::Bool,
            TyKind::Char => SharedKind::Char,
            TyKind::Int(i) => i.to_shared(),
            TyKind::Float(FloatTy::F16) => SharedKind::Float(FloatSize::F16),
            TyKind::Float(FloatTy::BF16) => SharedKind::Float(FloatSize::BF16),
            TyKind::Float(FloatTy::F32) => SharedKind::Float(FloatSize::F32),
            TyKind::Float(FloatTy::F64) => SharedKind::Float(FloatSize::F64),
            TyKind::Unit => SharedKind::Unit,
            TyKind::Never => SharedKind::Never,
            TyKind::Str => SharedKind::StaticStr,
            TyKind::Tuple(ts) => SharedKind::Tuple(tcx.intern_list(&ts)),
            TyKind::Array(e, n) => SharedKind::Array {
                elem: e,
                len: ArrayLen::Known(n),
            },
            TyKind::Slice(e) => SharedKind::Slice {
                elem: e,
                mutable: false,
            },
            TyKind::Adt(a) => SharedKind::Adt {
                def: a.def(),
                args: none(),
            },
            TyKind::Shared(a) => SharedKind::Shared {
                def: a.def(),
                args: none(),
            },
            TyKind::Weak(t) => SharedKind::Weak(t),
            TyKind::Ref(t) => SharedKind::Ref(t),
            TyKind::MutRef(t) => SharedKind::MutRef(t),
            TyKind::RawPtr { pointee, mutable } => SharedKind::RawPtr { pointee, mutable },
            TyKind::Intrinsic(IntrinsicTy::String) => SharedKind::Str,
            TyKind::Intrinsic(IntrinsicTy::Vec(e)) => SharedKind::Intrinsic {
                kind: IntrinsicKind::Vec,
                args: tcx.intern_list(&[e]),
            },
            TyKind::Intrinsic(IntrinsicTy::Map(k, v)) => SharedKind::Intrinsic {
                kind: IntrinsicKind::Map,
                args: tcx.intern_list(&[k, v]),
            },
            TyKind::Intrinsic(IntrinsicTy::Set(e)) => SharedKind::Intrinsic {
                kind: IntrinsicKind::Set,
                args: tcx.intern_list(&[e]),
            },
            TyKind::Intrinsic(IntrinsicTy::VecDeque(e)) => SharedKind::Intrinsic {
                kind: IntrinsicKind::VecDeque,
                args: tcx.intern_list(&[e]),
            },
            TyKind::Intrinsic(IntrinsicTy::SortedMap(k, v)) => SharedKind::Intrinsic {
                kind: IntrinsicKind::SortedMap,
                args: tcx.intern_list(&[k, v]),
            },
            TyKind::Intrinsic(IntrinsicTy::SortedSet(e)) => SharedKind::Intrinsic {
                kind: IntrinsicKind::SortedSet,
                args: tcx.intern_list(&[e]),
            },
            TyKind::FnDef(d) => SharedKind::FnDef {
                def: d,
                args: none(),
            },
            TyKind::Closure(d, caps) => SharedKind::Closure {
                def: d,
                captures: tcx.intern_list(&caps),
            },
            TyKind::Fn { params, ret, kind } => SharedKind::Fn {
                params: tcx.intern_list(&params),
                ret,
                once: kind == FnKind::OnceFn,
                mutable: kind == FnKind::MutFn,
            },
            TyKind::Other => SharedKind::Error,
        };
        tcx.intern(shared)
    }

    /// Whether `t` is a `Vector[T, N]`, whose operators work lane by lane
    /// (`a + b`, and `a < b` giving `Vector[bool, N]`); MIR otherwise sees
    /// it as `Array[T, N]`.
    pub fn is_vector(&self, t: Ty) -> bool {
        matches!(self.tcx.kind(t), SharedKind::Vector { .. })
    }

    /// MIR's view of `t`.
    pub fn kind(&self, t: Ty) -> TyKind {
        let tcx = &self.tcx;
        match tcx.kind(t) {
            SharedKind::Bool => TyKind::Bool,
            SharedKind::Char => TyKind::Char,
            k @ (SharedKind::Int(_) | SharedKind::UInt(_)) => match IntTy::from_shared(k) {
                Some(i) => TyKind::Int(i),
                None => TyKind::Other,
            },
            SharedKind::Float(FloatSize::F16) => TyKind::Float(FloatTy::F16),
            SharedKind::Float(FloatSize::BF16) => TyKind::Float(FloatTy::BF16),
            SharedKind::Float(FloatSize::F32) => TyKind::Float(FloatTy::F32),
            SharedKind::Float(FloatSize::F64) => TyKind::Float(FloatTy::F64),
            SharedKind::Unit => TyKind::Unit,
            SharedKind::Never => TyKind::Never,
            SharedKind::StaticStr => TyKind::Str,
            SharedKind::Str => TyKind::Intrinsic(IntrinsicTy::String),
            SharedKind::Tuple(l) => TyKind::Tuple(tcx.list(l)),
            // A lane vector is laid out and read as its lanes' array;
            // `is_vector` tells the operators apart.
            SharedKind::Array {
                elem,
                len: ArrayLen::Known(n),
            }
            | SharedKind::Vector {
                elem,
                lanes: ArrayLen::Known(n),
            } => TyKind::Array(elem, n),
            SharedKind::Slice { elem, .. } => TyKind::Slice(elem),
            SharedKind::Adt { def, .. } => TyKind::Adt(AdtId(def.0)),
            SharedKind::Shared { def, .. } => TyKind::Shared(AdtId(def.0)),
            SharedKind::Weak(t) if matches!(tcx.kind(t), SharedKind::Shared { .. }) => {
                TyKind::Weak(t)
            }
            SharedKind::Ref(t) => TyKind::Ref(t),
            SharedKind::MutRef(t) => TyKind::MutRef(t),
            SharedKind::RawPtr { pointee, mutable } => TyKind::RawPtr { pointee, mutable },
            SharedKind::Intrinsic { kind, args } => {
                let a = tcx.list(args);
                match (kind, a.as_slice()) {
                    (IntrinsicKind::Vec, [e]) => TyKind::Intrinsic(IntrinsicTy::Vec(*e)),
                    (IntrinsicKind::Map, [k, v]) => TyKind::Intrinsic(IntrinsicTy::Map(*k, *v)),
                    (IntrinsicKind::Set, [e]) => TyKind::Intrinsic(IntrinsicTy::Set(*e)),
                    (IntrinsicKind::VecDeque, [e]) => TyKind::Intrinsic(IntrinsicTy::VecDeque(*e)),
                    (IntrinsicKind::SortedMap, [k, v]) => {
                        TyKind::Intrinsic(IntrinsicTy::SortedMap(*k, *v))
                    }
                    (IntrinsicKind::SortedSet, [e]) => {
                        TyKind::Intrinsic(IntrinsicTy::SortedSet(*e))
                    }
                    _ => TyKind::Other,
                }
            }
            SharedKind::FnDef { def, .. } => TyKind::FnDef(def),
            SharedKind::Closure { def, captures } => TyKind::Closure(def, tcx.list(captures)),
            SharedKind::Fn {
                params,
                ret,
                once,
                mutable,
            } => TyKind::Fn {
                params: tcx.list(params),
                ret,
                kind: if once {
                    FnKind::OnceFn
                } else if mutable {
                    FnKind::MutFn
                } else {
                    FnKind::Fn
                },
            },
            _ => TyKind::Other,
        }
    }

    /// Register `adt` under its DefId, which is its [`AdtId`].
    pub fn add_adt(&self, adt: AdtDef) -> AdtId {
        let id = AdtId(adt.def.0);
        self.tcx.add_adt_def(adt);
        id
    }

    /// The definition behind `id`. Panics when nothing is registered, as an
    /// index past the end did.
    pub fn adt(&self, id: AdtId) -> Rc<AdtDef> {
        self.tcx
            .adt_def(id.def())
            .unwrap_or_else(|| panic!("no ADT registered for {id:?}"))
    }

    // Shorthands for the common leaves.
    pub fn unit(&self) -> Ty {
        self.tcx.unit()
    }
    pub fn bool(&self) -> Ty {
        self.tcx.bool()
    }
    pub fn int(&self, i: IntTy) -> Ty {
        self.tcx.intern(i.to_shared())
    }

    /// `Copy` per core semantics §1.1: primitives, and tuples / arrays /
    /// ADTs whose parts are `Copy` (an ADT only when derived).
    pub fn is_copy(&self, t: Ty) -> bool {
        self.tcx.is_copy(t)
    }

    /// Core semantics §1.4: owns heap memory, holds a shared handle, or
    /// has a `Drop` body anywhere inside.
    pub fn needs_drop(&self, t: Ty) -> bool {
        self.tcx.needs_drop(t)
    }

    /// Does this type itself have a user `Drop` body (not counting parts)?
    pub fn has_drop_impl(&self, t: Ty) -> bool {
        self.tcx.has_drop_impl(t)
    }

    /// The type of field `f` of `t` (seen as `variant` when `t` is an
    /// enum), or `None` when `t` has no such field.
    pub fn field_ty(&self, t: Ty, variant: Option<u32>, f: u32) -> Option<Ty> {
        self.tcx.field_ty(t, variant, f)
    }

    /// The MIR text form of `t`.
    pub fn display(&self, t: Ty) -> String {
        match self.kind(t) {
            TyKind::Bool => "bool".into(),
            TyKind::Char => "char".into(),
            TyKind::Int(i) => i.name().into(),
            TyKind::Float(f) => f.name().into(),
            TyKind::Unit => "()".into(),
            TyKind::Never => "!".into(),
            TyKind::Str => "str".into(),
            TyKind::Tuple(ts) => {
                let parts: Vec<String> = ts.iter().map(|&t| self.display(t)).collect();
                format!("({})", parts.join(", "))
            }
            TyKind::Array(e, n) if self.is_vector(t) => {
                format!("Vector[{}, {}]", self.display(e), n)
            }
            TyKind::Array(e, n) => format!("Array[{}, {}]", self.display(e), n),
            TyKind::Slice(e) => format!("Slice[{}]", self.display(e)),
            TyKind::Adt(_) => self.adt_name(t),
            TyKind::Shared(_) => format!("shared {}", self.adt_name(t)),
            TyKind::Weak(s) => format!("weak {}", self.adt_name(s)),
            TyKind::Ref(t) => format!("ref {}", self.display(t)),
            TyKind::MutRef(t) => format!("mut ref {}", self.display(t)),
            TyKind::RawPtr { pointee, mutable } => {
                let m = if mutable { "mut" } else { "const" };
                format!("*{m} {}", self.display(pointee))
            }
            TyKind::Intrinsic(IntrinsicTy::String) => "String".into(),
            TyKind::Intrinsic(IntrinsicTy::Vec(e)) => format!("Vec[{}]", self.display(e)),
            TyKind::Intrinsic(IntrinsicTy::Map(k, v)) => {
                format!("Map[{}, {}]", self.display(k), self.display(v))
            }
            TyKind::Intrinsic(IntrinsicTy::Set(e)) => format!("Set[{}]", self.display(e)),
            TyKind::Intrinsic(IntrinsicTy::VecDeque(e)) => {
                format!("VecDeque[{}]", self.display(e))
            }
            TyKind::Intrinsic(IntrinsicTy::SortedMap(k, v)) => {
                format!("SortedMap[{}, {}]", self.display(k), self.display(v))
            }
            TyKind::Intrinsic(IntrinsicTy::SortedSet(e)) => {
                format!("SortedSet[{}]", self.display(e))
            }
            TyKind::FnDef(d) => format!("fn#{}", d.0),
            TyKind::Closure(d, caps) => {
                let parts: Vec<String> = caps.iter().map(|&t| self.display(t)).collect();
                format!("closure#{}({})", d.0, parts.join(", "))
            }
            TyKind::Fn { params, ret, kind } => {
                let parts: Vec<String> = params.iter().map(|&t| self.display(t)).collect();
                format!(
                    "{}({}) -> {}",
                    kind.name(),
                    parts.join(", "),
                    self.display(ret)
                )
            }
            TyKind::Other => self.tcx.display(t, &|d| format!("def#{}", d.0)),
        }
    }

    /// An ADT type's name: its definition's, followed by the instance
    /// arguments when it has any.
    pub fn adt_name(&self, t: Ty) -> String {
        let Some((adt, args)) = self.tcx.adt_of(t) else {
            return self.tcx.display(t, &|d| format!("def#{}", d.0));
        };
        if args.is_empty() {
            adt.name.clone()
        } else {
            let parts: Vec<String> = args.iter().map(|&a| self.display(a)).collect();
            format!("{}[{}]", adt.name, parts.join(", "))
        }
    }
}
