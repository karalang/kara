# pipeline/replenish_plan

A weekly replenishment batch, the job a retailer's supply-chain team runs
over its distribution centres (north, south, east and west). It builds
each centre's SKUs with a year of weekly sales, writes them as CSV, reads
them back and parses them with a loader's checks, fits a Holt forecast to
every SKU over a grid of smoothing constants, ranks the SKUs ABC by
revenue, plans reorder points and order quantities, runs five reports per
centre, and sends the four centres' purchase orders to one supplier.

```
east.bad line 79: bad w27 '12.5'
south.csv line 2: bad sku 'SO-0001'
...
east wmape: household 28.3%, toys 24.9%, electronics 38.4%; most chosen alpha .20 beta .00 (19 skus)
...
north orders: 53 lines, 6,312 units, $50,961; largest NO-0084 342 $3,074, NO-0043 636 $2,862, NO-0080 258 $2,319
...
PO-0003 east: 2,450 of 3,812 units filled, 15 backorders from BO-0002; left dry 0, seasonal 348, hard 550
PO-0004 west: 898 of 2,414 units filled, 35 backorders from BO-0017; left dry 0, seasonal 0, hard 0
supplier.log: 273 entries, 7637 bytes, fnv 33394c10
```

- A centre stocks three of seven categories (grocery, household, pets,
  garden, outdoor, toys, electronics) and has 120 SKUs. Each category has
  a price, a bound on a SKU's base weekly units, a seasonal swing that
  peaks in its own week, a supplier lead time, a case pack and a supplier
  family (dry, seasonal, hard). A SKU's 52 weeks of sales are its base
  times the season times up to 20% noise, doubled in a promotion week; a
  sixth of the SKUs sell sporadically, and about one week in 31 the shelf
  was empty. Everything comes from a 32-bit integer hash of the centre's
  seed and the SKU.
- The CSV has a SKU's id, category, on-hand and on-order units and one
  column per week, with `-` for a stockout week. The parser checks the
  header, the field count, the id's centre prefix and digits, that the
  category is stocked at the centre, and every count, and reports
  `file line N: what`. A copy of the east file with `12.5` in week 27, and
  the south file read as west, show two of the errors.
- The forecast is Holt's linear smoothing in milli-units, run for 9 alphas
  times 5 betas per SKU. Weeks 27 to 52 are the holdout: the fit with the
  lowest absolute error there wins, the first in grid order on a tie. A
  stockout week carries the forecast forward without an update, since an
  empty shelf says nothing about demand.
- ABC is by revenue: A to 70% of the centre's revenue, B to 90%, C the
  rest. The reorder point is the forecast over lead time plus a review
  week, plus safety stock: the class's service factor (98%, 95%, 90%)
  times 1.25 times the holdout's mean absolute error times the square root
  of the lead time plus a week (an integer square root). A SKU at or
  under its reorder point orders up to four more weeks of forecast,
  rounded up to its case pack.
- The reports: units, revenue, stockout weeks and the top category; the
  ABC split; weighted MAPE per category and the most chosen constants;
  the order lines with the three largest by value; and weeks of cover
  with the dead stock (on hand, no sales in eight weeks).
- The supplier has one week's capacity per family. Each centre's PO is
  filled line by line in SKU order from what is left, in whole cases, and
  a short line raises a numbered backorder. Every line and backorder goes
  in `supplier.log`, which is written and read back.

Everything is integer arithmetic (units, cents, milli-units, division that
truncates toward zero), so the backends and the Python mirror
`replenish_plan.py` agree by construction; the mirror wrote `expected.out`.

stmt-par: eight groups of statements that do not depend on each other,
in `run()` (`replenish_plan.orig.kara` lines, the same in
`replenish_plan.kara`):
- L647-L650 four centres of SKUs, each from its centre's seed;
- L653-L656 four `fs.write` calls, each with the `encode_skus` call that
  builds its text;
- L667-L670 four reads, each with `?`;
- L677-L680 four parses. Each can fail with `?`, so a parallel run has to
  report the first failure in source order;
- L694-L697 four forecasts, 45 Holt runs over 52 weeks for each of 120
  SKUs, which is where the time goes;
- L700-L703 four ABC ranks;
- L706-L709 four plans;
- L712-L731 twenty reports, five per centre, that only read what the
  earlier groups made.

The four `submit` calls (L739-L742) are the case that must stay in order:
each takes `mut sup`, a centre gets only the capacity the centres before
it left, and backorders are numbered as they are raised. Submitted in
reverse, west is filled in full, east is short 13 lines, south 13 and
north 22, and the PO and backorder numbers all change.

## Edits from replenish_plan.orig.kara to replenish_plan.kara

All three were my mistakes, and `karac check` named each:

- `const Z` is a one-letter name, which is Type-class, so it cannot be a
  const. It is `SERVICE_Z`.
- `let s = skus[i];` and five bindings like it move a non-`Copy` struct
  out of an index. The message says to borrow it, `ref skus[i]`, which is
  what the code meant. Its fix-it adds `#[derive(Clone)]` instead, which kara
  ledger row B-2026-10-24-18 already covers.
- `m.get(k).copied().unwrap_or(0)` (twice) is `no method 'copied' on type
  'Option'`. Kāra's `Option[ref V]` has `.cloned()`, and for a `Copy`
  value the library's idiom is `m.get(k) ?? 0`, which the code uses now.
  The message names neither, filed as B-2026-10-24-26.

After those edits it matched the mirror on the first run under the MIR
interpreter, the LLVM backend, the ASan build and the native binary, with
nothing on stderr.

## Running

```bash
karac run replenish_plan.kara
karac build --mir replenish_plan.kara && ./replenish_plan
python3 -I -B replenish_plan.py > expected.out
```

About 5 s on the MIR interpreter (a debug build of `karac`) and 0.02 s
built. It writes its six files to the working directory and removes them
at the end.
