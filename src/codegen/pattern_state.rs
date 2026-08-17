//! Pattern-match lowering state.
//!
//! Tenth slice of the Phase-2 `Codegen` decomposition
//! ([`docs/spikes/state-decomposition-codegen-methodcall.md`]). Groups the
//! state `control_flow_match.rs` and `pattern_binding.rs` thread through a
//! `match` lowering:
//!
//! - the **scrutinee classification** flags that decide whether a leaf
//!   binding borrows or owns — is the scrutinee an elidable `ref` param,
//!   an owned param, a fresh owning temp, an `Option`/`Result`, a shared
//!   enum; does the arm only borrow; does the source retain an inline
//!   payload;
//! - the scrutinee's `Option`/`Result` payload slot and area, and its
//!   payload body sources;
//! - the per-binding type tables (type names, inner `TypeExpr`s, borrow
//!   modes) keyed by span;
//! - the enum hint for the scrutinee, the current variant's payload
//!   bindings, and the discarded-branch spans.
//!
//! Most of these are a *single* `match`'s working state rather than
//! program-wide tables — set on entry to a lowering and read by the arm
//! code. Grouping them is the first step toward making that scope
//! explicit; today they are flags on the god struct that nested matches
//! must save and restore by hand.
//!
//! Named `pattern_state` to avoid the sibling `pattern_binding.rs`, which
//! holds the behaviour this data feeds.
//!
//! Accessed as `self.pattern_state.<name>` from the sibling `impl Codegen`
//! modules.

use std::collections::{HashMap, HashSet};

use inkwell::values::FunctionValue;

use crate::ast::TypeExpr;

