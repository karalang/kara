//! GPU compute spine — phase-10 GPU codegen, spike **slices 0a + 0c**
//! ([`docs/spikes/gpu-wgsl-slice0.md`]).
//!
//! Proves the wgpu plumbing end-to-end: a WGSL compute shader applied
//! element-wise to an `f32` buffer, dispatched on the platform's native GPU
//! API (Metal on macOS, Vulkan/DX12 elsewhere) and read back. The internal
//! [`dispatch_f32_map`] helper (slice-0a) is the spine; the WGSL it runs is
//! produced by the compiler's `src/gpu_wgsl.rs` emitter (slice-0b). Slice-0c
//! exposes it to compiled Kāra through the byte-oriented C symbol
//! [`karac_runtime_gpu_map`], which `gpu.dispatch(kernel, buffer)` lowers to
//! (type-agnostic — `f32`/`i32`/`u32` share one path, the WGSL declares the
//! element type). Behind the opt-in `gpu` feature; not compiled into any
//! production or wasm archive — the compiler links the dedicated
//! `libkarac_runtime_gpu.a` (built `--features gpu`) only when a program
//! references this symbol.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use wgpu::util::DeviceExt;

/// The GPU adapter + device + queue, created once and reused across every
/// `gpu.dispatch`. Requesting a fresh adapter/device per dispatch (the pre-4a
/// shape) was ~ms of pure setup on every call — the dominant cost of an
/// iterative sim's dispatch loop (the round-trip bench spent most of its time
/// here, not in compute or transfer). wgpu `Device`/`Queue` are `Send + Sync`,
/// so a process-wide `OnceLock` is sound; on native Metal the adapter/device
/// requests resolve synchronously, so the one-time `block_on` never suspends.
struct GpuContext {
    device: wgpu::Device,
    queue: wgpu::Queue,
}

/// CG-6: pick the adapter, honoring `KARAC_GPU=<index>` and
/// `KARAC_GPU_BACKEND=cpu`.
///
/// `KARAC_GPU` unset → wgpu's default `request_adapter` (the platform's
/// preferred high-performance device), exactly as before. Set → the N-th
/// adapter in `enumerate_adapters` order; a non-numeric value or an
/// out-of-range index is a structured error LISTING every available adapter
/// (index, name, backend, device type), so the fix is copy-paste visible.
///
/// `KARAC_GPU_BACKEND=cpu` (the debug escape hatch, checked first) forces a
/// SOFTWARE (`DeviceType::Cpu`) adapter — lavapipe/llvmpipe on Linux, WARP on
/// Windows — for exercising the real GPU pipeline on a GPU-less host; no such
/// adapter is a structured error naming the software-driver fix, and any value
/// other than `cpu` is rejected outright (same UX as `KARAC_GPU`).
///
/// The error is returned (not aborted) so the once-cell caller can route it
/// through the same fatal path as "no adapter"; unit tests exercise it
/// in-process.
fn select_adapter(
    instance: &wgpu::Instance,
    requested: Option<&str>,
    backend: Option<&str>,
) -> Result<wgpu::Adapter, String> {
    // `KARAC_GPU_BACKEND=cpu` — force a SOFTWARE (CPU) adapter: lavapipe/llvmpipe
    // via wgpu's Vulkan backend on Linux, WARP on Windows. A debug escape hatch
    // (CG-6 deferred leg) for exercising `gpu.dispatch` where no real GPU exists;
    // the interpreter remains the kernel-*logic* hatch, this exercises the actual
    // GPU pipeline. Only `cpu` is recognized. Takes precedence over `KARAC_GPU`
    // (which indexes the raw enumerate order) — the two are alternative selectors.
    if let Some(b) = backend {
        if !b.eq_ignore_ascii_case("cpu") {
            return Err(format!(
                "KARAC_GPU_BACKEND only supports `cpu` (a software adapter for \
                 GPU-less debugging), got `{b}`"
            ));
        }
        let adapters = pollster::block_on(instance.enumerate_adapters(wgpu::Backends::all()));
        if let Some(pos) = adapters
            .iter()
            .position(|a| a.get_info().device_type == wgpu::DeviceType::Cpu)
        {
            return Ok(adapters
                .into_iter()
                .nth(pos)
                .expect("position checked above"));
        }
        return Err(format!(
            "KARAC_GPU_BACKEND=cpu: no software (CPU) adapter is available. Install a \
             software Vulkan implementation (Linux: `mesa-vulkan-drivers` / lavapipe; \
             Windows: the built-in WARP). Available adapters:\n{}",
            adapter_listing(&adapters)
        ));
    }
    let Some(raw) = requested else {
        return pollster::block_on(
            instance.request_adapter(&wgpu::RequestAdapterOptions::default()),
        )
        .map_err(|_| "no GPU adapter is available on this host".to_string());
    };
    let adapters = pollster::block_on(instance.enumerate_adapters(wgpu::Backends::all()));
    let Ok(idx) = raw.parse::<usize>() else {
        return Err(format!(
            "KARAC_GPU must be a device index, got `{raw}`. Available:\n{}",
            adapter_listing(&adapters)
        ));
    };
    if idx >= adapters.len() {
        return Err(format!(
            "KARAC_GPU={idx} is out of range ({} adapter(s) available). Available:\n{}",
            adapters.len(),
            adapter_listing(&adapters)
        ));
    }
    Ok(adapters.into_iter().nth(idx).expect("index checked above"))
}

/// Structured adapter listing (index, name, backend, device type) shared by the
/// `KARAC_GPU` and `KARAC_GPU_BACKEND` selection errors — the fix is copy-paste
/// visible.
fn adapter_listing(adapters: &[wgpu::Adapter]) -> String {
    let mut msg = String::new();
    if adapters.is_empty() {
        msg.push_str("  (no adapters found)\n");
    }
    for (i, a) in adapters.iter().enumerate() {
        let info = a.get_info();
        msg.push_str(&format!(
            "  KARAC_GPU={i}: {} [{:?}, {:?}]\n",
            info.name, info.backend, info.device_type
        ));
    }
    msg
}

fn gpu_context() -> Option<&'static GpuContext> {
    static CTX: OnceLock<Option<GpuContext>> = OnceLock::new();
    CTX.get_or_init(|| {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        // CG-6: `KARAC_GPU=<index>` device override. A selection ERROR (bad
        // index / unparseable) must not silently fall back to the default
        // adapter — print the structured message and abort here, mirroring
        // the no-adapter posture (a `gpu.dispatch` has no CPU fallback).
        let requested = std::env::var("KARAC_GPU").ok();
        let backend = std::env::var("KARAC_GPU_BACKEND").ok();
        let explicit = requested.is_some() || backend.is_some();
        let adapter = match select_adapter(&instance, requested.as_deref(), backend.as_deref()) {
            Ok(a) => a,
            Err(msg) => {
                if explicit {
                    crate::fatal::eprint_fmt(format_args!("runtime error: {msg}\n"));
                    std::process::exit(1);
                }
                // No override + no adapter: preserve the existing behaviour —
                // yield None and let each entry point report its own
                // structured no-GPU error.
                return None;
            }
        };
        // Request the ADAPTER's full limits, not `Limits::default()`. The default
        // is wgpu's conservative cross-platform floor (`max_storage_buffers_per_
        // shader_stage = 8`), which caps a Path-A SoA kernel at 4 fields (in+out
        // buffers). The real Slipstream D2Q9 collide is 9 fields → 18 storage
        // buffers; native Metal on Apple Silicon supports 31/stage, so requesting
        // `adapter.limits()` (always satisfiable by construction — it's what the
        // adapter reports) lifts the cap to the hardware ceiling. Enables any
        // multi-field `#[gpu]` kernel up to the device's real limit.
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("gpu-cg4-device"),
            required_limits: adapter.limits(),
            ..Default::default()
        }))
        .ok()?;
        Some(GpuContext { device, queue })
    })
    .as_ref()
}

/// The compiled compute pipeline for `wgsl`, cached by shader source (GPU-SLIP-4a).
/// An iterative sim dispatches the same handful of shaders thousands of times;
/// compiling WGSL → the Metal pipeline every call was ~ms of the per-dispatch
/// cost. Keyed by the exact shader string (the emitter is deterministic, so the
/// same kernel produces the same WGSL). Returns an `Arc` so the cache lock is
/// released before the (awaited) dispatch runs.
fn compute_pipeline(device: &wgpu::Device, wgsl: &str) -> Arc<wgpu::ComputePipeline> {
    static PIPELINES: OnceLock<Mutex<HashMap<String, Arc<wgpu::ComputePipeline>>>> =
        OnceLock::new();
    let cache = PIPELINES.get_or_init(|| Mutex::new(HashMap::new()));
    let mut map = cache.lock().unwrap();
    if let Some(p) = map.get(wgsl) {
        return p.clone();
    }
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("gpu-cg4-shader"),
        source: wgpu::ShaderSource::Wgsl(wgsl.into()),
    });
    let pipeline = Arc::new(
        device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("gpu-cg4-pipeline"),
            layout: None,
            module: &module,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        }),
    );
    map.insert(wgsl.to_string(), pipeline.clone());
    pipeline
}

/// Run `wgsl` over `input` element-wise and return the result buffer.
///
/// The shader must declare `@compute @workgroup_size(64) fn main(...)` with
/// binding 0 = `var<storage, read> input: array<f32>` and binding 1 =
/// `var<storage, read_write> output: array<f32>` in `@group(0)`.
///
/// Returns `None` when no GPU adapter is available (headless CI, no driver,
/// `KARAC_GPU_BACKEND` unset on a GPU-less box). The internal test treats that
/// as a graceful skip; the `karac_runtime_gpu_map` C entry point turns it into
/// a fatal, diagnosed abort — a compiled `gpu.dispatch` has no CPU fallback
/// (the kernel exists only as GPU-side WGSL), so a GPU-less host is a hard
/// error, not a silent no-op. Test-only: the compiled path goes through the
/// byte-oriented [`karac_runtime_gpu_map`]; this typed `f32` wrapper only backs
/// the slice-0a spine test.
#[cfg(test)]
pub fn dispatch_f32_map(wgsl: &str, input: &[f32]) -> Option<Vec<f32>> {
    // `&[f32]` → `&[u8]` (little-endian) without pulling in `bytemuck`, run the
    // byte-oriented core, then reinterpret the result bytes as `f32`.
    let mut bytes = Vec::with_capacity(std::mem::size_of_val(input));
    for &x in input {
        bytes.extend_from_slice(&x.to_le_bytes());
    }
    let out = pollster::block_on(dispatch_bytes_async(wgsl, &bytes, 4))?;
    Some(
        out.chunks_exact(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect(),
    )
}

/// C entry point for `gpu.dispatch(kernel, buffer)` — slice-0c.
///
/// Runs the compile-time-baked `wgsl` shader over an `n`-element input buffer
/// of `elem_size`-byte elements and returns a **freshly `malloc`'d**
/// `n * elem_size`-byte output buffer. Type-agnostic: the GPU buffer is raw
/// bytes and the WGSL shader declares the element type (`array<f32>` /
/// `array<i32>` / `array<u32>` — all 4-byte in slice-0), so `f32` / `i32` /
/// `u32` dispatch all share this one path. The compiler wraps the returned
/// pointer into an owned `Vec[T]` of length/capacity `n`; the buffer comes from
/// the same platform `malloc` the collection codegen uses
/// ([`crate::alloc::karac_alloc_or_panic`]), so the Kāra-side `Vec` drop frees
/// it with the matching `free`. An empty input (`n == 0`) skips the GPU and
/// returns a unique non-null one-byte allocation (never dereferenced) so the
/// owned-`Vec` contract holds without a null special case.
///
/// # Safety
///
/// `wgsl_ptr` must point to `wgsl_len` valid UTF-8 bytes and `in_ptr` to
/// `n * elem_size` valid bytes for the duration of the call (both are
/// compile-time constants / a live buffer at the call site). The returned
/// pointer transfers ownership to the caller.
///
/// # Aborts
///
/// On no available GPU adapter — the dispatch cannot fall back to the CPU, so
/// this writes a diagnostic and aborts rather than returning null (which the
/// caller would wrap into a length-`n` `Vec` over garbage).
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_gpu_map(
    wgsl_ptr: *const u8,
    wgsl_len: usize,
    in_ptr: *const u8,
    n: usize,
    elem_size: usize,
) -> *mut u8 {
    unsafe {
        let byte_len = n.saturating_mul(elem_size);

        // Empty dispatch: a unique non-null allocation the caller never reads.
        if byte_len == 0 {
            return crate::alloc::karac_alloc_or_panic(1);
        }

        let wgsl_bytes = std::slice::from_raw_parts(wgsl_ptr, wgsl_len);
        let Ok(wgsl) = std::str::from_utf8(wgsl_bytes) else {
            crate::fatal::write_stderr(b"panic: gpu.dispatch shader is not valid UTF-8\n");
            std::process::abort();
        };
        let input = std::slice::from_raw_parts(in_ptr, byte_len);

        let Some(output) = pollster::block_on(dispatch_bytes_async(wgsl, input, elem_size)) else {
            crate::fatal::write_stderr(
                b"panic: gpu.dispatch found no available GPU adapter (no CPU fallback)\n",
            );
            std::process::abort();
        };
        debug_assert_eq!(output.len(), byte_len, "element-wise map preserves length");

        // Hand the result back through the collection allocator so the owned
        // `Vec[T]` the compiler builds frees it with the matching `free`.
        let out = crate::alloc::karac_alloc_or_panic(byte_len);
        std::ptr::copy_nonoverlapping(output.as_ptr(), out, byte_len);
        out
    }
}

/// C entry point for `gpu.sum(buffer)` — a WHOLE-BUFFER REDUCTION returning a
/// scalar (B-2026-08-19-10, slice 1).
///
/// Unlike [`karac_runtime_gpu_map`], the result is one value rather than a
/// buffer, so the shader's convention is that lane 0 of each workgroup writes
/// that workgroup's partial to `output[workgroup_id]` and every other output
/// slot is ignored.
///
/// **Any length.** Each dispatch collapses every workgroup's `WORKGROUP`-wide
/// chunk to one partial at `output[workgroup_id]`, so re-dispatching the same
/// shader over those partials converges — a buffer longer than one workgroup
/// is a TREE OF TREES, and the grouping is part of the answer (see the loop
/// below). `identity` is the operation's own (`0.0` for a sum, `1.0` for a
/// product): the shader pads a short chunk with it, and it is also the answer
/// for an empty buffer, which needs no device at all.
///
/// **The shader defines the summation order** (pad to the workgroup width with
/// the identity, then halve), and the interpreter twin reproduces it exactly —
/// a GPU tree reduction is not a left fold, and f32 addition is not
/// associative, so the order is language semantics rather than an
/// implementation detail. That is what lets `karac run` and `karac build`
/// agree bit-for-bit instead of within an epsilon.
///
/// # Safety
///
/// `wgsl_ptr`/`wgsl_len` a valid UTF-8 shader; `in_ptr` points to `n` valid
/// `f32` values. Aborts on no available GPU adapter (no CPU fallback), same as
/// the map entry point.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_gpu_reduce_f32(
    wgsl_ptr: *const u8,
    wgsl_len: usize,
    in_ptr: *const f32,
    n: usize,
    identity: f32,
) -> f32 {
    unsafe {
        // Empty reduction is the operation's identity, and needs no device.
        // It must be the OP's identity, not `0.0`: an empty `gpu.prod` is 1,
        // and the interpreter twin says so — returning 0 here would be a
        // run/build divergence on the one input that never reaches a device.
        if n == 0 {
            return identity;
        }
        // Must match the shader's `@workgroup_size`, its `scratch` array
        // length, and `reduce_kernel::GPU_REDUCE_WIDTH`.
        const WORKGROUP: usize = 64;

        let wgsl_bytes = std::slice::from_raw_parts(wgsl_ptr, wgsl_len);
        let Ok(wgsl) = std::str::from_utf8(wgsl_bytes) else {
            crate::fatal::write_stderr(b"panic: gpu.sum shader is not valid UTF-8\n");
            std::process::abort();
        };

        let elem_size = std::mem::size_of::<f32>();
        let mut level: Vec<u8> =
            std::slice::from_raw_parts(in_ptr as *const u8, n * elem_size).to_vec();
        let mut count = n;

        // MULTI-WORKGROUP FOLD. Each dispatch reduces every workgroup's chunk
        // to one partial at `output[workgroup_id]`, so a pass over `count`
        // elements yields `ceil(count / WORKGROUP)` partials. Re-dispatching
        // the SAME shader over those partials converges to one value.
        //
        // The chunking is what makes the answer deterministic — and it is
        // observable: a 4096-element sum is a tree of trees, not a flat
        // 4096-wide tree, and in f32 those differ. `reduce_kernel`'s twin
        // reproduces this recursion exactly, which is what keeps run == build
        // for long buffers as well as short ones.
        while count > 1 {
            let Some(output) = pollster::block_on(dispatch_bytes_async(wgsl, &level, elem_size))
            else {
                crate::fatal::write_stderr(
                    b"panic: gpu.sum found no available GPU adapter (no CPU fallback)\n",
                );
                std::process::abort();
            };
            let partials = count.div_ceil(WORKGROUP);
            // Keep only the slots the workgroups actually wrote; the rest of
            // the output buffer is scratch from the previous level.
            level = output[..partials * elem_size].to_vec();
            count = partials;
        }

        f32::from_le_bytes([level[0], level[1], level[2], level[3]])
    }
}

