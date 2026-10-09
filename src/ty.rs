//! Interned types for the v2 middle end (`review/REDESIGN_PROPOSAL_2026-10-06.md`
//! § 5.1: "typed HIR … interned `Ty` with DefIds").
//!
//! A [`Ty`] is a `Copy` `u32` handle into a [`TyCtxt`]. Two structurally equal
//! types always intern to the same handle, so type equality is a word
//! comparison and a type can key a hash map without cloning anything. User
//! types are named by [`DefId`], never by a string, so two modules' `Node`s are
//! different types and a renamed import is the same one.
//!
//! The legacy typechecker keeps its own `typechecker::Type`; its results reach
//! v2 through [`TyCtxt::lower_legacy`], which converts one `Type` and reports
//! what it cannot represent rather than guessing. Anything outside the v2 core
//! (proposal § 2, C11: tensors, shapes, `impl Trait`, refinements, SIMD
//! vectors) comes back as [`LowerError::Unsupported`], which the driver turns
//! into "construct X is not supported by v2 yet".
//!
//! Design notes:
//!
//! - **Per compilation, single-threaded.** Like [`crate::intern::Interner`],
//!   interning takes `&self` through a `RefCell`, so read-mostly walkers can
//!   mint types without `&mut` plumbing. Handles from different contexts must
//!   never be mixed.
//! - **No inference variables.** Typed HIR is built from FINISHED inference;
//!   an unresolved metavariable is a lowering error, not a type.
//! - **Generic parameters are positional.** [`TyKind::Param`] carries the
//!   parameter's index in its owner's generics plus its name for display;
//!   [`TyCtxt::subst`] replaces it by index.

use std::cell::RefCell;

use rustc_hash::FxHashMap;

use crate::ids::DefId;
use crate::intern::{Interner, Symbol};
use crate::typechecker::types::{ConstArg, FloatSize, IntSize, Type, UIntSize};

/// An interned type. Equality is identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Ty(u32);

/// An interned list of types (tuple elements, generic arguments, parameters).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TyList(u32);

/// A generic type parameter: its position in the owner's generics, plus its
/// name for display. Equality includes the name, so `T` and `U` at the same
/// index of two different owners never collapse into one type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ParamTy {
    pub index: u32,
    pub name: Symbol,
}

/// The length of an array type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ArrayLen {
    Known(u64),
    /// A const generic parameter, positional like [`ParamTy`].
    Param(ParamTy),
}

/// What a type name denotes, as the caller's name lookup reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeName {
    /// A struct, enum or other nominal type.
    Adt(DefId),
    /// A trait, written where a type goes: see [`TyKind::Opaque`].
    Trait(DefId),
}

/// The structure of a type. Every field is a handle, so this is `Copy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TyKind {
    Int(IntSize),
    UInt(UIntSize),
    Float(FloatSize),
    Bool,
    Char,
    /// `String`. Stays primitive until the stdlib defines it in Kāra
    /// (proposal § 5.4), when it becomes an `Adt`.
    Str,
    Unit,
    Never,
    Tuple(TyList),
    Array {
        elem: Ty,
        len: ArrayLen,
    },
    Slice {
        elem: Ty,
        mutable: bool,
    },
    /// `Vector[T, N]`, the portable-SIMD lane vector: `Copy`, laid out and
    /// read like `Array[T, N]`, with element-wise operators.
    Vector {
        elem: Ty,
        lanes: ArrayLen,
    },
    /// A struct or enum, including the builtin collections (`Vec`, `Option`,
    /// `Result`, `Map`, …), which the caller's name lookup maps to DefIds.
    ///
    /// Whether the type is `shared` or `par` (core semantics § 6) is a
    /// property of its DEFINITION, so it is read from the item table by
    /// `def`, not carried here a second time: a legacy `Type::Shared(name)`
    /// lowers to the same `Adt` as `Type::Named { name }`.
    Adt {
        def: DefId,
        args: TyList,
    },
    /// A `weak` field's storage type (core semantics § 6.5): a non-owning
    /// handle to a `shared` value. Reading it yields `Option[T]`.
    Weak(Ty),
    /// A function or closure type. `once` marks a closure that may be called
    /// at most one time, `mutable` one whose calls may change its captures
    /// (`OnceFn` and `MutFn`, core semantics §9.6).
    Fn {
        params: TyList,
        ret: Ty,
        once: bool,
        mutable: bool,
    },
    Ref(Ty),
    MutRef(Ty),
    /// A raw pointer, for the stdlib's intrinsics (proposal § 5.4).
    RawPtr {
        mutable: bool,
        pointee: Ty,
    },
    Param(ParamTy),
    /// A value of some type implementing the trait `bound`, instantiated with
    /// `args`, whose concrete type the checker did not record. The legacy
    /// checker collapses every iterator adapter (`map`, `filter`, `iter()`,
    /// …) to `Iterator[T]`, so this is how those values reach typed HIR. It
    /// never reaches MIR: once the stdlib's iterators are Kāra structs
    /// (proposal § 5.4), each adapter call has a concrete result type.
    Opaque {
        bound: DefId,
        args: TyList,
    },
    /// The associated type `assoc` of `base` (`I.Item` in a generic body).
    /// The MIR builder normalizes it once `base` is concrete at an instance,
    /// through that type's impl binding; it never reaches MIR itself.
    Proj {
        base: Ty,
        assoc: Symbol,
    },
    /// A type that already failed to check; never reaches MIR.
    Error,

    // ── produced by MIR lowering only; typed HIR never contains these ──
    /// A static string slice: the type of a `"..."` constant in MIR.
    StaticStr,
    /// A handle to a `shared` ADT instance (reference counted). Typed HIR
    /// writes a shared type as [`TyKind::Adt`] (shared-ness is the
    /// definition's); MIR makes the handle explicit.
    Shared {
        def: DefId,
        args: TyList,
    },
    /// A library collection the MIR interpreter implements natively
    /// (`docs/spikes/mir-types.md` § 8, open question 2) until the stdlib
    /// defines it in Kāra.
    Intrinsic {
        kind: IntrinsicKind,
        args: TyList,
    },
    /// A function item; its value is zero-sized.
    FnDef {
        def: DefId,
        args: TyList,
    },
    /// A closure; its fields are the captures, in capture order.
    Closure {
        def: DefId,
        captures: TyList,
    },
}

