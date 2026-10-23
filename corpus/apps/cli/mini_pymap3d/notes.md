# mini_pymap3d

A port of [pymap3d](https://pypi.org/project/pymap3d/) 3.2.0 (BSD-2,
SciVision, Felipe Geremia Nievinski and Michael Kleder): the coordinate
conversions that radar, GNSS, drone and satellite-tracking code in Python
leans on. A target 1000 m out on an azimuth of 33 degrees and 70 degrees up
from a point near Detroit is:

```
geodetic2aer(42.002582, -81.997752, 1139.7018, 42.0, -82.0, 200.0) => (32.99927414904914, 69.99996788897073, 1000.0002044008343)
```

## What it does

`mk.py` writes `data/jobs.tsv`, one call per line. `oracle.py` replays each
call through the vendored library and `mini_pymap3d.kara` through the port.
Each prints the `repr()` of what the call returns, or the exception it
raises:

```
ecef2geodetic(0.0, 0.0, 0.0) => (0.0, 0.0, -6378137.0)
latitude.geodetic2isometric(90.0) => inf
aer2enu(33.0, 70.0, -1.0) => ValueError: Slant range  [0, Infinity)
vincenty.track2(40.0, -80.0, 65.0, 148.0, npts=2) => ([40.0, 65.0], [-80.0, 148.0])
```

A bare argument is a float. `ell=<model>` picks one of the library's
built-in ellipsoids and `ell=<a>:<b>` builds one from its two axes;
`deg=False`, `method=`, `npts=` and `alt=` are the keyword arguments of the
same name.

The 1,035 jobs cover 84 functions:

- **Geodetic and ECEF**, both ways, on places from the poles to a
  geostationary satellite and a point 6,000 km underground, on 16 bodies
  from WGS84 to Io and Pluto, and on the ECEF axis and origin, where the
  inverse divides by zero and falls back to its special case.
- **Local frames**: ENU, NED, azimuth-elevation-range and
  down-range/cross-range, each from ECEF and from geodetic coordinates and
  back, plus the velocity-vector rotations.
- **n-vectors, geocentric-spherical coordinates and the polar/spherical
  helpers.**
- **The auxiliary latitudes** (isometric, conformal, rectifying, authalic,
  parametric, geocentric) both ways, including the poles, where the
  isometric latitude is infinite and the conformal one divides by zero.
- **Radii**: of curvature along the meridian, the prime vertical and the
  parallel, the geocentric radius and the six mean-sphere radii.
- **Rhumb lines**: meridian arcs, the loxodrome in both directions and the
  departure.
- **Vincenty's inverse and direct problems**, including the nearly antipodal
  pairs where the iteration gives up after 50 rounds (Wellington to
  Salamanca comes out at an azimuth of 179.99999999999915 degrees, where
  geographiclib gives 161.07), and `track2`.
- **The haversine separation and the line-of-sight intersection** with the
  ellipsoid, including a line of sight that misses it.
- **Every error the library raises**: its own checks (a slant range, ground
  distance or altitude below zero, a latitude past a pole, an unknown model,
  a polar axis longer than the equatorial one, an unknown radius method) and
  the ones Python raises on the way (division by zero, the square root of a
  negative number, `atanh(1)`, the sine of an infinity).

## How the port works

- **The pure-Python path.** Without numpy the library takes a branch in
  every function: assigning into a float raises `TypeError`, a missing numpy
  name raises `NameError`, and each `try` falls back to scalar code. The port
  takes the same branches, so `meanm` (which needs numpy's `array`) fails
  with the same `NameError`, and `track2` with more than two points hands a
  list to `vreckon`, whose first comparison raises `TypeError`.
- **Exceptions are values.** Each function returns a `Result`, and every
  operation that can raise in Python is a helper that returns the same
  exception: `sqrt`, `asin`, `atanh` and `log` out of their domain, `sin`,
  `cos` and `tan` of an infinity, float `/` and `%` by zero, and a power
  that overflows. The library's own `except ArithmeticError` blocks catch
  exactly the division and overflow errors.
- **Three primitives come from CPython** and are ported: `math.hypot`
  (CPython's scaled sum with a compensated Dekker product, not the C
  library's), `math.radians` and `math.degrees` (a multiply by `pi/180` and
  `180/pi`) and the float `%` operator (`fmod`, then moved onto the
  divisor's sign).
- **Formulas keep Python's evaluation order**, and `x ** y` is `pow`.

## Compiler gaps

None were hit.

## Scope

- **Left out:** the ECI conversions, `azel2radec`/`radec2azel` and sidereal
  time, which take `datetime` values and use astropy when it is present.
- **Inputs that make a complex number** are left out. Python answers a
  negative number raised to a fractional power with one, which happens in
  `rsphere.rectifying` and `rsphere.triaxial("norm")` for a negative polar
  axis, and in `spherical2geodetic` within about 43 km of the centre.
- **Inputs so large that a square overflows** (above about 1e154) are left
  out too. Python raises `OverflowError` for those and the port would too,
  but no job checks it.
- **Without astropy.** The oracle asserts that astropy is not loaded, since
  `anglesep` switches to it when it is.
- Inputs are floats; the library also takes ints and numpy arrays.

The vendored library is unchanged.

## Running

```bash
karac run mini_pymap3d.kara                 # interpret
karac build --mir mini_pymap3d.kara         # native binary
python3 -s -P -B mk.py                      # regenerate data/jobs.tsv
python3 -I -B oracle.py > expected.out      # regenerate expected.out
python3 -s -P -B gen_kara.py                # regenerate the embedded jobs
```
