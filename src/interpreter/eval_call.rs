//! Call evaluation: free-fn / closure / builtin-fn / `with_provider`
//! / provider-method / generic-fn dispatch, plus the function-value
//! invocation helpers used across the interpreter.
//!
//! Houses `eval_call` (the entry from `eval_expr_inner`), the
//! `with_provider` shape match + body, `eval_providers_block` (the
//! sugar form), and the four lower-level invokers:
//! `invoke_zero_arg_closure`, `invoke_function_value`,
//! `invoke_value_comparator`, plus `entry_or_insert_value` (shared
//! between map `Entry.or_insert` and `Entry.or_insert_with`).
//!
//! Lives in a sibling `impl<'a> super::Interpreter<'a>` block.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, RwLock};

use regex::Regex as RustRegex;

use crate::ast::*;
use crate::token::Span;

use super::exec::ControlFlow;
use super::helpers::{
    base64_decode, base64_encode, decode_err, decode_ok_bytes, decode_ok_string, eval_http_get,
    eval_http_post, eval_stats_fn, eval_stats_fn_int, hex_decode, hex_encode, make_json_error,
    serde_json_to_kara_json, url_decode, url_encode, value_bytes,
};
use super::method_call::result_ok;
use super::value::narrow_to_i64;
use super::value::{EnumData, Value};

impl<'a> super::Interpreter<'a> {
    /// `process.exit(code)` — design.md § Stdio & Exit Control. Raises
    /// `ControlFlow::ExitUnwind`, which propagates through every pending
    /// `defer` / `errdefer` on the way out, matching the spec's "controlled
    /// stack unwind that runs all pending `defer`/`errdefer` blocks before
    /// terminating".
    ///
    /// B-2026-08-30-30 — extracted so the TWO spellings the front end can hand
    /// us share one implementation. The parser emits `process.exit(3)` as a
    /// METHOD CALL with `process` as a pseudo-variable receiver, not as a
    /// `Path`, so the path-keyed dispatch this was lifted out of never fired
    /// and the program died on the `Identifier("process")` arm of
    /// `eval_expr` with an "internal ... this is a compiler bug" diagnostic —
    /// on an accepted program that both compiled backends ran correctly.
    /// `codegen/method_call.rs` had already had to special-case the method
    /// shape and its comment says so; this is the interpreter catching up
    /// rather than a second, drifting copy.
    pub(crate) fn eval_process_exit(&mut self, args: &[CallArg]) -> Value {
        self.track_effect("panics");
        let code = if let Some(arg) = args.first() {
            match self.eval_expr_inner(&arg.value) {
                Value::Int(v) => v as i32,
                _ => 1,
            }
        } else {
            0
        };
        // Run all pending defers via ExitUnwind propagation.
        self.pending_cf = Some(ControlFlow::ExitUnwind { code });
        Value::Unit
    }

