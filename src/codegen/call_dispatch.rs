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
    BasicMetadataValueEnum, BasicValue, BasicValueEnum, CallSiteValue, FunctionValue, PointerValue,
};
use inkwell::{AddressSpace, IntPredicate};

use super::declarations::KARAC_PARK_ON_FD;
use super::helpers::{expr_as_type_expr_codegen, match_with_provider_call, match_with_span_call};
use super::state::{LayoutId, UserDropKind};

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
                if let Some(inner_te) = self.fn_sig.fn_ref_return_inner.get(n).cloned() {
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
            if self.target_abi.boxed_export_names.contains(n) {
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
            if self.target_abi.abi_adapted_export_names.contains(n) {
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

        // `<C-like #[repr(intN)] enum>.try_from(v)` — design.md § Enum
        // Discriminant Runtime Surface (B-2026-08-21-26). Sibling of the
        // refinement intercept above and of the interpreter's arm in
        // `eval_call.rs`; a non-enum head returns `None` and falls through.
        if let ExprKind::Path { segments, .. } = &callee.kind {
            if segments.len() == 2 && segments[1] == "try_from" {
                if let Some(arg) = args.first() {
                    if let Some(v) =
                        self.compile_enum_try_from(&segments[0].clone(), &arg.value, call_span)?
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
                && self
                    .contract_state
                    .distinct_bases
                    .contains_key(&segments[0])
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
            return self.compile_with_provider(&resource, provider_expr, closure_expr, call_span);
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
                .build_call(
                    self.runtime_fns.karac_tracing_get_active_span_fn,
                    &[],
                    "active_span",
                )
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
                        | ExprKind::ByteStringLit(_)
                );
                if is_literal_index && self.mono_state.generic_fns.contains_key(name) {
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

        // B-2026-07-11-1: `ptr.null[u8]()` / `ptr.dangling[T]()` — a dotted
        // pointer-module builtin with a SINGLE type-arg turbofish parses as
        // `Call { callee: Index { object: FieldAccess{Identifier("ptr"), fn},
        // index: <type> } }` (the same indexing-vs-turbofish ambiguity
        // `size_of[T]()` hits). The plain method form `ptr.null()` is lowered by
        // `compile_ptr_module_call` via the method-call path, but the turbofish
        // Index-callee shape never reached it and fell through to the i64 default
        // — so the binding registered as `i64` and a later `p.read()` panicked
        // (`expected PointerValue`). Route it to the same pointer intrinsic (the
        // constructors ignore the type arg — a `null`/`dangling` value is
        // pointee-agnostic), producing a real `ptr` so the binding is a pointer.
        if let ExprKind::Index { object, .. } = &callee.kind {
            if let ExprKind::FieldAccess {
                object: inner,
                field,
            } = &object.kind
            {
                if let ExprKind::Identifier(module) = &inner.kind {
                    if module == "ptr" && !self.variables.contains_key("ptr") {
                        if let Some(value) = self.compile_ptr_module_call(field, args)? {
                            return Ok(value);
                        }
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
                if let Some(layout) = self.type_decls.enum_layouts.get("Json") {
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
                // A GENERIC user type's ASSOCIATED fn (`W.make(7)` for
                // `impl[T] W[T]`) lives ONLY in `generic_fns` — the impl-method
                // declaration pass registers it there rather than as a concrete
                // module function (a bare `W.make` would get the all-`i64`
                // default). `compile_assoc_call`'s `module.get_function("W.make")`
                // lookup therefore misses it and the call fell through to that
                // method's `Ok(const 0)` tail, SILENTLY returning a ZEROED struct
                // (B-2026-07-11-25). Route it through the same monomorphization
                // pipeline a generic free fn / instance method uses
                // (B-2026-07-03-15/-23) — `infer_type_args` binds the type params
                // from the call. Keyed by the LITERAL segment name, so a
                // `T.assoc()` inside a monomorph (the segment is the param name
                // `T`, never a registered key) is unaffected and keeps its
                // `compile_assoc_call` type-subst remap.
                let qualified = format!("{}.{}", segments[0], segments[1]);
                if self.mono_state.generic_fns.contains_key(&qualified) {
                    // Explicit turbofish args (`W[i64].make(..)`) ride in the path
                    // itself and are rare for an associated ctor; the common
                    // `W.make(7)` infers its type params from the args
                    // (`infer_type_args`) and a no-arg `S.new()` from the
                    // typechecker's recorded per-call type args.
                    return self.compile_generic_call(&qualified, args, None, call_span);
                }
                // B-2026-08-02-12 — `Map.new()` / `Set.new()` / `SortedMap.new()`
                // / `SortedSet.new()` in a non-`let` EXPRESSION position (a
                // `v.push(Map.new())` arg, a `Vec.filled(n, Map.new())` fill
                // value, a call arg) used to fall through `compile_assoc_call`
                // to its `i64 0` tail — a NULL handle stored wherever a real
                // map was expected, segfaulting at the first element use
                // (`smap[0].insert(..)`, `smap[1].len()`). The `let` /
                // struct-literal-field paths intercept before ever compiling
                // the expression (annotation-driven), so the only shapes that
                // reach here are exactly the broken ones. Build the real
                // handle from the typechecker's span-keyed inferred type; when
                // that entry is missing, fail LOUD with an actionable hint —
                // the null handle WILL crash at runtime, and a compile error
                // beats a segv.
                if segments[1] == "new"
                    && matches!(
                        segments[0].as_str(),
                        "Map" | "Set" | "SortedMap" | "SortedSet"
                    )
                    && args.is_empty()
                {
                    let key = (call_span.offset, call_span.length);
                    // The recorded TE must carry CONCRETE generic args. An
                    // `Error`-kinded arg is the render of an unresolved
                    // inference var — building from it would default the
                    // key/value to i64 and produce a handle whose key size
                    // and hash are silently wrong for `Map[String, _]`
                    // (lookups MISS, key buffers leak at free). Loud-bail
                    // instead so the gap is a compile error, not a
                    // miscompiled map.
                    let usable_te = self
                        .drop_rc
                        .owned_temp_drops
                        .get(&key)
                        .cloned()
                        .filter(|te| match &te.kind {
                            TypeKind::Path(p) => p.generic_args.as_ref().is_some_and(|ga| {
                                !ga.is_empty()
                                    && ga.iter().all(|g| match g {
                                        crate::ast::GenericArg::Type(t) => {
                                            !matches!(t.kind, TypeKind::Error)
                                        }
                                        _ => true,
                                    })
                            }),
                            _ => false,
                        });
                    if let Some(te) = usable_te {
                        if let Some(h) = self.build_map_new_handle_from_type_expr(&te) {
                            return Ok(h.into());
                        }
                    }
                    return Err(format!(
                        "`{0}.new()` in this position has no concrete key/value type \
                         visible to codegen; bind it first (`let m: {0}[..] = {0}.new()`) \
                         and pass the binding instead (B-2026-08-02-12)",
                        segments[0]
                    ));
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

        // B-2026-08-01-26 — a LOCAL closure binding shadows every name-keyed
        // intercept and fn-table dispatch below. Before this check ran FIRST,
        // `let take = |x| ..; take(v)` compiled a direct call to the spliced
        // stdlib `std.mem::take` (the generic-fn path at this function's tail
        // ran before the `closure_fn_types` indirect-call check), silently
        // returning `*dest`'s first word — pointer garbage for a String arg —
        // while the typechecker and interpreter both resolved the local
        // closure (locals-first), a three-way divergence. The typechecker's
        // own intercepts already guard with `local_scope.lookup(name)`; this
        // is the codegen twin of that rule. Gated on a LIVE local slot
        // (`variables`) so a `closure_fn_types` entry from another scope
        // cannot hijack an ordinary fn call, and on no explicit generic args
        // (a closure call never has a turbofish). Module-binding closures
        // keep their existing dispatch via the later `closure_fn_types`
        // check, unchanged.
        if explicit_generic_args.is_none()
            && self.closure_state.closure_fn_types.contains_key(&name)
            && self.variables.contains_key(name.as_str())
        {
            return self.compile_closure_call(&name, args);
        }

        // `Vector[T, N](lane0, …)` SIMD construction (design.md § Portable
        // SIMD). Intercepted before the generic-fn path — `Vector` is a
        // builtin type, not a user function. Builds an `<N x T>` value via an
        // insertelement chain.
        if name == "Vector" {
            if let Some(ga) = explicit_generic_args.as_deref() {
                return self.compile_vector_construction(ga, args);
            }
        }

        // `eprintln` belongs here with its stdout siblings: it is a prelude
        // free function (`PRELUDE_FUNCTIONS`), not a resource method, so no
        // later arm claims it. Without this it fell through to the
        // unknown-callee fallback and compiled to nothing at all — every
        // stderr write silently dropped under both JIT and AOT while the
        // interpreter printed it (B-2026-08-23-14). The qualified
        // `Stderr.println` form was never affected; it lowers on the ambient
        // method path (`compile_ambient_ffi`).
        if name == "println" || name == "print" || name == "eprintln" {
            return self.compile_print(&name, args);
        }

        // `dbg(x)` — emit the diagnostic line, hand back `x` (design.md §
        // `dbg()`: an identity function with a side effect). See
        // `src/codegen/dbg.rs`.
        //
        // This arm previously REFUSED, because the fallback below hands back a
        // constant `i64 0` without compiling the argument at all — for a
        // value-returning builtin that is not a dropped diagnostic but a
        // miscompile: `dbg(41) + 1` evaluated to 1, `dbg(side_effect())` never
        // called `side_effect`, and once the typechecker learned dbg's real
        // (identity) type, binding its result to a `String` segfaulted the
        // binary on first read — a 0 where a data pointer belongs
        // (B-2026-08-23-16). `compile_dbg` keeps that fail-closed posture for
        // any shape it cannot render: it returns an Err naming the case rather
        // than printing a placeholder (B-2026-08-23-18).
        if name == "dbg" {
            return self.compile_dbg(args, call_span);
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

        // Diverging prelude builtins `todo()` / `unreachable()` / `panic()`
        // (type `!`). They print a panic message + `exit(101)`, then terminate
        // the block with `unreachable` so no `ret` is emitted after them.
        // Lowered here — before the generic-call / unknown-callee fallback that
        // would otherwise hand back an `i64 0` placeholder and let the function
        // tail emit `ret i64 0` against a non-i64 return type (the historical
        // `fn boom() -> FakeClock { unreachable() }` module-verification
        // failure). Mirrors the interpreter's `eval_builtin_diverge`.
        if name == "todo" || name == "unreachable" || name == "panic" {
            return self.compile_diverge(&name, args);
        }

        // Volatile MMIO intrinsics `volatile_read(src)` /
        // `volatile_write(dst, value)` (`runtime/stdlib/intrinsics.kara`).
        // Lower to a volatile load / store through the raw-pointer argument,
        // sized by the pointee type recorded for every pointer-typed
        // expression in `raw_pointer_pointee_types` (the arg has its own span,
        // distinct from the call span, so no method-form span collision). The
        // recursive `#[compiler_builtin]` placeholder bodies never lower;
        // `unsafe`-context is enforced by the `unsafe_op` lint upstream.
        if name == "volatile_read" && args.len() == 1 {
            return self.compile_volatile_read(&args[0].value);
        }
        if name == "volatile_write" && args.len() == 2 {
            return self.compile_volatile_write(&args[0].value, &args[1].value);
        }

        // Standalone atomic barriers `fence(order)` / `compiler_fence(order)`
        // (`runtime/stdlib/intrinsics.kara`). Lower to an LLVM `fence`: `fence`
        // is cross-thread; `compiler_fence` uses the singlethread syncscope (a
        // compiler-only reordering barrier). `order` must be a compile-time
        // `MemoryOrdering` literal — an LLVM fence carries a static ordering.
        if name == "fence" && args.len() == 1 {
            return self.compile_atomic_fence(&args[0].value, false);
        }
        if name == "compiler_fence" && args.len() == 1 {
            return self.compile_atomic_fence(&args[0].value, true);
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
        // `ref_eq(a, b) -> bool` — reference-identity for `shared` handles
        // (design.md § Equality Semantics). A shared value is a pointer to its
        // RC heap object, so identity is `icmp eq` on the two pointers.
        // Intercepted before the generic-fn path so the `#[compiler_builtin]`
        // stub never lowers; the reads are non-consuming (no rc bump, no drop).
        if name == "ref_eq" && args.len() == 2 {
            let a = self.compile_expr(&args[0].value)?;
            let b = self.compile_expr(&args[1].value)?;
            let (Ok(ap), Ok(bp)) = (
                inkwell::values::BasicValueEnum::try_into(a),
                inkwell::values::BasicValueEnum::try_into(b),
            ) else {
                return Err(
                    "ref_eq expects two `shared` handles (reference-identity comparison)"
                        .to_string(),
                );
            };
            let ap: inkwell::values::PointerValue<'ctx> = ap;
            let bp: inkwell::values::PointerValue<'ctx> = bp;
            let eq = self
                .builder
                .build_int_compare(inkwell::IntPredicate::EQ, ap, bp, "ref_eq")
                .unwrap();
            return Ok(eq.into());
        }

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
        if self.contract_state.distinct_bases.contains_key(&name) {
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
        if self.mono_state.generic_fns.contains_key(&name) {
            return self.compile_generic_call(
                &name,
                args,
                explicit_generic_args.as_deref(),
                call_span,
            );
        }

        // Check if this is an indirect call through a closure variable.
        if self.closure_state.closure_fn_types.contains_key(&name) {
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
            // body (`self.conc.coro_spawn_slot` is `Some`), the runtime owns the
            // completion slot and binds it to the `TaskHandle`. We hand that
            // slot to the ramp and return *without* waiting — the worker is
            // freed while the coroutine stays parked. Otherwise (the inline
            // drive) the caller owns the slot: allocate it, ramp, block on it,
            // free it.
            let spawn_slot = self.conc.coro_spawn_slot;
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
            let ref_flags = self
                .fn_sig
                .fn_param_ref
                .get(&name)
                .cloned()
                .unwrap_or_default();
            let slice_elems = self
                .fn_sig
                .fn_param_slice_elem
                .get(&name)
                .cloned()
                .unwrap_or_default();
            let mut call_args: Vec<BasicMetadataValueEnum<'ctx>> = Vec::with_capacity(args.len());
            for (i, arg) in args.iter().enumerate() {
                let is_ref = ref_flags.get(i).copied().unwrap_or(false);
                if !is_ref {
                    // B-2026-07-28-4: by-value struct arg whose param declined
                    // the entry copy — move it, don't leave both sides owning it.
                    self.move_declined_copy_struct_arg(&arg.value);
                }
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
                    .fn_sig
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
        if let Some(ctor_fn) = self
            .conc
            .state_machine_state_constructors
            .get(&name)
            .copied()
        {
            let poll_fn = self
                .conc
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
                .conc
                .state_struct_types
                .get(&name)
                .copied()
                .expect("state struct type co-emitted with constructor");
            let ref_flags = self
                .fn_sig
                .fn_param_ref
                .get(&name)
                .cloned()
                .unwrap_or_default();
            let slice_elems = self
                .fn_sig
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
                if !is_ref {
                    // B-2026-07-28-4: by-value struct arg whose param declined
                    // the entry copy — move it, don't leave both sides owning it.
                    self.move_declined_copy_struct_arg(&arg.value);
                }
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
                .build_call(self.runtime_fns.sched_yield_fn, &[], "kara.yield_result")
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
                if let Some(ret_ty) = self.conc.state_machine_return_types.get(&name).copied() {
                    let state_struct = self
                        .conc
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
                .build_call(self.runtime_fns.free_fn, &[state_ptr.into()], "")
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
                self.fn_sig
                    .fn_asts
                    .get(&name)
                    .is_some_and(Self::return_is_layout_carrying)
            })
            .unwrap_or(LayoutId::Aos);

        // Cheap gate first: only a callee with a layout-carrying (`Vec[E]`)
        // value param OR a `Vec[E]` return can ever specialize, so skip the AST
        // clone for the common case — most user calls pay only a HashMap lookup
        // plus a param/return scan here.
        let callee_may_specialize = self.fn_sig.fn_asts.get(&name).is_some_and(|f| {
            f.params.iter().any(Self::param_is_layout_carrying)
                || Self::return_is_layout_carrying(f)
        });
        if callee_may_specialize {
            let callee_fn = self.fn_sig.fn_asts[&name].clone();
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
            .fn_sig
            .extern_link_names
            .get(&name)
            .cloned()
            .unwrap_or_else(|| name.clone());
        let func = match self.module.get_function(&lookup_name) {
            Some(f) => f,
            None => {
                // Fail CLOSED. This arm used to "silently return 0 (e.g. stdlib
                // builtins not yet codegen'd)", and that silence produced the
                // same bug three separate times: `eprintln` compiled to nothing
                // so every stderr write vanished (B-2026-08-23-14), the
                // `providers { } in { }` block compiled to nothing
                // (B-2026-07-31-9), and `dbg` compiled to a constant 0 that
                // corrupted its own return value (B-2026-08-23-16). In each
                // case no phase reported anything and the divergence was found
                // by accident, months later.
                //
                // Returning `0` is only ever right when the callee returns unit
                // AND has no side effects — which is never true of a callee
                // that reached here, because arguments are not even compiled at
                // this point, so any effect in them is dropped too.
                //
                // Measured blast radius before switching: zero callees reach
                // this arm across 12 example packages, 46 standalone examples
                // and 398 katas. It is dead for real programs; what it caught
                // was compiler gaps, silently.
                return Err(format!(
                    "codegen: no lowering for call to `{lookup_name}` at {}:{} - the \
                     callee resolved to no LLVM function. This is a compiler gap, not \
                     a program error: add a lowering arm in `compile_call`, or run it \
                     with `karac run --interp`. This arm used to return a constant 0 \
                     and compile the call away, dropping its arguments and effects.",
                    call_span.line, call_span.column
                ));
            }
        };

        let ref_flags = self
            .fn_sig
            .fn_param_ref
            .get(&name)
            .cloned()
            .unwrap_or_default();
        let mut_ref_flags = self
            .fn_sig
            .fn_param_mut_ref
            .get(&name)
            .cloned()
            .unwrap_or_default();
        let slice_elems = self
            .fn_sig
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
        let saved_pending_elem = self.var_types.pending_let_elem_type.take();
        let saved_pending_elem_te = self.var_types.pending_let_elem_type_expr.take();
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
            if !is_ref {
                // B-2026-07-28-4: by-value struct arg whose param declined the
                // entry copy — move it, don't leave both sides owning it.
                self.move_declined_copy_struct_arg(&a.value);
            }
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
                // B-2026-07-30-3 widened the Array test to cover an Array REF
                // PARAM forwarding here, not just an owned Array local — see
                // `arg_is_array_source`. The ref-param spelling crashed on this
                // non-generic path too.
                if let Some(Some(elem_ty)) = slice_elems.get(i).cloned() {
                    if self.arg_is_array_source(&a.value) {
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
                // B-2026-08-05-37: a `mut ref` parameter given a PLACE
                // argument (`bump(mut g.val)`, `bump(mut g.q.v)`,
                // `bump(mut t.0)`, `bump(p.val)` forwarding through a borrow)
                // must receive a pointer to that place. The rvalue path below
                // materialises a shallow COPY and passes a pointer to the
                // copy, so the callee's write lands on a temporary and is
                // silently discarded — measured 7 instead of 8 on all three
                // backends for every one of those spellings.
                //
                // Why this is the general form of two earlier one-shape fixes.
                // B-2026-07-12-1 and B-2026-08-05-2 taught the field and
                // tuple-element arms to borrow in place, but gated both to a
                // `{ptr,len,cap}` element "so a scalar or enum element keeps
                // the existing path" — the scoping was right for what those
                // rows had measured (a DOUBLE FREE, which only a heap element
                // can have). The lost-write half is not type-specific: it hits
                // scalars, user structs, floats and bools alike, and B-2026-08-05-2's
                // own text predicted it — "a lost `mut ref` write on a shape
                // with no bounds-checked read after it is a SILENT wrong
                // answer". So the gate here is the PARAMETER MODE, not the
                // payload type: a mutate-through borrow of a place always
                // needs the place.
                //
                // Read-only `ref` params are deliberately untouched. A copy is
                // a correct borrow for a reader, and the existing arms below
                // do type-specific work on that path (the `Option[shared T]`
                // field RC-inc, the declared-`ref` field forward) that has its
                // own regression history.
                if mut_ref_flags.get(i).copied().unwrap_or(false) {
                    if let Some(place_ptr) = self.mut_ref_place_arg_ptr(&a.value) {
                        compiled_args.push(place_ptr.into());
                        continue;
                    }
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
            //
            // B-2026-08-05-40 carve-out: a `ref Slice[T]` / `mut ref Slice[T]`
            // slot takes a POINTER, and the field/tuple arms below already pass
            // a pointer to the argument's Vec header — correct because
            // `{ptr,len,cap}` starts with `{ptr,len}`, so the callee reads the
            // right two words. `coerce_to_slice`'s place arm produces a VALUE,
            // and pushing that into a `ptr` slot is the same verification
            // failure this fix removes, one slot-shape over. A ref slot's
            // OTHER argument shapes still coerce here as before.
            if let Some(Some(elem_ty)) = slice_elems.get(i).cloned() {
                if !(is_ref && self.arg_is_vec_header_place(&a.value)) {
                    if let Some(slice_val) = self.coerce_to_slice(&a.value, elem_ty)? {
                        // B-2026-08-21-24 — a `ref Slice[T]` slot takes a
                        // POINTER to a header, and `coerce_to_slice` produces
                        // the header as a VALUE. The place arms above are
                        // excluded because they already pass a pointer; every
                        // OTHER ref-slot shape reached here and pushed the
                        // bare `{ptr, i64}` into a `ptr` parameter, which LLVM
                        // rejects at module verification:
                        //
                        //   fn total(b: ref Slice[u8]) -> i64 { b.len() }
                        //   total([1u8, 2u8, 3u8])
                        //
                        // The literal is a Vec RVALUE (a
                        // `PrefixCollectionLiteral`, not an array — the row's
                        // "array literal" framing is off), so it owns no
                        // storage the pointer fast-paths can borrow. Spilling
                        // the synthesized header into an entry alloca gives the
                        // slot the pointer it declares, and reuses the same
                        // helper the rvalue-ref path already uses rather than
                        // inventing a second materialization.
                        let to_push = if is_ref {
                            self.materialize_rvalue_for_ref_arg(slice_val, i)
                        } else {
                            slice_val
                        };
                        compiled_args.push(to_push.into());
                        continue;
                    }
                }
            }
            if is_ref {
                // B-2026-07-12-1: a struct FIELD place (`self.names`,
                // `obj.field`) passed by `ref` / `mut ref` must borrow the field
                // IN PLACE — pass a pointer to the field within the receiver
                // struct, exactly as the Identifier fast-path passes a local's
                // slot pointer. The rvalue path below would instead shallow-copy
                // the field's `{ptr,len,cap}` header into a temp and queue a
                // scope-exit FREE of that buffer (`queue_ref_rvalue_arg_cleanup`)
                // — but the field is still owned by the receiver (its own
                // field-drop frees it at the owner's scope exit), so the temp's
                // free double-frees the shared buffer. `scan(self.names, q)` with
                // `names: ref Vec[String]` aborted with glibc `free(): double
                // free detected` under AOT (interp was correct — a silent
                // run/build divergence). A LOCAL `Vec` arg took the Identifier
                // fast-path and was clean; only the field place fell through here.
                // B-2026-08-05-1 / B-2026-08-05-2 — the TUPLE-ELEMENT sibling of
                // the field arm below, which never got one. `peek(t.0)` with
                // `v: ref Vec[i64]` fell through to the rvalue path, which
                // shallow-copies the element's `{ptr,len,cap}` into a temp and
                // queues a scope-exit free of it — but the tuple still owns that
                // buffer, so the temp's free doubled it (`free(): double free
                // detected`, interp correct — the same run/build divergence
                // B-2026-07-12-1 fixed for `self.names`).
                //
                // The `mut ref` spelling is the SAME gap wearing a different
                // symptom, which is why one arm closes both: `bump(mut t.0)`
                // handed the callee a pointer to that temp copy, so `v.push(42)`
                // grew the temp and the write never reached the tuple —
                // `t.0.len()` stayed 2 and the following `t.0[2]` panicked. A
                // lost mutation is the quieter half; on a shape without a
                // bounds-checked read after it, it is a silent wrong answer.
                //
                // Gated exactly like the field arm: only a `{ptr,len,cap}`
                // element (the confirmed class), so a scalar or enum element
                // keeps the existing path. `field_chain_place_ptr` already walks
                // a tuple-index hop, and `tuple_index_elem_type_expr` is the
                // resolver B-2026-08-04-16 factored out for exactly this kind of
                // "resolve the element the same way the store does" question.
                if let ExprKind::TupleIndex { object, index } = &a.value.kind {
                    let elem_is_vec_struct = self
                        .place_chain_aggregate_llvm_type(object)
                        .and_then(|t| t.get_field_type_at_index(*index as u32))
                        .is_some_and(|t| t == self.vec_struct_type().into());
                    if elem_is_vec_struct {
                        if let Some(elem_ptr) = self.field_chain_place_ptr(&a.value) {
                            compiled_args.push(elem_ptr.into());
                            continue;
                        }
                    }
                }
                // `lower_field_access_ptr` GEPs the field off the (deref'd, for a
                // `ref self`) receiver pointer. Only the cleanly-handled Some case
                // is intercepted; a chained (`a.b.c`) or otherwise unrecognized
                // shape returns None/Err and falls through to the rvalue path
                // unchanged.
                if let ExprKind::FieldAccess { object, field } = &a.value.kind {
                    let self_norm;
                    let obj_ref: &Expr = if matches!(object.kind, ExprKind::SelfValue) {
                        self_norm = Expr {
                            kind: ExprKind::Identifier("self".to_string()),
                            span: object.span,
                        };
                        &self_norm
                    } else {
                        object
                    };
                    if let Ok(Some((field_ptr, field_ty, field_te))) =
                        self.lower_field_access_ptr(obj_ref, field, "ref-arg field borrow")
                    {
                        // Restrict the in-place borrow to the confirmed
                        // double-free class: a `Vec` / `String` field (the
                        // `{ptr,len,cap}` aggregate the rvalue path copies into a
                        // temp and then FREEs). Other field types keep the
                        // existing path — an `Option[shared T]` field arg needs
                        // the `share_option_shared_field_ref_for_arg` RC-inc below
                        // (bypassing it broke a recursive `any_negative(n.next)`
                        // borrow), a niche/enum field isn't a plain owned buffer,
                        // and a scalar field was never double-freed. This keeps
                        // the fix scoped to exactly the reported shape.
                        if field_ty == self.vec_struct_type().into() {
                            compiled_args.push(field_ptr.into());
                            continue;
                        }
                        // B-2026-07-16-5: a DECLARED `ref` / `mut ref` field
                        // (design.md Feature 4 Part 3 — the slot lowers to
                        // `ptr` and stores the BORROW pointer, not the value).
                        // Forward the stored borrow directly: it is already
                        // the exact `ptr` ABI the callee's `ref T` param
                        // expects. The rvalue fall-through instead DEREF'd the
                        // borrow into a `{ptr,len,cap}` temp and queued a
                        // cap-guarded free of it — but that cap is the
                        // LENDER's real cap, so the temp cleanup freed the
                        // lender's buffer and the lender's own scope-exit
                        // free doubled it (`shout(p.source)` with
                        // `source: ref String` aborted `free(): double free`
                        // under AOT; interp was correct — a silent run/build
                        // divergence caught by safety_design's
                        // runtime-confirmation harness).
                        if field_ty.is_pointer_type()
                            && matches!(field_te.kind, TypeKind::Ref(_) | TypeKind::MutRef(_))
                        {
                            let ptr_ty = self.context.ptr_type(AddressSpace::default());
                            let fwd = self
                                .builder
                                .build_load(ptr_ty, field_ptr, "ref_field_fwd")
                                .unwrap();
                            compiled_args.push(fwd.into());
                            continue;
                        }
                    }
                }
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
                // Thread the callee's DECLARED tensor element type into
                // `pending_let_tensor_info` around the arg compile so an inline
                // `Tensor.{from,…}` bound to a `ref Tensor[f32,…]` free-fn param
                // lays its data out at the expected element width — the free-fn
                // sibling of the assoc-call threading (B-2026-07-18-10). Without
                // it the unsuffixed f64 literals produce an 8-byte block the f32
                // callee misreads.
                let param_tensor = self
                    .fn_sig
                    .fn_param_tensor_info
                    .get(&name)
                    .and_then(|v| v.get(i).cloned().flatten());
                let saved_pending_tensor = param_tensor.as_ref().map(|info| {
                    let prev = self.accel.pending_let_tensor_info.take();
                    self.accel.pending_let_tensor_info = Some(info.clone());
                    prev
                });
                let val = self.compile_expr(&a.value)?;
                if let Some(prev) = saved_pending_tensor {
                    self.accel.pending_let_tensor_info = prev;
                }
                let cur_fn = self
                    .builder
                    .get_insert_block()
                    .and_then(|bb| bb.get_parent())
                    .expect("compile_call inside a function context");
                let temp =
                    self.create_entry_alloca(cur_fn, &format!("ref_rvalue_arg{i}"), val.get_type());
                self.builder.build_store(temp, val).unwrap();
                // A materialized fresh-owned TENSOR temp needs a `FreeTensor`
                // (its block has no other cleanup owner); everything else routes
                // through the shared Vec/String/Map owned-temp cleanup. Mirrors
                // the assoc-call path (B-2026-07-18-9).
                if param_tensor.is_some() && self.expr_yields_fresh_owned_temp(&a.value) {
                    self.track_tensor_var(temp);
                } else {
                    self.queue_ref_rvalue_arg_cleanup(temp, val, &a.value);
                }
                compiled_args.push(temp.into());
                continue;
            }
            // B-2026-08-13-21 — the tuple sibling of the tensor threading in
            // the `ref`-param branch above, needed for the same reason: a tuple
            // literal cannot recover its declared element widths from its own
            // elements. `take((b, d))` against an `(i64, i64)` param built
            // `{i8, i32}` and the module verifier rejected the CALL ("Call
            // parameter type does not match function signature").
            //
            // Staged on the BY-VALUE path specifically. The `ref`-param branch
            // above passes a pointer to caller-side storage and returns early;
            // a tuple argument is passed by value, so this is the compile it
            // actually goes through.
            //
            // The declared param type comes from the callee's AST rather than
            // its LLVM signature because `compile_tuple` lowers each slot from a
            // `TypeExpr`, and because the AST is what the READ side resolves too
            // — both ends deriving the layout from one source is the point.
            let param_te = self
                .fn_sig
                .fn_asts
                .get(&name)
                .and_then(|f| f.params.get(i))
                .map(|p| p.ty.clone());
            let saved_agg_te = self.stage_declared_aggregate_te(Some(&a.value), param_te.as_ref());
            let val = self.compile_expr(&a.value)?;
            self.restore_declared_aggregate_te(saved_agg_te);
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
            // Index companion: a direct `v[i]` element read carrying
            // `Option[shared T]` loads the inner without an inc, so the
            // callee's param `RcDecOption` would free the element the
            // container still owns (B-2026-07-11-29). Inc the loaded inner.
            if !borrow_skip {
                self.share_option_shared_index_ref_for_arg(&a.value, val);
            }
            // B-2026-07-28-17: an `Option[<heap>]` field of a shared-enum
            // payload VIEW must be copied, not aliased — the node keeps its
            // original and the callee frees its own. No-op for every other arg.
            let val = if borrow_skip {
                val
            } else {
                self.clone_shared_view_optres_field_arg(&a.value, val)
            };
            // Widen a narrow scalar to the callee's declared param width HERE,
            // where the argument expression is still in hand to say whether the
            // extension is signed or zero (B-2026-08-13-15). The boundary sweep
            // after the loop only sees LLVM values, and `u8` and `i8` are both
            // `i8` there. `compiled_args.len()` is the slot this value is about
            // to take, so the two never drift.
            let val = self.coerce_call_arg_scalar(func, compiled_args.len(), val, &a.value);
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
            let flows_into_return = self.call_arg_flows_into_return(&name, i);
            // B-2026-08-26-9 — the SECOND escape route. `flows_into_return`
            // stays narrow because the admission test just below is about the
            // entry-copy passthrough specifically; what the REGISTRAR needs is
            // the union, since its bodies-vs-memory split turns on "does this
            // value outlive the call" and not on which exit it took.
            let escapes_frame =
                flows_into_return || self.call_arg_moves_into_outliving_place(&name, i, false);
            // B-2026-08-05-7 — the TENSOR sibling of the `#20` arm above. A
            // tensor is a bare `ptr` to one `[rank][dims][data]` block, so
            // `llvm_ty_is_vec_struct` never admits it and the fresh-temp
            // materialization above skips it entirely. Owned tensor params
            // follow the same caller-retains convention Vec/String do — the
            // callee registers no `FreeTensor` for its by-value param, which
            // `let m = make(); first(m);` proves by being single-free — so a
            // temp passed DIRECTLY (`first(make())`) had no owner anywhere and
            // leaked the whole block, 56 B per call.
            //
            // Same passthrough guard as the aggregate registrar below: when the
            // callee hands the param back (`fn id(t: Tensor…) -> Tensor… { t }`)
            // the caller's RESULT binding owns the block and a free here would
            // double it. A tensor param is never entry-copied, so unlike the
            // struct/enum arms there is no copy-supported exception to carve out.
            if !flows_into_return
                && val.is_pointer_value()
                && self.expr_yields_fresh_owned_temp(&a.value)
                && self.is_owned_tensor_param(&name, i)
            {
                let cur_fn = self
                    .builder
                    .get_insert_block()
                    .and_then(|bb| bb.get_parent())
                    .expect("compile_call inside a function context");
                let slot =
                    self.create_entry_alloca(cur_fn, &format!("tensor_arg_tmp{i}"), val.get_type());
                self.builder.build_store(slot, val).unwrap();
                self.track_tensor_var(slot);
            }
            // B-2026-08-05-7 — the same shape once more, now for an `Option[T]`
            // whose payload `T` was HEAP-BOXED because its LLVM width exceeds
            // Option's seeded 3-word area (`coerce_to_payload_words`). A NAMED
            // binding gets its box drop at the let site (`track_boxed_enum_var`)
            // and a fresh-temp SCRUTINEE gets one from
            // `materialize_freshtemp_enum_scrutinee`; a fresh temp handed
            // straight to an owned param — `classify(Some(Some(42)))` — had
            // neither, so the box leaked once per call. Params register no drop
            // of their own (see `track_enum_var`'s note), so the caller is the
            // only frame that can own it.
            //
            // BOX-ONLY (`inner_struct_name = None`), the same choice the
            // fresh-temp scrutinee path documents: if the callee's arm binds the
            // payload out, that binding owns `T`'s interior and dropping `T`
            // here would double-free it.
            if !flows_into_return
                && val.is_struct_value()
                && self.expr_yields_fresh_owned_temp(&a.value)
                && self.owned_boxed_option_param_struct(&name, i).is_some()
            {
                let inner_struct = self.owned_boxed_option_param_struct(&name, i);
                let cur_fn = self
                    .builder
                    .get_insert_block()
                    .and_then(|bb| bb.get_parent())
                    .expect("compile_call inside a function context");
                let slot =
                    self.create_entry_alloca(cur_fn, &format!("optbox_arg_tmp{i}"), val.get_type());
                self.builder.build_store(slot, val).unwrap();
                self.track_boxed_enum_var(
                    &format!("__optbox_arg_tmp{i}"),
                    slot,
                    "Option",
                    "Some",
                    inner_struct.as_deref(),
                );
            }
            if !flows_into_return
                || self.arg_is_entry_copied_heap_struct(&a.value)
                // B-2026-08-01-14 — enum ctor args are entry-copied too:
                // a passthrough callee returns the COPY, so the original
                // must still be freed caller-side (memory only, inside
                // the registrar's enum arm).
                || self.arg_is_entry_copied_heap_enum(&a.value)
                // B-2026-08-27-44 — and so is a whole TUPLE
                // (`make_tuple_param_callee_owned`). Without this third
                // sibling `passthru((Bag { .. }, 7))` never reached the
                // registrar AT ALL on the escape path — not a gate that
                // declined inside it, an admission test that excluded it —
                // so the caller's orphaned original leaked 48 bytes.
                || self.arg_is_entry_copied_heap_tuple(&a.value, &name, i)
            {
                let escaping_parts = self.callee_returned_param_parts(&name, i);
                self.track_inline_owned_aggregate_arg_parts(
                    val,
                    &a.value,
                    escapes_frame,
                    &escaping_parts,
                );
            }
            // B-2026-07-10-4 residual — an inline-heap `Option[String]`/
            // `Option[Vec]` binding MOVED by value into a user function that
            // OWNS + frees it (`consume(sv)` where `sv: Option[String]` is a
            // tracked `inline_option_payload_vars` local). The callee's param
            // consumption frees the `Some` payload, but the caller's scope-exit
            // `FreeInlineOptionPayload` also fires → double-free (surfaced by the
            // self-hosted `render_attr(a)` attribute path over an `Option[String]`
            // whose payload was matched out of a token). Zero the source slot so
            // the caller's guard skips — the same whole-move suppression the
            // enum-variant / struct-literal field-init paths already apply, now
            // for a by-value call argument. Gated OUT of a borrowed position
            // (`borrow_skip` — the callee doesn't consume) and a return-passthrough
            // (`fn id(o) -> Option { o }` — the callee hands `o` back and the
            // caller's RESULT binding owns it, so the source must stay live).
            // B-2026-08-02-23 leg 2 — the DROP-BODIES half of the same
            // passthrough rule. The caller-drops-the-owned-arg convention makes
            // the caller's binding fire its body at the call (correct when the
            // value dies inside the callee), but when the callee hands that
            // very value BACK the caller's RESULT binding becomes the owner and
            // the arg-site fire is a duplicate: `fn passthru(v: Vec[Res]) ->
            // Vec[Res] { v }` over `let ys = passthru(xs);` printed the element
            // body twice on both backends. Only the memory/Option channels
            // consulted `flows_into_return`; the `UserDrop` channel never did.
            //
            // Borrowed positions are excluded (no ownership transfer). The
            // predicate is conservative-true on a mixed-path callee, so a
            // non-passthrough path loses the body side effect — the same
            // leak-of-side-effect, never-a-double-drop trade `fn_returns_param`
            // already documents for the memory channel.
            //
            // CONTAINER-ELEMENT walkers only, never the binding's own
            // `karac_drop_<T>` wrapper. That wrapper is body + fields + MEMORY
            // (see `emit_drop_fn_for_type_expr`'s note on the shared name), and
            // an own-`Drop` struct param is ENTRY-COPIED by the callee — so the
            // caller still owns the original and retracting its wrapper orphans
            // that copy's buffer (measured: `fn mk(r: Res) -> Bx { Bx { r: r } }`
            // went vg-clean → 3-byte definite leak under the strong form).
            // `arg_is_entry_copied_heap_struct` can't gate this: it matches only
            // literal and call args, and the shape here is a bare identifier.
            // Two entry-copied values genuinely exist, so the own-wrapper case
            // keeping two bodies is consistent with its two frees.
            //
            // B-2026-08-09-15 widens the gate one level down the same rule: the
            // callee may hand back not the param but a PAYLOAD bound out of it
            // (`match b { Box2.Full(r) => return r }`). `fn_returns_param` cannot
            // see that — `b` reaches no return site — so the caller kept firing
            // its walk while the result's binding fired too.
            if !borrow_skip && (flows_into_return || self.callee_returns_enum_arg_payload(&name, i))
            {
                if let ExprKind::Identifier(var_name) = &a.value.kind {
                    let var_name = var_name.clone();
                    self.suppress_container_elem_bodies_for_var(&var_name);
                }
            }
            if !borrow_skip && !self.call_arg_flows_into_return(&name, i) {
                // B-2026-08-06-31 — a binding whose box carries a user STRUCT
                // interior keeps its cleanup across a by-value call. The
                // whole-slot zero below is a MOVE, and it only balances when
                // some other frame takes the box over; for this payload class
                // nobody does. B-2026-08-06-9 leg A gave the callee the box for
                // NON-struct payloads precisely because admitting struct ones
                // double-frees the three fixtures named on that row, so here
                // the caller is the only owner there is and zeroing the slot
                // stranded 32 B of box plus the whole interior per call.
                //
                // A callee arm that moves the payload (or a field of it) out is
                // still safe, and NOT because of anything here: it neutralizes
                // through the box's own words (B-2026-08-06-10's mirror, which
                // is gated on exactly this owned-param shape). That mirror
                // needed a THIRD consumer for the let-destructure shape; see
                // `finish_owned_struct_destructure` and
                // `suppress_destructured_struct_pattern_cleanup_at`.
                let boxed_struct_binding = matches!(
                    &a.value.kind,
                    ExprKind::Identifier(n) if self.payload_vars.boxed_struct_payload_vars.contains(n.as_str())
                );
                // B-2026-08-12-1 — an ENTRY-COPIED param owns its own buffer,
                // so the caller keeps the original and this whole-slot zero
                // would be a move with nothing on the other side of it.
                let callee_entry_copies = self.callee_optres_param_entry_copied(&name, i);
                if !boxed_struct_binding && callee_entry_copies.is_none() {
                    self.suppress_inline_option_result_binding_move(&a.value);
                }
                // B-2026-08-12-1 — a FRESH TEMP handed to an entry-copying param
                // has no binding to own it. The callee frees its own copy, so
                // without this the caller's original leaks once per call — which
                // is precisely the B-2026-08-11-30 leak, reappearing through the
                // other door the moment the callee stopped taking the buffer.
                //
                // A named binding needs nothing: its let-site registration
                // already owns the value and, now that the whole-slot zero above
                // is suppressed for this shape, still fires at scope exit.
                // Identifier args are excluded for exactly that reason — owning
                // one here would be the double free the zero used to prevent.
                if let Some(param_te) = callee_entry_copies.clone() {
                    // Two independent ownership questions about one temp — the
                    // `{ptr,len,cap}` payload buffer and the boxed field
                    // envelope — with different freshness rules and so
                    // different answers. B-2026-08-12-15.
                    let own_payload = self.optres_arg_is_unowned_temp(&a.value);
                    let own_envelope = self.optres_arg_mints_field_envelope(&a.value);
                    if own_payload || own_envelope {
                        self.track_optres_arg_temp(val, &param_te, own_payload, own_envelope);
                    }
                }
                // B-2026-08-07-2 shapes 1+2 — the NESTED-box sibling of the
                // suppressor above. The callee's owned non-escaping param now
                // registers a `NestedBoxedEnumDrop` of its own (functions.rs),
                // so a binding argument that keeps its let-site registration
                // would make two owners of one pointer — the double free this
                // family's CAUTION is about, not a leak.
                //
                // Retraction rather than the slot-zeroing the direct sibling
                // uses: this action's guard is a TWO-tag walk, and `Result`'s
                // `Ok` is tag 0, so zeroing the slot leaves the outer guard
                // PASSING and relies on the inner tag and null check to save
                // it. Removing the action says what is meant and does not
                // depend on which variant happens to be tag 0.
                //
                // Gated by the same `!flows_into_return` as its siblings, which
                // is what keeps the escape shape (`fn id(r) -> r`) with exactly
                // one owner: the callee registers nothing when the param can
                // escape, so the caller must keep its own.
                // Every spelling that still aliases a live owner has to disarm
                // it, not just a bare identifier: `cls(id(b))`, `cls(id(id(b)))`
                // and `let c = id(b); cls(c)` each hand the callee a temp that
                // is `b`'s box, and each was measured as a glibc `double free
                // detected in tcache 2` at -O0 while the resolver was too
                // shallow to reach `b`.
                //
                // B-2026-08-12-15 excepts the struct-FIELD subset, whose callee
                // registers nothing (see `struct_field_boxed_payload_vars` and
                // the `functions.rs` loop it points at). Retracting there is the
                // move-with-nothing-on-the-other-side this whole block's sibling
                // gates already guard against: measured as the row's 32 B per
                // call, unchanged by the retraction being present or absent
                // while the caller had no registration to retract — which is why
                // gating it was a byte-identical no-op on the first attempt and
                // is load-bearing only now that the let site arms one.
                if let Some(src) = self.nested_boxed_owner_source_of(&a.value) {
                    if !self
                        .payload_vars
                        .struct_field_boxed_payload_vars
                        .contains(src.as_str())
                    {
                        self.suppress_nested_boxed_drop_for_var(&src);
                    }
                }
                // B-2026-07-28-16 — the same move, but from a FIELD rather than
                // a binding: `consume(nd.hp)` where `nd` is an owned struct with
                // an `Option`/`Result` field. The suppressor above is
                // Identifier-only, so a `FieldAccess` argument fell straight
                // through: the callee's param consumption freed the payload and
                // the owning struct's scope-exit drop freed it again.
                //
                // B-2026-07-21-16 built exactly this source-zeroing for the
                // pattern / `let` / assign move sites and this one was missed,
                // which is why `match nd.hp { … }` was already correct while
                // `f(nd.hp)` was not. Same helper, same gates — it bails on
                // borrowed roots and on payload shapes whose drop it cannot
                // neutralize.
                // B-2026-08-12-1 — same carve-out, the FIELD spelling. An
                // entry-copied param leaves the owning struct's field payload
                // with the caller, so zeroing the field here orphans it: 1,280 B
                // over 40 iterations in the fixture that caught it. Gated on the
                // one shared predicate, like its binding sibling above.
                if callee_entry_copies.is_none() {
                    self.suppress_place_optres_field_whole_move_source(&a.value);
                }
                // B-2026-08-06-9 leg A — the argument is a PASSTHROUGH RESULT
                // binding, which owns nothing: its source does
                // (B-2026-08-06-21 skips the result's registration). The
                // suppressor above zeroes the named slot and so found nothing
                // to disarm, while the callee — which now owns a boxed
                // non-struct `Option` payload — freed the box, and the source's
                // own cleanup freed it again.
                //
                // Retract the SOURCE's box with the word-scoped suppressor
                // rather than by forwarding the whole-slot zero above: the
                // source may carry an inline payload on the other side of the
                // same slot (`Result[Wide, String]`), and zeroing all of it
                // strands that. Only the box word moves owner here.
                if let ExprKind::Identifier(n) = &a.value.kind {
                    if let Some(owner) = self
                        .payload_vars
                        .boxed_passthrough_owner_alias
                        .get(n.as_str())
                        .cloned()
                    {
                        self.suppress_boxed_enum_payload_cleanup_for_owner(&owner);
                    }
                }
            }
        }
        // Restore the pending-let hint cleared above for the arg loop.
        self.var_types.pending_let_elem_type = saved_pending_elem;
        self.var_types.pending_let_elem_type_expr = saved_pending_elem_te;

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
        if self.fn_sig.track_caller_fns.contains(&lookup_name) {
            match self.fn_ctx.current_fn_caller_loc {
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
        let call = if let Some(slot) = self.conc.hot_swap_slots.get(&name).copied() {
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
            let v = self.unpack_niche_abi_ret(&name, basic_val.unwrap_basic());
            // LazyFrame codegen twin — rule 3 of the ownership model
            // (`src/codegen/lazyframe.rs`): a user fn DECLARED to return
            // LazyExpr/LazyFrame hands back an escaping +1 (retained in the
            // callee's retain-on-return hook); register the matching release
            // in THIS scope. A no-op for every other callee.
            self.register_lazy_user_call_result(&name, v);
            Ok(v)
        }
    }

    /// Is parameter `i` of free function `name` an OWNED (non-borrow)
    /// `Tensor[T, S]` param? `fn_param_tensor_info` peels one `ref`/`mut ref`
    /// (it exists to thread the element type into an inline `Tensor.from`
    /// argument, which a borrowed param needs just as much), so borrow-ness has
    /// to come from `fn_param_ref` / `fn_param_mut_ref` separately — the
    /// tensor-info entry alone would admit `fn f(t: ref Tensor…)`, whose block
    /// the caller's binding still owns.
    pub(super) fn is_owned_tensor_param(&self, name: &str, i: usize) -> bool {
        let is_tensor = self
            .fn_sig
            .fn_param_tensor_info
            .get(name)
            .and_then(|v| v.get(i))
            .is_some_and(|info| info.is_some());
        let flagged = |table: &HashMap<String, Vec<bool>>| {
            table
                .get(name)
                .and_then(|v| v.get(i))
                .copied()
                .unwrap_or(false)
        };
        is_tensor && !flagged(&self.fn_sig.fn_param_ref) && !flagged(&self.fn_sig.fn_param_mut_ref)
    }

    /// B-2026-08-09-15 — does the callee RETURN something it bound out of
    /// parameter `i`, so the caller must stop running that arg's payload bodies?
    ///
    /// The payload sibling of [`Self::call_arg_flows_into_return`], and it exists
    /// for the same reason: the caller-retains convention fires an owned arg's
    /// bodies at the moved-from binding's live-range end, which is right when the
    /// value dies in the callee and a duplicate when it comes back out.
    /// `fn_returns_param` answers only for the param ITSELF; here the thing that
    /// escapes is a payload a `match` arm bound out of it, so `b` never appears at
    /// a return site and that predicate is blind to it.
    ///
    /// `name` is a free function's name or a qualified `Type.method` — the same
    /// key `fn_param_ref` uses, so the METHOD path asks with its own `i + 1` (the
    /// receiver holds slot 0). Both are looked up because the method spelling has
    /// the identical defect: measured, `t.take(b)` printed the callee's copy AND
    /// the caller's original (`drop 99 MUT`, `drop 7 e7`) where `--interp` printed
    /// one.
    ///
    /// Borrowed params are excluded outright: nothing transfers, so whatever the
    /// callee does with a borrow leaves the caller's walk its own.
    pub(super) fn callee_returns_enum_arg_payload(&self, name: &str, i: usize) -> bool {
        let flagged = |table: &HashMap<String, Vec<bool>>| {
            table
                .get(name)
                .and_then(|v| v.get(i))
                .copied()
                .unwrap_or(false)
        };
        if flagged(&self.fn_sig.fn_param_ref) || flagged(&self.fn_sig.fn_param_mut_ref) {
            return false;
        }
        let Some(program) = self.program_snapshot.as_deref() else {
            return false;
        };
        // Free fns by bare name; impl methods by the `Type.method` key the
        // method path passes.
        let (recv_ty, bare) = match name.split_once('.') {
            Some((t, m)) => (Some(t), m),
            None => (None, name),
        };
        let matching_fns = program.items.iter().flat_map(|item| match item {
            crate::ast::Item::Function(f) if recv_ty.is_none() && f.name == name => vec![f],
            crate::ast::Item::ImplBlock(b)
                if recv_ty.is_some_and(|t| {
                    matches!(&b.target_type.kind,
                        TypeKind::Path(p) if p.segments.first().is_some_and(|h| h == t))
                }) =>
            {
                b.items
                    .iter()
                    .filter_map(|ii| match ii {
                        crate::ast::ImplItem::Method(f) if f.name == bare => Some(f.as_ref()),
                        _ => None,
                    })
                    .collect()
            }
            _ => Vec::new(),
        });
        matching_fns.into_iter().any(|f| {
            // `fn_param_ref` counts the receiver as slot 0; the AST does NOT —
            // `Function::params` excludes it and `self_param` carries it. So the
            // caller's declared index has to shift back by one for a method, or
            // `params.get` reads the wrong param (and for a one-arg method reads
            // past the end, which is how this silently answered `false` for every
            // method until it was instrumented).
            let ast_i = if f.self_param.is_some() {
                match i.checked_sub(1) {
                    Some(n) => n,
                    None => return false,
                }
            } else {
                i
            };
            let Some(param) = f.params.get(ast_i) else {
                return false;
            };
            // Bare `Path` naming a non-shared user enum — the types whose
            // payload bodies ride the `__karac_dropelems_enum_<E>` channel this
            // retraction acts on. `Option`/`Result` have their own.
            let TypeKind::Path(path) = &param.ty.kind else {
                return false;
            };
            let is_value_enum = path.segments.first().is_some_and(|en| {
                en != "Option"
                    && en != "Result"
                    && self
                        .type_decls
                        .enum_layouts
                        .get(en.as_str())
                        .is_some_and(|l| !l.is_shared)
            });
            is_value_enum && crate::ast::fn_returns_param_payload(f, ast_i)
        })
    }

    /// Does parameter `i` of free function `name` take a by-value `Option[T]`
    /// whose payload `T` is a user STRUCT that is HEAP-BOXED — i.e. `T`'s LLVM
    /// word count exceeds `Option`'s seeded payload area, the exact predicate
    /// `coerce_to_payload_words` boxes on and `reconstruct_payload_value`
    /// deboxes on? Returns the struct's NAME, so the caller-side registration
    /// can give the box drop that struct's `__karac_drop_struct_<T>` interior
    /// walk instead of freeing the envelope alone (B-2026-08-06-31).
    ///
    /// `Option` only, deliberately. `Result` boxes PER VARIANT (`Ok` and `Err`
    /// each measured against the 5-word area), so a caller-side free would have
    /// to know which tag is live before it can name the variant that carries a
    /// box; `Option` has one such tag.
    ///
    /// STRUCT payloads only, as of B-2026-08-06-9 leg A: the CALLEE now owns a
    /// boxed non-struct `Option` payload, so keeping this arm for those would
    /// free the same box twice. Which frame owns a boxed STRUCT payload is
    /// decided by the ARGUMENT FORM, not by the callee — a `FieldAccess`
    /// (`f(nd.hp)`) leaves the owning struct's field drop in charge, a named
    /// binding keeps its let-site drop (see the arg-site skip that consults
    /// `boxed_struct_payload_vars`), and a FRESH TEMP has neither, which is what
    /// this arm exists for.
    pub(super) fn owned_boxed_option_param_struct(&self, name: &str, i: usize) -> Option<String> {
        let flagged = |table: &HashMap<String, Vec<bool>>| {
            table
                .get(name)
                .and_then(|v| v.get(i))
                .copied()
                .unwrap_or(false)
        };
        if flagged(&self.fn_sig.fn_param_ref) || flagged(&self.fn_sig.fn_param_mut_ref) {
            return None;
        }
        let program = self.program_snapshot.as_deref()?;
        let param_te = program.items.iter().find_map(|item| match item {
            Item::Function(f) if f.name == name => f.params.get(i).map(|p| p.ty.clone()),
            _ => None,
        })?;
        let TypeKind::Path(p) = &param_te.kind else {
            return None;
        };
        if p.segments.first().map(|s| s.as_str()) != Some("Option") {
            return None;
        }
        let Some(GenericArg::Type(payload_te)) = p.generic_args.as_ref().and_then(|a| a.first())
        else {
            return None;
        };
        let TypeKind::Path(pp) = &payload_te.kind else {
            return None;
        };
        let struct_name = pp
            .segments
            .last()
            .filter(|s| self.type_decls.struct_types.contains_key(s.as_str()))?;
        self.option_payload_is_boxed(payload_te)
            .then(|| struct_name.clone())
    }

    /// Would an `Option[T]` payload of type `T` be HEAP-BOXED — i.e. does `T`'s
    /// LLVM word count exceed `Option`'s seeded payload area? The exact
    /// predicate `coerce_to_payload_words` boxes on and
    /// `reconstruct_payload_value` deboxes on, in the one place callers can ask
    /// it of a payload type-expr alone.
    pub(super) fn option_payload_is_boxed(&self, payload_te: &TypeExpr) -> bool {
        self.enum_payload_is_boxed("Option", payload_te)
    }

    /// The same question for a `Result[T, E]` side. B-2026-08-06-26.
    ///
    /// `Result` seeds a different payload area than `Option`, so the predicate
    /// has to read ITS layout — asking `option_payload_is_boxed` about a
    /// `Result` payload would compare against the wrong width.
    pub(super) fn result_payload_is_boxed(&self, payload_te: &TypeExpr) -> bool {
        self.enum_payload_is_boxed("Result", payload_te)
    }

    /// Shared body of the two predicates above: does `payload_te` exceed
    /// `enum_name`'s seeded payload area, so `coerce_to_payload_words` spills it
    /// behind a pointer?
    fn enum_payload_is_boxed(&self, enum_name: &str, payload_te: &TypeExpr) -> bool {
        let Some(layout) = self.type_decls.enum_layouts.get(enum_name) else {
            return false;
        };
        let area = (layout.llvm_type.count_fields() as usize).saturating_sub(1);
        Self::llvm_type_word_count(self.llvm_type_for_type_expr(payload_te)) > area
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
        // The self-exclusion below is a passthrough-chain guard for a
        // FUNCTION call `g(make())`: g may forward make()'s box, so the box is
        // dec'd once at the outer link, not here. A VARIANT CONSTRUCTOR is not
        // a passthrough — `Node(inner)` MOVES `inner` into its own payload and
        // its recursive `__karac_rc_drop` frees the payload, so the outer temp
        // always needs its own caller-side dec regardless of whether `inner` is
        // itself a fresh shared temp. Applying the guard to a constructor made
        // the drop TOGGLE with nesting depth: `fresh_arg_..` flipped Some/None
        // each level, so odd-depth `Node(Node(…))` args registered no drop and
        // leaked the whole RC chain while even depths were clean
        // (B-2026-07-12-25). Skip the guard for a variant constructor.
        let is_variant_ctor = self.enum_name_of_expr(expr).is_some();
        if !is_variant_ctor
            && args
                .iter()
                .any(|a| self.fresh_arg_bare_shared_heap_type(&a.value).is_some())
        {
            return None;
        }
        let type_name = match &callee.kind {
            ExprKind::Identifier(n) => self
                .fn_sig
                .fn_return_type_names
                .get(n)
                .cloned()
                .or_else(|| self.enum_name_of_expr(expr)),
            ExprKind::Path { .. } => self.enum_name_of_expr(expr),
            _ => None,
        }?;
        self.type_decls
            .shared_types
            .get(&type_name)
            .map(|i| i.heap_type)
    }

    /// B-2026-07-01-7 passthrough guard — whether the free-fn callee
    /// `callee_name`'s body can RETURN its parameter `arg_index`
    /// (`crate::ast::fn_returns_param` — any bare-param return site counts,
    /// conservative toward skipping). Resolved from the program snapshot's
    /// top-level functions; unknown callees (externs, builtins) → `false`
    /// (register — the status-quo caller-drops convention).
    /// B-2026-08-12-1 — does parameter `arg_index` of `callee_name` take a
    /// by-value `Option`/`Result` the CALLEE entry-COPIES? When it does, the
    /// callee owns an independent buffer and the caller must KEEP its original,
    /// so the arg-site whole-slot zero must not fire. Methods are resolved with
    /// the same receiver-index shift `callee_returns_enum_arg_payload` documents.
    /// B-2026-08-12-1 — is this argument an `Option`/`Result` temp that NO other
    /// frame owns, and which therefore needs the caller-side cleanup
    /// [`Self::track_optres_arg_temp`] registers?
    ///
    /// Deliberately narrow: a direct call to a real FUNCTION (`take(mk())`),
    /// which is the shape whose payload is manufactured by the callee and
    /// referenced by nothing else. Everything else is excluded, and the
    /// exclusions are the whole point of the predicate:
    ///
    ///   * an IDENTIFIER (`take(r)`) — its let-site registration owns it, and
    ///     since the arg-site zero is now suppressed for this shape, that
    ///     registration still fires.
    ///   * an enum-CONSTRUCTOR call (`take(Some(x))`, `take(Ok(x))`) — the
    ///     temp is fresh but its PAYLOAD need not be: wrapping a live local
    ///     hands us a buffer that binding still owns, so registering here makes
    ///     two owners of it. This is not hypothetical — admitting ctor args
    ///     aborted the self-host parser with `double free detected in tcache 2`
    ///     while the whole -O0 valgrind matrix and 1044 memory_sanitizer
    ///     fixtures stayed green. The oracle is what caught it.
    ///
    /// Excluding the ctor shape leaves `take(Some(mkv()))` — a fresh payload
    /// inside a fresh ctor — leaking as it did before, which is the safe
    /// direction and is recorded as the row's remaining scope rather than
    /// papered over.
    /// B-2026-08-12-15 — ENVELOPE sibling of
    /// [`Self::optres_arg_is_unowned_temp`]: does this argument expression MINT
    /// the boxed field envelope it carries, rather than copy a pointer to one
    /// some other frame still owns?
    ///
    /// A separate predicate rather than a widening of that one, because the two
    /// ask about different allocations and the answers genuinely differ. That
    /// one guards a `{ptr,len,cap}` PAYLOAD buffer, which a ctor can wrap
    /// without minting (`Ok(x)` hands us `x`'s buffer) — hence its narrowness,
    /// paid for by a self-host double free. This one guards a
    /// `coerce_to_payload_words` ENVELOPE, which is minted by the construction
    /// itself and is never named by the source program, so the only way an
    /// argument can carry a LIVE one is by reading it out of a place that
    /// already owns it.
    ///
    /// So the question is asked PER FIELD rather than per expression kind, and
    /// only of the fields that actually box. Whatever is inside the boxing
    /// field's own payload is irrelevant to who owns the ENVELOPE — that
    /// allocation is minted by the `Option.Some(…)` construction itself,
    /// however its payload was computed. Which is why an expression-kind
    /// safe-list is the wrong shape here: the measured argument is
    /// `cls(Result.Ok(W { o: Option.Some(Option.Some(n + i)) }))`, whose `n`
    /// and `i` are IDENTIFIERS — refused by any place-based rule, and unable to
    /// carry a box at all.
    ///
    /// DEFAULT-FALSE on everything else, `if`/`match`/block expressions
    /// included: those can evaluate to a live binding's value, and the cost of
    /// a false negative is the status-quo leak while a false positive is a
    /// double free.
    /// B-2026-08-12-17 — the NESTED-operand rule for
    /// [`Self::optres_arg_mints_field_envelope`]: an expression sitting INSIDE
    /// a construction, rather than the whole argument.
    ///
    /// A bare identifier is a different question at the two depths, which is
    /// why this is a separate entry point rather than an arm of the main match.
    /// At top level `take(r)` names the enum value itself and its let site owns
    /// the envelope, so admitting it would make two owners. One level in,
    /// `take(Result.Ok(w))` names the STRUCT being moved into a fresh envelope
    /// — the ctor mints the envelope and the move disarms `w`, so the temp is
    /// the only owner left. The `let r = Result.Ok(w);` spelling of that move
    /// is CLEAN, which is what localizes the leak to argument position rather
    /// than to the move.
    ///
    /// Still refuses an identifier that is ITSELF an armed envelope owner —
    /// `Option.Some(o)` for a bound `o: Option[Option[i64]]` hands over a box
    /// `o`'s let site still frees. The three sets are the three registration
    /// families that can own one.
    fn envelope_operand_is_unowned(&self, e: &Expr) -> bool {
        if let ExprKind::Identifier(n) = &e.kind {
            return !self
                .payload_vars
                .boxed_enum_payload_vars
                .contains(n.as_str())
                && !self
                    .payload_vars
                    .nested_boxed_payload_vars
                    .contains(n.as_str())
                && !self
                    .payload_vars
                    .struct_field_boxed_payload_vars
                    .contains(n.as_str());
        }
        self.optres_arg_mints_field_envelope(e)
    }

    pub(super) fn optres_arg_mints_field_envelope(&self, arg: &Expr) -> bool {
        match &arg.kind {
            ExprKind::Call { args, .. } => {
                if self.enum_name_of_expr(arg).is_some() {
                    return args
                        .iter()
                        .all(|a| self.envelope_operand_is_unowned(&a.value));
                }
                // B-2026-08-12-17 — a plain call to a known function
                // (`take(mkw(n))`). The callee MANUFACTURED the envelope and
                // then returned it, which retracts its own registration as an
                // escape, so the value arrives owned by nobody: 32 B per call.
                //
                // Admitted only when no live binding owns it, and
                // `nested_boxed_owner_source_of` is exactly that question —
                // it walks passthrough chains and the alias map to a fixpoint.
                // `take(idw(b))` for a bound `b` resolves to `b` and is refused
                // (registering there is a double free, and it is measured clean
                // today because `b`'s let site is the owner); `take(idw(mkw(n)))`
                // resolves to nothing and is admitted, because the passthrough
                // is forwarding a temp rather than a binding.
                matches!(&arg.kind, ExprKind::Call { callee, .. }
                    if matches!(&callee.kind, ExprKind::Identifier(n)
                        if self.fn_sig.fn_return_type_names.contains_key(n)))
                    && self.nested_boxed_owner_source_of(arg).is_none()
            }
            ExprKind::StructLiteral {
                path,
                fields,
                spread,
                ..
            } => {
                // A spread copies fields wholesale out of another value, box
                // pointers included, and nothing here can tell which.
                if spread.is_some() {
                    return false;
                }
                let Some(sname) = path.last() else {
                    return false;
                };
                let Some(field_names) = self.type_decls.struct_field_names.get(sname.as_str())
                else {
                    return false;
                };
                let Some(field_tes) = self.type_decls.struct_field_type_exprs.get(sname.as_str())
                else {
                    return false;
                };
                fields.iter().all(|f| {
                    let Some(fte) = field_names
                        .iter()
                        .position(|n| *n == f.name)
                        .and_then(|i| field_tes.get(i))
                    else {
                        return false;
                    };
                    // A field that boxes nothing has no envelope to own, so its
                    // initializer is unconstrained.
                    if self.boxed_enum_payload_variants(fte).is_empty() {
                        return true;
                    }
                    self.enum_name_of_expr(&f.value).is_some()
                        && matches!(&f.value.kind, ExprKind::Call { .. })
                })
            }
            _ => false,
        }
    }

    pub(super) fn optres_arg_is_unowned_temp(&self, arg: &Expr) -> bool {
        match &arg.kind {
            // A live binding or a place rooted at one: something else owns it.
            ExprKind::Identifier(_)
            | ExprKind::FieldAccess { .. }
            | ExprKind::Index { .. }
            | ExprKind::MethodCall { .. } => false,
            ExprKind::Call { callee, args, .. } => {
                // An enum CONSTRUCTOR is only as fresh as what it wraps:
                // `Ok(mkv())` manufactures its payload, `Ok(x)` wraps a buffer
                // `x` still owns. Recurse rather than accept or reject the whole
                // ctor shape.
                if self.enum_name_of_expr(arg).is_some() {
                    return args
                        .iter()
                        .all(|a| self.optres_arg_is_unowned_temp(&a.value));
                }
                // A direct call to a real function manufactures its result.
                matches!(&callee.kind, ExprKind::Identifier(n)
                    if self.fn_sig.fn_return_type_names.contains_key(n))
            }
            // A fresh aggregate is unowned exactly when every initializer is.
            ExprKind::StructLiteral { fields, .. } => fields
                .iter()
                .all(|f| self.optres_arg_is_unowned_temp(&f.value)),
            ExprKind::Integer(..)
            | ExprKind::Float(..)
            | ExprKind::CharLit(..)
            | ExprKind::ByteLit(..)
            | ExprKind::ByteStringLit(..)
            | ExprKind::StringLit(..)
            | ExprKind::MultiStringLit(..)
            | ExprKind::Bool(..) => true,
            _ => false,
        }
    }

    /// B-2026-08-12-1 — own a FRESH-TEMP `Option`/`Result` argument handed to a
    /// param the callee ENTRY-COPIES. The temp has no binding, and the callee
    /// now frees only its own copy, so this frame is the only one that can free
    /// the original.
    ///
    /// Spills the value to an entry alloca and registers the same inline-payload
    /// cleanups a `let` of the same value would get, so the free is the one the
    /// drop machinery already knows how to emit — including its `cap > 0` /
    /// tag guards, which is what makes an `Err`/`None` temp a no-op rather than
    /// a wild free.
    ///
    /// The tracker key is a fixed name, matching `__owned_agg_tmp`'s precedent
    /// one arm over. It is inert either way: the name is never inserted into
    /// `variables`, so the name-keyed suppressors resolve nothing through it and
    /// cannot retarget one temp's cleanup at another's slot.
    ///
    /// `own_payload` / `own_envelope` select the two independent halves, each
    /// gated by its own freshness predicate at the call site — see
    /// [`Self::optres_arg_mints_field_envelope`] for why one answer cannot
    /// serve both. They share this one spill so a temp needing both does not
    /// get two slots holding the same value.
    pub(super) fn track_optres_arg_temp(
        &mut self,
        val: BasicValueEnum<'ctx>,
        param_te: &TypeExpr,
        own_payload: bool,
        own_envelope: bool,
    ) {
        let inkwell::types::BasicTypeEnum::StructType(agg_ty) = val.get_type() else {
            return;
        };
        let Some(cur_fn) = self.current_fn else {
            return;
        };
        let slot = self.create_entry_alloca(cur_fn, "__optres_arg_tmp", agg_ty.into());
        if self.builder.build_store(slot, val).is_err() {
            return;
        }
        if own_payload {
            self.track_inline_option_payload_var("__optres_arg_tmp", slot, param_te);
            self.track_inline_result_payload_var("__optres_arg_tmp", slot, param_te);
        }
        if !own_envelope {
            return;
        }
        // B-2026-08-12-15 — the same argument one level in: a box inside a
        // FIELD of the temp's inline STRUCT payload (`cls(Result.Ok(W { o:
        // Option.Some(Option.Some(n)) }))`). The two inline registrations above
        // free `{ptr,len,cap}` payload words and cannot see it, and the callee
        // declines this population by design (see the `functions.rs` loop), so
        // the spilled temp is the only owner available — 32 B per call without
        // it, which is the ONLY form of this row's leak that survives the
        // let-site fix, precisely because it is the only one with no binding.
        //
        // No double free against the callee's arm: the param is entry-copied
        // (this fn only runs when it is), so the arm's `__karac_drop_struct_<T>`
        // frees the COPY's box and this frees the original's.
        // B-2026-08-12-18 — `box_contents` rides along so the interior gets an
        // owner here too. This is the position the row was filed against and
        // the one where the absence is starkest: a fresh temp has no binding
        // in the caller AND the callee declines this population, so before
        // this neither the envelope (fixed by B-2026-08-12-15) nor the heap
        // inside it had an owner in any frame.
        for (
            outer_enum,
            outer_variant,
            inner_tag_field,
            inner_enum,
            inner_variant,
            deeper,
            box_contents,
        ) in self.struct_payload_boxed_field_variants(param_te)
        {
            self.track_nested_boxed_enum_var_at_field(
                "__optres_arg_tmp",
                slot,
                outer_enum,
                outer_variant,
                inner_tag_field,
                inner_enum,
                inner_variant,
                deeper,
                box_contents,
            );
        }
    }

    pub(super) fn callee_optres_param_entry_copied(
        &self,
        callee_name: &str,
        arg_index: usize,
    ) -> Option<TypeExpr> {
        let program = self.program_snapshot.as_deref()?;
        let bare = callee_name.rsplit('.').next().unwrap_or(callee_name);
        let check = |f: &crate::ast::Function, ast_i: usize| -> Option<TypeExpr> {
            f.params
                .get(ast_i)
                .filter(|p| self.optres_param_entry_copied_te(&p.ty))
                .map(|p| p.ty.clone())
        };
        program.items.iter().find_map(|item| match item {
            crate::ast::Item::Function(f) if f.name == callee_name => check(f, arg_index),
            crate::ast::Item::ImplBlock(b) => b.items.iter().find_map(|ii| match ii {
                crate::ast::ImplItem::Method(f) if f.name == bare => {
                    let ast_i = if f.self_param.is_some() {
                        arg_index.checked_sub(1)?
                    } else {
                        arg_index
                    };
                    check(f, ast_i)
                }
                _ => None,
            }),
            _ => None,
        })
    }

    pub(super) fn call_arg_flows_into_return(&self, callee_name: &str, arg_index: usize) -> bool {
        let Some(program) = self.program_snapshot.as_deref() else {
            return false;
        };
        program.items.iter().any(|item| {
            matches!(item, crate::ast::Item::Function(f)
                if f.name == callee_name && crate::ast::fn_returns_param(f, arg_index))
        })
    }

    /// B-2026-08-26-9 — the ESCAPE-THROUGH-A-BORROW sibling of
    /// [`Self::call_arg_flows_into_return`]. That one asks whether the callee
    /// hands the argument back through the return value; this asks whether it
    /// stores the argument into `self` or into a `ref`/`mut ref` parameter —
    /// a place the CALLER already holds, so the value is still alive when the
    /// call returns and the caller's fresh-temp drop would be a second owner.
    ///
    /// Resolves through [`super::declarations::find_function_ast`] rather than
    /// the free-function-only scan its sibling uses, because the shape that
    /// motivated this is a METHOD (`q.push(Item { .. })` on
    /// `fn push(mut ref self, x: T)`); a `Item::Function`-only lookup would
    /// answer false for exactly the calls that need it.
    ///
    /// See [`crate::ast::fn_moves_param_into_outliving_place`] for the
    /// predicate itself and for why it is conservative toward `true`.
    ///
    /// `receiver_counted` says whether `arg_index` counts the RECEIVER as
    /// argument 0. The three call sites disagree and the mismatch is silent —
    /// an index one off simply reads a parameter that isn't there and answers
    /// `false`, i.e. the gate quietly stops existing. The free-fn arg loop and
    /// the method arg loop both index the non-self args (`false`);
    /// `compile_generic_call` receives a `make_generic_impl_method_function`
    /// desugaring whose `all_args` puts the receiver at 0 (`true`), while the
    /// AST this resolves against still carries that receiver in `self_param`
    /// and not in `params`. Normalizing here rather than at each site keeps the
    /// conversion in the one place that can see both conventions — the same
    /// `checked_sub(1)` shape [`Self::callee_optres_param_entry_copied`] uses.
    /// Argument 0 under `receiver_counted` IS the receiver, which is a borrow
    /// with no caller temp drop to suppress, so `false` there is right.
    pub(super) fn call_arg_moves_into_outliving_place(
        &self,
        callee_name: &str,
        arg_index: usize,
        receiver_counted: bool,
    ) -> bool {
        // `program_snapshot` holds the USER program only, so a stdlib callee
        // (`PriorityQueue.push` — the shape this row was filed on) resolves to
        // nothing there. `mono_state.generic_fns` carries it: every generic
        // callee, stdlib included, is registered there as the
        // `make_generic_impl_method_function` desugaring. That form moves the
        // receiver into `params[0]` (still named `self`, still typed `mut ref
        // Target[T]`) and leaves `self_param` empty, which is why the
        // normalization below keys on `self_param` rather than on the call
        // site's own shape — under the desugaring `receiver_counted` indices
        // already line up with `params` and must NOT be shifted.
        let Some(f) = self
            .program_snapshot
            .as_deref()
            .and_then(|p| super::declarations::find_function_ast(p, callee_name))
            .or_else(|| self.mono_state.generic_fns.get(callee_name))
        else {
            return false;
        };
        let declared = if receiver_counted && f.self_param.is_some() {
            match arg_index.checked_sub(1) {
                Some(d) => d,
                None => return false,
            }
        } else {
            arg_index
        };
        crate::ast::fn_moves_param_into_outliving_place(f, declared)
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
        let ret_ty_name = match &tail.kind {
            ExprKind::Call { callee, .. } => match &callee.kind {
                ExprKind::Identifier(fn_name) => {
                    self.fn_sig.fn_return_type_names.get(fn_name).cloned()
                }
                _ => None,
            },
            // B-2026-07-30-11 (user-method discard): `f.make();` /
            // `Fac.new();` — a USER impl method returning an owned struct or
            // value enum by value, resolved through the qualified
            // `Type.method` entry (only declared impl methods have one, so
            // builtins self-gate). Borrow-returning user methods are excluded
            // (`user_ref_method_names`), the builtin borrow names outright
            // (a user method shadowing `get` must not fire on an arena/
            // container alias). Enum returns admitted since B-2026-08-01-2
            // (the payload walker below handles them, same as the free-fn
            // arm). Interp twin: the widened MethodCall arm of
            // `discard_rhs_produces_owned_value`.
            ExprKind::MethodCall { object, method, .. }
                if !self.user_ref_method_names.contains(method.as_str())
                    && !matches!(method.as_str(), "get" | "first" | "last" | "peek") =>
            {
                let recv_ty = match &object.kind {
                    // Associated call: `Type.new(...)` parses as a MethodCall
                    // whose receiver Identifier names a type, not a variable
                    // (same disambiguation as `type_name_of_expr`).
                    ExprKind::Identifier(recv)
                        if !self.var_types.var_type_names.contains_key(recv.as_str())
                            && (self.type_decls.struct_types.contains_key(recv.as_str())
                                || self.type_decls.enum_layouts.contains_key(recv.as_str())) =>
                    {
                        Some(recv.clone())
                    }
                    _ => self.type_name_of_expr(object),
                };
                recv_ty
                    .and_then(|t| {
                        self.fn_sig
                            .fn_return_type_names
                            .get(&format!("{t}.{method}"))
                            .cloned()
                    })
                    // B-2026-08-03-2 (class 2) — a BUILTIN container removal
                    // that hands back the element BY VALUE (`v.remove(i);`,
                    // `v.swap_remove(i);`). No `Vec.remove` entry exists in
                    // `fn_return_type_names` (builtins are not declared impl
                    // methods), so the lookup above missed and the discarded
                    // element got no registration at all: its Drop body never
                    // ran AND its heap leaked, on the shipping path. Resolve
                    // the element type from the receiver's recorded element
                    // TypeExpr instead — the same side-table the container's
                    // own drop machinery keys on. `pop` is NOT here: it
                    // returns `Option[T]`, which the optres arm of this same
                    // battery already covers.
                    .or_else(|| {
                        if !matches!(method.as_str(), "remove" | "swap_remove") {
                            return None;
                        }
                        let ExprKind::Identifier(recv) = &object.kind else {
                            return None;
                        };
                        // Vec/VecDeque receivers only — a Map's `remove`
                        // returns an Option and is already handled.
                        if !matches!(
                            self.var_types
                                .var_type_names
                                .get(recv.as_str())
                                .map(|s| s.as_str()),
                            Some("Vec") | Some("VecDeque")
                        ) {
                            return None;
                        }
                        match &self.var_types.var_elem_type_exprs.get(recv.as_str())?.kind {
                            TypeKind::Path(p) => p.segments.first().cloned(),
                            _ => None,
                        }
                    })
                    .filter(|ret| {
                        self.type_decls.struct_types.contains_key(ret.as_str())
                            || self.type_decls.enum_layouts.contains_key(ret.as_str())
                    })
            }
            _ => None,
        };
        let Some(ret_ty_name) = ret_ty_name else {
            return;
        };
        let has_user_drop = self
            .program_snapshot
            .as_deref()
            .map(|p| p.drop_method_keys.contains_key(&ret_ty_name))
            .unwrap_or(false);
        if self.type_decls.shared_types.contains_key(&ret_ty_name) {
            return;
        }
        // B-2026-07-30-11 SHAPE 2 (discard position) — the return type declares
        // no `Drop` of its own but CONTAINS a Drop-bearing field, so `make();`
        // ran nothing and leaked that field's resource. There is no
        // `karac_drop_<T>` wrapper to hang the body off (none is built for a
        // type with no `Drop`), so register the field-body walk directly, the
        // same substitution the `let`-path makes (`stmts.rs`,
        // `has_field_user_drop`). Bodies only — the memory side is the discard
        // path's own business and is untouched here.
        let field_bodies_only = !has_user_drop;
        let is_enum = self.type_decls.enum_layouts.contains_key(&ret_ty_name);
        if !is_enum && !self.type_decls.struct_types.contains_key(&ret_ty_name) {
            return;
        }
        let inkwell::types::BasicTypeEnum::StructType(agg_ty) = val.get_type() else {
            return;
        };
        let Some(cur_fn) = self.current_fn else {
            return;
        };
        let bodies_fn = if !field_bodies_only {
            None
        } else if is_enum {
            // B-2026-08-01-2 — a discarded user-ENUM return whose live
            // variant carries a Drop-bearing payload (`let _ = mk_enum();`,
            // `mk_enum();`). This arm used to return here ("the field-body
            // walk is struct-shaped; an enum payload is SHAPE 1 and stays
            // out of scope"), which made `karac run` (the interpreter's
            // discard walk fires the payload body) and `karac build`
            // (silent) DIVERGE. The enum sibling of the struct field-body
            // walk is `__karac_dropelems_enum_<E>` — declared-type-driven
            // and BODIES ONLY, so an erased-generic payload stays silent on
            // both backends and no memory free moves. `None` means no
            // variant carries a Drop-bearing payload: nothing to run.
            match self.emit_enum_payload_user_drop_bodies_fn(&ret_ty_name) {
                Some(f) => Some((f, UserDropKind::ContainerElemBodies)),
                None => return,
            }
        } else {
            if !self.type_runs_user_drop(&ret_ty_name, &mut Vec::new()) {
                return;
            }
            match self.field_bodies_fn_for_owned_temp(&ret_ty_name) {
                Some(f) => Some((f, UserDropKind::StructFieldBodies)),
                None => return,
            }
        };
        let slot = self.create_entry_alloca(cur_fn, "__owned_agg_tmp", agg_ty.into());
        self.builder.build_store(slot, val).unwrap();
        // Memory BEFORE bodies, deliberately: the one-shot discard frame
        // drains LIFO, so the later-pushed UserDrop body runs FIRST and the
        // enum drop switch frees the payload's heap after it. The reverse
        // order let the switch free a `String` payload before the body read
        // it — `drop 41 h41` printed garbage under AOT/JIT while the
        // interpreter was fine (B-2026-08-01-2; same rule as the ctor arm's
        // "pushed AFTER the battery so the LIFO drain runs them before the
        // battery's frees").
        if is_enum && self.enum_has_heap_payload(&ret_ty_name) {
            self.track_enum_var(&ret_ty_name, slot);
        }
        // Struct sibling of the enum memory call above (b164 leg 2): the
        // field-bodies walk is BODIES ONLY and the generic owned-temp
        // chokepoint declines struct aggregates, so a discarded
        // no-own-Drop struct temp with heap fields (`mk_h();` /
        // `let _ = mk_h();` over `Holder { r: Res { name: String } }`)
        // fired the field body but leaked the String on both discard
        // arms. Register the memory-only StructDrop synthesis — same
        // memory-before-bodies frame order, and `track_struct_var`
        // no-ops when nothing needs freeing. Own-Drop structs are
        // covered by their wrapper (body+memory) below; generic structs
        // keep the conservative skip (base-name synthesis could free
        // through the wrong element type, B-2026-07-11-35).
        if !is_enum
            && field_bodies_only
            && self
                .type_decls
                .struct_generic_params
                .get(ret_ty_name.as_str())
                .is_none_or(|ps| ps.is_empty())
        {
            self.track_struct_var(&ret_ty_name, slot);
        }
        // The kind travels WITH the function from the branch that built it:
        // this site registers an enum-payload container walk on one leg and a
        // struct field-bodies walk on the other, so there is no single answer
        // to hard-code here (B-2026-08-27-8).
        match bodies_fn {
            Some((f, kind)) => {
                self.track_user_drop_var_with_fn(&ret_ty_name, "__owned_agg_tmp", slot, f, kind)
            }
            None => self.track_user_drop_var(&ret_ty_name, "__owned_agg_tmp", slot),
        }
    }

    /// B-2026-07-30-11 SHAPE 2 — the `__karac_dropbodies_<T>` walk for an owned
    /// AGGREGATE TEMP of a type that declares no `Drop` of its own but carries a
    /// Drop-bearing field, or `None` when `T` is not that shape.
    ///
    /// Every owned-temp registrar in this file gated on `drop_method_keys`
    /// alone, so `struct Holder { r: Res }` (where only `Res` implements `Drop`)
    /// ran nothing when the temp died: `consume(Holder { .. })`,
    /// `consume(make())` and `make();` each leaked the field's resource once per
    /// call, while the `let`-bound sibling (`let h = Holder { .. }; consume(h)`)
    /// worked — B-2026-07-29-39 taught the let-path this walk and stopped there.
    /// This is the temp-side half of that same substitution: a type with no
    /// `Drop` has no `karac_drop_<T>` wrapper to hang a body off, so the
    /// field-body fn goes on the `UserDrop` channel directly.
    ///
    /// BODIES ONLY, deliberately — the caller keeps whatever memory
    /// registration it already made. `emit_user_drop_field_bodies_fn` never
    /// frees (its own doc-comment explains why: the parent's memory drop already
    /// reaches those fields as `NestedStruct`), so this is purely additive and
    /// cannot double-free.
    ///
    /// Struct-shaped only. The walk GEPs the parent's fields, so an enum
    /// variant's Drop-bearing payload is not reachable through it — that is
    /// SHAPE 1 of the same ledger entry, which needs the walk hung off
    /// `emit_enum_drop_switch` instead and is still open.
    ///
    /// The generic substitution is empty, matching `emit_user_drop_wrapper`'s
    /// call: a temp has no binding name, so there is no recorded instantiation
    /// to derive one from. A generic parent therefore gets the base-layout walk.
    pub(super) fn field_bodies_fn_for_owned_temp(
        &mut self,
        type_name: &str,
    ) -> Option<FunctionValue<'ctx>> {
        self.field_bodies_fn_for_owned_temp_skipping(type_name, &std::collections::HashSet::new())
    }

    /// [`Self::field_bodies_fn_for_owned_temp`] with a set of field indices
    /// MASKED OUT of the walk — the fields of this temp that the callee hands
    /// back through its return value, whose bodies the RESULT's owner runs
    /// (B-2026-08-28-17). A separate entry point rather than a widened
    /// signature because the other four callers of the plain form register a
    /// temp nobody is extracting fields out of; `emit_user_drop_field_bodies_-
    /// fn_skipping` already folds the surviving index list into the symbol
    /// name, so a masked walker never aliases the full one in the module memo.
    pub(super) fn field_bodies_fn_for_owned_temp_skipping(
        &mut self,
        type_name: &str,
        skip: &std::collections::HashSet<usize>,
    ) -> Option<FunctionValue<'ctx>> {
        if !self.type_decls.struct_types.contains_key(type_name)
            || !self.type_runs_user_drop(type_name, &mut Vec::new())
        {
            return None;
        }
        self.emit_user_drop_field_bodies_fn_skipping(
            type_name,
            &std::collections::HashMap::new(),
            skip,
        )
    }

    /// The struct type name of an argument expression that materializes a FRESH
    /// owned struct temporary — the shapes whose heap this frame must free.
    ///
    /// Two of them, and they are the same value with two spellings. A struct
    /// LITERAL (`f(S { name: g() })`) is the original. A top-level `.clone()`
    /// (`f(a.clone())`) is the one B-2026-08-27-29 was: `clone` deep-copies the
    /// receiver's heap into a value with no binding, and the callee's entry copy
    /// (`make_aggregate_param_callee_owned_inst`) duplicates it AGAIN into the
    /// callee's frame — so the callee frees only its own copy and the caller's
    /// clone is orphaned. Measured on `take(a.clone())` over `It { id: i64,
    /// name: String }` as 34 bytes per call, the cloned `String` field, once per
    /// iteration.
    ///
    /// A `String` / `Vec` clone in the same position is CLEAN and stays out of
    /// this: those pass their `{ptr,len,cap}` header by value and the callee
    /// frees it directly, with no entry copy to orphan an original. The helper
    /// never sees them — the caller early-returns on `vec_struct_type`.
    ///
    /// The receiver must be an IDENTIFIER of known struct type, which is what
    /// makes the answer a name rather than a guess: every drop registered below
    /// is keyed off it, and `clone`'s result is the receiver's own type by
    /// definition. A field / index / call receiver declines into the status-quo
    /// leak rather than into a name this frame cannot verify.
    fn owned_struct_temp_arg_name(&self, arg: &Expr) -> Option<String> {
        let is_struct = |n: &str| self.type_decls.struct_types.contains_key(n);
        match &arg.kind {
            ExprKind::StructLiteral { path, .. } => {
                path.last().filter(|n| is_struct(n.as_str())).cloned()
            }
            ExprKind::MethodCall { object, method, .. } if method == "clone" => {
                let name = match &object.kind {
                    ExprKind::Identifier(var) => {
                        self.var_types.var_type_names.get(var.as_str()).cloned()?
                    }
                    // `f(v[i].clone())` — the element read is a borrow and the
                    // clone is an independent copy of it, so the temporary is
                    // this frame's exactly as the identifier form's is. This is
                    // the spelling B-2026-08-26-21's diagnostic points authors
                    // at, which is why it is covered rather than left to the
                    // status-quo leak.
                    ExprKind::Index {
                        object: base,
                        index,
                    } if !matches!(&index.kind, ExprKind::Range { .. }) => {
                        let te = self.vec_index_elem_type_expr(base)?;
                        let TypeKind::Path(p) = &te.kind else {
                            return None;
                        };
                        p.segments.last()?.clone()
                    }
                    _ => return None,
                };
                Some(name).filter(|n| is_struct(n.as_str()))
            }
            _ => None,
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
        arg_escapes_frame: bool,
    ) {
        self.track_inline_owned_aggregate_arg_inst(val, arg, arg_escapes_frame, None, false, &[])
    }

    /// [`Self::track_inline_owned_aggregate_arg`] carrying the callee's
    /// per-ELEMENT escape set (B-2026-08-28-2). Used by the two by-value CALL
    /// ARGUMENT sites, where the callee is known by name and a tuple element it
    /// returns must not have its body registered caller-side. Every other site
    /// keeps the plain wrapper and its empty set. Interp twin: the tuple arm of
    /// `run_fresh_temp_arg_drops`.
    pub(super) fn track_inline_owned_aggregate_arg_parts(
        &mut self,
        val: BasicValueEnum<'ctx>,
        arg: &Expr,
        arg_escapes_frame: bool,
        escaping_parts: &[crate::ast::ParamPart],
    ) {
        self.track_inline_owned_aggregate_arg_inst(
            val,
            arg,
            arg_escapes_frame,
            None,
            false,
            escaping_parts,
        )
    }

    /// B-2026-08-28-2 — which top-level PARTS of by-value argument slot
    /// `arg_index` the named callee hands back through its return value. Thin
    /// lookup over [`crate::ast::fn_returns_param_parts`]; empty for an unknown
    /// name, a method, or any shape that analysis declines to classify (it
    /// under-approximates on purpose — see its doc). Interp twin:
    /// `callee_returned_param_parts` in `interpreter/eval_call.rs`.
    pub(super) fn callee_returned_param_parts(
        &self,
        callee_name: &str,
        arg_index: usize,
    ) -> Vec<crate::ast::ParamPart> {
        let Some(program) = self.program_snapshot.as_deref() else {
            return Vec::new();
        };
        program
            .items
            .iter()
            .find_map(|item| match item {
                crate::ast::Item::Function(f) if f.name == callee_name => {
                    Some(crate::ast::fn_returns_param_parts(f, arg_index))
                }
                _ => None,
            })
            .unwrap_or_default()
    }

    /// The tuple-INDEX half of a [`Self::callee_returned_param_parts`] answer,
    /// in the shape `track_discarded_tuple_elem_bodies` takes its skip list.
    /// Struct-field parts are not tuple elements; they are resolved by
    /// [`Self::escaping_field_indices`] instead.
    pub(super) fn tuple_indices_of(parts: &[crate::ast::ParamPart]) -> Vec<usize> {
        parts
            .iter()
            .filter_map(|p| match p {
                crate::ast::ParamPart::TupleIndex(i) => Some(*i),
                crate::ast::ParamPart::Field(_) => None,
            })
            .collect()
    }

    /// The struct-FIELD half of a [`Self::callee_returned_param_parts`] answer,
    /// resolved against `struct_name`'s DECLARED field order into the index set
    /// `field_bodies_fn_for_owned_temp_skipping` masks with (B-2026-08-28-17).
    ///
    /// A name the struct does not declare is dropped rather than erroring: the
    /// analysis reports a field name lifted off the callee's source, and this
    /// resolves it against the layout the argument actually has. Dropping an
    /// unresolvable one keeps the walk at its pre-fix (over-firing) behaviour,
    /// which is the safe direction — a WRONGLY masked field would suppress the
    /// only body that runs, the same asymmetry `fn_returns_param_parts`
    /// under-approximates for.
    pub(super) fn escaping_field_indices(
        &self,
        struct_name: &str,
        parts: &[crate::ast::ParamPart],
    ) -> std::collections::HashSet<usize> {
        let Some(names) = self.type_decls.struct_field_names.get(struct_name) else {
            return std::collections::HashSet::new();
        };
        parts
            .iter()
            .filter_map(|p| match p {
                crate::ast::ParamPart::Field(n) => names.iter().position(|f| f == n),
                crate::ast::ParamPart::TupleIndex(_) => None,
            })
            .collect()
    }

    /// [`Self::track_inline_owned_aggregate_arg`] with the struct-literal arg's
    /// resolved generic INSTANTIATION (`Box[String]`), supplied ONLY by the
    /// monomorph call path and ONLY when the callee provably entry-copies the
    /// param (`mono_entry_copies_aggregate_param`, evaluated under the callee's
    /// own substitution). It exists because a generic struct whose heap sits
    /// behind a bare `T` has no name-keyed drop to register, so the caller's
    /// temp was orphaned — B-2026-08-06-2 defect (B). `None` — every other call
    /// site — reproduces the previous behavior exactly.
    ///
    /// The gate is deliberately the CALLEE's predicate rather than a caller-side
    /// look-alike: the same struct reaches its param as own-by-transfer through
    /// a CONCRETE fn (`fn take(b: Box[String])`) and through a monomorph whose
    /// fn-level type param is named differently from the struct's
    /// (`fn take[U](b: Box[U])`, where `mono_struct_type_from_active_subst`
    /// finds no binding for `T` and falls back to the base layout). In both the
    /// callee TAKES the buffer, so a caller drop here would be a double free.
    ///
    /// `callee_entry_copies_mono` is that same predicate's raw answer, and it is
    /// SEPARATE from `mono_inst` because the two can disagree in the direction
    /// that matters. `mono_inst` is additionally conditioned on recovering the
    /// instantiation from the span map, so it reads `None` both when the callee
    /// takes the buffer and when it entry-copies but the span carries no
    /// annotation. Only the first of those may suppress the caller's drop
    /// (B-2026-08-07-17), so `struct_param_owned_by_transfer` is asked with the
    /// flag rather than with `mono_inst.is_none()`.
    pub(super) fn track_inline_owned_aggregate_arg_inst(
        &mut self,
        val: BasicValueEnum<'ctx>,
        arg: &Expr,
        arg_escapes_frame: bool,
        mono_inst: Option<TypeExpr>,
        callee_entry_copies_mono: bool,
        escaping_parts: &[crate::ast::ParamPart],
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
                if let Some(ret_ty_name) = self.fn_sig.fn_return_type_names.get(fn_name).cloned() {
                    let has_user_drop = self
                        .program_snapshot
                        .as_deref()
                        .map(|p| p.drop_method_keys.contains_key(&ret_ty_name))
                        .unwrap_or(false);
                    if has_user_drop && !self.type_decls.shared_types.contains_key(&ret_ty_name) {
                        let is_enum = self.type_decls.enum_layouts.contains_key(&ret_ty_name);
                        let is_struct = self.type_decls.struct_types.contains_key(&ret_ty_name);
                        if is_enum || is_struct {
                            let slot =
                                self.create_entry_alloca(cur_fn, "__owned_agg_tmp", agg_ty.into());
                            self.builder.build_store(slot, val).unwrap();
                            if arg_escapes_frame && is_struct {
                                // B-2026-07-30-12 — MEMORY ONLY, for the reason
                                // spelled out at the struct-literal sibling
                                // below: on the entry-copy passthrough path the
                                // orphaned buffer is ours to free but the body
                                // belongs to the result's consumer. Struct-only
                                // because the enum leg's dual registration below
                                // has no memory-half-alone form.
                                self.track_struct_var(&ret_ty_name, slot);
                                return;
                            }
                            self.track_user_drop_var(&ret_ty_name, "__owned_agg_tmp", slot);
                            if is_enum && self.enum_has_heap_payload(&ret_ty_name) {
                                self.track_enum_var(&ret_ty_name, slot);
                            }
                            return;
                        }
                    }
                    // B-2026-07-30-11 SHAPE 2, fn-call arm — the return type
                    // declares no `Drop` of its own but CONTAINS a Drop-bearing
                    // field (`consume(make())` where `make() -> Holder` and
                    // `Holder { r: Res }`), so the arm above skipped the temp
                    // entirely and the field's resource leaked once per call.
                    // Register the field-body walk on the same UserDrop channel
                    // the struct-literal sibling below uses. Struct-shaped only:
                    // the walk GEPs the parent's fields, and an enum payload is
                    // SHAPE 1 (out of scope, still leaks).
                    if !has_user_drop
                        && !arg_escapes_frame
                        && self.type_decls.struct_types.contains_key(&ret_ty_name)
                    {
                        let bodies_fn = self.field_bodies_fn_for_owned_temp(&ret_ty_name);
                        // B-2026-08-02-28 — the MEMORY half, which this arm
                        // omitted: it registered the bodies walk and returned,
                        // so `use_it(mk(xs))` where `mk() -> Holder` and
                        // `Holder { xs: Vec[Res] }` printed the element body but
                        // freed nothing, leaking the Vec buffer AND its element
                        // leaves once per call. Its struct-LITERAL sibling below
                        // registers both halves for the identical value; the two
                        // arms differ only in how the temp was produced (a call
                        // vs an inline literal), which is not a reason to own it
                        // differently. The callee entry-copies a copy-supported
                        // struct — `arg_is_entry_copied_heap_struct` already
                        // resolves exactly this Call shape through
                        // `fn_return_type_names` — so this caller temp is an
                        // INDEPENDENT buffer and freeing it cannot touch the
                        // callee's copy.
                        // B-2026-08-05-22 — `option_field_te_has_drop_heap` joins
                        // the source-level fallback because BOTH signals above are
                        // blind to a field whose heap sits behind an `Option`:
                        // `aggregate_has_heap_field` walks the LLVM type, where an
                        // `Option` is erased `i64` payload words (the same blindness
                        // the enum-leaf case documents), and `type_expr_has_drop_heap`
                        // returns false for `Option`/`Result` by design, because
                        // "their inline payloads are freed by the let-binding
                        // machinery" — which a fresh-temp ARGUMENT does not have.
                        // So `use_a(mk())` where `A { value: Option[String] }`
                        // registered nothing and leaked one payload per call, while
                        // the same struct plus any plain `String` field was clean
                        // (that field is LLVM-visible, and the Option payload rode
                        // along on its drop).
                        //
                        // Stays under the existing `copy_supported` gate, so the
                        // ownership argument is unchanged: the callee entry-copies a
                        // copy-supported struct, making this caller temp an
                        // INDEPENDENT buffer that nothing else frees.
                        let needs_memory_drop = self.aggregate_has_heap_field(agg_ty)
                            || (self.aggregate_param_copy_supported_struct(
                                &ret_ty_name,
                                &mut Vec::new(),
                            ) && self
                                .type_decls
                                .struct_field_type_exprs
                                .get(&ret_ty_name)
                                .is_some_and(|ftes| {
                                    ftes.iter().any(|f| {
                                        self.type_expr_has_drop_heap(f)
                                            || self.option_field_te_has_drop_heap(f)
                                    })
                                }));
                        if needs_memory_drop || bodies_fn.is_some() {
                            let slot =
                                self.create_entry_alloca(cur_fn, "__owned_agg_tmp", agg_ty.into());
                            self.builder.build_store(slot, val).unwrap();
                            // ORDER IS LOAD-BEARING, same rule as the
                            // struct-literal sibling: the frame drains LIFO, so
                            // memory is pushed FIRST and the bodies walk second,
                            // making the bodies run BEFORE the fields they read
                            // are freed.
                            if needs_memory_drop {
                                self.track_struct_var(&ret_ty_name, slot);
                            }
                            if let Some(bodies_fn) = bodies_fn {
                                self.track_user_drop_var_with_fn(
                                    &ret_ty_name,
                                    "__owned_agg_tmp",
                                    slot,
                                    bodies_fn,
                                    UserDropKind::StructFieldBodies,
                                );
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
            let shared = self.type_decls.shared_types.contains_key(&enum_name);
            let user_drop = has_user_drop && !shared;
            // B-2026-08-06-9 leg B — `enum_drop_switch_does_work`, not
            // `enum_has_heap_payload`. A fresh enum TEMP whose only drop work is
            // freeing an `Option`/`Result` payload BOX (`EnumDropKind::BoxedOptRes`,
            // not heap-BEARING by design) still needs this caller-side owner: it
            // has no binding, so nothing else registers `track_enum_var` for it
            // and the box leaked once per call. The two predicates are separate
            // because `enum_has_heap_payload` also selects `compile_enum_eq`'s
            // variant-aware comparison, which this must not disturb.
            let heap_payload = self.enum_drop_switch_does_work(&enum_name);
            // B-2026-08-01-14 — entry-copy passthrough (`pass2(E2.B(..))`
            // where the callee returns its param): the callee deep-copies
            // the payload at entry and the COPY flows out to the result's
            // consumer, so the ORIGINAL aggregate here is orphaned — free
            // it, MEMORY ONLY (the bodies belong to the result's consumer,
            // exactly the B-2026-07-30-12 struct rule).
            if arg_escapes_frame {
                if heap_payload && !shared {
                    let slot = self.create_entry_alloca(cur_fn, "__owned_agg_tmp", agg_ty.into());
                    self.builder.build_store(slot, val).unwrap();
                    self.track_enum_var(&enum_name, slot);
                }
                return;
            }
            // B-2026-08-01-13 (c1/c5) — the payload-bodies WALKER, the
            // declared-type-driven `__karac_dropelems_enum_<E>` the ctor
            // arm never registered: `check(E2.B(Res { .. }))` fired no
            // payload body on either backend when the callee dropped the
            // enum whole (a destructuring callee's arm channel is now
            // param-gated, so this caller-side fire is the single owner).
            // Option/Result stay with their own payload machinery.
            let walker = if !shared && enum_name != "Option" && enum_name != "Result" {
                self.emit_enum_payload_user_drop_bodies_fn(&enum_name)
            } else {
                None
            };
            if user_drop || heap_payload || walker.is_some() {
                let slot = self.create_entry_alloca(cur_fn, "__owned_agg_tmp", agg_ty.into());
                self.builder.build_store(slot, val).unwrap();
                // Memory first — the frame drains LIFO, so the bodies
                // pushed below fire before the switch frees what they read
                // (the B-2026-08-01-2 rule).
                if heap_payload {
                    self.track_enum_var(&enum_name, slot);
                }
                if let Some(w) = walker {
                    self.track_user_drop_var_with_fn(
                        "",
                        "__owned_agg_tmp",
                        slot,
                        w,
                        UserDropKind::ContainerElemBodies,
                    );
                }
                if user_drop {
                    self.track_user_drop_var(&enum_name, "__owned_agg_tmp", slot);
                }
            }
        } else if let Some(elem_tes) = self.tuple_arg_elem_type_exprs(arg) {
            // #21 — a tuple-shaped arg. The callee entry-copies a heap-bearing
            // tuple param (`make_tuple_param_callee_owned`), so this caller temp
            // is an INDEPENDENT buffer that must free its own heap. The
            // LLVM-type `track_tuple_var` is enum-blind, so derive the element
            // `TypeExpr`s and register a `TypeExpr`-driven drop when any leaf is
            // an enum / nested struct; fall back to the enum-blind path for a
            // pure Vec/String tuple (its layout is visible).
            //
            // B-2026-08-27-44 widened this from a tuple LITERAL to any
            // tuple-shaped arg. `use2(mk(..))` — a CALL returning a tuple — DID
            // reach this registrar (it does not escape, so the admission gate
            // let it through) and then matched NO arm at all: not the enum arm,
            // not this one while it tested `ExprKind::Tuple`, and not the
            // named-struct arm below, whose `owned_struct_temp_arg_name` answers
            // only for a struct literal or a `.clone()`. So it registered
            // nothing and leaked. The literal's behaviour is unchanged —
            // `tuple_arg_elem_type_exprs` returns exactly the old
            // `infer_arg_elem_te` vector for that spelling.
            if elem_tes.iter().any(|e| self.type_expr_has_drop_heap(e)) {
                let slot = self.create_entry_alloca(cur_fn, "__owned_agg_tmp", agg_ty.into());
                self.builder.build_store(slot, val).unwrap();
                if let Some(drop_fn) = self.synthesize_tuple_drop_fn_te(agg_ty, &elem_tes) {
                    if let Some(frame) = self.drop_rc.scope_cleanup_actions.last_mut() {
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
            // B-2026-07-30-11 (param-tuple leg, the A shape): the memory
            // registrations above run no user Drop BODIES, so
            // `take_tuple((Res { id: 41 }, 10))` never fired the element's
            // body on this backend. Register the bodies walk on the same
            // frame, AFTER the memory push — the drain is LIFO, so bodies
            // run before the frees that invalidate what they read. Gated to
            // all-fresh-or-scalar elements exactly like the wildcard-let
            // discard (a place element's body belongs to its own binding),
            // and off the passthrough path (a returned tuple's bodies belong
            // to the result's consumer). Interp twin:
            // `run_fresh_temp_arg_drops`' tuple arm.
            //
            // B-2026-08-27-44: LITERAL-ONLY, and deliberately so. The gate is
            // per-ELEMENT freshness, which needs the element EXPRESSIONS; a call
            // that returns a tuple has none to inspect, so there is no way to
            // tell a fresh element from a place element whose body belongs to
            // its own binding. Memory-only there matches what the interpreter
            // twin (`run_fresh_temp_arg_drops`) already does for that spelling,
            // so widening the memory registration above does not open a
            // run-vs-build divergence here.
            if let ExprKind::Tuple(tuple_elems) = &arg.kind {
                if !arg_escapes_frame
                    && tuple_elems
                        .iter()
                        .all(|e| self.discard_tuple_elem_is_fresh_expr(e))
                {
                    // B-2026-08-28-2 — per-ELEMENT. `arg_escapes_frame` above is
                    // the WHOLE-param question; a callee that extracts one
                    // element and returns THAT passes it while still handing the
                    // element to the caller's result owner, so its body ran here
                    // and again there. Suppressing the whole registration
                    // instead loses the bodies of the elements that really do
                    // die in the call, so the escaping ones are skipped
                    // individually. Interp twin: `run_fresh_temp_arg_drops`.
                    let skip = Self::tuple_indices_of(escaping_parts);
                    self.track_discarded_tuple_elem_bodies(tuple_elems, val, &skip);
                }
            }
        } else if let Some(name) = self.owned_struct_temp_arg_name(arg) {
            {
                // MEMORY ONLY for a `.clone()` temp, and the reason is parity
                // rather than caution. A clone temp runs no user `Drop` body on
                // EITHER backend today: this arm's body registrations are
                // literal-shaped, and the interpreter's twin
                // (`run_fresh_temp_arg_drops`) resolves a type name for a struct
                // literal / fn call / variant ctor and returns `None` for a
                // method call. So the two agree, and registering the body here
                // alone would trade B-2026-08-27-29's leak for a run-vs-build
                // divergence — the strictly worse defect. Whether a clone temp
                // SHOULD run a body is a separate question about a shape both
                // backends currently skip; the free is not, and the free is what
                // this row is.
                let clone_temp = matches!(
                    &arg.kind,
                    ExprKind::MethodCall { method, .. } if method == "clone"
                );
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
                if has_user_drop && !clone_temp && !self.type_decls.shared_types.contains_key(&name)
                {
                    let slot = self.create_entry_alloca(cur_fn, "__owned_agg_tmp", agg_ty.into());
                    self.builder.build_store(slot, val).unwrap();
                    if arg_escapes_frame {
                        // B-2026-07-30-12 — MEMORY ONLY on the passthrough path.
                        // We are here despite the guard because
                        // `arg_is_entry_copied_heap_struct` overrode it
                        // (B-2026-07-08-6): the callee entry-copies and returns
                        // an INDEPENDENT copy, so this original buffer is
                        // orphaned and does need freeing. Its user `Drop` BODY
                        // does not belong here — the value flows out to the
                        // result's consumer, whose own drop runs it. Registering
                        // the full `karac_drop_<T>` wrapper (body + fields +
                        // memory) ran the body TWICE: `let p = pass(Guard { .. })`
                        // printed the body once under the interpreter and twice
                        // under AOT/JIT. That parity break shipped with -08-6 and
                        // is what this arm fixes; `track_struct_var` is the
                        // memory half alone.
                        self.track_struct_var(&name, slot);
                    } else {
                        self.track_user_drop_var(&name, "__owned_agg_tmp", slot);
                    }
                    return;
                }
                // B-2026-07-30-11 SHAPE 2 — `name` declares no `Drop` of its own
                // but CONTAINS a Drop-bearing field, so nothing above fires and
                // the field's resource leaked once per call
                // (`consume(Holder { r: Res { .. } })` where only `Res: Drop`).
                // The field-body walk is ADDITIVE to the memory registration
                // below, exactly as on the `let`-path (`stmts.rs`,
                // `has_field_user_drop`): bodies on the `UserDrop` channel, the
                // ordinary struct drop unchanged. Both target the same slot, so
                // it is materialized once here and reused.
                //
                // NOT on the passthrough path, and that asymmetry is the point.
                // We only reach this helper with `arg_escapes_frame` when
                // `arg_is_entry_copied_heap_struct` overrode the guard
                // (B-2026-07-08-6) — the callee entry-COPIES the struct and
                // returns an independent copy, so the caller's original buffer
                // is orphaned and its MEMORY does need freeing here. Its user
                // body does not: the value flows out to the result's consumer,
                // whose own drop runs it. Registering both would print the body
                // twice for `let p = pass(Holder { .. })`. Memory follows the
                // buffer; bodies follow the value.
                let field_bodies_fn = if arg_escapes_frame || clone_temp {
                    None
                } else {
                    // B-2026-08-28-17 — per-FIELD, the struct twin of the
                    // per-element tuple filter above. `arg_escapes_frame` is the
                    // WHOLE-param question; a callee that extracts one field and
                    // returns THAT (`fn take(w: W) -> R { let W { r, n } = w; r }`,
                    // or the `w.r` spelling) passes it while still handing the
                    // field to the caller's result owner, so the field's body ran
                    // here AND there. Suppressing the whole walk instead loses the
                    // bodies of the fields that really do die in the call —
                    // measured on `struct W { a: R, b: R }` returning `a`, which
                    // needs `b`'s body and not `a`'s. Interp twin:
                    // `run_fresh_temp_arg_drops`' masked-value call.
                    let skip = self.escaping_field_indices(&name, escaping_parts);
                    self.field_bodies_fn_for_owned_temp_skipping(&name, &skip)
                };
                let llvm_heap = self.aggregate_has_heap_field(agg_ty);
                let src_heap_copyable = !llvm_heap
                    && self.aggregate_param_copy_supported_struct(&name, &mut Vec::new())
                    && (self.type_decls.struct_field_type_exprs.get(&name).is_some_and(|ftes| {
                        ftes.iter().any(|f| {
                            self.type_expr_has_drop_heap(f)
                                // B-2026-08-07-12 root A — `type_expr_has_drop_heap`
                                // hardcodes `"Option" | "Result" => false`, and
                                // `aggregate_has_heap_field` above is LLVM-structural
                                // so an erased `Option` payload never matches its
                                // `{ptr,len,cap}` test either. Both signals blind
                                // means `f(S { s: Option.Some("x") })` registered NO
                                // caller temp drop and leaked the buffer at BOTH opt
                                // levels — ordinary `karac build` output, not an -O0
                                // curiosity. The FN-CALL arm of this same function
                                // already ORs this companion in (B-2026-08-05-22);
                                // the struct-LITERAL arm never got it, which is why
                                // `f(mk())` was clean and `f(S { .. })` was not.
                                //
                                // A sibling plain `String` field masked it entirely:
                                // the struct then qualified via `llvm_heap` and its
                                // drop freed the `Option` field correctly. So the
                                // drop was always right and only this gate was wrong.
                                || self.option_field_te_has_drop_heap(f)
                        })
                    })
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
                // B-2026-08-07-15 — the callee TAKES this struct. The `llvm_heap`
                // disjunct above is registered unconditionally, and the comment
                // at the head of this arm gives the reason: "whenever its drop
                // frees a buffer the callee either entry-copies (independent) or
                // caller-retains (shares, never frees)". That was exhaustive when
                // it was written and B-2026-08-05-33 added a THIRD case —
                // own-by-transfer, where the callee neither copies nor retains but
                // takes the caller's buffers and registers the drop that frees
                // them. Its safety argument is an explicit lockstep with the
                // caller's retraction, and `move_declined_copy_struct_arg` honours
                // that only for an IDENTIFIER argument; a fresh struct LITERAL has
                // no binding to retract and lands here instead, so both frames
                // freed the same heap.
                //
                // Measured on `fn ig(x: S)` with `struct S { a: String, m: Map[..]
                // }` — a callee whose body is `1` — as 480 valgrind errors over 10
                // iterations at BOTH opt levels, i.e. ordinary `karac build`
                // output. `Set`, `Vec`-instead-of-`String`, methods and generic
                // fns all reproduce; the same struct passed as a NAMED binding is
                // clean, which is exactly the half of the lockstep that exists.
                //
                // A GENERIC struct is not exempt, and B-2026-08-07-17 is what
                // that costs when it is. The predicate's first cut excluded
                // every generic struct, on the sound observation that a
                // caller-side look-alike cannot evaluate the callee's mono
                // rescue — but the rescue only exists on the monomorph path,
                // and a generic struct also reaches a CONCRETE param
                // (`fn take(x: Mix[String])`) where there is no subst to read.
                // `Mix[T] { v: T, s: String }` spelled that way kept both owners
                // and stayed at 10 invalid frees per 10 iterations at BOTH opt
                // levels. Erasure is just another way to fail copy-support — the
                // bare `T` lands on `field_copy_supported`'s conservative
                // `_ => false` — so it is this bug, not a neighbour of it.
                //
                // What the predicate needs is the callee's ANSWER, which
                // `compile_generic_call` computes under the callee's own
                // substitution and threads down as `callee_entry_copies_mono`.
                // Not `mono_inst.is_some()`: that is additionally conditioned on
                // the span map carrying the instantiation, so it reads `None`
                // for an entry-copying callee whose span has no annotation, and
                // suppressing there would trade this corruption for a leak.
                let callee_owns_by_transfer =
                    self.struct_param_owned_by_transfer(&name, callee_entry_copies_mono);
                let needs_memory_drop = !callee_owns_by_transfer
                    && (llvm_heap || src_heap_copyable || src_shared_owning);
                if needs_memory_drop || field_bodies_fn.is_some() {
                    let slot = self.create_entry_alloca(cur_fn, "__owned_agg_tmp", agg_ty.into());
                    self.builder.build_store(slot, val).unwrap();
                    // ORDER IS LOAD-BEARING and inverted from the `let`-path.
                    // The frame drains LIFO (`drain_top_frame_with_emit`), so the
                    // memory drop must be pushed FIRST for the body to run
                    // BEFORE the fields it reads are freed — `karac_drop_<T>`'s
                    // own body-then-fields order, reproduced across two actions.
                    // The `let`-path registers the bodies first because there
                    // `fire_due_user_drops` lifts them out at the binding's NLL
                    // point, ahead of the scope-exit drain entirely; a temp has
                    // no last-use entry, so both actions drain together here and
                    // only the push order separates them. Reversed, the body
                    // reads a freed `String` and prints garbage.
                    if needs_memory_drop {
                        // B-2026-08-06-2 defect (B) — a GENERIC struct whose heap
                        // sits behind a bare `T` (`Box[T] { v: T }`) has no
                        // name-keyed drop: `emit_struct_drop_synthesis_mono` reads
                        // the erased field, classifies it as no-heap, and
                        // `track_struct_var` silently registers NOTHING. The
                        // caller's temp was then orphaned for exactly the callees
                        // that entry-copy it — one leaked buffer per call. The
                        // monomorph call path resolves the instantiation and hands
                        // it down here (`mono_inst`); everyone else keeps the
                        // name-keyed behavior byte-for-byte.
                        match mono_inst {
                            Some(inst) => self.track_struct_var_inst(&name, slot, Some(inst)),
                            None => self.track_struct_var(&name, slot),
                        }
                    }
                    if let Some(bodies_fn) = field_bodies_fn {
                        self.track_user_drop_var_with_fn(
                            &name,
                            "__owned_agg_tmp",
                            slot,
                            bodies_fn,
                            UserDropKind::StructFieldBodies,
                        );
                    }
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
        self.mono_state.generic_fns.contains_key(name) || self.module.get_function(name).is_some()
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
                if let Some(&inner_ty) = self.borrow_vars.ref_params.get(name.as_str()) {
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

    /// Pointer to the PLACE a `mut ref` argument denotes — B-2026-08-05-37.
    ///
    /// `None` for anything that is not a place this pass can resolve, and the
    /// caller then falls through to the existing rvalue path unchanged. That
    /// is the pre-fix behaviour, so an unresolved shape is no worse than
    /// before (its write is still lost) rather than miscompiled differently.
    ///
    /// Mostly [`Self::field_chain_place_ptr`], with ONE deliberate difference:
    /// that function bails at a `ref` / `mut ref` parameter root, because its
    /// other callers must not write through a borrow they do not own. Here
    /// following the borrow is exactly the point — `fn go(p: mut ref P) {
    /// bump(p.v); }` is the FORWARDING spelling, the one the typechecker
    /// directs authors to ("this argument is already a mut-ref; drop the `mut`
    /// marker"), and it was silently losing its write. So the root arm loads
    /// the stored pointer and the projection hops proceed from there, rather
    /// than widening `field_chain_place_ptr` for every one of its callers.
    pub(super) fn mut_ref_place_arg_ptr(&mut self, expr: &Expr) -> Option<PointerValue<'ctx>> {
        match &expr.kind {
            // Only projections need this. A bare identifier already took the
            // `get_data_ptr` fast path above, and a non-place expression has
            // no caller storage to write back into.
            ExprKind::FieldAccess { object, field } => {
                let obj_ty = self.place_chain_type_name(object)?;
                // A `shared` / `par` struct binding's slot holds a HANDLE, not
                // the aggregate, so GEPing it as the struct would write past
                // the 8-byte slot into neighbouring stack storage. The place
                // is one load in, at the header-shifted offset — resolve it
                // through the same `shared_gep_layout` funnel every other
                // shared field site uses (B-2026-08-05-41 codegen half).
                // Before this, the arm bailed and the write went to the
                // rvalue COPY: `bump(mut g.val)` on a `shared struct N { mut
                // val: i64 }` printed 7 instead of 8 under AOT and JIT while
                // the interpreter printed 8.
                //
                // The write's LEGALITY is the typechecker's job, not this
                // arm's: half 1 of the same row extended `SharedFieldNotMut`
                // to a `mut`-marked argument, so an undeclared-`mut` shared
                // field is refused before codegen ever sees it — exactly as
                // the assignment spelling already was.
                if self.type_decls.shared_types.contains_key(obj_ty.as_str()) {
                    return self.shared_mut_ref_place_arg_ptr(object, &obj_ty, field);
                }
                let base = self.mut_ref_place_root_ptr(object)?;
                let st = *self.type_decls.struct_types.get(obj_ty.as_str())?;
                let idx = self
                    .type_decls
                    .struct_field_names
                    .get(obj_ty.as_str())?
                    .iter()
                    .position(|n| n == field)? as u32;
                self.builder
                    .build_struct_gep(st, base, idx, "mutref.arg.field.p")
                    .ok()
            }
            ExprKind::TupleIndex { object, index } => {
                let base = self.mut_ref_place_root_ptr(object)?;
                let tuple_ty = self.place_chain_aggregate_llvm_type(object)?;
                self.builder
                    .build_struct_gep(tuple_ty, base, *index as u32, "mutref.arg.tupidx.p")
                    .ok()
            }
            _ => None,
        }
    }

    /// Place pointer for a `mut ref` argument that projects a field out of a
    /// `shared` / `par` STRUCT receiver — the RC half of B-2026-08-05-41.
    ///
    /// The receiver compiles to the heap-node pointer (`load_variable` applies
    /// the right number of loads for an owned local, a constructor binding and
    /// a `ref self` param alike), and the user field sits at the
    /// header-shifted offset [`Self::shared_gep_layout`] reports — base 1 for
    /// a conventional headed box, 2 for a weak-targeted one, 0 for a
    /// headerless member. `par` needs no separate handling: its layout is
    /// identical and only the count ops differ, and none are emitted here (a
    /// borrow is not an ownership transfer).
    ///
    /// Deliberately narrow, because this family's history is over-broad
    /// widenings that had to be reverted:
    /// * **Pure field chains only** — an identifier or `self` root, with
    ///   `FieldAccess` hops above it (`g.val`, `self.val`, `o.inner.v`). Both
    ///   call sites may have already compiled the argument, so re-evaluating
    ///   an arbitrary receiver expression here would duplicate its side
    ///   effects; a chain of variable loads and GEPs has none. An `Index` hop
    ///   is excluded on the same rule — the subscript would be evaluated a
    ///   second time.
    /// * **No shared ENUM**, which has no named fields at all.
    /// * **No `weak` field and no niche `Option[shared]` field.** Neither slot
    ///   holds the value its source type names — one is a non-owning back-edge
    ///   that reads as an upgrade, the other a bare pointer standing in for a
    ///   four-word Option — so handing a callee a raw pointer to either would
    ///   let it write a value of the wrong shape. Those fall back to the
    ///   pre-existing rvalue path (no worse than before this fix).
    pub(super) fn shared_mut_ref_place_arg_ptr(
        &mut self,
        object: &Expr,
        type_name: &str,
        field: &str,
    ) -> Option<PointerValue<'ctx>> {
        if !Self::is_pure_field_chain(object) {
            return None;
        }
        let info = self.type_decls.shared_types.get(type_name)?.clone();
        if info.is_enum {
            return None;
        }
        let idx = self
            .type_decls
            .struct_field_names
            .get(type_name)?
            .iter()
            .position(|n| n == field)?;
        if self.struct_field_is_weak(type_name, idx)
            || self.niche_field_inner_heap_type(type_name, idx).is_some()
        {
            return None;
        }
        let ptr = self.compile_expr(object).ok()?;
        if !ptr.is_pointer_value() {
            return None;
        }
        let (gep_ty, base) = self.shared_gep_layout(type_name, info.heap_type);
        self.builder
            .build_struct_gep(
                gep_ty,
                ptr.into_pointer_value(),
                idx as u32 + base,
                "mutref.arg.sh.field.p",
            )
            .ok()
    }

    /// A place expression built only from variable loads and field
    /// projections, so compiling it a second time is free of side effects.
    /// Used by [`Self::shared_mut_ref_place_arg_ptr`] to decide whether the
    /// receiver may be re-compiled at the argument site.
    pub(super) fn is_pure_field_chain(expr: &Expr) -> bool {
        match &expr.kind {
            ExprKind::Identifier(_) | ExprKind::SelfValue => true,
            ExprKind::FieldAccess { object, .. } => Self::is_pure_field_chain(object),
            _ => false,
        }
    }

    /// The base pointer a [`Self::mut_ref_place_arg_ptr`] projection hangs off.
    /// Identical to [`Self::field_chain_place_ptr`] except at a borrow root,
    /// where the stored pointer is followed instead of bailing.
    fn mut_ref_place_root_ptr(&mut self, expr: &Expr) -> Option<PointerValue<'ctx>> {
        let name = match &expr.kind {
            ExprKind::Identifier(n) => n.as_str(),
            ExprKind::SelfValue => "self",
            // A nested projection (`g.q.v`) or a `vec[i]` root — resolved by
            // the shared walk, which handles those and bails on the rest.
            _ => return self.field_chain_place_ptr(expr),
        };
        let slot = self.variables.get(name)?.ptr;
        if self.borrow_vars.ref_params.contains_key(name) {
            let ptr_ty = self.context.ptr_type(AddressSpace::default());
            return self
                .builder
                .build_load(ptr_ty, slot, "mutref.arg.root")
                .ok()
                .map(|v| v.into_pointer_value());
        }
        Some(slot)
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
        let name = match &arg.kind {
            ExprKind::StructLiteral { path, .. } => {
                // An enum struct-variant literal (`E.V { .. }`) forwards through
                // the enum arm, not the struct entry-copy — exclude it.
                if self.enum_name_of_expr(arg).is_some() {
                    return false;
                }
                match path.last() {
                    Some(n) => n.clone(),
                    None => return false,
                }
            }
            // B-2026-07-30-12 — a fn-call arg is entry-copied just the same:
            // `pass(mk())` where `mk() -> Guard`. Only the struct-LITERAL shape
            // was matched here, so the fn-call temp fell through the
            // B-2026-07-08-6 override, registered no cleanup, and its buffer was
            // orphaned exactly as the literal's had been — the same leak, one
            // expression shape over. `enum_name_of_expr`'s Call arm resolves
            // variant ctors, so an enum-variant call is excluded by the
            // `struct_types` test below.
            ExprKind::Call { callee, .. } => {
                let ExprKind::Identifier(fn_name) = &callee.kind else {
                    return false;
                };
                match self.fn_sig.fn_return_type_names.get(fn_name) {
                    Some(n) => n.clone(),
                    None => return false,
                }
            }
            _ => return false,
        };
        self.type_decls.struct_types.contains_key(name.as_str())
            && !self.type_decls.shared_types.contains_key(name.as_str())
            && self.aggregate_param_copy_supported_struct(&name, &mut Vec::new())
            && self
                .type_decls
                .struct_field_type_exprs
                .get(name.as_str())
                .is_some_and(|ftes| {
                    ftes.iter().any(|f| {
                        // B-2026-08-07-12 root A, third site. Same blind spot as the
                        // registrar's struct-literal arm: without the companion, a
                        // struct whose ONLY heap is an `Option`/`Result` field is not
                        // recognized as entry-copied, so the passthrough path never
                        // reaches the registrar at all and the orphaned original
                        // leaks. Kept in lockstep with that gate — they answer the
                        // same question about the same structs.
                        self.type_expr_has_drop_heap(f) || self.option_field_te_has_drop_heap(f)
                    })
                })
    }

    /// B-2026-08-01-14 — the ENUM sibling of
    /// [`Self::arg_is_entry_copied_heap_struct`]: a fresh value-enum ctor
    /// arg (`pass2(E2.B(Res { .. }))`) whose enum carries a heap payload is
    /// ENTRY-COPIED by the callee (the owned-enum-param dcopy region), so
    /// on a return-passthrough the callee returns the COPY and the
    /// ORIGINAL aggregate is orphaned — the caller must free it (memory
    /// only; the bodies belong to the result's consumer). Same premise
    /// break B-2026-07-08-6 fixed for heap structs, one type shape over.
    pub(super) fn arg_is_entry_copied_heap_enum(&self, arg: &Expr) -> bool {
        if !matches!(
            &arg.kind,
            ExprKind::Call { .. } | ExprKind::Path { .. } | ExprKind::StructLiteral { .. }
        ) {
            return false;
        }
        self.enum_name_of_expr(arg).is_some_and(|en| {
            en != "Option"
                && en != "Result"
                && !self.type_decls.shared_types.contains_key(en.as_str())
                && self.enum_has_heap_payload(&en)
        })
    }

    /// B-2026-08-27-44 — the element `TypeExpr`s of a TUPLE-shaped argument,
    /// for the two spellings that hand a whole tuple to an owned param: a tuple
    /// LITERAL (`use2((Bag { .. }, 7))`) and a CALL that RETURNS a tuple
    /// (`use2(mk(..))`). `None` for anything else, which is what keeps this out
    /// of the way of the named-struct and enum arms.
    ///
    /// The call leg reads `fn_return_type_exprs` rather than
    /// `fn_return_type_names` on purpose: a tuple return has no NAME, which is
    /// exactly why the registrar's literal-only arm let `use2(mk(..))` fall
    /// through every arm and register nothing at all.
    pub(super) fn tuple_arg_elem_type_exprs(&self, arg: &Expr) -> Option<Vec<TypeExpr>> {
        match &arg.kind {
            ExprKind::Tuple(elems) => {
                Some(elems.iter().map(|e| self.infer_arg_elem_te(e)).collect())
            }
            ExprKind::Call { callee, .. } => {
                let ExprKind::Identifier(fn_name) = &callee.kind else {
                    return None;
                };
                // Same two-map lookup as `callee_tuple_param_elem_type_exprs`,
                // and for the same measured reason: a GENERIC callee is absent
                // from the `fn_sig` maps, so `mk[T]` resolved to nothing here
                // and `use2(mk(..))` kept leaking after the non-generic
                // spelling was fixed.
                let ret = self.fn_sig.fn_return_type_exprs.get(fn_name).or_else(|| {
                    self.mono_state
                        .generic_fns
                        .get(fn_name)?
                        .return_type
                        .as_ref()
                })?;
                match &ret.kind {
                    TypeKind::Tuple(elems) => Some(elems.clone()),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// The callee's DECLARED tuple-param element types, for
    /// [`Self::arg_is_entry_copied_heap_tuple`]'s fill-in.
    ///
    /// `fn_asts` alone is not enough: a GENERIC callee is not registered there
    /// at all (measured — `passthru[T]` reports `in_fn_asts=false`), so the
    /// fill-in silently missed on exactly the monomorph path this row is about.
    /// `mono_state.generic_fns` carries every generic callee, stdlib included —
    /// the same two-map lookup `mono_call_arg_moves_into_self` already performs.
    fn callee_tuple_param_elem_type_exprs(
        &self,
        callee: &str,
        idx: usize,
    ) -> Option<Vec<TypeExpr>> {
        let f = self
            .fn_sig
            .fn_asts
            .get(callee)
            .or_else(|| self.mono_state.generic_fns.get(callee))?;
        match &f.params.get(idx)?.ty.kind {
            TypeKind::Tuple(elems) => Some(elems.clone()),
            _ => None,
        }
    }

    /// B-2026-08-27-44 — the TUPLE sibling of
    /// [`Self::arg_is_entry_copied_heap_struct`] and
    /// [`Self::arg_is_entry_copied_heap_enum`]: does this tuple-shaped argument
    /// have a type the callee ENTRY-COPIES? True exactly when
    /// `make_tuple_param_callee_owned` (the callee's own gate) would copy — some
    /// element owns drop-bearing heap AND every element is copy-supported — so
    /// caller and callee stay in the lockstep B-2026-07-08-6 requires: the
    /// callee returns its COPY and the caller's original is orphaned, so the
    /// return-passthrough guard must NOT suppress the caller's drop.
    ///
    /// The declared-type FILL-IN is not a refinement, it is what makes the
    /// predicate answer at all. `infer_arg_elem_te` names an element by
    /// resolving an EXPRESSION, and it cannot name an integer literal: the `7`
    /// in `passthru((Bag { .. }, 7))` infers to an empty path, which
    /// `field_copy_supported` rejects. So the `all(..)` conjunct failed on the
    /// SCALAR and the whole predicate read `false`, while the callee — which
    /// sees the declared `(Bag, i64)` — entry-copied. The lockstep this exists
    /// to maintain was being broken by an artifact of inference rather than by
    /// any property of the program. Measured, not reasoned about: the probe
    /// printed `elem Path([""]) drop_heap=false copy_sup=false` for that literal.
    ///
    /// Filling only the UNRESOLVED positions keeps the caller's view
    /// authoritative wherever it has one — it is the view that carries the
    /// concrete instantiation at a generic call site, which the declared
    /// `(Bag[T], i64)` does not.
    ///
    /// Fail-CLOSED either way: when neither view resolves an element it stays an
    /// empty path, which reads as no-drop and not-copyable, so the predicate
    /// declines and the shape degrades to the pre-existing leak rather than to a
    /// caller drop of a buffer the callee took, which would be a double free.
    pub(super) fn arg_is_entry_copied_heap_tuple(
        &self,
        arg: &Expr,
        callee: &str,
        idx: usize,
    ) -> bool {
        let Some(inferred) = self.tuple_arg_elem_type_exprs(arg) else {
            return false;
        };
        let declared = self.callee_tuple_param_elem_type_exprs(callee, idx);
        let is_unresolved = |te: &TypeExpr| match &te.kind {
            TypeKind::Path(p) => p.segments.iter().all(|s| s.is_empty()),
            _ => false,
        };
        let resolved: Vec<TypeExpr> = inferred
            .iter()
            .enumerate()
            .map(|(j, e)| {
                if is_unresolved(e) {
                    declared
                        .as_ref()
                        .and_then(|d| d.get(j).cloned())
                        .unwrap_or_else(|| e.clone())
                } else {
                    e.clone()
                }
            })
            .collect();
        resolved.iter().any(|e| self.type_expr_has_drop_heap(e))
            && resolved
                .iter()
                .all(|e| self.field_copy_supported(e, &mut Vec::new()))
    }

    /// #21 — best-effort `TypeExpr` for a tuple-literal arg ELEMENT, so its
    /// caller-temp gets an enum-aware drop (the LLVM type is enum-blind). A
    /// nested tuple recurses; otherwise infer the element's type NAME
    /// (enum-constructor / value type) and wrap it in a single-segment Path.
    /// An unresolved name yields an empty Path, which `type_expr_has_drop_heap`
    /// treats as no-drop — safe (worst case a missed free degrades to the
    /// pre-existing enum-blind leak, never a double-free).
    /// Name the SCALAR type of an expression `enum_name_of_expr` /
    /// `type_name_of` cannot name — chiefly an ARITHMETIC expression, whose
    /// type is its operands'. B-2026-08-06-23.
    ///
    /// `Box { v: f * 2.0 }.take()` built an `insertvalue { i64 } undef, double`
    /// and failed module verification while the interpreter was correct.
    /// B-2026-08-06-12's receiver-literal recovery reads a field initializer's
    /// type through those two namers and is deliberately FAIL-CLOSED — an
    /// unnameable initializer declines the recovery rather than guessing,
    /// because a wrong instantiation silently lowers a struct at another type's
    /// layout. Neither namer handles an arithmetic expression, so `f * 2.0`
    /// yielded the empty string, the literal fell back to the erased base
    /// layout `{ i64 }`, and the `double` store was invalid. `n + 41` survived
    /// only by accident: the erased default IS i64, so the wrong answer
    /// happened to be the right one.
    ///
    /// THE OPERATOR IS ALREADY LOWERED by the time codegen sees it. `f * 2.0`
    /// is not an `ExprKind::Binary` here — `rewrite_binary` (src/lowering.rs)
    /// has turned it into `Call { callee: Path(["f64", "mul"]), .. }`, and that
    /// path's FIRST segment is the operand type name outright. So the name is
    /// read from the callee rather than recovered by walking operands, which is
    /// both simpler and exact: it is the same `type_name` channel
    /// `compile_assoc_call` dispatches its own narrow-int lowering on.
    ///
    /// A comparison lowers through the same shape but yields `bool`, not the
    /// operand type — naming it `f64` would be a silently wrong instantiation,
    /// exactly what the fail-closed design guards against.
    ///
    /// The `Binary` arm below is NOT dead: `compile_expr` still has one, so a
    /// shape lowering does not rewrite reaches codegen unlowered. It walks the
    /// operands for the same answer.
    ///
    /// Deliberately conservative about LITERALS: a bare integer literal stays
    /// unnamed. Its width is context-dependent and the erased fallback is
    /// already i64, so naming it would swap a correct-by-accident result for a
    /// guess. Returning `None` anywhere keeps the previous behaviour — the
    /// caller declines the recovery and falls back — so nothing that worked
    /// before can regress.
    fn scalar_type_name_of_expr(&self, e: &Expr) -> Option<String> {
        match &e.kind {
            // The LOWERED operator form: `Type.op(lhs, rhs)` for a binary op,
            // `Type.op(operand)` for a unary one (`rewrite_unary` emits `neg` /
            // `not`). Both spellings put the operand type in `segments[0]`.
            ExprKind::Call { callee, args } if args.len() == 1 || args.len() == 2 => {
                let ExprKind::Path { segments, .. } = &callee.kind else {
                    return None;
                };
                if segments.len() != 2 {
                    return None;
                }
                let (ty, op) = (segments[0].as_str(), segments[1].as_str());
                if !Self::is_scalar_type_name(ty) {
                    return None;
                }
                match (args.len(), op) {
                    // A comparison lowers through the same shape but yields
                    // `bool`, not the operand type; so does logical `not`.
                    (2, "eq" | "ne" | "lt" | "le" | "gt" | "ge") => Some("bool".to_string()),
                    (1, "not") if ty == "bool" => Some("bool".to_string()),
                    (
                        2,
                        "add" | "sub" | "mul" | "div" | "rem" | "bitand" | "bitor" | "bitxor"
                        | "shl" | "shr",
                    ) => Some(ty.to_string()),
                    // `neg` on a numeric type, and `not` on an INTEGER (the
                    // bitwise complement, which keeps its width).
                    (1, "neg" | "not") => Some(ty.to_string()),
                    _ => None,
                }
            }
            // Unlowered spellings, for any path that reaches codegen without
            // `rewrite_binary` having run over it.
            ExprKind::Binary { op, left, right } => match op {
                BinOp::Eq
                | BinOp::NotEq
                | BinOp::Lt
                | BinOp::LtEq
                | BinOp::Gt
                | BinOp::GtEq
                | BinOp::And
                | BinOp::Or => Some("bool".to_string()),
                BinOp::Range | BinOp::RangeInclusive => None,
                _ => self
                    .named_scalar_operand(left)
                    .or_else(|| self.named_scalar_operand(right)),
            },
            ExprKind::Unary { op, operand } => match op {
                crate::ast::UnaryOp::Not => Some("bool".to_string()),
                crate::ast::UnaryOp::Neg | crate::ast::UnaryOp::BitNot => {
                    self.named_scalar_operand(operand)
                }
                _ => None,
            },
            ExprKind::Cast { ty, .. } => match &ty.kind {
                TypeKind::Path(p) => p.segments.last().cloned(),
                _ => None,
            },
            ExprKind::Float(_, sfx) => Some(match sfx {
                Some(f) => format!("{f:?}").to_lowercase(),
                None => "f64".to_string(),
            }),
            ExprKind::Bool(_) => Some("bool".to_string()),
            ExprKind::Integer(_, Some(sfx)) => Some(format!("{sfx:?}").to_lowercase()),
            _ => None,
        }
    }

    /// The scalar type names a lowered operator callee may legitimately carry.
    /// Gated so a genuine 2-segment call (`Module.func`, an enum-variant
    /// constructor) is never mistaken for a lowered operator.
    fn is_scalar_type_name(n: &str) -> bool {
        matches!(
            n,
            "i8" | "i16"
                | "i32"
                | "i64"
                | "i128"
                | "isize"
                | "u8"
                | "u16"
                | "u32"
                | "u64"
                | "u128"
                | "usize"
                | "f32"
                | "f64"
                | "bool"
                | "char"
        )
    }

    /// One operand of an unlowered arithmetic expression, named through the
    /// ordinary namers first and then recursively — so a nested `(a + b) * c`
    /// resolves from whichever leaf is nameable.
    fn named_scalar_operand(&self, e: &Expr) -> Option<String> {
        self.type_name_of(e)
            .or_else(|| self.scalar_type_name_of_expr(e))
    }

    pub(super) fn infer_arg_elem_te(&self, e: &Expr) -> TypeExpr {
        if let ExprKind::Tuple(inner) = &e.kind {
            return TypeExpr {
                kind: TypeKind::Tuple(inner.iter().map(|x| self.infer_arg_elem_te(x)).collect()),
                span: e.span,
            };
        }
        let name = self
            .enum_name_of_expr(e)
            .or_else(|| self.type_name_of(e))
            .or_else(|| self.scalar_type_name_of_expr(e))
            .unwrap_or_default();
        TypeExpr {
            kind: TypeKind::Path(crate::ast::PathExpr {
                segments: vec![name],
                generic_args: None,
                span: e.span,
            }),
            span: e.span,
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
        let Some(abi) = self.target_abi.fn_niche_abi.get(callee) else {
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
        if self
            .target_abi
            .fn_niche_abi
            .get(callee)
            .is_some_and(|abi| abi.ret)
        {
            return self.niche_ptr_to_option_value(v.into_pointer_value(), "call.niche");
        }
        v
    }

    /// Lower a diverging prelude builtin (`todo()` / `unreachable()` /
    /// `panic()`, type `!`). Prints a panic message and `exit(101)` via
    /// `emit_panic`, then terminates the current block with an `unreachable`
    /// instruction so the caller's terminator-guarded paths (`compile_block`
    /// between statements, `if`/`match` branch merges, and the function-tail
    /// `ret` in `compile_function`) all skip emitting a follow-on instruction.
    /// This is what fixes `fn boom() -> T { unreachable() }`: without the
    /// terminator, the tail logic emitted `ret i64 0` (the placeholder this
    /// used to return) against `T`'s real LLVM type, failing module
    /// verification.
    ///
    /// Message parity with the interpreter's `eval_builtin_diverge`: default
    /// `"not yet implemented"` (todo) / `"entered unreachable code"`
    /// (unreachable) / `"explicit panic"` (panic). `todo`/`unreachable` fold a
    /// literal argument in as `"<default>: <msg>"`, while `panic("msg")` uses
    /// the user message *directly* (no prefix) — `panic` is the explicit
    /// user-facing form, so its argument replaces rather than annotates the
    /// default. `emit_panic` takes a compile-time `&str`, so a non-literal
    /// (runtime-valued) argument — rare for these builtins — degrades to the
    /// bare default message rather than threading a runtime string through the
    /// panic printf.
    fn compile_diverge(
        &mut self,
        name: &str,
        args: &[CallArg],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let default_msg = match name {
            "todo" => "not yet implemented",
            "panic" => "explicit panic",
            _ => "entered unreachable code",
        };
        let full_msg = match args.first().map(|a| &a.value.kind) {
            // `panic("msg")` surfaces the user message verbatim; the other two
            // annotate their default with it.
            Some(ExprKind::StringLit(s)) if name == "panic" => s.clone(),
            Some(ExprKind::StringLit(s)) => format!("{}: {}", default_msg, s),
            _ => default_msg.to_string(),
        };
        self.emit_panic(&full_msg);
        self.builder.build_unreachable().unwrap();
        // Placeholder value: the block is now terminated, so every value-
        // consuming caller respects the terminator guard and never reads it.
        Ok(self.context.i64_type().const_int(0, false).into())
    }

    /// `volatile_read(src)` — a volatile load of the pointee through the raw
    /// pointer `src`. Mirrors the `p.read_volatile()` method form
    /// (`compile_pointer_instance_method`): the pointee width comes from the
    /// `raw_pointer_pointee_types` entry recorded for the pointer argument's
    /// span (lowering surfaces one for every `*const T` / `*mut T`-typed
    /// expression), and the load carries the LLVM `volatile` flag so the
    /// optimizer neither elides, reorders, nor duplicates the access.
    fn compile_volatile_read(&mut self, src: &Expr) -> Result<BasicValueEnum<'ctx>, String> {
        let key = (src.span.offset, src.span.length);
        let pointee_te = self
            .span_tables
            .raw_pointer_pointee_types
            .get(&key)
            .cloned()
            .ok_or_else(|| {
                "volatile_read: could not resolve the pointee type of the pointer \
                 argument at codegen (no raw-pointer pointee recorded for it)"
                    .to_string()
            })?;
        let ptr_val = self.compile_expr(src)?.into_pointer_value();
        let pointee_ty = self.llvm_type_for_type_expr(&pointee_te);
        let loaded = self
            .builder
            .build_load(pointee_ty, ptr_val, "volatile.read")
            .map_err(|e| format!("volatile_read: {e:?}"))?;
        loaded
            .as_instruction_value()
            .expect("build_load yields an instruction value")
            .set_volatile(true)
            .map_err(|e| format!("volatile_read set_volatile: {e:?}"))?;
        Ok(loaded)
    }

    /// `volatile_write(dst, value)` — a volatile store of `value` through the
    /// raw pointer `dst`. Peer of `compile_volatile_read`: the store width comes
    /// from `dst`'s recorded POINTEE type, and the value is coerced to it before
    /// the store — NOT from the value's own LLVM type. Using the value's type
    /// was a silent miscompile (B-2026-07-12-7): an integer literal defaults to
    /// `i64` in codegen, so `volatile_write(pw /* *mut i32 */, 777)` emitted
    /// `store volatile i64 777` through an `i32*`. That 8-byte store both wrote
    /// 4 bytes past the field AND — because its width didn't match the paired
    /// `load volatile i32` — defeated the optimizer's volatile load-forwarding
    /// under `-O` (AOT), so a same-function read-back value-numbered back to the
    /// pre-write value (`karac build` printed the stale `20`, `karac run`/JIT at
    /// `-O0` happened to print the correct `777`). Matching the store width to
    /// the pointee makes the two accesses the same shape and the AOT round-trip
    /// correct. Returns the shared `i64 0` unit placeholder (call type is unit).
    fn compile_volatile_write(
        &mut self,
        dst: &Expr,
        value: &Expr,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let ptr_val = self.compile_expr(dst)?.into_pointer_value();
        let v = self.compile_expr(value)?;
        // Coerce the value to `dst`'s pointee type so the store width matches
        // the memory it targets (and the paired volatile read). The pointee is
        // recorded per `*const T` / `*mut T`-typed expression by lowering, keyed
        // on the expression's span — the same table `compile_volatile_read`
        // consults. Missing entry ⇒ fall back to the value's own type (a
        // best-effort store rather than a hard error).
        let key = (dst.span.offset, dst.span.length);
        let v = if let Some(pointee_te) = self
            .span_tables
            .raw_pointer_pointee_types
            .get(&key)
            .cloned()
        {
            let pointee_ty = self.llvm_type_for_type_expr(&pointee_te);
            self.coerce_scalar_to_type(v, pointee_ty)
        } else {
            v
        };
        let store = self
            .builder
            .build_store(ptr_val, v)
            .map_err(|e| format!("volatile_write: {e:?}"))?;
        store
            .set_volatile(true)
            .map_err(|e| format!("volatile_write set_volatile: {e:?}"))?;
        Ok(self.context.i64_type().const_int(0, false).into())
    }

    /// `fence(order)` / `compiler_fence(order)` — standalone memory barriers
    /// (`runtime/stdlib/intrinsics.kara`). Lower to an LLVM `fence`: `fence`
    /// is a cross-thread barrier (`single_thread == false`), `compiler_fence`
    /// uses the singlethread syncscope (`single_thread == true`), which
    /// restrains the optimizer without emitting a CPU barrier. `order` must be
    /// a compile-time `MemoryOrdering` literal — an LLVM `fence` carries its
    /// ordering as a static instruction attribute, so a runtime value cannot
    /// be lowered — and must not be `Relaxed`: LLVM forbids `fence monotonic`
    /// (a relaxed fence orders nothing). Returns unit (the shared `i64 0`
    /// void-builtin placeholder).
    fn compile_atomic_fence(
        &mut self,
        order: &Expr,
        single_thread: bool,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let ordering = self.parse_memory_ordering(order)?;
        if matches!(
            ordering,
            inkwell::AtomicOrdering::Monotonic | inkwell::AtomicOrdering::Unordered
        ) {
            return Err(
                "codegen: fence ordering must be Acquire / Release / AcqRel / SeqCst \
                 (a Relaxed fence orders nothing and is rejected by LLVM)"
                    .into(),
            );
        }
        // A `fence` is a void-typed instruction, so it must NOT carry a name
        // (LLVM: "Instruction has a name, but provides a void value"). Pass an
        // empty name — the `single_thread` flag selects the syncscope.
        self.builder
            .build_fence(ordering, single_thread, "")
            .map_err(|e| format!("codegen: build_fence failed: {e:?}"))?;
        Ok(self.context.i64_type().const_int(0, false).into())
    }

    /// `reg.read()` / `reg.write(v)` on a `VolatileCell[T]` binding — the
    /// transparent MMIO wrapper (`runtime/stdlib/volatile_cell.kara`). Like
    /// `Atomic[T]`, `VolatileCell[T]` lowers to the bare inner `T` (see the arm
    /// in `llvm_type_for_type_expr`), so the binding's alloca IS the register's
    /// storage: `.read()` is a volatile load of that alloca, `.write(v)` a
    /// volatile store into it — the same `volatile` flag the `volatile_read` /
    /// `volatile_write` intrinsics emit, with the field-address plumbing
    /// collapsed away by the transparent layout. `.write` coerces the value to
    /// the slot width (an `i64` literal into an `i32` register, etc.). Restricted
    /// to an identifier receiver (an owned/`ref` `VolatileCell` binding), the
    /// shape `var_type_names` tags; other receiver shapes fall through.
    pub(super) fn compile_volatile_cell_method(
        &mut self,
        recv_name: &str,
        method: &str,
        args: &[CallArg],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let storage_ptr = self.get_data_ptr(recv_name).ok_or_else(|| {
            format!("codegen: VolatileCell receiver '{recv_name}' has no storage slot")
        })?;
        let ty = self.variables.get(recv_name).map(|s| s.ty).ok_or_else(|| {
            format!("codegen: VolatileCell receiver '{recv_name}' has no slot type")
        })?;
        match method {
            "read" => {
                let loaded = self
                    .builder
                    .build_load(ty, storage_ptr, "volcell.read")
                    .map_err(|e| format!("VolatileCell.read: {e:?}"))?;
                loaded
                    .as_instruction_value()
                    .expect("build_load yields an instruction value")
                    .set_volatile(true)
                    .map_err(|e| format!("VolatileCell.read set_volatile: {e:?}"))?;
                Ok(loaded)
            }
            "write" => {
                let v = self.compile_expr(&args[0].value)?;
                let v = self.coerce_scalar_to_type(v, ty);
                let store = self
                    .builder
                    .build_store(storage_ptr, v)
                    .map_err(|e| format!("VolatileCell.write: {e:?}"))?;
                store
                    .set_volatile(true)
                    .map_err(|e| format!("VolatileCell.write set_volatile: {e:?}"))?;
                Ok(self.context.i64_type().const_int(0, false).into())
            }
            other => Err(format!("codegen: unknown VolatileCell method '{other}'")),
        }
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
        let n = self.conc.hot_swap_fns.len() as u32;
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
                    .type_decls
                    .enum_layouts
                    .get(en)
                    .is_some_and(|l| l.tags.contains_key(name)) =>
            {
                Some(en.to_string())
            }
            _ => {
                let mut user_match: Option<String> = None;
                let mut seed_match: Option<String> = None;
                for (en, layout) in &self.type_decls.enum_layouts {
                    if layout.tags.contains_key(name) {
                        if self.type_decls.seeded_enum_names.contains(en) {
                            seed_match.get_or_insert_with(|| en.clone());
                        } else {
                            user_match.get_or_insert_with(|| en.clone());
                        }
                    }
                }
                // B-2026-08-14-10 — seeded wins for `Option`/`Result`'s four
                // constructors only; see `seeded_variant_owner`. Every other
                // colliding name keeps the user-first preference described
                // above, which the `MyIoErr.Other` case still needs.
                if Self::seeded_variant_owner(name).is_some() && seed_match.is_some() {
                    seed_match
                } else {
                    user_match.or(seed_match)
                }
            }
        };

        let enum_name = match enum_name {
            Some(n) => n,
            None => return Ok(None),
        };

        let (tag, llvm_type) = {
            let layout = &self.type_decls.enum_layouts[&enum_name];
            (*layout.tags.get(name).unwrap(), layout.llvm_type)
        };

        let i64_t = self.context.i64_type();

        // Shared enum: heap-allocate with refcount header.
        if let Some(info) = self.type_decls.shared_types.get(&enum_name).cloned() {
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
            let offsets: Vec<(usize, usize)> = self.type_decls.enum_layouts[&enum_name]
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
                // B-2026-07-16-5: borrow-sourced payload — zero the cap word
                // so the stored triple is a view (see the non-shared arm).
                let val = self.zero_cap_if_ref_heap_borrow(&arg.value, val);
                // #226 (B-2026-06-15): a `Variant(nodes[i])` payload reading a
                // bare-`shared` Vec element is aliased, not moved — inc so the
                // new enum owns its own ref (else freed when the Vec drops).
                self.share_bare_shared_ctor_payload(&arg.value, val);
                // Shared-enum twin of the coercion in the non-shared arm below
                // (B-2026-08-13-18) — same packer, same silent failure mode.
                let val = self.coerce_enum_payload_scalar(&enum_name, name, i, val, &arg.value);
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
                    // B-2026-07-18-29: a struct binding that owns a `shared` /
                    // `Vec[shared]` field, moved whole into this shared-enum
                    // variant, hands its inline shared children to the new box.
                    // The cap-zeroing above suppresses only its buffer half; its
                    // combined StructDrop still rc-DECs the shared children,
                    // double-decing against the box's own rc-drop. Retract it.
                    if let Some(tn) = self.var_types.var_type_names.get(&n).cloned() {
                        if self.struct_owns_shared_field(&tn, &mut Vec::new()) {
                            self.suppress_struct_cleanup_for_tail_identifier(&n);
                        }
                    }
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
        let offsets: Vec<(usize, usize)> = self.type_decls.enum_layouts[&enum_name]
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
            // B-2026-07-16-5: a payload sourced from a BORROW — `Some(s)`
            // with `s: ref String` (the `Option[ref String]` adversarial-
            // accept shape) — packs the LENDER's `{ptr,len,cap}` triple
            // into the payload words, so the match-arm binding's
            // cap-guarded cleanup freed the lender's buffer and the
            // lender's own scope-exit free doubled it. Zero the cap word:
            // the payload is a read-only view; the lender stays sole owner.
            let val = self.zero_cap_if_ref_heap_borrow(&arg.value, val);
            // #226 (B-2026-06-15): `Some(nodes[i])` — a bare-`shared` Vec
            // element read is aliased, not moved; inc so the Option owns its
            // own ref (else freed when the source Vec drops).
            self.share_bare_shared_ctor_payload(&arg.value, val);
            // Declared-payload class/width coercion before the bit-level pack
            // (B-2026-08-13-18). See `coerce_enum_payload_scalar`: the packer
            // reinterprets rather than converts, so an int reaching an `f64`
            // payload is a silent wrong value, not a verifier error.
            let val = self.coerce_enum_payload_scalar(&enum_name, name, i, val, &arg.value);
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
            // B-2026-07-30-11 (Option/Result leg) — a bare-identifier payload
            // arg (`Ok(h)`, `Slot.Held(r)`) moves the WHOLE binding into the
            // variant, so its user-Drop body belongs to the enum's eventual
            // owner. Retract every `UserDrop` for the source (own-body
            // wrapper and container-bodies walks alike) — leaving them armed
            // ran the body a second time over the moved-from slot. Interp
            // twin: the ctor arm of `record_ctor_arg_moves` inserting into
            // `moved_out_user_drop_bindings`.
            if let ExprKind::Identifier(n) = &arg.value.kind {
                let n = n.clone();
                self.suppress_user_drop_for_var(&n);
            }
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
        let layout = self.type_decls.enum_layouts.get(enum_name).ok_or_else(|| {
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
            let layout = &self.type_decls.enum_layouts[enum_name];
            (*layout.tags.get(variant).unwrap(), layout.llvm_type)
        };
        let offsets: Vec<(usize, usize)> = self.type_decls.enum_layouts[enum_name]
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
        if let Some(info) = self.type_decls.shared_types.get(enum_name).cloned() {
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
            // GPU-SLIP-4b-5 — a device buffer RETURNED out of the scope that
            // bound it. Its `let` queued a scope-exit free that would run while
            // the caller still holds the handle, which is a use-after-free the
            // runtime catches as "a field reduction on an already-freed device
            // buffer" rather than a leak. Only the ESCAPING positions zero: a
            // by-value call argument does NOT take ownership of a buffer,
            // because there is no deep copy to hand the callee, so the caller
            // stays the owner and its free must survive.
            self.gpu_zero_moved_buffer_handle(expr);
            self.suppress_source_vec_cleanup_for_arg(expr);
            // B-2026-08-22-18 follow-up — tail `a[0]` moving a constant-index
            // element out of an owned `Array[T, N]` (the explicit-`return`
            // sibling is in `exprs.rs`). Cap-zero the element in the array's slot
            // so its scope-exit drop skips the moved-out buffer.
            self.suppress_array_elem_move_source(expr);
            // B-2026-08-28-15 — the TUPLE-INDEX peer of the array-element line
            // above: a tail `p.0` moving a heap-carrying element out of an
            // owned tuple. Both `let`-statement positions already call this
            // (`stmts.rs`), but no ESCAPING position did, so the element left
            // the frame while the tuple's own scope-exit drop still freed it —
            // `free(): double free detected in tcache 2` on a program the
            // interpreter runs correctly. The explicit-`return` twin is in
            // `exprs.rs`, the aggregate-literal twin in the struct-literal
            // field loop; all three mirror the array peer's hook set.
            self.suppress_tuple_index_move_source(expr);
            // B-2026-08-28-15 — the DEEPER-PLACE peer: a tail `p.0.name`
            // moving a heap field out of a struct reached THROUGH a tuple
            // element. Hooked at the same four `let` positions and nowhere
            // else, so it had the identical escaping-position hole.
            self.suppress_place_field_struct_move_source(expr);
            // B-2026-08-24-5 — the WHOLE owned `Array[T, N]` returned out
            // (`fn mk() -> Array[String, 2] { ...; return a; }`). The element
            // sibling above covers `return a[0]`; this covers `return a`. The
            // retraction already existed for the by-value CALL-ARGUMENT
            // position (`move_declined_copy_struct_arg`) but was never hooked
            // at either return position, so the array's scope-exit
            // `StructDrop` fired here AND the caller freed the same buffers —
            // a double free.
            //
            // Invisible at the default -O2, which is why it survived: the
            // fixture's allocation is provably dead there and LLVM deletes it
            // along with the evidence. Only the `KARAC_OPT_LEVEL=0` ASAN leg
            // runs the allocation the program actually writes.
            self.suppress_array_binding_move_arg(expr);
            // B-2026-07-22-2: `fn get() -> String { mk().s }` tail — the
            // caller owns the extracted field; zero it in the staged
            // fresh-temp slot so the frame drain frees only the remainder.
            self.consume_freshtemp_field_move(expr);
            // B-2026-08-02-23 leg 2 — an AGGREGATE-LITERAL tail
            // (`fn mk(v: Vec[Res]) -> Holder { Holder { xs: v, tag: 9 } }`):
            // every binding named inside it is moved into the returned value,
            // so its Drop-body action must retract exactly as it does at the
            // let-RHS and consuming-arg positions. Only the bare-Identifier
            // tail was handled below, so a source consumed by the literal kept
            // its walk armed and fired at THIS frame's exit — then a second
            // time when the caller's binding died. Same recursive source set
            // and same strong disarm as the sibling positions, so the three
            // move channels stay in agreement.
            //
            // Bodies only: memory for the returned aggregate is already
            // transferred by `suppress_source_vec_cleanup_for_arg` above, and
            // `suppress_user_drop_for_var` frees nothing. Static and
            // flow-insensitive like every sibling — a conditional return
            // disarms on all paths, which can only under-fire.
            if matches!(
                &expr.kind,
                ExprKind::StructLiteral { .. } | ExprKind::Tuple(_)
            ) {
                let mut sources = Vec::new();
                Self::collect_aggregate_literal_sources(expr, &mut sources);
                for n in sources {
                    self.suppress_user_drop_for_var(&n);
                }
            }
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
                // B-2026-08-06-32 — a returned binding's NESTED box escapes
                // into the caller's value; freeing it here is a UAF.
                self.suppress_nested_boxed_drop_for_var(name);
                // B-2026-08-07-1 — a returned binding whose payload is heap
                // BOXED. The caller receives an enum whose word 0 still points
                // at this frame's box, so the scope-exit `BoxedEnumDrop` frees
                // memory the caller then reads and frees again — a double free
                // plus two invalid reads, at BOTH opt levels. This action was
                // never added to the tail-return retraction list the Vec /
                // String / Map / channel / user-Drop suppressions around it
                // form; it post-dates most of them.
                //
                // Disarmed by ZEROING word 0 at runtime (the box drop's
                // null-guard then skips), not by retracting the queued action.
                // The distinction is load-bearing: a binding can be returned on
                // one path and CONSUMED on another, and a compile-time retract
                // is flow-insensitive, so it would strand the box on the
                // consuming path — trading this double free for a leak. The
                // store executes only on the path that actually returns.
                //
                // Ordered AFTER the return value is loaded, per B-2026-06-12-6:
                // zeroing first corrupts the value the caller receives.
                self.suppress_boxed_enum_payload_cleanup_for_owner(name);
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
        if !self.closure_state.heap_env_closure_vars.contains(name) {
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
        let Some(field_names) = self.type_decls.struct_field_names.get(struct_name).cloned() else {
            return;
        };
        let Some(st) = self.type_decls.struct_types.get(struct_name).copied() else {
            return;
        };
        for f in fields {
            let is_fresh = self.is_heap_env_producing_call(&f.value);
            let is_binding = matches!(
                &f.value.kind,
                ExprKind::Identifier(src) if self.closure_state.heap_env_closure_vars.contains(src)
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
            if let Some(frame) = self.drop_rc.scope_cleanup_actions.last_mut() {
                frame.push(super::state::CleanupAction::FreeClosureEnv {
                    fat_alloca: field_gep,
                });
            }
            // Record the owned field so `neutralize_moved_aggregate_env_slots` can
            // null this env slot if `var_name` is later moved out via a return
            // (aggregate-escape slice).
            self.closure_state
                .heap_env_owner_fields
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
            .closure_state
            .escape
            .fns_returning_heap_env_aggregate
            .get(&callee_name)
            .cloned()
        else {
            return;
        };
        let Some(field_names) = self.type_decls.struct_field_names.get(struct_name).cloned() else {
            return;
        };
        let Some(st) = self.type_decls.struct_types.get(struct_name).copied() else {
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
            if let Some(frame) = self.drop_rc.scope_cleanup_actions.last_mut() {
                frame.push(super::state::CleanupAction::FreeClosureEnv {
                    fat_alloca: field_gep,
                });
            }
            self.closure_state
                .heap_env_owner_fields
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
        let Some(fields) = self.closure_state.heap_env_owner_fields.get(src).cloned() else {
            return;
        };
        for (struct_name, idx) in &fields {
            let Some(st) = self.type_decls.struct_types.get(struct_name).copied() else {
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
            if let Some(frame) = self.drop_rc.scope_cleanup_actions.last_mut() {
                frame.push(super::state::CleanupAction::FreeClosureEnv {
                    fat_alloca: field_gep,
                });
            }
        }
        self.closure_state
            .heap_env_owner_fields
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
        if let Some(idxs) = self
            .closure_state
            .escape
            .heap_env_tuple_owners
            .get(src)
            .cloned()
        {
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
                if let Some(frame) = self.drop_rc.scope_cleanup_actions.last_mut() {
                    frame.push(super::state::CleanupAction::FreeClosureEnv {
                        fat_alloca: elem_gep,
                    });
                }
            }
        } else if let Some(idxs) = self
            .closure_state
            .escape
            .heap_env_array_owners
            .get(src)
            .cloned()
        {
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
                if let Some(frame) = self.drop_rc.scope_cleanup_actions.last_mut() {
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
                ExprKind::Identifier(src) if self.closure_state.heap_env_closure_vars.contains(src)
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
            if let Some(frame) = self.drop_rc.scope_cleanup_actions.last_mut() {
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
                ExprKind::Identifier(src) if self.closure_state.heap_env_closure_vars.contains(src)
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
            if let Some(frame) = self.drop_rc.scope_cleanup_actions.last_mut() {
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
        let Some(fields) = self.closure_state.heap_env_owner_fields.get(name).cloned() else {
            return;
        };
        let Some(slot_ptr) = self.variables.get(name).map(|s| s.ptr) else {
            return;
        };
        let fat_ty = self.closure_value_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        for (struct_name, idx) in fields {
            let Some(st) = self.type_decls.struct_types.get(&struct_name).copied() else {
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
        if let Some(idxs) = self
            .closure_state
            .escape
            .heap_env_tuple_owners
            .get(name)
            .cloned()
        {
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
        } else if let Some(idxs) = self
            .closure_state
            .escape
            .heap_env_array_owners
            .get(name)
            .cloned()
        {
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
        if let Some(idxs) = self
            .closure_state
            .escape
            .fns_returning_heap_env_tuple
            .get(&callee_name)
            .cloned()
        {
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
                if let Some(frame) = self.drop_rc.scope_cleanup_actions.last_mut() {
                    frame.push(super::state::CleanupAction::FreeClosureEnv {
                        fat_alloca: elem_gep,
                    });
                }
            }
        } else if let Some(idxs) = self
            .closure_state
            .escape
            .fns_returning_heap_env_array
            .get(&callee_name)
            .cloned()
        {
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
                if let Some(frame) = self.drop_rc.scope_cleanup_actions.last_mut() {
                    frame.push(super::state::CleanupAction::FreeClosureEnv {
                        fat_alloca: elem_gep,
                    });
                }
            }
        }
    }

    pub(super) fn suppress_source_vec_cleanup_for_arg(&mut self, arg_expr: &Expr) {
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
    /// Struct sibling of [`Self::suppress_map_cleanup_for_tail_identifier`]
    /// (B-2026-07-18-29): a tracked struct binding moved WHOLE into an
    /// enum-variant payload (`MethodCall(mc)` — a shared-enum tuple variant
    /// re-wrapping a match-bound-then-owned struct) hands ALL of its heap to the
    /// new enum box. Its scope-exit `StructDrop` — which for a struct owning a
    /// `shared`/`Vec[shared]` field runs the COMBINED value-drop + shared-field
    /// rc-DEC — must therefore be retracted wholesale: the value-drop would
    /// re-free the String/Vec buffers the box now owns AND the shared-field
    /// rc-DEC would double-dec the box's inline shared children. Zeroing the
    /// source's caps (`suppress_source_vec_cleanup_for_arg`) neutralizes only the
    /// buffer half, not the rc-DEC, so a full retraction is required. Removes the
    /// binding's `StructDrop` from whichever frame holds it (a move site can fire
    /// with a transient inner arg/method-eval frame on top — same rationale as
    /// the Map sibling).
    /// B-2026-07-28-4 (cross_graph): a by-value struct argument whose parameter
    /// DECLINED the callee entry-copy must be a true MOVE — the caller gives up
    /// its drop.
    ///
    /// Normally a by-value aggregate param is "callee-owned": the callee
    /// deep-copies on entry, so caller and callee hold disjoint buffers and each
    /// legitimately frees its own. When `aggregate_param_copy_supported_struct`
    /// declines (B-2026-07-28-3 makes it decline for a self-referential
    /// `struct N { edges: Vec[N] }`, because the per-element copy is unrolled at
    /// emission and has no finite form), the param falls back to caller-retains
    /// and the callee receives an ALIAS. That is safe only while the callee just
    /// reads it. The moment the callee stores it into an owning container —
    /// `self.edges.push(t)`, the entire point of an adjacency list — the
    /// container and the caller's binding both own the same buffers, and both
    /// free them.
    ///
    /// So the decline swapped a compiler stack overflow for a runtime
    /// double-free. Suppressing the caller's drop restores the language-level
    /// semantics (a bare `T` param is owned, so the argument is moved in) for
    /// exactly the shape that cannot be copied. Narrow by construction: it fires
    /// only for an Identifier argument of a non-shared user struct that the
    /// copy-support analysis rejects — every copy-supported struct keeps the
    /// existing entry-copy behaviour untouched.
    pub(super) fn move_declined_copy_struct_arg(&mut self, arg: &Expr) {
        // B-2026-08-22-18 follow-up — an owned `Array[T, N]` binding/param passed
        // BY VALUE into a callee transfers ownership (the callee frees it,
        // `make_array_param_callee_owned`), so retract the caller's own array
        // element drop here. Without this, `fn g(a: Array[String,2]) { h(a) }`
        // frees the shared buffers in both frames — a double free. Hooked at this
        // shared by-value-owned-arg choke point so every call-arg site is
        // covered. No-op for a temporary or non-owning root.
        self.suppress_array_binding_move_arg(arg);
        let ExprKind::Identifier(var) = &arg.kind else {
            return;
        };
        let Some(type_name) = self.var_types.var_type_names.get(var.as_str()).cloned() else {
            return;
        };
        if !self
            .type_decls
            .struct_types
            .contains_key(type_name.as_str())
            || self
                .type_decls
                .shared_types
                .contains_key(type_name.as_str())
        {
            return;
        }
        if self.aggregate_param_copy_supported_struct(&type_name, &mut Vec::new()) {
            return;
        }
        // B-2026-08-05-32 — a struct with a DIRECT `shared` field must KEEP its
        // drop. Copy-support declines for two unrelated reasons, and the move
        // semantics above only follow from one of them:
        //
        //   * a self-referential `struct N { edges: Vec[N] }` (B-2026-07-28-3)
        //     declines because the per-element copy has no finite emission. The
        //     callee then receives an ALIAS it may STORE into an owning
        //     container, so the caller must give up its drop or both free the
        //     same buffers. That is the case this retraction was written for.
        //
        //   * a direct `shared` field declines because `field_copy_supported`
        //     bails on it. Nothing is stored and nothing is aliased: the callee
        //     is caller-retains and therefore never entry-copies, so it never
        //     rc-INCs and never rc-DECs. The binding's drop is the box's ONLY
        //     rc-dec, and retracting it stranded the box — one leaked RC box per
        //     call (`let d = DirH { value: Val.Ident(..) }; f(d);`).
        //
        // Keeping the drop cannot double-dec here precisely because the callee
        // is caller-retains: there is no second owner to balance against. The
        // fresh-TEMP form of this shape already reasons the same way and
        // registers a drop for exactly this reason (B-2026-07-04-9(b)'s
        // `src_shared_owning`); the comment there claims the let-bound sibling
        // "is already covered by `track_struct_var` at its binding site", which
        // was true only until this retraction removed it.
        if self.struct_owns_shared_field(&type_name, &mut Vec::new()) {
            return;
        }
        let var = var.clone();
        self.suppress_struct_cleanup_for_tail_identifier(&var);
    }

    /// Zero the handle word of a `GpuBuffer` binding that has just been MOVED,
    /// so the origin's free goes inert and the destination is sole owner.
    /// Returns whether `expr` named such a binding.
    ///
    /// Zeroing rather than retracting the queued `FreeGpuBuffer`, for reasons
    /// that all bear on this type. It is FLOW-SENSITIVE — the store lands on
    /// the path the move actually took, where a static retraction disarms every
    /// path and can only under-fire, which for a device allocation is a leak
    /// LeakSanitizer cannot see. It composes with the struct-field drop, which
    /// lives inside `__karac_drop_struct_<S>` and is not in any scope's action
    /// list to retract. And `karac_runtime_gpu_free_soa(0)` is already inert —
    /// that is precisely why the scope-exit drain needs no live-guard — so a
    /// zeroed slot is a shape every existing free path already handles rather
    /// than a new one to teach them.
    ///
    /// Every caller orders this load-then-suppress, and that ordering is not
    /// incidental: zeroing before the value is materialized would hand the
    /// consumer the null handle instead of the buffer. B-2026-06-12-6 is the
    /// same mistake made against `Vec`'s `cap`, and the tail-return path
    /// documents the order it fixed on (compile body, load result, suppress).
    ///
    /// Gated on `gpu_buffer_vars` and not on the LLVM type alone: `{i64, i64}`
    /// is structurally identical to any two-field all-`i64` user struct
    /// (B-2026-07-18-7), and zeroing a field of one of those would be a silent
    /// wrong answer rather than a leak.
    pub(super) fn gpu_zero_moved_buffer_handle(&mut self, expr: &Expr) -> bool {
        let ExprKind::Identifier(name) = &expr.kind else {
            return false;
        };
        // Same guard `suppress_source_vec_cleanup_for_arg_ex` opens with: at a
        // move the ownership pass flagged as REUSED, the source must keep its
        // handle. There is no defensive copy to fall back on for a device
        // buffer — a handle copy aliases the same allocation — so zeroing here
        // would hand the later read a null handle instead of merely costing a
        // redundant free.
        if self
            .span_tables
            .uam_copied_sites
            .contains(&(expr.span.offset, expr.span.length))
        {
            return false;
        }
        if !self.accel.gpu_buffer_vars.contains(name.as_str()) {
            return false;
        }
        let buf_ty = self.gpu_buffer_type();
        let Some(slot) = self.variables.get(name.as_str()).copied() else {
            return false;
        };
        if slot.ty != buf_ty.into() {
            return false;
        }
        if let Ok(handle_ptr) =
            self.builder
                .build_struct_gep(buf_ty, slot.ptr, 0, "gpu.moved.handle.p")
        {
            let _ = self
                .builder
                .build_store(handle_ptr, self.context.i64_type().const_zero());
        }
        true
    }

    pub(super) fn suppress_struct_cleanup_for_tail_identifier(&mut self, name: &str) {
        let slot_ptr = match self.variables.get(name) {
            Some(s) => s.ptr,
            None => return,
        };
        for frame in self.drop_rc.scope_cleanup_actions.iter_mut() {
            frame.retain(|action| {
                !matches!(
                    action,
                    crate::codegen::state::CleanupAction::StructDrop { struct_alloca, .. }
                        if *struct_alloca == slot_ptr
                )
            });
        }
    }

    /// B-2026-08-06-32 — drop the `NestedBoxedEnumDrop` queued for `name` when
    /// that binding is the function's tail/return value.
    ///
    /// The box ESCAPES the frame: `fn mk() -> Result[Option[Wide], E] { let b =
    /// …; b }` hands the caller a value whose inline payload still points at
    /// this frame's box. Freeing it at scope exit is a use-after-free in the
    /// caller — measured as an `Invalid read of size 8` and a wrong answer,
    /// which is strictly worse than the leak this action exists to fix, so the
    /// registration is retracted rather than the return being reshaped.
    ///
    /// Mirrors `suppress_map_cleanup_for_tail_identifier`'s all-frames scan for
    /// the same reason it gives: the owning frame is not necessarily the
    /// innermost one at the moment the tail is walked.
    pub(super) fn suppress_nested_boxed_drop_for_var(&mut self, name: &str) {
        let slot_ptr = match self.variables.get(name) {
            Some(s) => s.ptr,
            None => return,
        };
        for frame in self.drop_rc.scope_cleanup_actions.iter_mut() {
            frame.retain(|action| match action {
                crate::codegen::state::CleanupAction::NestedBoxedEnumDrop { enum_slot, .. } => {
                    *enum_slot != slot_ptr
                }
                _ => true,
            });
        }
        self.payload_vars.nested_boxed_payload_vars.remove(name);
    }

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
        for frame in self.drop_rc.scope_cleanup_actions.iter_mut() {
            frame.retain(|action| match action {
                crate::codegen::state::CleanupAction::FreeMapHandle { map_alloca, .. } => {
                    *map_alloca != slot_ptr
                }
                // B-2026-08-09-17: `FreeFileHandle` is queue-driven in exactly
                // the same way and had NO suppression at all, so a `File` moved
                // out of its origin binding was still closed at that binding's
                // scope exit — `karac_runtime_file_close` reconstructs the Box
                // and drops it, leaving whatever now owns the handle (a
                // `Vec[File]`, a struct field, a returned aggregate) pointing at
                // freed memory. The next method call on it locks a
                // `Mutex<std::fs::File>` inside that freed allocation, and a
                // poisoned-or-garbage lock word does not fault, it BLOCKS: the
                // program hangs with no diagnostic, while `--interp` runs it
                // correctly. Retracting the action here is the same reasoning as
                // the Map arm above — the binding has been moved, so its origin
                // must never free the handle, whichever frame holds the action.
                crate::codegen::state::CleanupAction::FreeFileHandle { file_alloca } => {
                    *file_alloca != slot_ptr
                }
                _ => true,
            });
        }
    }

    /// Eagerly free the OLD handle of a `Map`/`Set` VARIABLE before a
    /// reassignment (`m = m2`) overwrites its slot (B-2026-07-15-25). The
    /// var's queued `FreeMapHandle` carries the exact per-entry drop params
    /// (key/val Vec flags, shared-half heap types, per-value drop fn); reuse
    /// them via `emit_free_one_map_handle` on the currently-stored handle. The
    /// var's own `FreeMapHandle` is LEFT in place — it frees the NEW handle at
    /// scope exit — and the moved source's handle is suppressed separately, so
    /// the shared handle is freed exactly once. A no-op when the var has no
    /// queued map cleanup. `karac_map_free_*` is null-safe, so an empty/un-init
    /// handle is harmless.
    pub(super) fn eager_free_old_map_var_handle(&mut self, name: &str) {
        let slot_ptr = match self.variables.get(name) {
            Some(s) => s.ptr,
            None => return,
        };
        let drop = self
            .drop_rc
            .scope_cleanup_actions
            .iter()
            .flatten()
            .find_map(|action| {
                if let crate::codegen::state::CleanupAction::FreeMapHandle {
                    map_alloca,
                    key_is_vec,
                    val_is_vec,
                    val_shared_heap_type,
                    key_shared_heap_type,
                    val_drop_fn,
                    key_drop_fn,
                } = action
                {
                    if *map_alloca == slot_ptr {
                        return Some(crate::codegen::state::MapElemDrop {
                            key_is_vec: *key_is_vec,
                            val_is_vec: *val_is_vec,
                            val_shared_heap_type: *val_shared_heap_type,
                            key_shared_heap_type: *key_shared_heap_type,
                            val_drop_fn: *val_drop_fn,
                            key_drop_fn: *key_drop_fn,
                        });
                    }
                }
                None
            });
        let Some(drop) = drop else {
            return;
        };
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let handle = self
            .builder
            .build_load(ptr_ty, slot_ptr, "reassign.old.map.handle")
            .unwrap()
            .into_pointer_value();
        self.emit_free_one_map_handle(handle, &drop);
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
        for frame in self.drop_rc.scope_cleanup_actions.iter_mut() {
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
        if self.borrow_vars.ref_params.contains_key(name) {
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
            let container_te = self.drop_rc.owned_temp_drops.get(&span_key).cloned();
            let elem_ty = container_te
                .as_ref()
                .and_then(|te| self.extract_vec_elem_type(te));
            // B-2026-08-15-14 (second registration site) — the `ref`-param
            // sibling of the same shortcut `materialize_owned_temp` had. A
            // fresh `Vec` rvalue passed to a `ref Vec[T]` param is materialized
            // HERE, not there, so `agg(ns.clone())` against `ns: ref Vec[Node]`
            // kept leaking one RC box per element after that fix: the callee
            // only borrows, so the caller still owns the temp and owes its
            // elements a release. Same three-way dispatch, deliberately
            // identical rather than merely similar — the two sites answer one
            // question ("what drops an element of this container?") and the
            // whole class of bug here is them answering it differently.
            let map_elem_drop = container_te
                .as_ref()
                .and_then(|te| self.extract_vec_elem_type_expr(te))
                .and_then(|et| self.vec_elem_map_drop_for_type_expr(&et));
            let agg_elem_drop = container_te
                .as_ref()
                .and_then(|te| self.extract_vec_elem_type_expr(te))
                .and_then(|et| self.vec_elem_agg_drop_for_type_expr(&et));
            match (map_elem_drop, agg_elem_drop, elem_ty) {
                (Some(map_drop), _, _) => self.track_vec_of_maps_var(slot, map_drop),
                (None, Some(agg_drop), Some(elem_ty)) => {
                    self.track_vec_of_aggs_var(slot, elem_ty, agg_drop)
                }
                _ => self.track_vec_var(slot, elem_ty),
            }
            return;
        }
        // B-2026-08-01-4 — a fresh owned Drop-bearing STRUCT/ENUM rvalue
        // passed to a `ref` param (`peek(mk(2))`): the callee only borrows,
        // so the caller owns the temp, and NOTHING here registered its user
        // Drop body — `karac run` fires it as the call returns
        // (`run_fresh_temp_arg_drops`), `karac build` was silent forever.
        // Register under the `__refarg_tmp` name the statement-end drain
        // fires. Same channel selection as the discard registrar: the
        // `karac_drop_<T>` wrapper for an own-`impl Drop` type (body, plus
        // struct-memory synthesis — the temp is dead after the statement),
        // the field-bodies walk for a transitively-Drop struct, the
        // declared-type payload walker for a value enum. Shared types stay
        // with the rc machinery; a non-fresh arg (a place rvalue) is the
        // owner's business.
        // A struct LITERAL is fresh by construction but
        // `expr_yields_fresh_owned_temp` only admits Call/MethodCall shapes,
        // so `peek(Res { .. })` silently skipped this leg while the
        // interpreter's arg hook fired (the B-2026-08-01-4 recorded
        // residual, closed here).
        let arg_is_fresh = self.expr_yields_fresh_owned_temp(arg_expr)
            || matches!(&arg_expr.kind, ExprKind::StructLiteral { .. });
        if val.get_type().is_struct_type() && arg_is_fresh {
            let tn = match &arg_expr.kind {
                ExprKind::Call { callee, .. } => match &callee.kind {
                    ExprKind::Identifier(n) => self
                        .fn_sig
                        .fn_return_type_names
                        .get(n)
                        .cloned()
                        .or_else(|| self.enum_name_of_expr(arg_expr)),
                    ExprKind::Path { .. } => self.enum_name_of_expr(arg_expr),
                    _ => None,
                },
                ExprKind::StructLiteral { path, .. } => path.last().cloned(),
                _ => None,
            };
            if let Some(tn) = tn {
                if !self.type_decls.shared_types.contains_key(&tn) {
                    let has_own_drop = self
                        .program_snapshot
                        .as_deref()
                        .is_some_and(|p| p.drop_method_keys.contains_key(&tn));
                    if self.type_decls.enum_layouts.contains_key(&tn) {
                        if has_own_drop {
                            self.track_user_drop_var(&tn, "__refarg_tmp", slot);
                        } else if let Some(w) = self.emit_enum_payload_user_drop_bodies_fn(&tn) {
                            self.track_user_drop_var_with_fn(
                                &tn,
                                "__refarg_tmp",
                                slot,
                                w,
                                UserDropKind::ContainerElemBodies,
                            );
                        }
                        if self.enum_has_heap_payload(&tn) {
                            self.track_enum_var(&tn, slot);
                        }
                    } else if self.type_decls.struct_types.contains_key(&tn) {
                        if has_own_drop {
                            self.track_user_drop_var(&tn, "__refarg_tmp", slot);
                        } else if self.type_runs_user_drop(&tn, &mut Vec::new()) {
                            if let Some(f) = self.field_bodies_fn_for_owned_temp(&tn) {
                                self.track_user_drop_var_with_fn(
                                    &tn,
                                    "__refarg_tmp",
                                    slot,
                                    f,
                                    UserDropKind::StructFieldBodies,
                                );
                            }
                            self.track_struct_var(&tn, slot);
                        } else {
                            // No Drop anywhere: heap-field memory only.
                            self.track_struct_var(&tn, slot);
                        }
                    }
                }
            }
            return;
        }
        if !val.is_pointer_value() {
            return;
        }
        let Some(te) = self.drop_rc.owned_temp_drops.get(&span_key).cloned() else {
            return;
        };
        let head = match &te.kind {
            TypeKind::Path(p) => p.segments.first().map(|s| s.as_str()).unwrap_or(""),
            _ => return,
        };
        if head == "Map" || head == "Set" {
            let (key_is_vec, val_is_vec, key_shared, val_shared, val_drop_fn, key_drop_fn) =
                self.map_temp_cleanup_parts(&te);
            self.track_map_var_with_val_drop(
                slot,
                key_is_vec,
                val_is_vec,
                val_shared,
                key_shared,
                val_drop_fn,
                key_drop_fn,
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
                // `!= None`, NOT `is_heap_bearing()` — B-2026-08-05-7. A
                // `BoxedOptRes` field answers false to `is_heap_bearing` (see
                // its doc: the entry-copy must not duplicate its box and the
                // match-out suppression must not strand it), but a WHOLE-VALUE
                // move is the one site where its word DOES have to be zeroed:
                // the destination receives the box pointer verbatim, so leaving
                // the source armed makes `let w2 = w;` free the same box twice
                // (measured as an ASAN double-free the moment the drop existed).
                // The match-out sibling keeps the `is_heap_bearing` gate — there
                // the source still owns the box and only its interior moved.
                if *kind == super::state::EnumDropKind::None {
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
        self.zero_struct_move_caps_mono(base_ptr, struct_name, None);
    }

    /// `zero_struct_move_caps` with an explicit generic instantiation subst, so
    /// a SINGLE-field generic wrapper `W[T] { f: T }` whose mono drop now frees
    /// a bare-T Vec/String field (B-2026-07-15-11) gets that field's `cap`/`len`
    /// zeroed on a whole-struct move — keeping the moved-out source's drop a
    /// no-op (the consumer is the sole owner). Same single-field offset gate as
    /// the drop classifier: `struct_types[W]` erases a bare-T field to one i64
    /// word, so reinterpreting field 0 as a `{ptr,len,cap}` is offset-correct
    /// only at offset 0. `None`/empty subst reproduces the name-keyed behavior
    /// exactly (non-generic structs pass `None`).
    pub(super) fn zero_struct_move_caps_mono(
        &self,
        base_ptr: PointerValue<'ctx>,
        struct_name: &str,
        subst: Option<&std::collections::HashMap<String, TypeExpr>>,
    ) {
        let Some(&base_st) = self.type_decls.struct_types.get(struct_name) else {
            return;
        };
        // B-2026-07-15-24 — GEP fields with the PER-MONOMORPH layout under a
        // real subst (the move-suppression twin of the drop-synthesis `st`
        // override): a bare generic-param field bound to a wider heap type
        // widens the layout, so a following Vec/Map/enum field's cap/handle
        // null-store must land at the mono offset, not the base erased one, or
        // the source stays live and double-frees the moved-out destination.
        let st = subst
            .and_then(|s| self.mono_struct_type_from_subst(struct_name, s))
            .unwrap_or(base_st);
        let Some(field_names) = self
            .type_decls
            .struct_field_type_names
            .get(struct_name)
            .cloned()
        else {
            return;
        };
        let field_tes = self
            .type_decls
            .struct_field_type_exprs
            .get(struct_name)
            .cloned();
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
            // B-2026-07-15-24 (generalizes B-2026-07-15-11) — a bare generic-
            // param field this monomorph binds to a direct Vec/String: the mono
            // drop now frees it (drop classifier VecOrString), so zero its
            // `len`+`cap` — mirroring the concrete Vec/String field arm — to keep
            // the moved-out source drop a no-op. Applies to ANY field position
            // now that `st` is the per-monomorph layout (the original B-11 gate
            // was single-field-only because the base layout mis-offset a mid
            // bare-T field). A no-op unless a real subst binds this field's bare
            // param to a direct heap type.
            // The field's CONCRETE type under this subst, when the declared type
            // is a bare generic param. `None` for a concrete field (nothing to
            // resolve) and outside a subst.
            let bare_param_concrete = subst.and_then(|s| {
                let fte = field_tes.as_ref()?.get(i)?;
                let is_bare_param = matches!(
                    &fte.kind,
                    TypeKind::Path(p)
                        if p.segments.len() == 1
                            && p.generic_args.is_none()
                            && s.contains_key(&p.segments[0])
                );
                if !is_bare_param {
                    return None;
                }
                Some(crate::codegen::helpers::subst_type_params_in_type_expr(
                    fte, s,
                ))
            });
            let bare_t_heap = bare_param_concrete.as_ref().is_some_and(|cte| {
                let is_vec_head = matches!(
                    &cte.kind,
                    TypeKind::Path(p)
                        if matches!(
                            p.segments.last().map(|s| s.as_str()),
                            Some("Vec") | Some("VecDeque")
                        )
                );
                self.is_string_type_expr(cte) || is_vec_head
            });
            if bare_t_heap {
                for word in [1u32, 2u32] {
                    if let Ok(wp) = self.builder.build_struct_gep(
                        vec_ty,
                        field_ptr,
                        word,
                        &format!("smv.bt{i}.w"),
                    ) {
                        let _ = self.builder.build_store(wp, zero);
                    }
                }
                continue;
            }
            // B-2026-08-06-1 — every arm below dispatches on `fname`, the
            // DECLARED field type name, which for a bare generic-param field is
            // the erased `T`: no arm matches and the source keeps a live handle.
            // `bare_t_heap` above rescued only the Vec/String heads, so a Map /
            // Set / shared / enum / nested-struct instantiation fell through and
            // the moved-out source's drop double-freed against the destination's
            // (`let c = b;` on a `Box[Map[i64, String]]` — 1880 valgrind errors,
            // once the drop classifier learned to free that field at all).
            // Substituting the head name puts these arms on the same type the
            // drop synthesizer classifies by. Concrete fields resolve to `None`
            // and keep their declared name, so this is inert outside a subst.
            let bare_head = bare_param_concrete
                .as_ref()
                .and_then(|cte| match &cte.kind {
                    TypeKind::Path(p) => p.segments.last().map(|s| s.as_str()),
                    _ => None,
                });
            let fname = bare_head.unwrap_or(fname);
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
            } else if fname == "Result" {
                // B-2026-07-21-15 — the Result sibling: the struct drop now
                // frees a direct-String/Vec-halves Result field's payload, so
                // a whole-struct move must zero the source's payload area
                // (the destination owns it). No-op layout-wise for wider
                // Result shapes (their struct drop registers no free).
                if let Some(layout) = self.type_decls.enum_layouts.get("Result") {
                    let result_ty = layout.llvm_type;
                    self.zero_result_payload_area(result_ty, field_ptr, "smv.res");
                }
            } else if matches!(
                fname,
                "Map" | "HashMap" | "Set" | "HashSet" | "SortedMap" | "SortedSet"
            ) {
                // B-2026-07-15-23 — a Map/Set field is a single opaque `ptr`
                // handle stored inline. The struct's `StructDrop` (`FieldDrop::
                // MapOrSet`) frees it UNCONDITIONALLY via `karac_map_free_with_
                // drop_vec(handle, ..)` — no `handle != 0` guard, unlike the HTTP
                // handle arm. So a whole-struct move (`let g = f`) or a struct-
                // field move-out that leaves the SOURCE handle live double-frees
                // the map storage against the destination's own drop (SIGSEGV /
                // `free(): double free`). Null the source handle so the drop's
                // free no-ops: `karac_map_free_with_drop_vec` (and every map-free
                // variant) early-returns on `map.is_null()`. This closes the gap
                // the line-4350 comment flagged as "needs a separate runtime
                // change" — stale: the runtime null-guard already exists, so it's
                // a pure codegen null-store, exactly parallel to the Vec cap-zero.
                //
                // B-2026-08-06-6 added `SortedMap` / `SortedSet` to this list:
                // B-2026-08-02-21 taught the drop classifier to free them, and a
                // field the drop frees but the move does not neutralize is the
                // same double-free this arm exists to prevent.
                let ptr_ty = self.context.ptr_type(AddressSpace::default());
                let _ = self.builder.build_store(field_ptr, ptr_ty.const_null());
            } else if self.type_decls.shared_types.contains_key(fname) {
                // B-2026-07-28-9 — a `shared struct` / `shared enum` HANDLE
                // field is one inline `ptr`, and the struct's drop rc-dec's it
                // (`__karac_vec_elem_full_drop_<S>`'s `nstr.sh.*` block). A
                // whole-struct move that left the source handle live therefore
                // rc-dec'd TWICE for one owned reference: once from the
                // moved-out source's drop and once from the destination's. The
                // second dec then reads the refcount word of a block the first
                // already freed — a use-after-free, and the following `free`
                // is skipped only because the garbage it reads is rarely 1, so
                // alloc/free counts still balance and only a sanitizer sees it.
                //
                // Null the source handle. The drop's `nstr.sh.isnull` guard
                // makes it a no-op, exactly parallel to the Map/Set arm above.
                // Must precede the `enum_layouts` / `struct_types` arms below:
                // a shared ENUM has an `enum_layouts` entry (skipped there for
                // `is_shared`) and a shared STRUCT is explicitly excluded from
                // the nested-struct recursion, so both fell through to nothing.
                let ptr_ty = self.context.ptr_type(AddressSpace::default());
                let _ = self.builder.build_store(field_ptr, ptr_ty.const_null());
            } else if fname != "Result" {
                if let Some(layout) = self.type_decls.enum_layouts.get(fname).cloned() {
                    if !layout.is_shared {
                        self.zero_enum_payload_caps(field_ptr, &layout);
                    }
                } else if self.type_decls.struct_types.contains_key(fname)
                    && !self.type_decls.shared_types.contains_key(fname)
                {
                    // B-2026-07-15-11 — recurse with the nested struct's own mono
                    // subst (derived from the field's declared `Box[String]`,
                    // resolving the parent's subst first), so a nested single-
                    // field generic wrapper's bare-T Vec/String field cap is
                    // zeroed on a whole-parent move — matching the nested mono
                    // drop and preventing a double-free. Empty subst → the
                    // name-shared recursion, unchanged.
                    let nsub = self.nested_struct_field_subst(struct_name, i, subst, fname);
                    self.zero_struct_move_caps_mono(field_ptr, fname, Some(&nsub));
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
        self.zero_struct_field_move_cap_in(base_ptr, struct_name, field, None)
    }

    /// [`Self::zero_struct_field_move_cap_in`] with the SOURCE BINDING's recorded
    /// generic instantiation (`Box[Map[i64, String]]` for `fn take(b: Box[Map[i64,
    /// String]])`), so a field declared as a bare type param is classified by what
    /// the param actually binds to.
    ///
    /// Without it this helper resolves the field type through
    /// `subst_monomorph_type_params` alone — the ACTIVE FUNCTION's monomorph
    /// subst. A generic wrapper taken at concrete args sits in a CONCRETE
    /// function, so there is no active subst and a bare-`T` field stays literally
    /// `T`: every name arm below misses and the neutralizer emits nothing, while
    /// the struct drop — synthesized against the same binding's instantiation via
    /// `track_struct_var_inst` — DOES free the field. That disagreement is a
    /// use-after-free the moment the field is moved out (B-2026-08-06-1).
    ///
    /// Threading the instantiation is what puts the two sides back on the same
    /// type. It is the per-field sibling of the whole-struct
    /// `zero_struct_move_caps_mono` call in
    /// [`Self::suppress_source_vec_cleanup_for_arg_ex`], which has resolved its
    /// source through `enum_inst_var_types` since B-2026-07-15-11.
    pub(super) fn zero_struct_field_move_cap_inst(
        &self,
        base_ptr: PointerValue<'ctx>,
        struct_name: &str,
        field: &str,
        st_override: Option<inkwell::types::StructType<'ctx>>,
        inst: Option<&TypeExpr>,
    ) {
        self.zero_struct_field_move_cap_impl(base_ptr, struct_name, field, st_override, inst)
    }

    /// [`Self::zero_struct_field_move_cap`] with the struct's CONCRETE LLVM
    /// type supplied by the caller.
    ///
    /// The no-override form resolves the GEP type as "active monomorph subst,
    /// else the declared base". That is wrong for a CONCRETE function over a
    /// generic struct — `fn take(b: Box[String]) -> String { b.v }` — which
    /// has no active subst, so it fell back to the base, whose bare-`T` field
    /// is an erased placeholder. The `held == st` caller gate then failed, no
    /// cap-zero was emitted, and the struct drop freed the very buffer the
    /// function was about to return: a DOUBLE FREE on a default -O2 build
    /// (B-2026-08-06-2). The generic monomorph escaped it only because it DOES
    /// have a subst — same gap B-2026-08-05-33(a) hit at its own site.
    ///
    /// A binding's slot type is that concrete layout by construction, so the
    /// caller can just hand it over. Byte-identical for a non-generic struct
    /// (slot type == base) and for a monomorph (slot type == the subst type);
    /// only the concrete-instantiation case changes.
    pub(super) fn zero_struct_field_move_cap_in(
        &self,
        base_ptr: inkwell::values::PointerValue<'ctx>,
        struct_name: &str,
        field: &str,
        st_override: Option<inkwell::types::StructType<'ctx>>,
    ) {
        self.zero_struct_field_move_cap_impl(base_ptr, struct_name, field, st_override, None)
    }

    fn zero_struct_field_move_cap_impl(
        &self,
        base_ptr: inkwell::values::PointerValue<'ctx>,
        struct_name: &str,
        field: &str,
        st_override: Option<inkwell::types::StructType<'ctx>>,
        inst: Option<&TypeExpr>,
    ) {
        let Some(field_names) = self.type_decls.struct_field_names.get(struct_name) else {
            return;
        };
        let Some(idx) = field_names.iter().position(|n| n == field) else {
            return;
        };
        // GEP struct type: inside a generic-fn monomorph use the CONCRETE mono
        // struct type — its field offsets match the stored value, whereas the
        // generic base type has an erased placeholder at each bare-`T` position
        // (`Box[T].v` at T=String: base lays the field out as `i64`, the mono as
        // `{ptr,len,cap}`), so GEP-ing field 2 (`cap`) off the base would write
        // the wrong offset. Outside a monomorph (empty subst) this returns the
        // base type, so non-generic callers are byte-identical to before.
        // B-2026-07-18-44.
        let Some(st) = st_override
            .or_else(|| self.mono_struct_type_from_active_subst(struct_name))
            .or_else(|| self.type_decls.struct_types.get(struct_name).copied())
        else {
            return;
        };
        // Concrete field type, resolved through the active monomorph subst so a
        // bare-`T` field is seen as its real type (a no-op outside a monomorph).
        //
        // B-2026-08-06-1 — the active-subst resolution alone is blind to a
        // generic wrapper instantiated at CONCRETE args, because that lives in a
        // concrete function with no subst to consult. Fall back to the source
        // BINDING's recorded instantiation, which is the same binding the struct
        // drop was synthesized against (`track_struct_var_inst`), so the free
        // list and the neutralize list classify the field identically. Applied
        // only when the active-subst pass left the field a bare param, so a
        // monomorph body is byte-identical to before.
        let field_te = self
            .type_decls
            .struct_field_type_exprs
            .get(struct_name)
            .and_then(|v| v.get(idx))
            .map(|te| self.subst_monomorph_type_params(te))
            .map(|te| {
                let unresolved = matches!(
                    &te.kind,
                    TypeKind::Path(p)
                        if p.segments.len() == 1
                            && p.generic_args.is_none()
                            && self
                                .type_decls
                                    .struct_generic_params
                                .get(struct_name)
                                .is_some_and(|ps| ps.contains(&p.segments[0]))
                );
                match (unresolved, inst) {
                    (true, Some(inst)) => {
                        let subst = self.generic_struct_subst_from_inst(struct_name, inst);
                        crate::codegen::helpers::subst_type_params_in_type_expr(&te, &subst)
                    }
                    _ => te,
                }
            });
        let fname = field_te
            .as_ref()
            .and_then(|te| match &te.kind {
                TypeKind::Path(p) => p.segments.first().map(|s| s.to_string()),
                _ => None,
            })
            .or_else(|| {
                self.type_decls
                    .struct_field_type_names
                    .get(struct_name)
                    .and_then(|v| v.get(idx))
                    .and_then(|o| o.clone())
            })
            .unwrap_or_default();
        let Ok(field_ptr) = self
            .builder
            .build_struct_gep(st, base_ptr, idx as u32, "sfld.move.p")
        else {
            return;
        };
        let vec_ty = self.vec_struct_type();
        let zero = self.context.i64_type().const_int(0, false);
        // Match Vec/String by concrete LLVM shape too: a String field resolved
        // through the monomorph subst carries the name `str`, which the name list
        // below would miss — but it lowers to the same `{ptr,len,cap}` vec struct.
        let field_is_vecish = matches!(fname.as_str(), "Vec" | "VecDeque" | "String")
            || field_te
                .as_ref()
                .is_some_and(|te| self.llvm_type_for_type_expr(te) == vec_ty.into())
            // Concrete-layout fallback (B-2026-08-06-2): with no active subst
            // a bare-`T` field's type-expr stays `T` and lowers to the erased
            // placeholder, so both tests above miss a `Box[String]`. The GEP
            // type we are about to use already carries the real field layout.
            || st.get_field_type_at_index(idx as u32) == Some(vec_ty.into());
        if field_is_vecish {
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
        } else if fname == "Result" {
            // B-2026-07-21-15 — a moved-out `Result` field: zero the payload
            // area so the owner's struct-drop Result overlay free skips it
            // (no-op for wider Result shapes, which register no free).
            if let Some(layout) = self.type_decls.enum_layouts.get("Result") {
                let result_ty = layout.llvm_type;
                self.zero_result_payload_area(result_ty, field_ptr, "sfld.move.res");
            }
        } else if matches!(
            fname.as_str(),
            "Map" | "HashMap" | "Set" | "HashSet" | "SortedMap" | "SortedSet"
        ) {
            // B-2026-08-06-6 — the PER-FIELD sibling of the whole-struct
            // Map/Set null-store `zero_struct_move_caps_mono` has carried since
            // B-2026-07-15-23. It was never added here, so a field moved out
            // INDIVIDUALLY left the source handle live and the owner's
            // `FieldDrop::MapOrSet` freed storage the destination still owns:
            //
            //     struct MapH { m: Map[i64, String] }
            //     fn take(h: MapH) -> Map[i64, String] { return h.m; }
            //
            // segfaulted on a use-after-free (valgrind: `Invalid read of size
            // 8` inside `karac_map_free_with_drop_vec`, into a block already
            // freed by the callee's struct drop), while the interpreter printed
            // the right answer — a run-vs-build divergence on a concrete,
            // non-generic program.
            //
            // The convention is the runtime's, already: every map-free variant
            // early-returns on `map.is_null()`, and both half-walks
            // (`emit_map_key_drop_fn_walk`, `emit_map_shared_half_rc_dec_walk`)
            // null-guard internally. So this is a pure codegen null-store,
            // exactly parallel to the Vec cap-zero above.
            //
            // `SortedMap` / `SortedSet` are listed for SYMMETRY WITH THE DROP
            // CLASSIFIER, which B-2026-08-02-21 taught to free them — leaving
            // the two lists asymmetric is the exact shape of this bug. Stated
            // honestly: that pair is NOT independently reproducible today. A
            // `SortedMap` field moved out measures clean both before and after
            // (166 allocs / 166 frees, 0 valgrind errors), so this is defensive
            // symmetry rather than a measured fix, and adding a type to a
            // NEUTRALIZER list is safe by construction — it nulls a handle the
            // drop would otherwise free.
            let ptr_ty = self.context.ptr_type(AddressSpace::default());
            let _ = self.builder.build_store(field_ptr, ptr_ty.const_null());
        } else if self.type_decls.shared_types.contains_key(fname.as_str()) {
            // B-2026-08-06-8 — a moved-out DIRECT `shared` field. The peer of
            // the Map/Set null-store above, and the arm that makes the shared
            // half of that bug's fix safe.
            //
            // Once a struct local's scope-exit drop rc-dec's its shared fields
            // (the combined drop that generic owners now also reach), moving the
            // field OUT leaves two owners of one +1: the returned/destructured
            // handle, and the source struct whose drop still decs. The box hits
            // zero while the moved handle is live — a USE-AFTER-FREE, not a leak
            // (valgrind: `Invalid read of size 8`, 0 bytes into a free'd 32-byte
            // block, on a DEFAULT -O2 build).
            //
            // Nulling the source slot is the neutralizer the rc-dec walker was
            // already built to expect: every arm of
            // `emit_nested_struct_shared_rc_decs_ex` loads the field and
            // `build_is_null`-guards before dec'ing, so a nulled slot is simply
            // skipped. No inc, no runtime change — the same pure codegen
            // null-store the Map/Set arm does, against a drop that already
            // null-checks.
            //
            // Chosen over inc'ing the returned alias, which was the other
            // candidate: an inc has to be threaded onto BOTH the explicit-return
            // and tail-expression paths (they diverge — `compile_tail_final_expr`
            // is only reached from `return` when the fn returns `Option[shared]`),
            // and an over-inc is a silent leak. Nulling is one arm, on the path
            // every move-out already goes through.
            let ptr_ty = self.context.ptr_type(AddressSpace::default());
            let _ = self.builder.build_store(field_ptr, ptr_ty.const_null());
        } else {
            if let Some(layout) = self.type_decls.enum_layouts.get(fname.as_str()).cloned() {
                if !layout.is_shared {
                    self.zero_enum_payload_caps(field_ptr, &layout);
                }
            } else if self.type_decls.struct_types.contains_key(fname.as_str())
                && !self.type_decls.shared_types.contains_key(fname.as_str())
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
        if let Some(layout) = self.type_decls.enum_layouts.get("Option") {
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

    /// B-2026-08-13-3 — the struct-field move-out cap-zero one or more hops
    /// DEEP: `d.inner.word` rather than `d.word`.
    ///
    /// Same act, same reason, longer reach. The caller's arm zeroes a moved
    /// heap field's `cap` in its owner so the owner's drop skips it and the
    /// consumer is left sole owner; it resolves the owner by matching the
    /// receiver against an `Identifier`/`SelfValue`, which a chained place
    /// expression is not. So an owned by-value param whose NESTED field was
    /// moved out kept a live `cap`, its callee-owned struct drop recursed into
    /// the nested struct and freed the buffer, and the caller freed it again.
    ///
    /// Walks the chain to its root and GEPs down the same path the drop walks,
    /// then hands the innermost struct to the existing helper — so the zeroing
    /// rule itself is unchanged and lives in one place.
    ///
    /// GATED THE WAY THE ONE-LEVEL ARM IS, for the same safety property rather
    /// than by analogy:
    ///
    ///   - The ROOT slot must hold its struct INLINE. A `ref Struct` param's
    ///     slot is an 8-byte pointer into the CALLER's frame; GEP-ing a `cap`
    ///     off it writes past the alloca (the B-2026-07-07-4 class), and a
    ///     borrow owns nothing to move out of anyway.
    ///   - Every hop must be a non-shared user struct held INLINE in its
    ///     parent's layout. A pointer-shaped field (an RC box, a boxed enum
    ///     payload) is someone else's storage, and the refcount machinery owns
    ///     that decision.
    ///   - Each hop's LLVM field type must match the declared struct's
    ///     registered type. Inside a monomorph a bare-`T` field is erased in
    ///     the base layout, so a mismatch means the offsets cannot be trusted
    ///     — decline rather than GEP at a guessed offset.
    ///
    /// Every decline is the status-quo double free, not a new failure mode.
    fn zero_nested_struct_field_move_cap(&self, object: &Expr, field: &str) {
        // Collect the hops from the innermost outward, then reverse: for
        // `d.inner.word` (called with object = `d.inner`) this yields
        // `["inner"]` and root `d`.
        let mut hops: Vec<&str> = Vec::new();
        let mut cur = object;
        let root = loop {
            match &cur.kind {
                ExprKind::FieldAccess { object, field } => {
                    hops.push(field.as_str());
                    cur = object;
                }
                ExprKind::Identifier(n) => break n.as_str(),
                ExprKind::SelfValue => break "self",
                _ => return,
            }
        };
        if hops.is_empty() {
            // A plain `d.word` — the caller's own arm handles it.
            return;
        }
        hops.reverse();
        let Some(slot) = self.variables.get(root).copied() else {
            return;
        };
        let BasicTypeEnum::StructType(mut cur_ty) = slot.ty else {
            return;
        };
        let Some(mut cur_name) = self.var_types.var_type_names.get(root).cloned() else {
            return;
        };
        let mut cur_ptr = slot.ptr;
        for hop in hops {
            if self.type_decls.shared_types.contains_key(cur_name.as_str()) {
                return;
            }
            let Some(idx) = self
                .type_decls
                .struct_field_names
                .get(cur_name.as_str())
                .and_then(|names| names.iter().position(|n| n == hop))
            else {
                return;
            };
            // The hop's declared type must name a non-shared user struct, and
            // its LLVM slot must be that struct held inline.
            let Some(next_name) = self
                .type_decls
                .struct_field_type_exprs
                .get(cur_name.as_str())
                .and_then(|tes| tes.get(idx))
                .and_then(|te| match &te.kind {
                    TypeKind::Path(p) => p.segments.last().cloned(),
                    _ => None,
                })
            else {
                return;
            };
            if !self
                .type_decls
                .struct_types
                .contains_key(next_name.as_str())
                || self
                    .type_decls
                    .shared_types
                    .contains_key(next_name.as_str())
            {
                return;
            }
            let Some(BasicTypeEnum::StructType(next_ty)) =
                cur_ty.get_field_type_at_index(idx as u32)
            else {
                return;
            };
            if self.type_decls.struct_types.get(next_name.as_str()) != Some(&next_ty) {
                return;
            }
            let Ok(next_ptr) =
                self.builder
                    .build_struct_gep(cur_ty, cur_ptr, idx as u32, "nested.move.p")
            else {
                return;
            };
            cur_ptr = next_ptr;
            cur_ty = next_ty;
            cur_name = next_name;
        }
        if self.type_decls.shared_types.contains_key(cur_name.as_str()) {
            return;
        }
        self.zero_struct_field_move_cap_in(cur_ptr, &cur_name, field, Some(cur_ty));
    }

    pub(super) fn suppress_source_vec_cleanup_for_arg_ex(
        &mut self,
        arg_expr: &Expr,
        apply_shared_transfer: bool,
    ) {
        // B-2026-08-10-21 — the source of a `UseAfterMove` keeps its cleanup.
        //
        // Every one of this helper's ~87 call sites funnels here, which is why
        // the disarm half of the defensive copy is a single edit: at a move the
        // ownership pass flagged as reused, the consumer has been handed an
        // independent deep copy at the identifier load, so the source still
        // owns its original buffer and must still free it. Disarming here would
        // leave that buffer with no owner (a leak) while the later read stayed
        // valid — the mirror of the pre-fix bug, which disarmed the source and
        // let the consumer's free dangle it.
        //
        // Ordering note: this is the ONLY place the two halves have to agree.
        // If a future path copies without consulting this set, it double-frees;
        // if it skips the disarm without copying, it leaks.
        if self
            .span_tables
            .uam_copied_sites
            .contains(&(arg_expr.span.offset, arg_expr.span.length))
        {
            return;
        }
        // B-2026-08-15-10 — the CALL-ARGUMENT consume site, which the copy half
        // above cannot reach.
        //
        // That half runs at the three positions that compile a moved value
        // themselves — a `let` RHS and the two struct-literal field inits — and
        // there is no fourth hook to add, because an ARGUMENT has no shared
        // compile path: every builtin, method and free-fn lowers its own. So a
        // move that happens *as an argument* (`index.insert(e.service, 0)`) was
        // never copied, and this helper then disarmed the source as usual.
        //
        // The result was a borrow with no owner, not a double free, which is
        // why it stayed invisible: the disarm zeroes `e.service.cap`, so the
        // later reuse reads `{ptr, len, cap: 0}` — the right BYTES, pointing
        // into the buffer the callee now owns, with a cap that makes every drop
        // skip it. Nothing double-frees; the value is simply correct until the
        // consumer is freed. In `main` that is after the last read, so the
        // program prints the right answer and ASAN is clean, which is exactly
        // how "this class is fixed" survived: `main` is the one place the
        // timing hides it. Anywhere else the consumer dies at the callee's
        // scope exit and the escaping reuse dangles.
        //
        // COPY THE SOURCE, NOT THE CONSUMER. By the time a disarm site runs the
        // consumer has already been handed `{ptr,len,cap}`, so there is nothing
        // left to intercept on that side — but the source place is still right
        // here, and it is what this helper already writes to. Giving the SOURCE
        // a fresh buffer reaches the same end state (two owners, two buffers,
        // one free each) from the only side still reachable. The site is
        // recorded in `uam_copied_sites` for the same reason the other half
        // records: every disarm keys on "a copy really happened", so a source
        // that now owns its own buffer keeps its cleanup everywhere, not just
        // at the site that copied it.
        if self.uam_reclone_source_field(arg_expr) {
            return;
        }
        // B-2026-08-12-27 — a heap FIELD read off a Vec element
        // (`ps[0].word`) was deep-cloned at the read, and the clone carries
        // its own scope cleanup so a NON-consuming read does not leak. This is
        // a CONSUMING position, so the destination takes the clone over: zero
        // the clone's `cap` and leave it sole owner.
        //
        // Zeroing rather than retracting keeps this `&self`, which matters —
        // all ~87 call sites funnel here, so the takeover is one edit instead
        // of eight. The container's element is NOT touched: it still owns its
        // own buffer, which is the whole point of cloning rather than
        // spreading the `let` site's source cap-zeroing.
        if let Some(slot) = self
            .vec_elem_field_clone_slots
            .get(&(arg_expr.span.offset, arg_expr.span.length))
        {
            if let Ok(cap_ptr) =
                self.builder
                    .build_struct_gep(self.vec_struct_type(), *slot, 2, "vfld.clone.cap")
            {
                let _ = self
                    .builder
                    .build_store(cap_ptr, self.context.i64_type().const_int(0, false));
            }
            return;
        }
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
            // The receiver is an owned struct binding — a named `Identifier`
            // OR an owned `self` receiver (`fn get(self) -> String { self.v }`),
            // which parses as `SelfValue`, not `Identifier("self")`. Both bind an
            // inline struct slot under a name (`self` normalises to the "self"
            // binding); WITHOUT the `SelfValue` arm a method's `self.field`
            // tail-return move-out was not cap-zeroed, so `self`'s callee-owned
            // StructDrop freed the moved heap field AND the caller freed the
            // returned value — a double-free (the free-fn `b.field` form already
            // worked via the Identifier arm) (B-2026-07-18-37).
            let recv = match &object.kind {
                ExprKind::Identifier(s) => Some(s.as_str()),
                ExprKind::SelfValue => Some("self"),
                _ => None,
            };
            if let Some(s) = recv {
                if let (Some(slot), Some(struct_name)) = (
                    self.variables.get(s).copied(),
                    self.var_types.var_type_names.get(s).cloned(),
                ) {
                    if !self
                        .type_decls
                        .shared_types
                        .contains_key(struct_name.as_str())
                    {
                        let gep_st = self
                            .mono_struct_type_from_active_subst(struct_name.as_str())
                            .or_else(|| {
                                self.type_decls
                                    .struct_types
                                    .get(struct_name.as_str())
                                    .copied()
                            });
                        // The slot must hold the struct INLINE (owned): a `ref
                        // Struct` param's slot is an 8-byte pointer, not the
                        // struct value, so cap-zeroing there would corrupt the
                        // caller. Accept the slot when it equals EITHER the base
                        // struct type OR — inside a generic-fn monomorph — the
                        // concrete mono struct type (`Box[T].get` at T=String:
                        // the slot is the mono `{ {ptr,len,cap} }`, not the
                        // generic base whose field is an erased `i64`). Without
                        // the mono arm a generic method/free-fn returning a heap
                        // field of an owned by-value self/param never cap-zeroed
                        // the moved field, so the mono struct-drop freed it AND
                        // the caller freed the returned value — a double-free
                        // (B-2026-07-18-44; the monomorph analogue of the
                        // non-generic B-2026-07-18-37).
                        // The slot holding a StructType (not a pointer) is what
                        // proves this is an OWNED inline struct rather than a
                        // `ref Struct` borrow — that is the safety property this
                        // gate exists for, and it does not need the type equality.
                        // Hand the slot's own layout down as the GEP type: it is
                        // the concrete one even when `gep_st` degraded to the
                        // erased base (B-2026-08-06-2).
                        let _ = gep_st;
                        if let BasicTypeEnum::StructType(held) = slot.ty {
                            // B-2026-08-06-1 — hand down the receiver binding's
                            // recorded instantiation as well. The slot type gives
                            // the right OFFSETS; only the declared type-expr says
                            // WHAT the field is, and for a bare-`T` field in a
                            // concrete function nothing else can resolve it.
                            let inst = self.type_decls.enum_inst_var_types.get(s).cloned();
                            self.zero_struct_field_move_cap_inst(
                                slot.ptr,
                                &struct_name,
                                field,
                                Some(held),
                                inst.as_ref(),
                            );
                            // B-2026-08-06-10 — the receiver was DEBOXED out of
                            // an enum payload box, so the slot above is this
                            // frame's private copy and the zero lands where the
                            // box's owner cannot see it. When that owner is the
                            // CALLER (a by-value `Option[Struct]` param sharing
                            // the box pointer), its `karac_drop_Option_<T>`
                            // reads the box's own `{ptr,len,cap}`, still finds a
                            // live `cap`, and frees the buffer this move just
                            // handed to the destination.
                            //
                            // Mirror the neutralization into the box. Same
                            // helper, same layout — the box holds exactly the
                            // payload struct the slot copied — so a field the
                            // move did NOT take keeps its live cap and the
                            // owner still frees it. Retracting a cleanup action
                            // (`suppress_boxed_payload_view_move`) cannot serve
                            // here: across a call there is no action to retract,
                            // only data the caller's drop fn will read.
                            if let Some(box_ptr) = self
                                .payload_vars
                                .deboxed_payload_box_ptrs
                                .get(&slot.ptr)
                                .copied()
                            {
                                self.zero_struct_field_move_cap_inst(
                                    box_ptr,
                                    &struct_name,
                                    field,
                                    Some(held),
                                    inst.as_ref(),
                                );
                            } else if let Some(box_ptr) =
                                self.payload_vars.deferred_payload_box_ptrs.get(s).copied()
                            {
                                // B-2026-08-18-4 — a user `Drop` BODIES walk
                                // still has to read this box, so the zero above
                                // cannot be written HERE: the walk fires at the
                                // binding's death, after this move site, and
                                // would see the neutralized field. That is the
                                // exact regression B-2026-08-06-10's comment
                                // records — a double free traded for a Drop body
                                // printing an empty string.
                                //
                                // Queue it instead. The drain sits immediately
                                // after that walk in the `UserDrop` cleanup arm,
                                // which is before the box's own memory drop, so
                                // the body reads live fields and the memory drop
                                // reads the neutralized one. Same helper, same
                                // layout, same field — only the emission point
                                // moves.
                                self.payload_vars
                                    .pending_box_field_zeroes
                                    .entry(s.to_string())
                                    .or_default()
                                    .push(crate::codegen::payload_vars::PendingBoxFieldZero {
                                        box_ptr,
                                        struct_name: struct_name.to_string(),
                                        field: field.to_string(),
                                        st: Some(held),
                                        inst: inst.clone(),
                                    });
                            }
                        }
                    }
                }
            } else {
                // B-2026-08-13-3 — the receiver is itself a place expression
                // (`d.inner.word`), so the arms above see no `Identifier` and
                // decline. A NESTED heap field moved out of an owned by-value
                // aggregate param was therefore never cap-zeroed: the param's
                // callee-owned struct drop walked into `inner` and freed
                // `word`, while the value handed to the caller owned it too —
                // `fn take(d: Deep) -> String { d.inner.word }` aborts with
                // `free(): double free detected in tcache 2` on both compiled
                // backends where the interpreter prints the string. The
                // one-level twin (`fn take(p: Pair) -> String { p.word }`) has
                // always worked, which is what makes this a gap in reach
                // rather than a difference in ownership.
                self.zero_nested_struct_field_move_cap(object, field);
            }
            return;
        }
        let var_name = match &arg_expr.kind {
            ExprKind::Identifier(n) => n.as_str(),
            // An owned `self` receiver moved OUT of a method — `fn m(self) -> T
            // { self }` (tail return) or `let b = self` (rebind) — needs the
            // same move-out cap-zeroing as an owned struct Identifier, else
            // self's callee-owned StructDrop and the moved-out value's owner
            // (the caller's binding) both free the same heap-field buffer: a
            // use-after-free / double-free (B-2026-07-17-3, the owned-`self`
            // builder/fluent pattern). Gated to a self slot that holds the
            // aggregate INLINE (owned): a `ref self` holds a POINTER into the
            // caller's frame, and although a borrow can't be moved out (so this
            // is never reached for `ref self`), the inline guard makes the
            // struct arm's cap GEP provably safe — it never writes through a
            // ref pointer into the caller's struct.
            ExprKind::SelfValue
                if self.variables.get("self").is_some_and(|s| {
                    matches!(s.ty, inkwell::types::BasicTypeEnum::StructType(_))
                }) =>
            {
                "self"
            }
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
        if self.var_types.vec_elem_types.contains_key(var_name) {
            // B-2026-07-25-1: an OWNED `String`/`Vec` PARAM is caller-retains.
            // The callee never registers a `FreeVecBuffer` for it (that is why
            // it lands in `owned_vecstr_params` instead of `track_vec_var`), and
            // every retaining consume site deep-copies it via
            // `maybe_defensive_copy_param_arg` — so the param's buffer is never
            // actually moved out and there is NO cleanup here to suppress.
            // Zeroing its `cap` is therefore pure damage: it destroys the
            // header's ownership bit for every LATER use in the same function.
            // The next consume reads `cap == 0`, concludes "borrowed view", and
            // SKIPS its own defensive copy — storing a raw alias into a buffer
            // this frame does not own. In the ledger repro that alias points at
            // the caller's map-derived `Vec[String]` element, which is freed as
            // soon as the recursive call returns, so `route` ends up holding a
            // dangling pointer (ASan: read in `main` of a block allocated by
            // `karac_string_clone` and freed in `visit`). Invisible whenever the
            // consume happens to be the param's LAST use — which is why this
            // survived so long.
            if self.borrow_vars.owned_vecstr_params.contains(var_name) {
                return;
            }
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
        if self.accel.tensor_var_infos.contains_key(var_name) {
            let _ = self.builder.build_store(slot.ptr, ptr_ty.const_null());
            return;
        }
        // Column binding: null the source slot so its `FreeColumn`
        // cleanup's null-guard skips — the consumer (tail return, by-
        // value call arg, `let b = a;`) now owns the control block + its
        // two buffers. The Column analog of the Tensor arm above.
        if self.accel.column_var_infos.contains_key(var_name) {
            let _ = self.builder.build_store(slot.ptr, ptr_ty.const_null());
            return;
        }
        // DataFrame binding: null the source slot so its `FreeDataFrame`
        // cleanup's null-guard skips — the consumer (`let b = a;`, by-value
        // arg, tail return) now owns the control block + every column /
        // name it holds. The DataFrame analog of the Column arm above.
        if self.accel.dataframe_var_infos.contains(var_name) {
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
        if let Some(tn) = self.var_types.var_type_names.get(var_name) {
            if matches!(tn.as_str(), "Map" | "Set")
                && !self.borrow_vars.ref_params.contains_key(var_name)
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
        if let Some(type_name) = self.var_types.var_type_names.get(var_name).cloned() {
            if let Some(info) = self
                .type_decls
                .shared_types
                .get(type_name.as_str())
                .cloned()
            {
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
        if let Some(type_name) = self.var_types.var_type_names.get(var_name).cloned() {
            if let Some(layout) = self
                .type_decls
                .enum_layouts
                .get(type_name.as_str())
                .cloned()
            {
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
        if let Some(type_name) = self.var_types.var_type_names.get(var_name).cloned() {
            if self.type_decls.struct_types.contains_key(&type_name) {
                // Recursive move-suppression: zero every transitive heap field's
                // cap (Vec/String, enum payloads post-#15/#19, nested structs
                // — #18's `Wrap { sp: Span { tok } }`) + the HTTP handle, so the
                // source struct's `StructDrop` no-ops and the consumer (caller /
                // new binding / struct or enum literal) is the sole owner.
                // B-2026-07-15-11 — thread the source binding's recorded generic
                // instantiation so a single-field bare-T Vec/String wrapper's
                // mono drop (now freeing that field) is matched by a cap-zero
                // here; otherwise a `let c = b` / `return b` whole-move
                // double-frees against the added scope-exit drop.
                let subst = self
                    .type_decls
                    .enum_inst_var_types
                    .get(var_name)
                    .map(|i| self.generic_struct_subst_from_inst(&type_name, i));
                self.zero_struct_move_caps_mono(slot.ptr, &type_name, subst.as_ref());
                // B-2026-08-06-10, whole-payload sibling of the field move-out
                // mirror above. `x` here is a deboxed COPY of a payload box the
                // CALLER owns (`fn f(h: Option[H]) { match h { Some(x) => x } }`
                // moves the whole struct out), so the zeroing lands in this
                // frame and the caller's `karac_drop_Option_<T>` still frees the
                // buffers the return value carries away — a double free, which
                // is exactly what the caller-side carve-out turns the leak into
                // if this mirror is missing. Registration is gated to an
                // owned-param scrutinee, so an in-frame boxed view keeps
                // B-2026-08-04-2's retraction and never reaches this store.
                if let Some(box_ptr) = self
                    .payload_vars
                    .deboxed_payload_box_ptrs
                    .get(&slot.ptr)
                    .copied()
                {
                    self.zero_struct_move_caps_mono(box_ptr, &type_name, subst.as_ref());
                }
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
                .var_types
                .var_type_names
                .get(var_name)
                .is_some_and(|n| self.type_decls.struct_types.contains_key(n.as_str()));
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
        // B-2026-08-02-10: an ANNOTATED tuple binding carries full element
        // TypeExprs — prefer them (generic args intact: `Vec[i64]` stays
        // `Vec[i64]`, not the erased name "Vec"). Unannotated bindings fall
        // through to the names-derived synthesis below.
        if let Some(tes) = self.var_types.tuple_var_elem_type_exprs.get(var_name) {
            return Some(tes.clone());
        }
        // B-2026-08-03-3: the let-site's own record (`tuple_binding_elem_tes` —
        // annotation, else the RHS literal refined through
        // `refined_tuple_literal_elem_te`) beats the names-derived synthesis
        // below, which renders an `Option[Res]` element as an EMPTY path when
        // the element has no recorded type NAME. That empty path reads as a
        // no-drop leaf, so `suppress_tuple_index_move_source` silently skipped
        // the move-out neutralization for `let x = t.0`.
        let names = self.var_types.tuple_var_elem_type_names.get(var_name)?;
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
            .type_decls
            .struct_field_names
            .get(&type_name)
            .and_then(|names| names.iter().position(|n| n == field))
        else {
            return;
        };
        let Some(field_te) = self
            .type_decls
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

    /// Is this type-expr still a bare, unresolved single-segment generic param?
    pub(super) fn type_expr_is_bare_param(te: &TypeExpr) -> bool {
        matches!(
            &te.kind,
            TypeKind::Path(p) if p.segments.len() == 1 && p.generic_args.is_none()
        )
    }

    /// Direct-`shared` companion to [`Self::share_option_shared_field_ref_for_arg`],
    /// for a field RETURNED out of a CALLER-RETAINS struct param (B-2026-08-06-14).
    ///
    /// `fn giveback(b: Holder) -> Node { return b.v; }` with
    /// `struct Holder { v: Node }` over a `shared struct Node` was a
    /// use-after-free on a DEFAULT -O2 build: valgrind `Invalid read of size 8`,
    /// 0 bytes into a free'd 32-byte block, with the rc box's count driven one
    /// dec below zero. It reproduces with the field never read by the caller, so
    /// it is a scope-exit accounting bug, not a bad load.
    ///
    /// THE ROW FOR THIS BUG BLAMED THE WRONG THING — it read the generic
    /// spelling being clean as "B-2026-08-06-8's null-store reaches that move
    /// site and not this one". Instrumenting both shows the null-store fires
    /// identically for `Holder` and `Box[Node]`. The real split is a REGIME
    /// difference set one branch away, at the by-value-param arm in
    /// `param_own.rs`:
    ///
    ///   * a struct that does NOT transitively own a `shared` field is owned BY
    ///     TRANSFER (B-2026-08-05-33) — the callee registers the drop and the
    ///     caller retracts its own, in lockstep. Returning a field then just
    ///     needs the source neutralized, which the null-store does. This is the
    ///     path `Box[T]` takes, because the gate there is NAME-ONLY and a bare
    ///     `T` reads as non-shared — which is why the generic spelling looked
    ///     clean and made the bug appear generics-related.
    ///   * a struct that DOES own one stays CALLER-RETAINS (B-2026-08-05-32):
    ///     the callee deliberately registers nothing, because rc-dec'ing here
    ///     as well would dec twice. That is the regime `Holder` is in, and in it
    ///     the caller's +1 is still live — so a field handed out of the callee
    ///     is an ALIAS and needs its own ref, exactly as
    ///     `clone_on_extract_view_field`'s bare-`shared` arm gives a view
    ///     extract.
    ///
    /// So this is the one place the inc really is the right instrument, and
    /// null-storing would be actively wrong: the source the caller still owns
    /// must keep its handle. A `ref` param is the same regime by definition and
    /// is covered by the same condition — it reproduced too.
    ///
    /// Narrow on purpose, since an over-inc is a leak and harder to attribute
    /// than a crash:
    ///   * only an Identifier object that is a PARAM of this function;
    ///   * only a non-shared struct type in the caller-retains regime, tested
    ///     with the SAME name-only predicate the param arm gates on, so the two
    ///     cannot drift apart;
    ///   * only a DIRECT `shared T` field, resolved through the active
    ///     monomorph subst first.
    pub(super) fn share_direct_shared_field_ref_for_return(
        &self,
        object: &Expr,
        field: &str,
        val: BasicValueEnum<'ctx>,
    ) {
        let ExprKind::Identifier(obj) = &object.kind else {
            return;
        };
        // Params only: a LOCAL's field move-out is the same-scope case, where
        // the null-store neutralizer already transfers the handle (measured
        // clean both before and after this change).
        if !self.fn_ctx.current_fn_param_names.contains(obj.as_str()) {
            return;
        }
        // A shared OBJECT's field read incs on its own path.
        if self.shared_type_for_expr(object).is_some() {
            return;
        }
        let Some(type_name) = self.var_types.var_type_names.get(obj).cloned() else {
            return;
        };
        if self
            .type_decls
            .shared_types
            .contains_key(type_name.as_str())
        {
            return;
        }
        // THE REGIME TEST, and the whole correctness argument: inc only where
        // the callee did NOT take the param's drop, so the caller's ref is still
        // live. Same predicate, same arguments as the `param_own.rs` gate.
        if !self.struct_owns_shared_field(&type_name, &mut Vec::new()) {
            return;
        }
        let Some(idx) = self
            .type_decls
            .struct_field_names
            .get(&type_name)
            .and_then(|names| names.iter().position(|n| n == field))
        else {
            return;
        };
        let Some(field_te) = self
            .type_decls
            .struct_field_type_exprs
            .get(&type_name)
            .and_then(|v| v.get(idx))
            .cloned()
        else {
            return;
        };
        let field_te = self.subst_monomorph_type_params(&field_te);
        let Some(heap_type) = self.shared_heap_type_for_type_expr(&field_te) else {
            return;
        };
        let BasicValueEnum::PointerValue(ptr) = val else {
            return;
        };
        self.emit_refcount_inc(&type_name, heap_type, ptr);
    }

    /// Index companion to `share_option_shared_ref_for_arg` /
    /// `share_option_shared_field_ref_for_arg`: when the call arg is a plain
    /// (non-range) Vec-element index `v[i]` whose element type is
    /// `Option[shared T]`, inc the inner of the already-loaded value `val`. The
    /// niche Vec-element read (`compile_index`'s niche path) LOADS the inner
    /// pointer without an inc — the container still owns that +1 — so passing
    /// it by value to a callee whose param queues an `RcDecOption`
    /// over-decrements the element the container keeps, freeing it prematurely
    /// (a use-after-free: a later alloc reuses the slot and a subsequent
    /// `v[i]` read returns the wrong node). Inc the loaded inner so the callee
    /// holds an independent +1; the container's per-element drop still owns
    /// its own ref. This is the direct-`v[i]`-arg leg of B-2026-07-11-29 (the
    /// #95 shape-DP `clone_offset(shapes[i][j], ..)`); the `let s = v[i]`
    /// workaround already deep-clones via `clone_owned_vec_index_element`, and
    /// a `let`-bound Identifier arg is covered by the Identifier companion.
    /// Nested indices (`m[i][j]` over `Vec[Vec[Option[T]]]`) resolve through
    /// `vec_index_elem_type_expr`'s recursive peel.
    pub(super) fn share_option_shared_index_ref_for_arg(
        &self,
        arg_expr: &Expr,
        val: BasicValueEnum<'ctx>,
    ) {
        let ExprKind::Index { object, index } = &arg_expr.kind else {
            return;
        };
        if matches!(&index.kind, ExprKind::Range { .. }) {
            return;
        }
        let Some(elem_te) = self.vec_index_elem_type_expr(object) else {
            return;
        };
        let Some((_, inner_info)) = self.option_inner_shared_type_for_type_expr(&elem_te) else {
            return;
        };
        self.emit_option_inner_rc_inc_for_loaded(val, inner_info.heap_type);
    }

    /// Bare-`shared struct` sibling of the `share_option_shared_*` push-retain
    /// family (B-2026-07-21-13). When a `Vec[shared T]`/`Vec[Node]` receives a
    /// BARE shared-struct element that is an ALIASING read — an indexed element
    /// `v[i]` or a struct field `n.field`, where the source container/struct
    /// still owns that node — the container co-owns the node and needs an
    /// independent `+1`. A `shared struct` is reference-semantic, so the read
    /// yields the box POINTER (no clone, no inc); without the retain the source's
    /// drop (e.g. a function-local pool `Vec[Node]` whose one node is returned)
    /// frees the node while the container still points at it — use-after-free
    /// (kata #133 Clone Graph's `nodes[i].neighbors.push(nodes[j])`). A fresh
    /// `Node{..}` / call move-out is not a place expression, so it never reaches
    /// here and keeps its sole `+1`. `Option[shared]` and enum elements are
    /// handled by the `share_option_shared_*` siblings and excluded here.
    pub(super) fn share_shared_struct_ref_for_arg(
        &self,
        arg_expr: &Expr,
        val: BasicValueEnum<'ctx>,
    ) {
        let BasicValueEnum::PointerValue(ptr) = val else {
            return;
        };
        // Resolve the pushed element's declared TypeExpr from an aliasing read.
        let elem_te: Option<TypeExpr> = match &arg_expr.kind {
            ExprKind::Index { object, index } if !matches!(index.kind, ExprKind::Range { .. }) => {
                self.vec_index_elem_type_expr(object)
            }
            ExprKind::FieldAccess { object, field } => self
                .shared_type_for_expr(object)
                .and_then(|(tn, _)| {
                    self.type_decls
                        .struct_field_names
                        .get(&tn)
                        .and_then(|ns| ns.iter().position(|n| n == field))
                        .map(|idx| (tn, idx))
                })
                .and_then(|(tn, idx)| {
                    self.type_decls
                        .struct_field_type_exprs
                        .get(&tn)
                        .and_then(|v| v.get(idx))
                        .cloned()
                }),
            _ => None,
        };
        let Some(te) = elem_te else {
            return;
        };
        // `Option[shared T]` is the sibling helpers' job — not this one.
        if self.option_inner_shared_type_for_type_expr(&te).is_some() {
            return;
        }
        // Must name a BARE shared STRUCT (not a shared enum).
        if let TypeKind::Path(p) = &te.kind {
            if let Some(seg) = p.segments.last() {
                if let Some(info) = self.type_decls.shared_types.get(seg.as_str()) {
                    if !info.is_enum {
                        let heap_type = info.heap_type;
                        self.emit_refcount_inc(seg, heap_type, ptr);
                    }
                }
            }
        }
    }

    /// B-2026-07-16-5: true when `e` denotes a BORROWED String/Vec value —
    /// an Identifier naming a `ref` / `mut ref` param whose pointee is the
    /// `{ptr,len,cap}` triple layout, or a field access whose DECLARED
    /// field type is `ref`/`mut ref` to a String/str/Vec/VecDeque. Such an
    /// expression's compiled value carries the LENDER's live `cap`, so any
    /// consumer that stores it into an owned-looking slot must first zero
    /// the cap (see `zero_cap_if_ref_heap_borrow`) or every downstream
    /// cap-guarded free releases a buffer the lender still owns.
    pub(super) fn expr_is_ref_heap_borrow(&self, e: &Expr) -> bool {
        match &e.kind {
            ExprKind::Identifier(n) => self
                .borrow_vars
                .ref_params
                .get(n)
                .is_some_and(|inner| *inner == self.vec_struct_type().into()),
            ExprKind::FieldAccess { object, field } => {
                let obj_name = match &object.kind {
                    ExprKind::Identifier(n) => n.clone(),
                    ExprKind::SelfValue => "self".to_string(),
                    _ => return false,
                };
                let Some(type_name) = self.var_types.var_type_names.get(&obj_name).cloned() else {
                    return false;
                };
                let Some(idx) = self
                    .type_decls
                    .struct_field_names
                    .get(&type_name)
                    .and_then(|names| names.iter().position(|n| n == field))
                else {
                    return false;
                };
                let Some(field_te) = self
                    .type_decls
                    .struct_field_type_exprs
                    .get(&type_name)
                    .and_then(|v| v.get(idx))
                else {
                    return false;
                };
                let inner = match &field_te.kind {
                    TypeKind::Ref(inner) | TypeKind::MutRef(inner) | TypeKind::Frozen(inner) => {
                        inner
                    }
                    _ => return false,
                };
                match &inner.kind {
                    TypeKind::Path(p) => matches!(
                        p.segments.last().map(|s| s.as_str()),
                        Some("String") | Some("str") | Some("Vec") | Some("VecDeque")
                    ),
                    _ => false,
                }
            }
            _ => false,
        }
    }

    /// B-2026-07-16-5 companion: when `arg` is a ref-heap borrow (see
    /// `expr_is_ref_heap_borrow`) and `val` is its materialized
    /// `{ptr,len,cap}` triple, zero the cap word so the stored value is a
    /// read-only VIEW — the same borrow-view discipline as the map
    /// `get(k).unwrap()` family (B-2026-07-14-15 / B-2026-07-15-26): every
    /// cap-guarded free downstream (a match-arm binding cleanup, an enum
    /// payload drop, a struct field drop) skips, and the lender remains the
    /// buffer's sole owner. Reads (`len`, `println`, clone) never consult
    /// `cap`. Pass-through for non-borrow args and non-triple values.
    pub(super) fn zero_cap_if_ref_heap_borrow(
        &self,
        arg: &Expr,
        val: BasicValueEnum<'ctx>,
    ) -> BasicValueEnum<'ctx> {
        if !self.expr_is_ref_heap_borrow(arg) {
            return val;
        }
        let BasicValueEnum::StructValue(sv) = val else {
            return val;
        };
        if sv.get_type() != self.vec_struct_type() {
            return val;
        }
        let zero = self.context.i64_type().const_zero();
        self.builder
            .build_insert_value(sv, zero, 2, "plref.cap0")
            .unwrap()
            .into_struct_value()
            .into()
    }

    pub(super) fn share_option_shared_ref_for_arg(&self, arg_expr: &Expr) {
        let var_name = match &arg_expr.kind {
            ExprKind::Identifier(n) => n.as_str(),
            _ => return,
        };
        let heap_type = match self
            .borrow_vars
            .var_option_shared_heap
            .get(var_name)
            .copied()
        {
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
        let option_ty = self.type_decls.enum_layouts["Option"].llvm_type;
        let i64_t = self.context.i64_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let some_tag = self
            .type_decls
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
        // B-2026-08-27-43 — record that THIS expression node took a leaf
        // retain, so `control_flow_owned_option_shared` can ask the emission
        // what it did instead of re-deriving it from the name env. The env
        // answer is unavailable for an ARM-LOCAL leaf: the arm's frame reverts
        // its bindings before the consuming `let` classifies the RHS. Keyed by
        // span, which identifies the node uniquely across the compilation.
        self.borrow_vars
            .option_shared_leaf_retains
            .borrow_mut()
            .insert((arg_expr.span.offset, arg_expr.span.length), heap_type);
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
        let elem_te = self.var_types.var_elem_type_exprs.get(name.as_str())?;
        let TypeKind::Path(p) = &elem_te.kind else {
            return None;
        };
        let seg = p.segments.last()?;
        let info = self.type_decls.shared_types.get(seg.as_str())?;
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
            // A scalar WIDER than a word splits across words, little-endian
            // (B-2026-08-19-19). The `_` arm below pushes one word, which for
            // an i128 silently dropped the high half AND kept `out.len() ==
            // num_words`, so neither the inline nor the boxing path noticed.
            BasicValueEnum::IntValue(iv) if iv.get_type().get_bit_width() > 64 => {
                let i64_t = self.context.i64_type();
                let wide = iv.get_type();
                out.push(
                    self.builder
                        .build_int_truncate(iv, i64_t, "pl.w.lo")
                        .unwrap(),
                );
                let mut shifted = iv;
                let mut remaining = wide.get_bit_width();
                while remaining > 64 {
                    shifted = self
                        .builder
                        .build_right_shift(shifted, wide.const_int(64, false), false, "pl.w.sh")
                        .unwrap();
                    out.push(
                        self.builder
                            .build_int_truncate(shifted, i64_t, "pl.w.hi")
                            .unwrap(),
                    );
                    remaining -= 64;
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
                .build_call(self.runtime_fns.malloc_fn, &[size.into()], "enumbox")
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
        let option_ty = self.type_decls.enum_layouts["Option"].llvm_type;

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
            // f64 shares the word's 64-bit width — bitcast directly. Narrower
            // floats (f32, f16/bf16) must bitcast at their EXACT width (a
            // float↔int bitcast requires equal widths — B-2026-07-20-11 f32,
            // B-2026-07-20-12 f16/bf16) then zero-extend, the low-bits packing
            // every unpack site expects (`pat.f32.tr/bc`, `col.f32bits`).
            BasicValueEnum::FloatValue(fv) => {
                let bits_ty = self.float_bits_int_type(fv.get_type());
                if bits_ty.get_bit_width() == 64 {
                    Ok(self
                        .builder
                        .build_bit_cast(fv, i64_t, "fcast")
                        .unwrap()
                        .into_int_value())
                } else {
                    let bits = self
                        .builder
                        .build_bit_cast(fv, bits_ty, "fbits")
                        .unwrap()
                        .into_int_value();
                    Ok(self
                        .builder
                        .build_int_z_extend(bits, i64_t, "fbits.zx")
                        .unwrap())
                }
            }
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

    /// The SEEDED enum that owns `name` as a bare constructor, if any —
    /// codegen's mirror of the typechecker's `builtin_variant_owner`.
    ///
    /// B-2026-08-14-10. The bare-name scans below prefer a USER-declared enum
    /// over a seeded one when a variant name collides, and for most names that
    /// is right: a user's `MyIoErr.Other` must not be hijacked by the seeded
    /// `TcpError.Other`. It is NOT right for `Option`/`Result`'s four
    /// constructors, because the typechecker cannot make the same choice there
    /// — measured, a user-declared `None` winning at check time makes even
    /// `let x: Option[i64] = None` a type error, since check-mode does not push
    /// the expected type into a bare constructor. The two phases must agree, so
    /// these four names resolve to the seed on both sides and every other
    /// colliding name keeps the user-first preference it already had.
    ///
    /// A program that means its own variant writes the qualified form
    /// (`MyOption.None`), which resolves by enum name and never reaches a
    /// bare-name scan.
    fn seeded_variant_owner(name: &str) -> Option<&'static str> {
        match name {
            "Some" | "None" => Some("Option"),
            "Ok" | "Err" => Some("Result"),
            _ => None,
        }
    }

    /// Look up a unit enum variant by identifier name and construct its value.
    pub(super) fn try_unit_enum_variant(&self, name: &str) -> Option<BasicValueEnum<'ctx>> {
        // When a variant name (`None` / `Some` / `Ok` / `Err`) collides
        // between a user-defined enum and the seeded built-ins, pick the
        // SEEDED one. HashMap iteration order is non-deterministic otherwise,
        // which is why the two candidates are separated at all.
        //
        // B-2026-08-14-10 REVERSED THE PREFERENCE, and it had to. This used to
        // pick the user-declared enum while the typechecker's own scan picked
        // whichever the hash map yielded first — so on the runs where check
        // typed a bare `None` as `Option[i64]`, codegen constructed the user's
        // one-word `Sink.None` and the module failed the LLVM verifier with
        // `ret i64 0` against `{i64, i64, i64, i64}`. The two must agree, and
        // the typechecker's side is not free to choose: measured, a
        // user-declared `None` winning there makes even `let x: Option[i64] =
        // None` a type error, because check-mode does not push the expected
        // type into a bare constructor. So a user enum declaring `None` would
        // poison every use of the real `Option` in the file.
        //
        // The old rationale — "the wider seeded `Option` layout would
        // mis-construct a value for a user-defined `MyOption.None`" — is
        // answered by the qualified form: `MyOption.None` resolves by enum name
        // through `try_compile_enum_variant` and never reaches this bare-name
        // scan, so a program that means its own variant still gets it.
        let (mut user_pick, mut seed_pick) = (None, None);
        for (enum_name, layout) in &self.type_decls.enum_layouts {
            if let Some(&tag) = layout.tags.get(name) {
                if layout.field_counts.get(name).copied().unwrap_or(0) == 0 {
                    if self.type_decls.seeded_enum_names.contains(enum_name) {
                        seed_pick.get_or_insert((enum_name.clone(), tag, layout));
                    } else {
                        user_pick.get_or_insert((enum_name.clone(), tag, layout));
                    }
                }
            }
        }
        let prefer_seed = Self::seeded_variant_owner(name).is_some() && seed_pick.is_some();
        let (enum_name, tag, layout) = if prefer_seed {
            seed_pick.or(user_pick)?
        } else {
            user_pick.or(seed_pick)?
        };
        let i64_t = self.context.i64_type();

        // Shared enum: heap-allocate.
        if let Some(info) = self.type_decls.shared_types.get(&enum_name) {
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
        // B-2026-07-21-3 (contiguous leg): `Vector[T, N](v[i], v[i+1], …)`
        // over one plain Vec whose element type IS the lane type is a
        // contiguous N-lane region — one vector load beats N scalar loads +
        // N insertelements (the wasm backend does not clean the chain up;
        // Prism's vertical Lanczos pass is the motivating shape).
        if let Some(v) = self.try_compile_vector_adjacent_vec_load(vt, args)? {
            return Ok(v);
        }
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

    /// B-2026-07-21-3: lower `Vector[T, N](v[b], v[b+1], …, v[b+N-1])` — every
    /// lane an index into the SAME plain (non-array-slot) Vec variable whose
    /// element type equals the lane type, at consecutive offsets from a
    /// side-effect-free base index — as ONE `load <N x T>` from the element
    /// pointer at `b`. The per-tap construction shape otherwise emits N
    /// checked scalar loads + N insertelements, which the wasm backend never
    /// re-fuses (measured ~4.7x on Prism's vertical Lanczos pass, where
    /// `Vector[f64, 2](tmp[p], tmp[p+1])` is literally a contiguous f64x2).
    ///
    /// Semantics parity with the scalar chain:
    ///   - base `b` compiles ONCE (identifier / int-literal bases only, and
    ///     the `b + k` offsets are re-derived arithmetically, so no
    ///     side-effect is duplicated or dropped);
    ///   - bounds: `b` is checked first (respecting the BCE-proven halves),
    ///     then `b + N-1` gets the upper check with its lower half proven by
    ///     the base check — the same panic, in the same order, the scalar
    ///     form produces (`v[b]` panics before `v[b+1]` when both are out);
    ///   - the load's alignment is the ELEMENT's, not the vector's — the
    ///     address is only elem-aligned (wasm `v128.load` and native movups
    ///     both take unaligned).
    ///
    /// Returns `Ok(None)` for every non-matching shape (different vars, a
    /// cast lane, non-consecutive offsets, slice/array receivers, mismatched
    /// elem type) — the insertelement chain remains the general path.
    fn try_compile_vector_adjacent_vec_load(
        &mut self,
        vt: inkwell::types::VectorType<'ctx>,
        args: &[CallArg],
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        let n = args.len();
        if n < 2 {
            return Ok(None);
        }
        // Lane 0 shapes the pattern: `vec_var[base]` with a reusable base.
        let ExprKind::Index { object, index } = &args[0].value.kind else {
            return Ok(None);
        };
        let ExprKind::Identifier(vec_var) = &object.kind else {
            return Ok(None);
        };
        if !self.var_types.vec_elem_types.contains_key(vec_var.as_str()) {
            return Ok(None);
        }
        // Array-slot Vec bindings have a distinct representation — mirror the
        // bypass in `ref_arg_index_borrow_ptr`.
        if self
            .variables
            .get(vec_var.as_str())
            .is_some_and(|s| matches!(s.ty, BasicTypeEnum::ArrayType(_)))
        {
            return Ok(None);
        }
        let elem_ty = self.vec_elem_type_for_var(vec_var);
        if elem_ty != vt.get_element_type() {
            return Ok(None);
        }
        // Base must be side-effect-free and reproducible: a bare identifier
        // or an int literal. (A compound base would need its own temp to
        // avoid double-eval; not worth it for the motivating idiom.)
        let base = index.as_ref();
        let base_lit = match &base.kind {
            ExprKind::Identifier(_) => None,
            ExprKind::Integer(c, _) => Some(*c),
            _ => return Ok(None),
        };
        // Lanes 1..N must be `vec_var[base + k]` (or literal `c + k`).
        for (k, arg) in args.iter().enumerate().skip(1) {
            let ExprKind::Index {
                object: obj_k,
                index: idx_k,
            } = &arg.value.kind
            else {
                return Ok(None);
            };
            if !matches!(&obj_k.kind, ExprKind::Identifier(v) if v == vec_var) {
                return Ok(None);
            }
            // `base + k`, in either operand order. The lowering pass rewrites
            // integer `+` into `iN.add(a, b)` (a `Call` on the width's
            // intrinsic path) in index position, so both the surface `Binary`
            // and the desugared `Call` spellings are accepted.
            let ident_plus_lit = |a: &Expr, b_: &Expr| {
                matches!((&a.kind, &base.kind), (ExprKind::Identifier(x), ExprKind::Identifier(y)) if x == y)
                    && matches!(&b_.kind, ExprKind::Integer(c, _) if *c == (k as i64).into())
            };
            let matches_offset = match (&idx_k.kind, base_lit) {
                (ExprKind::Integer(c, _), Some(b)) => *c == b + i128::from(k as i64),
                (
                    ExprKind::Binary {
                        op: BinOp::Add,
                        left,
                        right,
                    },
                    None,
                ) => ident_plus_lit(left, right) || ident_plus_lit(right, left),
                (
                    ExprKind::Call {
                        callee,
                        args: cargs,
                    },
                    None,
                ) => {
                    matches!(
                        &callee.kind,
                        ExprKind::Path { segments, .. }
                            if segments.last().map(|s| s.as_str()) == Some("add")
                                && segments.len() == 2
                                && (segments[0].starts_with('i') || segments[0].starts_with('u'))
                    ) && cargs.len() == 2
                        && (ident_plus_lit(&cargs[0].value, &cargs[1].value)
                            || ident_plus_lit(&cargs[1].value, &cargs[0].value))
                }
                _ => false,
            };
            if !matches_offset {
                return Ok(None);
            }
        }
        let vec_var = vec_var.clone();
        let i64_t = self.context.i64_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let vec_struct = self.vec_struct_type();
        let Some(vec_ptr) = self.get_data_ptr(&vec_var) else {
            return Ok(None);
        };
        let (lower_proven, upper_proven) = self.index_bounds_already_proven(base, &vec_var);
        let idx_raw = self.compile_proven_index_expr(base, lower_proven, upper_proven)?;
        let idx_val = self.coerce_to_i64(idx_raw)?;
        // Base check first (whatever the BCE analysis didn't prove) — same
        // order and panic as the scalar `v[b]`.
        self.emit_split_bounds_check(
            "vsimd.base",
            idx_val,
            vec_struct,
            vec_ptr,
            lower_proven,
            upper_proven,
            Some(elem_ty),
        );
        // Last-lane upper check: `b >= 0` is established past the base check
        // (checked or proven), so only the upper half can fail — exactly the
        // panic the scalar `v[b+N-1]` would raise.
        let last_idx = self
            .builder
            .build_int_add(
                idx_val,
                i64_t.const_int((n - 1) as u64, false),
                "vsimd.last",
            )
            .unwrap();
        self.emit_split_bounds_check(
            "vsimd.last",
            last_idx,
            vec_struct,
            vec_ptr,
            true,
            false,
            Some(elem_ty),
        );
        let data_pp = self
            .builder
            .build_struct_gep(vec_struct, vec_ptr, 0, "v.data.ptr")
            .unwrap();
        let data = self
            .builder
            .build_load(ptr_ty, data_pp, "v.data")
            .unwrap()
            .into_pointer_value();
        // In-bounds: `b` and `b+N-1` are both checked above, and the region
        // between them is contiguous within the same allocation.
        let elem_ptr = unsafe {
            self.builder
                .build_in_bounds_gep(elem_ty, data, &[idx_val], "vsimd.base.ptr")
                .unwrap()
        };
        let loaded = self
            .builder
            .build_load(vt, elem_ptr, "vsimd.load")
            .map_err(|e| format!("Vector adjacent-load failed: {e}"))?;
        // Element alignment, not the vector's natural 16 — the address is
        // only guaranteed elem-aligned.
        let elem_align = match elem_ty {
            BasicTypeEnum::FloatType(ft) => {
                (self.float_bits_int_type(ft).get_bit_width() / 8).max(1)
            }
            BasicTypeEnum::IntType(it) => (it.get_bit_width() / 8).max(1),
            _ => return Ok(None),
        };
        if let Some(inst) = loaded.as_instruction_value() {
            let _ = inst.set_alignment(elem_align);
        }
        Ok(Some(loaded))
    }
}
