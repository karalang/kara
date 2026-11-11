//! `concat()` on a `Vec` or `Slice` (design.md § Vec, ruling for
//! B-2026-11-11-51). A sequence of `Vec`s flattens, as Rust's does: a
//! `Vec[Vec[T]]` or `Slice[Vec[T]]` gives the `Vec[T]` of every element's
//! items in order; lowering routes the call to the library's
//! `Slice[Vec[T]].__concat_vecs`. A sequence of `String` or `Str` keeps the
//! library's text join. On any other element, `concat()` joined their
//! `Display` forms in 0.8.0; in 0.8.x that is a `deprecated` warning whose
//! machine-applicable fix writes `join("")`, and the next minor release
//! makes it an error (B-2026-11-11-81).

use super::types::Type;
use super::{FixIt, TypeChecker, TypeErrorKind};
use crate::ast::{CallArg, Expr};
use crate::token::Span;

/// The element type of a `Vec` or `Slice` receiver, through a borrow.
fn sequence_elem(obj_ty: &Type) -> Option<Type> {
    let ty = match obj_ty {
        Type::Ref(inner) | Type::MutRef(inner) => inner.as_ref(),
        t => t,
    };
    match ty {
        Type::Named { name, args } if name == "Vec" && args.len() == 1 => Some(args[0].clone()),
        Type::Slice { element, .. } => Some((**element).clone()),
        _ => None,
    }
}

/// The item type of a `Vec` element, through a borrow.
pub(crate) fn vec_items(elem: &Type) -> Option<Type> {
    let ty = match elem {
        Type::Ref(inner) | Type::MutRef(inner) => inner.as_ref(),
        t => t,
    };
    match ty {
        Type::Named { name, args } if name == "Vec" && args.len() == 1 => Some(args[0].clone()),
        _ => None,
    }
}

impl TypeChecker<'_> {
    /// `xs.concat()` over `Vec`s: the flattened `Vec`. Over elements that
    /// are neither sequences nor text, the deprecation of the `Display`
    /// join, after which the call goes on to the existing typing.
    pub(super) fn try_concat_of_sequences(
        &mut self,
        object: &Expr,
        method: &str,
        args: &[CallArg],
        span: &Span,
        args_close_span: &Span,
        obj_ty: &Type,
    ) -> Option<Type> {
        if method != "concat" || !args.is_empty() {
            return None;
        }
        let elem = sequence_elem(obj_ty)?;
        // A program's own `concat` on the receiver's type (a trait it
        // implements for `Vec`) is the one called.
        let own = self.env.impls.iter().any(|imp| {
            imp.trait_name.is_some()
                && matches!(imp.target_type.as_str(), "Vec" | "Slice")
                && imp.methods.contains_key(method)
        });
        if own {
            return None;
        }
        if let Some(items) = vec_items(&elem) {
            return Some(Type::Named {
                name: "Vec".to_string(),
                args: vec![items],
            });
        }
        let text = match &elem {
            Type::Ref(inner) | Type::MutRef(inner) => inner.as_ref(),
            t => t,
        };
        let stringy = matches!(text, Type::Str | Type::TypeVar(_) | Type::Error)
            || matches!(text, Type::Named { name, args } if name == "StringSlice" && args.is_empty());
        if stringy || !self.type_supports_display(text) {
            return None;
        }
        // `concat()` -> `join("")`, from the method name to the `)`.
        let fix = args_close_span
            .offset
            .checked_sub(1 + method.len())
            .filter(|&o| o > object.span.offset)
            .map(|offset| FixIt {
                span: Span {
                    offset,
                    length: args_close_span.offset + args_close_span.length - offset,
                    ..*args_close_span
                },
                replacement: "join(\"\")".to_string(),
            });
        self.type_lint_warning_with_fix(
            "`concat()` joining the elements' `Display` forms is deprecated: \
             `concat()` flattens a sequence of sequences, and a text join of other \
             elements is `join(\"\")`; it becomes an error at the next minor release \
             (design.md § Vec) — suppress with `#[allow(deprecated)]` on the \
             enclosing item"
                .to_string(),
            *span,
            TypeErrorKind::Deprecated,
            "deprecated",
            fix,
        );
        None
    }
}