    pub(crate) fn eval_call(&mut self, callee: &Expr, args: &[CallArg], span: &Span) -> Value {
        // Comptime `Type` reflection in the path-call form: `MyType.fields()`,
        // `MyType.name()`, … parse as `Call(Path([Type, method]))`. The
        // typechecker has already validated this is a reflection call on a
        // known type at comptime; dispatch on the head segment as a `Type`
        // value. Substrate 2.
        if let ExprKind::Path { segments, .. } = &callee.kind {
            if segments.len() == 2 && Self::is_reflection_method_name(&segments[1]) {
                // Head names a concrete type directly (`Widget.fields()`).
                if self.is_known_type_name(&segments[0]) {
                    return self.eval_type_reflection(
                        &segments[0].clone(),
                        &segments[1].clone(),
                        args,
                        span,
                    );
                }
                // Head is a `comptime T: Type` parameter bound to a `Type`
                // pseudovalue in the current frame (`T.fields()`). For a
                // user-program `derive_*` the branch above already catches this
                // — the typechecker records the comptime param in this
                // program's type tables. A baked-stdlib `derive_*` (e.g.
                // `derive_message` for `#[derive(Message)]`) is typechecked
                // separately, so its `T` is absent there; recover it from the
                // bound value, which is a `TypeVal` regardless of definition
                // site.
                if let Some(Value::TypeVal(name)) = self.env.get(&segments[0]) {
                    return self.eval_type_reflection(
                        &name.clone(),
                        &segments[1].clone(),
                        args,
                        span,
                    );
                }
            }
        }

        // Comptime stdlib surface (substrate 3): `ast.expr(s)` quasi-quote
        // builder and `compiler.error(msg)` compile-time diagnostic. The
        // typechecker has validated these are comptime-only; dispatch here.
        if let ExprKind::Path { segments, .. } = &callee.kind {
            if segments.len() == 2 {
                match (segments[0].as_str(), segments[1].as_str()) {
                    ("ast", "expr") => return self.eval_ast_expr_builder(args, span),
                    ("ast", "item") => return self.eval_ast_item_builder(args, span),
                    ("compiler", "error") => return self.eval_compiler_error(args, span),
                    _ => {}
                }
            }
        }

        // Volatile MMIO intrinsics `volatile_read(p)` / `volatile_write(p, v)`
        // are codegen-only (they operate on raw pointers the interpreter cannot
        // model). Reject cleanly here rather than execute their `{ 0 }`
        // placeholder stub bodies and silently return a wrong value — the peer
        // of the `ptr.*` intercept in `eval_method_call` (B-2026-07-12-7).
        // Guarded on the name not being shadowed by a user binding.
        if let ExprKind::Identifier(name) = &callee.kind {
            if (name == "volatile_read" || name == "volatile_write") && self.env.get(name).is_none()
            {
                return self.record_runtime_error(
                    format!(
                        "MMIO intrinsic `{name}(..)` is only supported under `karac build` / \
                         the JIT (codegen), not the tree-walk interpreter — it operates on raw \
                         pointers the interpreter cannot model. Run without `--interp` (unset \
                         KARAC_RUN_JIT) to use the compiled backend."
                    ),
                    span,
                );
            }
        }

        // A FOREIGN IMPORT (`unsafe extern "C" { fn abs(n: i32) -> i32; }`).
        // The tree-walk interpreter has no FFI boundary — there is no library
        // loaded and no calling convention to cross — so this is a designed
        // limitation, not a failure.
        //
        // B-2026-08-06-24: without this arm the callee fell through to bare
        // identifier evaluation, found no binding, and reported "internal:
        // name 'abs' resolved but has no binding at run time. This is a
        // compiler bug (the resolver should have rejected or bound it) —
        // please report it with the source." Wrong on three counts: it calls a
        // deliberate design property internal, it tells the author of a
        // perfectly good program to file a compiler bug, and it blames the
        // resolver, which is right to bind a declared import. Every FFI
        // program hit it, and `karac run` is the first thing anyone reaching
        // for the FFI docs tries.
        //
        // The wording matches the sibling refusals on this same boundary —
        // `CStr.as_ptr()` (method_call_seq.rs), `CStr.from_ptr` below, and the
        // MMIO intrinsics just above — so the FFI surface answers with one
        // voice: name the construct, say why interpreted mode cannot do it,
        // point at `karac build`.
        //
        // Guarded on the name not being shadowed by a user binding, exactly
        // like the MMIO arm: a local closure named after an import is a real
        // (if unwise) program, and it should still run.
        //
        // Arguments are deliberately NOT evaluated first, where `CStr.from_ptr`
        // below does evaluate its one argument for effects. The difference is
        // which message the user should get: FFI arguments are themselves
        // usually pointer producers, so `strlen(msg.as_ptr())` would hit
        // `as_ptr`'s own refusal and report THAT — pointing at the argument
        // when the thing that cannot work is the call. Refusing first names the
        // construct the author actually wrote. The program is ending either
        // way, so no side effect is owed.
        if let ExprKind::Identifier(name) = &callee.kind {
            if self.env.get(name).is_none() && self.is_declared_extern_fn(name) {
                return self.record_runtime_error(
                    format!(
                        "`{name}(..)` is a foreign import (`extern`) and cannot be called \
                         under `karac run --interp`: the tree-walk interpreter has no FFI \
                         boundary — no foreign library is loaded and no calling convention \
                         is crossed. Compile with `karac build` (or run without `--interp`, \
                         which uses the JIT) instead."
                    ),
                    span,
                );
            }
        }

        // Layout-query intrinsics `size_of[T]()` / `align_of[T]()`
        // (design.md § Field Offsets family). Intercepted before normal
        // dispatch — like codegen's `compile_call` twin — so the `{ 0 }`
        // placeholder body in `runtime/stdlib/intrinsics.kara` is never
        // consulted and the `Call(Index(Ident, T))` parse shape doesn't
        // fall through to variable lookup (which panicked "variable
        // 'size_of' not found"). `offset_of` is a parser special form
        // (`ExprKind::OffsetOf`), handled in `eval_expr` instead.
        if args.is_empty() {
            if let Some((name, ty)) = Self::match_layout_query(callee) {
                let Some(ty) = ty else {
                    return self.record_runtime_error(
                        format!(
                            "{name} requires a plain type argument — call shape \
                             is `{name}[T]()`"
                        ),
                        span,
                    );
                };
                return self.eval_layout_query(&name, &ty, span);
            }
        }

        // `with_provider[R](provider, closure)` — surface for scoped provider
        // injection (design.md § Provider-Rooted Resources). Parses today as
        // `Call(Index(Ident("with_provider"), <R>), [provider, closure])`
        // because the current parser treats `[...]` at expression position as
        // indexing; we pattern-match that shape and extract the resource name
        // from the bracket operand. A future parser slice that recognizes
        // `IDENT[TYPE_ARGS](` as a generic call will feed through the same
        // intercept via the new Call shape.
        //
        // TODO(auto-traits): the typechecker should verify `Send + Sync` on
        // the concrete provider type `P` here — deferred until Kāra's
        // auto-trait / concurrency work lands. See
        // `docs/deferred.md § Send + Sync Enforcement on with_provider
        // Concrete Provider Type`. The single-threaded tree-walk interpreter
        // has no Send/Sync failure modes to catch until then.
        if let Some((resource, provider_expr, closure_expr)) =
            Self::match_with_provider(callee, args)
        {
            return self.eval_with_provider(&resource, provider_expr, closure_expr, span);
        }

        // Phase-8 line 153: `with_span(span, ||body)` runs the body with
        // `span` installed as the ambient active span, restoring the prior
        // one on exit. Mirrors `with_provider`'s closure-scoped shape.
        if let Some((span_expr, closure_expr)) = Self::match_with_span(callee, args) {
            return self.eval_with_span(span_expr, closure_expr, span);
        }

        // Phase-8 line 153: `tracing_active_span()` reads the ambient
        // active span id (0 = none). Intercept rather than run the
        // `#[compiler_builtin]` placeholder body (which returns 0) so the
        // active span installed by `with_span` is observed.
        if args.is_empty() {
            let is_active_span = match &callee.kind {
                ExprKind::Identifier(n) => n == "tracing_active_span",
                ExprKind::Path { segments, .. } => segments.as_slice() == ["tracing_active_span"],
                _ => false,
            };
            if is_active_span {
                return Value::Int(self.active_span_stack.last().copied().unwrap_or(0).into());
            }
        }

        // Phase-8 line 156 (codegen half): the rewritten `Log.*` bodies gate
        // on `tracing_level_enabled(rank)` and emit through
        // `tracing_emit_event(event)`, and `Log.set_min_level` / `Log.reset`
        // lower to `tracing_set_min_level` / `tracing_reset`. Under the
        // interpreter the `Log.*`-level config special-cases (drop below
        // threshold without evaluating the message; route to a registered
        // sink) are still handled by `try_eval_log_call` below — these
        // builtin handlers back the *default* fall-through (passing level,
        // no registered sink, where the `Log.*` body runs) and keep the
        // builtins consistent if invoked directly. They read/write the same
        // `tracing_min_level` / `tracing_exporter` state.
        if let Some(v) = self.try_eval_tracing_config_builtin(callee, args) {
            return v;
        }

        // Phase-8 line 156 (interpreter half): configurable ambient logging.
        // `Log.set_min_level` / `set_exporter` / `reset` write the ambient
        // state; `Log.{trace,debug,info,warn,error}` consult it (drop below
        // the min level, route to a registered sink). Returns `None` for the
        // default level-method case so the existing `Log.*` Kāra body runs
        // (the per-call `StdoutExporter` stdout path), keeping the common
        // path on the already-tested lowering.
        if let Some(v) = self.try_eval_log_call(callee, args) {
            return v;
        }

        // Effect-resource method call — `UserDB.query(...)` parses as
        // `Call(Path(["UserDB", "query"]), args)` because `starts_upper(&name)`
        // roots a Path in `parse_primary`. Dispatch through the provider
        // stack instead of normal path-call resolution when the head segment
        // names an `effect resource` (design.md § Provider-Rooted Resources).
        if let ExprKind::Path { segments, .. } = &callee.kind {
            if segments.len() == 2 && self.effect_resources.contains(&segments[0]) {
                return self.eval_resource_method(&segments[0], &segments[1], args, span);
            }
        }

        // `old(expr)` inside an `ensures` clause reads the pre-state snapshot
        // captured at function entry (design.md § Contracts rule 4). Keyed by
        // the arg's span on the top `old_snapshots` frame. Falls back to
        // evaluating the arg directly when no snapshot is active (defensive —
        // the typechecker restricts `old(...)` to `ensures` clauses).
        if let ExprKind::Identifier(n) = &callee.kind {
            if n == "old" && args.len() == 1 && self.env.get("old").is_none() {
                if let Some(snap) = self.old_snapshots.last() {
                    if let Some(v) =
                        snap.get(&crate::resolver::SpanKey::from_span(&args[0].value.span))
                    {
                        return v.clone();
                    }
                }
                return self.eval_expr_inner(&args[0].value);
            }
        }

        // Refinement construction: `Name.try_from(x)` runs the predicate at
        // runtime (phase-9 step 5b). Parses as `Call(Path([Name, try_from]))`
        // because an uppercase head segment roots a Path in `parse_primary`.
        // Returns `Ok(x)` / `Err(msg)`; `None` (not a refinement) falls
        // through to normal path-call dispatch below.
        if let ExprKind::Path { segments, .. } = &callee.kind {
            if segments.len() == 2 && segments[1] == "try_from" {
                if let Some(v) = self.eval_refinement_try_from(&segments[0], args) {
                    return v;
                }
                // `<C-like #[repr(intN)] enum>.try_from(v)` — design.md § Enum
                // Discriminant Runtime Surface (B-2026-08-21-26). The inbound
                // twin of `.discriminant()`, reading the SAME folded table
                // backwards, so a declared `Audio = BASE + 1` maps here
                // exactly as it reads there. Placed ahead of the numeric arm
                // because an enum name can never be a numeric target, and the
                // typechecker has already refused a payload enum or one with
                // no declared `#[repr]`.
                if let Some(disc) = self
                    .typecheck_result
                    .enum_discriminants
                    .get(&segments[0])
                    .cloned()
                {
                    let raw = match args.first().map(|a| self.eval_expr_inner(&a.value)) {
                        Some(Value::Int(n)) => n,
                        _ => 0,
                    };
                    return match disc.values.iter().find(|(_, v)| i128::from(*v) == raw) {
                        Some((variant, _)) => result_ok(Value::EnumVariant {
                            enum_name: segments[0].clone(),
                            variant: variant.clone(),
                            data: EnumData::Unit,
                        }),
                        None => Value::EnumVariant {
                            enum_name: "Result".to_string(),
                            variant: "Err".to_string(),
                            data: EnumData::Tuple(vec![Value::EnumVariant {
                                enum_name: "DiscriminantError".to_string(),
                                variant: "OutOfRange".to_string(),
                                data: EnumData::Struct(
                                    [("value".to_string(), Value::Int(raw))]
                                        .into_iter()
                                        .collect(),
                                ),
                            }]),
                        },
                    };
                }
                // Numeric narrowing `<int>.try_from(x)` in path form — the
                // `.try_into()` desugar (`x.try_into()` → `T.try_from(x)`)
                // lowers to this shape. Same range check + Result shape as the
                // identifier-form receiver arm in `method_call.rs`.
                if super::method_call::is_numeric_try_from_target(&segments[0]) {
                    let n = match args.first().map(|a| self.eval_expr_inner(&a.value)) {
                        Some(Value::Int(n)) => n,
                        _ => 0,
                    };
                    return super::method_call::numeric_try_from_value(
                        narrow_to_i64(n),
                        &segments[0],
                    );
                }
            }
        }

        // Fallible-allocation constructor companions (phase-8-stdlib-floor
        // item 2). `Vec.try_with_capacity(n)` / `Vec.try_from_slice(src)` /
        // `String.try_with_capacity(n)` run the base constructor and wrap the
        // result in `Result.Ok(_)` — the tree-walk host allocator never OOMs.
        // Recurse into the base constructor by rewriting the path's method
        // segment. Gated on the recognized `(collection, base)` pairs.
        if let ExprKind::Path { segments, .. } = &callee.kind {
            if segments.len() == 2 {
                if let Some(base) = crate::fallible_alloc::static_companion_base(&segments[1]) {
                    let coll = segments[0].as_str();
                    let recognized = match base {
                        "with_capacity" => matches!(coll, "Vec" | "VecDeque" | "String"),
                        "from_slice" => coll == "Vec",
                        // B-2026-08-26-27 — `Vec.try_from_iter`. See the
                        // dedicated arm below: it cannot use the rewrite-and-
                        // recurse path the others take, because its base
                        // `Vec.from_iter` has no interpreter rule of its own
                        // (`lowering.rs` eliminates it into `iter.collect()`
                        // before the interpreter ever sees it).
                        "from_iter" => false,
                        _ => false,
                    };
                    if recognized {
                        let mut base_callee = callee.clone();
                        if let ExprKind::Path { segments, .. } = &mut base_callee.kind {
                            segments[1] = base.to_string();
                        }
                        let base_val = self.eval_call(&base_callee, args, span);
                        return super::method_call::result_ok(base_val);
                    }
                }
            }
        }

        // B-2026-08-26-27 — `Vec.try_from_iter(it)`. Evaluated as
        // `it.collect()` wrapped in `Result.Ok`, which is the same shape the
        // companions above take and the same lowering its panicking base gets;
        // it needs its own arm only because that base is rewritten away before
        // the interpreter runs, so there is nothing to recurse into.
        //
        // Always `Ok` here, deliberately and consistently with every other
        // `try_*` companion in this backend: the tree-walk host allocator does
        // not OOM. Codegen is where the failure path is real — it grows the
        // accumulator through `karac_alloc_fallible` and returns
        // `Err(AllocError.OutOfMemory { requested_bytes })`.
        if let ExprKind::Path { segments, .. } = &callee.kind {
            if segments.len() == 2
                && segments[0] == "Vec"
                && segments[1] == "try_from_iter"
                && args.len() == 1
            {
                let collect_call = Expr {
                    span: *span,
                    kind: ExprKind::MethodCall {
                        object: Box::new(args[0].value.clone()),
                        method: "collect".to_string(),
                        turbofish: None,
                        args: Vec::new(),
                        args_close_span: *span,
                    },
                };
                let v = self.eval_expr_inner(&collect_call);
                return super::method_call::result_ok(v);
            }
        }

        // `Vector[T, N](lane0, …)` SIMD construction (design.md § Portable
        // SIMD, slice 1b). Parses as `Call(Path(["Vector"], generic_args))`.
        // The typechecker has already verified lane count == N and each lane's
        // type; the interpreter just evaluates each lane into a value-semantics
        // `Value::Vector`. Mirrors the codegen insertelement chain.
        if let ExprKind::Path {
            segments,
            generic_args: Some(_),
        } = &callee.kind
        {
            if segments.len() == 1 && segments[0] == "Vector" {
                let lanes: Vec<Value> = args
                    .iter()
                    .map(|a| self.eval_expr_inner(&a.value))
                    .collect();
                return Value::Vector(lanes);
            }
        }

        // `<numeric>.parse(s) -> Option[T]` in PATH-call form — the lowering
        // rewrite of the string-receiver `s.parse()` sugar (B-2026-07-19-5).
        // Mirrors the method-call parse in method_call.rs (int types produce
        // `Value::Int`, `f64` produces `Value::Float`); the method form
        // (`i64.parse(s)`) stays there, this handles the rewritten path form.
        if let ExprKind::Path { segments, .. } = &callee.kind {
            if segments.len() == 2 && segments[1] == "parse" {
                let ty = segments[0].as_str();
                let is_int = matches!(
                    ty,
                    "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64" | "usize" | "isize"
                );
                let is_f64 = ty == "f64";
                if is_int || is_f64 {
                    let none = || Value::EnumVariant {
                        enum_name: "Option".to_string(),
                        variant: "None".to_string(),
                        data: EnumData::Unit,
                    };
                    if let Some(arg) = args.first() {
                        if let Value::String(s) = self.eval_expr_inner(&arg.value) {
                            if is_int {
                                return match s.trim().parse::<i64>() {
                                    Ok(n) => Value::EnumVariant {
                                        enum_name: "Option".to_string(),
                                        variant: "Some".to_string(),
                                        data: EnumData::Tuple(vec![Value::Int(n.into())]),
                                    },
                                    Err(_) => none(),
                                };
                            }
                            return match s.trim().parse::<f64>() {
                                Ok(v) => Value::EnumVariant {
                                    enum_name: "Option".to_string(),
                                    variant: "Some".to_string(),
                                    data: EnumData::Tuple(vec![Value::Float(v)]),
                                },
                                Err(_) => none(),
                            };
                        }
                    }
                    return none();
                }
            }
        }

        // Built-in path-qualified functions (e.g. process.exit, Ordering.Relaxed, F64.from)
        if let ExprKind::Path { segments, .. } = &callee.kind {
            let path_str = segments.join(".");
            match path_str.as_str() {
                "process.exit" => return self.eval_process_exit(args),
                "Atomic.new" => {
                    let val = if let Some(arg) = args.first() {
                        self.eval_expr_inner(&arg.value)
                    } else {
                        Value::Int(0)
                    };
                    return Value::Atomic(Arc::new(Mutex::new(val)));
                }
                "Mutex.new" => {
                    let val = if let Some(arg) = args.first() {
                        self.eval_expr_inner(&arg.value)
                    } else {
                        Value::Int(0)
                    };
                    return Value::Mutex(Arc::new(Mutex::new(val)));
                }
                // `TaskGroup.new()` — the scope-local fan-out container
                // (design.md § Structured Concurrency / TaskGroup). Codegen
                // wires this to `karac_runtime_taskgroup_new`; the tree-walk
                // interpreter runs spawned children eagerly at each
                // `.spawn(closure)` site (see `eval_taskgroup_spawn`), so the
                // group is a stateless marker. Sibling of `Atomic.new` /
                // `Mutex.new` above. (B-2026-06-30-8 — run/build agreement.)
                "TaskGroup.new" => {
                    return Value::TaskGroup;
                }
                // Debugger Contract slice 5: `std.runtime` introspection
                // surface (`runtime/stdlib/runtime.kara`). The tree-walk
                // interpreter has its own par-block evaluation path and does
                // not construct `KaracFrame` / `ACTIVE_FRAMES` state, so all
                // three return the empty / false form per design.md's
                // try-then-degrade contract — generic tooling sees no frames
                // and falls back to an alternate code path. Real values flow
                // through the codegen-side dispatch in `compile_assoc_call`,
                // which calls into `karac_runtime_*` extern fns to read the
                // slice-3 globals + slice-4 active-frames registry.
                "Runtime.has_debug_metadata" => {
                    return Value::Bool(false);
                }
                "Runtime.list_par_blocks" | "Runtime.list_tasks" => {
                    return Value::array_of(Vec::new());
                }
                // Slice F (`std.json`): `Json.parse(s)` parses via
                // `serde_json` and builds a Kāra `Json` enum tree. The
                // runtime crate exposes the same impl through
                // `karac_runtime_json_parse` for the codegen path; the
                // interpreter calls `serde_json` directly to avoid the
                // FFI cross-over (both link the same crate). Returns
                // `Result[Json, JsonError]` per the signature in
                // `runtime/stdlib/json.kara`.
                "Json.parse" => {
                    let s = if let Some(arg) = args.first() {
                        match self.eval_expr_inner(&arg.value) {
                            Value::String(s) => s,
                            _ => String::new(),
                        }
                    } else {
                        String::new()
                    };
                    return match serde_json::from_str::<serde_json::Value>(&s) {
                        Ok(v) => Value::EnumVariant {
                            enum_name: "Result".to_string(),
                            variant: "Ok".to_string(),
                            data: EnumData::Tuple(vec![serde_json_to_kara_json(&v)]),
                        },
                        Err(e) => Value::EnumVariant {
                            enum_name: "Result".to_string(),
                            variant: "Err".to_string(),
                            data: EnumData::Tuple(vec![make_json_error(&e)]),
                        },
                    };
                }
                // `CStr.from_ptr(p: *const u8) -> ref CStr` — the inbound
                // raw-pointer constructor. The tree-walk interpreter has no
                // raw-pointer representation (the same reason `CStr.as_ptr()`
                // rejects in `method_call_seq.rs`), so a meaningful `len`
                // walk over `p` is impossible here. Evaluate the argument
                // for effects, then reject loudly at the producer rather than
                // fabricating a CStr from a value the interpreter cannot
                // model. Real values flow through `karac build` (codegen
                // lowers it to a libc `strlen` + `{ptr, len}` aggregate).
                "CStr.from_ptr" => {
                    if let Some(arg) = args.first() {
                        let _ = self.eval_expr_inner(&arg.value);
                    }
                    return self.record_runtime_error(
                        "CStr.from_ptr(...) is not supported under `karac run`: the tree-walk \
                         interpreter has no raw-pointer representation. Compile with \
                         `karac build` instead.",
                        span,
                    );
                }
                "Map.new" => {
                    return self.new_hash_container(Value::empty_map());
                }
                "Vec.new" => {
                    return Value::array_of(Vec::new());
                }
                // `String.new() -> String` — empty growable string. Wired
                // here because `String.new` has no syntactic stdlib
                // declaration; the typechecker special-cases the path the
                // same way (see `typechecker/expr_call.rs`). Without this
                // arm the call fell through to bare-path evaluation and
                // died on the unwired-path diagnostic. The three arms
                // below close the rest of that special-cased family —
                // every path the typechecker accepts via its String /
                // with_capacity special arms must have an evaluation rule
                // here or `karac run` faults at the call site.
                "String.new" => {
                    return Value::String(String::new());
                }
                // `String.with_capacity(n) -> String` — capacity is a
                // codegen-side allocation hint; at the Value layer every
                // observable behavior matches `String.new()`. The arg is
                // still evaluated for effects.
                "String.with_capacity" => {
                    if let Some(arg) = args.first() {
                        let _ = self.eval_expr_inner(&arg.value);
                    }
                    return Value::String(String::new());
                }
                // `String.from(x) -> String` — passthrough, mirroring the
                // codegen treatment (string literals / StringSlices /
                // Strings all arrive as `Value::String` here).
                "String.from" => {
                    if let Some(arg) = args.first() {
                        let v = self.eval_expr_inner(&arg.value);
                        if self.pending_cf.is_some() {
                            return v;
                        }
                        match v {
                            Value::String(s) => return Value::String(s),
                            // `From[char] for String` — a single `char` becomes
                            // a one-glyph owned String. Also the target of the
                            // `c.into()` desugar (`Call(Path([String, from]))`).
                            Value::Char(c) => return Value::String(c.to_string()),
                            _ => {
                                return self.record_runtime_error(
                                    "String.from expects a string or char argument",
                                    span,
                                );
                            }
                        }
                    }
                    return Value::String(String::new());
                }
                // `VecDeque.with_capacity(n) -> VecDeque[T]` — same
                // capacity-hint treatment as `Vec.with_capacity`; the
                // VecDeque runtime shape mirrors `Vec.new`'s storage (see
                // the `VecDeque.new` arm below), so the Vec helper is
                // reused verbatim.
                "VecDeque.with_capacity" => {
                    return self.eval_vec_with_capacity(args, span);
                }
                // `VecDeque.new() -> VecDeque[T]` — runtime shape mirrors
                // `Vec.new`'s shared `Arc<RwLock<Vec<Value>>>` storage.
                // Front-end ops (`push_front`/`pop_front`) translate to
                // `Vec::insert(0, …)` / `Vec::remove(0)` at the
                // method-dispatch layer (see `eval_method_call`'s `_front`
                // arms). The asymptotic O(n) cost is acceptable for the
                // tree-walk interpreter — perf-relevant workloads run
                // through codegen, where a real `VecDeque` lowering lands
                // as a peer slice.
                "VecDeque.new" => {
                    return Value::array_of(Vec::new());
                }
                // `Vec.filled(n: i64, val: T) -> Vec[T] where T: Clone` —
                // spec at design.md:1631. Routed through a helper so
                // its locals don't bloat the surrounding `eval_call`
                // match's debug-mode stack frame (the inline form
                // overflowed `test_e2e_fibonacci`, same shape as the
                // `and`/`or` short-circuit fix).
                "Vec.filled" => return self.eval_vec_filled(args, span),
                // `Vec.from_fn(n: i64, f: Fn(i64) -> T) -> Vec[T]` — the
                // index-driven constructor beside `filled` in design.md's
                // `Vec` table (B-2026-08-21-10). Same helper-frame treatment.
                "Vec.from_fn" => return self.eval_vec_from_fn(args, span),
                // `Vec.with_capacity(n: i64) -> Vec[T]` — empty Vec
                // (len=0) with pre-allocated capacity n. In the
                // tree-walk interpreter capacity is a hint to the
                // underlying `Vec<Value>` so subsequent pushes up to n
                // are realloc-free; every observable behavior matches
                // `Vec.new()`. Element type is erased at the Value layer
                // (matches `Vec.new`'s treatment).
                "Vec.with_capacity" => return self.eval_vec_with_capacity(args, span),
                // `Vec.from_slice(src) -> Vec[T]` — clone the source's
                // elements into a fresh Vec. Mirrors codegen's bulk-copy
                // shape; here the storage isn't shared so a fresh clone
                // of the inner Vec<Value> is correct (matches what the
                // `push_loop_from_iter` shape produces semantically).
                "Vec.from_slice" => {
                    let src = args
                        .first()
                        .map(|a| self.eval_expr_inner(&a.value))
                        .unwrap_or(Value::Unit);
                    let elements: Vec<Value> = match src {
                        Value::Array(rc) => rc.read().unwrap().clone(),
                        Value::Slice {
                            storage,
                            start,
                            len,
                            ..
                        } => storage.read().unwrap()[start..start + len].to_vec(),
                        _ => Vec::new(),
                    };
                    return Value::array_of(elements);
                }
                "SortedSet.new" => {
                    return Value::SortedSet(BTreeMap::new());
                }
                "SortedMap.new" => {
                    return Value::SortedMap(BTreeMap::new());
                }
                "Set.new" => {
                    return self.new_hash_container(Value::empty_set());
                }
                "Client.new" => {
                    return Value::Struct {
                        name: "Client".to_string(),
                        fields: HashMap::new(),
                    };
                }
                "Client.get" => {
                    let url = args
                        .first()
                        .map(|a| match self.eval_expr_inner(&a.value) {
                            Value::String(s) => s,
                            _ => String::new(),
                        })
                        .unwrap_or_default();
                    return eval_http_get(&url);
                }
                "Client.post" => {
                    let mut arg_iter = args.iter();
                    let url = arg_iter
                        .next()
                        .map(|a| match self.eval_expr_inner(&a.value) {
                            Value::String(s) => s,
                            _ => String::new(),
                        })
                        .unwrap_or_default();
                    let body = arg_iter
                        .next()
                        .map(|a| match self.eval_expr_inner(&a.value) {
                            Value::String(s) => s,
                            _ => String::new(),
                        })
                        .unwrap_or_default();
                    return eval_http_post(&url, &body);
                }
                "Channel.new" | "Channel.bounded" => {
                    // `Channel.bounded(cap)` is `Channel.new()` plus a queue
                    // bound (design.md § `Channel[T]` API; B-2026-08-22-16) —
                    // same `(Sender[T], Receiver[T])` pair, so one arm serves
                    // both. Capacity 0 marks the unbounded constructor.
                    let capacity = if path_str == "Channel.bounded" {
                        match args.first().map(|a| self.eval_expr_inner(&a.value)) {
                            Some(Value::Int(n)) if n > 0 => n as usize,
                            // `requires cap > 0` is a typecheck-time contract,
                            // enforced there for a literal. A non-literal that
                            // turns out non-positive at runtime cannot silently
                            // become "unbounded", so it is reported.
                            _ => {
                                return self.record_runtime_error(
                                    "`Channel.bounded` requires a capacity greater than 0"
                                        .to_string(),
                                    span,
                                );
                            }
                        }
                    } else {
                        0
                    };
                    let buf = crate::interpreter::value::ChannelBuf::new(capacity);
                    let sender = Value::Sender(crate::interpreter::value::SenderHandle::new(
                        Arc::clone(&buf),
                    ));
                    let receiver =
                        Value::Receiver(crate::interpreter::value::ReceiverHandle::new(buf));
                    return Value::Tuple(vec![sender, receiver]);
                }
                "File.open" | "File.create" | "File.append" => {
                    // Phase 8 slice F1: stateful file I/O constructors.
                    // Each routes through the corresponding std::fs::File
                    // open mode (read-only / write+truncate / append).
                    // Errors map through `io_error_from_std` to IoError
                    // variants; success wraps the `Arc<Mutex<File>>` in
                    // `Value::File`. `reads(FileSystem)` /
                    // `writes(FileSystem)` is tracked per arm.
                    let path = match args.first() {
                        Some(arg) => match self.eval_expr_inner(&arg.value) {
                            Value::String(s) => s,
                            _ => {
                                return self.record_runtime_error(
                                    format!("{path_str} expects a String path"),
                                    span,
                                );
                            }
                        },
                        None => {
                            return self.record_runtime_error(
                                format!("{path_str} expects a String path"),
                                span,
                            );
                        }
                    };
                    use super::helpers::{io_err_value, io_error_from_std, io_ok};
                    let mut opts = std::fs::OpenOptions::new();
                    match path_str.as_str() {
                        "File.open" => {
                            self.track_effect("reads(FileSystem)");
                            opts.read(true);
                        }
                        "File.create" => {
                            self.track_effect("writes(FileSystem)");
                            opts.write(true).create(true).truncate(true);
                        }
                        "File.append" => {
                            self.track_effect("writes(FileSystem)");
                            opts.append(true).create(true);
                        }
                        _ => unreachable!(),
                    }
                    return match opts.open(&path) {
                        Ok(f) => io_ok(Value::File(Arc::new(Mutex::new(f)))),
                        Err(e) => io_err_value(io_error_from_std(&e)),
                    };
                }
                "BufReader.new" | "BufReader.with_capacity" => {
                    // Phase 8 `BufReader[R]` slice: wrap a `File` reader with
                    // a buffered `std::io::BufReader`. The wrapped reader `R`
                    // is concretely `File` at v1. To give the BufReader an
                    // owned reader while leaving the original `File` value
                    // usable, we `try_clone` (dup) the underlying fd — the
                    // clone shares the OS file offset, so reads through the
                    // BufReader resume from wherever the File last left off.
                    // Construction performs no observable read, so no effect
                    // is tracked here (the read methods carry it).
                    let reader_val = match args.first() {
                        Some(arg) => self.eval_expr_inner(&arg.value),
                        None => {
                            return self.record_runtime_error(
                                format!("{path_str} expects a File reader argument"),
                                span,
                            );
                        }
                    };
                    let file_arc = match reader_val {
                        Value::File(arc) => arc,
                        other => {
                            return self.record_runtime_error(
                                format!(
                                    "{path_str} expects a File reader, got `{}`",
                                    other.variant_name()
                                ),
                                span,
                            );
                        }
                    };
                    // Default 8 KiB buffer for `new`; explicit capacity for
                    // `with_capacity` (a non-positive value falls back to the
                    // default rather than erroring — matches the permissive
                    // interpreter posture).
                    let cap = if path_str == "BufReader.with_capacity" {
                        match args.get(1).map(|a| self.eval_expr_inner(&a.value)) {
                            Some(Value::Int(n)) if n > 0 => n as usize,
                            _ => 8192,
                        }
                    } else {
                        8192
                    };
                    let cloned = {
                        let guard = file_arc.lock().unwrap();
                        guard.try_clone()
                    };
                    return match cloned {
                        Ok(f) => Value::BufReader(Arc::new(Mutex::new(
                            std::io::BufReader::with_capacity(cap, f),
                        ))),
                        Err(e) => self.record_runtime_error(
                            format!("{path_str}: failed to clone file handle: {e}"),
                            span,
                        ),
                    };
                }
                "BufWriter.new" | "BufWriter.with_capacity" => {
                    // Phase 8 `BufWriter[W]` slice (Write-side peer of
                    // `BufReader`): wrap a `File` writer with a buffered
                    // `std::io::BufWriter`. The wrapped writer `W` is
                    // concretely `File` at v1. As with `BufReader.new`, we
                    // `try_clone` (dup) the underlying fd so the BufWriter
                    // owns its writer while the original `File` value stays
                    // usable — the clone shares the OS file offset, so writes
                    // through the BufWriter land wherever the File last left
                    // off. Construction performs no observable write, so no
                    // effect is tracked here (the write methods carry it).
                    let writer_val = match args.first() {
                        Some(arg) => self.eval_expr_inner(&arg.value),
                        None => {
                            return self.record_runtime_error(
                                format!("{path_str} expects a File writer argument"),
                                span,
                            );
                        }
                    };
                    let file_arc = match writer_val {
                        Value::File(arc) => arc,
                        other => {
                            return self.record_runtime_error(
                                format!(
                                    "{path_str} expects a File writer, got `{}`",
                                    other.variant_name()
                                ),
                                span,
                            );
                        }
                    };
                    // Default 8 KiB buffer for `new`; explicit capacity for
                    // `with_capacity` (a non-positive value falls back to the
                    // default rather than erroring — matches the permissive
                    // interpreter posture, mirroring `BufReader`).
                    let cap = if path_str == "BufWriter.with_capacity" {
                        match args.get(1).map(|a| self.eval_expr_inner(&a.value)) {
                            Some(Value::Int(n)) if n > 0 => n as usize,
                            _ => 8192,
                        }
                    } else {
                        8192
                    };
                    let cloned = {
                        let guard = file_arc.lock().unwrap();
                        guard.try_clone()
                    };
                    return match cloned {
                        Ok(f) => Value::BufWriter(Arc::new(Mutex::new(
                            std::io::BufWriter::with_capacity(cap, f),
                        ))),
                        Err(e) => self.record_runtime_error(
                            format!("{path_str}: failed to clone file handle: {e}"),
                            span,
                        ),
                    };
                }
                "F32.from" => {
                    let val = if let Some(arg) = args.first() {
                        match self.eval_expr_inner(&arg.value) {
                            Value::Float(v) => v as f32,
                            Value::Int(v) => v as f32,
                            _ => 0.0,
                        }
                    } else {
                        0.0
                    };
                    return Value::TotalFloat32(super::helpers::canonical_wrapper_f32(val));
                }
                "F64.from" => {
                    let val = if let Some(arg) = args.first() {
                        match self.eval_expr_inner(&arg.value) {
                            Value::Float(v) => v,
                            Value::Int(v) => v as f64,
                            _ => 0.0,
                        }
                    } else {
                        0.0
                    };
                    return Value::TotalFloat64(super::helpers::canonical_wrapper_f64(val));
                }
                "F16.from" => {
                    // Stored promoted to f64 (the tree-walk interpreter has no
                    // native 16-bit float — same f64-promotion posture as the
                    // f16 primitive; the compiled path is exact half precision).
                    let val = if let Some(arg) = args.first() {
                        match self.eval_expr_inner(&arg.value) {
                            Value::Float(v) => v,
                            Value::Int(v) => v as f64,
                            _ => 0.0,
                        }
                    } else {
                        0.0
                    };
                    return Value::TotalFloat16(super::helpers::canonical_wrapper_f64(val));
                }
                "Bf16.from" => {
                    let val = if let Some(arg) = args.first() {
                        match self.eval_expr_inner(&arg.value) {
                            Value::Float(v) => v,
                            Value::Int(v) => v as f64,
                            _ => 0.0,
                        }
                    } else {
                        0.0
                    };
                    return Value::TotalBFloat16(super::helpers::canonical_wrapper_f64(val));
                }
                "Regex.compile" => {
                    let pattern = if let Some(arg) = args.first() {
                        match self.eval_expr_inner(&arg.value) {
                            Value::String(s) => s,
                            _ => String::new(),
                        }
                    } else {
                        String::new()
                    };
                    match RustRegex::new(&pattern) {
                        Ok(_) => {
                            let mut fields = HashMap::new();
                            fields.insert("pattern".to_string(), Value::String(pattern));
                            let regex_val = Value::Struct {
                                name: "Regex".to_string(),
                                fields,
                            };
                            return Value::EnumVariant {
                                enum_name: "Result".to_string(),
                                variant: "Ok".to_string(),
                                data: EnumData::Tuple(vec![regex_val]),
                            };
                        }
                        Err(e) => {
                            let mut fields = HashMap::new();
                            fields.insert("message".to_string(), Value::String(e.to_string()));
                            let err_val = Value::Struct {
                                name: "RegexError".to_string(),
                                fields,
                            };
                            return Value::EnumVariant {
                                enum_name: "Result".to_string(),
                                variant: "Err".to_string(),
                                data: EnumData::Tuple(vec![err_val]),
                            };
                        }
                    }
                }
                "Pool.new" => {
                    if let Some(v) = self.eval_pool_new(args) {
                        return v;
                    }
                }
                "Arena.new" => {
                    if let Some(v) = self.eval_arena_new(args) {
                        return v;
                    }
                }
                "Interner.new" => {
                    if let Some(v) = self.eval_interner_new(args) {
                        return v;
                    }
                }
                // `LazyLock.new(|| ...)` — store the closure against a fresh
                // handle and hand back an UNFILLED cell. The closure runs on
                // the first `get`, not here (B-2026-08-26-3).
                "LazyLock.new" => {
                    if let Some(v) = self.eval_lazy_new(args) {
                        return v;
                    }
                }
                "OnceLock.new" => {
                    if let Some(v) = self.eval_once_new("OnceLock") {
                        return v;
                    }
                }
                "OnceCell.new" => {
                    if let Some(v) = self.eval_once_new("OnceCell") {
                        return v;
                    }
                }
                // Phase-11 Tensor constructors (interpreter MVP) — see
                // runtime/stdlib/tensor.kara for the fill-type note.
                "Tensor.zeros" | "Tensor.ones" | "Tensor.full" => {
                    if let Some(v) = self.eval_tensor_new(&path_str, args, span) {
                        return v;
                    }
                }
                // Literal constructor — dims from the argument's syntactic
                // nesting (the walk is total: it returns a Value or a
                // recorded runtime error, never falls through).
                "Tensor.from" => {
                    return self.eval_tensor_from(args, span);
                }
                // Phase-11 Arrow IPC — parse an `arrow.fixed_shape_tensor`
                // stream (the inverse of `t.to_arrow_ipc()`): shape from the
                // field's extension metadata, values from the FixedSizeList
                // storage. A stream carrying no shape metadata reads as 1-D
                // over the flattened values. See `src/interpreter/arrow_ipc.rs`.
                "Tensor.from_arrow_ipc" => {
                    let bytes = match args.first() {
                        Some(arg) => {
                            let v = self.eval_expr_inner(&arg.value);
                            match super::method_call_column::value_to_bytes(&v) {
                                Some(b) => b,
                                None => {
                                    return self.record_runtime_error(
                                        "Tensor.from_arrow_ipc expects a Vec[u8] byte buffer",
                                        span,
                                    );
                                }
                            }
                        }
                        None => {
                            return self.record_runtime_error(
                                "Tensor.from_arrow_ipc expects a Vec[u8] byte buffer",
                                span,
                            );
                        }
                    };
                    return match super::arrow_ipc::tensor_from_ipc(&bytes) {
                        Ok((dims, data)) => {
                            // B-2026-07-28-10, tensor face: the stream carries
                            // its own dims, and nothing in the argument says
                            // what the binding declared. Codegen reconciles the
                            // two and traps on a mismatch; without the same
                            // check here `let bad: Tensor[i64, [3, 2]] =
                            // Tensor.from_arrow_ipc(<a [2,3] stream>)` silently
                            // produced a [2,3] tensor under `karac run`.
                            if let Some(err) = self.tensor_ann_shape_mismatch(&dims) {
                                return self.record_runtime_error(err, span);
                            }
                            Value::Tensor {
                                dims: std::sync::Arc::new(dims),
                                data: std::sync::Arc::new(std::sync::RwLock::new(data)),
                                // The IPC stream decodes to f64 slots; no
                                // narrowing claim to carry. B-2026-08-05-31.
                                elem: crate::interpreter::value::TensorElemWidth::F64,
                            }
                        }
                        Err(msg) => self.record_runtime_error(msg, span),
                    };
                }
                // Phase-11 Column constructors (interpreter MVP) — see
                // runtime/stdlib/column.kara.
                "Column.new"
                | "Column.with_capacity"
                | "Column.from_vec"
                | "Column.from_iter_nullable"
                | "Column.from_arrow_ipc" => {
                    if let Some(v) = self.eval_column_new(&path_str, args, span) {
                        return v;
                    }
                }
                // Phase-11 DataFrame constructor (interpreter MVP) — see
                // runtime/stdlib/dataframe.kara.
                "DataFrame.new" => {
                    if let Some(v) = self.eval_dataframe_new(&path_str) {
                        return v;
                    }
                }
                // Lazy expression column reference (phase-11 LazyDataFrame
                // slice 2): `LazyExpr.col("age")` — the root constructor of
                // every predicate tree (std.lazy's free `col` delegates
                // here). Resolution against visible columns happens at
                // collect, so construction never fails.
                "LazyExpr.col" => {
                    let name = match args.first() {
                        Some(arg) => match self.eval_expr_inner(&arg.value) {
                            Value::String(s) => s,
                            _ => {
                                return self.record_runtime_error(
                                    "LazyExpr.col expects a String column name",
                                    span,
                                );
                            }
                        },
                        None => {
                            return self.record_runtime_error(
                                "LazyExpr.col expects a String column name",
                                span,
                            );
                        }
                    };
                    return Value::LazyExpr(std::sync::Arc::new(
                        crate::interpreter::value::LazyExprIR::Col(name),
                    ));
                }
                // Literal expression constructor (phase-11 LazyDataFrame
                // slice 6): `LazyExpr.lit(true)` / std.lazy's free `lit` —
                // the constant-folding trigger. A runtime-constant flag
                // baked into a predicate folds at plan time.
                "LazyExpr.lit" => {
                    use crate::interpreter::value::LazyExprIR;
                    let Some(arg) = args.first() else {
                        return self.record_runtime_error(
                            "LazyExpr.lit expects a scalar literal (i64 / f64 / String / bool)",
                            span,
                        );
                    };
                    let ir = match self.eval_expr_inner(&arg.value) {
                        Value::Int(n) => LazyExprIR::LitInt(narrow_to_i64(n)),
                        Value::Float(v) => LazyExprIR::LitFloat(v),
                        Value::String(s) => LazyExprIR::LitStr(s),
                        Value::Bool(b) => LazyExprIR::LitBool(b),
                        other => {
                            return self.record_runtime_error(
                                format!(
                                    "LazyExpr.lit expects a scalar literal (i64 / f64 / \
                                     String / bool), got {}",
                                    other.variant_name()
                                ),
                                span,
                            );
                        }
                    };
                    return Value::LazyExpr(std::sync::Arc::new(ir));
                }
                // Phase-11 CSV leg slice 2: parse a CSV file into a table
                // (the inverse of `df.write_csv`). Read errors map through
                // `io_error_from_std`; parse errors (ragged rows, empty
                // file, unterminated quote) surface as `IoError.Other` with
                // the parser's message.
                "DataFrame.read_csv" => {
                    let path = match args.first() {
                        Some(arg) => match self.eval_expr_inner(&arg.value) {
                            Value::String(s) => s,
                            _ => {
                                return self.record_runtime_error(
                                    "DataFrame.read_csv expects a String path",
                                    span,
                                );
                            }
                        },
                        None => {
                            return self.record_runtime_error(
                                "DataFrame.read_csv expects a String path",
                                span,
                            );
                        }
                    };
                    self.track_effect("reads(FileSystem)");
                    use super::helpers::{io_err_value, io_error_from_std, io_ok};
                    let text = match std::fs::read_to_string(&path) {
                        Ok(t) => t,
                        Err(e) => return io_err_value(io_error_from_std(&e)),
                    };
                    return match super::method_call_dataframe::parse_csv_to_dataframe(&text) {
                        Ok(df) => io_ok(df),
                        Err(msg) => io_err_value(Value::EnumVariant {
                            enum_name: "IoError".to_string(),
                            variant: "Other".to_string(),
                            data: crate::interpreter::value::EnumData::Tuple(vec![Value::String(
                                msg,
                            )]),
                        }),
                    };
                }
                // Phase-11 Arrow IPC — parse a `Vec[u8]` IPC stream into a table
                // (the inverse of `df.to_arrow_ipc`): one Column per field, names
                // and per-column types from the batch schema. Pure in-memory (no
                // FileSystem effect, unlike read_csv); an arrow-side failure
                // surfaces as an ordinary runtime error. See
                // `src/interpreter/arrow_ipc.rs`.
                "DataFrame.from_arrow_ipc" => {
                    let bytes = match args.first() {
                        Some(arg) => {
                            let v = self.eval_expr_inner(&arg.value);
                            match super::method_call_column::value_to_bytes(&v) {
                                Some(b) => b,
                                None => {
                                    return self.record_runtime_error(
                                        "DataFrame.from_arrow_ipc expects a Vec[u8] byte buffer",
                                        span,
                                    );
                                }
                            }
                        }
                        None => {
                            return self.record_runtime_error(
                                "DataFrame.from_arrow_ipc expects a Vec[u8] byte buffer",
                                span,
                            );
                        }
                    };
                    return match super::arrow_ipc::dataframe_from_ipc(&bytes) {
                        Ok(cols) => {
                            let columns: Vec<(String, Value)> = cols
                                .into_iter()
                                .map(|(name, data, valid)| {
                                    (
                                        name,
                                        Value::Column {
                                            data: std::sync::Arc::new(std::sync::RwLock::new(data)),
                                            valid: std::sync::Arc::new(std::sync::RwLock::new(
                                                valid,
                                            )),
                                        },
                                    )
                                })
                                .collect();
                            Value::DataFrame {
                                columns: std::sync::Arc::new(std::sync::RwLock::new(columns)),
                            }
                        }
                        Err(msg) => self.record_runtime_error(msg, span),
                    };
                }
                "Semaphore.new" => {
                    if let Some(v) = self.eval_semaphore_new(args) {
                        return v;
                    }
                }
                "RateLimiter.new_token_bucket" => {
                    if let Some(v) = self.eval_rate_limiter_new(args) {
                        return v;
                    }
                }
                "BoundedChannel.new" => {
                    if let Some(v) = self.eval_bounded_channel_new(args) {
                        return v;
                    }
                }
                "Stats.sum" | "Stats.prod" | "Stats.mean" | "Stats.variance" | "Stats.stddev"
                | "Stats.median" | "Stats.min" | "Stats.max" | "Stats.percentile"
                | "Stats.argmin" | "Stats.argmax" | "Stats.sort" | "Stats.argsort" => {
                    let elems: Vec<Value> = if let Some(arg) = args.first() {
                        match self.eval_expr_inner(&arg.value) {
                            Value::Array(rc) => rc.read().unwrap().clone(),
                            // A `Slice[T]` argument (`Stats.mean(v.as_slice())`,
                            // or any borrowed sub-window) views `storage[start..
                            // start+len]`. Without this arm a non-empty slice
                            // fell to the `_ => vec![]` empty case, so every
                            // `Stats.*` on a slice read ZERO elements — `sum` a
                            // spurious 0/-0, `mean`/`median`/… a panic — while
                            // codegen read the slice correctly (a run-vs-build
                            // divergence, B-2026-07-18-12). The declared param
                            // type is `ref Slice[f64]`, so a Slice arg is the
                            // canonical form.
                            Value::Slice {
                                storage,
                                start,
                                len,
                                ..
                            } => storage.read().unwrap()[start..start + len].to_vec(),
                            _ => vec![],
                        }
                    } else {
                        vec![]
                    };
                    // Element kind (S5): the static i64/f64 decision comes
                    // from the typechecker's recorded ARG type (so an EMPTY
                    // `Vec[i64]` still gets the integer identities — `sum`
                    // 0, not the float `-0.0`); without type info (`karac
                    // run` executes despite typecheck errors) fall back to
                    // value inspection: non-empty and all-Int → integer.
                    let static_int = args.first().and_then(|arg| {
                        let key =
                            crate::resolver::SpanKey(arg.value.span.offset, arg.value.span.length);
                        let ty = self.typecheck_result.expr_types.get(&key)?;
                        let core = match ty {
                            crate::typechecker::Type::Ref(inner)
                            | crate::typechecker::Type::MutRef(inner) => inner.as_ref(),
                            other => other,
                        };
                        let elem = match core {
                            crate::typechecker::Type::Named { name, args }
                                if name == "Vec" && args.len() == 1 =>
                            {
                                &args[0]
                            }
                            crate::typechecker::Type::Slice { element, .. } => element.as_ref(),
                            crate::typechecker::Type::Array { element, .. } => element.as_ref(),
                            _ => return None,
                        };
                        match elem {
                            crate::typechecker::Type::Int(crate::typechecker::IntSize::I64) => {
                                Some(true)
                            }
                            crate::typechecker::Type::Float(crate::typechecker::FloatSize::F64) => {
                                Some(false)
                            }
                            _ => None,
                        }
                    });
                    let int_mode = static_int.unwrap_or_else(|| {
                        !elems.is_empty() && elems.iter().all(|v| matches!(v, Value::Int(_)))
                    });
                    // `percentile(xs, p)` reads its second argument; every
                    // other `Stats` function is unary.
                    let p = match args.get(1) {
                        Some(arg) => match self.eval_expr_inner(&arg.value) {
                            Value::Float(f) => Some(f),
                            Value::Int(i) => Some(i as f64),
                            _ => None,
                        },
                        None => None,
                    };
                    // B-2026-08-19-20 — refusals go through the ordinary
                    // runtime-error channel, so `karac run` reports an empty
                    // `Stats.mean()` the way `karac build` does (a Kara-level
                    // diagnostic with a span, exit 1) instead of a raw
                    // `panic!` with a Rust backtrace and exit 101. Both element
                    // axes consult the one policy function, so int and f64
                    // cannot drift apart on the message.
                    if let Some(msg) = crate::interpreter::helpers::stats_trap_message(
                        &path_str,
                        elems.is_empty(),
                        p,
                    ) {
                        return self.record_runtime_error(msg, span);
                    }
                    if int_mode {
                        let xs: Vec<i64> = elems
                            .iter()
                            .map(|v| match v {
                                Value::Int(i) => narrow_to_i64(*i),
                                Value::Float(f) => *f as i64,
                                _ => 0,
                            })
                            .collect();
                        // B-2026-08-19-25 — the overflow trap comes back as
                        // `Err` (it is only discovered inside the fold, unlike
                        // the empty-input refusals pre-checked above) and is
                        // reported on the same channel, so `karac run` matches
                        // `karac build`'s `integer overflow` at exit 1 instead
                        // of a Rust backtrace at exit 101.
                        return match eval_stats_fn_int(&path_str, &xs, p) {
                            Ok(v) => v,
                            Err(msg) => self.record_runtime_error(msg, span),
                        };
                    }
                    let xs: Vec<f64> = elems
                        .iter()
                        .map(|v| match v {
                            Value::Float(f) => *f,
                            Value::Int(i) => *i as f64,
                            _ => 0.0,
                        })
                        .collect();
                    return eval_stats_fn(&path_str, &xs, p);
                }
                // `String.from_utf8(bytes: Vec[u8]) -> Result[String, Utf8Error]`.
                // UTF-8-validating String constructor. Error variant mapping
                // follows Rust's `std::str::Utf8Error::error_len()` shape:
                // `None` means the byte stream is a truncated multi-byte
                // sequence (`IncompleteSequence`); `Some(_)` means the byte
                // at `valid_up_to` is an invalid lead/continuation byte
                // (`InvalidByte`). The `Other(String)` variant exists for
                // forward-compatibility with future failure modes — none
                // are produced by this path today.
                "String.from_utf8" => {
                    let to_byte = |v: &Value| match v {
                        Value::Int(i) => *i as u8,
                        _ => 0,
                    };
                    let bytes: Vec<u8> = if let Some(arg) = args.first() {
                        match self.eval_expr_inner(&arg.value) {
                            Value::Array(rc) => rc.read().unwrap().iter().map(to_byte).collect(),
                            // B-2026-08-14-20 — the parameter is signed
                            // `Slice[u8]` now, so `String.from_utf8(s.bytes())`
                            // arrives as a borrowed VIEW. Without this arm it
                            // fell to the `_` below and produced `Ok("")` — a
                            // silent empty String for a well-formed round trip,
                            // which is worse than the typecheck error the
                            // widening removed.
                            Value::Slice {
                                storage,
                                start,
                                len,
                                ..
                            } => storage.read().unwrap()[start..start + len]
                                .iter()
                                .map(to_byte)
                                .collect(),
                            _ => Vec::new(),
                        }
                    } else {
                        Vec::new()
                    };
                    return match std::str::from_utf8(&bytes) {
                        Ok(s) => Value::EnumVariant {
                            enum_name: "Result".to_string(),
                            variant: "Ok".to_string(),
                            data: EnumData::Tuple(vec![Value::String(s.to_string())]),
                        },
                        Err(e) => {
                            let variant = match e.error_len() {
                                None => "IncompleteSequence",
                                Some(_) => "InvalidByte",
                            };
                            Value::EnumVariant {
                                enum_name: "Result".to_string(),
                                variant: "Err".to_string(),
                                data: EnumData::Tuple(vec![Value::EnumVariant {
                                    enum_name: "Utf8Error".to_string(),
                                    variant: variant.to_string(),
                                    data: EnumData::Unit,
                                }]),
                            }
                        }
                    };
                }
                // `StableHash.siphash24(bytes, k0, k1) -> u64` (B-2026-08-25-22)
                // — the interpreted twin of the `karac_stable_siphash24`
                // lowering in `codegen/assoc_call.rs`. BOTH call
                // `karac_hash::siphash24`, so the two backends agree by
                // construction; that is the entire point of a digest sold as
                // stable, and it is why neither side open-codes the
                // permutation.
                //
                // Unlike the encoding arm below, a shape this does not
                // recognise is a hard error rather than an empty input. A
                // digest that silently hashes `""` is the worst possible
                // failure for this function: it returns a plausible `u64`, the
                // caller writes it into a content address or an on-disk index,
                // and every distinct input collides. Failing loudly is the
                // only safe default here.
                "StableHash.siphash24" => {
                    // B-2026-08-02-13 class — stdlib and user types share one
                    // flat namespace, so a program declaring its own
                    // `struct StableHash` reaches this arm and gets the BUILT-IN
                    // digest instead of its own method body. Measured: a user
                    // `siphash24` returning `42` printed the SipHash value on
                    // both backends. That is the worst shape of this bug for a
                    // hash function (a plausible number nobody asked for), so
                    // refuse and name the fix. `codegen/assoc_call.rs` refuses
                    // the same program through `reject_shadowed_prelude_types`,
                    // so `karac run` and `karac build` agree.
                    if self
                        .program
                        .items
                        .iter()
                        .any(|i| matches!(i, Item::StructDef(s) if s.name == "StableHash"))
                    {
                        return self.record_runtime_error(
                            "this program declares its own `struct StableHash`, which shadows \
                             the built-in stdlib namespace of that name, so \
                             `StableHash.siphash24(..)` would run the built-in digest rather \
                             than your method. Rename the user-defined struct (e.g. \
                             `MyStableHash`). Stdlib and user types currently share one \
                             namespace — tracked as B-2026-08-02-13."
                                .to_string(),
                            span,
                        );
                    }
                    let bytes = match args.first().map(|a| self.eval_expr_inner(&a.value)) {
                        Some(Value::Array(rc)) => value_bytes(&rc.read().unwrap()),
                        Some(Value::Slice {
                            storage,
                            start,
                            len,
                            ..
                        }) => value_bytes(&storage.read().unwrap()[start..start + len]),
                        other => {
                            return self.record_runtime_error(
                                format!(
                                    "internal: StableHash.siphash24 expected a Slice[u8] or \
                                     Vec[u8] first argument, got {}",
                                    match &other {
                                        Some(v) => v.variant_name(),
                                        None => "no argument",
                                    }
                                ),
                                span,
                            );
                        }
                    };
                    let mut key = [0u64; 2];
                    for (slot, arg) in key.iter_mut().zip(args.iter().skip(1)) {
                        match self.eval_expr_inner(&arg.value) {
                            Value::Int(i) => *slot = i as u64,
                            other => {
                                return self.record_runtime_error(
                                    format!(
                                        "internal: StableHash.siphash24 expected an integer key \
                                         half, got {}",
                                        other.variant_name()
                                    ),
                                    span,
                                );
                            }
                        }
                    }
                    return Value::Int(karac_hash::siphash24(&bytes, key[0], key[1]) as i128);
                }
                "Base64.encode" | "Base64.encode_url_safe" | "Hex.encode" | "Hex.encode_upper" => {
                    let to_bytes = |vals: &[Value]| -> Vec<u8> {
                        vals.iter()
                            .map(|v| match v {
                                Value::Int(i) => *i as u8,
                                _ => 0,
                            })
                            .collect()
                    };
                    let bytes: Vec<u8> = if let Some(arg) = args.first() {
                        match self.eval_expr_inner(&arg.value) {
                            Value::Array(rc) => to_bytes(&rc.read().unwrap()),
                            // A `Slice[u8]` argument (`Base64.encode(v.as_slice())`,
                            // the declared `Slice[u8]` param's canonical form)
                            // views `storage[start..start+len]`. Without this arm a
                            // non-empty slice fell to the empty case, so encoding a
                            // slice produced "" while a Vec arg read the real bytes
                            // — the same run-vs-build/interp class as the
                            // Stats-on-slice bug (B-2026-07-18-12).
                            Value::Slice {
                                storage,
                                start,
                                len,
                                ..
                            } => to_bytes(&storage.read().unwrap()[start..start + len]),
                            _ => Vec::new(),
                        }
                    } else {
                        Vec::new()
                    };
                    let s = match path_str.as_str() {
                        "Base64.encode" => base64_encode(&bytes, false),
                        "Base64.encode_url_safe" => base64_encode(&bytes, true),
                        "Hex.encode" => hex_encode(&bytes, false),
                        "Hex.encode_upper" => hex_encode(&bytes, true),
                        _ => unreachable!(),
                    };
                    return Value::String(s);
                }
                "Base64.decode" | "Hex.decode" | "Url.encode" | "Url.decode" => {
                    let s = if let Some(arg) = args.first() {
                        match self.eval_expr_inner(&arg.value) {
                            Value::String(s) => s,
                            _ => String::new(),
                        }
                    } else {
                        String::new()
                    };
                    return match path_str.as_str() {
                        "Base64.decode" => match base64_decode(&s) {
                            Ok(b) => decode_ok_bytes(b),
                            Err(m) => decode_err(m),
                        },
                        "Hex.decode" => match hex_decode(&s) {
                            Ok(b) => decode_ok_bytes(b),
                            Err(m) => decode_err(m),
                        },
                        "Url.encode" => Value::String(url_encode(&s)),
                        "Url.decode" => match url_decode(&s) {
                            Ok(out) => decode_ok_string(out),
                            Err(m) => decode_err(m),
                        },
                        _ => unreachable!(),
                    };
                }
                _ => {
                    // Check for Ordering / MemoryOrdering qualified-variant pattern
                    if segments.len() == 2
                        && (segments[0] == "Ordering" || segments[0] == "MemoryOrdering")
                    {
                        return Value::EnumVariant {
                            enum_name: segments[0].clone(),
                            variant: segments[1].clone(),
                            data: EnumData::Unit,
                        };
                    }
                    // Slice F (`std.json`): qualified `Json.Variant(args)`
                    // construction. The bare-name path (`Bool(true)`)
                    // collides with `bool::from`, so users must qualify
                    // every Json variant. The interpreter's generic
                    // `find_enum_for_variant` fallback only fires when
                    // the callee evaluates to a non-callable, but
                    // `eval_expr_inner(Path)` panics before that on
                    // unknown enum variants — so we build the variant
                    // directly here. Mirrors the Ordering arm above.
                    if segments.len() == 2 && segments[0] == "Json" {
                        let variant = segments[1].clone();
                        let arg_vals: Vec<Value> = args
                            .iter()
                            .map(|a| self.eval_expr_inner(&a.value))
                            .collect();
                        let data = if variant == "Null" {
                            EnumData::Unit
                        } else {
                            EnumData::Tuple(arg_vals)
                        };
                        return Value::EnumVariant {
                            enum_name: "Json".to_string(),
                            variant,
                            data,
                        };
                    }
                    // Numeric primitive From conversion: `T.from(x)` for
                    // integer/float widening. Interpreter stores all ints as
                    // i64 and floats as f64, so widening is the identity.
                    // F32/F64 wrappers are handled by their dedicated cases above.
                    if segments.len() == 2 && segments[1] == "from" {
                        let target = segments[0].as_str();
                        if matches!(
                            target,
                            "i8" | "i16"
                                | "i32"
                                | "i64"
                                | "u8"
                                | "u16"
                                | "u32"
                                | "u64"
                                | "usize"
                                | "isize"
                                | "f32"
                                | "f64"
                        ) {
                            if let Some(arg) = args.first() {
                                return self.eval_expr_inner(&arg.value);
                            }
                        }
                    }
                    // Lowered operator dispatch: `<Primitive>.<op>(args)`
                    // synthesized by `lowering.rs`. Routes back into the
                    // interpreter's intrinsic ops by reconstructing the
                    // BinOp/UnaryOp and reusing eval_binary/eval_unary.
                    if segments.len() == 2 {
                        let target = segments[0].as_str();
                        let method = segments[1].as_str();
                        // B-2026-08-30-40 — the canonical primitive list plus
                        // the wrappers, not a longhand copy. The copy stopped at
                        // `f32`/`f64` and omitted `i128`, `u128`, `f16`, `bf16`,
                        // so `f16.add(a, b)` was never recognized as a lowered
                        // operator call and the interpreter tried to EVALUATE
                        // `f16` as a value: "name 'f16' resolved but has no
                        // binding at run time. This is a compiler bug".
                        //
                        // B-2026-07-22-11: the total-order float wrappers are
                        // kept alongside. `a > b` on an `F32`/`F64` lowers to
                        // `F32.gt(a, b)`; route it to the binop evaluator (whose
                        // TotalFloat arms give the total order) instead of the
                        // "no evaluation rule" path. They are not in
                        // `PRELUDE_PRIMITIVES` — they are stdlib STRUCTS — so
                        // they stay spelled out.
                        let is_primitive = crate::prelude::PRELUDE_PRIMITIVES.contains(&target)
                            || matches!(target, "F32" | "F64" | "F16" | "Bf16");
                        if is_primitive {
                            if let Some(result) = self.dispatch_lowered_op(method, args, span) {
                                return result;
                            }
                        }
                    }
                }
            }
        }

        // Built-in functions. B-2026-08-01-26 — the whole intercept match is
        // skipped when the name is bound to a LOCAL closure value: a binding
        // shadows every builtin (`let spawn = |x| x + 1; spawn(4)` calls the
        // closure — the unguarded `spawn` arm used to hijack it), the same
        // locals-first rule the typechecker's intercept guards
        // (`local_scope.lookup(..)`) and codegen's closure-first dispatch
        // apply. The generic callee-eval below then dispatches the closure
        // through the ordinary `Value::Function` call path.
        if let ExprKind::Identifier(name) = &callee.kind {
            let shadowed_by_local_fn = matches!(self.env.get(name), Some(Value::Function { .. }));
            if !shadowed_by_local_fn {
                match name.as_str() {
                    "todo" | "unreachable" | "panic" => {
                        return self.eval_builtin_diverge(name, args, span);
                    }
                    "Some" => {
                        let val = if let Some(a) = args.first() {
                            self.eval_expr_inner(&a.value)
                        } else {
                            Value::Unit
                        };
                        // Ctor-arg move (B-2026-07-30-11 Option/Result leg).
                        self.record_ctor_arg_moves(args);
                        return Value::EnumVariant {
                            enum_name: "Option".to_string(),
                            variant: "Some".to_string(),
                            data: EnumData::Tuple(vec![val]),
                        };
                    }
                    "Ok" => {
                        let val = if let Some(a) = args.first() {
                            self.eval_expr_inner(&a.value)
                        } else {
                            Value::Unit
                        };
                        // Ctor-arg move (B-2026-07-30-11 Option/Result leg).
                        self.record_ctor_arg_moves(args);
                        return Value::EnumVariant {
                            enum_name: "Result".to_string(),
                            variant: "Ok".to_string(),
                            data: EnumData::Tuple(vec![val]),
                        };
                    }
                    "Err" => {
                        let val = if let Some(a) = args.first() {
                            self.eval_expr_inner(&a.value)
                        } else {
                            Value::Unit
                        };
                        // Ctor-arg move (B-2026-07-30-11 Option/Result leg).
                        self.record_ctor_arg_moves(args);
                        return Value::EnumVariant {
                            enum_name: "Result".to_string(),
                            variant: "Err".to_string(),
                            data: EnumData::Tuple(vec![val]),
                        };
                    }
                    "print" | "println" | "eprintln" => {
                        return self.eval_builtin_print(name, args, span);
                    }
                    "dbg" => {
                        return self.eval_builtin_dbg(args, span);
                    }
                    "assert" => {
                        return self.eval_builtin_assert(args, span);
                    }
                    "assert_eq" => {
                        return self.eval_builtin_assert_eq(args, span);
                    }
                    "assert_ne" => {
                        return self.eval_builtin_assert_ne(args, span);
                    }
                    "spawn" => {
                        return self.eval_spawn(args, span);
                    }
                    "collect_all_vec" => {
                        return self.eval_collect_all_vec(args, span);
                    }
                    "collect_all" => {
                        return self.eval_collect_all(args, span);
                    }
                    "sleep_ms" => {
                        return self.eval_builtin_sleep_ms(args, span);
                    }
                    "forget" => {
                        // FFI ownership-handoff primitive (design.md §
                        // Exported C ABI, Slice 4). Evaluate the argument to
                        // consume it, then return unit. The argument's
                        // scope-exit Drop is suppressed at the statement level
                        // (`suppress_forget_stmt_user_drop` in eval_stmt) —
                        // the tree-walk analogue of codegen's drop
                        // suppression — so the destructor never fires. The
                        // `#[compiler_builtin]` stub body is skipped by this
                        // intercept (it would otherwise drop the owned param).
                        if let Some(a) = args.first() {
                            let _ = self.eval_expr_inner(&a.value);
                        }
                        return Value::Unit;
                    }
                    "ref_eq" => {
                        // Reference-identity comparison for `shared` handles
                        // (design.md § Equality Semantics). Two shared values are
                        // `ref_eq` iff they share one `Arc` allocation. Typecheck
                        // (`infer_ref_eq_intrinsic`) requires `shared` args, so the
                        // non-shared arms below are unreachable for a well-formed
                        // program — they keep eval total.
                        let a = args.first().map(|x| self.eval_expr_inner(&x.value));
                        if self.pending_cf.is_some() {
                            return a.unwrap_or(Value::Unit);
                        }
                        let b = args.get(1).map(|x| self.eval_expr_inner(&x.value));
                        if self.pending_cf.is_some() {
                            return b.unwrap_or(Value::Unit);
                        }
                        let same = match (a, b) {
                            (Some(Value::SharedStruct(x)), Some(Value::SharedStruct(y))) => {
                                std::sync::Arc::ptr_eq(&x, &y)
                            }
                            _ => false,
                        };
                        return Value::Bool(same);
                    }
                    "fence" | "compiler_fence" => {
                        // Standalone memory barriers (`runtime/stdlib/intrinsics.kara`).
                        // A single-threaded tree-walk interpreter observes no memory
                        // reordering, so a fence is semantically inert here — a
                        // no-op, matching codegen's `fence` which only constrains
                        // *inter-thread* visibility. The `#[compiler_builtin]` stub
                        // body is skipped by this intercept (it would otherwise fail
                        // to resolve the `fence` callee as a binding). No need to
                        // evaluate the ordering argument (a pure `MemoryOrdering`
                        // literal with no side effects).
                        return Value::Unit;
                    }
                    "volatile_read" | "volatile_write" => {
                        // MMIO intrinsics (`runtime/stdlib/intrinsics.kara`). The
                        // tree-walk interpreter has no raw-pointer representation
                        // (the same reason `CStr.from_ptr` / the `ptr` method
                        // family reject in `karac run`), so a volatile load/store
                        // through a pointer is meaningless here. Reject loudly at
                        // the producer; the compiled backend lowers these.
                        return self.record_runtime_error(
                            format!(
                                "{name}(...) is not supported under `karac run`: the \
                             tree-walk interpreter has no raw-pointer \
                             representation. Compile with `karac build` instead."
                            ),
                            span,
                        );
                    }
                    "swap" if args.len() == 2 && self.env.get("swap").is_none() => {
                        // std.mem::swap — exchange the values at two `mut ref`
                        // places without dropping either. Read both current
                        // values, then write each back to the OTHER place. The
                        // `#[compiler_builtin]` stub body is skipped by this
                        // intercept. (Tree-walk analogue of codegen's
                        // load/load/store/store — no destructor runs.)
                        let va = self.eval_expr_inner(&args[0].value);
                        let vb = self.eval_expr_inner(&args[1].value);
                        self.write_back_receiver(&args[0].value, vb);
                        self.write_back_receiver(&args[1].value, va);
                        return Value::Unit;
                    }
                    "replace" if args.len() == 2 && self.env.get("replace").is_none() => {
                        // std.mem::replace — write `value` into `*dest`, return
                        // the PREVIOUS `*dest`. The old value is moved out
                        // (returned, not dropped); `value` is moved in.
                        let old = self.eval_expr_inner(&args[0].value);
                        let new = self.eval_expr_inner(&args[1].value);
                        self.write_back_receiver(&args[0].value, new);
                        return old;
                    }
                    _ => {}
                }
            }
        }

        // Evaluate arguments
        let arg_vals: Vec<Value> = args
            .iter()
            .map(|a| self.eval_expr_inner(&a.value))
            .collect();

        // Check for enum variant constructor before evaluating callee
        if let ExprKind::Identifier(name) = &callee.kind {
            if self.env.get(name).is_none() {
                if let Some(enum_name) = self.find_enum_for_variant(name) {
                    // Ctor-arg move (B-2026-07-30-11 Option/Result leg).
                    self.record_ctor_arg_moves(args);
                    return Value::EnumVariant {
                        enum_name,
                        variant: name.clone(),
                        data: EnumData::Tuple(arg_vals),
                    };
                }
                // Distinct-type constructor: `UserId(value)` is a zero-cost
                // wrap — the runtime value IS the base value. For the combined
                // `distinct type T = B where P` form, the constructor enforces
                // the predicate at runtime (a const-arg violation was already
                // caught at compile time); a false predicate is a `contract
                // violated` fault, exactly like `x as Refined`.
                if self.is_distinct_type(name) {
                    let val = arg_vals.into_iter().next().unwrap_or(Value::Unit);
                    if let Some(pred) = self.refinement_predicate(name) {
                        if self.eval_refinement_predicate(&pred, val.clone()) != Some(true) {
                            return self.record_runtime_error(
                                format!(
                                    "contract violated: value does not satisfy distinct type `{name}`"
                                ),
                                span,
                            );
                        }
                    }
                    return val;
                }
            }
        }

        // Qualified enum-variant constructor: `Result.Ok(x)`, `Color.Blue(7)`,
        // `Option.Some(v)` — generic over any user-program or baked-stdlib
        // enum. The resolver and codegen accept this qualified form; without
        // this arm the interpreter would `eval_expr_inner` the callee path
        // `Enum.Variant` below, which is neither a binding nor a registered
        // function, and panic ("path '…' not found"). Peer to the hand-rolled
        // `Ordering.*` / `Json.*` arms in the segments match above, but
        // data-driven from the enum's declaration. Placed after the builtin /
        // `from` / lowered-op / method-dispatch arms so a genuine
        // `Type.method(...)` (incl. `Enum.assoc_fn(...)`) still wins — a
        // variant name and a method name never collide on one type.
        if let ExprKind::Path { segments, .. } = &callee.kind {
            if segments.len() == 2 {
                if let Some(is_unit) =
                    self.qualified_enum_variant_is_unit(&segments[0], &segments[1])
                {
                    let data = if is_unit {
                        EnumData::Unit
                    } else {
                        // Ctor-arg move (B-2026-07-30-11 Option/Result leg).
                        self.record_ctor_arg_moves(args);
                        EnumData::Tuple(arg_vals)
                    };
                    return Value::EnumVariant {
                        enum_name: segments[0].clone(),
                        variant: segments[1].clone(),
                        data,
                    };
                }
            }
        }

        // `<Type>.default()` where `Type` is (or resolves through a bound type
        // param to) a PRIMITIVE — the built-in zero value (`0` / `0.0` /
        // `false` / `'\0'` / `""`). Named types have a `<Type>.default`
        // function (derived or hand-written) registered in env and route
        // through the normal callee-eval below; primitives have no such
        // function, so without this intercept `T.default()` monomorphized to a
        // primitive (std.mem `take[T: Default]` on `i64`, any `fn f[T:
        // Default]`) falls through to the "no interpreter evaluation rule"
        // path error. Mirrors codegen's primitive-default fallthrough in
        // `compile_assoc_call`.
        if let ExprKind::Path { segments, .. } = &callee.kind {
            if segments.len() == 2 && segments[1] == "default" && args.is_empty() {
                let concrete = self
                    .resolve_type_param(&segments[0])
                    .unwrap_or_else(|| segments[0].clone());
                if let Some(v) = primitive_default_value(&concrete) {
                    return v;
                }
            }
        }

        // Evaluate callee
        let callee_val = self.eval_expr_inner(callee);
        // Callee evaluation can itself fault (e.g. the unwired-path
        // runtime error in eval_expr's Path fallback). Short-circuit
        // before dispatching on the placeholder Value it returned, or
        // the non-callable `unreachable!` below fires on Value::Unit.
        if self.pending_cf.is_some() {
            return callee_val;
        }
        let callee_variant = callee_val.variant_name();

        match callee_val {
            Value::Function {
                name: fn_name,
                param_patterns,
                param_defaults,
                body,
                closure_env,
                ..
            } => {
                self.env.push_scope();
                let pushed_subs = self.push_type_subs_for_call(span);
                if let Some(ref captured) = closure_env {
                    for (k, v) in captured {
                        self.env.define(k.clone(), v.clone());
                    }
                }
                // B-2026-08-14-2 — an int argument at a FLOAT-declared parameter
                // is an implicit widening the language permits; performing it
                // here is what keeps `fn f(x: f64)` called with a `u8` from
                // binding an Int that the body's first float comparison then
                // ABORTS on. Looked up once per call, and `None` for a closure
                // or builtin leaves those untouched.
                let declared_tys = self.declared_param_tys_of_fn(&fn_name);
                for (i, pat) in param_patterns.iter().enumerate() {
                    let val = if let Some(v) = arg_vals.get(i) {
                        v.clone()
                    } else if let Some(Some(default_expr)) = param_defaults.get(i) {
                        self.eval_expr_inner(default_expr)
                    } else {
                        continue;
                    };
                    // B-2026-08-30-34 — `arg_vals` is built positionally from
                    // `args`, so index `i` names this argument's own expression
                    // and its recorded unsigned width.
                    let src_u = args
                        .get(i)
                        .and_then(|a| self.span_unsigned_int_width(&a.value.span));
                    let val = match declared_tys.as_ref().and_then(|tys| tys.get(i)) {
                        Some(te) => super::exec::coerce_int_value_to_declared_float(val, te, src_u),
                        None => val,
                    };
                    self.bind_pattern(pat, val);
                }

                // Contract checking (design.md § Contracts): `requires`
                // predicates run at entry (params in scope), `ensures` at the
                // return point (with `result` bound). A false predicate
                // faults `contract violated`; the body does not run if a
                // `requires` fails. `None` for the no-contract common case.
                let contract = self.function_contract(&fn_name);
                let mut contract_fault: Option<String> = None;
                if let Some((requires, _)) = &contract {
                    for req in requires {
                        match self.eval_contract_predicate(req) {
                            super::ContractOutcome::Held => {}
                            super::ContractOutcome::Violated => {
                                contract_fault =
                                    Some("contract violated: requires clause".to_string());
                                break;
                            }
                            super::ContractOutcome::Panicked(msg) => {
                                contract_fault =
                                    Some(format!("contract predicate panicked: {msg}"));
                                break;
                            }
                        }
                    }
                }

                // Capture `old(expr)` pre-state snapshots for the ensures
                // clauses BEFORE the body runs (design.md § Contracts rule 4):
                // each `old(arg)` arg is evaluated at entry and stashed by
                // span; the postcondition reads it back at exit.
                let mut pushed_old = false;
                if contract_fault.is_none() {
                    if let Some((_, ensures)) = &contract {
                        let mut snap = rustc_hash::FxHashMap::default();
                        for ens in ensures {
                            let ens_body = ens.body.clone();
                            self.capture_old_in_expr(&ens_body, &mut snap);
                        }
                        if !snap.is_empty() {
                            self.old_snapshots.push(snap);
                            pushed_old = true;
                        }
                    }
                }

                // B-2026-07-21-17 — entering a spliced gated-stdlib
                // wrapper's body: record THIS call's span (a user-file
                // location) so a runtime error raised inside the body is
                // attributed to it, not to the wrapper's own module-source
                // line/col (which the CLI would render against the user
                // file's path). `record_runtime_error` consults only the
                // OUTERMOST frame, so nested wrapper→wrapper calls still
                // attribute to the user-visible entry call.
                let is_stdlib_wrapper = self.program.items.iter().any(|item| {
                    matches!(item, crate::ast::Item::Function(f)
                        if f.name == fn_name && f.stdlib_origin)
                });
                if is_stdlib_wrapper {
                    self.stdlib_wrapper_call_spans.push(*span);
                }
                // B-2026-08-01-12: expose the callee's OWNED param names to
                // the body's let-destructure gate — a struct destructure of
                // an owned by-value param binds views of the entry copy, and
                // its Drop observability belongs to the caller (see
                // `owned_param_names_stack`). Ref/mut-ref params are
                // excluded at collection time; a closure (no program fn of
                // this name) contributes an empty set, so the gate never
                // fires inside closures.
                let seed_params = self.owned_param_names_of_call(
                    &fn_name,
                    &param_patterns,
                    closure_env.is_some(),
                );
                self.owned_param_names_stack.push(seed_params);
                self.owned_param_frame_is_method.push(false);
                // B-2026-08-09-10 — `moved_out_user_drop_bindings` is keyed by
                // NAME with no frame scoping, so a callee that moves a payload
                // out of its own binding silently disarmed an UNRELATED caller
                // binding that happened to share the name. Measured: with the
                // caller's local and the callee's param both spelled `b`, the
                // caller's `Drop` body never ran; renaming either one to
                // anything else made both backends agree. That is why the row
                // read as "owned enum params don't fire" — the shape it was
                // found in reused the name, as ordinary code routinely does.
                //
                // The callee opens with an EMPTY set (nothing in a fresh frame
                // has been moved out yet) and the caller's is restored on the
                // way out, so a move recorded inside the body can no longer
                // escape the frame that made it. Not a stack, because the
                // entries are consulted by bare name from many places; swapping
                // the whole map is what makes the isolation total.
                // B-2026-08-27-48 — the three CONTAINER/ELEMENT move sets join
                // the isolation for the reason B-2026-08-09-10 gave for the
                // first three: they are keyed by bare NAME with no frame
                // scoping, so a callee acting on its own param silently
                // disarmed an unrelated CALLER binding that happened to share
                // the name. Measured on `fn take(p: (R, i64)) { let (r, n) =
                // p; … }`: with the caller's local also spelled `p` the
                // caller's tuple element walk went silent and the body ran
                // once; renaming it to `q` — nothing else changed — ran it
                // twice. Two defects cancelling looked like correct code.
                let saved_moved_out = (
                    std::mem::take(&mut self.moved_out_user_drop_bindings),
                    std::mem::take(&mut self.moved_out_enum_payload_bindings),
                    std::mem::take(&mut self.moved_out_drop_field_bindings),
                    std::mem::take(&mut self.moved_out_container_bodies_bindings),
                    std::mem::take(&mut self.moved_out_tuple_elem_bodies),
                    std::mem::take(&mut self.moved_out_struct_field_bodies),
                    // B-2026-08-29-33 — the two payload-only masks ride the
                    // same per-frame save/restore for the same reason: they are
                    // keyed by NAME, so a callee local sharing a caller
                    // binding's name would otherwise disarm the caller's walk.
                    std::mem::take(&mut self.moved_out_struct_field_payload_bodies),
                    std::mem::take(&mut self.moved_out_tuple_elem_payload_bodies),
                    // B-2026-08-29-24 — and the enum-payload SLOT mask, for the
                    // same name-keyed reason.
                    std::mem::take(&mut self.moved_out_enum_payload_slots),
                    // B-2026-08-29-47 — the param-view FIELD record, name-keyed
                    // like the masks above and isolated for the same reason.
                    std::mem::take(&mut self.param_view_struct_fields),
                    // B-2026-09-01-3 — the tuple peer, isolated beside it.
                    std::mem::take(&mut self.param_view_tuple_elems),
                );
                // B-2026-08-28-22 — hand the callee ownership of the `Drop`
                // BODY of any owned param it returns on some tail paths and not
                // others. The caller has already declined its side for every
                // path (`fn_returns_param` answers over the UNION of return
                // sites), so without this the value that actually died inside
                // the call ran no body at all. Seeded here, immediately before
                // the body, so `eval_block_inner` adopts it into the body
                // block's own cleanup; the arm tail that returns the param
                // disarms it through `record_conditional_move_tail`.
                self.pending_param_drop_bindings = self.cond_returned_param_drop_names(&fn_name);
                // B-2026-08-30-33 — keep the names for the whole frame; the
                // list above is taken by the body block before any statement
                // runs, and the per-path disarm needs to ask later.
                let saved_cond_store_params = std::mem::replace(
                    &mut self.cond_store_param_names,
                    self.pending_param_drop_bindings.iter().cloned().collect(),
                );
                let result = if contract_fault.is_some() {
                    Ok(Value::Unit)
                } else {
                    self.eval_body_growing(&body)
                };
                (
                    self.moved_out_user_drop_bindings,
                    self.moved_out_enum_payload_bindings,
                    self.moved_out_drop_field_bindings,
                    self.moved_out_container_bodies_bindings,
                    self.moved_out_tuple_elem_bodies,
                    self.moved_out_struct_field_bodies,
                    self.moved_out_struct_field_payload_bodies,
                    self.moved_out_tuple_elem_payload_bodies,
                    self.moved_out_enum_payload_slots,
                    self.param_view_struct_fields,
                    self.param_view_tuple_elems,
                ) = saved_moved_out;
                // B-2026-08-30-33 — restore with the rest of the per-frame
                // move bookkeeping. Left un-restored, a callee's parameter name
                // stays live in the CALLER's frame, where any binding that
                // happens to share the name would be disarmed by a hand-over
                // that has nothing to do with it. No probe reproduced that --
                // the same-name case measures identical before and after -- but
                // every neighbour in this block is saved and restored, and a
                // set that outlives its frame is a hazard whether or not one
                // program has found it yet.
                self.cond_store_param_names = saved_cond_store_params;
                self.owned_param_names_stack.pop();
                self.owned_param_frame_is_method.pop();
                if is_stdlib_wrapper {
                    self.stdlib_wrapper_call_spans.pop();
                }

                // `ensures` predicates run after the body, with `result`
                // bound to the return value (skipped if the body itself
                // already faulted). `old(arg)` reads the entry snapshot.
                if contract_fault.is_none() {
                    if let Some((_, ensures)) = &contract {
                        let ret_val = match &result {
                            Ok(v) => Some(v.clone()),
                            Err(ControlFlow::Return(v)) => Some(v.clone()),
                            _ => None,
                        };
                        if let Some(rv) = ret_val {
                            for ens in ensures {
                                self.env.push_scope();
                                if let Some(param) = &ens.param {
                                    self.env.define(param.clone(), rv.clone());
                                }
                                let outcome = self.eval_contract_predicate(&ens.body);
                                self.env.pop_scope();
                                match outcome {
                                    super::ContractOutcome::Held => {}
                                    super::ContractOutcome::Violated => {
                                        contract_fault =
                                            Some("contract violated: ensures clause".to_string());
                                        break;
                                    }
                                    super::ContractOutcome::Panicked(msg) => {
                                        contract_fault =
                                            Some(format!("contract predicate panicked: {msg}"));
                                        break;
                                    }
                                }
                            }
                        }
                    }
                }

                if pushed_old {
                    self.old_snapshots.pop();
                }

                // Constructor invariants (design.md § Contracts: "Constructors
                // (pub associated functions that return `Self`) also check the
                // invariant at their return point"). A constructor has no
                // receiver, so the *return value* is bound as `self` and each of
                // the type's invariants (impl-always / plain-if-pub) is checked
                // — the construction boundary, mirroring the method-exit check in
                // `eval_method_call`. The qualified `Type.method` key comes from
                // the *callee* path (`Counter.bad`), not the `Value::Function`'s
                // inner name, which is the bare `bad`. Inert for free functions,
                // bare-identifier calls, and non-Self returns
                // (`constructor_invariants_to_check` yields an empty list).
                // Skipped if the body already faulted.
                let qualified_callee = match &callee.kind {
                    ExprKind::Path { segments, .. } if segments.len() == 2 => {
                        Some(segments.join("."))
                    }
                    _ => None,
                };
                if contract_fault.is_none() {
                    let invariants = qualified_callee
                        .as_deref()
                        .map(|q| self.constructor_invariants_to_check(q))
                        .unwrap_or_default();
                    if !invariants.is_empty() {
                        let ret_val = match &result {
                            Ok(v) => Some(v.clone()),
                            Err(ControlFlow::Return(v)) => Some(v.clone()),
                            _ => None,
                        };
                        if let Some(rv) = ret_val {
                            for inv in &invariants {
                                self.env.push_scope();
                                self.env.define("self".to_string(), rv.clone());
                                let outcome = self.eval_contract_predicate(inv);
                                self.env.pop_scope();
                                match outcome {
                                    super::ContractOutcome::Held => {}
                                    super::ContractOutcome::Violated => {
                                        contract_fault =
                                            Some("contract violated: invariant".to_string());
                                        break;
                                    }
                                    super::ContractOutcome::Panicked(msg) => {
                                        contract_fault =
                                            Some(format!("contract predicate panicked: {msg}"));
                                        break;
                                    }
                                }
                            }
                        }
                    }
                }

                // CICO write-back: for each call arg that denotes a PLACE and
                // whose corresponding param is a mutate-through borrow, copy
                // the callee's final binding for that param back into that
                // place before the scope is popped.
                //
                // B-2026-08-05-37 widened this from a bare identifier to any
                // place. A projection argument — `bump(mut g.val)`,
                // `bump(mut g.q.v)`, `bump(mut t.0)`, `bump(mut v[0])` — used
                // to `continue` here, so the callee's write was silently
                // discarded: the program printed the PRE-call value with no
                // diagnostic at any phase. Codegen answers the same question
                // by handing the callee a pointer to the place; the
                // interpreter has no pointers, so it stores back through
                // `assign_to_place`, which already handles every assignable
                // form and is the same walk `g.val = x` uses.
                //
                // The trigger is EITHER the call-site `mut` marker (the fresh-
                // owned-root case) OR the callee param being declared `mut ref`
                // / `mut Slice`. The latter is essential for FORWARDED borrows:
                // an already-in-scope `mut ref` arg forwards WITHOUT a marker
                // (design.md § Call-site mutation markers), so a marker-only
                // gate silently drops write-back through nested/recursive calls
                // — e.g. a `mut ref i64` accumulator threaded down a recursion
                // never accumulates. Keying on the param mode too restores the
                // chain and matches codegen's full aliasing semantics.
                let param_mut_ref = self.fn_param_mut_ref_flags(&fn_name);
                let mut writebacks: Vec<(Expr, Value)> = Vec::new();
                for (i, arg) in args.iter().enumerate() {
                    let param_is_mut_ref = param_mut_ref
                        .as_ref()
                        .and_then(|flags| flags.get(i))
                        .copied()
                        .unwrap_or(false);
                    if !arg.mut_marker && !param_is_mut_ref {
                        continue;
                    }
                    if !Self::place_is_writeback_safe(&arg.value) {
                        continue;
                    }
                    if let Some(pat) = param_patterns.get(i) {
                        if let crate::ast::PatternKind::Binding(param_name) = &pat.kind {
                            if let Some(val) = self.env.get(param_name) {
                                writebacks.push((arg.value.clone(), val));
                            }
                        }
                    }
                }

                self.env.pop_scope();
                if pushed_subs {
                    self.type_subs_stack.pop();
                }

                for (place, val) in writebacks {
                    self.assign_to_place(&place, val, None);
                }

                if let Some(msg) = contract_fault {
                    return self.record_runtime_error(msg, span);
                }

                // Fresh-temp Drop-typed call args (B-2026-07-01-8, interp
                // twin of codegen's B-2026-07-01-6): `consume(Guard { id: 7
                // })` / `consume(Sig.A(1))` / `consume(Sig.B)` have no
                // caller binding, so no `CleanupAction::Drop` ever fired
                // their user body — silent under `karac run`, one drop per
                // call under `karac build`. Run the body on the temp's
                // value after the call returns (the caller-side temp-drop
                // position codegen uses). Identifier args are excluded —
                // the caller binding's own NLL drop covers those.
                // B-2026-07-01-7: the callee name feeds the passthrough
                // guard (`fn_returns_param`) — an arg the callee can RETURN
                // flows out to the result's consumer and must not also drop
                // here.
                self.run_fresh_temp_arg_drops(&fn_name, None, args, &arg_vals);
                // B-2026-08-02-23 leg 2 — the IDENTIFIER-arg sibling of the
                // guard above: `run_fresh_temp_arg_drops` skips named bindings
                // on the "their own NLL drop covers it" rule, which duplicates
                // the body when the callee returns that very arg.
                self.record_passthrough_arg_moves(&fn_name, args);

                // B-2026-08-14-2 — the RETURN half of the same widening rule:
                // `fn f(v: u8) -> f64 { v }` hands back a float. Applied to the
                // tail value and to an explicit `return` alike, so the two exits
                // cannot disagree.
                let ret_te = self.declared_return_ty_of_fn(&fn_name);
                match result {
                    Ok(v) | Err(ControlFlow::Return(v)) => match ret_te.as_ref() {
                        Some(te) => super::exec::coerce_int_value_to_declared_float(v, te, None),
                        None => v,
                    },
                    Err(cf) => self.set_cf(cf),
                }
            }
            _ => {
                // Try enum variant constructor by name
                let variant_name = match &callee.kind {
                    ExprKind::Identifier(n) => n.clone(),
                    ExprKind::Path { segments, .. } => segments.last().cloned().unwrap_or_default(),
                    _ => String::new(),
                };
                if let Some(enum_name) = self.find_enum_for_variant(&variant_name) {
                    // Ctor-arg move (B-2026-07-30-11 Option/Result leg).
                    self.record_ctor_arg_moves(args);
                    return Value::EnumVariant {
                        enum_name,
                        variant: variant_name,
                        data: EnumData::Tuple(arg_vals),
                    };
                }
                unreachable!(
                    "call target at {}:{} was Value::{} (not Function, not an enum-variant \
                     constructor); either an interpreter codepath produced the wrong variant \
                     or the typechecker accepted a non-callable callee",
                    span.line, span.column, callee_variant
                )
            }
        }
    }

