# mini_read_fwf

A Kāra port of [pandas](https://pandas.pydata.org/) 3.0.5's `read_fwf`
(BSD-3-Clause): the fixed-width reader and the python-engine parser
behind it. Fixed-width text is what older systems still export:
weather-station inventories, mainframe payroll and bank files with a
header and a trailer record, census extracts, and the column-aligned
output of `kubectl get`, `df -h` and `docker ps` that people paste into
a notebook. Jobs come one per line:

```
read stations.txt colspecs=0:11,12:20,...,80:85 header=None names=id,lat,...,wmo dtype=wmo:str
read payroll.dat widths=1,6,15,12,4,8,9,1 header=None names=... skiprows=1 skipfooter=1
read sensors.log comment=# colspecs=0:16,18:24,26:32,34:40,42:48 chunksize=4
read ledger.txt encoding=latin-1 skiprows=[1] decimal=, thousands=.
```

A `read` job is `pd.read_fwf("data/<file>", ...)` with `colspecs` (a
list of `a:b` with either end open, or `None`), `widths`, `infer_nrows`,
`delimiter`, `header`, `names`, `usecols`, `index_col`, `skiprows`,
`skipfooter`, `nrows`, `chunksize`, `comment`, `dtype`, `na_values`,
`keep_default_na`, `na_filter`, `skip_blank_lines`, `thousands`,
`decimal`, `encoding` and `dtype_backend`. It prints the shape, the
dtypes and `to_csv`; with `chunksize` it prints each chunk and `done`.

## Running it

The program carries its inputs: it writes `data/` and runs
`data/jobs.txt`, 128 jobs over 17 files that `mk.py` writes:

- **stations.txt**, 27 rows in the GHCN station-inventory layout, with
  blank GSN/HCN flags, a missing WMO id and `-999.9` for an unknown
  elevation.
- **payroll.dat**, a mainframe export: an `H` header record, `D` detail
  records with zero-padded ids and salaries in cents, and a `T` trailer.
- **pods.txt**, **df.txt** and **docker.txt**, the output of `kubectl
  get pods`, `df -h` and `docker ps`.
- **frame.txt** and **named.txt**, real `DataFrame.to_string` output,
  one with a named index; **grouped.txt**, a two-level header.
- **ledger.txt**, a German bank ledger in ISO-8859-1 with a dashed rule
  under the header and `1.180,00` amounts; **invoice.txt**, windows-1252
  with curly quotes and `€`.
- **sensors.log**, CRLF lines with `#` comments, `-9999` and `ERR`
  markers and a trailing comment on a row; **register.txt**, columns
  padded with `_`; **bom.txt**, UTF-8 with a byte-order mark;
  **mac.txt**, old Mac `\r` line ends.
- **empty.txt**, **blank.txt** and a missing file.

Each job prints `$ <job>`, then what Python prints, or `! ` and the
exception (25 of the 128: both or neither of `colspecs` and `widths`,
names that do not fit, `usecols` not found or out of range, a bad
`index_col`, `skipfooter` with `nrows` or chunks, a column that will
not cast, a two-character decimal, decode errors, an unknown encoding,
an empty file, a missing file).

```
python3 -s -P -B mk.py          # writes data/ and data/jobs.txt
python3 -I -B gen_kara.py       # copies data/ into the end of mini_read_fwf.kara
python3 -I -B oracle.py > expected.out
karac run mini_read_fwf.kara | diff - expected.out
```

`oracle.py` is the oracle: pandas 3.0.5 on Python 3.13.

## What pandas does, and so the port does

These are its answers, kept because `expected.out` is theirs:

- **Inferred columns are runs of non-blank characters** in the first
  100 rows (`infer_nrows`), so a header with a space in it splits only
  where every row has a gap: `Mounted on` stays one column, but
  `docker ps`'s `Exited (1) 40 minutes ago` leaves `ago` in a column of
  its own, named `Unnamed: 5`. With `infer_nrows=3` the minus signs of
  later rows fall outside the columns (`118.3889` for `-118.3889`) and
  `LOS ANGELES` is cut in two. A byte-order mark counts as a character
  when inferring, so in `bom.txt` `qty` and `price` merge.
- **`skiprows` counts lines, comments included**, and `skiprows=1` on
  the ledger skips the header, making the dashed rule the column names;
  `skiprows=[1]` is what drops the rule.
- **`thousands="."` runs on every column that matches the number
  pattern**, so the ledger's `01.10.2026` dates become the integers
  `1102026`.
- **Types come from the values.** Zero-padded ids read as `int64`
  (`000017` is `17`) unless `dtype=emp:str`; one missing WMO id makes the
  column `float64` (`72534.0`). `dtype=status:bool` on `A`/`I` flags is
  each string's truthiness, so every row is `True`.
- **Chunks are typed one at a time**, so `rh_pct` is `str` in the chunk
  with `ERR` and `int64` in the next. Comment lines count toward a chunk,
  so `chunksize=4` gives chunks of 4, 2 and 4 rows and then a 0-row chunk
  whose columns are `object`.
- **Encodings.** UTF-8 is the default, so the ledger fails at its first
  `ä` and the invoice at its first curly quote; `latin-1`, `cp1252` and
  `iso-8859-1` are aliases Python normalizes. The default decoder drops
  a byte-order mark only from the first field of the first line.
- **`index_col` with `usecols`** indexes into the selected columns, so
  `index_col=4,0` with three columns is an `IndexError` from inside
  pandas.

## Parallel work

Every job runs in its own `TaskGroup` task and the results join in
order, so the transcript is the same on every run and thread count.
The program matches all 1,575 lines of `expected.out` on `__mir-run`,
`__mir-llvm-run`, `__mir-llvm-asan` (no reports) and a `karac build`
binary, at `KARAC_THREADS=1` and `4`, and on five runs of the auto-par
build (0.01 s real).

## The code, ported

About 3,800 lines before the data section. About 1,450 are reused from
**mini_read_html**: Python's number parsing, `repr`, the NA value sets,
pandas' column inference, nullable dtypes and `to_csv`.

The new code is the rest: Python's strict UTF-8, ASCII, Latin-1 and
cp1252 decoders with their exact error messages and the codec alias
table; text lines split as `newline=""` splits them; the
`FixedWidthReader` with `detect_colspecs`; and the python engine's
`PythonParser`: `skiprows` before the first read, comments and blank
lines, header inference with `Unnamed` names and de-duplication,
MultiIndex headers, `usecols` by name or position, implicit and explicit
index columns, `thousands` and `decimal` through the number pattern,
`skipfooter`, `nrows`, chunked reading, and casting to `int64`,
`float64`, `bool`, `str`, `category`, `Int64`, `Float64` and `string`
with pandas' errors.

## The Mend loop

`karac check` reported three errors and a warning. Two were a `match`
on a tuple that moved an element of a borrowed vector, and one moved a
`for` element; `karac fix` offers `.clone()` for each, and in a scratch
copy it applied all three. I fixed them by hand with two small helpers.
The warning was a `struct Parser` that shadows the prelude's, renamed
`Engine`. The first run matched `expected.out` on the interpreter.

## Gaps found

- **B-2026-10-29-34** (diagnostics): `ref mut m` in a pattern, Rust's
  order for Kāra's `mut ref m`, is reported as "'mut' is a reserved
  keyword" plus a cascading parse error, with no fix. `mut m` in a
  pattern already gets a Rust-syntax diagnostic with a fix.

No gaps in the running code.

## What reading fixed-width files would need (notes for M4c)

Compared with `docs/library/data.md` in the kara repo, whose `read_csv`
takes `delimiter`, `header`, `null_value` and `infer`:

- **A reader**: `read_fwf(path; colspecs or widths, infer_rows)`, or a
  `parse_fwf(text)` with the same options, returning `Result[DataFrame,
  DataError]`. Inferring the columns needs a cap on the rows it reads,
  like `Infer.First(n)`, and the result should be visible, since pandas'
  guesses here are wrong often (split headers, lost minus signs).
- **Record types.** Mainframe files mix header, detail and trailer
  records. Skipping by line number and from the end (`skiprows`,
  `skipfooter`), or a filter on a record-type column, is the minimum.
- **A schema override** (Q13's next option) matters more here than for
  CSV: these files are full of zero-padded ids, dates written as digits
  and amounts in cents.
- **Missing-value markers** like `-9999`, `ERR` and `-999.9`, as a list
  and per column; `null_value` takes one string.
- **Comments and blank lines**, which logs and exports both have.
- **Number formats**: `thousands` and `decimal`, matched only in the
  columns the caller names, since matching every column turned dates
  into integers here.
- **Chunked reading** with one schema for every chunk, so a type does
  not change between chunks the way it does in pandas.
- **Encodings**: Latin-1 and windows-1252 are common in these files; a
  reader needs an `encoding` option or a step that decodes first.
- **Errors**: `DataError` would need variants for a column
  specification that does not fit the names, and for a decode error
  with its byte position.
