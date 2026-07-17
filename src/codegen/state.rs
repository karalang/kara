//! Internal state types used by `Codegen` during compilation.
//!
//! Houses the carrier types threaded through the `Codegen` impl
//! methods: variable slots, slice-pattern descriptors, shared-struct
//! layout info, enum layout / drop-kind classifiers, SOA layout,
//! cleanup-action records, return slots, set-op filters, loop frames,
//! asserted-bound facts (bounds-check elision), spawn-site records,
//! and map-monomorphization method handles.
//!
//! All `pub(super)` so the `Codegen` impl methods in sibling modules
//! can read and mutate them; the types themselves are not part of
//! codegen's external API.

use std::collections::HashMap;

use inkwell::basic_block::BasicBlock;
use inkwell::types::{BasicTypeEnum, StructType};
use inkwell::values::{FunctionValue, IntValue, PointerValue};

use crate::ast::Block;

// ── Variable slot: pointer + LLVM type for typed loads ─────────

#[derive(Clone, Copy)]
pub(crate) struct VarSlot<'ctx> {
    pub(crate) ptr: PointerValue<'ctx>,
    pub(crate) ty: BasicTypeEnum<'ctx>,
}

/// The concrete handle-backed builtin a generic call site binds to a
/// bare type-param param (`fn report[C: Reduce[i64]](c: ref C)` called
/// with a `Column[i64]` / `Tensor[i64, [4]]` argument). Plain AST data
/// (lowering side-table records), NOT LLVM types — the codegen
/// containment invariant holds. Collected per call site in
/// `compile_generic_call` from `column_typed_exprs` /
/// `tensor_typed_exprs` (span-keyed, borrow-unwrapped), keyed by the
/// mangled mono name in `mono_handle_param_infos`, and consumed by
/// `compile_mono_function`'s prologue to (re)register the param in
/// `column_var_infos` / `tensor_var_infos` — the LLVM value type of
/// such an argument is an opaque `ptr`, so `infer_type_args` alone can
/// neither distinguish Column from Tensor nor recover the element type
/// (S6a: two same-shaped instantiations previously shared one mono and
/// the second one miscompiled).
#[derive(Clone)]
pub(crate) enum MonoHandleArgInfo {
    Column(crate::ast::ColumnTypeInfo),
    Tensor(crate::ast::TensorTypeInfo),
}

/// Codegen-side view of a `Tensor[T, Shape]` binding (phase-11
/// numerical stdlib). The runtime value is a single pointer to one
/// malloc'd block laid out `[i64 rank][rank × i64 dims][C-order data]`
/// — see `src/codegen/tensor.rs` for the layout helpers. `elem` is the
/// element's LLVM type; `dims` mirrors `TensorTypeInfo.dims`:
/// `Some(n)` per statically-known dim (stride folding, bounds-check
/// elision), `None` per runtime dim (loaded from the header, which is
/// always authoritative).
#[derive(Clone)]
pub(crate) struct TensorVarInfo<'ctx> {
    pub(crate) elem: BasicTypeEnum<'ctx>,
    /// True iff the element type is an unsigned integer — the LLVM `IntType`
    /// can't carry this, but reductions (`min`/`max` compare, `sum`/`prod`
    /// overflow detection) need it. Set from the element `TypeExpr`.
    pub(crate) elem_unsigned: bool,
    pub(crate) dims: Vec<Option<i64>>,
}

/// Codegen-side view of a `Column[T]` binding (phase-11 data-science
/// stdlib, Arrow commitment Q5). The runtime value is a single pointer
/// to one malloc'd control block laid out
/// `{ ptr data, ptr null_bitmap, i64 len, i64 capacity }` (field order
/// per design.md § Column) — `data` is a contiguous buffer of
/// `capacity` elements, `null_bitmap` a bit-packed validity buffer of
/// `ceil(capacity/8)` bytes (bit `i`: 1 = valid, 0 = SQL null). See
/// `src/codegen/column.rs` for the layout helpers. `elem` is the
/// element's LLVM type; `elem_unsigned` mirrors `TensorVarInfo` (set
/// from the element `TypeExpr`) for the future 3VL-arithmetic slice's
/// signed/unsigned compare choice.
#[derive(Clone, Copy)]
pub(crate) struct ColumnVarInfo<'ctx> {
    pub(crate) elem: BasicTypeEnum<'ctx>,
    /// Reserved for the follow-on 3VL-arithmetic slice's signed/unsigned
    /// compare choice (mirrors `TensorVarInfo.elem_unsigned`); not read
    /// by the core slice's accessors / indexing.
    #[allow(dead_code)]
    pub(crate) elem_unsigned: bool,
}

/// Resolved view of a slice-pattern scrutinee (`Array[T, N]`, `Vec[T]`,
/// or `Slice[T]`) — `data_ptr` is normalized to a `T*` element pointer
/// and `len` is the runtime element count as i64. `mutable` mirrors the
/// source's mutability for `Slice` rest-binding header construction.
#[derive(Clone, Copy)]
pub(crate) struct SliceSource<'ctx> {
    pub(crate) data_ptr: PointerValue<'ctx>,
    pub(crate) len: IntValue<'ctx>,
    pub(crate) elem_ty: BasicTypeEnum<'ctx>,
    pub(crate) mutable: bool,
}

// ── Shared type (RC) layout ────────────────────────────────────

/// Metadata for a `shared struct` / `shared enum` (RC) or a `par struct` /
/// `par enum` (Arc) — both are heap-allocated reference-semantic types with an
/// identical `{ i64 refcount, … }` layout. The single distinction is whether
/// the refcount header is mutated atomically (`par`/Arc) or not (`shared`/Rc);
/// see `is_par` below.
/// Heap layout for structs: `{ i64 refcount, field0, field1, … }`
/// Heap layout for enums:   `{ i64 refcount, i64 tag, i64 word0, … }`
#[derive(Clone)]
pub(crate) struct SharedTypeInfo<'ctx> {
    /// The LLVM struct type for the heap object (includes refcount header).
    pub(crate) heap_type: StructType<'ctx>,
    /// Field names in declaration order (structs only; empty for enums).
    #[allow(dead_code)]
    pub(crate) field_names: Vec<String>,
    /// true if this is a shared enum (vs shared struct).
    pub(crate) is_enum: bool,
    /// `par struct` / `par enum` (always Arc) rather than `shared struct` /
    /// `shared enum` (Rc). When set, every refcount increment / decrement on
    /// this type's header is emitted as an `atomicrmw` (via `emit_arc_inc` /
    /// `emit_arc_dec`) instead of a plain load/add/store — the values cross
    /// task boundaries, so the count must be race-free. Every other codegen
    /// path (layout, niche, field access, method dispatch, construction, drop)
    /// is identical to the `shared` case, which is why `par` types register in
    /// this same `shared_types` map. See design.md § Part 5b "Always Arc".
    pub(crate) is_par: bool,
    /// Niche optimization for `Option[shared T]` fields. Indexed by user-field
    /// index (0-based; the heap-field index is `user_idx + 1` because index 0
    /// is the refcount). For each entry `Some(inner_name)`, the field at that
    /// index has source type `Option[<inner_name>]` where `<inner_name>` is a
    /// `shared struct`, and the heap stores a single `ptr` (null = `None`,
    /// non-null = `Some`) at this slot instead of the conventional 4-i64
    /// `{tag, w0, w1, w2}` Option layout. Saves 24 bytes per field. The inner
    /// struct's `heap_type` is resolved via `shared_types.get(inner_name)` at
    /// use time so self-referential shapes (`Node → Option[Node]`) work even
    /// when the field's type isn't yet registered at struct-declaration time.
    /// `None` entries mean conventional layout for that field.
    pub(crate) niche_option_fields: Vec<Option<String>>,
}

/// Phase-B2 build-side elision role for one cluster-local binding
/// (ownership `src/ownership/elision.rs` § Phase B2). Loaded into
/// `Codegen::elided_b2_bindings` for clusters whose `b2` flag is set.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum B2Role {
    /// The cluster root — keeps its B1 `FreeClusterWalk` cleanup; also
    /// a legal link-store target.
    Root,
    /// A fresh node consumed by the single link store: no count ops,
    /// no cleanup (the chain owns it; the root walk frees it).
    Fresh,
    /// Bare `T` cursor: non-owning alias — no receive-inc, no cleanup,
    /// plain-store reassignment.
    BareCursor,
    /// `Option[T]` link-read cursor: non-owning — no alias-acquire inc,
    /// no `RcDecOption`, plain-store reassignment.
    OptionCursor,
}

/// Per-binding B2 record (denormalized cluster info so each codegen
/// site needs a single lookup).
#[derive(Clone)]
pub(crate) struct B2Binding {
    pub(crate) role: B2Role,
    pub(crate) member_type: String,
    pub(crate) link_field_index: usize,
}

/// Call-ABI niche record for one function (the fn-signature analog of
/// `niche_option_fields` above — see `Codegen::fn_niche_abi`). `ret`
/// means the function's LLVM return type is a single nullable `ptr`
/// standing in for `Option[shared T]`; `params[i]` means parameter `i`
/// is likewise `ptr`-shaped at the ABI. The body-side shape stays the
/// conventional 4-i64 Option struct — packing/unpacking happens only at
/// the call boundary (`declare_function` / `compile_function` entry /
/// the return sites / `compile_call`).
#[derive(Clone)]
pub(crate) struct NicheAbi {
    pub(crate) ret: bool,
    pub(crate) params: Vec<bool>,
}

// ── Enum variant layout ─────────────────────────────────────────

