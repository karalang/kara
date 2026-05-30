//! HTTP handler ABI shim and Request method codegen.
//!
//! Houses the methods that bridge Kāra `Server.serve(handler)` to the
//! FFI extern `void (*)(*const KaracHttpRequest, *mut
//! KaracHttpResponse)` slot: `resolve_free_fn_for_handler_arg`
//! (validates and dereferences the user fn pointer),
//! `emit_http_handler_shim` (synthesizes the per-handler extern "C"
//! shim function), `compile_request_string_method` (lowers
//! `Request.path()` / `Request.method()` to the runtime externs),
//! `compile_request_body` (lowers `Request.body()` through the raw-
//! byte pair `karac_runtime_http_request_body_ptr` / `_body_len`),
//! and `compile_request_header` (lowers `Request.header(name)` through
//! `karac_runtime_http_request_header` and wraps the result in
//! `Option[String]`).

use crate::ast::*;

use inkwell::module::Linkage;
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum, PointerValue};
use inkwell::AddressSpace;

/// Which `Request` full-map accessor `compile_request_pairs` should
/// drive — the two share an identical loop shape (count + indexed
/// key/val accessors) and differ only in the extern names.
#[derive(Clone, Copy)]
pub(super) enum RequestPairsKind {
    Headers,
    Query,
}

impl<'ctx> super::Codegen<'ctx> {
    // ── Helpers ─────────────────────────────────────────────────

