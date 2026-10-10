//! MIR to LLVM (M2): the backend that compiles an elaborated MIR
//! [`Program`] to an LLVM module.
//!
//! This file holds the skeleton both halves build on: the [`Cx`] that owns
//! the LLVM context, module and builder; the memory type of every MIR type
//! ([`Cx::llvm_ty`]), taken from [`crate::mir::layout`], the one layout
//! source the MIR interpreter shares; the declaration of a function per
//! body; and the walk that gives each body its locals and blocks. `drop.rs`
//! lowers the elaborated `Drop` terminator and builds each type's drop
//! glue; `body.rs` lowers places, operands, rvalues and the other
//! terminators.
//!
//! **Values live in memory.** Every MIR local is an `alloca` sized and
//! aligned by its layout, and every aggregate is reached as bytes at the
//! offsets [`crate::mir::layout::field_offset`] gives, so no LLVM struct
//! type is ever built and LLVM cannot disagree with the interpreter about a
//! layout. LLVM's `mem2reg`/SROA recover registers.
//!
//! **Calling convention.** A body `f(_1: A1, ..., _n: An) -> R` is
//! `void @f(ptr ret, ptr a1, ..., ptr an)`. `ret` points at caller memory
//! for `R`, and each `ai` points at caller memory holding the argument
//! value, which the callee then owns (or borrows, for a reference: the
//! slot holds the reference). The callee uses those pointers as the
//! storage of `_0` and `_1.._n`, so a call copies nothing.
//!
//! **Heap memory** (`String` and `Vec` buffers, `shared` boxes) comes from
//! `malloc` and goes back through `free`.

mod body;
mod drop;

use std::collections::HashMap;

use inkwell::basic_block::BasicBlock as LlBlock;
use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::{Linkage, Module};
use inkwell::types::{BasicMetadataTypeEnum, BasicTypeEnum, FunctionType};
use inkwell::values::{FunctionValue, IntValue, PointerValue};
use inkwell::AddressSpace;

use crate::mir::interp::Program;
use crate::mir::layout::{self, Layout};
use crate::mir::ty::{FloatTy, TyKind};
use crate::mir::{Body, Place, TerminatorKind, Ty, TyInterner};

pub(crate) type R<T> = Result<T, String>;

/// The LLVM name of the body `name`: prefixed, so a Kāra function cannot
/// collide with a C symbol (`main`, `free`).
pub fn fn_symbol(name: &str) -> String {
    format!("kara.{name}")
}

/// Compiles the elaborated `program` into a module named `name`, with a C
/// `main` that runs the Kāra `main` when the program has one.
pub fn compile_mir<'ctx>(
    llcx: &'ctx Context,
    name: &str,
    program: &Program,
    tys: &TyInterner,
) -> R<Module<'ctx>> {
    let mut cx = Cx::new(llcx, name, program, tys);
    cx.declare_bodies()?;
    for body in program.bodies.values() {
        cx.lower_body(body)?;
    }
    cx.c_main()?;
    cx.finish_glue()?;
    cx.module.verify().map_err(|e| e.to_string())?;
    Ok(cx.module)
}

/// The backend's state: the LLVM handles, the program being compiled, and
/// the body being lowered.
pub struct Cx<'ctx, 'p> {
    pub llcx: &'ctx Context,
    pub module: Module<'ctx>,
    pub builder: Builder<'ctx>,
    pub program: &'p Program,
    pub tys: &'p TyInterner,
    /// The function of each body, by its MIR name.
    pub fns: HashMap<String, FunctionValue<'ctx>>,
    /// The body being lowered, and where each of its locals lives.
    pub body: Option<&'p Body>,
    pub locals: Vec<PointerValue<'ctx>>,
    /// The LLVM block of each MIR block of `body`.
    pub blocks: Vec<LlBlock<'ctx>>,
    glue: drop::Glue<'ctx>,
}

