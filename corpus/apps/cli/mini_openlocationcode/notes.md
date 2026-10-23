# mini_openlocationcode

A port of Google's [Open Location Code](https://pypi.org/project/openlocationcode/)
library 1.0.1 (Apache-2.0): the "Plus Codes" Google Maps shows for places that
have no street address. A Plus Code such as `8FVC9G8F+6X` names a roughly
14 m square anywhere on Earth, can be worked out offline, and can be shortened
to `9G8F+6X` next to a nearby town and recovered from it.

## What it does

`mk.py` writes `data/jobs.tsv`, one library call per line. `oracle.py` replays
each call through the vendored library and `mini_openlocationcode.kara` through
the port. Both print one line per job:

```
encode(47.365590, 8.524997) => 8FVC9G8F+6X
decode(8FVC0000+) => [47.0, 8.0, 48.0, 9.0, 47.5, 8.5, 4]
check(WC2345+G6g) => True True False
shorten(9C3W9QCJ+2VX, 51.3701125, -1.217765625) => +2VX
recoverNearest(8F+6X, 47.4, 8.6) => 8FVCCJ8F+6X
```

A decode prints the library's own `CodeArea` repr (south-west corner,
north-east corner, centre, code length), so its float noise, such as
`47.36562499999999`, has to come out the same. A `ValueError` prints as
`ValueError: <message>`.

The 100 jobs cover `encode` at every legal length and the illegal ones, the
poles, the antimeridian and out-of-range coordinates; `decode` of full, padded,
lower-case and grid-refined codes and of codes that are not full; the
library's validity cases for `isValid` / `isShort` / `isFull`; `shorten` at
each trimming level and its errors; and `recoverNearest` near the poles and the
date line, from full codes, and from invalid short codes.

## How the port works

- **`round(x, n)`.** The library calls Python's `round` with a digit count in
  two places: `encode` snaps the scaled coordinate with `round(..., 6)` before
  truncating it to an integer, and `decode` rounds every corner with
  `round(..., 14)`. CPython rounds the exact binary value of the float to `n`
  decimal places, half to even, and converts the decimal back to the nearest
  float. `py_round` does the same thing: it writes the float as `m / 2^k`
  (doubling a float is exact, so `k` is found by doubling until the value is
  whole), rounds `m * 10^n / 2^k` half to even in `i128`, formats the result
  as a decimal string and parses it back with the correctly rounding
  `f64.parse`.
- **Everything else** is the library's own arithmetic in the library's order:
  the integer digit loops of `encode`, the place-value sums of `decode` (whose
  float place values are whole numbers, so integer arithmetic gives the same
  values), and the float comparisons of `shorten` and `recoverNearest`. `pow`
  is the same libm `pow` Python calls.

The prototype used during development replaced only `round` with this
algorithm and agreed with the library on 283,408 checks over random
coordinates, lengths and reference points, plus every length at the poles and
the date line. A separate check compared `py_round` with `round` itself on
300,000 random floats across nine orders of magnitude.

Every job is computed in order with no hash-map iteration, so the output is
deterministic and `run == build` holds.

## Scope

- Codes are ASCII. A code with a non-ASCII character is rejected by the
  library and by the port, but at different steps, since the library counts
  positions in code points and the port in bytes.
- `CodeArea.latlng()` and the `latitudeCenter` / `longitudeCenter` attributes
  are covered through the repr; the class has no other behaviour.
- The library's unreachable `shorten` branch for codes shorter than six digits
  is not ported.

The vendored library imports only the standard library; `vendor/` is otherwise
left untouched. The sdist ships no LICENSE file, so `LICENSE-openlocationcode.txt`
reproduces the Apache License 2.0 named in the source header.

## Running

```bash
karac run mini_openlocationcode.kara                # interpret
karac build --mir mini_openlocationcode.kara        # native binary
python3 -s -P -B mk.py                              # regenerate data/jobs.tsv
python3 -I -B oracle.py > expected.out              # regenerate expected.out
python3 -s -P -B gen_kara.py                        # regenerate the embedded jobs
```
