# mini_base91

A port of [base91](https://pypi.org/project/base91/) 1.0.1 (New BSD; Adrien
Beraud, Guillaume Jacquenot), the **basE91** binary-to-text encoding. basE91
packs binary into a 91-character printable alphabet, denser than Base64 — it
averages about 1.23× the input size against Base64's 1.33× — at the cost of a
variable 13-or-14-bit group size. That density is why it appears where payload
size matters over a text-only channel (QR-ish transports, compact tokens).

## What it does

`mk.py` writes `data/jobs.tsv`: one tab-separated job per line, either

- `e <hex>` — encode the bytes given as hex into a basE91 string;
- `d <str>` — decode a basE91 string back to bytes (printed as hex).

`oracle.py` replays each against the vendored `base91` library and writes
`expected.out`; `mini_base91.kara` reproduces it byte-for-byte on every backend.

## How the port works

`mini_base91.kara` follows `base91/__init__.py` exactly. The encoder keeps a bit
accumulator `b` with `n` valid bits: each input byte is shifted in, and once
`n > 13` it emits one group — the low 13 bits when they exceed 88, otherwise the
low 14 bits — as two alphabet characters (the low then the high digit of a
base-91 pair). Leftover bits are flushed with the library's tail rule (`n > 7 or
b > 90` decides whether a second character is written). The decoder reverses it,
accumulating pairs and draining whole bytes, and — like the library — silently
skips any character outside the alphabet (so surrounding whitespace is ignored).

The accumulator never exceeds roughly 21 bits, so the whole codec runs in plain
`i64` arithmetic with no big integers.

## Scope

The full transform is covered for arbitrary byte strings; there is no documented
boundary beyond the library's own surface (`encode` / `decode`). The vendored
`base91.py` uses only `struct` from the standard library, so `oracle.py` runs
exactly the code the port mirrors.

## Running

```bash
karac run mini_base91.kara                   # interpret
karac build --mir mini_base91.kara           # native binary
python3 -s -P -B mk.py                        # regenerate data/jobs.tsv
python3 -I oracle.py                           # regenerate expected.out
python3 -s -P -B gen_kara.py                   # regenerate the embedded jobs
```
