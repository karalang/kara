//! Per-variable type-shape tables.
//!
//! The "what type is this local?" side tables `Codegen` keeps alongside the
//! `variables` slot map: surface type names, Vec/Slice/Array/tuple element
//! types (LLVM and `TypeExpr` forms), Option/Result payload instantiations,
//! String/CStr/handle classifications, the per-binding layout carrier, and
//! the three `pending_let_*` staging cells threaded through a let-RHS
//! compile. Extracted from `Codegen` as the first sub-slice of cluster 15
//! (`VarTables`) of the state-decomposition spike; see
//! `docs/spikes/state-decomposition-codegen-methodcall.md`.
//!
//! `variables` itself — the name → `VarSlot` alloca map — stays on
//! `Codegen`: it is the frame's core, not a side table.

use std::collections::{HashMap, HashSet};

use inkwell::types::BasicTypeEnum;
use inkwell::values::PointerValue;

use super::arena;
use super::state::LayoutId;
use crate::ast::TypeExpr;

pub(crate) struct VarTypes<'ctx> {
    /// Maps variable name → Kāra type name (for struct/enum field resolution).
    pub(crate) var_type_names: HashMap<String, String>,
    /// Per-element type names of a let-bound TUPLE binding (`let t = (i, Inner
    /// { .. })` → `[None, Some("Inner")]`), so a struct-field access through a
    /// tuple element (`t.1.name`) can resolve the element's struct type.
    /// `type_name_of_expr` is structural (the parser shares spans across
    /// chained postfix, so a span-keyed expr-type lookup can't distinguish
    /// `t` / `t.1` / `t.1.name`); this records the element types at the
    /// binding site from the annotation or the RHS tuple literal
    /// (B-2026-06-11-6). `None` for a non-struct element (primitive / nested
    /// tuple / unresolved RHS — those don't field-access into a struct).
    pub(crate) tuple_var_elem_type_names: HashMap<String, Vec<Option<String>>>,
    /// Full element `TypeExpr`s for tuple bindings whose LET carried a
    /// tuple ANNOTATION (`let t: (Vec[i64], i64) = …`) — B-2026-08-02-10.
    /// The names registry above erases generic arguments (a `Vec[i64]`
    /// element records the bare name "Vec"), which is not enough to
    /// register a synth receiver for tuple-element method dispatch or to
    /// drop a displaced Vec-of-heap element precisely. Populated only from
    /// annotations (full fidelity by construction); an unannotated tuple
    /// literal records nothing here and its consumers keep their
    /// names-registry / LLVM-layout fallbacks.
    pub(crate) tuple_var_elem_type_exprs: HashMap<String, Vec<TypeExpr>>,
    /// B-2026-08-13-10 — IMMUTABLE locals bound to an integer literal
    /// (`let k = 32i64;`), name -> value, for the full-unroll guard.
    ///
    /// `while_loop_wants_full_unroll` reads the loop bound off the SOURCE
    /// guard and only accepted an integer LITERAL (`while j < 32`). The
    /// natural spelling of a counted loop names a constant instead
    /// (`let k = 32i64; … while j < k`), which took the literal-only path's
    /// `None` and got no hint — even though LLVM had already
    /// constant-propagated the bound and was emitting `cmp $0x20`. It then
    /// unrolled by 4 and left a data-dependent branch per element where
    /// rustc and clang fully unroll the same loop branchlessly.
    ///
    /// Only `let` WITHOUT `mut` lands here, so "never reassigned" is
    /// structural rather than an analysis: an assignment to the name would
    /// not have typechecked. Any other binding of the same name (a `mut`
    /// let, a non-literal let, a pattern bind) REMOVES the entry, so the map
    /// never outlives the constant it describes.
    ///
    /// Staleness is bounded to a missed or spurious HINT, never a
    /// miscompile: `llvm.loop.unroll.full` carries no count and LLVM still
    /// has to prove the trip count itself before it unrolls anything — the
    /// same advisory-only argument the literal path already rests on.
    pub(crate) int_const_locals: std::collections::HashMap<String, i64>,
    /// Per-variable Vec element type tracking (variable name → element LLVM type).
    pub(crate) vec_elem_types: HashMap<String, BasicTypeEnum<'ctx>>,
    /// Element type for the let-binding currently being compiled, threaded
    /// through `compile_expr(rhs)` so zero-arg `Vec.with_capacity(n)` can
    /// recover `T` from the annotation. Set just before compiling the let's
    /// RHS, cleared just after. Read by `Vec.with_capacity` in
    /// `compile_assoc_call`.
    pub(crate) pending_let_elem_type: Option<BasicTypeEnum<'ctx>>,
    /// Surface `TypeExpr` of the element type for the let-binding currently
    /// being compiled — the `TypeExpr` sibling of `pending_let_elem_type`.
    /// `Vec.filled(n, val)` reads this to decide whether each slot needs a deep
    /// clone (heap-backed element types: `Vec[Vec[_]]`, `Vec[String]`) versus a
    /// trivial bit-copy. Taken (not just read) at the start of the `filled` arm
    /// — before the fill argument is compiled — so a nested inner
    /// `Vec.filled(...)` does not inherit the outer binding's stale element type.
    pub(crate) pending_let_elem_type_expr: Option<TypeExpr>,
    /// B-2026-08-13-17 — the annotated TUPLE type of the `let` binding whose RHS
    /// is being compiled, staged so `compile_tuple` can lay the aggregate out at
    /// the DECLARED element widths instead of at the compiled values' own.
    ///
    /// The tuple sibling of `pending_let_elem_type_expr`, and it exists for the
    /// same reason: a tuple literal cannot recover its declared widths from its
    /// elements. Without it, `let t: (i64, i64) = (b, d)` with `b: u8` laid out
    /// `{i8, i32}` while every read resolved `{i64, i64}` from the annotation,
    /// so `t.0` sign-extended and printed 200 as -56 on both compiled backends.
    ///
    /// Set only for a `TypeKind::Tuple` annotation, consumed (taken) by
    /// `compile_tuple`, and saved/restored around the RHS like its siblings.
    pub(crate) pending_let_tuple_te: Option<TypeExpr>,
    /// Per-variable Slice element type tracking (variable name → element LLVM type).
    /// Entries only exist for values whose LLVM representation is the
    /// 2-field slice struct `{ptr, i64}`; used to dispatch indexing and
    /// iteration lowering.
    pub(crate) slice_elem_types: HashMap<String, BasicTypeEnum<'ctx>>,
    /// Per-binding physical-layout value carrier (slice 5): the active
    /// `LayoutId` of each in-function *local* binding, seeded at its binding
    /// site (`seed_binding_site_layout` — the one sanctioned origin name-match)
    /// and read by `active_layout_id` at every use site. This is design.md
    /// Feature 1's "the value carrier is a `LayoutId` attached to bindings, not
    /// the binding name": it replaces the old name-keyed `soa_layouts` fallback
    /// in the access paths, so a base-symbol param named like a layout block no
    /// longer lowers SoA by coincidence (its layout comes from `layout_subst`,
    /// set by the call dispatch, only inside a monomorph). Function-scoped:
    /// cleared at each function entry and save/restored (`mem::take`) around the
    /// mono entry points, exactly like `variables` / `ref_params`.
    pub(crate) binding_layouts: HashMap<String, LayoutId>,
    /// Element `TypeExpr`s per let-bound TUPLE variable, recorded where the
    /// let-site resolves them (annotation / literal / callee return). A bare
    /// rebind `let t2 = t;` has none of those sources, so without this table
    /// the destination could not re-register the tuple's element-bodies walk
    /// (`__karac_dropelems_tuple_*`) after the move disarms the source's —
    /// the body would then run on one backend and not the other. Cleared per
    /// function alongside `enum_inst_var_types`.
    pub(crate) tuple_var_elem_tes: HashMap<String, Vec<TypeExpr>>,
    /// Resolved `Option[P]` / `Result[O, E]` instantiation per let-bound
    /// variable whose payload bodies walk registered
    /// (`__karac_dropelems_opt_*` / `__karac_dropelems_res_*`). Consulted by
    /// a bare rebind (`let o2 = o;`) so the destination can re-register after
    /// the move disarms the source — the same role `tuple_var_elem_tes`
    /// plays for tuples. Cleared per function alongside it.
    pub(crate) optres_var_payload_tes: HashMap<String, TypeExpr>,
    pub(crate) var_elem_type_exprs: HashMap<String, TypeExpr>,
    /// Per `Array[T, N]` BINDING: the element `TypeExpr` `T` (B-2026-07-30-3).
    ///
    /// Deliberately a SEPARATE map rather than an `Array` arm in
    /// `var_elem_type_exprs`: that table has ~170 readers across codegen, many
    /// of which treat a present entry as "this binding is a Vec/Slice/Map", so
    /// adding Arrays to it changes behaviour far outside the one call-site
    /// substitution this is for. One writer (`register_var_from_type_expr`) and
    /// one reader (`arg_container_elem_type_expr`), saved/restored alongside the
    /// other var side-tables so a nested mono compile can't see the caller's
    /// arrays (see `SavedVarSideTables`).
    pub(crate) array_elem_type_exprs: HashMap<String, TypeExpr>,
    /// Per closure BINDING (`let g = || v`): the `Vec[T]` / `VecDeque[T]`
    /// `TypeExpr` the closure RETURNS, recorded at the let site (where the
    /// captured source's element type is still visible). Lets an INLINE index of
    /// a closure-call result (`g()[i]`) resolve the element type — the `Call`
    /// and its wrapping `Index` share a span, so `expr_types` / `owned_temp_drops`
    /// are clobbered, and a closure callee has no `fn_return_type_exprs` entry
    /// (B-2026-07-18-43). Consulted by `inline_temp_vec_te`.
    pub(crate) closure_ret_vec_te: HashMap<String, TypeExpr>,
    /// Per-`OnceLock[T]` / `OnceCell[T]` binding: the element `T` `TypeExpr`
    /// plus whether the receiver is a thread-safe `OnceLock` (`true`) or a
    /// single-task `OnceCell` (`false`). Populated by
    /// `register_var_from_type_expr`; membership is also the dispatch gate for
    /// `compile_once_method` (`OnceLock`/`OnceCell` are baked stdlib structs
    /// with no user impl, so `set`/`get`/`is_set` must be intercepted before
    /// the user-impl lookup). `T` sizes the `value_size` FFI arg and the
    /// `Option[ref T]` / `Result` payload shape. Both primitives share one
    /// runtime primitive at v1 (the `OnceCell` never contends the lock).
    pub(crate) once_var_types: HashMap<String, (TypeExpr, bool)>,
    /// Local bindings holding an `Interner` handle (`let i = Interner.new()`
    /// or an `Interner`-annotated binding). Membership is the dispatch gate
    /// for `compile_interner_method` — `Interner` is a baked stdlib struct
    /// with no user impl, so `intern`/`resolve`/`len` must be intercepted
    /// before the user-impl lookup. The slot holds the opaque
    /// `*mut KaracInterner` (no element type to record — the payloads are
    /// always byte strings, and `Symbol` erases to `i64`).
    pub(crate) interner_vars: std::collections::HashSet<String>,
    /// Local bindings holding an `Arena[T]` handle, with the recorded
    /// element kind from the `let a: Arena[T] = Arena.new()` annotation.
    /// Membership is the dispatch gate for `compile_arena_method` (the
    /// `interner_vars` posture); the elem kind drives the per-`T` blob
    /// marshalling. `ArenaRef[T]` / `ArenaCheckpoint` erase to bare `i64`s.
    pub(crate) arena_vars: HashMap<String, arena::ArenaElemKind>,
    /// Static foreign-checkpoint guard: checkpoint binding → the arena
    /// binding that minted it (`let cp = a.high_water_mark()`). A
    /// `rewind_to(cp)` whose owner differs from the receiver compiles to a
    /// no-op, matching the interpreter's handle-id guard.
    pub(crate) arena_checkpoint_owner: HashMap<String, String>,
    /// B-2026-07-08-9: per-`Option[T]`-variable payload `TypeExpr`, so the
    /// f-string / `println` Display path can synthesize a concrete
    /// `Some(<T>)`/`None` renderer. Option/Result are generic built-ins whose
    /// variant defs carry only the generic `T`; the concrete payload type is
    /// recovered here (populated by `register_var_from_type_expr`) — the
    /// missing plumbing that made Option/Result Display unsupported in codegen
    /// while the interpreter rendered them. Keyed by variable name.
    pub(crate) var_option_payload_te: HashMap<String, TypeExpr>,
    /// B-2026-07-08-9 sibling: per-`Result[T, E]`-variable `(ok, err)` payload
    /// `TypeExpr`s for the `Ok(<T>)`/`Err(<E>)` Display renderer.
    pub(crate) var_result_payload_te: HashMap<String, (TypeExpr, TypeExpr)>,
    /// Variables whose surface type is `String`. Disambiguates Strings from
    /// `Vec[u8]` at iteration time — both share the `{ptr, i64, i64}`
    /// physical layout and are both registered in `vec_elem_types` with
    /// element-LLVM-type `i8`, so the for-loop dispatcher otherwise can't
    /// tell which iteration shape to emit. `for c in s` and `for c in
    /// s.chars()` on a String iterate per Unicode scalar value via the
    /// `karac_string_decode_char` runtime helper; `for b in v` on a
    /// `Vec[u8]` iterates per byte. Populated alongside the existing
    /// `vec_elem_types` insertion at every String-registration site.
    pub(crate) string_vars: HashSet<String>,
    /// String bindings proven to be stable compile-time ALL-ASCII constants,
    /// mapped to the String-struct alloca their `let` created
    /// (B-2026-07-27-7). Gates the branch-free stride-1 `.chars()` loop in
    /// `control_flow_for.rs`, which is only correct when every byte is a
    /// complete 1-byte UTF-8 scalar. The name-level proof comes from
    /// `ascii_const_chars::ascii_const_string_lets`; the alloca is stored so
    /// the loop site can additionally verify the receiver resolves to THAT
    /// binding — a shadow, a same-named parameter, or a stale cross-function
    /// entry then misses and keeps the general decode loop rather than
    /// silently mis-iterating multibyte text.
    pub(crate) ascii_const_string_lets: HashMap<String, PointerValue<'ctx>>,
    /// Variables whose surface type is `ref CStr` (the `c"..."` literal
    /// type — design.md § C-String Literals). Physically a `{ptr, i64}`
    /// slice-struct value: the NUL-terminated rodata pointer plus the
    /// source byte count (excluding the NUL), which is what makes `len()`
    /// O(1) per the design. Kept separate from `slice_elem_types` so the
    /// CStr method surface (`as_ptr` / `as_bytes` / `len` / `is_empty`,
    /// dispatched in `compile_cstr_method`) doesn't leak onto real
    /// slices and vice versa. Populated by the `let` RHS/annotation
    /// heuristics (stmts.rs) and `register_var_from_type_expr` (params).
    pub(crate) cstr_vars: HashSet<String>,
}
