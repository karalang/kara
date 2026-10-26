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
mod call;
mod callable;
mod cells;
mod clone;
mod debug;
mod deque;
mod drop;
mod flag;
mod fmt;
mod foreign;
mod io;
mod key;
mod num;
mod pool;
mod seq;
mod table;
mod task;
mod tracing;
mod vector;

use std::collections::HashMap;

use inkwell::attributes::{Attribute, AttributeLoc};
use inkwell::basic_block::BasicBlock as LlBlock;
use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::{Linkage, Module};
use inkwell::types::{BasicMetadataTypeEnum, BasicType, BasicTypeEnum, FunctionType};
use inkwell::values::{BasicMetadataValueEnum, BasicValue, FunctionValue, IntValue, PointerValue};
use inkwell::AddressSpace;
use inkwell::IntPredicate;

use crate::mir::coroutine;
use crate::mir::interp::{self, Program};
use crate::mir::layout::{self, Layout};
use crate::mir::ty::{FloatTy, TyKind};
use crate::mir::{Body, Place, TerminatorKind, Ty, TyInterner};

/// The largest body, in blocks and in statements, that is hinted for
/// inlining ([`Cx::lower_body`]).
const INLINE_HINT_MAX_BLOCKS: usize = 32;
const INLINE_HINT_MAX_STATEMENTS: usize = 32;

pub(crate) type R<T> = Result<T, String>;

/// The LLVM name of the body `name`: prefixed, so a Kāra function cannot
/// collide with a C symbol (`main`, `free`).
pub fn fn_symbol(name: &str) -> String {
    format!("kara.{name}")
}

/// Compiles the elaborated `program` into a module named `name`, with a C
/// `main` that runs the Kāra `main` when the program has one. Unless
/// `KARAC_MIR_COROUTINES=0`, each coroutine is compiled as its state
/// machine, and a coroutine `main` runs as the task root.
pub fn compile_mir<'ctx>(
    llcx: &'ctx Context,
    name: &str,
    program: &Program,
    tys: &TyInterner,
) -> R<Module<'ctx>> {
    let transformed;
    let program = if interp::coroutines_flag() {
        transformed = interp::with_coroutines(program, tys)?;
        &transformed
    } else {
        program
    };
    let mut cx = Cx::new(llcx, name, program, tys);
    if let Some(source) = interp::source_name() {
        cx.debug_init(source);
    }
    cx.c_main()?;
    cx.drain()?;
    cx.mark_noalias();
    cx.debug_finish();
    cx.module.verify().map_err(|e| e.to_string())?;
    Ok(cx.module)
}

/// Whether direct-call parameters are marked `noalias` and promoted
/// (`Cx::mark_noalias`); `KARAC_MIR_NOALIAS=0` turns it off.
pub fn noalias_flag() -> bool {
    std::env::var("KARAC_MIR_NOALIAS").as_deref() != Ok("0")
}

/// The LLVM IR of `program`, for `karac_jit_runner` to run: that binary
/// links the runtime the compiled code calls, which `karac` itself does
/// not.
pub fn compile_mir_ir(program: &Program, tys: &TyInterner) -> R<String> {
    let llcx = Context::create();
    let module = compile_mir(&llcx, "main", program, tys)?;
    Ok(module.print_to_string().to_string())
}

