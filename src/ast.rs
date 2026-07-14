// src/ast.rs

//! Abstract Syntax Tree definitions for the Kāra language.
//! Every node carries a `Span` for source location tracking.

use crate::token::Span;

/// Three-level visibility per `design.md § Three-level visibility`.
/// Items carry `is_pub: bool` and `is_private: bool`; this enum is the
/// single-value view used by the resolver / typechecker when enforcing
/// cross-module access rules (CR-24 slice 6). Exactly one of the two
/// bools may be true; both false means `Default` (project-internal).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    /// Marked `pub` — visible to end users and all project files.
    Pub,
    /// No visibility keyword — project-internal (visible to all files in
    /// the package, not to external consumers).
    Default,
    /// Marked `private` — visible only within the same directory.
    Private,
}

impl Visibility {
    /// Build a Visibility from the two transitional booleans. Callers that
    /// violate the "at most one true" invariant get `Pub` as the safe fallback
    /// — parser validation should have rejected the combination earlier.
    pub fn from_flags(is_pub: bool, is_private: bool) -> Self {
        if is_pub {
            Visibility::Pub
        } else if is_private {
            Visibility::Private
        } else {
            Visibility::Default
        }
    }

    pub fn is_pub(self) -> bool {
        matches!(self, Visibility::Pub)
    }

    pub fn is_private(self) -> bool {
        matches!(self, Visibility::Private)
    }
}

// ── Program ──────────────────────────────────────────────────────

/// Side-table populated by `lowering::lower_program` from the typechecker's
/// `TypeCheckResult.question_conversions`. Maps each `?` expression's span
/// (offset, length as a `(usize, usize)` tuple) to the fully-qualified name
/// of the target error type when a `From`-based conversion must run before
/// propagation. Used by codegen to emit `Target.from(e)` ahead of the early
/// return; see `src/codegen.rs:compile_question`.
pub type QuestionConversionTable = std::collections::HashMap<(usize, usize), String>;

/// Side-table populated by the cli pipeline from `EffectCheckResult`. Maps
/// each callable's canonical name (free fn `name`, assoc/method `Type.method`)
/// to whether its inferred or declared effects include any of the four
/// "side-effect-bearing" verbs — `reads`, `writes`, `sends`, `receives`.
/// Read by codegen at par-branch call sites: a callee marked `false` skips
/// the cooperative cancel-check atomic load; absent or `true` callees fall
/// back to the conservative "always fire" behavior. See design.md
/// § Effect-boundary cooperative cancellation.
pub type CalleeEffectfulTable = std::collections::HashMap<String, bool>;

/// Side-table populated by the cli pipeline from `EffectCheckResult`. Maps
/// each callable's canonical name to whether its effect set carries a
/// `sends(Network)` or `receives(Network)` verb-resource pair — the only
/// effects that route through the network event loop's non-blocking
/// park-and-yield path at v1. Other suspending effects (`Receiver.recv` via
/// `suspends`, custom user `suspends`, future channel waits) stay
/// thread-blocking and are NOT marked. Consumed by the state-machine
/// transform codegen (phase 6 line 26) to identify which functions need
/// the transform, and by codegen at network-effect call sites (phase 6
/// line 17 sub-item 6) to identify which call boundaries lower to "register
/// fd + park + yield" instead of a synchronous call. `Polymorphic` and
/// `PolymorphicWithFixed` declared-effect callees are conservatively marked
/// `true` because a monomorphization may bind their effect parameter to a
/// network-bearing effect; the transform itself reads the resolved
/// monomorphized effect set when deciding to apply.
pub type CalleeNetworkYieldEffectTable = std::collections::HashMap<String, bool>;

/// Side-table mirroring `EffectCheckResult.call_effect_subs` (slice 8aa).
/// Maps a call-expression's `(offset, length)` span key to a per-call-site
/// effect-variable substitution: each entry binds an effect-variable name
/// (the `E` in `with E`) to the set of `(verb, resource)` effects resolved
/// by `compute_call_var_bindings` at that call site. Empty inner set
/// indicates `E` resolved to ⊥ (no effect-bearing Fn-arg slots); absence of
/// an outer entry indicates the call site has no polymorphic-effect
/// callee. Slice 8ab (entry 35) populates this from the effectchecker's
/// `call_effect_subs` field; slice 8y (entry 32) consumes it to gate
/// state-machine emission on whether the resolved per-call effects
/// include any network-yield verb.
///
/// Encoded as a plain `(usize, usize)` key pair (offset + length) so the
/// table stays inkwell-free, matching `MethodCalleeTypesTable`'s
/// discipline (codegen containment invariant). Inner `EffectKey` carries
/// the verb's discriminant index + resource name so it round-trips
/// through codegen without dragging the `Effect` struct's lifetime
/// parameters.
pub type CallEffectSubsTable =
    std::collections::HashMap<(usize, usize), std::collections::HashMap<String, Vec<EffectKey>>>;

/// Plain-data encoding of an `Effect` for cross-phase tables. Mirrors
/// `karac::effectchecker::Effect` but omits the lifetime / `TracedEffect`
/// wrapper so it can be cloned into AST-side tables without dragging
/// effect-checker internals into the codegen layer.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EffectKey {
    /// Effect verb's display name (`"reads"`, `"writes"`, `"sends"`,
    /// `"receives"`, `"allocates"`, `"panics"`, `"blocks"`, `"suspends"`).
    pub verb: String,
    /// Resource name (`""` for execution verbs that take no resource).
    pub resource: String,
}

/// Phase 6 line 26 slice 8y: set of callee names whose declared
/// effects are `DeclaredEffects::Polymorphic` only — purely `with E`
/// (or `with _`) with no static fixed portion. Used by codegen's
/// per-mono state-machine gating to decide whether
/// `CallEffectSubsTable` alone is authoritative for the call's
/// resolved-effect classification. See
/// `Program.callee_purely_polymorphic_effects` for the full
/// rationale.
pub type CalleePurelyPolymorphicEffectsSet = std::collections::HashSet<String>;

/// One yield-point entry within a network-boundary function: the call site
/// where execution suspends pending I/O readiness, paired with the
/// resolved callee key (`Identifier(name) → name`, two-segment
/// `Type.method` Path → joined, `MethodCall` resolved via
/// `TypeCheckResult.method_callee_types`). The state-machine transform
/// (phase 6 line 26) consumes the per-function vector to size the state
/// struct (one tag per yield point), and the codegen lowering pass
/// consumes the per-yield-point callee key to identify which network
/// runtime FFI helper to call at each yield site.
#[derive(Debug, Clone)]
pub struct YieldPoint {
    /// Resolved callee key — same shape as `CalleeNetworkYieldEffectTable`
    /// keys (`name`, `Type.method`). The state-machine transform looks
    /// this up to determine the parking convention at the call boundary.
    pub callee: String,
    /// Span of the call expression (the `MethodCall` or `Call` node, not
    /// the callee identifier). Used to thread debugger metadata through
    /// the state-machine transform — `WaitTarget.NetworkIo` per the
    /// debugger contract carries this span so `list_tasks()` can show
    /// the source-level yield site, identical to a thread-blocking
    /// syscall's stack frame.
    pub span: Span,
    /// V1 conservative over-approximation of the locals that the
    /// state-machine transform must preserve across this suspension —
    /// every binding lexically in scope at the yield site (function
    /// parameters + every `let` / `let-else` / `for`-loop / pattern
    /// binding introduced earlier in source order that hasn't gone out
    /// of scope). Names are listed in introduction order — params first
    /// (left-to-right), then per-block let-binding sequence. The
    /// captures-union packed-across-non-overlapping-live-ranges
    /// optimization (per design.md § State-Machine Transform) is a later
    /// refinement; v1 codegen packs every entry unconditionally.
    /// Closures are NOT descended into during the walk — a yield point
    /// inside a closure body is the closure's own state machine, not
    /// the enclosing function's. Empty when slice 3 hasn't run (e.g.
    /// before phase 6 line 26 slice 3's pipeline pass).
    pub captured_locals: Vec<String>,
}

