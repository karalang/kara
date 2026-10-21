# mini_piexif

Exif metadata as piexif 1.1.3 reads and writes it. piexif is the
pure-Python library that photo tools use to read the camera settings,
capture time and GPS position that cameras put in JPEG, TIFF and WebP
files, and to change or strip them. The port covers:
- `load()`, from a file name (reading the JPEG segments only as far as
  the Exif one) or from bytes, with every TIFF value type, little- and
  big-endian files, the thumbnail and its `1st` IFD, and `key_is_name`;
- `dump()`, which lays an exif dict out as a big-endian TIFF block,
  with its type checks and the errors `struct.pack` gives for a value
  out of range;
- `insert()`, `remove()` and `transplant()` on JPEG files, and the WebP
  chunk handling (`VP8`, `VP8L`, `VP8X`, `EXIF`, `XMP`, `ANIM`) behind
  `insert` and `remove`;
- `helper.UserComment`'s `load` and `dump` for the ASCII, JIS and
  Unicode encodings;
- five programs over a folder of holiday photos in `data/photos/`:
  - a report of each photo's camera, capture time, exposure, aperture,
    ISO, focal length, lens, orientation, position and comment, with the
    private fields it carries;
  - a library organised by capture date in `data/out/library/`, with an
    `index.txt`;
  - a camera clock fix from `data/clock.txt` followed by geotagging from
    the GPS track in `data/track.csv` (the nearest point within 15
    minutes, the earlier one on a tie), written to `data/out/tagged/`;
  - a privacy strip for sharing (location, owner, serial number, maker
    note, comment and thumbnail), written to `data/out/share/`;
  - thumbnail extraction to `data/out/thumbs/`.

The run writes `data/`, works through the 103 jobs in
`data/jobs.jsonl`, and then runs the five programs. `mk.py` wrote
`data/`; the binary files are held as hex in the program.

`oracle.py` runs the same jobs and programs through piexif itself and
wrote `expected.out`. It uses the `piexif/` package from the 1.1.3
release, unchanged.

The quirks are kept. Some examples:
- `merge_segments` puts the Exif segment in place of an `APP0`, so
  inserting into a JFIF file drops the JFIF header; and inserting into
  a file whose first `APP1` is XMP adds a second Exif segment rather
  than replacing the one after it.
- A rational is always a pair, but any other value read with a count
  of one is unwrapped from its tuple.
- An `ASCII` value with a count of 1, just its NUL, comes back as
  `b''`.
- `dump()` rewraps a bad value's error as a `ValueError` naming the tag
  and the IFD; a value whose type is not checked fails inside
  `struct.pack` with its own message, and an unknown type ends in the
  `UnboundLocalError` the Python code raises.
- A value of a type `load()` does not know (13, say) is a `ValueError`
  naming the tag and the type, not a skipped entry.
- A broken photo fails with the bare `struct.error` that unpacking a
  short buffer raises, so the programs report it as `error`.

Not ported: `load()` of an open file object rather than a path or
bytes.

## License

piexif is under the MIT License, in `LICENSE-piexif.txt`:

    Copyright (c) 2014, 2015 hMatoba

That covers `piexif/`.
