//! Function-call and method-call compilation.
//!
//! Houses `compile_assoc_call` (associated/free fn dispatch),
//! `compile_method_call` (object method dispatch including the big
//! per-receiver-type dispatch table), `compile_indexed_receiver_method`
//! (slice/vec indexed-receiver methods), `compile_for_indexed_iter`
//! (for-loop iteration over indexed sources), `compile_nested_index_read`
//! (`a[i][j]`-style chained index reads), `compile_entry_chain_receiver_method`
//! (map `entry().or_insert(...)` chains), the `lower_indexed_elem_ptr_*`
//! helpers (`vec`, `slice`, `array`), and `inferred_receiver_type` for
//! method-call receiver type recovery.

use crate::ast::*;

use inkwell::module::Linkage;
use inkwell::types::{BasicMetadataTypeEnum, BasicType, BasicTypeEnum};
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum, PointerValue};
use inkwell::AddressSpace;
use inkwell::IntPredicate;

use super::state::VarSlot;

impl<'ctx> super::Codegen<'ctx> {
    pub(super) fn compile_assoc_call(
        &mut self,
        type_name: &str,
        method: &str,
        _args: &[CallArg],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let args = _args;
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
                    let synth = Expr {
                        span: _args[0].value.span.clone(),
                        kind: ExprKind::Binary {
                            op,
                            left: Box::new(_args[0].value.clone()),
                            right: Box::new(_args[1].value.clone()),
                        },
                    };
                    return self.compile_expr(&synth);
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
                if let Some(v) = self.try_compile_enum_variant(method, _args)? {
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
            let mut compiled_args: Vec<BasicMetadataValueEnum<'ctx>> = Vec::new();
            for (i, a) in _args.iter().enumerate() {
                let is_ref = ref_flags.get(i).copied().unwrap_or(false);
                if is_ref {
                    if let ExprKind::Identifier(var_name) = &a.value.kind {
                        if let Some(ptr) = self.get_data_ptr(var_name) {
                            compiled_args.push(ptr.into());
                            continue;
                        }
                    }
                }
                compiled_args.push(self.compile_expr(&a.value)?.into());
            }
            let call_site = self
                .builder
                .build_call(fn_val, &compiled_args, "usercall")
                .unwrap();
            let basic_val = call_site.try_as_basic_value();
            return if basic_val.is_instruction() {
                Ok(self.context.i64_type().const_int(0, false).into())
            } else {
                Ok(basic_val.unwrap_basic())
            };
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
        // Limitation: the per-slot store is `build_store(elem_ptr, val)`
        // which is a bit-copy. For aggregate element types whose Kāra
        // semantics need deep clone per slot (matches the interpreter's
        // `deep_clone_value` fix at `beb7310` for nested-collection
        // element types), the bit-copy aliases storage. The kata's
        // `Vec.filled(cap + 1, Vec.new())` is safe because Vec.new
        // returns an empty `{null, 0, 0}` aggregate — every slot points
        // at null and the first `factors[j].push(...)` allocates a
        // fresh buffer per row. Non-empty aggregate element types
        // need a Clone-codegen upgrade (separate slice).
        if type_name == "Vec" && method == "filled" {
            if args.len() < 2 {
                return Err("Vec.filled requires 2 arguments (n, val)".to_string());
            }
            let n = self.compile_expr(&args[0].value)?.into_int_value();
            let val = self.compile_expr(&args[1].value)?;
            let elem_ty = val.get_type();
            let elem_size = elem_ty.size_of().unwrap();
            let i64_t = self.context.i64_type();
            let fn_val = self.current_fn.unwrap();

            // Allocate buffer: malloc(n * sizeof(elem)). `free(malloc(0))`
            // is well-defined; we don't special-case n == 0.
            let alloc_bytes = self
                .builder
                .build_int_mul(n, elem_size, "filled.alloc_bytes")
                .unwrap();
            let buf = self
                .builder
                .build_call(self.malloc_fn, &[alloc_bytes.into()], "filled.buf")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_pointer_value();

            // Fill loop: for i in 0..n { buf[i] = val }
            let counter = self.create_entry_alloca(fn_val, "filled.i", i64_t.into());
            self.builder
                .build_store(counter, i64_t.const_int(0, false))
                .unwrap();
            let cond_bb = self.context.append_basic_block(fn_val, "filled.cond");
            let body_bb = self.context.append_basic_block(fn_val, "filled.body");
            let exit_bb = self.context.append_basic_block(fn_val, "filled.exit");

            self.builder.build_unconditional_branch(cond_bb).unwrap();

            self.builder.position_at_end(cond_bb);
            let cur = self
                .builder
                .build_load(i64_t, counter, "filled.cur")
                .unwrap()
                .into_int_value();
            let cond = self
                .builder
                .build_int_compare(inkwell::IntPredicate::ULT, cur, n, "filled.lt")
                .unwrap();
            self.builder
                .build_conditional_branch(cond, body_bb, exit_bb)
                .unwrap();

            self.builder.position_at_end(body_bb);
            let elem_ptr = unsafe {
                self.builder
                    .build_gep(elem_ty, buf, &[cur], "filled.elem.ptr")
                    .unwrap()
            };
            self.builder.build_store(elem_ptr, val).unwrap();
            let one = i64_t.const_int(1, false);
            let next = self.builder.build_int_add(cur, one, "filled.next").unwrap();
            self.builder.build_store(counter, next).unwrap();
            self.builder.build_unconditional_branch(cond_bb).unwrap();

            self.builder.position_at_end(exit_bb);

            // Build {data=buf, len=n, cap=n} aggregate.
            let vec_ty = self.vec_struct_type();
            let mut agg = vec_ty.get_undef();
            agg = self
                .builder
                .build_insert_value(agg, buf, 0, "vec.data")
                .unwrap()
                .into_struct_value();
            agg = self
                .builder
                .build_insert_value(agg, n, 1, "vec.len")
                .unwrap()
                .into_struct_value();
            agg = self
                .builder
                .build_insert_value(agg, n, 2, "vec.cap")
                .unwrap()
                .into_struct_value();
            return Ok(agg.into());
        }

        // `Vec.from_slice(src: Slice[T]) -> Vec[T]` — bulk-copy a slice
        // (also accepts Array / Vec via the existing `coerce_to_slice`
        // shape recognition) into a freshly-allocated Vec. One malloc +
        // one memcpy, vs the `Vec.new() + push-in-loop` shape which
        // grow-and-reallocs ~log2(n) times. Element type comes from the
        // source's compile-time element registry, so the arg must be a
        // named local (slice / vec / array binding); arbitrary expression
        // args are deferred to a follow-up since we'd need to consult the
        // typechecker's expr-type table to recover the element type.
        if type_name == "Vec" && method == "from_slice" {
            if args.len() != 1 {
                return Err(format!(
                    "Vec.from_slice expects 1 argument (source slice / vec / array), got {}",
                    args.len()
                ));
            }
            let arg = &args[0].value;
            let ExprKind::Identifier(src_name) = &arg.kind else {
                return Err(
                    "Vec.from_slice: source must currently be a named slice / vec / array variable"
                        .to_string(),
                );
            };

            // Recover element LLVM type from the source's binding.
            let elem_ty: BasicTypeEnum<'ctx> =
                if let Some(&t) = self.slice_elem_types.get(src_name.as_str()) {
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

            // Coerce arg into a slice header `{data, len}` — reuses the same
            // path-mapping that the rest of codegen uses for slice-typed
            // params (Array → slice header with stack-pointer + array len,
            // Vec → slice header with data-ptr + len field, Slice →
            // passthrough load).
            let slice_val = self.coerce_to_slice(arg, elem_ty)?.ok_or_else(|| {
                format!(
                    "Vec.from_slice: could not coerce '{}' to a slice header",
                    src_name
                )
            })?;
            let slice_sv = slice_val.into_struct_value();
            let src_data = self
                .builder
                .build_extract_value(slice_sv, 0, "from_slice.src.data")
                .unwrap()
                .into_pointer_value();
            let src_len = self
                .builder
                .build_extract_value(slice_sv, 1, "from_slice.src.len")
                .unwrap()
                .into_int_value();

            let elem_size = elem_ty.size_of().unwrap();
            let alloc_bytes = self
                .builder
                .build_int_mul(src_len, elem_size, "from_slice.bytes")
                .unwrap();
            let new_buf = self
                .builder
                .build_call(self.malloc_fn, &[alloc_bytes.into()], "from_slice.buf")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_pointer_value();
            self.builder
                .build_memcpy(new_buf, 8, src_data, 8, alloc_bytes)
                .unwrap();

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
            if let Some(value) =
                self.try_compile_field_receiver_method(inner, field, method, args, call_span)?
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
        if method == "clone" && args.is_empty() {
            if let Some(value) = self.try_compile_clone(object)? {
                return Ok(value);
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
                }
                // SoA layout methods
                if let Some(soa) = self.soa_layouts.get(name.as_str()).cloned() {
                    return self.compile_soa_method(name, &soa, slot, method, args);
                }
                // Vec/String methods (owned or ref)
                if self.vec_elem_types.contains_key(name.as_str()) {
                    let data_ptr = self.get_data_ptr(name).unwrap();
                    return self.compile_vec_method(name, data_ptr, method, args);
                }
                // Slice[T] / mut Slice[T] read-only methods. The slice's
                // stack alloca holds the 2-field `{ptr, i64}` struct (see
                // `slice_struct_type`); GEP field 1 is the length.
                if self.slice_elem_types.contains_key(name.as_str()) {
                    let i64_t = self.context.i64_type();
                    let slice_ty = self.slice_struct_type();
                    match method {
                        "len" => {
                            let len_ptr = self
                                .builder
                                .build_struct_gep(slice_ty, slot.ptr, 1, "slice.len.ptr")
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
                                .build_struct_gep(slice_ty, slot.ptr, 1, "slice.len.ptr")
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
            }
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
                let receiver_arg: BasicMetadataValueEnum<'ctx> = if first_param_is_ptr {
                    if let ExprKind::Identifier(var_name) = &object.kind {
                        if let Some(ptr) = self.get_data_ptr(var_name) {
                            ptr.into()
                        } else {
                            self.compile_expr(object)?.into()
                        }
                    } else {
                        // Non-identifier receiver into a ref-self method:
                        // unsupported in v1 (would require materializing a
                        // temporary alloca). Fall through to compile_expr;
                        // mismatched ABI may surface at link time.
                        self.compile_expr(object)?.into()
                    }
                } else {
                    self.compile_expr(object)?.into()
                };
                let mut compiled_args: Vec<BasicMetadataValueEnum<'ctx>> = vec![receiver_arg];
                for a in args {
                    compiled_args.push(self.compile_expr(&a.value)?.into());
                }
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
                    Ok(basic_val.unwrap_basic())
                };
            }
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

    /// Slice MR helper: lower an indexed-receiver method call
    /// `obj[i].method(args)`. Computes the element pointer through the outer
    /// container's index machinery, synthesizes an identifier name pointing
    /// into the outer storage with the element's type registries populated,
    /// recursively dispatches the method through the existing identifier
    /// path, and cleans up the synth registrations on return.
    ///
    /// Locked design choices (MR1–MR5, see `phase-7-codegen.md`):
    /// - MR1 receiver-shape early dispatch at the top of `compile_method_call`.
    /// - MR2 routes by container shape (Vec/Slice/Array), not method name.
    /// - MR3 read-only and mutating methods both flow through the same path
    ///   — the elem pointer aliases the outer storage so writes propagate.
    /// - MR4 synthesized name `__indexed_elem_<n>` + per-call-site temporary
    ///   registry injection + post-call cleanup.
    /// - MR5 chained `a[i][j].method()` is rejected (single-level only in v1).
    pub(super) fn compile_indexed_receiver_method(
        &mut self,
        inner: &Expr,
        index: &Expr,
        method: &str,
        args: &[CallArg],
        call_span: &crate::token::Span,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        // MR5: reject chained indexed receivers up front. The user must bind
        // the inner element to a temporary first.
        if matches!(inner.kind, ExprKind::Index { .. }) {
            return Err(format!(
                "codegen: chained indexed receivers (`a[i][j].{}(...)`) are deferred to v1.x; \
                 bind the inner element to a temporary first",
                method
            ));
        }

        // Container must be an identifier in v1 — `get_grid()[i].push(x)` is
        // out of scope. The error mirrors the existing fall-through diagnostic.
        let outer_name = if let ExprKind::Identifier(name) = &inner.kind {
            name.clone()
        } else {
            return Err(format!(
                "codegen: indexed-receiver method '{}' requires the indexed container to be a \
                 named variable in v1 (got non-identifier inner expression)",
                method
            ));
        };

        // Determine the element TypeExpr from the outer's recorded element
        // type. Without this we can't populate the synth's side tables, so
        // the recursive dispatch would fall through to the silent-`0` arm.
        let elem_te = self
            .var_elem_type_exprs
            .get(outer_name.as_str())
            .cloned()
            .ok_or_else(|| {
                format!(
                    "codegen: indexed-receiver method '{}' on '{}' — element TypeExpr unknown \
                     (outer is not a tracked Vec/Slice/Array variable)",
                    method, outer_name
                )
            })?;

        // Lower the index access to an element pointer through the outer's
        // container-shape-specific path. Bounds check goes through
        // `emit_panic` on OOB; the OK BB leaves the builder positioned for
        // the post-elem-ptr work.
        let (elem_ptr, elem_ll_ty) = if self.vec_elem_types.contains_key(outer_name.as_str()) {
            self.lower_indexed_elem_ptr_vec(&outer_name, index)?
        } else if self.slice_elem_types.contains_key(outer_name.as_str()) {
            self.lower_indexed_elem_ptr_slice(&outer_name, index)?
        } else {
            // Array shape via slot.ty inspection. v1 supports fixed-size
            // arrays only when the slot's LLVM type is ArrayType.
            let slot = self
                .variables
                .get(outer_name.as_str())
                .copied()
                .ok_or_else(|| {
                    format!(
                        "codegen: indexed-receiver method '{}' — outer '{}' has no slot",
                        method, outer_name
                    )
                })?;
            if let BasicTypeEnum::ArrayType(_) = slot.ty {
                self.lower_indexed_elem_ptr_array(slot, index)?
            } else {
                return Err(format!(
                    "codegen: indexed-receiver method '{}' on '{}' — outer is not a Vec/Slice/Array",
                    method, outer_name
                ));
            }
        };

        // Mint a fresh synth name and register it so the recursive dispatch
        // sees a regular identifier-receiver flow.
        let synth = format!("__indexed_elem_{}", self.indexed_elem_counter);
        self.indexed_elem_counter += 1;
        self.variables.insert(
            synth.clone(),
            VarSlot {
                ptr: elem_ptr,
                ty: elem_ll_ty,
            },
        );
        self.register_var_from_type_expr(&synth, &elem_te);
        // User-struct receiver: also populate `var_type_names` so the
        // impl-block dispatch path resolves `Type.method`.
        if let TypeKind::Path(path) = &elem_te.kind {
            if let Some(seg) = path.segments.first() {
                if self.struct_types.contains_key(seg.as_str()) {
                    self.var_type_names.insert(synth.clone(), seg.clone());
                }
            }
        }

        // Build a fresh Identifier expr at the original call site's span and
        // recursively dispatch. The recursive call will skip this arm
        // (Identifier, not Index) and fall into the regular flow.
        let synth_expr = Expr {
            kind: ExprKind::Identifier(synth.clone()),
            span: inner.span.clone(),
        };
        let result = self.compile_method_call(&synth_expr, method, args, call_span);

        // Clean up synth registrations.  The LLVM IR is already emitted; this
        // is bookkeeping cleanup so subsequent compilations in the same
        // function don't see stale entries.
        self.variables.remove(&synth);
        self.vec_elem_types.remove(&synth);
        self.slice_elem_types.remove(&synth);
        self.var_elem_type_exprs.remove(&synth);
        self.var_type_names.remove(&synth);
        self.map_key_types.remove(&synth);
        self.map_val_types.remove(&synth);
        self.map_key_type_names.remove(&synth);
        self.map_key_type_exprs.remove(&synth);
        self.set_elem_types.remove(&synth);
        self.set_elem_type_names.remove(&synth);
        self.set_elem_type_exprs.remove(&synth);

        result
    }

    /// Slice FR (2026-05-16): field-receiver method dispatch. Sibling to
    /// `compile_indexed_receiver_method` (MR slice) for the
    /// `outer.field.method(...)` shape. The outer must be a named
    /// variable bound to a struct (shared or plain) so we can recover
    /// the struct name from `var_type_names` and the per-field LLVM /
    /// `TypeExpr` info from the declaration registries. Returns
    /// `Ok(None)` when the shape isn't a known struct field — caller
    /// falls through to the regular dispatch.
    ///
    /// Locked design choices (FR1–FR4, sibling to MR1–MR5):
    /// - FR1 receiver-shape early dispatch at the top of
    ///   `compile_method_call`.
    /// - FR2 routes by struct kind (shared via heap-GEP, plain via
    ///   slot-GEP), not by method name.
    /// - FR3 synth identifier `__field_elem_<n>` bound to the field
    ///   pointer with the field's TypeExpr-derived registries
    ///   populated through `register_var_from_type_expr`; both
    ///   read-only and mutating methods flow through the same path
    ///   because the field pointer aliases the parent storage.
    /// - FR4 chained `outer.a.b.method()` is rejected with a clear
    ///   diagnostic — bind the inner field to a temporary first.
    pub(super) fn try_compile_field_receiver_method(
        &mut self,
        inner: &Expr,
        field: &str,
        method: &str,
        args: &[CallArg],
        call_span: &crate::token::Span,
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        // FR4: reject chained field receivers up front.
        if matches!(inner.kind, ExprKind::FieldAccess { .. }) {
            return Err(format!(
                "codegen: chained field receivers (`a.b.c.{}(...)`) are deferred to v1.x; \
                 bind the inner field to a temporary first",
                method
            ));
        }
        // Outer must be a named variable so we can look up its struct
        // type. Anything else (a call return, an index, …) falls through
        // to the regular dispatch; the existing fall-through diagnostic
        // already says the right thing.
        let outer_name = match &inner.kind {
            ExprKind::Identifier(n) => n.clone(),
            _ => return Ok(None),
        };
        let type_name = match self.var_type_names.get(outer_name.as_str()).cloned() {
            Some(t) => t,
            None => return Ok(None),
        };
        // Look up the field's declaration-order index and full TypeExpr.
        let field_idx = match self
            .struct_field_names
            .get(&type_name)
            .and_then(|names| names.iter().position(|n| n == field))
        {
            Some(i) => i,
            None => return Ok(None),
        };
        let field_te = match self
            .struct_field_type_exprs
            .get(&type_name)
            .and_then(|tes| tes.get(field_idx).cloned())
        {
            Some(te) => te,
            None => return Ok(None),
        };

        // GEP the field pointer. Shared: load the handle, GEP at
        // (idx + 1) past the refcount slot. Plain: GEP directly into
        // the slot at idx.
        let (field_ptr, field_ll_ty) =
            if let Some(info) = self.shared_types.get(&type_name).cloned() {
                if info.is_enum {
                    return Ok(None);
                }
                // Load the handle pointer from the outer var slot.
                let slot = self
                    .variables
                    .get(outer_name.as_str())
                    .copied()
                    .ok_or_else(|| {
                        format!(
                            "codegen: field-receiver method '{}' — outer '{}' has no slot",
                            method, outer_name
                        )
                    })?;
                let ptr_ty = self.context.ptr_type(AddressSpace::default());
                let handle = self
                    .builder
                    .build_load(ptr_ty, slot.ptr, "fr.shared.handle")
                    .unwrap()
                    .into_pointer_value();
                let fp = self
                    .builder
                    .build_struct_gep(
                        info.heap_type,
                        handle,
                        (field_idx + 1) as u32,
                        &format!("fr_sh_{}", field),
                    )
                    .unwrap();
                let fty = info
                    .heap_type
                    .get_field_type_at_index((field_idx + 1) as u32)
                    .ok_or_else(|| {
                        format!(
                        "codegen: field-receiver method '{}' on '{}.{}' — field LLVM type missing",
                        method, type_name, field
                    )
                    })?;
                (fp, fty)
            } else if let Some(st) = self.struct_types.get(&type_name).copied() {
                // Plain struct: outer's slot stores the struct by value, so
                // GEP into the slot directly.
                let slot = self
                    .variables
                    .get(outer_name.as_str())
                    .copied()
                    .ok_or_else(|| {
                        format!(
                            "codegen: field-receiver method '{}' — outer '{}' has no slot",
                            method, outer_name
                        )
                    })?;
                let fp = self
                    .builder
                    .build_struct_gep(st, slot.ptr, field_idx as u32, &format!("fr_pl_{}", field))
                    .unwrap();
                let fty = st
                    .get_field_type_at_index(field_idx as u32)
                    .ok_or_else(|| {
                        format!(
                    "codegen: field-receiver method '{}' on '{}.{}' — field LLVM type missing",
                    method, type_name, field
                )
                    })?;
                (fp, fty)
            } else {
                // Not a tracked struct shape — fall through.
                return Ok(None);
            };

        // Mint a fresh synth identifier and populate its registries so
        // the recursive dispatch sees a regular Identifier-receiver flow.
        let synth = format!("__field_elem_{}", self.indexed_elem_counter);
        self.indexed_elem_counter += 1;
        self.variables.insert(
            synth.clone(),
            VarSlot {
                ptr: field_ptr,
                ty: field_ll_ty,
            },
        );
        self.register_var_from_type_expr(&synth, &field_te);
        // User-struct field: also populate `var_type_names` so the
        // impl-block dispatch path resolves `Type.method`.
        if let TypeKind::Path(path) = &field_te.kind {
            if let Some(seg) = path.segments.first() {
                if self.struct_types.contains_key(seg.as_str())
                    || self.shared_types.contains_key(seg.as_str())
                {
                    self.var_type_names.insert(synth.clone(), seg.clone());
                }
            }
        }

        let synth_expr = Expr {
            kind: ExprKind::Identifier(synth.clone()),
            span: inner.span.clone(),
        };
        let result = self.compile_method_call(&synth_expr, method, args, call_span);

        // Clean up synth registrations.
        self.variables.remove(&synth);
        self.vec_elem_types.remove(&synth);
        self.slice_elem_types.remove(&synth);
        self.var_elem_type_exprs.remove(&synth);
        self.var_type_names.remove(&synth);
        self.map_key_types.remove(&synth);
        self.map_val_types.remove(&synth);
        self.map_key_type_names.remove(&synth);
        self.map_key_type_exprs.remove(&synth);
        self.set_elem_types.remove(&synth);
        self.set_elem_type_names.remove(&synth);
        self.set_elem_type_exprs.remove(&synth);

        result.map(Some)
    }

    /// Nested indexed read codegen (`a[i][j]`) — sibling to
    /// `compile_indexed_receiver_method` (MR slice). The outer
    /// container `a` must be a named variable in v1; chained
    /// `a[i][j][k]` rejected with a clear diagnostic. The inner index
    /// lowers to an element pointer via the same per-container
    /// machinery (`lower_indexed_elem_ptr_vec` / `_slice` / `_array`),
    /// a synth identifier is minted with the right side-table
    /// registrations, then `compile_index` is re-invoked with the
    /// synth as the outer object so the existing identifier-keyed
    /// dispatch (`compile_vec_index` / `compile_slice_index` /
    /// generic Array path) handles the second index correctly.
    /// Drive `for x in coll[i].iter()` codegen by synthesizing a
    /// temp identifier for the indexed receiver, registering it in
    /// the appropriate elem-type tables, and recursing into
    /// `compile_for` with the synth as the iterable. Mirrors
    /// `compile_nested_index_read` for the read-only side.
    pub(super) fn compile_for_indexed_iter(
        &mut self,
        label: Option<&str>,
        pattern: &Pattern,
        outer: &Expr,
        idx: &Expr,
        body: &Block,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        if matches!(outer.kind, ExprKind::Index { .. }) {
            return Err(
                "codegen: `for x in a[i][j].iter()` (chained indexed receiver) \
                 is deferred — bind the intermediate element first"
                    .to_string(),
            );
        }
        let outer_name = if let ExprKind::Identifier(name) = &outer.kind {
            name.clone()
        } else {
            return Err(
                "codegen: indexed-receiver `.iter()` requires the outer container \
                 to be a named variable in v1"
                    .to_string(),
            );
        };
        let elem_te = self
            .var_elem_type_exprs
            .get(outer_name.as_str())
            .cloned()
            .ok_or_else(|| {
                format!(
                    "codegen: `for x in {}[i].iter()` — outer element TypeExpr unknown",
                    outer_name
                )
            })?;
        let (elem_ptr, elem_ll_ty) = if self.vec_elem_types.contains_key(outer_name.as_str()) {
            self.lower_indexed_elem_ptr_vec(&outer_name, idx)?
        } else if self.slice_elem_types.contains_key(outer_name.as_str()) {
            self.lower_indexed_elem_ptr_slice(&outer_name, idx)?
        } else {
            let slot = self
                .variables
                .get(outer_name.as_str())
                .copied()
                .ok_or_else(|| {
                    format!(
                        "codegen: `for x in {}[i].iter()` — outer has no slot",
                        outer_name
                    )
                })?;
            if let BasicTypeEnum::ArrayType(_) = slot.ty {
                self.lower_indexed_elem_ptr_array(slot, idx)?
            } else {
                return Err(format!(
                    "codegen: `for x in {}[i].iter()` — outer is not a Vec/Slice/Array",
                    outer_name
                ));
            }
        };
        let synth = format!("__indexed_elem_{}", self.indexed_elem_counter);
        self.indexed_elem_counter += 1;
        self.variables.insert(
            synth.clone(),
            VarSlot {
                ptr: elem_ptr,
                ty: elem_ll_ty,
            },
        );
        self.register_var_from_type_expr(&synth, &elem_te);
        let synth_expr = Expr {
            kind: ExprKind::Identifier(synth.clone()),
            span: outer.span.clone(),
        };
        let result = self.compile_for(label, pattern, &synth_expr, body);
        self.variables.remove(&synth);
        self.vec_elem_types.remove(&synth);
        self.slice_elem_types.remove(&synth);
        self.var_elem_type_exprs.remove(&synth);
        result
    }

    pub(super) fn compile_nested_index_read(
        &mut self,
        inner_object: &Expr,
        inner_idx: &Expr,
        outer_idx: &Expr,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        // MR5 symmetric guard: chained `a[i][j][k]` not supported.
        if matches!(inner_object.kind, ExprKind::Index { .. }) {
            return Err(
                "codegen: chained indexed reads (`a[i][j][k]`) are deferred to v1.x; \
                 bind the intermediate element to a temporary first"
                    .to_string(),
            );
        }
        let outer_name = if let ExprKind::Identifier(name) = &inner_object.kind {
            name.clone()
        } else {
            return Err(
                "codegen: nested indexed read requires the outer container to be a \
                 named variable in v1 (got non-identifier inner expression)"
                    .to_string(),
            );
        };
        // Recover the element TypeExpr — needed to populate the synth
        // identifier's vec_elem_types / slice_elem_types registrations.
        let elem_te = self
            .var_elem_type_exprs
            .get(outer_name.as_str())
            .cloned()
            .ok_or_else(|| {
                format!(
                    "codegen: nested indexed read on '{}' — element TypeExpr unknown \
                     (outer is not a tracked Vec/Slice/Array variable)",
                    outer_name
                )
            })?;
        // Lower the inner `outer[i]` to an element pointer + LLVM type.
        let (elem_ptr, elem_ll_ty) = if self.vec_elem_types.contains_key(outer_name.as_str()) {
            self.lower_indexed_elem_ptr_vec(&outer_name, inner_idx)?
        } else if self.slice_elem_types.contains_key(outer_name.as_str()) {
            self.lower_indexed_elem_ptr_slice(&outer_name, inner_idx)?
        } else {
            let slot = self
                .variables
                .get(outer_name.as_str())
                .copied()
                .ok_or_else(|| {
                    format!(
                        "codegen: nested indexed read — outer '{}' has no slot",
                        outer_name
                    )
                })?;
            if let BasicTypeEnum::ArrayType(_) = slot.ty {
                self.lower_indexed_elem_ptr_array(slot, inner_idx)?
            } else {
                return Err(format!(
                    "codegen: nested indexed read on '{}' — outer is not a Vec/Slice/Array",
                    outer_name
                ));
            }
        };
        // Mint a synth identifier so the recursive call sees the
        // inner element as a regular Vec/Slice/Array variable.
        let synth = format!("__indexed_elem_{}", self.indexed_elem_counter);
        self.indexed_elem_counter += 1;
        self.variables.insert(
            synth.clone(),
            VarSlot {
                ptr: elem_ptr,
                ty: elem_ll_ty,
            },
        );
        self.register_var_from_type_expr(&synth, &elem_te);
        // Rebuild the outer Index expression against the synth
        // identifier and dispatch.
        let synth_expr = Expr {
            kind: ExprKind::Identifier(synth.clone()),
            span: inner_object.span.clone(),
        };
        let result = self.compile_index(&synth_expr, outer_idx);
        // Tear down the per-call synth registrations so subsequent
        // dispatch sites don't see a stale entry.
        self.variables.remove(&synth);
        self.vec_elem_types.remove(&synth);
        self.slice_elem_types.remove(&synth);
        self.var_elem_type_exprs.remove(&synth);
        result
    }

    /// Trailing-method dispatch on an entry-chain receiver. When the call
    /// is
    /// `<m.entry(k){.and_modify(f)}*.{or_insert|or_insert_with}(d)>.method(args)`,
    /// the inner chain produces a slot pointer (`*mut V`, the LLVM
    /// realisation of `mut ref V` per `design.md § Entry[K, V]`).
    /// Mirrors `compile_indexed_receiver_method`: mint a synth identifier
    /// bound to the slot pointer with V's side-tables populated, recurse
    /// into `compile_method_call` with the synth as receiver, tear down on
    /// exit. Closes the LeetCode 3629 kata's canonical
    /// `bucket.entry(p).or_insert(Vec.new()).push(j)` shape.
    ///
    /// Returns `Ok(None)` when the receiver isn't a recognised
    /// or_insert / or_insert_with chain, so the caller falls through to
    /// the regular dispatch (which surfaces its own diagnostic for
    /// unrecognised non-identifier receivers).
    pub(super) fn compile_entry_chain_receiver_method(
        &mut self,
        inner_object: &Expr,
        method: &str,
        args: &[CallArg],
        call_span: &crate::token::Span,
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        // Inner receiver must itself be a method call ending in
        // or_insert / or_insert_with. and_modify-terminal returns the
        // Entry struct, not a slot pointer, so we don't peel that here.
        let ExprKind::MethodCall {
            object: chain_recv,
            method: inner_method,
            args: inner_args,
            ..
        } = &inner_object.kind
        else {
            return Ok(None);
        };
        if !matches!(inner_method.as_str(), "or_insert" | "or_insert_with") {
            return Ok(None);
        }

        // Walk chain_recv (peeling and_modify wrappers) to find the map
        // identifier. Mirrors the loop in `try_compile_entry_chain`.
        let map_name = {
            let mut current: &Expr = chain_recv;
            loop {
                let ExprKind::MethodCall {
                    object: inner_obj,
                    method: m,
                    args: inner_args2,
                    ..
                } = &current.kind
                else {
                    return Ok(None);
                };
                if m == "entry" && inner_args2.len() == 1 {
                    let ExprKind::Identifier(name) = &inner_obj.kind else {
                        return Ok(None);
                    };
                    break name.clone();
                } else if m == "and_modify" && inner_args2.len() == 1 {
                    current = inner_obj;
                } else {
                    return Ok(None);
                }
            }
        };

        // Receiver must be a tracked Map variable; without map_val_types
        // we can't size the synth slot.
        if !self.map_key_types.contains_key(map_name.as_str()) {
            return Ok(None);
        }
        let val_te = self
            .var_elem_type_exprs
            .get(map_name.as_str())
            .cloned()
            .ok_or_else(|| {
                format!(
                    "codegen: entry-chain trailing-method '{}' on map '{}' \
                     — value TypeExpr unknown",
                    method, map_name
                )
            })?;
        let val_ty = *self.map_val_types.get(map_name.as_str()).ok_or_else(|| {
            format!(
                "codegen: entry-chain trailing-method '{}' on map '{}' \
                     — value LLVM type unknown",
                method, map_name
            )
        })?;

        // Compile the inner chain — returns the slot pointer (`*mut V`).
        let slot_value = self
            .try_compile_entry_chain(chain_recv, inner_method, inner_args)?
            .ok_or_else(|| {
                format!(
                    "codegen: entry-chain trailing-method '{}' — inner chain \
                     '{}' unexpectedly didn't compile as an entry chain",
                    method, inner_method
                )
            })?;
        let slot_ptr = slot_value.into_pointer_value();

        // Mint the synth identifier. Same teardown contract as
        // compile_indexed_receiver_method — entries are bookkeeping for
        // the recursive dispatch only; synth owns no allocation.
        let synth = format!("__entry_slot_{}", self.indexed_elem_counter);
        self.indexed_elem_counter += 1;
        self.variables.insert(
            synth.clone(),
            VarSlot {
                ptr: slot_ptr,
                ty: val_ty,
            },
        );
        self.register_var_from_type_expr(&synth, &val_te);
        if let TypeKind::Path(path) = &val_te.kind {
            if let Some(seg) = path.segments.first() {
                if self.struct_types.contains_key(seg.as_str()) {
                    self.var_type_names.insert(synth.clone(), seg.clone());
                }
            }
        }

        let synth_expr = Expr {
            kind: ExprKind::Identifier(synth.clone()),
            span: inner_object.span.clone(),
        };
        let result = self.compile_method_call(&synth_expr, method, args, call_span);

        self.variables.remove(&synth);
        self.vec_elem_types.remove(&synth);
        self.slice_elem_types.remove(&synth);
        self.var_elem_type_exprs.remove(&synth);
        self.var_type_names.remove(&synth);
        self.map_key_types.remove(&synth);
        self.map_val_types.remove(&synth);
        self.map_key_type_names.remove(&synth);
        self.map_key_type_exprs.remove(&synth);
        self.set_elem_types.remove(&synth);
        self.set_elem_type_names.remove(&synth);
        self.set_elem_type_exprs.remove(&synth);

        Ok(Some(result?))
    }

    /// Slice MR: lower `outer[i]` for an outer Vec[T] receiver into an
    /// element pointer + element LLVM type. Bounds-checks against `len`
    /// (not `cap`). Mirrors `compile_vec_index`'s machinery.
    pub(super) fn lower_indexed_elem_ptr_vec(
        &mut self,
        outer_name: &str,
        index: &Expr,
    ) -> Result<(PointerValue<'ctx>, BasicTypeEnum<'ctx>), String> {
        let i64_t = self.context.i64_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let vec_ty = self.vec_struct_type();
        let elem_ty = self.vec_elem_type_for_var(outer_name);
        let vec_ptr = self.get_data_ptr(outer_name).ok_or_else(|| {
            format!(
                "Undefined Vec variable '{}' in indexed-receiver lowering",
                outer_name
            )
        })?;
        let idx_val = self.compile_expr(index)?.into_int_value();

        let len_ptr = self
            .builder
            .build_struct_gep(vec_ty, vec_ptr, 1, "v.mr.len.ptr")
            .unwrap();
        let len = self
            .builder
            .build_load(i64_t, len_ptr, "v.mr.len")
            .unwrap()
            .into_int_value();
        let data_pp = self
            .builder
            .build_struct_gep(vec_ty, vec_ptr, 0, "v.mr.data.ptr")
            .unwrap();
        let data = self
            .builder
            .build_load(ptr_ty, data_pp, "v.mr.data")
            .unwrap()
            .into_pointer_value();

        let fn_val = self.current_fn.unwrap();
        let oob_bb = self.context.append_basic_block(fn_val, "v.mr.oob");
        let ok_bb = self.context.append_basic_block(fn_val, "v.mr.ok");
        let cmp = self
            .builder
            .build_int_compare(IntPredicate::UGE, idx_val, len, "bounds")
            .unwrap();
        self.builder
            .build_conditional_branch(cmp, oob_bb, ok_bb)
            .unwrap();
        self.builder.position_at_end(oob_bb);
        self.emit_panic("vec index out of bounds");
        self.builder.build_unreachable().unwrap();

        self.builder.position_at_end(ok_bb);
        let elem_ptr = unsafe {
            self.builder
                .build_gep(elem_ty, data, &[idx_val], "v.mr.elem.ptr")
                .unwrap()
        };
        Ok((elem_ptr, elem_ty))
    }

    /// Slice MR: lower `outer[i]` for an outer Slice[T] receiver.
    pub(super) fn lower_indexed_elem_ptr_slice(
        &mut self,
        outer_name: &str,
        index: &Expr,
    ) -> Result<(PointerValue<'ctx>, BasicTypeEnum<'ctx>), String> {
        let i64_t = self.context.i64_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let slice_ty = self.slice_struct_type();
        let elem_ty = *self.slice_elem_types.get(outer_name).ok_or_else(|| {
            format!(
                "Undefined Slice variable '{}' in indexed-receiver lowering",
                outer_name
            )
        })?;
        let slice_ptr = self.get_data_ptr(outer_name).ok_or_else(|| {
            format!(
                "Undefined Slice variable '{}' in indexed-receiver lowering",
                outer_name
            )
        })?;
        let idx_val = self.compile_expr(index)?.into_int_value();

        let data_pp = self
            .builder
            .build_struct_gep(slice_ty, slice_ptr, 0, "s.mr.data.pp")
            .unwrap();
        let len_p = self
            .builder
            .build_struct_gep(slice_ty, slice_ptr, 1, "s.mr.len.p")
            .unwrap();
        let data = self
            .builder
            .build_load(ptr_ty, data_pp, "s.mr.data")
            .unwrap()
            .into_pointer_value();
        let len = self
            .builder
            .build_load(i64_t, len_p, "s.mr.len")
            .unwrap()
            .into_int_value();

        let fn_val = self.current_fn.unwrap();
        let oob_bb = self.context.append_basic_block(fn_val, "s.mr.oob");
        let ok_bb = self.context.append_basic_block(fn_val, "s.mr.ok");
        let cmp = self
            .builder
            .build_int_compare(IntPredicate::UGE, idx_val, len, "bounds")
            .unwrap();
        self.builder
            .build_conditional_branch(cmp, oob_bb, ok_bb)
            .unwrap();
        self.builder.position_at_end(oob_bb);
        self.emit_panic("slice index out of bounds");
        self.builder.build_unreachable().unwrap();

        self.builder.position_at_end(ok_bb);
        let elem_ptr = unsafe {
            self.builder
                .build_gep(elem_ty, data, &[idx_val], "s.mr.elem.ptr")
                .unwrap()
        };
        Ok((elem_ptr, elem_ty))
    }

    /// Slice MR: lower `outer[i]` for a fixed-size Array[T, N] receiver.
    pub(super) fn lower_indexed_elem_ptr_array(
        &mut self,
        slot: VarSlot<'ctx>,
        index: &Expr,
    ) -> Result<(PointerValue<'ctx>, BasicTypeEnum<'ctx>), String> {
        let i64_t = self.context.i64_type();
        let arr_ty = match slot.ty {
            BasicTypeEnum::ArrayType(at) => at,
            _ => return Err("Array shape required for Array indexed-receiver lowering".to_string()),
        };
        let elem_ty = arr_ty.get_element_type();
        let idx_val = self.compile_expr(index)?.into_int_value();
        let len = i64_t.const_int(arr_ty.len() as u64, false);

        let fn_val = self.current_fn.unwrap();
        let oob_bb = self.context.append_basic_block(fn_val, "a.mr.oob");
        let ok_bb = self.context.append_basic_block(fn_val, "a.mr.ok");
        let cmp = self
            .builder
            .build_int_compare(IntPredicate::UGE, idx_val, len, "bounds")
            .unwrap();
        self.builder
            .build_conditional_branch(cmp, oob_bb, ok_bb)
            .unwrap();
        self.builder.position_at_end(oob_bb);
        self.emit_panic("array index out of bounds");
        self.builder.build_unreachable().unwrap();

        self.builder.position_at_end(ok_bb);
        let zero = i64_t.const_int(0, false);
        let elem_ptr = unsafe {
            self.builder
                .build_gep(arr_ty, slot.ptr, &[zero, idx_val], "a.mr.elem.ptr")
                .unwrap()
        };
        Ok((elem_ptr, elem_ty))
    }

    /// Infer the declared struct/enum type name of a method-call receiver,
    /// or `None` if we can't — in which case the caller falls back to its
    /// built-in/primitive handling. Keys off `var_type_names`, which the
    /// existing struct-literal and struct-param paths populate.
    pub(super) fn inferred_receiver_type(&self, object: &Expr) -> Option<String> {
        if let ExprKind::Identifier(name) = &object.kind {
            return self.var_type_names.get(name.as_str()).cloned();
        }
        None
    }
}
