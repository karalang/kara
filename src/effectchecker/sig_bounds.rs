//! The signature rule (B-2026-11-23-17): a call to a generic method is
//! charged the instance's impl of each trait its signature bounds
//! (design.md § Effects).
//!
//! `karac check` checks a program against the library's signatures: a
//! library method's body is not in the program, so the calls it makes
//! through its bounds are not there to follow. `it.count()` on a program
//! iterator runs its `next`, `xs.iter().sum()` runs the item's `add`,
//! `v.extend(bag)` runs `bag`'s `into_iter`, and none of it is in the text
//! the effect pass walks. What a method without a body may call on the
//! types of a call is what its signature says: the trait whose impl it is a
//! method of, for the receiver, and each bound on a parameter of the method
//! or of its impl, for the type the call binds it to. Each is charged every
//! method the program wrote in that impl; a library impl in the way
//! (`IterMap[I, F]: Iterator`) is charged by its own bounds (`I: Iterator`).

use rustc_hash::FxHashMap;

use crate::ast::*;
use crate::ids::NodeId;
use crate::intern::Symbol;
use crate::token::Span;
use crate::typechecker::Type;

use super::elem_impls::{self, ElemTrait, Scope};
use super::{impl_trait_name, BoundEvent};

/// One impl block.
pub(crate) struct ImplEntry<'a> {
    imp: &'a ImplBlock,
    /// The trait it implements, by its last name.
    tr: Option<String>,
    /// The trait methods it wrote a body for, by key.
    written: Vec<Symbol>,
}

/// The program's impls and traits as the signature rule reads them.
#[derive(Default)]
pub(crate) struct SigIndex<'a> {
    entries: Vec<ImplEntry<'a>>,
    /// The impls of each trait for each type, by the trait's and the type's
    /// names.
    impls: FxHashMap<(String, String), Vec<usize>>,
    /// Each trait's impls.
    by_trait: FxHashMap<String, Vec<usize>>,
    /// Each impl method by its key, with its impl.
    methods: FxHashMap<Symbol, (usize, &'a Function)>,
    /// Each free function by name.
    fns: FxHashMap<Symbol, &'a Function>,
    traits: FxHashMap<String, &'a TraitDef>,
    /// Each associated type an impl binds, by the type's name and its own.
    assoc: FxHashMap<(String, String), Vec<(usize, &'a TypeExpr)>>,
}

/// A bound and what it is on: a parameter's name, `Self`, or a projection
/// (`Self.Item`).
type Bounds<'b> = Vec<(String, &'b TraitBound)>;

fn has_body(f: &Function) -> bool {
    !f.body.stmts.is_empty() || f.body.final_expr.is_some()
}

/// A library method the compiler implements: it has no Kāra body even in
/// the library, and the table in `elem_impls` says what it runs.
pub(crate) fn is_native(f: &Function) -> bool {
    f.attributes.iter().any(|a| a.is_bare("compiler_builtin"))
}

fn generics_of(gp: Option<&GenericParams>) -> Vec<&str> {
    gp.iter()
        .flat_map(|g| g.params.iter())
        .filter(|p| !p.is_const)
        .map(|p| p.name.as_str())
        .collect()
}

fn bounds_of<'b>(gp: Option<&'b GenericParams>, wc: Option<&'b WhereClause>) -> Bounds<'b> {
    let mut out: Bounds<'b> = Vec::new();
    for p in gp.iter().flat_map(|g| g.params.iter()) {
        out.extend(p.bounds.iter().map(|b| (p.name.clone(), b)));
    }
    for c in wc.iter().flat_map(|w| w.constraints.iter()) {
        match c {
            WhereConstraint::TypeBound {
                type_name, bounds, ..
            } => out.extend(bounds.iter().map(|b| (type_name.clone(), b))),
            // `Self.Item: Ord`.
            WhereConstraint::ProjectionBound {
                projection, bounds, ..
            } => {
                if let TypeKind::Path(p) = &projection.kind {
                    let name = p.segments.join(".");
                    out.extend(bounds.iter().map(|b| (name.clone(), b)));
                }
            }
            _ => {}
        }
    }
    out
}

