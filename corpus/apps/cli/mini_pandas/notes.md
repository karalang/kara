# mini_pandas

A Kāra port of the core of [pandas](https://pandas.pydata.org/) 3.0.5
(BSD-3-Clause): a column-typed `DataFrame`, `read_csv` with pandas' type
inference, and about sixty of the operations a data-cleaning or
reporting script reaches for, each computed with the kernel pandas uses
for it, so that every float prints with the digits pandas prints. The
program runs pipelines of these operations written in a small job
language, one step per ` | `:

```
read orders.csv | assign total = qty * unit_price | agg region,status total:sum,total:mean
read readings.csv | where sensor == 's1' | rolling temp 3 mean avg min=1 | select day,temp,avg
read orders.csv | merge customers.csv on=customer how=outer | select order_id,customer,segment
```

| Group | Steps (the pandas call each one is) |
|---|---|
| Reading | `read` (`read_csv` with `sep`, `na_values`, `keep_default_na`, `header=None`, `usecols`, `dtype`), `dtypes` |
| Selecting | `head`, `tail`, `select`, `drop`, `rename` |
| Filtering | `where` (`== != < <= > >=` against a number, a `'string'` or `@column`), `isna`, `notna`, `isin`, `contains` (`case=False`), `startswith`, `between` |
| Columns | `assign` (`+ - * /` on two columns or a column and a number), `round`, `upper`, `lower`, `strip`, `len`, `astype` (`int`, `float`, `str`), `clip`, `abs` |
| Missing values | `fillna`, `ffill`, `bfill`, `dropna` (`subset`, `how="all"`) |
| Sorting and duplicates | `sort` (several keys, per-key `ascending`, `na_position`), `dedup` (`drop_duplicates` with `subset` and `keep` first, last or `False`) |
| Grouping | `agg` (named aggregation of `sum`, `mean`, `count`, `size`, `min`, `max`, `median`, `std`, `var`, `nunique`, `first`, `last`), `size`, `transform`, and the group-wise `cumsum`, `shift` and `diff` |
| Combining | `merge` (`inner`, `left`, `right`, `outer`, `on`, `left_on`/`right_on`, `suffixes`), `concat` |
| Reshaping | `pivot` (`pivot_table` with `aggfunc` and `fill_value`), `melt` (`id_vars`, `value_vars`, `var_name`, `value_name`), `crosstab` |
| Windows | `rolling` (`mean`, `sum`, `max`, `min`, `count`, `min_periods`), `cumsum`, `cummax`, `shift`, `diff`, `pct` (`pct_change`) |
| Ranks and counts | `rank` (`average`, `min`, `max`, `first`, `dense`, descending), `counts` (`value_counts`, `normalize`) |
| Summaries | `describe`, `quantile` |

The column types are pandas' own: `int64`, `float64`, `bool`, `str` and
`object`, with `NaN` as the missing value of a float column and of a
`str` one. An int column that gains a missing value (a `shift`, a left
merge, a pivot cell nobody filled) becomes `float64`, as in pandas.

## Running it

The program carries its inputs: it writes `data/` (a shop's order
export with missing quantities and an accented customer, a customer
table with a duplicated row, returns that refer to a missing order, the
next quarter's export with an extra column, sensor readings with gaps,
staff with salary ties, a survey with skipped answers, a tab-separated
access log, a semicolon-separated European export with `-` for a
missing value, a file with one column for each spelling `read_csv` has
an opinion on, and a header-only file) and runs `data/jobs.txt`, 159
jobs. Each job prints `$ <job>`, then the result as
`to_csv(index=False)` writes it (`describe` and `quantile` keep their
index), or `! ` and the exception pandas raised.

```
python3 -s -P -B mk.py          # writes data/ and data/jobs.txt
python3 -I -B gen_kara.py       # copies data/ into the end of mini_pandas.kara
python3 -I -B oracle.py > expected.out
karac run mini_pandas.kara | diff - expected.out
```

`oracle.py` is the oracle, and it is pandas itself: it runs each job
through the pandas call the step names. It needs pandas 3.0.5 on numpy
with neither bottleneck nor pyarrow installed, since both change which
kernels and which string arrays pandas uses.

## Parallel work

Every job runs in its own `TaskGroup` task; inside a job each `agg`
aggregate and each `describe` column is computed in a task of its own,
and the results join in order, so the transcript is the same on every
run and every thread count. The program matches `expected.out` on
`karac run`, `__mir-llvm-run`, `__mir-llvm-asan` (no reports) and a
`karac build` binary, at `KARAC_THREADS=1` and `4`, and five runs of an
auto-par build at `KARAC_THREADS=4`.

## Matching pandas' digits

pandas does not sum floats the obvious way, and a port that does
prints a different last digit in about one aggregate in ten. Each
kernel here is the one pandas runs:

- `groupby` sums and means are Kahan-compensated (`group_sum`,
  `group_mean` in `_libs/groupby.pyx`), `std` and `var` are Welford's
  update with `ddof=1`, and `median` averages the two middle values.
- A column's own `mean` (in `describe`) is numpy's `pairwise_sum` (eight
  running partial sums, split in halves above 128 values), and its
  `std` is the two-pass `nanvar`.
- `quantile` and `describe`'s quartiles are numpy's linear method,
  including its two-sided lerp (`b - diff * (1 - g)` once `g >= 0.5`).
- `rolling` sums and means keep separate compensations for the values
  entering and leaving the window, give the exact product for a run of
  equal values, and clamp a mean whose sign disagrees with every value
  to 0 (`roll_sum`, `roll_mean` in `_libs/window/aggregations.pyx`).
- `round` is numpy's `rint(x * 10^d) / 10^d`, which rounds half to even,
  and floats print as Python's `repr` does.

## pandas' own quirks, kept

- **Inner-merge order.** `merge(how="inner")` returns the rows in left
  order, except when the number of matched pairs equals the number of
  left rows: `libjoin.inner_join` then inverts the left sorter instead
  of sorting the pairs, a shortcut that assumes no left row matched
  twice. Here one order has no customer and `hooli` appears twice in the
  customer table, so the counts agree and the first `hooli` copy lands
  after order 1011. The join is written as pandas writes it (key codes
  in order of first appearance, counting sorts, the shortcut), so the
  order is pandas' order.
- `value_counts` breaks a tie in count by first appearance, which holds
  while there are fewer than 16 distinct values (above that numpy's
  sort is not stable); every job here is under that.
- `pivot_table` drops the combinations whose aggregate is missing and
  the columns that are entirely missing, and its result is `int64` only
  when the aggregate is an int and a `fill_value` filled every hole.
- `concat` fills a column one side lacks with `NaN`, which makes an int
  column float, and with a missing string next to a `str` column.
- A row that would be empty when written is written as `""`, as
  Python's `csv` module writes it.
- `describe` skips non-numeric columns; `rank` and `shift` of an int
  column are float.

## Where it differs from pandas, on purpose

- **No index.** Results are written with `index=False`, so every
  operation that would build one (`groupby`, `pivot_table`,
  `value_counts`) has it reset into columns; only `describe` and
  `quantile` keep theirs, since their index is the statistic's name.
- **A job language instead of Python.** Steps take positional
  arguments and `key=value` options; there are no expressions beyond one
  binary operator in `assign` and `where`.
- **Errors print the exception's type and message** for the ones the
  jobs exercise (a missing file, a missing column, an unknown
  aggregate), not a traceback.

## The Mend loop

The first `karac check` reported 18 errors: six assignments through a
`mut ref` parameter that needed `*` (the rolling-window helpers), four
`*` dereferences of `Copy` values that borrow-by-default made unneeded,
a non-`Copy` column moved out of an index, and two match bindings that
moved their payload. `karac fix` applied those 13. The other five were
two calls of `Ordering.reverse`, which the library does not have (a
`flip` helper replaces it), and three errors from one compiler gap
(below). After `check` passed, the MIR build refused one function on a
second gap. The first interpreter run then matched pandas on every job
but the inner merge above, which led to porting pandas' join.
`mini_pandas.orig.kara` is the program as first written.

## Gaps found

- **B-2026-10-28-28** (open): a closure that calls a method on its
  parameter and passes a captured `String` to `contains`
  (`|s| s.to_uppercase().contains(upper)`) is typed as consuming the
  capture, so it is a once-callable closure and `map(...).collect()`
  refuses it; `|s| s.contains(upper)` passes. `contains_rows` passes
  `upper_pat.as_str()` instead.
- **B-2026-10-28-29** (open): `Ok((out, qs.iter().map(|q|
  q.to_string()).collect()))` passes `karac check`, but the MIR build
  refuses it ("an expression whose type does not lower
  (UnsolvedParam("C"))"). A `collect` that is a tuple element inside
  `Ok` or `Some` fails; in a bare tuple, or directly inside `Ok`, it
  builds. `quantile` binds the labels first.

## What a dataframe API would need (notes for M4c)

Compared with `docs/library/data.md` in the kara repo:

- **pandas' digits.** A port of a pandas or Polars report is checked
  against that library's output, and the last digit depends on the
  summation: Kahan for groupby sums and means, pairwise for a column's
  mean, Welford for a group's variance. `data.md` says what `mean` and
  `var` compute, not how; naming the algorithm (and offering a
  compensated `sum`) is what lets a port match.
- **Window expressions.** `rolling` mean, sum, min, max and count with
  `min_periods`, `shift`, `diff`, `pct_change`, `cumsum`, `cummax`, and
  the group-wise `cumsum`, `shift` and `diff`. `data.md` has none of
  these; Polars' `rolling_mean`, `shift`, `diff` and `cum_sum` with
  `.over(keys)` are the shape.
- **Group-relative columns.** `transform` broadcasts a group's aggregate
  back onto each of its rows (`sum().over(keys)`); `agg` alone cannot
  write `salary - max(salary) per dept`.
- **Joins.** `how="right"` and `how="outer"` and `left_on`/`right_on`
  (Decision Q10) are five of the twelve merge jobs, including the
  self-join of staff to their managers. Rows in left order with each
  left row's matches in right order, as `data.md` promises, is cleaner
  than pandas' shortcut, and a port of a pandas job then differs from
  it in row order, which is worth a sentence in the page.
- **Reshaping.** `pivot_table`, `melt` and `crosstab` have no
  counterpart; nor does `concat` of frames whose columns differ
  (`vstack` needs equal schemas), the common case of appending a newer
  export with an added column.
- **Ranks, counts, duplicates.** `rank` with its five tie methods,
  `value_counts` with `normalize`, and `drop_duplicates` with
  `keep="last"` and `keep=False` (`unique` keeps only the first).
- **Summaries.** `describe` here is pandas' (no `null_count` row, the
  sample `std`); `data.md`'s adds `null_count`, which is better, but its
  `std` must stay the sample one, and `Stats.stddev` is the population
  one, so a port that reaches for `Stats` gets a different number.
  `quantile` over several `q` at once returns a frame indexed by `q`.
- **NaN and null.** pandas uses `NaN` for a missing float and for a
  missing `str`; `data.md` keeps null and NaN apart, which is right, but
  `read_csv` then needs `na_values` (several spellings, here `-`),
  `keep_default_na` and the default list (`NA`, `N/A`, `null`, `nan`,
  ...) to read the same files, and a per-column `dtype` override (the
  "schema override next" of Decision Q13).
- **Strings.** `contains` with `case=False`, `startswith`, `strip`,
  `upper`, `lower` and `len` on a string column are the filters every
  log-cleaning job uses; they are `map` calls today.
- **Sorting.** `sort_by` with a per-key direction and `na_position`.
  The Kāra side also wanted `Ordering.reverse()`, which the core
  library does not have.
