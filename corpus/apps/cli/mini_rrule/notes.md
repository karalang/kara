# mini_rrule

iCalendar recurrence rules (RFC 5545 section 3.3.10), ported from
dateutil 2.9.0's `rrule` module, the expander under icalendar,
recurring_ical_events, Odoo's calendar and many job schedulers.

- `rrulestr`'s parser: `DTSTART`, `RRULE`, `RDATE`, `EXRULE` and
  `EXDATE` lines, the `unfold`, `forceset` and `compatible` options, and
  every error it raises, word for word.
- `rrule`'s expansion: all seven frequencies with `INTERVAL`, `COUNT`,
  `UNTIL`, `WKST` and every `BYxxx` part (`BYSETPOS`, `BYMONTH`,
  `BYMONTHDAY`, `BYYEARDAY`, `BYWEEKNO`, `BYDAY` with ordinals such as
  `-1FR`, `BYHOUR`, `BYMINUTE`, `BYSECOND`, and dateutil's `BYEASTER`),
  worked one period at a time over day masks the way dateutil does it.
- `rruleset`'s merge of rules and dates, minus exclusion rules and dates.
- `str()` of a rule, back to rule text.

The run writes 103 files to `data/rules/`. Each one holds a recurrence,
a `--` line, and queries, one per line:
- `all`, `list N`, `count`;
- `after DT [inc]`, `before DT [inc]`, `between A B [inc]` and
  `xafter DT N [inc]`;
- `in DT` and `at K`, with negative K counting from the end;
- `str`.

The files cover every example in RFC 5545 section 3.8.5.3, dateutil's
corner cases (the 31st of short months, leap days, ISO week 53 and week 1
across the year boundary, Easter offsets, `BYSETPOS` with times, HOURLY
and MINUTELY steps that can only reach some `BYHOUR` values), sets with
exclusions, 17 malformed rules, and two that fail only when queried.

`oracle.py` runs the same files through the real dateutil
(`pip install python-dateutil`) and wrote `expected.out`.
