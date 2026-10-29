# mini_pandas_query

A Kāra port of [pandas](https://pandas.pydata.org/) 3.0.5's
`DataFrame.query` and `DataFrame.eval` (BSD-3-Clause): the string
expressions people type to filter a frame and to compute columns from
it, evaluated the way pandas does when numexpr is not installed (its
python engine). Like [mini_pandas](../mini_pandas/README.md) it runs
pipelines in a small job language, one step per ` | `:

```
read orders.csv | query units >= 8 and region == 'West'
read orders.csv | let regs=['West','North'] | query region in @regs and units == 9
read inventory.csv | query `on hand` < `reorder at`
read orders.csv | eval gross = units * unit_price ; saved = gross - revenue | select order_id,gross,saved | head 5
read pairs.csv | eval a // b
```

| Step | The pandas call |
|---|---|
| `read` | `read_csv`, with `index` for `index_col` |
| `let k=v ...` | Adds to the `local_dict` the later steps see as `@k`: a quoted string, a list in brackets, a bool or a number |
| `query <expr>` | `df.query(expr, local_dict=..., global_dict={})` |
| `eval <expr>` | `df.eval(expr, ...)`, where ` ; ` separates the lines of a multi-line expression |
| `select`, `head`, `tail`, `sort`, `setidx` | Column selection (`\s` is a space in a name), `head`, `tail`, `sort_values(kind="stable")`, `set_index` |
| `show`, `string` | `repr` under `option_context`, and `to_string`, as in [mini_pandas_display](../mini_pandas_display/README.md) |

## Running it

The program carries its inputs: it writes `data/` and runs
`data/jobs.txt`, 181 jobs over five files:

- 96 shop orders (region, channel, product, units, price, a discount
  that is often missing, revenue, a gift flag and a free-text note);
- a warehouse stock count whose column titles were typed by people
  (`on hand`, `unit cost (€)`), so queries quote them in backticks, with
  zero and negative counts and missing costs and bins;
- hourly readings from four weather stations, with frost, calm hours,
  dry hours and gaps, for the math functions;
- eight integer pairs with negative operands and zero divisors, and a
  float column beside them, for numpy's division rules;
- a staff list read with the name as its index.

The jobs cover comparisons and their chains (`3 < units <= 4`),
`and`/`or`/`not` and `&`/`|`/`~`, strings and membership (`==` with a
list, `in`, `not in`), Series methods (`mean`, `median`, `max`, `min`,
`sum`, `isna`, `notna`, `fillna`, `between`, `isin`, `round`, `abs`) and
`.str` methods (`startswith`, `endswith`, `contains`, `lower`, `upper`,
`len`), `@` locals, backticked names, the index (`index`, `ilevel_0`, a
named index or level), values and scalars from `eval`, integer and float
arithmetic, assignments and multi-line `eval`, the 22 math functions,
and the errors pandas raises for what it refuses.

Each job prints `$ <job>`, then what pandas prints for the result, or
`! ` and the exception pandas raised.

```
python3 -s -P -B mk.py          # writes data/ and data/jobs.txt
python3 -I -B gen_kara.py       # copies data/ into the end of mini_pandas_query.kara
python3 -I -B oracle.py > expected.out
karac run mini_pandas_query.kara | diff - expected.out
```

`oracle.py` is the oracle, and it is pandas itself (pandas 3.0.5 on
numpy 2.5.3, without numexpr, bottleneck or pyarrow). The frames, the
CSV reader and every line of printing come from mini_pandas_display.

## Parallel work

Every job runs in its own `TaskGroup` task and the results join in
order, so the transcript is the same on every run and thread count. The
program matches `expected.out` on `karac run`, `__mir-llvm-run`,
`__mir-llvm-asan` (no reports) and a `karac build` binary, at
`KARAC_THREADS=1` and `4`, and on five runs of an auto-par build at
`KARAC_THREADS=4`.

## pandas' own code, ported

- **The preparser** (`core/computation/parsing.py`, `expr.py`):
  `&` and `|` become `and` and `or` before Python parses the text, so
  `a > 0 & b > 0` means `(a > 0) and (b > 0)`, not Python's precedence;
  `@name` becomes a local; a backticked name becomes
  `clean_column_name`'s identifier (`unit cost (€)` is
  `BACKTICK_QUOTED_STRING_unit_cost__LPAR__UNICODE_u20ac_RPAR_`).
- **Python's grammar** for the subset an expression can hold:
  `x if c else y`, `or`, `and`, `not`, chained comparisons with `in`,
  `not in`, `is` and `is not`, `^`, `+ -`, `* / // %`, unary `+ - ~`,
  `**` (right-associative, binding tighter than a unary minus on its
  left), attributes, calls with keyword arguments, lists and tuples.
- **The visitor** (`BaseExprVisitor`): `==` and `!=` become `in` and
  `not in` when a side is a list or a string, with the string wrapped in
  a list; a chained comparison becomes an `&` of its pairs; `and` and
  `or` fold left; an `in`, or any op on a text term, is evaluated on the
  spot; the checks `_disallow_scalar_only_bool_ops` (`units > 5 and 2`)
  and `has_invalid_return_type` (`region + '/'`, with pandas' message
  naming `'str'` and `'<class 'str'>'`); a call either of a method found
  by attribute or of a name in `MATHOPS`, which takes no keywords; the
  nodes it refuses (`IfExp`, `Is`, and `^`, which fails with an
  `AttributeError` because the visitor has no `visit_BitXor`).
