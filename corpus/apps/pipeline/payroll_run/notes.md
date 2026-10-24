# pipeline/payroll_run

A biweekly payroll run, the shape of a payroll service's batch for one
company with four sites (Denver, Austin, Oakland, Boston). It builds each
site's roster and timesheets, writes them as CSV, reads the files back and
parses them with the checks a payroll import makes, turns punches into
regular, overtime and double-time hours, computes gross pay, deductions and
withholding, runs five reports per site, and posts the payments against one
bank account as an ACH-style file.

```
oakland.bad line 302: bad time '25:61'
boston.times line 1: bad header
...
  OAK0002 Danjo Karimor (hourly, S): gross $1,805.65 - 401k $108.34 - health $132.00 - fed $151.22 - ss $103.77 - medicare $24.27 - state $39.21 = $1,246.84
...
oakland overtime: 5746.5h at 1.5x, 37.3h at 2x; most: OAK0039 25.3h over 13 days; OAK0735 22.1h over 13 days; OAK0714 22.0h over 10 days
...
boston posted: 611 payments, checks 103601-104211, 589 held ($1,551,330.56), balance $2.86
payments.ach: 4211 entries, 589 held ($1,551,330.56), 240027 bytes, fnv 2653ff2b
```

- A site has 1200 employees. About 30% are salaried (an annual salary paid
  in 26 periods), the rest hourly at $18.00 to $65.00. Each has a filing
  status, a 401(k) rate, a health plan and the Social Security wages paid
  so far this year; one in ten is near or past the wage base. Everything
  comes from a 32-bit integer hash of a seed and a position, and all money
  is integer cents.
- An hourly employee works about five days in seven, 4 to 11 hours a day,
  with a 30-minute unpaid break on a shift over 6 hours and the odd 14-hour
  day. One punch in 200 has no punch-out and one in 200 is followed by an
  overlapping duplicate. Three sites also carry a punch for an id that is
  not on the roster.
- The parsers check the header, the field count, the id's shape, the kind,
  every number, the filing status, a 401(k) rate over 15%, the plan, the
  day, each `HH:MM` time and a punch-out before its punch-in, and report
  `file line N: what`. A copy of the Oakland timesheet with an impossible
  time on one line, and the Boston timesheet read as a roster, show two of
  the errors.
- Hours: each day's worked minutes are the sum of its punches less breaks.
  Oakland follows California's daily rules (over 8 hours is overtime, over
  12 is double time); every site moves regular hours over 40 in a week to
  overtime. Missing, overlapping and unknown punches are left out and
  counted as anomalies.
- Pay: hourly gross is regular, 1.5x and 2x minutes at the rate; the
  401(k) is a percentage of gross and the health premium comes off before
  tax. Federal withholding annualizes the period's taxable pay over a
  simplified bracket table per filing status. State tax is California's
  brackets, a flat 4.4% in Colorado and 5% in Massachusetts, none in Texas.
  Social Security is 6.2% up to the wage base, counting the year so far;
  Medicare is 1.45% plus 0.9% past $200,000 for the year. The employer pays
  its own matching share.
- The reports: totals with three payslips, overtime with the three
  employees who worked the most of it, anomalies, taxes by kind with the
  number of employees past the wage base, and net pay (median, p90, a
  histogram by $1000).
- The ledger starts with $11,500,000. Sites post in turn; each payment
  gets the next check number and trace number and a 56-character ACH entry
  line, and a payment larger than what is left is held. The money runs
  out partway through Boston.

The brackets and rates are simplified (no standard deduction, no W-4
allowances, no local taxes), so the numbers are realistic in shape but are
not any year's actual withholding.

Everything is integer arithmetic, so the backends and the Python mirror
`payroll_run.py` agree by construction; the mirror wrote `expected.out`.

stmt-par: eight groups of statements that do not depend on each other, in
`run()` (`payroll_run.orig.kara` lines):
- L792-L795 four rosters, each from its site's seed;
- L798-L801 four timesheets;
- L804-L811 eight `fs.write` calls, each with the `encode_roster` or
  `encode_times` call that builds its text;
- L822-L829 eight reads, each with `?`;
- L840-L847 eight parses. Each can fail with `?`, so a parallel run has
  to report the first failure in source order;
- L862-L865 four timecards;
- L868-L871 four pay runs;
- L874-L893 twenty reports, five per site, that only read what the
  earlier groups made.

The four `post` calls (L901-L904) are the case that must stay in order:
each takes `mut gl`, the balance runs down as each site posts, a payment
that no longer fits is held, and check and trace numbers go out in posting
order. Posting Boston before Oakland holds Oakland's payments instead, and
renumbers every check after Austin's.

## Edits from payroll_run.orig.kara to payroll_run.kara

All author errors, and every diagnostic but one was right:
- `vec[...]` (a Rust habit) in five tables. `karac check` says `undefined
  name 'vec', did you mean 'Vec'?`; the fix is a plain list literal, which
  takes the function's `Vec` return type.
- `.map(...)` straight on two list literals. The message says adaptors need
  an explicit `.iter()` and shows the line with it.
- A `Str` constant (`ROSTER_HEADER`, `TIMES_HEADER`) passed where a
  `ref String` parameter was declared, twice. `karac fix` added
  `.to_string()`.
- The eight file texts were `u1` .. `u8`, and `u8` is a reserved type
  name. The design says binding it is an error, but `karac check` accepts
  the `let` and instead reports `no associated function 'len' on type
  'u8'` at `{u8.len()}`, which never names the binding. They are `txt1` ..
  `txt8` now. That is kara ledger row B-2026-10-24-22.

After those edits it matched the mirror on the first run under
`karac run`, the MIR interpreter, the LLVM backend, the ASan build and the
native binary, with nothing on stderr.

## Running

```bash
karac run payroll_run.kara
karac build --mir payroll_run.kara && ./payroll_run
python3 -I -B payroll_run.py > expected.out
```

About 24 s on the MIR interpreter (a debug build of `karac`) and 0.08 s
built. It writes its ten files to the working directory and removes them
at the end.
