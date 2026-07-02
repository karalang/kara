//! Object method-call dispatch.
//!
//! Houses `compile_method_call` — the top-level dispatcher for
//! `object.method(args)` shapes. Recognises indexed-receiver,
//! field-receiver, entry-chain, and clone-on-collection shortcuts
//! before falling through to the impl-block lookup path. Also
//! handles primitive-type-receiver associated calls
//! (`i64.add(...)`) by delegating to `compile_assoc_call`, and the
//! receiver-form `cmp` (`lhs.cmp(rhs)` → Ordering tag synthesis).
//!
//! Lives in a sibling `impl<'ctx> super::Codegen<'ctx>` block.

use crate::ast::*;

use inkwell::types::{BasicMetadataTypeEnum, BasicTypeEnum};
use inkwell::values::{BasicMetadataValueEnum, BasicValue, BasicValueEnum};
use inkwell::AddressSpace;
use inkwell::AtomicOrdering;
use inkwell::AtomicRMWBinOp;
use inkwell::IntPredicate;

/// Natural alignment (bytes) for an Atomic primitive lowering. LLVM's
/// `load atomic` / `store atomic` require alignment ≥ the type's size
/// in bytes; the v1 Atomic codegen surface admits power-of-two-byte
/// integer widths (i8/i16/i32/i64/usize/i128) per the gate in
/// `compile_atomic_method`. Narrower / non-power-of-two widths (e.g.
/// `i1` from `Atomic[bool]`) are rejected at the dispatch site with a
/// clear diagnostic; the rounding-up branch here is defensive only.
fn atomic_alignment_for(ty: BasicTypeEnum<'_>) -> u32 {
    match ty {
        BasicTypeEnum::IntType(it) => {
            let bits = it.get_bit_width();
            bits.div_ceil(8).max(1)
        }
        _ => 8,
    }
}

impl<'ctx> super::Codegen<'ctx> {
    /// `char.try_from(n) -> Result[char, i64]` (#10). Widen the codepoint arg
    /// to i64 (sign- or zero-extend per the source's signedness, so a negative
    /// signed input stays negative and fails the lower bound), validate it is a
    /// Unicode scalar value (`0 <= cp <= 0x10FFFF` and NOT in the surrogate
    /// range `0xD800..=0xDFFF`), then branch: `Ok(char)` with the codepoint
    /// truncated to the i32 `char` repr, or `Err(cp)` carrying the offending
    /// value. PHI-merge the two `Result` aggregates. Mirrors the branch+phi
    /// shape of `Vec.try_from_slice`.
    fn compile_char_try_from(&mut self, args: &[CallArg]) -> Result<BasicValueEnum<'ctx>, String> {
        if args.len() != 1 {
            return Err(format!(
                "char.try_from expects 1 argument, got {}",
                args.len()
            ));
        }
        let i64_t = self.context.i64_type();
        let i32_t = self.context.i32_type();
        let raw = self.compile_expr(&args[0].value)?;
        let iv = match raw {
            BasicValueEnum::IntValue(iv) => iv,
            _ => return Err("char.try_from expects an integer argument".to_string()),
        };
        let src_unsigned = self.expr_is_unsigned_int(&args[0].value);
        let cp = if iv.get_type().get_bit_width() < 64 {
            if src_unsigned {
                self.builder
                    .build_int_z_extend(iv, i64_t, "ctf.zx")
                    .unwrap()
            } else {
                self.builder
                    .build_int_s_extend(iv, i64_t, "ctf.sx")
                    .unwrap()
            }
        } else {
            iv
        };
        let zero = i64_t.const_int(0, false);
        let max = i64_t.const_int(0x10FFFF, false);
        let sur_lo = i64_t.const_int(0xD800, false);
        let sur_hi = i64_t.const_int(0xDFFF, false);
        let ge0 = self
            .builder
            .build_int_compare(IntPredicate::SGE, cp, zero, "ctf.ge0")
            .unwrap();
        let le_max = self
            .builder
            .build_int_compare(IntPredicate::SLE, cp, max, "ctf.lemax")
            .unwrap();
        let in_range = self.builder.build_and(ge0, le_max, "ctf.inrange").unwrap();
        let ge_sur = self
            .builder
            .build_int_compare(IntPredicate::SGE, cp, sur_lo, "ctf.gesur")
            .unwrap();
        let le_sur = self
            .builder
            .build_int_compare(IntPredicate::SLE, cp, sur_hi, "ctf.lesur")
            .unwrap();
        let is_sur = self.builder.build_and(ge_sur, le_sur, "ctf.issur").unwrap();
        let not_sur = self.builder.build_not(is_sur, "ctf.notsur").unwrap();
        let valid = self
            .builder
            .build_and(in_range, not_sur, "ctf.valid")
            .unwrap();

        let cur_fn = self
            .builder
            .get_insert_block()
            .and_then(|bb| bb.get_parent())
            .ok_or("char.try_from outside a function context")?;
        let ok_bb = self.context.append_basic_block(cur_fn, "ctf.ok");
        let err_bb = self.context.append_basic_block(cur_fn, "ctf.err");
        let merge_bb = self.context.append_basic_block(cur_fn, "ctf.merge");
        self.builder
            .build_conditional_branch(valid, ok_bb, err_bb)
            .unwrap();

        self.builder.position_at_end(ok_bb);
        let ch = self
            .builder
            .build_int_truncate(cp, i32_t, "ctf.ch")
            .unwrap();
        let ok_result = self.build_nonshared_enum_value("Result", "Ok", &[ch.into()])?;
        let ok_end = self.builder.get_insert_block().unwrap();
        self.builder.build_unconditional_branch(merge_bb).unwrap();

        self.builder.position_at_end(err_bb);
        let err_result = self.build_nonshared_enum_value("Result", "Err", &[cp.into()])?;
        let err_end = self.builder.get_insert_block().unwrap();
        self.builder.build_unconditional_branch(merge_bb).unwrap();

