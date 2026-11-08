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

mod carry_store;
mod overflow_flag;
mod select_branch;
mod split;
mod walk_carry;

/// The walk-index marker `walk_carry` carries or expands after the
/// passes (`src/backend/mir/table.rs` emits it).
pub(crate) const WALK_INDEX: &str = walk_carry::MARKER;

/// A walk's words, out of line through the passes so a loop that writes
/// nothing the table could be runs it once, then inlined
/// (`src/backend/mir/table.rs`).
pub(crate) const WALK_WORDS: &str = "karac.walk.words";

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
    compile_module(&llcx, &module, obj, opt, true)
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
    compile_program_object_with(program, tys, obj, opt, keep_ll, &[])
}

/// [`compile_program_object`] with the bodies `hot_swap` names called
/// through `karac_hotswap_table` (`--enable-hot-swap`,
/// [`super::mir::compile_mir_with`]).
pub fn compile_program_object_with(
    program: &crate::mir::interp::Program,
    tys: &crate::mir::TyInterner,
    obj: &Path,
    opt: u8,
    keep_ll: Option<&Path>,
    hot_swap: &[String],
) -> Result<(), String> {
    let mut t = std::time::Instant::now();
    let llcx = Context::create();
    let module = super::mir::compile_mir_with(&llcx, "main", program, tys, hot_swap)?;
    crate::mir::lower::phase_time("compile_mir", &mut t);
    if let Some(path) = keep_ll {
        let _ = std::fs::write(path, module.print_to_string().to_string());
    }
    compile_module(&llcx, &module, obj, opt, true)
}

/// The entry of the in-order program a parallel build carries
/// ([`compile_dual_program_object`]).
const IN_ORDER_MAIN: &str = "karac.main.seq";

/// [`compile_program_object`] for a build that runs loops or statements as
/// tasks and carries the program as `KARAC_AUTO_PAR=0` builds it
/// (`Lowered::in_order`, lowered by `seq`): each is its own module,
/// optimised and emitted as its own build would be, and the objects are
/// merged. `karac.main` asks the pool once and, on one worker, where the
/// tasks only cost, runs the in-order program's. Its code is the in-order
/// build's, so it runs that build's instructions, which an in-order copy of
/// each function beside the tasks in one module did not (B-2026-10-28-1).
pub fn compile_dual_program_object(
    (program, tys): (&crate::mir::interp::Program, &crate::mir::TyInterner),
    seq: impl FnOnce() -> Result<crate::mir::lower::Lowered, String>,
    obj: &Path,
    opt: u8,
    keep_ll: Option<&Path>,
) -> Result<(), String> {
    let mut t = std::time::Instant::now();
    let llcx = Context::create();
    let module = super::mir::compile_mir_with(&llcx, "main", program, tys, &[])?;
    pick_by_pool(&llcx, &module)?;
    crate::mir::lower::phase_time("compile_mir", &mut t);
    if let Some(path) = keep_ll {
        let _ = std::fs::write(path, module.print_to_string().to_string());
    }
    let part = |tag: &str| {
        let mut p = obj.as_os_str().to_owned();
        p.push(format!(".{tag}.o"));
        std::path::PathBuf::from(p)
    };
    let parts = [part("par"), part("seq")];
    // The two modules share nothing, so the parallel one is optimised and
    // emitted, read back in a context of its own thread, while this one
    // lowers and builds the in-order program.
    let par_bitcode = split::bitcode(module.as_mut_ptr());
    drop(module);
    let built = std::thread::scope(|s| {
        let par = s.spawn(|| {
            let llcx = Context::create();
            let buf = MemoryBuffer::create_from_memory_range_copy(&par_bitcode, "par");
            let m = Module::parse_bitcode_from_buffer(&buf, &llcx)
                .map_err(|e| format!("read the parallel module: {}", e.to_string()))?;
            compile_module(&llcx, &m, &parts[0], opt, true)
        });
        let seq = seq().and_then(|seq| {
            let mut t = std::time::Instant::now();
            let llcx = Context::create();
            let module = super::mir::compile_mir_with(&llcx, "main", &seq.program, &seq.tys, &[])?;
            in_order_module(&module)?;
            crate::mir::lower::phase_time("compile_mir (in order)", &mut t);
            if let Some(path) = keep_ll {
                let mut seq_path = path.as_os_str().to_owned();
                seq_path.push(".seq.ll");
                let _ = std::fs::write(seq_path, module.print_to_string().to_string());
            }
            compile_module(&llcx, &module, &parts[1], opt, false)
        });
        let par = par
            .join()
            .unwrap_or_else(|_| Err("the parallel module's build panicked".to_string()));
        par.and(seq)
    })
    .and_then(|()| split::partial_link(obj, &parts));
    for p in &parts {
        let _ = std::fs::remove_file(p);
    }
    built
}