    /// Owned (by-value) parameter names of the top-level function `fn_name`,
    /// for the let-destructure gate (B-2026-08-01-12). A `ref T` /
    /// `mut ref T` param takes no ownership and stays out of the set; a
    /// name with no program-level fn (a closure) yields the empty set.
    /// The callee's DECLARED parameter types, for the int-to-float coercion at
    /// an argument position (B-2026-08-14-2).
    ///
    /// `Value::Function` carries patterns and defaults but not types, so the
    /// declared type has to be read back off the program item. Same scan (and
    /// same per-call cost) as [`Self::owned_param_names_of_fn`], which every
    /// user-fn call already performs. `None` for a closure or a builtin, which
    /// leaves those calls exactly as they are.
    fn declared_param_tys_of_fn(&self, fn_name: &str) -> Option<Vec<crate::ast::TypeExpr>> {
        if let Some(tys) = self.program.items.iter().find_map(|item| match item {
            crate::ast::Item::Function(f) if f.name == fn_name => {
                Some(f.params.iter().map(|p| p.ty.clone()).collect::<Vec<_>>())
            }
            _ => None,
        }) {
            return Some(tys);
        }
        // An IMPL METHOD reaches this arm under its bare name — the env key is
        // `Type.method` but `Value::Function` carries only `name`. Resolving by
        // name alone would be unsound if two types both defined it, so this is
        // FAIL-CLOSED: a name that matches more than one impl method yields
        // `None` and leaves the call exactly as it is. Converting an Int to a
        // Float against the wrong signature would be a miscompile, which is
        // strictly worse than the crash it would be papering over.
        let mut found: Option<Vec<crate::ast::TypeExpr>> = None;
        for item in &self.program.items {
            let crate::ast::Item::ImplBlock(imp) = item else {
                continue;
            };
            for m in imp.items.iter().filter_map(|it| match it {
                crate::ast::ImplItem::Method(f) => Some(f),
                _ => None,
            }) {
                if m.name != fn_name {
                    continue;
                }
                if found.is_some() {
                    return None;
                }
                // A receiver occupies `param_patterns[0]`, so the type list has
                // to be padded to keep the caller's index arithmetic honest.
                // `Unit` is not a float, so the self slot never converts.
                let mut tys: Vec<crate::ast::TypeExpr> = Vec::new();
                if m.self_param.is_some() {
                    tys.push(crate::ast::TypeExpr {
                        kind: crate::ast::TypeKind::Tuple(Vec::new()),
                        span: m.span,
                    });
                }
                tys.extend(m.params.iter().map(|p| p.ty.clone()));
                found = Some(tys);
            }
        }
        found
    }

