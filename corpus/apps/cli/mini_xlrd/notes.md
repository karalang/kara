# mini_xlrd

A port of [xlrd](https://github.com/python-excel/xlrd) 2.0.2 (BSD; Stephen
John Machin, Lingfo Pty Ltd and contributors), the pure-Python reader for
Excel `.xls` files. It covers the OLE2 compound document, the BIFF8
workbook globals and worksheet records, the formatting records, xldate,
the cell-name helpers and the format sniffing of `open_workbook`. Three
small programs run on top: a CSV export, a sales report and a text search.

```
> dump raw/oddities.xls
  log: *** Unexpected embedded BOF (0x0809) at offset 376: version=0x0600 type=0x0040
  log: WARNING *** SCL rcd sheet 0: should have 0.1 <= num/den <= 4; got 1/20
  ...
  sheet 0 'Odd': 2 rows x 2 cols, visibility 0
    view: selected 1, active 1, frozen 0, grid 1, zoom 100/100, first (2, 1), split (4, 2) at (4, 2)
    A1 number 1.0
    B2 number 2.0
...
> !report books/sales.xls
  Lookup (hidden): 3 alias(es)
  Q1!D7: units 'n/a' is text, row skipped
  Q1!B10: region 'Ost' read as 'East'
  ...
  product Widget  revenue    307.25  share  43.8%
  product Gadget  revenue    227.96  share  32.5%
  product Gizmo   revenue    165.75  share  23.6%
  total revenue 700.96 over 14 sale(s)
```

## What it does