/// The natively implemented collections of [`TyKind::Intrinsic`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IntrinsicKind {
    /// `Vec[T]`
    Vec,
    /// `Map[K, V]`
    Map,
    /// `Set[T]`
    Set,
    /// `VecDeque[T]`
    VecDeque,
    /// `SortedMap[K, V]`
    SortedMap,
    /// `SortedSet[T]`
    SortedSet,
}

impl IntrinsicKind {
    pub fn name(self) -> &'static str {
        match self {
            IntrinsicKind::Vec => "Vec",
            IntrinsicKind::Map => "Map",
            IntrinsicKind::Set => "Set",
            IntrinsicKind::VecDeque => "VecDeque",
            IntrinsicKind::SortedMap => "SortedMap",
            IntrinsicKind::SortedSet => "SortedSet",
        }
    }
}

/// Library structs whose values hold values of their one type argument
/// that the struct's declared fields do not show (`OnceLock[T]` declares
/// only a handle): they need dropping exactly when the argument does.
pub const LIBRARY_CELLS: &[&str] = &["OnceLock", "OnceCell", "Arena", "TaskHandle"];

/// Library handles that are always dropped, whatever they hold: a dropped
/// `Channel` end is what closes the channel and frees what is still queued
/// in it, and a dropped `File` closes the file.
pub const HANDLES: &[&str] = &["Sender", "Receiver", "File"];

/// A struct or enum definition as the middle end needs it. Field types are
/// written in the definition's own generic parameters ([`TyKind::Param`] by
/// position); [`TyCtxt::field_ty`] instantiates them. A struct is a
/// single-variant ADT whose variant carries the struct's name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdtDef {
    pub def: DefId,
    pub name: String,
    pub is_enum: bool,
    pub variants: Vec<VariantDef>,
    /// A user `Drop` body exists for this type itself.
    pub has_drop_impl: bool,
    /// Derives `Copy` (`Option` is `Copy` without saying so, core
    /// semantics §1.1); an instance is `Copy` when its arguments are too.
    pub is_copy: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VariantDef {
    pub name: String,
    pub fields: Vec<(String, Ty)>,
}

/// Why a legacy `Type` has no v2 counterpart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LowerError {
    /// A construct deferred until after v2 parity (C11) or not yet built.
    Unsupported(&'static str),
    /// A named type the lookup could not map to a definition.
    UnknownName(String),
    /// A generic parameter the lookup could not place.
    UnknownParam(String),
    /// An inference variable survived type checking.
    Unresolved,
}

#[derive(Default)]
struct Tables {
    kinds: Vec<TyKind>,
    kind_ids: FxHashMap<TyKind, Ty>,
    lists: Vec<Box<[Ty]>>,
    list_ids: FxHashMap<Box<[Ty]>, TyList>,
}

/// The type context: owns every interned type and list of one compilation.
pub struct TyCtxt {
    tables: RefCell<Tables>,
    names: Interner,
    adts: RefCell<FxHashMap<DefId, std::rc::Rc<AdtDef>>>,
}

impl Default for TyCtxt {
    fn default() -> Self {
        Self::new()
    }
}

impl TyCtxt {
    pub fn new() -> Self {
        TyCtxt {
            tables: RefCell::new(Tables::default()),
            names: Interner::new(),
            adts: RefCell::new(FxHashMap::default()),
        }
    }

    /// Intern a type. Structurally equal kinds return the same handle.
    pub fn intern(&self, kind: TyKind) -> Ty {
        let mut t = self.tables.borrow_mut();
        if let Some(&ty) = t.kind_ids.get(&kind) {
            return ty;
        }
        let ty = Ty(t.kinds.len() as u32);
        t.kinds.push(kind);
        t.kind_ids.insert(kind, ty);
        ty
    }

    /// Intern a list of types.
    pub fn intern_list(&self, tys: &[Ty]) -> TyList {
        let mut t = self.tables.borrow_mut();
        if let Some(&list) = t.list_ids.get(tys) {
            return list;
        }
        let list = TyList(t.lists.len() as u32);
        let boxed: Box<[Ty]> = tys.into();
        t.lists.push(boxed.clone());
        t.list_ids.insert(boxed, list);
        list
    }

