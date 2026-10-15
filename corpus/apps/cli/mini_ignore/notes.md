# mini_ignore

Which files a repository ignores, decided the way git decides it. Every
tool that walks a working tree needs this: search tools, file watchers,
build systems, linters, backup and sync tools. The program ports git's
`wildmatch.c` and the exclude rules of `dir.c`, and its answers are
byte-identical to git 2.43's.

The run writes a monorepo's working tree into `data/repo`: a C build at
the top, a Python package, a web package with its own `node_modules` and
`dist`, a Rust API, a legacy directory whose `.gitignore` was saved on
Windows, editor and OS files, logs, secrets and docs. It has four
`.gitignore` files and a `.git/info/exclude` (kept as
`data/exclude.txt`). It then prints:
- **untracked**: the files `git ls-files --others --exclude-standard`
  lists;
- **ignored**: the files `git ls-files --others --ignored
  --exclude-standard` lists;
- **check-ignore**: for every file and directory, what `git check-ignore
  -v --non-matching` prints: the file, line and pattern that decided it,
  or `::` when nothing matched.

**wildmatch** is a line-by-line port of git's `dowild`: `?`, `*` (which
stops at a slash in a path), `**` at a directory boundary (`**/x`,
`a/**/b`, `abc/**`) and inside a name (where it acts as `*`), the abort
codes that keep it from backtracking forever, character classes with
ranges, `!` and `^` negation, escapes and the POSIX names such as
`[[:upper:]]`.

**Pattern files** are read as `add_patterns_from_buffer` reads them: a
`#` line is a comment and `\#` is not, `\!` is a literal `!`, unescaped
trailing spaces are dropped and an escaped one is kept, a `\r` before
the newline is dropped, and a last line with no newline still counts. A
leading `!` negates, a trailing `/` matches only directories, and a
slash anywhere else anchors the pattern to its file's directory; without
one it matches the basename at any depth.

**The rules**: for each directory on the way down to a path, the
directory itself is checked first against the files read so far. If it
is excluded, that decides everything under it, which is why
`!dist/keep.txt` and `!build/a.txt` can't bring a file back. Otherwise
its `.gitignore` is pushed. A path is then checked against the
`.gitignore` files from the deepest up, and then `info/exclude`. The
last matching line of a file wins.

Not supported: `core.excludesFile`, case-insensitive matching
(`core.ignoreCase`), tracked files (an index), and quoting of unusual
path names in the output.

**Oracle.** `expected.out` is the output of `oracle.py`, which copies
`data/repo` into a fresh repository, installs the exclude file, and runs
the three git commands. Run it from this directory after `mk.py` writes
`data/`.

## Edits from mini_ignore.orig.kara to mini_ignore.kara

One, for an existing row: `karac check` called the path passed to
`fs.read_dir(full)` moved, though the library borrows it, so the error
message after it could not name the path (B-2026-10-14-53). It is now
`fs.read_dir(full.clone())`. All MIR surfaces run the original.

After that `karac check` passed, and the program matched expected.out
on the first run on all three MIR surfaces.

Written 2026-10-15 by the real-world apps thread; karac at 35fcd8197c.
