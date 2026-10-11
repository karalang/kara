//! Number and char methods beyond [`Cx::scalar_native`], as the
//! interpreter answers them (`src/mir/interp.rs`): char classes and case
//! mappings through Rust's own (`karac_mir_char`), bit counts, `pow`,
//! `parse`, float bit patterns and the libm functions; and the in-place
//! moves `Option.take` / `Option.replace` and `mem.replace`.

use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum, FloatValue, IntValue};
use inkwell::IntPredicate;

use super::{Cx, R};
use crate::mir::ty::{FloatTy, IntTy};
use crate::mir::{Operand, Place, Ty, TyKind};

/// The char methods `karac_mir_char` answers, in its order
/// (`runtime/src/mir.rs`, `CHAR_OPS`, which must match).
const CHAR_OPS: &[&str] = &[
    "is_alphabetic",
    "is_numeric",
    "is_alphanumeric",
    "is_uppercase",
    "is_lowercase",
    "is_whitespace",
    "is_ascii",
    "is_ascii_digit",
    "is_ascii_alphabetic",
    "is_ascii_alphanumeric",
    "is_ascii_uppercase",
    "is_ascii_lowercase",
    "is_ascii_punctuation",
    "is_ascii_whitespace",
    "is_ascii_hexdigit",
    "to_lowercase",
    "to_uppercase",
    "to_ascii_lowercase",
    "to_ascii_uppercase",
    "is_digit",
    "to_digit",
];

fn e<T: ToString>(x: T) -> String {
    x.to_string()
}

