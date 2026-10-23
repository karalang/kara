# mini_sgp4

A port of [sgp4](https://pypi.org/project/sgp4/) 2.27 (MIT, Brandon Rhodes):
the SGP4/SDP4 satellite propagator that Skyfield and most Python tools use to
turn a two-line element set (TLE) into a position and velocity. Vanguard 1,
a day and a half after its 2000 epoch:

```
sgp4_tsince(2160.0) => (0, (190.19796987850174, 7746.966536135271, 5110.006754118664), (-6.112325142013842, 1.5270081835204619, -0.13915235788255884))
```

## What it does

`mk.py` writes `data/jobs.tsv`, one job per line. `oracle.py` replays each
job through the vendored library and `mini_sgp4.kara` through the port. Each
job prints a header naming the call, then the `repr()` of what it returns, or
the exception it raises as `ValueError: <repr of the message>`.

The 543 jobs:

- **The library's own verification run.** `tests.py` checks the Python code
  by regenerating `tcppver.out`, Vallado's reference output for the 33
  satellites in `SGP4-VER.TLE`: positions and velocities, then the orbital
  elements from `rv2coe` and the calendar date from `invjday`, every few
  minutes over each satellite's test span, with the propagation errors the
  library expects (a decayed satellite, an eccentricity out of range, a
  negative semilatus rectum). The jobs print those lines for all 33 through
  `Satrec.sgp4_tsince`, and for every eighth satellite through
  `Satrec.sgp4(jd, fr)` and the 1.x API under each gravity model and
  operation mode.
- **Reading TLEs** with `Satrec.twoline2rv` and the 1.x `io.twoline2rv`,
  printing the satellite record (every attribute for the verification set)
  and a few propagated points. The special cases come from `tests.py` and
  beyond: Alpha-5 satellite numbers, leading spaces, a 7-character
  international designator, missing checksums, the "December 32nd" epoch
  that needs `invjday` to build its datetime, a January 0th, leap days, and
  each way a line can be rejected (a column out of place, a short line,
  mismatched satellite numbers, non-ASCII characters, letters where digits
  belong, an underscore, `inf` in a number).
- **`export_tle`** for every satellite, including its errors for an angle out
  of range.
- **`Satrec.sgp4init`** with the Vanguard elements under all three gravity
  models and both operation modes, the Alpha-5 limits (and 340000, which it
  refuses), a 1999 epoch, and 40 seeded random orbits, near-Earth and deep
  space, each propagated and exported.
- **The helpers**: both `jday` functions, `invjday`, `days2mdhms`, `gstime`,
  `newtonnu` (elliptic, parabolic and hyperbolic), `rv2coe` (every orbit
  type, circular and elliptic, inclined and equatorial, and a zero position), the checksum
  routines, `to_alpha5`, `from_alpha5` and `getgravconst`.

## How the port works

- **The propagation core is translated mechanically.** `translate.py` reads
  `vendor/sgp4/propagation.py` with Python's `ast` module and writes the
  Kāra for `sgp4init`, `sgp4`, the deep-space routines (`_dscom`, `_dpper`,
  `_dsinit`, `_dspace`), `_initl` and `gstime` between two markers in
  `mini_sgp4.kara`, statement for statement and in the same evaluation
  order. Locals are declared once per function, the way Python scopes them,
  and a function that returns a tuple of 85 values returns a struct instead.
  It also generates the satellite record from `Satrec.__slots__`. Run it
  again and the file is unchanged.
- **The rest is ported by hand**: the TLE reader, `export_tle`, the
  checksums, Alpha-5, the date helpers, `rv2coe`, and the `Satrec` methods
  in `model.py`.
- **Python's arithmetic comes along.** Float `%` (`fmod`, then moved onto the
  divisor's sign), `//` and `divmod` (CPython's algorithm), `round()` (half
  to even on the exact binary value), and the `'{: .4e}'` format `export_tle`
  uses for the drag terms (rounded half to even from the exact decimal
  expansion).
- **Python's `int()` and `float()` are ported too**, with their whitespace,
  sign, underscore, `inf` and `nan` rules, because they decide which
  malformed TLE fields are rejected and with what message.
- **`datetime`'s checks** (and its `repr`) are ported for the 1.x API's
  `epoch` attribute, which is where the December 32nd epoch fails before it
  falls back to `invjday`.

## Compiler gaps

None were hit.

## Scope

- **The pure-Python path only.** The C++ extension (`sgp4.wrapper`, which
  `sgp4.api` prefers when it is installed) gives slightly different floats;
  the oracle uses `sgp4.model.Satrec` and asserts the extension is not
  loaded.
- **Left out:** the NumPy array methods (`sgp4_array`, `SatrecArray`), the
  OMM reader and `export_omm`, and the `conveniences` module, which builds
  `datetime` objects with time zones.
- **Orbits that make Python raise inside the propagator** are left out:
  `mk.py` keeps a random orbit only when the library propagates it without a
  `ZeroDivisionError`, an `OverflowError` or a complex number (which `pow`
  returns for a negative mean motion). The port would compute an infinity
  or a NaN where Python raises.
- **Satellite numbers are given as integers to `sgp4init`**, as in the
  library's tests.

The vendored library is unchanged.

## Running

```bash
karac run mini_sgp4.kara                 # interpret
karac build --mir mini_sgp4.kara         # native binary
python3 -I -B mk.py                      # regenerate data/jobs.tsv
python3 -I -B oracle.py > expected.out   # regenerate expected.out
python3 -I -B translate.py               # regenerate the translated core
python3 -s -P -B gen_kara.py             # regenerate the embedded jobs
```
