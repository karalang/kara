# mini_lzw

A port of the [lzw3](https://pypi.org/project/lzw3/) library (0.4; MIT, Stefano
Dottore): **LZW compression**, the dictionary coder behind Unix `compress`
(`.Z`), **GIF**, **TIFF** and the **PDF `LZWDecode`** filter. For each input the
program LZW-compresses the bytes to lzw3's variable-width code stream and
decompresses the result back.

## What it does

`mk.py` writes `data/jobs.tsv`: one lowercase hex string per line, the raw input
bytes for a job. For each line the job compresses the bytes and decompresses the
result back, printing two lines:

```
enc <hex of the compressed bytes>
dec <hex of the decompressed bytes>     (equals the input)
```

For example `aaaa` (hex `61616161`) compresses to `30c04c3000` and decompresses
back to `61616161`.

`oracle.py` replays every line through the vendored `lzw3` library and writes
`expected.out`; `mini_lzw.kara` reproduces it byte-for-byte on every backend.

## How the port works

LZW builds a dictionary of byte sequences as it reads: sequences `0..255` are the
single bytes, `256` is the end-of-stream marker, and every new sequence is a
known one plus one more byte. Each emitted code is written **most-significant-bit
first** with a width that **grows as the dictionary fills** (9 bits, then 10, 11,
…). Three details make this port byte-identical to the library:

- **The width recurrence, with lzw3's exact asymmetry.** Both sides widen the
  code when a new sequence number sets a fresh high bit (`bitcount += seq >>
  bitcount`), but the **compressor widens AFTER assigning** a sequence number
  and the **decompressor widens BEFORE** — "the decompressor switches one step
  before the compressor". The port reproduces each order exactly. A shift `n >>
  k` is the integer `n / pow2(k)` — Kāra has no bitwise operators.
- **The final flush byte.** lzw3's bit writer flushes whole bytes as they fill,
  then `close()` **always** writes one more byte holding the leftover bits
  right-padded — which is a `00` byte when the stream happens to be exactly
  byte-aligned. So the output is `floor(total_bits / 8) + 1` bytes, not
  `ceil(...)`; the port matches this (it is what distinguishes the two on a long
  input whose code stream lands on a byte boundary).
- **The KwKwK special case** on decode: when a code equals the not-yet-defined
  next sequence number, its expansion is the previous sequence plus its own
  first byte. The port handles it as the library does.

Everything is integer arithmetic over `Vec[i64]`, so the four backends agree by
construction.

## Scope

- **The core `compress` / `decompress` path** over byte strings (lzw3's own
  `.Z`-style raw code stream — it carries no Unix-`compress` magic header or
  block-mode flag). The file-walking CLI, permission copying and size heuristics
  are not ported.
- Inputs are **non-empty** (the format is not meaningful for an empty stream).

The port was checked against the library on every backend over the job list
(which includes small-alphabet runs long enough to cross the 9→10→11-bit width
transitions), and separately during development over thousands of random
round-trips.

The vendored library is pure Python and imports only the standard library. It
predates Python 3.10's move of `collections.Callable` to `collections.abc`, so
`oracle.py` restores that one alias before importing it — a stdlib name
relocation, not a behaviour change; `vendor/` is left untouched.

## Running

```bash
karac run mini_lzw.kara             # interpret
karac build --mir mini_lzw.kara     # native binary
python3 -s -P -B mk.py                 # regenerate data/jobs.tsv
python3 -I oracle.py                    # regenerate expected.out
python3 -s -P -B gen_kara.py            # regenerate the embedded jobs
```
