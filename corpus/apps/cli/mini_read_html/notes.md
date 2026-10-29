# mini_read_html

A Kāra port of [pandas](https://pandas.pydata.org/) 3.0.5's `read_html`
(BSD-3-Clause) with its lxml flavor, and of the libxml2 2.14 HTML parser
under lxml 6.1 (MIT). Scraping a table out of a web page is how a lot of
data first reaches a notebook: a Wikipedia list, a league table, a
statistics release, a bank's statement export that is really an HTML
file, a page from the 1990s that nobody will ever turn into a CSV. So
the port has three parts: an HTML parser that builds the tree libxml2
builds, `read_html`'s table extraction on that tree, and pandas'
`TextParser`, which turns the rows into typed columns. Jobs come one per
line:

```
read cities.html match=Population extract_links=body head=3
read standings.html attrs=id:standings index_col=0,1
read statement.html attrs=id:tx thousands=. decimal=, dtype_backend=numpy_nullable
read prices.html attrs=id:bundles skiprows=0:3:2
```

A `read` job is `pd.read_html("data/<file>", ...)` with `match`,
`attrs`, `header`, `index_col`, `skiprows` (a count, a list or a slice),
`thousands`, `decimal`, `na_values`, `keep_default_na`, `displayed_only`,
`extract_links`, `encoding`, `flavor` and `dtype_backend`. It prints the
number of tables, then for each one its shape, its dtypes and `to_csv`
(`head=n` prints only the first n rows).

## Running it

The program carries its inputs: it writes `data/` and runs
`data/jobs.txt`, 99 jobs over 10 pages that `mk.py` writes, each shaped
like a page a scraper meets:

- **cities.html**, an encyclopedia article: an infobox, a sortable table
  with `rowspan` regions, sort keys hidden with `display:none`, footnote
  markers, `<br>` inside cells, `&minus;` and `&nbsp;`, a table with a
  caption, and a navigation box.
- **standings.html**, a league table with a two-row header (`colspan`
  and `rowspan`), links to each team, and a `tfoot` note, plus a fixtures
  table with no `thead`.
- **statement.html**, a bank's export in **windows-1252** declared by a
  `<meta http-equiv>`, with German numbers (`1.150,00`).
- **release.html**, a statistics release: a layout table holding the
  data tables, a hidden column, a hidden table, a `<style>` inside a
  table, `..`, `x` and `–` as missing-value markers, an empty table and
  a blank one.
- **legacy.html**, 1990s markup: upper-case tags, unclosed `<TD>` and
  `<TR>`, unquoted attributes, and a `document.write` of a table inside
  a script comment.
- **prices.html**, with a byte-order mark, a `thead` whose cells sit
  outside any row, a short row, an empty row, and French prices
  (`1 299,00`).
- **latin1.html**, ISO-8859-1 with no declaration, which libxml2 guesses.

The other three are the broken inputs people meet: a page with no
tables, a fragment, an empty file, and a file that is not there. Each
job prints `$ <job>`, then what Python prints, or `! ` and the exception
(17 of the 99: no table matching, a bad regex, an unknown encoding name,
an `extract_links` value, an `index_col` or `header` past the table, an
empty document, a missing file).

```
python3 -s -P -B mk.py          # writes data/ and data/jobs.txt
python3 -I -B gen_kara.py       # copies data/ into the end of mini_read_html.kara
python3 -I -B oracle.py > expected.out
karac run mini_read_html.kara | diff - expected.out
```

`oracle.py` is the oracle: pandas 3.0.5 with lxml 6.1.3 (libxml2
2.14.6) on Python 3.13. html5lib is not installed, so every job names
the lxml flavor, the one pandas tries first.

## What libxml2 and pandas do, and so the port does

These are their answers, kept because `expected.out` is theirs:

- **libxml2 is not an HTML5 tree builder.** It tokenizes as HTML5 does
  but builds the tree with HTML 4 rules: it adds no `tbody`, so
  `.//tbody//tr` finds nothing in a table written without one; a start
  tag closes the open elements its table says it closes (`<TR>` closes
  `<TD>`, `<P>` inside a cell closes nothing outside it); a misplaced
  `html`, `head` or `body` is dropped.
- **Encodings.** A byte-order mark or a `<meta charset>` or
  `http-equiv` switches the decoder at the point it is read. With
  nothing declared, the first byte that is not ASCII makes libxml2 guess,
  and invalid UTF-8 becomes ISO-8859-1 from there on. `encoding="utf-8"`
  on the windows-1252 statement keeps the bytes as UTF-8, so `Währung`
  reads as `W�hrung`; `encoding="latin-1"` is a `LookupError`, because
  libxml2 knows `latin1` and `ISO-8859-1` but not that spelling.
- **`match` runs on each text node**, script text included, and the
  table must contain the node: `communes of metropolitan` is in a
  paragraph, so no table matches. The pattern reaches the XPath through
  `repr`, so a backslash in it doubles.
- **Hidden things go first, for every table at once.** `display:none`
  is checked with spaces removed and nothing else, and dropping happens
  on the shared tree before any table is read, so the layout table's
  cell text has lost the hidden column, the `<style>` text and the
  hidden table.
- **Nested tables leak.** `.//thead/tr` and `.//tbody//tr` search the
  whole subtree, so the layout table's header and body rows include the
  data table's, and it reads as seven rows.
- **The header.** With no `thead`, leading rows of only `th` cells are
  the header. Several header rows make a MultiIndex; a blank header cell
  is `Unnamed: 4`, and repeats become `Price.1`.
- **`thousands=","` is the default** and runs only on cells that match
  pandas' number pattern, `^[\-\+]?([0-9]+,|[0-9])*(\.[0-9]*)?...$`, so
  `84,37` and `89,90` become `8437` and `8990` while `2,102,650[a]`
  keeps its commas. Header rows are not touched, so `skiprows=2` names a
  column `121,583`.
- **Types come from the values.** One `..` makes a column `str`; an
  empty cell is NaN and makes integers `float64`; `numpy_nullable` gives
  `Int64`, `Float64` and `string`. `extract_links` makes `(text, href)`
  tuples, and a column holding one is `object`.

## Parallel work

Every job runs in its own `TaskGroup` task and the results join in
order, so the transcript is the same on every run and thread count.
The program matches all 1,435 lines of `expected.out` on `__mir-run`,
`__mir-llvm-run`, `__mir-llvm-asan` (no reports) and a `karac build`
binary, at `KARAC_THREADS=1` and `4`, and on five runs of the auto-par
build (0.06 s real or less).

## The code, ported

About 5,900 lines before the data section, about 2,300 of them reused:
from **mini_re**, Python's `re` (parser, compiler and matcher), which
`match` and the number pattern run on; from **mini_read_excel**,
Python's number parsing, the NA value sets, pandas' column inference
and `to_csv`.

The new code is the rest:

- libxml2's HTML parser: the tokenizer with its raw-text, RCDATA and
  script states, character references and the HTML5 entity table, the
  element table (which elements have no end tag, which start tags close
  which, end-tag priorities), the implied `html`, `head` and `body`, the
  encodings, and SAX2's text merging. Its tree matches lxml's on every
  page, checked against dumps of `lxml.html.parse`;
- `read_html` on that tree: the `br` tails, the table query, `attrs`,
  `displayed_only`, the header, body and footer rows, `colspan` and
  `rowspan` with their overflow into the footer, `extract_links`, and
  `_remove_whitespace`;
- `TextParser` on the python engine: header rows and MultiIndex headers
  with their extra row, `index_col` lists, `skiprows` with the reader's
  buffer, `thousands` and `decimal`, nullable dtypes, empty frames, and
  `to_csv` with MultiIndex columns.

## The Mend loop

`karac check` reported five errors, and `karac fix` applies a fix to
each; I applied them by hand from the messages: three `match` arms that
moved an owned `Option` they only cloned (`Some(ref v)`), a bare `for`
element pushed into another vector (`.into_iter()`, where the fix writes
`.clone()`), and `str.contains` given a `char`. In a scratch copy with
those errors put back, `karac fix` applied all of them. The first run
matched `expected.out` on the interpreter.

## Gaps found

None in the compiler. Both a temporary tree dump and the whole program
matched their oracles the first time they ran.

## What reading HTML tables would need (notes for M4c)

Compared with `docs/library/data.md` in the kara repo, which reads CSV
and Arrow IPC:

- **A reader**: `read_html(path; match, attrs) -> Result[Vec[DataFrame],
  DataError]`, or a `parse_html_tables(text)` that returns the rows so a
  caller can build the frame. A page holds many tables, so the result is
  a list and the selector (`match`, `attrs`) is part of the call.
- **The rows before the frame.** Most of the work here is between the
  page and the frame: spans, headers, footers, hidden cells. A
  `Vec[Vec[String]]` with the header row count is the useful middle
  step, and `DataFrame.from_rows(rows; header)` (Q9) would take it.
- **A header of several rows.** `data.md` names columns with one
  `String`. Two-row headers (`Home / W`) are common in tables; joining
  the levels into one name, or a `MultiIndex`, needs a decision.
- **Number formats.** Q13's inference reads `1,014` and `1.150,00` as
  `String`. A reader for pages needs `thousands` and `decimal` options,
  and should match them more strictly than pandas does, since pandas
  turns `84,37` into 8437.
- **Missing-value markers.** Statistics pages use `..`, `x`, `–` and
  `-`. `null_value` takes one string; a list is what these need.
- **Links.** `extract_links` keeps an href with the text. A frame needs
  a struct or a second column for that, since `data.md` has no tuple
  element type.
- **Encodings.** Pages arrive in windows-1252 and ISO-8859-1 as well as
  UTF-8, and declare it inside the page; a reader needs an `encoding`
  option and the meta-tag rules, or a step that decodes first.
- **Errors**: `DataError` would need a variant for "no table matched",
  which is the error people meet most.
