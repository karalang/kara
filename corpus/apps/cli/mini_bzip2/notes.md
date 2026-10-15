# mini_bzip2

bzip2 1.0.8 and `sha256sum`, written from bzip2's format and its
sources. Backup tools, package builders, log shippers and archive
readers handle `.bz2` files made by someone else. They have to read
several streams in one file, check every block and stream CRC, and tell
a truncated download from a damaged one. A tool that writes `.bz2` and
wants reproducible output, so that checksums and package signatures
match, has to produce bzip2's own bytes. This program's compressor does:
the archives it writes have the same SHA-256 as the real bzip2's.

The inputs in `data/` were made here with bzip2 1.0.8:
- **access.log** (400 lines of web-server log) and **access.log.bz2**
  (`-9`).
- **big.txt.bz2**: 2900 lines at `-1`, nearly one whole 100k block.
- **poem.txt**, **runs.bin** (long byte runs, which exercise
  run-length stage one and the RUNA/RUNB codes) and **empty.txt**, which
  the jobs compress; **empty.bz2**, an empty file's archive.
- **concat.bz2**: two streams joined with `cat`.
- **Damaged files**: `trunc.bz2` (the first half of `access.log.bz2`),
  `flipped.bz2` (one bit changed a third of the way in), `trailing.bz2`
  (text after the end of the stream), and `notes.bz2`, which is plain
  text.

The program runs the commands in `data/jobs.txt`:
- `bzip2 -t`, `-tv` and `-tvv`, which test files. It covers several
  files with bzip2's aligned names, the per-block `-vv` trace, a
  truncated file, a CRC error, trailing garbage, a file that is not
  bzip2, and a missing file, with bzip2's messages and exit codes.
- `bzip2 -dc` and `-dk`, which decompress, including both streams of
  `concat.bz2`.
- `bzip2 -k`, `-kv` and `-kfv` at levels 1, 6 and 9, with the `-v`
  compression report and the refusal to overwrite an existing file.
  `sha256sum` then checks each archive against the real bzip2's. The
  compressor follows bzip2 exactly:
  - run-length stage one, the Burrows-Wheeler transform (here by prefix
    doubling) and move-to-front with RUNA/RUNB zero runs;
  - the choice of two to six Huffman tables, the initial partition of
    the symbols, and four passes that assign each 50-symbol group to
    its cheapest table;
  - bzip2's length-limited Huffman builder, with its heap and its
    weight halving when a code is too long;
  - the MTF-coded selectors, the delta-coded code lengths and the
    16-by-16 symbol map.

Not supported: randomised blocks (written only by bzip2 versions before 0.9.5), the
`-vvv` trace, `-s`, `bzip2recover`, and reading standard input.

**Oracle.** `expected.out` is the output of `oracle.py`, the same program
in Python. `real.py` runs `jobs.txt` with the real bzip2 1.0.8 and
`sha256sum`, putting each command's stdout before its stderr. Its
output is byte-identical to `expected.out`. `mk.py` shows how `data/`
was built.

## Edits from mini_bzip2.orig.kara to mini_bzip2.kara

Compiler gaps:
- B-2026-10-15-60: `karac check` refuses `BLOCK_MAGIC.to_vec()` on an
  `Array[i64, 6]` const, though design.md § Method resolution gives an
  `Array[T, N]` receiver `Slice[T]`'s methods, and `contains` already
  works that way. The four uses now read `BLOCK_MAGIC.as_slice().to_vec()`.

After that, the output matched `expected.out` on `__mir-run`,
`__mir-llvm-run` and `__mir-llvm-asan` on the first run.

Written 2026-10-15 by the real-world apps thread; karac at eb605b2e5b.