/// Side-table populated by the cli pipeline after `EffectCheckResult` and
/// `CalleeNetworkYieldEffectTable` are available. Maps each
/// network-boundary function's canonical name to the ordered list of
/// yield points within its body (in source-traversal order). Functions
/// without any yield-point calls — even network-boundary ones reaching the
/// classification through their own emitted `sends(Network)` /
/// `receives(Network)` effect at the FFI primitive layer rather than via
/// a sub-call — have no entry. Consumed by:
///   - the state-machine transform codegen (phase 6 line 26) — one state
///     per entry sizes the function's poll-function switch arm count;
///   - the live-range pass that computes the captured-locals union per
///     yield point — needs the yield-point spans to define the
///     suspension-boundary set.
pub type YieldPointsTable = std::collections::HashMap<String, Vec<YieldPoint>>;

/// One field in a network-boundary function's state struct: a binding
/// from the function body (parameter, `let`, pattern binding) that the
/// state-machine transform must preserve across at least one of the
/// function's yield points. Slice 4 of phase 6 line 26: the v1 layout
/// is the union of every yield point's captured-locals set, in
/// source-introduction order, with no overlap optimization.
#[derive(Debug, Clone)]
pub struct StateStructField {
    /// Source-level binding name (matches the names in
    /// `YieldPoint.captured_locals`).
    pub name: String,
    /// Surface type name as recorded by the typechecker's
    /// `pattern_binding_types` map at this binding's pattern span.
    /// `None` when the typechecker did not record a name there: at v1
    /// this covers primitive-typed bindings (`i64`/`bool`/`u8`/...),
    /// anonymous-tuple shapes the recorder skips, and bindings whose
    /// pattern span was not threaded into `pattern_binding_types`
    /// (e.g. `let-uninit` and slice-pattern rest bindings — neither
    /// passes through `bind_pattern_types`). Codegen consults this
    /// name plus the sibling `pattern_binding_inner_types` table to
    /// materialize the LLVM shape; `None` entries fall through to the
    /// existing primitive-sizing path.
    pub type_name: Option<String>,
    /// Span of the binding's introducing pattern — the `let` /
    /// parameter / match-arm binding position the user wrote. Used by
    /// `raii_check` to anchor a "binding declared here" secondary
    /// highlight at the source position the user needs to act on
    /// (release the binding, or `impl CancelSafe` for its type).
    /// `None` for `self` (no source-level pattern; receiver shape lives
    /// in the impl signature) and for synthetic bindings without a
    /// recorded pattern span. Codegen ignores this field — it's
    /// diagnostic-only.
    pub binding_span: Option<crate::token::Span>,
}

/// State-struct layout synthesized per network-boundary function. The
/// `fields` list is the union of every yield point's captured-locals
/// set within the function body, in source-introduction order
/// (parameters first left-to-right, then per-block let-binding sequence;
/// the first occurrence of a name across yield points fixes its
/// position). Slice 4 of phase 6 line 26 produces this conservative
/// over-approximation layout; a later slice may refine to per-yield
/// non-overlapping live ranges per design.md § State-Machine Transform.
#[derive(Debug, Clone)]
pub struct StateStructLayout {
    pub fields: Vec<StateStructField>,
}

/// Side-table populated by the cli pipeline after `Program.yield_points`
/// is built. Maps each network-boundary function with at least one
/// concrete yield point in its body to a `StateStructLayout`. Functions
/// classified network-boundary by Polymorphic declared-effect candidacy
/// without any actual sub-call yield points (FFI-primitive-emitting
/// shape) have no entry — matches `YieldPointsTable`'s presence rule.
/// Consumed by the state-machine transform codegen to size and lower
/// the state struct one-per-function-instantiation.
pub type StateStructLayoutTable = std::collections::HashMap<String, StateStructLayout>;

/// Side-table populated by the lowering pass from the typechecker's
/// `expr_types` map. Maps each `MethodCall` expression's span to the
/// canonical `Type.method` callee key — the same shape used in
/// `CalleeEffectfulTable`. Codegen consults this table at method-call
/// sites in par branches so the cooperative cancel-check narrowing
/// applies to instance methods, not just free-function / `Type.assoc`
/// calls.
pub type MethodCalleeTypesTable = std::collections::HashMap<(usize, usize), String>;

/// Side-table populated by the lowering pass from the typechecker's
/// `method_unwrap_inner_types` map. Maps each `unwrap`/`expect`/`is_*`
/// `MethodCall` expression's span to the inner `T` (for `Option[T]`) or
/// success-`T` (for `Result[T, E]`) `TypeExpr`. Codegen consults this
/// table in the `compile_method_call` arm for those methods to know
/// the LLVM shape of the value to reconstitute from the Option/Result
/// payload words.
pub type MethodUnwrapInnerTypesTable = std::collections::HashMap<(usize, usize), TypeExpr>;

/// Side-table populated by the lowering pass from the typechecker's
/// `temp_recv_elem_types` map. Maps each fresh-temp `Vec`/`VecDeque` receiver
/// read-method (`get`/`first`/`last`/`get_unchecked`/`contains`) `MethodCall`
/// span to the receiver's scalar element `TypeExpr`. Codegen consults it to
/// materialize the temp + register the element type before re-dispatching
/// through `compile_vec_method` (general-owned-temp-tracking spike, slice 3b).
pub type TempRecvElemTypesTable = std::collections::HashMap<(usize, usize), TypeExpr>;

/// Side-table populated by the lowering pass from the typechecker's
/// `iter_terminal_elem_types` map. Maps each numeric `Iterator.sum()` /
/// `Iterator.reduce(f)` terminal `MethodCall` span to the yielded element
/// `TypeExpr`. Codegen seeds the fused loop's accumulator with a width-correct
/// `(0 as <elem>)` zero (B-2026-07-11-19).
pub type IterTerminalElemTypesTable = std::collections::HashMap<(usize, usize), TypeExpr>;

/// Side-table populated by the lowering pass from the typechecker's
/// `iter_terminal_acc_types` map. Maps each `Iterator.fold(init, f)` terminal
/// `MethodCall` span to the accumulator `TypeExpr`. Codegen stamps it as the
/// type annotation on the synthetic accumulator `let`, so a heap accumulator
/// registers as a tracked `String`/`Vec` and its move-machinery fires
/// (B-2026-07-13-18).
pub type IterTerminalAccTypesTable = std::collections::HashMap<(usize, usize), TypeExpr>;

