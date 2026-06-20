# Design spike — per-layout monomorphization (SoA across call boundaries)

**Status:** ⬜ **SCOPED — ADR, not started (2026-06-20).** Decision recorded; the
implementation is a multi-slice Phase-11 effort gated on the full
`tests/codegen.rs` suite + the Linux-LSan leak gate per slice. This file is the
architecture of record; update its `Status:` line (and the `docs/spikes/README.md`
row) as slices land. Tracks **[B-2026-06-19-14](../bug-ledger.jsonl)** (the
`partial` SoA-across-functions entry) and design.md **Feature 1 / P1.5 (Phase 11)**.

Cross-refs: [design.md § Feature 1: Data Layout](../design.md), the Slipstream
SoA follow-up in [dogfooding.md](../dogfooding.md), and the codegen follow-on
cluster in [phase-7-codegen.md](../implementation_checklist/phase-7-codegen.md).

---

## 1. The problem

A `layout` block makes a `Vec[E]` binding compile to a 4-field struct-of-arrays
(SoA) value `{ g0_ptr, …, [cold_ptr,] len, cap }` instead of the default AoS
`{ ptr, len, cap }`. Today this works **only at the binding's declaring
function**, plus two cross-function slices:

- **By-ref reads** (`fn f(es: ref Vec[E])`) — landed `b5e0fc58`. The callee
  derefs the param slot once and reads through the caller's SoA struct.
- **By-value params** (`fn f(es: Vec[E])`) — landed `58d81d29` (slice 1 of
  B-2026-06-19-14). The param's signature type is the SoA struct; caller-retains
  ownership.

Both rely on the **name-keyed model**: `soa_layouts` is a `HashMap<String,
SoaLayout>` keyed by *binding name*, and the access paths look up
`soa_layouts.get(var_name)`. A `layout es: Vec[E]` block makes any binding named
`es` — in any function — SoA. The two cross-function slices work precisely
because the caller's variable and the callee's param share the name `es`.

This breaks down for the two cases that block Slipstream from going full-SoA:

1. **SoA return values.** `fn init_grid() -> Vec[LbmNode]` has *no binding name*
   on the return type to key a layout on. The caller `let grid = init_grid()`
   keys off `grid`'s layout; the callee's returned local might be named
   `out`/`next`/`grid` — with name-keying these are three unrelated layouts that
   only happen to be structurally identical.

2. **Differing names / multi-buffer.** A real kernel passes `grid`, `coll`,
   `next`, `chunk` — each a `Vec[LbmNode]` — through the same helpers. The
   name-keyed model demands a *separate* `layout <name>` block per binding name,
   all structurally identical. `substep(grid)`'s param is named `grid`; calling
   `substep(coll)` would pass `coll`'s SoA struct into a param the signature
   built for layout `grid` — only safe because they coincide, and a genuine
   layout mismatch (SoA arg → AoS-named param, or two *different* groupings) is
   an LLVM verification failure, not a clean diagnostic.

The name-keyed model is a crude stand-in for what design.md actually specifies.

## 2. The design contract (design.md § Feature 1)

The implementation must honor these invariants (design.md:5413–5427):

- **Same type.** `Vec[Entity]` with a layout block and without are the *same
  type* at the type-system level — spelled identically, type-compatible at every
  call boundary. `fn process(data: Vec[Entity])` accepts any `Vec[Entity]`.
- **Layout binds to the binding site.** `layout entities: Vec[Entity] { … }`
  makes the *name* `entities` SoA; `Entity` keeps its AoS view everywhere else.
  (So the `layout <name>` *declaration* syntax — and a name→layout origin map —
  **stays**. This spike is about *propagation*, not re-spelling the source.)
- **No O(n) copy at call boundaries.** The callee operates on the collection's
  *existing* layout. There is no marshalling/transcode at a call.
- **Distinct monomorphs per grouping.** "Two layout blocks for the same
  collection type with different groupings produce distinct codegen monomorphs."
  Layout is part of the concrete type *at codegen level*, invisible to the type
  system.
- **`ref entities[i]` (whole element) is a compile error**; `let e =
  entities[i]` materializes an AoS copy. Already enforced; mono must preserve it.