        self.builder.position_at_end(merge_bb);
        let phi = self
            .builder
            .build_phi(ok_result.get_type(), "ctf.result")
            .unwrap();
        phi.add_incoming(&[(&ok_result, ok_end), (&err_result, err_end)]);
        Ok(phi.as_basic_value())
    }

    /// Coerce an integer value to `target` width: truncate when wider, zero- or
    /// sign-extend (per `unsigned`) when narrower, identity when equal.
    pub(super) fn coerce_int_to(
        &self,
        v: inkwell::values::IntValue<'ctx>,
        target: inkwell::types::IntType<'ctx>,
        unsigned: bool,
    ) -> inkwell::values::IntValue<'ctx> {
        let sw = v.get_type().get_bit_width();
        let tw = target.get_bit_width();
        if sw == tw {
            v
        } else if sw > tw {
            self.builder.build_int_truncate(v, target, "iw.tr").unwrap()
        } else if unsigned {
            self.builder.build_int_z_extend(v, target, "iw.zx").unwrap()
        } else {
            self.builder.build_int_s_extend(v, target, "iw.sx").unwrap()
        }
    }

    /// Recover the receiver's declared integer width + signedness for a
    /// width-dependent scalar method (`pow`, the bit intrinsics). Codegen widens
    /// narrow integers to i64 in value flow, so the LLVM value type is unreliable;
    /// the typechecker's `method_callee_types["<recv>.<method>"]` entry (keyed by
    /// the call/receiver span) carries the exact source type. When an OUTER chained
    /// call has clobbered that span's entry (its method segment no longer matches
    /// `method`), fall back to the receiver expression's declared type / literal
    /// suffix — matching the interpreter's non-aliased `args_close_span` recovery.
    /// Defaults to signed 64-bit (the language's default integer).
    fn receiver_int_kind(
        &self,
        object: &Expr,
        call_span: &crate::token::Span,
        method: &str,
    ) -> (u32, bool) {
        fn parse(name: &str) -> Option<(u32, bool)> {
            Some(match name {
                "i8" => (8, false),
                "i16" => (16, false),
                "i32" => (32, false),
                "i64" | "isize" => (64, false),
                "u8" => (8, true),
                "u16" => (16, true),
                "u32" => (32, true),
                "u64" | "usize" => (64, true),
                _ => return None,
            })
        }
        if let Some(callee) = self
            .method_callee_types
            .get(&(call_span.offset, call_span.length))
        {
            if let Some((recv, m)) = callee.split_once('.') {
                if m == method {
                    if let Some(k) = parse(recv) {
                        return k;
                    }
                }
            }
        }
        if let Some(name) = self.type_name_of_expr(object) {
            if let Some(k) = parse(&name) {
                return k;
            }
        }
        if let ExprKind::Integer(_, Some(suf)) = &object.kind {
            use crate::token::IntSuffix::*;
            return match suf {
                I8 => (8, false),
                I16 => (16, false),
                I32 => (32, false),
                I64 | I128 => (64, false),
                U8 => (8, true),
                U16 => (16, true),
                U32 => (32, true),
                U64 | U128 => (64, true),
            };
        }
        (64, false)
    }

    pub(super) fn compile_method_call(
        &mut self,
        object: &Expr,
        method: &str,
        args: &[CallArg],
        call_span: &crate::token::Span,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        // Cooperative cancel check before each call inside a par-branch.
        // The receiver's `Type.method` key is precomputed by lowering and
        // stored in `method_callee_types`; consult it so a provably pure
        // method elides the check, mirroring the narrowing applied to
        // free-function calls in `compile_call`.
        let callee_key = self
            .method_callee_types
            .get(&(call_span.offset, call_span.length))
            .cloned();
        self.emit_branch_cancel_check("mcall", callee_key.as_deref());

        // `<string>.chars()` as a STANDALONE value (e.g. `let it = s.chars()`).
        // Codegen has no first-class iterator value, so materialize the eager
        // `Vec[char]` snapshot — the faithful representation of a char-iterator
        // — by reusing the `.chars().collect()` lowering (`for c in s.chars() {
        // v.push(c) }`). This fires ONLY when `chars()` is compiled as a value:
        // `for c in s.chars()` is special-cased in the for-loop codegen (the
        // iterable never reaches here), and `s.chars().collect()` is caught by
        // the chain intercept below (its inner `chars()` is never compiled
        // standalone). The let-binding handler registers the binding as
        // `Vec[char]` so `it.collect()` / `for c in it` dispatch as Vec ops
        // (B-2026-06-18-5). `chars()` exists only on `String`, so the method
        // name alone identifies the shape.
        if method == "chars" && args.is_empty() {
            let chars_call = Expr {
                kind: ExprKind::MethodCall {
                    object: Box::new(object.clone()),
                    method: "chars".to_string(),
                    turbofish: None,
                    args: vec![],
                    args_close_span: call_span.clone(),
                },
                span: call_span.clone(),
            };
            return self.compile_chars_collect_to_vec(&chars_call, call_span);
        }

        // `<it>.collect()` where `it` is an identifier the codegen materialized
        // as a `Vec` (a bound `s.chars()`, B-2026-06-18-5). The eager snapshot
        // already IS the collected Vec, so return an independent copy (collect
        // yields a fresh owned Vec). `collect()` only typechecks on an
        // `Iterator`, so a Vec-typed receiver here is always such a materialized
        // iterator — never a user Vec. Placed before the identifier
        // → `compile_vec_method` dispatch, which has no `collect` arm. (The
        // `s.chars().collect()` chain, whose `collect` receiver is a `MethodCall`
        // not an identifier, is handled by the chain intercept further below.)
        if method == "collect" && args.is_empty() {
            if let ExprKind::Identifier(name) = &object.kind {
                if self.vec_elem_types.contains_key(name.as_str()) {
                    if let Some(v) = self.try_compile_clone(object)? {
                        return Ok(v);
                    }
                }
            }
        }

        // `process.exit(code: i32) -> !` — lower to libc `exit`. The typechecker
        // registers `process.exit` as a dotted free function and the interpreter
        // (eval_call.rs) handles it as a path-call, but the parser hands codegen a
        // method call with `process` as a (pseudo-variable) identifier receiver.
        // Match the interpreter's semantics: evaluate the code as i32, call libc
        // `exit` (declared `void exit(i32)` in `Codegen::new`), and terminate the
        // block with `unreachable` — the call is `Never`, so no value flows out.
        // Gated on `process` not being a real local (mirrors the ambient-resource
        // guard below) so a user binding named `process` is never hijacked.
        if method == "exit" {
            if let ExprKind::Identifier(name) = &object.kind {
                if name == "process" && !self.variables.contains_key("process") {
                    let i32_ty = self.context.i32_type();
                    // Default code is 0 (matches the interpreter's no-arg path).
                    let code = match args.first() {
                        Some(arg) => {
                            let iv = self.compile_expr(&arg.value)?.into_int_value();
                            let w = iv.get_type().get_bit_width();
                            match w.cmp(&32) {
                                std::cmp::Ordering::Greater => self
                                    .builder
                                    .build_int_truncate(iv, i32_ty, "exit.code.tr")
                                    .unwrap(),
                                std::cmp::Ordering::Less => self
                                    .builder
                                    .build_int_s_extend(iv, i32_ty, "exit.code.sx")
                                    .unwrap(),
                                std::cmp::Ordering::Equal => iv,
                            }
                        }
                        None => i32_ty.const_int(0, false),
                    };
                    let exit_fn = self
                        .module
                        .get_function("exit")
                        .expect("libc `exit` extern declared in Codegen::new");
                    self.builder
                        .build_call(exit_fn, &[code.into()], "process_exit")
                        .unwrap();
                    self.builder.build_unreachable().unwrap();
                    // Block is terminated; this placeholder is never read (every
                    // value-consuming caller respects the terminator guard).
                    return Ok(self.context.i64_type().const_int(0, false).into());
                }
            }
        }

        // Fallible-allocation instance companions (phase-8-stdlib-floor item 8).
        // Companions whose codegen lowering has landed
        // (`CODEGEN_FALLIBLE_INSTANCE_BASES`, e.g. `try_push`) fall through to
        // their dispatcher (`compile_vec_method`) and emit real fallible
        // allocation + `Result`. The remaining companions are still
        // interpreter-only; reject those at `karac build` with a clear message
        // when the receiver is a builtin collection. Gated on the collection
        // side-tables so a user type's own `try_*` method (which dispatches
        // through the qualified user-method path below) is never blocked.
        if let Some(base) = crate::fallible_alloc::instance_companion_base(method) {
            if !crate::fallible_alloc::instance_companion_has_codegen(method) {
                if let ExprKind::Identifier(name) = &object.kind {
                    let n = name.as_str();
                    let is_builtin_coll = self.vec_elem_types.contains_key(n)
                        || self.map_key_types.contains_key(n)
                        || self.set_elem_types.contains_key(n)
                        || self
                            .var_type_names
                            .get(n)
                            .is_some_and(|t| t == "String" || t.starts_with("String"));
                    if is_builtin_coll {
                        return Err(format!(
                            "codegen: fallible-allocation companion `.{method}(...)` is \
                             interpreter-only in v1; its codegen lowering is phase-8-stdlib-floor \
                             item 8. Run under `karac run`, or use the panicking `.{base}(...)` \
                             base method under `karac build`."
                        ));
                    }
                }
            }
        }

        // Borrow-returning method call used outside a `let x = recv.m()`
        // binding: the result is a `ptr` (the borrow's address); any other
        // context would mishandle it as a value. The let arm sets
        // `compiling_ref_return_let_rhs` for the sanctioned site; reject
        // elsewhere rather than miscompile (sibling of the free-fn gate in
        // `compile_call`). The MethodCall expr shares the receiver's span,
        // which is the key the lowering pass used for the call's result
        // type. Direct use is a tracked follow-on (B-2026-06-07-5).
        if !self.compiling_ref_return_let_rhs
            && self.user_ref_method_names.contains(method)
            && self
                .ref_return_inner_types
                .contains_key(&(object.span.offset, object.span.length))
        {
            return Err(format!(
                "borrow-returning method call `.{method}(...)` must be bound directly with \
                 `let x = ...{method}(...)` before use; direct use of a `-> ref T` result \
                 is not yet supported (B-2026-06-07-5)"
            ));
        }

        // Chained-call span collision guard. The parser sets
        // `MethodCall.span == receiver.span`, so in `recv.inner().outer()`
        // the inner and outer calls share one `method_callee_types` key, and
        // it resolves to the *inner* call's `Type.method` (the effect-checker
        // relies on that — see the unwrap-family skip in
        // `typechecker/expr_method_call.rs`). For DISPATCH below we must not
        // let the inner key drive the outer call: e.g. compiling the `unwrap`
        // of `listener.accept().unwrap()` sees `key == "TcpListener.accept"`
        // and would re-lower `accept` on its own result (a double-lowering +
        // type mismatch). Require the key's method segment to match THIS
        // call's `method` before using it to pick a builtin / state-machine
        // lowering; the conservative cancel-check above keeps the raw key.
        let dispatch_key = callee_key
            .as_ref()
            .filter(|k| {
                k.rsplit_once('.')
                    .map(|(_, m)| m == method)
                    .unwrap_or(false)
            })
            .cloned();

        // Distinct-type `.raw()` unwrap (design.md § Distinct Types). A
        // distinct type is a zero-cost wrapper — its compiled value already
        // IS the base value (layout-identical), so `.raw()` returns the
        // compiled receiver unchanged. `.raw()` is reserved to distinct types
        // by the typechecker, so a zero-arg `.raw()` reaching codegen is
        // always this unwrap.
        if method == "raw" && args.is_empty() {
            return self.compile_expr(object);
        }

        // Tensor shape-transform family (`reshape` / `permute` / `slice`
        // / `squeeze`, phase-11 numerical stdlib — `src/codegen/tensor.rs`).
        // Handled here (before the rest of dispatch) so both identifier
        // and chained / value receivers route uniformly; returns `None`
        // when the method isn't a transform or the receiver isn't a
        // statically-ranked tensor. `iter_axis` is a separate follow-on
        // slice and is NOT handled here (it errors in the identifier
        // block below).
        if let Some(v) = self.try_compile_tensor_transform(object, method, args, call_span)? {
            return Ok(v);
        }

        // Column instance methods (`push` / `push_null` / `len` /
        // `null_count` / `valid_count` / `is_null`, phase-11 data-science
        // stdlib — `src/codegen/column.rs`). Identifier receiver only
        // (gated on `column_var_infos`, span-collision-immune); returns
        // `None` when the receiver isn't a column or the method isn't one
        // of ours. The Vec-returning transforms (`iter` / `iter_valid` /
        // `fillna` / `dropna`) are a follow-on slice and stay on
        // `karac run`.
        if let Some(v) = self.try_compile_column_method(object, method, args, call_span)? {
            return Ok(v);
        }
        // DataFrame methods (`insert` / `column` / `has_column` / `width`
        // / `height`) — gated on `dataframe_var_infos` (identifier
        // receiver). `None` for a non-DataFrame receiver. See
        // `src/codegen/dataframe.rs`.
        if let Some(v) = self.try_compile_dataframe_method(object, method, args, call_span)? {
            return Ok(v);
        }

        // Tensor reductions — `sum`/`mean`/`prod`/`min`/`max` (→ scalar) and
        // `sum_axis`/`mean_axis` (→ rank-1-lower tensor), phase-11 line 47
        // Slice B. Handled here so identifier / chained / value receivers
        // route uniformly; `None` when the method isn't a reduce or the
        // receiver isn't a tensor.
        if let Some(v) = self.try_compile_tensor_reduce(object, method, args, call_span)? {
            return Ok(v);
        }

        // Tensor broadcasting — `broadcast_add`/`broadcast_sub`/`broadcast_mul`
        // /`broadcast_div` apply an element-wise op with NumPy-style shape
        // broadcasting (size-1 dims expand; shapes align from the right).
        // Identifier receiver only (like reductions; span-collision-immune);
        // `None` for a value / chained receiver (bind to a `let` first) or a
        // non-tensor receiver. `src/codegen/tensor.rs`.
        if let Some(v) = self.try_compile_tensor_broadcast(object, method, args, call_span)? {
            return Ok(v);
        }

        // SIMD static constructor — `Vector[T, N].splat(x)` (design.md
        // § Portable SIMD). The receiver is the bare vector type-path, not a
        // value, so intercept before the receiver is compiled as an
        // expression. Broadcast the scalar across all `N` lanes.
        if method == "splat"
            || method == "from_array"
            || method == "from_slice"
            || method == "load_masked"
            || method == "gather"
            || method == "cast_from"
        {
            if let ExprKind::Path {
                segments,
                generic_args: Some(ga),
            } = &object.kind
            {
                if segments.len() == 1 && segments[0] == "Vector" {
                    return match method {
                        "splat" => self.compile_vector_splat(ga, args),
                        "from_array" => self.compile_vector_from_array(ga, args),
                        "load_masked" => self.compile_vector_load_masked(ga, args),
                        "gather" => self.compile_vector_gather(ga, args),
                        "cast_from" => self.compile_vector_cast_from(ga, args),
                        _ => self.compile_vector_from_slice(ga, args),
                    };
                }
            }
        }

        // `Vector[T, N]` instance methods (design.md § Portable SIMD, slice 2):
        // the two core Vector→scalar reductions. The receiver compiles to an
        // `<N x T>` VectorValue; reductions fold via extractelement + scalar
        // binop (LLVM re-vectorizes where profitable). dispatch_key is
        // `"Vector.<method>"` from `method_callee_type_name`.
        if let Some(ref key) = dispatch_key {
            if matches!(
                key.as_str(),
                "Vector.dot"
                    | "Vector.cross"
                    | "Vector.reduce_sum"
                    | "Vector.reduce_product"
                    | "Vector.reduce_min"
                    | "Vector.reduce_max"
                    | "Vector.reduce_and"
                    | "Vector.reduce_or"
                    | "Vector.reduce_xor"
                    | "Vector.select"
                    | "Vector.reverse"
                    | "Vector.rotate_lanes_left"
                    | "Vector.rotate_lanes_right"
                    | "Vector.replace"
                    | "Vector.shuffle"
                    | "Vector.store_masked"
                    | "Vector.scatter"
            ) {
                return self.compile_vector_method(object, method, args);
            }
        }

        // `CStr` method dispatch (design.md § C-String Literals). The
        // receiver compiles to the `{ptr, i64}` slice-struct the
        // CStringLit lowering produces (see `compile_expr`); every method
        // is an extract/compare on that aggregate, so one helper serves
        // literal, local-binding, and call-result receivers alike. Keyed
        // off the typechecker-recorded `CStr.<method>` (the same pattern
        // as the Vector arm above) — `cstr_vars` exists for *binding*
        // registration heuristics, not dispatch.
        if let Some(ref key) = dispatch_key {
            if matches!(
                key.as_str(),
                "CStr.as_ptr" | "CStr.len" | "CStr.is_empty" | "CStr.as_bytes"
            ) {
                return self.compile_cstr_method(object, method);
            }
            // `CStr.to_string() -> Result[String, Utf8Error]` — the UTF-8-
            // validating read of a C string (FFI/host-fn `char*` boundary).
            // Unlike the borrowed-surface methods above, it allocates a heap
            // String and builds a Result enum, so it has its own helper.
            if key.as_str() == "CStr.to_string" {
                return self.compile_cstr_to_string(object);
            }
        }

        // Phase 6 line 17 — stdlib `TcpListener` / `TcpStream`
        // compiler-builtin dispatch. Routes through the lowerings in
        // `src/codegen/tcp.rs`, each of which composes a
        // `karac_park_on_fd(self.fd, direction)` state-machine
        // invocation with a raw-syscall FFI call. Runs ahead of the
        // state-machine intercept below so the compiler-builtin shape
        // takes precedence over the generic network-boundary lowering
        // (the baked stdlib's bodies are stubs — without these arms,
        // the generic dispatch would emit a call into a non-existent
        // symbol).
        if let Some(ref key) = dispatch_key {
            if key == "TcpListener.accept" {
                let self_val = self.compile_expr(object)?;
                return self.lower_tcp_listener_accept(self_val);
            }
            // Phase 8 `File` handle slice F4: instance method
            // dispatch. `file.read(buf: mut Slice[u8])` /
            // `file.write(buf: Slice[u8])` / `file.flush()` lower
            // through `karac_runtime_file_*` externs; the
            // KaracIoResult return unpacks into `Result[usize/Unit,
            // IoError]` via `Codegen::lower_kara_io_result`. The
            // receiver `self_val` is the `File` opaque pointer (per
            // F3's `File` → opaque ptr lowering).
            if key == "File.read" && args.len() == 1 {
                let self_val = self.compile_expr(object)?;
                let elem_ty: BasicTypeEnum = self.context.i8_type().into();
                let buf_val = match self.coerce_to_slice(&args[0].value, elem_ty)? {
                    Some(v) => v,
                    None => self.compile_expr(&args[0].value)?,
                };
                return self.compile_file_read(self_val, buf_val);
            }
            if key == "File.write" && args.len() == 1 {
                let self_val = self.compile_expr(object)?;
                let elem_ty: BasicTypeEnum = self.context.i8_type().into();
                let buf_val = match self.coerce_to_slice(&args[0].value, elem_ty)? {
                    Some(v) => v,
                    None => self.compile_expr(&args[0].value)?,
                };
                return self.compile_file_write(self_val, buf_val);
            }
            if key == "File.flush" && args.is_empty() {
                let self_val = self.compile_expr(object)?;
                return self.compile_file_flush(self_val);
            }
            if key == "TcpStream.read" && args.len() == 1 {
                let self_val = self.compile_expr(object)?;
                let elem_ty: BasicTypeEnum = self.context.i8_type().into();
                let buf_val = match self.coerce_to_slice(&args[0].value, elem_ty)? {
                    Some(v) => v,
                    None => self.compile_expr(&args[0].value)?,
                };
                return self.lower_tcp_stream_read(self_val, buf_val);
            }
            if key == "TcpStream.write" && args.len() == 1 {
                let self_val = self.compile_expr(object)?;
                let elem_ty: BasicTypeEnum = self.context.i8_type().into();
                let buf_val = match self.coerce_to_slice(&args[0].value, elem_ty)? {
                    Some(v) => v,
                    None => self.compile_expr(&args[0].value)?,
                };
                return self.lower_tcp_stream_write(self_val, buf_val);
            }
            if key == "TcpStream.write_all" && args.len() == 1 {
                let self_val = self.compile_expr(object)?;
                let elem_ty: BasicTypeEnum = self.context.i8_type().into();
                let buf_val = match self.coerce_to_slice(&args[0].value, elem_ty)? {
                    Some(v) => v,
                    None => self.compile_expr(&args[0].value)?,
                };
                return self.lower_tcp_stream_write_all(self_val, buf_val);
            }
            if key == "TcpStream.try_clone" && args.is_empty() {
                // `dup(2)` the socket into a second owned handle — splits a
                // connection into read-half + write-half for a full-duplex
                // splice. Dispatched here (before the generic Vec/String
                // `try_clone` deep-copy arm) so TcpStream gets the fd-dup
                // lowering, not the buffer-clone one.
                let self_val = self.compile_expr(object)?;
                return self.lower_tcp_stream_try_clone(self_val);
            }
            if key == "TcpStream.shutdown_write" && args.is_empty() {
                // Half-close the write side (`shutdown(SHUT_WR)`) — sends a
                // FIN so a proxy can propagate one direction's EOF across a
                // full-duplex splice.
                let self_val = self.compile_expr(object)?;
                return self.lower_tcp_stream_shutdown_write(self_val);
            }
            // Phase 6 line 236 slice 2 — TLS-side method dispatch. Same
            // shape as the TCP dispatch above; lowerings in
            // `src/codegen/tls.rs` route through `karac_runtime_tls_*`.
            if key == "TlsListener.accept" {
                let self_val = self.compile_expr(object)?;
                return self.lower_tls_listener_accept(self_val);
            }
            if key == "TlsStream.read" && args.len() == 1 {
                let self_val = self.compile_expr(object)?;
                let elem_ty: BasicTypeEnum = self.context.i8_type().into();
                let buf_val = match self.coerce_to_slice(&args[0].value, elem_ty)? {
                    Some(v) => v,
                    None => self.compile_expr(&args[0].value)?,
                };
                return self.lower_tls_stream_read(self_val, buf_val);
            }
            if key == "TlsStream.write" && args.len() == 1 {
                let self_val = self.compile_expr(object)?;
                let elem_ty: BasicTypeEnum = self.context.i8_type().into();
                let buf_val = match self.coerce_to_slice(&args[0].value, elem_ty)? {
                    Some(v) => v,
                    None => self.compile_expr(&args[0].value)?,
                };
                return self.lower_tls_stream_write(self_val, buf_val);
            }
            if key == "TlsStream.write_all" && args.len() == 1 {
                let self_val = self.compile_expr(object)?;
                let elem_ty: BasicTypeEnum = self.context.i8_type().into();
                let buf_val = match self.coerce_to_slice(&args[0].value, elem_ty)? {
                    Some(v) => v,
                    None => self.compile_expr(&args[0].value)?,
                };
                return self.lower_tls_stream_write_all(self_val, buf_val);
            }
            // Phase 6 line 17 slice 9e.1 — stdlib `WebSocket` dispatch.
            // Same compose-at-leaf shape as TcpStream above:
            // `karac_park_on_fd(self.fd, direction)` then the encode +
            // write or read + decode FFI. The runtime FFIs
            // (`karac_runtime_ws_send_text` / `_recv_text`) handle the
            // RFC 6455 framing details.
            if key == "WebSocket.send_text" && args.len() == 1 {
                let self_val = self.compile_expr(object)?;
                let elem_ty: BasicTypeEnum = self.context.i8_type().into();
                let buf_val = match self.coerce_to_slice(&args[0].value, elem_ty)? {
                    Some(v) => v,
                    None => self.compile_expr(&args[0].value)?,
                };
                return self.lower_websocket_send_text(self_val, buf_val);
            }
            if key == "WebSocket.recv_text" && args.len() == 1 {
                let self_val = self.compile_expr(object)?;
                let elem_ty: BasicTypeEnum = self.context.i8_type().into();
                let buf_val = match self.coerce_to_slice(&args[0].value, elem_ty)? {
                    Some(v) => v,
                    None => self.compile_expr(&args[0].value)?,
                };
                return self.lower_websocket_recv_text(self_val, buf_val);
            }
            // Phase 6 line 17 slice 9e.3 — binary frame send/recv.
            // Mirror of send_text / recv_text but routes through
            // the binary-opcode FFIs.
            if key == "WebSocket.send_binary" && args.len() == 1 {
                let self_val = self.compile_expr(object)?;
                let elem_ty: BasicTypeEnum = self.context.i8_type().into();
                let buf_val = match self.coerce_to_slice(&args[0].value, elem_ty)? {
                    Some(v) => v,
                    None => self.compile_expr(&args[0].value)?,
                };
                return self.lower_websocket_send_binary(self_val, buf_val);
            }
            if key == "WebSocket.recv_binary" && args.len() == 1 {
                let self_val = self.compile_expr(object)?;
                let elem_ty: BasicTypeEnum = self.context.i8_type().into();
                let buf_val = match self.coerce_to_slice(&args[0].value, elem_ty)? {
                    Some(v) => v,
                    None => self.compile_expr(&args[0].value)?,
                };
                return self.lower_websocket_recv_binary(self_val, buf_val);
            }
            // Phase 6 line 17 slice 9e.4 — client-side masked send
            // for kara binaries acting as WebSocket clients
            // (RFC 6455 §5.1 client→server frames require MASK=1).
            if key == "WebSocket.send_text_masked" && args.len() == 1 {
                let self_val = self.compile_expr(object)?;
                let elem_ty: BasicTypeEnum = self.context.i8_type().into();
                let buf_val = match self.coerce_to_slice(&args[0].value, elem_ty)? {
                    Some(v) => v,
                    None => self.compile_expr(&args[0].value)?,
                };
                return self.lower_websocket_send_text_masked(self_val, buf_val);
            }
            if key == "WebSocket.send_binary_masked" && args.len() == 1 {
                let self_val = self.compile_expr(object)?;
                let elem_ty: BasicTypeEnum = self.context.i8_type().into();
                let buf_val = match self.coerce_to_slice(&args[0].value, elem_ty)? {
                    Some(v) => v,
                    None => self.compile_expr(&args[0].value)?,
                };
                return self.lower_websocket_send_binary_masked(self_val, buf_val);
            }
            // Phase 6 line 218 slice 5: `tg.spawn(closure)` — synthesize
            // the SpawnFn wrapper + malloc/populate env + call
            // karac_runtime_spawn (same path as free `spawn`), then
            // register the returned handle with the TaskGroup so the
            // group's drop can wait for the child. The receiver carries
            // the runtime-side group pointer in its `i64 id` field
            // (`TaskGroup.new()` lowers to ptrtoint of a Box<KaracTaskGroupHandle>).
            if key == "TaskGroup.spawn" && args.len() == 1 {
                let self_val = self.compile_expr(object)?;
                return self.lower_taskgroup_spawn(self_val, &args[0].value);
            }
            // A2 slice 5b-1: `tg.cancel()` — flip every registered child's
            // per-task cancel flag via karac_runtime_taskgroup_cancel. Inert
            // until the dispatcher routes the flag to parked coroutines
            // (slice 5c). Returns unit.
            if key == "TaskGroup.cancel" && args.is_empty() {
                let self_val = self.compile_expr(object)?;
                return self.lower_taskgroup_cancel(self_val);
            }
            // Phase 6 line 218 slice 4: `h.join()` dispatch. Lowers to
            // `karac_runtime_task_join(handle, &out_slot)` then reads
            // T from the slot. The return type T is recovered from the
            // enclosing function's `let v: T = h.join()` annotation
            // (typechecker doesn't bind T from receiver for the
            // `impl[T] T<T> { fn m(self) -> T }` shape today — see slice
            // 1's surfaced typechecker gap). Falls back to i64 when no
            // annotation is recoverable.
            if key == "TaskHandle.join" && args.is_empty() {
                let self_val = self.compile_expr(object)?;
                let return_ty = self.recover_task_handle_join_return_ty(call_span);
                return self.lower_task_handle_join(self_val, return_ty);
            }
            // `BoundedChannel.send` / `.recv` (`src/codegen/bounded_channel.rs`).
            // Routed here off the `dispatch_key` the typechecker's
            // `infer_bounded_channel_method` records — ahead of the unbounded
            // `channel_elem_types` gate below, so a bounded `recv` (whose `T`
            // also lives in `channel_elem_types`) is never misrouted to the
            // unbounded `*mut KaracChannel` lowering.
            if key == "BoundedChannel.send" && args.len() == 1 {
                return self.compile_bounded_channel_send(object, args);
            }
            if key == "BoundedChannel.recv" && args.is_empty() {
                return self.compile_bounded_channel_recv(object, call_span);
            }
        }

        // Phase 6 line 26 slice 8g: method-call network-boundary intercept.
        // Mirrors slice 8d's free-function intercept (`compile_call`) for
        // `obj.method(args)` shapes where the resolved `Type.method` key
        // is in `state_machine_state_constructors`. The receiver `obj`
        // becomes `self` and stores into state struct field 1 (slice 4's
        // layout puts `self` at position 0). Method args follow at
        // fields 2..K. Runs ahead of every other method-call dispatch
        // path so the intercept fires before any receiver-shape
        // shortcuts (Option/Result, indexed-receiver, field-receiver,
        // entry-chain, clone-on-collection) — for a network-boundary
        // method those shortcuts would emit an inappropriate direct
        // call. Receiver compilation routes through the standard
        // `compile_expr` path, matching slice 8f's arg-store handling.
        if let Some(ref key) = dispatch_key {
            // A2 slice 2b.4(b): coroutine-compiled method handler. Same
            // dispatcher-driven slot-wait drive as the free-fn intercept
            // (call_dispatch.rs), but the receiver `object` is the ramp's first
            // arg (self at param index 0), method args follow at 1..K, and the
            // hidden completion slot is last. The caller never resumes — the
            // dispatcher drives via the unchanged 2b.1 shim. Runs ahead of the
            // degenerate poll-loop intercept below so a coro method key takes the
            // coroutine path.
            if self.is_coroutine_compiled(key) {
                let ramp = self
                    .module
                    .get_function(key)
                    .expect("coroutine method ramp declared in declare_function");
                let ref_flags = self.fn_param_ref.get(key).cloned().unwrap_or_default();
                let slice_elems = self
                    .fn_param_slice_elem
                    .get(key)
                    .cloned()
                    .unwrap_or_default();
                let mut call_args: Vec<inkwell::values::BasicMetadataValueEnum<'ctx>> =
                    Vec::with_capacity(args.len() + 2);
                // self (param index 0), dispatched by its declared mode.
                let self_is_ref = ref_flags.first().copied().unwrap_or(false);
                let self_val: BasicValueEnum<'ctx> = if self_is_ref {
                    if let ExprKind::Identifier(var_name) = &object.kind {
                        if let Some(ptr) = self.get_data_ptr(var_name) {
                            ptr.into()
                        } else {
                            let v = self.compile_expr(object)?;
                            self.materialize_rvalue_for_ref_arg(v, usize::MAX)
                        }
                    } else {
                        let v = self.compile_expr(object)?;
                        self.materialize_rvalue_for_ref_arg(v, usize::MAX)
                    }
                } else {
                    // Owned receiver moved into the coroutine method — the
                    // coroutine owns + drops it at completion, so suppress the
                    // caller's drop (mirrors the free-fn coroutine arg path in
                    // `call_dispatch`). No-op for non-`UserDrop` receivers; the
                    // channel-end sibling suppresses an early `DropChannelEnd`
                    // close on a moved `Sender`/`Receiver` receiver.
                    if let ExprKind::Identifier(var_name) = &object.kind {
                        self.suppress_user_drop_for_var(var_name);
                        self.suppress_channel_drop_for_var(var_name);
                    }
                    self.compile_expr(object)?
                };
                call_args.push(self_val.into());
                // Method args at param indices 1..K.
                for (i, arg) in args.iter().enumerate() {
                    let param_idx = i + 1;
                    let is_ref = ref_flags.get(param_idx).copied().unwrap_or(false);
                    let slice_elem = slice_elems.get(param_idx).copied().flatten();
                    let val: BasicValueEnum<'ctx> = if is_ref {
                        if let ExprKind::Identifier(var_name) = &arg.value.kind {
                            if let Some(ptr) = self.get_data_ptr(var_name) {
                                ptr.into()
                            } else {
                                let v = self.compile_expr(&arg.value)?;
                                self.materialize_rvalue_for_ref_arg(v, i)
                            }
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
                        // Owned method arg moved into the coroutine — suppress the
                        // caller's drop (see the receiver case above), including
                        // an early channel-end close on a moved `Sender`/
                        // `Receiver`.
                        if let ExprKind::Identifier(var_name) = &arg.value.kind {
                            self.suppress_user_drop_for_var(var_name);
                            self.suppress_channel_drop_for_var(var_name);
                        }
                        self.compile_expr(&arg.value)?
                    };
                    call_args.push(val.into());
                }
                // Hidden trailing completion slot. A2 slice 5a — inside a
                // `__spawn_coro_wrap` body (`self.coro_spawn_slot` is `Some`)
                // the runtime owns the slot and binds it to the `TaskHandle`;
                // we ramp and return (worker freed). Otherwise the caller owns
                // it: allocate, ramp, block, free (the inline drive).
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
                call_args.push(slot.into());
                self.builder
                    .build_call(ramp, &call_args, "kara.coro.drive")
                    .expect("call coroutine method ramp");
                if spawn_slot.is_none() {
                    let wait_fn = self
                        .module
                        .get_function("karac_runtime_park_slot_wait")
                        .expect("karac_runtime_park_slot_wait declared in Codegen::new");
                    self.builder
                        .build_call(wait_fn, &[slot.into()], "")
                        .expect("call karac_runtime_park_slot_wait");
                    let free_fn = self
                        .module
                        .get_function("karac_runtime_park_slot_free")
                        .expect("karac_runtime_park_slot_free declared in Codegen::new");
                    self.builder
                        .build_call(free_fn, &[slot.into()], "")
                        .expect("call karac_runtime_park_slot_free");
                }
                return Ok(self.context.i64_type().const_int(0, false).into());
            }
            if let Some(ctor_fn) = self.state_machine_state_constructors.get(key).copied() {
                let poll_fn = self
                    .state_machine_poll_fns
                    .get(key)
                    .copied()
                    .expect("poll-fn co-emitted with state-machine constructor");
                let state_struct = self
                    .state_struct_types
                    .get(key)
                    .copied()
                    .expect("state struct type co-emitted with constructor");
                let ptr_ty = self.context.ptr_type(AddressSpace::default());
                let i8_ty = self.context.i8_type();
                let cur_fn = self
                    .builder
                    .get_insert_block()
                    .and_then(|bb| bb.get_parent())
                    .expect("compile_method_call inside a function context");
                // Slice 8ae: consult the method's ref / slice tables
                // so `self` and method args dispatch by mode (ref →
                // data ptr; mut Slice → coerce_to_slice; owned →
                // loaded value), mirroring slice 8z (per-mono
                // intercept in `compile_generic_call`) and slice 8ad
                // (non-generic free-fn intercept in `compile_call`).
                // Without this, a method whose param is `ref T` /
                // `mut Slice[T]` would store the wrong-shape value
                // into the ptr- or Slice-struct-shaped state-struct
                // field. `fn_param_ref` / `fn_param_slice_elem` are
                // keyed on the impl-method's dotted name (e.g.
                // `"Hub.run"`) — populated by `declare_function`
                // against the synthesized impl-method function whose
                // `params[0]` is self after `make_impl_method_function`
                // promotes the `SelfParam` into a real `Param`. So
                // `ref_flags[0]` covers `ref self` / `mut ref self`;
                // `ref_flags[1..]` covers method args at param indices
                // 1..K.
                let ref_flags = self.fn_param_ref.get(key).cloned().unwrap_or_default();
                let slice_elems = self
                    .fn_param_slice_elem
                    .get(key)
                    .cloned()
                    .unwrap_or_default();

                // Allocate the state struct via the constructor.
                let state_call = self
                    .builder
                    .build_call(ctor_fn, &[], "kara.state")
                    .expect("call state-struct constructor");
                let state_ptr = state_call
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_pointer_value();
                // Store the receiver into state struct field 1 (self
                // is at layout position 0 → state struct field 1
                // after the i32 tag at field 0). Dispatch by self's
                // declared mode: `ref self` / `mut ref self` route
                // through `get_data_ptr` for Identifier receivers (or
                // materialize an rvalue temp); plain `self` stores
                // the loaded value as before.
                let self_field_ptr = self
                    .builder
                    .build_struct_gep(state_struct, state_ptr, 1, "kara.self.field_ptr")
                    .expect("GEP state struct field 1 for self");
                let self_is_ref = ref_flags.first().copied().unwrap_or(false);
                let self_to_store: BasicValueEnum<'ctx> = if self_is_ref {
                    if let ExprKind::Identifier(var_name) = &object.kind {
                        if let Some(ptr) = self.get_data_ptr(var_name) {
                            ptr.into()
                        } else {
                            let val = self.compile_expr(object)?;
                            self.materialize_rvalue_for_ref_arg(val, usize::MAX)
                        }
                    } else {
                        let val = self.compile_expr(object)?;
                        self.materialize_rvalue_for_ref_arg(val, usize::MAX)
                    }
                } else {
                    self.compile_expr(object)?
                };
                self.builder
                    .build_store(self_field_ptr, self_to_store)
                    .expect("store self into state struct field 1");
                // Method args follow at fields 2..K. ref_flags /
                // slice_elems param indices are offset by 1 (self at
                // index 0, so method arg `i` is at param index
                // `i + 1`).
                for (i, arg) in args.iter().enumerate() {
                    let field_idx = (i + 2) as u32;
                    let field_ptr = self
                        .builder
                        .build_struct_gep(
                            state_struct,
                            state_ptr,
                            field_idx,
                            &format!("kara.arg{i}.field_ptr"),
                        )
                        .expect("GEP state struct field for method arg");

                    let param_idx = i + 1;
                    let is_ref = ref_flags.get(param_idx).copied().unwrap_or(false);
                    let slice_elem = slice_elems.get(param_idx).copied().flatten();

                    let to_store: BasicValueEnum<'ctx> = if is_ref {
                        if let ExprKind::Identifier(var_name) = &arg.value.kind {
                            if let Some(ptr) = self.get_data_ptr(var_name) {
                                ptr.into()
                            } else {
                                let val = self.compile_expr(&arg.value)?;
                                self.materialize_rvalue_for_ref_arg(val, i)
                            }
                        } else {
                            let val = self.compile_expr(&arg.value)?;
                            self.materialize_rvalue_for_ref_arg(val, i)
                        }
                    } else if let Some(elem_ty) = slice_elem {
                        match self.coerce_to_slice(&arg.value, elem_ty)? {
                            Some(slice_val) => slice_val,
                            None => self.compile_expr(&arg.value)?,
                        }
                    } else {
                        self.compile_expr(&arg.value)?
                    };

                    self.builder
                        .build_store(field_ptr, to_store)
                        .expect("store method arg into state struct field");
                }
                // Poll loop + cooperative yield + done + free — same
                // shape as slice 8d/8e for the free-function intercept.
                let loop_bb = self.context.append_basic_block(cur_fn, "kara.poll_loop");
                let yield_bb = self.context.append_basic_block(cur_fn, "kara.poll_yield");
                let done_bb = self.context.append_basic_block(cur_fn, "kara.poll_done");
                self.builder
                    .build_unconditional_branch(loop_bb)
                    .expect("br to poll loop");
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
                self.builder.position_at_end(yield_bb);
                self.builder
                    .build_call(self.sched_yield_fn, &[], "kara.yield_result")
                    .expect("call sched_yield");
                self.builder
                    .build_unconditional_branch(loop_bb)
                    .expect("br back to poll loop after yield");
                self.builder.position_at_end(done_bb);
                // Slice 8i: load the callee's terminal return-value
                // field before `free`. Mirrors the call_dispatch.rs
                // intercept's load-before-free ordering — once the
                // state struct is freed, the field is no longer
                // dereferenceable.
                let call_result =
                    if let Some(ret_ty) = self.state_machine_return_types.get(key).copied() {
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
                            .expect("GEP terminal return-value field on caller side (method call)");
                        self.builder
                            .build_load(ret_ty, terminal_ptr, "kara.return.value")
                            .expect("load callee return value from terminal field (method call)")
                    } else {
                        self.context.i64_type().const_int(0, false).into()
                    };
                self.builder
                    .build_call(self.free_fn, &[state_ptr.into()], "")
                    .expect("call free on state struct");
                return Ok(call_result);
            }
        }

        // Strict-provenance `ptr` module — `ptr.addr(p)` /
        // `ptr.with_addr(p, a)` / `ptr.expose(p)` / `ptr.from_exposed(a)`
        // (and the `_mut` variants), per `design.md § Pointer
        // Provenance` (v60 item 20). Skipped when a local binding
        // shadows `ptr` — the prelude module loses to a user-scope
        // binding by the standard shadow rule. The seven entries are
        // also registered in `env.functions` for the typechecker (see
        // `src/typechecker/env_build.rs`), so the dispatch shapes line
        // up between the two phases. Helper's docstring covers the
        // pragmatic-lowering rationale under the current i64-pointer
        // ABI plus the follow-up path to a provenance-preserving
        // variant.
        if let ExprKind::Identifier(name) = &object.kind {
            if name == "ptr" && !self.variables.contains_key("ptr") {
                if let Some(value) = self.compile_ptr_module_call(method, args)? {
                    return Ok(value);
                }
            }
        }

        // Slice OR (2026-05-16): Option/Result `unwrap`/`expect`/`is_*`
        // dispatch is receiver-shape-agnostic — the receiver may be any
        // Option-/Result-valued expression (identifier, method chain,
        // field access, index, …). Lower the receiver to its
        // `{ i64 tag, i64 w0, i64 w1, i64 w2 }` aggregate, dispatch on
        // the tag, and either reconstitute the payload (`unwrap`/`expect`)
        // or yield a bool (`is_some`/`is_none`/`is_ok`/`is_err`). The
        // inner `T` for payload reconstitution is recovered from the
        // typechecker-populated `method_unwrap_inner_types` side-table.
        // Routing this dispatch BEFORE the Index/FieldAccess
        // synth-identifier arms is intentional: those arms mint a synth
        // tied to the *receiver's storage*, which doesn't exist for
        // method-chain receivers like `m.get(k).unwrap()`. Keeping the
        // receiver as a temporary SSA value sidesteps that constraint
        // entirely.
        if matches!(
            method,
            "unwrap" | "expect" | "is_some" | "is_none" | "is_ok" | "is_err" | "unwrap_or"
        ) {
            if let Some(value) =
                self.try_compile_option_result_method(object, method, args, call_span)?
            {
                return Ok(value);
            }
        }

        // Slice MR (2026-05-09): indexed-receiver method dispatch. When the
        // receiver expression is `obj[i]` (an `Index` node), lower the index
        // access to obtain a pointer into the outer container's storage,
        // synthesize an identifier bound to that pointer with the element's
        // type registries populated, and re-dispatch the method through the
        // existing identifier path. Closes the LeetCode 3629 kata's primary
        // blocker (`factors[j].push(i)`). MR5: chained `a[i][j].method()` is
        // rejected with a clear diagnostic — bind to a temporary first.
        if let ExprKind::Index {
            object: inner,
            index,
        } = &object.kind
        {
            return self.compile_indexed_receiver_method(inner, index, method, args, call_span);
        }

        // Slice FR (2026-05-16): field-receiver method dispatch. Sibling to
        // the MR slice above — when the receiver is `outer.field` (a
        // `FieldAccess`), GEP into the struct (shared or plain) to the field
        // pointer, mint a synth identifier bound to that pointer with the
        // field type's side tables populated, and re-dispatch the method.
        // Closes the LeetCode 133 kata's primary blocker
        // (`curr_clone.neighbors.push(nb_clone)` on a `shared struct Node`
        // with `mut neighbors: Vec[Node]`). Returns `Some(_)` only when the
        // receiver shape is one we know how to lower; otherwise the regular
        // dispatch below runs (so the generic field-by-value extract path
        // and the fall-through diagnostic still apply for unsupported
        // shapes).
        if let ExprKind::FieldAccess {
            object: inner,
            field,
        } = &object.kind
        {
            // `self.field.method()` — `self` parses as `SelfValue`, which the
            // shared `lower_field_access_ptr` (used by the helper below)
            // deliberately leaves at `Ok(None)` so the atomic-on-self path
            // (`self.count.fetch_add(...)`, dispatched further down via
            // `is_atomic_receiver` → `compile_atomic_method`) keeps its
            // dedicated handler. For NON-atomic self-field receivers we
            // normalise to a synthetic `Identifier("self")` (self is registered
            // under the name "self" in every per-binding registry) so String /
            // Vec field methods dispatch through the field-receiver helper.
            // Gated on `!is_atomic_receiver(object)` so the atomic fall-through
            // is byte-identical. Self-hosting lexer: `self.src.substring(a, b)`.
            let self_ident;
            let inner: &Expr =
                if matches!(inner.kind, ExprKind::SelfValue) && !self.is_atomic_receiver(object) {
                    self_ident = Expr {
                        kind: ExprKind::Identifier("self".to_string()),
                        span: inner.span.clone(),
                    };
                    &self_ident
                } else {
                    inner
                };
            if let Some(value) =
                self.try_compile_field_receiver_method(inner, field, method, args, call_span)?
            {
                return Ok(value);
            }
        }

        // `h.m.0.method()` — a method on a Map/Set TUPLE element (#26). The
        // `FieldAccess` arm above handles `s.m.method()`; this is the
        // tuple-index sibling. Returns `Some` only for a Map/Set element (the
        // ptr-handle case that needs a named handle slot); Vec/scalar/struct
        // tuple elements fall through to the value-extraction path below.
        if matches!(object.kind, ExprKind::TupleIndex { .. }) {
            if let Some(value) =
                self.try_compile_tuple_index_receiver_method(object, method, args, call_span)?
            {
                return Ok(value);
            }
        }

        // Trailing-method dispatch on an entry-chain receiver — e.g.
        // `bucket.entry(p).or_insert(Vec.new()).push(j)`. The chain
        // produces a slot pointer (`*mut V`); the synth-identifier
        // pattern (mirrors MR-slice indexed-receiver dispatch) wraps it
        // so the recursive call resolves `.method(args)` through the
        // regular identifier-keyed flow. Returns Some(_) only when the
        // receiver is a recognised or_insert / or_insert_with chain.
        if let Some(value) =
            self.compile_entry_chain_receiver_method(object, method, args, call_span)?
        {
            return Ok(value);
        }

        // Map.entry(k) chain dispatch — `m.entry(k){.and_modify(f)}*.{or_insert(d)|
        // or_insert_with(f)|and_modify(f)}` is lowered as a single sequence
        // around one `karac_map_entry` call so the slot pointer stays valid
        // and there's exactly one hash. Returns Some(_) only when the receiver
        // chain is recognised; otherwise the regular dispatch below runs.
        if let Some(value) = self.try_compile_entry_chain(object, method, args)? {
            return Ok(value);
        }

        // `clone()` dispatch on collection variables — Vec[T], String,
        // Map[K, V], Set[T]. Routes through the per-type clone-fn machinery
        // (`emit_clone_fn_for_type_expr`); see the `Clone trait surface for
        // collections` bullet in `phase-8-stdlib-floor.md`. Returns Some(_)
        // when the receiver is an identifier-bound collection variable;
        // otherwise the regular dispatch below runs (so user `impl X { fn
        // clone(...) }` continues to resolve through the impl-block path).
        // Is this call's receiver a scalar `Copy` primitive (int / float /
        // bool / char)? Read it from the static receiver type the typechecker
        // recorded for this call span (`dispatch_key` = "<Type>.<method>"),
        // NOT from the compiled value's LLVM kind — so we can gate `clone` /
        // `to_string` below WITHOUT pre-compiling the receiver, which keeps a
        // single evaluation for any receiver form (literal, `(expr)`, field,
        // call) and never double-evaluates a side-effecting receiver.
        let recv_is_scalar_primitive = dispatch_key
            .as_deref()
            .and_then(|k| k.rsplit_once('.'))
            .map(|(t, _)| {
                matches!(
                    t,
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
                )
            })
            .unwrap_or(false);

        if method == "clone" && args.is_empty() {
            if let Some(value) = self.try_compile_clone(object)? {
                return Ok(value);
            }
            // Scalar `Copy` primitive — clone is identity.
            if recv_is_scalar_primitive {
                return self.compile_expr(object);
            }
        }

        // `recv.try_clone() -> Result[Self, AllocError]` — the fallible
        // companion of `clone` (phase-8-stdlib-floor item 8). Routed here
        // (before the receiver-type dispatch below) so Vec/VecDeque/String
        // share one lowering; Map/Set-bearing receivers are rejected loudly
        // inside `try_compile_try_clone` (blocked on a fallible
        // `karac_map_*` runtime API).
        if method == "try_clone" && args.is_empty() {
            if let Some(value) = self.try_compile_try_clone(object)? {
                return Ok(value);
            }
        }

        // Scalar-primitive `x.to_string() -> String` (typed in
        // expr_method_call.rs). Render the value via the same path f-strings
        // use, then copy the bytes into an owning `String`. `char` lowers to
        // i32, so render it as a glyph rather than the integer codepoint.
        // String/struct receivers (whose explicit `.to_string()` is a
        // separate, unimplemented codegen path) are not scalar primitives and
        // fall through unchanged.
        if method == "to_string" && args.is_empty() && recv_is_scalar_primitive {
            let v = self.compile_expr(object)?;
            let (src_ptr, src_len) = if self.expr_is_char(object) {
                self.emit_codepoint_to_utf8(v.into_int_value())
            } else {
                self.compile_fstr_part_to_cstr(v, object)
            };
            return Ok(self.build_owned_string_from_parts(src_ptr, src_len));
        }

        // `String.to_string()` — an owning copy. The receiver's static type is
        // `String` when `dispatch_key`'s receiver segment is "String". Compile
        // the receiver to its `{data,len,cap}` value and copy the bytes into a
        // fresh heap String, so it works for any receiver form (identifier,
        // literal, expression) and the result owns its buffer.
        //
        // `StringSlice.to_string()` is the borrowed-view escape hatch (design.md
        // § StringSlice: "To store a slice beyond the borrow, call .to_string()")
        // — the same copy: a `StringSlice` is `{ptr,len,cap=0}`, so copying its
        // `len` bytes yields an independent owned `String`.
        if method == "to_string"
            && args.is_empty()
            && dispatch_key
                .as_deref()
                .and_then(|k| k.rsplit_once('.'))
                .map(|(t, _)| t == "String" || t == "StringSlice")
                .unwrap_or(false)
        {
            let v = self.compile_expr(object)?.into_struct_value();
            let data = self
                .builder
                .build_extract_value(v, 0, "ts.s.data")
                .unwrap()
                .into_pointer_value();
            let len = self
                .builder
                .build_extract_value(v, 1, "ts.s.len")
                .unwrap()
                .into_int_value();
            return Ok(self.build_owned_string_from_parts(data, len));
        }

        // `myStruct.to_string()` for a `#[derive(Display)]` / `impl Display`
        // struct → render to an owning `String` in declaration order (matches
        // the interpreter). See `synth_display.rs`.
        //
        // A user `impl Display` (a compiled `<Type>.to_string`) wins: skip the
        // built-in renderers below so the call falls through to the generic
        // user-method dispatch, which invokes the user body. GAP-W4.
        if method == "to_string" && args.is_empty() && self.user_display_impl_type(object).is_none()
        {
            if let Some(sname) = self.expr_user_struct_name(object) {
                return self.compile_struct_display_string(object, &sname);
            }
            // All-unit enum → owning String of the variant name.
            if let Some(ename) = self.expr_user_enum_name(object) {
                let (ptr, len) = self.compile_unit_enum_display(object, &ename)?;
                return Ok(self.build_owned_string_from_parts(ptr, len));
            }
            // Collection (Vec/Map/Set) → owning String via its Display fn. The
            // returned value owns the rendered buffer (the binding frees it);
            // the throwaway acc alloca is not separately tracked.
            if let Some((_acc, sval)) = self.try_compile_collection_display(object)? {
                return Ok(sval);
            }
        }

        // Type-receiver associated calls: `T.method(...)` where `T` is a
        // primitive type name. Receiver `T` is an identifier naming a type,
        // not a variable, so the normal receiver pipeline would fail. Handle
        // `.from` (numeric widening = passthrough) and the operator methods
        // (add/sub/eq/lt/bitand/not/…) by delegating to `compile_assoc_call`,
        // which already knows the primitive fast-path.
        if let ExprKind::Identifier(type_name) = &object.kind {
            let is_primitive = matches!(
                type_name.as_str(),
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
                const OP_METHODS: &[&str] = &[
                    "from", "add", "sub", "mul", "div", "rem", "neg", "eq", "ne", "lt", "le", "gt",
                    "ge", "bitand", "bitor", "bitxor", "shl", "shr", "not",
                ];
                if OP_METHODS.contains(&method) {
                    return self.compile_assoc_call(type_name.as_str(), method, args);
                }
                // `<int_type>.parse(s: String) -> Option[i64]` — base-10
                // signed parse. Extends the primitive-type-receiver
                // dispatch already used by binop methods.
                if method == "parse"
                    && matches!(
                        type_name.as_str(),
                        "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64" | "usize"
                    )
                {
                    return self.compile_assoc_call(type_name.as_str(), method, args);
                }
                // `<int_type>.from_str_radix(s, radix) -> Option[i64]` — radix
                // parse; same delegation as `parse` (impl in assoc_call.rs).
                if method == "from_str_radix"
                    && matches!(
                        type_name.as_str(),
                        "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64" | "usize"
                    )
                {
                    return self.compile_assoc_call(type_name.as_str(), method, args);
                }
                // `f64.parse(s) -> Option[f64]` — float parse; same delegation
                // as int `parse` (impl in assoc_call.rs).
                if method == "parse" && type_name.as_str() == "f64" {
                    return self.compile_assoc_call(type_name.as_str(), method, args);
                }
                // `char.try_from(n) -> Result[char, i64]` — fallible codepoint→
                // char conversion (#10; the `E_INT_AS_CHAR` rejection of
                // `n as char` redirects here). Validates the Unicode scalar
                // range and returns `Ok(char)` / `Err(codepoint)`.
                if method == "try_from" && type_name.as_str() == "char" {
                    return self.compile_char_try_from(args);
                }
            }
        }

        // Receiver-form `lhs.cmp(rhs)` — synthesizes an `Ordering` enum
        // value from a signed-integer comparison. The receiver may be an
        // identifier (closure param or local) or an arbitrary expression
        // (e.g., `(b.1 - b.0).cmp(...)`), so we evaluate both sides and
        // dispatch on the LLVM value kind. Tag layout matches the
        // declaration order in `runtime/stdlib/ordering.kara` (Less=0,
        // Equal=1, Greater=2); the `Vec.sort_by` bridge thunk relies on
        // that ordering to turn the tag into a `-1 / 0 / +1` comparator
        // via `tag - 1`.
        // Built-in `abs` on signed-integer / float primitives (typed in
        // expr_method_call.rs). Integer abs reuses the checked-neg lowering:
        // `abs(x) = select(x < 0, 0 - x, x)` where `0 - x` goes through the
        // same `ssub.with.overflow` trap path as unary `-`, so `iN::MIN.abs()`
        // traps as `integer overflow` (the neg is computed for all x but only
        // overflows at `iN::MIN`; for x ≥ 0, `0 - x` is in range). Float abs is
        // `select(x < 0.0, -x, x)` — correct for finite values (−0.0/NaN sign
        // edge cases are immaterial here and not exercised).
        if method == "abs" && args.is_empty() {
            let v = self.compile_expr(object)?;
            match v {
                BasicValueEnum::IntValue(iv) => {
                    let zero = iv.get_type().const_zero();
                    let is_neg = self
                        .builder
                        .build_int_compare(IntPredicate::SLT, iv, zero, "abs.isneg")
                        .unwrap();
                    let neg = self
                        .compile_unaryop(&UnaryOp::Neg, iv.into())?
                        .into_int_value();
                    let r = self.builder.build_select(is_neg, neg, iv, "abs").unwrap();
                    return Ok(r);
                }
                BasicValueEnum::FloatValue(fv) => {
                    let zero = fv.get_type().const_zero();
                    let is_neg = self
                        .builder
                        .build_float_compare(inkwell::FloatPredicate::OLT, fv, zero, "fabs.isneg")
                        .unwrap();
                    let neg = self.builder.build_float_neg(fv, "fabs.neg").unwrap();
                    let r = self.builder.build_select(is_neg, neg, fv, "fabs").unwrap();
                    return Ok(r);
                }
                _ => {}
            }
        }

        // Built-in `sqrt` on float primitives (typed in expr_method_call.rs):
        // `x.sqrt() -> Self`, lowered to the overloaded `llvm.sqrt` intrinsic —
        // a single `f64.sqrt` instruction on wasm (and `sqrtsd` on x86), no
        // libm dependency. Float-only; other receivers fall through.
        if method == "sqrt" && args.is_empty() {
            let v = self.compile_expr(object)?;
            if let BasicValueEnum::FloatValue(fv) = v {
                let intrinsic = inkwell::intrinsics::Intrinsic::find("llvm.sqrt")
                    .expect("llvm.sqrt intrinsic must exist");
                let decl = intrinsic
                    .get_declaration(&self.module, &[fv.get_type().into()])
                    .expect("llvm.sqrt declaration for float type");
                let r = self
                    .builder
                    .build_call(decl, &[fv.into()], "fsqrt")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic();
                return Ok(r);
            }
        }

        // Built-in scalar transcendental + rounding math on float primitives
        // (typed in expr_method_call.rs; surface in `crate::float_math`): unary
        // `sin`/`cos`/`tan`/`exp`/`ln`/`log2`/`floor`/`ceil`/`round` and binary
        // `pow`/`atan2`. Most lower to their overloaded LLVM intrinsic, which
        // becomes a libm call on most targets — and on wasm too, where the math
        // symbols live in wasi-libc's `libc.a` (already linked by the wasm-ld
        // path), so no archive/`--export` work is needed. `tan` and `atan2` are
        // the exceptions: `llvm.tan` / `llvm.atan2` are LLVM-19+, absent on the
        // 18.1 pin, so they lower to a direct width-correct libm call
        // (`tan`/`tanf`, `atan2`/`atan2f`). Float-only; a non-float receiver
        // (e.g. a user type with its own `round` method) falls through to
        // normal dispatch.
        if let Some(kind) = crate::float_math::classify(method) {
            let v = self.compile_expr(object)?;
            if let BasicValueEnum::FloatValue(fv) = v {
                let fty = fv.get_type();
                let is_f32 = fty == self.context.f32_type();
                // `tan` / `atan2` have no LLVM-18 intrinsic — call libm directly,
                // picking the width-correct symbol (`f`-suffixed for f32).
                let libm_sym = match (method, is_f32) {
                    ("tan", false) => Some("tan"),
                    ("tan", true) => Some("tanf"),
                    ("atan2", false) => Some("atan2"),
                    ("atan2", true) => Some("atan2f"),
                    _ => None,
                };
                if let Some(sym) = libm_sym {
                    let mut call_args = vec![fv.into()];
                    let mut params = vec![fty.into()];
                    if matches!(kind, crate::float_math::FloatMathKind::Binary) {
                        let BasicValueEnum::FloatValue(yv) = self.compile_expr(&args[0].value)?
                        else {
                            panic!(
                                "{method} argument must be a float value (typechecker invariant)"
                            );
                        };
                        call_args.push(yv.into());
                        params.push(fty.into());
                    }
                    let fn_val = match self.module.get_function(sym) {
                        Some(f) => f,
                        None => {
                            let fn_ty = fty.fn_type(&params, false);
                            self.module.add_function(sym, fn_ty, None)
                        }
                    };
                    let r = self
                        .builder
                        .build_call(fn_val, &call_args, "flibm")
                        .unwrap()
                        .try_as_basic_value()
                        .unwrap_basic();
                    return Ok(r);
                }
                let intrinsic_name = match method {
                    "sin" => "llvm.sin",
                    "cos" => "llvm.cos",
                    "exp" => "llvm.exp",
                    "ln" => "llvm.log",
                    "log2" => "llvm.log2",
                    "floor" => "llvm.floor",
                    "ceil" => "llvm.ceil",
                    "round" => "llvm.round",
                    "pow" => "llvm.pow",
                    _ => unreachable!("float_math codegen classify/match drift"),
                };
                let intrinsic = inkwell::intrinsics::Intrinsic::find(intrinsic_name)
                    .unwrap_or_else(|| panic!("{intrinsic_name} intrinsic must exist"));
                let decl = intrinsic
                    .get_declaration(&self.module, &[fty.into()])
                    .unwrap_or_else(|| panic!("{intrinsic_name} declaration for float type"));
                let r = match kind {
                    crate::float_math::FloatMathKind::Binary => {
                        let av = self.compile_expr(&args[0].value)?;
                        self.builder
                            .build_call(decl, &[fv.into(), av.into()], "fmath")
                    }
                    crate::float_math::FloatMathKind::Unary => {
                        self.builder.build_call(decl, &[fv.into()], "fmath")
                    }
                }
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic();
                return Ok(r);
            }
        }

        // Wrapping integer arithmetic (typed in expr_method_call.rs):
        // `wrapping_add` / `wrapping_sub` / `wrapping_mul`, the non-trapping
        // sibling of the checked `+`/`-`/`*` path. Lowers to a bare
        // `build_int_{add,sub,mul}` — silent two's-complement wraparound, no
        // `with.overflow` intrinsic and no trap branch (cf.
        // `emit_checked_int_arith` in expr_ops.rs). A straight-line loop body
        // with no per-element overflow-trap side-exit is precisely what lets
        // LLVM auto-vectorize integer slice kernels (the trap branch is the
        // proven vectorization blocker — roadmap.md § Codegen Optimization).
        // Typecheck restricts the receiver + arg to the 64-bit widths
        // (i64/u64/usize), so the i64-backed operands wrap at the right width.
        if matches!(method, "wrapping_add" | "wrapping_sub" | "wrapping_mul") && args.len() == 1 {
            let lv = self.compile_expr(object)?.into_int_value();
            let rv = self.compile_expr(&args[0].value)?.into_int_value();
            let r = match method {
                "wrapping_add" => self.builder.build_int_add(lv, rv, "wadd"),
                "wrapping_sub" => self.builder.build_int_sub(lv, rv, "wsub"),
                "wrapping_mul" => self.builder.build_int_mul(lv, rv, "wmul"),
                _ => unreachable!(),
            }
            .unwrap();
            return Ok(r.into());
        }

        // Integer `.pow(exp)` (typed in expr_method_call.rs): `n.pow(k) -> Self`,
        // a repeated-multiply loop whose body reuses the `*` operator's
        // overflow-trapping multiply (`emit_checked_int_arith("mul", …)`), so an
        // out-of-range partial product traps `integer overflow` at the receiver
        // width exactly as `*` does. `acc` starts at 1; the `u32` exponent counts
        // the multiplications (`acc *= base`, `exp` times). Both operands stay at
        // the receiver's iN width; `exp == 0` yields `1`.
        if method == "pow" && args.len() == 1 {
            // Codegen widens narrow integers to i64 in value flow, so the receiver
            // width is recovered from the typechecker's callee record, not the
            // compiled value's type. The base is narrowed to that width so the
            // per-step trap fires at the declared width; the result is re-extended
            // to the i64-backed representation narrow integers flow in.
            let (bits, unsigned) = self.receiver_int_kind(object, call_span, "pow");
            let int_ty = self.int_type_for_bits(bits);
            let i64_t = self.context.i64_type();
            let base_raw = self.compile_expr(object)?.into_int_value();
            let base = self.coerce_int_to(base_raw, int_ty, unsigned);
            let exp = self.compile_expr(&args[0].value)?.into_int_value();
            let exp_ty = exp.get_type();
            let fn_val = self.current_fn.unwrap();

            let acc_slot = self.create_entry_alloca(fn_val, "pow.acc", int_ty.into());
            self.builder
                .build_store(acc_slot, int_ty.const_int(1, false))
                .unwrap();
            let i_slot = self.create_entry_alloca(fn_val, "pow.i", exp_ty.into());
            self.builder
                .build_store(i_slot, exp_ty.const_zero())
                .unwrap();

            let cond_bb = self.context.append_basic_block(fn_val, "pow.cond");
            let body_bb = self.context.append_basic_block(fn_val, "pow.body");
            let exit_bb = self.context.append_basic_block(fn_val, "pow.exit");
            self.builder.build_unconditional_branch(cond_bb).unwrap();

            // cond: i < exp (unsigned)
            self.builder.position_at_end(cond_bb);
            let i_cur = self
                .builder
                .build_load(exp_ty, i_slot, "pow.i.cur")
                .unwrap()
                .into_int_value();
            let go = self
                .builder
                .build_int_compare(IntPredicate::ULT, i_cur, exp, "pow.lt")
                .unwrap();
            self.builder
                .build_conditional_branch(go, body_bb, exit_bb)
                .unwrap();

            // body: acc = checked_mul(acc, base); i += 1  (the trapping mul
            // appends its own ok/trap blocks and leaves the builder on the ok
            // continuation, where the loop's increment + back-branch are emitted).
            self.builder.position_at_end(body_bb);
            let acc_cur = self
                .builder
                .build_load(int_ty, acc_slot, "pow.acc.cur")
                .unwrap()
                .into_int_value();
            let prod = self.emit_checked_int_arith("mul", acc_cur, base, unsigned)?;
            self.builder.build_store(acc_slot, prod).unwrap();
            let i_now = self
                .builder
                .build_load(exp_ty, i_slot, "pow.i.now")
                .unwrap()
                .into_int_value();
            let i_next = self
                .builder
                .build_int_add(i_now, exp_ty.const_int(1, false), "pow.i.next")
                .unwrap();
            self.builder.build_store(i_slot, i_next).unwrap();
            self.builder.build_unconditional_branch(cond_bb).unwrap();

            self.builder.position_at_end(exit_bb);
            let acc_final = self
                .builder
                .build_load(int_ty, acc_slot, "pow.result")
                .unwrap()
                .into_int_value();
            let result = self.coerce_int_to(acc_final, i64_t, unsigned);
            return Ok(result.into());
        }

        // Bit intrinsics (typed in expr_method_call.rs): `count_ones` /
        // `leading_zeros` / `trailing_zeros` -> u32, lowered to the overloaded
        // `llvm.ctpop` / `llvm.ctlz` / `llvm.cttz` intrinsics. The receiver is
        // narrowed to its declared width first (codegen widens narrow ints to
        // i64, which would otherwise count over 64 bits); the intrinsic is then
        // width-correct. `ctlz` / `cttz` take an `is_zero_poison` i1 (`false` →
        // defined to return the bit width on a zero input, matching Rust and the
        // interpreter). The non-negative count is z-extended to the i64-backed
        // representation the `u32` result flows in.
        if args.is_empty() && matches!(method, "count_ones" | "leading_zeros" | "trailing_zeros") {
            let (bits, unsigned) = self.receiver_int_kind(object, call_span, method);
            let int_ty = self.int_type_for_bits(bits);
            let v_raw = self.compile_expr(object)?.into_int_value();
            let v = self.coerce_int_to(v_raw, int_ty, unsigned);
            let (base_name, is_clz_ctz) = match method {
                "count_ones" => ("llvm.ctpop", false),
                "leading_zeros" => ("llvm.ctlz", true),
                "trailing_zeros" => ("llvm.cttz", true),
                _ => unreachable!(),
            };
            let intrinsic = inkwell::intrinsics::Intrinsic::find(base_name)
                .ok_or_else(|| format!("{base_name} intrinsic must exist in LLVM"))?;
            let decl = intrinsic
                .get_declaration(&self.module, &[int_ty.into()])
                .ok_or_else(|| format!("{base_name} has no declaration for width {bits}"))?;
            let raw = if is_clz_ctz {
                let no_poison = self.context.bool_type().const_zero();
                self.builder
                    .build_call(decl, &[v.into(), no_poison.into()], "bitintr")
            } else {
                self.builder.build_call(decl, &[v.into()], "bitintr")
            }
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();
            let i64_t = self.context.i64_type();
            // The count is non-negative and ≤ 64, so a zero-extend is always
            // correct regardless of the receiver's signedness.
            let res = self.coerce_int_to(raw, i64_t, true);
            return Ok(res.into());
        }

        // Overflow-aware integer arithmetic — `{checked,saturating,overflowing}_{add,sub,mul}`
        // (C2, B-2026-06-19-10). Lowered at the receiver's DECLARED width via the
        // `llvm.{s,u}{op}.with.overflow.iN` intrinsic (codegen widens narrow ints
        // to i64 in value flow, so both operands are first truncated back to iN).
        // The single `(wrapped, did_overflow)` pair feeds all three families,
        // matching the interpreter's width-correct semantics bit-for-bit:
        //   checked_*     -> `None` on overflow, else `Some(wrapped)` (Option[T])
        //   saturating_*  -> `wrapped` unless overflow, then the saturation bound
        //   overflowing_* -> `(wrapped, did_overflow)` tuple `(T, bool)`
        if args.len() == 1 {
            let fam_op = ["checked_", "saturating_", "overflowing_"]
                .into_iter()
                .find_map(|p| method.strip_prefix(p).map(|op| (p, op)))
                .filter(|(_, op)| matches!(*op, "add" | "sub" | "mul"));
            if let Some((fam, op)) = fam_op {
                let (bits, unsigned) = self.receiver_int_kind(object, call_span, method);
                let int_ty = self.int_type_for_bits(bits);
                let i64_t = self.context.i64_type();

                let recv_raw = self.compile_expr(object)?.into_int_value();
                let recv = self.coerce_int_to(recv_raw, int_ty, unsigned);
                let arg_raw = self.compile_expr(&args[0].value)?.into_int_value();
                let arg = self.coerce_int_to(arg_raw, int_ty, unsigned);

                let (wrapped, ovf) = self.emit_overflow_intrinsic(op, recv, arg, unsigned)?;

                match fam {
                    // overflowing_* -> `(T, bool)`: the tuple field for T is the
                    // declared width iN (matching `llvm_type_for_type_expr` of the
                    // `(T, bool)` tuple), the flag is the i1 overflow bit.
                    "overflowing_" => {
                        let bool_t = self.context.bool_type();
                        let tup_ty = self
                            .context
                            .struct_type(&[int_ty.into(), bool_t.into()], false);
                        let mut agg = tup_ty.get_undef();
                        agg = self
                            .builder
                            .build_insert_value(agg, wrapped, 0, "ovf.tup.v")
                            .unwrap()
                            .into_struct_value();
                        agg = self
                            .builder
                            .build_insert_value(agg, ovf, 1, "ovf.tup.f")
                            .unwrap()
                            .into_struct_value();
                        return Ok(agg.into());
                    }
                    // checked_* -> `Option[T]`: None on overflow, else Some(wrapped).
                    // The Some payload word is the result coerced to the i64-backed
                    // Option payload slot (zext for unsigned, sext for signed).
                    "checked_" => {
                        let fn_val = self.current_fn.unwrap();
                        let some_bb = self.context.append_basic_block(fn_val, "chk.some");
                        let none_bb = self.context.append_basic_block(fn_val, "chk.none");
                        let merge_bb = self.context.append_basic_block(fn_val, "chk.merge");
                        self.builder
                            .build_conditional_branch(ovf, none_bb, some_bb)
                            .unwrap();

                        self.builder.position_at_end(some_bb);
                        let payload = self.coerce_int_to(wrapped, i64_t, unsigned);
                        self.builder.build_unconditional_branch(merge_bb).unwrap();

                        self.builder.position_at_end(none_bb);
                        self.builder.build_unconditional_branch(merge_bb).unwrap();

                        self.builder.position_at_end(merge_bb);
                        let agg = self.build_option_some_via_phis(
                            &[payload],
                            some_bb,
                            none_bb,
                            "chk.opt",
                        );
                        return Ok(agg);
                    }
                    // saturating_* -> `T`: `wrapped` unless overflow, then clamp to
                    // the saturation bound. Unsigned: sub underflows to 0, add/mul
                    // overflow to UMAX. Signed: the bound is SMAX/SMIN by the sign
                    // of the true result — for add/sub `a >= 0 ? SMAX : SMIN` (on
                    // overflow the operands force that sign), for mul
                    // `sign(a)==sign(b) ? SMAX : SMIN`. Matches Rust / the interp's
                    // i128 clamp without needing a wider type (no `llvm.*mul.sat`).
                    _ => {
                        let zero = int_ty.const_zero();
                        let bound = if unsigned {
                            if op == "sub" {
                                int_ty.const_zero()
                            } else {
                                int_ty.const_all_ones()
                            }
                        } else {
                            let smax = int_ty.const_int(((1u128 << (bits - 1)) - 1) as u64, false);
                            let smin = int_ty.const_int((1u128 << (bits - 1)) as u64, false);
                            let pick_max = if op == "mul" {
                                let sa = self
                                    .builder
                                    .build_int_compare(IntPredicate::SLT, recv, zero, "sat.sa")
                                    .unwrap();
                                let sb = self
                                    .builder
                                    .build_int_compare(IntPredicate::SLT, arg, zero, "sat.sb")
                                    .unwrap();
                                self.builder
                                    .build_int_compare(IntPredicate::EQ, sa, sb, "sat.same")
                                    .unwrap()
                            } else {
                                self.builder
                                    .build_int_compare(IntPredicate::SGE, recv, zero, "sat.age")
                                    .unwrap()
                            };
                            self.builder
                                .build_select(pick_max, smax, smin, "sat.bound")
                                .unwrap()
                                .into_int_value()
                        };
                        let sat = self
                            .builder
                            .build_select(ovf, bound, wrapped, "sat.res")
                            .unwrap()
                            .into_int_value();
                        let res = self.coerce_int_to(sat, i64_t, unsigned);
                        return Ok(res.into());
                    }
                }
            }
        }

        // ASCII byte-classification predicates on integer scalars (the `u8`
        // bytes from `String.bytes()`): `is_ascii_digit` / `is_ascii_alphabetic`
        // / `is_ascii_hexdigit` → bool (i1). Phase-8 floor for the self-hosting
        // lexer's byte-indexed scan (phase-12-self-hosting.md). Lowered to inline
        // unsigned range checks — no runtime extern. Unsigned predicates so a
        // byte ≥ 0x80 never spuriously matches a signed range.
        if args.is_empty()
            && matches!(
                method,
                "is_ascii_digit" | "is_ascii_alphabetic" | "is_ascii_hexdigit"
            )
        {
            let v = self.compile_expr(object)?;
            if let BasicValueEnum::IntValue(iv) = v {
                let ty = iv.get_type();
                // in_range(lo, hi) = (iv >= lo) & (iv <= hi), unsigned.
                let in_range = |s: &Self, lo: u64, hi: u64, tag: &str| {
                    let ge = s
                        .builder
                        .build_int_compare(
                            IntPredicate::UGE,
                            iv,
                            ty.const_int(lo, false),
                            &format!("{tag}.ge"),
                        )
                        .unwrap();
                    let le = s
                        .builder
                        .build_int_compare(
                            IntPredicate::ULE,
                            iv,
                            ty.const_int(hi, false),
                            &format!("{tag}.le"),
                        )
                        .unwrap();
                    s.builder.build_and(ge, le, &format!("{tag}.in")).unwrap()
                };
                let digit = in_range(self, b'0' as u64, b'9' as u64, "ascii.d");
                let r = match method {
                    "is_ascii_digit" => digit,
                    "is_ascii_alphabetic" => {
                        let lower = in_range(self, b'a' as u64, b'z' as u64, "ascii.l");
                        let upper = in_range(self, b'A' as u64, b'Z' as u64, "ascii.u");
                        self.builder.build_or(lower, upper, "ascii.alpha").unwrap()
                    }
                    "is_ascii_hexdigit" => {
                        let lower = in_range(self, b'a' as u64, b'f' as u64, "ascii.hl");
                        let upper = in_range(self, b'A' as u64, b'F' as u64, "ascii.hu");
                        let af = self.builder.build_or(lower, upper, "ascii.hex.af").unwrap();
                        self.builder.build_or(digit, af, "ascii.hex").unwrap()
                    }
                    _ => unreachable!(),
                };
                return Ok(r.into());
            }
        }

        // `char.to_digit(radix) -> Option[u32]` (typed in expr_method_call.rs):
        // interpreter-complete (`karac run`), but the `Option[u32]` construction
        // lowering (shared with the `checked_to_*` float→int follow-on) is not
        // yet wired in codegen. Emit a clear, actionable error rather than
        // falling through to "no handler" / a miscompile. Gated on a `char`
        // receiver so a user `to_digit` on another type is unaffected.
        if method == "to_digit" && args.len() == 1 && self.expr_is_char(object) {
            return Err(
                "`char.to_digit(radix)` is not yet supported under `karac build` (codegen); \
                 it works under `karac run`. The `Option[u32]` construction lowering is a \
                 tracked follow-on."
                    .to_string(),
            );
        }

        // Unicode `char` classification predicates (phase-12 #13): `is_alphabetic`
        // / `is_numeric` / `is_alphanumeric` / `is_whitespace` → bool (i1). The
        // typechecker admits these only on a `char` receiver (lowered to i32), so
        // a method-name match suffices. Unlike the inlined ASCII byte predicates
        // above, Unicode classification needs the runtime's Unicode tables, so
        // route through the `karac_runtime_char_is_*` externs (declared in
        // `Codegen::new`). The extern returns i8 (0/1) → compare `!= 0` for i1.
        if args.is_empty()
            && matches!(
                method,
                "is_alphabetic"
                    | "is_numeric"
                    | "is_alphanumeric"
                    | "is_whitespace"
                    | "is_uppercase"
                    | "is_lowercase"
            )
        {
            let v = self.compile_expr(object)?;
            if let BasicValueEnum::IntValue(iv) = v {
                let i32_t = self.context.i32_type();
                let cp = match iv.get_type().get_bit_width() {
                    32 => iv,
                    w if w < 32 => self
                        .builder
                        .build_int_z_extend(iv, i32_t, "char.cp.z")
                        .unwrap(),
                    _ => self
                        .builder
                        .build_int_truncate(iv, i32_t, "char.cp.t")
                        .unwrap(),
                };
                let fname = match method {
                    "is_alphabetic" => "karac_runtime_char_is_alphabetic",
                    "is_numeric" => "karac_runtime_char_is_numeric",
                    "is_alphanumeric" => "karac_runtime_char_is_alphanumeric",
                    "is_whitespace" => "karac_runtime_char_is_whitespace",
                    "is_uppercase" => "karac_runtime_char_is_uppercase",
                    "is_lowercase" => "karac_runtime_char_is_lowercase",
                    _ => unreachable!(),
                };
                let f = self
                    .module
                    .get_function(fname)
                    .expect("char predicate extern declared in Codegen::new");
                let ret = self
                    .builder
                    .build_call(f, &[cp.into()], "char.pred")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_int_value();
                let b = self
                    .builder
                    .build_int_compare(
                        IntPredicate::NE,
                        ret,
                        self.context.i8_type().const_zero(),
                        "char.pred.b",
                    )
                    .unwrap();
                return Ok(b.into());
            }
        }

        if method == "cmp" && args.len() == 1 {
            let lhs = self.compile_expr(object)?;
            let rhs = self.compile_expr(&args[0].value)?;
            if let (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) = (lhs, rhs) {
                let i64_t = self.context.i64_type();
                let lt = self
                    .builder
                    .build_int_compare(IntPredicate::SLT, l, r, "cmp.lt")
                    .unwrap();
                let gt = self
                    .builder
                    .build_int_compare(IntPredicate::SGT, l, r, "cmp.gt")
                    .unwrap();
                let zero = i64_t.const_zero();
                let one = i64_t.const_int(1, false);
                let two = i64_t.const_int(2, false);
                let tag_gt = self
                    .builder
                    .build_select(gt, two, one, "cmp.tag.gt")
                    .unwrap()
                    .into_int_value();
                let tag = self
                    .builder
                    .build_select(lt, zero, tag_gt, "cmp.tag")
                    .unwrap()
                    .into_int_value();
                let ord_struct_ty = self
                    .enum_layouts
                    .get("Ordering")
                    .map(|l| l.llvm_type)
                    .unwrap_or_else(|| self.context.struct_type(&[i64_t.into()], false));
                let agg = ord_struct_ty.get_undef();
                let agg = self.builder.build_insert_value(agg, tag, 0, "ord").unwrap();
                return Ok(agg.into_struct_value().into());
            }
            // String.cmp(other) -> Ordering — byte-lexicographic, the method
            // form of the `<`/`>` operators. `karac_string_cmp` returns -1/0/+1
            // (the same order Vec[String].sort / binary_search use), and the
            // Ordering tags are Less=0 / Equal=1 / Greater=2, so tag = cmp + 1
            // maps them directly. Guard on the operand LAYOUT (the String
            // {ptr,len,cap} header) rather than `inferred_receiver_type`, which
            // only resolves NAMED receivers — a string LITERAL (`"a".cmp(b)`) or
            // an INDEX (`v[0].cmp(v[1])`) receiver typechecks + runs but has no
            // var-name to look up, so the earlier name-only guard left them
            // falling through to the "not yet supported" catch-all (a run/build
            // divergence). The typechecker admits `.cmp` only on int/char/bool/
            // String, and int/char/bool are `IntValue` (handled above), so any
            // `{ptr,len,cap}`-shaped struct pair reaching here IS a String;
            // user-struct `.cmp` is rejected at typecheck and never arrives.
            if let (BasicValueEnum::StructValue(l), BasicValueEnum::StructValue(r)) = (lhs, rhs) {
                if l.get_type() == self.vec_struct_type() {
                    let i64_t = self.context.i64_type();
                    let ptr_ty = self.context.ptr_type(AddressSpace::default());
                    let l_ptr = self
                        .builder
                        .build_extract_value(l, 0, "cmp.l.ptr")
                        .unwrap()
                        .into_pointer_value();
                    let l_len = self
                        .builder
                        .build_extract_value(l, 1, "cmp.l.len")
                        .unwrap()
                        .into_int_value();
                    let r_ptr = self
                        .builder
                        .build_extract_value(r, 0, "cmp.r.ptr")
                        .unwrap()
                        .into_pointer_value();
                    let r_len = self
                        .builder
                        .build_extract_value(r, 1, "cmp.r.len")
                        .unwrap()
                        .into_int_value();
                    let cmp_fn =
                        self.module
                            .get_function("karac_string_cmp")
                            .unwrap_or_else(|| {
                                let fn_ty = i64_t.fn_type(
                                    &[ptr_ty.into(), i64_t.into(), ptr_ty.into(), i64_t.into()],
                                    false,
                                );
                                self.module.add_function(
                                    "karac_string_cmp",
                                    fn_ty,
                                    Some(inkwell::module::Linkage::External),
                                )
                            });
                    let raw = self
                        .builder
                        .build_call(
                            cmp_fn,
                            &[l_ptr.into(), l_len.into(), r_ptr.into(), r_len.into()],
                            "cmp.scmp",
                        )
                        .unwrap()
                        .try_as_basic_value()
                        .unwrap_basic()
                        .into_int_value();
                    let tag = self
                        .builder
                        .build_int_add(raw, i64_t.const_int(1, false), "cmp.tag")
                        .unwrap();
                    let ord_struct_ty = self
                        .enum_layouts
                        .get("Ordering")
                        .map(|l| l.llvm_type)
                        .unwrap_or_else(|| self.context.struct_type(&[i64_t.into()], false));
                    let agg = ord_struct_ty.get_undef();
                    let agg = self.builder.build_insert_value(agg, tag, 0, "ord").unwrap();
                    return Ok(agg.into_struct_value().into());
                }
            }
        }

        // `.as_slice()` / `.as_slice_mut()` on Array, Vec, or Slice —
        // synthesize a `{ptr, i64}` slice header. The element type for the
        // resulting slice is inferred from the source variable, not from a
        // user-supplied argument. See design.md § Slices.
        if (method == "as_slice" || method == "as_slice_mut") && args.is_empty() {
            if let ExprKind::Identifier(name) = &object.kind {
                if let Some(slot) = self.variables.get(name.as_str()).copied() {
                    let i64_t = self.context.i64_type();
                    let slice_ty = self.slice_struct_type();
                    if let BasicTypeEnum::ArrayType(at) = slot.ty {
                        let len = i64_t.const_int(at.len() as u64, false);
                        return Ok(self.build_slice_header(slice_ty, slot.ptr, len));
                    }
                    if self.slice_elem_types.contains_key(name.as_str()) {
                        return Ok(self
                            .builder
                            .build_load(slice_ty, slot.ptr, "as_slice.passthrough")
                            .unwrap());
                    }
                    if self.vec_elem_types.contains_key(name.as_str()) {
                        let ptr_ty = self.context.ptr_type(AddressSpace::default());
                        let vec_ty = self.vec_struct_type();
                        let data_pp = self
                            .builder
                            .build_struct_gep(vec_ty, slot.ptr, 0, "as_slice.v.data.pp")
                            .unwrap();
                        let data = self
                            .builder
                            .build_load(ptr_ty, data_pp, "as_slice.v.data")
                            .unwrap()
                            .into_pointer_value();
                        let len_p = self
                            .builder
                            .build_struct_gep(vec_ty, slot.ptr, 1, "as_slice.v.len.p")
                            .unwrap();
                        let len = self
                            .builder
                            .build_load(i64_t, len_p, "as_slice.v.len")
                            .unwrap()
                            .into_int_value();
                        return Ok(self.build_slice_header(slice_ty, data, len));
                    }
                }
            }
        }

        // Module-binding receivers dispatch through the same Vec / Map / Set
        // codegen paths as local Vec / Map / Set variables — the slice-10
        // `reseed_module_binding_side_tables` registers `vec_elem_types` /
        // `map_key_types` / `set_elem_types` for each module binding, and
        // `get_data_ptr` falls back to the binding's global pointer when
        // the name isn't a local. The typechecker's
        // `path_call_method_dispatch` rewrite + the lowering pass already
        // converted the `Call(Path([X, method]))` shape to `MethodCall(X,
        // method)` for value-binding receivers, so the receiver-shape
        // routing here is uniform with the local-variable case.
        if let ExprKind::Identifier(name) = &object.kind {
            if !self.variables.contains_key(name.as_str())
                && self.module_bindings.contains_key(name.as_str())
            {
                if self.vec_elem_types.contains_key(name.as_str()) {
                    let data_ptr = self.get_data_ptr(name).unwrap();
                    return self.compile_vec_method(name, data_ptr, method, args);
                }
                if self.map_key_types.contains_key(name.as_str()) {
                    let name = name.clone();
                    return self.compile_map_method(&name, method, args);
                }
                if self.set_elem_types.contains_key(name.as_str()) {
                    let name = name.clone();
                    return self.compile_set_method(&name, method, args);
                }
            }
        }

        if let ExprKind::Identifier(name) = &object.kind {
            if let Some(slot) = self.variables.get(name.as_str()).copied() {
                // Array methods (owned — slot.ty is ArrayType)
                if let BasicTypeEnum::ArrayType(at) = slot.ty {
                    if method == "len" {
                        return Ok(self
                            .context
                            .i64_type()
                            .const_int(at.len() as u64, false)
                            .into());
                    }
                    // `as_ptr()` / `as_mut_ptr()` — the element-0 address of
                    // the owned array's storage, handed out as the raw
                    // pointer `*const T` / `*mut T` (raw pointers lower to a
                    // genuine LLVM `ptr`; the typechecker types these in
                    // `infer_method_call`'s Array arm). Mirrors `CStr.as_ptr`,
                    // except the producer is a GEP to element 0 rather than a
                    // struct field — `slot.ptr` points at the `[N x T]`
                    // alloca, and `[0, 0]` is its first element.
                    if method == "as_ptr" || method == "as_mut_ptr" {
                        let zero = self.context.i32_type().const_zero();
                        let elem0 = unsafe {
                            self.builder
                                .build_in_bounds_gep(at, slot.ptr, &[zero, zero], "arr.as_ptr")
                                .map_err(|e| format!("Array.{method} gep: {e}"))?
                        };
                        return Ok(elem0.into());
                    }
                }
                // Ref Array methods — ref_params has the inner type
                if let Some(&BasicTypeEnum::ArrayType(at)) = self.ref_params.get(name.as_str()) {
                    if method == "len" {
                        return Ok(self
                            .context
                            .i64_type()
                            .const_int(at.len() as u64, false)
                            .into());
                    }
                    // `as_ptr()` / `as_mut_ptr()` on a `ref Array` — the ref
                    // param already carries the data pointer (element-0), so
                    // hand it out directly. Same `*const T` / `*mut T` result
                    // as the owned arm above.
                    if method == "as_ptr" || method == "as_mut_ptr" {
                        let data = self.get_data_ptr(name).ok_or_else(|| {
                            format!("Array.{method}: no data pointer for ref array '{name}'")
                        })?;
                        return Ok(data.into());
                    }
                }
                // SoA layout methods
                if let Some(soa) = self.active_soa_layout(name.as_str()) {
                    return self.compile_soa_method(name, &soa, slot, method, args);
                }
                // Tensor instance methods — shape()/rank() read the
                // `[rank][dims][data]` header (`src/codegen/tensor.rs`).
                // The reshape/permute/slice/squeeze family is handled by
                // `try_compile_tensor_transform` at the top of this fn
                // (covers identifier + chained receivers); only `iter_axis`
                // remains a follow-on codegen slice and errors loudly here
                // rather than falling through to the silent-0 default.
                if let Some(info) = self.tensor_var_infos.get(name.as_str()) {
                    match method {
                        "shape" | "rank" => {
                            let t_ptr = self.tensor_ptr_for_var(name)?;
                            return self.compile_tensor_shape_method(t_ptr, method);
                        }
                        "iter_axis" => {
                            let (elem, rank) = (info.elem, info.dims.len());
                            let t_ptr = self.tensor_ptr_for_var(name)?;
                            return self
                                .compile_tensor_iter_axis(t_ptr, elem, rank, args, call_span);
                        }
                        _ => {}
                    }
                }
                // Vec/String methods (owned or ref)
                if self.vec_elem_types.contains_key(name.as_str()) {
                    let data_ptr = self.get_data_ptr(name).unwrap();
                    return self.compile_vec_method(name, data_ptr, method, args);
                }
                // Slice[T] / mut Slice[T] read-only methods. For an OWNED
                // slice the stack alloca holds the 2-field `{ptr, i64}` struct
                // directly (see `slice_struct_type`); for a `ref Slice[T]` /
                // `mut ref Slice[T]` parameter the alloca holds a pointer TO
                // that struct instead. `get_data_ptr` normalizes both to a
                // pointer at the `{ptr, i64}` header (owned → the alloca as-is,
                // ref → one load through it), so every method below GEPs off
                // `slice_ptr`, not the raw `slot.ptr`. Using `slot.ptr` for a
                // ref param GEP'd into the pointer-to-header itself and read
                // the caller's stack words as if they were slice fields —
                // `get_unchecked` then indexed the header struct instead of the
                // buffer and printed the data-pointer / len as "elements"
                // (B-2026-07-02-28). The `xs[i]` index path already routes
                // through `get_data_ptr` (`compile_slice_index`); this mirrors
                // it for the method family.
                if self.slice_elem_types.contains_key(name.as_str()) {
                    let i64_t = self.context.i64_type();
                    let slice_ty = self.slice_struct_type();
                    let slice_ptr = self.get_data_ptr(name).ok_or_else(|| {
                        format!("Slice.{method}: no data pointer for slice '{name}'")
                    })?;
                    match method {
                        "len" => {
                            let len_ptr = self
                                .builder
                                .build_struct_gep(slice_ty, slice_ptr, 1, "slice.len.ptr")
                                .unwrap();
                            let len = self
                                .builder
                                .build_load(i64_t, len_ptr, "slice.len")
                                .unwrap();
                            return Ok(len);
                        }
                        "is_empty" => {
                            let len_ptr = self
                                .builder
                                .build_struct_gep(slice_ty, slice_ptr, 1, "slice.len.ptr")
                                .unwrap();
                            let len = self
                                .builder
                                .build_load(i64_t, len_ptr, "slice.len")
                                .unwrap()
                                .into_int_value();
                            let zero = i64_t.const_zero();
                            let is_empty = self
                                .builder
                                .build_int_compare(IntPredicate::EQ, len, zero, "slice.is_empty")
                                .unwrap();
                            return Ok(is_empty.into());
                        }
                        // `Slice[T].get_unchecked(i) -> T` — direct-index read
                        // with NO bounds check (mirror of `Vec.get_unchecked`,
                        // `vec_method.rs`). GEP field 0 → load data ptr → GEP
                        // elem at idx → load, skipping `emit_split_bounds_check`.
                        // UB on out-of-range; the unsafe-block requirement is
                        // enforced upstream by `unsafe_lint`. Reaching here
                        // means that check already passed.
                        "get_unchecked" => {
                            if args.is_empty() {
                                return Err(
                                    "Slice.get_unchecked requires an index argument".to_string()
                                );
                            }
                            let ptr_ty = self.context.ptr_type(AddressSpace::default());
                            let elem_ty = *self.slice_elem_types.get(name.as_str()).unwrap();
                            let idx_val = self.compile_expr(&args[0].value)?.into_int_value();
                            let data_pp = self
                                .builder
                                .build_struct_gep(slice_ty, slice_ptr, 0, "s.uchk.data.pp")
                                .unwrap();
                            let data = self
                                .builder
                                .build_load(ptr_ty, data_pp, "s.uchk.data")
                                .unwrap()
                                .into_pointer_value();
                            let elem_ptr = unsafe {
                                self.builder
                                    .build_gep(elem_ty, data, &[idx_val], "s.uchk.elem.ptr")
                                    .unwrap()
                            };
                            let val = self
                                .builder
                                .build_load(elem_ty, elem_ptr, "s.uchk.elem")
                                .unwrap();
                            return Ok(val);
                        }
                        // `Slice[T].binary_search(x) -> Option[i64]`. Same
                        // algorithm as the Vec path; the only difference is the
                        // 2-field `{ptr, len}` slice header (no `cap`). Shares
                        // `compile_binary_search`, so the duplicate-key index
                        // matches the interpreter exactly.
                        "binary_search" => {
                            if args.len() != 1 {
                                return Err("Slice.binary_search requires 1 argument".to_string());
                            }
                            let elem_name = self.vec_elem_type_name(name).ok_or_else(|| {
                                "Slice.binary_search: could not resolve the element type \
                                 in codegen"
                                    .to_string()
                            })?;
                            let elem_ty = *self.slice_elem_types.get(name.as_str()).unwrap();
                            let ptr_ty = self.context.ptr_type(AddressSpace::default());
                            let data = {
                                let p = self
                                    .builder
                                    .build_struct_gep(slice_ty, slice_ptr, 0, "bs.s.data.p")
                                    .unwrap();
                                self.builder
                                    .build_load(ptr_ty, p, "bs.s.data")
                                    .unwrap()
                                    .into_pointer_value()
                            };
                            let len = {
                                let p = self
                                    .builder
                                    .build_struct_gep(slice_ty, slice_ptr, 1, "bs.s.len.p")
                                    .unwrap();
                                self.builder
                                    .build_load(i64_t, p, "bs.s.len")
                                    .unwrap()
                                    .into_int_value()
                            };
                            return self
                                .compile_binary_search(data, len, elem_ty, &elem_name, &args[0]);
                        }
                        _ => {
                            return Err(format!(
                                "codegen: no handler for slice method '{}' on '{}'",
                                method, name
                            ));
                        }
                    }
                }
                // Map methods
                if self.map_key_types.contains_key(name.as_str()) {
                    let name = name.clone();
                    return self.compile_map_method(&name, method, args);
                }
                // Set methods
                if self.set_elem_types.contains_key(name.as_str()) {
                    let name = name.clone();
                    return self.compile_set_method(&name, method, args);
                }
                // HTTP handler ABI trampoline (2026-05-09): `Request.path()`
                // and `Request.method()`. Request is an opaque-ptr value
                // (F2) wrapping the runtime's `*const KaracHttpRequest`.
                // Both methods round-trip through runtime externs that
                // return a borrowed `*const c_char`; we copy the bytes into
                // a fresh Kāra String per call so the resulting value
                // outlives the request struct (which the runtime drops
                // after the handler returns).
                if matches!(self.var_type_names.get(name.as_str()), Some(n) if n == "Request")
                    && (method == "path" || method == "method")
                {
                    let name = name.clone();
                    return self.compile_request_string_method(&name, method);
                }
                if matches!(self.var_type_names.get(name.as_str()), Some(n) if n == "Request")
                    && method == "body"
                {
                    let name = name.clone();
                    return self.compile_request_body(&name);
                }
                // `Request.header(name)` — case-insensitive lookup
                // through `karac_runtime_http_request_header`; returns
                // `Option[String]` with `Some(value)` on hit, `None` on
                // miss. Args[0] is the header name (`String`); the
                // payload's data ptr + len round-trip through the FFI.
                if matches!(self.var_type_names.get(name.as_str()), Some(n) if n == "Request")
                    && method == "header"
                    && args.len() == 1
                {
                    let name = name.clone();
                    return self.compile_request_header(&name, &args[0].value);
                }
                // `Request.headers()` / `Request.query()` — full-map
                // iteration returning `Vec[(String, String)]`. Both walk
                // the runtime's count + indexed key/val accessors, copying
                // each borrowed cstring into a fresh owned String (phase-8
                // line 13). `query()` parameters are percent-decoded
                // runtime-side; `headers()` keys are hyper-normalized
                // lowercase.
                if matches!(self.var_type_names.get(name.as_str()), Some(n) if n == "Request")
                    && (method == "headers" || method == "query")
                    && args.is_empty()
                {
                    let name = name.clone();
                    let kind = if method == "headers" {
                        super::http::RequestPairsKind::Headers
                    } else {
                        super::http::RequestPairsKind::Query
                    };
                    return self.compile_request_pairs(&name, kind);
                }
                // Phase-8 line 17 — `Client.get(url)` / `Client.post(url,
                // body)` codegen dispatch. Receiver `c` is `ref self`,
                // an empty `Client { }` struct; the runtime extern does
                // the real synchronous-HTTP work via `ureq`. Returns
                // `Result[Response, HttpError]` packed into the seeded
                // 5-word Result enum (`tag, w0=status, w1..w3=body /
                // err.message`).
                if matches!(self.var_type_names.get(name.as_str()), Some(n) if n == "Client")
                    && (method == "get" || method == "post")
                {
                    return self.compile_client_http_method(method, args);
                }
                // Phase-8 line 24 — `Client.request(method, url)`
                // chained-builder entrypoint. Returns a `RequestBuilder
                // { handle: i64 }` wrapping a runtime-side
                // `HTTP_BUILDERS` entry; subsequent `.header(...) /
                // .body(...) / .timeout(...) / .send()` chain through
                // the handle-based runtime externs.
                if matches!(self.var_type_names.get(name.as_str()), Some(n) if n == "Client")
                    && method == "request"
                {
                    return self.compile_client_request_builder(args);
                }
                // Phase-8 line 24 — `RequestBuilder` chained methods
                // (`.header / .body / .timeout / .send`). Configuration
                // methods route through `compile_request_builder_setter`
                // (handle stays the same, runtime entry mutates); `.send()`
                // routes through `compile_request_builder_send` (consumes
                // the handle and packs the result).
                if matches!(self.var_type_names.get(name.as_str()), Some(n) if n == "RequestBuilder")
                {
                    if method == "header" || method == "body" || method == "timeout" {
                        let name = name.clone();
                        return self.compile_request_builder_setter(&name, method, args);
                    }
                    if method == "send" && args.is_empty() {
                        let name = name.clone();
                        return self.compile_request_builder_send(&name);
                    }
                }
                // Phase-8 line 17 slice 3 — `Response.status() / .body()`
                // and `HttpError.message()`. Stdlib stubs are
                // `#[compiler_builtin]` so the bodies are never compiled;
                // these arms emit direct field extractions on the
                // receiver's struct value. `status` is i64 — passthrough.
                // `body` / `message` are owned-String returns and route
                // through `karac_string_clone` so the caller's String
                // doesn't alias the receiver's field (a subsequent
                // `Drop` of either would double-free otherwise).
                // `body` / `text` clone the entity as a `String`; `bytes`
                // clones the same buffer as `Vec[u8]` (phase-8 line 32) —
                // the buffers are layout-identical (`{ptr, len, cap}`), so
                // all three route through `compile_response_accessor`; the
                // binding's surface type (String vs Vec[u8]) comes from the
                // typechecker, not the cloned aggregate.
                if matches!(self.var_type_names.get(name.as_str()), Some(n) if n == "Response")
                    && matches!(method, "status" | "body" | "bytes")
                    && args.is_empty()
                {
                    let name = name.clone();
                    return self.compile_response_accessor(&name, method);
                }
                // Phase-8 line 39 — `Response.header(name)` →
                // `Option[String]`. Distinct from the no-arg accessors
                // above: it takes the header name and routes through
                // `compile_response_header`, which reads the hidden
                // `headers` handle off the Response and calls the runtime
                // `HTTP_RESPONSE_HEADERS` side-table lookup
                // (case-insensitive, RFC 7230 §3.2).
                if matches!(self.var_type_names.get(name.as_str()), Some(n) if n == "Response")
                    && method == "header"
                    && args.len() == 1
                {
                    let name = name.clone();
                    return self.compile_response_header(&name, &args[0].value);
                }
                // Phase-8 line 39 follow-up — `Response.headers()` →
                // `Vec[(String, String)]` (full-map iteration over the
                // captured response headers, mirror of `Request.headers()`).
                // Routes through `compile_response_pairs`, which reads the
                // hidden headers handle and drives the runtime count +
                // key_at/val_at iteration accessors.
                if matches!(self.var_type_names.get(name.as_str()), Some(n) if n == "Response")
                    && method == "headers"
                    && args.is_empty()
                {
                    let name = name.clone();
                    return self.compile_response_pairs(&name);
                }
                if matches!(self.var_type_names.get(name.as_str()), Some(n) if n == "HttpError")
                    && method == "message"
                    && args.is_empty()
                {
                    let name = name.clone();
                    return self.compile_http_error_message(&name);
                }
                // `std.json` codegen-side wiring (phase-8 line 435):
                // `j.stringify()` on a Kāra-side `Json` enum value.
                // Loads the receiver's four enum words, dispatches
                // through the synthesized `__karac_json_kara_to_ffi`
                // walker, calls `karac_runtime_json_stringify`, and
                // copies the result into a fresh Kāra String.
                if method == "stringify"
                    && args.is_empty()
                    && matches!(self.var_type_names.get(name.as_str()), Some(n) if n == "Json")
                {
                    let recv_val = self.compile_expr(object)?;
                    return self.compile_json_stringify(recv_val);
                }
            }
        }

        // `std.json` codegen-side wiring (phase-8 line 435) —
        // non-identifier-receiver path: `Json.Object([...]).stringify()`,
        // `Json.Array([...]).stringify()`, etc. The receiver is an
        // expression that evaluates to a Json enum value; we compile it
        // to its struct value and feed it through the same lowering
        // path as the identifier case.
        if method == "stringify" && args.is_empty() && self.expr_is_json_value(object) {
            let recv_val = self.compile_expr(object)?;
            return self.compile_json_stringify(recv_val);
        }

        // `Atomic[T].load(ord)` / `Atomic[T].store(value, ord)` —
        // compiler-builtin dispatch for the transparent Atomic wrapper.
        // Two receiver shapes supported:
        //   1. Identifier `a` where `var_type_names["a"] == "Atomic"`
        //      (populated by the let-stmt Atomic-RHS recognizer in
        //      `compile_stmt`).
        //   2. FieldAccess `c.count` where struct `Counter`'s `count`
        //      field has declared type `Atomic[T]` (recorded in
        //      `struct_field_type_names`). This is the shape the
        //      `karac migrate --atomic` consumer-rewrite emits
        //      (L215c-cons), so the migration tool's output compiles
        //      under codegen without further hand-conversion.
        // Both shapes route through `compile_atomic_method`, which
        // resolves the receiver's storage pointer + element LLVM type,
        // pattern-matches the trailing `MemoryOrdering.X` qualified-
        // variant arg into an `inkwell::AtomicOrdering`, and emits
        // `load atomic` / `store atomic`.
        if matches!(
            method,
            "load"
                | "store"
                | "fetch_add"
                | "fetch_sub"
                | "swap"
                | "fetch_and"
                | "fetch_or"
                | "fetch_xor"
                | "compare_exchange"
        ) && self.is_atomic_receiver(object)
        {
            return self.compile_atomic_method(object, method, args);
        }

        // Phase 6 "Channel AOT codegen lowering": `Sender.send/clone` and
        // `Receiver.recv/try_recv` on a channel-end receiver. `Sender`/
        // `Receiver` are empty stdlib structs (no impl bodies), so this must
        // intercept BEFORE the user-impl dispatch below — otherwise the
        // qualified `Sender.send` lookup misses and the call falls through to
        // a "no such method" error. The gate is the presence of a
        // typechecker-recorded `channel_elem_types` entry at this call span:
        // only `infer_channel_method` populates that table, so an entry is an
        // unambiguous, scope-stable "this is a channel op" signal (the
        // `var_type_names` receiver-type lookup is unreliable here — the
        // statement-hoisting pre-pass binds channel ends then resets
        // `var_type_names` before this method-call pass runs).
        if self
            .channel_elem_types
            .contains_key(&(call_span.offset, call_span.length))
        {
            return self.compile_channel_method(object, method, args, call_span);
        }

        // User impl-block method on a struct receiver: route `obj.method(args)`
        // through the `Type.method` function emitted by the impl-block pass.
        // Requires knowing the object's declared type; the typechecker stashes
        // it via `var_type_names` for struct-kind locals.
        if let Some(receiver_type) = self.inferred_receiver_type(object) {
            let qualified = format!("{}.{}", receiver_type, method);
            if let Some(fn_val) = self.module.get_function(&qualified) {
                // Inspect the resolved fn's first param to decide the receiver
                // calling convention: pointer-typed (ref self / mut ref self)
                // means pass the address of the receiver's storage; struct-
                // typed (owned self) means pass the value. Mismatch silently
                // miscompiles, which is exactly what shipped before this slice.
                let first_param_is_ptr = fn_val
                    .get_type()
                    .get_param_types()
                    .first()
                    .map(|t| matches!(t, BasicMetadataTypeEnum::PointerType(_)))
                    .unwrap_or(false);
                // OWNED self on a SHARED receiver is ALSO ptr-typed at the
                // LLVM level (shared types lower to the heap pointer), but
                // it expects the heap pointer BY VALUE — one indirection
                // less than the ref-self convention (whose body loads the
                // param to reach the heap ptr; see `compile_function`'s
                // `inner_type_of_ref` registration). The LLVM param type
                // can't discriminate the two, so consult the source-level
                // ref flag recorded by `declare_function`. Before this,
                // `node.step()` with `fn step(self)` passed the STACK SLOT
                // address: the callee's entry rc_inc then incremented a
                // stack word as if it were a refcount header and every
                // field GEP was one indirection off — the owned-`self`
                // receiver-move segfault (bugs.md entry, 2026-06-05).
                let first_param_is_ref = self
                    .fn_param_ref
                    .get(&qualified)
                    .and_then(|flags| flags.first().copied())
                    .unwrap_or(false);
                // Receiver storage name for the ptr-self ABI. Both `obj`
                // (Identifier) and `self` (SelfValue, registered under the
                // synthesized "self" param) resolve to a data pointer; any
                // other shape has no stable storage to address.
                let recv_storage_name: Option<&str> = match &object.kind {
                    ExprKind::Identifier(var_name) => Some(var_name.as_str()),
                    ExprKind::SelfValue => Some("self"),
                    _ => None,
                };
                let receiver_arg: BasicMetadataValueEnum<'ctx> = if first_param_is_ptr
                    && !first_param_is_ref
                    && self.shared_types.contains_key(&receiver_type)
                {
                    // Owned shared `self`: the heap pointer by value. The
                    // callee's entry emits its own receive-inc ("caller
                    // keeps its reference"), so no caller-side count
                    // change here. `compile_expr` on an Identifier loads
                    // the slot, which holds exactly the heap ptr.
                    self.compile_expr(object)?.into()
                } else if first_param_is_ptr {
                    if let Some(ptr) = recv_storage_name.and_then(|n| self.get_data_ptr(n)) {
                        ptr.into()
                    } else {
                        // Non-identifier / non-self receiver into a ref-self
                        // method: unsupported in v1 (would require materializing
                        // a temporary alloca). Fall through to compile_expr;
                        // mismatched ABI may surface at link time.
                        self.compile_expr(object)?.into()
                    }
                } else {
                    self.compile_expr(object)?.into()
                };
                // Positional-arg ref/slice lowering — mirrors the free-fn
                // path in `compile_call` (call_dispatch.rs). Before this, the
                // method path compiled every non-receiver arg by *value* and
                // pushed it, so a `ref`/`mut ref` struct param (declared `ptr`)
                // received a `{ ... }` struct value and module verification
                // rejected the call (B-2026-06-12-8). The receiver occupies
                // param slot 0 (`self`), so source arg `i` maps to declared
                // param slot `i + 1` in `fn_param_ref` / `fn_param_slice_elem`
                // (both keyed by the qualified `Type.method` name and built
                // from `func.params`, whose element 0 is the receiver).
                let ref_flags = self
                    .fn_param_ref
                    .get(&qualified)
                    .cloned()
                    .unwrap_or_default();
                let slice_elems = self
                    .fn_param_slice_elem
                    .get(&qualified)
                    .cloned()
                    .unwrap_or_default();
                let mut compiled_args: Vec<BasicMetadataValueEnum<'ctx>> = vec![receiver_arg];
                for (i, a) in args.iter().enumerate() {
                    let pidx = i + 1;
                    let is_ref = ref_flags.get(pidx).copied().unwrap_or(false);
                    if is_ref {
                        // Identifier place — pass its data pointer.
                        if let ExprKind::Identifier(var_name) = &a.value.kind {
                            if let Some(ptr) = self.get_data_ptr(var_name) {
                                compiled_args.push(ptr.into());
                                continue;
                            }
                        }
                        // `vec[idx]` borrow — pass the element pointer in place
                        // (no shallow-copy + drop double-free).
                        if let Some(elem_ptr) = self.ref_arg_index_borrow_ptr(&a.value)? {
                            compiled_args.push(elem_ptr.into());
                            continue;
                        }
                        // Borrow-returning call in ref-arg position — forward
                        // the raw `-> ref T` borrow pointer (bypass the
                        // direct-use intercept that would load the pointee).
                        if self.is_borrow_returning_call_expr(&a.value) {
                            let prev = self.compiling_ref_return_let_rhs;
                            self.compiling_ref_return_let_rhs = true;
                            let ptr = self.compile_expr(&a.value);
                            self.compiling_ref_return_let_rhs = prev;
                            compiled_args.push(ptr?.into());
                            continue;
                        }
                    }
                    // `Slice[T]` / `mut Slice[T]` param: synthesize the
                    // `{ ptr, i64 }` header from an Array/Vec/slice arg.
                    if let Some(Some(elem_ty)) = slice_elems.get(pidx).cloned() {
                        if let Some(slice_val) = self.coerce_to_slice(&a.value, elem_ty)? {
                            compiled_args.push(slice_val.into());
                            continue;
                        }
                    }
                    if is_ref {
                        // Rvalue ref path: a non-place arg (literal, call
                        // return, arithmetic) bound to a `ref T` param.
                        // Materialize into a stack temp so the callee receives
                        // the `ptr` ABI its signature declares; queue the
                        // temp's cleanup (the callee only borrows). Mirrors the
                        // free-fn rvalue-ref arm in `compile_call`.
                        let val = self.compile_expr(&a.value)?;
                        let cur_fn = self
                            .builder
                            .get_insert_block()
                            .and_then(|bb| bb.get_parent())
                            .expect("compile_method_call inside a function context");
                        let temp = self.create_entry_alloca(
                            cur_fn,
                            &format!("ref_rvalue_marg{i}"),
                            val.get_type(),
                        );
                        self.builder.build_store(temp, val).unwrap();
                        self.queue_ref_rvalue_arg_cleanup(temp, val, &a.value);
                        compiled_args.push(temp.into());
                        continue;
                    }
                    let val = self.compile_expr(&a.value)?;
                    // `Option[shared T]` arg-share discipline — mirrors
                    // the free-fn call path in `compile_call`: a tracked
                    // Identifier binding gets a tag+null-guarded inner
                    // inc so the callee receives an independent +1 (its
                    // param `RcDecOption` decs at exit; the caller's
                    // binding keeps its own +1 for its scope-exit dec);
                    // a FieldAccess arg reading an `Option[shared T]`
                    // field gets the loaded inner inc'd. Without these,
                    // reusing a binding after passing it — `m.total(c);
                    // m.total(c)` — read freed memory (2026-06-05 probe,
                    // pre-existing on the conventional ABI).
                    self.share_option_shared_ref_for_arg(&a.value);
                    self.share_option_shared_field_ref_for_arg(&a.value, val);
                    // B-2026-06-12-10: register the caller-side drop for an inline
                    // owned-aggregate arg (enum-variant constructor / tuple /
                    // struct literal) — the lexer's `self.make_spanned(Token.V(…))`
                    // reaches here, not the free-fn `compile_call` path. Shared
                    // helper keeps both arg loops in lockstep.
                    self.track_inline_owned_aggregate_arg(val, &a.value);
                    // Fresh-heap by-value arg materialization — the method-call
                    // sibling of the #20 arm in `compile_call` (call_dispatch.rs).
                    // A `String`/`Vec` produced by a Call/MethodCall (or a block /
                    // inline-temp-Vec heap index) and passed DIRECTLY by value to a
                    // method — `lx.ident_matches("Fn".to_string())` — has no
                    // consuming binding, and an owned `String`/`Vec` by-value param
                    // is NOT freed by the callee (it lands in `owned_vecstr_params`
                    // for retaining-consume deep-copy, never a callee-side
                    // `track_vec_var`), so the temp orphaned and leaked one buffer
                    // per call (B-2026-06-20: the self-host string-eq method leak).
                    // `materialize_owned_temp` self-guards on the Vec/String LLVM
                    // shape (+ the `owned_temp_drops` hint for Map), so non-heap
                    // args are a no-op; `rhs_stages_fstr_acc` excludes a struct/enum
                    // `.to_string()` (its f-string acc already owns a caller-scope
                    // cleanup). The free-fn arm's full rationale applies verbatim.
                    let is_block_arg = matches!(
                        &a.value.kind,
                        ExprKind::Block(_)
                            | ExprKind::Seq(_)
                            | ExprKind::Unsafe(_)
                            | ExprKind::LabeledBlock { .. }
                    );
                    // B-2026-07-02-6 follow-on: collection-literal args share
                    // #20's orphaned-fresh-heap shape (see the free-fn arm).
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
                    if is_block_arg || is_fresh_heap_call_arg {
                        self.materialize_owned_temp(
                            val,
                            (a.value.span.offset, a.value.span.length),
                        );
                    }
                    // A fresh bare-`shared` (RC-box) call / variant-ctor result
                    // passed by value: the callee inc/decs net-zero, so the caller
                    // still owns the temp's +1 and must release it — the bare-shared
                    // sibling of the arm above (`fresh_arg_bare_shared_heap_type`
                    // self-excludes a `g(make())` passthrough chain).
                    if val.is_pointer_value() {
                        if let Some(heap_type) = self.fresh_arg_bare_shared_heap_type(&a.value) {
                            self.track_rc_var(
                                "__owned_arg_tmp",
                                val.into_pointer_value(),
                                heap_type,
                            );
                        }
                    }
                    compiled_args.push(val.into());
                }
                // Niche-ABI pack/unpack at the `obj.method(...)` boundary
                // — the receiver occupies position 0 (`self`, never an
                // Option, never a niche position) so source args line up
                // with declared params 1..N.
                self.pack_niche_abi_args(&qualified, &mut compiled_args);
                // Scalar width coercion at the method-arg boundary —
                // mirrors the free-fn site in `call_dispatch.rs`
                // (`p.scale(2)` against `fn scale(self, k: i8)` would
                // otherwise emit a width-mismatched call). See
                // `coerce_scalar_to_type`.
                self.coerce_args_to_fn_params(fn_val, &mut compiled_args);
                let call_site = self
                    .builder
                    .build_call(fn_val, &compiled_args, "usermethod")
                    .unwrap();
                let basic_val = call_site.try_as_basic_value();
                return if basic_val.is_instruction() {
                    // Void-return placeholder: callee returns unit, so fill the
                    // expression slot with const-0 i64. NOT a dispatch fall-through.
                    Ok(self.context.i64_type().const_int(0, false).into())
                } else {
                    Ok(self.unpack_niche_abi_ret(&qualified, basic_val.unwrap_basic()))
                };
            }
        }

        // Non-identifier receiver of Vec / String type — e.g.
        // `list_primes_under(n).len()`. Compile the receiver to a `{ptr,
        // len, cap}` struct value, then service the read-only Vec methods
        // (`len`, `is_empty`) via direct field extraction. Methods that
        // would mutate the receiver (`push`, `sort`, etc.) don't make
        // semantic sense on a temporary — the mutation would be lost when
        // the temp goes out of scope at the end of the statement — so
        // those keep falling through to the dispatch-fail Err below.
        //
        // For element-type-aware Vec methods (`contains`, `get`, `iter`),
        // a follow-up slice can materialize the value to a temporary
        // alloca + synthesize a name + register elem_ty from the typed
        // AST. Today's narrow scope: just `len` and `is_empty`, which
        // are element-type-agnostic.
        // Read-only `len` / `is_empty` on a borrow-LOCAL receiver — a
        // `let n = name_of(u);` / chained borrow result (B-2026-06-07-5).
        // Such a binding is registered in `ref_params` (the let-RHS path
        // stores it as a `ptr` and derefs on use), so `compile_expr(n)`
        // yields the same `{ptr,len,cap}` struct a temp receiver does, and
        // the field-extraction below services it. A ref *parameter* receiver
        // (`s: ref String`) is dispatched by an earlier String arm and never
        // reaches here, so this only rescues the let-bound borrows that
        // otherwise fell through to the dispatch-fail error below. Owned
        // String/Vec locals are likewise handled earlier (via the
        // string/var-type paths); the `== vec_ty` struct guard makes a
        // non-`{ptr,len,cap}` borrow (`ref i64`) fall through safely.
        let borrow_local_recv =
            matches!(&object.kind, ExprKind::Identifier(n) if self.ref_params.contains_key(n));
        if (!matches!(&object.kind, ExprKind::Identifier(_)) || borrow_local_recv)
            && matches!(method, "len" | "is_empty")
        {
            let recv_val = self.compile_expr(object)?;
            if let BasicValueEnum::StructValue(sv) = recv_val {
                let vec_ty = self.vec_struct_type();
                if sv.get_type() == vec_ty {
                    // General owned-temp tracking, slice 3 (method-chain
                    // receiver temps): when the receiver is a *fresh-owned*
                    // Vec/String temporary (`make_vec().len()`), `len` /
                    // `is_empty` borrow it read-only — so the caller owns the
                    // temp and must drop it. Without this its heap buffer
                    // leaks (the field-extract below reads `len` and discards
                    // the struct, orphaning `data`). Route the receiver value
                    // through the owned-temp chokepoint so a `FreeVecBuffer`
                    // (with the element type from `owned_temp_drops`, closing
                    // nested-heap leaks) drains at scope exit. Gated to
                    // Call/MethodCall: a *place*-expression receiver
                    // (`obj.items.len()`, `arr[0].len()`) reloads a buffer an
                    // existing binding owns, which a second free would
                    // double-free; `expr_yields_fresh_owned_temp` excludes
                    // those (and the `cap > 0` guard in `FreeVecBuffer` keeps
                    // a non-owning / borrowed value safe regardless).
                    if self.expr_yields_fresh_owned_temp(object) {
                        self.materialize_owned_temp(
                            recv_val,
                            (object.span.offset, object.span.length),
                        );
                    }
                    let i64_t = self.context.i64_type();
                    let len_val = self
                        .builder
                        .build_extract_value(sv, 1, "tmp.vec.len")
                        .unwrap()
                        .into_int_value();
                    return Ok(match method {
                        "len" => len_val.into(),
                        "is_empty" => self
                            .builder
                            .build_int_compare(
                                inkwell::IntPredicate::EQ,
                                len_val,
                                i64_t.const_zero(),
                                "tmp.vec.is_empty",
                            )
                            .unwrap()
                            .into(),
                        _ => unreachable!(),
                    });
                }
                // Slice-header receiver — `s.bytes().len()`, `slice.len()`
                // where the receiver is a method-chain result. `bytes()` (and
                // the other zero-copy views) return the `{ptr, i64}` slice
                // header, not the `{ptr,len,cap}` Vec struct, so the `vec_ty`
                // branch above misses them and the chain fell through to the
                // dispatch-fail error (B surfaced by kata-katas #722 bench
                // harness's `out[k].bytes().len()`). A slice is a borrowed
                // view that owns no buffer, so there is NO owned-temp drop
                // here — just extract `len` (field 1, same index as the Vec).
                if sv.get_type() == self.slice_struct_type() {
                    let i64_t = self.context.i64_type();
                    let len_val = self
                        .builder
                        .build_extract_value(sv, 1, "tmp.slice.len")
                        .unwrap()
                        .into_int_value();
                    return Ok(match method {
                        "len" => len_val.into(),
                        "is_empty" => self
                            .builder
                            .build_int_compare(
                                inkwell::IntPredicate::EQ,
                                len_val,
                                i64_t.const_zero(),
                                "tmp.slice.is_empty",
                            )
                            .unwrap()
                            .into(),
                        _ => unreachable!(),
                    });
                }
            }
        }

        // Phase-8 line 24 — `RequestBuilder` non-identifier receiver
        // dispatch. The chained-builder shape
        // `c.request("GET", url).header(...).timeout(...).send()` has
        // each call's receiver as the prior call's return value (a
        // MethodCall expr, not an Identifier). Detect the receiver's
        // LLVM struct type at the seeded `RequestBuilder` shape, stash
        // it in a synthesized alloca, register the synth name in
        // `var_type_names`, then re-dispatch through the identifier
        // path so the existing setter / send arms fire.
        if !matches!(&object.kind, ExprKind::Identifier(_))
            && matches!(method, "header" | "body" | "timeout" | "send")
        {
            let rb_ty = self.struct_types.get("RequestBuilder").copied();
            if let Some(rb_ty) = rb_ty {
                let recv_val = self.compile_expr(object)?;
                if let BasicValueEnum::StructValue(sv) = recv_val {
                    if sv.get_type() == rb_ty {
                        let fn_val = self.current_fn.ok_or_else(|| {
                            "RequestBuilder chained method call outside fn".to_string()
                        })?;
                        let synth = format!("__rb_tmp_{}", self.indexed_elem_counter);
                        self.indexed_elem_counter += 1;
                        let slot_ptr = self.create_entry_alloca(fn_val, &synth, rb_ty.into());
                        self.builder.build_store(slot_ptr, sv).unwrap();
                        self.variables.insert(
                            synth.clone(),
                            super::VarSlot {
                                ptr: slot_ptr,
                                ty: rb_ty.into(),
                            },
                        );
                        self.var_type_names
                            .insert(synth.clone(), "RequestBuilder".to_string());
                        let synth_expr = Expr {
                            kind: ExprKind::Identifier(synth.clone()),
                            span: object.span.clone(),
                        };
                        let result = self.compile_method_call(&synth_expr, method, args, call_span);
                        self.variables.remove(&synth);
                        self.var_type_names.remove(&synth);
                        return result;
                    }
                }
            }
        }

        // `std.tracing` builder-chain non-identifier receiver dispatch.
        // `LogEvent.info(msg).with_field(k, v).in_span(id)` and
        // `Span.root(n, id).child(c, id).with_field(k, v)` chain owned-self
        // builders, so each call's receiver is the prior call's return
        // value (a `Call` / `MethodCall` expr, not an Identifier). Same
        // shape as the `RequestBuilder` block above: compile the receiver,
        // match its LLVM struct type against the seeded `Span` / `LogEvent`
        // layouts (`with_field` lives on both, so the type — not the method
        // name — disambiguates), stash it in a synthesized alloca, and
        // re-dispatch through the identifier path so the compiled
        // `Type.method` body fires. Gated on the tracing builder method
        // names so an unrelated non-identifier `.with_field(...)` on a user
        // type whose value isn't a tracing struct falls through untouched.
        if !matches!(&object.kind, ExprKind::Identifier(_))
            && matches!(method, "with_field" | "child" | "in_span")
        {
            let recv_val = self.compile_expr(object)?;
            if let BasicValueEnum::StructValue(sv) = recv_val {
                let sv_ty = sv.get_type();
                let matched = ["LogEvent", "Span"]
                    .into_iter()
                    .find(|name| self.struct_types.get(*name) == Some(&sv_ty));
                if let Some(type_name) = matched {
                    let fn_val = self
                        .current_fn
                        .ok_or_else(|| "tracing builder chain outside fn".to_string())?;
                    let synth = format!("__trace_tmp_{}", self.indexed_elem_counter);
                    self.indexed_elem_counter += 1;
                    let slot_ptr = self.create_entry_alloca(fn_val, &synth, sv_ty.into());
                    self.builder.build_store(slot_ptr, sv).unwrap();
                    self.variables.insert(
                        synth.clone(),
                        super::VarSlot {
                            ptr: slot_ptr,
                            ty: sv_ty.into(),
                        },
                    );
                    self.var_type_names
                        .insert(synth.clone(), type_name.to_string());
                    let synth_expr = Expr {
                        kind: ExprKind::Identifier(synth.clone()),
                        span: object.span.clone(),
                    };
                    let result = self.compile_method_call(&synth_expr, method, args, call_span);
                    self.variables.remove(&synth);
                    self.var_type_names.remove(&synth);
                    return result;
                }
            }
        }

        // ── Ambient built-in resource methods (BuiltinDefault) ─────
        // Last resort before the dispatch-fail error: lower the ambient
        // resource methods (`env.set`, `clock.now`, ...) the interpreter
        // services via `dispatch_builtin_resource_method_with_values`
        // (`src/interpreter/resource_method.rs`). The receiver is a bare
        // lowercase alias (`env`, `clock`) — see the interpreter's alias
        // table in `src/interpreter/method_call.rs` — that is NOT a bound
        // local; a user variable named `env` shadows the ambient resource,
        // so guard on `self.variables`. User `with_provider` overrides of
        // overridable resources are dispatched earlier via
        // `try_compile_provider_dispatch` (`call_dispatch.rs`), so reaching
        // here means no provider claimed the call.
        if let ExprKind::Identifier(recv) = &object.kind {
            if !self.variables.contains_key(recv) {
                if let Some(resource) = ambient_resource_for_alias(recv) {
                    return self.compile_ambient_resource_method(resource, method, args);
                }
            }
        }

        // Float→int / int→float conversion methods (phase-8 § "Saturating
        // float→int", slice 4 — the codegen for the slice-2 surface). Reaching
        // the fall-through means no impl/user method claimed the call, so a
        // conversion-named method here is the primitive form (a user-defined
        // `to_f32`/`to_f64` on a struct dispatches via the impl-block path above
        // and never reaches here). Semantics match `crate::numeric_conv` (the
        // slice-2 interpreter oracle): `saturating_to_iN` ≡ the `f as iN`
        // saturating cast, `wrapping_to_iN` = modular truncation,
        // `checked_to_iN` → `Option[iN]`, `trunc_to_iN` traps on out-of-range.
        if args.is_empty() {
            if let Some((family, _target, bits, signed)) =
                crate::numeric_conv::parse_float_to_int(method)
            {
                let recv = self.compile_expr(object)?;
                if let BasicValueEnum::FloatValue(fv) = recv {
                    let int_ty = self.int_type_for_bits(bits);
                    return self.emit_float_to_int_conv(fv, family, int_ty, !signed);
                }
            }
            // `i.to_f32()` / `i.to_f64()` — int→float widening (`sitofp`/
            // `uitofp` per the source-integer signedness).
            if method == "to_f32" || method == "to_f64" {
                let src_unsigned = self.expr_is_unsigned_int(object);
                let recv = self.compile_expr(object)?;
                if let BasicValueEnum::IntValue(iv) = recv {
                    let ft = if method == "to_f32" {
                        self.context.f32_type()
                    } else {
                        self.context.f64_type()
                    };
                    let r = if src_unsigned {
                        self.builder.build_unsigned_int_to_float(iv, ft, "to_float")
                    } else {
                        self.builder.build_signed_int_to_float(iv, ft, "to_float")
                    }
                    .unwrap();
                    return Ok(r.into());
                }
            }
        }

        // `<string>.chars().collect()` → materialize a `Vec[char]`. Codegen has
        // no general iterator/`collect` lowering (the chars-iterator value is
        // unsupported, and `collect` on a non-identifier receiver — here the
        // `.chars()` call — falls through to the dispatch-fail error). But the
        // equivalent `for c in <string>.chars() { v.push(c) }` IS fully
        // supported, so lower this idiom to exactly that block and compile it.
        // Surfaced by kata:38 (B-2026-06-18-1). The `.chars()` call is `object`
        // here (the receiver of `collect`); it is reused verbatim as the loop
        // iterable, so no string-receiver shape needs re-synthesizing.
        if method == "collect" && args.is_empty() {
            if let ExprKind::MethodCall {
                method: inner_method,
                args: inner_args,
                ..
            } = &object.kind
            {
                if inner_method == "chars" && inner_args.is_empty() {
                    return self.compile_chars_collect_to_vec(object, call_span);
                }
            }
        }

        // General owned-temp tracking, slice 3b — element-type-aware read
        // methods (`get`/`first`/`last`/`get_unchecked`/`contains`) on a
        // FRESH-TEMP `Vec`/`VecDeque` receiver (`make_vec().get(0)`). Needs the
        // receiver's element type, recorded span-keyed by the typechecker in
        // `temp_recv_elem_types` (unrecoverable from the LLVM `{ptr,len,cap}`
        // shape, which is element-erased). Runs before the String redispatch
        // below; no-ops (returns `Ok(None)`) when there's no recorded element
        // type, so the String path and the diagnostic are untouched.
        if let Some(result) = self.try_compile_freshtemp_vec_read_method(object, method, args)? {
            return Ok(result);
        }

        // Slice 3d sibling — read methods (`get`/`contains_key`/`contains`) on a
        // FRESH-TEMP `Map`/`Set` receiver (`make_map().get(k)`). The handle is a
        // plain `ptr` (no struct shape to detect), so it keys off the
        // typechecker's `temp_recv_mapset_types`; no-ops (`Ok(None)`) when absent.
        if let Some(result) = self.try_compile_freshtemp_mapset_read_method(object, method, args)? {
            return Ok(result);
        }

        // Last-resort before the dispatch-fail error: a String collection
        // method (`split`, `contains`, …) on a **non-identifier** receiver
        // (`"a,b,c".split(",")`, `make_csv().split(",")`). The collection
        // dispatch above is identifier-keyed (it looks the receiver up by name
        // in `vec_elem_types`), so a literal / call-result receiver falls
        // through. Materialize it into a synthetic local and re-route through
        // `compile_vec_method`.
        if let Some(result) = self.try_compile_nonident_collection_method(
            object,
            method,
            args,
            dispatch_key.as_deref(),
        )? {
            return Ok(result);
        }

        // Slice 3j — a USER impl-block method on a FRESH-TEMP (non-identifier)
        // struct receiver (`make_thing().method()`). The identifier-keyed
        // user-impl dispatch above resolves only Identifier / self receivers
        // (`inferred_receiver_type` reads `var_type_names`), so a call-result
        // receiver falls through here even though `Type.method` exists.
        // Materialize the receiver into a synth local and re-dispatch.
        if let Some(result) = self.try_compile_freshtemp_user_method(
            object,
            method,
            args,
            dispatch_key.as_deref(),
            call_span,
        )? {
            return Ok(result);
        }

        let receiver_desc = match &object.kind {
            ExprKind::Identifier(name) => format!("variable '{}'", name),
            _ => "non-identifier receiver".to_string(),
        };
        Err(format!(
            "codegen: no handler for method '{}' on {} (method dispatch fell through; \
             this is a codegen bug — add a dispatcher arm in `compile_method_call` \
             or mark the test `#[ignore]` if the method is genuinely deferred)",
            method, receiver_desc
        ))
    }

    /// Lower `<string>.chars().collect()` to the already-supported
    /// `for c in <string>.chars() { v.push(c) }` build and compile that
    /// (B-2026-06-18-1, kata:38). `chars_call` is the `<string>.chars()`
    /// expression (the `collect` receiver), reused verbatim as the loop
    /// iterable. We synthesize the block
    ///
    /// ```text
    /// { let mut __cas_N: Vec[char] = Vec.new();
    ///   for __casc_N in <string>.chars() { __cas_N.push(__casc_N); }
    ///   __cas_N }
    /// ```
    ///
    /// and hand it to `compile_expr`. The `Vec[char]` annotation makes the
    /// let-binding handler register the element type at codegen time (no
    /// typechecker dependency — see `stmts.rs` let lowering), so `push`
    /// dispatches and the result is a usable `Vec[char]`. Reusing the
    /// existing for-chars + push + block-return paths means no new low-level
    /// Vec/iterator codegen, and the block's move-out gives the caller the
    /// freshly built Vec exactly as a `fn() -> Vec[char]` would.
    fn compile_chars_collect_to_vec(
        &mut self,
        chars_call: &Expr,
        call_span: &crate::token::Span,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let n = self.indexed_elem_counter;
        self.indexed_elem_counter += 1;
        let vec_name = format!("__cas_{}", n);
        let char_name = format!("__casc_{}", n);
        let sp = call_span.clone();

        let ident = |name: &str, sp: &crate::token::Span| Expr {
            kind: ExprKind::Identifier(name.to_string()),
            span: sp.clone(),
        };

        // Vec[char] type annotation.
        let char_ty = TypeExpr {
            kind: TypeKind::Path(PathExpr {
                segments: vec!["char".to_string()],
                generic_args: None,
                span: sp.clone(),
            }),
            span: sp.clone(),
        };
        let vec_char_ty = TypeExpr {
            kind: TypeKind::Path(PathExpr {
                segments: vec!["Vec".to_string()],
                generic_args: Some(vec![GenericArg::Type(char_ty)]),
                span: sp.clone(),
            }),
            span: sp.clone(),
        };

        // `Vec.new()`
        let vec_new = Expr {
            kind: ExprKind::Call {
                callee: Box::new(Expr {
                    kind: ExprKind::Path {
                        segments: vec!["Vec".to_string(), "new".to_string()],
                        generic_args: None,
                    },
                    span: sp.clone(),
                }),
                args: vec![],
            },
            span: sp.clone(),
        };

        // `let mut __cas_N: Vec[char] = Vec.new();`
        let let_stmt = Stmt {
            kind: StmtKind::Let {
                is_mut: true,
                pattern: Pattern {
                    kind: PatternKind::Binding(vec_name.clone()),
                    span: sp.clone(),
                },
                ty: Some(vec_char_ty),
                value: vec_new,
            },
            span: sp.clone(),
        };

        // `__cas_N.push(__casc_N)`
        let push_call = Expr {
            kind: ExprKind::MethodCall {
                object: Box::new(ident(&vec_name, &sp)),
                method: "push".to_string(),
                turbofish: None,
                args: vec![CallArg {
                    label: None,
                    mut_marker: false,
                    value: ident(&char_name, &sp),
                    span: sp.clone(),
                }],
                args_close_span: sp.clone(),
            },
            span: sp.clone(),
        };

        // `for __casc_N in <string>.chars() { __cas_N.push(__casc_N); }`
        let for_stmt = Stmt {
            kind: StmtKind::Expr(Expr {
                kind: ExprKind::For {
                    label: None,
                    pattern: Pattern {
                        kind: PatternKind::Binding(char_name.clone()),
                        span: sp.clone(),
                    },
                    iterable: Box::new(chars_call.clone()),
                    body: Block {
                        stmts: vec![Stmt {
                            kind: StmtKind::Expr(push_call),
                            span: sp.clone(),
                        }],
                        final_expr: None,
                        span: sp.clone(),
                    },
                    attributes: vec![],
                },
                span: sp.clone(),
            }),
            span: sp.clone(),
        };

        // `{ <let>; <for>; __cas_N }`
        let block = Expr {
            kind: ExprKind::Block(Block {
                stmts: vec![let_stmt, for_stmt],
                final_expr: Some(Box::new(ident(&vec_name, &sp))),
                span: sp.clone(),
            }),
            span: sp.clone(),
        };

        self.compile_expr(&block)
    }

    /// Materialize a **non-identifier String** method receiver into a synthetic
    /// local, then route through the identifier-keyed collection dispatch
    /// (`compile_vec_method`). Closes the Weave non-identifier-receiver gap for
    /// String collection methods like `"a,b,c".split(",")` /
    /// `make_csv().split(",")` — the receiver-shape-keyed dispatch in
    /// `compile_method_call` only fires for `Identifier` receivers, so a literal
    /// or call-result String receiver fell through to "no handler". (The
    /// call-result `.to_string()` case already works via the receiver-shape-
    /// agnostic `String.to_string` arm.) Returns `Ok(None)` when the receiver
    /// isn't a String — the caller falls through to its diagnostic, so this is a
    /// pure addition that can't change existing cases.
    ///
    /// Scoped to String deliberately: the receiver type is resolved from the
    /// `Type.method` callee key's receiver segment (span-independent — robust),
    /// and String needs no element type. A non-identifier **Vec** receiver
    /// (`make_vec().contains(x)`) additionally needs the element type, which is
    /// only available span-keyed in `owned_temp_drops` — and a `Call` receiver's
    /// `object.span` is the callee-name span, not the call-expr span those
    /// tables use, so it doesn't resolve. That's a separate follow-on (tracked
    /// in phase-7-codegen.md "non-identifier receiver"); it errors loudly today
    /// exactly as before, no regression.
    ///
    /// Drop: the receiver temp's free is owned by the existing statement-level
    /// owned-temp machinery (the RHS sub-expression's `owned_temp_drops` entry
    /// queues it), so the synth slot is NOT separately drop-tracked — tracking
    /// it too double-frees a heap receiver like `make_csv().split(",")` (proven:
    /// a tracked variant SIGABRT'd at scope exit; the untracked one is leak- and
    /// double-free-clean under `leaks` + ASAN). IR parity with the one-line
    /// `let s = <recv>; s.split(",")` workaround.
    fn try_compile_nonident_collection_method(
        &mut self,
        object: &Expr,
        method: &str,
        args: &[CallArg],
        dispatch_key: Option<&str>,
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        // Identifier / self receivers already route through the main dispatch.
        if matches!(&object.kind, ExprKind::Identifier(_) | ExprKind::SelfValue) {
            return Ok(None);
        }
        let span_key = (object.span.offset, object.span.length);

        // Receiver must be a String. Prefer the `Type.method` callee key's
        // receiver segment (span-independent); fall back to the
        // String-typed-expr span set.
        let recv_is_string = dispatch_key
            .and_then(|k| k.rsplit_once('.'))
            .map(|(t, _)| t == "String")
            .unwrap_or(false)
            || self.string_typed_exprs.contains(&span_key);
        if !recv_is_string {
            return Ok(None);
        }

        let cur_fn = self
            .current_fn
            .ok_or_else(|| "method receiver materialization outside fn".to_string())?;
        let val = self.compile_expr(object)?;

        // Store the receiver value into a synthetic slot for dispatch. NOT
        // drop-tracked — see the doc comment (the statement-level owned-temp
        // machinery owns the free; double-tracking double-frees).
        let slot = self.create_entry_alloca(cur_fn, "__recv_tmp", val.get_type());
        self.builder.build_store(slot, val).unwrap();

        let synth = format!("__recv_tmp_{}", self.indexed_elem_counter);
        self.indexed_elem_counter += 1;
        let i8t = self.context.i8_type().into();
        self.variables.insert(
            synth.clone(),
            super::VarSlot {
                ptr: slot,
                ty: val.get_type(),
            },
        );
        self.vec_elem_types.insert(synth.clone(), i8t);
        self.string_vars.insert(synth.clone());
        self.var_type_names
            .insert(synth.clone(), "String".to_string());

        let result = self.compile_vec_method(&synth, slot, method, args);

        // Drop the dispatch-only registrations (unique synth name).
        self.variables.remove(&synth);
        self.var_type_names.remove(&synth);
        self.vec_elem_types.remove(&synth);
        self.string_vars.remove(&synth);

        result.map(Some)
    }

    /// Slice 3j — a USER impl-block method on a FRESH-TEMP (non-identifier)
    /// receiver whose type is a non-shared user struct (`make_thing().method()`,
    /// `build().total()`). The identifier-keyed user-impl dispatch
    /// (`inferred_receiver_type` → `Type.method`) resolves only Identifier / self
    /// receivers, so a call-result receiver falls through to the dispatch-fail
    /// error even though `Type.method` exists — a silent hard error, not a
    /// miscompile. Recover the struct type from the typechecker's `Type.method`
    /// callee key, materialize the receiver value into a synth local, register it
    /// under that struct name (so the recursion's `inferred_receiver_type`
    /// resolves and `get_data_ptr` yields the ptr-self ABI address), drop-track it
    /// **iff `self` is borrowed** (`ref self` / `mut ref self` — the caller owns
    /// the temp; owned `self` moves it into the method, which drops its fields, so
    /// tracking the caller's shallow copy too would double-free the shared heap
    /// buffers), then re-dispatch through the identifier path by recursing into
    /// `compile_method_call` with a synth Identifier receiver (which hits the
    /// user-impl arm *before* reaching this helper again — no infinite recursion).
    ///
    /// Returns `Ok(None)` when the receiver isn't a serviceable fresh-temp user
    /// struct (no callee key, not a known struct, shared, or `Type.method`
    /// absent), so the caller falls through to its own diagnostic — a pure
    /// addition that can't change any existing case. Enum / shared-struct
    /// receivers (heap-pointer self, RC drop) are follow-ons.
    fn try_compile_freshtemp_user_method(
        &mut self,
        object: &Expr,
        method: &str,
        args: &[CallArg],
        dispatch_key: Option<&str>,
        call_span: &crate::token::Span,
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        // Identifier / self receivers already route through the main user-impl
        // dispatch.
        if matches!(&object.kind, ExprKind::Identifier(_) | ExprKind::SelfValue) {
            return Ok(None);
        }
        // Recover the struct type from the `Type.method` callee key.
        let Some(type_name) = dispatch_key
            .and_then(|k| k.rsplit_once('.'))
            .map(|(t, _)| t.to_string())
        else {
            return Ok(None);
        };
        // Accept any user type that carries `impl`-block methods: a non-shared
        // struct, a value enum, or a shared struct/enum (RC). The three differ
        // only in the scope-exit DROP they need for the materialized temp (see
        // the drop-track block below); the DISPATCH is uniform — store the
        // receiver into a synth local and re-enter `compile_method_call` with an
        // Identifier, which resolves the same for all three. The `qualified`
        // function-existence check below is the real "is this a method" gate.
        let is_shared = self.shared_types.contains_key(&type_name);
        let is_value_enum = !is_shared && self.enum_layouts.contains_key(&type_name);
        let is_plain_struct = !is_shared && self.struct_types.contains_key(&type_name);
        if !(is_shared || is_value_enum || is_plain_struct) {
            return Ok(None);
        }
        let qualified = format!("{type_name}.{method}");
        if self.module.get_function(&qualified).is_none() {
            return Ok(None);
        }
        let cur_fn = self
            .current_fn
            .ok_or_else(|| "user-method receiver materialization outside fn".to_string())?;
        let val = self.compile_expr(object)?;
        let slot = self.create_entry_alloca(cur_fn, "__urecv_tmp", val.get_type());
        self.builder.build_store(slot, val).unwrap();

        let synth = format!("__urecv_tmp_{}", self.indexed_elem_counter);
        self.indexed_elem_counter += 1;
        self.variables.insert(
            synth.clone(),
            super::VarSlot {
                ptr: slot,
                ty: val.get_type(),
            },
        );
        self.var_type_names.insert(synth.clone(), type_name.clone());

        // Drop-track the materialized temp UNCONDITIONALLY (for a fresh-owned
        // receiver), mirroring the `let`-binding path in `stmts.rs`, which always
        // tracks a fresh local regardless of how it's later used. This is correct
        // for BOTH self modes: a `ref self` / `mut ref self` method borrows, so the
        // caller obviously owns the temp; and an owned `self` method does NOT drop
        // `self` either (the user-impl dispatch passes the receiver by shallow
        // value copy and emits no receiver drop — proven by LSan: the owned-`self`
        // struct case leaked the field `Vec` once per call without this), so the
        // caller's binding/temp remains the sole owner. Only when the receiver is
        // NOT a fresh-owned temp (a borrow-returning call) do we skip — we don't
        // own it. The drop machinery differs by kind, each mirroring the matching
        // `let`-binding site in `stmts.rs`:
        //   • shared struct / enum (or `par`): one scope-exit `RcDec` on the box —
        //     `track_rc_var` with the heap type from `shared_types`. The method
        //     borrows / shallow-copies `self`, net-zero on the count, so this
        //     single dec frees the box (identical to `let c = make(); c.m()`).
        //   • value enum: `track_enum_var` — a no-op for scalar payloads, a
        //     recursive drop-switch for heap-bearing variants.
        //   • non-shared struct: user-`impl Drop` wrapper when present, else the
        //     synthesized struct-field drop.
        if self.expr_yields_fresh_owned_temp(object) {
            if is_shared {
                if let Some(heap_type) = self.shared_types.get(&type_name).map(|i| i.heap_type) {
                    self.track_rc_var(&synth, val.into_pointer_value(), heap_type);
                }
            } else if is_value_enum {
                self.track_enum_var(&type_name, slot);
            } else {
                let has_user_drop = self
                    .program_snapshot
                    .as_deref()
                    .map(|p| p.drop_method_keys.contains_key(&type_name))
                    .unwrap_or(false);
                if has_user_drop {
                    self.track_user_drop_var(&type_name, &synth, slot);
                } else {
                    self.track_struct_var(&type_name, slot);
                }
            }
        }

        let synth_expr = Expr {
            kind: ExprKind::Identifier(synth.clone()),
            span: object.span.clone(),
        };
        let result = self.compile_method_call(&synth_expr, method, args, call_span);

        // Drop the dispatch-only registrations (the queued drop, if any,
        // references the alloca, not the name, so it stays armed).
        self.variables.remove(&synth);
        self.var_type_names.remove(&synth);

        result.map(Some)
    }

    /// General owned-temp tracking, slice 3b — element-type-aware read methods
    /// (`get`/`first`/`last`/`get_unchecked`/`contains`) on a FRESH-TEMP
    /// (non-identifier) `Vec`/`VecDeque` receiver: `make_vec().get(0)`,
    /// `build_ids().contains(x)`. The typechecker records the (scalar) element
    /// `TypeExpr` keyed by the MethodCall span in `temp_recv_elem_types` — it
    /// can't be recovered from `expr_types` because the receiver and the
    /// method call share one span, which holds the method's `Option[T]`
    /// *result* type, and the LLVM `{ptr,len,cap}` shape is element-erased.
    /// With the element type in hand: compile the receiver, materialize it into
    /// a synthetic local, register the element type, drop-track the fresh temp
    /// (a `FreeVecBuffer` at the enclosing frame's exit — the read methods
    /// borrow `self`, so the caller owns the temp), then re-dispatch through
    /// the identifier-keyed `compile_vec_method`.
    ///
    /// Returns `Ok(None)` when there's no recorded element type (not a
    /// serviceable fresh-temp Vec read), so the caller falls through to the
    /// String redispatch / diagnostic — a pure addition that can't change any
    /// existing case.
    ///
    /// Element-type-generic: the typechecker records SCALAR elements for all
    /// five read methods, STRING elements for the borrow-returning
    /// `get`/`first`/`last` plus `contains` (slice 3b-heap), and one-level nested
    /// `Vec[scalar]` / `VecDeque[scalar]` elements (`Vec[Vec[i64]]`) for
    /// `get`/`first`/`last` (slice 3e). For a String *or* nested-Vec element the
    /// recorded
    /// `TypeExpr` lowers to `vec_struct_type`, so `track_vec_var`'s
    /// `FreeVecBuffer` takes the vec-struct recursion and per-element frees each
    /// inner buffer (a `String`'s bytes, or a row's POD data) before the outer
    /// buffer — and the `Option[ref String]` / `Option[ref Vec[scalar]]`
    /// `get`/`first`/`last` return is suppressed from independent drop at the
    /// match arm by `scrutinee_is_borrow_call` (which keys off the method, not
    /// the receiver shape), so the per-element storage is freed exactly once at
    /// frame exit while the borrow reads it. `contains` returns `bool` — no
    /// borrow escapes, so it carries no suppression obligation; it only needs
    /// the same per-element receiver free, and the compared arg is borrowed not
    /// consumed (a fresh-owned arg is the separate 3b-c operand-temp leak). A
    /// scalar element owns no nested
    /// heap, so the outer-buffer `FreeVecBuffer` is its complete drop. The
    /// drop-track is gated on `expr_yields_fresh_owned_temp`, and the `cap > 0`
    /// guard inside `FreeVecBuffer` is a second backstop, so a (hypothetical)
    /// borrow-returning receiver is never double-freed. Other heap elements
    /// (`Vec[T]`, user struct/enum, Map/Set) are not recorded — they need
    /// element-drop threading (`elem_agg_drop`) this helper doesn't carry.
    fn try_compile_freshtemp_vec_read_method(
        &mut self,
        object: &Expr,
        method: &str,
        args: &[CallArg],
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        // Identifier / self receivers route through the named-binding dispatch.
        if matches!(&object.kind, ExprKind::Identifier(_) | ExprKind::SelfValue) {
            return Ok(None);
        }
        let span_key = (object.span.offset, object.span.length);
        let Some(elem_te) = self.temp_recv_elem_types.get(&span_key).cloned() else {
            return Ok(None);
        };
        let cur_fn = self
            .current_fn
            .ok_or_else(|| "fresh-temp Vec read method outside fn".to_string())?;

        let recv_val = self.compile_expr(object)?;
        // The receiver must be the `{ptr, len, cap}` Vec struct; bail otherwise
        // (the typechecker gate should guarantee it, but stay shape-defensive).
        let BasicValueEnum::StructValue(sv) = recv_val else {
            return Ok(None);
        };
        if sv.get_type() != self.vec_struct_type() {
            return Ok(None);
        }

        let elem_llvm = self.llvm_type_for_type_expr(&elem_te);
        let slot = self.create_entry_alloca(cur_fn, "__vrecv_tmp", recv_val.get_type());
        self.builder.build_store(slot, recv_val).unwrap();

        // Drop the fresh-owned receiver at the enclosing frame's exit (the
        // position ceiling). The cleanup references the slot pointer, not the
        // synth name, so it stays valid after the name is unregistered below.
        // For a user-STRUCT element (slice 3f), thread the synthesized
        // per-element `__karac_drop_<S>` so the `FreeVecBuffer` runs it on every
        // live element (freeing String/Vec/shared fields) before releasing the
        // outer buffer — the inline vec-struct recursion only reaches elements
        // that are *themselves* Vec/String. Scalar/String/nested-Vec elements
        // return `None` here (not in `struct_types`) and keep the plain path.
        if self.expr_yields_fresh_owned_temp(object) {
            if let Some(agg_drop) = self.vec_elem_agg_drop_for_type_expr(&elem_te) {
                self.track_vec_of_aggs_var(slot, elem_llvm, agg_drop);
            } else {
                self.track_vec_var(slot, Some(elem_llvm));
            }
        }

        // Register the synth name so the identifier-keyed `compile_vec_method`
        // resolves the element type and the slot. Unique per call site.
        let synth = format!("__vrecv_tmp_{}", self.indexed_elem_counter);
        self.indexed_elem_counter += 1;
        self.variables.insert(
            synth.clone(),
            super::VarSlot {
                ptr: slot,
                ty: recv_val.get_type(),
            },
        );
        self.vec_elem_types.insert(synth.clone(), elem_llvm);
        self.var_elem_type_exprs.insert(synth.clone(), elem_te);
        self.var_type_names.insert(synth.clone(), "Vec".to_string());

        let result = self.compile_vec_method(&synth, slot, method, args);

        // Drop the dispatch-only registrations.
        self.variables.remove(&synth);
        self.vec_elem_types.remove(&synth);
        self.var_elem_type_exprs.remove(&synth);
        self.var_type_names.remove(&synth);

        result.map(Some)
    }

    /// General owned-temp tracking, slice 3d — read methods on a FRESH-TEMP
    /// (non-identifier) `Map`/`Set` receiver: `make_map().get(k)`,
    /// `make_map().contains_key(k)`, `make_set().contains(x)`. The Map/Set
    /// handle is a plain `ptr`, so unlike the Vec path there's no struct shape
    /// to key off — the receiver's whole `Map[K,V]` / `Set[T]` `TypeExpr` is
    /// recorded span-keyed by the typechecker in `temp_recv_mapset_types`
    /// (`compile_map_method` needs K+V; the handle drop is classified from the
    /// full type). With it in hand: compile the receiver to its handle,
    /// materialize the handle into a synthetic slot, register K/V (or elem) so
    /// the identifier-keyed `compile_map_method` / `compile_set_method` resolve
    /// it, drop-track the handle (a `FreeMapHandle` via `track_map_var`,
    /// classified by `map_temp_cleanup_parts`, at the enclosing frame's exit —
    /// the read methods borrow the map, so the caller owns the temp), then
    /// re-dispatch.
    ///
    /// Returns `Ok(None)` when there's no recorded type (not a serviceable
    /// fresh-temp Map/Set read), so the caller falls through unchanged.
    ///
    /// Type-generic over the recorded K/V/elem: the typechecker records SCALAR
    /// and owned-`String` K/V/elem (slice 3d + 3d-heap). The helper itself needs
    /// no per-type branching — `map_temp_cleanup_parts` classifies `key_is_vec`/
    /// `val_is_vec` from the `TypeExpr`, so a `String` K/V makes the single
    /// `FreeMapHandle` per-entry free the element buffers
    /// (`karac_map_free_with_drop_vec`), and `compile_map_method` resolves the
    /// String LLVM type for the lookup. `Map.get` returns `Option[ref V]`
    /// aliasing a value slot inside the map; the arm binding's independent drop
    /// is suppressed by `scrutinee_is_borrow_call` (keys off the method, not the
    /// receiver), so for a String V the per-entry buffer is freed exactly once at
    /// frame exit while the borrow reads it — the same single-free shape the
    /// `Vec[String]` slice established. `contains_key`/`contains` return `bool`
    /// (no borrow). The drop-track is gated on `expr_yields_fresh_owned_temp`.
    /// Other heap K/V (`Vec[T]`, user struct/enum, nested Map) are excluded by
    /// the typechecker gate — they need element-drop threading not carried here.
    fn try_compile_freshtemp_mapset_read_method(
        &mut self,
        object: &Expr,
        method: &str,
        args: &[CallArg],
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        if matches!(&object.kind, ExprKind::Identifier(_) | ExprKind::SelfValue) {
            return Ok(None);
        }
        let span_key = (object.span.offset, object.span.length);
        let Some(recv_te) = self.temp_recv_mapset_types.get(&span_key).cloned() else {
            return Ok(None);
        };
        let cur_fn = self
            .current_fn
            .ok_or_else(|| "fresh-temp Map/Set read method outside fn".to_string())?;

        // Extract the container head + K/V (or elem) TypeExprs from the recorded
        // `Map[K,V]` / `Set[T]` type.
        let crate::ast::TypeKind::Path(path) = &recv_te.kind else {
            return Ok(None);
        };
        let head = path
            .segments
            .first()
            .map(|s| s.as_str())
            .unwrap_or("")
            .to_string();
        let nth = |i: usize| -> Option<TypeExpr> {
            match path.generic_args.as_ref()?.get(i)? {
                crate::ast::GenericArg::Type(t) => Some(t.clone()),
                _ => None,
            }
        };

        let recv_val = self.compile_expr(object)?;
        // The handle must be a plain pointer; bail otherwise (the typechecker
        // gate should guarantee it, but stay shape-defensive).
        if !recv_val.is_pointer_value() {
            return Ok(None);
        }
        let slot = self.create_entry_alloca(cur_fn, "__mrecv_tmp", recv_val.get_type());
        self.builder.build_store(slot, recv_val).unwrap();

        // Drop the fresh-owned handle at the enclosing frame's exit, classified
        // from the full receiver type (scalar K/V → no per-entry heap drop).
        if self.expr_yields_fresh_owned_temp(object) {
            let (key_is_vec, val_is_vec, key_shared, val_shared, val_drop_fn) =
                self.map_temp_cleanup_parts(&recv_te);
            self.track_map_var_with_val_drop(
                slot,
                key_is_vec,
                val_is_vec,
                val_shared,
                key_shared,
                val_drop_fn,
            );
        }

        // Register the synth name so the identifier-keyed dispatch resolves the
        // slot + the K/V (or elem) LLVM types. Unique per call site.
        let synth = format!("__mrecv_tmp_{}", self.indexed_elem_counter);
        self.indexed_elem_counter += 1;
        self.variables.insert(
            synth.clone(),
            super::VarSlot {
                ptr: slot,
                ty: recv_val.get_type(),
            },
        );

        let result = if head == "Set" {
            self.var_type_names.insert(synth.clone(), "Set".to_string());
            if let Some(elem) = nth(0) {
                self.set_elem_types
                    .insert(synth.clone(), self.llvm_type_for_type_expr(&elem));
            }
            let r = self.compile_set_method(&synth, method, args);
            self.set_elem_types.remove(&synth);
            r
        } else {
            self.var_type_names.insert(synth.clone(), "Map".to_string());
            if let Some(k) = nth(0) {
                self.map_key_types
                    .insert(synth.clone(), self.llvm_type_for_type_expr(&k));
            }
            if let Some(v) = nth(1) {
                self.map_val_types
                    .insert(synth.clone(), self.llvm_type_for_type_expr(&v));
            }
            let r = self.compile_map_method(&synth, method, args);
            self.map_key_types.remove(&synth);
            self.map_val_types.remove(&synth);
            r
        };

        self.variables.remove(&synth);
        self.var_type_names.remove(&synth);

        result.map(Some)
    }

    /// Lower a `CStr` borrowed-surface method (design.md § C-String
    /// Literals). The receiver value is the `{ptr, i64}` slice-struct the
    /// `CStringLit` lowering in `compile_expr` produces: field 0 is the
    /// NUL-terminated rodata pointer, field 1 the source byte count
    /// (excluding the NUL). `as_ptr` is the language's first safe
    /// pointer-producer — it hands out field 0 directly (the FFI/host-fn
    /// handoff per the design's `puts(msg.as_ptr())` example). `as_bytes`
    /// returns the receiver aggregate unchanged: `Slice[u8]` shares the
    /// exact `{ptr, i64}` layout and the NUL stays invisible because the
    /// recorded len excludes it. Args are validated empty by the
    /// typechecker (`infer_cstr_method`), so they're not threaded here.
    fn compile_cstr_method(
        &mut self,
        object: &Expr,
        method: &str,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let recv = self.compile_expr(object)?;
        let agg = recv.into_struct_value();
        match method {
            "as_ptr" => Ok(self
                .builder
                .build_extract_value(agg, 0, "cstr.as_ptr")
                .unwrap()),
            "len" => Ok(self
                .builder
                .build_extract_value(agg, 1, "cstr.len")
                .unwrap()),
            "is_empty" => {
                let len = self
                    .builder
                    .build_extract_value(agg, 1, "cstr.len")
                    .unwrap()
                    .into_int_value();
                let zero = self.context.i64_type().const_zero();
                Ok(self
                    .builder
                    .build_int_compare(IntPredicate::EQ, len, zero, "cstr.is_empty")
                    .unwrap()
                    .into())
            }
            "as_bytes" => Ok(recv),
            _ => Err(format!(
                "codegen: no handler for CStr method '{}' (typechecker admits \
                 as_ptr/len/is_empty/as_bytes only — this is a codegen bug)",
                method
            )),
        }
    }

    /// Lower `CStr.to_string() -> Result[String, Utf8Error]` (phase-12
    /// Cluster 2). The receiver is the `{ptr, i64}` slice-struct (field 0 the
    /// NUL-terminated bytes, field 1 the source length). The runtime extern
    /// `karac_runtime_cstr_to_string` validates UTF-8 and either writes a heap
    /// `String` (`{ptr,len,cap}`) into an out-slot and returns `true`, or
    /// writes the `Utf8Error` variant discriminant (0 = InvalidByte,
    /// 1 = IncompleteSequence) into a second out-slot and returns `false`.
    /// Codegen owns the enum-tag assignment: it builds `Result.Ok(String)` on
    /// success and, on failure, *selects* the `Utf8Error` variant tag from the
    /// runtime discriminant before wrapping it in `Result.Err`. Structural twin
    /// of the `env.var -> Result[String, VarError]` lowering above.
    fn compile_cstr_to_string(&mut self, object: &Expr) -> Result<BasicValueEnum<'ctx>, String> {
        let recv = self.compile_expr(object)?;
        let agg = recv.into_struct_value();
        let data_ptr = self
            .builder
            .build_extract_value(agg, 0, "cstr.ts.ptr")
            .unwrap()
            .into_pointer_value();
        let data_len = self
            .builder
            .build_extract_value(agg, 1, "cstr.ts.len")
            .unwrap()
            .into_int_value();
        self.build_utf8_validated_result(data_ptr, data_len)
    }

    /// `String.from_utf8(bytes: Vec[u8]) -> Result[String, Utf8Error]` — the
    /// UTF-8-validating String constructor (interpreter parity in
    /// `eval_call.rs`). Extracts the input `Vec`'s `{data, len}` (fields 0/1 of
    /// the `{data, len, cap}` aggregate) and delegates to the shared
    /// `build_utf8_validated_result`. The bytes are validated and COPIED into a
    /// fresh heap String (the consume-by-copy convention `Vec.push(param)`
    /// uses), so the input `Vec`'s own scope-exit drop frees its buffer — no
    /// move/ownership transfer needed. Was interpreter-only (B-2026-06-18-11);
    /// this wires the codegen path so `match String.from_utf8(v) { Ok(s) => …,
    /// Err(_) => … }` builds (the Relay slice-4 request-line parse).
    pub(super) fn compile_string_from_utf8(
        &mut self,
        arg: &Expr,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let vec_val = self.compile_expr(arg)?;
        let agg = vec_val.into_struct_value();
        let data_ptr = self
            .builder
            .build_extract_value(agg, 0, "fu8.data")
            .unwrap()
            .into_pointer_value();
        let data_len = self
            .builder
            .build_extract_value(agg, 1, "fu8.len")
            .unwrap()
            .into_int_value();
        self.build_utf8_validated_result(data_ptr, data_len)
    }

    /// Shared core of `CStr.to_string()` and `String.from_utf8(Vec[u8])`:
    /// given a `(data_ptr, data_len)` byte range, validate UTF-8 via
    /// `karac_runtime_cstr_to_string` (which COPIES the bytes into a fresh heap
    /// String on success) and build `Result[String, Utf8Error]` — `Ok(String)`
    /// on valid UTF-8, else `Err(Utf8Error.{InvalidByte | IncompleteSequence})`
    /// selected from the runtime discriminant. The range is only READ (the
    /// runtime copies), so the caller's source buffer keeps its own scope-exit
    /// drop — no ownership transfer.
    fn build_utf8_validated_result(
        &mut self,
        data_ptr: inkwell::values::PointerValue<'ctx>,
        data_len: inkwell::values::IntValue<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let i64_t = self.context.i64_type();
        let i8_t = self.context.i8_type();
        let str_ty = self.vec_struct_type();

        let fn_val = self
            .current_fn
            .ok_or_else(|| "codegen: CStr.to_string called outside a function".to_string())?;
        let out_str = self.create_entry_alloca(fn_val, "cstr.ts.outstr", str_ty.into());
        let out_err = self.create_entry_alloca(fn_val, "cstr.ts.outerr", i8_t.into());

        let f = self
            .module
            .get_function("karac_runtime_cstr_to_string")
            .expect("karac_runtime_cstr_to_string declared in Codegen::new");
        let ok = self
            .builder
            .build_call(
                f,
                &[
                    data_ptr.into(),
                    data_len.into(),
                    out_str.into(),
                    out_err.into(),
                ],
                "cstr.ts.ok",
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();

        // Copy out the Result llvm-type and the two Utf8Error variant tags
        // before any `&mut self` call (drops the `enum_layouts` borrow).
        let result_ty = self
            .enum_layouts
            .get("Result")
            .map(|l| l.llvm_type)
            .ok_or_else(|| "codegen: Result enum layout missing (codegen bug)".to_string())?;
        let (tag_invalid, tag_incomplete) = {
            let utf8 = self.enum_layouts.get("Utf8Error").ok_or_else(|| {
                "codegen: Utf8Error enum layout missing (codegen bug)".to_string()
            })?;
            let inv = *utf8.tags.get("InvalidByte").ok_or_else(|| {
                "codegen: Utf8Error.InvalidByte missing (codegen bug)".to_string()
            })?;
            let inc = *utf8.tags.get("IncompleteSequence").ok_or_else(|| {
                "codegen: Utf8Error.IncompleteSequence missing (codegen bug)".to_string()
            })?;
            (inv, inc)
        };

        let ok_bb = self.context.append_basic_block(fn_val, "cstr.ts.ok_bb");
        let err_bb = self.context.append_basic_block(fn_val, "cstr.ts.err_bb");
        let merge_bb = self.context.append_basic_block(fn_val, "cstr.ts.merge");
        self.builder
            .build_conditional_branch(ok, ok_bb, err_bb)
            .unwrap();

        // Ok arm: Result.Ok(<heap String the runtime wrote into out_str>).
        self.builder.position_at_end(ok_bb);
        let string_val = self
            .builder
            .build_load(str_ty, out_str, "cstr.ts.str")
            .unwrap();
        let ok_val = self.build_nonshared_enum_value("Result", "Ok", &[string_val])?;
        self.builder.build_unconditional_branch(merge_bb).unwrap();
        let ok_end = self.builder.get_insert_block().unwrap();

        // Err arm: Result.Err(Utf8Error.<runtime-selected variant>). Both
        // candidate variants are unit-payload, so building a base aggregate for
        // one and overwriting its tag word yields the other with no extra block.
        self.builder.position_at_end(err_bb);
        let err_tag = self
            .builder
            .build_load(i8_t, out_err, "cstr.ts.errtag")
            .unwrap()
            .into_int_value();
        let is_invalid = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                err_tag,
                i8_t.const_zero(),
                "cstr.ts.is_invalid",
            )
            .unwrap();
        let sel_tag = self
            .builder
            .build_select(
                is_invalid,
                i64_t.const_int(tag_invalid, false),
                i64_t.const_int(tag_incomplete, false),
                "cstr.ts.errsel",
            )
            .unwrap()
            .into_int_value();
        let base_err = self
            .build_nonshared_enum_value("Utf8Error", "InvalidByte", &[])?
            .into_struct_value();
        let utf8_err = self
            .builder
            .build_insert_value(base_err, sel_tag, 0, "cstr.ts.utf8err")
            .unwrap()
            .into_struct_value();
        let err_val = self.build_nonshared_enum_value("Result", "Err", &[utf8_err.into()])?;
        self.builder.build_unconditional_branch(merge_bb).unwrap();
        let err_end = self.builder.get_insert_block().unwrap();

        self.builder.position_at_end(merge_bb);
        let phi = self.builder.build_phi(result_ty, "cstr.ts.result").unwrap();
        phi.add_incoming(&[(&ok_val, ok_end), (&err_val, err_end)]);
        Ok(phi.as_basic_value())
    }

    /// Lower an ambient built-in resource method (`env.set`, `clock.now`).
    ///
    /// A `with_provider[R]` override of an ambient resource is pushed onto
    /// the runtime provider stack (see `compile_with_provider_ambient`), so
    /// the override is visible across function-call boundaries — including
    /// the `karac test` synthesized-main path, which wraps a *call* to the
    /// test fn. When an override vtable for this resource exists in the
    /// module, emit a runtime branch: consult `karac_provider_lookup`, and
    /// if an override frame is active, dispatch through its vtable;
    /// otherwise fall to the builtin FFI default. When no override vtable
    /// exists (no `with_provider[R]` in the module), no override can be
    /// active, so skip the branch and emit the FFI default directly.
    pub(super) fn compile_ambient_resource_method(
        &mut self,
        resource: &str,
        method: &str,
        args: &[CallArg],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        // Compile args ONCE — they must not be re-evaluated across the
        // override / default branches (side effects would double-run).
        let arg_vals: Vec<BasicValueEnum<'ctx>> = args
            .iter()
            .map(|a| self.compile_expr(&a.value))
            .collect::<Result<_, _>>()?;

        // Runtime override dispatch is possible only when (a) this method
        // has a canonical vtable slot and (b) some override vtable for this
        // resource was emitted in the module. Otherwise no override can be
        // active at runtime — emit the FFI default directly.
        if let Some(method_idx) = ambient_method_index(resource, method) {
            if let Some(fn_type) = self.ambient_override_fn_type(resource, method) {
                return self.compile_ambient_dispatch_branch(
                    resource, method, method_idx, fn_type, &arg_vals,
                );
            }
        } else if self.ambient_override_fn_type(resource, method).is_some() {
            // The method has NO `AMBIENT_RESOURCE_METHODS` vtable slot, yet a
            // `with_provider[<resource>]` override in this module supplies an
            // impl of it (its `@<Type>.<method>` symbol exists). With no slot
            // there is no runtime dispatch branch, so falling through to the
            // builtin FFI default would SILENTLY ignore the override and
            // diverge from the interpreter. Error loudly instead. Every
            // ambient method that has both an FFI default and override support
            // is listed in `AMBIENT_RESOURCE_METHODS` (so it takes the branch
            // above) — reaching here means a method gained an override impl
            // before earning a slot; add it to the table to lift this.
            return Err(format!(
                "codegen: a `with_provider[{resource}]` override supplies `{method}`, but \
                 ambient overrides of `{resource}.{method}` are not yet lowered (the method has \
                 no vtable slot, so the override would be silently ignored). Run this program \
                 with `karac run` (interpreter), or drop the override of `{method}`. Tracked in \
                 docs/implementation_checklist/phase-7-codegen.md."
            ));
        }
        self.compile_ambient_ffi(resource, method, &arg_vals)
    }

    /// Emit the runtime override-vs-default branch for an ambient method
    /// call whose resource has an override vtable in this module:
    /// ```text
    ///   {data, vt} = karac_provider_lookup(<resource_id>)
    ///   br (data != null), %override, %default
    /// override: fn = vt[<method_idx>]; r1 = call fn(self=data, args...)
    /// default:  r2 = <ambient FFI default>
    /// merge:    phi <ret> [r1, override], [r2, default]
    /// ```
    /// The merge phi takes the method's real return type, read off the
    /// FFI-default value (`default_val.get_type()`): i64 for the scalar /
    /// unit-placeholder methods (`Clock.now`, `RandomSource.next_u64`,
    /// `Env.set`, `Stdout/Stderr.*`), the `Vec` struct for `Env.args`, the
    /// `Result` enum for `Env.var` / `Stdin.*` / `FileSystem.*`. The
    /// override arm and the default arm both lower the same Kāra signature,
    /// so they produce the identical LLVM type (aggregates return by value —
    /// no sret), and a void-returning override yields the same i64-0
    /// placeholder the unit FFI default does. A null fn-ptr slot (override
    /// implements only some methods) would null-deref in the override arm —
    /// but the override arm is only taken when a frame is active, and an
    /// active provider must implement every method the body calls (the
    /// interpreter errors otherwise — `resource_method.rs`, no per-method
    /// fallback), so the slot for a called method is non-null.
    fn compile_ambient_dispatch_branch(
        &mut self,
        resource: &str,
        method: &str,
        method_idx: usize,
        fn_type: inkwell::types::FunctionType<'ctx>,
        arg_vals: &[BasicValueEnum<'ctx>],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let resource_id = *self.provider_resource_ids.get(resource).ok_or_else(|| {
            format!("codegen: ambient resource '{resource}' has no minted ID (codegen bug)")
        })?;
        let i32_t = self.context.i32_type();
        let i64_t = self.context.i64_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let fn_val = self
            .current_fn
            .ok_or_else(|| "ambient dispatch: no current function".to_string())?;

        // Runtime lookup → {data, vtable}.
        let id_v = i32_t.const_int(resource_id as u64, false);
        let lookup_sv = self
            .builder
            .build_call(self.karac_provider_lookup_fn, &[id_v.into()], "amb.lookup")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_struct_value();
        let data_ptr = self
            .builder
            .build_extract_value(lookup_sv, 0, "amb.data")
            .unwrap()
            .into_pointer_value();
        let vtable_ptr = self
            .builder
            .build_extract_value(lookup_sv, 1, "amb.vt")
            .unwrap()
            .into_pointer_value();
        let is_present = self
            .builder
            .build_is_not_null(data_ptr, "amb.present")
            .unwrap();

        let override_bb = self.context.append_basic_block(fn_val, "amb.override");
        let default_bb = self.context.append_basic_block(fn_val, "amb.default");
        let merge_bb = self.context.append_basic_block(fn_val, "amb.merge");
        self.builder
            .build_conditional_branch(is_present, override_bb, default_bb)
            .unwrap();

        // override arm: indirect call through the vtable slot.
        self.builder.position_at_end(override_bb);
        let idx_v = i32_t.const_int(method_idx as u64, false);
        let fn_slot = unsafe {
            self.builder
                .build_gep(ptr_ty, vtable_ptr, &[idx_v], "amb.fn.slot")
                .unwrap()
        };
        let fn_ptr = self
            .builder
            .build_load(ptr_ty, fn_slot, "amb.fn")
            .unwrap()
            .into_pointer_value();
        // self-arg lowering mirrors `try_compile_provider_dispatch`: ptr
        // for `ref/mut ref/shared self`, loaded struct for owned `self`.
        let self_param_ty = fn_type
            .get_param_types()
            .into_iter()
            .next()
            .ok_or_else(|| {
                format!("ambient dispatch: override method `{resource}.{method}` has no self param")
            })?;
        let self_arg: BasicMetadataValueEnum<'ctx> = match self_param_ty {
            inkwell::types::BasicMetadataTypeEnum::PointerType(_) => {
                BasicMetadataValueEnum::from(data_ptr)
            }
            inkwell::types::BasicMetadataTypeEnum::StructType(st) => {
                let loaded = self
                    .builder
                    .build_load(st, data_ptr, "amb.self.owned")
                    .unwrap();
                BasicMetadataValueEnum::from(loaded)
            }
            other => {
                return Err(format!(
                    "ambient dispatch: unexpected self-param lowering `{other:?}` for `{resource}.{method}`"
                ));
            }
        };
        let mut call_args: Vec<BasicMetadataValueEnum<'ctx>> = vec![self_arg];
        for v in arg_vals {
            call_args.push(BasicMetadataValueEnum::from(*v));
        }
        let override_call = self
            .builder
            .build_indirect_call(fn_type, fn_ptr, &call_args, "amb.call")
            .unwrap();
        let override_val: BasicValueEnum<'ctx> =
            if override_call.try_as_basic_value().is_instruction() {
                i64_t.const_int(0, false).into()
            } else {
                override_call.try_as_basic_value().unwrap_basic()
            };
        self.builder.build_unconditional_branch(merge_bb).unwrap();
        let override_end = self.builder.get_insert_block().unwrap();

        // default arm: the builtin FFI default.
        self.builder.position_at_end(default_bb);
        let default_val = self.compile_ambient_ffi(resource, method, arg_vals)?;
        self.builder.build_unconditional_branch(merge_bb).unwrap();
        let default_end = self.builder.get_insert_block().unwrap();

        // merge: phi the two results at the method's real return type. Both
        // arms lower the same Kāra signature, so their LLVM types match; a
        // void override reuses the unit i64-0 placeholder (= `default_val`).
        self.builder.position_at_end(merge_bb);
        let phi = self
            .builder
            .build_phi(default_val.get_type(), "amb.result")
            .unwrap();
        phi.add_incoming(&[(&override_val, override_end), (&default_val, default_end)]);
        Ok(phi.as_basic_value())
    }

    /// The builtin-FFI default lowering for an ambient method (the codegen
    /// counterpart of the interpreter's
    /// `dispatch_builtin_resource_method_with_values`). Takes already-
    /// compiled arg values so it can serve both the no-override fast path
    /// and the default arm of `compile_ambient_dispatch_branch` without
    /// re-evaluating args. Only the resource/method pairs the runtime backs
    /// are lowered; others error naming the gap rather than miscompiling.
    fn compile_ambient_ffi(
        &mut self,
        resource: &str,
        method: &str,
        arg_vals: &[BasicValueEnum<'ctx>],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let i64_t = self.context.i64_type();
        let ptr_t = self.context.ptr_type(AddressSpace::default());
        match (resource, method) {
            ("Env", "set") => {
                if arg_vals.len() != 2 {
                    return Err(format!(
                        "codegen: env.set expects 2 arguments, found {}",
                        arg_vals.len()
                    ));
                }
                let (name_ptr, name_len) = self.extract_string_ptr_len(arg_vals[0], "env.set.name");
                let (val_ptr, val_len) = self.extract_string_ptr_len(arg_vals[1], "env.set.val");
                let fn_val = match self.module.get_function("karac_runtime_env_set") {
                    Some(f) => f,
                    None => {
                        let fn_ty = self.context.void_type().fn_type(
                            &[ptr_t.into(), i64_t.into(), ptr_t.into(), i64_t.into()],
                            false,
                        );
                        self.module
                            .add_function("karac_runtime_env_set", fn_ty, None)
                    }
                };
                self.builder
                    .build_call(
                        fn_val,
                        &[
                            name_ptr.into(),
                            name_len.into(),
                            val_ptr.into(),
                            val_len.into(),
                        ],
                        "env.set",
                    )
                    .unwrap();
                // `env.set` returns Unit → the i64-0 void-return placeholder.
                Ok(i64_t.const_int(0, false).into())
            }
            ("Clock", "now") => {
                if !arg_vals.is_empty() {
                    return Err(format!(
                        "codegen: clock.now expects 0 arguments, found {}",
                        arg_vals.len()
                    ));
                }
                let fn_val = match self.module.get_function("karac_runtime_clock_now") {
                    Some(f) => f,
                    None => {
                        let fn_ty = i64_t.fn_type(&[], false);
                        self.module
                            .add_function("karac_runtime_clock_now", fn_ty, None)
                    }
                };
                let call = self.builder.build_call(fn_val, &[], "clock.now").unwrap();
                Ok(call.try_as_basic_value().unwrap_basic())
            }
            ("RandomSource", "next_u64") => {
                if !arg_vals.is_empty() {
                    return Err(format!(
                        "codegen: rand.next_u64 expects 0 arguments, found {}",
                        arg_vals.len()
                    ));
                }
                let fn_val = match self.module.get_function("karac_runtime_rand_next_u64") {
                    Some(f) => f,
                    None => {
                        let fn_ty = i64_t.fn_type(&[], false);
                        self.module
                            .add_function("karac_runtime_rand_next_u64", fn_ty, None)
                    }
                };
                let call = self
                    .builder
                    .build_call(fn_val, &[], "rand.next_u64")
                    .unwrap();
                Ok(call.try_as_basic_value().unwrap_basic())
            }
            ("Env", "args") => {
                if !arg_vals.is_empty() {
                    return Err(format!(
                        "codegen: env.args expects 0 arguments, found {}",
                        arg_vals.len()
                    ));
                }
                // `env.args() -> Vec[String]` — first aggregate-returning
                // ambient method. Out-pointer ABI: alloca a `{ptr, i64, i64}`
                // Vec slot, hand its address to the runtime fn (which
                // heap-allocates the element buffer + each String in Kāra
                // shape so scope-exit cleanup frees them), then load the Vec
                // value. Mirrors the `Runtime.list_par_blocks` lowering.
                let vec_ty = self.vec_struct_type();
                let fn_val = self
                    .current_fn
                    .ok_or_else(|| "codegen: env.args called outside a function".to_string())?;
                let slot = self.create_entry_alloca(fn_val, "env.args.slot", vec_ty.into());
                let f = match self.module.get_function("karac_runtime_env_args_into") {
                    Some(f) => f,
                    None => {
                        let fn_ty = self.context.void_type().fn_type(&[ptr_t.into()], false);
                        self.module
                            .add_function("karac_runtime_env_args_into", fn_ty, None)
                    }
                };
                self.builder
                    .build_call(f, &[slot.into()], "env.args.fill")
                    .unwrap();
                let value = self
                    .builder
                    .build_load(vec_ty, slot, "env.args.val")
                    .unwrap();
                Ok(value)
            }
            ("Env", "var") => {
                if arg_vals.len() != 1 {
                    return Err(format!(
                        "codegen: env.var expects 1 argument, found {}",
                        arg_vals.len()
                    ));
                }
                // `env.var(name) -> Result[String, VarError]`. The runtime FFI
                // does the OS read + heap String copy and returns `found:i1`,
                // writing the String into an out-slot; codegen builds the
                // Result enum here — `Ok(string)` on found, `Err(VarError
                // .NotPresent)` on miss — so all enum-layout knowledge stays
                // on the codegen side (codegen-containment). String shares the
                // `{ptr, i64, i64}` shape with Vec, so `vec_struct_type()` is
                // the out-slot type.
                let (name_ptr, name_len) = self.extract_string_ptr_len(arg_vals[0], "env.var.name");
                let str_ty = self.vec_struct_type();
                let fn_val = self
                    .current_fn
                    .ok_or_else(|| "codegen: env.var called outside a function".to_string())?;
                let out_slot = self.create_entry_alloca(fn_val, "env.var.out", str_ty.into());
                let f = match self.module.get_function("karac_runtime_env_var") {
                    Some(f) => f,
                    None => {
                        let fn_ty = self
                            .context
                            .bool_type()
                            .fn_type(&[ptr_t.into(), i64_t.into(), ptr_t.into()], false);
                        self.module
                            .add_function("karac_runtime_env_var", fn_ty, None)
                    }
                };
                let found = self
                    .builder
                    .build_call(
                        f,
                        &[name_ptr.into(), name_len.into(), out_slot.into()],
                        "env.var.found",
                    )
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_int_value();

                let result_ty = self
                    .enum_layouts
                    .get("Result")
                    .map(|l| l.llvm_type)
                    .ok_or_else(|| {
                        "codegen: Result enum layout missing (codegen bug)".to_string()
                    })?;

                let found_bb = self.context.append_basic_block(fn_val, "env.var.found_bb");
                let notfound_bb = self
                    .context
                    .append_basic_block(fn_val, "env.var.notfound_bb");
                let merge_bb = self.context.append_basic_block(fn_val, "env.var.merge");
                self.builder
                    .build_conditional_branch(found, found_bb, notfound_bb)
                    .unwrap();

                // found arm: Result.Ok(<heap String the FFI wrote>).
                self.builder.position_at_end(found_bb);
                let string_val = self
                    .builder
                    .build_load(str_ty, out_slot, "env.var.str")
                    .unwrap();
                let ok_val = self.build_nonshared_enum_value("Result", "Ok", &[string_val])?;
                self.builder.build_unconditional_branch(merge_bb).unwrap();
                let found_end = self.builder.get_insert_block().unwrap();

                // miss arm: Result.Err(VarError.NotPresent).
                self.builder.position_at_end(notfound_bb);
                let varerr = self.build_nonshared_enum_value("VarError", "NotPresent", &[])?;
                let err_val = self.build_nonshared_enum_value("Result", "Err", &[varerr])?;
                self.builder.build_unconditional_branch(merge_bb).unwrap();
                let notfound_end = self.builder.get_insert_block().unwrap();

                self.builder.position_at_end(merge_bb);
                let phi = self.builder.build_phi(result_ty, "env.var.result").unwrap();
                phi.add_incoming(&[(&ok_val, found_end), (&err_val, notfound_end)]);
                Ok(phi.as_basic_value())
            }
            ("Stdin", "read_line") | ("Stdin", "read_to_string") => {
                if !arg_vals.is_empty() {
                    return Err(format!(
                        "codegen: stdin.{method} expects 0 arguments, found {}",
                        arg_vals.len()
                    ));
                }
                // `stdin.read_line()` / `read_to_string()` -> Result[String,
                // IoError]. Same `KaracIoResult` out-param ABI + String-payload
                // unpack as `FileSystem.read_to_string`: alloca the 32-byte
                // result slot, call the runtime fn, then `lower_kara_io_result`
                // builds `Result.Ok(string)` (error_kind == 0) or
                // `Result.Err(IoError)` (variant from the runtime's error_kind),
                // so all IoError-layout knowledge stays in the shared file-IO
                // lowering rather than being duplicated here.
                let symbol = if method == "read_line" {
                    "karac_runtime_stdin_read_line"
                } else {
                    "karac_runtime_stdin_read_to_string"
                };
                let io_ty = self.kara_io_result_type();
                let fn_val = self
                    .current_fn
                    .ok_or_else(|| format!("codegen: stdin.{method} called outside a function"))?;
                let slot = self.create_entry_alloca(fn_val, "stdin.read.slot", io_ty.into());
                let f = match self.module.get_function(symbol) {
                    Some(f) => f,
                    None => {
                        let fn_ty = self.context.void_type().fn_type(&[ptr_t.into()], false);
                        self.module.add_function(symbol, fn_ty, None)
                    }
                };
                self.builder
                    .build_call(f, &[slot.into()], "stdin.read.call")
                    .unwrap();
                self.lower_kara_io_result(slot, super::file::FileOkKind::StringPayload)
            }
            ("Stdout", "print")
            | ("Stdout", "println")
            | ("Stderr", "print")
            | ("Stderr", "println") => {
                if arg_vals.len() != 1 {
                    return Err(format!(
                        "codegen: {resource}.{method} expects 1 argument, found {}",
                        arg_vals.len()
                    ));
                }
                let to_stderr = resource == "Stderr";
                let newline = method == "println";
                self.emit_console_str_write(arg_vals[0], to_stderr, newline)?;
                // Returns Unit → the i64-0 void-return placeholder.
                Ok(i64_t.const_int(0, false).into())
            }
            ("Stdout", "flush") | ("Stderr", "flush") => {
                if !arg_vals.is_empty() {
                    return Err(format!(
                        "codegen: {resource}.flush expects 0 arguments, found {}",
                        arg_vals.len()
                    ));
                }
                // `fflush(NULL)` flushes every open output stream — portable
                // (POSIX), and crucially flushes the libc stdout buffer that
                // `printf` (free `print`/`println` and `Stdout.*`) writes
                // into. `Stderr.*` goes to fd 2 unbuffered via `dprintf`, so
                // its flush is a no-op, but `fflush(NULL)` covers both
                // uniformly. No FILE*-global access needed (the `stdout` /
                // `__stderrp` symbol differs across libc).
                let fflush = match self.module.get_function("fflush") {
                    Some(f) => f,
                    None => {
                        let ty = self.context.i32_type().fn_type(&[ptr_t.into()], false);
                        self.module.add_function("fflush", ty, None)
                    }
                };
                self.builder
                    .build_call(fflush, &[ptr_t.const_null().into()], "fflush")
                    .unwrap();
                Ok(i64_t.const_int(0, false).into())
            }
            ("FileSystem", "read_to_string") => {
                // Lowercase `fs.read_to_string(path)`. The capitalized
                // `FileSystem.read_to_string` is lowered on the associated-call
                // path (`assoc_call.rs` → `compile_file_read_to_string`); the
                // ambient-alias path arrives here with the path already
                // compiled, so route to the value-core variant.
                if arg_vals.len() != 1 {
                    return Err(format!(
                        "codegen: fs.read_to_string expects 1 argument, found {}",
                        arg_vals.len()
                    ));
                }
                self.compile_file_read_to_string_val(arg_vals[0])
            }
            ("FileSystem", "write") => {
                // Lowercase `fs.write(path, contents)`. Capitalized form is
                // lowered via `assoc_call.rs` → `compile_fs_write`; here both
                // args are pre-compiled, so use the value-core variant.
                if arg_vals.len() != 2 {
                    return Err(format!(
                        "codegen: fs.write expects 2 arguments, found {}",
                        arg_vals.len()
                    ));
                }
                self.compile_fs_write_vals(arg_vals[0], arg_vals[1])
            }
            _ => Err(format!(
                "codegen: ambient resource method '{}.{}' is not yet lowered \
                 (interpreter-only); add a runtime FFI + an arm in \
                 `compile_ambient_ffi`",
                resource, method
            )),
        }
    }

    /// Emit a console write of a Kāra `String` value to stdout or stderr,
    /// optionally with a trailing newline. Backs the `Stdout.{print,println}`
    /// / `Stderr.{print,println}` ambient methods (L646 slice 4b).
    ///
    /// **Stdout** reuses `self.printf_fn` — the SAME libc `printf` / stdout
    /// buffer the free `print`/`println` builtins use (`compile_print`), so a
    /// program mixing `println(x)` and `Stdout.println(y)` never interleaves
    /// out of order. **Stderr** writes to fd 2 via POSIX `dprintf`, avoiding
    /// the non-portable `stderr` / `__stderrp` FILE*-global; fd 2 is
    /// unbuffered. Both use `%.*s` with the explicit length (field 1) so a
    /// non-NUL-terminated heap `String` is read exactly `len` bytes —
    /// identical to `compile_print`'s String-value arm (which documents the
    /// ASan heap-overflow that a bare `%s` would cause).
    fn emit_console_str_write(
        &mut self,
        str_val: BasicValueEnum<'ctx>,
        to_stderr: bool,
        newline: bool,
    ) -> Result<(), String> {
        if !str_val.is_struct_value() {
            return Err(format!(
                "codegen: console write expects a String value, got {str_val:?}"
            ));
        }
        let sv = str_val.into_struct_value();
        let str_ptr = self
            .builder
            .build_extract_value(sv, 0, "con.str.ptr")
            .unwrap()
            .into_pointer_value();
        let str_len = self
            .builder
            .build_extract_value(sv, 1, "con.str.len")
            .unwrap()
            .into_int_value();
        let nl = if newline { "\n" } else { "" };
        // NUL-safe `fwrite` to the stdout / stderr `FILE*` (L5) — the old
        // `printf`/`dprintf("%.*s")` form truncated a String at an interior
        // NUL. stderr's `FILE*` is unbuffered by default, preserving the
        // immediate-flush semantics the prior `dprintf(fd 2)` had.
        self.emit_nul_safe_write(str_ptr, str_len, nl, to_stderr);
        Ok(())
    }

    /// True iff `object` is a receiver shape whose static type is
    /// `Atomic[T]` — either an Identifier `a` (var_type_names registers
    /// "Atomic" via the let-stmt RHS recognizer in `compile_stmt`) or a
    /// FieldAccess `c.field` where `c`'s struct registers `field`'s
    /// declared type as `Atomic` in `struct_field_type_names`.
    /// Companion gate to `compile_atomic_method`.
    fn is_atomic_receiver(&self, object: &Expr) -> bool {
        match &object.kind {
            ExprKind::Identifier(name) => {
                matches!(self.var_type_names.get(name.as_str()), Some(n) if n == "Atomic")
            }
            ExprKind::FieldAccess { object, field } => {
                if let Some(obj_ty) = self.type_name_of_expr(object) {
                    if let Some(field_names) = self.struct_field_names.get(obj_ty.as_str()) {
                        if let Some(idx) = field_names.iter().position(|n| n == field) {
                            if let Some(field_ty_names) =
                                self.struct_field_type_names.get(obj_ty.as_str())
                            {
                                return field_ty_names.get(idx).and_then(|n| n.as_deref())
                                    == Some("Atomic");
                            }
                        }
                    }
                }
                false
            }
            _ => false,
        }
    }

    /// Codegen for `Atomic[T].load(MemoryOrdering.X)` and
    /// `Atomic[T].store(value, MemoryOrdering.X)`. Resolves the
    /// receiver's storage pointer + element LLVM type, parses the
    /// trailing `MemoryOrdering.X` qualified-variant arg into an
    /// `inkwell::AtomicOrdering`, and emits `load atomic` / `store
    /// atomic` against the slot. Supports both Identifier receivers
    /// (`a.load(...)` where `a` is a top-level Atomic[T] binding) and
    /// FieldAccess receivers (`c.field.load(...)` where `c.field` is
    /// an Atomic-typed struct field — the shape the `karac migrate
    /// --atomic` consumer-rewrite emits). The receiver gate runs in
    /// `is_atomic_receiver` upstream.
    fn compile_atomic_method(
        &mut self,
        object: &Expr,
        method: &str,
        args: &[CallArg],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let (storage_ptr, elem_ty, inner_is_bool) = self.resolve_atomic_storage(object)?;
        // LLVM requires atomic load/store on a power-of-two-byte
        // integer (i8/i16/i32/i64/i128 plus pointer/float of those
        // widths). Reject narrower / odd-width integers explicitly so
        // the user sees a clear codegen diagnostic rather than an
        // opaque LLVM verifier failure. `Atomic[bool]` is supported
        // via i8 slot-widening (`is_bool_type_expr` arm in
        // `llvm_type_for_type_expr` returns i8, not i1; the load/store
        // arms below trunc/zext at the i1↔i8 boundary).
        if let BasicTypeEnum::IntType(it) = elem_ty {
            let bw = it.get_bit_width();
            if bw < 8 || !bw.is_power_of_two() {
                return Err(format!(
                    "codegen: Atomic[T] requires T to be a power-of-two-byte integer \
                     (i8/i16/i32/i64/i128/usize) or `bool` (widened to i8); \
                     received {}-bit integer.",
                    bw
                ));
            }
        }
        match method {
            "load" => {
                if args.len() != 1 {
                    return Err(format!(
                        "codegen: Atomic.load takes 1 MemoryOrdering argument, got {}",
                        args.len()
                    ));
                }
                let ordering = self.parse_memory_ordering(&args[0].value)?;
                if matches!(
                    ordering,
                    AtomicOrdering::Release | AtomicOrdering::AcquireRelease
                ) {
                    return Err(format!(
                        "codegen: Atomic.load rejects MemoryOrdering.{:?} (LLVM forbids \
                         Release / AcqRel on a load); use Relaxed / Acquire / SeqCst",
                        ordering
                    ));
                }
                let loaded = self
                    .builder
                    .build_load(elem_ty, storage_ptr, "atomic.load")
                    .unwrap();
                let inst = loaded
                    .as_instruction_value()
                    .expect("build_load produces an instruction with an instruction value");
                let align = atomic_alignment_for(elem_ty);
                inst.set_alignment(align).map_err(|e| {
                    format!("codegen: set_alignment failed on atomic load: {:?}", e)
                })?;
                inst.set_atomic_ordering(ordering).map_err(|e| {
                    format!(
                        "codegen: set_atomic_ordering failed on atomic load: {:?}",
                        e
                    )
                })?;
                // Atomic[bool]: the slot is i8 (widened); the surface
                // type the user sees is `bool` (i1). Trunc back to i1
                // so downstream comparison / branch ops see the
                // expected bit width.
                if inner_is_bool {
                    let i8v = loaded.into_int_value();
                    let i1 = self
                        .builder
                        .build_int_truncate(i8v, self.context.bool_type(), "atomic.bool.trunc")
                        .unwrap();
                    return Ok(i1.into());
                }
                Ok(loaded)
            }
            "store" => {
                if args.len() != 2 {
                    return Err(format!(
                        "codegen: Atomic.store takes (value, MemoryOrdering), got {} args",
                        args.len()
                    ));
                }
                let value = self.compile_expr(&args[0].value)?;
                let ordering = self.parse_memory_ordering(&args[1].value)?;
                if matches!(
                    ordering,
                    AtomicOrdering::Acquire | AtomicOrdering::AcquireRelease
                ) {
                    return Err(format!(
                        "codegen: Atomic.store rejects MemoryOrdering.{:?} (LLVM forbids \
                         Acquire / AcqRel on a store); use Relaxed / Release / SeqCst",
                        ordering
                    ));
                }
                // Atomic[bool]: the value coming in is i1, but the slot
                // is i8. Zext at the boundary so the store's value
                // width matches the slot's. The matched trunc on load
                // restores the i1 view above.
                let value = if inner_is_bool {
                    if let BasicValueEnum::IntValue(iv) = value {
                        if iv.get_type().get_bit_width() == 1 {
                            self.builder
                                .build_int_z_extend(iv, self.context.i8_type(), "atomic.bool.zext")
                                .unwrap()
                                .into()
                        } else {
                            value
                        }
                    } else {
                        value
                    }
                } else {
                    value
                };
                let store_inst = self.builder.build_store(storage_ptr, value).unwrap();
                let align = atomic_alignment_for(elem_ty);
                store_inst.set_alignment(align).map_err(|e| {
                    format!("codegen: set_alignment failed on atomic store: {:?}", e)
                })?;
                store_inst.set_atomic_ordering(ordering).map_err(|e| {
                    format!(
                        "codegen: set_atomic_ordering failed on atomic store: {:?}",
                        e
                    )
                })?;
                // Stores return unit — fill the expression slot with the
                // i64-0 placeholder used elsewhere for void returns.
                Ok(self.context.i64_type().const_int(0, false).into())
            }
            // Single-operand read-modify-write ops — all lower to one LLVM
            // `atomicrmw` and return the PREVIOUS value (matching Rust's
            // `Atomic::fetch_*` / `swap`), so e.g. `count.fetch_add(1, ..)` is
            // a race-free increment yielding the pre-increment count. `atomicrmw`
            // accepts any memory ordering (unlike load/store), so no ordering
            // rejection. The arithmetic / bitwise ops are integer-only
            // (`Atomic[bool]` has no arithmetic/bitwise RMW); `swap` (Xchg) is a
            // plain exchange and is the one RMW that also works on `Atomic[bool]`
            // (i8 slot — incoming i1 widened, returned old i8 truncated, same as
            // load/store). `compare_exchange` is a separate slice (two operands,
            // `cmpxchg`, Result-shaped return).
            "fetch_add" | "fetch_sub" | "fetch_and" | "fetch_or" | "fetch_xor" | "swap" => {
                if args.len() != 2 {
                    return Err(format!(
                        "codegen: Atomic.{} takes (value, MemoryOrdering), got {} args",
                        method,
                        args.len()
                    ));
                }
                let is_swap = method == "swap";
                if inner_is_bool && !is_swap {
                    return Err(format!(
                        "codegen: Atomic[bool] does not support {} (no arithmetic/bitwise RMW \
                         on a bool); only `swap` / `load` / `store`",
                        method
                    ));
                }
                let value = self.compile_expr(&args[0].value)?;
                let ordering = self.parse_memory_ordering(&args[1].value)?;
                // Atomic[bool] swap: the slot is i8 but the incoming value is
                // i1 — widen at the boundary (mirrors `store`).
                let value = if inner_is_bool {
                    if let BasicValueEnum::IntValue(iv) = value {
                        if iv.get_type().get_bit_width() == 1 {
                            self.builder
                                .build_int_z_extend(iv, self.context.i8_type(), "atomic.bool.zext")
                                .unwrap()
                                .into()
                        } else {
                            value
                        }
                    } else {
                        value
                    }
                } else {
                    value
                };
                let val_int = match value {
                    BasicValueEnum::IntValue(iv) => iv,
                    _ => {
                        return Err(format!(
                            "codegen: Atomic.{} requires an integer value argument",
                            method
                        ))
                    }
                };
                let op = match method {
                    "fetch_add" => AtomicRMWBinOp::Add,
                    "fetch_sub" => AtomicRMWBinOp::Sub,
                    "fetch_and" => AtomicRMWBinOp::And,
                    "fetch_or" => AtomicRMWBinOp::Or,
                    "fetch_xor" => AtomicRMWBinOp::Xor,
                    "swap" => AtomicRMWBinOp::Xchg,
                    _ => unreachable!("RMW arm gated on the method set above"),
                };
                let old = self
                    .builder
                    .build_atomicrmw(op, storage_ptr, val_int, ordering)
                    .map_err(|e| format!("codegen: build_atomicrmw failed: {:?}", e))?;
                // Atomic[bool] swap: returned old is i8 → trunc to i1 for the
                // surface `bool` view (mirrors `load`). `build_atomicrmw`
                // returns an `IntValue` directly.
                if inner_is_bool {
                    let i1 = self
                        .builder
                        .build_int_truncate(old, self.context.bool_type(), "atomic.bool.trunc")
                        .unwrap();
                    return Ok(i1.into());
                }
                Ok(old.into())
            }
            // `compare_exchange(old, new, success, failure) -> Result[T, T]`
            // (deferred.md § Atomic Operations). Lowers to LLVM `cmpxchg`, which
            // returns a `{ T, i1 }` struct: field 0 is the value loaded from the
            // slot, field 1 is the success flag. The Kāra surface returns
            // `Ok(prev)` on success / `Err(actual)` on failure — both payloads
            // are the loaded value, so the ONLY thing that varies is the tag.
            // Result's tags are `Ok = 1`, `Err = 0`, which is exactly
            // `zext(success_i1)` — so the Result aggregate is built directly with
            // no branch: tag = the success bit, payload word 0 = the loaded
            // value. Integer-only for v1 (`Atomic[bool]` rejected — its i8/i1
            // round-trip through the Result payload is a follow-on).
            "compare_exchange" => {
                if args.len() != 4 {
                    return Err(format!(
                        "codegen: Atomic.compare_exchange takes (old, new, success, failure), \
                         got {} args",
                        args.len()
                    ));
                }
                if inner_is_bool {
                    return Err(
                        "codegen: Atomic[bool].compare_exchange is not supported in v1 \
                         (use `swap` / `load` / `store` for bool flags); CAS on bool is a \
                         tracked follow-on"
                            .to_string(),
                    );
                }
                let expected = self.compile_expr(&args[0].value)?;
                let new_val = self.compile_expr(&args[1].value)?;
                let success_ord = self.parse_memory_ordering(&args[2].value)?;
                let failure_ord = self.parse_memory_ordering(&args[3].value)?;
                // LLVM forbids Release / AcqRel as the *failure* ordering (it is
                // the load-only path — no store happens on failure).
                if matches!(
                    failure_ord,
                    AtomicOrdering::Release | AtomicOrdering::AcquireRelease
                ) {
                    return Err(format!(
                        "codegen: Atomic.compare_exchange rejects MemoryOrdering.{:?} as the \
                         failure ordering (LLVM forbids Release / AcqRel on the no-store path); \
                         use Relaxed / Acquire / SeqCst",
                        failure_ord
                    ));
                }
                let (exp_int, new_int) = match (expected, new_val) {
                    (BasicValueEnum::IntValue(a), BasicValueEnum::IntValue(b)) => (a, b),
                    _ => {
                        return Err(
                            "codegen: Atomic.compare_exchange requires integer old/new values"
                                .to_string(),
                        )
                    }
                };
                let cmpxchg = self
                    .builder
                    .build_cmpxchg(storage_ptr, exp_int, new_int, success_ord, failure_ord)
                    .map_err(|e| format!("codegen: build_cmpxchg failed: {:?}", e))?;
                // `cmpxchg` yields `{ T, i1 }` — extract the loaded value + flag.
                let loaded = self
                    .builder
                    .build_extract_value(cmpxchg, 0, "cas.loaded")
                    .unwrap();
                let success = self
                    .builder
                    .build_extract_value(cmpxchg, 1, "cas.ok")
                    .unwrap()
                    .into_int_value();
                // Build the Result[T, T] aggregate: tag = the success bit
                // (Ok=1 / Err=0), payload word 0 = the loaded value.
                let i64_t = self.context.i64_type();
                let result_layout = self
                    .enum_layouts
                    .get("Result")
                    .ok_or_else(|| "codegen: Result enum layout not registered".to_string())?;
                let result_ty = result_layout.llvm_type;
                let payload_words = result_ty.count_fields().saturating_sub(1);
                let tag = self
                    .builder
                    .build_int_z_extend(success, i64_t, "cas.tag")
                    .unwrap();
                let loaded_word = self.coerce_to_i64(loaded)?;
                let mut agg = result_ty.get_undef();
                agg = self
                    .builder
                    .build_insert_value(agg, tag, 0, "cas.res.tag")
                    .unwrap()
                    .into_struct_value();
                agg = self
                    .builder
                    .build_insert_value(agg, loaded_word, 1, "cas.res.val")
                    .unwrap()
                    .into_struct_value();
                // Zero-fill the remaining payload words so the aggregate carries
                // no `undef` past the single value word (Result is sized for its
                // widest payload; a CAS value occupies only word 0).
                for w in 2..=payload_words {
                    agg = self
                        .builder
                        .build_insert_value(agg, i64_t.const_zero(), w, "cas.res.pad")
                        .unwrap()
                        .into_struct_value();
                }
                Ok(agg.into())
            }
            _ => unreachable!(
                "compile_atomic_method gated on method in {{load, store, fetch_add, fetch_sub, \
                 fetch_and, fetch_or, fetch_xor, swap, compare_exchange}}"
            ),
        }
    }

    /// Resolve a `lock` place expression to the `(Mutex struct type, pointer to
    /// the aggregate)` pair. Handles the two place shapes: an `Identifier` (a
    /// local / par-captured `Mutex` binding — its `VarSlot` IS the aggregate)
    /// and a `FieldAccess` on a `par` / `shared` struct (a `Mutex` field stored
    /// inline in the heap layout — GEP at `field_idx + 1`, reusing the
    /// shared-field deref the atomic-field path uses).
    fn resolve_mutex_storage(
        &mut self,
        mutex: &Expr,
    ) -> Result<
        (
            inkwell::types::StructType<'ctx>,
            inkwell::values::PointerValue<'ctx>,
        ),
        String,
    > {
        match &mutex.kind {
            ExprKind::Identifier(name) => {
                let slot = self.variables.get(name).copied().ok_or_else(|| {
                    format!("codegen: lock target '{}' has no storage slot", name)
                })?;
                // A `ref`/`mut ref Mutex[T]` parameter: the alloca holds a
                // pointer TO the aggregate, and the pointee `{ lockflag, value }`
                // struct type is recorded in `ref_params`. Load through the ref.
                if let Some(&BasicTypeEnum::StructType(st)) = self.ref_params.get(name) {
                    if st.count_fields() == 2 {
                        let agg_ptr = self
                            .builder
                            .build_load(slot.ty, slot.ptr, "mutex.ref.load")
                            .map_err(|e| format!("codegen: lock ref-param load failed: {:?}", e))?
                            .into_pointer_value();
                        return Ok((st, agg_ptr));
                    }
                }
                // A directly-bound (or par-captured) local: the slot IS the
                // aggregate.
                match slot.ty {
                    BasicTypeEnum::StructType(st) if st.count_fields() == 2 => Ok((st, slot.ptr)),
                    other => Err(format!(
                        "codegen: lock target '{}' is not a Mutex[T] (slot type {:?})",
                        name, other
                    )),
                }
            }
            ExprKind::FieldAccess {
                object: inner,
                field,
            } => {
                // `lock self.state` — `self.state` is a `Mutex` field stored
                // inline in the `par`/`shared` struct's heap aggregate
                // `{ i64 refcount, …, { i64 lockflag, T value }, … }`.
                let (type_name, info) = self.shared_type_for_expr(inner).ok_or_else(|| {
                    format!(
                        "codegen: lock field receiver '.{}' is not on a par/shared struct",
                        field
                    )
                })?;
                let idx = self
                    .struct_field_names
                    .get(&type_name)
                    .and_then(|names| names.iter().position(|n| n == field))
                    .ok_or_else(|| {
                        format!("codegen: struct '{}' has no field '{}'", type_name, field)
                    })?;
                let heap_ptr = self.compile_expr(inner)?.into_pointer_value();
                let field_ptr = self
                    .builder
                    .build_struct_gep(
                        info.heap_type,
                        heap_ptr,
                        (idx + 1) as u32, // +1: heap index 0 is the refcount
                        "mutex.field.ptr",
                    )
                    .map_err(|e| format!("codegen: lock field gep failed: {:?}", e))?;
                match info.heap_type.get_field_type_at_index((idx + 1) as u32) {
                    Some(BasicTypeEnum::StructType(st)) if st.count_fields() == 2 => {
                        Ok((st, field_ptr))
                    }
                    other => Err(format!(
                        "codegen: lock field '{}.{}' is not a Mutex[T] (field type {:?})",
                        type_name, field, other
                    )),
                }
            }
            other => Err(format!(
                "codegen: unsupported lock place expression {:?}",
                std::mem::discriminant(other)
            )),
        }
    }

    /// Codegen for `lock <place> [alias] { body }` (design.md § Part 5: Shared
    /// Types, `lock` blocks). `place` names a `Mutex[T]` laid out as
    /// `{ i64 lockflag, T value }` (a local binding or a `par`/`shared` struct
    /// field). Emits a TAS spinlock: acquire by `atomicrmw xchg`-ing the flag to
    /// 1 and spinning until the previous value was 0; expose the value field as a
    /// `mut ref T` binding (the `alias`, or the mutex name itself shadowed for an
    /// `Identifier` place) for the body; release by atomically storing 0.
    /// Straight-line only — the typechecker rejects early exits from the body,
    /// so the single fall-through release is sound.
    pub(super) fn compile_lock_block(
        &mut self,
        mutex: &Expr,
        alias: Option<&str>,
        body: &Block,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let (mutex_struct, base_ptr) = self.resolve_mutex_storage(mutex)?;
        let flag_ptr = self
            .builder
            .build_struct_gep(mutex_struct, base_ptr, 0, "mutex.flag.ptr")
            .map_err(|e| format!("codegen: lock flag gep failed: {:?}", e))?;
        let value_ptr = self
            .builder
            .build_struct_gep(mutex_struct, base_ptr, 1, "mutex.val.ptr")
            .map_err(|e| format!("codegen: lock value gep failed: {:?}", e))?;
        let value_ty = mutex_struct.get_field_type_at_index(1).unwrap();

        let i64_t = self.context.i64_type();
        let current_fn = self.current_fn.unwrap();
        let contended_bb = self
            .context
            .append_basic_block(current_fn, "lock.contended");
        let held_bb = self.context.append_basic_block(current_fn, "lock.held");
        let after_bb = self.context.append_basic_block(current_fn, "lock.after");

        // Acquire — futex 3-state fast path (0 = free, 1 = locked-uncontended,
        // 2 = locked-contended). `cmpxchg(0 -> 1)`: on success we hold the lock
        // with NO runtime call — the uncontended path stays fully inline, at
        // roughly the old spinlock's cost, so this is a pure no-regression win
        // for the common case. On failure (someone else holds it) branch to the
        // contended path, which blocks in the runtime parking lot (marking the
        // flag `2`) instead of burning CPU spinning. Release lives in
        // `CleanupAction::ReleaseMutex` (`runtime.rs`): `xchg(-> 0)` + wake iff
        // the prior state was `2`.
        let cas = self
            .builder
            .build_cmpxchg(
                flag_ptr,
                i64_t.const_zero(),
                i64_t.const_int(1, false),
                AtomicOrdering::SequentiallyConsistent,
                AtomicOrdering::SequentiallyConsistent,
            )
            .map_err(|e| format!("codegen: lock acquire cmpxchg failed: {:?}", e))?;
        let acquired = self
            .builder
            .build_extract_value(cas, 1, "lock.acquired")
            .unwrap()
            .into_int_value();
        self.builder
            .build_conditional_branch(acquired, held_bb, contended_bb)
            .unwrap();

        // Contended — block in the runtime until we hold the lock. The fast
        // cmpxchg already failed; `karac_runtime_mutex_lock` re-tries under a
        // bucketed condvar (Drepper's protocol) and returns holding the lock.
        self.builder.position_at_end(contended_bb);
        let lock_fn = self
            .module
            .get_function("karac_runtime_mutex_lock")
            .expect("karac_runtime_mutex_lock declared in Codegen::new");
        self.builder
            .build_call(lock_fn, &[flag_ptr.into()], "lock.wait")
            .unwrap();
        self.builder.build_unconditional_branch(held_bb).unwrap();

        // Critical section.
        self.builder.position_at_end(held_bb);
        // Bind the body's inner-value name (the alias, or — for an `Identifier`
        // place — the mutex name shadowed) to the value slot: a `mut ref T`
        // whose storage IS the mutex's value field, so the body's reads /
        // writes / field accesses operate in place under the lock. A field
        // place without an alias is rejected by the typechecker.
        let bind_name = match (alias, &mutex.kind) {
            (Some(a), _) => Some(a.to_string()),
            (None, ExprKind::Identifier(n)) => Some(n.clone()),
            (None, _) => None,
        };
        let saved = bind_name
            .as_ref()
            .and_then(|n| self.variables.get(n).copied());
        if let Some(ref name) = bind_name {
            self.variables.insert(
                name.clone(),
                super::VarSlot {
                    ptr: value_ptr,
                    ty: value_ty,
                },
            );
        }
        // Seed a cleanup frame whose bottom action is the lock release, so the
        // release rides the normal scope-cleanup machinery and fires on EVERY
        // exit path — not just the straight-line fall-through. The body's own
        // scope cleanups (Vec frees, RC-decs, drops, user `defer`s) stack ABOVE
        // the release on this frame, so a drain runs them first and releases
        // last (reverse-construction RAII: drop body resources under the lock,
        // then unlock). `flag_ptr` was GEP'd in the lock's entry block, so it
        // dominates every body BB and the re-emitted store at a break/continue/
        // return site is well-formed. This is what retires the `LockEarlyExit`
        // (`E0259`) typechecker rejection — early exits from a lock body are now
        // legal and release the lock on the way out.
        self.scope_cleanup_actions
            .push(vec![super::state::CleanupAction::ReleaseMutex { flag_ptr }]);

        let body_val = self.compile_block(body)?;
        // Restore the shadowed binding (mutex name) / drop the alias. This is
        // compile-time `self.variables` bookkeeping and is correct on the
        // early-exit path too (the IR has already branched away; only the
        // symbol table is restored for the code that follows the lock).
        if let Some(ref name) = bind_name {
            match saved {
                Some(s) => {
                    self.variables.insert(name.clone(), s);
                }
                None => {
                    self.variables.remove(name);
                }
            }
        }

        // Drain the release frame. On straight-line fall-through the body block
        // has no terminator, so emit the body cleanups + release here and branch
        // to `after_bb`. On an early exit the body block is already terminated
        // (break/continue ran `emit_scope_cleanup_from`, return ran
        // `emit_scope_cleanup` — both walked this frame and emitted the release
        // before branching), so just pop the now-drained frame. `after_bb` is
        // then dead-but-filled by trailing code / the function epilogue, exactly
        // as `compile_loop`'s exit block is for a no-break loop.
        let body_terminated = self
            .builder
            .get_insert_block()
            .unwrap()
            .get_terminator()
            .is_some();
        if !body_terminated {
            self.drain_top_frame_with_emit();
            self.builder.build_unconditional_branch(after_bb).unwrap();
        } else {
            self.scope_cleanup_actions.pop();
        }
        self.builder.position_at_end(after_bb);

        Ok(body_val.unwrap_or_else(|| i64_t.const_int(0, false).into()))
    }

    /// Recover the (storage pointer, element LLVM type) pair for an
    /// `Atomic[T]` receiver. Identifier path reads from `variables`;
    /// FieldAccess path GEPs to the struct field. Element type is the
    /// LLVM type of the inner primitive (Atomic[T] is laid out
    /// transparently as T — see `llvm_type_for_type_expr`'s Atomic
    /// arm).
    fn resolve_atomic_storage(
        &mut self,
        object: &Expr,
    ) -> Result<
        (
            inkwell::values::PointerValue<'ctx>,
            BasicTypeEnum<'ctx>,
            bool,
        ),
        String,
    > {
        match &object.kind {
            ExprKind::Identifier(name) => {
                let slot =
                    self.variables.get(name.as_str()).copied().ok_or_else(|| {
                        format!("codegen: Atomic receiver '{}' has no slot", name)
                    })?;
                let is_bool = self.atomic_var_inner_is_bool.contains(name.as_str());
                Ok((slot.ptr, slot.ty, is_bool))
            }
            ExprKind::FieldAccess {
                object: inner,
                field,
            } => {
                // `shared`/`par` struct field receiver — e.g. `self.count.load(..)`
                // on a `par struct Counter { count: Atomic[i64] }`. These live in
                // `shared_types` (heap layout `{ i64 refcount, fields... }`), NOT
                // `struct_types`, so the plain path below would error with "no LLVM
                // type". Reuse the proven shared field-read deref: `compile_expr(inner)`
                // yields the heap pointer (handling the `ref self` ptr-to-heap-ptr
                // load), then GEP at `idx + 1` (index 0 is the refcount) into the
                // heap type. The field slot IS the transparent `Atomic[T]` = `T`
                // storage the atomic load/store operates on. Mirrors the shared
                // field-read path in `expr_ops.rs::compile_field_access`.
                if let Some((type_name, info)) = self.shared_type_for_expr(inner) {
                    if !info.is_enum {
                        if let Some(idx) = self
                            .struct_field_names
                            .get(&type_name)
                            .and_then(|names| names.iter().position(|n| n == field))
                        {
                            let heap_ptr = self.compile_expr(inner)?.into_pointer_value();
                            let field_ptr = self
                                .builder
                                .build_struct_gep(
                                    info.heap_type,
                                    heap_ptr,
                                    (idx + 1) as u32,
                                    "atomic.sh_field.ptr",
                                )
                                .map_err(|e| format!("codegen: struct_gep failed: {:?}", e))?;
                            let elem_ty = info
                                .heap_type
                                .get_field_type_at_index((idx + 1) as u32)
                                .ok_or_else(|| {
                                    format!(
                                        "codegen: shared/par struct '{}' field {} out of range",
                                        type_name, idx
                                    )
                                })?;
                            let inner_is_bool = self
                                .struct_field_type_exprs
                                .get(&type_name)
                                .and_then(|fields| fields.get(idx))
                                .map(super::types_lowering::is_atomic_bool_type_expr)
                                .unwrap_or(false);
                            return Ok((field_ptr, elem_ty, inner_is_bool));
                        }
                    }
                }
                let obj_ty_name = self.type_name_of_expr(inner).ok_or_else(|| {
                    format!(
                        "codegen: Atomic field receiver '.{}' has unknown object type",
                        field
                    )
                })?;
                let field_names = self
                    .struct_field_names
                    .get(obj_ty_name.as_str())
                    .cloned()
                    .ok_or_else(|| {
                        format!("codegen: struct '{}' has no registered fields", obj_ty_name)
                    })?;
                let idx = field_names.iter().position(|n| n == field).ok_or_else(|| {
                    format!("codegen: struct '{}' has no field '{}'", obj_ty_name, field)
                })? as u32;
                let struct_ty = *self.struct_types.get(obj_ty_name.as_str()).ok_or_else(|| {
                    format!(
                        "codegen: struct '{}' has no LLVM type (shared structs not \
                             supported as Atomic field receivers)",
                        obj_ty_name
                    )
                })?;
                let inner_name = if let ExprKind::Identifier(n) = &inner.kind {
                    n.clone()
                } else {
                    return Err(format!(
                        "codegen: Atomic FieldAccess receiver must be `<identifier>.{}` \
                         in v1 (got nested receiver)",
                        field
                    ));
                };
                let base_ptr = self.get_data_ptr(&inner_name).ok_or_else(|| {
                    format!(
                        "codegen: Atomic field receiver base '{}' has no storage ptr",
                        inner_name
                    )
                })?;
                let field_ptr = self
                    .builder
                    .build_struct_gep(struct_ty, base_ptr, idx, "atomic.field.ptr")
                    .map_err(|e| format!("codegen: struct_gep failed: {:?}", e))?;
                let elem_ty = struct_ty.get_field_type_at_index(idx).ok_or_else(|| {
                    format!(
                        "codegen: struct '{}' field {} index out of range",
                        obj_ty_name, idx
                    )
                })?;
                // Inner-is-bool detection for struct fields reads the
                // full per-field TypeExpr registered at struct
                // declaration time. Fields ALWAYS carry their
                // annotation (declaration syntax requires it), so this
                // path is exact — no missing-info fallback needed.
                let inner_is_bool = self
                    .struct_field_type_exprs
                    .get(obj_ty_name.as_str())
                    .and_then(|fields| fields.get(idx as usize))
                    .map(super::types_lowering::is_atomic_bool_type_expr)
                    .unwrap_or(false);
                Ok((field_ptr, elem_ty, inner_is_bool))
            }
            _ => Err(format!(
                "codegen: Atomic method receiver shape {:?} not supported in v1",
                std::mem::discriminant(&object.kind)
            )),
        }
    }

    /// Parse the canonical `MemoryOrdering.X` qualified-variant
    /// expression into an `inkwell::AtomicOrdering`. Mirrors the
    /// interpreter's `MemoryOrdering` qualified-variant recognizer at
    /// `src/interpreter/eval_call.rs:474+`. The Kāra surface spelling
    /// for `Relaxed` maps to LLVM's `Monotonic`; all others map by
    /// name.
    fn parse_memory_ordering(&self, expr: &Expr) -> Result<AtomicOrdering, String> {
        if let ExprKind::Path { segments, .. } = &expr.kind {
            if segments.len() == 2 && segments[0] == "MemoryOrdering" {
                return match segments[1].as_str() {
                    "Relaxed" => Ok(AtomicOrdering::Monotonic),
                    "Acquire" => Ok(AtomicOrdering::Acquire),
                    "Release" => Ok(AtomicOrdering::Release),
                    "AcqRel" => Ok(AtomicOrdering::AcquireRelease),
                    "SeqCst" => Ok(AtomicOrdering::SequentiallyConsistent),
                    other => Err(format!(
                        "codegen: unknown MemoryOrdering variant '{}'",
                        other
                    )),
                };
            }
        }
        Err(
            "codegen: Atomic.load / .store ordering arg must be a MemoryOrdering.X variant literal"
                .to_string(),
        )
    }

    /// Slice 3 of the strict-provenance work (line 511). Lower one of
    /// the seven `ptr.*` module functions to its LLVM cast counterpart.
    /// Returns `Ok(None)` for an unknown method so the caller's
    /// fall-through diagnostic stays in place; the typechecker has
    /// already accepted only the seven valid names so reaching `None`
    /// here means a real codegen bug rather than a user error.
    ///
    /// **ABI note.** The current codegen lowers `*const T` / `*mut T`
    /// to LLVM `i64` at function-signature and binding-slot boundaries
    /// (see `llvm_type_for_type_expr` — raw pointer kinds fall through
    /// to the `i64` default). Under that ABI all four ptr↔int casts in
    /// the strict-provenance API are *identity at the LLVM level*: the
    /// address bits already round-trip losslessly through the i64 slot
    /// that holds the raw pointer. The pragmatic lowering here mirrors
    /// that — emit a no-op (when both sides are already i64) or a
    /// `ptrtoint` (when the receiver happens to flow as an LLVM
    /// pointer-typed SSA, which can happen for some intermediate
    /// values). The provenance-preserving lowering the spec describes
    /// (`ptrtoint`+`!provenance.preserve` markers; `inttoptr` with
    /// `noalias` invalidation for the `expose` family) requires
    /// raw-pointer-typed LLVM slots end-to-end — that uplift is
    /// tracked as a follow-up. Tests in `tests/codegen.rs` pin the
    /// runtime round-trip; the IR-shape pins live alongside.
    fn compile_ptr_module_call(
        &mut self,
        method: &str,
        args: &[CallArg],
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        let i64_ty = self.context.i64_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        // Raw pointers lower to genuine LLVM `ptr` since the CStr/as_ptr
        // slice lifted `TypeKind::Pointer` off the historical i64
        // fall-through (see `llvm_type_for_type_expr`) — the "deferred
        // refinement" the original i64-ABI lowering here anticipated.
        // ptr→usize ops emit `ptrtoint`, usize→ptr ops emit `inttoptr`,
        // exactly the spec's provenance story (design.md § Pointer
        // Provenance; the `!provenance` metadata refinement remains
        // open). The two coercion helpers absorb either value shape so
        // intermediate results that still flow as integers (e.g. a
        // usize-typed local) compose with pointer-typed params.
        let to_i64 =
            |this: &mut Self, v: BasicValueEnum<'ctx>, label: &str| -> BasicValueEnum<'ctx> {
                match v {
                    BasicValueEnum::PointerValue(pv) => this
                        .builder
                        .build_ptr_to_int(pv, i64_ty, label)
                        .unwrap()
                        .into(),
                    BasicValueEnum::IntValue(_) => v,
                    _ => v,
                }
            };
        let to_ptr =
            |this: &mut Self, v: BasicValueEnum<'ctx>, label: &str| -> BasicValueEnum<'ctx> {
                match v {
                    BasicValueEnum::IntValue(iv) => this
                        .builder
                        .build_int_to_ptr(iv, ptr_ty, label)
                        .unwrap()
                        .into(),
                    BasicValueEnum::PointerValue(_) => v,
                    _ => v,
                }
            };
        match method {
            // p: *_ T -> usize  (ptr.addr / ptr.expose / ptr.expose_mut)
            "addr" | "expose" | "expose_mut" if args.len() == 1 => {
                let p = self.compile_expr(&args[0].value)?;
                let label = match method {
                    "addr" => "ptr.addr",
                    "expose" => "ptr.expose",
                    _ => "ptr.expose_mut",
                };
                Ok(Some(to_i64(self, p, label)))
            }
            // (p: *_ T, addr: usize) -> *_ T  (ptr.with_addr / ptr.with_addr_mut)
            //
            // Compile the first arg for side effects only — a
            // provenance-aware lowering would consult `p`'s
            // `!provenance` metadata to reseat the address bits; until
            // that metadata lands, the result is just `addr` reseated
            // into a pointer via `inttoptr`.
            "with_addr" | "with_addr_mut" if args.len() == 2 => {
                let _ = self.compile_expr(&args[0].value)?;
                let a = self.compile_expr(&args[1].value)?;
                let label = if method == "with_addr" {
                    "ptr.with_addr"
                } else {
                    "ptr.with_addr_mut"
                };
                Ok(Some(to_ptr(self, a, label)))
            }
            // addr: usize -> *_ T  (ptr.from_exposed / ptr.from_exposed_mut)
            "from_exposed" | "from_exposed_mut" if args.len() == 1 => {
                let a = self.compile_expr(&args[0].value)?;
                let label = if method == "from_exposed" {
                    "ptr.from_exposed"
                } else {
                    "ptr.from_exposed_mut"
                };
                Ok(Some(to_ptr(self, a, label)))
            }
            // (field_ptr: *_ F, offset: usize) -> *_ T
            //   (ptr.container_of / ptr.container_of_mut)
            //
            // Intrusive-DS pointer recovery — subtract the field
            // offset from the field-pointer's address bits. The
            // provenance-preserving lowering the spec describes is
            // `field_ptr.with_addr(field_ptr.addr() - offset)`, which
            // is exactly the `ptrtoint` → integer subtract → `inttoptr`
            // sequence emitted here.
            "container_of" | "container_of_mut" if args.len() == 2 => {
                let field_ptr_val = self.compile_expr(&args[0].value)?;
                let offset_val = self.compile_expr(&args[1].value)?;
                let label = if method == "container_of" {
                    "ptr.container_of"
                } else {
                    "ptr.container_of_mut"
                };
                let field_ptr_i64 = to_i64(self, field_ptr_val, &format!("{label}.fp"));
                let offset_i64 = to_i64(self, offset_val, &format!("{label}.off"));
                let result = self
                    .builder
                    .build_int_sub(
                        field_ptr_i64.into_int_value(),
                        offset_i64.into_int_value(),
                        &format!("{label}.bits"),
                    )
                    .unwrap();
                Ok(Some(to_ptr(self, result.into(), label)))
            }
            // `ptr.const(place)` / `ptr.mut(place)` — raw pointer
            // construction from a place expression (typechecker
            // place-validator gate is upstream — design.md § Raw
            // Pointer Construction, v60 item 19). The result is the
            // place's storage address as a genuine `ptr` value. v1
            // covers the two common shapes:
            //  - Identifier place: look up the binding's storage
            //    slot via `get_data_ptr` (handles owned alloca and
            //    ref-param indirection) — that slot pointer IS the
            //    result, no cast needed.
            //  - Deref of an already-pointer SSA: the operand's value
            //    *is* the address; reseat through `to_ptr` in case it
            //    still flows as an integer.
            // Field / index / nested-deref places fall through to
            // the generic identifier path via the synth-identifier
            // mechanism if reachable; a focused diagnostic for
            // unsupported shapes lands as a follow-up.
            "const" | "mut" if args.len() == 1 => {
                let place = &args[0].value;
                match &place.kind {
                    ExprKind::Identifier(name) => {
                        if let Some(ptr) = self.get_data_ptr(name) {
                            return Ok(Some(ptr.into()));
                        }
                        // Identifier didn't resolve to a binding (e.g.
                        // a free function name reached here). Fall
                        // through to the generic method-call path,
                        // which will surface its own diagnostic.
                        Ok(None)
                    }
                    ExprKind::Unary {
                        op: UnaryOp::Deref,
                        operand,
                    } => {
                        let v = self.compile_expr(operand)?;
                        Ok(Some(to_ptr(self, v, "ptr.place.deref")))
                    }
                    _ => Ok(None),
                }
            }
            // `ptr.null[T]()` / `ptr.null_mut[T]()` -> the all-zeroes
            // pointer (LLVM `ptr null`). The two methods differ only
            // in their typechecker-reported return type (`*const T`
            // vs `*mut T`); the codegen value is identical.
            "null" | "null_mut" if args.is_empty() => Ok(Some(ptr_ty.const_null().into())),
            // `ptr.dangling[T]()` / `ptr.dangling_mut[T]()` -> a
            // non-null pointer aligned to T's natural alignment, *not*
            // dereferenceable. Spec: design.md § Raw Pointer
            // Construction (v60 item 19); mirrors Rust's
            // `NonNull::dangling` (= `align_of::<T>() as *const T`).
            //
            // T-aware lowering would consult the type argument and
            // emit `align_of[T]`. The type argument is not threaded to
            // this hook, so v1 emits a fixed alignment of 8 (the max
            // alignment of any built-in primitive on a 64-bit target —
            // correct for every T whose alignment is <= 8, conservative
            // for over-aligned SIMD / `#[repr(align(N))]` types),
            // reseated into a `ptr` via constant `inttoptr`. The actual
            // deref of a dangling pointer is unsafe and *always* UB; the
            // only observable property is non-null + alignment, both of
            // which hold here. Tracker: phase-5-diagnostics line 573.
            "dangling" | "dangling_mut" if args.is_empty() => Ok(Some(
                i64_ty.const_int(8, false).const_to_pointer(ptr_ty).into(),
            )),
            // `ptr.is_null[T](p)` -> `p == 0` as bool (i1). The
            // typechecker reports the result as `Type::Bool`; codegen
            // returns an i1 matching how the BinOp::Eq path produces
            // bool values (`build_int_compare(EQ, ...)`).
            "is_null" if args.len() == 1 => {
                let p = self.compile_expr(&args[0].value)?;
                let p_i64 = to_i64(self, p, "ptr.is_null.p");
                let zero = i64_ty.const_zero();
                let result = self
                    .builder
                    .build_int_compare(
                        IntPredicate::EQ,
                        p_i64.into_int_value(),
                        zero,
                        "ptr.is_null",
                    )
                    .unwrap();
                Ok(Some(result.into()))
            }
            _ => Ok(None),
        }
    }

    /// `Vector[T, N].splat(x)` — broadcast scalar `x` to all `N` lanes
    /// (design.md § Portable SIMD). Compile the scalar once and
    /// `insertelement` it into every lane of an undef `<N x T>`; LLVM folds
    /// the chain into a native broadcast (`shufflevector` w/ zero mask) on
    /// targets that have one.
    fn compile_vector_splat(
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
    fn compile_vector_from_array(
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
        let lanes: Vec<BasicValueEnum<'ctx>> =
            if let ExprKind::ArrayLiteral(elems) = &args[0].value.kind {
                elems
                    .iter()
                    .map(|e| self.compile_expr(e))
                    .collect::<Result<_, _>>()?
            } else {
                let arr = self.compile_expr(&args[0].value)?;
                let agg = arr.into_array_value();
                (0..n)
                    .map(|i| {
                        self.builder
                            .build_extract_value(agg, i, "from_array.lane")
                            .map_err(|e| format!("from_array extractvalue failed: {e}"))
                    })
                    .collect::<Result<_, _>>()?
            };
        let i32_ty = self.context.i32_type();
        let mut acc = vt.get_undef();
        for (i, val) in lanes.iter().enumerate() {
            // Literal-width boundary coercion for the array-literal arm
            // (a bare `0.5` element lowers as f64); no-op for the
            // aggregate arm's already-`T`-typed extracts.
            let val = self.coerce_scalar_to_type(*val, vt.get_element_type());
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
    fn compile_vector_from_slice(
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
    fn compile_vector_load_masked(
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
    fn compile_vector_gather(
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
    fn compile_vector_cast_from(
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
    /// typechecker guarantees `N >= 1`, an integer element for the bitwise
    /// folds, and a same-typed vector argument for `dot`.
    fn compile_vector_method(
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
}

/// Map a bare lowercase ambient-resource alias (`env`, `clock`, ...) to
/// its capitalized effect-resource name, mirroring the interpreter's
/// alias table in `src/interpreter/method_call.rs`. Returns `None` for
/// any identifier that is not an ambient resource alias. Codegen lowers
/// only the subset the runtime currently backs (see
/// `compile_ambient_resource_method`); the rest still resolve here so
/// they get a precise "not yet lowered" error rather than the generic
/// dispatch fall-through.
fn ambient_resource_for_alias(alias: &str) -> Option<&'static str> {
    match alias {
        "clock" => Some("Clock"),
        "env" => Some("Env"),
        "rand" => Some("RandomSource"),
        "stdin" => Some("Stdin"),
        "stdout" => Some("Stdout"),
        "stderr" => Some("Stderr"),
        "fs" => Some("FileSystem"),
        _ => None,
    }
}

/// Vtable slot index of `method` within `resource`'s canonical method
/// order (`prelude::AMBIENT_RESOURCE_METHODS`), or `None` if the pair has
/// no slot — in which case there's no runtime override dispatch for it
/// and the call falls straight to the FFI default.
pub(super) fn ambient_method_index(resource: &str, method: &str) -> Option<usize> {
    crate::prelude::AMBIENT_RESOURCE_METHODS
        .iter()
        .find(|(r, _)| *r == resource)
        .and_then(|(_, methods)| methods.iter().position(|m| *m == method))
}

/// True iff `compile_ambient_ffi` has a builtin-default lowering for this
/// `(resource, method)` pair. MUST stay in lockstep with that match's arms.
///
/// Used to route a capitalized `Resource.method()` call (`call_dispatch.rs`)
/// to `compile_ambient_resource_method` even when the pair has no
/// `AMBIENT_RESOURCE_METHODS` vtable slot — i.e. FFI-default methods like
/// `RandomSource.next_u64` / `Env.args`. Without this, only the lowercase
/// alias form (`rand.next_u64()`, routed in `compile_method_call`) reached
/// the FFI lowering; the capitalized form fell through to `compile_assoc_call`
/// and errored "no handler". (Vtable-slotted pairs — `Clock.now`, `Env.set` —
/// are already routed by the `ambient_method_index` check at the call site;
/// this is purely the no-slot complement.)
pub(super) fn ambient_ffi_lowered(resource: &str, method: &str) -> bool {
    matches!(
        (resource, method),
        ("Env", "set")
            | ("Clock", "now")
            | ("RandomSource", "next_u64")
            | ("Env", "args")
            | ("Env", "var")
            | ("Stdin", "read_line")
            | ("Stdin", "read_to_string")
            | ("Stdout", "print")
            | ("Stdout", "println")
            | ("Stdout", "flush")
            | ("Stderr", "print")
            | ("Stderr", "println")
            | ("Stderr", "flush")
    )
}
