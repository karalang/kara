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
mod dyn_trait;
mod flag;
mod fmt;
mod foreign;
mod hot_swap;
mod inline_budget;
mod io;
mod key;
mod log;
mod num;
mod pool;
mod seq;
mod sort_order;
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
use inkwell::types::{BasicMetadataTypeEnum, BasicType, BasicTypeEnum, FunctionType, IntType};
use inkwell::values::{BasicMetadataValueEnum, BasicValue, FunctionValue, IntValue, PointerValue};
use inkwell::AddressSpace;
use inkwell::IntPredicate;

use crate::mir::coroutine;
use crate::mir::interp::{self, Program};
use crate::mir::layout::{self, Layout};
use crate::mir::ty::{FloatTy, IntrinsicTy, TyKind};
use crate::mir::{
    AggregateKind, Body, Const, ConstKind, Operand, Place, Rvalue, StatementKind, TerminatorKind,
    Ty, TyInterner,
};

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
    compile_mir_with(llcx, name, program, tys, &[])
}

/// [`compile_mir`] with `--enable-hot-swap` (deferred.md § Continuous PGO
/// with Shared-Object Hot-Swap): each body named in `hot_swap` gets a slot
/// in `karac_hotswap_table`, which a constructor fills with its function,
/// and every direct call of it loads its slot and calls through it. With
/// none named the module is [`compile_mir`]'s.
pub fn compile_mir_with<'ctx>(
    llcx: &'ctx Context,
    name: &str,
    program: &Program,
    tys: &TyInterner,
    hot_swap: &[String],
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
    cx.hot_swap_init(hot_swap)?;
    cx.c_main()?;
    cx.drain()?;
    cx.mark_noalias();
    cx.hot_swap_finish()?;
    cx.debug_finish();
    super::driver::mark_frame_pointers(llcx, &cx.module);
    cx.module.verify().map_err(|e| e.to_string())?;
    Ok(cx.module)
}

/// The named metadata marking a module with a binary-search midpoint
/// stated (`Cx::assume_midpoint`), whose passes then run twice.
pub const MIDPOINT_MARK: &str = "karac.midpoint";

/// Whether a by-value function's `ref T` parameters of a frozen `T` are
/// marked `noalias` (`Cx::mark_deref_params`); `KARAC_MIR_NOALIAS_REF=0`
/// turns it off.
pub fn noalias_ref_flag() -> bool {
    std::env::var("KARAC_MIR_NOALIAS_REF").as_deref() != Ok("0")
}

/// Whether a by-value function's `mut ref T` parameters of a `T` that
/// takes a borrow flag are marked `noalias` (`Cx::mark_deref_params`);
/// `KARAC_MIR_NOALIAS_MUTREF=0` turns it off.
pub fn noalias_mutref_flag() -> bool {
    std::env::var("KARAC_MIR_NOALIAS_MUTREF").as_deref() != Ok("0")
}

/// Whether direct-call parameters are marked `noalias` and promoted
/// (`Cx::mark_noalias`); `KARAC_MIR_NOALIAS=0` turns it off.
pub fn noalias_flag() -> bool {
    std::env::var("KARAC_MIR_NOALIAS").as_deref() != Ok("0")
}

/// The least value of the signed integer type `t`: its sign bit alone.
/// (LLVM 19 took away the constant `shl` this was built with.)
pub(super) fn signed_min(t: IntType<'_>) -> IntValue<'_> {
    let w = t.get_bit_width();
    if w <= 64 {
        t.const_int(1u64 << (w - 1), false)
    } else {
        let mut words = vec![0u64; w.div_ceil(64) as usize];
        words[((w - 1) / 64) as usize] = 1u64 << ((w - 1) % 64);
        t.const_int_arbitrary_precision(&words)
    }
}

/// Whether a body whose result has no register class keeps its
/// parameters in registers, taking a pointer to the result first
/// (`Cx::ret_in_memory`); `KARAC_MIR_SRET_REGS=0` passes it all in memory.
pub fn sret_flag() -> bool {
    std::env::var("KARAC_MIR_SRET_REGS").as_deref() != Ok("0")
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
    let (module, passes) = jit_module(&llcx, program, tys)?;
    jit_ir(&module, passes)
}

/// What `karac_jit_runner` loads for a program: its IR, or, for a program
/// large enough to split, one object per codegen unit.
pub enum JitCode {
    /// The IR text, which the runner compiles on one thread.
    Ir(String),
    /// Objects compiled in parallel here (B-2026-11-10-5), which the runner
    /// only links. The caller removes the files.
    Objects(Vec<std::path::PathBuf>),
}

/// [`compile_mir_ir_for_jit`], except that a program the build would split
/// into codegen units is compiled here, one unit per core, into objects
/// named after `stem` (`mir_aot::compile_jit_units`). The JIT compiles a
/// module on one thread: mini_tldextract's run took 92 s where its build
/// took 16 s over four cores. `KARAC_MIR_LL`, which keeps the IR, keeps
/// the IR path.
pub fn compile_mir_for_jit(
    program: &Program,
    tys: &TyInterner,
    stem: &std::path::Path,
) -> R<JitCode> {
    let llcx = Context::create();
    let (module, passes) = jit_module(&llcx, program, tys)?;
    if std::env::var_os("KARAC_MIR_LL").is_none() {
        if let Some(parts) = super::mir_aot::compile_jit_units(&module, passes, stem)? {
            return Ok(JitCode::Objects(parts));
        }
    }
    jit_ir(&module, passes).map(JitCode::Ir)
}

/// The JIT's module, laid out for the host, with the passes it is to run.
fn jit_module<'ctx>(
    llcx: &'ctx Context,
    program: &Program,
    tys: &TyInterner,
) -> R<(Module<'ctx>, Option<&'static str>)> {
    let module = compile_mir(llcx, "main", program, tys)?;
    // The host's layout, as the AOT path stamps it (`mir_aot`). Without it
    // the passes below read the IR under LLVM's default layout, where an
    // `i64` is 4-aligned: a register pair `{ i8, i64 }` then has its `i64`
    // at byte 4, not the 8 every load of the tuple uses, and SROA splits
    // the pair wrongly (mini_astral's `parse_tz` offset read as garbage).
    let tm = super::driver::create_target_machine()?;
    module.set_triple(&tm.get_triple());
    module.set_data_layout(&tm.get_target_data().get_data_layout());
    if std::env::var("KARAC_MIR_JIT_SROA").as_deref() == Ok("0") {
        return Ok((module, None));
    }
    // Inlining as well: every argument is passed by pointer, and a
    // small callee left out of line costs its loads and stores at each
    // call. On kata 3629's bench (release build) the run drops from
    // 24.5 s to 8.1 s (legacy's JIT: 7.2 s), at a compile cost the
    // largest programs feel: the 20,000-line mini_pyasn1 app takes
    // 10.5 s to start where it took 7.6 s.
    // `KARAC_MIR_JIT_INLINE=0` leaves it at `sroa`.
    if std::env::var("KARAC_MIR_JIT_INLINE").as_deref() == Ok("0") {
        return Ok((module, Some("function(sroa)")));
    }
    // A function the inliner would grow past what code generation handles
    // in reasonable time keeps its largest single-call callees out of line
    // (B-2026-11-10-5).
    let mut t = std::time::Instant::now();
    // SAFETY: the module is live and this thread's alone.
    let kept = unsafe { inline_budget::cap_inline_growth(module.as_mut_ptr()) };
    crate::mir::lower::phase_time(&format!("inline budget, {kept} kept out"), &mut t);
    Ok((
        module,
        Some(
            "function(sroa),cgscc(inline),function(sroa,instcombine<no-verify-fixpoint>,simplifycfg)",
        ),
    ))
}