    pub fn kind(&self, ty: Ty) -> TyKind {
        self.tables.borrow().kinds[ty.0 as usize]
    }

    /// The elements of an interned list, copied out (lists are short).
    pub fn list(&self, list: TyList) -> Vec<Ty> {
        self.tables.borrow().lists[list.0 as usize].to_vec()
    }

    pub fn empty_list(&self) -> TyList {
        self.intern_list(&[])
    }

    /// Intern a generic parameter's name.
    pub fn param_name(&self, name: &str) -> Symbol {
        self.names.intern(name)
    }

    pub fn resolve_name(&self, sym: Symbol) -> std::rc::Rc<str> {
        self.names.resolve(sym)
    }

    // ── common constructors ─────────────────────────────────────────

    pub fn unit(&self) -> Ty {
        self.intern(TyKind::Unit)
    }
    pub fn bool(&self) -> Ty {
        self.intern(TyKind::Bool)
    }
    pub fn i64(&self) -> Ty {
        self.intern(TyKind::Int(IntSize::I64))
    }
    pub fn str(&self) -> Ty {
        self.intern(TyKind::Str)
    }
    pub fn error(&self) -> Ty {
        self.intern(TyKind::Error)
    }
    pub fn tuple(&self, elems: &[Ty]) -> Ty {
        let list = self.intern_list(elems);
        self.intern(TyKind::Tuple(list))
    }
    pub fn adt(&self, def: DefId, args: &[Ty]) -> Ty {
        let args = self.intern_list(args);
        self.intern(TyKind::Adt { def, args })
    }
    pub fn reference(&self, inner: Ty, mutable: bool) -> Ty {
        self.intern(if mutable {
            TyKind::MutRef(inner)
        } else {
            TyKind::Ref(inner)
        })
    }

    // ── queries ─────────────────────────────────────────────────────

    /// True when no generic parameter occurs anywhere in `ty`.
    pub fn is_concrete(&self, ty: Ty) -> bool {
        let mut concrete = true;
        self.walk(ty, &mut |k| {
            if matches!(
                k,
                TyKind::Param(_)
                    | TyKind::Array {
                        len: ArrayLen::Param(_),
                        ..
                    }
                    | TyKind::Vector {
                        lanes: ArrayLen::Param(_),
                        ..
                    }
            ) {
                concrete = false;
            }
        });
        concrete
    }

    /// Visit `ty` and every type nested in it, outermost first.
    pub fn walk(&self, ty: Ty, f: &mut dyn FnMut(TyKind)) {
        let kind = self.kind(ty);
        f(kind);
        match kind {
            TyKind::Tuple(list)
            | TyKind::Adt { args: list, .. }
            | TyKind::Opaque { args: list, .. }
            | TyKind::Shared { args: list, .. }
            | TyKind::Intrinsic { args: list, .. }
            | TyKind::FnDef { args: list, .. }
            | TyKind::Closure { captures: list, .. } => {
                for t in self.list(list) {
                    self.walk(t, f);
                }
            }
            TyKind::Fn { params, ret, .. } => {
                for t in self.list(params) {
                    self.walk(t, f);
                }
                self.walk(ret, f);
            }
            TyKind::Array { elem, .. }
            | TyKind::Vector { elem, .. }
            | TyKind::Slice { elem, .. } => self.walk(elem, f),
            TyKind::Ref(t)
            | TyKind::MutRef(t)
            | TyKind::Weak(t)
            | TyKind::Proj { base: t, .. }
            | TyKind::RawPtr { pointee: t, .. } => self.walk(t, f),
            TyKind::Int(_)
            | TyKind::UInt(_)
            | TyKind::Float(_)
            | TyKind::Bool
            | TyKind::Char
            | TyKind::Str
            | TyKind::Unit
            | TyKind::Never
            | TyKind::Param(_)
            | TyKind::Error
            | TyKind::StaticStr => {}
        }
    }

