//! `deprecated` on `s.slice(a, b)` over a `String` or a `Str`
//! (docs/library/strings.md, review-thread ruling 2026-11-25,
//! B-2026-11-25-63): the documented view is `s[a..b]` (design.md §5) and
//! the copy `substring`, so `slice` is a second spelling of the view. 0.8.x
//! warns, with the rewrite to `s[a..b]`; a later release removes it. A
//! `Vec`'s, an `Array`'s and a `Tensor`'s own `slice` are untouched.

use crate::ast::{CallArg, Expr};
use crate::token::Span;

use super::TypeChecker;

impl TypeChecker<'_> {
    pub(super) fn warn_string_slice(
        &mut self,
        object: &Expr,
        args: &[CallArg],
        obj_ty: &super::Type,
        close: &Span,
    ) {
        let mut ty = obj_ty;
        while let super::Type::Ref(t) | super::Type::MutRef(t) = ty {
            ty = t;
        }
        let is_text = match ty {
            super::Type::Str => true,
            super::Type::Named { name, .. } => {
                matches!(name.as_str(), "String" | "Str" | "StringSlice")
            }
            _ => false,
        };
        if self.compiling_stdlib || !is_text || !self.string_slice_reported.insert(close.offset) {
            return;
        }
        let from = object.span.offset + object.span.length;
        let end = close.offset + close.length;
        let recv = crate::formatter::render_expr_one_line(object);
        let lo = crate::formatter::render_expr_one_line(&args[0].value);
        let hi = crate::formatter::render_expr_one_line(&args[1].value);
        let fix_it = (end > from && close.length > 0 && args.iter().all(|a| a.label.is_none()))
            .then(|| super::FixIt {
                span: Span {
                    offset: from,
                    length: end - from,
                    ..object.span
                },
                replacement: format!("[{lo}..{hi}]"),
            });
        self.type_lint_warning_with_fix(
            format!(
                "use of deprecated `slice(a, b)` on a string: the view is spelled `s[a..b]` \
                 (`{recv}[{lo}..{hi}]` here), and the owned copy `substring`; `slice` is removed \
                 in a later release — suppress with `#[allow(deprecated)]` on the enclosing item"
            ),
            Span {
                length: end.saturating_sub(object.span.offset),
                ..object.span
            },
            super::TypeErrorKind::Deprecated,
            "deprecated",
            fix_it,
        );
    }
}
