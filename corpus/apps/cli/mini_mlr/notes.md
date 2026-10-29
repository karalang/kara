# mini_mlr

A Kāra port of [Miller](https://github.com/johnkerl/miller) 6.13 by John
Kerl (BSD-2-Clause), the command line tool for name-indexed data: CSV,
TSV, DKVP (`key=value` lines), NIDX (whitespace-split lines), JSON,
pprint, xtab and markdown. Miller treats data as a stream of records
whose fields can differ from one record to the next, and 33 of its
verbs are ported from its Go source (`pkg/input`, `pkg/output`,
`pkg/mlrval`, `pkg/bifs`, `pkg/transformers`, `pkg/climain`), together
with its value model: each field keeps the text it was read with, and
its type (int, float, string, empty) is inferred from that text the way
Miller's scanner does it.

| Group | Verbs |
|---|---|
| Records as they come | `cat` (`-n`, `-N`, `-g`, `--filename`, `--filenum`), `head`, `tail` (`-n`, `-g`), `tac`, `grep` (`-v`, `-i`, `-a`), `decimate` (`-n`, `-b`, `-e`, `-g`) |
| Fields | `cut` (`-f`, `-o`, `-x`, `-r` with `"..."i`), `having-fields` (every test), `template`, `unsparsify` (`--fill-with`, `-f`), `regularize`, `sort-within-records` (`-r`), `reorder` (`-f`, `-e`, `-r`, `-a`, `-b`), `rename` (`-r`, `-g`, capture groups), `label` |
| Missing values | `fill-down` (`-f`, `-a`, `--all`, `--only-if-absent`), `fill-empty` (`-v`, `-S`) |
| Sorting and grouping | `sort` (`-f`, `-r`, `-c`, `-cr`, `-nf`, `-nr`, mixed keys), `top` (`-n`, `-f`, `-g`, `--min`, `-a`, `-o`), `group-by`, `group-like`, `count`, `count-distinct` (`-n`, `-u`), `count-similar`, `uniq` (`-g`, `-c`, `-n`, `-a`) |
| Statistics | `stats1` (count, mode, antimode, sum, mean, var, stddev, meaneb, kurtosis, min, max, minlen, maxlen, null_count, distinct_count, mad, percentiles and `median`, `-i` interpolation, `--fr`, `--fx`), `merge-fields` (`-a`, `-f`, `-r`, `-c`, `-k`, `-o`), `fraction` (`-p`, `-c`, `-g`), `step` (delta, shift, shift_lag, shift_lead, from-first, ratio, rsum, rprod, counter, ewma with `-d` and `-o`, `slwin_b_f`) |
| Reshaping | `nest` (explode and implode, values and pairs, across records and fields, `--evar`, `--ivar`), `join` (`-j`, `-l`, `-r`, `--lp`, `--rp`, `--np`, `--ul`, `--ur`, unsorted) |
| Generators and time | `seqgen` (`-f`, `--start`, `--stop`, `--step`), `sec2gmt` (`-1` to `-9`, `--millis`, `--micros`, `--nanos`) |

The main flags cover the input and output formats and their shorthands
(`--icsv`, `--c2p`, `--c2j`, ...), `--ifs`, `--ofs`, `--fs`, `--ops`,
`--repifs` and the separator names (`semicolon`, `tab`, `colon`, ...),
`--implicit-csv-header`, `--headerless-csv-output`,
`--allow-ragged-csv-input`, `--quote-all`, the JSON flags
(`--jvstack`, `--jlistwrap`, `--ojsonl`, `--no-auto-unflatten`),
`--xvright`, `--barred`, `--right`, `--from` and `-n`, and verb chains
joined by `then`.

## Running it

The program carries its inputs: it writes `data/` (an order export, a
customer list, sensor readings in every numeric spelling Miller's
scanner has an opinion on, quoted CSV with a byte-order mark and CRLF,
duplicate header names, a ragged file, TSV with escapes and a short
row, DKVP events with missing keys, whitespace-split words, flattened
JSON keys, tag lists for `nest`, epoch times, an empty file and a
header-only file) and runs `data/jobs.txt`, 192 mlr command lines. Each
job prints `$ mlr ...`, then what the command wrote, then each line it
wrote to standard error after `! `, then `[exit 1]` when it failed.
Output that ends without a newline is followed by
`\ no newline at end of output`, and a carriage return is shown as `^M`,
as `cat -v` shows it.

```
python3 -s -P -B mk.py          # writes data/ and data/jobs.txt
python3 -I -B gen_kara.py       # copies data/ into the end of mini_mlr.kara
python3 -I -B oracle.py > expected.out
karac run mini_mlr.kara | diff - expected.out
```

`oracle.py` is the oracle: a Python mirror of the same verbs written
from Miller's Go source, including Go's float formatting (`'f'`, shortest
digits, no exponent), Miller's number grammar (`0x1F` is an int, `007`,
`1_000` and a decimal past the int64 range are strings, and `+3`, `.5`,
`5.` and `1e3` are numbers that print back as written) and its
arithmetic (an int `+` or `*` that overflows becomes a float, and `/`
keeps an int when the division is exact).

## Parallel work

Every job runs in its own `TaskGroup` task, and inside a job every
input file is read in its own task; the reads join in file order, and
the jobs print in job order, so the transcript is the same on every run
and every thread count. The program matches `expected.out` on
`karac run`, `__mir-llvm-run`, `__mir-llvm-asan` (no reports) and a
`karac build` binary, at `KARAC_THREADS=1` and `4`, with auto-par on
for the build.

