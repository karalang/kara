//! SIMD `Vector[T, N]` method lowerings: splat / from_array /
//! from_slice and the elementwise operation surface.
//!
//! Extracted verbatim from `method_call.rs` (structural-debt second-level
//! split). Sibling `impl<'ctx> super::Codegen<'ctx>` block; moved methods
//! are `pub(super)`.

use crate::ast::*;

use inkwell::types::BasicTypeEnum;
use inkwell::values::{BasicValueEnum, IntValue, PointerValue};
use inkwell::AddressSpace;
use inkwell::IntPredicate;

impl<'ctx> super::Codegen<'ctx> {
    /// `Vector[T, N].splat(x)` — broadcast scalar `x` to all `N` lanes
    /// (design.md § Portable SIMD). Compile the scalar once and
    /// `insertelement` it into every lane of an undef `<N x T>`; LLVM folds
    /// the chain into a native broadcast (`shufflevector` w/ zero mask) on
    /// targets that have one.
    pub(super) fn compile_vector_splat(
        &mut self,
        generic_args: &[GenericArg],
        args: &[CallArg],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let vec_ty = self
            .llvm_vector_type(&Some(generic_args.to_vec()))
            .ok_or_else(|| "splat: could not lower Vector[T, N] type".to_string())?;
        let BasicTypeEnum::VectorType(vt) = vec_ty else {
            return Err("splat: lowered type is not an LLVM vector".to_string());
        };
        let scalar = self.compile_expr(&args[0].value)?;
        // Literal-width boundary coercion, same as vector construction:
        // a bare `0.5` / `1` scalar lowers at the literal default width
        // (f64 / i64) and would broadcast a mistyped lane.
        let scalar = self.coerce_scalar_to_type(scalar, vt.get_element_type());
        let i32_ty = self.context.i32_type();
        let mut acc = vt.get_undef();
        for i in 0..vt.get_size() {
            acc = self
                .builder
                .build_insert_element(acc, scalar, i32_ty.const_int(i as u64, false), "splat.lane")
                .map_err(|e| format!("splat insertelement failed: {e}"))?;
        }
        Ok(acc.into())
    }

    /// `Vector[T, N].from_array(a)` — build a `<N x T>` from a fixed `[T; N]`
    /// array (design.md § Portable SIMD). The `N` lane scalars are recovered
    /// and `insertelement`'d into an undef vector. When the argument is a
    /// syntactic array literal the elements are compiled directly (no array
    /// aggregate round-trip); otherwise the argument compiles to an `[N x T]`
    /// aggregate and each lane is pulled out with `extractvalue`.
    pub(super) fn compile_vector_from_array(
        &mut self,
        generic_args: &[GenericArg],
        args: &[CallArg],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let vec_ty = self
            .llvm_vector_type(&Some(generic_args.to_vec()))
            .ok_or_else(|| "from_array: could not lower Vector[T, N] type".to_string())?;
        let BasicTypeEnum::VectorType(vt) = vec_ty else {
            return Err("from_array: lowered type is not an LLVM vector".to_string());
        };
        let n = vt.get_size();
        // Each lane is paired with its SOURCE EXPRESSION where one exists, so
        // the widening coercion below can tell a `u8` from an `i8`
        // (B-2026-08-13-22). The aggregate arm's extracts have no expression and
        // are already `T`-typed, so they pass `None` and keep the old behaviour.
        let lanes: Vec<(BasicValueEnum<'ctx>, Option<&Expr>)> =
            if let ExprKind::ArrayLiteral(elems) = &args[0].value.kind {
                elems
                    .iter()
                    .map(|e| self.compile_expr(e).map(|v| (v, Some(e))))
                    .collect::<Result<_, _>>()?
            } else {
                let arr = self.compile_expr(&args[0].value)?;
                let agg = arr.into_array_value();
                (0..n)
                    .map(|i| {
                        self.builder
                            .build_extract_value(agg, i, "from_array.lane")
                            .map(|v| (v, None))
                            .map_err(|e| format!("from_array extractvalue failed: {e}"))
                    })
                    .collect::<Result<_, _>>()?
            };
        let i32_ty = self.context.i32_type();
        let mut acc = vt.get_undef();
        for (i, (val, src)) in lanes.iter().enumerate() {
            // Literal-width boundary coercion for the array-literal arm
            // (a bare `0.5` element lowers as f64); no-op for the
            // aggregate arm's already-`T`-typed extracts.
            //
            // B-2026-08-13-22 — this was the signedness-BLIND
            // `coerce_scalar_to_type`, so a `u8` lane widening to an `i64`
            // vector element sign-extended: `Vector[i64, 2].from_array([v, v])`
            // with `v: u8 = 200` reduced to -112 (two lanes of -56) on both
            // compiled backends while the interpreter said 400. Passing the
            // source expression selects zext, exactly as every other widening
            // boundary in the compiler does.
            let val = self.coerce_scalar_to_type_src(*val, vt.get_element_type(), *src);
            acc = self
                .builder
                .build_insert_element(
                    acc,
                    val,
                    i32_ty.const_int(i as u64, false),
                    "from_array.lane",
                )
                .map_err(|e| format!("from_array insertelement failed: {e}"))?;
        }
        Ok(acc.into())
    }

    /// `Vector[T, N].from_slice(s)` — build a `<N x T>` from a `Slice[T]`. The
    /// argument compiles to the 2-word slice header `{ptr data, i64 len}`; the
    /// slice length is a runtime property, so we emit a `len == N` guard that
    /// panics on mismatch (mirrors the slice-index bounds check) before loading
    /// the `N` lanes from `data` and `insertelement`-ing each into the vector.
    pub(super) fn compile_vector_from_slice(
        &mut self,
        generic_args: &[GenericArg],
        args: &[CallArg],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let vec_ty = self
            .llvm_vector_type(&Some(generic_args.to_vec()))
            .ok_or_else(|| "from_slice: could not lower Vector[T, N] type".to_string())?;
        let BasicTypeEnum::VectorType(vt) = vec_ty else {
            return Err("from_slice: lowered type is not an LLVM vector".to_string());
        };
        let n = vt.get_size();
        let elem_ty = vt.get_element_type();

        // Compiled slice is an SSA `{ptr, i64}` struct value — pull the data
        // pointer (field 0) and length (field 1) out directly.
        let slice_val = self.compile_expr(&args[0].value)?.into_struct_value();
        let data = self
            .builder
            .build_extract_value(slice_val, 0, "from_slice.data")
            .map_err(|e| format!("from_slice extract data failed: {e}"))?
            .into_pointer_value();
        let len = self
            .builder
            .build_extract_value(slice_val, 1, "from_slice.len")
            .map_err(|e| format!("from_slice extract len failed: {e}"))?
            .into_int_value();

        // Runtime guard: slice length must equal the static lane count `N`.
        let i64_t = self.context.i64_type();
        let n_const = i64_t.const_int(n as u64, false);
        let fn_val = self.current_fn.unwrap();
        let bad_bb = self.context.append_basic_block(fn_val, "from_slice.badlen");
        let ok_bb = self.context.append_basic_block(fn_val, "from_slice.ok");
        let cmp = self
            .builder
            .build_int_compare(IntPredicate::NE, len, n_const, "from_slice.lencheck")
            .unwrap();
        self.builder
            .build_conditional_branch(cmp, bad_bb, ok_bb)
            .unwrap();
        self.builder.position_at_end(bad_bb);
        self.emit_panic("from_slice: slice length does not match Vector lane count");
        self.builder.build_unreachable().unwrap();

        // Load each lane from `data[i]` and insert into the vector.
        self.builder.position_at_end(ok_bb);
        let i32_ty = self.context.i32_type();
        let mut acc = vt.get_undef();
        for i in 0..n {
            let elem_ptr = unsafe {
                self.builder
                    .build_gep(
                        elem_ty,
                        data,
                        &[i64_t.const_int(i as u64, false)],
                        "from_slice.elem.ptr",
                    )
                    .map_err(|e| format!("from_slice gep failed: {e}"))?
            };
            let val = self
                .builder
                .build_load(elem_ty, elem_ptr, "from_slice.lane")
                .map_err(|e| format!("from_slice load failed: {e}"))?;
            acc = self
                .builder
                .build_insert_element(
                    acc,
                    val,
                    i32_ty.const_int(i as u64, false),
                    "from_slice.lane",
                )
                .map_err(|e| format!("from_slice insertelement failed: {e}"))?;
        }
        Ok(acc.into())
    }

    /// `Vector[T, N].load_masked(slice, mask)` — build a `<N x T>` loading only
    /// the lanes the `mask` selects (design.md § Portable SIMD, "Masked
    /// load/store"). Lane `i` is *active* iff `mask[i]`; an active lane whose
    /// index is past the slice length traps (`emit_panic`, like the `v[i]`
    /// bounds check), an active in-bounds lane loads `slice[i]`, and an inactive
    /// lane reads `0` without touching memory — so a tail mask reads a short
    /// slice without an out-of-bounds access. Per lane: branch on
    /// `mask[i] && i >= len` to the panic block, then on `mask[i]` to a load /
    /// zero pair joined by a phi that feeds the `insertelement`.
    pub(super) fn compile_vector_load_masked(
        &mut self,
        generic_args: &[GenericArg],
        args: &[CallArg],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let vec_ty = self
            .llvm_vector_type(&Some(generic_args.to_vec()))
            .ok_or_else(|| "load_masked: could not lower Vector[T, N] type".to_string())?;
        let BasicTypeEnum::VectorType(vt) = vec_ty else {
            return Err("load_masked: lowered type is not an LLVM vector".to_string());
        };
        let n = vt.get_size();
        let elem_ty = vt.get_element_type();
        let i64_t = self.context.i64_type();
        let i32_ty = self.context.i32_type();

        // Slice header `{ptr data, i64 len}` (field 0 / field 1).
        let slice_val = self.compile_expr(&args[0].value)?.into_struct_value();
        let data = self
            .builder
            .build_extract_value(slice_val, 0, "load_masked.data")
            .map_err(|e| format!("load_masked extract data failed: {e}"))?
            .into_pointer_value();
        let len = self
            .builder
            .build_extract_value(slice_val, 1, "load_masked.len")
            .map_err(|e| format!("load_masked extract len failed: {e}"))?
            .into_int_value();
        // Mask `<N x i1>`.
        let mask = self.compile_expr(&args[1].value)?.into_vector_value();

        let fn_val = self.current_fn.unwrap();
        let zero: BasicValueEnum<'ctx> = match elem_ty {
            BasicTypeEnum::IntType(t) => t.const_zero().into(),
            BasicTypeEnum::FloatType(t) => t.const_zero().into(),
            other => return Err(format!("load_masked: unsupported element type {other:?}")),
        };
        let mut acc = vt.get_undef();
        for i in 0..n {
            let lane_idx = i32_ty.const_int(i as u64, false);
            let mask_i = self
                .builder
                .build_extract_element(mask, lane_idx, "load_masked.mask")
                .map_err(|e| format!("load_masked extractelement mask failed: {e}"))?
                .into_int_value();
            let i_const = i64_t.const_int(i as u64, false);
            let oob = self
                .builder
                .build_int_compare(IntPredicate::UGE, i_const, len, "load_masked.oob")
                .map_err(|e| format!("load_masked bounds compare failed: {e}"))?;
            let bad = self
                .builder
                .build_and(mask_i, oob, "load_masked.bad")
                .map_err(|e| format!("load_masked and failed: {e}"))?;
            let panic_bb = self.context.append_basic_block(fn_val, "load_masked.panic");
            let ok_bb = self.context.append_basic_block(fn_val, "load_masked.ok");
            self.builder
                .build_conditional_branch(bad, panic_bb, ok_bb)
                .map_err(|e| format!("load_masked panic branch failed: {e}"))?;
            self.builder.position_at_end(panic_bb);
            self.emit_panic("load_masked: active lane index out of bounds");
            self.builder
                .build_unreachable()
                .map_err(|e| format!("load_masked unreachable failed: {e}"))?;

            self.builder.position_at_end(ok_bb);
            let load_bb = self.context.append_basic_block(fn_val, "load_masked.load");
            let zero_bb = self.context.append_basic_block(fn_val, "load_masked.zero");
            let merge_bb = self.context.append_basic_block(fn_val, "load_masked.merge");
            self.builder
                .build_conditional_branch(mask_i, load_bb, zero_bb)
                .map_err(|e| format!("load_masked active branch failed: {e}"))?;
            // Active lane → load `data[i]`.
            self.builder.position_at_end(load_bb);
            let elem_ptr = unsafe {
                self.builder
                    .build_gep(elem_ty, data, &[i_const], "load_masked.elem.ptr")
                    .map_err(|e| format!("load_masked gep failed: {e}"))?
            };
            let loaded = self
                .builder
                .build_load(elem_ty, elem_ptr, "load_masked.lane")
                .map_err(|e| format!("load_masked load failed: {e}"))?;
            self.builder
                .build_unconditional_branch(merge_bb)
                .map_err(|e| format!("load_masked load->merge failed: {e}"))?;
            let load_end = self.builder.get_insert_block().unwrap();
            // Inactive lane → zero.
            self.builder.position_at_end(zero_bb);
            self.builder
                .build_unconditional_branch(merge_bb)
                .map_err(|e| format!("load_masked zero->merge failed: {e}"))?;
            // Join the loaded / zero value and insert it.
            self.builder.position_at_end(merge_bb);
            let phi = self
                .builder
                .build_phi(elem_ty, "load_masked.val")
                .map_err(|e| format!("load_masked phi failed: {e}"))?;
            phi.add_incoming(&[(&loaded, load_end), (&zero, zero_bb)]);
            acc = self
                .builder
                .build_insert_element(acc, phi.as_basic_value(), lane_idx, "load_masked.ins")
                .map_err(|e| format!("load_masked insertelement failed: {e}"))?;
        }
        Ok(acc.into())
    }

    /// `Vector[T, N].gather(slice, indices)` — build a `<N x T>` reading
    /// `slice[indices[i]]` for each lane (design.md § Portable SIMD, "Gather /
    /// scatter"). Every lane is active; each index is widened to i64 and
    /// bounds-checked (`UGE idx, len`, so a negative signed index also trips it,
    /// exactly like the `v[i]` read) before loading `data[idx]`.
    pub(super) fn compile_vector_gather(
        &mut self,
        generic_args: &[GenericArg],
        args: &[CallArg],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let vec_ty = self
            .llvm_vector_type(&Some(generic_args.to_vec()))
            .ok_or_else(|| "gather: could not lower Vector[T, N] type".to_string())?;
        let BasicTypeEnum::VectorType(vt) = vec_ty else {
            return Err("gather: lowered type is not an LLVM vector".to_string());
        };
        let n = vt.get_size();
        let elem_ty = vt.get_element_type();
        let i64_t = self.context.i64_type();
        let i32_ty = self.context.i32_type();

        let slice_val = self.compile_expr(&args[0].value)?.into_struct_value();
        let data = self
            .builder
            .build_extract_value(slice_val, 0, "gather.data")
            .map_err(|e| format!("gather extract data failed: {e}"))?
            .into_pointer_value();
        let len = self
            .builder
            .build_extract_value(slice_val, 1, "gather.len")
            .map_err(|e| format!("gather extract len failed: {e}"))?
            .into_int_value();
        let indices = self.compile_expr(&args[1].value)?.into_vector_value();

        let fn_val = self.current_fn.unwrap();
        let mut acc = vt.get_undef();
        for i in 0..n {
            let lane_idx = i32_ty.const_int(i as u64, false);
            let raw = self
                .builder
                .build_extract_element(indices, lane_idx, "gather.idx")
                .map_err(|e| format!("gather extractelement index failed: {e}"))?
                .into_int_value();
            // Widen the index lane to i64 for the gep / bounds check.
            let idx = match raw.get_type().get_bit_width().cmp(&64) {
                std::cmp::Ordering::Less => self
                    .builder
                    .build_int_s_extend(raw, i64_t, "gather.idx.sx")
                    .map_err(|e| format!("gather index sext failed: {e}"))?,
                std::cmp::Ordering::Greater => self
                    .builder
                    .build_int_truncate(raw, i64_t, "gather.idx.tr")
                    .map_err(|e| format!("gather index truncate failed: {e}"))?,
                std::cmp::Ordering::Equal => raw,
            };
            let oob = self
                .builder
                .build_int_compare(IntPredicate::UGE, idx, len, "gather.oob")
                .map_err(|e| format!("gather bounds compare failed: {e}"))?;
            let panic_bb = self.context.append_basic_block(fn_val, "gather.panic");
            let ok_bb = self.context.append_basic_block(fn_val, "gather.ok");
            self.builder
                .build_conditional_branch(oob, panic_bb, ok_bb)
                .map_err(|e| format!("gather panic branch failed: {e}"))?;
            self.builder.position_at_end(panic_bb);
            self.emit_panic("gather: index out of bounds");
            self.builder
                .build_unreachable()
                .map_err(|e| format!("gather unreachable failed: {e}"))?;

            self.builder.position_at_end(ok_bb);
            let elem_ptr = unsafe {
                self.builder
                    .build_gep(elem_ty, data, &[idx], "gather.elem.ptr")
                    .map_err(|e| format!("gather gep failed: {e}"))?
            };
            let loaded = self
                .builder
                .build_load(elem_ty, elem_ptr, "gather.lane")
                .map_err(|e| format!("gather load failed: {e}"))?;
            acc = self
                .builder
                .build_insert_element(acc, loaded, lane_idx, "gather.ins")
                .map_err(|e| format!("gather insertelement failed: {e}"))?;
        }
        Ok(acc.into())
    }

