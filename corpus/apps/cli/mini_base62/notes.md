# mini_base62

A port of [pybase62](https://github.com/suminb/base62) 0.6.0 (BSD-3-Clause;
Sumin Byeon), the Base62 codec used for short, URL-safe identifiers — the style
behind shortened links and compact database keys. Base62 writes a number in the
62 digits `0-9 A-Z a-z`: denser than hex, and URL-safe unlike Base64 (no `+`,
`/` or `=`).

## What it does

`mk.py` writes `data/jobs.tsv`: one tab-separated job per line, one of

- `e <n>` — encode a non-negative integer (default charset);
- `em <n> <minlen>` — encode, left-padded with the zero digit to `minlen`;
- `ei <n>` — encode using the inverted charset;
- `d <s>` / `di <s>` — decode (default / inverted charset), stripping a leading
  `0z` prefix, `ERR` on a character outside the charset;
- `eb <hex>` — `encodebytes` of the byte string given as hex;
- `db <s>` — `decodebytes` back to bytes (printed as hex).

`oracle.py` replays every job against the vendored `base62` library and writes
`expected.out`; `mini_base62.kara` reproduces it byte-for-byte on every backend.

## How the port works

`mini_base62.kara` follows `base62.py`. The integer path (`encode` / `decode`
with `minlen`, the default and inverted charsets, the `0z` prefix and the
invalid-character error) is plain i64 arithmetic, decoding by Horner so no
intermediate power overflows.

The bytes path is the interesting part. The library reads the byte string as one
big-endian Python integer — unbounded — and encodes that. To avoid a bignum,
this port carries the value as a **base-256 digit array** and runs schoolbook
arithmetic against base 62: `encodebytes` is repeated long division by 62
(each step emits one Base62 digit), and `decodebytes` is repeated
multiply-by-62-add-digit. A digest far wider than i64 therefore round-trips
exactly — verified against the library on values up to 80 bits and beyond.
`decodebytes` strips leading zero bytes, so the zero value decodes to the empty
byte string, matching the library.

## Scope

The plain integer `encode` / `decode` are i64-bounded, which is the documented
boundary of that path here; the `encodebytes` / `decodebytes` path has no such
bound (it is the digit-array arithmetic above). The library's `encodebytes`
also accepts a `readfunc`/`writefunc` streaming form over file objects, which is
an I/O-shape convenience over the same transform and out of scope. The vendored
`base62.py` is pure Python with no third-party imports, so `oracle.py` runs
exactly the code the port mirrors.

## Running

```bash
karac run mini_base62.kara                  # interpret
karac build --mir mini_base62.kara          # native binary
python3 -s -P -B mk.py                       # regenerate data/jobs.tsv
python3 -I oracle.py                          # regenerate expected.out
python3 -s -P -B gen_kara.py                  # regenerate the embedded jobs
```
