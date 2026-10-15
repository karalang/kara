# cron_sched: a cron scheduler

This app reads a crontab file. It supports the five classic fields with lists,
ranges, steps, month and weekday names, weekday 7 as Sunday, and the
`@hourly`, `@daily`, `@weekly`, `@monthly` and `@yearly` macros. It also
follows Vixie cron's rule that when both the day-of-month and day-of-week
fields are restricted, a day matches if either one does. So `0 12 13 * fri`
fires every Friday as well as on the 13th. Bad lines are reported with
their line number and the field that is out of range.

Times are UTC seconds since the epoch. They are converted to and from civil
dates by hand, using Howard Hinnant's days-from-civil algorithm, which is
checked against fixed dates before 1970, on leap days and at century rules.
`next_fire` jumps month by month, day by day and hour by hour rather than
minute by minute. A spec that can never fire, such as Feb 30, gives up
after a horizon.

The scheduler keeps each job's last run in a state file. When it starts, it
counts the slots it missed while it was down. It runs each due job once
through `sh`, not once per missed slot, and records the exit code. Then it
prints every job's next fire time. A second start 70 minutes later shows the
state carried over.

Oracle: `cron_sched.py`. Its calendar math comes from Python's `datetime`
and `calendar`, which makes it independent of the hand-written date code.
The cron logic is a port. `expected.out` is its output.

## Compiler gaps (filed in kara `docs/bug-ledger.jsonl`)

- **B-2026-10-15-22:** the following code passes `karac check`, but every
  MIR surface stops with an internal error, "invalid MIR: assigning i64 to
  _8 of type ref i64":

  ```
  let since = m.get(k).unwrap_or(d);
  let mut t = since;
  t = f;
  ```

  The app writes `let since: i64 = ...`.
- Known and worked around: B-2026-10-12-6 (`parse` returns `Option`),
  B-2026-10-12-7 (`split` is an eager `Vec[String]`).

`cron_sched.orig.kara` is the first draft.

Reverted 2026-10-15, karac at aec4d15cc0: B-2026-10-15-22 is fixed, so
`since` in `start` is no longer annotated. Each of these is the original
line again, and the app still matches on all four MIR surfaces.