/// Sibling of `TempRecvElemTypesTable` for `Map`/`Set` fresh-temp receivers
/// (`make_map().get(k)`, `make_set().contains(x)`): `MethodCall` span → the
/// receiver's whole `Map[K, V]` / `Set[T]` `TypeExpr`. Codegen materializes the
/// handle, registers K/V (or elem) for the redispatch through
/// `compile_map_method` / `compile_set_method`, and drop-tracks the handle
/// (`FreeMapHandle`) (general-owned-temp-tracking spike, slice 3d).
pub type TempRecvMapSetTypesTable = std::collections::HashMap<(usize, usize), TypeExpr>;

/// Side-table populated by the lowering pass from the typechecker's
/// `channel_elem_types` map. Maps each `Sender.send` / `Receiver.recv` /
/// `Receiver.try_recv` `MethodCall` expression's span to the channel
/// element `T` `TypeExpr`. Codegen consults it in the channel-op arm of
/// `compile_method_call` to size the type-erased `karac_runtime_channel_*`
/// transfer (`elem_size`) and to shape the `recv`/`try_recv` out slot. The
/// element type is statically known at each op site (the typed receiver)
/// but NOT at `Channel.new()`, so it travels per call site, not on the
/// channel handle.
pub type ChannelElemTypesTable = std::collections::HashMap<(usize, usize), TypeExpr>;

/// Set by the lowering pass from `TypeCheckResult.stats_elem_types`. Maps a
/// `Stats.<fn>(xs, …)` Call span to the slice's element `TypeExpr` (`i64` or
/// `f64` — S5, the non-f64 element axis). Codegen reads it to pick the
/// reduction's element LLVM type; a missing entry defaults to `f64`.
pub type StatsElemTypesTable = std::collections::HashMap<(usize, usize), TypeExpr>;

/// Set by the lowering pass from `TypeCheckResult.gpu_dispatch_wgsl`. Maps a
/// `gpu.dispatch(kernel, buffer)` kernel-argument span to the WGSL compute
/// shader the typechecker generated from the `#[gpu]` kernel (spike slice-0c).
/// Codegen reads it to bake the shader constant and call the runtime GPU
/// dispatch symbol; the `ast`-importing `gpu_wgsl` emitter runs in the
/// typechecker so `codegen.rs` stays free of AST-shape lowering (the
/// codegen-containment invariant).
pub type GpuDispatchWgslTable = std::collections::HashMap<(usize, usize), String>;

/// `TaskHandle[T].join()` MethodCall span → the result type `T`. Lets codegen
/// size the join out-slot and the cross-task result memcpy for a non-scalar
/// `T` (a `Vec`/`String`/struct return from `spawn`); without it the join
/// defaults to reading `i64`-shaped bytes and a heap return comes back as
/// garbage. Same `(offset, length)` keying as [`ChannelElemTypesTable`].
pub type TaskJoinReturnTypesTable = std::collections::HashMap<(usize, usize), TypeExpr>;

/// Side-table populated by the lowering pass from `TypeCheckResult.expr_types`:
/// for every expression whose Kāra type is a borrow (`ref T` / `mut ref T`),
/// the inner `T` as a `TypeExpr`. Lets codegen learn that a call result
/// (free-fn or method) is a borrow — keyed by the expression's span — so it
/// binds the result as a ref-local (deref on use) instead of as a value.
/// The method-ref half of B-2026-06-07-5 (free-fn calls use
/// `fn_ref_return_inner`; method calls have no static name to key, so they
/// route through this span table). Empty unless the lowering pass ran.
pub type RefReturnInnerTypesTable = std::collections::HashMap<(usize, usize), TypeExpr>;

/// Side-table populated by the lowering pass from `TypeCheckResult.expr_types`:
/// for every expression whose Kāra type is a built-in `Option[T]` /
/// `Result[T, E]`, the full `Option`/`Result` `TypeExpr` keyed by the
/// expression's span `(offset, length)`. Lets codegen render an Option/Result
/// *call result* (`f"{cache.get(1)}"`, `println(opt_fn())`) via its concrete
/// per-payload Display fn — the variable case keys off `var_option_payload_te`
/// / `var_result_payload_te` (populated at the `let` binding), but a bare call
/// expression has no variable name, so it needs a span-addressed lookup. The
/// call-result half of B-2026-07-08-9; codegen splits the payload and applies
/// the same inline-displayable guard the variable case does. Empty unless the
/// lowering pass ran.
pub type DisplayOptionResultTypesTable = std::collections::HashMap<(usize, usize), TypeExpr>;

/// Side-table populated by the lowering pass from `TypeCheckResult.expr_types`:
/// for every expression whose Kāra type is a function type (`Fn(...)` /
/// `OnceFn(...)`), maps its span `(offset, length)` to the equivalent `FnType`
/// `TypeExpr`. Lets codegen recover a first-class fn value's signature from the
/// expression alone — e.g. an un-annotated `let g = h.f;` / `let g = v[0];`
/// reading a `Fn(..)`-typed struct field or `Vec` element — so the binding can
/// be registered in `closure_fn_types` for indirect calls (B-2026-06-21-3).
pub type FnValueTypedExprsTable = std::collections::HashMap<(usize, usize), TypeExpr>;

/// Side-table populated by the lowering pass from
/// `TypeCheckResult.call_type_subs`: for every generic call site, maps the
/// call expression's span `(offset, length)` to the resolved
/// `{ formal-generic-param-name -> concrete-type-name }` substitution the
/// typechecker inferred (e.g. `head(a)` where `a: Vec[i64]` records
/// `{ "T" -> "i64" }`). Codegen's `compile_generic_call` consumes this to
/// bind type params the LLVM-type-based `infer_type_args` can't recover —
/// notably a container element type (`ref Vec[T]`), whose `{ptr,len,cap}`
/// LLVM shape is element-erased, so two element-type instantiations would
/// otherwise collide into one monomorph (B-2026-07-02-41). The concrete
/// name is resolved through the active `type_subst` at the call site, so a
/// nested generic call inside a monomorph (`"T"`) flattens to the outer
/// binding.
pub type CallTypeSubsTable =
    std::collections::HashMap<(usize, usize), std::collections::HashMap<String, String>>;

/// Side-table populated by the lowering pass from the typechecker's
/// `pattern_binding_types` map. Maps each pattern-binding's span (offset,
/// length) to the canonical surface type name (e.g. `"MyError"`). Used by
/// codegen at match-arm bind sites: when binding a tuple-variant payload
/// to a name whose surface type is a struct, codegen reconstitutes the
/// struct value from the i64 payload word so subsequent `.field` access
/// dispatches through the right struct shape.
pub type PatternBindingTypesTable = std::collections::HashMap<(usize, usize), String>;

/// Sibling to `PatternBindingTypesTable`: maps each pattern-binding's span
/// `(offset, length)` to the inner element `TypeExpr` for `Vec[T]` /
/// `Slice[T]` bindings only. Populated by the lowering pass from the
/// typechecker's `pattern_binding_inner_types` map. Consumed by codegen at
/// `bind_pattern_values` to register `vec_elem_types` / `slice_elem_types`
/// under the binding's variable name, so direct method dispatch on a
/// pattern-bound collection payload (`xs.len()` / `xs[0]` / `xs.push(...)`)
/// routes through the right element-typed path. PB sibling slice
/// (2026-05-09).
pub type PatternBindingInnerTypesTable = std::collections::HashMap<(usize, usize), TypeExpr>;

