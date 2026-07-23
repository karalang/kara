//! `Pool[T]` AOT codegen — `acquire` / `release` lowering (phase-8 backend
//! platform). Slice 2 of the `Pool[T]` codegen work; slice 1 (the runtime
//! substrate + ABI + type layer) is `runtime/src/pool.rs` +
//! `compiled_stdlib_programs`.
//!
//! Same opaque-handle shape as `BoundedChannel` / `Semaphore`: the receiver is a
//! `Pool { handle_id: i64 }` whose field is the `*mut KaracPool` round-tripped
//! through `ptrtoint`/`inttoptr`. `Pool.new` (an `assoc_call.rs` intercept)
//! stores the `create_fn` fat pointer + bounds in the runtime; the runtime is
//! TYPE-ERASED (every `T` moves as `elem_size` opaque bytes) and never calls
//! Kāra code.
//!
//! **Codegen-orchestrated minting.** `acquire` calls
//! `karac_runtime_pool_begin_acquire`, which returns a status: reuse an idle
//! slot (codegen loads it), reserve a new mint (codegen reconstructs the stored
//! `create_fn` fat pointer and does the ABI-correct indirect call HERE, in the
//! monomorph where `T`'s layout is known), or fail at cap / closed. This is what
//! keeps the runtime `T`-agnostic without a runtime->Kāra callback.
//!
//! **v1 scope.** `T` must be POD (all-scalar / no-heap-field) — idle slots are
//! freed wholesale at `pool_drop`, so a heap-owning `T` would leak (matches the
//! `Arena[T]` codegen staging). `create_fn` must have a NULL env (bare fn /
//! non-capturing lambda); a capturing closure is gated out at `Pool.new`.

use super::CallArg;
use crate::ast::{Expr, GenericArg, TypeExpr, TypeKind};
use inkwell::values::{BasicValueEnum, PointerValue};

impl<'ctx> super::Codegen<'ctx> {
    /// Dispatch `Pool.acquire` / `Pool.release`, gated on the receiver's static
    /// type name so an unrelated `acquire` / `release` falls through. `Pool.new`
    /// is intercepted separately in `assoc_call.rs`; the two Drops in
    /// `synth_drop.rs`.
    pub(super) fn try_compile_pool_method(
        &mut self,
        object: &Expr,
        method: &str,
        args: &[CallArg],
        call_span: &crate::token::Span,
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        if !matches!(method, "acquire" | "release") {
            return Ok(None);
        }
        let Some(type_name) = self.type_name_of_expr(object) else {
            return Ok(None);
        };
        if type_name != "Pool" {
            return Ok(None);
        }
        match (method, args.len()) {
            ("acquire", 1) => self.compile_pool_acquire(object, call_span).map(Some),
            ("release", 1) => self.compile_pool_release(object, args, call_span).map(Some),
            _ => Ok(None),
        }
    }

    /// Peel the pool's element type `T` out of a recorded instantiation type —
    /// `Pool[T]`, `Result[PooledConnection[T], PoolError]`, or a bare
    /// `PooledConnection[T]` — all of which carry `T` in an extractable position.
    fn peel_pool_elem_te(te: &TypeExpr) -> Option<TypeExpr> {
        let TypeKind::Path(p) = &te.kind else {
            return None;
        };
        let last = p.segments.last().map(String::as_str)?;
        let args = p.generic_args.as_ref()?;
        match last {
            "Pool" | "PooledConnection" => match args.first()? {
                GenericArg::Type(t) => Some(t.clone()),
                _ => None,
            },
            "Result" => {
                let GenericArg::Type(ok) = args.first()? else {
                    return None;
                };
                Self::peel_pool_elem_te(ok)
            }
            _ => None,
        }
    }

    /// Recover `T` from the recorded generic-`Named` type at `span` (the
    /// `acquire` call site records `Pool[T]` / `Result[PooledConnection[T], _]`;
    /// a `release` argument records `PooledConnection[T]`). `None` when no
    /// instantiation was recorded (e.g. a hand-rolled `Pool { handle_id }`).
    fn pool_elem_te_at(&self, span: &crate::token::Span) -> Option<TypeExpr> {
        let te = self.enum_inst_type_exprs.get(&(span.offset, span.length))?;
        Self::peel_pool_elem_te(te)
    }

