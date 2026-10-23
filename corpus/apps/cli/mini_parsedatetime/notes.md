# mini_parsedatetime

A port of [parsedatetime](https://pypi.org/project/parsedatetime/) 2.6
(Apache-2.0; Mike Taylor and contributors): the library that reads
"tomorrow at 9:30am", "3 days ago", "next friday at 3pm" or "4-5pm" as a
date and time, the way reminder bots, to-do tools and schedulers use it.

```
> agenda inbox/standup.txt
  wrote out/standup.agenda (14 lines)
  | 1	17-35	tomorrow at 9:30am	2026-10-24 09:30:00	pdtContext(accuracy=pdtContext.ACU_DAY | pdtContext.ACU_HOUR | pdtContext.ACU_MIN)
  | 2	9-27	next friday at 3pm	2026-10-30 15:00:00	pdtContext(accuracy=pdtContext.ACU_DAY | pdtContext.ACU_HOUR)
```

## What it does

`mk.py` writes an inbox of notes (a stand-up, a family chat, an ops
channel and an empty file) and a to-do list with due times to `data/`,
and a script of 1407 steps to `data/jobs.tsv`. `oracle.py` replays the
script through the vendored library and `mini_parsedatetime.kara` replays
it through the port. Time tuples print as tuples, everything else as
`repr()` shows it, and an exception as `error <class>: <message>`.

- **Every phrase on a Friday afternoon.** 256 calls on `parse()`
  with dates in each order and separator, month and weekday names, times
  with and without a meridian, units and quantities in digits and words,
  modifiers ("next", "last", "this", "end of", "from now", "ago"), and
  text that is not a date at all, in both the old flag style and the
  context style, and on `parseDT()`.
- **Other clocks**: a share of the same phrases at six other clocks,
  among them a leap day and the last second of a year (414 calls), and the edges of the calendar (9999-12-31,
  0001-01-02), where the library raises or swallows `OverflowError` and
  `ValueError` as it goes, and an explicit `sourceTime`.
- **The constants** that change answers: `StartHour`,
  `StartTimeFromSourceTime`, `YearParseStyle`, `DOWParseStyle`,
  `CurrentDOWParseStyle` and `BirthdayEpoch`.
- **The Australian locale** (day before month), and a locale name the
  library does not have, which falls back to US English.
- **Ranges** (`evalRanges`), **finding dates in text** (`nlp`, with each
  match's span and text) and the helpers (`inc`, `_convertUnitAsWords`,
  `_quantityToReal`, `_CalculateDOWDelta`, `parseDate`, `parseDateText`).
- **A reminders tool**: for each inbox file it writes an agenda under
  `data/out/` (each date found, line by line, or the line itself when
  none is), and it sorts the to-do list by due time with the undated
  tasks last; the clock then moves to the end of the year and the list is
  sorted again.

## How the port works

- **Text is a Vec of code points**, and a datetime is a count of
  microseconds from 0001-01-01, with `timedelta`'s rounding of float
  arguments reproduced so fractional units land where CPython puts them.
- **The regular expressions are the library's.** `gen_kara.py` reads each
  locale's compiled sources (`cre_source`, `IGNORECASE | VERBOSE`) and,
  by running each function once with `re` wrapped, the patterns the
  Calendar passes to `re.sub`, `re.search` and `re.split` itself. The
  port compiles them for a backtracking matcher with named groups,
  backreferences, conditionals, lookaround and repetition that stops
  after an empty iteration, as `sre` does.
- **The locale tables come from `Constants()`** after it has filled them
  in (months, weekdays, units, modifiers, offsets, number words and
  magnitudes), so the port reads the same words the library does.
- **The pdtContext stack is a stack of accuracies**, pushed and popped as
  the library's `with self.context():` blocks do, so each answer carries
  the same `ACU_*` flags.
- **The library's quirks are kept**: a range like "3-7" always gets "PM"
  (its `re.search('AM', …)` runs on lower-cased text), `inc()` treats a
  month or year it cannot read as 0, and an overflow inside `_buildTime`
  is swallowed.

## Compiler gaps

- **B-2026-10-23-10**: a top-level `fn todo(..)` is accepted but never
  called; calls go to the builtin `todo()`. The function is named
  `todo_tool`.

`karac fix` made 55 edits in three passes. By hand, two struct names were
made PascalCase, `ref loc(w, c).rx` became a borrow of the locale bound
first (only a local or `ref v[i]` can be borrowed), indexed by the
calendar's locale number so it does not hold a borrow of the calendar,
and one borrowed parameter was cloned before it is reassigned. The port
then matched the oracle on every backend.

## Scope

- US English and Australian English only; other locales fall back to US
  English as the library does when one is missing.
- ASCII text and digits. Number magnitudes above 10**18 are out of range.
- `_evalDT`'s RFC 822 and W3C DTF branches are not reached: the source
  time is never `None` there.
- `float()` reads decimal forms only (no `inf`, `nan` or underscores).

## Running

```bash
karac run mini_parsedatetime.kara                 # interpret
karac build --mir mini_parsedatetime.kara         # native binary
python3 -s -P -B mk.py                            # regenerate data/
python3 -I -B oracle.py > expected.out            # regenerate expected.out
python3 -I -B gen_kara.py                         # regenerate the generated tables
```