/// Side-table populated by the lowering pass from the typechecker's
/// `expr_types` map: for every expression that produces a heap-owning
/// *temporary* — a `Vec`/`VecDeque`/`String`, a `Map`/`Set` handle, or a
/// shared-struct RC box — maps `(span.offset, span.length)` to that
/// expression's surface `TypeExpr`. Codegen's `materialize_owned_temp`
/// keys this by span to reconstruct the scope-exit cleanup for an
/// *unnamed* temporary: the element type that closes the `Vec` nested-heap
/// leak, the key/val classification a `Map` handle needs, or the heap
/// layout an RC box needs — none of which is recoverable from the LLVM
/// value alone (a `Map` handle and an RC box are both plain pointers).
/// Mirrors the existing TypeExpr-valued hint tables (e.g.
/// [`PatternBindingInnerTypesTable`]) so codegen stays free of a full
/// `TypeCheckResult` dependency (codegen containment, CLAUDE.md). See
/// `docs/spikes/general-owned-temp-tracking.md` (slice 2).
pub type OwnedTempDropsTable = std::collections::HashMap<(usize, usize), TypeExpr>;

/// Side-table populated by the lowering pass from the typechecker's
/// `expr_types` map: for every expression whose Kāra type is a raw pointer
/// (`Type::Pointer { inner, .. }` — `*const T` / `*mut T`), maps
/// `(span.offset, span.length)` to the pointee's surface `TypeExpr`. Codegen's
/// unary-deref arm keys this by the *operand* span to decide whether `*p` must
/// emit a real `load` of the pointee (raw pointer — the operand value is the
/// address itself) versus the no-op pass-through that already suffices for
/// `ref T` / `mut ref T` (whose `load_variable` two-step deref has already
/// produced the inner value). A missing entry degrades to the pass-through
/// path, which stays correct for references. Mirrors the other TypeExpr-valued
/// hint tables (e.g. [`OwnedTempDropsTable`]) so codegen stays free of a full
/// `TypeCheckResult` dependency (codegen containment, CLAUDE.md).
pub type RawPointerPointeeTypesTable = std::collections::HashMap<(usize, usize), TypeExpr>;

/// Side-table populated by the lowering pass from the typechecker's
/// `expr_types` map: for every expression whose Kāra type is a *generic*
/// `Named` type instantiation (`Type::Named { name, args }` with non-empty
/// `args` — e.g. `Option[String]`, `Result[i64, AllocError]`), maps
/// `(span.offset, span.length)` to that expression's fully-instantiated
/// surface `TypeExpr`. Codegen's heap-payload enum `==` consumer
/// (`compile_enum_eq`) keys this by operand span to recover the concrete
/// type argument a generic enum's variant payload was instantiated with —
/// the bare `var_type_names` entry is only `"Option"`, losing the `[String]`
/// that decides whether the `Some` payload compares by content (String/Vec)
/// or by word (scalar). A missing entry simply degrades to the word-wise
/// path (sound for scalar/unit enums); it never introduces a miscompile.
/// Mirrors the other TypeExpr-valued hint tables (e.g.
/// [`OwnedTempDropsTable`]) so codegen stays free of a full
/// `TypeCheckResult` dependency (codegen containment, CLAUDE.md).
pub type EnumInstTypeExprsTable = std::collections::HashMap<(usize, usize), TypeExpr>;

/// Side-table populated by the lowering pass from the typechecker's
/// `expr_types` map: the set of `(span.offset, span.length)` keys for every
/// expression whose Kāra type is `String`. Codegen consults this to
/// distinguish `String` from `Vec[T]` and other 3-word `{ptr, len, cap}`
/// types — they share the LLVM struct shape, so the value alone isn't
/// enough. First consumer: `emit_sort_by_key_inline_thunk` dispatches to a
/// `karac_string_cmp` arm when the key body's span lives in this set.
/// Reusable for any other codegen path that needs the same disambiguation
/// without taking a full `TypeCheckResult` dependency.
pub type StringTypedExprsTable = std::collections::HashSet<(usize, usize)>;

/// Spans of every expression whose typechecked type is `Iterator[..]`.
/// Codegen uses this as the SOUND gate for materializing a `let it =
/// <chain>` iterator binding (B-2026-07-11-19): only an expression the
/// typechecker actually typed `Iterator` is inlined, so an `.iter()` that
/// returns a real collection (`Column.iter() -> Vec[Option[T]]`, the eager
/// `chars()`/`bytes()` materializations) is never mis-intercepted. Sibling of
/// `StringTypedExprsTable`; same key shape.
pub type IteratorTypedExprsTable = std::collections::HashSet<(usize, usize)>;

/// Plain-data record describing a `Tensor[T, Shape]`-typed expression for
/// codegen (phase-11 numerical stdlib). `elem` is the element type as an
/// AST `TypeExpr` (codegen lowers it via `llvm_type_for_type_expr`);
/// `dims` has one entry per static-rank dim — `Some(n)` when the dim is a
/// concrete literal in the type (codegen may fold strides / elide bounds
/// checks), `None` when it is `?` / a dim param / an unresolved dim
/// metavariable (codegen reads the dim from the tensor value's header at
/// runtime — the header is always authoritative). Plain data only: the
/// codegen-containment invariant (CLAUDE.md § Architecture) forbids LLVM
/// types in upstream-phase outputs.
#[derive(Debug, Clone)]
pub struct TensorTypeInfo {
    pub elem: TypeExpr,
    pub dims: Vec<Option<i64>>,
}

/// Side-table populated by the lowering pass from the typechecker's
/// `expr_types` map: for every expression whose Kāra type is
/// `Tensor[T, Shape]` with a statically-known rank (a concrete
/// `Type::Shape` carrying no `...` splice), maps `(span.offset,
/// span.length)` to its [`TensorTypeInfo`]. Codegen consumes this to
/// learn a tensor expression's element type and static dims — at
/// `Tensor.from(...)` construction sites, let-binding registration for
/// unannotated tensor bindings, and indexing on non-identifier
/// receivers. Splice-bearing / bare-param shapes are deliberately
/// absent: their rank isn't statically known, and the only operations
/// the typechecker admits on them (`shape()` / `rank()`) read the
/// tensor value's runtime header instead.
pub type TensorTypedExprsTable = std::collections::HashMap<(usize, usize), TensorTypeInfo>;

/// Plain-data element type of a `Column[T]`-typed expression (phase-11
/// data-science stdlib, Arrow commitment Q5). The codegen-containment
/// invariant (CLAUDE.md § Architecture) forbids LLVM types in
/// upstream-phase outputs, so this carries the element `TypeExpr` only;
/// codegen lowers it to an LLVM type at consumption sites. `Column` is
/// always 1-D with a runtime length, so there is no shape/dims payload
/// (unlike [`TensorTypeInfo`]).
#[derive(Debug, Clone)]
pub struct ColumnTypeInfo {
    pub elem: TypeExpr,
}

/// Side-table populated by the lowering pass from the typechecker's
/// `expr_types` map: for every expression whose Kāra type is
/// `Column[T]`, maps `(span.offset, span.length)` to its
/// [`ColumnTypeInfo`]. Codegen consumes this to learn a column
/// expression's element type at construction sites and at let-binding
/// registration for unannotated column bindings (column-returning
/// calls / transforms).
pub type ColumnTypedExprsTable = std::collections::HashMap<(usize, usize), ColumnTypeInfo>;

