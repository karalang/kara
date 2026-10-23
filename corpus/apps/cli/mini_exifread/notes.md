# mini_exifread

A port of [ExifRead](https://pypi.org/project/ExifRead/) 3.5.1
(BSD-3-Clause, Gene Cash, Ianaré Sévi and contributors): the pure-Python
EXIF reader that photo organisers, galleries and upload pipelines use to get
the camera, the exposure, the date and the GPS position out of a photo. A
Nikon with a maker note, read by the command line:

```
== cli - nikon_d70.nef (1138 bytes)
Opening: nikon_d70.nef
File has JPEG thumbnail
EXIF ExposureTime (Ratio): 1/250
EXIF FNumber (Ratio): 14/5
GPS GPSLatitude (Ratio): [51, 30, 663/25]
MakerNote ProgramShift (Undefined): -2/3 EV
...
```

## What it does

`mk.py` builds 188 photo files byte by byte and writes them to
`data/jobs.tsv`, one job a line. `oracle.py` replays the jobs through the
vendored library, and `mini_exifread.kara` replays them through the port.
A job is one of:

- **`cli`** (164 jobs): the `EXIF.py` command line on one file, with its
  flags: `-q` (no maker notes or thumbnails), `-s` (strict), `-b` (built-in
  Python types) and `-t` (stop at a tag). It prints the sorted tags with
  their field types and printable values, and the library's warnings as
  they happen.
- **`gps`** (17 jobs): `process_file`, then `utils.get_gps_coords`, on
  plain and on serialized tags.
- **`thumb`** (7 jobs): the JPEG thumbnail the library extracts, from the
  thumbnail IFD or from an Olympus maker note.

The files cover:

- **Every container ExifRead reads.** These are raw TIFF in both byte
  orders and JPEG with JFIF, JFXX, ICC, Adobe, XMP and comment segments in
  front of the Exif one. There is also PNG `eXIf`, WebP `VP8X`, HEIC, AVIF
  and `mif1` with `iloc` versions 0 to 2, the Nokia layout with a bare
  TIFF header, and JPEG XL.
- **Maker notes from ten makers.** Nikon has three layouts, including
  `ev_bias` on every exposure step. There are Olympus, Casio, Sony,
  Fujifilm, Apple and DJI notes. Canon has its offset tags and the
  CameraInfo blocks of the 5D, 5D Mark II, 5D Mark III and 600D / Rebel
  T3i / Kiss X5.
- **The files a real library meets.** These include IFD loops, unknown and
  zero field types, values past the end of the file, ASCII that is not
  UTF-8, zero denominators, counts of 1000 or more, and sub-IFD pointers
  that are ratios, strings or empty. There are truncated JPEGs, PNGs,
  WebPs and HEICs, broken HEIC boxes, and files that are not images.
- **Behaviour that is not obvious.** ExifRead reads the WebP size field as
  the VP8X flags, and does not skip RIFF padding. It adds 18 bytes to Nikon
  offsets in JFIF files. A Canon CameraInfo block of 1000 bytes or more is
  dropped. HEIC boxes from the previous file are kept, because `Box.subs`
  is a class attribute. The Olympus maker-note thumbnail is read at an
  offset taken from its own first byte.

An exception the library lets escape prints as `error <Class>: <message>`.
In the real command line it would end the program.

## How the port works

- **Values are Python values.** A `Val` enum holds int, `Ratio`, float,
  tuple, str, bytes, list and None, with Python's `str` and `repr` for each.
  A Ratio stays unreduced when its denominator is zero, as `utils.Ratio`
  does. A tag's printable, its dict lookups and the formatter functions
  all depend on the Python type of the values (for example, iterating an
  ASCII string yields characters, and iterating bytes yields ints). So the
  port keeps those types.
- **The file is a `BytesIO`.** Reads past the end return nothing, and a
  negative count reads to the end. Every finder and `s2n` read goes through
  it, so short reads, slice-length warnings and positions match.
- **The tag tables are the library's own.** `gen_kara.py` turns the
  vendored `exifread/tags` dicts into `tables_text()`. That is 18 tables,
  173 lookup dicts and their 1610 value names. The port parses them at
  start-up.
- **`get_printable` follows `_get_printable_for_field`.** It truncates past
  20 values, and calls `make_string`, `make_string_uc`, `ev_bias`,
  `special_mode` and `convert_temp`. It dumps sub-IFDs before the pointer
  tag is stored, and does dict lookups that match a `Ratio` equal to an
  int.
- **Maker notes move the frame.** Fujifilm, Apple and DJI notes change the
  byte order and the base offset while they are read, and restore them
  afterwards. Canon's offset tags turn into proprietary tags.
- **The HEIC box reader keeps a shared `subs` map** in the world state,
  for the same reason the library does.
- **`convert_types` and `get_gps_coords`** are ported, with each
  conversion function's handling of empty, single and list values.
- **`gen_kara.py` writes `tables_text()` and `jobs_text()`** at the end of
  the `.kara` file, so the kata reads no files at run time.

## Compiler gaps

None were hit.

## Scope

- **XMP is left out.** The library only parses it in debug mode.
- **`extract_tiff_thumbnail` is left out.** It runs only when the thumbnail
  Compression printable is "Uncompressed TIFF", and the Compression table
  spells that value "Uncompressed", so it never runs. The port keeps that
  check in a comment.
- **Python repr and regex.** `repr` of a str escapes the non-printable
  Latin-1 characters and treats every other non-ASCII character as
  printable. The Canon model patterns are matched with `$` as end of string
  and `\b` with every non-ASCII character as a word character.
- **Value types outside the jobs.** Some formatters get a value whose
  Python type no job produces. These are a `Ratio` or tuple passed to
  `ev_bias`, `special_mode` or `convert_temp`, or as a thumbnail offset.
  For them the port raises a `TypeError` whose message may differ from
  Python's. A negative seek is not checked.

The vendored library is unchanged.

## Running

```bash
karac run mini_exifread.kara                # interpret
karac build --mir mini_exifread.kara        # native binary
python3 -s -P -B mk.py                      # regenerate data/jobs.tsv
python3 -I -B oracle.py > expected.out      # regenerate expected.out
python3 -I -B gen_kara.py                   # regenerate tables_text() and jobs_text()
```
