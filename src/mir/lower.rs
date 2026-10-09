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
    self, BinOp as AstBinOp, Block, CallArg, CompoundOp, EnsuresClause, Expr, ExprKind, Function,
    ImplItem, Item, LiteralPattern, ParLoop, ParsedInterpolationPart, Pattern, PatternKind,
    SelfParam, Stmt, StmtKind, UnaryOp,
};
use crate::def_path::DefPath;
use crate::def_table::ProgramDefs;
use crate::ids::{DefId, DefKind, NodeId};
use crate::node_res::Res;
use crate::ownership::OwnershipMode;
use crate::resolver::{ResolveResult, SpanKey, SymbolId};
use crate::token::Span;
use crate::ty::{AdtDef, IntrinsicKind, TyKind as HK, TypeName, VariantDef};
use crate::typechecker::types::{FloatSize, IntSize, Type, UIntSize, VariantTypeInfo};
use crate::typechecker::TypeCheckResult;
use crate::typed_hir::{Callee, ResolvedCall, TypedHir};

use super::build::BodyBuilder;
use super::interp;
use super::syntax::*;
use super::ty::{AdtId, FloatTy, Ty, TyInterner};

/// The value of an operator tree made only of unsuffixed integer literals,
/// when every step is in range. A lone literal is not a tree.
fn fold_int(e: &Expr) -> Option<i128> {
    fn go(e: &Expr) -> Option<i128> {
        match &e.kind {
            ExprKind::Integer(v, None) => Some(*v),
            ExprKind::Unary {
                op: UnaryOp::Neg,
                operand,
            } => go(operand)?.checked_neg(),
            ExprKind::Binary { op, left, right } => {
                let (a, b) = (go(left)?, go(right)?);
                match op {
                    AstBinOp::Add => a.checked_add(b),
                    AstBinOp::Sub => a.checked_sub(b),
                    AstBinOp::Mul => a.checked_mul(b),
                    AstBinOp::BitAnd => Some(a & b),
                    AstBinOp::BitOr => Some(a | b),
                    AstBinOp::BitXor => Some(a ^ b),
                    AstBinOp::Shl if (0..64).contains(&b) => a.checked_shl(b as u32),
                    AstBinOp::Shr if (0..64).contains(&b) => Some(a >> b),
                    _ => None,
                }
            }
            _ => None,
        }
    }
    match &e.kind {
        ExprKind::Binary { .. } => go(e),
        ExprKind::Unary { operand, .. } if !matches!(operand.kind, ExprKind::Integer(..)) => go(e),
        _ => None,
    }
}

/// An `if`/`if let` with an `else`, or a `match`: a value one of several
/// arms computes.
fn is_branching(e: &Expr) -> bool {
    matches!(
        e.kind,
        ExprKind::If {
            else_branch: Some(_),
            ..
        } | ExprKind::IfLet {
            else_branch: Some(_),
            ..
        } | ExprKind::Match { .. }
    )
}

/// The lowered program, or why it could not be lowered.
pub struct Lowered {
    pub program: interp::Program,
    pub tys: TyInterner,
    /// One line per construct the builder does not lower yet, with its
    /// position. Empty when every reachable body was built.
    pub errors: Vec<String>,
    /// The instances that take `ref self` or `mut ref self`.
    pub receivers: FxHashSet<String>,
    /// The receivers whose result borrows what the receiver's value
    /// borrows, not its place (§5.4; [`view_result`]).
    pub views: FxHashSet<String>,
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
        escaping_fns: &program.escaping_fn_types,
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
        par_joins: par_joins(rr),
        unbound_vars: rr
            .symbol_table
            .all_symbols()
            .iter()
            .filter(|s| {
                matches!(s.kind, crate::resolver::SymbolKind::Variable { .. })
                    && !rr.binding_nodes.contains_key(&s.id)
            })
            .map(|s| ((s.name.clone(), s.span.offset, s.span.length), s.id))
            .collect(),
        fns: FxHashMap::default(),
        consts: FxHashMap::default(),
        bindings: FxHashMap::default(),
        statics: FxHashMap::default(),
        static_queue: Vec::new(),
        self_tys: FxHashMap::default(),
        queue: Vec::new(),
        queued: FxHashSet::default(),
        program: interp::Program::default(),
        errors: Vec::new(),
        variants: FxHashMap::default(),
        generic_drops: FxHashMap::default(),
        registered: FxHashSet::default(),
        capture_modes,
        next_closure: defs.table.len() as u32,
        closures: FxHashMap::default(),
        closure_exprs: FxHashMap::default(),
        closure_queue: Vec::new(),
        fn_param_tys: FxHashMap::default(),
    };
    lcx.index_functions(program);
    // Statics are initialized in declaration order, so they are numbered
    // in it.
    for item in &program.items {
        if let Item::ModuleBinding(b) = item {
            if let Some(d) = defs.lookup(0, &b.name) {
                lcx.static_of(d);
            }
        }
    }
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
    let mut views = FxHashSet::default();
    loop {
        if let Some((def, args, name)) = lcx.queue.pop() {
            let has_self = lcx.fns.get(&def).is_some_and(|i| i.f.self_param.is_some());
            let view = lcx.fns.get(&def).is_some_and(|i| {
                // The impl's parameters only: a method's own can be filled
                // from another argument (`pick[U](ref self, f: Fn(ref Self)
                // -> U)`), so its value may borrow the receiver's place.
                let params: Vec<&str> = i
                    .impl_generics
                    .iter()
                    .flat_map(|g| g.params.iter().map(|p| p.name.as_str()))
                    .collect();
                i.f.return_type
                    .as_ref()
                    .is_some_and(|t| view_result(t, &params))
            });
            lcx.lower_instance(def, &args, &name);
            // A receiver the body borrows, written or (D5) a bare `self`.
            let borrows = lcx.program.bodies.get(&name).is_some_and(|b| {
                b.arg_count > 0
                    && matches!(
                        lcx.tys.tcx().kind(b.locals[1].ty),
                        HK::Ref(_) | HK::MutRef(_)
                    )
            });
            if has_self && borrows {
                if view {
                    views.insert(name.clone());
                }
                receivers.insert(name.clone());
            }
        } else if let Some(job) = lcx.closure_queue.pop() {
            lcx.lower_closure(job);
        } else if let Some((def, name)) = lcx.static_queue.pop() {
            lcx.lower_static(def, &name);
        } else {
            break;
        }
    }
    Lowered {
        program: lcx.program,
        tys: lcx.tys,
        errors: lcx.errors,
        receivers,
        views,
    }
}

/// Whether a method's result, of declared type `t`, can hold a reference
/// only through a type parameter of its impl (`params`) or an associated type
/// (`Self.Item`, `I.Item`): built from those, scalars, `String`, `Option`,
/// `Result`, tuples and arrays. Such a reference came from outside the
/// receiver, since a value cannot borrow itself, so the result borrows
/// what the receiver's value borrows (§5.4). A `ref` written in the type,
/// or a named type that may hold one, keeps the receiver's place.
fn view_result(t: &ast::TypeExpr, params: &[&str]) -> bool {
    match &t.kind {
        ast::TypeKind::Path(p) => {
            let args_ok = || {
                p.generic_args.as_ref().is_none_or(|g| {
                    g.iter().all(|a| match a {
                        ast::GenericArg::Type(t) => view_result(t, params),
                        _ => false,
                    })
                })
            };
            match p.segments.as_slice() {
                [base, _] => {
                    (base == "Self" || params.contains(&base.as_str())) && p.generic_args.is_none()
                }
                [n] if params.contains(&n.as_str()) => p.generic_args.is_none(),
                [n] if matches!(n.as_str(), "Option" | "Result") => args_ok(),
                [n] => {
                    p.generic_args.is_none()
                        && matches!(
                            n.as_str(),
                            "i8" | "i16"
                                | "i32"
                                | "i64"
                                | "i128"
                                | "isize"
                                | "u8"
                                | "u16"
                                | "u32"
                                | "u64"
                                | "u128"
                                | "usize"
                                | "f32"
                                | "f64"
                                | "bool"
                                | "char"
                                | "String"
                        )
                }
                _ => false,
            }
        }
        ast::TypeKind::Tuple(ts) => ts.iter().all(|t| view_result(t, params)),
        ast::TypeKind::Array { element, .. } => view_result(element, params),
        _ => false,
    }
}

struct FnItem<'a> {
    f: &'a Function,
    /// The impl's target type and how many generic parameters the impl
    /// declares (they come first in the method's positional generics).
    impl_target: Option<DefId>,
    impl_params: usize,
    /// The primitive an impl is for (`impl Step for i64`), which has no
    /// definition to be `impl_target`.
    impl_prim: Option<Ty>,
    /// The impl's generic parameters, which come before the method's.
    impl_generics: Option<&'a ast::GenericParams>,
    /// Where each impl parameter sits among the target's arguments, when
    /// that is not simply the leading ones in order: `impl[I, B] ... for
    /// MapIter[I, I.Item, B]` gives `[0, 2]`.
    impl_arg_pos: Option<Vec<usize>>,
    /// The impl block's span: with the method's own span, the key of the
    /// checker's per-function tables (a trait default method copied into
    /// several impls has the same span in each).
    impl_span: Option<SpanKey>,
}

impl FnItem<'_> {
    /// The impl's generic arguments, read off its target's arguments.
    fn impl_args(&self, targs: &[Ty]) -> Option<Vec<Ty>> {
        match &self.impl_arg_pos {
            Some(pos) => pos.iter().map(|&k| targs.get(k).copied()).collect(),
            None => targs.get(..self.impl_params).map(<[Ty]>::to_vec),
        }
    }
}

/// Where each of `b`'s generic parameters is written among its target's
/// arguments, or `None` when they are those arguments' first ones in
/// order (or are not each written bare, which nothing maps yet).
fn impl_arg_positions(b: &ast::ImplBlock) -> Option<Vec<usize>> {
    let params = &b.generic_params.as_ref()?.params;
    let ast::TypeKind::Path(p) = &b.target_type.kind else {
        return None;
    };
    let targs = p.generic_args.as_ref()?;
    let bare = |a: &ast::GenericArg| match a {
        ast::GenericArg::Type(t) => match &t.kind {
            ast::TypeKind::Path(q) if q.generic_args.is_none() && q.segments.len() == 1 => {
                Some(q.segments[0].clone())
            }
            _ => None,
        },
        _ => None,
    };
    let pos = params
        .iter()
        .map(|g| {
            targs
                .iter()
                .position(|a| bare(a).as_deref() == Some(g.name.as_str()))
        })
        .collect::<Option<Vec<_>>>()?;
    (!pos.iter().copied().eq(0..params.len())).then_some(pos)
}

struct Lcx<'a> {
    tc: &'a TypeCheckResult,
    /// `ref name` pattern bindings, by the binding's span.
    ref_bindings: &'a FxHashSet<SpanKey>,
    /// The types of `escaping Fn(..)` parameters, by span.
    escaping_fns: &'a FxHashSet<SpanKey>,
    defs: &'a ProgramDefs,
    res: &'a FxHashMap<NodeId, Res>,
    node_types: FxHashMap<NodeId, Ty>,
    calls: FxHashMap<NodeId, ResolvedCall>,
    tys: TyInterner,
    /// The symbol a pattern node binds under a name (a struct pattern's
    /// shorthand fields all bind through that pattern's node).
    binding_syms: FxHashMap<(NodeId, String), SymbolId>,
    /// A `par {}` branch's binding and the symbols its join re-defines
    /// it under in the enclosing scope (the tail and the code after the
    /// block read those).
    par_joins: FxHashMap<SymbolId, Vec<SymbolId>>,
    /// Variables the resolver defined with no binding pattern (an
    /// `ensures(result)` binding), by name and span.
    unbound_vars: FxHashMap<(String, usize, usize), SymbolId>,
    fns: FxHashMap<DefId, FnItem<'a>>,
    /// Module-level constants and immutable `let` bindings, by
    /// definition, with their initializers.
    consts: FxHashMap<DefId, &'a Expr>,
    /// Every module `let` binding, by definition.
    bindings: FxHashMap<DefId, &'a ast::ModuleBinding>,
    /// The module bindings that live in a static: each one's index in
    /// `program.statics`, its type, and whether it is `let mut`.
    statics: FxHashMap<DefId, (u32, Ty, bool)>,
    /// Statics whose initializer body is still to be built.
    static_queue: Vec<(DefId, String)>,
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
    /// The `drop` method of each generic ADT with a `Drop` body, which is
    /// instantiated for each instance of the type.
    generic_drops: FxHashMap<DefId, DefId>,
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

/// An element position in a sequence a slice pattern matches.
#[derive(Debug, Clone, Copy)]
enum SeqPos {
    /// The `i`th from the start.
    Start(u64),
    /// The `m`th from the end, counting the last as 1.
    End(u64),
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
    /// Its parameters borrow what the caller passes: a closure handed to
    /// a library call, which lends it elements in place.
    ref_params: bool,
}

/// A variant as the checker records it: its name and named field types.
type CheckedVariant = (String, Vec<(String, Type)>);

/// An integer constant re-typed to `t` (the checker records an unsuffixed
/// literal as `i64` whatever the other operand is); any other operand as
/// it is.
fn retype_const(o: Operand, t: Ty) -> Operand {
    match o {
        Operand::Const(Const {
            kind: ConstKind::Scalar(v),
            ..
        }) => Operand::Const(Const {
            ty: t,
            kind: ConstKind::Scalar(v),
        }),
        o => o,
    }
}

/// D5's bare-parameter borrow, on while `KARAC_D5=1` (until the corpus is
/// migrated to `own T` where a parameter is moved).
fn d5_params() -> bool {
    #[cfg(test)]
    if let Some(on) = tests::D5.with(|c| c.get()) {
        return on;
    }
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("KARAC_D5").is_ok_and(|v| v == "1"))
}

fn at(span: Span) -> String {
    format!("{}:{}", span.line, span.column)
}

impl<'a> Lcx<'a> {
    // ── items ───────────────────────────────────────────────────────

