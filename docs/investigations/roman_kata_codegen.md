# Roman-numeral katas — codegen investigation

**Status:** ✓ Resolved (2026-05-27). **Started:** 2026-05-27.
**Owner:** unassigned.

Surfaced while building kara-katas LeetCode #12 (Integer to Roman) and
#13 (Roman to Integer) bench mirrors. The benchmark numbers were clean;
*what they meant* was wrong in the first cut, and chasing the mechanism
turned up three codegen bugs plus a corrected perf story. This is where
that work lives. Three bugs fixed; two optimizations deferred with
triggers; one "optimization" declined with evidence.

## Status snapshot

| Finding | Status | Outcome |
|---|---|---|
| Range patterns matched unconditionally | ✓ Fixed | `f1b5a46` — `compile_pattern_condition` had no `RangePattern` arm; fell through to `_ => true`. i64/char/u8 all affected; interpreter was correct, codegen-only. |
| Parser rejected byte-literal patterns | ✓ Fixed | `f1b5a46` — `b'I'` now desugars to `Integer(_, U8)`, reusing the integer-pattern pipeline. |
| `Vec[Vec[T]]` index → `ref` param double-free | ✓ Fixed | `e56e8fc` — `vec[idx]` aggregate element passed to a `ref` param shallow-copied + dropped, freeing the outer Vec's buffer. Now borrows the element in place. |
| "Kāra's allocator path beats C" (kata 12/13 README claim) | ✓ Refuted + corrected | Kāra and C make byte-identical malloc calls. Real cause is clang over-vectorizing the generator. READMEs corrected (`kara-katas` `d0f9ae7`). |
| `match`-on-literals → LLVM `switch` | → Deferred | Impact nil on sparse fan-ins (LLVM rebuilds the same tree). Trigger in [`phase-7-codegen.md`](../implementation_checklist/phase-7-codegen.md). |
| LICM / partial-eval across pure calls | → Deferred | Synthetic-bench artifact; high effort. Trigger in [`phase-7-codegen.md`](../implementation_checklist/phase-7-codegen.md). |
| `memset_pattern16` bulk-fill recognition | ⊘ Declined | Would make karac *slower* on this shape — see § allocator mechanism. |

## Methodology

Two reusable probes, both no-sudo (dtrace is SIP-gated on unsigned
binaries on this host):

1. **`DYLD_INSERT_LIBRARIES` malloc-counting shim** — a ~30-LOC C dylib
   interposing `malloc`/`free`, counting calls + a size histogram,
   printed at exit. Run against own-compiled binaries only. Decisively
   answered "do Kāra and C allocate differently?" (no — identical).
2. **Parse-only harness** — pre-stake inputs *outside* the timed K-loop
   (no per-iter alloc), alternate two inputs by `k % 2` to defeat
   constant-folding, time only the kernel. Isolated `roman_to_int`'s
   algorithmic cost from the allocator + generator confound.

Both are worth rebuilding for any future allocator/codegen perf probe;
neither is checked in (scratch).

## Allocator mechanism — the kata 12/13 headline was mis-attributed

The kata 12 README originally explained Kāra's seq-lane lead over C as
"Kāra's path through libsystem `_malloc`/`_free` lands at the favorable
end of the spread for small-Vec churn." **Wrong.** The malloc shim shows
Kāra and C are identical on a K=10M run: 10,000,028 malloc / 10,000,014
free / 10,000,000 of them at exactly 60 bytes / same totals (±28 bytes
of startup noise). There is no allocator-path difference.

The real mechanism: **clang -O3 over-optimizes the *generator*
(`int_to_roman`)**. It rewrites the `while (n >= 1000) { push 'M'; n -=
1000; }` subtract-loops into Barrett-reduction division + a
`memset_pattern16` bulk-fill call — 4 such call sites in the C binary, 0
in Kāra (`otool -tV`). For the 1–12 byte fills typical of this kata
(mean Roman length ~9.5), `memset_pattern16`'s cross-module call
overhead (~3–5 ns) exceeds the inline stores it replaces. Kāra wins the
generator by lowering literally and *not* applying that transform.

**Implication — `memset_pattern16` recognition is declined, not
deferred.** Adding clang-style bulk-fill recognition to karac would
regress this shape. Empirical evidence that "match what clang does" is
not a sound perf goal; codegen choices must be validated on real output
sizes, not copied from a peer compiler.

## Parse-only diagnostic — C's parser is actually faster

With the generator + allocator removed (parse-only harness), the
`roman_to_int` kernel alone:

| Variant | Kāra | C |
|---|---|---|
| std lookahead | 84.8 ms | 78.5 ms |
| cached-next (value() once/char) | 66.9 ms | 58.7 ms |

C edges Kāra by ~8% on the parser kernel (and the cached-next algorithmic
variant helps C *more*: 1.34× vs 1.27×). So Kāra's lead on the *fused*
kata 13 wall comes entirely from the generator's clang-pessimization, not
from parser codegen. The cached-next variant is a real ~1.3× algorithmic
win but is language-neutral (not a karac advantage) — left as a noted
technique, not applied (kata educational source stays the textbook
forward-scan).

A single-input (non-alternating) parse loop exposed the **LICM-across-
pure-calls** gap: clang folded it to one multiply (1.9 ms); karac ran the
parse 10M times (99 ms). Deferred — see § match lowering's sibling entry
in the tracker.

## match lowering — `switch` vs `icmp`-chain

Tested a 7-way byte classifier (`value()` from kata 13) as both an
if-chain and a `match`, reading from a runtime `Vec[u8]` (to defeat
constant-folding). Both forms produce **byte-identical** machine code:
karac emits an `icmp eq + br` chain, and LLVM's `SimplifyCFG` rebuilds it
into a balanced binary-search tree. Wall-time delta 1.06× ± 0.12× —
sub-noise. The `switch` form would only diverge on *dense* fan-ins where
LLVM's chain-recognizer gives up. Deferred with that trigger; details in
[`phase-7-codegen.md`](../implementation_checklist/phase-7-codegen.md).

Note: byte-literal *patterns* (`match b { b'I' => ... }`) were a hard
parse error before `f1b5a46`; the integer form `73u8 => ...` was the
workaround. Fixed independently of the lowering question.

## Cross-refs

- Fixes: `f1b5a46` (range patterns + byte-literal patterns),
  `e56e8fc` (Vec[Vec[T]] borrow). Tests in `tests/codegen.rs`
  (`test_e2e_match_range_*`, `test_e2e_match_byte_*`,
  `test_e2e_vec_of_vec_index_ref_arg`) and `tests/parser.rs`
  (`test_byte_literal_in_match_pattern`, `test_range_pattern_byte`).
- README corrections: kara-katas `d0f9ae7` (kata 12 + 13 § Benchmarks).
- Deferred follow-ups: [`phase-7-codegen.md`](../implementation_checklist/phase-7-codegen.md)
  (`[->]` entries: match→switch, LICM-across-pure-calls).
