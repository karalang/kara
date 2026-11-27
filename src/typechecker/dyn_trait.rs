//! `dyn Trait` (design.md § `dyn Trait`): the type, the coercions into it,
//! and method calls through it.
//!
//! A `dyn Trait` written in a type lowers to `Type::Dyn`, with every
//! associated type of the trait and its supertraits bound. A value becomes
//! one only where a `dyn` type is expected (`dyn_coercion`, from
//! `check_assignable`), and each such site is recorded with its concrete
//! and target types in `node_dyn_coercions`; the expression keeps its
//! concrete type. A method call through a `dyn` value (or a view of one)
//! types as the trait's method and records `Trait.method` as its callee,
//! which the MIR builder turns into a call through the method table.
//!
//! Lives in a sibling `impl<'a> super::TypeChecker<'a>` block.

use crate::ast::*;
use crate::resolver::SpanKey;
use crate::token::Span;

use super::expr_method_call::BoundReceiver;
use super::inference::{resolve_type_var_top, substitute_type_params};
use super::types::{
    contains_type_param, substitute_existential_assoc_bindings, type_display, types_compatible,
    SubstValue, Type,
};
use super::TypeErrorKind;

impl<'a> super::TypeChecker<'a> {
    /// Lower `dyn Trait[A.., Name = T..]`. The trait's own arguments are
    /// positional; every associated type of the trait and its supertraits
    /// must be bound, and the bindings are kept in `dyn_assoc_names` order.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn lower_dyn_type(
        &mut self,
        trait_path: &PathExpr,
        args: &[GenericArg],
        assoc_bindings: &[AssocBinding],
        effects: Option<&EffectList>,
        cross_task: bool,
        span: Span,
        generic_scope: &[String],
    ) -> Type {
        let trait_name = trait_path.segments.join(".");
        let lowered_args: Vec<Type> = args
            .iter()
            .filter_map(|a| match a {
                GenericArg::Type(t) => Some(self.lower_type_expr_inner(t, generic_scope, false)),
                GenericArg::Const(_) | GenericArg::Shape(_) => None,
            })
            .collect();
        let bound: Vec<(String, Type, Span)> = assoc_bindings
            .iter()
            .map(|b| {
                (
                    b.name.clone(),
                    self.lower_type_expr_inner(&b.ty, generic_scope, false),
                    b.span,
                )
            })
            .collect();
        let Some(params) = self
            .env
            .traits
            .get(&trait_name)
            .map(|t| t.generic_param_names.clone())
        else {
            self.type_error(
                format!("`dyn {trait_name}`: `{trait_name}` is not a trait"),
                span,
                TypeErrorKind::TypeMismatch,
            );
            return Type::Error;
        };
        if lowered_args.len() != params.len() {
            self.type_error(
                format!(
                    "`dyn {trait_name}` takes {} type argument{}, found {}",
                    params.len(),
                    if params.len() == 1 { "" } else { "s" },
                    lowered_args.len()
                ),
                span,
                TypeErrorKind::WrongNumberOfArgs,
            );
            return Type::Error;
        }
        let names = self.dyn_assoc_names(&trait_name);
        for (name, _, at) in &bound {
            if !names.contains(name) {
                self.type_error(
                    format!(
                        "`dyn {trait_name}` binds `{name}`, which is not an associated type \
                         of `{trait_name}` or its supertraits"
                    ),
                    *at,
                    TypeErrorKind::TypeMismatch,
                );
                return Type::Error;
            }
        }
        if let Some((owner, method, why)) = self.dyn_first_undispatchable(&trait_name) {
            // A signature's types are lowered more than once.
            if self
                .errors
                .iter()
                .any(|e| e.span == span && e.message.starts_with("error[E_DYN_NOT_DISPATCHABLE]"))
            {
                return Type::Error;
            }
            let at = if owner == trait_name {
                String::new()
            } else {
                format!(" (from its supertrait `{owner}`)")
            };
            // A standard-library method's declaration is not the program's
            // to change, so the help names the supertrait instead.
            let declared_here = self
                .program
                .items
                .iter()
                .any(|i| matches!(i, Item::TraitDef(d) if d.name == owner));
            let change = if declared_here {
                format!("Give `{method}` a default body")
            } else {
                format!("Drop the supertrait `{owner}`")
            };
            self.type_error(
                format!(
                    "error[E_DYN_NOT_DISPATCHABLE]: `{trait_name}` cannot be used as `dyn`: \
                     its required method `{method}`{at} cannot be called through a method \
                     table, because {why}. {change}, or take the value through a generic \
                     parameter `[T: {trait_name}]` (design.md § `dyn Trait`)"
                ),
                span,
                TypeErrorKind::TypeMismatch,
            );
            return Type::Error;
        }
        let mut assoc = Vec::new();
        for name in names {
            let Some((_, ty, _)) = bound.iter().find(|(n, _, _)| *n == name) else {
                self.type_error(
                    format!(
                        "`dyn {trait_name}` leaves the associated type `{name}` unbound: a \
                         `dyn` type binds every one, as `dyn {trait_name}[{name} = T]`"
                    ),
                    span,
                    TypeErrorKind::TypeMismatch,
                );
                return Type::Error;
            };
            assoc.push((name, ty.clone()));
        }
        // `+ CrossTask` written, or `CrossTask` among the trait's
        // supertraits: every `dyn` of such a trait may cross.
        let cross_task = cross_task
            || self
                .env
                .supertrait_closure_traits(&trait_name)
                .iter()
                .any(|t| t == "CrossTask");
        Type::Dyn {
            trait_name,
            args: lowered_args,
            assoc,
            cross_task,
            clause: super::types::DynClause(effects.cloned()),
        }
    }

    /// The associated types a `dyn Trait` binds, in the order its type
    /// carries them: the trait's own, then each supertrait's, breadth-first.
    pub(super) fn dyn_assoc_names(&self, trait_name: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for t in self.env.supertrait_closure_traits(trait_name) {
            if let Some(info) = self.env.traits.get(&t) {
                for a in &info.assoc_types {
                    if !out.contains(a) {
                        out.push(a.clone());
                    }
                }
            }
        }
        out
    }

    /// A value flowing into a `dyn` slot: `Some(true)` when `found` coerces
    /// into `expected` (recorded for the MIR builder), `Some(false)` when it
    /// cannot and the error is reported, `None` when this is no coercion
    /// (not a `dyn` slot, or a `dyn` value already), which the ordinary
    /// rules judge.
    ///
    /// An owned `S` becomes an owned `dyn Trait`; a `ref S` / `mut ref S`, or
    /// an owned `S` lent, becomes the view of the same mutability.
    pub(super) fn dyn_coercion(
        &mut self,
        expected: &Type,
        found: &Type,
        span: Span,
    ) -> Option<bool> {
        let expected = resolve_type_var_top(expected, &self.env.substitutions);
        let (target, slot_view) = match &expected {
            Type::Dyn { .. } => (&expected, None),
            Type::Ref(inner) if matches!(**inner, Type::Dyn { .. }) => {
                (inner.as_ref(), Some(false))
            }
            Type::MutRef(inner) if matches!(**inner, Type::Dyn { .. }) => {
                (inner.as_ref(), Some(true))
            }
            _ => return None,
        };
        let found = resolve_type_var_top(found, &self.env.substitutions);
        let (concrete, found_view) = match &found {
            Type::Ref(inner) => (inner.as_ref(), Some(false)),
            Type::MutRef(inner) => (inner.as_ref(), Some(true)),
            t => (t, None),
        };
        if matches!(
            concrete,
            Type::Dyn { .. }
                | Type::Error
                | Type::Never
                | Type::TypeVar(_)
                | Type::Ref(_)
                | Type::MutRef(_)
        ) {
            return None;
        }
        // An owned `dyn` takes an owned value, and a `mut ref dyn` view
        // writes through a `mut ref`; the ordinary rules name the mismatch.
        if (slot_view.is_none() && found_view.is_some())
            || (slot_view == Some(true) && found_view == Some(false))
        {
            return None;
        }
        let Type::Dyn {
            trait_name,
            args,
            assoc,
            cross_task,
            ..
        } = target
        else {
            return None;
        };
        // A generic value still open (`Bag { xs: Vec.new() }`) takes its
        // arguments from the type's one concrete impl of the trait
        // (`impl Ac for Bag[String]`), as a call's bound would
        // (B-2026-11-20-8).
        let settled;
        let concrete = match self.open_dyn_value_from_impl(concrete, trait_name) {
            Some(t) => {
                settled = t;
                &settled
            }
            None => concrete,
        };
        let why = self
            .dyn_unfit(concrete, trait_name, args, assoc)
            .or_else(|| {
                // `dyn Trait + CrossTask`: the concrete type must be `CrossTask`,
                // since the `dyn` type no longer shows it (design.md § `dyn Trait`).
                let leak = cross_task
                    .then(|| {
                        crate::cross_task_safe::is_cross_task_safe_with(
                            concrete,
                            &self.env.structs,
                            &self.env.enums,
                        )
                        .err()
                    })
                    .flatten()?;
                let at = if leak.path.is_empty() {
                    String::new()
                } else {
                    format!(" (through {})", leak.path.join(", "))
                };
                Some(format!(
                    "`{}` is not `CrossTask`: it holds `{}`{at}, which cannot cross a task \
                 boundary",
                    type_display(concrete),
                    leak.unsafe_leaf
                ))
            });
        if let Some(why) = why {
            self.type_error(
                format!(
                    "expected '{}', found '{}': {why}",
                    type_display(&expected),
                    type_display(&found),
                ),
                span,
                TypeErrorKind::TypeMismatch,
            );
            return Some(false);
        }
        self.dyn_coercion_spans.insert(
            (self.current_impl_span, SpanKey::from_span(&span)),
            (found.clone(), expected.clone()),
        );
        Some(true)
    }

    /// Whether `expr`, checked against the `dyn` slot type `expected` and
    /// typed `ty`, is a compound (an `if`, a `match`, a block) whose
    /// results coerced into the slot one by one, rather than a coerced
    /// value itself, which keeps its concrete type.
    pub(super) fn coerced_within(&self, expr: &Expr, expected: &Type, ty: &Type) -> bool {
        let dyn_slot = |t: &Type| match resolve_type_var_top(t, &self.env.substitutions) {
            Type::Dyn { .. } => true,
            Type::Ref(inner) | Type::MutRef(inner) => matches!(*inner, Type::Dyn { .. }),
            _ => false,
        };
        dyn_slot(expected)
            && !dyn_slot(ty)
            && !matches!(ty, Type::Error | Type::Never)
            && matches!(
                expr.kind,
                ExprKind::If { .. } | ExprKind::Match { .. } | ExprKind::Block(_)
            )
            && !self
                .dyn_coercion_spans
                .contains_key(&(self.current_impl_span, SpanKey::from_span(&expr.span)))
    }

    /// Why `concrete` cannot become `dyn trait_name[args, assoc]`, or
    /// `None` when it can.
    /// v2 core: a collection's `.clone()` (and `to_vec`) clones each element,
    /// so the element type needs `.clone()` too (design.md § Derive). A local
    /// type that lacks it gets a derive; a `dyn` element needs its trait to
    /// have `Clone` as a supertrait, or its table has no `clone` entry.
    pub(super) fn require_elements_clone(&mut self, what: &str, self_ty: &Type, span: &Span) {
        if !self.cli_lint_overrides.strict_core {
            return;
        }
        let shown = type_display(self_ty);
        // `type_supports_clone` lets any type parameter through, so a
        // `Vec[T]` of an unbounded `T` cloned field by field: a `Mutex`
        // duplicated, a struct with no `Clone` copied (B-2026-11-27-7, the
        // remainder of B-2026-11-25-48). Only a parameter of an enclosing
        // item counts, as in `unbounded_param_within`.
        if let Some(p) = self.unbounded_clone_param_in_elements(self_ty) {
            self.missing_param_bound(
                format!(
                    "`{what}` on '{shown}' clones each element, and the type parameter `{p}` \
                     has no `Clone` bound: declare it `{p}: Clone`"
                ),
                &p,
                "Clone",
                span,
            );
            return;
        }
        if self.type_supports_clone(self_ty) {
            return;
        }
        if let Some((name, fix)) = self.copy_derive_fix_named(self_ty) {
            self.type_error_with_fix_it(
                format!(
                    "`{what}` on '{shown}' clones each element, and '{name}' has no \
                     `.clone()`: add `{}` to it",
                    fix.replacement.trim()
                ),
                *span,
                TypeErrorKind::TraitBoundNotSatisfied,
                fix,
            );
        } else if let Some(trait_name) = self.uncloneable_dyn_within(self_ty) {
            self.type_error(
                format!(
                    "`{what}` on '{shown}' clones each element, and `dyn {trait_name}` has no \
                     `.clone()`: give `{trait_name}` a `Clone` supertrait (`trait \
                     {trait_name}: Clone`)"
                ),
                *span,
                TypeErrorKind::TraitBoundNotSatisfied,
            );
        } else if let Some(f) = fn_within(self_ty) {
            // A function value is move-only (design.md § Escaping function
            // values): only a closure literal that is `Copy` copies, and a
            // value of an erased `Fn` type has no `.clone()`
            // (B-2026-11-03-3).
            let build = if what == ".to_vec()" {
                "build the `Vec` with a list literal `[f, g]` or `push` instead, or "
            } else {
                ""
            };
            self.type_error(
                format!(
                    "`{what}` on '{shown}' clones each element, and a value of function \
                     type '{}' is move-only and has no `.clone()` (design.md § Escaping \
                     function values): {build}wrap the function in a `shared struct` to \
                     give it many holders",
                    type_display(f)
                ),
                *span,
                TypeErrorKind::TraitBoundNotSatisfied,
            );
        }
    }

    /// `what` copies one value of `elem` into each new slot (`resize`,
    /// `fill`, `Vec.filled`, `extend_from_slice`, `[v; n]`), so `elem`
    /// needs `.clone()`: each is `where T: Clone` (collections.md,
    /// design.md § Collection literals), and `Clone` is never implied
    /// (design.md § Derive). The bound went unchecked, so a struct with
    /// no `Clone`, an unbounded `T` and a `Mutex` each got field-by-field
    /// copies (B-2026-11-25-48). A `Copy` value copies.
    pub(super) fn require_value_clone(&mut self, what: &str, elem: &Type, span: &Span) {
        if !self.cli_lint_overrides.strict_core {
            return;
        }
        let elem = self.resolved_now(elem);
        if self.value_clones(&elem) {
            return;
        }
        let shown = type_display(&elem);
        if let Some(p) = self.unbounded_clone_param_within(&elem) {
            self.missing_param_bound(
                format!(
                    "`{what}` copies its value into each new slot, so '{shown}' needs \
                     `.clone()`, and the type parameter `{p}` has no `Clone` bound: declare \
                     it `{p}: Clone`"
                ),
                &p,
                "Clone",
                span,
            );
        } else if let Some((name, fix)) = self.copy_derive_fix_named(&elem) {
            self.type_error_with_fix_it(
                format!(
                    "`{what}` copies its value into each new slot, and '{name}' has no \
                     `.clone()`: add `{}` to it",
                    fix.replacement.trim()
                ),
                *span,
                TypeErrorKind::TraitBoundNotSatisfied,
                fix,
            );
        } else {
            self.type_error(
                format!(
                    "`{what}` copies its value into each new slot, and '{shown}' does not \
                     implement `Clone`: build each element on its own (a loop that \
                     `push`es a new value)"
                ),
                *span,
                TypeErrorKind::TraitBoundNotSatisfied,
            );
        }
    }

    /// The first enclosing type parameter with no `Clone` bound among the
    /// elements a collection's `.clone()` / `to_vec` clones: through
    /// tuples, arrays, `Option`, `Result` and the library collections, not
    /// behind a view (a view copies) and not into a user or `shared` type,
    /// whose own `Clone` decides.
    fn unbounded_clone_param_in_elements(&self, ty: &Type) -> Option<String> {
        match ty {
            Type::TypeParam(p) => (self.enclosing_bounds.contains_key(p)
                && !self.type_param_satisfies_bound(p, "Clone"))
            .then(|| p.clone()),
            Type::Refinement { base, .. } => self.unbounded_clone_param_in_elements(base),
            Type::Tuple(elems) => elems
                .iter()
                .find_map(|e| self.unbounded_clone_param_in_elements(e)),
            Type::Array { element, .. } | Type::Vector { element, .. } => {
                self.unbounded_clone_param_in_elements(element)
            }
            Type::Named { name, args }
                if matches!(
                    name.as_str(),
                    "Option"
                        | "Result"
                        | "Vec"
                        | "VecDeque"
                        | "Slice"
                        | "Map"
                        | "SortedMap"
                        | "Set"
                        | "SortedSet"
                ) =>
            {
                super::types::element_args(name, args)
                    .iter()
                    .find_map(|a| self.unbounded_clone_param_in_elements(a))
            }
            _ => None,
        }
    }

    /// Records where each type parameter of `generics` is declared, for
    /// [`Self::param_bound_fix`].
    pub(super) fn note_param_sites(&mut self, generics: &Option<crate::ast::GenericParams>) {
        if let Some(g) = generics {
            for p in &g.params {
                if p.is_const || p.is_variadic_shape || p.default.is_some() {
                    self.enclosing_param_sites.remove(&p.name);
                    continue;
                }
                self.enclosing_param_sites
                    .insert(p.name.clone(), (p.span, !p.bounds.is_empty()));
            }
        }
    }

    /// A fix-it adding `trait_name` to the declaration of the enclosing
    /// type parameter `p`: `T` becomes `T: Clone`, `T: Ord` becomes
    /// `T: Ord + Clone`. `None` when `p` has no recorded declaration.
    pub(super) fn param_bound_fix(&self, p: &str, trait_name: &str) -> Option<super::FixIt> {
        let (site, bounded) = self.enclosing_param_sites.get(p)?;
        if site.length == 0 {
            return None;
        }
        Some(super::FixIt {
            span: Span {
                offset: site.offset + site.length,
                length: 0,
                line: site.line,
                column: site.column + site.length,
            },
            replacement: if *bounded {
                format!(" + {trait_name}")
            } else {
                format!(": {trait_name}")
            },
        })
    }

    /// Reports that the enclosing type parameter `p` lacks `trait_name`,
    /// with the fix-it that declares it when the declaration is known.
    fn missing_param_bound(&mut self, message: String, p: &str, trait_name: &str, span: &Span) {
        match self.param_bound_fix(p, trait_name) {
            Some(fix) => self.type_error_with_fix_it(
                message,
                *span,
                TypeErrorKind::TraitBoundNotSatisfied,
                fix,
            ),
            None => self.type_error(message, *span, TypeErrorKind::TraitBoundNotSatisfied),
        }
    }

    /// Whether a value of `ty` copies or clones, with a type parameter
    /// held to its declared bounds (`type_supports_clone` lets any `T`
    /// through) and a view copying as the `Copy` value it is.
    fn value_clones(&self, ty: &Type) -> bool {
        match ty {
            Type::TypeParam(p) => {
                !self.enclosing_bounds.contains_key(p)
                    || self.type_param_satisfies_bound(p, "Clone")
            }
            Type::Ref(_) => true,
            Type::Refinement { base, .. } => self.value_clones(base),
            Type::Tuple(elems) => elems.iter().all(|e| self.value_clones(e)),
            Type::Array { element, .. } | Type::Vector { element, .. } => {
                self.value_clones(element)
            }
            Type::Named { name, args }
                if matches!(
                    name.as_str(),
                    "Option"
                        | "Result"
                        | "Vec"
                        | "VecDeque"
                        | "Map"
                        | "SortedMap"
                        | "Set"
                        | "SortedSet"
                ) =>
            {
                super::types::element_args(name, args)
                    .iter()
                    .all(|a| self.value_clones(a))
            }
            _ => self.is_type_copy(ty) || self.type_supports_clone(ty),
        }
    }

    /// The first type parameter inside `ty` with no `Clone` bound.
    fn unbounded_clone_param_within(&self, ty: &Type) -> Option<String> {
        self.unbounded_param_within(ty, "Clone")
    }

    /// The first type parameter inside `ty` (not behind a view) that
    /// carries no `trait_name` bound.
    pub(super) fn unbounded_param_within(&self, ty: &Type, trait_name: &str) -> Option<String> {
        match ty {
            // Only a parameter of an enclosing item: a library signature's
            // own `T` can reach here unsolved, and is not the caller's.
            Type::TypeParam(p) => (self.enclosing_bounds.contains_key(p)
                && !self.type_param_satisfies_bound(p, trait_name))
            .then(|| p.clone()),
            Type::Named { args, .. } | Type::Tuple(args) => args
                .iter()
                .find_map(|a| self.unbounded_param_within(a, trait_name)),
            Type::Array { element, .. }
            | Type::Vector { element, .. }
            | Type::Refinement { base: element, .. } => {
                self.unbounded_param_within(element, trait_name)
            }
            _ => None,
        }
    }

    /// `dedup` compares neighbouring elements with `==`, so its element
    /// needs `PartialEq` (collections.md, `where T: PartialEq`), which went
    /// unchecked (B-2026-11-25-48).
    pub(super) fn require_value_eq(&mut self, what: &str, elem: &Type, span: &Span) {
        if !self.cli_lint_overrides.strict_core {
            return;
        }
        let elem = self.resolved_now(elem);
        let shown = type_display(&elem);
        if let Some(p) = self.unbounded_param_within(&elem, "PartialEq") {
            self.missing_param_bound(
                format!(
                    "`{what}` compares elements with `==`, so '{shown}' needs `PartialEq`, \
                     and the type parameter `{p}` has no `PartialEq` bound: declare it \
                     `{p}: PartialEq`"
                ),
                &p,
                "PartialEq",
                span,
            );
        } else if !self.type_supports_partial_eq(&elem) {
            self.type_error(
                format!(
                    "`{what}` compares elements with `==`, and '{shown}' does not implement \
                     PartialEq: add `#[derive(PartialEq)]` to it"
                ),
                *span,
                TypeErrorKind::TraitBoundNotSatisfied,
            );
        }
    }

    /// The trait of the first `dyn` inside `ty` whose values do not clone.
    fn uncloneable_dyn_within(&self, ty: &Type) -> Option<String> {
        match ty {
            Type::Dyn { trait_name, .. } => {
                (!self.type_supports_clone(ty)).then(|| trait_name.clone())
            }
            Type::Named { args, .. } | Type::Tuple(args) => {
                args.iter().find_map(|a| self.uncloneable_dyn_within(a))
            }
            Type::Array { element, .. } | Type::Ref(element) | Type::MutRef(element) => {
                self.uncloneable_dyn_within(element)
            }
            _ => None,
        }
    }

    /// `concrete` with its open type variables fixed by the one impl of
    /// `trait_name` for its type whose target is concrete, when it has
    /// open variables and exactly one such impl and no generic one.
    fn open_dyn_value_from_impl(&mut self, concrete: &Type, trait_name: &str) -> Option<Type> {
        let Type::Named { name, args } = concrete else {
            return None;
        };
        if !args.iter().any(super::exprs::contains_type_var) {
            return None;
        }
        let impls: Vec<&super::env::ImplInfo> = self
            .env
            .impls
            .iter()
            .filter(|imp| &imp.target_type == name && imp.trait_name.as_deref() == Some(trait_name))
            .collect();
        let [imp] = impls.as_slice() else {
            return None;
        };
        if imp.target_args.len() != args.len() {
            return None;
        }
        let target = Type::Named {
            name: name.clone(),
            args: imp.target_args.clone(),
        };
        super::inference::unify_types(
            concrete,
            &target,
            &mut self.env.substitutions,
            &mut self.env.const_substitutions,
        )
        .then_some(target)
    }

    fn dyn_unfit(
        &mut self,
        concrete: &Type,
        trait_name: &str,
        args: &[Type],
        assoc: &[(String, Type)],
    ) -> Option<String> {
        let shown = type_display(concrete);
        // A type parameter fits through its bounds: each instance builds the
        // table of the type it is given.
        if let Some(name) = self.type_param_name(concrete).cloned() {
            let bounded = self.enclosing_bounds.get(&name).is_some_and(|bs| {
                bs.iter().any(|b| {
                    b.path.last().is_some_and(|t| {
                        self.env
                            .supertrait_closure_traits(t)
                            .iter()
                            .any(|s| s == trait_name)
                    })
                })
            });
            if !bounded {
                return Some(format!(
                    "`{shown}` does not implement `{trait_name}`; bound it `[{shown}: {trait_name}]`"
                ));
            }
            // Trait arguments are read off a bound on the trait itself
            // (`[H: Handler[T]]`); a supertrait's are not written there.
            if !args.is_empty() {
                let scope: Vec<String> = self.enclosing_bounds.keys().cloned().collect();
                let written: Vec<Vec<GenericArg>> = self
                    .enclosing_bounds
                    .get(&name)
                    .into_iter()
                    .flatten()
                    .filter(|b| b.path.last().is_some_and(|t| t == trait_name))
                    .filter_map(|b| b.generic_args.clone())
                    .collect();
                let mut agree = false;
                for gargs in written {
                    let have: Vec<Type> = gargs
                        .iter()
                        .filter_map(|g| match g {
                            GenericArg::Type(te) => Some(self.lower_type_expr(te, &scope)),
                            _ => None,
                        })
                        .collect();
                    // A type parameter matches only itself here:
                    // `types_compatible` lets it stand for anything.
                    let same = |h: &Type, w: &Type| {
                        if contains_type_param(h) || contains_type_param(w) {
                            h == w
                        } else {
                            types_compatible(h, w) && types_compatible(w, h)
                        }
                    };
                    if have.len() == args.len() && have.iter().zip(args).all(|(h, w)| same(h, w)) {
                        agree = true;
                        break;
                    }
                }
                if !agree {
                    let want: Vec<String> = args.iter().map(type_display).collect();
                    return Some(format!(
                        "no bound of `{shown}` gives `{trait_name}[{}]`; bound it `[{shown}: \
                         {trait_name}[{}]]`",
                        want.join(", "),
                        want.join(", ")
                    ));
                }
            }
            for (assoc, promised) in assoc {
                let projection = Type::AssocProjection {
                    param: name.clone(),
                    assoc: assoc.clone(),
                    args: Vec::new(),
                    receiver_args: Vec::new(),
                    trait_name: None,
                };
                let actual = self.normalize_enclosing_assoc_eqs(&projection);
                let actual = self.resolve_assoc_projections(&actual);
                if matches!(actual, Type::AssocProjection { .. } | Type::Error) {
                    return Some(format!(
                        "`{shown}.{assoc}` is not fixed; bound it `[{shown}: \
                         {trait_name}[{assoc} = {}]]`",
                        type_display(promised)
                    ));
                }
                if !types_compatible(promised, &actual) {
                    return Some(format!(
                        "`{shown}` implements `{trait_name}` with `{assoc} = {}`, not `{}`",
                        type_display(&actual),
                        type_display(promised),
                    ));
                }
            }
            return None;
        }
        if !self.type_satisfies_bound(concrete, trait_name) {
            return Some(format!("`{shown}` does not implement `{trait_name}`"));
        }
        if !args.is_empty() {
            // Any of the type's impls of the trait may be the one at the
            // `dyn` type's arguments (`impl Cv[Str]` and `impl Cv[i64]`
            // both, B-2026-11-20-4).
            let impls: Vec<Vec<Type>> = self
                .env
                .impls_concrete_trait_args(concrete, trait_name)
                .into_iter()
                .filter(|a| !a.is_empty())
                .collect();
            let agrees = |have: &Vec<Type>| {
                have.len() == args.len()
                    && have
                        .iter()
                        .zip(args)
                        .all(|(h, w)| types_compatible(h, w) && types_compatible(w, h))
            };
            if !impls.is_empty() && !impls.iter().any(agrees) {
                let have: Vec<String> = impls
                    .iter()
                    .map(|a| {
                        let a: Vec<String> = a.iter().map(type_display).collect();
                        format!("`{trait_name}[{}]`", a.join(", "))
                    })
                    .collect();
                return Some(format!("`{shown}` implements {}", have.join(" and ")));
            }
        }
        self.settle_existential_literals(concrete, assoc);
        if let Some((name, promised, actual)) = self.existential_binding_mismatch(concrete, assoc) {
            return Some(format!(
                "`{shown}` implements `{trait_name}` with `{name} = {}`, not `{}`",
                type_display(&actual),
                type_display(&promised),
            ));
        }
        None
    }

    /// `x.method(..)` where `x` is a `dyn Trait` or a view of one: the
    /// trait's (or a supertrait's) method, with `Self` the `dyn` type and
    /// its associated types the bound ones. `None` when the receiver is no
    /// `dyn` value.
    pub(super) fn try_dyn_receiver_method(
        &mut self,
        receiver: &Type,
        method: &str,
        args: &[CallArg],
        span: &Span,
    ) -> Option<Type> {
        let mut base = receiver;
        while let Type::Ref(inner) | Type::MutRef(inner) = base {
            base = inner;
        }
        let Type::Dyn {
            trait_name,
            args: trait_args,
            assoc,
            ..
        } = base
        else {
            return None;
        };
        let dyn_ty = base.clone();
        let (trait_name, trait_args, assoc) =
            (trait_name.clone(), trait_args.clone(), assoc.clone());
        // A `dyn` of a trait under `Clone` is `Clone`: its `clone` copies the
        // concrete value into a new block, of the same `dyn` type (design.md
        // § Which traits can be used as `dyn`). `clone` returns `Self`, so it
        // has no slot of its own.
        if method == "clone"
            && args.is_empty()
            && self
                .env
                .supertrait_closure_traits(&trait_name)
                .iter()
                .any(|t| t == "Clone")
        {
            return Some(dyn_ty);
        }
        let declared = self
            .env
            .supertrait_closure_traits(&trait_name)
            .into_iter()
            .find_map(|t| {
                let m = self.find_trait_method(&t, method)?;
                m.self_param.as_ref()?;
                Some((t, m.clone()))
            });
        let Some((owner, m)) = declared else {
            for arg in args {
                self.infer_expr(&arg.value);
            }
            self.type_error(
                format!(
                    "no method '{method}' on `{}`: a `dyn` value has the methods of \
                     `{trait_name}` and its supertraits only",
                    type_display(&dyn_ty)
                ),
                *span,
                TypeErrorKind::NoMethodFound,
            );
            return Some(Type::Error);
        };
        // A default method with no slot runs its body with `Self` the `dyn`
        // type, its calls to the slots dispatching (design.md § Which traits
        // can be used as `dyn`). A required one with no slot makes the `dyn`
        // type itself an error (`E_DYN_NOT_DISPATCHABLE`), so this is only
        // the backstop for a type that slipped past that check.
        if !m.is_dyn_dispatchable() && m.body.is_none() {
            for arg in args {
                self.infer_expr(&arg.value);
            }
            self.type_error(
                format!(
                    "error[E_DYN_NOT_DISPATCHABLE]: `{method}` cannot be called through \
                     `{}`: it has no slot in the method table (it is generic or names \
                     `Self`) and no default body; call it on the concrete type, or \
                     through a generic parameter `[T: {owner}]`",
                    type_display(&dyn_ty)
                ),
                *span,
                TypeErrorKind::NoMethodFound,
            );
            return Some(Type::Error);
        }
        let callee = format!("{owner}.{method}");
        let key = SpanKey::from_span(span);
        if let Some((node, _)) = self.current_call_node.filter(|(_, k)| *k == key) {
            if !node.is_dummy() {
                self.node_method_callees.insert(node, callee.clone());
            }
        }
        // The ownership check reads the trait method's parameter modes here,
        // as for a call on a bounded type parameter.
        self.method_typeparam_trait_key.insert(key, callee);
        let trait_subs: Vec<(String, Type)> = if owner == trait_name {
            self.env
                .traits
                .get(&trait_name)
                .map(|t| t.generic_param_names.clone())
                .unwrap_or_default()
                .into_iter()
                .zip(trait_args)
                .collect()
        } else {
            Vec::new()
        };
        // `Self` stands for the `dyn` value as a type parameter no program
        // can name, whose associated types are the `dyn` type's bindings
        // for the call (a method's bound reads them too: `map`'s
        // `MutFn(own Self.Item) -> B`).
        let stand_in = format!("dyn {trait_name}");
        let saved: Vec<_> = assoc
            .iter()
            .map(|(n, t)| {
                let key = (stand_in.clone(), n.clone());
                let entry = super::env::ImplAssocTypeEntry {
                    ty: t.clone(),
                    gat_params: Vec::new(),
                    param_bound_traits: Vec::new(),
                    where_clause: None,
                };
                (key.clone(), self.env.impl_assoc_types.insert(key, entry))
            })
            .collect();
        let ret = self.dispatch_trait_method_on(
            Type::TypeParam(stand_in.clone()),
            &m,
            &trait_subs,
            None,
            Some(BoundReceiver {
                key: &stand_in,
                trait_name: &owner,
                bindings: &assoc,
            }),
            args,
            span,
        );
        for (key, old) in saved {
            match old {
                Some(e) => self.env.impl_assoc_types.insert(key, e),
                None => self.env.impl_assoc_types.remove(&key),
            };
        }
        let ret = substitute_existential_assoc_bindings(&ret, &stand_in, &assoc);
        // `Self` in the signature is the `dyn` type itself (`Clone.clone`).
        let mut self_sub = std::collections::HashMap::new();
        self_sub.insert(stand_in, SubstValue::Type(dyn_ty));
        Some(substitute_type_params(&ret, &self_sub))
    }

    /// The coercion sites by node, for `TypeCheckResult::node_dyn_coercions`.
    /// A site was recorded by its span and the impl block being checked: a
    /// default body copied into several impls keeps the trait's spans, and
    /// each copy coerces its own value (B-2026-10-28-12). Of the
    /// expressions with that span in that block (a method call shares its
    /// receiver's), the one typed as the coerced value is the site, else
    /// the outermost.
    pub(super) fn dyn_coercion_nodes(
        &self,
    ) -> rustc_hash::FxHashMap<crate::ids::NodeId, (Type, Type)> {
        let mut out = rustc_hash::FxHashMap::default();
        if self.dyn_coercion_spans.is_empty() {
            return out;
        }
        let mut by_span: rustc_hash::FxHashMap<CoercionKey, Vec<crate::ids::NodeId>> =
            rustc_hash::FxHashMap::default();
        for item in &self.program.items {
            let block = match item {
                Item::ImplBlock(imp) => Some(SpanKey::from_span(&imp.span)),
                _ => None,
            };
            let mut copy = item.clone();
            crate::span_visitor::walk_item_mut_with(
                &mut copy,
                &mut ExprsBySpan {
                    block,
                    out: &mut by_span,
                },
            );
        }
        for (key, (concrete, target)) in &self.dyn_coercion_spans {
            let Some(ids) = by_span.get(key) else {
                continue;
            };
            let typed = ids.iter().find(|id| {
                self.node_types
                    .get(id)
                    .is_some_and(|(t, _)| types_compatible(t, concrete))
            });
            if let Some(&id) = typed.or(ids.first()) {
                out.insert(id, (concrete.clone(), target.clone()));
            }
        }
        out
    }
}

/// A coercion site's impl block (none outside one) and span.
pub(super) type CoercionKey = (Option<SpanKey>, SpanKey);

/// Every expression's id under its impl block and span, outermost first.
struct ExprsBySpan<'m> {
    block: Option<SpanKey>,
    out: &'m mut rustc_hash::FxHashMap<CoercionKey, Vec<crate::ids::NodeId>>,
}

impl crate::span_visitor::MutVisitor for ExprsBySpan<'_> {
    fn span(&mut self, _: &mut Span) {}
    fn expr(&mut self, e: &mut Expr) {
        if !e.id.is_dummy() {
            self.out
                .entry((self.block, SpanKey::from_span(&e.span)))
                .or_default()
                .push(e.id);
        }
    }
}

/// The first function type inside `ty`, whose values do not clone.
pub(super) fn fn_within(ty: &Type) -> Option<&Type> {
    match ty {
        Type::Function { .. } | Type::OnceFunction { .. } => Some(ty),
        Type::Named { args, .. } | Type::Tuple(args) => args.iter().find_map(fn_within),
        Type::Array { element, .. } | Type::Ref(element) | Type::MutRef(element) => {
            fn_within(element)
        }
        _ => None,
    }
}
