# mini_s2sphere

A port of [s2sphere](https://pypi.org/project/s2sphere/) 0.2.5 (MIT; Jonas
Kunze and Sven Kreiss), the pure-Python port of Google's S2 Geometry
library. S2 projects the sphere onto the six faces of a cube and orders
each face along a Hilbert curve, so a 64-bit cell id names a quadtree cell
at any of 31 levels and nearby places share id prefixes. That is what
geospatial indexes, delivery and ride-hailing grids, and "which cells
cover this circle" queries are built on.

```
> cover 0 30 8 1 cap 37.7749 -122.4194 0.1
  8 cells
  80857f 808584 8085884 808588c 808f7c 808f81 808f824 808f83d4
  normalized 8
> near 37.7749 -122.4194 60 8 30
  covering 8 cells
  candidates 6
  san_francisco 0.0 km
  sausalito 11.0 km
  oakland 13.4 km
  berkeley 16.8 km
  palo_alto 44.3 km
> union 89c25 expand 8
  1 cells 89c25
  error AttributeError: 'CellId' object has no attribute 'append_all_neighbors'
```

## What it does

`mk.py` writes 320 jobs (and 12 section comments) to `data/jobs.tsv`, one
command each, plus a 46-place index (`data/places.tsv`) and six named
zones (`data/zones.tsv`). `oracle.py` runs each job against the vendored
library and prints the result, or `error <class>: <message>` when it
raises; `mini_s2sphere.kara` runs the same jobs. 36 jobs raise.

- **Cell ids**: from lat/lng and points, tokens and raw ids (Python ints
  reduced modulo `0xffffffffffffffff`, negative and oversized ones
  included), face, level, parent and children, child ranges, `next` /
  `prev` and their wrapping forms, `advance` and `advance_wrap`, edge,
  vertex and all-neighbour sets, containment and ordering, and walks over
  every cell of a level.
- **Points, lat/lngs, caps and rects**: distances, normalization,
  face/uv projection, `ortho`, caps by angle, height and area, cap growth,
  expansion and complements, rect unions, intersections, expansion,
  convolution with a cap, cap and rect bounds, and every containment and
  intersection test, including rect against cell.
- **Intervals**: `SphereInterval` (inverted, empty and full intervals,
  union, intersection, complement, Hausdorff distance) and
  `LineInterval`, plus `drem` and its two decimal errors.
- **Cells and unions**: subdivision, vertices, edges, cap and rect
  bounds, exact and approximate areas; cell unions normalized, unioned,
  intersected, differenced, clipped, denormalized and bounded.
- **RegionCoverer**: coverings and interior coverings of caps, rects and
  cells with min and max levels, `max_cells` and `level_mod`, the simple
  flood-fill covering, and the place index queried by radius (`near`) and
  by zone.

## How the port works

- **Ids are u64, arithmetic is i128.** A `CellId` keeps
  `id % 0xffffffffffffffff`, so ids live below 2^64 - 1; every operation
  that can leave that range (children, `prev` past face 0, `advance`)
  runs in i128 and goes back through the same reduction.
- **The Hilbert curve runs one level at a time.** The library batches four
  levels through its `LOOKUP_POS` and `LOOKUP_IJ` tables; the port applies
  the same `POS_TO_IJ` and orientation transitions level by level, which
  gives the same ids and orientations.
- **Floats keep the library's evaluation order.** Python's `max` and
  `min` keep their first argument on ties, which picks between 0.0 and
  -0.0, and negating the library's int-literal coordinates never yields
  -0.0; both are kept. `drem` is the IEEE remainder that
  `Decimal.remainder_near` computes, raising `InvalidOperation` for a zero
  divisor and `DivisionImpossible` for a quotient over 28 digits.
- **RegionCoverer's heap is `heapq`.** Candidates live in an arena, and
  the priority queue is a sift-for-sift copy of `heappush` and `heappop`
  over `(priority, candidate)` pairs ordered as Python compares them.
- **Asserts are errors.** Each library `assert` returns an
  `AssertionError` with an empty message, so a bad level, an invalid id or
  a negative angle fails at the same point and prints what the library
  printed before it.

## Compiler gaps

None. `karac check` flagged three struct names that were not PascalCase
and a `!` used for bitwise not, both language rules. After two port
mistakes were corrected (a face table and an output label), the
interpreter matched the oracle byte for byte.

## Scope

- The quadratic projection only, as the library ships it; the linear and
  tan projections it can be switched to are not ported.
- `Polygon`-style regions are not part of s2sphere 0.2.5; the regions are
  caps, rects and cells, as the library defines them.
- The optional C extension (`cbackend.c`) is not used by the package and
  is not vendored.

## Running

```bash
karac run mini_s2sphere.kara                  # interpret
karac build --mir mini_s2sphere.kara          # native binary
python3 -s -P -B mk.py                        # regenerate data/
python3 -I -B oracle.py > expected.out        # regenerate expected.out
python3 -I -B gen_kara.py                     # regenerate the embedded inputs
```
