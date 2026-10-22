# mini_leb128

A port of [leb128](https://github.com/mohanson/leb128) 1.0.9 (MIT), the
**LEB128** (Little-Endian Base 128) variable-length integer codec. LEB128
stores an integer in as few 7-bit groups as its magnitude needs; every byte
carries its seven low bits plus a high *continuation* bit that is set on all but
the last. It is how the [DWARF](https://dwarfstd.org/) debug format and the
[WebAssembly](https://webassembly.github.io/spec/core/binary/values.html#integers)
binary format encode every integer literal, and its unsigned half is exactly
Protocol Buffers' *varint*. The library — and this port — provides both the
unsigned codec (`u`) and the signed, two's-complement one (`i`).

## What it does

`mk.py` writes `data/jobs.tsv`: one job per line (tab-separated), one of
`ue <dec>` / `se <dec>` (unsigned / signed encode of a decimal value) or
`ud <hex>` / `sd <hex>` (unsigned / signed decode of hex bytes). The decode jobs
carry encodings produced by `mk.py`'s own pure encoders; `oracle.py` replays
every job against the vendored `leb128` library and writes `expected.out` — the
encoded bytes as lowercase hex, or the decoded decimal integer.
`mini_leb128.kara` reproduces it byte-for-byte on every backend.

The values exercise the group boundaries (0, 63/64, 127/128, 16383/16384,
2097151/2097152 — each the point where one more byte is needed) and, for the
signed codec, the sign-bit cases that make it differ from the unsigned one
(`-1`, `-64`, `-65`, `-128`, `-129`, and larger negatives), so both the
encoder's two-sided stop test and the decoder's sign extension are covered.

## How the port works

`mini_leb128.kara` follows `leb128/__init__.py` line for line. Both encoders
peel seven bits at a time (`b = x & 0x7f; x = x >> 7`) and append `0x80 | b`
until a stop test holds: unsigned stops when the rest is `0`; signed stops when
the rest is `0` with bit 6 of the group clear, or `-1` with bit 6 set. Both
decoders sum `(byte & 0x7f) << (i*7)`; the signed one then ORs in
`-(1 << (len*7))` when the last byte's bit 6 is set, extending the sign. This is
exact in i64 because Kāra's `>>` on a negative i64 is arithmetic and its `&`/`|`
act on the two's-complement bits, matching Python's semantics (verified against
the library for every value here, negatives included).

## Scope

The library uses Python's arbitrary-precision integers; this port is i64, so
values outside the signed 64-bit range — and the library's `decode_reader`
streaming helper, which needs real byte I/O — are the documented boundary. The
vendored `leb128/` package is pure Python importing only `typing`, so
`oracle.py` runs exactly the code the port mirrors.

## Running

```bash
karac run mini_leb128.kara                     # interpret
karac build --mir mini_leb128.kara             # native binary
python3 -s -P -B mk.py                              # regenerate data/jobs.tsv
python3 -I oracle.py                                 # regenerate expected.out
python3 -s -P -B gen_kara.py                         # regenerate the embedded jobs
```
