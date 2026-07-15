# Spike: GPU-SLIP-4b — persistent on-device (resident) buffers

**Goal.** Keep the LBM grid resident on the GPU across substeps so an iterative
sim stops paying the host↔device round-trip every step (the 218 ms 4a baseline
is ~all transfer, not compute). Owner-decided design (2026-07-11): **explicit
device-buffer handles riding ownership, `move` semantics** — the cliff-cost
host↔device boundary becomes a type-system invariant, not a suggestion.

## Surface (owner-decided)

```kara
let mut grid = gpu.upload(init_grid());        // moves Vec[S] -> GpuBuffer[S]
loop {
    let coll = gpu.dispatch(collide, ref grid, om);   // borrows grid, new buffer
    let next = gpu.dispatch(stream, ref coll, s);     // borrows coll, new buffer
    grid = next;                                       // old grid freed; grid = next
}
let field = gpu.download(grid);                // moves handle back -> Vec[S]
```

- `gpu.upload(vec: Vec[S]) -> GpuBuffer[S]` — **moves** the `Vec` to the device.
  The host binding is moved-out (you physically cannot read stale host data).
- `gpu.dispatch(kernel, ref buf, uniforms…) -> GpuBuffer[S]` — **borrows** the
  input handle (`ref`), returns a fresh owned output handle (device→device, no
  round-trip). This is the resident overload of the existing `gpu.dispatch`.
- `gpu.download(buf: GpuBuffer[S]) -> Vec[S]` — **moves/consumes** the handle.
- A `GpuBuffer` that leaves scope without being downloaded → `free_soa`.

The move checker then gives the double-buffer ping-pong + device-buffer freeing
for free (no new lifetime machinery).

## Runtime (4b-1 — DONE, landed `ab907c37`)

Four `#[no_mangle]` C symbols in `runtime/src/gpu.rs` + an opaque `u64` handle
registry (`ResidentSoa`): `karac_runtime_gpu_upload_soa(n_groups, in_ptrs,
group_strides, n) -> u64`, `karac_runtime_gpu_dispatch_resident(wgsl, in_handle,
n_uniforms, uniform_ptrs, uniform_size) -> u64`, `karac_runtime_gpu_download_soa(
handle, n_fields, field_group, field_src, field_dst, field_size, aos_stride, n)
-> *mut u8`, and `karac_runtime_gpu_free_soa(handle)`. 4b-2 is the compiler
surface that emits calls to these.

## Type representation decision

`GpuBuffer[S]` is a **typechecker-synthesized** `Type::Named { name: "GpuBuffer",
args: [S] }` — the same way `infer_gpu_dispatch` already fabricates `Vec[S]`
(`expr_method_call.rs:6885`). NOT a stdlib `.kara` struct, because:

- `S` must ride the type so `download(buf)` knows which `Vec[S]` to produce; a
  synthesized `Named{args:[S]}` carries it with no phantom-generic question.
- Ownership needs nothing: `is_copy_type`'s `Named` arm (`ownership.rs:861`)
  returns Copy only if `struct_info[name]` derives `Copy`; an unknown name →
  non-Copy → every use is a **move**, and magic-module method args **default to
  consume** (`ownership/expr_check.rs:657`, `borrow.rs:108` returns `false` with
  no resolved mode). So `upload(v)` moves `v` and `download(buf)` moves `buf`
  with **zero `ownership.rs` changes**. (Only `dispatch(kernel, ref buf)` in
  4b-2b needs a borrow override for arg 1.)

Codegen lowers `GpuBuffer[S]` to a plain **`i64`** (the handle). The writable
`gpu.Buffer[S]` *type annotation* (a qualified type path) is a follow-on;
inference (`let grid = gpu.upload(…)`) covers the ping-pong without it.

## Codegen reuse map (from the existing SoA dispatch)

`compile_gpu_dispatch_soa` (`method_call.rs:13947`) is **upload + dispatch +
download fused**; 4b-2 factors it the same way 4b-1 factored the runtime:

- **upload**: reuse the group-pointer GEP walk + `in_ptrs` array
  (`method_call.rs:14019–14065`) and `group_strides`
  (`14073–14077`) from `active_soa_layout(vec_name)` → call a new
  `gpu_upload_soa_fn` (declare near `runtime.rs:7532`) → the returned `i64` is
  the `GpuBuffer` value. Register the binding for scope-exit free.
- **download**: reuse the field-scatter descriptors
  (`field_group/field_src/field_dst/field_size/aos_stride/n`,
  `method_call.rs:14078–14096`, built with `build_i64_stack_array`
  `13915`) derived from the receiving binding's `SoaLayout` → call a new
  `gpu_download_soa_fn` → wrap the AoS `Vec[S]` (`14162`) → if the LHS is a SoA
  `layout` binding, scatter via `compile_soa_let_from_gpu_dispatch` /
  `soa_scatter_aos_into` (`exprs.rs:2606/2721`), the exact GPU-SLIP-3 path.

`SoaLayout` (`codegen/state.rs:299`) + `active_soa_layout(name)`
(`mono.rs:2494`) give the groups (`SoaGroup.field_indices` drive `field_dst =
idx*4`). Upload/download need **no shader** (pure transfer) — skip
`emit_kernel_soa`.

## Ownership / drop wiring (the load-bearing piece)

`GpuBuffer` is an `i64` in a slot. Mirror the **File / Channel** owned-handle
pattern (NOT `TaskHandle`, which registers no free):

- Add `CleanupAction::FreeGpuBuffer { buf_alloca }` (`codegen/state.rs`, near
  `FreeFileHandle` `:657`).
- `track_gpu_buffer_var(&mut self, buf_alloca)` (mirror `track_file_var`
  `runtime.rs:3443`) pushes it onto the current `scope_cleanup_actions` frame,
  called at the `let`-binding site when the surface type is `GpuBuffer`
  (`stmts.rs`, beside the File/Channel/Map registration blocks).
- Drain arm in `emit_cleanup_action_at` (`runtime.rs:6154` region, the
  `FreeFileHandle` template): `handle = load i64 buf_alloca; if handle != 0 {
  karac_runtime_gpu_free_soa(handle) }`.
- **Move-suppression = zero-sentinel** (the `HttpHandleFree` analog): on a move
  (`download(buf)` consuming, reassignment drop-old, pass-by-value / tail
  return), store `0` into the source slot so the `handle != 0` guard skips the
  free. Wire into `suppress_cleanup_for_tail_return` (`call_dispatch.rs:2994`)
  and the download/reassign sites. (Simpler than the Channel/Map queue-removal
  because the i64 slot carries its own sentinel.)

The reassignment `grid = next` frees the OLD grid: the SoA-assign path already
frees displaced group buffers; here the assign target is a `GpuBuffer` i64, so
the drop-old is `free_soa(old_handle)` before storing the new handle.

## Sub-slices

- **4b-2a** — `GpuBuffer[S]` type + `gpu.upload` / `gpu.download` + ownership
  move/drop. The minimal leak-free round-trip (`upload` then `download`,
  byte-identical to the input; an un-downloaded buffer freed on scope exit). The
  ownership/drop wiring is the load-bearing correctness piece; validate on Metal
  + a leak check (`leaks --atExit` locally / Linux-LSan in CI).
- **4b-2b** — handle-overloaded `gpu.dispatch(kernel, ref buf, uniforms…) ->
  GpuBuffer[S]` (emit `dispatch_resident`), the `ref`-borrow override for arg 1
  in `ownership.rs`, and the double-buffer ping-pong reassignment (drop-old).

## 4b-3 (after 4b-2)

Validate the full resident sim on Metal + re-bench vs CPU 17.5 ms and the 218 ms
4a baseline. Batching all N substeps into one command buffer is a **4c** follow-on.
