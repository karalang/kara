# mini_pandas_calendar

A Kāra port of the calendar half of [pandas](https://pandas.pydata.org/)
3.0.5 (BSD-3-Clause): the date offsets of `tslibs/offsets.pyx` and
`to_offset`, the holiday calendars of `tseries/holiday.py`, `Period` and
`period_range`, Timedelta's parser, formatting and rounding,
`date_range` and `bdate_range`, with the pieces of numpy (BSD-3) they
stand on (`busdaycalendar`, `busday_offset`, `busday_count`,
`is_busday`) and dateutil's `relativedelta` and `easter` (Apache-2.0 or
BSD-3). Business calendars are where scripts that bill, schedule and
report meet dates: an SLA due date in business hours, the next payroll
day after a holiday, revenue by fiscal quarter. Jobs come one per line:

```
off 2026-11-25T16:30 BusinessHour(3)
freq 1.5h
range start=2026-03-07T12:00 periods=4 freq=D tz=America/New_York
brange start=2026-11-20 end=2026-12-04 freq=C cal=us
hol nyse 2026-01-01 2026-12-31
pasfreq 2026Q3 Q-JUN M e
pfmt 2026Q3 Q-JUN FY%F_Q%q
td -1_days_+23:59:59.5
busday count 2026-11-01 2026-12-01 cal=us
due tickets.csv opened add BusinessHour(8)
bucket sales.csv date Q-JUN amount
```

- `off <ts> <Offset(...)>` builds one of 38 offset kinds (ticks, `Day`,
  business days, hours and months, the custom ones with a holiday
  calendar and weekmask, month, quarter, half-year and year starts and
  ends, `SemiMonth`, `Week`, `WeekOfMonth`, `FY5253` and its quarters,
  `Easter`, `DateOffset` over `relativedelta`) and prints its repr,
  freqstr, `ts + off`, `ts - off`, `rollforward`, `rollback` and
  `is_on_offset`.
- `freq` is `to_offset`: aliases, multiples, `1.5h`, deprecated and
  renamed aliases and their warnings.
- `range` and `brange` are `date_range` and `bdate_range` with `start`,
  `end`, `periods`, `freq`, `inclusive`, `normalize`, `tz`, `holidays`,
  `cal` and `weekmask`.
- `hol` lists a calendar's holidays with names: pandas'
  `USFederalHolidayCalendar`, or an NYSE or UK calendar that `oracle.py`
  builds from pandas' `Holiday` rules and observances.
- `per`, `pasfreq`, `padd`, `pfmt`, `pdiff` and `prange` are `Period`,
  `asfreq`, `+ n`, `strftime`, `Period - Period` and `period_range`.
- `td`, `tdround` and `tsadd` parse and print a Timedelta, floor, ceil
  and round it, and add it to a Timestamp.
- `due` and `bucket` read a CSV: a column of dates plus, minus, or
  rolled by an offset, written back with `to_csv`; and a groupby on
  `dt.to_period(freq)` with a count and a sum.

## Running it

The program carries its inputs: it writes `data/` and runs
`data/jobs.txt`, 328 jobs over five CSV files that `mk.py` writes:
support tickets opened at all hours (**tickets.csv**), invoices issued
around month ends (**invoices.csv**), 471 daily sales across
two fiscal years (**sales.csv**), trades on and around market holidays
(**trades.csv**) and pay dates (**pay.csv**).

Each job prints `$ <job>`, then what Python prints, then the warnings it
raised (`warn: FutureWarning: ...`, 16 of them), or `! ` and the
exception (25 of the 328: `to_offset`'s `ValueError`s, `date_range`'s
validation, an out-of-bounds Timestamp).

```
python3 -s -P -B mk.py          # writes data/ and data/jobs.txt
python3 -I -B oracle.py > expected.out
python3 -I -B gen_kara.py       # copies data/ into the end of mini_pandas_calendar.kara
karac run mini_pandas_calendar.kara | diff - expected.out
```

`oracle.py` is the oracle: pandas 3.0.5, numpy and dateutil on Python
3.13.

## What pandas does, and so the port does

These are its answers, kept because `expected.out` is theirs:

- **`DateOffset(weekday=TH(4))` goes forward both ways.** From
  2026-11-01, `+` and `-` both give 2026-11-26 (Thanksgiving): negating
  a `relativedelta` negates its counts but not its `weekday`.
- **`DateOffset` is on offset everywhere**, so its `rollforward` and
  `rollback` never move a date, even `DateOffset(months=1)` on the 31st.
- **`BusinessHour` knows no holidays.** Three business hours after 16:30
  on the day before Thanksgiving is 11:30 on Thanksgiving. Its `offset`
  shows in the repr and is ignored when it is applied: one hour from
  16:30 with `offset=30min` is 09:30 the next morning, as without it.
  `BusinessHour(8)` from 16:45 on a Tuesday is 16:45 on Wednesday.
- **`D` is a calendar day and `24h` is not.** Across the March DST
  change in `America/New_York`, `freq=D` stays at 12:00 and `freq=24h`
  moves to 13:00.
- **The business offsets are not vectorized.** Adding
  `BusinessHour`, `CustomBusinessHour`, `CustomBusinessDay` or
  `CustomBusinessMonthEnd` to a column raises `PerformanceWarning:
  Non-vectorized DateOffset being applied to Series or DatetimeIndex.`;
  `DateOffset(days=30)`, `MonthEnd`, `BusinessMonthEnd`, `BusinessDay`
  and `Week` do not.
- **`H`, `T`, `S` and `A` are gone** in pandas 3: `freq H` fails with
  `Did you mean h?`, and the message wraps the inner `ValueError` whole,
  so its text appears twice. `Q-FOO` fails as `KeyError('Q')`.
