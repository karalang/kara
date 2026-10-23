# mini_holidays

A port of [holidays](https://pypi.org/project/holidays/) 0.62 (MIT; the
Vacanza team, dr-prodigy and ryanss): the library payroll, billing,
scheduling, SLA and trading systems use to ask whether a date is a working
day. A holidays object is a dict from dates to holiday names that fills
itself one calendar year at a time, the first time any date of that year
is asked for.

```
> getlist tx 2021-12-24
  ['Christmas Day (observed)', 'Christmas Eve']
> country rm US subdiv=TX years=2021
> popnamed rm Thanks
  [datetime.date(2021, 11, 25), datetime.date(2021, 11, 26)]
> popnamed rm Christmas
  error KeyError: datetime.date(2021, 12, 24)
```

## What it does

`mk.py` writes a script of 357 steps to `data/jobs.tsv`, two CSV files for
the programs, and ten gettext catalogs copied from the vendored package.
`oracle.py` replays the script against the vendored library and
`mini_holidays.kara` replays it against the port. A step that raises
prints `error <class>: <message>`; 21 do.

- **Seven calendars**: the United States (federal days, the unofficial
  category, and the CA, DC, GA, IL, MA, NY, TX and WA tables), the United
  Kingdom (its four nations and the 1977 to 2023 jubilees, weddings and
  funerals), Germany (all sixteen states and the Bavarian catholic
  category), France (its overseas departments and the old subdivision
  names), the Netherlands (with its optional category), the New York Stock
  Exchange (its 67 one-off closures from the 1888 blizzard on, the 1914
  closure and the 1968 paper crisis) and the ECB's TARGET2 calendar.
- **Observed days**: a holiday on a weekend is observed on a weekday by
  each calendar's rules (the US and NYSE move Saturday back and Sunday
  forward, the UK moves to the next free weekday, Texas moves Christmas
  Eve back a day), `observed=False` drops them, and the NYSE moves a
  holiday rather than adding one, so a Saturday New Year's Day vanishes.
- **Translations**: `language=` reads the library's own `.mo` catalogs, so
  German, French and Dutch names come out in English, Ukrainian or Thai.
- **The dict**: insertion order (a multi-year object fills itself in the
  order of its `years` set, so `{2026, 2023}` lists 2026 first), names
  merged with `"; "` in sorted order, `pop`, `pop_named` (including the
  `KeyError` it raises when one date matches twice), the six `get_named`
  lookups, slices with steps, `update` and `append`, `repr` and `str` of an
  empty object, `==`, and the working-day functions.
- **Years on demand**: a lookup in a new year computes it, even from inside
  another lookup; `expand=False` turns that off; setting `observed` or
  `categories` recomputes a filled object and does nothing to an empty one.
- **Sums and copies**: `us + de` merges calendars and keeps computing its
  operands' years into them; `copy()` shares its `years` set with the
  original, so a copy that computes a new year stops the original from
  computing it.
- **Five programs**: a payroll calendar that pays early when the 15th or
  the month end is not a working day, ticket due dates on each team's
  calendar, a holiday overlap planner for a team in five countries, an
  NYSE trading-day count per month, and an iCalendar export written to a
  file and read back.

## How the port works

- **The dict is a slot table.** Keys, values and a live flag in insertion
  order, with a `Map` from date to slot. A popped date leaves a dead slot,
  and setting it again appends, as a Python dict does.
- **Years are a CPython set.** Iteration order of the `years` set decides
  the order a multi-year object fills itself in, so the port keeps the set
  as CPython lays it out: the same table size, linear probes and
  perturbation, resize rule and merge for `a | b`.
- **Every lookup goes through `__keytransform__`.** The library computes a
  year from inside `get_list()` and `in` while it is computing another
  object's year for a sum, so the port routes every lookup through the same
  expansion step rather than computing years up front.
- **Dates are proleptic Gregorian ordinals**, so weekdays, `nth weekday of
  month` and Easter (dateutil's western method) are integer arithmetic.
- **The special-holiday tables are generated.** `gen_kara.py` reads the
  vendored `*StaticHolidays` classes and embeds their rows, so the 81
  one-off days are the library's own data.

## Compiler gaps

None. `karac check` asked for `.clone()` on eight borrowed `for` elements and
for `ref` on six bindings of a `Vec` element; both are language rules. The
port matched on its first run.

## Scope

- Seven of the library's 156 countries and 5 markets. Of the 56 US
  subdivisions the eight above have their state tables; the others are
  accepted, as the library accepts them, but get only the federal days.
- Dates given as strings are read in the forms the script uses (`Y-M-D`,
  `Y/M/D`, `M/D/Y`, `YYYYMMDD`, `July 4, 2024`, `4 Jul 2024`), not every
  form `dateutil.parser.parse` reads.
- The library is vendored without the translation catalogs of the
  countries it does not cover.

## Running

```bash
karac run mini_holidays.kara                   # interpret
karac build --mir mini_holidays.kara           # native binary
python3 -s -P -B mk.py                         # regenerate data/
python3 -I -B oracle.py > expected.out         # regenerate expected.out
python3 -I -B gen_kara.py                      # regenerate the embedded inputs
```
