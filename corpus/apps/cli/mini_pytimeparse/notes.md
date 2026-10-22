# mini_pytimeparse

The parse core of `pytimeparse` 1.1.10's `timeparse` (MIT) — the small
library Ansible, Fabric and a long tail of ops/config tooling use to turn a
human-written duration like `32m`, `2h13m`, `4:13:02` or `2 days, 4:13:02`
into a number of seconds.

The port follows `pytimeparse/timeparse.py` directly:

- A leading sign (`[+-]`, with surrounding whitespace) is stripped first and
  applied to the final result.
- The remainder is tried against **five time formats, in order**, and the
  first that fully matches with a non-empty body wins:
  1. **unit words** — optional weeks / days / hours / mins / secs, each a
     `[\d.]+` value and a unit word (`w`/`wk`/`wks`/`week`/`weeks`, and the
     same shape for days, hours — `h`/`hr`/`hrs`/`hour`/`hours` — minutes and
     seconds), with optional `,` or `/` separators;
  2. **`MM:SS`** (`\d{1,2}:\d{2}(.\d+)?`);
  3. optional weeks/days then **`HH:MM:SS`**;
  4. **`D:HH:MM:SS`** (the day separator may be `:` or `-`);
  5. **`:SS`**.
- The unit words are matched case-insensitively, and the whole match is
  anchored, exactly as the library's compiled patterns are.

The result's **type** follows the library: an `int` when every component is a
whole number; an `int` with the fractional non-seconds components summed
exactly and then truncated (the library sums with `Decimal` so that `4.1m`
isn't a second short) when only the non-seconds parts are fractional; and a
`float` when the seconds component itself is fractional. The
`granularity="minutes"` reinterpretation of an ambiguous single-colon `M:S` as
`H:M` is reproduced, and a component that looks numeric but isn't (`1.2.3`)
makes that format fall through to the next, as the library's
`except (ValueError, InvalidOperation)` does.

**Exact arithmetic.** Rather than use floating point (which would not
reproduce CPython's `Decimal` truncation or its `float` repr), every
component is parsed into an exact scaled integer `value * 10^p` where `p` is
the maximum number of fractional digits in the components being summed. The
integer branch divides back down (a floor, matching `int(Decimal)` for the
non-negative sums here); the float branch renders the scaled sum as a decimal
string with trailing zeros trimmed. For the curated job set this is
byte-identical to the library's output.

**Jobs.** `data/jobs.tsv` holds one job per line, tab-separated: the duration
string and the granularity (`seconds` or `minutes`). The set covers every one
of the five formats, signed input, the unit-word spellings and `,`/`/`
separators, decimal components, all three output branches, the
granularity=minutes reinterpretation, the malformed-number fall-through, and
the no-match (`None`) cases.

**Oracle.** `expected.out` is the output of `oracle.py`, which runs the same
jobs through the copy of `pytimeparse` in `vendor/pytimeparse/` (MIT, in
`LICENSE-pytimeparse.txt`). `mk.py` writes `data/jobs.tsv`; `gen_kara.py`
embeds it into the Kāra source as `jobs_text()`.

Written 2026-10-22 by the real-world apps thread. It compiled clean under
`karac check` and matched `expected.out` byte for byte on `__mir-run`,
`__mir-llvm-run`, `__mir-llvm-asan` and `build --mir` on the first run, with
no new compiler gaps.
