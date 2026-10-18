# mini_hashsum

GNU coreutils 9.4's checksum tools: `md5sum`, `sha1sum`, `sha224sum`,
`sha256sum`, `sha384sum`, `sha512sum`, `b2sum`, `cksum` and `sum`. Every
release page, package mirror, container registry and installer publishes
a `SHA256SUMS` file or a tagged checksum list, and download scripts, CI
jobs and backup tools verify against them and have to say exactly which
file is missing, damaged or listed in a line they cannot read. The hash
functions are written here from their specifications: MD5 (RFC 1321),
SHA-1, SHA-224, SHA-256, SHA-384 and SHA-512 (FIPS 180-4), BLAKE2b with
any output length (RFC 7693), the POSIX `cksum` CRC and the BSD and
System V `sum` checksums.

The inputs in `data/` were made by `mk.py`:
- **README**, **LICENSE**, **empty.dat**, a file named `odd\name.txt`
  (whose name has to be escaped in the output), and a `release/`
  directory with a 20000-byte tarball, release notes and a signature.
- **SHA256SUMS**, **SHA1SUMS**, **B2SUMS** and **MD5SUMS** (in the
  tagged form), as the tools write them.
- **SHA256SUMS.bad**, with a wrong digest, a listed file that does not
  exist, a line that is not a checksum and a blank line.
- **CHECKSUMS**, a mixed list of tagged lines for `cksum -c`: SHA-512,
  SHA-256 in base64, BLAKE2b-256, SHA-1, SHA-224, a SHA-384 with one
  digit changed and an MD5 line with a short digest.

The program runs the commands in `data/jobs.txt`:
- Hashing with every tool, in the untagged and `--tag` forms, with
  `b2sum -l` for shorter BLAKE2b digests and its error for a length that
  is not a multiple of 8.
- `cksum` with its default CRC and with `-a` for `sysv`, `bsd`, the SHA
  family, `blake2b -l` and `md5`, with `--untagged` and `--base64`, and
  its error for an unknown algorithm. `sum` and `sum -s` give the BSD and
  System V checksums with their block counts.
- `-c` with `--quiet`, `--status`, `--ignore-missing`, `--strict` and
  `-w`. It covers the per-file OK and FAILED lines, the warnings counted
  at the end, escaped names, base64 digests, and the errors for a missing
  list, a list with no checksum lines and a list for a different
  algorithm. `cksum -c` reads the algorithm from each tagged line.
- The errors for a directory and a missing file.

Not supported: reading standard input, `--zero`, `--binary` and `--text`
(which only change the marker between digest and name), SM3, and the
`--debug` and `--raw` options.

**Oracle.** `expected.out` is the output of `oracle.py`, the same program
in Python using `hashlib`. `real.py` runs `jobs.txt` with the real GNU
coreutils 9.4 tools, putting each command's stdout before its stderr. Its
output is byte-identical to `expected.out`.

## Edits from mini_hashsum.orig.kara to mini_hashsum.kara

Compiler gaps:
- B-2026-10-15-67: `u32.rotate_left` and `u64.rotate_right` pass
  `karac check`, but `__mir-run` stops with "call of unknown function
  u32.rotate_left" and `__mir-llvm-run` with "native `u64.rotate_right`
  is not lowered yet". The 16 rotations in MD5, SHA-1, the SHA-2 family
  and BLAKE2b call hand-written `rotl32`, `rotr32` and `rotr64` instead.

After that, the output matched `expected.out` on `__mir-run`,
`__mir-llvm-run` and `__mir-llvm-asan` on the first run.

Written 2026-10-15 by the real-world apps thread; karac at 7cb1840d66.

Reverted 2026-10-15, karac at eae83f9d88: B-2026-10-15-67 is fixed for
32- and 64-bit rotates on every v2 surface, so `mini_hashsum.kara` is
`mini_hashsum.orig.kara` again, with the natural `.rotate_left(n)` and
`.rotate_right(n)` calls, and still matches on all four MIR surfaces.

## Library migration, 2026-10-18

Thread A's a0ca700228 and acd593fc77 changed the library to match docs/library: a number's `parse` returns `Result[T, ParseError]`, and `split`, `lines`, `split_whitespace` and `trim` give `Str` views (`split` and `lines` are now lazy iterators). Arguments forwarded to a `mut ref` parameter are now marked too. `karac fix` made the edits. The app matches expected.out on all four MIR backends again (karac at a4b0967495).
