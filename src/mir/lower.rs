//! The MIR builder: typed HIR (the AST plus the checker's node tables) to
//! `Built` MIR, one body per monomorphic instance reachable from `main`
//! (redesign M1; `docs/spikes/mir-types.md` §4).
//!
//! What `Built` promises is in the spike's §3: every scope end, early exit,
//! assignment and temporary already carries its `Drop`, in the order core
//! semantics D1 to D5 require, and a `Drop` may name a place that was moved
//! on some path. Drop elaboration removes those.
//!
//! The builder never guesses. A construct it does not lower yet is reported
//! in [`Lowered::errors`] with its span, and the program is not run.

use rustc_hash::{FxHashMap, FxHashSet};

use crate::ast::{
    self, BinOp as AstBinOp, Block, CallArg, CompoundOp, Expr, ExprKind, Function, ImplItem, Item,
    LiteralPattern, ParsedInterpolationPart, Pattern, PatternKind, SelfParam, Stmt, StmtKind,
    UnaryOp,
};
use crate::def_path::DefPath;
use crate::def_table::ProgramDefs;
use crate::ids::{DefId, DefKind, NodeId};
use crate::node_res::Res;
use crate::ownership::OwnershipMode;
use crate::resolver::{ResolveResult, SpanKey, SymbolId};
use crate::token::Span;
use crate::ty::{AdtDef, IntrinsicKind, TyKind as HK, TypeName, VariantDef};
use crate::typechecker::types::{IntSize, Type, UIntSize, VariantTypeInfo};
use crate::typechecker::TypeCheckResult;
use crate::typed_hir::{Callee, ResolvedCall, TypedHir};

use super::build::BodyBuilder;
use super::interp;
use super::syntax::*;
use super::ty::{AdtId, Ty, TyInterner};

/// The lowered program, or why it could not be lowered.
pub struct Lowered {
    pub program: interp::Program,
    pub tys: TyInterner,
    /// One line per construct the builder does not lower yet, with its
    /// position. Empty when every reachable body was built.
    pub errors: Vec<String>,
    /// The instances that take `ref self` or `mut ref self`.
    pub receivers: FxHashSet<String>,
}

/// Lower every instance reachable from `main`.
pub fn lower_program(
    program: &ast::Program,
    tc: &TypeCheckResult,
    defs: &ProgramDefs,
    res: &FxHashMap<NodeId, Res>,
    rr: &ResolveResult,
    hir: TypedHir,
    capture_modes: &FxHashMap<SpanKey, Vec<(String, OwnershipMode)>>,
) -> Lowered {
    let TypedHir {
        tcx,
        node_types,
        calls,
        errors: _,
    } = hir;
    let mut lcx = Lcx {
        tc,
        ref_bindings: &program.ref_binding_spans,
        defs,
        res,
        node_types,
        calls,
        tys: TyInterner::from_tcx(tcx),
        binding_syms: rr
            .binding_nodes
            .iter()
            .map(|(&s, &n)| ((n, rr.symbol_table.get_symbol(s).name.clone()), s))
            .collect(),
        fns: FxHashMap::default(),
        consts: FxHashMap::default(),
        self_tys: FxHashMap::default(),
        queue: Vec::new(),
        queued: FxHashSet::default(),
        program: interp::Program::default(),
        errors: Vec::new(),
        variants: FxHashMap::default(),
        registered: FxHashSet::default(),
        capture_modes,
        next_closure: defs.table.len() as u32,
        closures: FxHashMap::default(),
        closure_exprs: FxHashMap::default(),
        closure_queue: Vec::new(),
        fn_param_tys: FxHashMap::default(),
    };
    lcx.index_functions(program);
    // Contracts are checked at run time; until the builder emits those
    // checks, a program with one is refused rather than run without them.
    for item in &program.items {
        if let Item::StructDef(sd) = item {
            if !sd.invariants.is_empty() || !sd.impl_invariants.is_empty() {
                lcx.errors.push(format!(
                    "{}: a type invariant is not lowered yet",
                    at(sd.span)
                ));
            }
        }
    }
    match defs.lookup(0, "main") {
        Some(main) => {
            lcx.instance(main, Vec::new());
        }
        None => lcx.errors.push("no `main` function".into()),
    }
    let mut receivers = FxHashSet::default();
    loop {
        if let Some((def, args, name)) = lcx.queue.pop() {
            if lcx
                .fns
                .get(&def)
                .is_some_and(|i| matches!(i.f.self_param, Some(SelfParam::Ref | SelfParam::MutRef)))
            {
                receivers.insert(name.clone());
            }
            lcx.lower_instance(def, &args, &name);
        } else if let Some(job) = lcx.closure_queue.pop() {
            lcx.lower_closure(job);
        } else {
            break;
        }
    }
    Lowered {
        program: lcx.program,
        tys: lcx.tys,
        errors: lcx.errors,
        receivers,
    }
}

struct FnItem<'a> {
    f: &'a Function,
    /// The impl's target type and how many generic parameters the impl
    /// declares (they come first in the method's positional generics).
    impl_target: Option<DefId>,
    impl_params: usize,
}

struct Lcx<'a> {
    tc: &'a TypeCheckResult,
    /// `ref name` pattern bindings, by the binding's span.
    ref_bindings: &'a FxHashSet<SpanKey>,
    defs: &'a ProgramDefs,
    res: &'a FxHashMap<NodeId, Res>,
    node_types: FxHashMap<NodeId, Ty>,
    calls: FxHashMap<NodeId, ResolvedCall>,
    tys: TyInterner,
    /// The symbol a pattern node binds under a name (a struct pattern's
    /// shorthand fields all bind through that pattern's node).
    binding_syms: FxHashMap<(NodeId, String), SymbolId>,
    fns: FxHashMap<DefId, FnItem<'a>>,
    /// Module-level constants, by definition.
    consts: FxHashMap<DefId, &'a ast::ConstDecl>,
    /// The receiver type a method of a non-generic impl was called with:
    /// its `self` type when the impl names a concrete instance of a
    /// generic type (`impl Joiner for Vec[String]`).
    self_tys: FxHashMap<DefId, Ty>,
    queue: Vec<(DefId, Vec<Ty>, String)>,
    queued: FxHashSet<String>,
    program: interp::Program,
    errors: Vec<String>,
    /// A variant's enum and its index there.
    variants: FxHashMap<DefId, (DefId, u32)>,
    /// ADTs whose definition (and `Drop` body) has been registered.
    registered: FxHashSet<DefId>,
    /// The ownership checker's capture modes, by the closure's span.
    capture_modes: &'a FxHashMap<SpanKey, Vec<(String, OwnershipMode)>>,
    /// Closures get DefIds past the definition table's.
    next_closure: u32,
    /// Each closure's body name and how it takes its environment.
    closures: FxHashMap<DefId, (String, EnvMode)>,
    closure_exprs: FxHashMap<DefId, &'a Expr>,
    closure_queue: Vec<ClosureJob<'a>>,
    /// Instances whose function-typed parameters take a concrete closure
    /// or function type: by instance name, one entry per parameter.
    fn_param_tys: FxHashMap<String, Vec<Option<Ty>>>,
}

/// How a closure's body takes its environment (core semantics §9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EnvMode {
    Ref,
    Mut,
    /// A closure whose body moves a capture out: called once, by value.
    Value,
}

/// How one capture is held in the environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CapMode {
    /// Moved or copied in.
    Value,
    Ref,
    Mut,
}

struct ClosureJob<'a> {
    def: DefId,
    name: String,
    expr: &'a Expr,
    /// The enclosing instance's type arguments.
    args: Vec<Ty>,
    /// Captured symbol, how, and the environment field's type.
    caps: Vec<(SymbolId, CapMode, Ty)>,
    env: EnvMode,
    ty: Ty,
}

/// A variant as the checker records it: its name and named field types.
type CheckedVariant = (String, Vec<(String, Type)>);

fn at(span: Span) -> String {
    format!("{}:{}", span.line, span.column)
}

impl<'a> Lcx<'a> {
    // ── items ───────────────────────────────────────────────────────

