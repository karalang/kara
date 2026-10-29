# mini_read_stata

A Kāra port of [pandas](https://pandas.pydata.org/) 3.0.5's Stata reader,
`pandas.io.stata` (BSD-3-Clause): `read_stata` and the `StataReader`
under it. Survey, labour-force, health and trade data reach programs as
Stata `.dta` files, from statistics offices, the World Bank's
microdata library, the DHS surveys and most of applied economics, and
`read_stata` is how a Python pipeline opens them. The port reads every
format pandas does, 102 to 119, in either byte order. Jobs come one per
line:

```
meta hh_survey_w3.dta
read hh_survey_w3.dta convert_missing=True columns=hhid,head_age,hhsize,income,weight
read clinic_visits.dta index_col=index columns=index,patient,notes
read cpi_regions.zip columns=region,cpi
chunks hh_survey_w3.dta 4
```

| Job | The Python call |
|---|---|
| `meta` | `StataReader(path)`: the format, byte order, counts, data label and time stamp, each variable's type, display format, value-label name and label, then the value-label tables |
| `read` | `pd.read_stata(path, ...)` with `convert_dates`, `convert_categoricals`, `convert_missing`, `preserve_dtypes`, `order_categoricals`, `index_col` and `columns`; it prints the shape, the index if one was set, the dtypes (with a categorical's categories), then `to_csv` |
| `chunks` | `pd.read_stata(path, chunksize=n, ...)`, each chunk printed as `read` prints a frame |

## Running it

The program carries its inputs: it writes `data/` and runs
`data/jobs.txt`, 83 jobs over 20 files. `mk.py` writes them with its own
`.dta` writer, since pandas writes only formats 114 to 119 and only one
shape of each, except three it lets pandas' `to_stata` write:

- **hh_survey_w3.dta**, a household survey (format 118, little-endian):
  UTF-8 names (`Thắng Nguyễn`, `Ana-María Oyarzún`), value labels whose
  text is stored out of order, Stata's extended missing values `.a` and
  `.b` in every numeric type, a code (9) no label covers, a `%td`
  interview date and a `%tc` sync time with milliseconds.
- **clinic_visits.dta**, by pandas (117, big-endian): its index written as
  a column, a categorical diagnosis, strL notes (a long one, one with quotes
  and a comma), `%tc` and `%td` columns, a bool, and NaN in a
  float and a double. **cpi_regions.dta**, by pandas (114): `%tm`, `%tq`
  and `%ty` dates and labels given as a dict; the same file as `.dta.gz`
  and in a `.zip`. **allnan_118.dta**, by pandas: a column with no value.
- **trade_119.dta**, customs declarations (119): strLs shared between
  rows, a binary strL (a PDF and a PNG header), `%tw` and `%th` dates, and
  bytes at the ends of their valid range.
- **lfs_104.dta** to **lfs_115.dta**, one labour-force file in each old
  format, alternating byte order: 8-byte value labels before 108,
  expansion fields, the old missing-value ranges up to 111 (where every
  missing value is `.`) and 2^333 as a 105 double's missing value.
- **ward_census_117.dta**, a census extract whose tenure labels repeat
  ("Other" twice), with a partly labelled column and characteristics.
- **empty_118.dta**, a big-endian file with no observations.

Four more are the errors people meet: a file in format 116 and one in
112 (which no Stata wrote), a CSV renamed to `.dta`, and a file that is
not there. Each job prints `$ <job>`, then what Python prints, or `! `
and the exception pandas raised (15 of the 83, most of them real pandas
behaviour on valid files: `convert_missing=True` with a date column
fails, and so do repeated labels).

```
python3 -s -P -B mk.py          # writes data/ and data/jobs.txt
python3 -I -B gen_kara.py       # copies data/ into the end of mini_read_stata.kara
python3 -I -B oracle.py > expected.out
karac run mini_read_stata.kara | diff - expected.out
```

`oracle.py` is the oracle, and it is pandas 3.0.5 on numpy 2.5.3.

## Parallel work

Every job runs in its own `TaskGroup` task and the results join in
order, so the transcript is the same on every run and thread count. The
program matches all 1,153 lines of `expected.out` on `karac run`,
`__mir-run`, `__mir-llvm-run`, `__mir-llvm-asan` (no reports) and a
`karac build` binary, at `KARAC_THREADS=1` and `4`, and on five runs of
the auto-par build (0.01 s real or less).

## The code, ported

About 2,850 lines before the data section, 760 of them reused from
mini_read_excel (text helpers, CRC-32, inflate, the zip reader):

- **Opening** (`io/common.py`): compression from the file name, gzip
  (header flags, CRC-32 and length checked) and a zip of one member.
- **The header** (`_read_old_header`, `_read_new_header`): the fixed
  layouts of 102 to 115 with each field's width per format and the
  expansion fields, and the tagged layout of 117 to 119 through its map,
  with 117's misplaced variable labels; struct's "unpack requires a
  buffer" on a short read.
- **Tables**: value labels in both layouts (offsets sorted, a repeated
  code keeping its place), and the strL table with 117's 8-byte and
  118/119's packed 12-byte keys.
- **read**: records cut by the type list, strings decoded (latin-1
  before 118, UTF-8 with a latin-1 fallback after), strLs inserted,
  `columns`, missing values to NaN or to `.a`..`.z`, the seven date
  formats with pandas' resolution for each (`%tc` in milliseconds, the
  rest in seconds), value labels to categoricals (including the fixed
  categories it uses across chunks when every value is labelled), the
  repeated-label error, `preserve_dtypes`, `index_col` (and set_index
  turning evenly stepped integers into a RangeIndex), and `to_csv`.

## The Mend loop

The first `karac check` stopped at one parse error, mine:
`-> Result<(), PyErr>`, Rust's angle brackets, which got "Expected
LeftBrace, found LessThan" with no hint and no fix, and a second, false
error at the end of the `impl` it was in (B-2026-10-29-23). I fixed it by hand.
`karac fix` then applied 4 edits (three `own` parameters and a
`.clone()`), and the program checked clean.

The first run printed `! KeyError: 1.7014118346046923e+38` for every job
that keeps float32 missing values: `let f: f64 = f32_of(bits)` gave a
denormal on the compiled backends (B-2026-10-29-22, below). With that one line
written as `f32_of(bits) as f64`, the first run matched all 1,153 lines.
`mini_read_stata.orig.kara` is the program as first written.

## Gaps found

- **B-2026-10-29-22** (open): the implicit widening of a call's result is dropped
  when the call initializes an annotated `let` (`let x: f64 = f()` with
  `f() -> f32`, or `let n: i64 = g()` with `g() -> i32`), is a function's
  return value (`return f()` or a tail `f()`), or is an `if` arm. The
  interpreter prints the right value; `__mir-llvm-run`, `karac run`,
  `karac build` and the ASan lane read a half-written slot (1.5 prints
  as `5.28426686e-315`, -7 as `4294967289` or junk). The workaround is
  one `as f64` in `float_missing`.
- **B-2026-10-29-23** (open): Rust-style `Result<(), E>`, `Vec<i64>` or
  `Option<i64>` in a type gets a bare token error with no pointer to
  Kāra's `[T]`, and no fix.

## What reading Stata files would need (notes for M4c)

Compared with `docs/library/data.md` in the kara repo, which reads CSV
and Arrow IPC:

- **A Stata reader**, and beside it SAS (`.sas7bdat`, `.xpt`) and SPSS
  (`.sav`). Statistics offices, survey programmes and research data
  archives publish in these, and a pipeline that loads them has no
  other way in: `read_stata(path; columns, convert_dates,
  convert_categoricals, missing)` beside `read_csv` would cover every job
  here. Its parts are general: gzip and zip readers and inflate are not
  in `std` or on any library page.
- **Labelled values.** Stata stores codes and a label table per
  variable; pandas makes them categoricals. `data.md` has no dictionary
  or categorical element type (a dictionary-encoded Arrow field is
  `Err(Unsupported)`), so the page should say whether labels become a
  `Column[String]`, a dictionary column, or a codes column with the table
  on the side. The two answers pandas gives depending on chunking, and
  its refusal of repeated labels, are both things a design should settle
  on purpose.
- **Kinds of missing.** `.a` to `.z` mean different things in a survey
  (refused, don't know, not asked). `data.md` has one null per slot; a
  reader that keeps the reason needs a side column or an option, as
  `convert_missing` is in pandas.
- **Dates.** Decision Q1 reads dates as `String`; Stata stores them as
  numbers plus a display format, and seven of the formats here (daily,
  millisecond, weekly, monthly, quarterly, half-yearly, yearly) need the
  reader to turn them into dates, or into text the program parses back.
- **Narrow types.** Stata's byte, int, long and float are `i8`, `i16`,
  `i32` and `f32`; the element types cover them, and Decision Q14 keeps
  each width, which is what `preserve_dtypes` asks for.
- **Chunked reading** (`chunksize`): survey files run to gigabytes, so a
  reader that yields frames of n rows, with a stable schema across
  chunks, is part of the API.
- **Variable and data labels**, notes and characteristics are metadata
  a frame has nowhere to keep; `Field` could carry a description.
- **Errors**: an unsupported version, a truncated file, a missing column
  and a repeated column are each a `DataError` the page could name.
