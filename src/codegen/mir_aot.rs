//! An ahead-of-time build of the MIR backend's IR (redesign M2).
//!
//! [`compile_object`] takes the textual IR that
//! [`super::mir::compile_mir_ir`] produces and writes a native object; the
//! caller links it with [`super::link_executable`], which picks the runtime
//! archive the object needs. This is what `karac build --mir` runs.
//!
//! The IR defines the C `main` itself, so the object links as a program
//! with nothing else around it.

use inkwell::context::Context;
use inkwell::memory_buffer::MemoryBuffer;
use inkwell::module::Module;
use inkwell::passes::PassBuilderOptions;
use inkwell::targets::FileType;
use inkwell::types::BasicTypeEnum;
use inkwell::AddressSpace;
use std::path::Path;

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
    let tm = super::create_target_machine()?;
    module.set_triple(&tm.get_triple());
    module.set_data_layout(&tm.get_target_data().get_data_layout());

    // The runtime declares the spawn-site table `extern`. Legacy codegen
    // emits it; the MIR backend does not (the JIT runner fills it in at run
    // time), so a program gets an empty one.
    let i32t = llcx.i32_type();
    empty_global(&module, "KARAC_SPAWN_SITES_ENABLED", llcx.i8_type().into());
    empty_global(&module, "KARAC_SPAWN_SITES_LEN", i32t.into());
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
    empty_global(&module, "KARAC_SPAWN_SITES", entry.into());

    if opt > 0 {
        let mut passes = format!("default<O{}>", opt.min(3));
        // Every argument is passed by pointer; `default<O2>` has no
        // argument promotion, which turns the read-only scalar ones of
        // internal functions back into registers (their pointers are
        // `noalias`, `mod.rs`), so it runs after, with the cleanup it
        // leaves for.
        if opt >= 2 && super::mir::noalias_flag() {
            passes.push_str(",cgscc(argpromotion),function(sroa,instcombine,simplifycfg)");
        }
        module
            .run_passes(&passes, &tm, PassBuilderOptions::create())
            .map_err(|e| format!("{passes}: {}", e.to_string()))?;
    }
    tm.write_to_file(&module, FileType::Object, obj)
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
