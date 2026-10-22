# mini_isoweek

A port of [isoweek](http://github.com/gisle/isoweek) 1.3.3 (BSD, Gisle Aas) —
the `Week` class that reporting, scheduling and fiscal-week code uses to name
and do arithmetic on ISO 8601 weeks. A `Week` is a `(year, week)` pair where
week 1 is the first week with four or more days in January.

## What it does

`mk.py` writes `data/jobs.tsv`, one tab-separated job per line (an op name and
its arguments). `oracle.py` runs each through `vendor/isoweek.py` and prints
`expected.out`. `mini_isoweek.kara` reimplements the class and must match
byte-for-byte on every backend.

## How the port works

The library is a thin layer over `datetime.date`, so the port reimplements the
proleptic-Gregorian calendar it leans on — `ymd ↔ ordinal`, the ISO-calendar
rule (`isoweek1monday`) and the weekday — in `i64`, then builds every `Week`
method on top:

- **construction** with the library's out-of-range-week normalization:
  `Week(2011, 0)` rolls back to `2010W52`, `Week(2011, 53)` rolls forward past
  the 52-week year to `2012W01`, while `Week(2020, 53)` stays (2020 genuinely
  has 53 weeks). This is `cls(year, 1) + (week - 1)`, so it goes through the
  week↔ordinal round-trip;
- `fromstring` (`2011W08` and `2011-W08`) with the exact `ValueError`;
- `withdate`, where the Jan-1 / Dec-31 boundary makes the ISO year differ from
  the calendar year (2021-01-01 → `2020W53`, 2012-12-31 → `2013W01`);
- the seven `days()`, `day(num)`, `monday`/`sunday`;
- `toordinal` / `fromordinal`, `week + n`, `week - week`, `contains`,
  `replace`, and `last_week_of_year` / `weeks_of_year` (52 vs 53).

The one place Python's **floor** division matters is `isocalendar`: a date that
falls before its calendar year's ISO week 1 gives a negative week quotient, and
that negative is the signal to back up a year. Since `i64` division truncates
toward zero, the port uses an explicit `floordiv` there so the sign is right.

## Scope

Years are in the library's own `1..9999` range. The `datetime.timedelta`
overloads of `+`/`-` reduce to the integer forms (`days // 7`) and are covered
by the integer cases; the live `thisweek()` (today's date) is not reproduced,
as it has no fixed oracle.

## Running

```bash
karac run mini_isoweek.kara              # interpret
karac build --mir mini_isoweek.kara      # native binary
python3 -s -P -B mk.py                     # regenerate data/jobs.tsv
python3 -I oracle.py                        # regenerate expected.out
```