    /// `Vector[U, N].cast_from(v)` — per-lane numeric conversion of a source
    /// `Vector[S, N]` to the target element `U` (design.md § Portable SIMD,
    /// "Conversion"). Each source lane is extracted and run through the scalar
    /// `compile_cast` (int↔float via sitofp/uitofp/fptosi, int width via
    /// trunc/sext/zext, float width via fpcast — the same lowering scalar `as`
    /// uses), then inserted into the `<N x U>` result. The source element's
    /// signedness rides the `unsigned_vector_exprs` span side-table (so a
    /// `u*`-lane source picks `uitofp` / zext over the signed forms).
    pub(super) fn compile_vector_cast_from(
        &mut self,
        generic_args: &[GenericArg],
        args: &[CallArg],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let vec_ty = self
            .llvm_vector_type(&Some(generic_args.to_vec()))
            .ok_or_else(|| "cast_from: could not lower Vector[U, N] type".to_string())?;
        let BasicTypeEnum::VectorType(vt) = vec_ty else {
            return Err("cast_from: lowered type is not an LLVM vector".to_string());
        };
        let n = vt.get_size();
        let target_elem = vt.get_element_type();
        let i32_ty = self.context.i32_type();

        let src_span = &args[0].value.span;
        let src_unsigned = self
            .span_tables
            .unsigned_vector_exprs
            .contains(&(src_span.offset, src_span.length));
        // Target element signedness (for the float→int saturating lane) — read
        // from the destination `Vector[U, N]`'s element type name.
        let target_unsigned = generic_args.first().is_some_and(|ga| {
            matches!(ga, GenericArg::Type(t)
                if matches!(&t.kind, TypeKind::Path(p)
                    if matches!(
                        p.segments.first().map(|s| s.as_str()),
                        Some("u8") | Some("u16") | Some("u32") | Some("u64") | Some("u128") | Some("usize")
                    )))
        });
        let src = self.compile_expr(&args[0].value)?.into_vector_value();

        let mut acc = vt.get_undef();
        for i in 0..n {
            let lane_idx = i32_ty.const_int(i as u64, false);
            let lane = self
                .builder
                .build_extract_element(src, lane_idx, "cast_from.lane")
                .map_err(|e| format!("cast_from extractelement failed: {e}"))?;
            let converted = self.compile_cast(lane, target_elem, src_unsigned, target_unsigned)?;
            acc = self
                .builder
                .build_insert_element(acc, converted, lane_idx, "cast_from.ins")
                .map_err(|e| format!("cast_from insertelement failed: {e}"))?;
        }
        Ok(acc.into())
    }

