# mini_timeago

A port of [timeago](https://pypi.org/project/timeago/) 1.0.16 (MIT,
hustcc): the library that turns a timestamp into "3 hours ago" or
"in 2 days", in 44 languages.

```
> format('2026-10-23 14:29:49', datetime.datetime(2026, 10, 23, 14, 30), 'de')
  -> vor 11 Sekunden
> format(datetime.timedelta(days=1825), datetime.datetime(2026, 10, 23, 14, 30), 'ru')
  -> 4 года назад
```

## What it does

`mk.py` writes 7 jobs to `data/jobs.tsv`: 3525 calls on `timeago.format()`
and 71 on `timeago.parser.parse()`, the labels of a team's activity feed
(commits, deploys, comments and scheduled jobs). `oracle.py` replays them
through the vendored library, and `mini_timeago.kara` replays them through
the port. Each call prints with its arguments as `repr()` shows them,
then its result or `error <Class>: <message>`.

- **The feed.** Log timestamps from two seconds to ten years either side
  of "now", in English, German and Chinese.
- **Unit boundaries.** Differences on both sides of every step: 9 and 10
  seconds ("just now" until 10), 60 and 120 seconds, an hour, a day, a
  week, a month (365/7/12 weeks, 2,628,000 seconds) and a year, up to the
  largest `timedelta`. Microseconds truncate toward zero on both sides.
- **Every locale.** All 44 over the same 15 differences, past and future.
- **Plural rules.** The six locales that choose a form by the number
  (Russian, Ukrainian, Polish, Lithuanian, Arabic and Slovak) over 10 to
  59 seconds, 1 to 59 minutes and up to 1022 years. The years show the
  library's float arithmetic as it is: 1825 days is "4 years", because
  dividing by 365/7/12 leaves 59.99... months.
- **Locale names.** `None`, unknown names, case, a hyphen, a trailing
  space and a non-string all fall back to English.
- **The parser.** Strings with `-` or `/`, a date alone, a time alone,
  extra fields, invalid dates (February 29 of 2015, month 13, year 0 and
  10000) and times, wrong spacing, signs, underscores and Arabic-Indic or
  fullwidth digits (which `int()` reads); dates, naive and aware
  datetimes, int, float and bool timestamps, `None` and a list.
- **Format inputs.** Every kind of value on either side, aware datetimes
  with different offsets, the `TypeError` for an aware datetime against a
  naive one, `ParameterUnvalid` for a date or a "now" the parser rejects,
  and a `timedelta`, which ignores "now".

## How the port works

- **`format()` follows the library line for line**, dividing the seconds
  by the same float steps, so its rounding is reproduced exactly.
- **`timedelta.total_seconds()` is rounded once from the exact
  microsecond count**, as Python's integer division rounds it, using the
  correctly rounded decimal-to-float conversion from `mini_markupsafe`.
  The difference matters: from year 1 to year 9999 the microseconds round
  the seconds up by one.
- **`datetime.fromtimestamp()` of a float** splits the float with `modf`,
  rounds the fraction to microseconds half to even and carries into the
  seconds, as CPython does.
- **Dates are counted in days from 1970-01-01**, with the subtraction
  done per field and normalized as `timedelta` normalizes it. `repr()` of
  dates, datetimes, time zones and timedeltas follows CPython's.
- **The locale tables are data**: `gen_kara.py` reads each locale file
  with `ast.literal_eval` and writes `locale_text()`. The six plural
  functions are ported by hand.
- **`gen_kara.py` also writes `jobs_text()`** and `digits_text()` (the
  runs of decimal digits `int()` reads) at the end of the `.kara` file, so
  the kata reads no files at run time.

## Compiler gaps

None. The first draft needed two edits from `karac check`: struct names
must be PascalCase (`DT` became `DateTime`, `TD` became `Delta`), and an
element of a `Vec` of a non-`Copy` struct has to be borrowed with `ref`.
The port then matched the oracle on every backend.

## Scope

- **Two inputs are pinned** so the run is reproducible: the time zone is
  UTC, and `date.today()`, which the parser uses for a time with no date,
  is 2026-10-23. Every `format()` call passes "now", except those given a
  `timedelta`.
- **Time zones are fixed offsets** (`datetime.timezone`), the kind an API
  timestamp carries.
- **`str.isspace()` is Unicode `White_Space` plus U+001C..U+001F**, and
  `str.isprintable()` is approximated for the characters the jobs hold.
- **`time` values and other `tzinfo` classes are not passed.**

The vendored library is unchanged.

## Running

```bash
karac run mini_timeago.kara               # interpret
karac build --mir mini_timeago.kara       # native binary
python3 -s -P -B mk.py                    # regenerate data/jobs.tsv
python3 -I -B oracle.py > expected.out    # regenerate expected.out
python3 -I -B gen_kara.py                 # regenerate jobs_text()
```
