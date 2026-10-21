# mini_schedule

In-process job scheduling as schedule 1.2.2 does it ("every 10 minutes
do this"). schedule is the Python library behind many small daemons,
bots, cron replacements and scrapers. The port is schedule as it runs
without pytz, so `at()` has no time zones. It covers:
- the builder: `every(n)`, the unit properties and their
  IntervalErrors, the days of the week, `at()` with the daily, hourly
  and minutely time formats, `to()` with random intervals from MT19937,
  `until()` with datetimes, timedeltas, times and strings, `tag()` and
  `do()`;
- `until()`'s strings, read through the regexes `_strptime` builds for
  its five formats, with their alternation order, `\s+`, digits of any
  script and the dates `datetime` refuses;
- when a job runs next, from `_schedule_next_run`'s arithmetic on naive
  datetimes, with `timedelta`'s rounding for float intervals;
- the Scheduler: `run_pending` in next-run order, `run_all` with its
  delay, `get_jobs`, `clear` and `cancel_job` (by tag or not),
  `next_run` and `idle_seconds`, CancelJob, and the jobs' `__str__` and
  `__repr__` with their debug log lines;
- two programs:
  - an ops daemon. It reads its tasks from a config file and sleeps for
    `idle_seconds()` between runs. Failures go through the docs'
    `catch_exceptions` decorator. It runs for two simulated days, writes
    `data/out/daemon.log` and summarizes it;
  - a health monitor. It checks a service every 30 seconds and retries
    with exponential backoff through one-off jobs that cancel
    themselves, then clears them by tag when the service recovers.

The run writes `data/` and then works through the 21 jobs in
`data/jobs.jsonl`, 348 operations in all. `mk.py` wrote `data/`.

`oracle.py` runs the same jobs through schedule itself and wrote
`expected.out`. It uses `schedule/__init__.py` from the 1.2.2 wheel,
unchanged. It makes `import pytz` fail as it would on a machine without
pytz. The clock is fake: `datetime.datetime.now()` reads it, and
`time.sleep()` and each tick of a job move it on.

The quirks are kept. Some examples:
- `every(2).days.at("10:30")` skips today even when 10:30 is still
  ahead, because a non-1 interval is added before the catch-up loop.
- `every(1.0).minute` and `every(True).hour` pass the "use minutes"
  check, and `__repr__` prints "Every True hour".
- `every(3)` with no unit prints "Every 3 None do [None]", while
  `every()` with no unit raises TypeError in `__repr__`.
- "24:00" and "29:59:59" pass the daily regex and then fail with
  "Invalid number of hours ({} is not between 0 and 23)", braces and all.
- `$` matches before a final newline, so `at("07:30\n")` is 07:30.
  "1０:3０" with fullwidth digits is 10:30, since `\d` and `int()` take
  any decimal digit, but "[0-2]" takes only ASCII.
- An hourly `at(":30")` runs at minute 30, but `at("15:30")` runs at
  15 minutes 30 seconds past.
- `to()` with a float interval fails with TypeError from `randrange`,
  and `to()` below the interval raises "`latest` is greater than
  `interval`".
- `until("2026-10-23   18:00")` passes (`\s+`), as does a fullwidth
  year, but `"2026-10-23 18:00:61"` and a trailing space do not.
- A job that raises stops `run_pending` there. It is not rescheduled,
  so it is due again at the next tick, ahead of the jobs that were
  never reached.
- `get_jobs(True)` finds jobs tagged `1` and `1.0`, since tags are a
  set, and `clear(1.0)` clears all three.
- A job that clears the scheduler while `run_pending` runs leaves the
  jobs already picked to run; their CancelJob then logs "Cancelling
  not-scheduled job".

Not ported: `at(..., tz)`, which needs pytz, and `repeat()`, which is
`do()` as a decorator.

## License

schedule is under the MIT license in `LICENSE-schedule.txt`:

    Copyright (c) 2013 Daniel Bader (http://dbader.org)

That covers `schedule/__init__.py`.
