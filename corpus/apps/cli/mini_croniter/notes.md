# mini_croniter

Cron schedules as croniter 6.2.4 reads them and steps through them.
Many Python schedulers, task queues and job runners use croniter to find
when a job runs next. The port covers:
- The expression parser:
  - 5, 6 and 7 fields, with seconds sixth or first;
  - month and weekday names, ranges, steps, and ranges that wrap
    around;
  - `L` for the last day, `W` for the nearest weekday, `#` and `L` for
    the nth weekday of a month, and `?`;
  - the `@` aliases;
  - Jenkins-style `H` fields, hashed from a job id with CRC-32;
  - `strict` checking for days that never occur.
- Stepping through a schedule:
  - `get_next`, `get_prev`, `get_current`, `all_next` and `all_prev`;
  - the options `day_or`, `implement_cron_bug`,
    `max_years_between_matches` and `expand_from_start_time`.
- Times:
  - naive, UTC, fixed-offset and `zoneinfo` times, with croniter's rules
    for wall times that are skipped or happen twice when the clocks
    change;
  - the zones are read from real TZif files with mini_zoneinfo's loader;
  - the float timestamps croniter works in, rounded as CPython rounds
    them.
- `match`, `match_range` and `croniter_range`.

The run writes `data/` and then works through the 40 jobs in
`data/jobs.jsonl`, 514 operations in all:
- plain schedules, ranges, steps and the special characters;
- `day_or`, seconds, years, and every error the parser raises;
- hashed and aliased schedules;
- float timestamps from before 1970 to 2200;
- fixed offsets;
- New York, London, Lord Howe (whose DST is 30 minutes) and Kolkata
  across their clock changes;
- Apia, which skipped 30 December 2011;
- `match`, `match_range` and `croniter_range`, including ranges that
  run backwards or across zones;
- 24 jobs of schedules drawn at random from a fixed seed.

`oracle.py` runs the same jobs through croniter itself and wrote
`expected.out`. It uses the Python files of the 6.2.4 wheel in
`croniter/`, unchanged. Next to them are the parts of python-dateutil
2.9.0 that croniter imports, in `dateutil/`, and `six.py` 1.17.0. `mk.py`
copies the five zones from `/usr/share/zoneinfo`, so it reproduces
`data/` exactly only with the same tzdata (2026c, Ubuntu 24.04).

The quirks are kept. Some examples:
- A range whose ends are equal, such as `5-5`, means the whole field.
- A step over a wrap-around range can skip a value at the wrap. For
  example, `50-10/7` gives minutes 50, 57 and then 5, not 4.
- `0` is read as `1` in the day field of a 6-field expression but not
  of a 5-field one. `7` is Sunday in 5 fields, but out of range in 7.
- `day_or` (the default) runs a job when either the day of the month or
  the weekday matches. But an nth weekday such as `fri#2` is still
  enforced on the day-of-month side. So `* * 4 * 5#3` never runs, since
  it would need the 4th to be the third Friday.
- With a `hash_id`, `@daily` becomes a 6-field schedule whose seconds
  are hashed too.
- A wall time that the clocks skip moves to the first minute after the
  gap. A wall time that happens twice is used once going forward and
  once going back, so `30 1 * * *` runs twice on the night the clocks go
  back.

Not ported:
- `R` (random) fields;
- pytz zones;
- a `start_time` argument to `get_next`;
- non-ASCII expressions. Python lowercases non-ASCII letters and reads
  non-ASCII digits, and the port does neither; the jobs use ASCII.

## License

croniter is under the MIT license in `LICENSE-croniter`:

    Copyright (C) 2010-2012 Matsumoto Taichi

python-dateutil is under the Apache 2.0 and BSD 3-Clause licenses in
`LICENSE-dateutil`, and six is under the MIT license in `LICENSE-six`.
Those cover the files in `croniter/` and `dateutil/` and `six.py`.
