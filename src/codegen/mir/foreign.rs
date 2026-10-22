//! Calls to the `extern "C"` functions a program declares (design.md § FFI;
//! B-2026-10-22-10): a direct call to the external symbol by the C calling
//! convention, which the linker resolves against the libraries `kara.toml`'s
//! `[link]` table names (and the C library). The lowering already refuses a
//! signature with anything but integers, `bool` and raw pointers
//! (`src/mir/lower.rs`), so those are all this has to pass: each in its own
//! register type, an `i1` for a `bool`, and the integers narrower than 32
//! bits extended by their signedness, as C compilers expect.

use inkwell::attributes::{Attribute, AttributeLoc};
use inkwell::module::Linkage;
use inkwell::types::{BasicMetadataTypeEnum, BasicType, BasicTypeEnum};
use inkwell::values::{BasicMetadataValueEnum, FunctionValue};

use super::{Cx, R};
use crate::mir::interp::ExternSig;
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
            TyKind::RawPtr { .. } => (self.ptr_ty().into(), None),
            _ => {
                return Err(format!(
                    "MIR to LLVM: the `extern \"C\"` function `{name}` takes or returns a `{}`",
                    self.tys.display(t)
                ))
            }
        })
    }

    /// The declaration of the C function `name`.
    fn foreign_fn(&self, name: &str, sig: &ExternSig) -> R<FunctionValue<'ctx>> {
        let mut params: Vec<BasicMetadataTypeEnum<'ctx>> = Vec::new();
        let mut exts = Vec::new();
        for &p in &sig.params {
            let (t, x) = self.c_type(name, p)?;
            params.push(t.into());
            exts.push(x);
        }
        let unit = matches!(self.tys.kind(sig.ret), TyKind::Unit);
        let (fty, rext) = if unit {
            (self.llcx.void_type().fn_type(&params, false), None)
        } else {
            let (t, x) = self.c_type(name, sig.ret)?;
            (t.fn_type(&params, false), x)
        };
        // A symbol the module already declares (the runtime's `malloc`) is
        // called through its address at the declared type.
        if let Some(f) = self.module.get_function(name) {
            if f.get_type() == fty {
                return Ok(f);
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
        for (i, x) in exts.into_iter().enumerate() {
            if let Some(x) = x {
                f.add_attribute(AttributeLoc::Param(i as u32), attr(x));
            }
        }
        if let Some(x) = rext {
            f.add_attribute(AttributeLoc::Return, attr(x));
        }
        Ok(f)
    }

    /// `dest = name(args)` for the `extern "C"` function `name`.
    pub(super) fn foreign_call(
        &mut self,
        name: &str,
        sig: &ExternSig,
        args: &[Operand],
        dest: &Place,
    ) -> R<()> {
        let f = self.foreign_fn(name, sig)?;
        let mut vals: Vec<BasicMetadataValueEnum<'ctx>> = Vec::new();
        for (a, &t) in args.iter().zip(&sig.params) {
            let p = self.operand_ptr(a)?;
            // Loaded at the C type: the in-memory form of a raw pointer is an
            // opaque byte array, which no C signature takes.
            let v = if matches!(self.tys.kind(t), TyKind::Bool) {
                let v = self.load(p, t)?;
                self.builder
                    .build_int_truncate(v.into_int_value(), self.llcx.bool_type(), "")
                    .map_err(e)?
                    .into()
            } else {
                let (ct, _) = self.c_type(name, t)?;
                self.builder.build_load(ct, p, "").map_err(e)?
            };
            vals.push(v.into());
        }
        let r = self.builder.build_call(f, &vals, "").map_err(e)?;
        let Some(v) = r.try_as_basic_value().basic() else {
            return Ok(());
        };
        let d = self.place_ptr(dest)?;
        if matches!(self.tys.kind(sig.ret), TyKind::Bool) {
            self.store_i1(d, v.into_int_value())
        } else {
            self.store(d, v)
        }
    }
}
