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
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum};
use inkwell::AddressSpace;

impl<'ctx> super::Codegen<'ctx> {
    pub(super) fn compile_assoc_call(
        &mut self,
        type_name: &str,
        method: &str,
        _args: &[CallArg],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let args = _args;
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
                    return self.compile_binop_typed(&op, lhs, rhs, is_unsigned);
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
        if type_name == "Vec" && method == "with_capacity" {
            if args.len() != 1 {
                return Err(format!(
                    "Vec.with_capacity expects 1 argument (capacity), got {}",
                    args.len()
                ));
            }
            let elem_ty = self.pending_let_elem_type.ok_or_else(|| {
                "Vec.with_capacity: element type unknown — requires a `let v: Vec[T] = ...` annotation"
                    .to_string()
            })?;
            let n = self.compile_expr(&args[0].value)?.into_int_value();
            let elem_size = elem_ty.size_of().unwrap();
            let alloc_bytes = self
                .builder
                .build_int_mul(n, elem_size, "with_cap.alloc_bytes")
                .unwrap();
            let buf = self
                .builder
                .build_call(self.malloc_fn, &[alloc_bytes.into()], "with_cap.buf")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_pointer_value();

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

            // Element type recovery — bare Identifier path first, then
            // nested-Index path for Vec[Vec[T]] sources. Returns
            // (LLVM elem type, optional elem TypeExpr for the
            // RC-clone path below, label for diagnostics).
            let (elem_ty, src_elem_te, src_label): (BasicTypeEnum<'ctx>, Option<TypeExpr>, String) =
                match &arg.kind {
                    ExprKind::Identifier(src_name) => {
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
            // Index path compiles the expression directly to get the
            // inner Vec aggregate value and extracts its first two
            // fields (same fallback shape as `extend_from_slice`).
            let (src_data, src_len) = if matches!(arg.kind, ExprKind::Identifier(_)) {
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

            // Branch on element triviality: memcpy for primitives,
            // per-element synth_clone for anything carrying a heap
            // pointer (String, Vec, Map, Set, shared T, tuples /
            // structs that recursively contain those). Without the
            // clone path, `Vec.from_slice` on a `Vec[String]` /
            // `Vec[Vec[T]]` source bit-copies the aggregate values
            // and both src and dst alias the same inner heap
            // pointers — first scope-exit free wins, second
            // double-frees (ASAN-flagged in `tests/memory_sanitizer
            // .rs :: asan_vec_from_slice_string_elements_independent`).
            let elem_te = src_elem_te;
            let trivial = elem_te
                .as_ref()
                .map(super::vec_method::is_trivially_copyable_te)
                .unwrap_or(true);
            if trivial {
                self.builder
                    .build_memcpy(new_buf, 8, src_data, 8, alloc_bytes)
                    .unwrap();
            } else {
                let elem_te = elem_te.unwrap();
                let clone_fn = self.emit_clone_fn_for_type_expr(&elem_te);
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
}
