# mini_pandas_json

A Kāra port of the part of [pandas](https://pandas.pydata.org/) 3.0.5
(BSD-3-Clause) that turns an API's JSON into a table and a table's text
into categories: `json_normalize` with its record paths, meta fields,
separators, depth limits and prefixes, `explode`, `str.split`,
`get_dummies`, `cut` and `qcut`, categoricals, `nlargest`, `interpolate`,
`cumcount` and `ngroup`, with the groupby, `value_counts`, sorting and
`to_csv` a report on them needs. Like [mini_pandas](../mini_pandas/README.md)
it runs pipelines in a small job language, one step per ` | `:

```
json orders.json record_path=items meta=id,customer.name | explode tags | counts tags
jsonl events.jsonl | where event == 'purchase' | select user.id,props.amount,props.items
read survey.csv | cut age 0,18,35,50,120 band | agg band income:mean,respondent:count
```

| Group | Steps (the pandas call each one is) |
|---|---|
| Reading | `json` (`json.load`, then `json_normalize` with `record_path`, `meta`, `sep`, `max_level`, `record_prefix`, `meta_prefix`, `errors="ignore"`), `jsonl` (`json.loads` per line, then the same), `read` (`read_csv`) |
| Lists in cells | `len` (`str.len` of a list), `explode`, `split` (`str.split`), `splitx` (`str.split(n=..., expand=True)`) |
| Indicators | `dummies` (`str.get_dummies(sep)`, joined on), `getdummies` (`pd.get_dummies(df, columns=..., prefix=...)`) |
| Binning | `cut` (edges or a number of bins, `right`, `labels`, `include_lowest`), `qcut` (a number of quantiles or the quantiles, `labels`) |
| Categoricals | `cat` (`astype(CategoricalDtype(cats, ordered))`), and `sort`, `where`, `counts`, `agg` and `dedup` on the result |
| Rows | `nlargest`, `nsmallest`, `interpolate`, `cumcount`, `ngroup`, `replace` |
| Basics | `dtypes`, `head`, `tail`, `select`, `drop`, `where`, `round`, `sort`, `dedup`, `agg`, `counts` |

The column types are pandas': `int64`, `float64`, `bool`, `str`,
`category`, and `object` for a column of Python objects (lists, dicts,
mixed values), whose cells to_csv writes as Python's `repr` writes them.

## Running it

The program carries its inputs: it writes `data/` (an online shop's order
export as its API returns it, with a nested customer and address, line
items with optional options objects, a usually-null coupon, a gift flag
only some orders carry, returns on two orders, one customer without an
address, one order with no items and one with `null` for them; the same
shop's single order; webhook events as JSON lines, one with a `null` user;
a developer survey with multi-select answers joined by `;`, an answer off
the scale and ages on the bin edges; a sensor with dropped readings) and
runs `data/jobs.txt`, 91 jobs. Each job prints `$ <job>`, then the result
as `to_csv(index=False)` writes it, or `! ` and the exception pandas raised.

```
python3 -s -P -B mk.py          # writes data/ and data/jobs.txt
python3 -I -B gen_kara.py       # copies data/ into the end of mini_pandas_json.kara
python3 -I -B oracle.py > expected.out
karac run mini_pandas_json.kara | diff - expected.out
```

`oracle.py` is the oracle, and it is pandas itself, with mini_pandas'
requirements (pandas 3.0.5 on numpy, without bottleneck or pyarrow).

## Parallel work

Every job runs in its own `TaskGroup` task, and inside a job each `agg`
aggregate is computed in a task of its own; the results join in order, so
the transcript is the same on every run and thread count. The program
matches `expected.out` on `karac run`, `__mir-llvm-run`, `__mir-llvm-asan`
(no reports) and a `karac build` binary, at `KARAC_THREADS=1` and `4`, and
five runs of an auto-par build at `KARAC_THREADS=4`.

## pandas' own rules, kept

- **Column order.** `json_normalize` keeps each object's plain fields in
  place and moves a nested object's fields to the end, so `returns` comes
  before `customer.id` in the single order, and after it in the list,
  where it first appears in the fifth order. Across objects the columns
  are in order of first appearance.
- **Dtypes from objects.** Ints alone are `int64`; ints with a missing key
  are `float64` (every `user.id` in the events, because one user is
  `null`); bools with a missing key are `object` (`gift`); strings with
  missing values are `str`; a column that is `null` in one event and
  missing in the rest is `float64`. Meta columns are built from an object
  array, so `id` stays `object` while `status` becomes `str`, and an
  exploded list of strings becomes `str`.
- **Meta paths follow the record path.** With `record_path=items,tags` a
  meta path of length two is read one level down, from the item, not the
  order. A record path through a `null` raises Python's `TypeError:
  'NoneType' object is not iterable`, and a record that is a string, not
  an object, makes a column named `0` (`tag_0` with a prefix).
- **Errors.** A record path missing from one object, a meta field missing
  without `errors="ignore"`, and a meta name that collides with a record
  column (pandas' message ends in a space) each print pandas' exception.
- **Interval labels.** `cut` and `qcut` round each edge to the fewest
  digits, from three, that keep the edges apart (`_round_frac`,
  `_infer_precision`), move the first edge down by `10**-precision` when
  the lowest value is included, which turns integer edges into floats
  (`(-0.001, 18.0]`), and widen an equal-width cut by 0.1% of the range on
  its open side (`71.053`). `qcut` with an integer `q` nudges each
  quantile that `q * quantile` does not reproduce up to the next float,
  as pandas does, before taking numpy's linear quantile.
- **Categoricals.** Comparing an unordered categorical with `<` raises;
  sorting and grouping follow the category order, not the text;
  `value_counts` lists every category, zeros included, ties in category
  order; `groupby` keeps only the observed categories (pandas 3's
  `observed=True`).
- **`interpolate`** is `np.interp` over row positions with
  `limit_direction="forward"`: readings before the first one stay
  missing, and readings after the last one take its value.
- `nlargest` keeps ties in row order, and `ngroup` numbers groups in key
  order.

## Where it differs from pandas, on purpose

- **No index.** Results are written with `index=False`; `explode`
  repeats the row, not an index label.
- **A job language instead of Python.** Meta and record paths are written
  with dots (`customer.address.city`), and `_` stands for a space in a
  category name.

## The Mend loop

The first `karac check` stopped at a parse error: `if is_nan(x) { na =
true } else { floats = true }` in a match arm, the Rust way of writing it,
needs a `;` after each assignment. I added the two `;` by hand; on the
original source `karac fix` adds them, one per run. Probing the same `if`
as a statement found that there `karac fix` refuses the fix (row B-2026-10-28-31).
The next check reported 15 errors: `karac fix` applied 13 (eight `*`
dereferences of `Copy` values, a `+` with a `Str` right operand, two moves
out of a bare `for` element, a match binding that moved its payload, and
a value moved inside a loop). The other two were edited by hand: a type
annotation the check asked for on a `collect` inside a `map` closure, and
`prefix.clone() ?? ...` for a gap (B-2026-10-28-32). The first interpreter run then
matched pandas on every job. `mini_pandas_json.orig.kara` is the program
as first written.

## Gaps found

- **B-2026-10-28-31** (fixed in kara d9df626fe4): `if c { a = x } else { b = y }` as a statement gets a
  correct `;` fix-it for the first arm only, and `karac fix` refuses to
  apply it, because the parser does not recover as if the `;` were there
  and the fixed file reports more errors than the original.
- **B-2026-10-28-32** (fixed in kara 03e92379b1): `prefix ?? name.to_string()` on a bare (borrowed)
  `Option[String]` parameter is refused as "'??' requires an `Option` or a
  `Result` on the left, found 'ref Option[String]'", with the help "drop
  the '?? ...'", which is wrong; on an `Option[i64]` it checks. The app
  writes `prefix.clone() ?? ...`.

## What nested data would need (notes for M4c)

Compared with `docs/library/data.md` in the kara repo:

- **List and struct columns.** Every job here starts from nested JSON,
  and `data.md`'s element types are Arrow primitives and `String` only (a
  list or struct field is `Err(Unsupported)`). `explode`, `str.split`
  into lists, `len` of a list and `json_normalize`'s flattening are
  Polars' `explode`, `str.split`, `list.len` and `unnest` on Arrow List
  and Struct columns, which is the shape to aim for.
- **Reading JSON.** `read_ndjson` / `read_json` with a schema inferred
  from the values, and a flattening step with a separator and a depth
  limit. `std.json` already keeps an integer apart from a float
  (`Json.Int` and `Json.Number`), which is what lets the inference here
  match Python's.
- **What a mixed column becomes.** pandas falls back to `object`, a
  column of Python objects, for bools with gaps and for a column that
  mixes numbers and strings. A typed library needs a rule instead (a
  supertype, nullable bools, or an error naming the column), and the page
  should state it, since a port of a pandas job meets these columns at
  once.
- **Categoricals.** An ordered categorical (Arrow's dictionary encoding,
  which `data.md` reads as `Err(Unsupported)`) with comparisons, sorting
  and grouping in category order, and `value_counts` that lists unseen
  categories. `cut` and `qcut` return one, with pandas' interval labels;
  Polars' `cut` and `qcut` are the same idea.
- **Indicators and binning.** `get_dummies` (Polars' `to_dummies`) and
  `str.get_dummies` for multi-select answers, the most common way survey
  and tag data arrives.
- **Gaps and ranks.** `interpolate`, `nlargest` / `nsmallest` (Polars'
  `top_k`), `cumcount` and `ngroup` have no counterpart yet.