/// Per-payload-field drop classification recorded at `declare_enums`
/// time (Phase 7.2 Slice DP — Compound-payload enum follow-up: drop-path
/// implementation, 2026-05-09). Drives `emit_enum_drop_switch`'s per-
/// variant cleanup-BB body: for each heap-bearing field the drop function
/// emits the matching cleanup (the `cap > 0 ? free(data)` shape that
/// `CleanupAction::FreeVecBuffer` uses for top-level Vec/String bindings,
/// or a recursive call to a nested struct/enum drop fn).
///
/// The drop classification is kept **symmetric** with the
/// deep-copy-on-entry (`param_own.rs`) and the move-suppression
/// (`control_flow_match.rs` / `zero_enum_payload_caps`): each must account
/// for exactly the same heap, or a by-value param copy / a pattern-move-out
/// double-frees or leaks. See [`Self::is_heap_bearing`].
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum EnumDropKind {
    /// No cleanup — primitive, slice (no ownership), or RC-pointer (handled
    /// by the shared-type RC machinery).
    None,
    /// Three-word `String` / `Vec[T]` payload — payload words at
    /// `(start, start+1, start+2)` are `(data, len, cap)`. Free with
    /// `karac_runtime_free(data)` when `cap > 0`. For a `Vec[heap-element]`
    /// only the OUTER buffer is freed — per-element heap is a bounded leak,
    /// symmetric with the outer-buffer-only deep-copy-on-entry
    /// (B-2026-06-13-13 part 2 / `param_own.rs` "bounded leak on both sides,
    /// never corruption"; a deliberate v1 design position).
    VecOrString,
    /// B-2026-06-13-13: payload field is a named non-shared user **struct**
    /// laid out inline in the variant's payload words. Dropped by calling that
    /// struct's `__karac_drop_struct_<S>` on the field's word-region pointer
    /// (`emit_enum_drop_switch`), mirroring the struct-drop `NestedStruct`
    /// (#18) arm. The type name is recovered at emit time from
    /// `enum_variant_field_type_exprs`. The struct's own drop fn transitively
    /// frees its Vec/String, enum, and Map fields, so this one kind covers an
    /// arbitrarily-nested struct payload (e.g. the lexer's
    /// `CStringLiteral(CStr { bytes: Vec[u8], … })`). A *directly*-inline
    /// nested enum payload (`enum Outer { V(Inner) }`) is a rarer remaining
    /// gap — its classification needs an all-enum-names pre-pass (sibling
    /// enums aren't in `enum_layouts` yet at `declare_enums` time) — tracked
    /// separately, not in this slice.
    NestedStruct,
}

impl EnumDropKind {
    /// Whether this kind owns heap the variant cleanup must free — and that
    /// the deep-copy-on-entry must duplicate and the move-suppression must
    /// neutralize. Every kind except `None` is heap-bearing.
    pub(crate) fn is_heap_bearing(self) -> bool {
        self != EnumDropKind::None
    }
}

/// Tracks how an enum is laid out in LLVM IR as a tagged union.
/// Representation: `{ i64 tag, i64 word_0, ..., i64 word_N }`.
/// All payload words are stored as i64 (signed-extended / reinterpreted).
#[derive(Clone)]
pub(crate) struct EnumLayout<'ctx> {
    /// The LLVM struct type for all instances of this enum.
    pub(crate) llvm_type: StructType<'ctx>,
    /// variant name → discriminant tag (0, 1, 2, …)
    pub(crate) tags: HashMap<String, u64>,
    /// variant name → number of source-position payload fields. Preserved
    /// verbatim from `VariantKind::Tuple(tys).len()` / `Struct(fields).len()`
    /// so existing pattern-binding code that counts source fields keeps
    /// working unchanged.
    pub(crate) field_counts: HashMap<String, usize>,
    /// Compound-payload enum codegen (Phase 7.2 Slice CP) — per-variant
    /// per-field word range in the unified payload area. Each variant's
    /// vec entry has one `(start_word, num_words)` pair per source field
    /// (in declaration order). The variant's total payload-word count is
    /// the last field's `start + num_words`; the enum-wide payload-area
    /// width is `max_payload_words = max(variant_totals)`. Used by
    /// construction (`try_compile_enum_variant`) to write per-field word
    /// streams instead of single-i64-coerced collapse, and by
    /// destructure (`bind_pattern_values` `TupleVariant` arm) to read
    /// each field's word range and reconstruct the original aggregate.
    pub(crate) field_word_offsets: HashMap<String, Vec<(usize, usize)>>,
    /// Phase 7.2 Slice DP — drop-path classification per source field.
    /// Same shape as `field_word_offsets`: variant name → vec of
    /// per-field `EnumDropKind` (declaration order). Read by
    /// `emit_enum_drop_switch` to decide which payload-word ranges
    /// require destructor invocations at scope exit. `None` for every
    /// field of a variant means the variant's cleanup BB short-circuits
    /// to `ret void` without emitting any work.
    pub(crate) field_drop_kinds: HashMap<String, Vec<EnumDropKind>>,
    /// Whether this enum is a `shared enum` (RC heap-allocated). When
    /// true, the layout's value-type drop machinery is dormant — RC
    /// inc/dec via `track_rc_var` handles cleanup through refcount
    /// semantics. The DP slice's `track_enum_var` registration site
    /// guards on `!is_shared` per design lock DP3.
    pub(crate) is_shared: bool,
}

// ── SoA layout metadata ─────────────────────────────────────────

/// Metadata for a single group in a SoA layout.
#[derive(Clone, Debug)]
pub(crate) struct SoaGroup {
    #[allow(dead_code)]
    pub(crate) name: String,
    #[allow(dead_code)]
    pub(crate) fields: Vec<String>,
    /// Index of each field in the original struct's field list.
    pub(crate) field_indices: Vec<usize>,
    #[allow(dead_code)]
    pub(crate) elem_type: Option<StructType<'static>>,
    /// Optional `align(N)` — N is a power-of-two byte alignment for the group's backing array.
    pub(crate) align: Option<u32>,
    #[allow(dead_code)]
    pub(crate) is_cold: bool,
}

/// Full SoA layout for a named collection.
#[derive(Clone, Debug)]
pub(crate) struct SoaLayout {
    #[allow(dead_code)]
    pub(crate) name: String,
    /// Element struct name (e.g., "Entity").
    pub(crate) struct_name: String,
    /// Hot groups in declaration order (excludes the cold group).
    pub(crate) groups: Vec<SoaGroup>,
    /// Optional cold group (separate allocation, appended after all hot group pointers).
    pub(crate) cold_group: Option<SoaGroup>,
    /// Number of hot groups (including implicit trailing group for unassigned fields).
    /// Does NOT include the cold group — the cold pointer is always last in the struct.
    pub(crate) num_groups: usize,
}

// ── Per-layout monomorphization axis ────────────────────────────

/// Names the concrete physical layout of a `Vec[E]` / `Array[E, N]` binding at
/// a codegen monomorph: either the default array-of-structs (`Aos`) or a
/// struct-of-arrays origin keyed by the `layout <name>` block that declared it.
///
/// This is the value carrier for the per-layout-monomorphization axis
/// (`docs/spikes/per-layout-monomorphization.md`). Distinct `layout` blocks —
/// even structurally identical, even over the same element struct — get
/// distinct `Soa(name)` values, so two groupings of one element type produce
/// distinct monomorphs (the "distinct monomorphs per grouping" invariant,
/// design.md:5426).
///
/// Forward inference (`compute_call_layout_subst`, slice 2) reads each
/// bare-binding argument's binding-site layout; an all-`Aos` call adds no
/// mangle suffix, so non-SoA code is byte-identical to the name-keyed model.
/// Slice 3 adds backward return-layout inference.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum LayoutId {
    /// Default array-of-structs: a `Vec[E]` is `{ ptr, len, cap }`.
    Aos,
    /// Struct-of-arrays, keyed by the originating `layout <name>` block (the
    /// key into `soa_layouts`).
    Soa(String),
}

impl LayoutId {
    /// Mangling fragment for this layout, or `None` for the default `Aos`
    /// (which contributes no suffix — so an all-`Aos` monomorph keeps the
    /// existing symbol, and non-SoA code emits zero new symbols).
    pub(crate) fn mangle_suffix(&self) -> Option<String> {
        match self {
            LayoutId::Aos => None,
            LayoutId::Soa(name) => Some(format!("soa_{name}")),
        }
    }
}

// ── Scope cleanup action ────────────────────────────────────────

/// Per-element drop classification for a `Vec[Map[K,V]]` / `Vec[Set[T]]`
/// whose elements are opaque map handles. Carries the same K/V flags as
/// [`CleanupAction::FreeMapHandle`] so the `FreeVecBuffer` drop loop can
/// free each element handle exactly as a standalone Map binding would
/// (`emit_free_one_map_handle`). A Map handle is a bare `ptr`, so the
/// element LLVM type alone can't signal "this is a map" — this struct
/// (set only when the element TypeExpr is `Map`/`Set`) carries the intent,
/// mirroring `FreeVecBuffer::elem_is_tensor`.
#[derive(Clone)]
pub(crate) struct MapElemDrop<'ctx> {
    /// KEY half follows the Vec/String `{ptr,len,cap}` layout.
    pub(crate) key_is_vec: bool,
    /// VALUE half follows the Vec/String layout (always `false` for Set).
    pub(crate) val_is_vec: bool,
    /// Heap-struct type for a shared-struct/enum VALUE (per-bucket rc_dec).
    pub(crate) val_shared_heap_type: Option<StructType<'ctx>>,
    /// Heap-struct type for a shared-struct/enum KEY (or `Set[shared T]`
    /// element).
    pub(crate) key_shared_heap_type: Option<StructType<'ctx>>,
    /// Synthesized per-VALUE drop fn (`karac_drop_<T>(ptr)`) for a value
    /// that owns heap beyond the one-level `{ptr,len,cap}` overlay —
    /// slice 3r (deferred gap (d)). Mutually exclusive with `val_is_vec`
    /// and `val_shared_heap_type`; routes the free through
    /// `karac_map_free_with_val_drop_fn`.
    pub(crate) val_drop_fn: Option<inkwell::values::FunctionValue<'ctx>>,
}

