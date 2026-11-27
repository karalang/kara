//! Rust's `set_read_timeout(Some(Duration::from_millis(n)))`, carried over
//! (B-2026-11-15-153).
//!
//! A Kāra socket's `set_read_timeout` takes `Option[i64]` milliseconds
//! (library/io.md § Timeouts), where Rust's takes `Option<Duration>`. The
//! `Duration::from_millis` path mends to `Duration.ms` first, which leaves
//! "expected 'i64', found 'Duration'" at the argument. The `Some(..)`
//! argument of a `set_read_timeout` call is noted when the call is
//! checked, with the edits that make it milliseconds: `Duration.ms(n)`
//! becomes `n`, `Duration.secs(n)` becomes `n * 1000`, and any other
//! `Duration` takes `.as_ms()`. The mismatch then carries them.

use crate::ast::{CallArg, Expr, ExprKind};
use crate::resolver::{SpanKey, TextEdit};

use super::TypeChecker;

/// `Duration.<unit>(n)` as a call: the unit and its one argument.
fn duration_ctor(e: &Expr) -> Option<(&str, &Expr)> {
    let ExprKind::Call { callee, args } = &e.kind else {
        return None;
    };
    let [arg] = args.as_slice() else {
        return None;
    };
    let unit = match &callee.kind {
        ExprKind::Path { segments, .. } if segments.len() == 2 && segments[0] == "Duration" => {
            segments[1].as_str()
        }
        ExprKind::FieldAccess { object, field } => match &object.kind {
            ExprKind::Identifier(t) if t == "Duration" => field.as_str(),
            _ => return None,
        },
        _ => return None,
    };
    (arg.label.is_none() && !arg.mut_marker).then_some((unit, &arg.value))
}

/// Whether `e` can take a postfix call or an infix operand without
/// parentheses.
fn is_atomic(e: &Expr) -> bool {
    matches!(
        e.kind,
        ExprKind::Identifier(_)
            | ExprKind::Integer(..)
            | ExprKind::FieldAccess { .. }
            | ExprKind::TupleIndex { .. }
            | ExprKind::Call { .. }
            | ExprKind::MethodCall { .. }
            | ExprKind::Index { .. }
            | ExprKind::SelfValue
    )
}

/// Where `e` ends in the source, when its span says so.
fn end_of(e: &Expr) -> Option<usize> {
    let end = match &e.kind {
        ExprKind::MethodCall {
            args_close_span, ..
        } => args_close_span.offset + args_close_span.length,
        _ => e.span.offset + e.span.length,
    };
    (e.span.length > 0 && end > e.span.offset).then_some(end)
}

impl TypeChecker<'_> {
    /// Note the `Some(d)` argument of a `set_read_timeout` call with the
    /// edits that turn `d` into milliseconds.
    pub(super) fn note_timeout_duration_arg(&mut self, method: &str, args: &[CallArg]) {
        if method != "set_read_timeout" {
            return;
        }
        let [arg] = args else {
            return;
        };
        let ExprKind::Call { callee, args: some } = &arg.value.kind else {
            return;
        };
        if !matches!(&callee.kind, ExprKind::Identifier(n) if n == "Some") {
            return;
        }
        let [inner] = some.as_slice() else {
            return;
        };
        let d = &inner.value;
        self.timeout_ms_args.insert(SpanKey::from_span(&d.span));
        let Some(end) = end_of(d) else {
            return;
        };
        let del = |offset: usize, length: usize, replacement: &str| TextEdit {
            offset,
            length,
            replacement: replacement.to_string(),
        };
        let edits = match duration_ctor(d) {
            Some((unit @ ("ms" | "secs"), n))
                if n.span.offset > d.span.offset && n.span.length > 0 =>
            {
                // The call's `)` is its last byte.
                let atomic = is_atomic(n);
                let (open, close) = match (unit, atomic) {
                    ("ms", _) => ("", ""),
                    (_, true) => ("", " * 1000"),
                    _ => ("(", ") * 1000"),
                };
                vec![
                    del(d.span.offset, n.span.offset - d.span.offset, open),
                    del(end - 1, 1, close),
                ]
            }
            _ if is_atomic(d) => vec![del(end, 0, ".as_ms()")],
            _ => vec![del(d.span.offset, 0, "("), del(end, 0, ").as_ms()")],
        };
        self.timeout_duration_args
            .insert(SpanKey::from_span(&d.span), edits);
    }

    /// The mismatch at a noted timeout argument: a `Duration` where the
    /// milliseconds go. Returns whether it reported.
    pub(super) fn report_timeout_duration_arg(
        &mut self,
        expected: &super::Type,
        found: &super::Type,
        span: crate::token::Span,
    ) -> bool {
        let is_duration = matches!(
            super::inference::resolve_type_var_top(found, &self.env.substitutions),
            super::Type::Named { ref name, .. } if name == "Duration"
        );
        if !is_duration || !matches!(expected, super::Type::Int(super::IntSize::I64)) {
            return false;
        }
        let Some(edits) = self
            .timeout_duration_args
            .get(&SpanKey::from_span(&span))
            .cloned()
        else {
            return false;
        };
        self.type_error(
            "expected 'i64', found 'Duration': Kāra's `set_read_timeout` takes the \
             timeout in milliseconds, `Some(ms)`, not a `Duration` (library/io.md § \
             Timeouts)"
                .to_string(),
            span,
            super::TypeErrorKind::TypeMismatch,
        );
        self.error_fix_diffs
            .insert(SpanKey::from_span(&span), edits);
        true
    }
}
