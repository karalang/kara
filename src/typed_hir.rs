//! Typed HIR tables (redesign M0): the type checker's results keyed by
//! [`NodeId`] and expressed in interned [`Ty`]s, for the MIR builder.
//!
//! Three tables, all read off [`TypeCheckResult`]'s node-keyed fields:
//!
//! * `node_types` — the final type of every expression and pattern;
//! * `calls` — for every call, the definition it calls and its generic
//!   arguments in the callee's positional order (impl's parameters first,
//!   then the function's, the same order [`ParamTy::index`] uses);
//! * `errors` — every node the bridge could not express, with the reason.
//!   Nothing is guessed: a node that fails to lower is missing from its table
//!   and present here, so the MIR builder can refuse it by name.
//!
//! Names are mapped to definitions through [`HirDefs`], which the
//! module-qualified definition table implements. The type checker still
//! speaks in names, so this bridge is where a name becomes a [`DefId`].

use rustc_hash::FxHashMap;

use crate::ids::{DefId, NodeId};
use crate::ty::{LowerError, Ty, TyCtxt, TyList};
use crate::typechecker::types::Type;
use crate::typechecker::TypeCheckResult;

/// What a call calls.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Callee {
    /// A function or method with a definition.
    Def(DefId),
    /// A compiler builtin with no definition (`println`, an intrinsic), by
    /// name.
    Builtin(String),
}

/// One call, resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedCall {
    pub callee: Callee,
    /// The callee's generic arguments by position. Empty for a non-generic
    /// callee.
    pub substs: TyList,
}

/// Why a node has no entry in its table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HirError {
    /// The node's type (or one of its call's type arguments) does not lower.
    Type(LowerError),
    /// The call's callee did not resolve to a definition or a builtin.
    UnknownCallee,
    /// The call has type arguments but its callee's generic parameter order
    /// is unknown, so they cannot be placed.
    UnorderedSubsts,
    /// The callee's generic parameter with this name was never solved.
    UnsolvedParam(String),
}

/// The name-to-definition map the bridge needs.
pub trait HirDefs {
    /// The struct or enum a type name denotes.
    fn type_def(&self, name: &str) -> Option<DefId>;
    /// The method `owner.method` (the type checker's `Type.method` key).
    fn method_callee(&self, owner: &str, method: &str) -> Option<Callee>;
    /// What a call's callee expression resolves to, by that expression's
    /// node.
    fn path_callee(&self, callee_expr: NodeId) -> Option<Callee>;
    /// A callable definition's generic parameter names in positional order,
    /// or `None` when unknown.
    fn generics(&self, def: DefId) -> Option<Vec<String>>;
}

/// The typed HIR tables of one program.
pub struct TypedHir {
    pub tcx: TyCtxt,
    pub node_types: FxHashMap<NodeId, Ty>,
    pub calls: FxHashMap<NodeId, ResolvedCall>,
    pub errors: Vec<(NodeId, HirError)>,
}

/// Build the typed HIR tables from a finished type check.
pub fn build(tc: &TypeCheckResult, defs: &dyn HirDefs) -> TypedHir {
    let tcx = TyCtxt::new();
    let mut node_types = FxHashMap::default();
    let mut calls = FxHashMap::default();
    let mut errors = Vec::new();

    let lookup = |name: &str| defs.type_def(name);
    let lower = |ty: &Type, frame: u32| {
        let names = tc
            .node_generic_frames
            .get(frame as usize)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        // An inner parameter shadows an outer one of the same name.
        let param = |p: &str| names.iter().rposition(|n| n == p).map(|i| i as u32);
        tcx.lower_legacy(ty, &lookup, &param)
    };

    // A call's callee expression is typed with the callee's GENERIC
    // signature, in the callee's parameters rather than the caller's. The
    // call's entry in `calls` (callee plus type arguments) is what describes
    // it, so it gets no `node_types` entry of its own.
    let callee_exprs: rustc_hash::FxHashSet<NodeId> =
        tc.node_call_callees.values().copied().collect();
    for (&id, (ty, frame)) in &tc.node_types {
        if callee_exprs.contains(&id) {
            continue;
        }
        match lower(ty, *frame) {
            Ok(t) => {
                node_types.insert(id, t);
            }
            Err(e) => errors.push((id, HirError::Type(e))),
        }
    }

    let call_ids = tc
        .node_method_callees
        .keys()
        .chain(tc.node_call_callees.keys())
        .copied();
    for id in call_ids {
        let callee = if let Some(key) = tc.node_method_callees.get(&id) {
            key.rsplit_once('.')
                .and_then(|(owner, method)| defs.method_callee(owner, method))
        } else {
            defs.path_callee(tc.node_call_callees[&id])
        };
        let Some(callee) = callee else {
            errors.push((id, HirError::UnknownCallee));
            continue;
        };
        // Type arguments are spelled in the CALLER's generics, which are the
        // frame the call node itself was typed in.
        let frame = tc.node_types.get(&id).map_or(0, |(_, f)| *f);
        match place_substs(tc.node_call_subs.get(&id), &callee, defs, |t| {
            lower(t, frame)
        }) {
            Ok(tys) => {
                let substs = tcx.intern_list(&tys);
                calls.insert(id, ResolvedCall { callee, substs });
            }
            Err(e) => errors.push((id, e)),
        }
    }

    errors.sort_by_key(|(id, _)| *id);
    TypedHir {
        tcx,
        node_types,
        calls,
        errors,
    }
}

