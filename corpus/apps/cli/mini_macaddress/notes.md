# mini_macaddress

A port of the `macaddress` library (2.0.2, MIT / SPDX 0BSD, Alexander
Kozhevnikov) — the `ipaddress`-style library for hardware identifiers such as
MAC addresses, used to parse, normalise and order EUI-48 / EUI-64 and related
identifiers.

The port follows the library class for class and function for function:

- **Parsing** — `macaddress.parse` tries a value against several address
  classes at once, using the library's exact char-by-char *candidate
  narrowing*: it collects every format string of the input's length, sorts
  them, and walks the input position by position, discarding formats whose
  separator or hex slot cannot match, until one class remains. The six classes
  — `OUI` (24-bit), `CDI32`, `CDI40`, `EUI48` (a.k.a. `MAC`), `EUI60`, `EUI64`
  — and all of their dash / colon / dotted / bare formats are reproduced.
- **Canonical string & repr** — `str` fills the first format string's `x`
  slots with upper-case hex nibbles; `repr` wraps it as `EUI48('01-23-45-…')`.
- **Integer construction** — building a class from an `int`, including the
  `is too big for`, `cannot be negative` and `cannot be an empty string`
  errors, with the library's exact message wording and class-name lists
  (`OUI, CDI32, CDI40, EUI48, EUI60, or EUI64`).
- **The `.oui` prefix** — the leading 24 bits of any `_StartsWithOUI` subclass,
  returned as an `OUI`.
- **Ordering** — the library's cross-class order: align two addresses to the
  wider size, compare the raw bit strings, then break ties by size. This
  intuitively groups an OUI just ahead of the MACs that start with it.

Addresses are carried as a vector of 4-bit **nibbles**, most-significant
first, of length `size/4`. Every class size (24, 32, 40, 48, 60, 64) is a
multiple of four, so the library's sub-nibble bit offset is always zero and a
nibble vector is a faithful model that never overflows `i64` — an EUI-64 is
sixteen nibbles, not `2^64`. All arithmetic is integer and string; **no float
is formed**.

**Jobs.** `data/jobs.tsv` holds one job per line, tab-separated: an operation
and its argument. `parse` takes a raw address string; `int` takes
`CLASS,DECIMAL`; `oui` takes `CLASS,STRING`; `cmp` takes two `CLASS=STRING`
operands separated by `|`; `sort` takes a comma-separated list of
`CLASS=STRING`. The set exercises every operation plus the error paths (an
empty string, an unparseable length, a bad hex digit, a too-big integer and a
negative integer).

**Oracle.** `expected.out` is the output of `oracle.py`, which runs the same
jobs through the copy of `macaddress` in `vendor/` (MIT / 0BSD, in
`LICENSE-macaddress.txt`). `mk.py` writes `data/jobs.tsv`; `gen_kara.py` embeds
the jobs as `jobs_text()`.

Written 2026-10-22 by the real-world apps thread. It compiled clean under
`karac check` and matched `expected.out` byte for byte on `__mir-run`,
`__mir-llvm-run`, `__mir-llvm-asan` and `build --mir` on the first run, with no
new compiler gaps. The port carries its own single-separator `split` helper
because the stdlib `split` / `lines` iterator surface was mid-change the day it
was written.
