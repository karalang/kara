# mini_xz

XZ Utils 5.4.5's `xz` and `sha256sum`, written from the .xz file format
specification, the LZMA specification and liblzma's sources. Linux
distributions ship packages and kernel tarballs as `.xz`, and package
managers, installers, container builders and backup tools have to unpack
them, check them, and say exactly what is wrong when a download is
truncated or damaged. Inventory and security tools also list what an
archive holds, such as its streams, blocks, checks and filters, without
unpacking it.

The inputs in `data/` were made here with XZ Utils 5.4.5:
- **notes.txt.xz**, **notes-crc32.xz** and **notes-sha256.xz**: one text
  file at three presets with the CRC-64, CRC-32 and SHA-256 checks.
- **none.xz**: a C source with no check at all.
- **multi.xz**: four blocks of 4 KiB from the multithreaded compressor,
  with their sizes stored in the block headers.
- **concat.xz**: two streams with different checks, joined with stream
  padding between and after them.
- **audio.raw.xz** (the delta filter on 16-bit stereo samples),
  **hello.xz** (the x86 BCJ filter on a compiled program) and
  **rand.bin.xz** (random bytes, stored as uncompressed LZMA2 chunks).
- **empty.xz**, a stream with no blocks, and **legacy.lzma**, the old
  `.lzma` format with an end marker.
- **Damaged files**: `trunc.xz` (the first 400 bytes), `flip.xz` (one bit
  changed inside the compressed data), `badcheck.xz` (a changed CRC-64),
  `badfooter.xz` (a broken footer magic), `plain.xz` (text) and
  `tiny.xz` (three bytes).

The program runs the commands in `data/jobs.txt`:
- `xz -l`, `-lv` and `-lvv`. They read each file backwards from its
  footers, the way liblzma's file-info decoder does, and print the
  streams, blocks, offsets, sizes, ratios, checks and stream padding,
  with totals over several files. `-lvv` also reads every block header,
  for its stored check value, header size, size flags, memory use and
  filter chain. It covers xz's errors for a missing, tiny, damaged and
  non-.xz file.
- `xz -t` and `-tv`, which decode and check every file, including both
  streams of `concat.xz` and the `.lzma` file. It covers xz's messages for
  a truncated file, corrupt data, a bad check and a file in no known
  format.
- `xz -dc` and `-dk`, then `sha256sum` of what `-dk` wrote, which shows
  that the delta and x86 filters and the uncompressed chunks decode to
  the original bytes. It also covers xz's refusal to overwrite a file.

Not supported: compressing, the ARM, ARM64, PowerPC, IA-64, SPARC and
RISC-V BCJ filters, `--robot` output, and reading standard input. xz's
memory figure is computed from the dictionary size plus a fixed
allowance for the decoder's state, which gives xz's numbers for these
files but not liblzma's exact byte count.

**Oracle.** `expected.out` is the output of `oracle.py`, the same program
in Python. `real.py` runs `jobs.txt` with the real xz 5.4.5 and
`sha256sum`, putting each command's stdout before its stderr. Its output
is byte-identical to `expected.out`. `mk.py` shows how `data/` was built,
but a different gcc would produce a different `hello`, so the committed
`data/` is the input.

## Edits from mini_xz.orig.kara to mini_xz.kara

Compiler gaps:
- B-2026-10-14-43: `karac check` refuses string-literal patterns in a
  `match` on a `Str` parameter, so `message` matches on `e.to_string()`.

After that, the output matched `expected.out` on `__mir-run`,
`__mir-llvm-run` and `__mir-llvm-asan` on the first run.

Written 2026-10-15 by the real-world apps thread; karac at eb605b2e5b.