    /// Map each function and method in the AST to its DefId. An impl's
    /// DefId comes from the definition table, which numbered the program's
    /// impl blocks in item order after the stdlib's: a program's
    /// `impl Option[T]` is `impl#1` when the stdlib already has one.
    fn index_functions(&mut self, program: &'a ast::Program) {
        let mut impl_ids = self.defs.module_impls.first().into_iter().flatten();
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
                                impl_prim: None,
                                impl_generics: None,
                                impl_arg_pos: None,
                                impl_span: None,
                            },
                        );
                    }
                }
                Item::ConstDecl(c) => {
                    if let Some(d) = self.defs.lookup(0, &c.name) {
                        self.consts.insert(d, &c.value);
                    }
                }
                // An immutable module `let` is evaluated where it is used,
                // like a constant, unless it lives in a static
                // (`static_of`).
                Item::ModuleBinding(b) => {
                    if let Some(d) = self.defs.lookup(0, &b.name) {
                        self.bindings.insert(d, b);
                        if !b.is_mut {
                            self.consts.insert(d, &b.value);
                        }
                    }
                }
                Item::ImplBlock(b) => {
                    let Some(&impl_id) = impl_ids.next() else {
                        continue;
                    };
                    let target = self.defs.impls.get(&impl_id).and_then(|i| i.target);
                    let prim = match (&b.target_type.kind, target) {
                        (ast::TypeKind::Path(p), None) if b.generic_params.is_none() => {
                            p.segments.last().and_then(|n| self.primitive(n))
                        }
                        _ => None,
                    };
                    if target.is_none() && prim.is_none() {
                        continue;
                    }
                    let impl_path = self.defs.table.get(impl_id).path.segments.clone();
                    let impl_params = b.generic_params.as_ref().map_or(0, |g| g.params.len());
                    let impl_arg_pos = impl_arg_positions(b);
                    for it in &b.items {
                        let ImplItem::Method(f) = it else { continue };
                        // A `#[compiler_builtin]` method's body is a
                        // placeholder: a call is the interpreter's.
                        if f.attributes.iter().any(|a| a.is_bare("compiler_builtin")) {
                            continue;
                        }
                        let mut p = impl_path.clone();
                        p.push(f.name.clone());
                        if let Some(d) = self.defs.table.lookup(&DefPath::new(p)) {
                            if let Some(t) = prim {
                                self.self_tys.insert(d, t);
                            }
                            self.fns.insert(
                                d,
                                FnItem {
                                    f,
                                    impl_target: target,
                                    impl_params,
                                    impl_prim: prim,
                                    impl_generics: b.generic_params.as_ref(),
                                    impl_arg_pos: impl_arg_pos.clone(),
                                    impl_span: Some(SpanKey::from_span(&b.span)),
                                },
                            );
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// The scalar a primitive type name spells, for an impl on it.
    fn primitive(&self, name: &str) -> Option<Ty> {
        let kind = match name {
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
            "f32" => HK::Float(FloatSize::F32),
            "f64" => HK::Float(FloatSize::F64),
            "bool" => HK::Bool,
            "char" => HK::Char,
            "String" => HK::Str,
            _ => return None,
        };
        Some(self.tys.tcx().intern(kind))
    }

    /// The method `d` resolves to, or the same method of another impl of
    /// the same trait on the same type that has a body here. The baked
    /// stdlib's impls are not lowered; a library source re-states one in
    /// Kāra (`impl Display for AllocError` is the baked `impl Display#0`
    /// and the appended source's `impl Display#1`), and calls resolved to
    /// the baked one run the source one.
    fn lowered_def(&self, d: DefId) -> Option<DefId> {
        if self.fns.contains_key(&d) {
            return Some(d);
        }
        let segs = &self.defs.table.get(d).path.segments;
        let n = segs.len();
        if n < 2 {
            return None;
        }
        let (label, _) = segs[n - 2].rsplit_once('#')?;
        if !label.starts_with("impl") {
            return None;
        }
        let mut path = segs.clone();
        for k in 0..8 {
            path[n - 2] = format!("{label}#{k}");
            if let Some(t) = self.defs.table.lookup(&DefPath::new(path.clone())) {
                if self.fns.contains_key(&t) {
                    return Some(t);
                }
            }
        }
        None
    }

    /// The type whose impl defines `d` (`Command` for `Command.new`).
    fn def_owner(&self, d: DefId) -> Option<String> {
        let segs = &self.defs.table.get(d).path.segments;
        let n = segs.len();
        (n >= 3 && segs[n - 2].starts_with("impl")).then(|| segs[n - 3].clone())
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
            Some(FnItem {
                impl_prim: Some(t), ..
            }) => {
                let s = format!("{}.{name}", self.tys.display(*t));
                if args.is_empty() {
                    s
                } else {
                    format!("{s}[{}]", show(args))
                }
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
        let ret = match self.fn_return(def, args) {
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

    /// The static a module binding lives in, as its index, type and
    /// mutability, allocated (and its initializer queued) on first ask.
    /// A `let mut` binding needs one global place; so does an immutable
    /// one holding a cell (`Atomic`, `Mutex`, ...), whose writes through
    /// a shared borrow every use must see. Any other binding is a value
    /// computed where it is used.
    fn static_of(&mut self, d: DefId) -> Option<(u32, Ty, bool)> {
        if let Some(&s) = self.statics.get(&d) {
            return Some(s);
        }
        let b = *self.bindings.get(&d)?;
        let t = *self.node_types.get(&b.value.id)?;
        let t = self.mir_ty(t, &[]).ok()?;
        if !b.is_mut {
            let shown = self.tys.display(t);
            let cell = [
                "Atomic", "Mutex", "RwLock", "Arena", "OnceLock", "OnceCell", "LazyLock",
            ]
            .iter()
            .any(|c| shown == *c || shown.starts_with(&format!("{c}[")));
            if !cell {
                return None;
            }
        }
        let idx = self.program.statics.len() as u32;
        let init = format!("static.{}", b.name);
        self.program.statics.push(StaticDef {
            name: b.name.clone(),
            ty: t,
            mutable: b.is_mut,
            init: init.clone(),
        });
        self.statics.insert(d, (idx, t, b.is_mut));
        self.static_queue.push((d, init));
        Some((idx, t, b.is_mut))
    }

    /// A static's initializer: a body of no parameters returning its value.
    fn lower_static(&mut self, def: DefId, name: &str) {
        let Some(b) = self.bindings.get(&def).copied() else {
            return;
        };
        let Some(&(_, t, _)) = self.statics.get(&def) else {
            return;
        };
        let instance = InstanceId {
            def,
            args: Vec::new(),
            name: name.to_string(),
        };
        let mut bx = Bx::new(self, instance, t, Vec::new());
        let _ = bx.lower_static_init(&b.value);
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

    /// The key of `def` in the checker's per-function tables.
    fn fn_key(&self, def: DefId) -> (Option<SpanKey>, SpanKey) {
        let item = &self.fns[&def];
        (item.impl_span, SpanKey::from_span(&item.f.span))
    }

    /// The declared return type of `f`, instantiated.
    fn fn_return(&mut self, def: DefId, args: &[Ty]) -> Result<Ty, String> {
        let key = self.fn_key(def);
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
        let hir = self.fill_erased_target(def, hir, args);
        self.mir_ty(hir, args)
    }

    /// `t` with the instance arguments `args` of `def` written into its
    /// impl target where the checker recorded the target erased (`Tk` for
    /// `impl[I] It for Tk[I]`, which is how `self`, a local bound to it, and
    /// `Self` in a signature are typed).
    fn fill_erased_target(&self, def: DefId, t: Ty, args: &[Ty]) -> Ty {
        let Some(item) = self.fns.get(&def) else {
            return t;
        };
        let (Some(target), n) = (item.impl_target, item.impl_params) else {
            return t;
        };
        if n == 0 {
            // A concrete impl over an applied generic (`impl Bx[String]`):
            // its target's args are the receiver's, as a call recorded them.
            let tcx = self.tys.tcx();
            let applied = match self.self_tys.get(&def).map(|&st| tcx.kind(st)) {
                Some(HK::Adt { args, .. } | HK::Shared { args, .. }) => tcx.list(args),
                _ => Vec::new(),
            };
            if applied.is_empty() {
                return t;
            }
            return tcx.fill_erased_args(t, target, &applied);
        }
        if n > args.len() {
            return t;
        }
        self.tys.tcx().fill_erased_args(t, target, &args[..n])
    }

    /// `self`'s type in `f`, instantiated, when the checker recorded the
    /// impl target with its args written out: `Option[Option[i64]]` for
    /// `impl[T] Option[Option[T]]` at `T = i64`, where building the target
    /// from the impl's params by position gives `Option[i64]`. `None` when
    /// the recorded target has its args erased, as most generic impls'
    /// targets are.
    fn fn_self_written(&mut self, def: DefId, args: &[Ty]) -> Option<Result<Ty, String>> {
        let key = self.fn_key(def);
        let (ty, frame) = self.tc.fn_self_types.get(&key)?;
        if !matches!(ty, Type::Named { args, .. } if !args.is_empty()) {
            return None;
        }
        let params = self
            .tc
            .node_generic_frames
            .get(*frame as usize)
            .cloned()
            .unwrap_or_default();
        Some(
            self.lower_legacy(ty, &params)
                .and_then(|hir| self.mir_ty(hir, args)),
        )
    }

    fn lower_legacy(&self, ty: &Type, params: &[String]) -> Result<Ty, String> {
        let defs = self.defs;
        let lookup = |name: &str| {
            let d = defs.lookup(0, name)?;
            if defs.is_builtin_unit(d) {
                // `Unit` spelled by name is `()`.
                return None;
            }
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
                    let targs = args.clone();
                    let args = self.tys.tcx().intern_list(&args);
                    let new = if shared {
                        HK::Shared { def, args }
                    } else {
                        HK::Adt { def, args }
                    };
                    // A generic type's `Drop` body, for this instance.
                    if let Some(&d) = self.generic_drops.get(&def) {
                        let concrete = !targs.iter().any(|&t| has_param(&self.tys, t));
                        if concrete {
                            let ty = self.tys.tcx().intern(new);
                            if !self.program.drop_by_ty.contains_key(&ty) {
                                let inst = if self.fns.contains_key(&d) {
                                    self.instance(d, targs)
                                } else {
                                    // A `#[compiler_builtin]` drop: the
                                    // interpreter's, by the type's name.
                                    format!("{}.drop", self.tys.display(ty))
                                };
                                self.program.drop_by_ty.insert(ty, inst);
                            }
                        }
                    }
                    new
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
            HK::Proj { base, assoc } => return self.normalize_proj(base, assoc),
            HK::Error => return Err("a type that failed to check".into()),
            HK::Weak(t) => HK::Weak(self.convert(t)?),
            HK::Fn {
                params,
                ret,
                once,
                mutable,
            } => HK::Fn {
                params: {
                    let tys = list(self, params)?;
                    self.tys.tcx().intern_list(&tys)
                },
                ret: self.convert(ret)?,
                once,
                mutable,
            },
            HK::RawPtr { .. } => return Err("a raw pointer".into()),
            other => other,
        };
        Ok(self.tys.tcx().intern(new))
    }

    /// `base.assoc` at an instance: the binding of `assoc` in the impl for
    /// `base`'s type, instantiated with `base`'s arguments.
    fn normalize_proj(&mut self, base: Ty, assoc: crate::intern::Symbol) -> Result<Ty, String> {
        let base = self.convert(base)?;
        let tcx = self.tys.tcx();
        let (name, args) = match tcx.kind(base) {
            HK::Adt { def, args } | HK::Shared { def, args } => (self.def_name(def), args),
            HK::Intrinsic { kind, args } => (kind.name().to_string(), args),
            _ => {
                return Err(format!(
                    "an associated type of `{}`",
                    tcx.display(base, &|d| self.def_name(d))
                ))
            }
        };
        let assoc = tcx.resolve_name(assoc).to_string();
        let tc = self.tc;
        let Some(bound) = tc.impl_assoc_types.get(&(name.clone(), assoc.clone())) else {
            return Err(format!("`{name}.{assoc}`, which no impl binds"));
        };
        let params = tc
            .struct_info
            .get(&name)
            .map(|s| s.generic_params.clone())
            .or_else(|| tc.enum_info.get(&name).map(|e| e.generic_params.clone()))
            .unwrap_or_default();
        let hir = self.lower_legacy(bound, &params)?;
        let args = tcx.list(args);
        self.mir_ty(hir, &args)
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
                    s.is_shared || s.is_par,
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
                (
                    e.generic_params.clone(),
                    vs,
                    e.is_shared || e.is_par,
                    &e.derived_traits,
                )
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
        if is_enum && tc.display_snake_case_enums.contains(&name) {
            self.program
                .display_styles
                .insert(AdtId(def.0), interp::DisplayStyle::SnakeCase);
        }
        if name == "Secret"
            && tc
                .struct_info
                .get(&name)
                .is_some_and(|si| si.defining_stdlib_origin)
        {
            self.program
                .display_styles
                .insert(AdtId(def.0), interp::DisplayStyle::Redacted);
        }
        self.tys.tcx().add_adt_def(AdtDef {
            def,
            name: name.clone(),
            is_enum,
            variants: vdefs,
            has_drop_impl,
            is_copy: derived.contains("Copy") || (is_enum && name == "Option"),
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
                    // A `#[compiler_builtin]` drop (`TaskGroup`'s) has no
                    // Kāra body: it is the interpreter's `TaskGroup.drop`.
                    let inst = if self.fns.contains_key(&d) {
                        self.instance(d, Vec::new())
                    } else {
                        format!("{name}.drop")
                    };
                    self.program.drop_impls.insert(AdtId(def.0), inst);
                }
                Some(d) => {
                    self.generic_drops.insert(def, d);
                }
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

/// Whether `t` still names a generic parameter.
fn has_param(tys: &TyInterner, t: Ty) -> bool {
    let tcx = tys.tcx();
    match tcx.kind(t) {
        HK::Param(_) => true,
        HK::Adt { args, .. } | HK::Shared { args, .. } | HK::Intrinsic { args, .. } => {
            tcx.list(args).into_iter().any(|a| has_param(tys, a))
        }
        HK::Tuple(l) => tcx.list(l).into_iter().any(|a| has_param(tys, a)),
        HK::Ref(i) | HK::MutRef(i) | HK::Weak(i) => has_param(tys, i),
        HK::Array { elem, .. } | HK::Slice { elem, .. } => has_param(tys, elem),
        _ => false,
    }
}

/// Each `par {}` branch binding's join symbols: a variable the resolver
/// defined at a binding's name and span with no binding node of its own.
fn par_joins(rr: &ResolveResult) -> FxHashMap<SymbolId, Vec<SymbolId>> {
    let table = &rr.symbol_table;
    let bound: FxHashMap<(&str, usize, usize), SymbolId> = rr
        .binding_nodes
        .keys()
        .map(|&s| {
            let sym = table.get_symbol(s);
            ((sym.name.as_str(), sym.span.offset, sym.span.length), s)
        })
        .collect();
    let mut out: FxHashMap<SymbolId, Vec<SymbolId>> = FxHashMap::default();
    for sym in table.all_symbols() {
        if !matches!(sym.kind, crate::resolver::SymbolKind::Variable { .. })
            || rr.binding_nodes.contains_key(&sym.id)
        {
            continue;
        }
        if let Some(&b) = bound.get(&(sym.name.as_str(), sym.span.offset, sym.span.length)) {
            out.entry(b).or_default().push(sym.id);
        }
    }
    out
}

// ── bodies ──────────────────────────────────────────────────────────

enum ScopeEntry<'a> {
    Drop(Place),
    Defer(&'a Block),
    /// An `errdefer` body, with `errdefer(e)`'s name and statement node.
    ErrDefer(&'a Block, Option<(&'a str, NodeId)>),
    /// A local whose storage ends with the scope.
    Storage(Local),
}

/// An iterator type's `next`, instantiated for a `for` loop: its definition,
/// instance args and name, and the `Option[Item]` it returns.
type NextFn = (DefId, Vec<Ty>, String, Ty);

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
    /// The next closure literal lowered goes to a library call, so its
    /// parameters borrow.
    closure_ref_params: bool,
    /// The next closure literal lowered escapes (it is stored as a
    /// function value), so it captures by move.
    closure_escapes: bool,
    /// The captures a closure body assigns to, found with its moves.
    assigned_caps: std::cell::RefCell<FxHashSet<SymbolId>>,
    /// Types for expressions the checker left untyped, taken from where
    /// they are used (`v.push(None)` gives `None` the element type).
    ty_hints: FxHashMap<NodeId, Ty>,
    /// The `par for` whose body is lowered next: its body, the `Vec` local
    /// that collects the bodies' values, and that `Vec`'s type.
    par_acc: Option<(*const Block, Local, Ty, usize)>,
    /// Each `old(e)` of this function's `ensures` clauses: `e`'s value,
    /// taken on entry.
    old_vals: FxHashMap<NodeId, Local>,
    /// This function's `ensures` clauses, checked at every return.
    ensures: &'a [EnsuresClause],
    /// The impl this function belongs to and its generic parameter count.
    impl_target: Option<(DefId, usize)>,
    /// Subscripts of an assignment's target, evaluated before its value.
    pre_indices: FxHashMap<NodeId, Operand>,
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
            closure_ref_params: false,
            closure_escapes: false,
            assigned_caps: Default::default(),
            ty_hints: FxHashMap::default(),
            par_acc: None,
            old_vals: FxHashMap::default(),
            ensures: &[],
            impl_target: None,
            pre_indices: FxHashMap::default(),
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
        if let Some(&l) = self.old_vals.get(&id) {
            return Ok(self.b.local_ty(l));
        }
        let Some(&t) = self.lcx.node_types.get(&id) else {
            if let Some(&t) = self.ty_hints.get(&id) {
                return Ok(t);
            }
            return self.unsupported(span, "an expression with no recorded type");
        };
        let args = self.args.clone();
        let t = self.fill_erased_target(t);
        match self.lcx.mir_ty(t, &args) {
            Ok(t) => Ok(t),
            Err(e) => self.unsupported(span, &e),
        }
    }

    /// `t` with this instance's impl arguments written into its impl
    /// target where the checker recorded the target erased (a local bound
    /// to `self` in `impl[I] It for Tk[I]` is typed `Tk`).
    fn fill_erased_target(&self, t: Ty) -> Ty {
        self.lcx
            .fill_erased_target(self.b.instance().def, t, &self.args)
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
            _ if is_branching(e) => {
                let t = self.node_ty(e.id, e.span)?;
                return Ok(self.arm_width(e, t));
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
        let place = place.into();
        let pt = self.place_type(&place);
        let rv = match rv {
            // A closure or function item stored in a function-typed slot
            // is erased to that type.
            Rvalue::Use(op) if self.erases(&op, pt) => {
                Rvalue::Cast(CastKind::Erase, self.erase_source(op), pt)
            }
            Rvalue::Use(op) if self.downgrades(&op, pt) => Rvalue::Use(self.downgrade(op, pt)),
            Rvalue::Use(op) if self.reads_through(&op, pt) => {
                Rvalue::Use(self.read_through(op, pt))
            }
            Rvalue::Use(op @ (Operand::Copy(_) | Operand::Move(_)))
                if self.widening(self.operand_ty(&op), pt).is_some() =>
            {
                let kind = self.widening(self.operand_ty(&op), pt).unwrap();
                Rvalue::Cast(kind, op, pt)
            }
            Rvalue::Aggregate(kind, ops) => {
                // A variant of a generic type named bare in its own impl
                // (`self = Cnt.Done`) is recorded without its arguments:
                // it builds the type of the place it goes to.
                let kind = match kind {
                    AggregateKind::Adt { ty, variant } if self.bare_of(ty, pt) => {
                        AggregateKind::Adt { ty: pt, variant }
                    }
                    AggregateKind::Shared { ty, variant } if self.bare_of(ty, pt) => {
                        AggregateKind::Shared { ty: pt, variant }
                    }
                    k => k,
                };
                let ops = self.erase_parts(&place, &kind, ops);
                Rvalue::Aggregate(kind, ops)
            }
            rv => rv,
        };
        let rv = match rv {
            Rvalue::Use(o @ Operand::Const(_)) => {
                Rvalue::Use(self.fit_const(o, self.place_type(&place)))
            }
            Rvalue::Aggregate(kind, ops) => {
                let ops = self.fit_parts(&place, &kind, ops);
                Rvalue::Aggregate(kind, ops)
            }
            rv => rv,
        };
        let bb = self.cur;
        self.b.assign(bb, place, rv);
    }

    /// Whether `t` is the generic type `full` names, written with no
    /// arguments.
    fn bare_of(&self, t: Ty, full: Ty) -> bool {
        let tcx = self.tys().tcx();
        match (tcx.kind(t), tcx.kind(full)) {
            (HK::Adt { def, args }, HK::Adt { def: d2, args: a2 })
            | (HK::Shared { def, args }, HK::Shared { def: d2, args: a2 }) => {
                def == d2 && tcx.list(args).is_empty() && !tcx.list(a2).is_empty()
            }
            _ => false,
        }
    }

    /// The type of an operand's value.
    fn operand_ty(&self, op: &Operand) -> Ty {
        match op {
            Operand::Copy(p) | Operand::Move(p) => self.place_type(p),
            Operand::Const(c) => c.ty,
        }
    }

    /// The cast that widens a value of numeric type `from` into the
    /// numeric slot `to`, when they differ (design.md §5, lossless widening
    /// is implicit at a binding, an argument, a field or a return).
    fn widening(&self, from: Ty, to: Ty) -> Option<CastKind> {
        if from == to {
            return None;
        }
        let tcx = self.tys().tcx();
        let int = |k| matches!(k, HK::Int(_) | HK::UInt(_));
        let float = |k| matches!(k, HK::Float(_));
        let (f, t) = (tcx.kind(from), tcx.kind(to));
        if int(f) && int(t) {
            Some(CastKind::IntToInt)
        } else if int(f) && float(t) {
            Some(CastKind::IntToFloat)
        } else if float(f) && float(t) {
            Some(CastKind::FloatToFloat)
        } else {
            None
        }
    }

    /// Whether every value of numeric type `from` is exactly a value of `to`.
    fn lossless(&self, from: Ty, to: Ty) -> bool {
        // (signed, unsigned or float; value bits)
        let class = |t: Ty| match self.tys().tcx().kind(t) {
            HK::Int(s) => Some((
                's',
                match s {
                    IntSize::I8 => 8,
                    IntSize::I16 => 16,
                    IntSize::I32 => 32,
                    IntSize::I64 | IntSize::Isize => 64,
                    IntSize::I128 => 128,
                },
            )),
            HK::UInt(s) => Some((
                'u',
                match s {
                    UIntSize::U8 => 8,
                    UIntSize::U16 => 16,
                    UIntSize::U32 => 32,
                    UIntSize::U64 | UIntSize::Usize => 64,
                    UIntSize::U128 => 128,
                },
            )),
            // The significand's bits, the implicit one included.
            HK::Float(s) => Some((
                'f',
                match s {
                    FloatSize::BF16 => 8,
                    FloatSize::F16 => 11,
                    FloatSize::F32 => 24,
                    FloatSize::F64 => 53,
                },
            )),
            _ => None,
        };
        let (Some((fc, fb)), Some((tc, tb))) = (class(from), class(to)) else {
            return false;
        };
        match (fc, tc) {
            ('s', 's') | ('u', 'u') | ('u', 's') => fb < tb,
            ('s', 'f') => fb - 1 <= tb,
            ('u', 'f') => fb <= tb,
            ('f', 'f') => fb < tb,
            _ => false,
        }
    }

    /// The type of a branching value recorded at numeric type `t`: its
    /// widest arm. The checker's branch join keeps the first arm's type when
    /// the other only widens to it, so `if c { 0 } else { big }` is recorded
    /// at the literal's `i64` though `big` is an `i128`. An unsuffixed
    /// literal arm takes whatever type the value has.
    fn arm_width(&mut self, e: &Expr, t: Ty) -> Ty {
        match &e.kind {
            ExprKind::If {
                then_block,
                else_branch: Some(els),
                ..
            }
            | ExprKind::IfLet {
                then_block,
                else_branch: Some(els),
                ..
            } => {
                let t = match &then_block.final_expr {
                    Some(x) => self.arm_width(x, t),
                    None => t,
                };
                self.arm_width(els, t)
            }
            ExprKind::Match { arms, .. } => {
                let mut t = t;
                for a in arms {
                    t = self.arm_width(&a.body, t);
                }
                t
            }
            ExprKind::Block(b) => match &b.final_expr {
                Some(x) => self.arm_width(x, t),
                None => t,
            },
            ExprKind::Integer(_, None) | ExprKind::Float(_, None) => t,
            _ if fold_int(e).is_some() => t,
            _ => match self.expr_ty(e) {
                Ok(at) if self.lossless(t, at) => at,
                _ => t,
            },
        }
    }

    /// `op` widened into the numeric slot `t`, if it needs it.
    fn widen(&mut self, op: Operand, t: Ty) -> Operand {
        if let Operand::Const(_) = op {
            return self.fit_const(op, t);
        }
        let Some(kind) = self.widening(self.operand_ty(&op), t) else {
            return op;
        };
        let l = self.temp(t);
        let bb = self.cur;
        self.b
            .assign(bb, Place::local(l), Rvalue::Cast(kind, op, t));
        Operand::Copy(Place::local(l))
    }

    /// Whether `p` is reached through a `shared` value, a reference or an
    /// index, so its contents cannot be moved out.
    fn borrowed_place(&self, p: &Place) -> bool {
        let mut q = Place::local(p.local);
        for el in &p.projection {
            let t = self.place_type(&q);
            if matches!(
                self.tys().tcx().kind(t),
                HK::Shared { .. } | HK::Ref(_) | HK::MutRef(_)
            ) || matches!(el, ProjElem::Index(_) | ProjElem::ConstIndex(_))
            {
                return true;
            }
            q = q.project(*el);
        }
        false
    }

    /// `let y = x` where `x` is a reference to a value that is neither
    /// `Copy` nor counted: `y` copies the reference (core semantics §4.6,
    /// §5.1), so it reads through it rather than moving the target out.
    fn ref_rebind(&mut self, value: &'a Expr, t: Ty) -> R<Option<Ty>> {
        if self.is_copy(t) || self.is_handle(t) || self.is_handle_aggregate(t) {
            return Ok(None);
        }
        if !matches!(value.kind, ExprKind::Identifier(_)) {
            return Ok(None);
        }
        let vt = self.expr_ty(value)?;
        Ok(match self.tys().tcx().kind(vt) {
            HK::Ref(i) | HK::MutRef(i) if i == t => Some(vt),
            _ => None,
        })
    }

    /// Whether `op` is a reference whose target goes into the slot `t` by
    /// value: a borrowed parameter (D5) stored into a field or a local.
    fn reads_through(&self, op: &Operand, t: Ty) -> bool {
        let tcx = self.tys().tcx();
        !matches!(op, Operand::Const(_))
            && matches!(tcx.kind(self.operand_ty(op)), HK::Ref(i) | HK::MutRef(i) if i == t)
    }

    /// The value `op` refers to, for the slot `t`: a `Copy` value copies, a
    /// handle or handle aggregate counts (§6.1), and anything else moves
    /// out of the borrow, which the borrow check refuses.
    fn read_through(&mut self, op: Operand, t: Ty) -> Operand {
        let (Operand::Copy(p) | Operand::Move(p)) = op else {
            return op;
        };
        let target = p.project(ProjElem::Deref);
        if self.is_handle(t) || self.is_handle_aggregate(t) {
            let l = self.temp(t);
            self.count_copy(target, t, Place::local(l));
            return Operand::Move(Place::local(l));
        }
        self.use_place(target, t)
    }

    /// Whether `op` is a strong handle going into the `weak` slot `t`.
    fn downgrades(&self, op: &Operand, t: Ty) -> bool {
        let tcx = self.tys().tcx();
        matches!(tcx.kind(t), HK::Weak(_)) && !matches!(tcx.kind(self.operand_ty(op)), HK::Weak(_))
    }

    /// A strong handle stored into the `weak` slot `t` is downgraded:
    /// moved into a temporary, read through a borrow into a new weak
    /// reference, and the strong handle released (design.md, "strong to
    /// weak is implicit").
    fn downgrade(&mut self, op: Operand, t: Ty) -> Operand {
        let st = self.operand_ty(&op);
        let (strong, rt) = {
            let tcx = self.tys().tcx();
            let st = match tcx.kind(st) {
                HK::Ref(i) | HK::MutRef(i) => i,
                _ => st,
            };
            (st, tcx.reference(st, false))
        };
        let src = match op {
            Operand::Copy(p) | Operand::Move(p)
                if self.operand_ty(&Operand::Copy(p.clone())) != strong =>
            {
                // A reference to the handle: borrow through it.
                p.project(ProjElem::Deref)
            }
            op => {
                let h = self.temp(strong);
                let bb = self.cur;
                self.b.assign(bb, Place::local(h), Rvalue::Use(op));
                Place::local(h)
            }
        };
        let owned = src.projection.is_empty();
        let r = self.temp(rt);
        let w = self.temp(t);
        let bb = self.cur;
        self.b.assign(
            bb,
            Place::local(r),
            Rvalue::Ref(BorrowKind::Shared, src.clone()),
        );
        self.b.assign(
            bb,
            Place::local(w),
            Rvalue::Cast(CastKind::Downgrade, Operand::Move(Place::local(r)), t),
        );
        if owned {
            let next = self.b.new_block();
            self.goto_with(
                TerminatorKind::Drop {
                    place: src,
                    target: next,
                    unwind: UnwindAction::Abort,
                },
                next,
            );
        }
        Operand::Move(Place::local(w))
    }

    /// Whether `op` is a closure, function item or function value of
    /// another kind going into the function-typed slot `t`.
    fn erases(&self, op: &Operand, t: Ty) -> bool {
        let tcx = self.tys().tcx();
        if !matches!(tcx.kind(t), HK::Fn { .. }) {
            return false;
        }
        let mut ot = self.operand_ty(op);
        if let HK::Ref(inner) | HK::MutRef(inner) = tcx.kind(ot) {
            ot = inner;
        }
        ot != t
            && matches!(
                tcx.kind(ot),
                HK::Closure { .. } | HK::FnDef { .. } | HK::Fn { .. }
            )
    }

    /// The operand an `Erase` takes: the value itself, moved out from
    /// behind a reference when it is one (which the borrow check refuses:
    /// a borrowed closure, such as a non-escaping parameter, cannot be
    /// stored).
    fn erase_source(&self, op: Operand) -> Operand {
        let ot = self.operand_ty(&op);
        match (&op, self.tys().tcx().kind(ot)) {
            (Operand::Copy(p) | Operand::Move(p), HK::Ref(_) | HK::MutRef(_)) => {
                Operand::Move(p.clone().project(ProjElem::Deref))
            }
            _ => op,
        }
    }

    /// An aggregate's parts, each closure or function item going into a
    /// function-typed field erased first.
    fn erase_parts(
        &mut self,
        place: &Place,
        kind: &AggregateKind,
        ops: Vec<Operand>,
    ) -> Vec<Operand> {
        let mut out = Vec::with_capacity(ops.len());
        for (i, o) in ops.into_iter().enumerate() {
            let tcx = self.tys().tcx();
            let slot = match kind {
                AggregateKind::Array(elem) => Some(*elem),
                AggregateKind::Tuple => tcx.field_ty(self.place_type(place), None, i as u32),
                AggregateKind::Adt { ty, variant } | AggregateKind::Shared { ty, variant } => {
                    let is_enum = tcx.adt_of(*ty).is_some_and(|(a, _)| a.is_enum);
                    tcx.field_ty(*ty, is_enum.then_some(variant.0), i as u32)
                }
                AggregateKind::Closure { .. } => None,
            };
            match slot {
                Some(t) if self.erases(&o, t) => {
                    let l = self.temp(t);
                    let bb = self.cur;
                    let o = self.erase_source(o);
                    self.b.assign(bb, l, Rvalue::Cast(CastKind::Erase, o, t));
                    out.push(Operand::Move(Place::local(l)));
                }
                Some(t) if self.downgrades(&o, t) => out.push(self.downgrade(o, t)),
                Some(t) if self.reads_through(&o, t) => out.push(self.read_through(o, t)),
                Some(t) if !matches!(o, Operand::Const(_)) => out.push(self.widen(o, t)),
                _ => out.push(o),
            }
        }
        out
    }

    /// A numeric literal in a slot of another numeric type takes the
    /// slot's (the checker records an unsuffixed literal as `i64` or `f64`
    /// wherever it sits; `let x: f64 = 2` reads the integer as a float).
    fn fit_const(&self, o: Operand, t: Ty) -> Operand {
        let Operand::Const(c) = o else {
            return o;
        };
        let tcx = self.tys().tcx();
        let int = |t| matches!(tcx.kind(t), HK::Int(_) | HK::UInt(_));
        let float = |t| matches!(tcx.kind(t), HK::Float(_));
        let kind = match c.kind {
            ConstKind::Scalar(v) if int(c.ty) && float(t) => {
                ConstKind::Float(((v as i128) as f64).to_bits())
            }
            k @ ConstKind::Scalar(_) if int(c.ty) && int(t) => k,
            k @ ConstKind::Float(_) if float(c.ty) && float(t) => k,
            kind => return Operand::Const(Const { ty: c.ty, kind }),
        };
        Operand::Const(Const { ty: t, kind })
    }

    /// An aggregate's literal parts, fitted to their slots' types.
    fn fit_parts(&self, place: &Place, kind: &AggregateKind, ops: Vec<Operand>) -> Vec<Operand> {
        let tcx = self.tys().tcx();
        let slot = |i: usize| -> Option<Ty> {
            match kind {
                AggregateKind::Array(elem) => Some(*elem),
                AggregateKind::Tuple => tcx.field_ty(self.place_type(place), None, i as u32),
                AggregateKind::Adt { ty, variant } | AggregateKind::Shared { ty, variant } => {
                    let is_enum = tcx.adt_of(*ty).is_some_and(|(a, _)| a.is_enum);
                    tcx.field_ty(*ty, is_enum.then_some(variant.0), i as u32)
                }
                AggregateKind::Closure { .. } => None,
            }
        };
        ops.into_iter()
            .enumerate()
            .map(|(i, o)| match (&o, slot(i)) {
                (Operand::Const(_), Some(t)) => self.fit_const(o, t),
                _ => o,
            })
            .collect()
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
    /// A temporary holding `e`'s value, dropped at the end of the scope.
    /// Its drop is scheduled once the value exists, so it drops before
    /// the temporaries made while computing it (core semantics §7.4).
    fn temp_of(&mut self, e: &'a Expr, t: Ty) -> R<Local> {
        let l = self.temp(t);
        self.expr_into(e, Place::local(l))?;
        if self.needs_drop(t) {
            self.schedule(ScopeEntry::Drop(Place::local(l)));
        }
        Ok(l)
    }

    /// The place `e` names when it is an owned local's (or a part of one
    /// reached without a reference), and holds no `shared` handle.
    fn owned_place(&mut self, e: &'a Expr) -> R<Option<Place>> {
        if !is_place_expr(e) {
            return Ok(None);
        }
        if let ExprKind::Identifier(_) = &e.kind {
            let local = matches!(self.lcx.res.get(&e.id), Some(Res::Local(s)) if self.locals.contains_key(s));
            if !local {
                return Ok(None);
            }
        }
        let p = self.expr_place(e, false)?;
        let t = self.place_type(&p);
        let through_handle = (0..p.projection.len()).any(|i| {
            let prefix = Place {
                local: p.local,
                projection: p.projection[..i].to_vec(),
            };
            let pt = self.place_type(&prefix);
            self.is_handle(pt)
                || self
                    .tys()
                    .tcx()
                    .adt_of(pt)
                    .is_some_and(|(a, _)| a.has_drop_impl)
        });
        let owned = !through_handle
            && !p.projection.contains(&ProjElem::Deref)
            && match self.tys().tcx().kind(t) {
                HK::Adt { .. } => !self
                    .tys()
                    .tcx()
                    .adt_of(t)
                    .is_some_and(|(a, _)| a.has_drop_impl),
                HK::Tuple(_) | HK::Array { .. } => true,
                _ => false,
            };
        Ok(owned.then_some(p))
    }

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
                ScopeEntry::ErrDefer(body, None) if error => self.defer_body(body)?,
                ScopeEntry::ErrDefer(body, Some((name, node))) if error => {
                    self.errdefer_bound(body, name, *node)?
                }
                ScopeEntry::ErrDefer(..) => {}
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

    /// `errdefer(e) { .. }` on the error exit: `e` borrows the `Err`
    /// payload of the value being returned, for the body only.
    fn errdefer_bound(&mut self, body: &'a Block, name: &str, node: NodeId) -> R<()> {
        let ret = self.b.local_ty(Local::RETURN_PLACE);
        let err = self.error_variant(ret);
        let et = err.and_then(|vi| self.tys().tcx().field_ty(ret, Some(vi), 0));
        let (Some(vi), Some(et)) = (err, et) else {
            return self.unsupported(body.span, "an `errdefer` binding outside a `Result`");
        };
        let payload = Place::local(Local::RETURN_PLACE)
            .project(ProjElem::Downcast(VariantIdx(vi)))
            .field(0, et);
        self.push_scope();
        let mut binds = Vec::new();
        self.bind_one(name, node, payload, et, true, &mut binds);
        for (l, t) in binds {
            self.declare(l, t);
        }
        self.defer_body(body)?;
        self.pop_scope()
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

    fn lower_static_init(&mut self, value: &'a Expr) -> R<()> {
        let ret = Place::local(Local::RETURN_PLACE);
        self.push_scope();
        self.expr_into(value, ret)?;
        self.pop_scope()?;
        self.return_exit()?;
        self.scopes.clear();
        Ok(())
    }

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
        self.impl_target = impl_target;
        self.push_scope();
        if let Some(mode) = &f.self_param {
            let seen = impl_target
                .is_none_or(|(_, n)| n == 0)
                .then(|| self.lcx.self_tys.get(&self.b.instance().def).copied())
                .flatten();
            let t = match seen {
                Some(t) => t,
                None => {
                    let Some((target, n)) = impl_target else {
                        return self.unsupported(f.span, "a `self` parameter outside an impl");
                    };
                    let args = self.args.clone();
                    let def = self.b.instance().def;
                    match self.lcx.fn_self_written(def, &args).filter(|_| n > 0) {
                        Some(Ok(t)) => t,
                        Some(Err(e)) => return self.unsupported(f.span, &e),
                        None => {
                            let tcx = self.tys().tcx();
                            let params: Vec<Ty> = self.args[..n.min(self.args.len())].to_vec();
                            let target_ty = tcx.adt(target, &params);
                            match self.lcx.convert(target_ty) {
                                Ok(t) => t,
                                Err(e) => return self.unsupported(f.span, &e),
                            }
                        }
                    }
                }
            };
            let t = match self.self_mode(f, mode.clone(), t) {
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
                None => {
                    let t = self.node_ty(p.pattern.id, p.span)?;
                    self.param_mode_ty(p, t)
                }
            };
            let l = self.b.arg(name, t);
            if let Some(&sym) = self.lcx.binding_syms.get(&(p.pattern.id, name.clone())) {
                self.locals.insert(sym, l);
            }
            if self.needs_drop(t) {
                self.schedule(ScopeEntry::Drop(Place::local(l)));
            }
        }
        self.contracts_on_entry(f)?;
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

    /// A function's contracts on entry (design.md § Contracts): each
    /// `requires` must hold, and each `old(e)` its `ensures` clauses read
    /// is evaluated now. An `ensures(result)` clause reads the return place.
    fn contracts_on_entry(&mut self, f: &'a Function) -> R<()> {
        for r in &f.requires {
            self.contract_check(r)?;
        }
        let mut olds = Vec::new();
        for clause in &f.ensures {
            old_calls(&clause.body, &mut olds);
            if let Some(name) = &clause.param {
                let key = (name.clone(), clause.span.offset, clause.span.length);
                if let Some(&sym) = self.lcx.unbound_vars.get(&key) {
                    self.locals.insert(sym, Local::RETURN_PLACE);
                }
            }
        }
        for o in olds {
            let ExprKind::Call { args, .. } = &o.kind else {
                continue;
            };
            let [arg] = args.as_slice() else {
                return self.unsupported(o.span, "this `old`");
            };
            let t = self.expr_ty(&arg.value)?;
            let l = self.temp(t);
            let bb = self.cur;
            self.b.push(bb, StatementKind::StorageLive(l));
            if self.is_copy(t) || !is_place_expr(&arg.value) {
                self.expr_into(&arg.value, Place::local(l))?;
            } else {
                let p = self.expr_place(&arg.value, false)?;
                self.clone_into(p, t, Place::local(l));
            }
            self.declare(l, t);
            self.old_vals.insert(o.id, l);
        }
        self.ensures = &f.ensures;
        self.b.suspends = f.effects.as_ref().is_some_and(|e| {
            e.items.iter().any(|i| {
                matches!(i, ast::EffectItem::Verb(v) if matches!(v.kind, ast::EffectVerbKind::Suspends))
            })
        });
        Ok(())
    }

    /// Aborts unless the contract condition `c` holds (a panic: exit 101).
    fn contract_check(&mut self, c: &'a Expr) -> R<()> {
        let bool_t = self.tys().bool();
        let v = self.temp(bool_t);
        self.push_scope();
        self.expr_into(c, Place::local(v))?;
        self.pop_scope()?;
        let bad = self.b.new_block();
        let ok = self.b.new_block();
        self.goto_with(
            TerminatorKind::SwitchInt {
                discr: Operand::Copy(Place::local(v)),
                targets: SwitchTargets::if_else(ok, bad),
            },
            bad,
        );
        self.diverge(TerminatorKind::Abort {
            reason: AbortReason::Panic,
        });
        self.cur = ok;
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
            StmtKind::Let {
                pattern, value, ty, ..
            } => {
                // The value's temporaries are the statement's; the bindings
                // belong to the enclosing block.
                if let PatternKind::Binding(name) = &pattern.kind {
                    // A closure or function item keeps its own type; any
                    // other function-typed value (a call's result, a
                    // field) is an erased `Fn` value.
                    let item = matches!(
                        value.kind,
                        ExprKind::Closure { .. } | ExprKind::Identifier(_) | ExprKind::Path { .. }
                    );
                    if item && self.is_fn_typed(pattern.id) {
                        self.push_scope();
                        let (op, t) = self.fn_value(value)?;
                        let l = self.user_local(name, t, pattern.id);
                        self.assign(l, Rvalue::Use(op));
                        self.pop_scope()?;
                        self.declare(l, t);
                        return Ok(());
                    }
                    let mut t = self.node_ty(pattern.id, pattern.span)?;
                    if ty.is_none() {
                        if let Some(rt) = self.ref_rebind(value, t)? {
                            t = rt;
                        }
                        if is_branching(value) {
                            t = self.arm_width(value, t);
                        }
                    }
                    let l = self.user_local(name, t, pattern.id);
                    self.push_scope();
                    // `let idx: i64 = m.get(k).unwrap()` reads the `ref i64`
                    // (core semantics §5.10).
                    let vt = self.expr_ty(value)?;
                    if ty.is_some()
                        && !matches!(self.tys().tcx().kind(t), HK::Ref(_) | HK::MutRef(_))
                        && matches!(self.tys().tcx().kind(vt), HK::Ref(i) | HK::MutRef(i) if i == t)
                    {
                        let op = self.owned_operand(value, t)?;
                        self.assign(l, Rvalue::Use(op));
                    } else {
                        self.expr_into(value, Place::local(l))?;
                    }
                    self.pop_scope()?;
                    self.declare(l, t);
                    return Ok(());
                }
                // `let _ = place;` binds nothing, so it moves nothing.
                if matches!(pattern.kind, PatternKind::Wildcard) && is_place_expr(value) {
                    return Ok(());
                }
                self.push_scope();
                // An owned place is destructured where it is (§4.6): the
                // parts it keeps live on in it. Anything else is evaluated
                // into a temporary that dies with the statement.
                let (src, t) = match self.owned_place(value)? {
                    Some(p) => {
                        let t = self.place_type(&p);
                        (p, t)
                    }
                    None => {
                        let t = self.expr_ty(value)?;
                        (Place::local(self.temp_of(value, t)?), t)
                    }
                };
                let mut binds = Vec::new();
                self.bind_irrefutable(pattern, src, t, &mut binds)?;
                self.pop_scope()?;
                for (l, t) in binds {
                    self.declare(l, t);
                }
                Ok(())
            }
            StmtKind::Expr(e) => {
                self.push_scope();
                let t = self.expr_ty(e)?;
                self.temp_of(e, t)?;
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
            StmtKind::LetElse {
                pattern,
                value,
                else_block,
                ..
            } => {
                // `let P = v else { diverge };`: the bindings belong to the
                // enclosing block; the scrutinee's temporaries are the
                // statement's, dropped on either path.
                self.push_scope();
                let (place, st, by_ref) = self.scrutinee(value)?;
                let fail = self.b.new_block();
                self.test_pattern(pattern, &place, st, fail)?;
                let matched = self.cur;
                self.cur = fail;
                self.push_scope();
                let t = self.unit();
                let tmp = self.temp(t);
                self.block_into(else_block, Place::local(tmp))?;
                self.pop_scope()?;
                // The else block diverges (the type checker requires it).
                self.diverge(TerminatorKind::Unreachable);
                self.cur = matched;
                let mut binds = Vec::new();
                self.bind_pattern(pattern, &place, st, by_ref, &mut binds)?;
                self.pop_scope()?;
                for (l, t) in binds {
                    self.declare(l, t);
                }
                Ok(())
            }
            StmtKind::Defer { body } => {
                self.schedule(ScopeEntry::Defer(body));
                Ok(())
            }
            StmtKind::ErrDefer { binding, body } => {
                let bound = binding.as_deref().map(|n| (n, s.id));
                self.schedule(ScopeEntry::ErrDefer(body, bound));
                Ok(())
            }
            // `let x: T;`: storage now, a value at its first assignment
            // (definite assignment is the move check's).
            StmtKind::LetUninit {
                name, name_span, ..
            } => {
                let key = SpanKey::from_span(name_span);
                let Some(ty) = self.lcx.tc.expr_types.get(&key) else {
                    return self.unsupported(s.span, "this statement");
                };
                let args = self.args.clone();
                let t = match self
                    .lcx
                    .lower_legacy(ty, &[])
                    .and_then(|h| self.lcx.mir_ty(h, &args))
                {
                    Ok(t) => t,
                    Err(e) => return self.unsupported(s.span, &e),
                };
                let l = self.b.push_local(
                    t,
                    Mutability::Mut,
                    LocalKind::User {
                        name: name.clone(),
                        node: s.id,
                    },
                );
                let key = (name.clone(), name_span.offset, name_span.length);
                if let Some(&sym) = self.lcx.unbound_vars.get(&key) {
                    self.locals.insert(sym, l);
                }
                let bb = self.cur;
                self.b.push(bb, StatementKind::StorageLive(l));
                self.declare(l, t);
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
            for &j in self.lcx.par_joins.get(&sym).into_iter().flatten() {
                self.locals.insert(j, l);
            }
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
            PatternKind::Slice { .. } | PatternKind::AtBinding { .. } => {
                self.bind_pattern(pat, &place, t, false, out)
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
        // `m[k] = v` on a map inserts (`IndexSet`, design.md §9): the
        // entry is created when the key is missing, and an old value is
        // handed back and dropped.
        if let ExprKind::Index { object, index } = &target.kind {
            if !matches!(index.kind, ExprKind::Range { .. }) {
                let ot = self.expr_ty(object)?;
                let (k, base) = self.strip_ty_full(ot);
                if let HK::Intrinsic {
                    kind: IntrinsicKind::Map | IntrinsicKind::SortedMap,
                    args,
                } = k
                {
                    let targs = self.tys().tcx().list(args);
                    return self.index_set(target, object, index, value, base, &targs);
                }
            }
        }
        // Left to right: the target's own subexpressions, then the value,
        // then the old value is dropped and the new one stored (D4).
        let pending = self.prepare_place(target)?;
        // A tuple literal is built at the target's element types, as a
        // `let`'s is at its annotation's (`b[1] = (7, 8)` into `(i32, i32)`).
        let op = if matches!(value.kind, ExprKind::Tuple(_)) {
            let tt = self.expr_ty(target)?;
            let (_, tt) = self.strip_ty_full(tt);
            self.operand_at(value, tt)?
        } else {
            self.expr_operand(value)?
        };
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

    /// `m[k] = v` on a map: `insert(&mut m, k, v)`, with the key, then
    /// the value, evaluated before the map's borrow starts (§5.6).
    fn index_set(
        &mut self,
        target: &'a Expr,
        object: &'a Expr,
        index: &'a Expr,
        value: &'a Expr,
        base: Ty,
        targs: &[Ty],
    ) -> R<()> {
        let (Some(&kt), Some(&vt)) = (targs.first(), targs.get(1)) else {
            return self.unsupported(target.span, "an index assignment to this map");
        };
        let Some(ret) = self.option_of(vt) else {
            return self.unsupported(target.span, "an index assignment without `Option`");
        };
        let recv = self.recv_place(object, true)?;
        let key = self.owned_operand(index, kt)?;
        let mut val = self.owned_operand(value, vt)?;
        val = if self.downgrades(&val, vt) {
            self.downgrade(val, vt)
        } else {
            self.widen(val, vt)
        };
        let mut rest = vec![key, val];
        let mut ops = vec![self.recv_borrow_after(recv, &mut rest)?];
        ops.extend(rest);
        let old = self.temp(ret);
        let name = format!("{}.insert", self.tys().display(base));
        self.call_native(&name, ops, Place::local(old));
        if self.needs_drop(ret) {
            let next = self.b.new_block();
            self.goto_with(
                TerminatorKind::Drop {
                    place: Place::local(old),
                    target: next,
                    unwind: UnwindAction::Abort,
                },
                next,
            );
        }
        Ok(())
    }

    /// `Option[t]`.
    fn option_of(&mut self, t: Ty) -> Option<Ty> {
        let table = &self.lcx.defs.table;
        let d = (0..table.len() as u32).map(DefId).find(|&d| {
            let e = table.get(d);
            e.kind == DefKind::Enum && e.path.segments.last().is_some_and(|s| s == "Option")
        })?;
        self.lcx.register_adt(d).ok()?;
        Some(self.tys().tcx().adt(d, &[t]))
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
        let rhs = if matches!(bin, BinOp::Shl | BinOp::Shr) {
            rhs
        } else {
            self.fit_const(rhs, t)
        };
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
            // `a < b` and the rest through the library's `String.lt` (byte
            // order): `a > b` is `b < a`, `a <= b` is `!(b < a)`.
            BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge => {
                let l = self.lib_arg(left, true)?;
                let r = self.lib_arg(right, true)?;
                let (args, negate) = match bin {
                    BinOp::Lt => (vec![l, r], false),
                    BinOp::Gt => (vec![r, l], false),
                    BinOp::Le => (vec![r, l], true),
                    _ => (vec![l, r], true),
                };
                if !negate {
                    self.call_native("String.lt", args, dest);
                } else {
                    let bool_t = self.tys().bool();
                    let t = self.temp(bool_t);
                    self.call_native("String.lt", args, Place::local(t));
                    self.assign(
                        dest,
                        Rvalue::UnaryOp(UnOp::Not, Operand::Move(Place::local(t))),
                    );
                }
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
            ExprKind::Identifier(_) => match self.lcx.res.get(&e.id) {
                Some(Res::Local(_)) => true,
                Some(Res::Def(d)) => self.lcx.statics.contains_key(d),
                _ => false,
            },
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
    /// Every subscript along the place is evaluated now, outermost
    /// object first (`hs[idx(1)].t.1[idx(0)]` calls `idx(1)` then
    /// `idx(0)`), before the value it is assigned (core semantics §8).
    fn prepare_place(&mut self, e: &'a Expr) -> R<PendingPlace<'a>> {
        self.pre_index(e)?;
        Ok(PendingPlace::Plain(e))
    }

    fn pre_index(&mut self, e: &'a Expr) -> R<()> {
        match &e.kind {
            ExprKind::Index { object, index } => {
                self.pre_index(object)?;
                if !matches!(index.kind, ExprKind::Range { .. }) {
                    let idx = match self.index_operand(object, index)? {
                        // Read now: the value may assign the variable.
                        Operand::Copy(p) => {
                            let t = self.place_type(&p);
                            let l = self.temp(t);
                            self.assign(l, Rvalue::Use(Operand::Copy(p)));
                            Operand::Copy(Place::local(l))
                        }
                        op => op,
                    };
                    self.pre_indices.insert(index.id, idx);
                }
                Ok(())
            }
            ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
                self.pre_index(object)
            }
            _ => Ok(()),
        }
    }

    fn finish_place(&mut self, p: PendingPlace<'a>, mutable: bool) -> R<Place> {
        match p {
            PendingPlace::Plain(e) => self.expr_place(e, mutable),
        }
    }

    /// The place `e` names. Field access through a reference derefs it.
    fn expr_place(&mut self, e: &'a Expr, mutable: bool) -> R<Place> {
        let p = self.expr_place_of(e, mutable)?;
        if mutable {
            return Ok(p);
        }
        // Reading a `weak` slot upgrades it: `Some` of a counted handle
        // while the object lives, else `None` (design.md, Cycles and
        // `weak`).
        let pt = self.place_type(&p);
        let HK::Weak(inner) = self.tys().tcx().kind(pt) else {
            return Ok(p);
        };
        let ot = self.expr_ty(e)?;
        if matches!(self.tys().tcx().kind(ot), HK::Weak(_)) {
            return Ok(p);
        }
        let rt = self.tys().tcx().reference(pt, false);
        let r = self.temp(rt);
        self.assign(r, Rvalue::Ref(BorrowKind::Shared, p));
        let _ = inner;
        let l = self.temp(ot);
        self.assign(
            l,
            Rvalue::Cast(CastKind::Upgrade, Operand::Move(Place::local(r)), ot),
        );
        if self.needs_drop(ot) {
            self.schedule(ScopeEntry::Drop(Place::local(l)));
        }
        Ok(Place::local(l))
    }

    fn expr_place_of(&mut self, e: &'a Expr, mutable: bool) -> R<Place> {
        match &e.kind {
            ExprKind::Identifier(name) => match self.lcx.res.get(&e.id) {
                Some(Res::Local(sym)) => match self.locals.get(sym) {
                    Some(&l) => Ok(Place::local(l)),
                    None => match self.captured.get(sym) {
                        Some(p) => Ok(p.clone()),
                        None => self.unsupported(e.span, &format!("the capture of `{name}`")),
                    },
                },
                Some(&Res::Def(d)) if self.lcx.statics.contains_key(&d) => Ok(self.static_place(d)),
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
                // A `Copy` part bound through a `ref` is bound by value
                // (`bind_one`), so `*min` of it is the binding itself.
                let pt = self.place_type(&p);
                if !matches!(
                    self.tys().tcx().kind(pt),
                    HK::Ref(_) | HK::MutRef(_) | HK::RawPtr { .. }
                ) && self.is_copy(pt)
                {
                    return Ok(p);
                }
                Ok(p.project(ProjElem::Deref))
            }
            _ => self.temp_place(e),
        }
    }

    /// The place of a module binding that lives in a static: `*_t`, with
    /// `_t` the static's borrow.
    fn static_place(&mut self, d: DefId) -> Place {
        let (i, t, mutable) = self.lcx.statics[&d];
        let rt = self.tys().tcx().reference(t, mutable);
        let l = self.temp(rt);
        self.assign(
            l,
            Rvalue::Use(Operand::Const(Const {
                ty: rt,
                kind: ConstKind::Static(i),
            })),
        );
        Place::local(l).project(ProjElem::Deref)
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
        if let Some(op) = self.pre_indices.remove(&index.id) {
            return Ok(op);
        }
        let mut t = self.expr_ty(object)?;
        while let HK::Ref(inner) | HK::MutRef(inner) = self.tys().tcx().kind(t) {
            t = inner;
        }
        if let HK::Intrinsic {
            kind: IntrinsicKind::Map | IntrinsicKind::SortedMap,
            ..
        } = self.tys().tcx().kind(t)
        {
            let kt = self.expr_ty(index)?;
            if !self.is_copy(kt) {
                return self.lib_arg(index, true);
            }
        }
        // A position held through a `mut ref` reads through it.
        Ok(self.scalar_operand(index)?.0)
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
        // A `Vec`, `VecDeque` or slice takes a `usize` position; a `Map`
        // its key.
        let (elem, positional) = match self.tys().tcx().kind(bt) {
            HK::Intrinsic {
                kind: IntrinsicKind::Vec | IntrinsicKind::VecDeque,
                args,
            } => (self.tys().tcx().list(args)[0], true),
            HK::Intrinsic {
                kind: IntrinsicKind::Map | IntrinsicKind::SortedMap,
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
        let l = self.temp_of(e, t)?;
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

    /// `e` taken as an owned value of type `want`. A borrowed place (a
    /// `ref` or `mut ref` parameter, which every bare parameter is under
    /// D5) is read through its reference: a `Copy` value copies, a handle
    /// counts, and anything else moves out of the borrow, which the borrow
    /// check refuses (core semantics §3.7).
    fn owned_operand(&mut self, e: &'a Expr, want: Ty) -> R<Operand> {
        let at = self.expr_ty(e)?;
        let inner = match self.tys().tcx().kind(at) {
            HK::Ref(t) | HK::MutRef(t) => t,
            _ => return self.expr_operand(e),
        };
        if inner != want {
            return self.expr_operand(e);
        }
        // A reference a call returns is read through its temporary.
        let p = if self.is_place(e) {
            self.expr_place(e, false)?
        } else {
            Place::local(self.temp_of(e, at)?)
        }
        .project(ProjElem::Deref);
        if self.is_handle(inner) || self.is_handle_aggregate(inner) {
            let l = self.temp(inner);
            self.count_copy(p, inner, Place::local(l));
            return Ok(Operand::Move(Place::local(l)));
        }
        Ok(self.use_place(p, inner))
    }

    /// `e` as an operand for a slot of type `want`. A tuple literal is
    /// built at the slot's element types, each element widened into its
    /// position: `(b, d)` with `b: u8` fills a `(i64, i64)` slot as
    /// `(b as i64, d as i64)`, not as the `(u8, u32)` the checker records.
    fn operand_at(&mut self, e: &'a Expr, want: Ty) -> R<Operand> {
        if let ExprKind::Tuple(es) = &e.kind {
            if let HK::Tuple(l) = self.tys().tcx().kind(want) {
                let wts = self.tys().tcx().list(l);
                if !es.is_empty() && wts.len() == es.len() && self.expr_ty(e)? != want {
                    let mut ops = Vec::with_capacity(es.len());
                    for (x, &wt) in es.iter().zip(&wts) {
                        ops.push(self.operand_at(x, wt)?);
                    }
                    let l = self.temp(want);
                    self.assign(l, Rvalue::Aggregate(AggregateKind::Tuple, ops));
                    return Ok(Operand::Move(Place::local(l)));
                }
            }
        }
        // A `ref` to a `Copy` value or handle in a value slot is read
        // (core semantics §5.10, §6.1).
        let et = self.expr_ty(e)?;
        if !matches!(self.tys().tcx().kind(want), HK::Ref(_) | HK::MutRef(_))
            && matches!(self.tys().tcx().kind(et), HK::Ref(_) | HK::MutRef(_))
        {
            return self.owned_operand(e, want);
        }
        let op = self.expr_operand(e)?;
        Ok(self.widen(op, want))
    }

    fn expr_operand(&mut self, e: &'a Expr) -> R<Operand> {
        if let Some(c) = self.constant(e)? {
            return Ok(Operand::Const(c));
        }
        if self.is_place(e) {
            let p = self.expr_place(e, false)?;
            let mut t = self.expr_ty(e)?;
            if let HK::Weak(_) = self.tys().tcx().kind(t) {
                // A strong handle the checker typed as the `weak` slot it
                // goes to: read it as the handle it is.
                t = self.place_type(&p);
            }
            if self.is_handle(t) || self.is_handle_aggregate(t) {
                let l = self.temp(t);
                self.count_copy(p, t, Place::local(l));
                return Ok(Operand::Move(Place::local(l)));
            }
            if let HK::MutRef(_) = self.tys().tcx().kind(t) {
                // A `mut ref` used as a value is reborrowed.
                let l = self.temp(t);
                self.assign(l, Rvalue::Ref(BorrowKind::Mut, p.project(ProjElem::Deref)));
                return Ok(Operand::Move(Place::local(l)));
            }
            return Ok(self.use_place(p, t));
        }
        let t = self.expr_ty(e)?;
        let l = self.temp_of(e, t)?;
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
            ExprKind::Float(f, suffix) => {
                // A suffixed literal is a value of its suffix's type even
                // where it widens (`let w: f64 = 0.1f32`).
                use crate::token::FloatSuffix;
                let f = match suffix {
                    Some(FloatSuffix::F32) => FloatTy::F32.round(*f),
                    Some(FloatSuffix::BF16) => FloatTy::BF16.round(*f),
                    Some(FloatSuffix::F16) => FloatTy::F16.round(*f),
                    _ => *f,
                };
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
            ExprKind::Binary { .. } | ExprKind::Unary { .. } if fold_int(e).is_some() => {
                // An operator tree of unsuffixed integer literals is one
                // constant, so it takes its destination's type as a single
                // literal does (`let n: i32 = -(2 * 3)`).
                scalar(self, fold_int(e).unwrap_or(0) as u128)
            }
            ExprKind::Unary {
                op: UnaryOp::Neg,
                operand,
            } => match &operand.kind {
                ExprKind::Integer(v, _) => scalar(self, (-*v) as u128),
                ExprKind::Float(f, _) => {
                    let ty = self.expr_ty(e)?;
                    Ok(Some(Const {
                        ty,
                        kind: ConstKind::Float((-*f).to_bits()),
                    }))
                }
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
            ExprKind::StringLit(s) => {
                let op = self.static_str(s);
                self.call_native("String.from", vec![op], dest);
                Ok(())
            }
            ExprKind::Identifier(_) if !self.is_local(e) => self.path_value(e, dest),
            ExprKind::Closure { .. } => {
                // A closure stored as a function value escapes, so it
                // captures by move (core semantics §9.3).
                let dt = self.place_type(&dest);
                self.closure_escapes = matches!(self.tys().tcx().kind(dt), HK::Fn { .. });
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
                let t = self.expr_ty(e)?;
                let range = (start.as_deref(), end.as_deref(), *inclusive);
                let s = self.slice_view(object, Some(range), t, false)?;
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
                // A `mut ref` used as a value is reborrowed, `&mut (*r)`, so
                // its source keeps it (as when it is passed on).
                if let (HK::MutRef(inner), HK::Ref(di) | HK::MutRef(di)) =
                    (self.tys().tcx().kind(t), self.tys().tcx().kind(dt))
                {
                    if di == inner {
                        let kind = if mutable {
                            BorrowKind::Mut
                        } else {
                            BorrowKind::Shared
                        };
                        self.assign(dest, Rvalue::Ref(kind, p.project(ProjElem::Deref)));
                        return Ok(());
                    }
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
                let (o, _) = self.scalar_operand(operand)?;
                self.assign(dest, Rvalue::UnaryOp(mop, o));
                Ok(())
            }
            ExprKind::Tuple(es) => {
                // Each element is built at its position's type in the
                // destination, which may be wider than the element's own.
                let dt = self.place_type(&dest);
                let wts = match self.tys().tcx().kind(dt) {
                    HK::Tuple(l) => self.tys().tcx().list(l),
                    _ => Vec::new(),
                };
                let mut ops = Vec::new();
                for (i, x) in es.iter().enumerate() {
                    ops.push(match wts.get(i) {
                        Some(&wt) if wts.len() == es.len() => self.operand_at(x, wt)?,
                        _ => self.expr_operand(x)?,
                    });
                }
                self.assign(dest, Rvalue::Aggregate(AggregateKind::Tuple, ops));
                Ok(())
            }
            ExprKind::ArrayLiteral(es) => self.array_literal(e, es, dest),
            // `b"..."` is an `Array[u8, N]` of its bytes (design.md § Byte
            // and byte-string literals).
            ExprKind::ByteStringLit(bytes) => {
                let u8_t = self.tys().tcx().intern(HK::UInt(UIntSize::U8));
                let ops = bytes
                    .iter()
                    .map(|&b| {
                        Operand::Const(Const {
                            ty: u8_t,
                            kind: ConstKind::Scalar(b as u128),
                        })
                    })
                    .collect();
                self.assign(dest, Rvalue::Aggregate(AggregateKind::Array(u8_t), ops));
                Ok(())
            }
            ExprKind::MapLiteral { entries, .. } => self.map_literal(e, entries, dest),
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
                    let Some((idx, ft)) = self.field_of(t, variant, &fi.name) else {
                        return self.unsupported(fi.span, "this field");
                    };
                    let op = self.operand_at(&fi.value, ft)?;
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
            ExprKind::Par(b) => self.par_block(e, b, dest),
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
                    let (char_k, bool_k) =
                        (|t| tcx.kind(t) == HK::Char, |t| tcx.kind(t) == HK::Bool);
                    match (int(from) || from == to, float(from), int(to), float(to)) {
                        _ if from == to => None,
                        _ if char_k(to) && matches!(tcx.kind(from), HK::UInt(UIntSize::U8)) => {
                            Some(CastKind::IntToChar)
                        }
                        _ if char_k(from) && int(to) => Some(CastKind::CharToInt),
                        _ if bool_k(from) && int(to) => Some(CastKind::BoolToInt),
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
                par: Some(par),
                ..
            } => self.par_for(e, label, pattern, iterable, body, par, dest),
            ExprKind::For {
                label,
                pattern,
                iterable,
                body,
                ..
            } => self.for_range(e, label, pattern, iterable, body, dest),
            ExprKind::Return(value) => {
                let ret = Place::local(Local::RETURN_PLACE);
                // A closure whose every exit is a `return` was typed as
                // returning `!`; its returns say what it gives back.
                if let Some(v) = value {
                    let never = self.tys().tcx().intern(HK::Never);
                    if self.b.local_ty(Local::RETURN_PLACE) == never {
                        let vt = self.expr_ty(v)?;
                        if vt != never {
                            self.b.set_local_ty(Local::RETURN_PLACE, vt);
                        }
                    }
                }
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
            ExprKind::NilCoalesce { left, right } => self.nil_coalesce(left, right, dest),
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
        // `None` stored into a `weak` slot is an empty weak reference: the
        // `None` downgraded, the same shape as an `Option` value stored there.
        let t = self.expr_ty(e)?;
        if let HK::Weak(inner) = self.tys().tcx().kind(t) {
            let Some(ot) = self.option_of(inner) else {
                return self.unsupported(e.span, "this path");
            };
            let none = self
                .tys()
                .tcx()
                .adt_of(ot)
                .and_then(|(a, _)| a.variants.iter().position(|v| v.name == "None"));
            let Some(idx) = none else {
                return self.unsupported(e.span, "this path");
            };
            let l = self.temp(ot);
            let kind = self.adt_aggregate(ot, idx as u32);
            self.assign(l, Rvalue::Aggregate(kind, Vec::new()));
            let w = self.downgrade(Operand::Move(Place::local(l)), t);
            self.assign(dest, Rvalue::Use(w));
            return Ok(());
        }
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
                    None if self.lcx.statics.contains_key(&d) => {
                        let p = self.static_place(d);
                        let t = self.place_type(&p);
                        if !self.is_copy(t) {
                            return self.unsupported(e.span, "a move out of a module binding");
                        }
                        self.assign(dest, Rvalue::Use(Operand::Copy(p)));
                        Ok(())
                    }
                    None => match self.lcx.consts.get(&d) {
                        // A constant's value is computed where it is used.
                        Some(value) => self.const_value(e, value, dest),
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
            ops.push(self.operand_at(x, elem)?);
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

    /// `{k: v, ..}`: a new map, and each entry inserted in order, a later
    /// duplicate key replacing the earlier value.
    fn map_literal(&mut self, e: &'a Expr, entries: &'a [(Expr, Expr)], dest: Place) -> R<()> {
        let t = self.expr_ty(e)?;
        let targs = match self.tys().tcx().kind(t) {
            HK::Intrinsic {
                kind: IntrinsicKind::Map | IntrinsicKind::SortedMap,
                args,
            } => self.tys().tcx().list(args),
            _ => return self.unsupported(e.span, "this map literal"),
        };
        let (kt, vt) = (targs[0], targs[1]);
        let Some(ret) = self.option_of(vt) else {
            return self.unsupported(e.span, "a map literal without `Option`");
        };
        let name = self.tys().display(t);
        self.call_native(&format!("{name}.new"), Vec::new(), dest.clone());
        for (k, v) in entries {
            let key = self.owned_operand(k, kt)?;
            let key = self.widen(key, kt);
            let val = self.owned_operand(v, vt)?;
            let val = if self.downgrades(&val, vt) {
                self.downgrade(val, vt)
            } else {
                self.widen(val, vt)
            };
            let rt = self.tys().tcx().reference(t, true);
            let r = self.temp(rt);
            self.assign(r, Rvalue::Ref(BorrowKind::Mut, dest.clone()));
            let old = self.temp(ret);
            self.call_native(
                &format!("{name}.insert"),
                vec![Operand::Move(Place::local(r)), key, val],
                Place::local(old),
            );
            if self.needs_drop(ret) {
                let next = self.b.new_block();
                self.goto_with(
                    TerminatorKind::Drop {
                        place: Place::local(old),
                        target: next,
                        unwind: UnwindAction::Abort,
                    },
                    next,
                );
            }
        }
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
        if !scalar(self.tys().tcx().kind(lbase))
            && matches!(bin, BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge)
        {
            return self.ordered_exprs(left, bin, right, dest);
        }
        let (l, lt) = self.scalar_operand(left)?;
        if !self.is_copy(lt) {
            return self.unsupported(left.span, "an operator on a non-scalar");
        }
        let (r, rt) = self.scalar_operand(right)?;
        // An unsuffixed literal takes the other operand's type (the
        // checker records `x * 2` with `x: f64` as an `i64` literal). A
        // shift's amount keeps its own.
        let shift = matches!(bin, BinOp::Shl | BinOp::Shr);
        let lit = |o: &Operand| matches!(o, Operand::Const(_));
        let t = if lit(&l) && !lit(&r) && !shift {
            rt
        } else {
            lt
        };
        let l = self.fit_const(l, t);
        let r = if shift { r } else { self.fit_const(r, t) };
        // Operands of different widths meet at the wider one when the
        // widening is lossless (design.md, implicit widening).
        let (l, r, t) = match (self.operand_ty(&l), self.operand_ty(&r)) {
            (a, b) if a != b && !shift && self.lossless(a, b) => (self.widen(l, b), r, b),
            (a, b) if a != b && !shift && self.lossless(b, a) => (l, self.widen(r, a), a),
            _ => (l, r, t),
        };
        self.arith(bin, l, r, t, dest);
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

    /// `a < b` on a type ordered by `Ord`: `a.cmp(b)`, and a test of the
    /// `Ordering` it returns. `<` and `>` require `Less` and `Greater`; `<=`
    /// and `>=` rule out `Greater` and `Less`.
    fn ordered_exprs(&mut self, left: &'a Expr, bin: BinOp, right: &'a Expr, dest: Place) -> R<()> {
        let lp = self.expr_place(left, false)?;
        let rp = self.expr_place(right, false)?;
        let (lp, lt) = self.strip_refs(lp);
        let (rp, _) = self.strip_refs(rp);
        let (ot, [less, _, greater]) = self.ordering_ty(left.span)?;
        let (want, eq) = match bin {
            BinOp::Lt => (less, true),
            BinOp::Gt => (greater, true),
            BinOp::Le => (greater, false),
            _ => (less, false),
        };
        let o = self.temp(ot);
        self.cmp_places(left.span, lp, rp, lt, Place::local(o))?;
        let isize_t = self.tys().tcx().intern(HK::Int(IntSize::I64));
        let disc = self.temp(isize_t);
        self.assign(disc, Rvalue::Discriminant(Place::local(o)));
        let want = Operand::Const(Const {
            ty: isize_t,
            kind: ConstKind::Scalar(want as u128),
        });
        let op = if eq { BinOp::Eq } else { BinOp::Ne };
        self.assign(
            dest,
            Rvalue::BinaryOp(op, Operand::Copy(Place::local(disc)), want),
        );
        Ok(())
    }

    /// The `Ordering` type, with the indices of `Less`, `Equal` and
    /// `Greater`.
    fn ordering_ty(&mut self, span: Span) -> R<(Ty, [u32; 3])> {
        let Some(ord) = self.lcx.defs.lookup(0, "Ordering") else {
            return self.unsupported(span, "an ordering with no `Ordering`");
        };
        let ot = self.tys().tcx().adt(ord, &[]);
        let ot = match self.lcx.mir_ty(ot, &[]) {
            Ok(t) => t,
            Err(err) => return self.unsupported(span, &err),
        };
        let Some((adt, _)) = self.tys().tcx().adt_of(ot) else {
            return self.unsupported(span, "an ordering with no `Ordering`");
        };
        let variant = |name: &str| {
            adt.variants
                .iter()
                .position(|v| v.name == name)
                .map(|i| i as u32)
        };
        match (variant("Less"), variant("Equal"), variant("Greater")) {
            (Some(l), Some(e), Some(g)) => Ok((ot, [l, e, g])),
            _ => self.unsupported(span, "an ordering with no `Ordering`"),
        }
    }

    /// `dest = l.cmp(r)` for two places of type `t`: the user's `Ord` body
    /// when the type has one; otherwise the order a derive gives, scalars
    /// by value, `String`s by the library, and aggregates field by field.
    fn cmp_places(&mut self, span: Span, l: Place, r: Place, t: Ty, dest: Place) -> R<()> {
        let (ot, [less, equal, greater]) = self.ordering_ty(span)?;
        match self.tys().tcx().kind(t) {
            HK::Int(_) | HK::UInt(_) | HK::Float(_) | HK::Bool | HK::Char => {
                let bool_t = self.tys().bool();
                let join = self.b.new_block();
                for (op, variant) in [(BinOp::Lt, less), (BinOp::Gt, greater)] {
                    let c = self.temp(bool_t);
                    self.assign(
                        c,
                        Rvalue::BinaryOp(op, Operand::Copy(l.clone()), Operand::Copy(r.clone())),
                    );
                    let yes = self.b.new_block();
                    let no = self.b.new_block();
                    self.goto_with(
                        TerminatorKind::SwitchInt {
                            discr: Operand::Copy(Place::local(c)),
                            targets: SwitchTargets::if_else(yes, no),
                        },
                        yes,
                    );
                    let kind = self.adt_aggregate(ot, variant);
                    self.assign(dest.clone(), Rvalue::Aggregate(kind, Vec::new()));
                    self.goto(join);
                    self.cur = no;
                }
                let kind = self.adt_aggregate(ot, equal);
                self.assign(dest, Rvalue::Aggregate(kind, Vec::new()));
                self.goto(join);
                self.cur = join;
                Ok(())
            }
            HK::Ref(inner) | HK::MutRef(inner) => self.cmp_places(
                span,
                l.project(ProjElem::Deref),
                r.project(ProjElem::Deref),
                inner,
                dest,
            ),
            HK::Str => {
                let lo = self.ref_to(l, t);
                let ro = self.ref_to(r, t);
                self.call_native("String.cmp", vec![lo, ro], dest);
                Ok(())
            }
            HK::Tuple(list) => {
                let tys = self.tys().tcx().list(list);
                let parts = tys
                    .iter()
                    .enumerate()
                    .map(|(i, &ft)| (l.field(i as u32, ft), r.field(i as u32, ft), ft))
                    .collect();
                self.lex_cmp(span, parts, dest)
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
                self.lex_cmp(span, parts, dest)
            }
            HK::Adt { .. } | HK::Shared { .. } if self.total_float_wrapper(t).is_some() => {
                // `F64` / `F32` order their field totally: `-0.0 < 0.0`, and
                // a NaN equals itself.
                let (float, ft) = self.total_float_wrapper(t).expect("checked");
                let lo = self.ref_to(l.field(0, ft), ft);
                let ro = self.ref_to(r.field(0, ft), ft);
                self.call_native(&format!("{float}.total_cmp"), vec![lo, ro], dest);
                Ok(())
            }
            HK::Adt { .. } | HK::Shared { .. } => {
                if let Some((d, args)) = self.user_impl_method(t, "Ord", "cmp") {
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
                            return self.unsupported(span, "ordering this type");
                        };
                        parts.push((l.clone().field(i, ft), r.clone().field(i, ft), ft));
                    }
                    return self.lex_cmp(span, parts, dest);
                }
                self.enum_cmp(span, l, r, t, &adt, dest)
            }
            _ => self.unsupported(span, "ordering this type"),
        }
    }

    /// The library's total-order float wrapper `t` is (`F64`, `F32`): the
    /// float type its one field holds, and that field's type.
    fn total_float_wrapper(&self, t: Ty) -> Option<(&'static str, Ty)> {
        let (adt, _) = self.tys().tcx().adt_of(t)?;
        let float = match adt.name.as_str() {
            "F64" => "f64",
            "F32" => "f32",
            _ => return None,
        };
        let ft = self.tys().tcx().field_ty(t, None, 0)?;
        matches!(self.tys().tcx().kind(ft), HK::Float(_)).then_some((float, ft))
    }

    /// `dest` is the first pair's ordering that is not `Equal`, else `Equal`.
    fn lex_cmp(&mut self, span: Span, parts: Vec<(Place, Place, Ty)>, dest: Place) -> R<()> {
        let (ot, [_, equal, _]) = self.ordering_ty(span)?;
        let (isize_t, bool_t) = {
            let tcx = self.tys().tcx();
            (tcx.intern(HK::Int(IntSize::I64)), tcx.intern(HK::Bool))
        };
        let join = self.b.new_block();
        for (lp, rp, ft) in parts {
            let o = self.temp(ot);
            self.cmp_places(span, lp, rp, ft, Place::local(o))?;
            let disc = self.temp(isize_t);
            self.assign(disc, Rvalue::Discriminant(Place::local(o)));
            let same = self.temp(bool_t);
            self.assign(
                same,
                Rvalue::BinaryOp(
                    BinOp::Eq,
                    Operand::Copy(Place::local(disc)),
                    Operand::Const(Const {
                        ty: isize_t,
                        kind: ConstKind::Scalar(equal as u128),
                    }),
                ),
            );
            let next = self.b.new_block();
            let differ = self.b.new_block();
            self.goto_with(
                TerminatorKind::SwitchInt {
                    discr: Operand::Copy(Place::local(same)),
                    targets: SwitchTargets::if_else(next, differ),
                },
                differ,
            );
            self.assign(dest.clone(), Rvalue::Use(Operand::Copy(Place::local(o))));
            self.goto(join);
            self.cur = next;
        }
        let kind = self.adt_aggregate(ot, equal);
        self.assign(dest, Rvalue::Aggregate(kind, Vec::new()));
        self.goto(join);
        self.cur = join;
        Ok(())
    }

    /// Enum values order by variant, in declaration order, and then by
    /// their payloads.
    fn enum_cmp(
        &mut self,
        span: Span,
        l: Place,
        r: Place,
        t: Ty,
        adt: &crate::ty::AdtDef,
        dest: Place,
    ) -> R<()> {
        let isize_t = self.tys().tcx().intern(HK::Int(IntSize::I64));
        let dl = self.temp(isize_t);
        self.assign(dl, Rvalue::Discriminant(l.clone()));
        let dr = self.temp(isize_t);
        self.assign(dr, Rvalue::Discriminant(r.clone()));
        let join = self.b.new_block();
        let switch = self.b.new_block();
        // The variants' order is their indices' order.
        self.cmp_places(
            span,
            Place::local(dl),
            Place::local(dr),
            isize_t,
            dest.clone(),
        )?;
        let (_, [_, equal, _]) = self.ordering_ty(span)?;
        let bool_t = self.tys().bool();
        let disc = self.temp(isize_t);
        self.assign(disc, Rvalue::Discriminant(dest.clone()));
        let same = self.temp(bool_t);
        self.assign(
            same,
            Rvalue::BinaryOp(
                BinOp::Eq,
                Operand::Copy(Place::local(disc)),
                Operand::Const(Const {
                    ty: isize_t,
                    kind: ConstKind::Scalar(equal as u128),
                }),
            ),
        );
        let differ = self.b.new_block();
        self.goto_with(
            TerminatorKind::SwitchInt {
                discr: Operand::Copy(Place::local(same)),
                targets: SwitchTargets::if_else(switch, differ),
            },
            differ,
        );
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
                    return self.unsupported(span, "ordering this type");
                };
                let down = |p: &Place| p.project(ProjElem::Downcast(VariantIdx(vi))).field(fi, ft);
                parts.push((down(&l), down(&r), ft));
            }
            self.lex_cmp(span, parts, dest.clone())?;
            self.goto(join);
        }
        self.cur = join;
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

    /// `dest = p.clone()` for a place of type `t`: a copy, a counted copy
    /// of a handle, a tuple part by part, else the library's `clone`.
    fn clone_into(&mut self, p: Place, t: Ty, dest: Place) {
        if self.is_copy(t) {
            self.assign(dest, Rvalue::Use(Operand::Copy(p)));
            return;
        }
        if self.is_handle(t) || self.is_handle_aggregate(t) {
            self.count_copy(p, t, dest);
            return;
        }
        if let HK::Tuple(parts) = self.tys().tcx().kind(t) {
            let parts = self.tys().tcx().list(parts).to_vec();
            let mut ops = Vec::new();
            for (i, pt) in parts.into_iter().enumerate() {
                let l = self.temp(pt);
                self.clone_into(p.field(i as u32, pt), pt, Place::local(l));
                ops.push(Operand::Move(Place::local(l)));
            }
            self.assign(dest, Rvalue::Aggregate(AggregateKind::Tuple, ops));
            return;
        }
        if let HK::Array {
            elem,
            len: crate::ty::ArrayLen::Known(n),
        } = self.tys().tcx().kind(t)
        {
            let mut ops = Vec::new();
            for i in 0..n {
                let l = self.temp(elem);
                self.clone_into(
                    p.clone().project(ProjElem::ConstIndex(i)),
                    elem,
                    Place::local(l),
                );
                ops.push(Operand::Move(Place::local(l)));
            }
            self.assign(dest, Rvalue::Aggregate(AggregateKind::Array(elem), ops));
            return;
        }
        let r = self.ref_to(p, t);
        let name = format!("{}.clone", self.tys().display(t));
        self.call_native(&name, vec![r], dest);
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
            HK::Intrinsic {
                kind: IntrinsicKind::Vec,
                args,
            } => {
                let elem = self.tys().tcx().list(args)[0];
                self.seq_eq(span, l, r, t, elem, dest)
            }
            HK::Slice { elem, .. } => self.seq_eq(span, l, r, t, elem, dest),
            HK::Adt { .. } | HK::Shared { .. } if self.total_float_wrapper(t).is_some() => {
                let (ot, [_, equal, _]) = self.ordering_ty(span)?;
                let o = self.temp(ot);
                self.cmp_places(span, l, r, t, Place::local(o))?;
                let isize_t = self.tys().tcx().intern(HK::Int(IntSize::I64));
                let disc = self.temp(isize_t);
                self.assign(disc, Rvalue::Discriminant(Place::local(o)));
                let want = Operand::Const(Const {
                    ty: isize_t,
                    kind: ConstKind::Scalar(equal as u128),
                });
                self.assign(
                    dest,
                    Rvalue::BinaryOp(BinOp::Eq, Operand::Copy(Place::local(disc)), want),
                );
                Ok(())
            }
            HK::Adt { .. } | HK::Shared { .. } => {
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
    /// `dest = l == r` for two `Vec`s or slices of `elem`: the same length
    /// and equal elements, compared in order until one differs.
    fn seq_eq(&mut self, span: Span, l: Place, r: Place, t: Ty, elem: Ty, dest: Place) -> R<()> {
        let (bool_t, usize_t, rt, et) = {
            let tcx = self.tys().tcx();
            (
                tcx.intern(HK::Bool),
                tcx.intern(HK::UInt(UIntSize::Usize)),
                tcx.reference(t, false),
                tcx.reference(elem, false),
            )
        };
        let ty_name = self.tys().display(t);
        let (lr, rr) = (self.temp(rt), self.temp(rt));
        self.assign(lr, Rvalue::Ref(BorrowKind::Shared, l));
        self.assign(rr, Rvalue::Ref(BorrowKind::Shared, r));
        let (nl, nr) = (self.temp(usize_t), self.temp(usize_t));
        let len = format!("{ty_name}.len");
        self.call_native(
            &len,
            vec![Operand::Copy(Place::local(lr))],
            Place::local(nl),
        );
        self.call_native(
            &len,
            vec![Operand::Copy(Place::local(rr))],
            Place::local(nr),
        );
        let c = self.temp(bool_t);
        self.assign(
            c,
            Rvalue::BinaryOp(
                BinOp::Eq,
                Operand::Copy(Place::local(nl)),
                Operand::Copy(Place::local(nr)),
            ),
        );
        let (differ, join, head, body, step, same) = (
            self.b.new_block(),
            self.b.new_block(),
            self.b.new_block(),
            self.b.new_block(),
            self.b.new_block(),
            self.b.new_block(),
        );
        let i = self.temp(usize_t);
        self.assign(
            i,
            Rvalue::Use(Operand::Const(Const {
                ty: usize_t,
                kind: ConstKind::Scalar(0),
            })),
        );
        self.goto_with(
            TerminatorKind::SwitchInt {
                discr: Operand::Copy(Place::local(c)),
                targets: SwitchTargets::if_else(head, differ),
            },
            head,
        );
        let more = self.temp(bool_t);
        self.assign(
            more,
            Rvalue::BinaryOp(
                BinOp::Lt,
                Operand::Copy(Place::local(i)),
                Operand::Copy(Place::local(nl)),
            ),
        );
        self.goto_with(
            TerminatorKind::SwitchInt {
                discr: Operand::Copy(Place::local(more)),
                targets: SwitchTargets::if_else(body, same),
            },
            body,
        );
        let index = format!("{ty_name}.index");
        let (el, er) = (self.temp(et), self.temp(et));
        self.call_native(
            &index,
            vec![
                Operand::Copy(Place::local(lr)),
                Operand::Copy(Place::local(i)),
            ],
            Place::local(el),
        );
        self.call_native(
            &index,
            vec![
                Operand::Copy(Place::local(rr)),
                Operand::Copy(Place::local(i)),
            ],
            Place::local(er),
        );
        let e = self.temp(bool_t);
        self.eq_places(
            span,
            Place::local(el).project(ProjElem::Deref),
            Place::local(er).project(ProjElem::Deref),
            elem,
            Place::local(e),
        )?;
        self.goto_with(
            TerminatorKind::SwitchInt {
                discr: Operand::Copy(Place::local(e)),
                targets: SwitchTargets::if_else(step, differ),
            },
            step,
        );
        let one = Operand::Const(Const {
            ty: usize_t,
            kind: ConstKind::Scalar(1),
        });
        self.assign(
            i,
            Rvalue::BinaryOp(BinOp::Add, Operand::Copy(Place::local(i)), one),
        );
        self.goto(head);
        self.cur = same;
        self.assign(dest.clone(), Rvalue::Use(bool_const(bool_t, true)));
        self.goto(join);
        self.cur = differ;
        self.assign(dest, Rvalue::Use(bool_const(bool_t, false)));
        self.goto(join);
        self.cur = join;
        Ok(())
    }

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
    /// Queue the user `Display` body of every type a printed `t` contains,
    /// for the interpreter to show it by at any depth (a `Vec[P]`, an
    /// `Option[P]`, a field of type `P`). A type with its own `Display`
    /// is shown by that body alone, so its parts are not visited.
    fn queue_nested_displays(&mut self, t: Ty) {
        let mut seen = FxHashSet::default();
        let mut work = vec![t];
        while let Some(t) = work.pop() {
            if !seen.insert(t) {
                continue;
            }
            let tcx = self.tys().tcx();
            match tcx.kind(t) {
                HK::Ref(x) | HK::MutRef(x) => work.push(x),
                HK::Tuple(l) => work.extend(tcx.list(l)),
                HK::Array { elem, .. } | HK::Slice { elem, .. } => work.push(elem),
                HK::Intrinsic { args, .. } => work.extend(tcx.list(args)),
                HK::Adt { args, .. } | HK::Shared { args, .. } => {
                    if let Some((d, args)) = self.user_impl_method(t, "Display", "to_string") {
                        self.lcx.instance(d, args);
                        continue;
                    }
                    let tcx = self.tys().tcx();
                    work.extend(tcx.list(args));
                    if let Some((adt, _)) = tcx.adt_of(t) {
                        for (k, v) in adt.variants.iter().enumerate() {
                            let variant = adt.is_enum.then_some(k as u32);
                            for i in 0..v.fields.len() {
                                work.extend(tcx.field_ty(t, variant, i as u32));
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// Queue the user `PartialEq.eq` of a collection's element or key
    /// type, which the interpreter compares keys and searches by.
    fn queue_key_eq(&mut self, collection: Ty) {
        let (_, base) = self.strip_ty_full(collection);
        let elem = match self.tys().tcx().kind(base) {
            HK::Intrinsic { args, .. } => self.tys().tcx().list(args).first().copied(),
            _ => None,
        };
        if let Some(e) = elem {
            let (_, e) = self.strip_ty_full(e);
            if let Some((d, args)) = self.user_impl_method(e, "PartialEq", "eq") {
                self.lcx.instance(d, args);
            }
        }
    }

    fn user_impl_method(&self, t: Ty, tr: &str, method: &str) -> Option<(DefId, Vec<Ty>)> {
        let (adt, args) = self.tys().tcx().adt_of(t)?;
        let mut path = self.lcx.defs.table.get(adt.def).path.segments.clone();
        path.push(format!("impl {tr}#0"));
        path.push(method.to_string());
        let d = self.lcx.defs.table.lookup(&DefPath::new(path))?;
        let d = self.lcx.lowered_def(d)?;
        let item = self.lcx.fns.get(&d)?;
        Some((d, item.impl_args(&args)?))
    }

    /// The `from` of the user's non-generic `impl From[source] for
    /// target`, as `?` converts an error.
    fn user_from(&mut self, target: Ty, source: Ty) -> Option<DefId> {
        let (adt, _) = self.tys().tcx().adt_of(target)?;
        let base = self.lcx.defs.table.get(adt.def).path.segments.clone();
        for k in 0.. {
            let mut path = base.clone();
            path.push(format!("impl From#{k}"));
            path.push("from".to_string());
            let d = self.lcx.defs.table.lookup(&DefPath::new(path))?;
            let item = self.lcx.fns.get(&d)?;
            if item.impl_params != 0 {
                continue;
            }
            let p = item.f.params.first()?;
            let t = *self.lcx.node_types.get(&p.pattern.id)?;
            if self.lcx.mir_ty(t, &[]).ok() == Some(source) {
                return Some(d);
            }
        }
        None
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
        let r = self.for_body(body);
        self.loops.pop();
        r?;
        self.goto(continue_bb);
        Ok(())
    }

    /// A loop body, whose value is dropped, or for the body of a `par for`,
    /// pushed onto the `Vec` the loop builds.
    fn for_body(&mut self, body: &'a Block) -> R<()> {
        let acc = match self.par_acc {
            Some((b, acc, vt, region)) if std::ptr::eq(b, body) => {
                self.par_acc = None;
                Some((acc, vt, region))
            }
            _ => None,
        };
        let Some((acc, vt, region)) = acc else {
            let t = self.unit();
            let tmp = self.temp(t);
            return self.block_into(body, Place::local(tmp));
        };
        let et = match self.tys().tcx().kind(vt) {
            HK::Intrinsic {
                kind: IntrinsicKind::Vec,
                args,
            } => self.tys().tcx().list(args)[0],
            _ => return self.unsupported(body.span, "this `par for`"),
        };
        let v = self.temp(et);
        let entry = self.branch_begin();
        self.block_into(body, Place::local(v))?;
        self.branch_end(region, entry);
        let rt = self.tys().tcx().reference(vt, true);
        let r = self.temp(rt);
        self.assign(r, Rvalue::Ref(BorrowKind::Mut, Place::local(acc)));
        let u = self.unit();
        let u = self.temp(u);
        let name = format!("{}.push", self.tys().display(vt));
        self.call_native(
            &name,
            vec![
                Operand::Move(Place::local(r)),
                Operand::Move(Place::local(v)),
            ],
            Place::local(u),
        );
        Ok(())
    }

    /// Opens a `par` region whose branches are filled in as they lower.
    fn open_par_region(&mut self, kind: ParKind, span: Span) -> usize {
        self.b.par_regions.push(ParRegion {
            kind,
            span,
            branches: Vec::new(),
        });
        self.b.par_regions.len() - 1
    }

    /// Starts a branch in a block of its own, so the branch's blocks are
    /// exactly those opened from here to [`Self::branch_end`].
    fn branch_begin(&mut self) -> BasicBlock {
        let entry = self.b.new_block();
        self.goto(entry);
        self.cur = entry;
        entry
    }

    /// Ends the branch begun at `entry`, recording its blocks in `region`,
    /// and continues in a fresh block.
    fn branch_end(&mut self, region: usize, entry: BasicBlock) {
        let end = self.b.block_count() as u32;
        let blocks = (entry.0..end).map(BasicBlock).collect();
        self.b.par_regions[region].branches.push(blocks);
        let after = self.b.new_block();
        self.goto(after);
        self.cur = after;
    }

    /// `par { a, b }`: each expression is a branch and the value is the
    /// tuple of theirs. The older `par { s1; s2; }` has one branch per
    /// statement. Branches that do not conflict may run in any order, so
    /// running them in source order is one of the allowed schedules.
    fn par_block(&mut self, e: &'a Expr, b: &'a Block, dest: Place) -> R<()> {
        let region = self.open_par_region(ParKind::Block, e.span);
        if b.stmts.is_empty() {
            if let Some(ExprKind::Tuple(es)) = b.final_expr.as_deref().map(|x| &x.kind) {
                if !es.is_empty() {
                    let mut ops = Vec::new();
                    for x in es {
                        let entry = self.branch_begin();
                        ops.push(self.expr_operand(x)?);
                        self.branch_end(region, entry);
                    }
                    self.assign(dest, Rvalue::Aggregate(AggregateKind::Tuple, ops));
                    return Ok(());
                }
            }
        }
        // The branches' bindings join the enclosing scope, so they live on
        // past the block.
        for s in &b.stmts {
            let entry = self.branch_begin();
            self.stmt(s)?;
            self.branch_end(region, entry);
        }
        match &b.final_expr {
            Some(x) => {
                let entry = self.branch_begin();
                self.push_scope();
                self.expr_into(x, dest)?;
                self.pop_scope()?;
                self.branch_end(region, entry);
            }
            None => self.assign(dest, Rvalue::Use(unit_const(self.unit()))),
        }
        Ok(())
    }

    /// `par for x in it { body }`: one branch per element, and the value is
    /// the `Vec` of the bodies' values in iteration order (design.md
    /// § `par for`). Branches that do not conflict may run in any order, so
    /// running them one after another in iteration order is one of the
    /// allowed schedules. A limit is evaluated first, and `n <= 0` panics.
    #[allow(clippy::too_many_arguments)]
    fn par_for(
        &mut self,
        e: &'a Expr,
        label: &Option<String>,
        pattern: &'a Pattern,
        iterable: &'a Expr,
        body: &'a Block,
        par: &'a ParLoop,
        dest: Place,
    ) -> R<()> {
        let mut limit = None;
        if let Some(n) = &par.limit {
            let nt = self.expr_ty(n)?;
            let nv = self.expr_operand(n)?;
            let held = self.temp(nt);
            self.assign(held, Rvalue::Use(nv));
            let nv = Operand::Copy(Place::local(held));
            limit = Some(nv.clone());
            let bool_t = self.tys().bool();
            let c = self.temp(bool_t);
            let zero = Operand::Const(Const {
                ty: nt,
                kind: ConstKind::Scalar(0),
            });
            self.assign(c, Rvalue::BinaryOp(BinOp::Le, nv, zero));
            let bad = self.b.new_block();
            let ok = self.b.new_block();
            self.goto_with(
                TerminatorKind::SwitchInt {
                    discr: Operand::Copy(Place::local(c)),
                    targets: SwitchTargets::if_else(bad, ok),
                },
                bad,
            );
            self.diverge(TerminatorKind::Abort {
                reason: AbortReason::Panic,
            });
            self.cur = ok;
        }
        let vt = self.expr_ty(e)?;
        self.push_scope();
        let acc = self.temp(vt);
        let bb = self.cur;
        self.b.push(bb, StatementKind::StorageLive(acc));
        let name = format!("{}.new", self.tys().display(vt));
        self.call_native(&name, Vec::new(), Place::local(acc));
        self.declare(acc, vt);
        let region = self.open_par_region(ParKind::For { limit }, e.span);
        let saved = self
            .par_acc
            .replace((body as *const Block, acc, vt, region));
        let t = self.unit();
        let unit = self.temp(t);
        let r = self.for_range(e, label, pattern, iterable, body, Place::local(unit));
        self.par_acc = saved;
        r?;
        self.assign(dest, Rvalue::Use(Operand::Move(Place::local(acc))));
        self.pop_scope()
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
        // An `Iterator` implementation in source, the library's adaptors
        // included (`it.skip(2).enumerate()`), loops through its own `next`.
        if let Some(next) = self.iterator_next(iterable)? {
            return self.for_iterator(label, pattern, iterable, next, body, dest);
        }
        // `for (i, x) in c.iter().enumerate()`: the loop over `c`, with `i`
        // bound to the position.
        let (iterable, idx, pattern) = match (&iterable.kind, &pattern.kind) {
            (
                ExprKind::MethodCall {
                    object,
                    method,
                    args,
                    ..
                },
                PatternKind::Tuple(ps),
            ) if method == "enumerate" && args.is_empty() && ps.len() == 2 => {
                (&**object, Some(&ps[0]), &ps[1])
            }
            _ => (iterable, None, pattern),
        };
        if let Some(text) = self.chars_of(iterable)? {
            if idx.is_some() {
                return self.unsupported(e.span, "`enumerate` over chars");
            }
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
        if idx.is_some() && mode == Mode::Owned {
            return self.unsupported(e.span, "`enumerate` over a moved collection");
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
            Mode::Owned => self.temp_of(src, coll)?,
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
            idx,
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
            this.for_body(body)
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
            self.for_body(body)?;
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
        idx: Option<&'a Pattern>,
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
        if let Some(ip) = idx {
            if !matches!(ip.kind, PatternKind::Wildcard) {
                let it = self.node_ty(ip.id, ip.span)?;
                let ut = self.b.local_ty(i);
                let v = self.temp(it);
                let at = Operand::Copy(Place::local(i));
                let rv = if it == ut {
                    Rvalue::Use(at)
                } else {
                    Rvalue::Cast(CastKind::IntToInt, at, it)
                };
                self.assign(v, rv);
                self.bind_pattern(ip, &Place::local(v), it, false, &mut binds)?;
            }
        }
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
                let r = Place::local(place.local);
                let op = if mutable {
                    Operand::Move(r)
                } else {
                    Operand::Copy(r)
                };
                self.assign(x, Rvalue::Use(op));
                binds.push((x, elem_ref));
            }
            _ => self.bind_pattern(pattern, &place, elem, by_ref, &mut binds)?,
        }
        for &(l, t) in &binds {
            self.declare(l, t);
        }
        self.for_body(body)?;
        self.pop_scope()
    }

    // ── exits ───────────────────────────────────────────────────────

    /// The return place holds the value: run every scope's exit sequence,
    /// with the `errdefer` bodies when the value is an error, and return.
    fn return_exit(&mut self) -> R<()> {
        for clause in self.ensures {
            self.contract_check(&clause.body)?;
        }
        let has_errdefer = self
            .scopes
            .iter()
            .flatten()
            .any(|e| matches!(e, ScopeEntry::ErrDefer(..)));
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
            let p = self.expr_place(inner, false)?;
            if self.borrowed_place(&p) && (self.is_handle_aggregate(it) || self.is_handle(it)) {
                // Read out of a shared value or a borrow: a handle
                // aggregate is copied by counting (core semantics §6.1).
                let l = self.temp(it);
                self.count_copy(p, it, Place::local(l));
                self.schedule(ScopeEntry::Drop(Place::local(l)));
                Place::local(l)
            } else {
                p
            }
        } else {
            let l = self.temp_of(inner, it)?;
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
        let have = fields.first().copied();
        for (i, ft) in fields.into_iter().enumerate() {
            let fp = p
                .project(ProjElem::Downcast(VariantIdx(err)))
                .field(i as u32, ft);
            ops.push(self.use_place(fp, ft));
        }
        let Some(ret_err) = self.error_variant(ret_ty) else {
            return self.unsupported(e.span, "`?` in a function that returns no `Result`");
        };
        // An error of another type converts through the target's `From`.
        let want = self.tys().tcx().field_ty(ret_ty, Some(ret_err), 0);
        if let (Some(want), Some(have), 1) = (want, have, ops.len()) {
            if want != have {
                let Some(d) = self.user_from(want, have) else {
                    return self.unsupported(e.span, "`?` converting this error");
                };
                let name = self.lcx.instance(d, Vec::new());
                let func = self.fn_operand(&name, d, Vec::new());
                // Under D5 `from` borrows its source: pass a reference to
                // the payload and drop the payload after the call.
                let item = &self.lcx.fns[&d];
                let pt = self.callee_param_ty(&item.f.params[0], &[])?;
                let mut held = None;
                if matches!(self.tys().tcx().kind(pt), HK::Ref(i) if i == have) {
                    let h = self.temp(have);
                    self.assign(h, Rvalue::Use(ops.pop().expect("one payload")));
                    ops.push(self.ref_to(Place::local(h), have));
                    held = Some(h);
                }
                let t = self.temp(want);
                let next = self.b.new_block();
                self.goto_with(
                    TerminatorKind::Call {
                        func,
                        args: std::mem::take(&mut ops),
                        destination: Place::local(t),
                        target: Some(next),
                        unwind: UnwindAction::Abort,
                    },
                    next,
                );
                if let Some(h) = held.filter(|_| self.needs_drop(have)) {
                    let next = self.b.new_block();
                    self.goto_with(
                        TerminatorKind::Drop {
                            place: Place::local(h),
                            target: next,
                            unwind: UnwindAction::Abort,
                        },
                        next,
                    );
                }
                ops.push(Operand::Move(Place::local(t)));
            }
        }
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
            if let PatternKind::Or(alts) = &arm.pattern.kind {
                if arm.guard.is_none() && self.pattern_binds(&arm.pattern) {
                    self.or_arm(alts, &arm.body, &place, st, by_ref, join, dest.clone())?;
                    continue;
                }
            }
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

    /// A `match` arm `A(x) | B(x) => body`: each alternative is tested in
    /// turn and binds its own locals, which move into the first
    /// alternative's; the body reads those. Leaves `self.cur` at the next
    /// arm's test.
    #[allow(clippy::too_many_arguments)]
    fn or_arm(
        &mut self,
        alts: &'a [Pattern],
        body: &'a Expr,
        place: &Place,
        st: Ty,
        by_ref: bool,
        join: BasicBlock,
        dest: Place,
    ) -> R<()> {
        let body_bb = self.b.new_block();
        let next = self.b.new_block();
        self.push_scope();
        let mut canon: Vec<(String, Local, Ty)> = Vec::new();
        for (i, alt) in alts.iter().enumerate() {
            let fail = if i + 1 == alts.len() {
                next
            } else {
                self.b.new_block()
            };
            self.test_pattern(alt, place, st, fail)?;
            let mut binds = Vec::new();
            self.bind_pattern(alt, place, st, by_ref, &mut binds)?;
            for (l, t) in binds {
                let Some((name, node)) = self.b.user_name(l) else {
                    continue;
                };
                let c = match canon.iter().find(|(n, ..)| *n == name) {
                    Some(&(_, c, _)) => {
                        let op = self.use_place(Place::local(l), t);
                        self.assign(c, Rvalue::Use(op));
                        c
                    }
                    None => {
                        canon.push((name.clone(), l, t));
                        l
                    }
                };
                // The body's uses of `name` resolve to one alternative's
                // binding: every alternative's names the shared local.
                if let Some(&sym) = self.lcx.binding_syms.get(&(node, name)) {
                    self.locals.insert(sym, c);
                }
            }
            self.goto(body_bb);
            self.cur = fail;
        }
        self.cur = body_bb;
        for &(_, l, t) in &canon {
            self.declare(l, t);
        }
        self.expr_into(body, dest)?;
        self.pop_scope()?;
        self.goto(join);
        self.cur = next;
        Ok(())
    }

    /// The place a `match` or `if let` reads its scrutinee from (a temporary
    /// in the current scope unless it names a local's place), seen through
    /// any reference: the place, its type, and whether it binds by
    /// reference.
    fn scrutinee(&mut self, scrutinee: &'a Expr) -> R<(Place, Ty, bool)> {
        let st = self.expr_ty(scrutinee)?;
        // An element (`v[i]`) is matched where it lies, as a local is: a
        // binding then copies, counts or borrows it out of the collection.
        fn element(e: &Expr) -> bool {
            match &e.kind {
                ExprKind::Index { .. } => true,
                ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
                    element(object)
                }
                _ => false,
            }
        }
        let element = element(scrutinee);
        let place = if self.is_place(scrutinee) && (element || self.is_local_rooted(scrutinee)) {
            self.expr_place(scrutinee, false)?
        } else {
            let l = self.temp_of(scrutinee, st)?;
            Place::local(l)
        };
        // Matching through a reference binds by reference.
        Ok(match self.tys().tcx().kind(st) {
            HK::Ref(inner) | HK::MutRef(inner) => (place.project(ProjElem::Deref), inner, true),
            // A place reached through a reference or handle (`self.label`
            // in a `ref self` method, `v[i]`) is a `ref` scrutinee too
            // (core semantics §4.6): matched through a shared reborrow, so
            // its bindings are `ref`s even under a `mut ref`.
            _ if place.projection.iter().any(|p| {
                matches!(
                    p,
                    ProjElem::Deref | ProjElem::Index(_) | ProjElem::ConstIndex(_)
                )
            }) =>
            {
                let rt = self.tys().tcx().reference(st, false);
                let r = self.temp(rt);
                self.assign(r, Rvalue::Ref(BorrowKind::Shared, place));
                (Place::local(r).project(ProjElem::Deref), st, true)
            }
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

    /// The `next` method of `iterable`'s type, instantiated, when the type
    /// is a program or library type that implements it: what a `for` loop
    /// over an `Iterator` calls. `None` for every other type, and for a
    /// library collection, whose loop reads it by position.
    fn iterator_next(&mut self, iterable: &'a Expr) -> R<Option<NextFn>> {
        // A type the builder cannot spell (the checker's opaque
        // `Iterator[T]` of `v.iter()`) is not an `Iterator` implementation;
        // the caller's own paths take it, so its refusal is not recorded.
        let before = self.errors.len();
        let Ok(t) = self.expr_ty(iterable) else {
            self.errors.truncate(before);
            return Ok(None);
        };
        let tcx = self.tys().tcx();
        let HK::Adt { def, args } = tcx.kind(t) else {
            return Ok(None);
        };
        let args = tcx.list(args).to_vec();
        let Some(cands) = self.lcx.defs.methods.get(&def).and_then(|m| m.get("next")) else {
            return Ok(None);
        };
        let Some(d) = cands.iter().copied().find(|d| self.lcx.fns.contains_key(d)) else {
            return Ok(None);
        };
        let info = &self.lcx.fns[&d];
        let f = info.f;
        let Some(args) = info.impl_args(&args) else {
            return Ok(None);
        };
        if !f.params.is_empty()
            || f.self_param != Some(SelfParam::MutRef)
            || f.generic_params
                .as_ref()
                .is_some_and(|g| !g.params.is_empty())
        {
            return Ok(None);
        }
        let ret = match self.lcx.fn_return(d, &args) {
            Ok(r) => r,
            Err(err) => return self.unsupported(iterable.span, &err),
        };
        let name = self.lcx.instance(d, args.clone());
        Ok(Some((d, args, name, ret)))
    }

    /// `for pattern in it` over an `Iterator`: the loop owns `it` and calls
    /// its `next` until that returns `None`, binding each `Some` payload to
    /// `pattern`. `it` is dropped when the loop ends, however it ends.
    fn for_iterator(
        &mut self,
        label: &Option<String>,
        pattern: &'a Pattern,
        iterable: &'a Expr,
        (d, args, name, opt_t): NextFn,
        body: &'a Block,
        dest: Place,
    ) -> R<()> {
        let Some(some) = self
            .error_variant(opt_t)
            .filter(|_| {
                self.tys()
                    .tcx()
                    .adt_of(opt_t)
                    .is_some_and(|(a, _)| a.name == "Option")
            })
            .map(|none| 1 - none)
        else {
            return self.unsupported(iterable.span, "a `next` that returns no `Option`");
        };
        let Some(item_t) = self.tys().tcx().field_ty(opt_t, Some(some), 0) else {
            return self.unsupported(iterable.span, "a `next` with no payload");
        };
        let it_t = self.expr_ty(iterable)?;
        self.push_scope();
        let it = self.temp_of(iterable, it_t)?;
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
        let r = self.for_iterator_round(
            pattern,
            it,
            (d, args, name, opt_t),
            some,
            item_t,
            body,
            head,
            exit,
        );
        self.loops.pop();
        r?;
        self.cur = exit;
        self.pop_scope()?;
        self.assign(dest, Rvalue::Use(unit_const(self.unit())));
        Ok(())
    }

    /// One round of [`Self::for_iterator`], shaped like a `while let`
    /// round: `next`, then leave for `exit` on `None` or bind and run the
    /// body on `Some`.
    #[allow(clippy::too_many_arguments)]
    fn for_iterator_round(
        &mut self,
        pattern: &'a Pattern,
        it: Local,
        (d, args, name, opt_t): NextFn,
        some: u32,
        item_t: Ty,
        body: &'a Block,
        head: BasicBlock,
        exit: BasicBlock,
    ) -> R<()> {
        self.push_scope();
        let it_t = self.b.local_ty(it);
        let rt = self.tys().tcx().reference(it_t, true);
        let r = self.temp(rt);
        self.assign(r, Rvalue::Ref(BorrowKind::Mut, Place::local(it)));
        let opt = self.scoped_temp(opt_t);
        let func = self.fn_operand(&name, d, args);
        let next = self.b.new_block();
        self.goto_with(
            TerminatorKind::Call {
                func,
                args: vec![Operand::Move(Place::local(r))],
                destination: Place::local(opt),
                target: Some(next),
                unwind: UnwindAction::Abort,
            },
            next,
        );
        let fail = self.b.new_block();
        self.switch_variant(&Place::local(opt), some, fail);
        let matched = self.cur;
        self.cur = fail;
        let entries = std::mem::take(self.scopes.last_mut().unwrap());
        let r = self.exit_entries(&entries, false);
        *self.scopes.last_mut().unwrap() = entries;
        r?;
        self.goto(exit);
        self.cur = matched;
        self.push_scope();
        let payload = Place::local(opt)
            .project(ProjElem::Downcast(VariantIdx(some)))
            .field(0, item_t);
        let mut binds = Vec::new();
        self.bind_pattern(pattern, &payload, item_t, false, &mut binds)?;
        for &(l, t) in &binds {
            self.declare(l, t);
        }
        self.for_body(body)?;
        self.pop_scope()?;
        self.pop_scope()?;
        self.goto(head);
        Ok(())
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
        // A part that is a reference (`match (region, cat)` over two
        // `ref`s) is tested through it.
        if let HK::Ref(inner) | HK::MutRef(inner) = self.tys().tcx().kind(t) {
            if self.destructures(pat) {
                return self.test_pattern(pat, &place.project(ProjElem::Deref), inner, fail);
            }
        }
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
            PatternKind::Literal(LiteralPattern::String(text)) => {
                // `"USD" =>`: the library's `String.eq` against the constant.
                let r = match self.tys().tcx().kind(t) {
                    HK::Ref(_) | HK::MutRef(_) => Operand::Copy(place.clone()),
                    _ => self.ref_to(place.clone(), t),
                };
                let bool_t = self.tys().bool();
                let eq = self.temp(bool_t);
                self.call_native(
                    "String.eq",
                    vec![r, self.static_str(text)],
                    Place::local(eq),
                );
                let ok = self.b.new_block();
                self.goto_with(
                    TerminatorKind::SwitchInt {
                        discr: Operand::Move(Place::local(eq)),
                        targets: SwitchTargets::if_else(ok, fail),
                    },
                    ok,
                );
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
            // Each alternative in turn: the first that matches goes on.
            PatternKind::Or(alts) => {
                let ok = self.b.new_block();
                for (i, alt) in alts.iter().enumerate() {
                    let next = if i + 1 == alts.len() {
                        fail
                    } else {
                        self.b.new_block()
                    };
                    self.test_pattern(alt, place, t, next)?;
                    self.goto(ok);
                    self.cur = next;
                }
                self.cur = ok;
                Ok(())
            }
            PatternKind::AtBinding { pattern, .. } => self.test_pattern(pattern, place, t, fail),
            PatternKind::RangePattern {
                start,
                end,
                inclusive,
            } => {
                let lo = self.range_bound(start.as_ref(), t)?;
                let hi = self.range_bound(end.as_ref(), t)?;
                let (Some(lo), Some(hi)) = (lo, hi) else {
                    return self.unsupported(pat.span, "this range pattern bound");
                };
                let bool_t = self.tys().bool();
                let konst = |v: u128| {
                    Operand::Const(Const {
                        ty: t,
                        kind: ConstKind::Scalar(v),
                    })
                };
                let checks = [
                    lo.map(|v| (BinOp::Ge, v)),
                    hi.map(|v| (if *inclusive { BinOp::Le } else { BinOp::Lt }, v)),
                ];
                for (op, v) in checks.into_iter().flatten() {
                    let c = self.temp(bool_t);
                    self.assign(
                        c,
                        Rvalue::BinaryOp(op, Operand::Copy(place.clone()), konst(v)),
                    );
                    let ok = self.b.new_block();
                    self.goto_with(
                        TerminatorKind::SwitchInt {
                            discr: Operand::Copy(Place::local(c)),
                            targets: SwitchTargets::if_else(ok, fail),
                        },
                        ok,
                    );
                }
                Ok(())
            }
            PatternKind::Slice {
                prefix,
                rest,
                suffix,
            } => {
                let Some((elem, known)) = self.seq_of(t) else {
                    return self.unsupported(pat.span, "a slice pattern on this type");
                };
                let (k, j) = (prefix.len() as u64, suffix.len() as u64);
                // An array's length is in its type, which the checker held
                // the pattern to; a view's is tested.
                if known.is_none() {
                    let n = self.seq_len(place, t);
                    let usize_t = self.tys().tcx().intern(HK::UInt(UIntSize::Usize));
                    let bool_t = self.tys().bool();
                    let op = if rest.is_some() { BinOp::Ge } else { BinOp::Eq };
                    let c = self.temp(bool_t);
                    self.assign(
                        c,
                        Rvalue::BinaryOp(
                            op,
                            Operand::Copy(Place::local(n)),
                            Operand::Const(Const {
                                ty: usize_t,
                                kind: ConstKind::Scalar((k + j) as u128),
                            }),
                        ),
                    );
                    let ok = self.b.new_block();
                    self.goto_with(
                        TerminatorKind::SwitchInt {
                            discr: Operand::Copy(Place::local(c)),
                            targets: SwitchTargets::if_else(ok, fail),
                        },
                        ok,
                    );
                }
                for (i, p) in prefix.iter().enumerate() {
                    if self.destructures(p) {
                        let ep = self.seq_elem(place, t, SeqPos::Start(i as u64))?;
                        self.test_pattern(p, &ep, elem, fail)?;
                    }
                }
                for (i, p) in suffix.iter().enumerate() {
                    if self.destructures(p) {
                        let ep = self.seq_elem(place, t, SeqPos::End(j - i as u64))?;
                        self.test_pattern(p, &ep, elem, fail)?;
                    }
                }
                Ok(())
            }
        }
    }

    /// A range pattern's bound as the bits of a `t`: `Some(None)` when it
    /// is open, `None` when it is not a constant the builder can read. A
    /// named bound is a constant or an integer type's limit (design.md
    /// § Range patterns).
    fn range_bound(
        &mut self,
        b: Option<&'a crate::ast::RangeBound>,
        t: Ty,
    ) -> R<Option<Option<u128>>> {
        use crate::ast::RangeBound;
        let Some(b) = b else {
            return Ok(Some(None));
        };
        Ok(match b {
            RangeBound::Literal(LiteralPattern::Integer(v, _)) => Some(Some(*v as u128)),
            RangeBound::Literal(LiteralPattern::Char(c)) => Some(Some(*c as u128)),
            RangeBound::Literal(_) => None,
            RangeBound::Path { segments, .. } => match segments.as_slice() {
                [name] => {
                    let value = self
                        .lcx
                        .defs
                        .lookup(0, name)
                        .and_then(|d| self.lcx.consts.get(&d).copied());
                    match value {
                        Some(v) => match self.expr_operand(v)? {
                            Operand::Const(Const {
                                kind: ConstKind::Scalar(x),
                                ..
                            }) => Some(Some(x)),
                            _ => None,
                        },
                        None => None,
                    }
                }
                [_, _] => int_limit(self.tys().tcx().kind(t), &segments.join(".")).map(Some),
                _ => None,
            },
        })
    }

    /// The element type of a sequence a slice pattern matches, and its
    /// length when that is in the type (an array).
    fn seq_of(&self, t: Ty) -> Option<(Ty, Option<u64>)> {
        let tcx = self.tys().tcx();
        match tcx.kind(t) {
            HK::Array {
                elem,
                len: crate::ty::ArrayLen::Known(n),
            } => Some((elem, Some(n))),
            HK::Slice { elem, .. } => Some((elem, None)),
            HK::Intrinsic {
                kind: IntrinsicKind::Vec,
                args,
            } => Some((tcx.list(args)[0], None)),
            _ => None,
        }
    }

    /// The length of the sequence at `place`, as a `usize` local.
    fn seq_len(&mut self, place: &Place, t: Ty) -> Local {
        let usize_t = self.tys().tcx().intern(HK::UInt(UIntSize::Usize));
        let n = self.temp(usize_t);
        if matches!(self.tys().tcx().kind(t), HK::Array { .. }) {
            self.assign(n, Rvalue::Len(place.clone()));
        } else {
            let r = self.ref_to(place.clone(), t);
            let name = self.tys().display(t);
            self.call_native(&format!("{name}.len"), vec![r], Place::local(n));
        }
        n
    }

    /// The place of one element of the sequence at `place`: an array's
    /// element itself, or a view's through the library's `index`, which
    /// lends it (design.md § Slice and array patterns: a pattern over a
    /// `Vec` or `Slice` sees a view).
    fn seq_elem(&mut self, place: &Place, t: Ty, pos: SeqPos) -> R<Place> {
        let Some((elem, known)) = self.seq_of(t) else {
            unreachable!("a sequence type")
        };
        if let Some(n) = known {
            let i = match pos {
                SeqPos::Start(i) => i,
                SeqPos::End(m) => n - m,
            };
            return Ok(place.project(ProjElem::ConstIndex(i)));
        }
        let usize_t = self.tys().tcx().intern(HK::UInt(UIntSize::Usize));
        let idx = match pos {
            SeqPos::Start(i) => Operand::Const(Const {
                ty: usize_t,
                kind: ConstKind::Scalar(i as u128),
            }),
            SeqPos::End(m) => {
                let n = self.seq_len(place, t);
                let i = self.temp(usize_t);
                self.assign(
                    i,
                    Rvalue::BinaryOp(
                        BinOp::Sub,
                        Operand::Copy(Place::local(n)),
                        Operand::Const(Const {
                            ty: usize_t,
                            kind: ConstKind::Scalar(m as u128),
                        }),
                    ),
                );
                Operand::Copy(Place::local(i))
            }
        };
        let r = self.ref_to(place.clone(), t);
        let et = self.tys().tcx().reference(elem, false);
        let out = self.temp(et);
        let name = self.tys().display(t);
        self.call_native(&format!("{name}.index"), vec![r, idx], Place::local(out));
        Ok(Place::local(out).project(ProjElem::Deref))
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

    /// Does `pat` bind a name (a unit variant written bare does not)?
    /// Whether `pat` looks inside the value it matches, rather than naming
    /// all of it (a binding or `_`) or comparing a string with it.
    fn destructures(&self, pat: &Pattern) -> bool {
        match &pat.kind {
            PatternKind::Wildcard | PatternKind::Literal(LiteralPattern::String(_)) => false,
            PatternKind::Binding(_) => matches!(self.lcx.res.get(&pat.id), Some(Res::Def(_))),
            _ => true,
        }
    }

    fn pattern_binds(&self, pat: &Pattern) -> bool {
        !pat.binding_names().is_empty()
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
        // A part that is a reference binds through it, by reference.
        if let HK::Ref(inner) | HK::MutRef(inner) = self.tys().tcx().kind(t) {
            if self.destructures(pat) {
                let p = place.project(ProjElem::Deref);
                return self.bind_pattern(pat, &p, inner, true, out);
            }
        }
        // The parts of a `shared` value are only reached through a handle,
        // so a plain binding of one is a `ref` (core semantics §4.6).
        let by_ref = by_ref || matches!(self.tys().tcx().kind(t), HK::Shared { .. });
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
            PatternKind::RangePattern { .. } => Ok(()),
            // Which alternative matched is not known here, so only an
            // or-pattern that binds nothing is lowered.
            PatternKind::Or(_) if !self.pattern_binds(pat) => Ok(()),
            // `x @ p`: `x` names the whole, `p` binds its parts. Both can
            // hold their values only when the whole is copied or borrowed.
            PatternKind::AtBinding {
                name,
                pattern,
                by_ref: at_ref,
            } => {
                let by_ref = by_ref || *at_ref;
                // Under an owned scrutinee `name` takes the whole, so `p`
                // may bind only `Copy` parts (design.md § `@` bindings):
                // those are copied out first.
                self.bind_pattern(pattern, place, t, by_ref, out)?;
                self.bind_one(name, pat.id, place.clone(), t, by_ref, out);
                Ok(())
            }
            PatternKind::Slice {
                prefix,
                rest,
                suffix,
            } => {
                let Some((elem, known)) = self.seq_of(t) else {
                    return self.unsupported(pat.span, "a slice pattern on this type");
                };
                // Over a view every element binding borrows.
                let by_ref = by_ref || known.is_none();
                let (k, j) = (prefix.len() as u64, suffix.len() as u64);
                for (i, p) in prefix.iter().enumerate() {
                    if self.pattern_binds(p) {
                        let ep = self.seq_elem(place, t, SeqPos::Start(i as u64))?;
                        self.bind_pattern(p, &ep, elem, by_ref, out)?;
                    }
                }
                for (i, p) in suffix.iter().enumerate() {
                    if self.pattern_binds(p) {
                        let ep = self.seq_elem(place, t, SeqPos::End(j - i as u64))?;
                        self.bind_pattern(p, &ep, elem, by_ref, out)?;
                    }
                }
                if let Some(ast::RestPattern::Bound(name)) = rest {
                    // Over an array, `..rest` is the middle as an array; a
                    // middle of `Copy` elements is copied, which reads the
                    // same as the borrow the spec describes.
                    if let Some(n) = known {
                        if !self.is_copy(elem) {
                            return self
                                .unsupported(pat.span, "a named rest of non-`Copy` elements");
                        }
                        let ops = (k..n - j)
                            .map(|i| Operand::Copy(place.project(ProjElem::ConstIndex(i))))
                            .collect();
                        let at = self.tys().tcx().intern(HK::Array {
                            elem,
                            len: crate::ty::ArrayLen::Known(n - k - j),
                        });
                        let l = self.user_local(name, at, pat.id);
                        self.assign(l, Rvalue::Aggregate(AggregateKind::Array(elem), ops));
                        out.push((l, at));
                        return Ok(());
                    }
                    // `..rest` is a view of the middle, `[k, len - j)`.
                    let usize_t = self.tys().tcx().intern(HK::UInt(UIntSize::Usize));
                    let n = self.seq_len(place, t);
                    let hi = self.temp(usize_t);
                    self.assign(
                        hi,
                        Rvalue::BinaryOp(
                            BinOp::Sub,
                            Operand::Copy(Place::local(n)),
                            Operand::Const(Const {
                                ty: usize_t,
                                kind: ConstKind::Scalar(j as u128),
                            }),
                        ),
                    );
                    let st = self.tys().tcx().intern(HK::Slice {
                        elem,
                        mutable: false,
                    });
                    let r = self.ref_to(place.clone(), t);
                    let tname = self.tys().display(t);
                    let l = self.user_local(name, st, pat.id);
                    self.call_native(
                        &format!("{tname}.slice"),
                        vec![
                            r,
                            Operand::Const(Const {
                                ty: usize_t,
                                kind: ConstKind::Scalar(k as u128),
                            }),
                            Operand::Copy(Place::local(hi)),
                        ],
                        Place::local(l),
                    );
                    out.push((l, st));
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
        // A binding that covers a whole handle (or handle aggregate) copies
        // it even from a `ref` scrutinee (core semantics §4.6, §6.1).
        let handle = self.is_handle(t) || self.is_handle_aggregate(t);
        if mutable || (by_ref && !self.is_copy(t) && !handle) {
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
            if handle {
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
        if let Some(&l) = self.old_vals.get(&e.id) {
            let t = self.b.local_ty(l);
            self.clone_into(Place::local(l), t, dest);
            return Ok(());
        }
        let rc = self.lcx.calls.get(&e.id);
        // `T.m(..)` with `T` a type parameter of this body goes to the
        // instance's type, also where the program has a type or value named
        // `T` that the checker took the path for (`struct V` against `sum`'s
        // `V.default()`).
        if let ExprKind::Path { segments, .. } = &callee.kind {
            if let [tp, m] = segments.as_slice() {
                if rc.is_none_or(|rc| matches!(rc.callee, Callee::Builtin(_))) {
                    if let Some(t) = self.type_param(tp) {
                        return self.type_param_call(e, t, m, args, dest);
                    }
                }
            }
        }
        let Some(rc) = rc else {
            return self.unsupported(e.span, "a call with no resolved callee");
        };
        let callee_kind = rc.callee.clone();
        let substs = self.lcx.tys.tcx().list(rc.substs);
        // `LOG_PATH.clone()` parses as a path: a library method of the
        // constant's value, which is computed here.
        if let ExprKind::Path { segments, .. } = &callee.kind {
            if let [c, m] = segments.as_slice() {
                // A binding in a static is the receiver itself, borrowed.
                let st = self.lcx.defs.lookup(0, c);
                if let Some(d) = st.filter(|d| self.lcx.statics.contains_key(d)) {
                    let (_, vt, mutable) = self.lcx.statics[&d];
                    let p = self.static_place(d);
                    let rt = self.tys().tcx().reference(vt, mutable);
                    let r = self.temp(rt);
                    let kind = if mutable {
                        BorrowKind::Mut
                    } else {
                        BorrowKind::Shared
                    };
                    self.assign(r, Rvalue::Ref(kind, p));
                    let mut ops = vec![Operand::Move(Place::local(r))];
                    // A stored value moves in (`LOG.push(line)`), as into
                    // any collection; other non-Copy arguments are lent.
                    let stores = matches!(
                        m.as_str(),
                        "push"
                            | "push_back"
                            | "push_front"
                            | "insert"
                            | "extend"
                            | "append"
                            | "resize"
                            | "fill"
                            | "set"
                            | "send"
                            | "try_send"
                    );
                    for a in args {
                        let t = self.expr_ty(&a.value)?;
                        let by_ref = !stores && !self.is_copy(t);
                        ops.push(self.lib_arg(&a.value, by_ref)?);
                    }
                    let name = format!("{}.{m}", self.tys().display(vt));
                    self.call_native(&name, ops, dest);
                    return Ok(());
                }
                let value = self
                    .lcx
                    .defs
                    .lookup(0, c)
                    .and_then(|d| self.lcx.consts.get(&d).copied());
                if let Some(value) = value {
                    let vt = self.expr_ty(value)?;
                    let tmp = self.temp_of(value, vt)?;
                    let mut ops = vec![self.ref_to(Place::local(tmp), vt)];
                    for a in args {
                        let t = self.expr_ty(&a.value)?;
                        let by_ref = !self.is_copy(t);
                        ops.push(self.lib_arg(&a.value, by_ref)?);
                    }
                    let name = format!("{}.{m}", self.tys().display(vt));
                    self.call_native(&name, ops, dest);
                    return Ok(());
                }
            }
        }
        // `Stderr.println(x)` is `eprintln(x)`.
        let user_def = matches!(callee_kind, Callee::Def(d) if self.lcx.fns.contains_key(&d));
        if let Some(name) = stderr_print(callee).filter(|_| !user_def) {
            return self.builtin_call(e, name, args, dest);
        }
        match callee_kind {
            Callee::Builtin(name) => self.builtin_call(e, &name, args, dest),
            Callee::Value => self.call_value(e, callee, args, dest),
            Callee::Def(d) => {
                if let Some((_, idx)) = self.lcx.variant(d) {
                    let t = self.expr_ty(e)?;
                    let mut ops = Vec::new();
                    for (i, a) in args.iter().enumerate() {
                        // Each payload at its field's type (`Some((1, 2))`
                        // as an `Option[(i32, i32)]`).
                        let ft = self.tys().tcx().field_ty(t, Some(idx), i as u32);
                        ops.push(match ft {
                            Some(ft) => self.operand_at(&a.value, ft)?,
                            None => self.expr_operand(&a.value)?,
                        });
                    }
                    let kind = self.adt_aggregate(t, idx);
                    self.assign(dest, Rvalue::Aggregate(kind, ops));
                    return Ok(());
                }
                if let ("iter_read_value", None, [a]) =
                    (self.lcx.def_name(d).as_str(), self.lcx.def_owner(d), args)
                {
                    return self.read_value(e, &a.value, dest);
                }
                if let ("iter_clone_value", None, [a]) =
                    (self.lcx.def_name(d).as_str(), self.lcx.def_owner(d), args)
                {
                    let (p, t) = self.deref_place(&a.value, false)?;
                    self.clone_into(p, t, dest);
                    return Ok(());
                }
                // Labels need no lowering: the default-argument fill has
                // already put a labeled call's arguments in declaration
                // order, and the typechecker rejected any it could not.
                let inst_args = self.instance_args(e.span, &substs)?;
                let f = self.lcx.fns.get(&d).map(|i| i.f);
                let Some(f) = f else {
                    let name = self.lcx.def_name(d);
                    if self.scalar_min_max(e, &name, args, dest.clone())? {
                        return Ok(());
                    }
                    // `T.size_of()` / `T.align_of()`: a fact about this
                    // instance's `T`, answered from its layout.
                    let layout_op = match name.as_str() {
                        "size_of" => Some(NullOp::SizeOf),
                        "align_of" => Some(NullOp::AlignOf),
                        _ => None,
                    };
                    if let (Some(op), None, [t]) =
                        (layout_op, self.lcx.def_owner(d), inst_args.as_slice())
                    {
                        self.assign(dest, Rvalue::NullaryOp(op, *t));
                        return Ok(());
                    }
                    // A library function with no Kāra body (`sleep_ms(2)`,
                    // `Command.new("ls")`, `Arena[i64].new()`): the
                    // interpreter's, by its name qualified with the owning
                    // type and that type's arguments. Copy arguments go by
                    // value, the rest by reference.
                    let name = match (self.lcx.def_owner(d), inst_args.is_empty()) {
                        (Some(owner), true) => format!("{owner}.{name}"),
                        (Some(owner), false) => {
                            let tys = self.tys();
                            let shown: Vec<String> =
                                inst_args.iter().map(|&t| tys.display(t)).collect();
                            format!("{owner}[{}].{name}", shown.join(", "))
                        }
                        (None, true) => name,
                        (None, false) => {
                            return self.unsupported(e.span, "a call to this function");
                        }
                    };
                    let mut ops = Vec::new();
                    for a in args {
                        let t = self.expr_ty(&a.value)?;
                        let by_ref = !self.is_copy(t);
                        ops.push(self.lib_arg(&a.value, by_ref)?);
                    }
                    self.call_native(&name, ops, dest);
                    return Ok(());
                };
                let recv = self.variant_receiver(callee, d, f, &inst_args)?;
                self.def_call(e, d, f, inst_args, recv, args, dest)
            }
        }
    }

    /// The library's `iter_read_value(x)`: `x` read as the call's type. A
    /// `ref` to a value of that type is copied out (core semantics §5.10);
    /// any other value moves.
    fn read_value(&mut self, e: &'a Expr, x: &'a Expr, dest: Place) -> R<()> {
        let xt = self.expr_ty(x)?;
        let rt = self.expr_ty(e)?;
        let tcx = self.tys().tcx();
        if let HK::Ref(inner) | HK::MutRef(inner) = tcx.kind(xt) {
            if !matches!(tcx.kind(rt), HK::Ref(_) | HK::MutRef(_)) {
                let p = if self.is_place(x) && self.is_local_rooted(x) {
                    self.expr_place(x, false)?
                } else {
                    Place::local(self.temp_of(x, xt)?)
                };
                let op = self.use_place(p.project(ProjElem::Deref), inner);
                self.assign(dest, Rvalue::Use(op));
                return Ok(());
            }
        }
        let op = self.expr_operand(x)?;
        self.assign(dest, Rvalue::Use(op));
        Ok(())
    }

    /// `Ordering.Less.is_lt()` parses as a call of the path
    /// `Ordering.Less.is_lt`: the unit variant its first two segments name
    /// is the receiver of the method the last one names.
    fn variant_receiver(
        &mut self,
        callee: &'a Expr,
        d: DefId,
        f: &'a Function,
        inst_args: &[Ty],
    ) -> R<Option<Operand>> {
        let Some(mode) = f.self_param.clone() else {
            return Ok(None);
        };
        let ExprKind::Path { segments, .. } = &callee.kind else {
            return Ok(None);
        };
        let [ty, var, _] = segments.as_slice() else {
            return Ok(None);
        };
        let Some(def) = self.lcx.defs.lookup(0, ty) else {
            return Ok(None);
        };
        let n = self.lcx.fns[&d].impl_params;
        let targs = inst_args.get(..n).unwrap_or(&[]);
        let t = self.tys().tcx().adt(def, targs);
        let t = match self.lcx.mir_ty(t, &[]) {
            Ok(t) => t,
            Err(err) => return self.unsupported(callee.span, &err),
        };
        let Some((adt, _)) = self.tys().tcx().adt_of(t) else {
            return Ok(None);
        };
        let Some(idx) = adt
            .variants
            .iter()
            .position(|v| v.name == *var && v.fields.is_empty())
        else {
            return Ok(None);
        };
        let l = self.temp(t);
        let kind = self.adt_aggregate(t, idx as u32);
        self.assign(l, Rvalue::Aggregate(kind, Vec::new()));
        Ok(Some(match self.self_mode(f, mode, t) {
            SelfParam::Owned => Operand::Move(Place::local(l)),
            SelfParam::Ref => self.ref_to(Place::local(l), t),
            SelfParam::MutRef => {
                let rt = self.tys().tcx().reference(t, true);
                let r = self.temp(rt);
                self.assign(r, Rvalue::Ref(BorrowKind::Mut, Place::local(l)));
                Operand::Move(Place::local(r))
            }
        }))
    }

    /// The type a generic parameter named `name` has in this instance.
    fn type_param(&self, name: &str) -> Option<Ty> {
        let item = self.lcx.fns.get(&self.b.instance().def)?;
        let names = item
            .impl_generics
            .into_iter()
            .chain(item.f.generic_params.as_ref())
            .flat_map(|g| g.params.iter());
        let i = names.into_iter().position(|p| p.name == name)?;
        self.args.get(i).copied()
    }

    /// `T.make(args)` in a generic body: the impl's method for `T`'s type
    /// in this instance; for a primitive, its library function (`default`
    /// is zero).
    fn type_param_call(
        &mut self,
        e: &'a Expr,
        t: Ty,
        m: &str,
        args: &'a [CallArg],
        dest: Place,
    ) -> R<()> {
        if let Some((d, inst_args)) = self.method_by_receiver(t, m) {
            let f = self.lcx.fns[&d].f;
            return self.def_call(e, d, f, inst_args, None, args, dest);
        }
        if let Some((d, inst_args)) = self.generic_fn_by_receiver(t, m, args)? {
            let f = self.lcx.fns[&d].f;
            return self.def_call(e, d, f, inst_args, None, args, dest);
        }
        let tcx = self.tys().tcx();
        let zero = match tcx.kind(t) {
            HK::Int(_) | HK::UInt(_) | HK::Bool | HK::Char => Some(ConstKind::Scalar(0)),
            HK::Float(_) => Some(ConstKind::Float(0f64.to_bits())),
            _ => None,
        };
        if let (Some(kind), "default", []) = (zero, m, args) {
            self.assign(dest, Rvalue::Use(Operand::Const(Const { ty: t, kind })));
            return Ok(());
        }
        let mut ops = Vec::new();
        for a in args {
            let at = self.expr_ty(&a.value)?;
            let by_ref = !self.is_copy(at);
            ops.push(self.lib_arg(&a.value, by_ref)?);
        }
        let name = format!("{}.{m}", self.tys().display(t));
        self.call_native(&name, ops, dest);
        Ok(())
    }

    /// A call of the program's function `d` (`f`) in instance `inst_args`,
    /// with `recv` before the arguments when a path names the receiver.
    #[allow(clippy::too_many_arguments)]
    fn def_call(
        &mut self,
        e: &'a Expr,
        d: DefId,
        f: &'a Function,
        inst_args: Vec<Ty>,
        recv: Option<Operand>,
        args: &'a [CallArg],
        dest: Place,
    ) -> R<()> {
        if args.len() != f.params.len() {
            return self.unsupported(e.span, "a call that leaves out default arguments");
        }
        let mut ops = Vec::new();
        let mut fn_tys: Vec<Option<Ty>> = Vec::new();
        for (a, p) in args.iter().zip(&f.params) {
            if self.is_fn_typed(p.pattern.id) && !self.fn_param_owned(p) {
                let (op, t) = self.fn_arg(&a.value)?;
                ops.push(PendingRecv::Ready(op));
                fn_tys.push(Some(t));
                continue;
            }
            if let Some(op) = self.closure_for_param(&a.value, p, &inst_args)? {
                ops.push(PendingRecv::Ready(op));
                fn_tys.push(None);
                continue;
            }
            let pt = self.callee_param_ty(p, &inst_args)?;
            ops.push(self.arg_pending(&a.value, pt)?);
            fn_tys.push(None);
        }
        let mut ops = self.finish_args(ops)?;
        if let Some(r) = recv {
            ops.insert(0, r);
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

    /// A closure literal passed for a parameter whose type is a bare type
    /// parameter the checker solved to an erased `Fn(..)` (`f: own F` in
    /// `inspect`). The callee may keep it in what it returns, but that value
    /// is a local view (core semantics §9.2), so the closure does not escape
    /// (§9.3): it captures what it writes by `mut ref` (§9.1), and borrowck
    /// holds the loan for as long as the erased value lives.
    fn closure_for_param(
        &mut self,
        a: &'a Expr,
        p: &'a ast::Param,
        inst_args: &[Ty],
    ) -> R<Option<Operand>> {
        if !matches!(a.kind, ExprKind::Closure { .. }) {
            return Ok(None);
        }
        let Some(&pt) = self.lcx.node_types.get(&p.pattern.id) else {
            return Ok(None);
        };
        let HK::Param(param) = self.tys().tcx().kind(pt) else {
            return Ok(None);
        };
        let i = param.index as usize;
        let erased = inst_args
            .get(i)
            .is_some_and(|&t| matches!(self.tys().tcx().kind(t), HK::Fn { .. }));
        if !erased {
            return Ok(None);
        }
        self.closure_escapes = false;
        let (op, _) = self.closure_value(a)?;
        let l = self.temp(inst_args[i]);
        self.assign(l, Rvalue::Use(op));
        Ok(Some(Operand::Move(Place::local(l))))
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
            Ok(t) => Ok(self.param_mode_ty(p, t)),
            Err(e) => self.unsupported(p.span, &e),
        }
    }

    /// A parameter's type as its body takes it. Under D5 (behind
    /// `KARAC_D5=1` until the corpus is migrated) a bare parameter of a
    /// non-`Copy` type borrows; `own T`, the written borrow forms and
    /// function values keep their type.
    fn param_mode_ty(&self, p: &ast::Param, t: Ty) -> Ty {
        if !d5_params() || p.is_own || self.is_copy(t) {
            return t;
        }
        let tcx = self.tys().tcx();
        match tcx.kind(t) {
            HK::Ref(_)
            | HK::MutRef(_)
            | HK::Slice { .. }
            | HK::Fn { .. }
            | HK::FnDef { .. }
            | HK::Closure { .. } => t,
            _ => tcx.reference(t, false),
        }
    }

    /// The receiver mode a body takes: under D5 a bare `self` of a
    /// non-`Copy` type borrows (core semantics §8), and `own self` is owned.
    fn self_mode(&self, f: &ast::Function, mode: SelfParam, t: Ty) -> SelfParam {
        if mode == SelfParam::Owned && d5_params() && !f.self_is_own && !self.is_copy(t) {
            return SelfParam::Ref;
        }
        mode
    }

    /// Whether a function-typed parameter takes its function by value
    /// (`escaping Fn(..)`, core semantics §9.3): the callee may store it,
    /// so it is not specialised to the argument's closure.
    fn fn_param_owned(&self, p: &ast::Param) -> bool {
        self.lcx
            .escaping_fns
            .contains(&SpanKey::from_span(&p.ty.span))
    }

    /// An argument for a `mut ref` parameter whose borrow waits until every
    /// argument is evaluated (core semantics §5.6, two-phase borrows):
    /// `insert(nodes, nodes[root].left, v)` reads `nodes` before lending it.
    /// Any other argument is evaluated now.
    fn arg_pending(&mut self, a: &'a Expr, pt: Ty) -> R<PendingRecv<'a>> {
        if let HK::MutRef(inner) = self.tys().tcx().kind(pt) {
            let slice = matches!(self.tys().tcx().kind(inner), HK::Slice { .. });
            if !slice && self.is_place(a) {
                let at = self.expr_ty(a)?;
                match self.tys().tcx().kind(at) {
                    HK::MutRef(_) => {
                        let p = self.expr_place(a, true)?.project(ProjElem::Deref);
                        let t = self.place_type(&p);
                        return Ok(PendingRecv::Borrow(p, t, true));
                    }
                    HK::Ref(_) => {}
                    _ => return self.recv_place(a, true),
                }
            }
        }
        Ok(PendingRecv::Ready(self.arg_operand(a, pt)?))
    }

    /// The arguments, with each waiting `mut ref` borrow taken last. An
    /// argument that still names a place is read into a temporary first,
    /// so the borrow does not cover the read.
    fn finish_args(&mut self, pending: Vec<PendingRecv<'a>>) -> R<Vec<Operand>> {
        let mut ops = Vec::with_capacity(pending.len());
        let mut later = Vec::new();
        for (i, p) in pending.into_iter().enumerate() {
            match p {
                PendingRecv::Ready(op) => ops.push(op),
                p => {
                    ops.push(unit_const(self.unit()));
                    later.push((i, p));
                }
            }
        }
        if later.is_empty() {
            return Ok(ops);
        }
        for (i, op) in ops.iter_mut().enumerate() {
            if later.iter().any(|(j, _)| *j == i) {
                continue;
            }
            let (Operand::Copy(p) | Operand::Move(p)) = op else {
                continue;
            };
            if p.projection.is_empty() && self.b.is_temp(p.local) {
                continue;
            }
            let t = self.place_type(p);
            let l = self.temp(t);
            let read = std::mem::replace(op, Operand::Move(Place::local(l)));
            self.assign(l, Rvalue::Use(read));
        }
        for (i, p) in later {
            ops[i] = self.recv_borrow(p)?;
        }
        Ok(ops)
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
            let reborrow = mutable
                && self.is_place(a)
                && matches!(self.strip_ty(at), HK::Slice { mutable: true, .. });
            if reborrow || !matches!(self.strip_ty(at), HK::Slice { .. }) {
                // A `Vec` or array passed for a slice: view it as one. A
                // `mut Slice` place is re-viewed, a reborrow, so the caller
                // keeps it.
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
            None => {
                let op = self.owned_operand(a, pt)?;
                if self.erases(&op, pt) {
                    // A closure or function item passed to an `escaping`
                    // function parameter, as the parameter's type.
                    let l = self.temp(pt);
                    let o = self.erase_source(op);
                    self.assign(l, Rvalue::Cast(CastKind::Erase, o, pt));
                    return Ok(Operand::Move(Place::local(l)));
                }
                Ok(self.widen(op, pt))
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
        range: Option<(Option<&'a Expr>, Option<&'a Expr>, bool)>,
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
        let Some((start, end, inclusive)) = range else {
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
            // `a..=b` ends after `b`.
            Some(x) if inclusive => {
                let o = self.expr_operand(x)?;
                let o = self.cast_index(o, usize_t);
                let h = self.temp(usize_t);
                self.assign(
                    h,
                    Rvalue::BinaryOp(
                        BinOp::Add,
                        o,
                        Operand::Const(Const {
                            ty: usize_t,
                            kind: ConstKind::Scalar(1),
                        }),
                    ),
                );
                Operand::Copy(Place::local(h))
            }
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

    /// The prelude's `min(a, b)` and `max(a, b)` on integers and chars,
    /// in place until the library's Kāra bodies are lowered: `min` is `b`
    /// when `a > b`, else `a`; `max` is `b` when `a < b`, else `a` (the
    /// bodies in `ordering.kara`). `false` for anything else.
    fn scalar_min_max(
        &mut self,
        e: &'a Expr,
        name: &str,
        args: &'a [CallArg],
        dest: Place,
    ) -> R<bool> {
        let [a, b] = args else {
            return Ok(false);
        };
        let cmp = match name {
            "min" => BinOp::Gt,
            "max" => BinOp::Lt,
            _ => return Ok(false),
        };
        let t = self.expr_ty(e)?;
        if !matches!(
            self.tys().tcx().kind(t),
            HK::Int(_) | HK::UInt(_) | HK::Char
        ) {
            return Ok(false);
        }
        let bool_t = self.tys().bool();
        let x = self.temp(t);
        let (o, _) = self.scalar_operand(&a.value)?;
        self.assign(x, Rvalue::Use(retype_const(o, t)));
        let y = self.temp(t);
        let (o, _) = self.scalar_operand(&b.value)?;
        self.assign(y, Rvalue::Use(retype_const(o, t)));
        let c = self.temp(bool_t);
        self.assign(
            c,
            Rvalue::BinaryOp(
                cmp,
                Operand::Copy(Place::local(x)),
                Operand::Copy(Place::local(y)),
            ),
        );
        let take_b = self.b.new_block();
        let take_a = self.b.new_block();
        let join = self.b.new_block();
        self.goto_with(
            TerminatorKind::SwitchInt {
                discr: Operand::Copy(Place::local(c)),
                targets: SwitchTargets::if_else(take_b, take_a),
            },
            take_b,
        );
        self.assign(dest.clone(), Rvalue::Use(Operand::Copy(Place::local(y))));
        self.goto(join);
        self.cur = take_a;
        self.assign(dest, Rvalue::Use(Operand::Copy(Place::local(x))));
        self.goto(join);
        self.cur = join;
        Ok(true)
    }

    /// `a.cmp(b)` on integers, `char` or `bool`: two comparisons choosing
    /// the `Ordering` variant.
    fn scalar_cmp(
        &mut self,
        e: &'a Expr,
        object: &'a Expr,
        args: &'a [CallArg],
        dest: Place,
    ) -> R<bool> {
        let [b] = args else {
            return Ok(false);
        };
        let ot = self.expr_ty(object)?;
        let t = match self.tys().tcx().kind(ot) {
            HK::Ref(inner) | HK::MutRef(inner) => inner,
            _ => ot,
        };
        if !matches!(
            self.tys().tcx().kind(t),
            HK::Int(_) | HK::UInt(_) | HK::Char | HK::Bool
        ) {
            return Ok(false);
        }
        let ord = self.expr_ty(e)?;
        let Some((adt, _)) = self.tys().tcx().adt_of(ord) else {
            return Ok(false);
        };
        let variant = |name: &str| adt.variants.iter().position(|v| v.name == name);
        let (Some(less), Some(equal), Some(greater)) =
            (variant("Less"), variant("Equal"), variant("Greater"))
        else {
            return Ok(false);
        };
        let x = self.temp(t);
        let (o, _) = self.scalar_operand(object)?;
        self.assign(x, Rvalue::Use(o));
        let y = self.temp(t);
        let (o, _) = self.scalar_operand(&b.value)?;
        self.assign(y, Rvalue::Use(o));
        let bool_t = self.tys().bool();
        let join = self.b.new_block();
        let pick = |me: &mut Self, op: BinOp, variant: usize| {
            let c = me.temp(bool_t);
            let (xo, yo) = (
                Operand::Copy(Place::local(x)),
                Operand::Copy(Place::local(y)),
            );
            me.assign(c, Rvalue::BinaryOp(op, xo, yo));
            let yes = me.b.new_block();
            let no = me.b.new_block();
            me.goto_with(
                TerminatorKind::SwitchInt {
                    discr: Operand::Copy(Place::local(c)),
                    targets: SwitchTargets::if_else(yes, no),
                },
                yes,
            );
            let kind = me.adt_aggregate(ord, variant as u32);
            me.assign(dest.clone(), Rvalue::Aggregate(kind, Vec::new()));
            me.goto(join);
            me.cur = no;
        };
        pick(self, BinOp::Lt, less);
        pick(self, BinOp::Gt, greater);
        let kind = self.adt_aggregate(ord, equal as u32);
        self.assign(dest, Rvalue::Aggregate(kind, Vec::new()));
        self.goto(join);
        self.cur = join;
        Ok(true)
    }

    fn builtin_call(&mut self, e: &'a Expr, name: &str, args: &'a [CallArg], dest: Place) -> R<()> {
        match name {
            "println" | "print" | "eprintln" | "eprint" => {
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
            // `String.from(x)`: a literal is passed as the constant; a
            // `String` value is already the result.
            "String.from"
                if args.len() == 1
                    && (matches!(args[0].value.kind, ExprKind::StringLit(_))
                        || self.lcx.node_types.get(&args[0].value.id)
                            == Some(&self.place_type(&dest))) =>
            {
                match &args[0].value.kind {
                    ExprKind::StringLit(s) => {
                        let c = self.static_str(s);
                        self.call_native(name, vec![c], dest);
                        Ok(())
                    }
                    _ => self.expr_into(&args[0].value, dest),
                }
            }
            _ if name.contains('.') => {
                let (owner, m) = name.rsplit_once('.').unwrap();
                // `x.unwrap_or(Vec.new())`: a constructor whose type the
                // checker left to its context has its destination's.
                let t = if self.lcx.node_types.contains_key(&e.id)
                    || self.ty_hints.contains_key(&e.id)
                {
                    self.expr_ty(e)?
                } else {
                    self.place_type(&dest)
                };
                let mut ty_name = self.tys().display(t);
                // `Channel.new()` builds a `(Sender[T], Receiver[T])` pair:
                // named `Channel[T].new`.
                if owner == "Channel" {
                    if let HK::Tuple(parts) = self.tys().tcx().kind(t) {
                        let parts = self.tys().tcx().list(parts);
                        if let Some(&s) = parts.first() {
                            let shown = self.tys().display(s);
                            if let Some(args) = shown.strip_prefix("Sender") {
                                ty_name = format!("Channel{args}");
                            }
                        }
                    }
                }
                if ty_name.split('[').next() != Some(owner) {
                    // A library function that builds something else
                    // (`String.from_utf8` returns a `Result`): named by
                    // its owner alone.
                    if !matches!(owner, "String" | "char") {
                        return self.unsupported(e.span, &format!("the builtin `{name}`"));
                    }
                    ty_name = owner.to_string();
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
            ExprKind::StringLit(s) => {
                ops.push(self.static_str(s));
                Ok(())
            }
            ExprKind::InterpolatedStringLit(parts) => {
                for p in parts {
                    match p {
                        ParsedInterpolationPart::Text(s) => ops.push(self.static_str(s)),
                        ParsedInterpolationPart::Expr(x, None) => self.print_operands(x, ops)?,
                        ParsedInterpolationPart::Expr(x, Some(spec)) => {
                            // `{x:.3}`: the library's `format_spec` renders
                            // the value by the spec into a String the
                            // statement drops.
                            let mut inner = Vec::new();
                            self.print_operands(x, &mut inner)?;
                            inner.push(self.static_str(spec));
                            let st = self.tys().tcx().intern(HK::Str);
                            let s = self.scoped_temp(st);
                            self.call_native("format_spec", inner, Place::local(s));
                            ops.push(self.ref_to(Place::local(s), st));
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
                    self.queue_nested_displays(t);
                    ops.push(self.expr_operand(a)?);
                } else {
                    self.queue_nested_displays(t);
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
        if let Some(owner) = self.primitive_owner(object) {
            // `i64.parse(s)`, `f64.parse(s)`: a library function of a
            // primitive type, named after it. Copy arguments go by value,
            // the rest by reference.
            let mut ops = Vec::new();
            for a in args {
                let t = self.expr_ty(&a.value)?;
                let by_ref = !self.is_copy(t);
                ops.push(self.lib_arg(&a.value, by_ref)?);
            }
            self.call_native(&format!("{owner}.{method}"), ops, dest);
            return Ok(());
        }
        if method == "cmp" && self.scalar_cmp(e, object, args, dest.clone())? {
            return Ok(());
        }
        if let ("cmp", [other], false) = (method, args, self.lcx.calls.contains_key(&e.id)) {
            // A tuple's `cmp` (the checker records no callee for it).
            let ot = self.expr_ty(object)?;
            let (_, base) = self.strip_ty_full(ot);
            if matches!(self.tys().tcx().kind(base), HK::Tuple(_)) {
                let lp = self.expr_place(object, false)?;
                let rp = self.expr_place(&other.value, false)?;
                let (lp, lt) = self.strip_refs(lp);
                let (rp, _) = self.strip_refs(rp);
                return self.cmp_places(e.span, lp, rp, lt, dest);
            }
        }
        if method == "clone" && args.is_empty() && !self.lcx.calls.contains_key(&e.id) {
            // A tuple's `clone` (the checker records no callee for it).
            let (p, t) = self.deref_place(object, false)?;
            self.clone_into(p, t, dest);
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
                if let Some(t) = self.lcx.lowered_def(d) {
                    let inst_args = self.instance_args(e.span, &substs)?;
                    return self.user_method_call(e, object, t, inst_args, args, dest);
                }
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
        // A primitive's impl (`impl Step for i64`) has no target definition.
        let mut prim = self
            .lcx
            .fns
            .iter()
            .filter(|(_, i)| i.impl_prim == Some(base) && i.f.name == method);
        if let Some((&d, item)) = prim.next() {
            let generic = item
                .f
                .generic_params
                .as_ref()
                .is_some_and(|g| !g.params.is_empty());
            return (prim.next().is_none() && !generic).then(|| (d, Vec::new()));
        }
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
        Some((d, item.impl_args(&targs)?))
    }

    /// The associated function `m` of `t`'s impl when it has generic
    /// parameters of its own (`C.from_iter(it)` with `C` a `Vec`), each read
    /// off the argument its parameter is written as (`iter: own I`).
    fn generic_fn_by_receiver(
        &mut self,
        t: Ty,
        m: &str,
        args: &'a [CallArg],
    ) -> R<Option<(DefId, Vec<Ty>)>> {
        let (kind, base) = self.strip_ty_full(t);
        // A primitive's impl (`impl FromIterator[char] for String`) has no
        // target definition and no type arguments.
        let mut prim = self
            .lcx
            .fns
            .iter()
            .filter(|(_, i)| i.impl_prim == Some(base) && i.f.name == m);
        if let Some((&d, _)) = prim.next() {
            if prim.next().is_some() {
                return Ok(None);
            }
            return self.own_generics_from_args(d, Vec::new(), args);
        }
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
                let Some(d) = self.lcx.defs.lookup(0, name) else {
                    return Ok(None);
                };
                (d, self.tys().tcx().list(args))
            }
            _ => return Ok(None),
        };
        let mut found = self
            .lcx
            .fns
            .iter()
            .filter(|(_, i)| i.impl_target == Some(target) && i.f.name == m);
        let Some((&d, item)) = found.next() else {
            return Ok(None);
        };
        if found.next().is_some() {
            return Ok(None);
        }
        let Some(inst) = item.impl_args(&targs) else {
            return Ok(None);
        };
        self.own_generics_from_args(d, inst, args)
    }

    /// `d`'s instance arguments: `inst` (its impl's), then each of its own
    /// generic parameters read off the argument written as that bare name.
    fn own_generics_from_args(
        &mut self,
        d: DefId,
        mut inst: Vec<Ty>,
        args: &'a [CallArg],
    ) -> R<Option<(DefId, Vec<Ty>)>> {
        let f = self.lcx.fns[&d].f;
        let Some(own) = f.generic_params.as_ref() else {
            return Ok(None);
        };
        let mut from: Vec<&'a Expr> = Vec::new();
        for gp in &own.params {
            let at = f
                .params
                .iter()
                .zip(args)
                .find_map(|(p, a)| match &p.ty.kind {
                    ast::TypeKind::Path(q)
                        if q.generic_args.is_none() && q.segments == [gp.name.clone()] =>
                    {
                        Some(&a.value)
                    }
                    _ => None,
                });
            let Some(a) = at else {
                return Ok(None);
            };
            from.push(a);
        }
        for a in from {
            inst.push(self.expr_ty(a)?);
        }
        Ok(Some((d, inst)))
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
        // The argument list the typechecker completed: defaults filled in
        // and named arguments in declaration order (the legacy pipeline's
        // `lowering` splices the same list into the tree).
        let tc: &'a TypeCheckResult = self.lcx.tc;
        let args: &'a [CallArg] = match &e.kind {
            ExprKind::MethodCall { method, .. } => tc
                .method_default_fills
                .get(&(SpanKey::from_span(&e.span), method.clone()))
                .map_or(args, |v| v.as_slice()),
            _ => args,
        };
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
                let mode = match mode {
                    Some(m) => {
                        let st = self.expr_ty(object)?;
                        let (_, st) = self.strip_ty_full(st);
                        Some(self.self_mode(f, m, st))
                    }
                    None => None,
                };
                // No receiver: `G[i64].mk()` names an associated function
                // through its type, which is all the object is.
                let recv = match mode {
                    Some(SelfParam::Owned) => {
                        let st = self.expr_ty(object)?;
                        let (_, st) = self.strip_ty_full(st);
                        Some(PendingRecv::Ready(self.owned_operand(object, st)?))
                    }
                    Some(SelfParam::Ref) => Some(self.recv_place(object, false)?),
                    Some(SelfParam::MutRef) => Some(self.recv_place(object, true)?),
                    None => None,
                };
                let mut rest = Vec::with_capacity(args.len());
                let mut fn_tys: Vec<Option<Ty>> = Vec::with_capacity(args.len());
                for (a, p) in args.iter().zip(&f.params) {
                    // A function-typed parameter takes the closure or function
                    // item itself, and the instance is specialised to it, as
                    // for a free function's call.
                    if self.is_fn_typed(p.pattern.id) && !self.fn_param_owned(p) {
                        let (op, t) = self.fn_arg(&a.value)?;
                        rest.push(PendingRecv::Ready(op));
                        fn_tys.push(Some(t));
                        continue;
                    }
                    if let Some(op) = self.closure_for_param(&a.value, p, &inst_args)? {
                        rest.push(PendingRecv::Ready(op));
                        fn_tys.push(None);
                        continue;
                    }
                    let pt = self.callee_param_ty(p, &inst_args)?;
                    rest.push(self.arg_pending(&a.value, pt)?);
                    fn_tys.push(None);
                }
                let mut rest = self.finish_args(rest)?;
                let mut ops = match recv {
                    Some(recv) => vec![self.recv_borrow_after(recv, &mut rest)?],
                    None => Vec::new(),
                };
                ops.extend(rest);
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

    /// Give the untyped arguments of a library method (`v.push(None)`)
    /// the parameter type the receiver implies.
    /// The parameter types of a library collection method, where known.
    fn lib_param_tys(&self, recv: Ty, method: &str) -> Option<Vec<Ty>> {
        let HK::Intrinsic { kind, args: targs } = self.tys().tcx().kind(recv) else {
            return None;
        };
        let targs = self.tys().tcx().list(targs);
        let usize_t = self.tys().tcx().intern(HK::UInt(UIntSize::Usize));
        Some(match (kind, method) {
            (IntrinsicKind::Vec, "push" | "contains") => vec![targs[0]],
            (IntrinsicKind::VecDeque, "push_back" | "push_front") => vec![targs[0]],
            (IntrinsicKind::Vec, "insert") => vec![usize_t, targs[0]],
            (IntrinsicKind::Map | IntrinsicKind::SortedMap, "insert") => vec![targs[0], targs[1]],
            (IntrinsicKind::Map, "get" | "contains_key" | "remove") => vec![targs[0]],
            (IntrinsicKind::Set, "insert" | "contains" | "remove") => vec![targs[0]],
            _ => return None,
        })
    }

    fn hint_lib_args(&mut self, recv: Ty, method: &str, args: &[CallArg]) {
        let Some(params) = self.lib_param_tys(recv, method) else {
            return;
        };
        for (a, p) in args.iter().zip(params) {
            if !self.lcx.node_types.contains_key(&a.value.id) {
                self.ty_hints.insert(a.value.id, p);
            }
        }
    }

    /// The ambient resource a lowercase module receiver names (`env` is
    /// `Env`), unless a local shadows it.
    /// The primitive type `object` names when it is a type rather than a
    /// value: `i64` in `i64.parse(s)`.
    fn primitive_owner(&self, object: &Expr) -> Option<&'a str> {
        let ExprKind::Identifier(name) = &object.kind else {
            return None;
        };
        if matches!(self.lcx.res.get(&object.id), Some(Res::Local(_)))
            || self.lcx.node_types.contains_key(&object.id)
        {
            return None;
        }
        const PRIMS: [&str; 16] = [
            "i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64", "u128", "usize",
            "f32", "f64", "bool", "char",
        ];
        PRIMS.iter().copied().find(|p| *p == name)
    }

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
        self.recv_borrow(pending)
    }

    /// The first half of a receiver borrow: evaluate the receiver's place,
    /// leaving the borrow itself to `recv_borrow`, which the caller emits
    /// after the arguments (core semantics §5.6, two-phase borrows).
    fn recv_place(&mut self, object: &'a Expr, mutable: bool) -> R<PendingRecv<'a>> {
        let t = self.expr_ty(object)?;
        if !matches!(self.tys().tcx().kind(t), HK::Ref(_) | HK::MutRef(_)) && through_index(object)
        {
            self.pre_index(object)?;
            return Ok(PendingRecv::Deferred(object, mutable));
        }
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
        if let ExprKind::StringLit(lit) = &object.kind {
            // A literal receiver is a static `Str` with no origins (core
            // semantics §5.4): a view of it may outlive the statement, so
            // its `String` lives to the end of the enclosing block.
            let l = self.temp(t);
            let op = self.static_str(lit);
            self.call_native("String.from", vec![op], Place::local(l));
            let n = self.scopes.len();
            let at = n.saturating_sub(2);
            self.scopes[at].push(ScopeEntry::Drop(Place::local(l)));
            return Ok(PendingRecv::Borrow(Place::local(l), t, mutable));
        }
        let p = self.expr_place(object, mutable)?;
        Ok(PendingRecv::Borrow(p, t, mutable))
    }

    /// The receiver's borrow, taken after the arguments `rest` were
    /// evaluated (core semantics §5.6, two-phase borrows): an argument that
    /// still names a place (`doc.move_subtree(a, doc.root, 9)`) is read
    /// into a temporary first, so `&mut doc` does not cover the read.
    fn recv_borrow_after(&mut self, pending: PendingRecv<'a>, rest: &mut [Operand]) -> R<Operand> {
        if matches!(
            pending,
            PendingRecv::Borrow(_, _, true) | PendingRecv::Deferred(_, true)
        ) {
            for op in rest.iter_mut() {
                let (Operand::Copy(p) | Operand::Move(p)) = op else {
                    continue;
                };
                if p.projection.is_empty() && self.b.is_temp(p.local) {
                    continue;
                }
                let t = self.place_type(p);
                let l = self.temp(t);
                let read = std::mem::replace(op, Operand::Move(Place::local(l)));
                self.assign(l, Rvalue::Use(read));
            }
        }
        self.recv_borrow(pending)
    }

    fn recv_borrow(&mut self, pending: PendingRecv<'a>) -> R<Operand> {
        let (p, t, mutable) = match pending {
            PendingRecv::Ready(op) => return Ok(op),
            PendingRecv::Borrow(p, t, m) => (p, t, m),
            PendingRecv::Deferred(object, m) => {
                let p = self.expr_place(object, m)?;
                let t = self.place_type(&p);
                (p, t, m)
            }
        };
        let rt = self.tys().tcx().reference(t, mutable);
        let r = self.temp(rt);
        let kind = if mutable {
            BorrowKind::Mut
        } else {
            BorrowKind::Shared
        };
        self.assign(r, Rvalue::Ref(kind, p));
        Ok(Operand::Move(Place::local(r)))
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

    /// Record the capture an assignment's target is rooted at.
    fn note_assigned(&self, target: &Expr, caps: &[SymbolId]) {
        let mut e = target;
        while let ExprKind::FieldAccess { object, .. }
        | ExprKind::TupleIndex { object, .. }
        | ExprKind::Index { object, .. } = &e.kind
        {
            e = object;
        }
        if let Some(Res::Local(sym)) = self.lcx.res.get(&e.id) {
            if caps.contains(sym) {
                self.assigned_caps.borrow_mut().insert(*sym);
            }
        }
    }

    fn mv_block(&self, b: &'a Block, ctx: bool, caps: &[SymbolId], out: &mut FxHashSet<SymbolId>) {
        for st in &b.stmts {
            match &st.kind {
                StmtKind::Let { value, .. } => self.mv_expr(value, true, caps, out),
                StmtKind::Assign { target, value } => {
                    self.note_assigned(target, caps);
                    self.mv_expr(target, false, caps, out);
                    self.mv_expr(value, true, caps, out);
                }
                StmtKind::CompoundAssign { target, value, .. } => {
                    self.note_assigned(target, caps);
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
                let recv_owned = match def {
                    Some(d) => {
                        let f = self.lcx.fns[&d].f;
                        let st = self.lcx.node_types.get(&object.id).copied();
                        match (&f.self_param, st) {
                            (Some(m), Some(st)) => {
                                let (_, st) = self.strip_ty_full(st);
                                self.self_mode(f, m.clone(), st) == SelfParam::Owned
                            }
                            _ => false,
                        }
                    }
                    None => false,
                };
                // A method that writes its receiver mutates the capture
                // it is rooted at.
                let writes = match def {
                    Some(d) => matches!(self.lcx.fns[&d].f.self_param, Some(SelfParam::MutRef)),
                    None => {
                        crate::ast::is_mutating_collection_method(method)
                            || matches!(method.as_str(), "push_str" | "set" | "entry")
                    }
                };
                if writes {
                    self.note_assigned(object, caps);
                }
                go(object, recv_owned, out);
                let stores = matches!(
                    method.as_str(),
                    "push"
                        | "push_back"
                        | "push_front"
                        | "insert"
                        | "extend"
                        | "append"
                        | "set"
                        | "or_insert"
                        | "send"
                        | "try_send"
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
        let ref_params = std::mem::take(&mut self.closure_ref_params);
        let escapes = std::mem::take(&mut self.closure_escapes);
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
        // Whether the body writes a capture, which then needs the
        // environment lent mutably even when the capture was moved in.
        let mut writes = false;
        let captured = self.closure_captures(body);
        self.assigned_caps.borrow_mut().clear();
        let moved = self.moved_captures(body, &captured);
        let assigned = std::mem::take(&mut *self.assigned_caps.borrow_mut());
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
            writes |= mode == Some(OwnershipMode::MutRef);
            let cm = match mode {
                _ if escapes => CapMode::Value,
                Some(OwnershipMode::MutRef) => CapMode::Mut,
                // A capture the body assigns to is lent by `mut ref` (§9.1).
                _ if !explicit && assigned.contains(&sym) && !moved.contains(&sym) => CapMode::Mut,
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
        } else if writes || caps.iter().any(|&(_, m, _)| m == CapMode::Mut) {
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
            ref_params,
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
        // The arguments come first (`_2..`); a borrowed one binds its name
        // after all of them are declared.
        let mut through = Vec::new();
        for p in params {
            let PatternKind::Binding(name) = &p.pattern.kind else {
                return self.unsupported(p.span, "a destructuring closure parameter");
            };
            let t = self.node_ty(p.pattern.id, p.span)?;
            let sym = self
                .lcx
                .binding_syms
                .get(&(p.pattern.id, name.clone()))
                .copied();
            if job.ref_params && !matches!(self.tys().tcx().kind(t), HK::Ref(_) | HK::MutRef(_)) {
                let rt = self.tys().tcx().reference(t, false);
                let arg = self.b.arg("arg", rt);
                through.push((name, p.pattern.id, sym, arg, t));
                continue;
            }
            let l = self.b.arg(name, t);
            if let Some(sym) = sym {
                self.locals.insert(sym, l);
            }
            if self.needs_drop(t) {
                self.schedule(ScopeEntry::Drop(Place::local(l)));
            }
        }
        // A closure handed to a library call gets each element by
        // reference; its name binds through it as a match part does (a
        // `Copy` value is copied out).
        for (name, node, sym, arg, t) in through {
            let mut out = Vec::new();
            let elem = Place::local(arg).project(ProjElem::Deref);
            self.bind_one(name, node, elem, t, true, &mut out);
            for (l, lt) in out {
                if let Some(sym) = sym {
                    self.locals.insert(sym, l);
                }
                self.declare(l, lt);
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
            // An erased function value: called through a `ref` (`Fn`), a
            // `mut ref` (`MutFn`) or by value (`OnceFn`).
            HK::Fn {
                params,
                once,
                mutable,
                ..
            } => {
                let mut p = self.expr_place(callee, mutable && !once)?;
                if peeled != ct {
                    p = p.project(ProjElem::Deref);
                }
                let func = if once {
                    Operand::Move(p)
                } else {
                    let rt = self.tys().tcx().reference(peeled, mutable);
                    let r = self.temp(rt);
                    let kind = if mutable {
                        BorrowKind::Mut
                    } else {
                        BorrowKind::Shared
                    };
                    self.assign(r, Rvalue::Ref(kind, p));
                    Operand::Copy(Place::local(r))
                };
                let pts = self.tys().tcx().list(params).to_vec();
                (func, Vec::new(), pts)
            }
            _ => return self.unsupported(e.span, "a call through a function value"),
        };
        if param_tys.len() != args.len() {
            return self.unsupported(e.span, "a call through a value with the wrong arity");
        }
        let mut pending = Vec::with_capacity(args.len());
        for (a, pt) in args.iter().zip(param_tys) {
            pending.push(self.arg_pending(&a.value, pt)?);
        }
        ops.extend(self.finish_args(pending)?);
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
        if let ExprKind::StringLit(s) = &a.kind {
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
                let l = self.temp_of(object, base)?;
                Place::local(l)
            }
        } else {
            self.deref_place(object, false)?.0
        };
        // `unwrap_or`'s argument is evaluated before the test, and dropped
        // at the statement's end when the payload is taken instead.
        let default = match (method, args) {
            ("unwrap_or", [a]) => {
                let l = self.temp_of(&a.value, base_payload_or(self, base, ok))?;
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

    /// `left ?? right`: `left`'s `Some`/`Ok` payload, else `right`, which is
    /// evaluated only then (design.md § Optional chaining). `left` is taken
    /// by value, as `unwrap_or` takes its receiver.
    fn nil_coalesce(&mut self, left: &'a Expr, right: &'a Expr, dest: Place) -> R<()> {
        let lt = self.expr_ty(left)?;
        let unsupported = |s: &mut Self| s.unsupported(left.span, "`??` on this type");
        let Some((adt, _)) = self.tys().tcx().adt_of(lt) else {
            return unsupported(self);
        };
        let ok_name = match adt.name.as_str() {
            "Option" => "Some",
            "Result" => "Ok",
            _ => return unsupported(self),
        };
        let Some(ok) = adt.variants.iter().position(|v| v.name == ok_name) else {
            return unsupported(self);
        };
        let ok = ok as u32;
        let p = if self.is_place(left) && self.is_local_rooted(left) {
            self.expr_place(left, false)?
        } else {
            let l = self.temp_of(left, lt)?;
            Place::local(l)
        };
        let i64_t = self.tys().tcx().intern(HK::Int(IntSize::I64));
        let d = self.temp(i64_t);
        self.assign(d, Rvalue::Discriminant(p.clone()));
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
        match self.tys().tcx().field_ty(lt, Some(ok), 0) {
            Some(pt) => {
                let mut fp = p.project(ProjElem::Downcast(VariantIdx(ok))).field(0, pt);
                let mut pt = pt;
                // `Option[ref V] ?? v` with a `Copy` `V` reads the value
                // (design.md, `?` and `??`).
                let dt = self.place_type(&dest);
                if let HK::Ref(inner) | HK::MutRef(inner) = self.tys().tcx().kind(pt) {
                    if !matches!(self.tys().tcx().kind(dt), HK::Ref(_) | HK::MutRef(_)) {
                        fp = fp.project(ProjElem::Deref);
                        pt = inner;
                    }
                }
                let op = self.use_place(fp, pt);
                self.assign(dest.clone(), Rvalue::Use(op));
            }
            None => self.assign(dest.clone(), Rvalue::Use(unit_const(self.unit()))),
        }
        self.goto(join);
        self.cur = other;
        self.push_scope();
        self.expr_into(right, dest)?;
        self.pop_scope()?;
        self.goto(join);
        self.cur = join;
        Ok(())
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
        if let ("cmp", [other]) = (method, args) {
            if matches!(
                self.tys().tcx().kind(base),
                HK::Adt { .. } | HK::Shared { .. } | HK::Tuple(_)
            ) {
                // A derived `Ord`: field by field.
                let lp = self.expr_place(object, false)?;
                let rp = self.expr_place(&other.value, false)?;
                let (lp, lt) = self.strip_refs(lp);
                let (rp, _) = self.strip_refs(rp);
                return self.cmp_places(e.span, lp, rp, lt, dest);
            }
        }
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
                let mut ops = vec![self.recv_borrow_after(recv, &mut rest)?];
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
                } else if self.tys().display(base).starts_with("Entry[")
                    && matches!(
                        method,
                        "and_modify" | "or_insert" | "or_insert_with" | "or_default"
                    )
                {
                    // An `Entry` is consumed by each step of its chain
                    // (docs/library/collections.md).
                    PendingRecv::Ready(self.expr_operand(object)?)
                } else {
                    // `Atomic`, `Mutex` and the library cells write through
                    // a shared borrow (core semantics §6.3).
                    let interior = {
                        let shown = self.tys().display(base);
                        ["Atomic", "Mutex", "Arena", "OnceLock", "OnceCell"]
                            .iter()
                            .any(|c| shown.starts_with(c))
                    };
                    let mutates = !interior
                        && (crate::ast::is_mutating_collection_method(method)
                            || matches!(
                                method,
                                "push_str" | "set" | "sort_unstable" | "sort_unstable_by" | "entry"
                            ));
                    self.recv_place(object, mutates)?
                };
                if method == "to_string" {
                    self.queue_nested_displays(base);
                }
                self.queue_key_eq(base);
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
                        | "entry"
                        | "or_insert"
                        | "send"
                        | "try_send"
                );
                for a in args {
                    if self.is_fn_typed(a.value.id)
                        || matches!(a.value.kind, ExprKind::Closure { .. })
                    {
                        // A closure or function goes by value; the native
                        // calls it, lending each element by reference.
                        self.closure_ref_params = true;
                        let v = self.fn_value(&a.value);
                        self.closure_ref_params = false;
                        let (op, _) = v?;
                        rest.push(op);
                        continue;
                    }
                    let at = self.expr_ty(&a.value)?;
                    if a.mut_marker && self.is_place(&a.value) {
                        // `f.read(mut buf)`: a `mut Slice[T]` / `mut ref T`
                        // parameter lends the place mutably, even a `Copy`
                        // array, so the native writes the caller's elements.
                        let p = self.expr_place(&a.value, true)?;
                        let rt = self.tys().tcx().reference(at, true);
                        let r = self.temp(rt);
                        self.assign(r, Rvalue::Ref(BorrowKind::Mut, p));
                        rest.push(Operand::Move(Place::local(r)));
                        continue;
                    }
                    let callable = matches!(self.tys().tcx().kind(at), HK::Closure { .. });
                    let by_ref = !stores && !callable && !self.is_copy(at);
                    // A number's method reads a `ref` to the same number as
                    // its value: `n.min(m)` with `m: ref i64` (§5.10).
                    if matches!(
                        self.tys().tcx().kind(base),
                        HK::Int(_) | HK::UInt(_) | HK::Float(_)
                    ) && matches!(self.tys().tcx().kind(at), HK::Ref(t) | HK::MutRef(t) if t == base)
                    {
                        rest.push(self.owned_operand(&a.value, base)?);
                        continue;
                    }
                    // A borrowed value stored into a container of owned
                    // elements is a move out of the borrow, which the move
                    // check refuses (a container of references stores it).
                    if stores && self.is_place(&a.value) {
                        if let HK::Ref(inner) | HK::MutRef(inner) = self.tys().tcx().kind(at) {
                            let holds_refs = match self.tys().tcx().kind(base) {
                                HK::Intrinsic { args, .. } => {
                                    self.tys().tcx().list(args).into_iter().any(|t| {
                                        matches!(
                                            self.tys().tcx().kind(t),
                                            HK::Ref(_) | HK::MutRef(_)
                                        )
                                    })
                                }
                                _ => true,
                            };
                            if !holds_refs
                                && (self.is_handle(inner) || self.is_handle_aggregate(inner))
                            {
                                // A borrowed handle stored is a counted copy (§6.1).
                                let p = self.expr_place(&a.value, false)?;
                                let l = self.temp(inner);
                                self.count_copy(p.project(ProjElem::Deref), inner, Place::local(l));
                                rest.push(Operand::Move(Place::local(l)));
                                continue;
                            }
                            if !holds_refs && !self.is_copy(inner) {
                                let p = self.expr_place(&a.value, false)?;
                                rest.push(Operand::Move(p.project(ProjElem::Deref)));
                                continue;
                            }
                        }
                    }
                    // A tuple literal stored is built at the slot's types,
                    // reading any `ref` element (§5.10).
                    if let (true, ExprKind::Tuple(_)) = (stores, &a.value.kind) {
                        let slot = self
                            .lib_param_tys(base, method)
                            .and_then(|s| s.get(rest.len()).copied());
                        if let Some(slot) = slot {
                            rest.push(self.operand_at(&a.value, slot)?);
                            continue;
                        }
                    }
                    rest.push(self.lib_arg(&a.value, by_ref)?);
                }
                // A strong handle stored into a `weak` slot is downgraded,
                // and a narrower number widened.
                if let Some(slots) = self.lib_param_tys(base, method).filter(|_| stores) {
                    for (op, t) in rest.iter_mut().zip(slots) {
                        let o = std::mem::replace(op, unit_const(self.unit()));
                        *op = if self.downgrades(&o, t) {
                            self.downgrade(o, t)
                        } else {
                            self.widen(o, t)
                        };
                    }
                }
                let mut ops = vec![self.recv_borrow_after(recv, &mut rest)?];
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
enum PendingRecv<'a> {
    Ready(Operand),
    Borrow(Place, Ty, bool),
    /// A receiver reached through a subscript (`nodes[i].kids`): its
    /// subscripts are evaluated, and its place is formed after the
    /// arguments, so the container's borrow starts after them too.
    Deferred(&'a Expr, bool),
}

enum PendingPlace<'a> {
    Plain(&'a Expr),
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
/// The `old(e)` calls in a contract condition, outermost first.
fn old_calls<'a>(e: &'a Expr, out: &mut Vec<&'a Expr>) {
    let block = |b: &'a Block, out: &mut Vec<&'a Expr>| {
        for s in &b.stmts {
            if let StmtKind::Expr(x) | StmtKind::Let { value: x, .. } = &s.kind {
                old_calls(x, out);
            }
        }
        if let Some(x) = &b.final_expr {
            old_calls(x, out);
        }
    };
    match &e.kind {
        ExprKind::Call { callee, args } => {
            if matches!(&callee.kind, ExprKind::Identifier(n) if n == "old") {
                out.push(e);
                return;
            }
            old_calls(callee, out);
            for a in args {
                old_calls(&a.value, out);
            }
        }
        ExprKind::MethodCall { object, args, .. } => {
            old_calls(object, out);
            for a in args {
                old_calls(&a.value, out);
            }
        }
        ExprKind::Binary { left, right, .. } | ExprKind::NilCoalesce { left, right } => {
            old_calls(left, out);
            old_calls(right, out);
        }
        ExprKind::Index { object, index } => {
            old_calls(object, out);
            old_calls(index, out);
        }
        ExprKind::Unary { operand: x, .. }
        | ExprKind::FieldAccess { object: x, .. }
        | ExprKind::TupleIndex { object: x, .. } => old_calls(x, out),
        ExprKind::Block(b) => block(b, out),
        ExprKind::If {
            condition,
            then_block,
            else_branch,
        } => {
            old_calls(condition, out);
            block(then_block, out);
            if let Some(x) = else_branch {
                old_calls(x, out);
            }
        }
        _ => {}
    }
}

/// Whether the place `e` names is reached through a subscript.
fn through_index(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Index { index, .. } => !matches!(index.kind, ExprKind::Range { .. }),
        ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
            through_index(object)
        }
        _ => false,
    }
}

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
    let lowered = build_source(src)?;
    par_check(&lowered)?;
    Ok(interp::run_untraced(
        &lowered.program,
        &lowered.tys,
        "main",
        vec![],
    ))
}

/// Core §11.2: conflicting `par` branches are a compile error.
/// `KARAC_MIR_EFFECTS=0` skips the check.
fn par_check(lowered: &Lowered) -> Result<(), String> {
    let has_par = lowered
        .program
        .bodies
        .values()
        .any(|b| !b.par_regions.is_empty());
    if has_par && std::env::var("KARAC_MIR_EFFECTS").as_deref() != Ok("0") {
        let report = crate::mir::effects::analyze(&lowered.program, &lowered.tys);
        if let Some((name, c)) = report
            .par_conflicts
            .iter()
            .find_map(|(n, v)| v.first().map(|c| (n, c)))
        {
            let which = if c.branches.0 == c.branches.1 {
                "two iterations of the `par for`".to_string()
            } else {
                format!(
                    "branches {} and {} of the `par`",
                    c.branches.0 + 1,
                    c.branches.1 + 1
                )
            };
            return Err(format!(
                "effects: in `{name}` at line {}, {which} conflict: `{}` and `{}`",
                c.line, c.first.1, c.second.1
            ));
        }
    }
    Ok(())
}

/// [`run_source`], writing the program's output to the process's stdout
/// and stderr as it runs (`karac __mir-run`).
pub fn run_source_streaming(src: &str) -> Result<interp::RunResult, String> {
    let lowered = build_source(src)?;
    par_check(&lowered)?;
    Ok(interp::run_streaming(
        &lowered.program,
        &lowered.tys,
        "main",
        vec![],
    ))
}

/// Baked library modules appended to a program that names one of their
/// words.
const BAKED_SOURCES: &[(&str, &[&str])] = &[
    (
        include_str!("../../runtime/stdlib/protobuf.kara"),
        &["ProtoBuf", "ProtoReader", "Message", "proto_schema"],
    ),
    (
        include_str!("../../runtime/stdlib/process.kara"),
        &["Command", "Child", "ExitStatus"],
    ),
];

/// Whether the program declares a type the baked file declares. A program
/// with its own `Command` or `Message` means its own, and appending the
/// library's would define the name twice.
fn defines_a_baked_type(src: &str, baked: &str) -> bool {
    let declared = |text: &str| -> Vec<String> {
        let mut names = Vec::new();
        for line in text.lines() {
            let line = line
                .trim_start()
                .trim_start_matches("pub ")
                .trim_start_matches("shared ");
            for kw in ["struct ", "enum ", "trait ", "type "] {
                if let Some(rest) = line.strip_prefix(kw) {
                    let name: String = rest
                        .chars()
                        .take_while(|c| c.is_alphanumeric() || *c == '_')
                        .collect();
                    if !name.is_empty() {
                        names.push(name);
                    }
                }
            }
        }
        names
    };
    let ours = declared(src);
    declared(baked).iter().any(|n| ours.contains(n))
}

/// The appended library's `#[compiler_builtin]` functions are stdlib items,
/// which the resolver lets carry the attribute; their bodies are
/// placeholders, so they stay the interpreter's (`index_functions`).
fn mark_library_builtins(program: &mut ast::Program, user_len: usize) {
    let mark = |f: &mut ast::Function| {
        if f.span.offset >= user_len && f.attributes.iter().any(|a| a.is_bare("compiler_builtin")) {
            f.stdlib_origin = true;
        }
    };
    for item in &mut program.items {
        match item {
            Item::Function(f) => mark(f),
            Item::ImplBlock(b) => {
                for it in &mut b.items {
                    if let ImplItem::Method(f) = it {
                        mark(f);
                    }
                }
            }
            _ => {}
        }
    }
}

/// Check one source file, build its MIR and elaborate drops; or the first
/// stage that refused it.
pub fn build_source(src: &str) -> Result<Lowered, String> {
    // The library's Kāra methods go after the program, so the program's own
    // line numbers and spans are unchanged (redesign A2).
    let mut src = src.to_string();
    let user_len = src.len();
    // Baked modules whose bodies are Kāra, for a program that names them
    // (`#[derive(Message)]`'s `encode` / `decode` call `std.protobuf`), as
    // `PriorityQueue`'s is appended. Most programs do not, and they are long.
    let baked: Vec<&str> = BAKED_SOURCES
        .iter()
        .filter(|(text, words)| {
            words.iter().any(|w| src.contains(w)) && !defines_a_baked_type(&src, text)
        })
        .map(|(text, _)| *text)
        .collect();
    for (_, lib) in crate::prelude::LIBRARY_SOURCES {
        src.push('\n');
        src.push_str(lib);
    }
    for text in baked {
        // A `//!` header is a module doc comment, legal only at the top, and
        // the baked spelling `Unit` is `()` outside the stdlib.
        for line in text.lines() {
            src.push('\n');
            if !line.starts_with("//!") {
                src.push_str(&line.replace("[Unit,", "[(),"));
            }
        }
    }
    let parsed = crate::parse(&src);
    if !parsed.errors.is_empty() {
        return Err(format!("parse: {:?}", parsed.errors[0]));
    }
    let mut program = parsed.program;
    mark_library_builtins(&mut program, user_len);
    crate::desugar::with_collect_through_from_iterator(|| {
        crate::prepare_for_resolve(&mut program);
    });
    let r = crate::resolve(&program);
    if !r.errors.is_empty() {
        return Err(format!("resolve: {:?}", r.errors[0]));
    }
    let mut r = r;
    let mut tc = crate::typecheck_with_library_source(&program, &r);
    if !tc.errors.is_empty() {
        return Err(format!("typecheck: {}", tc.errors[0].message));
    }
    // A `#[derive(X)]` backed by a `comptime fn derive_x` (`Message`'s
    // `encode` / `decode`) splices its items into the program; they are
    // resolved and checked like the rest, as the CLI's pipeline does.
    if crate::comptime::has_derives_to_expand(&program) {
        let errs = crate::comptime::evaluate(&mut program, &tc);
        if let Some(e) = errs.first() {
            return Err(format!("comptime: {e:?}"));
        }
        crate::node_ids::assign_node_ids(&mut program);
        r = crate::resolve(&program);
        if !r.errors.is_empty() {
            return Err(format!("resolve: {:?}", r.errors[0]));
        }
        tc = crate::typecheck_with_library_source(&program, &r);
        if !tc.errors.is_empty() {
            return Err(format!("typecheck: {}", tc.errors[0].message));
        }
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
        let receivers = &lowered.receivers;
        let views = &lowered.views;
        let has_receiver = |i: &InstanceId| {
            if views.contains(&i.name) {
                crate::mir::ResultBorrows::ReceiverValue
            } else if !names.contains(i.name.as_str()) || receivers.contains(&i.name) {
                crate::mir::ResultBorrows::Receiver
            } else {
                crate::mir::ResultBorrows::Args
            }
        };
        if borrowck {
            crate::mir::check_borrows(body, &lowered.tys, &has_receiver)
                .map_err(|e| format!("borrow check {}: {}", body.instance.name, e.join("; ")))?;
        }
        crate::mir::elaborate_drops(body, &mut lowered.tys)
            .map_err(|e| format!("elaborate {}: {e}", body.instance.name))?;
        // §6.2's run-time borrow flags, on the elaborated body.
        crate::mir::insert_borrow_flags(body, &lowered.tys, &has_receiver);
        if dump.as_deref() == Some("elaborated") {
            eprintln!("{}", crate::mir::pretty::pretty_body(body, &lowered.tys));
        }
    }
    Ok(lowered)
}

/// `eprintln` or `eprint` when the callee is `Stderr.println` or
/// `Stderr.print`.
fn stderr_print(callee: &Expr) -> Option<&'static str> {
    let ExprKind::Path { segments, .. } = &callee.kind else {
        return None;
    };
    match segments.as_slice() {
        [r, m] if r == "Stderr" && m == "println" => Some("eprintln"),
        [r, m] if r == "Stderr" && m == "print" => Some("eprint"),
        _ => None,
    }
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

    /// `par for` gives the `Vec` of its bodies' values in iteration order;
    /// `?` in the body returns the earliest error, and a limit is checked
    /// before the loop runs (design.md § `par for`).
    #[test]
    fn par_for_collects_the_bodies_values() {
        let src = r#"
fn check(n: i64) -> Result[i64, String] {
    if n == 3 { Err(f"bad {n}") } else { Ok(n * 10) }
}
fn upto(k: i64) -> Result[Vec[i64], String] {
    let v = par(limit: 2) for i in 0..k { check(i)? };
    Ok(v)
}
fn main() {
    let sq = par for i in 0..5 { i * i };
    println(f"{sq.len()} {sq[0]} {sq[4]}");
    let mut names: Vec[String] = Vec.new();
    names.push("ab".to_string());
    names.push("cde".to_string());
    let tagged = par for n in names { f"<{n}>" };
    println(f"{tagged[0]}{tagged[1]} {names.len()}");
    println(f"{upto(3).unwrap().len()} {upto(5).unwrap_err()}");
}
"#;
        assert_eq!(
            run_source(src),
            Ok(("5 0 16\n<ab><cde> 2\n3 bad 3\n".to_string(), Some(0)))
        );
        let zero = "fn main() {\n    let v = par(limit: 0) for i in 0..2 { i };\n    println(f\"{v.len()}\");\n}\n";
        assert_eq!(run_source(zero).map(|r| r.1), Ok(Some(101)));
    }

    /// `par { a, b }` and `par for` record their branches' blocks as
    /// `par` regions, outer before inner, for the effect-conflict check.
    #[test]
    fn par_constructs_record_their_regions() {
        let src = r#"
fn f(n: i64) -> i64 { n * 2 }
fn main() {
    let (a, b) = par { f(1), f(2) };
    let n: i64 = 2;
    let v = par(limit: n) for i in 0..3 { let w = par { f(i), 1 }; w.0 };
    println(f"{a} {b} {v.len()}");
}
"#;
        let lowered = super::build_source(src).unwrap_or_else(|e| panic!("{e}"));
        let main = &lowered.program.bodies["main"];
        let rs = &main.par_regions;
        assert_eq!(rs.len(), 3, "{rs:?}");
        assert!(matches!(rs[0].kind, ParKind::Block));
        assert_eq!(rs[0].branches.len(), 2);
        assert!(matches!(rs[1].kind, ParKind::For { limit: Some(_) }));
        assert_eq!(rs[1].branches.len(), 1);
        // The inner `par` lies inside the `par for` body's blocks.
        assert!(matches!(rs[2].kind, ParKind::Block));
        let body = &rs[1].branches[0];
        assert!(rs[2].branches.iter().flatten().all(|b| body.contains(b)));
        for r in rs {
            assert!(r.branches.iter().all(|bs| !bs.is_empty()));
        }
        let empty = "fn main() {\n    par {}\n    println(\"ok\");\n}\n";
        assert_eq!(run_source(empty), Ok(("ok\n".to_string(), Some(0))));
        let printed = crate::mir::pretty::pretty_body(main, &lowered.tys);
        assert!(printed.contains("    par#0 block ["), "{printed}");
        assert!(printed.contains("    par#1 for ["), "{printed}");
        assert!(printed.contains("] limit copy _"), "{printed}");
        assert_eq!(run_source(src), Ok(("2 4 3\n".to_string(), Some(0))));
    }

    /// Kata-triage fixes: a user `Display` at every depth, `Secret` and
    /// snake_case derives, a user `PartialEq` in table keys and
    /// `Vec.contains`, `shared` keys by content, stderr, `main`'s `Err`,
    /// negative `Vec` lengths, a clamped `substring` end, and `f32`
    /// arithmetic rounded to `f32`.
    #[test]
    fn display_keys_stderr_and_narrow_floats() {
        let src = r#"
struct P { x: i64 }
impl Display for P {
    fn to_string(ref self) -> String { f"<{self.x}>" }
}
#[derive(Display(snake_case))]
enum Mode { FastPath, Slow }
struct K { id: i64, tag: i64 }
impl PartialEq for K { fn eq(ref self, other: ref K) -> bool { self.id == other.id } }
impl Eq for K {}
impl Hash for K { fn hash[H: Hasher](ref self, hasher: mut ref H) { hasher.write_i64(self.id) } }
#[derive(Hash, Eq, PartialEq)]
shared struct S { n: i64 }
fn main() -> Result[(), String] {
    let v = vec![P { x: 1 }, P { x: 2 }];
    let o: Option[P] = Some(P { x: 3 });
    println(f"{v} {o} {Mode.FastPath} {Mode.Slow}");
    let mut m: Map[K, i64] = Map.new();
    m.insert(K { id: 1, tag: 0 }, 10);
    m.insert(K { id: 1, tag: 5 }, 11);
    let ks = vec![K { id: 4, tag: 0 }];
    let mut s: Set[S] = Set.new();
    s.insert(S { n: 2 });
    println(f"{m.len()} {ks.contains(K { id: 4, tag: 9 })} {s.contains(S { n: 2 })}");
    println("hello".substring(2, 100));
    let a: f32 = 4000000000u32 as f32;
    println(f"{a + 1 as f32}");
    eprintln("to stderr");
    Stderr.println("also stderr");
    Err("bad".to_string())
}
"#;
        let r = super::run_source(src).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            r.output, "[<1>, <2>] Some(<3>) fast_path slow\n1 true true\nllo\n4000000000\n",
            "{:?}",
            r.outcome
        );
        assert_eq!(r.stderr, "to stderr\nalso stderr\nError: bad\n");
        assert_eq!(r.exit_code(), Some(1));

        let neg = "fn main() {\n    let n: i64 = 0 - 1;\n    let v: Vec[i64] = Vec.with_capacity(n);\n    println(f\"{v.len()}\");\n}\n";
        assert_eq!(run_source(neg).map(|r| r.1), Ok(Some(101)));
    }

    /// The library cells run as interpreter natives: `Atomic` writes its
    /// cell through a shared reference, `OnceLock` keeps its first value
    /// and hands a rejected one back, and `Arena` drops what a rewind cuts
    /// off (a leak would fail the run). A `bool` atomic's `fetch_xor`
    /// stores the xor (legacy leaves the value unchanged).
    #[test]
    fn library_cells_run_as_natives() {
        let src = r#"
fn main() {
    let a = Atomic.new(5);
    a.store(12, MemoryOrdering.SeqCst);
    let p = a.fetch_add(3, MemoryOrdering.SeqCst);
    let q = a.fetch_and(6, MemoryOrdering.SeqCst);
    let r = a.fetch_or(9, MemoryOrdering.SeqCst);
    let s = a.fetch_xor(1, MemoryOrdering.SeqCst);
    let t = a.swap(40, MemoryOrdering.SeqCst);
    println(f"{p} {q} {r} {s} {t} {a.load(MemoryOrdering.SeqCst)}");
    match a.compare_exchange(40, 41, MemoryOrdering.SeqCst, MemoryOrdering.SeqCst) {
        Ok(v) => println(f"ok {v}"),
        Err(v) => println(f"err {v}"),
    }
    match a.compare_exchange(40, 42, MemoryOrdering.SeqCst, MemoryOrdering.SeqCst) {
        Ok(v) => println(f"ok {v}"),
        Err(v) => println(f"err {v}"),
    }
    let b = Atomic.new(false);
    let b1 = b.swap(true, MemoryOrdering.SeqCst);
    let b2 = b.fetch_xor(true, MemoryOrdering.SeqCst);
    println(f"{b1} {b2} {b.load(MemoryOrdering.SeqCst)}");
    let c: OnceLock[String] = OnceLock.new();
    println(c.is_set());
    match c.set(String.from("a")) {
        Ok(_) => println("set"),
        Err(e) => println(f"rejected {e.rejected}"),
    }
    match c.set(String.from("b")) {
        Ok(_) => println("set"),
        Err(e) => println(f"rejected {e.rejected}"),
    }
    match c.get() {
        Some(s) => println(s),
        None => println("none"),
    }
    let d: OnceLock[String] = OnceLock.new();
    println(d.get_or_init(|| String.from("init")));
    println(d.get_or_init(|| String.from("again")));
    let ar: Arena[String] = Arena.new();
    let r1 = ar.push(String.from("x"));
    let cp = ar.high_water_mark();
    let r2 = ar.push(String.from("y"));
    println(f"{ar.get(r1)} {ar.get(r2)} {ar.len()}");
    ar.rewind_to(cp);
    println(ar.len());
}
"#;
        let r = super::run_source(src).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(r.outcome, interp::Outcome::Returned(interp::Value::Unit));
        assert_eq!(r.output, "12 15 6 15 14 40\nok 40\nerr 41\nfalse true false\nfalse\nset\nrejected b\na\ninit\ninit\nx y 2\n1\n");
    }

    /// `Map.entry` chains: `or_insert` into a vacant entry (in key order
    /// for a SortedMap), `and_modify` on an occupied one, an unused value
    /// or closure dropped, and an entry dropped unconsumed (a leak would
    /// fail the run). The output is legacy's.
    #[test]
    fn entry_chains_insert_modify_and_drop() {
        let src = r#"
fn main() {
    let mut m: SortedMap[String, String] = SortedMap.new();
    m.entry(String.from("m")).or_insert(String.from("1"));
    m.entry(String.from("c")).or_insert(String.from("2"));
    m.entry(String.from("x")).or_insert_with(|| String.from("3"));
    m.entry(String.from("c")).or_insert(String.from("unused"));
    let tag = String.from("!");
    m.entry(String.from("m")).and_modify(|v| { v.push_str("!"); }).or_insert(String.from("no"));
    m.entry(String.from("a")).and_modify(|v| { v.push_str("?"); }).or_insert(String.from("4"));
    m.entry(String.from("z"));
    for (k, v) in m.iter() { println(f"{k}={v}"); }
}
"#;
        let r = super::run_source(src).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(r.outcome, interp::Outcome::Returned(interp::Value::Unit));
        assert_eq!(r.output, "a=4\nc=2\nm=1!\nx=3\n");
    }

    /// Map ranges and bounds, set algebra, slice copies, binary search,
    /// String text methods, char classes and width-dependent integer
    /// methods, each as legacy prints them.
    #[test]
    fn collection_char_and_int_natives_match_legacy() {
        let src = r#"
fn main() {
    let mut m: Map[i64, i64] = Map.new();
    m.insert(1, 10);
    let es = m.entries();
    println(f"{es}");
    let mut s: SortedMap[i64, i64] = SortedMap.new();
    s.insert(1, 10); s.insert(5, 50); s.insert(9, 90);
    println(f"{s.floor(6)} {s.floor(0)} {s.ceiling(6)} {s.ceiling(10)} {s.min()} {s.max()}");
    let mut r: Vec[i64] = Vec.new();
    for (k, v) in s.range(2, 9) { r.push(k * 1000 + v); }
    println(f"{r}");
    println(f"{'a'.is_alphabetic()} {'1'.is_alphabetic()} {'A'.is_uppercase()} {'7'.is_ascii_digit()} {'Q'.to_lowercase()} {'q'.to_ascii_uppercase()} {'7'.to_digit(10)}");
    let v = vec![3, 1, 2];
    let sl = v.as_slice();
    let w = sl.to_vec();
    println(f"{w} {v.binary_search(1)} {v.binary_search(3)}");
    println(f"{"cab".sorted()} {"  x ".trim_start()} {"ab-c".strip_prefix("ab")} {"a b  c".split_whitespace()} {"aaa".replacen("a", "b", 2)} {"x\ny".lines()} {"héllo".char_count()}");
    let mut a: Set[i64] = Set.new(); a.insert(1); a.insert(2); a.insert(3);
    let mut b: Set[i64] = Set.new(); b.insert(2); b.insert(3); b.insert(4);
    let mut u: Vec[i64] = Vec.new();
    for x in a.union(b) { u.push(x); }
    u.sort();
    let mut d: Vec[i64] = Vec.new();
    for x in a.difference(b) { d.push(x); }
    let mut sa: SortedSet[i64] = SortedSet.new(); sa.insert(1); sa.insert(2); sa.insert(3);
    let mut sb: SortedSet[i64] = SortedSet.new(); sb.insert(2); sb.insert(3); sb.insert(4);
    let mut i: Vec[i64] = Vec.new();
    for x in sa.intersection(sb) { i.push(x); }
    println(f"{u} {d} {i}");
    let t: i32 = 40;
    println(f"{t.trailing_zeros()} {t.leading_zeros()} {(17).clamp(0, 10)}");
}
"#;
        let r = super::run_source(src).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(r.outcome, interp::Outcome::Returned(interp::Value::Unit));
        assert_eq!(r.output, "[(1, 10)]\nSome((5, 50)) None Some((9, 90)) None Some((1, 10)) Some((9, 90))\n[5050, 9090]\ntrue false true true q Q Some(7)\n[3, 1, 2] Some(1) None\nabc x  Some(-c) [a, b, c] bba [x, y] 5\n[1, 2, 3, 4] [1] [2, 3]\n3 26 10\n");
    }

    /// `Vec.from_slice` of a Vec passed by value clones its elements and
    /// drops the Vec (a leak would fail the run).
    #[test]
    fn vec_from_slice_consumes_an_owned_vec() {
        let src = r#"
fn main() {
    let mut src: Vec[String] = Vec.new();
    src.push(String.from("alpha"));
    src.push(String.from("beta"));
    let dst: Vec[String] = Vec.from_slice(src);
    println(f"{dst[0]} {dst[1]}");
}
"#;
        let r = super::run_source(src).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(r.outcome, interp::Outcome::Returned(interp::Value::Unit));
        assert_eq!(r.output, "alpha beta\n");
    }

    /// Channel ends name a queue the interpreter keeps; what is still
    /// queued drops with the last end. A user `struct Entry` keeps its
    /// derived `clone` rather than reaching the library entry's natives.
    #[test]
    fn channels_deque_get_and_user_entry_run_as_natives() {
        let src = r#"
#[derive(Clone)]
struct Entry { key: String, n: i64 }
fn main() {
    let (tx, rx): (Sender[String], Receiver[String]) = Channel.new();
    tx.send("a");
    let tx2 = tx.clone();
    tx2.send(String.from("b"));
    tx.send(String.from("left behind"));
    println(rx.recv());
    match rx.try_recv() {
        Some(s) => println(s),
        None => println("empty"),
    }
    let (btx, brx) = Channel.bounded(1);
    match btx.try_send(String.from("one")) {
        Ok(_) => println("sent"),
        Err(SendError.Full(v)) => println(f"full {v}"),
        Err(SendError.Closed(v)) => println(f"closed {v}"),
    }
    match btx.try_send(String.from("two")) {
        Ok(_) => println("sent"),
        Err(SendError.Full(v)) => println(f"full {v}"),
        Err(SendError.Closed(v)) => println(f"closed {v}"),
    }
    println(brx.recv());
    let mut q: VecDeque[i64] = VecDeque.new();
    q.push_back(7);
    q.push_back(9);
    match q.get(1) {
        Some(v) => println(v),
        None => println("none"),
    }
    match q.get(2) {
        Some(v) => println(v),
        None => println("none"),
    }
    let e = Entry { key: String.from("k"), n: 3 };
    let e2 = e.clone();
    println(f"{e2.key} {e2.n}");
}
"#;
        let r = super::run_source(src).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(r.outcome, interp::Outcome::Returned(interp::Value::Unit));
        assert_eq!(r.output, "a\nb\nsent\nfull two\none\n9\nnone\nk 3\n");
    }

    /// Statics initialize before `main`, every use sees earlier writes,
    /// and a static's heap lives to exit without counting as a leak.
    #[test]
    fn statics_hold_writes_across_calls() {
        let src = r#"
let mut COUNT: i64 = 0;
let SEEN: Atomic[i64] = Atomic.new(5);
let mut LOG: Vec[i64] = Vec.new();

fn bump() {
    COUNT = COUNT + 1;
    LOG.push(COUNT * 10);
}

fn main() {
    bump();
    bump();
    SEEN.fetch_add(2, MemoryOrdering.SeqCst);
    println(f"{COUNT} {SEEN.load(MemoryOrdering.SeqCst)} {LOG.len()} {LOG[1]}");
}
"#;
        let r = super::run_source(src).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(r.outcome, interp::Outcome::Returned(interp::Value::Unit));
        assert_eq!(r.output, "2 7 2 20\n");
    }

    /// The protobuf codecs' bit reinterpretation, as legacy prints it.
    #[test]
    fn float_bits_round_trip() {
        let src = r#"
fn main() {
    let x: f64 = 1.5;
    let y: f32 = 1.5;
    let n: f64 = -2.0;
    let b = x.to_bits();
    let c = y.to_bits32();
    let d = y.to_bits();
    println(f"{b} {c} {d} {n.to_bits()}");
    println(f"{b.bits_as_f64()} {c.bits_as_f32()} {n.to_bits().bits_as_f64()}");
}
"#;
        let r = super::run_source(src).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(r.outcome, interp::Outcome::Returned(interp::Value::Unit));
        assert_eq!(
            r.output,
            "4609434218613702656 1069547520 4609434218613702656 13835058055282163712\n\
             1.5 1.5 -2\n"
        );
    }

    /// `File` reads into the caller's `mut` buffer, writes, seeks and
    /// reports a missing file as `IoError.NotFound`, as legacy does.
    #[test]
    fn file_natives_read_write_and_seek() {
        let path = std::env::temp_dir().join(format!("karac_mir_file_{}", std::process::id()));
        let src = r#"
fn main() with reads(FileSystem) writes(FileSystem) {
    let path = "PATH";
    match File.create(path) {
        Ok(f) => {
            let mut bytes: Vec[u8] = Vec.new();
            bytes.push(104u8);
            bytes.push(105u8);
            bytes.push(10u8);
            match f.write(bytes.as_slice()) {
                Ok(n) => println(f"wrote {n}"),
                Err(_) => println("write err"),
            }
        }
        Err(_) => println("create err"),
    }
    match File.open(path) {
        Ok(f) => {
            let mut buf: Array[u8, 8] = [0u8; 8];
            match f.read(mut buf) {
                Ok(n) => println(f"read {n} first {buf[0]} last {buf[2]}"),
                Err(_) => println("read err"),
            }
            match f.seek(SeekFrom.Start, 1) {
                Ok(p) => println(f"at {p}"),
                Err(_) => println("seek err"),
            }
        }
        Err(_) => println("open err"),
    }
    match File.open("/nonexistent/karac-mir") {
        Ok(_) => println("opened"),
        Err(IoError.NotFound) => println("not found"),
        Err(_) => println("other"),
    }
    match FileSystem.read_lines(path) {
        Ok(ls) => println(ls.len()),
        Err(_) => println("read_lines err"),
    }
}
"#
        .replace("PATH", &path.display().to_string());
        let r = super::run_source(&src).unwrap_or_else(|e| panic!("{e}"));
        let _ = std::fs::remove_file(&path);
        assert_eq!(r.outcome, interp::Outcome::Returned(interp::Value::Unit));
        assert_eq!(
            r.output,
            "wrote 3\nread 3 first 104 last 10\nat 1\nnot found\n1\n"
        );
    }

    /// Float math natives compute at the receiver's width, and float
    /// literals and results round to their type.
    #[test]
    fn i128_arithmetic_keeps_128_bits_and_narrow_floats_round() {
        let src = r#"
fn half(x: f16) -> f16 { x / 2.0 }
fn main() {
    let big: i128 = 9223372036854775807 as i128 * 4;
    println(f"{big:+}");
    let m: i128 = i128.MAX;
    let w = m.wrapping_add(1);
    println(f"{w}");
    let a: f16 = 65500.0;
    let b: bf16 = 1.1;
    println(f"{a} {half(a) * 2.0 + half(a) * 2.0} {b}");
}
"#;
        assert_eq!(
            run_source(src),
            Ok((
                "+36893488147419103228\n\
                 -170141183460469231731687303715884105728\n\
                 65504 inf 1.1015625\n"
                    .to_string(),
                Some(0)
            ))
        );
    }

    #[test]
    fn float_math_at_each_width() {
        let src = r#"
fn main() {
    let b: bf16 = 1.1bf16;
    println(f"{b.exp()} {b.sqrt()}");
    let f: f32 = 2.0;
    println(f"{f.ln()} {f.pow(0.5 as f32)} {f.cbrt()}");
    let d: f64 = 2.0;
    println(f"{d.ln()} {d.pow(0.5)} {d.atan2(1.0)}");
    let c: f32 = 16777217.0;
    let w: f64 = 0.1f32;
    println(f"{c} {w}");
}
"#;
        assert_eq!(
            run_source(src),
            Ok((
                "3.015625 1.046875\n\
                 0.6931471824645996 1.4142135381698608 1.2599210739135742\n\
                 0.6931471805599453 1.4142135623730951 1.1071487177940904\n\
                 16777216 0.10000000149011612\n"
                    .to_string(),
                Some(0)
            ))
        );
    }

    /// `char.try_from`, `String.cmp` (here through `sort_by`) and
    /// `is_power_of_two` from source.
    #[test]
    fn try_from_cmp_and_power_of_two() {
        let src = r#"
fn main() {
    let a = char.try_from(97);
    let b = char.try_from(55296);
    println(f"{a.unwrap()} {b.unwrap_err()}");
    let mut words: Vec[String] = Vec.new();
    words.push("pear".to_string());
    words.push("apple".to_string());
    words.push("fig".to_string());
    words.sort_by(|x, y| x.cmp(y));
    println(f"{words[0]} {words[1]} {words[2]}");
    let n: u64 = 64;
    let m: u64 = 96;
    println(f"{n.is_power_of_two()} {m.is_power_of_two()}");
}
"#;
        assert_eq!(
            run_source(src),
            Ok(("a 55296\napple fig pear\ntrue false\n".to_string(), Some(0)))
        );
    }

    /// `u8 as char`, `char as` an integer and `bool as` an integer.
    #[test]
    fn char_and_bool_casts() {
        let src = r#"
fn main() {
    let k: i64 = 30;
    let c = ((k * 7919) % 26 + 97) as u8 as char;
    let bs: Vec[u8] = Vec.new();
    let d = 'z' as u32;
    let e = 'A' as i64 + 1;
    let t = true as i64;
    println(f"{c} {d} {e} {t} {b'(' as char} {bs.len()}");
}
"#;
        assert_eq!(
            run_source(src),
            Ok(("i 122 66 1 ( 0\n".to_string(), Some(0)))
        );
    }

    /// What the real-world apps (`corpus/apps`) lean on: `main` returning a
    /// `Result`, module `let`s and constants with methods, `??`, `let`-else,
    /// string literal patterns, or-arms that bind, matching a field through
    /// `ref self`, `Option` of a `Copy` type read out of a `Vec`, a
    /// two-phase receiver borrow and `enumerate`.
    #[test]
    fn apps_slice_constructs() {
        let src = r#"
let LIMIT: i64 = 3;
const TAG: String = "t";
enum Shape { Sq(i64), Rect(i64, i64), Dot }
struct Node { parent: Option[i64], label: Option[String] }
struct Doc { nodes: Vec[Node], root: i64 }
impl Doc {
    fn depth(ref self, id: i64) -> i64 {
        let mut d = 0;
        let mut cur = self.nodes[id].parent;
        while let Some(c) = cur {
            d += 1;
            cur = self.nodes[c].parent;
        }
        d
    }
    fn label(ref self, id: i64) -> String {
        match self.nodes[id].label {
            Some(l) => l.clone(),
            None => "-".to_string(),
        }
    }
    fn shift(mut ref self, by: i64, at: i64) -> i64 { self.root = by + at; self.root }
}
fn side(s: Shape) -> i64 {
    match s {
        Shape.Sq(a) | Shape.Rect(a, _) => a,
        Shape.Dot => 0,
    }
}
fn code(s: ref String) -> i64 {
    match s { "a" => 1, "b" => 2, _ => 0 }
}
fn first(v: ref Vec[i64]) -> i64 {
    let Some(x) = v.first() else { return -1 };
    *x
}
fn main() -> Result[(), String] {
    let none: Option[i64] = None;
    let n = none ?? LIMIT;
    let mut doc = Doc {
        nodes: [
            Node { parent: None, label: Some("r".to_string()) },
            Node { parent: Some(0), label: None },
            Node { parent: Some(1), label: None },
        ],
        root: 0,
    };
    let e: Vec[i64] = Vec.new();
    let moved = doc.shift(doc.root, 4);
    println(f"{n} {TAG.clone()} {side(Shape.Rect(7, 2))} {side(Shape.Dot)} {code("b".to_string())}");
    println(f"{doc.depth(2)} {doc.label(0)} {doc.label(1)} {first([5, 6])} {first(e)} {moved}");
    for (i, x) in [10, 20].iter().enumerate() {
        println(f"{i}:{x}");
    }
    Ok(())
}
"#;
        assert_eq!(
            run_source(src),
            Ok(("3 t 7 0 2\n2 r - 5 -1 4\n0:10\n1:20\n".to_string(), Some(0)))
        );
    }

    /// A tuple of references is matched through them, and a binding of
    /// a part of a `shared` value borrows it rather than moving it out.
    #[test]
    fn ref_parts_and_shared_parts_in_patterns() {
        let src = r#"
enum Region { Eu, Us }
enum Cat { Food, Tech }
shared enum Jv { Str(String), Num(i64) }
fn rate(region: ref Region, cat: ref Cat) -> i64 {
    match (region, cat) {
        (Region.Eu, Cat.Food) => 5,
        (Region.Eu, _) => 20,
        (Region.Us, Cat.Tech) => 8,
        _ => 0,
    }
}
fn text(v: Option[Jv]) -> Option[String] {
    match v {
        Some(Jv.Str(s)) => Some(s.clone()),
        _ => None,
    }
}
fn main() {
    let r = Region.Eu;
    let c = Cat.Tech;
    println(f"{rate(r, c)} {rate(Region.Us, Cat.Tech)}");
    let t = text(Some(Jv.Str("hi".to_string()))) ?? "none".to_string();
    let u = text(Some(Jv.Num(3))) ?? "none".to_string();
    println(f"{t} {u}");
}
"#;
        assert_eq!(
            run_source(src),
            Ok(("20 8\nhi none\n".to_string(), Some(0)))
        );
    }

    /// A closure given to a library call borrows its elements; `?`
    /// converts the error through `From`; unsuffixed literals take their
    /// slot's type; `cmp` on scalars; a `mut ref` position and flag read
    /// through; `T.make()` on a type parameter calls `T`'s impl.
    #[test]
    fn library_closures_from_literals_and_type_param_calls() {
        let src = r#"
trait Make { fn make() -> Self; }
struct P { x: i64 }
impl Make for P { fn make() -> P { return P { x: 7 }; } }
fn mk[T: Make]() -> T { return T.make(); }
struct Low { n: i64 }
struct High { n: i64 }
impl From[Low] for High { fn from(l: Low) -> High { High { n: l.n * 10 } } }
fn low(fail: bool) -> Result[i64, Low] { if fail { Err(Low { n: 4 }) } else { Ok(1) } }
fn high(fail: bool) -> Result[i64, High] { let v = low(fail)?; Ok(v + 1) }
fn at(v: ref Vec[i64], i: mut ref i64, seen: mut ref bool) -> i64 {
    let x = v[i];
    i = i + 1;
    if not seen { seen = true; }
    x
}
fn main() {
    let mut cells: Vec[(i64, i64)] = Vec.new();
    cells.push((3, 1));
    cells.push((5, 2));
    cells.push((1, 3));
    cells.sort_by(|a, b| b.0.cmp(a.0));
    let p: P = mk();
    let r = match high(true) { Ok(v) => v, Err(e) => e.n };
    let s = match high(false) { Ok(v) => v, Err(e) => e.n };
    let mut f: f64 = 1.5;
    f += 1;
    let g = f * 2;
    let w: (i32, u8) = (7, 200);
    let mut i = 0;
    let mut seen = false;
    let v = [4, 9];
    let a = at(v, mut i, mut seen);
    let b = at(v, mut i, mut seen);
    println(f"{cells[0].0} {cells[2].1} {p.x} {r} {s} {g} {w.0} {w.1} {a} {b} {i} {seen}");
}
"#;
        assert_eq!(
            run_source(src),
            Ok(("5 3 7 40 2 5 7 200 4 9 2 true\n".to_string(), Some(0)))
        );
    }

    /// `par {}` branch bindings read after the block; `let x: T;`;
    /// `==` on `Vec`s; an array `clone`; `String.from` of a literal and of
    /// a `String`; `Self` and the bare type name in a generic impl; a
    /// store's subscripts before its value; an owned place destructured
    /// in place, its other parts dropped with it at scope end.
    #[test]
    fn par_joins_seq_eq_bare_self_and_store_order() {
        let src = r#"
struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"drop{self.id}"); } }
struct S3 { a: R, b: R }
enum G[T] { X(T), Y }
impl[T] G[T] {
    fn id(self) -> Self { return self; }
    fn alt(self, o: Self) -> Self { match self { G.Y => o, _ => self } }
}
fn idx(k: i64) -> i64 { print(f"i{k} "); k }
fn val(v: i64) -> i64 { print(f"v{v} "); v }
fn main() {
    let total = par {
        let a = idx(1);
        let b = 2;
        a + b
    };
    let x: i64;
    x = total * 10;
    let v = [1, 2, 3];
    let w: Vec[i64] = [1, 2, 3];
    let u: Vec[i64] = [1, 2];
    let w2: Vec[i64] = [1, 2, 3];
    let names = ["ab".to_string(), "cd".to_string()];
    let copy = names.clone();
    let s = String.from("lit");
    let t = String.from(s);
    let g: G[i64] = G.Y;
    let h = g.alt(G.X(4)).id();
    let mut m: Vec[Vec[i64]] = [[0, 0], [0, 0]];
    m[idx(1)][idx(0)] = val(9);
    println("");
    println(f"{total} {x} {w == w2} {w == u} {v[1]} {copy[1]} {t} {m[1][0]}");
    match h { G.X(n) => println(f"x{n}"), G.Y => println("y") }
    let p = S3 { a: R { id: 1 }, b: R { id: 2 } };
    let S3 { b, .. } = p;
    println(f"got{b.id}");
}
"#;
        assert_eq!(
            run_source(src),
            Ok((
                "i1 i1 i0 v9 \n3 30 true false 2 cd lit 9\nx4\ngot2\ndrop2\ndrop1\n".to_string(),
                Some(0)
            ))
        );
    }

    thread_local! {
        /// `KARAC_D5` for the test on this thread, which the process-wide
        /// flag cannot give.
        pub(super) static D5: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
    }

    /// Under D5 a bare parameter is borrowed, so passing it where an owned
    /// value is expected reads through the borrow: a `Copy` value copies, a
    /// handle counts, and anything else is a move out of the borrow, which
    /// the borrow check refuses. `own` keeps the old meaning.
    #[test]
    fn d5_borrowed_param_passed_as_owned() {
        D5.with(|c| c.set(Some(true)));
        let ok = r#"
#[derive(Copy, Clone)]
struct P { v: i64 }
impl P { fn twice(own self) -> i64 { self.v * 2 } }
shared struct H { v: i64 }
struct S { v: i64 }
impl S { fn triple(own self) -> i64 { self.v * 3 } }
fn take_p(p: own P) -> i64 { p.v }
fn take_h(h: own H) -> i64 { h.v }
fn f(p: P, h: H, s: own S) -> i64 { take_p(p) + p.twice() + take_h(h) + s.triple() }
fn main() { let h = H { v: 5 }; println(f"{f(P { v: 1 }, h, S { v: 2 })} {h.v}"); }
"#;
        assert_eq!(run_source(ok), Ok(("14 5\n".to_string(), Some(0))));
        let moved = r#"
struct S { v: i64 }
impl S { fn triple(own self) -> i64 { self.v * 3 } }
fn take(s: own S) -> i64 { s.v }
fn f(s: S) -> i64 { s.triple() }
fn g(s: S) -> i64 { take(s) }
fn main() { println(f"{f(S { v: 1 })} {g(S { v: 2 })}"); }
"#;
        let err = run_source(moved).unwrap_err();
        D5.with(|c| c.set(None));
        assert!(
            err.contains("move of (*_1) through a shared reference"),
            "{err}"
        );
    }

    /// Under D5 a borrowed handle (or `Option` of one) stored into a field
    /// counts a new reference, and `?` lends its error to a borrowing
    /// `From.from`, dropping the error after the call.
    #[test]
    fn d5_borrowed_handle_stored_and_from_borrows() {
        D5.with(|c| c.set(Some(true)));
        let src = r#"
shared struct T { v: i64, l: Option[T] }
fn node(v: i64, l: Option[T]) -> T { T { v, l } }
struct Low { n: i64 }
impl Drop for Low { fn drop(mut ref self) { println(f"drop {self.n}"); } }
struct High { n: i64 }
impl From[Low] for High { fn from(l: Low) -> High { High { n: l.n * 10 } } }
fn low(n: i64) -> Result[i64, Low] { if n > 0 { Ok(n) } else { Err(Low { n: 7 }) } }
fn high(n: i64) -> Result[i64, High] { let v = low(n)?; Ok(v) }
fn main() {
    let leaf = Some(node(1, None));
    let t = node(2, leaf);
    match t.l { Some(c) => println(f"{t.v} {c.v}"), None => println("none") }
    match leaf { Some(c) => println(f"{c.v}"), None => println("none") }
    match high(0) { Ok(v) => println(f"ok {v}"), Err(h) => println(f"err {h.n}") }
}
"#;
        let out = run_source(src);
        D5.with(|c| c.set(None));
        assert_eq!(out, Ok(("2 1\n1\ndrop 7\nerr 70\n".to_string(), Some(0))));
    }

    /// Under D5 a bare `self` of a non-`Copy` type borrows (core semantics
    /// §8): the caller keeps the receiver; `own self` still takes it.
    #[test]
    fn d5_bare_self_borrows() {
        D5.with(|c| c.set(Some(true)));
        let src = r#"
struct Ha { g: Array[String, 2] }
impl Ha { fn give(self) { println("held") } fn take(own self) -> Array[String, 2] { self.g } fn n(self) -> i64 { self.g[0].len() as i64 } }
#[derive(Copy, Clone)]
struct P { v: i64 }
impl P { fn twice(self) -> i64 { self.v * 2 } }
fn pass(h: Ha) { h.give() }
fn main() {
    let h = Ha { g: ["a".to_string(), "b".to_string()] };
    pass(h); h.give(); println(f"{h.n()} {P { v: 4 }.twice()}");
    println(h.take()[1]);
}
"#;
        let out = run_source(src);
        D5.with(|c| c.set(None));
        assert_eq!(
            out,
            Ok((
                "held
held
1 8
b
"
                .to_string(),
                Some(0)
            ))
        );
    }

    /// `let x = s` over a reference copies the reference (§5.1); a
    /// `sync struct` is a counted handle (§6.3); a `mut Slice` passed on
    /// is reborrowed; a literal's `bytes()` view outlives the statement.
    #[test]
    fn ref_rebind_sync_handle_slice_reborrow_literal_view() {
        let src = r#"
sync struct C { n: i64 }
fn read(c: C) -> i64 { c.n }
fn bump(xs: mut Slice[i64]) { xs[0] = xs[0] + 1; }
fn twice(xs: mut Slice[i64]) { bump(xs); bump(xs); }
fn main() {
    let v: Vec[String] = ["ab".to_string(), "cde".to_string()];
    let mut n = 0;
    for s in v { let x = s; n = n + x.len(); }
    let a = C { n: 4 };
    let b = a;
    let mut w: Vec[i64] = [1, 2];
    twice(mut w);
    let bs = "héllo".bytes();
    println(f"{n} {read(a)} {b.n} {w[0]} {bs.len()} {v.len()}");
}
"#;
        assert_eq!(
            run_source(src),
            Ok((
                "5 4 4 3 6 2
"
                .to_string(),
                Some(0)
            ))
        );
    }

    /// A capture the closure body assigns to, or writes through a
    /// mutating method, is lent by `mut ref` (core semantics §9.1).
    #[test]
    fn closure_mutating_a_capture_borrows_it_mutably() {
        let src = r#"
fn main() {
    let mut log: Vec[i64] = [];
    let mut n = 0;
    let mut add = |x: i64| { log.push(x); n += x; };
    add(2);
    add(5);
    println(f"{log.len()} {n}");
}
"#;
        assert_eq!(
            run_source(src),
            Ok((
                "2 7
"
                .to_string(),
                Some(0)
            ))
        );
    }

    /// A closure passed for an `own F` slot the checker erased to `Fn(..)`
    /// still lends what it writes by `mut ref` (core semantics §9.1-§9.3):
    /// the adaptor holding it is a local view. The loan lasts while the
    /// adaptor lives and ends when `fold` consumes it.
    #[test]
    fn closure_in_a_generic_slot_lends_its_capture() {
        let iter = r#"
struct Counter { n: i64 }
impl Iterator for Counter {
    type Item = i64;
    fn next(mut ref self) -> Option[i64] {
        if self.n < 5 { self.n = self.n + 1; Some(self.n) } else { None }
    }
}
"#;
        let ok = format!(
            "{iter}
fn main() {{
    let mut seen = 0;
    let it = Counter {{ n: 0 }}.take(4).inspect(|x| {{ seen = seen + 1; }});
    let total = it.fold(0, |a, x| a + x);
    seen = seen + 100;
    println(f\"{{total}} {{seen}}\");
}}
"
        );
        assert_eq!(run_source(&ok), Ok(("10 104\n".to_string(), Some(0))));
        let live = format!(
            "{iter}
fn main() {{
    let mut seen = 0;
    let it = Counter {{ n: 0 }}.inspect(|x| {{ seen = seen + 1; }});
    seen = 7;
    println(f\"{{it.fold(0, |a, x| a + x)}}\");
}}
"
        );
        let err = run_source(&live).expect_err("a write while the adaptor lives");
        assert!(err.contains("E0516"), "{err}");
    }

    /// A tuple literal is built at its slot's element types: a struct
    /// field, an array element, a nested tuple, a return value.
    #[test]
    fn tuple_literal_takes_its_slots_element_widths() {
        let src = r#"
struct P { t: (i64, i64), n: i64 }
fn nest() -> ((i64, i64), i64) { let b: u8 = 200u8; let d: u32 = 4000000000u32; ((b, d), 7) }
fn main() {
    let b: u8 = 200u8;
    let d: u32 = 4000000000u32;
    let p = P { t: (b, d), n: 7 };
    let w: Vec[(i32, u8, i64)] = vec![(-1, 200, 3), (5, 7, 9)];
    let r = nest();
    println(f"{p.t.0 + p.t.1} {w[1].2} {r.0.0 + r.0.1}");
}
"#;
        assert_eq!(
            run_source(src),
            Ok(("4000000200 9 4000000200\n".to_string(), Some(0)))
        );
    }

    /// `T.size_of()` and `T.align_of()` answer from `mir::layout`, in a
    /// generic body for the instance's `T`.
    #[test]
    fn layout_queries() {
        let src = r#"
struct P { a: u8, b: i64, c: u16 }
enum E { A, B(i64), C(u8, String) }
shared struct S { x: i64 }
fn sz[T](x: T) -> i64 { T.size_of() }
fn main() {
    let a = i32.size_of();
    let b = u8.align_of();
    let c = String.size_of();
    let d = P.size_of();
    let e = P.align_of();
    let g = E.size_of();
    let i = sz(P { a: 1, b: 2, c: 3 });
    let j = sz((1u8, 2i32));
    let k = S.size_of();
    let l = i128.align_of();
    println(f"{a} {b} {c} {d} {e} {g} {i} {j} {k} {l}");
}
"#;
        assert_eq!(
            run_source(src),
            Ok(("4 1 24 24 8 32 24 8 8 16\n".to_string(), Some(0)))
        );
    }

    /// A branch value takes its widest arm: a literal first arm does not
    /// narrow an `i128` second arm to the literal's `i64`.
    #[test]
    fn branch_value_takes_its_widest_arm() {
        let src = r#"
fn pick(n: i64) -> i128 {
    let b: i128 = 100000000000000000000i128;
    let c = if b > 0 { b - 3 } else { 0 };
    let d = if b < 0 { 0 } else { b - 3 };
    let e = match n { 0 => 1, 1 => b * 2, _ => -1 };
    println(f"{c} {d} {e}");
    d
}
fn main() {
    let r = pick(1);
    println(f"{r}");
}
"#;
        assert_eq!(
            run_source(src),
            Ok((
                "99999999999999999997 99999999999999999997 200000000000000000000
99999999999999999997
"
                .to_string(),
                Some(0)
            ))
        );
    }

    /// `m[k] = v` on a map inserts, dropping any old value (design.md §9,
    /// `IndexSet`); a `SortedMap` reads by key like a `Map`. A `mut ref`
    /// argument is lent after the other arguments are evaluated (§5.6), so
    /// a later argument may read the same collection.
    #[test]
    fn map_index_set_and_two_phase_arguments() {
        let src = r#"
struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}"); } }
struct Node { left: i64, v: i64 }
fn insert(nodes: mut ref Vec[Node], at: i64, v: i64) -> i64 {
    nodes.push(Node { left: at, v });
    nodes.len() as i64 - 1
}
fn main() {
    let mut m: Map[String, i64] = Map.new();
    m["a"] = 1;
    m["a"] = 2;
    m[f"b"] = 3;
    m["b"] += 10;
    println(f"{m["a"]} {m["b"]} {m.len()}");
    let mut s: SortedMap[i64, R] = SortedMap.new();
    s[1] = R { id: 1 };
    s[1] = R { id: 2 };
    s[0] = R { id: 0 };
    println(f"{s[0].id} {s[1].id}");
    let mut nodes: Vec[Node] = Vec.new();
    nodes.push(Node { left: 7, v: 0 });
    let k = insert(mut nodes, nodes[0].left, 5);
    println(f"{k} {nodes[1].left} {nodes[1].v}");
}
"#;
        assert_eq!(
            run_source(src),
            Ok(("2 13 2\nd1\n0 2\n1 7 5\nd0\nd2\n".to_string(), Some(0)))
        );
    }

    /// An `escaping` function parameter takes its closure by value, as the
    /// parameter's function type, so the callee may store it (core
    /// semantics §9.3).
    #[test]
    fn escaping_fn_parameter_takes_the_closure_by_value() {
        let src = r#"
struct MapIt[T, B] { v: T, f: MutFn(T) -> B }
fn wrap[T, B](v: own T, f: escaping MutFn(T) -> B) -> MapIt[T, B] { MapIt { v: v, f: f } }
fn keep(f: escaping Fn(i64) -> i64) -> Vec[Fn(i64) -> i64] { let mut v: Vec[Fn(i64) -> i64] = Vec.new(); v.push(f); v }
fn main() {
    let k = 3;
    let mut m = wrap(4, |x| x * 10 + k);
    let r = (m.f)(m.v);
    let fs = keep(|x| x + k);
    println(f"{r} {fs[0](1)}");
}
"#;
        assert_eq!(run_source(src), Ok(("43 4\n".to_string(), Some(0))));
    }

    /// A generic type's `Drop` body runs for each instance of the type, at
    /// the instance's own argument types.
    #[test]
    fn generic_drop_body_per_instance() {
        let src = r#"
struct Box3[T] { v: T, tag: String }
impl[T] Drop for Box3[T] { fn drop(mut ref self) { println(f"dB{self.tag.len()}"); } }
fn main() {
    let a: Box3[String] = Box3 { v: f"aaaa", tag: f"ttt" };
    let b: Box3[i64] = Box3 { v: 7, tag: f"uuuuu" };
    println(f"{a.v.len()} {b.v}");
}
"#;
        assert_eq!(
            run_source(src),
            Ok(("4 7\ndB5\ndB3\n".to_string(), Some(0)))
        );
    }

    /// Lossless widening is implicit wherever the destination type is
    /// known: a binding, an argument, a field and a stored element
    /// (design.md §5, mixed-width operands).
    #[test]
    fn implicit_widening_at_known_destinations() {
        let src = r#"
fn takef(x: f64) -> f64 { x }
fn takel(x: i64) -> i64 { x }
struct S { f: f64, l: i64 }
fn main() {
    let a: i32 = 3;
    let u: u32 = 4000000000;
    let f: f64 = a;
    let g: f64 = u;
    let l: i64 = a;
    let s = S { f: a, l: u };
    let mut v: Vec[f64] = Vec.new();
    v.push(a);
    let h: f32 = 1.5;
    let d: f64 = h;
    println(f"{f} {g} {l} {takef(a)} {takel(a)} {s.f} {s.l} {v[0]} {d}");
}
"#;
        assert_eq!(
            run_source(src),
            Ok((
                "3 4000000000 3 3 3 3 4000000000 3 1.5\n".to_string(),
                Some(0)
            ))
        );
    }

    /// A `weak` slot read as a value is upgraded to an `Option` of a
    /// counted handle, and a strong handle stored into one is downgraded,
    /// in a field, a literal and a collection element alike.
    #[test]
    fn weak_slots_upgrade_on_read_and_downgrade_on_store() {
        let src = r#"
shared struct P { id: i64 }
shared struct N { id: i64, mut r: weak P }
fn main() {
    let z = P { id: 0 };
    let mut v: Vec[N] = Vec.new();
    v.push(N { id: 1, r: z });
    let ps: Vec[P] = vec![P { id: 5 }];
    v[0].r = ps[0];
    let mut w: Vec[weak P] = Vec.new();
    w.push(z);
    match v[0].r { Some(p) => println(f"{p.id}"), None => println("gone") }
    match w[0] { Some(p) => println(f"{p.id}"), None => println("gone") }
}
"#;
        let lowered = build_source(src).expect("builds");
        let text: String = lowered
            .program
            .bodies
            .values()
            .map(|b| crate::mir::pretty::pretty_body(b, &lowered.tys))
            .collect();
        for b in lowered.program.bodies.values() {
            let errs = crate::mir::validate::validate(b, &lowered.tys);
            assert!(errs.is_empty(), "{errs:?}");
        }
        assert_eq!(text.matches("(Downgrade)").count(), 3, "{text}");
        assert_eq!(text.matches("(Upgrade)").count(), 2, "{text}");
    }

    /// A method of an impl whose target nests its params: `self` in
    /// `impl[T] Option[Option[T]]` at `T = i64` is an `Option[Option[i64]]`,
    /// so its payload is the `Option[i64]` the method returns.
    #[test]
    fn a_nested_impl_target_types_self() {
        let src = r#"
impl[T] Option[Option[T]] {
    fn flat(self) -> Option[T] {
        match self {
            Some(inner) => inner,
            None => None,
        }
    }
}
fn main() {
    let a: Option[Option[i64]] = Some(Some(5));
    let b: Option[Option[i64]] = Some(None);
    match a.flat() {
        Some(v) => println(f"{v}"),
        None => println("none"),
    }
    println(f"{b.flat().is_none()}");
}
"#;
        assert_eq!(run_source(src), Ok(("5\ntrue\n".to_string(), Some(0))));
    }

    /// A trait implemented on a primitive (`impl Step for i64`): the
    /// method is called directly, through a generic function's bound, and
    /// on a generic impl's field.
    #[test]
    fn a_trait_impl_on_a_primitive_is_called_directly_and_generically() {
        let src = r#"
trait Step {
    fn succ(self) -> Self;
    fn gap(ref self, o: Self) -> i64;
}
impl Step for i64 {
    fn succ(self) -> i64 { self + 1 }
    fn gap(ref self, o: i64) -> i64 { o - self }
}
impl Step for char {
    fn succ(self) -> char { if self == 'a' { 'b' } else { 'z' } }
    fn gap(ref self, o: char) -> i64 { if self == o { 0 } else { 1 } }
}
fn twice[T: Step](x: T) -> T { x.succ().succ() }
struct R[T] { lo: T, hi: T }
impl[T: Step] R[T] {
    fn width(ref self) -> i64 { self.lo.gap(self.hi) }
}
fn main() {
    println(5.succ());
    println(twice(5));
    println(twice('a'));
    println(R { lo: 3, hi: 10 }.width());
    println(R { lo: 'a', hi: 'a' }.width());
}
"#;
        assert_eq!(
            run_source(src),
            Ok(("6\n7\nz\n7\n0\n".to_string(), Some(0)))
        );
    }

    /// Or-patterns, range patterns (ints and chars, open and closed) and
    /// `x @ p` / `ref x @ p` bindings.
    #[test]
    fn or_range_and_at_patterns() {
        let src = r#"
struct Foo { a: i64, n: i64 }
fn vowel(c: char) -> bool {
    return match c {
        'a' | 'e' | 'i' | 'o' | 'u' => true,
        _ => false,
    };
}
fn grade(n: i64) -> String {
    match n {
        0..10 => "low",
        10..=19 => "mid",
        x @ (20 | 21) => if x == 20 { "twenty" } else { "21" },
        _ => "high",
    }
}
fn main() {
    let f = Foo { a: 1, n: 2 };
    match f {
        ref w @ Foo { a, n } => println(f"{w.a} {a} {n}"),
    }
    println(f"{vowel('e')} {vowel('z')} {grade(3)} {grade(15)} {grade(20)} {grade(21)} {grade(99)}");
    let c = 'k';
    let k = match c { 'a'..='m' => 1, _ => 2 };
    println(k);
}
"#;
        assert_eq!(
            run_source(src),
            Ok((
                "1 1 2\ntrue false low mid twenty 21 high\n1\n".to_string(),
                Some(0)
            ))
        );
    }

    /// A `mut ref` read into a `let` is reborrowed, so writing through the
    /// parameter afterwards still reaches the caller; the prelude's
    /// `min`/`max` on integers and chars.
    #[test]
    fn mut_ref_let_reborrows_and_scalar_min_max() {
        let src = r#"
fn bump(acc: mut ref i64) {
    let a = acc;
    acc = a * 2 + 1;
}
fn main() {
    let mut x = 3;
    bump(mut x);
    let a: i32 = 7;
    println(f"{x} {min(3, 9)} {max(a, 2)} {min('q', 'c')}");
}
"#;
        assert_eq!(run_source(src), Ok(("7 3 7 c\n".to_string(), Some(0))));
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

    /// A `let mut` module binding, or one holding a cell, lives in a static:
    /// numbered in declaration order, computed by its own initializer body,
    /// and borrowed at each use. Any other module binding is computed where
    /// it is used.
    #[test]
    fn module_bindings_that_need_one_place_are_statics() {
        let src = r#"
let LIMIT: i64 = 40 + 2;
let HITS: Atomic[i64] = Atomic.new(5);
let mut NAMES: Vec[String] = Vec.new();
let mut COUNT: i64 = 0;
fn bump() {
    COUNT = COUNT + 1;
    NAMES.push(f"x{COUNT}");
    HITS.fetch_add(1, MemoryOrdering.SeqCst);
}
fn main() {
    bump();
    println(f"{COUNT} {NAMES.len()} {HITS.load(MemoryOrdering.SeqCst)} {LIMIT}");
}
"#;
        let lowered = build_source(src).unwrap_or_else(|e| panic!("{e}"));
        let tys = &lowered.tys;
        let statics: Vec<_> = lowered
            .program
            .statics
            .iter()
            .map(|s| crate::mir::parse::pretty_static(tys, s))
            .collect();
        assert_eq!(
            statics,
            [
                "static HITS: Atomic[i64] = static.HITS",
                "static mut NAMES: Vec[String] = static.NAMES",
                "static mut COUNT: i64 = static.COUNT",
            ]
        );
        for s in &lowered.program.statics {
            assert!(lowered.program.bodies.contains_key(&s.init), "{}", s.init);
        }
        let text = |name: &str| crate::mir::pretty::pretty_body(&lowered.program.bodies[name], tys);
        let bump = text("bump");
        for want in ["const &static#0", "const &static#1", "const &static#2"] {
            assert!(bump.contains(want), "{want}\n{bump}");
        }
        // The pushed `String` moves into the static `Vec`; it is not lent.
        assert!(!bump.contains("ref String"), "{bump}");
        // `LIMIT` is no static: its value is computed at the use.
        assert!(text("main").contains("const 42_i64"), "{}", text("main"));
        for b in lowered.program.bodies.values() {
            let errs = crate::mir::validate::validate(b, tys);
            assert!(errs.is_empty(), "{errs:?}");
        }
    }

    /// Slice patterns (design.md § Slice and array patterns): a `Vec` is
    /// matched as a view by length, with its elements and a named rest
    /// borrowed; an array destructures by position.
    #[test]
    fn slice_patterns() {
        let src = r#"
fn ends(v: ref Vec[i64]) -> i64 {
    match v {
        [first, .., last] => first + last,
        [only] => only,
        [] => -1,
    }
}
fn tail(v: ref Vec[String]) -> String {
    match v {
        [_, ..rest] => f"{rest.len()} {rest[0]}",
        [] => "none",
    }
}
fn main() {
    let mut v: Vec[i64] = Vec.new();
    println(ends(v));
    v.push(4);
    println(ends(v));
    v.push(5);
    v.push(6);
    println(ends(v));
    let w: Vec[String] = vec!["a", "b", "c"];
    println(tail(w));
    let arr: Array[i64, 5] = [1, 2, 3, 4, 5];
    let [first, ..mid, last] = arr;
    println(f"{first} {mid.len()} {mid[2]} {last}");
    let [a, b, .., z] = arr;
    println(f"{a} {b} {z}");
}
"#;
        assert_eq!(
            run_source(src),
            Ok(("-1\n4\n10\n2 b\n1 3 4 5\n1 2 5\n".to_string(), Some(0)))
        );
    }

    /// A range pattern's bound may be a constant or an integer type's
    /// limit; `v[a..=b]` ends after `b`; `b"..."` is an array of its bytes;
    /// `x @ P` under an owned scrutinee copies `P`'s `Copy` parts first.
    #[test]
    fn named_range_bounds_inclusive_slices_and_byte_strings() {
        let src = r#"
const LO: i64 = 10;
const HI: i64 = 20;
struct Foo { a: i64, n: i64 }
fn classify(n: i64) -> i64 {
    match n {
        ..LO => 1,
        LO..=HI => 2,
        _ => 3,
    }
}
fn main() {
    println(f"{classify(5)} {classify(10)} {classify(20)} {classify(25)}");
    let v: Vec[i64] = vec![1, 2, 3, 4, 5];
    let s = v[1..=3];
    println(f"{s.len()} {s[2]}");
    let b = b"a\x01";
    println(f"{b.len()} {b[0]} {b[1]}");
    match Foo { a: 1, n: 7 } {
        x @ Foo { a, n } => println(f"{a} {n} {x.n}"),
    }
    let m: i128 = 100000000000000000000i128;
    println(m.saturating_add(1));
}
"#;
        assert_eq!(
            run_source(src),
            Ok((
                "1 2 2 3\n3 4\n2 97 1\n1 7 7\n100000000000000000001\n".to_string(),
                Some(0)
            ))
        );
    }

    /// `v.push([])` checks the empty literal against the element slot, so
    /// it is built at that type.
    #[test]
    fn empty_literal_pushed_takes_the_element_type() {
        let src = r#"
fn main() {
    let mut cases: Vec[Vec[i64]] = Vec.new();
    cases.push([]);
    cases.push([1, 2]);
    println(f"{cases.len()} {cases[0].len()} {cases[1].len()}");
}
"#;
        assert_eq!(run_source(src), Ok(("2 0 2\n".to_string(), Some(0))));
    }

    #[test]
    fn variant_constructors_pushed_take_the_element_type() {
        let src = r#"
fn main() {
    let mut rs: Vec[Result[String, String]] = Vec.new();
    rs.push(Err("b"));
    rs.push(Ok("a"));
    let mut os: Vec[Option[i64]] = Vec.new();
    os.push(None);
    os.push(Some(3));
    let k = rs.len();
    let mut n = 0;
    for r in rs { match r { Ok(s) => n += s.len(), Err(_) => n += 10 } }
    println(f"{k} {n} {os.len()}");
}
"#;
        assert_eq!(run_source(src), Ok(("2 11 2\n".to_string(), Some(0))));
    }

    #[test]
    fn errdefer_bindings_deque_indexing_and_type_qualified_calls() {
        let src = r#"
fn body(n: i64) -> Result[i64, String] {
    errdefer(e) { println(f"cleanup {e}"); }
    if n < 0 { return Err(f"neg {n}"); }
    Ok(n * 2)
}
fn caller(n: i64) -> Result[i64, String] {
    errdefer(e) { println(e.len()); }
    let v = body(n)?;
    Ok(v + 1)
}
struct G[T] { k: T }
impl[T] G[T] {
    fn mk(x: own T) -> G[T] { G { k: x } }
}
fn main() {
    println(caller(3).unwrap_or(0));
    println(caller(-2).unwrap_or(0));
    let mut d: VecDeque[String] = VecDeque.new();
    d.push_back("b");
    d.push_front("a");
    d[1] = "c";
    println(f"{d[0]}{d[1]}");
    let g = G[i64].mk(5);
    println(g.k);
}
"#;
        assert_eq!(
            run_source(src),
            Ok(("7\ncleanup neg -2\n6\n0\nac\n5\n".to_string(), Some(0)))
        );
    }

    #[test]
    fn tuple_literals_take_the_slot_types() {
        let src = r#"
fn main() {
    let mut b: Vec[(i32, i32)] = Vec.new();
    b.push((1, 2));
    b[0] = (7, 8);
    let mut u: (u8, i32) = (0, 0);
    u = (3, 4);
    let o: Option[(i32, i32)] = Some((5, 6));
    match o { Some(p) => println(f"{b[0].0} {u.0} {p.1}"), None => {} }
}
"#;
        assert_eq!(run_source(src), Ok(("7 3 6\n".to_string(), Some(0))));
    }

    #[test]
    fn derived_message_round_trips() {
        let src = r#"
#[derive(Message)]
struct Pair { x: i64, name: String, ok: bool }

fn main() {
    let p = Pair { x: 150, name: "ab", ok: true };
    let bytes = p.encode();
    let q = Pair.decode(bytes);
    println(f"{q.x} {q.name} {q.ok}");
}
"#;
        assert_eq!(run_source(src), Ok(("150 ab true\n".to_string(), Some(0))));
    }

    #[test]
    fn variant_constructors_stored_through_mut_ref() {
        let src = r#"
fn gr(x: mut ref Result[String, i64]) { x = Err(7); }
fn gn(x: mut ref Option[String]) { x = None; }
fn main() {
    let mut r: Result[String, i64] = Ok("a");
    gr(mut r);
    let mut o: Option[String] = Some("b");
    gn(mut o);
    println(f"{r.is_err()} {o.is_none()}");
}
"#;
        assert_eq!(run_source(src), Ok(("true true\n".to_string(), Some(0))));
    }

    #[test]
    fn a_generic_call_takes_its_type_from_the_enclosing_return() {
        let src = r#"
trait Mk { fn mk() -> Self; }
struct B { v: i64 }
impl Mk for B { fn mk() -> B { B { v: 100 } } }
fn make[T: Mk]() -> T { T.mk() }
fn outer[T: Mk]() -> T { make() }
fn main() {
    let b: B = outer();
    println(b.v);
}
"#;
        assert_eq!(run_source(src), Ok(("100\n".to_string(), Some(0))));
    }

    /// `std.process`'s builder methods are Kāra, appended for a program that
    /// names `Command`; its `#[compiler_builtin]` methods stay native calls.
    #[test]
    fn baked_process_builder_bodies() {
        let src = r#"
struct Holder { c: Command, n: i64 }
fn main() {
    let h = Holder { c: Command.new("echo").arg("hi").env("K", "V"), n: 7 };
    println(h.n);
}
"#;
        assert_eq!(run_source(src), Ok(("7\n".to_string(), Some(0))));
        let src = r#"
fn main() {
    let c = Command.new("true");
    let r = c.spawn();
    println(r.is_ok());
}
"#;
        let lowered = build_source(src).unwrap_or_else(|e| panic!("{e}"));
        assert!(!lowered.program.bodies.contains_key("Command.spawn"));
        let main = crate::mir::pretty::pretty_body(&lowered.program.bodies["main"], &lowered.tys);
        assert!(main.contains("Command.spawn("), "{main}");
        // A program with its own `Command` keeps it; the library's would
        // define the name twice.
        let src = r#"
enum Command { Go(i64) }
fn main() {
    let c = Command.Go(3);
    match c { Command.Go(n) => println(n) }
}
"#;
        assert_eq!(run_source(src), Ok(("3\n".to_string(), Some(0))));
    }
}
