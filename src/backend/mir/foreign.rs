//! Calls to the `extern "C"` functions a program declares (design.md § FFI;
//! B-2026-10-22-10), and to its `host fn`s, which on wasm are imports of
//! the `kara_host` module (B-2026-10-23-17): a direct call to the external symbol by the C calling
//! convention, which the linker resolves against the libraries `kara.toml`'s
//! `[link]` table names (and the C library). The lowering already refuses a
//! signature with anything but integers, `bool`, `f32`, `f64` and raw
//! pointers (`src/mir/lower.rs`), so those are all this has to pass: each in
//! its own register type, an `i1` for a `bool`, and the integers narrower
//! than 32 bits extended by their signedness, as C compilers expect.

use inkwell::attributes::{Attribute, AttributeLoc};
use inkwell::module::Linkage;
use inkwell::types::{AnyType, BasicMetadataTypeEnum, BasicType, BasicTypeEnum};
use inkwell::values::{BasicMetadataValueEnum, FunctionValue, PointerValue};

use super::{Cx, R};
use crate::mir::c_abi;
use crate::mir::interp::ExternSig;
use crate::mir::ty::FloatTy;
use crate::mir::{Operand, Place, Ty, TyKind};

fn e<T: ToString>(x: T) -> String {
    x.to_string()
}

