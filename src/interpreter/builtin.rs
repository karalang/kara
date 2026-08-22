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
    /// Render a value the way the user-facing surfaces do — `print`/`println`,
    /// `.to_string()`, and f-string interpolation — with the value's STATIC
    /// TYPE threaded alongside it, peeled one layer per level of the recursion.
    ///
    /// B-2026-08-19-27. Rendering an integer needs its type, because the
    /// interpreter holds an unsigned value as its two's-complement bit pattern
    /// in a signed carrier — a `u64` at or above 2^63, or a `u128` above
    /// `i128::MAX`, is a NEGATIVE `Value::Int`. The scalar `to_string` arm has
    /// always known this and consults the receiver's span. This renderer never
    /// did: it walks `Value`s structurally, so every nested integer was
    /// formatted signed and `println(o)` on an `Option[u64]` holding `u64::MAX`
    /// printed `Some(-1)` while both compiled backends printed the value. The
    /// divergence covered every container and payload shape — `Vec`, tuple,
    /// `Map`, struct field, enum payload, and any nesting of them.
    ///
    /// `ty` is `None` when the caller has no static type to offer (a `Value`
    /// reached from a context with no span, and the recursive positions whose
    /// parent type was itself unknown). That degrades to exactly the previous
    /// behaviour — signed — so an unresolved type is never worse than before.
    pub(crate) fn display_render_typed(
        &self,
        v: &Value,
        ty: Option<&crate::typechecker::Type>,
    ) -> String {
        use crate::typechecker::Type;
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
                let field_tys = self.struct_field_display_types(name, ty);
                let body = order
                    .iter()
                    .filter_map(|fname| {
                        fields.get(fname).map(|fv| {
                            let fty = field_tys.get(fname);
                            format!("{}: {}", fname, self.display_render_typed(fv, fty))
                        })
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{} {{ {} }}", name, body)
            }
            Value::Tuple(vals) => {
                let elem_tys: Option<&Vec<Type>> = match ty {
                    Some(Type::Tuple(ts)) => Some(ts),
                    _ => None,
                };
                let body = vals
                    .iter()
                    .enumerate()
                    .map(|(i, x)| self.display_render_typed(x, elem_tys.and_then(|ts| ts.get(i))))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("({})", body)
            }
            Value::Array(rc) => {
                let elem = Self::display_element_type(ty);
                let vals = rc.read().unwrap();
                let body = vals
                    .iter()
                    .map(|x| self.display_render_typed(x, elem))
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
                let elem = Self::display_element_type(ty);
                let vals = storage.read().unwrap();
                let body = vals[*start..*start + *len]
                    .iter()
                    .map(|x| self.display_render_typed(x, elem))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("[{}]", body)
            }
            Value::Map(entries) => {
                let entries = entries.read().unwrap();
                // `Map[K, V]` / `SortedMap[K, V]` — both K and V can be an
                // unsigned scalar, so both sides peel.
                let (kt, vt) = match ty {
                    Some(Type::Named { name, args })
                        if (name == "Map" || name == "SortedMap") && args.len() == 2 =>
                    {
                        (Some(&args[0]), Some(&args[1]))
                    }
                    _ => (None, None),
                };
                // Hash order — the `Display` / `for` / `.iter()` walks all
                // agree, and design.md § Map requires it to vary per process
                // (B-2026-08-21-6).
                let body = entries
                    .iter_observable()
                    .map(|(k, val)| {
                        format!(
                            "{}: {}",
                            self.display_render_typed(k, kt),
                            self.display_render_typed(val, vt)
                        )
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
                    let ptys = self.variant_payload_display_types(enum_name, variant, ty);
                    let body = vals
                        .iter()
                        .enumerate()
                        .map(|(i, x)| {
                            self.display_render_typed(x, ptys.as_ref().and_then(|(_, t)| t.get(i)))
                        })
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
                    let ptys = self.variant_payload_display_types(enum_name, variant, ty);
                    let body = order
                        .iter()
                        .filter_map(|fname| {
                            fields.get(fname).map(|fv| {
                                let fty = ptys.as_ref().and_then(|(names, t)| {
                                    names.iter().position(|n| n == fname).and_then(|i| t.get(i))
                                });
                                format!("{}: {}", fname, self.display_render_typed(fv, fty))
                            })
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!("{} {{ {} }}", variant, body)
                }
            },
            // The one leaf that depends on its type. An unsigned value at a
            // width whose top half does not fit the signed carrier is held as a
            // negative bit pattern, so the signed `Display` prints the wrong
            // reading — `-1` for `u64::MAX`. Narrower unsigned widths fit
            // non-negatively, so signed and unsigned coincide and they are
            // absent from `type_unsigned_int_width` rather than forgotten.
            Value::Int(n) => match ty.and_then(Self::type_unsigned_int_width) {
                Some(64) => format!("{}", *n as u64),
                Some(128) => format!("{}", *n as u128),
                _ => format!("{}", v),
            },
            other => format!("{}", other),
        }
    }

    /// Declared field types for `struct_name`, with the struct's generic
    /// parameters substituted from the concrete `ty` when there is one — so a
    /// `Wrap[u64]`'s `T` field renders as a `u64`, not as an unresolved param.
    /// Empty when the struct is unknown.
    fn struct_field_display_types(
        &self,
        struct_name: &str,
        ty: Option<&crate::typechecker::Type>,
    ) -> std::collections::HashMap<String, crate::typechecker::Type> {
        let Some(si) = self.typecheck_result.struct_info.get(struct_name) else {
            return std::collections::HashMap::new();
        };
        let subs = Self::display_type_subs(&si.generic_params, ty);
        si.fields
            .iter()
            .map(|(n, t, _)| {
                (
                    n.clone(),
                    crate::typechecker::inference::substitute_type_params(t, &subs),
                )
            })
            .collect()
    }

    /// Declared payload types for `enum_name::variant` — the field NAMES (empty
    /// for a tuple variant) and their types, with the enum's generic parameters
    /// substituted from the concrete `ty`. That substitution is what makes the
    /// seeded generics work: `Option`'s `Some` declares a bare `T`, so without
    /// it an `Option[u64]` payload carries no width at all.
    #[allow(clippy::type_complexity)]
    fn variant_payload_display_types(
        &self,
        enum_name: &str,
        variant: &str,
        ty: Option<&crate::typechecker::Type>,
    ) -> Option<(Vec<String>, Vec<crate::typechecker::Type>)> {
        use crate::typechecker::VariantTypeInfo;
        let ei = self.typecheck_result.enum_info.get(enum_name)?;
        let (_, vt) = ei.variants.iter().find(|(n, _)| n == variant)?;
        let subs = Self::display_type_subs(&ei.generic_params, ty);
        let sub = |t: &crate::typechecker::Type| {
            crate::typechecker::inference::substitute_type_params(t, &subs)
        };
        Some(match vt {
            VariantTypeInfo::Unit => (Vec::new(), Vec::new()),
            VariantTypeInfo::Tuple(ts) => (Vec::new(), ts.iter().map(sub).collect()),
            VariantTypeInfo::Struct(fs) => (
                fs.iter().map(|(n, _)| n.clone()).collect(),
                fs.iter().map(|(_, t)| sub(t)).collect(),
            ),
        })
    }

    /// Positional generic substitution from a concrete `Named` type onto a
    /// declaration's parameter list. Empty when either side is missing, which
    /// leaves `substitute_type_params` a no-op and the rendering signed —
    /// the same degradation as an unknown type.
    fn display_type_subs(
        params: &[String],
        ty: Option<&crate::typechecker::Type>,
    ) -> std::collections::HashMap<String, crate::typechecker::SubstValue> {
        use crate::typechecker::{SubstValue, Type};
        let mut subs = std::collections::HashMap::new();
        if let Some(Type::Named { args, .. }) = ty {
            for (p, a) in params.iter().zip(args.iter()) {
                subs.insert(p.clone(), SubstValue::Type(a.clone()));
            }
        }
        subs
    }

    /// The element type of a sequence type, for the `Array` / `Slice` arms.
    /// Covers every spelling those two `Value`s can carry — `Vec[T]` and the
    /// sorted/set family arrive as `Named`, fixed arrays and slices as their
    /// own variants.
    fn display_element_type(
        ty: Option<&crate::typechecker::Type>,
    ) -> Option<&crate::typechecker::Type> {
        use crate::typechecker::Type;
        match ty? {
            Type::Array { element, .. }
            | Type::Vector { element, .. }
            | Type::Slice { element, .. } => Some(element),
            Type::Named { name, args }
                if matches!(
                    name.as_str(),
                    "Vec" | "VecDeque" | "Set" | "SortedSet" | "Column" | "Tensor"
                ) && !args.is_empty() =>
            {
                Some(&args[0])
            }
            _ => None,
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
            // `args_close_span` is the ARGUMENT's span, not the `println`
            // call's. The dispatch uses that span to recover the RECEIVER's
            // type — for a real `x.to_string()` the close paren is the only
            // leaf the parser has not aliased to the receiver — and passing the
            // call's span instead resolved it to the call's own `Unit`, a hit
            // that shadowed the correct fallback. That is why `println(o)` on
            // an `Option[u64]` still rendered signed after the renderer learned
            // to take a type (B-2026-08-19-27).
            match self.eval_method_call(&arg.value, "to_string", &[], span, &arg.value.span) {
                Value::String(s) => s,
                other => {
                    self.display_render_typed(&other, self.span_expr_type(&arg.value.span).as_ref())
                }
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
        } else {
            Self::write_program_stdout(s, newline);
        }
    }

    /// The interpreter's one real write to the program's stdout.
    ///
    /// NOT `println!` — B-2026-08-19-2. That macro panics when the write
    /// fails, and the write fails routinely: `karac run --interp prog | head`
    /// closes the reader after a couple of lines, and the next write returns
    /// `BrokenPipe`. The panic then spilled 55 lines of Rust backtrace naming
    /// `karac::interpreter::builtin::write_stdout`, which reads as a compiler
    /// crash rather than as the reader having gone away, and exited 101.
    ///
    /// WHY THE INTERPRETER AND NOT THE OTHER BACKENDS: an AOT binary is the
    /// process, so it inherits the default `SIGPIPE` disposition and the kernel
    /// kills it — silently, with status 141. `karac` is a Rust program, and
    /// Rust sets `SIGPIPE` to `SIG_IGN` at startup, so inside the interpreter
    /// the signal never arrives and the write reports `EPIPE` instead.
    ///
    /// So this reproduces what the kernel would have done: exit 141
    /// (`128 + SIGPIPE`) without a message. Skipping destructors is not a
    /// shortcut — a signal death runs none either, so the abrupt exit is the
    /// faithful part rather than the lossy part.
    ///
    /// Every OTHER io error is left to `expect`. A full disk or a closed
    /// terminal is a real failure the user needs told about; only the
    /// reader-went-away case is normal enough to be silent.
    fn write_program_stdout(s: &str, newline: bool) {
        use std::io::Write;
        let stdout = std::io::stdout();
        let mut lock = stdout.lock();
        let res = if newline {
            writeln!(lock, "{s}")
        } else {
            write!(lock, "{s}").and_then(|()| lock.flush())
        };
        match res {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {
                std::process::exit(141);
            }
            Err(e) => panic!("failed printing to stdout: {e}"),
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
