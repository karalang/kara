# mini_fnv

A port of the core of [fnvhash](https://pypi.org/project/fnvhash/) 0.2.1
(MIT; Lorenz Schori), a pure-Python implementation of the
**Fowler/Noll/Vo (FNV)** non-cryptographic hash — the hash used all over the
place for hash tables, checksums and bloom-filter keys because it is tiny and
fast.

## What it does

`mk.py` writes `data/jobs.tsv`: one hash job per line, `variant<TAB>input`.
`variant` is one of `fnv0_32`, `fnv1_32`, `fnv1a_32`; `input` is ASCII text
whose UTF-8 bytes are hashed. The result is printed as 8 lowercase hex digits.

```
fnv1a_32    (empty)   -> 811c9dc5     # the FNV-1a 32-bit offset basis
fnv1a_32    a         -> e40c292c
fnv1_32     hello     -> b6fa7167
fnv0_32     a         -> 00000061
```

(The first three lines above are the canonical FNV test vectors.)

`oracle.py` replays every job through the vendored `fnvhash` library and writes
`expected.out`; `mini_fnv.kara` reproduces it byte-for-byte on every backend.

## How the port works

FNV is a two-step loop over the input bytes:

- **FNV-0 / FNV-1:** `hval = (hval * prime) % 2^32`, then `hval = hval ^ byte`.
- **FNV-1a:** the XOR comes first, then the multiply-modulo.

FNV-0 starts from `0`; FNV-1 and FNV-1a start from the offset basis
`0x811c9dc5`. The 32-bit prime is `0x01000193` (16777619).

Kāra has **no bitwise operators**, so the XOR is done with integer arithmetic:

- `xor8(a, b)` XORs two values in `[0, 255]` bit by bit, pulling each bit out
  with `/` and `% 2` and adding it back with its place value.
- Folding a byte into the accumulator (`hval ^ byte`, `byte < 256`) only ever
  changes the **low byte** of `hval`, so `xor_byte` splits off `hval % 256`,
  XORs that, and reassembles.

The 32-bit accumulator stays below `2^32`, and the prime is about `2^24`, so
`hval * prime` stays below `2^56` — comfortably inside Kāra's `i64` — before the
`% 2^32`. Hex formatting is a plain repeated `/ 16`, `% 16` with an
MSB-first reversal.

## Scope

- **32-bit variants only** (`fnv0_32`, `fnv1_32`, `fnv1a_32`). The 64-bit
  variants (`fnv1_64`, `fnv1a_64`, …) use a 64-bit accumulator and a `2^40`
  prime, whose products overflow a signed `i64`, and their offset basis
  (`0xcbf29ce484222325`) is itself larger than `i64::MAX`. They need a 64-bit
  unsigned / big-integer accumulator and are out of scope.
- **ASCII inputs.** Jobs carry ASCII text (no tab or newline, which delimit the
  job file); the hash itself is defined over raw bytes and the port hashes the
  input's UTF-8 bytes unchanged.

The port was checked against the library over the job list on every backend,
and separately (during development) over 40,000 random byte strings of length
0–24, plus an exhaustive check of the 8-bit XOR helper over all 65,536 byte
pairs.

The vendored library is pure Python and imports only the standard library.

## Running

```bash
karac run mini_fnv.kara               # interpret
karac build --mir mini_fnv.kara       # native binary
python3 -s -P -B mk.py                  # regenerate data/jobs.tsv
python3 -I oracle.py                     # regenerate expected.out
python3 -s -P -B gen_kara.py             # regenerate the embedded jobs
```