/// C entry point for `gpu.dispatch(kernel, buffer)` over a **SoA `layout`-block
/// buffer** — CG-4 (layout groups → coalesced GPU buffers).
///
/// Generalizes [`karac_runtime_gpu_map`] from one buffer to `n_buffers` — one
/// per layout group (Path A: one field per group, so each group's backing array
/// is a contiguous `array<f32>`). All `n_buffers` inputs share the same element
/// count `n` and `elem_size`. Bindings follow the emitter's convention: input
/// buffers occupy `@binding(0..n_buffers)`, outputs `@binding(n_buffers..2*n_buffers)`.
/// Each output is a freshly `malloc`'d `n * elem_size`-byte buffer; the `k`-th
/// result pointer is written into `out_ptrs[k]` (a caller-provided array of
/// `n_buffers` slots), which codegen scatters back into the SoA `Vec`'s per-group
/// pointers. Empty input (`n == 0`) writes a unique non-null 1-byte allocation to
/// every slot (never dereferenced), mirroring the single-buffer contract.
///
/// # Safety
///
/// `wgsl_ptr`/`wgsl_len` a valid UTF-8 shader; `in_ptrs` an array of `n_buffers`
/// pointers, each to `n * elem_size` valid bytes; `out_ptrs` an array of
/// `n_buffers` writable pointer slots. Each written pointer transfers ownership
/// to the caller. Aborts on no available GPU adapter (no CPU fallback), same as
/// the single-buffer entry point.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_gpu_map_multi(
    wgsl_ptr: *const u8,
    wgsl_len: usize,
    n_buffers: usize,
    in_ptrs: *const *const u8,
    n: usize,
    elem_size: usize,
    out_ptrs: *mut *mut u8,
) {
    unsafe {
        let byte_len = n.saturating_mul(elem_size);
        let out_slots = std::slice::from_raw_parts_mut(out_ptrs, n_buffers);

        // Empty dispatch: a unique non-null allocation per group, never read.
        if byte_len == 0 || n_buffers == 0 {
            for slot in out_slots.iter_mut() {
                *slot = crate::alloc::karac_alloc_or_panic(1);
            }
            return;
        }

        let wgsl_bytes = std::slice::from_raw_parts(wgsl_ptr, wgsl_len);
        let Ok(wgsl) = std::str::from_utf8(wgsl_bytes) else {
            crate::fatal::write_stderr(b"panic: gpu.dispatch shader is not valid UTF-8\n");
            std::process::abort();
        };
        let in_ptr_slice = std::slice::from_raw_parts(in_ptrs, n_buffers);
        let inputs: Vec<&[u8]> = in_ptr_slice
            .iter()
            .map(|&p| std::slice::from_raw_parts(p, byte_len))
            .collect();

        let Some(outputs) = pollster::block_on(dispatch_multi_bytes_async(wgsl, &inputs, &[], n))
        else {
            crate::fatal::write_stderr(
                b"panic: gpu.dispatch found no available GPU adapter (no CPU fallback)\n",
            );
            std::process::abort();
        };
        debug_assert_eq!(outputs.len(), n_buffers, "one output buffer per group");

        for (slot, obytes) in out_slots.iter_mut().zip(outputs.iter()) {
            debug_assert_eq!(obytes.len(), byte_len, "element-wise map preserves length");
            let out = crate::alloc::karac_alloc_or_panic(byte_len);
            std::ptr::copy_nonoverlapping(obytes.as_ptr(), out, byte_len);
            *slot = out;
        }
    }
}

/// C entry point for a struct-SoA `gpu.dispatch` — CG-4 / GPU-LBM-3's codegen
/// target. Handles multi-field layout groups (each group's element is a coalesced
/// sub-struct of `group_strides[k]` bytes).
///
/// Dispatches the kernel over `n_groups` coalesced input group-arrays (`in_ptrs[k]`,
/// each `n * group_strides[k]` bytes) and returns a single **AoS** result buffer.
/// The shader (bindings `0..n_groups` in, `n_groups..2*n_groups` out) writes
/// `n_groups` output group-arrays, which are scattered into one `n * aos_stride`
/// buffer field by field: for each of the `n_fields` struct fields, field `f` lives
/// in group `field_group[f]` at byte offset `field_src[f]` within that group's
/// element, and is copied (`field_size` bytes) to byte offset `field_dst[f]` within
/// each AoS element. The returned buffer is freshly `malloc`'d (via
/// [`crate::alloc::karac_alloc_or_panic`]) so the owned `Vec[S]` frees it with the
/// matching `free`; the GPU group outputs are internal `Vec`s dropped here. Empty
/// (`n == 0` / `n_groups == 0`) returns a unique non-null allocation.
///
/// # Safety
///
/// `wgsl_ptr`/`wgsl_len` a valid UTF-8 shader; `in_ptrs`/`group_strides` arrays of
/// `n_groups` (each `in_ptrs[k]` to `n * group_strides[k]` valid bytes);
/// `field_group`/`field_src`/`field_dst` arrays of `n_fields`. The returned pointer
/// transfers ownership. Aborts on no available GPU adapter (no CPU fallback).
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn karac_runtime_gpu_dispatch_soa(
    wgsl_ptr: *const u8,
    wgsl_len: usize,
    n_groups: usize,
    in_ptrs: *const *const u8,
    group_strides: *const usize,
    n_fields: usize,
    field_group: *const usize,
    field_src: *const usize,
    field_dst: *const usize,
    field_size: usize,
    aos_stride: usize,
    n: usize,
    n_uniforms: usize,
    uniform_ptrs: *const *const u8,
    uniform_size: usize,
) -> *mut u8 {
    unsafe {
        let aos_total = n.saturating_mul(aos_stride);

        // Empty dispatch: a unique non-null allocation the caller never reads.
        if aos_total == 0 || n_groups == 0 {
            return crate::alloc::karac_alloc_or_panic(aos_total.max(1));
        }

        let wgsl_bytes = std::slice::from_raw_parts(wgsl_ptr, wgsl_len);
        let Ok(wgsl) = std::str::from_utf8(wgsl_bytes) else {
            crate::fatal::write_stderr(b"panic: gpu.dispatch shader is not valid UTF-8\n");
            std::process::abort();
        };
        let strides = std::slice::from_raw_parts(group_strides, n_groups);
        let in_ptr_slice = std::slice::from_raw_parts(in_ptrs, n_groups);
        let inputs: Vec<&[u8]> = in_ptr_slice
            .iter()
            .zip(strides.iter())
            .map(|(&p, &stride)| std::slice::from_raw_parts(p, n * stride))
            .collect();
        // Scalar uniforms (GPU-LBM-2): each `uniform_size` bytes (f32 = 4). Guard the
        // empty case — codegen passes a null `uniform_ptrs` for a zero-uniform kernel,
        // and `from_raw_parts(null, 0)` violates the aligned-non-null precondition.
        let uniforms: Vec<&[u8]> = if n_uniforms == 0 {
            Vec::new()
        } else {
            std::slice::from_raw_parts(uniform_ptrs, n_uniforms)
                .iter()
                .map(|&p| std::slice::from_raw_parts(p, uniform_size))
                .collect()
        };

        let Some(outputs) =
            pollster::block_on(dispatch_multi_bytes_async(wgsl, &inputs, &uniforms, n))
        else {
            crate::fatal::write_stderr(
                b"panic: gpu.dispatch found no available GPU adapter (no CPU fallback)\n",
            );
            std::process::abort();
        };
        debug_assert_eq!(outputs.len(), n_groups, "one output group-array per group");

        // Scatter each struct field from its group's output element to the AoS element.
        let fgroup = std::slice::from_raw_parts(field_group, n_fields);
        let fsrc = std::slice::from_raw_parts(field_src, n_fields);
        let fdst = std::slice::from_raw_parts(field_dst, n_fields);
        let out = crate::alloc::karac_alloc_or_panic(aos_total);
        for f in 0..n_fields {
            let g = fgroup[f];
            let src_buf = &outputs[g];
            let gstride = strides[g];
            for i in 0..n {
                std::ptr::copy_nonoverlapping(
                    src_buf.as_ptr().add(i * gstride + fsrc[f]),
                    out.add(i * aos_stride + fdst[f]),
                    field_size,
                );
            }
        }
        out
    }
}

/// Byte-oriented GPU element-wise map core. `input` is the raw element bytes
/// (`n * elem_size`); the returned buffer is the same length. The WGSL shader
/// supplies the element interpretation via its `array<T>` binding declarations,
/// so this stays type-agnostic. `elem_size` sets the per-element stride used to
/// derive the invocation count.
async fn dispatch_bytes_async(wgsl: &str, input: &[u8], elem_size: usize) -> Option<Vec<u8>> {
    // The single-buffer path is the `n_buffers == 1` case of the multi core:
    // input at `@binding(0)`, output at `@binding(1)` — byte-identical to the
    // slice-0 WGSL contract.
    let mut outs = dispatch_multi_bytes_async(wgsl, &[input], &[], input.len() / elem_size).await?;
    outs.pop()
}

/// Byte-oriented GPU map core over `n = inputs.len()` coalesced buffers — the
/// CG-4 generalization of the slice-0 single-buffer spine. Each `inputs[k]` is
/// one layout group's contiguous field-array (raw bytes, `n_elems * elem_size`);
/// all groups share the same element count. Binds input buffers at
/// `@binding(0..n)` and output buffers at `@binding(n..2n)`; returns one output
/// byte-buffer per group (same length as its input). The WGSL supplies the
/// element interpretation via its `array<T>` declarations, so this stays
/// type-agnostic.
async fn dispatch_multi_bytes_async(
    wgsl: &str,
    inputs: &[&[u8]],
    uniforms: &[&[u8]],
    elem_count: usize,
) -> Option<Vec<Vec<u8>>> {
    let n_buffers = inputs.len();
    if n_buffers == 0 {
        return Some(Vec::new());
    }
    // Each group's output/staging buffer matches its input's byte length — groups
    // can have different per-element strides (a multi-field group is wider).
    // `elem_count` (one logical row per GPU thread) is passed explicitly.

    // Reuse the process-wide device/queue (GPU-SLIP-4a) instead of requesting a
    // fresh adapter+device every dispatch.
    let ctx = gpu_context()?;
    let device = &ctx.device;
    let queue = &ctx.queue;

    let sizes: Vec<u64> = inputs.iter().map(|b| b.len() as u64).collect();
    let input_bufs: Vec<wgpu::Buffer> = inputs
        .iter()
        .map(|bytes| {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("gpu-cg4-input"),
                contents: bytes,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            })
        })
        .collect();
    // Run the pass into fresh device output buffers, then read them back — the
    // round-trip path. GPU-SLIP-4b factors both halves (`run_compute` +
    // `readback`) so the resident path can reuse the exact same dispatch core
    // without the host transfer.
    let output_bufs = run_compute(
        device,
        queue,
        wgsl,
        &input_bufs,
        &sizes,
        uniforms,
        elem_count,
    );
    readback(device, queue, &output_bufs, &sizes)
}

/// One level-0 `gpu.dot` dispatch: two input buffers in, ONE partials buffer
/// out (B-2026-08-19-13).
///
/// Distinct from [`dispatch_multi_bytes_async`] only in the output count —
/// that path allocates one output per input, which is right for an
/// element-wise map and wrong for a reduction. Here the shader declares
/// `a` at `@binding(0)`, `b` at `@binding(1)` and `output` at `@binding(2)`,
/// so exactly one output buffer is created and the bind group matches the
/// layout wgpu derives from the shader.
///
/// The output is allocated at the INPUT's byte length, like the sum path, and
/// the caller keeps only the `ceil(n / WORKGROUP)` slots the workgroups
/// actually wrote.
async fn dispatch_dot_bytes_async(
    wgsl: &str,
    a: &[u8],
    b: &[u8],
    elem_size: usize,
) -> Option<Vec<u8>> {
    let ctx = gpu_context()?;
    let device = &ctx.device;
    let queue = &ctx.queue;

    let input_bufs: Vec<wgpu::Buffer> = [a, b]
        .iter()
        .map(|bytes| {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("gpu-dot-input"),
                contents: bytes,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            })
        })
        .collect();
    let out_sizes = [a.len() as u64];
    let output_bufs = run_compute(
        device,
        queue,
        wgsl,
        &input_bufs,
        &out_sizes,
        &[],
        a.len() / elem_size,
    );
    let mut outs = readback(device, queue, &output_bufs, &out_sizes)?;
    outs.pop()
}

/// One CHECKED integer reduction dispatch: one input buffer in, a partials
/// buffer AND a per-workgroup overflow-flag buffer out (B-2026-08-19-13).
///
/// Two outputs from one input — the shape `run_compute`'s split input/output
/// sizing exists for. The flags are `u32` regardless of the element type, so
/// their buffer is sized independently of the values'.
async fn dispatch_checked_int_async(
    wgsl: &str,
    input: &[u8],
    elem_size: usize,
) -> Option<(Vec<u8>, Vec<u8>)> {
    let ctx = gpu_context()?;
    let device = &ctx.device;
    let queue = &ctx.queue;

    let n = input.len() / elem_size;
    let input_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("gpu-int-reduce-input"),
        contents: input,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
    });
    // Values keep the input's byte length (the caller slices off the partials
    // it actually wrote); flags are one u32 per workgroup.
    let out_sizes = [
        input.len() as u64,
        (n.div_ceil(64).max(1) * std::mem::size_of::<u32>()) as u64,
    ];
    let output_bufs = run_compute(device, queue, wgsl, &[input_buf], &out_sizes, &[], n);
    let mut outs = readback(device, queue, &output_bufs, &out_sizes)?;
    let flags = outs.pop()?;
    let values = outs.pop()?;
    Some((values, flags))
}

