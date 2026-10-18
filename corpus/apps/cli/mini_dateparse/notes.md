# mini_dateparse

dateutil 2.9.0's date parsing, the library countless scripts use to read
a date that a person or another program wrote. Three parts are ported:
- `parse()` from `dateutil/parser/_parser.py`, the forgiving parser, with
  `dayfirst`, `yearfirst`, `fuzzy`, `fuzzy_with_tokens`, `ignoretz`,
  `default` and `tzinfos` (names mapped to offsets in seconds, or to
  `None`). It covers the lexer's reading of dots and commas, the
  year/month/day guesser, 12-hour clocks, `h`/`m`/`s` units, offsets with a
  zone name in brackets, the POSIX sign flip of `GMT+3`, and its warning
  for zone names it does not know.
- `isoparse()` from `isoparser.py`, the strict ISO-8601 parser, with
  week and ordinal dates, `24:00`, the separator option and every error
  message, including Python's `int()` errors on the string's bytes.
- `relativedelta` from `relativedelta.py`: the difference of two
  datetimes, and a datetime moved by years, months, weeks, days, leap days,
  a weekday (`FR(-1)`) or absolute fields and year days, with its `repr()`.

Python's `datetime` is ported alongside, as far as these need it: its
range checks and their messages, its arithmetic and overflow, `repr()`
and `isoformat()`.

dateutil's quirks are kept. Some examples: `nan` and `inf` read as
numbers and then fail, `10 am pm` is 22:00, `UTC` gives `tzlocal()` but
`GMT` gives `tzutc()`, and a `relativedelta` prints its counts with `+g`,
so 1234567 days is `days=+1.23457e+06`.

dateutil reads two things from the machine: the local zone and the current
year, which decides the century of a two-digit year. Here the local zone is
UTC and the year is 2026, and `oracle.py` pins both. Every `parse()` gets
2003-09-25 as its `default` unless the job gives one.

The run writes `data/jobs.jsonl` and five files to `data/files/`, then
runs the 578 jobs, each printed after `== ` and the job:
- `parse`, `iso`, `diff` and `shift` jobs call the library on one string
  or one pair;
- `file` jobs do what a program does with the library: merge an
  application log whose lines carry a dozen timestamp formats into UTC
  order, read mail `Date:` headers, work out ages and next birthdays,
  list subscription renewals that fall at the end of short months, and
  pull dates out of free-text notes.

Digits are ASCII: dateutil also takes other Unicode digits, and the jobs
use none. A minute or hour fraction is read to 17 digits.

`oracle.py` runs the same jobs through the real dateutil
(`pip install python-dateutil==2.9.0.post0`), and wrote `expected.out`.

Author errors: a struct named `RD` (struct names are PascalCase, so it is
`RelDelta`), a `width` helper used before it was written, and a missing
`mut` marker on an argument to a `mut ref` parameter.

Compiler gaps: none. The first draft that passed `karac check` matched
expected.out on all four MIR backends.

Written 2026-10-18 by the real-world apps thread; karac at b6761f33ac.
