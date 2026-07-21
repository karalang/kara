//! Method-call evaluation: the big `eval_method_call` dispatch on
//! receiver shape (Vec/String/Slice/Map/Set/iterator-adapters/etc.).
//!
//! Lives in a sibling `impl<'a> super::Interpreter<'a>` block.

use crate::ast::*;
use crate::token::Span;
use std::sync::{Arc, RwLock};

use super::eval_expr::cast_value;
use super::exec::ControlFlow;
use super::helpers::{kara_json_to_serde_json, value_compare};
use super::pascal_to_snake;
use super::value::{try_write_or_panic, EnumData, Value};

/// Host CPU-feature probe for the interpreter's `cpu.supports(name)` — the
/// tree-walk twin of the runtime `karac_cpu_supports` (`runtime/src/cpu.rs`).
/// Deliberately mirrors that function's recognised-name set per architecture,
/// so `karac run --interp` and `karac build`/JIT report the same features on the
/// machine they run on. An unknown name is `false`.
#[cfg(target_arch = "x86_64")]
fn host_cpu_supports(name: &str) -> bool {
    match name {
        "sse4.2" => std::is_x86_feature_detected!("sse4.2"),
        "avx" => std::is_x86_feature_detected!("avx"),
        "avx2" => std::is_x86_feature_detected!("avx2"),
        "fma" => std::is_x86_feature_detected!("fma"),
        "bmi1" => std::is_x86_feature_detected!("bmi1"),
        "bmi2" => std::is_x86_feature_detected!("bmi2"),
        "avx512f" => std::is_x86_feature_detected!("avx512f"),
        "avx512bw" => std::is_x86_feature_detected!("avx512bw"),
        "avx512vl" => std::is_x86_feature_detected!("avx512vl"),
        "avx512dq" => std::is_x86_feature_detected!("avx512dq"),
        "avx512cd" => std::is_x86_feature_detected!("avx512cd"),
        _ => false,
    }
}

