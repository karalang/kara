# mini_cobs

A port of the byte-stuffing codecs of
[cobs](https://github.com/cmcqueen/cobs-python) 1.2.2 (MIT, Craig McQueen).
**Consistent Overhead Byte Stuffing (COBS)** rewrites an arbitrary byte string
so that it contains no zero bytes, adding a small and predictable overhead. A
zero byte can then be used as an unambiguous frame delimiter — the standard
trick for packetising a byte stream over a serial line, USB CDC, or a raw
socket. **COBS/R ("Reduced")** is the same framing that usually saves the one
trailing overhead byte, by folding the final data byte into the length code when
it is large enough.

## What it does

`mk.py` writes `data/jobs.tsv`, one job per line:

    cobs_enc  / cobsr_enc  \t <hex payload>
    cobs_dec  / cobsr_dec  \t <hex frame>

- `cobs_enc <hex>` / `cobsr_enc <hex>` → the encoded frame as hex;
- `cobs_dec <hex>` / `cobsr_dec <hex>` → `ok <hex>` (the decoded payload) or
  `err <message>` when the frame is malformed.

`oracle.py` runs the jobs through the vendored `cobs` / `cobsr`;
`mini_cobs.kara` reproduces `expected.out` byte-for-byte on every backend. The
`dec` inputs are the frames the vendored encoder produced (so each round-trips),
plus a handful of deliberately broken frames — an embedded zero byte, a length
code that runs past the end of the frame — that exercise the two error paths.

## How the port works

Both encoders and both decoders follow the vendored pure-Python implementation
statement for statement. The payloads deliberately walk the length-code
boundaries each codec branches on: a run of `0xFD` non-zero bytes forces COBS to
emit an `0xFF` continuation code, and a run of `0xFE` does the same for COBS/R;
embedded and trailing zeros, the empty string, and a trailing high byte (for the
COBS/R reduction) are all covered.

- **encode** walks the input, emitting a length code for each zero-terminated
  run and splitting any run that reaches the boundary length. COBS/R adds the
  final-byte reduction: if the last data byte is `≥` the final length code, it
  becomes the length code and is dropped from the data.
- **decode** walks the length codes, copying each run and reinserting the zero a
  non-maximal code implies. A zero byte inside the frame, or a length code that
  points past the end, raises the library's `DecodeError`; the port returns
  `err` with the exact message string (`"zero byte found in input"` /
  `"not enough input bytes for length code"`). COBS/R decode treats an
  overrunning final length code as the reduced final byte instead of an error,
  mirroring the library.

## Scope

The byte codecs themselves, over the full byte range. The library's incremental
/ chunked encoder-decoder wrappers and its `str`-input `TypeError`s are out of
scope. The vendored `cobs` package is pure Python and self-contained (its
optional C extension is not built, so the library itself falls back to exactly
the pure-Python code ported here).

## Running

```bash
karac run mini_cobs.kara                      # interpret
karac build --mir mini_cobs.kara              # native binary
python3 -s -P -B mk.py                            # regenerate data/jobs.tsv
python3 -I oracle.py                               # regenerate expected.out
python3 -s -P -B gen_kara.py                       # regenerate the embedded jobs
```