/// C entry point for the CHECKED integer reductions — `gpu.sum` / `gpu.min` /
/// `gpu.max` over `Vec[i32]` / `Vec[u32]` (B-2026-08-19-13).
///
/// **Integer reductions trap on overflow**, matching `v.sum()` over a
/// `Vec[i32]`, which already fails with `integer overflow` on both surfaces.
/// WGSL has no trapping arithmetic — its integer ops are defined to wrap — so
/// the shader computes the overflow bit itself and OR-folds it through the
/// same halving tree as the value. This entry point ORs the per-workgroup bits
/// at every level and aborts the moment any is set.
///
/// Note what "the moment any is set" means: the check happens at the END of
/// each dispatch, so a buffer that overflows at some stride does not reach the
/// next LEVEL, but the rest of its own level still runs. The CPU twin
/// (`reduce_kernel::tree_reduce_i32`) fails at the first overflowing combine
/// instead. Both refuse the same programs — which is what matters — they just
/// stop at slightly different points inside a dispatch nobody can observe.
///
/// **Overflow is REPORTED, not aborted.** The return value is `0` on success
/// and `1` on overflow, and the value goes through `out`. That split exists so
/// codegen can raise Kāra's OWN panic at the call site — same `integer
/// overflow` message, same exit code, same source span as `v.sum()` over a
/// `Vec[i32]`. Aborting from in here would produce a bare `SIGABRT` with no
/// span, which is a worse diagnostic than the CPU path gives for the identical
/// condition. (The adapter-missing and UTF-8 paths still abort: those are
/// environment failures with no Kāra-level meaning.)
///
/// # Safety
///
/// `wgsl_ptr`/`wgsl_len` a valid UTF-8 shader; `in_ptr` points to `n` valid
/// 4-byte elements; `out` is a writable 4-byte slot. Aborts on no available
/// GPU adapter (no CPU fallback), same as every other entry point here.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_gpu_reduce_i32(
    wgsl_ptr: *const u8,
    wgsl_len: usize,
    in_ptr: *const i32,
    n: usize,
    identity: i32,
    out: *mut i32,
) -> i32 {
    unsafe {
        // Empty reduction is the operation's identity and needs no device —
        // and cannot overflow, so there is nothing to check.
        if n == 0 {
            *out = identity;
            return 0;
        }
        const WORKGROUP: usize = 64;

        let Ok(wgsl) = std::str::from_utf8(std::slice::from_raw_parts(wgsl_ptr, wgsl_len)) else {
            crate::fatal::write_stderr(b"panic: gpu integer reduction shader is not valid UTF-8\n");
            std::process::abort();
        };

        let elem_size = std::mem::size_of::<i32>();
        let mut level: Vec<u8> =
            std::slice::from_raw_parts(in_ptr as *const u8, n * elem_size).to_vec();
        let mut count = n;

        loop {
            let Some((values, flags)) =
                pollster::block_on(dispatch_checked_int_async(wgsl, &level, elem_size))
            else {
                crate::fatal::write_stderr(
                    b"panic: gpu integer reduction found no available GPU adapter \
                      (no CPU fallback)\n",
                );
                std::process::abort();
            };
            let partials = count.div_ceil(WORKGROUP);
            // Any workgroup that overflowed poisons the whole reduction. Kāra
            // traps on integer overflow; wrapping here would hand back a
            // plausible wrong number, which is the one outcome this family
            // must never produce.
            if flags[..partials * std::mem::size_of::<u32>()]
                .chunks_exact(4)
                .any(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]]) != 0)
            {
                return 1;
            }
            level = values[..partials * elem_size].to_vec();
            count = partials;
            if count <= 1 {
                break;
            }
        }

        *out = i32::from_le_bytes([level[0], level[1], level[2], level[3]]);
        0
    }
}

/// One `argmin`/`argmax` dispatch. `cand` is `None` at level 0 (every element
/// is its own candidate) and the surviving indices thereafter
/// (B-2026-08-19-13).
///
/// Always ONE output — a `u32` index per workgroup — regardless of the input
/// count, which is what `run_compute`'s split input/output sizing exists for.
/// The output is sized to the CANDIDATE count, not the buffer's, because that
/// is what shrinks each level.
async fn dispatch_arg_async(
    wgsl: &str,
    input: &[u8],
    cand: Option<&[u8]>,
    n_candidates: usize,
) -> Option<Vec<u8>> {
    let ctx = gpu_context()?;
    let device = &ctx.device;
    let queue = &ctx.queue;

    let mut inputs: Vec<wgpu::Buffer> = Vec::with_capacity(2);
    for bytes in std::iter::once(input).chain(cand) {
        inputs.push(
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("gpu-arg-input"),
                contents: bytes,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            }),
        );
    }
    let out_sizes = [(n_candidates.max(1) * std::mem::size_of::<u32>()) as u64];
    let output_bufs = run_compute(device, queue, wgsl, &inputs, &out_sizes, &[], n_candidates);
    let mut outs = readback(device, queue, &output_bufs, &out_sizes)?;
    outs.pop()
}

/// One level-0 deviation dispatch: one input buffer plus the MEAN as a
/// uniform, one partials buffer out (B-2026-08-19-13).
///
/// The uniform is what makes variance two passes rather than one. Bound after
/// the in/out buffers, following `run_compute`'s convention, so a 1-in/1-out
/// kernel finds it at `@binding(2)`.
async fn dispatch_deviation_async(
    wgsl: &str,
    input: &[u8],
    mean: f32,
    elem_size: usize,
) -> Option<Vec<u8>> {
    let ctx = gpu_context()?;
    let device = &ctx.device;
    let queue = &ctx.queue;

    let input_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("gpu-dev-input"),
        contents: input,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
    });
    let mean_bytes = mean.to_le_bytes();
    let out_sizes = [input.len() as u64];
    let output_bufs = run_compute(
        device,
        queue,
        wgsl,
        &[input_buf],
        &out_sizes,
        &[&mean_bytes],
        input.len() / elem_size,
    );
    let mut outs = readback(device, queue, &output_bufs, &out_sizes)?;
    outs.pop()
}

/// C entry point for `gpu.variance(buffer)` / `gpu.stddev(buffer)` — returns
/// the SUM OF SQUARED DEVIATIONS (B-2026-08-19-13).
///
/// **The first genuinely two-pass reduction.** Every other one here is a
/// single converging fold; this one cannot be, because the mean has to exist
/// before a single deviation can be formed. So it runs a complete sum
/// reduction, divides to get the mean, and dispatches a second pass that
/// squares each deviation on load and folds again.
///
/// Returns the sum of squares rather than the variance because the final
/// divisor is the caller's choice — `n` for the population form, `n - 1` for
/// the sample one — and `stddev` needs one more operation on top. Keeping
/// both in codegen means the CPU twin mirrors them in one obvious place, and
/// means ONE entry point serves `variance` and `stddev` both.
///
/// Takes two shaders: the deviation kernel for level 0 of the second pass, and
/// the ordinary SUM kernel, used BOTH for the whole first pass and to fold the
/// second pass's partials. Reusing it is what makes the answer identical to
/// summing the squared deviations by hand.
///
/// # Safety
///
/// Both `*_wgsl_ptr`/`_len` pairs a valid UTF-8 shader; `in_ptr` points to `n`
/// valid `f32` values. Aborts on no available GPU adapter (no CPU fallback),
/// same as every other entry point here.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_gpu_sumsq_dev_f32(
    dev_wgsl_ptr: *const u8,
    dev_wgsl_len: usize,
    sum_wgsl_ptr: *const u8,
    sum_wgsl_len: usize,
    in_ptr: *const f32,
    n: usize,
) -> f32 {
    unsafe {
        // No elements, no deviations. The caller turns this into `None`; it
        // never divides by the zero that would otherwise follow.
        if n == 0 {
            return 0.0;
        }
        const WORKGROUP: usize = 64;

        let Ok(dev_wgsl) =
            std::str::from_utf8(std::slice::from_raw_parts(dev_wgsl_ptr, dev_wgsl_len))
        else {
            crate::fatal::write_stderr(
                b"panic: gpu.variance deviation shader is not valid UTF-8\n",
            );
            std::process::abort();
        };
        let Ok(sum_wgsl) =
            std::str::from_utf8(std::slice::from_raw_parts(sum_wgsl_ptr, sum_wgsl_len))
        else {
            crate::fatal::write_stderr(b"panic: gpu.variance fold shader is not valid UTF-8\n");
            std::process::abort();
        };

        let elem_size = std::mem::size_of::<f32>();
        let input = std::slice::from_raw_parts(in_ptr as *const u8, n * elem_size);

        // PASS 1: the whole sum reduction, then the mean. Identical to
        // `gpu.mean`, deliberately — the two must agree about what the mean of
        // this buffer is, or the deviations are measured from the wrong place.
        let sum = karac_runtime_gpu_reduce_f32(sum_wgsl_ptr, sum_wgsl_len, in_ptr, n, 0.0);
        let mean = sum / n as f32;

        // PASS 2, level 0: square each deviation on load and reduce per
        // workgroup.
        let Some(output) =
            pollster::block_on(dispatch_deviation_async(dev_wgsl, input, mean, elem_size))
        else {
            crate::fatal::write_stderr(
                b"panic: gpu.variance found no available GPU adapter (no CPU fallback)\n",
            );
            std::process::abort();
        };
        let mut count = n.div_ceil(WORKGROUP);
        let mut level = output[..count * elem_size].to_vec();

        // PASS 2, levels 1+: the ordinary sum fold over the partials.
        while count > 1 {
            let Some(next) = pollster::block_on(dispatch_bytes_async(sum_wgsl, &level, elem_size))
            else {
                crate::fatal::write_stderr(
                    b"panic: gpu.variance found no available GPU adapter (no CPU fallback)\n",
                );
                std::process::abort();
            };
            count = count.div_ceil(WORKGROUP);
            level = next[..count * elem_size].to_vec();
        }

        f32::from_le_bytes([level[0], level[1], level[2], level[3]])
    }
}

/// C entry point for `gpu.argmin(buffer)` / `gpu.argmax(buffer)` — the index
/// of the extremum (B-2026-08-19-13).
///
/// **Element-type agnostic, and named for it.** The input is opaque 4-byte
/// words: this function never interprets them, the SHADER does (declaring
/// `array<f32>` / `array<i32>` / `array<u32>` is what fixes the comparison,
/// including whether `<` is signed). And the result is an INDEX, so unlike the
/// value reductions there is no widening or signedness decision on the way out
/// either. One entry point genuinely serves all three element types — which is
/// why it is not called `_f32`.
///
/// Takes TWO shaders, like `gpu.dot`, but for a different reason. Level 0
/// seeds every element as its own candidate; every level after that receives
/// the surviving candidate INDICES and re-reads their values from the original
/// buffer, which stays bound throughout. So indices are absolute at every
/// level and no value is ever carried between dispatches — the host never
/// ships values back and forth, and nothing can be lost in transit.
///
/// Returns the index, or `u32::MAX` for an empty buffer. The caller turns that
/// into `None`: an empty buffer has no extremum, and `Stats.argmin` says the
/// same.
///
/// # Safety
///
/// Both `*_wgsl_ptr`/`_len` pairs a valid UTF-8 shader; `in_ptr` points to `n`
/// valid 4-byte elements. Aborts on no available GPU adapter (no CPU
/// fallback), same as every other entry point here.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_gpu_arg_index(
    seed_wgsl_ptr: *const u8,
    seed_wgsl_len: usize,
    fold_wgsl_ptr: *const u8,
    fold_wgsl_len: usize,
    in_ptr: *const u32,
    n: usize,
) -> u32 {
    unsafe {
        // No extremum, and no device needed to know it.
        if n == 0 {
            return u32::MAX;
        }
        const WORKGROUP: usize = 64;

        let Ok(seed_wgsl) =
            std::str::from_utf8(std::slice::from_raw_parts(seed_wgsl_ptr, seed_wgsl_len))
        else {
            crate::fatal::write_stderr(b"panic: gpu arg-reduction shader is not valid UTF-8\n");
            std::process::abort();
        };
        let Ok(fold_wgsl) =
            std::str::from_utf8(std::slice::from_raw_parts(fold_wgsl_ptr, fold_wgsl_len))
        else {
            crate::fatal::write_stderr(
                b"panic: gpu arg-reduction fold shader is not valid UTF-8\n",
            );
            std::process::abort();
        };

        // Bytes in, bytes to the device. The element type lives in the shader.
        let input = std::slice::from_raw_parts(in_ptr as *const u8, n * std::mem::size_of::<u32>());

        // LEVEL 0: every element is its own candidate.
        let Some(mut level) = pollster::block_on(dispatch_arg_async(seed_wgsl, input, None, n))
        else {
            crate::fatal::write_stderr(
                b"panic: gpu arg-reduction found no available GPU adapter (no CPU fallback)\n",
            );
            std::process::abort();
        };
        let mut count = n.div_ceil(WORKGROUP);
        level.truncate(count * std::mem::size_of::<u32>());

        // LEVELS 1+: fold the surviving candidates, re-reading their values
        // from the SAME input buffer.
        while count > 1 {
            let Some(next) =
                pollster::block_on(dispatch_arg_async(fold_wgsl, input, Some(&level), count))
            else {
                crate::fatal::write_stderr(
                    b"panic: gpu arg-reduction found no available GPU adapter (no CPU fallback)\n",
                );
                std::process::abort();
            };
            count = count.div_ceil(WORKGROUP);
            level = next;
            level.truncate(count * std::mem::size_of::<u32>());
        }

        u32::from_le_bytes([level[0], level[1], level[2], level[3]])
    }
}

/// C entry point for `gpu.dot(a, b)` — the fused multiply-then-sum reduction
/// (B-2026-08-19-13).
///
/// Takes TWO shaders. Level 0 is the dot kernel: it multiplies the buffers
/// element-wise on load and reduces each workgroup's chunk, leaving one
/// partial per workgroup. Every level after that is a plain SUM over those
/// partials, which is the ordinary reduction shader — a dot product is a map
/// fused into the first level of a sum, and only the first level has two
/// buffers to read.
///
/// Passing the sum shader in rather than re-deriving it here is what makes
/// `gpu.dot(a, b)` and `gpu.sum(a * b)` agree bit-for-bit: after level 0 they
/// are literally the same computation on the same values, in the same tree
/// order. The alternative — a second dot-shaped fold over the partials, with
/// `b` implicitly all-ones — would be a second code path that could drift.
///
/// The fused form is also the reason `dot` is its own entry point rather than
/// sugar: no `n`-element product buffer is ever written, so device traffic is
/// halved against a map-then-reduce.
///
/// **Mismatched lengths trap.** A dot product over buffers of different
/// lengths is not defined, and the two plausible salvages are both wrong for
/// Kāra: truncating to the shorter (Rust's `zip`) silently answers a question
/// nobody asked, and zero-extending invents data. Kāra checks rather than
/// wraps on integer overflow for the same reason, so this aborts with both
/// lengths named. The check lives HERE, at the one place that knows both, so
/// no `b[i]` read can run past its buffer.
///
/// # Safety
///
/// Both `*_wgsl_ptr`/`_len` pairs a valid UTF-8 shader; `a_ptr` points to
/// `n_a` and `b_ptr` to `n_b` valid `f32` values. Aborts on no available GPU
/// adapter (no CPU fallback), same as every other entry point here.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_gpu_dot_f32(
    dot_wgsl_ptr: *const u8,
    dot_wgsl_len: usize,
    sum_wgsl_ptr: *const u8,
    sum_wgsl_len: usize,
    a_ptr: *const f32,
    n_a: usize,
    b_ptr: *const f32,
    n_b: usize,
) -> f32 {
    unsafe {
        if n_a != n_b {
            crate::fatal::write_stderr(b"panic: gpu.dot requires buffers of equal length (");
            crate::fatal::write_stderr(n_a.to_string().as_bytes());
            crate::fatal::write_stderr(b" vs ");
            crate::fatal::write_stderr(n_b.to_string().as_bytes());
            crate::fatal::write_stderr(b")\n");
            std::process::abort();
        }
        let n = n_a;
        // The empty dot product is 0 — the additive identity, and no device is
        // needed to know it.
        if n == 0 {
            return 0.0;
        }
        // Must match the shaders' `@workgroup_size` and
        // `reduce_kernel::GPU_REDUCE_WIDTH`.
        const WORKGROUP: usize = 64;

        let Ok(dot_wgsl) =
            std::str::from_utf8(std::slice::from_raw_parts(dot_wgsl_ptr, dot_wgsl_len))
        else {
            crate::fatal::write_stderr(b"panic: gpu.dot shader is not valid UTF-8\n");
            std::process::abort();
        };
        let Ok(sum_wgsl) =
            std::str::from_utf8(std::slice::from_raw_parts(sum_wgsl_ptr, sum_wgsl_len))
        else {
            crate::fatal::write_stderr(b"panic: gpu.dot fold shader is not valid UTF-8\n");
            std::process::abort();
        };

        let elem_size = std::mem::size_of::<f32>();
        let byte_len = n * elem_size;
        let a = std::slice::from_raw_parts(a_ptr as *const u8, byte_len);
        let b = std::slice::from_raw_parts(b_ptr as *const u8, byte_len);

        // LEVEL 0: fused multiply + per-workgroup reduce.
        let Some(output) = pollster::block_on(dispatch_dot_bytes_async(dot_wgsl, a, b, elem_size))
        else {
            crate::fatal::write_stderr(
                b"panic: gpu.dot found no available GPU adapter (no CPU fallback)\n",
            );
            std::process::abort();
        };
        let mut count = n.div_ceil(WORKGROUP);
        let mut level = output[..count * elem_size].to_vec();

        // LEVELS 1+: the ordinary sum fold, identical to `gpu.sum`'s.
        while count > 1 {
            let Some(output) =
                pollster::block_on(dispatch_bytes_async(sum_wgsl, &level, elem_size))
            else {
                crate::fatal::write_stderr(
                    b"panic: gpu.dot found no available GPU adapter (no CPU fallback)\n",
                );
                std::process::abort();
            };
            let partials = count.div_ceil(WORKGROUP);
            level = output[..partials * elem_size].to_vec();
            count = partials;
        }

        f32::from_le_bytes([level[0], level[1], level[2], level[3]])
    }
}