/// `karac.main` becomes `karac.main.par`, and a new `karac.main` runs it,
/// or [`IN_ORDER_MAIN`] when the pool has one worker (`karac_mir_pooled`).
fn pick_by_pool<'ctx>(llcx: &'ctx Context, module: &Module<'ctx>) -> Result<(), String> {
    let e = |e: inkwell::builder::BuilderError| e.to_string();
    let par = module
        .get_function("karac.main")
        .ok_or("no `karac.main` to pick the in-order program from")?;
    par.as_global_value().set_name("karac.main.par");
    par.set_linkage(inkwell::module::Linkage::Internal);
    let ty = par.get_type();
    let main = module.add_function("karac.main", ty, None);
    par.as_global_value()
        .as_pointer_value()
        .replace_all_uses_with(main.as_global_value().as_pointer_value());
    let seq = module.add_function(IN_ORDER_MAIN, ty, None);
    let pooled = module.get_function("karac_mir_pooled").unwrap_or_else(|| {
        module.add_function("karac_mir_pooled", llcx.i8_type().fn_type(&[], false), None)
    });
    let b = llcx.create_builder();
    let entry = llcx.append_basic_block(main, "entry");
    let alone = llcx.append_basic_block(main, "alone");
    let tasks = llcx.append_basic_block(main, "tasks");
    b.position_at_end(entry);
    let workers = b
        .build_call(pooled, &[], "pooled")
        .map_err(e)?
        .try_as_basic_value()
        .unwrap_basic()
        .into_int_value();
    let one = b
        .build_int_compare(
            inkwell::IntPredicate::EQ,
            workers,
            llcx.i8_type().const_zero(),
            "one",
        )
        .map_err(e)?;
    b.build_conditional_branch(one, alone, tasks).map_err(e)?;
    for (block, f) in [(alone, seq), (tasks, par)] {
        b.position_at_end(block);
        let call = b.build_call(f, &[], "").map_err(e)?;
        // The new `karac.main` has no debug-info subprogram, so the
        // program's own entry, which may, is not inlined into it.
        let noinline = inkwell::attributes::Attribute::get_named_enum_kind_id("noinline");
        call.add_attribute(
            inkwell::attributes::AttributeLoc::Function,
            llcx.create_enum_attribute(noinline, 0),
        );
        let r = call.try_as_basic_value().unwrap_basic();
        b.build_return(Some(&r)).map_err(e)?;
    }
    Ok(())
}

/// The in-order program's module, to link beside the parallel one: its
/// `karac.main` is [`IN_ORDER_MAIN`], the C `main` is the parallel
/// module's, and every other definition takes a `#seq` suffix (a nameless
/// one, a `seq#anon` name), so no name meets the parallel module's, nor
/// one a codegen unit shares (`split.rs`). A name changes nothing LLVM
/// does with a definition, so this module optimises as its own build.
fn in_order_module(module: &Module<'_>) -> Result<(), String> {
    if let Some(f) = module.get_function("main") {
        // SAFETY: nothing in the module refers to the C entry.
        unsafe { f.delete() };
    }
    let entry = module
        .get_function("karac.main")
        .ok_or("no `karac.main` in the in-order program")?;
    entry.as_global_value().set_name(IN_ORDER_MAIN);
    let mut anon = 0;
    let mut rename = |g: inkwell::values::GlobalValue<'_>| {
        let name = g.get_name().to_string_lossy().into_owned();
        // LLVM's own (`llvm.global_ctors`, `llvm.used`) mean what their
        // name says.
        if name.starts_with("llvm.") {
            return;
        }
        let to = if name.is_empty() {
            anon += 1;
            format!("seq#anon.{anon}")
        } else {
            format!("{name}#seq")
        };
        g.set_name(&to);
    };
    for f in module.get_functions() {
        if f != entry && f.count_basic_blocks() > 0 {
            rename(f.as_global_value());
        }
    }
    for g in module.get_globals() {
        if g.get_initializer().is_some() {
            rename(g);
        }
    }
    Ok(())
}

