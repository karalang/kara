# mini_pystrich

A port of [pyStrich](https://github.com/mmulqueen/pyStrich) 0.20
(Apache-2.0; Michael Mulqueen, Helen Taylor and contributors), the
pure-Python barcode library. It covers pyStrich's two 2D symbologies,
Data Matrix (ECC 200) and Aztec Code: the input model with its charsets
and markers, the mode optimisers, Reed-Solomon, module placement and the
ASCII rendering. Three small programs run on top: GS1 product labels,
travel tickets and a table of symbol sizes.

```
> az payloads/one.txt qz=1 art
  10 bits, compact L1, 15x15, 6-bit codewords, 2 data + 15 check
  mode 0000000110110100011000100001
  data 380b
  ascii 17x17 fnv e3922b47
    |                 |
    |     XXXXXX   X  |
    | XXXX X XXXXX XX |
    | X XX        XX  |
    |   XXXXXXXXXXXXX |
...
> !tickets payloads/tickets.txt
  rail: 51 characters -> aztec compact L4 27x27 (42+34), data matrix 26x26, fnv 68870c4f
  ...
  transit: 36 characters -> aztec compact L3 23x23 (30+21), data matrix 22x22, fnv 21aef337
```

## What it does

`mk.py` writes 38 files to `data/`: 35 payloads, a product table, a list
of tickets, and `data/jobs.txt` (96 jobs in 7 sections). `data/files.txt`
lists the inputs. `oracle.py` runs each job on the vendored pyStrich and
prints the result; `mini_pystrich.kara` runs the same jobs.

- **`dm`** (51 jobs): `DataMatrixEncoder` on a payload, from a plain
  `str` or a `DataMatrixData` with `enc=ascii|iso-8859-1|utf-8|compat|auto`,
  with `shape=`, `bytes` (force byte mode) and `qz=`. A job prints the
  warnings, then the symbol size, its regions, the data and error
  codewords, the size and FNV-1a hash of `get_ascii()`, and with `art`
  the symbol itself; or the error.
- **`az`** (42 jobs): `AztecEncoder` with `ecc=`, `kind=`, `layers=` and
  `qz=`. A job prints the bit count, the symbol kind, layers, size and
  codeword width, the mode message bits and the data codewords, then the
  rendering as above.
- **`!labels`** checks each GTIN's length and check digit, then builds a
  GS1 Data Matrix from `GS1Fixed`/`GS1Variable` fields (GTIN, expiry,
  batch and serial). Four of the eight rows are rejected: a wrong check
  digit, a 13-digit GTIN, an empty batch and a tab in a batch.
- **`!tickets`** encodes five ticket records as Aztec and Data Matrix.
- **`!capacity`** prints the square, rectangular and Aztec symbol each
  payload class needs at 1, 10, 50 and 200 characters.

## How the port works

- **The input model.** A payload is a list of segments, text or raw
  codewords, with a charset. As in pyStrich, a plain `str` goes to Data
  Matrix through `fnc1_workaround_compat`: a leading `chr(231)` makes
  every `chr(231)` an FNC1 marker, with a warning. Otherwise the charset
  is the narrowest of ASCII, Latin-1 and UTF-8 that holds the text, and a
  non-ASCII charset adds an ECI prologue.
- **Data Matrix** runs pyStrich's dynamic program over (mode, position,
  phase) for ASCII, C40, Text and X12, with costs in thirds of a
  codeword. The relaxations run in pyStrich's order with the same strict
  comparisons, so ties go the same way. Then come the packers for each
  run of one mode, padding (the trailing Unlatch is dropped on an exact
  fit), the 253-state pad, Reed-Solomon over GF(256) in interleaved
  blocks, the diagonal placement with its four corner patterns, and the
  finder handles of each region.
- **Aztec** runs its DP over the Upper, Lower, Mixed, Punct and Digit
  modes, with latches, shifts and Punct digraphs. Byte runs close at a
  position through a sliding-window minimum for each of the two
  length-prefix regimes, as in pyStrich. Then come bit stuffing, the size
  choice (compact before full at the same module count), Reed-Solomon
  over GF(64), GF(256), GF(1024) or GF(4096) by codeword width, the mode
  message over GF(16), and the bullseye, chevrons, reference grid, mode
  ring and data spiral.

pyStrich's behaviour shows in the output, and the port keeps it:

- The legacy `compat` encoding turns each non-ASCII character into the
  raw codeword one above its code point, which no reader decodes back to
  the letter (`Grüße` becomes `48 73 fd e0 66`). `€` raises
  `ValueError: codeword must be 0-255, got 8365`.
- An empty Aztec payload picks a compact L1 symbol for zero codewords,
  and the mode message then refuses it with
  `invalid data_codewords=0 for compact symbol; expected 1..64`.
- At `ecc=95`, `Hello, World!` needs a full L9 symbol, 53x53 with 8 data
  and 222 check codewords; at the default 23% it fits a compact L2,
  19x19.
- `shape=auto` picks the smallest area, so `Hello, World!` gets a 12x26
  rectangle, while lower-case text and the URL stay square.

## Compiler gaps

One, a new shape of an open row. `for s in data.segs` over a field of a
borrowed struct, then `match s { Seg.Code(v) => cws.push(*v), ... }`,
passes `karac check`, and every MIR front end refuses the `*` because it
types `v` as `i64`. Over a `ref Vec[Seg]` parameter, both sides accept
`*v`. The port drops the `*` at that one site. This is the third shape of
kara `B-2026-10-23-8`, which now records it.

Apart from that, `karac check` refused `gen` as a variable name (it is
reserved) and reported ten ownership errors, which `#[derive(Copy)]` on
the symbol spec, two `.clone()` calls and a `ref` binding fixed. After those changes the interpreter matched the oracle
byte for byte on its first full run, as did the LLVM, ASan and native
builds.

## Scope

- No PNG, SVG, EPS or DXF output, and no terminal art: the jobs check
  `get_ascii()`.
- No QR Code, PDF417 or 1D symbologies, and no EDIFACT or Base 256
  encodation (pyStrich does not implement those two either).
- pyStrich's matrix renderers import its SVG and EPS writers, which
  import the Courier Prime glyph outlines (under the SIL Open Font
  License). The jobs never draw text, so that module is not vendored and
  `oracle.py` installs an empty stand-in before the import.

## Running

```bash
karac run mini_pystrich.kara                      # interpret
karac build --mir mini_pystrich.kara              # native binary
python3 -s -P -B mk.py                            # regenerate data/
python3 -I -B oracle.py > expected.out            # regenerate expected.out
python3 -I -B gen_kara.py                         # regenerate the embedded data
```
