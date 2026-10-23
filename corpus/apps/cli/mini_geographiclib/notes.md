# mini_geographiclib

A port of [geographiclib](https://pypi.org/project/geographiclib/) 2.1 (MIT,
Charles Karney): the geodesic library behind pyproj's `Geod`, geopy's
`geodesic` distance and most GIS code that needs distances, azimuths and
polygon areas on the ellipsoid. Wellington to Salamanca on WGS84 is
19959679.267 m, leaving on an azimuth of 161.07 degrees.

## What it does

`mk.py` writes `data/jobs.tsv`, one job per line. `oracle.py` replays each job
through the vendored library and `mini_geographiclib.kara` through the port.
A single call prints the `repr()` of what it returns, or the exception:

```
inv(6378137.0,0.0033528106647474805, -41.32, 174.81, 40.96, -5.5, ALL) => {'lat1': -41.32, 'lon1': 174.81, 'lat2': 40.96, 'lon2': -5.5, 'a12': 179.6197069334283, 's12': 19959679.26735382, 'azi1': 161.06766998616015, 'azi2': 18.825195123247063, 'm12': 76442.05326595774, 'M12': -0.9950033037161168, 'M21': -1.0048870439736906, 'S12': -100704891846320.48}
geod(6400000.0, 3.0) => ValueError: math domain error
```

The first field after the op is the ellipsoid as `a,f`; the last is the output
mask. A `line`, `poly` or `acc` job builds a `GeodesicLine`, a `PolygonArea`
or an `Accumulator` and prints one indented line per call made on it:

```
poly(6378137.0,0.0033528106647474805, False):
  AddPoint(89.0, 0.0)
  ...
  Compute(False, True) => (4, 631819.8745280146, 24952305678.0)
```

The 306 jobs cover:

- **The constructor** on 9 ellipsoids and in each of its failures (a radius
  that is negative, zero, NaN or infinite; a flattening that makes the polar
  axis non-positive, divides by zero at `f = 1` and `f = 2`, or takes the
  square root of a negative number at `f = 3`).
- **Inverse, Direct and ArcDirect**: the library's twenty WGS84 reference
  geodesics in all three forms, its GeodSolve regression cases (nearly
  antipodal points, the equator, meridians, poles, coincident points, NaN and
  infinite inputs, `LONG_UNROLL`), places on a sphere, on oblate and prolate
  ellipsoids and on the extreme `f = -3` and `f = 0.99`, and each output mask
  on its own.
- **GeodesicLine**: all four constructors, `Position`, `ArcPosition`,
  `SetDistance`, `SetArc`, and a line built without the capability a later
  call needs.
- **PolygonArea**: the library's Planimeter cases (polygons around a pole,
  across the antimeridian, wound three times, with `TestPoint` in each
  reverse/sign combination and `TestEdge`), polylines, `AddEdge`, `Clear`, empty and
  one-point areas, and three other ellipsoids.
- **Accumulator**: the running error-free sum on values that cancel.

## How the port works

- **The series, the starting guess and the Newton iteration** are
  transcribed in the library's evaluation order, so every intermediate float
  rounds the same way and the iteration takes the same steps.
- **Four primitives come from CPython, not the C library**, and are ported:
  `math.hypot` (CPython's own scaled sum with a compensated Dekker product,
  which differs from glibc's `hypot` in the last bit for about one input in
  300), `math.remainder` (CPython's fmod-based `m_remainder`),
  `math.radians` and `math.degrees` (a multiply by `pi/180` and `180/pi`) and
  `round()` (half to even). `sin`, `cos`, `atan2`, `atanh`, `sqrt`, `fmod` and
  `pow` are libm on both sides.
- **`max(a, b)` and `min(a, b)`** keep `a` unless `b` is strictly beyond it,
  which decides what happens to a NaN, as in Python.
- **The constructor's exceptions** are the ones Python raises along the way:
  a zero divisor is a `ZeroDivisionError` and a negative square root a
  `ValueError`, checked at the step that would raise them.

A restricted-op prototype used during development made these substitutions
and agreed with the library on 548,524 checks over random points, ellipsoids
and masks.

## Compiler gaps

Two gaps are worked around and listed in the source header:

- **B-2026-10-23-3:** a method call on a `const` (`TOL0.sqrt()`) passes
  `karac check` and runs under `--interp`, but v2 lowers it as a call of
  `f64.sqrt` and fails. `tiny()` and `tol2()` copy the const into a local
  first.
- **B-2026-10-23-4:** when a float lies exactly halfway between two equally
  short decimals, Kāra prints the one farther from zero and Python the one
  with the even last digit (`-75455689800034.625` is `...34.63` in Kāra and
  `...34.62` in Python). One polygon area in the set hits this. `fmt()` reads
  the exact expansion (`{x:.60}`) and keeps the even candidate; it matched
  Python's `repr` on 5,847 floats, 142 of them ties. Only plain decimals are
  checked, since no output here ties in exponent form.

## Scope

- The port covers the whole package: `Geodesic`, `GeodesicLine`,
  `PolygonArea` and `Accumulator`.
- Inputs are floats; the library also accepts Python ints, which leak through
  into the returned dictionaries as ints.

The vendored library imports only the standard library; `vendor/` is otherwise
left untouched.

## Running

```bash
karac run mini_geographiclib.kara                 # interpret
karac build --mir mini_geographiclib.kara         # native binary
python3 -s -P -B mk.py                            # regenerate data/jobs.tsv
python3 -I -B oracle.py > expected.out            # regenerate expected.out
python3 -s -P -B gen_kara.py                      # regenerate the embedded jobs
```