impl<'ctx> Cx<'ctx, '_> {
    /// `f16` / `bf16`, which are held as their bits and computed through
    /// `f32`.
    pub(super) fn narrow_float(&self, ty: Ty) -> Option<FloatTy> {
        match self.tys.kind(ty) {
            TyKind::Float(f @ (FloatTy::F16 | FloatTy::BF16)) => Some(f),
            _ => None,
        }
    }

    /// The `f32` the narrow float bits `v` stand for.
    pub(super) fn widen_bits(&self, v: IntValue<'ctx>, ft: FloatTy) -> R<FloatValue<'ctx>> {
        let f32t = self.llcx.f32_type();
        if ft == FloatTy::BF16 {
            let b = &self.builder;
            let w = b
                .build_int_z_extend(v, self.llcx.i32_type(), "")
                .map_err(e)?;
            let w = b
                .build_left_shift(w, self.llcx.i32_type().const_int(16, false), "")
                .map_err(e)?;
            return Ok(b.build_bit_cast(w, f32t, "").map_err(e)?.into_float_value());
        }
        let f = self.extern_fn(
            "karac_mir_f16_to",
            f32t.fn_type(&[self.llcx.i16_type().into()], false),
        );
        Ok(self
            .builder
            .build_call(f, &[v.into()], "")
            .map_err(e)?
            .try_as_basic_value()
            .unwrap_basic()
            .into_float_value())
    }

    /// The `f32` `x` rounded to the narrow float `ft`, as its bits.
    pub(super) fn narrow_bits(&self, x: FloatValue<'ctx>, ft: FloatTy) -> R<IntValue<'ctx>> {
        let f32t = self.llcx.f32_type();
        let x = self.builder.build_float_cast(x, f32t, "").map_err(e)?;
        let name = if ft == FloatTy::BF16 {
            "karac_mir_bf16_from"
        } else {
            "karac_mir_f16_from"
        };
        let f = self.extern_fn(name, self.llcx.i16_type().fn_type(&[f32t.into()], false));
        Ok(self
            .builder
            .build_call(f, &[x.into()], "")
            .map_err(e)?
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value())
    }

    /// The float value `v` of type `ty`, a narrow one widened to `f32`.
    pub(super) fn float_of(&self, v: BasicValueEnum<'ctx>, ty: Ty) -> R<FloatValue<'ctx>> {
        match self.narrow_float(ty) {
            Some(ft) => self.widen_bits(v.into_int_value(), ft),
            None => Ok(v.into_float_value()),
        }
    }

    /// Writes `Some(v)` at `dest` (an `Option` of an integer or `char`)
    /// when the `i64` `v` is non-negative, else `None`.
    pub(super) fn store_opt_int(&mut self, dest: &Place, v: IntValue<'ctx>) -> R<()> {
        let dty = self.place_type(dest)?;
        let d = self.place_ptr(dest)?;
        let (some, none) = self.option_variants(dty)?;
        self.store_tag(d, dty, none)?;
        let ok = self
            .builder
            .build_int_compare(IntPredicate::SGE, v, self.i64(0), "")
            .map_err(e)?;
        let pt = self
            .tys
            .field_ty(dty, Some(some), 0)
            .ok_or("an option without a payload")?;
        let q = self.offset(d, self.field_offset(dty, Some(some), 0)?)?;
        let it = self.llvm_ty(pt)?.into_int_type();
        self.if_then(ok, |cx| {
            cx.store_tag(d, dty, some)?;
            let n = cx
                .builder
                .build_int_truncate_or_bit_cast(v, it, "")
                .map_err(e)?;
            cx.store(q, n.into())
        })
    }

    /// The methods of this file, or `None` for one not lowered here.
    pub(super) fn num_native(
        &mut self,
        base: &str,
        method: &str,
        args: &[Operand],
        dest: &Place,
    ) -> R<Option<()>> {
        match (base, method) {
            ("mem", "replace") => return self.mem_replace(args, dest),
            ("Option", "take" | "replace") => return self.option_take(method, args, dest),
            ("char", "try_from") => return self.char_try_from(args, dest),
            (_, "parse" | "from_str_radix") => return self.parse_num(base, method, args, dest),
            _ => {}
        }
        let Some(first) = args.first() else {
            return Ok(None);
        };
        let t = self.operand_type(first)?;
        match self.tys.kind(t) {
            TyKind::Char => self.char_op(method, args, dest, false),
            TyKind::Int(it)
                if matches!(
                    method,
                    "is_ascii_digit"
                        | "is_ascii_alphabetic"
                        | "is_ascii_alphanumeric"
                        | "is_ascii_whitespace"
                        | "is_ascii_hexdigit"
                ) =>
            {
                let _ = it;
                self.char_op(method, args, dest, true)
            }
            TyKind::Int(it) => self.int_op(it, method, args, dest),
            TyKind::Float(ft) => self.float_op(ft, method, args, dest),
            _ => Ok(None),
        }
    }

    /// A char method through `karac_mir_char`; `byte` reads an integer
    /// receiver as a byte.
    fn char_op(
        &mut self,
        method: &str,
        args: &[Operand],
        dest: &Place,
        byte: bool,
    ) -> R<Option<()>> {
        let Some(k) = CHAR_OPS.iter().position(|m| *m == method) else {
            return Ok(None);
        };
        let radix = matches!(method, "is_digit" | "to_digit");
        if args.len() != 1 + usize::from(radix) {
            return Ok(None);
        }
        let ct = self.operand_type(&args[0])?;
        let c = self.operand_scalar(&args[0])?.into_int_value();
        let signed = matches!(self.tys.kind(ct), TyKind::Int(it) if it.signed());
        let c = self
            .builder
            .build_int_cast_sign_flag(c, self.llcx.i64_type(), signed, "")
            .map_err(e)?;
        let r = if radix {
            self.int_arg(&args[1])?
        } else {
            self.i64(0)
        };
        let op = self
            .llcx
            .i32_type()
            .const_int(k as u64 | if byte { 0x100 } else { 0 }, false);
        let v = self.rt_int("char", &[op.into(), c.into(), r.into()], 64)?;
        let dty = self.place_type(dest)?;
        match method {
            "to_digit" => self.store_opt_int(dest, v)?,
            m if m.starts_with("to_") => {
                let it = self.llvm_ty(dty)?.into_int_type();
                let c = self
                    .builder
                    .build_int_truncate_or_bit_cast(v, it, "")
                    .map_err(e)?;
                let d = self.place_ptr(dest)?;
                self.store(d, c.into())?;
            }
            _ => {
                let b = self
                    .builder
                    .build_int_compare(IntPredicate::NE, v, self.i64(0), "")
                    .map_err(e)?;
                self.store_bool(dest, b)?;
            }
        }
        Ok(Some(()))
    }

    /// `char.try_from(n)`: `Ok(c)` for a Unicode scalar, else `Err(n)`.
    fn char_try_from(&mut self, args: &[Operand], dest: &Place) -> R<Option<()>> {
        let [n] = args else {
            return Ok(None);
        };
        let nt = self.operand_type(n)?;
        let TyKind::Int(it) = self.tys.kind(nt) else {
            return Ok(None);
        };
        if it.bits() > 64 {
            return Ok(None);
        }
        let dty = self.place_type(dest)?;
        let ok_v = self.variant_of(dty, "Ok")?;
        let err_v = self.variant_of(dty, "Err")?;
        let v = self.operand_scalar(n)?.into_int_value();
        let w = self
            .builder
            .build_int_cast_sign_flag(v, self.llcx.i64_type(), it.signed(), "")
            .map_err(e)?;
        let b = &self.builder;
        let in_range = b
            .build_int_compare(IntPredicate::ULE, w, self.i64(0x10FFFF), "")
            .map_err(e)?;
        let lo = b
            .build_int_compare(IntPredicate::ULT, w, self.i64(0xD800), "")
            .map_err(e)?;
        let hi = b
            .build_int_compare(IntPredicate::UGT, w, self.i64(0xDFFF), "")
            .map_err(e)?;
        let not_sur = b.build_or(lo, hi, "").map_err(e)?;
        let ok = b.build_and(in_range, not_sur, "").map_err(e)?;
        let d = self.place_ptr(dest)?;
        let oq = self.offset(d, self.field_offset(dty, Some(ok_v), 0)?)?;
        let eq = self.offset(d, self.field_offset(dty, Some(err_v), 0)?)?;
        let et = self
            .tys
            .field_ty(dty, Some(err_v), 0)
            .ok_or("an Err without a payload")?;
        let eit = self.llvm_ty(et)?.into_int_type();
        self.store_tag(d, dty, err_v)?;
        let ev = self
            .builder
            .build_int_cast_sign_flag(v, eit, it.signed(), "")
            .map_err(e)?;
        self.store(eq, ev.into())?;
        self.if_then(ok, |cx| {
            cx.store_tag(d, dty, ok_v)?;
            let c = cx
                .builder
                .build_int_truncate(w, cx.llcx.i32_type(), "")
                .map_err(e)?;
            cx.store(oq, c.into())
        })?;
        Ok(Some(()))
    }

    /// `T.parse(s)` / `T.from_str_radix(s, r)` into an `Option[T]`, for an
    /// integer of at most 64 bits or an `f64`.
    fn parse_num(
        &mut self,
        base: &str,
        method: &str,
        args: &[Operand],
        dest: &Place,
    ) -> R<Option<()>> {
        let dty = self.place_type(dest)?;
        let Ok((some, none)) = self.option_variants(dty) else {
            return Ok(None);
        };
        let pt = self
            .tys
            .field_ty(dty, Some(some), 0)
            .ok_or("an option without a payload")?;
        let (s, n) = match (method, args) {
            ("parse", [s]) | ("from_str_radix", [s, _]) => self.str_parts(s)?,
            _ => return Ok(None),
        };
        let d = self.place_ptr(dest)?;
        let q = self.offset(d, self.field_offset(dty, Some(some), 0)?)?;
        let slot = self.entry_slot(8, "parsed")?;
        let got = match self.tys.kind(pt) {
            TyKind::Float(FloatTy::F64) if base == "f64" && method == "parse" => {
                self.rt_u8("parse_f64", &[s.into(), n.into(), slot.into()])?
            }
            TyKind::Int(it) if it.bits() <= 64 => {
                let radix = match args {
                    [_, r] => self.int_arg(r)?,
                    _ => self.i64(10),
                };
                let bits = self.llcx.i32_type().const_int(u64::from(it.bits()), false);
                let sg = self.llcx.i8_type().const_int(u64::from(it.signed()), false);
                self.rt_u8(
                    "parse_int",
                    &[
                        s.into(),
                        n.into(),
                        radix.into(),
                        bits.into(),
                        sg.into(),
                        slot.into(),
                    ],
                )?
            }
            _ => return Ok(None),
        };
        self.drop_moved(args)?;
        self.store_tag(d, dty, none)?;
        let ok = self
            .builder
            .build_int_compare(IntPredicate::NE, got, got.get_type().const_zero(), "")
            .map_err(e)?;
        self.if_then(ok, |cx| {
            cx.store_tag(d, dty, some)?;
            let v: BasicValueEnum = match cx.tys.kind(pt) {
                TyKind::Float(_) => cx
                    .builder
                    .build_load(cx.llcx.f64_type(), slot, "")
                    .map_err(e)?,
                _ => {
                    let w = cx.load_i64(slot)?;
                    let it = cx.llvm_ty(pt)?.into_int_type();
                    cx.builder
                        .build_int_truncate_or_bit_cast(w, it, "")
                        .map_err(e)?
                        .into()
                }
            };
            cx.store(q, v)
        })?;
        Ok(Some(()))
    }

    /// Stores the integer `v` at `dest`, cast to its integer type.
    pub(super) fn store_int_as(&mut self, dest: &Place, v: IntValue<'ctx>, signed: bool) -> R<()> {
        let dty = self.place_type(dest)?;
        let it = self.llvm_ty(dty)?.into_int_type();
        let v = self
            .builder
            .build_int_cast_sign_flag(v, it, signed, "")
            .map_err(e)?;
        let d = self.place_ptr(dest)?;
        self.store(d, v.into())
    }

    /// Stores the float `v` at `dest`, converted to its float type.
    fn store_float_as(&mut self, dest: &Place, v: FloatValue<'ctx>) -> R<()> {
        let dty = self.place_type(dest)?;
        if let Some(ft) = self.narrow_float(dty) {
            let h = self.narrow_bits(v, ft)?;
            let d = self.place_ptr(dest)?;
            return self.store(d, h.into());
        }
        let ft = self.llvm_ty(dty)?.into_float_type();
        let v = self.builder.build_float_cast(v, ft, "").map_err(e)?;
        let d = self.place_ptr(dest)?;
        self.store(d, v.into())
    }

    fn int_intrinsic(
        &self,
        name: &str,
        args: &[BasicMetadataValueEnum<'ctx>],
        ty: inkwell::types::IntType<'ctx>,
    ) -> R<IntValue<'ctx>> {
        let i = inkwell::intrinsics::Intrinsic::find(&format!("llvm.{name}"))
            .ok_or_else(|| format!("no intrinsic llvm.{name}"))?;
        let f = i
            .get_declaration(&self.module, &[ty.into()])
            .ok_or_else(|| format!("llvm.{name} has no declaration"))?;
        Ok(self
            .builder
            .build_call(f, args, "")
            .map_err(e)?
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value())
    }

    fn int_op(&mut self, it: IntTy, method: &str, args: &[Operand], dest: &Place) -> R<Option<()>> {
        if it.bits() > 64 {
            return Ok(None);
        }
        let x = self.operand_scalar(&args[0])?.into_int_value();
        let xt = x.get_type();
        let f = self.llcx.bool_type().const_zero();
        match (method, &args[1..]) {
            ("trailing_zeros" | "leading_zeros" | "count_ones" | "count_zeros", []) => {
                let v = match method {
                    "trailing_zeros" => self.int_intrinsic("cttz", &[x.into(), f.into()], xt)?,
                    "leading_zeros" => self.int_intrinsic("ctlz", &[x.into(), f.into()], xt)?,
                    "count_ones" => self.int_intrinsic("ctpop", &[x.into()], xt)?,
                    _ => {
                        let n = self.int_intrinsic("ctpop", &[x.into()], xt)?;
                        let w = xt.const_int(u64::from(it.bits()), false);
                        self.builder.build_int_sub(w, n, "").map_err(e)?
                    }
                };
                self.store_int_as(dest, v, false)?;
            }
            ("is_power_of_two", []) => {
                let b = &self.builder;
                let z = xt.const_zero();
                let pos = b
                    .build_int_compare(
                        if it.signed() {
                            IntPredicate::SGT
                        } else {
                            IntPredicate::NE
                        },
                        x,
                        z,
                        "",
                    )
                    .map_err(e)?;
                let m = b.build_int_sub(x, xt.const_int(1, false), "").map_err(e)?;
                let a = b.build_and(x, m, "").map_err(e)?;
                let single = b.build_int_compare(IntPredicate::EQ, a, z, "").map_err(e)?;
                let v = b.build_and(pos, single, "").map_err(e)?;
                self.store_bool(dest, v)?;
            }
            ("abs_diff", [y]) => {
                if self.operand_type(y)? != self.operand_type(&args[0])? {
                    return Ok(None);
                }
                let y = self.operand_scalar(y)?.into_int_value();
                let b = &self.builder;
                let p = if it.signed() {
                    IntPredicate::SGT
                } else {
                    IntPredicate::UGT
                };
                let gt = b.build_int_compare(p, x, y, "").map_err(e)?;
                let d1 = b.build_int_sub(x, y, "").map_err(e)?;
                let d2 = b.build_int_sub(y, x, "").map_err(e)?;
                let v = b.build_select(gt, d1, d2, "").map_err(e)?.into_int_value();
                self.store_int_as(dest, v, false)?;
            }
            ("pow", [y]) => {
                let a = self
                    .builder
                    .build_int_cast_sign_flag(x, self.llcx.i64_type(), it.signed(), "")
                    .map_err(e)?;
                let yt = self.operand_type(y)?;
                let ys = matches!(self.tys.kind(yt), TyKind::Int(t) if t.signed());
                let ev = self.operand_scalar(y)?.into_int_value();
                let ev = self
                    .builder
                    .build_int_cast_sign_flag(ev, self.llcx.i64_type(), ys, "")
                    .map_err(e)?;
                let slot = self.entry_slot(8, "pow")?;
                let bits = self.llcx.i32_type().const_int(u64::from(it.bits()), false);
                let sg = self.llcx.i8_type().const_int(u64::from(it.signed()), false);
                let r = self.rt_u8(
                    "ipow",
                    &[a.into(), ev.into(), bits.into(), sg.into(), slot.into()],
                )?;
                let ok = self.is_zero(r)?;
                self.abort_unless(ok, "integer overflow")?;
                let v = self.load_i64(slot)?;
                self.store_int_as(dest, v, it.signed())?;
            }
            ("clamp", [l, h]) => {
                let t = self.operand_type(&args[0])?;
                if self.operand_type(l)? != t || self.operand_type(h)? != t {
                    return Ok(None);
                }
                let l = self.operand_scalar(l)?.into_int_value();
                let h = self.operand_scalar(h)?.into_int_value();
                let (lp, gp) = if it.signed() {
                    (IntPredicate::SLT, IntPredicate::SGT)
                } else {
                    (IntPredicate::ULT, IntPredicate::UGT)
                };
                let b = &self.builder;
                let lt = b.build_int_compare(lp, x, l, "").map_err(e)?;
                let gt = b.build_int_compare(gp, x, h, "").map_err(e)?;
                let v = b.build_select(gt, h, x, "").map_err(e)?.into_int_value();
                let v = b.build_select(lt, l, v, "").map_err(e)?.into_int_value();
                self.store_int_as(dest, v, it.signed())?;
            }
            ("rem_euclid" | "div_euclid", [y]) => {
                if self.operand_type(y)? != self.operand_type(&args[0])? {
                    return Ok(None);
                }
                let y = self.operand_scalar(y)?.into_int_value();
                let z = xt.const_zero();
                let nz = self
                    .builder
                    .build_int_compare(IntPredicate::NE, y, z, "")
                    .map_err(e)?;
                self.abort_unless(nz, "division by zero")?;
                let b = &self.builder;
                let v = if it.signed() {
                    // `MIN % -1` is undefined in LLVM and 0 by the rule; a
                    // divisor of 1 gives that remainder.
                    let m1 = b
                        .build_int_compare(IntPredicate::EQ, y, xt.const_all_ones(), "")
                        .map_err(e)?;
                    let safe = b
                        .build_select(m1, xt.const_int(1, false), y, "")
                        .map_err(e)?
                        .into_int_value();
                    let r = b.build_int_signed_rem(x, safe, "").map_err(e)?;
                    let neg = b
                        .build_int_compare(IntPredicate::SLT, r, z, "")
                        .map_err(e)?;
                    let ypos = b
                        .build_int_compare(IntPredicate::SGT, y, z, "")
                        .map_err(e)?;
                    if method == "rem_euclid" {
                        let up = b.build_int_add(r, y, "").map_err(e)?;
                        let dn = b.build_int_sub(r, y, "").map_err(e)?;
                        let fixed = b.build_select(ypos, up, dn, "").map_err(e)?;
                        b.build_select(neg, fixed, r.into(), "")
                            .map_err(e)?
                            .into_int_value()
                    } else {
                        let min = xt.const_int(1u64 << (it.bits() - 1), false);
                        let at_min = b
                            .build_int_compare(IntPredicate::EQ, x, min, "")
                            .map_err(e)?;
                        let over = b.build_and(at_min, m1, "").map_err(e)?;
                        let fine = b.build_not(over, "").map_err(e)?;
                        self.abort_unless(fine, "integer overflow")?;
                        let b = &self.builder;
                        let safe2 = b
                            .build_select(over, xt.const_int(1, false), y, "")
                            .map_err(e)?
                            .into_int_value();
                        let q = b.build_int_signed_div(x, safe2, "").map_err(e)?;
                        let one = xt.const_int(1, false);
                        let qm = b.build_int_sub(q, one, "").map_err(e)?;
                        let qp = b.build_int_add(q, one, "").map_err(e)?;
                        let fixed = b.build_select(ypos, qm, qp, "").map_err(e)?;
                        b.build_select(neg, fixed, q.into(), "")
                            .map_err(e)?
                            .into_int_value()
                    }
                } else if method == "rem_euclid" {
                    b.build_int_unsigned_rem(x, y, "").map_err(e)?
                } else {
                    b.build_int_unsigned_div(x, y, "").map_err(e)?
                };
                self.store_int_as(dest, v, it.signed())?;
            }
            ("bits_as_f64", []) => {
                let w = self
                    .builder
                    .build_int_cast_sign_flag(x, self.llcx.i64_type(), it.signed(), "")
                    .map_err(e)?;
                let v = self
                    .builder
                    .build_bit_cast(w, self.llcx.f64_type(), "")
                    .map_err(e)?
                    .into_float_value();
                self.store_float_as(dest, v)?;
            }
            ("bits_as_f32", []) => {
                let w = self
                    .builder
                    .build_int_cast_sign_flag(x, self.llcx.i32_type(), it.signed(), "")
                    .map_err(e)?;
                let v = self
                    .builder
                    .build_bit_cast(w, self.llcx.f32_type(), "")
                    .map_err(e)?
                    .into_float_value();
                self.store_float_as(dest, v)?;
            }
            _ => return Ok(None),
        }
        Ok(Some(()))
    }

    /// A libm function of one float, at the float's width.
    fn libm(
        &self,
        name: &str,
        x: FloatValue<'ctx>,
        y: Option<FloatValue<'ctx>>,
    ) -> R<FloatValue<'ctx>> {
        let ft = x.get_type();
        let narrow = ft == self.llcx.f32_type();
        let name = if narrow {
            format!("{name}f")
        } else {
            name.to_string()
        };
        let params: Vec<_> = std::iter::repeat_n(ft.into(), 1 + usize::from(y.is_some())).collect();
        let f = self.extern_fn(&name, ft.fn_type(&params, false));
        let mut vals: Vec<BasicMetadataValueEnum> = vec![x.into()];
        if let Some(y) = y {
            vals.push(y.into());
        }
        Ok(self
            .builder
            .build_call(f, &vals, "")
            .map_err(e)?
            .try_as_basic_value()
            .unwrap_basic()
            .into_float_value())
    }

    fn float_op(
        &mut self,
        ft: FloatTy,
        method: &str,
        args: &[Operand],
        dest: &Place,
    ) -> R<Option<()>> {
        let _ = ft;
        let xt = self.operand_type(&args[0])?;
        let x = self.operand_scalar(&args[0])?;
        let x = self.float_of(x, xt)?;
        let f64t = self.llcx.f64_type();
        let f32t = self.llcx.f32_type();
        let b = &self.builder;
        let v: FloatValue = match (method, &args[1..]) {
            ("to_bits", []) => {
                let w = if x.get_type() == f64t {
                    x
                } else {
                    b.build_float_ext(x, f64t, "").map_err(e)?
                };
                let bits = b
                    .build_bit_cast(w, self.llcx.i64_type(), "")
                    .map_err(e)?
                    .into_int_value();
                self.store_int_as(dest, bits, false)?;
                return Ok(Some(()));
            }
            ("to_bits32", []) => {
                let w = if x.get_type() == f32t {
                    x
                } else {
                    b.build_float_trunc(x, f32t, "").map_err(e)?
                };
                let bits = b
                    .build_bit_cast(w, self.llcx.i32_type(), "")
                    .map_err(e)?
                    .into_int_value();
                self.store_int_as(dest, bits, false)?;
                return Ok(Some(()));
            }
            (
                "sin" | "cos" | "tan" | "exp" | "ln" | "log2" | "log10" | "asin" | "acos" | "atan"
                | "sinh" | "cosh" | "tanh" | "exp2" | "exp_m1" | "ln_1p" | "trunc",
                [],
            ) => {
                // Computed at the destination's width, as the interpreter's
                // `float_unary`.
                let x = self.at_dest_width(x, dest)?;
                let name = match method {
                    "ln" => "log",
                    "exp_m1" => "expm1",
                    "ln_1p" => "log1p",
                    m => m,
                };
                self.libm(name, x, None)?
            }
            ("pow" | "atan2" | "hypot" | "copysign", [y]) => {
                let yt = self.operand_type(y)?;
                if !matches!(self.tys.kind(yt), TyKind::Float(_)) {
                    return Ok(None);
                }
                let y = self.operand_scalar(y)?;
                let y = self.float_of(y, yt)?;
                let x = self.at_dest_width(x, dest)?;
                let y = self.at_dest_width(y, dest)?;
                let name = if method == "pow" { "pow" } else { method };
                self.libm(name, x, Some(y))?
            }
            // A narrow float's (a wide one's are `scalar_native`'s).
            ("abs" | "sqrt" | "floor" | "ceil" | "round", [])
                if self.narrow_float(xt).is_some() =>
            {
                let x = self.at_dest_width(x, dest)?;
                let name = if method == "abs" { "fabs" } else { method };
                self.float_intrinsic(name, &[x])?
            }
            ("max" | "min", [y]) if self.narrow_float(xt).is_some() => {
                if self.operand_type(y)? != xt {
                    return Ok(None);
                }
                let y = self.operand_scalar(y)?;
                let y = self.float_of(y, xt)?;
                let name = if method == "max" { "maxnum" } else { "minnum" };
                self.float_intrinsic(name, &[x, y])?
            }
            ("signum", []) => {
                // NaN stays NaN; anything else is 1 with its sign.
                let x = self.at_dest_width(x, dest)?;
                let one = x.get_type().const_float(1.0);
                let s = self.float_intrinsic("copysign", &[one, x])?;
                let nan = self
                    .builder
                    .build_float_compare(inkwell::FloatPredicate::UNO, x, x, "")
                    .map_err(e)?;
                self.builder
                    .build_select(nan, x, s, "")
                    .map_err(e)?
                    .into_float_value()
            }
            ("clamp", [l, h]) => {
                if self.operand_type(l)? != xt || self.operand_type(h)? != xt {
                    return Ok(None);
                }
                let l = self.operand_scalar(l)?;
                let l = self.float_of(l, xt)?;
                let h = self.operand_scalar(h)?;
                let h = self.float_of(h, xt)?;
                let b = &self.builder;
                let lt = b
                    .build_float_compare(inkwell::FloatPredicate::OLT, x, l, "")
                    .map_err(e)?;
                let gt = b
                    .build_float_compare(inkwell::FloatPredicate::OGT, x, h, "")
                    .map_err(e)?;
                let v = b.build_select(gt, h, x, "").map_err(e)?.into_float_value();
                b.build_select(lt, l, v, "").map_err(e)?.into_float_value()
            }
            ("rem_euclid" | "div_euclid", [y]) => {
                if self.operand_type(y)? != xt {
                    return Ok(None);
                }
                let y = self.operand_scalar(y)?;
                let y = self.float_of(y, xt)?;
                let x = self.at_dest_width(x, dest)?;
                let y = self.at_dest_width(y, dest)?;
                let t = x.get_type();
                let b = &self.builder;
                let r = b.build_float_rem(x, y, "").map_err(e)?;
                let neg = b
                    .build_float_compare(inkwell::FloatPredicate::OLT, r, t.const_zero(), "")
                    .map_err(e)?;
                if method == "rem_euclid" {
                    let ay = self.float_intrinsic("fabs", &[y])?;
                    let b = &self.builder;
                    let up = b.build_float_add(r, ay, "").map_err(e)?;
                    b.build_select(neg, up, r, "")
                        .map_err(e)?
                        .into_float_value()
                } else {
                    let q = b.build_float_div(x, y, "").map_err(e)?;
                    let q = self.libm("trunc", q, None)?;
                    let b = &self.builder;
                    let ypos = b
                        .build_float_compare(inkwell::FloatPredicate::OGT, y, t.const_zero(), "")
                        .map_err(e)?;
                    let one = t.const_float(1.0);
                    let qm = b.build_float_sub(q, one, "").map_err(e)?;
                    let qp = b.build_float_add(q, one, "").map_err(e)?;
                    let fixed = b.build_select(ypos, qm, qp, "").map_err(e)?;
                    b.build_select(neg, fixed, q.into(), "")
                        .map_err(e)?
                        .into_float_value()
                }
            }
            ("recip" | "fract" | "to_degrees" | "to_radians", []) => {
                let x = self.at_dest_width(x, dest)?;
                let t = x.get_type();
                let narrow = t == f32t;
                let b = &self.builder;
                match method {
                    "recip" => b.build_float_div(t.const_float(1.0), x, "").map_err(e)?,
                    "fract" => {
                        let tr = self.libm("trunc", x, None)?;
                        self.builder.build_float_sub(x, tr, "").map_err(e)?
                    }
                    "to_degrees" => {
                        let k = if narrow {
                            f64::from(57.295_78_f32)
                        } else {
                            180.0f64 / std::f64::consts::PI
                        };
                        b.build_float_mul(x, t.const_float(k), "").map_err(e)?
                    }
                    _ => {
                        let k = if narrow {
                            f64::from(std::f32::consts::PI / 180.0f32)
                        } else {
                            std::f64::consts::PI / 180.0
                        };
                        b.build_float_mul(x, t.const_float(k), "").map_err(e)?
                    }
                }
            }
            _ => return Ok(None),
        };
        self.store_float_as(dest, v)?;
        Ok(Some(()))
    }

    /// `x` converted to the width of `dest`'s float type.
    fn at_dest_width(&mut self, x: FloatValue<'ctx>, dest: &Place) -> R<FloatValue<'ctx>> {
        let dty = self.place_type(dest)?;
        let narrow = !matches!(self.tys.kind(dty), TyKind::Float(FloatTy::F64));
        let t = if narrow {
            self.llcx.f32_type()
        } else {
            self.llcx.f64_type()
        };
        self.builder.build_float_cast(x, t, "").map_err(e)
    }

    /// `Option.take(ref o)`: the value moves out and `None` is left;
    /// `Option.replace(ref o, v)` leaves `Some(v)`.
    fn option_take(&mut self, method: &str, args: &[Operand], dest: &Place) -> R<Option<()>> {
        let Some(recv) = args.first() else {
            return Ok(None);
        };
        let rt = self.operand_type(recv)?;
        let (TyKind::Ref(ot) | TyKind::MutRef(ot)) = self.tys.kind(rt) else {
            return Ok(None);
        };
        let at = self.ref_arg(recv)?;
        let d = self.place_ptr(dest)?;
        self.copy(d, at, ot)?;
        let (some, none) = self.option_variants(ot)?;
        match (method, &args[1..]) {
            ("take", []) => self.store_tag(at, ot, none)?,
            ("replace", [v]) => {
                let pt = self
                    .tys
                    .field_ty(ot, Some(some), 0)
                    .ok_or("an option without a payload")?;
                let src = self.operand_ptr(v)?;
                self.store_tag(at, ot, some)?;
                let q = self.offset(at, self.field_offset(ot, Some(some), 0)?)?;
                self.copy(q, src, pt)?;
            }
            _ => return Ok(None),
        }
        Ok(Some(()))
    }

    /// `mem.replace(ref a, v)`: the old value out, `v` in.
    fn mem_replace(&mut self, args: &[Operand], dest: &Place) -> R<Option<()>> {
        let [a, v] = args else {
            return Ok(None);
        };
        let rt = self.operand_type(a)?;
        let (TyKind::Ref(t) | TyKind::MutRef(t)) = self.tys.kind(rt) else {
            return Ok(None);
        };
        let at = self.ref_arg(a)?;
        let src = self.operand_ptr(v)?;
        let d = self.place_ptr(dest)?;
        self.copy(d, at, t)?;
        self.copy(at, src, t)?;
        Ok(Some(()))
    }
}
