# mini_mercantile

A port of [mercantile](https://pypi.org/project/mercantile/) 1.2.1
(BSD-3-Clause, Mapbox): the spherical-Mercator "XYZ" tile toolkit behind slippy
web maps and tile servers. Given a longitude/latitude it names the map tile that
contains it at a zoom level; given a tile it finds its parent, children,
neighbours and Bing-style quadkey; given a bounding box it lists every tile
covering it or finds the single smallest tile that holds it.

## What it does

`mk.py` writes `data/jobs.tsv`, one library call per line (operation, then
tab-separated arguments). `oracle.py` replays each call through the vendored
library and `mini_mercantile.kara` through the port. Both print one line per job:

```
tile(-105.0, 39.75, 10) => 213/388/10
quadkey(486, 332, 10) => [0313102310]
children(486, 332, 10, -) => 4: 972/664/11 973/664/11 973/665/11 972/665/11
```

A tile prints as `x/y/z`, a tile list as its count followed by the tiles in the
library's order, a quadkey in brackets, a missing parent as `None`, a bad
quadkey as `ERR QuadKeyError`, and `minmax` as `lo hi`. A target zoom of `-`
means the argument was not given.

The 53 jobs cover `tile` for cities on every continent plus the clamped poles
and antimeridian, `quadkey` and `quadkey_to_tile` (including the empty key and a
bad digit), `parent` and `children` one level and several levels away,
`neighbors` in the middle of the map and at the corners where some are cut off,
`tiles` for a small box, the whole world, two zooms at once and a box that
crosses the antimeridian, `bounding_tile` for boxes from a single point (zoom
28) to the whole world (zoom 0), and `minmax`.

## How the port works

- **Projection.** `tile` is the library's `_xy` followed by its floor-with-
  `EPSILON` index step, using the same f64 operations (`sin`, natural `log`,
  `pow`, `floor`) and the same explicit constants, so a point maps to exactly
  the library's tile. Points outside the projection clamp to the first or last
  tile, as in the library.
- **Bit tricks as arithmetic.** The library builds quadkeys and bounding tiles
  with `&`, `|`, `<<` and `>>`. Kāra has no bitwise operators, so each one is an
  integer `/` by a power of two followed by `% 2`, which is exact for the
  non-negative tile indices involved.
- **`children`** reproduces the library's breadth-first expansion (pop the
  front tile, append its four children top-left, top-right, bottom-right,
  bottom-left) with a head index into one growing vector, so the output order
  is the library's.
- **`tiles`** splits an antimeridian-crossing box in two, clamps to the
  projection's latitude limits, and walks the corner tiles' index ranges.
- **`bounding_tile`** finds the first zoom at which the two corners' zoom-32
  indices differ in a bit, then shifts down to that zoom.

Everything is computed in order with no hash-map iteration, so the output is
deterministic and `run == build` holds.

## Scope

- **Integer results only.** The functions that return floating-point
  coordinates (`ul`, `bounds`, `xy`, `lnglat`, `xy_bounds`, `feature`,
  `geojson_bounds`) are out of scope, which keeps the oracle independent of
  float formatting. Their inverse, `tile`, is in scope and computes with floats
  internally.
- **`simplify` is not ported** because it returns a Python `set`, whose order
  is unspecified; a port would have to invent an order the library does not
  have.
- The `truncate=True` option, the tuple-or-`Tile` argument overloading, and the
  `mercantile` click command-line tool (`vendor/` omits its `scripts/` package)
  are out of scope.
- Inputs are chosen away from tile edges, where the oracle and the port would
  still agree but a reader checking by hand could not tell.

The port was checked against the library on every backend over the job stream,
and separately a restricted-op reimplementation using only the operations the
port uses was compared job-for-job against the library during development.

The vendored library imports only the standard library; `vendor/` is otherwise
left untouched.

## Running

```bash
karac run mini_mercantile.kara                # interpret
karac build --mir mini_mercantile.kara        # native binary
python3 -s -P -B mk.py                        # regenerate data/jobs.tsv
python3 -I oracle.py > expected.out           # regenerate expected.out
python3 -s -P -B gen_kara.py                  # regenerate the embedded jobs
```
