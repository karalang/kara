# Spike: automatic collection capacity pre-sizing

**Status:** ⬜ **SCOPED 2026-07-03 — not started. Narrow-A targets PROBED 2026-07-03; the
highest-value one is blocked on a correctness bug, not a perf gap.** Decision framing: the
*narrow* version (size-hinted bulk construction — `collect`/map-to-`Vec`/comprehension/
`from_iter` pre-sizes from a known source length) is the defensible long-term fix; the
*general* version (static push-count inference over arbitrary loops) is **out of scope** —
fragile, unpredictable, memory-hazardous on loose bounds, and Rust deliberately declined it.
The hand-written-`push`-loop residual is served by documenting the existing
`Vec.with_capacity` idiom in the book, not by a prover.

**Probe results (2026-07-03) — they reprioritize the slices below:**
- **S1 (`from_slice`) is already done.** Measured: building a 16-elem `Vec[(i64,i64)]` K=1M
  times via `Vec.from_slice` is **16.7 ms** — faster even than `with_capacity`+push
  (22.8 ms), so codegen already single-allocs + bulk-copies. No pre-sizing win to capture.
- **S2 (`.map().collect()`) does not compile under `karac build` at all** — it works in the
  interpreter but codegen has no handler (`no handler for method 'collect' on non-identifier
  receiver`; only identifier-Vec `.collect()`→clone and `chars().collect()` are lowered). A
  **run/build divergence on a book-documented idiom** (ch10), tracked as
  [`B-2026-07-03-25`](../bug-ledger.md). So S2 is **correctness-first**: implement the
  adaptor-chain → `Vec` lowering (pre-sizing while materializing closes S2), and there is no
  "before" perf number because the idiom errors out today.
- Net: the top item is no longer "flip `new`→`with_capacity` and watch a number drop"; it is
  "make `.map().collect()` compile, pre-sized." The visible outcome is a **fixed divergence
  plus a new benchmark for that idiom**, not a delta on an existing working program.

No prototype built yet; this spike defines what to build and, more importantly, what not to.

**Question this spike gates:** A `Vec` built by `Vec.new()` + `push` grows by reallocation
(cap 0 → 1 → 2 → 4 → 8 → …). On allocation-bound code with no other heavy per-call work,
that realloc chain is the whole cost. Measured on kata
[#57 Insert Interval](../../../kara-katas/leetcode/1-100/57-insert-interval/) (K=1M calls,
each returning a bounded `Vec[(i64,i64)]`): the natural `Vec.new()` solution runs **74.8 ms**,
`Vec.with_capacity(n+1)` runs **22.8 ms** — a **3.3× speedup landing at C parity (21.7 ms)**.
The entire seq gap to C was the grow-from-empty tax, not codegen quality. Rust pays the
identical tax (its `Vec::new()` mirror is 79.6 ms — Kāra *ties* it on the natural idiom),
and Rust also needs a manual `with_capacity` to reach C. Can the compiler make the natural
code fast automatically, so the win doesn't depend on the user knowing an idiom?

This matters for a Kāra-specific reason: the project's thesis is *the natural code is the
fast code*. A 3.3× win that requires the programmer to already know `with_capacity` is a
manual tuning knob, not a language property — and the idiom is currently invisible
(`with_capacity` appears nowhere in the book; `ch09-collections.md` only shows `Vec.new()`).
So the honest options are (A) make the compiler do it, or (B) document the idiom. This spike
is about how far (A) can soundly go.

## The trap in the general version

The naive framing — "bound the push count in any loop, reserve it" — is a trap for three
reasons, each independently disqualifying:

1. **A loose bound is a pessimization, not just a missed win.** Reserving only helps when the
   push count is provably *close* to the reserved bound. A loop bounded by a large `n` that
   pushes conditionally is the counterexample:
   ```
   while i < 1_000_000 {
       if data[i].matches(rare) { v.push(data[i]) }   // pushes ~10 times
   }
   ```
   The trip-count bound is 1e6; the real push count is ~10. Auto-reserving 1e6 slots turns a
   tiny allocation into a multi-MB one — a pessimization, potentially an OOM under
   adversarial `n`. So the analysis cannot reserve on a trip-count bound alone; it must prove
   pushes are **near-unconditional per iteration** (push count ≈ trip count). That
   tightness proof is the hard part, and it fails on exactly the common "filter" shape.

2. **Unpredictable firing violates the principle it's meant to serve.** The goal is for
   natural code to be *reliably* fast. A heuristic that fires on "counted loop, unconditional
   push" but silently doesn't on the same code with one conditional push gives an **opaque
   performance model**: sometimes-3×-faster for reasons the user can't see, predict, or
   teach. For a language whose selling point is a legible performance story, that is arguably
   worse than no optimization. A compiler optimization only satisfies "documented so an LLM
   applies it at the right time" if its firing rule is simple enough to state in the book —
   which general loop inference is not.

3. **It mutates allocation — the highest-bug-surface region of the compiler.** The bug ledger
   is dense with allocation-adjacent miscompiles (double-free, UAF, move-elision
   interactions, HashMap-order nondeterminism). A pass that rewrites `Vec.new()` →
   `with_capacity` runs in that blast radius and interacts with move-elision, drop insertion,
   and auto-par. The upside is a constant factor on allocation-bound code; the downside is a
   correctness surface over *all* code.

## Rust's precedent — the tell

Rust does **not** do general push-loop capacity inference. What it does: `Iterator::size_hint`
+ `FromIterator`, so `iter.map(..).collect::<Vec<_>>()` pre-sizes *because the iterator knows
its length*. Capacity is inferred **only where it is cheap and exactly known (iterator
length), through the idiom (`collect`), not through loop analysis.** A perf-obsessed language
examined this exact tradeoff and chose the idiom/size-hint path over an opaque prover. That
is the strong signal for where Kāra's effort should go.

## The defensible narrow version (what to actually build)

Pre-size the **bulk-construction idioms** where the source length is already known, at the
point of lowering — not by analyzing hand-written loops:

- `iter.map(f).collect()` / `.filter(p).collect()` → reserve source length (exact for `map`,
  upper-bound-with-shrink for `filter`; a length upper bound here is *tight by construction*,
  unlike an arbitrary loop).
- list/`Vec` comprehensions → reserve the driving collection's length.
- `Vec.from_slice(s)` / `from_iter(known_len)` → reserve `s.len()`.
- any builtin that transforms one sized container into another (the Column/Tensor/DataFrame
  bulk ops already do this internally; unify them on the same reservation helper).

Why this is sound *and* predictable *and* teachable, where the general version is none of
those:

- **Sound** — the source length is exact or a construction-tight upper bound; no
  tightness-guessing, no adversarial-`n` blowup (a `filter().collect()` over-reserves by at
  most the filtered-out count and can shrink-to-fit once, which `FromIterator` already does).
- **Predictable / documentable** — one rule: *"building a collection from another sized
  collection pre-sizes."* That sentence goes in `ch09-collections.md` and an LLM can apply
  it. There is no dataflow the user has to simulate in their head.
- **Natural** — most real bounded-`Vec` building *is* a transform of an existing collection,
  so this captures the common case without touching a single user-written loop.
- **Contained** — it lives in the `collect`/`FromIterator`/comprehension lowering (one or a
  few sites), not a whole-program analysis pass; the allocation-mutation blast radius is a
  handful of known constructors, not "every `Vec.new()`."

Kāra's existing **escape analysis is a real asset** here: the "is this collection still local
/ not yet aliased when we choose its capacity?" question is already answered by the
ownership machinery, so the reservation can be placed safely.

## The residual — and why it stays path B

