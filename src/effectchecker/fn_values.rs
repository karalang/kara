//! A call through an escaping function value (design.md §12 How a call
//! through a function value gets its effects, B-2026-11-19-38).
//!
//! A non-escaping parameter has, at each call site, the effects of the
//! argument passed there, charged to that caller. An escaping value (an
//! `escaping` parameter, a struct field, a collection element, and a `for`
//! binding over a collection of them) has the effects its type declares:
//! a call through it is charged the clause written on its function type,
//! nothing for `with ()`, and every effect when the type writes no clause.
//!
//! The type comes from the source: a field's declared type, a parameter's,
//! or a `let`'s annotation, and the element type of a collection typed one
//! of those ways. A clause that names an effect variable (`with E`) is not
//! charged here: it is solved per instance (`effect_vars.rs`).

use rustc_hash::{FxHashMap, FxHashSet};

use crate::ast::*;
use crate::ids::NodeId;
use crate::intern::Symbol;
use crate::resolver::SpanKey;
use crate::span_visitor::MutVisitor;
use crate::token::Span;

use super::{EffectOrigin, EffectSet};

/// The seeded key a call through a function value whose type writes no
/// clause is charged: every effect.
pub(crate) const FN_EVERY_KEY: &str = "__fn_every";

/// The prefix of the seeded key a call through a function value whose type
/// writes a clause is charged: `__fn_clause[writes(Db)]`.
pub(crate) const FN_CLAUSE_PREFIX: &str = "__fn_clause[";

/// How a name a function binds is typed, when the source says.
#[derive(Clone)]
enum Bound {
    /// A parameter, `escaping` or not, with its type.
    Param { ty: TypeExpr, escaping: bool },
    /// A `let` with a type annotation.
    Let(TypeExpr),
    /// A `for` binding: the iterated expression.
    For(Expr),
}

/// One pass over a function: how often each name is bound, the bindings
/// whose type the source says, and the calls through something other than
/// a function's name.
#[derive(Default)]
struct Scan {
    counts: FxHashMap<String, usize>,
    bound: FxHashMap<String, Bound>,
    calls: Vec<(NodeId, Expr)>,
}

impl MutVisitor for Scan {
    fn span(&mut self, _span: &mut Span) {}
    fn pattern(&mut self, p: &mut Pattern) {
        if let PatternKind::Binding(n) | PatternKind::AtBinding { name: n, .. } = &p.kind {
            *self.counts.entry(n.clone()).or_default() += 1;
        }
    }
    fn stmt(&mut self, s: &mut Stmt) {
        if let StmtKind::Let {
            pattern:
                Pattern {
                    kind: PatternKind::Binding(n),
                    ..
                },
            ty: Some(ty),
            ..
        } = &s.kind
        {
            self.bound.insert(n.clone(), Bound::Let(ty.clone()));
        }
    }
    fn expr(&mut self, e: &mut Expr) {
        match &e.kind {
            ExprKind::For {
                pattern:
                    Pattern {
                        kind: PatternKind::Binding(n),
                        ..
                    },
                iterable,
                ..
            } => {
                self.bound
                    .insert(n.clone(), Bound::For((**iterable).clone()));
            }
            ExprKind::Call { callee, .. } => {
                let mut c: &Expr = callee;
                while let ExprKind::Unary {
                    op: UnaryOp::Deref,
                    operand,
                } = &c.kind
                {
                    c = operand;
                }
                if matches!(
                    c.kind,
                    ExprKind::FieldAccess { .. } | ExprKind::Index { .. } | ExprKind::Identifier(_)
                ) {
                    self.calls.push((e.id, c.clone()));
                }
            }
            _ => {}
        }
    }
}

/// `t` without `ref` / `mut ref`.
fn peel(mut t: &TypeExpr) -> &TypeExpr {
    while let TypeKind::Ref(inner) | TypeKind::MutRef(inner) = &t.kind {
        t = inner;
    }
    t
}