#[cfg(target_arch = "aarch64")]
fn host_cpu_supports(name: &str) -> bool {
    match name {
        "neon" => std::arch::is_aarch64_feature_detected!("neon"),
        "dotprod" => std::arch::is_aarch64_feature_detected!("dotprod"),
        "fp16" => std::arch::is_aarch64_feature_detected!("fp16"),
        "sve" => std::arch::is_aarch64_feature_detected!("sve"),
        "sve2" => std::arch::is_aarch64_feature_detected!("sve2"),
        "i8mm" => std::arch::is_aarch64_feature_detected!("i8mm"),
        "bf16" => std::arch::is_aarch64_feature_detected!("bf16"),
        _ => false,
    }
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
fn host_cpu_supports(_name: &str) -> bool {
    false
}

/// Clone a method receiver for a by-value category dispatcher in
/// `eval_method_call`.
///
/// Most category guards (`try_eval_iterator_method`, `..._map_method`, …)
/// take the receiver **by value** because their signatures consume it. For
/// a large collection receiver (a `Map`/`Set`/`Vec` with N entries) each
/// such clone is O(N), so the count of by-value guards a receiver traverses
/// before its handler accepts is a silent per-op cost multiplier — the
/// exact shape of the B-2026-06-07-4 map-heavy regression, where
/// speculative backpressure guards sitting *above* the map handler each
/// deep-cloned the map (O(n²) kata → 3 extra whole-map clones per op).
///
/// Routing every by-value clone through this one choke point lets the perf
/// gates (`tests::map_receiver_dispatch_clones_are_bounded`,
/// `tests::vec_receiver_dispatch_clones_are_bounded`) count exactly how many
/// times a heavy collection receiver (`Map`/`Vec`) is deep-cloned in a single
/// dispatch and assert it stays O(1), not O(handlers).
///
/// A new category guard added above an existing handler MUST either borrow
/// the receiver (`&obj` — preferred when the `try_eval_*` only reads it, as
/// the iterator/http/regex/set/map/backpressure/process/tensor/pool guards now
/// do) or clone through this helper. A raw `obj.clone()` is invisible to the
/// gate.
#[inline]
fn clone_receiver(obj: &Value) -> Value {
    #[cfg(test)]
    if matches!(obj, Value::Map(_) | Value::Array(_)) {
        test_probe::bump_collection_receiver_clone();
    }
    obj.clone()
}

/// Per-thread counter of by-value heavy-collection-receiver (`Map`/`Vec`)
/// clones performed by `clone_receiver`, used only by the perf-gate unit
/// tests below. Compiled out of production builds (`cfg(test)`), so
/// `clone_receiver` is a plain `obj.clone()` there with zero added cost. Each
/// gate drives one dispatch with a single receiver type, so the shared
/// counter unambiguously attributes the clones to that type.
#[cfg(test)]
pub(crate) mod test_probe {
    use std::cell::Cell;

    thread_local! {
        static COLLECTION_RECEIVER_CLONES: Cell<u32> = const { Cell::new(0) };
    }

    /// Record one by-value clone of a heavy collection receiver.
    pub(super) fn bump_collection_receiver_clone() {
        COLLECTION_RECEIVER_CLONES.with(|c| c.set(c.get() + 1));
    }

    /// Reset the per-thread counter to zero before a measured run.
    pub(crate) fn reset_collection_receiver_clones() {
        COLLECTION_RECEIVER_CLONES.with(|c| c.set(0));
    }

    /// Read the per-thread counter.
    pub(crate) fn collection_receiver_clones() -> u32 {
        COLLECTION_RECEIVER_CLONES.with(|c| c.get())
    }
}

/// `true` when `v` is a builtin heap-allocating collection — the receiver
/// shapes whose `try_*` companions (phase-8-stdlib-floor item 2) the
/// interpreter wraps in `Result.Ok`. `Vec` and `VecDeque` both back onto
/// `Value::Array`.
fn value_is_alloc_collection(v: &Value) -> bool {
    matches!(
        v,
        Value::Array(_)
            | Value::Map(_)
            | Value::Set(_)
            | Value::SortedSet(_)
            | Value::SortedMap(_)
            | Value::String(_)
    )
}

/// Wrap `v` in `Result.Ok(v)` — the success arm every fallible-allocation
/// `try_*` companion returns on the interpreter path (the host allocator never
/// OOMs, so the `Err(AllocError)` arm is unreachable here). Shared with
/// `eval_call`'s static-constructor companion path.
pub(super) fn result_ok(v: Value) -> Value {
    Value::EnumVariant {
        enum_name: "Result".to_string(),
        variant: "Ok".to_string(),
        data: EnumData::Tuple(vec![v]),
    }
}

/// `<int>.try_from(x) -> Result[<int>, String]` — numeric narrowing /
/// sign-changing conversion (design.md § Conversion Traits). Shared by the
/// identifier-form receiver dispatch (`Type.try_from(x)`) and the path-form
/// `.try_into()` desugar (`Call(Path([Type, try_from]))`). In range →
/// `Ok(value)`; otherwise `Err("out of range for T")`. The range check
/// (`numeric_conv::fits_in_target`) and the `Err` message are shared bit-for-bit
/// with codegen so `karac run` and `karac build` stay at parity. `target` must
/// be one of the integer type names; the single arg is evaluated here.
pub(super) fn numeric_try_from_value(n: i64, target: &str) -> Value {
    if crate::numeric_conv::fits_in_target(n as i128, target) {
        result_ok(Value::Int(n))
    } else {
        Value::EnumVariant {
            enum_name: "Result".to_string(),
            variant: "Err".to_string(),
            data: EnumData::Tuple(vec![Value::String(format!("out of range for {}", target))]),
        }
    }
}

/// `true` iff `name` is an integer type that carries a numeric `try_from`.
pub(super) fn is_numeric_try_from_target(name: &str) -> bool {
    matches!(
        name,
        "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64" | "usize"
    )
}

impl<'a> super::Interpreter<'a> {
    /// Run a refinement type's `try_from` at runtime if `type_name` names a
    /// refinement (phase-9 step 5b): evaluate the predicate against the
    /// argument and return `Some(Ok(v))` / `Some(Err(msg))`. Returns `None`
    /// when `type_name` is not a refinement, so callers fall through to
    /// normal associated-function dispatch. The synthetic `try_from` impl
    /// the typechecker registers carries no AST body — this is where the
    /// predicate actually runs on the interpreter path. `Name.try_from(x)`
    /// is the *recoverable* construction surface (`x as Name` is the
    /// asserting form that faults on violation).
    pub(crate) fn eval_refinement_try_from(
        &mut self,
        type_name: &str,
        args: &[CallArg],
    ) -> Option<Value> {
        let pred = self.refinement_predicate(type_name)?;
        let arg_val = match args.first() {
            Some(arg) => self.eval_expr_inner(&arg.value),
            None => Value::Unit,
        };
        if self.check_cf() {
            return Some(Value::Unit);
        }
        let base = self
            .refinement_base_name(type_name)
            .unwrap_or_else(|| type_name.to_string());
        let casted = cast_value(arg_val, &base);
        Some(
            match self.eval_refinement_predicate(&pred, casted.clone()) {
                Some(true) => Value::EnumVariant {
                    enum_name: "Result".to_string(),
                    variant: "Ok".to_string(),
                    data: EnumData::Tuple(vec![casted]),
                },
                _ => Value::EnumVariant {
                    enum_name: "Result".to_string(),
                    variant: "Err".to_string(),
                    data: EnumData::Tuple(vec![Value::String(format!(
                        "value does not satisfy refinement `{type_name}`"
                    ))]),
                },
            },
        )
    }

    /// Extract the lane count `N` from a `Vector[T, N]` generic-arg list
    /// (`N` is the const arg of `[T, N]`), evaluated to a `usize`. Defensive
    /// `0` if absent / non-integer — the typechecker guarantees a valid const
    /// lane count upstream, so that branch is unreachable in checked programs.
    fn vector_lane_count(&mut self, ga: &[GenericArg]) -> usize {
        for arg in ga {
            if let GenericArg::Const(expr) = arg {
                if let Value::Int(n) = self.eval_expr_inner(expr) {
                    return n.max(0) as usize;
                }
            }
        }
        0
    }

    /// Dispatch `method` to an impl-block method registered in the env as
    /// `Type.method` for this receiver's type, executing the body with
    /// method contracts, struct invariants, and the `mut ref self` CICO
    /// write-back. Returns `None` when no impl method is registered (or the
    /// registration is not a function value) — callers fall through to the
    /// builtin arms / the final missing-dispatch error.
    ///
    /// Called twice from `eval_method_call`: early for struct-shaped
    /// receivers, so a user method that shares a builtin container name
    /// (`first`, `last`, `get_unchecked`, …) dispatches to the user's impl
    /// instead of being captured by a builtin arm that swallows receiver
    /// shapes it doesn't handle into `Value::Unit` (B-2026-07-02-10) — and at
    /// the dispatch tail for every other receiver shape, as before.
    fn try_eval_impl_method(
        &mut self,
        object: &Expr,
        method: &str,
        args: &[CallArg],
        span: &Span,
        obj: &Value,
    ) -> Option<Value> {
        let mut type_name = self.value_type_name(obj);
        let mut method_key = format!("{}.{}", type_name, method);
        // Width-erased primitive receiver: `Value::Int` reports "i64" and
        // `Value::Float` reports "f64" regardless of the declared width, so a
        // user `impl Dbl for u8` (registered under "u8.dbl") is NOT reachable
        // via the erased key. Worse, when an `impl Dbl for i64` also exists the
        // erased "i64.dbl" key resolves and would wrongly shadow the narrow
        // receiver's own impl. So for a numeric primitive receiver ALWAYS
        // prefer the DECLARED receiver type the typechecker recorded for this
        // exact call site (`method_callee_types`, e.g. "u8.dbl"). The
        // `.{method}` suffix guard rejects a stale recording from the
        // chained-call span collision (`MethodCall.span == receiver.span`, so
        // in `a.dbl().other()` the outer call clobbers the key) — if it names a
        // different method, fall back to the erased key. B-2026-07-03-5.
        if matches!(obj, Value::Int(_) | Value::Float(_)) {
            let span_key = crate::resolver::SpanKey::from_span(span);
            // Type-param receiver inside a generic body (`x.tag()` where
            // `x: T`): the typechecker records the receiver's type-param NAME
            // in `method_typeparam_receiver` (keyed by the method-call span —
            // `expr_types[receiver.span]` can't be used, it is clobbered by the
            // method's own result type via `MethodCall.span == receiver.span`).
            // Resolve that param name through the runtime type-subs stack
            // (pushed per generic call from `call_type_subs`) to the concrete
            // instantiation. Checked FIRST and preferred whenever it resolves:
            // the width-erased key can otherwise coincidentally hit a
            // same-erased-width impl (`Value::Float` reports "f64", so an `f32`
            // receiver would wrongly dispatch to an existing `f64` impl — and
            // an `i64` impl likewise shadows a narrow int receiver).
            // B-2026-07-03-24 (generic-bound analog of the direct-call
            // recovery below).
            let mut resolved = false;
            if let Some(pname) = self
                .typecheck_result
                .method_typeparam_receiver
                .get(&span_key)
                .cloned()
            {
                if let Some(concrete) = self.resolve_type_param(&pname) {
                    let candidate = format!("{concrete}.{method}");
                    if self.env.get(&candidate).is_some() {
                        type_name = concrete;
                        method_key = candidate;
                        resolved = true;
                    }
                }
            }
            // Direct value-receiver call (concrete receiver): prefer the
            // DECLARED receiver type the typechecker recorded for this exact
            // call site (`method_callee_types`, e.g. "u8.dbl") over the
            // width-erased "i64"/"f64" key. The `.{method}` suffix guard
            // rejects a stale recording from the chained-call span collision
            // (`a.dbl().other()`, where the outer call clobbers the key).
            // B-2026-07-03-5.
            if !resolved {
                if let Some(recorded) = self
                    .typecheck_result
                    .method_callee_types
                    .get(&span_key)
                    .cloned()
                {
                    if recorded.ends_with(&format!(".{method}"))
                        && self.env.get(&recorded).is_some()
                    {
                        if let Some((tn, _)) = recorded.rsplit_once('.') {
                            type_name = tn.to_string();
                        }
                        method_key = recorded;
                    }
                }
            }
        }
        if let Some(func) = self.env.get(&method_key) {
            let mut arg_vals: Vec<Value> = vec![clone_receiver(obj)];
            arg_vals.extend(args.iter().map(|a| self.eval_expr_inner(&a.value)));

            if let Value::Function {
                param_patterns,
                param_defaults,
                body,
                closure_env,
                ..
            } = func
            {
                self.env.push_scope();
                if let Some(ref captured) = closure_env {
                    for (k, v) in captured {
                        self.env.define(k.clone(), v.clone());
                    }
                }
                // `param_patterns` already includes the `self` binding for
                // self-taking methods (prepended at impl-registration time),
                // so a straight in-order bind handles both receiver and args.
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
                // Method `requires` / `ensures` contracts (design.md
                // § Contracts) — same enforcement as free functions, applied
                // on the method-dispatch path. `requires` at entry (self +
                // params in scope), `old(arg)` pre-state captured before the
                // body, `ensures` at the return point with `result` bound.
                let mcontract = self.method_contract(&type_name, method);
                let mut contract_fault: Option<String> = None;
                if let Some((requires, _)) = &mcontract {
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
                let mut pushed_old = false;
                if contract_fault.is_none() {
                    if let Some((_, ensures)) = &mcontract {
                        let mut snap = std::collections::HashMap::new();
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

                if contract_fault.is_none() {
                    if let Some((_, ensures)) = &mcontract {
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

                // Struct-invariant check at method exit (design.md
                // § Contracts rule 3): `impl invariant` fires at every method
                // exit, plain `invariant` at `pub` method exits — both
                // re-checked with `self` bound to the (possibly mutated)
                // receiver value.
                if contract_fault.is_none() {
                    let invariants = self.method_invariants_to_check(&type_name, method);
                    if !invariants.is_empty() {
                        if let Some(self_val) = self.env.get("self") {
                            for inv in &invariants {
                                self.env.push_scope();
                                self.env.define("self".to_string(), self_val.clone());
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

                // CICO write-back for a `mut ref self` receiver. The method
                // ran against a by-value copy of the receiver bound to `self`
                // in this scope; copy that (possibly mutated) value back to the
                // call-site place before the scope is popped, mirroring the
                // free-function `mut ref T` write-back in `eval_call.rs`. Gated
                // strictly on `MutRef` so an owned (consuming) or `ref self`
                // receiver is never written back. The place dispatch matches
                // `StmtKind::Assign` (identifier / field / index), plus
                // `SelfValue` so a nested self-method call (`self.adv()` inside
                // `skip_ws`) propagates the mutation up the receiver chain.
                let self_writeback = if matches!(
                    self.method_self_param(&type_name, method),
                    Some(crate::ast::SelfParam::MutRef)
                ) {
                    self.env.get("self")
                } else {
                    None
                };

                self.env.pop_scope();

                if let Some(self_val) = self_writeback {
                    match &object.kind {
                        ExprKind::Identifier(name) => self.env.set(name, self_val),
                        ExprKind::FieldAccess { object, field } => {
                            self.set_field(object, field, self_val)
                        }
                        ExprKind::Index { object, index } => {
                            self.set_index(object, index, self_val)
                        }
                        ExprKind::SelfValue => self.env.set("self", self_val),
                        _ => {}
                    }
                }

                if let Some(msg) = contract_fault {
                    return Some(self.record_runtime_error(msg, span));
                }
                return Some(match result {
                    Ok(v) => v,
                    Err(ControlFlow::Return(v)) => v,
                    Err(cf) => self.set_cf(cf),
                });
            }
        }
        None
    }

    /// `gpu.dispatch(kernel, buffer)` under `karac run` (spike slice-0c).
    ///
    /// The interpreter has no GPU, so it computes the element-wise map on the
    /// CPU — applying the named `#[gpu]` kernel to each buffer element. That is
    /// exactly what the compiled GPU path computes, so `karac run` and `karac
    /// build` agree on the result (the run == build parity the kata/book A/B
    /// checks rely on). Runs past typecheck errors, so every malformed shape is
    /// a recorded runtime error rather than a panic.
    /// Evaluate `critical_section.acquire()` in the tree-walk interpreter.
    /// Inert: returns a `CriticalSectionGuard` value (restore token 0). The
    /// guard's Drop is a no-op (`try_eval_builtin_drop`), so the single-threaded
    /// interpreter observes no interrupt-mask semantics — mirroring the memory
    /// `fence` intrinsics' inert posture.
    /// `cpu.supports("avx2") -> bool` — the interpreter twin of the codegen CPU
    /// probe (`compile_cpu_supports` → runtime `karac_cpu_supports`). Runs the
    /// same host `is_*_feature_detected!` query via [`host_cpu_supports`], so
    /// `karac run --interp` agrees with `karac build`/JIT on the running machine.
    fn eval_cpu_supports(&mut self, args: &[CallArg], span: &Span) -> Value {
        if args.len() != 1 {
            return self.record_runtime_error(
                format!(
                    "cpu.supports takes 1 argument (a feature name), found {}",
                    args.len()
                ),
                span,
            );
        }
        match self.eval_expr_inner(&args[0].value) {
            Value::String(s) => Value::Bool(host_cpu_supports(&s)),
            _ => self.record_runtime_error(
                "cpu.supports expects a String feature name — e.g. `cpu.supports(\"avx2\")`"
                    .to_string(),
                span,
            ),
        }
    }

    fn eval_critical_section_acquire(&mut self, args: &[CallArg], span: &Span) -> Value {
        if !args.is_empty() {
            return self.record_runtime_error(
                format!(
                    "critical_section.acquire takes no arguments (found {})",
                    args.len()
                ),
                span,
            );
        }
        let mut fields = std::collections::HashMap::new();
        fields.insert("restore_token".to_string(), Value::Int(0));
        Value::Struct {
            name: "CriticalSectionGuard".to_string(),
            fields,
        }
    }

    fn eval_gpu_dispatch(&mut self, args: &[CallArg], span: &Span) -> Value {
        if args.len() < 2 {
            return self.record_runtime_error(
                format!(
                    "gpu.dispatch expects a kernel and a buffer (found {} argument(s))",
                    args.len()
                ),
                span,
            );
        }
        let ExprKind::Identifier(kernel_name) = &args[0].value.kind else {
            return self.record_runtime_error(
                "gpu.dispatch kernel must be a `#[gpu]` function name".to_string(),
                span,
            );
        };
        let kernel_name = kernel_name.clone();

        let Value::Array(rc) = self.eval_expr_inner(&args[1].value) else {
            return self
                .record_runtime_error("gpu.dispatch buffer must be a Vec[f32]".to_string(), span);
        };
        let elems = rc.read().unwrap().clone();

        // Scalar uniforms (GPU-LBM-2): the args beyond kernel + buffer, evaluated
        // once and passed to every per-element kernel call after the element.
        let uniforms: Vec<Value> = args[2..]
            .iter()
            .map(|a| self.eval_expr_inner(&a.value))
            .collect();

        // A stencil kernel (GPU-LBM-6) takes the whole `Vec[S]` buffer plus an
        // index, not an element — its first parameter is a `Vec[...]`. Mirror the
        // GPU thread model: pass the shared read-only buffer and a synthesized
        // per-element index to each call (run == build parity).
        let is_stencil = self.program.items.iter().any(|it| {
            matches!(it, Item::Function(f)
                if f.name == kernel_name
                    && f.is_gpu
                    && f.params.first().map(|p| matches!(&p.ty.kind,
                        TypeKind::Path(pp) if pp.segments.len() == 1 && pp.segments[0] == "Vec"))
                        .unwrap_or(false))
        });

        let mut out = Vec::with_capacity(elems.len());
        if is_stencil {
            let buffer = Value::Array(Arc::new(RwLock::new(elems.clone())));
            for i in 0..elems.len() {
                let mut call_args = Vec::with_capacity(2 + uniforms.len());
                call_args.push(buffer.clone());
                call_args.push(Value::Int(i as i64));
                call_args.extend(uniforms.iter().cloned());
                out.push(self.call_function(&kernel_name, &call_args));
            }
        } else {
            for elem in elems {
                let mut call_args = Vec::with_capacity(1 + uniforms.len());
                call_args.push(elem);
                call_args.extend(uniforms.iter().cloned());
                out.push(self.call_function(&kernel_name, &call_args));
            }
        }
        Value::Array(Arc::new(RwLock::new(out)))
    }

    pub(crate) fn eval_method_call(
        &mut self,
        object: &Expr,
        method: &str,
        args: &[CallArg],
        span: &Span,
        // Closing-paren leaf span of the call. The typechecker stashes the
        // receiver type here for receiver-width-dependent methods whose result
        // type differs from the receiver (`pow`, the bit intrinsics), because
        // `span` aliases the receiver span and `expr_types[span]` has been
        // clobbered with the call's result type by the time the interpreter
        // runs. See `int_width_at` below.
        args_close_span: &Span,
    ) -> Value {
        // Comptime stdlib modules (substrate 3): `ast.expr(s)` and
        // `compiler.error(msg)` parse as method calls on the lowercase module
        // identifier. Intercept before the receiver is evaluated as a value
        // (there is no `ast` / `compiler` binding). The typechecker has
        // already gated these to comptime contexts.
        if let ExprKind::Identifier(module) = &object.kind {
            match (module.as_str(), method) {
                ("ast", "expr") => return self.eval_ast_expr_builder(args, span),
                ("ast", "item") => return self.eval_ast_item_builder(args, span),
                ("compiler", "error") => return self.eval_compiler_error(args, span),
                ("gpu", "dispatch") => return self.eval_gpu_dispatch(args, span),
                // `gpu.upload` / `gpu.download` (resident device buffers) are
                // compiled-only: the tree-walk interpreter has no device-buffer
                // model. A clean diagnostic, not the `variable 'gpu' not found`
                // ICE the fall-through used to hit (B-2026-07-18-5).
                ("gpu", "upload") | ("gpu", "download") => {
                    return self.record_runtime_error(
                        format!(
                            "gpu.{method} requires the compiled path (`karac build`) — resident \
                             GPU buffers have no interpreter model"
                        ),
                        span,
                    )
                }
                // Critical sections (design.md § Critical sections). The
                // tree-walk interpreter is single-threaded with no real
                // interrupts, so acquiring is inert — return the guard value;
                // its Drop is a no-op (`try_eval_builtin_drop`). Same posture
                // the memory `fence` intrinsics take under the interpreter.
                // Guarded on `critical_section` not being a user binding so a
                // local of that name still dispatches its own methods.
                ("critical_section", "acquire") if self.env.get("critical_section").is_none() => {
                    return self.eval_critical_section_acquire(args, span)
                }
                // `cpu.supports("avx2") -> bool` — runtime CPU-feature probe
                // (the `#[multiversion]` dispatch primitive). The interpreter runs
                // the SAME host `is_*_feature_detected!` query as codegen's runtime
                // call, so `karac run --interp` agrees with `karac build` / the JIT
                // on the machine the program runs on. Guarded so a local `cpu`
                // binding still dispatches its own methods.
                ("cpu", "supports") if self.env.get("cpu").is_none() => {
                    return self.eval_cpu_supports(args, span)
                }
                _ => {}
            }
            // Raw-pointer / MMIO intrinsics (`ptr.const`/`ptr.mut`/`ptr.addr`/
            // …) are codegen-only — the tree-walk interpreter has no model for
            // raw pointers. Emit a clean, honest diagnostic instead of falling
            // through to evaluate the `ptr` receiver as a value, which panicked
            // with an `unreachable!("variable 'ptr' not found")` internal error
            // (B-2026-07-12-7). Guarded on `ptr` not being a user binding so a
            // local genuinely named `ptr` still dispatches its own methods.
            if module == "ptr" && self.env.get("ptr").is_none() {
                return self.record_runtime_error(
                    format!(
                        "raw-pointer intrinsic `ptr.{method}(..)` is only supported under \
                         `karac build` / the JIT (codegen), not the tree-walk interpreter — \
                         it has no raw-pointer model. Run without `--interp` (unset \
                         KARAC_RUN_JIT) to use the compiled backend."
                    ),
                    span,
                );
            }
        }

        // SIMD static constructor — `Vector[T, N].splat(x)`. The receiver is
        // the bare vector type-path (not a value), so intercept before the
        // generic eval below treats `Vector[T, N]` as a value. Broadcast the
        // scalar to all `N` lanes (`N` is the second generic arg).
        if method == "splat" {
            if let ExprKind::Path {
                segments,
                generic_args: Some(ga),
            } = &object.kind
            {
                if segments.len() == 1 && segments[0] == "Vector" {
                    let scalar = self.eval_expr_inner(&args[0].value);
                    let n = self.vector_lane_count(ga);
                    return Value::Vector(vec![scalar; n]);
                }
            }
        }

        // SIMD static constructor — `Vector[T, N].from_array([..])`. Same
        // type-path-receiver intercept as `splat`. The argument evaluates to a
        // `Value::Array`; its `N` elements become the vector lanes directly
        // (the typechecker guarantees the element count matches `N`).
        if method == "from_array" {
            if let ExprKind::Path {
                segments,
                generic_args: Some(_),
            } = &object.kind
            {
                if segments.len() == 1 && segments[0] == "Vector" {
                    if let Value::Array(rc) = self.eval_expr_inner(&args[0].value) {
                        let elems = rc.read().unwrap().clone();
                        return Value::Vector(elems);
                    }
                }
            }
        }

        // SIMD static constructor — `Vector[T, N].from_slice(s)`. Same
        // type-path-receiver intercept. The argument evaluates to a
        // `Value::Slice` window; its length is a runtime property, so unlike
        // `from_array` we must check it equals `N` and panic on mismatch.
        if method == "from_slice" {
            if let ExprKind::Path {
                segments,
                generic_args: Some(ga),
            } = &object.kind
            {
                if segments.len() == 1 && segments[0] == "Vector" {
                    let n = self.vector_lane_count(ga);
                    if let Value::Slice {
                        storage,
                        start,
                        len,
                        ..
                    } = self.eval_expr_inner(&args[0].value)
                    {
                        if len != n {
                            return self.record_runtime_error(
                                format!(
                                    "from_slice: slice length {len} does not match \
                                     Vector lane count {n}"
                                ),
                                span,
                            );
                        }
                        let guard = storage.read().unwrap();
                        let elems = guard[start..start + len].to_vec();
                        return Value::Vector(elems);
                    }
                }
            }
        }

        // SIMD static constructor — `Vector[T, N].load_masked(slice, mask)`.
        // Same type-path-receiver intercept. Loads only the lanes the mask
        // selects: lane `i` is active iff `mask[i]`; an active lane past the
        // slice length panics (parity with codegen's `emit_panic`), and an
        // inactive lane reads a typed zero without touching the slice.
        if method == "load_masked" {
            if let ExprKind::Path {
                segments,
                generic_args: Some(ga),
            } = &object.kind
            {
                if segments.len() == 1 && segments[0] == "Vector" {
                    let n = self.vector_lane_count(ga);
                    let elem_is_float = ga.iter().any(|a| {
                        matches!(a, GenericArg::Type(t)
                        if matches!(&t.kind, crate::ast::TypeKind::Path(p)
                            if matches!(
                                p.segments.last().map(|s| s.as_str()),
                                Some("f32") | Some("f64") | Some("float")
                            )))
                    });
                    let zero = if elem_is_float {
                        Value::Float(0.0)
                    } else {
                        Value::Int(0)
                    };
                    let slice_v = self.eval_expr_inner(&args[0].value);
                    let mask_v = self.eval_expr_inner(&args[1].value);
                    let (storage, start, slen) = match slice_v {
                        Value::Slice {
                            storage,
                            start,
                            len,
                            ..
                        } => (storage, start, len),
                        other => {
                            return self.record_runtime_error(
                                format!(
                                    "load_masked expects a Slice argument, got `{}`",
                                    other.variant_name()
                                ),
                                span,
                            )
                        }
                    };
                    let Value::Vector(mask) = mask_v else {
                        return self.record_runtime_error(
                            "load_masked expects a Vector[bool, N] mask".to_string(),
                            span,
                        );
                    };
                    let guard = storage.read().unwrap();
                    let mut out = Vec::with_capacity(n);
                    for i in 0..n {
                        let active = matches!(mask.get(i), Some(Value::Bool(true)));
                        if active {
                            if i >= slen {
                                return self.record_runtime_error(
                                    "load_masked: active lane index out of bounds".to_string(),
                                    span,
                                );
                            }
                            out.push(guard[start + i].clone());
                        } else {
                            out.push(zero.clone());
                        }
                    }
                    return Value::Vector(out);
                }
            }
        }

        // SIMD static constructor — `Vector[T, N].gather(slice, indices)`.
        // Same type-path-receiver intercept. Reads `slice[indices[i]]` for
        // each lane; every index is bounds-checked (`0 <= idx < len`, panic
        // otherwise) like the `slice[i]` read.
        if method == "gather" {
            if let ExprKind::Path {
                segments,
                generic_args: Some(_),
            } = &object.kind
            {
                if segments.len() == 1 && segments[0] == "Vector" {
                    let slice_v = self.eval_expr_inner(&args[0].value);
                    let indices_v = self.eval_expr_inner(&args[1].value);
                    let (storage, start, slen) = match slice_v {
                        Value::Slice {
                            storage,
                            start,
                            len,
                            ..
                        } => (storage, start, len),
                        Value::Array(rc) => {
                            let len = rc.read().unwrap().len();
                            (rc, 0, len)
                        }
                        other => {
                            return self.record_runtime_error(
                                format!(
                                    "gather expects a Slice argument, got `{}`",
                                    other.variant_name()
                                ),
                                span,
                            )
                        }
                    };
                    let Value::Vector(indices) = indices_v else {
                        return self.record_runtime_error(
                            "gather expects an integer index vector".to_string(),
                            span,
                        );
                    };
                    let guard = storage.read().unwrap();
                    let mut out = Vec::with_capacity(indices.len());
                    for idx_v in &indices {
                        let Value::Int(idx) = idx_v else {
                            return self.record_runtime_error(
                                "gather index lane must be an integer".to_string(),
                                span,
                            );
                        };
                        if *idx < 0 || (*idx as usize) >= slen {
                            return self.record_runtime_error(
                                "gather: index out of bounds".to_string(),
                                span,
                            );
                        }
                        out.push(guard[start + *idx as usize].clone());
                    }
                    return Value::Vector(out);
                }
            }
        }

        // SIMD static constructor — `Vector[U, N].cast_from(v)`. Per-lane
        // numeric conversion of the source vector's lanes to the target
        // element `U`. The interpreter models every int as `Value::Int(i64)`
        // and every float as `Value::Float(f64)`, so only the int↔float
        // direction changes a lane's carrier here: int→int and float→float
        // are identity (a narrower-int / f32 target's truncation/rounding is a
        // codegen-time concern, consistent with the interpreter's existing
        // width-agnostic numeric model).
        if method == "cast_from" {
            if let ExprKind::Path {
                segments,
                generic_args: Some(ga),
            } = &object.kind
            {
                if segments.len() == 1 && segments[0] == "Vector" {
                    let target_is_float = ga.iter().any(|a| {
                        matches!(a, GenericArg::Type(t)
                        if matches!(&t.kind, crate::ast::TypeKind::Path(p)
                            if matches!(
                                p.segments.last().map(|s| s.as_str()),
                                Some("f32") | Some("f64") | Some("float")
                            )))
                    });
                    let Value::Vector(src) = self.eval_expr_inner(&args[0].value) else {
                        return self.record_runtime_error(
                            "cast_from expects a source vector".to_string(),
                            span,
                        );
                    };
                    let out: Vec<Value> = src
                        .into_iter()
                        .map(|lane| {
                            if target_is_float {
                                match lane {
                                    Value::Int(i) => Value::Float(i as f64),
                                    other => other,
                                }
                            } else {
                                match lane {
                                    Value::Float(f) => Value::Int(f as i64),
                                    other => other,
                                }
                            }
                        })
                        .collect();
                    return Value::Vector(out);
                }
            }
        }

        // Type-receiver associated calls: `T.method(...)` where `T` is a
        // primitive type name. The receiver is an identifier naming a type
        // — not a value — so eval_expr_inner would panic. Handle two shapes:
        //   (a) `.from(x)` — numeric widening (identity at interpreter layer)
        //   (b) operator methods (add/sub/lt/eq/bitand/not/…) — delegate to
        //       the same dispatch used for the lowered `Call(Path)` form.
        if let ExprKind::Identifier(type_name) = &object.kind {
            let target = type_name.as_str();
            // `Name.try_from(x)` on a refinement type runs the predicate at
            // runtime (phase-9 step 5b). It usually parses as a path call
            // (`Call(Path([Name, try_from]))`, handled in `eval_call`); this
            // covers the method-on-type-identifier shape defensively.
            if method == "try_from" {
                if let Some(v) = self.eval_refinement_try_from(target, args) {
                    return v;
                }
            }
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
                if method == "from" {
                    if let Some(arg) = args.first() {
                        return self.eval_expr_inner(&arg.value);
                    }
                }
                // `<int_type>.parse(s: String) -> Option[T]`. Base-10
                // parse via Rust's `str::parse::<i64>()`. Currently all
                // ints lower to `i64` at the Value layer, so every
                // primitive-int type's `parse` produces `Value::Int`;
                // narrower-typed `parse` (`i8.parse`, `u32.parse`,
                // etc.) is a future codegen-time tweak.
                if method == "parse"
                    && matches!(
                        target,
                        "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64" | "usize"
                    )
                {
                    if let Some(arg) = args.first() {
                        let s_val = self.eval_expr_inner(&arg.value);
                        if let Value::String(s) = s_val {
                            return match s.trim().parse::<i64>() {
                                Ok(n) => Value::EnumVariant {
                                    enum_name: "Option".to_string(),
                                    variant: "Some".to_string(),
                                    data: EnumData::Tuple(vec![Value::Int(n)]),
                                },
                                Err(_) => Value::EnumVariant {
                                    enum_name: "Option".to_string(),
                                    variant: "None".to_string(),
                                    data: EnumData::Unit,
                                },
                            };
                        }
                    }
                    return Value::EnumVariant {
                        enum_name: "Option".to_string(),
                        variant: "None".to_string(),
                        data: EnumData::Unit,
                    };
                }
                // `f64.parse(s: String) -> Option[f64]`. Float parse via Rust's
                // `str::parse`. The self-hosting lexer's float-literal path.
                // (f32.parse is deferred — its narrower Option payload width
                // needs its own runtime path; the lexer parses every float as
                // f64 then attaches the suffix.)
                if method == "parse" && target == "f64" {
                    let make_none = || Value::EnumVariant {
                        enum_name: "Option".to_string(),
                        variant: "None".to_string(),
                        data: EnumData::Unit,
                    };
                    if let Some(arg) = args.first() {
                        let s_val = self.eval_expr_inner(&arg.value);
                        if let Value::String(s) = s_val {
                            return match s.trim().parse::<f64>() {
                                Ok(v) => Value::EnumVariant {
                                    enum_name: "Option".to_string(),
                                    variant: "Some".to_string(),
                                    data: EnumData::Tuple(vec![Value::Float(v)]),
                                },
                                Err(_) => make_none(),
                            };
                        }
                    }
                    return make_none();
                }
                // `<int_type>.from_str_radix(s: String, radix: u32) ->
                // Option[i64]`. Radix 2..=36 via Rust's `i64::from_str_radix`.
                // The self-hosting lexer's hex/binary/octal literal path.
                if method == "from_str_radix"
                    && matches!(
                        target,
                        "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64" | "usize"
                    )
                {
                    let make_none = || Value::EnumVariant {
                        enum_name: "Option".to_string(),
                        variant: "None".to_string(),
                        data: EnumData::Unit,
                    };
                    if args.len() >= 2 {
                        let s_val = self.eval_expr_inner(&args[0].value);
                        let radix_val = self.eval_expr_inner(&args[1].value);
                        if let (Value::String(s), Value::Int(radix)) = (s_val, radix_val) {
                            if (2..=36).contains(&radix) {
                                if let Ok(n) = i64::from_str_radix(s.trim(), radix as u32) {
                                    return Value::EnumVariant {
                                        enum_name: "Option".to_string(),
                                        variant: "Some".to_string(),
                                        data: EnumData::Tuple(vec![Value::Int(n)]),
                                    };
                                }
                            }
                        }
                    }
                    return make_none();
                }
                // `char.try_from(n: <int>) -> Result[char, i64]` (#10). Mirrors
                // the codegen handler: valid Unicode scalar (`0..=0x10FFFF`,
                // excluding the `0xD800..=0xDFFF` surrogate range) → `Ok(char)`;
                // otherwise `Err(cp)` carrying the offending codepoint.
                if method == "try_from" && target == "char" {
                    let mut cp_opt: Option<i64> = None;
                    if let Some(arg) = args.first() {
                        if let Value::Int(cp) = self.eval_expr_inner(&arg.value) {
                            cp_opt = Some(cp);
                        }
                    }
                    let cp = cp_opt.unwrap_or(0);
                    let ch = if (0..=0x10FFFF).contains(&cp) && !(0xD800..=0xDFFF).contains(&cp) {
                        char::from_u32(cp as u32)
                    } else {
                        None
                    };
                    return match ch {
                        Some(c) => Value::EnumVariant {
                            enum_name: "Result".to_string(),
                            variant: "Ok".to_string(),
                            data: EnumData::Tuple(vec![Value::Char(c)]),
                        },
                        None => Value::EnumVariant {
                            enum_name: "Result".to_string(),
                            variant: "Err".to_string(),
                            data: EnumData::Tuple(vec![Value::Int(cp)]),
                        },
                    };
                }
                // `<int>.try_from(x: <int>) -> Result[<int>, String]` — numeric
                // narrowing / sign-changing conversion (design.md § Conversion
                // Traits). In range → `Ok(value)`; otherwise `Err("out of range
                // for T")`. The range check is `numeric_conv::fits_in_target`,
                // shared bit-for-bit with codegen. Caveat: `Value::Int` is i64,
                // so a `u64` source above `i64::MAX` is already stored as a
                // negative i64 (the pre-existing interpreter wide-int limit) and
                // would be misjudged — the same limitation the int `parse` arms
                // carry; codegen is exact.
                if method == "try_from" && is_numeric_try_from_target(target) {
                    let n = match args.first().map(|a| self.eval_expr_inner(&a.value)) {
                        Some(Value::Int(n)) => n,
                        _ => 0,
                    };
                    return numeric_try_from_value(n, target);
                }
                if let Some(result) = self.dispatch_lowered_op(method, args, span) {
                    return result;
                }
            }

            // Lowercase stdlib module aliases: `env.args()`, `clock.now()`,
            // `stdout.println(s)`, `fs.write(p, c)`, … Map to the capitalized
            // effect resource name so the provider stack lookup in
            // `eval_resource_method` finds the right binding. Mirrors the
            // resolver alias `push`, the typechecker alias map, and codegen's
            // `ambient_resource_for_alias`. A local binding of the same name
            // shadows the module (`let clock = Timer { ... }; clock.now()`),
            // so skip the alias when `type_name` names a bound variable — the
            // same `!variables.contains_key` guard codegen and the typechecker
            // (`local_scope.lookup`) apply.
            let resource_alias = if self.env.get(type_name).is_some() {
                None
            } else {
                match type_name.as_str() {
                    "env" => Some("Env"),
                    "clock" => Some("Clock"),
                    "rand" => Some("RandomSource"),
                    "stdin" => Some("Stdin"),
                    "stdout" => Some("Stdout"),
                    "stderr" => Some("Stderr"),
                    "fs" => Some("FileSystem"),
                    _ => None,
                }
            };
            if let Some(resource) = resource_alias {
                return self.eval_resource_method(resource, method, args, span);
            }

            // Effect-resource receiver: `UserDB.query(...)` resolves through
            // the top-of-stack provider binding for `UserDB` (design.md §
            // Provider-Rooted Resources > Runtime mechanics). `UserDB` is
            // not a value — it's a tracked identity — so we skip
            // `eval_expr_inner(object)` on this path and dispatch directly
            // on the provider instance stored in `provider_stack`.
            if self.effect_resources.contains(type_name) {
                return self.eval_resource_method(type_name, method, args, span);
            }
        }

        let obj = self.eval_expr_inner(object);

        // A `mut ref V` returned by `Map.entry(k).or_insert(d)` is a place-ref
        // into the live Map slot. Method calls (`.push(x)`, …) dispatch on the
        // underlying value, so resolve the ref here. For an Arc-backed element
        // (e.g. `Vec`), the resolved clone shares storage with the slot, so an
        // in-place mutation writes through to the map. (An identifier receiver
        // bound to a `MapSlotRef` is already resolved by `Env::get`; only the
        // bare `…or_insert(d).method()` chain reaches here as a raw ref.)
        let obj = if let Value::MapSlotRef { map_var, key } = &obj {
            self.env.read_map_slot(map_var, key)
        } else {
            obj
        };

        // Comptime `Type` reflection (substrate 2): `MyType.name()`,
        // `.fields()`, `.variants()`, `.is_struct()`, … on a `Type`
        // pseudovalue. Dispatches against the typecheck result's
        // struct/enum/union tables. Only reachable at comptime — the
        // typechecker rejects a `Type` value at runtime.
        if let Value::TypeVal(type_name) = &obj {
            return self.eval_type_reflection(&type_name.clone(), method, args, span);
        }

        // Fallible-allocation companions (phase-8-stdlib-floor item 2). A
        // `try_<base>` instance method on a builtin collection runs the
        // panicking `<base>` operation and wraps its result in `Result.Ok(_)`:
        // the tree-walk host allocator never actually OOMs, so the companion
        // always succeeds (failure injection arrives with the codegen runtime
        // allocator wrappers, item 8). The base op recurses through
        // `eval_method_call`; a builtin collection's backing store is shared
        // (`Arc<RwLock<…>>` / re-read place), so re-evaluating an
        // identifier/place receiver in the recursion mutates the same store.
        // Gated on a builtin-collection receiver value so a user type's own
        // `try_push` / `try_clone` / … is never shadowed.
        if value_is_alloc_collection(&obj) {
            if let Some(base) = crate::fallible_alloc::instance_companion_base(method) {
                let base_val = self.eval_method_call(object, base, args, span, args_close_span);
                return result_ok(base_val);
            }
        }

        // Distinct-type `.raw()` unwrap (design.md § Distinct Types). A
        // distinct type is zero-cost — its runtime value already *is* the
        // base value — so `.raw()` returns the receiver unchanged. `.raw()`
        // is reserved to distinct types by the typechecker (the only
        // built-in method they carry), so a zero-arg `.raw()` reaching the
        // interpreter is always this unwrap.
        if method == "raw" && args.is_empty() {
            return obj;
        }

        // Slice 3 — mut-Slice mutation methods that route their writes
        // back to the original storage. These dispatch BEFORE the
        // Slice→Array normalization below; the normalization is for
        // read-only methods that can safely operate on a fresh snapshot.
        if let Value::Slice {
            storage,
            start,
            len,
            ..
        } = &obj
        {
            if method == "swap" {
                let i_val = args
                    .first()
                    .map(|a| self.eval_expr_inner(&a.value))
                    .unwrap_or(Value::Int(0));
                let j_val = args
                    .get(1)
                    .map(|a| self.eval_expr_inner(&a.value))
                    .unwrap_or(Value::Int(0));
                if let (Value::Int(i_v), Value::Int(j_v)) = (i_val, j_val) {
                    let label = match &object.kind {
                        ExprKind::Identifier(n) => n.clone(),
                        _ => "<value>".to_string(),
                    };
                    let mut guard = try_write_or_panic(storage, &label);
                    let i = i_v as usize;
                    let j = j_v as usize;
                    if i < *len && j < *len {
                        guard.swap(start + i, start + j);
                    }
                }
                return Value::Unit;
            }
        }

        // Slice 3 — methods on `Slice[T]` / `mut Slice[T]` dispatch via
        // the existing Array-method surface. The interpreter snapshots
        // the slice's window into a fresh `Value::Array` so each
        // read-only method (`first` / `last` / `get` / `contains` /
        // `chunks` / `windows` / `len` / `is_empty` / `iter` / etc.)
        // sees a uniform shape. The slice itself is preserved by the
        // `.as_slice` / `.as_slice_mut` MethodCall arm above (which
        // detects the Slice receiver and rebuilds the view) and by the
        // Index expression path for read/write through `[i]`. Mutation
        // methods that need source-aliasing semantics (`swap`) dispatch
        // above this fence.
        let obj = match obj {
            Value::Slice {
                storage,
                start,
                len,
                ..
            } if !matches!(method, "as_slice" | "as_slice_mut") => {
                let snap = storage.read().unwrap()[start..start + len].to_vec();
                Value::array_of(snap)
            }
            other => other,
        };

        // Structured-concurrency dispatch (design.md § Structured
        // Concurrency / TaskGroup). The tree-walk interpreter runs spawned
        // children eagerly (see `eval_spawn_closure`), so these are the
        // receiver-typed entry points that route the `TaskGroup` /
        // `TaskHandle` surface declared in `runtime/stdlib/task_group.kara`.
        // Gated on the concrete receiver value, so they never shadow a same-
        // named method on another type (`Command.spawn`, `String.join`, …).
        match &obj {
            // `tg.spawn(closure)` — run the child now, return its
            // `TaskHandle`. `mut ref self`, but the group is a stateless
            // marker, so there is nothing to write back to `tg`.
            Value::TaskGroup if method == "spawn" => {
                let Some(arg0) = args.first() else {
                    return Value::TaskHandle(Box::new(Value::Unit));
                };
                return self.eval_spawn_closure(arg0);
            }
            // `tg.cancel()` — cooperative cancellation. In the eager model
            // every child has already run to completion by the time control
            // returns to the spawner, so there is nothing left to cancel.
            Value::TaskGroup if method == "cancel" => {
                return Value::Unit;
            }
            // `handle.join()` — deliver the child's already-computed result.
            // `.join()` consumes `self` (typechecker-enforced), so a single
            // read of the boxed value is sound.
            Value::TaskHandle(result) if method == "join" => {
                return (**result).clone();
            }
            _ => {}
        }

        // Slice F (`std.json`): `j.stringify()` on a `Json`-typed
        // receiver. Walks the enum tree to a `serde_json::Value` and
        // calls `serde_json::to_string`. Locked design (ii)'s insertion-
        // order property is preserved because the receiver's `Object`
        // payload is a `Vec[(String, Json)]` and the runtime crate's
        // `serde_json` is built with `preserve_order`, so the
        // intermediate `serde_json::Map` round-trips key ordering.
        if method == "stringify" {
            if let Value::EnumVariant { ref enum_name, .. } = obj {
                if enum_name == "Json" {
                    let v = kara_json_to_serde_json(&obj);
                    let s = serde_json::to_string(&v).unwrap_or_else(|_| "null".to_string());
                    return Value::String(s);
                }
            }
        }

        // `String.to_cstring(ref self) -> Result[CString, NulError]` (design.md
        // § C-String Literals). The outbound conversion: copy the receiver's
        // UTF-8 bytes into an owning `CString` (a trailing NUL is a compiled-
        // mode buffer detail — `Value::CString` carries the NUL-excluded bytes,
        // like `Value::CStr`), unless the receiver holds an interior NUL byte,
        // which C would truncate at → `Err(NulError.InteriorNul)`. Mirrors the
        // codegen `karac_runtime_string_to_cstring` reject rule.
        if method == "to_cstring" {
            if let Value::String(ref s) = obj {
                let bytes = s.as_bytes();
                return if bytes.contains(&0) {
                    Value::EnumVariant {
                        enum_name: "Result".to_string(),
                        variant: "Err".to_string(),
                        data: EnumData::Tuple(vec![Value::EnumVariant {
                            enum_name: "NulError".to_string(),
                            variant: "InteriorNul".to_string(),
                            data: EnumData::Unit,
                        }]),
                    }
                } else {
                    Value::EnumVariant {
                        enum_name: "Result".to_string(),
                        variant: "Ok".to_string(),
                        data: EnumData::Tuple(vec![Value::CString(std::sync::Arc::new(
                            bytes.to_vec(),
                        ))]),
                    }
                };
            }
        }

        // `CStr.to_string() -> Result[String, Utf8Error]` and its zero-copy
        // sibling `CStr.to_string_slice() -> Result[StringSlice, Utf8Error]` —
        // both UTF-8-validating, and MUST precede the generic Display
        // `to_string` below (which returns a bare `String` and would mismatch
        // the `Result` type the typechecker and codegen produce for a CStr
        // receiver). The interpreter is dynamically typed and has no separate
        // `StringSlice` value — a borrowed view is just a `Value::String`, so
        // both methods produce the same observable result (content + Ok/Err);
        // codegen is where the borrow-vs-copy distinction is real. Same oracle
        // as `String.from_utf8` (eval_call.rs): `error_len()` distinguishes a
        // truncated trailing sequence (`IncompleteSequence`) from a bad byte at
        // a known offset (`InvalidByte`).
        if method == "to_string" || method == "to_string_slice" {
            if let Value::CStr(ref b) = obj {
                return match std::str::from_utf8(b) {
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
        }

        // `to_string()` dispatch order: a user `impl Display` (a registered
        // `<Type>.to_string` method) wins — fall through to the generic
        // impl-method dispatch below, which invokes the user body (with its
        // contracts). Only when NO user impl exists do we render via the
        // built-in `#[derive(Display)]` / `display_render` path. This is what
        // makes user `impl Display for MyEnum { fn to_string(...) }` actually
        // take effect for `x.to_string()` and (via the unified dispatch) for
        // `f"{x}"` / `println(x)`. See examples/weave GAP-W4.
        if method == "to_string" && self.user_display_impl_to_string_key(&obj).is_none() {
            // `#[derive(Display)]` — `to_string()` on a unit enum variant.
            if let Value::EnumVariant {
                enum_name,
                variant,
                data: EnumData::Unit,
            } = &obj
            {
                let has_display = self
                    .typecheck_result
                    .enum_info
                    .get(enum_name.as_str())
                    .map(|info| info.derived_traits.contains("Display"))
                    .unwrap_or(false);
                if has_display {
                    let s = if self
                        .typecheck_result
                        .display_snake_case_enums
                        .contains(enum_name.as_str())
                    {
                        pascal_to_snake(variant)
                    } else {
                        variant.clone()
                    };
                    return Value::String(s);
                }
            }
            // Unsigned-64 scalar (B-2026-07-04-8): the i64-carrier `Value::Int`
            // holds a `u64` / `usize` value ≥ 2⁶³ as a negative two's-complement
            // i64, which the signed `Display` would print with a spurious minus
            // sign. Recover the receiver's static type from its span and render
            // the bits as `u64` so `f"{hi}"` / `println(hi)` / `hi.to_string()`
            // match codegen's unsigned print. Only the bare scalar is reachable
            // this way — a whole `Vec[u64]` printed via `f"{xs}"` recurses into
            // elements as span-less `Value::Int`, which stay signed (documented
            // residual; the i64-carrier model can't recover per-element types).
            if let Value::Int(n) = &obj {
                if self.span_type_is_unsigned64(&object.span) {
                    return Value::String(format!("{}", *n as u64));
                }
            }
            // All other Display-able values: render via the user-facing
            // renderer (declaration-order struct fields, recursing into
            // containers) so `.to_string()` matches `println` and codegen.
            return Value::String(self.display_render(&obj));
        }

        // Category dispatchers — each returns `Some(Value)` if `method`
        // matches one of its handled names and the receiver shape is
        // compatible; otherwise `None` and we fall through to the next.
        // Column dispatch precedes the iterator machinery: `iter` /
        // `iter_valid` are Column method names that would otherwise be
        // claimed by `try_eval_iterator_method` (which `unreachable!`s on a
        // non-iterable `Value::Column` receiver). A non-Column receiver
        // returns `None` here and falls through unchanged.
        if let Some(v) = self.try_eval_column_method(method, &obj, args, span, args_close_span) {
            return v;
        }
        // DataFrame methods (`insert` / `column` / `column_names` / …) —
        // a non-DataFrame receiver returns `None` and falls through.
        if let Some(v) = self.try_eval_dataframe_method(method, &obj, args, span) {
            return v;
        }
        // LazyFrame plan builders + collect/explain (phase-11
        // LazyDataFrame slice 1) — a non-LazyFrame receiver falls through.
        if let Some(v) = self.try_eval_lazyframe_method(method, &obj, args, span) {
            return v;
        }
        if let Some(v) = self.try_eval_iterator_method(method, object, &obj, args, span) {
            return v;
        }
        if let Some(v) = self.try_eval_http_method(method, &obj, args, span) {
            return v;
        }
        if let Some(v) = self.try_eval_regex_method(method, &obj, args, span) {
            return v;
        }
        if let Some(v) = self.try_eval_process_method(method, &obj, args, span) {
            return v;
        }
        if let Some(v) = self.try_eval_tensor_method(method, &obj, args, span, args_close_span) {
            return v;
        }
        if let Some(v) = self.try_eval_pool_method(method, &obj, args, span) {
            return v;
        }
        if let Some(v) = self.try_eval_arena_method(method, &obj, args, span) {
            return v;
        }
        if let Some(v) = self.try_eval_interner_method(method, &obj, args, span) {
            return v;
        }
        if let Some(v) = self.try_eval_once_method(method, &obj, args, span) {
            return v;
        }
        if let Some(v) = self.try_eval_secret_method(method, &obj, args, span) {
            return v;
        }
        // Backpressure guards borrow the receiver (`&obj`) instead of
        // cloning it: each only reads the `{name, handle_id}` struct via
        // `*_handle(&obj)`, so a speculative clone here was pure waste —
        // and for a large receiver (e.g. a `Map` whose method is `get`/
        // `insert`) each clone is O(n), so the three guards multiplied a
        // map-heavy O(n²) workload's cost (B-2026-06-07-4). Mirrors the
        // `try_eval_tensor_method(&obj, ...)` precedent above.
        if let Some(v) = self.try_eval_semaphore_method(method, &obj, args, span) {
            return v;
        }
        if let Some(v) = self.try_eval_rate_limiter_method(method, &obj, args, span) {
            return v;
        }
        if let Some(v) = self.try_eval_bounded_channel_method(method, &obj, args, span) {
            return v;
        }
        if let Some(v) = self.try_eval_set_method(method, object, &obj, args, span) {
            return v;
        }
        // The map handler owns Map/Set/SortedSet/Entry receivers and consumes
        // an owned value (insert/merge move out of it). Borrow-check the
        // receiver shape BEFORE cloning so a non-matching receiver (a `Vec`/
        // `String` on the dispatch hot path) passes through uncloned — the
        // single legitimate clone (`clone_receiver`, counted by the perf gate)
        // only happens for a receiver this handler actually accepts
        // (B-2026-06-07-4a). The post-map guards below borrow `&obj`.
        if matches!(
            obj,
            Value::Map(_)
                | Value::SortedSet(_)
                | Value::SortedMap(_)
                | Value::Set(_)
                | Value::Entry { .. }
        ) {
            if let Some(v) =
                self.try_eval_map_method(method, object, clone_receiver(&obj), args, span)
            {
                return v;
            }
        }
        if let Some(v) = self.try_eval_option_result_method(method, object, &obj, args, span) {
            return v;
        }
        if let Some(v) = self.try_eval_channel_method(method, &obj, args, span) {
            return v;
        }
        if let Some(v) = self.try_eval_file_method(method, &obj, args, span) {
            return v;
        }
        if let Some(v) = self.try_eval_bufreader_method(method, &obj, args, span) {
            return v;
        }
        if let Some(v) = self.try_eval_bufwriter_method(method, &obj, args, span) {
            return v;
        }
        if let Some(v) = self.try_eval_vector_method(method, object, &obj, args, span) {
            return v;
        }
        // A struct-shaped receiver with a registered impl method dispatches
        // to that impl BEFORE the builtin container arms below. Without this,
        // a user method sharing a builtin seq name (`first`, `last`,
        // `get_unchecked`, …) was captured by the builtin arm, which swallows
        // receiver shapes it doesn't handle into `Value::Unit` — a trait
        // impl's `first()` on a user struct silently returned `()`
        // (B-2026-07-02-10). Struct-shaped runtime types (HTTP Request, Arena,
        // Interner, …) keep their native intercepts: those run above this
        // hop, and their `#[compiler_builtin]` methods are never
        // env-registered, so the lookup misses and falls through.
        // S6c-12: the handle-backed containers (Column/Tensor/DataFrame) also
        // reach here for a USER method — their builtin intercepts above
        // (`try_eval_column_method` / `try_eval_tensor_method` / …) return
        // `None` for a name they don't own, so only a genuinely user-defined
        // `impl Trait for Column[T]` method arrives, and builtin names still
        // win (their intercept ran first). Registration keys it `Column.method`
        // (last path segment), matching `value_type_name` above.
        if matches!(
            &obj,
            Value::Struct { .. }
                | Value::SharedStruct(_)
                | Value::Column { .. }
                | Value::Tensor { .. }
                | Value::DataFrame { .. }
        ) {
            if let Some(v) = self.try_eval_impl_method(object, method, args, span, &obj) {
                return v;
            }
        }

        if let Some(v) = self.try_eval_seq_method(
            method,
            object,
            clone_receiver(&obj),
            args,
            span,
            args_close_span,
        ) {
            return v;
        }

        // `.cmp(other)` on a `#[derive(Ord)]` struct/enum receiver returns
        // `Ordering` — the method form of the `<`/`>` operators. `value_compare`
        // already orders `Value::Struct` (declaration-order fields) and
        // `Value::EnumVariant` (variant index then payload) lexicographically —
        // the same ordering the operators use — so this just wraps its result
        // in the `Ordering` enum. The typechecker (`expr_method_call.rs`) admits
        // the call for a derived-Ord Named receiver; this makes it evaluate.
        // roadmap Phase 8 § Eq/Ord.
        if method == "cmp"
            && args.len() == 1
            && matches!(
                &obj,
                Value::Struct { .. } | Value::SharedStruct(_) | Value::EnumVariant { .. }
            )
        {
            let other = self.eval_expr_inner(&args[0].value);
            let ord = value_compare(&obj, &other);
            return Value::EnumVariant {
                enum_name: "Ordering".to_string(),
                variant: match ord {
                    std::cmp::Ordering::Less => "Less".to_string(),
                    std::cmp::Ordering::Equal => "Equal".to_string(),
                    std::cmp::Ordering::Greater => "Greater".to_string(),
                },
                data: EnumData::Unit,
            };
        }

        // Primitive value-receiver dispatch for the builtin Eq/Ord methods.
        // The typechecker registers `eq`/`ne`/`lt`/`le`/`gt`/`ge`/`cmp` for
        // every integer width, bool, char, String, and the F32/F64 total-
        // order wrappers (`register_builtin_impl("Ord", ...)` in
        // src/typechecker.rs) — but those registrations live in the
        // typechecker's env, not the interpreter's, so a call like
        // `b.cmp(a)` with a primitive receiver would otherwise fall through
        // to the impl-block lookup below and panic. The type-name receiver
        // form `i64.cmp(a, b)` already routes through `dispatch_lowered_op`;
        // this mirrors that path for the value-receiver form (one arg
        // instead of two) so `xs.sort_by(|a, b| b.cmp(a))` works.
        if matches!(
            &obj,
            Value::Int(_)
                | Value::Char(_)
                | Value::Bool(_)
                | Value::String(_)
                | Value::TotalFloat32(_)
                | Value::TotalFloat64(_)
        ) {
            if method == "cmp" && args.len() == 1 {
                let other = self.eval_expr_inner(&args[0].value);
                let ord = value_compare(&obj, &other);
                return Value::EnumVariant {
                    enum_name: "Ordering".to_string(),
                    variant: match ord {
                        std::cmp::Ordering::Less => "Less".to_string(),
                        std::cmp::Ordering::Equal => "Equal".to_string(),
                        std::cmp::Ordering::Greater => "Greater".to_string(),
                    },
                    data: EnumData::Unit,
                };
            }
            let bin_op = match method {
                "eq" => Some(BinOp::Eq),
                "ne" => Some(BinOp::NotEq),
                "lt" => Some(BinOp::Lt),
                "le" => Some(BinOp::LtEq),
                "gt" => Some(BinOp::Gt),
                "ge" => Some(BinOp::GtEq),
                _ => None,
            };
            if let Some(op) = bin_op {
                if args.len() == 1 {
                    let rhs = self.eval_expr_inner(&args[0].value);
                    // `x.lt(y)` / `x.gt(y)` on `u64` / `usize` receivers: the call
                    // result is `bool`, so recover operand signedness from the
                    // receiver span. B-2026-07-04-8.
                    let unsigned_hint = self.span_type_is_unsigned64(&object.span)
                        || self.span_type_is_unsigned64(&args[0].value.span);
                    return self.eval_binary(&op, obj.clone(), rhs, span, unsigned_hint);
                }
            }
        }

        // Built-in `abs` on signed-integer / float primitives (typed in
        // expr_method_call.rs). `iN::MIN.abs()` doesn't fit and traps as
        // `integer overflow`, matching the `checked_neg` arm in eval_ops.rs;
        // float abs follows IEEE (`f64::abs`). The primitive Eq/Ord block
        // above intentionally excludes `Value::Float`, so this is its own
        // arm handling both numeric value shapes.
        if method == "abs" && args.is_empty() {
            match &obj {
                Value::Int(n) => {
                    return match n.checked_abs() {
                        Some(a) => Value::Int(a),
                        None => self.record_runtime_error("integer overflow".to_string(), span),
                    };
                }
                Value::Float(f) => return Value::Float(f.abs()),
                _ => {}
            }
        }

        // Built-in `signum` (typed in expr_method_call.rs, signed-int / float
        // only): `iN::signum` → -1 / 0 / 1; `f64::signum` → -1.0 / +1.0 (sign of
        // a signed zero preserved) or NaN. Codegen mirrors this with a nested
        // `select` (int) / `copysign` + NaN guard (float).
        if method == "signum" && args.is_empty() {
            match &obj {
                Value::Int(n) => return Value::Int(n.signum()),
                Value::Float(f) => return Value::Float(f.signum()),
                _ => {}
            }
        }

        // Built-in float arithmetic helpers (typed in expr_method_call.rs,
        // float-only): `recip` = `1.0 / x`; `to_degrees` / `to_radians` scale
        // by Rust's exact constants. Codegen replicates the same `fdiv`/`fmul`
        // and constants, so `run == build` is bit-exact.
        if matches!(method, "recip" | "to_degrees" | "to_radians" | "fract") && args.is_empty() {
            if let Value::Float(f) = &obj {
                let r = match method {
                    "recip" => f.recip(),
                    "to_degrees" => f.to_degrees(),
                    "to_radians" => f.to_radians(),
                    "fract" => f.fract(),
                    _ => unreachable!(),
                };
                return Value::Float(r);
            }
        }

        // `min` / `max` on a numeric scalar (typed in expr_method_call.rs):
        // `a.min(b)` / `a.max(b)` → the smaller / larger. Handles both `Int` and
        // `Float` shapes; codegen lowers to a `select` on `icmp`/`fcmp`.
        if matches!(method, "min" | "max") && args.len() == 1 {
            match &obj {
                Value::Int(n) => {
                    let n = *n;
                    let other = self.eval_expr_inner(&args[0].value);
                    if self.pending_cf.is_some() {
                        return other;
                    }
                    if let Value::Int(m) = other {
                        return Value::Int(if method == "min" { n.min(m) } else { n.max(m) });
                    }
                }
                Value::Float(f) => {
                    let f = *f;
                    let other = self.eval_expr_inner(&args[0].value);
                    if self.pending_cf.is_some() {
                        return other;
                    }
                    if let Value::Float(g) = other {
                        return Value::Float(if method == "min" { f.min(g) } else { f.max(g) });
                    }
                }
                _ => {}
            }
        }

        // `clamp` on a numeric scalar (typed in expr_method_call.rs):
        // `v.clamp(lo, hi)` → `lo` if `v < lo`, else `hi` if `v > hi`, else `v`
        // (nested-bound form: `lo` wins on an inverted range, matching the
        // `clamp` free fn). Codegen lowers to nested `select`s.
        if method == "clamp" && args.len() == 2 {
            match &obj {
                Value::Int(v) => {
                    let v = *v;
                    let lo = self.eval_expr_inner(&args[0].value);
                    if self.pending_cf.is_some() {
                        return lo;
                    }
                    let hi = self.eval_expr_inner(&args[1].value);
                    if self.pending_cf.is_some() {
                        return hi;
                    }
                    if let (Value::Int(lo), Value::Int(hi)) = (lo, hi) {
                        let r = if v < lo {
                            lo
                        } else if v > hi {
                            hi
                        } else {
                            v
                        };
                        return Value::Int(r);
                    }
                }
                Value::Float(v) => {
                    let v = *v;
                    let lo = self.eval_expr_inner(&args[0].value);
                    if self.pending_cf.is_some() {
                        return lo;
                    }
                    let hi = self.eval_expr_inner(&args[1].value);
                    if self.pending_cf.is_some() {
                        return hi;
                    }
                    if let (Value::Float(lo), Value::Float(hi)) = (lo, hi) {
                        let r = if v < lo {
                            lo
                        } else if v > hi {
                            hi
                        } else {
                            v
                        };
                        return Value::Float(r);
                    }
                }
                _ => {}
            }
        }

        // Built-in `sqrt` on float primitives (typed in expr_method_call.rs):
        // `x.sqrt() -> Self`, IEEE `f64::sqrt` (NaN for negative input, as in
        // codegen's `llvm.sqrt`). Float-only; integer receivers fall through.
        if method == "sqrt" && args.is_empty() {
            if let Value::Float(f) = &obj {
                return Value::Float(f.sqrt());
            }
        }

        // Built-in scalar transcendental + rounding math on float primitives
        // (typed in expr_method_call.rs; surface in `crate::float_math`):
        // unary `sin`/`cos`/`tan`/`exp`/`ln`/`log2`/`floor`/`ceil`/`round`
        // (`x.m() -> Self`) and binary `pow`/`atan2` (`x.m(y) -> Self`). Each
        // delegates to the matching `f64::*`; codegen lowers to the equivalent
        // LLVM intrinsic (`atan2` to a libm call). Float-only — the typechecker
        // guarantees a `Value::Float` receiver, so integer obj falls through.
        if let Some(kind) = crate::float_math::classify(method) {
            if let Value::Float(x) = &obj {
                let x = *x;
                match kind {
                    crate::float_math::FloatMathKind::Unary if args.is_empty() => {
                        let r = match method {
                            "sin" => x.sin(),
                            "cos" => x.cos(),
                            "tan" => x.tan(),
                            "exp" => x.exp(),
                            "ln" => x.ln(),
                            "log2" => x.log2(),
                            "floor" => x.floor(),
                            "ceil" => x.ceil(),
                            "round" => x.round(),
                            "asin" => x.asin(),
                            "acos" => x.acos(),
                            "atan" => x.atan(),
                            "sinh" => x.sinh(),
                            "cosh" => x.cosh(),
                            "tanh" => x.tanh(),
                            "exp2" => x.exp2(),
                            "log10" => x.log10(),
                            "trunc" => x.trunc(),
                            "asinh" => x.asinh(),
                            "acosh" => x.acosh(),
                            "atanh" => x.atanh(),
                            "exp_m1" => x.exp_m1(),
                            "ln_1p" => x.ln_1p(),
                            _ => unreachable!("float_math unary classify/match drift"),
                        };
                        return Value::Float(r);
                    }
                    crate::float_math::FloatMathKind::Binary if args.len() == 1 => {
                        if let Value::Float(y) = self.eval_expr_inner(&args[0].value) {
                            let r = match method {
                                "pow" => x.powf(y),
                                "atan2" => x.atan2(y),
                                "hypot" => x.hypot(y),
                                "copysign" => x.copysign(y),
                                _ => unreachable!("float_math binary classify/match drift"),
                            };
                            return Value::Float(r);
                        }
                    }
                    _ => {}
                }
            }
        }

        // IEEE-754 bit reinterpretation (protobuf `float`/`double` codecs;
        // typed in expr_method_call.rs). `to_bits` → the f64 bit pattern as a
        // `u64`; `to_bits32` rounds to f32 then takes its `u32` pattern. The
        // inverse `bits_as_f64` / `bits_as_f32` read an integer's low bits back
        // as a float. Unsigned values are stored two's-complement in `Int`.
        if args.is_empty() {
            match (&obj, method) {
                (Value::Float(f), "to_bits") => return Value::Int(f.to_bits() as i64),
                (Value::Float(f), "to_bits32") => return Value::Int((*f as f32).to_bits() as i64),
                (Value::Int(b), "bits_as_f64") => return Value::Float(f64::from_bits(*b as u64)),
                (Value::Int(b), "bits_as_f32") => {
                    return Value::Float(f32::from_bits(*b as u32) as f64)
                }
                _ => {}
            }
        }

        // Wrapping integer arithmetic (typed in expr_method_call.rs): the
        // non-trapping sibling of `+`/`-`/`*` — two's-complement wraparound,
        // never `record_integer_overflow`. The typechecker restricts the
        // receiver + arg to the 64-bit widths (i64/u64/usize), all i64-backed
        // as `Value::Int(i64)`, so Rust's `i64::wrapping_*` is exact. (Gated on
        // the method name first so the argument is not evaluated for any other
        // 1-arg method on an integer receiver — `eval_expr_inner` is not
        // re-entrant-safe against double side effects.) Narrow-width masking
        // and i128/u128 are a tracked follow-on.
        if matches!(method, "wrapping_add" | "wrapping_sub" | "wrapping_mul") && args.len() == 1 {
            if let Value::Int(a) = &obj {
                let a = *a;
                if let Value::Int(b) = self.eval_expr_inner(&args[0].value) {
                    return Value::Int(match method {
                        "wrapping_add" => a.wrapping_add(b),
                        "wrapping_sub" => a.wrapping_sub(b),
                        "wrapping_mul" => a.wrapping_mul(b),
                        _ => unreachable!(),
                    });
                }
            }
        }

        // Overflow-aware integer arithmetic — `{checked,saturating,overflowing}_{add,sub,mul}`.
        // Width-correct: the receiver width comes from `expr_types[object.span]` (the same
        // span→type source `narrow_oob` uses). `checked_*` → `Option[Self]` (None on
        // overflow), `saturating_*` → `Self` (clamped), `overflowing_*` → `(Self, bool)`.
        // 64-bit unsigned reinterprets the `Value::Int(i64)` two's-complement bits as `u64`
        // (the model already stores unsigned values that way), so it is full-range correct.
        if let Some((fam, op)) = parse_overflow_arith(method) {
            if args.len() == 1 {
                if let Value::Int(a) = &obj {
                    let a = *a;
                    // Width from the ARGUMENT's span: the typechecker pins the
                    // arg to the receiver type, and the arg is a distinct leaf
                    // expression — unlike the receiver, whose span a chained
                    // `MethodCall` aliases (`x.checked_mul(y).is_none()`).
                    let w = self.overflow_arg_width(&args[0].value);
                    if let Value::Int(b) = self.eval_expr_inner(&args[0].value) {
                        return eval_overflow_arith(fam, op, a, b, w);
                    }
                }
            }
        }

        // Integer `.pow(exp)` (typed in expr_method_call.rs): `n.pow(k) -> Self`,
        // repeated multiplication that TRAPS `integer overflow` at the receiver
        // width — the same app/lib trap as the `*` operator. The receiver width
        // is read from the stash at `args_close_span` (the receiver's own span is
        // clobbered to `Self` after typecheck, which happens to be correct here,
        // but the close-paren leaf keeps recovery uniform with the bit intrinsics
        // and robust under chaining). The exponent is `u32`; it is evaluated
        // exactly once.
        if method == "pow" && args.len() == 1 {
            if let Value::Int(base) = &obj {
                let base = *base;
                if let Value::Int(exp) = self.eval_expr_inner(&args[0].value) {
                    let w = self.int_width_at(args_close_span);
                    return self.eval_int_pow(base, exp as u64, w, span);
                }
            }
        }

        // Euclidean division / remainder on `i64` (typed in expr_method_call.rs,
        // i64-only in this slice): `div_euclid` / `rem_euclid`, matching Rust's
        // `i64::{div_euclid,rem_euclid}`. Traps identically to `/` and `%` — a
        // zero divisor is `division by zero`, and `i64::MIN.{div,rem}_euclid(-1)`
        // is `integer overflow` (`checked_*_euclid` returns `None`). Codegen
        // mirrors the trap set via `emit_int_div_guards`.
        if matches!(method, "div_euclid" | "rem_euclid") && args.len() == 1 {
            if let Value::Int(a) = &obj {
                let a = *a;
                if let Value::Int(b) = self.eval_expr_inner(&args[0].value) {
                    if self.pending_cf.is_some() {
                        return Value::Int(b);
                    }
                    if b == 0 {
                        return self.record_runtime_error("division by zero", span);
                    }
                    let r = if method == "div_euclid" {
                        a.checked_div_euclid(b)
                    } else {
                        a.checked_rem_euclid(b)
                    };
                    return match r {
                        Some(v) => Value::Int(v),
                        None => self.record_integer_overflow(span),
                    };
                }
            }
        }

        // Bit intrinsics on integer scalars (typed in expr_method_call.rs):
        // `count_ones` / `leading_zeros` / `trailing_zeros` -> u32, computed at
        // the receiver width recovered from `args_close_span`. Signed `iN` values
        // are sign-extended in the i64-backed model, so the value is masked to the
        // width's low bits before counting.
        if args.is_empty()
            && matches!(
                method,
                "count_ones" | "count_zeros" | "leading_zeros" | "trailing_zeros"
            )
        {
            if let Value::Int(n) = &obj {
                let w = self.int_width_at(args_close_span);
                return Value::Int(eval_bit_intrinsic(method, *n, w) as i64);
            }
        }

        // `is_power_of_two` on unsigned integer scalars -> bool (typed in
        // expr_method_call.rs). The stored value is masked to the receiver width
        // recovered from `args_close_span` (a narrow unsigned value is already
        // zero-extended, but the mask keeps the test width-correct regardless);
        // the result is true iff exactly one bit is set — 0 is not a power of two.
        if args.is_empty() && method == "is_power_of_two" {
            if let Value::Int(n) = &obj {
                let w = self.int_width_at(args_close_span);
                let bits = match w {
                    IntW::S(b) | IntW::U(b) => b,
                };
                let masked: u64 = if bits >= 64 {
                    *n as u64
                } else {
                    (*n as u64) & ((1u64 << bits) - 1)
                };
                return Value::Bool(masked != 0 && masked & (masked - 1) == 0);
            }
        }

        // `next_power_of_two` on unsigned integer scalars -> Self (typed in
        // expr_method_call.rs). The smallest power of two ≥ self (0 and 1 → 1),
        // at the receiver width recovered from `args_close_span`. Traps
        // `integer overflow` when the result would exceed the width
        // (`self > 2^(bits-1)`), matching the `*`/`pow` trap policy.
        if args.is_empty() && method == "next_power_of_two" {
            if let Value::Int(n) = &obj {
                let w = self.int_width_at(args_close_span);
                let bits = match w {
                    IntW::S(b) | IntW::U(b) => b,
                };
                let m: u64 = if bits >= 64 {
                    *n as u64
                } else {
                    (*n as u64) & ((1u64 << bits) - 1)
                };
                // Overflow iff the smallest power of two ≥ m would be 2^bits.
                if m > (1u64 << (bits - 1)) {
                    return self.record_runtime_error("integer overflow".to_string(), span);
                }
                let result: u64 = if m <= 1 {
                    1
                } else {
                    // m ≤ 2^(bits-1), so the u128 next-power-of-two fits the width.
                    (m as u128).next_power_of_two() as u64
                };
                return Value::Int(result as i64);
            }
        }

        // `abs_diff(self, other) -> unsigned sibling` (typed in
        // expr_method_call.rs): |self - other| at the receiver width, always
        // non-negative, never traps. Computed in i128 (so a signed MIN/MAX diff
        // does not overflow) then masked to the width recovered from
        // `args_close_span` and returned zero-extended in the i64 model — a
        // 64-bit unsigned result rides its bit pattern and prints unsigned.
        if method == "abs_diff" && args.len() == 1 {
            if let Value::Int(a) = &obj {
                let a = *a;
                let other = self.eval_expr_inner(&args[0].value);
                if self.pending_cf.is_some() {
                    return other;
                }
                if let Value::Int(b) = other {
                    let w = self.int_width_at(args_close_span);
                    let (bits, signed) = match w {
                        IntW::S(x) => (x, true),
                        IntW::U(x) => (x, false),
                    };
                    let av: i128 = if signed {
                        a as i128
                    } else {
                        (a as u64) as i128
                    };
                    let bv: i128 = if signed {
                        b as i128
                    } else {
                        (b as u64) as i128
                    };
                    let diff: u128 = (av - bv).unsigned_abs();
                    let masked: u64 = if bits >= 64 {
                        diff as u64
                    } else {
                        (diff as u64) & ((1u64 << bits) - 1)
                    };
                    return Value::Int(masked as i64);
                }
            }
        }

        // Bit-permutation intrinsics `reverse_bits` / `swap_bytes` -> Self
        // (typed in expr_method_call.rs). Permute within the receiver width
        // recovered from `args_close_span`, then re-sign-extend so the i64-model
        // value round-trips (a narrow signed result keeps its two's-complement
        // shape). Codegen lowers to `llvm.bitreverse` / `llvm.bswap` on the iN.
        if args.is_empty() && matches!(method, "reverse_bits" | "swap_bytes") {
            if let Value::Int(n) = &obj {
                let w = self.int_width_at(args_close_span);
                return Value::Int(eval_bit_permute(method, *n, w));
            }
        }

        // Bit-rotation intrinsics `rotate_left(n)` / `rotate_right(n)` -> Self
        // (typed in expr_method_call.rs). Rotate within the receiver width
        // recovered from `args_close_span`; the amount is `u32`. Codegen lowers
        // to `llvm.fshl` / `llvm.fshr`.
        if matches!(method, "rotate_left" | "rotate_right") && args.len() == 1 {
            if let Value::Int(n) = &obj {
                let n = *n;
                let amount = self.eval_expr_inner(&args[0].value);
                if self.pending_cf.is_some() {
                    return amount;
                }
                if let Value::Int(amount) = amount {
                    let w = self.int_width_at(args_close_span);
                    return Value::Int(eval_bit_rotate(method, n, amount as u32, w));
                }
            }
        }

        // ASCII byte-classification predicates on integer scalars (the `u8`
        // bytes from `String.bytes()`): `is_ascii_digit` / `is_ascii_alphabetic`
        // / `is_ascii_hexdigit` → bool. Phase-8 floor for the self-hosting lexer
        // (typed in expr_method_call.rs; codegen lowers to inline range checks).
        // The value is masked to a byte first so callers can pass an arbitrary
        // integer without surprising sign/width behavior.
        if args.is_empty() {
            if let Value::Int(n) = &obj {
                let b = *n as u8;
                let r = match method {
                    "is_ascii_digit" => Some(b.is_ascii_digit()),
                    "is_ascii_alphabetic" => Some(b.is_ascii_alphabetic()),
                    "is_ascii_hexdigit" => Some(b.is_ascii_hexdigit()),
                    _ => None,
                };
                if let Some(r) = r {
                    return Value::Bool(r);
                }
            }
        }

        // Unicode `char` classification predicates (phase-12 #13):
        // `char.to_digit(radix) -> Option[u32]` (typed in expr_method_call.rs):
        // Rust's `char::to_digit`. An out-of-range radix (< 2 or > 36) traps,
        // matching Rust's panic; otherwise `Some(value)` when `self` is a digit
        // in that radix, `None` when it isn't.
        if method == "to_digit" && args.len() == 1 {
            if let Value::Char(c) = &obj {
                let c = *c;
                if let Value::Int(radix) = self.eval_expr_inner(&args[0].value) {
                    if !(2..=36).contains(&radix) {
                        return self.record_runtime_error(
                            format!("to_digit: radix must be in 2..=36, got {radix}"),
                            span,
                        );
                    }
                    return match c.to_digit(radix as u32) {
                        Some(d) => some_int(d as i64),
                        None => none_value(),
                    };
                }
            }
        }

        // `char.is_alphabetic()` / `is_numeric()` / `is_alphanumeric()` /
        // `is_whitespace()` → bool. The Unicode-aware companions of the ASCII
        // byte predicates above (codegen routes these through the
        // `karac_runtime_char_is_*` externs; interp uses Rust's `char` directly).
        if args.is_empty() {
            if let Value::Char(c) = &obj {
                let r = match method {
                    "is_alphabetic" => Some(c.is_alphabetic()),
                    "is_numeric" => Some(c.is_numeric()),
                    "is_alphanumeric" => Some(c.is_alphanumeric()),
                    "is_whitespace" => Some(c.is_whitespace()),
                    "is_uppercase" => Some(c.is_uppercase()),
                    "is_lowercase" => Some(c.is_lowercase()),
                    "is_ascii" => Some(c.is_ascii()),
                    _ => None,
                };
                if let Some(r) = r {
                    return Value::Bool(r);
                }
            }
        }

        // ASCII case folding on a `char` (typed in expr_method_call.rs):
        // `to_ascii_uppercase` / `to_ascii_lowercase` → char, mapping only the
        // ASCII letters (Rust's `char::to_ascii_*case`). Codegen inlines the
        // same codepoint arithmetic.
        if args.is_empty() {
            if let Value::Char(c) = &obj {
                let r = match method {
                    "to_ascii_uppercase" => Some(c.to_ascii_uppercase()),
                    "to_ascii_lowercase" => Some(c.to_ascii_lowercase()),
                    _ => None,
                };
                if let Some(r) = r {
                    return Value::Char(r);
                }
            }
        }

        // Float→int conversion families (phase-8 § "Saturating float→int",
        // slice 2; typed in expr_method_call.rs):
        // `f.{saturating,wrapping,checked,trunc}_to_<intN>()`. Semantics live in
        // `crate::numeric_conv` (shared with the typechecker / effectchecker).
        // `checked_*` yields `Option[intN]`; `trunc_*` raises a runtime panic on
        // NaN / out-of-range (the `panics`-effect form). Results widen through
        // `i128` and store into the `i64` `Value::Int`, so `u64`/`u128`/`i128`
        // magnitudes beyond `i64` are truncated here — the interpreter's
        // existing wide-int limitation; codegen (slice 4) is bit-exact.
        if args.is_empty() {
            if let Value::Float(f) = &obj {
                if let Some((family, _target, bits, signed)) =
                    crate::numeric_conv::parse_float_to_int(method)
                {
                    use crate::numeric_conv::{ConvOutcome, FloatToIntFamily};
                    let outcome =
                        crate::numeric_conv::convert_float_to_int(*f, family, bits, signed);
                    let make_none = || Value::EnumVariant {
                        enum_name: "Option".to_string(),
                        variant: "None".to_string(),
                        data: EnumData::Unit,
                    };
                    return match (family, outcome) {
                        (FloatToIntFamily::Checked, ConvOutcome::Value(v)) => Value::EnumVariant {
                            enum_name: "Option".to_string(),
                            variant: "Some".to_string(),
                            data: EnumData::Tuple(vec![Value::Int(v as i64)]),
                        },
                        (FloatToIntFamily::Checked, ConvOutcome::None) => make_none(),
                        (_, ConvOutcome::Value(v)) => Value::Int(v as i64),
                        (_, ConvOutcome::Panic) => {
                            self.record_runtime_error("float-to-int out of range".to_string(), span)
                        }
                        // Only `Checked` yields `None`; only `Trunc` yields
                        // `Panic` (see `convert_float_to_int`). This arm is a
                        // defensive fallback and is not reached in practice.
                        (_, ConvOutcome::None) => make_none(),
                    };
                }
            }
            // Int→float conversions (same slice): `n.to_f32()` / `n.to_f64()`.
            // `to_f32` rounds through `f32` then widens for the `f64`-backed
            // `Value::Float`; `to_f64` is the direct widening.
            if let Value::Int(n) = &obj {
                if method == "to_f32" {
                    return Value::Float((*n as f32) as f64);
                }
                if method == "to_f64" {
                    return Value::Float(*n as f64);
                }
            }
        }

        // Built-in `clone` on scalar `Copy` primitives (typed in
        // expr_method_call.rs) — identity. (`to_string` on primitives already
        // works through the `Display` fallback arm above.) String/struct
        // clone is handled by the impl-block path below / its own dispatch.
        if method == "clone"
            && args.is_empty()
            && matches!(
                &obj,
                Value::Int(_) | Value::Float(_) | Value::Bool(_) | Value::Char(_)
            )
        {
            return obj;
        }

        // Try to find method via impl block
        let type_name = self.value_type_name(&obj);
        if let Some(v) = self.try_eval_impl_method(object, method, args, span, &obj) {
            return v;
        }

        // No dispatch arm matched. For well-typed programs the typechecker has
        // already rejected unresolved methods (e.g. the numeric-primitive
        // `NoMethodFound` in expr_method_call.rs), so reaching here means an
        // interpreter dispatch arm is genuinely missing for a method the
        // typechecker accepted — emit a structured runtime error rather than
        // panicking (the "every phase emits diagnostics, never panic" rule;
        // `karac run` bypasses typecheck, so a typo on a primitive used to ICE
        // here instead of producing a clean error).
        self.record_runtime_error(
            format!(
                "method '{}' not found on type '{}' (no interpreter dispatch arm)",
                method, type_name
            ),
            span,
        )
    }

    /// Recover the integer width for the overflow-arith methods from the
    /// ARGUMENT's type in the typechecker's per-expression table (the same
    /// `expr_types` source `narrow_oob` uses). The argument is type-pinned to the
    /// receiver type by the typechecker, and — unlike the receiver — its span is
    /// not aliased by a chained `MethodCall` (`x.checked_mul(y).is_none()`, whose
    /// outer call would overwrite the receiver span's recorded type). Defaults to
    /// signed 64-bit when the type is unknown, matching the interpreter's
    /// i64-backed numeric model.
    fn overflow_arg_width(&self, arg: &Expr) -> IntW {
        self.int_width_at(&arg.span)
    }

    /// Map the integer type recorded at `span` in the typechecker's `expr_types`
    /// table to an `IntW` width. Shared width recovery for the overflow-arith
    /// (argument span) and the `pow` / bit-intrinsic (close-paren `args_close_span`)
    /// paths. Defaults to signed 64-bit when the type is unknown, matching the
    /// interpreter's i64-backed numeric model.
    fn int_width_at(&self, span: &Span) -> IntW {
        use crate::typechecker::types::{IntSize, Type, UIntSize};
        let key = crate::resolver::SpanKey::from_span(span);
        match self.typecheck_result.expr_types.get(&key) {
            Some(Type::Int(IntSize::I8)) => IntW::S(8),
            Some(Type::Int(IntSize::I16)) => IntW::S(16),
            Some(Type::Int(IntSize::I32)) => IntW::S(32),
            Some(Type::UInt(UIntSize::U8)) => IntW::U(8),
            Some(Type::UInt(UIntSize::U16)) => IntW::U(16),
            Some(Type::UInt(UIntSize::U32)) => IntW::U(32),
            // 64-bit unsigned (u64 / usize) is handled by reinterpreting the
            // i64 bit pattern as u64 — full-range correct.
            Some(Type::UInt(UIntSize::U64)) | Some(Type::UInt(UIntSize::Usize)) => IntW::U(64),
            // i64 / isize / unknown → signed 64-bit.
            _ => IntW::S(64),
        }
    }

    /// Evaluate `base.pow(exp)` at the receiver width `w`, trapping
    /// `integer overflow` (returning the runtime-error value) the moment a
    /// partial result leaves the width's range — matching the `*` operator's
    /// per-step trap. Square-and-multiply (O(log exp)); the intermediate squared
    /// base never overflows when the final result is in range (its exponent
    /// `2^k ≤ exp`), so checking it can't false-trap.
    fn eval_int_pow(&mut self, base: i64, exp: u64, w: IntW, span: &Span) -> Value {
        let (signed, bits) = match w {
            IntW::S(b) => (true, b),
            IntW::U(b) => (false, b),
        };
        let (lo, hi): (i128, i128) = if signed {
            (-(1i128 << (bits - 1)), (1i128 << (bits - 1)) - 1)
        } else {
            (0, (1i128 << bits) - 1)
        };
        let base128: i128 = if signed {
            base as i128
        } else {
            (base as u64) as i128
        };
        let in_range = |v: i128| v >= lo && v <= hi;
        let mut acc: i128 = 1;
        let mut b = base128;
        let mut e = exp;
        while e > 0 {
            if e & 1 == 1 {
                acc = match acc.checked_mul(b) {
                    Some(v) if in_range(v) => v,
                    _ => {
                        return self.record_runtime_error("integer overflow".to_string(), span);
                    }
                };
            }
            e >>= 1;
            if e > 0 {
                b = match b.checked_mul(b) {
                    Some(v) if in_range(v) => v,
                    _ => {
                        return self.record_runtime_error("integer overflow".to_string(), span);
                    }
                };
            }
        }
        Value::Int(acc as i64)
    }
}

/// Evaluate a width-correct bit intrinsic (`count_ones` / `leading_zeros` /
/// `trailing_zeros`) on the i64-backed value `n` at receiver width `w`. Signed
/// `iN` values are sign-extended in the model, so the value is masked to the
/// width's low bits before counting; `leading/trailing_zeros` count within the
/// width (`bits` on a zero input).
fn eval_bit_intrinsic(method: &str, n: i64, w: IntW) -> u32 {
    let bits = match w {
        IntW::S(b) | IntW::U(b) => b,
    };
    let masked: u128 = if bits >= 64 {
        (n as u64) as u128
    } else {
        ((n as u64) & ((1u64 << bits) - 1)) as u128
    };
    match method {
        "count_ones" => masked.count_ones(),
        // Zero bits within the `bits`-wide value: the complement of the ones.
        "count_zeros" => bits - masked.count_ones(),
        // Leading zeros within the `bits`-wide value: the 128-bit count minus
        // the high padding. For `masked == 0` this yields `bits`.
        "leading_zeros" => masked.leading_zeros() - (128 - bits),
        // Trailing zeros are width-independent for a non-zero value; the all-zero
        // value has `bits` trailing zeros.
        "trailing_zeros" => {
            if masked == 0 {
                bits
            } else {
                masked.trailing_zeros()
            }
        }
        _ => unreachable!("non-bit-intrinsic method routed to eval_bit_intrinsic: {method}"),
    }
}

/// Evaluate a width-correct bit permutation (`reverse_bits` / `swap_bytes`) on
/// the i64-backed value `n` at receiver width `w`, returning the result encoded
/// the way the interpreter models the receiver type: sign-extended from `bits`
/// for a signed narrow width, zero-extended otherwise. `reverse_bits` reverses
/// the `bits` low bits; `swap_bytes` reverses the `bits/8` bytes (identity for
/// `u8`/`i8`), matching Rust's `iN::{reverse_bits,swap_bytes}`.
fn eval_bit_permute(method: &str, n: i64, w: IntW) -> i64 {
    let (bits, signed) = match w {
        IntW::S(b) => (b, true),
        IntW::U(b) => (b, false),
    };
    let masked: u64 = if bits >= 64 {
        n as u64
    } else {
        (n as u64) & ((1u64 << bits) - 1)
    };
    let permuted: u64 = match method {
        // Reverse all 64 bits, then shift the meaningful `bits` back down.
        "reverse_bits" => {
            if bits >= 64 {
                masked.reverse_bits()
            } else {
                masked.reverse_bits() >> (64 - bits)
            }
        }
        "swap_bytes" => match bits {
            16 => u64::from((masked as u16).swap_bytes()),
            32 => u64::from((masked as u32).swap_bytes()),
            64 => masked.swap_bytes(),
            // 8-bit (and any non-multiple-of-16 width) → identity.
            _ => masked,
        },
        _ => unreachable!("non-permute method routed to eval_bit_permute: {method}"),
    };
    // Re-encode into the i64 model: sign-extend a signed narrow result whose
    // width-top bit is set, so it round-trips like the other narrow-int values.
    if signed && bits < 64 && (permuted & (1u64 << (bits - 1))) != 0 {
        (permuted | !((1u64 << bits) - 1)) as i64
    } else {
        permuted as i64
    }
}

/// Evaluate a width-correct bit rotation (`rotate_left` / `rotate_right`) on the
/// i64-backed value `n` at receiver width `w`, rotating by `amount` within the
/// receiver's `bits` (Rust `iN::rotate_{left,right}`, amount mod width). The
/// result is re-encoded like [`eval_bit_permute`] (sign-extended for a signed
/// narrow width). Rotation is bit-level, so signedness only affects the final
/// encoding, not the rotated bits.
fn eval_bit_rotate(method: &str, n: i64, amount: u32, w: IntW) -> i64 {
    let (bits, signed) = match w {
        IntW::S(b) => (b, true),
        IntW::U(b) => (b, false),
    };
    let masked: u64 = if bits >= 64 {
        n as u64
    } else {
        (n as u64) & ((1u64 << bits) - 1)
    };
    let left = method == "rotate_left";
    let rotated: u64 = match bits {
        8 => {
            let v = masked as u8;
            u64::from(if left {
                v.rotate_left(amount)
            } else {
                v.rotate_right(amount)
            })
        }
        16 => {
            let v = masked as u16;
            u64::from(if left {
                v.rotate_left(amount)
            } else {
                v.rotate_right(amount)
            })
        }
        32 => {
            let v = masked as u32;
            u64::from(if left {
                v.rotate_left(amount)
            } else {
                v.rotate_right(amount)
            })
        }
        _ => {
            if left {
                masked.rotate_left(amount)
            } else {
                masked.rotate_right(amount)
            }
        }
    };
    if signed && bits < 64 && (rotated & (1u64 << (bits - 1))) != 0 {
        (rotated | !((1u64 << bits) - 1)) as i64
    } else {
        rotated as i64
    }
}

/// The overflow-arith method family: return-shape selector.
#[derive(Clone, Copy)]
enum OvFam {
    Checked,
    Saturating,
    Overflowing,
}

/// The overflow-arith operation.
#[derive(Clone, Copy)]
enum OvOp {
    Add,
    Sub,
    Mul,
}

/// Receiver integer width: `S(bits)` signed, `U(bits)` unsigned.
#[derive(Clone, Copy)]
enum IntW {
    S(u32),
    U(u32),
}

/// Parse a `{checked,saturating,overflowing}_{add,sub,mul}` method name.
fn parse_overflow_arith(method: &str) -> Option<(OvFam, OvOp)> {
    let (fam, rest) = if let Some(r) = method.strip_prefix("checked_") {
        (OvFam::Checked, r)
    } else if let Some(r) = method.strip_prefix("saturating_") {
        (OvFam::Saturating, r)
    } else {
        let r = method.strip_prefix("overflowing_")?;
        (OvFam::Overflowing, r)
    };
    let op = match rest {
        "add" => OvOp::Add,
        "sub" => OvOp::Sub,
        "mul" => OvOp::Mul,
        _ => return None,
    };
    Some((fam, op))
}

fn some_int(v: i64) -> Value {
    Value::EnumVariant {
        enum_name: "Option".to_string(),
        variant: "Some".to_string(),
        data: EnumData::Tuple(vec![Value::Int(v)]),
    }
}

fn none_value() -> Value {
    Value::EnumVariant {
        enum_name: "Option".to_string(),
        variant: "None".to_string(),
        data: EnumData::Unit,
    }
}

/// Evaluate one overflow-aware integer operation at the receiver's width.
/// Operands arrive as the interpreter's i64-backed `Value::Int`; signed widths
/// and 64-bit unsigned compute exactly (i128 / u64), narrow unsigned widths use
/// their `[0, 2^bits)` bounds.
fn eval_overflow_arith(fam: OvFam, op: OvOp, a: i64, b: i64, w: IntW) -> Value {
    // 64-bit unsigned: reinterpret the two's-complement bits as u64 (full range).
    if let IntW::U(64) = w {
        let (au, bu) = (a as u64, b as u64);
        let (res, of) = match op {
            OvOp::Add => au.overflowing_add(bu),
            OvOp::Sub => au.overflowing_sub(bu),
            OvOp::Mul => au.overflowing_mul(bu),
        };
        return match fam {
            OvFam::Checked => {
                if of {
                    none_value()
                } else {
                    some_int(res as i64)
                }
            }
            OvFam::Saturating => {
                let s = if of {
                    match op {
                        OvOp::Sub => 0u64, // underflow → 0
                        _ => u64::MAX,     // add/mul overflow → MAX
                    }
                } else {
                    res
                };
                Value::Int(s as i64)
            }
            OvFam::Overflowing => Value::Tuple(vec![Value::Int(res as i64), Value::Bool(of)]),
        };
    }

    // Signed widths (8/16/32/64) and narrow unsigned (8/16/32): exact in i128.
    let (signed, bits) = match w {
        IntW::S(b) => (true, b),
        IntW::U(b) => (false, b),
    };
    let (lo, hi): (i128, i128) = if signed {
        (-(1i128 << (bits - 1)), (1i128 << (bits - 1)) - 1)
    } else {
        (0, (1i128 << bits) - 1)
    };
    // Unsigned narrow values are stored non-negative; signed keep their sign.
    let av = if signed {
        a as i128
    } else {
        (a as u64) as i128
    };
    let bv = if signed {
        b as i128
    } else {
        (b as u64) as i128
    };
    let r: i128 = match op {
        OvOp::Add => av + bv,
        OvOp::Sub => av - bv,
        OvOp::Mul => av * bv,
    };
    let in_range = r >= lo && r <= hi;
    match fam {
        OvFam::Checked => {
            if in_range {
                some_int(r as i64)
            } else {
                none_value()
            }
        }
        OvFam::Saturating => Value::Int(r.clamp(lo, hi) as i64),
        OvFam::Overflowing => {
            // Wrap into the width's value set, then back to signed range if signed.
            let modulus = 1i128 << bits;
            let mut wrapped = ((r % modulus) + modulus) % modulus;
            if signed && wrapped > hi {
                wrapped -= modulus;
            }
            Value::Tuple(vec![Value::Int(wrapped as i64), Value::Bool(!in_range)])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_probe::{collection_receiver_clones, reset_collection_receiver_clones};

    /// Run `program` to completion, counting heavy-collection-receiver
    /// (`Map`/`Vec`) deep-clones performed in the `eval_method_call` dispatch
    /// loop, and assert its trimmed stdout equals `expected_output`. Returns
    /// the clone count.
    ///
    /// The interpreter runs **inline on this thread** (mirroring
    /// `run_program_full`'s pipeline). `crate::run_program` runs it on a freshly
    /// spawned 16 MB-stack thread, which would increment the per-thread clone
    /// counter on that worker, not here — the tiny gate programs can't overflow
    /// the default test stack.
    fn dispatch_clone_count(program: &str, expected_output: &str) -> u32 {
        let mut parsed = crate::parse(program);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        crate::desugar_program(&mut parsed.program);
        let resolved = crate::resolve(&parsed.program);
        let typed = crate::typecheck(&parsed.program, &resolved);
        crate::lower(&mut parsed.program, &typed);
        let mut interp = crate::interpreter::Interpreter::new(&parsed.program, &typed);
        interp.captured_output = Some(Vec::new());

        reset_collection_receiver_clones();
        interp.run();

        let out = interp.captured_output.take().unwrap_or_default();
        assert_eq!(
            out.join("").trim(),
            expected_output,
            "unexpected program output"
        );
        collection_receiver_clones()
    }

    /// Perf gate for the B-2026-06-07-4 map-heavy regression (fixed in
    /// `6a049301`, which left no regression test — this is it). A method call
    /// on a `Map` receiver must deep-clone the map an O(1) number of times in
    /// the `eval_method_call` dispatch loop, independent of how many category
    /// guards exist above the map handler. The regression was speculative
    /// guards each deep-cloning the map (O(N)) per op, turning the O(n²)
    /// `hash_map.kara` kata's cost into extra whole-map clones per operation.
    ///
    /// As of B-2026-06-07-4a (the pre-map iterator/http/regex/set guards
    /// borrow `&obj`, and the map call site is borrow-checked before cloning),
    /// the ONLY clone a `Map` receiver incurs is the accepting map handler's
    /// own, so the count is **1** — the tight O(1) end-state.
    ///
    /// If this fails:
    /// - count **went up** → a new category guard above the map handler takes
    ///   the receiver by value. Make it borrow (`&obj`), or borrow-check its
    ///   call site before `clone_receiver` — that is the fix, not bumping the
    ///   ceiling. (See the `clone_receiver` doc comment.)
    /// - count **went down** → the map handler stopped cloning. Good — lower
    ///   `EXPECTED`. The O(1) property is what matters, not the constant.
    #[test]
    fn map_receiver_dispatch_clones_are_bounded() {
        // `Map.new()` is an associated-fn call (never enters the value-receiver
        // guard loop) and the f-string interpolates a bare `i64` binding, so
        // `m.get_or(..)` is the only Map-receiver dispatch.
        const EXPECTED: u32 = 1;
        let clones = dispatch_clone_count(
            "fn main() {\n\
                 let m: Map[i64, i64] = Map.new();\n\
                 let v = m.get_or(1, 0);\n\
                 println(f\"{v}\")\n\
             }",
            "0",
        );
        assert_eq!(
            clones, EXPECTED,
            "a single Map-receiver method dispatch deep-cloned the map {clones} times \
             (expected {EXPECTED}); see this test's doc comment"
        );
    }

    /// Perf gate for `Vec` (`Value::Array`) receivers — the post-map residue of
    /// B-2026-06-07-4a. A `Vec` receiver traverses every guard down to the
    /// `seq` handler; before this slice each by-value guard between the map
    /// handler and `seq` (`map`/`option_result`/`channel`/`file`/`bufreader`/
    /// `bufwriter`/`vector`) deep-cloned the vector (~8 clones/op for a
    /// Vec/String workload). Now those guards borrow `&obj` (or borrow-check
    /// before cloning), so the ONLY clone is `seq`'s own when it accepts the
    /// method — count **1**, the same O(1) end-state as the map gate.
    ///
    /// Same failure interpretation as `map_receiver_dispatch_clones_are_bounded`:
    /// a rise means a new by-value guard above `seq` that should borrow `&obj`.
    #[test]
    fn vec_receiver_dispatch_clones_are_bounded() {
        // `[1, 2, 3]` is a `Value::Array`; `.contains(..)` routes through the
        // full category-guard chain to the `seq` handler (unlike `len`/
        // `is_empty`, which are intercepted inline before the guards), so it
        // exercises every post-map guard. `println` of the bound `bool`
        // triggers no further collection dispatch.
        const EXPECTED: u32 = 1;
        let clones = dispatch_clone_count(
            "fn main() {\n\
                 let v: Vec[i64] = [1, 2, 3];\n\
                 let found = v.contains(2);\n\
                 println(f\"{found}\")\n\
             }",
            "true",
        );
        assert_eq!(
            clones, EXPECTED,
            "a single Vec-receiver method dispatch deep-cloned the vector {clones} times \
             (expected {EXPECTED}); see this test's doc comment — a rise means a new \
             by-value guard above the seq handler, which should borrow `&obj` instead"
        );
    }
}
