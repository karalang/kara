//! Sanitizer builds of the MIR backend's IR: AddressSanitizer (redesign M2)
//! and ThreadSanitizer (M4 item 11).
//!
//! M2's gate asks the compiled backend to be ASan/LSan clean on the corpus.
//! [`compile_asan_object`] takes the textual IR that
//! [`super::mir::compile_mir_ir`] produces, instruments every function it
//! defines, and writes a native object. The caller links it with
//! [`super::driver::link_executable_with_sanitizer`] and `-fsanitize=address`, so the
//! runtime's `malloc`/`free` are intercepted too.
//!
//! Unlike the legacy harness (see `link_executable_with_sanitizer`), the
//! emitted code is COMPILED with the sanitizer, so a load or store through a
//! freed pointer is reported, not only a double free or a leak.
//!
//! [`compile_tsan_object`] does the same for ThreadSanitizer, which the pool
//! (on by default, `KARAC_THREADS` workers) must be clean under. Its
//! link takes `-fsanitize=thread`, which picks the TSan-built runtime
//! archive (`libkarac_runtime_tsan.a`): the pool's lock and queues are
//! the runtime's, and TSan sees the order they impose only in instrumented
//! code, so an uninstrumented runtime would report every task's handoff
//! as a race.

use inkwell::attributes::{Attribute, AttributeLoc};
use inkwell::context::Context;
use inkwell::memory_buffer::MemoryBuffer;
use inkwell::module::Module;
use inkwell::passes::PassBuilderOptions;
use inkwell::targets::FileType;
use inkwell::types::BasicTypeEnum;
use inkwell::AddressSpace;
use std::path::Path;

/// Instrument `ir` for AddressSanitizer and write it to `obj` as an object.
///
/// - The module gets the host triple and data layout. Without a triple the
///   pass picks a shadow mapping the runtime does not use, and the first
///   instrumented stack access faults.
/// - Every defined function gets `sanitize_address`, which is what the pass
///   instruments; a C front end adds it per function, and IR has none. It
///   also keeps its frame pointer, so reports can name it.
/// - The spawn-site table the runtime declares `extern` is defined empty when
///   the module lacks it. Legacy codegen emits it; the MIR backend does not,
///   and the JIT runner fills it in at run time instead.
pub fn compile_asan_object(ir: &str, obj: &Path) -> Result<(), String> {
    compile_sanitized_object(ir, obj, "sanitize_address", "asan")
}

/// Instrument `ir` for ThreadSanitizer and write it to `obj`, as
/// [`compile_asan_object`] does for AddressSanitizer: every defined function
/// gets `sanitize_thread`, which is what the pass instruments.
pub fn compile_tsan_object(ir: &str, obj: &Path) -> Result<(), String> {
    compile_sanitized_object(ir, obj, "sanitize_thread", "tsan-module,function(tsan)")
}

/// Gives every function `ir` defines the attribute `attr`, runs the
/// sanitizer's `passes` and writes the object.
fn compile_sanitized_object(ir: &str, obj: &Path, attr: &str, passes: &str) -> Result<(), String> {
    let llcx = Context::create();
    // The IR parser reads a NUL-terminated buffer.
    let mut bytes = Vec::with_capacity(ir.len() + 1);
    bytes.extend_from_slice(ir.as_bytes());
    bytes.push(0);
    let buf = MemoryBuffer::create_from_memory_range_copy(&bytes, "main");
    let module = llcx
        .create_module_from_ir(buf)
        .map_err(|e| format!("parse IR: {}", e.to_string()))?;
    let tm = super::driver::create_unoptimized_target_machine()?;
    module.set_triple(&tm.get_triple());
    module.set_data_layout(&tm.get_target_data().get_data_layout());

    // Frame pointers let the sanitizer's fast unwinder walk through emitted
    // code, so a report names the Kāra functions that allocated.
    let kind = Attribute::get_named_enum_kind_id(attr);
    for f in module.get_functions() {
        if f.count_basic_blocks() > 0 {
            f.add_attribute(AttributeLoc::Function, llcx.create_enum_attribute(kind, 0));
            f.add_attribute(
                AttributeLoc::Function,
                llcx.create_string_attribute("frame-pointer", "all"),
            );
        }
    }

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

    module
        // No IR optimisation: at -O1 LLVM deletes allocations and loads nothing
        // observes, which hides exactly the faults this lane is for, and it
        // changes the output of some programs (reported to the MIR backend's
        // owner), which would make the lane disagree with `__mir-llvm-run`.
        .run_passes(passes, &tm, PassBuilderOptions::create())
        .map_err(|e| format!("{passes} passes: {}", e.to_string()))?;
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