    /// The callee's DECLARED return type — the `ret_coerce` half of the same
    /// rule: `fn f(v: u8) -> f64 { v }` must hand back a float.
    fn declared_return_ty_of_fn(&self, fn_name: &str) -> Option<crate::ast::TypeExpr> {
        self.program.items.iter().find_map(|item| match item {
            crate::ast::Item::Function(f) if f.name == fn_name => f.return_type.clone(),
            _ => None,
        })
    }

    /// B-2026-08-28-4 — the owned by-value param names of whatever `fn_name`
    /// resolves to at THIS call: a top-level fn, or a CLOSURE bound to that
    /// name.
    ///
    /// [`Self::owned_param_names_of_fn`] resolves against `program.items`, so a
    /// closure matched nothing and contributed the empty set. The
    /// let-destructure gate therefore never fired inside a closure body, its
    /// leaves kept their Drop slots, and a destructure of a by-value closure
    /// param ran the element's body TWICE — once from the callee's slot and
    /// once from the caller's fresh-temp argument walk, which fires for a
    /// closure call exactly as it does for a free fn. The free-fn spelling of
    /// the same body has been correct since B-2026-08-27-48; the closure was
    /// the one frame kind the gate could not see.
    ///
    /// A closure's params are `Pattern`s with no recorded type, so `ref` /
    /// `mut ref` cannot be filtered out the way the fn path filters them. Only
    /// plain `Binding` patterns are collected, and a borrow-typed closure param
    /// is covered by a control in the regression test rather than by a filter
    /// this shape cannot express.
    fn owned_param_names_of_call(
        &self,
        fn_name: &str,
        param_patterns: &[crate::ast::Pattern],
        is_closure: bool,
    ) -> std::collections::HashSet<String> {
        if !is_closure {
            return self.owned_param_names_of_fn(fn_name);
        }
        param_patterns
            .iter()
            .filter_map(|p| match &p.kind {
                crate::ast::PatternKind::Binding(n) => Some(n.clone()),
                _ => None,
            })
            .collect()
    }

