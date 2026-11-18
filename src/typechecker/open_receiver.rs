//! A method called on a closure parameter nothing has typed yet
//! (`let digits = |time| { time.chars().count() }; digits(a)`). The call
//! cannot be resolved while the body is checked, and a later call of the
//! closure that types the parameter comes too late for it, so the program
//! used to check with no diagnostic and fail in lowering with E0516 "a type
//! that failed to check is not lowered yet" (B-2026-11-17-144). It is reported
//! here, at the end of `check`, as the parameter's missing annotation, with
//! the type a call of the closure gave it as the machine fix.

use super::types::TypeVarId;
use super::{FixIt, TypeChecker, TypeErrorKind};
use crate::ast::*;
use crate::token::Span;
use crate::typechecker::{type_display, Type};

/// One method call on an open closure parameter.
#[derive(Debug, Clone)]
pub(crate) struct OpenReceiverCall {
    pub var: TypeVarId,
    pub name: String,
    pub receiver: Span,
    pub method: String,
}

impl TypeChecker<'_> {
    /// After a method call: a bare-name receiver whose type is still an open
    /// (non-literal) variable, and a call that resolved to nothing with no
    /// diagnostic of its own, is noted for [`Self::report_open_receiver_calls`].
    pub(super) fn note_open_receiver_call(
        &mut self,
        object: &Expr,
        method: &str,
        result: &Type,
        errors_before: usize,
    ) {
        if !matches!(result, Type::Error) || self.errors.len() > errors_before {
            return;
        }
        let ExprKind::Identifier(name) = &object.kind else {
            return;
        };
        let Some(t) = self
            .expr_types
            .get(&crate::resolver::SpanKey::from_span(&object.span))
        else {
            return;
        };
        let t = self.resolved_now(t);
        let t = match t {
            Type::Ref(inner) | Type::MutRef(inner) => *inner,
            other => other,
        };
        let Type::TypeVar(var) = t else {
            return;
        };
        if super::inference::literal_var_kind(var).is_some() {
            return;
        }
        self.open_receiver_calls.push(OpenReceiverCall {
            var,
            name: name.clone(),
            receiver: object.span,
            method: method.to_string(),
        });
    }

    /// One diagnostic per open parameter, at its first method call.
    pub(super) fn report_open_receiver_calls(&mut self) {
        let mut calls = std::mem::take(&mut self.open_receiver_calls);
        // Outermost first: a closure inside the body of one reported here
        // (`time.chars().filter(|c| c.is_ascii_digit())`) is open only
        // because that one is, and is not reported again.
        calls.sort_by_key(|c| c.receiver.offset);
        let mut reported: Vec<Span> = Vec::new();
        for call in calls {
            let inside = |s: &Span| {
                s.offset <= call.receiver.offset && call.receiver.offset < s.offset + s.length
            };
            if reported.iter().any(inside) {
                continue;
            }
            let found = self.closure_param_of(&call.name, &call.receiver);
            let param = found.map(|(p, _)| p);
            reported.push(found.map_or(call.receiver, |(_, closure)| closure));
            let solved = match self.resolved_now(&Type::TypeVar(call.var)) {
                Type::Ref(inner) | Type::MutRef(inner) => *inner,
                other => other,
            };
            let known = !matches!(solved, Type::TypeVar(_) | Type::Error);
            // Unsolved, with an error already reported: the parameter is
            // most likely open because of it (an adaptor refused on a
            // collection leaves its closure's item unknown), so say nothing.
            if !known && !self.errors.is_empty() {
                continue;
            }
            let name = &call.name;
            let method = &call.method;
            let message = if known {
                format!(
                    "cannot infer the type of closure parameter `{name}` where `.{method}()` is \
                     called on it: the body is checked before any call of the closure types it \
                     (design.md § Type inference). Annotate it: `|{name}: {}|`",
                    type_display(&solved)
                )
            } else {
                format!(
                    "cannot infer the type of closure parameter `{name}` where `.{method}()` is \
                     called on it: nothing types it before the call (design.md § Type \
                     inference). Annotate it: `|{name}: T|`"
                )
            };
            match (param, known) {
                (Some(at), true) => {
                    let fix = FixIt {
                        span: Span {
                            offset: at.offset + at.length,
                            length: 0,
                            line: at.line,
                            column: at.column + at.length,
                        },
                        replacement: format!(": {}", type_display(&solved)),
                    };
                    self.type_error_with_fix_it(
                        message,
                        call.receiver,
                        TypeErrorKind::CannotInferTypeParam,
                        fix,
                    );
                }
                _ => self.type_error(message, call.receiver, TypeErrorKind::CannotInferTypeParam),
            }
        }
    }

    /// The span of the unannotated closure parameter `name` whose body holds
    /// `at`, and of that closure: the innermost such closure.
    fn closure_param_of(&self, name: &str, at: &Span) -> Option<(Span, Span)> {
        let mut best: Option<(Span, Span)> = None;
        let inside = |s: &Span| s.offset <= at.offset && at.offset < s.offset + s.length;
        for item in &self.program.items {
            let bodies: Vec<&Block> = match item {
                Item::Function(f) if inside(&f.span) => vec![&f.body],
                Item::ImplBlock(imp) if inside(&imp.span) => imp
                    .items
                    .iter()
                    .filter_map(|it| match it {
                        ImplItem::Method(m) if inside(&m.span) => Some(&m.body),
                        _ => None,
                    })
                    .collect(),
                _ => continue,
            };
            for b in bodies {
                crate::span_visitor::for_each_expr_in_block(b, &mut |e| {
                    let ExprKind::Closure { params, body, .. } = &e.kind else {
                        return;
                    };
                    if !inside(&body.span) {
                        return;
                    }
                    let Some(p) = params.iter().find(|p| {
                        p.ty.is_none()
                            && matches!(&p.pattern.kind, PatternKind::Binding(n) if n == name)
                    }) else {
                        return;
                    };
                    if best.is_none_or(|(_, c)| e.span.length < c.length) {
                        best = Some((p.pattern.span, e.span));
                    }
                });
            }
        }
        best
    }
}
