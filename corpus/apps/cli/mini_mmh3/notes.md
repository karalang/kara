# mini_mmh3

A port of the x86 MurmurHash3 functions of
[pymmh3](https://github.com/wc-duck/pymmh3) 0.0.5 (CC0, public domain) — a
pure-Python MurmurHash3 that reproduces the canonical
[mmh3](https://github.com/hajimes/mmh3) C extension exactly. MurmurHash3 is the
fast non-cryptographic hash behind Bloom filters, count-min sketches, feature
hashing, and shard / partition keys across databases and data pipelines.

## What it does

`mk.py` writes `data/jobs.tsv`, one job per line:

    <op> \t <seed> \t <text>

- `h32  <seed> <text>` → MurmurHash3 x86_32 as a **signed 32-bit decimal** (the
  value `mmh3.hash` returns);
- `h128 <seed> <text>` → MurmurHash3 **x86 128-bit** as 32 lowercase hex digits
  (big-endian over the h4,h3,h2,h1 lanes).

`oracle.py` hashes each job through the vendored `pymmh3`; `mini_mmh3.kara`
reproduces `expected.out` byte-for-byte on every backend. The inputs walk every
tail size the two algorithms branch on — lengths 0..17 and 31..33, so the
length mod 4 (for h32) and mod 16 (for h128) each take every value — across
three seeds, including `0x9747b28c` from the SMHasher test vectors.

## How the port works

Both the 32-bit hash and the x86 128-bit hash use only 32-bit arithmetic: a
read of four little-endian bytes per lane, a fixed sequence of 32-bit
multiplies by the Murmur constants, `ROTL32` rotations, xors, and the `fmix32`
avalanche. The port follows pymmh3 statement for statement.

The one obstacle is that Kāra checks integer overflow by default, and a
32-bit × 32-bit product is up to 64 bits — it overflows `i64`. `mul32(a, b)`
takes the product modulo 2³² without overflowing by splitting the second
operand into 16-bit halves: `a * (b & 0xFFFF)` and `a * (b >> 16)` are each at
most about 2.8 × 10¹⁴, well inside `i64`, and recombine as
`(a*lo + ((a*hi) & 0xFFFF) * 2¹⁶) mod 2³²`. Every rotation, shift and `|`-based
byte assembly stays below 2³², so no other step can overflow either. The
signed 32-bit result of `h32` is the unsigned hash minus 2³² when its top bit
is set; the 128-bit result is rendered as four 8-digit hex lanes.

## Scope

The x86 variants — `hash` (x86_32) and `hash128(..., x64arch=False)` — which
are exactly the paths that stay within 32-bit arithmetic. The **x64 128-bit
variant** (`hash128` with the default `x64arch=True`, and the `hash64` /
`hash_bytes` built on it) needs 64-bit × 64-bit multiplies modulo 2⁶⁴ and is
out of scope. Inputs are byte strings (UTF-8 of the ASCII text here); the
seed is an unsigned 32-bit value.

The vendored `pymmh3/` is pure Python and self-contained. Its x86 outputs were
cross-checked against the `mmh3` C extension for every input in this kata (and
match); `pip install mmh3` if you want to re-verify.

## Running

```bash
karac run mini_mmh3.kara                      # interpret
karac build --mir mini_mmh3.kara              # native binary
python3 -s -P -B mk.py                            # regenerate data/jobs.tsv
python3 -I oracle.py                               # regenerate expected.out
python3 -s -P -B gen_kara.py                       # regenerate the embedded jobs
```
