# mini_pqdict

A port of [pqdict](https://pypi.org/project/pqdict/) 1.5.1 (MIT; Nezar
Abdennur), an **indexed priority queue** — a dict-like binary heap that maps
hashable keys to priority values and, on top of the usual heap operations, lets
any item be looked up, removed or re-prioritised **by key**. It is the data
structure behind Dijkstra's shortest paths, A\* open sets, discrete-event
simulators and priority schedulers, where a value's priority has to change while
it sits in the queue.

## What it does

`mk.py` writes `data/jobs.tsv`: a stream of queue sessions. `new <min|max>`
starts a fresh queue (min = smallest value is highest priority, the default;
max = largest), and the ops that follow act on it:

- `set <key> <val>` — add the key, or update its priority if already present;
- `add <key> <val>` — add; `ERR` if the key is already present;
- `upd <key> <val>` — change an existing key's priority; `ERR` if absent;
- `del <key>` — remove a key; `ERR` if absent;
- `pop` / `top` — remove / peek the highest-priority item (`key val`); `ERR` if empty;
- `get <key>` — the key's priority; `has <key>` — `yes`/`no`; `len` — the count;
- `pushpop <key> <val>` — insert then pop the top in one step; `ERR` if the key is present;
- `swap <k1> <k2>` — exchange two keys' priorities; `rekey <old> <new>` — rename a key in place;
- `drain` — pop every item in priority order, printed `key:val` space-joined.

The sessions cover a Dijkstra-style frontier with decrease-key, a max-priority
scheduler, every error path, ties, and `swap`/`rekey` edge cases.

`oracle.py` replays every job against the vendored `pqdict` library and writes
`expected.out`; `mini_pqdict.kara` reproduces it byte-for-byte on every backend.

## How the port works

`mini_pqdict.kara` follows `pqdict/__init__.py` exactly. The heap is two
parallel arrays — a `Vec[String]` of keys and a `Vec[i64]` of priorities — and
a `mode` flag selects the `precedes` comparison (`<` for a min-queue, `>` for a
max-queue, the library's `reverse=True`).

- **The key→index map is dropped.** The library keeps a `dict` mapping each key
  to its slot in the heap so lookups are O(1). That map is an *unobservable*
  performance detail: the only thing a program can see is the sequence of pops
  and peeks. So the port omits it and finds a key's position with a linear scan
  — behaviourally identical, since each key is unique in the heap.
- **The heap algorithms are reproduced to the line.** `_sink` (the Floyd
  "sink-to-the-bottom-then-swim" variant), `_swim`, `heaprepair` (bubble up or
  down after an in-place change), `heappush`, `heappop` (move the last node into
  the hole and repair), `heapupdate` and `heappushpop`. Because the comparison
  is on priority only, which item wins a tie is decided by heap structure, and
  reproducing these operations exactly is what makes the pop order — and
  `pushpop`, `swap` and `drain` — byte-identical to the library.
- **`swap_priority` and `replace_key`** move key labels between slots without
  disturbing the heap, exactly as the library does (the priorities, and so the
  structure, stay put).

The port was checked against the library over the job list on every backend,
and separately (during development) over sixty thousand random operation
sequences in both min- and max-mode.

## Scope

- **String keys, `i64` priorities, value == priority.** The library is generic
  over hashable keys, arbitrary priority values and an optional `key=` function
  that derives a priority from each value; this port fixes string keys and
  integer priorities used directly as the priority (no `keyfn`), which is the
  common case and the one with a determinate oracle. `nlargest`/`nsmallest`, the
  `key=` transform and the `Mapping` views are out of scope.
- **Iteration order is not exercised.** `pqdict` documents that iterating keys
  is in arbitrary (heap-array) order; the jobs only use the *defined*-order
  operations (pop/top/drain), so nothing here depends on unspecified order.

The vendored library imports only the standard library and is pure Python; a
minimal `dist-info` is vendored alongside it so its import-time
`importlib.metadata.version("pqdict")` resolves without the package being
installed.

## Running

```bash
karac run mini_pqdict.kara               # interpret
karac build --mir mini_pqdict.kara       # native binary
python3 -s -P -B mk.py                     # regenerate data/jobs.tsv
python3 -I oracle.py                        # regenerate expected.out
python3 -s -P -B gen_kara.py                # regenerate the embedded jobs
```
