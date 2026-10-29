# mini_read_excel

A Kāra port of the two libraries a Python program uses to read a
spreadsheet: [openpyxl](https://openpyxl.readthedocs.io/) 3.1.5's
read-only reader (MIT) and [pandas](https://pandas.pydata.org/) 3.0.5's
`read_excel` on top of it (BSD-3-Clause). It opens `.xlsx` files from
the zip up (its own zip reader and inflate, its own XML parser), reads
cells the way openpyxl hands them to Python, and builds frames the way
pandas does from those cells. Jobs come one per line:

```
sheets sales_q3.xlsx
rows sales_q3.xlsx Orders min_row=1 max_row=4 min_col=6 max_col=9 formulas
cells stock_mac.xlsx Count
rows ledger_stale.xlsx Ledger reset
read sales_q3.xlsx index_col=0 usecols=A,E,K nrows=12
read survey.xlsx header=1 na_values=-
read timesheet.xlsx sheet_name=Week\s42 index_col=0
```

| Job | The Python call |
|---|---|
| `sheets` | `load_workbook(read_only=True, data_only=True, keep_links=False)`, as pandas opens a file: the sheet names, the date epoch, and each sheet's state and dimension |
| `rows` | `ws.iter_rows(min_row=.., max_row=.., min_col=.., max_col=.., values_only=True)`, one `repr(tuple)` a row; `formulas` opens with `data_only=False`, `reset` calls `reset_dimensions()` first |
| `cells` | The same without `values_only`: each cell's coordinate, `data_type`, `number_format`, `is_date` and value |
| `read` | `pd.read_excel(engine="openpyxl", ...)` with `sheet_name`, `header`, `names`, `index_col`, `usecols`, `dtype`, `true_values`, `false_values`, `skiprows`, `nrows`, `na_values`, `keep_default_na`, `parse_dates` and `skipfooter`; it prints the shape, the dtypes, then `to_csv` |

In a job, `\s` is a space (`Week\s41`, `usecols=Order\sID,Rep`).

## Running it

The program carries its inputs: it writes `data/` and runs
`data/jobs.txt`, 103 jobs over seven workbooks, each shaped like a file
a real program wrote, since openpyxl itself writes only one shape:

- **sales_q3.xlsx**, a sales team's quarter saved by Excel 365: a shared
  formula down the Total column with its cached values, a percentage
  that is often blank, a `[h]:mm` duration, booleans, an error in one
  Total, notes with rich-text runs and a `_x000D_` escape, an order id
  typed as text with its leading zeros, a blank row; a printed Summary
  sheet (merged title, a note, a blank row, the table and a totals row);
  and a Lookups sheet with two tables side by side.
- **stock_mac.xlsx**, a warehouse count from Excel 2011 for Mac on the
  1904 date system, with every kind of number format a date test has to
  get right: builtin dates, times and datetimes, a builtin id the table
  does not have, custom codes with an escaped dash, a locale prefix, a
  colour, a literal and minutes:seconds, and a negative-in-red format
  that is not a date at all.
- **crm_contacts.xlsx**, a CRM export written with the .NET Open XML SDK:
  every element prefixed `x:`, inline strings and no shared string
  table, no dimension, rows and cells with no `r` attribute, `t="str"`
  and `t="d"` (ISO dates), a zip code with a leading zero, a padded name,
  a number in exponent form, and text that looks like a date.
- **ledger_stale.xlsx**, from a reporting tool that writes the dimension
  once and never updates it (it claims `A1:C4`; the data runs to `F12`),
  with a shared formula whose text has a double space, and a hidden and
  a very hidden sheet. openpyxl's read-only iteration believes the
  dimension; pandas resets it first.
- **survey.xlsx**, a form tool's export: two header rows, answers mixing
  numbers and words in one column, `N/A` and `-` for no answer,
  duplicated and empty column titles, trailing cells with only a style.
- **timesheet.xlsx**, written by openpyxl from Python values: dates,
  datetimes, times of day, timedeltas, `None`, a formula with no cached
  value, a merged header, and a second sheet with a header only.
- **stored.xlsx**, uncompressed, as some streaming writers produce, with
  an empty sheet and a header-only one.

Four more files are the errors people meet: a CSV renamed to `.xlsx`, a
zip with no workbook in it, an `.xls` name, and a file that is not
there. Each job prints `$ <job>`, then what Python prints, or `! ` and
the exception openpyxl or pandas raised (15 of the 103).

```
python3 -s -P -B mk.py          # writes data/ and data/jobs.txt
python3 -I -B gen_kara.py       # copies data/ into the end of mini_read_excel.kara
python3 -I -B oracle.py > expected.out
karac run mini_read_excel.kara | diff - expected.out
```

`oracle.py` is the oracle, and it is openpyxl 3.1.5 (with lxml) and
pandas 3.0.5 on numpy 2.5.3.

## Parallel work

Every job runs in its own `TaskGroup` task and the results join in
order, so the transcript is the same on every run and thread count. The
program matches all 1,347 lines of `expected.out` on `karac run`
(`__mir-run`), `__mir-llvm-run`, `__mir-llvm-asan` (no reports) and a
`karac build` binary, at `KARAC_THREADS=1` and `4`, and on five runs of
the auto-par build (0.08 to 0.10 s real, about 0.3 s user).

## The code, ported

About 5,400 lines before the data section:

- **The container** (`zipfile`, `zlib`): the end record and central
  directory, stored and deflated members, CRC-32, and an inflate
  (RFC 1951) with fixed and dynamic Huffman blocks; `BadZipFile` for a
  file that is not a zip.
- **XML** (`xml.etree.ElementTree` over expat): namespaces, entities and
  character references, CDATA, text and tail as ElementTree keeps
  them, and expat's error messages with line and
  column.
- **The workbook** (`openpyxl.reader.excel`, `workbook/_reader.py`,
  `packaging/`): content types and relationships, the workbook part
  wherever the relationships put it, sheet states, the 1904 flag,
  shared strings with rich text and `_xHHHH_` escapes, and styles down
  to each cell's number format; `is_date_format` with
  `BUILTIN_FORMATS`.
- **The sheet** (`worksheet/_reader.py`, `_read_only.py`,
  `cell/read_only.py`): `parse_row` and `parse_cell` for every `t`,
  rows and cells with and without `r`, `_cells_by_row` filling gaps with
  `EmptyCell`, the dimension and `reset_dimensions`, `from_excel` on
  both epochs (with the 1900 leap-year bug and times of day),
  `from_ISO8601`, and shared formulas translated by openpyxl's
  `Tokenizer` and `Translator`.
- **pandas** (`io/excel/_openpyxl.py`, `_base.py`,
  `io/parsers/python_parser.py`, `base_parser.py`, `_libs/lib.pyx`):
  `get_sheet_data` with its trailing-row trim, `_calc_rows`, header and
  `names` inference with duplicate renaming, `usecols` by letter,
  position or name, `index_col` and its name, the missing-value set,
  `maybe_convert_numeric` and `maybe_convert_bool`, `dtype` casts and
  their errors, object columns inferred to `str`, `datetime64` or
  `timedelta64`, and `to_csv`'s formatting for each.

## The Mend loop

The first `karac check` stopped at 13 parse errors, all mine, none with
a fix: a section that ended in a stray, unclosed `struct Keys {` (whose
error named the next `struct` and suggested `r#struct`), enum and
variant names written in capitals (`PV`, `DT`), a closure with a return
type, two `[Str; N]` constants, and `for row in mut out` for a loop that
writes (whose error suggested nothing and cascaded into a false "top-level
statements and `fn main()`"). I wrote `fn(u8) -> bool` for a parameter,
which is refused with "Expected type, found Fn" (B-2026-10-29-1); the
type is spelled `Fn(u8) -> bool`. I fixed these by hand.

`karac fix` then applied 9 edits (`.clone()` on moved loop elements,
`mut` at a call, `Some(ref ..)` bindings) and a second round 3 more,
which left one warning: `struct Parser` shadows a prelude type, so it is
`XmlParser`. The first run matched Python on all 1,347 lines.
`mini_read_excel.orig.kara` is the program as first written.

## Gaps found

- **B-2026-10-29-16** (open): `for x in mut v` gets a keyword error with
  no pointer to `v.iter_mut()`, and two cascade errors.
- **B-2026-10-29-17** (open): an unclosed struct before the next item is
  reported at the next `struct`, with an `r#struct` suggestion.
- **B-2026-10-29-1** (open), met once more.

## What reading a spreadsheet would need (notes for M4c)

Compared with `docs/library/data.md` in the kara repo, which reads CSV
and Arrow IPC:

- **An Excel reader at all.** Spreadsheets are the most common way data
  reaches a program from people: finance exports, CRM and form-tool
  downloads, the attachment to an email. A `read_excel(path; sheet,
  header, skip_rows, n_rows, columns, null_values, schema)` beside
  `read_csv` would cover every job here. Its parts are general too: a
  zip reader with inflate and an XML parser are not in `std` or on any
  library page, and this port needed about 1,200 lines of them.
- **Sheet selection.** By name or position, all sheets as a `Map` of
  frames, the names and states (hidden, very hidden) without reading
  the cells.
- **Where the table is.** Real sheets have a title, notes and blank rows
  above the header, totals below it, two header rows, and tables side by
  side: `header` (row), `skip_rows`, `n_rows`, a footer count, and
  `columns` by name, position or letter range (`A:E`) answer these.
- **Typed cells.** A cell's type comes from the file, not from guessing
  at text: numbers, booleans, strings, errors (a `#N/A` from a failed
  lookup reads as missing) and dates that are numbers plus a number format, on two
  epochs. Decision Q1 reads dates as `String`; from Excel that means
  formatting a serial number the program then parses back. Five of the
  seven workbooks here have date, time-of-day or duration (`[h]:mm`)
  columns.
- **A schema override.** Order ids and zip codes with leading zeros,
  and a column that mixes numbers and words, need a per-column type,
  the option Q13 names as next for CSV.
- **Missing values.** The empty cell, plus `N/A`, `-` and the like, per
  read, and whether the defaults apply.
- **Formulas.** A reader must say whether it returns the cached value
  or the formula text, and what a formula with no cached value reads as
  (openpyxl gives `None`); shared formulas need translating per cell.
- **Duplicate and empty titles.** pandas renames a second `Tools` to `Tools.1`
  and an empty title to `Unnamed: 3`; `data.md` requires unique names,
  so the page should say what a reader does with them.
- **Errors.** Not a zip, no workbook part, a missing sheet, a header row
  past the end and columns that are not there are each a `DataError`
  the page could name.
