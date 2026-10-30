//! Effects through a non-escaping `dyn` parameter (design.md § `dyn Trait`,
//! Effects).
//!
//! A `dyn` value is treated like a function value. A call through a `dyn`
//! parameter that is not `escaping` has, at each call site, the effects of
//! the value passed there, charged to that caller; for a value coerced at
//! that call, those are the concrete type's methods' effects. So the call
//! through the parameter adds nothing to the callee's own effects
//! (`dyn_param_uses`), and each call that passes such a parameter is charged
//! for what it passes (`charge_dyn_args`).
//!
//! A `dyn` type with a `with` clause refuses a coerced value whose methods
//! have an effect outside it (`check_dyn_clause_slot`), at each position the
//! `Fn(..)` slot check reads a slot.
//!
//! Anywhere else (a field, an element, an `escaping` parameter, a return, a
//! local) a call through a `dyn` value has the effects its type declares:
//! the `with` clause, narrowed to the trait method's ceiling
//! (`stored_dyn_clause_key`). A type with no clause has every effect, which
//! here is every impl of the trait in the program. A value passed to a
//! non-escaping parameter that was NOT coerced at the call contributes the
//! same, and one forwarded from the caller's own non-escaping `dyn`
//! parameter contributes nothing: the caller's callers pay for it.
//!
//! An effect variable in the clause (`dyn Handler with E`) is solved from
//! the values coerced into it, as a function type's is (`effect_vars.rs`),
//! and like naming a function value, the coercion charges the concrete
//! type's methods to the function that makes it
//! (`charge_var_clause_coercion`). A call through the value is then charged
//! only what the clause writes besides the variable.

use rustc_hash::{FxHashMap, FxHashSet};

use crate::ast::*;
use crate::ids::NodeId;
use crate::intern::Symbol;
use crate::resolver::SpanKey;
use crate::span_visitor::MutVisitor;
use crate::token::Span;

use super::{
    declared_admits, verb_name, DeclaredEffects, EffectError, EffectErrorKind, EffectOrigin,
    EffectSet,
};