/// Side-table populated by the lowering pass from the typechecker's
/// `expr_types` map: the set of `(span.offset, span.length)` keys for every
/// expression whose Kāra type is a `Vector[T, N]` with an **unsigned-integer**
/// element. The LLVM `<N x iX>` lane type is signless, so codegen can't
/// recover element signedness from the value alone; this set lets the SIMD
/// reduce / compare paths pick the unsigned predicate (`ult`/`ugt`) over the
/// signed default (`slt`/`sgt`). Sibling to `StringTypedExprsTable` — same
/// presence-as-signal, no per-span payload needed. First consumer:
/// `compile_vector_method`'s `reduce_min`/`reduce_max` arm, keyed by the
/// receiver-vector expression's span. Shared infra for the slice-3 mask
/// comparisons (phase-7 line 302).
pub type UnsignedVectorExprsTable = std::collections::HashSet<(usize, usize)>;

/// Side-table populated by the lowering pass from the typechecker's
/// `expr_types` map: for every expression whose Kāra type is a `Named`
/// struct (`Type::Named { name, .. }`), maps `(span.offset, span.length)`
/// to the canonical struct name. Sibling to `StringTypedExprsTable` but
/// expanded with a per-span value because the codegen consumer needs the
/// struct name to look up its field-type table. First consumer:
/// `emit_sort_by_key_inline_thunk` dispatches struct-typed keys
/// (`sort_by_key(|item| item)` where `item: MyStruct`) to a field-by-field
/// lex cascade that picks the right per-field comparator (int / String)
/// via `Codegen.struct_field_type_names`.
pub type ExprStructTypeNamesTable = std::collections::HashMap<(usize, usize), String>;

/// Side-table populated by the lowering pass: for every expression whose
/// Kāra type is a struct (or shared struct) with a user-supplied
/// `impl Ord for T` (rather than `#[derive(Ord)]`), maps
/// `(span.offset, span.length)` to the canonical callee key
/// (`"Type.cmp"`). `sort_by_key`'s codegen consults this map before the
/// field-by-field derive cascade, so a user impl can encode arbitrary
/// logic (reverse order, custom tiebreaks) the cascade can't reproduce.
pub type UserOrdTypedExprsTable = std::collections::HashMap<(usize, usize), String>;

/// Borrow form for a pattern binding under a `ref` / `mut ref` scrutinee.
/// `Ref` corresponds to a `ref T` scrutinee mode; `MutRef` to `mut ref T`.
/// Owned bindings have no entry in `PatternBindingBorrowModesTable` —
/// presence-as-signal lets the codegen short-circuit in the common case.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum PatternBindingBorrow {
    Ref,
    MutRef,
}

/// Per-pattern-binding borrow mode populated by the typechecker's
/// `check_pattern_against` walk and forwarded by the lowering pass for
/// codegen to consult. Codegen consumes this at every leaf binding site
/// (plain `Binding`, struct shorthand fields, slice rest bindings,
/// `@`-bindings) to wrap the binding in a "ref shim" — an extra alloca
/// holding a pointer to the value alloca, registered in `ref_params` —
/// so call sites that take a `ref T` / `mut ref T` parameter receive
/// the right ABI shape rather than the raw value. Mirrors the
/// typechecker's `ScrutineeMode::wrap_binding_ty` rule for the codegen
/// surface — design.md § Match Arm Binding Modes.
pub type PatternBindingBorrowModesTable =
    std::collections::HashMap<(usize, usize), PatternBindingBorrow>;

/// Maps each user type with an `impl Drop` to its canonical drop-method key
/// `"<Type>.drop"`. Populated by the typechecker (Prereq.1) and forwarded by
/// the lowering pass for codegen to consume. The presence of a `(Type, key)`
/// entry signals that the type has a validated user-defined `drop()` body
/// already declared+compiled under the `Type.drop` LLVM symbol by the
/// existing impl-method orchestration; codegen synthesizes a public
/// `karac_drop_<Type>` wrapper that invokes the user body + hands off to
/// existing field-cleanup synthesis (Prereq.2 in
/// `docs/implementation_checklist/phase-7-codegen.md`). Empty for programs
/// without any `impl Drop` blocks.
pub type DropMethodKeysTable = std::collections::HashMap<String, String>;