    /// Replace each `Param` by `args[index]`. `const_args` does the same for
    /// array lengths. An index past the end is left alone, so a partial
    /// substitution (an impl's params, then a method's) composes.
    pub fn subst(&self, ty: Ty, args: &[Ty], const_args: &[ArrayLen]) -> Ty {
        let kind = self.kind(ty);
        let new = match kind {
            TyKind::Param(p) => return args.get(p.index as usize).copied().unwrap_or(ty),
            TyKind::Tuple(list) => TyKind::Tuple(self.subst_list(list, args, const_args)),
            TyKind::Adt { def, args: a } => TyKind::Adt {
                def,
                args: self.subst_list(a, args, const_args),
            },
            TyKind::Shared { def, args: a } => TyKind::Shared {
                def,
                args: self.subst_list(a, args, const_args),
            },
            TyKind::Opaque { bound, args: a } => TyKind::Opaque {
                bound,
                args: self.subst_list(a, args, const_args),
            },
            TyKind::Intrinsic { kind, args: a } => TyKind::Intrinsic {
                kind,
                args: self.subst_list(a, args, const_args),
            },
            TyKind::FnDef { def, args: a } => TyKind::FnDef {
                def,
                args: self.subst_list(a, args, const_args),
            },
            TyKind::Closure { def, captures } => TyKind::Closure {
                def,
                captures: self.subst_list(captures, args, const_args),
            },
            TyKind::Fn {
                params,
                ret,
                once,
                mutable,
            } => TyKind::Fn {
                params: self.subst_list(params, args, const_args),
                ret: self.subst(ret, args, const_args),
                once,
                mutable,
            },
            TyKind::Array { elem, len } => TyKind::Array {
                elem: self.subst(elem, args, const_args),
                len: match len {
                    ArrayLen::Param(p) => const_args.get(p.index as usize).copied().unwrap_or(len),
                    known => known,
                },
            },
            TyKind::Vector { elem, lanes } => TyKind::Vector {
                elem: self.subst(elem, args, const_args),
                lanes: match lanes {
                    ArrayLen::Param(p) => {
                        const_args.get(p.index as usize).copied().unwrap_or(lanes)
                    }
                    known => known,
                },
            },
            TyKind::Slice { elem, mutable } => TyKind::Slice {
                elem: self.subst(elem, args, const_args),
                mutable,
            },
            TyKind::Ref(t) => TyKind::Ref(self.subst(t, args, const_args)),
            TyKind::MutRef(t) => TyKind::MutRef(self.subst(t, args, const_args)),
            TyKind::Weak(t) => TyKind::Weak(self.subst(t, args, const_args)),
            TyKind::RawPtr { mutable, pointee } => TyKind::RawPtr {
                mutable,
                pointee: self.subst(pointee, args, const_args),
            },
            TyKind::Proj { base, assoc } => TyKind::Proj {
                base: self.subst(base, args, const_args),
                assoc,
            },
            _ => return ty,
        };
        self.intern(new)
    }

    fn subst_list(&self, list: TyList, args: &[Ty], const_args: &[ArrayLen]) -> TyList {
        let tys: Vec<Ty> = self
            .list(list)
            .into_iter()
            .map(|t| self.subst(t, args, const_args))
            .collect();
        self.intern_list(&tys)
    }

    /// Write `args` into every `def` that occurs with none. The checker
    /// records a generic impl's target with its arguments erased (`Tk` for
    /// `impl[I] It for Tk[I]`), so a local bound to `self` in a method body
    /// has that type; at an instance the arguments are known.
    pub fn fill_erased_args(&self, ty: Ty, def: DefId, args: &[Ty]) -> Ty {
        let go = |t: Ty| self.fill_erased_args(t, def, args);
        let go_list = |l: TyList| {
            let tys: Vec<Ty> = self.list(l).into_iter().map(go).collect();
            self.intern_list(&tys)
        };
        let new = match self.kind(ty) {
            TyKind::Adt { def: d, args: a } if d == def && self.list(a).is_empty() => {
                return self.adt(def, args)
            }
            TyKind::Adt { def: d, args: a } => TyKind::Adt {
                def: d,
                args: go_list(a),
            },
            TyKind::Tuple(l) => TyKind::Tuple(go_list(l)),
            TyKind::Fn {
                params,
                ret,
                once,
                mutable,
            } => TyKind::Fn {
                params: go_list(params),
                ret: go(ret),
                once,
                mutable,
            },
            TyKind::Array { elem, len } => TyKind::Array {
                elem: go(elem),
                len,
            },
            TyKind::Vector { elem, lanes } => TyKind::Vector {
                elem: go(elem),
                lanes,
            },
            TyKind::Slice { elem, mutable } => TyKind::Slice {
                elem: go(elem),
                mutable,
            },
            TyKind::Ref(t) => TyKind::Ref(go(t)),
            TyKind::MutRef(t) => TyKind::MutRef(go(t)),
            TyKind::Weak(t) => TyKind::Weak(go(t)),
            TyKind::Proj { base, assoc } => TyKind::Proj {
                base: go(base),
                assoc,
            },
            _ => return ty,
        };
        self.intern(new)
    }

