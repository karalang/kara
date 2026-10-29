# mini_pandas_display

A Kāra port of the part of [pandas](https://pandas.pydata.org/) 3.0.5
(BSD-3-Clause) that turns a result into the text a person reads:
`DataFrame.__repr__` and `to_string`, `Series.__repr__` and `to_string`,
with their truncation, wrapping, float and label rules, and the shapes
worth printing (grouped results, MultiIndex rows and columns,
`pivot_table` with margins, `unstack`, `stack`, a transpose, `describe`
and `value_counts`). Like [mini_pandas](../mini_pandas/README.md) it
runs pipelines in a small job language, one step per ` | `:

```
read orders.csv | head 5 | show width=60
read budget.csv | pivot index=dept,category columns=year values=planned agg=sum margins=1
read staff.csv | col bio | show max_colwidth=12
read orders.csv | head 5 | string col_space=12 justify=center
```

| Group | Steps (the pandas call each one is) |
|---|---|
| Reading | `read` (`read_csv` with `sep`, `index` for `index_col`, and `dtype`) |
| Looking | `select`, `where`, `head`, `tail`, `col` |
| Ordering | `sort` (`sort_values(kind="stable")`, `by`, `asc`), `sortidx` |
| Index | `setidx`, `reset` |
| Grouping | `agg` (named aggregation), `size`, `counts` and `share` (`value_counts`, `normalize`) |
| Reshaping | `pivot` (`pivot_table` with `index`, `columns`, `values`, one aggregate or a list, `fill`, `margins`), `unstack`, `stack`, `T`, `describe` |
| Arithmetic | `round`, `mul`, `div` (a column from two others) |
| Printing | `show` (`repr` under `option_context` with `max_rows`, `min_rows`, `max_columns`, `width`, `precision`, `max_colwidth`, `show_dimensions`, `colheader_justify`, `multi_sparse`, `expand_frame_repr`); `string` (`to_string` with `index`, `header`, `na_rep`, `float_format`, `col_space`, `justify`, `sparsify`, `index_names`, `max_rows`, `min_rows`, `max_cols`, `show_dimensions`, `decimal`, `line_width`, `max_colwidth`, and a Series' `length`, `dtype` and `name`); a job with neither ends in `repr` |

In a text argument `\s` stands for a space and `\e` for the empty
string, since the job language splits on spaces.

## Running it

The program carries its inputs: it writes `data/` and runs
`data/jobs.txt`, 179 jobs over six files:

- 96 orders (region, channel, product, units, price, a discount that is
  often missing, revenue, a gift flag, and a free-text note holding a
  long sentence, a tab and a quoted line break);
- a lab's assay results, from `-8e-10` to `3.2e9`, so the float rules
  switch to scientific notation;
- twelve hourly readings of 25 sensors, wide enough to wrap at 80
  columns and to cut columns at 20;
- a staff list with names in three scripts and a long bio;
- three years of budget lines by department, quarter and category
  (no 2026 Q4), for pivot tables with gaps and margins;
- a file of edge values: 2^53 + 1, `-0.0`, a ratio of 0.1234565 that
  sits on a rounding edge at six decimals, and a column whose 12345678.9
  pushes it into scientific notation.

Each job prints `$ <job>`, then what pandas prints for the result, or
`! ` and the exception pandas raised.

```
python3 -s -P -B mk.py          # writes data/ and data/jobs.txt
python3 -I -B gen_kara.py       # copies data/ into the end of mini_pandas_display.kara
python3 -I -B oracle.py > expected.out
karac run mini_pandas_display.kara | diff - expected.out
```

`oracle.py` is the oracle, and it is pandas itself (pandas 3.0.5 on
numpy, without bottleneck or pyarrow). It prints with
`display.max_columns` at 20, its value outside a terminal; in a
terminal pandas sizes the frame to the window instead.

## Parallel work

Every job runs in its own `TaskGroup` task, and so do each `agg`
aggregate and each column `describe` summarises. The results join in
order, so the transcript is the same on every run and thread count. The
program matches `expected.out` on `karac run`, `__mir-llvm-run`,
`__mir-llvm-asan` (no reports) and a `karac build` binary, at
`KARAC_THREADS=1` and `4`, and on five runs of an auto-par build at
`KARAC_THREADS=4`.

## pandas' own code, ported

- **DataFrameFormatter** (`io/formats/format.py`): `_truncate_horizontally`
  and `_truncate_vertically`, with `_adjust_max_rows` (`min_rows` only
  when the frame is longer than `max_rows`); the column labels, where a
  flat label gets a leading space when its column is numeric (bools
  count); the index levels, fixed-width and adjoined, under one line per
  column level that holds the column index names; and every column
  padded to its header and justified by `colheader_justify`.
- **StringFormatter** (`io/formats/string.py`): the `...` column and row,
  `_join_multiline` with `_binify` (the ` \` that ends every block but
  the last), the `Empty DataFrame` text and `[N rows x M columns]`.
- **SeriesFormatter**: the centred dots, the index names line, and the
  footer (`Name`, `Length` when truncated, `dtype`), with `repr`'s
  defaults and `to_string`'s different ones (no footer, and
  `max_colwidth` still read from the option).
- **format_array**: FloatArrayFormatter's fixed `% .6f` with the zeros
  trimmed jointly, and its switch to `e` when a value is below
  `10^-precision` or when a value over `1e6` makes the text too long;
  `float_format` as a `%` string with no fixed width; the decimal comma;
  the object formatter's own `precision` rule; `_make_fixed_width` and
  its `max_colwidth` cut (`...` only when the cap is over 3).
- **Index formatting**: `_format_flat` (text labels as they are,
  numbers padded on the right, then `trim_front`), RangeIndex's own
  width rule, MultiIndex `_format_multi` and `sparsify_labels`.
- **The reshapes** the output depends on: `pivot_table`'s sorted
  columns, fill, margins (`All` row and column, a grand total from the
  pairwise sum) and a list of aggregates as a third column level;
  `unstack` (an int column becomes float when a cell is missing),
  `stack` (pandas 3's order of appearance and common dtype), `T` (one
  common dtype across all columns), `value_counts` (ties in first
  appearance) and `describe` (numpy's linear quantiles).
- **printf's `%f`, `%e` and `%g`**, and Python's `{: .6f}` and `repr`
  of a float, on the double's exact decimal value, as in mini_pandas_io.

Two pandas quirks are kept because the jobs show them: with room for
one row (`max_rows=20 min_rows=1`), the frame shows its first
`max_rows` rows, not one; and a cut row's dots take the width of the
line at that position in each column, header lines included.

## The Mend loop

The first `karac check` stopped at four parse errors, all Rust habits:
three closures declared their return type (`|s: String| -> String`) and
one `collect::[Vec[String]]()` used a turbofish. Five runs of
`karac fix` applied 35 edits: the four deletions, then `own` on
parameters that hand their value on, `.clone()` where a borrowed value
was pushed or matched, and two `*n` that were already copies. That left
12 errors:

- a `fold` as an enum constructor's payload took the constructor's type
  as its accumulator (B-2026-10-29-5); the app binds the fold first;
- the inner `collect` of a `map(..collect()).collect()` inside an `if`
  could not infer, though the same expression is fine as a function's
  tail (B-2026-10-29-6); the app binds it to a typed `let`;
- the closure whose `-> Vec[String]` the fix had deleted could no
  longer infer its body (B-2026-10-29-7); the app writes
  `let row: Fn(i64) -> Vec[String] = |r: i64| ..`;
- the turbofish fix in the middle of a chain left
  `no method 'join' on type parameter 'C'` (B-2026-10-29-8); the app binds the
  collect to a typed `let`.

The check was then clean, and the MIR build found two more:
`[None].to_vec()` into a typed `Vec[Option[String]]` does not lower
(B-2026-10-29-9), and `n ?? String.new()` on a loop item, which moves out of a
borrow, is refused by the build but passed by `karac check` (B-2026-10-29-10). With
those written as `let none: Option[String] = None; [none].to_vec()` and
`n.clone() ?? ..`, the first run matched pandas on all 2,292 lines.
`mini_pandas_display.orig.kara` is the program as first written.

## Gaps found

- **B-2026-10-29-5** (open): `Val.I(v.iter().fold(0, ..))` as a function's tail
  is refused; the fold takes `Val` as its accumulator.
- **B-2026-10-29-6** (open): an `if` or `match` with a known type does not pass it
  into its branches, so a nested `collect` there cannot infer.
- **B-2026-10-29-7** (fixed in kara 0a5d255d8a): the closure return-type fix deletes the type the body
  needed; it could move it onto the binding as an `Fn` type.
- **B-2026-10-29-8** (fixed in kara 0a5d255d8a): the turbofish fix mid-chain deletes the type and
  leaves an error at the next method.
- **B-2026-10-29-9** (fixed in af7bbf1e39): `[None].to_vec()` checked but did not
  build on MIR; the app now writes it that way.
- **B-2026-10-29-10** (fixed in kara 03e92379b1): `??` on a `ref Option[String]` loop item passes
  check and is refused by the build.

## What printing would need (notes for M4c)

Compared with `docs/library/data.md` in the kara repo, whose `Display`
for `DataFrame` is "the header, then at most the first 10 and last 10
rows, with `null` for a null slot and a final line of the shape":

- **One documented layout.** The page should say what a cell looks like
  for each element type: floats here follow one rule per column (a
  shared number of decimals, zeros trimmed together, scientific notation
  for the column when a value would show as zero or run too long),
  which is what makes a column of money or measurements readable. A
  per-cell shortest round-trip, as `println` prints a float, does not
  line up.
- **Alignment and width.** Numbers right-aligned, labels left-aligned,
  a cap on a cell's width with an ellipsis (a free-text column is the
  usual reason a table is unreadable), and widths counted in characters
  so that names in other scripts stay aligned. East Asian wide
  characters are a further choice the page should make explicitly;
  pandas counts them as one by default.
- **Truncation both ways.** Rows (first and last N, and an option for
  N) and columns: a frame of 26 columns needs either a cut with a `...`
  column or wrapping into blocks at a line width. The page has rows
  only.
- **Options.** A way to print the whole frame, or a different number of
  rows, columns, decimals or width, without changing global state:
  pandas' `option_context` and `to_string` arguments are the two shapes.
  An explicit `to_string(opts)` fits Kāra better than process-wide
  options, which would be shared mutable state across tasks.
- **Grouped and reshaped results.** `data.md`'s frames have no index;
  group keys are ordinary columns, which prints well enough. A pivot's
  two-level column header (value, then the pivoted key) and its margins
  are what a report actually is, and need either a header with several
  lines or a documented naming rule for the flattened columns
  (`planned_2024`).
- **Missing values.** One marker per element type (`null` on the page;
  pandas prints `NaN` for both a float NaN and a missing string), and a
  way to choose it, as `na_rep` does.