/// Bind a kernel over N group input buffers already resident on the device and
/// dispatch it, returning N fresh output buffers left **resident** on the GPU
/// (`STORAGE | COPY_SRC | COPY_DST` — ready to be the next dispatch's input or to
/// be read back). Shared by the round-trip [`dispatch_multi_bytes_async`] and the
/// resident [`karac_runtime_gpu_dispatch_resident`] path (GPU-SLIP-4b): the only
/// difference between them is whether the outputs are read back to the host or
/// kept on the device. `sizes[k]` is group `k`'s byte length — input and output
/// share it, since an element-wise SoA / stencil kernel preserves each group's
/// layout. `uniforms` are the raw scalar-uniform bytes (one storage buffer each,
/// bound after the in/out buffers). Only submits — never waits; a following
/// `readback` or next dispatch orders after it on the same queue.
///
/// `out_sizes` is the OUTPUT buffers' byte lengths, and is deliberately
/// separate from the inputs rather than derived from them. An element-wise SoA
/// or stencil kernel has one output per input and passes the same slice; a
/// REDUCTION does not — `gpu.dot` reads two buffers and writes one, so its
/// output lands at `@binding(2)` and no fourth buffer is allocated for a
/// binding the shader never declares (which wgpu would reject anyway, since
/// the bind-group layout is derived from the shader).
fn run_compute(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    wgsl: &str,
    input_bufs: &[wgpu::Buffer],
    out_sizes: &[u64],
    uniforms: &[&[u8]],
    elem_count: usize,
) -> Vec<wgpu::Buffer> {
    let n_buffers = input_bufs.len();
    // GPU-SLIP-4 buffer pooling: reuse a freed grid's output buffers (from the
    // pool) rather than allocating fresh ones each dispatch — the per-substep
    // output allocation was the dominant per-dispatch CPU cost once the transfer
    // was gone (4c re-bench). A miss creates a new buffer.
    let output_bufs: Vec<wgpu::Buffer> = out_sizes
        .iter()
        .map(|&sz| alloc_output_buffer(device, sz))
        .collect();
    // Read-only scalar uniforms (GPU-LBM-2): one storage buffer each, bound after
    // the group in/out buffers. Storage (not `uniform`) avoids the 16-byte
    // uniform-alignment constraint; the shader reads `<name>_u[0]`.
    let uniform_bufs: Vec<wgpu::Buffer> = uniforms
        .iter()
        .map(|bytes| {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("gpu-cg4-uniform"),
                contents: bytes,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            })
        })
        .collect();

    // Cached compiled pipeline (GPU-SLIP-4a) — compiled once per distinct shader.
    let pipeline = compute_pipeline(device, wgsl);
    let bind_group_layout = pipeline.get_bind_group_layout(0);
    // Inputs at binding 0..n_in, outputs at n_in..n_in+n_out, uniforms after
    // both. Counted from the OUTPUT buffers rather than assuming one per
    // input — a reduction has fewer (`gpu.dot` is 2 in, 1 out), and hardcoding
    // `2 * n_buffers` here would silently misplace its uniforms.
    let n_out = output_bufs.len();
    let mut entries: Vec<wgpu::BindGroupEntry> =
        Vec::with_capacity(n_buffers + n_out + uniform_bufs.len());
    for (i, buf) in input_bufs.iter().enumerate() {
        entries.push(wgpu::BindGroupEntry {
            binding: i as u32,
            resource: buf.as_entire_binding(),
        });
    }
    for (i, buf) in output_bufs.iter().enumerate() {
        entries.push(wgpu::BindGroupEntry {
            binding: (n_buffers + i) as u32,
            resource: buf.as_entire_binding(),
        });
    }
    for (i, buf) in uniform_bufs.iter().enumerate() {
        entries.push(wgpu::BindGroupEntry {
            binding: (n_buffers + n_out + i) as u32,
            resource: buf.as_entire_binding(),
        });
    }
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("gpu-cg4-bind-group"),
        layout: &bind_group_layout,
        entries: &entries,
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("gpu-cg4-encoder"),
    });
    {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("gpu-cg4-pass"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        // One invocation per element; @workgroup_size(64) in the shader.
        // wgpu caps each dispatch dimension at 65535 workgroups, so any
        // grid past 65535 × 64 = 4,194,240 elements spreads across a 2D
        // dispatch: X FIXED at 65535 whenever a second row exists, so the
        // kernels recover the flat index as `gid.y * (65535 * 64) + gid.x`
        // with a fold-time constant (src/gpu_wgsl.rs::DISPATCH_X_SPAN — the
        // two sites must agree). y == 1 degenerates to the old 1D form;
        // last-row overshoot threads exit on the `>= arrayLength` guard.
        let wg = (elem_count as u64).div_ceil(64);
        let x = wg.min(65535) as u32;
        let y = wg.div_ceil(65535);
        if y > 65535 {
            // 65535² workgroups × 64 ≈ 2.7e14 elements — unreachable for
            // any real buffer, but fail loud rather than truncate.
            crate::fatal::write_stderr(b"panic: gpu.dispatch grid exceeds the 2D dispatch limit\n");
            std::process::abort();
        }
        pass.dispatch_workgroups(x, y as u32, 1);
    }
    queue.submit(Some(encoder.finish()));
    output_bufs
}

/// Copy N resident device buffers back to host memory — one `MAP_READ` staging
/// buffer per group, a single submit + poll drains every readback. Returns one
/// byte-vector per group (same order + size as `bufs`); `None` on a map failure.
/// This is the host-transfer half GPU-SLIP-4b keeps OUT of the resident dispatch
/// loop (it runs only at `gpu.download`, not per substep).
fn readback(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    bufs: &[wgpu::Buffer],
    sizes: &[u64],
) -> Option<Vec<Vec<u8>>> {
    let staging_bufs: Vec<wgpu::Buffer> = sizes
        .iter()
        .map(|&sz| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("gpu-cg4-staging"),
                size: sz,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        })
        .collect();
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("gpu-cg4-readback-encoder"),
    });
    for ((buf, staging), &sz) in bufs.iter().zip(staging_bufs.iter()).zip(sizes.iter()) {
        encoder.copy_buffer_to_buffer(buf, 0, staging, 0, sz);
    }
    queue.submit(Some(encoder.finish()));

    // Kick off every staging map, then a single poll drains all callbacks.
    let receivers: Vec<_> = staging_bufs
        .iter()
        .map(|staging| {
            let (tx, rx) = std::sync::mpsc::channel();
            staging
                .slice(..)
                .map_async(wgpu::MapMode::Read, move |res| {
                    let _ = tx.send(res);
                });
            rx
        })
        .collect();
    device.poll(wgpu::PollType::wait_indefinitely()).ok()?;

    let mut outs = Vec::with_capacity(bufs.len());
    for (staging, rx) in staging_bufs.iter().zip(receivers) {
        rx.recv().ok()?.ok()?;
        let slice = staging.slice(..);
        let mapped = slice.get_mapped_range();
        outs.push(mapped.to_vec());
        drop(mapped);
        staging.unmap();
    }
    Some(outs)
}

// ── GPU-SLIP-4b: persistent on-device (resident) SoA buffers ─────────────────
//
// The round-trip `karac_runtime_gpu_dispatch_soa` uploads the grid, dispatches,
// and downloads on EVERY call — for an iterative LBM sim that host↔device
// transfer dominates (the 218 ms baseline is ~all transfer, not compute). The
// resident path keeps the grid on the GPU across substeps: `upload` moves it to
// the device once, `dispatch_resident` runs device→device with no round-trip,
// and `download` brings it back once at the end. A `gpu.Buffer[S]` value on the
// Kāra side carries the opaque handle; its ownership drop frees the device
// buffers (`free_soa`). This slice (4b-1) is the runtime substrate; the codegen
// + language surface that emits these calls is 4b-2.

/// A group-SoA buffer set resident on the GPU across dispatches. One
/// `wgpu::Buffer` per layout group plus its byte length (`sizes[k] == n *
/// group_stride[k]`); `n` is the element count (one GPU thread per element).
/// Dropping this (removing it from the registry) frees the device memory.
struct ResidentSoa {
    bufs: Vec<wgpu::Buffer>,
    sizes: Vec<u64>,
    n: usize,
}

/// Registry of live resident-buffer handles. An opaque `u64` handle (never 0)
/// keys each `ResidentSoa`; the Kāra `gpu.Buffer[S]` value carries the handle,
/// and its ownership drop calls [`karac_runtime_gpu_free_soa`].
fn resident_registry() -> &'static Mutex<HashMap<u64, ResidentSoa>> {
    static REG: OnceLock<Mutex<HashMap<u64, ResidentSoa>>> = OnceLock::new();
    REG.get_or_init(|| Mutex::new(HashMap::new()))
}

/// A fresh, never-reused, never-zero resident handle.
fn next_resident_handle() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// Process-wide pool of reusable device output buffers, keyed by byte size
/// (GPU-SLIP-4 buffer pooling). A resident sim loop `grid = gpu.dispatch(step,
/// grid)` frees the displaced grid every substep and allocates a fresh output —
/// and the 4c re-bench measured that per-dispatch allocation as the dominant CPU
/// cost once the host↔device transfer was gone. Recycling the freed buffers as
/// the next dispatch's output removes it. **Safe with the per-dispatch submit
/// model:** a recycled buffer's prior use is queued before its reuse-as-output,
/// and the queue executes submissions in order, so the earlier read completes
/// before the later write (no in-flight aliasing). Buffers are all
/// `STORAGE|COPY_SRC|COPY_DST` and the kernel overwrites every element, so a
/// pooled buffer needs no clear.
fn buffer_pool() -> &'static Mutex<HashMap<u64, Vec<wgpu::Buffer>>> {
    static POOL: OnceLock<Mutex<HashMap<u64, Vec<wgpu::Buffer>>>> = OnceLock::new();
    POOL.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Take a `size`-byte STORAGE output buffer from the pool, or create one on a miss.
fn alloc_output_buffer(device: &wgpu::Device, size: u64) -> wgpu::Buffer {
    if let Some(buf) = buffer_pool()
        .lock()
        .unwrap()
        .get_mut(&size)
        .and_then(|v| v.pop())
    {
        return buf;
    }
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("gpu-cg4-output"),
        size,
        usage: wgpu::BufferUsages::STORAGE
            | wgpu::BufferUsages::COPY_SRC
            | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

/// Return device buffers to the pool for reuse (keyed by byte size). Called when
/// a resident handle is freed (the loop's displaced grid) or downloaded.
fn recycle_buffers(bufs: Vec<wgpu::Buffer>, sizes: &[u64]) {
    if bufs.is_empty() {
        return;
    }
    let mut pool = buffer_pool().lock().unwrap();
    for (buf, &sz) in bufs.into_iter().zip(sizes.iter()) {
        pool.entry(sz).or_default().push(buf);
    }
}

/// Upload N group-arrays to the GPU as one resident SoA buffer set (GPU-SLIP-4b).
/// `in_ptrs[k]` points to `n * group_strides[k]` host bytes; each becomes a
/// STORAGE device buffer that stays resident until the handle is downloaded or
/// freed. Returns an opaque handle (never 0). The host source is NOT freed — the
/// `gpu.upload` codegen has moved the `Vec[S]` and its owner frees it.
///
/// # Safety
///
/// `in_ptrs` an array of `n_groups` pointers, each to `n * group_strides[k]`
/// valid bytes for the duration of the call; `group_strides` an array of
/// `n_groups`. Aborts on no available GPU adapter (no CPU fallback).
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_gpu_upload_soa(
    n_groups: usize,
    in_ptrs: *const *const u8,
    group_strides: *const usize,
    n: usize,
) -> u64 {
    unsafe {
        let handle = next_resident_handle();

        // Empty / degenerate: register a bufferless handle (download returns a unique
        // non-null allocation, dispatch yields another empty handle) — mirrors the
        // round-trip path's `n == 0` contract without a zero-size wgpu buffer.
        if n == 0 || n_groups == 0 {
            resident_registry().lock().unwrap().insert(
                handle,
                ResidentSoa {
                    bufs: Vec::new(),
                    sizes: Vec::new(),
                    n: 0,
                },
            );
            return handle;
        }

        let Some(ctx) = gpu_context() else {
            crate::fatal::write_stderr(
                b"panic: gpu.upload found no available GPU adapter (no CPU fallback)\n",
            );
            std::process::abort();
        };
        let device = &ctx.device;
        let strides = std::slice::from_raw_parts(group_strides, n_groups);
        let in_ptr_slice = std::slice::from_raw_parts(in_ptrs, n_groups);
        let mut bufs = Vec::with_capacity(n_groups);
        let mut sizes = Vec::with_capacity(n_groups);
        for (&p, &stride) in in_ptr_slice.iter().zip(strides.iter()) {
            let byte_len = n.saturating_mul(stride);
            let bytes = std::slice::from_raw_parts(p, byte_len);
            bufs.push(
                device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("gpu-4b-resident-input"),
                    contents: bytes,
                    usage: wgpu::BufferUsages::STORAGE
                        | wgpu::BufferUsages::COPY_SRC
                        | wgpu::BufferUsages::COPY_DST,
                }),
            );
            sizes.push(byte_len as u64);
        }
        resident_registry()
            .lock()
            .unwrap()
            .insert(handle, ResidentSoa { bufs, sizes, n });
        handle
    }
}

/// Dispatch a kernel against a RESIDENT input handle, producing a fresh resident
/// output handle — no host round-trip (GPU-SLIP-4b). Borrows the input (does not
/// free it): the caller frees both when their `gpu.Buffer` bindings drop, which
/// gives the double-buffer ping-pong its device-side lifecycle for free. Returns
/// a new opaque handle. Aborts on no GPU adapter / an unknown-or-freed input
/// handle.
///
/// # Safety
///
/// `wgsl_ptr`/`wgsl_len` a valid UTF-8 shader; `uniform_ptrs` an array of
/// `n_uniforms` pointers, each to `uniform_size` valid bytes. The returned handle
/// is owned by the caller.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_gpu_dispatch_resident(
    wgsl_ptr: *const u8,
    wgsl_len: usize,
    in_handle: u64,
    n_uniforms: usize,
    uniform_ptrs: *const *const u8,
    uniform_size: usize,
) -> u64 {
    unsafe {
        let Some(ctx) = gpu_context() else {
            crate::fatal::write_stderr(
                b"panic: gpu.dispatch found no available GPU adapter (no CPU fallback)\n",
            );
            std::process::abort();
        };
        let device = &ctx.device;
        let queue = &ctx.queue;
        let wgsl_bytes = std::slice::from_raw_parts(wgsl_ptr, wgsl_len);
        let Ok(wgsl) = std::str::from_utf8(wgsl_bytes) else {
            crate::fatal::write_stderr(b"panic: gpu.dispatch shader is not valid UTF-8\n");
            std::process::abort();
        };
        // Guard the empty case — codegen passes a null `uniform_ptrs` for a
        // zero-uniform kernel, and `from_raw_parts(null, 0)` violates the
        // aligned-non-null precondition (harmless in release, UB-flagged in debug).
        let uniforms: Vec<&[u8]> = if n_uniforms == 0 {
            Vec::new()
        } else {
            std::slice::from_raw_parts(uniform_ptrs, n_uniforms)
                .iter()
                .map(|&p| std::slice::from_raw_parts(p, uniform_size))
                .collect()
        };

        // Hold the registry lock across the (submit-only, non-blocking) dispatch: the
        // input buffers live in the registry and `wgpu::Buffer` is not clonable, so we
        // read them in place. The single-threaded sim loop never contends this.
        let mut reg = resident_registry().lock().unwrap();
        let (output_bufs, sizes, n) = {
            let Some(input) = reg.get(&in_handle) else {
                crate::fatal::write_stderr(
                    b"panic: gpu.dispatch on an unknown or already-freed device buffer\n",
                );
                std::process::abort();
            };
            if input.n == 0 {
                (Vec::new(), Vec::new(), 0)
            } else {
                let out = run_compute(
                    device,
                    queue,
                    wgsl,
                    &input.bufs,
                    &input.sizes,
                    &uniforms,
                    input.n,
                );
                (out, input.sizes.clone(), input.n)
            }
        };
        let handle = next_resident_handle();
        reg.insert(
            handle,
            ResidentSoa {
                bufs: output_bufs,
                sizes,
                n,
            },
        );
        handle
    }
}

