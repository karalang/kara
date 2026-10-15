# mini_textutils

The text tools that shell pipelines in deploy scripts, cron jobs and log
triage are built from, as one multi-call program. It answers what GNU
coreutils 9.4 answers in the C locale. The tools are `sort`, `cut`,
`uniq`, `tr`, `wc`, `head`, `tail`, `join`, `comm`, `paste`, `nl`,
`tac`, `numfmt` and `cat`.

The run writes the input files into `data/` and a `jobs.tsv` of 83
pipelines. Each line holds one pipeline's tab-separated arguments, with
a tab inside an argument written as a vertical tab and the stages
separated by a `|` argument. The run then runs each pipeline, feeding
each stage's output to the next, and prints what a shell prints: a
`$ ...` line, the last stage's stdout, every stage's stderr in order,
and `[exit N]` when the last stage's status is not 0.

The inputs:
- `/etc/passwd`;
- an access log;
- an inventory CSV;
- `du -h` output and a list of file sizes;
- a text with mixed case, extra spaces and a blank line;
- two host lists, for `comm`;
- a users and an orders TSV, for `join`, and an unsorted one;
- a log with repeated lines;
- version strings, numbers and number-like text;
- a file with no final newline.

The pipelines do what such scripts do:
- `sort` by keys (`-t: -k3,3n`, `-k7,7 -k1,1r`, `-k1.2,1.4`, `-k4,4nr`),
  numerically, by human sizes, folding case, uniquely, stably, ignoring
  leading blanks; check order with `-c`; and merge with `-m`;
- `cut` fields and character ranges, with `--output-delimiter`,
  `--complement` and `-s`;
- count the busiest clients with `cut | sort | uniq -c | sort -rn`;
- `uniq` with `-c`, `-d`, `-u`, `-i`, `-f` and `-s`;
- `tr` with ranges, classes, `-d`, `-s`, `-c` and escapes, including a
  word-frequency pipeline;
- `wc` with its column widths, `head` and `tail` by lines and bytes from
  either end, with file headers;
- `join` with `-a`, `-e`, `-o`, `-v` and `-1`/`-2`; `comm` with its
  column options; `paste` in parallel and serial modes with delimiter
  lists;
- `nl` with its numbering styles, `tac`, and `numfmt` to and from `iec`
  and `si` units with `--field`, `--suffix` and `--padding`;
- make nine errors: two `cut` usage errors, two `tr` errors, a missing
  file for `wc`, an invalid count for `head`, an invalid number for
  `numfmt`, a disordered file for `sort -c`, and an unsorted file for
  `join` (reported after the lines `join` can still pair).

**Options** follow GNU's syntax: clustered short options, a value
attached or in the next argument, `--long=value`, and options after the
operands.

**`sort`** finds key fields the way GNU does. A field without `-t`
starts at the blanks before it unless `b` is given. A key without its
own ordering letters takes the global ones. Lines whose keys compare
equal fall back to a whole-line byte comparison, which `-s` and `-u` turn
off. The merge sort is stable.

**`numfmt`** rounds away from zero, as GNU's default does. It shows one
decimal below 10 and moves to the next unit when rounding reaches the
base.

**`join`** checks order as GNU does by default. It reports an unsorted
file once, and only after it has seen an unpairable line.

Not supported:
- `sort`'s `-g`, `-M`, `-V`, `-R`, `-d`, `-i` and `-o`;
- `tr`'s `[c*n]` repeats and equivalence classes;
- `cut`'s `-n`;
- `nl`'s section delimiters and its `-b p` regular expressions;
- `numfmt`'s rounding modes and `--format`;
- multibyte characters (strings are bytes, as in the C locale).

`sort -m` sorts its inputs together rather than merging them, which is
the same when they are sorted.

**Oracle.** `expected.out` is GNU coreutils 9.4's output on the same
files, made by `oracle.py`, which is run from this directory after
`mk.py` writes `data/` and `jobs.tsv`.

## Edits from mini_textutils.orig.kara to mini_textutils.kara

Author errors:
- `let (sf, mut i) = ...` is Rust's way to make one binding of a pattern
  mutable. In Kāra `mut` goes before the whole pattern, as
  `let mut (sf, i) = ...`; `karac check` says so. This happened twice.

There were no compiler gaps. The program matched expected.out on the
first run on all three MIR surfaces.

Written 2026-10-15 by the real-world apps thread; karac at 72cc0d4a00.