- **Every `Period` with freq `B` warns** (`Period with BDay freq is
  deprecated`), and `period_range(freq=B)` adds a `PeriodDtype[B]`
  warning of its own.
- **`Period - Period` is an offset**: `2026-11` minus `2026-01` is
  `<10 * MonthEnds>`, and two business-day periods give
  `<20 * BusinessDays>`.
- **Fiscal quarters count from the year they end in.** `2026Q3` under
  `Q-JUN` is January to March 2026 (`FY2026 Q3`, ending 2026-03-31).
- **`busday_count` is signed**: December 1 back to November 1 is `-22`
  where forward it is 21: counting backwards takes in December 1 and
  stops before November 1.
- **Juneteenth starts on 2021-06-18**, the Friday it was first observed,
  since the 19th was a Saturday.

## Parallel work

Every job runs in its own `TaskGroup` task and the results join in
order, so the transcript is the same on every run and thread count.
The program matches all 2,021 lines of `expected.out` on `__mir-run`,
`__mir-llvm-run`, `__mir-llvm-asan` (no reports) and a `karac build`
binary, at `KARAC_THREADS=1` and `4`, and on five runs of the auto-par
build (0.01 s real).

## Where it differs from pandas

Deliberately, and outside what the jobs exercise:

- Time zones are UTC and `America/New_York` with the US rules in force
  since 2007, not the tz database; no job names a local time the clocks
  skip or repeat.
- Timestamps are nanoseconds in `i64`, so the range is pandas' (1677 to
  2262).
- The holiday calendars are the three the jobs name, written as rules
  in the program; a calendar is not a class to subclass.
- `Period.strftime` takes `%Y %y %m %d %H %M %S %j %a %A %b %B %W` and
  pandas' `%q %f %F %l %u %n`, in the C locale.

## The code, ported

About 5,060 lines before the data section, all new: the civil calendar
(days from civil and back, ISO weeks, Easter), Timedelta's string and
ISO 8601 parsers with their unit table and rounding modes, numpy's
business-day calendar and roll rules, every offset's `_apply`,
`is_on_offset`, `rollforward`, `rollback`, repr and freqstr, the
business-hour arithmetic, `FY5253`'s year ends, `to_offset`'s scanner
with its alias tables and deprecation map, the holiday rules with their
observances, `Period`'s ordinals, frequency groups and `asfreq`
dispatcher, `period_range`, and `date_range`'s regular and offset ranges
with time-zone handling and `inclusive` trimming.

## The Mend loop

`karac check` on the assembled program reported nine type and variant
names that must be PascalCase (`DT`, `BH`, `CBH`, `WOM`, `LWOM`,
`FY5253`, `FY5253Q`, `AF`, `CT`; renamed by hand, since the rename is not
machine-applied) and a closure with a declared return type, which
`karac fix` rewrote. A second `karac fix` applied 102 fixes: call-site
`mut` markers, `*` dropped on copied values, `own` on two parameters
and two `.clone()`s. Of the seven errors left, four were a borrowed
parameter assigned an owned value (`.clone()` where it is bound), two a
method that does not exist (`trim_end_matches`; it is `strip_suffix`),
and one a false positive, filed below. A tuple scrutinee
`(start, end)` then moved both; a nested `match` fixed that. The program
then matched pandas except for seven lines, all one bug of the port's
(a `KeyError` repr quoting its key twice).

## Gaps found

- **B-2026-10-29-37** (fixed in kara 39fd8d115b): `karac check` refused
  moving a binding after a non-escaping closure that only reads it, as
  "moved inside a loop" in a program with no loop; both MIR backends
  accepted and ran it. The fix covers a closure bound by `let`.
- **B-2026-10-29-43** (Thread A, open): the same refusal for a closure
  passed straight to an iterator adapter (`hols.iter().filter(|u|
  mask[..])` and then `mask` moved), which is this app's shape. `bus_cal`
  clones `mask` until it is fixed.

## What calendar work would need (notes for M4c)

`docs/library/data.md` in the kara repo has no date types (Decision Q1:
dates read as `String` in the first release), and `std.time` has
`Instant`, `SystemTime` and `Duration` but no civil date. What this port
shows a data package would need once dates arrive:

- **A civil date and a timestamp with a unit**, separate types, and a
  time zone carried by the column, not the value. Most of this port is
  the civil calendar every job leans on; it belongs in the standard
  library, under `kara-data`'s `Date32` and `Timestamp` columns.
- **Calendar steps and fixed spans as different things.** `D` versus
  `24h` across DST is the most common surprise here; a month step needs
  a stated clamp rule (January 31 plus a month).
- **Business calendars as data**: a weekmask and a holiday list, with
  numpy's four roll rules, `count` and `offset`, and holiday rules
  (fixed date, nth weekday, Easter-relative, observance) that build the
  list for a year range. pandas' class hierarchy and its `BusinessHour`,
  which ignores holidays, are the shape to avoid.
- **Periods for grouping**: `to_period(freq)` keys with fiscal year ends
  (`Q-JUN`, `Y-MAR`) for `group_by`, which data.md's first-appearance
  order already makes stable.
- **One offset algebra with no silent identities**: `DateOffset`'s
  "always on offset", the unsigned `weekday`, and the non-vectorized
  slow path are pandas' hazards; a step should either be defined on a
  date or be an error.
- **Frequency strings parsed once, with errors that name the alias**,
  and no aliases that change meaning between releases (`H`, `T`, `A`
  and `M` all did in pandas 2.2 to 3.0).