fn compile_module<'ctx>(
    llcx: &'ctx Context,
    module: &Module<'ctx>,
    obj: &Path,
    opt: u8,
    entry: bool,
) -> Result<(), String> {
    let mut t = std::time::Instant::now();
    let tm = super::driver::create_target_machine()?;
    module.set_triple(&tm.get_triple());
    module.set_data_layout(&tm.get_target_data().get_data_layout());

    // The runtime declares the spawn-site table `extern`. Legacy codegen
    // emits it; the MIR backend does not (the JIT runner fills it in at run
    // time), so a program gets an empty one: one, in the module with the
    // C `main`.
    if entry {
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
    }

    // A program that runs tasks frees through the runtime's per-thread
    // lists (`route_heap`), decided once for all its units.
    let heap = module.get_function("karac_mir_task_spawn").is_some()
        && tm
            .get_triple()
            .as_str()
            .to_string_lossy()
            .contains("-linux-gnu");

    // The hot-swap table and its constructor stay in one unit.
    let hot_swap = module.get_global("karac_hotswap_table").is_some();
    if let Some(plan) = split::plan(module.as_mut_ptr()).filter(|_| !hot_swap) {
        crate::mir::lower::phase_time("plan codegen units", &mut t);
        let units = plan.units();
        let r = split::compile_units(module.as_mut_ptr(), &plan, obj, &|m, part| {
            optimize_and_emit(m, part, opt, &super::driver::create_target_machine()?, heap)
        });
        crate::mir::lower::phase_time(&format!("opt + emission, {units} units"), &mut t);
        return r;
    }
    optimize_and_emit_timed(module, obj, opt, &tm, heap)
}

fn optimize_and_emit_timed(
    module: &Module<'_>,
    obj: &Path,
    opt: u8,
    tm: &TargetMachine,
    heap: bool,
) -> Result<(), String> {
    let mut t = std::time::Instant::now();
    run_passes(module, opt, tm)?;
    if heap {
        route_heap(module);
    }
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
    heap: bool,
) -> Result<(), String> {
    run_passes(module, opt, tm)?;
    if heap {
        route_heap(module);
    }
    emit(module, obj, tm)
}

/// Points the module's calls of `malloc` and `free` at the runtime's
/// `karac_malloc` and `karac_free`, which keep small chunks on per-thread
/// lists once the pool runs more than one thread: past glibc's own short
/// per-thread cache, its `free` then takes a lock, and grain_intake's
/// 140,000 frees cost it about a millisecond at four workers
/// (B-2026-10-28-15). After the passes, which know the C names.
fn route_heap(module: &Module<'_>) {
    for (libc, ours) in [("malloc", "karac_malloc"), ("free", "karac_free")] {
        if let Some(f) = module.get_function(libc) {
            f.as_global_value().set_name(ours);
        }
    }
}

