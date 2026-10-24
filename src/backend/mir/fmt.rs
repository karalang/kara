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
use crate::mir::{Operand, Place, Ty, TyKind};

fn e<T: ToString>(x: T) -> String {
    x.to_string()
}

#[derive(Default)]
pub(super) struct Fmt<'ctx> {
    fns: HashMap<(Ty, bool), FunctionValue<'ctx>>,
    todo: Vec<(Ty, bool, FunctionValue<'ctx>)>,
    /// Glue being built shows values in their `Debug` form (`dbg`):
    /// strings and chars quoted, no user `Display`.
    debug: bool,
}

impl<'ctx> Cx<'ctx, '_> {
    pub(super) fn fmt_pending(&self) -> bool {
        !self.fmt.todo.is_empty()
    }

    /// The display glue of `ty`, declared now and built by
    /// [`Cx::finish_fmt`].
    pub(super) fn fmt_fn(&mut self, ty: Ty) -> FunctionValue<'ctx> {
        let key = (ty, self.fmt.debug);
        if let Some(&f) = self.fmt.fns.get(&key) {
            return f;
        }
        let fn_ty = self
            .llcx
            .void_type()
            .fn_type(&[self.ptr_ty().into()], false);
        let name = format!(
            "{}.{}",
            if self.fmt.debug { "dbg" } else { "fmt" },
            self.tys.display(ty)
        );
        let f = self
            .module
            .add_function(&name, fn_ty, Some(Linkage::Internal));
        self.fmt.fns.insert(key, f);
        self.fmt.todo.push((ty, self.fmt.debug, f));
        f
    }

    pub(super) fn finish_fmt(&mut self) -> R<()> {
        while let Some((ty, debug, f)) = self.fmt.todo.pop() {
            self.fmt.debug = debug;
            let entry = self.llcx.append_basic_block(f, "entry");
            self.builder.position_at_end(entry);
            let p = f
                .get_first_param()
                .ok_or("display glue has no parameter")?
                .into_pointer_value();
            self.fmt_value(p, ty)?;
            self.builder.build_return(None).map_err(e)?;
        }
        self.fmt.debug = false;
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

    pub(super) fn lit(&mut self, s: &str) -> R<()> {
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
            // An `f32` prints its own shortest digits, as the interpreter
            // does (B-2026-10-18-47).
            TyKind::Float(FloatTy::F32) => {
                let v = self.load(p, ty)?;
                let v = self.float_of(v, ty)?;
                self.rt("fmt_f32", &[v.into()])
            }
            // A half-width float prints as the `f64` it widens to, as the
            // interpreter's floats are all `f64`.
            TyKind::Float(FloatTy::F16 | FloatTy::BF16) => {
                let v = self.load(p, ty)?;
                let v = self.float_of(v, ty)?;
                let w = self
                    .builder
                    .build_float_ext(v, self.llcx.f64_type(), "")
                    .map_err(e)?;
                self.rt("fmt_f64", &[w.into()])
            }
            TyKind::Bool => {
                let v = self.load(p, ty)?;
                self.rt("fmt_bool", &[v.into()])
            }
            TyKind::Char => {
                let v = self.load(p, ty)?;
                let f = if self.fmt.debug {
                    "fmt_char_debug"
                } else {
                    "fmt_char"
                };
                self.rt(f, &[v.into()])
            }
            TyKind::Unit => self.lit("()"),
            TyKind::Str | TyKind::Intrinsic(IntrinsicTy::String) => {
                let s = self.load_ptr(p)?;
                let n = self.load_i64(self.offset(p, 8)?)?;
                let f = if self.fmt.debug {
                    "fmt_str_debug"
                } else {
                    "fmt_str"
                };
                self.rt(f, &[s.into(), n.into()])
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
                // A lane vector names itself, as the interpreter prints it.
                let open = if self.tys.is_vector(ty) {
                    "Vector("
                } else {
                    "["
                };
                let close = if open == "[" { "]" } else { ")" };
                self.seq(p, self.i64(n), el, size, open, close)
            }
            TyKind::Slice(el) | TyKind::Intrinsic(IntrinsicTy::Vec(el)) => {
                let size = self.layout(el)?.size;
                let buf = self.load_ptr(p)?;
                let n = self.load_i64(self.offset(p, 8)?)?;
                self.seq(buf, n, el, size, "[", "]")
            }
            TyKind::Intrinsic(IntrinsicTy::VecDeque(el)) => {
                let size = self.layout(el)?.size;
                let n = self.load_i64(self.offset(p, 16)?)?;
                self.lit("[")?;
                self.count_loop(n, |cx, i| {
                    let first = cx.is_zero(i)?;
                    let not_first = cx.builder.build_not(first, "").map_err(e)?;
                    cx.if_then(not_first, |cx| cx.lit(", "))?;
                    let q = cx.mrt_ptr("deque_slot", &[p.into(), i.into(), cx.i64(size).into()])?;
                    cx.fmt_call(q, el)
                })?;
                self.lit("]")
            }
            // `{k: v, ...}` or `{k, ...}` in the table's own order.
            TyKind::Intrinsic(
                IntrinsicTy::Map(..)
                | IntrinsicTy::SortedMap(..)
                | IntrinsicTy::Set(_)
                | IntrinsicTy::SortedSet(_),
            ) => {
                let t = self.table_of(ty).ok_or("not a table")?;
                let h = self.load_ptr(p)?;
                let n = self.mrt_i64("map_len", &[h.into()])?;
                let voff = match t.val {
                    Some(_) => Some(self.field_offset(t.entry, None, 1)?),
                    None => None,
                };
                self.lit("{")?;
                self.count_loop(n, |cx, i| {
                    let first = cx.is_zero(i)?;
                    let not_first = cx.builder.build_not(first, "").map_err(e)?;
                    cx.if_then(not_first, |cx| cx.lit(", "))?;
                    let at = cx.mrt_ptr("map_at", &[h.into(), i.into()])?;
                    cx.fmt_call(at, t.key)?;
                    if let (Some(v), Some(off)) = (t.val, voff) {
                        cx.lit(": ")?;
                        cx.fmt_call(cx.offset(at, off)?, v)?;
                    }
                    Ok(())
                })?;
                self.lit("}")
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

    /// `format_spec(value, spec)`: one f-string hole with a spec, as a
    /// `String` at `dest`, through the interpreter's `FormatSpec`
    /// (`karac_mir_fmt_spec`): numbers by value, anything else by its
    /// display.
    pub(super) fn format_spec(&mut self, args: &[Operand], dest: &Place) -> R<()> {
        let [v, spec] = args else {
            return Err("format_spec takes a value and a spec".into());
        };
        let mut t = self.operand_type(v)?;
        let mut p = self.operand_ptr(v)?;
        while let TyKind::Ref(i) | TyKind::MutRef(i) = self.tys.kind(t) {
            // A reference to a `str` or a slice is that view itself, not a
            // pointer to one (`view_of`; B-2026-10-14-66).
            if matches!(self.tys.kind(i), TyKind::Str | TyKind::Slice(_)) {
                t = i;
                break;
            }
            p = self.load_ptr(p)?;
            t = i;
        }
        let (sp, sn) = self.str_parts(spec)?;
        let i64t = self.llcx.i64_type();
        let (mut lo, mut hi) = (i64t.const_zero(), i64t.const_zero());
        let mut f = self.llcx.f64_type().const_zero();
        let kind = match self.tys.kind(t) {
            TyKind::Int(it) => {
                let x = self.load(p, t)?.into_int_value();
                let signed = !matches!(
                    it,
                    IntTy::U8 | IntTy::U16 | IntTy::U32 | IntTy::U64 | IntTy::Usize | IntTy::U128
                );
                if matches!(it, IntTy::I128 | IntTy::U128) {
                    lo = self.builder.build_int_truncate(x, i64t, "").map_err(e)?;
                    let sh = self
                        .builder
                        .build_right_shift(x, x.get_type().const_int(64, false), false, "")
                        .map_err(e)?;
                    hi = self.builder.build_int_truncate(sh, i64t, "").map_err(e)?;
                    if signed {
                        2
                    } else {
                        3
                    }
                } else {
                    lo = self
                        .builder
                        .build_int_cast_sign_flag(x, i64t, signed, "")
                        .map_err(e)?;
                    if matches!(it, IntTy::U64 | IntTy::Usize) {
                        1
                    } else {
                        0
                    }
                }
            }
            TyKind::Float(_) => {
                let x = self.load(p, t)?;
                let x = self.float_of(x, t)?;
                f = self
                    .builder
                    .build_float_cast(x, self.llcx.f64_type(), "")
                    .map_err(e)?;
                4
            }
            _ => {
                self.rt("fmt_begin", &[])?;
                self.fmt_call(p, t)?;
                5
            }
        };
        let d = self.place_ptr(dest)?;
        let k = self.llcx.i32_type().const_int(kind, false);
        self.rt(
            "fmt_spec",
            &[
                d.into(),
                sp.into(),
                sn.into(),
                k.into(),
                lo.into(),
                hi.into(),
                f.into(),
            ],
        )?;
        self.drop_moved(args)
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

    /// `dbg(line, text, value)`: `[file:line] text = value` on standard
    /// error, the value in its `Debug` form.
    pub(super) fn dbg(&mut self, args: &[Operand]) -> R<()> {
        let [line, text, value] = args else {
            return Err("dbg takes a line, a text and a value".into());
        };
        self.rt("fmt_begin", &[])?;
        // The builder passes the line as a constant, so a package's combined
        // source maps it to its file here (project mode, B-2026-10-22-14).
        if let Operand::Const(crate::mir::syntax::Const {
            kind: crate::mir::syntax::ConstKind::Scalar(n),
            ..
        }) = line
        {
            let (file, l) = crate::mir::interp::source_at(*n as usize);
            self.lit(&format!("[{file}:{l}] "))?;
        } else {
            let file = crate::mir::interp::source_name().unwrap_or("<unknown>");
            self.lit(&format!("[{file}:"))?;
            let l = self.int_arg(line)?;
            self.rt("fmt_i64", &[l.into()])?;
            self.lit("] ")?;
        }
        let (s, n) = self.str_parts(text)?;
        self.rt("fmt_str", &[s.into(), n.into()])?;
        self.lit(" = ")?;
        let vt = self.operand_type(value)?;
        let p = self.operand_ptr(value)?;
        self.fmt.debug = true;
        let f = self.fmt_fn(vt);
        self.fmt.debug = false;
        self.builder.build_call(f, &[p.into()], "").map_err(e)?;
        let f = self.llcx.i32_type().const_int(3, false);
        self.rt("print_end", &[f.into()])?;
        self.drop_moved(args)
    }

    /// A value's `Debug` text, as `dbg` shows it, in a new String (a
    /// failed `assert_eq`'s operands).
    pub(super) fn debug_string(&mut self, args: &[Operand], dest: &Place) -> R<()> {
        let [value] = args else {
            return Err("debug_string takes one value".into());
        };
        self.rt("fmt_begin", &[])?;
        let vt = self.operand_type(value)?;
        let p = self.operand_ptr(value)?;
        self.fmt.debug = true;
        let f = self.fmt_fn(vt);
        self.fmt.debug = false;
        self.builder.build_call(f, &[p.into()], "").map_err(e)?;
        let d = self.place_ptr(dest)?;
        self.rt("fmt_end_string", &[d.into()])?;
        self.drop_moved(args)
    }

    fn fmt_adt(&mut self, p: PointerValue<'ctx>, ty: Ty, a: crate::mir::ty::AdtId) -> R<()> {
        let program = self.program;
        let style = program.display_styles.get(&a).copied();
        if style == Some(DisplayStyle::Redacted) {
            return self.lit("<redacted>");
        }
        let user = format!("{}.to_string", self.tys.adt_name(ty));
        if !self.fmt.debug && program.bodies.contains_key(&user) {
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
            // The total-order float wrappers show as the float they hold
            // (B-2026-10-22-32 for the 16-bit ones).
            if matches!(adt.name.as_str(), "F64" | "F32" | "F16" | "Bf16")
                && adt.variants[0].fields.len() == 1
            {
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
                crate::ident_case::pascal_to_snake(&name)
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
            let q = self.field_ptr(p, ty, k, i as u32)?;
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
