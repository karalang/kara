# mini_read_sas

A Kāra port of [pandas](https://pandas.pydata.org/) 3.0.5's SAS readers,
`pandas.io.sas` (BSD-3-Clause): `read_sas` and both readers under it,
`SAS7BDATReader` for SAS data sets and `XportReader` for SAS transport
files. Insurance, banking, utilities and clinical trials keep their data
in SAS, and regulators take it as transport files: the FDA's submission
standard (SDTM) ships every domain as an `.xpt`. `read_sas` is how a
Python pipeline opens either. Jobs come one per line:

```
meta claims_2023.sas7bdat
read branch_sales.sas7bdat encoding=infer index=WEEK
chunks meter_readings.sas7bdat 64 encoding=latin-1
rows lb_2024.xpt 10,30,8,5 encoding=latin-1
reader claims_2023.sas7bdat convert_dates=False blank_missing=False encoding=infer
```

| Job | The Python call |
|---|---|
| `meta` | `read_sas(path, iterator=True)`: the layout, byte order, encoding, compression, creator, the dates, the row and column counts, and each column's name, type, length, format and label |
| `read` | `pd.read_sas(path, ...)` with `format`, `encoding` and `index`; it prints the shape, the index if one was set, the dtypes, then `to_csv` |
| `chunks` | `pd.read_sas(path, chunksize=n, ...)`, each chunk printed as `read` prints a frame |
| `rows` | `reader.read(n)` for each n in the list, until the file runs out |
| `reader` | `SAS7BDATReader(path, ...)` with the options `read_sas` does not pass on: `convert_dates`, `blank_missing`, `convert_text`, `convert_header_text` |

## Running it

The program carries its inputs: it writes `data/` and runs
`data/jobs.txt`, 65 jobs over 30 files. pandas writes neither format, so
`mk.py` has its own writers for both, with SAS's two row compressions:

- **claims_2023.sas7bdat**, health-insurance claims as SAS 9.4 on Linux
  writes them (64-bit, little-endian, UTF-8): names with accents,
  `YYMMDD`, `DATETIME` and `TIME` columns, receipt times with a quarter
  second, a 3-byte numeric, blank strings and missing values.
- **branch_sales.sas7bdat**, from SAS on Windows (32-bit, WLATIN1, so
  `cp1252` under `encoding=infer`): metadata and the first rows share a
  mix page.
- **meter_readings.sas7bdat**, smart-meter readings from SAS on AIX
  (64-bit, big-endian, latin-1, `COMPRESS=CHAR`): every row an RLE
  subheader, two rows stored as they are because they did not shrink, and
  a truncated subheader SAS left behind.
- **survey_wave2.sas7bdat**, a numeric-heavy survey (`COMPRESS=BINARY`,
  RDC), with a page of a type pandas skips between the data pages.
- **inventory_sol.sas7bdat**, from SAS 8 on Solaris (32-bit, big-endian):
  a mix page whose rows start behind an odd number of subheaders.
- **empty_extract.sas7bdat**, an extract with no rows.
- **lb_2024.xpt**, SDTM lab results in transport v5 (also as `.xpt.gz`
  and in a `.zip`): records wider than a card, results of 0.0, and the
  missing values `.`, `.A` (not done) and `._`.
- **dm_screen.xpt** and **dm_enrolled.xpt**, demographics with records
  shorter than a card; **plant_vms.xpt**, from SAS 6 on VMS, with
  135-byte NAMESTR records, 2-, 4- and 6-byte numerics and a header date
  pandas cannot parse; **ae_ds.xpt**, two members in one file.

The other 17 are the errors people meet: a CSV renamed to `.sas7bdat`,
a file cut in its header, one copied before SAS wrote its last page, one
cut mid-page, a column-count mismatch (a warning, not an error), a
subheader signature pandas does not know, four corrupt compressed rows,
a data set with no columns, a SAS file named `.dat`, a CPORT file, a v8
transport file, a bad header prefix, a 9-byte numeric and a transport
file with no observation header; and one job reads a file that is not
there. Each job
prints `$ <job>`, then what Python prints, or `! ` and the exception
pandas raised (22 of the 65).

```
python3 -s -P -B mk.py          # writes data/ and data/jobs.txt
python3 -I -B gen_kara.py       # copies data/ into the end of mini_read_sas.kara
python3 -I -B oracle.py > expected.out
karac run mini_read_sas.kara | diff - expected.out
```

`oracle.py` is the oracle, and it is pandas 3.0.5 on numpy 2.5.3.

## What pandas does, and so the port does

These are pandas' answers, kept because `expected.out` is pandas:

- **A transport 0.0 reads as 5.397605346934028e-79.** The IBM-to-IEEE
  conversion runs in numpy's wrapping `uint32` arithmetic, and an IBM zero
  has no exponent to subtract from.
- **Short records lose rows.** With records of 80 bytes or less, pandas
  counts the rows by subtracting every all-blank 8-byte word in the last
  card, so `dm_enrolled.xpt`, whose last subject has a blank ARMCD, reads
  one subject short.
- **Only the first member.** `ae_ds.xpt` holds AE and DS; pandas reads AE
  and counts DS's header cards as rows of it.
- **Bytes by default.** With no `encoding`, sas7bdat strings and every
  transport string stay `bytes` and print as `b'...'`.
- **A row count that overstates the rows** is "Length of values (70) does
  not match length of index (47)", and `rows` past the end is a bare
  `StopIteration`.
- Dates come back at second resolution, datetimes at milliseconds, and
  `to_csv` prints a datetime column's date alone when every value is
  midnight.

## Parallel work

Every job runs in its own `TaskGroup` task and the results join in
order, so the transcript is the same on every run and thread count. The
program matches all 2,857 lines of `expected.out` on `__mir-run`,
`__mir-llvm-run`, `__mir-llvm-asan` (no reports) and a `karac build`
binary, at `KARAC_THREADS=1` and `4`, and on five runs of the auto-par
build (0.09 s real or less).

## The code, ported

About 3,170 lines before the data section, about 1,060 of them reused
from mini_read_stata (text helpers, CRC-32, inflate, zip and gzip):

- **Codecs**: strict UTF-8 with Python's error messages, latin-1, cp1252
  and cp1251, `LookupError` for the rest, and Python's `repr` of `bytes`
  and `str`.
- **sas7bdat** (`sas7bdat.py`, `_libs/sas.pyx`): the header in either
  layout and byte order, the page types, the subheader signatures, the row
  size, column size, text, name, attribute, format and label subheaders,
  the RLE and RDC decompressors with their bounds checks, numerics of 3 to
  8 bytes, blank strings to NaN, and the two date conversions.
- **xport** (`sas_xport.py`): the library and member headers, NAMESTR
  records, IBM doubles, truncated numerics, the missing values, pandas'
  record count, and its `%d%b%y:%H:%M:%S` header dates.
- **read_sas**: the format from the file name, compression, `index`,
  `chunksize`, and `to_csv`.

## The Mend loop

`karac check` reported one error with no fix-it:
`ch.d.push(Vec.filled(nrows, 0))` into a `Vec[Vec[u64]]` was refused as a
`Vec[i64]` (B-2026-10-29-24). I wrote `0u64` by hand. `karac fix` then
applied 4 edits (an `own` parameter, two `mut ref` bindings and a
`.clone()`), and the program checked clean.

The first run matched all 2,857 lines on the interpreter and the compiled
backends, but the ASan lane stopped with a stack-buffer-overflow:
`Vec.filled(nrows, None)` pushed into a `Vec[Vec[Option[Vec[u8]]]]`
clones its `None` from a slot too small for it (B-2026-10-29-25). With
the `None` given a typed `let`, every lane matched; since the fix
(e5f935eb0b) the bare `None` is back.
`mini_read_sas.orig.kara` is the program as first written.

## Gaps found

- **B-2026-10-29-24** (open): an integer literal passed to `Vec.filled`
  does not take its type from where the vector goes when that is a
  method argument (`d.push(Vec.filled(2, 0))` with `d: Vec[Vec[u64]]`), a
  tuple element or an `Option` payload. A `let` annotation, a function
  argument and a struct field all work.
- **B-2026-10-29-25** (fixed in e5f935eb0b, workaround reverted): `d.push(Vec.filled(n, None))` with `d` a
  `Vec[Vec[Option[T]]]` reads past the `None` it clones on the compiled
  backends (16 bytes from a 1-byte slot for `Option[i64]`). The
  interpreter is right, and `__mir-llvm-run` prints the right answer
  without ASan.

## What reading SAS files would need (notes for M4c)

Compared with `docs/library/data.md` in the kara repo, which reads CSV
and Arrow IPC:

- **A SAS reader**: `read_sas(path; format, encoding, columns)` beside
  `read_csv`, with the sas7bdat and transport formats behind it, and a
  `Stata` and `SPSS` sibling (mini_read_stata). Regulated industries
  hand data over in these, and a pipeline has no other way in.
- **Text encodings.** A data set records its encoding, and Windows SAS
  writes WLATIN1. `data.md` has only `String`, which is UTF-8, so the page
  should say whether a reader decodes with a named codec, fails, or
  returns bytes; pandas returns `bytes` unless asked, which a Kāra frame
  would need a `Column[Vec[u8]]` element type to match.
- **Kinds of missing.** `.A` to `.Z` and `._` mean "not done", "refused"
  and so on in clinical and survey data; `data.md` has one null per slot,
  so keeping the reason needs a side column or an option.
- **Dates.** Decision Q1 reads dates as `String`; SAS stores them as
  numbers plus a format (`DATE9.`, `DATETIME20.`, `TIME8.` and about 40
  more), so the reader has to turn them into dates, or into text the
  program parses back, and pick a resolution (pandas: seconds for dates,
  milliseconds for datetimes).
- **Truncated numerics.** SAS stores numerics in 3 to 8 bytes; they read
  back as `f64`, which `data.md` has.
- **Chunked reading** (`chunksize`, `read(n)`): SAS data sets run to
  gigabytes, so a reader that yields frames of n rows with one schema is
  part of the API.
- **Metadata**: column labels, formats, the data set label and the
  creation dates have nowhere to live on a frame; `Field` could carry
  them.
- **Errors**: not a SAS file, a truncated file, a corrupt compressed row
  and an unsupported transport version are each a `DataError` the page
  could name.
