# tar_gz: a ustar archiver and gzip codec

This app writes a small site to disk (an HTML page, a text file, an empty
file, 2000 bytes of noise, and a file whose path is longer than 100 bytes),
packs it into `site.tar.gz`, reads it back, lists it the way `tar tv` does,
and extracts it into `out/`. Everything is written from the specs:

- **ustar (POSIX):** octal fields, the header checksum with its NUL and
  space, the 155-byte prefix field for long paths, directory entries, the
  two zero blocks at the end, and padding to a 10240-byte record.
- **DEFLATE (RFC 1951):** an LZ77 match finder over hash chains (32 KiB
  window, 64 probes), and stored, fixed-Huffman and dynamic-Huffman blocks.
  The compressor costs all three in bits and writes the smallest. Code
  lengths are limited to 15 bits (7 for the code-length code) by halving
  the counts until the tree fits. The inflater is a full decoder in the
  style of Mark Adler's puff, with the same checks for incomplete and
  over-subscribed codes.
- **gzip (RFC 1952):** the header with its optional fields, and the CRC-32
  and size trailer.

The output is a real `.tar.gz`: GNU `tar tvzf`, `gzip -t` and Python's
`tarfile` read the file the Kara program writes, and `zlib` decodes each
of its stored, fixed and dynamic streams. The inflater also decodes a
stream that zlib produced at level 9. The last section damages the archive
(a flipped trailer CRC, truncation, a flipped bit in the compressed data,
a trailing byte, a header checksum) and prints where each one is caught.

Oracle: `tar_gz.py`. The compressor is a port of the Kara (the same match
finder and Huffman builder), so the sizes agree to the bit. The dates come
from Python's `datetime`. `expected.out` is its output.

## Edits from tar_gz.orig.kara to tar_gz.kara

Author errors (the diagnostics were right):
- `!x` for negation; `karac fix` rewrote both to `not x`.
- `show(e)` was used without being defined in this file.
- `String.from_utf8(r.out)` moved `r.out`, which was used afterwards.
- A struct named `Entry` shadowed the prelude type; it is now `Member`.

Compiler gaps, worked around and listed in the source header:
- **B-2026-10-14-53** also covers the new fs functions: `karac check`
  refuses `fs.create_dir_all(path)` followed by a use of `path` as a move,
  while every MIR surface runs it. The app passes `path.clone()`.
- **B-2026-10-14-44:** `File.open` and `File.create` take `ref String`.

No new rows. One environment trap: the ASan lane and `mir-build` link the
runtime archive, so an archive built before b4c775b2e9 makes
`fs.create_dir_all` fail with "not found" there while the JIT surfaces pass.
Rebuilding the archives fixed it.

Written 2026-10-15 by the real-world apps thread; karac at 1147b8ef08.