/// Tagged kind for per-scope destructor actions emitted at scope exit.
/// The `scope_cleanup_actions` stack holds one `Vec` per scope frame;
/// each frame accumulates these in push order and drains in reverse.
pub(crate) enum CleanupAction<'ctx> {
    /// Unconditionally free an RC-elided `shared struct` binding
    /// (ownership phase-A elision — `src/ownership/elision.rs`; design
    /// record in phase-7-codegen.md). The analysis proved the binding's
    /// refcount never exceeds 1 and its type has no heap-owning fields
    /// and no user Drop, so scope exit frees the heap object directly —
    /// no dec, no zero-test, no drop-fn dispatch. Null-guarded exactly
    /// like `RcDec` (the let may sit in a skipped conditional branch,
    /// where the slot carries the entry-block null sentinel).
    FreeSharedElided {
        /// Binding name — reloads the current pointer from the slot at
        /// drain time (mirrors `RcDec`; elided bindings are never
        /// reassigned, but the reload keeps the two arms uniform).
        name: String,
        /// Fallback pointer if the variable is no longer in scope.
        ptr: PointerValue<'ctx>,
    },
    /// Free an RC-elided phase-B1 cluster ROOT by walking its
    /// self-link chain (ownership `src/ownership/elision.rs` § Phase
    /// B1): `while p != null { next = p-><link>; free(p); p = next; }`
    /// — no dec, no zero-test, no drop-fn dispatch. Sound because the
    /// analysis proves each reachable node has exactly one parent and
    /// every cursor's RcDec drains first (LIFO; the root is the
    /// first-declared cluster binding). Falls back to plain RcDec
    /// emission at drain time if the link field turns out not to be
    /// niche-shaped (defensive — all `Option[shared Self]` fields are
    /// niched today).
    FreeClusterWalk {
        /// Root binding name — reloads the current pointer at drain.
        name: String,
        /// Fallback pointer if the variable is no longer in scope.
        ptr: PointerValue<'ctx>,
        /// Member struct name (resolves `heap_type` + niche lookup at
        /// emit time).
        member_type: String,
        /// User-field index of the `Option[Self]` link (heap index is
        /// `+1` past the refcount header).
        link_field_index: usize,
    },
    /// Phase C1c: free an ADOPTED cluster root — an `Option[shared T]`
    /// binding holding a fresh-return builder's chain (every node at
    /// rc==1 straight from the callee's b2 build). Tag-guarded like
    /// `RcDecOption` (the w0 word is garbage under `None`), then the
    /// same link-following free-walk as `FreeClusterWalk` from the
    /// recovered inner pointer. The family's read-only verify proves
    /// nothing was spliced in or aliased out, so freeing by structure
    /// is exact — no dec, no zero-test, no drop-fn dispatch.
    FreeClusterWalkOption {
        /// Binding name (diagnostics / IR value names only — the slot
        /// pointer below is authoritative).
        name: String,
        /// Alloca of the Option struct (`{tag, w0, w1, w2}`).
        option_slot: PointerValue<'ctx>,
        /// The Option struct's LLVM type (for tag/w0 GEPs).
        option_ty: StructType<'ctx>,
        /// Member struct name (resolves `heap_type` + niche lookup at
        /// emit time).
        member_type: String,
        /// User-field index of the `Option[Self]` link.
        link_field_index: usize,
        /// Numeric `Some` tag from the seeded Option layout.
        some_tag: u64,
    },
    /// Decrement the refcount of a `shared struct` value.
    RcDec {
        /// Variable name — used to reload the current pointer value in case
        /// the binding was reassigned after the track call.
        name: String,
        /// Fallback pointer if the variable is no longer in scope.
        ptr: PointerValue<'ctx>,
        /// LLVM struct type of the heap-allocated RC object.
        heap_type: StructType<'ctx>,
    },
    /// Free the heap buffer of an owned `Vec[T]` or `String`.
    FreeVecBuffer {
        /// Alloca pointer of the Vec/String struct (`{ptr, len, cap}`).
        vec_alloca: PointerValue<'ctx>,
        /// LLVM type of the element T. When this is itself a Vec struct
        /// (`vec_struct_type`) or a Map handle pointer, the cleanup loop
        /// recursively drops each live element's heap-owned content before
        /// freeing the outer buffer. `None` for legacy/registration sites
        /// that don't track element type — those degrade to the pre-fix
        /// shape of freeing the outer buffer only, which is correct for
        /// primitive / inline-tuple elements but leaks for nested-heap
        /// element types. New code should always pass `Some(elem_ty)`.
        /// Closes the 2026-05-13 leak documented in `deferred.md` §
        /// *Recursive Drop for Heap-Owned Collection Elements*.
        elem_ty: Option<BasicTypeEnum<'ctx>>,
        /// True when the elements are `Tensor` values — each element is a
        /// single `ptr` to a `[rank][dims][data]` block that must be
        /// `free`d (one free per element, no inner recursion — tensors are
        /// single allocations). Set for `Vec[Tensor]` bindings (the
        /// `iter_axis` result); the `elem_ty` (a `ptr`) can't be
        /// distinguished from a Map handle / borrow by LLVM type alone, so
        /// this flag carries the intent. Mutually exclusive with the
        /// vec-struct recursive-drop path.
        elem_is_tensor: bool,
        /// `Some` when the elements are `Map`/`Set` handles — each live
        /// element is freed via `emit_free_one_map_handle` (same K/V drop
        /// classification a standalone Map binding uses) before the outer
        /// buffer is released. A Map handle is a bare `ptr` indistinguishable
        /// by LLVM type from a tensor block or borrow, so the element
        /// TypeExpr (not the type) drives this — set by `track_vec_of_maps_var`.
        /// Mutually exclusive with `elem_is_tensor` and the vec-struct
        /// recursive-drop path. Closes the `Vec[Map]` premature-free / UAF
        /// (Cluster 1): a Map moved into a Vec transfers ownership to the
        /// Vec, which must free the handle on drop.
        elem_map_drop: Option<MapElemDrop<'ctx>>,
        /// `Some` when the elements are a *named user struct or enum* whose
        /// own synthesized `__karac_drop_<T>` must run per element. The
        /// type-driven inline paths above (`llvm_ty_is_vec_struct`,
        /// `struct_owned_vec_field_indices`) are blind to a struct's enum
        /// field (an enum's LLVM layout is all-i64 payload words) and to a
        /// user enum element entirely, so a `Vec[Span]` (`Span` holds a
        /// `Tok` enum) leaked each element's enum payload on the outer
        /// buffer's drop (B-2026-06-12-6 cluster 2 gap 2). The drop fn
        /// (`emit_struct_drop_synthesis` / `emit_enum_drop_switch`, the same
        /// synthesizers the `StructDrop` / `EnumDrop` actions use) frees
        /// *every* heap-bearing field cap-guarded, so it is strictly more
        /// complete than — and **mutually exclusive with** — the inline
        /// `elem_ty` paths: when set, the drain runs only the per-element
        /// drop-fn loop, never the inline struct/vec-struct walk (running
        /// both would double-free the direct Vec/String fields). The element
        /// LLVM type still rides in `elem_ty` for the per-element GEP stride.
        /// Threaded from the dispatch sites (which hold the element
        /// `TypeExpr`) via `track_vec_of_aggs_var` — reverse-lookup by LLVM
        /// type is unsafe, since user structs use anonymous by-shape
        /// `context.struct_type`, so two distinct same-shaped types collide.
        elem_agg_drop: Option<FunctionValue<'ctx>>,
    },
    /// Free an owned `Tensor[T, Shape]`'s single heap block at scope
    /// exit. The binding's slot holds one pointer to the
    /// `[rank][dims][data]` allocation (`src/codegen/tensor.rs`); the
    /// drain loads it and frees when non-null. Move-out sites
    /// (tail return, by-value call arg, `let b = a;`) suppress by
    /// storing null into the slot — the null check is the Tensor
    /// analog of `FreeVecBuffer`'s `cap > 0` guard.
    FreeTensor {
        /// Alloca pointer of the tensor binding's `ptr` slot.
        tensor_alloca: PointerValue<'ctx>,
    },
    /// Free an owned `Column[T]`'s heap allocations at scope exit. The
    /// binding's slot holds one pointer to the control block
    /// `{ ptr data, ptr null_bitmap, i64 len, i64 capacity }`
    /// (`src/codegen/column.rs`); the drain loads it and, when non-null,
    /// frees the two buffers (`data`, `null_bitmap`) and then the control
    /// block itself (three `free`s). Move-out sites (tail return,
    /// by-value call arg, `let b = a;`) suppress by storing null into the
    /// slot — the null check is the Column analog of `FreeTensor`'s
    /// null-guard / `FreeVecBuffer`'s `cap > 0` guard.
    FreeColumn {
        /// Alloca pointer of the column binding's `ptr` slot.
        column_alloca: PointerValue<'ctx>,
        /// `true` for a `Column[String]` (heap element): the drain frees
        /// each valid slot's String heap before the data buffer. POD
        /// columns (numeric / bool) leave this `false`.
        string_elem: bool,
    },
    /// Free an owned `DataFrame`'s heap at scope exit. The slot holds one
    /// pointer to the control block `{ ptr entries, i64 len, i64 capacity }`
    /// (`src/codegen/dataframe.rs`); the drain loops the entries freeing
    /// each column (data + bitmap + control) and name buffer, then the
    /// entries buffer and the control block. Null = moved-out (skip),
    /// like `FreeColumn`.
    FreeDataFrame {
        /// Alloca pointer of the DataFrame binding's `ptr` slot.
        df_alloca: PointerValue<'ctx>,
    },
    /// Free the per-group heap buffers of a SoA-laid-out `Vec[T]` at scope
    /// exit. SoA storage is multi-allocation — one buffer per hot group
    /// plus an optional cold-group buffer — and the outer struct's field
    /// layout is `{ ptr_g0, ..., ptr_g(N-1), [ptr_cold,] i64 len, i64 cap }`
    /// rather than `FreeVecBuffer`'s `{ ptr, len, cap }`. Routing SoA
    /// through `FreeVecBuffer` (the pre-2026-05-29 state) GEP'd the
    /// generic Vec struct type against the SoA alloca, which (a) read
    /// `len` from the cap slot for any N≥2 hot groups (struct field 2
    /// of `{ ptr, len, cap }` lands at offset 16, which is the SoA
    /// `len` field whenever there are ≥2 leading pointer fields) and
    /// (b) freed only `ptr_g0`, leaking every other group buffer. This
    /// variant fixes both: GEPs against the SoA struct type with the
    /// correct `cap` index, and frees every group pointer (hot + cold)
    /// in declaration order.
    FreeSoaGroups {
        /// Alloca pointer of the SoA Vec struct
        /// (`{ ptr_g0, ..., [ptr_cold,] len, cap }`).
        soa_alloca: PointerValue<'ctx>,
        /// LLVM struct type for the SoA Vec — needed for `struct_gep`
        /// at cleanup so the `cap` and per-group-pointer slots are
        /// addressed by their actual indices in this layout, not by
        /// the plain Vec `{ptr,len,cap}` shape's indices.
        soa_struct_ty: StructType<'ctx>,
        /// Number of hot groups (matches `SoaLayout.num_groups`). Cleanup
        /// iterates `0..num_hot_groups` to free each hot group buffer.
        num_hot_groups: u32,
        /// `true` when the layout has a cold group — its pointer lives
        /// at struct field index `num_hot_groups` (just before `len`).
        has_cold: bool,
        /// `Some` when the element struct has heap (String/Vec) fields:
        /// the synthesized `__karac_soa_drop_<layout>(*mut SoaStruct)` that
        /// frees every live element's String/Vec buffers BEFORE the group
        /// buffers are released. `None` for a fully-POD layout — the cleanup
        /// arm then emits exactly the pre-heap-field group-buffer frees, so
        /// POD SoA codegen is byte-identical. Called inside the `cap > 0`
        /// guard (a moved-out / empty header has `cap == 0`, so the drop is
        /// skipped — the move recipient owns and frees the elements).
        soa_drop_fn: Option<FunctionValue<'ctx>>,
    },
    /// Free an owned `Map[K,V]` / `Set[T]` handle. Routes to
    /// `karac_map_free_with_drop_vec(handle, key_is_vec, val_is_vec)` when
    /// either flag is set (i.e. the key or value type follows the
    /// `{ptr, len, cap}` Vec/String layout), otherwise plain
    /// `karac_map_free`. The drop-vec helper walks live buckets and
    /// frees each side's data buffer per the flags before deallocating
    /// the bucket storage.
    ///
    /// `Set[T]` lowers to `Map[T, ()]` with `val_size = 0`; for
    /// `Set[Vec[T]]` / `Set[String]` codegen sets `key_is_vec = true,
    /// val_is_vec = false`. For `Map[String, Vec[U]]` both flags are
    /// set. Primitive-only maps stay on plain `karac_map_free`.
    ///
    /// When `val_shared_heap_type` is `Some(heap_ty)`, the cleanup emits
    /// a codegen-side bucket walk that calls `emit_rc_dec` on each live
    /// slot's value-half pointer before invoking the underlying
    /// `karac_map_free*` runtime. This closes the `Map[K, shared T]`
    /// leak (2026-05-16): without it, the runtime helper bit-copies
    /// the value pointer out and never decrements its refcount, so
    /// the heap object stays alive past the Map's scope exit. The
    /// shared-val rc_dec must run BEFORE `karac_map_free` releases
    /// the bucket storage, since the walk reads value-half bytes
    /// from `kv[]`. Per-instantiation specialization (not a runtime
    /// extension) so each shared type's heap layout is open-coded
    /// against the matching `SharedTypeInfo.heap_type` — the runtime
    /// is type-erased and can't know per-V layouts.
    FreeMapHandle {
        /// Alloca that holds the opaque map ptr.
        map_alloca: PointerValue<'ctx>,
        /// Whether the KEY type follows the Vec/String layout
        /// (`{ptr, len, cap}`). `true` triggers per-entry key-buffer
        /// free in `karac_map_free_with_drop_vec`. `Map[i64, V]` /
        /// `Map[bool, V]` etc. → `false`.
        key_is_vec: bool,
        /// Whether the VALUE type follows the Vec/String layout. `true`
        /// triggers per-entry value-buffer free. `Map[K, i64]` /
        /// `Set[T]` (val_size = 0) → `false`.
        val_is_vec: bool,
        /// LLVM heap-struct type for the VALUE when V is a shared
        /// struct / shared enum. `Some` triggers the codegen-side
        /// per-bucket rc_dec walk; `None` means V is not a shared
        /// type. Mutually exclusive with `val_is_vec` in practice
        /// (a Vec/String value doesn't carry a shared-type heap
        /// layout); both can be live alongside `key_is_vec` for
        /// `Map[Vec[K], shared V]` shapes.
        val_shared_heap_type: Option<StructType<'ctx>>,
        /// LLVM heap-struct type for the KEY when K is a shared
        /// struct / shared enum (or, for `Set[shared T]`, the
        /// element type — Set lowers to `Map[T, ()]` and the
        /// element is the key half of the bucket). Mirrors
        /// `val_shared_heap_type` on the K side. Both fire on drop
        /// when both K and V are shared. Mutually exclusive with
        /// `key_is_vec` in practice (a Vec/String key doesn't carry
        /// a shared-type heap layout). Closes the `Map[shared K, V]`
        /// / `Set[shared T]` leak (2026-05-16).
        key_shared_heap_type: Option<StructType<'ctx>>,
        /// Synthesized per-VALUE drop fn — see [`MapElemDrop::val_drop_fn`].
        val_drop_fn: Option<inkwell::values::FunctionValue<'ctx>>,
    },
    /// Phase 8 `File` handle slice F4b: scope-exit close for a
    /// pattern-bound File handle. The alloca holds the opaque `ptr`
    /// the F4 `match Ok(f) => ...` destructure stored after int→ptr
    /// re-typing. The drain emits `karac_runtime_file_close(load(file_alloca))`
    /// — null-handle is a no-op on the runtime side, so we don't
    /// guard here. Mirrors `FreeMapHandle`'s shape minus the
    /// per-element drop logic (File has no inner heap state — just
    /// the OS fd that std::fs::File's Drop handles).
    FreeFileHandle {
        /// Alloca that holds the opaque `*mut KaracFile` pointer.
        file_alloca: PointerValue<'ctx>,
    },
    /// GPU-SLIP-4b: scope-exit free for a `GpuBuffer[S]` binding that leaves
    /// scope without being downloaded. The alloca holds the `{ i64 handle, i64 n }`
    /// buffer value; the drain loads field 0 (the resident handle) and emits
    /// `karac_runtime_gpu_free_soa(handle)`. That runtime free is **idempotent**
    /// (a no-op for a handle already consumed by `gpu.download`, and handles are
    /// never reused), so no move-suppression is needed — the free after a download
    /// is a harmless no-op, and an un-downloaded buffer is freed exactly once.
    FreeGpuBuffer {
        /// Alloca holding the `{ i64 handle, i64 n }` `GpuBuffer` value.
        buf_alloca: PointerValue<'ctx>,
    },
    /// Scope-exit free for a local `OnceLock`/`OnceCell` binding. The alloca
    /// holds the opaque `*mut KaracOnce` handle from `OnceLock.new()`. The
    /// drain runs the element drop (if any) on the sealed value, then emits
    /// `karac_runtime_once_free(load(once_alloca))` — null-handle is a runtime
    /// no-op, so no guard on the free. Mirrors `FreeFileHandle`'s shape.
    /// `elem_drop` is `Some(karac_drop_<T>)` for a heap-owning fitting `T`
    /// (`String`/`Vec`/small single-heap-field struct — B-2026-07-12-2 gap 1):
    /// `set(v)` moved `v`'s buffer into the cell, so the cell owns it and the
    /// drain must free the ELEMENT's inner heap (the char/element buffer) before
    /// reclaiming the header + control block. `None` for a heap-free `T` (the
    /// header free is complete on its own). Any-width `T` is supported (a wide
    /// `T`'s sealed value is dropped by its full `karac_drop_<T>` all the same).
    FreeOnceHandle {
        /// Alloca that holds the opaque `*mut KaracOnce` pointer.
        once_alloca: PointerValue<'ctx>,
        /// `karac_drop_<T>` run on the sealed value ptr before free, when `T`
        /// owns heap; `None` for a heap-free `T`.
        elem_drop: Option<FunctionValue<'ctx>>,
    },
    /// Scope-exit free for a local `Interner` binding. The alloca holds the
    /// opaque `*mut KaracInterner` handle from `Interner.new()`; the free
    /// reclaims the interner and every stored byte string in one call
    /// (`karac_runtime_interner_free(load(interner_alloca))` — null-handle is
    /// a runtime no-op). No element drop is ever needed: the runtime owns all
    /// stored bytes, and `resolve` borrows (`cap = 0` String views) never own.
    FreeInternerHandle {
        /// Alloca that holds the opaque `*mut KaracInterner` pointer.
        interner_alloca: PointerValue<'ctx>,
    },
    /// Scope-exit free for a local `Arena[T]` binding. The alloca holds the
    /// opaque `*mut KaracArena` handle from `Arena.new()`; the free reclaims
    /// the arena and every stored blob in one call
    /// (`karac_runtime_arena_free(load(arena_alloca))` — null-handle is a
    /// runtime no-op). No element drop is ever needed: only heap-free
    /// element kinds are lowered (scalars / all-POD structs / String bytes
    /// the runtime owns), and `get` borrows (`cap = 0` String views) never
    /// own.
    FreeArenaHandle {
        /// Alloca that holds the opaque `*mut KaracArena` pointer.
        arena_alloca: PointerValue<'ctx>,
    },
    /// Heap-closure-env epic Slice 1 (B-2026-06-22-2): a binding holding a
    /// heap-env closure value. At scope exit, load the fat pointer, extract its
    /// env slot (the RC box `{ i64 refcount, env }`), decrement the refcount,
    /// and `free` the box when it hits 0. A null env (non-capturing closure or
    /// a moved-out sentinel) is skipped.
    FreeClosureEnv {
        /// Alloca holding the `{ fn_ptr, env_ptr }` closure fat pointer. The env
        /// slot points at the RC box whose field 0 is the i64 refcount (a
        /// uniform `{ i64 }` GEP reaches it regardless of the captured payload).
        fat_alloca: PointerValue<'ctx>,
    },
    /// Phase 6 "Channel AOT codegen lowering": scope-exit drop for a
    /// channel-end (`Sender`/`Receiver`) binding. The alloca holds the opaque
    /// `*mut KaracChannel` pointer both ends share. The drain emits
    /// `karac_runtime_channel_drop_sender` (`is_sender`) or `_drop_receiver`
    /// — the split lets the last `Sender` drop *close* the channel (waking
    /// blocked `recv`s); both release one `total` reference and free the
    /// queue at zero. So a `Channel.new()` (`total` 2) is reclaimed once both
    /// ends drop, and a `Sender.clone()` (`total`++) balances its own
    /// binding's drop. `is_sender` comes from the binding's `Sender`/
    /// `Receiver` surface type at registration. Null-handle is a no-op
    /// runtime-side. Mirrors `FreeFileHandle`'s shape plus refcount + close.
    DropChannelEnd {
        /// Alloca that holds the opaque `*mut KaracChannel` pointer.
        chan_alloca: PointerValue<'ctx>,
        /// `true` for a `Sender` end (drop may close), `false` for `Receiver`.
        is_sender: bool,
    },
    /// Run a per-enum drop function on a value-type (non-shared) enum
    /// alloca at scope exit. The drop function is synthesized once per
    /// enum type by `emit_enum_drop_switch` (one `__karac_drop_<EnumName>`
    /// symbol per non-shared enum with at least one heap-bearing payload
    /// field; lazily emitted on first registration). The function loads
    /// the tag, switches to the matching variant's cleanup BB, and frees
    /// each heap-bearing payload field's data buffer (Vec/String:
    /// `karac_runtime_free` on the payload's data pointer when `cap > 0`).
    /// Variants with no heap-bearing payload short-circuit to the default
    /// `ret void` arm. See Compound-payload enum follow-up: drop-path
    /// slice (Phase 7.2 Slice DP, 2026-05-09) for the design lock.
    EnumDrop {
        /// Alloca holding the enum's tagged-union struct value
        /// (`{ i64 tag, i64 w0, ..., i64 wN }`).
        enum_alloca: PointerValue<'ctx>,
        /// Cached `__karac_drop_<EnumName>` function — emitted once per
        /// enum type, reused across all `track_enum_var` registrations of
        /// that type.
        drop_fn: FunctionValue<'ctx>,
    },
    /// Run a per-struct drop function on a non-shared struct alloca at
    /// scope exit. The drop fn is synthesized once per struct type via
    /// `emit_struct_drop_synthesis` (one `__karac_drop_struct_<Name>`
    /// symbol per struct with at least one heap-owning field — Vec /
    /// String / Map / Set). The function takes `*mut StructTy` and
    /// frees each heap-owning field's content:
    ///   - Vec / String field → free `(field).data` when `cap > 0`
    ///   - Map / Set field → call `karac_map_free_with_drop_vec` /
    ///     `karac_map_free` per the field's key/val heap-ness
    ///
    /// Structs whose every field is primitive don't get a drop fn
    /// emitted and don't reach this cleanup variant. Closes the
    /// 2026-05-14 leak class for `struct { v: Vec[i64] }` /
    /// `struct { cache: Map[String, V] }` / `Vec[Container]` shapes
    /// (slice γ of the recursive-drop work).
    StructDrop {
        /// Alloca holding the struct value (`StructTy` directly, not
        /// a pointer to it).
        struct_alloca: PointerValue<'ctx>,
        /// Cached `__karac_drop_struct_<Name>` function.
        drop_fn: FunctionValue<'ctx>,
    },
    /// Decrement the refcount of the inner shared pointer carried by an
    /// `Option[shared T]` binding (`let x: Option[ListNode] = ...;` or
    /// any binding whose RHS yields an Option-wrapped shared ref). The
    /// slot holds the full Option struct (`{i64 tag, i64 w0, i64 w1, i64
    /// w2}` — see `seed_builtin_enum_layouts` for the layout): on
    /// cleanup, load the tag from field 0, branch on `tag == 1`
    /// (Some), and when Some load the i64 word at field 1 and
    /// `int_to_ptr` it to recover the inner heap pointer. Dispatch
    /// through `emit_refcount_dec` so Arc-promoted bindings take the
    /// atomic path. The None side is a no-op (no inner allocation to
    /// release).
    ///
    /// Closes the LeetCode #2 kata's heap-retention bug (2026-05-17):
    /// `let out = add_two_numbers(...)` produced a leak of one 100-node
    /// chain per iteration at K=500_000 because the let-stmt handler's
    /// `shared_info` resolution matched plain `ListNode` only, not
    /// `Option[ListNode]`. With this variant queued at let-stmt time,
    /// every Option-of-shared binding tracks its inner refcount the
    /// same way a plain shared-struct binding does.
    RcDecOption {
        /// Variable name — used for the `is_arc_binding` dispatch in
        /// `emit_refcount_dec` (same convention as `RcDec`).
        name: String,
        /// Alloca holding the full `{tag, w0, w1, w2}` Option struct
        /// value. Cleanup reloads the slot rather than capturing the
        /// pointer at registration time, so reassignment of the binding
        /// is observed at scope exit (mirrors how `RcDec` walks
        /// `variables[name]`).
        option_slot: PointerValue<'ctx>,
        /// LLVM struct type for the Option payload — almost always the
        /// 4-i64 shape pinned by `seed_builtin_enum_layouts`. Stored on
        /// the action so the GEP at cleanup uses the matching type
        /// even when future layout-widening changes the shape.
        option_ty: StructType<'ctx>,
        /// LLVM struct type of the inner shared T's heap layout
        /// (`{i64 refcount, fields...}`) — passed to `emit_refcount_dec`
        /// so the dec lands on the correct heap shape.
        heap_type: StructType<'ctx>,
        /// Discriminant value for the `Some` variant. Captured from
        /// `enum_layouts["Option"].tags["Some"]` at registration time
        /// so cleanup is robust against a future seed-table renumber.
        some_tag: u64,
    },
    /// Free the heap box carried by an enum binding whose payload `T` was
    /// too wide to inline (`llvm_type_word_count(T) > area` — see
    /// `coerce_to_payload_words`'s boxing path). The slot holds the
    /// `{tag, w0, ...}` struct; on cleanup, load the tag, branch on the
    /// payload-bearing discriminant (`Some` / `Ok`), recover the box
    /// pointer from word 0 (`int_to_ptr`), run the inner drop fn (when
    /// `T` itself owns heap — e.g. a struct with a `Vec` field), then
    /// `free` the box. The non-payload side is a no-op. Mirrors
    /// `RcDecOption`'s reload-from-slot + tag-guard shape; the box is the
    /// non-shared analogue of the RC pointer.
    BoxedEnumDrop {
        /// Variable name — for diagnostic-labeled IR temporaries.
        name: String,
        /// Alloca holding the `{tag, w0, ...}` enum struct value.
        /// Reloaded at cleanup so reassignment is observed at scope exit.
        enum_slot: PointerValue<'ctx>,
        /// LLVM struct type of the enum's tagged-union value (Option's
        /// 4-i64 shape / Result's 6-i64 shape) — the GEP type.
        enum_ty: StructType<'ctx>,
        /// `__karac_drop_struct_<T>` for the boxed payload, when `T` owns
        /// heap and needs an inner drop before the box is freed. `None`
        /// when `T` is all-inline (the common wide-scalar-struct case) —
        /// then only the box itself is freed.
        inner_drop_fn: Option<FunctionValue<'ctx>>,
        /// Discriminant value for the payload-bearing variant (`Some` /
        /// `Ok`). Captured at registration so cleanup is robust to a
        /// seed-table renumber.
        some_tag: u64,
    },
    /// User-source `defer { ... }` block to compile at scope exit.
    /// Pushed in program order at the `defer` statement's site; drained
    /// LIFO together with the compiler-internal cleanup variants at
    /// scope exit. Slice 1 of Phase 7 § *defer / errdefer codegen*
    /// covers normal-exit semantics; error-exit dispatch (errdefer,
    /// `?`-propagation, panic) lands in slice 2.
    UserDefer(Block),
    /// Invoke the per-type user-Drop wrapper `karac_drop_<Type>` on a
    /// struct alloca at scope exit. The wrapper (emitted by Prereq.2's
    /// `emit_user_drop_wrappers`) (a) calls the user-defined
    /// `<Type>.drop` method body and (b) hands off to the existing
    /// `__karac_drop_struct_<Type>` field-cleanup synthesiser when the
    /// type has heap-owning fields. Registration at let-binding time
    /// replaces — does NOT add to — the existing `StructDrop`
    /// registration for the same alloca, so field cleanup runs exactly
    /// once (via the wrapper's internal call) and never double-frees.
    /// Prereq.3 of the user-`impl Drop` dispatch slice
    /// (`docs/implementation_checklist/phase-7-codegen.md`).
    UserDrop {
        /// Source-level binding name — `let f = Foo {...}` gives `"f"`.
        /// Used by `suppress_user_drop_for_var` to find and remove a
        /// specific binding's UserDrop entry when the value is moved
        /// out via `let g = f;` (RHS is an Identifier). Without the
        /// name on the action, `binding_ptr` equality would be the
        /// only matcher available, but `let g = f` produces a fresh
        /// alloca for `g` — the source's `binding_ptr` doesn't move,
        /// it's just abandoned.
        binding_name: String,
        /// Alloca holding the struct value — same shape passed to
        /// `StructDrop` (`StructTy` directly, not a pointer to it).
        binding_ptr: PointerValue<'ctx>,
        /// Cached `karac_drop_<Type>` wrapper from
        /// `Codegen::user_drop_wrapper_fns`.
        drop_fn: FunctionValue<'ctx>,
    },
    /// User-source `errdefer { ... }` block to compile on error-exit
    /// paths only. Pushed in program order at the `errdefer` statement's
    /// site; drained LIFO in phase 1 (before the regular drop+defer
    /// stack) on error paths. Slice 2 of Phase 7 § *defer / errdefer
    /// codegen* covers param-less `errdefer { ... }` firing on `?`-
    /// propagation and explicit `return Err(...)` / `return None` sites.
    /// `binding: Some(name)` is the `errdefer(e) { ... }` payload-binding
    /// form — present on the variant for forward compatibility with
    /// slice 4 but NOT pushed by slice 2's `compile_stmt`; binding-form
    /// errdefers fall through to the catch-all `_ => Ok(())` arm and
    /// remain a no-op until slice 4 wires the bind-payload-then-emit
    /// dispatch.
    UserErrDefer {
        /// `errdefer(e) { ... }` payload-binding name. Slice 2 never
        /// pushes this variant with `Some(_)` (the binding form falls
        /// through in `compile_stmt` until slice 4), so today the field
        /// is always `None` at construction sites. Kept on the variant
        /// for forward compatibility with slice 4's bind-payload-then-
        /// emit dispatch — once that lands, `compile_stmt`'s gate lifts
        /// and `emit_cleanup_action_at` reads this to allocate an entry
        /// alloca for the binding and store the about-to-be-returned Err
        /// value before running the body.
        #[allow(dead_code)]
        binding: Option<String>,
        body: Block,
    },
    /// Release a `lock`-block's TAS spinlock: atomically store `0` into
    /// the mutex's lock-flag word. Seeded at the BOTTOM of the lock
    /// body's cleanup frame (pushed before the body compiles), so it
    /// drains LAST — after the body's own bindings — preserving reverse-
    /// construction RAII order (drop body resources under the lock, then
    /// release). Because it rides the normal cleanup-frame machinery, the
    /// release fires on EVERY exit path: straight-line fall-through
    /// (`drain_top_frame_with_emit`), `break` / `continue`
    /// (`emit_scope_cleanup_from` at the loop boundary), and `return`
    /// (`emit_scope_cleanup`). `flag_ptr` is GEP'd in the lock's entry
    /// block (before the acquire spin), so it dominates every body basic
    /// block — re-emitting the store at an early-exit site is well-formed
    /// without the reload-by-name guard the binding-keyed actions use.
    ReleaseMutex {
        /// Pointer to the mutex's lock-flag word (`{ i64 lockflag, T value }`
        /// field 0).
        flag_ptr: PointerValue<'ctx>,
    },
    /// Free the inline (non-boxed, non-RC) heap payload of an `Option[T]`
    /// binding/temp at scope exit, where `T` is itself a heap-owning
    /// `{ptr,len,cap}` value (`String` / `Vec[U]`). The type-erased
    /// `Option` layout (one 4-word shape shared by `Option[i64]` /
    /// `Option[String]` / …) carries no static drop kind for its payload,
    /// so its `__karac_drop_Option` switch is a no-op and the payload
    /// leaks unless freed here with the CONCRETE element type captured at
    /// the registration site (B-2026-06-10-6). Tag-guarded (skips `None`)
    /// AND cap-guarded (the payload's `cap` word is zeroed by any move-out
    /// site — a `match`/`if let` arm that binds the `Some` payload — so a
    /// moved-out payload is skipped here and freed once by the binding).
    /// The `Some` payload's `{ptr,len,cap}` occupies words w0/w1/w2 (the
    /// 3 words past the tag), so it overlays a plain Vec/String struct at
    /// option field index 1; the free reuses `emit_free_vec_buffer_body`
    /// (same recursive inner-element drop as `FreeVecBuffer`).
    FreeInlineOptionPayload {
        /// Alloca of the `Option` struct (`{tag, w0, w1, w2}`).
        option_slot: PointerValue<'ctx>,
        /// The `Option` struct's LLVM type (tag + payload GEPs).
        option_ty: StructType<'ctx>,
        /// Numeric `Some` tag from the seeded Option layout.
        some_tag: u64,
        /// `emit_free_vec_buffer_body` element type for the payload's
        /// `{ptr,len,cap}`: `i8` for `String`, `llvm(U)` for `Vec[U]`
        /// (a Vec-struct element type triggers the recursive inner free).
        payload_elem_ty: Option<BasicTypeEnum<'ctx>>,
    },
    /// `Result[T, E]` sibling of `FreeInlineOptionPayload`. Same erased-
    /// layout problem (one `{tag, w0, w1, w2}` shape across instantiations),
    /// but TWO heap-capable payload variants — `Ok(T)` and `Err(E)` — that
    /// OVERLAY the same payload words, distinguished by the tag. The cleanup
    /// reads the tag and frees whichever variant is live, keyed on that
    /// variant's CONCRETE element type. Each side is independently `None`
    /// when its half is scalar / non-inline-heap (`Result[String, i64]` →
    /// only `ok_payload_elem_ty` is `Some`). Both being `None` means nothing
    /// is registered (the registrar returns early). B-2026-06-10-6 follow-on.
    FreeInlineResultPayload {
        /// Alloca of the `Result` struct (`{tag, w0, w1, w2}`).
        result_slot: PointerValue<'ctx>,
        /// The `Result` struct's LLVM type (tag + payload GEPs).
        result_ty: StructType<'ctx>,
        /// Numeric `Ok` tag from the seeded Result layout.
        ok_tag: u64,
        /// Numeric `Err` tag from the seeded Result layout.
        err_tag: u64,
        /// Payload element type for the `Ok` overlay (`None` = scalar/non-heap).
        ok_payload_elem_ty: Option<BasicTypeEnum<'ctx>>,
        /// Payload element type for the `Err` overlay (`None` = scalar/non-heap).
        err_payload_elem_ty: Option<BasicTypeEnum<'ctx>>,
        /// FULL struct drop fn for the `Ok` payload when it is a heap-bearing
        /// STRUCT the `{ptr,len,cap}` overlay can't free (a multi-field
        /// `Rec { id, name: String }`, or a transparent single-field wrapper of
        /// one like `AlreadySetError[Rec]`). `Some(karac_drop_<payload>)` runs
        /// on a pointer to the payload area (result field 1) for the live tag;
        /// mutually exclusive with `ok_payload_elem_ty` (the overlay handles the
        /// direct-heap case). `None` = scalar / direct-heap-overlay / heapless.
        /// B-2026-07-12-2 gap 3 (wide/struct-with-heap rejected-value discard).
        ok_payload_struct_drop: Option<FunctionValue<'ctx>>,
        /// `Err`-side twin of [`Self::FreeInlineResultPayload::ok_payload_struct_drop`].
        err_payload_struct_drop: Option<FunctionValue<'ctx>>,
    },
    /// `Option[Map[K,V]]` / `Option[Set[T]]` inline payload free. Unlike the
    /// `{ptr,len,cap}` Vec/String payload, the `Some` payload is a single
    /// opaque map handle (`ptr`) at word w0 (option field index 1), freed via
    /// `emit_free_one_map_handle` (the same K/V-classified drop a standalone
    /// Map binding uses), not the Vec overlay. Tag-guarded (skips `None`).
    /// Move-out coordination differs from the Vec case: a `match`/`if let`
    /// arm binding the `Some` payload out sets the source tag to `None`
    /// (`suppress_inline_option_map_payload_cleanup`) so this free skips —
    /// there's no `cap` word to zero. B-2026-06-10-6 follow-on.
    FreeInlineOptionMapPayload {
        /// Alloca of the `Option` struct (`{tag, w0, ...}`).
        option_slot: PointerValue<'ctx>,
        /// The `Option` struct's LLVM type.
        option_ty: StructType<'ctx>,
        /// Numeric `Some` tag from the seeded Option layout.
        some_tag: u64,
        /// K/V drop classification for the map handle (key/val Vec-ness,
        /// shared-heap types) — same record the `Vec[Map]` element drop uses.
        map_drop: MapElemDrop<'ctx>,
    },
}