impl<'ctx, 'p> Cx<'ctx, 'p> {
    pub fn new(llcx: &'ctx Context, name: &str, program: &'p Program, tys: &'p TyInterner) -> Self {
        Cx {
            llcx,
            module: llcx.create_module(name),
            builder: llcx.create_builder(),
            program,
            tys,
            fns: HashMap::new(),
            body: None,
            locals: Vec::new(),
            blocks: Vec::new(),
            glue: drop::Glue::default(),
        }
    }

    pub fn ptr_ty(&self) -> inkwell::types::PointerType<'ctx> {
        self.llcx.ptr_type(AddressSpace::default())
    }

    pub fn i64(&self, v: u64) -> IntValue<'ctx> {
        self.llcx.i64_type().const_int(v, false)
    }

    /// The integer type of `bits` bits (8, 16, 32, 64 or 128).
    pub fn int_ty(&self, bits: u32) -> inkwell::types::IntType<'ctx> {
        let c = self.llcx;
        match bits {
            8 => c.i8_type(),
            16 => c.i16_type(),
            32 => c.i32_type(),
            128 => c.i128_type(),
            _ => c.i64_type(),
        }
    }

    /// The layout of `t`, or an error naming a type with no size.
    pub fn layout(&self, t: Ty) -> R<Layout> {
        layout::layout(self.tys, t)
            .ok_or_else(|| format!("`{}` has no layout", self.tys.display(t)))
    }

    /// The LLVM type a value of `t` is loaded and stored as: the scalar
    /// for a scalar, a pointer for a one-word handle or reference, and
    /// otherwise `[size x i8]`, which is only ever reached by offset.
    pub fn llvm_ty(&self, t: Ty) -> R<BasicTypeEnum<'ctx>> {
        let c = self.llcx;
        Ok(match self.tys.kind(t) {
            TyKind::Bool => c.i8_type().into(),
            TyKind::Char => c.i32_type().into(),
            TyKind::Int(i) => self.int_ty(i.bits()).into(),
            TyKind::Float(FloatTy::F16) => c.f16_type().into(),
            TyKind::Float(FloatTy::BF16) => c.i16_type().into(),
            TyKind::Float(FloatTy::F32) => c.f32_type().into(),
            TyKind::Float(FloatTy::F64) => c.f64_type().into(),
            TyKind::Shared(_) | TyKind::Weak(_) => self.ptr_ty().into(),
            TyKind::Ref(_) | TyKind::MutRef(_) if self.layout(t)?.size == 8 => self.ptr_ty().into(),
            _ => c.i8_type().array_type(self.layout(t)?.size as u32).into(),
        })
    }

    /// `base + offset` bytes.
    pub fn offset(&self, base: PointerValue<'ctx>, offset: u64) -> R<PointerValue<'ctx>> {
        if offset == 0 {
            return Ok(base);
        }
        // SAFETY: an in-bounds byte offset within the value at `base`,
        // computed from its layout.
        unsafe {
            self.builder
                .build_in_bounds_gep(self.llcx.i8_type(), base, &[self.i64(offset)], "")
                .map_err(|e| e.to_string())
        }
    }

    /// `free` (and `malloc`), declared once.
    pub fn libc_fn(&self, name: &str) -> FunctionValue<'ctx> {
        if let Some(f) = self.module.get_function(name) {
            return f;
        }
        let p = self.ptr_ty();
        let ty = match name {
            "malloc" => p.fn_type(&[self.llcx.i64_type().into()], false),
            _ => self.llcx.void_type().fn_type(&[p.into()], false),
        };
        self.module.add_function(name, ty, Some(Linkage::External))
    }

    /// The external function `name` of type `ty`, declared once (a
    /// runtime entry point).
    pub fn extern_fn(&self, name: &str, ty: FunctionType<'ctx>) -> FunctionValue<'ctx> {
        self.module
            .get_function(name)
            .unwrap_or_else(|| self.module.add_function(name, ty, Some(Linkage::External)))
    }

    /// One function per body, by the calling convention above.
    fn declare_bodies(&mut self) -> R<()> {
        for (name, body) in &self.program.bodies {
            let params: Vec<BasicMetadataTypeEnum> =
                (0..=body.arg_count).map(|_| self.ptr_ty().into()).collect();
            let ty = self.llcx.void_type().fn_type(&params, false);
            let f = self
                .module
                .add_function(&fn_symbol(name), ty, Some(Linkage::Internal));
            self.fns.insert(name.clone(), f);
        }
        Ok(())
    }

    /// The function of the body `name`.
    pub fn function(&self, name: &str) -> R<FunctionValue<'ctx>> {
        self.fns
            .get(name)
            .copied()
            .ok_or_else(|| format!("no body named `{name}`"))
    }

    /// Lowers `body` into its declared function: an `alloca` per local
    /// that is not the return place or an argument, an LLVM block per MIR
    /// block, then each block's statements and terminator.
    fn lower_body(&mut self, body: &'p Body) -> R<()> {
        let f = self.function(&body.instance.name)?;
        let entry = self.llcx.append_basic_block(f, "entry");
        self.builder.position_at_end(entry);
        self.body = Some(body);
        self.locals.clear();
        for (i, decl) in body.locals.iter().enumerate() {
            let slot = if i <= body.arg_count {
                f.get_nth_param(i as u32)
                    .ok_or("a parameter is missing")?
                    .into_pointer_value()
            } else {
                let l = self.layout(decl.ty)?;
                let a = self
                    .builder
                    .build_alloca(
                        self.llcx.i8_type().array_type(l.size as u32),
                        &format!("_{i}"),
                    )
                    .map_err(|e| e.to_string())?;
                a.as_instruction()
                    .ok_or("an alloca is not an instruction")?
                    .set_alignment(l.align as u32)
                    .map_err(|e| e.to_string())?;
                a
            };
            self.locals.push(slot);
        }
        self.blocks = (0..body.blocks.len())
            .map(|i| self.llcx.append_basic_block(f, &format!("bb{i}")))
            .collect();
        self.builder
            .build_unconditional_branch(self.blocks[0])
            .map_err(|e| e.to_string())?;
        for (i, block) in body.blocks.iter().enumerate() {
            self.builder.position_at_end(self.blocks[i]);
            for s in &block.statements {
                self.statement(s)?;
            }
            match &block.terminator.kind {
                TerminatorKind::Drop { place, target, .. } => {
                    self.drop_terminator(place, *target)?
                }
                kind => self.terminator(kind)?,
            }
        }
        self.body = None;
        Ok(())
    }

    /// The body being lowered.
    pub fn cur(&self) -> R<&'p Body> {
        self.body
            .ok_or_else(|| "no body is being lowered".to_string())
    }

    /// A pointer to `place`'s storage. Locals only for now; `body.rs` adds
    /// the projections.
    pub fn place_ptr(&mut self, place: &Place) -> R<PointerValue<'ctx>> {
        if !place.projection.is_empty() {
            return self.projected_place_ptr(place);
        }
        self.locals
            .get(place.local.index())
            .copied()
            .ok_or_else(|| format!("no local {:?}", place.local))
    }

    /// A C `i32 main()` that runs the Kāra `main` and returns 0.
    fn c_main(&mut self) -> R<()> {
        let Some(&kmain) = self.fns.get("main") else {
            return Ok(());
        };
        let body = &self.program.bodies["main"];
        if body.arg_count != 0 {
            return Err("`main` takes arguments".into());
        }
        let i32t = self.llcx.i32_type();
        let f = self
            .module
            .add_function("main", i32t.fn_type(&[], false), None);
        self.builder
            .position_at_end(self.llcx.append_basic_block(f, "entry"));
        let l = self.layout(body.return_ty())?;
        let ret = self
            .builder
            .build_alloca(self.llcx.i8_type().array_type(l.size as u32), "ret")
            .map_err(|e| e.to_string())?;
        self.builder
            .build_call(kmain, &[ret.into()], "")
            .map_err(|e| e.to_string())?;
        self.builder
            .build_return(Some(&i32t.const_zero()))
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}
