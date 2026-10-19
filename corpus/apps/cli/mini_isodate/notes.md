# mini_isodate

[isodate](https://github.com/gweis/isodate) 0.7.2, the ISO 8601 parser
and formatter that many Python libraries use for dates, times and
durations, with a subscription renewal report and an incident report
built on it.

The port follows isodate's own code:
- `parse_date` with every pattern isodate builds for a year width:
  complete, week, ordinal, month, year and century dates, expanded years
  with a sign, and the default month and day;
- `parse_time` with decimal fractions on the hour, minute or second
  (through `Decimal`, as isodate does it) and the `Z`, `+hh:mm`, `+hhmm`
  and `+hh` designators, `parse_tzinfo` and `parse_datetime`;
- `parse_duration` in both forms, the designator form and the
  alternative `PYYYY-MM-DDThh:mm:ss` form, returning a `timedelta` or a
  `Duration`;
- the `Duration` class: years and months kept apart from the rest,
  `+`, `-` and `*` with durations, timedeltas, dates and datetimes
  (month ends clamp, so 31 January plus a month is 29 February in 2024),
  equality and `totimedelta`;
- isodate's `strftime` for dates, times, datetimes and durations, with
  the predefined formats.

Underneath are the parts of Python it relies on: `date`, `time`,
`datetime` and `timedelta` with their errors, the timedelta constructor
as CPython's C code computes it from floats, the `Decimal`s isodate
makes, and the regular-expression engine of `mini_re`.

The run writes `data/jobs.tsv`, the operations, and the two report
inputs. The jobs are:
- `dates`, `times`, `zones` and `datetimes`: parsing and formatting;
- `durations`: 67 duration strings parsed both ways, and 12 formatted
  with seven formats;
- `arithmetic`: durations added to and taken from dates and datetimes,
  multiplied, negated, compared and turned into timedeltas;
- `subscriptions`: renewal dates for twelve subscriptions from
  `data/subscriptions.tsv`, chained and by multiples of the period, with
  grace periods;
- `incidents`: outage lengths from `data/incidents.tsv` against their
  budgets, written in mixed ISO forms and zones, with totals per service.

The output matches `oracle.py`, which runs the same jobs through
isodate. The Python files are vendored unchanged in `isodate/`
(BSD-3-Clause, see `LICENSE-isodate`). Run
`python3 mk.py && python3 oracle.py` to regenerate `expected.out`.