/// One let-binding hoisted out of an auto-par group via the slice-A return-
/// slot mechanism (Phase-7 Slice A — Par codegen: return values).
///
/// A class-(ii) binding is one defined inside a parallel group's branch but
/// read by stmts *outside* the group (or by the function-body's final
/// expression). Each such binding gets a dedicated field in a per-group
/// return struct (`__karac_ParGroup_<spawn_site_id>_Returns`). The branch
/// fn computes the value into a local alloca (the existing `compile_stmt`
/// path), then the slot-write emitter copies the loaded value into the
/// return-struct field. After `karac_par_run` joins, the parent loads each
/// slot back and binds it as a new variable in the surrounding function-
/// body scope so subsequent stmts see the value as if it were a normal
/// let.
///
/// Slot semantics are move-only: branch writes once, parent reads once,
/// no destructor on the slot itself (the existing branch-fn cleanup
/// discard — `scope_cleanup_actions` is reset on entry and dropped on
/// exit — already strands the branch's local destructors, so the slot
/// store is effectively a bitcopy and the parent's subsequent
/// `track_*` on the loaded value is the unique cleanup owner).
/// Ownership metadata for a class-(ii) slot binding whose branch-side
/// cleanup action was REMOVED at branch end because the value moves to
/// the parent through the return slot (2026-06-05 fix — pre-fix, the
/// branch freed the handle/payload it had just published, and the
/// parent's first use of the slot value was a UAF: observed as a
/// segfault on `let name = "ka" + "ra"; let mut m: Map[..] = Map.new();
/// m.insert(..)` whose auto-par group published `m` and then ran the
/// branch's `FreeMapHandle`). The parent rebinding site re-registers
/// the equivalent cleanup against ITS fresh alloca using this record,
/// making the parent the unique owner — same "move-only slot" decision
/// the Vec `cap = 0` suppression implements for `{ptr, len, cap}`
/// slots.
///
/// `RcDec` / `RcDecOption` / Vec slots stay on the established
/// branch-side *mutation* suppression (null ptr / zero tag / zero cap)
/// and are not represented here.
#[derive(Clone, Copy)]
pub(crate) enum SlotOwnership<'ctx> {
    /// `FreeMapHandle` metadata minus the alloca (parent supplies its
    /// own).
    Map {
        key_is_vec: bool,
        val_is_vec: bool,
        val_shared_heap_type: Option<StructType<'ctx>>,
        key_shared_heap_type: Option<StructType<'ctx>>,
        /// Slice 3r per-VALUE drop fn — see [`MapElemDrop::val_drop_fn`].
        val_drop_fn: Option<FunctionValue<'ctx>>,
    },
    /// `FreeFileHandle` — close at parent scope exit.
    File,
    /// `EnumDrop` — the cached `__karac_drop_<Enum>` fn.
    Enum { drop_fn: FunctionValue<'ctx> },
    /// `StructDrop` — the cached `__karac_drop_struct_<Name>` fn.
    Struct { drop_fn: FunctionValue<'ctx> },
    /// `UserDrop` — the cached `karac_drop_<Type>` wrapper.
    User { drop_fn: FunctionValue<'ctx> },
    /// `FreeSoaGroups` — per-group buffer frees for SoA-laid-out Vecs.
    Soa {
        soa_struct_ty: StructType<'ctx>,
        num_hot_groups: u32,
        has_cold: bool,
        /// The synthesized per-element heap-field drop fn (`None` for a POD
        /// layout) — carried so a SoA binding transferred into a `par` branch
        /// still drops its String/Vec elements at the branch's scope exit.
        soa_drop_fn: Option<FunctionValue<'ctx>>,
    },
    /// `FreeColumn` — the three-buffer Column control-block free (data /
    /// null-bitmap / control) at parent scope exit. `string_elem` carries
    /// the per-slot String-drain flag so a `Column[String]` transferred out
    /// of a `par` branch still frees its element Strings. Without this
    /// transfer, an auto-par branch that produced a `let c = Column.from_vec(…)`
    /// (or `.iter_valid()` etc.) freed the column it had just published to the
    /// parent's return slot → the parent read a dangling control block
    /// (B-2026-07-03-32: `b.len()` == 0 / OOB after the join).
    Column { string_elem: bool },
    /// `FreeDataFrame` — the entries-loop + control-block free at parent
    /// scope exit. Same publish-and-forget transfer as `Column`.
    DataFrame,
    /// `FreeTensor` — the null-guarded tensor control-block free at parent
    /// scope exit. Same publish-and-forget transfer as `Column`.
    Tensor,
}

#[derive(Clone)]
pub(crate) struct ReturnSlot<'ctx> {
    /// Source-level binding name produced inside the branch.
    pub(crate) binding_name: String,
    /// Position of the statement in the group's branch order — also the
    /// branch index passed to `emit_par_branch_fn`. Slot-writes inside
    /// the branch are gated on this index.
    pub(crate) branch_index: usize,
    /// LLVM scalar/aggregate type for this slot's field. Matches what
    /// the branch's `compile_stmt` produces for the let-binding's value
    /// (derived from explicit annotation or call-target return type).
    pub(crate) llvm_ty: BasicTypeEnum<'ctx>,
    /// Surface type NAME of the binding (first path segment of an explicit
    /// annotation, e.g. `"u8"`), when one is present. The LLVM type erases
    /// signedness (`i8` covers both `i8` and `u8`), so the post-join
    /// materialization re-registers this into `var_type_names` — otherwise
    /// `expr_is_unsigned_int` falls back to signed and a `u8`/`u16`/`u32`
    /// slot binding prints as its negative signed view (B-2026-07-03-21).
    pub(crate) var_type_name: Option<String>,
}

/// Phase 7 — Par codegen: cancellation and error propagation (slice 1a,
/// 2026-05-18). A `ResultSlot` records a par-block branch whose terminal
/// source expression is `Result[T, E]`-typed. Codegen materialises one
/// `Result_t_e` cell per such branch into a parent-allocated array;
/// branches write their Result value before `ret void` and, when the
/// stored tag is `Err` (== 0), also store `true` into the per-call
/// `AtomicBool` cancel flag so siblings' cooperative-cancel checks fire.
///
/// `binding_name` is the let-bound name carrying the Result; the
/// branch-fn locates the value to copy by looking up
/// `self.variables[binding_name]`. `branch_index` is the par-block
/// branch position (== statement index); `array_index` is the position
/// in the parent-allocated `[N_results x Result_t_e]` slot array
/// (which is dense — only result-tracking branches consume slots).
#[derive(Clone)]
pub(crate) struct ResultSlot {
    pub(crate) binding_name: String,
    pub(crate) branch_index: usize,
    pub(crate) array_index: usize,
}

/// Per-element predicate driving `emit_set_op_iter` (`Set.union` /
/// `intersection` / `difference` codegen). `Always` means insert every
/// element; the other two consult `karac_map_contains` against the named
/// other-set handle and either insert on hit or on miss.
#[derive(Clone, Copy)]
pub(crate) enum SetOpFilter<'ctx> {
    Always,
    ContainsIn(PointerValue<'ctx>),
    NotContainsIn(PointerValue<'ctx>),
}

// ── Loop frame: break / continue targets ───────────────────────

/// One control-flow frame on `Codegen::loop_stack`. Pushed by every
/// labeled-loop / labeled-block compile entry point; popped on exit.
/// `compile_break` / `compile_continue` walk the stack to find the
/// matching frame: when the source-level `break` / `continue` carries a
/// label they take the topmost frame whose `label == Some(l)`; otherwise
/// they take the innermost frame (last in the stack).
///
/// `label: None` is used by unlabeled loops (the dominant case today).
/// `Copy` is intentionally not derived: `Option<String>` is not `Copy`.
/// Reads at the four `compile_break` / `compile_continue` sites use
/// `.last().cloned()` instead of `.copied()`.
#[derive(Clone)]
pub(crate) struct LoopFrame<'ctx> {
    /// Source-level label of this frame, or `None` for unlabeled loops.
    /// Set from the loop AST node's `label: Option<String>` field for
    /// loops, and from `ExprKind::LabeledBlock { label, .. }` for blocks.
    pub(crate) label: Option<String>,
    /// Block to branch to on `continue`. For labeled blocks this is a
    /// freshly-created `lblock.continue.unreachable` BB whose body is a
    /// single `unreachable` instruction — the resolver rejects
    /// `continue label` referring to a labeled-block label, so this BB
    /// is never reached at runtime; the field stays uniform to avoid
    /// splitting `LoopFrame` into a `LoopOrBlockFrame` enum.
    pub(crate) continue_bb: BasicBlock<'ctx>,
    /// Block to branch to on `break` (loop / labeled-block exit).
    pub(crate) break_bb: BasicBlock<'ctx>,
    /// Optional alloca for `break value`. For labeled blocks the slot is
    /// always `Some` and stores both the body's tail value (on normal
    /// fall-through) and any `break label expr` value (on early exit).
    pub(crate) result_slot: Option<PointerValue<'ctx>>,
    /// `scope_cleanup_actions.len()` at the moment this frame was pushed
    /// — i.e. the index of the first cleanup frame INSIDE the loop /
    /// labeled block. `compile_break` / `compile_continue` drain frames
    /// `[cleanup_depth..]` (emit-only, stack untouched) before branching
    /// out, so bindings tracked in the per-iteration frame and in any
    /// nested block / `if let` / match-arm frames between the jump and
    /// the loop boundary get their scope-exit cleanup on the early-exit
    /// path too. Without it, a `break` past a tracked shared binding
    /// skipped its `RcDec` — one leaked ref per early exit (surfaced by
    /// kata #24's `if let`-bound pair-swap cursors, latent for every
    /// loop-body `let` + `break` shape before that).
    pub(crate) cleanup_depth: usize,
}

/// One half of a Vec-index safety fact, asserted by a dominating
/// `while`-guard or `for`-range and consulted by `compile_vec_index`
/// to elide the matching half of its bounds check.
///
/// The two halves correspond to the unsigned bounds check
/// (`icmp uge idx, len` → panic) which catches both negative-idx and
/// idx-too-big in one compare. Splitting into signed-form facts lets
/// us drop one or both halves when the source-level guard already
/// proves them.
#[derive(Debug, Clone)]
pub(crate) enum AssertedIndexBound {
    /// `idx_var >= 0` is known true in the current scope. Elides the
    /// negative-idx half of the bounds check on `vec[idx_var]` regardless
    /// of which Vec is being indexed (the lower bound doesn't depend on
    /// the Vec).
    LowerBound { idx_var: String },
    /// `idx_var < vec_var.len()` is known true in the current scope.
    /// Elides the upper-half of the bounds check on `vec_var[idx_var]`
    /// (and only on that specific Vec). The Vec is identified by its
    /// source-level variable name; the `len_alias` table is consulted
    /// during guard parsing to resolve `idx_var < n` where `n` is a
    /// local binding to `vec_var.len()`.
    UpperBound { idx_var: String, vec_var: String },
}

/// Direction of a syntactically-monotone loop variable — a `let mut`
/// binding whose every write inside a loop body is `x = x + c` /
/// `x += c` (Increasing) or `x = x - c` / `x -= c` (Decreasing) with a
/// non-negative integer literal `c`. Sound to emit `llvm.assume(x >=
/// init)` / `(x <= init)` at body entry because AOT arithmetic traps
/// on overflow (design.md § Arithmetic Overflow; landed 2026-06-07) —
/// a wrapping update panics before the wrapped value could violate
/// the assume. See `control_flow_bce.rs` § monotone scan and
/// `docs/investigations/bce_monotonic_assume.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MonotoneDir {
    Increasing,
    Decreasing,
}

