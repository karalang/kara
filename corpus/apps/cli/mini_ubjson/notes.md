# mini_ubjson

A port of [py-ubjson](https://github.com/Iotic-Labs/py-ubjson) 0.16.1
(Apache-2.0, Iotic Labs) — the [Universal Binary JSON](https://ubjson.org/)
codec, a compact typed binary encoding of JSON data for wire formats and
storage that want JSON's model without its text overhead. `dumpb(value)`
encodes to bytes and `loadb(bytes)` decodes back.

## What it does

`mk.py` writes `data/jobs.tsv`, one job per line:

    <op> \t <payload>

- `enc <json>` → the UBJSON wire bytes for the JSON value, as lowercase hex
  (what `dumpb(value).hex()` prints);
- `dec <hex>` → the JSON value a UBJSON byte string decodes to (`loadb`),
  serialized as canonical JSON.

Every value is emitted as both an `enc` (from JSON) and a `dec` (from the hex
the library produced), so each one is exercised in both directions. `oracle.py`
runs the jobs through the vendored `py-ubjson`; `mini_ubjson.kara` reproduces
`expected.out` byte-for-byte on every backend.

## How the port works

A JSON value lives in a flat **arena** of nodes indexed by `i64` (tag `0` null,
`1` bool, `2` int, `3` str, `4` list, `5` dict). The encoder reproduces
py-ubjson's default options — no container counts, no float32, keys in
insertion order:

- `Z` null, `T` true, `F` false;
- integers take the smallest width on an unsigned-first ladder: `U` uint8 for
  `0..255`, `i` int8 for `-128..-1`, then `I` int16, `l` int32, `L` int64, each
  big-endian two's complement;
- a one-byte string is a `C` CHAR; any other string is `S` + a typed length +
  the UTF-8 bytes;
- arrays are `[ … ]` and objects are `{ (typed-length key, value)* }`, both
  terminator-delimited.

The decoder is the exact inverse: it dispatches on each marker and sign-extends
the fixed-width integers. Byte extraction uses Kāra's `>>` and `&` — the shift
is arithmetic, so a negative two's-complement value masks correctly at any
width (`(v >> 8*k) & 255`).

## Scope

The JSON value types. py-ubjson's floats (`d`/`D`, which need IEEE-754 bit
layout), high-precision numbers (`H`), the strongly-typed counted containers
(`$`/`#`), the no-op `N` and raw byte arrays are out of scope. Job strings use
ASCII text and avoid JSON string escapes. The vendored `ubjson/` package is
pure-Python with no third-party dependencies.

## Running

```bash
karac run mini_ubjson.kara                  # interpret
karac build --mir mini_ubjson.kara          # native binary
python3 -s -P -B mk.py                        # regenerate data/jobs.tsv
python3 -I oracle.py                           # regenerate expected.out
python3 -s -P -B gen_kara.py                   # regenerate the embedded jobs block
```
