//! Method-call evaluation: the big `eval_method_call` dispatch on
//! receiver shape (Vec/String/Slice/Map/Set/iterator-adapters/etc.).
//!
//! Lives in a sibling `impl<'a> super::Interpreter<'a>` block.

use crate::ast::*;
use crate::reduce_kernel::ReduceOp;
use crate::token::Span;
use std::sync::{Arc, RwLock};

use super::eval_expr::cast_value;
use super::exec::ControlFlow;
use super::helpers::{kara_json_to_serde_json, value_compare};
use super::pascal_to_snake;
use super::value::narrow_to_i64;
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
        result_ok(Value::Int(n.into()))
    } else {
        Value::EnumVariant {
            enum_name: "Result".to_string(),
            variant: "Err".to_string(),
            data: EnumData::Tuple(vec![Value::String(format!("out of range for {}", target))]),
        }
    }
}

/// `true` iff `name` is an integer type that carries a numeric `try_from`.
/// The single scalar of a Unicode case mapping, or `None` when it expands to
/// several. The twin of the runtime's `single_scalar` (`runtime/src/lib.rs`),
/// which backs the same two methods under codegen — keep the two in step.
fn single_scalar_case_map(mut it: impl Iterator<Item = char>) -> Option<char> {
    let first = it.next()?;
    it.next().is_none().then_some(first)
}

pub(super) fn is_numeric_try_from_target(name: &str) -> bool {
    matches!(
        name,
        "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64" | "usize" | "isize"
    )
}

