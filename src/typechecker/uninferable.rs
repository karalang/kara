//! A generic parameter no call can infer (review-thread ruling 2026-11-16,
//! design.md §8 Generic parameters and arguments).
//!
//! A call takes no type arguments, so a function's type parameter is
//! decided by its arguments and its expected result. One that appears in
//! no parameter type, not in the return type, and in no other parameter's
//! bound can never be decided, and every call of the function was refused
//! at the call. It is refused at the declaration instead, naming the
//! parameter.
//!
//! Lives in a sibling `impl<'a> super::TypeChecker<'a>` block.

use crate::ast::*;
use crate::token::Span;

use super::TypeErrorKind;

impl<'a> super::TypeChecker<'a> {
    /// Refuse every function, method and trait method of the program whose
    /// own type parameter nothing at a call can fix.
    pub(super) fn refuse_uninferable_type_params(&mut self) {
        if self.compiling_stdlib {
            return;
        }
        let library = self.program.library_start.unwrap_or(usize::MAX);
        let mut found: Vec<(String, String, Span)> = Vec::new();
        for item in &self.program.items {
            match item {
                Item::Function(f) if !f.stdlib_origin && !f.is_comptime => {
                    found.extend(uninferable(
                        &f.name,
                        f.generic_params.as_ref(),
                        &f.params,
                        f.return_type.as_ref(),
                        f.where_clause.as_ref(),
                    ));
                }
                // A trait impl's method has the trait's signature, which is
                // where it is reported.
                Item::ImplBlock(imp) if imp.trait_name.is_none() => {
                    for ii in &imp.items {
                        let ImplItem::Method(f) = ii else { continue };
                        if f.stdlib_origin || f.is_comptime {
                            continue;
                        }
                        found.extend(uninferable(
                            &f.name,
                            f.generic_params.as_ref(),
                            &f.params,
                            f.return_type.as_ref(),
                            f.where_clause.as_ref(),
                        ));
                    }
                }
                Item::TraitDef(t) if !t.stdlib_origin => {
                    for ti in &t.items {
                        let TraitItem::Method(m) = ti else { continue };
                        found.extend(uninferable(
                            &m.name,
                            m.generic_params.as_ref(),
                            &m.params,
                            m.return_type.as_ref(),
                            m.where_clause.as_ref(),
                        ));
                    }
                }
                _ => {}
            }
        }
        for (func, param, span) in found {
            if span.offset >= library {
                continue;
            }
            self.type_error(
                format!(
                    "type parameter `{param}` of `{func}` appears in no parameter type and not \
                     in the return type, so no call can decide it: a call takes no type \
                     arguments (design.md §8 Generic parameters and arguments). Name `{param}` \
                     in a parameter's type or the return type, or remove it"
                ),
                span,
                TypeErrorKind::CannotInferAssocFn,
            );
        }
    }
}

/// The type parameters of one signature that nothing at a call fixes, as
/// (function, parameter, the parameter's span).
fn uninferable(
    func: &str,
    generics: Option<&GenericParams>,
    params: &[Param],
    ret: Option<&TypeExpr>,
    where_clause: Option<&WhereClause>,
) -> Vec<(String, String, Span)> {
    let Some(g) = generics else {
        return Vec::new();
    };
    // A name is matched in the Debug rendering of a type or bound, where
    // every path segment is a quoted string: a false match only keeps a
    // parameter, never refuses one.
    let names = |dbg: &str, name: &str| dbg.contains(&format!("{name:?}"));
    let signature: Vec<String> = params
        .iter()
        .map(|p| format!("{:?}", p.ty))
        .chain(ret.map(|r| format!("{r:?}")))
        .collect();
    g.params
        .iter()
        .filter(|p| !p.is_const && p.const_type.is_none() && !p.is_variadic_shape)
        .filter(|p| {
            let n = p.name.as_str();
            if signature.iter().any(|s| names(s, n)) {
                return false;
            }
            let in_other_bound = g
                .params
                .iter()
                .filter(|o| o.name != p.name)
                .any(|o| o.bounds.iter().any(|b| names(&format!("{b:?}"), n)));
            let in_where = where_clause.is_some_and(|w| {
                w.constraints.iter().any(|c| match c {
                    WhereConstraint::TypeBound {
                        type_name, bounds, ..
                    } => type_name != n && bounds.iter().any(|b| names(&format!("{b:?}"), n)),
                    WhereConstraint::AssocTypeEq { type_name, ty, .. } => {
                        type_name != n && names(&format!("{ty:?}"), n)
                    }
                    other => names(&format!("{other:?}"), n),
                })
            });
            !in_other_bound && !in_where
        })
        .map(|p| (func.to_string(), p.name.clone(), p.span))
        .collect()
}
