# mini_unzip

Info-ZIP's `unzip` 6.0 and `zipinfo`, written from PKWARE's APPNOTE, RFC
1951 and Info-ZIP's output formats. Installers, package managers, CI
artifact steps and upload scanners all open zip files they did not make.
They have to inflate correctly, check CRCs, find the central directory
behind a self-extractor's stub, read zip64 sizes and data descriptors,
and refuse names like `../escape.txt` that would write outside the
target directory.

The inputs in `data/` were made here with Info-ZIP zip 3.0 and Python's
`zipfile`:
- **infozip.zip**: `zip -r -y` of a small project, with directories,
  stored and deflated files, a symbolic link and the archive comment
  `release 1.4`.
- **secret.zip**: two files encrypted with `zip -P hunter2`.
- **plain.zip**: `zip -0 -X`, stored only, with no extra fields.
- **stream.zip**: Python writing to a stream it cannot seek, so every
  entry has a data descriptor. It has a UTF-8 name (`café/naïve.txt`), a
  file comment and an archive comment.
- **zip64.zip**: one entry forced to zip64, and one deflated.
- **evil.zip**: zip-slip names: `../escape.txt`, `sub/../../../tmp/evil.sh`
  and `/etc/cron.d/job`.
- **Damaged files**: `badcrc.zip` (one byte of a stored file flipped),
  `sfx.zip` (a 128-byte shell stub in front), `trunc.zip` (the first 2000
  bytes, so no end record), and `notes.txt`, which is not an archive.

The program runs the commands in `data/jobs.txt`:
- `zipinfo`, with the mode, version, OS, text/binary and extra-field
  flags, method and date of each entry, and the totals line.
- `unzip -l` and `unzip -v`, with comments, CRCs, compression ratios and
  wildcard patterns.
- `unzip -t`, which inflates and checks every entry. It covers a bad CRC,
  the right and a wrong password, a self-extractor's offset, a missing
  end record, a missing file and unmatched names, each with Info-ZIP's
  messages and exit code.
- `unzip -p`, which writes an entry to stdout, one of them decrypted.
- `unzip -d`, which extracts five archives. It covers:
  - directory creation, and the failure for `-d out/nested`;
  - the deferred link;
  - Info-ZIP's zip-slip defences and their warnings;
  - decryption;
  - extracting a file with a bad CRC, which is written and reported.
- `tree`, which lists what each extraction wrote: each file's mode bits,
  modification time (UTC), size and CRC, and each link's target.

Not supported: compression methods other than stored and deflate,
AES encryption, multi-disk archives, `zipinfo -v`, and prompting
(`unzip` here never asks for a password or before overwriting).
Extraction restores what Info-ZIP restores on Unix: each file's
modification time from its DOS date and time (as UTC), its permission
bits when it was made on Unix (without setuid, setgid and sticky), and
symbolic links as real links. The time is set before the mode, since
changing it opens the file. Two things are left out because these
archives would not show them: the extended-timestamp extra field, whose
times here equal the DOS times, and the modes and times of directories,
which `tree` does not print because a directory unzip creates on its own
gets the current time.

**Oracle.** `expected.out` is the output of `oracle.py`, the same program
in Python. `real.py` runs `jobs.txt` with the real Info-ZIP unzip 6.0
and zipinfo, with `TZ=UTC`, putting each command's stdout before its
stderr. Its output is byte-identical to `expected.out`. Its `tree` prints
the same columns from `lstat`.
`mk.py` shows how `data/` was built, but a rerun makes different bytes
(Info-ZIP's encryption headers are random), so the committed `data/` is
the input.

## Edits from mini_unzip.orig.kara to mini_unzip.kara

Author errors:
- Bare parameters borrow (D5), so `inflate` and `open_archive`, which
  keep the bytes they are given, take `data: own Vec[u8]`, and
  `safe_name` pushes a clone of each path component it keeps.
- `if .. { "fat" } else if .. { .. }.to_string()` at the start of a
  statement does not parse as one expression, as in Rust. `os_name` now
  binds the `if` first.
- The struct for an archive member was called `Entry`, which shadows
  the prelude's map entry type. It is now `Member`.

After that, the output matched `expected.out` on `__mir-run`,
`__mir-llvm-run` and `__mir-llvm-asan` on the first run.

Written 2026-10-15 by the real-world apps thread; karac at eb605b2e5b.
