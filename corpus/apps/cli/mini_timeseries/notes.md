# mini_timeseries

A Kāra port of the time-series half of [pandas](https://pandas.pydata.org/)
3.0.5 (BSD-3-Clause): `to_datetime`, datetime and timedelta columns, the
`.dt` accessor, offset aliases, `date_range`, `resample`, `asfreq`,
time-based `rolling`, and `tz_localize` / `tz_convert`, each computed the
way pandas computes it, so that every bin edge, label and timestamp prints
as pandas prints it. It shares its frame, `read_csv` and aggregation code
with [mini_pandas](../mini_pandas/README.md), and runs pipelines in the
same job language, one step per ` | `:

```
read metrics.csv dates=ts | resample ts 6h cpu:mean closed=right label=right by=host
read sales.csv dates=date | resample date W-MON amount:sum by=store | head 8
read metrics.csv dates=ts | tz ts convert America/New_York | resample ts D req:sum,cpu:count
```

| Group | Steps (the pandas call each one is) |
|---|---|
| Parsing | `read ... dates=a,b` (`to_datetime` of ISO strings, with or without `Z`, of `format=` through strptime, of `unit=s`, and `errors="coerce"`), `range` (`date_range` with `tz`) |
| Date parts | `dt` (`year`, `month`, `day`, `hour`, `minute`, `second`, `microsecond`, `dayofweek`, `dayofyear`, `quarter`, `week` from `isocalendar`, `day_name`, `month_name`, `is_month_start`, `is_month_end`) |
| Rounding and formatting | `floor`, `ceil`, `dtround` (`.dt.round`, half to even), `normalize`, `strftime` |
| Filtering | `where` against a timestamp string (read in the column's zone unless it carries an offset) or a duration, `between_time` (wrapping past midnight), `isna` |
| Durations | `sub` (timestamp minus timestamp), `tdiff` (`.diff()`), `secs` (`.dt.total_seconds()`) |
| Resampling | `resample` with any aggregate mini_pandas has, `by=` (`groupby(...).resample`), `closed=`, `label=`, `origin=epoch`, `offset=`; `ohlc` |
| Regular grids | `asfreq` with `ffill` and `bfill` |
| Time windows | `trolling` (`rolling("2min", on=...)`: `sum`, `mean`, `min`, `max`, `count`) |
| Time zones | `tz ... localize`, `tz ... convert` |

The offset aliases are `s`, `min`, `h`, `D`, `W` and `W-MON`..`W-SUN`, `ME`,
`MS`, `QE`, `QS`, `YE` and `SME`, each with a multiple (`15min`, `6h`).
Timestamps are kept as microseconds since the epoch in UTC, with the unit
and the zone as column metadata, so a column prints its dtype the way
pandas does (`datetime64[us]`, `datetime64[s]`, `datetime64[us, UTC]`,
`datetime64[us, America/New_York]`, `timedelta64[us]`).

## Running it

The program carries its inputs: it writes `data/` (host metrics in UTC
across the night US clocks go forward, with lost samples and missing CPU
figures; millisecond trades on two symbols; a shop's daily takings in a
leap year with a lost day; support tickets stamped in Unix seconds, open
ones with no close stamp; a US-style `month/day/year` export with two cells
that are not dates; and irregular meter readings) and runs
`data/jobs.txt`, 101 jobs. Each job prints `$ <job>`, then the result as
`to_csv(index=False)` writes it, or `! ` and the exception pandas raised.

```
python3 -s -P -B mk.py          # writes data/ and data/jobs.txt
python3 -I -B gen_kara.py       # copies data/ into the end of mini_timeseries.kara
python3 -I -B oracle.py > expected.out
karac run mini_timeseries.kara | diff - expected.out
```

`oracle.py` is the oracle, and it is pandas itself, with the same
requirements as mini_pandas' (pandas 3.0.5 on numpy, without bottleneck or
pyarrow).

## Parallel work

Every job runs in its own `TaskGroup` task, and inside a job each group of
a grouped `resample` is binned and aggregated in a task of its own; the
results join in key order, so the transcript is the same on every run and
thread count. The program matches `expected.out` on `karac run`,
`__mir-llvm-run`, `__mir-llvm-asan` (no reports) and a `karac build`
binary, at `KARAC_THREADS=1` and `4`, and five runs of an auto-par build at
`KARAC_THREADS=4`.

## Matching pandas' bins

`resample` is where a port goes wrong, because pandas bins in two different
ways:

- **A fixed-length frequency** (`s`, `min`, `h`) bins in absolute time. The
  first edge comes from `_adjust_dates_anchored`: the origin is local
  midnight of the first value (`origin="start_day"`) or the epoch, moved by
  `offset`, and the first and last edges step outward from the data by
  whole bins, differently for `closed="left"` and `closed="right"`.
- **A calendar frequency** (`D`, which pandas 3 no longer treats as a fixed
  length, and every anchored offset) bins in wall-clock time: normalize,
  roll back to the offset, `date_range` in local time, then localize. A day
  in New York across the spring change is 23 hours long, and its bin is.
- **The end-anchored offsets** `W`, `ME`, `QE` and `YE` default to
  `closed="right"` and `label="right"`, and their edges are moved to the
  last microsecond of the day, so a value at noon on the 31st falls in the
  month that ends that day.
- An empty bin sums to 0, counts 0, and is `NaN` for every other aggregate,
  which makes an int `min` or `max` column float. A grouped resample bins
  each group over its own range, so two hosts' tables start at different
  edges.

## pandas' own quirks, kept

- **How a timestamp column is written.** A naive column writes only dates
  when every value is midnight, and otherwise the fraction digits of its
  finest value (none, 3 or 6) on every row. A zoned column writes each value
  as `str(Timestamp)` does, with the offset, and a fraction only on the rows
  that have one. A missing value is written empty.
- **How a duration is written.** `0 days 01:30:00`, or `3 days` when every
  value is whole days, and `-1 days +23:00:00` for a negative one.
- **Parsing.** A string gives microseconds, `unit="s"` gives seconds, and a
  trailing `Z` makes the column UTC. A value that does not match `format=`
  raises the four-line `ValueError` pandas raises, naming the value and the
  format; with `coerce` it is `NaT`.
- **Comparing with a string.** `ts >= '2026-03-08 00:40'` on a zoned column
  reads the string in that zone; a string with its own offset keeps it.
- **Errors.** An unknown frequency, a `resample` on a column that is not a
  timestamp, `.dt` on a number, a missing date column and `tz_localize` of a
  column that already has a zone each raise pandas' exception and message.

## Where it differs from pandas, on purpose

- **No index.** As in mini_pandas, results are written with `index=False`;
  `resample` and `asfreq` take the timestamp column by name (`on=`), and
  their result has it as a column.
- **Two time zones.** `UTC` and `America/New_York`, whose rules since 2007
  are written out in the program (the second Sunday of March at 2:00, the
  first Sunday of November at 2:00). A port of a real job needs the tz
  database.
- **A job language instead of Python**, with `_` for a space inside a quoted
  literal or a format.

## The Mend loop

The first `karac check` reported 12 errors: ten `*` dereferences of `Copy`
values (a microsecond count, a `Parts` record) that borrow-by-default made
unneeded, a match on a column that moved its payload out (`karac fix` bound
it `ref`), and a captured zone name moved into each iteration of a loop
(`karac fix` cloned it). `karac fix` applied all 12, and the first
interpreter run matched pandas on every job. No compiler gap was found.
`mini_timeseries.orig.kara` is the program as first written.

## What a time-series API would need (notes for M4c)

Compared with `docs/library/data.md` and `docs/library/time-random-env.md`
in the kara repo:

- **A timestamp element type (Decision Q1).** Every job here starts by
  turning a string column into timestamps, and the release that reads them
  as `String` cannot run any of them. The representation that worked is
  Arrow's: an `i64` count of a unit since the epoch, with the unit and an
  optional zone in the column's type, not in each value. A `Duration`
  column (Arrow Duration) comes with it, since subtracting two timestamp
  columns makes one.
- **Parsing and formatting.** `to_datetime` with ISO 8601 (offsets and `Z`),
  a strptime format, epoch units and a coerce mode, and `strftime` on the
  way out. The core library has neither: `std.time` has `Instant`,
  `SystemTime` and `Duration` but no calendar, so this program carries its
  own civil-date arithmetic, ISO weeks and month lengths.
- **Time zones.** Localize and convert need the tz database, and a policy
  for the local times a change skips or repeats (pandas' `nonexistent` and
  `ambiguous`). Nothing in the library or the runtime reads zone data yet.
- **Calendar offsets.** `ME`, `MS`, `QE`, `W-MON` and the rest, with their
  `rollforward` / `rollback`, are what monthly and weekly reports are
  written in, and a fixed `Duration` cannot express them: a month is not a
  length of time, and in a zone with summer time neither is a day.
- **Resample and group-by-time.** Polars' `group_by_dynamic(every, period,
  offset, closed, label, by)` is the shape. The page should say which
  origin it uses: pandas' default (`start_day`) and the epoch put bins in
  different places for any frequency that does not divide a day.
- **Upsampling.** `asfreq` and `date_range` onto a regular grid, then
  `ffill` / `bfill`, which `data.md` also lacks for plain columns.
- **Time windows.** `rolling("2min", on="ts")` is a variable-width window
  over the half-open interval `(t - w, t]`, unlike the fixed row-count
  windows mini_pandas needed; it wants the same compensated sums.
- **Date parts.** `year` ... `dayofweek`, `quarter`, the ISO week,
  `day_name`, `is_month_end`, `floor` / `ceil` / `round` to a frequency, and
  `between_time` on time of day, as one namespace of column methods
  (pandas' `.dt`, Polars' `.dt`).
