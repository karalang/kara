//! CG-5 spike, second half: take a REAL Kāra `#[gpu]` kernel body through
//! `codegen`'s ordinary lowering and emit **NVPTX** from it.
//!
//! The assessment
//! ([`docs/spikes/gpu-llvm-offload-assessment.md`](../docs/spikes/gpu-llvm-offload-assessment.md))
//! recommended exactly one next step: find out whether `codegen` can lower a
//! real kernel body to NVPTX, or whether GPU address-space / calling-convention
//! constraints force a separate lowering path. That question gates every host-
//! runtime option, and it needs no GPU to answer — this ends at emitted PTX.
//!
//! # Why this can reuse codegen rather than re-implement it
//!
//! `nvptx_body_probe` measured that every kernel shape which is LEGAL after
//! B-2026-08-19-1 lowers to device-clean IR: no `karac_*` runtime calls, no
//! unwind edges. That is not a coincidence — the `#[gpu]` effect gate already
//! proves no allocation / channels / host I/O / explicit panics, and the
//! trapping-arithmetic rule removed the last implicit panic sites. So the body
//! needs no rewriting, only re-targeting.
//!
//! # Method
//!
//! Rather than build a second `Codegen` against an NVPTX target machine — which
//! would mean threading a target through a large stateful struct — this takes
//! the IR codegen already produces, lifts the kernel function out of it, and
//! links it into a fresh NVPTX module under a hand-written kernel wrapper
//! (thread index, bounds check, load, call, store) plus the `nvvm.annotations`
//! that mark an entry point. If the body survives that transplant unchanged,
//! the body lowering is target-independent, which is the thing in question.
//!
//! Run: `cargo run --release --features llvm --example nvptx_kernel_spike`

use inkwell::context::Context;
use inkwell::memory_buffer::MemoryBuffer;
use inkwell::targets::{
    CodeModel, FileType, InitializationConfig, RelocMode, Target, TargetTriple,
};
use inkwell::OptimizationLevel;

fn main() {
    let cases: &[(&str, &str, &str)] = &[
        ("scalar map", "f32", "#[gpu]\nfn k(x: f32) -> f32 { x * 2.0 }"),
        (
            "locals",
            "f32",
            "#[gpu]\nfn k(x: f32) -> f32 { let t: f32 = x * 2.0; t + 1.0 }",
        ),
        (
            "for-range accumulator",
            "f32",
            "#[gpu]\nfn k(x: f32) -> f32 {\n    let mut acc: f32 = 0.0;\n    for n in 0..4 { acc = acc + x; }\n    acc\n}",
        ),
        (
            "statement if",
            "f32",
            "#[gpu]\nfn k(x: f32) -> f32 {\n    let mut acc: f32 = 0.0;\n    if x > 2.0 { acc = x; } else { acc = 0.0 - x; }\n    acc\n}",
        ),
        (
            "value match",
            "i32",
            "#[gpu]\nfn k(x: i32) -> i32 { match x { 0 => 10, 1 => 20, _ => 30 } }",
        ),
        (
            "wrapping accumulator",
            "i32",
            "#[gpu]\nfn k(x: i32) -> i32 {\n    let mut acc: i32 = 0;\n    for n in 0..4 { acc = acc.wrapping_add(x); }\n    acc\n}",
        ),
    ];

    let mut ok = 0usize;
    for (name, elem, kernel) in cases {
        match emit_ptx(kernel, elem) {
            Ok(ptx) => {
                ok += 1;
                let entry = ptx
                    .lines()
                    .find(|l| l.contains(".visible .entry"))
                    .unwrap_or("<no .entry!>")
                    .trim();
                let insns = ptx
                    .lines()
                    .filter(|l| {
                        let t = l.trim();
                        !t.is_empty()
                            && !t.starts_with('/')
                            && !t.starts_with('.')
                            && !t.ends_with(':')
                    })
                    .count();
                println!("{name:<24} OK   {insns:>4} insns   {entry}");
            }
            Err(e) => println!("{name:<24} FAIL {}", e.lines().next().unwrap_or("")),
        }
    }
    println!("\n{ok}/{} kernels reached PTX", cases.len());

    // Control flow is the claim worth checking: that the body did not merely
    // compile, but that loops and branches survived into device code. Written
    // out so the PTX can be read rather than only counted.
    let dir = std::env::var("KARA_PTX_OUT").unwrap_or_else(|_| "/tmp/kara-ptx".into());
    let _ = std::fs::create_dir_all(&dir);
    println!();
    for (name, elem, kernel) in cases {
        if let Ok(ptx) = emit_ptx(kernel, elem) {
            let slug: String = name
                .chars()
                .map(|c| if c.is_alphanumeric() { c } else { '_' })
                .collect();
            let path = format!("{dir}/{slug}.ptx");
            let _ = std::fs::write(&path, &ptx);
            println!("{name:<24} -> {path}");
        }
    }
}

