# pipeline/thumb_render

An image pipeline, the shape of a thumbnail service. It renders four
160x120 source images, writes each as a binary PPM file, reads the files
back and decodes them, makes a blurred quarter-size thumbnail of each, runs
five reports, and composes the thumbnails into a contact sheet that it
writes as a PNG and checks by decoding it again.

- The images: a Mandelbrot set and a Julia set in Q20 fixed point (96
  iterations), a plasma from a 256-step Bhaskara sine table, and a terrain
  map from three octaves of value noise over a 32-bit integer hash.
- The decoder reads the PPM header field by field and checks the magic,
  the maxval and the pixel count. A copy cut to 1000 bytes shows its error.
- A thumbnail is a 3x3 Gaussian blur with clamped edges, then a 4x4 box
  average down to 40x30.
- The reports: a luma histogram, each channel's mean and standard
  deviation, a Sobel edge count, a four-colour median-cut palette of each
  thumbnail, and an ASCII preview of each thumbnail.
- The contact sheet is 76x56. The four thumbnails overlap by four pixels,
  and each paste blends 3:1 over what is already there. The PNG is
  written by hand: zlib stored blocks with an Adler-32, CRC-32 on every
  chunk. The decoder checks both and gets the pixels back; a copy with one
  bit flipped in the IDAT chunk's type fails its CRC.

Everything is integer arithmetic, so the backends and the Python mirror
`thumb_render.py` agree by construction; the mirror wrote `expected.out`.

stmt-par: six groups of statements that do not depend on each other, in
`run()` (`thumb_render.orig.kara` lines):
- L717-L720 four renders, each from its own constants. This is where most
  of the time goes;
- L723-L726 four `fs.write_bytes` calls, each with the `encode_ppm` call
  that builds its bytes;
- L741-L744 four `fs.read_bytes` calls;
- L753-L756 four `decode_ppm` calls. Each can fail with `?`, so a parallel
  run has to report the first failure in source order;
- L766-L769 four `make_thumb` calls;
- L772-L791 twenty reports, five per image, that only read the images and
  thumbnails.

The four `paste` calls (L802-L805) are the case that must stay in order:
each takes `mut ref sheet`, and where two thumbnails overlap the blend of
the later one goes over the earlier one.

## Edits from thumb_render.orig.kara to thumb_render.kara

Author errors (the diagnostics were right):
- `const W` and `const H` are one-letter names, so they are Type class,
  not Const class. The error says no capitalization can fix that, so there
  is no machine fix; they are `WIDTH` and `HEIGHT` now.
- An enum named `DecodeError` shadows the prelude's (a warning); it is
  `ImageError` now.

Compiler gaps: none found. After those two edits it matched the mirror on
the first run under `karac run`, the MIR interpreter, the LLVM backend, the
ASan build and the native binary, with nothing on stderr.

## Running

```bash
karac run thumb_render.kara
karac build --mir thumb_render.kara && ./thumb_render
python3 -I -B thumb_render.py > expected.out
```

About 8 s on the MIR interpreter and 0.02 s built. It writes its six files
to the working directory and removes them at the end.