    /// B-2026-08-28-22 — the owned params of `fn_name` whose `Drop` body this
    /// frame must own, because the function returns them on some tail paths and
    /// not others.
    ///
    /// The admission rule is `fn_conditionally_returns_param_bare` — shared
    /// with codegen, so both backends flip ownership for exactly the same set
    /// of parameters rather than agreeing by convention. The value must
    /// currently be a plain struct with a user `Drop`: a shared struct drops
    /// through the RC path and never this drain, and the enum-payload channel
    /// is a different registration this row does not touch.
    fn cond_returned_param_drop_names(&self, fn_name: &str) -> Vec<String> {
        // Free functions and ASSOCIATED functions — never an instance method,
        // whose caller path does not stand down for a conditionally-returned
        // arg, so flipping ownership there yields two bodies. See the codegen
        // twin's comment (B-2026-08-28-70). The exclusion lives in
        // `callee_fn_for_param_ownership` now rather than in a `contains('.')`
        // test here, which never fired: this arm receives the callee's OWN
        // name, and `register_impl_methods` stores an impl method's bare name
        // even though its env key is qualified. That is what left the
        // associated spelling with no owner for a dying param (B-2026-09-01-44).
        let Some(f) = self.callee_fn_for_param_ownership(fn_name) else {
            return Vec::new();
        };
        // Generics are claimed like any other shape since B-2026-08-28-71.
        // They were excluded because codegen's mono leg could not hand the body
        // to the callee, and fixing the interpreter alone would have converted a
        // shared gap into a run-vs-build divergence. `compile_mono_function` now
        // carries the same registration (and the escaping-site seeding the
        // guard needs), so both backends admit the same set again.
        let mut out = Vec::new();
        for (i, p) in f.params.iter().enumerate() {
            let Some(name) = p.name() else { continue };
            // B-2026-08-30-28 — TWO conditional escape routes are claimed
            // here, not one. The original is the conditionally RETURNED param;
            // the second is the conditionally STORED one, which had no owner on
            // the path where the store did not happen: the caller stands down
            // (`record_passthrough_arg_moves`' `escapes_into_outliving_place`
            // leg) and, before this, the callee registered nothing, so the
            // value simply died. Codegen's twin is the conditional-store
            // registration in `compile_function`'s parameter loop, gated on the
            // same pair of predicates so both backends claim the same set.
            let cond_returned = crate::ast::fn_conditionally_returns_param_bare(f, i)
                && !crate::ast::fn_moves_param_into_outliving_place(f, i);
            let cond_stored = crate::ast::fn_conditionally_moves_param_into_outliving_place(f, i);
            if !cond_returned && !cond_stored {
                continue;
            }
            let Some(Value::Struct { name: tn, .. }) = self.env.get(name) else {
                continue;
            };
            if self.program.drop_method_keys.contains_key(tn.as_str()) {
                out.push(name.to_string());
            }
        }
        out
    }

    /// B-2026-08-28-70 — the owned params of IMPL METHOD `type_name.method`
    /// whose `Drop` body this frame must own.
    ///
    /// The method sibling of [`Self::cond_returned_param_drop_names`], and it
    /// admits a STRICTLY WIDER set, because the two frames differ in who else
    /// could fire. A free function's argument reaches the caller's
    /// `run_fresh_temp_arg_drops`, so the callee owns only the params the
    /// caller stood down for — the conditionally-returned ones. A METHOD's
    /// arguments reach no caller-side fire at all in this backend (the reason
    /// `owned_param_frame_is_method` exists), so the frame is the ONLY owner
    /// there is and every param that dies inside must fire here or nowhere.
    /// Measured: `impl B2 { fn eat(ref self, r: R) -> i64 { 7 } }` ran ZERO
    /// bodies against one on all three compiled backends and one for the
    /// free-function twin.
    ///
    /// The exclusions are the escape routes, and each hands the value to a
    /// different owner:
    ///
    ///  * UNCONDITIONALLY returned — the caller's result binding owns it.
    ///    Registering here as well is a double body, which is what the
    ///    `fn_returns_param && !conditionally` pair rules out. A callee that
    ///    returns the param on SOME paths keeps its registration and relies on
    ///    B-2026-08-28-51's per-path flag to disarm on the path that returned
    ///    it, exactly as the free-fn sibling does.
    ///  * Moved into an outliving place (`self.xs.push(x)`) — the new home
    ///    owns it (B-2026-08-26-9).
    ///  * A `ref` / `mut ref` param is borrowed, never owned.
    ///
    /// A `return r` shape is deliberately left where it is: `fn_returns_param`
    /// sees the return site and `fn_conditionally_returns_param_bare` declines
    /// `return` statements outright, so such a param is excluded here and keeps
    /// today's behaviour on every backend rather than gaining a new
    /// interpreter-only fire. That is the same line the free-fn sibling draws.
    ///
    /// Generics are claimed like any other shape, the conditionally-returned
    /// one included since B-2026-08-28-71 gave codegen's mono leg the matching
    /// registration.
    pub(crate) fn method_param_drop_names(
        &self,
        type_name: &str,
        method: &str,
        args: &[crate::ast::CallArg],
    ) -> Vec<String> {
        let Some(f) = self.impl_method_ast(type_name, method) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        // `f.params` is the RAW AST method, whose params EXCLUDE the receiver
        // (`self` lives in `self_param`), so `i` indexes the predicates
        // directly. Codegen's `compile_function` sees a LOWERED method with
        // `self` at param 0 and counts from there; both were verified by
        // instrumentation rather than assumed.
        for (i, p) in f.params.iter().enumerate() {
            let Some(name) = p.name() else { continue };
            if matches!(
                p.ty.kind,
                crate::ast::TypeKind::Ref(_) | crate::ast::TypeKind::MutRef(_)
            ) {
                continue;
            }
            // B-2026-09-03-7 — the caller's fresh-temp arg walk fires this one
            // after the call returns, so registering it here as well ran the
            // body TWICE (measured: `dR2` and `dR3` doubled the moment the
            // caller-side hook landed). The set this frame owns is now exactly
            // "owned params nobody else fires", which is what its own doc
            // claimed before a caller-side fire existed on this path.
            if args.get(i).is_some_and(|a| {
                self.caller_fires_fresh_temp_arg(method, Some(type_name), i, &a.value)
            }) {
                continue;
            }
            // B-2026-08-30-28 — `fn_always_...` rather than `fn_moves_...`,
            // exactly the split the `fn_always_returns_param` comment below
            // draws and for the same reason: the MAY-predicate answers true for
            // a param stored on one path and left to die on another, and
            // skipping the registration on that answer loses the body on the
            // path where it died. A param stored on SOME path keeps its
            // registration here and is disarmed per path by the store site.
            if crate::ast::fn_always_moves_param_into_outliving_place(f, i) {
                continue;
            }
            // Handed back on EVERY exit — the caller's result binding owns it.
            // `fn_always_returns_param` rather than `fn_returns_param`, for the
            // reason its doc gives: the union answers true for a param that
            // escapes on one path and dies on another, and skipping on that
            // would lose the body on the path where it died. A param that
            // escapes on SOME path keeps its registration here and relies on
            // B-2026-08-28-51's per-path flag to disarm on the path that
            // returned it — the same split codegen's method-argument site
            // makes, with the same two predicates.
            if crate::ast::fn_always_returns_param(f, i) {
                continue;
            }
            // A GENERIC method's conditionally-returned param used to be the
            // one shape this frame could not claim: codegen's mono leg had no
            // registration for it, so claiming here would have fixed the
            // interpreter alone and turned a shared gap into a run-vs-build
            // divergence. `compile_mono_function` carries it since
            // B-2026-08-28-71, so the exclusion is gone and every generic shape
            // is claimed exactly like a non-generic one.
            // Plain user-`Drop` structs only, the same narrowing the free-fn
            // sibling applies: a `shared` struct drops through the RC path and
            // never this drain, and the enum-payload channel is a different
            // registration.
            //
            // B-2026-08-30-55 — STRUCT and ENUM params, not structs alone.
            //
            // This gate admitted `Value::Struct` only, and a method frame
            // stands its caller down (B-2026-08-28-70), so an owned ENUM
            // argument had NO owner on either side: measured,
            // `t.eat(E.A(R { .. }))` ran zero `Drop` bodies under `--interp` —
            // neither the enum's own nor its payload's — against two on both
            // compiled backends. Two controls localize it exactly, and both are
            // correct on all four surfaces: the FREE-FUNCTION twin of the same
            // program (whose caller still fires), and a STRUCT param in the
            // same method (which this arm admits). It is the intersection that
            // had nobody.
            // Does the CALLER still own this argument? It fires a NAMED
            // argument unless `record_method_arg_moves` stood it down, which it
            // does exactly when the value ESCAPES the callee. The predicate is
            // spelled the same way here on purpose: the two sides decide one
            // question between them, and a drift would either double a body or
            // lose it.
            //
            // A FRESH TEMP has no caller binding at all, so nothing on that
            // side can fire it — which is the hole this row reports.
            // The gate is `record_returned_arg_user_drop_move`'s, NOT the wider
            // `escapes` one beside it. Those two retract different things and
            // conflating them double-fires: `escapes` stands the caller's
            // CONTAINER walk down, while the binding keeps its OWN `Drop`
            // action unless the callee is guaranteed to hand the value back as
            // itself. Measured on `fn assign_enum(b: E) -> R` — which returns a
            // payload BOUND OUT of `b`, so it escapes but `b` itself does not —
            // where the wider predicate had the caller and the frame both
            // firing the enum's body.
            //
            // `fn_always_returns_param` is not in the union because the loop
            // above already skipped those params outright.
            let caller_still_owns = matches!(
                args.get(i).map(|a| &a.value.kind),
                Some(ExprKind::Identifier(_))
            ) && !crate::ast::fn_conditionally_returns_param_bare(f, i);
            if caller_still_owns {
                continue;
            }
            let Some(value) = self.env.get(name) else {
                continue;
            };
            let claims = match value {
                Value::Struct { name: tn, .. } => {
                    self.program.drop_method_keys.contains_key(tn.as_str())
                }
                Value::EnumVariant { .. } => self.enum_value_runs_user_drop(&value),
                _ => false,
            };
            if claims {
                out.push(name.to_string());
            }
        }
        out
    }

    /// Does the CALLER still own every by-value argument of this method call?
    ///
    /// B-2026-08-30-55. This is the licence a retraction inside the frame
    /// needs, and it is a different question from "did this frame register any
    /// drop slots" — which is what an earlier version of the guard asked, and
    /// got wrong. `fn tup(ref self, p: (Res, i64))` called with a fresh temp
    /// registers NOTHING (the registration admits structs and enums, and a
    /// tuple is neither), yet the frame is still the only owner, because a
    /// fresh temp has no caller binding at all. Retracting there ran ZERO
    /// bodies against one on both compiled backends — `strc(h: Holder)`, whose
    /// `Holder` has no `Drop` of its own, failed the same way.
    ///
    /// So ask about the ARGUMENTS rather than about the registrations: only
    /// where a caller binding is firing every one of them may the frame hand
    /// its slots over.
    ///
    /// The per-argument test is the one `method_param_drop_names` uses, for the
    /// same reason — the caller fires a named argument unless the callee is
    /// guaranteed to hand it back as itself.
    pub(crate) fn method_frame_caller_retains_args(
        &self,
        type_name: &str,
        method: &str,
        args: &[crate::ast::CallArg],
    ) -> bool {
        let Some(f) = self.impl_method_ast(type_name, method) else {
            return false;
        };
        f.params.iter().enumerate().all(|(i, p)| {
            // A borrow is a view the caller never handed over.
            if matches!(
                p.ty.kind,
                crate::ast::TypeKind::Ref(_) | crate::ast::TypeKind::MutRef(_)
            ) {
                return true;
            }
            // B-2026-09-03-7 — a FRESH TEMP now reaches a caller-side fire too
            // (`run_fresh_temp_arg_drops` runs on this path), so it joins the
            // identifier case rather than forcing the frame to claim the
            // argument. Before this the shape answered false here, the whole
            // frame bailed, and the leaf owned the body — which placed it at
            // the leaf's NLL death against every compiled backend's
            // after-the-call.
            (matches!(
                args.get(i).map(|a| &a.value.kind),
                Some(ExprKind::Identifier(_))
            ) || args.get(i).is_some_and(|a| {
                self.caller_fires_fresh_temp_arg(method, Some(type_name), i, &a.value)
            })) && !crate::ast::fn_always_returns_param(f, i)
                && !crate::ast::fn_conditionally_returns_param_bare(f, i)
        })
    }

    /// The owned params of this method call for which THIS FRAME is the only
    /// owner of the payload bodies — nobody else will run them.
    ///
    /// B-2026-08-31-47. Two conditions, and both are load-bearing:
    ///
    /// * the CALLER is not firing the argument (a fresh temp has no binding to
    ///   fire), which is `method_frame_caller_retains_args`' per-argument test;
    /// * the payload does not ESCAPE through the return. `fn take(b: Box2) -> R
    ///   { match b { Box2.Full(r) => return r, .. } }` hands the payload back,
    ///   so the caller's RESULT binding owns it. Measured: without this half
    ///   the body ran twice — once from the frame's walk and once from the
    ///   result — against one on both compiled backends, which is what
    ///   `test_method_fresh_temp_arg_handed_back_runs_one_body` pins.
    pub(crate) fn method_frame_sole_owned_params(
        &self,
        type_name: &str,
        method: &str,
        args: &[crate::ast::CallArg],
    ) -> Vec<String> {
        let Some(f) = self.impl_method_ast(type_name, method) else {
            return Vec::new();
        };
        f.params
            .iter()
            .enumerate()
            .filter_map(|(i, p)| {
                if matches!(
                    p.ty.kind,
                    crate::ast::TypeKind::Ref(_) | crate::ast::TypeKind::MutRef(_)
                ) {
                    return None;
                }
                let caller_fires = matches!(
                    args.get(i).map(|a| &a.value.kind),
                    Some(ExprKind::Identifier(_))
                ) && !crate::ast::fn_always_returns_param(f, i)
                    && !crate::ast::fn_conditionally_returns_param_bare(f, i);
                if caller_fires
                    || crate::ast::fn_returns_param(f, i)
                    || crate::ast::fn_returns_param_payload(f, i)
                    || crate::ast::fn_moves_param_into_outliving_place(f, i)
                {
                    return None;
                }
                p.name().map(str::to_string)
            })
            .collect()
    }

    /// Does an owned ENUM value carry user-`Drop` work the frame that owns it
    /// must run — its own body, or one reachable through its payload?
    ///
    /// B-2026-08-30-55. Separate from `value_runs_user_drop`, which answers
    /// `false` for every non-struct BY DESIGN: its doc records that a bare
    /// Array/Tuple/Map classifies false at top level so the dedicated container
    /// walkers stay the sole firers for direct bindings. An enum param is not
    /// one of those — no other walker reaches it — so it needs its own answer.
    ///
    /// Both halves are required. `enum E { A(R), B }` with `impl Drop for E`
    /// over a `Drop`-bearing `R` runs `dE` AND `dR8` on the compiled backends,
    /// and an enum with no body of its own still owes its payload's.
    ///
    /// Deliberately narrow rather than "register every enum". The firing site
    /// pushes the binding's name onto `drop_trace` whether or not a body
    /// actually runs, so admitting an enum that owes nothing would add phantom
    /// entries to a trace the tests assert on.
    fn enum_value_runs_user_drop(&self, value: &Value) -> bool {
        let Value::EnumVariant {
            enum_name, data, ..
        } = value
        else {
            return false;
        };
        if self
            .program
            .drop_method_keys
            .contains_key(enum_name.as_str())
        {
            return true;
        }
        match data {
            EnumData::Unit => false,
            EnumData::Tuple(items) => items.iter().any(|v| self.value_runs_user_drop(v)),
            EnumData::Struct(fields) => fields.values().any(|v| self.value_runs_user_drop(v)),
        }
    }

    /// B-2026-08-29-11, CALLER leg — a bare-identifier arg passed BY VALUE to a
    /// method no longer belongs to the caller's binding, so that binding must
    /// stop walking it.
    ///
    /// This is the counterpart of `owned_param_frame_is_method`, and it exists
    /// because of it. A FREE fn follows the caller-retains convention — its
    /// owned-param scrutinee is non-consuming and the CALLER fires the payload
    /// body — so `eval_call` marks only the passthrough subset
    /// (`record_passthrough_arg_moves`). A METHOD frame is declared to own its
    /// arguments outright (B-2026-08-29-10), so for a method the caller must
    /// stand down on EVERY by-value arg, not just the ones handed back.
    ///
    /// Both destinations of the value are covered by the one rule: if the
    /// payload dies inside, the frame's arm stash fires it; if it is handed
    /// back, the caller's binding for the RESULT fires it. Either way the
    /// ARGUMENT binding is no longer an owner.
    ///
    /// Nothing marked this before, and nothing had to: the callee's own mark
    /// leaked out of the frame under a shared binding name and disarmed the
    /// caller by accident. That only worked while the caller SPELLED its
    /// binding the same as the param, and the frame isolation removes the
    /// leak, so the mark is made here on purpose.
    ///
    /// `record_container_move_source_name` carries the container/field-carried
    /// half of the narrowing. The OWN-`Drop` half is B-2026-08-29-15 and is
    /// gated separately below: that channel retracts the binding's whole
    /// action, so it is licensed only where the callee hands the argument back
    /// AS ITSELF and the caller's result binding becomes an equivalent owner.
    ///
    /// This paragraph previously read that an own-`Drop` struct stays armed
    /// "because a by-value struct param is ENTRY-COPIED and the two frames
    /// genuinely hold distinct values". The copy is real — the compiled
    /// backends allocate one extra 2,048-byte buffer for a 256-element
    /// `Vec[i64]` field on ANY by-value call — but the INFERENCE from it was
    /// what held B-2026-08-29-15 open. Two measurements retire it: the named
    /// and fresh-temp spellings allocate identically (`11 allocs, 11 frees,
    /// 10,269 bytes`, byte-for-byte), so the copy cannot be what makes their
    /// body counts differ; and the fresh-temp spelling has always run ONE body
    /// with that copy present, which nobody has called wrong. The copy is a
    /// lowering artifact of a move, not a second value for `Drop` to see.
    pub(crate) fn record_method_arg_moves(
        &mut self,
        type_name: &str,
        method: &str,
        args: &[crate::ast::CallArg],
    ) {
        let Some(f) = self.impl_method_ast(type_name, method) else {
            return;
        };
        // `f.params` excludes the receiver, so `i` indexes it directly — the
        // raw-AST convention `method_param_drop_names` documents, NOT codegen's
        // self-at-0 one.
        let moved: Vec<(String, bool)> = args
            .iter()
            .enumerate()
            .filter_map(|(i, arg)| {
                let ExprKind::Identifier(n) = &arg.value.kind else {
                    return None;
                };
                let p = f.params.get(i)?;
                // A borrow is a view: the caller keeps it and keeps its walk.
                if matches!(
                    p.ty.kind,
                    crate::ast::TypeKind::Ref(_) | crate::ast::TypeKind::MutRef(_)
                ) {
                    return None;
                }
                // B-2026-08-29-10 — stand down only where the value LEAVES the
                // callee, not on every by-value argument.
                //
                // The wider rule this narrows was paired with 57bfb26's arm
                // fire, and rested on it: "if the payload dies inside, the
                // frame's arm stash fires it". That holds only when there IS an
                // arm. A method that never matches its owned enum parameter has
                // none, so standing the caller down there left NOBODY to run the
                // body — `fn take(ref self, b: E) -> i64 { println("mid");
                // return 1 }` went from one body to zero, against one on all
                // three compiled backends. Narrowing to the escape predicates
                // was tried before and did double-fire, but only because the arm
                // was still firing too; with that reverted (see
                // `scrutinee_expr_is_consuming`) the caller is the single owner
                // for the dies-inside case, which is what every free-function
                // spelling has always done and what codegen does for a method
                // today — measured with a callee LOCAL between the two candidate
                // placements, so the order is pinned and not just the count.
                //
                // The three escape routes, all of which keep the stand-down:
                // the callee returns the parameter, returns a payload bound out
                // of it, or stores it somewhere that outlives the frame (`self`,
                // a `ref` parameter) — B-2026-08-29-11's own repro, `add(r)`
                // pushing into `self.xs`, is the third.
                // B-2026-08-31-46 — the conditional hand-back joins the escape
                // routes, for the reason `record_passthrough_arg_moves` gives:
                // a constructor-wrapped conditional return is invisible to the
                // three above, and the gate declined before the per-path owner
                // below was consulted.
                let escapes = crate::ast::fn_returns_param(f, i)
                    || crate::ast::fn_returns_param_payload(f, i)
                    || crate::ast::fn_moves_param_into_outliving_place(f, i)
                    || crate::ast::fn_conditionally_returns_param_bare(f, i);
                if !escapes {
                    return None;
                }
                // B-2026-08-29-15 / -50 — among the args that DO escape,
                // the binding's own `Drop` body is disarmed only where some
                // other frame is guaranteed to run it on EVERY path: the
                // callee hands it back at every exit
                // (`fn_always_returns_param`, bare or wrapped in a returned
                // aggregate), or hands it back on some paths and lets it die
                // inside on the rest (`fn_conditionally_returns_param_bare`),
                // where the callee frame owns it. Escaping into an OUTLIVING
                // PLACE is deliberately not in the union — there the argument
                // binding is still an owner and must keep firing. The two
                // rules compose: this one is asked only of what the escape
                // gate admits.
                Some((
                    n.clone(),
                    crate::ast::fn_always_returns_param(f, i)
                        || crate::ast::fn_conditionally_returns_param_bare(f, i),
                ))
            })
            .collect();
        for (n, callee_owns_body) in moved {
            self.record_container_move_source_name(&n);
            if callee_owns_body {
                self.record_returned_arg_user_drop_move(&n);
            }
        }
    }

