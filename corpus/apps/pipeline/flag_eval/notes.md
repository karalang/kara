# pipeline/flag_eval

A feature-flag evaluator as a service would embed it: flag definitions in a
JSON file (an internally tagged `Rule` enum, `#[serde(rename)]`,
`#[serde(default)]`, `Option` fields), evaluation requests as JSON lines with
a `SortedMap` of attributes, one JSON-line decision per request with its
reason, a stable FNV-1a rollout bucket over `u64` with `wrapping_mul`. A bad
config reload is refused and the old config kept; a second config version is
evaluated over the same requests and the changed decisions are listed. The
mirror `flag_eval.py` wrote `expected.out`.

Written 2026-10-14 by the real-world apps thread; karac at c8dad803.

## Edits from source.orig.kara to source.kara

No author errors under the v2 rules. Compiler gaps, worked around and listed
in the source header: B-2026-10-12-6 (`parse` returns `Option`),
B-2026-10-12-7 (`lines` is an eager `Vec[String]`), B-2026-10-14-44
(`fs.read_to_string` / `fs.write` take `ref String`).

Not worked around: B-2026-10-12-23, so the entry FAILS (refused by
`karac check`) on every mir backend. Without check, `__mir-run`,
`__mir-llvm-run`, `__mir-llvm-asan` and `karac build --mir` print
`expected.out` exactly (measured).
