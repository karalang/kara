# Spike: profile the self-hosted lexer — real-world Kāra codegen hotspots

**Status:** ✅ **RAN — resolved 2026-06-12.** Hypothesis *half* right: allocation is a
strong second (38% of self-time), but the **#1 hotspot is a surprise — string-literal
`match` dispatch lowered to a sequential `memcmp` chain (46%)**, a codegen-quality issue,
not allocation. The self-hosted lexer is **4.6× the Rust lexer's instruction count** on
identical work (token output bit-identical). Two new codegen levers filed in `roadmap.md`;
two bugs filed (`B-2026-06-12-9` `?`-in-`main` miscompile, `B-2026-06-12-10` suspected
per-iter leak). None of the three deferred SIMD-class levers (alias-scope / NT-stores /
fusion) is the real-world answer — confirming their deferral. Full results below.
**Follow-on (2026-06-12): lever #1 SHIPPED** — string-`match` dispatch now lowers to a
length/first-byte `switch` tree; re-profiled **111.7 B → 66.9 B instructions (−40%), Rust
gap 4.58× → 2.74×, `memcmp` 180 → 0 self-time samples**, and allocation is now the #1 leaf
(lever #2 promoted). See the Decision section.
Filed (scoped) 2026-06-12.
**Decision this spike gates:** where to spend `karac` codegen-perf effort next. The
kata corpus (leetcode) over-represents tiny allocation-bound algorithmic puzzles and
under-represents the workloads Kāra is actually positioned for (bulk-data/analytics,
systems/parsing, latency-bound small-tensor ML). It served as a *bug-finder*, not a
perf oracle. `selfhost/src/main.kara` (1864 lines: byte-scan + tokenize +
String-build) is a real Kāra systems program **we already have** — profiling it gives
the first honest signal of where real-world Kāra code, and karac's codegen for it,
spends time.

## The question

On a realistic input (lex hundreds of KB of `.kara` source), where does the
self-hosted lexer spend its time — and how much of that is **karac codegen quality**
(slow generated code for a common Kāra pattern) versus the algorithm itself?

**Hypothesis (to be measured, not assumed):** allocation / String-building bound, not
compute. If confirmed, the highest-leverage codegen lever is **reducing allocations in
String/Vec-heavy code** — which *every* real program hits — NOT vectorization, which
this session showed already reaches Rust parity (see
[independence-noalias-ilp.md](independence-noalias-ilp.md)) but only matters for the
narrow bulk-data class.

## Method

1. **Snapshot** `selfhost/src/main.kara` from `main`. Another session actively edits
   the `selfhost-lexer` worktree — do **not** profile a live worktree.
2. `karac build` it (release codegen, `-O2` default). Feed a **large** real input —
   concat of the compiler's own `.kara` sources / `examples/`, ≥ a few hundred KB,
   lexed in a loop for a stable sample window.
3. Sample under macOS `sample <pid>` (or Instruments / `xctrace`); rank functions by
   **self-time**. Cross-check **allocation behavior** (count `karac_alloc_or_panic`
   calls, or `leaks`/`heap`) — String-build sites are the prime suspects
   (`escape_for_render`, `strip_underscores`, `render`, token `Vec` growth).
4. **Parity number:** wall-time versus the Rust lexer (`src/lexer.rs`, or the
   `kara-katas` oracle) driven on the same input. A self-hosted compiler many× slower
   than its Rust self is a *credibility* problem, not just a perf one
   (`project_self_hosting_v1_credibility`).
5. **Trust the profile + asm** of the top function, not just wall-clock.

## Decision rule (set before measuring — no post-hoc goalposts)

- **Allocation/String-build dominates** (> ~40 % self-time, or the parity gap traces to
  alloc traffic) → next codegen-perf slice is **allocation reduction** (String-builder
  reuse, small-string optimization, `Vec` pre-sizing, ref-not-copy on hot string
  paths). File each concrete hotspot as its own tracked entry.
- **Compute/branch-bound** → different levers (branch hints from effect analysis, the
  cost model) — and a surprise worth knowing.
- Either way the deliverable is a **ranked list of real codegen-quality hotspots** —
  the honest replacement for the leetcode perf signal.

## Results (ran 2026-06-12, M5 Pro)

**Setup.** Harness = `selfhost/src/main.kara` (snapshot from `main`, *not* the live
`selfhost-lexer` worktree) with `main()` swapped for a stdin-slurp + lex-in-a-loop driver
(token count summed so the optimizer can't elide the lex; no per-token render/println, so
the profile is the *lexer*, not I/O). Input = the compiler's own `.kara` sources +
`examples/` concatenated ×6 = **441 KB** of real Kāra. 800 iterations.
Built sequential (`KARAC_AUTO_PAR=0` at build time — auto-par otherwise parallelizes the
loop and injects `psynch_cvwait`/`mach_absolute_time` scheduler-idle leaves + inflated
RSS; the instruction count is identical either way, confirming auto-par only *spreads* the
work). Sampled at 1 ms; self-time bucketed by leaf.

**Hotspot profile (3703 leaf samples, sequential):**

| Bucket | Self-time | What |
|---|---|---|
| **`memcmp`** | **46.0%** | string-literal `match` dispatch — `keyword_or_ident` (~90 `"kw" => Token` arms) + `is_structural_marker` + `is_reserved_*`, each lowered to a **sequential chain of `memcmp`** (every identifier walks up to ~90 compares). Confirmed structurally: memcmp self-time clusters across adjacent kara call-sites (the match chain), and `keyword_or_ident:1359` is a literal `match text { … }`. |
| `malloc`/`free` | 29.6% | allocation traffic — `substring`-per-token (allocating owned `String`, not zero-copy slice), the byte-by-byte `bytes: Vec[u8]` build in `Lexer.new`, token-`Vec` growth. |
| `memmove`/`bzero` | 8.6% | the copies behind those allocations. |
| kara code (self) | 15.6% | the actual scan/branch logic. |

→ **alloc-related = 38.2%** (malloc+memmove+bzero); **string-compare = 46%.**

**Parity vs the Rust lexer** (`karac::tokenize`, `src/lexer.rs`, identical 441 KB × 800):

| | Kāra self-host | Rust | ratio |
|---|---|---|---|
| tokens (correctness) | 53,933,600 | 53,933,600 | **bit-identical** |
| instructions retired | 111.7 B | 24.4 B | **4.58×** |
| user time | 5.07 s | 0.92 s | 5.5× |
| peak RSS | 731 MB | 321 MB | 2.3× |

The self-hosted lexer is **functionally exact** but **~4.6× the Rust instruction count**.
Decomposition: removing the memcmp chain alone (→ a length/first-byte switch or perfect
hash) reclaims ~46%; the alloc lever reclaims ~38% more — **both are needed** to approach
Rust, and the keyword-dispatch one is the bigger single lever and the more general (every
real Kāra program does string `match` / `==` dispatch: keyword tables, command/route
dispatch, config parsing).

## Decision (rule was set before measuring)

The pre-registered rule had two arms: alloc-dominant → allocation-reduction lever;
compute/branch-dominant → "different levers, a surprise worth knowing." **Both fired** —
this is the surprise arm *plus* a strong alloc signal. Outcome, as a ranked list of real
codegen-quality hotspots (the honest replacement for the leetcode perf signal), now filed
as tracked `roadmap.md` § Codegen Optimization entries:

1. **String-literal `match` / `==` dispatch lowering** (#1, 46%) — lower a `match` on
   string literals (and `==`-against-literal chains) to something better than a linear
   `memcmp` cascade: length-bucket + first-byte switch, a jump table, or a perfect-hash /
   trie for the keyword set. General-purpose; biggest single win.
   **✅ SHIPPED 2026-06-12** (the `match` half). Lowered to a length-bucket + first-byte
   `switch` tree with residual `memcmp` (`src/codegen/control_flow_match.rs`). Re-profiled
   on the same 441 KB input, token output still bit-identical: **111.7 B → 66.9 B
   instructions retired (−40%); Rust gap 4.58× → 2.74×; `memcmp` fell from the #1 self-time
   leaf (180 samples) to 0** — and **allocation (malloc/free) is now the #1 leaf**, i.e.
   lever #2 below is promoted to the top spot. Remaining sub-levers tracked in
   `roadmap.md`: the `==`-chain half, `Or`-pattern string arms, and a perfect-hash
   escalation only if a re-profile shows residual `memcmp` still dominant.
2. **Allocation reduction on hot String/byte paths** (#2, 38% → now #1) — `substring` returns an
   owned copy where a borrow/slice would do (the [[project-lexer-string-scan-shape]]
   zero-copy-slice lesson, here *inside* the lexer); the `for b in src.bytes()` Vec build
   in `Lexer.new` is a second copy of the whole input. Levers: slice-not-copy on
   classify-only reads, `Vec`/`String` pre-sizing, small-string optimization.

**Verdict on the three deferred SIMD-class levers** (alias-scope metadata, non-temporal
stores, fusion — all filed deferred and pointing here as their candidate-hunt vehicle):
**none is the real-world lever.** Real Kāra systems code is string-dispatch- and
allocation-bound, not vectorizable-bulk-arithmetic-bound. They stay deferred; effort goes
to the two levers above. This is the corpus evidence the noalias/autovec resolution said
to wait for.

**Bugs surfaced** (filed in `bugs.md`, gitignored): `B-2026-06-12-9` — `?` inside
`main() -> Result[..]` miscompiles (`ret {i64,i64}` vs `i32`); `B-2026-06-12-10` —
per-iteration leak (~0.9 MB / 441 KB lexed, linear RSS growth). **RESOLVED
2026-06-13 (`ecfa867a`):** verified real (`leaks --atExit` at O0, not a macOS RSS
artifact) and root-caused — NOT the hypothesized `Lexer` src/bytes drop but an
inline enum-variant-constructor temp passed by value as a call arg
(`make_spanned(Token.StringLiteral(value))`) whose caller-side drop was missing.
Fixed; rich-lex-loop leak 883 KB → 115 KB (−87%). The 115 KB residual is a
distinct composite-payload enum-drop gap (c-string `CStr{Vec[u8]}` + f-string
`Vec[InterpPart]`), re-filed as `B-2026-06-13-13`.

## Caveats

- Profiles the **lexer** specifically (the only self-hosted component today): a fair
  but partial slice of "real Kāra." Re-run as the parser / typechecker get ported — the
  signal gets richer and more representative.
- This is a *measure-first* spike: produce the profile + parity number first; do not
  pre-commit a codegen slice until the hotspots are known.

## Cross-references

- [independence-noalias-ilp.md](independence-noalias-ilp.md) — resolved 2026-06-12:
  vectorization/aliasing is **not** the real-world lever (param `noalias` inert;
  `wrapping_*` was the actual autovec enabler; at Rust parity; alias-scope metadata
  deferred). This profiling spike is the follow-on that asks "then what *is* the lever?"
- `roadmap.md` § Codegen Optimization (IR quality pass). This spike is the named
  candidate-hunt + decision-rule vehicle for three deferred levers there, all gated on a
  real kernel surfacing here: **alias-scope metadata** (needs an auto-vec-heavy
  non-hand-vectorized or many-slice kernel), **non-temporal / streaming stores** (needs a
  write-heavy bulk kernel that blows L2), and loop/kernel **fusion**. If this profile finds
  the lexer is allocation/String-build bound (the hypothesis), none of those three is the
  answer and the effort goes to the allocation/ownership path instead.
- `feedback_optimize_for_production_not_kata`, `feedback_simulate_demand_dont_wait`.