- **Cross-group disjointness** is a borrow-checker fact (`mut ref e[i].position`
  and `ref e[j].health` never alias). Already partially modeled; out of scope
  for the first slices (correctness, not a new capability).

The one-line reframe: **layout is a monomorphization axis** — like a generic
type parameter or a const-generic, except it is inferred from the argument's
binding-site layout (and, for returns, from the receiving binding) rather than
spelled in the source.

## 3. Decision

**Make layout a monomorphization axis layered on the existing generic-mono
engine.** Keep `layout <name>: Vec[E]` as the *origin* of layout identity; add
layout-flow propagation so a function that takes/returns a `Vec[E]` is
**monomorphized per layout** at its call sites, with the callee body lowered
against the active layout.

Rejected alternatives (see §7) — structural-match returns within name-keying
(doesn't satisfy "distinct monomorphs per grouping" and stays a footgun); a
global whole-program layout table (no per-call specialization, can't express two
groupings of one type); an implicit AoS↔SoA copy at boundaries (violates "no O(n)
copy").

## 4. The model

### 4.1 Layout identity

A `LayoutId` names a concrete physical layout: either `Aos` or
`Soa(<layout-block-id>)`. The origin map stays — `soa_layouts: HashMap<String,
SoaLayout>` keyed by the `layout` block's name — but the *value* carrier becomes
a `LayoutId` attached to bindings and propagated, not the binding *name*.
Distinct `layout` blocks (even structurally identical, even same element struct)
get distinct `Soa(id)` values, satisfying "distinct monomorphs per grouping."

### 4.2 Layout-flow inference

A pre-codegen pass (or an on-demand walk in codegen, reusing how
`compile_generic_call` collects `subst`) computes, per function call, the
`LayoutId` of each `Vec[E]`/`Array[E,N]` argument and of the return:

- **Forward (arguments):** an argument expression's `LayoutId` is the binding-site
  layout of its root — a `layout`-declared local/param is `Soa(id)`; anything
  else is `Aos`. (Index/field/call results that yield a `Vec` carry the layout
  of their producer.)
- **Backward (returns):** the call's return `LayoutId` is the layout of the
  *receiving binding* — `let grid = init_grid()` with `layout grid` ⇒ the call is
  monomorphized to return `Soa(grid)`. A discarded/un-layout-bound result ⇒
  `Aos`. This is the bidirectional step the name-keyed model can't do.

The monomorph key for a call is `(fn_name, [arg LayoutIds], ret LayoutId)`.

### 4.3 Monomorph keying & mangling

Extend the mono engine (`src/codegen/mono.rs`): today it keys on a type `subst`
+ `const_subst` and only fires for `self.generic_fns`. Add a **layout subst**
(`HashMap<param_name, LayoutId>` + a return `LayoutId`) and let a *non-generic*
function enter monomorphization when any param/return is a layout-carrying
collection with a non-`Aos` layout at some call site. `mangle_mono_name` gains a
layout suffix (e.g. `process$soa_entities`), so each layout variant is a distinct
LLVM symbol. The all-`Aos` monomorph is the existing default body (no new symbol
when nothing is laid out — zero overhead for non-SoA code).

### 4.4 Body lowering against the active layout

While compiling a layout-monomorph body, the active layout subst drives the SoA
access paths. The existing `compile_soa_index_read` / `compile_soa_method` /
`compile_soa_new` stay; their *trigger* moves from "is `var_name` in
`soa_layouts`?" to "what is this binding's active `LayoutId` in the current
monomorph?". Within an `Aos` monomorph the binding lowers as a normal Vec. This
is the same save/restore discipline `compile_generic_call` already uses for
`subst`/`const_subst`/`scope_cleanup_actions` (see mono.rs and the Tensor
shape-generic cleanup work, phase-11).

### 4.5 Return ABI

A `Soa(id)` return lowers the function's LLVM return type to the SoA struct
(reusing `soa_vec_type`); the returned local is built/owned per the existing SoA
cleanup discipline, and the caller binds the result into its SoA slot. Ownership
mirrors the by-value-param slice (caller-retains is wrong for a *returned* fresh
value — a returned SoA Vec is a move *out*, so the callee suppresses its own
`FreeSoaGroups` and the caller's binding owns it; this is the SoA analog of the
existing `suppress_cleanup_for_tail_return` for AoS Vec).

## 5. Slice plan (each gated on full `tests/codegen.rs` + LSan)

0. **(this ADR)** decision + invariants recorded.
1. **Layout as a mono axis — representation + plumbing.** Introduce `LayoutId`,
   the layout subst on the mono key, mangling suffix, and save/restore in the
   mono entry. No behavior change yet: every call resolves to `Aos`, so output is
   byte-identical to today. Pure scaffolding; the suite must stay 100% green.
2. **Forward arg-layout mono (supersede the by-value-params name-keying).** A
   `Vec[E]` param is lowered per the *caller's* arg layout via the mono key, not
   the param name. Retire the name-keyed by-value path once parity holds.
   Regression: by-value param with a caller-different binding name.
3. **SoA returns (backward inference).** Return-layout from the receiving
   binding; return ABI + tail-return move-suppression. Regression:
   `init_grid()`-shape returning a SoA Vec bound by a differently-named local.
4. **Multi-buffer / differing-name kernels.** Multiple SoA bindings of one
   element type through shared helpers; confirm distinct monomorphs.
5. **Retire / bridge the name-keyed lookups** in the access paths; `soa_layouts`
   becomes origin-only. Borrow-checker cross-group disjointness facts audited.
6. **Proof: convert `examples/slipstream/src/sim.kara`** to a `layout` block and
   confirm the native oracle checksums are byte-identical AoS↔SoA — Slipstream
   earns its "SoA layout" roster billing.

## 6. Migration from the name-keyed model

The name-keyed model is not deleted up front — slices 2–3 build the mono path
*beside* it and cut over once each shape reaches parity, so the suite never goes
red. `soa_layouts` survives as the layout-origin map (§4.1); only the *lookup
trigger* in the access paths moves (slice 5). The two shipped cross-function
slices (by-ref reads, by-value params) are preserved by construction until their
mono replacements pass the same regressions.

## 7. Alternatives considered

- **Structural-match SoA returns within name-keying.** Decide a return is SoA if
  its element struct has *any* layout, match by `(num_groups, has_cold)`. Cheap,
  but two different groupings of one struct collide, and it can't express the
  design's "distinct monomorphs per grouping." A footgun that papers over the
  real model. Rejected.
- **Global whole-program layout table** (one layout per element type). Simpler,
  but forbids two groupings of the same type and still has no per-call
  specialization. Contradicts design.md:5426. Rejected.
- **Implicit AoS↔SoA transcode at boundaries.** Lets any layout call any
  function via an O(n) copy. Directly violates "no implicit O(n) copy at call
  boundaries." Rejected.

## 8. Risks & open questions

- **Mono blow-up.** A function reachable with K distinct layouts compiles K
  times. Bounded in practice (few layout blocks per project); the `Aos` default
  is shared. Acceptable, same trade as generic mono.
- **Layout-flow through aggregates** (a `Vec[E]` *inside* a struct/tuple/enum, a
  `Vec[Vec[E]]`). First slices scope to top-level `Vec[E]` params/returns; nested
  layout flow is a later sub-slice (call it out, don't silently AoS it).
- **Higher-order / fn-pointer call sites** can't see a static arg layout. Resolve
  to `Aos` (the safe default) and document; a layout-capability bound is the P2
  deferred item (design.md:5427, deferred.md "Layout-Capability Bound").
- **Interpreter parity.** `karac run` is AoS-only for SoA today; mono is a
  codegen specialization, so `run` stays AoS (values identical). Keep the
  AoS↔SoA byte-identical-output invariant as the oracle (slice 6).

## 9. Doc footprint (update these together — see memory `maintain-scope-doc-index`)

- This spike (`Status:` line) + the `docs/spikes/README.md` index row.
- [bug-ledger.jsonl](../bug-ledger.jsonl) B-2026-06-19-14 `fix` field as slices land.
- [dogfooding.md](../dogfooding.md) Slipstream SoA follow-up (remaining items).
- [phase-7-codegen.md](../implementation_checklist/phase-7-codegen.md) and/or
  [phase-11-stdlib-longtail.md](../implementation_checklist/phase-11-stdlib-longtail.md)
  checklist entries.
