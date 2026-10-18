# mini_tabulate

python-tabulate 0.10.0, the table printer behind many command-line tools
and notebooks, ported from `tabulate/__init__.py`.

- All 37 table formats, from `simple`, `grid` and `pipe` to `html`,
  `latex_longtable`, `mediawiki`, `asciidoc` and the box-drawn grids and
  outlines.
- Column type inference: ints, floats, booleans and text, numbers with
  thousands separators or underscores, `inf` and `nan`, and the strings
  Python's `int()` and `float()` accept or refuse.
- `floatfmt` and `intfmt` through a port of Python's format-spec
  mini-language, with floats printed from their exact decimal value.
- Alignment of columns and headers: `numalign`, `stralign`, `colalign`,
  `colglobalalign`, `headersalign`, `headersglobalalign` and
  `preserve_whitespace`.
- ANSI colour codes and OSC 8 hyperlinks, which take up no width.
- Multiline cells, `rowalign`, separating lines, `missingval`,
  `showindex` and `disable_numparse`.
- Wrapping to `maxcolwidths` and `maxheadercolwidths` with tabulate's own
  copy of `textwrap`, which carries colours across wrapped lines.
- The input shapes: a list of lists, a list of dicts and a dict of
  columns, with `headers` as a list, `"firstrow"`, `"keys"` or a dict.
- The `tabulate` command-line tool, with getopt's errors.

tabulate's quirks are kept too. Some examples: `maxcolwidths` turns the
string `"False"` into `True`, and it raises `ValueError` on a coloured or
comma-grouped number. A `headersalign` change also moves the colons of a
`pipe` table, and of two separating lines in a `grid` table only the
first is drawn.

The run writes 18 JSON tables to `data/tables/` and 6 text files for the
command-line tool to `data/text/`. It then runs `data/jobs.jsonl`, 270
jobs, each printed after `== ` and the job:
- `table` jobs call `tabulate()` on a table with the job's keyword
  arguments;
- `format` jobs format a list of numbers, or strings through `float()`,
  with one float format spec, the way `floatfmt` does;
- `cli` jobs run the command-line tool. `-s` takes `\s+` (the default)
  or a literal separator written with regex escapes such as `\t` or `\|`,
  which covers what the jobs use.

`oracle.py` runs the same jobs through the real tabulate
(`pip install tabulate==0.10.0`, without the optional `wcwidth`, so a
width is a count of code points), and wrote `expected.out`.
