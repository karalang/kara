# Spike: Small-String Optimization (SSO) for the runtime `String`

**Status (2026-09-15):** 🟡 Slices 1–3 landed; **`KARAC_SSO` still defaults to
OFF, and two open rows block the flip.** Slice 1 = layout + accessors + free-gate
hardening (its follow-up claimed every String buffer-free/realloc gate was
inline-safe; **Slice 2 measured that claim FALSE** — 16 gates were still unsigned
`UGT`, 14 fixed in `3833ff8`). Slice 2 = inline construction. Slice 3 = the FFI
boundary and the read path.

### WHERE THIS STANDS — read this before the narrative below

Everything under this section is a running log in date order, so **the further
down you read, the older the numbers are.** The current picture:

**CURRENT RAIL SET — measured at `0382953`, `RUNS=9`, `ITERS=3M`,
`KARAC_AUTO_PAR=0`, x86-64.** Every figure below carries the commit it was taken
at, because this table has gone stale three times in two days: rail levels are
host-dependent AND the compiler moved underneath them twice.

| rail | `SSO=0` | `SSO=1` | delta |
|---|---|---|---|
| `lexer` | 1831 ms | 1481 ms | −19.1% |
| `lexlike` | 47 ms | 37 ms | −21.3% |
| `substr` | 32 ms | 7 ms | −78.1% |
| `builder20` | 50 ms | 47 ms | −6.0% |
| `builder60` | 72 ms | 68 ms | −5.6% |
| `promote` | 38 ms | 30 ms | −21.1% |
| `pfx_idx` | 13 ms | 13 ms | +0.0% |
| `pfx_chars` | 34 ms | 26 ms | −23.5% |
| **`vecread`** | 26 ms | 48 ms | **+84.6%** |
| `vechoist` | 4 ms | 4 ms | +0.0% |

**Every CONSTRUCTION rail is now a win, and the read-path rail is the only
loser.** After `9d3ceb9` and `066695ff`, `lexlike` went +23% → −21% and `substr`
+73% → −78%. `vecread` (a `Vec[String]` element read through `.bytes()`, no
construction in the timed loop) is +85%, and `vechoist` — the same reads through
a view built ONCE — is +0.0%. So SSO costs nothing to read a materialized
slice; all of it is view construction, and it is unconditional (an all-inline
`Vec` still pays +71%). That is the whole of `vertical`'s regression,
B-2026-09-14-28, and the SSO track is now one open question rather than several.

**Rail deltas — and they are NOT portable across machines.** Same compiler
(`karac` at `27466ba`, built and run on both hosts), `RUNS=15`, two samples,
`KARAC_AUTO_PAR=0`. Negative = SSO faster.

**Every number in this table PREDATES `9d3ceb9` and `substr`'s row is now
wrong by an order of magnitude.** That commit made `String.substring` emit its
inline encoding as IR instead of calling `karac_string_try_inline_into`, and the
rail went `+73% → −83%` (217 ms → 22 ms against a 126 ms `KARAC_SSO=0`
baseline) on host B; on arm64 it is 13.9x faster than `KARAC_SSO=0` and 6.8x
faster than the call. The rest of the table is untouched by that commit —
`lexlike` still routes through `karac_string_slice_into` and still reads +23%.
Re-run before quoting any of it:

| rail | host A | host B (`nproc=4`) |
|---|---|---|
| `lexer` | −17.7% | −13…−17% |
| `lexlike` | **−13%** | **+23%** |
| `substr` (pre-`9d3ceb9`) | +34% | **+73%** → now **−83%** |
| `builder20` | +15% | +3% |
| `builder60` | +12% | +3% |
| `promote` | +8% | +0.8% |
| `pfx_idx` | +15% | +16% |
| `pfx_chars` | −10% | −7.6% |
| **`substr` − `lexlike`** | **46–48 pts** | **49–51 pts** |

**A rail's SSO ratio is a property of the rail AND the host**, which this
document and `bench.sh` both denied until 2026-09-15 — the harness header
claimed its back-to-back build made "the verdict a RATIO" and disclaimed only
absolute milliseconds. Different rails are bound by different subsystems, so a
different CPU allocation reprices them non-uniformly. Established rather than
assumed: the old compiler on host B reproduces host B's column, so twenty
commits moved these rails by nothing and the whole difference is the machine.

**So: never compare a rail delta to a number from a previous session, including
every number in this document.** Re-run both legs on one machine. What survives
across hosts is the GAP between two rails measured together. **That rule stands;
the `substr`/`lexlike` gap it was derived from does not.** B-2026-09-15-6 is
closed `invalid`: at `KARAC_SSO=1` the two rails cost the same (217 ms vs
215 ms), so the "gap" was a difference of two ratios with equal numerators. A
differential is more durable than a level — and still has to be a differential
between two things that actually differ.

**`vertical` is an x86-64 phenomenon** — +85% here, +1.4% on an M5 Pro at its
own filing commit (B-2026-09-14-28) — so the default-flip decision is
per-platform. (The second "blocker", the `substr`/`lexlike` gap, turned out not
to be one at all; its 3-point arm64 reading was the same arithmetic artifact
seen from the other side.) What that does not license is flipping SSO on for
arm64: the
+5.7% corpus aggregate below is an x86-64 number, the arm64 corpus has never
been swept, and two rails plus one kata are not a corpus. That sweep is the
honest next step for the arm64 side.

**Anything here dated before 2026-09-15 additionally had auto-par ON**
(`bench.sh` did not pin the control until `375118d`), which is a separate and
compounding reason not to compare against it.

**Two further rules the 2026-09-15 re-run added, both about how numbers here are
written down.** First, **read the spread, not the third digit**: the harness
prints integer milliseconds, so a rail at 127–129 ms quantizes to ±0.8% and one
at 44 ms to ±2.3%. `promote`'s long-standing `+0.8%` is *zero* — it flips sign
between samples — and `lexer`, the rail SSO's whole case rests on, is the
LOOSEST at 5 points across five samples (~−16 ± 3%, not −17.6%). Two samples
cannot show either; take three or more. Second, **auto-par does not uniformly
compress**: pinning changes the SIGN for `substr` (+73% → ≈0%) and the
MAGNITUDE for `builder60` (+2.4…3.6% → +11.7…13.6%, amplified fourfold) in
OPPOSITE directions, so the pin is a control on both counts. Table and
mechanism-hypothesis in `bench/sso/README.md`.

**Kata corpus, x86-64** (17 residual regressions, sweep already pinned): median
+1.4%, aggregate +5.7%, 4 regressed ≥5%, 2 improved ≥5%.

**Kata corpus, arm64 / macOS (M5 Pro), 2026-09-15 — first sweep on this host.**
Every figure above this line is x86-64. The flip decision has been resting on a
corpus aggregate for one of the two platforms kāra ships on; this is the other.
All 340 bench programs, both arms, 3 builds per arm with the arms **alternated**,
min of 2 runs per build, `KARAC_AUTO_PAR=0`, katas checkout pinned at
`003e06a6`:

| | arm64 | x86-64 |
|---|---|---|
| median | **1.0004** | +1.4% |
| aggregate (total corpus cycles) | **1.0012** | +5.7% |
| ≥5% worse | 24 | 4 |
| ≥5% better | 12 | 2 |
| within 5% | 304 | — |
| sink mismatches | **0 / 340** | — |

**On arm64 SSO is corpus-neutral**: 420.1 B cycles → 420.7 B. The spread is
real — p05 0.964, p95 1.076 — but it cancels, and it cancels by *weight* as well
as by count: the 12 winners save 3,048 M cycles against the 24 losers' 2,839 M.
So this is not "a small aggregate hiding a large split"; the split is there and
the two halves are the same size.

**0 sink mismatches across 340 programs** is the other half of the result. SSO
changes the `String` representation, so a wrong answer was the failure mode worth
looking for, and there isn't one on this host.

What this does **not** say: that the flip is safe on arm64. A neutral aggregate
with a ±7% tail is an argument for flipping only if the tail is understood, and
two of the 24 regressions are the blockers below. It does say the flip decision
is **per-platform**, and that the +5.7% currently blocking it is an x86-64
number that does not describe arm64.

**The two blockers, both unattributed rather than unfixed:**

- **B-2026-09-14-28** — `vertical` regresses +85% and nobody knows why. The
  de-inline probe was ruled out by measurement (removing it moved the kata 1.3
  points) and `prefix_string` was ruled out too (an exact mirror runs 9–15%
  *faster* under SSO). Largest single item in the corpus.
  **arm64 (2026-09-15): +37–39%, disjoint distributions — same sign, smaller.**
  And the decomposition contradicts this row's own suspect: instructions go
  1.4118 while cycles go 1.3734 and **IPC is flat and high in both arms (7.26 →
  7.38)**. A store-to-load forwarding stall depresses IPC; IPC did not move. On
  arm64 the cost is 41% more instructions on the read path, i.e. *work*, not a
  hazard. Checking IPC on the two x86-64 arms is cheaper than building the
  upper-bound probe and decides whether the hosts share one mechanism or differ.
  (Note for anyone re-measuring: **two katas ship a `bench/vertical.kara`** —
  this one is `1-100/14-longest-common-prefix`, not
  `301-400/314-binary-tree-vertical-order-traversal`. Selecting by filename
  measures the wrong program and reads ≈0.98.)
- **B-2026-09-15-6 is CLOSED `invalid`, and it was the wrong question.** The two
  spellings do not differ under SSO: at `KARAC_SSO=1` `substr` is 217 ms and
  `lexlike` 215 ms, 0.9% apart, samples interleaving. The "~46–52 point gap" was
  (+72%) − (+23%) — two ratios with **equal numerators** and unequal
  denominators. They differ only at `KARAC_SSO=0` (126 ms vs 176 ms).
- **B-2026-09-15-13 replaced it and is now FIXED (`9d3ceb9`).** SSO reached
  inline construction through an **opaque runtime call**
  (`karac_string_try_inline_into`); `String.substring` now emits the encoding as
  IR. The rail went **217 ms → 22 ms** on x86-64 — 5.7x faster than the
  `KARAC_SSO=0` baseline — and **13.9x** faster than that baseline on arm64. The
  `+73%` this track called its blocker was the cost of the call, not of SSO.
- **But that did NOT clear the corpus blocker, and the claim that it might is
  refuted.** This document said "if that holds on the lexer and the corpus, the
  flip decision is being made against the wrong number." Measured on arm64: it
  does not hold for `vertical`, the corpus's largest single regression, whose
  SSO=1 instruction count moves by **0.003%** — because
  `14-longest-common-prefix/bench/vertical.kara` contains **zero `substring`
  calls**. It reads through `.bytes()`, `.chars()` and `.len()`.
- **So the track has (at least) two INDEPENDENT costs**, and only one is fixed:
  1. **Construction** via `substring` — an opaque call. Fixed in `9d3ceb9`.
  2. **Reads** through `.bytes()` / `.chars()` / `.len()` — untouched by (1),
     and the whole of `vertical`'s regression (B-2026-09-14-28). A construction
     fix was never going to reach it: different path.
- **`karac_string_slice_into` is the construction path that was NOT fixed.**
  `s[a..b]` still routes through it and `lexlike` still measures +23%; it is the
  same opaque-call shape, with UTF-8 validation folded in. Tracked separately.

**Correctness is not the blocker.** The inline path is pinned by
`test_sso_de_inline_rides_the_string_growth_test` (`tests/cli.rs`), verified
non-vacuous by backing the fold out — the `KARAC_SSO=1` leg then SIGSEGVs while
the `=0` leg stays byte-identical to the oracle, which is why a default-off suite
cannot catch this class on its own.

**What a flip decision needs** has changed shape. The `substr` question is
answered, and answered against this campaign's own framing: with SSO's inline
encoding emitted as IR rather than called, the rail that was the track's worst
regression becomes its largest win (B-2026-09-15-13). **The flip has been
weighed against the cost of a call, not against the cost of SSO.** Still
missing: whether that holds beyond one rail — the lexer, the corpus, `vertical`,
arm64, none of them measured — and an attribution for `vertical`.

**Slice 2 inline construction is LIVE behind `KARAC_SSO=1`, default OFF.** It
works and it is correct on every surface probed. Both construction sites are
inline — `s[a..b]` and `String.substring`.

**THE MOTIVATING WORKLOAD NOW WINS 14.8%, AND THE FIX IS LANDED. 2026-09-12.**
Slice 2's own exit gate — re-profile the self-hosted lexer — had never been run;
every payoff figure below it was synthetic. Run on
441 KiB of real Kāra it first showed SSO **4% SLOWER**, and attributing that
regression to a single function exposed the cause: one `cap > 0` ownership gate
in `emit_vecstr_defensive_copy` still read UNSIGNED, so every inline String was
deep-copied back onto the heap at each by-value consuming call — re-spending the
`malloc` construction had just avoided. Flipping it to `SGT` measures:

| 441 KiB × 200 passes | allocations | instructions | wall, best-of-7 |
|---|---|---|---|
| `KARAC_SSO=0` | 16,292,811 | 9,190,806,670 | 1484 ms |
| `KARAC_SSO=1` before | 11,232,411 | 9,507,462,371 | 1524 ms |
| **`KARAC_SSO=1` after** | **5,128,411 (−69%)** | **7,173,722,614 (−21.9%)** | **1265 ms (14.8% faster)** |

That gate survived Slice 2's sweep because `rustfmt` puts the predicate and its
`cap` operand on different lines, so no line-oriented grep could match it.

**THE GATE WAS MASKING A CLASS OF LATENT BUGS, AND THAT IS THE OTHER HALF OF THE
STORY.** By de-inlining every String at every by-value consuming call, the
unsigned gate kept inline descriptors out of most of the compiler — every site
downstream of it had been silently exempted from supporting them. Flipping it
reddened **eight binaries** on the first attempt. The de-masking then converged
in **two** fixes, not the open-ended list it looked like:

1. an enum-payload store made through the read-only accessor (`param_own.rs`),
   which wrote a spill-slot address into a payload word; and
2. the mutation chokepoint's Vec guard (`vec_method.rs`), which tested
   `!vec_elem_types.contains_key(v)` on the false belief that the table names
   `Vec` receivers — it holds Strings too, so `push_str` on a pattern-bound
   String skipped promotion entirely.

All seven selfhost differentials pass at `KARAC_SSO=1` with all three changes in. See "SLICE 2'S OWN GATE
FINALLY RAN" below for the profile, the four hypotheses it falsified first, the
latent bug the flip exposed, and the near-miss where a stale `karac` nearly got
the whole thing reverted as a no-op.

**The synthetic transient/retained table below was RE-MEASURED and is
UNCHANGED** — and finding out why is the most useful thing the re-run produced.
Not one of those benchmarks makes a by-value consuming call, so the gate never
fired in any of them; they modelled *what is done with a slice* and omitted *how
it travels*, and the second axis is where the 2.3 billion instructions were. A
shape taxonomy without parameter passing cannot predict a compiler front end.
The same entry records a second trap: on those benchmarks SSO runs **36% fewer
instructions and 63% slower**, so an instruction delta must never be quoted as
evidence of speed here.

This doc is the campaign's living handoff: layout decision (settled), staged
slice plan, the tag-aware accessor work list, and the verification matrix. Scoped 2026-06-12; Slice 1 landed
2026-07-09; Slice 2 construction 2026-09-11; Slice 3's FFI boundary opened
2026-09-12.

**That second blocker — the move-suppression disarm — is also FIXED
(2026-09-12).** `UseAfterMove` is advisory *because `cli.rs` promises the binary
is memory-safe anyway*, and at `KARAC_SSO=1` a moved-from short String was
reading wild memory, so the flip would have falsified that promise. Traced,
resolved and fixed: the disarm zeroed `cap` alone, which for an inline
descriptor destroys the tag and length together; it is now tag-aware and blanks
the descriptor on the inline path only, leaving heap and static sources exactly
as before. See "MOVED-FROM STRINGS READ WILD, NOT STALE" under the Slice 3
entry — including why the same bug printed silence in one shape and SIGSEGV'd in
another.

**CI now runs one `KARAC_SSO=1` fixture** —
`tests/cli.rs::test_sso_inline_string_survives_the_env_ffi_boundary`, added with
the Slice 3 entry below. Before it, no COMPILED Kāra program in the suite ever
emitted an inline descriptor — the runtime's own unit tests build them through
`new_inline`, but nothing exercised codegen's construction path — so every SSO
regression to date reached `main` through a green gate set and was caught only
by a human remembering to run the second leg by hand.
One fixture is not coverage; it is the first one, and the pattern it establishes
(shell out to `karac` so the env var is per-fixture, and build the String with
`substring` so it is actually inline) is what the rest of the surface should
copy.