    /// Recover the runtime `*mut KaracPool` from a `Pool { handle_id: i64 }`
    /// receiver.
    fn pool_handle_ptr(&mut self, object: &Expr) -> Result<PointerValue<'ctx>, String> {
        let recv = self.compile_expr(object)?.into_struct_value();
        let handle = self
            .builder
            .build_extract_value(recv, 0, "pool.handle")
            .unwrap()
            .into_int_value();
        let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        Ok(self
            .builder
            .build_int_to_ptr(handle, ptr_ty, "pool.ptr")
            .unwrap())
    }

    /// `pool.acquire(timeout) -> Result[PooledConnection[T], PoolError]`. Calls
    /// `begin_acquire` and switches on its status: GOT_IDLE loads the slot,
    /// NEED_MINT indirect-calls the stored `create_fn` fat pointer, AT_CAP →
    /// `Err(Timeout)`, CLOSED → `Err(PoolClosed)`. Builds a `PooledConnection {
    /// pool_handle_id, conn_id, val: T }` on the Ok paths and wraps it.
    fn compile_pool_acquire(
        &mut self,
        object: &Expr,
        call_span: &crate::token::Span,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let elem_te = self.pool_elem_te_at(call_span).ok_or_else(|| {
            "Pool.acquire: could not recover element type T at the call site \
             (the typechecker should record Pool[T] / Result[PooledConnection[T], _] there)"
                .to_string()
        })?;
        let elem_ty = self.llvm_type_for_type_expr(&elem_te);
        let i64_t = self.context.i64_type();
        let i32_t = self.context.i32_type();
        let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        let fn_val = self.current_fn.unwrap();

        // PooledConnection[T] LLVM type: { i64 pool_handle_id, i64 conn_id, T val }.
        let pc_ty = self
            .mono_struct_type("PooledConnection", &[GenericArg::Type(elem_te.clone())])
            .ok_or_else(|| {
                "Pool.acquire: PooledConnection[T] layout unavailable (pool stdlib not declared?)"
                    .to_string()
            })?;

        // Receiver handle + the i64 handle value (reused as pool_handle_id).
        let recv = self.compile_expr(object)?.into_struct_value();
        let handle_i64 = self
            .builder
            .build_extract_value(recv, 0, "pool.handle")
            .unwrap()
            .into_int_value();
        let pool_ptr = self
            .builder
            .build_int_to_ptr(handle_i64, ptr_ty, "pool.ptr")
            .unwrap();

        // Out-params for begin_acquire.
        let out_val = self.create_entry_alloca(fn_val, "pool.out_val", elem_ty);
        let out_conn = self.create_entry_alloca(fn_val, "pool.out_conn", i64_t.into());
        let out_fn = self.create_entry_alloca(fn_val, "pool.out_fn", i64_t.into());
        let out_env = self.create_entry_alloca(fn_val, "pool.out_env", i64_t.into());

        let begin_fn = self
            .module
            .get_function("karac_runtime_pool_begin_acquire")
            .expect("karac_runtime_pool_begin_acquire declared in Codegen::new");
        let status = self
            .builder
            .build_call(
                begin_fn,
                &[
                    pool_ptr.into(),
                    out_val.into(),
                    out_conn.into(),
                    out_fn.into(),
                    out_env.into(),
                ],
                "pool.begin",
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();

        let idle_bb = self.context.append_basic_block(fn_val, "pool.idle");
        let mint_bb = self.context.append_basic_block(fn_val, "pool.mint");
        let atcap_bb = self.context.append_basic_block(fn_val, "pool.atcap");
        let closed_bb = self.context.append_basic_block(fn_val, "pool.closed");
        let merge_bb = self.context.append_basic_block(fn_val, "pool.merge");

        // switch status: 0 => idle, 1 => mint, 3 => closed, default(2) => at-cap.
        self.builder
            .build_switch(
                status,
                atcap_bb,
                &[
                    (i32_t.const_int(0, false), idle_bb),
                    (i32_t.const_int(1, false), mint_bb),
                    (i32_t.const_int(3, false), closed_bb),
                ],
            )
            .unwrap();

        // Helper: build PooledConnection { handle_i64, conn_id, val } → Ok(..).
        let build_ok = |cg: &mut Self,
                        conn_id: inkwell::values::IntValue<'ctx>,
                        val: BasicValueEnum<'ctx>|
         -> Result<BasicValueEnum<'ctx>, String> {
            let mut pc = pc_ty.const_zero();
            pc = cg
                .builder
                .build_insert_value(pc, handle_i64, 0, "pc.h")
                .unwrap()
                .into_struct_value();
            pc = cg
                .builder
                .build_insert_value(pc, conn_id, 1, "pc.c")
                .unwrap()
                .into_struct_value();
            pc = cg
                .builder
                .build_insert_value(pc, val, 2, "pc.v")
                .unwrap()
                .into_struct_value();
            cg.build_nonshared_enum_value("Result", "Ok", &[pc.into()])
        };

        // GOT_IDLE — the slot bytes are in out_val.
        self.builder.position_at_end(idle_bb);
        let idle_conn = self
            .builder
            .build_load(i64_t, out_conn, "pool.idle.conn")
            .unwrap()
            .into_int_value();
        let idle_val = self
            .builder
            .build_load(elem_ty, out_val, "pool.idle.val")
            .unwrap();
        let idle_ok = build_ok(self, idle_conn, idle_val)?;
        let idle_end = self.builder.get_insert_block().unwrap();
        self.builder.build_unconditional_branch(merge_bb).unwrap();

        // NEED_MINT — reconstruct the create_fn fat pointer and indirect-call it.
        self.builder.position_at_end(mint_bb);
        let mint_conn = self
            .builder
            .build_load(i64_t, out_conn, "pool.mint.conn")
            .unwrap()
            .into_int_value();
        let fn_i64 = self
            .builder
            .build_load(i64_t, out_fn, "pool.mint.fn")
            .unwrap()
            .into_int_value();
        let env_i64 = self
            .builder
            .build_load(i64_t, out_env, "pool.mint.env")
            .unwrap()
            .into_int_value();
        let fn_ptr = self
            .builder
            .build_int_to_ptr(fn_i64, ptr_ty, "pool.mint.fnp")
            .unwrap();
        let env_ptr = self
            .builder
            .build_int_to_ptr(env_i64, ptr_ty, "pool.mint.envp")
            .unwrap();
        // env-first ABI: T (ptr env). No user args (create_fn is Fn() -> T).
        let abi_ty = self.closure_abi_fn_type(&[], Some(&elem_te));
        let minted = self
            .builder
            .build_indirect_call(abi_ty, fn_ptr, &[env_ptr.into()], "pool.mint.call")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic();
        let mint_ok = build_ok(self, mint_conn, minted)?;
        let mint_end = self.builder.get_insert_block().unwrap();
        self.builder.build_unconditional_branch(merge_bb).unwrap();

        // AT_CAP → Err(PoolError.Timeout).
        self.builder.position_at_end(atcap_bb);
        let timeout = self.build_nonshared_enum_value("PoolError", "Timeout", &[])?;
        let atcap_err = self.build_nonshared_enum_value("Result", "Err", &[timeout])?;
        let atcap_end = self.builder.get_insert_block().unwrap();
        self.builder.build_unconditional_branch(merge_bb).unwrap();

        // CLOSED → Err(PoolError.PoolClosed).
        self.builder.position_at_end(closed_bb);
        let closed = self.build_nonshared_enum_value("PoolError", "PoolClosed", &[])?;
        let closed_err = self.build_nonshared_enum_value("Result", "Err", &[closed])?;
        let closed_end = self.builder.get_insert_block().unwrap();
        self.builder.build_unconditional_branch(merge_bb).unwrap();

        // Merge — phi the four Result aggregates.
        self.builder.position_at_end(merge_bb);
        let result_ty = idle_ok.get_type();
        let phi = self
            .builder
            .build_phi(result_ty, "pool.acquire.res")
            .unwrap();
        phi.add_incoming(&[
            (&idle_ok, idle_end),
            (&mint_ok, mint_end),
            (&atcap_err, atcap_end),
            (&closed_err, closed_end),
        ]);
        Ok(phi.as_basic_value())
    }

    /// `pool.release(conn) -> Unit`. Extracts `conn_id` (field 1) + `val` (field
    /// 2) from the `PooledConnection`, spills the value, and calls
    /// `karac_runtime_pool_release` (idempotent on `conn_id`). Returns Unit.
    fn compile_pool_release(
        &mut self,
        object: &Expr,
        args: &[CallArg],
        call_span: &crate::token::Span,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        // `release` returns Unit (no generic type at the call span), so recover
        // `T` from the `PooledConnection[T]` argument's recorded type instead.
        let arg_span = &args[0].value.span;
        let elem_te = self
            .pool_elem_te_at(arg_span)
            .or_else(|| self.pool_elem_te_at(call_span))
            .ok_or_else(|| {
                "Pool.release: could not recover element type T from the connection argument"
                    .to_string()
            })?;
        let elem_ty = self.llvm_type_for_type_expr(&elem_te);
        let fn_val = self.current_fn.unwrap();

        let pool_ptr = self.pool_handle_ptr(object)?;
        let conn = self.compile_expr(&args[0].value)?.into_struct_value();
        let conn_id = self
            .builder
            .build_extract_value(conn, 1, "pool.rel.conn")
            .unwrap()
            .into_int_value();
        let val = self
            .builder
            .build_extract_value(conn, 2, "pool.rel.val")
            .unwrap();
        let val_slot = self.create_entry_alloca(fn_val, "pool.rel.valslot", elem_ty);
        self.builder.build_store(val_slot, val).unwrap();

        let release_fn = self
            .module
            .get_function("karac_runtime_pool_release")
            .expect("karac_runtime_pool_release declared in Codegen::new");
        self.builder
            .build_call(
                release_fn,
                &[pool_ptr.into(), conn_id.into(), val_slot.into()],
                "pool.release",
            )
            .unwrap();
        // Unit return (the i64-zero unit value, matching Semaphore.release).
        Ok(self.context.i64_type().const_zero().into())
    }
}