fn strip(mut ty: &Type) -> &Type {
    while let Type::Ref(inner) | Type::MutRef(inner) | Type::Refinement { base: inner, .. } = ty {
        ty = inner;
    }
    ty
}

/// A type's name and arguments, for a type an impl can target.
fn type_head(ty: &Type) -> Option<(String, &[Type])> {
    match ty {
        Type::Named { name, args } => Some((name.clone(), args.as_slice())),
        Type::Shared(name) => Some((name.clone(), &[])),
        Type::TypeParam(_) => None,
        _ => crate::typechecker::type_to_concrete_or_param_name(ty).map(|n| (n, &[][..])),
    }
}

/// `ty` with each type parameter in `scope` replaced by what it stands for.
fn substitute(ty: &Type, scope: &[(String, Type)]) -> Type {
    match ty {
        Type::TypeParam(p) => scope
            .iter()
            .find(|(n, _)| n == p)
            .map(|(_, t)| t.clone())
            .unwrap_or_else(|| ty.clone()),
        Type::Named { name, args } => Type::Named {
            name: name.clone(),
            args: args.iter().map(|a| substitute(a, scope)).collect(),
        },
        Type::Tuple(ts) => Type::Tuple(ts.iter().map(|t| substitute(t, scope)).collect()),
        Type::Ref(inner) => Type::Ref(Box::new(substitute(inner, scope))),
        Type::MutRef(inner) => Type::MutRef(Box::new(substitute(inner, scope))),
        _ => ty.clone(),
    }
}

/// Bind the generic names in `te` to the parts of `ty` they stand at.
fn match_type_expr(te: &TypeExpr, ty: &Type, generics: &[&str], out: &mut FxHashMap<String, Type>) {
    match (&te.kind, ty) {
        (TypeKind::Path(p), _)
            if p.segments.len() == 1
                && p.generic_args.is_none()
                && generics.contains(&p.segments[0].as_str()) =>
        {
            out.entry(p.segments[0].clone())
                .or_insert_with(|| ty.clone());
        }
        (TypeKind::Path(p), Type::Named { args, .. }) => {
            for (ga, a) in p.generic_args.iter().flatten().zip(args) {
                if let GenericArg::Type(t) = ga {
                    match_type_expr(t, a, generics, out);
                }
            }
        }
        (TypeKind::Ref(i) | TypeKind::MutRef(i), Type::Ref(a) | Type::MutRef(a)) => {
            match_type_expr(i, a, generics, out)
        }
        (TypeKind::Ref(i) | TypeKind::MutRef(i), _) => match_type_expr(i, ty, generics, out),
        (TypeKind::Tuple(ts), Type::Tuple(parts)) => {
            for (t, a) in ts.iter().zip(parts) {
                match_type_expr(t, a, generics, out);
            }
        }
        _ => {}
    }
}

const DEPTH: usize = 12;