/// The JIT module's IR text after `passes`.
fn jit_ir(module: &Module<'_>, passes: Option<&str>) -> R<String> {
    let Some(passes) = passes else {
        return Ok(module.print_to_string().to_string());
    };
    let tm = super::driver::create_target_machine()?;
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
    /// The `shared` types some `weak` type names, whose boxes carry a weak
    /// count after the strong one; every other box has the strong count
    /// alone ([`Cx::shared_header`]). Taken once, before any code, so each
    /// type keeps one layout.
    pub weak_shared: rustc_hash::FxHashSet<crate::mir::ty::AdtId>,
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
    /// Whether each body is located, once asked ([`Cx::located`]).
    located: HashMap<String, bool>,
    /// The function of each located body: its by-value or pointer
    /// convention, then the place of the call into it as two trailing
    /// parameters (`debug.rs`). Its `fns` and `vfns` functions, when
    /// something asks for one, wrap it.
    lfns: HashMap<String, FunctionValue<'ctx>>,
    /// Wrappers of `lfns` declared and not built yet, `true` for a
    /// by-value one.
    site_todo: Vec<(String, bool)>,
    /// The function being lowered with its place parameters, when it is
    /// one of `lfns`.
    site_params: Option<(FunctionValue<'ctx>, PointerValue<'ctx>, IntValue<'ctx>)>,
    /// `--enable-hot-swap`: the slot of each registered body in
    /// `karac_hotswap_table`, with the function the slot holds
    /// ([`compile_mir_with`]).
    hot_swap: HashMap<String, (u32, FunctionValue<'ctx>)>,
    /// The table, when a body is registered.
    hot_swap_table: Option<inkwell::values::GlobalValue<'ctx>>,
    /// Each call made through a slot, with the function the slot holds,
    /// whose attributes and calling convention the call takes once they
    /// are final ([`Cx::hot_swap_finish`]).
    hot_swap_calls: Vec<(inkwell::values::CallSiteValue<'ctx>, FunctionValue<'ctx>)>,
    /// While an index call whose upper bound a guard proved is lowered
    /// ([`crate::mir::guard_bounds`]): its check is `i >= 0` alone.
    upper_proven: bool,
    /// The locals of the body being lowered that hold a length
    /// ([`crate::mir::guard_bounds::length_locals`]).
    lengths: Vec<bool>,
    /// For each local of the body being lowered that a char's
    /// `to_string()` fills only to be lent, the frame's 4-byte buffer its
    /// UTF-8 bytes go in ([`crate::mir::char_strs`], B-2026-11-10-25).
    char_bufs: Vec<Option<PointerValue<'ctx>>>,
    /// The locals of the body being lowered that hold a checked result no
    /// one tests ([`crate::mir::range_bounds::untested_checks`]).
    untested: Vec<bool>,
    /// The locals holding a division the ranges show can be unsigned
    /// ([`crate::mir::range_bounds::unsigned_divisions`]).
    unsigned_div: Vec<bool>,
    /// The binary carries the runtime debug metadata (design.md §
    /// Debugger contract, B-2026-11-05-34): every build but `--release`,
    /// which sets `KARAC_STRIP_DEBUG_METADATA`.
    debug_meta: bool,
    /// The index of the block being lowered.
    cur_block: usize,
    /// In the body being lowered, the blocks whose `TaskGroup.new` makes
    /// a `par` block's group, with the block's place, branch count and
    /// branch starts ([`par_group_sites`]).
    par_sites: HashMap<usize, ParSite>,
}

impl<'ctx, 'p> Cx<'ctx, 'p> {
    pub fn new(llcx: &'ctx Context, name: &str, program: &'p Program, tys: &'p TyInterner) -> Self {
        Cx {
            llcx,
            module: llcx.create_module(name),
            builder: llcx.create_builder(),
            program,
            tys,
            weak_shared: tys.weak_targets(),
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
            located: HashMap::new(),
            lfns: HashMap::new(),
            site_todo: Vec::new(),
            site_params: None,
            hot_swap: HashMap::new(),
            hot_swap_table: None,
            upper_proven: false,
            lengths: Vec::new(),
            char_bufs: Vec::new(),
            untested: Vec::new(),
            unsigned_div: Vec::new(),
            hot_swap_calls: Vec::new(),
            debug_meta: !matches!(
                std::env::var("KARAC_STRIP_DEBUG_METADATA").as_deref(),
                Ok("1" | "true")
            ),
            cur_block: 0,
            par_sites: HashMap::new(),
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

    /// Whether the runtime function `name` keeps none of the pointers it
    /// is passed past its return: it reads or writes through them and no
    /// more. Each of these builds or grows the `String` or `Vec` header
    /// its first pointer names, so `captures(none)` lets a local vector's slot
    /// stay out of reach of the stores through its own buffer: kata 343's
    /// `best[i] = whole` reloaded the slot's length and buffer after every
    /// store, and its bounds check stayed in the loop (B-2026-11-04-39).
    fn keeps_no_pointer(name: &str) -> bool {
        matches!(
            name,
            "karac_mir_vec_alloc"
                | "karac_mir_vec_push"
                | "karac_mir_vec_reserve"
                | "karac_mir_string_from"
                | "karac_mir_string_from_chars"
                | "karac_mir_string_push_char"
                | "karac_mir_string_push_str"
                | "karac_mir_str_repeat"
                | "karac_mir_str_from_utf8"
                | "karac_mir_str_from_utf8_lossy"
        )
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
            if Self::keeps_no_pointer(name) {
                // `captures(none)`: LLVM 21 replaced `nocapture` with the
                // `captures` attribute, whose value 0 captures nothing.
                let k = Attribute::get_named_enum_kind_id("captures");
                for (i, t) in ty.get_param_types().into_iter().enumerate() {
                    if t.is_pointer_type() {
                        f.add_attribute(
                            AttributeLoc::Param(i as u32),
                            self.llcx.create_enum_attribute(k, 0),
                        );
                    }
                }
            }
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
    /// integer for any other value of 1, 2, 4 or 8 bytes, and `{ptr, i64}`
    /// for a slice or `str` (or a reference to one), whose two words are
    /// laid out so; `None` when it is passed in memory. A slice in memory
    /// made every recursive call of kata 10's matcher copy both of its
    /// slices and return its `bool` through a pointer (B-2026-10-31-40).
    fn reg_class(&self, t: Ty) -> Option<Option<BasicTypeEnum<'ctx>>> {
        let l = self.layout(t).ok()?;
        if l.size == 0 {
            return Some(None);
        }
        let view = |k: TyKind| matches!(k, TyKind::Str | TyKind::Slice(_));
        let two_words = match self.tys.kind(t) {
            TyKind::Ref(p) | TyKind::MutRef(p) => view(self.tys.kind(p)),
            k => view(k),
        };
        if two_words && l.size == 16 && std::env::var("KARAC_MIR_SLICE_REGS").as_deref() != Ok("0")
        {
            let pair = [self.ptr_ty().into(), self.llcx.i64_type().into()];
            return Some(Some(self.llcx.struct_type(&pair, false).into()));
        }
        if let Some(pair) = self.scalar_pair(t, l) {
            return Some(Some(pair));
        }
        let lt = self.llvm_ty(t).ok()?;
        if !lt.is_array_type() {
            return Some(Some(lt));
        }
        matches!(l.size, 1 | 2 | 4 | 8).then(|| Some(self.int_ty(l.size as u32 * 8).into()))
    }

    /// The two-register type of a tuple or plain struct of `l` (at most
    /// 16 bytes) whose parts other than empty ones are exactly two
    /// scalars or one-word pointers: `{a, b}`, which LLVM lays out where
    /// the C layout puts them, as Rust passes its scalar pairs. A function
    /// returning `(i64, i64)` was passed in memory, every parameter too,
    /// so a `ref Vec` parameter of kata 337's recursive `visit` went as a
    /// pointer to a slot holding the reference (B-2026-11-04-39).
    /// `KARAC_MIR_PAIR_REGS=0` leaves them in memory.
    fn scalar_pair(&self, t: Ty, l: Layout) -> Option<BasicTypeEnum<'ctx>> {
        if l.size > 16 || std::env::var("KARAC_MIR_PAIR_REGS").as_deref() == Ok("0") {
            return None;
        }
        let parts: Vec<Ty> = match self.tys.kind(t) {
            TyKind::Tuple(ts) => ts.to_vec(),
            TyKind::Adt(_) => {
                if layout::library_cell(self.tys, t).is_some()
                    || layout::null_niche(self.tys, t).is_some()
                {
                    return None;
                }
                let (adt, _) = self.tys.tcx().adt_of(t)?;
                if adt.is_enum {
                    return None;
                }
                let n = adt.variants.first()?.fields.len() as u32;
                (0..n)
                    .map(|f| {
                        (!layout::boxed_field(self.tys, t, None, f))
                            .then(|| self.tys.field_ty(t, None, f))
                            .flatten()
                    })
                    .collect::<Option<Vec<_>>>()?
            }
            _ => return None,
        };
        let mut regs: Vec<(u64, BasicTypeEnum<'ctx>, Layout)> = Vec::new();
        for (i, p) in parts.iter().enumerate() {
            let pl = self.layout(*p).ok()?;
            if pl.size == 0 {
                continue;
            }
            let lt = self.llvm_ty(*p).ok()?;
            if lt.is_array_type() {
                return None;
            }
            regs.push((layout::field_offset(self.tys, t, None, i as u32)?, lt, pl));
        }
        let [(o0, t0, l0), (o1, t1, l1)] = regs[..] else {
            return None;
        };
        let at = |v: u64, a: u64| v.div_ceil(a) * a;
        let whole = at(o1 + l1.size, l0.align.max(l1.align));
        (o0 == 0 && o1 == at(l0.size, l1.align) && whole == l.size)
            .then(|| self.llcx.struct_type(&[t0, t1], false).into())
    }

    /// The by-value signature of `body`, when every parameter has a
    /// register class: each parameter's register type (`None` for one
    /// passed as nothing), then the result's. A result with no register
    /// class is `None` here too, and is written through a pointer the
    /// function takes first ([`Self::ret_in_memory`]).
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
        let ret = match self.reg_class(body.locals[0].ty) {
            Some(r) => r,
            None if sret_flag() => None,
            None => return None,
        };
        let params = (1..=body.arg_count)
            .map(|i| self.reg_class(body.locals[i].ty))
            .collect::<Option<Vec<_>>>()?;
        Some((params, ret))
    }

    /// Whether the by-value function of `body` takes a pointer to its
    /// result first, as Rust passes a result too large for registers: the
    /// parameters stay in registers. A result passed in memory used to put
    /// every parameter there too, so kata 333's recursive `visit`, which
    /// returns a four-field struct, took each `ref Vec` and `mut ref i64`
    /// as a pointer to a slot holding the reference (B-2026-11-04-39).
    pub(super) fn ret_in_memory(&self, body: &Body) -> bool {
        self.reg_class(body.locals[0].ty).is_none()
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
        let mut ps: Vec<BasicMetadataTypeEnum> = Vec::new();
        if self.ret_in_memory(body) {
            ps.push(self.ptr_ty().into());
        }
        ps.extend(
            params
                .iter()
                .flatten()
                .map(|t| BasicMetadataTypeEnum::from(*t)),
        );
        let ty = match ret {
            Some(r) => r.fn_type(&ps, false),
            None => self.llcx.void_type().fn_type(&ps, false),
        };
        let f = self
            .module
            .add_function(&fn_symbol(name), ty, Some(Linkage::Internal));
        // Every call passes a result slot of its own (`call_by_value`).
        if self.ret_in_memory(body) && noalias_flag() {
            let k = Attribute::get_named_enum_kind_id("noalias");
            f.add_attribute(
                AttributeLoc::Param(0),
                self.llcx.create_enum_attribute(k, 0),
            );
        }
        self.mark_deref_params(body, &params, f)?;
        self.mark_range_params(body, &params, ret, f);
        self.vfns.insert(name.to_string(), f);
        if self.located(name) {
            self.site_todo.push((name.to_string(), true));
            self.located_function(name)?;
        } else {
            self.schedule(body);
        }
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
        let v = self
            .value_function(name)?
            .ok_or("a wrapper of a body passed in memory")?;
        self.build_wrapper_to(name, self.fns[name], v, &[])
    }

    /// [`Self::build_wrapper`] of `f` around `v`, which takes `extra`
    /// after the body's own arguments.
    fn build_wrapper_to(
        &mut self,
        name: &str,
        f: FunctionValue<'ctx>,
        v: FunctionValue<'ctx>,
        extra: &[BasicMetadataValueEnum<'ctx>],
    ) -> R<()> {
        let program = self.program;
        let body = &program.bodies[name];
        let (params, ret) = self
            .value_sig(body)
            .ok_or("a wrapper of a body passed in memory")?;
        let always = self
            .llcx
            .create_enum_attribute(Attribute::get_named_enum_kind_id("alwaysinline"), 0);
        f.add_attribute(AttributeLoc::Function, always);
        let entry = self.llcx.append_basic_block(f, "entry");
        self.builder.position_at_end(entry);
        self.builder.unset_current_debug_location();
        let mut args: Vec<inkwell::values::BasicMetadataValueEnum> = Vec::new();
        if self.ret_in_memory(body) {
            let out = f.get_nth_param(0).ok_or("the result slot is missing")?;
            args.push(out.into());
        }
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
        args.extend_from_slice(extra);
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

    /// Builds the `fns` function (`vfns`, when `by_value`) of the located
    /// body `name` as a call of its `lfns` one with no place: a call
    /// through a function value, or from the runtime, does not know the
    /// program's (B-2026-10-31-2).
    fn build_site_wrapper(&mut self, name: &str, by_value: bool) -> R<()> {
        let l = self.lfns[name];
        let none = [
            self.ptr_ty().const_null().into(),
            self.llcx.i64_type().const_zero().into(),
        ];
        let program = self.program;
        if !by_value && self.value_sig(&program.bodies[name]).is_some() {
            return self.build_wrapper_to(name, self.fns[name], l, &none);
        }
        let f = if by_value {
            self.vfns[name]
        } else {
            self.fns[name]
        };
        let always = self
            .llcx
            .create_enum_attribute(Attribute::get_named_enum_kind_id("alwaysinline"), 0);
        f.add_attribute(AttributeLoc::Function, always);
        let entry = self.llcx.append_basic_block(f, "entry");
        self.builder.position_at_end(entry);
        self.builder.unset_current_debug_location();
        let mut args: Vec<BasicMetadataValueEnum> = f.get_param_iter().map(|p| p.into()).collect();
        args.extend(none);
        let r = self
            .builder
            .build_call(l, &args, "")
            .map_err(|e| e.to_string())?;
        if f.get_type().get_return_type().is_some() {
            let x = r.try_as_basic_value().unwrap_basic();
            self.builder
                .build_return(Some(&x))
                .map_err(|e| e.to_string())?;
        } else {
            self.builder.build_return(None).map_err(|e| e.to_string())?;
        }
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
        let program = self.program;
        for (name, f) in &self.lfns {
            let by_value = program
                .bodies
                .get(name)
                .is_none_or(|b| self.value_sig(b).is_some());
            if by_value || self.shared_fns.contains(name) {
                continue;
            }
            for i in 0..f.count_params().saturating_sub(2) {
                f.add_attribute(AttributeLoc::Param(i), noalias);
            }
        }
    }

    /// Marks each `bool` parameter and return of a by-value function
    /// `range(i8 0, 2)`: a `bool` travels as an `i8` that only ever holds 0
    /// or 1, which LLVM cannot see across the call. On the frozen set it
    /// changed 50 of 707 binaries; kata 100's `same_tree` and kata 101's
    /// `is_symmetric` ran 0.90x as long, the 50 together 0.999x, with the
    /// instruction counts unchanged: the fact moves blocks, not work
    /// (B-2026-11-05-25). The same fact on a `char` (below 0x110000) moved
    /// none of them, so it is not stated. `KARAC_MIR_RANGE_ATTR=0` leaves
    /// them out.
    fn mark_range_params(
        &self,
        body: &Body,
        params: &[Option<BasicTypeEnum<'ctx>>],
        ret: Option<BasicTypeEnum<'ctx>>,
        f: FunctionValue<'ctx>,
    ) {
        if std::env::var("KARAC_MIR_RANGE_ATTR").as_deref() == Ok("0") {
            return;
        }
        let is_bool = |ty: Ty, lt: BasicTypeEnum<'ctx>| {
            matches!(self.tys.kind(ty), TyKind::Bool)
                && lt.is_int_type()
                && lt.into_int_type().get_bit_width() == 8
        };
        let k = Attribute::get_named_enum_kind_id("range");
        let (lo, hi) = ([0u64], [2u64]);
        // SAFETY: a context-owned attribute built from one word per bound,
        // as `LLVMCreateConstantRangeAttribute` takes for an 8-bit range.
        let attr = unsafe {
            Attribute::new(llvm_sys::core::LLVMCreateConstantRangeAttribute(
                self.llcx.raw(),
                k,
                8,
                lo.as_ptr(),
                hi.as_ptr(),
            ))
        };
        if ret.is_some_and(|r| is_bool(body.locals[0].ty, r)) {
            f.add_attribute(AttributeLoc::Return, attr);
        }
        let mut at = u32::from(self.ret_in_memory(body));
        for (i, p) in params.iter().enumerate() {
            let Some(lt) = p else { continue };
            if is_bool(body.locals[i + 1].ty, *lt) {
                f.add_attribute(AttributeLoc::Param(at), attr);
            }
            at += 1;
        }
    }

    /// Marks each `ref T` / `mut ref T` parameter of a by-value function
    /// `dereferenceable(size of T)` and `align`, which a borrow guarantees
    /// for the whole call, as Rust marks a `&T`. The `llvm.assume` that
    /// [`Self::assume_ref_params`] states inside the body says the same
    /// and survives inlining, but LLVM 22's LICM no longer speculates a
    /// load on it: it must first prove nothing between the assume and the
    /// hoisted load can free the memory, and an abort call on the
    /// bounds-check path defeats that. On the parameter, the fact holds
    /// for the whole function, so the callee's own loop passes hoist a
    /// `Vec`'s data pointer before it is inlined. Without it kata 495's
    /// `time_series[i]` reloaded that pointer every trip; the deeper
    /// chain made the x86 cmov converter branch the loop's `min`, 2.8x
    /// slower (B-2026-11-05-4). `KARAC_MIR_DEREF_PARAM=0` leaves them out.
    ///
    /// A `ref T` whose `T` holds nothing a shared borrow lets change (see
    /// [`Self::frozen_inline`]) is `noalias` too, as Rust marks a `&T`
    /// without an `UnsafeCell`: the borrow keeps its bytes as they are for
    /// the call, so a store through any other pointer cannot reach them.
    /// Kata 718's DP loop stored into its own row vector and so reloaded
    /// both inputs' `Vec` headers, and tested their bounds, on every step
    /// (B-2026-11-09-8). `KARAC_MIR_NOALIAS_REF=0` leaves it out.
    fn mark_deref_params(
        &self,
        body: &Body,
        params: &[Option<BasicTypeEnum<'ctx>>],
        f: FunctionValue<'ctx>,
    ) -> R<()> {
        if std::env::var("KARAC_MIR_DEREF_PARAM").as_deref() == Ok("0") {
            return Ok(());
        }
        let mut at = u32::from(self.ret_in_memory(body));
        for (i, p) in params.iter().enumerate() {
            if p.is_none() {
                continue;
            }
            let ty = body.locals[i + 1].ty;
            let here = at;
            at += 1;
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
            for (kind, v) in [("dereferenceable", l.size), ("align", l.align.max(1))] {
                let k = Attribute::get_named_enum_kind_id(kind);
                f.add_attribute(
                    AttributeLoc::Param(here),
                    self.llcx.create_enum_attribute(k, v),
                );
            }
            // A `mut ref` is the only way to its pointee for the call: the
            // borrow checker rejects a second path through one handle
            // place, and through another handle a `T` that takes a borrow
            // flag panics before it is read or written (§6.2). A `Copy`
            // or counted `T` takes no flag, so a `mut` field of a shared
            // value of that type may be read through another handle while
            // the `mut ref` lives, and stays unmarked (kata 695's
            // union-find reloaded its `mut ref Vec` header after every
            // store into it, B-2026-11-09-2).
            let frozen = matches!(self.tys.kind(ty), TyKind::Ref(_))
                && noalias_ref_flag()
                && self.frozen_inline(t, 0);
            let exclusive = matches!(self.tys.kind(ty), TyKind::MutRef(_))
                && noalias_mutref_flag()
                && crate::mir::flags::takes_flag(self.tys, t);
            if frozen || exclusive {
                let k = Attribute::get_named_enum_kind_id("noalias");
                f.add_attribute(
                    AttributeLoc::Param(here),
                    self.llcx.create_enum_attribute(k, 0),
                );
            }
        }
        Ok(())
    }

    /// Whether a value of `t` keeps its own bytes while a shared borrow of
    /// it lives: numbers, `bool`s, `char`s, the headers of a `Vec`, a
    /// `String` and a `VecDeque` (their elements are behind a pointer, which
    /// `noalias` does not reach), a `shared` or `weak` handle, and tuples,
    /// arrays and `Option`s of those. Anything else (a user type, a map or
    /// set, an `Atomic`, a `Mutex`) may change in place behind a shared
    /// borrow, or is not checked here.
    fn frozen_inline(&self, t: Ty, depth: u32) -> bool {
        if depth > 8 {
            return false;
        }
        match self.tys.kind(t) {
            TyKind::Int(_) | TyKind::Float(_) | TyKind::Bool | TyKind::Char | TyKind::Unit => true,
            TyKind::Intrinsic(
                IntrinsicTy::String | IntrinsicTy::Vec(_) | IntrinsicTy::VecDeque(_),
            ) => true,
            TyKind::Shared(_) | TyKind::Weak(_) => true,
            TyKind::Tuple(ts) => ts.iter().all(|&p| self.frozen_inline(p, depth + 1)),
            TyKind::Array(e, _) => self.frozen_inline(e, depth + 1),
            TyKind::Adt(a) if self.tys.adt(a).name == "Option" => {
                let adt = self.tys.adt(a);
                adt.variants.iter().enumerate().all(|(v, var)| {
                    (0..var.fields.len() as u32).all(|i| {
                        self.tys
                            .field_ty(t, Some(v as u32), i)
                            .is_some_and(|ft| self.frozen_inline(ft, depth + 1))
                    })
                })
            }
            _ => false,
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
            // One bundle per assume. LLVM 22.1's `drop-unnecessary-assumes`
            // rebuilds an assume that keeps some of its bundles and drops
            // others, and registers the copy in the assumption cache it is
            // iterating: the vector grows under the loop and the pass reads
            // freed memory (the selfhost parser's `Parser.parse_type`
            // segfaulted). Upstream replaces the entry in place since; with
            // a single bundle an assume is kept or erased whole.
            for (tag, ops) in [(c"dereferenceable", &mut deref), (c"align", &mut align)] {
                // SAFETY: every value comes from this module's context, and
                // the bundle is disposed after the call that copies it.
                unsafe {
                    let mut bundle = [LLVMCreateOperandBundle(
                        tag.as_ptr(),
                        tag.count_bytes(),
                        ops.as_mut_ptr(),
                        2,
                    )];
                    LLVMBuildCallWithOperandBundles(
                        self.builder.as_mut_ptr(),
                        assume.get_type().as_type_ref(),
                        assume.as_value_ref(),
                        arg.as_mut_ptr(),
                        1,
                        bundle.as_mut_ptr(),
                        1,
                        c"".as_ptr(),
                    );
                    LLVMDisposeOperandBundle(bundle[0]);
                }
            }
        }
        Ok(())
    }

    /// States `lo <= mid < hi` for a binary search's midpoint
    /// ([`crate::mir::midpoint`]), which lets LLVM fold `nums[mid]`'s
    /// bounds check against a length `hi` is below. Marks the module so
    /// its passes run a second time (`mir_aot::run_passes`): the fact
    /// meets the check only once the search is inlined into its caller.
    ///
    /// When the search's `lo` only rises and its `hi` only falls in its loop
    /// (`crate::mir::monotone`), `mid` also lies within their values on
    /// entry, `lo0 <= mid < hi0`: a fact about values from outside the loop,
    /// which LLVM 22 keeps where it weakens `mid >= lo` and loses `hi <= hi0`
    /// across its passes (hsearch's `citations[mid]`, B-2026-11-10-37).
    #[allow(clippy::type_complexity)]
    fn assume_midpoint(
        &mut self,
        m: &crate::mir::midpoint::Midpoint,
        monotones: &[(
            crate::mir::monotone::Monotone,
            PointerValue<'ctx>,
            IntType<'ctx>,
        )],
    ) -> R<()> {
        let e = |x: inkwell::builder::BuilderError| x.to_string();
        let mid = self
            .operand_scalar(&Operand::Copy(Place::local(m.mid)))?
            .into_int_value();
        let lo = self
            .operand_scalar(&Operand::Copy(Place::local(m.lo)))?
            .into_int_value();
        let hi = self
            .operand_scalar(&Operand::Copy(Place::local(m.hi)))?
            .into_int_value();
        let (ge, lt) = if m.signed {
            (IntPredicate::SGE, IntPredicate::SLT)
        } else {
            (IntPredicate::UGE, IntPredicate::ULT)
        };
        let above = self
            .builder
            .build_int_compare(ge, mid, lo, "mid.ge.lo")
            .map_err(e)?;
        let below = self
            .builder
            .build_int_compare(lt, mid, hi, "mid.lt.hi")
            .map_err(e)?;
        let assume = self.module.get_function("llvm.assume").unwrap_or_else(|| {
            let ty = self
                .llcx
                .void_type()
                .fn_type(&[self.llcx.bool_type().into()], false);
            self.module.add_function("llvm.assume", ty, None)
        });
        let mut facts = vec![above, below];
        for (b, slot, ty) in monotones {
            if !b.blocks.get(m.block).copied().unwrap_or(false) || b.signed != m.signed {
                continue;
            }
            let (pred, name) = match (b.local == m.lo && b.up, b.local == m.hi && !b.up) {
                (true, _) => (ge, "mid.from.lo0"),
                (_, true) => (lt, "mid.under.hi0"),
                _ => continue,
            };
            let init = self
                .builder
                .build_load(*ty, *slot, "mono.init")
                .map_err(e)?
                .into_int_value();
            facts.push(
                self.builder
                    .build_int_compare(pred, mid, init, name)
                    .map_err(e)?,
            );
        }
        for fact in facts {
            self.builder
                .build_call(assume, &[fact.into()], "")
                .map_err(e)?;
        }
        if self.module.get_global_metadata_size(MIDPOINT_MARK) == 0 {
            let _ = self
                .module
                .add_global_metadata(MIDPOINT_MARK, &self.llcx.metadata_node(&[]));
        }
        Ok(())
    }

    /// The loop counters of `body` that only move one way
    /// ([`crate::mir::monotone`]), each with an entry-block slot for the
    /// value it held when its loop was entered. At the builder's position,
    /// which must be the entry block.
    #[allow(clippy::type_complexity)]
    fn monotone_slots(
        &mut self,
        body: &Body,
    ) -> R<
        Vec<(
            crate::mir::monotone::Monotone,
            PointerValue<'ctx>,
            IntType<'ctx>,
        )>,
    > {
        let mut out = Vec::new();
        for m in crate::mir::monotone::monotones(body, self.tys) {
            let BasicTypeEnum::IntType(t) = self.llvm_ty(body.local(m.local).ty)? else {
                continue;
            };
            let slot = self
                .builder
                .build_alloca(t, "mono.init")
                .map_err(|e| e.to_string())?;
            out.push((m, slot, t));
        }
        Ok(out)
    }

    /// States that a loop counter is still on the side of its value on
    /// entry that its steps move it to (`crate::mir::monotone`): `slot`
    /// took that value on the loop's entry edge. At the top of the
    /// header, so LLVM meets it on every trip.
    fn assume_monotone(
        &mut self,
        m: &crate::mir::monotone::Monotone,
        slot: PointerValue<'ctx>,
        ty: IntType<'ctx>,
    ) -> R<()> {
        let e = |x: inkwell::builder::BuilderError| x.to_string();
        let cur = self
            .operand_scalar(&Operand::Copy(Place::local(m.local)))?
            .into_int_value();
        let init = self
            .builder
            .build_load(ty, slot, "mono.init")
            .map_err(e)?
            .into_int_value();
        let pred = match (m.up, m.signed) {
            (true, true) => IntPredicate::SGE,
            (true, false) => IntPredicate::UGE,
            (false, true) => IntPredicate::SLE,
            (false, false) => IntPredicate::ULE,
        };
        let fact = self
            .builder
            .build_int_compare(pred, cur, init, "mono.fact")
            .map_err(e)?;
        let assume = self.module.get_function("llvm.assume").unwrap_or_else(|| {
            let ty = self
                .llcx
                .void_type()
                .fn_type(&[self.llcx.bool_type().into()], false);
            self.module.add_function("llvm.assume", ty, None)
        });
        self.builder
            .build_call(assume, &[fact.into()], "")
            .map_err(e)?;
        Ok(())
    }

    /// Weighs the edges of `sw`, the switch lowered from `targets`, so the
    /// one to `rare` is taken about once in 2000 ([`crate::mir::counters::overflow_tests`]).
    fn rare_edge(
        &self,
        sw: inkwell::values::InstructionValue<'ctx>,
        targets: &crate::mir::SwitchTargets,
        rare: crate::mir::BasicBlock,
    ) -> R<()> {
        if sw.get_opcode() != inkwell::values::InstructionOpcode::Switch {
            return Ok(());
        }
        let i32t = self.llcx.i32_type();
        let weight = |bb| {
            i32t.const_int(if bb == rare { 1 } else { 2000 }, false)
                .into()
        };
        let mut ops = vec![self.llcx.metadata_string("branch_weights").into()];
        ops.push(weight(targets.otherwise));
        ops.extend(targets.values.iter().map(|&(_, bb)| weight(bb)));
        let node = self.llcx.metadata_node(&ops);
        sw.set_metadata(node, self.llcx.get_kind_id("prof"))
            .map_err(|_| "branch weights".to_string())
    }

    /// Marks the back edge `br` `!llvm.loop !{!self,
    /// !{!"llvm.loop.unroll.full"}}`, or with `count`,
    /// `!{!"llvm.loop.unroll.count", i32 count}`: the self-referential loop
    /// id built through a temporary node, as the legacy backend does
    /// (`attach_unroll_full_metadata`, `attach_unroll_count_metadata`).
    fn unroll_hint(&self, br: inkwell::values::InstructionValue<'ctx>, count: Option<u32>) {
        use inkwell::values::AsValueRef;
        use llvm_sys::core::{
            LLVMConstInt, LLVMGetMDKindIDInContext, LLVMInt32TypeInContext, LLVMMDNodeInContext2,
            LLVMMDStringInContext2, LLVMMetadataAsValue, LLVMSetMetadata, LLVMValueAsMetadata,
        };
        use llvm_sys::debuginfo::{LLVMMetadataReplaceAllUsesWith, LLVMTemporaryMDNode};
        use std::os::raw::{c_char, c_uint};
        let ctx = self.llcx.raw();
        // SAFETY: every node is made in `ctx`, which outlives the module,
        // and the temporary is replaced (and so freed) before it escapes.
        unsafe {
            let prop = match count {
                None => {
                    let key = b"llvm.loop.unroll.full";
                    let k = LLVMMDStringInContext2(ctx, key.as_ptr() as *const c_char, key.len());
                    let mut prop_ops = [k];
                    LLVMMDNodeInContext2(ctx, prop_ops.as_mut_ptr(), prop_ops.len())
                }
                Some(n) => {
                    let key = b"llvm.loop.unroll.count";
                    let k = LLVMMDStringInContext2(ctx, key.as_ptr() as *const c_char, key.len());
                    let n = LLVMValueAsMetadata(LLVMConstInt(
                        LLVMInt32TypeInContext(ctx),
                        u64::from(n),
                        0,
                    ));
                    let mut prop_ops = [k, n];
                    LLVMMDNodeInContext2(ctx, prop_ops.as_mut_ptr(), prop_ops.len())
                }
            };
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
        if self.located(name) {
            self.site_todo.push((name.to_string(), false));
            self.located_function(name)?;
            return Ok(f);
        }
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
            } else if let Some((name, by_value)) = self.site_todo.pop() {
                self.build_site_wrapper(&name, by_value)?;
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
        let f = if self.located(&body.instance.name) {
            self.located_function(&body.instance.name)?
        } else {
            match self.value_function(&body.instance.name)? {
                Some(v) => v,
                None => self.direct_function(&body.instance.name)?,
            }
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
        self.site_params = None;
        if self.lfns.get(name) == Some(&f) {
            let n = f.count_params();
            let p = f.get_nth_param(n - 2).ok_or("the place is missing")?;
            let l = f.get_nth_param(n - 1).ok_or("the place is missing")?;
            self.site_params = Some((f, p.into_pointer_value(), l.into_int_value()));
        }
        self.locals.clear();
        self.abort_blocks.clear();
        // A by-value body's parameters, in order, skipping those passed as
        // nothing.
        let mut incoming = f.get_param_iter();
        // A by-value body whose result is in memory takes its slot first.
        let out = match sig.is_some() && self.ret_in_memory(body) {
            true => Some(
                incoming
                    .next()
                    .ok_or("the result slot is missing")?
                    .into_pointer_value(),
            ),
            false => None,
        };
        for (i, decl) in body.locals.iter().enumerate() {
            let slot = if i <= body.arg_count && sig.is_none() {
                f.get_nth_param(i as u32)
                    .ok_or("a parameter is missing")?
                    .into_pointer_value()
            } else if let (0, Some(out)) = (i, out) {
                out
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
        // `KARAC_MIR_CHAR_STR=0` allocates every char's `to_string()`. A
        // coroutine's locals can live in its frame across a suspension,
        // past this function's stack, so its bodies keep the heap.
        self.char_bufs.clear();
        if std::env::var("KARAC_MIR_CHAR_STR").as_deref() != Ok("0")
            && !crate::mir::coroutine::is_resume(&body.instance.name)
        {
            for on in crate::mir::char_strs::borrowed_char_strings(body) {
                let buf = match on {
                    true => Some(
                        self.builder
                            .build_alloca(self.llcx.i8_type().array_type(4), "char.utf8")
                            .map_err(|e| e.to_string())?,
                    ),
                    false => None,
                };
                self.char_bufs.push(buf);
            }
        }
        self.assume_ref_params(body)?;
        self.flag_slots()?;
        // `KARAC_MIR_MONOTONE=0` leaves loop counters' directions unstated.
        let monotones = if std::env::var("KARAC_MIR_MONOTONE").as_deref() == Ok("0") {
            Vec::new()
        } else {
            self.monotone_slots(body)?
        };
        self.blocks = (0..body.blocks.len())
            .map(|i| self.llcx.append_basic_block(f, &format!("bb{i}")))
            .collect();
        self.builder
            .build_unconditional_branch(self.blocks[0])
            .map_err(|e| e.to_string())?;
        // `KARAC_MIR_UNROLL=0` leaves out the unroll hints, full and partial;
        // `KARAC_MIR_UNROLL_PARTIAL=0` only the partial ones.
        let program = self.program;
        let callee = |n: &str| program.bodies.get(n);
        let (unroll, partial) = if std::env::var("KARAC_MIR_UNROLL").as_deref() == Ok("0") {
            (Vec::new(), Vec::new())
        } else if std::env::var("KARAC_MIR_UNROLL_PARTIAL").as_deref() == Ok("0") {
            (
                crate::mir::counters::unroll_full_latches(body, self.tys, callee),
                Vec::new(),
            )
        } else {
            (
                crate::mir::counters::unroll_full_latches(body, self.tys, callee),
                crate::mir::counters::unroll_partial_latches(body, self.tys),
            )
        };
        // `KARAC_MIR_LENGTH_DIVISORS=0` checks every division in full.
        self.lengths = if std::env::var("KARAC_MIR_LENGTH_DIVISORS").as_deref() == Ok("0") {
            Vec::new()
        } else {
            let program = self.program;
            crate::mir::guard_bounds::length_locals(body, |n| !program.bodies.contains_key(n))
        };
        self.untested = crate::mir::range_bounds::untested_checks(body);
        // `KARAC_MIR_UNSIGNED_DIV=0` divides as the types say.
        self.unsigned_div = if std::env::var("KARAC_MIR_UNSIGNED_DIV").as_deref() == Ok("0") {
            Vec::new()
        } else {
            crate::mir::range_bounds::unsigned_divisions(body, self.tys)
        };
        // `KARAC_MIR_GUARD_BOUNDS=0` checks every index in full.
        let proven = if std::env::var("KARAC_MIR_GUARD_BOUNDS").as_deref() == Ok("0") {
            Vec::new()
        } else {
            crate::mir::guard_bounds::upper_proven_indexes(body, self.tys)
        };
        // `KARAC_MIR_COLD_OVERFLOW=0` leaves overflow tests unweighted.
        let mut overflows = if std::env::var("KARAC_MIR_COLD_OVERFLOW").as_deref() == Ok("0") {
            Vec::new()
        } else {
            crate::mir::counters::overflow_tests(body)
        };
        // `KARAC_MIR_RARE_EXITS=0` leaves the short-circuit arms inside a
        // loop's early-return guard unweighted (`rare_exits`).
        if std::env::var("KARAC_MIR_RARE_EXITS").as_deref() != Ok("0") {
            overflows.extend(crate::mir::rare_exits::rare_exit_arms(body, self.tys));
        }
        // `KARAC_MIR_MIDPOINT=0` leaves binary-search midpoints unstated.
        let midpoints = if std::env::var("KARAC_MIR_MIDPOINT").as_deref() == Ok("0") {
            Vec::new()
        } else {
            crate::mir::midpoint::midpoints(body, self.tys)
        };
        self.par_sites = if self.debug_meta {
            par_group_sites(body)
        } else {
            HashMap::new()
        };
        for (i, block) in body.blocks.iter().enumerate() {
            self.cur_block = i;
            self.builder.position_at_end(self.blocks[i]);
            for (m, slot, ty) in monotones.iter().filter(|(m, ..)| m.header == i) {
                self.assume_monotone(m, *slot, *ty)?;
            }
            for (si, s) in block.statements.iter().enumerate() {
                self.locate(s.source_info.span);
                self.statement(s)?;
                if let Some(m) = midpoints.iter().find(|m| m.block == i && m.statement == si) {
                    self.assume_midpoint(m, &monotones)?;
                }
            }
            for (m, slot, _) in monotones.iter().filter(|(m, ..)| m.entry == i) {
                let v = self.operand_scalar(&Operand::Copy(Place::local(m.local)))?;
                self.builder
                    .build_store(*slot, v)
                    .map_err(|e| e.to_string())?;
            }
            self.locate(block.terminator.source_info.span);
            self.upper_proven = proven.contains(&i);
            let lowered = match &block.terminator.kind {
                TerminatorKind::Drop { place, target, .. } => self.drop_terminator(place, *target),
                kind => self.terminator(kind),
            };
            self.upper_proven = false;
            lowered?;
            let hint = if unroll.contains(&i) {
                Some(None)
            } else if partial.contains(&i) {
                Some(Some(4))
            } else {
                None
            };
            if let Some(count) = hint {
                if matches!(block.terminator.kind, TerminatorKind::Goto { .. }) {
                    if let Some(br) = self
                        .builder
                        .get_insert_block()
                        .and_then(|b| b.get_terminator())
                    {
                        self.unroll_hint(br, count);
                    }
                }
            }
            if let Some(&(_, set)) = overflows.iter().find(|(b, _)| *b == i) {
                if let (TerminatorKind::SwitchInt { targets, .. }, Some(sw)) = (
                    &block.terminator.kind,
                    self.builder
                        .get_insert_block()
                        .and_then(|b| b.get_terminator()),
                ) {
                    self.rare_edge(sw, targets, set)?;
                }
            }
        }
        self.debug_leave();
        self.body = None;
        self.site_params = None;
        self.lengths.clear();
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
        if let Some(els) = self.const_table(def) {
            let g = self.const_table_global(&format!("kara.static.{}", def.name), &els)?;
            let p = g.as_pointer_value();
            self.statics.insert(i, p);
            return Ok(p);
        }
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
            // A constant table is its own initializer.
            if self.const_table(def).is_some() {
                continue;
            }
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

/// The blocks of `body` whose call is the `TaskGroup.new` of a `par`
/// block's group, with the block's place and branch count, for the runtime
/// debug metadata (design.md § Debugger contract, B-2026-11-05-34). The
/// group is made just before the first branch: the call's block leads to
/// the first branch's entry through at most a few single-successor blocks
/// (an auto-par group's `__hidden` call among them).
/// A `par` block's place, its branch count (0 for a `par for`), and the
/// closure each branch runs (where the branch starts), for `Runtime.list_par_blocks` (design.md § Debugger
/// contract).
struct ParSite {
    span: crate::token::Span,
    workers: u32,
    branches: Vec<Option<Ty>>,
}

/// The closure a `par` branch runs as its task: the first one its blocks
/// build. Where it starts is where the branch starts.
fn branch_closure(body: &Body, blocks: &[crate::mir::BasicBlock]) -> Option<Ty> {
    blocks
        .iter()
        .filter_map(|b| body.blocks.get(b.0 as usize))
        .flat_map(|b| &b.statements)
        .find_map(|s| match &s.kind {
            StatementKind::Assign(_, Rvalue::Aggregate(AggregateKind::Closure { ty }, _)) => {
                Some(*ty)
            }
            _ => None,
        })
}

fn par_group_sites(body: &Body) -> HashMap<usize, ParSite> {
    let mut sites = HashMap::new();
    if body.par_regions.is_empty() {
        return sites;
    }
    let mut preds: Vec<Vec<usize>> = vec![Vec::new(); body.blocks.len()];
    for (i, b) in body.blocks.iter().enumerate() {
        for t in b.terminator.kind.successors() {
            if let Some(p) = preds.get_mut(t.0 as usize) {
                p.push(i);
            }
        }
    }
    for region in &body.par_regions {
        let Some(&entry) = region.branches.first().and_then(|b| b.first()) else {
            continue;
        };
        let workers = match region.kind {
            crate::mir::ParKind::Block => region.branches.len() as u32,
            crate::mir::ParKind::For { .. } => 0,
        };
        let mut at = entry.0 as usize;
        for _ in 0..4 {
            let [p] = preds[at][..] else {
                break;
            };
            if let TerminatorKind::Call {
                func:
                    Operand::Const(Const {
                        kind: ConstKind::FnDef(i),
                        ..
                    }),
                ..
            } = &body.blocks[p].terminator.kind
            {
                if i.name == "TaskGroup.new" {
                    let branches = match region.kind {
                        crate::mir::ParKind::Block => region
                            .branches
                            .iter()
                            .map(|b| branch_closure(body, b))
                            .collect(),
                        crate::mir::ParKind::For { .. } => Vec::new(),
                    };
                    sites.insert(
                        p,
                        ParSite {
                            span: region.span,
                            workers,
                            branches,
                        },
                    );
                    break;
                }
            }
            at = p;
        }
    }
    sites
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