/// The element type of a collection type, for `xs[i]` (`index`) or for a
/// `for` over it.
fn element(t: &TypeExpr, index: bool) -> Option<&TypeExpr> {
    match &peel(t).kind {
        TypeKind::Array { element, .. } | TypeKind::MutSlice(element) => Some(element),
        TypeKind::Path(p) => {
            let args = p.generic_args.as_ref()?;
            let ty = |i: usize| match args.get(i) {
                Some(GenericArg::Type(t)) => Some(t),
                _ => None,
            };
            match p.segments.last()?.as_str() {
                "Vec" | "VecDeque" | "Slice" | "Array" => ty(0),
                // Indexing a map reads a value; a `for` over one binds pairs.
                "Map" | "SortedMap" if index => ty(1),
                _ => None,
            }
        }
        _ => None,
    }
}

/// What a call through a value of type `t` is charged, when `t` is a
/// function type: its clause, or `None` for no clause. `Err` when the type
/// is no function type, or its clause is solved elsewhere.
fn clause_of(t: &TypeExpr) -> Result<Option<&EffectList>, ()> {
    match &peel(t).kind {
        TypeKind::FnType {
            effect_spec,
            is_once: false,
            ..
        } => match effect_spec {
            None => Ok(None),
            Some(EffectSpec::Specific(list))
                if !list
                    .items
                    .iter()
                    .any(|i| matches!(i, EffectItem::Variable(_) | EffectItem::Polymorphic)) =>
            {
                Ok(Some(list))
            }
            Some(_) => Err(()),
        },
        _ => Err(()),
    }
}

impl<'a> super::EffectChecker<'a> {
    /// Fill `fn_value_calls`: each call through an escaping function value,
    /// with the seeded key holding what it is charged. Runs once
    /// `method_bodies` is complete.
    pub(crate) fn compute_fn_value_calls(&mut self) {
        let mut fields: FxHashMap<String, FxHashMap<String, TypeExpr>> = FxHashMap::default();
        for item in &self.program.items {
            if let Item::StructDef(s) = item {
                fields.insert(
                    s.name.clone(),
                    s.fields
                        .iter()
                        .map(|f| (f.name.clone(), f.ty.clone()))
                        .collect(),
                );
            }
        }
        let escaping = &self.program.escaping_fn_types;
        let mut found: Vec<(NodeId, Option<EffectList>)> = Vec::new();
        let mut seen: FxHashSet<NodeId> = FxHashSet::default();
        let bodies =
            self.function_bodies
                .values()
                .map(|f| (None, f))
                .chain(self.method_bodies.iter().map(|(k, f)| {
                    // `Type.method`: `self` is a `Type`.
                    let key = self.interner.resolve(*k);
                    let owner = key.rsplit_once('.').map(|(t, _)| t);
                    (
                        owner.and_then(|t| t.rsplit('.').next()).map(str::to_string),
                        f,
                    )
                }));
        for (self_ty, f) in bodies {
            let mut copy: Function = (**f).clone();
            let mut scan = Scan::default();
            crate::span_visitor::walk_function_mut_with(&mut copy, &mut scan);
            if scan.calls.is_empty() {
                continue;
            }
            for p in &f.params {
                if let PatternKind::Binding(n) = &p.pattern.kind {
                    scan.bound.insert(
                        n.clone(),
                        Bound::Param {
                            ty: p.ty.clone(),
                            escaping: escaping.contains(&SpanKey::from_span(&p.ty.span))
                                || escaping.contains(&SpanKey::from_span(&peel(&p.ty).span)),
                        },
                    );
                }
            }
            let cx = Cx {
                scan: &scan,
                fields: &fields,
                objects: &self.fn_field_objects,
                self_ty: self_ty.as_deref(),
            };
            for (call, callee) in &scan.calls {
                if !seen.insert(*call) {
                    continue;
                }
                let ty = match &callee.kind {
                    // A parameter that is not `escaping` is charged to its
                    // callers; a `let` names a local, whose value's
                    // effects were charged where it was made.
                    ExprKind::Identifier(n) => match cx.binding(n) {
                        Some(Bound::Param { ty, escaping: true }) => Some(ty.clone()),
                        Some(Bound::For(it)) => cx.elements(it),
                        _ => None,
                    },
                    _ => cx.type_of(callee),
                };
                if let Some(Ok(clause)) = ty.as_ref().map(clause_of) {
                    found.push((*call, clause.cloned()));
                }
            }
        }
        for (call, clause) in found {
            let key = match clause {
                None => {
                    let key = self.interner.intern(FN_EVERY_KEY);
                    self.inferred_effects.entry(key).or_insert_with(|| {
                        super::every_effect_set(EffectOrigin::Direct(Span::default()))
                    });
                    key
                }
                Some(list) => {
                    let text = super::dyn_effects::clause_text(&list);
                    let key = self.interner.intern(&format!("{FN_CLAUSE_PREFIX}{text}]"));
                    let mut set = EffectSet::new();
                    for e in self.resolve_effect_list_to_set(&list, None) {
                        set.add(e, EffectOrigin::Direct(list.span));
                    }
                    self.inferred_effects.entry(key).or_insert(set);
                    key
                }
            };
            self.fn_value_calls.insert(call, key);
        }
    }

