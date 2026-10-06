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
    /// at most one time.
    Fn {
        params: TyList,
        ret: Ty,
        once: bool,
    },
    Ref(Ty),
    MutRef(Ty),
    /// A raw pointer, for the stdlib's intrinsics (proposal § 5.4).
    RawPtr {
        mutable: bool,
        pointee: Ty,
    },
    Param(ParamTy),
    /// A type that already failed to check; never reaches MIR.
    Error,
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
            TyKind::Tuple(list) | TyKind::Adt { args: list, .. } => {
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
            TyKind::Array { elem, .. } | TyKind::Slice { elem, .. } => self.walk(elem, f),
            TyKind::Ref(t)
            | TyKind::MutRef(t)
            | TyKind::Weak(t)
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
            | TyKind::Error => {}
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
            TyKind::Fn { params, ret, once } => TyKind::Fn {
                params: self.subst_list(params, args, const_args),
                ret: self.subst(ret, args, const_args),
                once,
            },
            TyKind::Array { elem, len } => TyKind::Array {
                elem: self.subst(elem, args, const_args),
                len: match len {
                    ArrayLen::Param(p) => const_args.get(p.index as usize).copied().unwrap_or(len),
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
            TyKind::Fn { params, ret, once } => format!(
                "{}fn({}) -> {}",
                if once { "once " } else { "" },
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
            TyKind::Error => "{error}".into(),
        }
    }

    // ── bridge from the legacy typechecker ──────────────────────────

    /// Convert a legacy `typechecker::Type`. `lookup` maps a type name to its
    /// definition; `param` places a generic parameter name in the current
    /// owner's generics.
    pub fn lower_legacy(
        &self,
        ty: &Type,
        lookup: &dyn Fn(&str) -> Option<DefId>,
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
                len: match size {
                    ConstArg::Literal(n) if *n >= 0 => ArrayLen::Known(*n as u64),
                    ConstArg::ConstParam(name) => ArrayLen::Param(param_ty(name)?),
                    ConstArg::ConstVar(_) => return Err(LowerError::Unresolved),
                    ConstArg::Literal(_) | ConstArg::DynamicDim => {
                        return Err(LowerError::Unsupported("array length"))
                    }
                },
            },
            Type::Slice { element, mutable } => TyKind::Slice {
                elem: lower(element)?,
                mutable: *mutable,
            },
            Type::Named { name, args } => TyKind::Adt {
                def: lookup(name).ok_or_else(|| LowerError::UnknownName(name.clone()))?,
                args: lower_all(args)?,
            },
            Type::Shared(name) => TyKind::Adt {
                def: lookup(name).ok_or_else(|| LowerError::UnknownName(name.clone()))?,
                args: self.empty_list(),
            },
            Type::Function {
                params,
                return_type,
            } => TyKind::Fn {
                params: lower_all(params)?,
                ret: lower(return_type)?,
                once: false,
            },
            Type::OnceFunction {
                params,
                return_type,
            } => TyKind::Fn {
                params: lower_all(params)?,
                ret: lower(return_type)?,
                once: true,
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
            Type::Vector { .. } => return Err(LowerError::Unsupported("Vector[T, N]")),
            Type::Shape(_) => return Err(LowerError::Unsupported("shape arguments")),
            Type::AssocProjection { .. } => {
                return Err(LowerError::Unsupported("associated type projection"))
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
        ["Vec", "Node", "Option"][d.0 as usize].to_string()
    }

    fn lookup(name: &str) -> Option<DefId> {
        ["Vec", "Node", "Option"]
            .iter()
            .position(|n| *n == name)
            .map(|i| DefId(i as u32))
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
}
