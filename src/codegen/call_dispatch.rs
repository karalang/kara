//! Call dispatch: layout intrinsics, the main `compile_call` lowering,
//! and enum-variant value construction.
//!
//! Houses `compile_layout_query_intrinsic` (size_of/align_of/offset_of),
//! `compile_call` (the big free-function / assoc-call / generic-call
//! dispatch entry point), `try_compile_enum_variant` (lowers
//! `Foo.Variant(args)` constructor calls), the cleanup-suppression
//! helpers `suppress_cleanup_for_tail_return` and
//! `suppress_source_vec_cleanup_for_arg`, the payload-coercion
//! helpers `coerce_to_payload_words` / `build_option_some_via_phis`
//! / `coerce_to_i64`, and `try_unit_enum_variant` (lowers bare
//! `EnumName.UnitVariant` identifier references).

use crate::ast::*;
use std::collections::HashMap;

use inkwell::types::{BasicType, BasicTypeEnum};
use inkwell::values::{
    BasicMetadataValueEnum, BasicValueEnum, CallSiteValue, FunctionValue, PointerValue,
};
use inkwell::{AddressSpace, IntPredicate};

use super::declarations::KARAC_PARK_ON_FD;
use super::helpers::{expr_as_type_expr_codegen, match_with_provider_call, match_with_span_call};
use super::state::LayoutId;

impl<'ctx> super::Codegen<'ctx> {
    // ── Call ──────────────────────────────────────────────────────

    /// Lower a `size_of[T]()` / `align_of[T]()` call to the matching
    /// LLVM constant. `size_of` uses inkwell's `BasicTypeEnum::size_of()`
    /// (a constant-expr returning i64). `align_of` uses
    /// `TargetData::get_abi_alignment()` (a `u32` ABI alignment for the
    /// host target) materialized as an i64 constant. Both return `usize`
    /// to match the typechecker's signature, which lowers to i64 on the
    /// 64-bit-only target the rest of codegen assumes.
    pub(super) fn compile_layout_query_intrinsic(
        &mut self,
        name: &str,
        explicit_args: &[GenericArg],
        args: &[CallArg],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        // The typechecker has already validated argument shape; do a
        // defensive check here so a divergent path (e.g., direct codegen
        // invocation in tests) doesn't crash.
        for arg in args {
            self.compile_expr(&arg.value)?;
        }
        let ty_expr = match explicit_args {
            [GenericArg::Type(te)] => te,
            _ => {
                return Ok(self.context.i64_type().const_int(0, false).into());
            }
        };
        let llvm_ty = self.llvm_type_for_type_expr(ty_expr);
        let i64_ty = self.context.i64_type();
        match name {
            "size_of" => {
                let size = llvm_ty
                    .size_of()
                    .ok_or_else(|| "size_of[T]: type is not sized".to_string())?;
                Ok(size.into())
            }
            "align_of" => {
                let target_data = self.ensure_target_data()?;
                let align = target_data.get_abi_alignment(&llvm_ty);
                Ok(i64_ty.const_int(u64::from(align), false).into())
            }
            _ => unreachable!("compile_layout_query_intrinsic dispatched on unknown name"),
        }
    }

