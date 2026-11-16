//! The pre-v1 `read_to_string()` that returned the text, carried over
//! (B-2026-11-16-203).
//!
//! A child's pipe (`ChildStdout`, `ChildStderr`) and the other readers take
//! the `String` to append to and return the count read:
//! `fn read_to_string(ref self, buf: mut ref String) -> Result[i64, IoError]`
//! (library/io.md). Programs written against the old shape call it with no
//! argument and unwrap the text, `r.read_to_string().unwrap_or(String.new())`,
//! which left "expects 1 argument(s), found 0" with no fix and a cascade of
//! `i64` errors wherever the text was used (`no method 'trim' on type
//! 'i64'`). When the receiver is a local whose type has the appending
//! `read_to_string`, the unwrapped call is one error whose fix spells the
//! buffer out:
//!
//! - `r.read_to_string().unwrap_or(x)` becomes
//!   `{ let mut text = String.new(); match r.read_to_string(mut text) { Ok(_) => text, Err(_) => x } }`,
//!   so a failed read still yields `x`, as it did;
//! - `r.read_to_string().unwrap()` and `.expect(m)` become
//!   `{ let mut text = String.new(); r.read_to_string(mut text).unwrap(); text }`.

use crate::ast::{CallArg, Expr, ExprKind};
use crate::resolver::{SpanKey, TextEdit};
use crate::token::Span;

use super::{Type, TypeChecker, TypeErrorKind};

/// A name for the buffer that `text` (the rewritten source) does not use
/// as a word already.
fn fresh_name(text: &str) -> &'static str {
    let used = |name: &str| {
        text.match_indices(name).any(|(at, _)| {
            let before = text[..at].chars().next_back();
            let after = text[at + name.len()..].chars().next();
            let word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
            !word(before) && !word(after)
        })
    };
    ["text", "read_text", "pipe_text", "buf_text"]
        .into_iter()
        .find(|n| !used(n))
        .unwrap_or("read_to_string_text")
}