/// Working state for a `match` lowering plus the per-binding type tables.
pub(crate) struct PatternState<'ctx> {
    /// Branch expressions whose VALUE IS DISCARDED, keyed by the span of their
    /// condition / scrutinee — the span `compile_if` / `compile_if_let` /
    /// `compile_match` actually hold.
    ///
    /// Gates the arm-tail element clone in `deepcopy_owned_param_branch_tail`
    /// (B-2026-08-14-32). Cloning an arm's borrowed container element is right
    /// when a binding will own the result and a pure leak when the result is
    /// thrown away, and only the consumer knows which. Computed per function by
    /// `compute_discarded_branch_spans`, which documents the discarding
    /// positions and why the set is keyed by span rather than carried as a flag.
    pub(crate) discarded_branch_spans: rustc_hash::FxHashSet<crate::resolver::SpanKey>,
    /// Set by `compile_match` when the scrutinee is a borrow-returning
    /// call (`Map.get`, `Vec.first`, ...) — used by `bind_pattern_values`
    /// to suppress `track_vec_var` for the bound name, since the payload
    /// aliases the container's storage and the container's own cleanup
    /// already covers the buffer.
    pub(crate) pattern_binding_is_borrow: bool,
    /// B-2026-08-08-25 — set alongside `pattern_binding_is_borrow` when the
    /// scrutinee is a LIVE LOCAL owning an inline `Option`/`Result`
    /// `{ptr,len,cap}` payload and no arm moves that payload out
    /// (`scrutinee_is_readonly_inline_optres_local`). The source keeps
    /// ownership, so the inline-payload suppressors must NOT disarm it.
    ///
    /// This is a SEPARATE flag rather than a read of `pattern_binding_is_borrow`
    /// on purpose. That flag is also raised for borrow-call / borrowed-binding
    /// scrutinees at the `let…else` site, whose binding escapes into the
    /// enclosing scope by construction and must keep its unconditional
    /// suppression — keying the suppressors on the shared flag would silently
    /// change those paths too. This one is raised only by the classifier that
    /// proved the source survives.
    pub(crate) pattern_binding_source_retains_inline_payload: bool,
    /// Set by `compile_match` (B-2026-07-15-21 Part B) when the scrutinee is a
    /// bare identifier naming a param already in `rc_elide_ref_params` — i.e. a
    /// read-only, non-escaping borrowed `shared`/`Option[shared]` param whose
    /// payload `rc_elide.rs`'s condition 4 has proven projection-only. Read by
    /// `bind_pattern_values` to skip the Some-binding acquire + its scope-exit
    /// `RcDec` (the payload aliases the param, kept alive by the caller for the
    /// whole call, so the retain/release is a balanced no-op). Eliding it also
    /// removes the post-call release epilogue, letting LLVM's tailcallelim turn
    /// the tail recursion into a loop (the C/Rust structure).
    pub(crate) pattern_binding_scrutinee_is_elidable_param: bool,
    /// Set by `compile_match` when the scrutinee enum is the type-erased
    /// `Option` / `Result` (B-2026-06-13-13 residual A). Their inline / boxed
    /// payloads are owned by the dedicated `FreeInlineOptionPayload` /
    /// boxed-scrutinee cleanup, NOT a per-field `EnumDrop`, so a pattern-bound
    /// struct payload (`Some(h)`) must NOT get a `track_struct_var` — that would
    /// double-free against the Option's own free. Gates the user-struct arm of
    /// the pattern-binding struct-drop registration.
    pub(crate) pattern_binding_scrutinee_is_option_result: bool,
    /// B-2026-07-30-11 (boxed-payload bodies): true while binding a
    /// pattern whose scrutinee is a FRESH OWNING temp (a call, or a
    /// non-borrow method call like `v.pop()`). The boxed-payload
    /// bodies-only registration fires ONLY then — a BOUND scrutinee's
    /// moved-out payload already runs its body through the binding-side
    /// channel, and a second registration double-ran a mutating body
    /// (`self.buf.clear()` freed the buffer twice).
    pub(crate) pattern_binding_scrutinee_is_fresh_owning_temp: bool,
    /// B-2026-08-01-13 — true while binding a pattern whose scrutinee is an
    /// OWNED (by-value) param of the current function. Under the
    /// caller-retains convention the param holds the callee's entry copy;
    /// a payload bound out of it is a view whose Drop BODY belongs to the
    /// caller (its NLL / fresh-arg fire reads the original), so the
    /// pattern-binding registration goes MEMORY-ONLY (`track_struct_var`)
    /// instead of the body+memory wrapper — firing here too doubled the
    /// body on both backends (`match w { E2.B(r2) => … }` with `w: E2`).
    /// B-2026-08-12-2 — does the arm currently being bound only BORROW its
    /// payload bindings (no move-out to a call, a return, or another binding)?
    /// Per-ARM, unlike the scrutinee flags around it, because it is a property
    /// of the arm body rather than of the value being matched.
    pub(crate) pattern_binding_arm_only_borrows: bool,
    pub(crate) pattern_binding_scrutinee_is_owned_param: bool,
    /// B-2026-08-02-25 (match-arm leg) — the `(slot, walker)` of the armed
    /// `__karac_dropelems_opt_*` / `__karac_dropelems_res_*` payload-bodies
    /// action on a NAMED `Option`/`Result` scrutinee, sampled BEFORE the arm's
    /// suppressors run (which is also before `bind_pattern_values`), so it
    /// reports the pre-retraction state for every arm alike.
    ///
    /// `Some` means two things at once, and the registration needs both.
    /// (1) The source's walk is about to be RETRACTED by
    /// `suppress_optres_payload_bodies_for_match` — a binding sub-pattern
    /// consumes by definition — so for a heap-BOXED payload, which the inline
    /// registration declines on word count, the body would otherwise run
    /// nowhere. (2) It carries the SUBJECT the re-homed walk must use: the
    /// SOURCE's Option/Result slot, not the arm binding's reconstructed copy.
    /// That distinction is load-bearing — a boxed payload's memory stays owned
    /// by the box, so a Drop body that MUTATES a heap field (`self.buf.clear()`)
    /// must mutate the BOX's copy, the one the later box drop reads. Run
    /// against the binding's copy instead, the body frees the buffer and zeroes
    /// the copy's cap while the box keeps the stale `{ptr,len,cap}`, and the box
    /// drop frees it a second time.
    pub(crate) pattern_binding_scrutinee_payload_bodies_src:
        Option<(inkwell::values::PointerValue<'ctx>, FunctionValue<'ctx>)>,
    /// B-2026-08-04-2 — the scrutinee's `Option`/`Result` SLOT while binding a
    /// pattern: the named binding's own slot, or the staged
    /// `__freshtemp_boxed_scrut` alloca. Sibling of
    /// `pattern_binding_scrutinee_payload_bodies_src`, but tracked separately
    /// because that one exists only when the payload runs a user Drop, and this
    /// class is about MEMORY: a boxed payload's heap fields double-free when the
    /// binding moves, Drop impl or not.
    pub(crate) pattern_binding_scrutinee_optres_slot: Option<inkwell::values::PointerValue<'ctx>>,
    /// B-2026-07-10-3 — the inline payload-area word budget of the
    /// `Option`/`Result` scrutinee currently being compiled: 3 for `Option`,
    /// 5 for `Result`, 0 when the scrutinee is neither (the same fixed areas
    /// `coerce_to_payload_words` / `boxed_enum_payload_variants` pack with). A
    /// struct payload whose word count is ≤ this budget is held INLINE (not
    /// heap-boxed), so binding it whole can safely `track_struct_var` it to
    /// free its inner heap fields; a wider (boxed) payload is owned by the box
    /// drop and must be left untouched. Set/restored by `compile_match`
    /// alongside `pattern_binding_scrutinee_is_option_result`.
    pub(crate) pattern_binding_scrutinee_optres_area: usize,
    /// B-2026-06-14-31 — set by `compile_match` when the scrutinee enum is a
    /// user `shared enum` (RC-boxed). A struct payload bound in such an arm
    /// (`Wrapped(w)` from `shared enum Expr { Wrapped(Wrap) }`,
    /// `struct Wrap { items: Vec[Expr] }`) is a by-value VIEW of the box's
    /// inline payload words — its Vec/String buffer aliases the buffer the
    /// still-live RC box owns. The box's rc-drop walker
    /// (`emit_nested_struct_shared_rc_decs`) is the sole owner of that buffer
    /// and its elements, so the bound `w` must NOT get a `track_struct_var`,
    /// whose `__karac_drop_struct_<S>` would `free` the buffer prematurely and
    /// double-free against the box drop — silent on mac, a SEGV under the
    /// Linux LSan/ASAN gate. Peer of the Option/Result flag above. Note that a
    /// struct payload of ONLY shared fields, e.g. `BinOp { left, right }`, is
    /// already safe: it is not copy-supported and its drop fn is a no-op for
    /// shared fields — but a Vec/String field emits a real buffer-freeing drop
    /// fn, which is the gap this flag closes.
    pub(crate) pattern_binding_scrutinee_is_shared_enum: bool,
    /// #39 — the resolved enum type name of the match scrutinee currently being
    /// compiled (e.g. `Token` for `match self.tokens[i].token { … }`). An
    /// unqualified variant pattern (`Float(v, sfx)`) is resolved against THIS
    /// enum first, so a bare variant name that ALSO exists in another imported
    /// enum (`Expr.Float`) no longer mis-resolves to whichever enum the
    /// (unordered) `enum_layouts` map yields first. Set once at the top of
    /// `compile_match` from `type_name_of_expr(scrutinee)`, restored after the
    /// arm loop (nested matches save/restore). `None` when the scrutinee's
    /// enum can't be resolved statically, in which case the resolvers keep
    /// their prior user-vs-seed fallback.
    pub(crate) match_scrutinee_enum_hint: Option<String>,
    /// Per-pattern-binding surface type table — populated from
    /// `Program.pattern_binding_types` (set by the lowering pass from
    /// `TypeCheckResult.pattern_binding_types`). Key: pattern's
    /// `(span.offset, span.length)`. Value: canonical type name (e.g.
    /// `"MyError"`). Used in `bind_pattern_values` to reconstitute struct
    /// payloads from the i64 word when the surface binding type is a
    /// struct, so subsequent `.field` access dispatches through the right
    /// struct shape.
    pub(crate) pattern_binding_types: HashMap<(usize, usize), String>,
    /// Sibling to `pattern_binding_types` carrying the inner element
    /// `TypeExpr` for `Vec[T]` / `Slice[T]` pattern bindings only. Populated
    /// from `Program.pattern_binding_inner_types`. Read by
    /// `bind_pattern_values` to lower the inner element type to a
    /// `BasicTypeEnum` (via `llvm_type_for_type_expr`) and register it
    /// under the binding's variable name in `vec_elem_types` /
    /// `slice_elem_types`, so subsequent method-dispatch (`xs.len()` /
    /// `xs[0]` / `xs.push(...)`) on a pattern-bound `Vec` / `Slice` payload
    /// routes through the right element-typed path. PB sibling slice
    /// (2026-05-09).
    pub(crate) pattern_binding_inner_types: HashMap<(usize, usize), TypeExpr>,
    /// Per-leaf-binding borrow mode populated from
    /// `Program.pattern_binding_borrow_modes`. Consumed by
    /// `bind_pattern_values` (Binding arm) to wrap a value-typed leaf
    /// binding in a ref-shim — an extra `ptr` alloca holding the value
    /// alloca's address, registered in `ref_params` — so call sites
    /// expecting `ref T` / `mut ref T` receive the right ABI shape.
    /// Empty for owned bindings. Slice 3a, 2026-05-14.
    pub(crate) pattern_binding_borrow_modes:
        HashMap<(usize, usize), crate::ast::PatternBindingBorrow>,
    /// B-2026-07-30-11 (match-arm leg) — the binding names of the pattern
    /// currently being bound that sit in an enum VARIANT payload position
    /// (`collect_variant_payload_binding_names`; bare-tuple elements excluded
    /// — a tuple scrutinee's element walk stays armed and must remain the
    /// single body owner). `bind_pattern_values` consults this to route a
    /// Drop-declaring payload struct to the UserDrop channel instead of
    /// StructDrop; set/cleared around each bind call by the match / if-let
    /// compilers. Empty everywhere else, so `let` destructures are untouched.
    pub(crate) current_variant_payload_bindings: HashSet<String>,
}
