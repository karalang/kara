//! Associated / free function call codegen.
//!
//! Houses `compile_assoc_call` — the big dispatch for
//! `Type.assoc_fn(...)` and bare free-function call shapes that
//! aren't methods on an object. Covers every built-in associated
//! function the compiler knows how to lower: `Vec.new` / `Vec.with_capacity`
//! / `Vec.from_array` / `Vec.filled` / `Vec.from_iter`, `Set.new` /
//! `Map.new`, `String.from`, `Channel.new`, `Random.new`, the
//! numeric primitive `cmp` / `from` / `to_*` builders, the slice /
//! array constructors, etc.
//!
//! Lives in a sibling `impl<'ctx> super::Codegen<'ctx>` block.

use crate::ast::*;

use inkwell::module::Linkage;
use inkwell::types::{BasicType, BasicTypeEnum};
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum, IntValue, PointerValue};
use inkwell::AddressSpace;

impl<'ctx> super::Codegen<'ctx> {
    /// Shared source recovery for `Vec.from_slice` / `Vec.try_from_slice`:
    /// resolve the source argument's LLVM element type, its element
    /// `TypeExpr` (drives the RC-aware per-element clone path), and the
    /// `(data, len)` of the source's slice header. Bare-Identifier sources
    /// (slice / vec / array vars) coerce via `coerce_to_slice`; a
    /// `Vec[Vec[T]]` nested-index source (`rows[r]`) compiles the inner Vec
    /// aggregate and extracts its first two fields. Kept in one place so the
    /// fallible companion can't drift from the panicking constructor.
    #[allow(clippy::type_complexity)]
    pub(super) fn recover_from_slice_src(
        &mut self,
        arg: &Expr,
    ) -> Result<
        (
            BasicTypeEnum<'ctx>,
            Option<TypeExpr>,
            PointerValue<'ctx>,
            IntValue<'ctx>,
        ),
        String,
    > {
        // Element type recovery — bare Identifier path first, then
        // nested-Index path for Vec[Vec[T]] sources. Returns
        // (LLVM elem type, optional elem TypeExpr for the RC-clone path,
        // label for diagnostics).
        // `use_coerce`: the source is a slice header recoverable via
        // `coerce_to_slice` — a bare slice/vec/array Identifier, OR a
        // RANGE-slice `arr[a..b]` (which `coerce_to_slice` lowers through
        // `compile_range_slice`). A SCALAR nested index `rows[r]` is the
        // separate Vec[Vec[T]] path and keeps `use_coerce = false`.
        let mut use_coerce = false;
        let (elem_ty, src_elem_te, src_label): (BasicTypeEnum<'ctx>, Option<TypeExpr>, String) =
            match &arg.kind {
                ExprKind::Identifier(src_name) => {
                    use_coerce = true;
                    let t = if let Some(&t) = self.slice_elem_types.get(src_name.as_str()) {
                        t
                    } else if let Some(&t) = self.vec_elem_types.get(src_name.as_str()) {
                        t
                    } else if let Some(slot) = self.variables.get(src_name.as_str()).copied() {
                        if let BasicTypeEnum::ArrayType(at) = slot.ty {
                            at.get_element_type()
                        } else {
                            return Err(format!(
                                "Vec.from_slice: source '{}' is not a slice / vec / array",
                                src_name
                            ));
                        }
                    } else {
                        return Err(format!(
                            "Vec.from_slice: source '{}' not found in scope",
                            src_name
                        ));
                    };
                    let te = self.var_elem_type_exprs.get(src_name.as_str()).cloned();
                    (t, te, src_name.clone())
                }
                // Range-slice `arr[a..b]` — a contiguous window of an
                // array/vec/slice var. Same element type as the outer var;
                // `coerce_to_slice` builds the `{data, len}` header (via
                // `compile_range_slice`). Distinct from the scalar nested
                // index below: a `Range` index slices, a scalar index selects
                // one (inner-Vec) element. Without this arm, `Vec.from_slice(
                // buf[0..n])` mis-routed to the Vec[Vec[T]] path and errored.
                ExprKind::Index { object, index }
                    if matches!(index.kind, ExprKind::Range { .. }) =>
                {
                    use_coerce = true;
                    let ExprKind::Identifier(outer_name) = &object.kind else {
                        return Err(
                            "Vec.from_slice: range-slice source must root at a named variable"
                                .to_string(),
                        );
                    };
                    let t = if let Some(&t) = self.slice_elem_types.get(outer_name.as_str()) {
                        t
                    } else if let Some(&t) = self.vec_elem_types.get(outer_name.as_str()) {
                        t
                    } else if let Some(slot) = self.variables.get(outer_name.as_str()).copied() {
                        if let BasicTypeEnum::ArrayType(at) = slot.ty {
                            at.get_element_type()
                        } else {
                            return Err(format!(
                                "Vec.from_slice: range-slice source '{}' is not a slice / vec / array",
                                outer_name
                            ));
                        }
                    } else {
                        return Err(format!(
                            "Vec.from_slice: range-slice source '{}' not found in scope",
                            outer_name
                        ));
                    };
                    let te = self.var_elem_type_exprs.get(outer_name.as_str()).cloned();
                    (t, te, format!("{outer_name}[..]"))
                }
                ExprKind::Index {
                    object: outer,
                    index: _,
                } => {
                    let ExprKind::Identifier(outer_name) = &outer.kind else {
                        return Err(
                            "Vec.from_slice: nested-index source must root at a named variable"
                                .to_string(),
                        );
                    };
                    let inner_te = self
                        .var_elem_type_exprs
                        .get(outer_name.as_str())
                        .and_then(super::helpers::vec_inner_type_expr)
                        .ok_or_else(|| {
                            format!(
                                "Vec.from_slice: nested-index source `{outer_name}[i]` requires \
                                 outer to be Vec[Vec[T]]"
                            )
                        })?;
                    (
                        self.llvm_type_for_type_expr(&inner_te),
                        Some(inner_te),
                        format!("{outer_name}[i]"),
                    )
                }
                _ => {
                    return Err(
                        "Vec.from_slice: source must be a named slice / vec / array variable, \
                         or a nested index expression on a Vec[Vec[T]]"
                            .to_string(),
                    );
                }
            };

        // Get src {data, len}. Identifier path uses `coerce_to_slice`;
        // Index path compiles the expression directly to get the inner Vec
        // aggregate value and extracts its first two fields (same fallback
        // shape as `extend_from_slice`).
        let (src_data, src_len) = if use_coerce {
            let slice_val = self.coerce_to_slice(arg, elem_ty)?.ok_or_else(|| {
                format!(
                    "Vec.from_slice: could not coerce '{}' to a slice header",
                    src_label
                )
            })?;
            let slice_sv = slice_val.into_struct_value();
            let data = self
                .builder
                .build_extract_value(slice_sv, 0, "from_slice.src.data")
                .unwrap()
                .into_pointer_value();
            let len = self
                .builder
                .build_extract_value(slice_sv, 1, "from_slice.src.len")
                .unwrap()
                .into_int_value();
            (data, len)
        } else {
            let compiled = self.compile_expr(arg)?;
            let BasicValueEnum::StructValue(sv) = compiled else {
                return Err(format!(
                    "Vec.from_slice: nested-index source did not produce a struct value (got {compiled:?})"
                ));
            };
            let n_fields = sv.get_type().count_fields();
            if n_fields != 2 && n_fields != 3 {
                return Err(format!(
                    "Vec.from_slice: source struct has {n_fields} fields; expected 2 (Slice) or 3 (Vec)"
                ));
            }
            let data = self
                .builder
                .build_extract_value(sv, 0, "from_slice.src.data")
                .unwrap()
                .into_pointer_value();
            let len = self
                .builder
                .build_extract_value(sv, 1, "from_slice.src.len")
                .unwrap()
                .into_int_value();
            (data, len)
        };
        let _ = src_label; // retained for diagnostic clarity in errors above
        Ok((elem_ty, src_elem_te, src_data, src_len))
    }

    /// Copy `src_len` elements from `src_data` into the freshly-allocated
    /// `new_buf` for `Vec.from_slice` / `Vec.try_from_slice`. Branches on
    /// element triviality: a single `memcpy` of `alloc_bytes` for primitives,
    /// or a per-element `synth_clone` loop for anything carrying a heap
    /// pointer (String / Vec / Map / Set / shared T / nested aggregates).
    /// Without the clone path a `Vec[String]` / `Vec[Vec[T]]` source
    /// bit-copies the aggregate values and both src and dst alias the same
    /// inner heap pointers → double-free at scope exit (ASAN-flagged in
    /// `tests/memory_sanitizer.rs::asan_vec_from_slice_string_elements_independent`).
    /// Builder is left positioned after the copy (the clone-loop exit BB).
    pub(super) fn copy_from_slice_elems(
        &mut self,
        elem_ty: BasicTypeEnum<'ctx>,
        src_elem_te: &Option<TypeExpr>,
        src_data: PointerValue<'ctx>,
        new_buf: PointerValue<'ctx>,
        src_len: IntValue<'ctx>,
        alloc_bytes: IntValue<'ctx>,
    ) {
        let trivial = src_elem_te
            .as_ref()
            .map(super::vec_method::is_trivially_copyable_te)
            .unwrap_or(true);
        if trivial {
            self.builder
                .build_memcpy(new_buf, 8, src_data, 8, alloc_bytes)
                .unwrap();
            return;
        }
        let elem_te = src_elem_te.as_ref().unwrap();
        let clone_fn = self.emit_clone_fn_for_type_expr(elem_te);
        let i64_t = self.context.i64_type();
        let fn_val = self.current_fn.unwrap();
        let loop_cond_bb = self
            .context
            .append_basic_block(fn_val, "from_slice.clone.cond");
        let loop_body_bb = self
            .context
            .append_basic_block(fn_val, "from_slice.clone.body");
        let loop_exit_bb = self
            .context
            .append_basic_block(fn_val, "from_slice.clone.exit");
        let i_alloca = self.create_entry_alloca(fn_val, "from_slice.clone.i", i64_t.into());
        self.builder
            .build_store(i_alloca, i64_t.const_zero())
            .unwrap();
        self.builder
            .build_unconditional_branch(loop_cond_bb)
            .unwrap();

        self.builder.position_at_end(loop_cond_bb);
        let i_cur = self
            .builder
            .build_load(i64_t, i_alloca, "from_slice.clone.i.cur")
            .unwrap()
            .into_int_value();
        let cond = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::ULT,
                i_cur,
                src_len,
                "from_slice.clone.lt",
            )
            .unwrap();
        self.builder
            .build_conditional_branch(cond, loop_body_bb, loop_exit_bb)
            .unwrap();

        self.builder.position_at_end(loop_body_bb);
        let src_ep = unsafe {
            self.builder
                .build_gep(elem_ty, src_data, &[i_cur], "from_slice.clone.src.ep")
                .unwrap()
        };
        let dst_ep = unsafe {
            self.builder
                .build_gep(elem_ty, new_buf, &[i_cur], "from_slice.clone.dst.ep")
                .unwrap()
        };
        self.builder
            .build_call(clone_fn, &[src_ep.into(), dst_ep.into()], "")
            .unwrap();
        let one = i64_t.const_int(1, false);
        let i_next = self
            .builder
            .build_int_add(i_cur, one, "from_slice.clone.i.next")
            .unwrap();
        self.builder.build_store(i_alloca, i_next).unwrap();
        self.builder
            .build_unconditional_branch(loop_cond_bb)
            .unwrap();

        self.builder.position_at_end(loop_exit_bb);
    }