    /// The raw AST of impl method `type_name.method`, receiver excluded from
    /// `params`. Mirrors the lookup `method_owned_param_names` runs.
    fn impl_method_ast(&self, type_name: &str, method: &str) -> Option<&crate::ast::Function> {
        self.program.items.iter().find_map(|item| {
            let crate::ast::Item::ImplBlock(imp) = item else {
                return None;
            };
            let target = match &imp.target_type.kind {
                crate::ast::TypeKind::Path(p) => p.segments.last().map(String::as_str),
                _ => None,
            };
            if target != Some(type_name) {
                return None;
            }
            imp.items.iter().find_map(|it| match it {
                crate::ast::ImplItem::Method(m) if m.name == method => Some(&**m),
                _ => None,
            })
        })
    }

    fn owned_param_names_of_fn(&self, fn_name: &str) -> std::collections::HashSet<String> {
        self.program
            .items
            .iter()
            .find_map(|item| match item {
                crate::ast::Item::Function(f) if f.name == fn_name => Some(
                    f.params
                        .iter()
                        .filter(|p| {
                            !matches!(
                                p.ty.kind,
                                crate::ast::TypeKind::Ref(_) | crate::ast::TypeKind::MutRef(_)
                            )
                        })
                        .filter_map(|p| p.name().map(str::to_string))
                        .collect(),
                ),
                _ => None,
            })
            .unwrap_or_default()
    }

    /// B-2026-07-01-8 second half — run the user `impl Drop` body for
    /// each FRESH temporary call argument of a Drop-implementing type:
    /// struct literals (`consume(Guard { id: 7 })`), tuple-variant enum
    /// constructors (`consume(Sig.A(1))` — bare or `Enum.Variant`
    /// qualified), and unit variants (`consume(Sig.B)`). Mirrors codegen's
    /// `track_inline_owned_aggregate_arg` shapes exactly (fixed there as
    /// B-2026-07-01-6); bare Identifier args are the caller binding's own
    /// drop. Shared types are excluded (their teardown is refcount-driven).
    /// B-2026-08-28-2 — which top-level PARTS of by-value argument slot
    /// `arg_index` the named callee hands back through its return value. Thin
    /// lookup over [`crate::ast::fn_returns_param_part_paths`]; empty for an unknown
    /// name, a method, or any shape that analysis declines to classify (it
    /// under-approximates on purpose — see its doc). Codegen twin:
    /// `callee_returned_param_parts` in `call_dispatch.rs`.
    fn callee_returned_param_parts(
        &self,
        callee_name: &str,
        method_owner: Option<&str>,
        arg_index: usize,
    ) -> Vec<crate::ast::ParamPath> {
        // B-2026-09-03-7 — an INSTANCE method resolves by (type, name), not by
        // the bare-name scan below: that scan reads `Item::Function` only, so
        // for a method it answered "nothing escapes" for every argument. The
        // caller-side arg walk now runs on the method path too, and an
        // unconditional "nothing escapes" there would fire a body the callee
        // hands back.
        if let Some(ty) = method_owner {
            return self
                .impl_method_ast(ty, callee_name)
                .map(|f| crate::ast::fn_returns_param_part_paths(f, arg_index))
                .unwrap_or_default();
        }
        self.program
            .items
            .iter()
            .find_map(|item| match item {
                crate::ast::Item::Function(f) if f.name == callee_name => {
                    Some(crate::ast::fn_returns_param_part_paths(f, arg_index))
                }
                _ => None,
            })
            .unwrap_or_default()
    }

    /// The FIELD paths of parameter `i` that `callee_name` hands back to its
    /// caller (B-2026-08-28-17 / -21 / -23). A path whose head is a tuple index
    /// is the tuple arm's business, not this one's.
    fn escaping_field_paths(
        &self,
        callee_name: &str,
        method_owner: Option<&str>,
        i: usize,
    ) -> Vec<Vec<String>> {
        self.callee_returned_param_parts(callee_name, method_owner, i)
            .into_iter()
            .filter_map(|path| {
                path.into_iter()
                    .map(|p| match p {
                        crate::ast::ParamPart::Field(n) => Some(n),
                        crate::ast::ParamPart::TupleIndex(_) => None,
                    })
                    .collect::<Option<Vec<String>>>()
            })
            .filter(|path| !path.is_empty())
            .collect()
    }

    /// `value` with each escaping PATH removed, for handing to
    /// [`Self::drop_user_drop_fields_of_value`].
    ///
    /// Masking the VALUE rather than threading a gate through the walker is the
    /// B-2026-08-03-8 pattern, and works for the same reason: the walk resolves
    /// each declared field through `fields.get(..)` and skips a missing one, so
    /// its two dozen other callers are untouched.
    ///
    /// B-2026-08-28-23 — a path longer than one level does not remove the field
    /// it starts at; it REPLACES that field with a masked copy of itself, so the
    /// walk still reaches the field's Drop-bearing siblings one level down. That
    /// is the whole difference between masking `w.inner.r` and masking
    /// `w.inner`, and the codegen twin's `FieldSkipTree` has the same two arms
    /// for the same reason.
    fn mask_struct_fields(
        value: &super::value::Value,
        escaping: &[Vec<String>],
    ) -> super::value::Value {
        if escaping.is_empty() {
            return value.clone();
        }
        let super::value::Value::Struct { name, fields } = value else {
            return value.clone();
        };
        let mut fields = fields.clone();
        for path in escaping {
            let Some((head, rest)) = path.split_first() else {
                continue;
            };
            if rest.is_empty() {
                fields.remove(head);
                continue;
            }
            // Deeper: rebuild the intermediate field with its own mask. A field
            // already removed outright by a shorter path stays removed.
            if let Some(inner) = fields.get(head).cloned() {
                let masked = Self::mask_struct_fields(&inner, &[rest.to_vec()]);
                fields.insert(head.clone(), masked);
            }
        }
        super::value::Value::Struct {
            name: name.clone(),
            fields,
        }
    }

    /// The `Function` a bare callee name denotes, for the ownership guards in
    /// [`Self::run_fresh_temp_arg_drops`] (B-2026-08-30-22).
    ///
    /// Those guards searched `Item::Function` — FREE functions only. An
    /// ASSOCIATED fn lives in an `ImplBlock` as an `ImplItem::Method`, and
    /// `Value::Function` carries only the bare name, so `H.id(...)` arrives
    /// here as `id` and matched nothing: the passthrough guard never fired and
    /// a fresh temp handed straight back out had its body run at the call AND
    /// again at the result binding's scope exit. Two bodies for one object,
    /// against one on every compiled backend.
    ///
    /// STRICTLY ADDITIVE. A free function of that name still wins, exactly as
    /// before, so no existing resolution changes; an inherent-impl method is
    /// consulted only when no free function claims the name AND exactly one
    /// impl offers it. Ambiguity yields `None`, which leaves the guards at
    /// today's answer rather than guessing — suppressing against the wrong
    /// candidate would strand the body instead of duplicating it, trading a
    /// double-run for a leak.
    ///
    /// Trait-impl methods are excluded: they are dispatched through the trait,
    /// not reachable as the `Type.method` callee this resolves.
    /// B-2026-09-01-44 — the free function OR ASSOCIATED function behind the
    /// bare callee name that reaches `eval_call`'s `Value::Function` arm.
    ///
    /// An associated call `H.apick(a, k)` is dispatched by that arm, not by
    /// `method_call.rs`: its callee Path resolves through the env key
    /// `"H.apick"` to a `Value::Function` whose `name` field is the BARE
    /// `"apick"` (`register_impl_methods` stores the method's own name). So a
    /// helper in that arm that scans `program.items` for an `Item::Function`
    /// named `"apick"` finds nothing and silently answers as it would for a
    /// closure — which is how the SAME callee got one ownership protocol under
    /// its free-function spelling and none at all under its associated one.
    ///
    /// Ambiguity is FAIL-CLOSED, for the reason `declared_param_tys_of_fn`
    /// gives at the same problem: two types both defining `apick` cannot be
    /// told apart from the name, and picking one arbitrarily would move a
    /// `Drop` body against the wrong signature. A free function wins outright,
    /// matching the callee resolution a bare `apick(...)` call would perform.
    ///
    /// INSTANCE methods are excluded (`self_param.is_none()`). They do not
    /// reach this arm at all — `t.mpick(x)` is dispatched by `method_call.rs`,
    /// whose frame owns a deliberately WIDER param set
    /// (`method_param_drop_names`) precisely because a method's arguments reach
    /// no caller-side fire in this backend. Admitting them here could therefore
    /// only mis-answer for a spelling this arm never sees, and would double the
    /// body if it ever did.
    pub(crate) fn callee_fn_for_param_ownership(
        &self,
        name: &str,
    ) -> Option<&crate::ast::Function> {
        self.callee_fn_by_bare_name(name, /* assoc_only = */ true)
    }

    /// B-2026-08-30-22 — the same resolution with instance methods ADMITTED,
    /// for `run_fresh_temp_arg_drops`' passthrough and escape guards.
    ///
    /// Those guards only ever DECLINE to fire a body the caller would otherwise
    /// run, so admitting a method is safe in the direction that matters there
    /// and was measured to stop `H.id(a: R) -> R` running its argument's body
    /// twice. The ownership TRANSFERS that
    /// [`Self::callee_fn_for_param_ownership`] drives move a body BETWEEN
    /// frames and so cannot admit one. Hence a flag rather than a second copy
    /// of this walk: two near-identical resolvers differing by one line is the
    /// shape the free and associated spellings drifted apart in to begin with.
    fn callee_fn_for_ownership_guard(&self, name: &str) -> Option<&crate::ast::Function> {
        self.callee_fn_by_bare_name(name, /* assoc_only = */ false)
    }

    /// B-2026-09-03-7 — the same guard resolution for a call whose callee is
    /// already known EXACTLY, as it is on the method path: `(type, method)`
    /// names one AST, so the bare-name scan's fail-closed ambiguity handling
    /// (two types defining `apick`) cannot mis-answer here. Falls back to the
    /// bare-name resolution for a free call.
    fn callee_fn_for_ownership_guard_of(
        &self,
        name: &str,
        method_owner: Option<&str>,
    ) -> Option<&crate::ast::Function> {
        match method_owner {
            Some(ty) => self.impl_method_ast(ty, name),
            None => self.callee_fn_for_ownership_guard(name),
        }
    }

    /// The one bare-name callee resolution both of the above share: a free
    /// function first — a bare `apick(...)` call would resolve that way too —
    /// then a single unambiguous INHERENT impl method. `assoc_only` drops the
    /// methods that take a receiver.
    fn callee_fn_by_bare_name(
        &self,
        name: &str,
        assoc_only: bool,
    ) -> Option<&crate::ast::Function> {
        if let Some(f) = self.program.items.iter().find_map(|item| match item {
            crate::ast::Item::Function(f) if f.name == name => Some(f),
            _ => None,
        }) {
            return Some(f);
        }
        let mut found: Option<&crate::ast::Function> = None;
        for item in &self.program.items {
            let crate::ast::Item::ImplBlock(imp) = item else {
                continue;
            };
            if imp.trait_name.is_some() {
                continue;
            }
            for m in imp.items.iter().filter_map(|it| match it {
                crate::ast::ImplItem::Method(f) => Some(f),
                _ => None,
            }) {
                if m.name != name || (assoc_only && m.self_param.is_some()) {
                    continue;
                }
                if found.is_some() {
                    return None;
                }
                found = Some(m);
            }
        }
        found
    }

    /// `method_owner` is `Some(type)` when the call being walked is an INSTANCE
    /// METHOD call (B-2026-09-03-7). It selects exact `(type, method)` callee
    /// resolution for the guards below, and nothing else: the walk itself is
    /// identical for a method and a free function, which is the point — before
    /// this the method path had no caller-side arg fire at all, so a method's
    /// fresh-temp argument ran its body at the callee's NLL death while every
    /// compiled backend ran it after the call returned.
    pub(crate) fn run_fresh_temp_arg_drops(
        &mut self,
        callee_name: &str,
        method_owner: Option<&str>,
        args: &[CallArg],
        arg_vals: &[Value],
    ) {
        // REVERSE argument order (B-2026-08-29-46). Every temp in this list
        // has the same live-range end -- design.md's temporary-lifetime table
        // gives "Function/method call argument | After the call returns" -- so
        // which one dies FIRST is settled by the drop-ordering rule that
        // sequences co-expiring values: the unified drop+defer stack is "a
        // single LIFO stack ordered by program-order of introduction"
        // (design.md, Drop ordering within a branch, rule 1). Argument temps
        // are introduced left to right, so they pop right to left.
        //
        // Walking forward here made `fn take(r: R, q: R)` called with two
        // fresh temps print `dR1 dR2` on this backend against `dR2 dR1` on JIT
        // and AOT -- a run-vs-build divergence no A/B parity gate could see,
        // because the COUNT agreed and only the order differed. The compiled
        // backends register these on a cleanup frame that drains LIFO, which is
        // the rule above falling out of the mechanism; this loop is the odd one
        // out, and it is also out of step with the interpreter's OWN method
        // path (`method_param_drop_names`, drained by the callee frame), which
        // already ran `h.take(R { id: 1 }, R { id: 2 })` as `dR2 dR1`.
        //
        // Only calls with two or more FRESH-temp args change: an identifier arg
        // is skipped by this walk and drops through its own binding, so a mixed
        // `take(a, R { id: 2 })` is sequenced by the two owners' relative
        // program order and was already correct.
        for (i, arg) in args.iter().enumerate().rev() {
            // B-2026-07-01-7 passthrough guard + B-2026-08-26-9 escape guard,
            // both now asked through `callee_owns_arg_beyond_call` so the two
            // method-frame consumers get the SAME answer this walk acts on
            // (B-2026-09-03-7). When the callee returns the parameter, returns
            // it via a forwarded call (B-2026-08-28-62), or stores it into
            // `self` or a `ref` param, the value is still travelling when this
            // walk would fire — the RESULT's consumer or the new home owns it.
            // B-2026-08-30-22's associated-callee resolution is inside.
            if self.callee_owns_arg_beyond_call(callee_name, method_owner, i) {
                continue;
            }
            // B-2026-08-28-16 — a PLACE tuple argument (`take(q)`) whose
            // ELEMENT escapes through the callee's return. The fresh-temp walk
            // below is gated on `ExprKind::Tuple`, so a bare identifier never
            // reaches it and its B-2026-08-28-2 filter never applies; the second
            // body here is the LOCAL's own element walk firing at `q`'s
            // live-range end on a value the callee already handed back.
            //
            // Recorded as a per-element MOVE rather than skipped at a walk,
            // because that is the mechanism the local's cleanup already
            // consults: `run_array_element_user_drops` skips exactly the
            // indices in `moved_out_tuple_elem_bodies` (B-2026-08-03-3, for
            // `let x = t.N`). A callee that returns element N is the same
            // move, reached through a call instead of a projection.
            //
            // TOP-LEVEL elements only, matching the fresh-temp filter: a deeper
            // path names something INSIDE an element, which the per-element
            // skip cannot express (B-2026-08-28-23).
            if let ExprKind::Identifier(src) = &arg.value.kind {
                if matches!(arg_vals.get(i), Some(Value::Tuple(_))) {
                    for path in self.callee_returned_param_parts(callee_name, method_owner, i) {
                        if let [crate::ast::ParamPart::TupleIndex(idx)] = path.as_slice() {
                            self.moved_out_tuple_elem_bodies.insert((src.clone(), *idx));
                        }
                    }
                }
                // B-2026-09-05-6 — the STRUCT sibling of the tuple arm above,
                // and missing since -16 landed that one: `cEsc(g)` over
                // `fn cEsc(h: Cd) -> R { let Cd { r, z } = h; r }` hands `g.r`
                // back, so the local's own field walk ran that body at `g`'s
                // live-range end on a value the callee had already given away
                // — `in dR13 got13 dR13` against a due `in got13 dR13`, on all
                // four surfaces alike. The TEMP spelling of the same call was
                // already correct: it reaches the masked walk below, whose
                // `escaping_field_paths` filter is this same question asked of
                // a value rather than of a name.
                //
                // Per-FIELD, not per-binding, for `mask_struct_fields`'s
                // reason: `fn dEsc(h: Dd) -> R { let Dd { a, b } = h; a }`
                // still owes `b`'s body in the call, and silencing `g`
                // wholesale would take it with it (measured: `dR32` is due and
                // survives). The projection spelling (`return h.r`, no
                // destructure) is the same escape by a different route and is
                // covered here too, since `fn_returns_param_part_paths`
                // classifies both.
                //
                // TOP-LEVEL fields only, matching the tuple arm and for its
                // reason: `moved_out_struct_field_bodies` is keyed by one field
                // NAME, so a deeper path has no key here. The path-keyed
                // `moved_out_nested_field_bodies` is where that case would go;
                // no measurement asks for it yet (B-2026-08-28-23's rule).
                if matches!(arg_vals.get(i), Some(Value::Struct { .. })) {
                    for path in self.callee_returned_param_parts(callee_name, method_owner, i) {
                        if let [crate::ast::ParamPart::Field(f)] = path.as_slice() {
                            self.moved_out_struct_field_bodies
                                .insert((src.clone(), f.clone()));
                        }
                    }
                }
            }
            // B-2026-07-30-11 (param-tuple leg, the A shape): a tuple
            // LITERAL arg (`take_tuple((Res { id: 41 }, 10))`) moved into
            // the callee's tuple param never ran its Drop-carrying
            // elements' bodies — the per-arg type resolution below has no
            // tuple shape. Reuse the discard walk, under the same
            // all-fresh-or-scalar element gate as `let _ = (…)`: a PLACE
            // element (`take_tuple((g, 1))`) keeps its own binding's slot
            // armed, so firing the walk too would double its body.
            if let ExprKind::Tuple(elems) = &arg.value.kind {
                if let Some(Value::Tuple(items)) = arg_vals.get(i) {
                    // Place elements stay EXCLUDED in arg position — the
                    // binding's own Drop covers them (B-2026-08-01-8's
                    // wildcard-let widening must not reach here; the full
                    // suite caught the double fire on `take_tuple((h, 20))`).
                    if self.discard_tuple_all_elems_safe(elems, items, false) {
                        // B-2026-08-28-2 — per-ELEMENT, not per-argument. The
                        // whole-param passthrough guard at the top of this loop
                        // only fires when the callee hands `p` back BARE; a
                        // callee that extracts one element and returns THAT
                        // (`fn take(p: (R, i64)) -> R { let (r, n) = p; r }`)
                        // slips past it, so the element's body ran here AND
                        // again at the result's owner. Skipping the whole walk
                        // instead would be a different soundness hole: measured
                        // on `fn take(p: (R, R)) -> R { let (a, b) = p; a }`, it
                        // suppresses element 1's only body. So drop the escaping
                        // elements from the walk and keep the rest.
                        //
                        // Unrolling the tuple here is otherwise identical to the
                        // `Value::Tuple` arm of `run_discarded_value_user_drops`,
                        // which is exactly this loop without the filter.
                        let escaping =
                            self.callee_returned_param_parts(callee_name, method_owner, i);
                        for (idx, item) in items.iter().enumerate() {
                            // TOP-LEVEL elements only: a deeper path names
                            // something inside an element, which this walk's
                            // per-element skip cannot express, so it is left at
                            // its pre-existing behaviour (B-2026-08-28-23).
                            if escaping
                                .iter()
                                .any(|p| p.as_slice() == [crate::ast::ParamPart::TupleIndex(idx)])
                            {
                                continue;
                            }
                            self.run_discarded_value_user_drops(item.clone());
                        }
                    }
                }
                continue;
            }
            // B-2026-08-30-38 — a WRAPPER argument (`one(if c { mk(1) } else
            // { mk(2) })`, the `match` spelling, a bare block) resolves through
            // its TAILS. `fresh_temp_arg_type_name` names PRODUCER shapes only,
            // and a wrapper is none of them, so this walk claimed nothing and
            // the value's user `Drop` body never ran. Codegen twin: the redirect
            // at the head of `track_inline_owned_aggregate_arg_inst`.
            let type_name: Option<String> = self
                .fresh_temp_arg_type_name(&arg.value)
                .or_else(|| self.wrapper_tail_arg_type_name(&arg.value));
            let Some(tn) = type_name else { continue };
            let Some(v) = arg_vals.get(i) else { continue };
            // B-2026-08-01-13 (c1/c5) — a fresh USER-enum arg's payload
            // bodies: own body first (below, when the enum declares Drop),
            // then the declared-type payload walk — the caller-side single
            // owner now that a destructuring callee's arm channel is
            // param-gated. Option/Result stay with their own machinery;
            // the struct fields walk below is unreachable for an
            // EnumVariant value (`value_runs_user_drop` is Struct-shaped).
            let is_user_enum_value = tn != "Option"
                && tn != "Result"
                && matches!(v, super::value::Value::EnumVariant { .. });
            if self.program.drop_method_keys.contains_key(&tn) {
                // B-2026-08-28-21 — a parent that declares its OWN `Drop` takes
                // this branch and never reaches the masked field walk below, so
                // the escaping field's body ran here and again at the result's
                // owner. `run_user_drop_body_on_value` is body-then-fields, and
                // only the FIELDS half may be masked: the parent's own body may
                // read the field it is about to hand back, so it sees the whole
                // value. Split into the two halves the helper is already made of
                // rather than masking its input.
                let escaping = self.escaping_field_paths(callee_name, method_owner, i);
                self.run_user_drop_body_only(&tn, v.clone());
                self.drop_user_drop_fields_of_value(&Self::mask_struct_fields(v, &escaping));
            }
            if is_user_enum_value {
                let v = v.clone();
                self.run_enum_payload_user_drops_value(&v);
                continue;
            }
            if !self.program.drop_method_keys.contains_key(&tn) && self.value_runs_user_drop(v) {
                // B-2026-07-30-11 SHAPE 2 — the temp's OWN type declares no
                // `Drop`, but it carries a Drop-bearing FIELD, so a body still
                // has to run when it dies (`struct Holder { r: Res }` passed as
                // `consume(Holder { r: Res { .. } })` / `consume(make())`). The
                // `drop_method_keys` gate alone skipped the whole temp, so the
                // field's resource leaked once per call. Bodies only: the
                // interpreter's value model releases the memory when the
                // `Value` is dropped, exactly as on the working `let`-bound
                // path (`invoke_user_drop_if_applicable`'s no-own-Drop arm).
                //
                // B-2026-08-28-17 — but not the fields the callee hands BACK.
                // The whole-param passthrough guard at the top of this loop only
                // fires when the callee returns `w` bare; one that extracts a
                // field and returns THAT (`fn take(w: W) -> R { let W { r, n } =
                // w; r }`, or the `w.r` spelling) slips past it, so `r`'s body
                // ran here and again at the result's owner. Skipping the whole
                // walk instead loses the bodies of the fields that really do die
                // in the call (`struct W { a: R, b: R }` returning `a` needs
                // `b`'s), so the escaping fields are masked out individually.
                //
                // Masking the VALUE rather than threading a gate through the
                // walker is the B-2026-08-03-8 pattern, and works for the same
                // reason: the walk resolves each declared field through
                // `fields.get(..)` and skips a missing one, so its two dozen
                // other callers are untouched. Codegen twin: the struct arm of
                // `track_inline_owned_aggregate_arg_inst` re-emits the walker
                // with the same field indices masked.
                let escaping = self.escaping_field_paths(callee_name, method_owner, i);
                self.drop_user_drop_fields_of_value(&Self::mask_struct_fields(v, &escaping));
            }
        }
    }