/// LLVM's full-unroll threshold, set once per process before any pipeline
/// runs: O3's 300 rather than O2's 150, the rest of the pipeline staying
/// O2. Kata 139's 24-trip window loop with its inner loop is fully
/// unrolled only at 300, and ran 1.18x legacy's instructions without it
/// (0.91x with). Over the 707 frozen-set programs it costs 0.7% of build
/// time and 0.15% of stripped size (B-2026-11-01-36).
/// `KARAC_MIR_UNROLL_THRESHOLD=<n>` sets another; 150 is LLVM's O2 value.
///
/// SimplifyCFG's two-entry phi folding threshold is 6 rather than 4, so a
/// short-circuit `n > 0 and <test>` whose test became a `ctpop` (costed
/// above a plain `and` on aarch64) still folds into one branch-free
/// condition. At 4, kata 342's loop kept `n > 0` as its own branch on
/// arm64, a second data-dependent branch per element that legacy's loop
/// does not have (B-2026-11-04-37); x86 already branches three times in
/// both. Over the 707 frozen-set programs on x86, 29 build differently,
/// 0.969x geomean, best 0.68x (kata 229), worst 1.05x.
/// `KARAC_MIR_PHI_FOLD_THRESHOLD=<n>` sets another; 4 is LLVM's.
///
/// `KARAC_MIR_ALIGN_BRANCHES=1` also keeps x86 jumps from crossing or
/// ending on a 32-byte line (clang's `-mbranches-within-32B-boundaries`:
/// the assembler pads, nothing else changes). It is off by default
/// because what it buys depends on the host, not the program
/// (B-2026-11-05-33). On a Cascade Lake Xeon whose microcode carries
/// the SKX102 fix, a line holding such a jump loses its decoded-uop
/// cache: kata 661 ran 0.49 s, 0.32 s aligned, and the 707 frozen-set
/// programs 0.914x geomean. Later on the same container, the same
/// unaligned binary of 661 ran 0.27 s and alignment bought nothing,
/// 0.985x geomean, while kata 207's queue loop ran 2.3x slower aligned.
///
/// Loops are unswitched on a condition they never change even when that
/// takes a second copy of the loop (`-enable-nontrivial-unswitch`, which
/// LLVM's pipeline turns on only at O3; rustc's O2 does it too). Kata
/// 492's `while area % width != 0` otherwise tests `area == i64.MIN` on
/// every step (4.02G against 3.45G instructions), and kata 277's
/// verification loop keeps asking `knows(cand, j)` after `cand` is the
/// planted celebrity (3.19G against 1.08G; B-2026-11-08-3).
/// `KARAC_MIR_UNSWITCH=0` leaves it to LLVM's level.
fn unroll_threshold() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let n = std::env::var("KARAC_MIR_UNROLL_THRESHOLD")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(300);
        let mut args = vec![
            std::ffi::CString::new("karac").unwrap_or_default(),
            std::ffi::CString::new(format!("-unroll-threshold={n}")).unwrap_or_default(),
        ];
        if std::env::var("KARAC_MIR_ALIGN_BRANCHES").as_deref() == Ok("1") {
            args.push(
                std::ffi::CString::new("-x86-branches-within-32B-boundaries").unwrap_or_default(),
            );
        }
        if std::env::var("KARAC_MIR_UNSWITCH").as_deref() != Ok("0") {
            args.push(std::ffi::CString::new("-enable-nontrivial-unswitch").unwrap_or_default());
            if let Ok(t) = std::env::var("KARAC_MIR_UNSWITCH_THRESHOLD") {
                args.push(
                    std::ffi::CString::new(format!("-unswitch-threshold={t}")).unwrap_or_default(),
                );
            }
        }
        let fold = std::env::var("KARAC_MIR_PHI_FOLD_THRESHOLD")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(6);
        args.push(
            std::ffi::CString::new(format!("-two-entry-phi-node-folding-threshold={fold}"))
                .unwrap_or_default(),
        );
        let argv: Vec<*const std::ffi::c_char> = args.iter().map(|a| a.as_ptr()).collect();
        // SAFETY: NUL-terminated strings that outlive the call; LLVM
        // copies what it keeps.
        unsafe {
            llvm_sys::support::LLVMParseCommandLineOptions(
                argv.len() as i32,
                argv.as_ptr(),
                std::ptr::null(),
            )
        };
    });
}

fn run_passes(module: &Module<'_>, opt: u8, tm: &TargetMachine) -> Result<(), String> {
    unroll_threshold();
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
        // A binary search's midpoint facts (`mir::midpoint`) meet its
        // `nums[mid]` check only once the search is inlined, after the
        // pipeline's own range passes: a second run folds it (kata 153).
        // Only a module that stated one pays for it.
        // `KARAC_MIR_MIDPOINT_REOPT=0` runs the pipeline once.
        if module.get_global_metadata_size(super::mir::MIDPOINT_MARK) > 0
            && std::env::var("KARAC_MIR_MIDPOINT_REOPT").as_deref() != Ok("0")
        {
            passes = format!("{passes},default<O{}>", opt.min(3));
        }
        module
            .run_passes(&passes, tm, PassBuilderOptions::create())
            .map_err(|e| format!("{passes}: {}", e.to_string()))?;
        // SAFETY: the module is live for the call.
        unsafe { walk_carry::carry_walks(module.as_mut_ptr()) };
        // In a loop that may write, a walk asks for its words on every
        // step: in line, that is the header's loads, not a call.
        if let Some(f) = module.get_function(WALK_WORDS) {
            let kind = |name| inkwell::attributes::Attribute::get_named_enum_kind_id(name);
            let fun = inkwell::attributes::AttributeLoc::Function;
            f.remove_enum_attribute(fun, kind("noinline"));
            f.add_attribute(
                fun,
                module
                    .get_context()
                    .create_enum_attribute(kind("alwaysinline"), 0),
            );
            module
                .run_passes("always-inline", tm, PassBuilderOptions::create())
                .map_err(|e| format!("always-inline: {}", e.to_string()))?;
        }
        // SAFETY: the module is live for the calls.
        unsafe {
            carry_store::carry_stores(module.as_mut_ptr());
            select_branch::mark_index_steps(module.as_mut_ptr());
            overflow_flag::flag_steps(module.as_mut_ptr());
        }
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
