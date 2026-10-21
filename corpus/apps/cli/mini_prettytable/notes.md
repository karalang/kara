# mini_prettytable

Text tables as prettytable 3.16.0 draws them. prettytable is the table
printer behind many Python command line tools, test runners and
database shells. The port covers:
- tables built by rows, by columns and with an index column, with
  dividers, slices, copies and every option of the constructor and of
  `get_string`;
- borders, horizontal and vertical rules, padding, alignment, wrapping
  to a maximum width, minimum and maximum table widths, titles and
  header styles;
- sorting with sort keys, row filters and start/end slicing, before or
  after the sort;
- int, float, None and custom formats;
- the seven preset styles, and ColorTable with its nine themes and a
  custom one;
- CSV, JSON, HTML (simple and formatted), LaTeX and MediaWiki output,
  and pagination;
- tables read from CSV, with the dialect sniffed, and from JSON, HTML
  and MediaWiki;
- six programs:
  - a top-like process list from a `ps` export;
  - a disk report for a status page, in Markdown;
  - a test summary read from JSON, by suite and page;
  - a host inventory scraped from HTML and published to a wiki;
  - a dependency audit with wrapped notes and terminal hyperlinks;
  - an invoice with a total row under a divider.

Text widths come from the tables of wcwidth 0.2.13, which prettytable
uses, written to `data/wcwidth.txt`. The CSV reader, writer and
sniffer come from `mini_csv`, textwrap from `mini_textwrap`, the HTML
parser from `mini_htmlparser` and Python's format specs from
`mini_humanize`.

The run writes `data/` and then works through the 18 jobs in
`data/jobs.jsonl`, 575 operations in all.

`oracle.py` runs the same jobs through prettytable itself and wrote
`expected.out`. It uses the Python files of the 3.16.0 wheel in
`prettytable/` and of wcwidth 0.2.13 in `wcwidth/`, unchanged.

The quirks are kept. Some examples:
- `vrules=FRAME` in the constructor gives ALL, because FRAME is 0 and
  the constructor reads `vrules or ALL`. `left_padding_width=0` there
  is dropped the same way. Set as attributes, both work.
- The `horizontal_align_char` property returns the bottom-left
  junction character, or the junction character.
- `_compute_table_width` gives FRAME tables no border width, so
  `max_table_width` shrinks them less than it should.
- Renaming the fields resets alignment, valign, widths and formats to
  what the constructor was given, then copies the old alignments to
  the new names and keeps the old names in the dict too.
- `max_width` and the formats passed to `get_string` are checked and
  then ignored; only the table's own are used.
- `get_string(vertical_char=...)` reaches the rules and the header,
  while the rows keep the table's own character.
- A literal string `"None"` in a cell is replaced by `none_format` like
  a real None.
- `float_format="6.2f"` prints a trailing `f`, since the format becomes
  `%6.2ff`.
- With `preserve_internal_border` and no border, only the last line of
  a multi-line row loses its right edge.
- Dividers ignore `row_filter`, so they land under other rows, and all
  of them go when the table is sorted.
- Sorting compares `[value] + row`, so ties are broken by the rest of
  the row, and mixed types raise CPython's TypeError, naming the types
  in the order its sort compares them.
- A tab is -1 wide to wcwidth, so its cell is padded one extra space.
- `from_html` pads a short row with one dash fewer than it needs, so
  the row is then refused, and the widest row so far is kept from one
  table to the next.
- `set_style` with an unknown style applies the default style before
  it raises. ORGMODE, once set, stays set.
- ColorTable adds a reset code after every page and every `str()`, and
  `set_style` overwrites the theme's characters.
- LaTeX lines end in `\r\n`.

Not ported:
- `from_db_cursor` and the RANDOM style.

## License

prettytable is under the BSD 3-clause license in
`LICENSE-prettytable.txt`:

    Copyright (c) 2009-2014 Luke Maurits

That covers the files in `prettytable/`. wcwidth is under the MIT
license in `LICENSE-wcwidth`:

    Copyright (c) 2014 Jeff Quast <contact@jeffquast.com>

That covers the files in `wcwidth/`.
