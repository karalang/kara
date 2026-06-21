# Design spike — per-layout monomorphization (SoA across call boundaries)

**Status:** 🟩 **COMPLETE — slices 1–6 landed (2026-06-20).** Slice 1 (the
`LayoutId` axis scaffolding), slice 2 (forward arg-layout monomorphization — a
SoA `Vec[E]` passed by value to a helper is served by an on-demand layout
monomorph, regardless of the param name), slice 3 (SoA returns — a helper that
builds and returns a `Vec[E]` is monomorphized to *return* the receiving
binding's layout, so the returned local crosses the boundary even though it has
no binding name to key on), slice 4 (multi-buffer / differing-name kernels —
forward inference extended to `ref`/`mut ref Vec[E]` borrow params, so multiple
SoA buffers of one element type flow through shared by-ref helpers, each
monomorphizing a distinct symbol), slice 5 (origin-only `soa_layouts` — the
name-keyed access-path fallback is replaced by a per-binding `LayoutId` value
carrier seeded at the binding site, and the redundant name-keyed by-value param
ABI is retired, fixing the base-symbol footgun where a `Vec[E]` param named like
a layout block lowered SoA by coincidence), and **slice 6 (the Slipstream
full-SoA proof — `examples/slipstream/src/sim.kara`'s carried LBM grid is a
`layout` block; the native oracle's milestone checksums are byte-identical
AoS↔SoA and the browser flagship runs on SoA in real headless Chrome)** are on
`main`. Slice 6 surfaced and fixed five more cross-function gaps (the
`with_capacity`-presized SoA constructor, the returned-local base-symbol
name-match clash, SoA reassignment `grid = substep(grid)`, tail-CALL SoA-return
propagation, and SoA carried across a coroutine suspend — state-struct field +
par-slot typing). This file is the architecture of record; update its `Status:`
line (and the `docs/spikes/README.md` row) as slices land. Tracks
**[B-2026-06-19-14](../bug-ledger.jsonl)** (the SoA-across-functions entry) and
design.md **Feature 1 / P1.5 (Phase 11)**.

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

0. **(this ADR)** decision + invariants recorded. ✅
1. **Layout as a mono axis — representation + plumbing.** ✅ **Landed
   2026-06-20.** `LayoutId` (`Aos` | `Soa(<layout-block-name>)`) +
   `mangle_suffix` in `state.rs`; `layout_subst` field on `Codegen`
   saved/restored in `compile_generic_call`'s mono entry (parallel to
   `type_subst`/`const_subst`); `compute_call_layout_subst` (forward inference,
   returns `Aos` for every `Vec[E]` param for now); `mangle_mono_name` appends
   the layout suffix. No behavior change: every call resolves to `Aos`, so the
   mangled name is unchanged and output is byte-identical — codegen E2E 1680/0,
   non-codegen suite 6693/0. The body-lowering reads (the SoA access-path
   trigger) and the `Soa` construction land in slice 2.
2. **Forward arg-layout mono (supersede the by-value-params name-keying).** ✅
   **Landed 2026-06-20.** Forward layout-flow inference
   (`compute_call_layout_subst` now reads each bare-binding argument's
   binding-site layout via `active_layout_id`); a non-generic helper with a
   non-`Aos` arg is routed (`call_dispatch.rs`) to an on-demand monomorph
   (`fn_asts` registry + `ensure_layout_mono_generated`), with its `Vec[E]`
   params lowered as the SoA struct in `declare_mono_function` /
   `compile_mono_function` keyed on `layout_subst` (not the param name). The
   body's SoA access triggers moved to `active_soa_layout` (the bridge over the
   name-keyed origin). Mangling is per-param (`$<param>_soa_<layout>`), so two
   different layout assignments over one helper can't collide. Caller-retains
   ownership (LSan-clean). Codegen E2E 1683/0; non-codegen 6694/0; LSan
   by-value (same-name + caller-different-name) 2/0. The name-keyed by-value
   path is left in place (redundant but harmless) and retired in slice 5 once
   the access-path lookups are reduced to origins. Regressions: by-value param
   with a caller-different binding name; two distinct layouts through one
   helper → distinct monos.