/// Download a resident SoA handle back to a host AoS buffer and FREE the handle
/// (GPU-SLIP-4b): `gpu.download` moves the `gpu.Buffer[S]` back to a `Vec[S]`, so
/// the handle is consumed. Reads each group's device buffer, scatters the struct
/// fields into one freshly `malloc`'d `n * aos_stride` AoS buffer (the same
/// field-descriptor scheme as [`karac_runtime_gpu_dispatch_soa`]), and drops the
/// device buffers. The returned pointer is owned by the caller's `Vec[S]`. Empty
/// handle (`n == 0`) returns a unique non-null 1-byte allocation.
///
/// # Safety
///
/// `field_group`/`field_src`/`field_dst` arrays of `n_fields`. The returned
/// pointer transfers ownership. Aborts on no GPU adapter, an unknown handle, or a
/// device-buffer map failure.
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn karac_runtime_gpu_download_soa(
    handle: u64,
    n_fields: usize,
    field_group: *const usize,
    field_src: *const usize,
    field_dst: *const usize,
    field_size: usize,
    aos_stride: usize,
    n: usize,
) -> *mut u8 {
    unsafe {
        // Remove the handle up front — download consumes it (freeing the device
        // buffers when `resident` drops at end of scope).
        let Some(resident) = resident_registry().lock().unwrap().remove(&handle) else {
            crate::fatal::write_stderr(
                b"panic: gpu.download on an unknown or already-freed device buffer\n",
            );
            std::process::abort();
        };
        let aos_total = n.saturating_mul(aos_stride);
        if aos_total == 0 || resident.bufs.is_empty() {
            return crate::alloc::karac_alloc_or_panic(aos_total.max(1));
        }

        let Some(ctx) = gpu_context() else {
            crate::fatal::write_stderr(
                b"panic: gpu.download found no available GPU adapter (no CPU fallback)\n",
            );
            std::process::abort();
        };
        let Some(group_bytes) = readback(&ctx.device, &ctx.queue, &resident.bufs, &resident.sizes)
        else {
            crate::fatal::write_stderr(b"panic: gpu.download failed to map device buffers\n");
            std::process::abort();
        };

        // Scatter each struct field from its group's element to the AoS element —
        // identical to the round-trip `karac_runtime_gpu_dispatch_soa` scatter. Each
        // group's per-element stride is `sizes[g] / n`.
        let fgroup = std::slice::from_raw_parts(field_group, n_fields);
        let fsrc = std::slice::from_raw_parts(field_src, n_fields);
        let fdst = std::slice::from_raw_parts(field_dst, n_fields);
        let strides: Vec<usize> = resident.sizes.iter().map(|&s| (s as usize) / n).collect();
        let out = crate::alloc::karac_alloc_or_panic(aos_total);
        for f in 0..n_fields {
            let g = fgroup[f];
            let src_buf = &group_bytes[g];
            let gstride = strides[g];
            for i in 0..n {
                std::ptr::copy_nonoverlapping(
                    src_buf.as_ptr().add(i * gstride + fsrc[f]),
                    out.add(i * aos_stride + fdst[f]),
                    field_size,
                );
            }
        }
        // The device buffers are fully read (the readback poll waited); recycle them
        // for a subsequent frame's upload/dispatch (GPU-SLIP-4 buffer pooling).
        recycle_buffers(resident.bufs, &resident.sizes);
        out
    }
}