/// A call's solved type arguments, in the callee's positional order.
fn place_substs(
    solved: Option<&FxHashMap<String, Type>>,
    callee: &Callee,
    defs: &dyn HirDefs,
    lower: impl Fn(&Type) -> Result<Ty, LowerError>,
) -> Result<Vec<Ty>, HirError> {
    let order = match callee {
        Callee::Def(def) => defs.generics(*def),
        Callee::Builtin(_) => None,
    };
    let Some(order) = order else {
        return match solved {
            Some(s) if !s.is_empty() => Err(HirError::UnorderedSubsts),
            _ => Ok(Vec::new()),
        };
    };
    order
        .iter()
        .map(|name| {
            let ty = solved
                .and_then(|s| s.get(name))
                .ok_or_else(|| HirError::UnsolvedParam(name.clone()))?;
            lower(ty).map_err(HirError::Type)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{Expr, ExprKind, ImplItem, Item, StmtKind, TypeKind};
    use crate::ty::TyKind;

    /// A name-keyed stand-in for the definition table: one id per struct,
    /// enum, function and method, in source order.
    struct Names {
        defs: Vec<(String, Vec<String>)>,
        callee_exprs: FxHashMap<NodeId, String>,
    }

    impl Names {
        fn id(&self, name: &str) -> Option<DefId> {
            self.defs
                .iter()
                .position(|(n, _)| n == name)
                .map(|i| DefId(i as u32))
        }
    }

    impl HirDefs for Names {
        fn type_def(&self, name: &str) -> Option<DefId> {
            self.id(name)
        }
        fn method_callee(&self, owner: &str, method: &str) -> Option<Callee> {
            self.id(&format!("{owner}.{method}")).map(Callee::Def)
        }
        fn path_callee(&self, callee_expr: NodeId) -> Option<Callee> {
            let name = self.callee_exprs.get(&callee_expr)?;
            Some(match self.id(name) {
                Some(d) => Callee::Def(d),
                None => Callee::Builtin(name.clone()),
            })
        }
        fn generics(&self, def: DefId) -> Option<Vec<String>> {
            self.defs.get(def.0 as usize).map(|(_, g)| g.clone())
        }
    }

    fn generic_names(gp: &Option<crate::ast::GenericParams>) -> Vec<String> {
        gp.as_ref()
            .map(|g| g.params.iter().map(|p| p.name.clone()).collect())
            .unwrap_or_default()
    }

    fn collect_callees(e: &Expr, out: &mut FxHashMap<NodeId, String>) {
        match &e.kind {
            ExprKind::Call { callee, args } => {
                if let ExprKind::Identifier(n) = &callee.kind {
                    out.insert(callee.id, n.clone());
                }
                for a in args {
                    collect_callees(&a.value, out);
                }
            }
            ExprKind::MethodCall { object, args, .. } => {
                collect_callees(object, out);
                for a in args {
                    collect_callees(&a.value, out);
                }
            }
            ExprKind::StructLiteral { fields, .. } => {
                for f in fields {
                    collect_callees(&f.value, out);
                }
            }
            _ => {}
        }
    }

    fn names_of(program: &crate::ast::Program) -> Names {
        let mut defs = vec![
            ("Option".to_string(), vec!["T".to_string()]),
            ("Vec".to_string(), vec!["T".to_string()]),
        ];
        let mut callee_exprs = FxHashMap::default();
        for item in &program.items {
            match item {
                Item::Function(f) => {
                    defs.push((f.name.clone(), generic_names(&f.generic_params)));
                    for s in &f.body.stmts {
                        match &s.kind {
                            StmtKind::Let { value, .. } => {
                                collect_callees(value, &mut callee_exprs)
                            }
                            StmtKind::Expr(e) => collect_callees(e, &mut callee_exprs),
                            _ => {}
                        }
                    }
                    if let Some(e) = &f.body.final_expr {
                        collect_callees(e, &mut callee_exprs);
                    }
                }
                Item::StructDef(s) => defs.push((s.name.clone(), generic_names(&s.generic_params))),
                Item::ImplBlock(imp) => {
                    let TypeKind::Path(path) = &imp.target_type.kind else {
                        continue;
                    };
                    let name = path.segments.last().unwrap();
                    let outer = generic_names(&imp.generic_params);
                    for it in &imp.items {
                        if let ImplItem::Method(m) = it {
                            let mut g = outer.clone();
                            g.extend(generic_names(&m.generic_params));
                            defs.push((format!("{name}.{}", m.name), g));
                        }
                    }
                }
                _ => {}
            }
        }
        Names { defs, callee_exprs }
    }

    const SRC: &str = "
struct Pair[A, B] { a: A, b: B }
impl[A, B] Pair[A, B] {
    fn first(self) -> A { self.a }
    fn put[C](self, c: C) -> Pair[A, C] { Pair { a: self.a, b: c } }
}
fn id[T](x: T) -> T { x }
fn mk[T](x: T) -> Pair[T, bool] { Pair { a: id(x), b: true } }
fn main() {
    let p = mk(7);
    let q = p.put(\"s\");
    let n = q.first();
    println(n);
}
";

    fn typed(src: &str) -> (crate::ast::Program, TypedHir, Names) {
        let parsed = crate::parse(src);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let mut program = parsed.program;
        crate::prepare_for_resolve(&mut program);
        let r = crate::resolve(&program);
        let tc = crate::typecheck(&program, &r);
        assert!(tc.errors.is_empty(), "{:?}", tc.errors);
        let names = names_of(&program);
        let hir = build(&tc, &names);
        (program, hir, names)
    }

    fn let_value<'p>(program: &'p crate::ast::Program, fn_name: &str, i: usize) -> &'p Expr {
        for item in &program.items {
            if let Item::Function(f) = item {
                if f.name == fn_name {
                    if let StmtKind::Let { value, .. } = &f.body.stmts[i].kind {
                        return value;
                    }
                }
            }
        }
        panic!("no let {i} in {fn_name}");
    }

    #[test]
    fn calls_resolve_with_positional_substs() {
        let (program, hir, names) = typed(SRC);
        let show = |t: Ty| {
            hir.tcx
                .display(t, &|d: DefId| names.defs[d.0 as usize].0.clone())
        };
        assert!(hir.errors.is_empty(), "{:?}", hir.errors);
        let call = |i: usize| &hir.calls[&let_value(&program, "main", i).id];

        // `mk(7)`: a path call, T = i64.
        let mk = call(0);
        assert_eq!(mk.callee, Callee::Def(names.id("mk").unwrap()));
        let substs: Vec<String> = hir.tcx.list(mk.substs).into_iter().map(show).collect();
        assert_eq!(substs, ["i64"]);

        // `p.put("s")`: impl params first, then the method's.
        let with = call(1);
        assert_eq!(with.callee, Callee::Def(names.id("Pair.put").unwrap()));
        let substs: Vec<String> = hir.tcx.list(with.substs).into_iter().map(show).collect();
        assert_eq!(substs, ["i64", "bool", "String"]);

        let ty = hir.node_types[&let_value(&program, "main", 1).id];
        assert_eq!(show(ty), "Pair[i64, String]");
    }

    #[test]
    fn generic_bodies_type_nodes_with_positional_params() {
        let (_, hir, names) = typed(SRC);
        // `id(x)` inside `mk[T]`: the argument is spelled in mk's generics,
        // so it substitutes T by mk's position 0.
        let id_def = Callee::Def(names.id("id").unwrap());
        let id_call = hir
            .calls
            .values()
            .find(|c| c.callee == id_def)
            .expect("id(x) resolved");
        let [arg] = hir.tcx.list(id_call.substs)[..] else {
            panic!("one subst");
        };
        match hir.tcx.kind(arg) {
            TyKind::Param(p) => assert_eq!(p.index, 0),
            k => panic!("expected mk's T, got {k:?}"),
        }
    }

    #[test]
    fn builtins_resolve_and_errors_are_reported_not_guessed() {
        let (_, hir, names) = typed(SRC);
        assert!(hir
            .calls
            .values()
            .any(|c| c.callee == Callee::Builtin("println".into())));

        // A table that knows no types: every Adt-typed node is an error, and
        // nothing is silently dropped.
        let mut program = crate::parse(SRC).program;
        crate::prepare_for_resolve(&mut program);
        let r = crate::resolve(&program);
        let tc = crate::typecheck(&program, &r);
        let blind = Names {
            defs: Vec::new(),
            callee_exprs: names.callee_exprs.clone(),
        };
        let hir2 = build(&tc, &blind);
        let typed_nodes = tc
            .node_types
            .keys()
            .filter(|id| !tc.node_call_callees.values().any(|c| c == *id))
            .count();
        let type_errors = hir2
            .errors
            .iter()
            .filter(|e| matches!(e.1, HirError::Type(_)) && !hir2.calls.contains_key(&e.0))
            .count();
        assert_eq!(hir2.node_types.len() + type_errors, typed_nodes);
        assert!(hir2
            .errors
            .iter()
            .any(|(_, e)| matches!(e, HirError::Type(LowerError::UnknownName(n)) if n == "Pair")));
    }
}
