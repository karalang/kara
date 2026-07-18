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

fn gpu_context() -> Option<&'static GpuContext> {
    static CTX: OnceLock<Option<GpuContext>> = OnceLock::new();
    CTX.get_or_init(|| {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
                .ok()?;
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
    // Scalar uniforms (GPU-LBM-2): each `uniform_size` bytes (f32 = 4).
    let uniform_slice = std::slice::from_raw_parts(uniform_ptrs, n_uniforms);
    let uniforms: Vec<&[u8]> = uniform_slice
        .iter()
        .map(|&p| std::slice::from_raw_parts(p, uniform_size))
        .collect();

    let Some(outputs) = pollster::block_on(dispatch_multi_bytes_async(wgsl, &inputs, &uniforms, n))
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
fn run_compute(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    wgsl: &str,
    input_bufs: &[wgpu::Buffer],
    sizes: &[u64],
    uniforms: &[&[u8]],
    elem_count: usize,
) -> Vec<wgpu::Buffer> {
    let n_buffers = input_bufs.len();
    // GPU-SLIP-4 buffer pooling: reuse a freed grid's output buffers (from the
    // pool) rather than allocating fresh ones each dispatch — the per-substep
    // output allocation was the dominant per-dispatch CPU cost once the transfer
    // was gone (4c re-bench). A miss creates a new buffer.
    let output_bufs: Vec<wgpu::Buffer> = sizes
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
    // Inputs at binding 0..n, outputs at n..2n, uniforms at 2n..2n+u.
    let mut entries: Vec<wgpu::BindGroupEntry> =
        Vec::with_capacity(n_buffers * 2 + uniform_bufs.len());
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
            binding: (2 * n_buffers + i) as u32,
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
    let uniform_slice = std::slice::from_raw_parts(uniform_ptrs, n_uniforms);
    let uniforms: Vec<&[u8]> = uniform_slice
        .iter()
        .map(|&p| std::slice::from_raw_parts(p, uniform_size))
        .collect();

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