// ── Spawn-site metadata (Debugger Contract slice 3) ────────────

/// One row of the `KARAC_SPAWN_SITES` metadata table.
///
/// A `SpawnSiteId` (the `id` field) is minted per `par {}` block during
/// codegen — both explicit `par {}` blocks (`compile_par_block`) and
/// compiler-inferred parallel groups (`compile_function_body`'s auto-par
/// dispatch) flow through `emit_par_run`, which calls
/// `Codegen::record_spawn_site` to push a record into `Codegen::spawn_sites`.
/// The collected records are emitted as a module-scope global at the
/// end of compilation by `emit_spawn_sites_metadata`.
///
/// See `design.md § AI-First Compiler Interface > Debugger Contract` for
/// the four-part contract this is the foundation of:
///
/// - slice 3 (this entry) — produces `KARAC_SPAWN_SITES` + `_LEN` + `_ENABLED`.
/// - slice 4 — references these IDs in worker-frame metadata
///   (parent-frame ref + await-chain pointer).
/// - slice 5 — exposes the table to Kāra-callable code via
///   `std.runtime::list_par_blocks()` / `has_debug_metadata()`, reading
///   `KARAC_SPAWN_SITES` + `_LEN` + `_ENABLED` directly through external
///   linkage.
/// - the still-future `std.panic` crash report
///   (`design.md § Crash Report Format`) reads them for the
///   `parallel_context` field.
pub(crate) struct SpawnSiteRecord {
    /// Stable per-binary `SpawnSiteId`. Equal to the `par_counter` value
    /// at the time the record was minted; the same value is used to name
    /// the par-branch functions (`__par_branch_<id>_<i>`).
    pub(crate) id: u32,
    /// Source filename. Empty when `Codegen::source_filename` was not
    /// threaded in (most tests, ad-hoc IR dumps).
    pub(crate) file: String,
    /// 1-indexed line of the par-block keyword (or first stmt of an
    /// inferred group), per `crate::byte_offset_to_line_col`.
    pub(crate) line: u32,
    /// 1-indexed column of the par-block keyword (or first stmt of an
    /// inferred group), per `crate::byte_offset_to_line_col`.
    pub(crate) col: u32,
    /// Static branch count (number of stmts in the block at codegen
    /// time). `None` would indicate "unknown"; v1's runtime spawns one
    /// OS thread per branch (`karac_par_run` in `runtime/src/lib.rs`),
    /// so the count is statically the stmt count and the field is
    /// always `Some(stmts.len() as u32)` today. Recorded as `Option`
    /// to lock the field shape now — when work-stealing or
    /// thread-pool-bounded execution lands (Phase 6.2 / 6.3), the
    /// static count loses meaning and slice 4 / 5's introspection
    /// surface will need to choose between "branches in source" (this
    /// field) and a separate dynamic "currently active workers"
    /// surface from the runtime. Defer the decision; the field name
    /// captures the static-source intent.
    pub(crate) worker_count: Option<u32>,
}

