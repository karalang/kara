//! Display glue: `fmt.<T>(ptr)` appends the value of type `T` at `ptr` to
//! the runtime's format buffer (`karac_mir_fmt_*`), as the interpreter's
//! `display_typed` shows it (`src/mir/interp.rs`): sequences as `[a, b]`,
//! tuples as `(a, b)`, an enum value as its variant's name with its fields
//! in parentheses, a struct as `Name { f: v }` or `Name(v)`, and a type with
//! a user `Display` through its `to_string` body.

use std::collections::HashMap;

use inkwell::module::Linkage;
use inkwell::types::BasicMetadataTypeEnum;
use inkwell::values::{BasicMetadataValueEnum, FunctionValue, PointerValue};

use super::{Cx, R};
use crate::mir::interp::DisplayStyle;
use crate::mir::layout;
use crate::mir::ty::{FloatTy, IntTy, IntrinsicTy};
use crate::mir::{Ty, TyKind};

fn e<T: ToString>(x: T) -> String {
    x.to_string()
}

#[derive(Default)]
pub(super) struct Fmt<'ctx> {
    fns: HashMap<Ty, FunctionValue<'ctx>>,
    todo: Vec<(Ty, FunctionValue<'ctx>)>,
}

impl<'ctx> Cx<'ctx, '_> {
    pub(super) fn fmt_pending(&self) -> bool {
        !self.fmt.todo.is_empty()
    }

    /// The display glue of `ty`, declared now and built by
    /// [`Cx::finish_fmt`].
    pub(super) fn fmt_fn(&mut self, ty: Ty) -> FunctionValue<'ctx> {
        if let Some(&f) = self.fmt.fns.get(&ty) {
            return f;
        }
        let fn_ty = self
            .llcx
            .void_type()
            .fn_type(&[self.ptr_ty().into()], false);
        let name = format!("fmt.{}", self.tys.display(ty));
        let f = self
            .module
            .add_function(&name, fn_ty, Some(Linkage::Internal));
        self.fmt.fns.insert(ty, f);
        self.fmt.todo.push((ty, f));
        f
    }

    pub(super) fn finish_fmt(&mut self) -> R<()> {
        while let Some((ty, f)) = self.fmt.todo.pop() {
            let entry = self.llcx.append_basic_block(f, "entry");
            self.builder.position_at_end(entry);
            let p = f
                .get_first_param()
                .ok_or("display glue has no parameter")?
                .into_pointer_value();
            self.fmt_value(p, ty)?;
            self.builder.build_return(None).map_err(e)?;
        }
        Ok(())
    }

    /// Appends the value of type `ty` at `p`, at the builder's position.
    pub(super) fn fmt_call(&mut self, p: PointerValue<'ctx>, ty: Ty) -> R<()> {
        let f = self.fmt_fn(ty);
        self.builder.build_call(f, &[p.into()], "").map_err(e)?;
        Ok(())
    }

    /// `karac_mir_<name>(args) -> u8`, declared with the arguments' types.
    pub(super) fn rt_u8(
        &self,
        name: &str,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> R<inkwell::values::IntValue<'ctx>> {
        let tys = self.arg_tys(args);
        let f = self.extern_fn(
            &format!("karac_mir_{name}"),
            self.llcx.i8_type().fn_type(&tys, false),
        );
        let v = self.builder.build_call(f, args, "").map_err(e)?;
        Ok(v.try_as_basic_value().unwrap_basic().into_int_value())
    }

    pub(super) fn arg_tys(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> Vec<BasicMetadataTypeEnum<'ctx>> {
        args.iter()
            .map(|a| match a {
                BasicMetadataValueEnum::IntValue(v) => v.get_type().into(),
                BasicMetadataValueEnum::FloatValue(v) => v.get_type().into(),
                BasicMetadataValueEnum::PointerValue(v) => v.get_type().into(),
                _ => self.ptr_ty().into(),
            })
            .collect()
    }

    /// `karac_mir_<name>(args)`, declared with the arguments' types.
    pub(super) fn rt(&self, name: &str, args: &[BasicMetadataValueEnum<'ctx>]) -> R<()> {
        let tys = self.arg_tys(args);
        let f = self.extern_fn(
            &format!("karac_mir_{name}"),
            self.llcx.void_type().fn_type(&tys, false),
        );
        self.builder.build_call(f, args, "").map_err(e)?;
        Ok(())
    }

    fn lit(&mut self, s: &str) -> R<()> {
        let (p, n) = self.str_const(s)?;
        self.rt("fmt_str", &[p.into(), n.into()])
    }

    fn fmt_value(&mut self, p: PointerValue<'ctx>, ty: Ty) -> R<()> {
        let i64t = self.llcx.i64_type();
        match self.tys.kind(ty) {
            TyKind::Int(i) => {
                let v = self.load(p, ty)?.into_int_value();
                match i {
                    IntTy::I128 | IntTy::U128 => {
                        let lo = self.builder.build_int_truncate(v, i64t, "").map_err(e)?;
                        let sh = self
                            .builder
                            .build_right_shift(v, v.get_type().const_int(64, false), false, "")
                            .map_err(e)?;
                        let hi = self.builder.build_int_truncate(sh, i64t, "").map_err(e)?;
                        let s = self
                            .llcx
                            .bool_type()
                            .const_int(u64::from(i == IntTy::I128), false);
                        self.rt("fmt_i128", &[lo.into(), hi.into(), s.into()])
                    }
                    _ => {
                        let w = self
                            .builder
                            .build_int_cast_sign_flag(v, i64t, i.signed(), "")
                            .map_err(e)?;
                        self.rt(if i.signed() { "fmt_i64" } else { "fmt_u64" }, &[w.into()])
                    }
                }
            }
            TyKind::Float(FloatTy::F64) => {
                let v = self.load(p, ty)?;
                self.rt("fmt_f64", &[v.into()])
            }
            TyKind::Float(FloatTy::F32) => {
                let v = self.load(p, ty)?;
                self.rt("fmt_f32", &[v.into()])
            }
            TyKind::Bool => {
                let v = self.load(p, ty)?;
                self.rt("fmt_bool", &[v.into()])
            }
            TyKind::Char => {
                let v = self.load(p, ty)?;
                self.rt("fmt_char", &[v.into()])
            }
            TyKind::Unit => self.lit("()"),
            TyKind::Str | TyKind::Intrinsic(IntrinsicTy::String) => {
                let s = self.load_ptr(p)?;
                let n = self.load_i64(self.offset(p, 8)?)?;
                self.rt("fmt_str", &[s.into(), n.into()])
            }
            TyKind::Ref(t) | TyKind::MutRef(t) => {
                if matches!(self.tys.kind(t), TyKind::Str | TyKind::Slice(_)) {
                    return self.fmt_call(p, t);
                }
                let q = self.load_ptr(p)?;
                self.fmt_call(q, t)
            }
            TyKind::Array(el, n) => {
                let size = self.layout(el)?.size;
                self.seq(p, self.i64(n), el, size, "[", "]")
            }
            TyKind::Slice(el) | TyKind::Intrinsic(IntrinsicTy::Vec(el)) => {
                let size = self.layout(el)?.size;
                let buf = self.load_ptr(p)?;
                let n = self.load_i64(self.offset(p, 8)?)?;
                self.seq(buf, n, el, size, "[", "]")
            }
            TyKind::Tuple(ts) => {
                self.lit("(")?;
                for (i, t) in ts.iter().enumerate() {
                    if i > 0 {
                        self.lit(", ")?;
                    }
                    let off = self.field_offset(ty, None, i as u32)?;
                    let q = self.offset(p, off)?;
                    self.fmt_call(q, *t)?;
                }
                self.lit(")")
            }
            TyKind::Adt(a) | TyKind::Shared(a) => self.fmt_adt(p, ty, a),
            _ => Err(format!(
                "MIR to LLVM: printing a `{}` is not lowered yet",
                self.tys.display(ty)
            )),
        }
    }

    /// `open e0, e1, ... close` over `n` elements of `size` bytes from
    /// `base`.
    fn seq(
        &mut self,
        base: PointerValue<'ctx>,
        n: inkwell::values::IntValue<'ctx>,
        el: Ty,
        size: u64,
        open: &str,
        close: &str,
    ) -> R<()> {
        self.lit(open)?;
        self.count_loop(n, |cx, i| {
            let first = cx.is_zero(i)?;
            let not_first = cx.builder.build_not(first, "").map_err(e)?;
            cx.if_then(not_first, |cx| cx.lit(", "))?;
            let at = cx.builder.build_int_mul(i, cx.i64(size), "").map_err(e)?;
            // SAFETY: element `i < n` of the sequence.
            let q = unsafe {
                cx.builder
                    .build_in_bounds_gep(cx.llcx.i8_type(), base, &[at], "")
                    .map_err(e)?
            };
            cx.fmt_call(q, el)
        })?;
        self.lit(close)
    }

    fn fmt_adt(&mut self, p: PointerValue<'ctx>, ty: Ty, a: crate::mir::ty::AdtId) -> R<()> {
        let program = self.program;
        let style = program.display_styles.get(&a).copied();
        if style == Some(DisplayStyle::Redacted) {
            return self.lit("<redacted>");
        }
        let user = format!("{}.to_string", self.tys.adt_name(ty));
        if program.bodies.contains_key(&user) {
            return self.fmt_user(p, ty, &user);
        }
        // The body behind a `shared` handle.
        let (p, ty) = match self.tys.kind(ty) {
            TyKind::Shared(_) => {
                let b = self.load_ptr(p)?;
                let off =
                    layout::shared_body_offset(self.tys, ty).ok_or("a shared type has no body")?;
                (self.offset(b, off)?, self.tys.intern(TyKind::Adt(a)))
            }
            _ => (p, ty),
        };
        let adt = self.tys.adt(a);
        if style == Some(DisplayStyle::Transparent) {
            let ft = adt.variants[0].fields[0].1;
            let q = self.offset(p, self.field_offset(ty, None, 0)?)?;
            return self.fmt_call(q, ft);
        }
        if !adt.is_enum {
            if matches!(adt.name.as_str(), "F64" | "F32") && adt.variants[0].fields.len() == 1 {
                let ft = adt.variants[0].fields[0].1;
                return self.fmt_call(p, ft);
            }
            return self.fmt_variant(p, ty, None, &adt.name.clone(), style);
        }
        // An enum: switch on the tag, one block per variant.
        let f = self.cur_fn()?;
        let tag = self.load_tag(p, ty)?;
        let done = self.llcx.append_basic_block(f, "shown");
        let mut cases = Vec::new();
        for k in 0..adt.variants.len() {
            cases.push((
                tag.get_type().const_int(k as u64, false),
                self.llcx.append_basic_block(f, "variant"),
            ));
        }
        let none = self.llcx.append_basic_block(f, "bad.tag");
        self.builder.build_switch(tag, none, &cases).map_err(e)?;
        self.builder.position_at_end(none);
        self.builder.build_unreachable().map_err(e)?;
        for (k, (_, bb)) in cases.iter().enumerate() {
            self.builder.position_at_end(*bb);
            let name = adt.variants[k].name.clone();
            let name = if style == Some(DisplayStyle::SnakeCase) {
                crate::interpreter::pascal_to_snake(&name)
            } else {
                name
            };
            self.fmt_variant(p, ty, Some(k as u32), &name, style)?;
            self.builder.build_unconditional_branch(done).map_err(e)?;
        }
        self.builder.position_at_end(done);
        Ok(())
    }

    /// `Name`, `Name(a, b)` or `Name { x: a, y: b }`.
    fn fmt_variant(
        &mut self,
        p: PointerValue<'ctx>,
        ty: Ty,
        k: Option<u32>,
        name: &str,
        _style: Option<DisplayStyle>,
    ) -> R<()> {
        let (adt, _) = self.tys.tcx().adt_of(ty).ok_or("display of a non-ADT")?;
        let fields = adt.variants[k.unwrap_or(0) as usize].fields.clone();
        self.lit(name)?;
        if fields.is_empty() {
            return Ok(());
        }
        let named = fields.iter().any(|(n, _)| n.parse::<u32>().is_err());
        self.lit(if named { " { " } else { "(" })?;
        for (i, (fname, _)) in fields.iter().enumerate() {
            if i > 0 {
                self.lit(", ")?;
            }
            if fname.parse::<u32>().is_err() {
                self.lit(&format!("{fname}: "))?;
            }
            let ft = self
                .tys
                .field_ty(ty, k, i as u32)
                .ok_or("a variant field has no type")?;
            let q = self.offset(p, self.field_offset(ty, k, i as u32)?)?;
            self.fmt_call(q, ft)?;
        }
        self.lit(if named { " }" } else { ")" })
    }

    /// The user `Display`: `T.to_string(ref self) -> String`, shown, then
    /// dropped.
    fn fmt_user(&mut self, p: PointerValue<'ctx>, ty: Ty, name: &str) -> R<()> {
        let f = self.function(name)?;
        let string = self.tys.intern(TyKind::Intrinsic(IntrinsicTy::String));
        let ret = self.entry_slot(24, "shown")?;
        let arg = self.entry_slot(8, "self")?;
        self.builder.build_store(arg, p).map_err(e)?;
        // The body's argument is `ref T`; the format buffer stays pushed
        // across the call, since `to_string` formats into a new buffer of
        // its own.
        self.builder
            .build_call(f, &[ret.into(), arg.into()], "")
            .map_err(e)?;
        let _ = ty;
        self.fmt_value(ret, string)?;
        self.call_glue(ret, string)
    }
}
