# mini_bitstruct

A port of the pure-Python core of [bitstruct](https://pypi.org/project/bitstruct/)
8.23.0 (MIT; Erik Moqvist), the `struct`-for-bits library: pack and unpack
values to and from arbitrary **bit** widths rather than whole bytes. It is used
wherever a wire or file format packs fields that do not fall on byte
boundaries — radio and CAN frames, binary protocols, compact on-disk records.

## What it does

`mk.py` writes `data/jobs.tsv`: one operation per line, tab-separated.

- `pack <fmt> <args>` — pack the space-joined `args` (one per non-padding
  field) and print the bytes as lowercase hex;
- `unpack <fmt> <hexdata>` — unpack and print the values;
- `unpackt <fmt> <hexdata>` — unpack with `allow_truncated=True` (stop at the
  last field that fits);
- `unpackfrom <fmt> <offset> <hexdata>` — unpack starting at bit `offset`;
- `calcsize <fmt>` — the number of bits in the format;
- `byteswap <digits> <hexdata>` — reverse each byte group named by `digits`.

A format string is a run of **type-size** groups with optional bit-order and
byte-order markers. The eight types are `u`/`s` (unsigned/signed integer),
`f` (float), `b` (bool), `t` (text), `r` (raw bytes), and `p`/`P` (zero/one
padding). `<`/`>` before a group sets its bit order (LSB/MSB first); `<`/`>`
after all groups sets the byte order. An out-of-range integer, a bad format or
char, or too few bits to unpack prints the library's exact error message.

`oracle.py` replays every job against the vendored `bitstruct` library and
writes `expected.out`; `mini_bitstruct.kara` reproduces it byte-for-byte on
every backend.

## How the port works

The library builds each packed value as a string of `'0'`/`'1'` characters
(via Python's `bin()` and `binascii.hexlify`), concatenates them with the
bit/byte-order reordering, pads the last byte and converts back to bytes. The
port does the same over a **vector of 0/1 bits built with integer arithmetic
only** — no bitwise operators — so the layout, and therefore the packed bytes,
are identical:

- **Integers.** An unsigned value becomes its `size` least-significant bits,
  most-significant first; a signed value is the same after adding `2^size` when
  negative (two's complement), with the library's exact range check and error
  text.
- **Bit order `<`.** The value's bits are reversed before placement.
- **Byte order `<`.** The accumulated bit string is rebuilt
  least-significant-byte first, following the library's
  `aligned_offset` walk exactly (raw and text fields are exempt, as in the
  library).
- **Text / raw.** The UTF-8 (or raw) bytes are padded to `size/8` bytes and
  the first `size` bits taken.
- **unpack** reverses each step: slice `size` bits at the running offset, undo
  the byte- and bit-order reordering, then read the integer, bool, text or raw
  value.

## Scope

- **Integer / bool / text / raw / padding only.** The float format (`f16`,
  `f32`, `f64`) packs through the platform's IEEE-754 `struct` and renders as a
  Python float `repr`; neither the half-float encoding nor shortest-float
  printing is reproduced here, so `f` is out of scope (the one documented
  boundary — every other type is exact, errors included).
- **Positional API.** `pack`/`unpack`/`unpack_from`/`calcsize`/`byteswap` are
  ported. The dictionary API (`pack_dict`/`unpack_dict`) and the in-place
  `pack_into`/`unpack_from` buffer variants differ from the positional ones
  only in how fields are named and where bytes are written, so they are not
  separately exercised.
- Text fields use byte-aligned sizes (`size` a multiple of 8); the library's
  partial-last-byte text behaviour is not exercised.

The port was checked against the library over the job list on every backend,
and separately (during development) over 60,000 random format strings and value
sets for pack, unpack, unpack-from and truncated unpack, 5,000 random byteswaps,
and the full set of error cases.

The vendored library's core is a single pure-Python module importing only the
standard library (the optional `bitstruct.c` C-accelerated module is not used).

## Running

```bash
karac run mini_bitstruct.kara               # interpret
karac build --mir mini_bitstruct.kara       # native binary
python3 -s -P -B mk.py                       # regenerate data/jobs.tsv
python3 -I oracle.py                          # regenerate expected.out
python3 -s -P -B gen_kara.py                  # regenerate the embedded jobs
```
