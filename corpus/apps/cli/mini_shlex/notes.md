# mini_shlex

Python 3.13's `shlex`, `fnmatch` and `glob`, ported from
`Lib/shlex.py`, `Lib/fnmatch.py` and `Lib/glob.py`: shell-style
splitting with POSIX and non-POSIX rules, the `shlex` lexer with
`punctuation_chars` and `whitespace_split`, `quote` and `join`,
`fnmatch` and `filter`, and `glob` with `recursive`,
`include_hidden`, `root_dir` and `escape`.

The run writes a small project tree into `data/tree` and runs
`data/jobs.tsv`, 144 jobs with tab-separated fields. In a field, `⏎`
stands for a newline and `⇥` for a tab. Each job prints `== ` and its
fields joined by ` | `, then its result.

The tree has hidden files and directories, directories named `a*b` and
`q?`, a file named `sample[1].json`, a directory and file with
non-ASCII names, an empty directory, and two files whose names differ
only in case.

The jobs cover:
- `split` with and without POSIX rules and comments: quotes and their
  escapes, joined quoted parts, empty quotes, comments, newlines, a
  trailing backslash, unterminated quotes (and their `ValueError`),
  blank input, non-ASCII words and real command lines;
- `lex`: the token stream with `posix`, `whitespace_split`,
  `punctuation_chars` (on, or given as a string) and comments off;
- `quote` and `join`, including the empty string, a quote inside a
  word and a tab;
- `fnmatch` (36 cases: `*`, `?`, sets, negated sets, ranges,
  reversed and overlapping ranges, `[` with no close, a `-` at either
  end of a set, case sensitivity) and `filter`;
- `glob` (43 cases), with and without `recursive` and
  `include_hidden`: `**` alone, as a directory and inside a path,
  trailing slashes, literal paths that do and do not exist, bracketed
  magic characters, and hidden directories in the middle of a pattern;
- `escape`.

**The port** follows the three modules function by function:
`shlex.read_token` with its states and pushback, `split`, `quote`'s
safe-character test and `join`; `fnmatch.translate`'s chunk logic for
sets (with the merge of overlapping ranges and the removal of empty
ones), matched by a backtracking matcher rather than a regex; and
`glob`'s `_iglob`, `_glob0`, `_glob1`, `_glob2` and `_rlistdir`,
with `os.path.split` and `join` for POSIX paths. `glob` prints its
paths sorted, since `os.scandir` order is unspecified.

Not supported:
- `shlex`'s file input, `sourcehook`, `push_source` and `debug`;
- `fnmatch`'s case folding on Windows and its regex cache;
- `glob`'s `dir_fd` and `iglob` as an iterator.

**Oracle.** `expected.out` is Python 3.13.16's output for the same jobs,
made by `oracle.py`. Run `mk.py` first; it writes `data/`.
`gen_kara.py` prints the Kāra data section from the same files.

## Edits from mini_shlex.orig.kara to mini_shlex.kara

Author errors:
- The tree writer passed `dir` to `fs.create_dir_all`, which takes it
  by value, and then printed it in the error message. It passes a clone.

Compiler gaps:
- B-2026-10-15-15: `String` has no `rfind`. `split_path` finds the
  last `/` with a small `last_slash` over the bytes.
- B-2026-10-18-14: a range view in a `for` header, `for x in
  chunks[k2][1..].iter()`, stays borrowed after the loop, so `karac
  check` refuses the `chunks.remove(k2)` after it. The range merge
  copies by index instead.

After those edits the program matched expected.out on both MIR
surfaces. `karac fix` found nothing to fix.

Written 2026-10-18 by the real-world apps thread; karac at 9b85ca7828.

## Library migration, 2026-10-18

Thread A's a0ca700228 and acd593fc77 changed the library to match docs/library: a number's `parse` returns `Result[T, ParseError]`, and `split`, `lines`, `split_whitespace` and `trim` give `Str` views (`split` and `lines` are now lazy iterators). Arguments forwarded to a `mut ref` parameter are now marked too. `karac fix` made the edits, with one more by hand: `args[0].split(",").filter(..).collect()` into `Vec[String]` gained `.map(|o| o.to_string())` by hand (B-2026-10-18-35). The app matches expected.out on all four MIR backends again (karac at a4b0967495).
