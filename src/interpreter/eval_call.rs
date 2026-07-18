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

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::{Arc, Mutex, RwLock};

use regex::Regex as RustRegex;

use crate::ast::*;
use crate::token::Span;

use super::exec::ControlFlow;
use super::helpers::{
    base64_decode, base64_encode, decode_err, decode_ok_bytes, decode_ok_string, eval_http_get,
    eval_http_post, eval_stats_fn, eval_stats_fn_int, hex_decode, hex_encode, make_json_error,
    serde_json_to_kara_json, url_decode, url_encode,
};
use super::value::{EnumData, Value};

impl<'a> super::Interpreter<'a> {
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
                return Value::Int(self.active_span_stack.last().copied().unwrap_or(0));
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
                // Numeric narrowing `<int>.try_from(x)` in path form — the
                // `.try_into()` desugar (`x.try_into()` → `T.try_from(x)`)
                // lowers to this shape. Same range check + Result shape as the
                // identifier-form receiver arm in `method_call.rs`.
                if super::method_call::is_numeric_try_from_target(&segments[0]) {
                    let n = match args.first().map(|a| self.eval_expr_inner(&a.value)) {
                        Some(Value::Int(n)) => n,
                        _ => 0,
                    };
                    return super::method_call::numeric_try_from_value(n, &segments[0]);
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

        // Built-in path-qualified functions (e.g. process.exit, Ordering.Relaxed, F64.from)
        if let ExprKind::Path { segments, .. } = &callee.kind {
            let path_str = segments.join(".");
            match path_str.as_str() {
                "process.exit" => {
                    self.track_effect("panics");
                    let code = if let Some(arg) = args.first() {
                        match self.eval_expr_inner(&arg.value) {
                            Value::Int(v) => v as i32,
                            _ => 1,
                        }
                    } else {
                        0
                    };
                    // Run all pending defers via ExitUnwind propagation
                    self.pending_cf = Some(ControlFlow::ExitUnwind { code });
                    return Value::Unit;
                }
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
                    return Value::Map(Vec::new());
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
                    return Value::Set(Vec::new());
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
                "Channel.new" => {
                    let queue: Arc<Mutex<VecDeque<Value>>> = Arc::new(Mutex::new(VecDeque::new()));
                    let sender = Value::Sender(Arc::clone(&queue));
                    let receiver = Value::Receiver(queue);
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
                    return Value::TotalFloat32(val);
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
                    return Value::TotalFloat64(val);
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
                // Phase-11 Column constructors (interpreter MVP) — see
                // runtime/stdlib/column.kara.
                "Column.new"
                | "Column.with_capacity"
                | "Column.from_vec"
                | "Column.from_iter_nullable" => {
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
                    if int_mode {
                        let xs: Vec<i64> = elems
                            .iter()
                            .map(|v| match v {
                                Value::Int(i) => *i,
                                Value::Float(f) => *f as i64,
                                _ => 0,
                            })
                            .collect();
                        return eval_stats_fn_int(&path_str, &xs, p, span);
                    }
                    let xs: Vec<f64> = elems
                        .iter()
                        .map(|v| match v {
                            Value::Float(f) => *f,
                            Value::Int(i) => *i as f64,
                            _ => 0.0,
                        })
                        .collect();
                    return eval_stats_fn(&path_str, &xs, p, span);
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
                    let bytes: Vec<u8> = if let Some(arg) = args.first() {
                        match self.eval_expr_inner(&arg.value) {
                            Value::Array(rc) => rc
                                .read()
                                .unwrap()
                                .iter()
                                .map(|v| match v {
                                    Value::Int(i) => *i as u8,
                                    _ => 0,
                                })
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
                        let is_primitive = matches!(
                            target,
                            "i8" | "i16"
                                | "i32"
                                | "i64"
                                | "u8"
                                | "u16"
                                | "u32"
                                | "u64"
                                | "usize"
                                | "f32"
                                | "f64"
                                | "bool"
                                | "char"
                                | "String"
                        );
                        if is_primitive {
                            if let Some(result) = self.dispatch_lowered_op(method, args, span) {
                                return result;
                            }
                        }
                    }
                }
            }
        }

        // Built-in functions
        if let ExprKind::Identifier(name) = &callee.kind {
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

        // Evaluate arguments
        let arg_vals: Vec<Value> = args
            .iter()
            .map(|a| self.eval_expr_inner(&a.value))
            .collect();

        // Check for enum variant constructor before evaluating callee
        if let ExprKind::Identifier(name) = &callee.kind {
            if self.env.get(name).is_none() {
                if let Some(enum_name) = self.find_enum_for_variant(name) {
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
                for (i, pat) in param_patterns.iter().enumerate() {
                    let val = if let Some(v) = arg_vals.get(i) {
                        v.clone()
                    } else if let Some(Some(default_expr)) = param_defaults.get(i) {
                        self.eval_expr_inner(default_expr)
                    } else {
                        continue;
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
                        let mut snap = HashMap::new();
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

                let result = if contract_fault.is_some() {
                    Ok(Value::Unit)
                } else {
                    self.eval_body_growing(&body)
                };

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

                // CICO write-back: for each call arg whose value is a simple
                // identifier and whose corresponding param is a mutate-through
                // borrow, copy the callee's final binding for that param back
                // to the caller's variable before the scope is popped.
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
                let mut writebacks: Vec<(String, Value)> = Vec::new();
                for (i, arg) in args.iter().enumerate() {
                    let param_is_mut_ref = param_mut_ref
                        .as_ref()
                        .and_then(|flags| flags.get(i))
                        .copied()
                        .unwrap_or(false);
                    if !arg.mut_marker && !param_is_mut_ref {
                        continue;
                    }
                    let caller_var = match &arg.value.kind {
                        ExprKind::Identifier(n) => n.clone(),
                        _ => continue,
                    };
                    if let Some(pat) = param_patterns.get(i) {
                        if let crate::ast::PatternKind::Binding(param_name) = &pat.kind {
                            if let Some(val) = self.env.get(param_name) {
                                writebacks.push((caller_var, val));
                            }
                        }
                    }
                }

                self.env.pop_scope();
                if pushed_subs {
                    self.type_subs_stack.pop();
                }

                for (caller_var, val) in writebacks {
                    self.env.set(&caller_var, val);
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
                self.run_fresh_temp_arg_drops(&fn_name, args, &arg_vals);

                match result {
                    Ok(v) => v,
                    Err(ControlFlow::Return(v)) => v,
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

    /// B-2026-07-01-8 second half — run the user `impl Drop` body for
    /// each FRESH temporary call argument of a Drop-implementing type:
    /// struct literals (`consume(Guard { id: 7 })`), tuple-variant enum
    /// constructors (`consume(Sig.A(1))` — bare or `Enum.Variant`
    /// qualified), and unit variants (`consume(Sig.B)`). Mirrors codegen's
    /// `track_inline_owned_aggregate_arg` shapes exactly (fixed there as
    /// B-2026-07-01-6); bare Identifier args are the caller binding's own
    /// drop. Shared types are excluded (their teardown is refcount-driven).
    fn run_fresh_temp_arg_drops(
        &mut self,
        callee_name: &str,
        args: &[CallArg],
        arg_vals: &[Value],
    ) {
        for (i, arg) in args.iter().enumerate() {
            // B-2026-07-01-7 passthrough guard — mirrored with codegen's
            // `call_arg_flows_into_return`: when the callee can return
            // this parameter, the temp flows out and the RESULT's consumer
            // owns the drop; firing here too double-ran the body
            // (`let x = pass(Guard { id: 7 })` printed twice, probe f6).
            if self.program.items.iter().any(|item| {
                matches!(item, crate::ast::Item::Function(f)
                    if f.name == callee_name && crate::ast::fn_returns_param(f, i))
            }) {
                continue;
            }
            let type_name: Option<String> = match &arg.value.kind {
                ExprKind::StructLiteral { path, .. } => {
                    let n = path.last().cloned();
                    // A SHARED struct literal builds a refcounted value —
                    // its drop belongs to the rc machinery.
                    n.filter(|n| {
                        self.find_struct_def(n)
                            .is_some_and(|d| !d.is_shared && !d.is_par)
                    })
                }
                ExprKind::Call { callee, .. } => match &callee.kind {
                    ExprKind::Identifier(v) => self.find_enum_for_variant(v).or_else(|| {
                        // Fn-call-RETURNED Drop temp (B-2026-07-01-7):
                        // `consume(make())` — resolve the producing fn's
                        // declared return-type head. Shared types are
                        // filtered by the drop_method_keys + struct gate in
                        // the caller below plus the SharedStruct value shape
                        // (run_user_drop_body_on_value binds whatever value
                        // arrived; the drop_method_keys gate is the
                        // authoritative filter).
                        self.user_fn_return_type_name(v)
                    }),
                    ExprKind::Path { segments, .. } if segments.len() == 2 => self
                        .qualified_enum_variant_is_unit(&segments[0], &segments[1])
                        .map(|_| segments[0].clone()),
                    _ => None,
                },
                // Unit variant in path form (`consume(Sig.B)`).
                ExprKind::Path { segments, .. } if segments.len() == 2 => self
                    .qualified_enum_variant_is_unit(&segments[0], &segments[1])
                    .map(|_| segments[0].clone()),
                // Bare unit variant (`consume(B)` where B is a variant).
                ExprKind::Identifier(v) if self.env.get(v).is_none() => {
                    self.find_enum_for_variant(v)
                }
                _ => None,
            };
            let Some(tn) = type_name else { continue };
            if !self.program.drop_method_keys.contains_key(&tn) {
                continue;
            }
            if let Some(v) = arg_vals.get(i) {
                self.run_user_drop_body_on_value(&tn, v.clone());
            }
        }
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
                Some(Value::Bool(rank >= self.tracing_min_level))
            }
            "tracing_set_min_level" => {
                if let Some(Value::Int(r)) = args.first().map(|a| self.eval_expr_inner(&a.value)) {
                    self.tracing_min_level = r;
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

        self.active_span_stack.push(span_id);
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
        match self.env.get(&name) {
            Some(Value::Map(mut m)) => {
                if !m.iter().any(|(k, _)| *k == key) {
                    m.push((key.clone(), default));
                }
                self.env.set(&name, Value::Map(m));
                Value::MapSlotRef {
                    map_var: name,
                    key: Box::new(key),
                }
            }
            // SortedMap sibling: insert-if-absent into the BTreeMap by key, then
            // hand back the same `MapSlotRef` shape (its get/set choke points are
            // taught to resolve a SortedMap slot by key). Mirrors the Map arm.
            Some(Value::SortedMap(mut m)) => {
                m.entry(super::value::OrdValue(key.clone()))
                    .or_insert(default);
                self.env.set(&name, Value::SortedMap(m));
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
        "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64" | "usize" => {
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
