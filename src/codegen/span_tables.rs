//! Span-keyed side tables handed off from upstream phases.
//!
//! The `(span.offset, span.len)`-keyed maps and sets the LOWERING pass
//! copies out of the typechecker's `Program` side tables (typed-expr
//! classifications, method dispatch and callee types, `?`-conversion and
//! payload types, temp-receiver and iterator-terminal element types,
//! call-site type substitutions, unsigned classifications) and the
//! ownership pass's span verdicts (`uam_*` use-after-move sites,
//! `vec_index_*` borrow/clone whitelists). Codegen only READS these during
//! body emission — with one systematic exception: the tracing-stdlib
//! window (`compile_tracing_stdlib_methods`) swaps its own lowered
//! tables in and back out around emitting `tracing.kara` bodies.
//! Extracted from `Codegen` as a cluster-15 sub-slice of the
//! state-decomposition spike; see
//! `docs/spikes/state-decomposition-codegen-methodcall.md`.

use std::collections::{HashMap, HashSet};

use crate::ast::TypeExpr;
use crate::resolver::SpanKey;

pub(crate) struct SpanTables {
    /// Cross-error-type conversion targets at `?` sites — populated from
    /// `Program.question_conversions` (set by the lowering pass from the
    /// typechecker's `question_conversions` map). Key: `(span.offset,
    /// span.length)` of the `?` expression. Value: target type name (e.g.
    /// `"AppError"`). When present, `compile_question` emits `Target.from(e)`
    /// against the inner err payload before the propagation early-return.
    pub(crate) question_conversions: HashMap<(usize, usize), String>,
    /// `?` span → unwrapped Ok/Some payload `TypeExpr` — populated from
    /// `Program.question_ok_payload_types`. `reconstruct_question_ok_payload`
    /// reads it to rebuild a multi-word Ok payload of any shape (including a
    /// genuine nested `Option[T]`/`Result[T,E]` payload) without the
    /// span-collision wrapper ambiguity of `enum_inst_type_exprs`
    /// (B-2026-07-13-19).
    pub(crate) question_ok_payload_types: HashMap<(usize, usize), TypeExpr>,
    /// `with_provider`-call `let`-RHS result types (from
    /// `Program.wp_result_types`, lowering-pass derived). The Let arm reads
    /// an entry as an implicit type annotation so a heap-typed wp-result
    /// binding registers its method-dispatch metadata (B-2026-07-31-20).
    pub(crate) wp_result_types: HashMap<(usize, usize), TypeExpr>,
    /// Per-method-call → `Type.method` callee key side-table — populated
    /// from `Program.method_callee_types` (set by the lowering pass from
    /// `TypeCheckResult.expr_types`). Key: `(span.offset, span.length)` of
    /// the `MethodCall` expression. Value: canonical `Type.method` string
    /// usable as a lookup into `callee_effectful`. Lets
    /// `compile_method_call` apply the same narrowing that `compile_call`
    /// applies to free-function and `Type.assoc` calls.
    pub(crate) method_callee_types: HashMap<(usize, usize), String>,
    /// B-2026-08-13-8 — qualified dispatch segments for impls whose head name is
    /// not a unique identity (`impl Zero for Vec[i64]` alongside `impl Zero for
    /// Vec[String]`), keyed by the impl target's span. Computed here from the
    /// program AST via the same shared helper the interpreter and the
    /// typechecker call, so the symbol this module EMITS and the key those
    /// phases expect cannot drift. Drives emission only.
    pub(crate) impl_dispatch_names: crate::impl_dispatch::ImplDispatchNames,
    /// The dispatch half of the same row, snapshotted from
    /// `Program.method_impl_dispatch`: per call site, the resolved impl's
    /// qualified segment. Codegen cannot recompute it — `inferred_receiver_type`
    /// only ever yields a head name — so the typechecker, which compared
    /// `target_args` vector-wise at check time, supplies the winner.
    pub(crate) method_impl_dispatch: HashMap<((usize, usize), String), String>,
    /// Phase 6 line 26 slice 8ab: per-call-site effect-variable
    /// substitutions, snapshotted from `Program.call_effect_subs`
    /// (which `cli.rs::Pipeline` populates from
    /// `EffectCheckResult.call_effect_subs` via
    /// `build_call_effect_subs_table`). Slice 8y (entry 32) reads
    /// this in `compile_generic_call` to gate per-mono state-machine
    /// emission on whether the resolved per-call effects include any
    /// network-yield verb. Empty when effectcheck didn't run or no
    /// polymorphic-effect callees exist.
    pub(crate) call_effect_subs: crate::ast::CallEffectSubsTable,
    /// Per-`unwrap`/`expect`/`is_*` MethodCall → inner `TypeExpr` side-
    /// table — populated from `Program.method_unwrap_inner_types` (set by
    /// the lowering pass from `TypeCheckResult.method_unwrap_inner_types`).
    /// Key: `(span.offset, span.length)` of the MethodCall expression.
    /// Value: the `T` of `Option[T]` (or success-`T` of `Result[T, E]`).
    /// Codegen's `unwrap` arm uses this to lower the inner type to its
    /// LLVM shape and reconstitute the payload words back to a value.
    pub(crate) method_unwrap_inner_types: HashMap<(usize, usize), TypeExpr>,
    /// ERR (`E`) sibling of `method_unwrap_inner_types` — the Result forms of
    /// the absent-closure combinators (`unwrap_or_else`/`map_or_else`/
    /// `or_else`, B-2026-07-14-6) reconstruct the `Err` value at this type to
    /// feed their closure. Same keying (MethodCall span).
    pub(crate) method_unwrap_err_types: HashMap<(usize, usize), TypeExpr>,
    /// Per-fresh-temp `Vec`/`VecDeque` receiver read-method (`get`/`first`/
    /// `last`/`get_unchecked`/`contains`) MethodCall → scalar element
    /// `TypeExpr` side-table — populated from `Program.temp_recv_elem_types`.
    /// Key: `(span.offset, span.length)` of the MethodCall. Codegen
    /// materializes the non-identifier receiver into a synth local, registers
    /// this element type, and re-dispatches through `compile_vec_method`
    /// (general-owned-temp-tracking spike, slice 3b).
    pub(crate) temp_recv_elem_types: HashMap<(usize, usize), TypeExpr>,
    /// Per fresh-temp `Vec` receiver of `len`/`is_empty`/`count` → the
    /// receiver's heap-bearing element `TypeExpr` — populated from
    /// `Program.temp_recv_len_elem_types`. The intercept's drop-track uses it
    /// to walk the elements instead of freeing only the outer buffer
    /// (B-2026-07-31-43). Separate from `temp_recv_elem_types` on purpose: at
    /// a span-collided chain the two tables describe different receivers.
    pub(crate) temp_recv_len_elem_types: HashMap<(usize, usize), TypeExpr>,
    /// Per numeric iterator-terminal MethodCall (`Iterator.sum()` /
    /// `Iterator.reduce(f)`) → yielded element `TypeExpr` side-table —
    /// populated from `Program.iter_terminal_elem_types`. Key:
    /// `(span.offset, span.length)` of the MethodCall. `try_compile_iter_chain_sum`
    /// reads it to seed the fused-loop accumulator with a correctly-typed zero
    /// so `acc = acc + x` type-checks for every numeric width (B-2026-07-11-19).
    pub(crate) iter_terminal_elem_types: HashMap<(usize, usize), TypeExpr>,
    /// Per `Iterator.fold(init, f)` MethodCall → accumulator `TypeExpr`
    /// side-table — populated from `Program.iter_terminal_acc_types`. Key:
    /// `(span.offset, span.length)` of the MethodCall. `try_compile_iter_chain_fold`
    /// reads it to stamp a type annotation on the synthetic accumulator `let`,
    /// so a heap (`String`/`Vec`) accumulator registers as a tracked binding and
    /// the Assign move-machinery fires instead of double-freeing (B-2026-07-13-18).
    pub(crate) iter_terminal_acc_types: HashMap<(usize, usize), TypeExpr>,
    /// `Stats.<fn>` call-span -> slice element `TypeExpr` (`i64` | `f64`),
    /// from `Program.stats_elem_types` (S5). Missing entry = `f64`.
    pub(crate) stats_elem_types: HashMap<(usize, usize), TypeExpr>,
    /// Set of `(span.offset, span.length)` keys for every expression whose
    /// Kāra type is `String`. Populated from `Program.string_typed_exprs`
    /// (which the lowering pass derives from `TypeCheckResult.expr_types`).
    /// Lets codegen distinguish `String` from `Vec[T]` and other 3-word
    /// `{ptr, i64, i64}` types whose LLVM struct shape is identical.
    /// First consumer: `emit_sort_by_key_inline_thunk`'s String-key
    /// dispatch arm — `String` and `Vec[u8]` are indistinguishable from
    /// the LLVM value alone, so the span-set is what tells them apart.
    pub(crate) string_typed_exprs: HashSet<(usize, usize)>,
    /// Spans of every expression typed `Ref`/`MutRef` of a `Vec`/`VecDeque`/
    /// `Slice` (from `Program.borrow_vec_typed_exprs`). The Let path consults
    /// it so a whole-collection re-borrow (`let ps = params`, `params: ref
    /// Vec[T]`) binds `ps` as an alias with no scope-exit free instead of a
    /// second owner that double-frees the container's buffer (B-2026-07-18-4).
    pub(crate) borrow_vec_typed_exprs: HashSet<(usize, usize)>,
    /// Spans of every `Iterator[..]`-typed expression (from
    /// `Program.iterator_typed_exprs`) — the sound gate for materializing an
    /// iterator-let binding (B-2026-07-11-19).
    pub(crate) iterator_typed_exprs: HashSet<(usize, usize)>,
    /// Per-expression `Fn(..)` / `OnceFn(..)` signature (as a `FnType`
    /// TypeExpr), from `Program.fn_value_typed_exprs` (lowering pass, from
    /// `TypeCheckResult.expr_types`). Keyed by the expression's
    /// `(span.offset, span.length)`. Lets `let_binding_fn_value_type` register
    /// an un-annotated fn-value binding (`let g = h.f;`) in `closure_fn_types`
    /// so `g(x)` lowers to an indirect call (B-2026-06-21-3).
    pub(crate) fn_value_typed_exprs: HashMap<(usize, usize), TypeExpr>,
    /// Per-generic-call-site resolved type-arg substitution
    /// (`{ formal-param-name -> concrete-type-name }`), keyed by the call
    /// expression's `(span.offset, span.length)`. From
    /// `Program.call_type_subs` (lowering pass, from
    /// `TypeCheckResult.call_type_subs`). `compile_generic_call` consults it
    /// to bind type params the LLVM-type-based `infer_type_args` can't — a
    /// container element type (`ref Vec[T]`) is element-erased in its
    /// `{ptr,len,cap}` LLVM shape, so two element instantiations would share
    /// one monomorph without this (B-2026-07-02-41). Concrete names resolve
    /// through the active `type_subst` (via `llvm_type_for_name`), so a
    /// nested generic call inside a mono flattens transitively.
    pub(crate) call_type_subs: HashMap<(usize, usize), HashMap<String, String>>,
    /// Element-aware mono-mangle tokens per call site (`T` → `"Vec_i64"` /
    /// `"Vec_String"` / `"String"`), the sibling of `call_type_subs` (head-only).
    /// Consulted by `compile_generic_call` to give a generic fn's mono a distinct
    /// symbol per builtin-collection whole-type-param instantiation — String /
    /// Vec[i64] / Vec[String] all lower to `{ptr,i64,i64}` and would otherwise
    /// collide on one `$struct` symbol, sharing an element-erased body
    /// (B-2026-07-11-35 return-owned-param leg).
    pub(crate) call_type_subs_mangle: HashMap<(usize, usize), HashMap<String, String>>,
    /// B-2026-08-14-38 — the `Vec[T]` / `VecDeque[T]` `TypeExpr` of an `Index`
    /// RECEIVER that is a method call (`Program.index_recv_vec_types`). The Vec
    /// twin of `tensor_index_recv_types`: same key, same collision (the parser
    /// stamps a postfix expression with its receiver's span, so `expr_types`
    /// holds the index's ELEMENT type there). `compile_index` reads it to
    /// materialize the nameless temporary into a synth Vec local and lower the
    /// read through the identifier-keyed Vec path.
    pub(crate) index_recv_vec_types: HashMap<(usize, usize), TypeExpr>,
    /// Set of `(span.offset, span.length)` keys for every expression whose
    /// Kāra type is a `Vector[T, N]` with an unsigned-integer element.
    /// Populated from `Program.unsigned_vector_exprs`. The LLVM `<N x iX>`
    /// lane type is signless, so `compile_vector_method`'s `reduce_min`/
    /// `reduce_max` arm consults this (keyed by the receiver-vector span)
    /// to pick the unsigned compare predicate (`ult`/`ugt`) over the signed
    /// default. Shared infra for the slice-3 mask comparisons.
    pub(crate) unsigned_vector_exprs: HashSet<(usize, usize)>,
    /// B-2026-08-14-3 — scalar sibling of `unsigned_vector_exprs`. Populated
    /// from `Program::unsigned_int_exprs`; see that field for why the
    /// syntactic walk in `expr_is_unsigned_int` needs a fallback and why this
    /// one is consulted last.
    pub(crate) unsigned_int_exprs: HashSet<(usize, usize)>,
    /// B-2026-08-14-3 — spans of `x as T` whose SOURCE is unsigned. See
    /// `Program::cast_source_unsigned` for why the span-keyed type table
    /// cannot answer this.
    pub(crate) cast_source_unsigned: HashSet<(usize, usize)>,
    /// Spans of `Vector[T, N]` INSTANCE-METHOD calls, from
    /// `Program.vector_method_call_spans`. The vector dispatch in
    /// `compile_method_call` consults this when the span-keyed
    /// `method_callee_types` entry has been clobbered by an outer chain link
    /// (`v.reduce_sum().to_string()` — B-2026-07-29-7). Presence-only; the
    /// method name still has to be in the Vector instance set.
    pub(crate) vector_method_call_spans: HashSet<(usize, usize)>,
    /// Sibling to `string_typed_exprs`: for every expression whose Kāra
    /// type is a `Named` struct, the canonical struct name. Populated
    /// from `Program.expr_struct_type_names`. Lets codegen recover the
    /// source-level struct identity from a value alone — the LLVM struct
    /// type doesn't carry the name back — so `emit_sort_by_key_inline_thunk`
    /// can look up per-field type names via `struct_field_type_names` and
    /// dispatch the right per-field comparator (int / String) when the
    /// key is a struct with mixed-type fields.
    pub(crate) expr_struct_type_names: HashMap<(usize, usize), String>,
    /// Sibling to `expr_struct_type_names`: for every expression whose
    /// Kāra type is a struct with a user-supplied `impl Ord for T`, maps
    /// span → canonical `"Type.cmp"` callee key. Populated from
    /// `Program.user_ord_typed_exprs`. `emit_sort_by_key_inline_thunk`
    /// consults this map before the field-aware cascade so the user's
    /// `cmp` runs instead of a synthesized derive-equivalent lex compare.
    pub(crate) user_ord_typed_exprs: HashMap<(usize, usize), String>,
    /// Pointee surface `TypeExpr` per raw-pointer-typed (`*const T` / `*mut T`)
    /// expression, keyed by span — populated from
    /// `Program.raw_pointer_pointee_types`. The unary-deref arm keys this by the
    /// operand span to `load` through a raw pointer (whose value is the address)
    /// instead of returning the address; references are absent and take the
    /// pass-through path.
    pub(crate) raw_pointer_pointee_types: HashMap<(usize, usize), TypeExpr>,
    /// Arg-less (concrete, non-generic) `Named` type per expression span — the
    /// complement of `enum_inst_type_exprs`. Consumed ONLY by
    /// `reconstruct_question_ok_payload` to rebuild a multi-word concrete
    /// enum/struct `?`-Ok payload the generic-only table drops (B-2026-07-11-7).
    pub(crate) concrete_named_type_exprs: HashMap<(usize, usize), TypeExpr>,
    /// Borrow-elision for read-only `let r = v[i]` indexed-element bindings
    /// (B-2026-06-19-6, clone-elision). Per-function set of the RHS index
    /// expression's `SpanKey` for each `let r = v[i]` whose binding `r` is
    /// provably read-only and non-escaping AND whose container `v` is not
    /// mutated within `r`'s lexical scope — computed by the conservative
    /// whitelist scan `compute_vec_index_borrow_spans` at `compile_function`
    /// entry. At such a let site the heap-element deep-clone
    /// (`clone_owned_vec_index_element`) is skipped — `r` aliases the
    /// container element — and the binding's scope-exit `track_vec_*`
    /// (FreeVecBuffer + recursive element drop) is suppressed, since the
    /// container stays the unique owner. Recomputed (overwritten) per fn.
    pub(crate) vec_index_borrow_spans: HashSet<SpanKey>,
    /// B-2026-08-14-15 leg A — the RHS `SpanKey` of every index-read that
    /// `clone_owned_vec_index_element` actually deep-cloned. The clone makes the
    /// destination the owner of a FRESH value, but the `let` site's Map/Set
    /// cleanup arm keys on the RHS *shape* (`Call` / `.clone()` / …) and reads a
    /// bare `v[i]` as a caller-retains ALIAS — correct when the clone is elided,
    /// a leak of the whole cloned Map control block when it is not. Recording the
    /// emission (rather than re-deriving it from `!borrow_elided`) keeps the two
    /// sides exact: the clone self-gates on element copyability and on the read
    /// value's LLVM type, so "not borrow-elided" is strictly wider than "cloned".
    /// Accumulates across the module; `SpanKey`s are source-unique.
    pub(crate) vec_index_cloned_sites: HashSet<SpanKey>,
    /// Per-variable element-`TypeExpr` side-table for collection variables —
    /// the *element* of a Vec/Slice/Array, or the *value* of a Map. Used by
    /// `compile_for_*_var` so for-loop bindings inherit the right side-table
    /// registrations (`vec_elem_types`, `slice_elem_types`, `map_*_types`)
    /// when the element is itself a Vec/String/Slice/Map. Without this,
    /// LLVM-type-only tracking can't distinguish `Vec[String]` from
    /// `Vec[Vec[T]]` (both store `vec_struct_type` as the element LLVM type).
    /// B-2026-08-10-21 — span keys of the CONSUME sites the ownership pass
    /// reported a `UseAfterMove` for, straight from
    /// `OwnershipCheckResult::use_after_move_consume_sites`.
    ///
    /// `UseAfterMove` is non-fatal for `build` on the documented promise that
    /// "codegen defensive-copies the reuse, so the binary is memory-safe"
    /// (`cli.rs`'s `is_fatal_ownership_kind`). This is the set that makes the
    /// promise true. Two consumers, and BOTH are required at every flagged
    /// site:
    ///
    ///   * the identifier load deep-copies the moved value, so the consumer
    ///     gets its own buffer;
    ///   * `suppress_source_vec_cleanup_for_arg_ex` skips the source disarm, so
    ///     the source keeps its own buffer AND its own cleanup.
    ///
    /// A copy without the disarm-skip leaks the source; a disarm-skip without
    /// the copy turns the use-after-free into a double free. Empty for any
    /// program the ownership pass reported no `UseAfterMove` on, which is the
    /// overwhelming majority — nothing changes for them.
    pub(crate) uam_consume_sites: std::collections::HashSet<(usize, usize)>,
    /// B-2026-08-10-21 — the flagged sites at which a defensive copy was
    /// ACTUALLY emitted, recorded by `uam_defensive_copy`.
    ///
    /// The disarm skip keys on THIS set, not on `uam_consume_sites`, and the
    /// distinction is the whole safety argument. The copy currently covers the
    /// `{ptr,len,cap}` family only; skipping the disarm at a site where no copy
    /// was made leaves two owners of one buffer — measured as
    /// `free(): double free detected in tcache 2` on a struct-typed move, which
    /// is strictly worse than the wrong-output it replaced. Keying on
    /// "a copy happened" makes the pair inseparable by construction, so
    /// widening the copy to more types can never get out of step with the
    /// disarm.
    pub(crate) uam_copied_sites: std::collections::HashSet<(usize, usize)>,
}
