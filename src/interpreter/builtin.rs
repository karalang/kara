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

use super::value::{EnumData, Value};
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
        let default_msg = match name {
            "todo" => "not yet implemented",
            "panic" => "explicit panic",
            _ => "entered unreachable code",
        };
        let full_msg = if msg.is_empty() {
            default_msg.to_string()
        } else if name == "panic" {
            // `panic("msg")` surfaces the user message verbatim (mirrors
            // codegen's `compile_diverge`); todo/unreachable annotate instead.
            msg
        } else {
            format!("{}: {}", default_msg, msg)
        };
        self.record_runtime_error(full_msg, span)
    }

    /// User-facing Display rendering. Differs from `Value`'s context-free
    /// `std::fmt::Display` only in that **struct fields render in declaration
    /// order** — the `Value::Struct` payload is a `HashMap` that has lost
    /// source order, so its bare `Display` iterates in (random) hash order.
    /// Declaration order is recovered from `typecheck_result.struct_info`.
    /// Recurses through the container shapes so a struct nested inside a
    /// `Vec` / tuple / map / slice is ordered too; every other value
    /// (scalars, String, enums, …) delegates to the unchanged `Display`.
    /// Routed through the user-facing surfaces — `print`/`println`,
    /// `.to_string()`, and f-string interpolation — while `Display` itself
    /// stays for debug / diagnostic contexts. Codegen renders structs in the
    /// same declaration order (see `synth_display.rs`), so the two backends
    /// agree.
    pub(crate) fn display_render(&self, v: &Value) -> String {
        match v {
            Value::Struct { name, fields } => {
                // std.secret: never render a `Secret[T]`'s wrapped value in a
                // built-in / derived Debug/Display. Redacting the whole value
                // here (rather than only at containing-struct field sites)
                // covers every render path uniformly — as a field, an array /
                // map element, or a direct `println(secret)` — and matches
                // codegen's field-level `<redacted>` on the tested surface
                // (a struct with a `Secret` field). Scoped to the stdlib type
                // via `defining_stdlib_origin` so a user's own `struct Secret`
                // renders normally.
                if name == "Secret"
                    && self
                        .typecheck_result
                        .struct_info
                        .get("Secret")
                        .is_some_and(|si| si.defining_stdlib_origin)
                {
                    return "<redacted>".to_string();
                }
                let order: Vec<String> = self
                    .typecheck_result
                    .struct_info
                    .get(name)
                    .map(|si| si.fields.iter().map(|(n, _, _)| n.clone()).collect())
                    .unwrap_or_else(|| fields.keys().cloned().collect());
                let body = order
                    .iter()
                    .filter_map(|fname| {
                        fields
                            .get(fname)
                            .map(|fv| format!("{}: {}", fname, self.display_render(fv)))
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{} {{ {} }}", name, body)
            }
            Value::Tuple(vals) => {
                let body = vals
                    .iter()
                    .map(|x| self.display_render(x))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("({})", body)
            }
            Value::Array(rc) => {
                let vals = rc.read().unwrap();
                let body = vals
                    .iter()
                    .map(|x| self.display_render(x))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("[{}]", body)
            }
            Value::Slice {
                storage,
                start,
                len,
                ..
            } => {
                let vals = storage.read().unwrap();
                let body = vals[*start..*start + *len]
                    .iter()
                    .map(|x| self.display_render(x))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("[{}]", body)
            }
            Value::Map(entries) => {
                let body = entries
                    .iter()
                    .map(|(k, val)| {
                        format!("{}: {}", self.display_render(k), self.display_render(val))
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{{{}}}", body)
            }
            // Enum variants render `Variant` / `Variant(f0, f1)` /
            // `Variant { name: v }`, recursing so nested payloads format the
            // same way (and struct-variant fields in DECLARATION order, from
            // `enum_info`, not the payload `HashMap`'s hash order). This is the
            // enum sibling of the `Value::Struct` declaration-order fix above
            // and must match codegen's `emit_enum_display_fn` byte-for-byte.
            Value::EnumVariant {
                enum_name,
                variant,
                data,
            } => match data {
                EnumData::Unit => variant.clone(),
                EnumData::Tuple(vals) => {
                    let body = vals
                        .iter()
                        .map(|x| self.display_render(x))
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!("{}({})", variant, body)
                }
                EnumData::Struct(fields) => {
                    let order: Vec<String> = self
                        .typecheck_result
                        .enum_info
                        .get(enum_name)
                        .and_then(|ei| ei.variants.iter().find(|(n, _)| n == variant))
                        .and_then(|(_, vt)| match vt {
                            crate::typechecker::VariantTypeInfo::Struct(fs) => {
                                Some(fs.iter().map(|(n, _)| n.clone()).collect())
                            }
                            _ => None,
                        })
                        .unwrap_or_else(|| fields.keys().cloned().collect());
                    let body = order
                        .iter()
                        .filter_map(|fname| {
                            fields
                                .get(fname)
                                .map(|fv| format!("{}: {}", fname, self.display_render(fv)))
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!("{} {{ {} }}", variant, body)
                }
            },
            other => format!("{}", other),
        }
    }

    /// Return the impl-method key (`<TypeName>.to_string`) when `v` is a
    /// user-declared nominal type (struct / enum) carrying a user
    /// `impl Display` — i.e. a registered `to_string` method, as opposed to the
    /// built-in `display_render` renderer or a `#[derive(Display)]`. Used to let
    /// a user `impl Display` win over the built-in `to_string` path so it takes
    /// effect for `x.to_string()`, `f"{x}"`, and `println(x)`. GAP-W4.
    pub(crate) fn user_display_impl_to_string_key(&self, v: &Value) -> Option<String> {
        match v {
            Value::Struct { .. } | Value::EnumVariant { .. } => {}
            _ => return None,
        }
        let key = format!("{}.to_string", self.value_type_name(v));
        self.env.get(&key).is_some().then_some(key)
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
            // Render through the unified `to_string` dispatch so `println(x)`
            // honors a user `impl Display` (built-in types fall through to
            // `display_render` inside that dispatch). GAP-W4.
            match self.eval_method_call(&arg.value, "to_string", &[], span, span) {
                Value::String(s) => s,
                other => self.display_render(&other),
            }
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
        if self.pending_cf.is_some() {
            return cond;
        }
        if matches!(cond, Value::Bool(true)) {
            return Value::Unit;
        }
        // Optional 2-arg `assert(cond, "msg")` failure message. A string
        // LITERAL is used verbatim; a dynamic message falls back to the bare
        // "assertion failed" — kept symmetric with codegen's `compile_assert`
        // so the two backends report the same text (B-2026-07-18-26).
        let msg = match args.get(1).map(|a| &a.value.kind) {
            Some(ExprKind::StringLit(s)) => s.as_str(),
            _ => "assertion failed",
        };
        self.record_runtime_error(msg, span)
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

    /// `std.time::sleep_ms(ms: i64)` — the tree-walk interpreter has no
    /// async reactor, so the faithful semantics of a `suspends` sleep is
    /// a real wall-clock pause: block this thread for `ms` milliseconds.
    /// The codegen path (`emit_state_machine_invocation_for_park_on_timer`)
    /// instead parks the task on the reactor's timer wheel so siblings in a
    /// `par {}` overlap; the interpreter is sequential, so a thread sleep
    /// matches its execution model. Negative / missing arg → no-op.
    ///
    /// wasm32 (the browser playground runs this interpreter client-side):
    /// `std::thread::sleep` panics (`sys/unsupported`) and the synchronous
    /// tree-walk cannot block the browser main thread anyway, so the pause
    /// is a no-op there — the arg is still evaluated for its effects.
    pub(crate) fn eval_builtin_sleep_ms(&mut self, args: &[CallArg], span: &Span) -> Value {
        self.track_effect("suspends");
        let ms = match args.first() {
            Some(a) => self.eval_expr_inner(&a.value),
            None => return self.record_runtime_error("sleep_ms requires one argument", span),
        };
        if let Value::Int(ms) = ms {
            if ms > 0 {
                #[cfg(not(target_arch = "wasm32"))]
                std::thread::sleep(std::time::Duration::from_millis(ms as u64));
            }
        }
        Value::Unit
    }
}
