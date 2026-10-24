# mini_xlsxwriter

A port of [XlsxWriter](https://github.com/jmcnamara/XlsxWriter) 3.2.0
(BSD-2-Clause, John McNamara), the pure-Python library that writes Excel
`.xlsx` files. It covers what a report generator uses: cell writes of every
kind, cell formats, column and row settings, merged ranges, hyperlinks,
rich strings, autofilters, data validation, tables, defined names, panes,
page setup, sheet protection and document properties, packaged the way
XlsxWriter packages them. Every XML part comes out byte for byte, then a zip
is written to disk, read back and checked.

```
== inventory.xlsx
  49 s1.add_table warns: Duplicate header name in add_table(): 'same'
  53 s1.merge_range raised OverlappingRange: Merge range 'H3:I3' overlaps previous table range 'H1:I4'.
  16 parts, 2 sheets, 57 strings (91 uses), 6 cell formats, 3 tables, 0 names
  xl/worksheets/sheet1.xml                    8641 917897f1
  xl/tables/table1.xml                        1016 a35e9a82
```

## What it does

`mk.py` writes `data/jobs.txt`: seven workbooks, 6,692 calls, one call a
line with typed arguments, so the oracle and the port rebuild the same
Python values. `oracle.py` runs the jobs on the vendored XlsxWriter, in
memory, and prints every call that returned something other than 0 or
`None`, raised, or warned, then each book's counts and every part of the
file with its size and CRC-32. `mini_xlsxwriter.kara` runs the same jobs,
which `gen_kara.py` embeds, and prints the same lines.

- **regional_sales.xlsx** is a quarterly report: document properties, nine
  formats (two with the same properties, which share a style), 200 sales
  rows with dates, money and percentages, a merged title, an autofilter
  with a column filter, a frozen header, print titles, a print area, page
  breaks, a header and footer, a summary sheet of formulas and a lookup
  sheet, and defined names, four of them refused.
- **inventory.xlsx** sets `default_format_properties` (Arial 10) and holds a
  table with a total row, a calculated column and column formats, 17 data
  validations (lists, ranges, lengths, custom formulas, and each way one is
  refused), sheet protection with a password, unprotected ranges, a second
  sheet of tables that are refused for their names, headers or options,
  merges that clash with them, outlined and hidden rows, and `autofit()`.
- **timesheet.xlsx** sets `default_date_format` and `strings_to_numbers`:
  datetimes, dates, times and timedeltas (1900's edge dates included), URLs
  of every kind (external, internal, `mailto:`, local files, anchors,
  tooltips, a link overwritten, one too long), a very hidden sheet, the
  active and first sheets, and manual calculation.
- **edge_cases.xlsx** sets `nan_inf_to_errors`, `use_future_functions`,
  `strings_to_urls=False` and a short `max_url_length`: refused sheet names,
  17 formats over every font, fill, border and alignment setting, escapes
  and control characters, NaN and infinity, long numbers, formulas with
  future, dynamic and array functions, writes outside the sheet, rich
  strings (and each way one is refused), merges, every page setup option, a
  filter list with blanks, panes over a selection, and quoted sheet names in
  defined names.
- **broken.xlsx** has two tables whose names differ only in case, so
  `close()` raises and no file is written; **empty.xlsx** has no sheet, so
  `close()` adds one; **ledger.xlsx** is 1,500 journal rows with
  `autofit()` and a trial balance of `SUMIF` formulas.

## How the port works

- **Values** are a `Val` enum shaped like the Python values the jobs pass
  (`None`, `bool`, `int`, `float`, `str`, the four datetime types, a format,
  lists and dicts in insertion order). A format property keeps the value it
  was set to, because the format's key is built from its `str()` and the
  XML writes it with `%s`.
- **Formats** get their xf index the way `_get_xf_index()` does, lazily, in
  the order the worksheets ask while they are written, so a format whose key
  is already taken shares its index but keeps none of its own; a table
  column's dxf index is taken when the table is added. `styles.xml` follows
  the workbook's `_prepare_*` passes, which change fill colours in place.
- **Cells** are kept in one sorted map keyed by row and column, so writing
  a sheet walks its cells instead of its dimension (one sheet reaches
  `XFD1048576`). Spans, row attributes, the style fallback from cell to row
  to column, `%.16G` numbers and every cell type follow `_write_cell`.
- **Formulas** go through `_prepare_formula`'s 155 `re.sub()` rules, read
  off the vendored source by `gen_kara.py`, with `\b` and `.` matched as
  Python matches them, including the rule written `\SINGLE\(`, which is
  `\S` followed by `INGLE(`: `=LEN(MINGLE(D1))` comes out as
  `LEN(_xlfn.SINGLE(D1))`. A dynamic array function makes the formula an
  array formula and adds `metadata.xml`.
- **Quirks kept**: `_write_hyperlinks` carries the display text of a link
  on a non-string cell over to the external links after it (the timesheet
  overwrites one link with a number, and two later links show its URL);
  and a rich string with a theme or indexed colour raises
  `AttributeError`, because its font writer writes to the worksheet's file
  handle before there is one.
- **The zip** is written stored rather than deflated (the oracle prints
  each part's size and CRC-32, not its compressed form), read back from
  disk, and every part's CRC-32 recomputed.

## Compiler gaps

None. `karac check` raised what the code got wrong: a closure written with
a Rust return type, a Rust `[T; N]` array type, four bare parameters given
away (`own` was wanted), call-site `mut` markers missing on 19 calls, `dyn`
used as a field name, `char.from_u32` (Kāra spells it `char.try_from`), a
`Str` passed where a `ref String` was wanted, and a `*` move out of a
borrowed tuple of `String`s. I made those edits by hand; on a reduced copy,
`karac fix` writes the closure, `own` and `mut` fixes itself and leaves
`from_u32` to the reader, as its message says. With them, the MIR
interpreter, the LLVM backend, the ASan build and the native binary all
matched the oracle byte for byte on the first run, with nothing on stderr.

## Scope

- No charts, images, comments, VML, conditional formats, sparklines,
  chartsheets, VBA projects, custom document properties or
  `constant_memory` mode. `filter_column()` (the expression form) is not
  ported; `filter_column_list()` is.
- Split panes are ported but no job uses them; `date_1904` and
  `remove_timezone` are not ported, and no datetime carries a timezone.
- The jobs pass arguments positionally; keyword arguments are not
  supported.
- The library deflates its zip parts; the port stores them.

## Running

```bash
karac run mini_xlsxwriter.kara                    # interpret
karac build --mir mini_xlsxwriter.kara            # native binary
python3 -s -P -B mk.py                            # regenerate data/
python3 -I -B oracle.py > expected.out            # regenerate expected.out
python3 -I -B gen_kara.py                         # rebuild mini_xlsxwriter.kara
```

`gen_kara.py` builds `mini_xlsxwriter.kara` from the port's six pieces
(`p1.kara` to `p6.kara`) and appends the generated functions after its MARK
line. Each run writes its workbook files to the current directory, reads
them back and removes them.
