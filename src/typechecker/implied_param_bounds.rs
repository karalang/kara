//! B-2026-11-18-25: a generic body that compares (`==`, `<`) or interpolates
//! (`f"{x}"`) a value of a type parameter with no bound giving the trait is
//! a `deprecated` warning at the body in 0.8.x ([`Self::report_param_lacks_bound`]).
//! What makes it unsound is an instantiation whose type lacks the trait: a
//! struct with no `PartialEq` compared by its shape, a type with no
//! `Display` printed as its fields, a `Secret` whose `Display` is withheld
//! on purpose printed through a generic interpolation. That instantiation is
//! an error now, at the call: every call of such a function is checked
//! against the bounds its body implies, and a generic caller that hands its
//! own parameter on takes the requirement over (design.md §8 Bounds).

use super::types::{type_display, Type};
use super::TypeErrorKind;
use crate::ast::{Function, GenericParams, ImplItem, Item, TypeKind, WhereClause};
use crate::token::Span;
use rustc_hash::FxHashMap;

/// A requirement a body implies: its parameter, the trait, and what in the
/// body needs it (an operator or an interpolation, and where).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ImpliedBound {
    pub param: String,
    pub trait_name: String,
    pub what: String,
    pub at: Span,
}

/// A call of a function, from inside another one.
struct Site {
    caller: String,
    callee: String,
    span: Span,
    subs: FxHashMap<String, Type>,
}

impl<'a> super::TypeChecker<'a> {
    /// Note that the body being checked needs `param: trait_name` for `what`
    /// at `at`. Only a program's own functions are noted: the library's
    /// generics are checked against their declared bounds alone.
    pub(super) fn note_implied_bound(
        &mut self,
        param: &str,
        trait_name: &str,
        what: &str,
        at: Span,
    ) {
        let library = self.program.library_start.unwrap_or(usize::MAX);
        if at.offset >= library {
            return;
        }
        let Some(key) = self.current_fn_core_key.clone() else {
            return;
        };
        let entry = self.implied_param_bounds.entry(key).or_default();
        let b = ImpliedBound {
            param: param.to_string(),
            trait_name: trait_name.to_string(),
            what: what.to_string(),
            at,
        };
        if !entry
            .iter()
            .any(|e| e.param == b.param && e.trait_name == b.trait_name)
        {
            entry.push(b);
        }
    }

