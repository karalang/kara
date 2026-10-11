//! `Vector[T, N]`'s constructors and methods (design.md § Portable SIMD),
//! lane by lane with the scalar operators, as the interpreter's
//! `vector_method` runs them. A lane vector is laid out as `Array[T, N]`.

use inkwell::values::{BasicValueEnum, FloatValue, IntValue, PointerValue};
use inkwell::IntPredicate;

use super::{Cx, R};
use crate::mir::ty::FloatTy;
use crate::mir::{BinOp, Operand, Place, Ty, TyKind};

fn e<T: ToString>(x: T) -> String {
    x.to_string()
}

impl<'ctx> Cx<'ctx, '_> {
    /// The methods of `Vector`, or `None` for one not lowered here.
    pub(super) fn vector_native(
        &mut self,
        method: &str,
        args: &[Operand],
        dest: &Place,
    ) -> R<Option<()>> {
        let dty = self.place_type(dest)?;
        if matches!(
            method,
            "splat" | "from_array" | "from_slice" | "load_masked" | "gather" | "cast_from"
        ) {
            let Some((et, n)) = self.vector_of(dty) else {
                return Ok(None);
            };
            return self.vector_make(method, args, dest, et, n);
        }
        let Some(recv) = args.first() else {
            return Ok(None);
        };
        let (vp, vt) = self.behind_refs(recv)?;
        let Some((et, n)) = self.vector_of(vt) else {
            return Ok(None);
        };
        let es = self.layout(et)?.size;
        let lane = |cx: &Self, p: PointerValue<'ctx>, i: u64, size: u64| cx.offset(p, i * size);
        let d = self.place_ptr(dest)?;
        match (method, &args[1..]) {
            (
                "sqrt" | "exp" | "ln" | "tanh" | "sigmoid" | "floor" | "ceil" | "round" | "trunc",
                [],
            ) => {
                if !matches!(self.tys.kind(et), TyKind::Float(_)) {
                    return Ok(None);
                }
                let f64t = self.llcx.f64_type();
                for i in 0..n {
                    let x = self.load(lane(self, vp, i, es)?, et)?;
                    let x = self.float_of(x, et)?;
                    let x = self.builder.build_float_cast(x, f64t, "").map_err(e)?;
                    let r = match method {
                        "sqrt" | "floor" | "ceil" | "round" | "trunc" => {
                            self.float_intrinsic(method, &[x])?
                        }
                        "exp" => self.libm_f64("exp", x)?,
                        "ln" => self.libm_f64("log", x)?,
                        "tanh" => self.libm_f64("tanh", x)?,
                        _ => {
                            let nx = self.builder.build_float_neg(x, "").map_err(e)?;
                            let ex = self.libm_f64("exp", nx)?;
                            let one = f64t.const_float(1.0);
                            let den = self.builder.build_float_add(one, ex, "").map_err(e)?;
                            self.builder.build_float_div(one, den, "").map_err(e)?
                        }
                    };
                    let v = self.float_as(r, et)?;
                    self.store(lane(self, d, i, es)?, v)?;
                }
            }
            ("to_bits", []) => {
                let Some((dt, _)) = self.vector_of(dty) else {
                    return Ok(None);
                };
                let ds = self.layout(dt)?.size;
                let dit = self.llvm_ty(dt)?.into_int_type();
                for i in 0..n {
                    let x = self.load(lane(self, vp, i, es)?, et)?;
                    let x = self.float_of(x, et)?;
                    let bits = if matches!(self.tys.kind(et), TyKind::Float(FloatTy::F32)) {
                        self.builder
                            .build_bit_cast(x, self.llcx.i32_type(), "")
                            .map_err(e)?
                            .into_int_value()
                    } else {
                        let w = self
                            .builder
                            .build_float_cast(x, self.llcx.f64_type(), "")
                            .map_err(e)?;
                        self.builder
                            .build_bit_cast(w, self.llcx.i64_type(), "")
                            .map_err(e)?
                            .into_int_value()
                    };
                    let bits = self
                        .builder
                        .build_int_cast_sign_flag(bits, dit, false, "")
                        .map_err(e)?;
                    self.store(lane(self, d, i, ds)?, bits.into())?;
                }
            }
            ("bits_as_f32" | "bits_as_f64", []) => {
                let Some((dt, _)) = self.vector_of(dty) else {
                    return Ok(None);
                };
                let ds = self.layout(dt)?.size;
                let (it, ft): (_, inkwell::types::FloatType) = if method == "bits_as_f32" {
                    (self.llcx.i32_type(), self.llcx.f32_type())
                } else {
                    (self.llcx.i64_type(), self.llcx.f64_type())
                };
                for i in 0..n {
                    let x = self.load(lane(self, vp, i, es)?, et)?.into_int_value();
                    let x = self
                        .builder
                        .build_int_truncate_or_bit_cast(x, it, "")
                        .map_err(e)?;
                    let f = self
                        .builder
                        .build_bit_cast(x, ft, "")
                        .map_err(e)?
                        .into_float_value();
                    let v = self.float_as(f, dt)?;
                    self.store(lane(self, d, i, ds)?, v)?;
                }
            }
            ("reduce_sum" | "reduce_product" | "reduce_and" | "reduce_or" | "reduce_xor", []) => {
                let op = match method {
                    "reduce_sum" => BinOp::Add,
                    "reduce_product" => BinOp::Mul,
                    "reduce_and" => BinOp::BitAnd,
                    "reduce_or" => BinOp::BitOr,
                    _ => BinOp::BitXor,
                };
                let mut acc = self.load(vp, et)?;
                for i in 1..n {
                    let x = self.load(lane(self, vp, i, es)?, et)?;
                    acc = self.lane_op(op, et, acc, x)?;
                }
                self.store(d, acc)?;
            }
            ("reduce_min" | "reduce_max", []) => {
                let want = if method == "reduce_min" {
                    BinOp::Lt
                } else {
                    BinOp::Gt
                };
                let mut acc = self.load(vp, et)?;
                for i in 1..n {
                    let x = self.load(lane(self, vp, i, es)?, et)?;
                    let keep = self.binop(want, et, acc, x)?.into_int_value();
                    let keep = self
                        .builder
                        .build_int_compare(IntPredicate::NE, keep, keep.get_type().const_zero(), "")
                        .map_err(e)?;
                    acc = self.builder.build_select(keep, acc, x, "").map_err(e)?;
                }
                self.store(d, acc)?;
            }
            ("dot", [b]) => {
                let (bp, _) = self.behind_refs(b)?;
                let mut acc: Option<BasicValueEnum> = None;
                for i in 0..n {
                    let x = self.load(lane(self, vp, i, es)?, et)?;
                    let y = self.load(lane(self, bp, i, es)?, et)?;
                    let p = self.lane_op(BinOp::Mul, et, x, y)?;
                    acc = Some(match acc {
                        None => p,
                        Some(a) => self.lane_op(BinOp::Add, et, a, p)?,
                    });
                }
                if let Some(a) = acc {
                    self.store(d, a)?;
                }
            }
            ("cross", [b]) if n == 3 => {
                let (bp, _) = self.behind_refs(b)?;
                let mut out = Vec::with_capacity(3);
                for (p, q, r, t) in [(1, 2, 2, 1), (2, 0, 0, 2), (0, 1, 1, 0)] {
                    let ap = self.load(lane(self, vp, p, es)?, et)?;
                    let bq = self.load(lane(self, bp, q, es)?, et)?;
                    let ar = self.load(lane(self, vp, r, es)?, et)?;
                    let bt = self.load(lane(self, bp, t, es)?, et)?;
                    let pq = self.lane_op(BinOp::Mul, et, ap, bq)?;
                    let rt = self.lane_op(BinOp::Mul, et, ar, bt)?;
                    out.push(self.lane_op(BinOp::Sub, et, pq, rt)?);
                }
                for (i, v) in out.into_iter().enumerate() {
                    self.store(lane(self, d, i as u64, es)?, v)?;
                }
            }
            ("select", [a, b]) => {
                // The receiver is the mask.
                let (ap, at) = self.behind_refs(a)?;
                let (bp, _) = self.behind_refs(b)?;
                let Some((xt, _)) = self.vector_of(at) else {
                    return Ok(None);
                };
                let xs = self.layout(xt)?.size;
                let tmp = self.temp(dty, "selected")?;
                for i in 0..n {
                    let m = self.load(lane(self, vp, i, es)?, et)?.into_int_value();
                    let m = self
                        .builder
                        .build_int_compare(IntPredicate::NE, m, m.get_type().const_zero(), "")
                        .map_err(e)?;
                    let x = self.load(lane(self, ap, i, xs)?, xt)?;
                    let y = self.load(lane(self, bp, i, xs)?, xt)?;
                    let v = self.builder.build_select(m, x, y, "").map_err(e)?;
                    self.store(lane(self, tmp, i, xs)?, v)?;
                }
                self.copy(d, tmp, dty)?;
            }
            ("reverse", []) => {
                let tmp = self.temp(vt, "reversed")?;
                for i in 0..n {
                    let x = self.load(lane(self, vp, n - 1 - i, es)?, et)?;
                    self.store(lane(self, tmp, i, es)?, x)?;
                }
                self.copy(d, tmp, vt)?;
            }
            ("rotate_lanes_left" | "rotate_lanes_right", [k]) => {
                if n == 0 {
                    self.copy(d, vp, vt)?;
                    return Ok(Some(()));
                }
                // k mod n, by lane: a dynamic count picks each source lane.
                let k = self.int_arg(k)?;
                let nn = self.i64(n);
                let r = self.builder.build_int_signed_rem(k, nn, "").map_err(e)?;
                let neg = self
                    .builder
                    .build_int_compare(IntPredicate::SLT, r, self.i64(0), "")
                    .map_err(e)?;
                let up = self.builder.build_int_add(r, nn, "").map_err(e)?;
                let k = self
                    .builder
                    .build_select(neg, up, r, "")
                    .map_err(e)?
                    .into_int_value();
                let tmp = self.temp(vt, "rotated")?;
                for i in 0..n {
                    let src = if method == "rotate_lanes_left" {
                        self.builder.build_int_add(self.i64(i), k, "").map_err(e)?
                    } else {
                        self.builder
                            .build_int_sub(self.i64(i + n), k, "")
                            .map_err(e)?
                    };
                    let src = self
                        .builder
                        .build_int_unsigned_rem(src, nn, "")
                        .map_err(e)?;
                    let at = self.elem_ptr(vp, src, es)?;
                    self.copy(lane(self, tmp, i, es)?, at, et)?;
                }
                self.copy(d, tmp, vt)?;
            }
            ("replace", [i, x]) => {
                let i = self.int_arg(i)?;
                let ok = self
                    .builder
                    .build_int_compare(IntPredicate::ULT, i, self.i64(n), "")
                    .map_err(e)?;
                self.abort_unless(ok, "explicit panic")?;
                let tmp = self.temp(vt, "replaced")?;
                self.copy(tmp, vp, vt)?;
                let at = self.elem_ptr(tmp, i, es)?;
                self.store_lane(at, x, et)?;
                self.copy(d, tmp, vt)?;
            }
            ("shuffle", [idx]) => {
                let (ip, it) = self.behind_refs(idx)?;
                let TyKind::Array(ie, m) = self.tys.kind(it) else {
                    return Ok(None);
                };
                let is = self.layout(ie)?.size;
                let tmp = self.temp(dty, "shuffled")?;
                for j in 0..m {
                    let k = self.load(lane(self, ip, j, is)?, ie)?.into_int_value();
                    let k = self.lane_index(k, ie)?;
                    let at = self.checked_lane(vp, k, n, es)?;
                    self.copy(lane(self, tmp, j, es)?, at, et)?;
                }
                self.copy(d, tmp, dty)?;
            }
            ("store_masked" | "scatter", [s, sel]) => {
                let (sp, sn, _) = self.view_parts(s)?;
                let (selp, selt) = self.behind_refs(sel)?;
                let Some((selt, _)) = self.vector_of(selt) else {
                    return Ok(None);
                };
                let ss = self.layout(selt)?.size;
                for i in 0..n {
                    let at = if method == "store_masked" {
                        let m = self.load(lane(self, selp, i, ss)?, selt)?.into_int_value();
                        let on = self
                            .builder
                            .build_int_compare(IntPredicate::NE, m, m.get_type().const_zero(), "")
                            .map_err(e)?;
                        let x = lane(self, vp, i, es)?;
                        self.if_then(on, |cx| {
                            let ok = cx
                                .builder
                                .build_int_compare(IntPredicate::ULT, cx.i64(i), sn, "")
                                .map_err(e)?;
                            cx.abort_unless(ok, "explicit panic")?;
                            let at = cx.elem_ptr(sp, cx.i64(i), es)?;
                            cx.copy(at, x, et)
                        })?;
                        continue;
                    } else {
                        let k = self.load(lane(self, selp, i, ss)?, selt)?.into_int_value();
                        let k = self.lane_index(k, selt)?;
                        let ok = self
                            .builder
                            .build_int_compare(IntPredicate::ULT, k, sn, "")
                            .map_err(e)?;
                        self.abort_unless(ok, "explicit panic")?;
                        self.elem_ptr(sp, k, es)?
                    };
                    self.copy(at, lane(self, vp, i, es)?, et)?;
                }
            }
            _ => return Ok(None),
        }
        Ok(Some(()))
    }

    /// The constructors: the destination is the vector.
    fn vector_make(
        &mut self,
        method: &str,
        args: &[Operand],
        dest: &Place,
        et: Ty,
        n: u64,
    ) -> R<Option<()>> {
        let dty = self.place_type(dest)?;
        let es = self.layout(et)?.size;
        let d = self.place_ptr(dest)?;
        let lane = |cx: &Self, i: u64| cx.offset(d, i * es);
        match (method, args) {
            ("splat", [x]) => {
                let tmp = self.temp(et, "lane")?;
                self.store_lane(tmp, x, et)?;
                for i in 0..n {
                    self.copy(lane(self, i)?, tmp, et)?;
                }
            }
            ("from_array", [a]) => {
                let (p, at) = self.behind_refs(a)?;
                match self.tys.kind(at) {
                    TyKind::Array(se, _) if se != et => {
                        // Literal lanes typed wider than the vector's.
                        let ss = self.layout(se)?.size;
                        for i in 0..n {
                            let x = self.load(self.offset(p, i * ss)?, se)?;
                            let v = self.lane_cast(x, se, et)?;
                            self.store(lane(self, i)?, v)?;
                        }
                    }
                    _ => self.copy(d, p, dty)?,
                }
            }
            ("from_slice", [s]) => {
                let (p, len, _) = self.view_parts(s)?;
                let ok = self
                    .builder
                    .build_int_compare(IntPredicate::EQ, len, self.i64(n), "")
                    .map_err(e)?;
                self.abort_unless(ok, "explicit panic")?;
                self.copy(d, p, dty)?;
            }
            ("load_masked", [s, mask]) => {
                let (p, len, _) = self.view_parts(s)?;
                let (mp, mt) = self.behind_refs(mask)?;
                let Some((mt, _)) = self.vector_of(mt) else {
                    return Ok(None);
                };
                let ms = self.layout(mt)?.size;
                let tmp = self.temp(dty, "loaded")?;
                self.builder
                    .build_memset(tmp, 1, self.llcx.i8_type().const_zero(), self.i64(n * es))
                    .map_err(e)?;
                for i in 0..n {
                    let m = self.load(self.offset(mp, i * ms)?, mt)?.into_int_value();
                    let on = self
                        .builder
                        .build_int_compare(IntPredicate::NE, m, m.get_type().const_zero(), "")
                        .map_err(e)?;
                    let dl = self.offset(tmp, i * es)?;
                    self.if_then(on, |cx| {
                        let ok = cx
                            .builder
                            .build_int_compare(IntPredicate::ULT, cx.i64(i), len, "")
                            .map_err(e)?;
                        cx.abort_unless(ok, "explicit panic")?;
                        let at = cx.elem_ptr(p, cx.i64(i), es)?;
                        cx.copy(dl, at, et)
                    })?;
                }
                self.copy(d, tmp, dty)?;
            }
            ("gather", [s, idx]) => {
                let (p, len, _) = self.view_parts(s)?;
                let (ip, it) = self.behind_refs(idx)?;
                let Some((ie, _)) = self.vector_of(it) else {
                    return Ok(None);
                };
                let is = self.layout(ie)?.size;
                let tmp = self.temp(dty, "gathered")?;
                for i in 0..n {
                    let k = self.load(self.offset(ip, i * is)?, ie)?.into_int_value();
                    let k = self.lane_index(k, ie)?;
                    let ok = self
                        .builder
                        .build_int_compare(IntPredicate::ULT, k, len, "")
                        .map_err(e)?;
                    self.abort_unless(ok, "explicit panic")?;
                    let at = self.elem_ptr(p, k, es)?;
                    self.copy(self.offset(tmp, i * es)?, at, et)?;
                }
                self.copy(d, tmp, dty)?;
            }
            ("cast_from", [v]) => {
                let (sp, st) = self.behind_refs(v)?;
                let Some((se, _)) = self.vector_of(st) else {
                    return Ok(None);
                };
                let ss = self.layout(se)?.size;
                let tmp = self.temp(dty, "cast")?;
                for i in 0..n {
                    let x = self.load(self.offset(sp, i * ss)?, se)?;
                    let v = self.lane_cast(x, se, et)?;
                    self.store(self.offset(tmp, i * es)?, v)?;
                }
                self.copy(d, tmp, dty)?;
            }
            _ => return Ok(None),
        }
        Ok(Some(()))
    }

    /// Writes the operand `x` at `at` as a lane of type `et`, converting
    /// a literal typed otherwise.
    fn store_lane(&mut self, at: PointerValue<'ctx>, x: &Operand, et: Ty) -> R<()> {
        let xt = self.operand_type(x)?;
        if xt == et {
            return self.store_operand(at, x);
        }
        let v = self.operand_scalar(x)?;
        let v = self.lane_cast(v, xt, et)?;
        self.store(at, v)
    }

    /// One lane converted as `cast_from` converts it.
    fn lane_cast(&mut self, x: BasicValueEnum<'ctx>, from: Ty, to: Ty) -> R<BasicValueEnum<'ctx>> {
        let f64t = self.llcx.f64_type();
        match (self.tys.kind(from), self.tys.kind(to)) {
            (TyKind::Int(it), TyKind::Float(_)) => {
                let x = x.into_int_value();
                let f = if it.signed() {
                    self.builder.build_signed_int_to_float(x, f64t, "")
                } else {
                    self.builder.build_unsigned_int_to_float(x, f64t, "")
                }
                .map_err(e)?;
                self.float_as(f, to)
            }
            (TyKind::Float(_), TyKind::Float(_)) => {
                let f = self.float_of(x, from)?;
                let f = self.builder.build_float_cast(f, f64t, "").map_err(e)?;
                self.float_as(f, to)
            }
            (TyKind::Float(_), TyKind::Int(_)) => {
                // `f as i64`, saturating, then wrapped to the lane width.
                let f = self.float_of(x, from)?;
                let f = self.builder.build_float_cast(f, f64t, "").map_err(e)?;
                let i64t = self.llcx.i64_type();
                let sat = inkwell::intrinsics::Intrinsic::find("llvm.fptosi.sat")
                    .ok_or("no fptosi.sat")?
                    .get_declaration(&self.module, &[i64t.into(), f64t.into()])
                    .ok_or("no fptosi.sat declaration")?;
                let w = self
                    .builder
                    .build_call(sat, &[f.into()], "")
                    .map_err(e)?
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_int_value();
                let it = self.llvm_ty(to)?.into_int_type();
                Ok(self
                    .builder
                    .build_int_truncate_or_bit_cast(w, it, "")
                    .map_err(e)?
                    .into())
            }
            (TyKind::Int(fi), TyKind::Int(_)) => {
                let it = self.llvm_ty(to)?.into_int_type();
                Ok(self
                    .builder
                    .build_int_cast_sign_flag(x.into_int_value(), it, fi.signed(), "")
                    .map_err(e)?
                    .into())
            }
            _ => Ok(x),
        }
    }

    /// An `f64` (or `f32`) result rounded to the lane float type `t`, as
    /// that type's value.
    fn float_as(&self, f: FloatValue<'ctx>, t: Ty) -> R<BasicValueEnum<'ctx>> {
        if let Some(ft) = self.narrow_float(t) {
            // Through `f32`, as `FloatTy::round` rounds.
            return Ok(self.narrow_bits(f, ft)?.into());
        }
        let ft = self.llvm_ty(t)?.into_float_type();
        Ok(self.builder.build_float_cast(f, ft, "").map_err(e)?.into())
    }

    /// `op` on two lanes: an integer one that overflows aborts, as the
    /// interpreter's `fold` does.
    fn lane_op(
        &mut self,
        op: BinOp,
        et: Ty,
        x: BasicValueEnum<'ctx>,
        y: BasicValueEnum<'ctx>,
    ) -> R<BasicValueEnum<'ctx>> {
        if matches!(self.tys.kind(et), TyKind::Int(_))
            && matches!(op, BinOp::Add | BinOp::Sub | BinOp::Mul)
        {
            let (v, o) = self.checked(op, et, x.into_int_value(), y.into_int_value())?;
            let ok = self.builder.build_not(o, "").map_err(e)?;
            self.abort_unless(ok, "integer overflow")?;
            return Ok(v.into());
        }
        self.binop(op, et, x, y)
    }

    /// An integer lane as an `i64` index (a negative one out of range).
    fn lane_index(&self, k: IntValue<'ctx>, t: Ty) -> R<IntValue<'ctx>> {
        let signed = matches!(self.tys.kind(t), TyKind::Int(it) if it.signed());
        self.builder
            .build_int_cast_sign_flag(k, self.llcx.i64_type(), signed, "")
            .map_err(e)
    }

    /// Lane `k` of the `n` at `p`, aborting out of range (`n == 0`: no
    /// check).
    fn checked_lane(
        &mut self,
        p: PointerValue<'ctx>,
        k: IntValue<'ctx>,
        n: u64,
        size: u64,
    ) -> R<PointerValue<'ctx>> {
        if n > 0 {
            let ok = self
                .builder
                .build_int_compare(IntPredicate::ULT, k, self.i64(n), "")
                .map_err(e)?;
            self.abort_unless(ok, "explicit panic")?;
        }
        self.elem_ptr(p, k, size)
    }

    /// The `f64` libm function `name` of `x`.
    fn libm_f64(&self, name: &str, x: FloatValue<'ctx>) -> R<FloatValue<'ctx>> {
        let f64t = self.llcx.f64_type();
        let f = self.extern_fn(name, f64t.fn_type(&[f64t.into()], false));
        Ok(self
            .builder
            .build_call(f, &[x.into()], "")
            .map_err(e)?
            .try_as_basic_value()
            .unwrap_basic()
            .into_float_value())
    }
}
