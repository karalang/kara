//! MIR types: interned, monomorphic.
//!
//! A [`Ty`] is a `Copy` handle into a [`TyInterner`]. MIR is built after
//! monomorphisation, so there are no type parameters, projections or
//! inference variables here; the typed-HIR work will replace this
//! interner with the shared one, keeping the handle-plus-kind shape
//! (`docs/spikes/mir-types.md` §8, open question 1).

use std::collections::HashMap;

use crate::ids::DefId;

/// Handle to an interned [`TyKind`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Ty(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IntTy {
    I8,
    I16,
    I32,
    I64,
    U8,
    U16,
    U32,
    U64,
    Usize,
}

impl IntTy {
    pub fn name(self) -> &'static str {
        match self {
            IntTy::I8 => "i8",
            IntTy::I16 => "i16",
            IntTy::I32 => "i32",
            IntTy::I64 => "i64",
            IntTy::U8 => "u8",
            IntTy::U16 => "u16",
            IntTy::U32 => "u32",
            IntTy::U64 => "u64",
            IntTy::Usize => "usize",
        }
    }

    pub fn bits(self) -> u32 {
        match self {
            IntTy::I8 | IntTy::U8 => 8,
            IntTy::I16 | IntTy::U16 => 16,
            IntTy::I32 | IntTy::U32 => 32,
            IntTy::I64 | IntTy::U64 | IntTy::Usize => 64,
        }
    }

    pub fn signed(self) -> bool {
        matches!(self, IntTy::I8 | IntTy::I16 | IntTy::I32 | IntTy::I64)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FloatTy {
    F32,
    F64,
}

/// A user struct or enum definition as MIR needs it: monomorphic field
/// types per variant, and whether the type has a user `Drop` body. A
/// struct is a single-variant ADT.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AdtDef {
    pub def: DefId,
    pub name: String,
    pub is_enum: bool,
    pub variants: Vec<VariantDef>,
    pub has_drop_impl: bool,
    pub is_copy: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct VariantDef {
    pub name: String,
    pub fields: Vec<(String, Ty)>,
}

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
    /// A user struct or enum; index into [`TyInterner::adts`].
    Adt(AdtId),
    /// A `shared struct` / `shared enum` handle (reference counted).
    Shared(AdtId),
    Ref(Ty),
    MutRef(Ty),
    /// A library collection the MIR interpreter implements natively
    /// (`String`, `Vec[T]`, `Map[K, V]`, ...): §8 open question 2.
    Intrinsic(IntrinsicTy),
    /// A function item; its value is zero-sized.
    FnDef(DefId),
    /// A closure; its fields are the captures, in capture order.
    Closure(DefId, Vec<Ty>),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum IntrinsicTy {
    String,
    Vec(Ty),
    Map(Ty, Ty),
    Set(Ty),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AdtId(pub u32);

/// Interns [`TyKind`]s and holds the ADT definitions they refer to.
#[derive(Debug, Default)]
pub struct TyInterner {
    kinds: Vec<TyKind>,
    map: HashMap<TyKind, Ty>,
    adts: Vec<AdtDef>,
}

impl TyInterner {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn intern(&mut self, kind: TyKind) -> Ty {
        if let Some(&t) = self.map.get(&kind) {
            return t;
        }
        let t = Ty(self.kinds.len() as u32);
        self.kinds.push(kind.clone());
        self.map.insert(kind, t);
        t
    }

    pub fn kind(&self, t: Ty) -> &TyKind {
        &self.kinds[t.0 as usize]
    }

    pub fn add_adt(&mut self, adt: AdtDef) -> AdtId {
        let id = AdtId(self.adts.len() as u32);
        self.adts.push(adt);
        id
    }

    pub fn adt(&self, id: AdtId) -> &AdtDef {
        &self.adts[id.0 as usize]
    }

    // Shorthands for the common leaves.
    pub fn unit(&mut self) -> Ty {
        self.intern(TyKind::Unit)
    }
    pub fn bool(&mut self) -> Ty {
        self.intern(TyKind::Bool)
    }
    pub fn int(&mut self, i: IntTy) -> Ty {
        self.intern(TyKind::Int(i))
    }

    /// `Copy` per core semantics §1.1: primitives, and tuples / arrays /
    /// ADTs whose parts are `Copy` (an ADT only when derived).
    pub fn is_copy(&self, t: Ty) -> bool {
        match self.kind(t) {
            TyKind::Bool
            | TyKind::Char
            | TyKind::Int(_)
            | TyKind::Float(_)
            | TyKind::Unit
            | TyKind::Never
            | TyKind::Str
            | TyKind::Ref(_)
            | TyKind::FnDef(_) => true,
            TyKind::Tuple(ts) => ts.iter().all(|&t| self.is_copy(t)),
            TyKind::Array(e, _) => self.is_copy(*e),
            TyKind::Adt(a) => self.adt(*a).is_copy,
            TyKind::Closure(_, caps) => caps.iter().all(|&t| self.is_copy(t)),
            TyKind::Slice(_) | TyKind::Shared(_) | TyKind::MutRef(_) | TyKind::Intrinsic(_) => {
                false
            }
        }
    }

    /// Core semantics §1.4: owns heap memory, holds a shared handle, or
    /// has a `Drop` body anywhere inside.
    pub fn needs_drop(&self, t: Ty) -> bool {
        match self.kind(t) {
            TyKind::Shared(_) | TyKind::Intrinsic(_) => true,
            TyKind::Tuple(ts) | TyKind::Closure(_, ts) => ts.iter().any(|&t| self.needs_drop(t)),
            TyKind::Array(e, _) => self.needs_drop(*e),
            TyKind::Adt(a) => {
                let adt = self.adt(*a);
                adt.has_drop_impl
                    || adt
                        .variants
                        .iter()
                        .any(|v| v.fields.iter().any(|(_, t)| self.needs_drop(*t)))
            }
            _ => false,
        }
    }

    /// Does this type itself have a user `Drop` body (not counting parts)?
    pub fn has_drop_impl(&self, t: Ty) -> bool {
        matches!(self.kind(t), TyKind::Adt(a) if self.adt(*a).has_drop_impl)
    }

    /// The type of field `f` of `t` (seen as `variant` when `t` is an
    /// enum), or `None` when `t` has no such field.
    pub fn field_ty(&self, t: Ty, variant: Option<u32>, f: u32) -> Option<Ty> {
        match self.kind(t) {
            TyKind::Tuple(ts) | TyKind::Closure(_, ts) if variant.is_none() => {
                ts.get(f as usize).copied()
            }
            TyKind::Adt(a) | TyKind::Shared(a) => {
                let adt = self.adt(*a);
                let v = match (adt.is_enum, variant) {
                    (false, None) => adt.variants.first()?,
                    (true, Some(v)) => adt.variants.get(v as usize)?,
                    _ => return None,
                };
                v.fields.get(f as usize).map(|(_, t)| *t)
            }
            _ => None,
        }
    }

    pub fn display(&self, t: Ty) -> String {
        match self.kind(t) {
            TyKind::Bool => "bool".into(),
            TyKind::Char => "char".into(),
            TyKind::Int(i) => i.name().into(),
            TyKind::Float(FloatTy::F32) => "f32".into(),
            TyKind::Float(FloatTy::F64) => "f64".into(),
            TyKind::Unit => "()".into(),
            TyKind::Never => "!".into(),
            TyKind::Str => "str".into(),
            TyKind::Tuple(ts) => {
                let parts: Vec<String> = ts.iter().map(|&t| self.display(t)).collect();
                format!("({})", parts.join(", "))
            }
            TyKind::Array(e, n) => format!("Array[{}, {}]", self.display(*e), n),
            TyKind::Slice(e) => format!("Slice[{}]", self.display(*e)),
            TyKind::Adt(a) => self.adt(*a).name.clone(),
            TyKind::Shared(a) => format!("shared {}", self.adt(*a).name),
            TyKind::Ref(t) => format!("ref {}", self.display(*t)),
            TyKind::MutRef(t) => format!("mut ref {}", self.display(*t)),
            TyKind::Intrinsic(IntrinsicTy::String) => "String".into(),
            TyKind::Intrinsic(IntrinsicTy::Vec(e)) => format!("Vec[{}]", self.display(*e)),
            TyKind::Intrinsic(IntrinsicTy::Map(k, v)) => {
                format!("Map[{}, {}]", self.display(*k), self.display(*v))
            }
            TyKind::Intrinsic(IntrinsicTy::Set(e)) => format!("Set[{}]", self.display(*e)),
            TyKind::FnDef(d) => format!("fn#{}", d.0),
            TyKind::Closure(d, _) => format!("closure#{}", d.0),
        }
    }
}