#[derive(Debug, Clone, Default)]
pub struct Program {
    pub items: Vec<Item>,
    /// Joined `//!` doc-comment text at the top of the source file.
    /// Lines from a single run of `//!` are concatenated with `\n`.
    /// `None` when the file has no leading `//!` lines.
    pub module_doc_comment: Option<String>,
    /// Module-level inner attributes — `#![name(args)]` lines at the
    /// top of the source file. Parsed before any top-level item. v1
    /// recognizes `#![rc_budget(max: N)]` (phase-7 line 43); other
    /// names are accepted by the parser and surfaced as unknown-
    /// attribute diagnostics by later passes.
    pub inner_attrs: Vec<Attribute>,
    /// Set by the lowering pass; empty before lowering runs.
    pub question_conversions: QuestionConversionTable,
    /// Set by the cli pipeline after effectcheck; empty otherwise.
    pub callee_effectful: CalleeEffectfulTable,
    /// Set by the cli pipeline after effectcheck; empty otherwise. Identifies
    /// callees that route through the network event loop's park-and-yield
    /// path (i.e., carry `sends(Network)` / `receives(Network)`). Foundation
    /// for the state-machine transform (phase 6 line 26) and codegen
    /// lowering at yield points (phase 6 line 17 sub-item 6).
    pub callee_network_yield_effect: CalleeNetworkYieldEffectTable,
    /// Set by the cli pipeline after `callee_network_yield_effect` is
    /// populated; empty otherwise. For each network-boundary function (one
    /// where `callee_network_yield_effect.get(name) == Some(&true)`),
    /// lists the call sites whose callee is itself in
    /// `callee_network_yield_effect`. These are the suspension points the
    /// state-machine transform codegen lowers to "register fd + park +
    /// yield" code (phase 6 line 17 sub-item 6); their count drives the
    /// state struct's tag arity (phase 6 line 26).
    pub yield_points: YieldPointsTable,
    /// Set by the cli pipeline after `yield_points` is populated; empty
    /// otherwise. For each network-boundary function with at least one
    /// concrete yield point, the per-function state-struct layout (union
    /// of captured-locals across yield points, in source-introduction
    /// order, paired with their typechecker-recorded surface type
    /// names where available). Drives the state-machine transform's
    /// poll-function state-struct shape (phase 6 line 26).
    pub state_struct_layouts: StateStructLayoutTable,
    /// Set by the lowering pass from `TypeCheckResult.expr_types`; empty otherwise.
    pub method_callee_types: MethodCalleeTypesTable,
    /// Set by the lowering pass from
    /// `TypeCheckResult.method_unwrap_inner_types`; empty otherwise.
    pub method_unwrap_inner_types: MethodUnwrapInnerTypesTable,
    /// Set by the lowering pass from `TypeCheckResult.temp_recv_elem_types`;
    /// empty otherwise. Fresh-temp `Vec`/`VecDeque` receiver scalar element
    /// types for codegen's slice-3b read-method redispatch.
    pub temp_recv_elem_types: TempRecvElemTypesTable,
    /// Set by the lowering pass from `TypeCheckResult.temp_recv_mapset_types`;
    /// empty otherwise. Fresh-temp `Map`/`Set` receiver types for codegen's
    /// slice-3d read-method redispatch + handle drop-tracking.
    pub temp_recv_mapset_types: TempRecvMapSetTypesTable,
    /// Set by the lowering pass from `TypeCheckResult.iter_terminal_elem_types`;
    /// empty otherwise. Numeric `Iterator.sum()` / `Iterator.reduce(f)` terminal
    /// MethodCall span → yielded element `TypeExpr`, so codegen seeds the fused
    /// loop's accumulator with a width-correct zero (B-2026-07-11-19).
    pub iter_terminal_elem_types: IterTerminalElemTypesTable,
    /// Set by the lowering pass from `TypeCheckResult.iter_terminal_acc_types`;
    /// empty otherwise. `Iterator.fold(init, f)` terminal MethodCall span →
    /// accumulator `TypeExpr`, so codegen can annotate the synthetic
    /// accumulator `let` and register a heap accumulator for its move-machinery
    /// (B-2026-07-13-18).
    pub iter_terminal_acc_types: IterTerminalAccTypesTable,
    /// Set by the lowering pass from `TypeCheckResult.channel_elem_types`;
    /// empty otherwise. Channel-op element types for codegen's
    /// `karac_runtime_channel_*` lowering.
    pub channel_elem_types: ChannelElemTypesTable,
    /// Set by the lowering pass from `TypeCheckResult.stats_elem_types`;
    /// empty otherwise. `Stats.<fn>` call-span → slice element `TypeExpr`
    /// (S5). See [`StatsElemTypesTable`].
    pub stats_elem_types: StatsElemTypesTable,
    /// Set by the lowering pass from `TypeCheckResult.gpu_dispatch_wgsl`;
    /// empty otherwise. `gpu.dispatch` kernel-arg span → generated WGSL shader
    /// text (spike slice-0c). See [`GpuDispatchWgslTable`].
    pub gpu_dispatch_wgsl: GpuDispatchWgslTable,
    /// Set by the lowering pass from `TypeCheckResult.task_join_return_types`;
    /// empty otherwise. `TaskHandle[T].join()` result types for codegen's
    /// cross-task result-transfer sizing (non-scalar spawn returns).
    pub task_join_return_types: TaskJoinReturnTypesTable,
    /// Set by the lowering pass from `TypeCheckResult.expr_types`: inner
    /// type of every borrow-typed (`ref T`) expression, keyed by span. Lets
    /// codegen bind a borrow-returning method-call result as a ref-local.
    pub ref_return_inner_types: RefReturnInnerTypesTable,
    /// Set by the lowering pass from `TypeCheckResult.expr_types`: the full
    /// `Option[T]` / `Result[T, E]` `TypeExpr` of every such-typed expression,
    /// keyed by span. Lets codegen render an Option/Result *call result* (no
    /// variable name to key on) via its concrete Display fn — the call-result
    /// half of B-2026-07-08-9. See [`DisplayOptionResultTypesTable`].
    pub display_option_result_types: DisplayOptionResultTypesTable,
    /// Set by the lowering pass from `TypeCheckResult.expr_types`: the inner
    /// `T` of every expression typed `Secret[T]` (`std.secret`), keyed by span.
    /// Lets codegen resolve a `Secret[T]` receiver's inner type at a
    /// `.ct_eq(...)` call site (whose result is a plain `bool`, so it has no
    /// borrow-return entry to piggyback on) — gating the constant-time compare
    /// to the `Secret[String]` inner it supports in v1. Reuses the span-keyed
    /// `RefReturnInnerTypesTable` shape.
    pub secret_inner_types: RefReturnInnerTypesTable,
    /// Set by the lowering pass from `TypeCheckResult.pattern_binding_types`.
    pub pattern_binding_types: PatternBindingTypesTable,
    /// Set by the lowering pass from `TypeCheckResult.pattern_binding_inner_types`.
    /// PB sibling slice (2026-05-09).
    pub pattern_binding_inner_types: PatternBindingInnerTypesTable,
    /// Set by the lowering pass from `TypeCheckResult.expr_types`: spans of
    /// every expression whose Kāra type is `String`. Lets codegen
    /// distinguish `String` from `Vec[T]` and other 3-word types that share
    /// the same `{ptr, i64, i64}` LLVM struct shape without taking a
    /// `TypeCheckResult` dependency.
    pub string_typed_exprs: StringTypedExprsTable,
    /// Spans of every `Iterator[..]`-typed expression — the sound gate for
    /// materialized iterator-let inlining (B-2026-07-11-19).
    pub iterator_typed_exprs: IteratorTypedExprsTable,
    /// Set by the lowering pass from `TypeCheckResult.expr_types`: for every
    /// `Fn(..)` / `OnceFn(..)`-typed expression, its `FnType` `TypeExpr`. Lets
    /// codegen register an un-annotated fn-value binding (`let g = h.f;`) in
    /// `closure_fn_types` so a later `g(x)` lowers to an indirect call
    /// (B-2026-06-21-3). See [`FnValueTypedExprsTable`].
    pub fn_value_typed_exprs: FnValueTypedExprsTable,
    /// Set by the lowering pass from `TypeCheckResult.call_type_subs`: the
    /// resolved generic-call type-arg substitution per call span. Codegen's
    /// `compile_generic_call` consumes it to bind container element type
    /// params the LLVM-type-based inference can't (B-2026-07-02-41). See
    /// [`CallTypeSubsTable`].
    pub call_type_subs: CallTypeSubsTable,
    /// Set by the lowering pass from `TypeCheckResult.call_type_subs_mangle`:
    /// the ELEMENT-AWARE mono-mangle token per generic-call type-arg (`T` →
    /// `"Vec_i64"` / `"Vec_String"` / `"String"`), where `call_type_subs` above
    /// keeps only the head name (`"Vec"`). Codegen's `compile_generic_call`
    /// consumes it to give a distinct mono symbol to each builtin-collection
    /// whole-type-param instantiation that shares the `{ptr,i64,i64}` LLVM shape
    /// (B-2026-07-11-35 return-owned-param leg). Same `(offset, length)` keying
    /// as `call_type_subs`.
    pub call_type_subs_mangle: CallTypeSubsTable,
    /// Set by the lowering pass from `TypeCheckResult.expr_types`: for
    /// every `Tensor[T, Shape]`-typed expression with statically-known
    /// rank, its element type + static dims. See [`TensorTypedExprsTable`].
    pub tensor_typed_exprs: TensorTypedExprsTable,
    /// Set by the lowering pass from `TypeCheckResult.expr_types`: for
    /// every `Column[T]`-typed expression, its element type. Codegen
    /// consumes this to learn a column expression's element type at
    /// construction sites and let-binding registration for unannotated
    /// bindings (e.g. a column-returning call). See
    /// [`ColumnTypedExprsTable`].
    pub column_typed_exprs: ColumnTypedExprsTable,
    /// Set by the lowering pass from `TypeCheckResult.expr_types`: spans of
    /// every expression whose Kāra type is a `Vector[T, N]` with an
    /// unsigned-integer element. Lets the SIMD codegen pick the unsigned
    /// compare predicate (`ult`/`ugt`) — the LLVM lane type is signless, so
    /// the value alone can't tell signed from unsigned.
    pub unsigned_vector_exprs: UnsignedVectorExprsTable,
    /// Set by the lowering pass from `TypeCheckResult.expr_types`: for
    /// every expression whose Kāra type is a `Named` struct, maps
    /// `(offset, length)` to the canonical struct name. Lets codegen
    /// recover the source-level struct identity from a value alone (the
    /// LLVM struct type doesn't carry the name back) so `sort_by_key`
    /// can dispatch field-aware compares for struct keys with mixed
    /// integer / `String` fields.
    pub expr_struct_type_names: ExprStructTypeNamesTable,
    /// Set by the lowering pass: for every expression whose Kāra type
    /// is a struct with a user `impl Ord for T`, maps span → canonical
    /// `"Type.cmp"` callee key. Lets codegen route `sort_by_key` keys to
    /// the user's `cmp` function (via direct call) rather than the
    /// derive-equivalent field cascade, preserving custom orderings
    /// (e.g. reverse, multi-key tiebreaks).
    pub user_ord_typed_exprs: UserOrdTypedExprsTable,
    /// Set by the lowering pass from `TypeCheckResult.expr_types`: surface
    /// `TypeExpr` per heap-owning *temporary* expression (Vec/String,
    /// Map/Set, shared-struct RC box). Consumed by codegen's
    /// `materialize_owned_temp` to scope-drop unnamed temporaries. See
    /// [`OwnedTempDropsTable`].
    pub owned_temp_drops: OwnedTempDropsTable,
    /// Set by the lowering pass from `TypeCheckResult.expr_types`: pointee
    /// `TypeExpr` per raw-pointer-typed (`*const T` / `*mut T`) expression,
    /// keyed by span. Consumed by codegen's unary-deref arm to `load` through a
    /// raw pointer rather than yield the address. See
    /// [`RawPointerPointeeTypesTable`].
    pub raw_pointer_pointee_types: RawPointerPointeeTypesTable,
    /// Set by the lowering pass from `TypeCheckResult.expr_types`: surface
    /// `TypeExpr` per generic `Named` instantiation expression (`Option[String]`,
    /// `Result[i64, AllocError]`, …). Consumed by codegen's heap-payload enum
    /// `==` (`compile_enum_eq`) to recover the concrete type argument a generic
    /// enum's variant payload was instantiated with. See
    /// [`EnumInstTypeExprsTable`].
    pub enum_inst_type_exprs: EnumInstTypeExprsTable,
    /// Sibling of [`enum_inst_type_exprs`](Self::enum_inst_type_exprs) for the
    /// COMPLEMENT case: arg-less (concrete, non-generic) `Named` types — a user
    /// `enum Json { … }` or `struct Point { … }`, keyed by expression span.
    /// `enum_inst_type_exprs` deliberately excludes these (its consumers degrade
    /// safely to the word-wise path for a concrete enum), but the `?`-Ok-payload
    /// reconstruction (`reconstruct_question_ok_payload`) needs the payload's real
    /// type to rebuild a MULTI-WORD value — without it, `?` on `Result[Json, E]`
    /// truncated the 4-word `Json` Ok payload to its first word (B-2026-07-11-7).
    /// Consumed ONLY by that reconstruction, so the extra non-`?` entries are inert.
    pub concrete_named_type_exprs: EnumInstTypeExprsTable,
    /// Inferred type of each module-level `let` binding's value expression, keyed
    /// by binding NAME. Populated by lowering from the typechecker's `expr_types`.
    /// Codegen uses it to size the global for a COMPUTED, un-annotated binding
    /// (`let DOUBLED = COUNT * 2;`) — the typechecker is the source of truth for
    /// the type, so codegen never re-infers it (which could diverge). An
    /// annotated binding uses its `: TYPE` directly and never consults this.
    pub module_binding_types: std::collections::HashMap<String, TypeExpr>,
    /// Set by the lowering pass from
    /// `TypeCheckResult.pattern_binding_borrow_modes`. Consumed by codegen
    /// to apply the ref-binding shim at match-arm leaf bindings under a
    /// `ref` / `mut ref` scrutinee. Empty entries mean owned bindings.
    pub pattern_binding_borrow_modes: PatternBindingBorrowModesTable,
    /// Set by the cli pipeline after effectcheck (slice 8ab); empty
    /// otherwise. Per-call-site effect-variable substitutions for
    /// `with E`-bearing callees. Slice 8y consumes this to gate
    /// per-mono state-machine emission on whether the resolved per-call
    /// effects include any network-yield verb. See `CallEffectSubsTable`
    /// type docs for the encoding rationale.
    pub call_effect_subs: CallEffectSubsTable,
    /// Set by the cli pipeline after effectcheck (slice 8y); empty
    /// otherwise. Names of callees whose declared effects are
    /// `DeclaredEffects::Polymorphic` only — purely `with E` (or
    /// `with _`) with no static fixed portion. For these callees the
    /// `callee_network_yield_effect` table flags them conservatively
    /// as network-yield candidates (any monomorphization MIGHT bind
    /// `E` to `sends(Network)` / `receives(Network)`), so the
    /// state-machine transform fires for every call site by default.
    /// When this set contains the callee's name AND
    /// `call_effect_subs[span]` resolves every `E` binding to a
    /// non-network effect set, the per-mono caller-side state-machine
    /// intercept can be skipped in favor of a direct call (slice 8y
    /// optimization). `PolymorphicWithFixed` callees are intentionally
    /// NOT in this set — their fixed portion may carry static
    /// network-yield effects (`with sends(Network) + E`), so codegen
    /// must conservatively keep the intercept for every call site of
    /// those.
    pub callee_purely_polymorphic_effects: CalleePurelyPolymorphicEffectsSet,
    /// Set by the lowering pass from `TypeCheckResult.drop_method_keys`;
    /// empty otherwise. Maps each user type with an `impl Drop` to its
    /// canonical drop-method key `"<Type>.drop"`. Codegen consumes this to
    /// synthesize per-type `karac_drop_<Type>` wrapper functions that
    /// invoke the user-defined drop body. Prereq.2 of the user-`impl Drop`
    /// dispatch slice (phase-7-codegen.md § User-`impl Drop` dispatch).
    pub drop_method_keys: DropMethodKeysTable,
}