The hand-written `push`-in-a-loop case (kata #57's `insert_interval`: three counted loops,
combined trip count exactly `n`, result length in `[1, n+1]`) is the hard 20%. It is
*possible* to fire a very tightly gated inference here (unconditional pushes only, counted
loops with a provable tight bound, collection provably local), but that is precisely the
fragile, unpredictable, high-risk general version above wearing a smaller hat — and the
firing rule ("we optimized it when we could prove a tight bound") is not teachable. For this
residual the honest fix is **B: document `Vec.with_capacity` in the book** as the idiom to
reach for when you know the output bound, with the trigger ("building a bounded-size
collection in a loop → reserve first"). That keeps the performance model legible: bulk
construction is auto-fast (narrow-A), and the manual-loop case has a documented, one-line
opt-in — mirroring Rust's `collect`-pre-sizes / `with_capacity`-on-request split exactly.

## What the honest kata-#57 headline is

Independent of any of this: on the *natural* idiom, **Kāra ties Rust** (74.8 vs 79.6 ms). The
pre-sizing win is a manual idiom available in *both* languages, not a Kāra advantage — and
the README for #57 should read that way, not as "Kāra needs a tweak to compete." The
narrow-A work would turn the common (bulk-construction) case into a genuine Kāra-over-Rust
edge, since rustc does not auto-presize a `for`-push loop either; but for #57's specific
manual-loop shape, the win stays a documented idiom, not a compiler guarantee.

## Proposed slices (if greenlit)

1. **S1 — one reservation helper + `Vec.from_slice`/`from_iter(known_len)`.** ✅ **Already
   done for `from_slice`** (probed 2026-07-03: 16.7 ms/1M, single alloc + bulk copy — no win
   left). Keep only as the plumbing reference if `from_iter(known_len)` turns out not to
   pre-size; otherwise skip.
2. **S2 — `.map(..).collect()`: FIRST make it compile, THEN pre-size.** ⚠️ **Reprioritized —
   correctness before perf.** This idiom does not codegen at all today
   ([`B-2026-07-03-25`](../bug-ledger.md)): interp runs it, `karac build` errors
   `no handler for method 'collect' on non-identifier receiver`. So the slice is (a) add the
   codegen dispatcher arm that materializes a lazy adaptor chain into a `Vec`, then (b)
   reserve the source length while materializing (the size hint rides on the new lowering for
   free). Highest value — it closes a run/build divergence on a book-documented idiom, with
   pre-sizing as a bonus. There is no before/after *number* here, because it errors today;
   the deliverable is a fixed divergence + a new benchmark for the idiom.
3. **S3 — `.filter(..).collect()` + comprehensions.** Upper-bound reserve + one shrink-to-fit;
   confirm no adversarial blowup (a `filter` that drops everything must not hold a giant
   buffer — the shrink covers it). Likely shares the S2 dispatcher gap — verify each
   adaptor's collect lowering exists before assuming a pre-size is all that's needed.
4. **B (do regardless, independent of S1–S3)** — document `Vec.with_capacity` +
   the "reserve when the bound is known" idiom in `ch09-collections.md`, with the kata-#57
   number as the motivating example.

Explicitly **out of scope:** general static push-count inference over arbitrary user loops.
Revisit only if profiling shows manual-`push` loops (not bulk construction) dominate real
Kāra allocation cost — which the corpus does not currently show.

## Cross-references

- Corpus-wide allocation lever sibling: [small-string-optimization.md](small-string-optimization.md)
  (`malloc` is the #1 self-time leaf; this spike attacks the *count* of small allocations, that
  one attacks the *cost* of each short-string one — complementary).
- Motivating measurement: [kata #57 Insert Interval § Optimization](../../../kara-katas/leetcode/1-100/57-insert-interval/README.md).
- Prior "don't grow a prover" decision with the same soundness-vs-ROI shape:
  [overflow-check-elision.md](overflow-check-elision.md).
- `with_capacity` already exists as a runtime call and is used internally by the SoA
  constructor path (see [per-layout-monomorphization.md](per-layout-monomorphization.md)).