    /// Recover the LLVM element type of the collection inside a
    /// `Result[Vec[T] | VecDeque[T], _]` annotation — the Ok-payload `T` a
    /// `Vec.try_with_capacity(n)` RHS needs but its zero-arg signature can't
    /// supply. Used at the `let` site (`stmts.rs`) to seed
    /// `pending_let_elem_type` when the destination binds the fallible
    /// constructor's `Result` directly (the match form), where the bare
    /// `vec_elem_types[var]` lookup sees a `Result`, not a `Vec`.
    /// (phase-8-stdlib-floor item 8.)
    pub(super) fn result_ok_collection_elem_type(
        &self,
        te: &TypeExpr,
    ) -> Option<BasicTypeEnum<'ctx>> {
        let TypeKind::Path(path) = &te.kind else {
            return None;
        };
        if path.segments.first().map(|s| s.as_str()) != Some("Result") {
            return None;
        }
        let args = path.generic_args.as_ref()?;
        let GenericArg::Type(ok_te) = args.first()? else {
            return None;
        };
        super::helpers::vec_inner_type_expr(ok_te).map(|elem| self.llvm_type_for_type_expr(&elem))
    }

    /// The built-in `default()` value for a primitive or `String` type name,
    /// or `None` for any other type (which dispatches to its own `Type.default`
    /// function instead). Scalars are the properly-typed zero (`llvm_type_for_name`
    /// picks the native width — `i32` for `char`, `i8` for `i8`, etc.); `String`
    /// is the empty `{ptr, len, cap}` header built exactly like an empty string
    /// literal (valid data ptr, `len = 0`, `cap = 0` so scope-exit drop's
    /// `cap > 0` guard no-ops). Mirrors the interpreter's `primitive_default_value`
    /// and the derived-Default field initializers in `desugar.rs`. B-2026-07-08-25.
    fn primitive_default_value(&self, type_name: &str) -> Option<BasicValueEnum<'ctx>> {
        match type_name {
            "String" => {
                // Empty String: valid ptr to a NUL global, len 0, cap 0.
                // Matches the `ExprKind::StringLit("")` lowering so println /
                // drop treat it exactly like `"".to_string()`.
                let data_ptr = self.build_str_bytes_global(&[], "str.default");
                let str_ty = self.vec_struct_type();
                let i64_t = self.context.i64_type();
                let zero = i64_t.const_int(0, false);
                let mut agg = str_ty.get_undef();
                agg = self
                    .builder
                    .build_insert_value(agg, data_ptr, 0, "str.default.data")
                    .unwrap()
                    .into_struct_value();
                agg = self
                    .builder
                    .build_insert_value(agg, zero, 1, "str.default.len")
                    .unwrap()
                    .into_struct_value();
                agg = self
                    .builder
                    .build_insert_value(agg, zero, 2, "str.default.cap")
                    .unwrap()
                    .into_struct_value();
                Some(agg.into())
            }
            "i8" | "i16" | "i32" | "i64" | "isize" | "u8" | "u16" | "u32" | "u64" | "usize" => {
                match self.llvm_type_for_name(type_name) {
                    BasicTypeEnum::IntType(t) => Some(t.const_zero().into()),
                    _ => None,
                }
            }
            "f32" | "f64" => match self.llvm_type_for_name(type_name) {
                BasicTypeEnum::FloatType(t) => Some(t.const_zero().into()),
                _ => None,
            },
            "bool" | "char" => match self.llvm_type_for_name(type_name) {
                BasicTypeEnum::IntType(t) => Some(t.const_zero().into()),
                _ => None,
            },
            _ => None,
        }
    }

    pub(super) fn compile_assoc_call(
        &mut self,
        type_name: &str,
        method: &str,
        _args: &[CallArg],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let args = _args;
        // B-2026-07-08-25 — inside a monomorph body, a leading GENERIC type
        // param (`T.default()`, `T.new()`, `T.from(x)`) must dispatch at the
        // CONCRETE instantiation. Resolve `type_name` through the mono's
        // name-level substitution (`type_subst_names`, set by
        // `compile_generic_call`) so every `Type.method` lookup below —
        // including the user-impl `module.get_function("Type.method")` — keys
        // on the real type (`S.default`), not the erased param name (`T`),
        // which matches nothing and falls through to the silent `const 0`
        // default (miscompiling a generic `T.default()` to a 0-return, e.g.
        // std.mem `take[T: Default]`). No-op outside a mono / for concrete
        // types (not in `type_subst_names`); a param bound to a primitive with
        // no `<prim>.default` still falls through as before (no regression).
        let resolved_type_name = self.type_subst_names.get(type_name).cloned();
        let type_name = resolved_type_name.as_deref().unwrap_or(type_name);
        // Fallible-allocation constructor companions (phase-8-stdlib-floor
        // item 2) are interpreter-only in v1 — their codegen lowering (runtime
        // allocator wrappers) is item 8 (Phase 7). Reject at `karac build` with
        // a clear, actionable message; without this the unrecognized
        // `Type.try_<base>` would fall through to this function's silent
        // `Ok(const 0)` default and miscompile to a constant.
        if let Some(base) = crate::fallible_alloc::static_companion_base(method) {
            if matches!(type_name, "Vec" | "VecDeque" | "String")
                && !crate::fallible_alloc::static_companion_has_codegen(type_name, method)
            {
                return Err(format!(
                    "codegen: fallible-allocation constructor `{type_name}.{method}(...)` is \
                     interpreter-only in v1; its codegen lowering is phase-8-stdlib-floor item 8. \
                     Run under `karac run`, or use the panicking `{type_name}.{base}(...)` \
                     constructor under `karac build`."
                ));
            }
        }
        // `std.encoding` (`Base64` / `Hex` / `Url` encode/decode) has no codegen
        // lowering yet — the `#[compiler_builtin]` bodies are stubs and only the
        // interpreter implements them. Without this reject the unrecognized
        // `Base64.encode(...)` etc. fell through to this function's silent
        // `Ok(const 0)` tail and MISCOMPILED to the integer 0 (a `String`
        // function returning `0` — a silent run-vs-build divergence,
        // B-2026-07-18-20). Reject with an actionable message until the codegen
        // lowering lands; the interpreter (`karac run`) computes them correctly.
        if matches!(type_name, "Base64" | "Hex" | "Url")
            && matches!(
                method,
                "encode" | "encode_url_safe" | "encode_upper" | "decode"
            )
        {
            return Err(format!(
                "codegen: `{type_name}.{method}(...)` (std.encoding) is interpreter-only in v1 — \
                 its codegen lowering is not implemented yet (tracked: B-2026-07-18-20). Run \
                 under `karac run`. Emitting it under `karac build` would silently return 0."
            ));
        }
        // Phase 11 numerical stdlib — Tensor constructors. zeros/ones/
        // full thread the destination binding's element type + static
        // dims via `pending_let_tensor_info` (the `Vec.with_capacity`
        // expected-type mechanism); `from` is fully self-contained
        // (dims from the literal's nesting, element type from the first
        // leaf). See `src/codegen/tensor.rs`.
        if type_name == "Tensor" {
            match method {
                "zeros" | "ones" | "full" => return self.compile_tensor_new(method, args),
                "from" => return self.compile_tensor_from(args),
                _ => {}
            }
        }
        // Phase 11 data-science stdlib — Column constructors. `new` /
        // `with_capacity` carry no element value in their args, so they
        // thread the destination binding's element type via
        // `pending_let_column_info` (the `Tensor.zeros` mechanism);
        // `from_vec` deep-copies the Vec argument; `from_iter_nullable`
        // scatters a `Vec[Option[T]]` into values + validity bitmap. See
        // `src/codegen/column.rs`.
        if type_name == "Column" {
            match method {
                "new" | "with_capacity" | "from_vec" | "from_iter_nullable" => {
                    return self.compile_column_new(method, args)
                }
                _ => {}
            }
        }
        // `DataFrame.new()` — a fresh empty table. See
        // `src/codegen/dataframe.rs`.
        if type_name == "DataFrame" && method == "read_csv" {
            // Phase-11 CSV leg — the codegen twin (runtime-side parse +
            // frame construction; see `compile_dataframe_read_csv`).
            return self.compile_dataframe_read_csv(&_args[0].value);
        }
        if type_name == "DataFrame" && method == "new" {
            return self.compile_dataframe_new();
        }
        // Phase 6 line 218 slice 5: `TaskGroup.new()` — allocate a
        // runtime-side group via `karac_runtime_taskgroup_new()` and
        // wrap the returned pointer (cast to i64) as `TaskGroup { id: <i64> }`.
        // The slice-1 stdlib stub body returns `TaskGroup { id: 0 }`;
        // this intercept replaces that lowering with the FFI call so
        // `tg.spawn(...)` (slice 5 child-registration) and `tg`'s
        // implicit `Drop` (slice 5 wait-for-children) can find a real
        // scheduler-side container at the pointer.
        if type_name == "TaskGroup" && method == "new" && _args.is_empty() {
            let new_fn = self
                .module
                .get_function("karac_runtime_taskgroup_new")
                .expect("karac_runtime_taskgroup_new declared in Codegen::new");
            let call = self
                .builder
                .build_call(new_fn, &[], "__taskgroup_new")
                .unwrap();
            let group_ptr = call
                .try_as_basic_value()
                .unwrap_basic()
                .into_pointer_value();
            let i64_ty = self.context.i64_type();
            let id = self
                .builder
                .build_ptr_to_int(group_ptr, i64_ty, "taskgroup.id")
                .unwrap();
            let group_struct_ty = self.context.struct_type(&[i64_ty.into()], false);
            let undef = group_struct_ty.get_undef();
            let result = self
                .builder
                .build_insert_value(undef, id, 0, "task_group")
                .unwrap()
                .into_struct_value();
            return Ok(result.into());
        }

        // `BoundedChannel.new(capacity, on_full)` — allocate a runtime
        // bounded queue via `karac_runtime_bounded_channel_new` and wrap the
        // returned pointer (cast to i64) as `BoundedChannel { handle_id:
        // <i64> }`. The stdlib stub body returns `BoundedChannel { handle_id:
        // 0 }`; this intercept replaces it with the FFI so `.send`/`.recv`
        // (and the `BoundedChannel` Drop) find a real queue at the pointer.
        // `on_full` is v1-collapsed to fail-fast (both `Block` and `FailFast`
        // fail a full send) — the runtime accepts the discriminant for
        // forward-compat with parking-on-full but ignores it, so codegen
        // passes a constant `0` rather than lowering the `OnFull` enum value.
        if type_name == "BoundedChannel" && method == "new" && _args.len() == 2 {
            let capacity = self.compile_expr(&_args[0].value)?.into_int_value();
            let on_full = self.context.i8_type().const_zero();
            let new_fn = self
                .module
                .get_function("karac_runtime_bounded_channel_new")
                .expect("karac_runtime_bounded_channel_new declared in Codegen::new");
            let ch_ptr = self
                .builder
                .build_call(
                    new_fn,
                    &[capacity.into(), on_full.into()],
                    "__bounded_channel_new",
                )
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_pointer_value();
            let i64_ty = self.context.i64_type();
            let handle = self
                .builder
                .build_ptr_to_int(ch_ptr, i64_ty, "bch.handle")
                .unwrap();
            let struct_ty = self.context.struct_type(&[i64_ty.into()], false);
            let result = self
                .builder
                .build_insert_value(struct_ty.get_undef(), handle, 0, "bounded_channel")
                .unwrap()
                .into_struct_value();
            return Ok(result.into());
        }

        // `Regex.compile(pattern: String) -> Result[Regex, RegexError]`
        // (B-2026-07-14-19) — the AOT backend for `runtime/stdlib/regex.kara`'s
        // `#[compiler_builtin]` stub, matching the interpreter's
        // `RustRegex::new` path. Validate via the runtime `karac_regex_validate`
        // (from the opt-in `libkarac_runtime_regex.a`); Ok wraps an owned COPY
        // of the pattern as `Regex { pattern }` (copy, not move — the arg temp
        // keeps its own drop), Err yields `RegexError { message }` with a static
        // message. `Regex` and `RegexError` are single-`String`-field newtypes,
        // so a `String` value coerces straight into each variant's 3-word
        // payload. (Exact regex-crate error-string parity for an INVALID
        // pattern is a later slice; the repro/oracle use valid patterns.)
        if type_name == "Regex" && method == "compile" {
            if _args.len() != 1 {
                return Err(format!(
                    "Regex.compile expects 1 argument (a pattern String), got {}",
                    _args.len()
                ));
            }
            let i8_t = self.context.i8_type();
            let i64_t = self.context.i64_type();
            let pat_val = self.compile_expr(&_args[0].value)?;
            let pat_sv = pat_val.into_struct_value();
            let pat_data = self
                .builder
                .build_extract_value(pat_sv, 0, "rx.pat.data")
                .unwrap()
                .into_pointer_value();
            let pat_len = self
                .builder
                .build_extract_value(pat_sv, 1, "rx.pat.len")
                .unwrap()
                .into_int_value();

            let validate_fn = self
                .module
                .get_function("karac_regex_validate")
                .expect("karac_regex_validate declared in Codegen::new");
            let valid = self
                .builder
                .build_call(validate_fn, &[pat_data.into(), pat_len.into()], "rx.valid")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_int_value();
            let is_valid = self
                .builder
                .build_int_compare(
                    inkwell::IntPredicate::NE,
                    valid,
                    i8_t.const_zero(),
                    "rx.valid.bool",
                )
                .unwrap();

            let fn_val = self.current_fn.unwrap();
            let ok_bb = self.context.append_basic_block(fn_val, "rx.compile.ok");
            let err_bb = self.context.append_basic_block(fn_val, "rx.compile.err");
            let merge_bb = self.context.append_basic_block(fn_val, "rx.compile.merge");
            self.builder
                .build_conditional_branch(is_valid, ok_bb, err_bb)
                .unwrap();

            // Ok(Regex { pattern: <owned copy> }).
            self.builder.position_at_end(ok_bb);
            let pat_copy = self.emit_vecstr_defensive_copy(pat_val, i8_t.into(), None);
            let ok_result = self.build_nonshared_enum_value("Result", "Ok", &[pat_copy])?;
            let ok_end = self.builder.get_insert_block().unwrap();
            self.builder.build_unconditional_branch(merge_bb).unwrap();

            // Err(RegexError { message: "invalid regular expression" }) — a
            // cap=0 static String (never freed → no leak, no double-free).
            self.builder.position_at_end(err_bb);
            let msg = "invalid regular expression";
            let msg_ptr = self.build_str_bytes_global(msg.as_bytes(), "rx.err.msg");
            let str_ty = self.vec_struct_type();
            let mut msg_val = str_ty.get_undef();
            msg_val = self
                .builder
                .build_insert_value(msg_val, msg_ptr, 0, "rx.err.msg.ptr")
                .unwrap()
                .into_struct_value();
            msg_val = self
                .builder
                .build_insert_value(
                    msg_val,
                    i64_t.const_int(msg.len() as u64, false),
                    1,
                    "rx.err.msg.len",
                )
                .unwrap()
                .into_struct_value();
            msg_val = self
                .builder
                .build_insert_value(msg_val, i64_t.const_zero(), 2, "rx.err.msg.cap")
                .unwrap()
                .into_struct_value();
            let err_result = self.build_nonshared_enum_value("Result", "Err", &[msg_val.into()])?;
            let err_end = self.builder.get_insert_block().unwrap();
            self.builder.build_unconditional_branch(merge_bb).unwrap();

            // Merge the two `Result` aggregates.
            self.builder.position_at_end(merge_bb);
            let phi = self
                .builder
                .build_phi(ok_result.get_type(), "rx.compile.result")
                .unwrap();
            phi.add_incoming(&[(&ok_result, ok_end), (&err_result, err_end)]);
            return Ok(phi.as_basic_value());
        }

        // Phase 6 "Channel AOT codegen lowering": `Channel.new()` — allocate
        // a runtime channel (refcount 2) and return it as the `(Sender[T],
        // Receiver[T])` tuple. Both ends carry the *same* opaque pointer
        // (mirroring the interpreter's `Arc::clone` of one queue); the
        // refcount-2 from `channel_new` accounts for the two scope-exit
        // `DropChannelEnd` cleanups the destructured `tx`/`rx` bindings emit.
        // Element type erases here — it travels per send/recv call — so this
        // path needs no generic-arg info.
        if type_name == "Channel" && method == "new" && _args.is_empty() {
            let new_fn = self
                .module
                .get_function("karac_runtime_channel_new")
                .expect("karac_runtime_channel_new declared in Codegen::new");
            let call = self
                .builder
                .build_call(new_fn, &[], "__channel_new")
                .unwrap();
            let ch_ptr = call
                .try_as_basic_value()
                .unwrap_basic()
                .into_pointer_value();
            let ptr_ty = self.context.ptr_type(AddressSpace::default());
            let tuple_ty = self
                .context
                .struct_type(&[ptr_ty.into(), ptr_ty.into()], false);
            let undef = tuple_ty.get_undef();
            let with_sender = self
                .builder
                .build_insert_value(undef, ch_ptr, 0, "channel.sender")
                .unwrap();
            let pair = self
                .builder
                .build_insert_value(with_sender, ch_ptr, 1, "channel.pair")
                .unwrap()
                .into_struct_value();
            return Ok(pair.into());
        }

        // `OnceLock.new()` / `OnceCell.new()` — allocate an empty write-once
        // cell and return its opaque `*mut KaracOnce` handle, stored directly
        // in the binding's slot. Element type erases here (it travels per
        // `set`/`get` call via `once_var_types`), so no generic-arg info is
        // needed. A local binding's scope-exit `FreeOnceHandle` frees it (a
        // module-level binding lives for the process). B-8 OnceLock codegen.
        if (type_name == "OnceLock" || type_name == "OnceCell")
            && method == "new"
            && _args.is_empty()
        {
            let new_fn = self
                .module
                .get_function("karac_runtime_once_new")
                .expect("karac_runtime_once_new declared in Codegen::new");
            let handle = self
                .builder
                .build_call(new_fn, &[], "__once_new")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic();
            return Ok(handle);
        }

        // `Interner.new()` — allocate an empty string interner and return its
        // opaque `*mut KaracInterner` handle, stored directly in the binding's
        // slot (non-generic — the payloads are always byte strings, `Symbol`
        // erases to `i64`). A local binding's scope-exit `FreeInternerHandle`
        // frees it. Phase-8 Interner codegen.
        if type_name == "Interner" && method == "new" && _args.is_empty() {
            let new_fn = self
                .module
                .get_function("karac_runtime_interner_new")
                .expect("karac_runtime_interner_new declared in Codegen::new");
            let handle = self
                .builder
                .build_call(new_fn, &[], "__interner_new")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic();
            return Ok(handle);
        }

        // `Arena.new()` — allocate an empty blob arena and return its opaque
        // `*mut KaracArena` handle, stored directly in the binding's slot
        // (the element type `T` lives codegen-side, recorded from the
        // binding's `Arena[T]` annotation). A local binding's scope-exit
        // `FreeArenaHandle` frees it. Phase-8 Arena codegen.
        if type_name == "Arena" && method == "new" && _args.is_empty() {
            let new_fn = self
                .module
                .get_function("karac_runtime_arena_new")
                .expect("karac_runtime_arena_new declared in Codegen::new");
            let handle = self
                .builder
                .build_call(new_fn, &[], "__arena_new")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic();
            return Ok(handle);
        }

        // Numeric primitive From: `T.from(x)` for integer/float widening.
        // Codegen currently represents all ints as LLVM i64 and floats as
        // f64, so widening is a passthrough at this layer. When narrower
        // int types gain LLVM representation, this branch needs sext/zext.
        if method == "from"
            && matches!(
                type_name,
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
            )
        {
            if let Some(arg) = _args.first() {
                return self.compile_expr(&arg.value);
            }
        }
        // Numeric narrowing `T.try_from(x) -> Result[T, String]` in path-call
        // form — this is what the `.try_into()` desugar lowers to
        // (`x.try_into()` → `Call(Path([T, try_from]))`). The identifier-receiver
        // form (`T.try_from(x)`) is handled in `compile_method_call`; both route
        // to the same `compile_numeric_try_from`.
        if method == "try_from"
            && matches!(
                type_name,
                "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64" | "usize"
            )
        {
            return self.compile_numeric_try_from(type_name, _args);
        }
        // `<int_type>.parse(s: String) -> Option[i64]` — base-10 signed
        // parse via the `karac_runtime_parse_i64` extern. Returns
        // `Option.Some(value)` on success, `Option.None` on failure
        // (rejects empty / non-numeric / overflow). Trims whitespace
        // before parsing.
        if method == "parse"
            && matches!(
                type_name,
                "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64" | "usize"
            )
        {
            if _args.is_empty() {
                return Err(format!("{}.parse requires a String argument", type_name));
            }
            let i64_t = self.context.i64_type();
            let i8_t = self.context.i8_type();
            let ptr_ty = self.context.ptr_type(AddressSpace::default());

            // Evaluate the String arg, extract `{data, len}`.
            let s_val = self.compile_expr(&_args[0].value)?;
            let s_struct = s_val.into_struct_value();
            let s_data = self
                .builder
                .build_extract_value(s_struct, 0, "parse.s.ptr")
                .unwrap()
                .into_pointer_value();
            let s_len = self
                .builder
                .build_extract_value(s_struct, 1, "parse.s.len")
                .unwrap()
                .into_int_value();

            // Allocate the out-i64 slot the runtime writes through.
            let fn_val = self
                .current_fn
                .ok_or_else(|| "T.parse called outside fn".to_string())?;
            let out_slot = self.create_entry_alloca(fn_val, "parse.out", i64_t.into());

            // Call the runtime extern.
            let parse_fn = self
                .module
                .get_function("karac_runtime_parse_i64")
                .expect("karac_runtime_parse_i64 declared in Codegen::new");
            let success = self
                .builder
                .build_call(
                    parse_fn,
                    &[s_data.into(), s_len.into(), out_slot.into()],
                    "parse.ok",
                )
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_int_value();
            let is_ok = self
                .builder
                .build_int_compare(
                    inkwell::IntPredicate::NE,
                    success,
                    i8_t.const_zero(),
                    "parse.ok.bool",
                )
                .unwrap();

            // Branch on success: load the parsed value in the some
            // branch; the none branch holds no payload.
            let some_bb = self.context.append_basic_block(fn_val, "parse.some");
            let none_bb = self.context.append_basic_block(fn_val, "parse.none");
            let merge_bb = self.context.append_basic_block(fn_val, "parse.merge");

            self.builder
                .build_conditional_branch(is_ok, some_bb, none_bb)
                .unwrap();

            // Some: load *out, coerce to 3-word payload, branch to merge.
            self.builder.position_at_end(some_bb);
            let parsed = self
                .builder
                .build_load(i64_t, out_slot, "parse.value")
                .unwrap();
            let some_payload_words = self.coerce_to_payload_words(parsed, 3)?;
            let some_end_bb = self.builder.get_insert_block().unwrap();
            self.builder.build_unconditional_branch(merge_bb).unwrap();

            // None: just branch to merge.
            self.builder.position_at_end(none_bb);
            let none_end_bb = self.builder.get_insert_block().unwrap();
            self.builder.build_unconditional_branch(merge_bb).unwrap();

            // Merge: PHI-assemble Option[i64].
            self.builder.position_at_end(merge_bb);
            // Suppress the unused-warning on `ptr_ty` when the helper
            // closure doesn't touch it directly. (The build above
            // consumes only the existing locals.)
            let _ = ptr_ty;
            let agg = self.build_option_some_via_phis(
                &some_payload_words,
                some_end_bb,
                none_end_bb,
                "parse.opt",
            );
            return Ok(agg);
        }
        // `<int_type>.from_str_radix(s: String, radix: u32) -> Option[i64]` —
        // radix parse (2..=36) via the `karac_runtime_parse_i64_radix` extern.
        // Mirrors the `parse` arm above; the self-hosting lexer's hex/binary/
        // octal literal path (phase-12-self-hosting.md).
        if method == "from_str_radix"
            && matches!(
                type_name,
                "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64" | "usize"
            )
        {
            if _args.len() < 2 {
                return Err(format!(
                    "{}.from_str_radix requires a String and a radix argument",
                    type_name
                ));
            }
            let i64_t = self.context.i64_type();
            let i8_t = self.context.i8_type();
            let i32_t = self.context.i32_type();

            // String arg → {data, len}.
            let s_val = self.compile_expr(&_args[0].value)?;
            let s_struct = s_val.into_struct_value();
            let s_data = self
                .builder
                .build_extract_value(s_struct, 0, "radix.s.ptr")
                .unwrap()
                .into_pointer_value();
            let s_len = self
                .builder
                .build_extract_value(s_struct, 1, "radix.s.len")
                .unwrap()
                .into_int_value();

            // radix arg → i32 (the source value is i64-backed; truncate).
            let radix_val = self.compile_expr(&_args[1].value)?.into_int_value();
            let radix_i32 = self
                .builder
                .build_int_truncate(radix_val, i32_t, "radix.r")
                .unwrap();

            let fn_val = self
                .current_fn
                .ok_or_else(|| "T.from_str_radix called outside fn".to_string())?;
            let out_slot = self.create_entry_alloca(fn_val, "radix.out", i64_t.into());

            let parse_fn = self
                .module
                .get_function("karac_runtime_parse_i64_radix")
                .expect("karac_runtime_parse_i64_radix declared in Codegen::new");
            let success = self
                .builder
                .build_call(
                    parse_fn,
                    &[
                        s_data.into(),
                        s_len.into(),
                        radix_i32.into(),
                        out_slot.into(),
                    ],
                    "radix.ok",
                )
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_int_value();
            let is_ok = self
                .builder
                .build_int_compare(
                    inkwell::IntPredicate::NE,
                    success,
                    i8_t.const_zero(),
                    "radix.ok.bool",
                )
                .unwrap();

            let some_bb = self.context.append_basic_block(fn_val, "radix.some");
            let none_bb = self.context.append_basic_block(fn_val, "radix.none");
            let merge_bb = self.context.append_basic_block(fn_val, "radix.merge");

            self.builder
                .build_conditional_branch(is_ok, some_bb, none_bb)
                .unwrap();

            self.builder.position_at_end(some_bb);
            let parsed = self
                .builder
                .build_load(i64_t, out_slot, "radix.value")
                .unwrap();
            let some_payload_words = self.coerce_to_payload_words(parsed, 3)?;
            let some_end_bb = self.builder.get_insert_block().unwrap();
            self.builder.build_unconditional_branch(merge_bb).unwrap();

            self.builder.position_at_end(none_bb);
            let none_end_bb = self.builder.get_insert_block().unwrap();
            self.builder.build_unconditional_branch(merge_bb).unwrap();

            self.builder.position_at_end(merge_bb);
            let agg = self.build_option_some_via_phis(
                &some_payload_words,
                some_end_bb,
                none_end_bb,
                "radix.opt",
            );
            return Ok(agg);
        }
        // `f64.parse(s: String) -> Option[f64]` — float parse via the
        // `karac_runtime_parse_f64` extern. Mirrors the int `parse` arm; the
        // some-payload is the parsed f64 (bitcast to a word by
        // `coerce_to_payload_words`). The self-hosting lexer's float-literal
        // path. (f32.parse deferred — its narrower payload needs its own path.)
        if method == "parse" && type_name == "f64" {
            if _args.is_empty() {
                return Err(format!("{}.parse requires a String argument", type_name));
            }
            let i8_t = self.context.i8_type();
            let f64_t = self.context.f64_type();

            let s_val = self.compile_expr(&_args[0].value)?;
            let s_struct = s_val.into_struct_value();
            let s_data = self
                .builder
                .build_extract_value(s_struct, 0, "fparse.s.ptr")
                .unwrap()
                .into_pointer_value();
            let s_len = self
                .builder
                .build_extract_value(s_struct, 1, "fparse.s.len")
                .unwrap()
                .into_int_value();

            let fn_val = self
                .current_fn
                .ok_or_else(|| "f64.parse called outside fn".to_string())?;
            let out_slot = self.create_entry_alloca(fn_val, "fparse.out", f64_t.into());

            let parse_fn = self
                .module
                .get_function("karac_runtime_parse_f64")
                .expect("karac_runtime_parse_f64 declared in Codegen::new");
            let success = self
                .builder
                .build_call(
                    parse_fn,
                    &[s_data.into(), s_len.into(), out_slot.into()],
                    "fparse.ok",
                )
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_int_value();
            let is_ok = self
                .builder
                .build_int_compare(
                    inkwell::IntPredicate::NE,
                    success,
                    i8_t.const_zero(),
                    "fparse.ok.bool",
                )
                .unwrap();

            let some_bb = self.context.append_basic_block(fn_val, "fparse.some");
            let none_bb = self.context.append_basic_block(fn_val, "fparse.none");
            let merge_bb = self.context.append_basic_block(fn_val, "fparse.merge");

            self.builder
                .build_conditional_branch(is_ok, some_bb, none_bb)
                .unwrap();

            self.builder.position_at_end(some_bb);
            let parsed = self
                .builder
                .build_load(f64_t, out_slot, "fparse.value")
                .unwrap();
            let some_payload_words = self.coerce_to_payload_words(parsed, 3)?;
            let some_end_bb = self.builder.get_insert_block().unwrap();
            self.builder.build_unconditional_branch(merge_bb).unwrap();

            self.builder.position_at_end(none_bb);
            let none_end_bb = self.builder.get_insert_block().unwrap();
            self.builder.build_unconditional_branch(merge_bb).unwrap();

            self.builder.position_at_end(merge_bb);
            let agg = self.build_option_some_via_phis(
                &some_payload_words,
                some_end_bb,
                none_end_bb,
                "fparse.opt",
            );
            return Ok(agg);
        }
        // Lowered operator dispatch: `<Primitive>.<op>(args)` — synthesized
        // by the lowering pass. Reroute to the existing BinOp/UnaryOp
        // intrinsic compilation so we don't have to duplicate codegen logic.
        let is_primitive = matches!(
            type_name,
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
            let bin_op = match method {
                "add" => Some(BinOp::Add),
                "sub" => Some(BinOp::Sub),
                "mul" => Some(BinOp::Mul),
                "div" => Some(BinOp::Div),
                "rem" => Some(BinOp::Mod),
                "eq" => Some(BinOp::Eq),
                "ne" => Some(BinOp::NotEq),
                "lt" => Some(BinOp::Lt),
                "le" => Some(BinOp::LtEq),
                "gt" => Some(BinOp::Gt),
                "ge" => Some(BinOp::GtEq),
                "bitand" => Some(BinOp::BitAnd),
                "bitor" => Some(BinOp::BitOr),
                "bitxor" => Some(BinOp::BitXor),
                "shl" => Some(BinOp::Shl),
                "shr" => Some(BinOp::Shr),
                _ => None,
            };
            if let Some(op) = bin_op {
                if _args.len() == 2 {
                    // Compile operands directly and emit through the typed
                    // binop helper so unsigned primitives (`u8`/.../`usize`)
                    // dispatch to unsigned LLVM ops. Round-tripping through
                    // a synthesized `ExprKind::Binary` would lose the
                    // type-name's signedness — the AST node carries only the
                    // `BinOp` symbol, not the operand type.
                    let lhs = self.compile_expr(&_args[0].value)?;
                    let rhs = self.compile_expr(&_args[1].value)?;
                    let is_unsigned =
                        matches!(type_name, "u8" | "u16" | "u32" | "u64" | "u128" | "usize");
                    // Narrow integers (8/16/32-bit) are real fixed-width types
                    // (design.md § Integer overflow): normalize both operands
                    // to i64 (matching the interpreter, which evaluates all
                    // integer arithmetic at i64 — so an i64-canonical local and
                    // an i8 buffer element of the same type compute together)
                    // and, for arithmetic, trap if the result leaves the
                    // declared width. `type_name` gives the exact width here.
                    let narrow_bits = match type_name {
                        "u8" | "i8" => Some(8u32),
                        "u16" | "i16" => Some(16),
                        "u32" | "i32" => Some(32),
                        _ => None,
                    };
                    if let Some(bits) = narrow_bits {
                        return self.compile_narrow_int_binop(&op, lhs, rhs, bits, is_unsigned);
                    }
                    let result = self.compile_binop_typed(&op, lhs, rhs, is_unsigned)?;
                    // General owned-temp tracking, slice 3c: free a fresh-temp
                    // String OPERAND orphaned by the lowered string binop. `+`
                    // and the comparison ops on `String` desugar to this
                    // `String.add`/`eq`/`lt`/... assoc call (lowering.rs
                    // `rewrite_binary`); `compile_string_binop` reads each
                    // operand's bytes (concat copies into a fresh result buffer,
                    // a comparison scans) but takes no ownership, so a fresh-owned
                    // operand — `make_str() + "x"`, `make_str() == s`, a
                    // `substring`/slice, or the inner `String.add` of a chained
                    // concat (itself a fresh-temp Call) — leaks its buffer once
                    // per evaluation, unbounded in a loop. `free_fresh_owned_str_arg`
                    // self-gates to fresh-owned shapes (Call/MethodCall, fresh
                    // slice) with a `cap > 0` backstop, so a named binding,
                    // rodata literal, or borrow operand is never (double-)freed.
                    // Emitted AFTER the binop so every read of the operand buffers
                    // dominates the free. String only — int/float operands aren't
                    // heap (the helper no-ops on a non-vec-struct value anyway).
                    if type_name == "String" {
                        self.free_fresh_owned_str_arg(&_args[0].value, lhs);
                        self.free_fresh_owned_str_arg(&_args[1].value, rhs);
                    }
                    return Ok(result);
                }
            }
            if method == "neg" && _args.len() == 1 {
                let synth = Expr {
                    span: _args[0].value.span.clone(),
                    kind: ExprKind::Unary {
                        op: UnaryOp::Neg,
                        operand: Box::new(_args[0].value.clone()),
                    },
                };
                return self.compile_expr(&synth);
            }
            if method == "not" && _args.len() == 1 {
                // `not` covers `!bool` and `~int` — target type disambiguates.
                let un_op = if type_name == "bool" {
                    UnaryOp::Not
                } else {
                    UnaryOp::BitNot
                };
                let synth = Expr {
                    span: _args[0].value.span.clone(),
                    kind: ExprKind::Unary {
                        op: un_op,
                        operand: Box::new(_args[0].value.clone()),
                    },
                };
                return self.compile_expr(&synth);
            }
        }
        // Debugger Contract slice 5 — `std.runtime` introspection APIs
        // declared in `runtime/stdlib/runtime.kara`. Three Kāra-callable
        // methods on the empty-marker `Runtime` struct that materialize the
        // slice-3 `KARAC_SPAWN_SITES` metadata + slice-4 `ACTIVE_FRAMES`
        // registry. Routes here because baked-stdlib impl methods are
        // typechecked but not emitted as LLVM functions (see compile_program
        // line 2720+ — only `program.items` impls compile), so the
        // `module.get_function("Runtime.has_debug_metadata")` lookup below
        // would miss and fall through to the i64-zero default. Explicit
        // dispatch keeps the contract surface stable regardless of how
        // baked stdlib codegen evolves.
        if type_name == "Runtime" {
            match method {
                "has_debug_metadata" => {
                    // Single call to `karac_runtime_has_debug_metadata` —
                    // returns the `i1` value directly. The runtime fn reads
                    // `KARAC_SPAWN_SITES_ENABLED`.
                    let f = self
                        .module
                        .get_function("karac_runtime_has_debug_metadata")
                        .expect("karac_runtime_has_debug_metadata declared in Codegen::new");
                    let call = self
                        .builder
                        .build_call(f, &[], "runtime.has_debug_metadata")
                        .unwrap();
                    return Ok(call.try_as_basic_value().unwrap_basic());
                }
                "list_par_blocks" => {
                    // Runtime-side Vec materialization (hard-stop trigger 3
                    // fallback per slice 5 plan). Alloca a `{ptr, i64, i64}`
                    // slot in the entry block, pass its address to the
                    // runtime fn, and load the resulting Vec value.
                    //
                    // The Vec's heap buffer is owned by the caller — the
                    // runtime allocates via `std::alloc::alloc`, the
                    // codegen scope-cleanup machinery treats the returned
                    // Vec like any other Kāra Vec for free-on-exit. Per
                    // `runtime/stdlib/runtime.kara`'s comment on the
                    // method, an empty result is the `{null, 0, 0}` form
                    // (no heap allocation), matching `Vec.new()` so cleanup
                    // is a no-op.
                    let vec_ty = self.vec_struct_type();
                    let fn_val = self
                        .current_fn
                        .ok_or_else(|| "list_par_blocks called outside fn".to_string())?;
                    let slot = self.create_entry_alloca(
                        fn_val,
                        "runtime.list_par_blocks.slot",
                        vec_ty.into(),
                    );
                    let f = self
                        .module
                        .get_function("karac_runtime_list_par_blocks_into")
                        .expect("karac_runtime_list_par_blocks_into declared in Codegen::new");
                    self.builder
                        .build_call(f, &[slot.into()], "runtime.list_par_blocks.fill")
                        .unwrap();
                    let value = self
                        .builder
                        .build_load(vec_ty, slot, "runtime.list_par_blocks.val")
                        .unwrap();
                    return Ok(value);
                }
                "list_tasks" => {
                    // v1 always returns the empty Vec — no real task
                    // suspension exists yet. Identical to the `Vec.new()`
                    // arm below: synthesize `{null, 0, 0}` directly.
                    // Phase 6.3's network event loop replaces this with a
                    // runtime-side materialization mirroring
                    // `list_par_blocks`; the v1 contract pin lives in the
                    // tests under `tests::test_list_tasks_returns_empty_in_v1`.
                    let vec_ty = self.vec_struct_type();
                    let null_ptr = self.context.ptr_type(AddressSpace::default()).const_null();
                    let zero = self.context.i64_type().const_int(0, false);
                    let mut agg = vec_ty.get_undef();
                    agg = self
                        .builder
                        .build_insert_value(agg, null_ptr, 0, "tasks.data")
                        .unwrap()
                        .into_struct_value();
                    agg = self
                        .builder
                        .build_insert_value(agg, zero, 1, "tasks.len")
                        .unwrap()
                        .into_struct_value();
                    agg = self
                        .builder
                        .build_insert_value(agg, zero, 2, "tasks.cap")
                        .unwrap()
                        .into_struct_value();
                    return Ok(agg.into());
                }
                _ => {}
            }
        }

        // Phase-8 line 17 slice 2 — `Client.new()` returns an empty
        // `Client { }` struct value. The struct's storage is zero-sized
        // (no fields), but codegen needs to hand back a `StructValue` of
        // the registered Client struct type so the let-binding RHS
        // recognizer ast-hint path populates `var_type_names[c] =
        // "Client"`. Without this, the call falls through to the i64-0
        // default at the end of `compile_assoc_call` and the receiver
        // shape is lost — `c.get(url)` then can't reach the std.http
        // client dispatch arm.
        if type_name == "Client" && method == "new" && _args.is_empty() {
            // `Client { }` is an empty struct — zero fields, zero size.
            // Layout seeded into `struct_types` by
            // `seed_builtin_struct_types`.
            let client_ty = self
                .struct_types
                .get("Client")
                .copied()
                .expect("Client struct type seeded by seed_builtin_struct_types");
            return Ok(client_ty.get_undef().into());
        }
        // Slice B (2026-05-09): `Server.serve_static(addr, body)` —
        // hyper-backed minimal smoke entry. Dispatches to
        // `karac_runtime_serve_http_static`. Both args are Kāra
        // `String`s `{ptr, i64, i64}`; the runtime requires a null-
        // terminated C string for `addr`, so we allocate a `len+1`
        // buffer, memcpy + null-terminate. The body is passed as raw
        // bytes (`ptr` + `len`) — no null-termination needed.
        //
        // The returned i32 is mapped into a Kāra `Result[Unit, HttpError]`:
        // 0 → `Ok(())`, non-zero → `Err(HttpError { message })` with a
        // pinned message string per non-zero code (matches the runtime
        // crate's return-code table).
        // Phase 6 line 17 — `TcpListener.bind(addr) -> TcpListener`.
        // Routes through the codegen lowering in `src/codegen/tcp.rs`
        // which extracts the kara `String` `{ptr, len}` from the
        // addr arg and feeds them into the runtime FFI
        // `karac_runtime_tcp_bind(addr_ptr, addr_len) -> i32`, then
        // wraps the returned fd into a fresh `TcpListener { fd }`
        // struct value. The `:0` ephemeral-port + BOUND_PORT-print
        // convention lives runtime-side (see `karac_runtime_tcp_bind`).
        if type_name == "TcpListener" && method == "bind" && _args.len() == 1 {
            let addr_val = self.compile_expr(&_args[0].value)?;
            return self.lower_tcp_listener_bind(addr_val);
        }
        // Phase-8 line 74 prereq — `TcpStream.connect(addr) ->
        // Result[TcpStream, TcpError]`, the plain-TCP client. Mirror of
        // `bind` on the codegen side: extract the addr `String` `{ptr,
        // len}`, feed `karac_runtime_tcp_connect(addr_ptr, addr_len) ->
        // i32`, wrap the connected fd into `Result[TcpStream, TcpError]`.
        if type_name == "TcpStream" && method == "connect" && _args.len() == 1 {
            let addr_val = self.compile_expr(&_args[0].value)?;
            return self.lower_tcp_stream_connect(addr_val);
        }
        // Phase 6 line 236 slice 2 — `TlsListener.bind_tls(addr, cert,
        // key) -> TlsListener`. Lowering in `src/codegen/tls.rs` calls
        // `karac_runtime_tls_config_new` + `_tls_listener_bind` then
        // packs `{fd, config}` into the TlsListener struct value.
        if type_name == "TlsListener" && method == "bind_tls" && _args.len() == 3 {
            let addr_val = self.compile_expr(&_args[0].value)?;
            let cert_val = self.compile_expr(&_args[1].value)?;
            let key_val = self.compile_expr(&_args[2].value)?;
            return self.lower_tls_listener_bind_tls(addr_val, cert_val, key_val);
        }
        // Phase-8 line 22 — `TlsStream.connect(addr, server_name,
        // roots_pem) -> TlsStream`. Client-side counterpart of
        // `TlsListener.bind_tls + .accept`: TCP connect + sync rustls
        // handshake + register `Connection::Client` in the shared
        // per-fd session map; returns `TlsStream { fd }` (same shape
        // as `.accept`'s output — both directions are interchangeable
        // for downstream `read` / `write` / `Drop`).
        if type_name == "TlsStream" && method == "connect" && _args.len() == 3 {
            let addr_val = self.compile_expr(&_args[0].value)?;
            let server_name_val = self.compile_expr(&_args[1].value)?;
            let roots_pem_val = self.compile_expr(&_args[2].value)?;
            return self.lower_tls_stream_connect(addr_val, server_name_val, roots_pem_val);
        }
        // Phase 6 line 17 slice 9e.1 — `WebSocket.from_fd(fd) -> WebSocket`.
        // Pure value construction: pack the i32 fd into a fresh
        // `WebSocket { fd }` struct value (same single-i32-field
        // layout as `TcpListener` / `TcpStream`). Real-world entry
        // through HTTP upgrade ships in slice 9e.2; for v1 this is
        // the testing entry point.
        if type_name == "WebSocket" && method == "from_fd" && _args.len() == 1 {
            let fd_val = self.compile_expr(&_args[0].value)?;
            return self.lower_websocket_from_fd(fd_val);
        }
        // Phase 6 line 17 slice 9e.2 — `WebSocket.accept(listener: TcpListener) -> WebSocket`.
        // Parks on listener-readability then runs accept(2) + HTTP
        // upgrade handshake via the runtime FFI. Routes through
        // `lower_websocket_accept` in `src/codegen/tcp.rs`.
        if type_name == "WebSocket" && method == "accept" && _args.len() == 1 {
            let listener_val = self.compile_expr(&_args[0].value)?;
            return self.lower_websocket_accept(listener_val);
        }
        // Phase 6 line 236 slice 3 — `WebSocket.accept_tls(listener:
        // TlsListener) -> WebSocket`. Same shape as `accept` but the
        // connection is TLS-wrapped. Lowering in `src/codegen/tls.rs`
        // extracts both fd and config from the TlsListener, parks on
        // listener fd, calls `karac_runtime_ws_accept_tls(fd, config)`
        // which performs TCP accept + rustls handshake + HTTP
        // upgrade-over-TLS + per-fd TLS session registration. The
        // returned `WebSocket { fd }` is the same shape as plain-TCP
        // accept's return; subsequent `recv_text` / `send_text` calls
        // auto-route through TLS because the runtime FFIs check the
        // TLS session registry by fd.
        if type_name == "WebSocket" && method == "accept_tls" && _args.len() == 1 {
            let listener_val = self.compile_expr(&_args[0].value)?;
            return self.lower_websocket_accept_tls(listener_val);
        }
        // Phase 8 `File` handle slice F4: constructor dispatch.
        // `File.open` / `.create` / `.append` lower to the matching
        // `karac_runtime_file_*` extern; the KaracIoResult return
        // unpacks into `Result[File, IoError]` via
        // `Codegen::lower_kara_io_result`. The String `path` arg
        // contributes the `{ptr, len}` pair the runtime needs;
        // capacity is unused.
        if type_name == "File" && matches!(method, "open" | "create" | "append") && _args.len() == 1
        {
            let sym = match method {
                "open" => "karac_runtime_file_open",
                "create" => "karac_runtime_file_create",
                "append" => "karac_runtime_file_append",
                _ => unreachable!(),
            };
            return self.compile_file_constructor(sym, &_args[0].value);
        }

        // `FileSystem.read_to_string(path) -> Result[String, IoError]`.
        // One-shot whole-file slurp; lowers to
        // `karac_runtime_file_read_to_string` and unpacks the
        // String-payload KaracIoResult. (Distinct from the no-arg
        // `Stdin.read_to_string`, which routes through the ambient FFI path
        // — `compile_ambient_ffi`'s `("Stdin", …)` arm, L646 slice 3b —
        // sharing this same `lower_kara_io_result` String-payload unpack.)
        if type_name == "FileSystem" && method == "read_to_string" && _args.len() == 1 {
            return self.compile_file_read_to_string(&_args[0].value);
        }

        // `FileSystem.read_lines(path) -> Result[Vec[String], IoError]`.
        // One-shot whole-file slurp split into a `Vec[String]` of lines;
        // lowers to `karac_runtime_fs_read_lines` (two out-params: the
        // KaracIoResult status + a KaracVec filled with the line elements)
        // and builds `Result.Ok(<Vec[String]>)` / `Result.Err(IoError…)`.
        // B-2026-07-11-38. The ambient `fs.read_lines()` form routes to the
        // same value-core in `compile_ambient_ffi`.
        if type_name == "FileSystem" && method == "read_lines" && _args.len() == 1 {
            return self.compile_fs_read_lines(&_args[0].value);
        }

        // `FileSystem.write(path, contents) -> Result[Unit, IoError]`.
        // One-shot whole-file write (create-or-truncate); lowers to
        // `karac_runtime_fs_write` and unpacks the Unit-Ok KaracIoResult.
        // Companion to `read_to_string` above (L646 slice 4).
        if type_name == "FileSystem" && method == "write" && _args.len() == 2 {
            return self.compile_fs_write(&_args[0].value, &_args[1].value);
        }

        if type_name == "Server" && method == "serve_static" && _args.len() == 2 {
            {
                let addr_val = self.compile_expr(&_args[0].value)?;
                let body_val = self.compile_expr(&_args[1].value)?;
                let addr_sv = addr_val.into_struct_value();
                let body_sv = body_val.into_struct_value();
                let addr_ptr = self
                    .builder
                    .build_extract_value(addr_sv, 0, "addr.data")
                    .unwrap()
                    .into_pointer_value();
                let addr_len = self
                    .builder
                    .build_extract_value(addr_sv, 1, "addr.len")
                    .unwrap()
                    .into_int_value();
                let body_ptr = self
                    .builder
                    .build_extract_value(body_sv, 0, "body.data")
                    .unwrap()
                    .into_pointer_value();
                let body_len = self
                    .builder
                    .build_extract_value(body_sv, 1, "body.len")
                    .unwrap()
                    .into_int_value();

                // Allocate addr_len + 1 bytes, memcpy, null-terminate.
                let one = self.context.i64_type().const_int(1, false);
                let needed = self
                    .builder
                    .build_int_add(addr_len, one, "addr.cstr.len")
                    .unwrap();
                let cstr_buf = self
                    .builder
                    .build_call(self.malloc_fn, &[needed.into()], "addr.cstr.buf")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_pointer_value();
                self.builder
                    .build_memcpy(cstr_buf, 1, addr_ptr, 1, addr_len)
                    .unwrap();
                let i8_ty = self.context.i8_type();
                let zero_byte = i8_ty.const_int(0, false);
                let term_ptr = unsafe {
                    self.builder
                        .build_in_bounds_gep(i8_ty, cstr_buf, &[addr_len], "addr.cstr.term")
                        .unwrap()
                };
                self.builder.build_store(term_ptr, zero_byte).unwrap();

                let serve_fn = self
                    .module
                    .get_function("karac_runtime_serve_http_static")
                    .expect("karac_runtime_serve_http_static declared in Codegen::new");
                let call = self
                    .builder
                    .build_call(
                        serve_fn,
                        &[cstr_buf.into(), body_ptr.into(), body_len.into()],
                        "http.serve_static.call",
                    )
                    .unwrap();
                let rc_i32 = call.try_as_basic_value().unwrap_basic().into_int_value();

                // Free the cstr buffer (smoke path: the runtime call
                // typically blocks forever, so this free is unreachable
                // — but on bind failure the call returns immediately
                // and we want clean shutdown).
                self.builder
                    .build_call(
                        self.module.get_function("free").unwrap_or_else(|| {
                            let free_ty = self.context.void_type().fn_type(
                                &[self.context.ptr_type(AddressSpace::default()).into()],
                                false,
                            );
                            self.module
                                .add_function("free", free_ty, Some(Linkage::External))
                        }),
                        &[cstr_buf.into()],
                        "addr.cstr.free",
                    )
                    .unwrap();

                // Build `Result[Unit, HttpError]`. Layout per Slice CP
                // compound-payload enum codegen: tag at word 0, payload
                // at words 1..N. For a `Result[Unit, HttpError]`:
                //   - Ok(()): tag=0 (Ok), payload all zero
                //   - Err(HttpError { message: String }): tag=1, payload =
                //     `String` `{ptr, len, cap}` (3 words)
                //
                // Look up the layout — `Result` is registered as part of
                // the prelude pass.
                let result_layout = self
                    .enum_layouts
                    .get("Result")
                    .expect("Result layout registered before Server.serve_static dispatch");
                let result_ty = result_layout.llvm_type;
                let total_fields = result_ty.count_fields() as u64;
                let i64_ty = self.context.i64_type();
                let fn_val = self
                    .current_fn
                    .ok_or_else(|| "Server.serve_static called outside fn".to_string())?;
                let result_slot =
                    self.create_entry_alloca(fn_val, "http.serve_static.result", result_ty.into());

                // Branch on rc == 0.
                let rc_zero = self
                    .builder
                    .build_int_compare(
                        inkwell::IntPredicate::EQ,
                        rc_i32,
                        self.context.i32_type().const_int(0, false),
                        "rc.is_zero",
                    )
                    .unwrap();
                let ok_bb = self.context.append_basic_block(fn_val, "serve.ok");
                let err_bb = self.context.append_basic_block(fn_val, "serve.err");
                let cont_bb = self.context.append_basic_block(fn_val, "serve.cont");
                self.builder
                    .build_conditional_branch(rc_zero, ok_bb, err_bb)
                    .unwrap();

                // Ok arm: zero out tag + payload (Unit payload is empty).
                self.builder.position_at_end(ok_bb);
                let zero_w = i64_ty.const_int(0, false);
                for w in 0..total_fields {
                    let elem_ptr = self
                        .builder
                        .build_struct_gep(result_ty, result_slot, w as u32, &format!("ok.w{w}"))
                        .unwrap();
                    self.builder.build_store(elem_ptr, zero_w).unwrap();
                }
                self.builder.build_unconditional_branch(cont_bb).unwrap();

                // Err arm: tag=1, payload = HttpError { message: <pinned> }.
                self.builder.position_at_end(err_bb);
                let one_w = i64_ty.const_int(1, false);
                let tag_ptr = self
                    .builder
                    .build_struct_gep(result_ty, result_slot, 0, "err.tag")
                    .unwrap();
                self.builder.build_store(tag_ptr, one_w).unwrap();

                // Build a minimal HttpError String payload —
                // `"http: serve failed"`. Heap-allocated so the
                // standard String free-on-scope-exit path doesn't
                // double-free a global.
                let msg = "http: serve failed";
                let msg_global = self
                    .builder
                    .build_global_string_ptr(msg, "http.serve.err.msg")
                    .unwrap();
                let msg_len = i64_ty.const_int(msg.len() as u64, false);
                let msg_buf = self
                    .builder
                    .build_call(self.malloc_fn, &[msg_len.into()], "err.msg.buf")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_pointer_value();
                self.builder
                    .build_memcpy(msg_buf, 1, msg_global.as_pointer_value(), 1, msg_len)
                    .unwrap();

                // Payload offset: tag is field 0; payload is fields 1..N.
                // HttpError = `{ message: String }` = `{ptr, len, cap}` =
                // 3 i64 words. Stored at fields 1, 2, 3.
                let msg_ptr_buf_int = self
                    .builder
                    .build_ptr_to_int(msg_buf, i64_ty, "err.msg.ptr.i64")
                    .unwrap();
                if total_fields > 1 {
                    let p1 = self
                        .builder
                        .build_struct_gep(result_ty, result_slot, 1, "err.payload.ptr")
                        .unwrap();
                    self.builder.build_store(p1, msg_ptr_buf_int).unwrap();
                }
                if total_fields > 2 {
                    let p2 = self
                        .builder
                        .build_struct_gep(result_ty, result_slot, 2, "err.payload.len")
                        .unwrap();
                    self.builder.build_store(p2, msg_len).unwrap();
                }
                if total_fields > 3 {
                    let p3 = self
                        .builder
                        .build_struct_gep(result_ty, result_slot, 3, "err.payload.cap")
                        .unwrap();
                    self.builder.build_store(p3, msg_len).unwrap();
                }
                // Zero out remaining payload words (if Result's payload
                // is wider than 3 due to other variants in the program).
                for w in 4..total_fields {
                    let elem_ptr = self
                        .builder
                        .build_struct_gep(result_ty, result_slot, w as u32, &format!("err.w{w}"))
                        .unwrap();
                    self.builder.build_store(elem_ptr, zero_w).unwrap();
                }
                self.builder.build_unconditional_branch(cont_bb).unwrap();

                // Cont: load + return the result aggregate.
                self.builder.position_at_end(cont_bb);
                let result = self
                    .builder
                    .build_load(result_ty, result_slot, "http.serve_static.result.val")
                    .unwrap();
                return Ok(result);
            }
        }

        // Slice B follow-up (2026-05-09): `Server.serve(handler)` —
        // hyper-backed handler-dispatch entry. Mirrors `serve_static`'s
        // shape:
        //   - Arg 0: address String → null-terminated C string.
        //   - Arg 1: handler — free-fn name → fn-pointer LLVM value
        //     via `module.get_function`. Closures-with-captures and
        //     other non-free-fn shapes reject with
        //     `E_CLOSURE_AS_FN_PTR_NOT_YET` (sub-step (d)).
        //   - The runtime extern's `bound_port_out` slot is null in v1
        //     — the smoke test reads the bound port from the runtime's
        //     `BOUND_PORT=<n>\n` stdout line per Slice B's convention.
        //
        // Returns `Result[Unit, HttpError]`; rc=0 → Ok(()), rc≠0 →
        // Err(HttpError { message: "http: serve failed" }). Reuses the
        // `serve_static` Result-layout machinery verbatim — the
        // handler-dispatch and static-body entries differ only in arg
        // 1 + the extern they target, not in the return-value
        // translation.
        if type_name == "Server" && method == "serve" && _args.len() == 2 {
            // Address handling mirrors `Server.serve_static`'s shape:
            // the Kāra `String` is `{ptr, len, cap}`, but hyper's bind
            // path needs a null-terminated C string — allocate
            // `len + 1` bytes, memcpy, null-terminate.
            let addr_val = self.compile_expr(&_args[0].value)?;
            let addr_sv = addr_val.into_struct_value();
            let addr_ptr_raw = self
                .builder
                .build_extract_value(addr_sv, 0, "http.serve.addr.data")
                .unwrap()
                .into_pointer_value();
            let addr_len = self
                .builder
                .build_extract_value(addr_sv, 1, "http.serve.addr.len")
                .unwrap()
                .into_int_value();
            let one = self.context.i64_type().const_int(1, false);
            let needed = self
                .builder
                .build_int_add(addr_len, one, "http.serve.addr.cstr.len")
                .unwrap();
            let addr_cstr = self
                .builder
                .build_call(self.malloc_fn, &[needed.into()], "http.serve.addr.cstr.buf")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_pointer_value();
            self.builder
                .build_memcpy(addr_cstr, 1, addr_ptr_raw, 1, addr_len)
                .unwrap();
            let i8_ty = self.context.i8_type();
            let zero_byte = i8_ty.const_int(0, false);
            let term_ptr = unsafe {
                self.builder
                    .build_in_bounds_gep(i8_ty, addr_cstr, &[addr_len], "http.serve.addr.cstr.term")
                    .unwrap()
            };
            self.builder.build_store(term_ptr, zero_byte).unwrap();
            let addr_ptr = addr_cstr;

            let handler_arg = &_args[1];
            let handler_fn = self.resolve_free_fn_for_handler_arg(&handler_arg.value)?;
            // HTTP handler ABI trampoline (2026-05-09): pass the per-handler
            // shim's address rather than the user fn's directly. The user fn
            // takes a value-typed `Request` and returns a `Response`; the
            // FFI extern's handler slot expects
            // `extern "C" fn(*const KaracHttpRequest, *mut KaracHttpResponse)`.
            // The shim adapts between the two ABIs (cached per-handler).
            let shim_fn = self.emit_http_handler_shim(handler_fn);
            let handler_ptr = shim_fn.as_global_value().as_pointer_value();

            let serve_fn = self
                .module
                .get_function("karac_runtime_serve_http")
                .expect("karac_runtime_serve_http declared in Codegen::new");
            let null_port_out = self.context.ptr_type(AddressSpace::default()).const_null();
            let call = self
                .builder
                .build_call(
                    serve_fn,
                    &[addr_ptr.into(), handler_ptr.into(), null_port_out.into()],
                    "http.serve.call",
                )
                .unwrap();
            let rc_i32 = call.try_as_basic_value().unwrap_basic().into_int_value();

            // Build `Result[Unit, HttpError]` from the i32 return code.
            // Identical machinery to `Server.serve_static` — see the
            // long comment around lines 6375-6500 above.
            let result_layout = self
                .enum_layouts
                .get("Result")
                .expect("Result layout registered before Server.serve dispatch");
            let result_ty = result_layout.llvm_type;
            let total_fields = result_ty.count_fields() as u64;
            let i64_ty = self.context.i64_type();
            let fn_val = self
                .current_fn
                .ok_or_else(|| "Server.serve called outside fn".to_string())?;
            let result_slot =
                self.create_entry_alloca(fn_val, "http.serve.result", result_ty.into());

            let rc_zero = self
                .builder
                .build_int_compare(
                    inkwell::IntPredicate::EQ,
                    rc_i32,
                    self.context.i32_type().const_int(0, false),
                    "rc.is_zero",
                )
                .unwrap();
            let ok_bb = self.context.append_basic_block(fn_val, "serve.h.ok");
            let err_bb = self.context.append_basic_block(fn_val, "serve.h.err");
            let cont_bb = self.context.append_basic_block(fn_val, "serve.h.cont");
            self.builder
                .build_conditional_branch(rc_zero, ok_bb, err_bb)
                .unwrap();

            // Ok arm.
            self.builder.position_at_end(ok_bb);
            let zero_w = i64_ty.const_int(0, false);
            for w in 0..total_fields {
                let elem_ptr = self
                    .builder
                    .build_struct_gep(result_ty, result_slot, w as u32, &format!("ok.w{w}"))
                    .unwrap();
                self.builder.build_store(elem_ptr, zero_w).unwrap();
            }
            self.builder.build_unconditional_branch(cont_bb).unwrap();

            // Err arm.
            self.builder.position_at_end(err_bb);
            let one_w = i64_ty.const_int(1, false);
            let tag_ptr = self
                .builder
                .build_struct_gep(result_ty, result_slot, 0, "err.tag")
                .unwrap();
            self.builder.build_store(tag_ptr, one_w).unwrap();

            let msg = "http: serve failed";
            let msg_global = self
                .builder
                .build_global_string_ptr(msg, "http.serve.h.err.msg")
                .unwrap();
            let msg_len = i64_ty.const_int(msg.len() as u64, false);
            let msg_buf = self
                .builder
                .build_call(self.malloc_fn, &[msg_len.into()], "err.msg.buf")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_pointer_value();
            self.builder
                .build_memcpy(msg_buf, 1, msg_global.as_pointer_value(), 1, msg_len)
                .unwrap();
            let msg_ptr_buf_int = self
                .builder
                .build_ptr_to_int(msg_buf, i64_ty, "err.msg.ptr.i64")
                .unwrap();
            if total_fields > 1 {
                let p1 = self
                    .builder
                    .build_struct_gep(result_ty, result_slot, 1, "err.payload.ptr")
                    .unwrap();
                self.builder.build_store(p1, msg_ptr_buf_int).unwrap();
            }
            if total_fields > 2 {
                let p2 = self
                    .builder
                    .build_struct_gep(result_ty, result_slot, 2, "err.payload.len")
                    .unwrap();
                self.builder.build_store(p2, msg_len).unwrap();
            }
            if total_fields > 3 {
                let p3 = self
                    .builder
                    .build_struct_gep(result_ty, result_slot, 3, "err.payload.cap")
                    .unwrap();
                self.builder.build_store(p3, msg_len).unwrap();
            }
            for w in 4..total_fields {
                let elem_ptr = self
                    .builder
                    .build_struct_gep(result_ty, result_slot, w as u32, &format!("err.w{w}"))
                    .unwrap();
                self.builder.build_store(elem_ptr, zero_w).unwrap();
            }
            self.builder.build_unconditional_branch(cont_bb).unwrap();

            // Cont.
            self.builder.position_at_end(cont_bb);
            let result = self
                .builder
                .build_load(result_ty, result_slot, "http.serve.result.val")
                .unwrap();
            return Ok(result);
        }

        // Phase-8 std.http × TLS bridge: `Server.serve_tls(addr,
        // cert_pem, key_pem, handler)` — HTTPS variant of `serve`. The
        // handler-shim trampoline is reused verbatim (`Request` /
        // `Response` ABI is transport-independent); the runtime side
        // terminates TLS via tokio-rustls before feeding hyper. PEM
        // strings flow as inline `(ptr, len)` pairs (no null terminator
        // — `karac_runtime_serve_https` reads byte slices straight to
        // rustls-pemfile). Return value translation is the same
        // `Result[Unit, HttpError]` shape as `serve` / `serve_static`.
        if type_name == "Server" && method == "serve_tls" && _args.len() == 4 {
            // ── Addr (null-terminated C string, same shape as `serve`) ──
            let addr_val = self.compile_expr(&_args[0].value)?;
            let addr_sv = addr_val.into_struct_value();
            let addr_ptr_raw = self
                .builder
                .build_extract_value(addr_sv, 0, "https.serve.addr.data")
                .unwrap()
                .into_pointer_value();
            let addr_len = self
                .builder
                .build_extract_value(addr_sv, 1, "https.serve.addr.len")
                .unwrap()
                .into_int_value();
            let one = self.context.i64_type().const_int(1, false);
            let needed = self
                .builder
                .build_int_add(addr_len, one, "https.serve.addr.cstr.len")
                .unwrap();
            let addr_cstr = self
                .builder
                .build_call(
                    self.malloc_fn,
                    &[needed.into()],
                    "https.serve.addr.cstr.buf",
                )
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_pointer_value();
            self.builder
                .build_memcpy(addr_cstr, 1, addr_ptr_raw, 1, addr_len)
                .unwrap();
            let i8_ty = self.context.i8_type();
            let zero_byte = i8_ty.const_int(0, false);
            let term_ptr = unsafe {
                self.builder
                    .build_in_bounds_gep(
                        i8_ty,
                        addr_cstr,
                        &[addr_len],
                        "https.serve.addr.cstr.term",
                    )
                    .unwrap()
            };
            self.builder.build_store(term_ptr, zero_byte).unwrap();
            let addr_ptr = addr_cstr;

            // ── Cert PEM bytes (raw `(ptr, i64 len)`, no null term) ──
            let cert_val = self.compile_expr(&_args[1].value)?;
            let cert_sv = cert_val.into_struct_value();
            let cert_ptr = self
                .builder
                .build_extract_value(cert_sv, 0, "https.serve.cert.data")
                .unwrap()
                .into_pointer_value();
            let cert_len = self
                .builder
                .build_extract_value(cert_sv, 1, "https.serve.cert.len")
                .unwrap()
                .into_int_value();

            // ── Key PEM bytes ──
            let key_val = self.compile_expr(&_args[2].value)?;
            let key_sv = key_val.into_struct_value();
            let key_ptr = self
                .builder
                .build_extract_value(key_sv, 0, "https.serve.key.data")
                .unwrap()
                .into_pointer_value();
            let key_len = self
                .builder
                .build_extract_value(key_sv, 1, "https.serve.key.len")
                .unwrap()
                .into_int_value();

            // ── Handler (free-fn → shim, same as `serve`) ──
            let handler_arg = &_args[3];
            let handler_fn = self.resolve_free_fn_for_handler_arg(&handler_arg.value)?;
            let shim_fn = self.emit_http_handler_shim(handler_fn);
            let handler_ptr = shim_fn.as_global_value().as_pointer_value();

            // ── Call the extern ──
            let serve_fn = self
                .module
                .get_function("karac_runtime_serve_https")
                .expect("karac_runtime_serve_https declared in Codegen::new");
            let null_port_out = self.context.ptr_type(AddressSpace::default()).const_null();
            let call = self
                .builder
                .build_call(
                    serve_fn,
                    &[
                        addr_ptr.into(),
                        cert_ptr.into(),
                        cert_len.into(),
                        key_ptr.into(),
                        key_len.into(),
                        handler_ptr.into(),
                        null_port_out.into(),
                    ],
                    "https.serve.call",
                )
                .unwrap();
            let rc_i32 = call.try_as_basic_value().unwrap_basic().into_int_value();

            // ── Result[Unit, HttpError] translation — same machinery
            // as `serve` / `serve_static` (the rc → Result mapping is
            // transport-independent). ──
            let result_layout = self
                .enum_layouts
                .get("Result")
                .expect("Result layout registered before Server.serve_tls dispatch");
            let result_ty = result_layout.llvm_type;
            let total_fields = result_ty.count_fields() as u64;
            let i64_ty = self.context.i64_type();
            let fn_val = self
                .current_fn
                .ok_or_else(|| "Server.serve_tls called outside fn".to_string())?;
            let result_slot =
                self.create_entry_alloca(fn_val, "https.serve.result", result_ty.into());

            let rc_zero = self
                .builder
                .build_int_compare(
                    inkwell::IntPredicate::EQ,
                    rc_i32,
                    self.context.i32_type().const_int(0, false),
                    "rc.is_zero",
                )
                .unwrap();
            let ok_bb = self.context.append_basic_block(fn_val, "https.serve.h.ok");
            let err_bb = self.context.append_basic_block(fn_val, "https.serve.h.err");
            let cont_bb = self
                .context
                .append_basic_block(fn_val, "https.serve.h.cont");
            self.builder
                .build_conditional_branch(rc_zero, ok_bb, err_bb)
                .unwrap();

            // Ok arm.
            self.builder.position_at_end(ok_bb);
            let zero_w = i64_ty.const_int(0, false);
            for w in 0..total_fields {
                let elem_ptr = self
                    .builder
                    .build_struct_gep(result_ty, result_slot, w as u32, &format!("ok.w{w}"))
                    .unwrap();
                self.builder.build_store(elem_ptr, zero_w).unwrap();
            }
            self.builder.build_unconditional_branch(cont_bb).unwrap();

            // Err arm.
            self.builder.position_at_end(err_bb);
            let one_w = i64_ty.const_int(1, false);
            let tag_ptr = self
                .builder
                .build_struct_gep(result_ty, result_slot, 0, "err.tag")
                .unwrap();
            self.builder.build_store(tag_ptr, one_w).unwrap();

            let msg = "http: serve_tls failed";
            let msg_global = self
                .builder
                .build_global_string_ptr(msg, "https.serve.h.err.msg")
                .unwrap();
            let msg_len = i64_ty.const_int(msg.len() as u64, false);
            let msg_buf = self
                .builder
                .build_call(self.malloc_fn, &[msg_len.into()], "err.msg.buf")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_pointer_value();
            self.builder
                .build_memcpy(msg_buf, 1, msg_global.as_pointer_value(), 1, msg_len)
                .unwrap();
            let msg_ptr_buf_int = self
                .builder
                .build_ptr_to_int(msg_buf, i64_ty, "err.msg.ptr.i64")
                .unwrap();
            if total_fields > 1 {
                let p1 = self
                    .builder
                    .build_struct_gep(result_ty, result_slot, 1, "err.payload.ptr")
                    .unwrap();
                self.builder.build_store(p1, msg_ptr_buf_int).unwrap();
            }
            if total_fields > 2 {
                let p2 = self
                    .builder
                    .build_struct_gep(result_ty, result_slot, 2, "err.payload.len")
                    .unwrap();
                self.builder.build_store(p2, msg_len).unwrap();
            }
            if total_fields > 3 {
                let p3 = self
                    .builder
                    .build_struct_gep(result_ty, result_slot, 3, "err.payload.cap")
                    .unwrap();
                self.builder.build_store(p3, msg_len).unwrap();
            }
            for w in 4..total_fields {
                let elem_ptr = self
                    .builder
                    .build_struct_gep(result_ty, result_slot, w as u32, &format!("err.w{w}"))
                    .unwrap();
                self.builder.build_store(elem_ptr, zero_w).unwrap();
            }
            self.builder.build_unconditional_branch(cont_bb).unwrap();

            // Cont.
            self.builder.position_at_end(cont_bb);
            let result = self
                .builder
                .build_load(result_ty, result_slot, "https.serve.result.val")
                .unwrap();
            return Ok(result);
        }

        if type_name == "String" && method == "new" {
            let str_ty = self.vec_struct_type();
            let null_ptr = self.context.ptr_type(AddressSpace::default()).const_null();
            let zero = self.context.i64_type().const_int(0, false);
            let mut agg = str_ty.get_undef();
            agg = self
                .builder
                .build_insert_value(agg, null_ptr, 0, "str.data")
                .unwrap()
                .into_struct_value();
            agg = self
                .builder
                .build_insert_value(agg, zero, 1, "str.len")
                .unwrap()
                .into_struct_value();
            agg = self
                .builder
                .build_insert_value(agg, zero, 2, "str.cap")
                .unwrap()
                .into_struct_value();
            return Ok(agg.into());
        }
        // `String.from(x)`: compile the argument as-is. For the dominant
        // call shape `String.from("literal")`, `compile_expr` on a
        // `StringLit` already produces the `{global_ptr, len, cap=0}`
        // static-String aggregate that the rest of the pipeline expects.
        // Without this arm the call falls through every dispatch path
        // below and returns the `i64 0` placeholder — which then poisons
        // every downstream use of the binding (alloca'd as i64, then
        // GEP'd as the 24-byte Vec/String layout for cleanup and method
        // dispatch). The cascading UB lets LLVM DCE strip a program like
        // `let s = String.from("hello"); println(len_plus(5));` down to
        // `printf(undef) + brk #0x1`, which macOS parks at the brk with
        // no debugger — visible as a process spinning at ~57% CPU with
        // zero stdout, which is the 2026-05-29 closure-String "flake"
        // diagnostic that led here.
        if type_name == "String" && method == "from" {
            if let Some(arg) = _args.first() {
                // `From[char] for String` — a single `char` (lowered to i32)
                // becomes a one-glyph owned heap String via UTF-8 encoding, the
                // same path `char.to_string()` uses. Also the target of the
                // `c.into()` desugar. A string-literal / StringSlice / String
                // arg passes through `compile_expr` unchanged (its aggregate is
                // already what the pipeline expects).
                if self.expr_is_char(&arg.value) {
                    let v = self.compile_expr(&arg.value)?;
                    let (ptr, len) = self.emit_codepoint_to_utf8(v.into_int_value());
                    return Ok(self.build_owned_string_from_parts(ptr, len));
                }
                // `String.from(<String>)` returns an OWNED String built by COPYING
                // the source's bytes (the same fresh-buffer path `.to_string()`
                // uses), matching the interpreter's value-copy passthrough. The
                // prior code returned the source aggregate UNCHANGED — an alias of
                // its `{ptr,len,cap}` buffer — so a fresh owned source (an
                // f-string temp `String.from(f"x")` or an owned String binding
                // `String.from(s)`) was freed BOTH by its own scope-exit cleanup
                // and by the result binding: `free(): double free detected in
                // tcache 2` (B-2026-07-13-8). A string-literal / StringSlice
                // source stayed clean by luck (`cap == 0`, its free is a no-op);
                // copying keeps those correct and independent too. The copy is the
                // owning contract of `From` — the result outlives the argument.
                let v = self.compile_expr(&arg.value)?.into_struct_value();
                let data = self
                    .builder
                    .build_extract_value(v, 0, "sf.data")
                    .unwrap()
                    .into_pointer_value();
                let len = self
                    .builder
                    .build_extract_value(v, 1, "sf.len")
                    .unwrap()
                    .into_int_value();
                return Ok(self.build_owned_string_from_parts(data, len));
            }
        }
        // `String.from_utf8(bytes: Vec[u8]) -> Result[String, Utf8Error]` —
        // UTF-8-validating constructor (was interpreter-only, B-2026-06-18-11).
        // Validates + copies the bytes into a fresh String; reuses the
        // CStr.to_string runtime validator. Unblocks the Relay request-line
        // parse (read bytes -> Vec[u8] -> from_utf8 -> split/route).
        if type_name == "String" && method == "from_utf8" {
            if let Some(arg) = _args.first() {
                return self.compile_string_from_utf8(&arg.value);
            }
        }
        // `CStr.from_ptr(p: *const u8) -> ref CStr` — wrap a raw, caller-
        // owned C string pointer as the same `{ptr, len}` aggregate a
        // `c"..."` literal lowers to (`slice_struct_type`, see
        // `exprs.rs` `CStringLit`): field 0 is the pointer verbatim,
        // field 1 is `len` *excluding* the NUL, computed at runtime by
        // libc `strlen` (declared in `Codegen::new`) — the O(N)-walk
        // length the design describes for a runtime-constructed `CStr`.
        // Unsafe: the caller asserts `p` is non-null and NUL-terminated;
        // the `unsafe { ... }` wrap is enforced at the call site by the
        // `unsafe_op_in_unsafe_fn` lint. Seeds the self-hosted codegen's
        // outbound `char*` → owned-`String` read path (LLVM-C FFI spike
        // sub-q 4): `unsafe { CStr.from_ptr(p) }.to_string()`.
        if type_name == "CStr" && method == "from_ptr" && _args.len() == 1 {
            let ptr_val = self.compile_expr(&_args[0].value)?;
            let ptr = ptr_val.into_pointer_value();
            let strlen_fn = self
                .module
                .get_function("strlen")
                .expect("strlen declared in Codegen::new");
            let len = self
                .builder
                .build_call(strlen_fn, &[ptr.into()], "cstr.from_ptr.len")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_int_value();
            let slice_ty = self.slice_struct_type();
            let mut agg = slice_ty.get_undef();
            agg = self
                .builder
                .build_insert_value(agg, ptr, 0, "cstr.from_ptr.ptr")
                .unwrap()
                .into_struct_value();
            agg = self
                .builder
                .build_insert_value(agg, len, 1, "cstr.from_ptr.len")
                .unwrap()
                .into_struct_value();
            return Ok(agg.into());
        }
        // Qualified enum-variant constructor: `Enum.Variant(args)`.
        // The bare-name path (`Variant(args)`) is handled by
        // `try_compile_enum_variant` from `compile_call`; the qualified
        // form lands here. Look up the layout for `type_name`, verify
        // `method` is one of its variants, and dispatch through the
        // shared variant-construction helper. Compound-payload enum
        // codegen (Slice CP) makes this path matter for round-tripping
        // String / Vec / user-struct payloads.
        if let Some(layout) = self.enum_layouts.get(type_name) {
            if layout.tags.contains_key(method) {
                if let Some(v) = self.try_compile_enum_variant(method, Some(type_name), _args)? {
                    return Ok(v);
                }
            }
        }
        // User impl-block method: if a function named `Type.method` exists
        // in the module (declared by the impl-block pass in `compile`),
        // route the call there. Covers both source-form `Type.method(args)`
        // and the operator-lowered `Call(Path([Type, method]))` form.
        let qualified = format!("{}.{}", type_name, method);
        if let Some(fn_val) = self.module.get_function(&qualified) {
            let ref_flags = self
                .fn_param_ref
                .get(&qualified)
                .cloned()
                .unwrap_or_default();
            let param_tensor_infos = self.fn_param_tensor_info.get(&qualified).cloned();
            let mut compiled_args: Vec<BasicMetadataValueEnum<'ctx>> = Vec::new();
            for (i, a) in _args.iter().enumerate() {
                let is_ref = ref_flags.get(i).copied().unwrap_or(false);
                // Thread the callee's DECLARED tensor element type into
                // `pending_let_tensor_info` for the duration of this arg so a
                // `Tensor.{from,zeros,ones,full}` argument lays its data out at
                // the expected element width (e.g. `TensorVar.leaf(t,
                // Tensor.from([-1.0, 2.0]))` into a `ref Tensor[f32, …]` param —
                // without this the unsuffixed f64 literals produce an 8-byte
                // block that the f32 tape misreads). Save/restore so an
                // enclosing let / a non-tensor arg is unaffected. B-2026-07-18-10.
                let param_tensor = param_tensor_infos
                    .as_ref()
                    .and_then(|v| v.get(i).cloned().flatten());
                let is_tensor_param = param_tensor.is_some();
                let saved_pending_tensor = param_tensor.map(|info| {
                    let prev = self.pending_let_tensor_info.take();
                    self.pending_let_tensor_info = Some(info);
                    prev
                });
                // A materialized FRESH-OWNED tensor temp (`Tensor.from(…)` /
                // `Tensor.zeros(…)` / a transform call — never a borrow-return)
                // passed to a `ref Tensor` param has no other cleanup owner, so
                // register its block for scope-exit `FreeTensor` — the tensor
                // sibling of the `track_vec_var` call `materialize_rvalue_for_ref_arg`
                // already makes for a Vec/String temp (else the block leaks once
                // per call; B-2026-07-18-9). Only for the materialize path (a
                // place / index-borrow / identifier arg is owned elsewhere).
                let track_tensor_temp =
                    is_tensor_param && self.expr_yields_fresh_owned_temp(&a.value);
                let arg_meta: BasicMetadataValueEnum<'ctx> = if is_ref {
                    // Ref param: pass a POINTER to caller-side data, not the
                    // loaded value — mirroring the free-fn ref-arg path in
                    // `compile_call`. An Identifier place forwards its data
                    // ptr; a `vec[idx]` place forwards the element ptr; any
                    // other RVALUE (a fresh temp — `Type.from(...)`, an
                    // f-string, an arithmetic result) is MATERIALIZED into a
                    // slot so the callee's `ref` param receives a pointer.
                    // Without the rvalue-materialization fallback the assoc-call
                    // path passed a fresh-temp arg BY VALUE: for String/Vec an
                    // LLVM verifier mismatch (`{ptr,i64,i64}` vs `ptr`); for
                    // Tensor (whose value is already a `ptr`) it type-checked
                    // but the callee dereferenced the block's rank word as a
                    // pointer → SIGSEGV (the `TensorVar.leaf(t, Tensor.from(…))`
                    // autograd crash, B-2026-07-18-9). The free-fn call path
                    // (`compile_call`) already did this; only the
                    // `Type.method(...)` assoc-call path was missing it.
                    if let ExprKind::Identifier(var_name) = &a.value.kind {
                        if let Some(ptr) = self.get_data_ptr(var_name) {
                            ptr.into()
                        } else {
                            let val = self.compile_expr(&a.value)?;
                            let slot = self.materialize_rvalue_for_ref_arg(val, i);
                            if track_tensor_temp {
                                self.track_tensor_var(slot.into_pointer_value());
                            }
                            slot.into()
                        }
                    } else if let Some(elem_ptr) = self.ref_arg_index_borrow_ptr(&a.value)? {
                        elem_ptr.into()
                    } else {
                        let val = self.compile_expr(&a.value)?;
                        let slot = self.materialize_rvalue_for_ref_arg(val, i);
                        if track_tensor_temp {
                            self.track_tensor_var(slot.into_pointer_value());
                        }
                        slot.into()
                    }
                } else {
                    let val = self.compile_expr(&a.value)?;
                    // `Option[shared T]` arg-share discipline — mirrors the
                    // free-fn call path in `compile_call`: a tracked
                    // Identifier binding gets a tag+null-guarded inner inc so
                    // the callee receives an independent +1 (its param
                    // `RcDecOption` decs at exit; the caller's binding keeps
                    // its own +1 for its scope-exit dec); a FieldAccess arg
                    // reading an `Option[shared T]` field gets the loaded
                    // inner inc'd (the niche field read is a bare ptr load).
                    // Without these, passing the same binding twice — or
                    // reusing it after the call — read freed memory (probe
                    // `m.total(chain); m.total(chain)`, 2026-06-05,
                    // pre-existing on the conventional ABI).
                    self.share_option_shared_ref_for_arg(&a.value);
                    self.share_option_shared_field_ref_for_arg(&a.value, val);
                    val.into()
                };
                if let Some(prev) = saved_pending_tensor {
                    self.pending_let_tensor_info = prev;
                }
                compiled_args.push(arg_meta);
            }
            // Niche-ABI pack/unpack at the static `Type.method(...)`
            // boundary — positions are 1:1 with the declared params
            // (a self-taking method lowered through this form passes the
            // receiver as the first source arg, matching param 0).
            self.pack_niche_abi_args(&qualified, &mut compiled_args);
            let call_site = self
                .builder
                .build_call(fn_val, &compiled_args, "usercall")
                .unwrap();
            let basic_val = call_site.try_as_basic_value();
            return if basic_val.is_instruction() {
                Ok(self.context.i64_type().const_int(0, false).into())
            } else {
                Ok(self.unpack_niche_abi_ret(&qualified, basic_val.unwrap_basic()))
            };
        }

        // `<Prim>.default()` / `String.default()` — the built-in zero value for
        // a primitive or String `T` bound in a monomorph (std.mem
        // `take[T: Default]`, any `fn f[T: Default]`). Named user types with a
        // derived/hand-written `default` dispatch through the
        // `module.get_function(qualified)` block above; primitives have no such
        // function, so without this they fall through to the i64-zero default
        // at the tail of this method — correct for i64 but a MISCOMPILE for
        // String (an `i64 0` store zeros only the first 8 of the 24-byte
        // `{ptr,len,cap}` header, leaving a dangling `{null, old_len, old_cap}`
        // that reads freed/garbage bytes), and wrong-width for `f32`/`f64` /
        // `bool` / `char`. Produce the properly-typed zero. B-2026-07-08-25.
        if method == "default" && args.is_empty() {
            if let Some(v) = self.primitive_default_value(type_name) {
                return Ok(v);
            }
        }

        // `Vec.with_capacity(n: i64) -> Vec[T]` — empty Vec (len=0)
        // with pre-allocated capacity n, so subsequent push calls don't
        // grow until the (n+1)-th. Element type recovery: with_capacity
        // has no value arg, so `T` must come from the destination
        // binding's annotation — `compile_stmt` (stmts.rs around the
        // `compile_expr(value)` call) threads the binding's
        // `vec_elem_types[var]` lookup through `pending_let_elem_type`
        // for exactly this case. Untyped usage (`let v = Vec.with_capacity(8); v.push(...)`)
        // would need the typechecker's inferred-type table; not
        // supported here — requires an explicit `let v: Vec[T] = ...`
        // annotation.
        //
        // `VecDeque.with_capacity(n)` rides the same arm: VecDeque shares
        // Vec's `{ptr, len, cap}` storage and its element type flows through
        // `pending_let_elem_type` identically (`vec_inner_type_expr` peels
        // both `Vec[T]` and `VecDeque[T]`). Without it, a `VecDeque`-typed
        // binding fell through to the `Ok(const 0)` default and crashed
        // (B-2026-06-10-3).
        if (type_name == "Vec" || type_name == "VecDeque") && method == "with_capacity" {
            if args.len() != 1 {
                return Err(format!(
                    "{type_name}.with_capacity expects 1 argument (capacity), got {}",
                    args.len()
                ));
            }
            let elem_ty = self.pending_let_elem_type.ok_or_else(|| {
                format!(
                    "{type_name}.with_capacity: element type unknown — requires a \
                     `let v: {type_name}[T] = ...` annotation"
                )
            })?;
            // Normalize the count to i64: on wasm32 a `.len()`-derived capacity
            // arrives as i32, but the byte-size multiply, the `cap` field, and
            // the allocator param are all i64 — an un-widened i32 trips an LLVM
            // type mismatch. Surfaced when pre-sizing (`presize.rs`) began
            // injecting `with_capacity(<i32 bound>)` on wasm targets.
            let n_val = self.compile_expr(&args[0].value)?;
            let n = self.coerce_to_i64(n_val)?;
            let elem_size = elem_ty.size_of().unwrap();
            // User-controlled count: overflow-checked multiply, else a huge
            // `n` wraps to a tiny allocation with `cap = n` recorded — heap
            // overflow on the first pushes (see `checked_alloc_bytes`).
            let alloc_bytes = self.checked_alloc_bytes(n, elem_size, "with_cap")?;
            // A null buffer when `alloc_bytes == 0` — otherwise the zero-cap Vec
            // (`cap = 0`) would own a non-null 1-byte allocation the drop path
            // skips freeing (B-2026-07-11-15).
            let buf = self.with_capacity_buffer_or_null(alloc_bytes, "with_cap.buf");

            // Build {data=buf, len=0, cap=n} aggregate. `len = 0` is the
            // key difference from `Vec.filled`: capacity is reserved but
            // the Vec is logically empty, so the first n pushes hit the
            // pre-allocated slots without triggering grow.
            let vec_ty = self.vec_struct_type();
            let zero = self.context.i64_type().const_int(0, false);
            let mut agg = vec_ty.get_undef();
            agg = self
                .builder
                .build_insert_value(agg, buf, 0, "vec.data")
                .unwrap()
                .into_struct_value();
            agg = self
                .builder
                .build_insert_value(agg, zero, 1, "vec.len")
                .unwrap()
                .into_struct_value();
            agg = self
                .builder
                .build_insert_value(agg, n, 2, "vec.cap")
                .unwrap()
                .into_struct_value();
            return Ok(agg.into());
        }

        // `String.with_capacity(n: i64) -> String` — empty String (len=0)
        // reserving n bytes. String shares Vec's `{ptr, len, cap}` shape but
        // its element is always a byte (`u8`), so — unlike the Vec/VecDeque
        // arm — no `pending_let_elem_type` is needed: the allocation is
        // exactly `n` bytes. Without this arm a `String`-typed binding fell
        // through to the `Ok(const 0)` default and crashed (B-2026-06-10-3).
        if type_name == "String" && method == "with_capacity" {
            if args.len() != 1 {
                return Err(format!(
                    "String.with_capacity expects 1 argument (capacity), got {}",
                    args.len()
                ));
            }
            // Normalize the count to i64 (wasm32 `.len()` bounds are i32) — same
            // width fix as the Vec arm above; the `cap` field and the allocator
            // param are i64.
            let n_val = self.compile_expr(&args[0].value)?;
            let n = self.coerce_to_i64(n_val)?;
            // Byte element → cap bytes == n; reserve via the panicking
            // allocator (matches the panicking `Vec.with_capacity` policy). A
            // null buffer when `n == 0` — a zero-cap String (`cap = 0`) must not
            // own a non-null buffer the drop path skips freeing (B-2026-07-11-15).
            let buf = self.with_capacity_buffer_or_null(n, "str_with_cap.buf");
            let vec_ty = self.vec_struct_type();
            let zero = self.context.i64_type().const_int(0, false);
            let mut agg = vec_ty.get_undef();
            agg = self
                .builder
                .build_insert_value(agg, buf, 0, "str.data")
                .unwrap()
                .into_struct_value();
            agg = self
                .builder
                .build_insert_value(agg, zero, 1, "str.len")
                .unwrap()
                .into_struct_value();
            agg = self
                .builder
                .build_insert_value(agg, n, 2, "str.cap")
                .unwrap()
                .into_struct_value();
            return Ok(agg.into());
        }

        // `Vec.try_with_capacity(n: i64) -> Result[Vec[T], AllocError]` —
        // fallible `with_capacity` (phase-8-stdlib-floor item 8). Same empty
        // `{data, len=0, cap=n}` Vec as `with_capacity`, but the reservation
        // goes through `karac_alloc_fallible`: a null result short-circuits to
        // `Result.Err(AllocError.OutOfMemory{requested_bytes})` and the success
        // path wraps the Vec in `Result.Ok(_)`. Element-type recovery is the
        // crux: the zero-arg constructor can't get `T` from an argument, and
        // the destination binding is a `Result[Vec[T], _]` (not a `Vec[T]`),
        // so the `let` path threads `T` from the annotation's Ok payload into
        // `pending_let_elem_type` (see `stmts.rs`, the `result_ok_collection_
        // elem_type` recovery). The `?`-unwrap form (`let v: Vec[T] =
        // try_with_capacity(n)?`) already carries `T` via `vec_elem_types[v]`.
        // `VecDeque.try_with_capacity` rides this arm (shared Vec storage +
        // identical element-type recovery), now that its panicking base
        // `VecDeque.with_capacity` codegens (B-2026-06-10-3).
        if (type_name == "Vec" || type_name == "VecDeque") && method == "try_with_capacity" {
            if args.len() != 1 {
                return Err(format!(
                    "{type_name}.try_with_capacity expects 1 argument (capacity), got {}",
                    args.len()
                ));
            }
            let elem_ty = self.pending_let_elem_type.ok_or_else(|| {
                format!(
                    "{type_name}.try_with_capacity: element type unknown — requires a \
                     `let v: Result[{type_name}[T], AllocError] = ...` (or `let v: {type_name}[T] = ...?`) annotation"
                )
            })?;
            let n = self.compile_expr(&args[0].value)?.into_int_value();
            let elem_size = elem_ty.size_of().unwrap();
            // User-controlled count — same overflow-checked multiply as the
            // panicking `with_capacity` arm. The wrap case panics rather
            // than returning Err: it is a bogus count (only expressible with
            // a count whose BYTE size exceeds u64), not a recoverable OOM.
            let alloc_bytes = self.checked_alloc_bytes(n, elem_size, "try_with_cap")?;
            // Null buffer + non-OOM for a zero-byte reservation (B-2026-07-11-15).
            let (buf, is_oom) = self.fallible_with_capacity_buffer(alloc_bytes, "try_with_cap.buf");

            let fn_val = self.current_fn.unwrap();
            let ok_bb = self.context.append_basic_block(fn_val, "twc.ok");
            let oom_bb = self.context.append_basic_block(fn_val, "twc.oom");
            let merge_bb = self.context.append_basic_block(fn_val, "twc.merge");
            self.builder
                .build_conditional_branch(is_oom, oom_bb, ok_bb)
                .unwrap();

            // Alloc succeeded: build the empty {data=buf, len=0, cap=n} Vec,
            // wrap in Result.Ok(_).
            self.builder.position_at_end(ok_bb);
            let vec_ty = self.vec_struct_type();
            let zero = self.context.i64_type().const_int(0, false);
            let mut agg = vec_ty.get_undef();
            agg = self
                .builder
                .build_insert_value(agg, buf, 0, "vec.data")
                .unwrap()
                .into_struct_value();
            agg = self
                .builder
                .build_insert_value(agg, zero, 1, "vec.len")
                .unwrap()
                .into_struct_value();
            agg = self
                .builder
                .build_insert_value(agg, n, 2, "vec.cap")
                .unwrap()
                .into_struct_value();
            let ok_result = self.build_nonshared_enum_value("Result", "Ok", &[agg.into()])?;
            let ok_end = self.builder.get_insert_block().unwrap();
            self.builder.build_unconditional_branch(merge_bb).unwrap();

            // OOM → Result.Err(AllocError.OutOfMemory{requested_bytes}).
            self.builder.position_at_end(oom_bb);
            let err_result = self.build_alloc_oom_result(alloc_bytes)?;
            let oom_end = self.builder.get_insert_block().unwrap();
            self.builder.build_unconditional_branch(merge_bb).unwrap();

            // Merge the two `Result` aggregates.
            self.builder.position_at_end(merge_bb);
            let phi = self
                .builder
                .build_phi(ok_result.get_type(), "twc.result")
                .unwrap();
            phi.add_incoming(&[(&ok_result, ok_end), (&err_result, oom_end)]);
            return Ok(phi.as_basic_value());
        }

        // `String.try_with_capacity(n) -> Result[String, AllocError]` —
        // fallible `String.with_capacity` (phase-8-stdlib-floor item 8). Byte
        // element (`u8`), so the reservation is exactly `n` bytes and no
        // `pending_let_elem_type` is needed (cf. the panicking String arm).
        // Null fallible-alloc → `Result.Err(AllocError.OutOfMemory{n})`,
        // success → `Result.Ok({buf, len=0, cap=n})`.
        if type_name == "String" && method == "try_with_capacity" {
            if args.len() != 1 {
                return Err(format!(
                    "String.try_with_capacity expects 1 argument (capacity), got {}",
                    args.len()
                ));
            }
            let n = self.compile_expr(&args[0].value)?.into_int_value();
            // Byte element → `n` bytes. Null buffer + non-OOM for a zero-byte
            // reservation (B-2026-07-11-15).
            let (buf, is_oom) = self.fallible_with_capacity_buffer(n, "str_try_with_cap.buf");

            let fn_val = self.current_fn.unwrap();
            let ok_bb = self.context.append_basic_block(fn_val, "stwc.ok");
            let oom_bb = self.context.append_basic_block(fn_val, "stwc.oom");
            let merge_bb = self.context.append_basic_block(fn_val, "stwc.merge");
            self.builder
                .build_conditional_branch(is_oom, oom_bb, ok_bb)
                .unwrap();

            self.builder.position_at_end(ok_bb);
            let vec_ty = self.vec_struct_type();
            let zero = self.context.i64_type().const_int(0, false);
            let mut agg = vec_ty.get_undef();
            agg = self
                .builder
                .build_insert_value(agg, buf, 0, "str.data")
                .unwrap()
                .into_struct_value();
            agg = self
                .builder
                .build_insert_value(agg, zero, 1, "str.len")
                .unwrap()
                .into_struct_value();
            agg = self
                .builder
                .build_insert_value(agg, n, 2, "str.cap")
                .unwrap()
                .into_struct_value();
            let ok_result = self.build_nonshared_enum_value("Result", "Ok", &[agg.into()])?;
            let ok_end = self.builder.get_insert_block().unwrap();
            self.builder.build_unconditional_branch(merge_bb).unwrap();

            self.builder.position_at_end(oom_bb);
            let err_result = self.build_alloc_oom_result(n)?;
            let oom_end = self.builder.get_insert_block().unwrap();
            self.builder.build_unconditional_branch(merge_bb).unwrap();

            self.builder.position_at_end(merge_bb);
            let phi = self
                .builder
                .build_phi(ok_result.get_type(), "stwc.result")
                .unwrap();
            phi.add_incoming(&[(&ok_result, ok_end), (&err_result, oom_end)]);
            return Ok(phi.as_basic_value());
        }

        // `Vec.filled(n: i64, val: T) -> Vec[T]` — produces n copies of
        // val. Spec at design.md:1631. Codegen: malloc(n * sizeof(elem)),
        // loop i=0..n filling each slot with `val`, return
        // `{data=buf, len=n, cap=n}`. Without this, the assoc-call falls
        // through to the default i64 zero return and the let-binding
        // allocates an i64-sized alloca for a Vec-typed binding —
        // `v.len()` then GEPs past the alloca into stack garbage, the
        // scope-exit cleanup `free`s a garbage pointer, and the binary
        // exits SIGTRAP / SIGSEGV.
        //
        // Heap-backed element types (`Vec[Vec[_]]`, `Vec[String]`) deep-clone
        // per slot — `build_vec_filled` moves `val` into slot 0 and clones it
        // into the rest, so each row owns a distinct buffer (was a bit-copy that
        // aliased one backing buffer across all N slots → corruption + N-fold
        // free / AOT SIGTRAP, B-2026-06-19-8). The destination element TypeExpr
        // is threaded via `pending_let_elem_type_expr`, consumed below before
        // the fill argument is compiled so a nested inner `Vec.filled(...)`
        // doesn't inherit this binding's element type.
        if type_name == "Vec" && method == "filled" {
            if args.len() < 2 {
                return Err("Vec.filled requires 2 arguments (n, val)".to_string());
            }
            // Consume the destination element TypeExpr BEFORE compiling the fill
            // argument, so a nested inner `Vec.filled(...)` (compiled as that
            // argument) does not inherit this outer binding's element type.
            let elem_te = self.pending_let_elem_type_expr.take();
            let n = self.compile_expr(&args[0].value)?.into_int_value();
            let val = self.compile_expr(&args[1].value)?;
            return self.build_vec_filled(n, val, elem_te);
        }

        // `Vec.from_slice(src: Slice[T]) -> Vec[T]` — bulk-copy a slice
        // (also accepts Array / Vec via the existing `coerce_to_slice`
        // shape recognition) into a freshly-allocated Vec. One malloc +
        // one memcpy/clone-loop, vs the `Vec.new() + push-in-loop` shape
        // which grow-and-reallocs ~log2(n) times. Two source shapes are
        // supported:
        //   1. Identifier (`Vec.from_slice(src)`) — element type comes
        //      from the source binding's `slice_elem_types` /
        //      `vec_elem_types` registration or its Array slot type.
        //   2. Nested-Index (`Vec.from_slice(rows[r])`) on
        //      `Vec[Vec[T]]` — element type comes from the outer
        //      binding's `var_elem_type_exprs` entry (unwraps one
        //      Vec layer to get the inner T). Mirrors the same
        //      fallback shape in `Vec.extend_from_slice` (commit
        //      9d9c3ce) so kata 6's `rows[r]` usage works uniformly.
        // Other shapes (Index on Array, MethodCall returning a slice,
        // etc.) fall through to the existing "could not coerce" error.
        if type_name == "Vec" && method == "from_slice" {
            if args.len() != 1 {
                return Err(format!(
                    "Vec.from_slice expects 1 argument (source slice / vec / array), got {}",
                    args.len()
                ));
            }
            let arg = &args[0].value;
            let (elem_ty, src_elem_te, src_data, src_len) = self.recover_from_slice_src(arg)?;

            let elem_size = elem_ty.size_of().unwrap();
            let alloc_bytes = self
                .builder
                .build_int_mul(src_len, elem_size, "from_slice.bytes")
                .unwrap();
            let new_buf = self
                .builder
                .build_call(
                    self.alloc_or_panic_fn,
                    &[alloc_bytes.into()],
                    "from_slice.buf",
                )
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_pointer_value();

            self.copy_from_slice_elems(
                elem_ty,
                &src_elem_te,
                src_data,
                new_buf,
                src_len,
                alloc_bytes,
            );

            let vec_ty = self.vec_struct_type();
            let mut agg = vec_ty.get_undef();
            agg = self
                .builder
                .build_insert_value(agg, new_buf, 0, "vec.data")
                .unwrap()
                .into_struct_value();
            agg = self
                .builder
                .build_insert_value(agg, src_len, 1, "vec.len")
                .unwrap()
                .into_struct_value();
            agg = self
                .builder
                .build_insert_value(agg, src_len, 2, "vec.cap")
                .unwrap()
                .into_struct_value();
            return Ok(agg.into());
        }

        // `Vec.try_from_slice(src) -> Result[Vec[T], AllocError]` — fallible
        // `from_slice` (phase-8-stdlib-floor item 8). Same source recovery +
        // element copy as `from_slice`, but the single backing allocation goes
        // through `karac_alloc_fallible`: a null result short-circuits to
        // `Result.Err(AllocError.OutOfMemory{requested_bytes})` and the success
        // path wraps the freshly-built `Vec` aggregate in `Result.Ok(_)`. The
        // `Vec`-in-`Result` payload (3 words) packs inline into the Result's
        // 5-word payload area via the standard `coerce_to_payload_words` path
        // (well under the oversized-box threshold), so it round-trips through
        // match-extraction and scope-exit drop exactly like any other
        // `Result[Vec[T], _]` value — no special drop handling needed here.
        if type_name == "Vec" && method == "try_from_slice" {
            if args.len() != 1 {
                return Err(format!(
                    "Vec.try_from_slice expects 1 argument (source slice / vec / array), got {}",
                    args.len()
                ));
            }
            let arg = &args[0].value;
            let (elem_ty, src_elem_te, src_data, src_len) = self.recover_from_slice_src(arg)?;

            let elem_size = elem_ty.size_of().unwrap();
            let alloc_bytes = self
                .builder
                .build_int_mul(src_len, elem_size, "try_from_slice.bytes")
                .unwrap();
            let new_buf = self
                .builder
                .build_call(
                    self.alloc_fallible_fn,
                    &[alloc_bytes.into()],
                    "try_from_slice.buf",
                )
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_pointer_value();

            let fn_val = self.current_fn.unwrap();
            let ok_bb = self.context.append_basic_block(fn_val, "tfs.ok");
            let oom_bb = self.context.append_basic_block(fn_val, "tfs.oom");
            let merge_bb = self.context.append_basic_block(fn_val, "tfs.merge");
            let is_null = self.builder.build_is_null(new_buf, "tfs.is_null").unwrap();
            self.builder
                .build_conditional_branch(is_null, oom_bb, ok_bb)
                .unwrap();

            // Alloc succeeded: copy elements, build the Vec aggregate, wrap in
            // Result.Ok(_).
            self.builder.position_at_end(ok_bb);
            self.copy_from_slice_elems(
                elem_ty,
                &src_elem_te,
                src_data,
                new_buf,
                src_len,
                alloc_bytes,
            );
            let vec_ty = self.vec_struct_type();
            let mut agg = vec_ty.get_undef();
            agg = self
                .builder
                .build_insert_value(agg, new_buf, 0, "vec.data")
                .unwrap()
                .into_struct_value();
            agg = self
                .builder
                .build_insert_value(agg, src_len, 1, "vec.len")
                .unwrap()
                .into_struct_value();
            agg = self
                .builder
                .build_insert_value(agg, src_len, 2, "vec.cap")
                .unwrap()
                .into_struct_value();
            let ok_result = self.build_nonshared_enum_value("Result", "Ok", &[agg.into()])?;
            let ok_end = self.builder.get_insert_block().unwrap();
            self.builder.build_unconditional_branch(merge_bb).unwrap();

            // OOM → Result.Err(AllocError.OutOfMemory{requested_bytes}).
            self.builder.position_at_end(oom_bb);
            let err_result = self.build_alloc_oom_result(alloc_bytes)?;
            let oom_end = self.builder.get_insert_block().unwrap();
            self.builder.build_unconditional_branch(merge_bb).unwrap();

            // Merge the two `Result` aggregates.
            self.builder.position_at_end(merge_bb);
            let phi = self
                .builder
                .build_phi(ok_result.get_type(), "tfs.result")
                .unwrap();
            phi.add_incoming(&[(&ok_result, ok_end), (&err_result, oom_end)]);
            return Ok(phi.as_basic_value());
        }

        // `Atomic.new(v)` — transparent constructor for the `Atomic[T]`
        // wrapper. At the LLVM level `Atomic[T]` IS `T` (see the Atomic
        // arm in `llvm_type_for_type_expr`); the constructor just
        // forwards its argument's value, which the let-binding then
        // stores into a primitive-typed alloca. Subsequent
        // `.load(ord)` / `.store(v, ord)` on the binding lower to
        // `load atomic` / `store atomic` against that same alloca (see
        // the Atomic arm in `compile_method_call`).
        // **`Atomic[bool]` widens to i8 here** — LLVM rejects atomic
        // load/store on `i1`, so when the arg compiles to a bool value
        // (i1) we zext to i8 before handing the value off. The matched
        // trunc on `.load` / zext on `.store` is in `compile_atomic_method`.
        if type_name == "Atomic" && method == "new" {
            if let Some(arg) = _args.first() {
                let val = self.compile_expr(&arg.value)?;
                if let BasicValueEnum::IntValue(iv) = val {
                    if iv.get_type().get_bit_width() == 1 {
                        let widened = self
                            .builder
                            .build_int_z_extend(iv, self.context.i8_type(), "atomic.bool.zext")
                            .unwrap();
                        return Ok(widened.into());
                    }
                }
                return Ok(val);
            }
            return Err("Atomic.new requires an initial value argument".to_string());
        }

        // `VolatileCell.new(v)` — transparent constructor for the MMIO wrapper.
        // Like `Atomic[T]`, `VolatileCell[T]` IS `T` at the LLVM level (see the
        // arm in `llvm_type_for_type_expr`), so the constructor just forwards
        // its argument's value; the let-binding stores it into the primitive
        // alloca that subsequent `.read()` / `.write(v)` volatile-access.
        if type_name == "VolatileCell" && method == "new" {
            if let Some(arg) = _args.first() {
                return self.compile_expr(&arg.value);
            }
            return Err("VolatileCell.new requires an initial value argument".to_string());
        }

        // `Mutex.new(v)` — builds the spinlock-guarded cell aggregate
        // `{ i64 lockflag = 0, T value = v }` (layout per `llvm_type_for_type_expr`'s
        // Mutex arm). The unlocked state is lockflag = 0. `lock m { ... }` later
        // TAS-spins on field 0 and exposes field 1 as the `mut ref T` body alias.
        if type_name == "Mutex" && method == "new" {
            if let Some(arg) = _args.first() {
                let val = self.compile_expr(&arg.value)?;
                let i64_t = self.context.i64_type();
                let mutex_ty = self
                    .context
                    .struct_type(&[i64_t.into(), val.get_type()], false);
                let mut agg = mutex_ty.get_undef();
                agg = self
                    .builder
                    .build_insert_value(agg, i64_t.const_zero(), 0, "mutex.flag")
                    .unwrap()
                    .into_struct_value();
                agg = self
                    .builder
                    .build_insert_value(agg, val, 1, "mutex.val")
                    .unwrap()
                    .into_struct_value();
                return Ok(agg.into());
            }
            return Err("Mutex.new requires an initial value argument".to_string());
        }

        if (type_name == "Vec" || type_name == "VecDeque") && method == "new" {
            // `VecDeque.new()` lowers to the same zero-initialized
            // `{ptr=null, len=0, cap=0}` aggregate as `Vec.new()` —
            // codegen aliases VecDeque onto Vec's storage layout, with
            // `push_front` / `pop_front` translating to memmove-shifted
            // insert/remove at index 0 inside `compile_vec_method`.
            let vec_ty = self.vec_struct_type();
            let null_ptr = self.context.ptr_type(AddressSpace::default()).const_null();
            let zero = self.context.i64_type().const_int(0, false);
            let mut agg = vec_ty.get_undef();
            agg = self
                .builder
                .build_insert_value(agg, null_ptr, 0, "vec.data")
                .unwrap()
                .into_struct_value();
            agg = self
                .builder
                .build_insert_value(agg, zero, 1, "vec.len")
                .unwrap()
                .into_struct_value();
            agg = self
                .builder
                .build_insert_value(agg, zero, 2, "vec.cap")
                .unwrap()
                .into_struct_value();
            return Ok(agg.into());
        }
        Ok(self.context.i64_type().const_int(0, false).into())
    }

    /// Allocate a `with_capacity` data buffer of `alloc_bytes`, returning a
    /// **null** pointer when the byte count is zero rather than the allocator's
    /// zero-normalized 1-byte buffer.
    ///
    /// `karac_alloc_or_panic(0)` normalizes `0 → 1` and hands back a real,
    /// non-null heap pointer (`alloc.rs`, deliberate: a non-null result is the
    /// success signal). But a zero-capacity collection stores `cap = 0`, and the
    /// `cap > 0 ⇔ owned heap` drop convention (`clone_drop.rs`) *skips* freeing a
    /// `cap == 0` buffer — treating it as a static-literal / borrowed view — so
    /// that 1-byte allocation would leak. This bites `with_capacity(n)` whenever
    /// `n` evaluates to 0 at runtime, which the `presize.rs` pass makes common by
    /// rewriting `let mut v = Vec.new(); while i < k { v.push(..) }` to
    /// `Vec.with_capacity(k)` — a `k == 0` counted loop then leaks one byte per
    /// call (B-2026-07-11-15).
    ///
    /// Producing a null data pointer for the zero-byte case makes
    /// `with_capacity(0)` bit-identical to `Vec.new()` (`{null, 0, 0}`): the drop
    /// no-ops and the first push grows from null (`realloc(null, _) == malloc`).
    /// For a compile-time-constant nonzero `alloc_bytes` LLVM folds the `== 0`
    /// test and drops the empty arm, so the common `with_capacity(<literal>)`
    /// keeps its single unconditional allocation.
    fn with_capacity_buffer_or_null(
        &mut self,
        alloc_bytes: IntValue<'ctx>,
        label: &str,
    ) -> PointerValue<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let fn_val = self.current_fn.unwrap();
        let is_zero = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::EQ,
                alloc_bytes,
                self.context.i64_type().const_zero(),
                "with_cap.empty",
            )
            .unwrap();
        let alloc_bb = self.context.append_basic_block(fn_val, "with_cap.alloc");
        let empty_bb = self.context.append_basic_block(fn_val, "with_cap.zero");
        let cont_bb = self.context.append_basic_block(fn_val, "with_cap.cont");
        self.builder
            .build_conditional_branch(is_zero, empty_bb, alloc_bb)
            .unwrap();

        // Nonzero: a real heap allocation, freed by the owning binding's drop.
        self.builder.position_at_end(alloc_bb);
        let buf = self
            .builder
            .build_call(self.alloc_or_panic_fn, &[alloc_bytes.into()], label)
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        self.builder.build_unconditional_branch(cont_bb).unwrap();
        let alloc_end = self.builder.get_insert_block().unwrap();

        // Zero: no allocation — a null buffer, matching `Vec.new()`.
        self.builder.position_at_end(empty_bb);
        self.builder.build_unconditional_branch(cont_bb).unwrap();

        self.builder.position_at_end(cont_bb);
        let phi = self.builder.build_phi(ptr_ty, "with_cap.buf").unwrap();
        phi.add_incoming(&[(&buf, alloc_end), (&ptr_ty.const_null(), empty_bb)]);
        phi.as_basic_value().into_pointer_value()
    }

    /// Fallible `with_capacity` buffer: reserve `alloc_bytes` via
    /// `karac_alloc_fallible`, returning `(buffer, is_oom)`.
    ///
    /// A zero-byte request is a valid EMPTY reservation, not an allocation
    /// failure — it yields a NULL buffer with `is_oom = false` and emits no
    /// `malloc` at all, so the caller wraps `{null, 0, 0}` in `Result.Ok`
    /// (matching `Vec.new()`, and dodging the same `cap == 0`-skips-free leak the
    /// panicking path guards, B-2026-07-11-15) rather than
    /// `Result.Err(OutOfMemory)`. Only a genuine allocation attempt that returns
    /// null sets `is_oom`.
    fn fallible_with_capacity_buffer(
        &mut self,
        alloc_bytes: IntValue<'ctx>,
        label: &str,
    ) -> (PointerValue<'ctx>, IntValue<'ctx>) {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let bool_ty = self.context.bool_type();
        let fn_val = self.current_fn.unwrap();
        let is_zero = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::EQ,
                alloc_bytes,
                self.context.i64_type().const_zero(),
                "twc.empty",
            )
            .unwrap();
        let alloc_bb = self.context.append_basic_block(fn_val, "twc.alloc");
        let empty_bb = self.context.append_basic_block(fn_val, "twc.zero");
        let cont_bb = self.context.append_basic_block(fn_val, "twc.have_buf");
        self.builder
            .build_conditional_branch(is_zero, empty_bb, alloc_bb)
            .unwrap();

        // Nonzero: attempt the allocation; a null result is a real OOM.
        self.builder.position_at_end(alloc_bb);
        let alloc_buf = self
            .builder
            .build_call(self.alloc_fallible_fn, &[alloc_bytes.into()], label)
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        let alloc_null = self
            .builder
            .build_is_null(alloc_buf, "twc.alloc_null")
            .unwrap();
        self.builder.build_unconditional_branch(cont_bb).unwrap();
        let alloc_end = self.builder.get_insert_block().unwrap();

        // Zero: no allocation, a null buffer — success, never OOM.
        self.builder.position_at_end(empty_bb);
        self.builder.build_unconditional_branch(cont_bb).unwrap();

        self.builder.position_at_end(cont_bb);
        let buf_phi = self.builder.build_phi(ptr_ty, "twc.buf").unwrap();
        buf_phi.add_incoming(&[(&alloc_buf, alloc_end), (&ptr_ty.const_null(), empty_bb)]);
        let oom_phi = self.builder.build_phi(bool_ty, "twc.is_oom").unwrap();
        oom_phi.add_incoming(&[(&alloc_null, alloc_end), (&bool_ty.const_zero(), empty_bb)]);
        (
            buf_phi.as_basic_value().into_pointer_value(),
            oom_phi.as_basic_value().into_int_value(),
        )
    }
}