mod exprs;
mod items;
mod patterns;
mod stmts;
mod types;
pub use exprs::*;
pub use items::*;
pub use patterns::*;
pub use stmts::*;
pub use types::*;

// ── Closure capture-mutation analysis (design.md Rule 2) ─────────────
//
// A bare closure that MUTATES a captured name captures it by `mut ref`
// (read → ref, mutate → mut ref). Both backends need to know WHICH
// captured names a closure body mutates: the interpreter aliases those
// slots (promotes to a shared cell) so writes propagate; codegen refuses a
// stored closure that mutates a capture (by-value env capture can't
// propagate) rather than silently drop the write. These are pure AST walks
// shared by both — B-2026-07-11-23.

/// The root binding name of an assignment target place expression
/// (`c` → `c`; `c.f` / `c[i]` / `c.0` → `c`). `None` for a non-place root.
pub fn assign_target_root(target: &Expr) -> Option<String> {
    match &target.kind {
        ExprKind::Identifier(n) => Some(n.clone()),
        ExprKind::FieldAccess { object, .. }
        | ExprKind::TupleIndex { object, .. }
        | ExprKind::Index { object, .. } => assign_target_root(object),
        _ => None,
    }
}

/// Collect the root identifier of every assignment target reachable in
/// `block` (recursing into nested blocks and closures). No scope tracking:
/// callers intersect the result with a scope-correct free-variable set, so a
/// recorded name that is actually a local / parameter is filtered out there.
pub fn collect_assigned_roots_block(block: &Block, out: &mut std::collections::HashSet<String>) {
    for stmt in &block.stmts {
        match &stmt.kind {
            StmtKind::Assign { target, value } | StmtKind::CompoundAssign { target, value, .. } => {
                if let Some(root) = assign_target_root(target) {
                    out.insert(root);
                }
                collect_assigned_roots_expr(target, out);
                collect_assigned_roots_expr(value, out);
            }
            StmtKind::Let { value, .. } => collect_assigned_roots_expr(value, out),
            StmtKind::LetElse {
                value, else_block, ..
            } => {
                collect_assigned_roots_expr(value, out);
                collect_assigned_roots_block(else_block, out);
            }
            StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
                collect_assigned_roots_block(body, out)
            }
            StmtKind::Expr(e) => collect_assigned_roots_expr(e, out),
            StmtKind::LetUninit { .. } => {}
            StmtKind::MultiAssign { .. } => {}
        }
    }
    if let Some(final_expr) = &block.final_expr {
        collect_assigned_roots_expr(final_expr, out);
    }
}

