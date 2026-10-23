# mini_texttable

A port of [texttable](https://pypi.org/project/texttable/) 1.7.2 (MIT,
Gerome Fournier): the module that command-line tools use to print ASCII
tables, with wrapped cells, per-column alignment and number formatting.

```
> rows header=True [['Name', 'Age', 'Nickname'], ['Mr\nXavier\nHuon', 32, "Xav'"], ...]
> draw
    +----------+-----+----------+
    |   Name   | Age | Nickname |
    +==========+=====+==========+
    | Mr       |     |          |
    | Xavier   |  32 |          |
    | Huon     |     |   Xav'   |
    +----------+-----+----------+
```

## What it does

`mk.py` writes 20 jobs of 441 calls on a `Texttable` to `data/jobs.tsv`:
the tables an operations team's tools print. `oracle.py` replays them
through the vendored library, and `mini_texttable.kara` replays them
through the port. Each call prints with its arguments as `ascii()` shows
them; `draw()` prints the table (97 of them) or `-> None`. What the library
writes to stderr prints as `stderr:`, and an exception as
`error <Class>: <message>`, after which the job goes on.

- **The module's own demo**, then a server inventory, a cost report and
  a ticket list, drawn with and without each alignment, width limit and
  decoration.
- **Cell formatting.** The automatic type (`a`) over ints, floats, bools,
  `None`, bytes and strings that `float()` does or does not read (`" 42 "`,
  `"1_000"`, `"1__0"`, `"-Infinity"`, `"NaN"`, Arabic-Indic and fullwidth
  digits). The `t`, `f`, `e`, `i` and `b` types side by side, with
  `round()`'s ties to even, exact digits for `1e20` and `2**70`, and the
  `ValueError` and `OverflowError` that `int(round(...))` raises for NaN
  and infinity. Precisions from 0 to 20, and the values `set_precision`
  refuses.
- **Decoration.** All 16 combinations of `BORDER`, `HEADER`, `HLINES`
  and `VLINES`, plus -1 and 32, and drawing characters that are too long,
  empty, not strings, box-drawing or fullwidth.
- **Alignment.** `l`, `c` and `r`, `t`, `m` and `b`, header alignment,
  odd and even padding, and unknown values (which fall back to left and
  top).
- **Wrapping.** `max_width` from 80 down to the point where the library
  refuses with `max_width too low to render data`, the round-robin shrink
  of column widths, long URLs and hyphenated words, and explicit column
  widths, including values `set_cols_width` rejects (and the stderr
  line it writes first).
- **Whitespace.** Tabs measured to tab stops of 8, blank and
  whitespace-only lines inside a cell, carriage returns, form feeds and
  no-break spaces.
- **Unicode.** Japanese and Chinese cells (two columns wide, so a wrapped
  line can overflow its column), emoji, a combining accent (zero wide),
  halfwidth katakana and fullwidth letters.
- **Bytes.** Cells and headers given as bytes, decoded as UTF-8, with the
  `UnicodeDecodeError` line the library writes to stderr before decoding
  with replacement characters.
- **Errors and state.** Rows of the wrong size, `add_rows` on an empty
  list, an unknown datatype (`KeyError`), and `reset()`, which keeps the
  alignment, datatypes and explicit widths of the old table: a table of
  more or fewer columns then draws with the library's `zip` truncation,
  or fails with `IndexError`.

## How the port works

- **The table follows `texttable.py` method by method**, with the
  attributes that the library only has once they are set (`hasattr`)
  held as `Option`s, so `reset()` keeps exactly what it keeps.
- **Every `zip` is a loop to the shortest list**, and every list index
  that can fail returns the library's `IndexError`.
- **Cell widths use the library's fallback `uchar_width`**, since
  `wcwidth` is not installed: 2 for East Asian Wide and Fullwidth, 0 for
  a combining character. `gen_kara.py` writes those ranges from
  `unicodedata` into `width_text()`, with the runs of decimal digits that
  `float()` and `int()` read.
- **Wrapping is `textwrap.wrap` with its defaults**, taken from
  `mini_textwrap`. It counts code points, as the library's fallback does.
- **Numbers are formatted from their exact decimal value**, taken from
  `mini_markupsafe`: `'%.*f'`, `'%.*e'`, `repr()` and `round()` all round
  the exact binary value half to even, as CPython does. `float()` of a
  string rounds correctly too.
- **`gen_kara.py` writes `jobs_text()` and `width_text()`** at the end of
  the `.kara` file, so the kata reads no files at run time.

## Compiler gaps

None. `karac fix` made the one edit the first draft needed: a `for` over
the lines `wrap` returns borrows each one, so pushing it needed a
`.clone()`. The port then matched the oracle on every backend.

## Scope

- **Cell datatypes are the six named ones.** A callable datatype is not
  ported.
- **`str.isspace()` is Unicode `White_Space` plus U+001C..U+001F**, and
  `\w` in the wrapping rules is `is_alphanumeric` or `_`, as in
  `mini_textwrap`.
- **Ints stay within i64**, apart from the demo's 23-digit int, which is
  held as its digits (it only ever prints or converts to float).
- **Only the library's fallbacks are ported**: with `cjkwrap` or
  `wcwidth` installed, texttable measures and wraps differently.

The vendored library is unchanged.

## Running

```bash
karac run mini_texttable.kara              # interpret
karac build --mir mini_texttable.kara      # native binary
python3 -s -P -B mk.py                     # regenerate data/jobs.tsv
python3 -I -B oracle.py > expected.out     # regenerate expected.out
python3 -I -B gen_kara.py                  # regenerate jobs_text()
```
