//! Per-function signature and body-fact tables, keyed by function name.
//!
//! What `Codegen` knows about each FUNCTION as a callee or a compile
//! subject: the polymorphic ASTs (`fn_asts`), the parameter-mode tables
//! (`ref` / `mut ref` / tensor / slice-elem), the return-shape tables
//! (type names, `TypeExpr`s, ref-return inner, Option-of-shared inner),
//! extern link names, effect and inline-hint tables, `#[track_caller]`
//! membership, declare-only externs, and the per-body identifier-mention
//! offsets (`scrutinee_read_after_match`'s liveness input). Extracted from
//! `Codegen` as a cluster-15 sub-slice of the state-decomposition spike;
//! see `docs/spikes/state-decomposition-codegen-methodcall.md`.

use std::collections::HashMap;

use inkwell::types::BasicTypeEnum;

use crate::ast::{Function, TypeExpr};

pub(crate) struct FnSig<'ctx> {
    /// Non-generic top-level function AST nodes keyed by name. Retained so the
    /// per-layout-monomorphization dispatch (slice 2) can compile an on-demand
    /// SoA specialization of a plain `Vec[E]`-taking helper at a call site with
    /// a SoA argument (`docs/spikes/per-layout-monomorphization.md`). The
    /// non-specialized (all-`Aos`) function is still declared + compiled in the
    /// normal module pass; this registry feeds only the layout-specialized
    /// monomorphs.
    pub(crate) fn_asts: HashMap<String, Function>,
    /// Function parameter ref-ness (function name → vec of is_ref per param).
    pub(crate) fn_param_ref: HashMap<String, Vec<bool>>,
    /// The `mut ref` / `mut Slice` subset of [`Self::fn_param_ref`] — a
    /// MUTATE-THROUGH borrow, as opposed to a read-only `ref`.
    ///
    /// B-2026-08-05-37: the two need different argument lowering. A read-only
    /// borrow of a place may be satisfied by a pointer to a shallow copy (the
    /// callee only reads it), and for most types that is what the rvalue path
    /// produces. A `mut ref` borrow may NOT: the callee's writes have to land
    /// in the caller's storage, so the argument must be a pointer to the PLACE.
    /// Passing a copy makes the mutation silently disappear.
    pub(crate) fn_param_mut_ref: HashMap<String, Vec<bool>>,
    /// Per-parameter `Tensor[T, S]` info (function name → vec of `Some(info)`
    /// for each `(ref) Tensor` param, `None` otherwise). Lets a call site thread
    /// the DECLARED element type of a tensor param into `pending_let_tensor_info`
    /// before compiling a `Tensor.{from,zeros,ones,full}` argument — so an
    /// unsuffixed-literal `Tensor.from([-1.0, 2.0])` bound to a `ref
    /// Tensor[f32, …]` param lays its data out at the expected element width
    /// (the argument-position sibling of the let-annotation threading via
    /// `tensor_var_infos`). B-2026-07-18-9.
    pub(crate) fn_param_tensor_info:
        HashMap<String, Vec<Option<crate::codegen::state::TensorVarInfo<'ctx>>>>,
    /// `unsafe extern` imports that carry `#[link_name("symbol")]`: maps the
    /// Kāra fn identifier → the foreign symbol it actually binds. The import
    /// is registered in the LLVM module under the *symbol* name, so call
    /// sites must translate the Kāra name through this map before
    /// `module.get_function(...)` (an LLVM function's name *is* its symbol).
    /// Empty unless a program uses `#[link_name]`; the common case keeps the
    /// Kāra name and never touches this map. Lets a snake_case Kāra fn bind a
    /// PascalCase C symbol — the LLVM-C self-hosting binding's requirement
    /// (`docs/spikes/self-hosting-llvm-c-ffi.md`).
    pub(crate) extern_link_names: HashMap<String, String>,
    /// Function parameter slice element type (function name → per-param
    /// Some(elem_ty) if that param is Slice[T] / mut Slice[T], else None).
    /// Used at call sites to emit Array → Slice and Vec → Slice coercions.
    pub(crate) fn_param_slice_elem: HashMap<String, Vec<Option<BasicTypeEnum<'ctx>>>>,
    /// Function return-type name (function name → user-type name of the
    /// declared return type, if it is a bare `Path` to a known struct /
    /// enum). Used by `compile_field_access` to recover the static type
    /// of a call-chain field-access object (`helper().val`) when the
    /// callee returns a shared struct — without this, the field path
    /// falls through to the generic `StructValue` extract and silently
    /// loads `i64 0`. See bug #8 (call-chain field access on
    /// shared-struct return).
    pub(crate) fn_return_type_names: HashMap<String, String>,
    /// Function-name → inner `TypeExpr` of a borrow return (`-> ref T` /
    /// `-> mut ref T` ⇒ inner `T`). Lets the caller learn that a call
    /// result is a borrow so it can bind it as a ref-local (deref on use
    /// via `ref_params`) rather than treating the returned `ptr` as a
    /// value — the caller half of B-2026-06-07-5. Populated by
    /// `declare_function`.
    pub(crate) fn_ref_return_inner: HashMap<String, TypeExpr>,
    /// Function-name → inner-shared-name when the function returns
    /// `Option[shared T]`. Populated by `declare_function` from the
    /// return type's `Option[T]` generic arg when T is a known shared
    /// type. Read by the let-stmt handler's `Option[shared T]`
    /// detection to register an `RcDecOption` cleanup for untyped
    /// bindings whose RHS is a call (`let out = add_two_numbers(...)`).
    /// Closes the kata-bench retention gap (2026-05-17) for the
    /// inferred-annotation shape; the explicit-annotation shape
    /// (`let out: Option[ListNode] = ...`) reads the inner directly
    /// off the surface `TypeExpr`.
    pub(crate) fn_return_option_inner_shared: HashMap<String, String>,
    /// Function-name → full return `TypeExpr`. Populated by
    /// `declare_function`. Read by the let-stmt handler's oversized-enum
    /// boxing path (`boxed_enum_payload_variants`) for an *untyped* let whose
    /// RHS is a direct call (`let o = make_opt()`): the box drop needs the
    /// generic arg `T` of `Option[T]` / `Result[T, E]` to decide boxing and
    /// name the inner struct, which `fn_return_type_names` (bare segment only)
    /// can't supply. The annotated shape reads `T` off the `let`'s `ty`.
    /// docs/spikes/oversized-enum-payload.md §3.
    pub(crate) fn_return_type_exprs: HashMap<String, TypeExpr>,
    /// Per-callee effectfulness side-table — populated from
    /// `Program.callee_effectful` (set by the cli pipeline after effectcheck).
    /// Key: callable's canonical name (free fn `name`, assoc/method
    /// `Type.method`). Value: `true` iff the callee carries any of
    /// `reads`/`writes`/`sends`/`receives`. Read by `emit_branch_cancel_check`
    /// to skip the cooperative cancel atomic load when we can prove the
    /// callee is non-observably-effectful. Absent callees are treated as
    /// potentially effectful (fall back to the conservative MVP behavior).
    pub(crate) callee_effectful: HashMap<String, bool>,
    /// Compiler-driven inline hints (phase-11 Codegen Optimization). Maps a
    /// concrete user function's name to a heuristic `inlinehint` / `noinline`
    /// decision, computed once by `crate::inline_hints::compute` before the
    /// declaration pass and consulted by `emit_codegen_hint_attrs` only when
    /// the user wrote no explicit `#[inline]` hint (the user always wins).
    pub(crate) heuristic_inline_hints:
        std::collections::HashMap<String, crate::inline_hints::HeuristicHint>,
    /// Source OFFSETS of every expression-level mention of each identifier in
    /// the function body currently being compiled, recorded once at
    /// `compile_function_body` entry (B-2026-08-08-25 leg 1).
    ///
    /// The one consumer is `scrutinee_read_after_match`, which asks the
    /// liveness question the consuming-arm clone is gated on: is this
    /// scrutinee local READ AFTER the match that moves its payload out? A
    /// mention offset at or past the last arm body's end answers yes.
    ///
    /// Offsets rather than a bare count because the scrutinee's OWN mention is
    /// inside the match and must not count as a later read — the distinction
    /// between `match o { … }` (source dead, keep today's zero-cost transfer)
    /// and `match o { … } … match o { … }` (source live, needs the clone).
    ///
    /// Built from `bce_length_pin::block_all`, whose walk is exhaustive over
    /// `ExprKind` with no wildcard arm, so a new AST node breaks this at
    /// compile time instead of silently reading as "not mentioned" — which
    /// would take the fast path on a live source and dangle it again.
    /// Over-approximating is safe here (a needless clone costs one `malloc`);
    /// under-approximating reintroduces the use-after-free, so the walk's
    /// fail-closed discipline is load-bearing rather than incidental.
    pub(crate) fn_body_ident_mention_offsets: HashMap<String, Vec<usize>>,
    /// `#[track_caller]` slice 4: names of functions declared `#[track_caller]`
    /// that received the hidden caller-location parameter triple (populated in
    /// `declare_function`). A call site consults this to decide whether to
    /// append the `(file, line, col)` caller-location args. Empty for any
    /// program with no `#[track_caller]` functions, so the whole feature is
    /// inert by default.
    pub(crate) track_caller_fns: std::collections::HashSet<String>,
    /// Slice c-repl.B.4: free-fn names whose bodies should NOT be
    /// emitted in this module — only the LLVM `declare` (signature
    /// without body) is emitted, so the JIT resolves calls to these
    /// names against a previously-installed module in the same
    /// JITDylib. Used by `karac repl`'s cross-cell amortization
    /// pipeline so cell N+1 doesn't re-emit cell N's items. Empty
    /// in every other codegen entry point.
    pub(crate) declare_only_fns: std::collections::HashSet<String>,
}
