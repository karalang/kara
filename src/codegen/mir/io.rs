//! The I/O natives: `File` and `Clock`, each a call into the runtime's
//! blocking implementation through its `karac_mir_*` entry
//! (`runtime/src/mir_io.rs`). Their reference semantics are the
//! interpreter's `file_method` and its `Clock.now` arm (`src/mir/interp.rs`).
//!
//! A fallible entry reports as `karac_mir_fs` does: a negative return is
//! success, with the payload in an `i64` out slot; `0..=6` is the `IoError`
//! variant, in the order [`IO_ERRORS`] lists, and `Other` comes with its
//! message as a `String` in a second out slot.
use inkwell::values::{BasicMetadataValueEnum, IntValue, PointerValue};
use inkwell::IntPredicate;

use super::{Cx, R};
use crate::mir::ty::IntrinsicTy;
use crate::mir::{Operand, Place, TyKind};

fn e<T: ToString>(x: T) -> String {
    x.to_string()
}

/// The `IoError` variants by the runtime's code for them.
const IO_ERRORS: [&str; 7] = [
    "NotFound",
    "PermissionDenied",
    "AlreadyExists",
    "UnexpectedEof",
    "InvalidUtf8",
    "Interrupted",
    "Other",
];

impl<'ctx> Cx<'ctx, '_> {
    /// Lowers the I/O native `base.method`, or `None` when it is not one.
    pub(super) fn io_native(
        &mut self,
        base: &str,
        method: &str,
        args: &[Operand],
        dest: &Place,
    ) -> R<Option<()>> {
        match (base, method) {
            ("File", _) => self.file_native(method, args, dest),
            ("Clock", "now") => {
                let i64t = self.llcx.i64_type();
                let f = self.extern_fn("karac_runtime_clock_now", i64t.fn_type(&[], false));
                let v = self
                    .builder
                    .build_call(f, &[], "")
                    .map_err(e)?
                    .try_as_basic_value()
                    .unwrap_basic();
                let d = self.place_ptr(dest)?;
                self.store(d, v)?;
                Ok(Some(()))
            }
            _ => Ok(None),
        }
    }

    /// `File.open` / `create` / `append` and the handle methods, through
    /// `karac_mir_file(op, handle, ptr, len, n, value, msg)`.
    fn file_native(&mut self, method: &str, args: &[Operand], dest: &Place) -> R<Option<()>> {
        let op: u64 = match method {
            "open" => 0,
            "create" => 1,
            "append" => 2,
            "read" => 3,
            "write" => 4,
            "flush" => 5,
            "sync_all" => 6,
            "sync_data" => 7,
            "seek" => 8,
            _ => return Ok(None),
        };
        let null = self.ptr_ty().const_null();
        let (handle, ptr, len, n) = if op <= 2 {
            let (p, n) = self.str_parts(&args[0])?;
            (null, p, n, self.i64(0))
        } else {
            let (at, _) = self.behind_refs(&args[0])?;
            let handle = self.load_ptr(at)?;
            match method {
                "read" | "write" => {
                    let (p, n) = self.view_of(&args[1])?;
                    (handle, p, n, self.i64(0))
                }
                "seek" => {
                    let wt = self.operand_type(&args[1])?;
                    // The runtime's `whence` byte is the variant's index.
                    for (k, name) in ["Start", "Current", "End"].iter().enumerate() {
                        if self.variant_of(wt, name)? != k as u32 {
                            return Err(format!("SeekFrom.{name} is not variant {k}"));
                        }
                    }
                    let w = self.operand_ptr(&args[1])?;
                    let tag = self.load_tag(w, wt)?;
                    let i64t = self.llcx.i64_type();
                    let whence = self
                        .builder
                        .build_int_z_extend_or_bit_cast(tag, i64t, "")
                        .map_err(e)?;
                    let off = self.operand_ptr(&args[2])?;
                    let off = self.load_i64(off)?;
                    (handle, null, off, whence)
                }
                _ => (handle, null, self.i64(0), self.i64(0)),
            }
        };
        let i32t = self.llcx.i32_type();
        self.io_call(
            "file",
            &[
                i32t.const_int(op, false).into(),
                handle.into(),
                ptr.into(),
                len.into(),
                n.into(),
            ],
            args,
            dest,
        )?;
        Ok(Some(()))
    }

    /// Calls the fallible `karac_mir_{name}(args.., value, msg)` and writes
    /// its `Result` at `dest`: `Ok` of the `i64` it left in `value` (none
    /// for a `Unit` payload), or `Err` of the `IoError` its code names.
    /// Drops the arguments the native was handed by move.
    fn io_call(
        &mut self,
        name: &str,
        call_args: &[BasicMetadataValueEnum<'ctx>],
        args: &[Operand],
        dest: &Place,
    ) -> R<()> {
        let value = self.entry_slot(8, "io.value")?;
        let msg = self.temp(
            self.tys.intern(TyKind::Intrinsic(IntrinsicTy::String)),
            "io.msg",
        )?;
        let mut all = call_args.to_vec();
        all.push(value.into());
        all.push(msg.into());
        let code = self.rt_int(name, &all, 32)?;
        self.drop_moved(args)?;
        self.store_io_result(dest, code, value, msg)
    }

    /// Writes the `Result` a `karac_mir_*` I/O entry reported at `dest`.
    fn store_io_result(
        &mut self,
        dest: &Place,
        code: IntValue<'ctx>,
        value: PointerValue<'ctx>,
        msg: PointerValue<'ctx>,
    ) -> R<()> {
        let dty = self.place_type(dest)?;
        let d = self.place_ptr(dest)?;
        let (ok, er) = (self.variant_of(dty, "Ok")?, self.variant_of(dty, "Err")?);
        let okt = self
            .tys
            .field_ty(dty, Some(ok), 0)
            .ok_or("Result.Ok has no payload")?;
        let et = self
            .tys
            .field_ty(dty, Some(er), 0)
            .ok_or("Result.Err has no payload")?;
        let ok_size = self.layout(okt)?.size;
        if ok_size != 0 && ok_size != 8 {
            return Err(format!("an I/O result of `{}`", self.tys.display(okt)));
        }
        let ok_off = self.field_offset(dty, Some(ok), 0)?;
        let ep = self.offset(d, self.field_offset(dty, Some(er), 0)?)?;
        let i32t = self.llcx.i32_type();
        let done = self
            .builder
            .build_int_compare(IntPredicate::SLT, code, i32t.const_zero(), "")
            .map_err(e)?;
        self.store_tag(d, dty, er)?;
        self.if_then(done, |cx| {
            cx.store_tag(d, dty, ok)?;
            if ok_size == 8 {
                let v = cx.load_i64(value)?;
                cx.store(cx.offset(d, ok_off)?, v.into())?;
            }
            Ok(())
        })?;
        for (k, name) in IO_ERRORS.iter().enumerate() {
            let v = self.variant_of(et, name)?;
            let is = self
                .builder
                .build_int_compare(IntPredicate::EQ, code, i32t.const_int(k as u64, false), "")
                .map_err(e)?;
            let other = *name == "Other";
            let off = if other {
                self.field_offset(et, Some(v), 0)?
            } else {
                0
            };
            self.if_then(is, |cx| {
                cx.store_tag(ep, et, v)?;
                if other {
                    let s = cx.tys.intern(TyKind::Intrinsic(IntrinsicTy::String));
                    cx.copy(cx.offset(ep, off)?, msg, s)?;
                }
                Ok(())
            })?;
        }
        Ok(())
    }
}