/// Compile a Kāra `#[gpu]` kernel through codegen, transplant it into an NVPTX
/// module under a kernel wrapper, and emit PTX.
fn emit_ptx(kernel: &str, elem: &str) -> Result<String, String> {
    // Call the kernel directly: `gpu.dispatch` would route the WGSL path, and
    // what is under test is the BODY lowering.
    let arg = if elem == "i32" { "3" } else { "3.0" };
    let src =
        format!("{kernel}\nfn main() {{\n    let r = k({arg});\n    println(f\"{{r}}\")\n}}\n");
    let ir = compile(&src)?;
    let body = extract_fn(&ir, "k").ok_or("no `k` in the emitted module")?;

    let llty = if elem == "i32" { "i32" } else { "float" };
    // The wrapper is the part a real backend would generate: thread index,
    // bounds check against a length operand, load, call, store. Address space 1
    // is NVPTX's global space.
    let module_ir = format!(
        r#"target triple = "nvptx64-nvidia-cuda"

{body}
declare i32 @llvm.nvvm.read.ptx.sreg.tid.x()
declare i32 @llvm.nvvm.read.ptx.sreg.ctaid.x()
declare i32 @llvm.nvvm.read.ptx.sreg.ntid.x()

define void @k_kernel(ptr addrspace(1) %in, ptr addrspace(1) %out, i32 %n) {{
entry:
  %tid = call i32 @llvm.nvvm.read.ptx.sreg.tid.x()
  %ctaid = call i32 @llvm.nvvm.read.ptx.sreg.ctaid.x()
  %ntid = call i32 @llvm.nvvm.read.ptx.sreg.ntid.x()
  %base = mul i32 %ctaid, %ntid
  %idx = add i32 %base, %tid
  %inb = icmp slt i32 %idx, %n
  br i1 %inb, label %body, label %done

body:
  %src = getelementptr {llty}, ptr addrspace(1) %in, i32 %idx
  %val = load {llty}, ptr addrspace(1) %src
  %res = call {llty} @k({llty} %val)
  %dst = getelementptr {llty}, ptr addrspace(1) %out, i32 %idx
  store {llty} %res, ptr addrspace(1) %dst
  br label %done

done:
  ret void
}}

!nvvm.annotations = !{{!0}}
!0 = !{{ptr @k_kernel, !"kernel", i32 1}}
"#
    );

    Target::initialize_nvptx(&InitializationConfig::default());
    let ctx = Context::create();
    // inkwell requires a NUL-terminated slice here.
    let mut ir_bytes = module_ir.into_bytes();
    ir_bytes.push(0);
    let buf = MemoryBuffer::create_from_memory_range(&ir_bytes, "kara_nvptx");
    let module = ctx
        .create_module_from_ir(buf)
        .map_err(|e| format!("IR parse failed: {}", e.to_string().replace('\n', " | ")))?;

    let triple = TargetTriple::create("nvptx64-nvidia-cuda");
    let target = Target::from_triple(&triple).map_err(|e| format!("no nvptx target: {e}"))?;
    let tm = target
        .create_target_machine(
            &triple,
            "sm_70",
            "",
            OptimizationLevel::Aggressive,
            RelocMode::PIC,
            CodeModel::Default,
        )
        .ok_or("failed to create nvptx target machine")?;
    module.set_data_layout(&tm.get_target_data().get_data_layout());
    module
        .verify()
        .map_err(|e| format!("verify failed: {}", e.to_string().replace('\n', " | ")))?;

    let mem = tm
        .write_to_memory_buffer(&module, FileType::Assembly)
        .map_err(|e| format!("PTX emission failed: {e}"))?;
    Ok(String::from_utf8_lossy(mem.as_slice()).to_string())
}

fn compile(src: &str) -> Result<String, String> {
    let parsed = karac::parse(src);
    if !parsed.errors.is_empty() {
        return Err(format!("{:?}", parsed.errors[0]));
    }
    let program = parsed.program;
    let resolved = karac::resolve(&program);
    if !resolved.errors.is_empty() {
        return Err(format!("{:?}", resolved.errors[0]));
    }
    let tc = karac::typecheck(&program, &resolved);
    if !tc.errors.is_empty() {
        return Err(format!("{:?}", tc.errors[0]));
    }
    let own = karac::ownershipcheck(&program, &tc);
    karac::codegen::compile_to_ir(&program, Some(&own), None).map_err(|e| e.message)
}

/// Lift one `define ... @name(` block out of a textual module, dropping the
/// trailing attribute-group reference (`#0`) — those groups are not carried
/// across, and NVPTX rejects some native-target attributes outright.
fn extract_fn(ir: &str, name: &str) -> Option<String> {
    let needle = format!("@{name}(");
    let start = ir
        .lines()
        .position(|l| l.starts_with("define") && l.contains(&needle))?;
    let mut out = String::new();
    for (n, line) in ir.lines().skip(start).enumerate() {
        if n == 0 {
            // `define ... @k(float %x) #0 {` → strip the `#0`.
            let cleaned = match (line.find(") "), line.rfind('{')) {
                (Some(p), Some(_)) => {
                    let (head, _) = line.split_at(p + 1);
                    format!("{head} {{")
                }
                _ => line.to_string(),
            };
            out.push_str(&cleaned);
        } else {
            out.push_str(line);
        }
        out.push('\n');
        if line == "}" {
            break;
        }
    }
    Some(out)
}