impl<'a> super::Interpreter<'a> {
    /// Head type name of an `impl` block's target (`impl Bx { … }` -> `Bx`),
    /// for matching a method against its OWNING type rather than by bare name.
    pub(super) fn impl_target_head(te: &crate::ast::TypeExpr) -> Option<String> {
        match &te.kind {
            crate::ast::TypeKind::Path(p) => p.segments.last().cloned(),
            _ => None,
        }
    }

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
        // B-2026-08-13-8 — when two impls target two instantiations of one type
        // (`Vec[i64]` and `Vec[String]`), the head name this key is built from
        // is not an identity, and a type-ERASED runtime value cannot recover the
        // missing half: a `Value::Array` of ints knows nothing about its static
        // element type. So the typechecker, which resolved the call correctly at
        // check time, hands over the winning impl's qualified segment for this
        // exact call site and it is used verbatim. Without it the env kept
        // whichever impl was registered LAST and answered every receiver with
        // it — while codegen answered every receiver with the FIRST.
        //
        // The table is keyed by (span, method) so the chained-call span
        // aliasing (`recv.inner().outer()` share one span) cannot let an inner
        // link read the outer call's entry.
        if let Some(qualified) = self.typecheck_result.method_impl_dispatch.get(&(
            crate::resolver::SpanKey::from_span(span),
            method.to_string(),
        )) {
            type_name = qualified.clone();
            method_key = format!("{}.{}", qualified, method);
        }
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
                // B-2026-08-14-2 — an int argument at a FLOAT-declared method
                // parameter is the implicit widening the language permits, and
                // this dispatch path knows the receiver's type NAME, so the
                // signature can be resolved exactly rather than by bare method
                // name (two impls may both define `echo`). The self slot is
                // padded with a non-float placeholder so the index arithmetic
                // matches `param_patterns`, which carries the receiver.
                let declared_tys: Option<Vec<crate::ast::TypeExpr>> =
                    self.program.items.iter().find_map(|item| {
                        let crate::ast::Item::ImplBlock(imp) = item else {
                            return None;
                        };
                        imp.items.iter().find_map(|it| match it {
                            crate::ast::ImplItem::Method(m)
                                if m.name == method
                                    && Self::impl_target_head(&imp.target_type).as_deref()
                                        == Some(type_name.as_str()) =>
                            {
                                let mut tys: Vec<crate::ast::TypeExpr> = Vec::new();
                                if m.self_param.is_some() {
                                    tys.push(crate::ast::TypeExpr {
                                        kind: crate::ast::TypeKind::Tuple(Vec::new()),
                                        span: m.span,
                                    });
                                }
                                tys.extend(m.params.iter().map(|p| p.ty.clone()));
                                Some(tys)
                            }
                            _ => None,
                        })
                    });
                for (i, pat) in param_patterns.iter().enumerate() {
                    let val = if let Some(v) = arg_vals.get(i) {
                        v.clone()
                    } else if let Some(Some(default_expr)) = param_defaults.get(i) {
                        self.eval_expr_inner(default_expr)
                    } else {
                        continue;
                    };
                    let val = match declared_tys.as_ref().and_then(|tys| tys.get(i)) {
                        Some(te) => super::exec::coerce_int_value_to_declared_float(val, te),
                        None => val,
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

                // B-2026-08-01-6: expose the receiver mode to the body's
                // match machinery — `scrutinee_expr_is_consuming` treats a
                // `self` scrutinee as consuming only under an OWNED receiver
                // (a `ref self` match binds borrowed views whose bodies the
                // caller's owner fires, and `karac build` fires nothing).
                let pushed_self_mode = match self.method_self_param(&type_name, method) {
                    Some(sp) => {
                        // B-2026-08-01-7: an OWNED-`self` method consumes the
                        // receiver — a named value-enum binding's payload
                        // walk disarms like `let c = b;` would, leaving the
                        // arm channel inside the method as the payload's
                        // sole owner (it fired twice before: arm + walk).
                        // Enum receivers only; the struct receiver's single
                        // walk fire is the established in-parity convention.
                        if matches!(sp, crate::ast::SelfParam::Owned) {
                            if let ExprKind::Identifier(recv_name) = &object.kind {
                                if matches!(obj, Value::EnumVariant { .. }) {
                                    self.moved_out_container_bodies_bindings
                                        .insert(recv_name.clone());
                                }
                            }
                        }
                        self.self_param_stack.push(sp);
                        true
                    }
                    None => false,
                };
                // B-2026-08-01-13: expose the method's OWNED param names to
                // the body's param-scrutinee / destructure gates, exactly as
                // `eval_call` does for free fns — codegen's
                // `current_fn_param_names` covers methods, so the interp
                // stack must too or the gates diverge per-backend.
                self.owned_param_names_stack
                    .push(self.method_owned_param_names(&type_name, method));
                // A method frame hands its args to no caller-side fire — see
                // `owned_param_frame_is_method`.
                self.owned_param_frame_is_method.push(true);
                // B-2026-08-28-70 — and BECAUSE it reaches no caller-side fire,
                // this frame owns the `Drop` body of every owned param that
                // dies inside it. Seeded here, immediately before the body, so
                // `eval_block_inner` adopts it into the body block's own
                // cleanup; a tail that hands the param back disarms it through
                // `record_conditional_move_tail`. Same channel as the free-fn
                // seeding in `eval_call`, wider admission — see
                // `method_param_drop_names`.
                self.pending_param_drop_bindings = self.method_param_drop_names(&type_name, method);
                // B-2026-08-28-70 — isolate the callee frame's moved-out sets,
                // exactly as `eval_call` does for a free fn. They are keyed by
                // BINDING NAME with no frame qualifier, so without this a mark
                // left by one method leaks into the next frame that happens to
                // reuse the name: `Box3.add`'s `r` (moved into `self.xs`, so
                // legitimately marked there) suppressed the `Drop` slot of an
                // unrelated later method's `r`, and renaming that param to `q`
                // was enough to make the body reappear. Pre-existing — the
                // method path never had the free-fn path's save/restore — but
                // it only became observable once this frame started owning its
                // params, so it is fixed here rather than left as a trap.
                //
                // Taken AFTER B-2026-08-01-7's owned-enum-receiver insert just
                // above, deliberately: that insert names a CALLER-scope binding
                // and is meant for the caller's frame, so it must ride out on
                // the saved copy rather than be wiped by the restore.
                let saved_moved_out = (
                    std::mem::take(&mut self.moved_out_user_drop_bindings),
                    std::mem::take(&mut self.moved_out_enum_payload_bindings),
                    std::mem::take(&mut self.moved_out_drop_field_bindings),
                    std::mem::take(&mut self.moved_out_container_bodies_bindings),
                    std::mem::take(&mut self.moved_out_tuple_elem_bodies),
                    std::mem::take(&mut self.moved_out_struct_field_bodies),
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
                ) = saved_moved_out;
                self.owned_param_names_stack.pop();
                self.owned_param_frame_is_method.pop();
                if pushed_self_mode {
                    self.self_param_stack.pop();
                }

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
                let self_param = self.method_self_param(&type_name, method);
                let self_writeback = if matches!(self_param, Some(crate::ast::SelfParam::MutRef)) {
                    self.env.get("self")
                } else {
                    None
                };

                // B-2026-08-21-38 — the ARGUMENT half of the same CICO
                // write-back. `mut ref T` / `mut Slice[T]` method parameters
                // bind to a by-value copy in this scope exactly as `self`
                // does, so without this the callee's mutation of a SCALAR
                // parameter never reached the caller's variable — while the
                // identical free function (`eval_call.rs`) wrote it back, and
                // both compiled backends did too. Container arguments only
                // looked correct because `Value::Vec`/`String` share their
                // buffer, so the callee mutated the caller's storage directly.
                //
                // Gated on a self-taking method because that is what makes the
                // index arithmetic sound: `param_patterns` carries a leading
                // `self` slot, so declared parameter `i` — and therefore
                // `args[i]` — is `param_patterns[i + 1]`.
                let arg_writebacks: Vec<(Expr, Value)> = if self_param.is_some() {
                    let param_mut_ref = self.method_param_mut_ref_flags(&type_name, method);
                    args.iter()
                        .enumerate()
                        .filter(|(i, arg)| {
                            arg.mut_marker
                                || param_mut_ref
                                    .as_ref()
                                    .and_then(|f| f.get(*i))
                                    .copied()
                                    .unwrap_or(false)
                        })
                        .filter(|(_, arg)| Self::place_is_writeback_safe(&arg.value))
                        .filter_map(|(i, arg)| {
                            let pat = param_patterns.get(i + 1)?;
                            let PatternKind::Binding(param_name) = &pat.kind else {
                                return None;
                            };
                            let val = self.env.get(param_name)?;
                            Some((arg.value.clone(), val))
                        })
                        .collect()
                } else {
                    Vec::new()
                };

                self.env.pop_scope();

                for (place, val) in arg_writebacks {
                    self.assign_to_place(&place, val);
                }

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
                // B-2026-08-01-5: a fresh Drop-bearing RECEIVER temp dies at
                // this statement — a `ref self` / `mut ref self` method only
                // borrowed it, so the caller owns the body, fired here where
                // codegen's `__urecv_drop_tmp` registration drains
                // (statement end). Owned-`self` consumed the value and
                // borrow-returning methods alias it — both stay silent.
                self.run_fresh_recv_temp_drop(object, method, &type_name, obj);
                return Some(match result {
                    Ok(v) => v,
                    Err(ControlFlow::Return(v)) => v,
                    Err(cf) => self.set_cf(cf),
                });
            }
        }
        None
    }

    /// B-2026-08-01-5 — the method-receiver sibling of the free-fn
    /// fresh-temp arg hook (`run_fresh_temp_arg_drops`): run the user Drop
    /// body work for a FRESH receiver temp after a borrowing method call.
    /// Admitted shapes mirror the codegen registrar exactly: the receiver
    /// expr is a Call (user fn / variant ctor) or a struct literal — an
    /// Identifier/place receiver is its binding's own business, and a
    /// CHAAIN link (MethodCall receiver) stays silent on both backends
    /// (recorded residual); the method takes `ref self` / `mut ref self`
    /// (owned `self` consumed the value) and does not return a borrow (the
    /// result would alias the receiver). Value walk per kind matches the
    /// discard battery: own body + field walk for structs, own body or the
    /// declared-type payload walk for value enums; shared receivers are
    /// refcount-driven and never reach the Struct/EnumVariant arms.
    fn run_fresh_recv_temp_drop(
        &mut self,
        object: &Expr,
        method: &str,
        type_name: &str,
        obj: &Value,
    ) {
        let fresh = match &object.kind {
            ExprKind::StructLiteral { .. } => true,
            ExprKind::Call { callee, .. } => match &callee.kind {
                ExprKind::Path { .. } => true,
                ExprKind::Identifier(n) => self
                    .program
                    .items
                    .iter()
                    .any(|it| matches!(it, Item::Function(f) if &f.name == n)),
                _ => false,
            },
            _ => false,
        };
        if !fresh {
            return;
        }
        if !matches!(
            self.method_self_param(type_name, method),
            Some(crate::ast::SelfParam::Ref | crate::ast::SelfParam::MutRef)
        ) {
            return;
        }
        if self.method_returns_borrow(type_name, method) {
            return;
        }
        // STRUCT receivers only — an ENUM receiver whose ref-self method
        // matches on `self` and binds the payload already fires the
        // match-arm channel (a pre-existing interp-only fire on borrowed
        // self, B-2026-08-01-6); adding the walk here double-fired. The
        // codegen twin skips enum receiver bodies identically.
        if let Value::Struct { name, .. } = obj {
            let tn = name.clone();
            if self.program.drop_method_keys.contains_key(&tn) {
                self.run_user_drop_body_on_value(&tn, obj.clone());
            } else if self.value_runs_user_drop(obj) {
                self.drop_user_drop_fields_of_value(obj);
            }
        }
    }

    /// Does the user impl method `type_name.method` declare a `ref`/`mut
    /// ref` RETURN type? Companion of `method_self_param` for the
    /// borrow-return exclusion above.
    fn method_returns_borrow(&self, type_name: &str, method: &str) -> bool {
        self.program.items.iter().any(|it| {
            let Item::ImplBlock(imp) = it else {
                return false;
            };
            let target_ok = matches!(&imp.target_type.kind, crate::ast::TypeKind::Path(p)
                if p.segments.last().is_some_and(|s| s == type_name));
            target_ok
                && imp.items.iter().any(|ii| {
                    let crate::ast::ImplItem::Method(f) = ii else {
                        return false;
                    };
                    f.name == method
                        && matches!(
                            f.return_type.as_ref().map(|t| &t.kind),
                            Some(crate::ast::TypeKind::Ref(_) | crate::ast::TypeKind::MutRef(_))
                        )
                })
        })
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

    /// `gpu.sum` / `gpu.prod` / `gpu.min` / `gpu.max` under the interpreter
    /// (B-2026-08-19-10, extended by B-2026-08-19-13).
    ///
    /// Runs the reduction on the CPU — but in the GPU's TREE ORDER, not a left
    /// fold, so `karac run` and `karac build` print the same bits. That is the
    /// whole point of specifying the order: `f32` addition is not associative,
    /// and an interpreter that summed left-to-right would disagree with every
    /// compiled run of the same program.
    fn eval_gpu_reduce(
        &mut self,
        args: &[CallArg],
        span: &Span,
        op: ReduceOp,
        spelling: &str,
    ) -> Value {
        if args.len() != 1 {
            return self.record_runtime_error(
                format!("gpu.{spelling} expects one buffer (found {})", args.len()),
                span,
            );
        }
        // A RESIDENT field reduction (`gpu.sum(buf.mass)`) is compiled-only,
        // like the `gpu.upload` that produced the buffer: there is no device
        // buffer here to project a field out of. Reported BEFORE the argument
        // is evaluated, because evaluating it is what goes wrong — the nested
        // `gpu.upload` records its own error and yields `Unit`, and the field
        // access then lands on a non-struct receiver and trips the invariant
        // assert there. The `let`-bound form happens to stop at the statement
        // boundary first; a temporary receiver has no such boundary, which is
        // why this guard is the fix rather than softening that assert.
        //
        // The typechecker's own table is the discriminator, so an ordinary
        // field access that yields a host `Vec[f32]` (`gpu.sum(rec.values)`)
        // is untouched — it is not a resident reduction and never gets an
        // entry.
        if self
            .typecheck_result
            .gpu_resident_field
            .contains_key(&crate::resolver::SpanKey(
                args[0].value.span.offset,
                args[0].value.span.length,
            ))
        {
            return self.record_runtime_error(
                format!(
                    "gpu.{spelling} over a device buffer's field requires the compiled \
                     path (`karac build`) — resident GPU buffers have no interpreter \
                     model"
                ),
                span,
            );
        }
        let Value::Array(rc) = self.eval_expr_inner(&args[0].value) else {
            return self.record_runtime_error(
                format!("gpu.{spelling} buffer must be a Vec of f32/i32/u32"),
                span,
            );
        };
        let elems = rc.read().unwrap().clone();

        // Integer or float buffer, decided from the ELEMENTS. An empty buffer
        // is genuinely ambiguous here — `Value::Array` carries no element type
        // and the interpreter has no expr-type table — but it is not
        // observable in the answer: `sum`/`prod` return identities that print
        // the same either way, and `min`/`max`/`mean` return `None` regardless.
        if matches!(elems.first(), Some(Value::Int(_))) {
            return self.eval_gpu_reduce_int(&elems, &args[0].value.span, span, op, spelling);
        }

        let mut xs: Vec<f32> = Vec::with_capacity(elems.len());
        for v in &elems {
            match v {
                Value::Float(f) => xs.push(*f as f32),
                // NOT `Value::Int(i) => *i as f32`. Slice 1 is f32-only, and
                // routing an integer through the f32 core silently loses
                // precision above 2^24 — `[16777217, 1]` would sum to
                // 16777216. The typechecker rejects integer buffers, so this
                // arm is unreachable from checked code; it stays loud for
                // `karac run`, which bypasses typecheck.
                _ => {
                    return self.record_runtime_error(
                        format!("gpu.{spelling} buffer element must be f32"),
                        span,
                    )
                }
            }
        }

        // `min`/`max`/`mean` of an empty buffer is `None`, not a number — the
        // same refusal `Stats.min` and `Vec.min` give, and the same one
        // `Stats.mean` gives by trapping. Checked BEFORE the fold because the
        // fold would happily return the padding identity (+inf) or `0.0 / 0`
        // (NaN), both plausible wrong answers rather than obvious ones.
        let fallible = matches!(op, ReduceOp::Min | ReduceOp::Max | ReduceOp::Mean);
        if fallible && xs.is_empty() {
            return Value::EnumVariant {
                enum_name: "Option".to_string(),
                variant: "None".to_string(),
                data: EnumData::Unit,
            };
        }

        // Any length: up to one workgroup's width this is a single halving
        // tree, beyond it a tree of per-workgroup partials — the same
        // recursion the multi-dispatch runtime performs, so the two surfaces
        // agree bit-for-bit rather than one refusing what the other answers.
        // `mean` is the specified tree sum divided once, on the host — see
        // `reduce_kernel::tree_mean_f32` for why the division cannot live in
        // the shader.
        let folded = if matches!(op, ReduceOp::Mean) {
            crate::reduce_kernel::tree_mean_f32(&xs)
        } else {
            crate::reduce_kernel::tree_reduce_f32(&xs, op)
        };
        match folded {
            // `f32 as f64` is exact (every f32 is an f64), so widening to the
            // interpreter's carrier cannot change the answer the GPU computed.
            Some(r) if fallible => Value::EnumVariant {
                enum_name: "Option".to_string(),
                variant: "Some".to_string(),
                data: EnumData::Tuple(vec![Value::Float(r as f64)]),
            },
            Some(r) => Value::Float(r as f64),
            // Only the ops that need more than one associative pass land here,
            // and sum / prod / min / max are the only spellings routed in —
            // unreachable from any program, loud if that ever changes.
            None => self.record_runtime_error(
                format!("gpu.{spelling} is not an expressible GPU reduction"),
                span,
            ),
        }
    }

    /// The INTEGER arm of `gpu.sum` / `gpu.min` / `gpu.max`
    /// (B-2026-08-19-13).
    ///
    /// Split from the float arm because the two differ in more than a carrier
    /// type: an integer reduction can OVERFLOW, and Kāra traps rather than
    /// wrapping. The twin reproduces the trap POINTS as well as the values, so
    /// `karac run` and `karac build` agree about which programs fail — see
    /// `reduce_kernel::tree_reduce_i32`, where the tree order is what decides
    /// whether a given buffer overflows at all.
    fn eval_gpu_reduce_int(
        &mut self,
        elems: &[Value],
        buf_span: &Span,
        span: &Span,
        op: ReduceOp,
        spelling: &str,
    ) -> Value {
        // SIGNEDNESS COMES FROM THE TYPECHECKER, not from the values. A
        // `Value::Int` carries no width or sign, and inferring one from the
        // range would be wrong in a way that only shows on big inputs: a
        // `Vec[u32]` of small values looks exactly like a `Vec[i32]`, so it
        // would be folded with the SIGNED overflow rule and trap somewhere
        // past 2^31 that the compiled path sails through. A run/build
        // divergence reachable only on large data is the worst shape this
        // could take, so the element type is read from the same hint codegen
        // reads. A missing entry means i32 — the only way to get here without
        // one is `karac run` on a program that skipped typecheck.
        let unsigned = self
            .typecheck_result
            .gpu_reduce_int_elems
            .get(&crate::resolver::SpanKey(buf_span.offset, buf_span.length))
            .is_some_and(|e| e == "u32");

        let fallible = matches!(op, ReduceOp::Min | ReduceOp::Max | ReduceOp::Mean);
        // Empty `min`/`max`/`mean` is `None`, checked before the fold for the
        // same reason the float arm does it: the fold would return the padding
        // identity (or divide by zero), a plausible wrong answer.
        if fallible && elems.is_empty() {
            return Value::EnumVariant {
                enum_name: "Option".to_string(),
                variant: "None".to_string(),
                data: EnumData::Unit,
            };
        }

        let width = if unsigned { "u32" } else { "i32" };
        let mut xs: Vec<i128> = Vec::with_capacity(elems.len());
        for v in elems {
            let Value::Int(i) = v else {
                return self.record_runtime_error(
                    format!("gpu.{spelling} buffer elements must all be {width}"),
                    span,
                );
            };
            // Defensive: the typechecker proves the width, so an out-of-range
            // element cannot reach here from checked code. `karac run`
            // bypasses typecheck, so it stays loud rather than truncating.
            let fits = if unsigned {
                u32::try_from(*i).is_ok()
            } else {
                i32::try_from(*i).is_ok()
            };
            if !fits {
                return self.record_runtime_error(
                    format!("gpu.{spelling} buffer element {i} does not fit in {width}"),
                    span,
                );
            }
            xs.push(*i);
        }

        // `mean` PROMOTES — the mean of `[1, 2]` is 1.5, matching `Stats.mean`
        // — and it promotes to f64 because the integer sum it divides is
        // exact. So it leaves the integer carrier entirely and cannot share
        // the fold below.
        if matches!(op, ReduceOp::Mean) {
            let folded = if unsigned {
                let u: Vec<u32> = xs.iter().map(|&i| i as u32).collect();
                crate::reduce_kernel::tree_mean_u32(&u)
            } else {
                let i: Vec<i32> = xs.iter().map(|&i| i as i32).collect();
                crate::reduce_kernel::tree_mean_i32(&i)
            };
            return match folded {
                Some(Ok(m)) => Value::EnumVariant {
                    enum_name: "Option".to_string(),
                    variant: "Some".to_string(),
                    data: EnumData::Tuple(vec![Value::Float(m)]),
                },
                // The SUM overflowed, even if the mean would not have. The
                // price of computing it exactly rather than promoting first.
                Some(Err(_)) => {
                    self.record_runtime_error(format!("integer overflow in gpu.{spelling}"), span)
                }
                None => Value::EnumVariant {
                    enum_name: "Option".to_string(),
                    variant: "None".to_string(),
                    data: EnumData::Unit,
                },
            };
        }

        let folded = if unsigned {
            let u: Vec<u32> = xs.iter().map(|&i| i as u32).collect();
            crate::reduce_kernel::tree_reduce_u32(&u, op).map(|r| r.map(|v| v as i128))
        } else {
            let i: Vec<i32> = xs.iter().map(|&i| i as i32).collect();
            crate::reduce_kernel::tree_reduce_i32(&i, op).map(|r| r.map(|v| v as i128))
        };

        match folded {
            Some(Ok(r)) if fallible => Value::EnumVariant {
                enum_name: "Option".to_string(),
                variant: "Some".to_string(),
                data: EnumData::Tuple(vec![Value::Int(r)]),
            },
            Some(Ok(r)) => Value::Int(r),
            // Kāra traps on integer overflow. Wrapping here would hand back a
            // plausible wrong number and disagree with `v.sum()`, which
            // already fails on the same condition.
            Some(Err(_)) => {
                self.record_runtime_error(format!("integer overflow in gpu.{spelling}"), span)
            }
            None => self.record_runtime_error(
                format!("gpu.{spelling} is not an expressible integer GPU reduction"),
                span,
            ),
        }
    }

    /// `gpu.variance(buf)` / `gpu.stddev(buf)` under the interpreter
    /// (B-2026-08-19-13).
    ///
    /// POPULATION form (÷ n), matching `Stats.variance` / `Stats.stddev`. Both
    /// passes go through the same sum tree the device uses, so the two agree
    /// bit-for-bit rather than within an epsilon.
    /// `gpu.prefix_sum(buffer)` — the interpreter twin of the device's
    /// three-phase scan (B-2026-08-19-13).
    ///
    /// Returns a `Vec[f32]`, not an `Option`: the prefix sums of an empty
    /// buffer are the empty Vec, so there is nothing for `None` to mean.
    ///
    /// The whole of the semantics lives in `tree_prefix_sum_f32` — the
    /// Hillis-Steele step order and the chunk recursion — so this is only the
    /// `Value` glue, exactly as the fold twins are.
    fn eval_gpu_prefix_sum(&mut self, args: &[CallArg], span: &Span) -> Value {
        if args.len() != 1 {
            return self.record_runtime_error(
                format!("gpu.prefix_sum expects one buffer (found {})", args.len()),
                span,
            );
        }
        let Value::Array(rc) = self.eval_expr_inner(&args[0].value) else {
            return self.record_runtime_error("gpu.prefix_sum buffer must be a Vec of f32", span);
        };
        let elems = rc.read().unwrap().clone();

        // INTEGER buffers take the CHECKED scan. Converting them to f32 would
        // lose both precision above 2^24 and the trap.
        if matches!(elems.first(), Some(Value::Int(_))) {
            return self.eval_gpu_prefix_sum_int(&elems, &args[0].value.span, span);
        }

        let mut xs: Vec<f32> = Vec::with_capacity(elems.len());
        for v in &elems {
            match v {
                Value::Float(f) => xs.push(*f as f32),
                _ => {
                    return self
                        .record_runtime_error("gpu.prefix_sum buffer element must be f32", span)
                }
            }
        }

        let scanned: Vec<Value> = crate::reduce_kernel::tree_prefix_sum_f32(&xs)
            .into_iter()
            .map(|f| Value::Float(f as f64))
            .collect();
        Value::Array(std::sync::Arc::new(std::sync::RwLock::new(scanned)))
    }

    /// `gpu.matmul(a, b)` — the interpreter twin of the tiled device kernel
    /// (B-2026-08-19-13).
    ///
    /// **The one twin in this family that is just the obvious loop**, and that
    /// is the finding rather than a shortcut: a tiled matmul accumulates in
    /// ascending `k`, exactly as the naive triple loop does, so there is no
    /// device-specific order to reproduce. `tiled_matmul_f32` carries the
    /// argument and the property test; this is the `Value` glue.
    ///
    /// Consequently `gpu.matmul(a, b)` and `a.matmul(b)` agree bit-for-bit,
    /// where `gpu.sum(v)` and `v.sum()` do not.
    fn eval_gpu_matmul(&mut self, args: &[CallArg], span: &Span) -> Value {
        if args.len() != 2 {
            return self.record_runtime_error(
                format!("gpu.matmul expects two tensors (found {})", args.len()),
                span,
            );
        }
        let a = self.eval_expr_inner(&args[0].value);
        let b = self.eval_expr_inner(&args[1].value);
        let (
            Value::Tensor {
                dims: ad,
                data: adata,
                ..
            },
            Value::Tensor {
                dims: bd,
                data: bdata,
                ..
            },
        ) = (&a, &b)
        else {
            return self
                .record_runtime_error("gpu.matmul operands must be rank-2 f32 tensors", span);
        };
        // Re-emitted as runtime guards even though the typechecker enforces
        // them, per module policy: a bypassed typecheck must trap, not
        // silently produce a differently-shaped product.
        if ad.len() != 2 || bd.len() != 2 {
            return self.record_runtime_error(
                format!(
                    "gpu.matmul requires rank-2 x rank-2, found rank {} x rank {}",
                    ad.len(),
                    bd.len()
                ),
                span,
            );
        }
        let (m, k) = (ad[0] as usize, ad[1] as usize);
        let (k2, n) = (bd[0] as usize, bd[1] as usize);
        if k != k2 {
            return self.record_runtime_error(
                format!("gpu.matmul inner dimensions mismatch: [{m}x{k}] x [{k2}x{n}]"),
                span,
            );
        }

        // INTEGER tensors take the checked route — a matmul is a sum of
        // products, and both can overflow.
        if matches!(adata.read().unwrap().first(), Some(Value::Int(_))) {
            return self.eval_gpu_matmul_int(&a, &b, m, k, n, span);
        }

        let to_f32 = |vs: &[Value]| -> Option<Vec<f32>> {
            vs.iter()
                .map(|v| match v {
                    Value::Float(f) => Some(*f as f32),
                    _ => None,
                })
                .collect()
        };
        let (Some(av), Some(bv)) = (
            to_f32(&adata.read().unwrap()),
            to_f32(&bdata.read().unwrap()),
        ) else {
            return self.record_runtime_error(
                "gpu.matmul is f32-only — an integer matmul needs a widening multiply WGSL \
                 lacks; `a.matmul(b)` runs integer tensors on the CPU",
                span,
            );
        };

        let Some(out) = crate::reduce_kernel::tiled_matmul_f32(&av, &bv, m, k, n) else {
            return self.record_runtime_error(
                format!(
                    "gpu.matmul operand length does not match its shape: [{m}x{k}] x [{k2}x{n}]"
                ),
                span,
            );
        };
        Value::Tensor {
            dims: std::sync::Arc::new(vec![m as i64, n as i64]),
            data: std::sync::Arc::new(std::sync::RwLock::new(
                out.into_iter().map(|f| Value::Float(f as f64)).collect(),
            )),
            elem: crate::interpreter::value::TensorElemWidth::F32,
        }
    }

    fn eval_gpu_variance(
        &mut self,
        args: &[CallArg],
        span: &Span,
        sqrt: bool,
        spelling: &str,
    ) -> Value {
        if args.len() != 1 {
            return self.record_runtime_error(
                format!("gpu.{spelling} expects one buffer (found {})", args.len()),
                span,
            );
        }
        let Value::Array(rc) = self.eval_expr_inner(&args[0].value) else {
            return self.record_runtime_error(
                format!("gpu.{spelling} buffer must be a Vec of f32, i32 or u32"),
                span,
            );
        };
        let elems = rc.read().unwrap().clone();

        // INTEGER OR FLOAT, decided from the elements — and the two take
        // genuinely different routes rather than one converting to the other.
        // The integer form is EXACT: an integer shift, exact `u64` squares,
        // one rounding at the very end. Converting an integer buffer to f32
        // and reusing the float twin would reintroduce precisely the
        // quantisation the integer path was built to avoid.
        if matches!(elems.first(), Some(Value::Int(_))) {
            return self.eval_gpu_variance_int(&elems, &args[0].value.span, span, sqrt, spelling);
        }

        let mut xs: Vec<f32> = Vec::with_capacity(elems.len());
        for v in &elems {
            match v {
                Value::Float(f) => xs.push(*f as f32),
                _ => {
                    return self.record_runtime_error(
                        format!("gpu.{spelling} buffer element must be f32"),
                        span,
                    )
                }
            }
        }

        // Population form, so `bessel` is false. An empty buffer has no
        // variance — `None`, the answer every other GPU reduction gives for
        // an empty input, where `Stats.variance` raises instead.
        let folded = if sqrt {
            crate::reduce_kernel::tree_stddev_f32(&xs, false)
        } else {
            crate::reduce_kernel::tree_variance_f32(&xs, false)
        };
        match folded {
            Some(v) => Value::EnumVariant {
                enum_name: "Option".to_string(),
                variant: "Some".to_string(),
                data: EnumData::Tuple(vec![Value::Float(v as f64)]),
            },
            None => Value::EnumVariant {
                enum_name: "Option".to_string(),
                variant: "None".to_string(),
                data: EnumData::Unit,
            },
        }
    }

    /// The INTEGER arm of `gpu.matmul` under the interpreter
    /// (B-2026-08-19-13).
    ///
    /// `tiled_matmul_int` carries the specification: the tiled order is the
    /// naive one, so the same intermediates are formed and `gpu.matmul` agrees
    /// with `a.matmul(b)` about which contractions overflow, not only about
    /// what the successful ones return.
    ///
    /// Signedness is read from the typechecker's hint at the CALL span (where
    /// `infer_gpu_matmul` records it), rather than guessed from the values —
    /// the same rule every integer GPU op here follows.
    fn eval_gpu_matmul_int(
        &mut self,
        a: &Value,
        b: &Value,
        m: usize,
        k: usize,
        n: usize,
        span: &Span,
    ) -> Value {
        let unsigned = self
            .typecheck_result
            .gpu_reduce_int_elems
            .get(&crate::resolver::SpanKey(span.offset, span.length))
            .is_some_and(|e| e == "u32");
        let width = if unsigned { "u32" } else { "i32" };

        let mut flat: Vec<Vec<i64>> = Vec::with_capacity(2);
        for t in [a, b] {
            let Value::Tensor { data, .. } = t else {
                return self.record_runtime_error("gpu.matmul operands must be tensors", span);
            };
            let guard = data.read().unwrap();
            let mut xs: Vec<i64> = Vec::with_capacity(guard.len());
            for v in guard.iter() {
                let Value::Int(i) = v else {
                    return self.record_runtime_error(
                        format!("gpu.matmul tensor elements must all be {width}"),
                        span,
                    );
                };
                let fits = if unsigned {
                    u32::try_from(*i).is_ok()
                } else {
                    i32::try_from(*i).is_ok()
                };
                if !fits {
                    return self.record_runtime_error(
                        format!("gpu.matmul tensor element {i} does not fit {width}"),
                        span,
                    );
                }
                xs.push(*i as i64);
            }
            flat.push(xs);
        }

        match crate::reduce_kernel::tiled_matmul_int(&flat[0], &flat[1], m, k, n, unsigned) {
            Some(Err(_)) => self.record_runtime_error("integer overflow", span),
            Some(Ok(out)) => Value::Tensor {
                dims: std::sync::Arc::new(vec![m as i64, n as i64]),
                data: std::sync::Arc::new(std::sync::RwLock::new(
                    out.into_iter().map(|v| Value::Int(v as i128)).collect(),
                )),
                elem: crate::interpreter::value::TensorElemWidth::F64,
            },
            None => self.record_runtime_error(
                format!(
                    "gpu.matmul operand length does not match its shape: [{m}x{k}] x [{k}x{n}]"
                ),
                span,
            ),
        }
    }

    /// The INTEGER arm of `gpu.prefix_sum` under the interpreter
    /// (B-2026-08-19-13).
    ///
    /// Returns a `Vec` of the same element type — a prefix sum maps a buffer
    /// to a buffer, so nothing promotes — and TRAPS on overflow, where the
    /// float form cannot fail at all.
    ///
    /// Every element is an output here, so an overflow anywhere is an overflow
    /// in the answer; `tree_prefix_sum_i32` checks each lane at each step
    /// rather than only the value that lands in `out`.
    fn eval_gpu_prefix_sum_int(&mut self, elems: &[Value], buf_span: &Span, span: &Span) -> Value {
        let unsigned = self
            .typecheck_result
            .gpu_reduce_int_elems
            .get(&crate::resolver::SpanKey(buf_span.offset, buf_span.length))
            .is_some_and(|e| e == "u32");
        let width = if unsigned { "u32" } else { "i32" };

        let mut sx: Vec<i32> = Vec::with_capacity(elems.len());
        let mut ux: Vec<u32> = Vec::with_capacity(elems.len());
        for v in elems {
            let Value::Int(i) = v else {
                return self.record_runtime_error(
                    format!("gpu.prefix_sum buffer elements must all be {width}"),
                    span,
                );
            };
            if unsigned {
                let Ok(x) = u32::try_from(*i) else {
                    return self.record_runtime_error(
                        format!("gpu.prefix_sum buffer element {i} does not fit u32"),
                        span,
                    );
                };
                ux.push(x);
            } else {
                let Ok(x) = i32::try_from(*i) else {
                    return self.record_runtime_error(
                        format!("gpu.prefix_sum buffer element {i} does not fit i32"),
                        span,
                    );
                };
                sx.push(x);
            }
        }

        let scanned = if unsigned {
            crate::reduce_kernel::tree_prefix_sum_u32(&ux)
                .map(|v| v.into_iter().map(|x| x as i128).collect::<Vec<_>>())
        } else {
            crate::reduce_kernel::tree_prefix_sum_i32(&sx)
                .map(|v| v.into_iter().map(|x| x as i128).collect::<Vec<_>>())
        };
        match scanned {
            Err(_) => self.record_runtime_error("integer overflow", span),
            Ok(vals) => Value::Array(std::sync::Arc::new(std::sync::RwLock::new(
                vals.into_iter().map(Value::Int).collect(),
            ))),
        }
    }

    /// The INTEGER arm of `gpu.dot` under the interpreter (B-2026-08-19-13).
    ///
    /// `tree_dot_i32` / `_u32` are literally products-then-`tree_reduce`, so
    /// the identity `gpu.dot(a, b) == gpu.sum(a * b)` — including which
    /// programs trap — holds by construction rather than by testing.
    ///
    /// Signedness comes from the typechecker's hint for the reason spelled out
    /// in `eval_gpu_reduce_int`: a `Vec[u32]` of small values is
    /// indistinguishable from a `Vec[i32]` at the `Value` level, and guessing
    /// would produce a divergence visible only on large data.
    fn eval_gpu_dot_int(
        &mut self,
        xa: &[Value],
        ya: &[Value],
        buf_span: &Span,
        span: &Span,
    ) -> Value {
        let unsigned = self
            .typecheck_result
            .gpu_reduce_int_elems
            .get(&crate::resolver::SpanKey(buf_span.offset, buf_span.length))
            .is_some_and(|e| e == "u32");
        let width = if unsigned { "u32" } else { "i32" };

        let mut sx: Vec<i32> = Vec::with_capacity(xa.len());
        let mut sy: Vec<i32> = Vec::with_capacity(ya.len());
        let mut ux: Vec<u32> = Vec::with_capacity(xa.len());
        let mut uy: Vec<u32> = Vec::with_capacity(ya.len());
        for (src, (sd, ud)) in [(xa, (&mut sx, &mut ux)), (ya, (&mut sy, &mut uy))] {
            for v in src {
                let Value::Int(i) = v else {
                    return self.record_runtime_error(
                        format!("gpu.dot buffer elements must all be {width}"),
                        span,
                    );
                };
                if unsigned {
                    let Ok(x) = u32::try_from(*i) else {
                        return self.record_runtime_error(
                            format!("gpu.dot buffer element {i} does not fit u32"),
                            span,
                        );
                    };
                    ud.push(x);
                } else {
                    let Ok(x) = i32::try_from(*i) else {
                        return self.record_runtime_error(
                            format!("gpu.dot buffer element {i} does not fit i32"),
                            span,
                        );
                    };
                    sd.push(x);
                }
            }
        }

        let folded = if unsigned {
            crate::reduce_kernel::tree_dot_u32(&ux, &uy).map(|r| r.map(|v| v as i128))
        } else {
            crate::reduce_kernel::tree_dot_i32(&sx, &sy).map(|r| r.map(|v| v as i128))
        };
        match folded {
            Some(Err(_)) => self.record_runtime_error("integer overflow in gpu.dot", span),
            Some(Ok(v)) => Value::Int(v),
            // Mismatched lengths trap here exactly as in the runtime entry
            // point: truncating to the shorter buffer would silently answer a
            // question nobody asked.
            None => self.record_runtime_error(
                format!(
                    "gpu.dot requires buffers of equal length ({} vs {})",
                    xa.len(),
                    ya.len()
                ),
                span,
            ),
        }
    }

    /// The INTEGER arm of `gpu.variance` / `gpu.stddev` under the interpreter
    /// (B-2026-08-19-13).
    ///
    /// Exact, matching the device: `tree_variance_i32` / `_u32` shift by an
    /// integer `K`, accumulate `Σd²` exactly, and round once. Returns
    /// `Option[f64]` rather than `Option[f32]` because the computation really
    /// does have that much precision — the same promotion `gpu.mean` makes
    /// over an integer buffer.
    ///
    /// Signedness comes from the TYPECHECKER's hint, not from the values, for
    /// the reason spelled out in `eval_gpu_reduce_int`: a `Vec[u32]` of small
    /// values is indistinguishable from a `Vec[i32]` at the `Value` level, and
    /// guessing would produce a divergence visible only on large data.
    fn eval_gpu_variance_int(
        &mut self,
        elems: &[Value],
        buf_span: &Span,
        span: &Span,
        sqrt: bool,
        spelling: &str,
    ) -> Value {
        let unsigned = self
            .typecheck_result
            .gpu_reduce_int_elems
            .get(&crate::resolver::SpanKey(buf_span.offset, buf_span.length))
            .is_some_and(|e| e == "u32");
        let width = if unsigned { "u32" } else { "i32" };

        let mut signed: Vec<i32> = Vec::with_capacity(elems.len());
        let mut unsigned_xs: Vec<u32> = Vec::with_capacity(elems.len());
        for v in elems {
            let Value::Int(i) = v else {
                return self.record_runtime_error(
                    format!("gpu.{spelling} buffer elements must all be {width}"),
                    span,
                );
            };
            if unsigned {
                let Ok(x) = u32::try_from(*i) else {
                    return self.record_runtime_error(
                        format!("gpu.{spelling} buffer element {i} does not fit u32"),
                        span,
                    );
                };
                unsigned_xs.push(x);
            } else {
                let Ok(x) = i32::try_from(*i) else {
                    return self.record_runtime_error(
                        format!("gpu.{spelling} buffer element {i} does not fit i32"),
                        span,
                    );
                };
                signed.push(x);
            }
        }

        // Population form, so `bessel` is false — the sample form is decided
        // against for this family (see the ledger row).
        let folded = match (unsigned, sqrt) {
            (true, true) => crate::reduce_kernel::tree_stddev_u32(&unsigned_xs, false),
            (true, false) => crate::reduce_kernel::tree_variance_u32(&unsigned_xs, false),
            (false, true) => crate::reduce_kernel::tree_stddev_i32(&signed, false),
            (false, false) => crate::reduce_kernel::tree_variance_i32(&signed, false),
        };
        match folded {
            // The squared deviations did not fit in `u64`. Traps, exactly as
            // an overflowing integer `gpu.sum` does — an integer reduction
            // that cannot represent its answer refuses rather than saturating.
            Some(Err(_)) => self.record_runtime_error("integer overflow", span),
            Some(Ok(v)) => Value::EnumVariant {
                enum_name: "Option".to_string(),
                variant: "Some".to_string(),
                data: EnumData::Tuple(vec![Value::Float(v)]),
            },
            None => Value::EnumVariant {
                enum_name: "Option".to_string(),
                variant: "None".to_string(),
                data: EnumData::Unit,
            },
        }
    }

    /// `gpu.argmin(buf)` / `gpu.argmax(buf)` under the interpreter
    /// (B-2026-08-19-13).
    ///
    /// The tree carries (value, index) pairs, and its combine — strictly
    /// better value wins, exact tie goes to the smaller index, NaN always
    /// loses — is lexicographic and therefore grouping-independent. So unlike
    /// `sum`, this answers the same at every buffer length.
    ///
    /// Note the NaN rule DIFFERS from `Stats.argmin`, which is
    /// position-dependent on NaN and so cannot be reproduced by a tree at all.
    /// See `reduce_kernel::tree_arg_f32`.
    fn eval_gpu_arg(
        &mut self,
        args: &[CallArg],
        span: &Span,
        want_max: bool,
        spelling: &str,
    ) -> Value {
        if args.len() != 1 {
            return self.record_runtime_error(
                format!("gpu.{spelling} expects one buffer (found {})", args.len()),
                span,
            );
        }
        let Value::Array(rc) = self.eval_expr_inner(&args[0].value) else {
            return self.record_runtime_error(
                format!("gpu.{spelling} buffer must be a Vec of f32, i32 or u32"),
                span,
            );
        };
        let elems = rc.read().unwrap().clone();

        // INTEGER buffers order differently, and the interpreter cannot tell
        // which kind it holds from the values: a `Value::Int` carries no width
        // or sign, so a `Vec[u32]` of small values looks exactly like a
        // `Vec[i32]`. Above 2^31 the two disagree about which element is the
        // minimum, so the element type comes from the typechecker's hint —
        // the same one the value reductions read.
        if matches!(elems.first(), Some(Value::Int(_))) {
            let unsigned = self
                .typecheck_result
                .gpu_reduce_int_elems
                .get(&crate::resolver::SpanKey(
                    args[0].value.span.offset,
                    args[0].value.span.length,
                ))
                .is_some_and(|e| e == "u32");
            let mut ints: Vec<i128> = Vec::with_capacity(elems.len());
            for v in &elems {
                let Value::Int(i) = v else {
                    return self.record_runtime_error(
                        format!("gpu.{spelling} buffer elements must all be integers"),
                        span,
                    );
                };
                ints.push(*i);
            }
            let found = if unsigned {
                let u: Vec<u32> = ints.iter().map(|&i| i as u32).collect();
                crate::reduce_kernel::tree_arg_u32(&u, want_max)
            } else {
                let i: Vec<i32> = ints.iter().map(|&i| i as i32).collect();
                crate::reduce_kernel::tree_arg_i32(&i, want_max)
            };
            return match found {
                Some(i) => Value::EnumVariant {
                    enum_name: "Option".to_string(),
                    variant: "Some".to_string(),
                    data: EnumData::Tuple(vec![Value::Int(i as i128)]),
                },
                None => Value::EnumVariant {
                    enum_name: "Option".to_string(),
                    variant: "None".to_string(),
                    data: EnumData::Unit,
                },
            };
        }

        let mut xs: Vec<f32> = Vec::with_capacity(elems.len());
        for v in &elems {
            match v {
                Value::Float(f) => xs.push(*f as f32),
                _ => {
                    return self.record_runtime_error(
                        format!("gpu.{spelling} buffer element must be f32"),
                        span,
                    )
                }
            }
        }

        match crate::reduce_kernel::tree_arg_f32(&xs, want_max) {
            Some(i) => Value::EnumVariant {
                enum_name: "Option".to_string(),
                variant: "Some".to_string(),
                data: EnumData::Tuple(vec![Value::Int(i as i128)]),
            },
            // Empty: no extremum. `Stats.argmin` says the same.
            None => Value::EnumVariant {
                enum_name: "Option".to_string(),
                variant: "None".to_string(),
                data: EnumData::Unit,
            },
        }
    }

    /// `gpu.dot(a, b)` under the interpreter (B-2026-08-19-13).
    ///
    /// Runs the products through the SAME tree order the sum reduction uses,
    /// because that is what the device does: its level-0 shader forms the
    /// product on load and then runs the identical halving tree, and every
    /// later level is the ordinary sum shader. `gpu.dot(a, b)` and
    /// `gpu.sum(a * b)` are therefore the same number on both surfaces.
    fn eval_gpu_dot(&mut self, args: &[CallArg], span: &Span) -> Value {
        if args.len() != 2 {
            return self.record_runtime_error(
                format!("gpu.dot expects two buffers (found {})", args.len()),
                span,
            );
        }
        let mut buffers: Vec<Vec<f32>> = Vec::with_capacity(2);
        // INTEGER buffers take a different route entirely — checked products
        // into a checked tree — because `gpu.dot == gpu.sum(a * b)` extends
        // over integers to WHICH PROGRAMS TRAP, and converting to f32 would
        // both lose precision above 2^24 and lose the trap.
        let mut int_pair: Option<(Vec<Value>, Vec<Value>)> = None;
        {
            let vals: Vec<Value> = args
                .iter()
                .take(2)
                .map(|a| self.eval_expr_inner(&a.value))
                .collect();
            if let [Value::Array(x), Value::Array(y)] = &vals[..] {
                let xa = x.read().unwrap().clone();
                let ya = y.read().unwrap().clone();
                if matches!(xa.first(), Some(Value::Int(_)))
                    || matches!(ya.first(), Some(Value::Int(_)))
                {
                    int_pair = Some((xa, ya));
                }
            }
        }
        if let Some((xa, ya)) = int_pair {
            return self.eval_gpu_dot_int(&xa, &ya, &args[0].value.span, span);
        }

        for arg in args.iter().take(2) {
            let Value::Array(rc) = self.eval_expr_inner(&arg.value) else {
                return self
                    .record_runtime_error("gpu.dot buffers must be Vec of f32".to_string(), span);
            };
            let elems = rc.read().unwrap().clone();
            let mut xs: Vec<f32> = Vec::with_capacity(elems.len());
            for v in &elems {
                match v {
                    Value::Float(f) => xs.push(*f as f32),
                    // Same reasoning as `eval_gpu_reduce`: routing an integer
                    // through the f32 core loses precision above 2^24. The
                    // typechecker rejects non-f32 buffers, so this is
                    // unreachable from checked code and stays loud for
                    // `karac run`, which bypasses typecheck.
                    _ => {
                        return self.record_runtime_error(
                            "gpu.dot buffer element must be f32".to_string(),
                            span,
                        )
                    }
                }
            }
            buffers.push(xs);
        }

        // Mismatched lengths trap here exactly as they do in the runtime entry
        // point — truncating to the shorter buffer would silently answer a
        // question nobody asked, and the two surfaces must refuse the same
        // programs, not just agree on the ones they accept.
        match crate::reduce_kernel::tree_dot_f32(&buffers[0], &buffers[1]) {
            Some(r) => Value::Float(r as f64),
            None => self.record_runtime_error(
                format!(
                    "gpu.dot requires buffers of equal length ({} vs {})",
                    buffers[0].len(),
                    buffers[1].len()
                ),
                span,
            ),
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
                call_args.push(Value::Int((i as i64).into()));
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
                // B-2026-08-19-10 slice 1: whole-buffer reductions. The
                // interpreter uses the SAME TREE ORDER as the shader, so this
                // is the definition of the result rather than an approximation
                // of it — see `reduce_kernel::tree_reduce_f32`.
                ("gpu", "sum") => return self.eval_gpu_reduce(args, span, ReduceOp::Sum, "sum"),
                ("gpu", "prod") => return self.eval_gpu_reduce(args, span, ReduceOp::Prod, "prod"),
                ("gpu", "min") => return self.eval_gpu_reduce(args, span, ReduceOp::Min, "min"),
                ("gpu", "max") => return self.eval_gpu_reduce(args, span, ReduceOp::Max, "max"),
                ("gpu", "mean") => return self.eval_gpu_reduce(args, span, ReduceOp::Mean, "mean"),
                ("gpu", "dot") => return self.eval_gpu_dot(args, span),
                ("gpu", "variance") => {
                    return self.eval_gpu_variance(args, span, false, "variance")
                }
                ("gpu", "stddev") => return self.eval_gpu_variance(args, span, true, "stddev"),
                ("gpu", "prefix_sum") => return self.eval_gpu_prefix_sum(args, span),
                ("gpu", "matmul") => return self.eval_gpu_matmul(args, span),
                ("gpu", "argmin") => return self.eval_gpu_arg(args, span, false, "argmin"),
                ("gpu", "argmax") => return self.eval_gpu_arg(args, span, true, "argmax"),
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
                                    Value::Float(f) => Value::Int((f as i64).into()),
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
                    | "isize"
                    | "f32"
                    | "f64"
                    | "bool"
                    | "char"
                    | "String"
                    // B-2026-07-22-11: total-order float wrappers — method
                    // syntax `a.gt(b)` sibling of the lowered `F32.gt` path.
                    | "F32"
                    | "F64"
                    | "F16"
                    | "Bf16"
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
                        "i8" | "i16"
                            | "i32"
                            | "i64"
                            | "u8"
                            | "u16"
                            | "u32"
                            | "u64"
                            | "usize"
                            | "isize"
                    )
                {
                    if let Some(arg) = args.first() {
                        let s_val = self.eval_expr_inner(&arg.value);
                        if let Value::String(s) = s_val {
                            return match s.trim().parse::<i64>() {
                                Ok(n) => Value::EnumVariant {
                                    enum_name: "Option".to_string(),
                                    variant: "Some".to_string(),
                                    data: EnumData::Tuple(vec![Value::Int(n.into())]),
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
                        "i8" | "i16"
                            | "i32"
                            | "i64"
                            | "u8"
                            | "u16"
                            | "u32"
                            | "u64"
                            | "usize"
                            | "isize"
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
                                        data: EnumData::Tuple(vec![Value::Int(n.into())]),
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
                            cp_opt = Some(narrow_to_i64(cp));
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
                            data: EnumData::Tuple(vec![Value::Int(cp.into())]),
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
                    return numeric_try_from_value(narrow_to_i64(n), target);
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

        // B-2026-08-21-53 — TYPE-QUALIFIED ASSOCIATED CALL: `Type[Args].fn(a)`.
        //
        // This is the spelling design.md § Generics settles on for explicit
        // type selection (`[T]` constructs and declares types; it is never
        // applied at a call site), so it has to evaluate. It did not: the
        // typechecker accepted it via `try_path_receiver_method`, then the
        // receiver `Box[i64]` was evaluated as an ordinary expression, fell
        // through `eval_expr`'s `Path` arm, and produced
        // "internal: path 'Box' has no interpreter evaluation rule".
        //
        // A type is not a value, so — exactly like the effect-resource arm
        // just above — the receiver must NOT be evaluated. The call is
        // re-formed as the two-segment callee `Path([Type, fn])` that the
        // UNQUALIFIED spelling `Box.make(7)` already parses to, and handed to
        // `eval_call`. Delegating rather than re-implementing is the point:
        // enum-variant constructors, primitive `T.default()`, baked-stdlib
        // arms and user impls all keep their existing single dispatch, and a
        // qualified call can never drift from its unqualified twin.
        //
        // The type ARGUMENTS are dropped, which is correct here and only
        // here: the tree-walk interpreter carries no monomorphization: values
        // are dynamically typed, and the typechecker has already solved and
        // checked the instantiation this call site names. They still matter
        // to codegen, which mangles them into the symbol.
        //
        // Guarded on the head naming a known type AND no binding shadowing it,
        // the same `env.get(..).is_none()` rule the resource-alias arm above
        // applies — a local `let Box = ...` must keep winning.
        if let ExprKind::Path {
            segments,
            generic_args: Some(_),
        } = &object.kind
        {
            if segments.len() == 1
                && self.is_known_type_name(&segments[0])
                && self.env.get(&segments[0]).is_none()
            {
                let callee = Expr {
                    kind: ExprKind::Path {
                        segments: vec![segments[0].clone(), method.to_string()],
                        generic_args: None,
                    },
                    span: object.span,
                };
                return self.eval_call(&callee, args, span);
            }
        }

        let obj = self.eval_expr_inner(object);

        // B-2026-08-09-18 — a faulted RECEIVER (index OOB, unwrap of `None`,
        // div-by-zero, …) sets `pending_cf` and yields a `Unit` poison value;
        // propagate it instead of dispatching a method on the poison. Without
        // this guard the receiver-variant assertions further down turn a clean
        // runtime error into an ICE: `v[3].len()` on an empty `Vec[Vec[i64]]`
        // reached `try_eval_seq_method`'s `len` arm with `Value::Unit` and
        // panicked with `internal error: entered unreachable code`, blaming
        // either the typechecker or a wrong-variant codepath when in fact both
        // were right and the receiver had simply already failed.
        //
        // The guard belongs HERE, at the single receiver-eval site, not in the
        // arms: the assertions are per-method (`len`, `chars`, … each have
        // their own), the ones that instead fall through to a tolerant default
        // reported the fault correctly all along, and which arm you land in is
        // not what makes the program wrong. One check covers every method on
        // every builtin receiver, and keeps the arms' assertions meaning what
        // they say — that a NON-faulted receiver of the wrong variant is a real
        // compiler bug.
        //
        // Same treatment Binary and Unary operands get in the lowered-operator
        // path (B-2026-07-15-7) and `match` scrutinees get before `eval_match`
        // (B-2026-06-19-13's bonus bug); the method-call receiver was the
        // remaining unguarded position.
        if self.pending_cf.is_some() {
            return obj;
        }

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
            // B-2026-08-14-9 — the rest of the in-place mutators. `swap` above
            // was the only one behind this fence, so `fill` / `reverse` /
            // `sort` / `sort_by` / `sort_by_key` fell through to the snapshot
            // below, ran against the COPY, and were discarded: a `mut Slice`
            // receiver silently did nothing where the identical `Vec` receiver
            // mutated. That is the exact failure mode the `split_at_mut`
            // exemption note calls invisible, and these four were living it.
            //
            // SNAPSHOT-RUN-WRITE-BACK rather than four hand-written loops.
            // Every one of these is an in-place permutation or overwrite that
            // cannot change the LENGTH, so running the existing sequence
            // implementation over a detached copy and copying the result back
            // into the window is equivalent — and it inherits the comparator
            // machinery `sort` / `sort_by` / `sort_by_key` need (unsigned
            // ordering, key closures, user `Ord`) instead of reimplementing it
            // against a `Value` window.
            //
            // The copy also keeps the closure safe: a `sort_by_key` body that
            // reads the same collection runs while no lock on `storage` is
            // held, where a loop mutating under a write guard would deadlock
            // against its own receiver.
            if matches!(
                method,
                "fill" | "reverse" | "sort" | "sort_by" | "sort_by_key"
            ) {
                let (storage, start, len) = (storage.clone(), *start, *len);
                let label = match &object.kind {
                    ExprKind::Identifier(n) => n.clone(),
                    _ => "<value>".to_string(),
                };
                let window = { storage.read().unwrap()[start..start + len].to_vec() };
                let scratch = Value::array_of(window);
                let Value::Array(ref scratch_rc) = scratch else {
                    unreachable!("array_of returns Value::Array")
                };
                let scratch_rc = scratch_rc.clone();
                let out = self.try_eval_seq_method(
                    method,
                    object,
                    scratch.clone(),
                    args,
                    span,
                    args_close_span,
                );
                if self.pending_cf.is_some() {
                    return Value::Unit;
                }
                let result = { scratch_rc.read().unwrap().clone() };
                // Length is an invariant of all five; a mismatch would mean the
                // sequence arm did something other than permute in place, and
                // writing it back would corrupt the window's neighbours.
                if result.len() == len {
                    let mut guard = try_write_or_panic(&storage, &label);
                    guard[start..start + len].clone_from_slice(&result);
                }
                return out.unwrap_or(Value::Unit);
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
        //
        // B-2026-08-10-4 — `split_at_mut` is EXEMPTED here alongside
        // `as_slice`/`as_slice_mut` rather than moved above the fence, because
        // it does not mutate at this point: it RETURNS two views, and the seq
        // arm already builds them from a `Value::Slice` receiver's own
        // `storage`/`start`. Normalizing first would hand that arm a fresh
        // snapshot `Array`, so both halves would window a DETACHED copy and
        // every write through them would be silently lost.
        //
        // That failure is worth naming because it is invisible: measured
        // before this exemption, the identical program propagated writes with
        // a `Vec` receiver and dropped them with a `mut Slice` receiver — the
        // returned lengths were correct either way, so only a write-then-read
        // through the ORIGINAL collection distinguishes them. The test pins
        // both receivers for exactly that reason.
        //
        // B-2026-08-13-7 — a USER trait impl on `Slice[T]` is the third
        // exemption, and it is the one that made this snapshot a run-vs-build
        // divergence rather than an implementation detail. The snapshot renames
        // the receiver `Vec` (`value_type_name` reads the resulting
        // `Value::Array`), so `try_eval_impl_method` builds the key
        // `Vec.<method>` and never finds the impl registered under
        // `Slice.<method>` — `--interp` reported `no method` on a program both
        // compiled backends ran. Keeping the `Value::Slice` here is safe
        // precisely because the name is one no builtin arm answers, so the only
        // dispatch left below IS the impl-table path.
        //
        // BUILTIN NAMES KEEP PRECEDENCE, via the same list the typechecker's
        // call-site gate uses — one list rather than two, so the two surfaces
        // cannot drift into disagreeing about which impl wins.
        let slice_routes_to_user_impl = !crate::typechecker::SLICE_BUILTIN_METHODS
            .contains(&method)
            && self.env.get(&format!("Slice.{method}")).is_some();
        // B-2026-08-14-20 — `Slice[T].to_vec() -> Vec[T]`. The detached
        // snapshot below IS the answer, so this could ride the normal
        // dispatch — but it is answered here, on the `Value::Slice` itself,
        // so it stays SLICE-ONLY. Past the snapshot a slice receiver is
        // indistinguishable from a `Vec` one, and the typechecker admits
        // `to_vec` on neither `Vec` nor `Array`; sharing an arm would hand a
        // `Vec` receiver back its OWN storage `Arc` (an alias, not a copy)
        // under `karac run --interp`, which executes past type errors.
        if method == "to_vec"
            && !slice_routes_to_user_impl
            && matches!(obj, Value::Slice { .. } | Value::Array(_))
        {
            // `deep_clone_value` IS `to_vec`: its `Slice` arm already
            // "produces an independent owned snapshot — the original window's
            // storage is left alone", which is the method's whole contract.
            //
            // Both receiver shapes are accepted because a `Slice[T]` SLOT is
            // type-erased here: a `Vec` coerced at a call boundary
            // (`fn f(xs: Slice[i64])` called with a `Vec`) and a
            // `chunks`/`windows` element both arrive as `Value::Array`, so
            // demanding `Value::Slice` would answer `no method 'to_vec' on
            // type 'Vec'` for two shapes the typechecker accepts.
            //
            // DEEP, not `.clone()`: `Value::Array` is an `Arc`-shared cell, so
            // a shallow copy of a `Slice[Vec[i64]]` leaves both containers
            // pointing at the same rows — `copy[0][0] = 99` was then visible
            // through the source, while codegen (which allocates and
            // per-element clones) printed the source unchanged. `shared`
            // elements keep their identity either way; `deep_clone_value`
            // Arc-bumps those, matching what codegen's per-element clone
            // helper does for a `shared` element.
            return crate::interpreter::exec::deep_clone_value(&obj);
        }
        let obj = match obj {
            Value::Slice {
                storage,
                start,
                len,
                ..
            } if !slice_routes_to_user_impl
                && !matches!(method, "as_slice" | "as_slice_mut" | "split_at_mut") =>
            {
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
                // B-2026-08-11-21 leg 2: `args_close_span` FIRST. `object.span`
                // is aliased by this very call — the parser gives a MethodCall
                // its receiver's span — so `expr_types` there holds the call's
                // `Str` result, and a u64 receiver read as signed. The
                // typechecker stashes the receiver type at the closing paren for
                // exactly this reason (the same hatch `pow` uses); the
                // receiver-span probe stays as the fallback for the shapes that
                // are not aliased.
                // The WIDTH matters, not just the fact of unsignedness: a
                // `u128` past `i128::MAX` rides as a negative carrier value
                // exactly as a `u64` past `i64::MAX` does, but reading it back
                // at 64 bits keeps only the low half. `u128::MAX` printed `-1`
                // before this looked at the width (B-2026-08-19-23).
                match self
                    .span_unsigned_int_width(args_close_span)
                    .or_else(|| self.span_unsigned_int_width(&object.span))
                {
                    Some(64) => return Value::String(format!("{}", *n as u64)),
                    Some(128) => return Value::String(format!("{}", *n as u128)),
                    _ => {}
                }
            }
            // All other Display-able values: render via the user-facing
            // renderer (declaration-order struct fields, recursing into
            // containers) so `.to_string()` matches `println` and codegen.
            //
            // The receiver's static type goes with it (B-2026-08-19-27) so a
            // nested unsigned integer is read at its own width, the same way
            // the scalar arm above reads one. `args_close_span` FIRST for the
            // reason given there: the parser aliases a `MethodCall`'s span to
            // its receiver's, so `expr_types` at `object.span` holds this
            // call's `Str` result rather than the receiver's type.
            let recv_ty = self
                .span_expr_type(args_close_span)
                .or_else(|| self.span_expr_type(&object.span));
            return Value::String(self.display_render_typed(&obj, recv_ty.as_ref()));
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
        // LazyExpr comparison / boolean builders (slice 2) — a
        // non-LazyExpr receiver falls through.
        if let Some(v) = self.try_eval_lazyexpr_method(method, &obj, args, span) {
            return v;
        }
        // LazyGroupBy.agg (slice 4) — a non-LazyGroupBy receiver falls
        // through.
        if let Some(v) = self.try_eval_lazygroupby_method(method, &obj, args, span) {
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
        //
        // B-2026-08-27-41 adds the TUPLE receiver, which `value_compare`
        // already orders lexicographically (left to right, then by length) —
        // the same order the `<`/`>` operators on a tuple use.
        //
        // Gated on `value_is_totally_ordered` so this arm claims EXACTLY the
        // set the tuple operators claim. `value_compare` would happily answer
        // for a tuple carrying a bare `Float` or a struct, but the operators
        // decline both (a bare float has no total order; a struct leaf is out
        // of scope for the structural comparator) and so does codegen's
        // `te_is_totally_ordered`. Answering here for a shape the compiled
        // backends refuse would manufacture a run-vs-build split out of a fix
        // whose whole point is closing one.
        if method == "cmp"
            && args.len() == 1
            && match &obj {
                Value::Struct { .. } | Value::SharedStruct(_) | Value::EnumVariant { .. } => true,
                Value::Tuple(_) => Self::value_is_totally_ordered(&obj),
                _ => false,
            }
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
                | Value::TotalFloat16(_)
                | Value::TotalBFloat16(_)
        ) {
            if method == "cmp" && args.len() == 1 {
                let other = self.eval_expr_inner(&args[0].value);
                // `.cmp` has to recover operand signedness from the receiver
                // span exactly as the `.lt` / `.gt` arm below does — the same
                // B-2026-07-04-8 unsigned-64-bit model. It did not, so a `u64`
                // / `usize` / `u128` value riding as a negative two's-complement
                // carrier compared as signed: `u64.MAX.cmp(1u64)` answered
                // `Less` (B-2026-08-28-5).
                //
                // This half was INVISIBLE to the usual run-vs-build
                // differential, because codegen's `.cmp` was signed too and
                // the two backends agreed on the wrong answer. It surfaced
                // only when the codegen side was fixed.
                //
                // Routing through `eval_binary` rather than reimplementing the
                // comparison is the point: `.cmp` and `<` now share one
                // implementation and cannot drift apart again. Gated on a hint
                // being present, so every other operand shape (String, floats,
                // signed ints, bool) keeps `value_compare` unchanged.
                let unsigned_hint = self
                    .span_unsigned_int_width(&object.span)
                    .or_else(|| self.span_unsigned_int_width(&args[0].value.span));
                let ord = match unsigned_hint {
                    Some(_) => {
                        let is_lt = self.eval_binary(
                            &BinOp::Lt,
                            obj.clone(),
                            other.clone(),
                            span,
                            unsigned_hint,
                        );
                        let is_eq = self.eval_binary(
                            &BinOp::Eq,
                            obj.clone(),
                            other.clone(),
                            span,
                            unsigned_hint,
                        );
                        match (is_lt, is_eq) {
                            (Value::Bool(true), _) => std::cmp::Ordering::Less,
                            (_, Value::Bool(true)) => std::cmp::Ordering::Equal,
                            (Value::Bool(false), Value::Bool(false)) => std::cmp::Ordering::Greater,
                            // Neither comparison produced a bool — not a shape
                            // the hint applies to; fall back rather than guess.
                            _ => value_compare(&obj, &other),
                        }
                    }
                    None => value_compare(&obj, &other),
                };
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
                    let unsigned_hint = self
                        .span_unsigned_int_width(&object.span)
                        .or_else(|| self.span_unsigned_int_width(&args[0].value.span));
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
                    // `narrow_oob` as well as `checked_abs`: the carrier is i128
                    // now, so `(i64::MIN).abs()` no longer overflows it and the
                    // declared width is the only thing that still says this
                    // traps (B-2026-08-19-8 stage 1).
                    return match n.checked_abs() {
                        Some(a) if !self.narrow_oob(a, span) => Value::Int(a),
                        _ => self.record_runtime_error("integer overflow".to_string(), span),
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
                // Round to the receiver's declared width, same as the binop
                // path: codegen calls `sqrtf` for an `f32` receiver, and an
                // f64 result left unrounded is not even representable in the
                // f32 slot it lands in (B-2026-08-14-7). `span` aliases the
                // receiver span and holds the call's RESULT type, which for
                // these `-> Self` methods is the receiver type.
                return self.round_float_to_span_width(Value::Float(f.sqrt()), span);
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
                        // Narrow-width round, as at `sqrt` above. For the
                        // transcendentals this is a nearest-f32 of the f64
                        // result rather than a claim of bit-identity with
                        // libm's `sinf`/`expf`/`logf` — see B-2026-08-14-7.
                        return self.round_float_to_span_width(Value::Float(r), span);
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
                            return self.round_float_to_span_width(Value::Float(r), span);
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
                (Value::Float(f), "to_bits") => return Value::Int((f.to_bits() as i64).into()),
                (Value::Float(f), "to_bits32") => {
                    return Value::Int(((*f as f32).to_bits() as i64).into())
                }
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
                    // Wrap at the RECEIVER's width, not at i64 (B-2026-08-19-1).
                    // The interpreter is i64-backed, so `(2147483647i32)
                    // .wrapping_add(1)` would otherwise yield 2147483648 —
                    // no wrap at all — and disagree with codegen, where `i32`
                    // is a real LLVM `i32`. Width comes from the argument span
                    // via the same `expr_types` lookup the `checked_*` /
                    // `saturating_*` / `overflowing_*` families use; the
                    // typechecker pins the argument to the receiver's type.
                    let w = self.overflow_arg_width(&args[0].value);
                    return Value::Int(eval_wrapping_arith(method, a, b, w));
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
                    // `rem_euclid` of MIN by -1 is 0 — fits every width, so
                    // the result range check below cannot see the overflow.
                    if self.div_overflows_at_width(a, b, &object.span) {
                        return self.record_integer_overflow(span);
                    }
                    let r = if method == "div_euclid" {
                        a.checked_div_euclid(b)
                    } else {
                        a.checked_rem_euclid(b)
                    };
                    // Same carrier-widening caveat as `abs` above: MIN/-1 only
                    // overflows the DECLARED width now, not the i128 carrier.
                    // Width comes from the RECEIVER's span, the same source the
                    // `checked_*` family above uses — the call span's recorded
                    // type is the method's result, which is not what bounds the
                    // receiver's width.
                    return match r {
                        Some(v) if !self.narrow_oob(v, &object.span) => Value::Int(v),
                        _ => self.record_integer_overflow(span),
                    };
                }
            }
        }

        // `<c-like enum>.discriminant() -> D` (typed in expr_method_call.rs)
        // — design.md § Enum Discriminant Runtime Surface (B-2026-08-21-10).
        // The values come from the typechecker's folded table, so a declared
        // `Audio = BASE + 1` reads the same here as in codegen. A variant
        // missing from the table cannot happen for a receiver the typechecker
        // admitted, so falling through is a defensive no-op rather than a path.
        if args.is_empty() && method == "discriminant" {
            if let Value::EnumVariant {
                enum_name, variant, ..
            } = &obj
            {
                if let Some(disc) = self.typecheck_result.enum_discriminants.get(enum_name) {
                    if let Some((_, v)) = disc.values.iter().find(|(n, _)| n == variant) {
                        return Value::Int(i128::from(*v));
                    }
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
                return Value::Int(i128::from(eval_bit_intrinsic(method, *n, w)));
            }
        }

        // `to_ne_bytes()` on integer scalars -> `Array[u8, N]` (typed in
        // method_numeric.rs). The bytes are the value's NATIVE-order memory
        // image, so the width recovered from `args_close_span` selects both
        // how many bytes there are and which of them are significant: the
        // i64-backed model sign-extends a narrow `iN`, and taking the low N
        // bytes of that image is exactly the same reinterpretation codegen's
        // store-and-reload performs (B-2026-08-21-10).
        if args.is_empty() && method == "to_ne_bytes" {
            if let Value::Int(n) = &obj {
                let (bits, _signed) = match self.int_width_at(args_close_span) {
                    IntW::S(b) => (b, true),
                    IntW::U(b) => (b, false),
                };
                let nbytes = (bits as usize) / 8;
                let image = (*n as u128).to_ne_bytes();
                // `u128::to_ne_bytes` is the full 16-byte image in native
                // order; the receiver's own bytes are the first N of it on a
                // little-endian target and the last N on a big-endian one.
                let bytes: Vec<Value> = if cfg!(target_endian = "little") {
                    image[..nbytes]
                        .iter()
                        .map(|b| Value::Int(i128::from(*b)))
                        .collect()
                } else {
                    image[16 - nbytes..]
                        .iter()
                        .map(|b| Value::Int(i128::from(*b)))
                        .collect()
                };
                return Value::array_of(bytes);
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
                return Value::Int((result as i64).into());
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
                    let av: i128 = if signed { a } else { (a as u64) as i128 };
                    let bv: i128 = if signed { b } else { (b as u64) as i128 };
                    let diff: u128 = (av - bv).unsigned_abs();
                    let masked: u64 = if bits >= 64 {
                        diff as u64
                    } else {
                        (diff as u64) & ((1u64 << bits) - 1)
                    };
                    return Value::Int((masked as i64).into());
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
        // `char.is_digit(radix) -> bool` (B-2026-08-12-25) shares this arm: same
        // receiver, same radix trap, and `is_some()` of the same lookup — see
        // the typechecker, where the two also share one arm.
        if matches!(method, "to_digit" | "is_digit") && args.len() == 1 {
            if let Value::Char(c) = &obj {
                let c = *c;
                if let Value::Int(radix) = self.eval_expr_inner(&args[0].value) {
                    if !(2..=36).contains(&radix) {
                        return self.record_runtime_error(
                            format!("{method}: radix must be in 2..=36, got {radix}"),
                            span,
                        );
                    }
                    let digit = c.to_digit(radix as u32);
                    if method == "is_digit" {
                        return Value::Bool(digit.is_some());
                    }
                    return match digit {
                        Some(d) => some_int(i128::from(d)),
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

        // Unicode case folding on a `char` (B-2026-08-12-25): `to_lowercase` /
        // `to_uppercase` → char. Rust's mapping is an ITERATOR because a scalar
        // can fold to several (`ß` → `SS`); a `char → char` signature takes the
        // mapping only when it yields exactly one scalar and returns `self`
        // unchanged when it expands. Codegen computes the identical collapse in
        // `karac_runtime_char_to_*case`, so the backends cannot diverge. A
        // String receiver never reaches here — it is a different `Value` and is
        // handled by the full-Unicode String→String arm in method_call_seq.rs.
        if args.is_empty() {
            if let Value::Char(c) = &obj {
                let folded = match method {
                    "to_lowercase" => Some(single_scalar_case_map(c.to_lowercase())),
                    "to_uppercase" => Some(single_scalar_case_map(c.to_uppercase())),
                    _ => None,
                };
                if let Some(folded) = folded {
                    return Value::Char(folded.unwrap_or(*c));
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
                            data: EnumData::Tuple(vec![Value::Int((v as i64).into())]),
                        },
                        (FloatToIntFamily::Checked, ConvOutcome::None) => make_none(),
                        (_, ConvOutcome::Value(v)) => Value::Int((v as i64).into()),
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
            // 128-bit (B-2026-08-19-8 stage 3b). Without these arms `i128` fell
            // to the signed-64 default below, which is why every width-sensitive
            // method answered for 64 bits in the interpreter while codegen
            // (stage 3a) answered for 128 — a run-vs-build divergence waiting
            // for the type to become nameable.
            Some(Type::Int(IntSize::I128)) => IntW::S(128),
            Some(Type::UInt(UIntSize::U128)) => IntW::U(128),
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
    fn eval_int_pow(&mut self, base: i128, exp: u64, w: IntW, span: &Span) -> Value {
        let (signed, bits) = match w {
            IntW::S(b) => (true, b),
            IntW::U(b) => (false, b),
        };
        let (lo, hi) = width_bounds(signed, bits);
        let base128: i128 = if signed || bits == 128 {
            base
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
        Value::Int(acc)
    }
}

/// Evaluate a width-correct bit intrinsic (`count_ones` / `leading_zeros` /
/// `trailing_zeros`) on the i64-backed value `n` at receiver width `w`. Signed
/// `iN` values are sign-extended in the model, so the value is masked to the
/// width's low bits before counting; `leading/trailing_zeros` count within the
/// width (`bits` on a zero input).
fn eval_bit_intrinsic(method: &str, n: i128, w: IntW) -> u32 {
    let bits = match w {
        IntW::S(b) | IntW::U(b) => b,
    };
    // `bits >= 64` used to mean "the carrier IS the width, take it whole". With
    // the i128 carrier (B-2026-08-19-8) that is only true at 128, so 64 masks
    // like any other narrow width — otherwise a 64-bit receiver would count the
    // carrier's sign extension. `1u128 << 128` would overflow, hence the arm.
    let masked: u128 = match bits {
        128 => n as u128,
        64 => (n as u64) as u128,
        b => (n as u128) & ((1u128 << b) - 1),
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
fn eval_bit_permute(method: &str, n: i128, w: IntW) -> i128 {
    let (bits, signed) = match w {
        IntW::S(b) => (b, true),
        IntW::U(b) => (b, false),
    };
    // Widened from u64 to u128 (B-2026-08-19-8 stage 3b): the permutation has
    // to happen in a word at least as wide as the receiver, and `bits` can now
    // be 128. `1u128 << 128` would overflow, hence the explicit arm.
    let masked: u128 = match bits {
        128 => n as u128,
        64 => (n as u64) as u128,
        b => (n as u128) & ((1u128 << b) - 1),
    };
    let permuted: u128 = match method {
        // Reverse all 128 bits, then shift the meaningful `bits` back down.
        "reverse_bits" => {
            if bits >= 128 {
                masked.reverse_bits()
            } else {
                masked.reverse_bits() >> (128 - bits)
            }
        }
        "swap_bytes" => match bits {
            16 => u128::from((masked as u16).swap_bytes()),
            32 => u128::from((masked as u32).swap_bytes()),
            64 => u128::from((masked as u64).swap_bytes()),
            128 => masked.swap_bytes(),
            // 8-bit (and any non-multiple-of-16 width) → identity.
            _ => masked,
        },
        _ => unreachable!("non-permute method routed to eval_bit_permute: {method}"),
    };
    // Re-encode into the carrier: sign-extend a signed result whose width-top
    // bit is set, so it round-trips like the other values of that width.
    if signed && bits < 128 && (permuted & (1u128 << (bits - 1))) != 0 {
        (permuted | !((1u128 << bits) - 1)) as i128
    } else {
        permuted as i128
    }
}

/// Evaluate a width-correct bit rotation (`rotate_left` / `rotate_right`) on the
/// i64-backed value `n` at receiver width `w`, rotating by `amount` within the
/// receiver's `bits` (Rust `iN::rotate_{left,right}`, amount mod width). The
/// result is re-encoded like [`eval_bit_permute`] (sign-extended for a signed
/// narrow width). Rotation is bit-level, so signedness only affects the final
/// encoding, not the rotated bits.
fn eval_bit_rotate(method: &str, n: i128, amount: u32, w: IntW) -> i128 {
    let (bits, signed) = match w {
        IntW::S(b) => (b, true),
        IntW::U(b) => (b, false),
    };
    // u128 for the same reason as `eval_bit_permute` (B-2026-08-19-8 stage 3b):
    // the rotation word must be at least as wide as the receiver.
    let masked: u128 = match bits {
        128 => n as u128,
        64 => (n as u64) as u128,
        b => (n as u128) & ((1u128 << b) - 1),
    };
    let left = method == "rotate_left";
    let rotated: u128 = match bits {
        8 => {
            let v = masked as u8;
            u128::from(if left {
                v.rotate_left(amount)
            } else {
                v.rotate_right(amount)
            })
        }
        16 => {
            let v = masked as u16;
            u128::from(if left {
                v.rotate_left(amount)
            } else {
                v.rotate_right(amount)
            })
        }
        32 => {
            let v = masked as u32;
            u128::from(if left {
                v.rotate_left(amount)
            } else {
                v.rotate_right(amount)
            })
        }
        64 => {
            let v = masked as u64;
            u128::from(if left {
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
    if signed && bits < 128 && (rotated & (1u128 << (bits - 1))) != 0 {
        (rotated | !((1u128 << bits) - 1)) as i128
    } else {
        rotated as i128
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

fn some_int(v: i128) -> Value {
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

/// Two's-complement wraparound for `wrapping_{add,sub,mul}` at the receiver's
/// width (B-2026-08-19-1).
///
/// Sibling of [`eval_overflow_arith`]: same i64-backed operands and the same
/// width recovery, but this family never reports overflow — it defines it. The
/// arithmetic is done in `i128` (exact for every width this reaches, since
/// `i128`/`u128` receivers are rejected in the typechecker) and then reduced
/// into the width: mask to `bits`, and for a SIGNED width sign-extend the
/// result so the interpreter's `Value::Int(i64)` carries the same bit pattern
/// codegen's narrower LLVM integer would.
fn eval_wrapping_arith(method: &str, a: i128, b: i128, w: IntW) -> i128 {
    let (signed, bits) = match w {
        IntW::S(b) => (true, b),
        IntW::U(b) => (false, b),
    };
    // 128-bit: the carrier IS the width, so the plain i128 op is exact and the
    // mask below would shift by 128 (UB). Signed and unsigned wrap identically
    // on the two's-complement bit pattern at a given width, so one arm serves
    // both — the same argument the 64-bit case has always made
    // (B-2026-08-19-8 stage 3b).
    if bits == 128 {
        return match method {
            "wrapping_add" => a.wrapping_add(b),
            "wrapping_sub" => a.wrapping_sub(b),
            "wrapping_mul" => a.wrapping_mul(b),
            _ => unreachable!("caller matched the three wrapping methods"),
        };
    }
    // 64-bit: wrap on the i64 bit pattern, then widen back into the carrier.
    if bits == 64 {
        let (x, y) = (a as i64, b as i64);
        return i128::from(match method {
            "wrapping_add" => x.wrapping_add(y),
            "wrapping_sub" => x.wrapping_sub(y),
            "wrapping_mul" => x.wrapping_mul(y),
            _ => unreachable!("caller matched the three wrapping methods"),
        });
    }
    let (x, y) = (a, b);
    let raw = match method {
        "wrapping_add" => x.wrapping_add(y),
        "wrapping_sub" => x.wrapping_sub(y),
        "wrapping_mul" => x.wrapping_mul(y),
        _ => unreachable!("caller matched the three wrapping methods"),
    };
    let masked = raw & ((1i128 << bits) - 1);
    if signed && (masked >> (bits - 1)) & 1 == 1 {
        masked - (1i128 << bits) // set sign bit → negative in the width
    } else {
        masked
    }
}

/// Inclusive `(min, max)` of a width, in i128.
///
/// 128-bit needs its own arm because the shift form below (`1i128 << bits` /
/// `1i128 << (bits - 1)`) overflows at that width (B-2026-08-19-8 stage 3b).
/// `u128`'s ceiling is `i128::MAX`, not `u128::MAX`: the interpreter's carrier
/// is a SIGNED i128, so the top half of u128 has no representation there yet —
/// the same limit `narrow_oob` records, and a constraint stage 5 must lift
/// before `u128` is spellable.
fn width_bounds(signed: bool, bits: u32) -> (i128, i128) {
    match (signed, bits) {
        (true, 128) => (i128::MIN, i128::MAX),
        (false, 128) => (0, i128::MAX),
        (true, b) => (-(1i128 << (b - 1)), (1i128 << (b - 1)) - 1),
        (false, b) => (0, (1i128 << b) - 1),
    }
}

/// Evaluate one overflow-aware integer operation at the receiver's width.
/// Operands arrive as the interpreter's i64-backed `Value::Int`; signed widths
/// and 64-bit unsigned compute exactly (i128 / u64), narrow unsigned widths use
/// their `[0, 2^bits)` bounds.
fn eval_overflow_arith(fam: OvFam, op: OvOp, a: i128, b: i128, w: IntW) -> Value {
    // 128-bit signed: the carrier IS the width, so i128's own overflowing ops
    // are exact and the `1i128 << bits` bounds below would overflow
    // (B-2026-08-19-8 stage 3b). Mirrors the 64-bit-unsigned branch's shape.
    if let IntW::S(128) = w {
        let (res, of) = match op {
            OvOp::Add => a.overflowing_add(b),
            OvOp::Sub => a.overflowing_sub(b),
            OvOp::Mul => a.overflowing_mul(b),
        };
        return match fam {
            OvFam::Checked => {
                if of {
                    none_value()
                } else {
                    some_int(res)
                }
            }
            // Saturate toward the sign of the TRUE result, not by operation.
            // On overflow `add`/`sub` force the sign of `a` (add overflows only
            // with like-signed operands; sub only with unlike-signed ones), and
            // `mul`'s sign is `sign(a) XOR sign(b)`. The old by-operation rule
            // sent every overflowing `mul` to `MAX`, so
            // `(-1.2e30).saturating_mul(1e12)` — a large NEGATIVE product —
            // clamped to `i128::MAX` (B-2026-08-19-19). This is the same rule
            // codegen's `saturating_` arm already documents and applies, and
            // the narrow-width path below gets it for free by clamping an exact
            // i128 result.
            OvFam::Saturating => Value::Int(if of {
                let negative = match op {
                    OvOp::Add | OvOp::Sub => a < 0,
                    OvOp::Mul => (a < 0) != (b < 0),
                };
                if negative {
                    i128::MIN
                } else {
                    i128::MAX
                }
            } else {
                res
            }),
            OvFam::Overflowing => Value::Tuple(vec![Value::Int(res), Value::Bool(of)]),
        };
    }
    // 128-bit unsigned: reinterpret the carrier's bits as `u128` and use
    // `u128`'s own overflowing ops — the same trick the 64-bit-unsigned branch
    // below plays with `i64`/`u64`, and for the same reason (the carrier is
    // signed, the width is not). Without it these fell through to the generic
    // tail, whose `(a as u64) as i128` TRUNCATES an unsigned operand to 64
    // bits: `(2^100 as u128).checked_mul(2)` answered `0` in the interpreter
    // while codegen answered 2^101 — a run-vs-build divergence, reachable as
    // soon as `checked_*` on 128-bit was unblocked (B-2026-08-19-19).
    if let IntW::U(128) = w {
        let (au, bu) = (a as u128, b as u128);
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
                    some_int(res as i128)
                }
            }
            OvFam::Saturating => {
                let s = if of {
                    match op {
                        OvOp::Sub => 0u128, // underflow → 0
                        _ => u128::MAX,     // add/mul overflow → MAX
                    }
                } else {
                    res
                };
                Value::Int(s as i128)
            }
            OvFam::Overflowing => Value::Tuple(vec![Value::Int(res as i128), Value::Bool(of)]),
        };
    }
    // 64-bit unsigned: reinterpret the two's-complement bits as u64 (full range).
    if let IntW::U(64) = w {
        let (a, b) = (a as i64, b as i64);
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
                    some_int(i128::from(res))
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
                Value::Int((s as i64).into())
            }
            OvFam::Overflowing => {
                Value::Tuple(vec![Value::Int((res as i64).into()), Value::Bool(of)])
            }
        };
    }

    // Signed widths (8/16/32/64) and narrow unsigned (8/16/32): exact in i128.
    let (signed, bits) = match w {
        IntW::S(b) => (true, b),
        IntW::U(b) => (false, b),
    };
    let (lo, hi) = width_bounds(signed, bits);
    // Unsigned narrow values are stored non-negative; signed keep their sign.
    let av = if signed { a } else { (a as u64) as i128 };
    let bv = if signed { b } else { (b as u64) as i128 };
    let r: i128 = match op {
        OvOp::Add => av + bv,
        OvOp::Sub => av - bv,
        OvOp::Mul => av * bv,
    };
    let in_range = r >= lo && r <= hi;
    match fam {
        OvFam::Checked => {
            if in_range {
                some_int(r)
            } else {
                none_value()
            }
        }
        OvFam::Saturating => Value::Int((r.clamp(lo, hi) as i64).into()),
        OvFam::Overflowing => {
            // Wrap into the width's value set, then back to signed range if signed.
            let modulus = 1i128 << bits;
            let mut wrapped = ((r % modulus) + modulus) % modulus;
            if signed && wrapped > hi {
                wrapped -= modulus;
            }
            Value::Tuple(vec![
                Value::Int((wrapped as i64).into()),
                Value::Bool(!in_range),
            ])
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