/// Free a resident SoA handle's device buffers (GPU-SLIP-4b) — the drop-glue
/// target for a `gpu.Buffer[S]` that goes out of scope without being downloaded
/// (the double-buffer ping-pong's displaced grids). Idempotent: a no-op for an
/// unknown or already-freed handle.
///
/// # Safety
///
/// Safe to call with any `u64`; only touches the process-wide registry.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_gpu_free_soa(handle: u64) {
    // Recycle the freed grid's device buffers into the pool (GPU-SLIP-4) so the
    // next dispatch reuses them instead of allocating. The registry lock is
    // released before touching the pool (no nested lock).
    let freed = resident_registry().lock().unwrap().remove(&handle);
    if let Some(r) = freed {
        recycle_buffers(r.bufs, &r.sizes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The canonical slice-0 kernel: element-wise `x * 2.0`. This is the WGSL
    // that slice-0b's codegen will eventually generate from `#[gpu] fn
    // double(x: f32) -> f32 { x * 2.0 }`; for slice-0a it is hand-written to
    // prove the runtime spine in isolation.
    const DOUBLE_WGSL: &str = r#"
@group(0) @binding(0) var<storage, read>       input:  array<f32>;
@group(0) @binding(1) var<storage, read_write> output: array<f32>;

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.y * 4194240u + gid.x;
    if (i >= arrayLength(&input)) { return; }
    output[i] = input[i] * 2.0;
}
"#;

    #[test]
    fn select_adapter_honors_index_and_reports_structured_errors() {
        // CG-6. All three legs run against the real instance: a valid index
        // (0) must yield an adapter when any exist; an out-of-range index
        // and a non-numeric value must produce the structured listing
        // error, never a silent default-adapter fallback.
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let n = pollster::block_on(instance.enumerate_adapters(wgpu::Backends::all())).len();
        if n == 0 {
            eprintln!("gpu-cg6: no GPU adapters — skipping");
            return;
        }
        assert!(
            select_adapter(&instance, Some("0"), None).is_ok(),
            "index 0 must select the first adapter"
        );
        let oor = select_adapter(&instance, Some("99"), None).unwrap_err();
        assert!(
            oor.contains("out of range") && oor.contains("KARAC_GPU=0:"),
            "out-of-range error must list available adapters; got: {oor}"
        );
        let bad = select_adapter(&instance, Some("metal"), None).unwrap_err();
        assert!(
            bad.contains("must be a device index") && bad.contains("KARAC_GPU=0:"),
            "non-numeric error must list available adapters; got: {bad}"
        );
    }

    #[test]
    fn select_adapter_honors_backend_cpu() {
        // CG-6 deferred leg: `KARAC_GPU_BACKEND=cpu` forces a software (CPU)
        // adapter. On a host with a software Vulkan implementation (lavapipe)
        // it selects the `DeviceType::Cpu` adapter; without one it must be a
        // structured error (never a silent fallback to a real/absent GPU).
        // A non-`cpu` value is rejected outright.
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let unsupported = select_adapter(&instance, None, Some("vulkan")).unwrap_err();
        assert!(
            unsupported.contains("only supports `cpu`"),
            "a non-cpu backend must be rejected; got: {unsupported}"
        );
        match select_adapter(&instance, None, Some("cpu")) {
            Ok(a) => assert_eq!(
                a.get_info().device_type,
                wgpu::DeviceType::Cpu,
                "KARAC_GPU_BACKEND=cpu must select a CPU adapter"
            ),
            Err(msg) => assert!(
                msg.contains("no software (CPU) adapter") && msg.contains("lavapipe"),
                "with no CPU adapter, the error must name the software-driver fix; got: {msg}"
            ),
        }
    }

    /// Single-workgroup tree reduction (B-2026-08-19-10, slice 1).
    ///
    /// The first shader in the repo to use `var<workgroup>` and
    /// `workgroupBarrier()` — the emitter has neither, which is exactly why a
    /// reduction cannot be written in Kāra today. Validated here as raw WGSL
    /// first, because THIS SHADER DEFINES THE SUMMATION ORDER, and the
    /// interpreter twin has to reproduce it bit-for-bit: a GPU tree reduction
    /// is not a left fold, and f32 addition is not associative, so the order
    /// is semantics rather than an implementation detail.
    ///
    /// Order: pad to the workgroup width with the identity, then halve —
    /// `s[t] += s[t + stride]` for stride 32, 16, 8, 4, 2, 1. Deterministic,
    /// and cheap to reproduce on the CPU.
    const SUM_REDUCE_WGSL: &str = r#"
@group(0) @binding(0) var<storage, read>       input:  array<f32>;
@group(0) @binding(1) var<storage, read_write> output: array<f32>;

var<workgroup> scratch: array<f32, 64>;

@compute @workgroup_size(64)
fn main(@builtin(local_invocation_id) lid: vec3<u32>,
        @builtin(workgroup_id) wid: vec3<u32>,
        @builtin(global_invocation_id) gid: vec3<u32>) {
    let t = lid.x;
    let i = gid.x;
    // Out-of-range lanes contribute the additive identity, so the tree is a
    // full 64 wide regardless of input length.
    if (i < arrayLength(&input)) { scratch[t] = input[i]; } else { scratch[t] = 0.0; }
    workgroupBarrier();

    var stride: u32 = 32u;
    loop {
        if (stride == 0u) { break; }
        if (t < stride) { scratch[t] = scratch[t] + scratch[t + stride]; }
        workgroupBarrier();
        stride = stride / 2u;
    }

    // Each workgroup writes ITS OWN partial; the host folds them.
    if (t == 0u) { output[wid.x] = scratch[0]; }
}
"#;

    /// The variance second-pass level-0 shader, as `karac`'s emitter generates
    /// it (`gpu_wgsl::emit_deviation_kernel("f32")`). Copied here so the
    /// UNIFORM binding — the first one in this file — can be proven against a
    /// real device before any of the compiler surface is built on it.
    const DEVIATION_WGSL: &str = r#"
@group(0) @binding(0) var<storage, read>       input:  array<f32>;
@group(0) @binding(1) var<storage, read_write> output: array<f32>;
@group(0) @binding(2) var<storage, read>       mean_u: array<f32>;

var<workgroup> scratch: array<f32, 64>;

@compute @workgroup_size(64)
fn main(@builtin(local_invocation_id) lid: vec3<u32>,
        @builtin(workgroup_id) wid: vec3<u32>,
        @builtin(global_invocation_id) gid: vec3<u32>) {
    let t = lid.x;
    let i = gid.x;
    // The ONLY difference from the sum shader: the squared
    // deviation is formed on load, so no n-element intermediate
    // is written. The mean arrives as a uniform because it cannot
    // be known until a whole reduction has already finished.
    if (i < arrayLength(&input)) {
        let d = input[i] - mean_u[0];
        scratch[t] = d * d;
    } else {
        scratch[t] = 0.0;
    }
    workgroupBarrier();

    var stride: u32 = 32u;
    loop {
        if (stride == 0u) { break; }
        if (t < stride) { scratch[t] = scratch[t] + scratch[t + stride]; }
        workgroupBarrier();
        stride = stride / 2u;
    }

    // One partial per workgroup; the host folds them with the
    // plain SUM shader, exactly as the dot path does.
    if (t == 0u) { output[wid.x] = scratch[0]; }
}
"#;

    /// The `argmin` level-0 and fold shaders, as `karac`'s emitter generates
    /// them (`gpu_wgsl::emit_arg_kernel(ReduceOp::Argmin, "f32", fold)`).
    /// Copied here so the pair-carrying tree, the NaN rule and the index
    /// padding sentinel can be proven against a real device before any of the
    /// compiler surface is built on them.
    const ARGMIN_SEED_WGSL: &str = r#"
@group(0) @binding(0) var<storage, read>       input:  array<f32>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;

var<workgroup> idxs: array<u32, 64>;

// The combine, verbatim from `reduce_kernel::arg_takes_b`: a strictly
// better value wins, an exact tie goes to the SMALLER index, and a NaN
// loses to anything real (ties with another NaN, where the smaller
// index survives). Lexicographic, therefore grouping-independent.
fn takes_b(ia: u32, ib: u32) -> bool {
    if (ib == 4294967295u) { return false; }
    if (ia == 4294967295u) { return true; }
    let a = input[ia];
    let b = input[ib];
    if (!(a == a)) { return (b == b); }
    if (!(b == b)) { return false; }
    return (b < a) || (b == a && ib < ia);
}

@compute @workgroup_size(64)
fn main(@builtin(local_invocation_id) lid: vec3<u32>,
        @builtin(workgroup_id) wid: vec3<u32>,
        @builtin(global_invocation_id) gid: vec3<u32>) {
    let t = lid.x;
    let i = gid.x;
    if (i < arrayLength(&input)) { idxs[t] = i; } else { idxs[t] = 4294967295u; }
    workgroupBarrier();

    var stride: u32 = 32u;
    loop {
        if (stride == 0u) { break; }
        if (t < stride) {
            if (takes_b(idxs[t], idxs[t + stride])) { idxs[t] = idxs[t + stride]; }
        }
        workgroupBarrier();
        stride = stride / 2u;
    }

    // One surviving candidate per workgroup, as an ABSOLUTE index
    // into `input` — so the next level can look its value up again.
    if (t == 0u) { output[wid.x] = idxs[0]; }
}
"#;

    const ARGMIN_FOLD_WGSL: &str = r#"
@group(0) @binding(0) var<storage, read>       input:  array<f32>;
@group(0) @binding(1) var<storage, read>       cand:   array<u32>;
@group(0) @binding(2) var<storage, read_write> output: array<u32>;

var<workgroup> idxs: array<u32, 64>;

// The combine, verbatim from `reduce_kernel::arg_takes_b`: a strictly
// better value wins, an exact tie goes to the SMALLER index, and a NaN
// loses to anything real (ties with another NaN, where the smaller
// index survives). Lexicographic, therefore grouping-independent.
fn takes_b(ia: u32, ib: u32) -> bool {
    if (ib == 4294967295u) { return false; }
    if (ia == 4294967295u) { return true; }
    let a = input[ia];
    let b = input[ib];
    if (!(a == a)) { return (b == b); }
    if (!(b == b)) { return false; }
    return (b < a) || (b == a && ib < ia);
}

@compute @workgroup_size(64)
fn main(@builtin(local_invocation_id) lid: vec3<u32>,
        @builtin(workgroup_id) wid: vec3<u32>,
        @builtin(global_invocation_id) gid: vec3<u32>) {
    let t = lid.x;
    let i = gid.x;
    if (i < arrayLength(&cand)) { idxs[t] = cand[i]; } else { idxs[t] = 4294967295u; }
    workgroupBarrier();

    var stride: u32 = 32u;
    loop {
        if (stride == 0u) { break; }
        if (t < stride) {
            if (takes_b(idxs[t], idxs[t + stride])) { idxs[t] = idxs[t + stride]; }
        }
        workgroupBarrier();
        stride = stride / 2u;
    }

    // One surviving candidate per workgroup, as an ABSOLUTE index
    // into `input` — so the next level can look its value up again.
    if (t == 0u) { output[wid.x] = idxs[0]; }
}
"#;

    /// The checked `i32` sum shader, as `karac`'s emitter generates it
    /// (`gpu_wgsl::emit_int_reduce_kernel(ReduceOp::Sum, "i32")`). Copied here
    /// so the OVERFLOW FLAG can be proven against a real device before any of
    /// the compiler surface is built on top of it.
    ///
    /// WGSL integer arithmetic is defined to WRAP and offers no overflow flag,
    /// so the shader computes one: a signed add overflowed iff the operands
    /// shared a sign and the result did not, which is
    /// `((a ^ s) & (b ^ s)) < 0`. The bit is OR-folded through the same
    /// halving tree as the value, so one overflowing lane at any stride
    /// reaches lane 0 and then the host.
    const INT_SUM_REDUCE_WGSL: &str = r#"
@group(0) @binding(0) var<storage, read>       input:  array<i32>;
@group(0) @binding(1) var<storage, read_write> output: array<i32>;
@group(0) @binding(2) var<storage, read_write> flags:  array<u32>;

var<workgroup> scratch: array<i32, 64>;
var<workgroup> ovf: array<u32, 64>;

@compute @workgroup_size(64)
fn main(@builtin(local_invocation_id) lid: vec3<u32>,
        @builtin(workgroup_id) wid: vec3<u32>,
        @builtin(global_invocation_id) gid: vec3<u32>) {
    let t = lid.x;
    let i = gid.x;
    if (i < arrayLength(&input)) { scratch[t] = input[i]; } else { scratch[t] = 0; }
    ovf[t] = 0u;
    workgroupBarrier();

    var stride: u32 = 32u;
    loop {
        if (stride == 0u) { break; }
        if (t < stride) {
            let a = scratch[t];
            let b = scratch[t + stride];
            let s = a + b;
            scratch[t] = s;
            // Signed add overflowed iff the operands shared a sign
            // and the result did not. WGSL wraps by definition,
            // so `s` is the wrapped value and this is exact.
            ovf[t] = ovf[t] | ovf[t + stride] | select(0u, 1u, ((a ^ s) & (b ^ s)) < 0);
        }
        workgroupBarrier();
        stride = stride / 2u;
    }

    // One partial AND one overflow bit per workgroup; the host ORs
    // the bits and re-dispatches over the partials.
    if (t == 0u) { output[wid.x] = scratch[0]; flags[wid.x] = ovf[0]; }
}
"#;

    /// The `gpu.dot` level-0 shader, as `karac`'s emitter generates it
    /// (`gpu_wgsl::emit_dot_kernel("f32")`). Two inputs, ONE output — the
    /// binding shape a reduction needs and an element-wise map does not.
    const DOT_KERNEL_WGSL: &str = r#"
@group(0) @binding(0) var<storage, read>       a:      array<f32>;
@group(0) @binding(1) var<storage, read>       b:      array<f32>;
@group(0) @binding(2) var<storage, read_write> output: array<f32>;

var<workgroup> scratch: array<f32, 64>;

@compute @workgroup_size(64)
fn main(@builtin(local_invocation_id) lid: vec3<u32>,
        @builtin(workgroup_id) wid: vec3<u32>,
        @builtin(global_invocation_id) gid: vec3<u32>) {
    let t = lid.x;
    let i = gid.x;
    if (i < arrayLength(&a)) { scratch[t] = a[i] * b[i]; } else { scratch[t] = 0.0; }
    workgroupBarrier();

    var stride: u32 = 32u;
    loop {
        if (stride == 0u) { break; }
        if (t < stride) { scratch[t] = scratch[t] + scratch[t + stride]; }
        workgroupBarrier();
        stride = stride / 2u;
    }

    if (t == 0u) { output[wid.x] = scratch[0]; }
}
"#;

    /// The float `min` reduction shader, as `karac`'s emitter generates it
    /// (`gpu_wgsl::emit_reduce_kernel(ReduceOp::Min, "f32")`). Copied here so
    /// the NaN and ±∞ behaviour can be proven against a real device without
    /// the compiler in the loop; `karac`'s
    /// `reduce_kernel_min_max_ignore_nan_rather_than_calling_the_builtin`
    /// pins the generator to the same shape.
    ///
    /// **It does not call WGSL's `min` builtin.** That builtin is specified as
    /// "returns `e2` if `e2 < e1`, and `e1` otherwise", and every comparison
    /// against NaN is false — so its answer depends on which side the NaN
    /// lands on. In a halving tree that position is decided by the grouping,
    /// which would make the result depend on the buffer length. The hand-
    /// written helper ignores NaN from either side, matching `f32::min`, which
    /// makes the operation associative and the tree well-defined.
    const MIN_REDUCE_WGSL: &str = r#"
@group(0) @binding(0) var<storage, read>       input:  array<f32>;
@group(0) @binding(1) var<storage, read_write> output: array<f32>;

var<workgroup> scratch: array<f32, 64>;

fn karac_min(a: f32, b: f32) -> f32 {
    if (!(a == a)) { return b; }
    if (!(b == b)) { return a; }
    return select(a, b, b < a);
}

@compute @workgroup_size(64)
fn main(@builtin(local_invocation_id) lid: vec3<u32>,
        @builtin(workgroup_id) wid: vec3<u32>,
        @builtin(global_invocation_id) gid: vec3<u32>) {
    let t = lid.x;
    let i = gid.x;
    if (i < arrayLength(&input)) { scratch[t] = input[i]; } else { scratch[t] = bitcast<f32>(0x7f800000u); }
    workgroupBarrier();

    var stride: u32 = 32u;
    loop {
        if (stride == 0u) { break; }
        if (t < stride) { scratch[t] = karac_min(scratch[t], scratch[t + stride]); }
        workgroupBarrier();
        stride = stride / 2u;
    }

    if (t == 0u) { output[wid.x] = scratch[0]; }
}
"#;

    /// The CPU twin of the multi-dispatch fold: chunk into workgroup-wide
    /// pieces, reduce each with [`tree_sum_f32`], recurse on the partials.
    /// Mirrors `karac_runtime_gpu_reduce_f32`'s `while count > 1` loop, and
    /// `karac`'s own `reduce_kernel::tree_fold_f32`.
    fn tree_sum_multi_f32(xs: &[f32]) -> f32 {
        if xs.len() <= 64 {
            return tree_sum_f32(xs);
        }
        let partials: Vec<f32> = xs.chunks(64).map(tree_sum_f32).collect();
        tree_sum_multi_f32(&partials)
    }

    /// The CPU twin of ONE workgroup of [`SUM_REDUCE_WGSL`] — same padding,
    /// same halving order.
    fn tree_sum_f32(xs: &[f32]) -> f32 {
        let mut scratch = [0.0f32; 64];
        for (t, slot) in scratch.iter_mut().enumerate() {
            *slot = xs.get(t).copied().unwrap_or(0.0);
        }
        let mut stride = 32usize;
        while stride > 0 {
            for t in 0..stride {
                scratch[t] += scratch[t + stride];
            }
            stride /= 2;
        }
        scratch[0]
    }

    #[test]
    fn sums_an_f32_buffer_with_a_workgroup_tree_reduction() {
        // n <= 64 dispatches exactly one workgroup, which is slice 1's scope.
        let input: Vec<f32> = (1..=64).map(|i| i as f32).collect();
        let Some(output) = dispatch_f32_map(SUM_REDUCE_WGSL, &input) else {
            eprintln!("gpu-reduce: no GPU adapter available — skipping");
            return;
        };
        // 1..=64 sums to 2080 exactly in f32, so this leg is order-independent
        // and catches a shader that is simply wrong.
        assert_eq!(output[0], 2080.0, "tree sum of 1..=64");
        // The order-DEPENDENT leg: the CPU twin must agree bit-for-bit.
        assert_eq!(
            output[0],
            tree_sum_f32(&input),
            "GPU tree order != CPU twin"
        );
    }

    #[test]
    fn reduce_entry_point_matches_the_cpu_twin_through_the_c_abi() {
        // Exercises `karac_runtime_gpu_reduce_f32` itself — the symbol codegen
        // will call — rather than the `#[cfg(test)]` helper the other tests
        // use. Catches anything wrong in the byte reinterpretation or the
        // slot-0 readback that the typed helper would paper over.
        let input: Vec<f32> = std::iter::repeat_n(0.1f32, 64).collect();
        // Adapterless hosts abort inside the entry point by design, so probe
        // first with the skippable helper and only then call the real one.
        if dispatch_f32_map(SUM_REDUCE_WGSL, &input).is_none() {
            eprintln!("gpu-reduce-abi: no GPU adapter available — skipping");
            return;
        }
        let got = unsafe {
            karac_runtime_gpu_reduce_f32(
                SUM_REDUCE_WGSL.as_ptr(),
                SUM_REDUCE_WGSL.len(),
                input.as_ptr(),
                input.len(),
                0.0,
            )
        };
        assert_eq!(got, tree_sum_f32(&input), "C entry point != CPU twin");
    }

    #[test]
    fn reduce_entry_point_returns_the_identity_for_an_empty_buffer() {
        // Needs no device: an empty reduction is the operation's identity, and
        // returning it without dispatching is what keeps a GPU-less host from
        // aborting on a program that never had any work to do.
        //
        // Both identities, because the empty case is the one input that never
        // reaches the shader: if this returned a hardcoded 0.0, an empty
        // `gpu.prod` would print 0 under `karac build` and 1 under `karac run`
        // — a run/build divergence on the cheapest possible program.
        for identity in [0.0f32, 1.0f32] {
            let got = unsafe {
                karac_runtime_gpu_reduce_f32(
                    SUM_REDUCE_WGSL.as_ptr(),
                    SUM_REDUCE_WGSL.len(),
                    std::ptr::null(),
                    0,
                    identity,
                )
            };
            assert_eq!(got, identity);
        }
    }

    #[test]
    fn min_reduce_shader_ignores_nan_and_pads_with_infinity() {
        // Proves the two things the emitter cannot prove on its own: that a
        // real device accepts `bitcast<f32>(0x7f800000u)` and a user-defined
        // helper inside a reduction shader, and that NaN actually behaves as
        // the helper specifies rather than as the driver prefers.
        let cases: &[(&str, Vec<f32>, f32)] = &[
            // Ordinary min, and a short buffer so the +inf padding is exercised.
            ("short", vec![3.0, 1.5, 2.0], 1.5),
            // NaN on either side of a real value must be IGNORED, not
            // propagated — and must give the SAME answer both ways. WGSL's
            // `min` builtin would answer 1.0 for one of these and NaN for the
            // other, which is precisely why the shader does not call it.
            ("nan-first", vec![f32::NAN, 1.0, 2.0], 1.0),
            ("nan-last", vec![2.0, 1.0, f32::NAN], 1.0),
            // f32::MAX must not be beaten by the padding identity — the whole
            // reason the identity is +inf rather than a large finite value.
            ("max-elem", vec![f32::MAX], f32::MAX),
        ];
        for (tag, input, want) in cases {
            let Some(output) = dispatch_f32_map(MIN_REDUCE_WGSL, input) else {
                eprintln!("gpu-min: no GPU adapter available — skipping");
                return;
            };
            assert_eq!(output[0], *want, "min {tag}");
            // The CPU twin must agree — this is the pair `karac` relies on.
            let twin = input.iter().copied().fold(f32::INFINITY, f32::min);
            assert_eq!(output[0], twin, "min {tag}: GPU != f32::min fold");
        }

        // An ALL-NaN buffer is the one case where the padding is observable:
        // every real element is ignored, so the +inf identity survives. The
        // twin says the same, which is what makes it specified rather than
        // accidental.
        let all_nan = vec![f32::NAN; 3];
        if let Some(output) = dispatch_f32_map(MIN_REDUCE_WGSL, &all_nan) {
            assert_eq!(output[0], f32::INFINITY, "all-NaN min is the identity");
            assert_eq!(
                output[0],
                all_nan.iter().copied().fold(f32::INFINITY, f32::min)
            );
        }
    }

    #[test]
    fn sumsq_dev_entry_point_matches_a_hand_rolled_two_pass() {
        // The first two-pass reduction, and the first UNIFORM binding. Proves
        // both against a real device: the mean is computed by a complete sum
        // reduction, read back, and handed to a second dispatch that squares
        // each deviation on load.
        for n in [3usize, 8, 64, 65, 4096] {
            let xs: Vec<f32> = (0..n).map(|i| ((i * 37) % 101) as f32 * 0.25).collect();
            if dispatch_f32_map(SUM_REDUCE_WGSL, &xs).is_none() {
                eprintln!("gpu-var: no GPU adapter available — skipping");
                return;
            }
            let got = unsafe {
                karac_runtime_gpu_sumsq_dev_f32(
                    DEVIATION_WGSL.as_ptr(),
                    DEVIATION_WGSL.len(),
                    SUM_REDUCE_WGSL.as_ptr(),
                    SUM_REDUCE_WGSL.len(),
                    xs.as_ptr(),
                    xs.len(),
                )
            };

            // Reproduced with the same trees the device uses: the mean from a
            // tree sum, then the squared deviations through a tree sum. Not a
            // naive left fold — that would disagree in the last ulp and the
            // whole family exists to be bit-exact.
            let mean = tree_sum_multi_f32(&xs) / n as f32;
            let squared: Vec<f32> = xs
                .iter()
                .map(|x| {
                    let d = x - mean;
                    d * d
                })
                .collect();
            assert_eq!(
                got.to_bits(),
                tree_sum_multi_f32(&squared).to_bits(),
                "n={n}"
            );
        }
    }

    #[test]
    fn sumsq_dev_entry_point_is_zero_for_an_empty_buffer() {
        // Needs no device, and never reaches the divide that would follow.
        let got = unsafe {
            karac_runtime_gpu_sumsq_dev_f32(
                DEVIATION_WGSL.as_ptr(),
                DEVIATION_WGSL.len(),
                SUM_REDUCE_WGSL.as_ptr(),
                SUM_REDUCE_WGSL.len(),
                std::ptr::null(),
                0,
            )
        };
        assert_eq!(got, 0.0);
    }

    #[test]
    fn sumsq_dev_is_zero_for_a_constant_buffer() {
        // Every deviation is zero, so the uniform must actually be the mean —
        // a uniform that arrived as 0.0 (an unbound or misbound binding) would
        // give the sum of squares instead, which is loudly nonzero here.
        let xs = vec![7.0f32; 200];
        if dispatch_f32_map(SUM_REDUCE_WGSL, &xs).is_none() {
            eprintln!("gpu-var-const: no GPU adapter available — skipping");
            return;
        }
        let got = unsafe {
            karac_runtime_gpu_sumsq_dev_f32(
                DEVIATION_WGSL.as_ptr(),
                DEVIATION_WGSL.len(),
                SUM_REDUCE_WGSL.as_ptr(),
                SUM_REDUCE_WGSL.len(),
                xs.as_ptr(),
                xs.len(),
            )
        };
        assert_eq!(got, 0.0, "constant buffer has zero deviation from its mean");
    }

    #[test]
    fn arg_entry_point_matches_the_cpu_twin_including_nan_and_ties() {
        // The Arg family's two hard parts, on a real device: the tree carries
        // (value, index) PAIRS rather than values, and the level-1+ shader
        // re-reads values from the ORIGINAL buffer through the surviving
        // candidate indices. Lengths cover one workgroup, a partial chunk, and
        // two full fold levels.
        let cases: Vec<(&str, Vec<f32>, u32)> = vec![
            ("simple", vec![3.0, 1.0, 2.0], 1),
            // Ties take the FIRST occurrence — a tie-break that depended on
            // scratch position would drift with the grouping.
            ("tie", vec![3.0, 1.0, 1.0, 5.0], 1),
            // NaN always loses, from either side.
            ("nan-first", vec![f32::NAN, 3.0, 1.0], 2),
            ("nan-last", vec![3.0, 1.0, f32::NAN], 1),
            // Nothing wins, so the leftmost survives.
            ("all-nan", vec![f32::NAN; 3], 0),
            // 65 is the first length needing a second chunk, so the winner
            // lives in a workgroup that is 63/64 PADDING. If padding could
            // win, this comes back as the sentinel instead of 64.
            (
                "spill",
                {
                    let mut v = vec![5.0f32; 65];
                    v[64] = -3.0;
                    v
                },
                64,
            ),
        ];
        for (tag, input, want) in &cases {
            if pollster::block_on(dispatch_arg_async(
                ARGMIN_SEED_WGSL,
                bytemuck_f32(input).as_slice(),
                None,
                input.len(),
            ))
            .is_none()
            {
                eprintln!("gpu-arg: no GPU adapter available — skipping");
                return;
            }
            let got = unsafe {
                karac_runtime_gpu_arg_index(
                    ARGMIN_SEED_WGSL.as_ptr(),
                    ARGMIN_SEED_WGSL.len(),
                    ARGMIN_FOLD_WGSL.as_ptr(),
                    ARGMIN_FOLD_WGSL.len(),
                    input.as_ptr().cast(),
                    input.len(),
                )
            };
            assert_eq!(got, *want, "argmin {tag}");
            assert_ne!(got, u32::MAX, "argmin {tag}: sentinel is never an answer");
        }
    }

    #[test]
    fn arg_entry_point_folds_two_full_levels() {
        // 4096 elements is 64 workgroups then one — so the fold shader runs
        // for real, re-reading values from the original buffer through the
        // candidate indices rather than carrying them across the dispatch.
        let mut input: Vec<f32> = (0..4096).map(|i| ((i * 37) % 101) as f32).collect();
        input[3000] = -1.0;
        if pollster::block_on(dispatch_arg_async(
            ARGMIN_SEED_WGSL,
            bytemuck_f32(&input).as_slice(),
            None,
            input.len(),
        ))
        .is_none()
        {
            eprintln!("gpu-arg-multi: no GPU adapter available — skipping");
            return;
        }
        let got = unsafe {
            karac_runtime_gpu_arg_index(
                ARGMIN_SEED_WGSL.as_ptr(),
                ARGMIN_SEED_WGSL.len(),
                ARGMIN_FOLD_WGSL.as_ptr(),
                ARGMIN_FOLD_WGSL.len(),
                input.as_ptr().cast(),
                input.len(),
            )
        };
        assert_eq!(
            got, 3000,
            "the unique minimum, found across two fold levels"
        );
    }

    #[test]
    fn arg_entry_point_returns_the_sentinel_for_an_empty_buffer() {
        // Needs no device: an empty buffer has no extremum. The caller turns
        // the sentinel into `None`.
        let got = unsafe {
            karac_runtime_gpu_arg_index(
                ARGMIN_SEED_WGSL.as_ptr(),
                ARGMIN_SEED_WGSL.len(),
                ARGMIN_FOLD_WGSL.as_ptr(),
                ARGMIN_FOLD_WGSL.len(),
                std::ptr::null(),
                0,
            )
        };
        assert_eq!(got, u32::MAX);
    }

    fn bytemuck_f32(xs: &[f32]) -> Vec<u8> {
        xs.iter().flat_map(|v| v.to_le_bytes()).collect()
    }

    #[test]
    fn int_sum_shader_raises_the_overflow_flag_on_a_real_device() {
        // The claim the whole integer-reduction decision rests on: a GPU can
        // detect integer overflow even though WGSL cannot trap. Exercised
        // through the dispatch helper rather than the C entry point, because
        // that entry point ABORTS on overflow by design and an aborting
        // process cannot be asserted about in-process.
        let Some((values, flags)) = pollster::block_on(dispatch_checked_int_async(
            INT_SUM_REDUCE_WGSL,
            bytemuck_i32(&[3, 1, 2]).as_slice(),
            4,
        )) else {
            eprintln!("gpu-int-sum: no GPU adapter available — skipping");
            return;
        };
        assert_eq!(read_i32(&values, 0), 6, "3 + 1 + 2");
        assert_eq!(read_u32(&flags, 0), 0, "no overflow on a small sum");

        // Now overflow it. `[i32::MAX, 1]` is the minimal case, and the flag
        // must come back set rather than the wrapped `i32::MIN` passing as an
        // answer.
        let (values, flags) = pollster::block_on(dispatch_checked_int_async(
            INT_SUM_REDUCE_WGSL,
            bytemuck_i32(&[i32::MAX, 1]).as_slice(),
            4,
        ))
        .expect("adapter was available a moment ago");
        assert_eq!(read_u32(&flags, 0), 1, "i32::MAX + 1 must raise the flag");
        // The value IS the wrapped one — WGSL has no choice — which is exactly
        // why the flag has to exist. Reading it without checking the flag is
        // the failure mode being prevented.
        assert_eq!(read_i32(&values, 0), i32::MIN);
    }

    #[test]
    fn int_sum_flag_survives_every_stride_of_the_tree() {
        // The bit is OR-folded through the halving tree, so an overflow at ANY
        // stride has to reach lane 0 — not just one that happens at the last
        // step. These two buffers overflow at different depths: the adjacent
        // pair meets at stride 1 (after 63 zeros collapse), while the pair 32
        // apart meets at the very first stride.
        for (tag, pos) in [("stride-1", 1usize), ("stride-32", 32usize)] {
            let mut xs = vec![0i32; 64];
            xs[0] = i32::MAX;
            xs[pos] = 1;
            let Some((_, flags)) = pollster::block_on(dispatch_checked_int_async(
                INT_SUM_REDUCE_WGSL,
                bytemuck_i32(&xs).as_slice(),
                4,
            )) else {
                eprintln!("gpu-int-sum-stride: no GPU adapter available — skipping");
                return;
            };
            assert_eq!(
                read_u32(&flags, 0),
                1,
                "overflow at {tag} must reach lane 0"
            );
        }
    }

    #[test]
    fn int_sum_entry_point_matches_the_cpu_twin_across_workgroups() {
        // The success path through the real C entry point, including the
        // multi-level fold. Values chosen to stay well inside i32 so nothing
        // aborts.
        for n in [3usize, 64, 65, 4096] {
            let xs: Vec<i32> = (0..n).map(|i| (i % 11) as i32 - 5).collect();
            if pollster::block_on(dispatch_checked_int_async(
                INT_SUM_REDUCE_WGSL,
                bytemuck_i32(&xs).as_slice(),
                4,
            ))
            .is_none()
            {
                eprintln!("gpu-int-sum-abi: no GPU adapter available — skipping");
                return;
            }
            let mut got = 0i32;
            let status = unsafe {
                karac_runtime_gpu_reduce_i32(
                    INT_SUM_REDUCE_WGSL.as_ptr(),
                    INT_SUM_REDUCE_WGSL.len(),
                    xs.as_ptr(),
                    xs.len(),
                    0,
                    &mut got,
                )
            };
            assert_eq!(status, 0, "n={n}: no overflow expected");
            let want: i32 = xs.iter().sum();
            assert_eq!(got, want, "n={n}");
        }

        // Empty needs no device and cannot overflow.
        let mut got = 99i32;
        let status = unsafe {
            karac_runtime_gpu_reduce_i32(
                INT_SUM_REDUCE_WGSL.as_ptr(),
                INT_SUM_REDUCE_WGSL.len(),
                std::ptr::null(),
                0,
                0,
                &mut got,
            )
        };
        assert_eq!(status, 0);
        assert_eq!(got, 0, "empty integer sum is the additive identity");
    }

    #[test]
    fn int_sum_entry_point_reports_overflow_instead_of_aborting() {
        // The entry point RETURNS the overflow rather than aborting, so
        // codegen can raise Kāra's own panic at the call site — same message,
        // exit code and span `v.sum()` gives for the identical condition. An
        // abort in here would be a bare SIGABRT with no span, and would also
        // make this very test impossible to write.
        let xs = [i32::MAX, 1];
        if pollster::block_on(dispatch_checked_int_async(
            INT_SUM_REDUCE_WGSL,
            bytemuck_i32(&xs).as_slice(),
            4,
        ))
        .is_none()
        {
            eprintln!("gpu-int-ovf: no GPU adapter available — skipping");
            return;
        }
        let mut got = 0i32;
        let status = unsafe {
            karac_runtime_gpu_reduce_i32(
                INT_SUM_REDUCE_WGSL.as_ptr(),
                INT_SUM_REDUCE_WGSL.len(),
                xs.as_ptr(),
                xs.len(),
                0,
                &mut got,
            )
        };
        assert_eq!(status, 1, "i32::MAX + 1 must report overflow");
    }

    fn bytemuck_i32(xs: &[i32]) -> Vec<u8> {
        xs.iter().flat_map(|v| v.to_le_bytes()).collect()
    }
    fn read_i32(b: &[u8], i: usize) -> i32 {
        i32::from_le_bytes([b[i * 4], b[i * 4 + 1], b[i * 4 + 2], b[i * 4 + 3]])
    }
    fn read_u32(b: &[u8], i: usize) -> u32 {
        u32::from_le_bytes([b[i * 4], b[i * 4 + 1], b[i * 4 + 2], b[i * 4 + 3]])
    }

    #[test]
    fn dot_entry_point_equals_the_sum_of_the_products() {
        // The guarantee the whole two-shader design exists to hold:
        // `gpu.dot(a, b)` IS `gpu.sum(a * b)`, to the last bit. It holds
        // structurally rather than by luck — after level 0 the two paths run
        // the identical sum shader over identical partials — and this checks
        // it on a real device rather than trusting the argument.
        //
        // Lengths chosen to cover all three regimes: inside one workgroup, one
        // partial chunk, and two full levels of folding.
        for n in [3usize, 64, 65, 200, 4096] {
            let a: Vec<f32> = (0..n).map(|i| 0.5 + (i % 7) as f32).collect();
            let b: Vec<f32> = (0..n).map(|i| 1.5 - (i % 3) as f32).collect();

            // Probe with the skippable helper first: the entry points abort on
            // an adapterless host by design.
            if dispatch_f32_map(SUM_REDUCE_WGSL, &a).is_none() {
                eprintln!("gpu-dot: no GPU adapter available — skipping");
                return;
            }

            let dot = unsafe {
                karac_runtime_gpu_dot_f32(
                    DOT_KERNEL_WGSL.as_ptr(),
                    DOT_KERNEL_WGSL.len(),
                    SUM_REDUCE_WGSL.as_ptr(),
                    SUM_REDUCE_WGSL.len(),
                    a.as_ptr(),
                    a.len(),
                    b.as_ptr(),
                    b.len(),
                )
            };

            let products: Vec<f32> = a.iter().zip(&b).map(|(x, y)| x * y).collect();
            let summed = unsafe {
                karac_runtime_gpu_reduce_f32(
                    SUM_REDUCE_WGSL.as_ptr(),
                    SUM_REDUCE_WGSL.len(),
                    products.as_ptr(),
                    products.len(),
                    0.0,
                )
            };
            assert_eq!(
                dot.to_bits(),
                summed.to_bits(),
                "n={n}: gpu.dot != gpu.sum of the products"
            );

            // And both agree with the CPU twin's grouping — the fused level-0
            // shader must chunk exactly like the unfused one.
            assert_eq!(
                dot.to_bits(),
                tree_sum_multi_f32(&products).to_bits(),
                "n={n}"
            );
        }
    }

    #[test]
    fn dot_entry_point_is_zero_for_an_empty_buffer() {
        // Needs no device: the empty dot product is the additive identity.
        let got = unsafe {
            karac_runtime_gpu_dot_f32(
                DOT_KERNEL_WGSL.as_ptr(),
                DOT_KERNEL_WGSL.len(),
                SUM_REDUCE_WGSL.as_ptr(),
                SUM_REDUCE_WGSL.len(),
                std::ptr::null(),
                0,
                std::ptr::null(),
                0,
            )
        };
        assert_eq!(got, 0.0);
    }

    #[test]
    fn reduce_entry_point_folds_a_multi_workgroup_buffer() {
        // The whole point of the multi-dispatch loop: 4096 elements is 64
        // workgroups on the first pass, then one on the second. A shader that
        // wrote `output[0]` instead of `output[wid.x]` would have every
        // workgroup race on one slot and the answer would be a single chunk's
        // — 64.0 here rather than 4096.0.
        let input: Vec<f32> = vec![1.0f32; 4096];
        if dispatch_f32_map(SUM_REDUCE_WGSL, &input).is_none() {
            eprintln!("gpu-reduce-multi: no GPU adapter available — skipping");
            return;
        }
        let got = unsafe {
            karac_runtime_gpu_reduce_f32(
                SUM_REDUCE_WGSL.as_ptr(),
                SUM_REDUCE_WGSL.len(),
                input.as_ptr(),
                input.len(),
                0.0,
            )
        };
        // Order-independent leg: 4096 ones sum exactly however you group them.
        assert_eq!(got, 4096.0, "multi-workgroup sum of 4096 ones");

        // Order-DEPENDENT leg: 0.1s, where the grouping is observable. The
        // tree of trees is not the same number as a flat tree or a left fold,
        // and the CPU twin has to reproduce the chunking, not just "a tree".
        let drifty: Vec<f32> = vec![0.1f32; 4096];
        let got = unsafe {
            karac_runtime_gpu_reduce_f32(
                SUM_REDUCE_WGSL.as_ptr(),
                SUM_REDUCE_WGSL.len(),
                drifty.as_ptr(),
                drifty.len(),
                0.0,
            )
        };
        assert_eq!(
            got.to_bits(),
            tree_sum_multi_f32(&drifty).to_bits(),
            "multi-workgroup GPU fold != CPU twin"
        );
    }

    #[test]
    fn tree_sum_differs_from_a_left_fold_and_is_closer_to_the_truth() {
        // Why the order is SPECIFIED rather than incidental. Sixty-four copies
        // of 0.1: a left fold drifts to 6.399996, the tree gives 6.400000 —
        // and the tree is the more accurate answer, because pairing keeps the
        // partial sums at similar magnitudes instead of repeatedly adding a
        // tiny value to a growing one.
        //
        // (A first draft of this test used 1e9 followed by 1.0s, on the theory
        // that the big value would swamp the small ones. It does — in BOTH
        // orders, since index 0 pairs with index 32 on the very first tree
        // step. The test passed vacuously by asserting a difference that was
        // not there, which is why the inputs here were computed rather than
        // guessed.)
        let input: Vec<f32> = std::iter::repeat_n(0.1f32, 64).collect();
        let left: f32 = input.iter().fold(0.0, |a, b| a + b);
        let tree = tree_sum_f32(&input);
        assert_ne!(left, tree, "0.1 x 64 must expose the order difference");
        assert!(
            (tree - 6.4f32).abs() < (left - 6.4f32).abs(),
            "the tree order should be the more accurate one: tree={tree}, left={left}"
        );

        let Some(output) = dispatch_f32_map(SUM_REDUCE_WGSL, &input) else {
            eprintln!("gpu-reduce-order: no GPU adapter available — skipping");
            return;
        };
        assert_eq!(output[0], tree, "GPU must match the TREE");
        assert_ne!(output[0], left, "GPU must not match the left fold");
    }

    #[test]
    fn doubles_an_f32_buffer_on_the_gpu() {
        let input: Vec<f32> = (0..256).map(|i| i as f32).collect();
        let Some(output) = dispatch_f32_map(DOUBLE_WGSL, &input) else {
            eprintln!("gpu-slice0a: no GPU adapter available — skipping");
            return;
        };
        assert_eq!(output.len(), input.len(), "output length mismatch");
        for (i, (&inp, &out)) in input.iter().zip(output.iter()).enumerate() {
            assert_eq!(out, inp * 2.0, "element {i}: {inp} * 2.0 != {out}");
        }
    }

    #[test]
    fn doubles_past_the_1d_dispatch_cap_via_2d_grid() {
        // The 1D dispatch cap is 65535 workgroups × 64 = 4,194,240 elements —
        // anything larger used to PANIC in wgpu validation ("dispatch group
        // size ... must be ≤ 65535"). The 2D spread (x fixed at 65535, flat
        // index recovered as `gid.y * 4194240 + gid.x`) must produce exact
        // results past the cap. 5,308,416 = 2304² — the first LBM-shaped
        // grid size that crashed. ~21 MiB of f32 in/out; graceful skip
        // without a GPU adapter (headless CI).
        let n: usize = 2304 * 2304;
        let input: Vec<f32> = (0..n).map(|i| (i % 8192) as f32).collect();
        let Some(output) = dispatch_f32_map(DOUBLE_WGSL, &input) else {
            eprintln!("gpu-grid-2d: no GPU adapter available — skipping");
            return;
        };
        assert_eq!(output.len(), input.len(), "output length mismatch");
        // Spot-check the row boundaries the 2D recovery must get right:
        // below/at/above the old cap, plus head and tail.
        for &i in &[0usize, 1, 4_194_239, 4_194_240, 4_194_241, n - 2, n - 1] {
            assert_eq!(
                output[i],
                input[i] * 2.0,
                "element {i} wrong across the 2D dispatch boundary"
            );
        }
        // And the whole buffer, cheaply.
        let sum_in: f64 = input.iter().map(|&v| v as f64).sum();
        let sum_out: f64 = output.iter().map(|&v| v as f64).sum();
        assert_eq!(sum_out, sum_in * 2.0, "checksum mismatch past the cap");
    }

    // CG-4 multi-buffer kernel: the Path-A Particle step over two coalesced
    // f32 field-arrays (pos, vel) — one `array<f32>` binding per layout group.
    // This is the WGSL the emitter will generate from
    // `#[gpu] fn step(p: Particle) -> Particle { Particle { pos: p.pos + p.vel, vel: p.vel } }`
    // over `layout world: Vec[Particle] { group gp { pos } group gv { vel } }`.
    const PARTICLE_STEP_WGSL: &str = r#"
@group(0) @binding(0) var<storage, read>       gp_in:  array<f32>;
@group(0) @binding(1) var<storage, read>       gv_in:  array<f32>;
@group(0) @binding(2) var<storage, read_write> gp_out: array<f32>;
@group(0) @binding(3) var<storage, read_write> gv_out: array<f32>;

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.y * 4194240u + gid.x;
    if (i >= arrayLength(&gp_in)) { return; }
    let p_pos = gp_in[i];
    let p_vel = gv_in[i];
    gp_out[i] = p_pos + p_vel;
    gv_out[i] = p_vel;
}
"#;

    fn f32s_to_le(xs: &[f32]) -> Vec<u8> {
        xs.iter().flat_map(|x| x.to_le_bytes()).collect()
    }
    fn le_to_f32s(bytes: &[u8]) -> Vec<f32> {
        bytes
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect()
    }

    #[test]
    fn multi_buffer_particle_step_on_the_gpu() {
        let pos: Vec<f32> = (0..256).map(|i| i as f32).collect();
        let vel: Vec<f32> = (0..256).map(|i| (i as f32) * 0.5 + 1.0).collect();
        let pos_bytes = f32s_to_le(&pos);
        let vel_bytes = f32s_to_le(&vel);

        let Some(outs) = pollster::block_on(dispatch_multi_bytes_async(
            PARTICLE_STEP_WGSL,
            &[&pos_bytes, &vel_bytes],
            &[],
            256, // elem_count (one GPU thread per logical element)
        )) else {
            eprintln!("gpu-cg4: no GPU adapter available — skipping");
            return;
        };
        assert_eq!(outs.len(), 2, "expected one output buffer per group");
        let pos_out = le_to_f32s(&outs[0]);
        let vel_out = le_to_f32s(&outs[1]);
        assert_eq!(pos_out.len(), 256);
        assert_eq!(vel_out.len(), 256);
        for i in 0..256 {
            assert_eq!(pos_out[i], pos[i] + vel[i], "pos[{i}]");
            assert_eq!(vel_out[i], vel[i], "vel[{i}]");
        }
    }

    // GPU-LBM-3b: heterogeneous group strides — a 2-field group `ab` bound as
    // `array<G_ab>` (8-byte elements) alongside a 1-field group `cg` bound as
    // `array<f32>` (4-byte). Proves the core handles per-group byte lengths.
    const MULTI_FIELD_WGSL: &str = r#"
