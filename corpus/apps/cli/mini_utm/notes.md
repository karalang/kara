# mini_utm

A port of [utm](https://pypi.org/project/utm/) 0.9.0 (MIT, Tobias Bieniek):
the bidirectional converter between WGS84 latitude/longitude and Universal
Transverse Mercator coordinates, the metre grid that GIS scripts, survey and
drone tooling, and map pipelines use for local distances. Aachen at
50.77535 N, 6.08389 E is easting 294408.9 m, northing 5628898.0 m in zone 32U.

## What it does

`mk.py` writes `data/jobs.tsv`, one library call per line. `oracle.py` replays
each call through the vendored library and `mini_utm.kara` through the port.
API calls print one line, the `repr()` of the result or the exception:

```
fl(50.77535, 6.08389, None, None, None) => (294408.9171386983, 5628897.997839693, 32, 'U')
tl(313784.0, 5427057.0, 60, G, None, True) => (-41.2864628270766, 174.77623625173158)
zn(56.0, 3.0) => 32
cz(CX) => ValueError: substring not found
```

`fl` is `from_latlon(latitude, longitude, force_zone_number,
force_zone_letter, force_northern)` and `tl` is `to_latlon(easting,
northing, zone_number, zone_letter, northern, strict)`; `None` is an omitted
argument. A `cli` job runs the package's `utm-converter` script with that
command line and prints what it prints:

```
cli(latlon, 50.77535, 6.08389):
  294408.9171386983,5628897.997839693,32,U
```

The 128 jobs cover `from_latlon` at 24 places (Norway's zone 32 exception at
Bergen, Svalbard's zones 31 to 37, both poles' limits, the antimeridian, the
equator) and with every override, including a zone forced far from the point
and the library's quirks (a forced zone letter is only validated when a zone
number is forced too, and an empty one is returned as is); `to_latlon` for
both hemispheres by letter and by flag, at the edges of its strict bounds and
with `strict=False`; every `OutOfRangeError` and `ValueError`; the zone
helpers; and both `utm-converter` commands, including its usage error.

## How the port works

- **The series** for both directions are transcribed term for term in the
  library's evaluation order, so every intermediate float rounds the same way.
  The constants (`M1` to `M4`, `P2` to `P5`, `E_P2`) are computed with the
  same expressions at run time.
- **`x**2` is libm `pow()`.** CPython's float `**` calls the C library's
  `pow()`, which is not always the correctly rounded `x*x`: for the cosine of
  12.99210956845755 degrees the two differ in the last bit, and that changes
  the easting of a point projected onto zone 50. The port calls `pow(2.0)`
  where the library writes `**2`, and that job is in the set.
- **`math.radians` / `math.degrees`** are a multiply by `pi/180` and
  `180/pi`, which is how CPython computes them. Python's float `%` (used by
  `mod_angle` and to normalise longitudes) is `fmod` with the result moved
  into the divisor's sign.
- **Zone letters** compare as Python strings do (bytewise, ASCII upper-cased),
  so a two-letter "zone" like `CX` passes the range check and then fails in
  `ZONE_LETTERS.index`, as it does in the library.

A restricted-op prototype used during development made these substitutions
and agreed with the library on 1,782,383 checks over random coordinates,
zones and overrides.

## Scope

- The library uses numpy when it is installed and `math` otherwise. The port
  and the oracle take the `math` path (the oracle blocks numpy), which is
  what scalar calls use; array inputs are out of scope.
- The command-line jobs pass valid arguments; argparse's own usage errors
  for missing or non-numeric arguments are not reproduced, apart from the
  script's own check for an empty zone letter.

The vendored library imports only the standard library; `vendor/` is otherwise
left untouched.

## Running

```bash
karac run mini_utm.kara                       # interpret
karac build --mir mini_utm.kara               # native binary
python3 -s -P -B mk.py                        # regenerate data/jobs.tsv
python3 -I -B oracle.py > expected.out        # regenerate expected.out
python3 -s -P -B gen_kara.py                  # regenerate the embedded jobs
```
