# mini_itertools

Python 3.13's `itertools` as lazy Kāra iterators, run by a job list and
by the kind of program that uses them: a sales report that groups,
pages, totals and pairs up rows.

The run writes a sales CSV into `data/`, then runs `data/jobs.tsv`, 59
jobs with tab-separated fields. Each job prints `== ` and its fields
joined by ` | `, then its output, the Python `repr` of the list the
iterator gives.

The jobs cover:
- the infinite sources `count` (negative and zero steps) and `cycle`
  (of an empty input), cut with `islice`, and `repeat` with a count;
- `accumulate` with `add`, `mul`, `max` and `min`, with and without
  `initial`, on empty input;
- `batched` (a short last batch, a batch larger than the input),
  `chain` (with empty inputs), `compress` (selectors longer and shorter
  than the data);
- `dropwhile`, `takewhile`, `filterfalse`;
- `groupby` by value and by parity, so equal keys that are not adjacent
  make separate groups;
- `islice` with a step, no stop, a stop before the start and a stop past
  the end; `pairwise`; `zip_longest` with a fill value;
- `product` (with `repeat`, an empty pool, no pools),
  `permutations` (r of none, 0, larger than the pool), `combinations`
  and `combinations_with_replacement`, including the `()` results;
- `report`: `data/sales.csv` grouped by region and by month with running
  totals and month-to-month changes, paged in fours, product bundles,
  SKUs, a revenue budget, round-robin ticket assignment, promo rows,
  north against south, numbered big orders and the rows from March on.

**The port.** Every adapter is a struct that implements `Iterator` and
pulls from its source only when asked, so they compose as Python's do:
`islice(cycle(...), n)`, `zip(count(1), filterfalse(...))`. `cycle`
saves what it has seen; `groupby` keeps the first item of the next run.
`permutations`, `combinations` and `combinations_with_replacement`
follow the index-tuple recipes in the itertools docs, so their output
comes in the same order; `product` is an odometer over the pools'
indices. `sorted` is a stable insertion sort.

Not supported:
- `tee`, `starmap` and `zip` of more than two iterators;
- `groupby` groups that are themselves lazy (each group is a `Vec`);
- `islice` with negative arguments, which Python refuses too.

**Oracle.** `expected.out` is Python 3.13.16's output for the same jobs,
made by `oracle.py` with the real `itertools`. Run `mk.py` first; it
writes `data/`. `gen_kara.py` prints the Kāra data section from the same
files.

## Edits from mini_itertools.orig.kara to mini_itertools.kara

Author errors:
- Ten uses of `!`, `&&` and `||`, which Kāra spells `not`, `and` and
  `or`. `karac fix` made all ten edits.
- `for l in lists(f[1])` moved each borrowed element into an iterator;
  it now loops over `.into_iter()`.

Compiler gaps:
- B-2026-10-18-20: `karac check` cannot work out `T` in
  `fn f[I: Iterator[Item = T], T](it: own I)` from the iterator passed
  in, so fourteen calls such as `to_vec(pairwise(iter_of(v)))` failed
  with "cannot infer type parameter 'T'" (or, in the report's `for`
  loops over `zip(...)` and `enumerate(...)`, with "no field 'month'"
  on the loop variable). Each result is now bound to a typed local
  (`let v: Vec[(i64, i64)] = ...`, or the full `Zip[...]` type once),
  and those loops go through such a `Vec`. The calls that also take a closure with typed parameters were
  fine.
- B-2026-10-18-24: on the MIR surfaces a bare `None` passed for
  `initial: Option[T]` is not given a type. Here the build stopped
  ("an expression whose type does not lower"); in a smaller program the
  compiled backends ran it and gave the wrong answer. Both report calls
  pass a typed `no_start: Option[i64]` instead.

The natural way to write these adapters, with `I.Item` instead of a
second type parameter (`struct Pairwise[I: Iterator] { prev:
Option[I.Item] }`), was tried first and dropped before the first draft
because of three more gaps: B-2026-10-18-22 (`karac check` does not
match `I.Item` against a concrete type, in a typed local or in a closure
field), B-2026-10-18-23 (MIR does not lower such a struct when a generic
function builds it) and B-2026-10-18-21 (an impl's type parameters
listed in a different order from the struct's are matched by position).
B-2026-10-05-18 is why the program does not call `.collect()` or
`.map()` on its own iterators: default `karac check` refuses them.

After those edits the program matched expected.out on both MIR
surfaces.

Written 2026-10-18 by the real-world apps thread; karac at 79a6c2eb9a.

## Library migration, 2026-10-18

Thread A's a0ca700228 and acd593fc77 changed the library to match docs/library: a number's `parse` returns `Result[T, ParseError]`, and `split`, `lines`, `split_whitespace` and `trim` give `Str` views (`split` and `lines` are now lazy iterators). Arguments forwarded to a `mut ref` parameter are now marked too. `karac fix` made the edits. The app matches expected.out on all four MIR backends again (karac at a4b0967495).
