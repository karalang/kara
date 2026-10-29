# mini_pandas_clean

A Kāra port of the part of [pandas](https://pandas.pydata.org/) 3.0.5
(BSD-3-Clause) that cleans an export and writes it back out: the
`Series.str` methods a contact list needs, `to_numeric`, `duplicated`,
`mask` and `where`, `combine_first` and `update`, `merge_asof`,
exponentially weighted windows, and `to_json` in all six orients, with
the `read_csv`, groupby and `to_csv` they run between. Like
[mini_pandas](../mini_pandas/README.md) it runs pipelines in a small job
language, one step per ` | `:

```
read contacts.csv dtype=zip:str | str email strip | str email lower | duplicated subset=email as=dup
read trades.csv | asof quotes.csv on=ms by=ticker tolerance=10 | json records
read closes.csv | ewm close mean span=3 by=ticker as=ema | select day,ticker,close,ema
```

| Group | Steps (the pandas call each one is) |
|---|---|
| Reading | `read` (`read_csv` with `dtype`), `dtypes`, `select`, `head`, `tail`, `where` |
| Text | `str COL METHOD` for `lower`, `upper`, `strip`, `title`, `capitalize`, `swapcase`, `len`, `zfill`, `pad` (`side`, `fillchar`), `slice`, `replace` (`regex=False`), `removeprefix`, `removesuffix`, `find`, `count`; `cat` (`str.cat` with `sep` and `na_rep`) |
| Numbers from text | `numeric` (`pd.to_numeric`, `errors="raise"` or `"coerce"`) |
| Flags and conditions | `duplicated` (`subset`, `keep` first, last or `False`), `mask`, `keep` (`Series.where`), with or without `other` |
| Two sources | `combine` (`set_index(on).combine_first(...)`), `update` (`set_index(on).update(...)`) |
| As-of joins | `asof` (`merge_asof` with `on`, `by`, `direction` backward, forward or nearest, `tolerance`, `allow_exact_matches`) |
| Windows | `ewm` (`mean`, `std`, `var`; `com`, `span`, `halflife` or `alpha`; `adjust`, `ignore_na`, `min_periods`, `bias`; per group through `groupby(...).transform`) |
| Grouping | `agg` (named aggregation, from mini_pandas) |
| Writing | `json` (`to_json` with `orient` records, split, index, columns, values or table, `double_precision`, `force_ascii`, `lines`), or `to_csv(index=False)` when a job has no `json` step |

In a text argument `\s` stands for a space and `\e` for the empty string,
since the job language splits on spaces.

## Running it

The program carries its inputs: it writes `data/` and runs
`data/jobs.txt`, 108 jobs. The data is a CRM's contact export (names
typed every way, emails with stray spaces and a `mailto:` prefix, phone
numbers in four formats, zip codes a spreadsheet stripped of their
leading zeros, a hand-typed signup fee with `N/A`, `1,200`, `free`, an
exponent and a blank, and the same person entered twice), a cleaner
copy of the fees, a shop's catalog and a vendor's price feed for it, a
morning of trades and quotes on two tickers, daily closes with days the
feed missed, and measurements at both ends of the float range, as
numbers and as text with more digits than a double holds. Each job
prints `$ <job>`, then the result as `to_csv(index=False)` or `to_json`
writes it, or `! ` and the exception pandas raised.

```
python3 -s -P -B mk.py          # writes data/ and data/jobs.txt
python3 -I -B gen_kara.py       # copies data/ into the end of mini_pandas_clean.kara
python3 -I -B oracle.py > expected.out
karac run mini_pandas_clean.kara | diff - expected.out
```

`oracle.py` is the oracle, and it is pandas itself (pandas 3.0.5 on
numpy, without bottleneck or pyarrow, as for mini_pandas).

## Parallel work

Every job runs in its own `TaskGroup` task; a per-group `ewm` computes
each group in a task of its own, and each `agg` aggregate is a task too.
The results join in order, so the transcript is the same on every run
and thread count. The program matches `expected.out` on `karac run`,
`__mir-llvm-run`, `__mir-llvm-asan` (no reports) and a `karac build`
binary, at `KARAC_THREADS=1` and `4`, and on five runs of an auto-par
build at `KARAC_THREADS=4`.

## pandas' own kernels, ported

- **precise_xstrtod**, the C parser `read_csv` and `to_numeric` share
  (`tokenizer.c`). It keeps 17 significant digits, counting leading
  zeros, turns the rest into a power of ten, and scales once by a power
  of ten from a table of C literals. So it is not correctly rounded:
  `31566.726206662698e10` reads as `315667262066626.94` where Python's
  `float()` gives `315667262066627.0`, `9223372036854775807` next to a
  float reads as `9.223372036854778e+18`, and
  `0.00000000000000000001` reads as `0.0`. 6086 of 200,000 random long
  decimals differ from `float()` this way; the `sci.csv` jobs show the
  cases.
- **maybe_convert_numeric**, `to_numeric`'s loop: a column of whole
  numbers stays `int64`, any float, gap or unparsed text under `coerce`
  makes it `float64`, `inf` and `-Infinity` are accepted in any case,
  `nan` is not (it raises), and the error names the position.
- **The ewm and ewmcov loops** (`aggregations.pyx`), including the
  constant-series guard, `ignore_na`, the bias correction, and the line
  that resets the new weight when `adjust=False` and `com == 1`.
- **merge_asof's three join loops** (`join.pyx`), with the per-group
  table the backward and forward walks keep, `nearest` preferring the
  earlier quote on a tie, and pandas' checks: sorted keys, matching
  dtypes, a non-negative tolerance.