`mk.py` writes 87 input files to `data/`, listed in `data/files.txt`, and
`data/jobs.txt` (146 jobs in 9 sections). The vendored
[xlwt](https://github.com/python-excel/xlwt) 1.3.0 (BSD) saves 11 of the
workbooks; the others are BIFF8 streams built record by record, compound
documents built sector by sector, damaged copies and ZIP archives.
`oracle.py` runs each job on the vendored xlrd and prints the result;
`mini_xlrd.kara` runs the same jobs.

- **`dump`** (89 jobs) opens a file with `open_workbook` and prints what
  xlrd wrote to its log file, the book (BIFF version, codepage and
  encoding, date mode, user name, countries), and for each sheet its size,
  its WINDOW2/SCL/PANE view settings and every non-empty cell with its
  type and value. Date cells also show `xldate_as_tuple`. Options:
  `ragged` (`ragged_rows=True`, with the row lengths) and `ondemand`
  (`on_demand=True`, with which sheets are loaded before and after
  `unload_sheet` and `release_resources`). Or the job prints the error.
- **`fmt`** (5 jobs) is `dump` with `formatting_info=True`: the fonts,
  formats and their date classification, XFs, styles and colours used,
  then for each cell its XF index and rich-text runs, and for each sheet
  its merged ranges, COLINFO and ROW records and default widths.
- **`!inspect`** prints `inspect_format` for nine files.
- **`xldate`**, **`fromdate`** and **`fromtime`** (39 jobs) call
  `xldate_as_tuple` with `xldate_as_datetime`, `xldate_from_date_tuple`
  and `xldate_from_time_tuple`. **`names`**, **`cells`** and **`ranges`**
  call `colname`, `cellname`, `cellnameabs` and `rangename2d`.
- **`!csv`** writes one sheet to `data/out/<book>_<sheet>.csv`, with dates
  as ISO text, booleans as `TRUE`/`FALSE` and errors as their Excel text.
- **`!report`** reads region aliases from a hidden sheet, totals the sales
  sheets by region, month and product, and says which rows it skipped.
- **`!find`** searches every text cell for a word, case-insensitively.

## How the port works

- **The compound document.** The header, the MSAT and SAT (with the
  sector-size sanity checks), the SSAT and short-stream container, and
  the directory tree. `locate_named_stream` returns the whole file and an
  offset for a contiguous stream, as xlrd does, so a read past the
  stream's end sees the rest of the file. Repeated sectors and broken
  chains raise the same `CompDocError`s and write the same log
  lines, including xlrd's `dump_list` of a seen-sectors map.
- **The globals.** `getbof` with its version table, then the SST (with
  strings split across CONTINUE records, where the compression flag can
  change at the split), FONT, FORMAT, XF, STYLE, BOUNDSHEET, DATEMODE,
  CODEPAGE, COUNTRY, FILEPASS and WRITEACCESS records, and at EOF the XF
  and palette epilogues. `derive_encoding` keeps xlrd's trial decode of `b'trial'`, using a table
  of the codecs this Python has for `cp300`..`cp1999`.
- **The sheets.** Each cell record goes through `put_cell_unragged` or
  `put_cell_ragged` with xlrd's padding rules and `tidy_dimensions`, and a
  cell without an explicit type takes it from its XF's format, so a number
  under a date format reads as an xldate. FORMULA results follow their
  STRING, SHRFMLA or ARRAY record, with STRING's CONTINUE records.
- **Python's behaviour** is part of the oracle: the messages of
  `struct.error`, `IndexError`, `KeyError` and the UTF-16 decoder, the
  list-index error of a bad SST index, round-half-even in xldate, and
  `repr`/`ascii` of strings and bytes.

xlrd's behaviour shows in the output, and the port keeps it:

- xlwt writes WRITEACCESS as raw ASCII. xlrd reads it as a BIFF8 string,
  so the owner `Zoe Analyst` reads as `'祬瑳'`.
- xlwt saves formulas without results. `SUM(A1:A3)` and `1/0` read back as
  the text `''`.
- xlwt splits the SST at a fixed size, so a string whose surrogate pair
  straddles the split fails with `UnicodeDecodeError: 'utf-16-le' codec
  can't decode bytes in position 6-7: unexpected end of data`.
- A format that is not a date format is classed "general", so
  `'0.00 "kg"'` reads as general rather than number.
- A CODEPAGE of 9999 in a BIFF8 file becomes 1200 (UTF-16) without a
  word, while 1999 fails with `LookupError: unknown encoding: cp1999`.
- `xldate_as_tuple(43831.9999999, 0)` rounds to midnight of the next day,
  while `xldate_as_datetime` gives `2020-01-01 23:59:59.991000`.
- `colname(702)` raises `IndexError: string index out of range`:
  `formula.colname` has only two letters.
- A ZIP file whose end record is missing fails in `inspect_format` with
  `BadZipFile: File is not a zip file`, and an empty ZIP (no local file
  header) is read as BIFF and fails with `Expected BOF record`.

## Compiler gaps

None. `karac check` asked for 98 call-site `mut` markers, which
`karac fix` applied, and refused `XF` and `DT` as struct names (they must
be PascalCase), three bare `String` parameters handed on as owned values,
`Vec[u8]`/`Vec[Str]` annotations on `[...].to_vec()` (the literals stay
`i64` and `String`), `.copied()` on an `Option` (the library has
`.cloned()`) and moving a `Sheet` out of a `Vec` by index. After those
changes the interpreter matched the oracle byte for byte on its first
run, as did the LLVM, ASan and native builds.

## Scope

- BIFF8 only. BIFF2 to BIFF5 workbooks, and the BIFF4W and BIFF5 paths of
  each record, are not ported; such a file fails with a message saying so.
- Not ported, and not in the test data: NAME, EXTERNNAME, EXTERNSHEET,
  SUPBOOK and PALETTE records, hyperlinks, notes, drawing objects,
  conditional formats, label ranges, page breaks beyond their size check,
  and formula decompilation.
- ZIP entry names are decoded as UTF-8 when the flag says so and as
  Latin-1 otherwise, which differs from zipfile's cp437 only for non-ASCII
  names, and none of the archives has one.
- `encoding_override`, `use_mmap`, `verbosity` above 0 and
  `ignore_workbook_corruption` are not ported.

## Running

```bash
karac run mini_xlrd.kara                          # interpret
karac build --mir mini_xlrd.kara                  # native binary
python3 -s -P -B mk.py                            # regenerate data/
python3 -I -B oracle.py > expected.out            # regenerate expected.out
python3 -I -B gen_kara.py                         # regenerate the embedded data
```
