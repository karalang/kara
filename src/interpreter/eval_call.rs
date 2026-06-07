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
use std::sync::{Arc, Mutex};

use regex::Regex as RustRegex;

use crate::ast::*;
use crate::token::Span;

use super::exec::ControlFlow;
use super::helpers::{
    base64_decode, base64_encode, decode_err, decode_ok_bytes, decode_ok_string, eval_http_get,
    eval_http_post, eval_stats_fn, hex_decode, hex_encode, make_json_error,
    serde_json_to_kara_json, url_decode, url_encode,
};
use super::value::{EnumData, Value};

impl<'a> super::Interpreter<'a> {
    pub(crate) fn eval_call(&mut self, callee: &Expr, args: &[CallArg], span: &Span) -> Value {
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
                        if let Value::String(s) = v {
                            return Value::String(s);
                        }
                        return self
                            .record_runtime_error("String.from expects a string argument", span);
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
                | "Stats.median" | "Stats.min" | "Stats.max" => {
                    let xs: Vec<f64> = if let Some(arg) = args.first() {
                        match self.eval_expr_inner(&arg.value) {
                            Value::Array(rc) => rc
                                .read()
                                .unwrap()
                                .iter()
                                .map(|v| match v {
                                    Value::Float(f) => *f,
                                    Value::Int(i) => *i as f64,
                                    _ => 0.0,
                                })
                                .collect(),
                            _ => vec![],
                        }
                    } else {
                        vec![]
                    };
                    return eval_stats_fn(&path_str, &xs, span);
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
                "todo" | "unreachable" => {
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
                    self.eval_block_inner(&body)
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

                // CICO write-back: for each `mut`-marked call arg whose
                // value is a simple identifier, copy the callee's final
                // binding for the corresponding param back to the caller's
                // variable before the scope is popped.
                let mut writebacks: Vec<(String, Value)> = Vec::new();
                for (i, arg) in args.iter().enumerate() {
                    if !arg.mut_marker {
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
    fn invoke_zero_arg_closure(&mut self, callee_val: Value, span: &Span) -> Value {
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
                let result = self.eval_block_inner(&body);
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
    /// `Entry.or_insert_with(f)`. On Vacant, push the new (key, default)
    /// pair onto the live Map (re-fetched by `map_var`) and write back.
    /// On Occupied, return the existing slot value cloned. Either way,
    /// returns the inserted-or-existing value as a Value (NOT a true
    /// `mut ref V`); chained mutation through the return is only fully
    /// supported by the codegen path. Returns `Value::Unit` if the entry
    /// has no `map_var` (chain rooted at a non-identifier receiver) or
    /// the binding doesn't resolve to a Map.
    pub(crate) fn entry_or_insert_value(
        &mut self,
        map_var: Option<String>,
        key: Value,
        slot_idx: Option<usize>,
        default: Value,
    ) -> Value {
        let Some(name) = map_var else {
            return Value::Unit;
        };
        let Some(Value::Map(mut m)) = self.env.get(&name) else {
            return Value::Unit;
        };
        if let Some(idx) = slot_idx {
            if let Some((_, v)) = m.get(idx) {
                return v.clone();
            }
        }
        m.push((key, default.clone()));
        self.env.set(&name, Value::Map(m));
        default
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
        let result = self.eval_block_inner(&body);
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
