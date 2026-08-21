//! Accelerated / columnar data-layout state — SoA, GPU, tensors, columns.
//!
//! Seventh slice of the Phase-2 `Codegen` decomposition
//! ([`docs/spikes/state-decomposition-codegen-methodcall.md`]). Groups the
//! phase-10/11 surfaces that describe data *layout* rather than control
//! flow:
//!
//! - **SoA** — the struct-of-arrays layouts declared by `layout` blocks,
//!   the locals returned as SoA, and the per-layout drop functions;
//! - **GPU** — the WGSL source per `gpu.dispatch` site, and the buffer
//!   variables and their element struct names;
//! - **Tensor** — the tensor type per expression, the receiver type per
//!   index expression, the per-variable info, and the pending info for a
//!   `let` being bound;
//! - **Column / DataFrame** — the same shape for the Arrow-backed columnar
//!   types.
//!
//! The `pending_let_*` fields are a two-step handshake: the initializer's
//! compilation parks the info, and the `let` binding picks it up.
//!
//! Accessed as `self.accel.<name>` from the sibling `impl Codegen`
//! modules.

use std::collections::{HashMap, HashSet};

use inkwell::values::FunctionValue;

use super::state;
use super::state::SoaLayout;

/// SoA / GPU / tensor / column layout state.
pub(crate) struct Accel<'ctx> {
    /// SoA layout metadata (layout name → SoaLayout). **Origin-only** (slice 5):
    /// keyed by the `layout <name>` block's name, consulted to resolve a
    /// `LayoutId::Soa(<block>)` to its struct shape, to populate the layout
    /// catalogue (`collect_soa_layouts`), and — at a `let` binding *site* — to
    /// decide whether the binding's name matches a layout origin
    /// (`seed_binding_site_layout`). It is **never** the access-path trigger:
    /// a binding's physical layout is carried by `binding_layouts` /
    /// `layout_subst`, not re-derived from this map by name at each use.
    pub(crate) soa_layouts: HashMap<String, SoaLayout>,
    /// Names of the current function's *returned* local bindings — the
    /// tail-`return`ed bare `Vec[E]` identifiers (`soa_return_local_names`).
    /// A returned local's physical layout is dictated by the function's
    /// `return_layout` (the SoA-return monomorph seeds it in `layout_subst`),
    /// NOT by a coincidental name match against a `layout` block. So
    /// `seed_binding_site_layout`'s origin name-match fallback is suppressed for
    /// any binding in this set: in the AoS base symbol (and forward-param-only
    /// monos, where `return_layout` is `Aos`) a returned local stays AoS,
    /// matching the AoS return type — without this, a builder whose returned
    /// local is named like a layout block (`init_grid`'s `grid`,
    /// `fan_collide`'s `coll`) lowered its body SoA while the base signature
    /// returned the AoS `{ptr,len,cap}` struct (LLVM return-type mismatch). A
    /// terminal (non-returned) local is unaffected, so single-function SoA
    /// (`main`'s `entities`) still seeds by name. Set at each function entry
    /// (base + every mono) and save/restored around the mono entry points.
    pub(crate) soa_return_locals: std::collections::HashSet<String>,
    /// One per-element heap-field drop fn per SoA `layout` block that has at
    /// least one String/Vec-bearing group, keyed by the layout name. The fn
    /// takes `*mut SoaStruct` and, for every live element `[0, len)`, frees
    /// each heap group's String/Vec field buffers (cap-guarded, recursing
    /// nested tuples/structs) — the SoA analog of `struct_drop_fns`, called
    /// from the `FreeSoaGroups` cleanup arm and the reassignment inline-free
    /// before the group buffers themselves are released. A fully-POD layout
    /// gets no entry (`emit_soa_drop_fn` returns `None`) and emits exactly the
    /// pre-heap-field cleanup IR (the Slipstream native-oracle invariant).
    pub(crate) soa_drop_fns: HashMap<String, FunctionValue<'ctx>>,
    /// `gpu.dispatch` kernel-arg span -> generated WGSL shader text, from
    /// `Program.gpu_dispatch_wgsl` (spike slice-0c). `compile_method_call`
    /// bakes the shader as a constant and calls `karac_runtime_gpu_f32_map`.
    pub(crate) gpu_dispatch_wgsl: HashMap<(usize, usize), String>,
    /// Buffer-argument spans of `gpu.<reduce>` calls over an INTEGER element
    /// type, mapped to its spelling (`"i32"` / `"u32"`) — the plain-data hint
    /// that selects the CHECKED integer runtime entry point (which traps on
    /// overflow) over the float one, and decides whether the 32-bit result
    /// zero- or sign-extends into the i64 carrier. Codegen cannot re-derive
    /// it: a `Vec`'s data pointer is opaque at the LLVM level. See
    /// `ast::GpuReduceIntElemsTable`.
    pub(crate) gpu_reduce_int_elems: HashMap<(usize, usize), String>,
    /// Per-expression Tensor type info (element TypeExpr + static dims),
    /// keyed by `(span.offset, span.length)`. Populated from
    /// `Program.tensor_typed_exprs` (lowering pass, from
    /// `TypeCheckResult.expr_types`). Consumed at `Tensor.from(...)`
    /// construction sites, unannotated tensor let-bindings, and indexing
    /// dispatch. See `src/codegen/tensor.rs` for the value layout this
    /// drives.
    pub(crate) tensor_typed_exprs: HashMap<(usize, usize), crate::ast::TensorTypeInfo>,
    /// B-2026-08-14-17 — the `TensorTypeInfo` of an `Index` RECEIVER, keyed by
    /// the receiver's span (`Program.tensor_index_recv_types`). Shares its key
    /// with `tensor_typed_exprs` at every tensor index — the parser stamps a
    /// postfix expression with its receiver's span — where that table describes
    /// the index's scalar RESULT. `compile_index` installs this entry for the
    /// duration of compiling the receiver so a tensor-valued rvalue
    /// (`(t * 2)[0]`) routes through the tensor lowering instead of being
    /// compiled as scalar arithmetic on two pointers.
    pub(crate) tensor_index_recv_types: HashMap<(usize, usize), crate::ast::TensorTypeInfo>,
    /// Per-binding Tensor registration: element LLVM type + static dims
    /// (`Some(n)` = concrete literal usable for stride folding /
    /// bounds-check elision; `None` = read the dim from the value's
    /// runtime header). Populated by `register_var_from_type_expr`'s
    /// Tensor arm (annotations, params, for-bindings) and the let-path
    /// side-table fallback for unannotated bindings. Consulted by
    /// `compile_index` / `compile_index_store` / method dispatch.
    pub(crate) tensor_var_infos: HashMap<String, state::TensorVarInfo<'ctx>>,
    /// Expected-type threading for `Tensor.zeros` / `ones` / `full` —
    /// these constructors can't recover the element type or rank from
    /// their `dims: Vec[i64]` argument, so the let-binding path stashes
    /// the destination binding's registered `TensorVarInfo` here before
    /// compiling the RHS (the exact `pending_let_elem_type` mechanism
    /// `Vec.with_capacity` uses). `Tensor.from` never needs it (dims and
    /// element type both come from the literal).
    pub(crate) pending_let_tensor_info: Option<state::TensorVarInfo<'ctx>>,
    /// Per-expression Column element type, keyed by `(span.offset,
    /// span.length)`. Populated from `Program.column_typed_exprs`
    /// (lowering pass). Consumed at unannotated column let-bindings
    /// (column-returning calls) so the binding registers its element
    /// type. See `src/codegen/column.rs` for the value layout.
    pub(crate) column_typed_exprs: HashMap<(usize, usize), crate::ast::ColumnTypeInfo>,
    /// Per-binding Column registration: element LLVM type (+ unsigned
    /// flag). Populated by `register_var_from_type_expr`'s Column arm
    /// (annotations, params) and the let-path side-table fallback for
    /// unannotated bindings. Consulted by `compile_index` (`c[i] ->
    /// Option[T]`) and method dispatch (`push` / `len` / …).
    pub(crate) column_var_infos: HashMap<String, state::ColumnVarInfo<'ctx>>,
    /// Expected-type threading for `Column.new` / `with_capacity` /
    /// `from_vec` / `from_iter_nullable` — `new`/`with_capacity` carry no
    /// element value in their args, so the let-binding path stashes the
    /// destination binding's registered `ColumnVarInfo` here before
    /// compiling the RHS (the `pending_let_tensor_info` mechanism).
    pub(crate) pending_let_column_info: Option<state::ColumnVarInfo<'ctx>>,
    /// Set of binding names known to be `DataFrame`s (non-generic, so no
    /// per-binding type info — just membership). Populated by
    /// `register_var_from_type_expr`'s DataFrame arm; gates
    /// `try_compile_dataframe_method` and the `FreeDataFrame` tracker.
    pub(crate) dataframe_var_infos: std::collections::HashSet<String>,
    /// LLVM struct type for `ProviderLookupResult { data, vtable }` —
    /// matches the runtime's `#[repr(C)]` shape. Used once at codegen
    /// init to type the `karac_provider_lookup` extern's return; after
    /// that the call's return type carries the shape implicitly so
    /// extractvalue at sub-step 4 dispatch sites doesn't need to look
    /// it up here. Field retained as ABI documentation for future
    /// readers and as the canonical anchor if `ProviderLookupResult`'s
    /// shape ever changes.
    #[allow(dead_code)]
    // ── Map runtime ───────────────────────────────────────────────
    /// GPU-SLIP-4h: per-`GpuBuffer` binding → element struct name, recorded
    /// when `let buf = gpu.upload(vec)` / a resident `gpu.dispatch` binds a
    /// handle. `gpu.download` into a PLAIN (un-layouted) `Vec[S]` target
    /// needs `S` to synthesize the default interleaved manifest, and the
    /// `{handle, n}` value itself is type-erased.
    /// GPU-SLIP-4b-3: `gpu.<reduce>(buf.field)` arg span → `(struct, field)`.
    /// Its PRESENCE is what tells `compile_gpu_reduce` the argument is a
    /// resident device buffer's field rather than a host `Vec`, so the two
    /// lowerings never have to guess from LLVM types (a `GpuBuffer`'s
    /// `{i64, i64}` is structurally ambiguous — see `gpu_buffer_var_names`).
    pub(crate) gpu_resident_field: HashMap<(usize, usize), (String, String, bool)>,
    pub(crate) gpu_buffer_elem_structs: HashMap<String, String>,
    /// Names of variables that are actually a `GpuBuffer` (`{handle, n}` value,
    /// bound by `let buf = gpu.upload(...)` / a resident `gpu.dispatch`). The
    /// gpu-buffer LLVM type is an anonymous `{i64, i64}`, which is STRUCTURALLY
    /// identical to any 2-field all-`i64` user struct (`struct P { x: i64,
    /// y: i64 }`), so the gpu-buffer reassign / method arms must NOT key on
    /// `vs.ty == gpu_buffer_type()` alone — a plain `P` reassign otherwise routes
    /// old-value cleanup through `karac_runtime_gpu_free_soa` and pulls in the
    /// opt-in GPU archive, breaking `karac run`/`build` for a non-GPU program
    /// (B-2026-07-18-7). This is the authoritative membership test. Cleared
    /// per-function.
    pub(crate) gpu_buffer_vars: HashSet<String>,
}