/// [`compile_mir_ir`] with scalar replacement run first, for the JIT, which
/// otherwise generates code from the unoptimised IR: instruction selection
/// on one long function's thousands of stack slots grows faster than
/// quadratically (B-2026-10-21-78), and `sroa` removes them in linear time.
/// `KARAC_MIR_JIT_SROA=0` leaves the IR as built.
pub fn compile_mir_ir_for_jit(program: &Program, tys: &TyInterner) -> R<String> {
    let llcx = Context::create();
    let module = compile_mir(&llcx, "main", program, tys)?;
    // RuntimeDyldMachO is not known to link a thread-local, so on Apple
    // the JIT keeps the call-site globals plain: the pool's workers then
    // share them, which costs speed only (B-2026-10-26-7).
    if cfg!(target_vendor = "apple") {
        for name in ["karac.site", "karac.site.len"] {
            if let Some(g) = module.get_global(name) {
                g.set_thread_local(false);
            }
        }
    }
    if std::env::var("KARAC_MIR_JIT_SROA").as_deref() != Ok("0") {
        let tm = super::driver::create_target_machine()?;
        // Inlining as well: every argument is passed by pointer, and a
        // small callee left out of line costs its loads and stores at each
        // call. On kata 3629's bench (release build) the run drops from
        // 24.5 s to 8.1 s (legacy's JIT: 7.2 s), at a compile cost the
        // largest programs feel: the 20,000-line mini_pyasn1 app takes
        // 10.5 s to start where it took 7.6 s.
        // `KARAC_MIR_JIT_INLINE=0` leaves it at `sroa`.
        let passes = if std::env::var("KARAC_MIR_JIT_INLINE").as_deref() == Ok("0") {
            "function(sroa)"
        } else {
            "function(sroa),cgscc(inline),function(sroa,instcombine<no-verify-fixpoint>,simplifycfg)"
        };
        module
            .run_passes(passes, &tm, inkwell::passes::PassBuilderOptions::create())
            .map_err(|e| format!("{passes}: {}", e.to_string()))?;
        // `sroa` grows a name per split (`_463.sroa.0.sroa.0...`); past
        // the IR parser's length limit two such names read back as one.
        for f in module.get_functions() {
            for b in f.get_basic_blocks() {
                for i in b.get_instructions() {
                    if i.get_name().is_some_and(|n| n.to_bytes().len() > 64) {
                        let _ = i.set_name("");
                    }
                }
            }
        }
    }
    Ok(module.print_to_string().to_string())
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
    /// The bodies some caller other than a direct call reaches, which may
    /// hand a body a pointer that is not a fresh slot of its own
    /// ([`Cx::mark_noalias`]).
    shared_fns: std::collections::HashSet<String>,
    /// The body being lowered, and where each of its locals lives.
    pub body: Option<&'p Body>,
    pub locals: Vec<PointerValue<'ctx>>,
    /// The LLVM block of each MIR block of `body`.
    pub blocks: Vec<LlBlock<'ctx>>,
    glue: drop::Glue<'ctx>,
    fmt: fmt::Fmt<'ctx>,
    clones: clone::Clones<'ctx>,
    keys: key::Keys<'ctx>,
    shims: callable::Shims<'ctx>,
    /// Whether the program calls suspending functions through values
    /// (`callable.rs`), once asked.
    dyn_fns: Option<bool>,
    /// Whether the program makes an isolating group (core-semantics
    /// §11.12), once asked.
    isolating: Option<bool>,
    /// While a native call with a cancel exit is lowered in a program that
    /// makes an isolating group: the block the call returns to, which a
    /// panic the running task takes inside the native jumps to, to leave
    /// by that exit (`body.rs`'s `abort_with`).
    contain_to: Option<inkwell::basic_block::BasicBlock<'ctx>>,
    /// Bodies declared and not lowered yet.
    todo: Vec<&'p Body>,
    /// The bodies already put on `todo`.
    scheduled: std::collections::HashSet<String>,
    /// The by-value function of each body that has one ([`Cx::value_sig`]):
    /// a direct call passes its arguments and takes its result in
    /// registers, and the pointer-convention function in `fns`, when
    /// something asks for one, wraps it.
    vfns: HashMap<String, FunctionValue<'ctx>>,
    /// Pointer-convention wrappers declared and not built yet.
    wrap_todo: Vec<String>,
    /// How the body being lowered returns: `Some` when by value, with the
    /// result's register type (`None` for nothing).
    ret_by_value: Option<Option<BasicTypeEnum<'ctx>>>,
    /// The borrow flags the body being lowered can hold (`flag.rs`).
    flags: Vec<flag::FlagSlot<'ctx>>,
    /// The function's one abort block per message, with the phis that
    /// bring it each site (`abort_with`).
    abort_blocks: Vec<(
        String,
        inkwell::basic_block::BasicBlock<'ctx>,
        inkwell::values::PhiValue<'ctx>,
        inkwell::values::PhiValue<'ctx>,
    )>,
    /// `KARAC_MIR_FLAGS=0` emits no borrow flags.
    flags_on: bool,
    /// The global holding each static, by index.
    statics: HashMap<u32, PointerValue<'ctx>>,
    /// Line tables, when the program has a source file (`debug.rs`).
    debug: Option<debug::Debug<'ctx>>,
    /// Where the statement or terminator being lowered is (`debug.rs`).
    site: crate::token::Span,
    /// Whether each body can abort, as found so far (`debug.rs`).
    aborts: HashMap<String, bool>,
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
            shared_fns: std::collections::HashSet::new(),
            body: None,
            locals: Vec::new(),
            blocks: Vec::new(),
            glue: drop::Glue::default(),
            fmt: fmt::Fmt::default(),
            clones: clone::Clones::default(),
            keys: key::Keys::default(),
            shims: callable::Shims::default(),
            dyn_fns: None,
            isolating: None,
            contain_to: None,
            todo: Vec::new(),
            scheduled: std::collections::HashSet::new(),
            vfns: HashMap::new(),
            wrap_todo: Vec::new(),
            ret_by_value: None,
            flags: Vec::new(),
            abort_blocks: Vec::new(),
            flags_on: std::env::var("KARAC_MIR_FLAGS").as_deref() != Ok("0"),
            statics: HashMap::new(),
            debug: None,
            site: crate::token::Span::default(),
            aborts: HashMap::new(),
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
            TyKind::Float(FloatTy::F16) => c.i16_type().into(),
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

    /// `free` (and `malloc`, `calloc`), declared once.
    pub fn libc_fn(&self, name: &str) -> FunctionValue<'ctx> {
        if matches!(name, "malloc" | "calloc") && crate::target::active_target_is_wasm() {
            return self.libc_size_shim(name);
        }
        let p = self.ptr_ty();
        let (i32t, i64t) = (self.llcx.i32_type(), self.llcx.i64_type());
        // `memcmp(a, b, n)` and `memchr(s, c, n)`: their `size_t` is the
        // last parameter, 32 bits on wasm.
        let sized = match name {
            "memcmp" => Some(i32t.fn_type(&[p.into(), p.into(), i64t.into()], false)),
            "memchr" => Some(p.fn_type(&[p.into(), i32t.into(), i64t.into()], false)),
            _ => None,
        };
        if let (Some(ty), true) = (sized, crate::target::active_target_is_wasm()) {
            return self.narrow_shim(name, ty, &[2]);
        }
        if let Some(f) = self.module.get_function(name) {
            return f;
        }
        let ty = match name {
            "malloc" => p.fn_type(&[i64t.into()], false),
            "calloc" => p.fn_type(&[i64t.into(), i64t.into()], false),
            _ => sized.unwrap_or_else(|| self.llcx.void_type().fn_type(&[p.into()], false)),
        };
        self.module.add_function(name, ty, Some(Linkage::External))
    }

    /// On a 32-bit target `malloc` and `calloc` take 32-bit sizes, while
    /// MIR sizes are always `i64`.
    fn libc_size_shim(&self, name: &str) -> FunctionValue<'ctx> {
        let i64t = self.llcx.i64_type();
        let arity = if name == "calloc" { 2 } else { 1 };
        let ty = self.ptr_ty().fn_type(&vec![i64t.into(); arity], false);
        self.narrow_shim(name, ty, &[0, 1][..arity])
    }

    /// The `usize` parameters of the runtime entry points shared with the
    /// legacy backend, which keep their pointer-width C types (the
    /// `karac_mir_*` ones take `u64` instead).
    fn usize_params(name: &str) -> Option<&'static [u32]> {
        Some(match name {
            "karac_alloc_fallible" | "karac_alloc_or_panic" => &[0],
            "karac_alloc_zeroed_or_panic" => &[0, 1],
            "karac_cpu_supports" | "karac_free_buf" | "karac_hash_bytes" => &[1],
            "karac_hash_bytes_fx" | "karac_realloc_or_panic" => &[1],
            "karac_stable_siphash24" => &[1],
            "karac_runtime_http_builder_add_header" => &[2, 4],
            "karac_runtime_http_builder_new" => &[1, 3],
            "karac_runtime_http_builder_set_body" => &[2],
            _ => return None,
        })
    }

    /// `name` called with the `i64` arguments of `ty`, through a private
    /// shim that truncates the `narrow` ones to `i32`: on a 32-bit target
    /// they are `usize` on the C side, and MIR sizes are always `i64`.
    fn narrow_shim(
        &self,
        name: &str,
        ty: FunctionType<'ctx>,
        narrow: &[u32],
    ) -> FunctionValue<'ctx> {
        let shim = format!("__karac_{name}64");
        if let Some(f) = self.module.get_function(&shim) {
            return f;
        }
        let i32t = self.llcx.i32_type();
        let params: Vec<BasicMetadataTypeEnum<'ctx>> = ty
            .get_param_types()
            .into_iter()
            .enumerate()
            .map(|(i, t)| {
                if narrow.contains(&(i as u32)) {
                    i32t.into()
                } else {
                    t
                }
            })
            .collect();
        let real = self.module.get_function(name).unwrap_or_else(|| {
            let rty = match ty.get_return_type() {
                Some(r) => r.fn_type(&params, false),
                None => self.llcx.void_type().fn_type(&params, false),
            };
            self.module.add_function(name, rty, Some(Linkage::External))
        });
        let f = self.module.add_function(&shim, ty, Some(Linkage::Private));
        let saved = self.builder.get_insert_block();
        let b = self.llcx.append_basic_block(f, "entry");
        self.builder.position_at_end(b);
        let args: Vec<BasicMetadataValueEnum<'ctx>> = f
            .get_param_iter()
            .enumerate()
            .map(|(i, v)| {
                if narrow.contains(&(i as u32)) {
                    let v = v.into_int_value();
                    self.builder.build_int_truncate(v, i32t, "").unwrap().into()
                } else {
                    v.into()
                }
            })
            .collect();
        let r = self.builder.build_call(real, &args, "").unwrap();
        match r.try_as_basic_value().basic() {
            Some(r) => self.builder.build_return(Some(&r)).unwrap(),
            None => self.builder.build_return(None).unwrap(),
        };
        if let Some(saved) = saved {
            self.builder.position_at_end(saved);
        }
        f
    }

    /// The external function `name` of type `ty`, declared once (a
    /// runtime entry point).
    pub fn extern_fn(&self, name: &str, ty: FunctionType<'ctx>) -> FunctionValue<'ctx> {
        if let Some(narrow) = Self::usize_params(name) {
            if crate::target::active_target_is_wasm() {
                return self.narrow_shim(name, ty, narrow);
            }
        }
        self.module.get_function(name).unwrap_or_else(|| {
            let f = self.module.add_function(name, ty, Some(Linkage::External));
            self.bool_abi(f, true);
            // The runtime's `-> !` functions: a call to one is a path the
            // optimizer may treat as rare, so a check that aborts does not
            // weigh against inlining the function it is in.
            if matches!(
                name,
                "karac_mir_abort_at" | "karac_mir_abort" | "karac_mir_exit"
            ) {
                for a in ["noreturn", "cold", "nounwind"] {
                    let k = Attribute::get_named_enum_kind_id(a);
                    f.add_attribute(
                        AttributeLoc::Function,
                        self.llcx.create_enum_attribute(k, 0),
                    );
                }
            }
            f
        })
    }

    /// Marks the `i1` return of `f`, and for a runtime function its `i1`,
    /// `i8` and `i16` parameters (every one a `bool`, `u8` or `u16` on
    /// the Rust side), as zero-extended, the C ABI those types have: the
    /// Rust callee may test the whole register, so an optimised caller
    /// that leaves its upper bits undefined passes `false` as `true`.
    pub(super) fn bool_abi(&self, f: FunctionValue<'ctx>, params: bool) {
        let zext = self
            .llcx
            .create_enum_attribute(Attribute::get_named_enum_kind_id("zeroext"), 0);
        let width = |t: BasicTypeEnum| t.is_int_type().then(|| t.into_int_type().get_bit_width());
        let is_i1 = |t: BasicTypeEnum| width(t) == Some(1);
        let fty = f.get_type();
        if fty.get_return_type().is_some_and(is_i1) {
            f.add_attribute(AttributeLoc::Return, zext);
        }
        if params {
            for (i, t) in fty.get_param_types().into_iter().enumerate() {
                if let Ok(t) = BasicTypeEnum::try_from(t) {
                    if matches!(width(t), Some(1 | 8 | 16)) {
                        f.add_attribute(AttributeLoc::Param(i as u32), zext);
                    }
                }
            }
        }
    }

    /// Declares the function of `body` by the calling convention above.
    fn declare(&mut self, name: &str, body: &Body) -> FunctionValue<'ctx> {
        let params: Vec<BasicMetadataTypeEnum> =
            (0..=body.arg_count).map(|_| self.ptr_ty().into()).collect();
        let ty = self.llcx.void_type().fn_type(&params, false);
        // A body with a by-value function keeps the plain symbol for it.
        let symbol = match self.value_sig(body) {
            Some(_) => format!("{}.ptr", fn_symbol(name)),
            None => fn_symbol(name),
        };
        let f = self
            .module
            .add_function(&symbol, ty, Some(Linkage::Internal));
        self.fns.insert(name.to_string(), f);
        f
    }

    /// The register type a value of `t` is passed in by value: `Some(None)`
    /// for a value with no bytes, a scalar or pointer as it is loaded, an
    /// integer for any other value of 1, 2, 4 or 8 bytes; `None` when it
    /// is passed in memory.
    fn reg_class(&self, t: Ty) -> Option<Option<BasicTypeEnum<'ctx>>> {
        let l = self.layout(t).ok()?;
        if l.size == 0 {
            return Some(None);
        }
        let lt = self.llvm_ty(t).ok()?;
        if !lt.is_array_type() {
            return Some(Some(lt));
        }
        matches!(l.size, 1 | 2 | 4 | 8).then(|| Some(self.int_ty(l.size as u32 * 8).into()))
    }

    /// The by-value signature of `body`, when every parameter and the
    /// result have a register class: each parameter's register type
    /// (`None` for one passed as nothing), then the result's.
    /// `KARAC_MIR_BYVALUE=0` passes everything in memory.
    #[allow(clippy::type_complexity)]
    pub(super) fn value_sig(
        &self,
        body: &Body,
    ) -> Option<(
        Vec<Option<BasicTypeEnum<'ctx>>>,
        Option<BasicTypeEnum<'ctx>>,
    )> {
        if body.arg_count > 8 || std::env::var("KARAC_MIR_BYVALUE").as_deref() == Ok("0") {
            return None;
        }
        let ret = self.reg_class(body.locals[0].ty)?;
        let params = (1..=body.arg_count)
            .map(|i| self.reg_class(body.locals[i].ty))
            .collect::<Option<Vec<_>>>()?;
        Some((params, ret))
    }

    /// The by-value function of the body `name`, declared now (and the
    /// body scheduled) when it has one.
    pub(super) fn value_function(&mut self, name: &str) -> R<Option<FunctionValue<'ctx>>> {
        if let Some(&f) = self.vfns.get(name) {
            return Ok(Some(f));
        }
        let program = self.program;
        let body = program
            .bodies
            .get(name)
            .ok_or_else(|| format!("no body named `{name}`"))?;
        let Some((params, ret)) = self.value_sig(body) else {
            return Ok(None);
        };
        let ps: Vec<BasicMetadataTypeEnum> = params.iter().flatten().map(|t| (*t).into()).collect();
        let ty = match ret {
            Some(r) => r.fn_type(&ps, false),
            None => self.llcx.void_type().fn_type(&ps, false),
        };
        let f = self
            .module
            .add_function(&fn_symbol(name), ty, Some(Linkage::Internal));
        self.vfns.insert(name.to_string(), f);
        self.schedule(body);
        Ok(Some(f))
    }

    fn schedule(&mut self, body: &'p Body) {
        if self.scheduled.insert(body.instance.name.clone()) {
            self.todo.push(body);
        }
    }

    /// Builds the pointer-convention function of the body `name` as a
    /// call of its by-value function: the arguments loaded from their
    /// slots, the result stored to its own.
    fn build_wrapper(&mut self, name: &str) -> R<()> {
        let program = self.program;
        let body = &program.bodies[name];
        let (params, ret) = self
            .value_sig(body)
            .ok_or("a wrapper of a body passed in memory")?;
        let v = self
            .value_function(name)?
            .ok_or("a wrapper of a body passed in memory")?;
        let f = self.fns[name];
        let always = self
            .llcx
            .create_enum_attribute(Attribute::get_named_enum_kind_id("alwaysinline"), 0);
        f.add_attribute(AttributeLoc::Function, always);
        let entry = self.llcx.append_basic_block(f, "entry");
        self.builder.position_at_end(entry);
        self.builder.unset_current_debug_location();
        let mut args: Vec<inkwell::values::BasicMetadataValueEnum> = Vec::new();
        for (i, t) in params.iter().enumerate() {
            if let Some(t) = t {
                let p = f
                    .get_nth_param(i as u32 + 1)
                    .ok_or("a parameter is missing")?
                    .into_pointer_value();
                let x = self
                    .builder
                    .build_load(*t, p, "")
                    .map_err(|e| e.to_string())?;
                if let Some(i) = x.as_instruction_value() {
                    let _ = i.set_alignment(1);
                }
                args.push(x.into());
            }
        }
        let r = self
            .builder
            .build_call(v, &args, "")
            .map_err(|e| e.to_string())?;
        if ret.is_some() {
            let out = f
                .get_nth_param(0)
                .ok_or("the result slot is missing")?
                .into_pointer_value();
            let x = r.try_as_basic_value().unwrap_basic();
            let st = self
                .builder
                .build_store(out, x)
                .map_err(|e| e.to_string())?;
            let _ = st.set_alignment(1);
        }
        self.builder.build_return(None).map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Marks every pointer parameter `noalias` on the bodies only direct
    /// calls reach: each such call passes slots of its own (`call_body`),
    /// so nothing else in the callee reaches them, which lets LLVM pass a
    /// scalar argument in a register instead of through memory.
    fn mark_noalias(&self) {
        if !noalias_flag() {
            return;
        }
        let noalias = self
            .llcx
            .create_enum_attribute(Attribute::get_named_enum_kind_id("noalias"), 0);
        for (name, f) in &self.fns {
            if self.shared_fns.contains(name) {
                continue;
            }
            for i in 0..f.count_params() {
                f.add_attribute(AttributeLoc::Param(i), noalias);
            }
        }
    }

    /// Tells LLVM that each `ref T` / `mut ref T` parameter points at a
    /// whole live `T` (`llvm.assume` with `dereferenceable` and `align`
    /// bundles), which a borrow guarantees for the whole call. Unlike a
    /// parameter attribute the assume survives inlining, so a load
    /// through the reference that only some iterations reach (a `Vec`'s
    /// data pointer behind an `if`) can still be hoisted out of a loop.
    /// `KARAC_MIR_DEREF_ASSUME=0` leaves them out.
    fn assume_ref_params(&mut self, body: &Body) -> R<()> {
        use inkwell::types::AsTypeRef;
        use inkwell::values::AsValueRef;
        use llvm_sys::core::{
            LLVMBuildCallWithOperandBundles, LLVMCreateOperandBundle, LLVMDisposeOperandBundle,
        };
        if std::env::var("KARAC_MIR_DEREF_ASSUME").as_deref() == Ok("0") {
            return Ok(());
        }
        let i64t = self.llcx.i64_type();
        for i in 1..=body.arg_count {
            let ty = body.locals[i].ty;
            let (TyKind::Ref(t) | TyKind::MutRef(t)) = self.tys.kind(ty) else {
                continue;
            };
            // A `str` or slice reference is a view, not a thin pointer.
            if matches!(self.tys.kind(t), TyKind::Str | TyKind::Slice(_)) {
                continue;
            }
            if self.layout(ty)?.size != 8 {
                continue;
            }
            let l = self.layout(t)?;
            if l.size == 0 {
                continue;
            }
            let p = self
                .builder
                .build_load(self.ptr_ty(), self.locals[i], "")
                .map_err(|e| e.to_string())?
                .into_pointer_value();
            let assume = self.module.get_function("llvm.assume").unwrap_or_else(|| {
                let ty = self
                    .llcx
                    .void_type()
                    .fn_type(&[self.llcx.bool_type().into()], false);
                self.module.add_function("llvm.assume", ty, None)
            });
            let mut deref = [
                p.as_value_ref(),
                i64t.const_int(l.size, false).as_value_ref(),
            ];
            let mut align = [
                p.as_value_ref(),
                i64t.const_int(l.align.max(1), false).as_value_ref(),
            ];
            let mut arg = [self.llcx.bool_type().const_int(1, false).as_value_ref()];
            // SAFETY: every value comes from this module's context, and each
            // bundle is disposed after the call that copies it.
            unsafe {
                let mut bundles = [
                    LLVMCreateOperandBundle(c"dereferenceable".as_ptr(), 15, deref.as_mut_ptr(), 2),
                    LLVMCreateOperandBundle(c"align".as_ptr(), 5, align.as_mut_ptr(), 2),
                ];
                LLVMBuildCallWithOperandBundles(
                    self.builder.as_mut_ptr(),
                    assume.get_type().as_type_ref(),
                    assume.as_value_ref(),
                    arg.as_mut_ptr(),
                    1,
                    bundles.as_mut_ptr(),
                    2,
                    c"".as_ptr(),
                );
                for b in bundles {
                    LLVMDisposeOperandBundle(b);
                }
            }
        }
        Ok(())
    }

    /// Marks the back edge `br` `!llvm.loop !{!self,
    /// !{!"llvm.loop.unroll.full"}}`, the self-referential loop id built
    /// through a temporary node, as the legacy backend does
    /// (`attach_unroll_full_metadata`).
    fn unroll_full(&self, br: inkwell::values::InstructionValue<'ctx>) {
        use inkwell::values::AsValueRef;
        use llvm_sys::core::{
            LLVMGetMDKindIDInContext, LLVMMDNodeInContext2, LLVMMDStringInContext2,
            LLVMMetadataAsValue, LLVMSetMetadata,
        };
        use llvm_sys::debuginfo::{LLVMMetadataReplaceAllUsesWith, LLVMTemporaryMDNode};
        use std::os::raw::{c_char, c_uint};
        let ctx = self.llcx.raw();
        // SAFETY: every node is made in `ctx`, which outlives the module,
        // and the temporary is replaced (and so freed) before it escapes.
        unsafe {
            let key = b"llvm.loop.unroll.full";
            let prop = LLVMMDStringInContext2(ctx, key.as_ptr() as *const c_char, key.len());
            let mut prop_ops = [prop];
            let prop = LLVMMDNodeInContext2(ctx, prop_ops.as_mut_ptr(), prop_ops.len());
            let temp = LLVMTemporaryMDNode(ctx, std::ptr::null_mut(), 0);
            let mut ops = [temp, prop];
            let id = LLVMMDNodeInContext2(ctx, ops.as_mut_ptr(), ops.len());
            LLVMMetadataReplaceAllUsesWith(temp, id);
            let kind = b"llvm.loop";
            let kind =
                LLVMGetMDKindIDInContext(ctx, kind.as_ptr() as *const c_char, kind.len() as c_uint);
            LLVMSetMetadata(br.as_value_ref(), kind, LLVMMetadataAsValue(ctx, id));
        }
    }

    /// Declares every body without lowering any, for tests that write
    /// bodies by hand.
    #[cfg(test)]
    fn declare_bodies(&mut self) -> R<()> {
        for (name, body) in &self.program.bodies {
            self.declare(name, body);
        }
        Ok(())
    }

    /// The function of the body `name`, declared now and lowered by
    /// [`Cx::drain`] if it was not already: only the bodies a program
    /// reaches are compiled, since the library's are appended to every one.
    pub fn function(&mut self, name: &str) -> R<FunctionValue<'ctx>> {
        self.shared_fns.insert(name.to_string());
        self.direct_function(name)
    }

    /// [`Cx::function`] for a direct call, which passes a fresh slot for
    /// the result and for each argument.
    pub(super) fn direct_function(&mut self, name: &str) -> R<FunctionValue<'ctx>> {
        if let Some(&f) = self.fns.get(name) {
            return Ok(f);
        }
        let program = self.program;
        let body = program
            .bodies
            .get(name)
            .ok_or_else(|| format!("no body named `{name}`"))?;
        let f = self.declare(name, body);
        if self.value_sig(body).is_some() {
            self.wrap_todo.push(name.to_string());
        }
        self.schedule(body);
        Ok(f)
    }

    /// Lowers every requested body and builds every requested glue, until
    /// neither requests more.
    fn drain(&mut self) -> R<()> {
        loop {
            if let Some(body) = self.todo.pop() {
                self.lower_body(body)?;
            } else if let Some(name) = self.wrap_todo.pop() {
                self.build_wrapper(&name)?;
            } else if self.glue_pending() {
                self.finish_glue()?;
            } else if self.fmt_pending() {
                self.finish_fmt()?;
            } else if self.clone_pending() {
                self.finish_clones()?;
            } else if self.keys_pending() {
                self.finish_keys()?;
            } else if self.shims_pending() {
                self.finish_shims()?;
            } else {
                return Ok(());
            }
        }
    }

    /// Lowers `body` into its declared function: an `alloca` per local
    /// that is not the return place or an argument, an LLVM block per MIR
    /// block, then each block's statements and terminator.
    fn lower_body(&mut self, body: &'p Body) -> R<()> {
        let sig = self.value_sig(body);
        let f = match self.value_function(&body.instance.name)? {
            Some(v) => v,
            None => self.direct_function(&body.instance.name)?,
        };
        self.ret_by_value = sig.as_ref().map(|(_, r)| *r);
        // A small body (an iterator's `next`, an accessor) is hinted for
        // inlining, which lifts O2's inline threshold for it from 225 to 325:
        // with every value passed by pointer, a body this size can cost
        // just over 225 and stay a call in the loop that runs it.
        // `KARAC_MIR_INLINEHINT=0` leaves the hint out.
        // Storage markers and `()` stores emit nothing, so only the rest
        // count.
        let statements: usize = body
            .blocks
            .iter()
            .flat_map(|b| &b.statements)
            .filter(|st| match &st.kind {
                crate::mir::syntax::StatementKind::StorageLive(_)
                | crate::mir::syntax::StatementKind::StorageDead(_)
                | crate::mir::syntax::StatementKind::Nop => false,
                crate::mir::syntax::StatementKind::Assign(p, _) => {
                    !matches!(self.layout(crate::mir::place_ty::place_ty(body, self.tys, p).map(|t| t.ty).unwrap_or(self.tys.unit())), Ok(l) if l.size == 0)
                }
                _ => true,
            })
            .count();
        // An overflow check's abort, and a block that only jumps on, cost
        // nothing on the path that runs, so they do not count against it.
        let blocks = body
            .blocks
            .iter()
            .filter(|b| match b.terminator.kind {
                TerminatorKind::Abort { .. } | TerminatorKind::Unreachable => false,
                TerminatorKind::Goto { .. } => !b.statements.is_empty(),
                _ => true,
            })
            .count();
        if blocks <= INLINE_HINT_MAX_BLOCKS
            && statements <= INLINE_HINT_MAX_STATEMENTS
            && std::env::var("KARAC_MIR_INLINEHINT").as_deref() != Ok("0")
        {
            let kind = Attribute::get_named_enum_kind_id("inlinehint");
            let hint = self.llcx.create_enum_attribute(kind, 0);
            f.add_attribute(AttributeLoc::Function, hint);
        }
        let entry = self.llcx.append_basic_block(f, "entry");
        self.builder.position_at_end(entry);
        let name = &body.instance.name;
        let start = self.body_start(body);
        self.debug_enter(name, &fn_symbol(name), start, f);
        self.body = Some(body);
        self.locals.clear();
        self.abort_blocks.clear();
        // A by-value body's parameters, in order, skipping those passed as
        // nothing.
        let mut incoming = f.get_param_iter();
        for (i, decl) in body.locals.iter().enumerate() {
            let slot = if i <= body.arg_count && sig.is_none() {
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
            if let Some((params, _)) = &sig {
                if (1..=body.arg_count).contains(&i) && params[i - 1].is_some() {
                    let v = incoming.next().ok_or("a parameter is missing")?;
                    let st = self
                        .builder
                        .build_store(slot, v)
                        .map_err(|e| e.to_string())?;
                    let _ = st.set_alignment(1);
                }
            }
            self.locals.push(slot);
        }
        self.assume_ref_params(body)?;
        self.flag_slots()?;
        self.blocks = (0..body.blocks.len())
            .map(|i| self.llcx.append_basic_block(f, &format!("bb{i}")))
            .collect();
        self.builder
            .build_unconditional_branch(self.blocks[0])
            .map_err(|e| e.to_string())?;
        // `KARAC_MIR_UNROLL=0` leaves out the full-unroll hints.
        let unroll = if std::env::var("KARAC_MIR_UNROLL").as_deref() == Ok("0") {
            Vec::new()
        } else {
            crate::mir::counters::unroll_full_latches(body, self.tys)
        };
        for (i, block) in body.blocks.iter().enumerate() {
            self.builder.position_at_end(self.blocks[i]);
            for s in &block.statements {
                self.locate(s.source_info.span);
                self.statement(s)?;
            }
            self.locate(block.terminator.source_info.span);
            self.note_call_site(&block.terminator.kind)?;
            match &block.terminator.kind {
                TerminatorKind::Drop { place, target, .. } => {
                    self.drop_terminator(place, *target)?
                }
                kind => self.terminator(kind)?,
            }
            if unroll.contains(&i) && matches!(block.terminator.kind, TerminatorKind::Goto { .. }) {
                if let Some(br) = self
                    .builder
                    .get_insert_block()
                    .and_then(|b| b.get_terminator())
                {
                    self.unroll_full(br);
                }
            }
        }
        self.debug_leave();
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

    /// A C `i32 main()` that runs the Kāra `main` and returns 0. A
    /// coroutine `main` is a task root: its frame lives in this function,
    /// zeroed (state 0, before the first resume), and the runtime's task
    /// executor resumes it, with the tasks it spawns, until all return.
    fn c_main(&mut self) -> R<()> {
        let resume = coroutine::resume_name("main");
        let bodies = &self.program.bodies;
        let (name, task) = match (bodies.contains_key("main"), bodies.contains_key(&resume)) {
            (true, _) => ("main", false),
            (false, true) => (resume.as_str(), true),
            (false, false) => return Ok(()),
        };
        let kmain = self.function(name)?;
        let body = &self.program.bodies[name];
        if body.arg_count != usize::from(task) {
            return Err("`main` takes arguments".into());
        }
        let i32t = self.llcx.i32_type();
        let f = self
            .module
            .add_function("karac.main", i32t.fn_type(&[], false), None);
        self.builder
            .position_at_end(self.llcx.append_basic_block(f, "entry"));
        let start = self.body_start(body);
        self.debug_enter("karac.main", "karac.main", start, f);
        self.init_statics()?;
        // The program's `#[panic_handler]` runs through its library thunk,
        // which the runtime calls on a panic (core semantics §10.2).
        if self
            .program
            .bodies
            .contains_key(crate::mir::lower::PANIC_THUNK)
        {
            let thunk = self.function(crate::mir::lower::PANIC_THUNK)?;
            self.rt(
                "set_panic_handler",
                &[thunk.as_global_value().as_pointer_value().into()],
            )?;
        }
        // A static's initializer may be lowered meanwhile, leaving no
        // location set.
        self.debug_at(start);
        let ret_ty = body.return_ty();
        let ret = self.c_main_slot(ret_ty, "ret")?;
        if !task {
            self.builder
                .build_call(kmain, &[ret.into()], "")
                .map_err(|e| e.to_string())?;
        } else {
            let TyKind::MutRef(frame_ty) = self.tys.kind(body.locals[1].ty) else {
                return Err(format!("{name} does not take its frame"));
            };
            let frame = self.c_main_slot(frame_ty, "frame")?;
            let size = self.layout(frame_ty)?.size;
            self.builder
                .build_memset(frame, 8, self.llcx.i8_type().const_zero(), self.i64(size))
                .map_err(|e| e.to_string())?;
            // The resume body's argument slot holds the frame reference.
            let arg = self.c_main_slot(body.locals[1].ty, "frame.ref")?;
            self.builder
                .build_store(arg, frame)
                .map_err(|e| e.to_string())?;
            // The executor runs `main` as its root task, and every task
            // spawned meanwhile, until all have returned.
            let poll = self.task_poll(&resume, false);
            let p = self.ptr_ty();
            let run = self.extern_fn(
                "karac_mir_task_run",
                self.llcx
                    .void_type()
                    .fn_type(&[p.into(), p.into(), p.into()], false),
            );
            self.builder
                .build_call(
                    run,
                    &[
                        poll.as_global_value().as_pointer_value().into(),
                        arg.into(),
                        ret.into(),
                    ],
                    "",
                )
                .map_err(|e| e.to_string())?;
        }
        // What `main` returned is dropped before the process exits; a
        // returned frame holds nothing.
        let (vt, vp) = if task {
            let t = self
                .tys
                .field_ty(ret_ty, Some(0), 0)
                .ok_or("a `Poll` without a payload")?;
            (t, self.offset(ret, self.field_offset(ret_ty, Some(0), 0)?)?)
        } else {
            (ret_ty, ret)
        };
        let code = self.main_exit_code(vt, vp)?;
        if self.tys.needs_drop(ret_ty) {
            let glue = self.glue_fn(ret_ty);
            self.builder
                .build_call(glue, &[ret.into()], "")
                .map_err(|e| e.to_string())?;
        }
        self.rt("flush", &[])?;
        self.builder
            .build_return(Some(&code))
            .map_err(|e| e.to_string())?;
        self.debug_leave();
        // The C `main` runs that body on a thread with a deep stack, as the
        // interpreter's recursion is not bounded by the process's stack.
        let fty = i32t.fn_type(&[], false);
        let ptr = self.llcx.ptr_type(inkwell::AddressSpace::default());
        let run = self.extern_fn("karac_mir_run_main", i32t.fn_type(&[ptr.into()], false));
        let cmain = self.module.add_function("main", fty, None);
        self.builder
            .position_at_end(self.llcx.append_basic_block(cmain, "entry"));
        let code = self
            .builder
            .build_call(run, &[f.as_global_value().as_pointer_value().into()], "")
            .map_err(|e| e.to_string())?
            .try_as_basic_value()
            .unwrap_basic();
        self.builder
            .build_return(Some(&code))
            .map_err(|e| e.to_string())?;
        // On wasm32-wasip1, `_start` enters at `__main_void`, whose libc
        // definition chains to the `__main_argc_argv` clang mints for a C
        // `main`; defining it here calls `main` instead (B-2026-10-22-23).
        if crate::target::active_target_is_wasm() {
            let shim = self.module.add_function("__main_void", fty, None);
            self.builder
                .position_at_end(self.llcx.append_basic_block(shim, "entry"));
            let code = self
                .builder
                .build_call(cmain, &[], "")
                .map_err(|e| e.to_string())?
                .try_as_basic_value()
                .unwrap_basic();
            self.builder
                .build_return(Some(&code))
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    /// The process's exit code for `main` returning the `t` at `p`, as the
    /// interpreter's `main_result` has it: an `ExitCode`'s own code, and 1
    /// for an `Err`, whose error first goes to stderr as `Error: {e}`.
    fn main_exit_code(&mut self, t: Ty, p: PointerValue<'ctx>) -> R<IntValue<'ctx>> {
        let i32t = self.llcx.i32_type();
        let TyKind::Adt(a) = self.tys.kind(t) else {
            return Ok(i32t.const_zero());
        };
        let name = self.tys.adt(a).name.clone();
        if name == "ExitCode" {
            // The code's own width (`u8`), not a word: read as an `i64` it
            // ran past the slot.
            let ft = self
                .tys
                .field_ty(t, None, 0)
                .ok_or("ExitCode has no field")?;
            let n = self
                .load(self.offset(p, self.field_offset(t, None, 0)?)?, ft)?
                .into_int_value();
            return self
                .builder
                .build_int_cast_sign_flag(n, i32t, false, "")
                .map_err(|e| e.to_string());
        }
        if name != "Result" {
            return Ok(i32t.const_zero());
        }
        let er = self.variant_of(t, "Err")?;
        let et = self
            .tys
            .field_ty(t, Some(er), 0)
            .ok_or("Result.Err has no payload")?;
        let tag = self.load_tag(p, t)?;
        let is_err = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                tag,
                tag.get_type().const_int(u64::from(er), false),
                "",
            )
            .map_err(|e| e.to_string())?;
        let off = self.field_offset(t, Some(er), 0)?;
        self.if_then(is_err, |cx| {
            cx.rt("fmt_begin", &[])?;
            let (s, n) = cx.str_const("Error: ")?;
            cx.rt("fmt_str", &[s.into(), n.into()])?;
            cx.fmt_call(cx.offset(p, off)?, et)?;
            cx.rt("print_end", &[i32t.const_int(3, false).into()])?;
            cx.rt("error_trace_print", &[])
        })?;
        self.builder
            .build_int_z_extend(is_err, i32t, "")
            .map_err(|e| e.to_string())
    }

    /// The global holding static `i`, zeroed until its initializer runs.
    pub(super) fn static_ptr(&mut self, i: u32) -> R<PointerValue<'ctx>> {
        if let Some(&g) = self.statics.get(&i) {
            return Ok(g);
        }
        let def = self
            .program
            .statics
            .get(i as usize)
            .ok_or_else(|| format!("no static {i}"))?;
        let l = self.layout(def.ty)?;
        let ty = self.llcx.i8_type().array_type(l.size.max(1) as u32);
        let g = self
            .module
            .add_global(ty, None, &format!("kara.static.{}", def.name));
        g.set_initializer(&ty.const_zero());
        g.set_alignment(l.align.max(8) as u32);
        let p = g.as_pointer_value();
        self.statics.insert(i, p);
        Ok(p)
    }

    /// Runs each static's initializer into its global, in declaration
    /// order, at the builder's position (the start of the C `main`).
    fn init_statics(&mut self) -> R<()> {
        let program = self.program;
        for (i, def) in program.statics.iter().enumerate() {
            let g = self.static_ptr(i as u32)?;
            let f = self.function(&def.init)?;
            self.builder
                .build_call(f, &[g.into()], "")
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    /// An 8-aligned stack slot holding a `t`, at the builder's position.
    pub(super) fn c_main_slot(&self, t: Ty, name: &str) -> R<PointerValue<'ctx>> {
        let size = self.layout(t)?.size.max(1);
        let a = self
            .builder
            .build_alloca(self.llcx.i8_type().array_type(size as u32), name)
            .map_err(|e| e.to_string())?;
        a.as_instruction()
            .ok_or("an alloca is not an instruction")?
            .set_alignment(8)
            .map_err(|e| e.to_string())?;
        Ok(a)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mir::parse_module;
    use inkwell::targets::{InitializationConfig, Target};
    use inkwell::values::GlobalValue;
    use inkwell::OptimizationLevel;
    use std::sync::atomic::{AtomicI64, Ordering};

    /// A coroutine `main`: it yields once.
    const TASK: &str = "
fn main() -> () {
    let mut _0: ();
    let _1: ();
    bb0: {
        _1 = __yield_now() -> bb1;
    }
    bb1: {
        return;
    }
}
";

    fn counter<'c>(cx: &Cx<'c, '_>, name: &str) -> GlobalValue<'c> {
        // Declared here and mapped onto a Rust static by the test.
        cx.module.add_global(cx.llcx.i64_type(), None, name)
    }

    /// `*g += by`, at the builder's position.
    fn bump(cx: &Cx<'_, '_>, g: GlobalValue<'_>, by: IntValue<'_>) -> IntValue<'static> {
        let i64t = cx.llcx.i64_type();
        let p = g.as_pointer_value();
        let old = cx.builder.build_load(i64t, p, "").unwrap().into_int_value();
        let new = cx.builder.build_int_add(old, by, "").unwrap();
        cx.builder.build_store(p, new).unwrap();
        // SAFETY: the value lives as long as the test's context.
        unsafe { std::mem::transmute::<IntValue<'_>, IntValue<'static>>(new) }
    }

    static RESUMES: AtomicI64 = AtomicI64::new(0);
    static STATES: AtomicI64 = AtomicI64::new(0);
    static DROPS: AtomicI64 = AtomicI64::new(0);

    fn read(a: &AtomicI64) -> i64 {
        a.load(Ordering::SeqCst)
    }

    /// The C `main` of a coroutine `main` starts its frame zeroed and hands
    /// the executor a poll function that resumes it until `Ready`; dropping
    /// a frame runs its `drop_frame`.
    #[test]
    fn mir_llvm_c_main_drives_a_coroutine_main() {
        Target::initialize_native(&InitializationConfig::default()).unwrap();
        let m = parse_module(TASK).unwrap();
        let prog = interp::with_coroutines(&Program::from_module(&m), &m.tys).unwrap();
        assert!(!prog.bodies.contains_key("main"));
        let c = Context::create();
        let mut cx = Cx::new(&c, "task", &prog, &m.tys);
        cx.declare_bodies().unwrap();
        let (resumes, states, drops) = (
            counter(&cx, "resumes"),
            counter(&cx, "states"),
            counter(&cx, "drops"),
        );
        let i64t = c.i64_type();

        // `main#resume` by hand (its body is `body.rs`'s): `Pending` on the
        // first two calls, `Ready` on the third, adding up the states it
        // is handed.
        let resume = cx.function("main#resume").unwrap();
        cx.builder
            .position_at_end(c.append_basic_block(resume, "entry"));
        let n = bump(&cx, resumes, i64t.const_int(1, false));
        let slot = resume.get_nth_param(1).unwrap().into_pointer_value();
        let frame = cx
            .builder
            .build_load(cx.ptr_ty(), slot, "")
            .unwrap()
            .into_pointer_value();
        let state = cx
            .builder
            .build_load(c.i32_type(), frame, "")
            .unwrap()
            .into_int_value();
        let state = cx.builder.build_int_z_extend(state, i64t, "").unwrap();
        bump(&cx, states, state);
        let done = cx
            .builder
            .build_int_compare(IntPredicate::UGE, n, i64t.const_int(3, false), "")
            .unwrap();
        let tag = cx
            .builder
            .build_select(
                done,
                c.i8_type().const_zero(),
                c.i8_type().const_int(1, false),
                "",
            )
            .unwrap();
        let ret = resume.get_nth_param(0).unwrap().into_pointer_value();
        cx.builder.build_store(ret, tag).unwrap();
        // Advance the state, as a real resume does.
        cx.builder
            .build_store(frame, c.i32_type().const_int(1, false))
            .unwrap();
        cx.builder.build_return(None).unwrap();

        let drop_frame = cx.function("main#drop_frame").unwrap();
        cx.builder
            .position_at_end(c.append_basic_block(drop_frame, "entry"));
        bump(&cx, drops, i64t.const_int(1, false));
        cx.builder.build_return(None).unwrap();

        for f in cx.fns.values() {
            if f.count_basic_blocks() == 0 {
                cx.builder
                    .position_at_end(c.append_basic_block(*f, "entry"));
                cx.builder.build_unreachable().unwrap();
            }
        }
        cx.c_main().unwrap();
        // The root task's poll function.
        cx.finish_shims().unwrap();
        let body = &prog.bodies["main#resume"];
        let TyKind::MutRef(frame_ty) = m.tys.kind(body.locals[1].ty) else {
            panic!("main#resume does not take its frame");
        };
        let glue = cx.glue_fn(frame_ty);
        let glue_name = glue.get_name().to_str().unwrap().to_string();
        cx.finish_glue().unwrap();
        cx.module.verify().unwrap();
        let engine = cx
            .module
            .create_jit_execution_engine(OptimizationLevel::None)
            .unwrap();
        for (g, a) in [(resumes, &RESUMES), (states, &STATES), (drops, &DROPS)] {
            engine.add_global_mapping(&g, a as *const AtomicI64 as usize);
        }
        // The runtime's output buffer is not linked into this process.
        extern "C" fn flush() {}
        let f = cx.module.get_function("karac_mir_flush").unwrap();
        engine.add_global_mapping(&f, flush as *const () as usize);
        extern "C" fn run_main(body: extern "C" fn() -> i32) -> i32 {
            body()
        }
        let f = cx.module.get_function("karac_mir_run_main").unwrap();
        engine.add_global_mapping(&f, run_main as *const () as usize);
        // The executor, with only the root task: polled until it returns.
        extern "C" fn task_run(
            poll: extern "C" fn(*mut u8, *mut u8) -> u8,
            a: *mut u8,
            b: *mut u8,
        ) {
            while poll(a, b) == 0 {}
        }
        let f = cx.module.get_function("karac_mir_task_run").unwrap();
        engine.add_global_mapping(&f, task_run as *const () as usize);

        // SAFETY: `main` is the module's `i32 main()`.
        let main: extern "C" fn() -> i32 =
            unsafe { std::mem::transmute(engine.get_function_address("main").unwrap()) };
        assert_eq!(main(), 0);
        assert_eq!(read(&RESUMES), 3);
        // Zeroed before the first resume, then 1 as the resume left it.
        assert_eq!(read(&STATES), 2);
        assert_eq!(read(&DROPS), 0);

        let size = layout::layout(&m.tys, frame_ty).unwrap().size as usize;
        let mut buf = vec![0u64; size.div_ceil(8).max(1)];
        // SAFETY: the glue is a `void(ptr)` over a frame of `size` bytes.
        let g: extern "C" fn(*mut u64) =
            unsafe { std::mem::transmute(engine.get_function_address(&glue_name).unwrap()) };
        g(buf.as_mut_ptr());
        assert_eq!(read(&DROPS), 1);
    }
}