3. **SoA returns (backward inference).** ✅ **Landed 2026-06-20.** The SoA
   `let <recv> = <call>()` arm parks the receiving binding's layout in a
   one-shot `pending_return_layout`; `compile_call` consumes it (scoped to the
   call, before args compile) and — when the callee returns a `Vec[E]` — folds
   it into the same `ensure_layout_mono_generated` entry as the forward arg
   layouts, under a `return_layout` axis that adds a `$ret_soa_<name>` mangle
   suffix. `declare_mono_function` lowers the return type to `soa_vec_type`;
   `compile_mono_function` seeds the returned local(s) into `layout_subst`
   (detected via the same tail analysis as `suppress_cleanup_for_tail_return`)
   so the body's construction / pushes / tail all lower SoA, and
   `suppress_soa_cleanup_for_tail_identifier` drops the returned local's
   `FreeSoaGroups` so the move-out caller owns the buffers (no double-free, no
   leak). The SoA-new let trigger moved to `active_soa_layout` so a seeded
   returned local builds SoA. Regressions: `init_grid()`-shape returning a SoA
   Vec bound by a differently-named local; one builder bound into two layouts →
   two distinct return-SoA monos; ASAN/LSan move-out ownership (caller-owns).
   Branch-leaf / multi-`return` returns landed as a **follow-on** (2026-06-20):
   `soa_return_local_names` now recursively collects every bare-identifier
   return site (each explicit `return <id>;` in any branch / loop / nested
   block — not inside a closure — plus every tail leaf of a branch-bearing tail
   `if c { a } else { b }`), so each return value lowers SoA against the patched
   signature. (Before the follow-on these multi-site returns were a hard LLVM
   "return type does not match" verify failure, not the silent AoS degrade this
   note once assumed.) Early-return move-out uses a branch-safe runtime
   `cap = 0` sentinel (`neutralize_moved_soa_groups_slot`), not the tail path's
   compile-time `FreeSoaGroups` frame removal — the early-return frame is shared
   with the fall-through path where the local is NOT returned and must still be
   freed (the branch-buried-move footgun). Tests: 3 codegen E2E + 2 ASAN/LSan
   (early-return fall-through, branch-leaf tails — both paths exercised).
4. **Multiple SoA bindings of one element type through shared helpers;
   confirm distinct monomorphs.** ✅ **Landed 2026-06-20.** Forward layout-flow
   inference (slice 2) extended to the **borrow forms** `ref Vec[E]` /
   `mut ref Vec[E]`, the way real multi-buffer kernels (`grid`, `coll`, `next`)
   share helpers without moving the buffer. `param_is_layout_carrying` peels one
   `ref`/`mut ref`, so a borrow `Vec[E]` param gates the dispatch and receives a
   `layout_subst` entry from the caller's argument layout (driving the body's
   SoA access paths via `active_soa_layout`). The by-value SoA *signature* patch
   (`active_param_soa_layout`) is guarded to by-value only: a borrow param keeps
   its pointer ABI (caller passes `&struct`; the mono body derefs once), so only
   owned `Vec[E]` params' signatures become the 4-field SoA struct.
   `compile_mono_function`'s prologue registers a SoA borrow param in
   `ref_params` so `compile_soa_index_read` / `compile_soa_method` deref the slot
   once before GEPing groups/len — the by-ref-reads discipline the mono path
   otherwise omitted (without it the access path reads the pointer bytes as the
   SoA struct → garbage len → SIGTRAP). `ref_params` is now save/restored
   (`mem::take`) around both mono entry points so a mono's borrow param can't
   leak into the caller's context. Per-param mangling (`$<param>_soa_<layout>`)
   gives each buffer layout a distinct symbol (`total$data_soa_grid` vs
   `total$data_soa_coll`) — the borrow analog of slice 2's by-value distinctness.
   Regressions: by-ref read into a differently-named param; two SoA buffers
   through one by-ref helper → distinct correct monos (proven by each reading its
   OWN grouping correctly — a single shared body could not); mut-ref push (WRITE)
   across a function with write-back through the deref'd pointer; ASAN/LSan
   mut-ref borrow ownership (callee borrows, caller frees once). The whole-element
   SoA *index*-store path (`grid[i] = E{…}`) landed as a **follow-on**
   (2026-06-20): `compile_soa_index_store` scatters the RHS element struct's
   fields into each group's own buffer at `[i]` (strided by the group sub-struct
   — the same decomposition `push` does at `len`, with a leading bounds-check and
   no growth), dispatched ahead of the AoS Vec path on `active_soa_layout` and
   deref'ing a `ref`/`mut ref` param slot via `ref_params` so the scatter crosses
   a function boundary (the `mut ref` index-assignment kernel). Before it, the
   store fell into `compile_vec_index_store` and wrote whole AoS elements over one
   group's narrower stride — a silent heap-buffer-overflow. Tests: 3 codegen E2E
   (single-fn, `mut ref` cross-function, cold-group) + 1 ASAN overflow guard. POD
   elements only, matching the rest of the SoA subsystem (push / field-store /
   `FreeSoaGroups` all assume POD; SoA elements with heap fields stay an
   orthogonal gap shared across those paths).
