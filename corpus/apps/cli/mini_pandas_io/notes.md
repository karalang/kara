# mini_pandas_io

A Kāra port of the part of [pandas](https://pandas.pydata.org/) 3.0.5
(BSD-3-Clause) that reads other systems' exports and writes them back
out: `read_csv`'s C parser with the options an ETL job reaches for,
`to_csv` through Python's `csv` writer with every quoting mode, and the
cleaning steps between them. Like [mini_pandas](../mini_pandas/README.md)
it runs pipelines in a small job language, one step per ` | `:

```
read statement.csv sep=; skiprows=3 decimal=, thousands=. | where Betrag < 0 | agg Währung n=Betrag:count,ausgaben=Betrag:sum
read sensors.tsv sep=\t na=-9999,ERR | ffill temp_c by=device limit=1
read feed.csv on_bad_lines=skip | write quoting=3 escapechar=\
```

| Group | Steps (the pandas call each one is) |
|---|---|
| Reading | `read` (`read_csv` with `sep`, including `\s+`; `header=None`, `names`, `skiprows` as a count or a list, `nrows`, `comment`, `quotechar`, `escapechar`, `doublequote`, `skipinitialspace`, `quoting=3`, `na_values`, `keep_default_na`, `true_values`, `false_values`, `thousands`, `decimal`, `usecols`, `dtype`, `on_bad_lines`, `skip_blank_lines`) |
| Looking | `dtypes`, `head`, `select`, `where` |
| Gaps | `fillna`, `ffill` and `bfill` (with `limit`, and per group through `groupby(...).ffill`), `dropna` (`how`, `thresh`, `subset`) |
| Series | `shift`, `diff` (`periods`), `pct_change`, `cumsum`, `cumprod`, `cummax`, `cummin`, `clip`, `round`, `replace`, `astype`, `str.strip` |
| Frames | `rename`, `drop_duplicates` (`subset`, `keep` first, last or `False`), `agg` (named aggregation) |
| Writing | `write` (`to_csv(index=False)` with `sep`, `na_rep`, `float_format` `%.Nf`, `%.Ne` or `%.Ng`, `quoting` 0 to 5, `quotechar`, `escapechar`, `doublequote`, `decimal`, `header`, `columns`); a job with no `write` step ends in plain `to_csv(index=False)` |

In a text argument `\s` stands for a space, `\t` for a tab and `\e` for
the empty string, since the job language splits on spaces; `sep=ws` is
`sep=r"\s+"`.

## Running it

The program carries its inputs: it writes `data/` and runs
`data/jobs.txt`, 120 jobs. Each file is shaped like a real export:

- a German bank's statement (three lines of account details above the
  header, `;` between fields, decimal commas, `.` between thousands, a
  quoted purpose holding a `;`, blank lines at the end);
- a mainframe extract (`|`, no header, comment lines, fields padded with
  spaces, account ids with leading zeros, `-9999` and `n.a.` for
  missing, and a product name with a `#` in it);
- a building's sensor log as TSV (three devices interleaved, readings
  the network dropped, the device's own `-9999`, an `ERR` code);
- a product feed (commas, doubled quotes and a line break inside quoted
  fields, one row with an unquoted comma, one row cut short), and the
  same feed from a system that escapes quotes with a backslash;
- a survey tool's export (yes/no in three spellings, booleans in any
  case, a question exported twice under one name, an unnamed column,
  blank lines and a line of spaces);
- a report with columns lined up by spaces;
- Excel's "CSV UTF-8" (a byte-order mark and CRLF) and Excel for Mac's
  old "CSV (Macintosh)" (bare CR line ends);
- ids and counts at the edges of 64-bit integers, quoted, padded, signed
  and with thousands separators;
- a download cut off inside an open quote.

Each job prints `$ <job>`, then any `ParserWarning` line after `~ `,
then the result as `to_csv` writes it, or `! ` and the exception pandas
raised (its trailing newline dropped).

```
python3 -s -P -B mk.py          # writes data/ and data/jobs.txt
python3 -I -B gen_kara.py       # copies data/ into the end of mini_pandas_io.kara
python3 -I -B oracle.py > expected.out
karac run mini_pandas_io.kara | diff - expected.out
```

`oracle.py` is the oracle, and it is pandas itself (pandas 3.0.5 on
numpy, without bottleneck or pyarrow, as for mini_pandas).

## Parallel work

Every job runs in its own `TaskGroup` task, and each `agg` aggregate is
a task too. The results join in order, so the transcript is the same on
every run and thread count. The program matches `expected.out` on
`karac run`, `__mir-llvm-run`, `__mir-llvm-asan` (no reports) and a
`karac build` binary, at `KARAC_THREADS=1` and `4`, and on five runs of
an auto-par build at `KARAC_THREADS=4`.

## pandas' own code, ported

- **tokenize_bytes** (`tokenizer.c`), the C parser's state machine,
  state for state: the skipped-row states, the line of blanks it backs
  up over, the fall-throughs from one case into the next, the bare-CR
  line ends it reads a character twice for, and `end_line`, which
  compares each line with the one before it, exempts the header and the
  first data line, pads short lines and turns long ones into an error,
  a warning or nothing, unless `usecols` is given. The quirks a user
  meets all come from here, and the jobs show them: with
  `thousands="."` the statement's dates `01.09.2026` read as the integer
  `1092026`; the `#` in `WIDGETS #2 SUPPLY` ends that line when
  `comment="#"`; a first data line with more fields than the header
  makes the extra leading fields the index (so `quoting=3` on the feed
  shifts every column); a `-9999` padded with spaces is not the missing
  marker `-9999`; and a record count in an error message counts records,
  not physical lines.
