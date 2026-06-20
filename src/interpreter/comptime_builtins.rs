//! Comptime stdlib builtins (substrate 3) — the `ast` / `compiler` modules.
//!
//! - `ast.expr(s: String) -> Expr` — the quasi-quote AST builder: parse a
//!   Kāra expression string at compile time into an `Expr` AST value. When a
//!   `comptime { ... }` block yields the result, the fold pass splices the
//!   generated code at the comptime site (code generation). Interpolate
//!   comptime values with an f-string: `ast.expr(f"x * {n}")`.
//! - `compiler.error(msg: String)` — emit a compile-time diagnostic at the
//!   call site (compile-time validation). Non-halting: evaluation continues
//!   and the fold pass surfaces the message as `E_COMPTIME_ERROR`.
//!
//! Spec: deferred.md § Comptime — AST builder API / Comptime stdlib surface.

use crate::ast::{CallArg, Expr};
use crate::token::Span;

use super::value::{RuntimeError, Value};
use super::Interpreter;

impl Interpreter<'_> {
    /// `ast.expr(s)` — parse `s` into an `Expr` AST value.
    pub(crate) fn eval_ast_expr_builder(&mut self, args: &[CallArg], span: &Span) -> Value {
        let s = match args.first().map(|a| self.eval_expr_inner(&a.value)) {
            Some(Value::String(s)) => s,
            _ => {
                return self.record_runtime_error(
                    "ast.expr expects a single String argument".to_string(),
                    span,
                )
            }
        };
        match parse_comptime_expr(&s) {
            Ok(mut expr) => {
                // The quoted fragment's spans are relative to the quote string,
                // not the source file — re-anchor them at the splice site so
                // any later diagnostic points somewhere sane.
                reanchor_spans(&mut expr, span);
                Value::AstExpr(Box::new(expr))
            }
            Err(why) => self.record_runtime_error(
                format!("ast.expr: could not parse quoted expression `{s}`: {why}"),
                span,
            ),
        }
    }

    /// `compiler.error(msg)` — record a comptime diagnostic and continue.
    pub(crate) fn eval_compiler_error(&mut self, args: &[CallArg], span: &Span) -> Value {
        let msg = match args.first().map(|a| self.eval_expr_inner(&a.value)) {
            Some(Value::String(s)) => s,
            _ => "comptime error".to_string(),
        };
        self.comptime_user_errors.push(RuntimeError {
            message: msg,
            span: span.clone(),
            left: None,
            right: None,
        });
        Value::Unit
    }
}

/// Parse a single Kāra expression from `s` at comptime. Returns the parsed
/// `Expr` or a rendered parse-error string.
fn parse_comptime_expr(s: &str) -> Result<Expr, String> {
    let tokens = crate::tokenize(s);
    let mut parser = crate::parser::Parser::new(tokens);
    match parser.parse_expression() {
        Some(expr) if parser.errors.is_empty() => Ok(expr),
        _ => {
            let msgs: Vec<String> = parser.errors.iter().map(|e| e.message.clone()).collect();
            Err(if msgs.is_empty() {
                "not a valid expression".to_string()
            } else {
                msgs.join("; ")
            })
        }
    }
}

/// Overwrite every span in `expr` with `site` so a spliced quasi-quote points
/// at the comptime call site rather than at bogus offsets into the quote
/// string. A coarse re-anchor (one span for the whole fragment) — fine for
/// the tree-walk interpreter, which uses spans only for diagnostics.
fn reanchor_spans(expr: &mut Expr, site: &Span) {
    crate::span_visitor::visit_expr_spans_mut(expr, &mut |s| *s = site.clone());
}
