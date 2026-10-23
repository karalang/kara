# mini_pdf417

A port of [pdf417gen](https://pypi.org/project/pdf417gen/) 0.8.1 (MIT; Ivan
Habunek), the pure-Python PDF417 encoder. PDF417 is the stacked bar code on
airline boarding passes (IATA BCBP), North American driver's licences
(AAMVA), shipping labels, invoices and ID cards: rows of 17-module code
words, each row framed by start and stop patterns and by row indicators
that carry the row count, the column count and the error correction level.

```
> encode text:PDF417 bar code columns=4 security_level=1
  high 16: 12 453 178 121 236 811 17 782 423 149 900 900 570 800 916 132
  rows 4 cols 4 size 137x4
  1fea8 1eaf0 1adc0 1dd98 1f6bc 14306 1d4e0 3fa29
  ...
> svg boarding.svg file:in/boarding.txt columns=8 scale=2 color=#1a2b3c description=IATA BCBP <YUL> & "FRA"
  boarding.svg 410 x 36, 658 rects, 28731 bytes, crc32 59131193
  <svg version="1.1" xmlns="http://www.w3.org/2000/svg" width="410" height="36"><description>IATA BCBP &lt;YUL&gt; &amp; "FRA"</description><g id="barcode" fill="#1a2b3c" stroke="none">
```

## What it does

`mk.py` writes 154 jobs (and 8 section comments) to `data/jobs.tsv`, one
command each, plus nine input files under `data/in/`: a boarding pass, an
AAMVA licence record, an invoice, a vCard, a UPS-style parcel record with
its group and record separators, UTF-8 text, 92 digits, a binary blob and
an empty file. `oracle.py` runs each job against the vendored library and
prints the result, or `error <class>: <message>` when it raises;
`mini_pdf417.kara` runs the same jobs. 32 jobs raise.

- **Compaction**: how the input splits into text, numeric and byte
  chunks, the library's two optimizations (digit runs under 13 next to
  text become text, then neighbouring chunks of one mode merge) and the
  code words, including the latch codes between modes.
- **The three modes on their own**: text compaction's interim values and
  submode switches (upper, lower, mixed, punctuation), numeric
  compaction's base-900 conversion of up to 44 digits at a time, and byte
  compaction's 6-bytes-to-5-words packing.
- **Error correction and bases**: Reed-Solomon code words at levels 0 to 8
  (and the assert past them), `switch_base`, and the left and right row
  indicators.
- **Encoding**: full bar codes as high-level words and low-level rows, in
  UTF-8, Latin-1 and ASCII, with every size and argument check: columns,
  security level, too few or too many rows, a length descriptor over 928,
  non-str input, an unknown keyword, a missing file and text a codec
  cannot encode.
- **Module grids, colours and renderers**: the grids of dark modules,
  Pillow's `ImageColor.getrgb` over hex, `rgb()`, `rgba()`, `hsl()`,
  `hsv()` and `hsb()` specifiers, SVGs written to `data/out/` the way
  ElementTree serializes them, and images written there as binary PPM.

## How the port works

- **Tables come from the library.** `CODES` (3 x 929 bar patterns), the
  text submode lookup, the switch codes and the error correction factors
  are generated from `codes.py` and `data.py` into four constant strings,
  parsed once at startup.
- **Numbers keep Python's range.** Numeric compaction turns `"1" + digits`
  into base 900 by long division over decimal digits, so a 44-digit chunk
  needs no bignum type, and `int()`'s rules (whitespace, underscores, the
  error for anything else) are kept. Byte compaction and `switch_base` run
  in i128.
- **Codec errors are Python's.** Latin-1 and ASCII encoding report the
  first run of characters out of range, as one character or as a range of
  positions, with the escapes `UnicodeEncodeError` uses.
- **The renderers write what the library writes.** The SVG is the bytes
  `ElementTree.write` produces in its default us-ascii encoding (attribute
  order, `<rect ... />`, escaped description, character references), and
  the image is the module grid scaled by whole numbers, which is what
  Pillow's nearest-neighbour resize does at an integer zoom, then padded.
  Both files are checked by size and CRC-32, and are byte-identical to the
  oracle's.
- **Colours follow ImageColor.** The regexes are matched by hand, and
  `hsl()` and `hsv()` go through copies of `colorsys`, including its float
  modulo, so the rounding matches and an out-of-range saturation gives the
  same negative channels (and the same odd hex) as Pillow.

## Compiler gaps

None. `karac check` flagged three moves out of an index, which are
language rules (`karac fix` cloned two of them, and the port borrows
instead), and a function taking `Vec[Str]` that was passed a `vec!` of
string literals, which is a `Vec[String]`. After that the interpreter matched the oracle byte for byte on its first
run, files included.

## Scope

- Pillow's named colours (`"red"`, `"navy"` and the rest of its CSS
  table) are not ported; the jobs use the numeric specifiers, and an
  unknown name raises Pillow's error.
- The console script (`pdf417gen encode ...`) is not ported; its options
  map onto the library calls the jobs make.
- Only RGB images are produced, as `render_image` makes them; an RGBA
  colour's alpha is dropped.
- The images are compared with Pillow 12.3.0's output.

## Running

```bash
karac run mini_pdf417.kara                  # interpret
karac build --mir mini_pdf417.kara          # native binary
python3 -s -P -B mk.py                      # regenerate data/
python3 -I -B oracle.py > expected.out      # regenerate expected.out
python3 -I -B gen_kara.py                   # regenerate the embedded inputs
```
