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

use rustc_hash::{FxHashMap, FxHashSet};

use crate::ids::DefId;
use crate::intern::{Interner, Symbol};
use crate::typechecker::types::{ConstArg, FloatSize, IntSize, Type, UIntSize};

/// An interned type. Equality is identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Ty(u32);

impl Ty {
    /// The interned index: dense from zero, so a per-type table can be a
    /// vector.
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

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
    /// `dyn Trait` (design.md § `dyn Trait`): an owned value of some type
    /// implementing the trait `trait_`, chosen at run time, in its own heap
    /// block. `args` is the trait's generic arguments, then its associated
    /// types in declaration order. `ref dyn Trait` is `Ref(Dyn)`, a view of
    /// the same two words.
    Dyn {
        trait_: DefId,
        args: TyList,
        cross_task: bool,
    },
    /// The associated type `assoc` of `base` (`I.Item` in a generic body).
    /// The MIR builder normalizes it once `base` is concrete at an instance,
    /// through that type's impl binding; it never reaches MIR itself.
    Proj {
        base: Ty,
        assoc: Symbol,
        /// The bound trait that declares `assoc` when `base` is a parameter
        /// (`Add` for `T.Output` under `T: Add`), so a type with several
        /// operator impls normalizes through the right one (B-2026-10-18-1).
        trait_: Option<Symbol>,
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
/// in place, where the struct's declared fields do not show them: they
/// need dropping exactly when the argument does. (The cells that keep
/// theirs behind a runtime handle are [`HANDLES`].)
pub const LIBRARY_CELLS: &[&str] = &["Mutex"];

/// Library handles that are always dropped, whatever they hold: a dropped
/// `Channel` end is what closes the channel and frees what is still queued
/// in it, a dropped `File` (or `BufReader` / `BufWriter`, a writer
/// flushing first) closes the file, a dropped `TaskHandle`
/// detaches its task, a dropped `TaskGroup` has joined its tasks, and the rest free the runtime allocation behind the
/// handle (their held values, when those need dropping, go first).
pub const HANDLES: &[&str] = &[
    "Sender",
    "Receiver",
    "File",
    "CString",
    "OnceLock",
    "OnceCell",
    // Its initializer until the first `get`, and the value after.
    "LazyLock",
    "Arena",
    "TaskHandle",
    // A group's drop frees its runtime state (B-2026-10-15-30).
    "TaskGroup",
    "Interner",
    "BufReader",
    "BufWriter",
    // `std.process` (B-2026-10-15-6): a `Stdio`, a child and its pipes.
    "Stdio",
    "ChildProcess",
    "ChildStdout",
    "ChildStderr",
    "ChildStdin",
];

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
    /// Declared in the user's program, so a library type's name means
    /// nothing here: a user `struct Pool` is not the library's `Pool`.
    pub user: bool,
    /// A `sync struct` / `sync enum` (core semantics §6.3): its handle is
    /// counted atomically and may cross into a task, where a plain
    /// `shared` one never does.
    pub sync: bool,
}

impl AdtDef {
    /// The library type's name, when this is a library type.
    pub fn library_name(&self) -> Option<&str> {
        (!self.user).then_some(self.name.as_str())
    }
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

/// The hidden associated type a trait method's `-> impl Trait` is, in each
/// impl: `$<method>`, a name no source can spell.
pub fn rpit_assoc(method: &str) -> String {
    format!("${method}")
}

/// The type context: owns every interned type and list of one compilation.
pub struct TyCtxt {
    tables: RefCell<Tables>,
    names: Interner,
    adts: RefCell<FxHashMap<DefId, std::rc::Rc<AdtDef>>>,
    /// Each return-position `impl Trait`'s one concrete type, by the
    /// existential's origin: the type a value of it lowers to.
    witnesses: RefCell<FxHashMap<crate::resolver::SpanKey, Type>>,
    /// Each trait method's return-position `impl Trait`, by its origin in
    /// the trait: `(trait, method)`. A value of it is the impl's hidden
    /// associated type [`rpit_assoc`].
    trait_rpits: RefCell<FxHashMap<crate::resolver::SpanKey, (String, String)>>,
    /// Each concrete projection the MIR builder has normalized
    /// (`VecIter[i64].Item` → `i64`), so a generic ADT's field typed
    /// `I.Item` instantiates to the binding (B-2026-10-18-23).
    projs: RefCell<FxHashMap<Ty, Ty>>,
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
            witnesses: RefCell::new(FxHashMap::default()),
            trait_rpits: RefCell::new(FxHashMap::default()),
            projs: RefCell::new(FxHashMap::default()),
        }
    }

