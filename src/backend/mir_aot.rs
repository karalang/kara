//! An ahead-of-time build of the MIR backend's IR (redesign M2).
//!
//! [`compile_program_object`] compiles a MIR program to a native object
//! ([`compile_object`] does the same from the textual IR that
//! [`super::mir::compile_mir_ir`] produces); the caller links it with [`super::driver::link_executable`], which picks the runtime
//! archive the object needs. This is what `karac build --mir` runs.
//!
//! The IR defines the C `main` itself, so the object links as a program
//! with nothing else around it.

use inkwell::context::Context;
use inkwell::memory_buffer::MemoryBuffer;
use inkwell::module::Module;
use inkwell::passes::PassBuilderOptions;
use inkwell::targets::{FileType, TargetMachine};
use inkwell::types::BasicTypeEnum;
use inkwell::AddressSpace;
use std::path::Path;

mod select_branch;
mod split;

/// Compile `ir` to a native object at `obj`.
///
/// `opt` is the IR optimisation level, 0 to 3; at 0 no IR passes run.
pub fn compile_object(ir: &str, obj: &Path, opt: u8) -> Result<(), String> {
    let llcx = Context::create();
    // The IR parser reads a NUL-terminated buffer.
    let mut bytes = Vec::with_capacity(ir.len() + 1);
    bytes.extend_from_slice(ir.as_bytes());
    bytes.push(0);
    let buf = MemoryBuffer::create_from_memory_range_copy(&bytes, "main");
    let module = llcx
        .create_module_from_ir(buf)
        .map_err(|e| format!("parse IR: {}", e.to_string()))?;
    compile_module(&llcx, &module, obj, opt)
}

/// Compile `program` straight to a native object at `obj`, as
/// [`compile_object`] does with its IR, without printing the IR and parsing
/// it back (a large program's IR is megabytes). `keep_ll`, when given, is
/// where a copy of the IR is written (`KARAC_MIR_LL`).
pub fn compile_program_object(
    program: &crate::mir::interp::Program,
    tys: &crate::mir::TyInterner,
    obj: &Path,
    opt: u8,
    keep_ll: Option<&Path>,
) -> Result<(), String> {
    let mut t = std::time::Instant::now();
    let llcx = Context::create();
    let module = super::mir::compile_mir(&llcx, "main", program, tys)?;
    crate::mir::lower::phase_time("compile_mir", &mut t);
    if let Some(path) = keep_ll {
        let _ = std::fs::write(path, module.print_to_string().to_string());
    }
    compile_module(&llcx, &module, obj, opt)
}

fn compile_module<'ctx>(
    llcx: &'ctx Context,
    module: &Module<'ctx>,
    obj: &Path,
    opt: u8,
) -> Result<(), String> {
    let mut t = std::time::Instant::now();
    let tm = super::driver::create_target_machine()?;
    module.set_triple(&tm.get_triple());
    module.set_data_layout(&tm.get_target_data().get_data_layout());

    // The runtime declares the spawn-site table `extern`. Legacy codegen
    // emits it; the MIR backend does not (the JIT runner fills it in at run
    // time), so a program gets an empty one.
    let i32t = llcx.i32_type();
    empty_global(module, "KARAC_SPAWN_SITES_ENABLED", llcx.i8_type().into());
    empty_global(module, "KARAC_SPAWN_SITES_LEN", i32t.into());
    // `{ i32 id, ptr file_cstr, i32 line, i32 col, i32 worker_count, i32 reserved }`
    let ptr = llcx.ptr_type(AddressSpace::default());
    let entry = llcx.struct_type(
        &[
            i32t.into(),
            ptr.into(),
            i32t.into(),
            i32t.into(),
            i32t.into(),
            i32t.into(),
        ],
        false,
    );
    empty_global(module, "KARAC_SPAWN_SITES", entry.into());

    if let Some(plan) = split::plan(module.as_mut_ptr()) {
        crate::mir::lower::phase_time("plan codegen units", &mut t);
        let units = plan.units();
        let r = split::compile_units(module.as_mut_ptr(), &plan, obj, &|m, part| {
            optimize_and_emit(m, part, opt, &super::driver::create_target_machine()?)
        });
        crate::mir::lower::phase_time(&format!("opt + emission, {units} units"), &mut t);
        return r;
    }
    optimize_and_emit_timed(module, obj, opt, &tm)
}

fn optimize_and_emit_timed(
    module: &Module<'_>,
    obj: &Path,
    opt: u8,
    tm: &TargetMachine,
) -> Result<(), String> {
    let mut t = std::time::Instant::now();
    run_passes(module, opt, tm)?;
    crate::mir::lower::phase_time("opt passes", &mut t);
    let r = emit(module, obj, tm);
    crate::mir::lower::phase_time("object emission", &mut t);
    r
}

fn optimize_and_emit(
    module: &Module<'_>,
    obj: &Path,
    opt: u8,
    tm: &TargetMachine,
) -> Result<(), String> {
    run_passes(module, opt, tm)?;
    emit(module, obj, tm)
}

fn run_passes(module: &Module<'_>, opt: u8, tm: &TargetMachine) -> Result<(), String> {
    if opt > 0 {
        let mut passes = format!("default<O{}>", opt.min(3));
        // Every argument is passed by pointer; `default<O2>` has no
        // argument promotion, which turns the read-only scalar ones of
        // internal functions back into registers (their pointers are
        // `noalias`, `mod.rs`), so it runs after, with the cleanup it
        // leaves for.
        if opt >= 2 && super::mir::noalias_flag() {
            passes.push_str(
                ",cgscc(argpromotion),function(sroa,instcombine<no-verify-fixpoint>,simplifycfg)",
            );
            // Promoting once before `default<O2>` as well lets its inliner
            // and loop passes see scalar parameters rather than pointers: a
            // small function reached through a self-call (which the inliner
            // costs as its pointer-heavy form) is inlined and its loads
            // hoisted. `KARAC_MIR_EARLY_PROMOTE=0` leaves it out.
            if std::env::var("KARAC_MIR_EARLY_PROMOTE").as_deref() != Ok("0") {
                passes = format!(
                    "function(sroa,instcombine<no-verify-fixpoint>),cgscc(argpromotion),{passes}"
                );
            }
        }
        module
            .run_passes(&passes, tm, PassBuilderOptions::create())
            .map_err(|e| format!("{passes}: {}", e.to_string()))?;
        // SAFETY: the module is live for the call.
        unsafe { select_branch::mark_index_steps(module.as_mut_ptr()) };
    }
    Ok(())
}

fn emit(module: &Module<'_>, obj: &Path, tm: &TargetMachine) -> Result<(), String> {
    tm.write_to_file(module, FileType::Object, obj)
        .map_err(|e| format!("write {}: {}", obj.display(), e.to_string()))
}

/// Define `name` as a zero constant of `ty`, unless the module already has it.
fn empty_global<'ctx>(module: &Module<'ctx>, name: &str, ty: BasicTypeEnum<'ctx>) {
    if module.get_global(name).is_none() {
        let g = module.add_global(ty, Some(AddressSpace::default()), name);
        g.set_initializer(&ty.const_zero());
        g.set_constant(true);
    }
}
