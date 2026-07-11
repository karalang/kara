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
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()?;
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
    let output_bufs: Vec<wgpu::Buffer> = inputs
        .iter()
        .map(|bytes| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("gpu-cg4-output"),
                size: bytes.len() as u64,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            })
        })
        .collect();
    let staging_bufs: Vec<wgpu::Buffer> = inputs
        .iter()
        .map(|bytes| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("gpu-cg4-staging"),
                size: bytes.len() as u64,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        })
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

    // Cached compiled pipeline (GPU-SLIP-4a) — compiled once per distinct shader,
    // not once per dispatch.
    let pipeline = compute_pipeline(device, wgsl);

    let bind_group_layout = pipeline.get_bind_group_layout(0);
    // Inputs at binding 0..n, outputs at binding n..2n (the emitter's convention).
    let mut entries: Vec<wgpu::BindGroupEntry> = Vec::with_capacity(n_buffers * 2);
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
    // Uniforms at binding 2n..2n+u (after all group in/out buffers).
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
        pass.dispatch_workgroups((elem_count as u32).div_ceil(64), 1, 1);
    }
    for ((out_buf, staging), bytes) in output_bufs
        .iter()
        .zip(staging_bufs.iter())
        .zip(inputs.iter())
    {
        encoder.copy_buffer_to_buffer(out_buf, 0, staging, 0, bytes.len() as u64);
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

    let mut outs = Vec::with_capacity(n_buffers);
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
    let i = gid.x;
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
    let i = gid.x;
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
    let i = gid.x;
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
    let i = gid.x;
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
}