/// Per-(K, V) cache of monomorphized `Map[K, V]` method symbols.
///
/// Slice 1 of the monomorphized-collections work (see
/// [`wip-monomorphized-collections.md`](../docs/implementation_checklist/wip-monomorphized-collections.md)
/// § Slice 1) replaces the type-erased `karac_map_*` runtime dispatch
/// — which routes every operation through function pointers consulting
/// a byte-blob storage — with per-K/V LLVM symbols compiled into the
/// user crate. Each emitted function has `LinkOnceODR` linkage so
/// cross-crate / cross-TU duplication collapses at link time
/// (locked design § 3.2).
///
/// Slice 1 ships `Map[i64, i64]` only (the smallest realistic K/V).
/// `linkonce_odr`-emitted symbols that have no callers get DCE'd at
/// link time, so the cache is keyed by the K/V mangle pair and
/// emission is gated on the dispatch site at `compile_map_method`
/// finding the corresponding mono symbol via
/// `should_use_mono_map_for(key_ty, val_ty)`.
///
/// All wrapper bodies in Slice 1a delegate to the existing erased
/// `karac_map_*` runtime (1:1 forwarding) — the per-K/V symbol exists
/// at this slice purely to validate emission, mangling, and dispatch.
/// Slice 1b replaces the hot-path bodies (`insert_old`, `get`) with
/// fully-inlined LLVM bodies (direct i64 hash + icmp eq, no extern
/// call) and locks the bench gain.
#[derive(Copy, Clone)]
pub(crate) struct MapMonoMethods<'ctx> {
    /// `i64 karac_map_<keymangle>_<valmangle>_len(map: ptr)`.
    pub(crate) len_fn: FunctionValue<'ctx>,
    /// `i1 karac_map_<keymangle>_<valmangle>_insert_old(map: ptr,
    /// key: K, val: V, out_old_val: ptr)`. Slice 1b.2a ships a
    /// slow-path-only body that delegates to the erased
    /// `karac_map_insert_old` extern via stack-allocated key/val
    /// slots; Slice 1b.2b adds the inline fast-path (load-factor
    /// check + inline hash + probe loop + inline eq) that unlocks
    /// the bench gain.
    pub(crate) insert_old_fn: FunctionValue<'ctx>,
    /// `i1 karac_map_<keymangle>_<valmangle>_get(map: ptr,
    /// key: K, out_val: ptr)`. Slice 1b.3 lands the inline-probe
    /// body — no load-factor branch (get never resizes), no
    /// tombstone-tracking PHI; just hash + probe + i64 eq + val
    /// load on match. Mirrors the `KaracMap::lookup` /
    /// `KaracMap::get` shape from `runtime/src/map.rs`.
    pub(crate) get_fn: FunctionValue<'ctx>,
}

#[cfg(test)]
mod layout_id_tests {
    use super::LayoutId;

    // Locks the per-layout-monomorphization symbol-naming contract that
    // slices 2-3 build on (`docs/spikes/per-layout-monomorphization.md` §4.3):
    // `Aos` contributes no mangle suffix (so an all-`Aos` monomorph keeps the
    // existing symbol), and `Soa(name)` produces a stable `soa_<name>` fragment.
    #[test]
    fn mangle_suffix_contract() {
        assert_eq!(LayoutId::Aos.mangle_suffix(), None);
        assert_eq!(
            LayoutId::Soa("entities".to_string()).mangle_suffix(),
            Some("soa_entities".to_string())
        );
    }

    // Distinct `layout` blocks (distinct names) yield distinct suffixes, so two
    // groupings of one element type can't collide into one monomorph symbol.
    #[test]
    fn distinct_layout_names_distinct_suffixes() {
        let a = LayoutId::Soa("grid".to_string()).mangle_suffix();
        let b = LayoutId::Soa("coll".to_string()).mangle_suffix();
        assert_ne!(a, b);
    }
}