    /// Render `ty` in source syntax. `def_name` names a definition.
    pub fn display(&self, ty: Ty, def_name: &dyn Fn(DefId) -> String) -> String {
        let list = |l: TyList| -> String {
            self.list(l)
                .into_iter()
                .map(|t| self.display(t, def_name))
                .collect::<Vec<_>>()
                .join(", ")
        };
        match self.kind(ty) {
            TyKind::Int(s) => format!("{s:?}").to_lowercase(),
            TyKind::UInt(s) => format!("{s:?}").to_lowercase(),
            TyKind::Float(s) => format!("{s:?}").to_lowercase(),
            TyKind::Bool => "bool".into(),
            TyKind::Char => "char".into(),
            TyKind::Str => "String".into(),
            TyKind::Unit => "()".into(),
            TyKind::Never => "Never".into(),
            TyKind::Tuple(l) => {
                if self.list(l).len() == 1 {
                    format!("({},)", list(l))
                } else {
                    format!("({})", list(l))
                }
            }
            TyKind::Array { elem, len } => {
                let len = match len {
                    ArrayLen::Known(n) => n.to_string(),
                    ArrayLen::Param(p) => self.resolve_name(p.name).to_string(),
                };
                format!("Array[{}, {len}]", self.display(elem, def_name))
            }
            TyKind::Vector { elem, lanes } => {
                let lanes = match lanes {
                    ArrayLen::Known(n) => n.to_string(),
                    ArrayLen::Param(p) => self.resolve_name(p.name).to_string(),
                };
                format!("Vector[{}, {lanes}]", self.display(elem, def_name))
            }
            TyKind::Slice { elem, mutable } => format!(
                "{}Slice[{}]",
                if mutable { "mut " } else { "" },
                self.display(elem, def_name)
            ),
            TyKind::Adt { def, args } => {
                if self.list(args).is_empty() {
                    def_name(def)
                } else {
                    format!("{}[{}]", def_name(def), list(args))
                }
            }
            TyKind::Weak(t) => format!("weak {}", self.display(t, def_name)),
            TyKind::Fn {
                params,
                ret,
                once,
                mutable,
            } => format!(
                "{}fn({}) -> {}",
                if once {
                    "once "
                } else if mutable {
                    "mut "
                } else {
                    ""
                },
                list(params),
                self.display(ret, def_name)
            ),
            TyKind::Ref(t) => format!("ref {}", self.display(t, def_name)),
            TyKind::MutRef(t) => format!("mut ref {}", self.display(t, def_name)),
            TyKind::RawPtr { mutable, pointee } => format!(
                "*{} {}",
                if mutable { "mut" } else { "const" },
                self.display(pointee, def_name)
            ),
            TyKind::Param(p) => self.resolve_name(p.name).to_string(),
            TyKind::Proj { base, assoc } => {
                format!(
                    "{}.{}",
                    self.display(base, def_name),
                    self.resolve_name(assoc)
                )
            }
            TyKind::Opaque { bound, args } => {
                if self.list(args).is_empty() {
                    format!("impl {}", def_name(bound))
                } else {
                    format!("impl {}[{}]", def_name(bound), list(args))
                }
            }
            TyKind::Error => "{error}".into(),
            TyKind::StaticStr => "str".into(),
            TyKind::Shared { def, args } => {
                format!(
                    "shared {}",
                    self.display(self.adt(def, &self.list(args)), def_name)
                )
            }
            TyKind::Intrinsic { kind, args } => format!("{}[{}]", kind.name(), list(args)),
            TyKind::FnDef { def, .. } => format!("fn#{}", def.0),
            TyKind::Closure { def, .. } => format!("closure#{}", def.0),
        }
    }

    // ── ADT definitions ─────────────────────────────────────────────

    /// Register the definition of an ADT. A second registration of the
    /// same `def` replaces the first.
    pub fn add_adt_def(&self, adt: AdtDef) {
        self.adts
            .borrow_mut()
            .insert(adt.def, std::rc::Rc::new(adt));
    }

    /// The registered definition of `def`, if any.
    pub fn adt_def(&self, def: DefId) -> Option<std::rc::Rc<AdtDef>> {
        self.adts.borrow().get(&def).cloned()
    }

    /// The ADT definition and instance arguments behind an `Adt` or `Shared`
    /// type.
    pub fn adt_of(&self, ty: Ty) -> Option<(std::rc::Rc<AdtDef>, Vec<Ty>)> {
        match self.kind(ty) {
            TyKind::Adt { def, args } | TyKind::Shared { def, args } => {
                Some((self.adt_def(def)?, self.list(args)))
            }
            _ => None,
        }
    }

    /// The type of field `f` of `ty` (seen as `variant` when `ty` is an
    /// enum), instantiated, or `None` when `ty` has no such field. Tuples and
    /// closures index their elements and captures.
    pub fn field_ty(&self, ty: Ty, variant: Option<u32>, f: u32) -> Option<Ty> {
        match self.kind(ty) {
            TyKind::Tuple(l) | TyKind::Closure { captures: l, .. } if variant.is_none() => {
                self.list(l).get(f as usize).copied()
            }
            TyKind::Adt { .. } | TyKind::Shared { .. } => {
                let (adt, args) = self.adt_of(ty)?;
                let v = match (adt.is_enum, variant) {
                    (false, None) => adt.variants.first()?,
                    (true, Some(v)) => adt.variants.get(v as usize)?,
                    _ => return None,
                };
                let (_, t) = v.fields.get(f as usize)?;
                Some(self.subst(*t, &args, &[]))
            }
            _ => None,
        }
    }

    /// `Copy` per core semantics § 1.1: primitives and shared references,
    /// and tuples / arrays / closures / ADTs whose parts are `Copy` (an ADT
    /// only when it derives `Copy`).
    pub fn is_copy(&self, ty: Ty) -> bool {
        match self.kind(ty) {
            TyKind::Bool
            | TyKind::Char
            | TyKind::Int(_)
            | TyKind::UInt(_)
            | TyKind::Float(_)
            | TyKind::Unit
            | TyKind::Never
            | TyKind::StaticStr
            | TyKind::Ref(_)
            | TyKind::RawPtr { .. }
            | TyKind::FnDef { .. } => true,
            // A shared slice is a borrow, copied like `ref T`; a `mut Slice`
            // is unique, like `mut ref T`.
            TyKind::Slice { mutable, .. } => !mutable,
            TyKind::Tuple(l) | TyKind::Closure { captures: l, .. } => {
                self.list(l).into_iter().all(|t| self.is_copy(t))
            }
            TyKind::Array { elem, .. } => self.is_copy(elem),
            TyKind::Vector { .. } => true,
            // A generic type that is `Copy` is so at the instances whose
            // arguments are (`Option[i64]`, not `Option[String]`).
            TyKind::Adt { def, args } => {
                self.adt_def(def).is_some_and(|a| a.is_copy)
                    && self.list(args).into_iter().all(|t| self.is_copy(t))
            }
            TyKind::Str
            | TyKind::Shared { .. }
            | TyKind::MutRef(_)
            | TyKind::Intrinsic { .. }
            | TyKind::Weak(_)
            | TyKind::Fn { .. }
            | TyKind::Param(_)
            | TyKind::Proj { .. }
            | TyKind::Opaque { .. }
            | TyKind::Error => false,
        }
    }