    /// The user-type name of a fresh-temp CALL ARGUMENT, or `None` when the
    /// argument is not a producer this walk owns the drop for.
    ///
    /// Extracted verbatim from `run_fresh_temp_arg_drops`'s inline match
    /// (B-2026-08-30-38) so [`Self::wrapper_tail_arg_type_name`] can ask the
    /// same question of a wrapper's tails. Behaviour is unchanged; the shapes
    /// and their reasons are the ones each row below recorded.
    fn fresh_temp_arg_type_name(&self, e: &Expr) -> Option<String> {
        match &e.kind {
            ExprKind::StructLiteral { path, .. } => {
                let n = path.last().cloned();
                // A SHARED struct literal builds a refcounted value —
                // its drop belongs to the rc machinery.
                n.filter(|n| {
                    self.find_struct_def(n)
                        .is_some_and(|d| !d.is_shared && !d.is_par)
                })
                // B-2026-08-31-8 — an enum STRUCT-VARIANT literal
                // (`eat(Sv.Hold { inner: R { .. } })`) is spelled as a struct
                // literal whose path ends in the VARIANT, so the lookup above
                // answered `None` and this walk claimed no owner: neither the
                // enum's own body nor its payload's ran, against both compiled
                // backends running both. The tuple-variant twin
                // (`eat(Tv.A(R { .. }))`) is an `ExprKind::Call` and was always
                // correct — the spelling was the whole variable.
                .or_else(|| self.qualified_struct_variant_enum_name(path))
                // B-2026-09-01-32 — the UNQUALIFIED spelling of the same
                // construction (`eat(Hold { inner: R { .. } })`). B-2026-08-31-8
                // deliberately claimed only the qualified form, because moving
                // the interpreter alone would have turned an answer all four
                // surfaces agreed on into a fresh run-vs-build divergence. The
                // codegen half lands with this, so both move together.
                .or_else(|| self.unqualified_struct_variant_enum_name(path))
            }
            ExprKind::Call { callee, .. } => match &callee.kind {
                ExprKind::Identifier(v) => self.find_enum_for_variant(v).or_else(|| {
                    // Fn-call-RETURNED Drop temp (B-2026-07-01-7):
                    // `consume(make())` — resolve the producing fn's
                    // declared return-type head. Shared types are
                    // filtered by the drop_method_keys + struct gate in
                    // the caller plus the SharedStruct value shape
                    // (run_user_drop_body_on_value binds whatever value
                    // arrived; the drop_method_keys gate is the
                    // authoritative filter).
                    self.user_fn_return_type_name(v)
                }),
                // B-2026-08-30-20 — a qualified callee is EITHER a unit
                // variant (`Sig.B`) or an ASSOCIATED FN (`H.mkr(1)`). Only the
                // first was recognised, so an argument produced by an
                // associated call carried no owner: no `Drop` body on any
                // backend and 76 bytes leaked at `-O0`, while the free-function
                // producer one spelling over was correct.
                ExprKind::Path { segments, .. } if segments.len() == 2 => self
                    .qualified_enum_variant_is_unit(&segments[0], &segments[1])
                    .map(|_| segments[0].clone())
                    .or_else(|| self.assoc_fn_return_type_name(&segments[0], &segments[1])),
                _ => None,
            },
            // Unit variant in path form (`consume(Sig.B)`).
            ExprKind::Path { segments, .. } if segments.len() == 2 => self
                .qualified_enum_variant_is_unit(&segments[0], &segments[1])
                .map(|_| segments[0].clone()),
            // Bare unit variant (`consume(B)` where B is a variant).
            //
            // B-2026-08-28-43 — via `fresh_bare_unit_variant_enum`, not a
            // bare `env.get(v).is_none()`. That test excluded this shape
            // entirely: the item pass seeds every unit variant into the
            // outermost scope as a constant, so `env.get("B")` answers
            // `Some` for the VARIANT exactly as it does for a local, and
            // `consume(B)` ran no body while `consume(E.B)` ran one.
            ExprKind::Identifier(v) => self.fresh_bare_unit_variant_enum(v),
            _ => None,
        }
    }

    /// B-2026-09-03-7 — will the CALLER's fresh-temp arg walk fire this
    /// argument's body?
    ///
    /// The single predicate behind three consumers that must agree exactly or
    /// a body is lost or doubled: this walk (which fires), the method frame's
    /// `method_param_drop_names` (which must not register what the caller
    /// fires), and `method_frame_caller_retains_args` (whose bail exists
    /// precisely for arguments nobody else fires). Keeping one predicate for
    /// all three is what stops them drifting apart, the same reason
    /// B-2026-08-30-55 gives for sharing its own gate across two spellings.
    ///
    /// SHAPE only. The walk additionally declines on value-level tests
    /// (`drop_method_keys`, `value_runs_user_drop`), and those are deliberately
    /// NOT mirrored here: they suppress a body that does not exist, so a
    /// disagreement about them can neither lose nor double one. The
    /// return/escape guards are mirrored, because those decline a body that
    /// very much does exist — its owner is just somewhere else.
    fn caller_fires_fresh_temp_arg(
        &self,
        callee_name: &str,
        method_owner: Option<&str>,
        i: usize,
        e: &Expr,
    ) -> bool {
        // Mirrors the walk's own gate EXACTLY, wrapper tails included
        // (B-2026-08-30-38): an `if`/`match`/block argument resolves through
        // its tails, and recognising only the producer shapes here left the
        // callee registering an argument the walk went on to fire — `dR21`
        // twice in `mixed-wrapper-arg-method-call`.
        (matches!(&e.kind, ExprKind::Tuple(_))
            || self.fresh_temp_arg_type_name(e).is_some()
            || self.wrapper_tail_arg_type_name(e).is_some())
            && !self.callee_owns_arg_beyond_call(callee_name, method_owner, i)
    }

    /// The two ownership guards the caller's fresh-temp walk applies, as ONE
    /// question: does this argument outlive the call because the CALLEE hands
    /// it back or stores it somewhere that outlives the frame?
    ///
    /// Extracted (B-2026-09-03-7) so the walk and the two method-frame
    /// consumers cannot drift: gating those on argument SHAPE alone skipped a
    /// registration for a CONDITIONALLY-RETURNED param that the walk then
    /// declined to fire, and the body was lost outright —
    /// `test_method_owned_param_user_drop_body_runs_once`'s `cond-return-dies`
    /// cell went from `drop 41 / 99 / drop 99` to `99 / drop 99`. Shape says
    /// "the caller COULD fire this"; this says "and nothing else claims it".
    fn callee_owns_arg_beyond_call(
        &self,
        callee_name: &str,
        method_owner: Option<&str>,
        i: usize,
    ) -> bool {
        // B-2026-09-03-7 — on the METHOD path, also ask the question by RETURN
        // TYPE, exactly as B-2026-09-04-30's receiver gate does and through the
        // same shared predicate. The structural walks below recognise a bare
        // identifier and an aggregate LITERAL, but not a CONSTRUCTOR wrap
        // (`return Option.Some(r)` — B-2026-08-31-46's open shape), and five
        // method cells double-fired on exactly that: once from this new walk,
        // once from the RESULT binding that received the value. A return type
        // that cannot carry the argument out (unit, or a named non-`Drop`
        // type) is the licence this walk needs; anything that could carry it
        // — a generic path, `Self`, a tuple — stands the walk down.
        //
        // METHOD path only. The free path reaches its escapes through the
        // per-element `escaping` filters below, and widening it to a
        // type-level test would move cells this row never measured.
        if method_owner.is_some()
            && self
                .callee_fn_for_ownership_guard_of(callee_name, method_owner)
                .is_some_and(|f| {
                    let probe = &mut |n: &str| self.type_name_runs_user_drop(n, &mut Vec::new());
                    !crate::ast::owned_self_return_is_opaque_to_receiver(f, probe)
                        // The one shape the compiled backends keep INSIDE the
                        // callee: the parameter handed to a local aggregate,
                        // which owns it from there. Firing caller-side as well
                        // ran `dR4` twice against compiled's once.
                        || crate::ast::fn_moves_param_into_local_aggregate(f, i)
                })
        {
            return true;
        }
        self.callee_fn_for_ownership_guard_of(callee_name, method_owner)
            .is_some_and(|f| {
                crate::ast::fn_returns_param(f, i)
                    // B-2026-09-05-10 — an ALL-paths return of the param wrapped
                    // in an `Option`/`Result` ctor (`fn top(r) { return
                    // Option.Some(r); }`). `fn_returns_param` above is
                    // ctor-blind by design (widening the union would stand the
                    // caller down on a CONDITIONAL ctor's dies-inside path too,
                    // B-2026-08-31-46's trap); `fn_always_returns_param` is
                    // safe because it fires only when EVERY exit hands the param
                    // back, so the result binding owns it on every path. Codegen
                    // reaches its own stand-down through the same predicate.
                    || crate::ast::fn_always_returns_param(f, i)
                    || crate::ast::fn_returns_param_via_call(self.program, f, i)
                    || crate::ast::fn_returns_param_payload(f, i)
                    || crate::ast::fn_moves_param_into_outliving_place(f, i)
                    // B-2026-08-31-46 — a conditional hand-back the callee
                    // frame owns per path; `fn_returns_param`'s union used to
                    // cover the bare form, but not a constructor wrap.
                    || crate::ast::fn_conditionally_returns_param_bare(f, i)
            })
    }

    /// B-2026-08-30-38 — [`Self::fresh_temp_arg_type_name`] reached through a
    /// value-position WRAPPER: `if` / `match` / a bare block (and the `Seq`,
    /// `Unsafe`, `LabeledBlock` spellings of the last). The construct's value
    /// comes from exactly one tail, so its type — and who owns it — is the
    /// tails' answer, not the construct's.
    ///
    /// FAIL-CLOSED on every tail, and the closure is what keeps this sound
    /// rather than merely conservative. A tail naming an OUTER binding
    /// (`one(if c { g } else { mk(2) })`) leaves that binding's own name-keyed
    /// drop armed, so claiming the merged value here would run one body TWICE.
    /// Requiring every tail to resolve — and to the SAME type — declines that
    /// shape, at the measured cost of still losing the fresh tail's body when
    /// the taken branch is the fresh one. Closing that needs a per-branch drop
    /// flag rather than a wider predicate, so it is filed on its own.
    ///
    /// Codegen twin: `expr_is_fresh_owned_branch_tail_local` +
    /// `fresh_owned_branch_tail_repr`, which share the all-tails rule and the
    /// block-local exception below.
    fn wrapper_tail_arg_type_name(&self, e: &Expr) -> Option<String> {
        match &e.kind {
            ExprKind::Block(b)
            | ExprKind::Seq(b)
            | ExprKind::Unsafe(b)
            | ExprKind::LabeledBlock { body: b, .. } => {
                let tail = b.final_expr.as_deref()?;
                self.tail_arg_type_name(tail).or_else(|| {
                    // A binding the block declares ITSELF (`one({ let t =
                    // mk(5); t })`) has no reader after the tail: the block ends
                    // there and the move disarms the local's own drop, so the
                    // value reaches the call owned by nobody. That is the
                    // opposite of an OUTER binding tail, which is why this
                    // exception is safe where the general decline is required.
                    self.block_local_binding_tail_rhs(b, tail)
                        .and_then(|rhs| self.tail_arg_type_name(rhs))
                })
            }
            ExprKind::If {
                then_block,
                else_branch,
                ..
            } => {
                let then_ty = self.tail_arg_type_name(then_block.final_expr.as_deref()?)?;
                let else_ty = self.tail_arg_type_name(else_branch.as_deref()?)?;
                (then_ty == else_ty).then_some(then_ty)
            }
            ExprKind::Match { arms, .. } => {
                let mut it = arms.iter();
                let first = self.tail_arg_type_name(&it.next()?.body)?;
                for a in it {
                    if self.tail_arg_type_name(&a.body)? != first {
                        return None;
                    }
                }
                Some(first)
            }
            _ => None,
        }
    }

    /// One tail's worth of [`Self::wrapper_tail_arg_type_name`], recursing
    /// through nested wrappers so an arm-wrapped block and an `else if` chain
    /// resolve like the shapes that spell them flat.
    fn tail_arg_type_name(&self, e: &Expr) -> Option<String> {
        self.fresh_temp_arg_type_name(e)
            .or_else(|| self.wrapper_tail_arg_type_name(e))
            .or_else(|| self.cond_moved_place_tail_type_name(e))
    }

    /// B-2026-08-30-50 — does this expression have at least one tail that
    /// MINTS a fresh owned temp? The gate on seeding a call argument as an
    /// escaping site, and the twin of codegen's `first_minting_branch_tail`
    /// asked as a yes/no.
    ///
    /// Only a minting tail gives the argument walk a producer to name a type
    /// from, so only then does something take over ownership from the binding
    /// the seed disarms. Kept separate from [`Self::tail_arg_type_name`]
    /// because that one deliberately ALSO admits a place tail — which is the
    /// very thing this must not count.
    pub(crate) fn wrapper_has_minting_tail(&self, e: &Expr) -> bool {
        match &e.kind {
            ExprKind::Block(b)
            | ExprKind::Seq(b)
            | ExprKind::Unsafe(b)
            | ExprKind::LabeledBlock { body: b, .. } => {
                let Some(tail) = b.final_expr.as_deref() else {
                    return false;
                };
                self.wrapper_has_minting_tail(tail)
                    || self
                        .block_local_binding_tail_rhs(b, tail)
                        .is_some_and(|rhs| self.wrapper_has_minting_tail(rhs))
            }
            ExprKind::If {
                then_block,
                else_branch,
                ..
            } => {
                then_block
                    .final_expr
                    .as_deref()
                    .is_some_and(|t| self.wrapper_has_minting_tail(t))
                    || else_branch
                        .as_deref()
                        .is_some_and(|t| self.wrapper_has_minting_tail(t))
            }
            ExprKind::Match { arms, .. } => {
                arms.iter().any(|a| self.wrapper_has_minting_tail(&a.body))
            }
            _ => self.fresh_temp_arg_type_name(e).is_some(),
        }
    }

    /// B-2026-08-30-50 — a wrapper tail that hands out a BINDING, admitted
    /// because the conditional-move machinery disarms that binding on exactly
    /// the path where this tail runs.
    ///
    /// B-2026-08-30-38 had to DECLINE this shape, and the decline was correct
    /// on its own terms: the binding kept its own name-keyed drop, so claiming
    /// the merged value too would have run one body twice. What changes here is
    /// not the predicate's boldness but the FACT it rests on -- seeding a call
    /// argument as an escaping site (`note_escaping_site`) makes
    /// `record_conditional_move_tail` fire at the taken arm's tail, which is
    /// the runtime bit that turns "maybe moved" into "moved". The binding is
    /// then disarmed on the path that hands it over and armed on every other,
    /// so the argument temp is the single owner rather than a second one.
    ///
    /// GATED ON THE SITE, not on the shape, and that is the whole safety
    /// argument: the span must be one the seeding actually marked. An
    /// identifier tail in a position the seeding does not reach keeps its
    /// binding armed, so admitting it there would be the double fire again.
    /// The two are wired to the same set, so they cannot drift apart.
    ///
    /// Codegen twin: the `Identifier` arm of `arg_producer_mints_fresh_owned_temp`,
    /// gated on the flag that machinery creates for the same binding.
    fn cond_moved_place_tail_type_name(&self, e: &Expr) -> Option<String> {
        let ExprKind::Identifier(name) = &e.kind else {
            return None;
        };
        if !self
            .cond_move_escaping_sites
            .contains(&(e.span.offset, e.span.length))
        {
            return None;
        }
        let tn = match self.env.get(name)? {
            Value::Struct { name, .. } => name,
            Value::EnumVariant { enum_name, .. } => enum_name,
            _ => return None,
        };
        self.program
            .drop_method_keys
            .contains_key(&tn)
            .then_some(tn)
    }