/// Sibling of [`collect_assigned_roots_block`] for expressions.
pub fn collect_assigned_roots_expr(expr: &Expr, out: &mut std::collections::HashSet<String>) {
    match &expr.kind {
        ExprKind::Identifier(_)
        | ExprKind::Path { .. }
        | ExprKind::Integer(_, _)
        | ExprKind::Float(_, _)
        | ExprKind::Bool(_)
        | ExprKind::CharLit(_)
        | ExprKind::ByteLit(_)
        | ExprKind::StringLit(_)
        | ExprKind::MultiStringLit(_)
        | ExprKind::CStringLit { .. }
        | ExprKind::SelfValue
        | ExprKind::SelfType
        | ExprKind::PipePlaceholder
        | ExprKind::Continue { .. }
        | ExprKind::OffsetOf { .. }
        | ExprKind::Error => {}
        ExprKind::InterpolatedStringLit(parts) => {
            for part in parts {
                if let ParsedInterpolationPart::Expr(e, _) = part {
                    collect_assigned_roots_expr(e, out);
                }
            }
        }
        ExprKind::Binary { left, right, .. }
        | ExprKind::NilCoalesce { left, right }
        | ExprKind::Pipe { left, right } => {
            collect_assigned_roots_expr(left, out);
            collect_assigned_roots_expr(right, out);
        }
        ExprKind::Unary { operand, .. } => collect_assigned_roots_expr(operand, out),
        ExprKind::Call { callee, args } => {
            collect_assigned_roots_expr(callee, out);
            for arg in args {
                collect_assigned_roots_expr(&arg.value, out);
            }
        }
        ExprKind::MethodCall { object, args, .. } => {
            collect_assigned_roots_expr(object, out);
            for arg in args {
                collect_assigned_roots_expr(&arg.value, out);
            }
        }
        ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
            collect_assigned_roots_expr(object, out);
        }
        ExprKind::OptionalChain { object, args, .. } => {
            collect_assigned_roots_expr(object, out);
            if let Some(args) = args {
                for arg in args {
                    collect_assigned_roots_expr(&arg.value, out);
                }
            }
        }
        ExprKind::Index { object, index } => {
            collect_assigned_roots_expr(object, out);
            collect_assigned_roots_expr(index, out);
        }
        ExprKind::Block(b) | ExprKind::Comptime(b) => collect_assigned_roots_block(b, out),
        ExprKind::If {
            condition,
            then_block,
            else_branch,
        } => {
            collect_assigned_roots_expr(condition, out);
            collect_assigned_roots_block(then_block, out);
            if let Some(eb) = else_branch {
                collect_assigned_roots_expr(eb, out);
            }
        }
        ExprKind::IfLet {
            value,
            then_block,
            else_branch,
            ..
        } => {
            collect_assigned_roots_expr(value, out);
            collect_assigned_roots_block(then_block, out);
            if let Some(eb) = else_branch {
                collect_assigned_roots_expr(eb, out);
            }
        }
        ExprKind::While {
            condition, body, ..
        } => {
            collect_assigned_roots_expr(condition, out);
            collect_assigned_roots_block(body, out);
        }
        ExprKind::WhileLet { value, body, .. } => {
            collect_assigned_roots_expr(value, out);
            collect_assigned_roots_block(body, out);
        }
        ExprKind::Loop { body, .. } | ExprKind::LabeledBlock { body, .. } => {
            collect_assigned_roots_block(body, out)
        }
        ExprKind::For { iterable, body, .. } => {
            collect_assigned_roots_expr(iterable, out);
            collect_assigned_roots_block(body, out);
        }
        ExprKind::Match { scrutinee, arms } => {
            collect_assigned_roots_expr(scrutinee, out);
            for arm in arms {
                if let Some(g) = &arm.guard {
                    collect_assigned_roots_expr(g, out);
                }
                collect_assigned_roots_expr(&arm.body, out);
            }
        }
        ExprKind::Closure { body, .. } => collect_assigned_roots_expr(body, out),
        ExprKind::Tuple(items)
        | ExprKind::ArrayLiteral(items)
        | ExprKind::PrefixCollectionLiteral { items, .. } => {
            for it in items {
                collect_assigned_roots_expr(it, out);
            }
        }
        ExprKind::RepeatLiteral { value, count, .. } => {
            collect_assigned_roots_expr(value, out);
            collect_assigned_roots_expr(count, out);
        }
        ExprKind::MapLiteral(entries) => {
            for (k, v) in entries {
                collect_assigned_roots_expr(k, out);
                collect_assigned_roots_expr(v, out);
            }
        }
        ExprKind::StructLiteral { fields, spread, .. } => {
            for f in fields {
                collect_assigned_roots_expr(&f.value, out);
            }
            if let Some(s) = spread {
                collect_assigned_roots_expr(s, out);
            }
        }
        ExprKind::Return(opt) | ExprKind::Break { value: opt, .. } => {
            if let Some(e) = opt {
                collect_assigned_roots_expr(e, out);
            }
        }
        ExprKind::Question(inner) | ExprKind::Cast { expr: inner, .. } => {
            collect_assigned_roots_expr(inner, out);
        }
        ExprKind::Range { start, end, .. } => {
            if let Some(s) = start {
                collect_assigned_roots_expr(s, out);
            }
            if let Some(e) = end {
                collect_assigned_roots_expr(e, out);
            }
        }
        ExprKind::Par(b)
        | ExprKind::Seq(b)
        | ExprKind::Unsafe(b)
        | ExprKind::Try(b)
        | ExprKind::Providers { body: b, .. } => collect_assigned_roots_block(b, out),
        ExprKind::Lock { mutex, body, .. } => {
            collect_assigned_roots_expr(mutex, out);
            collect_assigned_roots_block(body, out);
        }
    }
}
