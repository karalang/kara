# mini_geopy

A port of [geopy](https://pypi.org/project/geopy/) 2.4.1 (MIT; the geopy
authors), the library most Python location code imports for its `Point`
type and its distances. It parses coordinates the way people write them,
formats them back as degrees, minutes and seconds, converts units, and
measures geodesic and great-circle distances and destinations.

```
> point 40°26′46″N 79°58′56″W
  repr Point(40.44611111111111, -79.98222222222222, 0.0)
  str 40 26m 46s N, 79 58m 56s W
  unicode 40° 26′ 46″ N, 79° 58′ 56″ W
  decimal 40.44611111111111, -79.98222222222222
  with altitude 40 26m 46s N, 79 58m 56s W, 0.0km
  altitude 0.0km 0.0m 0.0mi 0.0ft 0.0nm 0.0nmi
  items 40.44611111111111 -79.98222222222222 0.0
```

## What it does

`mk.py` writes 331 jobs (and 7 section comments) to `data/jobs.tsv`, the
library's own `POINT_PATTERN` to `data/point_pattern.txt`, and three input
files: a route of waypoints written in mixed notations, a table of 30
cities (one with a place name instead of coordinates), and ten delivery
stops, two with altitudes. `oracle.py` runs each job against the vendored
library and prints the result, or `error <class>: <message>` when it
raises; `mini_geopy.kara` runs the same jobs. 33 jobs raise.

- **`point`** parses a string with `Point()` and prints it every way geopy
  formats a point: `repr`, `str`, `format_unicode`, `format_decimal`,
  `format(altitude=True)`, `format_altitude` in all six units, and the
  three items. The strings cover decimal and degree-minute-second forms,
  `N/S/E/W` before or after the number, the `°′″`, `'"`, `D` and `*`
  symbols, altitudes with every unit, separators, prefixes, out-of-range
  and wrapping longitudes, and strings that do not parse.
- **`make`**, **`seq`** and **`lonlat`** build points from numbers,
  strings, `None` and sequences, including the single-number error, the
  latitude range check, non-finite values and longitude normalization.
- **`degrees`**, **`parse_degrees`**, **`altitude`** and **`units`** call
  `format_degrees` with each symbol set, `Point.parse_degrees`,
  `Point.parse_altitude` and the unit conversion functions with their
  aliases and keyword errors.
- **`dist`** measures with `distance`, `geodesic`, `great_circle` or the
  abstract `Distance`, over two or more points, on each named ellipsoid or
  a custom one, with a custom radius, a plain number, or unit keywords,
  and prints the distance in each unit.
- **`dest`** computes a destination point from a start, a bearing and a
  distance, with the `distance=` override.
- **`arith`** does `Distance` arithmetic (add, subtract, negate, multiply,
  divide, floor divide, abs), truth and comparisons, and prints which
  class the result is and its model.
- **`route`**, **`nearest`** and **`fence`** are small programs over the
  input files: each leg of a route and its total, the cities nearest a
  place (skipping one that does not parse), and which stops fall inside a
  radius (reporting the ones whose altitude makes the distance
  unsupported). A missing file raises `OSError`.

## How the port works

- **POINT_PATTERN runs on the mini_re engine.** geopy parses a point with
  one large verbose, Unicode regular expression and reads its named
  groups, so the port compiles the library's own pattern text with the
  regular-expression engine from [mini_re](../mini_re) (Python `re`
  semantics: Unicode `\s` and `\d`, lazy repeats, backtracking) and reads
  the same groups. `''` is replaced with `"` first, as the library does.
- **geodesic runs on the mini_geographiclib port.** geopy's `geodesic`
  calls geographiclib, so its `Inverse` and `Direct` come from
  [mini_geographiclib](../mini_geographiclib), which transcribes that
  library's evaluation order float for float.
- **`%g` is done by hand.** `format_degrees` prints the arcseconds with
  `%g`, which Kāra's format specs do not offer (they take `x X o b d` and a
  precision). The port rounds the float's exact binary value to six
  significant digits half to even, with the arbitrary-precision Decimal
  from [mini_zxcvbn](../mini_zxcvbn), and picks fixed or exponent notation
  by the exponent as C's `printf` does.
- **Python's numbers are followed exactly.** `float()` accepts Python's
  grammar (spaces, `_` between digits, `inf`, `nan`), `float(x or 0.0)`
  turns `None`, `""` and `-0.0` into `0.0`, longitudes wrap with `fmod`,
  floor division follows CPython's `float_floor_div`, and every result
  prints with Python's `repr`.
- **Distance results are built afresh.** As in the library, an
  arithmetic result is `self.__class__(km)`, so it carries the class's
  default radius or ellipsoid rather than the operand's, and the output
  shows which.

## Compiler gaps

None new. `karac check` asked for one `.into_iter()` on a `for` loop that
moved its elements, a language rule. After that the interpreter matched
the oracle byte for byte on its first run, and the LLVM, ASan and native
builds matched too. The reused geographiclib code keeps its two
workarounds (B-2026-10-23-3 and B-2026-10-23-4), described in that port.

## Scope

- The geocoders (Nominatim, Google and the rest) and the rate limiter are
  not ported; they call web services. The vendored copy keeps them only
  so the package imports.
- `Point` pickling, hashing and `Point.from_point` are not exercised
  separately; points passed between the programs' steps are rebuilt from
  their coordinates, which gives the same values.
- `lonlat` is only called with numbers.

## Running

```bash
karac run mini_geopy.kara                  # interpret
karac build --mir mini_geopy.kara          # native binary
python3 -s -P -B mk.py                     # regenerate data/
python3 -I -B oracle.py > expected.out     # regenerate expected.out
python3 -I -B gen_kara.py                  # regenerate the embedded data
```
