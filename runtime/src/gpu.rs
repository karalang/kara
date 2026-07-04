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

use wgpu::util::DeviceExt;

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

/// Byte-oriented GPU element-wise map core. `input` is the raw element bytes
/// (`n * elem_size`); the returned buffer is the same length. The WGSL shader
/// supplies the element interpretation via its `array<T>` binding declarations,
/// so this stays type-agnostic. `elem_size` sets the per-element stride used to
/// derive the invocation count.
async fn dispatch_bytes_async(wgsl: &str, input: &[u8], elem_size: usize) -> Option<Vec<u8>> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions::default())
        .await
        .ok()?;
    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor::default())
        .await
        .ok()?;

    let byte_len = input.len() as u64;
    let elem_count = input.len() / elem_size;

    let input_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("gpu-slice0a-input"),
        contents: input,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
    });
    let output_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("gpu-slice0a-output"),
        size: byte_len,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let staging_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("gpu-slice0a-staging"),
        size: byte_len,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("gpu-slice0a-shader"),
        source: wgpu::ShaderSource::Wgsl(wgsl.into()),
    });

    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("gpu-slice0a-pipeline"),
        layout: None,
        module: &module,
        entry_point: Some("main"),
        compilation_options: Default::default(),
        cache: None,
    });

    let bind_group_layout = pipeline.get_bind_group_layout(0);
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("gpu-slice0a-bind-group"),
        layout: &bind_group_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: input_buf.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: output_buf.as_entire_binding(),
            },
        ],
    });

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("gpu-slice0a-encoder"),
    });
    {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("gpu-slice0a-pass"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        // One invocation per element; @workgroup_size(64) in the shader.
        pass.dispatch_workgroups((elem_count as u32).div_ceil(64), 1, 1);
    }
    encoder.copy_buffer_to_buffer(&output_buf, 0, &staging_buf, 0, byte_len);
    queue.submit(Some(encoder.finish()));

    // Map the staging buffer and block until the GPU work + copy complete.
    let slice = staging_buf.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |res| {
        let _ = tx.send(res);
    });
    device.poll(wgpu::PollType::wait_indefinitely()).ok()?;
    rx.recv().ok()?.ok()?;

    let mapped = slice.get_mapped_range();
    let out: Vec<u8> = mapped.to_vec();
    drop(mapped);
    staging_buf.unmap();
    Some(out)
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
}
