# mini_pybktree

A port of [pybktree](https://github.com/Jetsetter/pybktree) 1.1 (MIT,
Jetsetter): a **BK-tree**, the metric-tree structure for fast *approximate*
lookups — "find every item within distance n of this one". It is what sits
behind spell-checkers, perceptual-image-hash dedup, and fuzzy record matching:
given any distance metric that obeys the triangle inequality, it answers
bounded-distance queries while visiting only a fraction of the items. The
metric here is **Hamming distance** on integers (the number of differing bits),
pybktree's documented default.

## What it does

`mk.py` writes `data/jobs.tsv`: a first block of `add` ops builds one tree from
a fixed sequence of integers, then queries follow. One op per line
(tab-separated):

    add  <v>          add integer v                     -> <repr> after the add
    find <item> <n>   items within hamming distance n    -> [(d, item), ...]
    iter              every item, in the tree's BFS order -> [a, b, ...]
    len               number of items
    dist <x> <y>      hamming_distance(x, y)

`oracle.py` replays the sequence against the vendored pybktree;
`mini_pybktree.kara` reproduces `expected.out` byte-for-byte on every backend.
Because a BK-tree branches on *distance-to-parent*, the add order fixes the
whole tree shape — and therefore the `iter` BFS order and the tie order inside
a `find` result — so those are stable, deterministic outputs rather than
anything hash-ordered. The `find` queries sweep n from 0 (an exact match) up to
a huge 1000 (which returns the whole tree), so the triangle-inequality pruning
(`[distance - n, distance + n]`) is exercised at both extremes.

## How the port works

The tree is an **integer-id arena** — parallel vectors `items`, `child_dist`,
`child_node` (each node's child distances and child ids, in add order) and a
`root` id — so no node owns another and the queries are plain index walks.
`bk_add` follows pybktree's loop exactly: at each node it computes the Hamming
distance to the node's item and either descends into the existing child at that
distance or creates a new child there. `bk_find` is pybktree's FIFO walk: it
pops a candidate, records it if within n, and enqueues only the children whose
edge distance lies in `[d - n, d + n]` — the triangle-inequality prune — then
returns the hits **stably sorted by distance** (ties kept in visitation order),
matching the library's `found.sort(key=distance)`. `bk_iter` is the same FIFO
walk without pruning. Hamming distance is `popcount(x ^ y)` using Kāra's `^`,
`&` and `>>` operators. `find` packs its `(distance, item)` pairs into one
`Vec[i64]` as `[d0, item0, d1, item1, …]` so a single return value carries both
columns; the formatter prints them as the library's `[(d, item), …]`.

## Scope

The `BKTree` over `i64` items with Hamming distance: `add`, `find(item, n)`,
iteration, length and the `repr`. pybktree takes an arbitrary distance function
(e.g. Levenshtein over strings); this port fixes the metric to Hamming on
integers, which is the metric its own doctests and README use. The vendored
`pybktree.py` is a single pure-Python module importing only the standard
library, so `oracle.py` runs exactly the code the port mirrors.

## Running

```bash
karac run mini_pybktree.kara                   # interpret
karac build --mir mini_pybktree.kara           # native binary
python3 -s -P -B mk.py                              # regenerate data/jobs.tsv
python3 -I oracle.py                                 # regenerate expected.out
python3 -s -P -B gen_kara.py                         # regenerate the embedded jobs
```
