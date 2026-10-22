# mini_morton

A port of [pymorton](https://github.com/trevorprater/pymorton) 0.1.0 (MIT,
Trevor Prater). **Morton order** (also Z-order, or a Morton code) maps
multi-dimensional coordinates to a single integer by interleaving the bits of
the coordinates, so that points that are close in space tend to stay close on
the resulting line. It is the ordering behind depth-first quadtree/octree
traversal and the spatial keys used by B-trees, geohashes, and GPU texture
memory: sort the codes and you have walked the tree.

## What it does

`mk.py` writes `data/jobs.tsv`, one job per line:

    il2 \t <x> \t <y>          2-D interleave  (16-bit coordinates)
    dl2 \t <n>                 2-D de-interleave
    il3 \t <x> \t <y> \t <z>   3-D interleave  (10-bit coordinates)
    dl3 \t <n>                 3-D de-interleave

- `il2 <x> <y>` / `il3 <x> <y> <z>` → the Morton code in decimal;
- `dl2 <n>` → `<x> <y>` and `dl3 <n>` → `<x> <y> <z>`, the coordinates of code
  `n`.

`oracle.py` runs the jobs through the vendored pymorton; `mini_morton.kara`
reproduces `expected.out` byte-for-byte on every backend. The coordinate grids
and boundary values walk the byte / nibble / crumb / bit boundaries the
bit-spreading masks branch on, and include coordinates above the range (16 bits
for 2-D, 10 for 3-D) to exercise the leading mask. The `dl*` inputs are the
codes the vendored encoder produced, so each round-trips, plus a few arbitrary
codes.

## How the port works

The four bit-spreading helpers — `part1by1` / `part1by2` and their inverses
`unpart1by1` / `unpart1by2` — are ported verbatim: the same magic masks
(`0x55555555`, `0x09249249`, …) and the same shift-and-or / shift-and-xor
ladder. `interleave2(x, y) = part1by1(x) | (part1by1(y) << 1)`, and `interleave3`
folds in a third coordinate at offset 2. De-interleaving runs the masks in
reverse. Because the helpers mask to the low 16 (2-D) or low 10 (3-D) bits as
their first step, a coordinate above that range is silently reduced to its low
bits — exactly as upstream, and the job set includes such inputs to pin that
behaviour. Every intermediate value stays within 32 bits, well inside `i64`, so
no wider arithmetic is needed.

## A note on the vendored source

`vendor/pymorton/__init__.py` is the upstream 0.1.0 module, untouched — it is
Python 2 (its type-guard branches use `print` statements), so it does not parse
on Python 3. `oracle.py` therefore imports `vendor/pymorton3`, a copy in which
**only** those four `print 'x'` statements are rewritten as `print('x')`; every
bit operation, mask and shift is byte-identical to the original (the two files
differ on exactly those four lines). The guard branches themselves are never
reached here, since Kāra's static types make the "non-integer argument" and
wrong-arity cases impossible.

## Scope

The integer interleave / de-interleave API. The library's `interleave_latlng` /
`deinterleave_latlng` geohash helpers build a base-4 string from floating-point
latitude/longitude and depend on float formatting, so they are out of scope.

## Running

```bash
karac run mini_morton.kara                    # interpret
karac build --mir mini_morton.kara            # native binary
python3 -s -P -B mk.py                            # regenerate data/jobs.tsv
python3 -I oracle.py                               # regenerate expected.out
python3 -s -P -B gen_kara.py                       # regenerate the embedded jobs
```
