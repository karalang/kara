# mini_heapq

Python 3.13's `heapq` and `bisect` as generic Kāra functions, and
`sched`'s scheduler built on them, run by the kinds of program that use
them: heap traces, top-k and k-way merges of web-server logs, latency
buckets, a shortest-route finder and an event scheduler.

The run writes a road map, four access logs (one empty) and a scheduler
script into `data/`, then runs `data/jobs.tsv`, 45 jobs with
tab-separated fields. Each job prints `== ` and its fields joined by
` | `, then its output.

The jobs cover:
- `heap`: `heapify`, then `heappush`, `heappop`, `heappushpop` and
  `heapreplace`, printing the heap list after each, including both
  `IndexError`s on an empty heap;
- `nlargest` and `nsmallest` on integers (n of 1, 0, negative, larger
  than the input, an empty input) and on log records by bytes, latency,
  status and path, with ties;
- `merge` of integer lists (with empty ones) and of logs by time, by
  host and by whole line;
- `bisect_left`, `bisect_right`, `insort`, and the bisect docs'
  `grade`;
- `buckets`: a Prometheus-style latency histogram, counted with
  `bisect_left` over the `le` bounds;
- `route`: Dijkstra over a heap of `(minutes, town)` pairs, with tied
  routes, a route to itself, an unreachable town and an unknown one;
- `sched`: `sched.scheduler` on a virtual clock, with events that run
  once, repeat by entering themselves again, enter another event, or
  cancel one (including one that is not queued), and its `queue`.

**The port** follows `heapq.py` exactly (`_siftdown`, `_siftup`, the
max-heap twins, and the three code paths of `nlargest` and
`nsmallest` with their decorated `(key, order, elem)` entries), so
equal keys come out where Python's do. Moving an item becomes a swap,
which leaves the same list. `merge` keeps `(key, list, position)`
entries as heapq.py does. `sorted` is a stable insertion sort.
`sched`'s events order by `(time, priority, sequence)`, and `cancel`
removes the event and heapifies, as `sched.py` does.

Not supported:
- `merge`'s `reverse`, and `bisect`'s `lo`, `hi` and `key`
  arguments;
- `sched`'s threads and non-blocking `run`.

**Oracle.** `expected.out` is Python 3.13.16's output for the same jobs,
made by `oracle.py` with the real `heapq`, `bisect` and `sched`. Run
`mk.py` first; it writes `data/`. `gen_kara.py` prints the Kāra data
section from the same files.

## Edits from mini_heapq.orig.kara to mini_heapq.kara

Author errors:
- `fs.read_dir` fails with an `IoError`, which `?` cannot pass on as
  the `String` error the function returns. It is matched instead.
- Four calls dropped the `Option` that `heappop` or `heapreplace`
  returns, which `karac check` warns about; they now bind it to `_`, as
  does the `Vec.pop` in `heapreplace_max`.

Compiler gaps:
- B-2026-10-18-15: `#[derive(PartialEq, Eq, PartialOrd, Ord)]` on the
  generic `Dec[K]` gives it no ordering, so no `Dec` could go in a
  heap or be sorted. The four impls are written by hand with
  `impl[K: Ord]`, comparing the fields in order.

After those edits the program matched expected.out on both MIR
surfaces. `karac fix` found nothing to fix.

Written 2026-10-18 by the real-world apps thread; karac at 98f0a2252d.