## Where it differs from Miller, on purpose

- **No DSL.** `put`, `filter` and the other verbs that take Miller's
  expression language are not ported; neither is JSON input.
- **One stage at a time.** Miller runs the reader, the verb chain and
  the writer as goroutines over batches of 500 records. Here each job
  reads all its input, then runs each verb over all the records, then
  writes. What is printed is the same, including the cases where an
  error stops a reader part way.
- **`skewness`** is accepted but no job asks for it: Miller computes it
  with `math.Pow(x, 1.5)`, whose last digit the Python mirror cannot
  promise to match.
- **Arguments** are parsed by a small table of each verb's flags. Usage
  text is not reproduced; a bad verb flag prints Miller's one-line
  message.

## Miller's own quirks, kept

- NIDX splits on runs of spaces and tabs only when neither `--ifs` nor
  `--repifs` is given; `--ifs ' '` alone splits on every space.
- `fill-down -f` fills a field that is absent or empty; `-a` fills only
  an absent one.
- `nest --explode --values` splits an empty value into one empty piece,
  but `--pairs` splits it into none, so the record (across records) or
  the field (across fields) disappears.
- `merge-fields -c` removes the void fields it read even when they
  contributed nothing, unless `-k` is given.
- `decimate -b` emits the first record of each bunch as soon as it
  arrives.
- Non-interpolated percentiles take the element at `int(p * n / 100)`,
  so `p50` of four values is the third.
- A reader error drops the rest of its 500-record batch, but the
  records before it and the other files are still written; the error
  prints last, and when several files fail the last error wins.

## The Mend loop

The first `karac check` reported 47 errors: 29 `*` dereferences of
`Copy` values that the borrow-by-default rules made unneeded, bare
parameters given away, non-`Copy` elements moved out of an index, `+`
on a `Str`, and values moved inside loops. `karac fix` applied 43
fixes. The other errors came from two compiler gaps (below), each
worked around by hand in one place, marked in the source with its
ledger id. The first interpreter run then stopped on an index out of
bounds in the program's own sliding window (`step` asked a window with
nothing behind it for the previous record); after that one fix every
backend matched the oracle. `mini_mlr.orig.kara` is the program as
first written.

## Gaps found

- **B-2026-10-28-24** (fixed in kara da37ab3afe): calling a closure
  bound by `let` moved its non-`Copy` arguments, though core-semantics
  §5.10 makes a closure's parameters ordinary bare (borrowed)
  parameters. `karac check` refused `eq(v, w[0])` on a borrowed `v`,
  `__mir-run` refused it with a type mismatch, and the fixes `karac fix`
  offered (an `own` parameter and a `.clone()` on every argument)
  cascaded into the callers. `top`'s binary search compared with named
  functions until the fix and is back to its `gt` and `lt` closures.
- **B-2026-10-28-25** (fixed in kara af329e798c and 0d1ad90014):
  `let r = match rx { Some(ref r) => r, None => ... }`, the form
  `karac fix` writes for an `Option` matched in a loop, was refused by
  `karac check` and by the MIR build when the payload type has no
  `.clone()` (here a `Regex`). `having-fields` binds the regex that way
  again.

## What a dataframe API would need (notes for M4c)

Compared with `docs/library/data.md` in the kara repo:

- **Records that differ.** Miller's records have their own fields
  (DKVP with missing keys, `unsparsify`, `regularize`, `group-like`,
  ragged CSV). A `DataFrame` has one schema, so loading such data needs
  a union of the keys with nulls for the gaps, which `read_csv` does not
  offer, or a row-oriented record type beside the frame.
- **Values that keep their text.** `0x1F`, `007` and `1.50` print back as
  read until arithmetic touches them. A column inferred as `i64` or `f64`
  loses the spelling, so an ETL job that must not rewrite untouched
  cells needs `Infer.None` for those columns or a typed column that
  remembers its source text.
- **Window expressions.** `step` is shift, lag and lead, a difference, a
  ratio, a running sum and product, a counter, an exponentially weighted
  mean for several alphas, and a sliding-window mean, each per group.
  `data.md` has none of these; Polars' `shift`, `diff`, `cum_sum`,
  `rolling_mean` and `ewm_mean` with `.over(keys)` are the shape.
- **More aggregates.** `mode`, `antimode` (first seen wins a tie),
  `null_count`, `minlen`, `maxlen`, `meaneb`, `kurtosis` and `mad` are
  not `Expr` aggregates; `count`, `n_unique` and `quantile` cover the
  rest. Miller's percentiles are not interpolated unless `-i` is given,
  so `quantile` needs an interpolation option (`lower`, `linear`).
- **Row-wise and group-relative columns.** `merge-fields` is a sum or a
  mean across columns of one row (`sum_horizontal`); `fraction` and
  `count-similar` divide by or broadcast a per-group total back onto
  each row (an aggregate `.over(keys)`); `top` keeps the first `n` rows
  per group.
- **Forward fill and splitting.** `fill-down` is a forward fill of nulls
  (`fill_null` with a strategy), and `nest` splits one cell into rows or
  columns and joins it back (`str.split` then `explode`, and its
  inverse).
- **Joins.** `--ul` and `--ur` keep unpaired rows on either side, and
  `-l`/`-r` join on differently named keys: the outer join and
  `left_on`/`right_on` that Decision Q10 leaves out.
- **Time and output.** `sec2gmt` wants a timestamp type (Decision Q1),
  and the writers here (pprint, xtab, markdown, JSON with nested keys
  rebuilt from `a.b` names) go beyond `to_csv` and `Display`.
