# mini_pathlib

Paths as Python 3.13's `pathlib` handles them. Build tools, packagers,
installers, sync clients and anything that takes a path from a user or
a config file must split, join, compare and match paths, often for a
platform other than the one they run on. The program ports
`PurePosixPath` and `PureWindowsPath` with their exact rules and error
messages, then uses them to package a real source tree.

The run has three parts:
- **paths**: the 37 strings in `data/paths.txt`, POSIX and Windows. For
  each one the program prints:
  - the string and the repr;
  - the drive, root and anchor;
  - the parts, name, suffix, suffixes and stem;
  - the parent and all the parents;
  - `is_absolute` and `as_posix`.

  The inputs include empty paths, `.` segments, doubled and tripled
  slashes (`//` keeps its root on POSIX), dot files, trailing dots,
  drive-relative Windows paths (`c:x`), rooted paths without a drive,
  UNC shares with and without a trailing separator, a server with no
  share, `\\?\` extended paths, `\\?\UNC\`, device paths, a colon in a
  name (`./c:x`) and backslashes in a POSIX name.
- **operations**: 111 calls covering:
  - joins with `posixpath.join` and `ntpath.join` rules: absolute
    segments, other drives, the same drive in another case, and UNC
    separators;
  - `with_name`, `with_suffix` and `with_stem` with each `ValueError`;
  - `relative_to` with and without `walk_up`, including the "not in the
    subpath", "different anchors" and "'..' segment" errors;
  - `is_relative_to`, `match` from the right, and `full_match` with
    `**`, `*`, `?` and `[...]` classes, case-blind on Windows;
  - equality and ordering, which compares segment by segment, so
    `a/b` sorts before `a-b`;
  - `as_uri` for POSIX, drive and UNC paths.
- **tree**: a walk over the 30-file Python project in `data/tree`,
  sorted the way pathlib sorts. Then:
  - include and exclude rules in `data/rules.txt` select files for a
    source distribution. Each file is decided by `full_match`, and the
    last matching rule wins. Each line names the rule that decided it.
  - the selected files are counted by their joined suffixes.
  - each package module is mapped to its Windows install path, its file
    URI and its `.pyc` path relative to the package (walking up).
  - each C source is mapped to its object file and the relative way
    back.

Not supported: the concrete `Path` class beyond reading a directory
(no `glob`, `resolve` or `stat` fields), `os.path.normpath`, and non-ASCII
case folding on Windows. The packaging rules are the program's own and
are the same in both versions.

**Oracle.** `expected.out` is the output of `oracle.py`, the same
program written with `pathlib`. Run it from this directory after
`mk.py` writes `data/`.

## Edits from mini_pathlib.orig.kara to mini_pathlib.kara

Compiler gaps: none found.

Author errors:
- `from_parts` stores its `tail` parameter in the struct it returns,
  so the parameter has to be `own Vec[String]`. A bare parameter
  borrows.

After that, the output matched `expected.out` on `__mir-run`,
`__mir-llvm-run` and `__mir-llvm-asan`.

Written 2026-10-15 by the real-world apps thread; karac at eb605b2e5b.
