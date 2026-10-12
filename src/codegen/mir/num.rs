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
            ("mem", "swap") => return self.mem_swap(args),
            (t, "try_from") if crate::numeric_conv::is_int_target(t) => {
                return self.int_try_from(t, args, dest)
            }
            (t, "from") if crate::numeric_conv::is_int_target(t) && args.len() == 1 => {
                // A widening conversion (`i64.from(x)`), as the source's sign
                // extends it.
                let at = self.operand_type(&args[0])?;
                let signed = matches!(self.tys.kind(at), TyKind::Int(it) if it.signed());
                if !matches!(
                    self.tys.kind(at),
                    TyKind::Int(_) | TyKind::Bool | TyKind::Char
                ) {
                    return Ok(None);
                }
                let v = self.operand_scalar(&args[0])?.into_int_value();
                self.store_int_as(dest, v, signed)?;
                return Ok(Some(()));
            }
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

    /// `mem.swap(a, b)`: the two places trade values.
    fn mem_swap(&mut self, args: &[Operand]) -> R<Option<()>> {
        let [a, b] = args else {
            return Ok(None);
        };
        let at = self.operand_type(a)?;
        let (TyKind::Ref(t) | TyKind::MutRef(t)) = self.tys.kind(at) else {
            return Ok(None);
        };
        let pa = self.operand_ptr(a)?;
        let pa = self.load_ptr(pa)?;
        let pb = self.operand_ptr(b)?;
        let pb = self.load_ptr(pb)?;
        let tmp = self.temp(t, "swap")?;
        self.copy(tmp, pa, t)?;
        self.copy(pa, pb, t)?;
        self.copy(pb, tmp, t)?;
        Ok(Some(()))
    }

    /// `T.try_from(n)` for an integer target `t`: `Ok(n)` when `n` fits,
    /// else `Err("out of range for {t}")`.
    fn int_try_from(&mut self, t: &str, args: &[Operand], dest: &Place) -> R<Option<()>> {
        let [n] = args else {
            return Ok(None);
        };
        let nt = self.operand_type(n)?;
        let src_signed = match self.tys.kind(nt) {
            TyKind::Int(it) if it.bits() <= 64 => it.signed(),
            TyKind::Bool => false,
            _ => return Ok(None),
        };
        let Some(&(_, bits, signed)) = crate::numeric_conv::INT_TARGETS
            .iter()
            .find(|(name, _, _)| *name == t)
        else {
            return Ok(None);
        };
        if bits > 64 {
            return Ok(None);
        }
        let dty = self.place_type(dest)?;
        let ok_v = self.variant_of(dty, "Ok")?;
        let err_v = self.variant_of(dty, "Err")?;
        let v = self.operand_scalar(n)?.into_int_value();
        let i64t = self.llcx.i64_type();
        let w = self
            .builder
            .build_int_cast_sign_flag(v, i64t, src_signed, "")
            .map_err(e)?;
        // The source as a 65-bit quantity: its i64 bits and whether it is
        // negative (a signed source below zero).
        let b = &self.builder;
        let neg = if src_signed {
            b.build_int_compare(IntPredicate::SLT, w, self.i64(0), "")
                .map_err(e)?
        } else {
            self.llcx.bool_type().const_zero()
        };
        let fits = match (signed, bits) {
            (true, 64) => {
                // Fits unless an unsigned source is above i64::MAX.
                if src_signed {
                    self.llcx.bool_type().const_int(1, false)
                } else {
                    b.build_int_compare(IntPredicate::SGE, w, self.i64(0), "")
                        .map_err(e)?
                }
            }
            (false, 64) => b.build_not(neg, "").map_err(e)?,
            (true, k) => {
                let lo = -(1i64 << (k - 1));
                let hi = (1i64 << (k - 1)) - 1;
                let ge = b
                    .build_int_compare(IntPredicate::SGE, w, i64t.const_int(lo as u64, true), "")
                    .map_err(e)?;
                let le = b
                    .build_int_compare(IntPredicate::SLE, w, self.i64(hi as u64), "")
                    .map_err(e)?;
                let both = b.build_and(ge, le, "").map_err(e)?;
                if src_signed {
                    both
                } else {
                    // An unsigned source above i64::MAX reads as negative.
                    let le_u = b
                        .build_int_compare(IntPredicate::ULE, w, self.i64(hi as u64), "")
                        .map_err(e)?;
                    le_u
                }
            }
            (false, k) => {
                let hi = (1u64 << k) - 1;
                let le = b
                    .build_int_compare(IntPredicate::ULE, w, self.i64(hi), "")
                    .map_err(e)?;
                let nn = b.build_not(neg, "").map_err(e)?;
                b.build_and(le, nn, "").map_err(e)?
            }
        };
        let d = self.place_ptr(dest)?;
        let oq = self.offset(d, self.field_offset(dty, Some(ok_v), 0)?)?;
        let eq = self.offset(d, self.field_offset(dty, Some(err_v), 0)?)?;
        let ot = self
            .tys
            .field_ty(dty, Some(ok_v), 0)
            .ok_or("an Ok without a payload")?;
        let oit = self.llvm_ty(ot)?.into_int_type();
        let msg = format!("out of range for {t}");
        let f = self.cur_fn()?;
        let (yes, no, done) = (
            self.llcx.append_basic_block(f, "try_from.ok"),
            self.llcx.append_basic_block(f, "try_from.err"),
            self.llcx.append_basic_block(f, "try_from.done"),
        );
        self.builder
            .build_conditional_branch(fits, yes, no)
            .map_err(e)?;
        self.builder.position_at_end(yes);
        self.store_tag(d, dty, ok_v)?;
        let c = self
            .builder
            .build_int_truncate_or_bit_cast(w, oit, "")
            .map_err(e)?;
        self.store(oq, c.into())?;
        self.builder.build_unconditional_branch(done).map_err(e)?;
        self.builder.position_at_end(no);
        self.store_tag(d, dty, err_v)?;
        let (mp, mn) = self.str_const(&msg)?;
        self.rt("string_from", &[eq.into(), mp.into(), mn.into()])?;
        self.builder.build_unconditional_branch(done).map_err(e)?;
        self.builder.position_at_end(done);
        Ok(Some(()))
    }

    /// `x.trunc_to_i32()` and its `saturating_` / `wrapping_` / `checked_`
    /// siblings, as the interpreter's `numeric_conv` has them.
    fn float_to_int(&mut self, method: &str, x: FloatValue<'ctx>, dest: &Place) -> R<Option<()>> {
        let Some((family, _, bits, signed)) = crate::numeric_conv::parse_float_to_int(method)
        else {
            return Ok(None);
        };
        if bits > 64 {
            return Ok(None);
        }
        use crate::numeric_conv::FloatToIntFamily as F;
        let fam = match family {
            F::Saturating => 0,
            F::Wrapping => 1,
            F::Checked => 2,
            F::Trunc => 3,
        };
        let f64t = self.llcx.f64_type();
        let x = if x.get_type() == f64t {
            x
        } else {
            self.builder.build_float_ext(x, f64t, "").map_err(e)?
        };
        let slot = self.entry_slot(8, "f2i")?;
        let i32t = self.llcx.i32_type();
        let r = self.rt_u8(
            "f2i",
            &[
                x.into(),
                i32t.const_int(fam, false).into(),
                i32t.const_int(u64::from(bits), false).into(),
                self.llcx
                    .i8_type()
                    .const_int(u64::from(signed), false)
                    .into(),
                slot.into(),
            ],
        )?;
        let v = self.load_i64(slot)?;
        match family {
            F::Checked => {
                let dty = self.place_type(dest)?;
                let d = self.place_ptr(dest)?;
                let (some, none) = self.option_variants(dty)?;
                self.store_tag(d, dty, none)?;
                let pt = self
                    .tys
                    .field_ty(dty, Some(some), 0)
                    .ok_or("an option without a payload")?;
                let q = self.offset(d, self.field_offset(dty, Some(some), 0)?)?;
                let it = self.llvm_ty(pt)?.into_int_type();
                let ok = self.is_zero(r)?;
                self.if_then(ok, |cx| {
                    cx.store_tag(d, dty, some)?;
                    let n = cx
                        .builder
                        .build_int_truncate_or_bit_cast(v, it, "")
                        .map_err(e)?;
                    cx.store(q, n.into())
                })?;
            }
            _ => {
                let ok = self.is_zero(r)?;
                self.abort_unless(ok, "explicit panic")?;
                self.store_int_as(dest, v, signed)?;
            }
        }
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

    /// The integer argument `y` as a peer of the receiver `x`: the same
    /// type, or an unsuffixed literal (typed `i64`) cast to it.
    fn int_peer(&mut self, x: IntValue<'ctx>, xty: Ty, y: &Operand) -> R<Option<IntValue<'ctx>>> {
        let yt = self.operand_type(y)?;
        let v = self.operand_scalar(y)?.into_int_value();
        if yt == xty {
            return Ok(Some(v));
        }
        match (self.tys.kind(yt), y) {
            (TyKind::Int(from), Operand::Const(_)) => Ok(Some(
                self.builder
                    .build_int_cast_sign_flag(v, x.get_type(), from.signed(), "")
                    .map_err(e)?,
            )),
            _ => Ok(None),
        }
    }

    /// `x op y` and whether it overflowed, through
    /// `llvm.{s,u}{add,sub,mul}.with.overflow`.
    fn with_overflow(
        &mut self,
        op: &str,
        signed: bool,
        x: IntValue<'ctx>,
        y: IntValue<'ctx>,
    ) -> R<(IntValue<'ctx>, IntValue<'ctx>)> {
        let name = format!("llvm.{}{op}.with.overflow", if signed { "s" } else { "u" });
        let i = inkwell::intrinsics::Intrinsic::find(&name)
            .ok_or_else(|| format!("no intrinsic {name}"))?;
        let f = i
            .get_declaration(&self.module, &[x.get_type().into()])
            .ok_or_else(|| format!("{name} has no declaration"))?;
        let r = self
            .builder
            .build_call(f, &[x.into(), y.into()], "")
            .map_err(e)?
            .try_as_basic_value()
            .unwrap_basic()
            .into_struct_value();
        let v = self
            .builder
            .build_extract_value(r, 0, "")
            .map_err(e)?
            .into_int_value();
        let o = self
            .builder
            .build_extract_value(r, 1, "")
            .map_err(e)?
            .into_int_value();
        Ok((v, o))
    }

    /// `checked_`, `saturating_` and `overflowing_` `add` / `sub` / `mul`,
    /// at any width; `None` for another method.
    fn int_overflow_op(
        &mut self,
        it: IntTy,
        method: &str,
        args: &[Operand],
        dest: &Place,
    ) -> R<Option<()>> {
        let Some((family, op)) = method.split_once('_') else {
            return Ok(None);
        };
        let ([a, y], "checked" | "saturating" | "overflowing", "add" | "sub" | "mul") =
            (args, family, op)
        else {
            return Ok(None);
        };
        let xty = self.operand_type(a)?;
        let x = self.operand_scalar(a)?.into_int_value();
        let Some(y) = self.int_peer(x, xty, y)? else {
            return Ok(None);
        };
        let signed = it.signed();
        let (v, o) = self.with_overflow(op, signed, x, y)?;
        let dty = self.place_type(dest)?;
        let d = self.place_ptr(dest)?;
        match family {
            "checked" => {
                let (some, none) = self.option_variants(dty)?;
                let q = self.offset(d, self.field_offset(dty, Some(some), 0)?)?;
                self.store(q, v.into())?;
                let i32t = self.llcx.i32_type();
                let k = self
                    .builder
                    .build_select(
                        o,
                        i32t.const_int(u64::from(none), false),
                        i32t.const_int(u64::from(some), false),
                        "",
                    )
                    .map_err(e)?
                    .into_int_value();
                self.store_tag_dyn(d, dty, k)?;
            }
            "overflowing" => {
                self.store(d, v.into())?;
                let q = self.offset(d, self.field_offset(dty, None, 1)?)?;
                let b = self
                    .builder
                    .build_int_z_extend(o, self.llcx.i8_type(), "")
                    .map_err(e)?;
                self.store(q, b.into())?;
            }
            _ => {
                // The bound the true result passed: below zero for an
                // unsigned `sub`, else by the signs for a signed one.
                let xt = x.get_type();
                let bits = it.bits();
                let (min, max) = if signed {
                    let min = xt
                        .const_int(1, false)
                        .const_shl(xt.const_int(u64::from(bits - 1), false));
                    (min, min.const_not())
                } else {
                    (xt.const_zero(), xt.const_all_ones())
                };
                let b = &self.builder;
                let high = if !signed {
                    xt.get_context()
                        .bool_type()
                        .const_int(u64::from(op != "sub"), false)
                } else {
                    let z = xt.const_zero();
                    let xn = b
                        .build_int_compare(IntPredicate::SLT, x, z, "")
                        .map_err(e)?;
                    let yn = b
                        .build_int_compare(IntPredicate::SLT, y, z, "")
                        .map_err(e)?;
                    match op {
                        // Overflow past MAX when both are non-negative.
                        "add" => b.build_not(xn, "").map_err(e)?,
                        // `x - y` passes MAX when x >= 0 (and y < 0).
                        "sub" => b.build_not(xn, "").map_err(e)?,
                        // A product passes MAX when the signs agree.
                        _ => {
                            let differ = b.build_xor(xn, yn, "").map_err(e)?;
                            b.build_not(differ, "").map_err(e)?
                        }
                    }
                };
                let bound = b
                    .build_select(high, max, min, "")
                    .map_err(e)?
                    .into_int_value();
                let r = b.build_select(o, bound, v, "").map_err(e)?.into_int_value();
                self.store(d, r.into())?;
            }
        }
        Ok(Some(()))
    }

    fn int_op(&mut self, it: IntTy, method: &str, args: &[Operand], dest: &Place) -> R<Option<()>> {
        if self.int_overflow_op(it, method, args, dest)?.is_some() {
            return Ok(Some(()));
        }
        if method == "abs_diff" && args.len() == 2 {
            let xty = self.operand_type(&args[0])?;
            let x = self.operand_scalar(&args[0])?.into_int_value();
            let Some(y) = self.int_peer(x, xty, &args[1])? else {
                return Ok(None);
            };
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
            return Ok(Some(()));
        }
        if it.bits() > 64 {
            return Ok(None);
        }
        let x = self.operand_scalar(&args[0])?.into_int_value();
        let xt = x.get_type();
        let f = self.llcx.bool_type().const_zero();
        match (method, &args[1..]) {
            ("to_f64" | "to_f32", []) => {
                let dty = self.place_type(dest)?;
                let ft = self.llvm_ty(dty)?.into_float_type();
                let v = if it.signed() {
                    self.builder.build_signed_int_to_float(x, ft, "")
                } else {
                    self.builder.build_unsigned_int_to_float(x, ft, "")
                }
                .map_err(e)?;
                let d = self.place_ptr(dest)?;
                self.store(d, v.into())?;
            }
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
                let (Some(l), Some(h)) = (self.int_peer(x, t, l)?, self.int_peer(x, t, h)?) else {
                    return Ok(None);
                };
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
            ("to_ne_bytes" | "to_le_bytes" | "to_be_bytes", []) => {
                // The integer's bytes, little-endian in memory; `to_be`
                // swaps them first.
                let v = if method == "to_be_bytes" && it.bits() > 8 {
                    self.int_intrinsic("bswap", &[x.into()], xt)?
                } else {
                    x
                };
                let d = self.place_ptr(dest)?;
                self.store(d, v.into())?;
            }
            ("rem_euclid" | "div_euclid", [y]) => {
                let t = self.operand_type(&args[0])?;
                let Some(y) = self.int_peer(x, t, y)? else {
                    return Ok(None);
                };
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
        let call = self.builder.build_call(f, &vals, "").map_err(e)?;
        // Not a builtin: LLVM would fold a constant argument with its own
        // correctly rounded math, where libm (and the interpreter, which
        // calls it) can differ by an ulp.
        let nobuiltin = self.llcx.create_enum_attribute(
            inkwell::attributes::Attribute::get_named_enum_kind_id("nobuiltin"),
            0,
        );
        call.add_attribute(inkwell::attributes::AttributeLoc::Function, nobuiltin);
        Ok(call.try_as_basic_value().unwrap_basic().into_float_value())
    }

    /// The float argument `y` as a peer of the receiver `x` (of type
    /// `xt`): the same type, or a literal (`f64` or an unsuffixed integer)
    /// converted to `x`'s width.
    fn float_peer(
        &mut self,
        x: FloatValue<'ctx>,
        xt: Ty,
        y: &Operand,
    ) -> R<Option<FloatValue<'ctx>>> {
        let yt = self.operand_type(y)?;
        let v = self.operand_scalar(y)?;
        if yt == xt {
            return Ok(Some(self.float_of(v, xt)?));
        }
        let to = x.get_type();
        Ok(match (self.tys.kind(yt), y) {
            (TyKind::Float(_), Operand::Const(_)) => {
                let v = self.float_of(v, yt)?;
                Some(self.builder.build_float_cast(v, to, "").map_err(e)?)
            }
            (TyKind::Int(it), Operand::Const(_)) => Some(
                if it.signed() {
                    self.builder
                        .build_signed_int_to_float(v.into_int_value(), to, "")
                } else {
                    self.builder
                        .build_unsigned_int_to_float(v.into_int_value(), to, "")
                }
                .map_err(e)?,
            ),
            _ => None,
        })
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
        if args.len() == 1 {
            if let Some(()) = self.float_to_int(method, x, dest)? {
                return Ok(Some(()));
            }
        }
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
                let Some(y) = self.float_peer(x, xt, y)? else {
                    return Ok(None);
                };
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
                let (Some(l), Some(h)) = (self.float_peer(x, xt, l)?, self.float_peer(x, xt, h)?)
                else {
                    return Ok(None);
                };
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
                let Some(y) = self.float_peer(x, xt, y)? else {
                    return Ok(None);
                };
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
