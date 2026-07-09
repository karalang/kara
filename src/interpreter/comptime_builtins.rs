//! Comptime stdlib builtins (substrates 3–4) — the `ast` / `compiler` modules.
//!
//! - `ast.expr(s: String) -> Expr` — the quasi-quote AST builder: parse a
//!   Kāra expression string at compile time into an `Expr` AST value. When a
//!   `comptime { ... }` block yields the result, the fold pass splices the
//!   generated code at the comptime site (code generation). Interpolate
//!   comptime values with an f-string: `ast.expr(f"x * {n}")`.
//! - `ast.item(s: String) -> Item` — the item-level quasi-quote builder
//!   (substrate 4): parse a whole top-level item (an `impl` block, a `fn`, …)
//!   at compile time into an `Item` AST value. A `#[derive(X)]` desugars to a
//!   call to `derive_x(comptime T: Type) -> Vec[Item]`; the returned
//!   `ast.item(...)` values are spliced into the module after the derive site.
//! - `compiler.error(msg: String)` — emit a compile-time diagnostic at the
//!   call site (compile-time validation). Non-halting: evaluation continues
//!   and the fold pass surfaces the message as `E_COMPTIME_ERROR`.
//!
//! Spec: deferred.md § Comptime — AST builder API / Comptime stdlib surface /
//! Code generation and derive desugaring.

use crate::ast::{CallArg, Expr, Item};
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

    /// `ast.item(s)` — parse `s` into an `Item` AST value (substrate 4).
    pub(crate) fn eval_ast_item_builder(&mut self, args: &[CallArg], span: &Span) -> Value {
        let s = match args.first().map(|a| self.eval_expr_inner(&a.value)) {
            Some(Value::String(s)) => s,
            _ => {
                return self.record_runtime_error(
                    "ast.item expects a single String argument".to_string(),
                    span,
                )
            }
        };
        match parse_comptime_item(&s) {
            Ok(mut item) => {
                // Claim a unique high span window for this fragment so its
                // nodes keep DISTINCT SpanKeys (codegen side-tables key on them);
                // preserve each node's relative offset by shifting, not
                // collapsing. See `reanchor_item_spans` (B-2026-07-08-15 Layer 1).
                let base = self.comptime_splice_base;
                self.comptime_splice_base += 1_000_000;
                reanchor_item_spans(&mut item, span, base);
                Value::AstItem(Box::new(item))
            }
            Err(why) => self.record_runtime_error(
                format!("ast.item: could not parse quoted item `{s}`: {why}"),
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
/// `Expr` or a rendered parse-error string. `pub(crate)` so the derive-
/// expansion pass (`comptime.rs`) can reuse it to build a `derive_x(T)` call.
pub(crate) fn parse_comptime_expr(s: &str) -> Result<Expr, String> {
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

/// Parse a single top-level Kāra item from `s` at comptime. Returns the parsed
/// `Item` or a rendered parse-error string. Rejects a quote that contains more
/// than one item (`ast.item` is singular — emit several by returning several
/// `ast.item(...)` values from the derive fn).
fn parse_comptime_item(s: &str) -> Result<Item, String> {
    let tokens = crate::tokenize(s);
    let result = crate::parser::Parser::new(tokens).parse();
    if !result.errors.is_empty() {
        let msgs: Vec<String> = result.errors.iter().map(|e| e.message.clone()).collect();
        return Err(msgs.join("; "));
    }
    let mut items = result.program.items;
    match items.len() {
        1 => Ok(items.pop().unwrap()),
        0 => Err("not a valid item".to_string()),
        _ => Err("quote contains more than one item".to_string()),
    }
}

/// Overwrite every span in `expr` with `site` so a spliced quasi-quote points
/// at the comptime call site rather than at bogus offsets into the quote
/// string. A coarse re-anchor (one span for the whole fragment) — fine for
/// the tree-walk interpreter, which uses spans only for diagnostics.
fn reanchor_spans(expr: &mut Expr, site: &Span) {
    crate::span_visitor::visit_expr_spans_mut(expr, &mut |s| *s = site.clone());
}

/// The item analogue of [`reanchor_spans`] for a spliced derive-generated item.
/// Points `line`/`column` at the derive call `site` (so diagnostics blame the
/// derive site, not bogus offsets into the quote string), but SHIFTS each
/// node's `offset` into `base`'s unique high window while PRESERVING its
/// relative offset and `length`. This keeps distinct generated nodes at
/// distinct `SpanKey`s — the previous behavior collapsed every node onto the
/// single site span, so codegen's span-keyed side-tables (element type of an
/// un-annotated `let v = Vec.new()`, etc.) all collided on one key and the
/// generated body miscompiled (B-2026-07-08-15 Layer 1). `base` is well above
/// any real source offset, so generated spans never collide with the host
/// program's or with another fragment's window.
fn reanchor_item_spans(item: &mut Item, site: &Span, base: usize) {
    crate::span_visitor::visit_item_spans_mut(item, &mut |s| {
        s.line = site.line;
        s.column = site.column;
        s.offset = base.wrapping_add(s.offset);
    });
}