- **ujson's double encoder** (`ultrajsonenc.c`, modp_dtoa's fixed-point
  writer): `double_precision` decimals with its own rounding, trailing
  zeros dropped, `-0.0` written as `0.0` but `-0.125` at precision 0 as
  `-0`, and `printf("%.10g")` past `1e16 - 1` and below `1e-15`. In a
  double `1e16 - 1` is `1e16`, so `1e16` itself is written out in full
  as `10000000000000000.0`, while `1.5e17` becomes `1.5e+17`. The `%g`
  here is computed from the double's exact decimal value, since Kāra's
  format specifiers have no exponent type. Strings escape `/` as `\/`
  and, with `force_ascii`, everything past ASCII as `\uXXXX`.

Python's own rules come along where pandas calls Python: `str.title`
starts a word after any uncased character (`3Rd Street Deli`,
`O'Neil`), `str.center` splits an odd margin by CPython's formula, and
`str.zfill` keeps a sign in front.

## Where it differs from pandas, on purpose

- **No index.** Results are written with `index=False`, and `to_json`
  is called after `reset_index(drop=True)`, so the index it writes is
  0, 1, 2 and so on.
- **`str.count` takes a plain substring.** pandas compiles the pattern
  as a regular expression; every job counts a single letter, where the
  two agree.

## The Mend loop

The first `karac check` stopped at four parse errors, all Rust habits:
two closures that declared their return type, which `karac fix`
removed, and two parameters typed `impl Fn(Str) -> String`, which got a
bare parse error and no fix (B-2026-10-29-1), so I wrote Kāra's
`Fn(Str) -> String` by hand. The next check found a helper I had not
copied over from mini_pandas, which I added. Then there were 24 errors:
`karac fix` applied 18 (seven `.copied()` on an `Option` to `.cloned()`,
five `+` with a `Str` right operand, four `mut` markers on arguments, and
two `.clone()`s on the arguments of a `let`-bound closure,
B-2026-10-28-24, fixed in kara da37ab3afe; the clones stay because a
`Str` parameter still moves a `String` argument, B-2026-10-29-49). Three were edited by hand: a closure passing a captured
`String` to `replace` was refused as once-callable (B-2026-10-29-2), so
the app passes `old.as_str()`, and two `.bytes().collect()` calls got
the `.iter()` the message asked for, which `karac fix` did not apply
(B-2026-10-29-3). The first interpreter run then matched pandas on every
job. `mini_pandas_clean.orig.kara` is the program as first written.

## Gaps found

- **B-2026-10-29-1** (fixed in kara 38876d470b): a parameter typed `impl Fn(i64) -> i64` or
  `fn(i64) -> i64`, Rust's two spellings, gets only `Expected
  RightParen, found LeftParen` (or `Expected type, found Fn`), with no
  hint that Kāra writes `Fn(i64) -> i64` and no fix-it.
- **B-2026-10-29-49** (Thread A, open; a remainder of B-2026-10-28-24):
  a `let` closure whose parameter is typed `Str` moves a `String`
  argument, so `skip(n)` then `right.find(n)` is refused; `|n: String|`
  and a `fn` with a `Str` parameter check. `merge_asof` calls
  `skip(n.clone())` until it is fixed.
- **B-2026-10-29-2** (fixed in 80e76392cf): `|s| s.replace(old, new)`
  with `old` a captured `String` was typed as consuming `old`, so
  `.map(...)` and any `Fn` slot refused it. `replacen`, `split`, `find`
  and `strip_prefix` did the same; `starts_with`, `contains` and
  `ends_with` passed. All of them borrow their argument, and the app
  passes `old` itself again.
- **B-2026-10-29-3** (open): `s.bytes().collect()` is refused with a
  message that spells out `s.bytes().iter().collect(...)`, but the
  diagnostic carries no fix, so `karac fix` leaves it.

## What cleaning and export would need (notes for M4c)

Compared with `docs/library/data.md` in the kara repo:

- **A string namespace.** Every cleaning job here is a `str` method over
  a column with gaps: case, whitespace, padding, slicing, prefixes,
  finding and counting, joining two columns with a stand-in for a gap.
  Polars' `str` namespace is the shape; Python's casing rules (`title`
  after an apostrophe or a digit) are a choice the page should make
  rather than inherit.
- **Parsing numbers out of text.** `to_numeric` with `raise` and
  `coerce`, the dtype it picks (int unless something forces float), and
  which spellings it takes (`inf`, a leading `+`, spaces around the
  number; not `1,200`, `1_000` or `nan`). The page should also say
  whether parsing is correctly rounded. pandas' is not past 17 digits,
  and a Kāra library that is will differ from pandas in the last digit
  on such input, which is the better behaviour but worth stating.
- **As-of joins and exponential windows.** `join_asof` with `by`,
  `strategy` and `tolerance`, and `ewm_mean` / `ewm_std` / `ewm_var`
  with `adjust`, `ignore_nulls` and `min_samples` are what a market-data
  or sensor pipeline reaches for first. Both are named in Polars and
  missing from `data.md`.
- **Writing JSON.** A writer for records, JSON lines and a split or
  table layout, with its float format stated. pandas' (ujson) is
  neither shortest round-trip nor `%g`. `std.json`'s writer would be the
  natural base, and a float formatter with an exponent form would help
  any writer: Kāra's format specifiers have only `x`, `X`, `o`, `b` and
  `d` (design.md §3), so this app computes `%g` itself.
- **Filling one table from another.** `combine_first` and `update`
  keyed on a column (Polars' `update` and a coalescing join), with the
  dtype rule when a key exists on only one side: an int column that
  gains a gap becomes float in pandas, and a typed library needs its
  own answer.