    /// Record that the projection `proj` normalizes to `to`; `subst`
    /// then answers it with `to`.
    pub fn record_proj(&self, proj: Ty, to: Ty) {
        self.projs.borrow_mut().insert(proj, to);
    }

    /// Record the concrete types return-position `impl Trait`s stand for.
    pub fn set_existential_witnesses(&self, w: FxHashMap<crate::resolver::SpanKey, Type>) {
        *self.witnesses.borrow_mut() = w;
    }

    /// Record the trait methods whose return is an `impl Trait`.
    pub fn set_trait_rpits(&self, r: FxHashMap<crate::resolver::SpanKey, (String, String)>) {
        *self.trait_rpits.borrow_mut() = r;
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
    /// `ref inner` (`mut ref` when `mutable`). A borrow of a borrow is the
    /// borrow (design.md; the checker's `ref_of`): `ref (ref T)` and
    /// `ref (mut ref T)` are `ref T`, `mut ref (mut ref T)` is `mut ref T`,
    /// and `mut ref (ref T)` is `ref T`.
    pub fn reference(&self, inner: Ty, mutable: bool) -> Ty {
        self.intern(self.ref_kind(inner, mutable))
    }

    /// The kind of [`Self::reference`].
    fn ref_kind(&self, inner: Ty, mutable: bool) -> TyKind {
        match self.kind(inner) {
            TyKind::Ref(t) => TyKind::Ref(t),
            TyKind::MutRef(t) if mutable => TyKind::MutRef(t),
            TyKind::MutRef(t) => TyKind::Ref(t),
            _ if mutable => TyKind::MutRef(inner),
            _ => TyKind::Ref(inner),
        }
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
            | TyKind::Dyn { args: list, .. }
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
            TyKind::Dyn {
                trait_,
                args: a,
                cross_task,
            } => TyKind::Dyn {
                trait_,
                args: self.subst_list(a, args, const_args),
                cross_task,
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
            TyKind::Ref(t) => self.ref_kind(self.subst(t, args, const_args), false),
            TyKind::MutRef(t) => self.ref_kind(self.subst(t, args, const_args), true),
            TyKind::Weak(t) => TyKind::Weak(self.subst(t, args, const_args)),
            TyKind::RawPtr { mutable, pointee } => TyKind::RawPtr {
                mutable,
                pointee: self.subst(pointee, args, const_args),
            },
            TyKind::Proj {
                base,
                assoc,
                trait_,
            } => {
                let p = self.intern(TyKind::Proj {
                    base: self.subst(base, args, const_args),
                    assoc,
                    trait_,
                });
                return self.projs.borrow().get(&p).copied().unwrap_or(p);
            }
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
            TyKind::Ref(t) => self.ref_kind(go(t), false),
            TyKind::MutRef(t) => self.ref_kind(go(t), true),
            TyKind::Weak(t) => TyKind::Weak(go(t)),
            TyKind::Proj {
                base,
                assoc,
                trait_,
            } => TyKind::Proj {
                base: go(base),
                assoc,
                trait_,
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
            TyKind::Proj { base, assoc, .. } => {
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
            TyKind::Dyn {
                trait_,
                args,
                cross_task,
            } => {
                let base = if self.list(args).is_empty() {
                    format!("dyn {}", def_name(trait_))
                } else {
                    format!("dyn {}[{}]", def_name(trait_), list(args))
                };
                if cross_task {
                    format!("{base} + CrossTask")
                } else {
                    base
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

    /// Why a value of `ty` may not enter a task (core semantics §11.3,
    /// design.md § Structured Concurrency): the first thing it reaches,
    /// through fields, arguments and borrows, that is a plain `shared`
    /// handle, a `OnceCell` or a raw pointer. `None` when it may.
    pub fn task_blocker(&self, ty: Ty) -> Option<String> {
        fn go(tcx: &TyCtxt, ty: Ty, seen: &mut FxHashSet<Ty>) -> Option<String> {
            if !seen.insert(ty) {
                return None;
            }
            let fields = |def: DefId, args: TyList, seen: &mut FxHashSet<Ty>| {
                let adt = tcx.adt_def(def)?;
                let args = tcx.list(args);
                adt.variants
                    .iter()
                    .flat_map(|v| &v.fields)
                    .find_map(|(_, f)| go(tcx, tcx.subst(*f, &args, &[]), seen))
            };
            match tcx.kind(ty) {
                TyKind::Shared { def, args } => match tcx.adt_def(def) {
                    Some(adt) if adt.sync => fields(def, args, seen),
                    Some(adt) => Some(format!("the `shared` type `{}`", adt.name)),
                    None => Some("a `shared` handle".to_string()),
                },
                TyKind::Adt { def, args } => {
                    if tcx
                        .adt_def(def)
                        .is_some_and(|a| a.library_name() == Some("OnceCell"))
                    {
                        return Some("a `OnceCell`".to_string());
                    }
                    tcx.list(args)
                        .into_iter()
                        .find_map(|t| go(tcx, t, seen))
                        .or_else(|| fields(def, args, seen))
                }
                TyKind::RawPtr { .. } => Some("a raw pointer".to_string()),
                TyKind::Tuple(list)
                | TyKind::Intrinsic { args: list, .. }
                | TyKind::Closure { captures: list, .. } => {
                    tcx.list(list).into_iter().find_map(|t| go(tcx, t, seen))
                }
                TyKind::Array { elem: t, .. }
                | TyKind::Vector { elem: t, .. }
                | TyKind::Slice { elem: t, .. }
                | TyKind::Ref(t)
                | TyKind::MutRef(t)
                | TyKind::Weak(t) => go(tcx, t, seen),
                _ => None,
            }
        }
        go(self, ty, &mut FxHashSet::default())
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
            // fields are (`Option[i64]`, not `Option[String]`); an argument
            // no field holds (`ArenaRef[Node]`, a handle) does not count.
            TyKind::Adt { def, args } => self.adt_def(def).is_some_and(|a| {
                let args = self.list(args);
                a.is_copy
                    && a.variants.iter().all(|v| {
                        v.fields
                            .iter()
                            .all(|(_, t)| self.is_copy(self.subst(*t, &args, &[])))
                    })
            }),
            TyKind::Str
            | TyKind::Shared { .. }
            | TyKind::MutRef(_)
            | TyKind::Intrinsic { .. }
            | TyKind::Weak(_)
            | TyKind::Fn { .. }
            | TyKind::Param(_)
            | TyKind::Proj { .. }
            | TyKind::Opaque { .. }
            | TyKind::Dyn { .. }
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
            | TyKind::Opaque { .. }
            | TyKind::Dyn { .. } => true,
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
                let lib = adt.library_name().unwrap_or("");
                if HANDLES.contains(&lib) {
                    return true;
                }
                if LIBRARY_CELLS.contains(&lib) {
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
        self.lower_legacy_with(ty, lookup, param, false)
    }

    /// [`Self::lower_legacy`], with every inference variable the checker left
    /// unsolved read as `()` when `vars_to_unit`. Nothing constrains such a
    /// variable, so no value of its type is ever built or read (`Err`'s
    /// payload in `match Result.Ok(a) { .. }`).
    pub fn lower_legacy_with(
        &self,
        ty: &Type,
        lookup: &dyn Fn(&str) -> Option<TypeName>,
        param: &dyn Fn(&str) -> Option<u32>,
        vars_to_unit: bool,
    ) -> Result<Ty, LowerError> {
        self.lower_legacy_full(ty, lookup, param, vars_to_unit, &|_, _| None)
    }

    /// [`Self::lower_legacy_with`], with `proj_trait` naming the bound trait
    /// that declares a parameter's associated type (`("T", "Output")` ->
    /// `Add` under `T: Add`), recorded on the projection (B-2026-10-18-1).
    pub fn lower_legacy_full(
        &self,
        ty: &Type,
        lookup: &dyn Fn(&str) -> Option<TypeName>,
        param: &dyn Fn(&str) -> Option<u32>,
        vars_to_unit: bool,
        proj_trait: &dyn Fn(&str, &str) -> Option<String>,
    ) -> Result<Ty, LowerError> {
        let lower = |t: &Type| self.lower_legacy_full(t, lookup, param, vars_to_unit, proj_trait);
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
                // A `Map` / `Set` that names the default hasher, or one never
                // inferred, is the type that names none (B-2026-10-25-31).
                Some(TypeName::Adt(def)) => TyKind::Adt {
                    def,
                    args: lower_all(crate::typechecker::types::without_default_hasher(
                        name, args,
                    ))?,
                },
                Some(TypeName::Trait(bound)) => TyKind::Opaque {
                    bound,
                    args: lower_all(args)?,
                },
                // `Unit` spelled by name is `()` (design.md § Entry Point).
                None if name == "Unit" && args.is_empty() => TyKind::Unit,
                // A comptime `Type` is carried as its name, an `Item` or an
                // `Expr` as its source text (B-2026-10-22-20).
                None if matches!(name.as_str(), "Type" | "Item" | "Expr") && args.is_empty() => {
                    TyKind::Str
                }
                None => return Err(LowerError::UnknownName(name.clone())),
            },
            Type::Shared(name) => match lookup(name) {
                Some(TypeName::Adt(def)) => TyKind::Adt {
                    def,
                    args: self.empty_list(),
                },
                _ => return Err(LowerError::UnknownName(name.clone())),
            },
            // The trait's own arguments, then its associated types in the
            // order the checker bound them.
            Type::Dyn {
                trait_name,
                args,
                assoc,
                cross_task,
            } => match lookup(trait_name) {
                Some(TypeName::Trait(trait_)) => {
                    let mut all = args.clone();
                    all.extend(assoc.iter().map(|(_, t)| t.clone()));
                    TyKind::Dyn {
                        trait_,
                        args: lower_all(&all)?,
                        cross_task: *cross_task,
                    }
                }
                _ => return Err(LowerError::UnknownName(trait_name.clone())),
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
            Type::Ref(inner) => self.ref_kind(lower(inner)?, false),
            Type::MutRef(inner) => self.ref_kind(lower(inner)?, true),
            Type::Pointer { is_mut, inner } => TyKind::RawPtr {
                mutable: *is_mut,
                pointee: lower(inner)?,
            },
            Type::TypeParam(name) => TyKind::Param(param_ty(name)?),
            Type::TypeVar(_) if vars_to_unit => TyKind::Unit,
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
                trait_name,
            } if args.is_empty() => {
                let (base, trait_) = if receiver_args.is_empty() && param(base).is_some() {
                    (
                        self.intern(TyKind::Param(param_ty(base)?)),
                        trait_name
                            .clone()
                            .or_else(|| proj_trait(base, assoc))
                            .map(|t| self.param_name(&t)),
                    )
                } else {
                    match lookup(base) {
                        Some(TypeName::Adt(def)) => (
                            self.intern(TyKind::Adt {
                                def,
                                args: lower_all(receiver_args)?,
                            }),
                            None,
                        ),
                        _ => return Err(LowerError::UnknownName(base.clone())),
                    }
                };
                TyKind::Proj {
                    base,
                    assoc: self.param_name(assoc),
                    trait_,
                }
            }
            Type::AssocProjection { .. } => {
                return Err(LowerError::Unsupported(
                    "generic associated type projection",
                ))
            }
            Type::Existential { origin, scope, .. } => {
                // A trait method's `-> impl Trait` is each impl's own type:
                // the hidden associated type the builder normalizes at the
                // instance, as `C.Item` (B-2026-10-20-32).
                let rpit = self.trait_rpits.borrow().get(origin).cloned();
                if let Some((trait_name, method)) = rpit {
                    let base = scope
                        .iter()
                        .find(|(n, _)| n == "Self")
                        .map(|(_, t)| t)
                        .ok_or(LowerError::Unsupported("impl Trait"))?;
                    TyKind::Proj {
                        base: lower(base)?,
                        assoc: self.param_name(&rpit_assoc(&method)),
                        trait_: Some(self.param_name(&trait_name)),
                    }
                } else {
                    let w = self.witnesses.borrow().get(origin).cloned();
                    return match w {
                        // The witness is written over the declaration's
                        // generic names; this use says what each stands for.
                        Some(w) => {
                            let subs = scope
                                .iter()
                                .filter(|(n, t)| !matches!(t, Type::TypeParam(p) if p == n))
                                .map(|(n, t)| {
                                    (
                                        n.clone(),
                                        crate::typechecker::types::SubstValue::Type(t.clone()),
                                    )
                                })
                                .collect::<std::collections::HashMap<_, _>>();
                            if subs.is_empty() {
                                lower(&w)
                            } else {
                                lower(&crate::typechecker::inference::substitute_type_params(
                                    &w, &subs,
                                ))
                            }
                        }
                        None => Err(LowerError::Unsupported("impl Trait")),
                    };
                }
            }
            // A refined value is its base's: the predicate is checked where
            // one is made, and the name only gates what checks.
            Type::Refinement { base, .. } => return lower(base),
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
            user: false,
            sync: false,
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
