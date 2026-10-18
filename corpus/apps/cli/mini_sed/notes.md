# mini_sed

A stream editor for a subset of GNU sed 4.9, run the way deploy scripts
and log pipelines run sed. The run writes eight input files into `data/`
and a `jobs.tsv` of 68 sed invocations (one per line, tab-separated
arguments). It then runs each job and prints what sed prints: a `$ sed
...` line, stdout, then stderr, and `[exit N]` when the status is not 0.
A job `@cat FILE` shows a file an earlier job wrote with `w` or `-i`.

The inputs:
- an nginx access log;
- a staging `/etc/hosts`;
- an INI service config;
- a `.env` file with no final newline;
- a sales CSV;
- a CHANGELOG;
- a word list;
- a footer to insert with `r`.

The jobs do what such scripts do:
- print, count and delete lines by number, by `$`, by `/re/` (with `I`)
  and by range (`/a/,/b/`, `addr,+N`, `0,/re/`, `first~step`, `!`);
- mask IP addresses and secrets, and reformat log lines into reports with
  `-E` groups;
- set config values in place (`-i`), with `w` writing the 5xx lines to a
  file;
- export `.env` lines as shell, commify numbers with a `:a ... ta` loop,
  convert case with `\U \L \u`, and link issue numbers in Markdown;
- reverse, join, swap and de-duplicate lines with the hold space and `N`,
  `P`, `D`;
- insert, append and change text (`a`, `i`, `c`, one-liner and `\`
  forms), and append a file with `r`;
- show the missing-newline handling, `-s` against one stream, `q` and
  `Q` with exit codes, and the empty regex (`s//X/`);
- make 11 errors: unterminated `s`, an unknown command, an unknown `s`
  option, an invalid `\2`, an undefined label (exit 4), an unclosed `{`,
  a stray `}`, a missing input file (exit 2, the other file still runs),
  a missing address, extra characters, and an unmatched `(`.

**The regular expressions** are POSIX basic and extended, with GNU's
extensions (`\+ \? \|` in basic, `\w \W \s \S \b \B \< \>`, `\n \t`,
back-references in both). A pattern is parsed to a tree and compiled to
a small instruction set. Matching is POSIX leftmost-longest, as glibc's
is: an NFA simulation finds the leftmost start and the longest end there
(`s/(an|ana)+/<&>/` gives `b<anana>`, where a backtracking matcher would
give `b<anan>a`). A backtracking pass then fills the groups for that
extent, taking the first assignment in the pattern's order. GNU sed's
`s///g` rules for empty matches are kept, including skipping an empty
match right after the previous one (`s/x*/-/g`).

**The editor** follows GNU sed's parser (its error messages and `char N`
positions) and its executor: range state per command, the append queue
flushed at the end of a cycle and before `n`/`N` read a line, `D`
restarting without a read, `n`/`N` at the last line printing and
exiting, `c` writing its text once at the end of a range, and a missing
final newline kept in the output and added back before anything written
after it.

Not supported:
- POSIX's rule for which groups a match assigns when two assignments
  give the same extent, and leftmost-longest for patterns with a
  back-reference (these are matched by backtracking alone);
- the commands `l e F v W` and the `s` flags `e` and `m`;
- `--posix`, `-z`, `-u`, `--debug`, combined short options like `-ne`,
  `-i` with a suffix, and reading stdin;
- equivalence classes and collating symbols in brackets;
- multibyte locales (the matcher works on bytes, as in the C locale).

**Oracle.** `expected.out` is GNU sed 4.9's output on the same files,
made by `oracle.py`, which is run from this directory after `mk.py`
writes `data/` and `jobs.tsv`. The `data/` files, `jobs.tsv`,
`errors.log` and the edited `app.env` that the run writes are
byte-identical to the ones sed leaves.

## Edits from mini_sed.orig.kara to mini_sed.kara

Author errors (the diagnostics were right):
- The replacement's case-conversion helper was also named `put`, the
  name of the file writer. It is `put_case`.
- Three methods that store their argument (`add`, `push`, `wfile`) had
  bare parameters, which borrow since D5. They are `own`.

`Out` once had a `copy()` method so `r.out.text.clone()` would not be
written, a workaround for B-2026-10-14-70. It was dropped after the fix
in 0bacd9912b.

Compiler gaps, worked around:
- The `Regex` struct is `Pattern`. `karac check` warns that it shadows
  the prelude `Regex`, and B-2026-10-15-8 and B-2026-10-15-41 show that
  such a shadow can break library code on v2.

Written 2026-10-15 by the real-world apps thread; karac at 72cc0d4a00.

## Library migration, 2026-10-18

Thread A's a0ca700228 and acd593fc77 changed the library to match docs/library: a number's `parse` returns `Result[T, ParseError]`, and `split`, `lines`, `split_whitespace` and `trim` give `Str` views (`split` and `lines` are now lazy iterators). Arguments forwarded to a `mut ref` parameter are now marked too. `karac fix` made the edits. The app matches expected.out on all four MIR backends again (karac at a4b0967495).
