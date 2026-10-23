# mini_imagesize

A port of [imagesize](https://github.com/shibukawa/imagesize_py) 1.4.1
(MIT; Yoshiki Shibukawa), the library that web frameworks, static site
generators and Sphinx use to read an image's width and height, and its
DPI, from the first bytes of the file without decoding the image. It
knows PNG, GIF, JPEG, JPEG 2000, TIFF and BigTIFF, SVG, Netpbm and WebP.

```
> !audit page.html
  images/photo.png: ok 1920x1080
  images/camera.jpg: no width/height attributes, actual 4032x3024
  images/progressive.jpg: mismatch, declared 1024x700, actual 1024x768
```

## What it does

`mk.py` builds 98 image files byte by byte in `data/images/`, with the
headers their formats lay out (PNG chunks with CRCs, JPEG markers, JP2
boxes, TIFF IFDs, RIFF chunks) over small bodies, plus broken and
truncated variants. It writes `data/jobs.txt`, which runs `get()` and
`getDPI()` on each of them and on a missing file, then three programs.
`oracle.py` runs the jobs against the vendored library and prints each
result, or `error <class>: <message>` when the library raises (31 times
from `get()`, 7 from `getDPI()`); `mini_imagesize.kara` runs the same
jobs.

- **PNG** (12 files): the IHDR size, and the pHYs density in pixels per
  metre or without a unit, after other chunks or before IHDR. Without
  IHDR at byte 12, the library reads the size from bytes 8 to 16.
- **GIF** (5): the screen size, read as signed, so 40000 is `-25536`.
- **JPEG** (15): the first SOFn frame, past Exif, DHT and fill bytes,
  for baseline, progressive, lossless and arithmetic files; the JFIF
  density in inches, centimetres or as an aspect ratio; and files cut
  short at five places.
- **JPEG 2000** (6), **TIFF and BigTIFF** (15), **SVG** (21), **Netpbm**
  (11) and **WebP** (9), and four files in formats the library does not
  know.
- **`gallery`** writes `<img>` tags at a scale for 14 files.
  **`thumbs`** fits 24 images into 320x240 and gives the printed size at
  the image's DPI. **`audit`** checks the `width` and `height` of the 11
  `<img>` tags in an HTML page against the files.

## How the port works

- **The file is a cursor.** The library seeks and reads a Python file as
  it walks the markers, chunks, boxes and IFDs. The port reads the whole
  file and moves a position over it with Python's rules: a read past the
  end returns fewer bytes or none and leaves the position alone, and a
  seek past the end is allowed.
- **Errors are Python's.** `get()` turns the JPEG loop's short reads into
  `Invalid JPEG file`, but the TIFF, BigTIFF and WebP readers let
  `struct.error` (whose class prints as `error`) and `IndexError` out.
  `getDPI()` catches only `struct.error` in its JPEG loop, so a file that
  ends early raises `ord()`'s `TypeError`. An empty `width=""` reaches
  `float(None)`. A BigTIFF header with the wrong byte size formats a
  variable before assigning it, so it raises `UnboundLocalError`.
- **The SVG reader is a regular expression.** `[^-]width="(.*?)"` on the
  first 1024 bytes finds the first `width` not after a `-`, with a value
  on one line, so `stroke-width` is skipped and a `<rect>`'s width counts
  when the `<svg>` has none. The bytes must be valid UTF-8, so a file
  whose 1024th byte splits a character is `Invalid SVG file`. Values go
  through `_convertToPx`, where `pt` divides by 6 like `pc`.
- **The quirks stay.** A big-endian SHORT is divided by 65536, the
  little-endian reader ignores the type, a VP8X size is one less than the
  image, and a Netpbm file that ends after the width has a height of 0.
  The JP2 `resd` box is read with `read(10)` unpacked as six bytes, so it
  works only when exactly six bytes are left, and then reads the
  vertical denominator as the horizontal density and the two bytes of
  the horizontal numerator as the units.

## Compiler gaps

None. `karac check` asked for `own String` on one parameter, an explicit
`.to_vec()` on `bytes()`, and flagged that a struct named `File` shadows
the prelude type. After those, the interpreter matched the oracle byte
for byte on its first run, as did the LLVM, ASan and native builds.

## Scope

- Paths only. The library also takes a `BytesIO`, which the port has no
  use for.
- `_convertToPx`'s `\d` matches any Unicode digit; the port takes ASCII
  digits, which is all the files hold.

## Running

```bash
karac run mini_imagesize.kara                     # interpret
karac build --mir mini_imagesize.kara             # native binary
python3 -s -P -B mk.py                            # regenerate data/
python3 -I -B oracle.py > expected.out            # regenerate expected.out
python3 -I -B gen_kara.py                         # regenerate the embedded data
```
