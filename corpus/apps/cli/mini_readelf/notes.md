# mini_readelf

GNU readelf for x86-64 ELF files, plus a checksec table and the dynamic
loader's checks, written from the ELF and gABI specs and checked against
binutils 2.42. Build pipelines, package linters, security scanners and
crash tools read ELF headers, symbol tables and version sections without
running the binary. They have to get readelf's details right: how a
section is placed in a segment, symbol versions, the `.gnu.hash` bloom
filter, and what to say when a file is cut short.

The inputs in `data/` were compiled here with gcc 13.3 and binutils 2.42:
- **app.o**: a relocatable object with `.rela.text` and `.rela.eh_frame`.
- **app**: a PIE linked against `libgreet.so.2` through `RUNPATH $ORIGIN`,
  with full RELRO, a stack canary and `_FORTIFY_SOURCE`.
- **libgreet.so.2**: a shared library with a version script. It defines
  `GREET_1.0` and `GREET_2.0`, an old `greet@GREET_1.0` and a default
  `greet@@GREET_2.0`, and a TLS variable.
- **legacy**: a non-PIE executable with no canary, an executable stack,
  no RELRO, an `RPATH` and the old SysV `.hash`.
- **app-stripped**: `app` after `strip --strip-all`.
- **Damaged files**: `app.trunc` (the first 5000 bytes of `app`),
  `app.head` (40 bytes), `badshoff.o` (section headers past the end),
  `badstrndx.o` (no section-name table), `app.v3` (`app` relinked, in
  effect, against a `GREET_3.0` that the library does not define), and
  `build.sh`, which is not an ELF file at all.
- **src/**: the C sources and version script.

The program runs the commands in `data/jobs.txt`:
- 21 `readelf` commands. Together they use `-h`, `-l`, `-S`, `-d`, `-r`,
  `-s`, `-n`, `-V`, `-I`, `-x` and `-p`, with several files at once and
  with readelf's errors and warnings. It covers:
  - the file header, with readelf's test for a position-independent
    executable;
  - the section headers and their flags;
  - the program headers and binutils' section-to-segment rule, including
    its `.tbss` special case;
  - the dynamic section's tags, flags and sizes;
  - relocations with their x86-64 types and signed addends;
  - both symbol tables, with `@`, `@@` and `(n)` versions;
  - `.gnu.version`, `.gnu.version_d` and `.gnu.version_r`;
  - build-id, ABI-tag and x86 property notes;
  - hex and string dumps;
  - the bucket histograms of `.hash` and `.gnu.hash`.
- `checksec` over every file: RELRO, stack canary, NX, PIE, RPATH,
  RUNPATH, symbols, FORTIFY with the fortified functions, and CET.
- `ldcheck` for four files, checking what `ld.so` checks before a program
  starts:
  - where each `DT_NEEDED` library is found through `$ORIGIN`;
  - each needed version, with its `vna_hash` recomputed and matched
    against the library's version definitions;
  - each undefined symbol, bound through the library's hash table.

  For `app.v3` it prints the loader's `version 'GREET_3.0' not found`
  refusal.
- `lookup`, which looks names up through `.gnu.hash` (the bloom filter,
  the bucket and the chain) and through SysV `.hash`, the way the
  loader does. The lookups include a default version, a hidden one, a
  local symbol, an undefined one, and a bloom-filter false positive that
  ends at the end of its chain.

Not supported: 32-bit and big-endian files, `REL` relocations (x86-64
uses `RELA`), section groups, compressed sections, DWARF, and the
non-wide (`-W`-less) layouts of the symbol and note tables.

**Oracle.** `expected.out` is the output of `oracle.py`, the same program
in Python. `real.py` runs the `readelf` commands in `jobs.txt` with
GNU readelf 2.42 itself. Its output is byte-identical to the first part
of `expected.out`, so the readelf half matches readelf. `mk.py` shows how
`data/` was built, but a different compiler would produce different
bytes, so the committed `data/` is the input.

## Edits from mini_readelf.orig.kara to mini_readelf.kara

Six uses of `d.syms.rows[k]` and `me.syms.rows.len()` once borrowed the
inner field first, a workaround for B-2026-10-14-70. It was dropped
after the fix in 0bacd9912b.

Author errors:
- `dyn` is a reserved word, so the field holding the dynamic entries is
  `dyns`.
- A closure had a Rust-style return type (`|o: Str| -> bool { .. }`),
  which Kāra infers.

After that, the output matched `expected.out` on `__mir-run`,
`__mir-llvm-run` and `__mir-llvm-asan` on the first run.

Written 2026-10-15 by the real-world apps thread; karac at eb605b2e5b.

## Back to the original, 2026-10-18

The rows behind the edits above are fixed, so `mini_readelf.kara` is now exactly what `karac fix` makes of `mini_readelf.orig.kara`. It matches expected.out on all four MIR backends (karac at 4ea06e27b9).