    /// Core semantics § 1.4: owns heap memory, holds a shared handle, or has
    /// a `Drop` body anywhere inside.
    pub fn needs_drop(&self, ty: Ty) -> bool {
        self.needs_drop_in(ty, &mut Vec::new())
    }

    /// [`Self::needs_drop`], with the types being asked about further out.
    /// A type that contains itself (`struct N { next: Option[N] }`) is
    /// stored behind a box, so meeting it again answers yes.
    fn needs_drop_in(&self, ty: Ty, outer: &mut Vec<Ty>) -> bool {
        if outer.contains(&ty) {
            return true;
        }
        match self.kind(ty) {
            // An opaque value's concrete type is unknown, so it is assumed to
            // need dropping.
            // An erased function value owns its closure's environment.
            TyKind::Str
            | TyKind::Shared { .. }
            | TyKind::Intrinsic { .. }
            | TyKind::Weak(_)
            | TyKind::Fn { .. }
            | TyKind::Opaque { .. } => true,
            TyKind::Tuple(l) | TyKind::Closure { captures: l, .. } => self
                .list(l)
                .into_iter()
                .any(|t| self.needs_drop_in(t, outer)),
            TyKind::Array { elem, .. } => self.needs_drop_in(elem, outer),
            TyKind::Vector { .. } => false,
            TyKind::Adt { .. } => {
                let Some((adt, args)) = self.adt_of(ty) else {
                    return false;
                };
                if adt.has_drop_impl {
                    return true;
                }
                // A library cell keeps values of its type argument in place
                // of its fields (the MIR interpreter's `once_method` and
                // `arena_method`).
                if HANDLES.contains(&adt.name.as_str()) {
                    return true;
                }
                if LIBRARY_CELLS.contains(&adt.name.as_str()) {
                    return args.first().is_some_and(|&t| self.needs_drop_in(t, outer));
                }
                outer.push(ty);
                let r = adt.variants.iter().any(|v| {
                    v.fields
                        .iter()
                        .any(|(_, t)| self.needs_drop_in(self.subst(*t, &args, &[]), outer))
                });
                outer.pop();
                r
            }
            _ => false,
        }
    }

    /// Does this type itself have a user `Drop` body (not counting parts)?
    pub fn has_drop_impl(&self, ty: Ty) -> bool {
        matches!(self.kind(ty), TyKind::Adt { def, .. }
            if self.adt_def(def).is_some_and(|a| a.has_drop_impl))
    }

    // ── bridge from the legacy typechecker ──────────────────────────

