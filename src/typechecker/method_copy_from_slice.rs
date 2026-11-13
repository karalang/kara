//! Rust's `dst.copy_from_slice(src)` on a `Vec`, `Array` or `Slice`. Kāra
//! spells the copy as a range assignment, `c[a..b] = src` for a `Copy`
//! element, and `clone_from_slice` for the rest (design.md § Range
//! indexing). The call used to be "no method 'copy_from_slice' on type
//! 'Slice'" with no fix, and the error type it left set off borrow errors
//! on every later use of the buffer (B-2026-11-13-15). It is now an error
//! whose machine-applicable fix writes the range assignment when the
//! receiver is a range index of a `Copy` element, and `clone_from_slice`
//! otherwise; the call is typed as the `()` either gives.

use super::types::Type;
use super::{FixIt, TypeChecker, TypeErrorKind};
use crate::ast::{CallArg, Expr, ExprKind};
use crate::token::Span;

/// The element type of a `Vec`, `Array` or `Slice` receiver, through a
/// borrow.
fn copy_target_elem(obj_ty: &Type) -> Option<Type> {
    let ty = match obj_ty {
        Type::Ref(inner) | Type::MutRef(inner) => inner.as_ref(),
        t => t,
    };
    match ty {
        Type::Named { name, args } if name == "Vec" && args.len() == 1 => Some(args[0].clone()),
        Type::Slice { element, .. } => Some((**element).clone()),
        Type::Array { element, .. } => Some((**element).clone()),
        _ => None,
    }
}

impl TypeChecker<'_> {
    /// `dst.copy_from_slice(src)`: the error naming Kāra's spelling, with
    /// its edit. `None` for any other call.
    pub(super) fn try_copy_from_slice(
        &mut self,
        object: &Expr,
        method: &str,
        args: &[CallArg],
        span: &Span,
        args_close_span: &Span,
        obj_ty: &Type,
    ) -> Option<Type> {
        if method != "copy_from_slice" || args.len() != 1 {
            return None;
        }
        let elem = copy_target_elem(obj_ty)?;
        // A program's own `copy_from_slice` (a trait it implements for a
        // sequence) is the one called.
        let own = self.env.impls.iter().any(|imp| {
            imp.trait_name.is_some()
                && matches!(imp.target_type.as_str(), "Vec" | "Slice" | "Array")
                && imp.methods.contains_key(method)
        });
        if own {
            return None;
        }
        self.infer_expr(&args[0].value);
        let ranged = matches!(
            &object.kind,
            ExprKind::Index { index, .. } if matches!(index.kind, ExprKind::Range { .. })
        );
        let copy = self.is_copy_type_during_check(&elem);
        let src = crate::formatter::render_expr(&args[0].value);
        let recv = crate::formatter::render_expr(object);
        let (edit, how) = if ranged && copy {
            (
                format!(" = {src}"),
                format!("a range assignment: `{recv} = {src}`"),
            )
        } else {
            (
                format!(".clone_from_slice({src})"),
                format!("`{recv}.clone_from_slice({src})`"),
            )
        };
        let start = object.span.offset + object.span.length;
        let end = args_close_span.offset + args_close_span.length;
        let fix = (end > start).then(|| FixIt {
            span: Span {
                offset: start,
                length: end - start,
                ..*args_close_span
            },
            replacement: edit,
        });
        let message = format!(
            "no method `copy_from_slice`: Rust's slice copy is {how} in Kāra \
             (design.md § Range indexing; a range assignment needs a `Copy` \
             element, and `clone_from_slice` copies any `Clone` one)"
        );
        match fix {
            Some(fix) => {
                self.type_error_with_fix_it(message, *span, TypeErrorKind::NoMethodFound, fix)
            }
            None => self.type_error(message, *span, TypeErrorKind::NoMethodFound),
        }
        Some(Type::Unit)
    }
}