**Layout decision — SETTLED (Slice 1):** Option A, **inline flag = sign bit (bit 63) of
`cap`**. Three states discriminated by `cap` read as `i64`: static-heap (`cap == 0`),
owned-heap (`cap > 0`), inline (`cap < 0`). Encoding the flag as the sign bit is
load-bearing — it collapses the buffer-free decision to the single signed compare
`cap > 0`, which is a *provable no-op today* (no code has ever produced a `cap` with
bit 63 set; a real capacity never approaches 2^63) yet is forward-correct for inline.
Full folly-`fbstring`-style 23-byte inline overlay: bytes `0..=22` hold data, byte 23
(`cap`'s MSB) holds `flag | length`. The **executable contract** lives in
`runtime/src/sso.rs` (exhaustively unit-tested; the single source of truth codegen
mirrors); the codegen tag helpers are in `src/codegen/sso.rs`.

## Why this campaign

Profiling the self-hosted lexer ([`selfhost-lexer-profile.md`](selfhost-lexer-profile.md))
found per-token **allocation** is the #1 remaining codegen-perf cost. After the
string-literal `match` dispatch lever shipped (commit `5adf2e90`, 111.7 B → 66.9 B
instructions, Rust gap **4.58× → 2.74×**), `malloc`/`free` is now the **#1 self-time
leaf**. The dominant source: per-token `substring` (`selfhost/src/main.kara:1239`,
`:1260`, …) returns an **owned `String` copy**. Most lexemes — identifiers, keywords,
short tokens — are **short** (< ~23 bytes), so they fit inline.

**SSO is the corpus-wide lever.** Inline short strings directly in the `{ptr,len,cap}`
struct → **no `malloc` when `len ≤ N`**. Unlike a lexer source rewrite (which only fixes
the lexer), SSO makes *every* short-string allocation in *every* Kāra program disappear —
the principled "natural `substring` code stays fast" fix that matches the project's
fix-the-compiler-not-the-workload rule. It is the lever to **close the gap and go
further** (the user's framing, 2026-06-12: "close it anyway or go further — only question
is now or later").

**Why later, not now (the real reason).** SSO has the **largest blast radius of any
change in the String subsystem**. It re-lays-out the struct that *every* String op
assumes; a subtle layout miscompile is *silent data corruption* — exactly the failure
class the guardmalloc/LSan discipline exists to catch. It deserves a fresh full context
window and deliberate staging, not a long-session bolt-on. "Later" is cheap **because
this doc preserves the warm context.**

## The central constraint (settle the layout around this)

`String`, `str`, `Vec`, and `VecDeque` **all share one LLVM struct** —
`vec_struct_type()` = `{ ptr: *u8, i64 len, i64 cap }` (24 bytes), defined at
`src/codegen/types_lowering.rs:337`. Confirmed shared at e.g.
`types_lowering.rs:1239`, `declarations.rs:4318`, `control_flow_match.rs:1654`.

**SSO must not change `Vec` semantics.** Therefore: **encode SSO *within* the existing
24-byte struct via a tag — do not split `String` into its own type.** A uniform
tag-aware data-ptr accessor is then *correctness-safe for `Vec` too*: `Vec` never sets
the inline tag, so the accessor always takes its heap path for a `Vec` and behaves
identically to today. (Threading the Kāra-level type to keep `Vec` on the branch-free
raw path is a *perf* refinement, not a correctness requirement — see Slice 3.)

### Layout decision — DECIDED (Slice 1): Option A, flag = sign bit of `cap`

- **Option A — in-struct tag (CHOSEN).** Reuse the 24 bytes. Inline form stores up
  to 23 bytes of data overlapping the `ptr`/`len`/`cap` words (folly `fbstring` style),
  with **bit 63 of `cap` (the sign bit)** as the inline flag. `Vec` leaves the flag clear.
  Minimal type churn — String stays `vec_struct_type` everywhere.
  - *Why the sign bit of `cap` and not the low bit of `ptr` or a bit of `len`:* the low
    bit of `ptr` is unsafe (a `.rodata` string-literal buffer is not guaranteed ≥2-byte
    aligned), and overlapping `len` would break the invariant that a `cap`-only signed
    compare distinguishes all three drop states. The sign bit of `cap` gives one predicate
    — `is_owned_heap ⇔ (i64)cap > 0` — that is simultaneously (a) a no-op today, (b)
    correct for inline (`cap < 0` skips free), (c) correct for static (`cap == 0` skips),
    and (d) identical to the old `UGT` gate for `Vec` (whose cap is a non-negative count).
- **Option B — split `String` into a distinct LLVM type.** Cleaner semantics but enormous
  churn: String currently *is* `vec_struct_type` across ~15 files, the by-value ABI, the
  recursive-drop type-identity checks (`llvm_ty_is_vec_struct`), and dispatch. **Rejected.**

### Hazard: the `cap == 0` "static literal, don't free" convention

Today `cap == 0` marks a String whose buffer is static `.rodata` (string literals;
`StringLit` at `exprs.rs:60`; the dispatch literal-pattern builds `cap_zero` at
`control_flow_match.rs`). Drop frees only `cap > 0`. SSO adds a **third state**, so the
encoding must distinguish all three cleanly:

| state | meaning | drop action |
|---|---|---|
| static-heap (`cap == 0`, flag clear) | literal, buffer in `.rodata` | none |
| owned-heap (`cap > 0`, flag clear) | malloc'd buffer | `free(ptr)` |
| **inline (flag set)** | bytes live in the struct | none (no buffer) |

## Work list — the tag-aware-accessor surface

Raw field-0 data-ptr reads (`extract_value(_, 0)` / `build_struct_gep(vec_ty, _, 0)`) and
field-1/2 len/cap reads, spread across ~15 codegen files. Counts (field-0 reads, `grep`
2026-06-12) as a scale guide — **not all are String; many are `Vec`** (uniform accessor
is safe for both):

```
vec_method.rs   79    runtime.rs      23    clone_drop.rs   22
http.rs         15    method_call.rs  13    expr_ops.rs     13
assoc_call.rs   11    reduce.rs        9    collections.rs   8
control_flow_for 7    tcp/synth_display/file 6    tls 5    exprs 4
```

Find the full set with:
`grep -rn "extract_value.*, 0\|extract_value.*, 1\|build_struct_gep(vec_ty" src/codegen/`.

**Must-not-miss sites:**
- The **string-match dispatch tree** just shipped (`control_flow_match.rs`,
  `emit_string_dispatch` / `emit_len_bucket` / `emit_byte_group`) reads
  `extract_value(sv, 0/1)` raw for ptr/len → route through the accessor.
- **Drop/clone** (`clone_drop.rs`, 22 sites) keyed on `cap > 0` → become tag-aware
  (inline ⇒ no free; inline clone ⇒ struct copy, no buffer alloc).
- **`Vec.push` inline grow** (`vec_method.rs:690`) — Vec path, must stay raw/unaffected;
  good canary that the accessor is a true no-op for Vec.
- **Runtime/FFI by-value ABI** (`runtime/src/*`, plus codegen `runtime.rs`, `file.rs`,
  `http.rs`, `tcp.rs`, `tls.rs`, `json.rs`): any runtime fn receiving a String by value
  (`println`, file write, http body, …) must also decode the tag. **Runtime-side change,
  not just codegen.**

## Staged slice plan

Each slice is independently shippable and gated on the full String + ASAN suite; the
perf payoff lands in Slice 2.

- **Slice 0** — *this spike.* Scope + layout-decision criteria. ✅ **DONE.**
- **Slice 1 — layout + accessors (no behavior change).** ✅ **DONE (2026-07-09).**
  Layout settled (Option A, sign bit of `cap`). Shipped:
  - `runtime/src/sso.rs` — the executable encoding contract on `RuntimeKaracString`
    (`is_inline` / `is_static` / `is_owned_heap` / `byte_len` / `data_ptr` / `as_bytes` /
    `new_inline` + `INLINE_CAPACITY = 23`), exhaustively unit-tested (all three states,
    every inline length 0..=23, boundary rejection, layout-pin). This is the single
    source of truth codegen mirrors, and the FFI-decode path Slice 3 will call.
  - `src/codegen/sso.rs` — codegen tag helpers `sso_string_is_owned_heap` (SGT, wired)
    and `sso_string_is_inline` (SLT, ready for Slice 2).
  - **Free-gate hardening:** the six `{ptr,len,cap}` buffer-free gates (`emit_string_drop_fn`,
    `emit_vec_drop_fn` in `clone_drop.rs`; the overwrite-free, enum-payload-free, and live
    `FreeVecBuffer` gates in `runtime.rs`; the enum `VecOrString` payload drop in
    `synth_drop.rs`) now route through `sso_string_is_owned_heap` (`UGT`→`SGT`). Proven
    no-op: full suite + 562 ASAN cases + 2103 codegen E2E + 153 par_codegen all green,
    zero perf delta.
  - **Follow-up gate hardening — GATE HALF landed 2026-07-11 (proven no-op).** The
    remaining String-buffer `was_heap` gates now route through
    `sso_string_is_owned_heap` (`UGT`→`SGT`): the three from-slice builders
    (`tss`/`efs`/`tefs` in `vec_method.rs`) **and** `emit_string_buffer_grow` (the
    `String.push`/`push_str` realloc-or-fresh gate, which the original list missed —
    an inline string must never be `realloc`'d, it must take the fresh-malloc path).
    So **every** String buffer-free/realloc gate is now inline-safe. Verified full
    suite + 590 ASAN/LSan + 2137 codegen E2E + 158 par_codegen green, zero perf delta
    (a real `cap` never has bit 63 set, so `SGT`≡`UGT`). **STILL DEFERRED to the flip
    (the coordinated, only-testable-with-construction half):** each of these gates' memcpy
    *source* (`data`, the raw field-0 load in the grow/fresh path) must become the
    tag-aware `string_data_ptr` — that is coupled to inline construction and can't be a
    no-op. `FreeSoaGroups` (`runtime.rs`) is **NOT** in scope (its `cap` is a SoA group
    count, never a String descriptor). Grow gates comparing `new_len`/`doubled` to `cap`
    are unrelated and must stay `UGT`.
- **Slice 2 — inline construction (the win).** `substring`, runtime-built `StringLit`,
  concat, `to_string`, `push_str` result → build **inline** when `len ≤ 23`. Concrete
  checklist for the fresh session:
  1. ~~Convert the remaining `was_heap` gates to `sso_string_is_owned_heap`~~ —
     **GATE HALF DONE 2026-07-11** (proven no-op; `tss`/`efs`/`tefs` + the missed
     `emit_string_buffer_grow`, so every String buffer gate is inline-safe).
     **Remaining:** fix each gate's memcpy *source* — the raw field-0 `data` load in
     the grow/fresh path (`vec_method.rs` `emit_string_buffer_grow` line ~133; the
     `tss`/`efs`/`tefs` builders' post-grow memcpy) — to `string_data_ptr` (step 2's
     accessor), so an inline source is copied from the struct-self pointer. This is
     the coupled, only-testable-with-construction half.
  2. **Tag-aware `string_data_ptr` / `string_len`** in codegen (mirror `runtime/src/sso.rs`)
     — **SSA FORM + THE DISPATCH TREE DONE (2026-09-11); the slot form and the rest of the
     sweep remain.** The whole SSO surface is now behind **`KARAC_SSO` (default OFF)**, which
     is what makes the remaining sweep landable incrementally: with the gate off each
     accessor emits *literally the same inkwell calls with the same value names* it replaced,
     so an intermediate commit is **byte-identical IR** rather than merely "the branch is
     never taken at runtime". That distinction is load-bearing — routing the string-`match`
     dispatch tree through a live `load`+`icmp`+`select` would put a cost on the hottest
     String read in the corpus (the `5adf2e90` lever) for every commit between here and the
     flip, with no malloc saving yet to pay for it. `KARAC_SSO=1` turns the whole thing on,
     which is how the sweep is tested ahead of the default flip.
     Shipped: `sso_string_parts_from_value` (the SSA form — spills to an **entry-block**
     alloca so the inline self-pointer has an address; entry placement is load-bearing twice
     over, the address is stable for the whole function and a call site inside a loop
     allocates once rather than per iteration), `sso_select_len` (decodes the inline length
     from `cap`'s high byte with a **logical** shift — a sign-extending one smears the flag
     bit into the length) and `sso_load_cap`. Step 4's dispatch tree
     (`emit_string_dispatch`) is routed through it.
     **The sharp edge the rest of the sweep must respect:** the returned pointer is valid
     only for an *immediate* read (a memcpy source, a comparison, a runtime call that
     copies). An inline descriptor is self-referential, so storing that pointer into
     anything outliving the frame dangles as soon as the value is copied elsewhere.
     Verified: a string-`match` program built at `KARAC_SSO=0` and `=1` produces **identical
     output** but **different binaries** — the gate is real (the on-path is not dead code)
     and behaviour is unchanged (no inline strings exist yet, so the tag select always
     yields the heap value). Full `--features llvm` suite green **both ways**: 16,739 tests
     across 109 binaries, `codegen` 3772/0 and `memory_sanitizer` 1589/0, with `gpu_e2e` the
     only red binary (the sanctioned missing-optional-archive state).
     Original note, still describing the remainder:
     a *slot* form (GEP field-0 address for inline, load field-0 for heap) is clean; a
     *value* (SSA) form must **spill to an alloca** to take the inline self-pointer — this
     is the main new complexity. Sweep the field-0 (data-ptr, ~224 sites) and field-1
     (len, ~204 sites) reads *on Strings* onto these (many are `Vec` — the accessor is a
     safe no-op there, but threading the Kāra type to keep `Vec` branch-free is Slice 3).
  3. ~~**Clone becomes tag-aware:** inline source ⇒ struct copy, no malloc~~ —
     **DONE (2026-09-11, proven no-op).** Both String-clone entry points now branch on
     the tag *first*, before any field read: the runtime `karac_string_clone`
     (`runtime/src/clone.rs`) and codegen's fallible `emit_string_try_clone_fn`
     (`clone_drop.rs`). An inline source is copied as a verbatim 24-byte descriptor —
     the bytes travel inside it and the destination's self-pointer re-derives from its
     own address — so nothing is allocated. `clone.rs` dropped its private duplicate of
     the `{ptr,len,cap}` layout and now aliases `RuntimeKaracString`, which is what
     carries the `sso.rs` accessors; a second accessor-less copy of the layout is how
     the two halves would drift. The two `EQ cap, 0` sites in `emit_vec_clone_fn` /
     `emit_vec_try_clone_fn` are **Vec** clones and stay as they are (a Vec never sets
     the flag); the String path never reached them. Regression tests
     (`clone_of_inline_source_is_a_struct_copy`,
     `inline_clone_data_ptr_follows_the_destination`) feed `karac_string_clone` an
     inline source built by the reference encoder `new_inline`, which is what makes the
     runtime half testable *before* construction exists — verified to FAIL with the new
     branch disabled, so they discriminate. The codegen half has no such handle and is
     only exercisable once construction lands.
  4. ~~Route the **string-match dispatch tree** through `string_data_ptr` + `string_len`~~
     — **DONE (2026-09-11)** as part of step 2: `emit_string_dispatch`'s two raw
     `extract_value(sv, 0/1)` reads are now the single `sso_string_parts_from_value` call.
     `emit_len_bucket` / `emit_byte_group` take the decoded ptr+len as arguments from it, so
     they needed no change of their own.
  Gate: **re-profile the self-host lexer** (instruction count + `malloc` leaf share must
  drop), full ASAN + **Linux/LSan** (SSO touches every free path — authoritative leak
  gate).
  **RAN 2026-09-12. Fails as the tree stands; passes with a change that is not
  yet landable.** First run: the `malloc` share dropped as predicted (31% fewer
  allocation calls) but the instruction count went **UP** 3.4% and wall time
  4.0% — a conjunction with only one half passing. Attributing that failure found
  an unswept unsigned `cap > 0` gate; flipping it passes both halves
  (allocations **−69%**, instructions **−21.9%**, wall **14.8% faster**) and
  reddens eight binaries, because that gate was masking latent inline bugs. See
  "SLICE 2'S OWN GATE FINALLY RAN" below.
- **Slice 2 progress — INLINE CONSTRUCTION IS LIVE behind `KARAC_SSO=1` (2026-09-11).**
  `s[a..b]` now builds an inline String when the slice fits the 23-byte overlay,
  allocating nothing. Default remains OFF; the read sweep is ~1/3 done.
  - **The encoder lives in the RUNTIME, not codegen** — a deliberate departure from
    this doc's original sketch. `karac_string_slice_into(data, len, start, end, out)`
    validates through the same `slice_validate` as `karac_string_slice` and writes a
    complete descriptor; codegen calls it and loads the 24 bytes back, owning no
    encoding at all. Two reasons: (a) the allocating path does bounds AND UTF-8
    char-boundary checking, both fatal, and an inline fast path in codegen that
    skipped them would have been a silent soundness regression; (b) `new_inline` is
    already the exhaustively-tested source of truth, and re-emitting the byte-packing
    as IR would be a second implementation free to drift — a codegen↔runtime layout
    mismatch is exactly the silent corruption this campaign's staging exists to
    prevent. It is not a new call either: the allocating path already made one. The
    win is removing the *malloc*.
  - **MUTATION PROMOTES rather than going tag-aware** (`sso_deinline_in_place`). A
    mutating op reads `len`/`cap` raw and repeatedly — growth test, destination
    offset, aliasing rebase, post-copy length store. The failure was QUIET, not loud:
    `needs_grow` is `UGT(new_len, cap)`, an inline `cap` is negative, so as unsigned
    it is enormous, the grow is SKIPPED, and the copy lands past the end of the
    descriptor. Promoting once at the head leaves every read downstream byte-for-byte
    the pre-SSO code. A short mutated string loses its inline win, which is the right
    trade — the corpus's short strings are overwhelmingly *read* — and it is what
    folly's `fbstring` does.
  - **SLICE 1'S "EVERY String buffer-free gate is now inline-safe" CLAIM WAS WRONG.**
    16 `is_heap` gates were still `UGT(cap, 0)`; an inline `cap < 0` reads as an
    enormous unsigned, so each would have freed the DESCRIPTOR'S OWN ADDRESS. 14 are
    now `SGT` (the 2 SoA group-count gates stay, per this doc's own exclusion). Five
    IR-assertion tests in `tests/codegen.rs` pinned the old `icmp ugt` text and were
    updated, with a note at the assertions so nobody "restores" `ugt`. **Consequence
    for the campaign's staging rule:** this commit is therefore NOT byte-identical IR
    at SSO=off — it is *semantically* identical with 14 predicates flipped.
  - **A latent UB in Slice 1 was found and fixed:** `RuntimeKaracString::as_bytes`
    called `from_raw_parts(null, 0)` for the canonical empty String `{null, 0, 0}`,
    which is UB at length zero and aborts under the debug precondition check. It had
    only test callers, so it would have become live the moment Slice 3 wired it into
    the FFI decode.
  - **The sweep was PROBE-DRIVEN, and that is the transferable lesson.** Each
    `KARAC_SSO=1` crash named exactly one unswept site — far more reliable than
    eyeballing ~430 candidates. It is also how the *class* boundary was found: a
    mechanical rewrite of 119 ptr+len PAIRS missed `String.len()`, which is a
    **len-only** read, and `f_str_slice` silently returned 0 instead of 3
    (`par_codegen::test_e2e_autopar_joined_range_slice_binding`).
  - **Remaining sweep (superseded by the round-2 entry below — read that first):**
    108 `struct_gep(vec_ty, _, 1)`, 57 `extract_value(_, 1)`, 69
    `extract_value(_, 0)`. Round 2 re-counted these with a balanced-paren scanner
    and classified them: of 88 field-1 READ sites on the shared struct, most are
    `Vec`-only methods (`sort`, `pop`, `retain`, …) that `String` does not have, so
    the real String-relevant remainder is far smaller than the raw count suggests.
  - Gates at this commit: fmt + both clippy legs green; `--features llvm` **SSO=off**
    16,001 passed (red: the load-flaky `signalling_karac_run_does_not_orphan_the_jit_runner`
    and `gpu_e2e`'s missing optional archive); **SSO=on** 16,739 passed, `gpu_e2e` the
    only red binary.

- **Slice 2 round 2 — correctness advanced, and the first PAYOFF MEASUREMENT is
  NEGATIVE (2026-09-11).** The probe now agrees byte-for-byte at both settings
  across three receiver forms. Then the first end-to-end measurement was taken,
  and it says the campaign **as currently built is a net wall-time LOSS on the
  shape it was designed for.** That is the headline; the rest is why.

  - **Routed this round**, each String-only so `Vec` pays nothing:
    `karac_hash_String` / `karac_eq_String` (Map/Set keys — the
    highest-consequence pair, since a wrong digest puts equal keys in different
    buckets where they never meet to be compared), the nested-String display leaf
    in `synth_display` (`Vec[String]` elements, struct fields — distinct from the
    two top-level print arms), `for c in s`, `Secret.ct_eq`, 11 receiver
    `(ptr, len)` pairs in `vec_method.rs`, and the len-family SSA arm in
    `method_call.rs`.
  - **A LENGTH NEEDS NO SPILL.** `cap` is field 2 of the aggregate already in
    hand, so a tag-aware length is a shift, a mask and a select over registers.
    Only a DATA POINTER needs the descriptor's address. This splits the remaining
    sweep by COST, not only by correctness: the 74 `extract_value(_, 1)` sites are
    cheap, the 94 `extract_value(_, 0)` ones are not.
  - **`emit_string_try_clone_fn` was deliberately left alone** — it already
    dispatches on the `cap` tag. A mechanical rewrite would have "routed" it into
    nonsense. Classifying each candidate beat transforming all of them, the
    counter-lesson to the previous round's 119-site mass rewrite.

  ### THE RECEIVER'S FORM SELECTS THE CODEGEN PATH

  The most transferable finding here. `src[0..5].is_empty()` dispatches through
  B-2026-08-18-22's borrowed scalar-reader arm and produces a `{ptr, len, cap = 0}`
  view — **never an inline descriptor**, so it cannot exercise the tag in either
  direction. `let t = src[0..5]; t.is_empty()` reaches the len-family SSA arm in
  `method_call.rs` instead, and THAT one was broken: it answered `true` for a 3-
  and a 5-byte inline String, `false` for 9 and 19 — exactly tracking whether byte
  8 happened to be occupied, because an inline `len` field spans bytes 8..=15 and
  `new_inline` zero-fills before copying.

  It survived a green 28-surface probe, and then survived a first fix aimed at the
  `"is_empty"` arm in `vec_method.rs` — the wrong site. **Testing one receiver form
  certifies one path.** The probe now runs every surface in three: subscript in
  receiver position, `let`-bound owned, and passed across a function boundary.

  Two other probe "findings" this round were the PROBE's bugs, recorded so nobody
  re-derives them: a binding reused across `Vec.push` and `Map.insert` (both take
  ownership) read as an SSO divergence, and `s[a..b].substring(..)` failing codegen
  at BOTH settings, which is B-2026-08-18-22's deliberately deferred half. Neither
  was filed. **Rule: a probe result is evidence about the probe until the probe is
  proven to reach the code it targets.**

  ### The payoff measurement — three workloads, and the answer flips

  > **RE-MEASURED 2026-09-12 and UNCHANGED — these rows stand.** They were
  > briefly marked stale on the theory that the unsigned `dcopy.owned` gate had
  > depressed them. It had not: none of these benchmarks makes a by-value
  > consuming call, so the gate never fired in any of them, and the allocation
  > counts re-measure byte-identical. See "The synthetic tables RE-MEASURED"
  > below — including why that is the sharpest available explanation of why this
  > corpus mispredicted the real workload, and why an instruction-count delta
  > must not be quoted as evidence of speed here.

  AOT, archives matching `3833ff8`, best-of-N wall time (best-of, not mean: the
  container is noisy and the minimum is the stabler statistic):

  | workload | allocations 0 → 1 | `SSO=0` | `SSO=1` | |
  |---|---|---|---|---|
  | **transient**, 3-byte literal — slice a token, compare, discard, 1M iters | 1,000,009 → 9 | 22 ms | 31 ms | **−41%** |
  | **transient**, 16-byte literal — as above, but both legs call `bcmp` | 1,000,009 → 9 | 21 ms | 25 ms | **−19%** |
  | **retained** — slice a token, push into a `Vec[String]`, keep, 200k iters | 200,012 → 12 | 16 ms | **10 ms** | **+37%** |

  **The premise holds, but only for RETAINED strings.** A short-lived
  `malloc`/`free` pair is a glibc tcache hit — far cheaper than "allocation is the
  #1 self-time leaf" suggests when the buffer is freed in the same loop iteration
  it was made in. Retain the strings so tcache cannot recycle them and the picture
  inverts: 200k allocations become 12, and the workload runs **37% faster**.

  **CORRECTION (2026-09-12).** This entry originally continued: "That matters for
  this campaign specifically, because the self-hosted lexer *keeps* its token
  texts — it does not slice-and-discard." That was read off `lexer.kara`, never
  measured, and it is wrong in the half that matters — see "THE LEXER IS NOT THE
  'RETAINED' SHAPE" below. The lexer is a mix, and the allocation saving and the
  read-path cost do not split along the same line.

  The two transient rows isolate WHY that leg loses. Their only difference is the
  compared literal's length, chosen so the 16-byte row emits `call bcmp@plt` on
  BOTH legs and the 3-byte row does not:

  * **~5 ms of the 9 ms is lost compare folding.** At SSO=0 the 3-byte compare
    folds to `movzwl`/`xor`/`movzbl`/`xor` — four instructions, no call. At SSO=1
    the pointer is a `cmovs` between the heap pointer and the descriptor's own
    address, so LLVM can no longer see where the bytes are and calls `bcmp`.
  * **~4 ms is residual read-path overhead** that survives even when both legs
    call `bcmp`. **A million removed allocations do not pay for it.** So fixing
    the folding alone would NOT make the transient leg a win — it would take it
    from −41% to about −19%.

  The disassembly on the losing leg also showed a second, separate inefficiency:

  1. **The tag-select makes the data pointer OPAQUE to LLVM.** At SSO=0 the 3-byte
     compare folds to `movzwl`/`xor`/`movzbl`/`xor` — four instructions, no call.
     At SSO=1 the pointer is a `cmovs` between the heap pointer and the
     descriptor's own address, so LLVM can no longer see where the bytes are and
     emits `call bcmp@plt`. An inlined 4-instruction compare became a PLT call; at
     ~25 cycles over 1M iterations that is the right order of magnitude for the
     whole 9 ms. (Not yet isolated — the `lexlong` variant, where both legs call
     `bcmp`, is built and unmeasured.)
  2. **The descriptor makes a pointless round trip.** `compile_string_slice` has
     `karac_string_slice_into` write the descriptor to an alloca, then LOADS it to
     an SSA aggregate; the consumer's `sso_string_parts_from_value` STORES it
     straight back to a second alloca. Six memory ops to return to where the callee
     already put it.

  (That round trip was then fixed and measured to be worthless — see below.)

  ### The landed construction site MISSES the motivating workload

  The self-hosted lexer's per-token copy is `self.src.substring(a, b)`
  (`selfhost/src/lexer.kara:275`, `:513`, `:651`, `:986`, …) — the **method**, not
  the `s[a..b]` subscript. `substring` mallocs inline in codegen (its own clamp +
  `malloc` + `memcpy`), a separate construction site entirely. Measured: the same
  benchmark written with `.substring()` allocates **1,000,046 times at
  `KARAC_SSO=1`**. So the commit that "turned on inline construction" does not
  touch the profile that motivated this campaign.

  ### Consequence for the staged plan

  **Step 5 ("flip the default and prove the payoff") cannot succeed as stated** and
  should not be attempted until the read path stops costing more than the
  allocation it saves.

  **ELIMINATING THE ROUND TRIP WAS TRIED AND IS WORTHLESS — do not redo it.**
  Implemented as provenance read off the IR (if the value is a `load`, reuse the
  pointer it loaded from; bail on any intervening `store`/`call` or a load from
  another block). It worked: the three redundant `mov …,0x18(%rsp)` stores
  disappear from the loop. It bought **nothing** — best-of-7 wall time 31 ms → 32
  ms, and retired instructions went the WRONG way, **162,335,574 → 168,335,584**.
  Deterministic counter, so that is not noise. The change was reverted rather than
  landed: a soundness-critical IR scan for no measured benefit is a bad trade.

  A plausible mechanism, offered as a HYPOTHESIS and not measured: reusing the
  variable's own slot takes the address of `t`, which can block mem2reg from
  promoting it, where the dedicated spill alloca kept the address-taking confined
  to a slot nothing else used. If someone revisits this, test that first.

  **Correction to an earlier draft of this entry**, which claimed the regression
  was "to a first approximation entirely" the opaque pointer. The 16-byte-literal
  row measures that: it is about **half**, and the other half is read-path
  overhead a million removed allocations still fail to cover. Fixing the folding
  is worth doing and is not sufficient.
  1. **Branch rather than select where the pointer feeds a length-known compare**,
     so each arm has a concrete pointer LLVM can still fold. Worth ~5 ms of the
     9 ms on the transient leg; leaves it still ~19% slower.
     **SUPERSEDED 2026-09-12 — do not start here.** On the self-hosted lexer the
     compare-folding effect is real but accounts for **0.9%** of the regression
     (`__memcmp_avx2_movbe`: present at `KARAC_SSO=1`, absent at `=0`,
     2,863,800 instructions of a +316,655,701 total). This lever was sized on a
     benchmark built to isolate it; those proportions do not survive a program
     whose String reads are diffuse.
  2. ~~**Make `substring` a construction site**~~ — **DONE**, see below.
  3. Only then re-measure, and only then consider the default.

  ### `String.substring` is now a construction site (2026-09-11)

  The site the profile actually uses. Same two-shape split as `s[a..b]`:

  > **NOT re-measured (2026-09-12).** The surviving harness file allocates
  > 1,000,009 → 9 where this table records 1,000,047 → 47, so it is a different
  > program and these rows could not be reproduced. They stand as originally
  > measured. The sibling table above WAS re-measured and came back unchanged.

  | shape | allocations 0 → 1 | `SSO=0` | `SSO=1` | |
  |---|---|---|---|---|
  | transient — substring, compare, discard | 1,000,047 → **47** | 9 ms | 10 ms | −11% |
  | **retained** — substring, push into a `Vec[String]` | 200,012 → **12** | 14 ms | **9 ms** | **+36%** |

  The retained row reproduces the `s[a..b]` benchmark's +37% almost exactly, which
  is the consistency worth having: the win is a property of the SHAPE, not of one
  benchmark.

  **The entrypoint is deliberately narrow, and the reason is the buffer contract.**
  The obvious design — one runtime constructor shared with
  `karac_string_slice_into` — dies on inspection: `String.substring` allocates
  exactly `n` bytes through `karac_alloc_or_panic` with no NUL, while
  `karac_string_slice` allocates `n + 1` through Rust's allocator and
  NUL-terminates. Sharing would force one site's contract onto the other and
  silently change which allocator a buffer came from, which the free path has to
  agree with. So `karac_string_try_inline_into(src, n, out) -> i8` owns only the
  inline ENCODING: it writes `out` and answers 1, or answers 0 and leaves `out`
  alone so the caller keeps its own heap arm. **Returning the verdict rather than
  the threshold is what keeps `INLINE_CAPACITY` out of codegen** — codegen
  branches on the answer and never learns the number.

  **A construction fast path is most dangerous where it makes validation
  skippable.** `substring` does a UTF-8 boundary check (B-2026-08-14-19: an
  unchecked cut inside a codepoint put invalid UTF-8 on stdout and made `karac
  run` and `karac build` disagree about a substring's LENGTH). The inline branch
  sits after that check and takes the same `start`/`end` — measured, not asserted:
  `"日本語".substring(0, 2)` faults identically at both settings and under
  `--interp`.

  ### Turning it on broke the self-hosted compiler, and that is the method working

  The first `KARAC_SSO=1` gate after this change went red on **six** binaries —
  all four self-host tests, the ASAN suite, and the known coroutine flake. Every
  string literal's text came back EMPTY:

      Kāra: "0 7 1 1 STR"      Rust: "0 7 1 1 STR hello"
      Kāra: (str  @0:4)        Rust: (str hi @0:4)

  `push_str`'s **argument** was read with raw `extract_value(src_val, 0/1)`. For
  an inline argument, field 0 is the first eight CONTENT bytes reinterpreted as a
  pointer and field 1 is content bytes 8..=15 — zero for anything under 8 bytes —
  so it appended nothing, silently. The lexer's string-literal path is
  `value.push_str(self.src.substring(run_start, self.current))`, so making
  `substring` inline lit a fuse that had been sitting there since the read sweep.

  **The site scanner's blind spot is why it was missed**, and it is worth naming
  because it is not a reasoning error. The scanner matched
  `build_extract_value(NAME, 0` with `NAME` as `[\w.]+`, which silently skips
  `build_extract_value(src_val.into_struct_value(), 0` — the parens break the
  match. Rewritten to accept an arbitrary aggregate expression, it found exactly
  one more String site of the same shape (`try_push_str`). Two scanner gaps have
  now each hidden a real site; a regex over call syntax is a lower bound, never a
  census.

  **PER-ARM PROMOTION LEAKS BY CONSTRUCTION — it is now a chokepoint.** The
  deeper find: `try_push_str` mutates its receiver and had NO
  `sso_deinline_in_place`. The previous round added promotion to `push_str`,
  `push` and `reserve` one arm at a time and simply missed it, and nothing failed
  until a new construction site made inline values common. Promotion now happens
  once in `compile_vec_method`, keyed off an over-broad list of mutating method
  names: a false positive costs one predictable compare, a false negative costs
  silent corruption. A `Vec` guard keeps that compare off the hot `Vec` paths
  while erring toward promoting whenever the receiver is not positively known to
  be a `Vec` — which is what covers a `ref String` parameter, filtered out of the
  String-typed tables by the `Ref(Str)` gap B-2026-08-18-22 hit.

  After the fix, both legs: **109 binaries, 16,801 passed, zero red.**

  Gates at this commit: fmt OK, both clippy legs green, 109 binaries per leg.
  SSO=off 16,028 passed, 2 red (`signalling_karac_run_does_not_orphan_the_jit_runner`
  = B-2026-09-09-1, and `coroutine_ws_over_tls_concurrent_handlers_all_execute`);
  SSO=on 16,801 passed, zero red. Both reds are on the leg this work cannot affect.

- **Slice 3 — sweep + runtime/FFI decode.** Remaining raw sites; runtime decode
  (`println`/file/http/tls/json); thread the Kāra type to keep `Vec` branch-free for perf.
  Gate: corpus re-bench.

- **Slice 3 progress — the FFI boundary: one gap MEASURED, one REASONED, and
  two different fix shapes (2026-09-12).** A String crossing into a runtime
  extern is the same failure class as the `push_str` argument bug that broke
  the self-hosted compiler, and it fails the same way — silently — because
  field 0 of an inline descriptor is a plausible-looking pointer built from the
  string's own first eight bytes.

  ### The gap that was measured: `extract_string_ptr_len`

  `tls.rs::extract_string_ptr_len` — a helper whose own doc comment says it
  extracts `{ptr, len}` from "a Kāra `String` struct value" — was still the two
  raw `extract_value`s, and it has **nine callers**: TLS listener bind
  (cert / key / addr), TLS client connect (addr / server-name / roots-PEM),
  `env.set` (name, value) and `env.var` (name). `env.set` is the one that is
  cheap to probe, and it is loud:

  ```
  let name = src.substring(0, 18);        // 18 bytes -> inline
  env.set(name, src.substring(19, 28));

  KARAC_SSO=0  ->  short-val
  KARAC_SSO=1  ->  memory allocation of 5715719208697159504 bytes failed
                   ... karac_runtime_env_set (runtime/src/lib.rs:1059)
  ```

  That byte count is not noise, it is **the name's own bytes 8..=15, plus one**.
  `5715719208697159504 - 1` is `0x4F5250564E455F4F`, whose little-endian bytes
  are `"O_ENVPRO"` — exactly `"KARAC_SSO_ENVPROBE"[8..16]`. The `+1` is
  `CString::new` reserving the NUL, which is worth spelling out: the first
  arithmetic on a corrupt length can hide the identity of the corruption, and
  the whole reason this decodes at all is that an inline `len` field *is*
  content.

  Routing the helper through `sso_string_parts_from_value` fixes all nine sites at once and
  is byte-identical IR at `KARAC_SSO=0` — the accessor's off-path is exactly the
  two extracts it replaced, with the same value names. Verified after the fix:
  all five lanes agree on `short-val` — `--interp`, JIT at `0` and `1`, AOT at
  `0` and `1`.

  ### The gap that is NOT routable: `cabi.rs::emit_string_return_area`

  Worth its own entry because **the obvious fix is the wrong one.** This is the
  wasm Component-Model export trampoline, and it *stores* the String's pointer
  into a module-level return area that the component lifter reads **after the
  trampoline has returned**. The accessor's contract is "valid for an immediate
  read only" — an inline descriptor's data pointer is the descriptor's own
  address — so routing it here would trade a garbage pointer for a **dangling**
  one, which is strictly harder to debug. The fix is `sso_deinline_in_place`:
  promote to a heap buffer before reading, restoring exactly the invariant the
  function's contract already assumed ("the string bytes already live in the
  guest's linear memory").

  So the FFI sweep has **two fix shapes**, and the question that picks between
  them is not "is this a String?" but **"does the pointer outlive the frame?"**
  Immediate read ⇒ route. Stored anywhere ⇒ promote.

  **MEASURED 2026-09-12, and it works** — see "THE INLINE OVERLAY IS BROKEN ON
  32-BIT TARGETS" below, and the section after it for what the fix turned out to
  be. A `wasm_browser` export returning a `substring` lifts the correct bytes at
  `KARAC_SSO=1`, so the promotion fires and does its job. (It did so even while
  the overlay was losing content bytes underneath it — the promotion was never
  the defect, which is why the two had to be separated before either could be
  read.)

  **The claim that this was unverifiable here was WRONG, and that is worth
  recording.** It was written as "a wasm component E2E needs `wasm-tools` and the
  two wasm runtime archives, and the container had neither." Neither was
  *present*; both were one command away. The two wasm rust targets ship in the
  pinned toolchain (`rust-toolchain.toml` declares them), the archives are two
  `cargo rustc … --crate-type staticlib` invocations, `wasm-tools` installs from
  crates.io, and node is already on PATH. The only genuine obstacle was a
  toolchain mismatch — the wasm targets live under the pinned `1.94.1` while
  `karac` resolves its sysroot from the active default — fixed with one
  `rustup target add … --toolchain stable`. An unchecked assumption about the
  environment had been standing in for a finding, and actually running it found
  a real defect the reasoning would have preserved indefinitely.


  ### THE INLINE OVERLAY IS BROKEN ON 32-BIT TARGETS (found 2026-09-12, FIXED
  — but not by the fix this entry prescribes; see the section after it)

  Found by finally running the thing this doc had recorded as unverifiable. The
  `cabi.rs` return-area promotion is **fine** — the corruption is upstream of it,
  in the reference encoder itself.

  **Measured.** A `wasm_browser` export returning `s.substring(0, n)`, run under
  node, sweeping `n` across the inline boundary:

  ```
  KARAC_SSO=0   ALL OK (0..30, incl. the 23-byte boundary)
  KARAC_SSO=1   n=4  "abcd"          ok
                n=5  "abcd\0"        want "abcde"
                n=8  "abcd\0\0\0\0"  want "abcdefgh"
                n=12 "abcd\0\0\0\0ijkl" want "abcdefghijkl"
                n=24,30               ok  (over capacity -> heap)
  ```

  **Content bytes 4..=7 are always zero.** Everything else survives.

  **Mechanism.** `RuntimeKaracString::new_inline` packs the first eight content
  bytes into a `u64` and then stores them through the pointer field:

  ```rust
  let data = u64::from_le_bytes(raw[0..8].try_into().unwrap());
  RuntimeKaracString { data: data as *mut u8, len, cap }
  ```

  On a 64-bit target `data` is eight bytes and the overlay is exact. On
  **wasm32 the pointer is four bytes**, so `data as *mut u8` truncates — content
  bytes 4..=7 are discarded — and in memory offsets 4..=7 are the alignment hole
  before the `i64` at offset 8, which nothing ever writes. Hence zeros, exactly
  where measured. The same hole means the 23-byte `INLINE_CAPACITY` is wrong on
  32-bit too: only 19 bytes are reachable through the struct fields.

  **Why the tests could not catch it.** `runtime/src/sso.rs`'s contract is
  described in this doc as "exhaustively unit-tested — all three states, every
  inline length 0..=23, boundary rejection, layout-pin", and it is. Those tests
  run on the HOST, x86-64, where `data` is eight bytes and the hole does not
  exist. A layout contract that is pointer-width dependent cannot be validated at
  one pointer width, and nothing in the suite runs them at another.

  **Nor could the existing wasm E2E.** `wasm_browser_rich_exports_marshal_e2e`
  exports `shout(s: String) -> String { s + "!" }` and calls it with `'hi'`.
  Concat is not an inline construction site, so the result is a heap String and
  the test passes identically with the bug present — it was green at
  `KARAC_SSO=1` throughout.

  **And a near-miss worth keeping.** The first probe returned `pick('abc…', 3)`
  and got `"abc"`, which was written down as verification. It is not: `n=3` fits
  entirely in bytes 0..=3, the bytes that survive truncation, so it passes with
  the bug present. **A single passing value on a boundary-shaped defect proves
  nothing** — the sweep is what made it visible, and the sweep should have been
  the first thing run.

  **Status when this entry was written: OPEN.** Reachable only at `KARAC_SSO=1`
  (default off), so nothing shipped broken. The shape of the fix looked clear —
  write the
  inline bytes through a raw byte pointer into the destination (`out as *mut u8`)
  rather than reconstructing struct fields, so the padding hole is written
  directly — but it touches the single source of truth that codegen mirrors, it
  changes `INLINE_CAPACITY`'s meaning on 32-bit, and the `karac_string_clone`
  24-byte struct-copy path needs the same audit (Rust does not guarantee padding
  is copied). That is a slice, not a patch, and this session had already used its
  budget for aiming a fix without full understanding.

  **The gate to add with the fix:** the sweep above, as an E2E. It needs the two
  wasm staticlib archives and node — all present in an ordinary container, which
  is the other thing this entry corrects.

  ### …AND THE FIX IS NOT THE ENCODER — THE OVERLAY IS 64-BIT-ONLY (2026-09-12)

  The entry above names the encoder as the mechanism and prescribes writing the
  inline bytes through a raw byte pointer. That fix was written, and **it does
  not fix the bug.** Applied alone, with the wasm archive rebuilt, the sweep
  fails in exactly the shape it failed before: content bytes 4..=7 zero at every
  length from 5 through 23. The encoder was a real defect and it was not the
  whole one.

  **The rest of it is visible in one line of IR.** `examples/dump_ir` on a
  `substring` program at `KARAC_SSO=1`:

  ```llvm
  ss.inline:
    %ss.inl.ok = call i8 @karac_string_try_inline_into(ptr %ss.inl.src, i64 %ss.inl.len, ptr %ss.result)
    br i1 %ss.inl.done, label %ss.cont, label %ss.copy
  ss.cont:
    %ss.load = load { ptr, i64, i64 }, ptr %ss.result, align 8
    ret { ptr, i64, i64 } %ss.load
  ```

  The runtime writes 24 bytes into `%ss.result`. Codegen then reads them back
  with `load { ptr, i64, i64 }` — which loads **three fields, not 24 bytes** —
  and returns the aggregate. On wasm32 bytes 4..=7 belong to no field, so they
  are dropped one instruction after being written, and every later store of that
  value writes three fields again. Writing the hole was never the hard part;
  *keeping* it is, and a `String` moves through the compiler as an aggregate
  everywhere.

  So the honest reading is that **the 24-byte overlay presupposes a gapless
  descriptor**, and `{ptr, i64, i64}` is gapless only at a pointer width of 8.
  Supporting 32-bit would mean lowering every descriptor move in the compiler to
  a 24-byte `memcpy` — on the value type that moves most, to pessimise the
  64-bit path SSO exists to speed up. Not a trade worth making for a target
  where the malloc SSO removes is not the bottleneck anyway.

  **What landed instead: inline construction is refused where the descriptor has
  a hole**, on both sides independently.

  * `RuntimeKaracString::DESCRIPTOR_IS_GAPLESS` (`runtime/src/sso.rs`) states the
    precondition as what it actually is — `size_of::<Self>() == size_of::<*mut
    u8>() + 16` — rather than as a target name. Both runtime construction
    entrypoints check it: `karac_string_try_inline_into` answers 0, which is the
    verdict its callers already handle, and `karac_string_slice_into` falls
    through to its heap arm.
  * `Codegen::sso_on()` is additionally `&& !active_target_is_wasm()`, so the
    inline blocks are not emitted on wasm at all.

  Where it is false, no inline descriptor is ever built, no reader can meet one,
  and `String` behaves exactly as it did pre-SSO. The redundancy is not
  decorative and was **measured**: with the codegen gate alone backed out, the
  emitted wasm still contains the `karac_string_try_inline_into` call and the
  sweep is still clean, because the runtime refuses and the caller takes its heap
  path.

  The encoder fix stayed in regardless — `write_inline` writes the descriptor
  through `out as *mut u8`, and `karac_string_clone`'s inline arm now does a raw
  24-byte copy instead of three field assignments. Neither changes a byte on
  64-bit; both remove a way for the layout to be wrong that nothing else was
  checking.

  **The gate:** `wasm_browser_inline_string_survives_export_at_every_length`
  (`tests/cli.rs`) builds the `substring` export at `KARAC_SSO=1` and sweeps
  `n = 0..=30` under node against the host's own `slice`. It is red with the
  gates backed out and green with them in.

  #### The negative control needed its own negative control

  Backing the two gates out of the SOURCE and re-running the test reported
  **pass** — which briefly read as "the new test is vacuous", the campaign's
  recurring failure shape and a plausible verdict given it had just been written.
  It was not. `cargo test` rebuilds `karac` and the runtime *rlib*; it does not
  rebuild `libkarac_runtime_wasm.a`, which is what a wasm build links. The
  archive still carried the gate, so the backout had not reached the binary under
  test. Rebuilding the archive made the same test fail immediately.

  This is CLAUDE.md's archive-staleness trap, met inside a backout experiment
  rather than a measurement — and it is the more dangerous placement, because a
  stale archive there does not merely mislead about a number, it certifies a real
  gate as useless. **A backout that changes runtime source is not in effect until
  the archive is rebuilt**, and the tell is the same one the campaign keeps
  relearning: a result that does not move when the input demonstrably did.

  One further check the first sabotage attempt failed to be: to prove the test
  body ran at all, the JS oracle was corrupted — but the corruption changed the
  shared `src` string feeding *both* the call and the expectation, so the two
  moved together and the test passed legitimately. Corrupting only the
  expectation (`want = src.slice(0, n) + 'X'`) made it fail in 0.62 s, proving
  the body builds, runs node, and compares.

  ### The remaining raw-site count is NOT a remaining-risk count

  The correction to the previous round's framing, and the transferable half of
  this entry. A balanced-paren scan still reports 92 `extract_value(_, 0)`, 72
  `extract_value(_, 1)` and 245 `build_struct_gep(vec_ty, _, 0)` sites raw,
  which reads as a large outstanding surface. Re-reading them says otherwise:
  `expr_ops.rs`'s field-0 sites are enum tags and overflow-check pairs,
  `synth_display.rs`'s are enum tags and float wrappers, `closures.rs` and
  `par_blocks.rs`'s are closure fat pointers, `tls.rs`'s others are
  `{fd, config}` listener handles, and `method_call_ffi.rs`'s are `CStr` /
  `CString` `{ptr, len}` receivers — a different two-field struct that never
  carries the tag. They share the `{ptr,len,cap}` *syntax*, not its semantics.

  Risk concentrates where a syntax scan does not look: in the **helpers that
  return a `(data_ptr, len)` pair**. Enumerating those found the straggler in
  one grep, where two rounds of site-by-site triage had walked past it.

  **The census that works is on the SIGNATURE, not the name and not the call
  syntax** — `fn … -> (PointerValue<'ctx>, IntValue<'ctx>)`. It depends on no
  naming discipline, and it is a small closed set. Run over `src/codegen`, it
  returns thirteen, of which six touch a String:

  | helper | file | state |
  |---|---|---|
  | `sso_string_parts_from_value` | `sso.rs` | the accessor itself |
  | `str_data_len` | `method_call.rs` | routed |
  | `load_string_data_len` | `vec_method.rs` | routed (`to_uppercase`, `join`, `replace`, `strip_*`, …) |
  | `regex_pattern_data_len` | `method_call.rs` | routed, both the flattened and nested `Regex` shapes |
  | `df_string_parts` | `dataframe.rs` | routed (returns a `Result`, so the signature scan misses it — see below) |
  | **`extract_string_ptr_len`** | **`tls.rs`** | **was raw — 9 callers** |

  The straggler survived because of *where it lives*: nobody auditing String
  behaviour greps `tls.rs`, and its `env.set` / `env.var` callers sit in
  `method_call_ffi.rs`, three files from anything named String. A name-keyed
  grep does find it — but it also misses `load_string_data_len`'s siblings and
  depends on whoever named them, whereas the signature is structural.

  And note the signature scan's own blind spot, recorded so the next person
  does not trust it further than it goes: `df_string_parts` returns
  `Result<(PointerValue, IntValue), String>` and does not match. That is the
  third scanner gap this campaign has hit. **Every mechanical census here has
  been a lower bound**; the value of this one is that it is a *different* lower
  bound from the `extract_value` scan, and the two together left one site
  standing rather than none.

  A useful negative from the same audit: of the three sites the previous round's
  handoff listed as suspected FFI gaps, one — `method_call_ffi.rs:129` — is a
  **false positive** (it is `CString.as_bytes`, not a String), one is the
  measured `tls.rs` gap, and one is the `cabi.rs` site needing the other fix
  shape entirely. One of three was actionable as listed. A suspected-site list
  is a starting point for reading, never a work list to apply.

  ### The first `KARAC_SSO=1` fixture in the tree

  `tests/cli.rs::test_sso_inline_string_survives_the_env_ffi_boundary`. Until
  now **no compiled Kāra program in CI ever emitted an inline descriptor** (the
  runtime unit tests build them via `new_inline`, which is why the *clone*
  half was testable ahead of construction — but nothing drove codegen's
  construction path) — so the entire SSO surface was covered by a two-leg gate
  cycle a human had to remember to run
  with the variable set, which is how both this bug and the `push_str` one
  reached `main`. It has to shell out: `KARAC_SSO` is read once per process
  through a `OnceLock` at codegen time and `tests/codegen.rs` compiles
  in-process, so a fixture there cannot set it per-test and the first reader
  would win regardless. Same constraint, and the same placement, as
  `KARAC_OPT_LEVEL` in
  `test_rc_promoted_param_move_suppression_is_sound_at_o0_and_on_the_jit`.

  It also pins the thing that made the *existing* `env.set` / `env.var` E2E
  fixtures useless here: **a string literal is static (`cap == 0`) and never
  inline**, so `env.set("NAME", "val")` cannot exercise the tag in either
  direction — which is why those fixtures stayed green through the whole
  regression. This one builds its name and value with `substring`, a real
  construction site. Same lesson as the receiver-form finding one round earlier:
  a test that does not reach the code path certifies nothing.

  Two details keep it from going quietly vacuous. Its AOT half skips when the
  runtime archive is absent, like every other build-and-run fixture here — so
  under `KARAC_REQUIRE_RUNTIME_ARCHIVE=1` (which CI's archive-building jobs set)
  it asserts that **both** AOT legs actually built and ran, rather than
  reporting green on two skips. And it was verified to FAIL with the fix backed
  out, so it discriminates.

  ### MOVED-FROM STRINGS READ WILD, NOT STALE — was a flip blocker, FIXED 2026-09-12

  Found while bisecting a probe divergence, and it is the most consequential
  thing in this entry — because it is not a footgun the user was warned about,
  it is a **documented guarantee that SSO breaks**. `UseAfterMove` is advisory
  by deliberate design, and `src/cli.rs` states the reason in as many words:

  > `UseAfterMove` — codegen **defensive-copies the reuse, so the binary is
  > memory-safe**; the diagnostic carries a machine-applicable `.clone()` fix
  > precisely because the program compiles and runs. Keeping it non-fatal for
  > `build` is deliberate.

  So `karac check` prints `warning[ownership]` and then `All checks passed.`,
  and the compiler promises the resulting binary is memory-safe. This compiles,
  runs, and is covered by that promise:

  ```
  let a = base.substring(0, 10);
  m.insert(a, 7);          // takes ownership
  println(a);              // warning[ownership], but permitted
  ```

  **What was measured** (and this is the part to trust):

  | program | `--interp` | `KARAC_SSO=0` | `KARAC_SSO=1` |
  |---|---|---|---|
  | one 10-byte source, `Map.insert` then read | `abcdefghij` | `abcdefghij` | *empty*, exit 0 |
  | 10-byte **and** 40-byte sources, both moved then read | both correct | both correct | **SIGSEGV, exit 139** |

  The 40-byte string is the control: over the 23-byte inline capacity, so it
  stays heap on both legs and reads back fine. Its presence is what turns the
  failure from a wrong answer into a crash — which is both why it is in the
  reproducer and why the inline path, not `Map`, is the thing implicated.

  **The mechanism is a HYPOTHESIS, and it does not yet fit both rows.** Move
  suppression disarms a moved-from source by zeroing `cap` — see
  `zero_struct_move_caps`'s contract, "each Vec/String field's `cap` is
  zeroed", with the `_mono` sibling saying `cap`/`len`. For a heap String that
  leaves `{ptr intact, len intact, cap = 0}`, which is exactly the
  static-literal state, so the moved-from read is benign and returns the old
  contents — consistent with both non-SSO columns. For an **inline** String
  `cap` is not spare: it carries the inline flag AND the length, so zeroing it
  should leave `{ptr = content bytes 0..=7, len = content bytes 8..=15}`, a low
  bogus pointer with a large length. That predicts a crash in row 1, and row 1
  prints *empty* instead — which fits `len` being zeroed too, and then does not
  explain row 2's crash.

  So two of the three facts are explained and one is not. **Trace the actual
  disarm site for a bare local String before writing the fix** — those cited
  helpers walk struct FIELDS, and whichever site handles a plain `let` binding
  was not read. A fix aimed at the wrong site would pass this reproducer for
  the wrong reason, which is the failure mode this campaign has already hit
  once (the `is_empty` fix that landed in `vec_method.rs` when the defect was
  in `method_call.rs`).

  So SSO does not merely change what a moved-from read returns. It converts
  *reads stale data* into *reads wild memory* on a program the compiler
  explicitly undertakes to keep memory-safe. Four things follow:

  - **Memory safety of programs with no ownership diagnostic is not affected**,
    and that is worth stating precisely so nobody over-reads this. An inline
    descriptor owns no buffer, and `cap = 0` makes its drop a no-op exactly as
    before — no leak, no double free. The defect is confined to reading a
    moved-from value.
  - **It is a hard flip blocker, and the reason is a contract rather than
    taste.** The `UseAfterMove` guarantee is not "we warned you"; it is "the
    binary is memory-safe". Flipping SSO on by default would falsify that
    sentence for every short String in the corpus, and the sentence would still
    be sitting in `cli.rs` saying otherwise. Either the disarm becomes
    tag-aware or that documented guarantee has to be withdrawn first — and
    withdrawing it means making `UseAfterMove` fatal, which is a language
    decision, not a codegen one.
  - **Not filed as a ledger row, deliberately.** It is reachable only at
    `KARAC_SSO=1`, an experimental gate that is off by default and has never
    shipped on, and this doc is the campaign's canonical tracker — the same
    reason the `push_str` and free-gate defects were recorded here rather than
    in `bug-ledger.jsonl`. It becomes a row the moment a flip is attempted.
  - **The likely fix, once the site is traced.** Disarm an inline source by
    writing the canonical empty descriptor `{null, 0, 0}` rather than clearing
    `cap` (and `len`) in place — a defined `""` for the moved-from read, which
    is also the honest answer, since the value really did move away. Offered as
    a direction, not a patch: the disarm is a family of sites
    (`zero_struct_move_caps`, `zero_struct_move_caps_mono`,
    `zero_enum_payload_caps`, plus the bare-local site above), it lives in the
    move machinery rather than the String subsystem, and the unexplained row 2
    may mean something else is wrong as well. It is not done here deliberately
    — sizing it honestly beats bolting a guess onto an FFI commit.


  ### RESOLVED 2026-09-12 — one bug with two faces, and the fix is tag-aware

  The entry above recorded the mechanism as a HYPOTHESIS that "does not yet fit
  both rows": cap-only zeroing predicts a crash, and the one-string case printed
  *empty* instead. The IR settles it, and the hypothesis was right — what was
  wrong was expecting one failure mode.

  `dump_ir` on the reproducer shows the disarm and the read exactly:

  ```llvm
  %short16     = load { ptr, i64, i64 }, ptr %short   ; the map gets the PRE-disarm value
  %move.cap.p  = getelementptr … ptr %short, i32 0, i32 2
  store i64 0, ptr %move.cap.p                        ; disarm: cap only
  …
  %str.cap      = 0
  %sso.inline18 = icmp slt i64 %str.cap, 0            ; FALSE — no longer looks inline
  %str.data_ptr = select i1 false, …, %str.ptr        ; content bytes 0..=7 AS A POINTER
  %str.byte_len = select i1 false, …, %str.len        ; content bytes 8..=15  = 27,241
  call void @__karac_write_console_line(ptr %str.data_ptr, i64 %str.byte_len, …)
  ```

  So the moved-from read really does hand a 27 KB length and a pointer made of
  the string's own text to the console writer. **What happens next is decided by
  the CONSUMER, not by the descriptor** — which is why it looked like two
  different defects:

  | consumer | what it does with the bogus pointer | result |
  |---|---|---|
  | `println(s)` | passes it straight to `write(2)` | kernel rejects the buffer with **EFAULT**, runtime ignores the short write → **prints nothing, exits 0** |
  | `println(f"[{s}]")` | **memcpy**s it in user space to build the interpolation | **SIGSEGV** |

  Proven by changing only the consumer on one otherwise identical program:
  `println(short)` exits 0 silently, `println(f"[{short}]")` exits 139. Optimization
  level is not involved — both behave identically at `-O0` and `-O2`.

  **The silent form is the common one and the worse one.** Any String corruption
  that reaches `println` directly looks like an empty line rather than a crash;
  it takes a user-space consumer to make it loud. Worth remembering well beyond
  this bug — it is a general silent-failure channel in this runtime.

  ### The fix

  `call_dispatch.rs`'s move-out disarm now reads the tag BEFORE clearing it and
  blanks the whole descriptor on the inline path only:

  - **heap / static source — unchanged.** The selects yield the old `ptr`/`len`,
    so `cap = 0` still leaves the static-literal state and a moved-from read
    still returns stale-but-valid bytes. That is what `cli.rs`'s advisory
    `UseAfterMove` undertakes, and it is preserved exactly.
  - **inline source — blanked to `{null, 0, 0}`**, so the moved-from read yields
    a defined `""`, which is also the honest answer: the value did move away.

  Branch-free (three loads, two selects, three stores), and compiled out entirely
  with SSO off.

  Measured, same programs:

  | | before | after |
  |---|---|---|
  | `println(f"[{s}]")` at `KARAC_SSO=1` | **SIGSEGV**, exit 139 | exit 0, prints `[]` |
  | two-source reproducer at `KARAC_SSO=1` | **SIGSEGV**, exit 139 | exit 0, `short-after-move=[]` |
  | the 40-byte HEAP source in the same program | stale contents | **stale contents — unchanged** |
  | every `KARAC_SSO=0` row | — | **byte-identical** |

  The heap row is the control that matters: it shows the change is confined to
  the inline path and did not quietly alter the documented behaviour for
  everything else.

  The reproducer is the snippet above, doubled — the two rows of the table.
  It lives here rather than in `examples/` on purpose: it segfaults on one leg,
  so it belongs in no corpus that gets run, and it carries an ownership
  diagnostic that every corpus fixture is supposed to be free of.

  ### RUN `karac check` ON THE PROBE BEFORE BELIEVING A DIVERGENCE

  The operational upgrade to last round's "a probe result is evidence about the
  probe until the probe is proven to reach the code it targets." That rule was
  already written down here, and this round produced the **same class of false
  alarm anyway**: a combined probe segfaulted at `KARAC_SSO=1` and ran clean at
  `=0`, which reads exactly like a fresh codegen bug. Every one of its eleven
  surfaces passed in isolation; the divergence came from a binding reused after
  `Map.insert` had taken ownership — the same ownership-reuse mistake the
  previous round recorded, in a new costume.

  What makes this actionable rather than just another warning is that **the
  compiler had already said so**: `karac check` on the probe prints
  `warning[ownership]: value 'a' moved here, used again here`. The check was
  free and was not run. So the pre-flight is now concrete —

  > `karac check <probe>.kara` must be clean of `warning[ownership]` before any
  > `KARAC_SSO=0` vs `=1` divergence is treated as a compiler finding.

  — and the same command is what distinguishes the two outcomes: the clean
  version of that probe (`ffiprobe.kara`, every ownership-taking use given its
  own fresh slice) is byte-identical at both settings across all eleven
  surfaces, which is a real *negative* result for `println`, f-string
  interpolation, `to_uppercase` / `to_lowercase` / `trim`, `len` /
  `char_count`, `contains` / `starts_with`, `i64.parse`, `Map` insert+get,
  `Vec[String]` display, and `+` concatenation.

  ### Still open on this boundary

  - **The borrowed-view lifetime — AUDITED 2026-09-12, safe by enumeration.**
    `karac_string_slice_borrow` returns a pointer *into* its source, and when
    the source is inline that source is the accessor's entry-block spill slot,
    so the view is only valid until the next accessor call reuses that slot.
    The consumer set is small and closed, and every member is an immediate
    read:

    | consumer | what it does with the view |
    |---|---|
    | `calls.rs:172` | B-2026-08-18-22's scalar-reader receiver arm (`len`, `contains`, `starts_with`, …) — reads, returns a scalar |
    | `maps.rs` ×5 | `get` / `contains_key` / `remove` / entry lookups — hashes and compares the bytes |
    | `vec_method.rs:5340` | `push_str`'s ARGUMENT — memcpy source |

    So it is correct today, and correct by enumeration rather than by
    construction: nothing stops a future consumer from storing the view. The
    check when adding one is "does this read the bytes before the next
    `sso_string_parts_from_value` call in the same function?" — and the same
    scan that found the enum-payload store (accessor result reaching a
    `build_store` / `store_enum_word` / `build_insert_value`) is the mechanical
    version of that question.
  - **The wasm return area** fix is reasoned, not measured (above).
  - **Unprobed surfaces**, each of which takes a `(ptr, len)` pair from a String
    and is one `substring`-built argument away from being probed exactly as
    `env.set` was: `serve_https` / `serve_ws_tls`, the HTTP *client* builder
    path, `json`, `interner`, `Regex`, `String.normalize`.

  Gates at this commit: fmt OK; clippy GREEN on both legs; `--features llvm`,
  109 binaries per leg. **`KARAC_SSO=0`: 16,805 passed, ZERO red.**
  `KARAC_SSO=1`: 16,770 passed, one red — `coro_e2e`'s
  `coroutine_ws_over_tls_concurrent_handlers_all_execute`, which is
  B-2026-09-12-1 and not this work (the SSO-off leg of the same tree is clean,
  and that row's two prior reds are on opposite settings of the gate). That red
  was worth more than the green: the row had asked for the assertion's
  `left:`/`right:` counts to be preserved on the next occurrence, and this one
  came back **15/16 — one handler wedged, the server did come up**, which
  settles the question the row was written to separate. The row is updated with
  it and stays open.

- **SLICE 2'S OWN GATE FINALLY RAN, AND THE MOTIVATING WORKLOAD LOSES
  (2026-09-12).** Slice 2's exit criterion was "re-profile the self-host lexer
  (instruction count + `malloc` leaf share must drop)". It had never been run.
  Every payoff number in this doc up to here is synthetic. Run now, on the
  workload the whole campaign was scoped around, SSO is a **4% REGRESSION**.

  **Setup**, following [`selfhost-lexer-profile.md`](selfhost-lexer-profile.md)'s
  method: a snapshot of `selfhost/src/{span,token,lexer}.kara` (never the live
  tree) plus a lex-in-a-loop driver, 441 KiB of real Kāra (the compiler's own
  sources + `examples/`), 200 passes, built sequential (`KARAC_AUTO_PAR=0`).
  Token output is **identical on both legs** (10,639,400) and the binaries
  differ, so the gate is real and correctness holds. Linux x86-64 container —
  **do not compare these absolute numbers against that doc's macOS/M5 figures**;
  only the two legs here are comparable to each other.

  | 441 KiB × 200 passes | `KARAC_SSO=0` | `KARAC_SSO=1` | |
  |---|---|---|---|
  | wall, best-of-7 | **1450 ms** | 1508 ms | **−4.0%** |
  | instructions retired (callgrind) | 9,190,806,670 | 9,507,462,371 | **−3.4%** |
  | heap allocations (valgrind) | 16,292,811 | 11,232,411 | **−31%** |
  | bytes allocated | 2,231,969,346 | 2,210,121,746 | −1.0% |

  Wall and instruction deltas agree in sign and size, so this is **extra work
  executed**, not a cache or branch-prediction artifact. The two allocation rows
  are the campaign's problem in one line: SSO removes **31% of allocation CALLS
  but only 1% of allocated BYTES**, because the strings it captures average
  **4.4 bytes**. Those are exactly the allocations glibc's tcache already serves
  almost free.

  ### Where the instructions went

  | bucket | delta | note |
  |---|---|---|
  | libc allocator (`malloc`/`free`/`_int_*`/consolidate) | **−629,573,607** | the win — 124 instructions per removed allocation |
  | `karac_free_buf` | −85,990,200 | fewer buffers to release |
  | `karac_string_try_inline_into` | +192,436,000 | 38 instructions/call over 5,060,400 calls |
  | Kāra code (`lex_all` + its static locals) | **+929,630,600** | the tag-aware read path |
  | `__memcmp_avx2_movbe` | +2,863,800 | **absent entirely at `KARAC_SSO=0`** |
  | net | **+316,655,701** | |

  The new runtime entrypoint **pays for itself comfortably** — 38 instructions
  to avoid a 124-instruction malloc/free pair. The campaign is not losing on its
  construction site. It is losing on the **read** side: +930 M instructions of
  tag-select spread through the generated Kāra code, against −716 M of allocator
  and free-path work removed.

  ### THE `bcmp` STORY IS TRUE AND IRRELEVANT — a correction to this doc

  Slice 2 round 2 decomposed a 9 ms synthetic regression as "**~5 ms is lost
  compare folding**: the tag-select makes the data pointer opaque, so an inlined
  4-instruction compare becomes `call bcmp@plt`", and filed **branch-not-select**
  as lever #1, "worth ~5 ms of the 9 ms".

  On the real lexer that mechanism is **confirmed in kind and negligible in
  size**. `__memcmp_avx2_movbe` appears at `KARAC_SSO=1` and is *completely
  absent* at `KARAC_SSO=0` — exactly the predicted effect, and a clean natural
  experiment for it — at **2,863,800 instructions: 0.9% of the regression.**

  So branch-not-select is not the lever here. It was sized on a microbenchmark
  built to isolate it (a 3-byte literal compare in a tight loop), and that
  benchmark's proportions do not survive contact with a program whose String
  reads are diffuse. **The cost is not concentrated in one foldable compare; it
  is one select on every String read, everywhere.** A lever that fixes the
  compare sites recovers ~1% of this.

  ### THE LEXER IS NOT THE "RETAINED" SHAPE — a second correction

  This doc claimed "the self-hosted lexer **keeps** its token texts — it does not
  slice-and-discard", and used that to argue the campaign's motivating workload
  sits on the winning side of the split. **That was read off the source, not
  measured, and it is wrong in the half that matters.** The hot path is:

  ```
  let text = self.src.substring(self.start, self.current);
  let token = keyword_or_ident(text);      // takes the String BY VALUE
  ```

  `keyword_or_ident` is a `match text { "fn" => Token.Fn, … }` over **87 arms**.
  A keyword returns a payload-free variant and the String is **dropped** — the
  transient shape. An identifier returns `Token.Identifier(text)` — retained. In
  the 441 KiB input: 41,866 identifier-shaped lexemes per pass, **19.6%
  keywords**, mean length 4.4 bytes, and **100% under the 23-byte capacity**.

  The trap is that the two effects do not split along the same line. The
  allocation saving follows the 20/80 keyword/identifier split; the read-path
  cost falls on **all 41,866**, because every one of them goes through the
  `match` dispatch regardless of which way it resolves. Reading the source tells
  you the first split and hides the second.


  ### Reproducing it

  The harness is small enough to keep here rather than in the tree, and it must
  not be run against the live `selfhost/` worktree (that doc's rule, and another
  session edits it):

  ```bash
  mkdir -p lexprof/src && cd lexprof
  printf '[package]\nname = "lexprof"\nversion = "0.1.0"\nauthors = []\nedition = "2026"\n\n[dependencies]\n' > kara.toml
  cp <kara>/selfhost/src/{span,token,lexer}.kara src/
  cat <kara>/selfhost/src/*.kara <kara>/examples/*.kara | head -c 451584 > input.kara
  # src/main.kara:
  #   import lexer.lex_all;
  #   fn main() with panics reads(FileSystem) {
  #       match fs.read_to_string("input.kara") {
  #           Ok(src) => {
  #               let mut total: i64 = 0;
  #               let mut i: i64 = 0;
  #               while i < 200 { let toks = lex_all(src.clone()); total = total + toks.len(); i = i + 1; }
  #               println(total);
  #           }
  #           Err(_) => { println("READ FAILED"); }
  #       }
  #   }
  for m in 0 1; do KARAC_SSO=$m KARAC_AUTO_PAR=0 karac build && mv lexprof lexprof_sso$m; done
  valgrind --tool=memcheck   ./lexprof_sso$m   # "total heap usage: N allocs"
  valgrind --tool=callgrind  ./lexprof_sso$m   # "I refs:"
  callgrind_annotate --threshold=97 callgrind.out.<pid>
  ```

  Both valgrind counters are deterministic, so they can be trusted from a single
  run and under load; only the wall-time row needs best-of-N.

  **The token total is NOT a correctness check, and treating it as one cost a
  session.** This harness sums `toks.len()`, so it proves the two binaries lexed
  the same NUMBER of tokens and says nothing about their CONTENT. A miscompile
  that drops String payload content keeps the count identical — and drops
  allocations, which reads as a spectacular win. That exact thing happened on
  2026-09-12: a −69%/−21.5% result was recorded from a compiler that was
  rendering `IDENT ab` as `IDENT x\u{fffd}`.

  Two rules, both cheap:

  1. **Gate every perf number on `KARAC_SSO=1 cargo test --features llvm --test
     selfhost_lexer` being GREEN for the exact `karac` that built the benchmark.**
     That differential compares against the Rust lexer token-for-token, which is
     the correctness oracle this harness does not have. Perf numbers from a
     compiler that fails it are meaningless.
  2. **`cmp` the benchmark binaries across any compiler change.** A change that
     does nothing and a change you did not compile look identical from the
     outside, and only one of them is worth reverting — see the near-miss under
     "A near-miss worth keeping".


  ### THE LOSS WAS ONE MISSED GATE — WORTH 14.8%, AND NOW LANDED

  **Everything above this heading is the measurement that found the bug — it is
  correct and worth reading, and its conclusion is superseded.** The profile did
  its job: by attributing the regression to a single function it exposed an
  unswept `cap > 0` gate, and flipping it turns SSO from a 2.6% loss into a
  **14.8% win** on the campaign's motivating workload.

  **The flip is ON `main` as of `86c9bb3` — this paragraph recorded the state
  mid-slice and is kept for the sequence.** When written it read "the flip is NOT
  on main": it had reddened eight binaries — all seven selfhost differentials
  plus the coroutine flake — because the unsigned gate was masking latent
  inline-descriptor bugs elsewhere. Both were found and fixed (the enum-payload
  verbatim store, `e4ed7ed`; the inverted `vec_elem_types` guard, `8eaa11a`),
  including the SIGSEGV in the self-hosted item parser that this paragraph left
  open. All eight selfhost differentials pass at `KARAC_SSO=1` in the current
  gate set, `selfhost_parser_matches_rust_parser_items` among them; the only red
  left on that leg is the pre-existing coroutine flake, B-2026-09-12-1.

  Same harness, same 441 KiB input, same 200 passes, all three verified against
  a `karac` whose selfhost differential passes:

  | | heap allocations | instructions retired | wall, best-of-7 |
  |---|---|---|---|
  | `KARAC_SSO=0` | 16,292,811 | 9,190,806,670 | 1484 ms |
  | `KARAC_SSO=1`, before | 11,232,411 (−31%) | 9,507,462,371 (**+3.4%**) | 1524 ms (**−2.6%**) |
  | `KARAC_SSO=1`, after | **5,128,411 (−69%)** | **7,173,722,614 (−21.9%)** | **1265 ms (14.8% faster)** |

  ### How the profile found it

  DWARF-attributed callgrind named one function: **`Lexer.make_spanned`, 348.6 M
  → 776.6 M instructions**, 46% of the entire Kāra-side regression. Disassembly
  showed why — **187 → 591 instructions, and its `malloc`/`memcpy` call sites
  went 5 → 13.** SSO was *adding* allocation sites to the hottest per-token
  function, which is the opposite of the whole design.

  `make_spanned(token: Token)` takes an owned aggregate by value, so it hits
  `emit_vecstr_defensive_copy` — the deep copy that gives a retaining callee its
  own buffer. Its ownership gate was:

  ```rust
  inkwell::IntPredicate::UGT, cap, i64_t.const_int(0, false), "dcopy.owned",
  ```

  An inline `cap` is negative, so read **unsigned** it is enormous and the gate is
  always true: every inline String was deep-copied onto the heap, re-spending
  exactly the `malloc` construction had just avoided. Construction removed the
  allocation and this handed it straight back, one function later.

  `SGT` sends an inline source down the pass-through arm, where the phi already
  returns the header verbatim. That is correct for the same reason the `cap == 0`
  literal case is — an inline String owns no buffer, so there is no alias to
  defend against, and the descriptor re-derives its data pointer from whatever
  address it lands at. `Vec` never sets the flag, so `SGT` ≡ `UGT` there.

  ### Why it survived every previous sweep

  **The predicate and its `cap` operand are on different lines.** Slice 2's sweep
  found 16 unsigned gates and flipped 14 with line-oriented greps; this one is
  formatted across five lines by `rustfmt`, so no `grep 'UGT.*cap'` could ever
  have matched it. A multi-line-aware census — balance the parens of each
  `build_int_compare(…)` call, then test its operands — finds it immediately, and
  reports it as **the last one in the tree**.

  That is the fourth scanner blind spot this campaign has hit, and the pattern is
  now unmistakable: *every* mechanical census here has been a lower bound, and
  each gap was a formatting accident rather than a reasoning error — a closing
  paren on the wrong line, an aggregate expression instead of an identifier, a
  `Result`-wrapped return type, and now a multi-line argument list. **Write the
  census against the syntax tree, not against lines.**

  Note also what this was NOT. Unlike the 14 gates Slice 2 flipped, this was never
  a soundness bug: the copy path handles an inline source correctly, because
  `sso_string_parts_from_value` hands it the right bytes. It was a pure
  performance bug — and a total one on that path, which is why a predicate nobody
  could see was worth 2.3 billion instructions.


  ### THE FLIP EXPOSED LATENT BUGS — the first is the doc's own rule, broken

  The gate flip alone is **not** the fix. Landing it by itself reddened eight
  binaries: all seven selfhost differentials plus the known coroutine flake, with
  the lexer rendering `IDENT ab` as `IDENT x\u{fffd}` — right length, wrong bytes.

  **This is the entry's real finding.** The unsigned gate de-inlined every String
  at every by-value consuming call, so inline descriptors never reached most of
  the compiler. It was load-bearing by accident — not for correctness of its own
  path, but as a *filter* keeping a whole representation out of code that had
  never been audited for it. Flipping it is therefore not a one-line perf fix; it
  is a de-masking exercise whose size is unknown until it is run. **It ran, and
  it converged in two defects** — see "THE DE-MASKING RAN" below for both and
  for the gate results. The second announced itself as:

  > `selfhost_parser_items` — **SIGSEGV** in the item-parser binary at
  > `KARAC_SSO=1` with the gate flipped, after the payload-store fix below made
  > the lexer, parser and codegen differentials pass.

  and turned out to be the mutation chokepoint's inverted Vec guard.

  The cause sits one layer up, in the enum-payload arm of the by-value param deep
  copy (`param_own.rs`). It reconstructs a `{ptr,len,cap}` from the enum's payload
  words, runs the defensive copy, and then writes the result back — and it wrote
  it back through **`sso_string_parts_from_value`**:

  ```rust
  let (cd, cl) = self.sso_string_parts_from_value(copied, "p14e.c");   // WRONG here
  let cc = build_extract_value(copied, 2, ...);                        // raw cap
  store_enum_word(data_idx, ptr_to_int(cd)); store_enum_word(len_idx, cl); …
  ```

  That is a **store**, not a read. For an inline `copied` the accessor returns the
  address of an entry-block SPILL SLOT, so the payload ends up holding a `cap`
  that still carries the inline tag next to a `ptr` into a frame that dies. A
  reader then trusts `cap`, recomputes the data pointer from the payload's own
  address, and reads whatever is there.

  **This is exactly the rule this doc has carried since the accessor landed** —
  *"the returned pointer is valid only for an immediate read; storing it into
  anything outliving the frame dangles"* — violated in the tree, by the read
  sweep, at a site that looks like a ptr+len read and is not one. It was latent
  only because the unsigned gate below it de-inlined every source first, so
  `copied` was always heap and the accessor was a no-op. The moment the gate went
  signed, the latent bug became the live one.

  The fix is to round-trip the three words verbatim — a descriptor is complete in
  every state, and the payload should hold it as-is.

  **A validated scan says this was the only such site.** The check is: for each
  accessor call, do its result names reach a `build_store` / `store_enum_word` /
  `build_ptr_to_int` within the next 25 lines? Run against `HEAD` it finds
  `param_own.rs:3728`; run against the fixed tree it finds nothing. **Validating
  the scanner against the known instance before trusting its zero is the step
  that makes a null result mean anything** — four scanner blind spots in this
  campaign say an unvalidated census is a guess.

  ### The first measurement of the fix was taken on the CORRUPT build

  Recorded because the reasoning that caught it was nearly right and the
  conclusion nearly wrong. The −69% allocations were first measured from the
  gate-flip-only compiler — the one corrupting payloads — which is grounds for
  suspecting the win was an artifact: a miscompile that drops String content
  would also drop allocations.

  Re-measured after the payload fix: allocations **identical** (5,128,411) and
  instructions slightly **better** (7,211,445,814 → 7,173,722,614). The suspicion
  was wrong — the corruption changed which bytes were stored, not how many
  allocations happened — but it was the right suspicion to have, and checking it
  cost one re-run. **A perf number from a compiler that fails its differential is
  not evidence, even when it turns out to be right.**

  Gates on the landed state (the payload fix, WITHOUT the gate flip): fmt OK;
  clippy GREEN on both legs; `--features llvm` 109 binaries per leg, **both
  `KARAC_SSO=0` and `=1` at 16,809 passed with ZERO red binaries** — which also
  confirms the payload fix is the no-op it should be while the gate stays
  unsigned.


  ### THE DE-MASKING RAN, AND IT WAS TWO FIXES — not an open-ended list

  The flip's first attempt reddened eight binaries and the honest report was
  "unknown number of defects behind this." Run properly — flip the gate, run the
  selfhost differentials at `KARAC_SSO=1`, fix what breaks, repeat — it converged
  in **two** rounds. All seven differentials green — and the full cycle with all
  three changes in reports **`SSO=0` 16,820 passed / zero red** and **`SSO=1`
  16,785 passed** with only B-2026-09-12-1's coroutine flake, plus the ASAN
  `-O0` ratchet leg green at BOTH SSO settings (1593 passed, quarantine list
  matched exactly). That last leg is the one that matters here: the flip moves
  values from a copy arm to a pass-through arm, and `-O2` deletes the unobserved
  allocation that a stranded buffer would show up as.

      selfhost_codegen        ok      selfhost_parser_types   ok
      selfhost_lexer          ok      selfhost_resolver       ok
      selfhost_parser         ok      selfhost_typechecker    ok
      selfhost_parser_items   ok

  **All three defects share one root, and it is worth naming as a class.** Every
  one is a guard or an accessor written while inline descriptors could not reach
  it, so its assumption about the value it was handed was never tested:

  | # | site | what was wrong |
  |---|---|---|
  | 1 | `param_own.rs` enum-payload store | used the read-only accessor to STORE a descriptor, writing a spill-slot address into a payload word |
  | 2 | `vec_method.rs` mutation chokepoint | its "is this a Vec?" guard tested `!vec_elem_types.contains_key(v)`, but that table holds Strings too |
  | 3 | *(none — 1 and 2 were the whole list)* | |

  ### THE SIDE-TABLE MISREADING, because it happened twice

  Defect 2 is the instructive one. The guard read:

  ```rust
  && !self.var_types.vec_elem_types.contains_key(var_name)   // "not a Vec" — WRONG
  ```

  `vec_elem_types` maps **any** `{ptr,len,cap}`-shaped local to its ELEMENT type.
  A pattern-bound or let-bound `String` is in it with element `i8` —
  `pattern_binding.rs`'s own comment calls it "the side table METHOD DISPATCH
  reads to pick the String-shaped arm." So the guard skipped promotion for
  exactly the Strings that needed it, and this doc's own description of it
  ("erring toward promoting whenever the receiver is not positively known to be a
  `Vec`") was backwards.

  Measured: the self-hosted item parser SIGSEGV'd in
  `collect_leading_doc_comments` on

  ```kara
  Some(prev) => { let mut joined = prev; joined.push_str("\n"); … }
  ```

  — an **invalid write of size 1** at address `0x656e6f20656e696c`, which is the
  little-endian ASCII of `"line one"`: the string's own content bytes used as a
  destination pointer. Valgrind names the function and decodes the address in one
  step, which is why it found in minutes what reading could not.

  **The guard is now gone rather than corrected.** Both Vec guards in this
  campaign were wrong in the same unsafe direction, and there is no reliable
  POSITIVE Vec signal to test (`var_type_names` carries struct names, not
  container names; `string_vars` would reintroduce the same failure the moment an
  entry is missing). So the mutating set promotes unconditionally:
  `sso_deinline_in_place` is a not-taken branch for a `Vec` — its `cap` is a
  count, never negative — and compiles out entirely with SSO off. The cost on
  `Vec.push` is one load, one compare and one not-taken branch. **Re-introduce a
  guard only off a positive Vec signal and only with a measurement showing that
  cost matters**; never off the absence of a String signal.

  ### What the masking cost, as a lesson

  A gate nobody could grep for was not just hiding 2.3 billion instructions of
  performance. It was **holding a whole value representation out of the
  compiler**, and every site downstream of it had been silently exempted from
  supporting inline Strings. That is why "flip one predicate" was the wrong
  mental model and "de-mask, then fix what surfaces" was the right one — and why
  the selfhost differentials, not the unit suite, were the instrument: they are
  the only tests that run a real 20,000-line Kāra program end to end and compare
  it against an independent implementation.

  ### Four mechanisms were falsified before this one was found

  Recorded because the ratio is the lesson. Each was plausible, each was derived
  by reading code or disassembly, and each was wrong or negligible:

  | hypothesis | predicted | measured |
  |---|---|---|
  | lost compare folding (`bcmp`) | "~5 of the 9 ms" | **0.9%** of the regression |
  | the descriptor round trip | fewer redundant stores | **negative** — instructions went UP |
  | the `Vec` len select | the diffuse cost | **42 instructions** out of 9.5 B |
  | `try_inline_into` call overhead | 61% of the regression | net **−612 M** — construction was always winning |

  Only end-to-end counters held up. The profile was worth more than all four
  readings of the source that preceded it.

  ### A near-miss worth keeping

  The first measurement of this fix reported allocations and instructions
  **exactly** unchanged — 9,507,462,371 to the single instruction — and was one
  command away from being written up as "the flip is a no-op, reverting." It was
  measuring a `karac` built two minutes earlier, from before the flip: the
  measurement was fired off the binary's mtime changing rather than off the build
  task's completion.

  An exact match to nine significant figures is not a null result, it is a
  fingerprint — a genuine no-op still perturbs layout and inlining. The cheap
  guard is `cmp` on the two artifacts: **a change that does nothing and a change
  you did not compile look identical from the outside**, and only one of them is
  worth reverting. Same shape as CLAUDE.md's stale-archive and `git archive`
  bisect traps — a result that fails to move when the input demonstrably did.

  ### What this means for the plan

  1. **The PERF objection IS ANSWERED, and the de-masking is done.** SSO is
     −21.9% instructions and 14.8% faster on the self-hosted lexer with 69% of
     its allocations removed, so Slice 2's gate ("instruction count + `malloc`
     leaf share must drop") passes on both halves for the first time. The gate
     flip and both of the latent bugs it un-masked are landed and gated.
  2. **The correctness blocker is FIXED.** The move-suppression disarm is now
     tag-aware, so the documented `UseAfterMove` guarantee holds at
     `KARAC_SSO=1`: both reproducers that SIGSEGV'd now exit 0 with a defined
     `""`, and a moved-from HEAP String still reads its stale bytes exactly as
     before. Worth recording that it was measured UNCHANGED by the de-masking
     first — the family resemblance ("inline descriptors reaching code written
     before they existed") made it reasonable to expect one fix to carry both,
     and it did not; they needed separate fixes at separate sites.
  3. **The synthetic shapes were re-measured and are UNCHANGED — done.** The
     prediction that they understated SSO was wrong: none of them makes a
     by-value consuming call, so the fixed gate never fired in any of them. What
     that buys is a better account of why this corpus mispredicted the real
     workload — it models what happens TO a slice and not how the slice TRAVELS
     — and a hard rule that an instruction delta is not evidence of speed on SSO
     work, since these benchmarks run 36% fewer instructions 63% slower. **The
     corpus needs a by-value-consume benchmark before it can be trusted to
     predict anything again.**
  4. **Branch-not-select stays a footnote** — measured at 0.9% of the regression
     it was filed against.
  5. **The read-path pessimism above was wrong, and the reason is worth keeping.**
     "The cost is diffuse, one select on every String read" was an inference from
     a per-function delta, not a measurement of the selects themselves. The
     diffuse cost was real but small; one concentrated gate was 2.3 billion
     instructions. **Attribute to a line before concluding a cost is structural.**

- **The synthetic tables RE-MEASURED, and the "stale" marking was WRONG
  (2026-09-12).** After the `dcopy.owned` flip landed, both tables above were
  marked STALE on the reasoning that every number in them was taken with the bad
  gate live and so understated SSO — the retained rows most of all, since a
  retained String is what gets deep-copied at a consuming call. Re-run against
  the fixed compiler, the allocation counts come back **byte-identical**:
  1,000,009 → 9 on each transient shape, 200,012 → 12 retained.

  **Because none of the four benchmarks exercises the fixed gate at all.**
  `emit_vecstr_defensive_copy` fires on a by-value consuming call into a function
  with an owned `String`/`Vec` parameter. Not one of the synthetics makes such a
  call — they slice, compare, and either discard or `push`. The self-hosted lexer
  makes two per token: `keyword_or_ident(text: String)` and
  `make_spanned(token: Token)`.

  **That is also why the synthetic corpus mispredicted the real workload**, and
  it is a sharper explanation than "the payoff is workload-shaped." The
  benchmarks modelled *what is done with a slice* (kept vs discarded) and omitted
  *how it travels* (by value into a callee, or not). The second axis turned out
  to carry the dominant cost — 2.3 billion instructions on the lexer — and no
  benchmark in the corpus had it. A shape-based taxonomy that does not include
  parameter passing cannot predict a compiler front end.

  ### INSTRUCTION COUNT IS NOT A PROXY FOR TIME ON THIS CHANGE

  The re-measurement turned up something the campaign has been quietly relying
  on, wrongly. On the synthetics, `KARAC_SSO=1` executes **far fewer
  instructions and takes far longer** — measured at 10× iterations so the
  ~5 ms process floor cannot explain it:

  **STALE — superseded by the table two sections below; the SSO=1 wall figures
  here were taken with the pre-`write_inline` encoder and are roughly double the
  current regression. The instruction counts and the throughput point stand.**

  | 10M iterations | instructions | wall, best-of-5 | throughput |
  |---|---|---|---|
  | `lexlike` `SSO=0` | 2,540,034,522 | 179 ms | **14.2 G instr/s** |
  | `lexlike` `SSO=1` | 1,619,437,650 (**−36%**) | 292 ms (**+63%**) | **5.6 G instr/s** |
  | `substr` `SSO=0` | 1,920,834,802 | 119 ms | **16.2 G instr/s** |
  | `substr` `SSO=1` | 1,630,235,033 (**−15%**) | 296 ms (**+149%**) | **5.5 G instr/s** |

  A ~3× collapse in instructions-per-second, and it is mechanical rather than
  mysterious: SSO trades a large number of **cheap, perfectly predictable**
  allocator instructions (glibc's tcache fast path in a tight loop) for a small
  number of **serially dependent** ones. An inline descriptor's data pointer
  comes out of a `cmovs`, so every read is a load whose *address* depends on a
  comparison — the address cannot be speculated, and the `bcmp` the opaque
  pointer forces is an indirect call on top. Instruction count cannot see a
  dependency chain.

  **So do not quote an instruction delta as evidence of speed on SSO work.** The
  two metrics happened to agree on the self-hosted lexer (−21.9% instructions,
  14.8% faster) because allocator work is a large share of a big program; on a
  tight loop they point in opposite directions. Wall time is the claim;
  instruction count is a diagnostic for *where* the work went.

  ### THE BRANCH EXPERIMENT: LLVM WON'T LET YOU RUN IT, AND HALF THE REGRESSION WAS THE ENCODER

  The table above prompted an obvious hypothesis: if the `cmovs` is what puts
  the data pointer on an unspeculatable address-dependency chain, emit a real
  BRANCH instead and let the (highly predictable) tag choose. Run 2026-09-12.
  Two results, and the second is the one that matters.

  **1. The experiment cannot be run at the accessor level.** A branch-and-phi
  variant of `sso_pick_data_ptr` was wired behind `KARAC_SSO_BRANCH=1`. The gate
  demonstrably fires — pre-optimization IR goes from 4 `select`s named
  `*.data_ptr` to 16 `*.dp.inl/.heap/.join` blocks — and the two final binaries
  are **byte-identical** (same SHA). SimplifyCFG's two-entry-phi folding
  re-forms the `select`: a phi of two already-computed values costs nothing to
  speculate, so LLVM always folds it back.

  The version that would actually test the hypothesis has to sink the *load*
  into the arms, because that is the only thing a branch buys — a load at a
  branch-dependent address can issue early, a load at a `cmov`-dependent address
  cannot. But the accessor hands back a POINTER, and its consumers are a memcpy
  source, a `bcmp`, a runtime call. Getting the loads inside the diamond means
  duplicating every consumer into both arms. **That is a redesign of the read
  surface, not a codegen tweak**, and it should not be attempted on the strength
  of a hypothesis that has never been isolated.

  **2. Half of `lexlike`'s regression was the ENCODER, and a correctness fix
  already removed it.** `write_inline` (landed for the 32-bit defect above,
  B-2026-09-12-20) writes the descriptor straight through `out as *mut u8`. The
  encoder it replaced built a 24-byte stack array, wrote 3 content bytes and a
  1-byte trailer into it, then read three overlapping 8-byte integers back out —
  a partial-store-then-wide-load, which x86 cannot store-to-load forward. Three
  stalls per constructed string, in a loop doing 10M of them.

  Measured by reverting the encoder and rebuilding the archives, everything else
  held fixed:

  | 10M iterations, best-of-7, idle | SSO=0 | SSO=1 old encoder | SSO=1 shipped |
  |---|---|---|---|
  | `lexlike10` | 178 ms | 295 ms (**+66%**) | 238 ms (**+34%**) |
  | `substr10`  |  61 ms |  78 ms (**+28%**) |  64 ms (**+5%**)  |

  The old-encoder column reproduces the table above almost exactly (295 vs its
  292), which is what licenses the comparison. **So the tight-loop regression
  recorded above is stale: it is now +34% and +5%, not +63% and +149%** — and
  nobody set out to fix it. The lesson is not about store forwarding; it is that
  a regression attributed to a value REPRESENTATION turned out to be half
  attributable to one avoidable detail of how that representation was written.
  Before concluding SSO is inherently bad for tight loops, the remaining 34%
  deserves the same treatment — and it needs a profiler, which the cloud
  container does not have (`perf` is absent; only `valgrind` is present).

  ### ABSOLUTE TIMINGS IN THIS DOC ARE NOT COMPARABLE ACROSS SESSIONS — RATIOS ARE

  Re-running the self-hosted lexer on an idle box the same day turned up a
  ~3.4x uniform speedup that has NOTHING to do with SSO, and the preserved
  binaries make it measurable rather than speculative — they were all timed
  back-to-back in one loop:

  | lexprof build | | wall, best-of-5 |
  |---|---|---|
  | `lexprof_sso0`  | 03:20, SSO=0 | 1203 ms |
  | `lexprof_sso1`  | 03:20, SSO=1 pre-de-masking | 1202 ms |
  | `lexprof_fix2`  | 04:57, SSO=1 post-de-masking | 1044 ms |
  | built now       | SSO=0 | **354 ms** |
  | built now       | SSO=1 | **298 ms** |

  Eliminated as causes, each by measurement rather than argument: **machine
  load** (old and new binaries timed in the same loop, same minute); **archive
  build form** (relinking against a deliberately wrong-form `cargo build`
  archive gives 355 ms vs 354 — it costs binary size, as CLAUDE.md says, not
  speed); **optimization level** (`KARAC_OPT_LEVEL=0` gives 946 ms and a 631 KB
  binary, against the preserved 436-449 KB); **`c9570e0`'s drop-handover fix**
  (reverting just `src/codegen/exprs.rs` to its parent and rebuilding gives
  347/299 — unchanged); and **a leak in the older builds** (the inverse of the
  guess: the NEW binaries peak at 33-35 MB RSS against the old ones' 16 MB, so
  the old ones were not retaining, they were churning). Several enum-payload
  free/drop fixes from other sessions landed in the window and remain the best
  unverified candidate; pinning it needs a real bisect.

  **What survives, and it is the part the flip decision rests on: the RATIO is
  stable.** 1203 -> 1044 is 13.2%; 354 -> 298 is 15.8%. The SSO win on the
  motivating workload reproduces across a 3.4x shift in the baseline it sits on.

  **The rule this earns:** quote SSO numbers as ratios measured within one
  session against one compiler and one set of archives, and never compare an
  absolute millisecond figure in this doc to one taken on another day. Every
  table here should be read as a within-row comparison only.

  One bookkeeping note: `substr.kara` as it survives in the harness allocates
  1,000,009 → 9, while this doc's `String.substring` table records 1,000,047 →
  47. Those are different programs, so that table's rows were **not** reproduced
  here and remain as originally measured.

- **THE KATA CORPUS A/B IS CLEAN — 1063 programs, zero divergences (2026-09-12).**
  The broadest correctness evidence SSO has, and the one net nobody had cast.
  Every `.kara` in `kara-katas` (1026 leetcode + 37 bespoke/oracle) built at
  `KARAC_SSO=0` and `=1` and run, comparing stdout AND exit code:

      examined 1063   built 1061   skipped 2   diverged 0

  Measured at `e1c4746`, with both legs built by the same `karac` binary (only
  the env gate differs between them).

  The two skips are a harness artifact, not a gap: both are `src/main.kara`
  inside a `kara.toml` project, which single-file `karac build <file>` cannot
  build. Re-run in project mode, that kata matches too — so the real coverage is
  **1063 of 1063**.

  **Why this is the net that mattered.** The `--features llvm` suite is 109
  binaries of largely targeted fixtures; the katas are a thousand independently
  written programs doing whatever string manipulation their problem suggested,
  by authors not thinking about descriptors. That is exactly where this
  campaign's defects lived — every one was in code that had never seen an inline
  descriptor: a guard keyed on a side table that means something else, a
  read-only accessor used to store, an encoder assuming 64-bit pointers.

  **Two controls, both from `kara-katas`' own rules.** `KARAC_HASH_SEED=7` is
  pinned, because `Map`/`Set` iteration order is per-process random and unpinned
  it manufactures divergences that are explicitly NOT compiler bugs;
  `KARAC_AUTO_PAR=0` is held fixed, since auto-par is a third surface and letting
  it vary would make any difference unattributable.

  **What it does NOT cover**, so nobody over-reads it: these are x86-64 native
  builds, so B-2026-09-12-20's 32-bit overlay defect is invisible here; `--interp`
  and the auto-par surface are deliberately held fixed rather than compared; and
  it is one pinned hash seed.

  **The harness's own vacuous-pass, caught by a pilot.**

  Worth recording because it is the third instance of one shape in this campaign.
  The first run of this harness reported `examined=30 built=0 skipped=30
  diverged=0` — it compared NOTHING and reported agreement, because it `cd`s to a
  temp dir and then passed a path relative to the corpus root. On the full corpus
  that would have printed "1063 examined, 0 diverged" and read as a clean sweep.

  A 30-kata pilot caught it in seconds, and what made it visible was that the
  harness prints `built` alongside `diverged` rather than only a verdict. The
  same tell would have caught the other two: a benchmark whose "correctness
  check" summed token COUNTS and so could not see corrupted token TEXT, and a
  single `pick(…, 3)` probe that could not see a defect confined to bytes 4..=7.
  **A comparison that can report agreement without comparing anything must
  publish its denominator.**

- **Slice 4 (optional, "go further").** Pair with the lexer source-slices (below) to get
  the hot path to Rust *zero*-copy; small-string fast paths in concat/compare.

## THE KATA CORPUS, TIMED — the measurement the flip actually turns on (2026-09-13)

The corpus had been swept for correctness (1063/1063 byte-identical) and **never
timed**, which is a strange gap for a performance feature and the reason the
flip question kept resisting an answer. 340 bench katas, interleaved rails,
`KARAC_HASH_SEED=7` and `KARAC_AUTO_PAR=0` held fixed as in the correctness
sweep. 322 measurable (the other 723 corpus katas run in ~5 ms of process
startup and cannot show a representation change at all).

```
median per-kata delta : +0.7%
aggregate wall time   : 180.8s -> 186.5s   (+3.1% SLOWER)
  improved >=5%:  17        regressed >=5%:  79
```

**On the measurement that decides it, SSO-by-default makes the corpus slower,
and regressions outnumber improvements nearly 5:1. The answer to the flip is
NO.** That verdict rests on an aggregate over 322 programs, which averages
per-kata noise out; it is the robust part of everything below.

### But the cost is not the String tradeoff — it is that codegen cannot tell a Vec from a String

The worst regressions contain **zero Strings**. `queue_using_stacks`,
`largest_rectangle`, `stack_using_queues`, `paint_ii`, `num_trees`,
`right_side_view` — pure Vec/stack/queue programs, on which a String
optimization should be a no-op. Static code barely moves (+157 instructions,
+5 `cmov` on a 62,000-instruction binary), so the cost is dynamic and in a hot
loop.

The site is `vec_method.rs`'s mutating-method chokepoint: `push`/`pop` are in
`MUTATES_RECEIVER_IN_PLACE`, and `sso_deinline_in_place` runs there
**unguarded**, taking the descriptor's ADDRESS — which forces a register-
resident `Vec` descriptor to memory on every push. That guard is unguarded
deliberately: it used to key off `vec_elem_types`, which is not a "this is a
Vec" table, so it skipped promotion for exactly the Strings that needed it and
the self-hosted item parser SIGSEGV'd. The comment left there says to
re-introduce a guard *only off a positive Vec signal and only with a measurement
showing the cost matters*. This is that measurement.

**Upper-bound probe.** A deliberately-incorrect compiler with that call compiled
out, three rails (`SSO=0`, `SSO=1`, `SSO=1`-probe) built and timed per kata in
ONE pass, best-of-15, on the 29 katas whose regression clears the noise floor
(>=40 ms AND >=15%). Probe output is compared against the `SSO=0` rail and the
kata excluded on mismatch, so a crash cannot read as a speedup (0 excluded).

```
recovery = (real - probe) / (real - sso0)
  median 90%      IQR 55%..100%      >=80%: 18/29      <=20%: 4/29
```

Several land exactly on baseline — `paint_ii` 257 -> 561 -> **257**,
`num_trees` 757 -> 1009 -> **757**, `right_side_view` 703 -> 1008 -> **703**.

**A SECOND cost exists for a minority.** Four katas recover <=20%, and three of
them (`zigzag`, `flatten_2d`, `largest_rectangle`) are also String-free — so
they pay a Vec cost somewhere the probe does not touch: the tag-aware READ
accessors, which apply their select to `Vec` too. Same root cause, different
site. `src/codegen/sso.rs`'s own comment already names it ("keeping `Vec` off
the select entirely is the Slice 3 perf refinement").

So this is not two fixes. **Threading the Kāra type to the accessors — the
standing Slice 3 item — fixes both**, and the probe puts a floor under what it
is worth: ~90% of the regression on two thirds of the worst-hit programs.

Note the direction of the bound: the probe removes the de-inline for Strings
too, which a correct fix cannot. On a String-free program (most of these) a
positive-Vec-signal guard recovers the full amount; where a String receiver
actually mutates, it recovers less.

### The two readings that had to be thrown away first

Worth recording, because both looked like results.

**Cross-sweep comparison is confounded on this container.** The probe was first
run as a separate full sweep and compared against the original: aggregate +3.1%
-> +0.7%, regressions 79 -> 37, a tidy story. It is not readable. The `SSO=0`
rail — which the probe cannot affect — moved from 180.8s to 242.3s between the
two sweeps, a 34% baseline drift with no stray process on the box (load 0.45),
i.e. host throttling. Per-kata deltas did not reproduce at all: one kata's
regression moved 93.5% -> 29.4% while another's moved 49.2% -> 92.7%, under a
change that can only help both.

**Per-kata resolution at best-of-5 is below the effect size.** The three-rail
design has a free self-check — the probe strictly removes work, so
`probe > real` is physically impossible. At N=5 over 79 katas that fired
**15/79 (19%)**, and only 55/79 of the regressions reproduced at all, so ~30% of
the population selected on the first sweep was noise. Its headline "median 80%
recovery" was computed over rows like `sso0=420 real=422 probe=409` — a 2 ms
numerator over a 2 ms denominator, rendered as 650%. At N=15 on high-effect
katas the same check fires **1/29** and 28/29 reproduce.

**The rule:** on this container, trust corpus AGGREGATES and distrust any
individual kata's delta unless it clears ~40 ms and survives a high-N three-rail
pass. Publish the impossible-sign count alongside any per-kata claim — it is the
cheapest noise floor available and it is what caught both bad readings here.

### ITEM #4, FIRST HALF: Vec is off the mutating-method SSO path (2026-09-13)

The measurement the standing comment at `vec_method.rs` asked for came back
saying the cost matters, so the guard it asked for is now in — off a POSITIVE
signal, which is the whole difference from the two that were wrong before.

```rust
fn receiver_is_definitely_not_string(&self, var_name: &str) -> bool {
    self.var_types.var_elem_type_exprs.contains_key(var_name)   // positive Vec/Slice signal
        && !self.var_types.string_vars.contains(var_name)        // and no positive String signal
}
```

**Why this signal and not the previous two.** Both earlier guards read the
ABSENCE of a String signal as evidence of a `Vec`. `vec_elem_types` cannot serve:
it maps any `{ptr,len,cap}`-shaped local to its ELEMENT type and a `String` goes
in it with element `i8`, so the guard skipped exactly the Strings that needed
promotion. `vec_elem_type_for_var` is worse — it DEFAULTS to `i64` for an absent
variable, so "unknown" and "`Vec[i64]`" are the same answer.
`var_elem_type_exprs` is different in kind: the registrar in `stmts.rs` branches
on the binding type, putting a `String` into `vec_elem_types` + `string_vars` in
one arm and everything else into `var_elem_type_exprs` in the other — its own
comment says "a String is none of them". All five insert sites were checked (the
registrar, a slice, a `.chars()` binding, a shadow restore, a match-binding
copy); none introduces a `String`. `control_flow.rs` already relies on the same
distinction. Both halves are required, and anything unknown answers `false` and
keeps the SSO path: guessing wrong costs silent memory corruption one way and a
few instructions the other.

**Measured, three rails in one pass at best-of-15** — `SSO=0`, `SSO=1` guarded,
and `SSO=1` with the de-inline compiled out entirely (the ceiling) — over the 29
katas whose regression clears the noise floor:

| | before | after |
|---|---|---|
| median regression vs `SSO=0` | **+39.3%** | **+2.1%** |
| still >=15% regressed | 28 / 29 | **10 / 29** |
| within +-5% | 0 | **16** |

Median capture of the available ceiling: **82%**. The guarded and probe columns
track each other closely almost everywhere (`stack_using_queues` 294 vs 293,
`largest_rectangle` 1766 vs 1769), which is the real confirmation: the guard got
essentially everything removing the call could give, so the signal fires where
it should. Several land exactly on baseline (`paint_ii` 258 vs 258, `rangesum`
681 vs 681, `meetpoint` 651 vs 651) and a few now beat it (`edit_distance`
-10%, `triangle` -11%).

**The 10 residual regressions are the SECOND cost, and they are now measured
rather than inferred.** For `zigzag` (+92%) and `largest_rectangle` (+85%) the
PROBE does not help either — 1389 vs 1392, 1766 vs 1769 — so they are not paying
at this site at all. That is the tag-aware READ accessors applying their select
to `Vec`, which `src/codegen/sso.rs` has named as the Slice 3 refinement all
along. Reaching it means threading the receiver's name (or type) into
`sso_string_data_ptr_from_slot` / `sso_string_parts_from_value`, which take a
slot or a bare SSA aggregate and have no var in scope. That is item #4's second
half, and it is worth roughly a third of the high-effect regressions.

**One check retired.** The three-rail design's impossible-ordering check (a probe
that strictly removes work can never be slower) is what caught two unreadable
sweeps, but it is MEANINGLESS in this comparison: guarded and probe emit nearly
identical code for these programs, so a 1-2 ms ordering between them is noise,
not a defect. Reported here so nobody reads its 16/29 as a failure — a check
outside the comparison it was built for should be retired, not quoted.

### ITEM #4, SECOND HALF: Vec is off the READ path too (2026-09-14)

The first half took `Vec` off the mutating-method chokepoint. The residual
regressions pointed at the tag-aware READS, and `src/codegen/sso.rs` had named
them as the outstanding Slice 3 refinement from the beginning. This closes that.

**Attributed to the line before anything was changed**, which is the discipline
this campaign keeps having to relearn. A `Vec[i64]` index+len loop with no
Strings anywhere, dumped through `examples/dump_ir` at `KARAC_SSO=1`, emitted 6
inline compares, 3 selects and 4 `byte_len` decodes. Four of the six were one
call — `v.len()` in the loop condition:

```llvm
%sso.inline     = icmp slt i64 %vec.len.cap, 0
%vec.len.ilen.sh = lshr i64 %vec.len.cap, 56
%vec.len.ilen   = and  i64 %vec.len.ilen.sh, 127
%vec.len.byte_len = select i1 %sso.inline, i64 %vec.len.ilen, i64 %vec.len15
%lt16 = icmp slt i64 %j13, %vec.len.byte_len
```

`lshr`/`and`/`select` on the trip count of every iteration, for a flag a `Vec`
can never set. The arm's own comment already said so: "This arm serves Vec too,
where the flag is never set and the select is the identity — keeping Vec off the
select is the Slice 3 refinement."

**The change.** `compile_vec_method` already has `var_name` in scope, so the
receiver-aware gate is computed once at the top and the **13 read sites** route
through it — `len`, `is_empty`, `bytes`, and the ten `(recv_data, recv_len)`
pairs. The mutation chokepoint folds into the same gate, so there is one
spelling rather than two. The predicate is unchanged from the first half
(`receiver_is_definitely_not_string`), and the reasoning for why THAT signal is
safe where two earlier guards were not is documented at its definition.

**One site deliberately left on the plain gate:** the `substring` inline
CONSTRUCTION arm. A receiver positively identified as a non-String never reaches
`substring`, so narrowing it buys nothing and widens the diff — and construction
is a different risk class from a read.

**Re-read the IR to confirm the instructions actually disappeared**, rather than
inferring from a timing delta:

| `Vec[i64]` loop, `KARAC_SSO=1` | inline compares | `byte_len` decodes |
|---|---|---|
| before | 6 | 4 |
| after | **4** | **2** |

and the loop condition is now a plain `load i64, ptr %vec.len.ptr`. The residual
4/2 are the `println(f"{total}")` path — a genuine String, correctly tag-aware.

**The risk delta is real and worth naming.** The first half touched one MUTATION
site; this touches thirteen READS. A misclassified String on a read takes the
raw `len`/`data` fields, and on an inline descriptor those are overlaid content
bytes — a plausible-looking pointer and a garbage length, silently. That is why
the gate bar did not move: all eight selfhost differentials green at
`KARAC_SSO=1` (the item parser among them, which is what caught the last bad
guard on this file), 110 binaries per leg, and both ASAN ratchet legs at 1618
passed with the quarantine matched exactly.

### THE CORPUS RE-TIMED WITH BOTH HALVES — the flip is no longer disqualified (2026-09-14)

Same 340 bench katas, same interleaved rails, same controls, on a compiler
carrying both halves of item #4 and the two overnight allocation-path commits
(`fea7ac0` concat chain, `fd6750a` scalar map probe) that made the old baseline
stale.

| | unguarded (2026-09-13) | both halves (2026-09-14) |
|---|---|---|
| median per-kata delta | +0.7% | **+0.4%** |
| aggregate wall time | **+3.1%** | **+0.2%** |
| improved >=5% | 17 | 12 |
| regressed >=5% | **79** | **17** |
| within +-5% | 226 | **292** |

**Regressions fell from 79 to 17 and the aggregate from +3.1% to +0.2%** — at
the noise floor. The verdict "SSO-by-default makes the corpus slower" no longer
holds; it now makes it neither faster nor slower, with a ~15% win still standing
on the motivating self-hosted-lexer workload.

**Every Vec-heavy regression is gone**, which is the confirmation that the two
guards attacked the right thing:

| kata | unguarded | both halves |
|---|---|---|
| `largest_rectangle` | +93% | **-1%** |
| `queue_using_stacks` | +93% | **+1%** |
| `zigzag` | +49% | **-3%** |
| `stack_using_queues` | +44% | **-2%** |
| `paint_ii` | +48% | +5% |
| `flatten_2d` | +11% | -2% |

`zigzag` and `largest_rectangle` are the two that had resisted BOTH the first
half and the upper-bound probe, and were predicted from that to be paying at the
read accessors. They were.

**What remains is the genuine tradeoff** — though these per-kata figures are
best-of-3 and do NOT survive re-measurement; see the section below, where 10 of
the 17 turn out to be noise and the worst three are far worse than stated here.
The residual losers are String programs — `vertical`, `shortest_distance`,
`alien_seq` — not Vec programs paying a tax. No further guard reaches these: they are SSO doing what SSO does, trading a
malloc for a tagged read, on workloads where the read side dominates. The one
String-free straggler is `row_buffers` (+29%), which is worth a look on its own
terms.

The wins are concentrated too, and on the shape the campaign predicted:
`exprops` -25%/-24% (allocation-heavy expression building),
`palindrome_partitioning` -17%, `word_pattern_ii` -17%.

**So the decision question has changed shape.** It is no longer "SSO costs the
corpus 3%" but "SSO is corpus-neutral, wins ~15% on the compiler's own hot
workload, and leaves 17 String programs measurably slower." That is a judgement
about which programs matter, not a measurement gap — and the 17 are few enough
to examine individually if someone wants the last word.

### THE RESIDUAL, MEASURED PROPERLY: 7 not 17, and the mechanism is promote-on-mutation

The corpus sweep runs best-of-3, and its per-kata rows do not survive contact
with a careful measurement — the aggregate does, because noise averages out over
321 katas, but individual rows do not. Re-running all 17 claimed regressions at
best-of-15 interleaved:

| kata | sweep N=3 | N=15 | |
|---|---|---|---|
| `vertical` | +36% | **+86%** | real |
| `shortest_distance` | +34% | **+69%** | real |
| `shortest_distance_iii` | +14% | **+37%** | real |
| `word_ladder` | +12% | +12% | real |
| `alien` / `alien_seq` | +19/26% | +11/11% | real |
| `atoi` | +8% | +5% | real |
| **10 others** | 5–29% | −4%..+4% | **noise** |

**The noise cut BOTH ways**, which is the part worth internalising: 10 of 17
evaporated (`row_buffers` +29% → +3%, `interleave_unchecked` +19% → +1%) while
the worst three got substantially WORSE (+36% → +86%). A best-of-3 sweep is not
a conservative estimate of a per-kata delta; it is an unbiased-but-wide one, and
reading its tails as a cost list is wrong in both directions.

`row_buffers` deserves its own line because it was chased as an anomaly — a
String-free program still regressing after both guards. It is not a regression.
Its IR carries two move-out disarms and a `for c in pattern.chars()` loop running
~11k times against 200M char-ops; there was never a mechanism for a 29% cost. A
predicted cause (an index-expression receiver `rows[cur].push(…)` defeating the
guard) was refuted first by a minimal `Vec[Vec[i64]]` repro that emits no tag
work at all.

**What the 7 survivors have in common.** `vertical`'s IR at `KARAC_SSO=1` adds 63
inline compares, and the dominant prefix is **`recv.mut` — 96 de-inline probes**,
not reads. The kata builds Strings character by character (`out.push(c)` in
`prefix_string`). Isolated:

```
char-by-char String builder, 4M calls
  build(20)   SSO=0  31ms   SSO=1  41ms   +32.3%
  build(60)   SSO=0  48ms   SSO=1  67ms   +39.6%
```

**`build(60)` can never be inline** — 60 bytes is past the 23-byte capacity, so
the string is heap from its first growth — and it still regresses 40%. So the
cost is not the promotion. It is the de-inline PROBE, emitted on every mutating
method call and doing nothing on every one of them.

**This is the price of a documented simplification, measured for the first
time.** The campaign chose "MUTATION PROMOTES rather than going tag-aware"
deliberately, and it is what makes the mutating surface correct without every
`push_str` read becoming tag-aware. What it costs was never measured: 32–40% on
String building, and it is the whole of the remaining corpus regression.
**[CORRECTED 2026-09-14 — the second clause is false.](#the-fold-the-growth-test-is-the-de-inline-test-2026-09-14)**
The probe was removed for `push`/`push_str` and `vertical` did not move: +84% →
+85%. The 32–40% on a *synthetic* builder was real and is now largely recovered;
the inference from it to "the whole of the remaining corpus regression" was a
static IR count standing in for an attribution.

**The alternative is the thing SSO is supposed to do.** Appending INTO the inline
buffer while the bytes still fit is the classic small-string win — no malloc at
all for a short built string — where promote-on-mutation instead pays the tag
check and then throws the inline representation away. That is a design slice, not
a guard: it makes `push`/`push_str` tag-aware on the write side rather than
promoting. Filed as its own ledger row rather than carried here, because it
outlives this spike.

### THE FOLD: the growth test IS the de-inline test (2026-09-14)

The row's own prescription, implemented: `String.push` and `String.push_str` no
longer emit a head-of-op `sso_deinline_in_place` branch. They already test
whether the buffer must grow, and `emit_string_buffer_grow` already routes an
inline receiver (`SGT cap, 0` is false) to its fresh-malloc arm, whose memcpy
source is the tag-aware data pointer. So the growth test is made to answer both
questions at once:

```llvm
;                        ... unchanged from KARAC_SSO=0 ...
  %spush.needs_grow     = icmp ugt i64 %spush.new_len, %spush.cap
  %sso.inline4          = icmp slt i64 %spush.cap, 0          ; + 1 instruction
  %spush.grow_or_inline = or i1 %spush.needs_grow, %sso.inline4 ; + 1 instruction
  br i1 %spush.grow_or_inline, label %spush.grow, label %spush.copy
```

An inline receiver is forced onto the grow edge and is promoted inside the
allocation the grow was going to perform anyway. **Two ALU ops and no extra
basic block**, where before there was a branch and a whole `recv.mut.deinline`
block ahead of the loop body's every iteration. The `lshr`/`and`/`select` that
decodes the overlaid `len`/`cap` moved into the grow block, which is cold.

`MUTATES_RECEIVER_IN_PLACE`'s other ~27 methods keep the head-of-op promote.
The two that opt out are the two that have a growth test to fold into.

**Measured — two independent samples, best-of-15, three rails built and timed
per program in ONE pass** (`SSO=0`, `SSO=1` folded, `SSO=1` at `7b6ebe8`), so
every delta is a within-pass ratio rather than a cross-sweep one.

> **⚠ THIS TABLE WAS MEASURED WITH AUTO-PAR ON, which this document's own kata
> sweep names as a control it holds fixed.** `bench/sso/bench.sh` did not pin
> `KARAC_AUTO_PAR=0` until 2026-09-15, and every micro rail except `lexlike`
> fans its driver loop out, so these are PARALLEL-THROUGHPUT deltas that
> understate per-iteration cost. The before/after comparison below is still
> internally valid — both columns came from the same unpinned harness, so the
> fold's improvement is real — but the absolute magnitudes are not the cost of
> SSO. See "Re-measured with the control applied" immediately after this table.


| rail | before | after | |
|---|---|---|---|
| `promote` — inline receiver, promoted then appended | +25.4 / +25.8% | **+7.7 / +8.8%** | −17 pts |
| `builder60` — 60-char build, never inline | +28.4 / +29.7% | **+10.7 / +11.4%** | −18 pts |
| `pfx_idx` — `vertical`'s builder, runtime-varying length | +17.8 / +19.4% | **+4.1 / +5.6%** | −14 pts |
| `builder20` — 20-char build, CONSTANT trip count | +17.2 / +16.1% | +15.8 / +14.8% | −1 pt |
| `pfx_chars` — the same builder driven by `chars()` | −15.3 / −15.3% | **−9.0 / −10.2%** | **+6 pts, a LOSS** |

Both directions reproduce across the two samples, including the loss.

**`promote` is the paired control, and it is the reason to believe the rest.**
The previous attempt on this row made the check cheaper by making the promotion
four times more expensive, which would have looked like a win on a builder rail
alone. Here the promotion path improves by MORE than the never-inline path, so
no cost was shuffled between them.

**`builder20` is a measurement artifact, not a short-string result.** It calls
`build(20)` with a compile-time-constant trip count, so LLVM unrolls the loop
and the rail stops measuring per-push cost. `pfx_idx` has the same 0–20 length
distribution with a runtime-varying bound and gains 14 points. A first reading
of this table blamed string length; the two rails differ only in whether the
count is a constant.

**`pfx_chars` gives back 6 points and the loss is real.** It is a rail where SSO
already wins, and two ALU ops per iteration cost more there than a
perfectly-predicted branch that is never taken. Net across the corpus this is
comfortably paid for, but it is not a free change.

#### Re-measured with the control applied (2026-09-15)

`bench.sh` now pins `KARAC_AUTO_PAR=0` at both build sites. Same machine,
`RUNS=15`, two independent samples, current `main`. These are the per-iteration
numbers the table above was meant to report:

| rail | unpinned (above) | **pinned** | |
|---|---|---|---|
| `promote` | +7.7 / +8.8% | **+7.8 / +7.9%** | unchanged |
| `builder20` | +15.8 / +14.8% | **+15.1 / +14.1%** | unchanged |
| `builder60` | +10.7 / +11.4% | **+11.9 / +12.5%** | slightly worse |
| `pfx_idx` | +4.1 / +5.6% | **+14.1 / +15.9%** | **~3x worse** |
| `pfx_chars` | −9.0 / −10.2% | **−10.3 / −10.3%** | unchanged |
| `substr` | +5% (2026-09-12, not reproducible) | **+33.7 / +34.3%** | see below — sign flips |
| `lexlike` | +34% (2026-09-12, NOT REPRODUCIBLE) | **−12.7 / −13.7%** | was never +34% — B-2026-09-15-1 |
| `lexer` | −15.8% (2026-09-12) | **−17.7 / −17.5%** | slightly better |

**What the pin changed, and what it did not.** The rails whose per-iteration
cost was being divided across cores moved a lot (`pfx_idx` ~3x, `substr` ~7x);
the rails dominated by per-call malloc rather than per-iteration ALU work
(`promote`, `builder20`) barely moved. So the fold's *conclusions* survive — the
paired-control argument, `builder20` being an unroll artifact, and `pfx_chars`
being a real loss are all unchanged — but **the per-push SSO tax is roughly
three times what the unpinned table reported.**

**`lexlike` never regressed, and the paragraph that stood here said it did.**
This section first claimed `lexlike` went +34% → −13% unexplained and listed
three candidate commits. The prescribed experiment was then run — the committed
rail built at five commits spanning 09-12..09-15, auto-par pinned, best-of-9 —
and it is **flat at −12 to −14% everywhere**, including `bab0491`, the commit
the +34% was attributed to:

| commit | | `SSO=0` → `SSO=1` |
|---|---|---|
| `bab0491` | the inline overlay is 64-bit-only | 260 → 226 ms, **−13.1%** |
| `4c544f2` | keep `Vec` off the mutating-method path | 263 → 228 ms, −13.3% |
| `09f795a` | keep `Vec` off the READ path too | 264 → 226 ms, −14.4% |
| `c1adb9c` | the growth test IS the de-inline test | 258 → 227 ms, −12.0% |
| `27466ba` | main | 261 → 229 ms, −12.3% |

`bench/sso/lexlike.kara` did not exist at `bab0491`. It was committed by
`5bcafe9` at 09-13 00:04, 1h42m later, and `5bcafe9` also wrote the header line
carrying "lexlike 34% SLOWER (2026-09-12)" — so that figure measured an
uncommitted pre-harness workload and was transcribed above rails that replaced
it. Refuted and recorded as B-2026-09-15-1 (**invalid**).

Worth stating plainly, because this document has now made the same mistake
twice in two directions. B-2026-09-14-20 promoted a static IR count to an
attribution; measurement refuted it. The paragraph above declined to credit any
commit — the right instinct — but still accepted that the movement HAPPENED,
without checking that its baseline was reproducible. Two numbers from different
dates disagreeing is not a finding until both are reproduced on the same
workload. The baseline is the first thing to re-measure, not the last.

**The GAP between `substr` and `lexlike` was never a finding, and the IR diff
proposed here is what dissolved it.** The two rails cost the SAME at
`KARAC_SSO=1` — 217 ms and 215 ms, 0.9% apart, samples interleaving. The
"46–52 point gap" was (+72%) − (+23%): two ratios with equal numerators and
unequal denominators. What differs is the `KARAC_SSO=0` baseline (126 ms vs
176 ms), because `substr`'s substring lowering is call-free in IR there while
`lexlike` routes through `karac_string_slice` in both legs. B-2026-09-15-6 is
closed `invalid`.

The arm64 section of that row already contained the same observation — "at
`SSO=1` the two rails are indistinguishable (cycles 1.0232, distributions
overlap)" — and both sessions read it as *the gap has no arm64 instance* rather
than as *the gap is arithmetic*. On both ISAs the numerators agree; only the
baselines differ.

**What the diff found instead is B-2026-09-15-13.** SSO adds an opaque
`karac_string_try_inline_into` call to a lowering that had none. Emitting the
encoding as IR — `memcpy`, zero-fill, byte-23 flag, exactly `write_inline` —
runs the rail in **22 ms** against 212 ms, output identical at 10M/20M/40M
iterations and scaling linearly (2.2 / 2.0 / 1.9 ns per iteration, against
12.7 / 12.4 / 12.3 at `KARAC_SSO=0`).

Two other hypotheses were tested and REFUTED on the way, both of them this
document's own. Annotating the call `memory(argmem: readwrite) nounwind
willreturn` — so LLVM knows it touches only its arguments — changed nothing
(213 ms vs 212 ms). Hand-folding the comparison's literal side changed nothing
either (211 ms), which retires the store-to-load-forwarding prediction this
section used to make: the shape is real (a `String == String` under SSO spills
BOTH operands and reloads `cap`, 7 IR instructions becoming ~18, including on a
literal whose `cap` is a constant 0) and it is not where the time goes. The call
is an optimization barrier, not an aliasing or a spill problem.

**An earlier version of this section said something sharper and wrong, in the
paragraph immediately below the warning against exactly this.** It claimed
`substr` flipped SIGN with the auto-par setting — a 14.7% SSO *win* fanned out
against a 30.9% loss pinned — on a single best-of-9 run at one commit. The win
half does not reproduce. Six trials at the default straddle zero
(−1.7 / 0.0 / −1.7% and +0.0 / −1.7 / +1.7%), none within 13 points of −14.7%,
and a `KARAC_PAR_WORKERS` sweep is flat at 1, 2, 4, 8 and 18 workers. The
−14.7% was one run on a loaded box. Best-of-9 within a single configuration
does not protect against a loaded box, because every one of the nine is loaded;
only a repeat at a different time does, which is what the paragraph above this
one says and what this table did not do.

**What replaces it: auto-par does not uniformly compress — it moves different
rails in OPPOSITE directions.** Re-measured at `f72fac4`, host B, `RUNS=15`,
three samples of each leg:

| rail | pinned | default (fanned out) | |
|---|---|---|---|
| `lexlike` | +20.9 … +24.0% | +21.7 … +22.9% | unchanged — does not fan out |
| `substr` | +72.8 … +74.4% | **+0.0 / −1.7 / +1.7%** | masked to zero |
| `pfx_idx` | +15.9 … +18.2% | +7.1 … +14.3% | compressed |
| `builder60` | +2.4 … +3.6% | **+11.7 … +13.6%** | **amplified ~4x** |
| `builder20` | +1.8 … +3.7% | +4.5 … +7.0% | amplified |
| `promote` | −0.8 … +0.8% (zero) | +2.9 / +2.9 / +2.9% | zero → real |
| `pfx_chars` | −7.5 … −8.5% | −6.7 … −12.9% | roughly same |
| `lexer` | −13.4 … −19.1% | −17.4 … −18.2% | roughly same |

The rails whose SSO cost is per-iteration serial work (`substr`, `pfx_idx`) get
compressed, because parallelism hides it; the ones that pay it in allocator
traffic (`builder*`, `promote`) get amplified, plausibly because four workers
contend on the allocator. **That mechanism is a hypothesis and has not been
measured.** The amplification has been: `builder60` is four times worse fanned
out than pinned, consistently across three samples. So the pin is a control in
both directions — it changes the SIGN of the answer for `substr` and the
MAGNITUDE for `builder*`, oppositely.

`lexlike` remains the control that makes the table readable, and that half of
the old section survives: the analyzer declines to fan it out (175–177 ms
pinned, 175–176 ms fanned), so its delta is identical under both settings.

#### The 17 residual katas, re-timed

| | at `7b6ebe8` | folded |
|---|---|---|
| median | +3.5% | **+1.4%** |
| aggregate | +7.9% | **+5.7%** |
| regressed ≥5% | 6 | **4** |
| improved ≥5% | 0 | **2** |

`alien_seq` +8.4% → **−11.9%** and `alien` +11.0% → **−8.1%** cross from
regression into win; `word_ladder` +14.6% → +6.4%.

#### What this REFUTES, including in this document

`vertical`, the worst regression in the corpus and the kata this row's mechanism
was derived from, **did not move: +84.1% → +85.4%.** Its IR now contains zero
`recv.mut.deinline` blocks — the fold applied — and the kata is unchanged.
`shortest_distance_iii` likewise: +35.7% → +36.1%.

The attribution those rested on was "`vertical`'s IR adds 63 inline compares and
the dominant prefix is `recv.mut` — 96 de-inline probes". **That is a static
count of IR occurrences, and a static count is not an attribution.** Removing
all 96 is worth about 1% on this kata.

Nor is it `prefix_string`, the function the count pointed at. `pfx_chars` mirrors
it exactly — a `ref String` walked with `chars()`, pushed into a fresh String,
returned by value — and that shape is **9–15% FASTER** under SSO, in the
opposite direction from the kata containing it.

So `vertical`'s +85% is, as of this writing, **unattributed**. The leading
suspect is the other loop: `longest_common_prefix` scans `strs[s].bytes()` and
`other[col]` over a `Vec[String]` tens to hundreds of times per outer iteration,
against at most 20 pushes. Those are tag-aware READS on String elements reached
through an INDEX EXPRESSION, which the item-#4 receiver guard structurally
cannot help — the receiver is not a named variable, and the elements really are
Strings, so the guard would be wrong to fire. `shortest_distance` and
`shortest_distance_iii` scan `Vec[String]` the same way. That is a hypothesis
with a mechanism and no measurement behind it yet, and it is filed as its own
row rather than asserted here — this section exists because the last mechanism
asserted from a static count was wrong.

#### Correctness

Seven negative controls, each a deliberate single-gate backout rebuilt and
re-probed, all against a probe that sweeps lengths 0–40 across `push`,
`push_str`-first, self-append, empty-append and reserve-then-push, on
`--interp` (oracle) vs AOT vs JIT and both auto-par settings (this sweep is now
a permanent fixture, `test_sso_de_inline_rides_the_string_growth_test` in
`tests/cli.rs` — re-verified non-vacuous by backing the fold out, which makes
the `KARAC_SSO=1` leg SIGSEGV while the `=0` leg stays byte-identical to the
oracle, i.e. the default-off suite cannot catch this class at all):

| gate removed | result |
|---|---|
| `push` de-inline gate | SIGSEGV |
| `push_str` de-inline gate | SIGSEGV |
| `push` publish decoded length after promote | SIGSEGV |
| `push_str` publish decoded length after promote | SIGABRT |
| `push` decoded growth geometry | SIGABRT |
| `push_str` decoded growth geometry | SIGABRT |
| `push_str` inline receiver never aliases (`& !is_inline`) | SIGSEGV |

`KARAC_SSO=0` stayed green through every one, so each failure is the gate rather
than the probe.

**Two vacuous controls had to be fixed before that table meant anything**, which
is the campaign's recurring shape showing up twice more. The first `push`
control PASSED because it was run against a binary whose rebuild had not
finished — the codegen form of the stale-archive trap. The first `push_str`
control PASSED because the probe called `push` before `push_str` on the same
receiver, so `push_str` only ever saw an already-promoted heap string; it needed
a case where `push_str` is the FIRST mutation.

**One control caught a bug in this slice's own first draft.** That draft took the
self-append alias base tag-aware, `select(is_inline, slot, data)`. That makes the
alias range a STACK range, and an unrelated alloca landing inside
`[slot, slot + len)` reads as an alias and rebases a valid source pointer to a
garbage offset. The correct answer is that an inline receiver never aliases its
source at all — a borrowed slice of the receiver is rejected by the ownership
checker, and `out.push_str(out)` arrives as a value that
`sso_string_parts_from_value` spills to its own entry-block alloca. The guard is
now `in_range & !is_inline`, and its control segfaults, so the hazard was live
rather than theoretical.


## Verification matrix

- **The whole `--features llvm` suite at `KARAC_SSO=0` AND `=1`** — the two-leg
  gate cycle. This is the only thing that exercises inline descriptors broadly,
  and it is MANUAL: nothing schedules it, so it happens when whoever is holding
  the campaign remembers. Every SSO regression so far landed through a green
  single-leg gate set.
- `tests/cli.rs::test_sso_inline_string_survives_the_env_ffi_boundary` — the one
  `KARAC_SSO=1` fixture that runs unprompted. It is `#[cfg(feature = "llvm")]`,
  so it rides the `--features llvm` leg — which is the leg that has codegen at
  all — and it covers exactly one shape: a String crossing into a runtime
  extern. Growing this list is how the manual cycle above stops being
  load-bearing.
- `tests/cli.rs::wasm_browser_inline_string_survives_export_at_every_length` —
  the second unprompted `KARAC_SSO=1` fixture, and the only one that watches a
  32-bit target. It builds a `substring` export at `KARAC_SSO=1` and sweeps
  `n = 0..=30` under node, so it covers both the inline/heap boundary and the
  four-byte window B-2026-09-12-20 lived in. Skips cleanly without the wasm
  archive or node; a single length would not have caught the defect it exists
  for, which is why it is a sweep.
- `tests/codegen.rs` String suite (E2E) + the new dispatch tests.
- `tests/memory_sanitizer.rs` ASAN on macOS (UAF/double-free) **and** the Linux/LSan CI
  `memory-sanitizer` job (leaks — *the* gate, since SSO rewrites the free path; macOS
  cannot see leaks).
- `leaks --atExit` guardmalloc at **both O0 and O2** (codegen leaks and double-frees hide
  oppositely under optimization — `reference_macos_leak_detection_methodology`).
- Re-profile the self-host lexer (instruction-count gate) + corpus re-bench before any
  published number.
- **Rebuild the runtime archives first — all of them, including the two wasm
  ones — whenever `runtime/src` changes at all.** CLAUDE.md says this for
  measurements; SSO has now been bitten by it twice, the second time inside a
  BACKOUT (see "the negative control needed its own negative control"), where a
  stale archive does not distort a number but certifies a working gate as
  useless. The wasm pair is the easiest to forget because no native gate touches
  it.

## The complementary, separately-owned win (record — do NOT do here)

The self-host *number* specifically also closes by rewriting the lexer to **classify on
borrowed slices** (`s[a..b]`, clone only when an identifier is actually stored) —
`selfhost/src/main.kara:1239`, `:1260`, `:696/:703/:720`, the string/char-scan sites, etc.
**The string-match dispatch tree already works zero-copy on a slice** (it reads ptr+len,
which a slice has), so there is **no compiler blocker** — this is the
[`project_lexer_string_scan_shape`] lesson applied inside the lexer. SSO (no-malloc) and
slices (zero-copy) are complementary: SSO helps the whole corpus; slices get this one hot
path fully to Rust. This file is **selfhost-session-owned source** — filed here for that
session, intentionally not edited from a compiler-side worktree (the
two-sessions-one-file hazard).

## Cross-references

- [`selfhost-lexer-profile.md`](selfhost-lexer-profile.md) — the profile that motivates
  this (allocation = #1 leaf post-dispatch).
- String-match dispatch lever — commit `5adf2e90`; shares the accessor surface (its
  dispatch tree must route through the tag-aware accessor in Slice 1/3).
- `roadmap.md` § Codegen Optimization — the allocation-reduction entry points here.
- `reference_macos_leak_detection_methodology`, `project_self_hosting_v1_credibility`.