5. **Retire / bridge the name-keyed lookups in the access paths; `soa_layouts`
   becomes origin-only. Borrow-checker cross-group disjointness facts audited.**
   ✅ **Landed 2026-06-20.** The access-path *trigger* — a binding's physical
   layout — now reads a per-binding **value carrier** (`binding_layouts:
   HashMap<String, LayoutId>`, §4.1's "the value carrier is a `LayoutId`
   attached to bindings, not the binding name") instead of re-deriving SoA-ness
   from the binding *name* against `soa_layouts` at each use. `active_layout_id`
   reads `layout_subst` (mono params/returns) then `binding_layouts`
   (in-function locals), with **no** `soa_layouts` fallback. The carrier is
   seeded once, at the binding *site*, by the new `seed_binding_site_layout`
   (the one sanctioned origin name-match — design.md's "layout binds to the
   binding site"): the `let` arm resolves the binding's layout (`layout_subst`
   for a return-SoA-seeded local, else the `layout`-block origin keyed by the
   binding's own name) and records it; every downstream use reads the carrier.
   The carrier is function-scoped — cleared at each function entry and
   `mem::take`-save/restored around both mono entry points, exactly like
   `variables` / `ref_params`. With this, `soa_layouts` is **origin-only**:
   consulted to build the layout catalogue (`collect_soa_layouts`), to resolve a
   `LayoutId::Soa(<block>)` to its struct shape, and at the binding site to match
   a name to an origin — never as a per-use access-path trigger. The redundant
   name-keyed by-value param ABI (`soa_value_param_layout`, slice-1/2 leftover)
   is **retired**: the BASE symbol now lowers every `Vec[E]` param AoS, and a
   SoA-laid-out argument is routed to a per-layout monomorph by the call
   dispatch regardless of param name. That fixes a footgun — a base symbol whose
   `Vec[E]` param merely *shared a name* with a `layout` block used to lower SoA
   on the name alone, so calling it with an ordinary AoS `Vec[E]` marshalled a
   3-field AoS struct into a 4-field SoA slot (LLVM "Call parameter type does not
   match function signature"). Regression:
   `test_e2e_soa_layout_named_param_base_is_aos_mono_is_soa` — one helper with a
   `Vec[E]` param named `es` (matching `layout es`), called once with the SoA
   binding `es` (→ SoA mono) and once with an AoS `plain` (→ AoS base), each
   reading its own layout correctly.

   **Cross-group disjointness audit.** The borrow checker is **layout-agnostic**
   by the codegen-containment invariant: `ownership.rs` never imports
   SoA/`layout` types and sees only a plain `Vec[Entity]`. Cross-group
   disjointness (design.md:5425 — `mut ref entities[i].position` and
   `ref entities[j].health` never alias) is therefore satisfied *by
   construction*, not by a new SoA-specific fact: groups **partition** the
   element's fields, so a cross-group pair is a *distinct-field* place pair,
   which the checker already separates via the same place-disjointness rule it
   uses for `mut ref user.a` + `ref user.b` (design.md:4943). SoA is purely a
   codegen lowering of those already-disjoint field places onto separate
   per-group buffers, which makes two checker-disjoint places *physically*
   disjoint too — it never makes two checker-disjoint places alias. The one
   genuinely SoA-specific borrow rule — `ref entities[i]` (a whole-element
   reference) is a compile error because no contiguous element exists
   (design.md:5424); whole-element use binds `let e = entities[i]`, an AoS copy —
   lives at a different layer and is preserved (the index-read materializer is
   untouched). Slice 5 changes only codegen's value carrier, leaving the checker
   byte-identical, so every disjointness fact it had is preserved trivially. The
   realized cross-group **read** surface is exercised by the existing tests
   (`sumall` reads two groups in one expression). The cross-group **write**
   surface via direct field-level index-store (`entities[i].position = …`, the
   `e.position += e.velocity` idiom) surfaced a **pre-existing,
   layout-agnostic-checker-orthogonal codegen bug** during this audit — the
   per-group element address was mis-strided, so a store at index ≥ 1 was dropped
   (index 0 coincidentally correct) — **now fixed in 38fb0b57** (B-2026-06-20-7,
   `compile_soa_field_store`: the store-side mirror of `compile_soa_index_read`'s
   group addressing). It was independent of slice 5 (the base compiler
   miscompiled it identically) and of the borrow checker (a codegen
   address-arithmetic fault, not an aliasing-fact gap). The whole-element
   index-store `grid[i] = E{…}` (same family, the scatter sibling of the
   field-level store) landed as a follow-on — see slice 4's closing note
   (`compile_soa_index_store`).
6. **Proof: convert `examples/slipstream/src/sim.kara`** to a `layout` block and
   confirm the native oracle checksums are byte-identical AoS↔SoA — Slipstream
   earns its "SoA layout" roster billing. **DONE.** The carried LBM grid (plus
   the per-substep `coll`/`next` intermediates) is a `layout` block split into
   two cache groups; the per-band chunks stay AoS (they cross the generic
   `TaskHandle[Vec[LbmNode]]` join). The native oracle's milestone checksums are
   byte-identical to the AoS build (1582897806 / 793640938 / 680974524) and the
   browser flagship runs on SoA in real headless Chrome (`verify_browser.mjs`
   PASS — isolated, evolving, 370-frame soak, wheel-angle control). The proof
   surfaced **five** more cross-function gaps, all fixed in the compiler (no
   demo-side workarounds, per the dogfooding charter):
   - **`with_capacity` SoA constructor.** The `presize` lowering rewrites a
     counted-loop-filled `Vec.new()` into `Vec.with_capacity(n)` (the
     `init_grid`/`fan_collide` shape). The SoA let path matched only `Vec.new`,
     so the rewritten binding kept the AoS `{ptr,len,cap}` slot under an SoA
     layout — an LLVM type mismatch. `is_vec_with_capacity_call` routes it to
     `compile_soa_new` (the capacity is a hint the lazily-grown groups drop).
   - **Returned-local base-symbol clash.** A builder whose returned local is
     named after a `layout` block (`init_grid`'s `grid`) name-matched its body
     SoA while the AoS-return base symbol's signature returned the 3-field Vec.
     A returned local's layout is the return mono's (`layout_subst`), not its
     name — `soa_return_locals` suppresses the origin name-match for it.
   - **SoA reassignment.** `grid = substep(grid, …)` (the carried-grid per-frame
     double-buffer) had no backward-mono path on the assignment arm (only the
     `let` arm did) — the call returned the AoS struct into the 4-field slot →
     SIGSEGV. `compile_soa_assign_from_call` parks the return layout, frees the
     OLD groups (the by-value param is caller-retains, so the displaced buffers
     are owned here), then stores the new SoA header; the binding's queued
     `FreeSoaGroups` frees the final frame at scope exit (no double-free/leak —
     Linux-LSan verified).
   - **Tail-CALL SoA-return propagation.** A SoA-returning fn whose body ENDS IN
     a layout-returning call (`substep`'s `fan_stream(coll, …)`) returned AoS
     while its signature was SoA. `compile_tail_final_expr` flows the function's
     return layout to the tail call (the tail-IDENTIFIER analog of
     `soa_return_local_names`).
   - **SoA across a coroutine suspend** (the browser render loop's `grid` carried
     across `frames.recv()`): collect `soa_layouts` and pre-populate `fn_asts`
     **before** the state-machine emission (the poll-fn body's SoA-return
     inference reads both); size an SoA persisted local's state-struct field as
     the 4-field struct; type an SoA binding's par-block / auto-par return slot
     SoA (`infer_let_binding_llvm_type`). Without the last one, the wasm-threads
     driver threaded `grid` through an AoS return slot and mismatched its SoA
     `substep` call.

## 6. Migration from the name-keyed model

The name-keyed model was not deleted up front — slices 2–4 built the mono path
*beside* it and cut over once each shape reached parity, so the suite never went
red. Slice 5 completed the cutover: `soa_layouts` survives as the layout-origin
map (§4.1), but the *lookup trigger* in the access paths is gone — a binding's
physical layout is the per-binding `binding_layouts` carrier (or `layout_subst`
for mono params/returns), seeded once at the binding site. The redundant
name-keyed by-value param ABI (`soa_value_param_layout`) is retired; the two
originally-shipped cross-function shapes (by-ref reads, by-value params) are now
served solely by the mono path.

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
