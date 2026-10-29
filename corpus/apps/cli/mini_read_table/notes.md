# mini_read_table

A Kāra port of [pandas](https://pandas.pydata.org/) 3.0.5's `read_csv`
and `read_table` with `engine="python"` (BSD-3-Clause), over a port of
CPython's `csv` reader and `csv.Sniffer` (PSF licence). The python
engine is the one pandas falls back to for everything the C engine
cannot do: separators longer than one character, which it treats as
regular expressions, `sep=None` with a sniffed delimiter, and
`skipfooter`. That makes it the reader for the files people actually
have open: MovieLens's `::` files, `ls -l` and `vmstat` output split on
`\s+`, a `psql` table pasted from a terminal, a MySQL `SELECT ... INTO
OUTFILE` dump with `\N` and backslash escapes, a German Excel export in
windows-1252 with `;` and decimal commas. Jobs come one per line:

```
read ratings.dat sep=:: header=None names=user,movie,rating,ts
read psql.txt sep=\s*\|\s* skiprows=[1] skipfooter=2 true_values=t false_values=f
read mysql.tsv sep={tab} header=None escapechar=\ na_values=\N
read feedback.csv on_bad_lines=warn
read umsatz.csv sep=None encoding=cp1252
```

A `read` job is `pd.read_csv("data/<file>", engine="python", ...)`, or
`pd.read_table` with `fn=table`, with `sep`, `delimiter`, `dialect`,
`quotechar`, `escapechar`, `doublequote`, `skipinitialspace`, `quoting`,
`header`, `names`, `usecols`, `index_col` (including `False`),
`skiprows`, `skipfooter`, `nrows`, `chunksize`, `comment`, `thousands`,
`decimal`, `true_values`, `false_values`, `encoding`,
`encoding_errors`, `on_bad_lines`, `na_values`, `keep_default_na`,
`na_filter`, `skip_blank_lines`, `dtype` and `dtype_backend`. `{sp}` in
a value is a space and `{tab}` a tab. It prints each warning as
`warn: <message>`, then the shape, the dtypes and `to_csv`; with
`chunksize` it prints each chunk and `done`.

## Running it

The program carries its inputs: it writes `data/` and runs
`data/jobs.txt`, 142 jobs over 27 files that `mk.py` writes:

- **ratings.dat** and **movies.dat**, MovieLens 1M's `::` layout; the
  movie titles are ISO-8859-1 (`Mon Café à Montréal`).
- **ls.txt** (`ls -l` with file names that contain spaces), **vmstat.txt**
  (its two-line header) and **psql.txt** (a `psql` table with its rule
  and `(4 rows)` footer); **services.md**, a Markdown table.
- **mysql.tsv**, a MySQL dump: `\N` for NULL, `\t` inside a field, a
  backslash-escaped newline and doubled backslashes in a Windows path.
- **umsatz.csv** (windows-1252, `;`, `1.250,00`) and **orders.csv**
  (`;`, dates as `01.10.2026`), German exports; **win.csv**, curly quotes
  and an en dash in windows-1252.
- **survey.csv**, `, ` separators with single-quoted fields and
  `yes`/`no`/`Y`/`N` answers; **feedback.csv** and **feedback2.csv**,
  rows with a stray quote or extra fields; **claims.txt**, `|`-separated
  with inch marks (`27" monitor`); **trailing.csv**, a trailing comma on
  every data row; **roles.csv**, `C# developer` and a `#` comment.
- **cities.tsv**, **quoted.csv** (CRLF, a quoted newline), **mixed.csv**,
  **single.csv**, **onecol.txt**, **bom.csv**, **eof.csv** (no final
  newline), **cr.csv** (old Mac `\r`), **nul.csv**, **empty.csv**,
  **blank.csv** and a missing file.

Each job prints `$ <job>`, then what Python prints, or `! ` and the
exception (33 of the 142: `ParserError` for bad lines and header
shapes, csv's `Error` from the Sniffer, `TypeError` and `ValueError`
from csv's dialect validation, `PatternError` from a separator that is
not a valid regular expression, decode errors, an unknown codec or
error handler, an empty file, a missing file). 13 jobs print warnings.

```
python3 -s -P -B mk.py          # writes data/ and data/jobs.txt
python3 -I -B oracle.py > expected.out
python3 -I -B gen_kara.py       # copies data/ into the end of mini_read_table.kara
karac run mini_read_table.kara | diff - expected.out
```

`oracle.py` is the oracle: pandas 3.0.5 on Python 3.13.

## What pandas does, and so the port does

These are its answers, kept because `expected.out` is theirs:

- **`sep=None` sniffs one line.** The Sniffer sees only the first line,
  so `Datum;Kunde;Ort;Umsatz, netto;MwSt` sniffs `,` (the header's one
  comma) and the German file reads as two columns. A one-column file
  whose first value is `alpha` sniffs `a`. A byte-order mark is not
  stripped on this path, so the first column is `'﻿id'`; with
  `sep=;` it is `id`. An empty or blank file is `Error: Could not
  determine delimiter`.
- **A longer separator is a regular expression and ignores quoting.**
  `ls -l` split on `\s+` fails at the first file name with a space,
  `Expected 9 fields in line 6, saw 11`, and pandas adds "Error could
  possibly be due to quotes being ignored when a multi-char delimiter
  is used", though no quotes are involved. `vmstat` read without
  `skiprows` takes the dashed group line as the header and the other
  eleven columns become the index.
- **`comment` cuts anywhere**, inside a word included: `C# developer`
  reads as `C` and its row's other fields are lost.
- **`escapechar` runs before `na_values`.** With `escapechar=\` the
  MySQL `\N` becomes `N`, so `na_values=\N` never matches, `\t` becomes
  `t`, and the escaped newline joins two lines into one field (4 rows,
  not 5).
- **Bad lines are reported in two passes.** csv's own errors (`','
  expected after '"'`) are caught as each line is read; rows with too
  many fields are found afterwards. So `on_bad_lines=warn` on
  feedback.csv warns about line 5 before line 3.
- **A trailing comma makes the first column the index**: trailing.csv's
  `sku` becomes the index and `price` is all NaN, unless
  `index_col=False`. With `index_col=False` and extra fields pandas
  drops them with "This leads to a loss of data".
- **Dialects win.** `dialect=excel-tab sep=;` warns and keeps the tab;
  `dialect=unix` overrides `skipinitialspace=True` with a warning. A
  `sep` that matches is silent.
- **true_values join pandas' own.** `true_values=yes` alone leaves `Y`
  and `N` unmatched, so the column stays `str`; with NaN in it the column
  is `object`, or `boolean` under `numpy_nullable`.
- **encoding_errors** follow Python's codecs: `replace` gives `�`,
  `ignore` drops the bytes, `backslashreplace` writes `\x93`.

## Parallel work

Every job runs in its own `TaskGroup` task and the results join in
order, so the transcript is the same on every run and thread count.
The program matches all 1,300 lines of `expected.out` on `__mir-run`,
`__mir-llvm-run`, `__mir-llvm-asan` (no reports) and a `karac build`
binary, at `KARAC_THREADS=1` and `4`, and on five runs of the auto-par
build (0.01 s real).

## The code, ported

About 6,550 lines before the data section. About 1,450 are reused from
**mini_read_fwf** (the python engine's `PythonParser`, Python's number
parsing and decoders, pandas' column inference, nullable dtypes and
`to_csv`) and about 1,800 from **mini_read_html** (mini_re, a port of
Python's `re`, used here for regular-expression separators).

The new code is the rest: CPython's `csv` reader as a state machine
over code points, with its strict-mode errors and dialect validation;
`csv.Sniffer`'s quote-and-delimiter and frequency guesses; the three
readers `_make_reader` chooses between; pandas' dialect merge with its
warnings; `on_bad_lines` in both passes; `true_values` and
`false_values`; and Python's `replace`, `ignore` and `backslashreplace`
error handlers.

## The Mend loop

`karac check` reported two parse errors: `let (mut delim, _) = ...`,
Rust's placement of `mut`, which `karac fix` rewrote to `let mut
(delim, _) = ...`. Then a name defined twice (`parse`, also mini_re's)
and a helper left behind in mini_read_fwf, which I fixed by hand, and
eight `mut` markers missing at call sites that forward a `mut ref`
parameter, all applied by `karac fix`. The first run matched
`expected.out` on the interpreter.

## Gaps found

None. The `mut` placement and call-site markers already have
diagnostics with fixes.

## What reading delimited text would need (notes for M4c)

Compared with `docs/library/data.md` in the kara repo, whose `read_csv`
takes `delimiter: char`, `header`, `null_value` and `infer`, and treats
a row with the wrong number of fields as `Err(Csv { line, message })`:

- **Multi-character and whitespace separators.** `::`, `||` and runs of
  spaces are common; a `delimiter: char` cannot express them. A
  `Delimiter.Str(..)` and a `Delimiter.Whitespace` would cover them
  without a regular-expression engine, and without pandas' misleading
  error about quotes.
- **Bad lines as an option**: fail (today's rule), skip, or keep a list
  of the skipped lines with their line numbers. Real exports have them,
  and failing the whole read on one row is often not what people want.
- **Quoting options**: `quote` (or none), `escape`, and whether a doubled
  quote is a quote. MySQL dumps need `escape = '\\'` and `\N` as the
  null, and the escape must not run before null matching the way pandas
  runs it.
- **Sniffing, if any, should be visible.** pandas' one-line sniff read
  the German export as two columns and a one-column file as split on
  `a`. A `sniff(text) -> Dialect` that the caller can check before
  reading is safer than `delimiter: None`.
- **Boolean spellings** (`t`/`f`, `yes`/`no`) for Q13's `bool`
  inference, as lists the caller gives.
- **Comments** only at the start of a line, or the caller's choice:
  pandas cutting `C#` is a data loss.
- **Skipping lines** at the start and the end (`psql`'s rule and row
  count footer, `vmstat`'s group header).
- **Encodings and decode errors**: windows-1252 and Latin-1 input, and
  a choice between failing, replacing and dropping, as Python offers.
- **A byte-order mark** dropped in every path.
