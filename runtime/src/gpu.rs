//! GPU compute spine — phase-10 GPU codegen, spike **slice-0a**
//! ([`docs/spikes/gpu-wgsl-slice0.md`]).
//!
//! Proves the wgpu plumbing end-to-end: a WGSL compute shader applied
//! element-wise to an `f32` buffer, dispatched on the platform's native GPU
//! API (Metal on macOS, Vulkan/DX12 elsewhere) and read back. **No codegen
//! yet** — the WGSL is supplied by the caller; the codegen that generates it
//! (slice-0b) and the `karac_runtime_gpu_*` C symbol that exposes this to
//! compiled Kāra (slice-0c) are later increments. Behind the opt-in `gpu`
//! feature; not compiled into any production or wasm archive.

use wgpu::util::DeviceExt;

/// Run `wgsl` over `input` element-wise and return the result buffer.
///
/// The shader must declare `@compute @workgroup_size(64) fn main(...)` with
/// binding 0 = `var<storage, read> input: array<f32>` and binding 1 =
/// `var<storage, read_write> output: array<f32>` in `@group(0)`.
///
/// Returns `None` when no GPU adapter is available (headless CI, no driver,
/// `KARAC_GPU_BACKEND` unset on a GPU-less box) — the caller treats that as a
/// graceful skip rather than a hard failure. This is a spike helper, not yet
/// reachable from compiled Kāra, so it has no non-test caller until slice-0c.
#[allow(dead_code)]
pub fn dispatch_f32_map(wgsl: &str, input: &[f32]) -> Option<Vec<f32>> {
    pollster::block_on(dispatch_f32_map_async(wgsl, input))
}

async fn dispatch_f32_map_async(wgsl: &str, input: &[f32]) -> Option<Vec<f32>> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions::default())
        .await
        .ok()?;
    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor::default())
        .await
        .ok()?;

    let n = input.len();
    let byte_len = std::mem::size_of_val(input) as u64;

    // `&[f32]` → `&[u8]` (little-endian) without pulling in `bytemuck`.
    let mut bytes: Vec<u8> = Vec::with_capacity(byte_len as usize);
    for &x in input {
        bytes.extend_from_slice(&x.to_le_bytes());
    }

    let input_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("gpu-slice0a-input"),
        contents: &bytes,
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
        pass.dispatch_workgroups((n as u32).div_ceil(64), 1, 1);
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
    let out: Vec<f32> = mapped
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect();
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
