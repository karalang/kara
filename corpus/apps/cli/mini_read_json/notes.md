# mini_read_json

A Kāra port of [pandas](https://pandas.pydata.org/) 3.0.5's `read_json`
(BSD-3-Clause) over its vendored ujson decoder (BSD-3), with the date
parsing `read_json` leans on: pandas' `guess_datetime_format`,
`array_strptime`, `array_to_datetime` and its ISO 8601 parser, dateutil's
parser (Apache-2.0 or BSD-3) and CPython's `_strptime` and `re` (PSF).
JSON is how most data reaches a script: a page from a REST API, an NDJSON
log, a `DataFrame.to_json` export in one of its orients. Jobs come one per
line:

```
read issues.json
read requests.ndjson lines=True chunksize=3 nrows=4
read customers.json dtype=cust_id:str,zip:str
read table.json orient=table
read daily.json typ=series
```

A `read` job is `pd.read_json("data/<file>", ...)` with `orient`, `typ`,
`dtype` (`True`, `False`, one type, or `col:type,...`), `convert_axes`,
`convert_dates` (`True`, `False` or `a,b`), `keep_default_dates`,
`precise_float`, `date_unit`, `encoding`, `encoding_errors`, `lines`,
`chunksize`, `nrows`, `dtype_backend` and `engine`; `head=n` prints the
first n rows. It prints the shape (or the Series' shape and name), the
dtypes, the index's dtype and `to_csv`; with `chunksize` it prints each
chunk and `done`.

## Running it

The program carries its inputs: it writes `data/` and runs
`data/jobs.txt`, 140 jobs over 55 files that `mk.py` writes:

- **issues.json**, a page of an issue tracker's API: ISO 8601 times in
  UTC, a null `closed_at`, nested user and label objects, non-ASCII
  titles. It is read in every orient, as a Series, and with each dtype
  and date option.
- **requests.ndjson**, an access log in NDJSON with epoch milliseconds,
  a field only some lines have and a blank line; **cutoff.ndjson**, the
  same log cut off mid-line; **crlf.ndjson**, Windows line ends;
  **sensors.ndjson**, epoch seconds, nulls and one reading that is the
  string `ERR`.
- What `to_json` writes in each orient (**split.json**, **index.json**,
  **columns.json**, **values.json**, a records file with missing keys),
  a split file with an extra key and one with duplicate columns, and a
  table-schema export with a UTC time, a category and a nullable
  integer.
- **prices.json** (long decimals), **customers.json** (zip codes and IDs
  as strings), **big_ids.json** (past `2**63`), NaN and Infinity, bools
  with a null.
- Dates as people write them: US month-first, European day-first, month
  names, weekday names, a date and a full time in one column, UTC
  written as `Z` and as `+02:00`, naive and aware together,
  nanoseconds, `20261001`, a bare year, words.
- A byte-order mark, a Latin-1 file, escapes and surrogate pairs, a bad
  escape, a Python dict, trailing data, a trailing comma, scalars, empty
  files and a missing one.

Each job prints `$ <job>`, then what Python prints, or `! ` and the
exception (33 of the 140: ujson's `ValueError`s, the constructors'
shape errors, read_json's own validation, a decode error, a missing
file).

```
python3 -s -P -B mk.py          # writes data/ and data/jobs.txt
python3 -I -B oracle.py > expected.out
python3 -I -B gen_kara.py       # copies data/ into the end of mini_read_json.kara
karac run mini_read_json.kara | diff - expected.out
```

`oracle.py` is the oracle: pandas 3.0.5 on Python 3.13.

## What pandas does, and so the port does

These are its answers, kept because `expected.out` is theirs:

- **A column is a date because of its name.** `created_at` and
  `updated_at`, `end_time`, `timestamp_ms`, `date`, `modified` and
  `datetime` are tried as dates; `due` (`2026-10-15`) and `order_date`
  are not, since `_date` is not one of the suffixes. `modified`
  (`yesterday`) is tried and stays text.
- **The unit of an epoch number is the first that fits.** Epoch
  milliseconds read as `datetime64[ms]` because seconds overflow first.
  With `date_unit=s` they are taken as seconds anyway, in the year
  58719: `as_unit("ns")` fails, the loop goes on, and the last value it
  parsed is kept. Numbers below 31536000 (one year of seconds) are never
  dates, so `created_at` of `1000` stays an int and `20261001` stays an
  int.
- **US and European dates mix silently.** `10/03/2026`, `13/03/2026`,
  `25/12/2026` read as October 3, March 13 and December 25: the guessed
  `%m/%d/%Y` fails on the second value, and the `mixed` fallback swaps
  day and month wherever the month is past 12.
- **`format="iso8601"` is lowercase here**, so it is not pandas' ISO
  path but a literal format that matches nothing; values that are ISO
  8601 in different shapes (`2026-10-01`, `…T09:30:00`, `… 18:45:10.25`)
  only parse on the `mixed` try, and the column then prints with
  milliseconds on every row.
- **`Z` and `+02:00` are mixed time zones.** `2026-10-01T09:00:00Z` with
  `2026-10-01T11:00:00+02:00` stays text, as does a naive time beside an
  aware one; `+00:00` on both rows becomes `datetime64[us, UTC]`.
- **Zip codes become numbers.** `"02139"` is read as the int `2139`, and
  IDs `"000123"` as `123`: a string column that parses as floats that
  are whole is cast to int64. `dtype=cust_id:str,zip:str` keeps them.
- **The default decoder keeps 15 digits.** `1.0000000000000002` reads
  as `1.0` and `6.02214076E23` as `6.0221407600000005e+23`;
  `precise_float=True` reads both exactly.
- **A bool column with a null is float64** (`1.0`, ``, `0.0`), and
  `boolean` under `numpy_nullable`.
- **`nrows=0` reads every line**, since 0 is falsy, and
  **`chunksize=3 nrows=4` reads 5 lines**: the limit is checked before
  each chunk, not inside it. Each chunk infers its own dtypes.
- **A byte-order mark is an error** (`Expected object or value`) unless
  `encoding=utf-8-sig`. **`[1,]` is an error but `{"a":1,}` is not**:
  ujson accepts a trailing comma in an object only.

## Parallel work

Every job runs in its own `TaskGroup` task and the results join in
order, so the transcript is the same on every run and thread count.
The program matches all 1,332 lines of `expected.out` on `__mir-run`,
`__mir-llvm-run`, `__mir-llvm-asan` (no reports) and a `karac build`
binary, at `KARAC_THREADS=1` and `4`, and on five runs of the auto-par
build (0.04 s real).

## Where it differs from pandas

Deliberately, and outside what the jobs exercise:

- A lone surrogate escape (`"\ud83d"`) becomes U+FFFD; Python keeps the
  surrogate.
- `%Z` in a guessed format knows UTC, GMT, UCT, Zulu, Universal,
  Greenwich, `Etc/UTC`, `Etc/GMT`, EST, MST and HST, not every zoneinfo
  name; a table schema's `tz` must be UTC.
- `now`, `today` and a time with no date (`09:30`) depend on the clock,
  so they never parse as dates here.
- `check_keys_split` lists unexpected keys in the order they appear;
  pandas prints a set, in an order that changes between runs.
- List column names are not made into a MultiIndex, and a table schema
  with several primary keys is refused.
- `dtype` takes float64, int64, str, string, object, bool, Int64,
  Float64, boolean and datetime64[ns] (with UTC); a uint64 column is
  never tried as a date; `dtype_backend=pyarrow` and `engine=pyarrow`
  are refused.

## The code, ported

About 9,300 lines before the data section. About 2,550 are reused from
**mini_read_table** (Python's number parsing, decoders, `py_repr` and
mini_re, a port of Python's `re`) and about 1,400 from
**mini_dateparse** (dateutil's lexer and token parser).

The new code is the rest: ujson's decoder with its error messages and
its digit-by-digit float sum; Python's dict and list values; numpy's and
pandas 3's dtype inference and the DataFrame and Series constructors
in each shape `read_json` hands them; the ISO 8601 parser from
`np_datetime_strings.c`; `guess_datetime_format` with pandas' tokenizer
and `strftime`; `_strptime`'s pattern building and `_parse_with_format`;
`array_strptime` and `array_to_datetime` with their resolution inference
and mixed-time-zone checks; `to_datetime` with a unit; `astype`;
`convert_dtypes`; the table schema; and read_json's reader for lines,
nrows and chunks.

## The Mend loop

`karac check` on the pieces reported, besides the usual mutual-recursion
notes: closures with a declared return type (`|x| -> T { ... }`, Rust's
form), which `karac fix` rewrote; a type named `Parser`, which shadows a
prelude type (a warning); `PV`, an enum name that must be PascalCase;
two names defined twice; moves out of a borrowed `for` element and out
of a value matched twice, fixed with `.into_iter()` and `ref`; a cast
from `char` to `u8`, which needs `as u32 as u8`; and one method that does
not exist (`String.parse_f64`; it is `f64.parse(s)`). The assembled
program then matched `expected.out` on the interpreter on its first run,
and a probe of 144 `to_datetime` calls matched pandas on every value.

## Gaps found

None. Each error above had a diagnostic that named the fix.

## What reading JSON would need (notes for M4c)

`docs/library/data.md` in the kara repo has no JSON reader: frames come
from `read_csv`, `parse_csv` and Arrow IPC, and Decision Q1 leaves dates
out of the first release (read as `String`). What this port shows a JSON
reader needs:

- **`read_json` and `read_ndjson`**, the second also over text in memory
  and in chunks. NDJSON logs are the common case, and pandas' `nrows`
  and `chunksize` rules (`nrows=0` meaning everything, chunks that
  overshoot `nrows`) are worth not copying.
- **Records first.** A list of objects with missing keys (null where a
  key is absent) covers API pages and logs; `split`, `columns` and
  `index` matter for reading `to_json` exports back.
- **Nested values.** `user` and `labels` arrive as objects and lists.
  `Column[String]` of their JSON text, or a flattening option
  (`user.login`), is the useful choice; pandas' Python reprs are not.
- **No silent string-to-number casts.** Zip codes and IDs written as
  strings must stay `String`; inference should look at JSON's own types
  and let a schema override them.
- **Integers past i64** (`u64` IDs) and a choice between failing and
  `f64` for larger ones; exact float parsing by default.
- **Dates by schema, not by column name.** Q1's `String` is the safe
  default; when timestamps arrive, the caller should name the columns and
  the format or the epoch unit. pandas' name list, its first-unit-that-
  fits loop and its US/EU mixing are the hazards to avoid.
- **Time zones kept as given**, or converted to UTC on request, not
  refused when `Z` meets `+02:00`.
- **Errors with positions**: ujson's messages name no line or column; a
  cut-off NDJSON line should report its line number.
- **A byte-order mark** skipped, and Latin-1 input decodable on request.
