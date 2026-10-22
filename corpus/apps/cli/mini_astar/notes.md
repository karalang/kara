# mini_astar

A port of [astar](https://pypi.org/project/astar/) 1.1 (BSD-3; Julien
Rialland), a generic **A\*** shortest-path search. A\* is the standard
best-first graph search — the algorithm behind game and robot pathfinding,
route planning and puzzle solvers — expanding the frontier node that minimises
`g + h` (cost so far plus an admissible estimate of the cost remaining).

This kata specialises the generic search to the canonical case: a 4-connected
grid maze with a Manhattan heuristic and unit step cost.

## What it does

`mk.py` writes `data/jobs.tsv`: a stream of grid mazes, each defined then
queried, in order:

- `grid <w> <h> <cells>` — start a new maze; `<cells>` is a `w*h` row-major
  string of `.` (open) and `#` (wall);
- `path <sr> <sc> <gr> <gc>` — find a shortest path from `(sr,sc)` to
  `(gr,gc)`.

`grid` prints nothing. `path` prints the path as `r,c` cells joined by `->`
(just `sr,sc` when start == goal), or `no path` when the goal is unreachable or
either endpoint is a wall. The mazes include a wide-open grid with many
equal-cost paths, forced corridors, a room split by a one-gap wall, a walled-off
goal, endpoints on walls, 1×N and N×1 strips, and an 8×8 maze with a long
detour.

`oracle.py` replays every job against the vendored `astar` library and writes
`expected.out`; `mini_astar.kara` reproduces it byte-for-byte on every backend.

## How the port works

`mini_astar.kara` follows `astar/__init__.py` exactly. A grid cell `(r,c)` is
the node `r*w + c`.

- **The frontier is a binary heap.** The library pushes `SearchNode`s onto a
  `heapq` whose `__lt__` compares `fscore` only. Among several equally short
  paths, which one is returned therefore depends on (a) the order neighbours are
  generated and (b) the exact sequence `heapq` pops. The port reproduces CPython
  `heapq` to the operation — `heappush` / `heappop` / `_siftup` / `_siftdown`,
  plus the `OpenSet.remove` decrease-key (`index` the node, pop the last, drop
  it into the hole, re-sift up and down) — over a flat `i64` heap of node ids
  ordered by a parallel `fscore` array.
- **The A\* loop (`astar`).** Pop the lowest-`f` node; if it is the goal,
  reconstruct the path through the `came_from` links and reverse it. Otherwise
  close it and relax each open neighbour: a neighbour is updated (and its heap
  entry moved) only when the new `g` is strictly smaller, so the first path
  found at a given cost wins — which, with the heap order above, is what pins
  the exact path.
- **The heuristic and neighbours.** `heuristic_cost_estimate` is Manhattan
  distance; `distance_between` is 1; `neighbors` yields the open, in-bounds
  cells in the order up, left, right, down. These three are the subclass the
  oracle instantiates, so the port carries the same definitions.

The port's output was checked against the library over the job list on every
backend, and separately (during development) over forty thousand random grids —
including wide-open ones with many equal-cost paths, which is what exercises the
heap tie-breaking.

## Scope

- **Grid A\*, not the generic class.** The library's `AStar` is generic over any
  node type with user-supplied `neighbors` / heuristic / distance. This port
  fixes the 4-connected grid instantiation (Manhattan heuristic, unit cost),
  which is the common case and the one with a determinate oracle. `reversePath`
  and `is_goal_reached` overrides are out of scope.
- **Integer costs.** The library's scores are floats (`gscore`/`fscore`,
  `math.inf`); with a unit step cost and an integer heuristic every score is a
  non-negative integer, so the port uses `i64` with a large sentinel for
  infinity. No float arithmetic is reachable on this path.

The vendored library imports only the standard library (`heapq`, `math`,
`operator`, `abc`, `typing`) and is pure Python; the search path the port covers
is integer and array arithmetic.

## Running

```bash
karac run mini_astar.kara               # interpret
karac build --mir mini_astar.kara       # native binary
python3 -s -P -B mk.py                   # regenerate data/jobs.tsv
python3 -I oracle.py                      # regenerate expected.out
python3 -s -P -B gen_kara.py              # regenerate the embedded jobs
```
