# mini_astral

A port of [astral](https://pypi.org/project/astral/) 3.2 (Apache-2.0, Simon
Kennedy): the Python library for the times of the sun and moon, used by Home
Assistant and many photography and prayer-time tools. London on the longest
day of 2016:

```
sun => {dawn: 2016-06-21 02:54:36.669513+00:00, sunrise: 2016-06-21 03:43:14.229587+00:00, noon: 2016-06-21 12:01:47+00:00, sunset: 2016-06-21 20:20:32.113024+00:00, dusk: 2016-06-21 21:09:09.222087+00:00}
moonrise => 2016-06-21 20:49:00+00:00
```

## What it does

`mk.py` writes `data/jobs.tsv`, one job per line. `oracle.py` replays each
job through the vendored library and `mini_astral.kara` through the port.
Each job prints a header naming the call, then one line per result:
datetimes and times as `str()`, floats as `repr()`, and an exception as
`ValueError: <repr of the message>`.

The 795 jobs:

- **A day at a place** (226 jobs): `dawn` and `dusk` (civil, and with the
  job's depression, a `Depression` or a number), `sunrise`, `noon`,
  `midnight`, `sunset`, `daylight`, `night`, `twilight`, `golden_hour` and
  `blue_hour` in both directions, `rahukaalam` by day and by night, `sun()`,
  a few `time_at_elevation` calls (elevations above 90° turn into a setting
  sun), `moonrise`, `moonset` and the moon's `phase`. The places include
  the library's test cities, the geocoder's `51°31'N` form, an observer
  above sea level and one behind an obscuring feature, latitudes past 89.8°
  (which the library clips), coordinates past ±90/±180 (which it limits),
  and polar days and nights in both hemispheres, where the sun never rises,
  never sets, or never gets far enough below the horizon, each with its own
  message. Fixed UTC offsets from −12:00 to +14:00 push events onto the
  next or previous local day, which is where the library retries the
  neighbouring date, and some days have no moonrise at all (`None`).
- **An instant at a place**: the sun's zenith, azimuth and elevation with
  and without refraction, and the moon's azimuth, elevation and zenith, for
  naive and offset datetimes and at the poles.
- **The geocoder**: `lookup` of cities and groups, case-insensitive, with a
  region after a comma, quoted names, and the misses (`London, England`
  with its space after the comma is one); `group` by time zone region; and
  the sun times at what was found.
- **The helpers**: `dms_to_float` (each optional part of its pattern, the
  `′`/`″` marks, lower-case directions, and the strings that `float()`
  takes first, such as `1e3`, `inf` and `1_000`), `hours_to_time`,
  `time_to_hours`, `refraction_at_zenith`, `julianday` in both calendars,
  `julianday_modified`, `julianday_to_datetime` (including the years
  `datetime` refuses), `day_fraction_to_time`, `minutes_to_timedelta`,
  `gmst`, `lmst` and the moon's position.

## How the port works

- **Each function is ported statement for statement**, keeping the order of
  every float operation, Python's float `%` (`fmod` moved onto the divisor's
  sign) and `int()` truncation.
- **Python's `datetime` is ported for fixed UTC offsets**: a datetime is its
  wall-clock fields plus an offset, `timedelta` and date arithmetic are exact
  microseconds, and the constructors check their ranges with Python's
  messages. `str()` of a datetime, time and timedelta is ported too.
- **`math.acos`, `asin` and `sqrt` raise "math domain error"** outside their
  domains, as in Python. The sun functions catch exactly that error to say
  the sun never reaches an angle; `twilight`, `golden_hour` and `blue_hour`
  let it through.
- **The library's quirks come along**: `night` asks for 6° below the horizon
  even though its docstring says astronomical, `rahukaalam` divides only the
  `seconds` part of the day's length, the moon's azimuth and elevation read
  a datetime's wall clock whatever its time zone, and `moonrise` keeps the
  first day's answer when the neighbouring day has no rise.
- **`gen_kara.py` writes three functions** at the end of the `.kara` file:
  the moon's series terms read from `table4.py` with Python's `ast` module,
  the geocoder's location list from `geocoder.py`, and the jobs.

## Compiler gaps

None were hit.

## Scope

- **Fixed UTC offsets only.** Named time zones need `zoneinfo` and the tz
  database, so the jobs give `tzinfo` as UTC or a `datetime.timezone` with
  a fixed offset. The geocoder's time zone names are printed but not used.
- **Left out:** the `Location` class in `location.py` (which wraps these
  functions with the location's named time zone), `astral.now()` and
  `today()`, and the command line in `__main__.py`.
- **The geocoder lower-cases names with ASCII rules**; its database is
  ASCII apart from the `°` sign.

The vendored library is unchanged.

## Running

```bash
karac run mini_astral.kara                 # interpret
karac build --mir mini_astral.kara         # native binary
python3 -s -P -B mk.py                     # regenerate data/jobs.tsv
python3 -I -B oracle.py > expected.out     # regenerate expected.out
python3 -I -B gen_kara.py                  # regenerate the generated functions
```