/// A clause as written, for keys and messages: its items in order.
fn clause_text(list: &EffectList) -> String {
    list.items
        .iter()
        .map(|i| match i {
            EffectItem::Verb(v) => {
                let resources: Vec<String> = v.resources.iter().map(|r| r.path.join(".")).collect();
                if resources.is_empty() {
                    verb_name(&v.kind)
                } else {
                    format!("{}({})", verb_name(&v.kind), resources.join(", "))
                }
            }
            EffectItem::Group(n) | EffectItem::Variable(n) => n.clone(),
            EffectItem::Polymorphic => "_".to_string(),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn clause_has_var(list: &EffectList) -> bool {
    list.items
        .iter()
        .any(|i| matches!(i, EffectItem::Variable(_) | EffectItem::Polymorphic))
}

/// A non-escaping `dyn` parameter of a callee: its position among the
/// non-receiver parameters, its name when it is a plain binding, and the
/// trait the type names.
#[derive(Debug, Clone)]
pub(crate) struct DynParam {
    index: usize,
    name: Option<String>,
    trait_name: String,
}

/// Counts the bindings of `name` in a function, parameters included, and
/// records the nodes of the identifiers spelling it.
struct NameUses<'n> {
    name: &'n str,
    bindings: usize,
    uses: Vec<NodeId>,
}

impl MutVisitor for NameUses<'_> {
    fn span(&mut self, _span: &mut Span) {}
    fn expr(&mut self, e: &mut Expr) {
        if matches!(&e.kind, ExprKind::Identifier(n) if n == self.name) {
            self.uses.push(e.id);
        }
    }
    fn pattern(&mut self, p: &mut Pattern) {
        match &p.kind {
            PatternKind::Binding(n) if n == self.name => self.bindings += 1,
            PatternKind::AtBinding { name, .. } if name == self.name => self.bindings += 1,
            _ => {}
        }
    }
}

impl<'a> super::EffectChecker<'a> {
    /// The trait a parameter's type names when it is a `dyn` type, or a view
    /// of one, without `escaping`.
    fn non_escaping_dyn_trait(&self, ty: &TypeExpr) -> Option<String> {
        let escaping = &self.program.escaping_fn_types;
        if escaping.contains(&SpanKey::from_span(&ty.span)) {
            return None;
        }
        let mut t = ty;
        while let TypeKind::Ref(inner) | TypeKind::MutRef(inner) = &t.kind {
            t = inner;
        }
        match &t.kind {
            TypeKind::Dyn { trait_path, .. }
                if !escaping.contains(&SpanKey::from_span(&t.span)) =>
            {
                trait_path.segments.last().cloned()
            }
            _ => None,
        }
    }

    fn dyn_params_of(&self, params: &[Param]) -> Vec<DynParam> {
        params
            .iter()
            .enumerate()
            .filter_map(|(index, p)| {
                self.non_escaping_dyn_trait(&p.ty)
                    .map(|trait_name| DynParam {
                        index,
                        name: p.name().map(str::to_string),
                        trait_name,
                    })
            })
            .collect()
    }

    /// Fill `dyn_params`, `dyn_param_uses` and the impl tables. Runs once
    /// `method_bodies` is complete.
    pub(crate) fn compute_dyn_tables(&mut self) {
        let mut dyn_params: FxHashMap<Symbol, Vec<DynParam>> = FxHashMap::default();
        let mut uses: FxHashSet<NodeId> = FxHashSet::default();
        for (&key, f) in self.function_bodies.iter().chain(self.method_bodies.iter()) {
            let ps = self.dyn_params_of(&f.params);
            if ps.is_empty() {
                continue;
            }
            // Every mention of the parameter's name is the parameter when
            // nothing else in the function binds that name.
            for p in &ps {
                let Some(name) = &p.name else { continue };
                let mut copy: Function = (**f).clone();
                let mut w = NameUses {
                    name,
                    bindings: 0,
                    uses: Vec::new(),
                };
                crate::span_visitor::walk_function_mut_with(&mut copy, &mut w);
                if w.bindings == 1 {
                    uses.extend(w.uses);
                }
            }
            dyn_params.insert(key, ps);
        }
        // A trait method with no body is a callee too, through a bound or a
        // `dyn` value: its impls share its signature.
        let mut impl_methods: FxHashMap<(String, String), Vec<Symbol>> = FxHashMap::default();
        let mut supertraits: FxHashMap<String, Vec<String>> = FxHashMap::default();
        let mut defaults: FxHashMap<String, Vec<(String, Symbol)>> = FxHashMap::default();
        for item in &self.program.items {
            match item {
                Item::TraitDef(t) => {
                    supertraits.insert(
                        t.name.clone(),
                        t.supertraits
                            .iter()
                            .filter_map(|b| b.path.last().cloned())
                            .collect(),
                    );
                    for it in &t.items {
                        let TraitItem::Method(m) = it else { continue };
                        let key = self.interner.dotted_str(&t.name, &m.name);
                        if m.body.is_some() {
                            defaults
                                .entry(t.name.clone())
                                .or_default()
                                .push((m.name.clone(), key));
                        }
                        if let std::collections::hash_map::Entry::Vacant(slot) =
                            dyn_params.entry(key)
                        {
                            let ps = self.dyn_params_of(&m.params);
                            if !ps.is_empty() {
                                slot.insert(ps);
                            }
                        }
                    }
                }
                Item::ImplBlock(imp) => {
                    let (Some(trait_path), TypeKind::Path(ty)) =
                        (&imp.trait_name, &imp.target_type.kind)
                    else {
                        continue;
                    };
                    let (Some(trait_name), Some(type_name)) =
                        (trait_path.segments.last(), ty.segments.last())
                    else {
                        continue;
                    };
                    let keys = impl_methods
                        .entry((trait_name.clone(), type_name.clone()))
                        .or_default();
                    for it in &imp.items {
                        if let ImplItem::Method(m) = it {
                            keys.push(self.interner.dotted_str(type_name, &m.name));
                        }
                    }
                }
                _ => {}
            }
        }
        // A default method the impl does not override runs the trait's body.
        for ((trait_name, type_name), keys) in impl_methods.iter_mut() {
            for (method, key) in defaults.get(trait_name).into_iter().flatten() {
                let own = self.interner.dotted_str(type_name, method);
                if !keys.contains(&own) {
                    keys.push(*key);
                }
            }
        }
        self.dyn_params = dyn_params;
        self.dyn_param_uses = uses;
        self.dyn_impl_methods = impl_methods;
        self.dyn_supertraits = supertraits;
        self.seed_dyn_clause_keys();
    }

    /// One seeded key per clause written on a `dyn` value's type and method
    /// of its trait: the clause's effects, narrowed to the method's ceiling
    /// when the trait declares one.
    fn seed_dyn_clause_keys(&mut self) {
        let mut pairs: Vec<(String, EffectList)> = self
            .dyn_clauses
            .values()
            .map(|v| (v.trait_name.clone(), v.clause.clone()))
            .collect();
        pairs.sort_by_key(|(t, c)| (t.clone(), clause_text(c)));
        pairs.dedup_by_key(|(t, c)| (t.clone(), clause_text(c)));
        let methods: FxHashMap<String, Vec<String>> = self
            .program
            .items
            .iter()
            .filter_map(|item| match item {
                Item::TraitDef(t) => Some((
                    t.name.clone(),
                    t.items
                        .iter()
                        .filter_map(|it| match it {
                            TraitItem::Method(m) => Some(m.name.clone()),
                            TraitItem::AssocType(_) => None,
                        })
                        .collect(),
                )),
                _ => None,
            })
            .collect();
        for (trait_name, clause) in pairs {
            let text = clause_text(&clause);
            let written = self.resolve_effect_list_to_set(&clause, None);
            for t in self.trait_closure(&trait_name) {
                for m in methods.get(&t).into_iter().flatten() {
                    let method_key = self.interner.dotted_str(&t, m);
                    let ceiling = match self.declared_effects.get(&method_key) {
                        Some(DeclaredEffects::Explicit(set)) => Some(set.effect_set()),
                        _ => None,
                    };
                    let mut set = EffectSet::new();
                    for e in &written {
                        if ceiling.as_ref().is_none_or(|c| declared_admits(c, e)) {
                            set.add(e.clone(), EffectOrigin::Direct(clause.span));
                        }
                    }
                    let key = self
                        .interner
                        .intern(&format!("__dyn_clause[{text}].{t}.{m}"));
                    self.inferred_effects.insert(key, set);
                    self.dyn_clause_keys.insert((text.clone(), method_key), key);
                }
            }
        }
    }

    /// The seeded key a call of `method` through `object` is charged, when
    /// `object` is a `dyn` value whose type writes a clause (design.md §
    /// `dyn Trait`: the type's clause, narrowed to the method's ceiling).
    pub(crate) fn stored_dyn_clause_key(&self, object: &Expr, method: &str) -> Option<Symbol> {
        let mut e = object;
        while let ExprKind::Unary {
            op: UnaryOp::Deref,
            operand,
        } = &e.kind
        {
            e = operand;
        }
        let v = self.dyn_clauses.get(&e.id)?;
        let text = clause_text(&v.clause);
        self.trait_closure(&v.trait_name).iter().find_map(|t| {
            let method_key = self.interner.get(&format!("{t}.{method}"))?;
            self.dyn_clause_keys
                .get(&(text.clone(), method_key))
                .copied()
        })
    }

    /// A coercion into a `dyn` type whose clause names an effect variable
    /// charges the concrete type's methods to the function making it, as
    /// naming a function value does: calls through the stored value are not
    /// charged them.
    pub(crate) fn charge_var_clause_coercion(&self, expr: &Expr, calls: &mut Vec<(Symbol, Span)>) {
        let Some(c) = self.dyn_coercions.get(&expr.id) else {
            return;
        };
        if !c.clause.as_ref().is_some_and(clause_has_var) {
            return;
        }
        for key in self.dyn_methods(&c.trait_name, c.concrete.as_deref()) {
            calls.push((key, expr.span));
        }
    }

    /// The methods a value coerced to `dyn` at `value` (or at the place a
    /// `ref` / `mut ref` there borrows) may run.
    pub(crate) fn coerced_methods(&self, value: &Expr) -> Option<Vec<Symbol>> {
        let (_, c) = self.coercion_at(value)?;
        Some(self.dyn_methods(&c.trait_name, c.concrete.as_deref()))
    }

    /// Is `object`, the receiver of a method call, a non-escaping `dyn`
    /// parameter of the function being walked? A call through it is charged
    /// to the callers.
    pub(crate) fn is_dyn_param_receiver(&self, object: &Expr) -> bool {
        let mut e = object;
        while let ExprKind::Unary {
            op: UnaryOp::Deref,
            operand,
        } = &e.kind
        {
            e = operand;
        }
        self.dyn_param_uses.contains(&e.id)
    }

    /// The trait and its supertraits, each once.
    fn trait_closure(&self, trait_name: &str) -> Vec<String> {
        let mut out = vec![trait_name.to_string()];
        let mut i = 0;
        while i < out.len() {
            for s in self.dyn_supertraits.get(&out[i]).into_iter().flatten() {
                if !out.contains(s) {
                    out.push(s.clone());
                }
            }
            i += 1;
        }
        out
    }

    /// The methods a `dyn trait_name` value of `concrete` type may run, or of
    /// any type when the concrete type is unknown.
    fn dyn_methods(&self, trait_name: &str, concrete: Option<&str>) -> Vec<Symbol> {
        let traits = self.trait_closure(trait_name);
        let mut out = Vec::new();
        for ((t, ty), keys) in &self.dyn_impl_methods {
            if traits.contains(t) && concrete.is_none_or(|c| c == ty) {
                out.extend(keys.iter().copied());
            }
        }
        if concrete.is_some() && out.is_empty() {
            return self.dyn_methods(trait_name, None);
        }
        out
    }

    /// Charge a call's arguments passed to the callee's non-escaping `dyn`
    /// parameters. `callees` are the keys the call may resolve to; `offset`
    /// is 1 when the receiver is the first argument (`Type.method(x, ..)`).
    pub(crate) fn charge_dyn_args(
        &self,
        callees: &[Symbol],
        args: &[CallArg],
        offset: usize,
        calls: &mut Vec<(Symbol, Span)>,
    ) {
        for callee in callees {
            let Some(ps) = self.dyn_params.get(callee) else {
                continue;
            };
            for p in ps {
                let arg = args
                    .iter()
                    .find(|a| a.label.is_some() && a.label == p.name)
                    .or_else(|| args.get(p.index + offset).filter(|a| a.label.is_none()));
                let Some(arg) = arg else { continue };
                for key in self.dyn_arg_methods(&p.trait_name, &arg.value) {
                    calls.push((key, arg.value.span));
                }
            }
        }
    }

    /// What passing `value` to a non-escaping `dyn trait_name` parameter
    /// charges the caller.
    fn dyn_arg_methods(&self, trait_name: &str, value: &Expr) -> Vec<Symbol> {
        let mut e = value;
        loop {
            if let Some(c) = self.dyn_coercions.get(&e.id) {
                return self.dyn_methods(trait_name, c.concrete.as_deref());
            }
            if self.dyn_param_uses.contains(&e.id) {
                return Vec::new();
            }
            match &e.kind {
                ExprKind::Unary {
                    op: UnaryOp::Ref | UnaryOp::MutRef | UnaryOp::Deref,
                    operand,
                } => e = operand,
                _ => return self.dyn_methods(trait_name, None),
            }
        }
    }

    /// A function mentioned as a value is called where no call site says
    /// with what, so its non-escaping `dyn` parameters are charged every
    /// impl of their traits.
    pub(crate) fn charge_dyn_params_unresolved(
        &self,
        callee: Symbol,
        span: Span,
        calls: &mut Vec<(Symbol, Span)>,
    ) {
        for p in self.dyn_params.get(&callee).into_iter().flatten() {
            for key in self.dyn_methods(&p.trait_name, None) {
                calls.push((key, span));
            }
        }
    }

    /// The coercion to `dyn` at `value`, or at the place a `ref` / `mut ref`
    /// there borrows.
    fn coercion_at(&self, value: &Expr) -> Option<(NodeId, &super::DynCoercion)> {
        let mut e = value;
        loop {
            if let Some(c) = self.dyn_coercions.get(&e.id) {
                return Some((e.id, c));
            }
            match &e.kind {
                ExprKind::Unary {
                    op: UnaryOp::Ref | UnaryOp::MutRef,
                    operand,
                } => e = operand,
                _ => return None,
            }
        }
    }

    /// Refuse a value coerced into a `dyn` slot whose `with` clause its
    /// concrete type's methods exceed (design.md § `dyn Trait`, Effects),
    /// naming the slot as `subject`: one error per method and effect, at the
    /// coercion. A clause naming an effect variable is solved instead
    /// (`effect_vars.rs`).
    pub(crate) fn check_dyn_clause_slot(&mut self, subject: &str, value: &Expr) {
        if let Some((id, _)) = self.coercion_at(value) {
            self.check_coercion_clause(id, subject, value.span);
        }
    }

    /// [`Self::check_dyn_clause_slot`] for every coercion the slot positions
    /// did not reach (an assignment to a field, a `push`, a `Some(..)`): the
    /// MIR effect analysis trusts the clause of a `dyn` value it cannot
    /// trace, so every value coerced into one must fit it.
    pub(crate) fn check_remaining_dyn_coercions(&mut self) {
        let mut wanted: FxHashSet<NodeId> = self
            .dyn_coercions
            .iter()
            .filter(|(id, c)| {
                !self.dyn_clause_checked.contains(id)
                    && c.clause.as_ref().is_some_and(|l| !clause_has_var(l))
            })
            .map(|(&id, _)| id)
            .collect();
        if wanted.is_empty() {
            return;
        }
        struct Spans<'w> {
            wanted: &'w mut FxHashSet<NodeId>,
            found: Vec<(NodeId, Span)>,
        }
        impl MutVisitor for Spans<'_> {
            fn span(&mut self, _span: &mut Span) {}
            fn expr(&mut self, e: &mut Expr) {
                if self.wanted.remove(&e.id) {
                    self.found.push((e.id, e.span));
                }
            }
        }
        let mut w = Spans {
            wanted: &mut wanted,
            found: Vec::new(),
        };
        let bodies: Vec<super::FnHandle<'a>> = self
            .function_bodies
            .values()
            .chain(self.method_bodies.values())
            .cloned()
            .collect();
        for f in bodies {
            if w.wanted.is_empty() {
                break;
            }
            let mut copy: Function = (*f).clone();
            crate::span_visitor::walk_function_mut_with(&mut copy, &mut w);
        }
        let mut found = w.found;
        found.sort_by_key(|(_, sp)| (sp.line, sp.column));
        for (id, span) in found {
            self.check_coercion_clause(id, "value", span);
        }
    }

    fn check_coercion_clause(&mut self, id: NodeId, subject: &str, span: Span) {
        if !self.dyn_clause_checked.insert(id) {
            return;
        }
        let Some(c) = self.dyn_coercions.get(&id) else {
            return;
        };
        let (Some(list), Some(concrete)) = (c.clause.as_ref(), c.concrete.clone()) else {
            return;
        };
        if clause_has_var(list) {
            return;
        }
        let trait_name = c.trait_name.clone();
        let clause = self.resolve_effect_list_to_set(list, None);
        let written = format!("dyn {trait_name} with {}", clause_text(list));
        let traits = self.trait_closure(&trait_name);
        let mut methods: Vec<Symbol> = self
            .dyn_impl_methods
            .iter()
            .filter(|((t, ty), _)| traits.contains(t) && *ty == concrete)
            .flat_map(|(_, keys)| keys.iter().copied())
            .collect();
        methods.sort_by_key(|k| self.interner.resolve(*k).to_string());
        methods.dedup();
        let mut errors = Vec::new();
        for key in methods {
            let Some(set) = self.inferred_effects.get(&key) else {
                continue;
            };
            for te in &set.effects {
                if declared_admits(&clause, &te.effect) || self.is_slot_transparent(&te.effect) {
                    continue;
                }
                errors.push(EffectError {
                    message: format!(
                        "{subject} coerces `{concrete}` to `{written}`, but `{}` has effect {}({}), \
                         which the clause does not allow",
                        self.interner.resolve(key),
                        verb_name(&te.effect.verb),
                        te.effect.resource,
                    ),
                    span,
                    kind: EffectErrorKind::EffectSubtypeViolation,
                    subtype_trace: None,
                    replacement: None,
                });
            }
        }
        self.errors.extend(errors);
    }
}
