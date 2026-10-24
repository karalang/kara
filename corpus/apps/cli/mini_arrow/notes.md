# mini_arrow

A port of [arrow](https://github.com/arrow-py/arrow) 1.3.0, the Python
date and time library, together with the parts of
[python-dateutil](https://github.com/dateutil/dateutil) 2.9.0.post0 it
stands on: `tz` (tzfile, tzutc, tzoffset, tzlocal, gettz) and
`relativedelta`. Both are Apache-2.0 (dateutil also BSD-3-Clause); the
sources the oracle runs are vendored under `vendor/` with their licences
(`LICENSE-arrow.txt`, `LICENSE-dateutil.txt`). Left out of `vendor/`:
`six`, which dateutil imports and which the oracle takes from the
environment, dateutil's parser, and the zoneinfo tarball dateutil
bundles. Without the tarball dateutil warns once on stderr when it looks
up a zone name that is not in `data/zones`; stdout is the same.

```
== 93 get iso week 53 bad
ValueError: Invalid week: 53
== 156 parse 'YYYY YYYY' '2024 2024'
ParserMatchError: Failed to generate regular expression pattern: redefinition of group name 'YYYY' as group 3; was group 2 at position 78.
== 205 format '[unclosed YYYY'
[unclo7e6 2024
== 437 log normalizer
nginx   2024-03-10T04:58:12+00:00  local Sat 23:58:12 -05:00
syslog  2024-03-10T06:30:00+00:00  local Sun 02:30:00 -04:00 (no such local time)
api     2024-03-10T02:15:31+00:00  local Sun 07:45:31 +05:30
...
window: 5 hours 59 minutes and 29 seconds (5 hours ago)
== 438 meeting planner
Quarterly planning, Sunday 3 November 2024 01:30 PDT, in 8 months (2024-11-03T08:30:00+00:00); happens twice on the organizer's clock
  2024-11-03 01:30 -07:00  New_York 03:30-06:30* | London 08:30-11:30
```

## What it does

`mk.py` copies eight zones from the system tzdata into `data/zones`
(New York, Los Angeles, London, Berlin, Kolkata, Sydney, Tokyo, São
Paulo) and writes `data/jobs.jsonl`, 440 jobs. 436 of them build an
Arrow and run operations on it, 908 in all:

- **Construction**: `Arrow(...)` with named zones, offsets, `local`,
  `(UTC+09:30)`-style names, folds on both sides of a fall-back hour, a
  wall time that does not exist, local mean time in 1850, the São Paulo
  midnight gap of 2018, and times after the last transition of the 32-bit
  data dateutil reads. `fromtimestamp` with seconds, milliseconds,
  microseconds, strings and half microseconds, `utcfromtimestamp`,
  `fromordinal`, and the constructor's errors.
- **`get()`**: every dispatch of `ArrowFactory.get`: timestamps, ISO
  strings through `parse_iso` (dates, ordinals, week dates, basic and
  extended times, fractions rounded on the seventh digit, `24:00`, zones),
  ISO tuples, datetimes and dates with and without a zone, three or more
  arguments, and the `TypeError`s.
- **`get(string, format)`**: 82 parses with arrow's tokens: month and day
  names, ordinals, meridians, `X` and `x` timestamps, `ZZZ` zone names,
  `DDDD` day of year, `W` week dates, bracketed literals, the word
  boundaries arrow wraps each pattern in, unknown tokens, format lists,
  and 23 parses in French, German, Spanish, Italian, Russian, Polish,
  Japanese, Chinese, Korean, Dutch, Portuguese, Finnish and Czech.
- **`format()`**: every token, bracketed literals (and an unclosed one),
  tokens with no branch, which arrow's `re.sub` drops, and 17 locales.
- **`to()`, `shift()`, `replace()`, `+ timedelta`**: across DST in both
  directions, month-end clipping, `weekday=`, overflow at both ends.
- **`span()`, `floor()`, `ceil()`, `range()`, `span_range()`,
  `interval()`**: every frame, bounds, `exact`, `week_start`, `limit`,
  `tz`, and the day-clipping rule of `range`.
- **`humanize()`**: the automatic thresholds in both directions, every
  granularity alone and in lists, `only_distance`, and 15 locales.
  **`dehumanize()`** in 13 locales, and its errors.
- **Comparisons**: `==` across zones for an ambiguous time (PEP 495 makes
  it unequal to everything outside its zone), subtraction within one zone
  (wall time, so a DST day is 24 hours) and across zones, `is_between`,
  `isoformat` with every `timespec`.

The last four jobs are small tools built on the library:

- **log normalizer**: `data/app.log` mixes nginx, application, mail,
  syslog, audit (epoch seconds), API and cron stamps, each service with
  the formats it is tried with in `data/formats.txt`. Each line comes out
  in UTC with its local time and a flag for a wall time that does not
  exist or happens twice, then an hourly histogram, the span per
  service, and the whole window humanized.
- **meeting planner**: recurring meetings from `data/meetings.txt`, each
  shown in every attendee's zone for four weeks, with out-of-hours marks,
  across the March and November changes on three continents.
- **billing periods**: subscriptions from `data/subscriptions.txt` split
  into calendar months in the billing zone with `span_range`, prorated
  by the second, and the renewal dates from `range`.
- **activity feed**: events in sixteen locales grouped into upcoming,
  today, yesterday, this week and earlier, humanized in the reader's
  locale and dehumanized back.

`mini_arrow.kara` carries all of `data/` as hex, writes it to the
working directory, runs the jobs and removes it again.

## How the port works

- **Zones.** `read_tzfile` is `tzfile._read_tzfile`: the version 1 data
  only (32-bit transitions, no POSIX rule), the standard-time wall clock
  of each transition, and the `dstoffset` dateutil assigns to the shared
  `_ttinfo` objects, so the last assignment is the one that stays. As in
  dateutil, the last transition reads as standard time and so does every
  time after it, which is why New York is EST all through 2040.
  `is_ambiguous`, `_resolve_ambiguous_time` and `fromutc` follow fold the
  same way. A tzinfo is a `Tz`; dateutil's caches hand out one object per
  zone and per offset, so two equal `Tz` values stand for the same object.
- **datetime.** `DateTime` is Python's datetime: the constructor's checks,
  arithmetic in microseconds that resets fold, `astimezone`, subtraction
  and comparison on wall time within one tzinfo and on UTC across two,
  with PEP 495's exception for `==`, `timestamp()`, `fromtimestamp` with
  CPython's round-half-even split, `isoformat` and `isocalendar`.
- **relativedelta** carries its fields the way `_fix` does, clips the
  day to the month, then applies the time and the weekday.
- **The parser** builds arrow's patterns the way `_generate_pattern_re`
  does, out of `re.escape`, `re.sub` and `finditer` over the format, and
  compiles them with the port of Python's `re` from mini_parse, so named
  groups, the lookbehinds of the word boundary, `IGNORECASE` and the
  compile errors are the real ones. `_parse_token` and `_build_datetime`
  follow, with `strptime`'s `%G-%V-%u` and `%Y-%j` rules for week dates
  and days of the year.
- **The formatter** is `FORMAT_RE.sub` with the same expression, including
  its lookahead-and-backreference way of matching a bracketed literal.
- **Locales.** `gen_kara.py` reads the vendored `locales.py` with `ast`,
  resolves inheritance (and German's `timeframes_only_distance`, built
  with `.copy()` and item assignments) and writes `data/locales.txt`. What
  the classes override (ordinals, the Finnish, Slavic and Czech plural
  forms, Korean's special days and years, English's `instantly`) is keyed
  on the class name.
- **humanize and dehumanize** are line-for-line: the float thresholds,
  `round()` to even, the granularity lists with their float remainders,
  the `KeyError` turned into arrow's message, and dehumanize's regular
  expressions built from each locale's timeframes.

## Compiler gaps

One, filed as B-2026-10-24-32: `karac check` counts `p.a.unwrap_or(0)`
as a move when `a` is an `Option[i64]` field and `p` is an owned local, an
`own` parameter or a `mut ref` parameter, so a second use of the field
is refused, though `Option[i64]` is `Copy` (core-semantics §1.1). Through
a `ref` parameter, and on a whole `Option` local, it is accepted, and
the MIR interpreter runs the refused programs correctly. `karac fix`
inserted `.clone()` before three `unwrap_or` calls on `Parts` fields in
`build_datetime`. Thread A fixed the row in kara ea1fd651e8, and those
calls are back to the natural `parts.minute.unwrap_or(0)`. The other fixes `karac fix` applied were call-site
`mut` markers and two `.clone()`s on a match of a borrowed `Option[Tz]`
pair. With those, the first run printed the oracle's output under all
four backends, with nothing on stderr.

## Scope

The port covers what the jobs exercise. Not ported: `now()` and
`utcnow()` (the clock), `Arrow.strptime`, `struct_time`, `Decimal` and
pytz inputs, dateutil's `tzstr`, `tzrange` and `tzical` and the bundled
zoneinfo fallback in `gettz`, the 2.7 compatibility paths, and the locale
classes other than the 17 the jobs use (English, Italian, Spanish,
French, French Canadian, Japanese, Finnish, Chinese, Korean, Dutch,
Polish, Russian, German, Austrian, Portuguese, Brazilian Portuguese and
Czech); any other locale name is refused with arrow's message.

## Running

```
python3 -s -P -B mk.py                   # writes data/
python3 -I -B oracle.py > expected.out   # the oracle, on vendor/
python3 -I -B gen_kara.py                # writes data/locales.txt, refreshes inputs_text()
karac run mini_arrow.kara                # the port; prints the same
```

About 11 s on the MIR interpreter (a debug build of `karac`) and 8 s on
the LLVM backend, compile included; the built binary runs in 0.03 s.