    /// Slice B follow-up (2026-05-09) — sub-steps (b)+(d).
    ///
    /// Resolve a `Server.serve(handler)` argument expression to the
    /// LLVM `FunctionValue` of a free fn, or emit a structured
    /// rejection diagnostic when the argument shape isn't a free-fn-
    /// name reference. Closures-with-captures, indirect-call values,
    /// and other identifier-as-value shapes that don't resolve to a
    /// `module.get_function(name)` hit get the same rejection — the
    /// `extern "C" fn(*const KaracHttpRequest, *mut KaracHttpResponse)`
    /// FFI slot only accepts a bare fn pointer (the closure-pair
    /// `{ fn_ptr, env_ptr }` ABI is incompatible at the indirect-call
    /// boundary), so the v1 surface is "free fn or rejection."
    ///
    /// **Sub-step (d) framing.** The diagnostic carries the
    /// `E_CLOSURE_AS_FN_PTR_NOT_YET` code so user-side tooling
    /// (`karac build --json`) can recognize it; the code is emitted
    /// inside the codegen error string rather than registered as a
    /// separate enum variant in `cli.rs` because all codegen errors
    /// flow through the single `error: codegen failed: {e}` path
    /// (see `src/cli.rs:2374`).
    pub(super) fn resolve_free_fn_for_handler_arg(
        &self,
        arg: &Expr,
    ) -> Result<inkwell::values::FunctionValue<'ctx>, String> {
        match &arg.kind {
            ExprKind::Identifier(name) => {
                // Resolution order mirrors `compile_expr`'s Identifier
                // arm: a local binding shadows; otherwise look up as a
                // free fn registered in the LLVM module. We refuse to
                // accept a local binding even if it would resolve —
                // that path is for closure-fat-pointer values which
                // don't match the FFI slot.
                if self.variables.contains_key(name.as_str()) {
                    return Err(format!(
                        "error[E_CLOSURE_AS_FN_PTR_NOT_YET]: cannot pass local binding `{name}` \
                         as the handler argument to `Server.serve` — only free fn names are \
                         supported in v1. Closures with captures (and other indirect-call \
                         values) cannot match the `extern \"C\" fn(*const Request, *mut \
                         Response)` ABI at the FFI boundary; pass a free fn instead. The \
                         closure-as-`Fn`-arg ABI fix is a separate codegen track."
                    ));
                }
                if let Some(fv) = self.module.get_function(name) {
                    return Ok(fv);
                }
                Err(format!(
                    "error[E_CLOSURE_AS_FN_PTR_NOT_YET]: cannot resolve `{name}` to a free fn \
                     for the handler argument to `Server.serve`. Only free fn names are \
                     supported in v1; closures-with-captures and other identifier shapes \
                     are rejected. Pass a top-level `fn` declaration instead."
                ))
            }
            ExprKind::Closure { .. } => Err(
                "error[E_CLOSURE_AS_FN_PTR_NOT_YET]: closures with captures cannot be \
                 passed where a fn-pointer is expected. The handler argument to \
                 `Server.serve` must be a free fn name (e.g. `Server.serve(addr, handle)`); \
                 the closure-pair `{ fn_ptr, env_ptr }` ABI does not match the FFI \
                 extern's bare-pointer parameter slot. Closure-as-`Fn`-arg is a \
                 separate codegen track."
                    .to_string(),
            ),
            _ => Err(format!(
                "error[E_CLOSURE_AS_FN_PTR_NOT_YET]: handler argument to `Server.serve` \
                 must be a free fn name; got expression shape `{:?}` which is not \
                 supported in v1.",
                std::mem::discriminant(&arg.kind)
            )),
        }
    }

    /// HTTP handler ABI trampoline (2026-05-09).
    ///
    /// Emit (or look up from `http_shim_cache`) a per-handler-fn `extern "C"`
    /// shim that adapts between hyper's FFI signature
    /// (`*const KaracHttpRequest, *mut KaracHttpResponse`) and the user's
    /// value-typed `fn(Request) -> Response`. The shim:
    ///   1. Forwards the request pointer arg as the user fn's `Request`
    ///      param (Request lowers to `ptr` per F2 — opaque-pointer shape
    ///      mirroring `Map[K, V]`).
    ///   2. Calls the user handler.
    ///   3. Extracts `status` from the returned `Response` aggregate, truncates
    ///      to u16, and writes it to the response slot via
    ///      `karac_runtime_http_response_set_status`.
    ///   4. Extracts the `body` String's `(data_ptr, len)` and copies it
    ///      into the response slot via
    ///      `karac_runtime_http_response_set_body`.
    ///   5. Returns void.
    ///
    /// Per-handler caching keeps the IR stable and avoids redundant emission
    /// when one program calls `Server.serve(handle)` multiple times.
    /// Pinned by `tests/codegen.rs::test_server_serve_handler_shim_caches`.
    ///
    /// **Panic semantics (F1).** The shim does nothing special — Kāra's
    /// `emit_panic` is `printf + exit(1)`, so handler panics terminate the
    /// server process. Recovery requires `std.panic` (separate Phase 8 work).
    pub(super) fn emit_http_handler_shim(
        &mut self,
        handler_fn: inkwell::values::FunctionValue<'ctx>,
    ) -> inkwell::values::FunctionValue<'ctx> {
        let user_name = handler_fn
            .get_name()
            .to_str()
            .map(|s| s.to_string())
            .unwrap_or_else(|_| "handler".to_string());
        if let Some(&cached) = self.http_shim_cache.get(&user_name) {
            return cached;
        }
        let shim_name = format!("_karac_http_shim_{user_name}");
        if let Some(existing) = self.module.get_function(&shim_name) {
            self.http_shim_cache.insert(user_name, existing);
            return existing;
        }

        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let void_ty = self.context.void_type();
        let i16_ty = self.context.i16_type();
        let i64_ty = self.context.i64_type();
        let shim_ty = void_ty.fn_type(&[ptr_ty.into(), ptr_ty.into()], false);
        let shim = self
            .module
            .add_function(&shim_name, shim_ty, Some(Linkage::External));

        // Save the builder's current cursor; we'll restore after shim emit
        // so the caller (`compile_assoc_call` for `Server.serve`) can keep
        // building the dispatch site's basic block.
        let saved_block = self.builder.get_insert_block();
        let saved_fn = self.current_fn;
        self.current_fn = Some(shim);

        let entry = self.context.append_basic_block(shim, "entry");
        self.builder.position_at_end(entry);

        let req_ptr = shim.get_nth_param(0).unwrap().into_pointer_value();
        let resp_ptr = shim.get_nth_param(1).unwrap().into_pointer_value();

        // Call the user handler. The user fn's signature is `fn(Request) ->
        // Response`; with F2's opaque-ptr Request, the Kāra ABI takes a
        // single `ptr` arg and returns the Response aggregate by value.
        let call = self
            .builder
            .build_call(handler_fn, &[req_ptr.into()], "shim.user.call")
            .unwrap();
        let resp_val = call.try_as_basic_value().unwrap_basic();
        let resp_struct = resp_val.into_struct_value();

        // Response layout: { i64 status, { ptr data, i64 len, i64 cap } body }.
        // Extract status (i64), truncate to i16 (the runtime extern takes u16
        // — the i16/u16 distinction is sign-vs-unsigned only at the source
        // level; the LLVM bit pattern is the same).
        let status_i64 = self
            .builder
            .build_extract_value(resp_struct, 0, "shim.resp.status.i64")
            .unwrap()
            .into_int_value();
        let status_i16 = self
            .builder
            .build_int_truncate(status_i64, i16_ty, "shim.resp.status.i16")
            .unwrap();
        let set_status_fn = self
            .module
            .get_function("karac_runtime_http_response_set_status")
            .expect("karac_runtime_http_response_set_status declared in Codegen::new");
        self.builder
            .build_call(
                set_status_fn,
                &[resp_ptr.into(), status_i16.into()],
                "shim.set_status",
            )
            .unwrap();

        // Extract the body String aggregate, then its data pointer + length.
        let body_struct = self
            .builder
            .build_extract_value(resp_struct, 1, "shim.resp.body")
            .unwrap()
            .into_struct_value();
        let body_data = self
            .builder
            .build_extract_value(body_struct, 0, "shim.resp.body.data")
            .unwrap()
            .into_pointer_value();
        let body_len = self
            .builder
            .build_extract_value(body_struct, 1, "shim.resp.body.len")
            .unwrap()
            .into_int_value();
        // Sign-extend / pass-through to i64 for the runtime call (Kāra's
        // String len is already i64, so this is a no-op for the typical
        // path — the explicit extension keeps us robust if a future
        // String layout uses a narrower len field).
        let body_len_i64 = self
            .builder
            .build_int_z_extend_or_bit_cast(body_len, i64_ty, "shim.resp.body.len.i64")
            .unwrap();
        let set_body_fn = self
            .module
            .get_function("karac_runtime_http_response_set_body")
            .expect("karac_runtime_http_response_set_body declared in Codegen::new");
        self.builder
            .build_call(
                set_body_fn,
                &[resp_ptr.into(), body_data.into(), body_len_i64.into()],
                "shim.set_body",
            )
            .unwrap();

        // Phase-8 line 14 — response-header round-trip. If the user's
        // `Response` struct carries a third field (the convention
        // is `headers: Vec[(String, String)]`), iterate it and emit
        // one `karac_runtime_http_response_set_header` call per pair.
        // 2-field Response (`{ status, body }`) — the existing
        // backward-compatible shape — skips the loop entirely. No
        // field-NAME introspection is required at v1: the convention
        // is positional (field 2) and the codegen check is purely
        // structural.
        if resp_struct.get_type().count_fields() >= 3 {
            let str_ty = self.vec_struct_type();
            let tuple_ty = self
                .context
                .struct_type(&[str_ty.into(), str_ty.into()], false);

            let headers_vec = self
                .builder
                .build_extract_value(resp_struct, 2, "shim.resp.headers")
                .unwrap()
                .into_struct_value();
            let headers_data = self
                .builder
                .build_extract_value(headers_vec, 0, "shim.resp.headers.data")
                .unwrap()
                .into_pointer_value();
            let headers_len = self
                .builder
                .build_extract_value(headers_vec, 1, "shim.resp.headers.len")
                .unwrap()
                .into_int_value();

            let cond_bb = self.context.append_basic_block(shim, "shim.hdrs.cond");
            let body_bb = self.context.append_basic_block(shim, "shim.hdrs.body");
            let done_bb = self.context.append_basic_block(shim, "shim.hdrs.done");
            let i_alloca = self.create_entry_alloca(shim, "shim.hdrs.i", i64_ty.into());
            self.builder
                .build_store(i_alloca, i64_ty.const_zero())
                .unwrap();
            self.builder.build_unconditional_branch(cond_bb).unwrap();

            self.builder.position_at_end(cond_bb);
            let i_cur = self
                .builder
                .build_load(i64_ty, i_alloca, "shim.hdrs.i.cur")
                .unwrap()
                .into_int_value();
            let lt = self
                .builder
                .build_int_compare(
                    inkwell::IntPredicate::SLT,
                    i_cur,
                    headers_len,
                    "shim.hdrs.lt",
                )
                .unwrap();
            self.builder
                .build_conditional_branch(lt, body_bb, done_bb)
                .unwrap();

            self.builder.position_at_end(body_bb);
            // GEP into the Vec's element buffer at index i_cur — stride
            // is the tuple type's size_of (LLVM computes this from the
            // element type).
            let elem_ptr = unsafe {
                self.builder
                    .build_gep(tuple_ty, headers_data, &[i_cur], "shim.hdrs.elem.ptr")
                    .unwrap()
            };
            // Load the tuple by value, then extract the two String
            // halves and their `(data, len)` slices.
            let elem_val = self
                .builder
                .build_load(tuple_ty, elem_ptr, "shim.hdrs.elem")
                .unwrap()
                .into_struct_value();
            let key_str = self
                .builder
                .build_extract_value(elem_val, 0, "shim.hdrs.key")
                .unwrap()
                .into_struct_value();
            let val_str = self
                .builder
                .build_extract_value(elem_val, 1, "shim.hdrs.val")
                .unwrap()
                .into_struct_value();
            let key_data = self
                .builder
                .build_extract_value(key_str, 0, "shim.hdrs.key.data")
                .unwrap()
                .into_pointer_value();
            let key_len = self
                .builder
                .build_extract_value(key_str, 1, "shim.hdrs.key.len")
                .unwrap()
                .into_int_value();
            let val_data = self
                .builder
                .build_extract_value(val_str, 0, "shim.hdrs.val.data")
                .unwrap()
                .into_pointer_value();
            let val_len = self
                .builder
                .build_extract_value(val_str, 1, "shim.hdrs.val.len")
                .unwrap()
                .into_int_value();
            let set_header_fn = self
                .module
                .get_function("karac_runtime_http_response_set_header")
                .expect("karac_runtime_http_response_set_header declared in Codegen::new");
            self.builder
                .build_call(
                    set_header_fn,
                    &[
                        resp_ptr.into(),
                        key_data.into(),
                        key_len.into(),
                        val_data.into(),
                        val_len.into(),
                    ],
                    "shim.set_header",
                )
                .unwrap();

            let one = i64_ty.const_int(1, false);
            let i_next = self
                .builder
                .build_int_add(i_cur, one, "shim.hdrs.i.next")
                .unwrap();
            self.builder.build_store(i_alloca, i_next).unwrap();
            self.builder.build_unconditional_branch(cond_bb).unwrap();

            self.builder.position_at_end(done_bb);
        }

        self.builder.build_return(None).unwrap();

        // Restore cursor.
        self.current_fn = saved_fn;
        if let Some(bb) = saved_block {
            self.builder.position_at_end(bb);
        }

        self.http_shim_cache.insert(user_name, shim);
        shim
    }

    /// HTTP handler ABI trampoline (2026-05-09).
    ///
    /// Compile `req.path()` / `req.method()` for a `Request`-typed local.
    /// The receiver's slot stores the opaque `*const KaracHttpRequest` (F2);
    /// load it, call the matching runtime extern to get a borrowed
    /// `*const c_char`, then copy the bytes into a fresh Kāra `String`
    /// `{ data, len, cap }` so the resulting value owns its buffer
    /// (the runtime drops the request struct after the handler returns,
    /// invalidating the borrowed pointer).
    ///
    /// Pinned by `tests/interpreter.rs::test_server_serve_handler_request_path_returns_owned_string`
    /// (interpreter parity for the owned-String contract) and
    /// `tests/http_server.rs::test_server_serve_handler_reads_path` /
    /// `_reads_method` (end-to-end runtime exercise).
    pub(super) fn compile_request_string_method(
        &mut self,
        var_name: &str,
        method: &str,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let extern_name = match method {
            "path" => "karac_runtime_http_request_path",
            "method" => "karac_runtime_http_request_method",
            other => {
                return Err(format!(
                    "compile_request_string_method called with unsupported method '{other}'"
                ));
            }
        };
        let slot = self
            .variables
            .get(var_name)
            .copied()
            .ok_or_else(|| format!("Request var '{var_name}' not bound"))?;
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_ty = self.context.i64_type();

        // Load the request pointer from the local's alloca.
        let req_ptr = self
            .builder
            .build_load(slot.ty, slot.ptr, &format!("{var_name}.req.load"))
            .unwrap()
            .into_pointer_value();

        let extern_fn = self
            .module
            .get_function(extern_name)
            .unwrap_or_else(|| panic!("{extern_name} declared in Codegen::new"));
        let cstr_ptr = self
            .builder
            .build_call(extern_fn, &[req_ptr.into()], &format!("req.{method}.cstr"))
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();

        // strlen(cstr_ptr) → i64.
        let strlen_fn = self
            .module
            .get_function("strlen")
            .expect("strlen declared in Codegen::new");
        let len_val = self
            .builder
            .build_call(strlen_fn, &[cstr_ptr.into()], &format!("req.{method}.len"))
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();
        // strlen returns size_t (i64 on 64-bit); ensure i64.
        let len_i64 = self
            .builder
            .build_int_z_extend_or_bit_cast(len_val, i64_ty, &format!("req.{method}.len.i64"))
            .unwrap();

        // Allocate len bytes (handle len==0 by passing 0 — malloc(0) is
        // implementation-defined but Vec/String elsewhere uses null for
        // empty buffers; mirror that here for consistency).
        let zero = i64_ty.const_zero();
        let is_zero = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::EQ,
                len_i64,
                zero,
                &format!("req.{method}.is_empty"),
            )
            .unwrap();
        let fn_val = self
            .current_fn
            .ok_or_else(|| "Request method called outside fn".to_string())?;
        let alloc_bb = self
            .context
            .append_basic_block(fn_val, &format!("req.{method}.alloc"));
        let empty_bb = self
            .context
            .append_basic_block(fn_val, &format!("req.{method}.empty"));
        let cont_bb = self
            .context
            .append_basic_block(fn_val, &format!("req.{method}.cont"));

        // Pre-branch alloca for the resulting (data, len, cap) buffer ptr.
        let buf_slot = self.create_entry_alloca(fn_val, "req.str.buf", ptr_ty.into());

        self.builder
            .build_conditional_branch(is_zero, empty_bb, alloc_bb)
            .unwrap();

        // Empty path: store null buffer.
        self.builder.position_at_end(empty_bb);
        self.builder
            .build_store(buf_slot, ptr_ty.const_null())
            .unwrap();
        self.builder.build_unconditional_branch(cont_bb).unwrap();

        // Non-empty: malloc + memcpy.
        self.builder.position_at_end(alloc_bb);
        let buf = self
            .builder
            .build_call(
                self.malloc_fn,
                &[len_i64.into()],
                &format!("req.{method}.buf"),
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        self.builder
            .build_memcpy(buf, 1, cstr_ptr, 1, len_i64)
            .unwrap();
        self.builder.build_store(buf_slot, buf).unwrap();
        self.builder.build_unconditional_branch(cont_bb).unwrap();

        // Cont: assemble the String aggregate.
        self.builder.position_at_end(cont_bb);
        let data = self
            .builder
            .build_load(ptr_ty, buf_slot, "req.str.data")
            .unwrap()
            .into_pointer_value();
        let str_ty = self.vec_struct_type();
        let mut str_val: BasicValueEnum<'ctx> = str_ty.get_undef().into();
        str_val = self
            .builder
            .build_insert_value(str_val.into_struct_value(), data, 0, "req.str.data.ins")
            .unwrap()
            .into_struct_value()
            .into();
        str_val = self
            .builder
            .build_insert_value(str_val.into_struct_value(), len_i64, 1, "req.str.len.ins")
            .unwrap()
            .into_struct_value()
            .into();
        str_val = self
            .builder
            .build_insert_value(str_val.into_struct_value(), len_i64, 2, "req.str.cap.ins")
            .unwrap()
            .into_struct_value()
            .into();
        Ok(str_val)
    }

    /// Compile `req.header(name)` for a `Request`-typed local. Returns
    /// `Option[String]`: `Some(value)` if the header is present
    /// (case-insensitive lookup), `None` otherwise. Pairs the runtime
    /// extern `karac_runtime_http_request_header(req, name_data,
    /// name_len) -> *const c_char` (null on miss; runtime-owned cstring
    /// on hit) with the same strlen + malloc + memcpy String-build
    /// path as `compile_request_string_method`. The found-end basic
    /// block hands off three payload words via
    /// `build_option_some_via_phis`, which merges them with a None
    /// branch into the final `Option[String]` aggregate.
    pub(super) fn compile_request_header(
        &mut self,
        var_name: &str,
        name_arg: &Expr,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let slot = self
            .variables
            .get(var_name)
            .copied()
            .ok_or_else(|| format!("Request var '{var_name}' not bound"))?;
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_ty = self.context.i64_type();

        // Compile the name arg first — outside any of the new BBs — so the
        // String aggregate's construction happens at the call site rather
        // than inside one of the option-merge branches.
        let name_val = self.compile_expr(name_arg)?;
        let name_struct = name_val.into_struct_value();
        let name_data = self
            .builder
            .build_extract_value(name_struct, 0, "req.hdr.name.data")
            .unwrap()
            .into_pointer_value();
        let name_len = self
            .builder
            .build_extract_value(name_struct, 1, "req.hdr.name.len")
            .unwrap()
            .into_int_value();

        // Load the request pointer from the local's alloca.
        let req_ptr = self
            .builder
            .build_load(slot.ty, slot.ptr, &format!("{var_name}.req.hdr.load"))
            .unwrap()
            .into_pointer_value();

        // Call the runtime extern; null return = header not present.
        let extern_fn = self
            .module
            .get_function("karac_runtime_http_request_header")
            .expect("karac_runtime_http_request_header declared in Codegen::new");
        let cstr_ptr = self
            .builder
            .build_call(
                extern_fn,
                &[req_ptr.into(), name_data.into(), name_len.into()],
                "req.hdr.cstr",
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();

        let fn_val = self
            .current_fn
            .ok_or_else(|| "Request.header called outside fn".to_string())?;
        let found_bb = self.context.append_basic_block(fn_val, "req.hdr.found");
        let notfound_bb = self.context.append_basic_block(fn_val, "req.hdr.notfound");
        let merge_bb = self.context.append_basic_block(fn_val, "req.hdr.merge");

        let is_null = self
            .builder
            .build_is_null(cstr_ptr, "req.hdr.is_null")
            .unwrap();
        self.builder
            .build_conditional_branch(is_null, notfound_bb, found_bb)
            .unwrap();

        // Found path: strlen + malloc + memcpy into a fresh String
        // aggregate, then split into three payload words for the PHI
        // merge. Mirrors the tail of `compile_request_string_method` —
        // including the `len == 0` empty-path branch — so an explicitly-
        // empty header value (e.g. `X-Trace-Id:`) materializes a
        // `Some("")` whose data ptr is null (matching how other empty
        // Kāra Strings are represented elsewhere in codegen).
        self.builder.position_at_end(found_bb);
        let strlen_fn = self
            .module
            .get_function("strlen")
            .expect("strlen declared in Codegen::new");
        let val_len_raw = self
            .builder
            .build_call(strlen_fn, &[cstr_ptr.into()], "req.hdr.val.len")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();
        let val_len_i64 = self
            .builder
            .build_int_z_extend_or_bit_cast(val_len_raw, i64_ty, "req.hdr.val.len.i64")
            .unwrap();

        let zero = i64_ty.const_zero();
        let is_empty = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::EQ,
                val_len_i64,
                zero,
                "req.hdr.val.is_empty",
            )
            .unwrap();
        let alloc_bb = self.context.append_basic_block(fn_val, "req.hdr.val.alloc");
        let empty_bb = self.context.append_basic_block(fn_val, "req.hdr.val.empty");
        let found_end_bb = self.context.append_basic_block(fn_val, "req.hdr.found.end");
        let buf_slot = self.create_entry_alloca(fn_val, "req.hdr.val.buf", ptr_ty.into());

        self.builder
            .build_conditional_branch(is_empty, empty_bb, alloc_bb)
            .unwrap();

        // Empty: null buffer.
        self.builder.position_at_end(empty_bb);
        self.builder
            .build_store(buf_slot, ptr_ty.const_null())
            .unwrap();
        self.builder
            .build_unconditional_branch(found_end_bb)
            .unwrap();

        // Non-empty: malloc + memcpy.
        self.builder.position_at_end(alloc_bb);
        let buf = self
            .builder
            .build_call(
                self.malloc_fn,
                &[val_len_i64.into()],
                "req.hdr.val.buf.alloc",
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        self.builder
            .build_memcpy(buf, 1, cstr_ptr, 1, val_len_i64)
            .unwrap();
        self.builder.build_store(buf_slot, buf).unwrap();
        self.builder
            .build_unconditional_branch(found_end_bb)
            .unwrap();

        // Assemble the String aggregate in `found_end_bb` and split it
        // into three i64 payload words for the option-merge.
        self.builder.position_at_end(found_end_bb);
        let data = self
            .builder
            .build_load(ptr_ty, buf_slot, "req.hdr.val.data")
            .unwrap()
            .into_pointer_value();
        let str_ty = self.vec_struct_type();
        let mut str_val: BasicValueEnum<'ctx> = str_ty.get_undef().into();
        str_val = self
            .builder
            .build_insert_value(str_val.into_struct_value(), data, 0, "req.hdr.str.data.ins")
            .unwrap()
            .into_struct_value()
            .into();
        str_val = self
            .builder
            .build_insert_value(
                str_val.into_struct_value(),
                val_len_i64,
                1,
                "req.hdr.str.len.ins",
            )
            .unwrap()
            .into_struct_value()
            .into();
        str_val = self
            .builder
            .build_insert_value(
                str_val.into_struct_value(),
                val_len_i64,
                2,
                "req.hdr.str.cap.ins",
            )
            .unwrap()
            .into_struct_value()
            .into();
        let some_payload_words = self.coerce_to_payload_words(str_val, 3)?;
        let some_end_bb = self.builder.get_insert_block().unwrap();
        self.builder.build_unconditional_branch(merge_bb).unwrap();

        // Not found: just branch to merge.
        self.builder.position_at_end(notfound_bb);
        self.builder.build_unconditional_branch(merge_bb).unwrap();

        // Merge — PHI-assemble `Option[String]`.
        self.builder.position_at_end(merge_bb);
        let agg = self.build_option_some_via_phis(
            &some_payload_words,
            some_end_bb,
            notfound_bb,
            "req.hdr.opt",
        );
        Ok(agg)
    }

    /// Compile `req.body()` for a `Request`-typed local. The body is not
    /// null-terminated — pair `karac_runtime_http_request_body_ptr` with
    /// `karac_runtime_http_request_body_len`, malloc + memcpy the bytes
    /// into a fresh Kāra `String` `{ data, len, cap }`. Mirrors the tail
    /// of `compile_request_string_method` but skips the `strlen` step
    /// (the runtime gives us the length directly).
    pub(super) fn compile_request_body(
        &mut self,
        var_name: &str,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let slot = self
            .variables
            .get(var_name)
            .copied()
            .ok_or_else(|| format!("Request var '{var_name}' not bound"))?;
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_ty = self.context.i64_type();

        let req_ptr = self
            .builder
            .build_load(slot.ty, slot.ptr, &format!("{var_name}.req.body.load"))
            .unwrap()
            .into_pointer_value();

        let body_ptr_fn = self
            .module
            .get_function("karac_runtime_http_request_body_ptr")
            .expect("karac_runtime_http_request_body_ptr declared in Codegen::new");
        let body_len_fn = self
            .module
            .get_function("karac_runtime_http_request_body_len")
            .expect("karac_runtime_http_request_body_len declared in Codegen::new");

        let src_ptr = self
            .builder
            .build_call(body_ptr_fn, &[req_ptr.into()], "req.body.src")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        let len_i64 = self
            .builder
            .build_call(body_len_fn, &[req_ptr.into()], "req.body.len")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();

        let zero = i64_ty.const_zero();
        let is_zero = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::EQ,
                len_i64,
                zero,
                "req.body.is_empty",
            )
            .unwrap();
        let fn_val = self
            .current_fn
            .ok_or_else(|| "Request method called outside fn".to_string())?;
        let alloc_bb = self.context.append_basic_block(fn_val, "req.body.alloc");
        let empty_bb = self.context.append_basic_block(fn_val, "req.body.empty");
        let cont_bb = self.context.append_basic_block(fn_val, "req.body.cont");

        let buf_slot = self.create_entry_alloca(fn_val, "req.body.buf", ptr_ty.into());

        self.builder
            .build_conditional_branch(is_zero, empty_bb, alloc_bb)
            .unwrap();

        self.builder.position_at_end(empty_bb);
        self.builder
            .build_store(buf_slot, ptr_ty.const_null())
            .unwrap();
        self.builder.build_unconditional_branch(cont_bb).unwrap();

        self.builder.position_at_end(alloc_bb);
        let buf = self
            .builder
            .build_call(self.malloc_fn, &[len_i64.into()], "req.body.buf.alloc")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        self.builder
            .build_memcpy(buf, 1, src_ptr, 1, len_i64)
            .unwrap();
        self.builder.build_store(buf_slot, buf).unwrap();
        self.builder.build_unconditional_branch(cont_bb).unwrap();

        self.builder.position_at_end(cont_bb);
        let data = self
            .builder
            .build_load(ptr_ty, buf_slot, "req.body.data")
            .unwrap()
            .into_pointer_value();
        let str_ty = self.vec_struct_type();
        let mut str_val: BasicValueEnum<'ctx> = str_ty.get_undef().into();
        str_val = self
            .builder
            .build_insert_value(str_val.into_struct_value(), data, 0, "req.body.data.ins")
            .unwrap()
            .into_struct_value()
            .into();
        str_val = self
            .builder
            .build_insert_value(str_val.into_struct_value(), len_i64, 1, "req.body.len.ins")
            .unwrap()
            .into_struct_value()
            .into();
        str_val = self
            .builder
            .build_insert_value(str_val.into_struct_value(), len_i64, 2, "req.body.cap.ins")
            .unwrap()
            .into_struct_value()
            .into();
        Ok(str_val)
    }

    /// Copy a borrowed runtime `*const c_char` into a fresh owned Kāra
    /// `String` aggregate `{ data, len, cap }` via strlen + malloc +
    /// memcpy. An empty C string yields `{ null, 0, 0 }` (matching how
    /// other empty Kāra Strings are represented elsewhere in codegen).
    /// Emits a `len == 0` empty / non-empty branch and leaves the
    /// builder positioned at the assembled-aggregate (`cont`) block,
    /// returning the String value. Shared by `compile_request_pairs`'s
    /// per-element key/val copies — same per-call ownership contract as
    /// `compile_request_string_method` / `compile_request_header` (the
    /// borrowed pointer is only valid for the duration of the handler).
    fn build_owned_string_from_cstr(
        &mut self,
        cstr_ptr: PointerValue<'ctx>,
        prefix: &str,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_ty = self.context.i64_type();
        let fn_val = self
            .current_fn
            .ok_or_else(|| "string-build called outside fn".to_string())?;

        let strlen_fn = self
            .module
            .get_function("strlen")
            .expect("strlen declared in Codegen::new");
        let len_raw = self
            .builder
            .build_call(strlen_fn, &[cstr_ptr.into()], &format!("{prefix}.len"))
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();
        let len_i64 = self
            .builder
            .build_int_z_extend_or_bit_cast(len_raw, i64_ty, &format!("{prefix}.len.i64"))
            .unwrap();

        let zero = i64_ty.const_zero();
        let is_empty = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::EQ,
                len_i64,
                zero,
                &format!("{prefix}.is_empty"),
            )
            .unwrap();
        let alloc_bb = self
            .context
            .append_basic_block(fn_val, &format!("{prefix}.alloc"));
        let empty_bb = self
            .context
            .append_basic_block(fn_val, &format!("{prefix}.empty"));
        let cont_bb = self
            .context
            .append_basic_block(fn_val, &format!("{prefix}.cont"));
        let buf_slot = self.create_entry_alloca(fn_val, &format!("{prefix}.buf"), ptr_ty.into());

        self.builder
            .build_conditional_branch(is_empty, empty_bb, alloc_bb)
            .unwrap();

        self.builder.position_at_end(empty_bb);
        self.builder
            .build_store(buf_slot, ptr_ty.const_null())
            .unwrap();
        self.builder.build_unconditional_branch(cont_bb).unwrap();

        self.builder.position_at_end(alloc_bb);
        let buf = self
            .builder
            .build_call(
                self.malloc_fn,
                &[len_i64.into()],
                &format!("{prefix}.buf.alloc"),
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        self.builder
            .build_memcpy(buf, 1, cstr_ptr, 1, len_i64)
            .unwrap();
        self.builder.build_store(buf_slot, buf).unwrap();
        self.builder.build_unconditional_branch(cont_bb).unwrap();

        self.builder.position_at_end(cont_bb);
        let data = self
            .builder
            .build_load(ptr_ty, buf_slot, &format!("{prefix}.data"))
            .unwrap()
            .into_pointer_value();
        let str_ty = self.vec_struct_type();
        let mut str_val: BasicValueEnum<'ctx> = str_ty.get_undef().into();
        str_val = self
            .builder
            .build_insert_value(
                str_val.into_struct_value(),
                data,
                0,
                &format!("{prefix}.data.ins"),
            )
            .unwrap()
            .into_struct_value()
            .into();
        str_val = self
            .builder
            .build_insert_value(
                str_val.into_struct_value(),
                len_i64,
                1,
                &format!("{prefix}.len.ins"),
            )
            .unwrap()
            .into_struct_value()
            .into();
        str_val = self
            .builder
            .build_insert_value(
                str_val.into_struct_value(),
                len_i64,
                2,
                &format!("{prefix}.cap.ins"),
            )
            .unwrap()
            .into_struct_value()
            .into();
        Ok(str_val)
    }

    /// Compile `req.headers()` / `req.query()` for a `Request`-typed
    /// local into a `Vec[(String, String)]`. Both round-trip through a
    /// `count` extern (the loop bound) plus index-for-index `key_at` /
    /// `val_at` externs; the only difference is which runtime function
    /// triplet `kind` selects. Allocates a `n * sizeof((String,String))`
    /// buffer, then a counted loop copies each borrowed key/val cstring
    /// into a fresh owned String (via `build_owned_string_from_cstr`),
    /// assembles the `(String, String)` tuple, and stores it at its
    /// slot. `n == 0` short-circuits to the empty-Vec `{ null, 0, 0 }`
    /// invariant. The resulting Vec owns its element Strings, so it
    /// outlives the request struct (dropped after the handler returns).
    pub(super) fn compile_request_pairs(
        &mut self,
        var_name: &str,
        kind: RequestPairsKind,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let (count_name, key_name, val_name) = match kind {
            RequestPairsKind::Headers => (
                "karac_runtime_http_request_headers_count",
                "karac_runtime_http_request_header_key_at",
                "karac_runtime_http_request_header_val_at",
            ),
            RequestPairsKind::Query => (
                "karac_runtime_http_request_query_count",
                "karac_runtime_http_request_query_key_at",
                "karac_runtime_http_request_query_val_at",
            ),
        };
        let slot = self
            .variables
            .get(var_name)
            .copied()
            .ok_or_else(|| format!("Request var '{var_name}' not bound"))?;
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_ty = self.context.i64_type();
        let fn_val = self
            .current_fn
            .ok_or_else(|| "Request.headers/query called outside fn".to_string())?;

        let req_ptr = self
            .builder
            .build_load(slot.ty, slot.ptr, &format!("{var_name}.req.pairs.load"))
            .unwrap()
            .into_pointer_value();

        let count_fn = self
            .module
            .get_function(count_name)
            .unwrap_or_else(|| panic!("{count_name} declared in Codegen::new"));
        let key_fn = self
            .module
            .get_function(key_name)
            .unwrap_or_else(|| panic!("{key_name} declared in Codegen::new"));
        let val_fn = self
            .module
            .get_function(val_name)
            .unwrap_or_else(|| panic!("{val_name} declared in Codegen::new"));

        let n = self
            .builder
            .build_call(count_fn, &[req_ptr.into()], "req.pairs.n")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();

        // Element type is the `(String, String)` tuple: two `{ ptr, i64,
        // i64 }` String aggregates back to back (mirrors `compile_tuple`'s
        // lowering and `Vec[(String, String)]`'s element type).
        let str_ty = self.vec_struct_type();
        let elem_ty = self
            .context
            .struct_type(&[str_ty.into(), str_ty.into()], false);
        let vec_ty = self.vec_struct_type();

        let zero = i64_ty.const_zero();
        let is_zero = self
            .builder
            .build_int_compare(inkwell::IntPredicate::EQ, n, zero, "req.pairs.is_empty")
            .unwrap();
        let empty_bb = self.context.append_basic_block(fn_val, "req.pairs.empty");
        let build_bb = self.context.append_basic_block(fn_val, "req.pairs.build");
        let done_bb = self.context.append_basic_block(fn_val, "req.pairs.done");
        let buf_slot = self.create_entry_alloca(fn_val, "req.pairs.buf", ptr_ty.into());

        self.builder
            .build_conditional_branch(is_zero, empty_bb, build_bb)
            .unwrap();

        // Empty: null buffer, len/cap 0 (the canonical empty-Vec shape).
        self.builder.position_at_end(empty_bb);
        self.builder
            .build_store(buf_slot, ptr_ty.const_null())
            .unwrap();
        self.builder.build_unconditional_branch(done_bb).unwrap();

        // Non-empty: malloc(n * elem_size), then fill via a counted loop.
        self.builder.position_at_end(build_bb);
        let elem_size = elem_ty.size_of().unwrap();
        let alloc_bytes = self
            .builder
            .build_int_mul(n, elem_size, "req.pairs.alloc_bytes")
            .unwrap();
        let buf = self
            .builder
            .build_call(self.malloc_fn, &[alloc_bytes.into()], "req.pairs.buf.alloc")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        self.builder.build_store(buf_slot, buf).unwrap();

        let i_alloca = self.create_entry_alloca(fn_val, "req.pairs.i", i64_ty.into());
        self.builder
            .build_store(i_alloca, i64_ty.const_zero())
            .unwrap();
        let cond_bb = self.context.append_basic_block(fn_val, "req.pairs.cond");
        let body_bb = self.context.append_basic_block(fn_val, "req.pairs.body");
        let exit_bb = self.context.append_basic_block(fn_val, "req.pairs.exit");
        self.builder.build_unconditional_branch(cond_bb).unwrap();

        self.builder.position_at_end(cond_bb);
        let i_cur = self
            .builder
            .build_load(i64_ty, i_alloca, "req.pairs.i.cur")
            .unwrap()
            .into_int_value();
        let lt = self
            .builder
            .build_int_compare(inkwell::IntPredicate::ULT, i_cur, n, "req.pairs.lt")
            .unwrap();
        self.builder
            .build_conditional_branch(lt, body_bb, exit_bb)
            .unwrap();

        self.builder.position_at_end(body_bb);
        let key_cstr = self
            .builder
            .build_call(
                key_fn,
                &[req_ptr.into(), i_cur.into()],
                "req.pairs.key.cstr",
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        let key_str = self.build_owned_string_from_cstr(key_cstr, "req.pairs.key")?;
        let val_cstr = self
            .builder
            .build_call(
                val_fn,
                &[req_ptr.into(), i_cur.into()],
                "req.pairs.val.cstr",
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        let val_str = self.build_owned_string_from_cstr(val_cstr, "req.pairs.val")?;

        let mut tuple_val: BasicValueEnum<'ctx> = elem_ty.get_undef().into();
        tuple_val = self
            .builder
            .build_insert_value(tuple_val.into_struct_value(), key_str, 0, "req.pairs.tup.k")
            .unwrap()
            .into_struct_value()
            .into();
        tuple_val = self
            .builder
            .build_insert_value(tuple_val.into_struct_value(), val_str, 1, "req.pairs.tup.v")
            .unwrap()
            .into_struct_value()
            .into();

        // Reload the buffer base (the SSA `buf` from `build_bb` dominates
        // here, but reloading keeps the GEP base local to the loop body).
        let buf_cur = self
            .builder
            .build_load(ptr_ty, buf_slot, "req.pairs.buf.cur")
            .unwrap()
            .into_pointer_value();
        let elem_ptr = unsafe {
            self.builder
                .build_gep(elem_ty, buf_cur, &[i_cur], "req.pairs.elem.ptr")
                .unwrap()
        };
        self.builder.build_store(elem_ptr, tuple_val).unwrap();

        let one = i64_ty.const_int(1, false);
        let i_next = self
            .builder
            .build_int_add(i_cur, one, "req.pairs.i.next")
            .unwrap();
        self.builder.build_store(i_alloca, i_next).unwrap();
        self.builder.build_unconditional_branch(cond_bb).unwrap();

        self.builder.position_at_end(exit_bb);
        self.builder.build_unconditional_branch(done_bb).unwrap();

        // Assemble the `{ data, len, cap }` Vec aggregate. `n` dominates
        // `done_bb` (computed before the empty/build split), so it serves
        // as both len and cap for the non-empty path and is 0 for empty.
        self.builder.position_at_end(done_bb);
        let data = self
            .builder
            .build_load(ptr_ty, buf_slot, "req.pairs.data")
            .unwrap()
            .into_pointer_value();
        let mut agg = vec_ty.get_undef();
        agg = self
            .builder
            .build_insert_value(agg, data, 0, "req.pairs.vec.data")
            .unwrap()
            .into_struct_value();
        agg = self
            .builder
            .build_insert_value(agg, n, 1, "req.pairs.vec.len")
            .unwrap()
            .into_struct_value();
        agg = self
            .builder
            .build_insert_value(agg, n, 2, "req.pairs.vec.cap")
            .unwrap()
            .into_struct_value();
        Ok(agg.into())
    }

    /// Phase-8 line 17 slice 2 — lower `Client.get(url)` / `Client.post(
    /// url, body)` to `karac_runtime_http_client_{get,post}` and pack
    /// the out-params into the seeded 5-word `Result[Response,
    /// HttpError]` aggregate.
    ///
    /// The runtime FFI returns five out-params: `status` (HTTP status
    /// 1xx–5xx on success, `0` flags a transport error); `body_ptr` /
    /// `body_len` (malloc'd body bytes on success — caller owns,
    /// freed via String Drop); `err_ptr` / `err_len` (malloc'd
    /// error-message bytes on transport error). `cap = len` for both
    /// String buffers.
    ///
    /// Packing into `Result[Response, HttpError]` (5-word `{tag, w0,
    /// w1, w2, w3}`):
    ///
    /// - Ok arm (status > 0): tag=Ok, w0=status, w1=body.data,
    ///   w2=body.len, w3=body.cap.
    /// - Err arm (status == 0): tag=Err, w0=msg.data, w1=msg.len,
    ///   w2=msg.cap, w3=0.
    ///
    /// Caller is the std.http client method-call dispatch arm in
    /// `compile_method_call`. Receiver is `ref self` on an empty
    /// `Client { }` struct — codegen ignores it.
    pub(super) fn compile_client_http_method(
        &mut self,
        method: &str,
        args: &[CallArg],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let expected_args = if method == "post" { 2 } else { 1 };
        if args.len() != expected_args {
            return Err(format!(
                "Client.{method} expects {expected_args} argument(s), got {}",
                args.len()
            ));
        }
        let fn_val = self
            .current_fn
            .ok_or_else(|| format!("Client.{method} called outside fn"))?;
        let ctx = self.context;
        let i64_ty = ctx.i64_type();
        let ptr_ty = ctx.ptr_type(AddressSpace::default());

        // Receiver is `ref self` on an empty `Client { }` struct —
        // zero-sized, no self param threaded into the FFI. Skip
        // compiling it entirely.

        // Arg 0: URL String — extract data/len from the `{data, len,
        // cap}` aggregate. Same shape as `compile_request_string_method`'s
        // input handling. The runtime borrows the pointer + length; no
        // copy needed at this layer.
        let url_val = self.compile_expr(&args[0].value)?;
        let url_sv = url_val.into_struct_value();
        let url_data = self
            .builder
            .build_extract_value(url_sv, 0, "client.url.data")
            .unwrap()
            .into_pointer_value();
        let url_len = self
            .builder
            .build_extract_value(url_sv, 1, "client.url.len")
            .unwrap()
            .into_int_value();

        // Arg 1 (post only): body String.
        let body_args: Option<(PointerValue<'ctx>, inkwell::values::IntValue<'ctx>)> =
            if method == "post" {
                let body_val = self.compile_expr(&args[1].value)?;
                let body_sv = body_val.into_struct_value();
                let body_data = self
                    .builder
                    .build_extract_value(body_sv, 0, "client.body.data")
                    .unwrap()
                    .into_pointer_value();
                let body_len = self
                    .builder
                    .build_extract_value(body_sv, 1, "client.body.len")
                    .unwrap()
                    .into_int_value();
                Some((body_data, body_len))
            } else {
                None
            };

        // Allocate the five out-param slots. `i64` for status / body_len
        // / err_len; pointer-typed for the two `*mut *mut u8` slots.
        let status_slot = self.create_entry_alloca(fn_val, "client.out.status", i64_ty.into());
        let body_ptr_slot = self.create_entry_alloca(fn_val, "client.out.body_ptr", ptr_ty.into());
        let body_len_slot = self.create_entry_alloca(fn_val, "client.out.body_len", i64_ty.into());
        let err_ptr_slot = self.create_entry_alloca(fn_val, "client.out.err_ptr", ptr_ty.into());
        let err_len_slot = self.create_entry_alloca(fn_val, "client.out.err_len", i64_ty.into());

        // Call the runtime extern.
        let extern_name = if method == "get" {
            "karac_runtime_http_client_get"
        } else {
            "karac_runtime_http_client_post"
        };
        let extern_fn = self
            .module
            .get_function(extern_name)
            .unwrap_or_else(|| panic!("{extern_name} declared in Codegen::new"));
        let mut call_args: Vec<BasicMetadataValueEnum<'ctx>> =
            vec![url_data.into(), url_len.into()];
        if let Some((body_data, body_len)) = body_args {
            call_args.push(body_data.into());
            call_args.push(body_len.into());
        }
        call_args.extend_from_slice(&[
            status_slot.into(),
            body_ptr_slot.into(),
            body_len_slot.into(),
            err_ptr_slot.into(),
            err_len_slot.into(),
        ]);
        self.builder
            .build_call(extern_fn, &call_args, &format!("client.{method}.call"))
            .unwrap();

        // Load the five out-param values.
        let status_val = self
            .builder
            .build_load(i64_ty, status_slot, "client.status")
            .unwrap()
            .into_int_value();
        let body_ptr_val = self
            .builder
            .build_load(ptr_ty, body_ptr_slot, "client.body_ptr")
            .unwrap()
            .into_pointer_value();
        let body_len_val = self
            .builder
            .build_load(i64_ty, body_len_slot, "client.body_len")
            .unwrap()
            .into_int_value();
        let err_ptr_val = self
            .builder
            .build_load(ptr_ty, err_ptr_slot, "client.err_ptr")
            .unwrap()
            .into_pointer_value();
        let err_len_val = self
            .builder
            .build_load(i64_ty, err_len_slot, "client.err_len")
            .unwrap()
            .into_int_value();

        // Build the Result[Response, HttpError] aggregate.
        let result_layout = self
            .enum_layouts
            .get("Result")
            .expect("Result layout registered before Client.{get,post} dispatch");
        let result_ty = result_layout.llvm_type;
        let result_slot = self.create_entry_alloca(fn_val, "client.result", result_ty.into());
        let total_fields = result_ty.count_fields() as u64;

        // Use the seeded variant tags. `enum_tag_for_variant` prefers
        // the user-declared Result over the seeded one, so this picks
        // up the canonical 0=Ok / 1=Err tags from the baked stdlib.
        let ok_tag = self
            .enum_tag_for_variant("Ok")
            .expect("Ok variant tag registered before Client.{get,post} dispatch");
        let err_tag = self
            .enum_tag_for_variant("Err")
            .expect("Err variant tag registered before Client.{get,post} dispatch");

        // Branch: status > 0 → Ok arm; status == 0 → Err arm.
        let zero_i64 = i64_ty.const_zero();
        let is_ok = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::SGT,
                status_val,
                zero_i64,
                "client.is_ok",
            )
            .unwrap();
        let ok_bb = ctx.append_basic_block(fn_val, "client.ok");
        let err_bb = ctx.append_basic_block(fn_val, "client.err");
        let cont_bb = ctx.append_basic_block(fn_val, "client.cont");
        self.builder
            .build_conditional_branch(is_ok, ok_bb, err_bb)
            .unwrap();

        // Ok arm: pack Response { status, body } into w0..w3.
        self.builder.position_at_end(ok_bb);
        let tag_ptr_ok = self
            .builder
            .build_struct_gep(result_ty, result_slot, 0, "ok.tag")
            .unwrap();
        self.builder
            .build_store(tag_ptr_ok, i64_ty.const_int(ok_tag, false))
            .unwrap();
        let body_ptr_int = self
            .builder
            .build_ptr_to_int(body_ptr_val, i64_ty, "ok.body_ptr.i64")
            .unwrap();
        if total_fields > 1 {
            let p = self
                .builder
                .build_struct_gep(result_ty, result_slot, 1, "ok.w0.status")
                .unwrap();
            self.builder.build_store(p, status_val).unwrap();
        }
        if total_fields > 2 {
            let p = self
                .builder
                .build_struct_gep(result_ty, result_slot, 2, "ok.w1.body.data")
                .unwrap();
            self.builder.build_store(p, body_ptr_int).unwrap();
        }
        if total_fields > 3 {
            let p = self
                .builder
                .build_struct_gep(result_ty, result_slot, 3, "ok.w2.body.len")
                .unwrap();
            self.builder.build_store(p, body_len_val).unwrap();
        }
        if total_fields > 4 {
            // cap = len (the runtime malloc'd exactly len bytes).
            let p = self
                .builder
                .build_struct_gep(result_ty, result_slot, 4, "ok.w3.body.cap")
                .unwrap();
            self.builder.build_store(p, body_len_val).unwrap();
        }
        for w in 5..total_fields {
            let p = self
                .builder
                .build_struct_gep(result_ty, result_slot, w as u32, &format!("ok.w{w}"))
                .unwrap();
            self.builder.build_store(p, zero_i64).unwrap();
        }
        self.builder.build_unconditional_branch(cont_bb).unwrap();

        // Err arm: pack HttpError { message: String } into w0..w2
        // (struct GEP indices 1, 2, 3). The pattern destructure for
        // `Err(e)` slices the first `pattern_payload_word_count(e)`
        // words from the payload area — for `e: HttpError` that's 3
        // (matching the `{String}` shape `{ptr, i64, i64}`). Packing
        // at w0..w2 means the slice picks up `data, len, cap` in
        // declaration order. w3 stays zeroed so the reconstruction
        // doesn't read stale stack bits when the seeded Result layout
        // ever widens.
        self.builder.position_at_end(err_bb);
        let tag_ptr_err = self
            .builder
            .build_struct_gep(result_ty, result_slot, 0, "err.tag")
            .unwrap();
        self.builder
            .build_store(tag_ptr_err, i64_ty.const_int(err_tag, false))
            .unwrap();
        let err_ptr_int = self
            .builder
            .build_ptr_to_int(err_ptr_val, i64_ty, "err.msg.ptr.i64")
            .unwrap();
        if total_fields > 1 {
            let p = self
                .builder
                .build_struct_gep(result_ty, result_slot, 1, "err.w0.msg.data")
                .unwrap();
            self.builder.build_store(p, err_ptr_int).unwrap();
        }
        if total_fields > 2 {
            let p = self
                .builder
                .build_struct_gep(result_ty, result_slot, 2, "err.w1.msg.len")
                .unwrap();
            self.builder.build_store(p, err_len_val).unwrap();
        }
        if total_fields > 3 {
            let p = self
                .builder
                .build_struct_gep(result_ty, result_slot, 3, "err.w2.msg.cap")
                .unwrap();
            self.builder.build_store(p, err_len_val).unwrap();
        }
        for w in 4..total_fields {
            let p = self
                .builder
                .build_struct_gep(result_ty, result_slot, w as u32, &format!("err.w{w}"))
                .unwrap();
            self.builder.build_store(p, zero_i64).unwrap();
        }
        self.builder.build_unconditional_branch(cont_bb).unwrap();

        // Cont: load + return the result aggregate.
        self.builder.position_at_end(cont_bb);
        let result = self
            .builder
            .build_load(result_ty, result_slot, &format!("client.{method}.result"))
            .unwrap();
        Ok(result)
    }

    /// Phase-8 line 17 slice 3 — `resp.status() -> i64` /
    /// `resp.body() -> String`. The stdlib stubs are `#[compiler_builtin]`
    /// so the bodies are never compiled into the user binary; this helper
    /// lowers the method call to a direct field read on the receiver's
    /// struct value (Response = `{ i64 status, String body }`).
    ///
    /// `status` is a primitive `i64` — copy-by-value, no ownership
    /// concerns. `body` is an owned `String`; the field carries a
    /// `{data, len, cap}` aggregate the receiver's drop will free, so
    /// we route through `karac_string_clone` to hand the caller a
    /// fresh buffer they own outright.
    pub(super) fn compile_response_accessor(
        &mut self,
        var_name: &str,
        method: &str,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let slot = self
            .variables
            .get(var_name)
            .copied()
            .ok_or_else(|| format!("Response var '{var_name}' not bound"))?;
        // `Response { status: i64, body: String }` — seeded into
        // `struct_types` by `seed_builtin_struct_types`. Layout =
        // `{ i64, { ptr, i64, i64 } }`.
        let resp_ty = self
            .struct_types
            .get("Response")
            .copied()
            .expect("Response struct type seeded by seed_builtin_struct_types");
        match method {
            "status" => {
                let field_ptr = self
                    .builder
                    .build_struct_gep(resp_ty, slot.ptr, 0, "resp.status.ptr")
                    .map_err(|e| format!("resp.status gep failed: {e:?}"))?;
                let val = self
                    .builder
                    .build_load(self.context.i64_type(), field_ptr, "resp.status")
                    .unwrap();
                Ok(val)
            }
            "body" => self.clone_string_field(slot.ptr, resp_ty, 1, "resp.body"),
            other => Err(format!(
                "compile_response_accessor called with unsupported method '{other}'"
            )),
        }
    }

    /// Phase-8 line 17 slice 3 — `e.message() -> String` on
    /// `HttpError { message: String }`. Same `karac_string_clone`-based
    /// ownership transfer as `Response.body()`. Layout seeded into
    /// `struct_types` by `seed_builtin_struct_types`.
    pub(super) fn compile_http_error_message(
        &mut self,
        var_name: &str,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let slot = self
            .variables
            .get(var_name)
            .copied()
            .ok_or_else(|| format!("HttpError var '{var_name}' not bound"))?;
        let err_ty = self
            .struct_types
            .get("HttpError")
            .copied()
            .expect("HttpError struct type seeded by seed_builtin_struct_types");
        self.clone_string_field(slot.ptr, err_ty, 0, "httperror.message")
    }

    /// Deep-clone the `String` field at `field_idx` on the struct stored
    /// at `slot_ptr`. Mirrors the contract `Response.body()` /
    /// `HttpError.message()` need: receiver owns the field's buffer; the
    /// caller takes ownership of a fresh copy via `karac_string_clone`.
    fn clone_string_field(
        &mut self,
        slot_ptr: PointerValue<'ctx>,
        struct_ty: inkwell::types::StructType<'ctx>,
        field_idx: u32,
        label: &str,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let fn_val = self
            .current_fn
            .ok_or_else(|| format!("{label} called outside fn"))?;
        let str_ty = self.vec_struct_type();
        let src_ptr = self
            .builder
            .build_struct_gep(struct_ty, slot_ptr, field_idx, &format!("{label}.src.ptr"))
            .map_err(|e| format!("{label} gep failed: {e:?}"))?;
        let dst_slot = self.create_entry_alloca(fn_val, &format!("{label}.dst"), str_ty.into());
        self.builder
            .build_call(
                self.karac_string_clone_fn,
                &[src_ptr.into(), dst_slot.into()],
                &format!("{label}.clone"),
            )
            .unwrap();
        let cloned = self
            .builder
            .build_load(str_ty, dst_slot, &format!("{label}.val"))
            .unwrap();
        Ok(cloned)
    }
}