    /// Convert a legacy `typechecker::Type`. `lookup` maps a type name to what
    /// it denotes; `param` places a generic parameter name in the current
    /// owner's generics.
    pub fn lower_legacy(
        &self,
        ty: &Type,
        lookup: &dyn Fn(&str) -> Option<TypeName>,
        param: &dyn Fn(&str) -> Option<u32>,
    ) -> Result<Ty, LowerError> {
        let lower = |t: &Type| self.lower_legacy(t, lookup, param);
        let lower_all = |ts: &[Type]| -> Result<TyList, LowerError> {
            let tys = ts.iter().map(lower).collect::<Result<Vec<_>, _>>()?;
            Ok(self.intern_list(&tys))
        };
        let param_ty = |name: &str| -> Result<ParamTy, LowerError> {
            let index = param(name).ok_or_else(|| LowerError::UnknownParam(name.to_string()))?;
            Ok(ParamTy {
                index,
                name: self.param_name(name),
            })
        };
        let length = |n: &ConstArg| -> Result<ArrayLen, LowerError> {
            match n {
                ConstArg::Literal(n) if *n >= 0 => Ok(ArrayLen::Known(*n as u64)),
                ConstArg::ConstParam(name) => Ok(ArrayLen::Param(param_ty(name)?)),
                ConstArg::ConstVar(_) => Err(LowerError::Unresolved),
                ConstArg::Literal(_) | ConstArg::DynamicDim => {
                    Err(LowerError::Unsupported("array length"))
                }
            }
        };
        let kind = match ty {
            Type::Int(s) => TyKind::Int(*s),
            Type::UInt(s) => TyKind::UInt(*s),
            Type::Float(s) => TyKind::Float(*s),
            Type::Bool => TyKind::Bool,
            Type::Char => TyKind::Char,
            Type::Str => TyKind::Str,
            Type::Unit => TyKind::Unit,
            Type::Never => TyKind::Never,
            Type::Error => TyKind::Error,
            Type::Tuple(elems) => TyKind::Tuple(lower_all(elems)?),
            Type::Array { element, size } => TyKind::Array {
                elem: lower(element)?,
                len: length(size)?,
            },
            Type::Vector { element, lanes } => TyKind::Vector {
                elem: lower(element)?,
                lanes: length(lanes)?,
            },
            Type::Slice { element, mutable } => TyKind::Slice {
                elem: lower(element)?,
                mutable: *mutable,
            },
            // The checker sometimes spells a generic parameter in scope as a
            // nominal type (the result of `T: Add`'s `+` is `Named("T")`); a
            // parameter shadows any type of the same name.
            Type::Named { name, args } if args.is_empty() && param(name).is_some() => {
                TyKind::Param(param_ty(name)?)
            }
            Type::Named { name, args } => match lookup(name) {
                Some(TypeName::Adt(def)) => TyKind::Adt {
                    def,
                    args: lower_all(args)?,
                },
                Some(TypeName::Trait(bound)) => TyKind::Opaque {
                    bound,
                    args: lower_all(args)?,
                },
                // `Unit` spelled by name is `()` (design.md § Entry Point).
                None if name == "Unit" && args.is_empty() => TyKind::Unit,
                None => return Err(LowerError::UnknownName(name.clone())),
            },
            Type::Shared(name) => match lookup(name) {
                Some(TypeName::Adt(def)) => TyKind::Adt {
                    def,
                    args: self.empty_list(),
                },
                _ => return Err(LowerError::UnknownName(name.clone())),
            },
            Type::Function {
                params,
                return_type,
            } => TyKind::Fn {
                params: lower_all(params)?,
                ret: lower(return_type)?,
                once: false,
                mutable: false,
            },
            Type::OnceFunction {
                params,
                return_type,
            } => TyKind::Fn {
                params: lower_all(params)?,
                ret: lower(return_type)?,
                once: true,
                mutable: false,
            },
            Type::Ref(inner) => TyKind::Ref(lower(inner)?),
            Type::MutRef(inner) => TyKind::MutRef(lower(inner)?),
            Type::Pointer { is_mut, inner } => TyKind::RawPtr {
                mutable: *is_mut,
                pointee: lower(inner)?,
            },
            Type::TypeParam(name) => TyKind::Param(param_ty(name)?),
            Type::TypeVar(_) => return Err(LowerError::Unresolved),
            Type::Rc(_) => return Err(LowerError::Unsupported("Rc[T]")),
            Type::Arc(_) => return Err(LowerError::Unsupported("Arc[T]")),
            Type::Weak(inner) => TyKind::Weak(lower(inner)?),
            Type::Shape(_) => return Err(LowerError::Unsupported("shape arguments")),
            // `I.Item` over a parameter in scope, or `Tk[I].Item` over a
            // nominal type; the builder normalizes it at each instance.
            Type::AssocProjection {
                param: base,
                assoc,
                args,
                receiver_args,
            } if args.is_empty() => {
                let base = if receiver_args.is_empty() && param(base).is_some() {
                    self.intern(TyKind::Param(param_ty(base)?))
                } else {
                    match lookup(base) {
                        Some(TypeName::Adt(def)) => self.intern(TyKind::Adt {
                            def,
                            args: lower_all(receiver_args)?,
                        }),
                        _ => return Err(LowerError::UnknownName(base.clone())),
                    }
                };
                TyKind::Proj {
                    base,
                    assoc: self.param_name(assoc),
                }
            }
            Type::AssocProjection { .. } => {
                return Err(LowerError::Unsupported(
                    "generic associated type projection",
                ))
            }
            Type::Existential { .. } => return Err(LowerError::Unsupported("impl Trait")),
            Type::Refinement { .. } => return Err(LowerError::Unsupported("refinement types")),
        };
        Ok(self.intern(kind))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(d: DefId) -> String {
        ["Vec", "Node", "Option", "Iterator"][d.0 as usize].to_string()
    }

    fn lookup(name: &str) -> Option<TypeName> {
        let i = ["Vec", "Node", "Option", "Iterator"]
            .iter()
            .position(|n| *n == name)?;
        let def = DefId(i as u32);
        Some(if name == "Iterator" {
            TypeName::Trait(def)
        } else {
            TypeName::Adt(def)
        })
    }

    fn params(name: &str) -> Option<u32> {
        ["T", "U", "N"]
            .iter()
            .position(|n| *n == name)
            .map(|i| i as u32)
    }

    #[test]
    fn equal_types_intern_to_one_handle() {
        let tcx = TyCtxt::new();
        let a = tcx.adt(DefId(0), &[tcx.i64()]);
        let b = tcx.adt(DefId(0), &[tcx.i64()]);
        let c = tcx.adt(DefId(0), &[tcx.str()]);
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(tcx.tuple(&[a, c]), tcx.tuple(&[b, c]));
        assert_ne!(tcx.reference(a, false), tcx.reference(a, true));
    }

    #[test]
    fn subst_replaces_params_by_index() {
        let tcx = TyCtxt::new();
        let t = tcx.intern(TyKind::Param(ParamTy {
            index: 0,
            name: tcx.param_name("T"),
        }));
        let n = ArrayLen::Param(ParamTy {
            index: 0,
            name: tcx.param_name("N"),
        });
        let generic = tcx.tuple(&[
            tcx.adt(DefId(0), &[t]),
            tcx.intern(TyKind::Array { elem: t, len: n }),
        ]);
        assert!(!tcx.is_concrete(generic));
        let got = tcx.subst(generic, &[tcx.str()], &[ArrayLen::Known(3)]);
        let want = tcx.tuple(&[
            tcx.adt(DefId(0), &[tcx.str()]),
            tcx.intern(TyKind::Array {
                elem: tcx.str(),
                len: ArrayLen::Known(3),
            }),
        ]);
        assert_eq!(got, want);
        assert!(tcx.is_concrete(got));
        assert_eq!(tcx.display(got, &names), "(Vec[String], Array[String, 3])");
    }

    #[test]
    fn legacy_types_lower_and_display() {
        let tcx = TyCtxt::new();
        let legacy = Type::Function {
            params: vec![
                Type::Ref(Box::new(Type::Named {
                    name: "Vec".into(),
                    args: vec![Type::TypeParam("T".into())],
                })),
                Type::Shared("Node".into()),
            ],
            return_type: Box::new(Type::Named {
                name: "Option".into(),
                args: vec![Type::Int(IntSize::I64)],
            }),
        };
        let ty = tcx.lower_legacy(&legacy, &lookup, &params).unwrap();
        assert_eq!(
            tcx.display(ty, &names),
            "fn(ref Vec[T], Node) -> Option[i64]"
        );
        // Lowering twice gives the same handle.
        assert_eq!(tcx.lower_legacy(&legacy, &lookup, &params).unwrap(), ty);
    }

    #[test]
    fn a_parameter_spelled_as_a_nominal_type_is_the_parameter() {
        let tcx = TyCtxt::new();
        let named_t = Type::Named {
            name: "T".into(),
            args: vec![],
        };
        assert_eq!(
            tcx.lower_legacy(&named_t, &lookup, &params),
            tcx.lower_legacy(&Type::TypeParam("T".into()), &lookup, &params)
        );
    }

    #[test]
    fn a_trait_in_type_position_lowers_to_an_opaque_type() {
        let tcx = TyCtxt::new();
        let iter = Type::Named {
            name: "Iterator".into(),
            args: vec![Type::TypeParam("T".into())],
        };
        let ty = tcx.lower_legacy(&iter, &lookup, &params).unwrap();
        assert_eq!(tcx.display(ty, &names), "impl Iterator[T]");
        let got = tcx.subst(ty, &[tcx.i64()], &[]);
        assert_eq!(tcx.display(got, &names), "impl Iterator[i64]");
        assert!(tcx.needs_drop(got) && !tcx.is_copy(got));
        // A trait is not a `shared` type.
        assert_eq!(
            tcx.lower_legacy(&Type::Shared("Iterator".into()), &lookup, &params),
            Err(LowerError::UnknownName("Iterator".into()))
        );
    }

    #[test]
    fn legacy_lowering_refuses_what_v2_does_not_have() {
        let tcx = TyCtxt::new();
        let rc = Type::Rc(Box::new(Type::Bool));
        assert_eq!(
            tcx.lower_legacy(&rc, &lookup, &params),
            Err(LowerError::Unsupported("Rc[T]"))
        );
        let unknown = Type::Named {
            name: "Missing".into(),
            args: vec![],
        };
        assert_eq!(
            tcx.lower_legacy(&unknown, &lookup, &params),
            Err(LowerError::UnknownName("Missing".into()))
        );
        assert_eq!(
            tcx.lower_legacy(
                &Type::TypeVar(crate::typechecker::types::TypeVarId(4)),
                &lookup,
                &params
            ),
            Err(LowerError::Unresolved)
        );
        assert_eq!(
            tcx.lower_legacy(&Type::TypeParam("Z".into()), &lookup, &params),
            Err(LowerError::UnknownParam("Z".into()))
        );
    }

    #[test]
    fn adt_instances_instantiate_their_fields() {
        let tcx = TyCtxt::new();
        let t = tcx.intern(TyKind::Param(ParamTy {
            index: 0,
            name: tcx.param_name("T"),
        }));
        // enum Option[T] { None, Some(T) }
        tcx.add_adt_def(AdtDef {
            def: DefId(2),
            name: "Option".into(),
            is_enum: true,
            variants: vec![
                VariantDef {
                    name: "None".into(),
                    fields: vec![],
                },
                VariantDef {
                    name: "Some".into(),
                    fields: vec![("0".into(), t)],
                },
            ],
            has_drop_impl: false,
            is_copy: false,
        });
        let of_int = tcx.adt(DefId(2), &[tcx.i64()]);
        let of_str = tcx.adt(DefId(2), &[tcx.str()]);
        assert_eq!(tcx.field_ty(of_int, Some(1), 0), Some(tcx.i64()));
        assert_eq!(tcx.field_ty(of_str, Some(1), 0), Some(tcx.str()));
        assert_eq!(tcx.field_ty(of_int, None, 0), None);
        assert!(!tcx.needs_drop(of_int));
        assert!(tcx.needs_drop(of_str));
        assert!(!tcx.has_drop_impl(of_str));
    }
}
