# mini_textdistance

A port of the integer edit- and sequence-distance algorithms of the
`textdistance` library (4.6.3, MIT, @orsinium) — the library behind fuzzy
string matching, deduplication and record linkage. The five metrics are run
through textdistance's own pure-Python implementations with `external=False`,
so each result is the library's deterministic integer, never a side C library.

For every string pair the port reports:

- **Hamming distance** — differing positions, counting each position past the
  shorter string as a difference (textdistance pads the shorter sequence under
  `zip_longest`), so it is defined for unequal lengths.
- **Levenshtein distance** — the minimum deletions, insertions and
  substitutions, by the standard row-rolling dynamic program.
- **Damerau-Levenshtein distance** — the *restricted* (optimal string
  alignment) variant, adding adjacent transposition, exactly as
  textdistance's `_pure_python_restricted`.
- **Longest-common-subsequence length** — `LCSSeq.similarity`.
- **Longest-common-substring length** — `LCSStr.similarity` (its length is the
  same whichever matching block difflib's `find_longest_match` picks).

Strings are compared as byte sequences; inputs are ASCII, where one byte is
one character exactly as textdistance compares `str` by code point. Every
matrix is a flat `Vec[i64]` indexed `i*cols + j`, so **no float and no nested
vector is formed**.

**Scope.** This is the integer-returning core. textdistance's float metrics
(Jaro-Winkler, the normalized ratios, token and compression distances) and the
q-gram (`qval`) and word (`qval=None`) tokenizations are out of scope.

**Jobs.** `data/jobs.tsv` holds one pair per line (`pair` and two strings
joined by `|`). The set covers identical and empty strings, the transposition
case where Damerau beats Levenshtein (`ba`/`ab`), equal-length Hamming cases
and classic subsequence/substring examples.

**Oracle.** `expected.out` is the output of `oracle.py`, which runs the same
pairs through the copy of `textdistance` in `vendor/` (MIT, in
`LICENSE-textdistance.txt`). `mk.py` writes `data/jobs.tsv`; `gen_kara.py`
embeds the jobs as `jobs_text()`.

Written 2026-10-22 by the real-world apps thread. It compiled clean under
`karac check` and matched `expected.out` byte for byte on `__mir-run`,
`__mir-llvm-run`, `__mir-llvm-asan` and `build --mir` on the first run, with no
new compiler gaps. The port carries its own single-separator `split` helper
because the stdlib `split`/`lines` iterator surface was mid-change the day it
was written.
