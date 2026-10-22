# mini_ulid

The codec core of `python-ulid` 4.0.1 (MIT, Martin Domke) — the library that
reads and writes ULIDs, the 128-bit universally-unique, lexicographically
sortable identifiers used as database keys and request ids in place of UUIDs.

The port follows the library function for function:

- **Crockford base32** — `base32.encode` / `decode` with the exact bit-packing
  of the timestamp (48 bits → 10 chars) and randomness (80 bits → 16 chars), the
  alphabet-membership check, and the three `ValueError`s (wrong length, a
  character outside the alphabet, and a leading char above `7` that would
  overflow 128 bits).
- **`from_*` constructors** — `from_str`, `from_hex`, `from_int`, `from_uuid`
  and `from_uuid7`, each validating length and rebuilding the 16 bytes.
- **`to_*` / accessors** — `__int__`, `hex`, `milliseconds`, `datetime`
  (ISO-8601 UTC), `to_uuid`, `to_uuid4` and `to_uuid7` in both the default
  (round-tripping) and `compliant=True` forms, the latter laying out the
  version-7 and variant bits and the 12-bit `rand_a` field exactly as the
  library does.
- **`parse`** — the dispatch that chooses `from_uuid` / `from_hex` / `from_str`
  by string length (36 / 32 / 26) and `from_int` for a 37-digit integer, with
  the `Cannot parse ULID from string of length N` error for anything else.
- **ordering** — `__lt__` / `__eq__` and `sorted()`, which the library defines
  over the raw bytes; because Crockford base32 is order-preserving and the
  string is fixed-length, this is the lexicographic order of the string form.

The ULID integer is 128 bits, which does not fit a machine word, so the port
carries it as its 16 bytes and does its own base-256 ↔ base-10 conversion for
`__int__` and `from_int` (including the `int too big to convert` overflow). The
`datetime` is computed from the millisecond timestamp with the proleptic
Gregorian ordinal, so **no float is ever formed**; the job set keeps `datetime`
to whole-second timestamps, where the library's `float` path and this integer
one agree exactly.

**Jobs.** `data/jobs.tsv` holds one job per line, tab-separated: an operation
and its argument. The arguments are minted from fixed `(timestamp, randomness)`
pairs through the vendored library, so each is a genuine canonical ULID, hex,
integer or uuid form. The set spans the codec, every conversion, the `parse`
dispatch, ordering and the error paths (bad length, bad character, 128-bit
overflow and the integer overflow).

**Oracle.** `expected.out` is the output of `oracle.py`, which runs the same
jobs through the copy of `python-ulid` in `vendor/ulid/` (MIT, in
`LICENSE-python_ulid.txt`; the vendored `__init__.py` has only its
`importlib.metadata` version lookup replaced by the literal `"4.0.1"`, since the
package is not installed). `mk.py` writes `data/jobs.tsv`; `gen_kara.py` embeds
the jobs as `jobs_text()`.

Written 2026-10-22 by the real-world apps thread. It compiled clean under
`karac check` and matched `expected.out` byte for byte on `__mir-run`,
`__mir-llvm-run`, `__mir-llvm-asan` and `build --mir` on the first run, with no
new compiler gaps.
