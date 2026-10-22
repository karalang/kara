# mini_pep440

The PEP 440 core of Python's `packaging` library — the rules every `pip`
install leans on to decide what a version string means and how two of them
order. `Version` parses a release string into its parts, normalizes the
dozen spellings the standard allows into one canonical form, compares and
sorts them, and `canonicalize_name` / `canonicalize_version` give the names
and versions their one true shape. This is the machinery that knows
`1.0alpha1` and `1.0-a.1` are both `1.0a1`, that a pre-release sorts below
its release, and that `01.002.03` is just `1.2.3`.

The port follows `packaging`'s own parser and comparison key exactly. It
reproduces the normalization table (`alpha`→`a`, `beta`→`b`,
`c`/`pre`/`preview`→`rc`, `rev`/`r`→`post`), the implicit-post shorthand
(`1.0-1` is `1.0.post1`), the dev/pre/post/local grammar, and the flat
six-field comparison key — including the two subtleties that trip hand-rolled
version code: a bare pre-release segment sorts *below* the plain release
(`1.0a1` < `1.0`) only because a missing pre-release ranks highest, and a
local version's segments compare string-against-int with numeric segments
ordering above alphabetic ones. Trailing-zero release segments are trimmed
for equality so `1.0` equals `1.0.0`.

**Jobs.** `data/jobs.tsv` holds 77 jobs, one per line, tab-separated, the
kind first:

- `parse <v>` — round-trips `str(Version(v))`, or prints the
  `InvalidVersion` message for a string the grammar rejects;
- `props <v>` — every attribute of a `Version` (epoch, release, pre, post,
  dev, local, public, base, major/minor/micro, and the three `is_*` flags);
- `cmp <a> <b>` — the six comparison operators on two versions;
- `sort <v,v,...>` — a comma-separated list sorted ascending, as strings;
- `cname <name>` — `canonicalize_name`;
- `cver <v> <0|1>` — `canonicalize_version`, the flag being
  `strip_trailing_zero`.

Each job prints `== ` and its fields joined by ` | `, then two spaces and
the result.

**Oracle.** `expected.out` is the output of `oracle.py`, which runs the same
jobs through the copy of `packaging` 26.3 in `vendor/packaging/` (dual
Apache-2.0 / BSD-2-Clause, in `LICENSE-packaging-APACHE.txt` and
`LICENSE-packaging-BSD.txt`). `mk.py` writes `data/jobs.tsv`; `gen_kara.py`
embeds it into the Kāra source as `jobs_text()`.

`packaging`'s version specifiers (`SpecifierSet`, `~=`, `>=` interval
matching) are a separate, larger surface built on a new interval module in
26.3 and are left to a future app; this one is the self-contained `Version`
and canonicalization core.

Written 2026-10-22 by the real-world apps thread. It compiled clean under
`karac check` and matched `expected.out` byte for byte on `__mir-run`,
`__mir-llvm-run`, `__mir-llvm-asan` and `build --mir` on the first run, with
no new compiler gaps. karac at 502e55cb69.