- **Scope** (`scope.py`, `DataFrame._get_cleaned_column_resolvers`,
  `_get_index_resolvers`): columns first, then the index as `index`, its
  levels by name or `ilevel_N`; `@name` only from the locals; the two
  `UndefinedVariableError` messages.
- **`pandas.eval`'s loop** (`eval.py`): each line parsed and run on its
  own, the target copied at the first assignment, the assigned name
  added to the resolvers for the lines after it, the multi-line rule,
  and `query`'s "cannot assign without a target object".
- **The arithmetic** a Series and a scalar get: numpy's int64 (wrapping,
  `//` and `%` taking the divisor's sign, `**` by squaring and refusing
  negative exponents), npy_divmod for floats (`-17 % 0.001` is
  `3.538836e-16`), pandas' own int `//` and `%` by zero (the column
  turns float, with `inf`, `-inf` and NaN), NaN comparing false except
  under `!=`, the result name (kept when both names agree), Python's
  ints and floats for constants (`2 ** 10`, `7 / 2`), and numpy scalars
  from a reduction, printed as `np.int64(427)`.

## The Mend loop

The first `karac check` stopped at a parse error, mine: `let and = op
== "&";`. Its fix renamed the binding to `r#and` and left the two uses
as `and` (B-2026-10-29-13), so I renamed it by hand. Two functions then
clashed with ones reused from mini_pandas_display (`tokenize`,
`py_float`), and I renamed mine. `karac fix` applied 10 edits over two
rounds (`own` on five parameters, four `.clone()`s and a `mut` at a call),
which left 7 errors and 2 warnings:

- `i64` has no `wrapping_abs` (the documented families are `add`, `sub`,
  `mul`, `div`, `rem`, `neg`, `shl`, `shr` and `pow`); the app writes
  `if x < 0 { x.wrapping_neg() } else { x }`;
- a `.str` method's closure passed the captured `String` to
  `starts_with`, which makes it once-callable (B-2026-10-28-28); the app
  passes `p.as_str()`;
- `op_ev(op, v, [a, b].to_vec())` with borrowed `a` and `b` is
  "expected 'ref Vec[Ev]', found 'Vec[ref Ev]'" with no help
  (B-2026-10-29-14); the app clones the elements;
- `Parser` and `Args` shadow prelude types; the app renames them.

The MIR build then refused `s.name ?? String.new()` on a match binding
of a borrowed parameter, which `karac check` had passed: the
B-2026-10-29-10 rule again, here outside a loop. With `.clone()` there
the first run matched pandas on all 1,803 lines.
`mini_pandas_query.orig.kara` is the program as first written.

## Gaps found

- **B-2026-10-29-13** (open): the fix for a keyword used as a binding
  name renames the binding and none of its uses.
- **B-2026-10-29-14** (open): a `Vec` built from borrowed parameters
  passed where a `Vec` is expected has no help or fix.
- **B-2026-10-28-28** (fixed in 80e76392cf; the app passes `p` itself
  again) and **B-2026-10-29-10** (open), each met once more.

## What query and eval would need (notes for M4c)

Compared with `docs/library/data.md` in the kara repo, whose `Expr` is
built with methods (`col("status").ge(500)`) and evaluated by `filter`,
`with_columns` and `agg`:

- **A text form.** pandas users type these expressions, and so do the
  users of the programs written with it: a CLI's filter flag, a query
  parameter on a service, a rule in a config file, a dashboard's search
  box. Every such program needs text to `Expr`. A documented
  `Expr.parse(text) -> Result[Expr, DataError]` with one small grammar
  (comparisons, `and`/`or`/`not`, `in` with a list, arithmetic, calls of
  the `Expr` methods), quoting for names with spaces, and errors that
  carry a span, would save each program its own parser and its own
  dialect. This port's parser and visitor are about 1,100 lines.
- **Membership and strings.** `is_in` is missing, though `x in [..]` and
  `x == [..]` are among the most common predicates in the jobs; so are
  `between` and the string predicates (`starts_with`, `ends_with`,
  `contains`, lower and upper case, length).
- **Math.** `round`, `abs`, `floor`, `ceil`, `sqrt`, `log`, `exp` and the
  trigonometric functions on a numeric `Expr` (the readings jobs turn a
  wind speed and direction into east and north components).
- **Sequential assignment.** In a multi-line `eval`, each line sees the
  columns the lines before it made (`gross = ..; saved = gross -
  revenue`). The page should say whether `with_columns` with several
  expressions evaluates each against the original frame or in order.
- **Scalars out.** `eval units.sum()` returns one number; the page has
  aggregates "over the whole frame" but no way to take a scalar out
  other than a one-row frame.
- **Differences to state.** pandas keeps a missing value under `!=`
  (`discount != 0.1` keeps the rows with no discount), where `Expr`'s
  nulls drop the row; pandas wraps int64 overflow
  (`a * 100000000000` is `-6101065173709551616`) where Kāra checks it;
  an int `//` or `%` by zero gives `inf` and NaN in pandas and panics
  under Decision Q5. Each is a fair choice, and a reader porting a
  pandas query needs to find it on the page.
- **No index.** `index`, a named index and its levels are predicates in
  pandas; `data.md`'s frames have none, so the equivalent is an ordinary
  column, which the page could suggest.
- **Names.** pandas cleans a backticked name for parsing and, on an
  assignment, keeps the cleaned name as the new column's
  (`BACKTICK_QUOTED_STRING_to_order`); a text form for `Expr` should
  keep the name as written.
