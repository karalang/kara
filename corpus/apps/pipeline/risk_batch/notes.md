# pipeline/risk_batch

A nightly market-risk batch, the shape of the job a bank's risk team runs
after the close, over four trading desks (rates, equity, fx and credit).
It builds each desk's positions, writes them as CSV, reads them back and
parses them with a loader's checks, sums each desk's factor exposures,
prices every position over 250 days of historical-style scenarios, takes
each desk's VaR and expected shortfall, runs five reports per desk, and
books the four desks into one firm-wide limit book.

```
scenarios: 250 days, 11 crisis days, 10 issuer defaults
equity.bad line 152: bad delta '1.5e3'
fx.csv line 2: bad id 'fx-0001'
...
credit VaR99 527.2k, ES97.5 2.0m; books CR-IG 544.8k, CR-HY 303.1k, CR-IDX 1.6m; diversification 1.9m
credit worst days: d236* -6.1m, d031* -4.0m, d068* -527.2k, d104* -523.7k, d204 -427.3k; 4 of 5 on crisis days, 9 days with a default
...
equity booked: firm VaR 563.1k -> 706.8k, marginal 143.7k; LB-0001 desk over its 800.0k limit by 62.6k; headroom 493.1k
fx booked: firm VaR 706.8k -> 1.3m, marginal 662.8k; LB-0002 firm over its 1.2m cap by 169.6k
...
limits.log: 7 entries, 293 bytes, fnv 59f6f17b
```

- A desk has 240 positions, each in one of the desk's books, of one of
  its instrument kinds (swaps, bonds and futures; stocks, calls and puts;
  spot, forwards and options; CDS and bonds) and sensitive to one of its
  market factors: four rate tenors, three equity indices, three currency
  pairs, two credit spread indices. A position has a signed notional, a
  delta in dollars per basis point and, for options and rates bonds, a
  gamma. Every credit position names one of 16 issuers. Everything comes
  from a 32-bit integer hash of a seed and a position.
- The scenarios are one per day of a 250-day window: a common market move
  that each factor takes with its own beta, plus an own move per factor,
  each a sum of three uniform draws. Eleven crisis days triple the common
  move, and an issuer defaults on a day with odds of 1 in 500, or 1 in 60
  on a crisis day.
- The parser checks the header, the field count, the id's desk prefix and
  digits, that the book, kind and factor belong to the desk, the three
  integers (through `i64.parse`), a zero notional, a gamma on a linear
  instrument and the issuer field, and reports `file line N: what`. A copy
  of the equity file with a delta of `1.5e3`, and the fx file read as
  credit, show two of the errors.
- A position's P&L on a day is `delta * move + gamma * move² / 200`, plus
  its jump to default when its issuer defaults that day: a bond holder
  loses 60% of face and a protection buyer gains it. Each book's P&L and
  the desk's total are kept per day.
- 99% one-day VaR over 250 days is the third-worst day and 97.5% expected
  shortfall the mean of the worst six, so a credit desk whose two worst
  days are defaults has an ES nearly four times its VaR.
- The reports: totals by kind, net and gross exposure per factor (and the
  worst issuer for credit), VaR per book against the desk with the
  diversification between them, the five worst days marked when they are
  crisis days, and three named stress moves (crash, taper, rates +200).
- The limit book holds the firm's P&L per day. Booking a desk adds its
  P&L, recomputes the firm's VaR, attributes the change to that desk as
  its marginal VaR, and raises a numbered ticket when a desk is over its
  own limit or the firm goes over its cap. Every booking and ticket goes
  in `limits.log`, which is written and read back.

Everything is integer arithmetic (dollars, basis points, division that
truncates toward zero), so the backends and the Python mirror
`risk_batch.py` agree by construction; the mirror wrote `expected.out`.

stmt-par: eight groups of statements that do not depend on each other,
in `run()` (`risk_batch.orig.kara` lines):
- L615-L618 four desks of positions, each from its desk's seed;
- L621-L624 four `fs.write` calls, each with the `encode_positions` call
  that builds its text;
- L635-L638 four reads, each with `?`;
- L645-L648 four parses. Each can fail with `?`, so a parallel run has to
  report the first failure in source order;
- L661-L664 four exposure sums;
- L667-L670 four pricing runs, each every position over every day, which
  is where the time goes;
- L673-L676 four VaR and ES computations;
- L679-L698 twenty reports, five per desk, that only read what the
  earlier groups made.

The four `book` calls (L706-L709) are the case that must stay in order:
each takes `mut lb`, a desk's marginal VaR is measured against the desks
booked before it, and tickets are numbered as they are raised. Booked in
reverse, credit, fx and equity take marginals of 527.2k, 479.9k and
443.4k, rates diversifies (-5.0k), and it is equity, not fx, that takes
the firm over its cap, as LB-0002 after its own LB-0001.

## Edits from risk_batch.orig.kara to risk_batch.kara

- `GAMMA_KINDS.contains(kind)`, twice, on `const GAMMA_KINDS: Array[Str, 4]`,
  is `no method 'contains' on type 'Array'`, though an `Array` has every
  `Slice` method. Both are `GAMMA_KINDS.as_slice().contains(...as_str())`
  now. That is kara ledger row B-2026-10-24-23, which index_build filed;
  this is a second program that hits it.
- `desk(..)` took `books: ref Vec[Str]` and was called with
  `["RT-NY", "RT-LDN", "RT-TKY"].to_vec()`. A string literal is a `String`
  unless a `Str` is expected, and nothing expects one through `.to_vec()`,
  so the argument is a `Vec[String]` and `karac check` said so. That is
  the language as design.md §3 has it; `desk` takes `own Vec[String]` now
  and keeps the vectors instead of converting them.

After those edits it matched the mirror on the first run under the MIR
interpreter, the LLVM backend, the ASan build and the native binary, with
nothing on stderr.

## Running

```bash
karac run risk_batch.kara
karac build --mir risk_batch.kara && ./risk_batch
python3 -I -B risk_batch.py > expected.out
```

About 15 s on the MIR interpreter (a debug build of `karac`) and 0.02 s
built. It writes its six files to the working directory and removes them
at the end.
