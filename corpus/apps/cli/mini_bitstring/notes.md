# mini_bitstring

A port of the core of [bitstring](https://github.com/scott-griffiths/bitstring)
3.1.9 (MIT, Scott Griffiths), the pure-Python library for constructing,
slicing and interpreting bitstrings. It is the tool reached for whenever bits
do not line up on byte boundaries — binary file formats, network protocol
headers, DSP and bit-packed data.

## What it does

`mk.py` writes `data/jobs.tsv`, one operation per line. `oracle.py` runs each
job through the vendored library (`vendor/bitstring.py`) to produce
`expected.out`. `mini_bitstring.kara` reimplements the same operations and must
match that output byte-for-byte on every backend.

A bitstring is modelled as a `Vec[i64]` of bits, most-significant first, each
`0` or `1` — the same whole-value vector trick `mini_macaddress` uses for
nibbles. Every interpretation is exact integer arithmetic (no bitwise
operators, no float), so nothing depends on `i64` width.

## Scope

- The `BitArray` auto-init grammar: `0x…`, `0b…`, `0o…`, `uint:N=V`,
  `int:N=V`, comma-joined token lists, and surrounding whitespace.
- Interpretations: `bin`, `hex` (requires a multiple of 4 bits), `oct`
  (multiple of 3), `uint`, `int` (two's complement), `len`, and the `str`
  representation.
- Slicing `[a:b]` and indexing `[i]` with Python semantics (negative indices,
  clamping, `a>b` → empty).
- `concat` (`+`), equality, `count(bit)`, `find` (first match, as a tuple),
  `findall` (overlapping), `reverse`, `invert`, `tobytes` (right-padded),
  and `unpack` of a `uint`/`int` format list.
- The library's exact error texts for the well-defined failure paths: bad
  hex/bin/oct symbols, unsigned/signed overflow with their stated ranges,
  negative unsigned, zero-length integer interpretation, the hex/oct
  "not a multiple" messages, out-of-range indexing, and reading off the end
  in `unpack`.

Integer interpretation is limited to bitstrings of at most 63 bits (the token
lengths the jobs use). The library's token-dispatch error family ("Don't
understand length …") and non-integer token kinds are out of scope.

## Running

```bash
karac run mini_bitstring.kara          # interpret
karac build --mir mini_bitstring.kara  # native binary
python3 -s -P -B mk.py                 # regenerate data/jobs.tsv
python3 -I oracle.py                   # regenerate expected.out
```