    /// Check every call of a function whose body implies a bound against
    /// the type the call gives the parameter.
    pub(super) fn check_implied_param_bounds(&mut self) {
        if self.implied_param_bounds.is_empty() {
            return;
        }
        let library = self.program.library_start.unwrap_or(usize::MAX);
        let mut sites: Vec<Site> = Vec::new();
        // Each caller's own parameters and the traits their bounds name.
        let mut caller_bounds: FxHashMap<String, FxHashMap<String, Vec<String>>> =
            FxHashMap::default();
        let program = self.program;
        for item in &program.items {
            let mut fns: Vec<(
                String,
                &Function,
                Option<&GenericParams>,
                Option<&WhereClause>,
            )> = Vec::new();
            match item {
                Item::Function(f) if f.span.offset < library => {
                    fns.push((f.name.clone(), f, None, None));
                }
                Item::ImplBlock(imp) if imp.span.offset < library => {
                    let TypeKind::Path(p) = &imp.target_type.kind else {
                        continue;
                    };
                    let Some(target) = p.segments.last() else {
                        continue;
                    };
                    for it in &imp.items {
                        if let ImplItem::Method(f) = it {
                            fns.push((
                                format!("{target}.{}", f.name),
                                f,
                                imp.generic_params.as_ref(),
                                imp.where_clause.as_ref(),
                            ));
                        }
                    }
                }
                _ => continue,
            }
            for (key, f, outer_generics, outer_where) in fns {
                let mut bounds =
                    Self::bounds_by_param(f.generic_params.as_ref(), f.where_clause.as_ref());
                for (p, b) in Self::bounds_by_param(outer_generics, outer_where) {
                    bounds.entry(p).or_default().extend(b);
                }
                caller_bounds.insert(key.clone(), bounds.into_iter().collect());
                crate::span_visitor::for_each_expr_in_block(&f.body, &mut |e| {
                    let callee = match &e.kind {
                        crate::ast::ExprKind::Call { callee, .. } => match &callee.kind {
                            crate::ast::ExprKind::Identifier(n) => Some(n.clone()),
                            crate::ast::ExprKind::Path { segments, .. } if segments.len() == 2 => {
                                Some(segments.join("."))
                            }
                            _ => None,
                        },
                        crate::ast::ExprKind::MethodCall { .. } => self
                            .method_callee_types
                            .get(&crate::resolver::SpanKey::from_span(&e.span))
                            .cloned(),
                        _ => None,
                    };
                    let Some(callee) = callee else {
                        return;
                    };
                    let Some(subs) = self.node_call_subs.get(&e.id) else {
                        return;
                    };
                    sites.push(Site {
                        caller: key.clone(),
                        callee,
                        span: e.span,
                        subs: subs.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
                    });
                });
            }
        }
        // A caller that gives the callee its own parameter, with no bound
        // of its own that provides the trait, needs it too.
        loop {
            let mut added: Vec<(String, ImpliedBound)> = Vec::new();
            for s in &sites {
                let Some(reqs) = self.implied_param_bounds.get(&s.callee) else {
                    continue;
                };
                for r in reqs {
                    let Some(Type::TypeParam(q)) = s
                        .subs
                        .get(&r.param)
                        .map(|t| self.resolved_now(t))
                        .map(strip_refs)
                    else {
                        continue;
                    };
                    let Some(own) = caller_bounds.get(&s.caller).and_then(|b| b.get(&q)) else {
                        continue;
                    };
                    let provides = own.iter().any(|t| {
                        t == &r.trait_name
                            || (t == "Numeric" && r.trait_name != "Display")
                            || self
                                .env
                                .supertrait_closure_traits(t)
                                .iter()
                                .any(|st| st == &r.trait_name)
                    });
                    let known = self.implied_param_bounds.get(&s.caller).is_some_and(|v| {
                        v.iter()
                            .any(|e| e.param == q && e.trait_name == r.trait_name)
                    }) || added.iter().any(|(c, e)| {
                        c == &s.caller && e.param == q && e.trait_name == r.trait_name
                    });
                    if !provides && !known {
                        added.push((
                            s.caller.clone(),
                            ImpliedBound {
                                param: q.clone(),
                                trait_name: r.trait_name.clone(),
                                what: format!("its call of `{}`", s.callee),
                                at: s.span,
                            },
                        ));
                    }
                }
            }
            if added.is_empty() {
                break;
            }
            for (caller, b) in added {
                self.implied_param_bounds.entry(caller).or_default().push(b);
            }
        }
        for s in sites {
            let Some(reqs) = self.implied_param_bounds.get(&s.callee).cloned() else {
                continue;
            };
            for r in reqs {
                let Some(t) = s.subs.get(&r.param) else {
                    continue;
                };
                let t = strip_refs(self.resolved_now(t));
                if matches!(t, Type::TypeParam(_) | Type::TypeVar(_) | Type::Error) {
                    continue;
                }
                if self.type_satisfies_bound(&t, &r.trait_name) {
                    continue;
                }
                self.type_error(
                    format!(
                        "this call gives `{callee}`'s `{p}` the type `{ty}`, which does not \
                         implement `{tr}`, and `{callee}` uses {what} on `{p}` (line {line}), \
                         which needs `{p}: {tr}` (design.md §8). Implement or derive `{tr}` \
                         for `{ty}`, or pass a type that has it",
                        callee = s.callee,
                        p = r.param,
                        ty = type_display(&t),
                        tr = r.trait_name,
                        what = r.what,
                        line = r.at.line,
                    ),
                    s.span,
                    TypeErrorKind::TypeMismatch,
                );
            }
        }
    }
}

fn strip_refs(t: Type) -> Type {
    match t {
        Type::Ref(inner) | Type::MutRef(inner) => strip_refs(*inner),
        t => t,
    }
}
