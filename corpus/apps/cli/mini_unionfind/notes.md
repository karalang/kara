# mini_unionfind

A port of [unionfind](https://pypi.org/project/unionfind/) 0.2.1 (Apache-2.0;
Saito Tsutomu), a **disjoint-set (union-find)** structure with path compression,
plus its grid-connectivity helper. Union-find is the structure behind Kruskal's
minimum spanning tree, connected-component labelling, image region analysis,
cycle detection and equivalence-class merging — any time you repeatedly merge
sets and ask whether two things ended up together.

## What it does

`mk.py` writes `data/jobs.tsv`: a stream of sessions. `new <n>` starts a fresh
union-find over the elements `0..n-1`, and the ops that follow act on it:

- `unite <i> <j>` — merge the sets containing `i` and `j`;
- `find <i>` — print the representative (root) of `i`'s set;
- `same <i> <j>` — print `yes`/`no`: are `i` and `j` in the same set?
- `groups` — print the current partition, one group per root in index order,
  members comma-joined, groups joined with `|`.

A standalone grid op drives the library's `isconnected`:

- `conn <nw> <nh> <cells>` — `cells` is an `nw*nh` row-major string of `0`/`1`
  (cell `(i,j) = cells[i*nh + j]`); print `yes` if every `1` cell forms a single
  4-connected region, else `no`.

The sessions cover Kruskal-style merges, a long chain that stresses path
compression, idempotent and self-unites, all-singleton partitions, and grid
shapes that are connected, diagonal-only (not 4-connected), split into blobs,
empty, or full.

`oracle.py` replays every job against the vendored `unionfind` library and
writes `expected.out`; `mini_unionfind.kara` reproduces it byte-for-byte on
every backend.

## How the port works

`mini_unionfind.kara` follows `unionfind/__init__.py` line for line over a flat
`Vec[i64]` parent array.

- **`find` with path compression.** `find(i)` recurses to the root and, on the
  way back, repoints `parent[i]` straight at it — the port keeps the recursion
  and the write-back.
- **`unite`** attaches `root(i)` under `root(j)` (`parent[root_i] = root_j`),
  with no union-by-rank, so which element becomes the representative is fully
  determined by the call order — this is what makes `find`'s output
  reproducible.
- **`groups`** iterates the roots (the indices with `parent[i] == i`) in order
  and, for each, lists every member in index order — exactly the library's
  nested comprehension, including that it calls `find` (and so compresses) as it
  tests membership.
- **`isconnected`** builds a throwaway union-find over the grid, unites each
  `1` cell with its equal-valued up and left neighbours using the library's flat
  index `i + j*nw`, then checks every `1` cell shares the first one's root.

The port was checked against the library over the job list on every backend, and
separately (during development) over forty thousand random operation sequences
and forty thousand random grids.

## Scope

- **Integer elements.** The data structure is pure integer arithmetic over the
  parent array — no floats, no hashing, so the four backends agree by
  construction. The library's `isconnectedlist` convenience wrapper (which just
  builds a grid and calls `isconnected`) is folded into the `conn` op.

The vendored library imports nothing outside the language itself and is pure
Python.

## Running

```bash
karac run mini_unionfind.kara               # interpret
karac build --mir mini_unionfind.kara       # native binary
python3 -s -P -B mk.py                        # regenerate data/jobs.tsv
python3 -I oracle.py                           # regenerate expected.out
python3 -s -P -B gen_kara.py                   # regenerate the embedded jobs
```