    /// Map each function and method in the AST to its DefId. An impl is
    /// `[..target, "impl#n"]` or `[..target, "impl Trait#n"]` with `n`
    /// counting that target's impls of that trait in order, which is the
    /// order the definition table numbered them in.
    fn index_functions(&mut self, program: &'a ast::Program) {
        let mut impl_counts: FxHashMap<(Vec<String>, String), usize> = FxHashMap::default();
        for item in &program.items {
            match item {
                Item::Function(f) => {
                    if let Some(d) = self.defs.lookup(0, &f.name) {
                        self.fns.insert(
                            d,
                            FnItem {
                                f,
                                impl_target: None,
                                impl_params: 0,
                            },
                        );
                    }
                }
                Item::ConstDecl(c) => {
                    if let Some(d) = self.defs.lookup(0, &c.name) {
                        self.consts.insert(d, c);
                    }
                }
                Item::ImplBlock(b) => {
                    let target_name = match &b.target_type.kind {
                        ast::TypeKind::Path(p) => p.segments.last().cloned(),
                        _ => None,
                    };
                    let Some(target) = target_name.and_then(|n| self.defs.lookup(0, &n)) else {
                        continue;
                    };
                    let target_path = self.defs.table.get(target).path.segments.clone();
                    let label = match b.trait_name.as_ref().and_then(|t| t.segments.last()) {
                        Some(t) => format!("impl {t}"),
                        None => "impl".to_string(),
                    };
                    let n = impl_counts
                        .entry((target_path.clone(), label.clone()))
                        .or_insert(0);
                    let mut impl_path = target_path;
                    impl_path.push(format!("{label}#{n}"));
                    *n += 1;
                    let impl_params = b.generic_params.as_ref().map_or(0, |g| g.params.len());
                    for it in &b.items {
                        let ImplItem::Method(f) = it else { continue };
                        let mut p = impl_path.clone();
                        p.push(f.name.clone());
                        if let Some(d) = self.defs.table.lookup(&DefPath::new(p)) {
                            self.fns.insert(
                                d,
                                FnItem {
                                    f,
                                    impl_target: Some(target),
                                    impl_params,
                                },
                            );
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn def_name(&self, d: DefId) -> String {
        self.defs
            .table
            .get(d)
            .path
            .segments
            .last()
            .cloned()
            .unwrap_or_default()
    }

    /// Queue an instance of `def[args]` whose function-typed parameters take
    /// the given closure or function types, and return its name.
    fn instance_with_fns(&mut self, def: DefId, args: Vec<Ty>, fns: Vec<Option<Ty>>) -> String {
        let shown: Vec<String> = fns.iter().flatten().map(|&t| self.tys.display(t)).collect();
        let name = format!("{}{{{}}}", self.instance_name(def, &args), shown.join(", "));
        if self.queued.insert(name.clone()) {
            self.fn_param_tys.insert(name.clone(), fns);
            self.queue.push((def, args, name.clone()));
        }
        name
    }

    /// Queue the instance `def[args]` if it is new, and return its name.
    fn instance(&mut self, def: DefId, args: Vec<Ty>) -> String {
        let name = self.instance_name(def, &args);
        if self.queued.insert(name.clone()) {
            self.queue.push((def, args, name.clone()));
        }
        name
    }

    /// `f`, `f[i64]`, `R.drop`, `Pair[i64].put[bool]`.
    fn instance_name(&self, def: DefId, args: &[Ty]) -> String {
        let show = |ts: &[Ty]| {
            ts.iter()
                .map(|&t| self.tys.display(t))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let name = self.def_name(def);
        match self.fns.get(&def) {
            Some(FnItem {
                impl_target: Some(t),
                impl_params,
                ..
            }) => {
                let (impl_args, own) = args.split_at((*impl_params).min(args.len()));
                let mut s = self.def_name(*t);
                if !impl_args.is_empty() {
                    s = format!("{s}[{}]", show(impl_args));
                }
                s.push('.');
                s.push_str(&name);
                if !own.is_empty() {
                    s = format!("{s}[{}]", show(own));
                }
                s
            }
            _ if args.is_empty() => name,
            _ => format!("{name}[{}]", show(args)),
        }
    }

    fn lower_instance(&mut self, def: DefId, args: &[Ty], name: &str) {
        let Some(item) = self.fns.get(&def) else {
            self.errors.push(format!("no body for `{name}`"));
            return;
        };
        let f = item.f;
        let impl_target = item.impl_target.map(|t| (t, item.impl_params));
        let instance = InstanceId {
            def,
            args: args.to_vec(),
            name: name.to_string(),
        };
        let ret = match self.fn_return(f, args) {
            Ok(t) => t,
            Err(e) => {
                self.errors.push(format!("{} `{name}`: {e}", at(f.span)));
                return;
            }
        };
        let fn_params = self.fn_param_tys.get(name).cloned().unwrap_or_default();
        let mut bx = Bx::new(self, instance, ret, args.to_vec());
        bx.lower_fn(f, impl_target, &fn_params);
        let (body, errors) = bx.finish();
        self.errors.extend(errors);
        if let Some(body) = body {
            self.program.add(body);
        }
    }

    /// Build a closure's body: the environment first, then its parameters.
    fn lower_closure(&mut self, job: ClosureJob<'a>) {
        let ExprKind::Closure { params, body, .. } = &job.expr.kind else {
            return;
        };
        let Some(&fn_ty) = self.node_types.get(&job.expr.id) else {
            self.errors.push(format!(
                "{} closure with no recorded type",
                at(job.expr.span)
            ));
            return;
        };
        let ret = match self.tys.tcx().kind(fn_ty) {
            HK::Fn { ret, .. } => self.mir_ty(ret, &job.args),
            _ => Err("a closure whose type is not a function".to_string()),
        };
        let ret = match ret {
            Ok(t) => t,
            Err(e) => {
                self.errors
                    .push(format!("{} `{}`: {e}", at(job.expr.span), job.name));
                return;
            }
        };
        let instance = InstanceId {
            def: job.def,
            args: job.args.clone(),
            name: job.name.clone(),
        };
        let mut bx = Bx::new(self, instance, ret, job.args.clone());
        let _ = bx.lower_closure_body(&job, params, body);
        let (body, errors) = bx.finish();
        self.errors.extend(errors);
        if let Some(body) = body {
            self.program.add(body);
        }
    }

    /// The declared return type of `f`, instantiated.
    fn fn_return(&mut self, f: &Function, args: &[Ty]) -> Result<Ty, String> {
        let key = SpanKey::from_span(&f.span);
        let Some((ty, frame)) = self.tc.fn_return_types.get(&key) else {
            return Err("its return type was not recorded".into());
        };
        let params = self
            .tc
            .node_generic_frames
            .get(*frame as usize)
            .cloned()
            .unwrap_or_default();
        let hir = self.lower_legacy(ty, &params)?;
        self.mir_ty(hir, args)
    }

    fn lower_legacy(&self, ty: &Type, params: &[String]) -> Result<Ty, String> {
        let defs = self.defs;
        let lookup = |name: &str| {
            let d = defs.lookup(0, name)?;
            match defs.table.get(d).kind {
                DefKind::Trait => Some(TypeName::Trait(d)),
                _ => Some(TypeName::Adt(d)),
            }
        };
        let param = |name: &str| params.iter().position(|p| p == name).map(|i| i as u32);
        self.tys
            .tcx()
            .lower_legacy(ty, &lookup, &param)
            .map_err(|e| format!("type {e:?}"))
    }

    // ── types ───────────────────────────────────────────────────────

    /// The MIR type of a typed-HIR type, with the instance's arguments
    /// substituted. The library collections become intrinsics and a
    /// `shared` ADT a counted handle; every ADT reached is registered.
    fn mir_ty(&mut self, ty: Ty, args: &[Ty]) -> Result<Ty, String> {
        let tcx = self.tys.tcx();
        let ty = tcx.subst(ty, args, &[]);
        self.convert(ty)
    }

    fn convert(&mut self, ty: Ty) -> Result<Ty, String> {
        let kind = self.tys.tcx().kind(ty);
        let list = |s: &mut Self, l| -> Result<Vec<Ty>, String> {
            let tys = s.tys.tcx().list(l);
            tys.into_iter().map(|t| s.convert(t)).collect()
        };
        let new = match kind {
            HK::Adt { def, args } => {
                let args = list(self, args)?;
                let name = self.def_name(def);
                let std = self
                    .defs
                    .table
                    .get(def)
                    .path
                    .segments
                    .first()
                    .map(String::as_str)
                    == Some("std");
                let intrinsic = match name.as_str() {
                    "Vec" if std => Some(IntrinsicKind::Vec),
                    "Map" if std => Some(IntrinsicKind::Map),
                    "Set" if std => Some(IntrinsicKind::Set),
                    "VecDeque" if std => Some(IntrinsicKind::VecDeque),
                    "SortedMap" if std => Some(IntrinsicKind::SortedMap),
                    "SortedSet" if std => Some(IntrinsicKind::SortedSet),
                    _ => None,
                };
                if let Some(kind) = intrinsic {
                    let args = self.tys.tcx().intern_list(&args);
                    HK::Intrinsic { kind, args }
                } else {
                    let shared = self.register_adt(def)?;
                    let args = self.tys.tcx().intern_list(&args);
                    if shared {
                        HK::Shared { def, args }
                    } else {
                        HK::Adt { def, args }
                    }
                }
            }
            HK::Tuple(l) => {
                let tys = list(self, l)?;
                HK::Tuple(self.tys.tcx().intern_list(&tys))
            }
            HK::Array { elem, len } => HK::Array {
                elem: self.convert(elem)?,
                len,
            },
            HK::Slice { elem, mutable } => HK::Slice {
                elem: self.convert(elem)?,
                mutable,
            },
            HK::Ref(t) => HK::Ref(self.convert(t)?),
            HK::MutRef(t) => HK::MutRef(self.convert(t)?),
            HK::Opaque { .. } => return Err("an iterator value".into()),
            HK::Error => return Err("a type that failed to check".into()),
            HK::Weak(_) => return Err("a `weak` type".into()),
            HK::Fn { .. } => return Err("a function value".into()),
            HK::RawPtr { .. } => return Err("a raw pointer".into()),
            other => other,
        };
        Ok(self.tys.tcx().intern(new))
    }

    /// Register the definition of the ADT `def` (and its `Drop` body),
    /// once. Returns whether it is `shared`.
    fn register_adt(&mut self, def: DefId) -> Result<bool, String> {
        let name = self.def_name(def);
        let tc = self.tc;
        let (generics, variants, shared, derived): (_, Vec<CheckedVariant>, _, _) =
            if let Some(s) = tc.struct_info.get(&name) {
                (
                    s.generic_params.clone(),
                    vec![(
                        name.clone(),
                        s.fields
                            .iter()
                            .map(|(n, t, _)| (n.clone(), t.clone()))
                            .collect(),
                    )],
                    s.is_shared,
                    &s.derived_traits,
                )
            } else if let Some(e) = tc.enum_info.get(&name) {
                let vs = e
                    .variants
                    .iter()
                    .map(|(n, v)| {
                        let fields = match v {
                            VariantTypeInfo::Unit => Vec::new(),
                            VariantTypeInfo::Tuple(ts) => ts
                                .iter()
                                .enumerate()
                                .map(|(i, t)| (i.to_string(), t.clone()))
                                .collect(),
                            VariantTypeInfo::Struct(fs) => fs.clone(),
                        };
                        (n.clone(), fields)
                    })
                    .collect();
                (e.generic_params.clone(), vs, e.is_shared, &e.derived_traits)
            } else {
                return Err(format!("no definition for type `{name}`"));
            };
        if !self.registered.insert(def) {
            return Ok(shared);
        }
        let is_enum = tc.enum_info.contains_key(&name);
        let mut vdefs = Vec::new();
        for (i, (vname, fields)) in variants.iter().enumerate() {
            let mut fs = Vec::new();
            for (fname, fty) in fields {
                let hir = self.lower_legacy(fty, &generics)?;
                fs.push((fname.clone(), self.convert(hir)?));
            }
            if is_enum {
                if let Some(v) = self.defs.variant(def, vname) {
                    self.variants.insert(v, (def, i as u32));
                }
            }
            vdefs.push(VariantDef {
                name: vname.clone(),
                fields: fs,
            });
        }
        let has_drop_impl = tc.drop_method_keys.contains_key(&name);
        self.tys.tcx().add_adt_def(AdtDef {
            def,
            name: name.clone(),
            is_enum,
            variants: vdefs,
            has_drop_impl,
            is_copy: derived.contains("Copy"),
        });
        if has_drop_impl {
            let drop_fn = self
                .defs
                .methods
                .get(&def)
                .and_then(|ms| ms.get("drop"))
                .and_then(|c| c.first().copied());
            match drop_fn {
                Some(d) if generics.is_empty() => {
                    let inst = self.instance(d, Vec::new());
                    self.program.drop_impls.insert(AdtId(def.0), inst);
                }
                Some(_) => return Err(format!("the `Drop` body of generic type `{name}`")),
                None => return Err(format!("no `drop` method found for `{name}`")),
            }
        }
        Ok(shared)
    }

    fn variant(&mut self, v: DefId) -> Option<(DefId, u32)> {
        if let Some(&x) = self.variants.get(&v) {
            return Some(x);
        }
        let enum_def = *self.defs.parent.get(&v)?;
        self.register_adt(enum_def).ok()?;
        self.variants.get(&v).copied()
    }
}

// ── bodies ──────────────────────────────────────────────────────────

enum ScopeEntry<'a> {
    Drop(Place),
    Defer(&'a Block),
    ErrDefer(&'a Block),
    /// A local whose storage ends with the scope.
    Storage(Local),
}

struct LoopCx {
    label: Option<String>,
    break_bb: BasicBlock,
    continue_bb: BasicBlock,
    depth: usize,
    dest: Option<Place>,
}

struct Bx<'l, 'a> {
    lcx: &'l mut Lcx<'a>,
    b: BodyBuilder,
    args: Vec<Ty>,
    cur: BasicBlock,
    locals: FxHashMap<SymbolId, Local>,
    self_local: Option<Local>,
    scopes: Vec<Vec<ScopeEntry<'a>>>,
    loops: Vec<LoopCx>,
    errors: Vec<String>,
    name: String,
    /// In a closure body: where each captured symbol lives.
    captured: FxHashMap<SymbolId, Place>,
    /// Closures created in this body so far, for their names.
    closure_count: u32,
    /// Types for expressions the checker left untyped, taken from where
    /// they are used (`v.push(None)` gives `None` the element type).
    ty_hints: FxHashMap<NodeId, Ty>,
}

type R<T> = Result<T, ()>;

impl<'l, 'a> Bx<'l, 'a> {
    fn new(lcx: &'l mut Lcx<'a>, instance: InstanceId, ret: Ty, args: Vec<Ty>) -> Self {
        let name = instance.name.clone();
        let mut b = BodyBuilder::new(instance, ret);
        let cur = b.new_block();
        Bx {
            lcx,
            b,
            args,
            cur,
            locals: FxHashMap::default(),
            self_local: None,
            scopes: Vec::new(),
            loops: Vec::new(),
            errors: Vec::new(),
            name,
            captured: FxHashMap::default(),
            closure_count: 0,
            ty_hints: FxHashMap::default(),
        }
    }

    fn finish(mut self) -> (Option<Body>, Vec<String>) {
        if !self.errors.is_empty() {
            return (None, self.errors);
        }
        self.b.terminate_open_blocks(TerminatorKind::Unreachable);
        match self.b.finish() {
            Ok(mut body) => {
                clear_unreachable_blocks(&mut body);
                (Some(body), Vec::new())
            }
            Err(e) => (None, vec![format!("`{}`: {e}", self.name)]),
        }
    }

    fn unsupported<T>(&mut self, span: Span, what: &str) -> R<T> {
        self.errors.push(format!(
            "{} in `{}`: {what} is not lowered yet",
            at(span),
            self.name
        ));
        Err(())
    }

    fn tys(&self) -> &TyInterner {
        &self.lcx.tys
    }

    /// The MIR type of the node `id` in this instance.
    fn node_ty(&mut self, id: NodeId, span: Span) -> R<Ty> {
        let Some(&t) = self.lcx.node_types.get(&id) else {
            if let Some(&t) = self.ty_hints.get(&id) {
                return Ok(t);
            }
            return self.unsupported(span, "an expression with no recorded type");
        };
        let args = self.args.clone();
        match self.lcx.mir_ty(t, &args) {
            Ok(t) => Ok(t),
            Err(e) => self.unsupported(span, &e),
        }
    }

    /// The MIR type of `e`'s value. A local reads as the type of its MIR
    /// local, which differs from the checker's when it binds by reference
    /// (`ref name` patterns, `self` under `ref self`).
    fn expr_ty(&mut self, e: &Expr) -> R<Ty> {
        match &e.kind {
            ExprKind::Identifier(_) => {
                if let Some(Res::Local(sym)) = self.lcx.res.get(&e.id) {
                    if let Some(&l) = self.locals.get(sym) {
                        return Ok(self.b.local_ty(l));
                    }
                    if let Some(p) = self.captured.get(sym).cloned() {
                        return Ok(self.place_type(&p));
                    }
                }
            }
            ExprKind::SelfValue => {
                if let Some(l) = self.self_local {
                    return Ok(self.b.local_ty(l));
                }
            }
            ExprKind::FieldAccess { .. } => {
                if let Some((t, _)) = self.int_limit_of(e) {
                    return Ok(t);
                }
            }
            _ => {}
        }
        self.node_ty(e.id, e.span)
    }

    fn unit(&self) -> Ty {
        self.tys().unit()
    }

    fn is_copy(&self, t: Ty) -> bool {
        self.tys().is_copy(t)
    }

    fn needs_drop(&self, t: Ty) -> bool {
        self.tys().needs_drop(t)
    }

    fn assign(&mut self, place: impl Into<Place>, rv: Rvalue) {
        let bb = self.cur;
        self.b.assign(bb, place, rv);
    }

    /// End the current block with `kind`; continue in `next`.
    fn goto_with(&mut self, kind: TerminatorKind, next: BasicBlock) {
        let bb = self.cur;
        self.b.terminate(bb, kind);
        self.cur = next;
    }

    fn goto(&mut self, target: BasicBlock) {
        let bb = self.cur;
        self.b.terminate(bb, TerminatorKind::Goto { target });
    }

    /// End the current block with a terminator that does not fall through,
    /// and continue in a fresh (unreachable) block.
    fn diverge(&mut self, kind: TerminatorKind) {
        let next = self.b.new_block();
        self.goto_with(kind, next);
    }

    fn temp(&mut self, t: Ty) -> Local {
        self.b.temp(t)
    }

    /// A temporary that is dropped at the end of the innermost scope (the
    /// statement's, for a statement's temporaries).
    fn scoped_temp(&mut self, t: Ty) -> Local {
        let l = self.temp(t);
        if self.needs_drop(t) {
            self.schedule(ScopeEntry::Drop(Place::local(l)));
        }
        l
    }

    fn schedule(&mut self, e: ScopeEntry<'a>) {
        self.scopes
            .last_mut()
            .expect("a body always has a scope")
            .push(e);
    }

    fn push_scope(&mut self) {
        self.scopes.push(Vec::new());
    }

    /// Leave the innermost scope normally: its drops and `defer` bodies in
    /// reverse order of introduction.
    fn pop_scope(&mut self) -> R<()> {
        let entries = self.scopes.pop().expect("scope underflow");
        self.exit_entries(&entries, false)
    }

    fn exit_entries(&mut self, entries: &[ScopeEntry<'a>], error: bool) -> R<()> {
        for e in entries.iter().rev() {
            match e {
                ScopeEntry::Drop(p) => {
                    let next = self.b.new_block();
                    self.goto_with(
                        TerminatorKind::Drop {
                            place: p.clone(),
                            target: next,
                            unwind: UnwindAction::Abort,
                        },
                        next,
                    );
                }
                ScopeEntry::Defer(body) => self.defer_body(body)?,
                ScopeEntry::ErrDefer(body) if error => self.defer_body(body)?,
                ScopeEntry::ErrDefer(_) => {}
                ScopeEntry::Storage(l) => {
                    let bb = self.cur;
                    self.b.push(bb, StatementKind::StorageDead(*l));
                }
            }
        }
        Ok(())
    }

    fn defer_body(&mut self, body: &'a Block) -> R<()> {
        let t = self.unit();
        let tmp = self.temp(t);
        self.block_into(body, Place::local(tmp))
    }

    /// Run the exit sequence of every scope above `depth`, innermost first,
    /// without popping them (an early exit).
    fn exit_to(&mut self, depth: usize, error: bool) -> R<()> {
        let mut i = self.scopes.len();
        while i > depth {
            i -= 1;
            let entries = std::mem::take(&mut self.scopes[i]);
            let r = self.exit_entries(&entries, error);
            self.scopes[i] = entries;
            r?;
        }
        Ok(())
    }

    // ── functions ───────────────────────────────────────────────────

    fn lower_fn(
        &mut self,
        f: &'a Function,
        impl_target: Option<(DefId, usize)>,
        fn_params: &[Option<Ty>],
    ) {
        let _ = self.lower_fn_inner(f, impl_target, fn_params);
    }

    fn lower_fn_inner(
        &mut self,
        f: &'a Function,
        impl_target: Option<(DefId, usize)>,
        fn_params: &[Option<Ty>],
    ) -> R<()> {
        if !f.requires.is_empty() || !f.ensures.is_empty() {
            return self.unsupported(f.span, "a `requires`/`ensures` contract");
        }
        self.push_scope();
        if let Some(mode) = &f.self_param {
            let Some((target, n)) = impl_target else {
                return self.unsupported(f.span, "a `self` parameter outside an impl");
            };
            let seen = (n == 0)
                .then(|| self.lcx.self_tys.get(&self.b.instance().def).copied())
                .flatten();
            let t = match seen {
                Some(t) => t,
                None => {
                    let tcx = self.tys().tcx();
                    let params: Vec<Ty> = self.args[..n.min(self.args.len())].to_vec();
                    let target_ty = tcx.adt(target, &params);
                    match self.lcx.convert(target_ty) {
                        Ok(t) => t,
                        Err(e) => return self.unsupported(f.span, &e),
                    }
                }
            };
            let t = match mode {
                SelfParam::Owned => t,
                SelfParam::Ref => self.tys().tcx().reference(t, false),
                SelfParam::MutRef => self.tys().tcx().reference(t, true),
            };
            let l = self.b.arg("self", t);
            self.self_local = Some(l);
            if self.needs_drop(t) {
                self.schedule(ScopeEntry::Drop(Place::local(l)));
            }
        }
        for (i, p) in f.params.iter().enumerate() {
            let PatternKind::Binding(name) = &p.pattern.kind else {
                return self.unsupported(p.span, "a destructuring parameter");
            };
            let t = match fn_params.get(i).copied().flatten() {
                Some(t) => t,
                None => self.node_ty(p.pattern.id, p.span)?,
            };
            let l = self.b.arg(name, t);
            if let Some(&sym) = self.lcx.binding_syms.get(&(p.pattern.id, name.clone())) {
                self.locals.insert(sym, l);
            }
            if self.needs_drop(t) {
                self.schedule(ScopeEntry::Drop(Place::local(l)));
            }
        }
        // The body's own scope sits inside the parameters' (D6). Falling
        // off the end is a return like any other, so a tail `Err(..)` runs
        // the `errdefer` bodies too.
        let ret = Place::local(Local::RETURN_PLACE);
        self.push_scope();
        for s in &f.body.stmts {
            self.stmt(s)?;
        }
        match &f.body.final_expr {
            Some(e) => {
                self.push_scope();
                self.expr_into(e, ret)?;
                self.pop_scope()?;
            }
            None => self.assign(ret, Rvalue::Use(unit_const(self.unit()))),
        }
        self.return_exit()?;
        self.scopes.clear();
        Ok(())
    }

    // ── blocks and statements ───────────────────────────────────────

    fn block_into(&mut self, block: &'a Block, dest: Place) -> R<()> {
        self.push_scope();
        for s in &block.stmts {
            self.stmt(s)?;
        }
        match &block.final_expr {
            Some(e) => {
                self.push_scope();
                self.expr_into(e, dest)?;
                self.pop_scope()?;
            }
            None => self.assign(dest, Rvalue::Use(unit_const(self.unit()))),
        }
        self.pop_scope()
    }

    fn stmt(&mut self, s: &'a Stmt) -> R<()> {
        match &s.kind {
            StmtKind::Let { pattern, value, .. } => {
                // The value's temporaries are the statement's; the bindings
                // belong to the enclosing block.
                if let PatternKind::Binding(name) = &pattern.kind {
                    if self.is_fn_typed(pattern.id) {
                        self.push_scope();
                        let (op, t) = self.fn_value(value)?;
                        let l = self.user_local(name, t, pattern.id);
                        self.assign(l, Rvalue::Use(op));
                        self.pop_scope()?;
                        self.declare(l, t);
                        return Ok(());
                    }
                    let t = self.node_ty(pattern.id, pattern.span)?;
                    let l = self.user_local(name, t, pattern.id);
                    self.push_scope();
                    self.expr_into(value, Place::local(l))?;
                    self.pop_scope()?;
                    self.declare(l, t);
                    return Ok(());
                }
                // `let _ = place;` binds nothing, so it moves nothing.
                if matches!(pattern.kind, PatternKind::Wildcard) && is_place_expr(value) {
                    return Ok(());
                }
                self.push_scope();
                let t = self.expr_ty(value)?;
                let tmp = self.scoped_temp(t);
                self.expr_into(value, Place::local(tmp))?;
                let mut binds = Vec::new();
                self.bind_irrefutable(pattern, Place::local(tmp), t, &mut binds)?;
                self.pop_scope()?;
                for (l, t) in binds {
                    self.declare(l, t);
                }
                Ok(())
            }
            StmtKind::Expr(e) => {
                self.push_scope();
                let t = self.expr_ty(e)?;
                let tmp = self.scoped_temp(t);
                self.expr_into(e, Place::local(tmp))?;
                self.pop_scope()
            }
            StmtKind::Assign { target, value } => {
                self.push_scope();
                let r = self.assign_stmt(target, value);
                r?;
                self.pop_scope()
            }
            StmtKind::CompoundAssign { target, op, value } => {
                self.push_scope();
                self.compound_assign(target, op.clone(), value)?;
                self.pop_scope()
            }
            StmtKind::Defer { body } => {
                self.schedule(ScopeEntry::Defer(body));
                Ok(())
            }
            StmtKind::ErrDefer {
                binding: None,
                body,
            } => {
                self.schedule(ScopeEntry::ErrDefer(body));
                Ok(())
            }
            _ => self.unsupported(s.span, "this statement"),
        }
    }

    fn user_local(&mut self, name: &str, t: Ty, node: NodeId) -> Local {
        let l = self.b.push_local(
            t,
            Mutability::Mut,
            LocalKind::User {
                name: name.to_string(),
                node,
            },
        );
        if let Some(&sym) = self.lcx.binding_syms.get(&(node, name.to_string())) {
            self.locals.insert(sym, l);
        }
        let bb = self.cur;
        self.b.push(bb, StatementKind::StorageLive(l));
        l
    }

    /// A binding's local now holds a value: it is dropped at the end of the
    /// enclosing scope.
    fn declare(&mut self, l: Local, t: Ty) {
        self.schedule(ScopeEntry::Storage(l));
        if self.needs_drop(t) {
            self.schedule(ScopeEntry::Drop(Place::local(l)));
        }
    }

    /// Bind the names of an irrefutable pattern by moving (or copying) the
    /// matching parts of `place` into fresh locals.
    fn bind_irrefutable(
        &mut self,
        pat: &'a Pattern,
        place: Place,
        t: Ty,
        out: &mut Vec<(Local, Ty)>,
    ) -> R<()> {
        // Destructuring through a reference binds references into the
        // referent, as a match through one does.
        if let HK::Ref(inner) | HK::MutRef(inner) = self.tys().tcx().kind(t) {
            if !matches!(pat.kind, PatternKind::Wildcard | PatternKind::Binding(_)) {
                return self.bind_pattern(pat, &place.project(ProjElem::Deref), inner, true, out);
            }
        }
        match &pat.kind {
            PatternKind::Wildcard => Ok(()),
            PatternKind::Binding(name) => {
                let l = self.user_local(name, t, pat.id);
                if self.is_handle(t) || self.is_handle_aggregate(t) {
                    self.count_copy(place, t, Place::local(l));
                } else {
                    let op = self.use_place(place, t);
                    self.assign(l, Rvalue::Use(op));
                }
                out.push((l, t));
                Ok(())
            }
            PatternKind::Tuple(ps) => {
                for (i, p) in ps.iter().enumerate() {
                    let Some(ft) = self.tys().field_ty(t, None, i as u32) else {
                        return self.unsupported(p.span, "this tuple pattern");
                    };
                    self.bind_irrefutable(p, place.field(i as u32, ft), ft, out)?;
                }
                Ok(())
            }
            PatternKind::Struct { fields, .. } => {
                for fp in fields {
                    let Some((idx, ft)) = self.field_of(t, None, &fp.name) else {
                        return self.unsupported(fp.span, "this field pattern");
                    };
                    let fplace = place.field(idx, ft);
                    match &fp.pattern {
                        Some(p) => self.bind_irrefutable(p, fplace, ft, out)?,
                        None => {
                            let l = self.user_local(&fp.name, ft, pat.id);
                            let op = self.use_place(fplace, ft);
                            self.assign(l, Rvalue::Use(op));
                            out.push((l, ft));
                        }
                    }
                }
                Ok(())
            }
            _ => self.unsupported(pat.span, "this pattern in a `let`"),
        }
    }

    /// The index and type of the field `name` of `t` (variant `variant` of
    /// an enum).
    fn field_of(&self, t: Ty, variant: Option<u32>, name: &str) -> Option<(u32, Ty)> {
        let (adt, _) = self.tys().tcx().adt_of(t)?;
        let v = adt.variants.get(variant.unwrap_or(0) as usize)?;
        let idx = v.fields.iter().position(|(n, _)| n == name)? as u32;
        let ft = self.tys().tcx().field_ty(t, variant, idx)?;
        Some((idx, ft))
    }

    fn assign_stmt(&mut self, target: &'a Expr, value: &'a Expr) -> R<()> {
        // Left to right: the target's own subexpressions, then the value,
        // then the old value is dropped and the new one stored (D4).
        let pending = self.prepare_place(target)?;
        let op = self.expr_operand(value)?;
        let op = self.settle(op);
        let place = self.finish_place(pending, true)?;
        let (place, t) = self.write_through(place, value)?;
        if self.needs_drop(t) {
            let next = self.b.new_block();
            self.goto_with(
                TerminatorKind::Drop {
                    place: place.clone(),
                    target: next,
                    unwind: UnwindAction::Abort,
                },
                next,
            );
        }
        self.assign(place, Rvalue::Use(op));
        Ok(())
    }

    /// An operand that reads through a projection (`copy (*_r)`, where `_r`
    /// borrows an element) read into a temporary now, so the borrow it
    /// reads through ends before the target's own borrow starts
    /// (`v[k] = v[j]`).
    fn settle(&mut self, op: Operand) -> Operand {
        match op {
            Operand::Copy(p) | Operand::Move(p) if !p.projection.is_empty() => {
                let t = self.place_type(&p);
                let l = self.temp(t);
                let read = self.use_place(p, t);
                self.assign(l, Rvalue::Use(read));
                self.use_place(Place::local(l), t)
            }
            other => other,
        }
    }

    /// The place an assignment writes: a `mut ref` binding assigned a value
    /// of its pointee type writes through the reference (`x = x + 1` for
    /// `x: mut ref i64`), and the type stored there.
    fn write_through(&mut self, place: Place, value: &'a Expr) -> R<(Place, Ty)> {
        let t = self.place_type(&place);
        if let HK::MutRef(inner) = self.tys().tcx().kind(t) {
            let vt = self.expr_ty(value)?;
            if !matches!(self.tys().tcx().kind(vt), HK::Ref(_) | HK::MutRef(_)) {
                return Ok((place.project(ProjElem::Deref), inner));
            }
        }
        Ok((place, t))
    }

    fn compound_assign(&mut self, target: &'a Expr, op: CompoundOp, value: &'a Expr) -> R<()> {
        let pending = self.prepare_place(target)?;
        let (rhs, _) = self.scalar_operand(value)?;
        let rhs = self.settle(rhs);
        let place = self.finish_place(pending, true)?;
        let (place, t) = self.write_through(place, value)?;
        let bin = match op {
            CompoundOp::Add => BinOp::Add,
            CompoundOp::Sub => BinOp::Sub,
            CompoundOp::Mul => BinOp::Mul,
            CompoundOp::Div => BinOp::Div,
            CompoundOp::Mod => BinOp::Rem,
            CompoundOp::BitAnd => BinOp::BitAnd,
            CompoundOp::BitOr => BinOp::BitOr,
            CompoundOp::BitXor => BinOp::BitXor,
            CompoundOp::Shl => BinOp::Shl,
            CompoundOp::Shr => BinOp::Shr,
        };
        if !self.is_copy(t) {
            return self.unsupported(target.span, "a compound assignment to a non-scalar");
        }
        let lhs = Operand::Copy(place.clone());
        self.arith(bin, lhs, rhs, t, place);
        Ok(())
    }

    /// An operator on `String`s: the library's `String.eq` (borrowing both
    /// sides) and `String.add` (taking the left, borrowing the right).
    fn string_op(&mut self, left: &'a Expr, bin: BinOp, right: &'a Expr, dest: Place) -> R<()> {
        match bin {
            BinOp::Eq | BinOp::Ne => {
                let l = self.lib_arg(left, true)?;
                let r = self.lib_arg(right, true)?;
                if bin == BinOp::Eq {
                    self.call_native("String.eq", vec![l, r], dest);
                } else {
                    let bool_t = self.tys().bool();
                    let t = self.temp(bool_t);
                    self.call_native("String.eq", vec![l, r], Place::local(t));
                    self.assign(
                        dest,
                        Rvalue::UnaryOp(UnOp::Not, Operand::Move(Place::local(t))),
                    );
                }
                Ok(())
            }
            BinOp::Add => {
                // `fn add(ref self, other: ref String) -> String` (core
                // semantics §3.1): `a + b` moves neither operand.
                let l = self.lib_arg(left, true)?;
                let r = self.lib_arg(right, true)?;
                self.call_native("String.add", vec![l, r], dest);
                Ok(())
            }
            _ => self.unsupported(left.span, "this operator on strings"),
        }
    }

    /// An operator's operand and its type, read through a reference
    /// (`*r + 1` and `r + 1` alike, for `r: ref i64`).
    fn scalar_operand(&mut self, e: &'a Expr) -> R<(Operand, Ty)> {
        let t = self.expr_ty(e)?;
        if let HK::Ref(inner) | HK::MutRef(inner) = self.tys().tcx().kind(t) {
            if self.is_copy(inner) {
                let p = self.expr_place(e, false)?.project(ProjElem::Deref);
                return Ok((Operand::Copy(p), inner));
            }
        }
        Ok((self.expr_operand(e)?, t))
    }

    /// `dest = l <op> r` on values of type `t`. Integer arithmetic is
    /// checked (core semantics C8): division by zero and overflow abort.
    fn arith(&mut self, bin: BinOp, l: Operand, r: Operand, t: Ty, dest: Place) {
        let int = matches!(self.tys().tcx().kind(t), HK::Int(_) | HK::UInt(_));
        // An unsuffixed literal takes the other operand's integer type (the
        // checker records `c += 27` with `c: i8` as an `i64` literal). A
        // shift's amount keeps its own type.
        let retype = |o: Operand| match o {
            Operand::Const(Const {
                kind: ConstKind::Scalar(v),
                ..
            }) if int => Operand::Const(Const {
                ty: t,
                kind: ConstKind::Scalar(v),
            }),
            o => o,
        };
        let l = retype(l);
        let r = if matches!(bin, BinOp::Shl | BinOp::Shr) {
            r
        } else {
            retype(r)
        };
        let checked = matches!(
            bin,
            BinOp::Add
                | BinOp::Sub
                | BinOp::Mul
                | BinOp::Div
                | BinOp::Rem
                | BinOp::Shl
                | BinOp::Shr
        );
        if !int || !checked {
            self.assign(dest, Rvalue::BinaryOp(bin, l, r));
            return;
        }
        if matches!(bin, BinOp::Div | BinOp::Rem) {
            let bool_t = self.tys().bool();
            let z = self.temp(bool_t);
            self.assign(
                z,
                Rvalue::BinaryOp(
                    BinOp::Eq,
                    r.clone(),
                    Operand::Const(Const {
                        ty: t,
                        kind: ConstKind::Scalar(0),
                    }),
                ),
            );
            let ok = self.b.new_block();
            let bad = self.b.new_block();
            self.goto_with(
                TerminatorKind::SwitchInt {
                    discr: Operand::Copy(Place::local(z)),
                    targets: SwitchTargets::if_else(bad, ok),
                },
                bad,
            );
            self.diverge(TerminatorKind::Abort {
                reason: AbortReason::DivByZero,
            });
            self.cur = ok;
        }
        let pair_t = {
            let tcx = self.tys().tcx();
            let b = tcx.intern(HK::Bool);
            tcx.tuple(&[t, b])
        };
        let pair = self.temp(pair_t);
        self.assign(pair, Rvalue::CheckedBinaryOp(bin, l, r));
        let bool_t = self.tys().bool();
        let ok = self.b.new_block();
        let bad = self.b.new_block();
        self.goto_with(
            TerminatorKind::SwitchInt {
                discr: Operand::Copy(Place::local(pair).field(1, bool_t)),
                targets: SwitchTargets::if_else(bad, ok),
            },
            bad,
        );
        self.diverge(TerminatorKind::Abort {
            reason: AbortReason::Overflow,
        });
        self.cur = ok;
        self.assign(
            dest,
            Rvalue::Use(Operand::Copy(Place::local(pair).field(0, t))),
        );
    }

    // ── places ──────────────────────────────────────────────────────

    /// Is `e` a place expression (it names memory rather than computing a
    /// value)?
    fn is_place(&self, e: &Expr) -> bool {
        match &e.kind {
            ExprKind::Identifier(_) => {
                matches!(self.lcx.res.get(&e.id), Some(Res::Local(_)))
            }
            ExprKind::SelfValue => true,
            // A field of a temporary is a place too: the temporary's.
            ExprKind::FieldAccess { .. } => self.int_limit_of(e).is_none(),
            // `v[a..b]` makes a slice value; `v[i]` names an element.
            ExprKind::Index { index, .. } => !matches!(index.kind, ExprKind::Range { .. }),
            ExprKind::TupleIndex { .. } => true,
            ExprKind::Unary {
                op: UnaryOp::Deref, ..
            } => true,
            _ => false,
        }
    }

    /// `i64.MAX`, `u8.MIN`: an integer type's limit, as its type and bits.
    fn int_limit_of(&self, e: &Expr) -> Option<(Ty, u128)> {
        let ExprKind::FieldAccess { object, field } = &e.kind else {
            return None;
        };
        let ExprKind::Identifier(name) = &object.kind else {
            return None;
        };
        if matches!(self.lcx.res.get(&object.id), Some(Res::Local(_))) {
            return None;
        }
        let tcx = self.tys().tcx();
        let kind = match name.as_str() {
            "i8" => HK::Int(IntSize::I8),
            "i16" => HK::Int(IntSize::I16),
            "i32" => HK::Int(IntSize::I32),
            "i64" => HK::Int(IntSize::I64),
            "i128" => HK::Int(IntSize::I128),
            "isize" => HK::Int(IntSize::Isize),
            "u8" => HK::UInt(UIntSize::U8),
            "u16" => HK::UInt(UIntSize::U16),
            "u32" => HK::UInt(UIntSize::U32),
            "u64" => HK::UInt(UIntSize::U64),
            "u128" => HK::UInt(UIntSize::U128),
            "usize" => HK::UInt(UIntSize::Usize),
            _ => return None,
        };
        let v = int_limit(kind, field)?;
        Some((tcx.intern(kind), v))
    }

    /// Evaluate the parts of a place expression that compute values (an
    /// index), leaving the place itself to `finish_place`.
    fn prepare_place(&mut self, e: &'a Expr) -> R<PendingPlace<'a>> {
        match &e.kind {
            ExprKind::Index { object, index } => {
                let idx = self.index_operand(object, index)?;
                Ok(PendingPlace::Index(object, idx, e))
            }
            _ => Ok(PendingPlace::Plain(e)),
        }
    }

    fn finish_place(&mut self, p: PendingPlace<'a>, mutable: bool) -> R<Place> {
        match p {
            PendingPlace::Plain(e) => self.expr_place(e, mutable),
            PendingPlace::Index(object, idx, e) => self.index_place(object, idx, e, mutable),
        }
    }

    /// The place `e` names. Field access through a reference derefs it.
    fn expr_place(&mut self, e: &'a Expr, mutable: bool) -> R<Place> {
        match &e.kind {
            ExprKind::Identifier(name) => match self.lcx.res.get(&e.id) {
                Some(Res::Local(sym)) => match self.locals.get(sym) {
                    Some(&l) => Ok(Place::local(l)),
                    None => match self.captured.get(sym) {
                        Some(p) => Ok(p.clone()),
                        None => self.unsupported(e.span, &format!("the capture of `{name}`")),
                    },
                },
                _ => self.temp_place(e),
            },
            ExprKind::SelfValue => match self.self_local {
                Some(l) => Ok(Place::local(l)),
                None => self.unsupported(e.span, "`self` here"),
            },
            ExprKind::FieldAccess { object, field } => {
                let (base, bt) = self.deref_place(object, mutable)?;
                match self.field_of(bt, None, field) {
                    Some((idx, ft)) => Ok(base.field(idx, ft)),
                    None => self.unsupported(e.span, &format!("the field `{field}`")),
                }
            }
            ExprKind::TupleIndex { object, index } => {
                let (base, bt) = self.deref_place(object, mutable)?;
                match self.tys().field_ty(bt, None, *index as u32) {
                    Some(ft) => Ok(base.field(*index as u32, ft)),
                    None => self.unsupported(e.span, "this tuple index"),
                }
            }
            // `v[a..b]` is a value (a slice, or a new `String`).
            ExprKind::Index { index, .. } if matches!(index.kind, ExprKind::Range { .. }) => {
                self.temp_place(e)
            }
            ExprKind::Index { object, index } => {
                let idx = self.index_operand(object, index)?;
                self.index_place(object, idx, e, mutable)
            }
            ExprKind::Unary {
                op: UnaryOp::Deref,
                operand,
            } => {
                let p = self.expr_place(operand, mutable)?;
                Ok(p.project(ProjElem::Deref))
            }
            _ => self.temp_place(e),
        }
    }

    /// The MIR type `p` ends at: its local's type through each projection.
    fn place_type(&self, p: &Place) -> Ty {
        let mut t = self.b.local_ty(p.local);
        for elem in &p.projection {
            let tcx = self.tys().tcx();
            t = match (elem, tcx.kind(t)) {
                (ProjElem::Field(_, ft), _) => *ft,
                (ProjElem::Downcast(_), _) => t,
                (ProjElem::Deref, HK::Ref(inner) | HK::MutRef(inner)) => inner,
                (ProjElem::Index(_) | ProjElem::ConstIndex(_), HK::Array { elem, .. }) => elem,
                _ => t,
            };
        }
        t
    }

    /// The place of `object`, through any references, and its type there.
    fn deref_place(&mut self, object: &'a Expr, mutable: bool) -> R<(Place, Ty)> {
        let mut p = self.expr_place(object, mutable)?;
        let mut t = self.place_type(&p);
        loop {
            match self.tys().tcx().kind(t) {
                HK::Ref(inner) | HK::MutRef(inner) => {
                    p = p.project(ProjElem::Deref);
                    t = inner;
                }
                _ => return Ok((p, t)),
            }
        }
    }

    /// The index of `object[index]`: a `Map` key that is not `Copy` is
    /// borrowed, anything else is a value.
    fn index_operand(&mut self, object: &'a Expr, index: &'a Expr) -> R<Operand> {
        let mut t = self.expr_ty(object)?;
        while let HK::Ref(inner) | HK::MutRef(inner) = self.tys().tcx().kind(t) {
            t = inner;
        }
        if let HK::Intrinsic {
            kind: IntrinsicKind::Map,
            ..
        } = self.tys().tcx().kind(t)
        {
            let kt = self.expr_ty(index)?;
            if !self.is_copy(kt) {
                return self.lib_arg(index, true);
            }
        }
        self.expr_operand(index)
    }

    /// `object[idx]`: a library call yielding a reference, then its target.
    fn index_place(
        &mut self,
        object: &'a Expr,
        idx: Operand,
        e: &'a Expr,
        mutable: bool,
    ) -> R<Place> {
        let (base, bt) = self.deref_place(object, mutable)?;
        // A `Vec` or slice takes a `usize` position; a `Map` its key.
        let (elem, positional) = match self.tys().tcx().kind(bt) {
            HK::Intrinsic {
                kind: IntrinsicKind::Vec,
                args,
            } => (self.tys().tcx().list(args)[0], true),
            HK::Intrinsic {
                kind: IntrinsicKind::Map,
                args,
            } => (self.tys().tcx().list(args)[1], false),
            HK::Slice { elem, .. } => (elem, true),
            HK::Array { .. } => return Ok(self.array_index(base, idx)),
            _ => return self.unsupported(e.span, "indexing this type"),
        };
        let (usize_t, rt, et) = {
            let tcx = self.tys().tcx();
            (
                tcx.intern(HK::UInt(UIntSize::Usize)),
                tcx.reference(bt, mutable),
                tcx.reference(elem, mutable),
            )
        };
        let idx = if positional {
            self.cast_index(idx, usize_t)
        } else {
            idx
        };
        let borrow = if mutable {
            BorrowKind::Mut
        } else {
            BorrowKind::Shared
        };
        let r = self.temp(rt);
        self.assign(r, Rvalue::Ref(borrow, base));
        let out = self.temp(et);
        let ty_name = self.tys().display(bt);
        let method = if mutable { "index_mut" } else { "index" };
        self.call_native(
            &format!("{ty_name}.{method}"),
            vec![Operand::Move(Place::local(r)), idx],
            Place::local(out),
        );
        Ok(Place::local(out).project(ProjElem::Deref))
    }

    /// `base[idx]` on a fixed-size array: a bounds check that aborts, then
    /// the element place.
    fn array_index(&mut self, base: Place, idx: Operand) -> Place {
        let (usize_t, bool_t) = {
            let tcx = self.tys().tcx();
            (tcx.intern(HK::UInt(UIntSize::Usize)), tcx.intern(HK::Bool))
        };
        let idx = self.cast_index(idx, usize_t);
        let i = self.temp(usize_t);
        self.assign(i, Rvalue::Use(idx));
        let n = self.temp(usize_t);
        self.assign(n, Rvalue::Len(base.clone()));
        let c = self.temp(bool_t);
        self.assign(
            c,
            Rvalue::BinaryOp(
                BinOp::Lt,
                Operand::Copy(Place::local(i)),
                Operand::Copy(Place::local(n)),
            ),
        );
        let ok = self.b.new_block();
        let bad = self.b.new_block();
        self.goto_with(
            TerminatorKind::SwitchInt {
                discr: Operand::Copy(Place::local(c)),
                targets: SwitchTargets::if_else(ok, bad),
            },
            bad,
        );
        self.diverge(TerminatorKind::Abort {
            reason: AbortReason::BoundsCheck,
        });
        self.cur = ok;
        base.project(ProjElem::Index(i))
    }

    /// `s[a..b]` on a `String`: a new `String` holding those bytes, from
    /// the library's `String.index_range(ref s, a, b)`, which panics off a
    /// char boundary or out of range. An open start is 0, an open end the
    /// length, and `a..=b` ends at `b + 1`.
    fn string_range(
        &mut self,
        object: &'a Expr,
        start: Option<&'a Expr>,
        end: Option<&'a Expr>,
        inclusive: bool,
        dest: Place,
    ) -> R<()> {
        let i64_t = self.tys().tcx().intern(HK::Int(IntSize::I64));
        let p = self.expr_place(object, false)?;
        let (p, ct) = self.strip_refs(p);
        let rt = self.tys().tcx().reference(ct, false);
        let bound = |this: &mut Self, x: &'a Expr| -> R<Operand> {
            let (o, t) = this.scalar_operand(x)?;
            Ok(if t == i64_t {
                o
            } else {
                this.cast_index(o, i64_t)
            })
        };
        let lo = match start {
            Some(x) => bound(self, x)?,
            None => Operand::Const(Const {
                ty: i64_t,
                kind: ConstKind::Scalar(0),
            }),
        };
        let hi = match end {
            Some(x) => {
                let o = bound(self, x)?;
                if inclusive {
                    let one = Operand::Const(Const {
                        ty: i64_t,
                        kind: ConstKind::Scalar(1),
                    });
                    let h = self.temp(i64_t);
                    self.arith(BinOp::Add, o, one, i64_t, Place::local(h));
                    Operand::Copy(Place::local(h))
                } else {
                    o
                }
            }
            None => {
                let r = self.temp(rt);
                self.assign(r, Rvalue::Ref(BorrowKind::Shared, p.clone()));
                let n = self.temp(i64_t);
                self.call_native(
                    "String.len",
                    vec![Operand::Move(Place::local(r))],
                    Place::local(n),
                );
                Operand::Copy(Place::local(n))
            }
        };
        let r = self.temp(rt);
        self.assign(r, Rvalue::Ref(BorrowKind::Shared, p));
        self.call_native(
            "String.index_range",
            vec![Operand::Move(Place::local(r)), lo, hi],
            dest,
        );
        Ok(())
    }

    fn cast_index(&mut self, idx: Operand, usize_t: Ty) -> Operand {
        match idx {
            Operand::Const(Const {
                kind: ConstKind::Scalar(v),
                ..
            }) => Operand::Const(Const {
                ty: usize_t,
                kind: ConstKind::Scalar(v),
            }),
            other => {
                let t = self.temp(usize_t);
                self.assign(t, Rvalue::Cast(CastKind::IntToInt, other, usize_t));
                Operand::Copy(Place::local(t))
            }
        }
    }

    /// A value expression evaluated into a fresh temporary, as a place.
    fn temp_place(&mut self, e: &'a Expr) -> R<Place> {
        let t = self.expr_ty(e)?;
        let l = self.scoped_temp(t);
        self.expr_into(e, Place::local(l))?;
        Ok(Place::local(l))
    }

    /// Whether `t` is a `shared` handle, which a read copies with a count.
    fn is_handle(&self, t: Ty) -> bool {
        matches!(self.tys().tcx().kind(t), HK::Shared { .. })
    }

    /// An `Option` or tuple made only of `shared` handles and `Copy` parts,
    /// with at least one handle: a read copies it by counting each handle,
    /// as for a bare handle (core semantics §6.1).
    fn is_handle_aggregate(&self, t: Ty) -> bool {
        let parts = match self.tys().tcx().kind(t) {
            HK::Tuple(l) => self.tys().tcx().list(l),
            HK::Adt { def, args } if self.lcx.def_name(def) == "Option" => {
                self.tys().tcx().list(args)
            }
            _ => return false,
        };
        let counted = |p: Ty| self.is_handle(p) || self.is_handle_aggregate(p);
        parts.iter().all(|&p| counted(p) || self.is_copy(p)) && parts.iter().any(|&p| counted(p))
    }

    /// `dest = <a counted copy of place>`, for a handle or handle aggregate.
    fn count_copy(&mut self, place: Place, t: Ty, dest: Place) {
        if self.is_handle(t) {
            self.assign(dest, Rvalue::Retain(place));
        } else {
            let recv = self.ref_to(place, t);
            let name = format!("{}.clone", self.tys().display(t));
            self.call_native(&name, vec![recv], dest);
        }
    }

    /// Read a place as an operand: a copy for a `Copy` type, else a move.
    fn use_place(&self, p: Place, t: Ty) -> Operand {
        if self.is_copy(t) {
            Operand::Copy(p)
        } else {
            Operand::Move(p)
        }
    }

    // ── expressions ─────────────────────────────────────────────────

    fn expr_operand(&mut self, e: &'a Expr) -> R<Operand> {
        if let Some(c) = self.constant(e)? {
            return Ok(Operand::Const(c));
        }
        if self.is_place(e) {
            let p = self.expr_place(e, false)?;
            let t = self.expr_ty(e)?;
            if self.is_handle(t) || self.is_handle_aggregate(t) {
                let l = self.temp(t);
                self.count_copy(p, t, Place::local(l));
                return Ok(Operand::Move(Place::local(l)));
            }
            return Ok(self.use_place(p, t));
        }
        let t = self.expr_ty(e)?;
        let l = self.scoped_temp(t);
        self.expr_into(e, Place::local(l))?;
        Ok(self.use_place(Place::local(l), t))
    }

    fn is_local(&self, e: &Expr) -> bool {
        matches!(self.lcx.res.get(&e.id), Some(Res::Local(_)))
    }

    /// A literal as a constant, if `e` is one.
    fn constant(&mut self, e: &'a Expr) -> R<Option<Const>> {
        let scalar = |s: &mut Self, v: u128| -> R<Option<Const>> {
            let ty = s.expr_ty(e)?;
            Ok(Some(Const {
                ty,
                kind: ConstKind::Scalar(v),
            }))
        };
        match &e.kind {
            ExprKind::Integer(v, _) => scalar(self, *v as u128),
            ExprKind::Bool(b) => scalar(self, *b as u128),
            ExprKind::CharLit(c) => scalar(self, *c as u128),
            ExprKind::ByteLit(b) => scalar(self, *b as u128),
            ExprKind::Float(f, _) => {
                let ty = self.expr_ty(e)?;
                Ok(Some(Const {
                    ty,
                    kind: ConstKind::Float(f.to_bits()),
                }))
            }
            ExprKind::Tuple(es) if es.is_empty() => Ok(Some(match unit_const(self.unit()) {
                Operand::Const(c) => c,
                _ => unreachable!(),
            })),
            ExprKind::Unary {
                op: UnaryOp::Neg,
                operand,
            } => match &operand.kind {
                ExprKind::Integer(v, _) => scalar(self, (-*v) as u128),
                _ => Ok(None),
            },
            _ => Ok(None),
        }
    }

    fn static_str(&self, s: &str) -> Operand {
        let t = self.tys().tcx().intern(HK::StaticStr);
        Operand::Const(Const {
            ty: t,
            kind: ConstKind::Str(s.to_string()),
        })
    }

    /// Evaluate `e` and store its value in `dest`.
    fn expr_into(&mut self, e: &'a Expr, dest: Place) -> R<()> {
        if let Some(mut c) = self.constant(e)? {
            // An unsuffixed literal takes its destination's numeric type
            // (`let c: i8 = 100` records the literal as `i64`).
            let dt = self.place_type(&dest);
            let tcx = self.tys().tcx();
            let int = |t| matches!(tcx.kind(t), HK::Int(_) | HK::UInt(_));
            let float = |t| matches!(tcx.kind(t), HK::Float(_));
            if (int(c.ty) && int(dt)) || (float(c.ty) && float(dt)) {
                c.ty = dt;
            }
            self.assign(dest, Rvalue::Use(Operand::Const(c)));
            return Ok(());
        }
        match &e.kind {
            ExprKind::StringLit(s) | ExprKind::MultiStringLit(s) => {
                let op = self.static_str(s);
                self.call_native("String.from", vec![op], dest);
                Ok(())
            }
            ExprKind::Identifier(_) if !self.is_local(e) => self.path_value(e, dest),
            ExprKind::Closure { .. } => {
                let (op, _) = self.closure_value(e)?;
                self.assign(dest, Rvalue::Use(op));
                Ok(())
            }
            ExprKind::Path { .. } => self.path_value(e, dest),
            ExprKind::Index { object, index } if !self.is_place(e) => {
                let ExprKind::Range {
                    start,
                    end,
                    inclusive,
                } = &index.kind
                else {
                    unreachable!("a non-place index is a range")
                };
                let ot = self.expr_ty(object)?;
                if matches!(self.strip_ty(ot), HK::Str) {
                    let (start, end) = (start.as_deref(), end.as_deref());
                    return self.string_range(object, start, end, *inclusive, dest);
                }
                if *inclusive {
                    return self.unsupported(index.span, "an inclusive slice range");
                }
                let t = self.expr_ty(e)?;
                let s =
                    self.slice_view(object, Some((start.as_deref(), end.as_deref())), t, false)?;
                self.assign(dest, Rvalue::Use(Operand::Move(Place::local(s))));
                Ok(())
            }
            ExprKind::FieldAccess { .. } if self.int_limit_of(e).is_some() => {
                let (t, v) = self.int_limit_of(e).expect("checked above");
                self.assign(
                    dest,
                    Rvalue::Use(Operand::Const(Const {
                        ty: t,
                        kind: ConstKind::Scalar(v),
                    })),
                );
                Ok(())
            }
            _ if self.is_place(e) => {
                let dt = self.place_type(&dest);
                let mutable = matches!(self.tys().tcx().kind(dt), HK::MutRef(_));
                let mut p = self.expr_place(e, mutable)?;
                let mut t = self.place_type(&p);
                // A `ref T` flowing into a `T` slot is read through: a copy
                // for a Copy `T` (`fn f(x: ref i64) -> i64 { return x; }`),
                // else a move the borrow check refuses.
                while let HK::Ref(inner) | HK::MutRef(inner) = self.tys().tcx().kind(t) {
                    if t == dt || self.ref_depth(t) <= self.ref_depth(dt) {
                        break;
                    }
                    p = p.project(ProjElem::Deref);
                    t = inner;
                }
                // A place of `T` flowing into a `ref T` slot is borrowed.
                if let HK::Ref(inner) | HK::MutRef(inner) = self.tys().tcx().kind(dt) {
                    if inner == t {
                        let kind = if mutable {
                            BorrowKind::Mut
                        } else {
                            BorrowKind::Shared
                        };
                        self.assign(dest, Rvalue::Ref(kind, p));
                        return Ok(());
                    }
                }
                if self.is_handle(t) || self.is_handle_aggregate(t) {
                    // Copying a `shared` handle counts; the source keeps
                    // its own (core semantics §6.1).
                    self.count_copy(p, t, dest);
                    return Ok(());
                }
                let op = self.use_place(p, t);
                self.assign(dest, Rvalue::Use(op));
                Ok(())
            }
            ExprKind::Binary { op, left, right } => self.binary(op.clone(), left, right, dest),
            ExprKind::Unary { op, operand } => {
                let mop = match op {
                    UnaryOp::Neg => UnOp::Neg,
                    // `!` on a bool, `~` on an integer: both are `Not`.
                    UnaryOp::Not | UnaryOp::BitNot => UnOp::Not,
                    // `ref v[i]`: a shared borrow of a place.
                    UnaryOp::Ref if self.is_place(operand) => {
                        let p = self.expr_place(operand, false)?;
                        self.assign(dest, Rvalue::Ref(BorrowKind::Shared, p));
                        return Ok(());
                    }
                    _ => return self.unsupported(e.span, "this unary operator"),
                };
                let o = self.expr_operand(operand)?;
                self.assign(dest, Rvalue::UnaryOp(mop, o));
                Ok(())
            }
            ExprKind::Tuple(es) => {
                let mut ops = Vec::new();
                for x in es {
                    ops.push(self.expr_operand(x)?);
                }
                self.assign(dest, Rvalue::Aggregate(AggregateKind::Tuple, ops));
                Ok(())
            }
            ExprKind::ArrayLiteral(es) => self.array_literal(e, es, dest),
            ExprKind::PrefixCollectionLiteral { type_name, items }
                if type_name == "Vec" || type_name == "Array" =>
            {
                self.array_literal(e, items, dest)
            }
            ExprKind::StructLiteral {
                fields,
                spread: None,
                ..
            } => {
                let t = self.expr_ty(e)?;
                // `E.A { v: .. }` builds a variant with named fields.
                let variant = match self.lcx.res.get(&e.id) {
                    Some(Res::Def(d)) => self.lcx.variant(*d).map(|(_, i)| i),
                    _ => None,
                };
                // Fields are evaluated in source order and stored in
                // declaration order.
                let mut vals: Vec<(u32, Operand)> = Vec::new();
                for fi in fields {
                    let Some((idx, _)) = self.field_of(t, variant, &fi.name) else {
                        return self.unsupported(fi.span, "this field");
                    };
                    let op = self.expr_operand(&fi.value)?;
                    vals.push((idx, op));
                }
                vals.sort_by_key(|(i, _)| *i);
                let ops = vals.into_iter().map(|(_, o)| o).collect();
                let kind = self.adt_aggregate(t, variant.unwrap_or(0));
                self.assign(dest, Rvalue::Aggregate(kind, ops));
                Ok(())
            }
            ExprKind::Block(b) => self.block_into(b, dest),
            ExprKind::If {
                condition,
                then_block,
                else_branch,
            } => {
                let c = self.expr_operand(condition)?;
                let then_bb = self.b.new_block();
                let else_bb = self.b.new_block();
                let join = self.b.new_block();
                self.goto_with(
                    TerminatorKind::SwitchInt {
                        discr: c,
                        targets: SwitchTargets::if_else(then_bb, else_bb),
                    },
                    then_bb,
                );
                self.block_into(then_block, dest.clone())?;
                self.goto(join);
                self.cur = else_bb;
                match else_branch {
                    Some(els) => {
                        self.push_scope();
                        self.expr_into(els, dest)?;
                        self.pop_scope()?;
                    }
                    None => self.assign(dest, Rvalue::Use(unit_const(self.unit()))),
                }
                self.goto(join);
                self.cur = join;
                Ok(())
            }
            ExprKind::Match { scrutinee, arms } => self.lower_match(e, scrutinee, arms, dest),
            ExprKind::IfLet {
                pattern,
                value,
                then_block,
                else_branch,
            } => self.if_let(pattern, value, then_block, else_branch.as_deref(), dest),
            ExprKind::WhileLet {
                label,
                pattern,
                value,
                body,
                ..
            } => {
                let head = self.b.new_block();
                let exit = self.b.new_block();
                self.goto(head);
                self.cur = head;
                self.loops.push(LoopCx {
                    label: label.clone(),
                    break_bb: exit,
                    continue_bb: head,
                    depth: self.scopes.len(),
                    dest: None,
                });
                let r = self.while_let_round(pattern, value, body, head, exit);
                self.loops.pop();
                r?;
                self.cur = exit;
                self.assign(dest, Rvalue::Use(unit_const(self.unit())));
                Ok(())
            }
            ExprKind::LabeledBlock { label, body, .. } => {
                let exit = self.b.new_block();
                self.loops.push(LoopCx {
                    label: Some(label.clone()),
                    break_bb: exit,
                    continue_bb: exit,
                    depth: self.scopes.len(),
                    dest: Some(dest.clone()),
                });
                let r = self.block_into(body, dest);
                self.loops.pop();
                r?;
                self.goto(exit);
                self.cur = exit;
                Ok(())
            }
            ExprKind::Unsafe(b) => self.block_into(b, dest),
            // The branches of a `par` block run one after another, in source
            // order: one of the schedules the block allows (the sequential
            // scheduler's).
            ExprKind::Par(b) => self.block_into(b, dest),
            ExprKind::RepeatLiteral { value, count, .. } => {
                self.repeat_literal(e, value, count, dest)
            }
            ExprKind::Cast { expr: inner, .. } => {
                let from = self.expr_ty(inner)?;
                let to = self.expr_ty(e)?;
                let o = self.expr_operand(inner)?;
                let kind = {
                    let tcx = self.tys().tcx();
                    let int = |t| matches!(tcx.kind(t), HK::Int(_) | HK::UInt(_));
                    let float = |t| matches!(tcx.kind(t), HK::Float(_));
                    match (int(from) || from == to, float(from), int(to), float(to)) {
                        _ if from == to => None,
                        (true, _, true, _) => Some(CastKind::IntToInt),
                        (true, _, _, true) => Some(CastKind::IntToFloat),
                        (_, true, true, _) => Some(CastKind::FloatToInt),
                        (_, true, _, true) => Some(CastKind::FloatToFloat),
                        _ => return self.unsupported(e.span, "this cast"),
                    }
                };
                match kind {
                    None => self.assign(dest, Rvalue::Use(o)),
                    Some(k) => self.assign(dest, Rvalue::Cast(k, o, to)),
                }
                Ok(())
            }
            ExprKind::While {
                label,
                condition,
                body,
                ..
            } => {
                let head = self.b.new_block();
                let body_bb = self.b.new_block();
                let exit = self.b.new_block();
                self.goto(head);
                self.cur = head;
                self.push_scope();
                let c = self.expr_operand(condition)?;
                self.pop_scope()?;
                self.goto_with(
                    TerminatorKind::SwitchInt {
                        discr: c,
                        targets: SwitchTargets::if_else(body_bb, exit),
                    },
                    body_bb,
                );
                self.loop_body(label, body, head, exit, None)?;
                self.cur = exit;
                self.assign(dest, Rvalue::Use(unit_const(self.unit())));
                Ok(())
            }
            ExprKind::Loop { label, body, .. } => {
                let head = self.b.new_block();
                let exit = self.b.new_block();
                self.goto(head);
                self.cur = head;
                self.loop_body(label, body, head, exit, Some(dest))?;
                self.cur = exit;
                Ok(())
            }
            ExprKind::For {
                label,
                pattern,
                iterable,
                body,
                ..
            } => self.for_range(e, label, pattern, iterable, body, dest),
            ExprKind::Return(value) => {
                let ret = Place::local(Local::RETURN_PLACE);
                match value {
                    Some(v) => self.expr_into(v, ret)?,
                    None => self.assign(ret, Rvalue::Use(unit_const(self.unit()))),
                }
                self.return_exit()
            }
            ExprKind::Break { label, value } => {
                let Some(i) = self.find_loop(label) else {
                    return self.unsupported(e.span, "this `break`");
                };
                if let Some(v) = value {
                    match self.loops[i].dest.clone() {
                        Some(d) => self.expr_into(v, d)?,
                        None => return self.unsupported(e.span, "`break` with a value here"),
                    }
                } else if let Some(d) = self.loops[i].dest.clone() {
                    self.assign(d, Rvalue::Use(unit_const(self.unit())));
                }
                let depth = self.loops[i].depth;
                self.exit_to(depth, false)?;
                let target = self.loops[i].break_bb;
                self.diverge(TerminatorKind::Goto { target });
                Ok(())
            }
            ExprKind::Continue { label, .. } => {
                let Some(i) = self.find_loop(label) else {
                    return self.unsupported(e.span, "this `continue`");
                };
                let depth = self.loops[i].depth;
                self.exit_to(depth, false)?;
                let target = self.loops[i].continue_bb;
                self.diverge(TerminatorKind::Goto { target });
                Ok(())
            }
            ExprKind::Call { callee, args } => self.call(e, callee, args, dest),
            ExprKind::MethodCall {
                object,
                method,
                args,
                ..
            } => self.method_call(e, object, method, args, dest),
            ExprKind::Question(inner) => self.question(e, inner, dest),
            ExprKind::InterpolatedStringLit(_) => {
                // An f-string as a value: the library's `format`, which takes
                // the parts the way `print` does.
                let mut ops = Vec::new();
                self.print_operands(e, &mut ops)?;
                self.call_native("format", ops, dest);
                Ok(())
            }
            other => {
                let dbg = format!("{other:?}");
                let kind = dbg.split(['(', ' ', '{']).next().unwrap_or("").to_string();
                self.unsupported(e.span, &format!("the expression kind `{kind}`"))
            }
        }
    }

    /// A path naming a value: a unit variant (`None`, `E.A`) or a constant.
    fn path_value(&mut self, e: &'a Expr, dest: Place) -> R<()> {
        match self.lcx.res.get(&e.id) {
            Some(Res::Def(d)) => {
                let d = *d;
                match self.lcx.variant(d) {
                    Some((_, idx)) => {
                        let t = self.expr_ty(e)?;
                        let kind = self.adt_aggregate(t, idx);
                        self.assign(dest, Rvalue::Aggregate(kind, Vec::new()));
                        Ok(())
                    }
                    None if self.lcx.fns.contains_key(&d) => {
                        let (op, _) = self.fn_item_value(e.span, d)?;
                        self.assign(dest, Rvalue::Use(op));
                        Ok(())
                    }
                    None => match self.lcx.consts.get(&d) {
                        // A constant's value is computed where it is used.
                        Some(c) => self.const_value(e, &c.value, dest),
                        None => self.unsupported(e.span, "this path"),
                    },
                }
            }
            Some(Res::Builtin(path)) => {
                let path = path.clone();
                let t = self.expr_ty(e)?;
                let Some(v) = int_limit(self.tys().tcx().kind(t), &path) else {
                    return self.unsupported(e.span, "this path");
                };
                self.assign(
                    dest,
                    Rvalue::Use(Operand::Const(Const {
                        ty: t,
                        kind: ConstKind::Scalar(v),
                    })),
                );
                Ok(())
            }
            _ => self.unsupported(e.span, "this name"),
        }
    }

    /// A use `e` of a constant whose value is `value`, at the type the use
    /// was checked at (an unsuffixed literal takes the declared type).
    fn const_value(&mut self, e: &'a Expr, value: &'a Expr, dest: Place) -> R<()> {
        let t = self.expr_ty(e)?;
        let vt = self.expr_ty(value)?;
        if t == vt {
            return self.expr_into(value, dest);
        }
        let op = self.expr_operand(value)?;
        let op = match op {
            Operand::Const(Const {
                kind: ConstKind::Scalar(v),
                ..
            }) => Operand::Const(Const {
                ty: t,
                kind: ConstKind::Scalar(v),
            }),
            Operand::Const(Const {
                kind: ConstKind::Float(v),
                ..
            }) => Operand::Const(Const {
                ty: t,
                kind: ConstKind::Float(v),
            }),
            _ => return self.unsupported(e.span, "a constant used at another type"),
        };
        self.assign(dest, Rvalue::Use(op));
        Ok(())
    }

    fn adt_aggregate(&self, t: Ty, variant: u32) -> AggregateKind {
        match self.tys().tcx().kind(t) {
            HK::Shared { .. } => AggregateKind::Shared {
                ty: t,
                variant: VariantIdx(variant),
            },
            _ => AggregateKind::Adt {
                ty: t,
                variant: VariantIdx(variant),
            },
        }
    }

    fn array_literal(&mut self, e: &'a Expr, es: &'a [Expr], dest: Place) -> R<()> {
        let t = self.expr_ty(e)?;
        let (elem, vec) = match self.tys().tcx().kind(t) {
            HK::Intrinsic {
                kind: IntrinsicKind::Vec,
                args,
            } => (self.tys().tcx().list(args)[0], true),
            HK::Array { elem, .. } => (elem, false),
            _ => return self.unsupported(e.span, "this collection literal"),
        };
        let mut ops = Vec::new();
        for x in es {
            ops.push(self.expr_operand(x)?);
        }
        let agg = Rvalue::Aggregate(AggregateKind::Array(elem), ops);
        if !vec {
            self.assign(dest, agg);
            return Ok(());
        }
        let at = self.tys().tcx().intern(HK::Array {
            elem,
            len: crate::ty::ArrayLen::Known(es.len() as u64),
        });
        let arr = self.temp(at);
        self.assign(arr, agg);
        let name = format!("{}.from_array", self.tys().display(t));
        self.call_native(&name, vec![Operand::Move(Place::local(arr))], dest);
        Ok(())
    }

    /// `[v; n]`: an array of `n` copies of a `Copy` value, or a `Vec`
    /// built by the library's `Vec.filled(n, v)`.
    fn repeat_literal(
        &mut self,
        e: &'a Expr,
        value: &'a Expr,
        count: &'a Expr,
        dest: Place,
    ) -> R<()> {
        let t = self.expr_ty(e)?;
        match self.tys().tcx().kind(t) {
            HK::Array {
                elem,
                len: crate::ty::ArrayLen::Known(n),
            } if self.is_copy(elem) => {
                let v = match self.expr_operand(value)? {
                    // An unsuffixed literal takes the element type.
                    Operand::Const(Const {
                        kind: ConstKind::Scalar(x),
                        ..
                    }) => Operand::Const(Const {
                        ty: elem,
                        kind: ConstKind::Scalar(x),
                    }),
                    v => v,
                };
                let tmp = self.temp(elem);
                self.assign(tmp, Rvalue::Use(v));
                let ops = (0..n).map(|_| Operand::Copy(Place::local(tmp))).collect();
                self.assign(dest, Rvalue::Aggregate(AggregateKind::Array(elem), ops));
                Ok(())
            }
            HK::Intrinsic {
                kind: IntrinsicKind::Vec,
                ..
            } => {
                let i64_t = self.tys().tcx().intern(HK::Int(IntSize::I64));
                let n = self.expr_operand(count)?;
                let ct = self.expr_ty(count)?;
                let n = if ct == i64_t {
                    n
                } else {
                    let c = self.temp(i64_t);
                    self.assign(c, Rvalue::Cast(CastKind::IntToInt, n, i64_t));
                    Operand::Copy(Place::local(c))
                };
                let v = self.expr_operand(value)?;
                let name = format!("{}.filled", self.tys().display(t));
                self.call_native(&name, vec![n, v], dest);
                Ok(())
            }
            _ => self.unsupported(e.span, "this repeat literal"),
        }
    }

    fn binary(&mut self, op: AstBinOp, left: &'a Expr, right: &'a Expr, dest: Place) -> R<()> {
        let bin = match op {
            AstBinOp::Add => BinOp::Add,
            AstBinOp::Sub => BinOp::Sub,
            AstBinOp::Mul => BinOp::Mul,
            AstBinOp::Div => BinOp::Div,
            AstBinOp::Mod => BinOp::Rem,
            AstBinOp::Eq => BinOp::Eq,
            AstBinOp::NotEq => BinOp::Ne,
            AstBinOp::Lt => BinOp::Lt,
            AstBinOp::LtEq => BinOp::Le,
            AstBinOp::Gt => BinOp::Gt,
            AstBinOp::GtEq => BinOp::Ge,
            AstBinOp::BitAnd => BinOp::BitAnd,
            AstBinOp::BitOr => BinOp::BitOr,
            AstBinOp::BitXor => BinOp::BitXor,
            AstBinOp::Shl => BinOp::Shl,
            AstBinOp::Shr => BinOp::Shr,
            AstBinOp::And | AstBinOp::Or => {
                // Short circuit: the right side runs only when it decides.
                let l = self.expr_operand(left)?;
                let rhs_bb = self.b.new_block();
                let short_bb = self.b.new_block();
                let join = self.b.new_block();
                let (t, f) = if op == AstBinOp::And {
                    (rhs_bb, short_bb)
                } else {
                    (short_bb, rhs_bb)
                };
                self.goto_with(
                    TerminatorKind::SwitchInt {
                        discr: l,
                        targets: SwitchTargets::if_else(t, f),
                    },
                    rhs_bb,
                );
                self.push_scope();
                self.expr_into(right, dest.clone())?;
                self.pop_scope()?;
                self.goto(join);
                self.cur = short_bb;
                let bool_t = self.tys().bool();
                self.assign(
                    dest,
                    Rvalue::Use(Operand::Const(Const {
                        ty: bool_t,
                        kind: ConstKind::Scalar((op == AstBinOp::Or) as u128),
                    })),
                );
                self.goto(join);
                self.cur = join;
                return Ok(());
            }
            AstBinOp::Range | AstBinOp::RangeInclusive => {
                return self.unsupported(left.span, "a range value");
            }
        };
        let lt0 = self.expr_ty(left)?;
        let lbase = match self.tys().tcx().kind(lt0) {
            HK::Ref(t) | HK::MutRef(t) => t,
            _ => lt0,
        };
        if matches!(self.tys().tcx().kind(lbase), HK::Str) {
            return self.string_op(left, bin, right, dest);
        }
        let scalar = |k: HK| {
            matches!(
                k,
                HK::Int(_) | HK::UInt(_) | HK::Float(_) | HK::Bool | HK::Char
            )
        };
        if !scalar(self.tys().tcx().kind(lbase)) && matches!(bin, BinOp::Eq | BinOp::Ne) {
            return self.eq_exprs(left, right, bin == BinOp::Ne, dest);
        }
        let (l, lt) = self.scalar_operand(left)?;
        if !self.is_copy(lt) {
            return self.unsupported(left.span, "an operator on a non-scalar");
        }
        let (r, _) = self.scalar_operand(right)?;
        self.arith(bin, l, r, lt, dest);
        Ok(())
    }

    // ── equality ────────────────────────────────────────────────────

    /// `left == right` (or `!=`) on values that are not scalars: the type's
    /// own `eq` when it has one, field by field when it derives it.
    fn eq_exprs(&mut self, left: &'a Expr, right: &'a Expr, negate: bool, dest: Place) -> R<()> {
        let lp = self.expr_place(left, false)?;
        let rp = self.expr_place(right, false)?;
        let (lp, lt) = self.strip_refs(lp);
        let (rp, _) = self.strip_refs(rp);
        if !negate {
            return self.eq_places(left.span, lp, rp, lt, dest);
        }
        let bool_t = self.tys().bool();
        let t = self.temp(bool_t);
        self.eq_places(left.span, lp, rp, lt, Place::local(t))?;
        self.assign(
            dest,
            Rvalue::UnaryOp(UnOp::Not, Operand::Move(Place::local(t))),
        );
        Ok(())
    }

    fn strip_refs(&self, mut p: Place) -> (Place, Ty) {
        let mut t = self.place_type(&p);
        while let HK::Ref(inner) | HK::MutRef(inner) = self.tys().tcx().kind(t) {
            p = p.project(ProjElem::Deref);
            t = inner;
        }
        (p, t)
    }

    fn ref_to(&mut self, p: Place, t: Ty) -> Operand {
        let rt = self.tys().tcx().reference(t, false);
        let r = self.temp(rt);
        self.assign(r, Rvalue::Ref(BorrowKind::Shared, p));
        Operand::Move(Place::local(r))
    }

    /// `dest = l == r` for two places of type `t`.
    fn eq_places(&mut self, span: Span, l: Place, r: Place, t: Ty, dest: Place) -> R<()> {
        let kind = self.tys().tcx().kind(t);
        match kind {
            HK::Int(_) | HK::UInt(_) | HK::Float(_) | HK::Bool | HK::Char => {
                self.assign(
                    dest,
                    Rvalue::BinaryOp(BinOp::Eq, Operand::Copy(l), Operand::Copy(r)),
                );
                Ok(())
            }
            HK::Ref(inner) | HK::MutRef(inner) => self.eq_places(
                span,
                l.project(ProjElem::Deref),
                r.project(ProjElem::Deref),
                inner,
                dest,
            ),
            HK::Str => {
                let lo = self.ref_to(l, t);
                let ro = self.ref_to(r, t);
                self.call_native("String.eq", vec![lo, ro], dest);
                Ok(())
            }
            HK::Tuple(list) => {
                let tys = self.tys().tcx().list(list);
                let parts = tys
                    .iter()
                    .enumerate()
                    .map(|(i, &ft)| (l.field(i as u32, ft), r.field(i as u32, ft), ft))
                    .collect();
                self.all_eq(span, parts, dest)
            }
            HK::Array {
                elem,
                len: crate::ty::ArrayLen::Known(n),
            } => {
                let parts = (0..n)
                    .map(|i| {
                        (
                            l.project(ProjElem::ConstIndex(i)),
                            r.project(ProjElem::ConstIndex(i)),
                            elem,
                        )
                    })
                    .collect();
                self.all_eq(span, parts, dest)
            }
            HK::Adt { .. } => {
                if let Some((d, args)) = self.user_eq(t) {
                    let lo = self.ref_to(l, t);
                    let ro = self.ref_to(r, t);
                    let name = self.lcx.instance(d, args.clone());
                    let func = self.fn_operand(&name, d, args);
                    let next = self.b.new_block();
                    self.goto_with(
                        TerminatorKind::Call {
                            func,
                            args: vec![lo, ro],
                            destination: dest,
                            target: Some(next),
                            unwind: UnwindAction::Abort,
                        },
                        next,
                    );
                    return Ok(());
                }
                let (adt, _) = self.tys().tcx().adt_of(t).expect("an ADT");
                if !adt.is_enum {
                    let n = adt.variants.first().map_or(0, |v| v.fields.len());
                    let mut parts = Vec::new();
                    for i in 0..n as u32 {
                        let Some(ft) = self.tys().tcx().field_ty(t, None, i) else {
                            return self.unsupported(span, "comparing this type");
                        };
                        parts.push((l.clone().field(i, ft), r.clone().field(i, ft), ft));
                    }
                    return self.all_eq(span, parts, dest);
                }
                self.enum_eq(span, l, r, t, &adt, dest)
            }
            _ => self.unsupported(span, "comparing this type"),
        }
    }

    /// Two enum values are equal when their variants are, and then their
    /// payloads.
    fn enum_eq(
        &mut self,
        span: Span,
        l: Place,
        r: Place,
        t: Ty,
        adt: &crate::ty::AdtDef,
        dest: Place,
    ) -> R<()> {
        let (isize_t, bool_t) = {
            let tcx = self.tys().tcx();
            (tcx.intern(HK::Int(IntSize::I64)), tcx.intern(HK::Bool))
        };
        let dl = self.temp(isize_t);
        self.assign(dl, Rvalue::Discriminant(l.clone()));
        let dr = self.temp(isize_t);
        self.assign(dr, Rvalue::Discriminant(r.clone()));
        let same = self.temp(bool_t);
        self.assign(
            same,
            Rvalue::BinaryOp(
                BinOp::Eq,
                Operand::Copy(Place::local(dl)),
                Operand::Copy(Place::local(dr)),
            ),
        );
        let differ = self.b.new_block();
        let switch = self.b.new_block();
        let join = self.b.new_block();
        self.goto_with(
            TerminatorKind::SwitchInt {
                discr: Operand::Copy(Place::local(same)),
                targets: SwitchTargets::if_else(switch, differ),
            },
            differ,
        );
        self.assign(dest.clone(), Rvalue::Use(bool_const(bool_t, false)));
        self.goto(join);
        self.cur = switch;
        let arms: Vec<BasicBlock> = adt.variants.iter().map(|_| self.b.new_block()).collect();
        let unreachable = self.b.new_block();
        self.goto_with(
            TerminatorKind::SwitchInt {
                discr: Operand::Copy(Place::local(dl)),
                targets: SwitchTargets {
                    values: arms
                        .iter()
                        .enumerate()
                        .map(|(i, &b)| (i as u128, b))
                        .collect(),
                    otherwise: unreachable,
                },
            },
            unreachable,
        );
        self.diverge(TerminatorKind::Unreachable);
        for (vi, v) in adt.variants.iter().enumerate() {
            self.cur = arms[vi];
            let vi = vi as u32;
            let mut parts = Vec::new();
            for fi in 0..v.fields.len() as u32 {
                let Some(ft) = self.tys().tcx().field_ty(t, Some(vi), fi) else {
                    return self.unsupported(span, "comparing this type");
                };
                let down = |p: &Place| p.project(ProjElem::Downcast(VariantIdx(vi))).field(fi, ft);
                parts.push((down(&l), down(&r), ft));
            }
            self.all_eq(span, parts, dest.clone())?;
            self.goto(join);
        }
        self.cur = join;
        Ok(())
    }

    /// `dest = true` when every pair is equal, stopping at the first that
    /// is not.
    fn all_eq(&mut self, span: Span, parts: Vec<(Place, Place, Ty)>, dest: Place) -> R<()> {
        let bool_t = self.tys().bool();
        let differ = self.b.new_block();
        let join = self.b.new_block();
        for (lp, rp, ft) in parts {
            let c = self.temp(bool_t);
            self.eq_places(span, lp, rp, ft, Place::local(c))?;
            let next = self.b.new_block();
            self.goto_with(
                TerminatorKind::SwitchInt {
                    discr: Operand::Copy(Place::local(c)),
                    targets: SwitchTargets::if_else(next, differ),
                },
                next,
            );
        }
        self.assign(dest.clone(), Rvalue::Use(bool_const(bool_t, true)));
        self.goto(join);
        self.cur = differ;
        self.assign(dest, Rvalue::Use(bool_const(bool_t, false)));
        self.goto(join);
        self.cur = join;
        Ok(())
    }

    /// A user `impl PartialEq` method `eq` for `t`, with its instance
    /// arguments.
    fn user_eq(&self, t: Ty) -> Option<(DefId, Vec<Ty>)> {
        self.user_impl_method(t, "PartialEq", "eq")
    }

    /// Method `method` of the user's `impl <tr> for t`, with the impl's
    /// generic arguments when it has any.
    fn user_impl_method(&self, t: Ty, tr: &str, method: &str) -> Option<(DefId, Vec<Ty>)> {
        let (adt, args) = self.tys().tcx().adt_of(t)?;
        let mut path = self.lcx.defs.table.get(adt.def).path.segments.clone();
        path.push(format!("impl {tr}#0"));
        path.push(method.to_string());
        let d = self.lcx.defs.table.lookup(&DefPath::new(path))?;
        let item = self.lcx.fns.get(&d)?;
        Some((
            d,
            if item.impl_params == 0 {
                Vec::new()
            } else {
                args
            },
        ))
    }

    // ── loops ───────────────────────────────────────────────────────

    fn find_loop(&self, label: &Option<String>) -> Option<usize> {
        match label {
            None => self.loops.len().checked_sub(1),
            Some(l) => self
                .loops
                .iter()
                .rposition(|c| c.label.as_deref() == Some(l.as_str())),
        }
    }

    fn loop_body(
        &mut self,
        label: &Option<String>,
        body: &'a Block,
        continue_bb: BasicBlock,
        exit: BasicBlock,
        dest: Option<Place>,
    ) -> R<()> {
        self.loops.push(LoopCx {
            label: label.clone(),
            break_bb: exit,
            continue_bb,
            depth: self.scopes.len(),
            dest,
        });
        let t = self.unit();
        let tmp = self.temp(t);
        let r = self.block_into(body, Place::local(tmp));
        self.loops.pop();
        r?;
        self.goto(continue_bb);
        Ok(())
    }

    /// `for i in a..b { body }` over integers.
    fn for_range(
        &mut self,
        e: &'a Expr,
        label: &Option<String>,
        pattern: &'a Pattern,
        iterable: &'a Expr,
        body: &'a Block,
        dest: Place,
    ) -> R<()> {
        let (left, right, inclusive): (&'a Expr, &'a Expr, bool) = match &iterable.kind {
            ExprKind::Binary {
                op: op @ (AstBinOp::Range | AstBinOp::RangeInclusive),
                left,
                right,
            } => (left, right, *op == AstBinOp::RangeInclusive),
            ExprKind::Range {
                start: Some(left),
                end: Some(right),
                inclusive,
            } => (left, right, *inclusive),
            _ => return self.for_collection(e, label, pattern, iterable, body, dest),
        };
        let name = match &pattern.kind {
            PatternKind::Binding(name) => Some(name),
            PatternKind::Wildcard => None,
            _ => return self.unsupported(pattern.span, "this loop pattern"),
        };
        let (lt, rt) = (self.expr_ty(left)?, self.expr_ty(right)?);
        self.push_scope();
        let start = self.expr_operand(left)?;
        let end = self.expr_operand(right)?;
        // An unsuffixed literal bound takes the other bound's type (the
        // checker records `-1000i32..1000001`'s end as an `i64`).
        let scalar = |o: &Operand| match o {
            Operand::Const(Const {
                kind: ConstKind::Scalar(v),
                ..
            }) => Some(*v),
            _ => None,
        };
        let t = if scalar(&start).is_some() && scalar(&end).is_none() {
            rt
        } else {
            lt
        };
        let retype = |o: Operand| match scalar(&o) {
            Some(v) => Operand::Const(Const {
                ty: t,
                kind: ConstKind::Scalar(v),
            }),
            None => o,
        };
        let (start, end) = (retype(start), retype(end));
        let end_t = self.temp(t);
        self.assign(end_t, Rvalue::Use(end));
        let i = self.temp(t);
        self.assign(i, Rvalue::Use(start));
        let var = name.map(|n| self.user_local(n, t, pattern.id));
        let head = self.b.new_block();
        let body_bb = self.b.new_block();
        let step = self.b.new_block();
        let exit = self.b.new_block();
        self.goto(head);
        self.cur = head;
        let bool_t = self.tys().bool();
        let c = self.temp(bool_t);
        let cmp = if inclusive { BinOp::Le } else { BinOp::Lt };
        self.assign(
            c,
            Rvalue::BinaryOp(
                cmp,
                Operand::Copy(Place::local(i)),
                Operand::Copy(Place::local(end_t)),
            ),
        );
        self.goto_with(
            TerminatorKind::SwitchInt {
                discr: Operand::Copy(Place::local(c)),
                targets: SwitchTargets::if_else(body_bb, exit),
            },
            body_bb,
        );
        // The body sees a copy of the counter, so assigning to the loop
        // variable inside it cannot change the iteration.
        if let Some(v) = var {
            self.assign(v, Rvalue::Use(Operand::Copy(Place::local(i))));
        }
        self.loop_body(label, body, step, exit, None)?;
        self.cur = step;
        let one = Operand::Const(Const {
            ty: t,
            kind: ConstKind::Scalar(1),
        });
        self.assign(
            i,
            Rvalue::BinaryOp(BinOp::Add, Operand::Copy(Place::local(i)), one),
        );
        self.goto(head);
        self.cur = exit;
        self.pop_scope()?;
        self.assign(dest, Rvalue::Use(unit_const(self.unit())));
        Ok(())
    }

    /// `for x in v` over a `Vec`. A borrowed collection (`v`, `v.iter()`,
    /// `v.iter_mut()`, or a `ref Vec`) yields references to its elements by
    /// index. `v.into_iter()` moves the collection into the loop, which
    /// gives up its elements front to front; what a `break` leaves behind
    /// drops with it at the loop's end.
    fn for_collection(
        &mut self,
        e: &'a Expr,
        label: &Option<String>,
        pattern: &'a Pattern,
        iterable: &'a Expr,
        body: &'a Block,
        dest: Place,
    ) -> R<()> {
        if let Some(text) = self.chars_of(iterable)? {
            return self.for_chars(label, pattern, body, text, dest);
        }
        #[derive(PartialEq, Clone, Copy)]
        enum Mode {
            Ref,
            Mut,
            Owned,
        }
        let (src, mut mode) = match &iterable.kind {
            ExprKind::MethodCall {
                object,
                method,
                args,
                ..
            } if args.is_empty()
                && matches!(method.as_str(), "iter" | "iter_mut" | "into_iter") =>
            {
                let m = match method.as_str() {
                    "iter" => Mode::Ref,
                    "iter_mut" => Mode::Mut,
                    _ => Mode::Owned,
                };
                (&**object, m)
            }
            // A bare `for x in c` borrows `c` (core semantics §5,
            // "`for x in c` borrows `c` through `Iterable`"); only
            // `c.into_iter()` moves it.
            _ => (iterable, Mode::Ref),
        };
        let st = self.expr_ty(src)?;
        let (coll, src_is_ref) = match self.tys().tcx().kind(st) {
            HK::Ref(t) => {
                if mode == Mode::Owned {
                    mode = Mode::Ref;
                }
                (t, true)
            }
            HK::MutRef(t) => {
                if mode == Mode::Owned {
                    mode = Mode::Mut;
                }
                (t, true)
            }
            _ => (st, false),
        };
        let elem = match self.tys().tcx().kind(coll) {
            HK::Intrinsic {
                kind: IntrinsicKind::Vec,
                args,
            } => self.tys().tcx().list(args)[0],
            // The other library collections are read by position, through
            // the same `len` and `index`, in the collection's own order. A
            // map's element is its `(key, value)` entry.
            HK::Intrinsic {
                kind:
                    kind @ (IntrinsicKind::Set
                    | IntrinsicKind::SortedSet
                    | IntrinsicKind::VecDeque
                    | IntrinsicKind::Map
                    | IntrinsicKind::SortedMap),
                args,
            } if mode == Mode::Ref => {
                let tcx = self.tys().tcx();
                let args = tcx.list(args);
                match kind {
                    IntrinsicKind::Map | IntrinsicKind::SortedMap => {
                        tcx.intern(HK::Tuple(tcx.intern_list(&args)))
                    }
                    _ => args[0],
                }
            }
            // A slice is a view: its loop reads the elements in place.
            HK::Slice { elem, mutable } => {
                if mode == Mode::Owned {
                    mode = if mutable { Mode::Mut } else { Mode::Ref };
                }
                elem
            }
            HK::Array {
                elem,
                len: crate::ty::ArrayLen::Known(n),
            } => {
                let how = match mode {
                    Mode::Owned | Mode::Ref if self.is_copy(elem) => None,
                    Mode::Owned => {
                        return self.unsupported(e.span, "a `for` loop that moves out of an array")
                    }
                    Mode::Ref => Some(false),
                    Mode::Mut => Some(true),
                };
                return self.for_array(label, pattern, body, src, src_is_ref, how, (elem, n), dest);
            }
            _ => return self.unsupported(e.span, "a `for` loop over a collection"),
        };
        if src_is_ref && mode == Mode::Owned {
            return self.unsupported(e.span, "a `for` loop that moves out of a borrow");
        }
        let coll_name = self.tys().display(coll);
        // A `Vec` is read by `index` with a `usize`; the other collections
        // by `entry_at` with an `i64` (`Map.index` is the key lookup).
        let by_entry = self.by_entry(coll);
        let (usize_t, bool_t, ref_coll, mut_coll, elem_ref) = {
            let tcx = self.tys().tcx();
            (
                if by_entry {
                    tcx.intern(HK::Int(IntSize::I64))
                } else {
                    tcx.intern(HK::UInt(UIntSize::Usize))
                },
                tcx.intern(HK::Bool),
                tcx.reference(coll, false),
                tcx.reference(coll, true),
                tcx.reference(elem, mode == Mode::Mut),
            )
        };
        let usize_const = |v: u128| {
            Operand::Const(Const {
                ty: usize_t,
                kind: ConstKind::Scalar(v),
            })
        };
        // The loop's scope holds the collection (or the borrow of it).
        self.push_scope();
        let handle = match mode {
            Mode::Owned => {
                let l = self.scoped_temp(coll);
                self.expr_into(src, Place::local(l))?;
                l
            }
            Mode::Ref | Mode::Mut => {
                let mutable = mode == Mode::Mut;
                let ht = if mutable { mut_coll } else { ref_coll };
                let l = self.temp(ht);
                if src_is_ref {
                    let p = self.expr_place(src, mutable)?.project(ProjElem::Deref);
                    let kind = if mutable {
                        BorrowKind::Mut
                    } else {
                        BorrowKind::Shared
                    };
                    self.assign(l, Rvalue::Ref(kind, p));
                } else {
                    let p = self.expr_place(src, mutable)?;
                    let kind = if mutable {
                        BorrowKind::Mut
                    } else {
                        BorrowKind::Shared
                    };
                    self.assign(l, Rvalue::Ref(kind, p));
                }
                l
            }
        };
        let i = self.temp(usize_t);
        let n = self.temp(usize_t);
        self.assign(i, Rvalue::Use(usize_const(0)));
        if mode != Mode::Owned {
            let r = self.temp(ref_coll);
            let target = Place::local(handle).project(ProjElem::Deref);
            self.assign(r, Rvalue::Ref(BorrowKind::Shared, target));
            self.call_native(
                &format!("{coll_name}.len"),
                vec![Operand::Move(Place::local(r))],
                Place::local(n),
            );
        }
        let head = self.b.new_block();
        let body_bb = self.b.new_block();
        let step = self.b.new_block();
        let exit = self.b.new_block();
        self.goto(head);
        self.cur = head;
        if mode == Mode::Owned {
            // Owned: run while anything is left.
            let r = self.temp(ref_coll);
            self.assign(r, Rvalue::Ref(BorrowKind::Shared, Place::local(handle)));
            self.call_native(
                &format!("{coll_name}.len"),
                vec![Operand::Move(Place::local(r))],
                Place::local(n),
            );
        }
        let c = self.temp(bool_t);
        let (lhs, rhs) = if mode == Mode::Owned {
            (usize_const(0), Operand::Copy(Place::local(n)))
        } else {
            (
                Operand::Copy(Place::local(i)),
                Operand::Copy(Place::local(n)),
            )
        };
        self.assign(c, Rvalue::BinaryOp(BinOp::Lt, lhs, rhs));
        self.goto_with(
            TerminatorKind::SwitchInt {
                discr: Operand::Copy(Place::local(c)),
                targets: SwitchTargets::if_else(body_bb, exit),
            },
            body_bb,
        );
        let continue_bb = if mode == Mode::Owned { head } else { step };
        self.loops.push(LoopCx {
            label: label.clone(),
            break_bb: exit,
            continue_bb,
            depth: self.scopes.len(),
            dest: None,
        });
        let r = self.for_collection_round(
            pattern,
            body,
            mode == Mode::Owned,
            mode == Mode::Mut,
            handle,
            i,
            (&coll_name, coll, elem, elem_ref),
        );
        self.loops.pop();
        r?;
        self.goto(continue_bb);
        self.cur = step;
        self.assign(
            i,
            Rvalue::BinaryOp(BinOp::Add, Operand::Copy(Place::local(i)), usize_const(1)),
        );
        self.goto(head);
        self.cur = exit;
        self.pop_scope()?;
        self.assign(dest, Rvalue::Use(unit_const(self.unit())));
        Ok(())
    }

    /// `s` when `e` is `s.chars()` on a `String`. Until the library's
    /// iterators are written in Kāra, a `for` over it and its `.collect()`
    /// into a `Vec[char]` are lowered in place.
    fn chars_of(&mut self, e: &'a Expr) -> R<Option<&'a Expr>> {
        let ExprKind::MethodCall {
            object,
            method,
            args,
            ..
        } = &e.kind
        else {
            return Ok(None);
        };
        if method != "chars" || !args.is_empty() {
            return Ok(None);
        }
        let t = self.expr_ty(object)?;
        Ok(matches!(self.strip_ty(t), HK::Str).then_some(&**object))
    }

    /// Walk the chars of the `String` `text`: `each` runs once per char,
    /// in a scope of its own, with the char in a local. The loop borrows
    /// the string for its whole run and steps by each char's UTF-8 width,
    /// through the library's `String.char_at_byte(ref s, i)` and
    /// `char.len_utf8(c)`. `continue` goes to the loop head (`i` has
    /// already stepped).
    fn walk_chars(
        &mut self,
        label: &Option<String>,
        text: &'a Expr,
        each: &mut dyn FnMut(&mut Self, Local) -> R<()>,
    ) -> R<()> {
        let (i64_t, bool_t, char_t) = {
            let tcx = self.tys().tcx();
            (
                tcx.intern(HK::Int(IntSize::I64)),
                tcx.intern(HK::Bool),
                tcx.intern(HK::Char),
            )
        };
        let p = if self.is_place(text) {
            self.expr_place(text, false)?
        } else {
            self.temp_place(text)?
        };
        let (p, st) = self.strip_refs(p);
        let rt = self.tys().tcx().reference(st, false);
        let h = self.temp(rt);
        self.assign(h, Rvalue::Ref(BorrowKind::Shared, p));
        let reborrow = |this: &mut Self| {
            let r = this.temp(rt);
            let target = Place::local(h).project(ProjElem::Deref);
            this.assign(r, Rvalue::Ref(BorrowKind::Shared, target));
            Operand::Move(Place::local(r))
        };
        let n = self.temp(i64_t);
        let r = reborrow(self);
        self.call_native("String.len", vec![r], Place::local(n));
        let i = self.temp(i64_t);
        self.assign(
            i,
            Rvalue::Use(Operand::Const(Const {
                ty: i64_t,
                kind: ConstKind::Scalar(0),
            })),
        );
        let head = self.b.new_block();
        let body_bb = self.b.new_block();
        let exit = self.b.new_block();
        self.goto(head);
        self.cur = head;
        let c = self.temp(bool_t);
        self.assign(
            c,
            Rvalue::BinaryOp(
                BinOp::Lt,
                Operand::Copy(Place::local(i)),
                Operand::Copy(Place::local(n)),
            ),
        );
        self.goto_with(
            TerminatorKind::SwitchInt {
                discr: Operand::Copy(Place::local(c)),
                targets: SwitchTargets::if_else(body_bb, exit),
            },
            body_bb,
        );
        let ch = self.temp(char_t);
        let r = reborrow(self);
        self.call_native(
            "String.char_at_byte",
            vec![r, Operand::Copy(Place::local(i))],
            Place::local(ch),
        );
        let w = self.temp(i64_t);
        self.call_native(
            "char.len_utf8",
            vec![Operand::Copy(Place::local(ch))],
            Place::local(w),
        );
        self.assign(
            i,
            Rvalue::BinaryOp(
                BinOp::Add,
                Operand::Copy(Place::local(i)),
                Operand::Move(Place::local(w)),
            ),
        );
        self.loops.push(LoopCx {
            label: label.clone(),
            break_bb: exit,
            continue_bb: head,
            depth: self.scopes.len(),
            dest: None,
        });
        let r = (|| {
            self.push_scope();
            each(self, ch)?;
            self.pop_scope()
        })();
        self.loops.pop();
        r?;
        self.goto(head);
        self.cur = exit;
        Ok(())
    }

    /// `for c in s.chars() { body }`.
    fn for_chars(
        &mut self,
        label: &Option<String>,
        pattern: &'a Pattern,
        body: &'a Block,
        text: &'a Expr,
        dest: Place,
    ) -> R<()> {
        let char_t = self.tys().tcx().intern(HK::Char);
        self.push_scope();
        let r = self.walk_chars(label, text, &mut |this, ch| {
            let mut binds = Vec::new();
            this.bind_pattern(pattern, &Place::local(ch), char_t, false, &mut binds)?;
            for &(l, t) in &binds {
                this.declare(l, t);
            }
            let t = this.unit();
            let tmp = this.temp(t);
            this.block_into(body, Place::local(tmp))
        });
        r?;
        self.pop_scope()?;
        self.assign(dest, Rvalue::Use(unit_const(self.unit())));
        Ok(())
    }

    /// `s.chars().collect()` into a `Vec[char]`: push each char.
    fn collect_chars(&mut self, e: &'a Expr, text: &'a Expr, dest: Place) -> R<()> {
        let vt = self.expr_ty(e)?;
        let char_t = self.tys().tcx().intern(HK::Char);
        let is_char_vec = matches!(self.tys().tcx().kind(vt), HK::Intrinsic {
            kind: IntrinsicKind::Vec,
            args,
        } if self.tys().tcx().list(args)[0] == char_t);
        if !is_char_vec {
            return self.unsupported(e.span, "collecting chars into this type");
        }
        let name = self.tys().display(vt);
        self.push_scope();
        let v = self.scoped_temp(vt);
        self.call_native(&format!("{name}.new"), vec![], Place::local(v));
        let mt = self.tys().tcx().reference(vt, true);
        let unit_t = self.unit();
        self.walk_chars(&None, text, &mut |this, ch| {
            let r = this.temp(mt);
            this.assign(r, Rvalue::Ref(BorrowKind::Mut, Place::local(v)));
            let u = this.temp(unit_t);
            this.call_native(
                &format!("{name}.push"),
                vec![
                    Operand::Move(Place::local(r)),
                    Operand::Copy(Place::local(ch)),
                ],
                Place::local(u),
            );
            Ok(())
        })?;
        self.assign(dest, Rvalue::Use(Operand::Move(Place::local(v))));
        self.pop_scope()
    }

    /// `for x in a` over a fixed-size array, by position: `x` is a copy of
    /// each element (`how` is `None`, the elements are `Copy`) or a
    /// reference to it (`Some(mutable)`).
    #[allow(clippy::too_many_arguments)]
    fn for_array(
        &mut self,
        label: &Option<String>,
        pattern: &'a Pattern,
        body: &'a Block,
        src: &'a Expr,
        src_is_ref: bool,
        how: Option<bool>,
        (elem, n): (Ty, u64),
        dest: Place,
    ) -> R<()> {
        let mutable = how == Some(true);
        let (usize_t, bool_t) = {
            let tcx = self.tys().tcx();
            (tcx.intern(HK::UInt(UIntSize::Usize)), tcx.intern(HK::Bool))
        };
        let usize_const = |v: u128| {
            Operand::Const(Const {
                ty: usize_t,
                kind: ConstKind::Scalar(v),
            })
        };
        self.push_scope();
        let mut base = if self.is_place(src) {
            self.expr_place(src, mutable)?
        } else {
            self.temp_place(src)?
        };
        if src_is_ref {
            base = base.project(ProjElem::Deref);
        }
        let i = self.temp(usize_t);
        self.assign(i, Rvalue::Use(usize_const(0)));
        let head = self.b.new_block();
        let body_bb = self.b.new_block();
        let step = self.b.new_block();
        let exit = self.b.new_block();
        self.goto(head);
        self.cur = head;
        let c = self.temp(bool_t);
        self.assign(
            c,
            Rvalue::BinaryOp(
                BinOp::Lt,
                Operand::Copy(Place::local(i)),
                usize_const(n as u128),
            ),
        );
        self.goto_with(
            TerminatorKind::SwitchInt {
                discr: Operand::Copy(Place::local(c)),
                targets: SwitchTargets::if_else(body_bb, exit),
            },
            body_bb,
        );
        self.loops.push(LoopCx {
            label: label.clone(),
            break_bb: exit,
            continue_bb: step,
            depth: self.scopes.len(),
            dest: None,
        });
        let r = (|| {
            self.push_scope();
            let place = base.clone().project(ProjElem::Index(i));
            let mut binds = Vec::new();
            match (how, &pattern.kind) {
                (Some(m), PatternKind::Binding(name)) => {
                    let rt = self.tys().tcx().reference(elem, m);
                    let x = self.user_local(name, rt, pattern.id);
                    let kind = if m {
                        BorrowKind::Mut
                    } else {
                        BorrowKind::Shared
                    };
                    self.assign(x, Rvalue::Ref(kind, place));
                    binds.push((x, rt));
                }
                _ => self.bind_pattern(pattern, &place, elem, how.is_some(), &mut binds)?,
            }
            for &(l, t) in &binds {
                self.declare(l, t);
            }
            let t = self.unit();
            let tmp = self.temp(t);
            self.block_into(body, Place::local(tmp))?;
            self.pop_scope()
        })();
        self.loops.pop();
        r?;
        self.goto(step);
        self.cur = step;
        self.assign(
            i,
            Rvalue::BinaryOp(BinOp::Add, Operand::Copy(Place::local(i)), usize_const(1)),
        );
        self.goto(head);
        self.cur = exit;
        self.pop_scope()?;
        self.assign(dest, Rvalue::Use(unit_const(self.unit())));
        Ok(())
    }

    /// Is `coll` a library collection read by `entry_at` (every one but
    /// `Vec` and slices, which use `index`)?
    fn by_entry(&self, coll: Ty) -> bool {
        matches!(
            self.tys().tcx().kind(coll),
            HK::Intrinsic { kind, .. } if kind != IntrinsicKind::Vec
        )
    }

    /// One round of [`Self::for_collection`]: take the element, bind the
    /// pattern to it, and run the body in its own scope.
    #[allow(clippy::too_many_arguments)]
    fn for_collection_round(
        &mut self,
        pattern: &'a Pattern,
        body: &'a Block,
        owned: bool,
        mutable: bool,
        handle: Local,
        i: Local,
        (coll_name, coll, elem, elem_ref): (&str, Ty, Ty, Ty),
    ) -> R<()> {
        self.push_scope();
        let usize_t = self.b.local_ty(i);
        let (place, by_ref) = if owned {
            let x = self.scoped_temp(elem);
            let mt = self.tys().tcx().reference(coll, true);
            let r = self.temp(mt);
            self.assign(r, Rvalue::Ref(BorrowKind::Mut, Place::local(handle)));
            self.call_native(
                &format!("{coll_name}.remove"),
                vec![
                    Operand::Move(Place::local(r)),
                    Operand::Const(Const {
                        ty: usize_t,
                        kind: ConstKind::Scalar(0),
                    }),
                ],
                Place::local(x),
            );
            (Place::local(x), false)
        } else {
            let ht = self.b.local_ty(handle);
            let h = self.temp(ht);
            let kind = if mutable {
                BorrowKind::Mut
            } else {
                BorrowKind::Shared
            };
            self.assign(
                h,
                Rvalue::Ref(kind, Place::local(handle).project(ProjElem::Deref)),
            );
            let r = self.temp(elem_ref);
            let by_entry = self.by_entry(coll);
            let method = match (by_entry, mutable) {
                (true, _) => "entry_at",
                (false, true) => "index_mut",
                (false, false) => "index",
            };
            self.call_native(
                &format!("{coll_name}.{method}"),
                vec![
                    Operand::Move(Place::local(h)),
                    Operand::Copy(Place::local(i)),
                ],
                Place::local(r),
            );
            (Place::local(r).project(ProjElem::Deref), true)
        };
        let mut binds = Vec::new();
        match &pattern.kind {
            // A shared reference to a `Copy` element reads the same as a
            // copy of it, which is what `x` holds.
            PatternKind::Binding(name) if by_ref && !mutable && self.is_copy(elem) => {
                let x = self.user_local(name, elem, pattern.id);
                self.assign(x, Rvalue::Use(Operand::Copy(place.clone())));
                binds.push((x, elem));
            }
            // `for x in v.iter()`: `x` is the reference itself.
            PatternKind::Binding(name) if by_ref => {
                let x = self.user_local(name, elem_ref, pattern.id);
                self.assign(x, Rvalue::Use(Operand::Copy(Place::local(place.local))));
                binds.push((x, elem_ref));
            }
            _ => self.bind_pattern(pattern, &place, elem, by_ref, &mut binds)?,
        }
        for &(l, t) in &binds {
            self.declare(l, t);
        }
        let t = self.unit();
        let tmp = self.temp(t);
        self.block_into(body, Place::local(tmp))?;
        self.pop_scope()
    }

    // ── exits ───────────────────────────────────────────────────────

    /// The return place holds the value: run every scope's exit sequence,
    /// with the `errdefer` bodies when the value is an error, and return.
    fn return_exit(&mut self) -> R<()> {
        let has_errdefer = self
            .scopes
            .iter()
            .flatten()
            .any(|e| matches!(e, ScopeEntry::ErrDefer(_)));
        let ret_ty = self.b.local_ty(Local::RETURN_PLACE);
        let err_variant = self.error_variant(ret_ty);
        match (has_errdefer, err_variant) {
            (true, Some(err)) => {
                let isize_t = self.tys().tcx().intern(HK::Int(IntSize::I64));
                let d = self.temp(isize_t);
                self.assign(d, Rvalue::Discriminant(Place::local(Local::RETURN_PLACE)));
                let err_bb = self.b.new_block();
                let ok_bb = self.b.new_block();
                self.goto_with(
                    TerminatorKind::SwitchInt {
                        discr: Operand::Copy(Place::local(d)),
                        targets: SwitchTargets {
                            values: vec![(err as u128, err_bb)],
                            otherwise: ok_bb,
                        },
                    },
                    err_bb,
                );
                self.exit_to(0, true)?;
                self.diverge(TerminatorKind::Return);
                self.cur = ok_bb;
                self.exit_to(0, false)?;
                self.diverge(TerminatorKind::Return);
            }
            _ => {
                self.exit_to(0, false)?;
                self.diverge(TerminatorKind::Return);
            }
        }
        Ok(())
    }

    /// The index of the error variant of a `Result` (or `None` of an
    /// `Option`), when `t` is one.
    fn error_variant(&self, t: Ty) -> Option<u32> {
        let (adt, _) = self.tys().tcx().adt_of(t)?;
        let err = match adt.name.as_str() {
            "Result" => "Err",
            "Option" => "None",
            _ => return None,
        };
        adt.variants
            .iter()
            .position(|v| v.name == err)
            .map(|i| i as u32)
    }

    /// `inner?`: the success payload, or return the error.
    fn question(&mut self, e: &'a Expr, inner: &'a Expr, dest: Place) -> R<()> {
        let it = self.expr_ty(inner)?;
        let Some(err) = self.error_variant(it) else {
            return self.unsupported(e.span, "`?` on this type");
        };
        let ok = 1 - err;
        let p = if self.is_place(inner) {
            self.expr_place(inner, false)?
        } else {
            let l = self.scoped_temp(it);
            self.expr_into(inner, Place::local(l))?;
            Place::local(l)
        };
        let isize_t = self.tys().tcx().intern(HK::Int(IntSize::I64));
        let d = self.temp(isize_t);
        self.assign(d, Rvalue::Discriminant(p.clone()));
        let err_bb = self.b.new_block();
        let ok_bb = self.b.new_block();
        self.goto_with(
            TerminatorKind::SwitchInt {
                discr: Operand::Copy(Place::local(d)),
                targets: SwitchTargets {
                    values: vec![(err as u128, err_bb)],
                    otherwise: ok_bb,
                },
            },
            err_bb,
        );
        // Error path: rebuild the error in the return type.
        let ret_ty = self.b.local_ty(Local::RETURN_PLACE);
        let ret = Place::local(Local::RETURN_PLACE);
        let fields: Vec<Ty> = match self.tys().tcx().adt_of(it) {
            Some((adt, _)) => (0..adt.variants[err as usize].fields.len() as u32)
                .filter_map(|f| self.tys().tcx().field_ty(it, Some(err), f))
                .collect(),
            None => Vec::new(),
        };
        let mut ops = Vec::new();
        for (i, ft) in fields.into_iter().enumerate() {
            let fp = p
                .project(ProjElem::Downcast(VariantIdx(err)))
                .field(i as u32, ft);
            ops.push(self.use_place(fp, ft));
        }
        let Some(ret_err) = self.error_variant(ret_ty) else {
            return self.unsupported(e.span, "`?` in a function that returns no `Result`");
        };
        let kind = self.adt_aggregate(ret_ty, ret_err);
        self.assign(ret, Rvalue::Aggregate(kind, ops));
        self.return_exit()?;
        // Success path: move the payload out.
        self.cur = ok_bb;
        let Some(pt) = self.tys().tcx().field_ty(it, Some(ok), 0) else {
            self.assign(dest, Rvalue::Use(unit_const(self.unit())));
            return Ok(());
        };
        let fp = p.project(ProjElem::Downcast(VariantIdx(ok))).field(0, pt);
        let op = self.use_place(fp, pt);
        self.assign(dest, Rvalue::Use(op));
        Ok(())
    }

    // ── match ───────────────────────────────────────────────────────

    fn lower_match(
        &mut self,
        e: &'a Expr,
        scrutinee: &'a Expr,
        arms: &'a [ast::MatchArm],
        dest: Place,
    ) -> R<()> {
        // The scrutinee temporary lives as long as the whole match.
        self.push_scope();
        let (place, st, by_ref) = self.scrutinee(scrutinee)?;
        let join = self.b.new_block();
        for arm in arms {
            let next = self.b.new_block();
            self.test_pattern(&arm.pattern, &place, st, next)?;
            self.push_scope();
            let mut binds = Vec::new();
            // A guard reads the bindings by reference; the arm moves them
            // only once it is chosen (`docs/spikes/mir-types.md` §9.5).
            let guard_by_ref = arm.guard.is_some() && !by_ref;
            self.bind_pattern(&arm.pattern, &place, st, by_ref || guard_by_ref, &mut binds)?;
            for &(l, t) in &binds {
                self.declare(l, t);
            }
            if let Some(g) = &arm.guard {
                let c = self.expr_operand(g)?;
                let body_bb = self.b.new_block();
                let fail = self.b.new_block();
                self.goto_with(
                    TerminatorKind::SwitchInt {
                        discr: c,
                        targets: SwitchTargets::if_else(body_bb, fail),
                    },
                    fail,
                );
                // A failed guard leaves the arm's scope and tries the next.
                let entries = std::mem::take(self.scopes.last_mut().unwrap());
                self.exit_entries(&entries, false)?;
                *self.scopes.last_mut().unwrap() = entries;
                self.goto(next);
                self.cur = body_bb;
                if guard_by_ref {
                    let mut moved = Vec::new();
                    self.bind_pattern(&arm.pattern, &place, st, false, &mut moved)?;
                    for &(l, t) in &moved {
                        self.declare(l, t);
                    }
                }
            }
            self.expr_into(&arm.body, dest.clone())?;
            self.pop_scope()?;
            self.goto(join);
            self.cur = next;
        }
        let _ = e;
        self.diverge(TerminatorKind::Abort {
            reason: AbortReason::UnreachableArm,
        });
        self.cur = join;
        self.pop_scope()
    }

    /// The place a `match` or `if let` reads its scrutinee from (a temporary
    /// in the current scope unless it names a local's place), seen through
    /// any reference: the place, its type, and whether it binds by
    /// reference.
    fn scrutinee(&mut self, scrutinee: &'a Expr) -> R<(Place, Ty, bool)> {
        let st = self.expr_ty(scrutinee)?;
        // An element (`v[i]`) is matched where it lies, as a local is: a
        // binding then copies, counts or borrows it out of the collection.
        let element = matches!(scrutinee.kind, ExprKind::Index { .. });
        let place = if self.is_place(scrutinee) && (element || self.is_local_rooted(scrutinee)) {
            self.expr_place(scrutinee, false)?
        } else {
            let l = self.scoped_temp(st);
            self.expr_into(scrutinee, Place::local(l))?;
            Place::local(l)
        };
        // Matching through a reference binds by reference.
        Ok(match self.tys().tcx().kind(st) {
            HK::Ref(inner) | HK::MutRef(inner) => (place.project(ProjElem::Deref), inner, true),
            _ => (place, st, false),
        })
    }

    /// `if let pattern = value { then } else { .. }`: a two-arm match.
    fn if_let(
        &mut self,
        pattern: &'a Pattern,
        value: &'a Expr,
        then_block: &'a Block,
        else_branch: Option<&'a Expr>,
        dest: Place,
    ) -> R<()> {
        self.push_scope();
        let (place, st, by_ref) = self.scrutinee(value)?;
        let else_bb = self.b.new_block();
        let join = self.b.new_block();
        self.test_pattern(pattern, &place, st, else_bb)?;
        self.push_scope();
        let mut binds = Vec::new();
        self.bind_pattern(pattern, &place, st, by_ref, &mut binds)?;
        for &(l, t) in &binds {
            self.declare(l, t);
        }
        self.block_into(then_block, dest.clone())?;
        self.pop_scope()?;
        self.goto(join);
        self.cur = else_bb;
        match else_branch {
            Some(els) => {
                self.push_scope();
                self.expr_into(els, dest)?;
                self.pop_scope()?;
            }
            None => self.assign(dest, Rvalue::Use(unit_const(self.unit()))),
        }
        self.goto(join);
        self.cur = join;
        self.pop_scope()
    }

    /// One round of `while let`: test the scrutinee, leaving for `exit` (its
    /// temporaries dropped) when the pattern fails, else run the body and
    /// go back to `head`.
    fn while_let_round(
        &mut self,
        pattern: &'a Pattern,
        value: &'a Expr,
        body: &'a Block,
        head: BasicBlock,
        exit: BasicBlock,
    ) -> R<()> {
        self.push_scope();
        let (place, st, by_ref) = self.scrutinee(value)?;
        let fail = self.b.new_block();
        self.test_pattern(pattern, &place, st, fail)?;
        let matched = self.cur;
        self.cur = fail;
        let entries = std::mem::take(self.scopes.last_mut().unwrap());
        let r = self.exit_entries(&entries, false);
        *self.scopes.last_mut().unwrap() = entries;
        r?;
        self.goto(exit);
        self.cur = matched;
        self.push_scope();
        let mut binds = Vec::new();
        self.bind_pattern(pattern, &place, st, by_ref, &mut binds)?;
        for &(l, t) in &binds {
            self.declare(l, t);
        }
        let t = self.unit();
        let tmp = self.temp(t);
        self.block_into(body, Place::local(tmp))?;
        self.pop_scope()?;
        self.pop_scope()?;
        self.goto(head);
        Ok(())
    }

    fn is_local_rooted(&self, e: &Expr) -> bool {
        match &e.kind {
            ExprKind::Identifier(_) => self.is_local(e),
            ExprKind::SelfValue => true,
            ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
                self.is_local_rooted(object)
            }
            _ => false,
        }
    }

    /// Branch to `fail` unless `place` (of type `t`) matches `pat`;
    /// continue in the current block when it does.
    fn test_pattern(&mut self, pat: &'a Pattern, place: &Place, t: Ty, fail: BasicBlock) -> R<()> {
        match &pat.kind {
            PatternKind::Wildcard | PatternKind::Binding(_) => {
                // A bare name may be a unit variant (`None`).
                if let PatternKind::Binding(_) = &pat.kind {
                    if let Some(Res::Def(d)) = self.lcx.res.get(&pat.id) {
                        let d = *d;
                        return self.test_variant(pat, d, &[], place, t, fail);
                    }
                }
                Ok(())
            }
            PatternKind::Literal(lit) => {
                let v = match lit {
                    LiteralPattern::Integer(v, _) => *v as u128,
                    LiteralPattern::Char(c) => *c as u128,
                    LiteralPattern::Bool(b) => *b as u128,
                    _ => return self.unsupported(pat.span, "this literal pattern"),
                };
                let ok = self.b.new_block();
                let op = Operand::Copy(place.clone());
                self.goto_with(
                    TerminatorKind::SwitchInt {
                        discr: op,
                        targets: SwitchTargets {
                            values: vec![(v, ok)],
                            otherwise: fail,
                        },
                    },
                    ok,
                );
                Ok(())
            }
            PatternKind::Tuple(ps) => {
                for (i, p) in ps.iter().enumerate() {
                    let Some(ft) = self.tys().field_ty(t, None, i as u32) else {
                        return self.unsupported(p.span, "this tuple pattern");
                    };
                    self.test_pattern(p, &place.field(i as u32, ft), ft, fail)?;
                }
                Ok(())
            }
            PatternKind::TupleVariant { patterns, .. } => match self.lcx.res.get(&pat.id) {
                Some(Res::Def(d)) => {
                    let d = *d;
                    self.test_variant(pat, d, patterns, place, t, fail)
                }
                _ => self.unsupported(pat.span, "this variant pattern"),
            },
            PatternKind::Struct { fields, .. } => {
                let variant = match self.lcx.res.get(&pat.id) {
                    Some(Res::Def(d)) => self.lcx.variant(*d),
                    _ => None,
                };
                let base = match variant {
                    Some((_, idx)) => {
                        self.switch_variant(place, idx, fail);
                        place.project(ProjElem::Downcast(VariantIdx(idx)))
                    }
                    None => place.clone(),
                };
                for fp in fields {
                    let Some((idx, ft)) = self.field_of(t, variant.map(|v| v.1), &fp.name) else {
                        return self.unsupported(fp.span, "this field pattern");
                    };
                    if let Some(p) = &fp.pattern {
                        self.test_pattern(p, &base.field(idx, ft), ft, fail)?;
                    }
                }
                Ok(())
            }
            _ => self.unsupported(pat.span, "this pattern"),
        }
    }

    fn test_variant(
        &mut self,
        pat: &'a Pattern,
        v: DefId,
        subs: &'a [Pattern],
        place: &Place,
        t: Ty,
        fail: BasicBlock,
    ) -> R<()> {
        let Some((_, idx)) = self.lcx.variant(v) else {
            return self.unsupported(pat.span, "this variant");
        };
        self.switch_variant(place, idx, fail);
        let base = place.project(ProjElem::Downcast(VariantIdx(idx)));
        for (i, p) in subs.iter().enumerate() {
            let Some(ft) = self.tys().tcx().field_ty(t, Some(idx), i as u32) else {
                return self.unsupported(p.span, "this payload pattern");
            };
            self.test_pattern(p, &base.field(i as u32, ft), ft, fail)?;
        }
        Ok(())
    }

    fn switch_variant(&mut self, place: &Place, idx: u32, fail: BasicBlock) {
        let isize_t = self.tys().tcx().intern(HK::Int(IntSize::I64));
        let d = self.temp(isize_t);
        self.assign(d, Rvalue::Discriminant(place.clone()));
        let ok = self.b.new_block();
        self.goto_with(
            TerminatorKind::SwitchInt {
                discr: Operand::Copy(Place::local(d)),
                targets: SwitchTargets {
                    values: vec![(idx as u128, ok)],
                    otherwise: fail,
                },
            },
            ok,
        );
    }

    /// Bind the names of a pattern known to match. A binding moves its
    /// part out (core semantics §3), or borrows it when matching went
    /// through a reference.
    fn bind_pattern(
        &mut self,
        pat: &'a Pattern,
        place: &Place,
        t: Ty,
        by_ref: bool,
        out: &mut Vec<(Local, Ty)>,
    ) -> R<()> {
        match &pat.kind {
            PatternKind::Wildcard | PatternKind::Literal(_) => Ok(()),
            PatternKind::Binding(name) => {
                if matches!(self.lcx.res.get(&pat.id), Some(Res::Def(_))) {
                    return Ok(());
                }
                let by_ref = by_ref
                    || self
                        .lcx
                        .ref_bindings
                        .contains(&SpanKey::from_span(&pat.span));
                self.bind_one(name, pat.id, place.clone(), t, by_ref, out);
                Ok(())
            }
            PatternKind::Tuple(ps) => {
                for (i, p) in ps.iter().enumerate() {
                    let ft = self.tys().field_ty(t, None, i as u32).unwrap();
                    self.bind_pattern(p, &place.field(i as u32, ft), ft, by_ref, out)?;
                }
                Ok(())
            }
            PatternKind::TupleVariant { patterns, .. } => {
                let Some(Res::Def(d)) = self.lcx.res.get(&pat.id) else {
                    return Ok(());
                };
                let Some((_, idx)) = self.lcx.variant(*d) else {
                    return Ok(());
                };
                let base = place.project(ProjElem::Downcast(VariantIdx(idx)));
                for (i, p) in patterns.iter().enumerate() {
                    let ft = self.tys().tcx().field_ty(t, Some(idx), i as u32).unwrap();
                    self.bind_pattern(p, &base.field(i as u32, ft), ft, by_ref, out)?;
                }
                Ok(())
            }
            PatternKind::Struct { fields, .. } => {
                let variant = match self.lcx.res.get(&pat.id) {
                    Some(Res::Def(d)) => self.lcx.variant(*d),
                    _ => None,
                };
                let base = match variant {
                    Some((_, idx)) => place.project(ProjElem::Downcast(VariantIdx(idx))),
                    None => place.clone(),
                };
                for fp in fields {
                    let (idx, ft) = self.field_of(t, variant.map(|v| v.1), &fp.name).unwrap();
                    let fplace = base.field(idx, ft);
                    match &fp.pattern {
                        Some(p) => self.bind_pattern(p, &fplace, ft, by_ref, out)?,
                        None => self.bind_one(&fp.name, pat.id, fplace, ft, by_ref, out),
                    }
                }
                Ok(())
            }
            _ => self.unsupported(pat.span, "this pattern"),
        }
    }

    /// Does `place` reach its target through a `mut ref`?
    fn through_mut_ref(&self, place: &Place) -> bool {
        (0..place.projection.len()).any(|k| {
            matches!(place.projection[k], ProjElem::Deref) && {
                let base = Place {
                    local: place.local,
                    projection: place.projection[..k].to_vec(),
                };
                matches!(self.tys().tcx().kind(self.place_type(&base)), HK::MutRef(_))
            }
        })
    }

    fn bind_one(
        &mut self,
        name: &str,
        node: NodeId,
        place: Place,
        t: Ty,
        by_ref: bool,
        out: &mut Vec<(Local, Ty)>,
    ) {
        // Matching through a `mut ref` binds `mut ref`s into the scrutinee,
        // `Copy` parts included (`Some(n) => set_to(n, 42)` writes the
        // payload); through a `ref`, a `Copy` part is copied, which reads
        // the same.
        let mutable = by_ref && self.through_mut_ref(&place);
        if mutable || (by_ref && !self.is_copy(t)) {
            let rt = self.tys().tcx().reference(t, mutable);
            let l = self.user_local(name, rt, node);
            let kind = if mutable {
                BorrowKind::Mut
            } else {
                BorrowKind::Shared
            };
            self.assign(l, Rvalue::Ref(kind, place));
            out.push((l, rt));
        } else {
            let l = self.user_local(name, t, node);
            if self.is_handle(t) || self.is_handle_aggregate(t) {
                // A `shared` handle binds a counted copy (§6.1).
                self.count_copy(place, t, Place::local(l));
            } else {
                let op = self.use_place(place, t);
                self.assign(l, Rvalue::Use(op));
            }
            out.push((l, t));
        }
    }

    // ── calls ───────────────────────────────────────────────────────

    fn call_native(&mut self, name: &str, args: Vec<Operand>, dest: Place) {
        let func = self.fn_operand(name, DefId(u32::MAX), Vec::new());
        let next = self.b.new_block();
        self.goto_with(
            TerminatorKind::Call {
                func,
                args,
                destination: dest,
                target: Some(next),
                unwind: UnwindAction::Abort,
            },
            next,
        );
    }

    fn fn_operand(&self, name: &str, def: DefId, args: Vec<Ty>) -> Operand {
        let unit = self.unit();
        Operand::Const(Const {
            ty: unit,
            kind: ConstKind::FnDef(InstanceId {
                def,
                args,
                name: name.to_string(),
            }),
        })
    }

    fn call(&mut self, e: &'a Expr, callee: &'a Expr, args: &'a [CallArg], dest: Place) -> R<()> {
        let Some(rc) = self.lcx.calls.get(&e.id) else {
            return self.unsupported(e.span, "a call with no resolved callee");
        };
        let callee_kind = rc.callee.clone();
        let substs = self.lcx.tys.tcx().list(rc.substs);
        let _ = callee;
        match callee_kind {
            Callee::Builtin(name) => self.builtin_call(e, &name, args, dest),
            Callee::Value => self.call_value(e, callee, args, dest),
            Callee::Def(d) => {
                if let Some((_, idx)) = self.lcx.variant(d) {
                    let t = self.expr_ty(e)?;
                    let mut ops = Vec::new();
                    for a in args {
                        ops.push(self.expr_operand(&a.value)?);
                    }
                    let kind = self.adt_aggregate(t, idx);
                    self.assign(dest, Rvalue::Aggregate(kind, ops));
                    return Ok(());
                }
                if args.iter().any(|a| a.label.is_some()) {
                    return self.unsupported(e.span, "a call with labelled arguments");
                }
                let inst_args = self.instance_args(e.span, &substs)?;
                let f = self.lcx.fns.get(&d).map(|i| i.f);
                let Some(f) = f else {
                    return self.unsupported(e.span, "a call to this function");
                };
                if args.len() != f.params.len() {
                    return self.unsupported(e.span, "a call that leaves out default arguments");
                }
                let mut ops = Vec::new();
                let mut fn_tys: Vec<Option<Ty>> = Vec::new();
                for (a, p) in args.iter().zip(&f.params) {
                    if self.is_fn_typed(p.pattern.id) {
                        let (op, t) = self.fn_arg(&a.value)?;
                        ops.push(op);
                        fn_tys.push(Some(t));
                        continue;
                    }
                    let pt = self.callee_param_ty(p, &inst_args)?;
                    ops.push(self.arg_operand(&a.value, pt)?);
                    fn_tys.push(None);
                }
                let name = if fn_tys.iter().any(Option::is_some) {
                    self.lcx.instance_with_fns(d, inst_args.clone(), fn_tys)
                } else {
                    self.lcx.instance(d, inst_args.clone())
                };
                let func = self.fn_operand(&name, d, inst_args);
                let next = self.b.new_block();
                self.goto_with(
                    TerminatorKind::Call {
                        func,
                        args: ops,
                        destination: dest,
                        target: Some(next),
                        unwind: UnwindAction::Abort,
                    },
                    next,
                );
                Ok(())
            }
        }
    }

    /// The MIR types of a call's type arguments, in this instance.
    fn instance_args(&mut self, span: Span, substs: &[Ty]) -> R<Vec<Ty>> {
        let args = self.args.clone();
        let mut out = Vec::new();
        for &t in substs {
            match self.lcx.mir_ty(t, &args) {
                Ok(t) => out.push(t),
                Err(e) => return self.unsupported(span, &e),
            }
        }
        Ok(out)
    }

    fn callee_param_ty(&mut self, p: &'a ast::Param, inst_args: &[Ty]) -> R<Ty> {
        let Some(&t) = self.lcx.node_types.get(&p.pattern.id) else {
            return self.unsupported(p.span, "a parameter with no recorded type");
        };
        match self.lcx.mir_ty(t, inst_args) {
            Ok(t) => Ok(t),
            Err(e) => self.unsupported(p.span, &e),
        }
    }

    /// An argument for a parameter of type `pt`: borrowed when the
    /// parameter is a reference and the argument is not.
    fn arg_operand(&mut self, a: &'a Expr, pt: Ty) -> R<Operand> {
        let tcx = self.tys().tcx();
        let borrow = match tcx.kind(pt) {
            HK::Ref(_) => Some(BorrowKind::Shared),
            HK::MutRef(_) => Some(BorrowKind::Mut),
            _ => None,
        };
        let at = self.expr_ty(a)?;
        let slice_t = match self.tys().tcx().kind(pt) {
            HK::Ref(t) | HK::MutRef(t) => t,
            _ => pt,
        };
        if let HK::Slice { mutable, .. } = self.tys().tcx().kind(slice_t) {
            if !matches!(self.strip_ty(at), HK::Slice { .. }) {
                // A `Vec` or array passed for a slice: view it as one.
                let s = self.slice_view(a, None, slice_t, mutable)?;
                return Ok(match borrow {
                    Some(kind) => {
                        let r = self.temp(pt);
                        self.assign(r, Rvalue::Ref(kind, Place::local(s)));
                        Operand::Move(Place::local(r))
                    }
                    None => Operand::Move(Place::local(s)),
                });
            }
        }
        let arg_is_ref = matches!(self.tys().tcx().kind(at), HK::Ref(_) | HK::MutRef(_));
        // A `mut ref` place passed on is reborrowed, `&mut (*r)` (or `&(*r)`
        // for a `ref` parameter), so the caller keeps its reference.
        if let (Some(kind), HK::MutRef(_)) = (borrow, self.tys().tcx().kind(at)) {
            if self.is_place(a) {
                let p = self
                    .expr_place(a, kind == BorrowKind::Mut)?
                    .project(ProjElem::Deref);
                let r = self.temp(pt);
                self.assign(r, Rvalue::Ref(kind, p));
                return Ok(Operand::Move(Place::local(r)));
            }
        }
        match borrow {
            Some(kind) if !arg_is_ref => {
                let p = self.expr_place(a, kind == BorrowKind::Mut)?;
                let r = self.temp(pt);
                self.assign(r, Rvalue::Ref(kind, p));
                Ok(Operand::Move(Place::local(r)))
            }
            _ => self.expr_operand(a),
        }
    }

    /// `t` under any references, and its kind.
    fn strip_ty_full(&self, mut t: Ty) -> (HK, Ty) {
        loop {
            match self.tys().tcx().kind(t) {
                HK::Ref(inner) | HK::MutRef(inner) => t = inner,
                k => return (k, t),
            }
        }
    }

    /// The kind of `t` under any references.
    /// How many references `t` is under.
    fn ref_depth(&self, mut t: Ty) -> usize {
        let mut n = 0;
        while let HK::Ref(inner) | HK::MutRef(inner) = self.tys().tcx().kind(t) {
            t = inner;
            n += 1;
        }
        n
    }

    fn strip_ty(&self, mut t: Ty) -> HK {
        loop {
            match self.tys().tcx().kind(t) {
                HK::Ref(inner) | HK::MutRef(inner) => t = inner,
                k => return k,
            }
        }
    }

    /// A slice of type `slice_t` viewing the `Vec` or array `coll`, whole
    /// or (`range`) the part from `start` to `end`: the library's
    /// `as_slice` / `as_mut_slice`, or `slice` / `slice_mut` with `usize`
    /// bounds (an open end is the length).
    fn slice_view(
        &mut self,
        coll: &'a Expr,
        range: Option<(Option<&'a Expr>, Option<&'a Expr>)>,
        slice_t: Ty,
        mutable: bool,
    ) -> R<Local> {
        let p = self.expr_place(coll, mutable)?;
        let (p, ct) = self.strip_refs(p);
        let (rt, usize_t) = {
            let tcx = self.tys().tcx();
            (
                tcx.reference(ct, mutable),
                tcx.intern(HK::UInt(UIntSize::Usize)),
            )
        };
        let kind = if mutable {
            BorrowKind::Mut
        } else {
            BorrowKind::Shared
        };
        let name = self.tys().display(ct);
        let s = self.temp(slice_t);
        let Some((start, end)) = range else {
            let r = self.temp(rt);
            self.assign(r, Rvalue::Ref(kind, p));
            let method = if mutable { "as_mut_slice" } else { "as_slice" };
            self.call_native(
                &format!("{name}.{method}"),
                vec![Operand::Move(Place::local(r))],
                Place::local(s),
            );
            return Ok(s);
        };
        let lo = match start {
            Some(x) => {
                let o = self.expr_operand(x)?;
                self.cast_index(o, usize_t)
            }
            None => Operand::Const(Const {
                ty: usize_t,
                kind: ConstKind::Scalar(0),
            }),
        };
        let hi = match end {
            Some(x) => {
                let o = self.expr_operand(x)?;
                self.cast_index(o, usize_t)
            }
            None => {
                let shared = self.tys().tcx().reference(ct, false);
                let r = self.temp(shared);
                self.assign(r, Rvalue::Ref(BorrowKind::Shared, p.clone()));
                let n = self.temp(usize_t);
                self.call_native(
                    &format!("{name}.len"),
                    vec![Operand::Move(Place::local(r))],
                    Place::local(n),
                );
                Operand::Copy(Place::local(n))
            }
        };
        let r = self.temp(rt);
        self.assign(r, Rvalue::Ref(kind, p));
        let method = if mutable { "slice_mut" } else { "slice" };
        self.call_native(
            &format!("{name}.{method}"),
            vec![Operand::Move(Place::local(r)), lo, hi],
            Place::local(s),
        );
        Ok(s)
    }

    fn builtin_call(&mut self, e: &'a Expr, name: &str, args: &'a [CallArg], dest: Place) -> R<()> {
        match name {
            "println" | "print" => {
                let mut ops = Vec::new();
                for a in args {
                    self.print_operands(&a.value, &mut ops)?;
                }
                self.call_native(name, ops, dest);
                Ok(())
            }
            "panic" => {
                self.diverge(TerminatorKind::Abort {
                    reason: AbortReason::Panic,
                });
                Ok(())
            }
            // `Vec.new()`, `String.new()`, `Map.new()`, `Vec.with_capacity(n)`:
            // a library constructor, named after the type it builds.
            _ if name.contains('.') => {
                let (owner, m) = name.rsplit_once('.').unwrap();
                let t = self.expr_ty(e)?;
                let ty_name = self.tys().display(t);
                if ty_name.split('[').next() != Some(owner) {
                    return self.unsupported(e.span, &format!("the builtin `{name}`"));
                }
                let mut ops = Vec::new();
                for a in args {
                    ops.push(self.expr_operand(&a.value)?);
                }
                self.call_native(&format!("{ty_name}.{m}"), ops, dest);
                Ok(())
            }
            _ => self.unsupported(e.span, &format!("the builtin `{name}`")),
        }
    }

    /// The operands `print` shows for one argument: an f-string's parts one
    /// after another, a string literal as a constant, a non-`Copy` value
    /// by reference.
    fn print_operands(&mut self, a: &'a Expr, ops: &mut Vec<Operand>) -> R<()> {
        match &a.kind {
            ExprKind::StringLit(s) | ExprKind::MultiStringLit(s) => {
                ops.push(self.static_str(s));
                Ok(())
            }
            ExprKind::InterpolatedStringLit(parts) => {
                for p in parts {
                    match p {
                        ParsedInterpolationPart::Text(s) => ops.push(self.static_str(s)),
                        ParsedInterpolationPart::Expr(x, None) => self.print_operands(x, ops)?,
                        ParsedInterpolationPart::Expr(x, Some(_)) => {
                            return self.unsupported(x.span, "a format spec")
                        }
                    }
                }
                Ok(())
            }
            _ => {
                let t = self.expr_ty(a)?;
                let (_, base) = self.strip_ty_full(t);
                if let Some((d, args)) = self.user_impl_method(base, "Display", "to_string") {
                    // A user type prints through its `Display`: its
                    // `to_string` into a String the statement drops.
                    let p = self.expr_place(a, false)?;
                    let (p, base) = self.strip_refs(p);
                    let recv = self.ref_to(p, base);
                    let st = self.tys().tcx().intern(HK::Str);
                    let s = self.scoped_temp(st);
                    let name = self.lcx.instance(d, args.clone());
                    let func = self.fn_operand(&name, d, args);
                    let next = self.b.new_block();
                    self.goto_with(
                        TerminatorKind::Call {
                            func,
                            args: vec![recv],
                            destination: Place::local(s),
                            target: Some(next),
                            unwind: UnwindAction::Abort,
                        },
                        next,
                    );
                    ops.push(self.ref_to(Place::local(s), st));
                } else if self.is_copy(t) {
                    ops.push(self.expr_operand(a)?);
                } else {
                    let p = self.expr_place(a, false)?;
                    let rt = self.tys().tcx().reference(t, false);
                    let r = self.temp(rt);
                    self.assign(r, Rvalue::Ref(BorrowKind::Shared, p));
                    ops.push(Operand::Move(Place::local(r)));
                }
                Ok(())
            }
        }
    }

    fn method_call(
        &mut self,
        e: &'a Expr,
        object: &'a Expr,
        method: &str,
        args: &'a [CallArg],
        dest: Place,
    ) -> R<()> {
        if let Some(resource) = self.ambient_module(object) {
            // `env.args()`, `clock.now()`, …: a call of the ambient
            // resource's library method, `Env.args`. Copy arguments go by
            // value, the rest by reference.
            let mut ops = Vec::new();
            for a in args {
                let t = self.expr_ty(&a.value)?;
                let by_ref = !self.is_copy(t);
                ops.push(self.lib_arg(&a.value, by_ref)?);
            }
            self.call_native(&format!("{resource}.{method}"), ops, dest);
            return Ok(());
        }
        if method == "collect" && args.is_empty() {
            if let Some(text) = self.chars_of(object)? {
                return self.collect_chars(e, text, dest);
            }
        }
        let Some(rc) = self.lcx.calls.get(&e.id) else {
            if self.option_method(object, method, args, dest.clone())? {
                return Ok(());
            }
            // In a generic body a trait method is found once the receiver's
            // type is known: the one impl method of that name for it.
            let rt = self.expr_ty(object)?;
            if let Some((d, inst_args)) = self.method_by_receiver(rt, method) {
                return self.user_method_call(e, object, d, inst_args, args, dest);
            }
            return self.unsupported(
                e.span,
                &format!("the method call `.{method}` with no resolved callee"),
            );
        };
        let callee = rc.callee.clone();
        let substs = self.lcx.tys.tcx().list(rc.substs);
        match callee {
            Callee::Def(d) if self.lcx.fns.contains_key(&d) => {
                let inst_args = self.instance_args(e.span, &substs)?;
                self.user_method_call(e, object, d, inst_args, args, dest)
            }
            Callee::Def(d) => {
                let key = format!("{}.{method}", self.lcx.def_name(d));
                self.builtin_method(e, &key, object, method, args, dest)
            }
            Callee::Builtin(key) => self.builtin_method(e, &key, object, method, args, dest),
            Callee::Value => self.unsupported(e.span, "this method call"),
        }
    }

    /// The method `method` of the impl for `recv`'s type, when exactly one
    /// impl has it, with its instance arguments (the impl's parameters
    /// are the receiver's type arguments).
    fn method_by_receiver(&self, recv: Ty, method: &str) -> Option<(DefId, Vec<Ty>)> {
        let (kind, base) = self.strip_ty_full(recv);
        let (target, targs) = match kind {
            HK::Adt { def, args } | HK::Shared { def, args } => (def, self.tys().tcx().list(args)),
            HK::Intrinsic { kind, args } => {
                let name = match kind {
                    IntrinsicKind::Vec => "Vec",
                    IntrinsicKind::Map => "Map",
                    IntrinsicKind::Set => "Set",
                    IntrinsicKind::VecDeque => "VecDeque",
                    IntrinsicKind::SortedMap => "SortedMap",
                    IntrinsicKind::SortedSet => "SortedSet",
                };
                (self.lcx.defs.lookup(0, name)?, self.tys().tcx().list(args))
            }
            _ => return None,
        };
        let mut found = self
            .lcx
            .fns
            .iter()
            .filter(|(_, i)| i.impl_target == Some(target) && i.f.name == method);
        let (&d, item) = found.next()?;
        if found.next().is_some()
            || item
                .f
                .generic_params
                .as_ref()
                .is_some_and(|g| !g.params.is_empty())
        {
            return None;
        }
        let _ = base;
        let n = item.impl_params;
        if n > targs.len() {
            return None;
        }
        Some((d, targs[..n].to_vec()))
    }

    fn user_method_call(
        &mut self,
        e: &'a Expr,
        object: &'a Expr,
        d: DefId,
        inst_args: Vec<Ty>,
        args: &'a [CallArg],
        dest: Place,
    ) -> R<()> {
        {
            {
                let f = self.lcx.fns[&d].f;
                if args.len() != f.params.len() {
                    return self.unsupported(e.span, "a call that leaves out default arguments");
                }
                let mode = f.self_param.clone();
                if self.lcx.fns[&d].impl_params == 0 {
                    let rt = self.expr_ty(object)?;
                    let (_, base) = self.strip_ty_full(rt);
                    self.lcx.self_tys.entry(d).or_insert(base);
                }
                let recv = match mode {
                    Some(SelfParam::Owned) => PendingRecv::Ready(self.expr_operand(object)?),
                    Some(SelfParam::Ref) => self.recv_place(object, false)?,
                    Some(SelfParam::MutRef) => self.recv_place(object, true)?,
                    None => return self.unsupported(e.span, "an associated function as a method"),
                };
                let mut rest = Vec::with_capacity(args.len());
                for (a, p) in args.iter().zip(&f.params) {
                    let pt = self.callee_param_ty(p, &inst_args)?;
                    rest.push(self.arg_operand(&a.value, pt)?);
                }
                let mut ops = vec![self.recv_borrow(recv)];
                ops.extend(rest);
                let name = self.lcx.instance(d, inst_args.clone());
                let func = self.fn_operand(&name, d, inst_args);
                let next = self.b.new_block();
                self.goto_with(
                    TerminatorKind::Call {
                        func,
                        args: ops,
                        destination: dest,
                        target: Some(next),
                        unwind: UnwindAction::Abort,
                    },
                    next,
                );
                Ok(())
            }
        }
    }

    /// Give the untyped arguments of a library method (`v.push(None)`)
    /// the parameter type the receiver implies.
    fn hint_lib_args(&mut self, recv: Ty, method: &str, args: &[CallArg]) {
        let HK::Intrinsic { kind, args: targs } = self.tys().tcx().kind(recv) else {
            return;
        };
        let targs = self.tys().tcx().list(targs);
        let usize_t = self.tys().tcx().intern(HK::UInt(UIntSize::Usize));
        let params: Vec<Ty> = match (kind, method) {
            (IntrinsicKind::Vec, "push" | "contains") => vec![targs[0]],
            (IntrinsicKind::Vec, "insert") => vec![usize_t, targs[0]],
            (IntrinsicKind::Map, "insert") => vec![targs[0], targs[1]],
            (IntrinsicKind::Map, "get" | "contains_key" | "remove") => vec![targs[0]],
            (IntrinsicKind::Set, "insert" | "contains" | "remove") => vec![targs[0]],
            _ => return,
        };
        for (a, p) in args.iter().zip(params) {
            if !self.lcx.node_types.contains_key(&a.value.id) {
                self.ty_hints.insert(a.value.id, p);
            }
        }
    }

    /// The ambient resource a lowercase module receiver names (`env` is
    /// `Env`), unless a local shadows it.
    fn ambient_module(&self, object: &Expr) -> Option<&'static str> {
        let ExprKind::Identifier(name) = &object.kind else {
            return None;
        };
        if matches!(self.lcx.res.get(&object.id), Some(Res::Local(_))) {
            return None;
        }
        Some(match name.as_str() {
            "env" => "Env",
            "clock" => "Clock",
            "rand" => "RandomSource",
            "stdin" => "Stdin",
            "stdout" => "Stdout",
            "stderr" => "Stderr",
            "fs" => "FileSystem",
            _ => return None,
        })
    }

    /// A reference to the receiver, unless it already is one.
    fn recv_ref(&mut self, object: &'a Expr, mutable: bool) -> R<Operand> {
        let pending = self.recv_place(object, mutable)?;
        Ok(self.recv_borrow(pending))
    }

    /// The first half of a receiver borrow: evaluate the receiver's place,
    /// leaving the borrow itself to `recv_borrow`, which the caller emits
    /// after the arguments (core semantics §5.6, two-phase borrows).
    fn recv_place(&mut self, object: &'a Expr, mutable: bool) -> R<PendingRecv> {
        let t = self.expr_ty(object)?;
        if let HK::Ref(inner) | HK::MutRef(inner) = self.tys().tcx().kind(t) {
            // A receiver that is already a reference is reborrowed, so a
            // `mut ref self` call through a `ref` reaches the borrow check
            // as the `&mut (*r)` it is (§5.9).
            if !self.is_place(object) {
                return Ok(PendingRecv::Ready(self.expr_operand(object)?));
            }
            let p = self.expr_place(object, mutable)?.project(ProjElem::Deref);
            return Ok(PendingRecv::Borrow(p, inner, mutable));
        }
        let p = self.expr_place(object, mutable)?;
        Ok(PendingRecv::Borrow(p, t, mutable))
    }

    fn recv_borrow(&mut self, pending: PendingRecv) -> Operand {
        let (p, t, mutable) = match pending {
            PendingRecv::Ready(op) => return op,
            PendingRecv::Borrow(p, t, m) => (p, t, m),
        };
        let rt = self.tys().tcx().reference(t, mutable);
        let r = self.temp(rt);
        let kind = if mutable {
            BorrowKind::Mut
        } else {
            BorrowKind::Shared
        };
        self.assign(r, Rvalue::Ref(kind, p));
        Operand::Move(Place::local(r))
    }

    // ── function values ─────────────────────────────────────────────

    /// Whether the checker gave node `id` a function type.
    fn is_fn_typed(&self, id: NodeId) -> bool {
        self.lcx
            .node_types
            .get(&id)
            .is_some_and(|&t| matches!(self.tys().tcx().kind(t), HK::Fn { .. }))
    }

    /// A function item as a value: a constant of its `FnDef` type.
    fn fn_item_value(&mut self, span: Span, d: DefId) -> R<(Operand, Ty)> {
        let generic = self.lcx.fns.get(&d).is_some_and(|i| {
            i.f.generic_params
                .as_ref()
                .is_some_and(|g| !g.params.is_empty())
        });
        if generic {
            return self.unsupported(span, "a generic function as a value");
        }
        let name = self.lcx.instance(d, Vec::new());
        let t = {
            let tcx = self.tys().tcx();
            let none = tcx.intern_list(&[]);
            tcx.intern(HK::FnDef { def: d, args: none })
        };
        let op = Operand::Const(Const {
            ty: t,
            kind: ConstKind::FnDef(InstanceId {
                def: d,
                args: Vec::new(),
                name,
            }),
        });
        Ok((op, t))
    }

    /// A function-typed value: a closure literal, a function item, or a
    /// local holding one (moved, or copied when it is `Copy`).
    fn fn_value(&mut self, e: &'a Expr) -> R<(Operand, Ty)> {
        match &e.kind {
            ExprKind::Closure { .. } => self.closure_value(e),
            ExprKind::Identifier(_) | ExprKind::Path { .. } => {
                if let Some(Res::Def(d)) = self.lcx.res.get(&e.id) {
                    return self.fn_item_value(e.span, *d);
                }
                let p = self.expr_place(e, false)?;
                let t = self.place_type(&p);
                Ok((self.use_place(p, t), t))
            }
            _ => self.unsupported(e.span, "this function value"),
        }
    }

    /// The symbols a closure body reads from its surroundings, in the order
    /// they first appear (core semantics §9.4).
    fn closure_captures(&self, body: &Expr) -> Vec<SymbolId> {
        let mut out = Vec::new();
        for id in crate::span_visitor::expr_node_ids(body) {
            if let Some(Res::Local(sym)) = self.lcx.res.get(&id) {
                let outer = self.locals.contains_key(sym) || self.captured.contains_key(sym);
                if outer && !out.contains(sym) {
                    out.push(*sym);
                }
            }
        }
        out
    }

    /// The captures in `caps` that `body` moves: a non-`Copy` capture used
    /// where a value is consumed (an owned argument or receiver, a `let`,
    /// a return, a field of a new value, the closure's result).
    fn moved_captures(&self, body: &'a Expr, caps: &[SymbolId]) -> FxHashSet<SymbolId> {
        let mut out = FxHashSet::default();
        self.mv_expr(body, true, caps, &mut out);
        out
    }

    fn owned_node(&self, id: NodeId) -> bool {
        self.lcx.node_types.get(&id).is_some_and(|&t| {
            !self.is_copy(t)
                && !matches!(
                    self.tys().tcx().kind(t),
                    HK::Shared { .. } | HK::Ref(_) | HK::MutRef(_)
                )
        })
    }

    /// Whether the user function `d`'s parameter `i` takes its argument by
    /// value (not as a reference or a slice view).
    fn owned_param(&self, d: DefId, i: usize) -> bool {
        self.lcx.fns.get(&d).is_some_and(|item| {
            item.f.params.get(i).is_some_and(|p| {
                self.lcx.node_types.get(&p.pattern.id).is_some_and(|&t| {
                    // A slice parameter views its argument.
                    !matches!(
                        self.tys().tcx().kind(t),
                        HK::Ref(_) | HK::MutRef(_) | HK::Slice { .. }
                    )
                })
            })
        })
    }

    fn mv_block(&self, b: &'a Block, ctx: bool, caps: &[SymbolId], out: &mut FxHashSet<SymbolId>) {
        for st in &b.stmts {
            match &st.kind {
                StmtKind::Let { value, .. } => self.mv_expr(value, true, caps, out),
                StmtKind::Assign { target, value } => {
                    self.mv_expr(target, false, caps, out);
                    self.mv_expr(value, true, caps, out);
                }
                StmtKind::CompoundAssign { target, value, .. } => {
                    self.mv_expr(target, false, caps, out);
                    self.mv_expr(value, false, caps, out);
                }
                StmtKind::Expr(e) => self.mv_expr(e, false, caps, out),
                StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
                    self.mv_block(body, false, caps, out)
                }
                _ => {}
            }
        }
        if let Some(e) = &b.final_expr {
            self.mv_expr(e, ctx, caps, out);
        }
    }

    fn mv_expr(&self, e: &'a Expr, ctx: bool, caps: &[SymbolId], out: &mut FxHashSet<SymbolId>) {
        let ctx = ctx && self.owned_node(e.id);
        let go =
            |x: &'a Expr, c: bool, out: &mut FxHashSet<SymbolId>| self.mv_expr(x, c, caps, out);
        match &e.kind {
            ExprKind::Identifier(_) => {
                if let Some(Res::Local(sym)) = self.lcx.res.get(&e.id) {
                    if ctx && caps.contains(sym) {
                        out.insert(*sym);
                    }
                }
            }
            ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
                go(object, ctx, out)
            }
            ExprKind::Index { object, index } => {
                go(object, false, out);
                go(index, false, out);
            }
            ExprKind::Call { callee, args } => {
                let def = match self.lcx.calls.get(&e.id).map(|c| &c.callee) {
                    Some(Callee::Def(d)) => Some(*d),
                    _ => match self.lcx.res.get(&callee.id) {
                        Some(Res::Def(d)) => Some(*d),
                        _ => None,
                    },
                };
                let variant =
                    def.is_some_and(|d| self.lcx.defs.table.get(d).kind == DefKind::Variant);
                go(callee, false, out);
                for (i, a) in args.iter().enumerate() {
                    let owned = variant || def.is_some_and(|d| self.owned_param(d, i));
                    go(&a.value, owned, out);
                }
            }
            ExprKind::MethodCall {
                object,
                method,
                args,
                ..
            } => {
                let def = match self.lcx.calls.get(&e.id).map(|c| &c.callee) {
                    Some(Callee::Def(d)) if self.lcx.fns.contains_key(d) => Some(*d),
                    _ => None,
                };
                let recv_owned = def.is_some_and(|d| {
                    matches!(self.lcx.fns[&d].f.self_param, Some(SelfParam::Owned))
                });
                go(object, recv_owned, out);
                let stores = matches!(
                    method.as_str(),
                    "push" | "push_back" | "push_front" | "insert" | "extend" | "append" | "set"
                );
                for (i, a) in args.iter().enumerate() {
                    let owned = match def {
                        Some(d) => self.owned_param(d, i),
                        None => stores,
                    };
                    go(&a.value, owned, out);
                }
            }
            ExprKind::StructLiteral { fields, spread, .. } => {
                for f in fields {
                    go(&f.value, true, out);
                }
                if let Some(s) = spread {
                    go(s, true, out);
                }
            }
            ExprKind::Tuple(es) | ExprKind::ArrayLiteral(es) => {
                for x in es {
                    go(x, true, out);
                }
            }
            ExprKind::Block(b) => self.mv_block(b, ctx, caps, out),
            ExprKind::If {
                condition,
                then_block,
                else_branch,
            } => {
                go(condition, false, out);
                self.mv_block(then_block, ctx, caps, out);
                if let Some(x) = else_branch {
                    go(x, ctx, out);
                }
            }
            ExprKind::IfLet {
                value,
                then_block,
                else_branch,
                ..
            } => {
                go(value, false, out);
                self.mv_block(then_block, ctx, caps, out);
                if let Some(x) = else_branch {
                    go(x, ctx, out);
                }
            }
            ExprKind::Match { scrutinee, arms } => {
                go(scrutinee, false, out);
                for arm in arms {
                    if let Some(g) = &arm.guard {
                        go(g, false, out);
                    }
                    go(&arm.body, ctx, out);
                }
            }
            ExprKind::While {
                condition, body, ..
            } => {
                go(condition, false, out);
                self.mv_block(body, false, caps, out);
            }
            ExprKind::WhileLet { value, body, .. } => {
                go(value, false, out);
                self.mv_block(body, false, caps, out);
            }
            ExprKind::For { iterable, body, .. } => {
                go(iterable, false, out);
                self.mv_block(body, false, caps, out);
            }
            ExprKind::Loop { body, .. } => self.mv_block(body, false, caps, out),
            ExprKind::Closure { body, .. } => go(body, true, out),
            ExprKind::Return(Some(x)) | ExprKind::Question(x) => go(x, true, out),
            ExprKind::Break { value: Some(x), .. } => go(x, true, out),
            ExprKind::Binary { left, right, .. } => {
                go(left, false, out);
                go(right, false, out);
            }
            ExprKind::Unary { operand, .. } => go(operand, false, out),
            ExprKind::Cast { expr, .. } => go(expr, false, out),
            ExprKind::InterpolatedStringLit(parts) => {
                for p in parts {
                    if let ParsedInterpolationPart::Expr(x, _) = p {
                        go(x, false, out);
                    }
                }
            }
            _ => {}
        }
    }

    /// Create a closure: its environment is an aggregate of the captures,
    /// each moved, copied or borrowed by the mode the body needs (§9.1).
    fn closure_value(&mut self, e: &'a Expr) -> R<(Operand, Ty)> {
        let ExprKind::Closure {
            body, capture_mode, ..
        } = &e.kind
        else {
            return self.unsupported(e.span, "this closure");
        };
        let once = match self
            .lcx
            .node_types
            .get(&e.id)
            .map(|&t| self.tys().tcx().kind(t))
        {
            Some(HK::Fn { once, .. }) => once,
            _ => return self.unsupported(e.span, "a closure with no function type"),
        };
        let legacy = self
            .lcx
            .capture_modes
            .get(&SpanKey::from_span(&e.span))
            .cloned()
            .unwrap_or_default();
        let mut caps = Vec::new();
        let mut ops = Vec::new();
        let captured = self.closure_captures(body);
        let moved = self.moved_captures(body, &captured);
        for sym in captured {
            let place = match self.locals.get(&sym) {
                Some(&l) => Place::local(l),
                None => self.captured[&sym].clone(),
            };
            let t = self.place_type(&place);
            let name = self
                .lcx
                .binding_syms
                .iter()
                .find(|(_, s)| **s == sym)
                .map(|((_, n), _)| n.clone())
                .unwrap_or_default();
            let declared = match capture_mode {
                Some(ast::CaptureMode::Own) => Some(OwnershipMode::Own),
                Some(ast::CaptureMode::Ref) => Some(OwnershipMode::Ref),
                Some(ast::CaptureMode::MutRef) => Some(OwnershipMode::MutRef),
                None => None,
            };
            let explicit = declared.is_some();
            let mode = declared.or_else(|| {
                legacy
                    .iter()
                    .find(|(n, _)| *n == name)
                    .map(|(_, m)| m.clone())
            });
            // A `Copy` place is copied in unless the body assigns to it. A
            // once-callable closure moves something out of its captures, so
            // its non-`Copy` captures move in (core semantics §9.1).
            let cm = match mode {
                Some(OwnershipMode::MutRef) => CapMode::Mut,
                Some(OwnershipMode::Own) => CapMode::Value,
                _ if self.is_copy(t) => CapMode::Value,
                None | Some(OwnershipMode::Ref) if (once || moved.contains(&sym)) && !explicit => {
                    CapMode::Value
                }
                _ => CapMode::Ref,
            };
            let (ft, op) = match cm {
                CapMode::Value => (t, self.use_place(place, t)),
                CapMode::Ref | CapMode::Mut => {
                    let mutable = cm == CapMode::Mut;
                    let rt = self.tys().tcx().reference(t, mutable);
                    let r = self.temp(rt);
                    let kind = if mutable {
                        BorrowKind::Mut
                    } else {
                        BorrowKind::Shared
                    };
                    self.assign(r, Rvalue::Ref(kind, place));
                    (rt, Operand::Move(Place::local(r)))
                }
            };
            caps.push((sym, cm, ft));
            ops.push(op);
        }
        let env = if (once || !moved.is_empty())
            && caps
                .iter()
                .any(|&(_, m, t)| m == CapMode::Value && !self.is_copy(t))
        {
            EnvMode::Value
        } else if caps.iter().any(|&(_, m, _)| m == CapMode::Mut) {
            EnvMode::Mut
        } else {
            EnvMode::Ref
        };
        let def = DefId(self.lcx.next_closure);
        self.lcx.next_closure += 1;
        let ty = {
            let tcx = self.tys().tcx();
            let list: Vec<Ty> = caps.iter().map(|&(_, _, t)| t).collect();
            let captures = tcx.intern_list(&list);
            tcx.intern(HK::Closure { def, captures })
        };
        let name = format!("{}.c{}", self.name, self.closure_count);
        self.closure_count += 1;
        self.lcx.closures.insert(def, (name.clone(), env));
        self.lcx.closure_exprs.insert(def, e);
        self.lcx.closure_queue.push(ClosureJob {
            def,
            name,
            expr: e,
            args: self.args.clone(),
            caps,
            env,
            ty,
        });
        let l = self.scoped_temp(ty);
        self.assign(l, Rvalue::Aggregate(AggregateKind::Closure { ty }, ops));
        Ok((Operand::Move(Place::local(l)), ty))
    }

    /// The body of a closure: `_1` is the environment, then the parameters.
    fn lower_closure_body(
        &mut self,
        job: &ClosureJob<'a>,
        params: &'a [ast::ClosureParam],
        body: &'a Expr,
    ) -> R<()> {
        self.push_scope();
        let env_ty = match job.env {
            EnvMode::Ref => self.tys().tcx().reference(job.ty, false),
            EnvMode::Mut => self.tys().tcx().reference(job.ty, true),
            EnvMode::Value => job.ty,
        };
        let env = self.b.arg("env", env_ty);
        let base = match job.env {
            EnvMode::Value => {
                self.schedule(ScopeEntry::Drop(Place::local(env)));
                Place::local(env)
            }
            _ => Place::local(env).project(ProjElem::Deref),
        };
        for (k, &(sym, cm, ft)) in job.caps.iter().enumerate() {
            let p = base.field(k as u32, ft);
            let p = match cm {
                CapMode::Value => p,
                CapMode::Ref | CapMode::Mut => p.project(ProjElem::Deref),
            };
            self.captured.insert(sym, p);
        }
        for p in params {
            let PatternKind::Binding(name) = &p.pattern.kind else {
                return self.unsupported(p.span, "a destructuring closure parameter");
            };
            let t = self.node_ty(p.pattern.id, p.span)?;
            let l = self.b.arg(name, t);
            if let Some(&sym) = self.lcx.binding_syms.get(&(p.pattern.id, name.clone())) {
                self.locals.insert(sym, l);
            }
            if self.needs_drop(t) {
                self.schedule(ScopeEntry::Drop(Place::local(l)));
            }
        }
        let ret = Place::local(Local::RETURN_PLACE);
        self.push_scope();
        self.expr_into(body, ret)?;
        self.pop_scope()?;
        self.return_exit()?;
        self.scopes.clear();
        Ok(())
    }

    /// The environment argument for calling the closure value `p` (of type
    /// `t`: the closure or a reference to it), as its body takes it.
    fn env_operand(&mut self, span: Span, p: Place, t: Ty) -> R<(Operand, Ty, String)> {
        let tcx = self.tys().tcx();
        let (inner, through) = match tcx.kind(t) {
            HK::Ref(i) => (i, Some(false)),
            HK::MutRef(i) => (i, Some(true)),
            _ => (t, None),
        };
        let HK::Closure { def, .. } = tcx.kind(inner) else {
            return self.unsupported(span, "a call through this value");
        };
        let Some((name, env)) = self.lcx.closures.get(&def).cloned() else {
            return self.unsupported(span, "a call to an unknown closure");
        };
        let target = match through {
            Some(_) => p.clone().project(ProjElem::Deref),
            None => p.clone(),
        };
        let op = match (env, through) {
            (EnvMode::Ref, Some(false)) => Operand::Copy(p),
            (EnvMode::Ref, _) | (EnvMode::Mut, None | Some(true)) => {
                let mutable = env == EnvMode::Mut;
                let rt = self.tys().tcx().reference(inner, mutable);
                let r = self.temp(rt);
                let kind = if mutable {
                    BorrowKind::Mut
                } else {
                    BorrowKind::Shared
                };
                self.assign(r, Rvalue::Ref(kind, target));
                Operand::Move(Place::local(r))
            }
            (EnvMode::Value, None) => self.use_place(target, inner),
            _ => return self.unsupported(span, "calling this closure through a reference"),
        };
        Ok((op, inner, name))
    }

    /// `f(args)` where `f` is a value: a closure (its body, with the
    /// environment first) or a function item (a direct call).
    fn call_value(
        &mut self,
        e: &'a Expr,
        callee: &'a Expr,
        args: &'a [CallArg],
        dest: Place,
    ) -> R<()> {
        let ct = self.expr_ty(callee)?;
        let peeled = match self.tys().tcx().kind(ct) {
            HK::Ref(i) | HK::MutRef(i) => i,
            _ => ct,
        };
        let (func, mut ops, param_tys) = match self.tys().tcx().kind(peeled) {
            HK::FnDef { def, .. } => {
                let Some(f) = self.lcx.fns.get(&def).map(|i| i.f) else {
                    return self.unsupported(e.span, "a call to this function value");
                };
                let mut pts = Vec::new();
                for p in &f.params {
                    pts.push(self.callee_param_ty(p, &[])?);
                }
                let name = self.lcx.instance(def, Vec::new());
                (self.fn_operand(&name, def, Vec::new()), Vec::new(), pts)
            }
            HK::Closure { def, .. } => {
                let p = self.expr_place(callee, false)?;
                let (env, _, name) = self.env_operand(callee.span, p, ct)?;
                let Some(job_params) = self.closure_param_tys(def) else {
                    return self.unsupported(e.span, "a call to this closure");
                };
                (
                    self.fn_operand(&name, def, self.args.clone()),
                    vec![env],
                    job_params,
                )
            }
            _ => return self.unsupported(e.span, "a call through a function value"),
        };
        if param_tys.len() != args.len() {
            return self.unsupported(e.span, "a call through a value with the wrong arity");
        }
        for (a, pt) in args.iter().zip(param_tys) {
            ops.push(self.arg_operand(&a.value, pt)?);
        }
        let next = self.b.new_block();
        self.goto_with(
            TerminatorKind::Call {
                func,
                args: ops,
                destination: dest,
                target: Some(next),
                unwind: UnwindAction::Abort,
            },
            next,
        );
        Ok(())
    }

    /// A closure's parameter types, from its checked function type.
    fn closure_param_tys(&mut self, def: DefId) -> Option<Vec<Ty>> {
        let expr = *self.lcx.closure_exprs.get(&def)?;
        let t = *self.lcx.node_types.get(&expr.id)?;
        let HK::Fn { params, .. } = self.tys().tcx().kind(t) else {
            return None;
        };
        let params = self.tys().tcx().list(params).to_vec();
        let args = self.args.clone();
        params
            .into_iter()
            .map(|p| self.lcx.mir_ty(p, &args).ok())
            .collect()
    }

    /// An argument for a function-typed parameter: the closure borrowed
    /// the way its body takes its environment (moved when called once), or
    /// the function item as a constant. The parameter then has the type
    /// returned here in that instance of the callee.
    fn fn_arg(&mut self, a: &'a Expr) -> R<(Operand, Ty)> {
        let (op, t) = match &a.kind {
            ExprKind::Closure { .. } => {
                let (op, t) = self.closure_value(a)?;
                let Operand::Move(p) = op else { unreachable!() };
                (p, t)
            }
            ExprKind::Identifier(_) | ExprKind::Path { .. }
                if matches!(self.lcx.res.get(&a.id), Some(Res::Def(_))) =>
            {
                let Some(Res::Def(d)) = self.lcx.res.get(&a.id) else {
                    unreachable!()
                };
                return self.fn_item_value(a.span, *d);
            }
            _ => {
                let p = self.expr_place(a, false)?;
                let t = self.place_type(&p);
                (p, t)
            }
        };
        let tcx = self.tys().tcx();
        let inner = match tcx.kind(t) {
            HK::Ref(i) | HK::MutRef(i) => i,
            _ => t,
        };
        match tcx.kind(inner) {
            HK::FnDef { .. } => Ok((self.use_place(op, t), t)),
            HK::Closure { def, .. } => {
                let env = self.lcx.closures.get(&def).map(|c| c.1);
                match (env, tcx.kind(t)) {
                    // Already a reference of the right kind: pass it on.
                    (Some(EnvMode::Ref), HK::Ref(_)) | (Some(EnvMode::Mut), HK::MutRef(_)) => {
                        Ok((Operand::Copy(op), t))
                    }
                    (Some(EnvMode::Value), _) => Ok((self.use_place(op, t), t)),
                    (Some(mode), _) => {
                        let mutable = mode == EnvMode::Mut;
                        let target = if t == inner {
                            op
                        } else {
                            op.project(ProjElem::Deref)
                        };
                        let rt = self.tys().tcx().reference(inner, mutable);
                        let r = self.temp(rt);
                        let kind = if mutable {
                            BorrowKind::Mut
                        } else {
                            BorrowKind::Shared
                        };
                        self.assign(r, Rvalue::Ref(kind, target));
                        Ok((Operand::Move(Place::local(r)), rt))
                    }
                    (None, _) => self.unsupported(a.span, "passing an unknown closure"),
                }
            }
            _ => self.unsupported(a.span, "this function argument"),
        }
    }

    /// An argument to a library method: by value, except that a borrowing
    /// parameter (`push_str`'s) takes a string literal as a constant and any
    /// other value by reference.
    fn lib_arg(&mut self, a: &'a Expr, borrows: bool) -> R<Operand> {
        if !borrows {
            return self.expr_operand(a);
        }
        if let ExprKind::StringLit(s) | ExprKind::MultiStringLit(s) = &a.kind {
            return Ok(self.static_str(s));
        }
        let at = self.expr_ty(a)?;
        match self.tys().tcx().kind(at) {
            // A `mut ref` place lends a shared reborrow, `&(*r)`, so the
            // caller keeps its reference.
            HK::MutRef(inner) if self.is_place(a) => {
                let p = self.expr_place(a, false)?.project(ProjElem::Deref);
                let rt = self.tys().tcx().reference(inner, false);
                let r = self.temp(rt);
                self.assign(r, Rvalue::Ref(BorrowKind::Shared, p));
                return Ok(Operand::Move(Place::local(r)));
            }
            HK::Ref(_) | HK::MutRef(_) => return self.expr_operand(a),
            _ => {}
        }
        let p = self.expr_place(a, false)?;
        let rt = self.tys().tcx().reference(at, false);
        let r = self.temp(rt);
        self.assign(r, Rvalue::Ref(BorrowKind::Shared, p));
        Ok(Operand::Move(Place::local(r)))
    }

    /// The `Option`/`Result` helpers the stdlib will define in Kāra, lowered
    /// in place until it does: `is_some`/`is_none`/`is_ok`/`is_err` read the
    /// discriminant; `unwrap`/`expect`/`unwrap_or` take the receiver by value
    /// and move the payload out. `false` when `method` is not one of them.
    fn option_method(
        &mut self,
        object: &'a Expr,
        method: &str,
        args: &'a [CallArg],
        dest: Place,
    ) -> R<bool> {
        let ot = self.expr_ty(object)?;
        let (base, through_ref) = match self.tys().tcx().kind(ot) {
            HK::Ref(t) | HK::MutRef(t) => (t, true),
            _ => (ot, false),
        };
        let Some((adt, _)) = self.tys().tcx().adt_of(base) else {
            return Ok(false);
        };
        let (ok_name, err_name) = match adt.name.as_str() {
            "Option" => ("Some", "None"),
            "Result" => ("Ok", "Err"),
            _ => return Ok(false),
        };
        let idx = |n: &str| {
            adt.variants
                .iter()
                .position(|v| v.name == n)
                .map(|i| i as u32)
        };
        let (Some(ok), Some(err)) = (idx(ok_name), idx(err_name)) else {
            return Ok(false);
        };
        let by_value = matches!(method, "unwrap" | "expect" | "unwrap_or");
        if !by_value && !matches!(method, "is_some" | "is_ok" | "is_none" | "is_err") {
            return Ok(false);
        }
        if by_value && through_ref {
            return Ok(false);
        }
        let p = if by_value {
            if self.is_place(object) && self.is_local_rooted(object) {
                self.expr_place(object, false)?
            } else {
                let l = self.scoped_temp(base);
                self.expr_into(object, Place::local(l))?;
                Place::local(l)
            }
        } else {
            self.deref_place(object, false)?.0
        };
        // `unwrap_or`'s argument is evaluated before the test, and dropped
        // at the statement's end when the payload is taken instead.
        let default = match (method, args) {
            ("unwrap_or", [a]) => {
                let l = self.scoped_temp(base_payload_or(self, base, ok));
                self.expr_into(&a.value, Place::local(l))?;
                Some(l)
            }
            _ => None,
        };
        let i64_t = self.tys().tcx().intern(HK::Int(IntSize::I64));
        let d = self.temp(i64_t);
        self.assign(d, Rvalue::Discriminant(p.clone()));
        if !by_value {
            let want = if matches!(method, "is_some" | "is_ok") {
                ok
            } else {
                err
            };
            self.assign(
                dest,
                Rvalue::BinaryOp(
                    BinOp::Eq,
                    Operand::Copy(Place::local(d)),
                    Operand::Const(Const {
                        ty: i64_t,
                        kind: ConstKind::Scalar(want as u128),
                    }),
                ),
            );
            return Ok(true);
        }
        let ok_bb = self.b.new_block();
        let other = self.b.new_block();
        let join = self.b.new_block();
        self.goto_with(
            TerminatorKind::SwitchInt {
                discr: Operand::Copy(Place::local(d)),
                targets: SwitchTargets {
                    values: vec![(ok as u128, ok_bb)],
                    otherwise: other,
                },
            },
            ok_bb,
        );
        match self.tys().tcx().field_ty(base, Some(ok), 0) {
            Some(pt) => {
                let fp = p.project(ProjElem::Downcast(VariantIdx(ok))).field(0, pt);
                let op = self.use_place(fp, pt);
                self.assign(dest.clone(), Rvalue::Use(op));
            }
            None => self.assign(dest.clone(), Rvalue::Use(unit_const(self.unit()))),
        }
        self.goto(join);
        self.cur = other;
        match default {
            Some(l) => {
                let t = self.b.local_ty(l);
                let op = self.use_place(Place::local(l), t);
                self.assign(dest, Rvalue::Use(op));
                self.goto(join);
            }
            None => self.diverge(TerminatorKind::Abort {
                reason: AbortReason::Panic,
            }),
        }
        self.cur = join;
        Ok(true)
    }

    /// A library method the interpreter implements natively.
    fn builtin_method(
        &mut self,
        e: &'a Expr,
        key: &str,
        object: &'a Expr,
        method: &str,
        args: &'a [CallArg],
        dest: Place,
    ) -> R<()> {
        let ot = self.expr_ty(object)?;
        let base = match self.tys().tcx().kind(ot) {
            HK::Ref(t) | HK::MutRef(t) => t,
            _ => ot,
        };
        match (key, method) {
            (_, "as_slice" | "as_mut_slice")
                if matches!(
                    self.tys().tcx().kind(base),
                    HK::Array { .. }
                        | HK::Intrinsic {
                            kind: IntrinsicKind::Vec,
                            ..
                        }
                ) =>
            {
                let t = self.expr_ty(e)?;
                let s = self.slice_view(object, None, t, method == "as_mut_slice")?;
                self.assign(dest, Rvalue::Use(Operand::Move(Place::local(s))));
                Ok(())
            }
            (_, "or_insert" | "or_insert_with" | "or_default") if matches!(&object.kind, ExprKind::MethodCall { method: m, .. } if m == "entry") =>
            {
                // `m.entry(k).or_insert(v)` is one call of the map's
                // `entry_or_insert(mut ref m, k, v)`, which returns a
                // `mut ref` to the value.
                let ExprKind::MethodCall {
                    object: map,
                    args: key,
                    ..
                } = &object.kind
                else {
                    unreachable!()
                };
                let mt = self.expr_ty(map)?;
                let (_, mbase) = self.strip_ty_full(mt);
                let recv = self.recv_place(map, true)?;
                let mut rest = Vec::new();
                for a in key.iter().chain(args) {
                    if self.is_fn_typed(a.value.id)
                        || matches!(a.value.kind, ExprKind::Closure { .. })
                    {
                        let (op, _) = self.fn_value(&a.value)?;
                        rest.push(op);
                    } else {
                        rest.push(self.expr_operand(&a.value)?);
                    }
                }
                let mut ops = vec![self.recv_borrow(recv)];
                ops.extend(rest);
                let name = format!("{}.entry_{method}", self.tys().display(mbase));
                self.call_native(&name, ops, dest);
                Ok(())
            }
            (_, "to_string") if matches!(&object.kind, ExprKind::StringLit(_)) => {
                let ExprKind::StringLit(s) = &object.kind else {
                    unreachable!()
                };
                let op = self.static_str(s);
                self.call_native("String.from", vec![op], dest);
                Ok(())
            }
            (_, "to_string") if self.is_copy(base) => {
                let recv = self.expr_operand(object)?;
                let recv = match self.tys().tcx().kind(ot) {
                    HK::Ref(_) | HK::MutRef(_) => {
                        let p = self.expr_place(object, false)?.project(ProjElem::Deref);
                        Operand::Copy(p)
                    }
                    _ => recv,
                };
                let name = format!("{}.to_string", self.tys().display(base));
                self.call_native(&name, vec![recv], dest);
                Ok(())
            }
            (_, "to_string") if matches!(self.tys().tcx().kind(base), HK::Str) => {
                let recv = self.recv_ref(object, false)?;
                self.call_native("String.clone", vec![recv], dest);
                Ok(())
            }
            _ => {
                // Any other library method is a call of the native
                // `<receiver type>.<method>`. A number takes its receiver by
                // value; a collection by `mut ref` when the method writes it,
                // else by `ref`.
                let number = matches!(
                    self.tys().tcx().kind(base),
                    HK::Int(_) | HK::UInt(_) | HK::Float(_) | HK::Bool | HK::Char
                );
                let recv = if number {
                    let op = self.expr_operand(object)?;
                    PendingRecv::Ready(match self.tys().tcx().kind(ot) {
                        HK::Ref(_) | HK::MutRef(_) => {
                            Operand::Copy(self.expr_place(object, false)?.project(ProjElem::Deref))
                        }
                        _ => op,
                    })
                } else {
                    let mutates = crate::ast::is_mutating_collection_method(method)
                        || matches!(
                            method,
                            "push_str" | "set" | "sort_unstable" | "sort_unstable_by"
                        );
                    self.recv_place(object, mutates)?
                };
                let mut rest = Vec::with_capacity(args.len());
                self.hint_lib_args(base, method, args);
                // Stored values and closures move in; other non-Copy
                // arguments (keys, needles, slices, text) are borrowed.
                let stores = matches!(
                    method,
                    "push"
                        | "push_back"
                        | "push_front"
                        | "insert"
                        | "extend"
                        | "append"
                        | "resize"
                        | "fill"
                        | "set"
                );
                for a in args {
                    if self.is_fn_typed(a.value.id)
                        || matches!(a.value.kind, ExprKind::Closure { .. })
                    {
                        // A closure or function goes by value; the native
                        // calls it.
                        let (op, _) = self.fn_value(&a.value)?;
                        rest.push(op);
                        continue;
                    }
                    let at = self.expr_ty(&a.value)?;
                    let callable = matches!(self.tys().tcx().kind(at), HK::Closure { .. });
                    let by_ref = !stores && !callable && !self.is_copy(at);
                    rest.push(self.lib_arg(&a.value, by_ref)?);
                }
                let mut ops = vec![self.recv_borrow(recv)];
                ops.extend(rest);
                let name = format!("{}.{method}", self.tys().display(base));
                self.call_native(&name, ops, dest);
                Ok(())
            }
        }
    }
}

/// A method receiver whose borrow is not taken yet: an operand already in
/// hand, or the place to borrow, its type and whether mutably.
enum PendingRecv {
    Ready(Operand),
    Borrow(Place, Ty, bool),
}

enum PendingPlace<'a> {
    Plain(&'a Expr),
    Index(&'a Expr, Operand, &'a Expr),
}

/// Empty every block control cannot reach: the code a builder emits after
/// a `return` or `break` is dead, and need not be well typed (`{ return x; }`
/// as a body still assigns its unit value to the return place).
/// `i64.MAX`, `u8.MIN`, …: the bits of an integer type's limit.
fn int_limit(kind: HK, path: &str) -> Option<u128> {
    let limit = path.rsplit('.').next()?;
    let (bits, signed) = match kind {
        HK::Int(s) => (
            match s {
                IntSize::I8 => 8,
                IntSize::I16 => 16,
                IntSize::I32 => 32,
                IntSize::I128 => 128,
                _ => 64,
            },
            true,
        ),
        HK::UInt(s) => (
            match s {
                UIntSize::U8 => 8,
                UIntSize::U16 => 16,
                UIntSize::U32 => 32,
                UIntSize::U128 => 128,
                _ => 64,
            },
            false,
        ),
        _ => return None,
    };
    let v: i128 = match (limit, signed) {
        ("MAX", true) => (1i128 << (bits - 1)).wrapping_sub(1),
        ("MIN", true) => (-1i128).wrapping_shl(bits - 1),
        ("MAX", false) if bits == 128 => return Some(u128::MAX),
        ("MAX", false) => (1i128 << bits) - 1,
        ("MIN", false) => 0,
        _ => return None,
    };
    Some(v as u128)
}

fn bool_const(t: Ty, b: bool) -> Operand {
    Operand::Const(Const {
        ty: t,
        kind: ConstKind::Scalar(b as u128),
    })
}

fn clear_unreachable_blocks(body: &mut Body) {
    let mut seen = vec![false; body.blocks.len()];
    let mut work = vec![0usize];
    while let Some(b) = work.pop() {
        if std::mem::replace(&mut seen[b], true) {
            continue;
        }
        for s in body.blocks[b].terminator.kind.successors() {
            work.push(s.index());
        }
    }
    for (b, data) in body.blocks.iter_mut().enumerate() {
        if !seen[b] {
            data.statements.clear();
            data.terminator.kind = TerminatorKind::Unreachable;
        }
    }
}

/// The type of `base`'s payload in variant `ok`, or unit.
fn base_payload_or(bx: &Bx<'_, '_>, base: Ty, ok: u32) -> Ty {
    bx.tys()
        .tcx()
        .field_ty(base, Some(ok), 0)
        .unwrap_or_else(|| bx.unit())
}

/// Whether `e` names an existing place rather than producing a value.
fn is_place_expr(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Identifier(_) | ExprKind::SelfValue => true,
        ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
            is_place_expr(object)
        }
        ExprKind::Unary {
            op: UnaryOp::Deref, ..
        } => true,
        _ => false,
    }
}

fn unit_const(unit: Ty) -> Operand {
    Operand::Const(Const {
        ty: unit,
        kind: ConstKind::Unit,
    })
}

/// Check one source file, build its MIR, elaborate drops and run `main`
/// on the MIR interpreter; or the first stage that refused it.
pub fn run_source(src: &str) -> Result<interp::RunResult, String> {
    let parsed = crate::parse(src);
    if !parsed.errors.is_empty() {
        return Err(format!("parse: {:?}", parsed.errors[0]));
    }
    let mut program = parsed.program;
    crate::prepare_for_resolve(&mut program);
    let r = crate::resolve(&program);
    if !r.errors.is_empty() {
        return Err(format!("resolve: {:?}", r.errors[0]));
    }
    let tc = crate::typecheck(&program, &r);
    if !tc.errors.is_empty() {
        return Err(format!("typecheck: {}", tc.errors[0].message));
    }
    let defs = ProgramDefs::build_for_program(&program);
    let res = crate::node_res::node_res(&r, &defs, 0, None);
    let hir = crate::typed_hir::build(&tc, &crate::typed_hir::ProgramHirDefs::new(&defs, 0, &res));
    let own = crate::ownershipcheck(&program, &tc);
    let mut lowered = lower_program(&program, &tc, &defs, &res, &r, hir, &own.closure_captures);
    if !lowered.errors.is_empty() {
        return Err(format!("build: {}", lowered.errors.join("; ")));
    }
    // `KARAC_MIR_DUMP=built` or `=elaborated` prints every body to stderr.
    let dump = std::env::var("KARAC_MIR_DUMP").ok();
    let names: FxHashSet<String> = lowered.program.bodies.keys().cloned().collect();
    let borrowck = std::env::var("KARAC_MIR_BORROWCK").as_deref() != Ok("0");
    for body in lowered.program.bodies.values_mut() {
        if dump.as_deref() == Some("built") {
            eprintln!("{}", crate::mir::pretty::pretty_body(body, &lowered.tys));
        }
        crate::mir::check_moves(body, &lowered.tys)
            .map_err(|e| format!("move check {}: {}", body.instance.name, e.join("; ")))?;
        // A library native's result borrows from its first argument, the
        // collection, when it borrows at all. `KARAC_MIR_BORROWCK=0` skips
        // the check.
        if borrowck {
            let has_receiver = |i: &InstanceId| {
                !names.contains(i.name.as_str()) || lowered.receivers.contains(&i.name)
            };
            crate::mir::check_borrows(body, &lowered.tys, &has_receiver)
                .map_err(|e| format!("borrow check {}: {}", body.instance.name, e.join("; ")))?;
        }
        crate::mir::elaborate_drops(body, &mut lowered.tys)
            .map_err(|e| format!("elaborate {}: {e}", body.instance.name))?;
        if dump.as_deref() == Some("elaborated") {
            eprintln!("{}", crate::mir::pretty::pretty_body(body, &lowered.tys));
        }
    }
    Ok(interp::run(&lowered.program, &lowered.tys, "main", vec![]))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Check, build, elaborate and run `src`; the output and exit code, or
    /// the first stage that refused it.
    pub(crate) fn run_source(src: &str) -> Result<(String, Option<i32>), String> {
        let r = super::run_source(src)?;
        if let interp::Outcome::Error(e) = &r.outcome {
            return Err(format!("run: {e}\n--- output so far\n{}", r.output));
        }
        let code = r.exit_code();
        Ok((r.output, code))
    }

    /// Core pins the builder cannot run yet, each with what it waits on.
    /// A listed pin must still fail, so the list only shrinks.
    pub(crate) const NOT_YET: &[&str] = &[
        // `TaskGroup` and `TaskHandle` have no MIR lowering or natives yet.
        "ok_taskgroup_borrows",
    ];

    /// Every runnable core pin, built from source, elaborated and run, prints
    /// its expected output and exits with its expected code.
    #[test]
    fn built_core_pins_match_their_expected_output() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("corpus/core");
        let mut pins: Vec<_> = std::fs::read_dir(&root)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        pins.sort();
        let (mut ran, mut bad) = (0, Vec::new());
        for dir in pins {
            let pin = dir.file_name().unwrap().to_str().unwrap().to_string();
            // The `err_` pins are programs the checker must reject.
            if pin.starts_with("err_") {
                continue;
            }
            let expected = std::fs::read_to_string(dir.join("expected.out")).unwrap();
            let meta = std::fs::read_to_string(dir.join("meta.toml")).unwrap();
            let exit: i32 = meta
                .lines()
                .find_map(|l| l.strip_prefix("exit = "))
                .unwrap()
                .trim()
                .parse()
                .unwrap();
            let src = std::fs::read_to_string(dir.join("source.kara")).unwrap();
            ran += 1;
            let result = run_source(&src);
            let pass =
                matches!(&result, Ok((out, code)) if *out == expected && *code == Some(exit));
            if NOT_YET.contains(&pin.as_str()) {
                if pass {
                    bad.push(format!("{pin}: passes now; take it off NOT_YET"));
                }
                continue;
            }
            match result {
                Ok((out, code)) if out == expected && code == Some(exit) => {}
                Ok((out, code)) => bad.push(format!(
                    "{pin}: exit {code:?} (want {exit})\n--- got\n{out}--- want\n{expected}"
                )),
                Err(e) => bad.push(format!("{pin}: {e}")),
            }
        }
        assert!(ran > 0);
        assert!(
            bad.is_empty(),
            "{} of {ran}:\n{}",
            bad.len(),
            bad.join("\n")
        );
    }

    /// A user type with a `Display` impl prints through its `to_string`,
    /// in `println(x)` and inside an f-string.
    #[test]
    fn user_display_prints_through_to_string() {
        let src = r#"
struct P { x: i64 }
impl Display for P {
    fn to_string(ref self) -> String { if self.x > 0 { "pos" } else { "neg" } }
}
fn main() {
    let p = P { x: 1 };
    let q = P { x: -1 };
    println(f"p = {p}, q = {q}");
    println(q);
}
"#;
        assert_eq!(
            run_source(src),
            Ok(("p = pos, q = neg\nneg\n".to_string(), Some(0)))
        );
    }

    /// A `mut ref` passed on to a function or a library method is
    /// reborrowed, and a shared slice is copied: the caller uses both again.
    #[test]
    fn mut_ref_arguments_reborrow_and_slices_copy() {
        let src = r#"
fn add_one(out: mut ref Vec[i64], n: i64) {
    if n > 0 {
        out.push(n);
        add_one(out, n - 1);
        add_one(out, 0);
    }
}

fn total(xs: Slice[i64]) -> i64 {
    let mut s = 0;
    for x in xs {
        s = s + x;
    }
    s
}

fn report(xs: Slice[i64]) {
    println(f"{total(xs)} {total(xs)}");
}

fn grow(x: mut ref String) {
    x = x + "tail";
}

fn main() {
    let mut v: Vec[i64] = Vec.new();
    add_one(mut v, 3);
    println(f"{v.len()}");
    let a: Array[i64, 3] = [1, 2, 3];
    report(a);
    let mut s: String = "head";
    grow(mut s);
    println(s);
}
"#;
        assert_eq!(
            run_source(src),
            Ok(("3\n6 6\nheadtail\n".to_string(), Some(0)))
        );
    }

    /// Struct shorthand in a `let`, a closure whose body moves its capture,
    /// a `ref` read into a value, and a match on an element that binds a
    /// `shared` handle.
    #[test]
    fn shorthand_moving_closure_ref_read_and_element_match() {
        let src = r#"
struct A { id: i64 }
struct S2 { a: A, b: A }
struct P { x: i64 }
impl P { fn take(self) -> i64 { self.x } }
shared struct N { v: i64 }
fn read(x: ref i64) -> i64 { return x; }
fn main() {
    let S2 { a, b } = S2 { a: A { id: 1 }, b: A { id: 2 } };
    let p = P { x: 7 };
    let f = || p.take();
    let mut v: Vec[Option[N]] = Vec.new();
    v.push(Some(N { v: 5 }));
    let got = match v[0] {
        Some(n) => n.v,
        None => 0,
    };
    println(f"{a.id} {b.id} {f()} {read(9)} {got}");
}
"#;
        assert_eq!(run_source(src), Ok(("1 2 7 9 5\n".to_string(), Some(0))));
    }
}