impl<'a> super::EffectChecker<'a> {
    pub(crate) fn build_sig_index(&self) -> SigIndex<'a> {
        let mut index = SigIndex::default();
        for item in &self.program.items {
            match item {
                Item::Function(f) => {
                    index.fns.entry(self.interner.intern(&f.name)).or_insert(f);
                }
                Item::TraitDef(t) => {
                    index.traits.entry(t.name.clone()).or_insert(t);
                }
                Item::ImplBlock(imp) => {
                    let TypeKind::Path(p) = &imp.target_type.kind else {
                        continue;
                    };
                    let Some(target) = p.segments.last() else {
                        continue;
                    };
                    let tr = impl_trait_name(imp);
                    let id = index.entries.len();
                    let mut written = Vec::new();
                    for it in &imp.items {
                        match it {
                            ImplItem::Method(m) => {
                                let key = self.impl_method_key(target, &m.name, tr);
                                index.methods.entry(key).or_insert((id, m));
                                if tr.is_some() && has_body(m) {
                                    written.push(key);
                                }
                            }
                            ImplItem::AssocType(a) => index
                                .assoc
                                .entry((target.clone(), a.name.clone()))
                                .or_default()
                                .push((id, &a.ty)),
                        }
                    }
                    if let Some(tr) = tr {
                        index
                            .impls
                            .entry((tr.to_string(), target.clone()))
                            .or_default()
                            .push(id);
                        index.by_trait.entry(tr.to_string()).or_default().push(id);
                    }
                    index.entries.push(ImplEntry {
                        imp,
                        tr: tr.map(str::to_string),
                        written,
                    });
                }
                _ => {}
            }
        }
        index
    }

    /// `tr` and the traits it requires of its implementors.
    fn sig_trait_closure(&self, tr: &str) -> Vec<String> {
        let mut out = vec![tr.to_string()];
        let mut i = 0;
        while i < out.len() {
            let supers: Vec<String> = match self.sig.traits.get(&out[i]) {
                Some(def) => def
                    .supertraits
                    .iter()
                    .filter_map(|b| b.path.last().cloned())
                    .collect(),
                None => match out[i].as_str() {
                    "Ord" => vec!["PartialOrd".to_string(), "PartialEq".to_string()],
                    "PartialOrd" | "Eq" => vec!["PartialEq".to_string()],
                    _ => Vec::new(),
                },
            };
            for s in supers {
                if !out.contains(&s) {
                    out.push(s);
                }
            }
            i += 1;
        }
        out
    }

    /// What the generic parameters of `imp` stand for when its target is
    /// `ty`.
    fn bind_impl(&self, imp: &ImplBlock, ty: &Type) -> FxHashMap<String, Type> {
        let mut bind = FxHashMap::default();
        let generics = generics_of(imp.generic_params.as_ref());
        if !generics.is_empty() {
            match_type_expr(&imp.target_type, strip(ty), &generics, &mut bind);
        }
        bind
    }

    /// The type `te` names under `bind`, where it is one this rule can look
    /// into.
    fn lower_type_expr(
        &self,
        te: &TypeExpr,
        bind: &FxHashMap<String, Type>,
        self_ty: Option<&Type>,
        depth: usize,
    ) -> Option<Type> {
        match &te.kind {
            TypeKind::Ref(inner) | TypeKind::MutRef(inner) => {
                self.lower_type_expr(inner, bind, self_ty, depth)
            }
            TypeKind::Tuple(ts) => ts
                .iter()
                .map(|t| self.lower_type_expr(t, bind, self_ty, depth))
                .collect::<Option<Vec<Type>>>()
                .map(Type::Tuple),
            TypeKind::Path(p) => match p.segments.as_slice() {
                [name] if p.generic_args.is_none() && name == "Self" => self_ty.cloned(),
                [name] if p.generic_args.is_none() && bind.contains_key(name) => {
                    bind.get(name).cloned()
                }
                [name] => {
                    let mut args = Vec::new();
                    for ga in p.generic_args.iter().flatten() {
                        let GenericArg::Type(t) = ga else {
                            return None;
                        };
                        args.push(self.lower_type_expr(t, bind, self_ty, depth)?);
                    }
                    Some(Type::Named {
                        name: name.clone(),
                        args,
                    })
                }
                [base, assoc] => {
                    let base = match base.as_str() {
                        "Self" => self_ty.cloned()?,
                        b => bind.get(b).cloned()?,
                    };
                    self.resolve_assoc(&base, assoc, depth + 1)
                }
                _ => None,
            },
            _ => None,
        }
    }

    /// `ty`'s associated type `assoc` (`Count`'s `Item`).
    fn resolve_assoc(&self, ty: &Type, assoc: &str, depth: usize) -> Option<Type> {
        if depth > DEPTH {
            return None;
        }
        let ty = strip(ty);
        if let Type::Existential { assoc_bindings, .. } = ty {
            return assoc_bindings
                .iter()
                .find(|(n, _)| n == assoc)
                .map(|(_, t)| t.clone());
        }
        let (head, _) = type_head(ty)?;
        let &(id, te) = self.sig.assoc.get(&(head, assoc.to_string()))?.first()?;
        let bind = self.bind_impl(self.sig.entries[id].imp, ty);
        self.lower_type_expr(te, &bind, Some(ty), depth + 1)
    }

    /// Every written method of every impl of `tr`: the type is not known.
    fn charge_trait_open(&self, tr: &str, span: Span, calls: &mut Vec<(Symbol, Span)>) {
        if let Some(et) = ElemTrait::of_trait(tr) {
            if let Some(keys) = self
                .interner
                .get(et.method())
                .and_then(|m| self.method_name_index.get(&m))
            {
                calls.extend(keys.iter().map(|&key| (key, span)));
            }
            return;
        }
        for &id in self.sig.by_trait.get(tr).into_iter().flatten() {
            for &key in &self.sig.entries[id].written {
                calls.push((key, span));
                self.charge_bound_calls_unresolved(key, span, calls);
            }
        }
    }

    /// Charge the methods the program wrote in the impl of `tr` for `ty`:
    /// what a body bounded on `tr` may call on a `ty`.
    fn charge_trait(
        &self,
        ty: &Type,
        tr: &str,
        span: Span,
        calls: &mut Vec<(Symbol, Span)>,
        depth: usize,
    ) {
        let ty = strip(ty);
        // The traits a built-in type implements natively and a derive
        // writes: what they reach in the type's parts.
        if depth > DEPTH || !elem_impls::is_solved(ty) {
            return self.charge_trait_open(tr, span, calls);
        }
        if let Some(et) = ElemTrait::of_trait(tr) {
            if let Some(impls) = self.elem_index.of(ty) {
                self.charge_reach(impls.reach(et, Scope::Whole), et, span, calls);
            }
            return;
        }
        match ty {
            Type::TypeParam(p) if p == "Self" => self.charge_trait_open(tr, span, calls),
            Type::TypeParam(p) => self.note_bound_event(BoundEvent::TraitParam {
                param: p.clone(),
                tr: tr.to_string(),
            }),
            Type::AssocProjection { .. } | Type::Dyn { .. } => {
                self.charge_trait_open(tr, span, calls)
            }
            Type::Existential { origin, scope, .. } => match self.witness_types.get(origin) {
                Some(witness) => {
                    let witness = substitute(witness, scope);
                    self.charge_trait(&witness, tr, span, calls, depth + 1)
                }
                // A library function's opaque result is built from what it
                // was given: the impls of those.
                None if self
                    .program
                    .library_start
                    .is_some_and(|start| origin.0 >= start) =>
                {
                    for (_, given) in scope {
                        self.charge_trait(given, tr, span, calls, depth + 1);
                    }
                }
                None => self.charge_trait_open(tr, span, calls),
            },
            _ => {
                let Some((head, _)) = type_head(ty) else {
                    return;
                };
                let Some(ids) = self.sig.impls.get(&(tr.to_string(), head)) else {
                    // Every iterator is an `IntoIterator` that hands itself
                    // over (`impl[I: Iterator] IntoIterator for I`).
                    if tr == "IntoIterator" {
                        self.charge_trait(ty, "Iterator", span, calls, depth + 1);
                    }
                    return;
                };
                for &id in ids {
                    let entry = &self.sig.entries[id];
                    let imp = entry.imp;
                    let bind = self.bind_impl(imp, ty);
                    if !entry.written.is_empty() {
                        // A generic impl's calls through its bounds, at
                        // what `ty` binds its parameters to.
                        for &key in &entry.written {
                            calls.push((key, span));
                            self.charge_bound_calls_under(key, Some(&bind), span, calls);
                        }
                        // What takes an `IntoIterator` drives the iterator
                        // `into_iter` returns.
                        if tr == "IntoIterator" {
                            if let Some(result) =
                                self.iterator_from_into_iter(imp, &bind, ty, depth)
                            {
                                self.charge_trait(&result, "Iterator", span, calls, depth + 1);
                            }
                        }
                        continue;
                    }
                    // A library impl: its methods call what its own bounds
                    // name.
                    let bounds = bounds_of(imp.generic_params.as_ref(), imp.where_clause.as_ref());
                    self.charge_bounds(&bounds, &bind, Some(ty), None, span, calls, depth + 1);
                }
            }
        }
    }

    /// The iterator a program's `into_iter` returns: the type behind its
    /// `impl Iterator`, or the type it names.
    fn iterator_from_into_iter(
        &self,
        imp: &ImplBlock,
        bind: &FxHashMap<String, Type>,
        ty: &Type,
        depth: usize,
    ) -> Option<Type> {
        let f = imp.items.iter().find_map(|it| match it {
            ImplItem::Method(m) if m.name == "into_iter" => Some(m),
            _ => None,
        })?;
        let ret = f.return_type.as_ref()?;
        match &ret.kind {
            TypeKind::ImplTrait { .. } => self
                .witness_types
                .get(&crate::resolver::SpanKey::from_span(&ret.span))
                .cloned(),
            _ => self.lower_type_expr(ret, bind, Some(ty), depth),
        }
    }

    /// `charge_trait` on `ty` and on every type inside it: a bound on a part
    /// of `ty` this rule cannot name.
    fn charge_trait_anywhere(
        &self,
        ty: &Type,
        tr: &str,
        span: Span,
        calls: &mut Vec<(Symbol, Span)>,
        depth: usize,
    ) {
        let ty = strip(ty);
        self.charge_trait(ty, tr, span, calls, depth);
        if ElemTrait::of_trait(tr).is_some() || depth > DEPTH {
            return;
        }
        match ty {
            Type::Named { args, .. } => {
                for a in args {
                    self.charge_trait_anywhere(a, tr, span, calls, depth + 1);
                }
            }
            Type::Tuple(ts) => {
                for t in ts {
                    self.charge_trait_anywhere(t, tr, span, calls, depth + 1);
                }
            }
            Type::Array { element, .. } | Type::Slice { element, .. } => {
                self.charge_trait_anywhere(element, tr, span, calls, depth + 1)
            }
            Type::Function { return_type, .. } | Type::OnceFunction { return_type, .. } => {
                self.charge_trait_anywhere(return_type, tr, span, calls, depth + 1)
            }
            _ => {}
        }
    }

    /// The type a bound is on: `Self`, a parameter the receiver's type or
    /// the call binds, or a projection of one.
    fn bound_subject(
        &self,
        name: &str,
        bind: &FxHashMap<String, Type>,
        self_ty: Option<&Type>,
        node: Option<NodeId>,
    ) -> Option<Type> {
        if name == "Self" {
            return self_ty.cloned();
        }
        if let Some((base, assoc)) = name.split_once('.') {
            let base = self.bound_subject(base, bind, self_ty, node)?;
            return self.resolve_assoc(&base, assoc, 0);
        }
        bind.get(name)
            .or_else(|| self.node_call_subs.get(&node?)?.get(name))
            .filter(|ty| elem_impls::is_solved(ty))
            .cloned()
    }

    /// Charge each bound in `bounds` on the type it is on.
    #[allow(clippy::too_many_arguments)]
    fn charge_bounds(
        &self,
        bounds: &Bounds<'_>,
        bind: &FxHashMap<String, Type>,
        self_ty: Option<&Type>,
        node: Option<NodeId>,
        span: Span,
        calls: &mut Vec<(Symbol, Span)>,
        depth: usize,
    ) {
        for (name, bound) in bounds {
            let name = name.as_str();
            // A function bound: the argument's effects are charged where it
            // is passed.
            if bound.fn_sig.is_some() {
                continue;
            }
            let Some(tr) = bound.path.last() else {
                continue;
            };
            if matches!(
                tr.as_str(),
                "Fn" | "MutFn" | "OnceFn" | "Copy" | "Send" | "Sync"
            ) {
                continue;
            }
            let subject = self.bound_subject(name, bind, self_ty, node);
            for t in self.sig_trait_closure(tr) {
                match (&subject, self_ty) {
                    (Some(ty), _) => self.charge_trait(ty, &t, span, calls, depth),
                    // A projection this rule could not resolve is some type
                    // inside the receiver's.
                    (None, Some(recv)) if name.contains('.') => {
                        self.charge_trait_anywhere(recv, &t, span, calls, depth)
                    }
                    (None, _) => self.charge_trait_open(&t, span, calls),
                }
            }
        }
    }

    /// Whether the signature rule has anything to charge: the program wrote
    /// a trait impl.
    pub(crate) fn sig_active(&self) -> bool {
        self.elem_index.has_program_impls()
    }

    /// `ty`, and `tr` with the traits it requires.
    pub(crate) fn charge_trait_bound(
        &self,
        ty: &Type,
        tr: &str,
        span: Span,
        calls: &mut Vec<(Symbol, Span)>,
    ) {
        if !self.sig_active() {
            return;
        }
        for t in self.sig_trait_closure(tr) {
            self.charge_trait(ty, &t, span, calls, 0);
        }
    }

    /// `key` is a method the library declares in Kāra, not one the compiler
    /// implements natively: its signature says what it runs.
    pub(crate) fn is_kara_library_method(&self, key: Symbol) -> bool {
        self.sig
            .methods
            .get(&key)
            .is_some_and(|&(_, f)| !has_body(f) && !is_native(f))
    }

    /// `tr` with the traits it requires, on a type the call does not say.
    pub(crate) fn charge_trait_bound_open(
        &self,
        tr: &str,
        span: Span,
        calls: &mut Vec<(Symbol, Span)>,
    ) {
        if !self.sig_active() {
            return;
        }
        for t in self.sig_trait_closure(tr) {
            self.charge_trait_open(&t, span, calls);
        }
    }

    /// A call to `callee`, a method or function with no body in the
    /// program: charge what its signature bounds. `recv` is the receiver's
    /// type, `shape` a type the impl's parameters are read off when there
    /// is no receiver (the call's own type, for `Type.build(..)`), and
    /// `node` the call, for the parameters the method itself declares; a
    /// call charged for a caller's instance has none.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn charge_signature(
        &self,
        callee: Symbol,
        recv: Option<&Type>,
        shape: Option<&Type>,
        node: Option<NodeId>,
        args: &[CallArg],
        span: Span,
        calls: &mut Vec<(Symbol, Span)>,
    ) {
        if !self.sig_active() {
            return;
        }
        let Some(&(id, f)) = self.sig.methods.get(&callee) else {
            if let (Some(f), Some(node)) = (self.sig.fns.get(&callee), node) {
                if !has_body(f) && !is_native(f) {
                    let bind = self.bind_from_args(f, args, FxHashMap::default(), Some(node));
                    let bounds = bounds_of(f.generic_params.as_ref(), f.where_clause.as_ref());
                    self.charge_bounds(&bounds, &bind, None, Some(node), span, calls, 0);
                }
            }
            return;
        };
        if has_body(f) || is_native(f) {
            return;
        }
        let entry = &self.sig.entries[id];
        let imp = entry.imp;
        let bind = match recv.or(shape) {
            Some(ty) => self.bind_impl(imp, ty),
            None => FxHashMap::default(),
        };
        // A method of a trait impl may call the impl's other methods on its
        // receiver (`count` calls `next`).
        if let (Some(tr), Some(ty), true) = (entry.tr.as_deref(), recv, f.self_param.is_some()) {
            for t in self.sig_trait_closure(tr) {
                self.charge_trait(ty, &t, span, calls, 0);
            }
        }
        let bounds = bounds_of(imp.generic_params.as_ref(), imp.where_clause.as_ref());
        self.charge_bounds(&bounds, &bind, recv, node, span, calls, 0);
        if node.is_some() {
            let bind = self.bind_from_args(f, args, bind, node);
            let bounds = bounds_of(f.generic_params.as_ref(), f.where_clause.as_ref());
            self.charge_bounds(&bounds, &bind, recv, node, span, calls, 0);
        }
    }

    /// `bind`, with each parameter the method declares that the call did not
    /// record read off the argument it types: a call the typechecker checks
    /// by its own rule (`v.extend(it)`) records none.
    fn bind_from_args(
        &self,
        f: &Function,
        args: &[CallArg],
        mut bind: FxHashMap<String, Type>,
        node: Option<NodeId>,
    ) -> FxHashMap<String, Type> {
        let generics = generics_of(f.generic_params.as_ref());
        if generics.is_empty() {
            return bind;
        }
        let recorded = node.and_then(|n| self.node_call_subs.get(&n));
        let mut found = FxHashMap::default();
        for (p, a) in f.params.iter().zip(args) {
            if let Some(ty) = self.sig_type_of(a.value.id) {
                match_type_expr(&p.ty, &ty, &generics, &mut found);
            }
        }
        // And one the result names (`collect`'s `C`), off the call's type.
        if let (Some(ret), Some(ty)) = (
            f.return_type.as_ref(),
            node.and_then(|n| self.sig_type_of(n)),
        ) {
            match_type_expr(ret, &ty, &generics, &mut found);
        }
        for (name, ty) in found {
            let solved = |t: &Type| elem_impls::is_solved(t);
            if !bind.contains_key(&name)
                && !recorded.and_then(|r| r.get(&name)).is_some_and(solved)
                && solved(&ty)
            {
                bind.insert(name, ty);
            }
        }
        bind
    }

    /// `x.method(..)` where `x`'s type is known only by the traits it
    /// implements (a type parameter's bounds, an `impl Trait` or a `dyn`),
    /// and one of them provides `method` with no body in the program: what
    /// the trait's signature says it may call, on whatever `x` is.
    pub(crate) fn charge_provided_method(
        &self,
        recv: &Type,
        traits: &[String],
        method: &str,
        node: NodeId,
        span: Span,
        calls: &mut Vec<(Symbol, Span)>,
    ) {
        if !self.sig_active() {
            return;
        }
        let no_bind = FxHashMap::default();
        for bound in traits {
            for t in self.sig_trait_closure(bound) {
                let Some(def) = self.sig.traits.get(&t) else {
                    continue;
                };
                let provided = def.items.iter().find_map(|ti| match ti {
                    TraitItem::Method(m) if m.name == method => m.body.as_ref().map(|b| (m, b)),
                    _ => None,
                });
                let Some((m, body)) = provided else {
                    continue;
                };
                if !body.stmts.is_empty() || body.final_expr.is_some() {
                    continue;
                }
                for s in self.sig_trait_closure(&t) {
                    self.charge_trait(recv, &s, span, calls, 0);
                }
                let bounds = bounds_of(m.generic_params.as_ref(), m.where_clause.as_ref());
                self.charge_bounds(&bounds, &no_bind, Some(recv), Some(node), span, calls, 0);
                return;
            }
        }
    }

    /// The traits a receiver of type `recv` is known by, when that is all
    /// that is known of it: a type parameter's bounds, an `impl Trait`'s or
    /// a `dyn`'s trait.
    pub(crate) fn receiver_traits(
        &self,
        recv: &Type,
        bounds: &FxHashMap<String, Vec<TraitBound>>,
    ) -> Vec<String> {
        match strip(recv) {
            Type::TypeParam(p) => bounds
                .get(p)
                .into_iter()
                .flatten()
                .filter_map(|b| b.path.last().cloned())
                .collect(),
            Type::Existential { trait_name, .. } | Type::Dyn { trait_name, .. } => {
                vec![trait_name.clone()]
            }
            _ => Vec::new(),
        }
    }

    /// The type of the expression `node`, where the signature rule could
    /// charge anything on it.
    pub(crate) fn sig_type_of(&self, node: NodeId) -> Option<std::sync::Arc<Type>> {
        self.sig_types.get(&node).cloned().or_else(|| {
            self.typeparam_nodes
                .get(&node)
                .map(|p| std::sync::Arc::new(Type::TypeParam(p.clone())))
        })
    }

    /// The `(parameter, trait)` pairs charging `tr` on `ty` leaves to the
    /// caller's own instance, for `compute_bound_calls`.
    pub(crate) fn trait_bound_events(&self, ty: &Type, tr: &str, span: Span) -> Vec<BoundEvent> {
        let saved = self.bound_events.borrow_mut().replace(Vec::new());
        let mut sink = Vec::new();
        for t in self.sig_trait_closure(tr) {
            self.charge_trait(ty, &t, span, &mut sink, 0);
        }
        let events = self.bound_events.borrow_mut().take().unwrap_or_default();
        *self.bound_events.borrow_mut() = saved;
        events
    }
}
