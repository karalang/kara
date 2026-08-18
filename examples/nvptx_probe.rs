//! Feasibility probe for the CG-5 GPU-backend assessment
//! (`docs/spikes/gpu-llvm-offload-assessment.md`): can the LLVM we
//! already link emit a real NVPTX kernel, and does the same module
//! round-trip through the AMDGPU backend?
//!
//! This answers the one question the assessment cannot answer by
//! reading code: whether `--target cuda` needs a different LLVM build
//! (a large, distribution-level cost) or is reachable from the stock
//! 18.1 we ship against (a contained one). It builds a
//! `__global__`-equivalent kernel by hand — an element-wise `out[i] =
//! in[i] * 2.0` over the thread index, i.e. exactly the slice-0 kernel
//! the WGSL path already runs — and prints the emitted assembly.
//!
//! Run: `cargo run --release --features llvm --example nvptx_probe`
//! (add `amdgcn` as an argument to emit the AMDGPU variant instead).

use inkwell::context::Context;
use inkwell::targets::{
    CodeModel, FileType, InitializationConfig, RelocMode, Target, TargetTriple,
};
use inkwell::AddressSpace;
use inkwell::OptimizationLevel;

fn main() {
    let amd = std::env::args().any(|a| a == "amdgcn");

    if amd {
        Target::initialize_amd_gpu(&InitializationConfig::default());
    } else {
        Target::initialize_nvptx(&InitializationConfig::default());
    }

    let (triple_str, cpu, kernel_md, global_as) = if amd {
        ("amdgcn-amd-amdhsa", "gfx90a", "amdgpu_kernel", 1u16)
    } else {
        ("nvptx64-nvidia-cuda", "sm_70", "kernel", 1u16)
    };

    let ctx = Context::create();
    let module = ctx.create_module("karac_gpu_probe");
    let triple = TargetTriple::create(triple_str);
    module.set_triple(&triple);

    let target = Target::from_triple(&triple).expect("target not registered in this LLVM build");
    let tm = target
        .create_target_machine(
            &triple,
            cpu,
            "",
            OptimizationLevel::Aggressive,
            RelocMode::PIC,
            CodeModel::Default,
        )
        .expect("failed to create target machine");
    module.set_data_layout(&tm.get_target_data().get_data_layout());

    // fn double(in: *global f32, out: *global f32) — one invocation per element.
    let f32_t = ctx.f32_type();
    let i32_t = ctx.i32_type();
    let ptr_t = ctx.ptr_type(AddressSpace::from(global_as));
    let fn_t = ctx
        .void_type()
        .fn_type(&[ptr_t.into(), ptr_t.into()], false);
    let func = module.add_function("double_kernel", fn_t, None);
    let entry = ctx.append_basic_block(func, "entry");
    let b = ctx.create_builder();
    b.position_at_end(entry);

    // Thread index: the vendor intrinsic each backend lowers natively.
    let tid_intrinsic = if amd {
        "llvm.amdgcn.workitem.id.x"
    } else {
        "llvm.nvvm.read.ptx.sreg.tid.x"
    };
    let tid_fn = module.add_function(tid_intrinsic, i32_t.fn_type(&[], false), None);
    let tid = b
        .build_call(tid_fn, &[], "tid")
        .unwrap()
        .try_as_basic_value()
        .unwrap_basic()
        .into_int_value();

    let in_ptr = func.get_nth_param(0).unwrap().into_pointer_value();
    let out_ptr = func.get_nth_param(1).unwrap().into_pointer_value();
    let src = unsafe { b.build_gep(f32_t, in_ptr, &[tid], "src").unwrap() };
    let val = b.build_load(f32_t, src, "val").unwrap().into_float_value();
    let doubled = b
        .build_float_mul(val, f32_t.const_float(2.0), "doubled")
        .unwrap();
    let dst = unsafe { b.build_gep(f32_t, out_ptr, &[tid], "dst").unwrap() };
    b.build_store(dst, doubled).unwrap();
    b.build_return(None).unwrap();

    // Mark it as a kernel entry point, not a device function.
    if amd {
        func.set_call_conventions(91); // AMDGPU_KERNEL
    } else {
        let md = ctx.metadata_node(&[
            func.as_global_value().as_pointer_value().into(),
            ctx.metadata_string(kernel_md).into(),
            i32_t.const_int(1, false).into(),
        ]);
        module
            .add_global_metadata("nvvm.annotations", &md)
            .expect("failed to add nvvm.annotations");
    }

    if let Err(e) = module.verify() {
        eprintln!("MODULE VERIFY FAILED:\n{}", e.to_string());
        std::process::exit(1);
    }

    let buf = tm
        .write_to_memory_buffer(&module, FileType::Assembly)
        .expect("failed to emit assembly");
    let asm = String::from_utf8_lossy(buf.as_slice()).to_string();

    println!("=== target: {triple_str} (cpu {cpu}) ===");
    println!("{asm}");
}