    /// Lower a `Vector[T, N]` instance method to a scalar (design.md
    /// § Portable SIMD, slices 2 / 2b). `reduce_{sum,product,and,or,xor}` fold
    /// all lanes with the matching scalar op; `dot` folds the element-wise
    /// product of the two vectors with `+`. Lanes are read via `extractelement`
    /// and combined with the scalar `compile_binop` (which selects int vs float
    /// automatically); LLVM re-vectorizes the fold where profitable. The
    /// Vectorized `exp(x)` — the core of `std.simd.math`'s guaranteed-SIMD
    /// transcendentals (phase-11). For an **f32** vector this is the Cephes
    /// `expf` algorithm: Cody-Waite range reduction `x = n·ln2 + r` (with the
    /// `ln2` split into a high + low part so the subtraction keeps full
    /// precision), a degree-6 minimax polynomial for `exp(r)` on
    /// `r ∈ [-ln2/2, ln2/2]`, then scale by `2^n` assembled directly into the
    /// IEEE-754 exponent field (`bitcast((n + 127) << 23)`). Genuinely
    /// vectorized on every target (no dependence on a target vector-math lib —
    /// the reason plain `@llvm.exp` scalarizes where libmvec is absent),
    /// accurate to ~1 ULP. `x` is clamped to `[-88.376, 88.376]` first so the
    /// exponent assembly can't overflow (larger magnitudes saturate to
    /// `~f32::MAX` / `0`, the Cephes posture). For an **f64** vector it falls
    /// back to the overloaded `llvm.exp` intrinsic (the f64 polynomial is a
    /// follow-up). Used by `v.exp()` and, transitively, `v.sigmoid()` /
    /// `v.tanh()` (both derived from `exp`).
    /// Apply one `std.simd.math` float-unary op to an already-computed
    /// float vector — the shared core of the `Vector[f32/f64,N]` method
    /// surface (`compile_vector_method`) and the tensor-map vectorizer
    /// (`try_emit_vectorized_map`), so a `t.map(|x| x.exp())` inner loop
    /// routes through the identical polynomial. `exp`/`ln` use the shipped
    /// guaranteed-SIMD polynomials; `sigmoid`/`tanh` derive from `exp`;
    /// `sqrt` + the four rounding ops lower to the overloaded LLVM
    /// float-vector intrinsic. `None` for a non-matching method name.
    pub(super) fn apply_vector_float_unary(
        &self,
        method: &str,
        recv: inkwell::values::VectorValue<'ctx>,
    ) -> Option<inkwell::values::VectorValue<'ctx>> {
        let vt = recv.get_type();
        let ft = vt.get_element_type().into_float_type();
        let n = vt.get_size();
        let i32_t = self.context.i32_type();
        let splat = |c: f64| -> inkwell::values::VectorValue<'ctx> {
            let scalar = ft.const_float(c);
            let mut sv = vt.get_undef();
            for i in 0..n {
                sv = self
                    .builder
                    .build_insert_element(sv, scalar, i32_t.const_int(i as u64, false), "splat")
                    .unwrap();
            }
            sv
        };
        let apply = |name: &str, v: inkwell::values::VectorValue<'ctx>| {
            let intr = inkwell::intrinsics::Intrinsic::find(name)
                .expect("float-vector intrinsic must exist");
            let decl = intr
                .get_declaration(&self.module, &[v.get_type().into()])
                .expect("intrinsic declaration for vector float type");
            self.builder
                .build_call(decl, &[v.into()], "simdmath")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_vector_value()
        };
        Some(match method {
            "sqrt" => apply("llvm.sqrt", recv),
            "exp" => self.compile_vector_exp(recv),
            "ln" => self.compile_vector_ln(recv),
            "floor" => apply("llvm.floor", recv),
            "ceil" => apply("llvm.ceil", recv),
            "round" => apply("llvm.round", recv),
            "trunc" => apply("llvm.trunc", recv),
            "sigmoid" => {
                let neg = self.builder.build_float_neg(recv, "sig.neg").unwrap();
                let e = self.compile_vector_exp(neg);
                let one = splat(1.0);
                let denom = self.builder.build_float_add(one, e, "sig.denom").unwrap();
                self.builder.build_float_div(one, denom, "sigmoid").unwrap()
            }
            "tanh" => {
                let two = splat(2.0);
                let x2 = self.builder.build_float_mul(recv, two, "tanh.x2").unwrap();
                let e = self.compile_vector_exp(x2);
                let one = splat(1.0);
                let num = self.builder.build_float_sub(e, one, "tanh.num").unwrap();
                let den = self.builder.build_float_add(e, one, "tanh.den").unwrap();
                self.builder.build_float_div(num, den, "tanh").unwrap()
            }
            _ => return None,
        })
    }

    pub(super) fn compile_vector_exp(
        &self,
        recv: inkwell::values::VectorValue<'ctx>,
    ) -> inkwell::values::VectorValue<'ctx> {
        let vt = recv.get_type();
        let ft = vt.get_element_type().into_float_type();
        let n = vt.get_size();
        let i32_t = self.context.i32_type();
        let iv_t = i32_t.vec_type(n);

        let fsplat = |c: f64| -> inkwell::values::VectorValue<'ctx> {
            let scalar = ft.const_float(c);
            let mut sv = vt.get_undef();
            for i in 0..n {
                sv = self
                    .builder
                    .build_insert_element(sv, scalar, i32_t.const_int(i as u64, false), "e.fsplat")
                    .unwrap();
            }
            sv
        };
        let isplat = |c: u64| -> inkwell::values::VectorValue<'ctx> {
            let scalar = i32_t.const_int(c, false);
            let mut sv = iv_t.get_undef();
            for i in 0..n {
                sv = self
                    .builder
                    .build_insert_element(sv, scalar, i32_t.const_int(i as u64, false), "e.isplat")
                    .unwrap();
            }
            sv
        };
        let unary_intr = |name: &str,
                          v: inkwell::values::VectorValue<'ctx>|
         -> inkwell::values::VectorValue<'ctx> {
            let intr = inkwell::intrinsics::Intrinsic::find(name).expect("intrinsic must exist");
            let decl = intr
                .get_declaration(&self.module, &[v.get_type().into()])
                .expect("intrinsic declaration");
            self.builder
                .build_call(decl, &[v.into()], "e.intr")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_vector_value()
        };
        let binary_intr = |name: &str,
                           a: inkwell::values::VectorValue<'ctx>,
                           b: inkwell::values::VectorValue<'ctx>|
         -> inkwell::values::VectorValue<'ctx> {
            let intr = inkwell::intrinsics::Intrinsic::find(name).expect("intrinsic must exist");
            let decl = intr
                .get_declaration(&self.module, &[a.get_type().into()])
                .expect("intrinsic declaration");
            self.builder
                .build_call(decl, &[a.into(), b.into()], "e.intr2")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_vector_value()
        };

        // f64: the Cephes double-precision `exp` — a rational P(r)/Q(r) instead
        // of the f32 single polynomial (higher precision), same range reduction
        // and `2^k` exponent assembly (into the 11-bit f64 exponent field).
        if ft == self.context.f64_type() {
            let i64_t = self.context.i64_type();
            let iv64 = i64_t.vec_type(n);
            let isplat64 = |c: u64| -> inkwell::values::VectorValue<'ctx> {
                let scalar = i64_t.const_int(c, false);
                let mut sv = iv64.get_undef();
                for i in 0..n {
                    sv = self
                        .builder
                        .build_insert_element(
                            sv,
                            scalar,
                            i32_t.const_int(i as u64, false),
                            "e64.isplat",
                        )
                        .unwrap();
                }
                sv
            };
            // Clamp to [MINLOG, MAXLOG] so the 2^k assembly can't overflow.
            let x = binary_intr("llvm.maxnum", recv, fsplat(-708.396_418_532_264_1));
            let x = binary_intr("llvm.minnum", x, fsplat(709.782_712_893_384));
            // n = floor(log2(e)·x + 0.5)
            let zin = self
                .builder
                .build_float_mul(x, fsplat(std::f64::consts::LOG2_E), "e64.z0")
                .unwrap();
            let zin = self
                .builder
                .build_float_add(zin, fsplat(0.5), "e64.z1")
                .unwrap();
            let z = unary_intr("llvm.floor", zin);
            // r = x - z*C1 - z*C2  (ln2 split hi+lo).
            let zc1 = self
                .builder
                .build_float_mul(z, fsplat(6.931_457_519_531_25E-1), "e64.zc1")
                .unwrap();
            let x = self.builder.build_float_sub(x, zc1, "e64.xr1").unwrap();
            let zc2 = self
                .builder
                .build_float_mul(z, fsplat(1.428_606_820_309_417_2E-6), "e64.zc2")
                .unwrap();
            let x = self.builder.build_float_sub(x, zc2, "e64.xr2").unwrap();
            let xx = self.builder.build_float_mul(x, x, "e64.xx").unwrap();
            // px = x · P(xx),  P degree 2.
            let pco = [
                1.261_771_930_748_105_9E-4_f64,
                3.029_944_077_074_419_6E-2,
                1.0,
            ];
            let mut p = fsplat(pco[0]);
            for &c in &pco[1..] {
                p = self.builder.build_float_mul(p, xx, "e64.pm").unwrap();
                p = self
                    .builder
                    .build_float_add(p, fsplat(c), "e64.pa")
                    .unwrap();
            }
            let px = self.builder.build_float_mul(x, p, "e64.px").unwrap();
            // Q(xx),  degree 3.
            let qco = [
                3.001_985_051_386_644_6E-6_f64,
                2.524_483_403_496_841E-3,
                2.272_655_482_081_550_3E-1,
                2.0,
            ];
            let mut q = fsplat(qco[0]);
            for &c in &qco[1..] {
                q = self.builder.build_float_mul(q, xx, "e64.qm").unwrap();
                q = self
                    .builder
                    .build_float_add(q, fsplat(c), "e64.qa")
                    .unwrap();
            }
            // r = px / (Q - px);  result mantissa = 1 + 2r.
            let denom = self.builder.build_float_sub(q, px, "e64.den").unwrap();
            let xr = self.builder.build_float_div(px, denom, "e64.div").unwrap();
            let two_xr = self
                .builder
                .build_float_mul(fsplat(2.0), xr, "e64.2xr")
                .unwrap();
            let res = self
                .builder
                .build_float_add(fsplat(1.0), two_xr, "e64.res")
                .unwrap();
            // 2^k = bitcast((int64(z) + 1023) << 52).
            let ki = self
                .builder
                .build_float_to_signed_int(z, iv64, "e64.ki")
                .unwrap();
            let ki = self
                .builder
                .build_int_add(ki, isplat64(1023), "e64.kb")
                .unwrap();
            let ei = self
                .builder
                .build_left_shift(ki, isplat64(52), "e64.shl")
                .unwrap();
            let pow2k = self
                .builder
                .build_bit_cast(ei, vt, "e64.pow")
                .unwrap()
                .into_vector_value();
            return self.builder.build_float_mul(res, pow2k, "e64.out").unwrap();
        }

        // Any other float width (f16 / bf16): fall back to the intrinsic.
        if ft != self.context.f32_type() {
            return unary_intr("llvm.exp", recv);
        }

        // Clamp so the `2^n` exponent assembly stays in range.
        let x = binary_intr("llvm.maxnum", recv, fsplat(-88.376_262_664_795_0));
        let x = binary_intr("llvm.minnum", x, fsplat(88.376_262_664_795_0));
        // z = floor(x * log2(e) + 0.5)  — the nearest integer exponent.
        let zin = self
            .builder
            .build_float_mul(x, fsplat(std::f64::consts::LOG2_E), "e.z0")
            .unwrap();
        let zin = self
            .builder
            .build_float_add(zin, fsplat(0.5), "e.z1")
            .unwrap();
        let z = unary_intr("llvm.floor", zin);
        // r = x - z*C1 - z*C2   (ln2 = C1 + C2, split for precision).
        let zc1 = self
            .builder
            .build_float_mul(z, fsplat(0.693_359_375), "e.zc1")
            .unwrap();
        let x = self.builder.build_float_sub(x, zc1, "e.xr1").unwrap();
        let zc2 = self
            .builder
            .build_float_mul(z, fsplat(-2.121_944_40e-4), "e.zc2")
            .unwrap();
        let x = self.builder.build_float_sub(x, zc2, "e.xr2").unwrap();
        // Horner evaluation of the degree-6 minimax polynomial for exp(r).
        let coeffs = [
            1.987_569_150_0e-4_f64,
            1.398_199_950_7e-3,
            8.333_451_907_3e-3,
            4.166_579_589_4e-2,
            1.666_666_545_9e-1,
            5.000_000_120_1e-1,
        ];
        let mut p = fsplat(coeffs[0]);
        for &c in &coeffs[1..] {
            p = self.builder.build_float_mul(p, x, "e.pm").unwrap();
            p = self.builder.build_float_add(p, fsplat(c), "e.pa").unwrap();
        }
        // p = p*r² + r + 1
        let x2 = self.builder.build_float_mul(x, x, "e.x2").unwrap();
        let p = self.builder.build_float_mul(p, x2, "e.px2").unwrap();
        let p = self.builder.build_float_add(p, x, "e.ppx").unwrap();
        let p = self
            .builder
            .build_float_add(p, fsplat(1.0), "e.pp1")
            .unwrap();
        // 2^n = bitcast((int(z) + 127) << 23) — assemble the exponent field.
        let ni = self
            .builder
            .build_float_to_signed_int(z, iv_t, "e.ni")
            .unwrap();
        let ni = self.builder.build_int_add(ni, isplat(127), "e.nb").unwrap();
        let ei = self
            .builder
            .build_left_shift(ni, isplat(23), "e.shl")
            .unwrap();
        let pow2n = self
            .builder
            .build_bit_cast(ei, vt, "e.pow")
            .unwrap()
            .into_vector_value();
        self.builder.build_float_mul(p, pow2n, "e.res").unwrap()
    }

    /// Vectorized `ln(x)` — the `log` sibling of [`compile_vector_exp`]
    /// (`std.simd.math`, phase-11). For an **f32** vector this is the Cephes
    /// `logf` algorithm: a branchless `frexp` (split `x = m·2^e`, `m ∈ [0.5,1)`)
    /// done directly on the IEEE bit pattern — `e` from the exponent field,
    /// `m` by forcing that field to the `0.5` biased value — then a
    /// `√½`-pivot normalization (`m < √½ → e−1, x = 2m−1`, else `x = m−1`), a
    /// degree-9 minimax polynomial for `ln(1+x)`, and the `e·ln2`
    /// reconstruction (with `ln2` split hi+lo for precision). Domain: `x < 0 →
    /// NaN`, `x == 0 → −∞` (per-lane `select`, matching `@llvm.log`).
    /// Genuinely vectorized, ~1 ULP; the f64 case falls back to `llvm.log`
    /// (the f64 polynomial is a follow-up). Used by `v.ln()`.
    pub(super) fn compile_vector_ln(
        &self,
        recv: inkwell::values::VectorValue<'ctx>,
    ) -> inkwell::values::VectorValue<'ctx> {
        let vt = recv.get_type();
        let ft = vt.get_element_type().into_float_type();
        let n = vt.get_size();
        let i32_t = self.context.i32_type();
        let iv_t = i32_t.vec_type(n);

        let fsplat = |c: f64| -> inkwell::values::VectorValue<'ctx> {
            let scalar = ft.const_float(c);
            let mut sv = vt.get_undef();
            for i in 0..n {
                sv = self
                    .builder
                    .build_insert_element(sv, scalar, i32_t.const_int(i as u64, false), "l.fsplat")
                    .unwrap();
            }
            sv
        };
        let isplat = |c: u64| -> inkwell::values::VectorValue<'ctx> {
            let scalar = i32_t.const_int(c, false);
            let mut sv = iv_t.get_undef();
            for i in 0..n {
                sv = self
                    .builder
                    .build_insert_element(sv, scalar, i32_t.const_int(i as u64, false), "l.isplat")
                    .unwrap();
            }
            sv
        };

        // f64: the Cephes double-precision `log` — same branchless frexp +
        // √½ pivot as f32, but a rational `x³·P(x)/Q(x)` (degree-5 each) instead
        // of the f32 single polynomial. Exponent field is 11 bits (bias 1023).
        if ft == self.context.f64_type() {
            let i64_t = self.context.i64_type();
            let iv64 = i64_t.vec_type(n);
            let isplat64 = |c: u64| -> inkwell::values::VectorValue<'ctx> {
                let scalar = i64_t.const_int(c, false);
                let mut sv = iv64.get_undef();
                for i in 0..n {
                    sv = self
                        .builder
                        .build_insert_element(
                            sv,
                            scalar,
                            i32_t.const_int(i as u64, false),
                            "l64.isplat",
                        )
                        .unwrap();
                }
                sv
            };
            // frexp: e = ((bits >> 52) & 0x7FF) - 1022; m ∈ [0.5, 1).
            let bits = self
                .builder
                .build_bit_cast(recv, iv64, "l64.bits")
                .unwrap()
                .into_vector_value();
            let ef = self
                .builder
                .build_right_shift(bits, isplat64(52), false, "l64.ef")
                .unwrap();
            let ef = self
                .builder
                .build_and(ef, isplat64(0x7FF), "l64.ef2")
                .unwrap();
            let e_int = self
                .builder
                .build_int_sub(ef, isplat64(1022), "l64.eint")
                .unwrap();
            let mant = self
                .builder
                .build_and(bits, isplat64(0x800F_FFFF_FFFF_FFFF), "l64.mant")
                .unwrap();
            let mant = self
                .builder
                .build_or(mant, isplat64(0x3FE0_0000_0000_0000), "l64.mant2")
                .unwrap();
            let m = self
                .builder
                .build_bit_cast(mant, vt, "l64.m")
                .unwrap()
                .into_vector_value();
            // √½ pivot.
            let cond = self
                .builder
                .build_float_compare(
                    inkwell::FloatPredicate::OLT,
                    m,
                    fsplat(std::f64::consts::FRAC_1_SQRT_2),
                    "l64.piv",
                )
                .unwrap();
            let m2 = self.builder.build_float_add(m, m, "l64.m2").unwrap();
            let x_lo = self
                .builder
                .build_float_sub(m2, fsplat(1.0), "l64.xlo")
                .unwrap();
            let x_hi = self
                .builder
                .build_float_sub(m, fsplat(1.0), "l64.xhi")
                .unwrap();
            let x = self
                .builder
                .build_select(cond, x_lo, x_hi, "l64.x")
                .unwrap()
                .into_vector_value();
            let e_dec = self
                .builder
                .build_int_sub(e_int, isplat64(1), "l64.edec")
                .unwrap();
            let e_sel = self
                .builder
                .build_select(cond, e_dec, e_int, "l64.esel")
                .unwrap()
                .into_vector_value();
            let fe = self
                .builder
                .build_signed_int_to_float(e_sel, vt, "l64.fe")
                .unwrap();
            let z = self.builder.build_float_mul(x, x, "l64.z").unwrap();
            // P(x), degree 5 (6 coeffs).
            let pco = [
                1.018_756_638_045_809_3E-4_f64,
                4.974_949_949_767_47E-1,
                4.705_791_198_788_817,
                1.449_892_253_416_109_3E1,
                1.793_686_785_078_198_2E1,
                7.708_387_337_558_854E0,
            ];
            let mut pnum = fsplat(pco[0]);
            for &c in &pco[1..] {
                pnum = self.builder.build_float_mul(pnum, x, "l64.pm").unwrap();
                pnum = self
                    .builder
                    .build_float_add(pnum, fsplat(c), "l64.pa")
                    .unwrap();
            }
            // Q(x), monic degree 5 (implicit leading 1 + 5 coeffs).
            let qco = [
                1.128_735_871_891_674_5E1_f64,
                4.522_791_458_375_322E1,
                8.298_752_669_127_766E1,
                7.115_447_506_185_639E1,
                2.312_516_201_267_653_4E1,
            ];
            let mut qden = fsplat(1.0);
            for &c in &qco {
                qden = self.builder.build_float_mul(qden, x, "l64.qm").unwrap();
                qden = self
                    .builder
                    .build_float_add(qden, fsplat(c), "l64.qa")
                    .unwrap();
            }
            // y = x · (z · P/Q)
            let ratio = self
                .builder
                .build_float_div(pnum, qden, "l64.ratio")
                .unwrap();
            let mut y = self.builder.build_float_mul(z, ratio, "l64.zr").unwrap();
            y = self.builder.build_float_mul(x, y, "l64.xy").unwrap();
            // y -= fe · ln2_lo ; y -= 0.5·z
            let ylo = self
                .builder
                .build_float_mul(fe, fsplat(2.121_944_400_546_905_8E-4), "l64.ylo")
                .unwrap();
            y = self.builder.build_float_sub(y, ylo, "l64.ysub1").unwrap();
            let hz = self
                .builder
                .build_float_mul(fsplat(0.5), z, "l64.hz")
                .unwrap();
            y = self.builder.build_float_sub(y, hz, "l64.ysub2").unwrap();
            // result = x + y + fe · ln2_hi
            let zr = self.builder.build_float_add(x, y, "l64.zr2").unwrap();
            let hi = self
                .builder
                .build_float_mul(fe, fsplat(0.693_359_375), "l64.hi")
                .unwrap();
            let zr = self.builder.build_float_add(zr, hi, "l64.zr3").unwrap();
            // Domain: x < 0 → NaN, x == 0 → -inf.
            let zero = fsplat(0.0);
            let is_zero = self
                .builder
                .build_float_compare(inkwell::FloatPredicate::OEQ, recv, zero, "l64.isz")
                .unwrap();
            let is_neg = self
                .builder
                .build_float_compare(inkwell::FloatPredicate::OLT, recv, zero, "l64.isn")
                .unwrap();
            let r = self
                .builder
                .build_select(is_zero, fsplat(f64::NEG_INFINITY), zr, "l64.rz")
                .unwrap()
                .into_vector_value();
            return self
                .builder
                .build_select(is_neg, fsplat(f64::NAN), r, "l64.rn")
                .unwrap()
                .into_vector_value();
        }

        // Any other float width (f16 / bf16): fall back to the intrinsic.
        if ft != self.context.f32_type() {
            let intr =
                inkwell::intrinsics::Intrinsic::find("llvm.log").expect("llvm.log must exist");
            let decl = intr
                .get_declaration(&self.module, &[vt.into()])
                .expect("llvm.log declaration");
            return self
                .builder
                .build_call(decl, &[recv.into()], "l.intr")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_vector_value();
        }

        // Branchless frexp on the bit pattern: bits = reinterpret(x).
        let bits = self
            .builder
            .build_bit_cast(recv, iv_t, "l.bits")
            .unwrap()
            .into_vector_value();
        // e = ((bits >> 23) & 0xFF) - 126  (unbiased exponent, frexp convention).
        let ef = self
            .builder
            .build_right_shift(bits, isplat(23), false, "l.ef")
            .unwrap();
        let ef = self.builder.build_and(ef, isplat(0xFF), "l.ef2").unwrap();
        let e_int = self
            .builder
            .build_int_sub(ef, isplat(126), "l.eint")
            .unwrap();
        // m = reinterpret((bits & 0x807FFFFF) | 0x3F000000) ∈ [0.5, 1).
        let mant = self
            .builder
            .build_and(bits, isplat(0x807F_FFFF), "l.mant")
            .unwrap();
        let mant = self
            .builder
            .build_or(mant, isplat(0x3F00_0000), "l.mant2")
            .unwrap();
        let m = self
            .builder
            .build_bit_cast(mant, vt, "l.m")
            .unwrap()
            .into_vector_value();
        // √½ pivot: if m < √½ then (e-=1; x = 2m-1) else x = m-1.
        let cond = self
            .builder
            .build_float_compare(
                inkwell::FloatPredicate::OLT,
                m,
                fsplat(std::f64::consts::FRAC_1_SQRT_2),
                "l.piv",
            )
            .unwrap();
        let m2 = self.builder.build_float_add(m, m, "l.m2").unwrap();
        let x_lo = self
            .builder
            .build_float_sub(m2, fsplat(1.0), "l.xlo")
            .unwrap();
        let x_hi = self
            .builder
            .build_float_sub(m, fsplat(1.0), "l.xhi")
            .unwrap();
        let x = self
            .builder
            .build_select(cond, x_lo, x_hi, "l.x")
            .unwrap()
            .into_vector_value();
        let e_dec = self
            .builder
            .build_int_sub(e_int, isplat(1), "l.edec")
            .unwrap();
        let e_sel = self
            .builder
            .build_select(cond, e_dec, e_int, "l.esel")
            .unwrap()
            .into_vector_value();
        let fe = self
            .builder
            .build_signed_int_to_float(e_sel, vt, "l.fe")
            .unwrap();
        // Degree-9 Horner minimax polynomial for ln(1+x), then × x × x².
        let z = self.builder.build_float_mul(x, x, "l.z").unwrap();
        let coeffs = [
            7.037_683_629_2e-2_f64,
            -1.151_461_031_0e-1,
            1.167_699_874_0e-1,
            -1.242_014_084_6e-1,
            1.424_932_278_7e-1,
            -1.666_805_766_5e-1,
            2.000_071_476_5e-1,
            -2.499_999_399_3e-1,
            3.333_333_117_4e-1,
        ];
        let mut y = fsplat(coeffs[0]);
        for &c in &coeffs[1..] {
            y = self.builder.build_float_mul(y, x, "l.pm").unwrap();
            y = self.builder.build_float_add(y, fsplat(c), "l.pa").unwrap();
        }
        y = self.builder.build_float_mul(y, x, "l.yx").unwrap();
        y = self.builder.build_float_mul(y, z, "l.yz").unwrap();
        // y += ln2_lo·e ; y -= 0.5·z
        let ylo = self
            .builder
            .build_float_mul(fsplat(-2.121_944_40e-4), fe, "l.ylo")
            .unwrap();
        y = self.builder.build_float_add(y, ylo, "l.yadd").unwrap();
        let hz = self
            .builder
            .build_float_mul(fsplat(0.5), z, "l.hz")
            .unwrap();
        y = self.builder.build_float_sub(y, hz, "l.ysub").unwrap();
        // result = x + y + ln2_hi·e
        let zr = self.builder.build_float_add(x, y, "l.zr").unwrap();
        let hi = self
            .builder
            .build_float_mul(fsplat(0.693_359_375), fe, "l.hi")
            .unwrap();
        let zr = self.builder.build_float_add(zr, hi, "l.zr2").unwrap();
        // Domain: x < 0 → NaN, x == 0 → -inf (matches @llvm.log).
        let zero = fsplat(0.0);
        let is_zero = self
            .builder
            .build_float_compare(inkwell::FloatPredicate::OEQ, recv, zero, "l.isz")
            .unwrap();
        let is_neg = self
            .builder
            .build_float_compare(inkwell::FloatPredicate::OLT, recv, zero, "l.isn")
            .unwrap();
        let r = self
            .builder
            .build_select(is_zero, fsplat(f64::NEG_INFINITY), zr, "l.rz")
            .unwrap()
            .into_vector_value();
        self.builder
            .build_select(is_neg, fsplat(f64::NAN), r, "l.rn")
            .unwrap()
            .into_vector_value()
    }

    /// typechecker guarantees `N >= 1`, an integer element for the bitwise
    /// folds, and a same-typed vector argument for `dot`.
    pub(super) fn compile_vector_method(
        &mut self,
        object: &Expr,
        method: &str,
        args: &[CallArg],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let recv = self.compile_expr(object)?.into_vector_value();
        let n = recv.get_type().get_size();
        let i32_t = self.context.i32_type();
        let lane = |cg: &Self, v: inkwell::values::VectorValue<'ctx>, i: u32| {
            cg.builder
                .build_extract_element(v, i32_t.const_int(i as u64, false), "lane")
                .map_err(|e| format!("vector extractelement failed: {e}"))
        };
        match method {
            // `std.simd.math` transcendentals + rounding (phase-11 numerical
            // stdlib) on a float-lane vector. `sqrt` / `exp` / `ln` and the four
            // rounding ops `floor` / `ceil` / `round` / `trunc` lower to the
            // overloaded LLVM float-vector intrinsics (`llvm.sqrt` is one
            // hardware `sqrtps`/`sqrtpd`; the rounding intrinsics lower to
            // `roundps`/`roundpd` on SSE4.1+; `llvm.exp`/`llvm.log` vectorize
            // where the target math lib supports it, else scalarize — still
            // correct). `sigmoid` = 1/(1+e^-x) and `tanh` = (e^2ˣ-1)/(e^2ˣ+1)
            // are derived from `exp` with vector arithmetic. `round` is
            // half-away-from-zero (`llvm.round`, matching the scalar `.round()`
            // and the interpreter). The typechecker guarantees a float element.
            "sqrt" | "exp" | "ln" | "sigmoid" | "tanh" | "floor" | "ceil" | "round" | "trunc" => {
                // The per-lane transcendental / rounding logic lives in
                // `apply_vector_float_unary` — shared with the tensor-map
                // vectorizer (`try_emit_vectorized_map`) so a `t.map(|x|
                // x.exp())` inner loop routes through the identical
                // polynomial. The typechecker guarantees a float element.
                let out = self
                    .apply_vector_float_unary(method, recv)
                    .expect("method matched the float-unary set above");
                Ok(out.into())
            }
            // `std.simd.math` bit-reinterpretation (phase-11): element-wise
            // IEEE-754 bitcast between a float vector and a same-width integer
            // vector, each a single LLVM vector `bitcast` (no data movement —
            // the same `<N x 32-or-64>` bits reinterpreted). `to_bits` picks
            // the int width from the receiver's float element (f32 → i32,
            // f64 → i64; the result is typed unsigned upstream); `bits_as_f32`
            // / `bits_as_f64` pick the float width from the method name. The
            // typechecker guarantees the receiver element width matches.
            "to_bits" => {
                let int_elem = if recv.get_type().get_element_type().into_float_type()
                    == self.context.f32_type()
                {
                    self.context.i32_type()
                } else {
                    self.context.i64_type()
                };
                let target = int_elem.vec_type(n);
                let out = self
                    .builder
                    .build_bit_cast(recv, target, "to_bits")
                    .map_err(|e| format!("vector to_bits bitcast failed: {e}"))?;
                Ok(out)
            }
            "bits_as_f32" | "bits_as_f64" => {
                let float_elem = if method == "bits_as_f32" {
                    self.context.f32_type()
                } else {
                    self.context.f64_type()
                };
                let target = float_elem.vec_type(n);
                let out = self
                    .builder
                    .build_bit_cast(recv, target, method)
                    .map_err(|e| format!("vector {method} bitcast failed: {e}"))?;
                Ok(out)
            }
            "reduce_sum" | "reduce_product" | "reduce_and" | "reduce_or" | "reduce_xor" => {
                let fold_op = match method {
                    "reduce_sum" => BinOp::Add,
                    "reduce_product" => BinOp::Mul,
                    "reduce_and" => BinOp::BitAnd,
                    "reduce_or" => BinOp::BitOr,
                    _ => BinOp::BitXor, // reduce_xor
                };
                let mut acc = lane(self, recv, 0)?;
                for i in 1..n {
                    let l = lane(self, recv, i)?;
                    acc = self.compile_binop(&fold_op, acc, l)?;
                }
                Ok(acc)
            }
            // Horizontal min/max via compare + select. Element is numeric
            // (signed-int / unsigned-int / float). The LLVM lane type is
            // signless, so signedness rides the `unsigned_vector_exprs` span
            // side-table keyed by the receiver-vector expression: a hit means
            // the element is unsigned → `ult`/`ugt` via `compile_binop_typed`;
            // otherwise the signed (`slt`/`sgt`) / ordered float compare.
            "reduce_min" | "reduce_max" => {
                let cmp_op = if method == "reduce_min" {
                    BinOp::Lt
                } else {
                    BinOp::Gt
                };
                let is_unsigned = self
                    .span_tables
                    .unsigned_vector_exprs
                    .contains(&(object.span.offset, object.span.length));
                let mut acc = lane(self, recv, 0)?;
                for i in 1..n {
                    let l = lane(self, recv, i)?;
                    // keep `acc` when `acc <op> l` holds, else take `l`.
                    let cmp = self
                        .compile_binop_typed(&cmp_op, acc, l, is_unsigned)?
                        .into_int_value();
                    acc = self
                        .builder
                        .build_select(cmp, acc, l, "minmax")
                        .map_err(|e| format!("vector min/max select failed: {e}"))?;
                }
                Ok(acc)
            }
            "dot" => {
                let other = self.compile_expr(&args[0].value)?.into_vector_value();
                let mut acc: Option<BasicValueEnum<'ctx>> = None;
                for i in 0..n {
                    let a = lane(self, recv, i)?;
                    let b = lane(self, other, i)?;
                    let prod = self.compile_binop(&BinOp::Mul, a, b)?;
                    acc = Some(match acc {
                        None => prod,
                        Some(s) => self.compile_binop(&BinOp::Add, s, prod)?,
                    });
                }
                // N >= 1 guaranteed by the typechecker.
                acc.ok_or_else(|| "dot on a zero-lane vector".to_string())
            }
            // Cross product — `<3 x T>` only (the typechecker rejects any
            // other lane count and a non-same-typed argument). Compute the
            // three components with scalar `compile_binop` (`c_i = p*q - r*s`)
            // and reassemble a `<3 x T>` vector via `insertelement`.
            // `BasicValueEnum` is `Copy`, so each lane is reused across the
            // components without re-extracting.
            "cross" => {
                let other = self.compile_expr(&args[0].value)?.into_vector_value();
                let (a0, a1, a2) = (
                    lane(self, recv, 0)?,
                    lane(self, recv, 1)?,
                    lane(self, recv, 2)?,
                );
                let (b0, b1, b2) = (
                    lane(self, other, 0)?,
                    lane(self, other, 1)?,
                    lane(self, other, 2)?,
                );
                let component = |cg: &mut Self,
                                 p: BasicValueEnum<'ctx>,
                                 q: BasicValueEnum<'ctx>,
                                 r: BasicValueEnum<'ctx>,
                                 s: BasicValueEnum<'ctx>|
                 -> Result<BasicValueEnum<'ctx>, String> {
                    let pq = cg.compile_binop(&BinOp::Mul, p, q)?;
                    let rs = cg.compile_binop(&BinOp::Mul, r, s)?;
                    cg.compile_binop(&BinOp::Sub, pq, rs)
                };
                let c0 = component(self, a1, b2, a2, b1)?;
                let c1 = component(self, a2, b0, a0, b2)?;
                let c2 = component(self, a0, b1, a1, b0)?;
                let mut out = recv.get_type().get_undef();
                for (i, c) in [c0, c1, c2].into_iter().enumerate() {
                    out = self
                        .builder
                        .build_insert_element(
                            out,
                            c,
                            i32_t.const_int(i as u64, false),
                            "cross.lane",
                        )
                        .map_err(|e| format!("vector insertelement failed: {e}"))?;
                }
                Ok(out.into())
            }
            // `mask.select(a, b)` — per-lane blend via LLVM `select <N x i1>`.
            // `recv` is the `<N x i1>` mask; the two args are the `<N x T>` data
            // vectors. The typechecker guarantees matching lane counts.
            "select" => {
                let a = self.compile_expr(&args[0].value)?.into_vector_value();
                let b = self.compile_expr(&args[1].value)?.into_vector_value();
                self.builder
                    .build_select(recv, a, b, "vselect")
                    .map_err(|e| format!("vector select failed: {e}"))
            }
            // Lane permutations (design.md § Portable SIMD, "Lane shuffling").
            // Each builds the result `<N x T>` by extractelement-ing the source
            // lane at the permuted index and insertelement-ing it into the
            // result — a constant lane permutation LLVM folds to a single
            // `shufflevector`. `reverse`: result lane i = source lane N-1-i.
            // `rotate_lanes_left(k)`: result lane i = source lane (i+k) mod N.
            // `rotate_lanes_right(k)`: result lane i = source lane (i+N-k) mod N.
            "reverse" | "rotate_lanes_left" | "rotate_lanes_right" => {
                let shift = if method == "reverse" {
                    0
                } else {
                    // The typechecker guarantees a non-negative integer literal.
                    let amt = match &args[0].value.kind {
                        ExprKind::Integer(v, _) => *v as u64,
                        _ => {
                            return Err(format!(
                                "{method} amount must be a compile-time integer literal"
                            ))
                        }
                    };
                    (amt % n as u64) as u32
                };
                let mut out = recv.get_type().get_undef();
                for i in 0..n {
                    let src = match method {
                        "reverse" => n - 1 - i,
                        "rotate_lanes_left" => (i + shift) % n,
                        _ => (i + n - shift) % n, // rotate_lanes_right
                    };
                    let v = lane(self, recv, src)?;
                    out = self
                        .builder
                        .build_insert_element(out, v, i32_t.const_int(i as u64, false), "perm.lane")
                        .map_err(|e| format!("vector insertelement failed: {e}"))?;
                }
                Ok(out.into())
            }
            // `v.replace(i, x) -> Vector[T, N]` — a new vector with lane `i`
            // set to `x`, via insertelement at a runtime index. The index is
            // bounds-checked (panic on out-of-range) exactly like the `v[i]`
            // lane read — an unchecked insertelement with an OOB index is
            // poison in LLVM. The receiver is unchanged (the value is returned).
            "replace" => {
                let idx = self.compile_expr(&args[0].value)?.into_int_value();
                let x = self.compile_expr(&args[1].value)?;
                // Literal-width boundary coercion (`v.replace(0, 0.5)` on a
                // `Vector[f32, N]` lowers the bare literal as f64), same as
                // construction / splat / from_array.
                let x = self.coerce_scalar_to_type(x, recv.get_type().get_element_type());
                // Bounds-check `idx` against `N`, comparing in the index's own
                // int width (UGE so a negative index also trips the panic).
                let len = idx.get_type().const_int(n as u64, false);
                let fn_val = self.current_fn.unwrap();
                let oob_bb = self.context.append_basic_block(fn_val, "replace.oob");
                let ok_bb = self.context.append_basic_block(fn_val, "replace.ok");
                let cmp = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::UGE, idx, len, "replace.bounds")
                    .map_err(|e| format!("vector replace bounds compare failed: {e}"))?;
                self.builder
                    .build_conditional_branch(cmp, oob_bb, ok_bb)
                    .map_err(|e| format!("vector replace branch failed: {e}"))?;
                self.builder.position_at_end(oob_bb);
                self.emit_panic("vector lane index out of bounds");
                self.builder
                    .build_unreachable()
                    .map_err(|e| format!("vector replace unreachable failed: {e}"))?;
                self.builder.position_at_end(ok_bb);
                let out = self
                    .builder
                    .build_insert_element(recv, x, idx, "replace.lane")
                    .map_err(|e| format!("vector insertelement failed: {e}"))?;
                Ok(out.into())
            }
            // `v.shuffle([i0..i_{M-1}]) -> Vector[T, M]` — gather source lanes
            // by a compile-time index list into a fresh `M`-lane vector (which
            // may differ from the source `N`). The indices are integer literals
            // the typechecker has already range-checked into `[0, N)`; build
            // the result via extractelement(recv, idx) + insertelement, which
            // LLVM folds to a single `shufflevector`.
            "shuffle" => {
                let ExprKind::ArrayLiteral(items) = &args[0].value.kind else {
                    return Err(
                        "shuffle requires a compile-time array literal of lane indices".to_string(),
                    );
                };
                let m = items.len() as u32;
                let res_ty = match recv.get_type().get_element_type() {
                    BasicTypeEnum::IntType(t) => t.vec_type(m),
                    BasicTypeEnum::FloatType(t) => t.vec_type(m),
                    other => {
                        return Err(format!(
                            "shuffle: unsupported vector element type {other:?}"
                        ))
                    }
                };
                let mut out = res_ty.get_undef();
                for (j, it) in items.iter().enumerate() {
                    let src = match &it.kind {
                        ExprKind::Integer(v, _) => *v as u32,
                        _ => {
                            return Err(
                                "shuffle index must be a compile-time integer literal".to_string()
                            )
                        }
                    };
                    let v = lane(self, recv, src)?;
                    out = self
                        .builder
                        .build_insert_element(out, v, i32_t.const_int(j as u64, false), "shuf.lane")
                        .map_err(|e| format!("vector insertelement failed: {e}"))?;
                }
                Ok(out.into())
            }
            // `v.store_masked(slice, mask)` — write each active lane `v[i]`
            // through the `mut Slice[T]` (design.md § Portable SIMD, "Masked
            // load/store"; the write sibling of `load_masked`). Lane `i` is
            // active iff `mask[i]`; an active lane past the slice length traps
            // (`emit_panic`), and an inactive lane leaves the slice untouched.
            // Per lane: branch on `mask[i] && i >= len` to the panic block, then
            // on `mask[i]` to a store / skip pair. Returns unit (`i64 0`).
            "store_masked" => {
                let slice_val = self.compile_expr(&args[0].value)?.into_struct_value();
                let data = self
                    .builder
                    .build_extract_value(slice_val, 0, "store_masked.data")
                    .map_err(|e| format!("store_masked extract data failed: {e}"))?
                    .into_pointer_value();
                let len = self
                    .builder
                    .build_extract_value(slice_val, 1, "store_masked.len")
                    .map_err(|e| format!("store_masked extract len failed: {e}"))?
                    .into_int_value();
                let mask = self.compile_expr(&args[1].value)?.into_vector_value();
                let elem_ty = recv.get_type().get_element_type();
                let i64_t = self.context.i64_type();
                let fn_val = self.current_fn.unwrap();
                for i in 0..n {
                    let lane_idx = i32_t.const_int(i as u64, false);
                    let mask_i = self
                        .builder
                        .build_extract_element(mask, lane_idx, "store_masked.mask")
                        .map_err(|e| format!("store_masked extractelement mask failed: {e}"))?
                        .into_int_value();
                    let i_const = i64_t.const_int(i as u64, false);
                    let oob = self
                        .builder
                        .build_int_compare(IntPredicate::UGE, i_const, len, "store_masked.oob")
                        .map_err(|e| format!("store_masked bounds compare failed: {e}"))?;
                    let bad = self
                        .builder
                        .build_and(mask_i, oob, "store_masked.bad")
                        .map_err(|e| format!("store_masked and failed: {e}"))?;
                    let panic_bb = self
                        .context
                        .append_basic_block(fn_val, "store_masked.panic");
                    let ok_bb = self.context.append_basic_block(fn_val, "store_masked.ok");
                    self.builder
                        .build_conditional_branch(bad, panic_bb, ok_bb)
                        .map_err(|e| format!("store_masked panic branch failed: {e}"))?;
                    self.builder.position_at_end(panic_bb);
                    self.emit_panic("store_masked: active lane index out of bounds");
                    self.builder
                        .build_unreachable()
                        .map_err(|e| format!("store_masked unreachable failed: {e}"))?;

                    self.builder.position_at_end(ok_bb);
                    let store_bb = self
                        .context
                        .append_basic_block(fn_val, "store_masked.store");
                    let skip_bb = self.context.append_basic_block(fn_val, "store_masked.skip");
                    self.builder
                        .build_conditional_branch(mask_i, store_bb, skip_bb)
                        .map_err(|e| format!("store_masked active branch failed: {e}"))?;
                    // Active lane → store `v[i]` into `data[i]`.
                    self.builder.position_at_end(store_bb);
                    let v_i = lane(self, recv, i)?;
                    let elem_ptr = unsafe {
                        self.builder
                            .build_gep(elem_ty, data, &[i_const], "store_masked.elem.ptr")
                            .map_err(|e| format!("store_masked gep failed: {e}"))?
                    };
                    self.builder
                        .build_store(elem_ptr, v_i)
                        .map_err(|e| format!("store_masked store failed: {e}"))?;
                    self.builder
                        .build_unconditional_branch(skip_bb)
                        .map_err(|e| format!("store_masked store->skip failed: {e}"))?;
                    // Inactive lane (or fall-through) continues at `skip_bb`.
                    self.builder.position_at_end(skip_bb);
                }
                Ok(i64_t.const_zero().into())
            }
            // `v.scatter(slice, indices)` — write each lane `v[i]` to
            // `slice[indices[i]]` (design.md § Portable SIMD, "Gather /
            // scatter"; the write mirror of `gather`). Every lane is active;
            // each index is widened to i64 and bounds-checked (`UGE idx, len`,
            // so a negative signed index also traps) before the store. Returns
            // unit (`i64 0`).
            "scatter" => {
                let slice_val = self.compile_expr(&args[0].value)?.into_struct_value();
                let data = self
                    .builder
                    .build_extract_value(slice_val, 0, "scatter.data")
                    .map_err(|e| format!("scatter extract data failed: {e}"))?
                    .into_pointer_value();
                let len = self
                    .builder
                    .build_extract_value(slice_val, 1, "scatter.len")
                    .map_err(|e| format!("scatter extract len failed: {e}"))?
                    .into_int_value();
                let indices = self.compile_expr(&args[1].value)?.into_vector_value();
                let elem_ty = recv.get_type().get_element_type();
                let i64_t = self.context.i64_type();
                let fn_val = self.current_fn.unwrap();
                for i in 0..n {
                    let lane_idx = i32_t.const_int(i as u64, false);
                    let raw = self
                        .builder
                        .build_extract_element(indices, lane_idx, "scatter.idx")
                        .map_err(|e| format!("scatter extractelement index failed: {e}"))?
                        .into_int_value();
                    let idx = match raw.get_type().get_bit_width().cmp(&64) {
                        std::cmp::Ordering::Less => self
                            .builder
                            .build_int_s_extend(raw, i64_t, "scatter.idx.sx")
                            .map_err(|e| format!("scatter index sext failed: {e}"))?,
                        std::cmp::Ordering::Greater => self
                            .builder
                            .build_int_truncate(raw, i64_t, "scatter.idx.tr")
                            .map_err(|e| format!("scatter index truncate failed: {e}"))?,
                        std::cmp::Ordering::Equal => raw,
                    };
                    let oob = self
                        .builder
                        .build_int_compare(IntPredicate::UGE, idx, len, "scatter.oob")
                        .map_err(|e| format!("scatter bounds compare failed: {e}"))?;
                    let panic_bb = self.context.append_basic_block(fn_val, "scatter.panic");
                    let ok_bb = self.context.append_basic_block(fn_val, "scatter.ok");
                    self.builder
                        .build_conditional_branch(oob, panic_bb, ok_bb)
                        .map_err(|e| format!("scatter panic branch failed: {e}"))?;
                    self.builder.position_at_end(panic_bb);
                    self.emit_panic("scatter: index out of bounds");
                    self.builder
                        .build_unreachable()
                        .map_err(|e| format!("scatter unreachable failed: {e}"))?;

                    self.builder.position_at_end(ok_bb);
                    let v_i = lane(self, recv, i)?;
                    let elem_ptr = unsafe {
                        self.builder
                            .build_gep(elem_ty, data, &[idx], "scatter.elem.ptr")
                            .map_err(|e| format!("scatter gep failed: {e}"))?
                    };
                    self.builder
                        .build_store(elem_ptr, v_i)
                        .map_err(|e| format!("scatter store failed: {e}"))?;
                }
                Ok(i64_t.const_zero().into())
            }
            other => Err(format!("unsupported Vector method '{other}' in codegen")),
        }
    }

    /// Lower `gpu.dispatch(kernel, buffer)` (spike slice-0c). The typechecker
    /// already validated the slice-0 element-wise-map contract and baked the
    /// kernel's WGSL into `gpu_dispatch_wgsl` (keyed on the kernel-arg span);
    /// here we bake that shader as a constant, read the input `Vec[f32]`'s
    /// `{data, len}`, call `karac_runtime_gpu_f32_map`, and wrap the returned
    /// `malloc`'d buffer as an owned `Vec[f32]` of the same length. The result
    /// buffer is exactly `n` f32s (element-wise maps preserve length), so
    /// `len == cap == n` and the binding's own scope drop frees it.
    /// `gpu.sum` / `gpu.prod` / `gpu.min` / `gpu.max` — a whole-buffer
    /// reduction lowering to ONE value (B-2026-08-19-10, extended by
    /// B-2026-08-19-13).
    ///
    /// Same prologue as [`Self::compile_gpu_dispatch`] — bake the WGSL the
    /// typechecker recorded, read `{data, len}` off the `Vec` — but the call
    /// returns an `f32` value instead of a buffer pointer, so there is no
    /// result `Vec` to build and nothing to free on the way out.
    ///
    /// The result is bit-identical to the interpreter's, because both run the
    /// tree order fixed in `reduce_kernel::tree_reduce_f32`. That is the
    /// property the whole slice exists to preserve.
    ///
    /// `sum`/`prod` lower to the bare call. `min`/`max` wrap it in an
    /// `Option[f32]` — empty in, `None` out — which is the same shape
    /// `Stats.min` produces and the reason this function branches at all.
    pub(super) fn compile_gpu_reduce(
        &mut self,
        args: &[CallArg],
        spelling: &str,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        if args.len() != 1 {
            return Err(format!(
                "gpu.{spelling} expects one buffer, found {} argument(s)",
                args.len()
            ));
        }

        // WGSL baked by the typechecker, keyed on the BUFFER-argument span —
        // `gpu.sum` has no kernel argument, so arg 0 is the buffer and the key
        // is the same expression the typechecker used.
        let key = (args[0].value.span.offset, args[0].value.span.length);
        let wgsl = self
            .accel
            .gpu_dispatch_wgsl
            .get(&key)
            .cloned()
            .ok_or_else(|| {
                format!(
                    "internal error: no WGSL recorded for `gpu.{spelling}` — the typechecker \
                     intercept must run before codegen"
                )
            })?;

        let i64_t = self.context.i64_type();
        let wgsl_len = i64_t.const_int(wgsl.len() as u64, false);
        let wgsl_ptr = self
            .builder
            .build_global_string_ptr(&wgsl, "gpu.reduce.wgsl")
            .map_err(|e| format!("baking gpu.{spelling} shader constant failed: {e}"))?
            .as_pointer_value();

        let (data, n) = self.read_gpu_reduce_buffer(&args[0].value)?;

        // INTEGER buffers take a different entry point entirely — one that can
        // TRAP. Selected from the typechecker's plain-data hint, because
        // nothing in the LLVM types distinguishes `Vec[i32]` from `Vec[f32]`
        // here (data pointers are opaque).
        if let Some(elem) = self.accel.gpu_reduce_int_elems.get(&key).cloned() {
            return self.compile_gpu_reduce_int(spelling, &elem, wgsl_ptr, wgsl_len, data, n);
        }

        // karac_runtime_gpu_reduce_f32(wgsl_ptr, wgsl_len, in_ptr, n,
        // identity) -> f32. The identity is passed rather than baked into the
        // runtime because an EMPTY buffer short-circuits before any dispatch,
        // and `gpu.prod([])` is 1 where `gpu.sum([])` is 0 — the interpreter
        // twin says so, and this is the one input no shader ever sees. For
        // `min`/`max` it is ±∞, which is also what pads a short chunk, so a
        // real `f32::MAX` element cannot be beaten by the padding.
        let f32_t = self.context.f32_type();
        let identity = match spelling {
            "prod" => f32_t.const_float(1.0),
            "min" => f32_t.const_float(f64::INFINITY),
            "max" => f32_t.const_float(f64::NEG_INFINITY),
            _ => f32_t.const_float(0.0),
        };
        let reduce_fn = self.gpu_reduce_f32_fn();
        let call_reduce = |me: &Self| {
            me.builder
                .build_call(
                    reduce_fn,
                    &[
                        wgsl_ptr.into(),
                        wgsl_len.into(),
                        data.into(),
                        n.into(),
                        identity.into(),
                    ],
                    "gpu.reduced",
                )
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
        };

        // `sum`/`prod` always have an answer, so the call IS the result.
        if !matches!(spelling, "min" | "max" | "mean") {
            return Ok(call_reduce(self));
        }

        // `min`/`max`/`mean` return `Option[f32]`: an empty buffer has no
        // extremum and no mean. Guarded HERE rather than in the runtime
        // because the runtime's empty short-circuit returns the identity, and
        // ±∞ (or `0.0 / 0`, NaN) is exactly the plausible wrong answer this
        // family must not produce. Same branch-and-phi shape as
        // `stats_minmax`, which refuses the same input for the same reason.
        let i64_t = self.context.i64_type();
        let fn_val = self.current_fn.expect("gpu reduce in function");
        let nonempty = self
            .builder
            .build_int_compare(IntPredicate::UGT, n, i64_t.const_zero(), "gpu.mm.ne")
            .unwrap();
        let some_bb = self.context.append_basic_block(fn_val, "gpu.mm.some");
        let none_bb = self.context.append_basic_block(fn_val, "gpu.mm.none");
        let merge_bb = self.context.append_basic_block(fn_val, "gpu.mm.merge");
        self.builder
            .build_conditional_branch(nonempty, some_bb, none_bb)
            .unwrap();

        self.builder.position_at_end(some_bb);
        let reduced = call_reduce(self);
        // `mean` is the tree sum divided ONCE, on the host, after the fold has
        // converged — a shader cannot know it is running the last level, so a
        // division inside it would divide once per level. That is the whole of
        // the operation beyond `sum`: no shader of its own, one `fdiv`. It
        // also makes `gpu.mean(v)` and `gpu.sum(v) / (v.len() as f32)` the
        // same number to the last bit, which the twin relies on.
        let reduced = if spelling == "mean" {
            let n_f32 = self
                .builder
                .build_signed_int_to_float(n, f32_t, "gpu.mean.n")
                .unwrap();
            self.builder
                .build_float_div(reduced.into_float_value(), n_f32, "gpu.mean")
                .unwrap()
                .into()
        } else {
            reduced
        };
        let word = self.coerce_to_payload_words(reduced, 1)?[0];
        let some_end_bb = self.builder.get_insert_block().unwrap();
        self.builder.build_unconditional_branch(merge_bb).unwrap();

        self.builder.position_at_end(none_bb);
        self.builder.build_unconditional_branch(merge_bb).unwrap();

        self.builder.position_at_end(merge_bb);
        Ok(self.build_option_some_via_phis(&[word], some_end_bb, none_bb, "gpu.mm"))
    }

    /// The INTEGER arm of `compile_gpu_reduce` (B-2026-08-19-13).
    ///
    /// A different runtime entry point, not a different constant: the integer
    /// reduction can TRAP on overflow, so it returns void and writes through
    /// an out-slot, and the abort happens inside the runtime the moment any
    /// workgroup raises its overflow flag. Kāra traps on integer overflow, and
    /// `v.sum()` over a `Vec[i32]` already fails on the same condition — a
    /// wrapping GPU sum would be a plausible wrong number where the CPU
    /// refused.
    ///
    /// The identity is the operation's own and is also what pads a short
    /// chunk, so a real `i32::MAX` element cannot be beaten by the padding.
    fn compile_gpu_reduce_int(
        &mut self,
        spelling: &str,
        elem: &str,
        wgsl_ptr: PointerValue<'ctx>,
        wgsl_len: IntValue<'ctx>,
        data: PointerValue<'ctx>,
        n: IntValue<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let i32_t = self.context.i32_type();
        let i64_t = self.context.i64_type();
        let unsigned = elem == "u32";
        // The identity's BITS, which is all the runtime carries: it copies the
        // slot on the empty path and never interprets it, so `u32::MAX` riding
        // in an `i32`-typed parameter is exact rather than lossy. (`min`/`max`
        // discard it anyway — an empty buffer is `None` — so it surfaces only
        // for the TOTAL folds, `sum` and `prod`.)
        //
        // `prod` is spelled out rather than left to the catch-all: the
        // multiplicative identity is 1, and `gpu.prod([])` answering 0 would
        // be a wrong answer for the one input no shader ever sees. Both the
        // float path and the interpreter twin already said 1 (design.md
        // § whole-buffer reductions), so a 0 here was a three-way
        // disagreement waiting on integer `prod` shipping.
        let identity = match (spelling, unsigned) {
            ("min", false) => i32_t.const_int(i32::MAX as u64, false),
            ("max", false) => i32_t.const_int(i32::MIN as u64, false),
            ("min", true) => i32_t.const_int(u32::MAX as u64, false),
            ("max", true) => i32_t.const_int(0, false),
            ("prod", _) => i32_t.const_int(1, false),
            _ => i32_t.const_int(0, false),
        };
        let out = self.builder.build_alloca(i32_t, "gpu.ireduce.out").unwrap();
        let reduce_fn = self.gpu_reduce_i32_fn();
        let status = self
            .builder
            .build_call(
                reduce_fn,
                &[
                    wgsl_ptr.into(),
                    wgsl_len.into(),
                    data.into(),
                    n.into(),
                    identity.into(),
                    out.into(),
                ],
                "gpu.ireduce.status",
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();

        // The trap. Raised HERE rather than inside the runtime so it is Kāra's
        // own panic — same `integer overflow` message, exit code and source
        // span that `v.sum()` over a `Vec[i32]` already produces for the
        // identical condition. A runtime-side abort would be a bare SIGABRT
        // with no span: a worse diagnostic for the same failure.
        let fn_for_trap = self.current_fn.expect("gpu int reduce in function");
        let overflowed = self
            .builder
            .build_int_compare(
                IntPredicate::NE,
                status,
                i32_t.const_zero(),
                "gpu.ireduce.ovf",
            )
            .unwrap();
        let trap_bb = self
            .context
            .append_basic_block(fn_for_trap, "gpu.ireduce.trap");
        let ok_bb = self
            .context
            .append_basic_block(fn_for_trap, "gpu.ireduce.ok");
        self.builder
            .build_conditional_branch(overflowed, trap_bb, ok_bb)
            .unwrap();
        self.builder.position_at_end(trap_bb);
        self.emit_panic("integer overflow");
        self.builder.build_unreachable().unwrap();
        self.builder.position_at_end(ok_bb);

        // `mean` PROMOTES, matching `Stats.mean` — the mean of `[1, 2]` is 1.5,
        // not 1 — and it promotes to f64 because the integer sum it divides is
        // EXACT. Widening a finished i32/u32 sum to f64 is lossless (32 bits
        // into a 53-bit mantissa), so the whole operation rounds exactly once,
        // at the divide. Promoting the ELEMENTS first, as `Stats.mean` does,
        // would be worse here: on a GPU that means f32, whose 24-bit mantissa
        // loses whole integers above 16777216.
        if spelling == "mean" {
            return self.compile_gpu_int_mean(out, n, unsigned);
        }

        // `sum` always has an answer (the empty case is 0), so the slot IS the
        // result — widened to the i64 carrier every integer travels in.
        if !matches!(spelling, "min" | "max") {
            let v = self
                .builder
                .build_load(i32_t, out, "gpu.isum")
                .unwrap()
                .into_int_value();
            return Ok(self
                .widen_gpu_reduce_result(v, unsigned, "gpu.isum.ext")
                .into());
        }

        // `min`/`max` are `Option[i32]`: an empty buffer has no extremum, and
        // the fold would otherwise return the padding identity — `i32::MAX`,
        // a plausible wrong answer. Same branch-and-phi shape as the float arm.
        let fn_val = self.current_fn.expect("gpu int reduce in function");
        let nonempty = self
            .builder
            .build_int_compare(IntPredicate::UGT, n, i64_t.const_zero(), "gpu.imm.ne")
            .unwrap();
        let some_bb = self.context.append_basic_block(fn_val, "gpu.imm.some");
        let none_bb = self.context.append_basic_block(fn_val, "gpu.imm.none");
        let merge_bb = self.context.append_basic_block(fn_val, "gpu.imm.merge");
        self.builder
            .build_conditional_branch(nonempty, some_bb, none_bb)
            .unwrap();

        self.builder.position_at_end(some_bb);
        let v = self
            .builder
            .build_load(i32_t, out, "gpu.imm")
            .unwrap()
            .into_int_value();
        let word = self.widen_gpu_reduce_result(v, unsigned, "gpu.imm.ext");
        let some_end_bb = self.builder.get_insert_block().unwrap();
        self.builder.build_unconditional_branch(merge_bb).unwrap();

        self.builder.position_at_end(none_bb);
        self.builder.build_unconditional_branch(merge_bb).unwrap();

        self.builder.position_at_end(merge_bb);
        Ok(self.build_option_some_via_phis(&[word], some_end_bb, none_bb, "gpu.imm"))
    }

    /// The tail of an integer `gpu.mean`: turn the exact 32-bit sum sitting in
    /// `out` into `Option[f64]` (B-2026-08-19-13).
    ///
    /// Empty is `None` — the mean of nothing is not a number, and dividing by
    /// zero would hand back a NaN that looks like an answer. Checked on the
    /// LENGTH rather than on the sum, because a sum of 0 is a perfectly good
    /// answer for a non-empty buffer.
    ///
    /// The widen is signed or unsigned to match the element type, and it is
    /// LOSSLESS either way: 32 bits fit a 53-bit mantissa with room to spare.
    /// So the only rounding in the whole operation is the single `fdiv`.
    fn compile_gpu_int_mean(
        &mut self,
        out: PointerValue<'ctx>,
        n: IntValue<'ctx>,
        unsigned: bool,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let i32_t = self.context.i32_type();
        let i64_t = self.context.i64_type();
        let f64_t = self.context.f64_type();
        let fn_val = self.current_fn.expect("gpu int mean in function");

        let nonempty = self
            .builder
            .build_int_compare(IntPredicate::UGT, n, i64_t.const_zero(), "gpu.imean.ne")
            .unwrap();
        let some_bb = self.context.append_basic_block(fn_val, "gpu.imean.some");
        let none_bb = self.context.append_basic_block(fn_val, "gpu.imean.none");
        let merge_bb = self.context.append_basic_block(fn_val, "gpu.imean.merge");
        self.builder
            .build_conditional_branch(nonempty, some_bb, none_bb)
            .unwrap();

        self.builder.position_at_end(some_bb);
        let sum = self
            .builder
            .build_load(i32_t, out, "gpu.imean.sum")
            .unwrap()
            .into_int_value();
        let sum_f = if unsigned {
            self.builder
                .build_unsigned_int_to_float(sum, f64_t, "gpu.imean.sumf")
                .unwrap()
        } else {
            self.builder
                .build_signed_int_to_float(sum, f64_t, "gpu.imean.sumf")
                .unwrap()
        };
        // The count is always non-negative, but it arrives as the i64 carrier,
        // so a signed conversion is the honest one.
        let n_f = self
            .builder
            .build_signed_int_to_float(n, f64_t, "gpu.imean.nf")
            .unwrap();
        let mean = self
            .builder
            .build_float_div(sum_f, n_f, "gpu.imean")
            .unwrap();
        let word = self.coerce_to_payload_words(mean.into(), 1)?[0];
        let some_end_bb = self.builder.get_insert_block().unwrap();
        self.builder.build_unconditional_branch(merge_bb).unwrap();

        self.builder.position_at_end(none_bb);
        self.builder.build_unconditional_branch(merge_bb).unwrap();

        self.builder.position_at_end(merge_bb);
        Ok(self.build_option_some_via_phis(&[word], some_end_bb, none_bb, "gpu.imean"))
    }

    /// Widen a 32-bit reduction result into Kāra's i64 integer carrier.
    ///
    /// **The one place signedness matters on this path.** The runtime entry
    /// point is bit-transparent — it casts the input straight to `*const u8`
    /// and writes the output from raw little-endian bytes, so a `u32` rides
    /// through its `i32`-typed slots exactly. The shader does the interpreting
    /// (`array<u32>` gives unsigned `min`/`max` and an unsigned carry check).
    /// Only here does the 32-bit word have to become a 64-bit one, and a
    /// SIGN-extend would report every `u32` at or above 2^31 as negative —
    /// `gpu.max` of `[u32::MAX]` coming back as `-1`. A plausible wrong number
    /// rather than a crash, which is the failure mode this family exists to
    /// avoid.
    fn widen_gpu_reduce_result(
        &self,
        v: IntValue<'ctx>,
        unsigned: bool,
        name: &str,
    ) -> IntValue<'ctx> {
        let i64_t = self.context.i64_type();
        if unsigned {
            self.builder.build_int_z_extend(v, i64_t, name).unwrap()
        } else {
            self.builder.build_int_s_extend(v, i64_t, name).unwrap()
        }
    }

    /// Read a reduction argument's `Vec` down to `{data, len}`.
    ///
    /// Spill + scalar `struct_gep` rather than an aggregate load +
    /// `extractvalue`, which mis-lowers the pointer field to null under
    /// arm64-Linux ASan — the same hazard the dispatch path documents.
    ///
    /// Also registers a fresh owned temp (`gpu.sum([..])`, `gpu.dot([..], b)`)
    /// for cleanup: a literal argument has no binding to drop it.
    fn read_gpu_reduce_buffer(
        &mut self,
        arg: &Expr,
    ) -> Result<(PointerValue<'ctx>, IntValue<'ctx>), String> {
        let i64_t = self.context.i64_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let buf_val = self.compile_expr(arg)?;
        let sv = buf_val.into_struct_value();
        let vec_ty = sv.get_type();
        let spill = self.builder.build_alloca(vec_ty, "gpu.rbuf").unwrap();
        self.builder.build_store(spill, sv).unwrap();
        let data_field = self
            .builder
            .build_struct_gep(vec_ty, spill, 0, "gpu.rdata.p")
            .unwrap();
        let data = self
            .builder
            .build_load(ptr_ty, data_field, "gpu.rdata")
            .unwrap()
            .into_pointer_value();
        let len_field = self
            .builder
            .build_struct_gep(vec_ty, spill, 1, "gpu.rlen.p")
            .unwrap();
        let n = self
            .builder
            .build_load(i64_t, len_field, "gpu.rn")
            .unwrap()
            .into_int_value();

        let is_fresh_temp = matches!(
            arg.kind,
            ExprKind::ArrayLiteral { .. } | ExprKind::PrefixCollectionLiteral { .. }
        );
        if is_fresh_temp && self.llvm_ty_is_vec_struct(buf_val.get_type()) {
            self.materialize_owned_temp(buf_val, (arg.span.offset, arg.span.length));
        }
        Ok((data, n))
    }

    /// `gpu.variance(buf)` / `gpu.stddev(buf)` — the two-pass statistics
    /// (B-2026-08-19-13).
    ///
    /// Bakes TWO shaders, keyed like the Arg family's: the DEVIATION kernel on
    /// the buffer-argument span and the SUM kernel on the call span. The sum
    /// kernel does double duty inside the runtime — the whole first pass that
    /// produces the mean, and the fold over the second pass's partials.
    ///
    /// The runtime returns the sum of squared deviations; the divisor and the
    /// square root are applied HERE, because they are the parts the CPU twin
    /// has to mirror and because it lets one entry point serve both spellings.
    /// POPULATION form (÷ n), matching `Stats.variance` / `Stats.stddev`.
    /// Lower `gpu.prefix_sum(buffer)` to a fresh owned `Vec[f32]`
    /// (B-2026-08-19-13).
    ///
    /// **The only GPU lowering here that allocates a result buffer.** Every
    /// reduction before it produced one value; `n` of them need storage, and
    /// codegen is what owns it — the `Vec` returned from here is an ordinary
    /// owned temporary that the caller's scope frees like any other, so
    /// nothing about the ownership story is GPU-specific.
    ///
    /// Two shaders, keyed exactly as `gpu.variance` keys its pair: phase 1 on
    /// the ARGUMENT span, phase 3 on the CALL span. Phase 2 needs none of its
    /// own — the runtime runs the same two again over the chunk totals.
    ///
    /// No empty-buffer branch, and that is the point of returning a `Vec`
    /// rather than an `Option`: `n = 0` allocates zero bytes, the runtime
    /// returns without dispatching, and the result is the empty Vec. There is
    /// no "no answer" case to guard.
    pub(super) fn compile_gpu_prefix_sum(
        &mut self,
        args: &[CallArg],
        call_span: &crate::token::Span,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        if args.len() != 1 {
            return Err(format!(
                "gpu.prefix_sum expects one buffer, found {} argument(s)",
                args.len()
            ));
        }
        let i64_t = self.context.i64_type();

        let mut baked = Vec::with_capacity(2);
        for (key, label) in [
            (
                (args[0].value.span.offset, args[0].value.span.length),
                "gpu.scan.wgsl",
            ),
            ((call_span.offset, call_span.length), "gpu.scan.off.wgsl"),
        ] {
            let wgsl = self
                .accel
                .gpu_dispatch_wgsl
                .get(&key)
                .cloned()
                .ok_or_else(|| {
                    "internal error: no WGSL recorded for `gpu.prefix_sum` — the typechecker \
                     intercept must run before codegen"
                        .to_string()
                })?;
            let len = i64_t.const_int(wgsl.len() as u64, false);
            let ptr = self
                .builder
                .build_global_string_ptr(&wgsl, label)
                .map_err(|e| format!("baking gpu prefix-sum shader constant failed: {e}"))?
                .as_pointer_value();
            baked.push((ptr, len));
        }

        let (data, n) = self.read_gpu_reduce_buffer(&args[0].value)?;

        // The destination: n four-byte elements, matching the `Vec[f32]` the
        // typechecker promised. `alloc_or_panic(0)` on an empty buffer is the
        // same no-op the regex `find_all` path already relies on.
        let bytes = self
            .builder
            .build_int_mul(n, i64_t.const_int(4, false), "gpu.scan.bytes")
            .unwrap();
        let out = self
            .builder
            .build_call(
                self.runtime_fns.alloc_or_panic_fn,
                &[bytes.into()],
                "gpu.scan.buf",
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();

        let scan_fn = self.gpu_prefix_sum_f32_fn();
        self.builder
            .build_call(
                scan_fn,
                &[
                    baked[0].0.into(),
                    baked[0].1.into(),
                    baked[1].0.into(),
                    baked[1].1.into(),
                    data.into(),
                    n.into(),
                    out.into(),
                ],
                "",
            )
            .unwrap();

        // Capacity is the length: this buffer is sized exactly once and never
        // grown, so there is no slack to record.
        Ok(self.build_vec_value(out, n, n))
    }

    pub(super) fn compile_gpu_variance(
        &mut self,
        args: &[CallArg],
        call_span: &crate::token::Span,
        sqrt: bool,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        if args.len() != 1 {
            return Err(format!(
                "gpu.variance/stddev expects one buffer, found {} argument(s)",
                args.len()
            ));
        }
        let i64_t = self.context.i64_type();
        let f32_t = self.context.f32_type();

        let mut baked = Vec::with_capacity(2);
        for (key, label) in [
            (
                (args[0].value.span.offset, args[0].value.span.length),
                "gpu.var.dev.wgsl",
            ),
            ((call_span.offset, call_span.length), "gpu.var.sum.wgsl"),
        ] {
            let wgsl = self
                .accel
                .gpu_dispatch_wgsl
                .get(&key)
                .cloned()
                .ok_or_else(|| {
                    "internal error: no WGSL recorded for `gpu.variance`/`gpu.stddev` — the \
                     typechecker intercept must run before codegen"
                        .to_string()
                })?;
            let len = i64_t.const_int(wgsl.len() as u64, false);
            let ptr = self
                .builder
                .build_global_string_ptr(&wgsl, label)
                .map_err(|e| format!("baking gpu variance shader constant failed: {e}"))?
                .as_pointer_value();
            baked.push((ptr, len));
        }

        let (data, n) = self.read_gpu_reduce_buffer(&args[0].value)?;
        let fn_val = self.current_fn.expect("gpu variance in function");

        // INTEGER buffers take a different entry point entirely — one that is
        // EXACT and can TRAP. Selected from the typechecker's plain-data hint,
        // because a `Vec`'s data pointer is opaque at the LLVM level and
        // nothing here distinguishes `Vec[i32]` from `Vec[f32]`.
        let int_key = (args[0].value.span.offset, args[0].value.span.length);
        if let Some(elem) = self.accel.gpu_reduce_int_elems.get(&int_key).cloned() {
            return self.compile_gpu_variance_int(
                &elem, baked[0].0, baked[0].1, baked[1].0, baked[1].1, data, n, sqrt,
            );
        }

        let sumsq_fn = self.gpu_sumsq_dev_f32_fn();

        // Empty has no variance — `None`, the answer every other GPU reduction
        // gives for an empty buffer. Guarded BEFORE the divide, which would
        // otherwise be by zero.
        let nonempty = self
            .builder
            .build_int_compare(IntPredicate::UGT, n, i64_t.const_zero(), "gpu.var.ne")
            .unwrap();
        let some_bb = self.context.append_basic_block(fn_val, "gpu.var.some");
        let none_bb = self.context.append_basic_block(fn_val, "gpu.var.none");
        let merge_bb = self.context.append_basic_block(fn_val, "gpu.var.merge");
        self.builder
            .build_conditional_branch(nonempty, some_bb, none_bb)
            .unwrap();

        self.builder.position_at_end(some_bb);
        let ss = self
            .builder
            .build_call(
                sumsq_fn,
                &[
                    baked[0].0.into(),
                    baked[0].1.into(),
                    baked[1].0.into(),
                    baked[1].1.into(),
                    data.into(),
                    n.into(),
                ],
                "gpu.var.ss",
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_float_value();
        // Population divisor. The count goes through i64 -> f32 to match the
        // twin's `xs.len() as f32` exactly.
        let n_f = self
            .builder
            .build_signed_int_to_float(n, f32_t, "gpu.var.nf")
            .unwrap();
        let var = self.builder.build_float_div(ss, n_f, "gpu.var").unwrap();
        let result = if sqrt {
            // `llvm.sqrt` is overloaded on the float width; declared at f32
            // here so `gpu.stddev` roots the f32 variance rather than
            // round-tripping through f64, which would round twice.
            let intrinsic = inkwell::intrinsics::Intrinsic::find("llvm.sqrt")
                .expect("llvm.sqrt intrinsic must exist");
            let decl = intrinsic
                .get_declaration(&self.module, &[f32_t.into()])
                .expect("llvm.sqrt declaration for f32");
            self.builder
                .build_call(decl, &[var.into()], "gpu.std")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
        } else {
            var.into()
        };
        let word = self.coerce_to_payload_words(result, 1)?[0];
        let some_end_bb = self.builder.get_insert_block().unwrap();
        self.builder.build_unconditional_branch(merge_bb).unwrap();

        self.builder.position_at_end(none_bb);
        self.builder.build_unconditional_branch(merge_bb).unwrap();

        self.builder.position_at_end(merge_bb);
        Ok(self.build_option_some_via_phis(&[word], some_end_bb, none_bb, "gpu.var"))
    }

    /// Lower an INTEGER `gpu.dot(a, b)` (B-2026-08-19-13).
    ///
    /// Returns the ELEMENT type, exactly as `gpu.sum` does over the same
    /// buffer — the identity `dot == sum(a * b)` would not survive a different
    /// result type — and traps on overflow of either the product or the
    /// accumulation, raised HERE so it carries Kāra's own `integer overflow`
    /// message and span.
    #[allow(clippy::too_many_arguments)]
    fn compile_gpu_dot_int(
        &mut self,
        elem: &str,
        dot_ptr: PointerValue<'ctx>,
        dot_len: IntValue<'ctx>,
        sum_ptr: PointerValue<'ctx>,
        sum_len: IntValue<'ctx>,
        a_data: PointerValue<'ctx>,
        b_data: PointerValue<'ctx>,
        n: IntValue<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let i32_t = self.context.i32_type();
        let fn_val = self.current_fn.expect("gpu.dot in function");
        let out = self.builder.build_alloca(i32_t, "gpu.idot.out").unwrap();
        let dot_fn = self.gpu_dot_int_fn();
        let status = self
            .builder
            .build_call(
                dot_fn,
                &[
                    dot_ptr.into(),
                    dot_len.into(),
                    sum_ptr.into(),
                    sum_len.into(),
                    a_data.into(),
                    b_data.into(),
                    n.into(),
                    out.into(),
                ],
                "gpu.idot.status",
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();

        let overflowed = self
            .builder
            .build_int_compare(IntPredicate::NE, status, i32_t.const_zero(), "gpu.idot.ovf")
            .unwrap();
        let trap_bb = self.context.append_basic_block(fn_val, "gpu.idot.trap");
        let ok_bb = self.context.append_basic_block(fn_val, "gpu.idot.ok");
        self.builder
            .build_conditional_branch(overflowed, trap_bb, ok_bb)
            .unwrap();
        self.builder.position_at_end(trap_bb);
        self.emit_panic("integer overflow");
        self.builder.build_unreachable().unwrap();
        self.builder.position_at_end(ok_bb);

        let word = self
            .builder
            .build_load(i32_t, out, "gpu.idot.val")
            .unwrap()
            .into_int_value();
        // Widened into the i64 carrier the rest of the compiler uses for
        // integers, sign- or zero-extending per THIS CALL's element type — the
        // same decision `compile_gpu_reduce_int` makes for its result. Read
        // from the argument rather than from the side table at large, so a
        // program containing both an i32 and a u32 dot classifies each on its
        // own.
        let unsigned = elem == "u32";
        let i64_t = self.context.i64_type();
        let widened = if unsigned {
            self.builder
                .build_int_z_extend(word, i64_t, "gpu.idot.zext")
                .unwrap()
        } else {
            self.builder
                .build_int_s_extend(word, i64_t, "gpu.idot.sext")
                .unwrap()
        };
        Ok(widened.into())
    }

    /// Lower an INTEGER `gpu.variance` / `gpu.stddev` (B-2026-08-19-13).
    ///
    /// **Returns `Option[f64]`, not `Option[f32]`, and it is exact.** The
    /// integer path shifts by an integer `K`, squares into a true `u64` via
    /// the emitted widening multiply, and rounds once at the end — so an f32
    /// result would discard precision the computation genuinely has.
    /// `gpu.mean` over an integer buffer already promotes the same way, and
    /// for the same reason.
    ///
    /// Traps on overflow of `Σd²`, which is the only way an integer variance
    /// can fail, and raises the panic HERE rather than in the runtime so it
    /// carries Kāra's own `integer overflow` message and source span — the
    /// same reasoning as `compile_gpu_reduce_int`.
    #[allow(clippy::too_many_arguments)]
    fn compile_gpu_variance_int(
        &mut self,
        elem: &str,
        dev_ptr: PointerValue<'ctx>,
        dev_len: IntValue<'ctx>,
        fold_ptr: PointerValue<'ctx>,
        fold_len: IntValue<'ctx>,
        data: PointerValue<'ctx>,
        n: IntValue<'ctx>,
        sqrt: bool,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let i32_t = self.context.i32_type();
        let i64_t = self.context.i64_type();
        let fn_val = self.current_fn.expect("gpu variance in function");

        // Empty has no variance — `None`, guarded before the call so the
        // runtime never divides by zero.
        let nonempty = self
            .builder
            .build_int_compare(IntPredicate::UGT, n, i64_t.const_zero(), "gpu.ivar.ne")
            .unwrap();
        let some_bb = self.context.append_basic_block(fn_val, "gpu.ivar.some");
        let none_bb = self.context.append_basic_block(fn_val, "gpu.ivar.none");
        let merge_bb = self.context.append_basic_block(fn_val, "gpu.ivar.merge");
        self.builder
            .build_conditional_branch(nonempty, some_bb, none_bb)
            .unwrap();

        self.builder.position_at_end(some_bb);
        let ovf_slot = self.create_entry_alloca(fn_val, "gpu.ivar.ovf", i32_t.into());
        self.builder
            .build_store(ovf_slot, i32_t.const_zero())
            .unwrap();
        let var_fn = self.gpu_variance_int_fn();
        let unsigned = i32_t.const_int(u64::from(elem == "u32"), false);
        let want_sqrt = i32_t.const_int(u64::from(sqrt), false);
        let var = self
            .builder
            .build_call(
                var_fn,
                &[
                    dev_ptr.into(),
                    dev_len.into(),
                    fold_ptr.into(),
                    fold_len.into(),
                    data.into(),
                    n.into(),
                    unsigned.into(),
                    want_sqrt.into(),
                    ovf_slot.into(),
                ],
                "gpu.ivar",
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic();

        let status = self
            .builder
            .build_load(i32_t, ovf_slot, "gpu.ivar.status")
            .unwrap()
            .into_int_value();
        let overflowed = self
            .builder
            .build_int_compare(IntPredicate::NE, status, i32_t.const_zero(), "gpu.ivar.of")
            .unwrap();
        let trap_bb = self.context.append_basic_block(fn_val, "gpu.ivar.trap");
        let ok_bb = self.context.append_basic_block(fn_val, "gpu.ivar.ok");
        self.builder
            .build_conditional_branch(overflowed, trap_bb, ok_bb)
            .unwrap();
        self.builder.position_at_end(trap_bb);
        self.emit_panic("integer overflow");
        self.builder.build_unreachable().unwrap();
        self.builder.position_at_end(ok_bb);

        let word = self.coerce_to_payload_words(var, 1)?[0];
        let some_end_bb = self.builder.get_insert_block().unwrap();
        self.builder.build_unconditional_branch(merge_bb).unwrap();

        self.builder.position_at_end(none_bb);
        self.builder.build_unconditional_branch(merge_bb).unwrap();

        self.builder.position_at_end(merge_bb);
        Ok(self.build_option_some_via_phis(&[word], some_end_bb, none_bb, "gpu.ivar"))
    }

    /// `gpu.argmin(buf)` / `gpu.argmax(buf)` — the INDEX of the extremum
    /// (B-2026-08-19-13).
    ///
    /// Bakes TWO shaders: the level-0 kernel (recorded by the typechecker
    /// against the BUFFER-argument span) and the fold kernel (against the CALL
    /// span). One argument means the `gpu.dot` trick of keying on two argument
    /// spans is unavailable, and the two spans here are always distinct — the
    /// call encloses its argument.
    ///
    /// Returns `Option[i64]`: the runtime reports `u32::MAX` for an empty
    /// buffer, and an index otherwise. The sentinel test is on the RETURNED
    /// word rather than on the length, so there is exactly one place the
    /// "no extremum" answer is decided.
    pub(super) fn compile_gpu_arg(
        &mut self,
        args: &[CallArg],
        call_span: &crate::token::Span,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        if args.len() != 1 {
            return Err(format!(
                "gpu.argmin/argmax expects one buffer, found {} argument(s)",
                args.len()
            ));
        }
        let i32_t = self.context.i32_type();
        let i64_t = self.context.i64_type();

        let mut baked = Vec::with_capacity(2);
        for (key, label) in [
            (
                (args[0].value.span.offset, args[0].value.span.length),
                "gpu.arg.seed.wgsl",
            ),
            ((call_span.offset, call_span.length), "gpu.arg.fold.wgsl"),
        ] {
            let wgsl = self
                .accel
                .gpu_dispatch_wgsl
                .get(&key)
                .cloned()
                .ok_or_else(|| {
                    "internal error: no WGSL recorded for `gpu.argmin`/`gpu.argmax` — the \
                     typechecker intercept must run before codegen"
                        .to_string()
                })?;
            let len = i64_t.const_int(wgsl.len() as u64, false);
            let ptr = self
                .builder
                .build_global_string_ptr(&wgsl, label)
                .map_err(|e| format!("baking gpu arg shader constant failed: {e}"))?
                .as_pointer_value();
            baked.push((ptr, len));
        }

        let (data, n) = self.read_gpu_reduce_buffer(&args[0].value)?;
        let arg_fn = self.gpu_arg_index_fn();
        let idx = self
            .builder
            .build_call(
                arg_fn,
                &[
                    baked[0].0.into(),
                    baked[0].1.into(),
                    baked[1].0.into(),
                    baked[1].1.into(),
                    data.into(),
                    n.into(),
                ],
                "gpu.argidx",
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();

        // `u32::MAX` means "no extremum" — an empty buffer, the same answer
        // `Stats.argmin` gives. Compared as a raw word, so the sentinel and
        // the index share one representation and there is no second source of
        // truth about emptiness.
        let fn_val = self.current_fn.expect("gpu arg reduce in function");
        let sentinel = i32_t.const_int(u32::MAX as u64, false);
        let found = self
            .builder
            .build_int_compare(IntPredicate::NE, idx, sentinel, "gpu.arg.found")
            .unwrap();
        let some_bb = self.context.append_basic_block(fn_val, "gpu.arg.some");
        let none_bb = self.context.append_basic_block(fn_val, "gpu.arg.none");
        let merge_bb = self.context.append_basic_block(fn_val, "gpu.arg.merge");
        self.builder
            .build_conditional_branch(found, some_bb, none_bb)
            .unwrap();

        self.builder.position_at_end(some_bb);
        // ZERO-extend: an index is unsigned, and a buffer long enough to reach
        // 2^31 would otherwise report a negative position.
        let word = self
            .builder
            .build_int_z_extend(idx, i64_t, "gpu.arg.ext")
            .unwrap();
        let some_end_bb = self.builder.get_insert_block().unwrap();
        self.builder.build_unconditional_branch(merge_bb).unwrap();

        self.builder.position_at_end(none_bb);
        self.builder.build_unconditional_branch(merge_bb).unwrap();

        self.builder.position_at_end(merge_bb);
        Ok(self.build_option_some_via_phis(&[word], some_end_bb, none_bb, "gpu.arg"))
    }

    /// `gpu.dot(a, b)` — the fused multiply-then-sum reduction
    /// (B-2026-08-19-13).
    ///
    /// Bakes TWO shaders: the level-0 kernel that forms the product on load
    /// (recorded by the typechecker against `a`'s span) and the ordinary sum
    /// kernel that folds the per-workgroup partials (against `b`'s). Reusing
    /// the sum kernel past level 0 is what makes `gpu.dot(a, b)` and
    /// `gpu.sum(a * b)` bit-identical: after the first level they are the same
    /// computation over the same values.
    ///
    /// Both lengths are passed through. Equal lengths are a runtime condition
    /// — no Vec carries its length in the type — so the entry point traps on a
    /// mismatch rather than reading `b` past its end.
    pub(super) fn compile_gpu_dot(
        &mut self,
        args: &[CallArg],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        if args.len() != 2 {
            return Err(format!(
                "gpu.dot expects two buffers, found {} argument(s)",
                args.len()
            ));
        }
        let i64_t = self.context.i64_type();

        // Two shaders, keyed on the two argument spans — the same channel and
        // the same keying the typechecker wrote them with.
        let mut baked = Vec::with_capacity(2);
        for (idx, label) in [(0usize, "gpu.dot.wgsl"), (1usize, "gpu.dot.fold.wgsl")] {
            let key = (args[idx].value.span.offset, args[idx].value.span.length);
            let wgsl = self
                .accel
                .gpu_dispatch_wgsl
                .get(&key)
                .cloned()
                .ok_or_else(|| {
                    "internal error: no WGSL recorded for `gpu.dot` — the typechecker intercept \
                     must run before codegen"
                        .to_string()
                })?;
            let len = i64_t.const_int(wgsl.len() as u64, false);
            let ptr = self
                .builder
                .build_global_string_ptr(&wgsl, label)
                .map_err(|e| format!("baking gpu.dot shader constant failed: {e}"))?
                .as_pointer_value();
            baked.push((ptr, len));
        }

        let (a_data, a_n) = self.read_gpu_reduce_buffer(&args[0].value)?;
        let (b_data, b_n) = self.read_gpu_reduce_buffer(&args[1].value)?;

        // INTEGER buffers take the entry point that can TRAP. Selected from
        // the typechecker's plain-data hint, since a `Vec`'s data pointer is
        // opaque at the LLVM level.
        let int_key = (args[0].value.span.offset, args[0].value.span.length);
        if let Some(elem) = self.accel.gpu_reduce_int_elems.get(&int_key).cloned() {
            return self.compile_gpu_dot_int(
                &elem, baked[0].0, baked[0].1, baked[1].0, baked[1].1, a_data, b_data, a_n,
            );
        }
        let _ = b_n;

        let dot_fn = self.gpu_dot_f32_fn();
        let out = self
            .builder
            .build_call(
                dot_fn,
                &[
                    baked[0].0.into(),
                    baked[0].1.into(),
                    baked[1].0.into(),
                    baked[1].1.into(),
                    a_data.into(),
                    a_n.into(),
                    b_data.into(),
                    b_n.into(),
                ],
                "gpu.dotted",
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic();
        Ok(out)
    }

    pub(super) fn compile_gpu_dispatch(
        &mut self,
        args: &[CallArg],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        if args.len() < 2 {
            return Err(format!(
                "gpu.dispatch expects a kernel and a buffer, found {} argument(s)",
                args.len()
            ));
        }

        // GPU-SLIP-4b-2b: a `GpuBuffer[S]` argument (a `{i64 handle, i64 n}`
        // binding) is a RESIDENT device→device dispatch — no host round-trip.
        // Distinguished from a Vec-SoA / scalar buffer by the binding's LLVM slot
        // type; the typechecker already proved the arg is a `GpuBuffer[S]`.
        if let ExprKind::Identifier(buf_name) = &args[1].value.kind {
            if self
                .variables
                .get(buf_name)
                .is_some_and(|vs| vs.ty == self.gpu_buffer_type().into())
            {
                return self.compile_gpu_dispatch_resident(args);
            }
        }

        // CG-4: a struct buffer bound with a `layout` block dispatches multi-buffer
        // (one coalesced GPU buffer per group). Detect via the binding's SoA layout
        // — the typechecker is layout-blind, so codegen owns the per-group shader.
        if let ExprKind::Identifier(buf_name) = &args[1].value.kind {
            if let Some(soa) = self.active_soa_layout(buf_name) {
                return self.compile_gpu_dispatch_soa(args, &soa);
            }
        }

        // WGSL baked by the typechecker, keyed on the kernel-argument span.
        let key = (args[0].value.span.offset, args[0].value.span.length);
        let wgsl = self
            .accel
            .gpu_dispatch_wgsl
            .get(&key)
            .cloned()
            .ok_or_else(|| {
                "internal error: no WGSL recorded for `gpu.dispatch` — the typechecker \
             intercept must run before codegen"
                    .to_string()
            })?;

        // Bake the shader text as a global constant; pass (ptr, byte length).
        let i64_t = self.context.i64_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let wgsl_len = i64_t.const_int(wgsl.len() as u64, false);
        let wgsl_ptr = self
            .builder
            .build_global_string_ptr(&wgsl, "gpu.wgsl")
            .map_err(|e| format!("baking gpu.dispatch shader constant failed: {e}"))?
            .as_pointer_value();

        // Compile the input buffer and read {data ptr, len} via a spill +
        // scalar `struct_gep` — NOT an aggregate `load` + `extractvalue`,
        // which mis-lowers the pointer field to null under arm64-Linux ASan
        // (see the identical note in `src/codegen/stats.rs`).
        let buf_val = self.compile_expr(&args[1].value)?;
        let sv = buf_val.into_struct_value();
        let vec_ty = sv.get_type();
        let spill = self.builder.build_alloca(vec_ty, "gpu.buf").unwrap();
        self.builder.build_store(spill, sv).unwrap();
        let data_field = self
            .builder
            .build_struct_gep(vec_ty, spill, 0, "gpu.data.p")
            .unwrap();
        let data = self
            .builder
            .build_load(ptr_ty, data_field, "gpu.data")
            .unwrap()
            .into_pointer_value();
        let len_field = self
            .builder
            .build_struct_gep(vec_ty, spill, 1, "gpu.len.p")
            .unwrap();
        let n = self
            .builder
            .build_load(i64_t, len_field, "gpu.n")
            .unwrap()
            .into_int_value();

        // Free a fresh owned-temp buffer argument (`gpu.dispatch(k, [..])` /
        // a temporary), mirroring the Stats reduction paths. A named binding's
        // own scope drop already covers it, so only fresh temps / collection
        // literals are materialized; the helper self-guards on the Vec shape.
        let is_fresh_temp = self.expr_yields_fresh_owned_temp(&args[1].value)
            || matches!(
                &args[1].value.kind,
                ExprKind::PrefixCollectionLiteral { .. }
            );
        if is_fresh_temp && self.llvm_ty_is_vec_struct(buf_val.get_type()) {
            self.materialize_owned_temp(
                buf_val,
                (args[1].value.span.offset, args[1].value.span.length),
            );
        }

        // karac_runtime_gpu_map(wgsl_ptr, wgsl_len, in_ptr, n, elem_size) -> ptr.
        // Slice-0 supports only the WGSL-native 4-byte scalars (f32/i32/u32),
        // enforced by the typechecker + emitter, so `elem_size` is 4; the
        // byte-oriented runtime handles f32/i32/u32 uniformly.
        let elem_size = i64_t.const_int(4, false);
        let dispatch_fn = self.gpu_map_fn();
        let out_ptr = self
            .builder
            .build_call(
                dispatch_fn,
                &[
                    wgsl_ptr.into(),
                    wgsl_len.into(),
                    data.into(),
                    n.into(),
                    elem_size.into(),
                ],
                "gpu.out",
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();

        // Wrap the returned buffer as an owned `Vec[f32]` {ptr, len=n, cap=n}.
        let result_ty = self.vec_struct_type();
        let mut agg = result_ty.get_undef();
        agg = self
            .builder
            .build_insert_value(agg, out_ptr, 0, "gpu.res.data")
            .unwrap()
            .into_struct_value();
        agg = self
            .builder
            .build_insert_value(agg, n, 1, "gpu.res.len")
            .unwrap()
            .into_struct_value();
        agg = self
            .builder
            .build_insert_value(agg, n, 2, "gpu.res.cap")
            .unwrap()
            .into_struct_value();
        Ok(agg.into())
    }

    /// Materialize `vals` as an `[N x i64]` stack array (alloca + element stores)
    /// and return its pointer — used to pass the GPU-dispatch interleave descriptor
    /// arrays (`group_strides`, `field_group/src/dst`) to the runtime.
    pub(super) fn build_i64_stack_array(
        &self,
        vals: &[u64],
        name: &str,
    ) -> inkwell::values::PointerValue<'ctx> {
        let i64_t = self.context.i64_type();
        let ty = i64_t.array_type(vals.len().max(1) as u32);
        let arr = self.builder.build_alloca(ty, name).unwrap();
        for (idx, &v) in vals.iter().enumerate() {
            let slot = unsafe {
                self.builder
                    .build_in_bounds_gep(
                        ty,
                        arr,
                        &[i64_t.const_zero(), i64_t.const_int(idx as u64, false)],
                        "gpu.desc.e",
                    )
                    .unwrap()
            };
            self.builder
                .build_store(slot, i64_t.const_int(v, false))
                .unwrap();
        }
        arr
    }

    /// CG-4: lower `gpu.dispatch(kernel, buffer)` for a struct buffer bound with
    /// a `layout` block. The typechecker is layout-blind (it validated and left
    /// the WGSL to codegen), so here we recover the SoA group structure via
    /// `active_soa_layout`, emit the per-group multi-buffer shader, read one
    /// coalesced GPU buffer per group, dispatch, and wrap the interleaved AoS
    /// result as an owned `Vec[S]` `{ptr, len=n, cap=n}`.
    pub(super) fn compile_gpu_dispatch_soa(
        &mut self,
        args: &[CallArg],
        soa: &super::state::SoaLayout,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        // Path A: one field per hot group, no cold group — each group maps to one
        // `array<f32>` binding. Reject the shapes CG-4 has not grown to yet.
        if soa.cold_group.is_some() {
            return Err(
                "gpu.dispatch: a `cold` layout group is not supported (CG-4 Path A)".to_string(),
            );
        }
        let num_groups = soa.num_groups;
        if num_groups == 0 {
            return Err("gpu.dispatch: the layout has no field groups".to_string());
        }

        // Kernel `Function` AST (for the SoA emitter) from the program snapshot.
        let ExprKind::Identifier(kernel_name) = &args[0].value.kind else {
            return Err("gpu.dispatch kernel must be a bare `#[gpu]` function name".to_string());
        };
        let program = self
            .program_snapshot
            .clone()
            .ok_or("internal error: no program snapshot for gpu.dispatch")?;
        let kernel = program
            .items
            .iter()
            .find_map(|it| match it {
                crate::ast::Item::Function(f) if &f.name == kernel_name && f.is_gpu => Some(f),
                _ => None,
            })
            .ok_or_else(|| format!("internal error: gpu kernel `{kernel_name}` not found"))?;

        // Group manifest (binding order == group order); emit the multi-buffer
        // WGSL. All fields are f32 (typechecker-enforced for the struct path).
        let manifest: Vec<crate::gpu_wgsl::SoaGpuGroup> = soa
            .groups
            .iter()
            .map(|g| crate::gpu_wgsl::SoaGpuGroup {
                name: g.name.clone(),
                fields: g.fields.clone(),
            })
            .collect();
        // Other `#[gpu]` functions are candidate helpers the kernel may call
        // (GPU-LBM-5); the emitter selects + emits the reachable ones.
        let helpers: Vec<&crate::ast::Function> = program
            .items
            .iter()
            .filter_map(|it| match it {
                crate::ast::Item::Function(f) if f.is_gpu && &f.name != kernel_name => Some(f),
                _ => None,
            })
            .collect();
        let wgsl = crate::gpu_wgsl::emit_kernel_soa(kernel, &manifest, &helpers).map_err(|e| {
            format!(
                "gpu.dispatch: cannot lower `{kernel_name}` to a GPU shader — {}",
                e.reason()
            )
        })?;

        let i64_t = self.context.i64_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());

        // Bake the shader constant.
        let wgsl_len = i64_t.const_int(wgsl.len() as u64, false);
        let wgsl_ptr = self
            .builder
            .build_global_string_ptr(&wgsl, "gpu.wgsl.soa")
            .map_err(|e| format!("baking gpu.dispatch shader constant failed: {e}"))?
            .as_pointer_value();

        // Read the SoA buffer's per-group pointers + len via spill + struct_gep.
        let buf_val = self.compile_expr(&args[1].value)?;
        let sv = buf_val.into_struct_value();
        let vec_ty = sv.get_type();
        let spill = self.builder.build_alloca(vec_ty, "gpu.soa.buf").unwrap();
        self.builder.build_store(spill, sv).unwrap();

        let mut group_ptrs = Vec::with_capacity(num_groups);
        for k in 0..num_groups {
            let gp_field = self
                .builder
                .build_struct_gep(vec_ty, spill, k as u32, "gpu.soa.gp")
                .unwrap();
            let gp = self
                .builder
                .build_load(ptr_ty, gp_field, "gpu.soa.g")
                .unwrap()
                .into_pointer_value();
            group_ptrs.push(gp);
        }
        let len_idx = Self::soa_len_index(num_groups, false);
        let len_field = self
            .builder
            .build_struct_gep(vec_ty, spill, len_idx, "gpu.soa.len.p")
            .unwrap();
        let n = self
            .builder
            .build_load(i64_t, len_field, "gpu.soa.n")
            .unwrap()
            .into_int_value();

        // in_ptrs: `[num_groups x ptr]` on the stack, one group pointer each.
        let arr_ty = ptr_ty.array_type(num_groups as u32);
        let in_ptrs = self.builder.build_alloca(arr_ty, "gpu.in_ptrs").unwrap();
        for (k, gp) in group_ptrs.iter().enumerate() {
            let slot = unsafe {
                self.builder
                    .build_in_bounds_gep(
                        arr_ty,
                        in_ptrs,
                        &[i64_t.const_zero(), i64_t.const_int(k as u64, false)],
                        "gpu.in.k",
                    )
                    .unwrap()
            };
            self.builder.build_store(slot, *gp).unwrap();
        }

        // Per-group + per-field interleave descriptor (all fields f32, 4 bytes):
        //   group_strides[k] = (# fields in group k) × 4  (bytes per group element)
        //   for each struct field f (flattened in group order):
        //     field_group[f] = its group index
        //     field_src[f]   = its byte offset within that group's element (j × 4)
        //     field_dst[f]   = its byte offset within the AoS element (struct idx × 4)
        let group_strides: Vec<u64> = soa
            .groups
            .iter()
            .map(|g| (g.fields.len() * 4) as u64)
            .collect();
        let mut fld_group: Vec<u64> = Vec::new();
        let mut fld_src: Vec<u64> = Vec::new();
        let mut fld_dst: Vec<u64> = Vec::new();
        for (k, g) in soa.groups.iter().enumerate() {
            for (j, &struct_idx) in g.field_indices.iter().enumerate() {
                fld_group.push(k as u64);
                fld_src.push((j * 4) as u64);
                fld_dst.push((struct_idx * 4) as u64);
            }
        }
        let n_fields = fld_group.len();
        let strides_arr = self.build_i64_stack_array(&group_strides, "gpu.strides");
        let fgroup_arr = self.build_i64_stack_array(&fld_group, "gpu.fgroup");
        let fsrc_arr = self.build_i64_stack_array(&fld_src, "gpu.fsrc");
        let fdst_arr = self.build_i64_stack_array(&fld_dst, "gpu.fdst");

        // aos_stride = (# struct fields) × 4 (all f32, contiguous, no padding).
        let field_size = i64_t.const_int(4, false);
        let aos_stride = i64_t.const_int((n_fields * 4) as u64, false);
        let n_groups_v = i64_t.const_int(num_groups as u64, false);
        let n_fields_v = i64_t.const_int(n_fields as u64, false);

        // Scalar uniforms (GPU-LBM-2): the dispatch args beyond kernel + buffer.
        // Compile each to f32, spill to a stack slot, and pass an array of pointers
        // to those 4-byte values.
        let f32_t = self.context.f32_type();
        let n_uniforms = args.len().saturating_sub(2);
        let u_arr_ty = ptr_ty.array_type(n_uniforms.max(1) as u32);
        let uniform_ptrs = self.builder.build_alloca(u_arr_ty, "gpu.uniforms").unwrap();
        for (u, ua) in args.iter().skip(2).enumerate() {
            let v = self.compile_expr(&ua.value)?.into_float_value();
            let v = if v.get_type() == f32_t {
                v
            } else {
                self.builder
                    .build_float_trunc(v, f32_t, "gpu.u.f32")
                    .unwrap()
            };
            let slot = self.builder.build_alloca(f32_t, "gpu.u.slot").unwrap();
            self.builder.build_store(slot, v).unwrap();
            let arr_slot = unsafe {
                self.builder
                    .build_in_bounds_gep(
                        u_arr_ty,
                        uniform_ptrs,
                        &[i64_t.const_zero(), i64_t.const_int(u as u64, false)],
                        "gpu.u.k",
                    )
                    .unwrap()
            };
            self.builder.build_store(arr_slot, slot).unwrap();
        }
        let n_uniforms_v = i64_t.const_int(n_uniforms as u64, false);
        let uniform_size = i64_t.const_int(4, false);

        let dispatch_fn = self.gpu_dispatch_soa_fn();
        let aos_ptr = self
            .builder
            .build_call(
                dispatch_fn,
                &[
                    wgsl_ptr.into(),
                    wgsl_len.into(),
                    n_groups_v.into(),
                    in_ptrs.into(),
                    strides_arr.into(),
                    n_fields_v.into(),
                    fgroup_arr.into(),
                    fsrc_arr.into(),
                    fdst_arr.into(),
                    field_size.into(),
                    aos_stride.into(),
                    n.into(),
                    n_uniforms_v.into(),
                    uniform_ptrs.into(),
                    uniform_size.into(),
                ],
                "gpu.soa.out",
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();

        // Wrap the interleaved AoS buffer as an owned `Vec[S]` {ptr, len=n, cap=n}.
        let result_ty = self.vec_struct_type();
        let mut agg = result_ty.get_undef();
        agg = self
            .builder
            .build_insert_value(agg, aos_ptr, 0, "gpu.soa.res.data")
            .unwrap()
            .into_struct_value();
        agg = self
            .builder
            .build_insert_value(agg, n, 1, "gpu.soa.res.len")
            .unwrap()
            .into_struct_value();
        agg = self
            .builder
            .build_insert_value(agg, n, 2, "gpu.soa.res.cap")
            .unwrap()
            .into_struct_value();
        Ok(agg.into())
    }

    /// GPU-SLIP-4b-2b: lower a resident `gpu.dispatch(kernel, buf: GpuBuffer[S],
    /// uniforms…)`. The input stays on the device; emit the kernel shader
    /// (recovered from a `layout` block for the kernel's element struct `S` — the
    /// same grouping the buffer was uploaded with), pass the resident input handle
    /// to `karac_runtime_gpu_dispatch_resident`, and wrap the fresh output handle
    /// as a `GpuBuffer[S]` `{handle, n}` (same element count). The input handle is
    /// BORROWED (the runtime does not free it), so it survives for the next
    /// dispatch or a download.
    pub(super) fn compile_gpu_dispatch_resident(
        &mut self,
        args: &[CallArg],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let ExprKind::Identifier(kernel_name) = &args[0].value.kind else {
            return Err("gpu.dispatch kernel must be a bare `#[gpu]` function name".to_string());
        };
        let program = self
            .program_snapshot
            .clone()
            .ok_or("internal error: no program snapshot for gpu.dispatch")?;
        let kernel = program
            .items
            .iter()
            .find_map(|it| match it {
                crate::ast::Item::Function(f) if &f.name == kernel_name && f.is_gpu => Some(f),
                _ => None,
            })
            .ok_or_else(|| format!("internal error: gpu kernel `{kernel_name}` not found"))?;
        // Element struct `S` = the kernel's return type (a bare struct name).
        let struct_name = match kernel.return_type.as_ref().map(|t| &t.kind) {
            Some(crate::ast::TypeKind::Path(p)) if p.segments.len() == 1 => p.segments[0].clone(),
            _ => {
                return Err(format!(
                    "gpu.dispatch resident kernel `{kernel_name}` has no struct return type"
                ));
            }
        };
        // Recover the SoA group structure from a `layout` block for `S` (the same
        // grouping the buffer was uploaded with) — or, when the program declares
        // NO layout for `S`, the default single interleaved group (GPU-SLIP-4h;
        // upload used the same rule, so handle and manifest always agree).
        let soa = self
            .accel
            .soa_layouts
            .values()
            .find(|l| l.struct_name == struct_name)
            .cloned()
            .or_else(|| self.default_gpu_soa_layout(&struct_name))
            .ok_or_else(|| {
                format!("gpu.dispatch resident: no `layout` block found for `{struct_name}`")
            })?;
        if soa.cold_group.is_some() {
            return Err(
                "gpu.dispatch resident: a `cold` layout group is not supported".to_string(),
            );
        }

        let manifest: Vec<crate::gpu_wgsl::SoaGpuGroup> = soa
            .groups
            .iter()
            .map(|g| crate::gpu_wgsl::SoaGpuGroup {
                name: g.name.clone(),
                fields: g.fields.clone(),
            })
            .collect();
        let helpers: Vec<&crate::ast::Function> = program
            .items
            .iter()
            .filter_map(|it| match it {
                crate::ast::Item::Function(f) if f.is_gpu && &f.name != kernel_name => Some(f),
                _ => None,
            })
            .collect();
        let wgsl = crate::gpu_wgsl::emit_kernel_soa(kernel, &manifest, &helpers).map_err(|e| {
            format!(
                "gpu.dispatch resident: cannot lower `{kernel_name}` to a GPU shader — {}",
                e.reason()
            )
        })?;

        let i64_t = self.context.i64_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let wgsl_len = i64_t.const_int(wgsl.len() as u64, false);
        let wgsl_ptr = self
            .builder
            .build_global_string_ptr(&wgsl, "gpu.wgsl.res")
            .map_err(|e| format!("baking resident gpu.dispatch shader failed: {e}"))?
            .as_pointer_value();

        // Input handle + element count from the `GpuBuffer` arg `{handle, n}`.
        let buf_sv = self.compile_expr(&args[1].value)?.into_struct_value();
        let in_handle = self
            .builder
            .build_extract_value(buf_sv, 0, "gpu.res.in.handle")
            .unwrap()
            .into_int_value();
        let n = self
            .builder
            .build_extract_value(buf_sv, 1, "gpu.res.in.n")
            .unwrap()
            .into_int_value();

        // Uniforms (each f32), spilled to stack slots with a pointer array.
        let f32_t = self.context.f32_type();
        let n_uniforms = args.len().saturating_sub(2);
        let u_arr_ty = ptr_ty.array_type(n_uniforms.max(1) as u32);
        let uniform_ptrs = self
            .builder
            .build_alloca(u_arr_ty, "gpu.res.uniforms")
            .unwrap();
        for (u, ua) in args.iter().skip(2).enumerate() {
            let v = self.compile_expr(&ua.value)?.into_float_value();
            let v = if v.get_type() == f32_t {
                v
            } else {
                self.builder
                    .build_float_trunc(v, f32_t, "gpu.res.u.f32")
                    .unwrap()
            };
            let slot = self.builder.build_alloca(f32_t, "gpu.res.u.slot").unwrap();
            self.builder.build_store(slot, v).unwrap();
            let arr_slot = unsafe {
                self.builder
                    .build_in_bounds_gep(
                        u_arr_ty,
                        uniform_ptrs,
                        &[i64_t.const_zero(), i64_t.const_int(u as u64, false)],
                        "gpu.res.u.k",
                    )
                    .unwrap()
            };
            self.builder.build_store(arr_slot, slot).unwrap();
        }
        let n_uniforms_v = i64_t.const_int(n_uniforms as u64, false);
        let uniform_size = i64_t.const_int(4, false);

        let dispatch_fn = self.gpu_dispatch_resident_fn();
        let out_handle = self
            .builder
            .build_call(
                dispatch_fn,
                &[
                    wgsl_ptr.into(),
                    wgsl_len.into(),
                    in_handle.into(),
                    n_uniforms_v.into(),
                    uniform_ptrs.into(),
                    uniform_size.into(),
                ],
                "gpu.res.out",
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();

        // Wrap as `GpuBuffer[S]` `{out_handle, n}` (same element count).
        let buf_ty = self.gpu_buffer_type();
        let mut agg = buf_ty.get_undef();
        agg = self
            .builder
            .build_insert_value(agg, out_handle, 0, "gpu.res.buf.handle")
            .unwrap()
            .into_struct_value();
        agg = self
            .builder
            .build_insert_value(agg, n, 1, "gpu.res.buf.n")
            .unwrap()
            .into_struct_value();
        Ok(agg.into())
    }

    /// The LLVM representation of a `GpuBuffer[S]` value (GPU-SLIP-4b): the opaque
    /// resident handle plus the element count `{ i64 handle, i64 n }`. `n` is
    /// carried so `gpu.download` can build the host `Vec[S]` (length `n`) and the
    /// AoS→SoA scatter loop without a runtime round-trip to query it.
    pub(super) fn gpu_buffer_type(&self) -> inkwell::types::StructType<'ctx> {
        let i64_t = self.context.i64_type();
        self.context
            .struct_type(&[i64_t.into(), i64_t.into()], false)
    }

    /// Read a SoA `layout` binding's per-group data pointers + element count
    /// (spill + struct_gep — the multi-pointer SoA struct layout). Factored
    /// from `compile_gpu_upload` so the plain-`Vec[S]` arm (GPU-SLIP-4h) can
    /// share the upload tail.
    pub(super) fn read_soa_group_ptrs_len(
        &mut self,
        buf_expr: &Expr,
        soa: &crate::codegen::state::SoaLayout,
    ) -> Result<
        (
            crate::codegen::state::SoaLayout,
            Vec<inkwell::values::PointerValue<'ctx>>,
            inkwell::values::IntValue<'ctx>,
        ),
        String,
    > {
        let num_groups = soa.num_groups;
        let i64_t = self.context.i64_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let sv = self.compile_expr(buf_expr)?.into_struct_value();
        let vec_ty = sv.get_type();
        let spill = self.builder.build_alloca(vec_ty, "gpu.up.buf").unwrap();
        self.builder.build_store(spill, sv).unwrap();
        let mut group_ptrs = Vec::with_capacity(num_groups);
        for k in 0..num_groups {
            let gp_field = self
                .builder
                .build_struct_gep(vec_ty, spill, k as u32, "gpu.up.gp")
                .unwrap();
            let gp = self
                .builder
                .build_load(ptr_ty, gp_field, "gpu.up.g")
                .unwrap()
                .into_pointer_value();
            group_ptrs.push(gp);
        }
        let len_idx = Self::soa_len_index(num_groups, false);
        let len_field = self
            .builder
            .build_struct_gep(vec_ty, spill, len_idx, "gpu.up.len.p")
            .unwrap();
        let n = self
            .builder
            .build_load(i64_t, len_field, "gpu.up.n")
            .unwrap()
            .into_int_value();
        Ok((soa.clone(), group_ptrs, n))
    }

    /// GPU-SLIP-4h: synthesize the DEFAULT GPU layout for an un-layouted
    /// all-`f32` struct `S`: ONE interleaved group (`aos`) carrying every
    /// field in declaration order. This is the measured-fastest shape on the
    /// LBM harness (the 9-single-field-group SoA split cost 1.18× wall via
    /// 18-buffer binds + a 9-field download scatter; the interleaved form is
    /// an `array<S>`-shaped device buffer, so upload/download are verbatim
    /// copies and the bind group is minimal — hand-wgpu-equal access).
    ///
    /// Returns `None` when the program declares ANY `layout` block over `S`:
    /// the grouping is then the user's explicit choice, and an un-layouted
    /// binding stays an error — a handle uploaded under one grouping and
    /// dispatched under another would read garbage, so upload / dispatch /
    /// download must all resolve the SAME manifest, which this rule
    /// guarantees (all three fall back only when no `S` layout exists).
    pub(super) fn default_gpu_soa_layout(
        &self,
        struct_name: &str,
    ) -> Option<crate::codegen::state::SoaLayout> {
        if self
            .accel
            .soa_layouts
            .values()
            .any(|l| l.struct_name == struct_name)
        {
            return None;
        }
        let program = self.program_snapshot.clone()?;
        let sdef = program.items.iter().find_map(|it| match it {
            crate::ast::Item::StructDef(sd) if sd.name == struct_name => Some(sd),
            _ => None,
        })?;
        let fields: Vec<String> = sdef.fields.iter().map(|f| f.name.clone()).collect();
        if fields.is_empty() {
            return None;
        }
        let field_indices: Vec<usize> = (0..fields.len()).collect();
        Some(crate::codegen::state::SoaLayout {
            name: format!("__gpu_default_{struct_name}"),
            struct_name: struct_name.to_string(),
            groups: vec![crate::codegen::state::SoaGroup {
                name: "aos".to_string(),
                fields,
                field_indices,
                elem_type: None,
                align: None,
                is_cold: false,
            }],
            cold_group: None,
            num_groups: 1,
        })
    }

    /// GPU-SLIP-4b: lower `gpu.upload(vec)` — move a `Vec[S]` to a resident
    /// GPU buffer, returning a `GpuBuffer[S]` value `{handle, n}`. A SoA
    /// `layout` binding uploads one device buffer per group (the SoA dispatch
    /// path's group-pointer read); a PLAIN un-layouted `Vec[S]` (GPU-SLIP-4h)
    /// uploads its AoS data buffer as ONE interleaved group — stride =
    /// `n_fields × 4`, a verbatim copy. The runtime keeps the buffers
    /// resident and returns an opaque handle.
    pub(super) fn compile_gpu_upload(
        &mut self,
        args: &[CallArg],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let ExprKind::Identifier(vec_name) = &args[0].value.kind else {
            return Err("gpu.upload buffer must be a bare binding".to_string());
        };
        let (soa, group_ptrs, n) = if let Some(soa) = self.active_soa_layout(vec_name) {
            if soa.cold_group.is_some() {
                return Err(
                    "gpu.upload: a `cold` layout group is not supported (CG-4 Path A)".to_string(),
                );
            }
            if soa.num_groups == 0 {
                return Err("gpu.upload: the layout has no field groups".to_string());
            }
            self.read_soa_group_ptrs_len(&args[0].value, &soa)?
        } else {
            // Un-layouted `Vec[S]` (GPU-SLIP-4h): default interleaved group.
            let struct_name = self
                .var_types
                .var_elem_type_exprs
                .get(vec_name)
                .and_then(|te| match &te.kind {
                    crate::ast::TypeKind::Path(p) if p.segments.len() == 1 => {
                        Some(p.segments[0].clone())
                    }
                    _ => None,
                })
                .ok_or_else(|| {
                    format!("gpu.upload: `{vec_name}` has no registered struct element type")
                })?;
            let soa = self.default_gpu_soa_layout(&struct_name).ok_or_else(|| {
                format!(
                    "gpu.upload: `{vec_name}` is not bound to the `layout` block declared for \
                     `{struct_name}` — bind the layout variable, or remove the layout to use \
                     the default interleaved GPU buffer"
                )
            })?;
            let i64_t = self.context.i64_type();
            let ptr_ty = self.context.ptr_type(AddressSpace::default());
            let sv = self.compile_expr(&args[0].value)?.into_struct_value();
            let vec_ty = sv.get_type();
            let spill = self.builder.build_alloca(vec_ty, "gpu.up.buf").unwrap();
            self.builder.build_store(spill, sv).unwrap();
            let data_p = self
                .builder
                .build_struct_gep(vec_ty, spill, 0, "gpu.up.aos.data.p")
                .unwrap();
            let data = self
                .builder
                .build_load(ptr_ty, data_p, "gpu.up.aos.data")
                .unwrap()
                .into_pointer_value();
            let len_p = self
                .builder
                .build_struct_gep(vec_ty, spill, 1, "gpu.up.aos.len.p")
                .unwrap();
            let n = self
                .builder
                .build_load(i64_t, len_p, "gpu.up.aos.len")
                .unwrap()
                .into_int_value();
            (soa, vec![data], n)
        };
        let num_groups = soa.num_groups;
        let i64_t = self.context.i64_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let arr_ty = ptr_ty.array_type(num_groups as u32);
        let in_ptrs = self.builder.build_alloca(arr_ty, "gpu.up.in_ptrs").unwrap();
        for (k, gp) in group_ptrs.iter().enumerate() {
            let slot = unsafe {
                self.builder
                    .build_in_bounds_gep(
                        arr_ty,
                        in_ptrs,
                        &[i64_t.const_zero(), i64_t.const_int(k as u64, false)],
                        "gpu.up.in.k",
                    )
                    .unwrap()
            };
            self.builder.build_store(slot, *gp).unwrap();
        }
        let group_strides: Vec<u64> = soa
            .groups
            .iter()
            .map(|g| (g.fields.len() * 4) as u64)
            .collect();
        let strides_arr = self.build_i64_stack_array(&group_strides, "gpu.up.strides");
        let n_groups_v = i64_t.const_int(num_groups as u64, false);

        let upload_fn = self.gpu_upload_soa_fn();
        let handle = self
            .builder
            .build_call(
                upload_fn,
                &[
                    n_groups_v.into(),
                    in_ptrs.into(),
                    strides_arr.into(),
                    n.into(),
                ],
                "gpu.up.handle",
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();

        // Wrap as the `GpuBuffer[S]` value `{handle, n}`.
        let buf_ty = self.gpu_buffer_type();
        let mut agg = buf_ty.get_undef();
        agg = self
            .builder
            .build_insert_value(agg, handle, 0, "gpu.buf.handle")
            .unwrap()
            .into_struct_value();
        agg = self
            .builder
            .build_insert_value(agg, n, 1, "gpu.buf.n")
            .unwrap()
            .into_struct_value();
        Ok(agg.into())
    }

    /// GPU-SLIP-4b: lower a `gpu.download(buf)` that is NOT bound to a SoA
    /// `layout` variable. The AoS-target reconstruction needs the buffer's group
    /// structure, which the MVP recovers only from the receiving SoA binding, so
    /// the supported form is `let <soa> = gpu.download(buf)` — handled at the
    /// let-site by `compile_soa_let_from_gpu_download`. Any other position is an
    /// error until the general AoS-target path lands.
    pub(super) fn compile_gpu_download(
        &mut self,
        _args: &[CallArg],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        Err(
            "gpu.download result must bind directly to a SoA `layout` variable \
             (`let grid = gpu.download(buf)`) in this build"
                .to_string(),
        )
    }
}
