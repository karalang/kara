# mini_calendar

Python 3.13's `calendar` module ported from `calendar.py` (`Calendar`,
`TextCalendar`, `HTMLCalendar` and the module functions), and a team
agenda built on it: recurring meetings, paydays that move off holidays,
and working days per month.

The run writes the team's meeting rules and the year's holidays into
`data/`, then runs `data/jobs.tsv`, 48 jobs with tab-separated fields.
Each job prints `== ` and its fields joined by ` | `, then its output.

The jobs cover:
- `isleap`, `leapdays` (an empty range, years 1 to 10000), `weekday`
  (including year 1, and years 0 and 10000 that Python moves into
  2000..2399), `monthrange` and its `IllegalMonthError` for months 0
  and 13;
- `TextCalendar.formatmonth` with first weekdays Monday, Sunday,
  Wednesday and 13 (which wraps to Sunday), wider columns (`w` of 3, 4
  and 9, which switches to full day names), extra line spacing, and
  February 2015, which fits in four weeks;
- `monthdayscalendar` and `itermonthdays3`, which runs into the months
  on either side;
- `formatyear` with three and four months a row, a wider spacing and a
  narrow one;
- `HTMLCalendar.formatmonth` and `formatyear` (four months a row);
- `timegm` around the epoch and at 2^31 seconds;
- `agenda`: a month's calendar and its events, from `data/rules.tsv`
  (weekly standups, the second Tuesday, the last Thursday, a fifth
  Friday that most months lack, payday on the last working day, rent
  on the 1st, a review on the 31st that falls back to the month's last
  day) and the holidays in `data/holidays.tsv`;
- `workdays`: Monday-to-Friday days per month of 2026, less holidays.

**The port** follows `calendar.py` method for method:
`itermonthdays` pads with `(day1 - firstweekday) % 7` zeros, the text
formatters `rstrip` each line and centre with `str.center`'s rule (the
extra space goes left when margin and width are both odd), and
`formatyear` centres each column with `formatstring`. Weekdays come
from a days-since-epoch count in the proleptic Gregorian calendar, as
`datetime.date` uses, so September 1752 has all its days. Python's
`//` and `%` round toward minus infinity, so the port has `floordiv`
and `pymod` for the negative cases.

Not supported: locales (`LocaleTextCalendar`), `HTMLCalendar.formatyearpage`
and the command-line interface.

**Oracle.** `expected.out` is Python 3.13.16's output for the same jobs,
made by `oracle.py` with the real `calendar` module. Run `mk.py` first;
it writes `data/`. `gen_kara.py` prints the Kāra data section from the
same files.

## Edits from mini_calendar.orig.kara to mini_calendar.kara

Author errors:
- `day_name`, `month_name` and `css_day` were written as
  `match d { ... }.to_string()`. A `match` in statement position ends at
  its `}` (design.md § Statements), so the parser stopped at the `.`
  with "Expected expression, found Dot", and `karac fix` found nothing
  to apply. Each now binds the `match` and returns `s.to_string()`.

Compiler gaps:
- B-2026-10-18-25: design.md's remedy for this, parenthesizing the
  `match` (`(match d { ... }).to_string()`), fails with the same parse
  error, as does its own example `(if c { a } else { b }).len()`. So
  binding the value first is the only form that parses.

After those edits the program matched expected.out on both MIR surfaces.

Updated 2026-10-18, karac at cff4e969aa: B-2026-10-18-25 is fixed, and
the parser now says a statement-position `match` ends at its `}` and
offers a machine fix. So `mini_calendar.kara` is now exactly what
`karac fix` makes of `mini_calendar.orig.kara`: the three `match`
expressions in parentheses, `(match d { ... }).to_string()`. It still
matches expected.out on all four backends.

Written 2026-10-18 by the real-world apps thread; karac at 79a6c2eb9a.