    /// The seeded key a call is charged for the function value it calls
    /// through, if it calls through an escaping one.
    pub(crate) fn fn_value_call_key(&self, call: &Expr) -> Option<Symbol> {
        self.fn_value_calls.get(&call.id).copied()
    }
}

/// What a scan of one function reads types from.
struct Cx<'s> {
    scan: &'s Scan,
    fields: &'s FxHashMap<String, FxHashMap<String, TypeExpr>>,
    objects: &'s FxHashMap<NodeId, String>,
    /// The type a method's `self` is.
    self_ty: Option<&'s str>,
}

impl Cx<'_> {
    /// The binding a name refers to, when the function binds it once.
    fn binding(&self, n: &str) -> Option<&Bound> {
        if self.scan.counts.get(n).copied().unwrap_or(0) != 1 {
            return None;
        }
        self.scan.bound.get(n)
    }

    /// The type the source gives `e`: a field's declared type, a typed
    /// binding's, or an element of one.
    fn type_of(&self, e: &Expr) -> Option<TypeExpr> {
        match &e.kind {
            ExprKind::Unary {
                op: UnaryOp::Deref,
                operand,
            } => self.type_of(operand),
            ExprKind::FieldAccess { object, field } => {
                let mut o: &Expr = object;
                while let ExprKind::Unary {
                    op: UnaryOp::Deref,
                    operand,
                } = &o.kind
                {
                    o = operand;
                }
                let s = match self.objects.get(&o.id) {
                    Some(s) => s.as_str(),
                    None if matches!(o.kind, ExprKind::SelfValue) => self.self_ty?,
                    None => return None,
                };
                self.fields.get(s)?.get(field).cloned()
            }
            ExprKind::Index { object, .. } => {
                let t = self.type_of(object)?;
                element(&t, true).cloned()
            }
            ExprKind::Identifier(n) => match self.binding(n)? {
                Bound::Param { ty, .. } | Bound::Let(ty) => Some(ty.clone()),
                Bound::For(it) => self.elements(it),
            },
            _ => None,
        }
    }

    /// The element type of what a `for` iterates: a collection typed by the
    /// source, or its `.iter()`.
    fn elements(&self, it: &Expr) -> Option<TypeExpr> {
        let base = match &it.kind {
            ExprKind::MethodCall {
                object,
                method,
                args,
                ..
            } if args.is_empty() && method == "iter" => object,
            _ => it,
        };
        let t = self.type_of(base)?;
        element(&t, false).cloned()
    }
}