    /// The `let` RHS that produced a block's identifier TAIL, when the block
    /// itself declares that identifier (B-2026-08-30-38). `None` when the tail
    /// is not an identifier, when the block does not bind it (an outer binding,
    /// which keeps its own drop), or when it arrives through a shape with no
    /// single producer to name — a destructuring pattern or a `LetUninit`.
    ///
    /// The LAST binding `let` wins: an earlier one is shadowed, so taking the
    /// first would classify `{ let t = mk(1); let t = outer; t }` on an RHS
    /// that no longer produces the tail's value. Codegen twin of the same name.
    fn block_local_binding_tail_rhs<'e>(
        &self,
        b: &'e crate::ast::Block,
        tail: &Expr,
    ) -> Option<&'e Expr> {
        let ExprKind::Identifier(name) = &tail.kind else {
            return None;
        };
        let mut rhs: Option<&Expr> = None;
        for st in &b.stmts {
            match &st.kind {
                crate::ast::StmtKind::Let { pattern, value, .. } => match &pattern.kind {
                    crate::ast::PatternKind::Binding(bn) if bn == name => rhs = Some(value),
                    _ if pattern.binding_names().iter().any(|n| n == name) => return None,
                    _ => {}
                },
                crate::ast::StmtKind::LetElse { pattern, .. }
                    if pattern.binding_names().iter().any(|n| n == name) =>
                {
                    return None
                }
                crate::ast::StmtKind::LetUninit { name: n, .. } if n == name => return None,
                _ => {}
            }
        }
        rhs
    }

    /// B-2026-07-11-26 (interp parity with the codegen
    /// `materialize_freshtemp_enum_scrutinee` user-Drop hook): the type name of
    /// a FRESH-temp enum scrutinee whose type carries a user `impl Drop`, else
    /// `None`. A fresh-temp enum scrutinee (`if let V(x) = make()`,
    /// `while let V(x) = it.next()`, `match make() { … }`, `let V(x) = make()
    /// else …`) must run its `Drop` body exactly as a bound `let s = make()`
    /// would — pre-fix it was silently skipped. Only a fresh temp (a
    /// call / method-call result) qualifies; a place scrutinee (bound var,
    /// field, index) is owned elsewhere and drops through its owner. Gated on
    /// `drop_method_keys` (the authoritative user-Drop filter).
    pub(crate) fn freshtemp_scrutinee_user_drop_type(&self, scrutinee: &Expr) -> Option<String> {
        match &scrutinee.kind {
            ExprKind::Call { .. } | ExprKind::MethodCall { .. } => {}
            _ => return None,
        }
        let key = crate::resolver::SpanKey(scrutinee.span.offset, scrutinee.span.length);
        let name = match self.typecheck_result.expr_types.get(&key)? {
            crate::typechecker::Type::Named { name, .. } => name.clone(),
            _ => return None,
        };
        if !self.program.drop_method_keys.contains_key(&name) {
            return None;
        }
        Some(name)
    }

    /// Declared return-type HEAD name of an ASSOCIATED fn (`Type.method`),
    /// for the same fn-returned Drop temp classification as
    /// [`Self::user_fn_return_type_name`] (B-2026-08-30-20).
    ///
    /// The qualified spelling reached the classifier's `Path` arm, which
    /// recognised only a qualified UNIT VARIANT — so `s1(H.mkr(1))` was
    /// classified as carrying no user `Drop` and its argument had no owner on
    /// ANY backend, where the free-function producer `s1(mkr(1))` was fine.
    /// All backends agreeing on the omission is why no A/B parity gate could
    /// see it; only an absolute expectation against the free spelling can.
    ///
    /// FAIL-CLOSED on an ambiguous name, matching the sibling lookups in this
    /// file: an impl whose target type is not a plain path, or two impls
    /// offering the same `Type.method`, yields `None` and leaves ownership
    /// exactly as it was. Guessing here would run a `Drop` body against the
    /// wrong type, which is worse than the leak it would be papering over.
    pub(crate) fn assoc_fn_return_type_name(&self, ty: &str, method: &str) -> Option<String> {
        let mut found: Option<String> = None;
        for item in &self.program.items {
            let crate::ast::Item::ImplBlock(imp) = item else {
                continue;
            };
            // Inherent impls only: a TRAIT impl's method is dispatched on the
            // trait, not reachable as `Type.method` in this position.
            if imp.trait_name.is_some() {
                continue;
            }
            let crate::ast::TypeKind::Path(tp) = &imp.target_type.kind else {
                continue;
            };
            if tp.segments.last().map(|s| s.as_str()) != Some(ty) {
                continue;
            }
            for m in imp.items.iter().filter_map(|it| match it {
                crate::ast::ImplItem::Method(f) => Some(f),
                _ => None,
            }) {
                if m.name != method {
                    continue;
                }
                let ret = m.return_type.as_ref().and_then(|te| match &te.kind {
                    crate::ast::TypeKind::Path(p) => p.segments.last().cloned(),
                    _ => None,
                })?;
                if found.is_some() {
                    return None;
                }
                found = Some(ret);
            }
        }
        found
    }

    /// Declared return-type HEAD name of a user free function, for the
    /// fn-returned Drop temp classification (B-2026-07-01-7). `None` for
    /// unknown names, methods, and functions without a declared return.
    pub(crate) fn user_fn_return_type_name(&self, fn_name: &str) -> Option<String> {
        self.program.items.iter().find_map(|item| match item {
            crate::ast::Item::Function(f) if f.name == fn_name => {
                f.return_type.as_ref().and_then(|te| match &te.kind {
                    crate::ast::TypeKind::Path(p) => p.segments.last().cloned(),
                    _ => None,
                })
            }
            _ => None,
        })
    }

    /// Recognize the `with_provider[R](provider, closure)` call shape. Returns
    /// the resource name, the provider argument, and the closure argument if
    /// the callee is `Index(Ident("with_provider") | Path(["with_provider"]), R)`
    /// where `R` is a bare identifier or a single-segment path, and `args` has
    /// exactly two entries with no label. Anything else returns `None` so the
    /// normal call dispatch runs.
    fn match_with_provider<'e>(
        callee: &'e Expr,
        args: &'e [CallArg],
    ) -> Option<(String, &'e Expr, &'e Expr)> {
        let ExprKind::Index { object, index } = &callee.kind else {
            return None;
        };
        let is_with_provider = match &object.kind {
            ExprKind::Identifier(n) => n == "with_provider",
            ExprKind::Path { segments, .. } => segments.as_slice() == ["with_provider"],
            _ => false,
        };
        if !is_with_provider {
            return None;
        }
        let resource = match &index.kind {
            ExprKind::Identifier(n) => n.clone(),
            ExprKind::Path { segments, .. } => segments.last().cloned()?,
            _ => return None,
        };
        if args.len() != 2 {
            return None;
        }
        Some((resource, &args[0].value, &args[1].value))
    }

    /// Configurable ambient logging (phase-8 line 156, interpreter half).
    /// Handles `Log.set_min_level` / `set_exporter` / `reset` (write the
    /// ambient state) and `Log.{trace,debug,info,warn,error}` (consult it).
    ///
    /// Returns `Some(Unit)` when the call is fully handled here — a config
    /// setter, a *dropped* level call (below the min level), or a level call
    /// routed to a *registered* sink. Returns `None` for a level call in the
    /// default configuration (no registered sink) so the caller falls through
    /// to the existing `Log.*` Kāra body (the per-call `StdoutExporter` stdout
    /// path), and for any non-`Log` callee.
    ///
    /// A dropped level call does **not** evaluate its message argument — the
    /// standard "don't pay for filtered logs" logging semantic. (Codegen does
    /// not yet honor any of this; a compiled `Log.*` always emits to stdout.)
    /// Configurable ambient logging builtins (phase-8 line 156). Back the
    /// `tracing_{level_enabled,emit_event,set_min_level,reset}` builtins the
    /// rewritten `Log.*` / `Log.set_min_level` / `Log.reset` bodies lower
    /// through, reading/writing the same `tracing_min_level` /
    /// `tracing_exporter` state as [`Self::try_eval_log_call`]. Returns
    /// `None` for any other callee. `Log.set_exporter` is *not* handled here
    /// — it's intercepted at the `Log.set_exporter` call shape in
    /// `try_eval_log_call`.
    fn try_eval_tracing_config_builtin(
        &mut self,
        callee: &Expr,
        args: &[CallArg],
    ) -> Option<Value> {
        let name = match &callee.kind {
            ExprKind::Identifier(n) => n.as_str(),
            ExprKind::Path { segments, .. } if segments.len() == 1 => segments[0].as_str(),
            _ => return None,
        };
        match name {
            "tracing_level_enabled" => {
                let rank = match args.first().map(|a| self.eval_expr_inner(&a.value)) {
                    Some(Value::Int(r)) => r,
                    _ => return Some(Value::Bool(true)),
                };
                Some(Value::Bool(rank >= self.tracing_min_level.into()))
            }
            "tracing_set_min_level" => {
                if let Some(Value::Int(r)) = args.first().map(|a| self.eval_expr_inner(&a.value)) {
                    self.tracing_min_level = narrow_to_i64(r);
                }
                Some(Value::Unit)
            }
            "tracing_reset" => {
                self.tracing_min_level = 0;
                self.tracing_exporter = None;
                Some(Value::Unit)
            }
            "tracing_emit_event" => {
                let event = args.first().map(|a| self.eval_expr_inner(&a.value))?;
                // Registered sink if one is set, else the default
                // `StdoutExporter` (an empty struct) — the same dispatch the
                // registered-sink arm of `try_eval_log_call` performs.
                let sink = self
                    .tracing_exporter
                    .clone()
                    .unwrap_or_else(|| Value::Struct {
                        name: "StdoutExporter".to_string(),
                        fields: HashMap::new(),
                    });
                let sink_type = self.value_type_name(&sink);
                if let Some(func) = self.env.get(&format!("{sink_type}.export_event")) {
                    self.invoke_function_value(func, vec![sink, event]);
                }
                Some(Value::Unit)
            }
            _ => None,
        }
    }

    fn try_eval_log_call(&mut self, callee: &Expr, args: &[CallArg]) -> Option<Value> {
        let method = match &callee.kind {
            ExprKind::Path { segments, .. } if segments.len() == 2 && segments[0] == "Log" => {
                segments[1].as_str()
            }
            _ => return None,
        };

        match method {
            "set_min_level" => {
                if let Some(Value::String(name)) =
                    args.first().map(|a| self.eval_expr_inner(&a.value))
                {
                    if let Some(rank) = log_level_rank(&name) {
                        self.tracing_min_level = rank;
                    }
                }
                Some(Value::Unit)
            }
            "set_exporter" => {
                if let Some(v) = args.first().map(|a| self.eval_expr_inner(&a.value)) {
                    self.tracing_exporter = Some(v);
                }
                Some(Value::Unit)
            }
            "reset" => {
                self.tracing_min_level = 0;
                self.tracing_exporter = None;
                Some(Value::Unit)
            }
            "trace" | "debug" | "info" | "warn" | "error" => {
                let rank = log_level_rank(method).unwrap_or(0);
                if rank < self.tracing_min_level {
                    // Below the threshold — drop without evaluating the message.
                    return Some(Value::Unit);
                }
                let Some(sink) = self.tracing_exporter.clone() else {
                    // Default configuration: let the `Log.*` body emit to stdout.
                    return None;
                };
                // Registered sink: build the event via the Kāra `LogEvent.<level>`
                // constructor (so active-span auto-stamping is preserved) and
                // dispatch the sink's `export_event`.
                let message = args.first().map(|a| self.eval_expr_inner(&a.value))?;
                let event = match self.env.get(&format!("LogEvent.{method}")) {
                    Some(ctor) => self.invoke_function_value(ctor, vec![message]),
                    None => return Some(Value::Unit),
                };
                let sink_type = self.value_type_name(&sink);
                if let Some(func) = self.env.get(&format!("{sink_type}.export_event")) {
                    self.invoke_function_value(func, vec![sink, event]);
                }
                Some(Value::Unit)
            }
            _ => None,
        }
    }

    /// Recognize `with_span(span, ||body)` (phase-8 line 153). Plain
    /// `Call` with an `Ident("with_span") | Path(["with_span"])` callee and
    /// two unlabeled args. Mirror of `codegen::helpers::match_with_span_call`.
    fn match_with_span<'e>(callee: &'e Expr, args: &'e [CallArg]) -> Option<(&'e Expr, &'e Expr)> {
        let is_with_span = match &callee.kind {
            ExprKind::Identifier(n) => n == "with_span",
            ExprKind::Path { segments, .. } => segments.as_slice() == ["with_span"],
            _ => false,
        };
        if !is_with_span || args.len() != 2 {
            return None;
        }
        Some((&args[0].value, &args[1].value))
    }

    /// Execute `with_span(span, ||body)`: read `span.span_id`, push it onto
    /// the active-span stack, invoke the body closure, pop on every exit
    /// path (cf / `?` / panic / normal), and return the body's value.
    /// Parallels `eval_with_provider`.
    fn eval_with_span(&mut self, span_expr: &Expr, closure_expr: &Expr, span: &Span) -> Value {
        let span_val = self.eval_expr_inner(span_expr);
        if self.check_cf() {
            return Value::Unit;
        }
        let span_id = match &span_val {
            Value::Struct { fields, .. } => match fields.get("span_id") {
                Some(Value::Int(id)) => *id,
                _ => 0,
            },
            _ => 0,
        };

        let closure = self.eval_expr_inner(closure_expr);
        if self.check_cf() {
            return Value::Unit;
        }

        self.active_span_stack.push(narrow_to_i64(span_id));
        let result = self.invoke_zero_arg_closure(closure, span);
        self.active_span_stack.pop();
        result
    }

    /// Execute `with_provider[R](provider, closure)`. Evaluates `provider`,
    /// pushes a frame binding `R` to the (`Arc`-wrapped) provider value,
    /// evaluates `closure` (must produce a callable `Value::Function`), invokes
    /// it with no arguments, then pops the frame on any exit path — including
    /// panics, `?` propagation, `ExitUnwind`, and runtime errors — so a test
    /// that fails mid-closure can't leak a provider binding into the next
    /// test. The returned value is whatever the closure produced.
    fn eval_with_provider(
        &mut self,
        resource: &str,
        provider_expr: &Expr,
        closure_expr: &Expr,
        span: &Span,
    ) -> Value {
        let provider = self.eval_expr_inner(provider_expr);
        if self.check_cf() {
            return Value::Unit;
        }

        self.push_provider_frame();
        self.bind_provider(resource.to_string(), provider);

        let closure = self.eval_expr_inner(closure_expr);
        if self.check_cf() {
            self.pop_provider_frame();
            return Value::Unit;
        }

        let result = self.invoke_zero_arg_closure(closure, span);
        self.pop_provider_frame();
        result
    }

    /// Execute a `providers { R => e, ... } in { body }` block.
    /// Evaluate-all-then-scope per design.md: every provider expression runs
    /// *before* any frame is pushed, so a failure in a later expression leaves
    /// no scopes to unwind. One frame is pushed per binding, matching the
    /// nested `with_provider` desugaring so future escape-check machinery can
    /// attribute captures to specific resources. Frames are popped on every
    /// exit path (normal return, `?`, panic, `ExitUnwind`, runtime error) so
    /// bindings cannot leak past the block.
    pub(crate) fn eval_providers_block(
        &mut self,
        bindings: &[ProviderBinding],
        body: &Block,
    ) -> Value {
        // Phase 1: evaluate all provider expressions. Stop on the first cf.
        let mut values: Vec<(String, Value)> = Vec::with_capacity(bindings.len());
        for b in bindings {
            let v = self.eval_expr_inner(&b.value);
            if self.check_cf() {
                return Value::Unit;
            }
            values.push((b.resource.clone(), v));
        }

        // Phase 2: push one frame per binding (outer-to-inner source order)
        // and bind each provider.
        let frames_pushed = values.len();
        for (resource, provider) in values {
            self.push_provider_frame();
            self.bind_provider(resource, provider);
        }

        // Phase 3: evaluate the body; value is the block's value.
        let result = match self.eval_block_inner(body) {
            Ok(v) => v,
            Err(ControlFlow::Return(v)) => v,
            Err(cf) => self.set_cf(cf),
        };

        // Phase 4: pop every frame we pushed — even on an error/unwind path.
        for _ in 0..frames_pushed {
            self.pop_provider_frame();
        }
        result
    }

    /// Invoke a `Value::Function` closure taking no arguments. Used by
    /// `with_provider` to run the body closure; factored out so future
    /// fixtures (`providers { }`, multi-attribute test wrapping) can reuse the
    /// invocation path without duplicating frame-management boilerplate.
    pub(super) fn invoke_zero_arg_closure(&mut self, callee_val: Value, span: &Span) -> Value {
        let callee_variant = callee_val.variant_name();
        match callee_val {
            Value::Function {
                body, closure_env, ..
            } => {
                self.env.push_scope();
                if let Some(ref captured) = closure_env {
                    for (k, v) in captured {
                        self.env.define(k.clone(), v.clone());
                    }
                }
                let result = self.eval_body_growing(&body);
                self.env.pop_scope();
                match result {
                    Ok(v) => v,
                    Err(ControlFlow::Return(v)) => v,
                    Err(cf) => self.set_cf(cf),
                }
            }
            _ => unreachable!(
                "with_provider closure at {}:{} was Value::{} not Function; \
                 either an interpreter codepath produced the wrong variant \
                 or the typechecker accepted a non-closure body argument",
                span.line, span.column, callee_variant
            ),
        }
    }

    /// Shared body for `Entry.or_insert(default)` and the vacant arm of
    /// `Entry.or_insert(default)` / `or_insert_with(f)`: ensure the slot for
    /// `key` exists in the live Map named by `map_var` (inserting `default`
    /// when the key is absent), then return a `Value::MapSlotRef` — a genuine
    /// `mut ref V` into that slot. Write-through mutations applied to the ref
    /// (`*r += 1`, `r += 1`, `*r = v`, or `.push(x)` on an Arc-backed element)
    /// reach the map through `Env`'s `MapSlotRef` resolution (get/set choke
    /// points). Returns `Value::Unit` when the entry has no `map_var` (chain
    /// rooted at a non-identifier receiver) or the binding doesn't resolve to
    /// a Map — the mutation is then unobservable, matching the prior
    /// best-effort behaviour for that degenerate shape.
    pub(crate) fn entry_or_insert_ref(
        &mut self,
        map_var: Option<String>,
        key: Value,
        default: Value,
    ) -> Value {
        let Some(name) = map_var else {
            return Value::Unit;
        };
        // Resolved as a PLACE (B-2026-08-18-34), so a map in a struct field
        // (`h.buckets`, `self.buckets`) inserts into the real map rather than
        // degrading to `Value::Unit`. Mutating in place also drops the
        // whole-map clone/write-back the name-keyed `get`/`set` pair did.
        match self.env.map_place_mut(&name) {
            Some(Value::Map(m)) => {
                {
                    let mut m = m.write().unwrap();
                    if !m.contains_key(&key) {
                        m.insert(key.clone(), default);
                    }
                }
                Value::MapSlotRef {
                    map_var: name,
                    key: Box::new(key),
                }
            }
            // SortedMap sibling: insert-if-absent into the BTreeMap by key, then
            // hand back the same `MapSlotRef` shape (its get/set choke points are
            // taught to resolve a SortedMap slot by key). Mirrors the Map arm.
            Some(Value::SortedMap(m)) => {
                m.entry(super::value::OrdValue(key.clone()))
                    .or_insert(default);
                Value::MapSlotRef {
                    map_var: name,
                    key: Box::new(key),
                }
            }
            _ => Value::Unit,
        }
    }

    /// `collect_all_vec(fs)` — the gather-all-errors homogeneous parallel
    /// primitive (design.md § Concurrency Semantics > `collect_all_vec` for
    /// homogeneous branches). Runs EVERY closure in the input
    /// `Vec[Fn() -> Result[T, E]]` to completion and returns one `Result`
    /// per input, position-bound: `output[i]` is the outcome of `fs[i]`.
    /// Unlike fail-fast `par {}`, an `Err` from one branch does NOT cancel
    /// its siblings — only a panic dominates (it short-circuits the gather
    /// via `pending_cf`, per design.md § Parallel Failure and Cleanup).
    ///
    /// The interpreter runs the branches **sequentially** in input order.
    /// This is observably correct for `collect_all_vec`: the result vector
    /// is position-bound (not completion-ordered), every branch runs to
    /// completion regardless of peer `Err`, and parallelism is unobservable
    /// absent shared mutation — which the interpreter models with real OS
    /// threads only for explicit `par {}` (see `eval_par_block`). Codegen
    /// (phase-6 slice 1b) provides the actually-parallel lowering.
    /// `collect_all(|| a, || b, …)` — the heterogeneous fixed-arity gather.
    /// Each argument is a closure; invoke every one to completion and gather
    /// the results into a position-bound tuple `(Result[A1,E1], …)`. Same
    /// gather semantics as `collect_all_vec` (no fail-fast on `Err`; a
    /// panicking branch dominates via `pending_cf`), but heterogeneous and
    /// returning a `Value::Tuple` rather than a `Value::Array`. The
    /// interpreter runs the branches sequentially in source order
    /// (observably correct: position-bound, every branch runs).
    pub(crate) fn eval_collect_all(&mut self, args: &[CallArg], _span: &Span) -> Value {
        // Arity (2..=8) and the closure-`Result` branch shapes are
        // guaranteed by the typechecker's `infer_collect_all`.
        let mut results: Vec<Value> = Vec::with_capacity(args.len());
        for arg in args {
            let closure = self.eval_expr_inner(&arg.value);
            if self.pending_cf.is_some() {
                return Value::Tuple(results);
            }
            let r = self.invoke_function_value(closure, Vec::new());
            // A panicking / diverging branch dominates: stop and let the
            // pending control-flow signal propagate (the partial tuple is
            // never observed).
            if self.pending_cf.is_some() {
                return Value::Tuple(results);
            }
            results.push(r);
        }
        Value::Tuple(results)
    }

    /// Free-function `spawn(closure)` — unscoped task creation (design.md
    /// § Explicit Concurrency; `runtime/stdlib/task_group.kara`). Returns a
    /// `Value::TaskHandle` carrying the child's result; `.join()` delivers
    /// it. Exactly one closure argument, guaranteed by the stdlib
    /// `#[compiler_builtin]` signature `fn spawn[T](f: OnceFn() -> T) ->
    /// TaskHandle[T]`. Runs the child eagerly on the calling thread — see
    /// [`Self::eval_spawn_closure`] for why the interpreter's eager model
    /// matches the parallel codegen for the shapes ScopeLocal permits.
    pub(crate) fn eval_spawn(&mut self, args: &[CallArg], _span: &Span) -> Value {
        let Some(arg0) = args.first() else {
            return Value::TaskHandle(Box::new(Value::Unit));
        };
        self.eval_spawn_closure(arg0)
    }

    /// Eagerly run a spawned closure and box its result into a
    /// `Value::TaskHandle`. Shared by free `spawn(closure)` and
    /// `TaskGroup.spawn(closure)`.
    ///
    /// The tree-walk interpreter has no deferred-task substrate for the
    /// *dynamic* spawn/join shape: `par {}` can use `std::thread::scope`
    /// because its branches are lexically bounded, but a `TaskHandle` can be
    /// `.join()`ed at an arbitrary later point, and the interpreter holds
    /// `program` / `typecheck_result` as borrows that cannot cross into a
    /// `'static` `std::thread::spawn`. So a spawned child runs synchronously
    /// at its spawn site and its result is stashed for the later `.join()`.
    /// This is observably identical to the genuinely-parallel codegen for
    /// the order-independent fan-out/join programs the typechecker's
    /// `ScopeLocal` rules permit (a handle cannot escape its spawning scope,
    /// so cross-task communication is confined to shared `Atomic`/`Mutex`
    /// cells, whose interpreter models are already thread-safe). A panicking
    /// child dominates via `pending_cf` — the same fail-fast the caller sees
    /// from `par {}` and `collect_all`.
    pub(crate) fn eval_spawn_closure(&mut self, closure_arg: &CallArg) -> Value {
        let closure = self.eval_expr_inner(&closure_arg.value);
        if self.pending_cf.is_some() {
            return Value::TaskHandle(Box::new(Value::Unit));
        }
        let result = self.invoke_function_value(closure, Vec::new());
        // On a panicking child `pending_cf` is now set and `result` is the
        // set_cf sentinel; box it anyway — the caller propagates the signal
        // before the handle is ever `.join()`ed (mirrors `collect_all`).
        Value::TaskHandle(Box::new(result))
    }

    pub(crate) fn eval_collect_all_vec(&mut self, args: &[CallArg], _span: &Span) -> Value {
        // Arity (exactly one `Vec[Fn() -> Result[T, E]]`) is guaranteed by
        // the typechecker against the stdlib `#[compiler_builtin]` signature.
        let Some(arg0) = args.first() else {
            return Value::Array(Arc::new(RwLock::new(Vec::new())));
        };
        let fs_val = self.eval_expr_inner(&arg0.value);
        if self.pending_cf.is_some() {
            return Value::Array(Arc::new(RwLock::new(Vec::new())));
        }
        // Snapshot the closures out from under the shared `Arc<RwLock>`
        // before invoking any — a branch body may re-enter the interpreter
        // against the same array, and `RwLock` is non-reentrant on one
        // thread (same caveat documented on `invoke_value_comparator`).
        let closures: Vec<Value> = match &fs_val {
            Value::Array(rc) => rc.read().unwrap().clone(),
            _ => return Value::Array(Arc::new(RwLock::new(Vec::new()))),
        };
        let mut results: Vec<Value> = Vec::with_capacity(closures.len());
        for closure in closures {
            let r = self.invoke_function_value(closure, Vec::new());
            // A panicking / diverging branch dominates: stop the gather and
            // let the pending control-flow signal propagate upward (panic
            // cancels siblings; the partial result vector is never observed).
            if self.pending_cf.is_some() {
                return Value::Array(Arc::new(RwLock::new(results)));
            }
            results.push(r);
        }
        Value::Array(Arc::new(RwLock::new(results)))
    }

    /// Invoke a `Value::Function` (closure or named function) with
    /// pre-evaluated argument values. Used by iterator adaptors that
    /// receive a closure as an already-evaluated value rather than via the
    /// AST path `eval_call` takes (no CICO write-back, no default-value
    /// evaluation, no type-substitution stack — the closure is fully
    /// monomorphic by the time it reaches an adaptor step).
    pub(crate) fn invoke_function_value(&mut self, callee: Value, arg_vals: Vec<Value>) -> Value {
        let Value::Function {
            param_patterns,
            body,
            closure_env,
            ..
        } = callee
        else {
            return Value::Unit;
        };
        self.env.push_scope();
        if let Some(captured) = closure_env {
            for (k, v) in captured {
                self.env.define(k, v);
            }
        }
        for (i, pat) in param_patterns.iter().enumerate() {
            if let Some(v) = arg_vals.get(i) {
                self.bind_pattern(pat, v.clone());
            }
        }
        let result = self.eval_body_growing(&body);
        self.env.pop_scope();
        match result {
            Ok(v) => v,
            Err(ControlFlow::Return(v)) => v,
            Err(cf) => self.set_cf(cf),
        }
    }

    /// Invoke a comparator closure (a `Value::Function` produced by an
    /// `ExprKind::Closure`) on two values and translate the returned
    /// `Ordering` enum variant into `std::cmp::Ordering`. Used by the
    /// closure-taking sort methods (`sort_by`, `sorted_by`) to bridge
    /// the user's `|a, b| ... -> Ordering` to Rust's `Vec::sort_by`.
    ///
    /// **Caller invariant — no `RwLock` held.** `std::sync::RwLock` is
    /// non-reentrant on the same thread; the user closure body may
    /// re-enter the interpreter on the same array (e.g. an inner
    /// `.len()` call), which would deadlock or panic against a held
    /// write guard. Each call site snapshots the source vector before
    /// invoking sort so no lock is live during the comparator callbacks.
    pub(crate) fn invoke_value_comparator(
        &mut self,
        cmp_val: &Value,
        a: Value,
        b: Value,
        method_label: &str,
    ) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        let result = self.invoke_function_value(cmp_val.clone(), vec![a, b]);
        match &result {
            Value::EnumVariant {
                enum_name, variant, ..
            } if enum_name == "Ordering" => match variant.as_str() {
                "Less" => Ordering::Less,
                "Equal" => Ordering::Equal,
                "Greater" => Ordering::Greater,
                other => panic!(
                    "{method_label}: comparator returned Ordering.{other} \
                     which is not one of Less/Equal/Greater"
                ),
            },
            _ => panic!(
                "{method_label}: comparator must return Ordering, returned a different value"
            ),
        }
    }
}

/// The built-in `default()` value for a primitive type name, or `None` for a
/// non-primitive (which routes to its `<Type>.default` function instead). The
/// interpreter models every integer width with `Value::Int` and both floats
/// with `Value::Float`, so the zero values collapse accordingly. Matches the
/// primitive-default constants codegen emits in `compile_assoc_call`.
fn primitive_default_value(type_name: &str) -> Option<Value> {
    match type_name {
        "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64" | "usize" | "isize" => {
            Some(Value::Int(0))
        }
        "f32" | "f64" => Some(Value::Float(0.0)),
        "bool" => Some(Value::Bool(false)),
        "char" => Some(Value::Char('\0')),
        "String" | "str" => Some(Value::String(String::new())),
        _ => None,
    }
}

/// Numeric rank of a log level for the `Log.set_min_level` filter
/// (trace < debug < info < warn < error). `None` for an unrecognized
/// name — `set_min_level` leaves the threshold unchanged in that case.
fn log_level_rank(level: &str) -> Option<i64> {
    match level {
        "trace" => Some(0),
        "debug" => Some(1),
        "info" => Some(2),
        "warn" => Some(3),
        "error" => Some(4),
        _ => None,
    }
}

impl super::Interpreter<'_> {
    /// Stamp a freshly built `Map` / `Set` with the hasher its construction
    /// site's type annotation names (B-2026-08-21-6).
    ///
    /// `pending_let_ty` is the interpreter's existing channel for "the
    /// declared type of the slot this expression is initializing" — a `let`'s
    /// annotation (`eval_stmt`) or a struct field's declared type
    /// (`eval_struct_literal`). Those are exactly the two positions from which
    /// codegen reads the same argument, so the two backends select from the
    /// same information rather than each guessing.
    ///
    /// Every other construction site defaults to `SipHash13BuildHasher`, which
    /// is both the spec's default and the safe direction to fail: a container
    /// that cannot see an annotation gets the DoS-resistant hasher, never the
    /// fast one.
    pub(crate) fn new_hash_container(
        &mut self,
        container: crate::interpreter::Value,
    ) -> crate::interpreter::Value {
        use crate::hasher_kind::HasherKind;
        use crate::interpreter::Value;

        let Some(te) = self.pending_let_ty.as_ref() else {
            return container;
        };
        // The hasher is NOT in the type expression any more — the parser
        // deleted it and recorded it on the program, keyed by the container
        // path's span (see `Program::container_hashers`). Codegen reads the
        // same table off the same span, so the two backends cannot disagree
        // about what a spelling means.
        let crate::ast::TypeKind::Path(p) = &te.kind else {
            return container;
        };
        let kind = self
            .program
            .container_hashers
            .get(&crate::resolver::SpanKey::from_span(&p.span))
            .cloned()
            .unwrap_or_default();
        if kind == HasherKind::default() {
            return container;
        }
        match &container {
            Value::Map(m) => m.write().unwrap().set_hasher(kind),
            Value::Set(t) => t.write().unwrap().set_hasher(kind),
            _ => {}
        }
        container
    }
}
