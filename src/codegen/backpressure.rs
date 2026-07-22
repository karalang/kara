//! Backpressure-primitive AOT codegen — `Semaphore` / `RateLimiter` method
//! lowering (phase-8). `new` / `new_token_bucket` are lowered in
//! `assoc_call.rs`; the runtime FFI lives in `runtime/src/semaphore.rs` and
//! `runtime/src/rate_limiter.rs`. Same opaque-handle shape as
//! `BoundedChannel`: the receiver is a `{ handle_id: i64 }` struct whose field
//! 0 `inttoptr`s to the runtime `*mut Karac{Semaphore,RateLimiter}`.
//!
//! Dispatch is keyed on the receiver's STATIC type name (`type_name_of_expr`,
//! span-collision-immune — the same gate `std.process` uses), so a user method
//! named `acquire` / `release` / `try_acquire` on an unrelated type never
//! routes here. The v1 semantics mirror the interpreter byte-for-byte:
//! `acquire` fails closed immediately on an exhausted semaphore (collapsed
//! non-parking), and `try_acquire` is the lazily-refilled token bucket.

use inkwell::values::{BasicValueEnum, PointerValue};
use inkwell::{AddressSpace, IntPredicate};

use crate::ast::{CallArg, Expr};

impl<'ctx> super::Codegen<'ctx> {
    /// Try to lower a `Semaphore` / `RateLimiter` builtin method call. Returns
    /// `Ok(None)` (fall through to the rest of dispatch) unless the receiver's
    /// static type + method + arity match.
    pub(super) fn try_compile_backpressure_method(
        &mut self,
        object: &Expr,
        method: &str,
        args: &[CallArg],
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        if !matches!(method, "acquire" | "release" | "try_acquire") {
            return Ok(None);
        }
        let Some(type_name) = self.type_name_of_expr(object) else {
            return Ok(None);
        };
        match (type_name.as_str(), method, args.len()) {
            ("Semaphore", "acquire", 1) => self.compile_semaphore_acquire(object, args).map(Some),
            ("Semaphore", "release", 0) => self.compile_semaphore_release(object).map(Some),
            ("RateLimiter", "try_acquire", 1) => self
                .compile_rate_limiter_try_acquire(object, args)
                .map(Some),
            _ => Ok(None),
        }
    }

    /// Recover the runtime handle pointer from a `{ handle_id: i64 }` receiver.
    fn backpressure_handle_ptr(&mut self, object: &Expr) -> Result<PointerValue<'ctx>, String> {
        let recv = self.compile_expr(object)?.into_struct_value();
        let handle = self
            .builder
            .build_extract_value(recv, 0, "bp.handle")
            .unwrap()
            .into_int_value();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        Ok(self
            .builder
            .build_int_to_ptr(handle, ptr_ty, "bp.ptr")
            .unwrap())
    }

    /// `sem.acquire(timeout)` → `Result[Unit, SemaphoreError]`. Calls
    /// `karac_runtime_semaphore_acquire` (1 = permit taken, 0 = exhausted) and
    /// builds `Ok(())` vs `Err(SemaphoreError.Timeout)` from the discriminant.
    fn compile_semaphore_acquire(
        &mut self,
        object: &Expr,
        args: &[CallArg],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let fn_val = self.current_fn.unwrap();
        let sem = self.backpressure_handle_ptr(object)?;
        let timeout = self.compile_expr(&args[0].value)?.into_int_value();
        let acquire_fn = self
            .module
            .get_function("karac_runtime_semaphore_acquire")
            .expect("karac_runtime_semaphore_acquire declared in Codegen::new");
        let got_i8 = self
            .builder
            .build_call(acquire_fn, &[sem.into(), timeout.into()], "sem.acquire.got")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();
        let zero_i8 = self.context.i8_type().const_zero();
        let got = self
            .builder
            .build_int_compare(IntPredicate::NE, got_i8, zero_i8, "sem.acquire.ok")
            .unwrap();

        let ok_bb = self.context.append_basic_block(fn_val, "sem.acq.ok.bb");
        let to_bb = self
            .context
            .append_basic_block(fn_val, "sem.acq.timeout.bb");
        let merge_bb = self.context.append_basic_block(fn_val, "sem.acq.merge");
        self.builder
            .build_conditional_branch(got, ok_bb, to_bb)
            .unwrap();

        // Permit taken → `Ok(())` (Unit payload is the i64-zero unit value).
        self.builder.position_at_end(ok_bb);
        let unit_val = self.context.i64_type().const_zero().into();
        let ok_val = self.build_nonshared_enum_value("Result", "Ok", &[unit_val])?;
        let ok_end = self.builder.get_insert_block().unwrap();
        self.builder.build_unconditional_branch(merge_bb).unwrap();

        // Exhausted → `Err(SemaphoreError.Timeout)`.
        self.builder.position_at_end(to_bb);
        let to_inner = self.build_nonshared_enum_value("SemaphoreError", "Timeout", &[])?;
        let err_val = self.build_nonshared_enum_value("Result", "Err", &[to_inner])?;
        let to_end = self.builder.get_insert_block().unwrap();
        self.builder.build_unconditional_branch(merge_bb).unwrap();

        self.builder.position_at_end(merge_bb);
        let phi = self
            .builder
            .build_phi(ok_val.get_type(), "sem.acquire.result")
            .unwrap();
        phi.add_incoming(&[(&ok_val, ok_end), (&err_val, to_end)]);
        Ok(phi.as_basic_value())
    }

    /// `sem.release()` → `Unit`. Calls `karac_runtime_semaphore_release`.
    fn compile_semaphore_release(&mut self, object: &Expr) -> Result<BasicValueEnum<'ctx>, String> {
        let sem = self.backpressure_handle_ptr(object)?;
        let release_fn = self
            .module
            .get_function("karac_runtime_semaphore_release")
            .expect("karac_runtime_semaphore_release declared in Codegen::new");
        self.builder
            .build_call(release_fn, &[sem.into()], "sem.release")
            .unwrap();
        // Unit return (the i64-zero unit value).
        Ok(self.context.i64_type().const_zero().into())
    }

    /// `rl.try_acquire(key)` → `bool`. Extracts the key String's `(ptr, len)`,
    /// calls `karac_runtime_rate_limiter_try_acquire` (1 = token taken), and
    /// truncates the u8 discriminant to an `i1` bool.
    fn compile_rate_limiter_try_acquire(
        &mut self,
        object: &Expr,
        args: &[CallArg],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let rl = self.backpressure_handle_ptr(object)?;
        let key_val = self.compile_expr(&args[0].value)?;
        let (key_ptr, key_len) = self.str_data_len(key_val.into_struct_value());
        let try_fn = self
            .module
            .get_function("karac_runtime_rate_limiter_try_acquire")
            .expect("karac_runtime_rate_limiter_try_acquire declared in Codegen::new");
        let got_i8 = self
            .builder
            .build_call(
                try_fn,
                &[rl.into(), key_ptr.into(), key_len.into()],
                "rl.try.got",
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();
        // u8 (0/1) → Kāra `bool` (i1).
        let bool_val = self
            .builder
            .build_int_compare(
                IntPredicate::NE,
                got_i8,
                self.context.i8_type().const_zero(),
                "rl.try.bool",
            )
            .unwrap();
        Ok(bool_val.into())
    }
}