impl TypeChecker<'_> {
    /// The pre-v1 `ws.recv_text(mut buf)` / `recv_binary(mut buf)`, which
    /// filled a byte buffer and returned the count (B-2026-11-16-204).
    /// A `WebSocket` now reads whole messages, so the call names `recv()`
    /// and the `WsMessage` it returns. The rewrite changes the call's type
    /// and the code that used the count, so it is a hint, not an edit.
    pub(super) fn explain_old_ws_recv(
        &mut self,
        object: &Expr,
        method: &str,
        args: &[CallArg],
    ) -> Option<Type> {
        let variant = match method {
            "recv_text" => "Text(t)",
            "recv_binary" => "Binary(b)",
            _ => return None,
        };
        let ExprKind::Identifier(name) = &object.kind else {
            return None;
        };
        let ty = self.local_scope.lookup(name)?.clone();
        let ty = super::inference::resolve_type_var_top(&ty, &self.env.substitutions);
        let is_ws = matches!(
            super::expr_ops::strip_refs_for_compare(&ty),
            Type::Named { name, .. } if name == "WebSocket"
        );
        let defined = !self
            .env
            .find_methods_with_args("WebSocket", &[], method)
            .is_empty();
        if !is_ws || defined || object.span.length == 0 {
            return None;
        }
        for a in args {
            self.infer_expr(&a.value);
        }
        self.type_error(
            format!(
                "no method '{method}' on type 'WebSocket': a `WebSocket` reads whole \
                 messages, not into a buffer; `{name}.recv()` returns \
                 `Result[WsMessage, NetError]`, and the message is \
                 `Ok(WsMessage.{variant})` (library/io.md § WebSocket)"
            ),
            object.span,
            TypeErrorKind::NoMethodFound,
        );
        Some(Type::Error)
    }

    /// Whether the nominal type `ty` (through references) has the appending
    /// `read_to_string(mut ref String)`.
    fn has_appending_read_to_string(&self, ty: &Type) -> bool {
        let ty = super::inference::resolve_type_var_top(ty, &self.env.substitutions);
        let ty = super::expr_ops::strip_refs_for_compare(&ty);
        let Type::Named { name, .. } = ty else {
            return false;
        };
        self.env.impls.iter().any(|imp| {
            imp.target_type == *name
                && imp.methods.get("read_to_string").is_some_and(|sig| {
                    matches!(
                        sig.params.as_slice(),
                        [Type::MutRef(inner)] if matches!(inner.as_ref(), Type::Str)
                    )
                })
        })
    }

    /// `r.read_to_string().unwrap_or(x)` (or `.unwrap()`, `.expect(m)`) on
    /// a local `r` whose `read_to_string` appends: one error with the fix,
    /// and the call is the `String` it meant. `None` leaves the call to the
    /// general checks.
    pub(super) fn mend_old_read_to_string(
        &mut self,
        object: &Expr,
        method: &str,
        args: &[CallArg],
        args_close_span: &Span,
    ) -> Option<Type> {
        if !matches!(
            (method, args.len()),
            ("unwrap_or", 1) | ("unwrap", 0) | ("expect", 1)
        ) {
            return None;
        }
        let ExprKind::MethodCall {
            object: reader,
            method: inner,
            args: inner_args,
            args_close_span: inner_close,
            ..
        } = &object.kind
        else {
            return None;
        };
        if inner != "read_to_string" || !inner_args.is_empty() {
            return None;
        }
        let ExprKind::Identifier(name) = &reader.kind else {
            return None;
        };
        let reader_ty = self.local_scope.lookup(name)?.clone();
        if !self.has_appending_read_to_string(&reader_ty) {
            return None;
        }
        // Spans the edits need: the reader's start, the inner `)`, and the
        // outer call's `(` .. `)`; a synthetic (zero-length) span gets no fix.
        let start = reader.span.offset;
        let inner_paren = inner_close.offset;
        let outer_end = args_close_span.offset + args_close_span.length;
        if reader.span.length == 0
            || inner_close.length == 0
            || args_close_span.length == 0
            || inner_paren < start
            || outer_end <= inner_paren
        {
            return None;
        }
        let rendered = args
            .iter()
            .map(|a| crate::formatter::render_expr(&a.value))
            .collect::<Vec<_>>()
            .join(" ");
        let buf = fresh_name(&format!("{name} {rendered}"));
        let edit = |offset: usize, length: usize, text: String| TextEdit {
            offset,
            length,
            replacement: text,
        };
        let open = edit(start, 0, format!("{{ let mut {buf} = String.new(); "));
        let pass = edit(inner_paren, 0, format!("mut {buf}"));
        let edits = match (method, args) {
            ("unwrap_or", [fallback]) => {
                let x = &fallback.value;
                if x.span.length == 0 || x.span.offset <= inner_paren {
                    return None;
                }
                vec![
                    edit(start, 0, format!("{{ let mut {buf} = String.new(); match ")),
                    pass,
                    // `.unwrap_or(` up to the fallback.
                    edit(
                        inner_paren + 1,
                        x.span.offset - inner_paren - 1,
                        format!(" {{ Ok(_) => {buf}, Err(_) => "),
                    ),
                    // The outer `)`.
                    edit(
                        args_close_span.offset,
                        args_close_span.length,
                        " } }".into(),
                    ),
                ]
            }
            _ => vec![open, pass, edit(outer_end, 0, format!("; {buf} }}"))],
        };
        for a in args {
            self.infer_expr(&a.value);
        }
        let span = reader.span;
        self.type_error(
            format!(
                "method 'read_to_string' expects 1 argument(s), found 0: it appends to \
                 the `String` it is given and returns the count read, `fn \
                 read_to_string(ref self, buf: mut ref String) -> Result[i64, IoError]` \
                 (library/io.md); pass one, `let mut {buf} = String.new(); \
                 {name}.read_to_string(mut {buf})`"
            ),
            span,
            TypeErrorKind::WrongNumberOfArgs,
        );
        self.error_fix_diffs
            .insert(SpanKey::from_span(&span), edits);
        Some(Type::Str)
    }
}
