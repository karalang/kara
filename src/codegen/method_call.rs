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
            "unwrap" | "expect" | "is_some" | "is_none" | "is_ok" | "is_err"
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
        if (method == "load" || method == "store") && self.is_atomic_receiver(object) {
            return self.compile_atomic_method(object, method, args);
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
                // Receiver storage name for the ptr-self ABI. Both `obj`
                // (Identifier) and `self` (SelfValue, registered under the
                // synthesized "self" param) resolve to a data pointer; any
                // other shape has no stable storage to address.
                let recv_storage_name: Option<&str> = match &object.kind {
                    ExprKind::Identifier(var_name) => Some(var_name.as_str()),
                    ExprKind::SelfValue => Some("self"),
                    _ => None,
                };
                let receiver_arg: BasicMetadataValueEnum<'ctx> = if first_param_is_ptr {
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
        if !matches!(&object.kind, ExprKind::Identifier(_)) && matches!(method, "len" | "is_empty")
        {
            let recv_val = self.compile_expr(object)?;
            if let BasicValueEnum::StructValue(sv) = recv_val {
                let vec_ty = self.vec_struct_type();
                if sv.get_type() == vec_ty {
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
    /// merge:    phi i64 [r1, override], [r2, default]
    /// ```
    /// All ambient methods codegen lowers today return an i64-shaped slot
    /// (`Clock.now` real i64; `Env.set` the unit-return placeholder), so
    /// the phi is uniformly i64. A null fn-ptr slot (override implements
    /// only some methods) would null-deref in the override arm — but the
    /// override arm is only taken when a frame is active, and a fixture
    /// only overrides methods it implements, so the implemented slot is
    /// non-null. (Generalizing the phi type for non-i64 ambient methods is
    /// tracked with the remaining-ambient-methods lowering gap.)
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

        // merge: phi the two i64 results.
        self.builder.position_at_end(merge_bb);
        let phi = self.builder.build_phi(i64_t, "amb.result").unwrap();
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
            _ => Err(format!(
                "codegen: ambient resource method '{}.{}' is not yet lowered \
                 (interpreter-only); add a runtime FFI + an arm in \
                 `compile_ambient_ffi`",
                resource, method
            )),
        }
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
            _ => unreachable!("compile_atomic_method gated on method in {{load, store}}"),
        }
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
        // Helper: coerce a compiled receiver / argument value to i64,
        // regardless of whether it flows as `PointerValue` (rare with
        // the current ABI but possible for an intermediate result) or
        // `IntValue`. Matches the same pattern used by
        // `call_dispatch::coerce_to_i64` for the message-payload path.
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
            // `!provenance` metadata to reseat the address bits;
            // current ABI represents both ptr and usize as i64, so the
            // result is just `addr` coerced to i64.
            "with_addr" | "with_addr_mut" if args.len() == 2 => {
                let _ = self.compile_expr(&args[0].value)?;
                let a = self.compile_expr(&args[1].value)?;
                let label = if method == "with_addr" {
                    "ptr.with_addr"
                } else {
                    "ptr.with_addr_mut"
                };
                Ok(Some(to_i64(self, a, label)))
            }
            // addr: usize -> *_ T  (ptr.from_exposed / ptr.from_exposed_mut)
            "from_exposed" | "from_exposed_mut" if args.len() == 1 => {
                let a = self.compile_expr(&args[0].value)?;
                let label = if method == "from_exposed" {
                    "ptr.from_exposed"
                } else {
                    "ptr.from_exposed_mut"
                };
                Ok(Some(to_i64(self, a, label)))
            }
            // (field_ptr: *_ F, offset: usize) -> *_ T
            //   (ptr.container_of / ptr.container_of_mut)
            //
            // Intrusive-DS pointer recovery — subtract the field
            // offset from the field-pointer's address bits. The
            // provenance-preserving lowering the spec describes is
            // `field_ptr.with_addr(field_ptr.addr() - offset)` — under
            // the current i64-pointer ABI that collapses to plain
            // integer subtraction, which is what we emit here. The
            // `with_addr` and `addr` round-trips are no-ops at the
            // LLVM level (see slice 3's helper-docstring).
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
                        label,
                    )
                    .unwrap();
                Ok(Some(result.into()))
            }
            // `ptr.const(place)` / `ptr.mut(place)` — raw pointer
            // construction from a place expression (typechecker
            // place-validator gate is upstream — design.md § Raw
            // Pointer Construction, v60 item 19). Under the i64-
            // pointer ABI, the result is the place's storage address
            // coerced to i64. v1 covers the two common shapes:
            //  - Identifier place: look up the binding's storage
            //    slot via `get_data_ptr` (handles owned alloca and
            //    ref-param indirection), then `ptrtoint` to i64.
            //  - Deref of an already-pointer SSA: the operand's i64
            //    value *is* the address; emit the operand's compile
            //    result directly.
            // Field / index / nested-deref places fall through to
            // the generic identifier path via the synth-identifier
            // mechanism if reachable; a focused diagnostic for
            // unsupported shapes lands as a follow-up.
            "const" | "mut" if args.len() == 1 => {
                let place = &args[0].value;
                match &place.kind {
                    ExprKind::Identifier(name) => {
                        if let Some(ptr) = self.get_data_ptr(name) {
                            let bits = self
                                .builder
                                .build_ptr_to_int(ptr, i64_ty, "ptr.place.addr")
                                .unwrap();
                            return Ok(Some(bits.into()));
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
                        Ok(Some(to_i64(self, v, "ptr.place.deref")))
                    }
                    _ => Ok(None),
                }
            }
            // `ptr.null[T]()` / `ptr.null_mut[T]()` -> the all-zeroes
            // pointer (LLVM null). Under the current i64-pointer ABI,
            // this is the i64 constant 0. The two methods differ only
            // in their typechecker-reported return type (`*const T`
            // vs `*mut T`); the codegen value is identical.
            "null" | "null_mut" if args.is_empty() => Ok(Some(i64_ty.const_int(0, false).into())),
            // `ptr.dangling[T]()` / `ptr.dangling_mut[T]()` -> a
            // non-null pointer aligned to T's natural alignment, *not*
            // dereferenceable. Spec: design.md § Raw Pointer
            // Construction (v60 item 19); mirrors Rust's
            // `NonNull::dangling` (= `align_of::<T>() as *const T`).
            //
            // T-aware lowering would consult the type argument and
            // emit `align_of[T]`. The current i64-pointer ABI does
            // not thread the type argument to this hook, so v1 emits
            // a fixed alignment of 8 (the max alignment of any built-
            // in primitive on a 64-bit target — correct for every T
            // whose alignment is <= 8, conservative for over-aligned
            // SIMD / `#[repr(align(N))]` types). The actual deref of
            // a dangling pointer is unsafe and *always* UB; the only
            // observable property is non-null + alignment, both of
            // which hold here. Tracker: phase-5-diagnostics line 573.
            "dangling" | "dangling_mut" if args.is_empty() => {
                Ok(Some(i64_ty.const_int(8, false).into()))
            }
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
