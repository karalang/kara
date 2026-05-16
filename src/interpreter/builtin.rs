//! Built-in function evaluation: `panic`/`unreachable`/`todo` (diverge),
//! `print`/`println`/`eprintln`, `dbg!`, and the three assert flavors.
//!
//! Houses `eval_builtin_diverge` (effect: `panics`, sets `ExitUnwind`),
//! `eval_builtin_print` (formats + routes through the
//! `Stdout.print` / `Stderr.println` provider arms), `write_stdout` /
//! `write_stderr` (the BuiltinDefault arms that honor the test
//! harness's captured-output buffer), `eval_builtin_dbg` (formatted
//! source-location-aware debug print), and `eval_builtin_assert*`
//! (the three assert flavors with structured failure-trace records).
//!
//! Lives in a sibling `impl<'a> super::Interpreter<'a>` block.

use crate::ast::*;
use crate::resolver::SpanKey;
use crate::token::Span;
use crate::typechecker::type_display;

use super::value::Value;
use super::{dbg_json_escape, DbgOutputMode};

impl<'a> super::Interpreter<'a> {
    // ── Built-in functions ───────────────────────────────────────

    pub(crate) fn eval_builtin_diverge(
        &mut self,
        name: &str,
        args: &[CallArg],
        span: &Span,
    ) -> Value {
        self.track_effect("panics");
        let msg = if let Some(arg) = args.first() {
            match self.eval_expr_inner(&arg.value) {
                Value::String(s) => s,
                _ => String::new(),
            }
        } else {
            String::new()
        };
        let default_msg = if name == "todo" {
            "not yet implemented"
        } else {
            "entered unreachable code"
        };
        let full_msg = if msg.is_empty() {
            default_msg.to_string()
        } else {
            format!("{}: {}", default_msg, msg)
        };
        self.record_runtime_error(full_msg, span)
    }

    pub(crate) fn eval_builtin_print(
        &mut self,
        name: &str,
        args: &[CallArg],
        span: &Span,
    ) -> Value {
        // Route through the Stdout / Stderr provider stack so a
        // `with_provider[Stdout]` / `[Stderr]` install can intercept idiomatic
        // `println(x)` calls — not just direct `Stdout.println(s)` calls.
        // The user's provider method receives an already-formatted String;
        // the BuiltinDefault arm writes through `write_stdout` /
        // `write_stderr` (honoring `captured_output` for the test harness).
        let val = if let Some(arg) = args.first() {
            format!("{}", self.eval_expr_inner(&arg.value))
        } else {
            String::new()
        };
        if self.check_cf() {
            return Value::Unit;
        }
        let (resource, method) = match name {
            "eprintln" => ("Stderr", "println"),
            "println" => ("Stdout", "println"),
            _ => ("Stdout", "print"),
        };
        self.dispatch_resource_method_with_values(resource, method, vec![Value::String(val)], span)
    }

    /// Write to stdout, honoring `captured_output` when the test harness
    /// installed it. Used by both the free `print` / `println` router
    /// and the `Stdout.print` / `Stdout.println` resource methods so the
    /// two surfaces share one capture path.
    pub(crate) fn write_stdout(&mut self, s: &str, newline: bool) {
        if let Some(ref mut output) = self.captured_output {
            if newline {
                output.push(format!("{}\n", s));
            } else {
                output.push(s.to_string());
            }
        } else if newline {
            println!("{}", s);
        } else {
            print!("{}", s);
        }
    }

    /// Write to stderr. No capture buffer today — `captured_output` is
    /// stdout-only and the test harness does not currently snapshot stderr.
    /// Mirrors `write_stdout` so the `Stderr` arms have the same shape as
    /// `Stdout`'s without forcing every Stderr test to learn a new pattern.
    pub(crate) fn write_stderr(&mut self, s: &str, newline: bool) {
        if newline {
            eprintln!("{}", s);
        } else {
            eprint!("{}", s);
        }
    }

