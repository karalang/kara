# mini_dbm

Python 3.13's `dbm.dumb`, ported from `Lib/dbm/dumb.py`, with
`dbm.whichdb` from `Lib/dbm/__init__.py`. `dbm.dumb` is the key-value
store that is always there when no other dbm is built in. It is also the
store `shelve` falls back to, and the plainest example of an on-disk
index with its crash-safety trade-offs.

A database is two files. The `.dat` file holds the values, each one
starting on a 512-byte block. The `.dir` text file has one line per key,
`repr(key), (pos, size)`, written in Latin-1. The index lives in memory:
- a new key appends its value to `.dat` and a line to `.dir` at once;
- an update that fits in the old value's blocks is written in place;
- a larger update is appended, and its old blocks are lost.

`.dir` only learns of an update when the database commits. That happens
on `sync`, `close` or any delete, and the old `.dir` is moved to `.bak`
first.

The port covers:
- `dumb.open` with the flags `r`, `w`, `c` and `n`, and its errors. A
  missing file is reported under its bytes name, and the reading-only and
  already-closed errors are kept;
- get, set, delete, `in`, `len`, `keys`, `items`, `sync` and `close`,
  over str, bytes and int keys, as `dumb.py` handles each;
- reading `.dir` lines the way `ast.literal_eval` reads that shape: str
  literals with every escape (`\x`, octal, `\u`, `\U`, named), tuples of
  ints, the `UnicodeEncodeError` for a key that is not Latin-1, and the
  unpack error for a line with three values;
- `whichdb` for dumb, ndbm, GNU dbm and SQLite files.

`mk.py` writes `data/`. It holds `jobs.tsv` with seven jobs and 13
`whichdb` calls, hand-made `.dir` and `.dat` files (escapes, a non-Latin-1
key, a three-value line, a damaged index), and the inputs of five
programs:
- a URL shortener that survives a restart;
- a session store with expiry sweeps;
- an HTTP response cache, then a compaction into a fresh database;
- a crash between updates, read back from a snapshot of the files;
- an index checker that finds misaligned, overlapping and truncated
  entries.

The files under `data/bin/` hold magic numbers that are not UTF-8, so
`mini_dbm.kara` writes them itself and `gen_kara.py` leaves them out.
`oracle.py` runs the same jobs through the real modules and wrote
`expected.out`.

The quirks are kept. Some examples:
- a UTF-8 key is written to `.dir` decoded as Latin-1, so `café` appears
  there as `'cafÃ©'`, and the file reads as UTF-8 again byte for byte;
- after an update in place, `.dir` still has the old size until a
  commit. The crash snapshot therefore reads the first 100 bytes of a
  400-byte value, and the moved value comes back as the old one;
- a delete commits, so every delete rewrites `.dir` and replaces `.bak`;
- an index entry past the end of `.dat` reads short with no error;
- `5 in db` is `False`, but `db[5] = 'v'` is a `TypeError`, and on a
  read-only database the reading-only error wins over both;
- `whichdb` calls a `.dir` that does not start with a quote "not dumb",
  then finds no file by the bare name and answers `None`.

It matched on every backend with no workaround in the port. One
diagnostic gap turned up: `#[allow(mutual_recursion_note)]` on an impl
method does not silence the note, though it does on free functions
(B-2026-10-20-28).
