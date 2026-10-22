# mini_pysemver

A port of python-semver 3.1.0 (BSD, Konstantine Rybnikov) — the semantic
version library used across release automation, dependency tooling and the
`pysemver` command line.

The port follows `semver.Version` method for method:

- **`parse` / `is_valid`** — the strict semver.org grammar: `major.minor.patch`
  with no leading zeros, an optional `-prerelease` of dot-separated identifiers
  (numeric with no leading zero, or alphanumeric-with-hyphen), and an optional
  `+build` of free alphanumeric-hyphen identifiers. Every rejection raises the
  library's `"<v> is not valid SemVer string"`.
- **`compare`** — the §11 precedence order: compare major/minor/patch, then the
  prerelease by the library's natural comparison (numeric identifiers rank
  below alphanumeric ones, a shorter identifier list ranks below a longer one),
  and a version with a prerelease ranks below the same version without one.
  Build metadata is ignored. The classic
  `alpha < alpha.1 < alpha.beta < beta < beta.2 < beta.11 < rc.1 < (release)`
  chain is reproduced exactly.
- **`match`** — an `<op><version>` expression with `<`, `>`, `<=`, `>=`, `==`,
  `!=`, or a bare version (`==`), with the library's exact error for a bad
  operator.
- **`bump_major` / `minor` / `patch` / `prerelease` / `build`** — including the
  `_increment_prerelease` (bump a trailing dot-number else append `.0`) and
  `_increment_string` (bump the last digit run, else append `.0`) helpers.
- **`next_version`** — natural-order next version for a part, including the
  "prerelease present, so a patch/minor/major bump just finalizes" rule and the
  `rc`-token replacement, with the exact `Invalid part` error.
- **`finalize_version`** — drop prerelease and build.

A version is three integers plus a prerelease and a build string (an empty
string stands for Python's `None`). All integer and string work; **no float is
formed**.

**Jobs.** `data/jobs.tsv` holds one operation per line: `parse`, `valid`,
`cmp A|B`, `match V|EXPR`, `bump WHICH|V`, `next PART|V` and `finalize`. The set
covers the valid grammar, every rejection the grammar makes, the full
precedence chain, each bump and `next_version` branch, and the operator and
invalid-part errors.

**Oracle.** `expected.out` is the output of `oracle.py`, which runs the same
jobs through the copy of `semver` in `vendor/` (BSD, in `LICENSE-semver.txt`).
`mk.py` writes `data/jobs.tsv`; `gen_kara.py` embeds the jobs as `jobs_text()`.

Written 2026-10-22 by the real-world apps thread. It compiled clean under
`karac check` and matched `expected.out` byte for byte on `__mir-run`,
`__mir-llvm-run`, `__mir-llvm-asan` and `build --mir` on the first run, with no
new compiler gaps. The port carries its own single-separator `split` helper
because the stdlib `split`/`lines` iterator surface was mid-change the day it
was written.