    pub(super) fn compile_call(
        &mut self,
        callee: &Expr,
        args: &[CallArg],
        call_span: &crate::token::Span,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        // Direct use of a borrow-returning call result in a *value* position
        // (`println(name_of(s))`, `name_of(s).len()`, an operand). The callee
        // lowers to the `ptr` borrow ABI; emit it once with the bind-directly
        // gate bypassed (`compiling_ref_return_let_rhs`), then load the pointee
        // so the consuming context sees the borrowed value. Sound because the
        // front-end only accepts direct use where a `ref T` is legal — the
        // typechecker rejects moving a borrow into an owned parameter
        // (`expected 'T', found 'ref T'`), so the loaded value is always
        // read-only at the use site (no ownership transfer, no drop
        // obligation). A borrow-call in a *ref-parameter argument* position is
        // intercepted earlier in the arg-passing loop — it needs the ptr
        // passed through directly (materializing a loaded value there would
        // queue a `track_vec_var` free and double-free the source), so it
        // never reaches here. Caller half of B-2026-06-07-5 (Tier-1.5).
        if !self.compiling_ref_return_let_rhs {
            if let ExprKind::Identifier(n) = &callee.kind {
                if let Some(inner_te) = self.fn_ref_return_inner.get(n).cloned() {
                    let inner = self.llvm_type_for_type_expr(&inner_te);
                    self.compiling_ref_return_let_rhs = true;
                    let ptr_res = self.compile_call(callee, args, call_span);
                    self.compiling_ref_return_let_rhs = false;
                    let ptr = ptr_res?.into_pointer_value();
                    return Ok(self
                        .builder
                        .build_load(inner, ptr, "ref.direct.use")
                        .unwrap());
                }
            }
        }

        // Reject an internal Kāra call to a boxed-return export (Slice 4
        // Path B). Its LLVM signature returns a `ptr` (the heap box), not
        // the `{data,len,cap}` value this call site's typecheck expects, so
        // lowering it would read a garbage Vec/String. Such an export is a
        // C-facing surface only.
        if let ExprKind::Identifier(n) = &callee.kind {
            if self.boxed_export_names.contains(n) {
                return Err(format!(
                    "cannot call `{n}` from Kāra code: it is a `pub extern \"C\" fn` whose \
                     aggregate return (`Vec`/`String`) is auto-boxed for the C ABI (returns an \
                     opaque handle to C, not a Kāra value). Move the body into a non-exported \
                     helper and call that from Kāra; keep `{n}` as the thin C-facing export. \
                     See design.md § Exported C ABI (Slice 4 Path B)."
                ));
            }
            // An export with a per-target-coerced `#[repr(C)]` struct
            // param/return takes a register-coerced type / indirect ptr / sret
            // slot this call site doesn't pack. Reject the internal call rather
            // than pass a mismatched arg (the boxed-export pattern — extract a
            // non-exported helper). Covers AAPCS on AArch64 (B-2026-07-09-2),
            // SysV MEMORY class on x86-64 (B-2026-07-09-2 Slice 3c), and the
            // Microsoft x64 aggregate rules on Windows (B-2026-07-09-8).
            if self.abi_adapted_export_names.contains(n) {
                return Err(format!(
                    "cannot call `{n}` from Kāra code: it is a `pub extern \"C\" fn` whose \
                     `#[repr(C)]` struct param/return uses the C-boundary ABI (per-target: \
                     AAPCS on AArch64 — register-coerced ≤ 16 B, indirect ptr > 16 B; SysV on \
                     x86-64 — `byval`/`sret` for > 16 B; Microsoft x64 on Windows — coerced iN \
                     at exactly 1/2/4/8 B, plain-ptr indirect / sret otherwise). Move the body \
                     into a non-exported helper and call that from Kāra; keep `{n}` as the thin \
                     C-facing export. Tracked: B-2026-07-09-2 / B-2026-07-09-8."
                ));
            }
        }

        // Cooperative cancel check before each call inside a par-branch.
        // No-op when not inside a par branch. Narrowed against the
        // `callee_effectful` side-table when the callee name is statically
        // recoverable (free fn or `Type.assoc`); other shapes (closure, FFI
        // through identifier resolved at link time, etc.) fall back to the
        // conservative "always fire" path via `None`.
        let callee_key: Option<String> = match &callee.kind {
            ExprKind::Identifier(n) => Some(n.clone()),
            ExprKind::Path { segments, .. } if segments.len() == 2 => {
                Some(format!("{}.{}", segments[0], segments[1]))
            }
            _ => None,
        };

        // `SortedMap[K, V]` (B-2026-07-09-17) shares `Map`'s `KaracMap` storage
        // and only orders its keys/values/entries/for-loop observation points
        // (via `karac_map_sorted_keys`), the map sibling of `SortedSet`
        // (B-2026-07-09-16). It is registered like `Map` and no longer rejected
        // here; `SortedMap.new` flows through the normal `Map.new` construction.

        self.emit_branch_cancel_check("call", callee_key.as_deref());

        // `old(expr)` inside an `ensures` postcondition reads the pre-state
        // snapshot captured at function entry (design.md § Contracts rule 4),
        // keyed by the arg's span. Falls back to compiling the arg directly
        // when no snapshot is active (defensive — the typechecker restricts
        // `old(...)` to `ensures` clauses).
        if let ExprKind::Identifier(n) = &callee.kind {
            if n == "old" && args.len() == 1 {
                if let Some(v) = self.contract_old_lookup(&args[0].value) {
                    return Ok(v);
                }
                return self.compile_expr(&args[0].value);
            }
        }

        // `Refined.try_from(x)` — emit a runtime predicate check producing a
        // `Result[Refined, String]` (phase-9 step 5c). Parses as a 2-segment
        // Path call (uppercase head roots a Path). The synthetic `try_from`
        // impl the typechecker registers has no AST body, so this intercept
        // is the only place the predicate runs on the codegen path; a
        // non-refinement head returns `None` and falls through.
        if let ExprKind::Path { segments, .. } = &callee.kind {
            if segments.len() == 2 && segments[1] == "try_from" {
                if let Some(arg) = args.first() {
                    if let Some(v) =
                        self.compile_refinement_try_from(&segments[0], &arg.value, call_span)?
                    {
                        return Ok(v);
                    }
                }
            }
        }

        // `ExitCode.from(code)` — the stdlib `from` constructor on the
        // `ExitCode` distinct type (Phase-8 entry-point contract Slice B).
        // Its Kāra body is the zero-cost wrap `{ ExitCode(code) }`, so the
        // codegen lowering is identical to the distinct constructor:
        // compile the argument (an `i32`), emit any refinement assert
        // (none for `ExitCode`), and return it. Gated on `distinct_bases`
        // so it fires only for distinct types — `from` on any other type
        // dispatches normally. Mirrors how the distinct `T(value)`
        // constructor and `try_from` are call-site-lowered rather than
        // compiled from a baked body.
        if let ExprKind::Path { segments, .. } = &callee.kind {
            if segments.len() == 2
                && segments[1] == "from"
                && self.distinct_bases.contains_key(&segments[0])
            {
                if let Some(arg) = args.first() {
                    let value = self.compile_expr(&arg.value)?;
                    let value = self.coerce_to_distinct_base(&segments[0], value);
                    self.emit_refinement_assert(&segments[0], value)?;
                    return Ok(value);
                }
            }
        }

        // Theme 6 sub-step 3: `with_provider[R](provider, ||body)`.
        // Recognize the call shape before the generic dispatch below — the
        // callee is an `Index` expression which would otherwise fall through
        // to the unknown-callee path and return const-0. The lowering pushes
        // a `ProviderFrame` onto the runtime stack, runs the body, pops, and
        // yields the body's value.
        if let Some((resource, provider_expr, closure_expr)) =
            match_with_provider_call(callee, args)
        {
            return self.compile_with_provider(&resource, provider_expr, closure_expr);
        }

        // Phase-8 line 153: `with_span(span, ||body)` installs `span`'s id
        // as the ambient active span for the body's dynamic extent and
        // restores the prior one on exit (mirrors `with_provider`'s
        // push/inline-body/pop shape, but with the per-thread active-span
        // register instead of the provider stack).
        if let Some((span_expr, closure_expr)) = match_with_span_call(callee, args) {
            return self.compile_with_span(span_expr, closure_expr);
        }

        // Phase-8 line 153: `tracing_active_span()` reads the ambient
        // active span id (the `#[compiler_builtin]` `Log.*` / `LogEvent`
        // bodies call it to auto-stamp events). Lower to the runtime getter
        // rather than the placeholder Kāra body (which returns 0).
        let is_tracing_active_span = match &callee.kind {
            ExprKind::Identifier(n) => n == "tracing_active_span",
            ExprKind::Path { segments, .. } => segments.as_slice() == ["tracing_active_span"],
            _ => false,
        };
        if args.is_empty() && is_tracing_active_span {
            let v = self
                .builder
                .build_call(self.karac_tracing_get_active_span_fn, &[], "active_span")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic();
            return Ok(v);
        }

        // Phase-8 line 156 (configurable ambient exporter, codegen half):
        // `Log.set_exporter(e)` (call-site intercept) plus the
        // `tracing_{level_enabled,emit_event,set_min_level,reset}` builtins
        // the rewritten `Log.*` / `Log.set_min_level` / `Log.reset` bodies
        // lower through, so a compiled `Log.*` honors the ambient config.
        if let Some(v) = self.try_compile_tracing_config_builtin(callee, args)? {
            return Ok(v);
        }

        // `Stats.*` free-function statistics over a `Slice[f64]` / `Vec[f64]`
        // (the AOT twin of `eval_stats_fn`). Intercepted before the generic
        // free-function dispatch — the `#[compiler_builtin]` bodies are
        // doc-only placeholders. Returns `None` for any non-`Stats` callee.
        if let Some(v) = self.try_compile_stats_call(callee, args, call_span)? {
            return Ok(v);
        }

        // Const generics slice 1c: `f[8]()` parses as
        // `Call { callee: Index { object: Identifier(name), index: literal }, args }`.
        // The typechecker disambiguation routes through a synthetic
        // Path-with-generic-args callee at type-check time, but the
        // codegen sees the original AST. Apply the same rewrite here
        // when the indexed object resolves to a generic free function
        // in `generic_fns`. (`callbacks[0]()` keeps its Index-then-Call
        // shape because `callbacks` isn't in `generic_fns`.)
        if let ExprKind::Index { object, index } = &callee.kind {
            if let ExprKind::Identifier(name) = &object.kind {
                let is_literal_index = matches!(
                    &index.kind,
                    ExprKind::Integer(_, _)
                        | ExprKind::Bool(_)
                        | ExprKind::CharLit(_)
                        | ExprKind::ByteLit(_)
                );
                if is_literal_index && self.generic_fns.contains_key(name) {
                    let explicit_args = vec![GenericArg::Const((**index).clone())];
                    return self.compile_generic_call(name, args, Some(&explicit_args), call_span);
                }
            }
        }

        // Layout-introspection intrinsics (`size_of[T]()` / `align_of[T]()`)
        // single-arg shape. The parser produces `Call { Index { Ident,
        // T_expr } }` because `lookahead_generic_args_call` requires a
        // top-level comma; recover the type expression from the value-
        // position `Expr` and dispatch the intrinsic. The typechecker
        // handles the matching shape in `infer_call`; this codegen mirror
        // is here so the placeholder body in
        // `runtime/stdlib/intrinsics.kara` is never lowered.
        if let ExprKind::Index { object, index } = &callee.kind {
            if let ExprKind::Identifier(name) = &object.kind {
                if (name == "size_of" || name == "align_of") && args.is_empty() {
                    if let Some(te) = expr_as_type_expr_codegen(index) {
                        let synth = vec![GenericArg::Type(te)];
                        return self.compile_layout_query_intrinsic(name, &synth, args);
                    }
                }
            }
        }

        // Three-segment Json method call: `Json.Variant.stringify()`
        // parses as `Call { callee: Path { segments: [Json, Variant,
        // stringify] }, args: [] }` when the variant is a bare-name
        // unit form (e.g. `Json.Null.stringify()`). The 2-segment
        // dispatch below wouldn't match this shape, so route to the
        // synthesized Json walker by hand: construct the unit-variant
        // value via `try_unit_enum_variant`, then feed it through
        // `compile_json_stringify`. Phase-8 line 435 slice 3.
        if let ExprKind::Path { segments, .. } = &callee.kind {
            if segments.len() == 3
                && segments[0] == "Json"
                && segments[2] == "stringify"
                && args.is_empty()
            {
                let variant = segments[1].clone();
                if let Some(layout) = self.enum_layouts.get("Json") {
                    if layout.tags.contains_key(&variant)
                        && layout.field_counts.get(&variant).copied().unwrap_or(0) == 0
                    {
                        if let Some(unit_val) = self.try_unit_enum_variant(&variant) {
                            return self.compile_json_stringify(unit_val);
                        }
                    }
                }
            }
        }

        // `Json.parse(s)` codegen dispatch (phase-8 line 435 slice 2).
        // Two-segment path `[Json, parse]` with one String arg. Routes
        // through the synthesized `__karac_json_ffi_to_kara` walker and
        // returns a `Result[Json, JsonError]`-shaped 5-i64 struct.
        // Intercepted ahead of the generic 2-segment associated-call
        // path below so the placeholder `Result.Err(...)` body in
        // `runtime/stdlib/json.kara` never lowers under compiled mode.
        if let ExprKind::Path { segments, .. } = &callee.kind {
            if segments.len() == 2
                && segments[0] == "Json"
                && segments[1] == "parse"
                && args.len() == 1
            {
                let input_val = self.compile_expr(&args[0].value)?;
                return self.compile_json_parse(input_val);
            }
        }

        // Associated function calls: Vec::new(), etc. Theme 6 sub-step 4
        // intercepts `R.method(args)` where R is an `effect resource R: T`
        // before assoc-call dispatch: those go through the runtime stack
        // via `karac_provider_lookup` + indirect vtable call. Any other
        // 2-segment path (Vec::new, T.from, primitive ops, user
        // `Type.method`, …) falls through to `compile_assoc_call`.
        if let ExprKind::Path { segments, .. } = &callee.kind {
            if segments.len() == 2 {
                if let Some(value) =
                    self.try_compile_provider_dispatch(&segments[0], &segments[1], args)?
                {
                    return Ok(value);
                }
                // Capitalized ambient resource call whose method the ambient
                // lowering backs: route through `compile_ambient_resource_method`,
                // which consults the runtime provider stack for an active
                // `with_provider[R]` override (cross-boundary, vtable-slotted
                // methods) and otherwise emits the builtin FFI default. Two
                // disjoint cases qualify: (a) `ambient_method_index`-known
                // pairs (`Clock.now`, `Env.set`) which have a vtable slot, and
                // (b) `ambient_ffi_lowered` no-slot pairs (`RandomSource.next_u64`,
                // `Env.args`) which have only an FFI default. Both gates are
                // required so OTHER ambient resource methods that already have a
                // dedicated lowering reached via `compile_assoc_call` (e.g.
                // `FileSystem.read_to_string`) keep their existing path rather
                // than erroring "not yet lowered".
                if super::method_call::ambient_method_index(&segments[0], &segments[1]).is_some()
                    || super::method_call::ambient_ffi_lowered(&segments[0], &segments[1])
                {
                    return self.compile_ambient_resource_method(&segments[0], &segments[1], args);
                }
                return self.compile_assoc_call(&segments[0], &segments[1], args);
            }
        }

        // Const generics slice 1b: `make_arr[i64, 4]()` parses callee
        // as `Path { segments: [name], generic_args: Some(args) }` (a
        // bare identifier with explicit generic args). Extract the
        // name + explicit generic args so the generic-call path can
        // bind the user-supplied const-args into the mango key.
        let (mut name, explicit_generic_args): (String, Option<Vec<GenericArg>>) =
            match &callee.kind {
                ExprKind::Identifier(n) => (n.clone(), None),
                ExprKind::Path {
                    segments,
                    generic_args: Some(ga),
                } if segments.len() == 1 => (segments[0].clone(), Some(ga.clone())),
                // A closure VALUE produced by a non-identifier callee — a struct
                // field `(h.f)(x)`, a Vec/array index `v[i](x)`, a tuple index
                // `t.0(x)`, a parenthesized closure expr, or any call result —
                // dispatches through the env-first fat-pointer indirect call
                // (B-2026-06-22-4). The named-identifier closure case is handled
                // below via `closure_fn_types`; this arm covers every other
                // place expression that evaluates to a `{fn_ptr, env_ptr}` value.
                // Falls through to the const-0 placeholder only when the callee
                // isn't a function-typed expression (no `fn_value_typed_exprs`
                // entry) — the same unknown-callee fallback as before.
                _ => {
                    if let Some(v) = self.compile_closure_value_call(callee, args)? {
                        return Ok(v);
                    }
                    return Ok(self.context.i64_type().const_int(0, false).into());
                }
            };

        // `Vector[T, N](lane0, …)` SIMD construction (design.md § Portable
        // SIMD). Intercepted before the generic-fn path — `Vector` is a
        // builtin type, not a user function. Builds an `<N x T>` value via an
        // insertelement chain.
        if name == "Vector" {
            if let Some(ga) = explicit_generic_args.as_deref() {
                return self.compile_vector_construction(ga, args);
            }
        }

        if name == "println" || name == "print" {
            return self.compile_print(&name, args);
        }

        // Slice c.1 — prelude `assert` / `assert_eq` / `assert_ne` lowering.
        // The interpreter dispatches these by name in
        // `src/interpreter/eval_call.rs`; before c.1 the codegen path
        // silently dropped them (the unknown-callee return-const-0
        // fallback below), which meant AOT-compiled programs ignored
        // failing asserts. We lower to a typed comparison plus a call
        // into `karac_test_record_failure` + `exit(1)` on failure. See
        // `src/codegen/test_assert.rs`.
        if name == "assert" {
            return self.compile_assert(args, call_span);
        }
        if name == "assert_eq" {
            return self.compile_assert_eq(args, call_span, false);
        }
        if name == "assert_ne" {
            return self.compile_assert_eq(args, call_span, true);
        }

        // Diverging prelude builtins `todo()` / `unreachable()` (type `!`).
        // They print a panic message + `exit(1)`, then terminate the block
        // with `unreachable` so no `ret` is emitted after them. Lowered here
        // — before the generic-call / unknown-callee fallback that would
        // otherwise hand back an `i64 0` placeholder and let the function
        // tail emit `ret i64 0` against a non-i64 return type (the historical
        // `fn boom() -> FakeClock { unreachable() }` module-verification
        // failure). Mirrors the interpreter's `eval_builtin_diverge`.
        if name == "todo" || name == "unreachable" {
            return self.compile_diverge(&name, args);
        }

        // Phase-5 auto-par divergence (A2a-2.2): `sleep_ms(ms: i64)` — the
        // leaf `suspends` async-sleep primitive. Intercepted before the
        // generic-fn path so the `#[compiler_builtin]` empty stub body in
        // `runtime/stdlib/time.kara` never lowers. Convert the millisecond
        // argument to nanoseconds and compose with the `karac_park_on_timer`
        // state machine (`emit_state_machine_invocation_for_park_on_timer`),
        // which arms a reactor deadline and parks on a completion slot.
        // Returns unit (the `i64 0` placeholder shared by all void builtins).
        if name == "sleep_ms" && args.len() == 1 {
            let ms = self.compile_expr(&args[0].value)?.into_int_value();
            let nanos_per_ms = self.context.i64_type().const_int(1_000_000, false);
            let nanos = self
                .builder
                .build_int_mul(ms, nanos_per_ms, "kara.timer.ms_to_nanos")
                .expect("ms * 1_000_000");
            self.emit_state_machine_invocation_for_park_on_timer(nanos);
            return Ok(self.context.i64_type().const_int(0, false).into());
        }

        // `forget[T](value)` — the FFI ownership-handoff primitive
        // (design.md § Exported C ABI, additive-interop Slice 4). Consume
        // the argument and suppress every scope-exit drop of its root
        // binding — the value's resources are handed off (deliberately
        // leaked from Kāra's view), so nothing is freed here. Intercepted
        // before the generic-fn path so the `#[compiler_builtin]` stub
        // body (`{}`, which would drop its owned param) never lowers.
        //
        // Soundness: the stdlib decl's owned param makes the ownership
        // checker + drop oracle treat `forget(v)` as a *consume*, so
        // neither schedules a scope-exit drop for `v`; the suppression
        // below matches that (belt-and-suspenders for the caller-side
        // cleanup queues the arg loop would otherwise register). The
        // value simply leaks — that IS the handoff.
        if name == "forget" && args.len() == 1 {
            if let ExprKind::Identifier(var_name) = &args[0].value.kind {
                self.suppress_user_drop_for_var(var_name);
                self.suppress_channel_drop_for_var(var_name);
                self.suppress_vec_buffer_drop_for_var(var_name);
            }
            // Evaluate the argument for its side effects (it may be a
            // temporary expression, not just a binding), then discard —
            // no drop, no store. `forget` returns unit (the `i64 0`
            // placeholder shared by all void builtins).
            let _ = self.compile_expr(&args[0].value)?;
            return Ok(self.context.i64_type().const_int(0, false).into());
        }

        // `std.mem::swap(mut a, mut b)` — exchange the values at two `mut ref`
        // places WITHOUT dropping either (roadmap Phase 8 § std.mem). Load
        // both current values, then store each into the OTHER place: raw
        // load/store moves the values, no destructor runs (both stay live,
        // just relocated). Intercepted before the generic-fn path so the
        // `#[compiler_builtin]` stub body (`{}`) never lowers. Returns unit
        // (the `i64 0` void-builtin placeholder, like `forget`).
        if name == "swap" && args.len() == 2 && !self.user_shadows_mem_builtin("swap") {
            let (pa, va) = self.mem_place_ptr_and_value(&args[0].value)?;
            let (pb, vb) = self.mem_place_ptr_and_value(&args[1].value)?;
            self.builder.build_store(pa, vb).unwrap();
            self.builder.build_store(pb, va).unwrap();
            return Ok(self.context.i64_type().const_int(0, false).into());
        }

        // `std.mem::replace(mut dest, value) -> T` — store `value` into
        // `*dest` and return the PREVIOUS `*dest`. Raw load of the old value
        // (moved out, returned — NOT dropped) then a raw store of the new
        // value (moved in): the caller owns the returned old value and the
        // place now owns the new one, so no buffer is freed here and none is
        // double-owned. `value`'s own scope-exit drop is already suppressed by
        // the ownership checker (the `value: T` param is a consume).
        if name == "replace" && args.len() == 2 && !self.user_shadows_mem_builtin("replace") {
            let (pd, old) = self.mem_place_ptr_and_value(&args[0].value)?;
            let new = self.compile_expr(&args[1].value)?;
            self.builder.build_store(pd, new).unwrap();
            // `value` is MOVED into `*dest` — the place now owns its buffer.
            // Neutralize the value temp's own scope-exit cleanup so it isn't
            // freed a second time (the double-free the raw store would leave:
            // an f-string / owned String-or-Vec / inline-Option arg carries a
            // cleanup that the normal call-arg move path suppresses; mirror it
            // here since this intercept bypasses that path).
            self.suppress_fstr_acc_if_moved_out(&args[1].value);
            self.suppress_source_vec_cleanup_for_arg(&args[1].value);
            self.suppress_inline_option_result_binding_move(&args[1].value);
            return Ok(old);
        }

        // Phase 6 line 218 slice 4: free `spawn(closure) -> TaskHandle[T]`
        // dispatch. Intercepted before the generic-fn path so the slice-1
        // stub body (`TaskHandle { task_id: 0 }`) never lowers. The
        // closure literal is recognised at the call site; bare-identifier
        // closures fall back to a placeholder (zero-handle) per the
        // task_group.rs documented limitation.
        if name == "spawn" && args.len() == 1 {
            return self.lower_spawn_call(&args[0].value);
        }

        // Phase 6 slice 1b — `collect_all_vec(fs)`. Intercepted before the
        // generic-fn path so the `#[compiler_builtin]` stub body
        // (`Vec.new()`) never lowers; the gather lowering runs every closure
        // in parallel via `karac_par_run` and assembles `Vec[Result[T, E]]`.
        if name == "collect_all_vec" && args.len() == 1 {
            return self.compile_collect_all_vec(&args[0].value, call_span);
        }

        // Phase 6 — `collect_all(|| a, || b, …)`, the heterogeneous
        // fixed-arity gather. Intercepted before the generic-fn path (it
        // has no stdlib decl); the typechecker's `infer_collect_all` has
        // already validated 2..=8 closure-`Result` branches. Lowers to the
        // same `karac_par_run` gather as `collect_all_vec` but with static
        // inline closures + a tuple result.
        if name == "collect_all" && (2..=8).contains(&args.len()) {
            return self.compile_collect_all(args, call_span);
        }

        // Layout-introspection intrinsics. Intercepted before the
        // generic-call lookup so the `{ 0 }` placeholder body in
        // `runtime/stdlib/intrinsics.kara` is never lowered. The
        // typechecker has already rejected opaque foreign type args
        // with `E_OPAQUE_TYPE_NO_KNOWN_SIZE`, so the type lowered here
        // is sized by construction.
        if name == "size_of" || name == "align_of" {
            if let Some(ga) = explicit_generic_args.as_deref() {
                return self.compile_layout_query_intrinsic(&name, ga, args);
            }
        }

        // Check if this is an enum variant constructor (tuple variant)
        if let Some(enum_val) = self.try_compile_enum_variant(&name, None, args)? {
            return Ok(enum_val);
        }

        // Distinct-type constructor: `UserId(value)` is a zero-cost wrap —
        // the compiled value IS the base value (layout-identical, no runtime
        // tag), so the constructor just compiles its single argument. For the
        // combined `distinct type T = Base where pred` form, it also emits the
        // runtime predicate assertion (`emit_refinement_assert` is a no-op
        // when `name` carries no predicate). design.md § Distinct Types.
        if self.distinct_bases.contains_key(&name) {
            if let Some(arg) = args.first() {
                let value = self.compile_expr(&arg.value)?;
                // Coerce to the base width so a bare literal arg
                // (`ExitCode(3)` — default `i64`) lands at the base type
                // (`i32`), keeping all values of a narrow-based distinct
                // type the same LLVM width (Slice B).
                let value = self.coerce_to_distinct_base(&name, value);
                self.emit_refinement_assert(&name, value)?;
                return Ok(value);
            }
        }

        // Check if this is a call to a generic function (monomorphize on demand)
        if self.generic_fns.contains_key(&name) {
            return self.compile_generic_call(
                &name,
                args,
                explicit_generic_args.as_deref(),
                call_span,
            );
        }

        // Check if this is an indirect call through a closure variable.
        if self.closure_fn_types.contains_key(&name) {
            return self.compile_closure_call(&name, args);
        }

        // Async-sched slice 2/3: a *direct* call to the leaf parking
        // primitive `karac_park_on_fd(fd, direction)` — from user source or
        // the `park_and_wake` test — routes to the same dispatcher-yield
        // helper the stdlib TCP/TLS lowerings use, rather than the generic
        // spin-loop intercept below. The leaf park is the one
        // network-boundary callee that yields to the dispatcher (register +
        // block on a per-park slot) instead of running its poll-fn
        // synchronously to completion on the calling thread.
        if name == KARAC_PARK_ON_FD && args.len() == 2 {
            let fd_val = self.compile_expr(&args[0].value)?.into_int_value();
            let dir_val = self.compile_expr(&args[1].value)?.into_int_value();
            self.emit_state_machine_invocation_for_park_on_fd(fd_val, dir_val);
            // `karac_park_on_fd` returns unit; mirror the generic
            // intercept's i64-0 unit placeholder.
            return Ok(self.context.i64_type().const_zero().into());
        }

        // Phase 6 line 26 slice 8d: network-boundary callee intercept.
        // When the callee is a network-boundary function (one with a
        // state-struct constructor + poll-fn emitted by slices 6 / 8c),
        // replace the direct `call @<name>(args)` with the state-machine
        // invocation shape:
        //
        //   %state  = call ptr @__kara_state_new_<name>()
        //   br label %kara.poll_loop_<n>
        // kara.poll_loop_<n>:
        //   %result = call i8 @__kara_poll_<name>(ptr %state, ptr null)
        //   %pending = icmp eq i8 %result, 0
        //   br i1 %pending, label %kara.poll_loop_<n>, label %kara.poll_done_<n>
        // kara.poll_done_<n>:
        //   call void @free(ptr %state)
        //   ; subsequent IR continues here
        //
        // The synchronous spin-loop is a v1 placeholder — slice 8e+
        // replaces the busy-loop with a yield to the line-17 runtime
        // scheduler dispatcher, so a Pending observation parks the
        // parent task until the event loop signals readiness. Args are
        // silently dropped at this slice (v1 user-program callers
        // overwhelmingly use no-arg shapes for network-boundary fns —
        // `driver()`, `fetch()`, …); a follow-on slice threads args
        // through the state-struct's captured-local fields at
        // constructor invocation time. Return value is `i64 0` — the
        // user-level return type for v1 network-boundary fns is unit;
        // when callees gain non-unit returns, the value lives in the
        // state struct's terminal field and is loaded after the loop.
        // A2 slice 2b.3: a coroutine-compiled callee is driven by the
        // *dispatcher*, not the caller — call the ramp (returns the completion-
        // slot `ptr`), block on it (`park_slot_wait`; the dispatcher resumes the
        // coroutine on fd-readiness and the body `park_slot_signal`s at
        // completion), then free the slot. No poll-loop and no caller
        // `coro.resume` (which would race the dispatcher / hit EWOULDBLOCK on
        // the non-blocking fd — §6¾). Unit return for this slice. Args are
        // compiled with the same ref/slice/owned mode dispatch as the
        // state-struct path below, but passed as ramp call arguments.
        if self.is_coroutine_compiled(&name) {
            let ramp = self
                .module
                .get_function(&name)
                .expect("coroutine ramp fn declared in declare_function");
            // A2 slice 5a — non-blocking spawn: inside a `__spawn_coro_wrap`
            // body (`self.coro_spawn_slot` is `Some`), the runtime owns the
            // completion slot and binds it to the `TaskHandle`. We hand that
            // slot to the ramp and return *without* waiting — the worker is
            // freed while the coroutine stays parked. Otherwise (the inline
            // drive) the caller owns the slot: allocate it, ramp, block on it,
            // free it.
            let spawn_slot = self.coro_spawn_slot;
            let slot = match spawn_slot {
                Some(s) => s,
                None => {
                    let slot_new = self
                        .module
                        .get_function("karac_runtime_park_slot_new")
                        .expect("karac_runtime_park_slot_new declared in Codegen::new");
                    self.builder
                        .build_call(slot_new, &[], "kara.coro.slot")
                        .expect("call karac_runtime_park_slot_new")
                        .try_as_basic_value()
                        .unwrap_basic()
                        .into_pointer_value()
                }
            };
            let ref_flags = self.fn_param_ref.get(&name).cloned().unwrap_or_default();
            let slice_elems = self
                .fn_param_slice_elem
                .get(&name)
                .cloned()
                .unwrap_or_default();
            let mut call_args: Vec<BasicMetadataValueEnum<'ctx>> = Vec::with_capacity(args.len());
            for (i, arg) in args.iter().enumerate() {
                let is_ref = ref_flags.get(i).copied().unwrap_or(false);
                let slice_elem = slice_elems.get(i).copied().flatten();
                let val: BasicValueEnum<'ctx> = if is_ref {
                    if let ExprKind::Identifier(var_name) = &arg.value.kind {
                        if let Some(ptr) = self.get_data_ptr(var_name) {
                            ptr.into()
                        } else {
                            let v = self.compile_expr(&arg.value)?;
                            self.materialize_rvalue_for_ref_arg(v, i)
                        }
                    } else if let Some(elem_ptr) = self.ref_arg_index_borrow_ptr(&arg.value)? {
                        elem_ptr.into()
                    } else {
                        let v = self.compile_expr(&arg.value)?;
                        self.materialize_rvalue_for_ref_arg(v, i)
                    }
                } else if let Some(elem_ty) = slice_elem {
                    match self.coerce_to_slice(&arg.value, elem_ty)? {
                        Some(slice_val) => slice_val,
                        None => self.compile_expr(&arg.value)?,
                    }
                } else {
                    // Owned-by-value arg moved into the coroutine: the coroutine
                    // now owns it and drops it at completion (see the coroutine-
                    // param registration in `compile_function_body`). Suppress
                    // the caller's user-`Drop` of the source binding so it isn't
                    // dropped twice — a synchronous (ramp+wait) caller would
                    // otherwise drop the same value the coroutine already
                    // dropped. No-op for non-`UserDrop` bindings. `ref`/`slice`
                    // args are borrows — never ownership transfers — so this only
                    // fires on owned moves.
                    //
                    // Channel-end (`Sender`/`Receiver`) moves need the same
                    // suppression on their `DropChannelEnd` action — and for the
                    // spawn-wrapper path this is load-bearing, not a no-op: the
                    // wrapper registered a channel-end cleanup for the captured
                    // `tx`/`rx` (`lower_spawn_shared`), and without suppressing it
                    // here the wrapper would drop (CLOSE) the channel on
                    // ramp-return — before the still-parked coroutine ran its
                    // `send`, so the receiver would see the closed-sentinel. The
                    // coroutine now owns that drop. No-op for non-channel args.
                    if let ExprKind::Identifier(var_name) = &arg.value.kind {
                        self.suppress_user_drop_for_var(var_name);
                        self.suppress_channel_drop_for_var(var_name);
                    }
                    self.compile_expr(&arg.value)?
                };
                call_args.push(val.into());
            }
            // Hidden trailing completion-slot param.
            call_args.push(slot.into());
            // Call the ramp (returns the coro handle — ignored; the dispatcher
            // drives + destroys via the shim). Control returns here once the
            // coroutine has parked at its first suspend.
            self.builder
                .build_call(ramp, &call_args, "kara.coro.drive")
                .expect("call coroutine ramp");
            // Non-blocking spawn (slot provided by the runtime): the wrapper
            // returns here, freeing the worker; the dispatcher drives the
            // parked coroutine and its completion signals the runtime-owned
            // slot (bound to the TaskHandle). No wait/free in this body.
            if spawn_slot.is_none() {
                let wait_fn = self
                    .module
                    .get_function("karac_runtime_park_slot_wait")
                    .expect("karac_runtime_park_slot_wait declared in Codegen::new");
                self.builder
                    .build_call(wait_fn, &[slot.into()], "")
                    .expect("call karac_runtime_park_slot_wait");
                // B-2026-06-19: a non-unit coroutine (`-> bool`/scalar) carried
                // its real return value into the slot at completion (see
                // `emit_coro_return_value_store`). Read it back here — after the
                // wait, before the free — into a temp of the callee's declared
                // return LLVM type. Pre-fix this path always returned `i64 0`,
                // discarding the value AND emitting the wrong type; using that
                // as a branch condition (`if ok` / `if not ok`) failed LLVM
                // verification (`Branch condition is not 'i1' type!`).
                let ret_ty = self
                    .fn_return_type_exprs
                    .get(&name)
                    .map(|te| self.llvm_type_for_type_expr(te));
                let is_unit = matches!(
                    ret_ty,
                    Some(BasicTypeEnum::StructType(s)) if s.count_fields() == 0
                );
                let loaded: Option<BasicValueEnum<'ctx>> = match ret_ty {
                    Some(ty) if !is_unit => {
                        let cur_fn = self
                            .builder
                            .get_insert_block()
                            .and_then(|bb| bb.get_parent())
                            .expect("coroutine call inside a function context");
                        let out = self.create_entry_alloca(cur_fn, "kara.coro.ret.out", ty);
                        let size = ty.size_of().expect("coroutine return type has a size");
                        let load_fn = self
                            .module
                            .get_function("karac_runtime_park_slot_load_result")
                            .expect("karac_runtime_park_slot_load_result declared in Codegen::new");
                        self.builder
                            .build_call(load_fn, &[slot.into(), out.into(), size.into()], "")
                            .expect("call karac_runtime_park_slot_load_result");
                        Some(
                            self.builder
                                .build_load(ty, out, "kara.coro.ret.value")
                                .expect("load coroutine return value"),
                        )
                    }
                    _ => None,
                };
                let free_fn = self
                    .module
                    .get_function("karac_runtime_park_slot_free")
                    .expect("karac_runtime_park_slot_free declared in Codegen::new");
                self.builder
                    .build_call(free_fn, &[slot.into()], "")
                    .expect("call karac_runtime_park_slot_free");
                if let Some(val) = loaded {
                    return Ok(val);
                }
            }
            return Ok(self.context.i64_type().const_int(0, false).into());
        }
        if let Some(ctor_fn) = self.state_machine_state_constructors.get(&name).copied() {
            let poll_fn = self
                .state_machine_poll_fns
                .get(&name)
                .copied()
                .expect("poll-fn co-emitted with state-machine constructor");
            let ptr_ty = self.context.ptr_type(AddressSpace::default());
            let i8_ty = self.context.i8_type();
            let cur_fn = self
                .builder
                .get_insert_block()
                .and_then(|bb| bb.get_parent())
                .expect("compile_call inside a function context");
            // Allocate the state struct via the constructor helper.
            let state_call = self
                .builder
                .build_call(ctor_fn, &[], "kara.state")
                .expect("call state-struct constructor");
            let state_ptr = state_call
                .try_as_basic_value()
                .unwrap_basic()
                .into_pointer_value();
            // Slice 8f: thread call args into the state struct's
            // captured-local slots. Per slice 4's layout ordering,
            // parameters occupy the first K fields of the layout (1..=K
            // in the state struct after skipping the i32 tag at field
            // 0); let-bindings introduced inside the body occupy fields
            // K+1..=N and stay uninitialized at construction time —
            // they're populated by the state-machine transform itself
            // when execution reaches the let-site.
            //
            // Slice 8ad: extend slice 8f's owned-arg-only discipline
            // to `ref T` / `mut ref T` / `mut Slice[T]` params,
            // mirroring slice 8z's identical fix on the per-mono
            // intercept in `compile_generic_call`. Without this, ref-
            // flagged args fell through to "compile, store loaded
            // value" — which mismatches the ptr- / Slice-struct-
            // shaped state-struct field LLVM type. Empirical probe
            // 2026-05-20 confirmed `fn driver(item: ref Vec[i64]) {
            // fetch(); }` emitted `store { ptr, i64, i64 } %v, ptr
            // %kara.arg0.field_ptr` against a ptr field — accepted
            // under opaque pointers but overflowed past the field's
            // 8-byte footprint. The fix consults `fn_param_ref` /
            // `fn_param_slice_elem` keyed on the bare fn name (the
            // non-generic look-up key) and dispatches by mode: ref
            // params with Identifier args route through
            // `get_data_ptr`; ref params with rvalue args route
            // through the shared `materialize_rvalue_for_ref_arg`
            // helper that slice 8z extracted (now `pub(super)` so
            // both intercepts share it); slice-elem params route
            // through `coerce_to_slice` to synthesize the
            // `{ ptr, i64 }` header at the call site.
            let state_struct = self
                .state_struct_types
                .get(&name)
                .copied()
                .expect("state struct type co-emitted with constructor");
            let ref_flags = self.fn_param_ref.get(&name).cloned().unwrap_or_default();
            let slice_elems = self
                .fn_param_slice_elem
                .get(&name)
                .cloned()
                .unwrap_or_default();
            for (i, arg) in args.iter().enumerate() {
                let field_idx = (i + 1) as u32;
                let field_ptr = self
                    .builder
                    .build_struct_gep(
                        state_struct,
                        state_ptr,
                        field_idx,
                        &format!("kara.arg{i}.field_ptr"),
                    )
                    .expect("GEP state struct field for arg");

                let is_ref = ref_flags.get(i).copied().unwrap_or(false);
                let slice_elem = slice_elems.get(i).copied().flatten();

                let to_store: BasicValueEnum<'ctx> = if is_ref {
                    // Ref param: pass a pointer to the caller-side
                    // data, not the loaded value.
                    if let ExprKind::Identifier(var_name) = &arg.value.kind {
                        if let Some(ptr) = self.get_data_ptr(var_name) {
                            ptr.into()
                        } else {
                            let val = self.compile_expr(&arg.value)?;
                            self.materialize_rvalue_for_ref_arg(val, i)
                        }
                    } else if let Some(elem_ptr) = self.ref_arg_index_borrow_ptr(&arg.value)? {
                        // `vec[idx]` borrow — pass the element pointer in
                        // place (no shallow-copy + drop double-free).
                        elem_ptr.into()
                    } else {
                        let val = self.compile_expr(&arg.value)?;
                        self.materialize_rvalue_for_ref_arg(val, i)
                    }
                } else if let Some(elem_ty) = slice_elem {
                    // `mut Slice[T]` param: synthesize the slice
                    // header from the arg. Falls through to the
                    // loaded value for shapes the coercion doesn't
                    // recognize (matches slice 8z's discipline).
                    match self.coerce_to_slice(&arg.value, elem_ty)? {
                        Some(slice_val) => slice_val,
                        None => self.compile_expr(&arg.value)?,
                    }
                } else {
                    self.compile_expr(&arg.value)?
                };

                self.builder
                    .build_store(field_ptr, to_store)
                    .expect("store arg into state struct field");
            }
            // Branch into the poll loop. Slice 8e routes the Pending
            // path through a `kara.poll_yield` block that calls
            // `sched_yield` before looping back to `kara.poll_loop`,
            // so the parent thread cooperatively yields the OS
            // scheduler quantum between poll-fn invocations instead
            // of busy-spinning. Without the yield, a tight loop would
            // starve the line-17 dispatcher thread (and any other
            // ready tasks on the same scheduler) of cycles needed to
            // process event-loop readiness wakeups, defeating the
            // purpose of the state-machine transform.
            let loop_bb = self.context.append_basic_block(cur_fn, "kara.poll_loop");
            let yield_bb = self.context.append_basic_block(cur_fn, "kara.poll_yield");
            let done_bb = self.context.append_basic_block(cur_fn, "kara.poll_done");
            self.builder
                .build_unconditional_branch(loop_bb)
                .expect("br to poll loop");
            // Loop body: invoke poll-fn, check discriminant.
            self.builder.position_at_end(loop_bb);
            let null_cancel = ptr_ty.const_null();
            let poll_call = self
                .builder
                .build_call(
                    poll_fn,
                    &[state_ptr.into(), null_cancel.into()],
                    "kara.poll_result",
                )
                .expect("call poll-fn");
            let poll_result = poll_call
                .try_as_basic_value()
                .unwrap_basic()
                .into_int_value();
            let is_pending = self
                .builder
                .build_int_compare(
                    IntPredicate::EQ,
                    poll_result,
                    i8_ty.const_int(0, false),
                    "kara.is_pending",
                )
                .expect("icmp eq i8 result, 0");
            self.builder
                .build_conditional_branch(is_pending, yield_bb, done_bb)
                .expect("br on poll discriminant");
            // Yield block (Pending path): cooperatively yield the OS
            // scheduler then loop back. `sched_yield` returns i32 — we
            // discard the result (a non-zero return means the OS
            // refused to yield, which on Linux / macOS only happens on
            // catastrophic failure and isn't recoverable from here).
            self.builder.position_at_end(yield_bb);
            self.builder
                .build_call(self.sched_yield_fn, &[], "kara.yield_result")
                .expect("call sched_yield");
            self.builder
                .build_unconditional_branch(loop_bb)
                .expect("br back to poll loop after yield");
            // Done: release the state struct, position for downstream IR.
            self.builder.position_at_end(done_bb);
            // Slice 8i: if the callee has a non-unit return type
            // (recorded in `state_machine_return_types`), load the
            // terminal return-value field from the state struct
            // BEFORE the `free` call — once we free the heap
            // allocation, the field is no longer dereferenceable. The
            // terminal field's index is the state struct's last field:
            // `1 + N` where N is the captured-local count.
            let call_result =
                if let Some(ret_ty) = self.state_machine_return_types.get(&name).copied() {
                    let state_struct = self
                        .state_struct_types
                        .get(&name)
                        .copied()
                        .expect("state struct type co-emitted with return-type entry");
                    let n_fields = state_struct.count_fields();
                    let terminal_idx = n_fields - 1;
                    let terminal_ptr = self
                        .builder
                        .build_struct_gep(
                            state_struct,
                            state_ptr,
                            terminal_idx,
                            "kara.return.field_ptr",
                        )
                        .expect("GEP terminal return-value field on caller side");
                    self.builder
                        .build_load(ret_ty, terminal_ptr, "kara.return.value")
                        .expect("load callee return value from terminal field")
                } else {
                    self.context.i64_type().const_int(0, false).into()
                };
            self.builder
                .build_call(self.free_fn, &[state_ptr.into()], "")
                .expect("call free on state struct");
            return Ok(call_result);
        }

        // Per-layout monomorphization (slice 2,
        // `docs/spikes/per-layout-monomorphization.md`): when this is a known
        // non-generic function and an argument carries a non-`Aos` layout at
        // this call site (a SoA `Vec[E]` binding passed whole), retarget the
        // call to an on-demand SoA monomorph whose matching params lower as the
        // 4-field SoA struct. The mono symbol is `<name>$soa_<layout>`, and its
        // ref/slice-elem ABI tables were registered under that mangled key by
        // `declare_mono_function`, so the direct-call resolution below picks
        // them up via the reassigned `name`. An all-`Aos` call adds no entry,
        // so non-SoA code falls straight through to the original function.
        //
        // Backward inference (slice 3): consume the one-shot return-layout the
        // SoA `let <recv> = <call>()` arm parked here. It applies to THIS call
        // only — `take` it before args are compiled (the arg loop runs further
        // below), so a nested call inside an argument can't inherit it. Honored
        // only when non-`Aos` AND the callee actually returns a `Vec[E]` (the
        // backward monomorph lowers that return to the SoA struct).
        let pending_ret = self.pending_return_layout.take();
        let return_layout = pending_ret
            .filter(|l| !matches!(l, LayoutId::Aos))
            .filter(|_| {
                self.fn_asts
                    .get(&name)
                    .is_some_and(Self::return_is_layout_carrying)
            })
            .unwrap_or(LayoutId::Aos);

        // Cheap gate first: only a callee with a layout-carrying (`Vec[E]`)
        // value param OR a `Vec[E]` return can ever specialize, so skip the AST
        // clone for the common case — most user calls pay only a HashMap lookup
        // plus a param/return scan here.
        let callee_may_specialize = self.fn_asts.get(&name).is_some_and(|f| {
            f.params.iter().any(Self::param_is_layout_carrying)
                || Self::return_is_layout_carrying(f)
        });
        if callee_may_specialize {
            let callee_fn = self.fn_asts[&name].clone();
            let layout_subst = self.compute_call_layout_subst(&callee_fn, args);
            let any_forward = layout_subst.values().any(|l| !matches!(l, LayoutId::Aos));
            let any_backward = !matches!(return_layout, LayoutId::Aos);
            if any_forward || any_backward {
                let mangled = self.mangle_mono_name(
                    &name,
                    &callee_fn,
                    &HashMap::new(),
                    &HashMap::new(),
                    &HashMap::new(),
                    &layout_subst,
                    &return_layout,
                );
                self.ensure_layout_mono_generated(
                    &callee_fn,
                    &mangled,
                    layout_subst,
                    return_layout,
                )?;
                name = mangled;
            }
        }

        // An `unsafe extern` import declared with `#[link_name("symbol")]`
        // was registered in the module under its foreign symbol, not its
        // Kāra name — translate before lookup (no-op for every other call,
        // since the map is empty unless `#[link_name]` is used).
        let lookup_name = self
            .extern_link_names
            .get(&name)
            .cloned()
            .unwrap_or_else(|| name.clone());
        let func = match self.module.get_function(&lookup_name) {
            Some(f) => f,
            None => {
                // Unknown function — silently return 0 (e.g. stdlib builtins not yet codegen'd)
                return Ok(self.context.i64_type().const_int(0, false).into());
            }
        };

        let ref_flags = self.fn_param_ref.get(&name).cloned().unwrap_or_default();
        let slice_elems = self
            .fn_param_slice_elem
            .get(&name)
            .cloned()
            .unwrap_or_default();
        // B-2026-07-02-13: the pending-let element hint describes the LET
        // BINDING's element width; a user callee's argument literals must
        // pack at the CALLEE's declared width (their own span record), not
        // the binding's — `let s: String = tail_str(vec![100, 200, 300]);`
        // packed the arg elements as i8 and the callee read garbage,
        // silently. Cleared for the argument loop, restored after (the hint
        // still serves the direct-RHS constructor lowering that follows the
        // call in other RHS shapes). Builtin constructor intercepts
        // (`Column.from_vec`, `Vec.filled`, …) never reach this loop and
        // keep the hint — their arg literal legitimately inherits the
        // binding's width.
        let saved_pending_elem = self.pending_let_elem_type.take();
        let saved_pending_elem_te = self.pending_let_elem_type_expr.take();
        let mut compiled_args: Vec<BasicMetadataValueEnum<'ctx>> = Vec::new();
        for (i, a) in args.iter().enumerate() {
            // B-2026-06-20-1: a bare named `fn` passed to a `Fn(...)`-typed
            // parameter is reified into the closure fat-pointer ABI
            // (`{trampoline, null env}`) so it dispatches through the callee's
            // env-first indirect call. Returns `None` for any other arg shape,
            // which then compiles normally below. Without this the bare fn name
            // lowers to a raw `ptr` and mismatches the fat-pointer param slot.
            if let Some(fat) = self.reify_named_fn_as_fn_value(&name, i, &a.value) {
                compiled_args.push(fat.into());
                continue;
            }
            let is_ref = ref_flags.get(i).copied().unwrap_or(false);
            if is_ref {
                // `ref Slice[T]` / `mut ref Slice[T]` param fed an `Array[T, N]`:
                // the callee receives a POINTER to a `{ptr,len}` slice header,
                // but an Array binding's storage is its raw elements — no header.
                // The `get_data_ptr` fast-path below would pass `&array[0]`, so
                // the callee read `{ptr,len}` out of the first two elements — a
                // bogus slice → segfault (B-2026-06-19-1). Synthesize the header
                // and pass a pointer to it instead (what the rvalue-ref path does
                // for `v.as_slice()`). Restricted to Array sources on purpose: a
                // `Vec` binding's storage starts with `{ptr,len}` (a header
                // superset) and a `Slice` / `ref Slice` binding's `get_data_ptr`
                // already yields a header pointer, so those forward correctly
                // through the fast-path below — intercepting them would re-coerce
                // a ref-slice binding and corrupt the forward.
                if let Some(Some(elem_ty)) = slice_elems.get(i).cloned() {
                    let src_is_array = matches!(&a.value.kind, ExprKind::Identifier(var)
                        if self
                            .variables
                            .get(var.as_str())
                            .is_some_and(|s| matches!(s.ty, BasicTypeEnum::ArrayType(_))));
                    if src_is_array {
                        if let Some(slice_val) = self.coerce_to_slice(&a.value, elem_ty)? {
                            let ptr = self.materialize_rvalue_for_ref_arg(slice_val, i);
                            compiled_args.push(ptr.into());
                            continue;
                        }
                    }
                }
                // Pass a pointer to the variable's data instead of the loaded value.
                if let ExprKind::Identifier(var_name) = &a.value.kind {
                    if let Some(ptr) = self.get_data_ptr(var_name) {
                        compiled_args.push(ptr.into());
                        continue;
                    }
                }
                // `vec[idx]` borrow: pass a pointer to the element in
                // place rather than a shallow-copied-then-dropped temp
                // (the latter double-frees an aggregate element's buffer
                // the outer Vec still owns).
                if let Some(elem_ptr) = self.ref_arg_index_borrow_ptr(&a.value)? {
                    compiled_args.push(elem_ptr.into());
                    continue;
                }
                // A borrow-returning call in `ref`-arg position
                // (`first(pick(v))`, B-2026-06-10-4): the call's result IS
                // already a pointer to the borrowed data (the `-> ref T`
                // ABI), so forward it directly. The normal `compile_expr`
                // path would hit `compile_call`'s direct-use intercept,
                // which LOADS the pointee into a `{ptr,len,cap}` value;
                // the rvalue-ref path below would then store that into a
                // temp and queue its cleanup — double-freeing the borrow
                // source the callee only borrows. Bypass the intercept via
                // `compiling_ref_return_let_rhs` so the call yields its raw
                // borrow ptr (mirrors the let-RHS / explicit-return
                // handling in stmts.rs / exprs.rs). No temp, no cleanup —
                // a borrow is never an ownership transfer.
                if self.is_borrow_returning_call_expr(&a.value) {
                    let prev = self.compiling_ref_return_let_rhs;
                    self.compiling_ref_return_let_rhs = true;
                    let ptr = self.compile_expr(&a.value);
                    self.compiling_ref_return_let_rhs = prev;
                    compiled_args.push(ptr?.into());
                    continue;
                }
            }
            // Slice-parameter coercion: if this parameter slot expects
            // Slice[T] / mut Slice[T] and the argument is an Array[T, N],
            // Vec[T], or already a slice, synthesize the `{ptr, i64}`
            // slice header at the call site. See design.md § Slices.
            if let Some(Some(elem_ty)) = slice_elems.get(i).cloned() {
                if let Some(slice_val) = self.coerce_to_slice(&a.value, elem_ty)? {
                    compiled_args.push(slice_val.into());
                    continue;
                }
            }
            if is_ref {
                // Rvalue ref path: the arg is a non-place expression
                // (string/integer/char/bool literal, function return,
                // arithmetic, etc.) bound to a `ref T` param. The
                // typechecker accepts these — design.md § Part 1½
                // Rule 4 documents that `ref T` accepts any source
                // unmarked. Codegen must materialize the value into a
                // stack temp so the callee receives the `ptr` ABI its
                // signature declares; without this the call IR mints
                // `call @f({ptr,i64,i64} %lit)` / `call @f(i32 42)` and
                // module verification rejects the mismatch against the
                // callee's `ptr` parameter. Mirrors what the let-binding
                // workaround did implicitly (`let c = "..."; f(c)` —
                // `let` allocates a slot, then the identifier fast-path
                // above passes that slot's pointer).
                //
                // Cleanup: scalars and the no-op `cap = 0` non-owning
                // case (string literals, .rodata-backed) need none. A
                // fresh *owned* rvalue — a Vec/String, a Map/Set handle —
                // would otherwise leak its heap storage after the call
                // returns (the callee only *borrows* via `ref T`). Route
                // the temp through `queue_ref_rvalue_arg_cleanup`, the
                // owned-temp classification shared with the discard
                // chokepoint (slice 2): it recovers the Vec element type
                // from `owned_temp_drops` (closing the nested-heap leak the
                // prior `track_vec_var(temp, None)` left open for
                // `Vec[String]` / `Vec[Vec[T]]`) and frees Map/Set handles
                // (which the old vec-struct-only check missed entirely).
                // The `cap > 0` / null guards inside the cleanup actions
                // keep the registration safe to apply unconditionally.
                let val = self.compile_expr(&a.value)?;
                let cur_fn = self
                    .builder
                    .get_insert_block()
                    .and_then(|bb| bb.get_parent())
                    .expect("compile_call inside a function context");
                let temp =
                    self.create_entry_alloca(cur_fn, &format!("ref_rvalue_arg{i}"), val.get_type());
                self.builder.build_store(temp, val).unwrap();
                self.queue_ref_rvalue_arg_cleanup(temp, val, &a.value);
                compiled_args.push(temp.into());
                continue;
            }
            let val = self.compile_expr(&a.value)?;
            // `Option[shared T]` ref-share at the call site: when
            // the arg is a tracked Identifier binding whose static
            // type is Option[shared T], emit a discriminant- and
            // null-guarded `rc_inc` on the inner pointer so the
            // callee receives an independent +1 ref. The caller's
            // slot is NOT mutated — its queued `RcDecOption` still
            // fires at scope-exit and balances the original +1.
            // The callee's `track_rc_option_var` (queued in
            // `compile_function` for Option[shared T] params)
            // owns the dec of the newly-incremented ref at
            // function exit.
            //
            // Mirrors the plain shared-T arm of
            // `suppress_source_vec_cleanup_for_arg`: caller-side
            // `emit_refcount_inc` so the consumer holds its own
            // ref while the source's dec stays in place. The
            // earlier (0866037) design here zeroed the caller's
            // slot to "move" ownership; that broke any call site
            // that passed the same Option[shared T] binding more
            // than once (e.g., `for i in 0..k { f(l1, l2); }` —
            // the first call would clear l1/l2 to None, every
            // subsequent call would receive None). The kata bench
            // surfaced this as `add_two_numbers(l1, l2)` returning
            // None on iterations 2..K.
            //
            // No-op for non-Identifier args (call-result
            // `make_chain(10)`, struct literals, fresh `Some(...)`)
            // — those carry their own +1 from the producer; the
            // callee's `track_rc_option_var` balances them. Also
            // no-op for non-shared Option[T] params (no entry in
            // `var_option_shared_heap`).
            // Phase C2b: borrowed positions of reconciled headerless
            // callees take no arg inc — the callee borrows (no exit
            // dec) and the chain has no rc word.
            let borrow_skip = self.borrowed_arg_skip(&name, i);
            if !borrow_skip {
                self.share_option_shared_ref_for_arg(&a.value);
            }
            // Companion for a FieldAccess arg reading an `Option[shared T]`
            // field of an Identifier/`self`-bound shared struct (`merge(n1.next,
            // l2)` in the recursive merge-two-sorted-lists). The niche field
            // read (`niche_load_option_field`) just LOADS the pointer — no inc —
            // so without this the callee's param `RcDecOption` decrements an
            // uncounted ref and frees the sub-list mid-recursion. Inc the loaded
            // inner so the callee holds an independent +1; the caller's heap
            // field still owns its own ref. Call-like objects (`get().next`) go
            // through `compile_field_access`'s call-chain branch which already
            // incs, so they are excluded (the object must match
            // `shared_type_for_expr`, i.e. an Identifier/self binding).
            if !borrow_skip {
                self.share_option_shared_field_ref_for_arg(&a.value, val);
            }
            compiled_args.push(BasicMetadataValueEnum::from(val));
            // B-2026-06-11-5: a block-construct call argument
            // (`take({ f"…" })`) had its tail acc suppressed by
            // `suppress_block_tail_cleanup` (B-2026-06-11-2) so a binding /
            // return consumer could own it — but a bare call argument has no
            // owning consumer, so the temp orphaned and leaked. A DIRECT
            // f-string arg is caller-owned (its acc stays armed in the caller
            // frame and frees after the call); re-establish that same caller
            // ownership for the block-wrapped form by materializing the temp
            // into the caller's scope (`materialize_owned_temp` self-guards on
            // Vec/String, so non-heap block args are a no-op). Single-tail
            // blocks only — mirrors `discarded_owned_temp_tail`'s conservatism;
            // a branching `if`/`match` arg whose tail is an aliased place would
            // double-free, so those stay a (safe) leak for a later slice.
            //
            // Two fresh-heap arg shapes share the same caller-scope
            // materialization (`materialize_owned_temp` self-guards on
            // Vec/String LLVM shape + the `owned_temp_drops` hint for Map/RC,
            // so a non-heap arg is a no-op):
            //
            //  • a single-tail BLOCK construct (`take({ f"…" })`) — B-2026-06-11-5;
            //  • #20: a heap String/Vec produced by a Call / MethodCall and passed
            //    DIRECTLY by value (`sink(mk(i))`, `f(a + n.to_string())`). Owned
            //    `String`/`Vec` by-value params are NOT freed by the callee (they
            //    land in `owned_vecstr_params` for retaining-consume deep-copy,
            //    never a callee-side `track_vec_var` — confirmed by
            //    `let t = mk(i); sink(t)` being single-free), so the temp orphaned
            //    and leaked one buffer per inline call. The #20 arm is restricted
            //    to the Vec/String shape (`llvm_ty_is_vec_struct`) on purpose:
            //    shared-RC / `Option[shared T]` call results are already balanced
            //    by the callee's `track_rc_option_var`, so routing them through
            //    `materialize_owned_temp` (a second `track_rc_var` dec) would
            //    double-free. `expr_yields_fresh_owned_temp` is Call/MethodCall-
            //    only and excludes borrow-returning calls (result aliases the
            //    borrow source). `ref T` rvalue args never reach here — they
            //    `continue` through `queue_ref_rvalue_arg_cleanup` above.
            //
            // Both arms peel only single-tail / direct shapes — a branching
            // `if`/`match` arg whose tail is an aliased place would double-free,
            // so those stay a (safe) leak for a later slice.
            let is_block_arg = matches!(
                &a.value.kind,
                ExprKind::Block(_)
                    | ExprKind::Seq(_)
                    | ExprKind::Unsafe(_)
                    | ExprKind::LabeledBlock { .. }
            );
            // `rhs_stages_fstr_acc` excludes a struct/enum `.to_string()` arg:
            // it lowers via the synthetic f-string, whose accumulator already
            // owns a caller-scope cleanup — materializing it again would
            // double-free. (A scalar/`String` `.to_string()` and a plain user-fn
            // result do NOT stage the acc, so they still get materialized.)
            // An inline-temp-Vec heap-element index (`sink(names()[0])`) is the
            // sibling of #20 for the by-value-arg consumer: the deep clone
            // `compile_inline_temp_vec_index` mints has no consuming binding and
            // its synth Vec local is de-registered, so the callee (which does
            // not free owned String/Vec by-value params — they land in
            // `owned_vecstr_params`) leaves it orphaned without a caller-scope
            // drop. Materialize it here exactly like a direct fresh call result
            // (B-2026-06-14-32).
            // B-2026-07-02-6 follow-on: a COLLECTION-LITERAL arg compiled to a
            // heap Vec (`f([10, 20, 30])`, `f([7; 3])`) is the same orphaned
            // fresh-heap shape as #20 — by-value Vec params are caller-retains
            // (the callee never frees; confirmed by the by_val IR having no
            // free), so without a caller-scope materialization the literal's
            // buffer leaks once per call. `llvm_ty_is_vec_struct` keeps stack
            // `[N x T]` array literals (Array-typed params) out.
            let is_collection_literal_arg = matches!(
                &a.value.kind,
                ExprKind::ArrayLiteral(_)
                    | ExprKind::PrefixCollectionLiteral { .. }
                    | ExprKind::RepeatLiteral { .. }
            );
            let is_fresh_heap_call_arg = (self.expr_yields_fresh_owned_temp(&a.value)
                || self.expr_is_inline_temp_vec_heap_index(&a.value)
                || is_collection_literal_arg)
                && self.llvm_ty_is_vec_struct(val.get_type())
                && !self.rhs_stages_fstr_acc(&a.value);
            // A fresh bare-`shared` (RC-box) call / variant-constructor result
            // passed BY VALUE: the callee's entry `emit_refcount_inc` + scope-exit
            // `track_rc_var` dec are NET-ZERO (the caller-keeps-reference
            // convention, `functions.rs`), so the caller still owns the temp's +1
            // and must release it — but a directly-passed temp has no binding to
            // carry that dec, so the box leaks (the self-hosted
            // `render_expr(parse_expr(src))` AST node: 80 bytes / parse). The #20
            // sibling above was Vec-only on the (correct-for-`Option[shared T]`,
            // wrong-for-bare-shared) belief that the callee balances the ref; a
            // bare shared param does NOT consume — it inc/decs — so queue the
            // caller-side dec here. `fresh_arg_bare_shared_heap_type` resolves the
            // box's heap layout from the producing fn's return type (or a variant
            // ctor) and self-excludes a `g(make())` passthrough chain, so the box
            // is dec'd exactly once. (Not routed through `materialize_owned_temp`:
            // a bare shared call result carries no `owned_temp_drops` entry — that
            // table only records `Type::Shared`, which a user `shared enum` result
            // is not — so the hint-driven shared arm there never fires for it.)
            if is_block_arg || is_fresh_heap_call_arg {
                self.materialize_owned_temp(val, (a.value.span.offset, a.value.span.length));
            }
            if val.is_pointer_value() {
                if let Some(heap_type) = self.fresh_arg_bare_shared_heap_type(&a.value) {
                    self.track_rc_var("__owned_arg_tmp", val.into_pointer_value(), heap_type);
                }
            }
            // Register the caller-side drop for an inline owned-aggregate arg
            // (tuple/struct literal — B-2026-06-11-4 part b; enum-variant
            // constructor — B-2026-06-12-10; fn-RETURNED Drop temp —
            // B-2026-07-01-7). Shared with the method-call path. Skipped
            // when the CALLEE can return this parameter (the passthrough
            // guard): `pass(make())` / `let x = pass(Guard{..})` flow the
            // value out to the caller's consumer of the RESULT, whose own
            // binding/temp drop covers it — registering here too was a
            // DOUBLE user-drop firing (both surfaces, probe f6).
            //
            // B-2026-07-08-6 — the passthrough guard's premise ("the result IS
            // this arg's buffer") holds only when the callee FORWARDS the arg.
            // A copy-supported heap STRUCT param is ENTRY-COPIED at the callee
            // (`make_aggregate_param_callee_owned` → `deep_copy_struct_heap_-
            // fields_in_place`): the callee returns an INDEPENDENT copy and the
            // ORIGINAL moved-in buffer is orphaned (the caller suppressed its
            // own cleanup as a move). So for a struct-literal arg the callee
            // entry-copies, register the caller's struct drop even on the
            // return-passthrough path — the copy flows out (freed via the
            // result binding), this drops the original. Confirmed leak: `fn
            // id(a: Name) -> Name { a }` over `Name { s: String }` leaked the
            // arg buffer; a String param (no entry-copy) and a true-forward
            // passthrough are unaffected (`arg_is_entry_copied_heap_struct`
            // matches only copy-supported heap structs).
            if !self.call_arg_flows_into_return(&name, i)
                || self.arg_is_entry_copied_heap_struct(&a.value)
            {
                self.track_inline_owned_aggregate_arg(val, &a.value);
            }
        }
        // Restore the pending-let hint cleared above for the arg loop.
        self.pending_let_elem_type = saved_pending_elem;
        self.pending_let_elem_type_expr = saved_pending_elem_te;

        // Niche-ABI arg pack — see `pack_niche_abi_args`. Runs AFTER the
        // arg loop so the refcount bookkeeping above
        // (`share_option_shared_ref_for_arg` & co.) operated on the
        // conventional shape.
        self.pack_niche_abi_args(&name, &mut compiled_args);

        // Scalar width coercion at the call-arg boundary — internal
        // values default to i64/f64 widths while the callee's params
        // lower at their declared width, so `f(5)` against
        // `fn f(x: i8)` would emit `call i8 @f(i64 5)` and fail
        // verification. Covers user fns AND extern/host declarations
        // (same dispatch path). See `coerce_scalar_to_type`.
        self.coerce_args_to_fn_params(func, &mut compiled_args);

        // `#[track_caller]` slice 4: a call to a `#[track_caller]` callee passes
        // the caller's source location as three trailing args matching the hidden
        // params `declare_function` appended. When THIS fn is itself
        // `#[track_caller]`, forward its own received location (the transitivity
        // rule); otherwise pass the literal call-site `(file, line, col)`.
        // Appended AFTER `coerce_args_to_fn_params` (which zips the N user args
        // against the callee's param types and stops at the shorter) so these
        // already-typed args pass through untouched. Inert unless the callee is
        // `#[track_caller]`, i.e. never for a program without the attribute.
        if self.track_caller_fns.contains(&lookup_name) {
            match self.current_fn_caller_loc {
                Some((file, line, col)) => {
                    compiled_args.push(file.into());
                    compiled_args.push(line.into());
                    compiled_args.push(col.into());
                }
                None => {
                    let file = self.source_filename.as_deref().unwrap_or("<unknown>");
                    let file_ptr = self
                        .builder
                        .build_global_string_ptr(&format!("{file}\0"), "tc_callsite_file")
                        .unwrap()
                        .as_pointer_value();
                    let i32_ty = self.context.i32_type();
                    compiled_args.push(file_ptr.into());
                    compiled_args.push(i32_ty.const_int(call_span.line as u64, false).into());
                    compiled_args.push(i32_ty.const_int(call_span.column as u64, false).into());
                }
            }
        }

        // Phase-7 line 5 sub-item 1 — hot-swap indirect dispatch.
        // For callees registered in `hot_swap_slots`, lower the call as
        // a load from the slot in `@karac_hotswap_table` followed by an
        // indirect call. The table is populated at startup by the ctor
        // emitted in `emit_hot_swap_table` so v1 binaries call the
        // intended target on first dispatch; the indirection exists so
        // post-v1 reload can replace the entry. Closure invocations,
        // FFI extern decls, and intrinsic / runtime calls take the
        // direct path below — slots are only minted for user-defined
        // pub fn declarations.
        let call = if let Some(slot) = self.hot_swap_slots.get(&name).copied() {
            self.build_hot_swap_indirect_call(func, slot, &compiled_args)
        } else {
            self.builder
                .build_call(func, &compiled_args, "call")
                .unwrap()
        };

        let basic_val = call.try_as_basic_value();
        if basic_val.is_instruction() {
            Ok(self.context.i64_type().const_int(0, false).into())
        } else {
            Ok(self.unpack_niche_abi_ret(&name, basic_val.unwrap_basic()))
        }
    }

    /// The shared (RC) heap-box layout produced by a fresh-temp by-value call
    /// argument whose `+1` the CALLER must release — the bare-`shared`
    /// enum/struct net-zero-param case (`render_expr(parse_expr(src))`: the AST
    /// node box). `Some(heap_type)` only when:
    ///   * the arg is a fresh owned temp (`expr_yields_fresh_owned_temp` — a
    ///     non-borrow Call / variant ctor; an identifier arg is an existing
    ///     tracked binding whose own scope-exit dec already covers it), AND
    ///   * its producing call returns a bare `shared` type (resolved from
    ///     `fn_return_type_names`, or `enum_name_of_expr` for a variant ctor)
    ///     that is in `shared_types`, AND
    ///   * NONE of that call's own arguments is itself such a fresh shared-box
    ///     temp — the passthrough guard. A `g(make())` chain where `g` returns
    ///     the same box it received would otherwise register a dec for BOTH
    ///     `make()` and `g(make())` against the one box (a double-free); skipping
    ///     the outer leaves exactly the innermost producer's dec, freeing the box
    ///     once. (Cost: a `g` that ignores its shared arg and mints a fresh box
    ///     is conservatively left a leak rather than risk the double-free.)
    pub(super) fn fresh_arg_bare_shared_heap_type(
        &self,
        expr: &Expr,
    ) -> Option<inkwell::types::StructType<'ctx>> {
        if !self.expr_yields_fresh_owned_temp(expr) {
            return None;
        }
        let ExprKind::Call { callee, args, .. } = &expr.kind else {
            return None;
        };
        if args
            .iter()
            .any(|a| self.fresh_arg_bare_shared_heap_type(&a.value).is_some())
        {
            return None;
        }
        let type_name = match &callee.kind {
            ExprKind::Identifier(n) => self
                .fn_return_type_names
                .get(n)
                .cloned()
                .or_else(|| self.enum_name_of_expr(expr)),
            ExprKind::Path { .. } => self.enum_name_of_expr(expr),
            _ => None,
        }?;
        self.shared_types.get(&type_name).map(|i| i.heap_type)
    }

    /// B-2026-07-01-7 passthrough guard — whether the free-fn callee
    /// `callee_name`'s body can RETURN its parameter `arg_index`
    /// (`crate::ast::fn_returns_param` — any bare-param return site counts,
    /// conservative toward skipping). Resolved from the program snapshot's
    /// top-level functions; unknown callees (externs, builtins) → `false`
    /// (register — the status-quo caller-drops convention).
    pub(super) fn call_arg_flows_into_return(&self, callee_name: &str, arg_index: usize) -> bool {
        let Some(program) = self.program_snapshot.as_deref() else {
            return false;
        };
        program.items.iter().any(|item| {
            matches!(item, crate::ast::Item::Function(f)
                if f.name == callee_name && crate::ast::fn_returns_param(f, arg_index))
        })
    }

    /// B-2026-07-01-7 (discard position): register the caller-side
    /// UserDrop for a DISCARDED statement-position fn result whose
    /// declared return type has a user `impl Drop` (`make();` — silent on
    /// both surfaces before this). Type-gated exactly like the arg-temp
    /// arm; shared types stay with the rc machinery.
    pub(super) fn try_track_discarded_user_drop_temp(
        &mut self,
        tail: &Expr,
        val: BasicValueEnum<'ctx>,
    ) {
        let ExprKind::Call { callee, .. } = &tail.kind else {
            return;
        };
        let ExprKind::Identifier(fn_name) = &callee.kind else {
            return;
        };
        let Some(ret_ty_name) = self.fn_return_type_names.get(fn_name).cloned() else {
            return;
        };
        let has_user_drop = self
            .program_snapshot
            .as_deref()
            .map(|p| p.drop_method_keys.contains_key(&ret_ty_name))
            .unwrap_or(false);
        if !has_user_drop || self.shared_types.contains_key(&ret_ty_name) {
            return;
        }
        let is_enum = self.enum_layouts.contains_key(&ret_ty_name);
        if !is_enum && !self.struct_types.contains_key(&ret_ty_name) {
            return;
        }
        let inkwell::types::BasicTypeEnum::StructType(agg_ty) = val.get_type() else {
            return;
        };
        let Some(cur_fn) = self.current_fn else {
            return;
        };
        let slot = self.create_entry_alloca(cur_fn, "__owned_agg_tmp", agg_ty.into());
        self.builder.build_store(slot, val).unwrap();
        self.track_user_drop_var(&ret_ty_name, "__owned_agg_tmp", slot);
        if is_enum && self.enum_has_heap_payload(&ret_ty_name) {
            self.track_enum_var(&ret_ty_name, slot);
        }
    }

    /// Register the caller-side drop for an inline owned-**aggregate** call
    /// argument — a fresh temp with no consuming binding that the callee owns
    /// by deep-copy (`make_aggregate_param_callee_owned`, the #14 model: the
    /// callee copies the heap payload at entry and frees only its own copy, so
    /// the caller still owns the argument temp and must drop it). A let-bound
    /// aggregate gets this drop at its binding site; an inline temp had no
    /// owner and leaked its heap payload. Shared by the free-function
    /// (`compile_call`) and method (`compile_method_call`) arg loops.
    ///
    /// Two shapes:
    ///   * **enum-variant constructor** (`f(Tok.V(mk()))`,
    ///     `make_spanned(Token.StringLiteral(value))`) — B-2026-06-12-10, the
    ///     dominant self-hosted-lexer leak (every `Token.<StringVariant>(…)`
    ///     plus the nested `InterpolatedStringLiteral(Vec[InterpPart])`). Enums
    ///     lower to flat `iN` words, so the LLVM-type `aggregate_has_heap_field`
    ///     check can't see the String/Vec payload — gate on the SOURCE-level
    ///     `enum_has_heap_payload`. Restricted to a `Call` (a fresh variant
    ///     constructor): an enum *identifier* arg is an existing tracked binding
    ///     and re-tracking it would double-free. `enum_name_of_expr` returns
    ///     `Some` only for a real variant constructor (a plain enum-returning fn
    ///     call → `None`), and `track_enum_var` self-filters shared (RC) enums —
    ///     so this neither double-frees a callee-balanced RC enum nor bloats a
    ///     unit-variant arg.
    ///   * **tuple / named-struct literal** (`show((2, f"z"))`,
    ///     `show(S { name: f"z" })`) — B-2026-06-11-4 part b; these keep their
    ///     heap fields as recognizable Vec/String LLVM types, so the
    ///     `aggregate_has_heap_field` gate applies.
    pub(super) fn track_inline_owned_aggregate_arg(
        &mut self,
        val: BasicValueEnum<'ctx>,
        arg: &Expr,
    ) {
        let inkwell::types::BasicTypeEnum::StructType(agg_ty) = val.get_type() else {
            return;
        };
        if agg_ty == self.vec_struct_type() || self.current_fn.is_none() {
            return;
        }
        let cur_fn = self.current_fn.unwrap();
        // Fresh enum-variant temp shapes: `E.V(args)` / bare-ctor `V(args)`
        // (Call), unit variant `E.V` (Path), and struct variant `E.V { .. }`
        // (StructLiteral whose enum owner `enum_name_of_expr` recognizes; a
        // plain struct literal yields `None` and falls to the struct arm
        // below). `Identifier` args are deliberately NOT matched — a
        // let-bound enum's drop is owned by its binding (let-path), and the
        // arg-pass move-suppression handles the transfer.
        // Fn-call-RETURNED Drop temp (B-2026-07-01-7): `consume(make())`
        // where `make() -> Guard`/`-> Sig` and the type has a user Drop —
        // `enum_name_of_expr`'s Call arm resolves only VARIANT ctors, so a
        // plain fn call matched nothing and the user body never fired.
        // Resolve the producing fn's return type; register the same
        // caller-side UserDrop the ctor arms use (the wrapper also runs
        // the struct field cleanup; enums get the dual EnumDrop payload
        // walk). Shared types stay with the rc machinery; the passthrough
        // guard at the call sites already skipped flow-through args.
        if let ExprKind::Call { callee, .. } = &arg.kind {
            if let ExprKind::Identifier(fn_name) = &callee.kind {
                if let Some(ret_ty_name) = self.fn_return_type_names.get(fn_name).cloned() {
                    let has_user_drop = self
                        .program_snapshot
                        .as_deref()
                        .map(|p| p.drop_method_keys.contains_key(&ret_ty_name))
                        .unwrap_or(false);
                    if has_user_drop && !self.shared_types.contains_key(&ret_ty_name) {
                        let is_enum = self.enum_layouts.contains_key(&ret_ty_name);
                        let is_struct = self.struct_types.contains_key(&ret_ty_name);
                        if is_enum || is_struct {
                            let slot =
                                self.create_entry_alloca(cur_fn, "__owned_agg_tmp", agg_ty.into());
                            self.builder.build_store(slot, val).unwrap();
                            self.track_user_drop_var(&ret_ty_name, "__owned_agg_tmp", slot);
                            if is_enum && self.enum_has_heap_payload(&ret_ty_name) {
                                self.track_enum_var(&ret_ty_name, slot);
                            }
                            return;
                        }
                    }
                }
            }
        }
        let fresh_enum_temp = match &arg.kind {
            ExprKind::Call { .. } | ExprKind::Path { .. } | ExprKind::StructLiteral { .. } => {
                self.enum_name_of_expr(arg)
            }
            _ => None,
        };
        if let Some(enum_name) = fresh_enum_temp {
            // B-2026-06-10 carry-forward (enum arm): a Drop-typed enum
            // temporary materialized directly as a call argument
            // (`consume(Sig.A(1))` where `Sig: Drop`) is caller-owned,
            // exactly like the struct-literal case below — but this arm
            // only ever registered the payload-walking `EnumDrop`, so
            // the user `drop` body never fired (and a heap-FREE enum
            // registered nothing at all). Mirror the let-path
            // (`stmts.rs` — `var_type_names` → `track_user_drop_var`):
            // register the `karac_drop_<Enum>` wrapper when the enum
            // has a validated user Drop and isn't shared (shared enums
            // run the body via the RC path, `emit_shared_enum_rc_drop_fn`).
            // Unlike the struct case, UserDrop and EnumDrop are
            // COMPLEMENTARY here, not mutually exclusive: the wrapper's
            // field-cleanup half (`emit_struct_drop_synthesis`) is a
            // no-op for enum type names, so the payload walk must still
            // be registered separately — the same dual registration the
            // let-path produces (`karac_drop_E` + `__karac_drop_E` on
            // the same slot). Coroutine-compiled callees never reach
            // this helper (early return upstream), so no double-drop.
            let has_user_drop = self
                .program_snapshot
                .as_deref()
                .map(|p| p.drop_method_keys.contains_key(&enum_name))
                .unwrap_or(false);
            let user_drop = has_user_drop && !self.shared_types.contains_key(&enum_name);
            let heap_payload = self.enum_has_heap_payload(&enum_name);
            if user_drop || heap_payload {
                let slot = self.create_entry_alloca(cur_fn, "__owned_agg_tmp", agg_ty.into());
                self.builder.build_store(slot, val).unwrap();
                if user_drop {
                    self.track_user_drop_var(&enum_name, "__owned_agg_tmp", slot);
                }
                if heap_payload {
                    self.track_enum_var(&enum_name, slot);
                }
            }
        } else if let ExprKind::Tuple(tuple_elems) = &arg.kind {
            // #21 — a tuple LITERAL arg. The callee now entry-copies a
            // heap-bearing tuple param (`make_tuple_param_callee_owned`), so this
            // caller temp is an INDEPENDENT buffer that must free its own heap.
            // The LLVM-type `track_tuple_var` is enum-blind, so derive the
            // element `TypeExpr`s from the literal and register a `TypeExpr`-driven
            // drop when any leaf is an enum / nested struct; fall back to the
            // enum-blind path for a pure Vec/String tuple (its layout is visible).
            let elem_tes: Vec<TypeExpr> = tuple_elems
                .iter()
                .map(|e| self.infer_arg_elem_te(e))
                .collect();
            if elem_tes.iter().any(|e| self.type_expr_has_drop_heap(e)) {
                let slot = self.create_entry_alloca(cur_fn, "__owned_agg_tmp", agg_ty.into());
                self.builder.build_store(slot, val).unwrap();
                if let Some(drop_fn) = self.synthesize_tuple_drop_fn_te(agg_ty, &elem_tes) {
                    if let Some(frame) = self.scope_cleanup_actions.last_mut() {
                        frame.push(super::state::CleanupAction::StructDrop {
                            struct_alloca: slot,
                            drop_fn,
                        });
                    }
                }
            } else if self.aggregate_has_heap_field(agg_ty) {
                let slot = self.create_entry_alloca(cur_fn, "__owned_agg_tmp", agg_ty.into());
                self.builder.build_store(slot, val).unwrap();
                self.track_tuple_var(slot, agg_ty);
            }
        } else if let ExprKind::StructLiteral { path, .. } = &arg.kind {
            if let Some(name) = path
                .last()
                .filter(|n| self.struct_types.contains_key(n.as_str()))
                .cloned()
            {
                // Register the caller-temp's struct drop when the struct carries
                // heap. A DIRECT Vec/String field is LLVM-visible
                // (`aggregate_has_heap_field`) and registered on the proven path —
                // unconditionally, since whenever its drop frees a buffer the
                // callee either entry-copies (independent) or caller-retains
                // (shares, never frees). But an ENUM / nested-struct leaf is
                // INVISIBLE to that check — the payload is all-i64 words, no
                // `vec_struct_type` field — so an enum-leaf struct
                // (`W { tok: Tok }`) constructed inline at the call site slipped
                // through and leaked its enum payload once per call (#22, the #19
                // fresh-temp tail). Add a SOURCE-level gate for that case,
                // restricted to copy-supported structs: the callee then provably
                // entry-copies (`make_aggregate_param_callee_owned`), so this
                // caller temp is an INDEPENDENT buffer and its drop frees a
                // distinct heap — never the callee's. A not-copy-supported struct
                // (Map / shared / Option leaf) stays caller-retains in the callee
                // and could be consumed internally, so registering a caller drop
                // would risk a double-free; leave it a (safe) leak, matching the
                // param-copy policy ("better to leak than double-free").
                // B-2026-06-10 — a Drop-typed temporary materialized DIRECTLY
                // as a call argument (`consume(Guard { id: 1 })` where
                // `Guard: Drop`) is caller-owned under the caller-drops
                // convention (`param_own.rs`), exactly like a let-bound arg.
                // The let-path (`stmts.rs`) routes such a binding through
                // `track_user_drop_var`, whose `karac_drop_<T>` wrapper runs
                // the user `drop` body at scope exit. This inline-temp path
                // only ever registered `track_struct_var` (a field-free walk
                // that never runs the user body) — and for a heap-free struct
                // it registered NOTHING, because the `llvm_heap ||
                // src_heap_copyable` gate is false AND `emit_struct_drop_-
                // synthesis` returns `None`. So the user `drop` never fired
                // and the temporary leaked. Mirror the let-path: when the type
                // has a validated user Drop (and isn't shared — those drop via
                // the RC path, `stmts.rs:3021`), register exactly ONE UserDrop,
                // materializing a slot even with no heap fields, since the user
                // body has observable side effects regardless of heap content.
                // UserDrop and StructDrop are mutually exclusive (the wrapper
                // calls `__karac_drop_struct_<T>` internally, so registering
                // both double-walks fields). Coroutine-compiled callees can't
                // double-drop here — they return early above (the
                // `is_coroutine_compiled` arm) and never reach this helper.
                let has_user_drop = self
                    .program_snapshot
                    .as_deref()
                    .map(|p| p.drop_method_keys.contains_key(&name))
                    .unwrap_or(false);
                if has_user_drop && !self.shared_types.contains_key(&name) {
                    let slot = self.create_entry_alloca(cur_fn, "__owned_agg_tmp", agg_ty.into());
                    self.builder.build_store(slot, val).unwrap();
                    self.track_user_drop_var(&name, "__owned_agg_tmp", slot);
                    return;
                }
                let llvm_heap = self.aggregate_has_heap_field(agg_ty);
                let src_heap_copyable = !llvm_heap
                    && self.aggregate_param_copy_supported_struct(&name, &mut Vec::new())
                    && (self
                        .struct_field_type_exprs
                        .get(&name)
                        .is_some_and(|ftes| ftes.iter().any(|f| self.type_expr_has_drop_heap(f)))
                        // B-2026-07-03-28 shared leg — a copy-supported struct
                        // whose only heap is a `shared` / `Option[shared]` field is
                        // INVISIBLE to `type_expr_has_drop_heap` (it reports false
                        // for RC leaves), so an inline fresh-temp arg
                        // (`f(Holder { value: Some(shared) })`) registered NO
                        // cleanup and leaked the box: the callee entry-copies
                        // (rc-INC) but the caller temp's ref was never rc-dec'd.
                        // The callee provably entry-copies a copy-supported struct,
                        // so this caller temp is an independent ref — register its
                        // combined drop (`track_struct_var` routes shared-owning
                        // structs through the rc-dec walker). Symmetric: caller temp
                        // dec + callee copy dec == create + entry-copy inc.
                        || self.struct_owns_shared_field(&name, &mut Vec::new()));
                // B-2026-07-04-9(b) — a struct with a DIRECT `shared` field
                // (`DirH { value: Val }`) is NOT copy-supported (`field_copy_-
                // supported` bails on a direct shared field), so `src_heap_-
                // copyable` above stays off and, as an INLINE fresh-temp arg
                // (`borrow_dir(DirH { value: Val.Ident(..) })`), it registered NO
                // caller-temp drop — while the caller-retains param doesn't drop
                // it either, so the box leaked. Such a struct is caller-retains
                // (the callee never entry-copies a non-copy-supported struct), so
                // the caller temp is its SOLE owner: register the combined drop
                // (`track_struct_var` routes a shared-owning struct through the
                // rc-dec walker — a pure rc-dec, no buffer copy needed). The
                // `let`-bound sibling (`let d = DirH { .. }; f(d)`) is already
                // covered by `track_struct_var` at its binding site.
                let src_shared_owning = !llvm_heap
                    && !src_heap_copyable
                    && self.struct_owns_shared_field(&name, &mut Vec::new());
                if llvm_heap || src_heap_copyable || src_shared_owning {
                    let slot = self.create_entry_alloca(cur_fn, "__owned_agg_tmp", agg_ty.into());
                    self.builder.build_store(slot, val).unwrap();
                    self.track_struct_var(&name, slot);
                }
            }
        }
    }

    /// Does the USER program define a function with this name, shadowing the
    /// `std.mem` `#[compiler_builtin]` of the same name? `swap` / `replace` are
    /// common names (`fn swap[T](a, b) -> (T, T)` is a legal user helper), so
    /// the call-site intercept must defer to a user definition. The stdlib
    /// builtins are compiler-intrinsic (never seeded into `generic_fns` nor
    /// declared as a module function), so a hit in EITHER means the user owns
    /// the name — fall through to the normal generic/concrete call path.
    fn user_shadows_mem_builtin(&self, name: &str) -> bool {
        self.generic_fns.contains_key(name) || self.module.get_function(name).is_some()
    }

    /// Resolve a `mut ref` place argument of a `std.mem` builtin (`swap` /
    /// `replace`) to `(place_ptr, loaded_value)` — the address to store the new
    /// value into, and the current value already loaded from it. Handles the
    /// place forms the call-site `mut` marker admits. An OWNED IDENTIFIER
    /// (`swap(mut a, ..)`) has `slot.ptr` as the alloca that holds `T` directly.
    /// A FORWARDED `mut ref` PARAM (`swap(x, ..)` inside `fn f(x: mut ref T)`)
    /// has `slot.ptr` as an alloca HOLDING the pointer-to-`T`, so it is loaded
    /// once to reach the real place — mirroring `load_variable`'s ref-param
    /// double-deref; a raw `field_chain_place_ptr` would return the pointer-slot
    /// itself and corrupt it. A FIELD / INDEX / SELF place (`swap(mut s.x, ..)`)
    /// takes the value via a fresh load and the store target via
    /// `field_chain_place_ptr`. Errors (rather than silently miscompiling) on an
    /// unsupported shape.
    fn mem_place_ptr_and_value(
        &mut self,
        expr: &Expr,
    ) -> Result<(PointerValue<'ctx>, BasicValueEnum<'ctx>), String> {
        if let ExprKind::Identifier(name) = &expr.kind {
            if let Some(slot) = self.variables.get(name.as_str()) {
                let (slot_ptr, slot_ty) = (slot.ptr, slot.ty);
                if let Some(&inner_ty) = self.ref_params.get(name.as_str()) {
                    let ptr_ty = self.context.ptr_type(AddressSpace::default());
                    let place = self
                        .builder
                        .build_load(ptr_ty, slot_ptr, &format!("{name}.mem.place"))
                        .unwrap()
                        .into_pointer_value();
                    let val = self
                        .builder
                        .build_load(inner_ty, place, &format!("{name}.mem.val"))
                        .unwrap();
                    return Ok((place, val));
                }
                let val = self
                    .builder
                    .build_load(slot_ty, slot_ptr, &format!("{name}.mem.val"))
                    .unwrap();
                return Ok((slot_ptr, val));
            }
        }
        let val = self.compile_expr(expr)?;
        let ptr = self.field_chain_place_ptr(expr).ok_or_else(|| {
            "std.mem swap/replace: unsupported `mut ref` place expression \
             (expected an identifier, struct field, or index place)"
                .to_string()
        })?;
        Ok((ptr, val))
    }

    /// B-2026-07-08-6 — does a STRUCT-LITERAL argument have a type the callee
    /// ENTRY-COPIES (`make_aggregate_param_callee_owned`'s struct arm)? True
    /// only for a non-shared, copy-supported struct that owns heap content, so
    /// the callee deep-copies its fields at entry and RETURNS an independent
    /// copy — meaning the return-passthrough guard must NOT suppress the
    /// caller's drop of the original moved-in temp (else it leaks). Mirrors the
    /// exact predicate the callee uses, so caller and callee stay in lockstep:
    /// a forwarded (non-copy) param — bare String/Vec, `Map`/shared/`Option`
    /// non-copyable field, user-`Drop` via a `Call` arg — yields `false`, and
    /// the passthrough guard's skip (B-2026-07-01-7) is preserved. Restricted
    /// to struct literals: an identifier arg's drop is owned by its binding
    /// (the arg-pass move-suppression handles the transfer), matching
    /// `track_inline_owned_aggregate_arg`'s own scope.
    pub(super) fn arg_is_entry_copied_heap_struct(&self, arg: &Expr) -> bool {
        let ExprKind::StructLiteral { path, .. } = &arg.kind else {
            return false;
        };
        // An enum struct-variant literal (`E.V { .. }`) forwards through the
        // enum arm, not the struct entry-copy — exclude it.
        if self.enum_name_of_expr(arg).is_some() {
            return false;
        }
        let Some(name) = path.last() else {
            return false;
        };
        self.struct_types.contains_key(name.as_str())
            && !self.shared_types.contains_key(name.as_str())
            && self.aggregate_param_copy_supported_struct(name, &mut Vec::new())
            && self
                .struct_field_type_exprs
                .get(name.as_str())
                .is_some_and(|ftes| ftes.iter().any(|f| self.type_expr_has_drop_heap(f)))
    }

    /// #21 — best-effort `TypeExpr` for a tuple-literal arg ELEMENT, so its
    /// caller-temp gets an enum-aware drop (the LLVM type is enum-blind). A
    /// nested tuple recurses; otherwise infer the element's type NAME
    /// (enum-constructor / value type) and wrap it in a single-segment Path.
    /// An unresolved name yields an empty Path, which `type_expr_has_drop_heap`
    /// treats as no-drop — safe (worst case a missed free degrades to the
    /// pre-existing enum-blind leak, never a double-free).
    pub(super) fn infer_arg_elem_te(&self, e: &Expr) -> TypeExpr {
        if let ExprKind::Tuple(inner) = &e.kind {
            return TypeExpr {
                kind: TypeKind::Tuple(inner.iter().map(|x| self.infer_arg_elem_te(x)).collect()),
                span: e.span.clone(),
            };
        }
        let name = self
            .enum_name_of_expr(e)
            .or_else(|| self.type_name_of(e))
            .unwrap_or_default();
        TypeExpr {
            kind: TypeKind::Path(crate::ast::PathExpr {
                segments: vec![name],
                generic_args: None,
                span: e.span.clone(),
            }),
            span: e.span.clone(),
        }
    }

    /// Niche-ABI arg pack: positions the callee declares as a nullable
    /// ptr (`Option[shared T]` under `fn_niche_abi`) receive the packed
    /// pointer instead of the conventional 4-i64 Option struct. Must run
    /// AFTER the caller's refcount bookkeeping
    /// (`share_option_shared_ref_for_arg` & co.) so that operated on the
    /// conventional shape; the pack is value-only and count-neutral —
    /// the callee's +1 travels through the pointer unchanged. Positions
    /// are 1:1 with the callee's declared params: free-fn call sites
    /// push one entry per source arg, method sites push the receiver at
    /// 0 (`self` — never an Option, so never a niche position) then the
    /// source args. No-op for callees without a `fn_niche_abi` record
    /// (closures, monos, builtins, extern decls).
    pub(super) fn pack_niche_abi_args(
        &self,
        callee: &str,
        compiled_args: &mut [BasicMetadataValueEnum<'ctx>],
    ) {
        let Some(abi) = self.fn_niche_abi.get(callee) else {
            return;
        };
        let positions: Vec<usize> = abi
            .params
            .iter()
            .enumerate()
            .filter_map(|(i, &n)| n.then_some(i))
            .collect();
        for i in positions {
            if let Some(slot) = compiled_args.get_mut(i) {
                if let BasicMetadataValueEnum::StructValue(sv) = *slot {
                    let packed = self.option_value_to_niche_ptr(sv.into());
                    *slot = packed.into();
                }
            }
        }
    }

    /// Niche-ABI result unpack: a callee returning `Option[shared T]` as
    /// a nullable ptr is rebuilt into the conventional 4-i64 Option
    /// struct, so every downstream consumer (let-binding `RcDecOption`
    /// registration via `fn_return_option_inner_shared`, pattern matches,
    /// `?`, re-returns) is shape-blind to the ABI. Pass-through for
    /// callees without a niche return.
    pub(super) fn unpack_niche_abi_ret(
        &self,
        callee: &str,
        v: BasicValueEnum<'ctx>,
    ) -> BasicValueEnum<'ctx> {
        if self.fn_niche_abi.get(callee).is_some_and(|abi| abi.ret) {
            return self.niche_ptr_to_option_value(v.into_pointer_value(), "call.niche");
        }
        v
    }

    /// Lower a diverging prelude builtin (`todo()` / `unreachable()`, type
    /// `!`). Prints a panic message and `exit(1)` via `emit_panic`, then
    /// terminates the current block with an `unreachable` instruction so the
    /// caller's terminator-guarded paths (`compile_block` between statements,
    /// `if`/`match` branch merges, and the function-tail `ret` in
    /// `compile_function`) all skip emitting a follow-on instruction. This is
    /// what fixes `fn boom() -> T { unreachable() }`: without the terminator,
    /// the tail logic emitted `ret i64 0` (the placeholder this used to
    /// return) against `T`'s real LLVM type, failing module verification.
    ///
    /// Message parity with the interpreter's `eval_builtin_diverge`: default
    /// `"not yet implemented"` (todo) / `"entered unreachable code"`
    /// (unreachable), with a literal argument folded in as
    /// `"<default>: <msg>"`. `emit_panic` takes a compile-time `&str`, so a
    /// non-literal (runtime-valued) argument — rare for these builtins —
    /// degrades to the bare default message rather than threading a runtime
    /// string through the panic printf.
    fn compile_diverge(
        &mut self,
        name: &str,
        args: &[CallArg],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let default_msg = if name == "todo" {
            "not yet implemented"
        } else {
            "entered unreachable code"
        };
        let full_msg = match args.first().map(|a| &a.value.kind) {
            Some(ExprKind::StringLit(s)) => format!("{}: {}", default_msg, s),
            _ => default_msg.to_string(),
        };
        self.emit_panic(&full_msg);
        self.builder.build_unreachable().unwrap();
        // Placeholder value: the block is now terminated, so every value-
        // consuming caller respects the terminator guard and never reads it.
        Ok(self.context.i64_type().const_int(0, false).into())
    }

    /// Phase-7 line 5 sub-item 1 — lower a call to a hot-swap-slotted
    /// callee as load-from-table + indirect call. `func` carries the
    /// FunctionType the indirect call must use (signatures match the
    /// declared symbol regardless of the indirection); `slot` indexes
    /// into `@karac_hotswap_table` (`[N x ptr]`, populated by the
    /// ctor emitted in `finalize_hot_swap_table`).
    pub(super) fn build_hot_swap_indirect_call(
        &mut self,
        func: FunctionValue<'ctx>,
        slot: u32,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> CallSiteValue<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_ty = self.context.i64_type();
        let n = self.hot_swap_fns.len() as u32;
        let arr_ty = ptr_ty.array_type(n);
        let table = self
            .module
            .get_global("karac_hotswap_table")
            .expect("pre_emit_hot_swap_table must run before body lowering");
        let gep = unsafe {
            self.builder.build_in_bounds_gep(
                arr_ty,
                table.as_pointer_value(),
                &[
                    i64_ty.const_int(0, false),
                    i64_ty.const_int(slot as u64, false),
                ],
                &format!("hotswap_slot_{slot}"),
            )
        }
        .unwrap();
        let loaded = self
            .builder
            .build_load(ptr_ty, gep, "hotswap_fnp")
            .unwrap()
            .into_pointer_value();
        self.builder
            .build_indirect_call(func.get_type(), loaded, args, "hotswap_call")
            .unwrap()
    }

    /// Try to construct an enum variant value if `name` matches a known variant.
    /// Returns `None` if `name` is not an enum variant.
    pub(super) fn try_compile_enum_variant(
        &mut self,
        name: &str,
        enum_name_override: Option<&str>,
        args: &[CallArg],
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        // Find which enum this variant belongs to. When the caller already
        // knows the enum (the qualified `Enum.Variant(args)` form in
        // `compile_assoc_call`), `enum_name_override` carries it — use it
        // verbatim rather than re-resolving by bare variant name, which is
        // ambiguous when the name collides across enums (`Other` is shared by
        // the seeded `IoError` / `Utf8Error` / `TcpError` / `TlsError`, so the
        // bare-name resolution below would pick one by HashMap order and write
        // the wrong tag — the B-2026-06-14 baked-enum companion bug).
        //
        // For the bare-name path (`Variant(args)` from `compile_call`): prefer
        // user-declared enums over the seeded built-ins (`Option`, `Result`,
        // `Json`, `TcpError`, …) when a variant name collides — without this
        // preference, HashMap iteration order non-deterministically picks a
        // seeded layout for a user-defined variant with the same name (e.g.
        // `MyIoErr.Other` vs the seeded `TcpError.Other`), producing a
        // wrong-shape value at the constructor site and emitting `unreachable`
        // for downstream dispatch. The 2026-05-25 codegen-suite hang
        // investigation surfaced the original hard-coded `Option`/`Result`
        // workaround missing the newer `Json` and `TcpError` seeds — replaced
        // with the `seeded_enum_names` set so any future seeded enum is
        // classified correctly without per-name maintenance. Symmetric to the
        // destructure disambiguation in `bind_pattern_values`.
        let enum_name = match enum_name_override {
            Some(en)
                if self
                    .enum_layouts
                    .get(en)
                    .is_some_and(|l| l.tags.contains_key(name)) =>
            {
                Some(en.to_string())
            }
            _ => {
                let mut user_match: Option<String> = None;
                let mut seed_match: Option<String> = None;
                for (en, layout) in &self.enum_layouts {
                    if layout.tags.contains_key(name) {
                        if self.seeded_enum_names.contains(en) {
                            seed_match.get_or_insert_with(|| en.clone());
                        } else {
                            user_match.get_or_insert_with(|| en.clone());
                        }
                    }
                }
                user_match.or(seed_match)
            }
        };

        let enum_name = match enum_name {
            Some(n) => n,
            None => return Ok(None),
        };

        let (tag, llvm_type) = {
            let layout = &self.enum_layouts[&enum_name];
            (*layout.tags.get(name).unwrap(), layout.llvm_type)
        };

        let i64_t = self.context.i64_type();

        // Shared enum: heap-allocate with refcount header.
        if let Some(info) = self.shared_types.get(&enum_name).cloned() {
            let ptr = self.emit_rc_alloc(info.heap_type);
            // Tag at heap index 1 (index 0 is refcount).
            let tag_ptr = self
                .builder
                .build_struct_gep(info.heap_type, ptr, 1, "sh_tag")
                .unwrap();
            self.builder
                .build_store(tag_ptr, i64_t.const_int(tag, false))
                .unwrap();
            // Payload words at heap indices 2, 3, … . Shared enums share
            // the same per-variant `field_word_offsets` layout as
            // non-shared enums; the heap struct's payload-word count is
            // sized to `max_payload_words` at declare time. Each source
            // field decomposes into its assigned word range.
            let offsets: Vec<(usize, usize)> = self.enum_layouts[&enum_name]
                .field_word_offsets
                .get(name)
                .cloned()
                .unwrap_or_default();
            for (i, arg) in args.iter().enumerate() {
                let val = self.compile_expr(&arg.value)?;
                // F-string payload (`Some(f"…")`): disarm the staged
                // accumulator cleanup — the enum's drop owns the buffer
                // now. Owned String/Vec PARAM payload (`Some(s)` where
                // `s: String` is a parameter): deep-copy, the caller
                // retains the free (kata-22 family, 2026-06-06).
                self.suppress_fstr_acc_if_moved_out(&arg.value);
                let val = self.maybe_defensive_copy_param_arg(&arg.value, val);
                // #226 (B-2026-06-15): a `Variant(nodes[i])` payload reading a
                // bare-`shared` Vec element is aliased, not moved — inc so the
                // new enum owns its own ref (else freed when the Vec drops).
                self.share_bare_shared_ctor_payload(&arg.value, val);
                let (start_word, num_words) = offsets.get(i).copied().unwrap_or((i, 1));
                let words = self.coerce_to_payload_words(val, num_words)?;
                for (j, w) in words.into_iter().enumerate() {
                    let word_ptr = self
                        .builder
                        .build_struct_gep(
                            info.heap_type,
                            ptr,
                            (start_word + j + 2) as u32, // +2 for refcount + tag
                            "sh_word",
                        )
                        .unwrap();
                    self.builder.build_store(word_ptr, w).unwrap();
                }
                // Phase 7.2 Slice DP — move-suppression for the source
                // binding when the arg is an Identifier referencing a
                // tracked Vec/String variable. Zeroing the source's
                // `cap` field neutralizes the existing
                // `FreeVecBuffer` cleanup at scope exit (it's gated
                // on `cap > 0`), preventing a double-free against the
                // payload buffer the new enum binding now owns. See
                // `suppress_source_vec_cleanup_for_arg` for the
                // shape-detection path.
                self.suppress_source_vec_cleanup_for_arg(&arg.value);
                // Boxed / inline-heap `Option`/`Result` binding moved whole into
                // this shared tuple-variant payload — mirrors the struct-literal
                // / struct-variant field-init paths.
                self.suppress_inline_option_result_binding_move(&arg.value);
                // Map/Set sibling of the Vec suppression: a `Map`/`Set`
                // local moved into this variant hands its handle to the
                // enum payload, so drop the source's scope-exit
                // `FreeMapHandle` — otherwise the source frees the handle
                // the returned enum now carries downstream (the
                // struct-literal UAF — phase-6 line 561 — for enum
                // variants). Set/Map share `FreeMapHandle`; mirrors the
                // struct-literal fix in `exprs.rs`.
                if let ExprKind::Identifier(n) = &arg.value.kind {
                    let n = n.clone();
                    self.suppress_map_cleanup_for_tail_identifier(&n);
                }
            }
            return Ok(Some(ptr.into()));
        }

        // Non-shared enum: stack-allocated aggregate. Zero-init so unused
        // payload words stay `0` (sound word-wise `==`; see build_nonshared).
        let mut agg = llvm_type.const_zero();

        // Store tag as field 0
        agg = self
            .builder
            .build_insert_value(agg, i64_t.const_int(tag, false), 0, "tag")
            .unwrap()
            .into_struct_value();

        // Compound-payload enum codegen (CP4): consult the variant's
        // `field_word_offsets` so each source field's value is written
        // into its assigned word range (start_word .. start_word +
        // num_words). Multi-word aggregates (String / Vec / user
        // structs / tuples) decompose to a sequence of i64 words via
        // `coerce_to_payload_words`; primitives produce a single word
        // and match the legacy `coerce_to_i64` path. Reading back is
        // the destructure path's job (see `bind_pattern_values`).
        let offsets: Vec<(usize, usize)> = self.enum_layouts[&enum_name]
            .field_word_offsets
            .get(name)
            .cloned()
            .unwrap_or_default();
        for (i, arg) in args.iter().enumerate() {
            let val = self.compile_expr(&arg.value)?;
            // Same consume-site ownership pair as the shared-enum branch
            // above: f-string payloads move in (disarm the staged acc
            // cleanup); owned String/Vec PARAM payloads deep-copy (the
            // caller retains the free). Kata-22 family, 2026-06-06.
            self.suppress_fstr_acc_if_moved_out(&arg.value);
            let val = self.maybe_defensive_copy_param_arg(&arg.value, val);
            // #226 (B-2026-06-15): `Some(nodes[i])` — a bare-`shared` Vec
            // element read is aliased, not moved; inc so the Option owns its
            // own ref (else freed when the source Vec drops).
            self.share_bare_shared_ctor_payload(&arg.value, val);
            let (start_word, num_words) = offsets.get(i).copied().unwrap_or((i, 1)); // legacy fallback if layout missing
            let words = self.coerce_to_payload_words(val, num_words)?;
            for (j, w) in words.into_iter().enumerate() {
                agg = self
                    .builder
                    .build_insert_value(
                        agg,
                        w,
                        (start_word + j + 1) as u32, // +1 for tag field
                        "word",
                    )
                    .unwrap()
                    .into_struct_value();
            }
            // Phase 7.2 Slice DP — move-suppression. Same shape as the
            // shared-enum branch above; zero the source binding's
            // `cap` so its scope-exit `FreeVecBuffer` becomes a no-op.
            // The new enum binding owns the buffer.
            self.suppress_source_vec_cleanup_for_arg(&arg.value);
            // Boxed / inline-heap `Option`/`Result` binding moved whole into
            // this non-shared tuple-variant payload — see the shared-enum
            // branch above and the struct-literal field-init paths.
            self.suppress_inline_option_result_binding_move(&arg.value);
            // Map/Set sibling of the Vec suppression — see the shared-enum
            // branch above. A `Map`/`Set` local moved into this variant
            // hands its handle to the enum payload, so drop the source's
            // scope-exit `FreeMapHandle` (the struct-literal UAF for enum
            // variants; Set/Map share `FreeMapHandle`).
            if let ExprKind::Identifier(n) = &arg.value.kind {
                let n = n.clone();
                self.suppress_map_cleanup_for_tail_identifier(&n);
            }
        }

        Ok(Some(agg.into()))
    }

    /// Construct a non-shared enum-variant aggregate value from already-
    /// compiled payload values (the value-level analog of
    /// `try_compile_enum_variant`, which compiles `Expr` args). Used where
    /// codegen synthesizes an enum from runtime-produced SSA values rather
    /// than source expressions — e.g. building `Result.Ok(<runtime String>)`
    /// / `Result.Err(VarError.NotPresent)` for the `env.var` ambient lowering
    /// (L646 slice 3a).
    ///
    /// MUST stay in lockstep with the non-shared tail of
    /// `try_compile_enum_variant`: same tag-at-field-0 + per-field
    /// `field_word_offsets` + `coerce_to_payload_words` layout. Restricted to
    /// non-shared enums (the seeded `Result` / `VarError` / `Option` family
    /// is never `shared`); a shared enum would need the heap-alloc + refcount
    /// path and is rejected with an error rather than mis-lowered.
    pub(super) fn build_nonshared_enum_value(
        &mut self,
        enum_name: &str,
        variant: &str,
        payload_vals: &[BasicValueEnum<'ctx>],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let layout = self.enum_layouts.get(enum_name).ok_or_else(|| {
            format!("build_nonshared_enum_value: no layout for enum `{enum_name}` (codegen bug)")
        })?;
        if layout.is_shared {
            return Err(format!(
                "build_nonshared_enum_value: `{enum_name}` is a shared enum; \
                 use the heap-alloc construction path (codegen bug)"
            ));
        }
        let tag = *layout.tags.get(variant).ok_or_else(|| {
            format!("build_nonshared_enum_value: enum `{enum_name}` has no variant `{variant}`")
        })?;
        let llvm_type = layout.llvm_type;
        let offsets: Vec<(usize, usize)> = layout
            .field_word_offsets
            .get(variant)
            .cloned()
            .unwrap_or_default();

        let i64_t = self.context.i64_type();
        // Zero-init (not `get_undef`) so a narrower variant's unused payload
        // words stay `0` — keeps the word-wise `==` path sound for unit/scalar-
        // payload enums (an undef payload word made `V::B == V::B` fold to undef).
        let mut agg = llvm_type.const_zero();
        agg = self
            .builder
            .build_insert_value(agg, i64_t.const_int(tag, false), 0, "tag")
            .unwrap()
            .into_struct_value();
        for (i, val) in payload_vals.iter().enumerate() {
            let (start_word, num_words) = offsets.get(i).copied().unwrap_or((i, 1));
            let words = self.coerce_to_payload_words(*val, num_words)?;
            for (j, w) in words.into_iter().enumerate() {
                agg = self
                    .builder
                    .build_insert_value(agg, w, (start_word + j + 1) as u32, "word")
                    .unwrap()
                    .into_struct_value();
            }
        }
        Ok(agg.into())
    }

    /// Declared struct-field names (in order) of `Enum.Variant` when it is a
    /// struct-shaped variant, scanning the user program and the baked stdlib
    /// (so prelude enums like `AllocError` resolve). `None` otherwise. Drives
    /// `compile_enum_struct_variant_init` (mapping named field inits onto the
    /// variant's positional `field_word_offsets`).
    pub(super) fn enum_variant_struct_field_names(
        &self,
        enum_name: &str,
        variant: &str,
    ) -> Option<Vec<String>> {
        fn scan(items: &[Item], enum_name: &str, variant: &str) -> Option<Vec<String>> {
            items.iter().find_map(|item| match item {
                Item::EnumDef(e) if e.name == enum_name => {
                    e.variants.iter().find(|v| v.name == variant).and_then(|v| {
                        if let VariantKind::Struct(fields) = &v.kind {
                            Some(fields.iter().map(|f| f.name.clone()).collect())
                        } else {
                            None
                        }
                    })
                }
                _ => None,
            })
        }
        self.program_snapshot
            .as_ref()
            .and_then(|p| scan(&p.items, enum_name, variant))
            .or_else(|| {
                crate::prelude::STDLIB_PROGRAMS
                    .iter()
                    .find_map(|(_, p)| scan(&p.items, enum_name, variant))
            })
    }

    /// Compile source-level enum struct-variant construction
    /// `Enum.Variant { field: value, ... }` into the seeded enum aggregate.
    /// The struct-variant twin of the tuple-variant constructor: it maps each
    /// *named* field init onto the variant's declared field position and writes
    /// its coerced payload words at that field's `field_word_offsets` slot. The
    /// aggregate is zero-initialized so a narrower variant's unused payload
    /// words stay `0` (keeps the word-wise `==` path sound for unit/scalar-
    /// payload enums). The typechecker (`infer_enum_struct_variant_literal`)
    /// and interpreter (`eval_struct_literal`) route the same shape.
    pub(super) fn compile_enum_struct_variant_init(
        &mut self,
        enum_name: &str,
        variant: &str,
        fields: &[FieldInit],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let field_names = self
            .enum_variant_struct_field_names(enum_name, variant)
            .ok_or_else(|| {
                format!("enum struct-variant `{enum_name}.{variant}` has no known field layout")
            })?;
        let (tag, llvm_type) = {
            let layout = &self.enum_layouts[enum_name];
            (*layout.tags.get(variant).unwrap(), layout.llvm_type)
        };
        let offsets: Vec<(usize, usize)> = self.enum_layouts[enum_name]
            .field_word_offsets
            .get(variant)
            .cloned()
            .unwrap_or_default();
        let i64_t = self.context.i64_type();

        // Shared enum struct-variant: heap-allocate `{ i64 rc, i64 tag,
        // <payload words> }` with a refcount header (B-2026-06-13-8). The
        // named-field twin of `try_compile_enum_variant`'s shared tuple-variant
        // path — tag at heap index 1, payload words at `start_word + j + 2`
        // (+2 for {rc, tag}). Without this the constructor returned the inline
        // `{tag, words}` aggregate for a shared enum too, so a `T.Node { v }`
        // value passed where `T` is the by-pointer shared ABI mismatched (LLVM
        // verifier: "Call parameter type does not match" / `expected
        // PointerValue`).
        if let Some(info) = self.shared_types.get(enum_name).cloned() {
            let ptr = self.emit_rc_alloc(info.heap_type);
            let tag_ptr = self
                .builder
                .build_struct_gep(info.heap_type, ptr, 1, "sh_tag")
                .unwrap();
            self.builder
                .build_store(tag_ptr, i64_t.const_int(tag, false))
                .unwrap();
            for (i, fname) in field_names.iter().enumerate() {
                let init = fields.iter().find(|f| &f.name == fname).ok_or_else(|| {
                    format!("missing field `{fname}` in `{enum_name}.{variant}` construction")
                })?;
                let val = self.compile_expr(&init.value)?;
                self.suppress_fstr_acc_if_moved_out(&init.value);
                let val = self.maybe_defensive_copy_param_arg(&init.value, val);
                let (start_word, num_words) = offsets.get(i).copied().unwrap_or((i, 1));
                let words = self.coerce_to_payload_words(val, num_words)?;
                for (j, w) in words.into_iter().enumerate() {
                    let word_ptr = self
                        .builder
                        .build_struct_gep(
                            info.heap_type,
                            ptr,
                            (start_word + j + 2) as u32, // +2 for refcount + tag
                            "sh_word",
                        )
                        .unwrap();
                    self.builder.build_store(word_ptr, w).unwrap();
                }
                self.suppress_source_vec_cleanup_for_arg(&init.value);
                // Boxed / inline-heap `Option`/`Result` binding moved whole into
                // this shared-enum struct-variant field — mirrors the
                // struct-literal field-init paths (`compile_struct_init`).
                self.suppress_inline_option_result_binding_move(&init.value);
                if let ExprKind::Identifier(n) = &init.value.kind {
                    let n = n.clone();
                    self.suppress_map_cleanup_for_tail_identifier(&n);
                }
            }
            return Ok(ptr.into());
        }

        let mut agg = llvm_type.const_zero();
        agg = self
            .builder
            .build_insert_value(agg, i64_t.const_int(tag, false), 0, "tag")
            .unwrap()
            .into_struct_value();
        for (i, fname) in field_names.iter().enumerate() {
            let init = fields.iter().find(|f| &f.name == fname).ok_or_else(|| {
                format!("missing field `{fname}` in `{enum_name}.{variant}` construction")
            })?;
            let val = self.compile_expr(&init.value)?;
            // F-string payload moves in — disarm the staged accumulator
            // cleanup so it isn't freed again at scope end.
            self.suppress_fstr_acc_if_moved_out(&init.value);
            // Owned String/Vec param captured into a payload field is deep-copied
            // (the caller retains the buffer free under the by-value ABI) — mirrors
            // the struct-literal / tuple-variant constructor paths.
            let val = self.maybe_defensive_copy_param_arg(&init.value, val);
            let (start_word, num_words) = offsets.get(i).copied().unwrap_or((i, 1));
            let words = self.coerce_to_payload_words(val, num_words)?;
            for (j, w) in words.into_iter().enumerate() {
                agg = self
                    .builder
                    .build_insert_value(agg, w, (start_word + j + 1) as u32, "word")
                    .unwrap()
                    .into_struct_value();
            }
            // Move-suppression: a String/Vec/Map local moved into this payload
            // field must NOT be dropped again at scope end. Mirror of the
            // shared-enum struct-variant branch above and the tuple-variant /
            // struct-literal paths — its absence here double-freed a local
            // String moved into a struct-variant payload (`E.NoAt { value:
            // email }`), the Weave dogfood's `ParseError` corruption.
            self.suppress_source_vec_cleanup_for_arg(&init.value);
            // Boxed / inline-heap `Option`/`Result` binding moved whole into
            // this non-shared enum struct-variant field — mirrors the
            // struct-literal field-init paths (`compile_struct_init`).
            self.suppress_inline_option_result_binding_move(&init.value);
            if let ExprKind::Identifier(n) = &init.value.kind {
                let n = n.clone();
                self.suppress_map_cleanup_for_tail_identifier(&n);
            }
        }
        Ok(agg.into())
    }

    /// Phase 7.2 Slice DP — move-suppression helper. When an enum-
    /// variant constructor's argument is an Identifier referencing a
    /// tracked Vec/String binding, zero the source binding's `cap`
    /// field. The existing `CleanupAction::FreeVecBuffer` drain checks
    /// `cap > 0` before invoking `free`, so a zeroed cap turns the
    /// scope-exit cleanup into a no-op for that source. The new enum
    /// binding's `EnumDrop` cleanup then owns the buffer's free.
    ///
    /// No-op for non-Identifier args (rvalue / literal / call result —
    /// no source alloca to mutate; the buffer is already an rvalue
    /// owned solely by the new enum) and for Identifier args that
    /// don't resolve to a tracked Vec/String variable (slice / int /
    /// struct / etc.).
    ///
    /// This mirrors the slice-A return-slot mechanism's cleanup
    /// strategy at `compile_function_body` (around line 4343), which
    /// also opts not to register a parent-side cleanup when the slot
    /// value is moved into a downstream consumer — the consumer
    /// becomes the unique cleanup owner.
    /// Move-aware scope-exit cleanup suppression for the function's
    /// tail-expression return. When the body's final expression is
    /// an `Identifier` naming a tracked Vec / String binding, the
    /// returned struct value carries the binding's data pointer out
    /// — but the let-site's `track_vec_var` queued a scope-exit
    /// `FreeVecBuffer` that would `free` that buffer before the
    /// caller can use it. Zero the source's `cap` field so the
    /// cleanup's `cap > 0` guard skips the free; the loaded return
    /// struct retains the original cap, and the caller's own
    /// scope-exit cleanup frees the buffer exactly once.
    pub(super) fn suppress_cleanup_for_tail_return(&mut self, body: &Block) {
        // Walk the tail of the body: if the final expression of the
        // block (or the value of the last `return expr;` statement)
        // is a bare Identifier for a tracked Vec / String, suppress.
        let from_final: Option<&Expr> = body.final_expr.as_deref();
        let from_last_stmt: Option<&Expr> = body.stmts.last().and_then(|s| match &s.kind {
            StmtKind::Expr(e) => match &e.kind {
                ExprKind::Return(Some(boxed)) => Some(boxed.as_ref()),
                _ => Some(e),
            },
            _ => None,
        });
        if let Some(expr) = from_final.or(from_last_stmt) {
            self.suppress_source_vec_cleanup_for_arg(expr);
            // Sub-slice (3) of move-suppression — when the tail
            // expression is an Identifier whose binding has a user
            // `impl Drop`, the source binding's value is moved out as
            // the function return value. Suppress its UserDrop so the
            // user-body (and thus the user-visible side effect, like
            // `karac_runtime_tcp_close(self.fd)`) doesn't fire at this
            // function's scope exit — the caller will fire it when
            // its own binding for the returned value goes out of
            // scope.
            if let ExprKind::Identifier(name) = &expr.kind {
                self.suppress_user_drop_for_var(name);
                // (Option[shared T] Identifier tail return — `fn f(h) { h }`,
                // or any branch leaf returning an aliasing Option[shared]
                // binding — is now inc'd per-branch during body compilation by
                // `compile_tail_final_expr`, which sees the SAME bare-Identifier
                // final expr in this block AND in each branch arm. Inc'ing it
                // here too would double-count, so the transfer-inc moved there.)
                // Map tail-return cleanup suppression: when the tail is a
                // bare Identifier bound to a Map (or Set, which lowers to
                // Map[T, ()]), drop the matching `FreeMapHandle` from the
                // current scope's cleanup queue. `track_map_var` was
                // queued at `let m = Map.new()`; without this, the queued
                // free fires at this function's scope exit BEFORE the
                // caller receives the handle, leaving the caller with a
                // dangling pointer. Mirrors the Vec/String tail
                // suppression in `suppress_source_vec_cleanup_for_arg`,
                // but Map's cleanup is queue-driven (no in-slot sentinel
                // like `cap = 0` to flip) so we mutate the queue
                // directly. AOT happens to mask this via post-codegen O2
                // elision of the dead store/free; JIT runs pre-O2 IR and
                // exposes it.
                self.suppress_map_cleanup_for_tail_identifier(name);
                // Channel-end tail return: when the tail is a bare
                // Identifier bound to a `Sender`/`Receiver`, the channel
                // end is moved out as the return value — but `bind_pattern`
                // queued a `DropChannelEnd` (refcount decrement) for it at
                // the let/destructure site. Without this, that drop fires at
                // this function's scope exit, decrementing the channel's
                // `total` before the caller's binding receives it: a
                // double-drop that frees the channel early under the
                // caller's nose (the host-async `pointer_moves()`/`wheel()`/
                // `keydown()` producers return `rx` this way, so the channel
                // was being freed while the host listener still held a sender
                // and kept calling `channel_send` on the freed pointer — the
                // recv-out-slot corruption + spurious-close race). The caller
                // fires the drop when its own binding goes out of scope.
                // Mirrors the Vec/String/Map/user-Drop suppressions above.
                self.suppress_channel_drop_for_var(name);
                // SoA move-out (per-layout-monomorphization slice 3): in a
                // return-SoA monomorph the tail identifier's 4-field SoA
                // struct — which shares the heap group buffers — is moved out
                // as the return value, so drop its queued `FreeSoaGroups`. The
                // caller's binding (which receives the struct) now owns the
                // buffers and frees them once; without this the callee frees
                // them at scope exit, leaving the caller's group pointers
                // dangling (double-free / UAF). Gated on the active return
                // layout so the non-mono / AoS-return tail is untouched — the
                // SoA analog of the AoS Vec tail suppression above.
                if matches!(self.return_layout, LayoutId::Soa(_)) {
                    self.suppress_soa_cleanup_for_tail_identifier(name);
                }
                // Return-again move-out (B-2026-06-22-2): a bare
                // heap-env-closure-binding tail hands its RC env box to the
                // caller — neutralize the source so its scope-exit
                // `FreeClosureEnv` doesn't dec the box the caller now owns
                // (sibling of the channel / Map / SoA tail suppressions above).
                self.neutralize_moved_closure_env_slot(name);
                // Aggregate-escape move-out (B-2026-06-22-2): a bare aggregate-
                // owner tail hands its struct (carrying the env boxes) to the
                // caller — null the owned fields' env slots so their scope-exit
                // `FreeClosureEnv` no-ops; the caller's binding frees them.
                self.neutralize_moved_aggregate_env_slots(name);
                // Container-escape move-out (B-2026-06-22-2): the tuple/array twin —
                // a bare tuple/array-owner tail hands its by-value aggregate
                // (carrying the env boxes) to the caller; null the owned elements'
                // env slots so their scope-exit `FreeClosureEnv` no-ops.
                self.neutralize_moved_container_env_slots(name);
            }
            // (Option[shared T] tail FIELD returns — `fn f() ->
            // Option[T] { x.next }` — are compensated during body
            // compilation by `compile_tail_final_expr`'s FieldAccess
            // arm, which incs the loaded inner: +1 for the returned
            // alias, balanced against the owner's drop wherever that
            // happens. This replaced the move-out field ZEROING
            // (`suppress_tail_field_option_dec`, retired 2026-06-05):
            // zeroing mutated the heap object, which is wrong whenever
            // any other live ref can observe it — an owned-shared
            // `self` receiver severed the caller's list — and its
            // ref-root addressing wrote through the un-deref'd param
            // slot into the caller's stack frame.)
        }
    }

    /// Return-again move-out (B-2026-06-22-2): when a heap-env closure binding
    /// is RETURNED (a bare-identifier tail or a top-level `return f;`), the RC
    /// env box flows to the caller — so the source binding must NOT RC-drop it at
    /// this function's scope exit. Null the source fat pointer's env-pointer slot
    /// (the second field) at runtime so its scope-exit `FreeClosureEnv` (which
    /// skips a null env) no-ops; the already-loaded return value keeps the env, and the
    /// caller's binding frees it (the function is in `fns_returning_heap_env`, so
    /// the caller's `let r = relay(..)` is given a `FreeClosureEnv`). Runtime
    /// null — not compile-time queue removal — so a branch that returns the
    /// binding neutralizes only on its own path while a fall-through path that
    /// does NOT return it still frees it. No-op for a non-heap-env name.
    pub(super) fn neutralize_moved_closure_env_slot(&mut self, name: &str) {
        if !self.heap_env_closure_vars.contains(name) {
            return;
        }
        let Some(slot_ptr) = self.variables.get(name).map(|s| s.ptr) else {
            return;
        };
        let fat_ty = self.closure_value_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let env_gep = self
            .builder
            .build_struct_gep(fat_ty, slot_ptr, 1, "clo.move.envslot")
            .unwrap();
        self.builder
            .build_store(env_gep, ptr_ty.const_null())
            .unwrap();
    }

    /// Store-in-struct slice (B-2026-06-22-2): for `let h = H { f: <src>, .. }`,
    /// register an instance-specific `FreeClosureEnv` on each struct field whose
    /// initializer is a sanctioned heap-env closure STORE, and — for a binding
    /// source — bump the shared RC env's refcount. A FRESH call store
    /// (`H { f: make(..) }`, a call to a fn in `fns_returning_heap_env`) leaves the
    /// field as the SOLE owner at refcount 1, so it takes NO inc; its
    /// `FreeClosureEnv` frees the box once at `h`'s scope exit. A BINDING source
    /// store (`H { f: f }`, `f` a heap-env closure local in `heap_env_closure_vars`)
    /// COPIES the source's fat pointer, so the source binding AND this field own the
    /// SAME RC env box; the refcount is INCREMENTED so each RC-drops exactly once
    /// (binding-source sub-slice — the source stays usable, closures being
    /// copy-semantics). The struct's `Fn` field is an inline fat pointer
    /// `{ fn_ptr, env_ptr }`, so a GEP to it is exactly the `fat_alloca` the cleanup
    /// expects (and the value to inc). This is INSTANCE-specific — NOT the
    /// type-driven `__karac_drop_struct_<S>` — because the same struct type may
    /// elsewhere hold a same-frame STACK-env closure (`H { f: |x| x + base }`),
    /// whose env must never be RC-freed. The misuse guard rejects any escape of
    /// `h`, so the field env never outlives `h`.
    pub(super) fn register_struct_literal_heap_env_field_drops(
        &mut self,
        value: &Expr,
        struct_name: &str,
        struct_alloca: PointerValue<'ctx>,
        var_name: &str,
    ) {
        let ExprKind::StructLiteral { fields, .. } = &value.kind else {
            return;
        };
        let Some(field_names) = self.struct_field_names.get(struct_name).cloned() else {
            return;
        };
        let Some(st) = self.struct_types.get(struct_name).copied() else {
            return;
        };
        for f in fields {
            let is_fresh = self.is_heap_env_producing_call(&f.value);
            let is_binding = matches!(
                &f.value.kind,
                ExprKind::Identifier(src) if self.heap_env_closure_vars.contains(src)
            );
            if !is_fresh && !is_binding {
                continue;
            }
            let Some(idx) = field_names.iter().position(|n| n == &f.name) else {
                continue;
            };
            let field_gep = self
                .builder
                .build_struct_gep(st, struct_alloca, idx as u32, "clo.field.envslot")
                .unwrap();
            // Binding source: co-own the box with the source binding — load the
            // field's fat pointer and bump the env refcount (mirrors the
            // `let g = f` inc-on-copy). A fresh-call field is already rc 1.
            if is_binding {
                let fat = self
                    .builder
                    .build_load(self.closure_value_type(), field_gep, "clo.field.fat")
                    .unwrap();
                self.emit_heap_closure_env_inc(fat);
            }
            if let Some(frame) = self.scope_cleanup_actions.last_mut() {
                frame.push(super::state::CleanupAction::FreeClosureEnv {
                    fat_alloca: field_gep,
                });
            }
            // Record the owned field so `neutralize_moved_aggregate_env_slots` can
            // null this env slot if `var_name` is later moved out via a return
            // (aggregate-escape slice).
            self.heap_env_owner_fields
                .entry(var_name.to_string())
                .or_default()
                .push((struct_name.to_string(), idx as u32));
        }
    }

    /// Aggregate-escape slice (B-2026-06-22-2): for `let r = build(k)` where
    /// `build` ∈ `fns_returning_heap_env_aggregate`, register an instance
    /// `FreeClosureEnv` on each of `r`'s owned heap-env fields. `build` MOVED the
    /// env boxes out at the same refcount (its tail/`return` neutralized the
    /// owner's field env slots), so `r`'s field drop is the new sole RC-owner — NO
    /// inc, freed exactly once at `r`'s scope exit. Also records the owned fields so
    /// `r` may itself be re-returned (relay-of-aggregate). The returned struct's
    /// `Fn` field is an inline fat pointer, so the field GEP is the `fat_alloca` the
    /// cleanup expects. Like the struct-literal registrar, this is INSTANCE-specific
    /// — the type-driven struct drop never RC-frees a `Fn` field.
    pub(super) fn register_aggregate_call_heap_env_field_drops(
        &mut self,
        value: &Expr,
        struct_name: &str,
        struct_alloca: PointerValue<'ctx>,
        var_name: &str,
    ) {
        let ExprKind::Call { callee, .. } = &value.kind else {
            return;
        };
        let callee_name = match &callee.kind {
            ExprKind::Identifier(n) => n.clone(),
            ExprKind::Path { segments, .. } if segments.len() == 1 => segments[0].clone(),
            _ => return,
        };
        let Some(owned_fields) = self
            .fns_returning_heap_env_aggregate
            .get(&callee_name)
            .cloned()
        else {
            return;
        };
        let Some(field_names) = self.struct_field_names.get(struct_name).cloned() else {
            return;
        };
        let Some(st) = self.struct_types.get(struct_name).copied() else {
            return;
        };
        // Iterate the struct's DECLARED field order (not `owned_fields`, a HashSet
        // with randomized iteration) so the emitted cleanup order is deterministic
        // across rebuilds — HashSet-order-dependent codegen is a known footgun.
        for (idx, fname) in field_names.iter().enumerate() {
            if !owned_fields.contains(fname) {
                continue;
            }
            let field_gep = self
                .builder
                .build_struct_gep(st, struct_alloca, idx as u32, "clo.aggret.envslot")
                .unwrap();
            if let Some(frame) = self.scope_cleanup_actions.last_mut() {
                frame.push(super::state::CleanupAction::FreeClosureEnv {
                    fat_alloca: field_gep,
                });
            }
            self.heap_env_owner_fields
                .entry(var_name.to_string())
                .or_default()
                .push((struct_name.to_string(), idx as u32));
        }
    }

    /// Owner-copy slice (B-2026-06-22-2): for `let s = a` where `a` is a heap-env
    /// struct OWNER, register `s`'s instance `FreeClosureEnv` on each owned field
    /// and INC the shared RC env. The struct value was COPIED (Kāra struct copy:
    /// heap Vec/String fields deep-copy to independent buffers, but a `Fn` field is
    /// an inline fat pointer copied SHALLOW — so `s`'s field aliases `a`'s SAME env
    /// box). COPY semantics (not move): `a` stays a live owner, so each owner must
    /// RC-drop the shared box exactly once — hence the inc, mirroring the `let g = f`
    /// closure-copy and the binding-source struct STORE. Records `s`'s owned fields
    /// so `s` may itself be copied / moved-out / returned. The struct's `Fn` field
    /// GEP is the `fat_alloca` the cleanup expects (and the value to inc). A no-op
    /// unless `value` is an identifier naming a struct owner.
    pub(super) fn register_owner_copy_struct_heap_env_field_drops(
        &mut self,
        value: &Expr,
        struct_alloca: PointerValue<'ctx>,
        var_name: &str,
    ) {
        let ExprKind::Identifier(src) = &value.kind else {
            return;
        };
        let Some(fields) = self.heap_env_owner_fields.get(src).cloned() else {
            return;
        };
        for (struct_name, idx) in &fields {
            let Some(st) = self.struct_types.get(struct_name).copied() else {
                continue;
            };
            let field_gep = self
                .builder
                .build_struct_gep(st, struct_alloca, *idx, "clo.owncopy.field")
                .unwrap();
            // Co-own the box with the source owner: load the (shallow-copied) field
            // fat pointer and bump its env refcount, so `s`'s and `a`'s drops each
            // free it once.
            let fat = self
                .builder
                .build_load(self.closure_value_type(), field_gep, "clo.owncopy.fat")
                .unwrap();
            self.emit_heap_closure_env_inc(fat);
            if let Some(frame) = self.scope_cleanup_actions.last_mut() {
                frame.push(super::state::CleanupAction::FreeClosureEnv {
                    fat_alloca: field_gep,
                });
            }
        }
        self.heap_env_owner_fields
            .insert(var_name.to_string(), fields);
    }

    /// Owner-copy slice (B-2026-06-22-2): for `let s = t` where `t` is a heap-env
    /// TUPLE or ARRAY owner, register `s`'s instance `FreeClosureEnv` on each owned
    /// element and INC the shared RC env. The by-value aggregate was COPIED: a `Fn`
    /// element is an inline `{ fn_ptr, env_ptr }` fat pointer copied SHALLOW, so
    /// `s`'s element aliases `t`'s SAME env box (a Fn-and-POD owner has no heap
    /// Vec/String sibling to deep-copy or move — the move path
    /// `suppress_source_vec_cleanup_for_arg` only fires when the aggregate has a
    /// directly-visible heap field, which a Fn+POD owner does not). COPY semantics
    /// (not move): `t` stays a live owner, so each owner RC-drops the shared box
    /// exactly once — hence the inc, mirroring
    /// `register_owner_copy_struct_heap_env_field_drops`. `s` is already in
    /// `heap_env_tuple_owners` / `_array` (the guard's `collect_tuple_array_owners`
    /// forward scan marked the copy), so a later move-out of `s`
    /// (`neutralize_moved_container_env_slots`) and the container-return fixpoint
    /// reach `s` with no extra bookkeeping here. The tuple/array twin of the struct
    /// owner-copy registrar; only the element GEP form differs (array
    /// `build_gep [0, idx]` vs tuple `build_struct_gep`). A no-op unless `value` is
    /// an identifier naming a tuple/array owner.
    pub(super) fn register_owner_copy_container_heap_env_elem_drops(
        &mut self,
        value: &Expr,
        var_name: &str,
    ) {
        let ExprKind::Identifier(src) = &value.kind else {
            return;
        };
        let Some(slot) = self.variables.get(var_name).copied() else {
            return;
        };
        let fat_ty = self.closure_value_type();
        if let Some(idxs) = self.heap_env_tuple_owners.get(src).cloned() {
            let inkwell::types::BasicTypeEnum::StructType(agg_ty) = slot.ty else {
                return;
            };
            // Deterministic IR: emit the per-element inc + cleanup in sorted index
            // order (the owner set is a HashSet with randomized iteration).
            let mut sorted: Vec<usize> = idxs.into_iter().collect();
            sorted.sort_unstable();
            for idx in sorted {
                let elem_gep = self
                    .builder
                    .build_struct_gep(agg_ty, slot.ptr, idx as u32, "clo.owncopy.tup.envslot")
                    .unwrap();
                let fat = self
                    .builder
                    .build_load(fat_ty, elem_gep, "clo.owncopy.tup.fat")
                    .unwrap();
                self.emit_heap_closure_env_inc(fat);
                if let Some(frame) = self.scope_cleanup_actions.last_mut() {
                    frame.push(super::state::CleanupAction::FreeClosureEnv {
                        fat_alloca: elem_gep,
                    });
                }
            }
        } else if let Some(idxs) = self.heap_env_array_owners.get(src).cloned() {
            let inkwell::types::BasicTypeEnum::ArrayType(arr_ty) = slot.ty else {
                return;
            };
            let i64_t = self.context.i64_type();
            let zero = i64_t.const_int(0, false);
            let mut sorted: Vec<usize> = idxs.into_iter().collect();
            sorted.sort_unstable();
            for idx in sorted {
                let elem_gep = unsafe {
                    self.builder
                        .build_gep(
                            arr_ty,
                            slot.ptr,
                            &[zero, i64_t.const_int(idx as u64, false)],
                            "clo.owncopy.arr.envslot",
                        )
                        .unwrap()
                };
                let fat = self
                    .builder
                    .build_load(fat_ty, elem_gep, "clo.owncopy.arr.fat")
                    .unwrap();
                self.emit_heap_closure_env_inc(fat);
                if let Some(frame) = self.scope_cleanup_actions.last_mut() {
                    frame.push(super::state::CleanupAction::FreeClosureEnv {
                        fat_alloca: elem_gep,
                    });
                }
            }
        }
    }

    /// Tuple-store slice (B-2026-06-22-2): for `let t = (<src>, ..)`, register an
    /// instance `FreeClosureEnv` on each tuple element whose initializer is a
    /// sanctioned heap-env closure STORE. A FRESH call (`(make(k), ..)`) leaves the
    /// element at refcount 1 (no inc); a heap-env BINDING source (`(f, ..)`, `f` in
    /// `heap_env_closure_vars`) COPIES the source's fat pointer, so the element
    /// co-owns the box — bump the refcount (mirrors the struct binding-source
    /// store). A tuple is a by-value aggregate `{ e0, e1, .. }`, so a `Fn` element is
    /// an inline `{ fn_ptr, env_ptr }` fat pointer and the element GEP is exactly the
    /// `fat_alloca` the cleanup expects. INSTANCE-specific — the type-driven tuple
    /// drop never RC-frees a `Fn` element. The misuse guard rejects any escape of
    /// `t`, so the element env never outlives `t` (tuple escape is a later slice).
    pub(super) fn register_tuple_literal_heap_env_elem_drops(
        &mut self,
        value: &Expr,
        tuple_alloca: PointerValue<'ctx>,
        agg_ty: inkwell::types::StructType<'ctx>,
    ) {
        let ExprKind::Tuple(elems) = &value.kind else {
            return;
        };
        for (idx, elem) in elems.iter().enumerate() {
            let is_fresh = self.is_heap_env_producing_call(elem);
            let is_binding = matches!(
                &elem.kind,
                ExprKind::Identifier(src) if self.heap_env_closure_vars.contains(src)
            );
            if !is_fresh && !is_binding {
                continue;
            }
            let elem_gep = self
                .builder
                .build_struct_gep(agg_ty, tuple_alloca, idx as u32, "clo.tuple.envslot")
                .unwrap();
            // Binding source co-owns the box with the source binding — bump the env
            // refcount (a fresh-call element is already rc 1).
            if is_binding {
                let fat = self
                    .builder
                    .build_load(self.closure_value_type(), elem_gep, "clo.tuple.fat")
                    .unwrap();
                self.emit_heap_closure_env_inc(fat);
            }
            if let Some(frame) = self.scope_cleanup_actions.last_mut() {
                frame.push(super::state::CleanupAction::FreeClosureEnv {
                    fat_alloca: elem_gep,
                });
            }
        }
    }

    /// Array-store slice (B-2026-06-22-2): for `let a: Array[Fn,N] = [<src>, ..]`,
    /// register an instance `FreeClosureEnv` on each fixed-size-array element whose
    /// initializer is a sanctioned heap-env closure STORE. A FRESH call
    /// (`[make(k), ..]`) leaves the element at refcount 1 (no inc); a heap-env
    /// BINDING source (`[f, ..]`, `f` in `heap_env_closure_vars`) COPIES the
    /// source's fat pointer, so the element co-owns the box — bump the refcount
    /// (mirrors the tuple binding-source store). An array is a by-value LLVM
    /// aggregate `[N x { fn_ptr, env_ptr }]`, so a `Fn` element GEP'd at `[0, idx]`
    /// yields exactly the inline fat pointer the cleanup expects as `fat_alloca`.
    /// INSTANCE-specific — there is no type-driven array drop for a `Fn`-element
    /// array (a `{ptr,ptr}` element looks like POD), so without this the env would
    /// leak; the misuse guard rejects any escape of `a`, so the element env never
    /// outlives `a` (array escape is a later slice). The tuple-store registrar's
    /// array twin; only the element GEP form differs (array `build_gep` `[0, idx]`
    /// vs tuple `build_struct_gep`).
    pub(super) fn register_array_literal_heap_env_elem_drops(
        &mut self,
        value: &Expr,
        arr_alloca: PointerValue<'ctx>,
        arr_ty: inkwell::types::ArrayType<'ctx>,
    ) {
        let ExprKind::ArrayLiteral(elems) = &value.kind else {
            return;
        };
        let i64_t = self.context.i64_type();
        let zero = i64_t.const_int(0, false);
        for (idx, elem) in elems.iter().enumerate() {
            let is_fresh = self.is_heap_env_producing_call(elem);
            let is_binding = matches!(
                &elem.kind,
                ExprKind::Identifier(src) if self.heap_env_closure_vars.contains(src)
            );
            if !is_fresh && !is_binding {
                continue;
            }
            let elem_idx = i64_t.const_int(idx as u64, false);
            let elem_gep = unsafe {
                self.builder
                    .build_gep(arr_ty, arr_alloca, &[zero, elem_idx], "clo.arr.envslot")
                    .unwrap()
            };
            // Binding source co-owns the box with the source binding — bump the env
            // refcount (a fresh-call element is already rc 1).
            if is_binding {
                let fat = self
                    .builder
                    .build_load(self.closure_value_type(), elem_gep, "clo.arr.fat")
                    .unwrap();
                self.emit_heap_closure_env_inc(fat);
            }
            if let Some(frame) = self.scope_cleanup_actions.last_mut() {
                frame.push(super::state::CleanupAction::FreeClosureEnv {
                    fat_alloca: elem_gep,
                });
            }
        }
    }

    /// Aggregate-escape move-out (B-2026-06-22-2): when an aggregate owner `name`
    /// is RETURNED (a bare-identifier tail or a top-level `return h;`), its struct
    /// VALUE is handed to the caller carrying the env boxes — so this function must
    /// NOT RC-drop them at scope exit. For each owned field, null the inline fat
    /// pointer's env-pointer slot in `name`'s alloca at runtime, so the field's
    /// scope-exit `FreeClosureEnv` (which skips a null env) no-ops. The already-
    /// materialized return value keeps the env, and the caller's `let r = build(..)`
    /// binding frees it (the caller registers the field drops via
    /// `register_aggregate_call_heap_env_field_drops`). Runtime null — not
    /// compile-time queue removal — mirrors `neutralize_moved_closure_env_slot`.
    /// No-op for a name that owns no heap-env fields.
    pub(super) fn neutralize_moved_aggregate_env_slots(&mut self, name: &str) {
        let Some(fields) = self.heap_env_owner_fields.get(name).cloned() else {
            return;
        };
        let Some(slot_ptr) = self.variables.get(name).map(|s| s.ptr) else {
            return;
        };
        let fat_ty = self.closure_value_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        for (struct_name, idx) in fields {
            let Some(st) = self.struct_types.get(&struct_name).copied() else {
                continue;
            };
            let field_gep = self
                .builder
                .build_struct_gep(st, slot_ptr, idx, "clo.agg.field")
                .unwrap();
            let env_gep = self
                .builder
                .build_struct_gep(fat_ty, field_gep, 1, "clo.agg.envslot")
                .unwrap();
            self.builder
                .build_store(env_gep, ptr_ty.const_null())
                .unwrap();
        }
    }

    /// Container-escape move-out (B-2026-06-22-2): when a TUPLE or ARRAY owner
    /// `name` is RETURNED (a bare-identifier tail or a top-level `return t;`), its
    /// by-value aggregate VALUE is handed to the caller carrying the env boxes — so
    /// this function must NOT RC-drop them at scope exit. For each owned element,
    /// null the inline fat pointer's env-pointer slot in `name`'s alloca at runtime,
    /// so the element's scope-exit `FreeClosureEnv` (which skips a null env) no-ops.
    /// The already-materialized return value keeps the env, and the caller's
    /// `let r = build(..)` binding frees it (the caller registers the element drops
    /// via `register_container_call_heap_env_elem_drops`). The tuple/array twin of
    /// `neutralize_moved_aggregate_env_slots`; tuple elements GEP via the slot's
    /// StructType, array elements via `[0, idx]`. No-op for a name owning no
    /// tuple/array heap-env elements.
    pub(super) fn neutralize_moved_container_env_slots(&mut self, name: &str) {
        let Some(slot) = self.variables.get(name).copied() else {
            return;
        };
        let fat_ty = self.closure_value_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let null = ptr_ty.const_null();
        if let Some(idxs) = self.heap_env_tuple_owners.get(name).cloned() {
            let inkwell::types::BasicTypeEnum::StructType(agg_ty) = slot.ty else {
                return;
            };
            for idx in idxs {
                let elem_gep = self
                    .builder
                    .build_struct_gep(agg_ty, slot.ptr, idx as u32, "clo.cont.tup.elem")
                    .unwrap();
                let env_gep = self
                    .builder
                    .build_struct_gep(fat_ty, elem_gep, 1, "clo.cont.tup.envslot")
                    .unwrap();
                self.builder.build_store(env_gep, null).unwrap();
            }
        } else if let Some(idxs) = self.heap_env_array_owners.get(name).cloned() {
            let inkwell::types::BasicTypeEnum::ArrayType(arr_ty) = slot.ty else {
                return;
            };
            let i64_t = self.context.i64_type();
            let zero = i64_t.const_int(0, false);
            for idx in idxs {
                let elem_gep = unsafe {
                    self.builder
                        .build_gep(
                            arr_ty,
                            slot.ptr,
                            &[zero, i64_t.const_int(idx as u64, false)],
                            "clo.cont.arr.elem",
                        )
                        .unwrap()
                };
                let env_gep = self
                    .builder
                    .build_struct_gep(fat_ty, elem_gep, 1, "clo.cont.arr.envslot")
                    .unwrap();
                self.builder.build_store(env_gep, null).unwrap();
            }
        }
    }

    /// Container-escape caller-adopt (B-2026-06-22-2): for `let r = build(k)` where
    /// `build` returns a closure-owning TUPLE / ARRAY (in
    /// `fns_returning_heap_env_tuple` / `_array`), register an instance
    /// `FreeClosureEnv` on each of `r`'s owned elements. `build` MOVED the env boxes
    /// out at the same refcount (its return neutralized the owner's element env
    /// slots), so `r`'s element drop is the new sole RC-owner — NO inc, freed once
    /// at `r`'s scope exit. Iterates a SORTED index list for deterministic IR. The
    /// tuple/array twin of `register_aggregate_call_heap_env_field_drops`; only the
    /// element GEP form differs (array `build_gep [0, idx]` vs tuple
    /// `build_struct_gep`). A no-op unless `value` is a call to such a fn.
    pub(super) fn register_container_call_heap_env_elem_drops(
        &mut self,
        value: &Expr,
        var_name: &str,
    ) {
        let ExprKind::Call { callee, .. } = &value.kind else {
            return;
        };
        let callee_name = match &callee.kind {
            ExprKind::Identifier(n) => n.clone(),
            ExprKind::Path { segments, .. } if segments.len() == 1 => segments[0].clone(),
            _ => return,
        };
        let Some(slot) = self.variables.get(var_name).copied() else {
            return;
        };
        if let Some(idxs) = self.fns_returning_heap_env_tuple.get(&callee_name).cloned() {
            let inkwell::types::BasicTypeEnum::StructType(agg_ty) = slot.ty else {
                return;
            };
            let mut sorted: Vec<usize> = idxs.into_iter().collect();
            sorted.sort_unstable();
            for idx in sorted {
                let elem_gep = self
                    .builder
                    .build_struct_gep(agg_ty, slot.ptr, idx as u32, "clo.contret.tup.envslot")
                    .unwrap();
                if let Some(frame) = self.scope_cleanup_actions.last_mut() {
                    frame.push(super::state::CleanupAction::FreeClosureEnv {
                        fat_alloca: elem_gep,
                    });
                }
            }
        } else if let Some(idxs) = self.fns_returning_heap_env_array.get(&callee_name).cloned() {
            let inkwell::types::BasicTypeEnum::ArrayType(arr_ty) = slot.ty else {
                return;
            };
            let i64_t = self.context.i64_type();
            let zero = i64_t.const_int(0, false);
            let mut sorted: Vec<usize> = idxs.into_iter().collect();
            sorted.sort_unstable();
            for idx in sorted {
                let elem_gep = unsafe {
                    self.builder
                        .build_gep(
                            arr_ty,
                            slot.ptr,
                            &[zero, i64_t.const_int(idx as u64, false)],
                            "clo.contret.arr.envslot",
                        )
                        .unwrap()
                };
                if let Some(frame) = self.scope_cleanup_actions.last_mut() {
                    frame.push(super::state::CleanupAction::FreeClosureEnv {
                        fat_alloca: elem_gep,
                    });
                }
            }
        }
    }

    pub(super) fn suppress_source_vec_cleanup_for_arg(&self, arg_expr: &Expr) {
        self.suppress_source_vec_cleanup_for_arg_ex(arg_expr, true);
    }

    /// Map move-out cleanup suppression — drop any `FreeMapHandle` whose
    /// `map_alloca` matches the named binding's slot, so a `Map`/`Set`
    /// binding that has been MOVED (tail return, enum-variant capture,
    /// `v.push(m)` into a `Vec[Map]`) is no longer freed by its origin
    /// binding. Map cleanup is queue-driven (no in-slot sentinel like
    /// Vec/String's `cap = 0` for the walker to skip), so the queue is
    /// edited directly. The `track_map_var` call site is
    /// `compile_map_new_stmt` (direct `Map.new()`) or the fresh-handle
    /// method-call branch in the let-stmt arm. Set bindings track via the
    /// same `FreeMapHandle` action (Set lowers to `Map[T, ()]`), so this
    /// helper covers both surfaces.
    ///
    /// Scans EVERY live frame — at a mid-function move (`v.push(m)`) a
    /// transient arg/method-call frame sits on top of the frame that owns
    /// the moved binding's `FreeMapHandle`, so filtering only `last()`
    /// would leave it armed (double-free against the consumer that now owns
    /// the handle). For tail-return callers the inner scopes have already
    /// drained, so only the function-body frame remains and the all-frames
    /// scan is equivalent to the old top-frame-only behavior.
    pub(super) fn suppress_map_cleanup_for_tail_identifier(&mut self, name: &str) {
        let slot_ptr = match self.variables.get(name) {
            Some(s) => s.ptr,
            None => return,
        };
        // Scan EVERY live frame, not just the innermost. A move site
        // (`v.push(m)`, enum-variant capture, tail return) can fire while a
        // transient inner scope sits on top of the frame that owns the
        // moved binding's `FreeMapHandle` — at a `v.push(m)` statement the
        // arg/method-call evaluation pushes an inner frame, so `m`'s
        // `FreeMapHandle` lives one frame below `last`. Filtering only the
        // top frame left it armed, double-freeing the handle the Vec now
        // owns (`Vec[Map]` element drop). Removing it from whichever frame
        // holds it is correct for all callers: the binding has been moved,
        // so its origin must never free the handle regardless of frame.
        for frame in self.scope_cleanup_actions.iter_mut() {
            frame.retain(|action| match action {
                crate::codegen::state::CleanupAction::FreeMapHandle { map_alloca, .. } => {
                    *map_alloca != slot_ptr
                }
                _ => true,
            });
        }
    }

    /// SoA move-out cleanup suppression (per-layout-monomorphization slice 3)
    /// — drop any `FreeSoaGroups` whose `soa_alloca` matches the named
    /// binding's slot, so a SoA-laid-out Vec moved out as a return value is no
    /// longer freed by its origin binding. The SoA analog of
    /// `suppress_map_cleanup_for_tail_identifier`: SoA cleanup is queue-driven
    /// (no in-slot sentinel like Vec/String's `cap = 0` for the walker to
    /// skip), so the action is removed directly, and from EVERY live frame —
    /// the move site can fire while a transient inner scope sits above the
    /// frame that owns the binding's `FreeSoaGroups`. The caller's binding for
    /// the returned struct owns the group buffers and frees them once.
    pub(super) fn suppress_soa_cleanup_for_tail_identifier(&mut self, name: &str) {
        let slot_ptr = match self.variables.get(name) {
            Some(s) => s.ptr,
            None => return,
        };
        for frame in self.scope_cleanup_actions.iter_mut() {
            frame.retain(|action| match action {
                crate::codegen::state::CleanupAction::FreeSoaGroups { soa_alloca, .. } => {
                    *soa_alloca != slot_ptr
                }
                _ => true,
            });
        }
    }

    /// Branch-safe SoA move-out for an EARLY `return a;` of a SoA local: zero
    /// the source's `cap` slot (a runtime store at the current — the return
    /// branch's — insertion point) so its queued `FreeSoaGroups` no-ops on THIS
    /// path (the cleanup's `cap > 0` guard reads the zeroed slot), while still
    /// firing on the fall-through path where `a` is NOT returned and must be
    /// freed. The runtime-sentinel analog of
    /// `suppress_soa_cleanup_for_tail_identifier`'s compile-time frame removal:
    /// at an early return the cleanup frame is shared with the non-returning
    /// path, so frame removal would leak `a` there (the branch-buried-move
    /// footgun — same reason the channel-end move uses a runtime null-sentinel,
    /// not compile-time suppression). Emit it AFTER the return value is loaded
    /// so the returned struct keeps the real `cap` and the caller frees the
    /// group buffers exactly once. No-op when `name` is not a SoA local, or is a
    /// `ref`/`mut ref` SoA param (a borrow never owns the buffers — and its slot
    /// holds a pointer to the caller's struct, not the struct itself).
    pub(super) fn neutralize_moved_soa_groups_slot(&mut self, name: &str) {
        let soa = match self.active_soa_layout(name) {
            Some(s) => s,
            None => return,
        };
        if self.ref_params.contains_key(name) {
            return;
        }
        let slot = match self.variables.get(name) {
            Some(s) => *s,
            None => return,
        };
        let has_cold = soa.cold_group.is_some();
        let soa_ty = self.soa_vec_type(soa.num_groups, has_cold);
        let cap_idx = Self::soa_cap_index(soa.num_groups, has_cold);
        if let Ok(cap_ptr) =
            self.builder
                .build_struct_gep(soa_ty, slot.ptr, cap_idx, "soa.moveout.cap.suppress")
        {
            let zero = self.context.i64_type().const_int(0, false);
            let _ = self.builder.build_store(cap_ptr, zero);
        }
    }

    /// Queue scope-exit cleanup for a `ref T` rvalue-arg temp materialized
    /// into `slot` (the `ref_rvalue_arg{i}` alloca). Generalizes the prior
    /// Vec/String-only `track_vec_var(slot, None)` (slice 2 part B):
    ///   - **Vec / String** — the element type is recovered from
    ///     `owned_temp_drops` so the `FreeVecBuffer` walk frees nested
    ///     element buffers (`Vec[String]` / `Vec[Vec[T]]`), closing the
    ///     nested-heap leak the prior `None` left open. Detection is still
    ///     by LLVM value type, so a missing hint entry degrades to the
    ///     slice-1 behavior (outer buffer freed, inner leaks) — never a
    ///     double-free.
    ///   - **Map / Set handle** — a plain pointer, recognized only via the
    ///     hint table; freed with the K/V Vec/shared classification from
    ///     `map_temp_cleanup_parts`. Map handles passed as fresh rvalues to
    ///     a `ref Map` param leaked entirely before this.
    ///
    /// RC-box rvalue args (`ref shared T`) are deferred — the `ref shared T`
    /// argument ABI needs separate handling and the prior code didn't cover
    /// them either, so leaving them out is not a regression.
    pub(super) fn queue_ref_rvalue_arg_cleanup(
        &mut self,
        slot: PointerValue<'ctx>,
        val: BasicValueEnum<'ctx>,
        arg_expr: &Expr,
    ) {
        let span_key = (arg_expr.span.offset, arg_expr.span.length);
        if self.llvm_ty_is_vec_struct(val.get_type()) {
            let elem_ty = self
                .owned_temp_drops
                .get(&span_key)
                .cloned()
                .and_then(|te| self.extract_vec_elem_type(&te));
            self.track_vec_var(slot, elem_ty);
            return;
        }
        if !val.is_pointer_value() {
            return;
        }
        let Some(te) = self.owned_temp_drops.get(&span_key).cloned() else {
            return;
        };
        let head = match &te.kind {
            TypeKind::Path(p) => p.segments.first().map(|s| s.as_str()).unwrap_or(""),
            _ => return,
        };
        if head == "Map" || head == "Set" {
            let (key_is_vec, val_is_vec, key_shared, val_shared, val_drop_fn) =
                self.map_temp_cleanup_parts(&te);
            self.track_map_var_with_val_drop(
                slot,
                key_is_vec,
                val_is_vec,
                val_shared,
                key_shared,
                val_drop_fn,
            );
        }
    }

    /// `apply_shared_transfer`: whether to emit the shared-struct/enum
    /// transfer-inc (the "consumer holds an independent ref, source's
    /// queued rc_dec balances" mechanism). True for genuine MOVE/consume
    /// sites (return tail, by-value call arg, collection insert, struct/
    /// tuple-field capture) where the consumer has no receive-inc of its
    /// own. FALSE for shared `let t = src;` COPY sites: the let-binding's
    /// own receive-inc (the `shared_info` `emit_refcount_inc` in
    /// `compile_stmt`) already grants `t` an independent ref, so adding the
    /// transfer-inc here would DOUBLE-count — the chain's head then never
    /// reaches rc 0 on its single scope-exit dec and the whole list leaks
    /// (the tail-cursor builder `let mut tail = head; … tail = node;`,
    /// LeetCode #19 bench). Vec/String cap-zeroing and non-shared StructDrop
    /// handle-zeroing run regardless (those ARE needed at let-copy sites).
    /// Zero the `cap` word of every variant's `VecOrString` payload field of the
    /// non-shared enum value at `base_ptr`, so a synthesized `__karac_drop_<E>`
    /// switch's `cap > 0` guard no-ops for whichever variant is live at runtime.
    /// The move-out dual of `emit_enum_drop_switch` (and the whole-value sibling
    /// of `suppress_destructured_enum_payload_cleanup_at`): used both for a
    /// moved whole-enum binding and — post-#15/#19 — for an enum FIELD of a
    /// moved struct (the struct's drop now frees its enum fields). Zeroing dead
    /// variants' overlay words is harmless: only the live variant's BB is
    /// entered by the drop switch. `&self` — pure IR emission.
    pub(super) fn zero_enum_payload_caps(
        &self,
        base_ptr: PointerValue<'ctx>,
        layout: &super::state::EnumLayout<'ctx>,
    ) {
        let zero = self.context.i64_type().const_int(0, false);
        for (variant, kinds) in &layout.field_drop_kinds {
            let Some(offsets) = layout.field_word_offsets.get(variant) else {
                continue;
            };
            for (kind, (start_word, num_words)) in kinds.iter().zip(offsets.iter()) {
                if !kind.is_heap_bearing() {
                    continue;
                }
                // Zero every payload word of the moved-out field (not just the
                // Vec/String cap) so a `NestedStruct` payload's inner caps/tag
                // all go to 0 and its drop fn no-ops — see the matching loop in
                // `suppress_destructured_enum_payload_cleanup_at`
                // (B-2026-06-13-13).
                for w in 0..*num_words {
                    let word_index = (start_word + 1 + w) as u32;
                    if let Ok(word_ptr) = self.builder.build_struct_gep(
                        layout.llvm_type,
                        base_ptr,
                        word_index,
                        "move.enum.suppress.wp",
                    ) {
                        let _ = self.builder.build_store(word_ptr, zero);
                    }
                }
            }
        }
    }

    /// Cap-zero the move-suppression caps of EVERY heap field of the non-shared
    /// struct value at `base_ptr`, recursing into nested struct fields — the
    /// move-out dual of `emit_struct_drop_synthesis`'s field walk. For a moved
    /// struct (`return s`, `let g = f`, struct/enum-literal field, push/insert),
    /// each Vec/String field's `cap` is zeroed, each ENUM field's live-variant
    /// payload cap is zeroed (`zero_enum_payload_caps`, post-#15/#19), each
    /// nested non-shared user STRUCT field is recursed into (the
    /// `Wrap { sp: Span { tok } }` transfer shape, #18), and the HTTP side-table
    /// handle is zeroed — so the source struct's `StructDrop` (which now frees
    /// all of these transitively) no-ops and the consumer is the sole owner.
    /// Value structs cannot be self-referential by value, so the recursion
    /// terminates. `&self` — pure IR emission.
    pub(super) fn zero_struct_move_caps(&self, base_ptr: PointerValue<'ctx>, struct_name: &str) {
        let Some(&st) = self.struct_types.get(struct_name) else {
            return;
        };
        let Some(field_names) = self.struct_field_type_names.get(struct_name).cloned() else {
            return;
        };
        let field_tes = self.struct_field_type_exprs.get(struct_name).cloned();
        let vec_ty = self.vec_struct_type();
        let zero = self.context.i64_type().const_int(0, false);
        for (i, opt_name) in field_names.iter().enumerate() {
            let fname = opt_name.as_deref().unwrap_or("");
            let Ok(field_ptr) =
                self.builder
                    .build_struct_gep(st, base_ptr, i as u32, &format!("smv.f{i}.p"))
            else {
                continue;
            };
            if matches!(fname, "Vec" | "VecDeque" | "String") {
                if let Ok(cap_ptr) =
                    self.builder
                        .build_struct_gep(vec_ty, field_ptr, 2, &format!("smv.f{i}.cap"))
                {
                    let _ = self.builder.build_store(cap_ptr, zero);
                }
                // B-2026-07-10-1 — also zero LEN. The struct's combined drop
                // (`__karac_vec_elem_full_drop_<S>`) frees the Vec BUFFER under a
                // `cap > 0` guard (neutralized by the cap-zero above) BUT ALSO runs
                // a SEPARATE, LEN-driven per-element rc-dec walk when the element
                // transitively owns a `shared` handle (B-2026-06-14-28 —
                // `Vec[Stmt]`, `Stmt::Exp(ExprStmt)`, `ExprStmt { expr: Expr }`).
                // That walk is NOT under the cap guard, so a whole-struct move
                // (`let b = Block{..}; Expr.Blk(b)`) that zeroed only `cap` still
                // rc-dec'd the moved-out elements' shared handles — which the
                // destination (the boxed enum payload) co-owns — corrupting them.
                // Zeroing `len` makes the element walk skip too, fully neutralizing
                // the moved-out source's drop. Harmless for a `Vec`/`String` with no
                // shared-bearing element (no such walk exists).
                if let Ok(len_ptr) =
                    self.builder
                        .build_struct_gep(vec_ty, field_ptr, 1, &format!("smv.f{i}.len"))
                {
                    let _ = self.builder.build_store(len_ptr, zero);
                }
            } else if fname == "Option" {
                // B-2026-07-03-28 Facet A — the whole struct is moved, so its
                // Option field is now owned by the destination; zero the source
                // tag so its struct-drop `OptionInline` skips it.
                self.zero_option_field_tag_at(field_ptr);
            } else if fname != "Result" {
                if let Some(layout) = self.enum_layouts.get(fname).cloned() {
                    if !layout.is_shared {
                        self.zero_enum_payload_caps(field_ptr, &layout);
                    }
                } else if self.struct_types.contains_key(fname)
                    && !self.shared_types.contains_key(fname)
                {
                    self.zero_struct_move_caps(field_ptr, fname);
                } else if let Some(crate::ast::TypeKind::Tuple(elems)) = field_tes
                    .as_ref()
                    .and_then(|tes| tes.get(i))
                    .map(|t| &t.kind)
                {
                    // #21 — a TUPLE field (no declared type name, so the
                    // name-based arms above all miss it) whose drop now frees
                    // enum / nested-struct leaves (`NestedTuple`). Cap-zero those
                    // leaves so the moved-out struct's drop no-ops on them — the
                    // tuple analog of the enum/struct arms above (was the P8
                    // `let g = h` double-free).
                    if let Some(inkwell::types::BasicTypeEnum::StructType(fst)) =
                        st.get_field_type_at_index(i as u32)
                    {
                        self.zero_tuple_elem_caps(field_ptr, fst, elems);
                    }
                }
            }
        }
        // HTTP side-table handle field (Response/RequestBuilder) — zero so the
        // synthesized Drop (guarded on `handle != 0`) no-ops; the consumer owns
        // the live handle. Idempotent runtime remove is the backstop.
        let handle_field = match struct_name {
            "Response" => Some(2u32),
            "RequestBuilder" => Some(0u32),
            _ => None,
        };
        if let Some(fidx) = handle_field {
            if let Ok(field_ptr) = self
                .builder
                .build_struct_gep(st, base_ptr, fidx, "smv.handle.p")
            {
                let _ = self.builder.build_store(field_ptr, zero);
            }
        }
    }

    /// Zero the moved-out heap field `field` of struct `struct_name` (rooted at
    /// `base_ptr`, which must hold the struct INLINE) so the struct's
    /// `StructDrop` skips it — the single-field analog of `zero_struct_move_caps`,
    /// used when ONE field is moved out of an owned struct via `FieldAccess`
    /// (`return s.a` / `f(s.a)` / `let x = s.a`). Vec/String → `cap = 0`;
    /// non-shared enum → live-variant payload caps; nested non-shared struct →
    /// recurse. No-op for scalar / shared / Option / Result fields (the struct
    /// drop already does the right thing for those).
    pub(super) fn zero_struct_field_move_cap(
        &self,
        base_ptr: PointerValue<'ctx>,
        struct_name: &str,
        field: &str,
    ) {
        let Some(&st) = self.struct_types.get(struct_name) else {
            return;
        };
        let Some(field_names) = self.struct_field_names.get(struct_name) else {
            return;
        };
        let Some(idx) = field_names.iter().position(|n| n == field) else {
            return;
        };
        let fname = self
            .struct_field_type_names
            .get(struct_name)
            .and_then(|v| v.get(idx))
            .and_then(|o| o.as_deref())
            .unwrap_or("")
            .to_string();
        let Ok(field_ptr) = self
            .builder
            .build_struct_gep(st, base_ptr, idx as u32, "sfld.move.p")
        else {
            return;
        };
        let vec_ty = self.vec_struct_type();
        let zero = self.context.i64_type().const_int(0, false);
        if matches!(fname.as_str(), "Vec" | "VecDeque" | "String") {
            if let Ok(cap_ptr) =
                self.builder
                    .build_struct_gep(vec_ty, field_ptr, 2, "sfld.move.cap")
            {
                let _ = self.builder.build_store(cap_ptr, zero);
            }
        } else if fname == "Option" {
            // B-2026-07-03-28 Facet A — a moved-out `Option[inline-heap]` field.
            // Zero its tag to `None` so the owner's struct-drop `OptionInline`
            // free (tag-guarded on `Some`) skips it; the destructure leaf now
            // owns the payload. The Option peer of the Vec cap-zero above.
            self.zero_option_field_tag_at(field_ptr);
        } else if fname != "Result" {
            if let Some(layout) = self.enum_layouts.get(fname.as_str()).cloned() {
                if !layout.is_shared {
                    self.zero_enum_payload_caps(field_ptr, &layout);
                }
            } else if self.struct_types.contains_key(fname.as_str())
                && !self.shared_types.contains_key(fname.as_str())
            {
                self.zero_struct_move_caps(field_ptr, &fname);
            }
        }
    }

    /// Zero the tag word (to `None`) of an inline `Option` value at `field_ptr`,
    /// so a tag-guarded `OptionInline` struct-drop / inline-Option cleanup skips
    /// it — the move-out neutralizer for a transferred `Option[heap]` field
    /// (B-2026-07-03-28 Facet A). No-op if the `Option` layout is unregistered.
    pub(super) fn zero_option_field_tag_at(&self, field_ptr: PointerValue<'ctx>) {
        if let Some(layout) = self.enum_layouts.get("Option") {
            let none_tag = layout.tags.get("None").copied().unwrap_or(0);
            let option_ty = layout.llvm_type;
            if let Ok(tag_ptr) =
                self.builder
                    .build_struct_gep(option_ty, field_ptr, 0, "opt.move.tag")
            {
                let _ = self
                    .builder
                    .build_store(tag_ptr, self.context.i64_type().const_int(none_tag, false));
            }
        }
    }

    pub(super) fn suppress_source_vec_cleanup_for_arg_ex(
        &self,
        arg_expr: &Expr,
        apply_shared_transfer: bool,
    ) {
        // Tuple field move-out (`let s = t.N`, `f(t.N)`, `return t.N`): the
        // heap field is moved into the consumer, but the tuple `t` still carries
        // its `track_tuple_var` drop (B-2026-06-11-4 part a), which would free
        // the same buffer — a double-free. Zero that field's `cap` so the
        // tuple's drop skips it (the consumer's own track is the sole owner).
        // Only a non-boxed tuple (a struct VALUE slot) with a heap field at
        // `index` is touched; an RC-fallback-boxed tuple has a pointer slot
        // (the `StructType` guard fails) and is handled by the rc machinery.
        if let ExprKind::TupleIndex { object, index } = &arg_expr.kind {
            if let ExprKind::Identifier(t) = &object.kind {
                if let Some(slot) = self.variables.get(t.as_str()).copied() {
                    if let inkwell::types::BasicTypeEnum::StructType(agg_ty) = slot.ty {
                        let vec_ty = self.vec_struct_type();
                        if agg_ty != vec_ty
                            && matches!(
                                agg_ty.get_field_type_at_index(*index as u32),
                                Some(inkwell::types::BasicTypeEnum::StructType(fst)) if fst == vec_ty
                            )
                        {
                            if let Ok(field_ptr) = self.builder.build_struct_gep(
                                agg_ty,
                                slot.ptr,
                                *index as u32,
                                "tupfld.move.p",
                            ) {
                                if let Ok(cap_ptr) = self.builder.build_struct_gep(
                                    vec_ty,
                                    field_ptr,
                                    2,
                                    "tupfld.move.cap",
                                ) {
                                    let _ = self.builder.build_store(
                                        cap_ptr,
                                        self.context.i64_type().const_int(0, false),
                                    );
                                }
                            }
                        }
                    }
                }
            }
            return;
        }
        // Struct field move-out (`return s.a`, `f(s.a)`, `let x = s.a`): the
        // heap field is moved into the consumer, but the OWNED struct `s` (a
        // callee-owned by-value param deep-copied at entry — #14/#17 — or any
        // local with a registered `StructDrop`) still frees that field at scope
        // exit, a double-free (selfhost slice 3c-ii minimal:
        // `fn f(s: S) -> String { s.a }`). Zero the moved field's `cap` (or its
        // enum-payload / nested-struct caps) in the source so the struct drop
        // skips it; the consumer is the sole owner. The struct counterpart of
        // the `TupleIndex` arm above. Guarded to a struct held INLINE in the
        // slot (`slot.ty == st`): a `ref Struct` param's slot holds a POINTER
        // into the caller's frame and takes no ownership, so zeroing there would
        // corrupt the caller. Shared (RC) structs are left to the refcount
        // machinery.
        if let ExprKind::FieldAccess { object, field } = &arg_expr.kind {
            if let ExprKind::Identifier(s) = &object.kind {
                if let (Some(slot), Some(struct_name)) = (
                    self.variables.get(s.as_str()).copied(),
                    self.var_type_names.get(s.as_str()).cloned(),
                ) {
                    if !self.shared_types.contains_key(struct_name.as_str()) {
                        if let Some(&st) = self.struct_types.get(struct_name.as_str()) {
                            if matches!(
                                slot.ty,
                                inkwell::types::BasicTypeEnum::StructType(held) if held == st
                            ) {
                                self.zero_struct_field_move_cap(slot.ptr, &struct_name, field);
                            }
                        }
                    }
                }
            }
            return;
        }
        let var_name = match &arg_expr.kind {
            ExprKind::Identifier(n) => n.as_str(),
            _ => return,
        };
        let slot = match self.variables.get(var_name) {
            Some(s) => *s,
            None => return,
        };
        let vec_ty = self.vec_struct_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();
        // Vec / String binding: zero the source's `cap` so the source's
        // `FreeVecBuffer` cleanup's `cap > 0` guard skips. The consumer
        // now owns the buffer.
        //
        // Guarded to a slot that holds the Vec/String struct INLINE
        // (`slot.ty == vec_ty`), exactly like the struct arm above. A
        // `ref Vec`/`ref String` param's slot is an 8-byte POINTER into the
        // caller's frame, not a 24-byte `{ptr,i64,i64}` — GEP-ing field 2
        // (`cap`, offset 16) off it and storing 8 bytes writes past the
        // alloca and corrupts the stack. That UB is invisible under `-O0`
        // (frame slack absorbs the write) but the optimizer weaponizes it:
        // a borrow-returning fn (`fn f(u: ref String) -> ref String { u }`)
        // segfaults under `-O2`/LLJIT, surfacing as empty output on the JIT
        // execution lane while the AOT oracle stayed green (B-2026-07-07-4).
        // A borrow takes no ownership, so there is nothing to move-null.
        if self.vec_elem_types.contains_key(var_name) {
            let holds_inline = matches!(
                slot.ty,
                inkwell::types::BasicTypeEnum::StructType(held) if held == vec_ty
            );
            if holds_inline {
                if let Ok(cap_ptr) =
                    self.builder
                        .build_struct_gep(vec_ty, slot.ptr, 2, "move.cap.p")
                {
                    let zero = i64_t.const_int(0, false);
                    let _ = self.builder.build_store(cap_ptr, zero);
                }
            }
            return;
        }
        // Tensor binding: null the source slot so its `FreeTensor`
        // cleanup's null-guard skips — the consumer (tail return, by-
        // value call arg, `let b = a;`) now owns the single heap block.
        // The null store is the Tensor analog of Vec's `cap = 0`.
        if self.tensor_var_infos.contains_key(var_name) {
            let _ = self.builder.build_store(slot.ptr, ptr_ty.const_null());
            return;
        }
        // Column binding: null the source slot so its `FreeColumn`
        // cleanup's null-guard skips — the consumer (tail return, by-
        // value call arg, `let b = a;`) now owns the control block + its
        // two buffers. The Column analog of the Tensor arm above.
        if self.column_var_infos.contains_key(var_name) {
            let _ = self.builder.build_store(slot.ptr, ptr_ty.const_null());
            return;
        }
        // DataFrame binding: null the source slot so its `FreeDataFrame`
        // cleanup's null-guard skips — the consumer (`let b = a;`, by-value
        // arg, tail return) now owns the control block + every column /
        // name it holds. The DataFrame analog of the Column arm above.
        if self.dataframe_var_infos.contains(var_name) {
            let _ = self.builder.build_store(slot.ptr, ptr_ty.const_null());
            return;
        }
        // Map / Set handle binding (slice 3r, gap (d) sibling): null the
        // source slot so its queued `FreeMapHandle` no-ops — the runtime
        // free (`karac_map_free` / `karac_map_free_with_drop_vec`)
        // null-checks the handle. Before this arm, `m.insert(k, inner)` /
        // a struct-literal Map field left the source's cleanup armed: the
        // inner handle was freed at the source's scope exit and the
        // consumer's stored copy dangled (SIGSEGV on read-back). The
        // null-store is BRANCH-SAFE (a runtime store on this path only),
        // unlike `suppress_map_cleanup_for_tail_identifier`'s compile-time
        // frame removal — a branch-buried consume must not leak the
        // not-taken path's handle. Gated to a plain pointer slot holding
        // the handle by value; a `ref Map` param's slot points into the
        // caller's frame and owns nothing.
        if let Some(tn) = self.var_type_names.get(var_name) {
            if matches!(tn.as_str(), "Map" | "Set")
                && !self.ref_params.contains_key(var_name)
                && slot.ty.is_pointer_type()
            {
                let _ = self.builder.build_store(slot.ptr, ptr_ty.const_null());
                return;
            }
        }
        // Shared-struct / shared-enum binding (RC-tier): the binding
        // holds a `ptr` whose pointee is the heap object with the i64
        // refcount header. The let-site `track_rc_var` queued a scope-
        // exit `RcDec` that, when fired against a freshly-constructed
        // local at RC=1, would drop the refcount to 0 and free the
        // allocation before the consumer (caller via tail-return,
        // `Map.insert`'s bucket, `Vec.push`'s buffer, etc.) can use it.
        // The Vec/String arm above can no-op the cleanup via the
        // `cap > 0` guard; the RC cleanup has no analogous guard (the
        // pointer slot is always followed). Instead, mirror the
        // `let b = a;` aliasing path at `stmts.rs:828`: emit an
        // `rc_inc` here so the *consumer* holds an independent ref,
        // and the source's queued `rc_dec` decrements the freshly-
        // incremented count back to the construction-time value (net
        // zero for the source's slot, +1 transferred to the consumer).
        // Symmetric to how the Vec arm's `cap = 0` makes the source's
        // free a no-op while the consumer assumes the buffer; here the
        // source's dec is balanced by a new inc, with the same net
        // effect of "consumer becomes the new owner of one ref".
        //
        // Without this: returning a `let n = SharedT { … }` from a
        // helper, or pushing one into a Vec/Map/Set, frees the
        // allocation at end-of-helper-scope before the caller / the
        // collection can read it (silent garbage value or a hang in
        // a follow-on RC inc loop, depending on what the freed memory
        // gets reused as). Closes bug #7 (`Map[K, SharedStruct]`
        // value insert + return) and the sibling cases
        // (`Vec[SharedStruct]`, plain `fn f() -> SharedT { let n = …; n }`).
        if let Some(type_name) = self.var_type_names.get(var_name).cloned() {
            if let Some(info) = self.shared_types.get(type_name.as_str()).cloned() {
                // C1b SomeRoot: `Some(<root>)` at fn tail is the
                // sanctioned structural transfer — the root queued NO
                // cleanup (the whole b2 count-free cluster leaves at
                // rc==1 per node), so the balancing inc this arm
                // normally emits (against the source's queued dec)
                // has nothing to balance and would leak one ref on
                // every chain head. The analysis guarantees this tail
                // is the root's only consumer position.
                if self
                    .cluster_root_info(var_name)
                    .is_some_and(|(_, _, mode)| mode == crate::ownership::ReturnedChain::SomeRoot)
                {
                    return;
                }
                if apply_shared_transfer {
                    if let Ok(loaded) = self.builder.build_load(ptr_ty, slot.ptr, "move.rc.load") {
                        let p = loaded.into_pointer_value();
                        self.emit_refcount_inc(var_name, info.heap_type, p);
                    }
                }
                return;
            }
        }
        // Value-type enum binding (#9, 2026-06-11): when the source is a
        // tracked non-shared enum whose active variant carries a heap
        // (`String`/`Vec`) payload, the `let`-site `track_enum_var` queued an
        // `EnumDrop` that frees that payload at scope exit. On a move-out
        // (tail return, `let g = f`, by-value arg, match-arm tail) the consumer
        // now owns the payload — without suppression both the source's
        // `EnumDrop` and the consumer free the same buffer (use-after-free /
        // double-free; surfaced by the self-hosting lexer's
        // `let token = keyword_or_ident(text); make_spanned(token)`). Zero the
        // `cap` word of EVERY variant's `VecOrString` field: the synthesized
        // drop switch's `cap > 0` guard then no-ops for whichever variant is
        // live at runtime. Zeroing dead variants' overlay words is harmless —
        // they are never read (the tag-switch enters only the live BB), and the
        // consumer already holds an independent value copy (this runs AFTER the
        // move loads the aggregate, identical ordering to the struct arm below,
        // which is why returning a struct-with-Vec already frees exactly once).
        // Mirrors `suppress_destructured_enum_payload_cleanup_at`'s cap-zeroing,
        // but for a whole-value move where the active variant is a runtime fact.
        if let Some(type_name) = self.var_type_names.get(var_name).cloned() {
            if let Some(layout) = self.enum_layouts.get(type_name.as_str()).cloned() {
                if !layout.is_shared {
                    self.zero_enum_payload_caps(slot.ptr, &layout);
                    return;
                }
            }
        }
        // Struct binding (slice γ, 2026-05-14): when the source is a
        // tracked non-shared struct, walk its fields and zero each
        // Vec/String field's `cap`. The struct's `StructDrop` cleanup
        // will then no-op on each freed field — the consumer (caller
        // / new binding / struct constructor) now owns the heap content.
        // Without this, returning a struct-with-Vec from a function
        // double-frees the inner buffer against the caller's own
        // tracked-struct cleanup. Map/Set field handles are NOT zeroed
        // by this helper today — they need a `null`-marker convention
        // through `karac_map_free` to no-op, which would be a separate
        // runtime change (filed under slice δ as the per-field K/V
        // type-info-aware drop work).
        if let Some(type_name) = self.var_type_names.get(var_name).cloned() {
            if self.struct_types.contains_key(&type_name) {
                // Recursive move-suppression: zero every transitive heap field's
                // cap (Vec/String, enum payloads post-#15/#19, nested structs
                // — #18's `Wrap { sp: Span { tok } }`) + the HTTP handle, so the
                // source struct's `StructDrop` no-ops and the consumer (caller /
                // new binding / struct or enum literal) is the sole owner.
                self.zero_struct_move_caps(slot.ptr, &type_name);
            }
        }
        // Tuple / anonymous-aggregate binding (B-2026-06-11-4 part a): a moved
        // tuple (`let u = t`, `return t`) shares its String/Vec buffers with the
        // destination; zero each heap field's `cap` (recursing into nested
        // aggregates) so the source's `track_tuple_var` StructDrop no-ops and
        // the destination owns the buffers. The named-struct arm above handles
        // the named case; this reaches the anonymous one its name-keyed walk
        // can't. Guarded off named structs (already handled, and double-zeroing
        // would be harmless but wasteful) and the Vec struct (String/Vec, the
        // early arm above).
        if let inkwell::types::BasicTypeEnum::StructType(agg_ty) = slot.ty {
            let named = self
                .var_type_names
                .get(var_name)
                .is_some_and(|n| self.struct_types.contains_key(n.as_str()));
            if !named && agg_ty != vec_ty {
                if self.aggregate_has_heap_field(agg_ty) {
                    // A directly-visible Vec/String field — the reliable
                    // LLVM-type walk (zeroes each `cap`). Kept FIRST so the
                    // proven Vec/String tuple-move suppression is unchanged; the
                    // name-reconstructed `TypeExpr`s below can't always re-derive
                    // `String`/`Vec` (an f-string element's inferred type name may
                    // differ), so routing this case through them regressed the
                    // by-value-tuple double-free guard.
                    self.zero_aggregate_field_caps(slot.ptr, agg_ty);
                } else if let Some(elem_tes) = self.tuple_var_elem_tes(var_name) {
                    // #23 — a Map/Set/enum-only tuple is INVISIBLE to the LLVM
                    // walk (all-i64 words, no `vec_struct` field). A tuple var
                    // owning a Map leaf (its scope-exit drop is the Part-A
                    // `synthesize_tuple_drop_fn_te`) moved into a struct literal
                    // field MUST null that handle, or both the tuple var's drop
                    // AND the owning struct's NestedTuple (#21) drop free the same
                    // handle (double-free). `zero_tuple_elem_caps` nulls Map
                    // handles / zeroes enum payload caps via the `TypeExpr`s
                    // reconstructed from the recorded per-element type names.
                    self.zero_tuple_elem_caps(slot.ptr, agg_ty, &elem_tes);
                }
            }
        }
    }

    /// #23 — reconstruct a tuple var's element `TypeExpr`s from the recorded
    /// per-element type NAMES (`tuple_var_elem_type_names`, populated at the
    /// let-binding site) as single-segment `Path`s, so the move-out suppressor
    /// can drive `zero_tuple_elem_caps` over Map / enum / Set leaves the
    /// LLVM-type walk can't see. A `None` name → empty `Path` (treated as a
    /// no-drop leaf — safe: worst case a missed cap-zero degrades to the
    /// pre-existing leak, never a double-free). Returns `None` when no names
    /// were recorded, so the caller keeps the Vec-only fallback.
    pub(super) fn tuple_var_elem_tes(&self, var_name: &str) -> Option<Vec<TypeExpr>> {
        let names = self.tuple_var_elem_type_names.get(var_name)?;
        Some(
            names
                .iter()
                .map(|n| TypeExpr {
                    kind: TypeKind::Path(crate::ast::PathExpr {
                        segments: n.clone().into_iter().collect(),
                        generic_args: None,
                        span: crate::token::Span::default(),
                    }),
                    span: crate::token::Span::default(),
                })
                .collect(),
        )
    }

    /// Ref-share at the call site for `Option[shared T]` Identifier
    /// args. Mirrors the shared-T branch of
    /// `suppress_source_vec_cleanup_for_arg` for the Option-wrapped
    /// shape: when an Identifier-typed argument's static type is
    /// `Option[shared T]`, emit a discriminant- and null-guarded
    /// `rc_inc` on the inner heap pointer so the consumer (callee
    /// param) holds an independent +1 ref. The caller's slot is
    /// NOT mutated — its queued `RcDecOption` still fires at
    /// scope-exit and balances the construction-time +1; the
    /// callee's `track_rc_option_var` cleanup (queued in
    /// `compile_function` for Option[shared T] params) balances
    /// the new +1 emitted here.
    ///
    /// IR shape (same as the Assign-arm's "inc new inner" branch
    /// in `compile_stmt`): load the slot's tag → branch on `Some`
    /// → load `w0` → `int_to_ptr` → null-guard → `emit_refcount_inc`.
    /// On `None` or null inner, all branches skip and no inc fires.
    ///
    /// Companion to `track_rc_option_var` on the callee side, which
    /// fires for `Option[shared T]` parameters in `compile_function`.
    /// The Caller's slot is preserved as-is so a call site that
    /// passes the same binding many times (e.g., `for i in 0..k {
    /// f(l1, l2); }`) sees the live chain on every call.
    ///
    /// No-op for non-Identifier args (call-result `make_chain(10)`,
    /// struct literals, fresh `Some(...)`), for non-shared
    /// Option[T] params, and for ref-bound aliasing — those carry
    /// their own ownership semantics (a Call's return value carries
    /// the callee's +1 directly into the caller's param slot;
    /// `track_rc_option_var` on the callee param owns the dec).
    /// Resolution uses `var_option_shared_heap` (populated by
    /// `track_rc_option_var` at the let-stmt and param-binding
    /// sites) as the single source of truth for "is this binding
    /// an Option[shared T]".
    /// FieldAccess companion to `share_option_shared_ref_for_arg`: when the
    /// call arg is `obj.field` whose static type is `Option[shared T]` and
    /// `obj` is an Identifier/`self`-bound shared struct, inc the inner of the
    /// already-loaded value `val`. The niche field read for such objects
    /// (`compile_field_access`'s `shared_type_for_expr` branch →
    /// `niche_load_option_field`) only LOADS the pointer without inc'ing, so
    /// passing it by value to a callee whose param queues an `RcDecOption`
    /// would over-decrement and free the sub-chain (recursive
    /// merge-two-sorted-lists `merge(n1.next, l2)`). Call-like objects
    /// (`get().next`) are excluded — their read goes through the call-chain
    /// branch that already incs — by requiring `shared_type_for_expr(obj)`.
    pub(super) fn share_option_shared_field_ref_for_arg(
        &self,
        arg_expr: &Expr,
        val: BasicValueEnum<'ctx>,
    ) {
        let ExprKind::FieldAccess { object, field } = &arg_expr.kind else {
            return;
        };
        let Some((type_name, _)) = self.shared_type_for_expr(object) else {
            return;
        };
        let Some(idx) = self
            .struct_field_names
            .get(&type_name)
            .and_then(|names| names.iter().position(|n| n == field))
        else {
            return;
        };
        let Some(field_te) = self
            .struct_field_type_exprs
            .get(&type_name)
            .and_then(|v| v.get(idx))
            .cloned()
        else {
            return;
        };
        let Some((_, inner_info)) = self.option_inner_shared_type_for_type_expr(&field_te) else {
            return;
        };
        self.emit_option_inner_rc_inc_for_loaded(val, inner_info.heap_type);
    }

    pub(super) fn share_option_shared_ref_for_arg(&self, arg_expr: &Expr) {
        let var_name = match &arg_expr.kind {
            ExprKind::Identifier(n) => n.as_str(),
            _ => return,
        };
        let heap_type = match self.var_option_shared_heap.get(var_name).copied() {
            Some(t) => t,
            None => return,
        };
        let slot = match self.variables.get(var_name) {
            Some(s) => *s,
            None => return,
        };
        let Some(fn_val) = self.current_fn else {
            return;
        };
        let option_ty = self.enum_layouts["Option"].llvm_type;
        let i64_t = self.context.i64_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let some_tag = self
            .enum_layouts
            .get("Option")
            .and_then(|l| l.tags.get("Some").copied())
            .unwrap_or(1);
        let some_tag_const = i64_t.const_int(some_tag, false);
        // Load tag, branch on Some.
        let Ok(tag_ptr) = self
            .builder
            .build_struct_gep(option_ty, slot.ptr, 0, "opt.arg.tag.p")
        else {
            return;
        };
        let Ok(tag) = self.builder.build_load(i64_t, tag_ptr, "opt.arg.tag") else {
            return;
        };
        let Ok(is_some) = self.builder.build_int_compare(
            IntPredicate::EQ,
            tag.into_int_value(),
            some_tag_const,
            "opt.arg.is_some",
        ) else {
            return;
        };
        let do_bb = self.context.append_basic_block(fn_val, "opt.arg.inc.do");
        let skip_bb = self.context.append_basic_block(fn_val, "opt.arg.inc.skip");
        let _ = self
            .builder
            .build_conditional_branch(is_some, do_bb, skip_bb);
        self.builder.position_at_end(do_bb);
        // Recover inner ptr from w0.
        let Ok(w0_ptr) = self
            .builder
            .build_struct_gep(option_ty, slot.ptr, 1, "opt.arg.w0.p")
        else {
            self.builder.position_at_end(skip_bb);
            return;
        };
        let Ok(w0) = self.builder.build_load(i64_t, w0_ptr, "opt.arg.w0") else {
            self.builder.position_at_end(skip_bb);
            return;
        };
        let Ok(inner) = self
            .builder
            .build_int_to_ptr(w0.into_int_value(), ptr_ty, "opt.arg.inner")
        else {
            self.builder.position_at_end(skip_bb);
            return;
        };
        let Ok(is_null) = self.builder.build_is_null(inner, "opt.arg.is_null") else {
            self.builder.position_at_end(skip_bb);
            return;
        };
        let real_do_bb = self
            .context
            .append_basic_block(fn_val, "opt.arg.inc.real_do");
        let _ = self
            .builder
            .build_conditional_branch(is_null, skip_bb, real_do_bb);
        self.builder.position_at_end(real_do_bb);
        self.emit_refcount_inc(var_name, heap_type, inner);
        let _ = self.builder.build_unconditional_branch(skip_bb);
        self.builder.position_at_end(skip_bb);
    }

    /// B-2026-06-15 (#226 invert-binary-tree). An enum-variant constructor
    /// (`Some(x)` / `Variant(x)`) whose payload `x` reads a bare `shared` value
    /// out of a `Vec` element (`Some(nodes[i])`) must rc-inc it: the new enum
    /// owns an independent reference, but a `Vec[shared]` element read shallow-
    /// aliases without an inc (`clone_owned_vec_index_element` treats a bare
    /// shared element as trivially copyable, and the ctor never inc'd it).
    /// `rhs_yields_fresh_ref` classifies the ctor as fresh, so the return /
    /// let-bind / field consumers SKIP their own receive-inc; without this
    /// self-inc the payload is under-counted and freed when the source `Vec`
    /// (whose correct per-element dec landed in 0890627c / B-2026-06-14-28)
    /// drops, leaving the enum dangling — a use-after-free (non-deterministic
    /// garbage / crash; masked by the pre-0890627c `Vec[shared]`-element leak).
    /// SCOPED TO the `v[i]` index by `bare_shared_heap_type_for_expr`: a bare
    /// Identifier / FieldAccess payload (`Some(node)`, `Some(head)` — fresh
    /// locals moved into a list) is already owned and would DOUBLE-count here.
    /// Fresh payloads (`Some(make())`, `Some(N { .. })`) are skipped outright.
    pub(super) fn share_bare_shared_ctor_payload(
        &self,
        arg_expr: &Expr,
        val: BasicValueEnum<'ctx>,
    ) {
        if self.rhs_yields_fresh_ref(arg_expr) {
            return;
        }
        let BasicValueEnum::PointerValue(ptr) = val else {
            return;
        };
        let Some(heap_type) = self.bare_shared_heap_type_for_expr(arg_expr) else {
            return;
        };
        let Some(fn_val) = self.current_fn else {
            return;
        };
        // Null-guard (a moved-out source can leave a null sentinel) then inc.
        let Ok(is_null) = self.builder.build_is_null(ptr, "ctorpl.isnull") else {
            return;
        };
        let do_bb = self.context.append_basic_block(fn_val, "ctorpl.inc.do");
        let skip_bb = self.context.append_basic_block(fn_val, "ctorpl.inc.skip");
        let _ = self
            .builder
            .build_conditional_branch(is_null, skip_bb, do_bb);
        self.builder.position_at_end(do_bb);
        self.emit_refcount_inc_by_type(heap_type, ptr);
        let _ = self.builder.build_unconditional_branch(skip_bb);
        self.builder.position_at_end(skip_bb);
    }

    /// Resolve the heap (RC) layout of a bare `shared` value read by a `v[i]`
    /// Vec-element index whose element type is a bare shared struct/enum —
    /// the genuinely uncovered gap (a `Vec[shared]` element read shallow-
    /// aliases without an inc; `clone_owned_vec_index_element` treats a bare
    /// shared element as trivially copyable, and the ctor never inc'd it).
    /// SCOPED TO INDEX ONLY: a bare Identifier / `self` / FieldAccess payload
    /// is already accounted for by the existing move / consumer-inc paths (a
    /// fresh local moved into a list, a niche field read, …), so inc'ing it
    /// here too DOUBLE-counts and leaks — `from_arr`'s `tail.next = Some(node)`
    /// / `Some(head)` (node/head fresh locals) are the canonical
    /// false-positives. `None` for any other shape, a range slice, a
    /// non-named-Vec object, or a non-shared element.
    pub(super) fn bare_shared_heap_type_for_expr(
        &self,
        expr: &Expr,
    ) -> Option<inkwell::types::StructType<'ctx>> {
        let ExprKind::Index { object, index } = &expr.kind else {
            return None;
        };
        if matches!(&index.kind, ExprKind::Range { .. }) {
            return None;
        }
        let ExprKind::Identifier(name) = &object.kind else {
            return None;
        };
        let elem_te = self.var_elem_type_exprs.get(name.as_str())?;
        let TypeKind::Path(p) = &elem_te.kind else {
            return None;
        };
        let seg = p.segments.last()?;
        let info = self.shared_types.get(seg.as_str())?;
        Some(info.heap_type)
    }

    /// Compound-payload enum codegen (CP4 helper) — decompose an
    /// arbitrary `BasicValueEnum` into exactly `num_words` i64 words
    /// suitable for storage in an enum payload area. Primitives (bool /
    /// int / float / pointer) always produce one word via `coerce_to_i64`;
    /// `num_words == 1` therefore short-circuits to the existing
    /// behaviour. Aggregates (String / Vec / user struct / tuple)
    /// destructure via `extract_value` over their LLVM-field layout and
    /// recurse on each field.
    ///
    /// If the supplied value's natural word count is **smaller** than the
    /// requested `num_words` the result is zero-padded (the common
    /// under-shoot — a primitive into Option's 3-word area, or a
    /// conservative `payload_word_count_for_type_expr` over-estimate).
    ///
    /// If it is **larger** the value is **heap-boxed**: `T` is malloc'd,
    /// stored, and the box pointer occupies word 0 (the rest of the area
    /// stays zero). A seeded enum (`Option` = 3 payload words, `Result` =
    /// 5) has a fixed payload area; a struct / tuple wider than that —
    /// which `Vec.pop()` / `Map.get()` / a `-> Option[Wide]` return all
    /// route through here — used to truncate and hand back garbage (a
    /// silent miscompile), then briefly errored (`E_ENUM_PAYLOAD_OVERSIZED`),
    /// and is now boxed natively. The unpack and drop sites recompute the
    /// same `llvm_type_word_count(T) > area` predicate and `inttoptr` word
    /// 0 to load / free `T`; the decision is a pure function of the static
    /// type so all sites stay coherent. See
    /// `docs/spikes/oversized-enum-payload.md`. Genuine nested *enum*
    /// payloads are still rejected earlier by the typechecker's
    /// `E_ENUM_NESTED_ENUM_PAYLOAD`, so the boxed surface is oversized
    /// struct / tuple payloads.
    pub(super) fn coerce_to_payload_words(
        &self,
        val: BasicValueEnum<'ctx>,
        num_words: usize,
    ) -> Result<Vec<inkwell::values::IntValue<'ctx>>, String> {
        // Primitive fast path — ONLY when `val` genuinely fits one word.
        //
        // #49 (phase-12 self-hosting): a struct whose enum-payload AREA was
        // under-sized to 1 word still arrives here with a multi-word aggregate
        // `val`. The canonical case is a struct whose only field is an
        // `Option[T]`/`Result[T,E]` (`struct Block { tail: Option[Expr] }` used
        // as `Expr.Blk(Block)`): `payload_word_count_for_type_expr` routes that
        // Option field through the enum-in-enum carve-out and returns 1, so the
        // variant's `field_word_offsets` hands us `num_words == 1` for a value
        // whose real LLVM width is 4. Taking the scalar fast path then calls
        // `coerce_to_i64` on the 4-word struct, which recurses into field 0 (a
        // multi-field sub-struct) and collapses to `0` — the payload is silently
        // dropped, and since the unpack/drop sites independently compute
        // `llvm_type_word_count(T) > area` and treat it as BOXED, they `inttoptr`
        // that `0` → null deref → SIGSEGV. Guarding the fast path on the value's
        // real width lets a wide-but-undersized payload fall through to the
        // decompose-and-box path below (`out.len() > num_words` → box), which is
        // exactly what unpack (`reconstruct_payload_value`) and drop expect, so
        // all three sites stay coherent. A genuine scalar (width ≤ 1) keeps the
        // fast path.
        if num_words <= 1 && Self::llvm_type_word_count(val.get_type()) <= 1 {
            return Ok(vec![self.coerce_to_i64(val)?]);
        }
        let mut out: Vec<inkwell::values::IntValue<'ctx>> = Vec::with_capacity(num_words.max(1));
        match val {
            BasicValueEnum::StructValue(sv) => {
                let n_fields = sv.get_type().count_fields();
                for i in 0..n_fields {
                    let f = self
                        .builder
                        .build_extract_value(sv, i, "pl.f")
                        .map_err(|e| {
                            format!(
                                "coerce_to_payload_words: extract_value failed at field {}: {:?}",
                                i, e
                            )
                        })?;
                    // Recurse: a struct field can itself be an aggregate
                    // (e.g. a user struct whose field is a String). Each
                    // top-level LLVM field of `sv` contributes its own
                    // word count to the running total. Push every word —
                    // the oversize check below sees the true count.
                    //
                    // #44 (phase-12 parser slice 2a): use the recursive WORD
                    // count, not `count_fields()`. A nested struct field (a
                    // `Block {Vec, Option, Span}` — 3 fields but 11 words —
                    // reached via `IfExpr.then_block: Block`) has
                    // `count_fields() == 3` but flattens to 11 words, so passing
                    // 3 as the recursion's `num_words` made `out.len()(11) >
                    // num_words(3)` fire the oversize-BOXING path INSIDE the
                    // recursion — the sub-struct got heap-boxed (a pointer in
                    // word 0) while the unpack (`reconstruct_payload_value`)
                    // reads it as inline words → wrong value. `llvm_type_word_count`
                    // recurses, so `out.len() == num_words` and the sub-struct
                    // flattens inline (boxing stays a top-level decision).
                    let sub_count = match f {
                        BasicValueEnum::StructValue(ssv) => {
                            Self::llvm_type_word_count(ssv.get_type().into())
                        }
                        BasicValueEnum::ArrayValue(av) => {
                            Self::llvm_type_word_count(av.get_type().into())
                        }
                        _ => 1,
                    };
                    let sub_words = if sub_count <= 1 {
                        vec![self.coerce_to_i64(f)?]
                    } else {
                        self.coerce_to_payload_words(f, sub_count)?
                    };
                    out.extend(sub_words);
                }
            }
            BasicValueEnum::ArrayValue(av) => {
                let len = av.get_type().len();
                for i in 0..len {
                    let f = self
                        .builder
                        .build_extract_value(av, i, "pl.a")
                        .map_err(|e| {
                            format!(
                                "coerce_to_payload_words: extract_value (array) failed at {}: {:?}",
                                i, e
                            )
                        })?;
                    out.push(self.coerce_to_i64(f)?);
                }
            }
            _ => {
                out.push(self.coerce_to_i64(val)?);
            }
        }
        // Oversized payload: heap-box the value and store the box pointer
        // in word 0 (the rest of the area stays zero). A seeded enum
        // (`Option` = 3 payload words, `Result` = 5) has a fixed area; a
        // struct / tuple `T` wider than it — which `Vec.pop()` /
        // `Map.get()` / a `-> Option[Wide]` return all route through here
        // — cannot be inlined. Boxing keeps the common small payload
        // byte-identical and confines the heap indirection to the wide
        // case. The unpack (`reconstruct_payload_value`,
        // `rebuild_value_from_payload_words`) and drop sites recompute the
        // SAME `llvm_type_word_count(T) > area` predicate — here it is
        // `out.len() > num_words` — and `inttoptr` word 0 to load / free
        // `T`. The decision is a pure function of the static type, so all
        // sites stay coherent by construction. See
        // docs/spikes/oversized-enum-payload.md.
        let i64_t = self.context.i64_type();
        if out.len() > num_words {
            let val_ty = val.get_type();
            let raw_size = val_ty.size_of().ok_or_else(|| {
                "coerce_to_payload_words: cannot size oversized enum payload for boxing".to_string()
            })?;
            let size = if raw_size.get_type().get_bit_width() == 64 {
                raw_size
            } else {
                self.builder
                    .build_int_z_extend(raw_size, i64_t, "enumbox.sz64")
                    .unwrap()
            };
            let box_ptr = self
                .builder
                .build_call(self.malloc_fn, &[size.into()], "enumbox")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_pointer_value();
            self.builder.build_store(box_ptr, val).unwrap();
            let box_word = self
                .builder
                .build_ptr_to_int(box_ptr, i64_t, "enumbox.w")
                .unwrap();
            let mut boxed = Vec::with_capacity(num_words);
            boxed.push(box_word);
            while boxed.len() < num_words {
                boxed.push(i64_t.const_int(0, false));
            }
            return Ok(boxed);
        }
        // Zero-pad the under-shoot to the exact width.
        while out.len() < num_words {
            out.push(i64_t.const_int(0, false));
        }
        Ok(out)
    }

    /// Build an `Option[V]` aggregate at the merge BB via per-payload-word phis.
    /// Mirrors the `Vec.pop` precedent at line 8588: 1 tag phi + 3 word phis,
    /// then `build_insert_value` at fields 0..=3. Caller is responsible for
    /// having computed `some_payload_words` (length 3, via
    /// `coerce_to_payload_words(elem_val, 3)`) inside the some-end BB and
    /// having positioned the builder at the merge BB. None-side fills all
    /// payload words with 0; tag is 1 on the some side and 0 on the none side.
    pub(super) fn build_option_some_via_phis(
        &self,
        some_payload_words: &[inkwell::values::IntValue<'ctx>],
        some_end_bb: inkwell::basic_block::BasicBlock<'ctx>,
        none_bb: inkwell::basic_block::BasicBlock<'ctx>,
        name_prefix: &str,
    ) -> BasicValueEnum<'ctx> {
        let i64_t = self.context.i64_type();
        let zero = i64_t.const_int(0, false);
        let one = i64_t.const_int(1, false);
        let option_ty = self.enum_layouts["Option"].llvm_type;

        let tag_phi = self
            .builder
            .build_phi(i64_t, &format!("{name_prefix}.tag"))
            .unwrap();
        tag_phi.add_incoming(&[(&zero, none_bb), (&one, some_end_bb)]);

        let mut word_phis: Vec<inkwell::values::PhiValue<'ctx>> =
            Vec::with_capacity(some_payload_words.len());
        for (i, w) in some_payload_words.iter().enumerate() {
            let phi = self
                .builder
                .build_phi(i64_t, &format!("{name_prefix}.w{i}"))
                .unwrap();
            phi.add_incoming(&[(&zero, none_bb), (w, some_end_bb)]);
            word_phis.push(phi);
        }

        // Zero-init so `None`'s unused payload words stay `0` (sound `==`).
        let mut agg: BasicValueEnum<'ctx> = option_ty.const_zero().into();
        agg = self
            .builder
            .build_insert_value(
                agg.into_struct_value(),
                tag_phi.as_basic_value(),
                0,
                &format!("{name_prefix}.tag.f"),
            )
            .unwrap()
            .into_struct_value()
            .into();
        for (i, phi) in word_phis.iter().enumerate() {
            agg = self
                .builder
                .build_insert_value(
                    agg.into_struct_value(),
                    phi.as_basic_value(),
                    (i + 1) as u32,
                    &format!("{name_prefix}.w{i}.f"),
                )
                .unwrap()
                .into_struct_value()
                .into();
        }
        agg
    }

    /// Coerce an arbitrary value to i64 for storage in an enum payload word.
    pub(super) fn coerce_to_i64(
        &self,
        val: BasicValueEnum<'ctx>,
    ) -> Result<inkwell::values::IntValue<'ctx>, String> {
        let i64_t = self.context.i64_type();
        match val {
            BasicValueEnum::IntValue(iv) => {
                let w = iv.get_type().get_bit_width();
                if w == 64 {
                    Ok(iv)
                } else if w < 64 {
                    Ok(self.builder.build_int_z_extend(iv, i64_t, "zext").unwrap())
                } else {
                    Ok(self.builder.build_int_truncate(iv, i64_t, "trunc").unwrap())
                }
            }
            BasicValueEnum::FloatValue(fv) => Ok(self
                .builder
                .build_bit_cast(fv, i64_t, "fcast")
                .unwrap()
                .into_int_value()),
            BasicValueEnum::PointerValue(pv) => {
                Ok(self.builder.build_ptr_to_int(pv, i64_t, "ptoi").unwrap())
            }
            // Single-field structs (e.g. `MyError { code: i64 }`) collapse to
            // their field-0 value so the result fits a uniform i64 payload
            // word. Multi-field structs intentionally fall through to the
            // zero default — there's no faithful single-i64 encoding for
            // them, and any such case here is a codegen-shape bug elsewhere
            // that we'd rather see surface than paper over.
            BasicValueEnum::StructValue(sv) if sv.get_type().count_fields() == 1 => {
                let field = self
                    .builder
                    .build_extract_value(sv, 0, "struct.f0")
                    .unwrap();
                self.coerce_to_i64(field)
            }
            _ => Ok(i64_t.const_int(0, false)),
        }
    }

    /// Look up a unit enum variant by identifier name and construct its value.
    pub(super) fn try_unit_enum_variant(&self, name: &str) -> Option<BasicValueEnum<'ctx>> {
        // Symmetric to `try_compile_enum_variant`'s user-declared-vs-
        // seeded preference: when a variant name (`None` / `Some` /
        // `Ok` / `Err`) collides between a user-defined enum and the
        // seeded built-ins, pick the user-declared one. HashMap
        // iteration order is non-deterministic otherwise, and the
        // wider seeded `Option` layout would mis-construct a value
        // for a user-defined `MyOption.None`.
        let (mut user_pick, mut seed_pick) = (None, None);
        for (enum_name, layout) in &self.enum_layouts {
            if let Some(&tag) = layout.tags.get(name) {
                if layout.field_counts.get(name).copied().unwrap_or(0) == 0 {
                    if self.seeded_enum_names.contains(enum_name) {
                        seed_pick.get_or_insert((enum_name.clone(), tag, layout));
                    } else {
                        user_pick.get_or_insert((enum_name.clone(), tag, layout));
                    }
                }
            }
        }
        let (enum_name, tag, layout) = user_pick.or(seed_pick)?;
        let i64_t = self.context.i64_type();

        // Shared enum: heap-allocate.
        if let Some(info) = self.shared_types.get(&enum_name) {
            let ptr = self.emit_rc_alloc(info.heap_type);
            let tag_ptr = self
                .builder
                .build_struct_gep(info.heap_type, ptr, 1, "sh_tag")
                .unwrap();
            self.builder
                .build_store(tag_ptr, i64_t.const_int(tag, false))
                .unwrap();
            return Some(ptr.into());
        }

        // Zero-init so a multi-word enum's unit variant has `0` payload words
        // (not undef) — makes `V::B == V::B` sound under the word-wise `==`.
        let mut agg = layout.llvm_type.const_zero();
        agg = self
            .builder
            .build_insert_value(agg, i64_t.const_int(tag, false), 0, "tag")
            .unwrap()
            .into_struct_value();
        Some(agg.into())
    }

    /// Compile `Vector[T, N](lane0, …, lane{N-1})` into an `<N x T>` SIMD value
    /// (design.md § Portable SIMD). Builds the vector by inserting each compiled
    /// lane argument into an undef vector at its index. The typechecker has
    /// already verified the arg count equals `N` and each lane's type matches
    /// `T`, so no shape re-validation is needed here — but each compiled lane
    /// still needs the standard literal-width boundary coercion
    /// (`coerce_scalar_to_type`): a bare `0.5` / `1` lane lowers at the
    /// literal default width (f64 / i64), and inserting it raw mislowered
    /// `Vector[f32, 4](0.5, …)` as `<4 x double>` — caught by the LLVM
    /// verifier only once the vector met a correctly-typed operand
    /// (surfaced 2026-06-07 by the WASM SIMD-128 slice's E2E fixture;
    /// target-independent, same failure on native).
    fn compile_vector_construction(
        &mut self,
        generic_args: &[GenericArg],
        args: &[CallArg],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let vec_ty = self
            .llvm_vector_type(&Some(generic_args.to_vec()))
            .ok_or_else(|| "Vector construction: could not lower Vector[T, N] type".to_string())?;
        let BasicTypeEnum::VectorType(vt) = vec_ty else {
            return Err("Vector construction: lowered type is not an LLVM vector".to_string());
        };
        let i32_ty = self.context.i32_type();
        let mut acc = vt.get_undef();
        for (i, arg) in args.iter().enumerate() {
            let lane = self.compile_expr(&arg.value)?;
            let lane = self.coerce_scalar_to_type(lane, vt.get_element_type());
            let idx = i32_ty.const_int(i as u64, false);
            acc = self
                .builder
                .build_insert_element(acc, lane, idx, "vec.ins")
                .map_err(|e| format!("Vector construction insertelement failed: {e}"))?;
        }
        Ok(acc.into())
    }
}
