//! B-2026-10-27-33: a generic item that sends or spawns a value of its own
//! type parameter must itself require `T: CrossTask` (design.md §
//! `CrossTask`: "The error is reported at that generic signature, not after
//! monomorphization"). The walker in `cross_task_safe.rs` accepts a type
//! parameter, since nothing about it is known there; each boundary site
//! asks this check of the parameters its value's type names: a task's
//! captures and its result, a channel send, and a provider that may be
//! shared across tasks.

use super::types::{type_display, Type};
use super::{FixIt, TypeErrorKind};
use crate::resolver::SpanKey;
use crate::token::Span;

impl<'a> super::TypeChecker<'a> {
    /// Refuse each type parameter of the enclosing item that `ty` names and
    /// whose declaration does not bound it by `CrossTask`, once per
    /// declaration. `descr` names the value at the site.
    pub(super) fn check_cross_task_params(&mut self, ty: &Type, descr: &str, span: &Span) {
        let mut params = Vec::new();
        self.key_params(ty, &mut params);
        params.dedup();
        for p in params {
            if !self.enclosing_bounds.contains_key(&p)
                || self.type_param_satisfies_bound(&p, "CrossTask")
            {
                continue;
            }
            let decl = self.param_decl_span(span.offset, &p).unwrap_or(*span);
            if !self
                .reported_cross_task_params
                .insert(SpanKey::from_span(&decl))
            {
                continue;
            }
            let message = format!(
                "error[E_NOT_CROSS_TASK]: {descr} (type `{}`) cannot cross a task boundary\n\
                 note: type parameter `{p}` is not bounded by `CrossTask`, so a caller may give \
                 it a `shared` value\n\
                 help: declare `{p}: CrossTask` where `{p}` is declared",
                type_display(ty)
            );
            match self.param_bound_edit(span.offset, &p, "CrossTask") {
                Some(edit) => self.type_error_with_fix_it(
                    message,
                    *span,
                    TypeErrorKind::CrossTaskUnsafeCapture,
                    FixIt {
                        span: Span {
                            offset: edit.offset,
                            length: 0,
                            line: decl.line,
                            column: decl.column,
                        },
                        replacement: edit.replacement,
                    },
                ),
                None => self.type_error(message, *span, TypeErrorKind::CrossTaskUnsafeCapture),
            }
        }
    }
}
