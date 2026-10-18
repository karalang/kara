# mini_png

A PNG checker, decoder and encoder in the manner of `pngcheck`, with its
own inflate and deflate and no image library. It checks a file the way
an upload handler, a static-site build or an asset pipeline does before
trusting it, decodes it to RGBA as a thumbnailer would, and writes it
back out recompressed as an optimizer does.

The run writes 69 files into `data/`, then runs `data/jobs.tsv`, 120 jobs
with tab-separated fields. Each job prints `== ` and its fields joined by
spaces, then its output:
- `check F` prints a line for each chunk (offset, length and what it
  says), then `OK:` with the size, type, interlacing and compression
  ratio, or the `ERROR:` that stopped it;
- `decode F` prints the size, the CRC-32 of the image as 8-bit RGBA, how
  many pixels are opaque, transparent and partly transparent, and an
  ASCII preview of up to 32 by 16 pixels composited over white;
- `encode F [stored]` re-encodes the image without interlacing, choosing
  each row's filter by the smallest sum of absolute differences, and
  compresses it with LZ77 and fixed Huffman codes (or as stored blocks),
  then prints the filter counts, the new size and CRC-32, and whether
  decoding the new file gives the same pixels.

The 36 images cover every colour type at every bit depth (gray at 1, 2,
4, 8 and 16 bits, RGB, palettes of 1 to 8 bits, gray and RGB with alpha
at 8 and 16), Adam7 interlacing including a 1x1 image and images smaller
than a pass, every row filter, `tRNS` keys for gray, RGB and palettes, a
32x32 picture compressed by zlib at levels 0, 1 and 9, with the fixed,
Huffman-only and RLE strategies, a 512-byte window, and split into IDAT
chunks of 1 and 100 bytes, and files carrying `gAMA`, `cHRM`, `sRGB`,
`sBIT`, `tEXt` in Latin-1, `zTXt`, `iTXt` plain and compressed, `tIME`,
`pHYs`, `bKGD`, `iCCP` and an unknown private chunk. The 32 broken files
cover a bad signature, an empty file, a bad CRC, a huge length, a bad
chunk name, truncation, IHDR not first or repeated, a missing PLTE or
IEND, PLTE in a grayscale image, data after IEND, IDAT chunks split by
another chunk, each invalid IHDR field, bad palette indexes and `tRNS`
lengths, a bad row filter, image data too short and too long, and the
zlib errors: bad header, method, block type and stored lengths, a
distance too far back, a bad Adler-32 and a truncated stream.

**The port** has its own inflate, following zlib's checks in zlib's
order so that each error is the message Python's `zlib.error` carries:
the header, the three block types, code tables checked the way
`inflate_table` does (over-subscribed and incomplete codes), and the
Adler-32. It undoes the five filters, splits Adam7's seven passes, and
converts every depth to 8 bits the way the oracle does: 16-bit samples
keep their high byte, and 1, 2 and 4-bit samples scale by `255 / max`.
The encoder hashes three-byte prefixes into a chain and tries the 64
nearest earlier positions within 32 KiB, taking the longest match up to
258 bytes.

Not supported: APNG, `sPLT`, `hIST`, gamma and colour correction, and
writing interlaced or dynamic-Huffman output.

**Oracle.** `expected.out` is the output of `oracle.py`, the same
program in Python with `zlib` for inflating and checksums. Run `mk.py`
first; it writes `data/` with Python's `zlib` at the levels and
strategies above. `gen_kara.py` prints the Kāra data section (everything
after the last comment line ending in `data` in `mini_png.kara`) from the
same files, the PNGs as hex and `jobs.tsv` as text.
