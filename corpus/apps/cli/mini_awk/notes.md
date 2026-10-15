# mini_awk

An awk for the one-liners and small report scripts that ops work runs
over logs and system output, answering what mawk 1.3.4 answers. The run
writes the input files into `data/` and a `jobs.tsv` of 67 awk
invocations. Each line holds one job's tab-separated arguments, with a
newline inside an argument written as a vertical tab. The run then runs
each job and prints what awk prints: a `$ awk ...` line, stdout, then
stderr, and `[exit N]` when the status is not 0. A job `@cat FILE` shows
a file an earlier job wrote with `print > FILE`.

The inputs:
- an access log with response times;
- `/etc/passwd`;
- a sales CSV with an empty field;
- a users and an orders TSV, for a join;
- `ps aux` and `df` output;
- a contacts file of blank-line-separated records;
- an application log with stack traces;
- a `;`-separated connection string;
- a file of numbers and number-like text;
- a report script for `-f`.

The jobs do what such scripts do:
- select and print fields with `-F`, patterns, `NR`/`FNR`/`NF`/`$NF`
  and `$(NF-2)`, and ranges (`NR==2, NR==4`, `/ERROR/, /^$/`);
- sum, average and count; de-duplicate with `!seen[$9]++`; group by
  status class in first-seen order; and rank the busiest clients with a
  user insertion sort, since awk has no sort;
- join two files with `NR == FNR { ...; next }`, and total by role;
- report revenue per region with `printf "%-6s %9.2f"`, flag missing
  values, and show percentage shares;
- pick busy processes out of `ps` and full disks out of `df`;
- read paragraph-mode records (`RS = ""`), `;`-separated records and
  regex field separators (`-F[:,]`, `split` on `/[\/:\[]/`);
- assign fields and `NF`, and rebuild `$0` with `OFS`;
- mask IP addresses with `gsub`, pull durations out with `match`,
  `RSTART` and `RLENGTH`, and test `sub`/`gsub`'s `&`, `\\&` and
  empty-match rules;
- test awk's number rules: integers print as integers, others by
  `CONVFMT` and `OFMT`, `printf` with every conversion and flag, and
  fields that look like numbers compare as numbers;
- use functions with recursion, local variables and arrays passed by
  reference; `while`, `do`, `break`, `continue` and `exit` with a code;
- read with `getline` from a file and from the main input, write
  per-status files with `print >` and `>>`, `close` them, and write to
  `/dev/stderr`;
- make five errors: a syntax error, an unclosed `{`, a call to an
  undefined function, a missing input file, and a bad regex.

**The regular expressions** are POSIX extended, with intervals and
bracket classes, matched leftmost-longest by an NFA simulation, as mawk
matches them.

**The interpreter** lexes and parses awk's grammar into a tree (with
awk's rules for where `/` starts a regex, where a newline ends a
statement, concatenation, and `>` in `print` being a redirection). It
runs the tree over records. Values are awk's four kinds:
uninitialized, number, string, and numeric string. printf's
floating-point conversions round exactly from the double's decimal
expansion, as glibc does. mawk's error messages are kept.

Not supported:
- pipes (`print | cmd`, `cmd | getline`), `system`, `rand` and `srand`,
  `ENVIRON`'s contents, and `nextfile`;
- a regular-expression `RS` longer than one character (it is matched
  literally);
- reading stdin;
- mawk's own rules for `substr` with a start below 1 (these jobs don't
  use them);
- multibyte characters (strings are bytes, as in the C locale).

`for (k in a)` visits keys in insertion order, not mawk's hash order, so
no job prints in that order.

**Oracle.** `expected.out` is mawk 1.3.4 20240123's output on the same
files, made by `oracle.py`, which is run from this directory after
`mk.py` writes `data/` and `jobs.tsv`. The files the jobs write into
`data/` are byte-identical to the ones mawk writes.

## Edits from mini_awk.orig.kara to mini_awk.kara

Compiler gaps, worked around:
- B-2026-10-14-70: `karac check` refuses chained field receivers like
  `self.prog.exprs[e]` (75 of them here), a legacy-codegen deferral that
  the v2 surfaces don't need. The interpreter holds the program's
  tables as its own fields instead of a `prog` field.
- The parser struct is `AwkParser`, not `Parser`. `karac check` warns
  that `Parser` shadows a prelude type, and B-2026-10-15-8 and
  B-2026-10-15-41 show that such a shadow can break library code on v2.

There were no author errors. The program matched expected.out on the
first run on all three MIR surfaces.

Written 2026-10-15 by the real-world apps thread; karac at 72cc0d4a00.