struct G_ab { a: f32, b: f32 };
@group(0) @binding(0) var<storage, read>       ab_in:  array<G_ab>;
@group(0) @binding(1) var<storage, read>       cg_in:  array<f32>;
@group(0) @binding(2) var<storage, read_write> ab_out: array<G_ab>;
@group(0) @binding(3) var<storage, read_write> cg_out: array<f32>;

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.y * 4194240u + gid.x;
    if (i >= arrayLength(&cg_in)) { return; }
    let a = ab_in[i].a;
    let b = ab_in[i].b;
    let c = cg_in[i];
    ab_out[i] = G_ab(a + c, b);
    cg_out[i] = c;
}
"#;

    #[test]
    fn multi_field_group_stride_dispatch() {
        let n = 128usize;
        // ab group element = {a, b} (8 bytes); cg group element = {c} (4 bytes).
        let mut ab_bytes = Vec::new();
        let mut cg_bytes = Vec::new();
        for i in 0..n {
            ab_bytes.extend_from_slice(&(i as f32).to_le_bytes()); // a
            ab_bytes.extend_from_slice(&((i as f32) * 2.0).to_le_bytes()); // b
            cg_bytes.extend_from_slice(&(100.0f32).to_le_bytes()); // c
        }
        let Some(outs) = pollster::block_on(dispatch_multi_bytes_async(
            MULTI_FIELD_WGSL,
            &[&ab_bytes, &cg_bytes],
            &[],
            n,
        )) else {
            eprintln!("gpu-cg4: no GPU adapter available — skipping");
            return;
        };
        assert_eq!(outs[0].len(), n * 8, "ab group is 8 bytes/elem");
        assert_eq!(outs[1].len(), n * 4, "cg group is 4 bytes/elem");
        for i in 0..n {
            let a = f32::from_le_bytes(outs[0][i * 8..i * 8 + 4].try_into().unwrap());
            let b = f32::from_le_bytes(outs[0][i * 8 + 4..i * 8 + 8].try_into().unwrap());
            let c = f32::from_le_bytes(outs[1][i * 4..i * 4 + 4].try_into().unwrap());
            assert_eq!(a, i as f32 + 100.0, "a[{i}]"); // a + c
            assert_eq!(b, (i as f32) * 2.0, "b[{i}]"); // unchanged
            assert_eq!(c, 100.0, "c[{i}]"); // unchanged
        }
    }

    // GPU-LBM-2: a scalar uniform `k` bound at `@binding(2n)` (after the group
    // in/out buffers) as a 1-element `array<f32>`, read `k_u[0]`.
    const UNIFORM_WGSL: &str = r#"