impl<'ctx> Cx<'ctx, '_> {
    /// The C type of a parameter or result of type `t`, and the extension
    /// it is passed with.
    fn c_type(&self, name: &str, t: Ty) -> R<(BasicTypeEnum<'ctx>, Option<&'static str>)> {
        Ok(match self.tys.kind(t) {
            TyKind::Bool => (self.llcx.bool_type().into(), Some("zeroext")),
            TyKind::Int(i) => {
                let ext = (i.bits() < 32).then_some(if i.signed() { "signext" } else { "zeroext" });
                (self.int_ty(i.bits()).into(), ext)
            }
            // `char` is C's `char32_t`, a full 32-bit integer.
            TyKind::Char => (self.int_ty(32).into(), None),
            TyKind::RawPtr { .. } => (self.ptr_ty().into(), None),
            // A float travels in a float register, with no extension
            // (B-2026-10-31-32).
            TyKind::Float(FloatTy::F32) => (self.llcx.f32_type().into(), None),
            TyKind::Float(FloatTy::F64) => (self.llcx.f64_type().into(), None),
            _ => {
                return Err(format!(
                    "MIR to LLVM: the `extern \"C\"` function `{name}` takes or returns a `{}`",
                    self.tys.display(t)
                ))
            }
        })
    }

    /// How each parameter and the result of `sig` cross (`c_abi`): a
    /// one-scalar struct as its scalar, a plain struct by its platform's
    /// rules.
    fn plan(&self, name: &str, sig: &ExternSig) -> R<(Vec<Crossing>, Crossing)> {
        let refuse = |t: Ty| {
            format!(
                "MIR to LLVM: the `extern \"C\"` function `{name}` takes or returns a `{}` \
                 by value, which crosses to C only on x86-64 and AArch64 Unix",
                self.tys.display(t)
            )
        };
        let crossing_of = |t: Ty, result: bool, free: (usize, usize)| -> R<Crossing> {
            if matches!(self.tys.kind(t), TyKind::Unit) && result {
                return Ok(Crossing::Void);
            }
            if let Some(s) = c_abi::single_scalar(self.tys, t) {
                return Ok(Crossing::Scalar(s));
            }
            // A `#[repr]` enum is its C tag, an integer held in place.
            if let Some(s) = c_abi::c_enum_int(self.tys, t) {
                return Ok(Crossing::Scalar(s));
            }
            if !matches!(self.tys.kind(t), TyKind::Adt(_)) {
                return Ok(Crossing::Scalar(t));
            }
            let abi = c_abi::Abi::host().ok_or_else(|| refuse(t))?;
            let size = crate::mir::layout::layout(self.tys, t)
                .ok_or_else(|| refuse(t))?
                .size;
            Ok(
                match c_abi::classify(self.tys, abi, t, result, free).ok_or_else(|| refuse(t))? {
                    c_abi::Pass::Regs(pieces) => Crossing::Regs {
                        pieces,
                        hfa: abi == c_abi::Abi::Aapcs64,
                        size,
                    },
                    c_abi::Pass::Indirect => Crossing::Indirect(size),
                    c_abi::Pass::Stack => Crossing::Stack(size),
                    c_abi::Pass::Sret => Crossing::Sret(size),
                },
            )
        };
        let (ints, floats) = c_abi::Abi::host().map_or((6, 8), |a| a.registers());
        let ret = crossing_of(sig.ret, true, (ints, floats))?;
        let mut used = (
            usize::from(matches!(ret, Crossing::Sret(_)) && cfg!(target_arch = "x86_64")),
            0,
        );
        let mut params = Vec::with_capacity(sig.params.len());
        for &t in &sig.params {
            let c = crossing_of(
                t,
                false,
                (ints.saturating_sub(used.0), floats.saturating_sub(used.1)),
            )?;
            match &c {
                Crossing::Scalar(s) if matches!(self.tys.kind(*s), TyKind::Float(_)) => used.1 += 1,
                Crossing::Scalar(_) | Crossing::Indirect(_) => used.0 += 1,
                Crossing::Regs { pieces, .. } => {
                    for p in pieces {
                        if p.float {
                            used.1 += 1;
                        } else {
                            used.0 += 1;
                        }
                    }
                }
                _ => {}
            }
            params.push(c);
        }
        Ok((params, ret))
    }

    /// The LLVM types a struct's register pieces travel as: an `i64` per
    /// integer one, a `float` or `double` per float one; on AArch64 an HFA's
    /// or a struct's words as one array, as clang passes them.
    fn piece_types(
        &self,
        pieces: &[c_abi::Piece],
        hfa: bool,
        result: bool,
    ) -> Vec<BasicTypeEnum<'ctx>> {
        let one = |p: &c_abi::Piece| -> BasicTypeEnum<'ctx> {
            match (p.float, p.size) {
                (true, 4) => self.llcx.f32_type().into(),
                (true, _) => self.llcx.f64_type().into(),
                (false, _) => self.llcx.i64_type().into(),
            }
        };
        let tys: Vec<BasicTypeEnum<'ctx>> = pieces.iter().map(one).collect();
        if result {
            return match tys.as_slice() {
                [t] => vec![*t],
                _ => vec![self.llcx.struct_type(&tys, false).into()],
            };
        }
        if hfa && tys.len() > 1 {
            let all_float = pieces.iter().all(|p| p.float);
            if all_float {
                return vec![tys[0].array_type(tys.len() as u32).into()];
            }
            return vec![self.llcx.i64_type().array_type(tys.len() as u32).into()];
        }
        tys
    }

    /// The declaration of the C function `name`.
    fn foreign_fn(
        &self,
        name: &str,
        sig: &ExternSig,
    ) -> R<(FunctionValue<'ctx>, Vec<Crossing>, Crossing)> {
        let (plan, ret) = self.plan(name, sig)?;
        let mut params: Vec<BasicMetadataTypeEnum<'ctx>> = Vec::new();
        // Per LLVM parameter, its extension or its `byval` / `sret` size.
        let mut attrs: Vec<ParamAttr> = Vec::new();
        if let Crossing::Sret(n) = ret {
            params.push(self.ptr_ty().into());
            attrs.push(ParamAttr::Sret(n));
        }
        for c in &plan {
            match c {
                Crossing::Scalar(t) => {
                    let (t, x) = self.c_type(name, *t)?;
                    params.push(t.into());
                    attrs.push(x.map_or(ParamAttr::None, ParamAttr::Ext));
                }
                Crossing::Regs { pieces, hfa, .. } => {
                    for t in self.piece_types(pieces, *hfa, false) {
                        params.push(t.into());
                        attrs.push(ParamAttr::None);
                    }
                }
                Crossing::Indirect(_) => {
                    params.push(self.ptr_ty().into());
                    attrs.push(ParamAttr::None);
                }
                Crossing::Stack(n) => {
                    params.push(self.ptr_ty().into());
                    attrs.push(ParamAttr::Byval(*n));
                }
                Crossing::Sret(_) | Crossing::Void => {}
            }
        }
        let (fty, rext) = match &ret {
            Crossing::Void | Crossing::Sret(_) => {
                (self.llcx.void_type().fn_type(&params, false), None)
            }
            Crossing::Scalar(t) => {
                let (t, x) = self.c_type(name, *t)?;
                (t.fn_type(&params, false), x)
            }
            Crossing::Regs { pieces, hfa, .. } => {
                let t = self.piece_types(pieces, *hfa, true)[0];
                (t.fn_type(&params, false), None)
            }
            _ => {
                return Err(format!(
                    "MIR to LLVM: `{name}`'s result cannot cross from C"
                ))
            }
        };
        // A symbol the module already declares (the runtime's `malloc`) is
        // called through its address at the declared type.
        if let Some(f) = self.module.get_function(name) {
            if f.get_type() == fty {
                return Ok((f, plan, ret));
            }
            return Err(format!(
                "MIR to LLVM: the `extern \"C\"` function `{name}` is declared with \
                 another signature elsewhere in the program"
            ));
        }
        let f = self.module.add_function(name, fty, Some(Linkage::External));
        let attr = |a: &str| {
            self.llcx
                .create_enum_attribute(Attribute::get_named_enum_kind_id(a), 0)
        };
        let bytes = |n: u64| self.llcx.i8_type().array_type(n as u32).as_any_type_enum();
        for (i, x) in attrs.into_iter().enumerate() {
            let at = AttributeLoc::Param(i as u32);
            match x {
                ParamAttr::None => {}
                ParamAttr::Ext(x) => f.add_attribute(at, attr(x)),
                ParamAttr::Byval(n) => {
                    let k = Attribute::get_named_enum_kind_id("byval");
                    f.add_attribute(at, self.llcx.create_type_attribute(k, bytes(n)));
                    f.add_attribute(
                        at,
                        self.llcx
                            .create_enum_attribute(Attribute::get_named_enum_kind_id("align"), 8),
                    );
                }
                ParamAttr::Sret(n) => {
                    let k = Attribute::get_named_enum_kind_id("sret");
                    f.add_attribute(at, self.llcx.create_type_attribute(k, bytes(n)));
                }
            }
        }
        if let Some(x) = rext {
            f.add_attribute(AttributeLoc::Return, attr(x));
        }
        // A `host fn` on a wasm target is an import of the `kara_host`
        // module, which the embedder's import object provides at
        // instantiation; the attributes are also what lets wasm-ld leave
        // the symbol undefined. A plain `extern "C"` gets none, so an
        // unresolved one is still a link error.
        if sig.host && crate::target::active_target_is_wasm() {
            for (k, v) in [
                ("wasm-import-module", "kara_host"),
                ("wasm-import-name", name),
            ] {
                f.add_attribute(
                    AttributeLoc::Function,
                    self.llcx.create_string_attribute(k, v),
                );
            }
        }
        Ok((f, plan, ret))
    }

    /// `dest = name(args)` for the `extern "C"` function `name`.
    pub(super) fn foreign_call(
        &mut self,
        name: &str,
        sig: &ExternSig,
        args: &[Operand],
        dest: &Place,
    ) -> R<()> {
        let (f, plan, ret) = self.foreign_fn(name, sig)?;
        let mut vals: Vec<BasicMetadataValueEnum<'ctx>> = Vec::new();
        let sret = match ret {
            Crossing::Sret(n) => {
                let slot = self.entry_slot(n.max(1) as u32, "sret")?;
                vals.push(slot.into());
                Some(slot)
            }
            _ => None,
        };
        for (a, c) in args.iter().zip(&plan) {
            let p = self.operand_ptr(a)?;
            match c {
                // Loaded at the C type: the in-memory form of a raw pointer
                // is an opaque byte array, which no C signature takes.
                Crossing::Scalar(t) => {
                    let v = if matches!(self.tys.kind(*t), TyKind::Bool) {
                        let v = self.load(p, *t)?;
                        self.builder
                            .build_int_truncate(v.into_int_value(), self.llcx.bool_type(), "")
                            .map_err(e)?
                            .into()
                    } else {
                        let (ct, _) = self.c_type(name, *t)?;
                        self.builder.build_load(ct, p, "").map_err(e)?
                    };
                    vals.push(v.into());
                }
                // Through a copy padded to whole words, so a piece never
                // reads past the struct.
                Crossing::Regs { pieces, hfa, size } => {
                    let tmp = self.padded_copy(p, *size)?;
                    let tys = self.piece_types(pieces, *hfa, false);
                    if tys.len() == pieces.len() {
                        for (piece, t) in pieces.iter().zip(tys) {
                            let at = self.offset(tmp, piece.offset)?;
                            vals.push(self.builder.build_load(t, at, "").map_err(e)?.into());
                        }
                    } else {
                        // One array of an HFA's members or a struct's words.
                        let t = tys[0];
                        let mut agg = t.into_array_type().get_undef();
                        for (k, piece) in pieces.iter().enumerate() {
                            let et = t.into_array_type().get_element_type();
                            let at = self.offset(tmp, piece.offset)?;
                            let v = self.builder.build_load(et, at, "").map_err(e)?;
                            agg = self
                                .builder
                                .build_insert_value(agg, v, k as u32, "")
                                .map_err(e)?
                                .into_array_value();
                        }
                        vals.push(agg.into());
                    }
                }
                // A copy the callee may write, its address passed.
                Crossing::Indirect(n) | Crossing::Stack(n) => {
                    let tmp = self.padded_copy(p, *n)?;
                    vals.push(tmp.into());
                }
                Crossing::Sret(_) | Crossing::Void => {}
            }
        }
        let r = self.builder.build_call(f, &vals, "").map_err(e)?;
        let d = self.place_ptr(dest)?;
        match ret {
            Crossing::Void => Ok(()),
            Crossing::Sret(n) => {
                let slot = sret.ok_or("an sret call without its slot")?;
                self.builder
                    .build_memcpy(d, 1, slot, 1, self.i64(n))
                    .map_err(e)?;
                Ok(())
            }
            Crossing::Scalar(t) => {
                let Some(v) = r.try_as_basic_value().basic() else {
                    return Ok(());
                };
                if matches!(self.tys.kind(t), TyKind::Bool) {
                    self.store_i1(d, v.into_int_value())
                } else {
                    self.store(d, v)
                }
            }
            Crossing::Regs { pieces, size, .. } => {
                let v = r
                    .try_as_basic_value()
                    .basic()
                    .ok_or("a struct result with no value")?;
                let words = size.div_ceil(8).max(2) * 8 + 8;
                let tmp = self.entry_slot(words as u32, "cret")?;
                if pieces.len() == 1 {
                    self.store(tmp, v)?;
                } else {
                    let sv = v.into_struct_value();
                    for (k, piece) in pieces.iter().enumerate() {
                        let x = self
                            .builder
                            .build_extract_value(sv, k as u32, "")
                            .map_err(e)?;
                        let at = self.offset(tmp, piece.offset)?;
                        self.store(at, x)?;
                    }
                }
                self.builder
                    .build_memcpy(d, 1, tmp, 1, self.i64(size))
                    .map_err(e)?;
                Ok(())
            }
            _ => Err(format!(
                "MIR to LLVM: `{name}`'s result cannot cross from C"
            )),
        }
    }

    /// A word-padded stack copy of the `size` bytes at `p`.
    fn padded_copy(&mut self, p: PointerValue<'ctx>, size: u64) -> R<PointerValue<'ctx>> {
        let words = size.div_ceil(8).max(1) * 8 + 8;
        let tmp = self.entry_slot(words as u32, "carg")?;
        self.builder
            .build_memset(tmp, 8, self.llcx.i8_type().const_zero(), self.i64(words))
            .map_err(e)?;
        self.builder
            .build_memcpy(tmp, 1, p, 1, self.i64(size))
            .map_err(e)?;
        Ok(tmp)
    }
}

/// How one parameter or the result crosses to C (`Cx::plan`).
#[derive(Debug, Clone)]
enum Crossing {
    /// The result of a function returning nothing.
    Void,
    /// A scalar, or a one-scalar struct as its scalar.
    Scalar(Ty),
    /// A struct in registers, one per piece.
    Regs {
        pieces: Vec<c_abi::Piece>,
        hfa: bool,
        size: u64,
    },
    /// A pointer to a copy of the struct's `size` bytes.
    Indirect(u64),
    /// A `byval` copy on the stack.
    Stack(u64),
    /// The result, through a hidden pointer to a slot of this size.
    Sret(u64),
}

/// An attribute of one LLVM parameter of a C declaration.
enum ParamAttr {
    None,
    Ext(&'static str),
    Byval(u64),
    Sret(u64),
}
