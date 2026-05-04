# Design Studies — Aggregate Findings

**Exercise.** Starting from concrete data-oriented programs rather than from syntax options, write the same program in Java, Python, Rust, and Kāra, then score each implementation on 4 axes: **Robustness**, **Locality**, **Composability**, **Ceremony cost**. The goal: determine whether Kāra's current trait/impl shape (v44 D1a) holds up against real programs, and surface any design issues the v44 brainstorm missed.

**Bricks completed.**

| # | Brick | Exercises | Per-brick findings |
|---|---|---|---|
| 0 | `db_read/` | DB access, optional DI, provider-rooted resources, connection lifecycle | No explicit findings.md — served as the entry point that surfaced the "triangle is elective, not structural" observation |
| 0 | `json_read/` | File I/O, derive-based deserialization | No explicit findings.md — served as the entry point that surfaced `#[derive]` as the bundled-at-definition-site escape hatch |
| 1 | `money_type/` | Domain type with behavior — D1 stress test | `money_type/findings.md` |
| 2 | `http_api_call/` | Async, network I/O, typed response parsing | `http_api_call/findings.md` |
| 3 | `parallel_fanout/` | Concurrency primitives, data-race freedom, auto-parallelism | `parallel_fanout/findings.md` |
| 4 | `event_stream/` | Push-model source, unbounded loop, per-item robustness | `event_stream/findings.md` |

---

## Headline

**The trait/impl shape (v44 D1) is not where Kāra's distinctiveness lives.** Four bricks exercised it across a range of program shapes; only brick #1 (domain type) surfaced any D1 tension at all, and the tension was bounded (readable, not catastrophic) and already paid for by typed errors + mechanical refactor + retroactive-impl capability.

**The effect system + concurrency model *are* where the distinctiveness lives.** Brick #3 (parallel fanout) was the first brick to produce a clear Kāra-distinctive result — **auto-parallelism inferred from effect signatures, with no `async` keyword and no library-level primitives**. That's not matched in any of Java, Python, or Rust.

---

## Scorecard across all bricks

Simple tally. **K** = Kāra wins clearly; **T** = ties with Rust; **~** = roughly even with Rust/Java/Python.

| Brick | Robustness | Locality | Composability | Ceremony |
|---|---|---|---|---|
| `money_type` | T | ~ | T | ~ |
| `http_api_call` | K (effects) | T | T | T |
| `parallel_fanout` | K (effects) | T | K | K |
| `event_stream` | T | T | T | T |

**Kāra's clear wins come from the effect system**, not the trait/impl shape. Wins:
1. `reads(Network)` / `reads(FileSystem)` / `reads(Stdin)` visible in inferred signatures — callers know what resources a function touches without reading the body.
2. Auto-parallelism for source-level independent calls — no mainstream-language analog.
3. No "async coloring" — functions don't fork into sync/async worlds; the effect system handles suspension.
4. Provider-rooted resources give test isolation + dependency inversion at the language level (not a library pattern).

**Kāra's parity with Rust holds everywhere else.** Typed errors, pattern matching, `?` propagation, derive-based (de)serialization — all match Rust's shape. This is the inherited foundation.

---

## Verdict on v44

### D1 (bundled vs. separated) — recommend closing at **D1a (status quo)**

The separated form is adequate for every brick we wrote. The bundled form would save ~5-10 lines per type in the common case but:

- Loses mechanical refactor (moving impls between files becomes non-trivial)
- Loses retroactive impls (unless a new escape hatch is introduced, which is just the hybrid form with extra steps)
- **Is NOT the bottleneck Kāra faces.** The bottleneck is making sure the effect system + concurrency story hold up; the trait/impl shape is a supporting cast member.

**Close v44 at D1a. Do not open a bundled-form track.** If future bricks with 5+ trait conformances per type surface real locality problems, revisit.

### D1d (effect-trait fusion) — recommend opening as a **separate v45 track**

Brick #3 made a case that v44 didn't fully capture: the `trait` + `effect resource R: TraitName` pairing in current Kāra is the most duplicative part of the language. Fusing them (what v44 called D1d) would:

- Collapse four concepts (struct, trait, impl, effect resource) to three
- Directly improve the concurrency story (effect resources already carry conformance bounds; making that the primary shape removes the `trait`+`effect resource` pair)
- Be orthogonal to D1a/b/c — fusion is about *what a trait is*, not *where impls live*

**D1d is the genuinely interesting redesign v44 surfaced.** If v45 happens, this should be its center.

### Other v44 items — defer to v45

- D2/D3/D4 (bundled-form details) are moot if D1a closes.
- D5 (retroactive impls) — keep (as v44 leaned). Bricks confirmed it earns its keep for the stdlib-trait-on-user-type case.
- D6 (generics in bundled form) — moot.
- T1 (fmt normalization) — moot.

---

## What the programs-first approach produced

**Confirmed design instincts:**
- Rust-way trait/impl shape is fine for Kāra's typical programs.
- Typed errors, derive-based serialization, pattern matching — all land without friction.

**Surfaced things v44 missed:**
- Effect system + concurrency are the center of the language; trait/impl is peripheral.
- Auto-parallelism via effect analysis is unique and needs protection from design changes that would break it.
- Effect-trait fusion (D1d) is the genuinely interesting redesign.
- Java `record` (post-16) and Python decorators (`@total_ordering`) close much of the ceremony gap that earlier Rust-vs-Java comparisons assumed. "Bundled languages are verbose" is not a standalone Kāra-side argument anymore.

**Did not surface:**
- Any case where the current design produces a clearly-wrong program.
- Any case where a bundled form would be a clear win.
- Any case where the effect system feels like overhead rather than value.

---

## Next steps (if pursued)

1. **Decide v44 disposition.**
   - Close v44 at D1a (recommended).
   - Archive v44 as "menu of options; outcome D1a per design studies."

2. **Open v45 on effect-trait fusion (D1d).**
   - Start from the same programs-first discipline: which existing bricks change shape under fusion? Which stay identical? Which improve?
   - `db_read/postgres_query_injected.kara` is the single brick most affected — rewrite it under fusion and compare.

3. **Add bricks only if a specific question is blocked.**
   - "Type with 8 trait conformances" — if someone claims D1a's locality cost compounds, test it. Otherwise skip.
   - "Multi-file module graph" — if module/visibility design is next in the roadmap, useful. Otherwise skip.
   - "Real Kafka/SSE event stream" — only if the stdin version isn't enough to decide something concrete.

4. **Do not retroactively normalize the studies** — they're dated artifacts. If v45 changes the language shape, the studies become a historical record of what the language looked like when the decision was made, which has ongoing value for design archaeology.

---

## Appendix: Decision lens (the 4 axes, restated)

| Axis | What it means | Direction |
|---|---|---|
| **Robustness** | Compiler catches more errors; diagnostics pinpoint exactly; fewer plausible-but-wrong programs typecheck | More = better |
| **Locality** | Reader (human or LLM) understands more from a single file/function/definition without cross-referencing | More = better |
| **Composability** | Features stack without hidden interactions; mechanical tooling (fmt, rename, move-between-files) works without semantic understanding | More = better |
| **Ceremony cost** | Scaffolding / concepts-in-head beyond the program's actual work | Less = better, UNLESS paid for by wins above |

Kāra's ceremony is paid for when it buys robustness, locality, or composability that mainstream languages don't get. The four bricks confirm this is the case for the *effect system* and *concurrency model*; it is marginally the case for the *trait/impl shape*; it is not the case for anything else tested.