- **The number readers**: `precise_xstrtod` with the decimal and
  thousands characters (a separator is skipped after any digit of the
  whole part, so `1.2.3` is `123`), `str_to_int64` and `str_to_uint64`
  (strtoll's range, and its overflow that counts only when nothing
  follows the digits), and `to_boolean`.
- **The TextReader's type rules**: int64, then uint64 after an
  overflow (text again when a minus sign or a gap sits beside a value
  past i64, and `object` past uint64), then float64, then bool from
  `true_values`, `false_values`, the default spellings and any case of
  TRUE and FALSE (an `object` column when a gap is present), else text;
  `Unnamed: N` for an empty header and `dedup_names` for a repeated one.
- **get_values_for_csv and the csv writer** (`_csv.c`'s
  `join_append`): which values reach the writer as numbers (ints,
  bools, and floats unless `float_format` or a decimal comma made them
  strings), which characters force quoting or need an escape, the
  `need to escape, but no escapechar set` error, and the quoted empty
  record. `QUOTE_NONNUMERIC` therefore quotes a float column once it has
  a `float_format`, and quotes every gap.
- **printf's `%f`, `%e` and `%g`** on the double's exact decimal value,
  rounded half to even, since Kāra's format specifiers have only `x`,
  `X`, `o`, `b` and `d` (design.md §3): `0.125` at `%.2f` is `0.12`.

## Where it differs from pandas, on purpose

- **No index.** Results are written with `index=False`, so an implicit
  index (above) is left out of the output, as pandas leaves it out.
- **Reading stops at the line limit.** With `nrows`, the tokenizer stops
  after the header and `nrows` lines, as pandas' does; pandas may read a
  line further to look for an implicit index, which no job can see.
- **`object` cells are text.** A bool-with-gaps column and an
  integer-past-uint64 column hold the text pandas writes for each value;
  no job writes one under `QUOTE_NONNUMERIC`, where pandas would leave
  those cells unquoted.

## The Mend loop

The first `karac check` stopped at one parse error, a Rust habit:
`let (mut cur, warnings) = ...`, which the diagnostic explained and
`karac fix` rewrote as `let mut (cur, warnings) = ...`. The second run
of `karac fix` applied five more: two parameters that hand a state on
(`next: own St`), two `.clone()`s before `Option.map` on borrowed
parameters, and a `**j` that should be `*j`. One error was left: a
borrowed parameter put in an array literal, `[key].to_vec()` where a
`Vec[String]` is wanted, got only `expected 'ref Vec[String]', found
'Vec[ref String]'`, with no hint and no fix (B-2026-10-29-4); the app writes
`[key.clone()]`. The first run of the checked program then matched
pandas on every job. A warning that the struct `Args` shadows a prelude
type is gone after a rename to `StepArgs`. `mini_pandas_io.orig.kara` is
the program as first written.

## Gaps found

- **B-2026-10-29-4** (open): a borrowed parameter or pattern binding put in an
  array literal (`count([key].to_vec())` with `key: String`) is refused
  with only the two types; the same through a `ref` binding gets
  `borrow_projection_copy`'s message with the `.clone()` it needs.

## What reading and writing would need (notes for M4c)

Compared with `docs/library/data.md` in the kara repo, whose CSV
section is RFC 4180 with a delimiter, a header flag, one null marker
and type inference:

- **A schema argument.** Decision Q13 names it as the next option ETL
  needs, and every file here shows why: zip codes and account ids lose
  their leading zeros, dates become integers under a thousands
  separator, and one stray value turns a column to text. pandas'
  `dtype=` per column is the shape; a typed library can make it the
  default path rather than an override.
- **Dialect options.** Skipped preamble lines, comment lines, quote and
  escape characters, a decimal comma with a thousands separator (most of
  Europe), several missing markers (`-9999`, `n.a.`, `ERR`), and words
  for true and false. Each one here is a real export's convention.
  The page should also say what a comment character does mid-field.
- **Bad lines.** A choice of error, skip or warn, with the record
  number, and what a line with fewer fields means (pandas pads it). The
  page should not copy pandas' implicit index: a line with one extra
  field silently shifting every column is the quirk users trip on most.
- **Line ends and the BOM.** CRLF, a bare CR and a UTF-8 byte-order mark
  all appear in files people are sent; the reader should take all three.
- **Writing.** `write_csv` with quoting modes (minimal, all, non-numeric,
  none with an escape character), a missing-value text, a float format
  and a decimal comma. The float format needs an `e` or `g` form, which
  Kāra's format specifiers lack.
- **The cleaning steps.** `ffill` and `bfill` with a limit and per
  group, `dropna` with `how`, `thresh` and `subset`, `shift`, `diff`,
  `pct_change`, the cumulative reductions, `clip`, `round` (to even, as
  numpy rounds) and `drop_duplicates`. `data.md` has `fillna` and
  `dropna` without options; the rest are not listed.