@group(0) @binding(0) var<storage, read>       gp_in:  array<f32>;
@group(0) @binding(1) var<storage, read_write> gp_out: array<f32>;
@group(0) @binding(2) var<storage, read>       k_u:    array<f32>;

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.y * 4194240u + gid.x;
    if (i >= arrayLength(&gp_in)) { return; }
    gp_out[i] = gp_in[i] * k_u[0];
}
"#;

    #[test]
    fn single_uniform_dispatch() {
        let n = 64usize;
        let input: Vec<f32> = (0..n).map(|i| i as f32).collect();
        let in_bytes: Vec<u8> = input.iter().flat_map(|x| x.to_le_bytes()).collect();
        let k: f32 = 3.0;
        let k_bytes = k.to_le_bytes().to_vec();
        let Some(outs) = pollster::block_on(dispatch_multi_bytes_async(
            UNIFORM_WGSL,
            &[&in_bytes],
            &[&k_bytes],
            n,
        )) else {
            eprintln!("gpu-lbm2: no GPU adapter available — skipping");
            return;
        };
        let out: Vec<f32> = outs[0]
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect();
        for i in 0..n {
            assert_eq!(out[i], input[i] * k, "elem {i}");
        }
    }

    // GPU-SLIP-4b: the resident-buffer path — upload once, dispatch device→device
    // across N substeps (ping-pong, freeing each consumed grid), download once.
    // The Particle step is `pos += vel; vel unchanged`, so after `STEPS` resident
    // dispatches `pos == pos0 + STEPS*vel` — proving the output of one dispatch is
    // correctly consumed as the input of the next with NO host round-trip, and
    // that the download AoS scatter matches the round-trip path's result.
    #[test]
    fn resident_ping_pong_particle_step() {
        if gpu_context().is_none() {
            eprintln!("gpu-4b: no GPU adapter available — skipping");
            return;
        }
        extern "C" {
            // Match the crate-wide `free` signature (map.rs / lib.rs) to avoid a
            // clashing-extern-declarations lint — same C symbol, one signature.
            fn free(ptr: *mut core::ffi::c_void);
        }
        const N: usize = 200;
        const STEPS: usize = 12;
        let pos0: Vec<f32> = (0..N).map(|i| i as f32).collect();
        let vel: Vec<f32> = (0..N).map(|i| (i as f32) * 0.25 + 1.0).collect();
        let pos_bytes = f32s_to_le(&pos0);
        let vel_bytes = f32s_to_le(&vel);

        // Upload the two group-arrays (pos, vel); 4-byte f32 elements each.
        let in_ptrs = [pos_bytes.as_ptr(), vel_bytes.as_ptr()];
        let strides = [4usize, 4usize];
        let mut handle =
            unsafe { karac_runtime_gpu_upload_soa(2, in_ptrs.as_ptr(), strides.as_ptr(), N) };
        assert_ne!(handle, 0, "upload returned a null handle");

        // Ping-pong: each dispatch produces a new resident handle; free the old
        // one (what a `gpu.Buffer` ownership drop does in the compiled loop).
        for _ in 0..STEPS {
            let next = unsafe {
                karac_runtime_gpu_dispatch_resident(
                    PARTICLE_STEP_WGSL.as_ptr(),
                    PARTICLE_STEP_WGSL.len(),
                    handle,
                    0,
                    std::ptr::null(),
                    0,
                )
            };
            unsafe { karac_runtime_gpu_free_soa(handle) };
            handle = next;
        }

        // Download to AoS {pos: f32 @0, vel: f32 @4}: field 0 (pos) in group 0
        // (gp), field 1 (vel) in group 1 (gv), each src offset 0, dst 0/4, 8-byte
        // AoS stride. Consumes the handle.
        let field_group = [0usize, 1];
        let field_src = [0usize, 0];
        let field_dst = [0usize, 4];
        let aos = unsafe {
            karac_runtime_gpu_download_soa(
                handle,
                2,
                field_group.as_ptr(),
                field_src.as_ptr(),
                field_dst.as_ptr(),
                4,
                8,
                N,
            )
        };
        assert!(!aos.is_null());
        let aos_bytes = unsafe { std::slice::from_raw_parts(aos, N * 8) };
        for i in 0..N {
            let pos = f32::from_le_bytes(aos_bytes[i * 8..i * 8 + 4].try_into().unwrap());
            let v = f32::from_le_bytes(aos_bytes[i * 8 + 4..i * 8 + 8].try_into().unwrap());
            assert_eq!(
                pos,
                pos0[i] + STEPS as f32 * vel[i],
                "pos[{i}] after {STEPS} resident steps"
            );
            assert_eq!(v, vel[i], "vel[{i}] unchanged");
        }
        unsafe { free(aos as *mut core::ffi::c_void) };
    }

    // Regression guard for the raised device limits (`adapter.limits()` instead of
    // `Limits::default()`). A 5-group SoA kernel binds 5 inputs + 5 outputs = 10
    // storage buffers — over wgpu's default `max_storage_buffers_per_shader_stage`
    // of 8, so before the limit fix this dispatch panicked at pipeline creation
    // with "Too many bindings of type StorageBuffers ... limit is 8, count was 10".
    // The real Slipstream D2Q9 collide is 9 fields (18 buffers), so this class of
    // kernel must dispatch. Bindings follow run_compute's convention: inputs
    // @binding(0..5), outputs @binding(5..10).
    const FIVE_GROUP_WGSL: &str = r#"
@group(0) @binding(0) var<storage, read>       a_in: array<f32>;
@group(0) @binding(1) var<storage, read>       b_in: array<f32>;
@group(0) @binding(2) var<storage, read>       c_in: array<f32>;
@group(0) @binding(3) var<storage, read>       d_in: array<f32>;
@group(0) @binding(4) var<storage, read>       e_in: array<f32>;
@group(0) @binding(5) var<storage, read_write> a_out: array<f32>;
@group(0) @binding(6) var<storage, read_write> b_out: array<f32>;
@group(0) @binding(7) var<storage, read_write> c_out: array<f32>;
@group(0) @binding(8) var<storage, read_write> d_out: array<f32>;
@group(0) @binding(9) var<storage, read_write> e_out: array<f32>;
@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.y * 4194240u + gid.x;
    if (i >= arrayLength(&a_in)) { return; }
    a_out[i] = a_in[i] + 1.0;
    b_out[i] = b_in[i] + 2.0;
    c_out[i] = c_in[i] + 3.0;
    d_out[i] = d_in[i] + 4.0;
    e_out[i] = e_in[i] + 5.0;
}
"#;

    #[test]
    fn five_group_kernel_exceeds_default_storage_buffer_limit() {
        if gpu_context().is_none() {
            eprintln!("gpu: no GPU adapter available — skipping");
            return;
        }
        extern "C" {
            fn free(ptr: *mut core::ffi::c_void);
        }
        const N: usize = 128;
        let groups: Vec<Vec<u8>> = (0..5)
            .map(|g| f32s_to_le(&(0..N).map(|i| (g * 100 + i) as f32).collect::<Vec<_>>()))
            .collect();
        let in_ptrs: Vec<*const u8> = groups.iter().map(|v| v.as_ptr()).collect();
        let strides = [4usize; 5];
        let handle =
            unsafe { karac_runtime_gpu_upload_soa(5, in_ptrs.as_ptr(), strides.as_ptr(), N) };
        assert_ne!(handle, 0, "upload returned a null handle");
        let out = unsafe {
            karac_runtime_gpu_dispatch_resident(
                FIVE_GROUP_WGSL.as_ptr(),
                FIVE_GROUP_WGSL.len(),
                handle,
                0,
                std::ptr::null(),
                0,
            )
        };
        unsafe { karac_runtime_gpu_free_soa(handle) };
        assert_ne!(out, 0, "10-buffer dispatch returned a null handle");

        // Download all 5 groups into a 20-byte AoS record (field g at offset 4*g).
        let field_group = [0usize, 1, 2, 3, 4];
        let field_src = [0usize; 5];
        let field_dst = [0usize, 4, 8, 12, 16];
        let aos = unsafe {
            karac_runtime_gpu_download_soa(
                out,
                5,
                field_group.as_ptr(),
                field_src.as_ptr(),
                field_dst.as_ptr(),
                4,
                20,
                N,
            )
        };
        assert!(!aos.is_null());
        let bytes = unsafe { std::slice::from_raw_parts(aos, N * 20) };
        for i in 0..N {
            for g in 0..5 {
                let v = f32::from_le_bytes(
                    bytes[i * 20 + g * 4..i * 20 + g * 4 + 4]
                        .try_into()
                        .unwrap(),
                );
                let expect = (g * 100 + i) as f32 + (g as f32 + 1.0);
                assert_eq!(v, expect, "group {g} elem {i}");
            }
        }
        unsafe { free(aos as *mut core::ffi::c_void) };
    }

    // GPU-SLIP-4f regression guard: a STENCIL shader (reads a NEIGHBOUR, not just
    // its own element) dispatched over a RESIDENT buffer. The resident path was
    // built for element-wise collide; this locks in that `dispatch_resident` binds
    // the WHOLE grid read-only (`as_entire_binding`), so a shader reading `in[i-1]`
    // sees the neighbour — the property the resident LBM `stream` pass depends on.
    // Shifts each element from its left neighbour (clamped at 0): out[i] = in[i-1].
    const STENCIL_SHIFT_WGSL: &str = r#"
@group(0) @binding(0) var<storage, read>       a_in:  array<f32>;
@group(0) @binding(1) var<storage, read_write> a_out: array<f32>;
@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.y * 4194240u + gid.x;
    if (i >= arrayLength(&a_in)) { return; }
    if (i == 0u) { a_out[0] = a_in[0]; } else { a_out[i] = a_in[i - 1u]; }
}
"#;

    #[test]
    fn resident_stencil_reads_neighbour() {
        if gpu_context().is_none() {
            eprintln!("gpu: no GPU adapter available — skipping");
            return;
        }
        extern "C" {
            fn free(ptr: *mut core::ffi::c_void);
        }
        const N: usize = 64;
        let src: Vec<f32> = (0..N).map(|i| i as f32).collect();
        let bytes = f32s_to_le(&src);
        let in_ptrs = [bytes.as_ptr()];
        let strides = [4usize];
        let handle =
            unsafe { karac_runtime_gpu_upload_soa(1, in_ptrs.as_ptr(), strides.as_ptr(), N) };
        assert_ne!(handle, 0, "upload returned a null handle");
        let out = unsafe {
            karac_runtime_gpu_dispatch_resident(
                STENCIL_SHIFT_WGSL.as_ptr(),
                STENCIL_SHIFT_WGSL.len(),
                handle,
                0,
                std::ptr::null(),
                0,
            )
        };
        unsafe { karac_runtime_gpu_free_soa(handle) };
        assert_ne!(out, 0, "resident stencil dispatch returned a null handle");

        let field_group = [0usize];
        let field_src = [0usize];
        let field_dst = [0usize];
        let aos = unsafe {
            karac_runtime_gpu_download_soa(
                out,
                1,
                field_group.as_ptr(),
                field_src.as_ptr(),
                field_dst.as_ptr(),
                4,
                4,
                N,
            )
        };
        assert!(!aos.is_null());
        let got = unsafe { le_to_f32s(std::slice::from_raw_parts(aos, N * 4)) };
        // out[0] = in[0] = 0; out[i] = in[i-1] = i-1 for i >= 1.
        assert_eq!(got[0], 0.0, "clamped left edge");
        for (i, &v) in got.iter().enumerate().skip(1) {
            assert_eq!(v, (i - 1) as f32, "neighbour shift at {i}");
        }
        unsafe { free(aos as *mut core::ffi::c_void) };
    }
}