    pub(crate) fn eval_builtin_dbg(&mut self, args: &[CallArg], span: &Span) -> Value {
        // dbg() uses the transparent `debugs` effect (design.md § dbg() —
        // transparent and stripped in release builds), but the underlying
        // I/O still writes stderr. The track_effect call records that for
        // any future runtime instrumentation; transparency is enforced by
        // the static effect checker, not here.
        self.track_effect("writes(Stderr)");
        let arg_expr = args.first().map(|a| &a.value);
        let val = if let Some(expr) = arg_expr {
            self.eval_expr_inner(expr)
        } else {
            Value::Unit
        };

        // Source slice for the `expr` field. Falls back to "<expr>" when
        // the interpreter was constructed without a source-text setter
        // (some unit tests bypass the CLI) or the slice would be empty.
        let expr_text = arg_expr
            .and_then(|e| {
                let off = e.span.offset;
                let end = off.saturating_add(e.span.length);
                self.source_text.get(off..end)
            })
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| "<expr>".to_string());

        // Type lookup via the typecheck side table. "?" when unavailable;
        // not all expression kinds reach the typechecker's recording path,
        // and ad-hoc test harnesses sometimes synthesize a TypeCheckResult
        // without populating expr_types.
        let type_text = arg_expr
            .and_then(|e| {
                self.typecheck_result
                    .expr_types
                    .get(&SpanKey::from_span(&e.span))
            })
            .map(type_display)
            .unwrap_or_else(|| "?".to_string());

        let file = if self.source_filename.is_empty() {
            "<unknown>".to_string()
        } else {
            self.source_filename.clone()
        };
        let value_str = val.debug_fmt();

        let line = match self.dbg_output_mode {
            DbgOutputMode::Terminal => match self.current_task_id {
                Some(tid) => format!(
                    "[task:{} {}:{}] {} = {}\n",
                    tid, file, span.line, expr_text, value_str
                ),
                None => format!("[{}:{}] {} = {}\n", file, span.line, expr_text, value_str),
            },
            DbgOutputMode::Json => {
                let task_id = match self.current_task_id {
                    Some(tid) => tid.to_string(),
                    None => "null".to_string(),
                };
                format!(
                    "{{\"kind\":\"dbg\",\"task_id\":{},\"file\":{},\"line\":{},\"expr\":{},\"type\":{},\"value\":{}}}\n",
                    task_id,
                    dbg_json_escape(&file),
                    span.line,
                    dbg_json_escape(&expr_text),
                    dbg_json_escape(&type_text),
                    dbg_json_escape(&value_str),
                )
            }
        };

        if let Some(ref mut cap) = self.captured_dbg {
            cap.push(line);
        } else {
            // Single atomic write — POSIX guarantees writes up to
            // PIPE_BUF bytes (4096 on Linux) are atomic at the
            // syscall level, so sibling-task lines never tear.
            use std::io::Write;
            let stderr = std::io::stderr();
            let mut handle = stderr.lock();
            let _ = handle.write_all(line.as_bytes());
        }

        val
    }

    pub(crate) fn eval_builtin_assert(&mut self, args: &[CallArg], span: &Span) -> Value {
        self.track_effect("panics");
        let cond = match args.first() {
            Some(a) => self.eval_expr_inner(&a.value),
            None => return self.record_runtime_error("assert called with no arguments", span),
        };
        if matches!(cond, Value::Bool(true)) {
            return Value::Unit;
        }
        self.record_runtime_error("assertion failed", span)
    }

    pub(crate) fn eval_builtin_assert_eq(&mut self, args: &[CallArg], span: &Span) -> Value {
        self.track_effect("panics");
        let left = match args.first() {
            Some(a) => self.eval_expr_inner(&a.value),
            None => return self.record_runtime_error("assert_eq requires two arguments", span),
        };
        let right = match args.get(1) {
            Some(a) => self.eval_expr_inner(&a.value),
            None => return self.record_runtime_error("assert_eq requires two arguments", span),
        };
        if left == right {
            return Value::Unit;
        }
        let lstr = left.debug_fmt();
        let rstr = right.debug_fmt();
        self.record_runtime_assertion("assertion failed: left != right", lstr, rstr, span)
    }

    pub(crate) fn eval_builtin_assert_ne(&mut self, args: &[CallArg], span: &Span) -> Value {
        self.track_effect("panics");
        let left = match args.first() {
            Some(a) => self.eval_expr_inner(&a.value),
            None => return self.record_runtime_error("assert_ne requires two arguments", span),
        };
        let right = match args.get(1) {
            Some(a) => self.eval_expr_inner(&a.value),
            None => return self.record_runtime_error("assert_ne requires two arguments", span),
        };
        if left != right {
            return Value::Unit;
        }
        let lstr = left.debug_fmt();
        let rstr = right.debug_fmt();
        self.record_runtime_assertion("assertion failed: left == right", lstr, rstr, span)
    }
}
