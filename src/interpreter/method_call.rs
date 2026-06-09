//! Method-call evaluation: the big `eval_method_call` dispatch on
//! receiver shape (Vec/String/Slice/Map/Set/iterator-adapters/etc.).
//!
//! Lives in a sibling `impl<'a> super::Interpreter<'a>` block.

use crate::ast::*;
use crate::token::Span;

use super::eval_expr::cast_value;
use super::exec::ControlFlow;
use super::helpers::{kara_json_to_serde_json, value_compare};
use super::pascal_to_snake;
use super::value::{try_write_or_panic, EnumData, Value};

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

    pub(crate) fn eval_method_call(
        &mut self,
        object: &Expr,
        method: &str,
        args: &[CallArg],
        span: &Span,
    ) -> Value {
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

        // `#[derive(Display)]` — `to_string()` on a unit enum variant.
        if method == "to_string" {
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
            // All other Display-able values: render via the user-facing
            // renderer (declaration-order struct fields, recursing into
            // containers) so `.to_string()` matches `println` and codegen.
            return Value::String(self.display_render(&obj));
        }

        // Category dispatchers — each returns `Some(Value)` if `method`
        // matches one of its handled names and the receiver shape is
        // compatible; otherwise `None` and we fall through to the next.
        if let Some(v) = self.try_eval_iterator_method(method, object, obj.clone(), args, span) {
            return v;
        }
        if let Some(v) = self.try_eval_http_method(method, obj.clone(), args, span) {
            return v;
        }
        if let Some(v) = self.try_eval_regex_method(method, obj.clone(), args, span) {
            return v;
        }
        if let Some(v) = self.try_eval_process_method(method, obj.clone(), args, span) {
            return v;
        }
        if let Some(v) = self.try_eval_tensor_method(method, &obj, args, span) {
            return v;
        }
        if let Some(v) = self.try_eval_pool_method(method, obj.clone(), args, span) {
            return v;
        }
        if let Some(v) = self.try_eval_semaphore_method(method, obj.clone(), args, span) {
            return v;
        }
        if let Some(v) = self.try_eval_rate_limiter_method(method, obj.clone(), args, span) {
            return v;
        }
        if let Some(v) = self.try_eval_bounded_channel_method(method, obj.clone(), args, span) {
            return v;
        }
        if let Some(v) = self.try_eval_set_method(method, object, obj.clone(), args, span) {
            return v;
        }
        if let Some(v) = self.try_eval_map_method(method, object, obj.clone(), args, span) {
            return v;
        }
        if let Some(v) = self.try_eval_option_result_method(method, object, obj.clone(), args, span)
        {
            return v;
        }
        if let Some(v) = self.try_eval_channel_method(method, obj.clone(), args, span) {
            return v;
        }
        if let Some(v) = self.try_eval_file_method(method, obj.clone(), args, span) {
            return v;
        }
        if let Some(v) = self.try_eval_bufreader_method(method, obj.clone(), args, span) {
            return v;
        }
        if let Some(v) = self.try_eval_bufwriter_method(method, obj.clone(), args, span) {
            return v;
        }
        if let Some(v) = self.try_eval_vector_method(method, object, obj.clone(), args, span) {
            return v;
        }
        if let Some(v) = self.try_eval_seq_method(method, object, obj.clone(), args, span) {
            return v;
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
                    return self.eval_binary(&op, obj.clone(), rhs, span);
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
        let method_key = format!("{}.{}", type_name, method);

        if let Some(func) = self.env.get(&method_key) {
            let mut arg_vals: Vec<Value> = vec![obj];
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

                self.env.pop_scope();
                if let Some(msg) = contract_fault {
                    return self.record_runtime_error(msg, span);
                }
                return match result {
                    Ok(v) => v,
                    Err(ControlFlow::Return(v)) => v,
                    Err(cf) => self.set_cf(cf),
                };
            }
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
}
