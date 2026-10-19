# mini_cachetools

The caches a web service puts in front of slow lookups, as cachetools
7.2.3 implements them. cachetools is the library behind caching in many
Python services and SDKs. The port covers:
- All seven cache classes:
  - `Cache`, `FIFOCache`, `LRUCache` and `LFUCache`;
  - `RRCache`, which evicts a random key;
  - `TTLCache`, where items expire a fixed time after they are set;
  - `TLRUCache`, where a function gives each item its own expiry.
- Each class keeps:
  - its own eviction order;
  - size accounting with `getsizeof`, including the "value too large" and
    "value size must be non-negative" errors;
  - the mapping methods `get`, `pop`, `setdefault`, `popitem`, `update`,
    `keys`, `values` and `items`;
  - Python's repr and exceptions.
- The timed caches keep:
  - their timer, which is frozen while one operation runs;
  - the expiry ring of `TTLCache` and the heap of `TLRUCache`, with its
    removed-item marks and clean-ups;
  - `expire()`, which returns what it dropped.
- The `cached` decorator with and without a lock, `info=True` and
  `cache=None` (with its deprecation warning).
- The functools-style `lru_cache`, `lfu_cache`, `fifo_cache`,
  `rr_cache` and `ttl_cache`, with `cache_info()`, `cache_clear()` and
  `cache_parameters()`.
- The key functions `hashkey` and `typedkey`.

The run writes `data/` and then works through the 57 jobs in
`data/jobs.jsonl`:
- 38 scripts of 2636 cache operations:
  - the same operations run against every class;
  - a byte budget for rendered pages;
  - LRU reordering, LFU counts and ties;
  - session expiry in a `TTLCache` and per-key lifetimes in a
    `TLRUCache`, each with a clock that stands still or moves on every
    read;
  - 13 scripts drawn at random from a fixed seed.
- 17 memoized runs:
  - a recursive `fib` through `cached` and four of the functools-style
    decorators;
  - a user lookup with a TTL, where `1`, `1.0` and `True` share a key
    unless `typed=True`;
  - a price table called with keyword arguments in different orders;
  - a page renderer under a size budget.
- Two jobs of cache keys.

`oracle.py` runs the same jobs through the Python files of the 7.2.3
wheel in `cachetools/`, unchanged, and wrote `expected.out`. The caches
get the job's own clock: it starts at `start` and moves on by `step`
every time it is read. `RRCache` gets a choice function that takes the
job's `picks` in turn.

The quirks are kept. Some examples:
- `LFUCache` keeps the keys of each use count in a Python set and evicts
  whichever comes first in it. So the port carries CPython's set table
  (linear probing, perturbation, dummies and resizes) and its str, int
  and tuple hashes. The oracle runs under `PYTHONHASHSEED=0` so that str
  hashes, and so the eviction order, are the same on every run.
- `list(cache)`, `items()` and `values()` ask a timed cache for its
  length first, which reads the clock and expires items before the walk
  starts.
- Reading an item of a timed cache inside `items()` uses the time frozen
  for that step of the walk.
- `del` on a `TTLCache` item that has expired removes it and then raises
  `KeyError` anyway.
- `values()` on an `LRUCache` moves every key to the end, and on an
  `LFUCache` it counts as a use of each key.
- Setting a `TLRUCache` item whose expiry is already past removes the old
  item and stores nothing.
- `ttl_cache(maxsize=None)` reports `maxsize=None` in `cache_info()` but
  `maxsize=inf` in the cache's repr.
- Keyword arguments go into the key behind a marker, the
  `_HashedTuple` class itself, so `price('apple')`,
  `price('apple', 1)` and `price('apple', qty=1)` are three keys.

## License

cachetools is under the MIT license in `LICENSE-cachetools`:

    Copyright (c) 2014-2026 Thomas Kemmer

That covers the package in `cachetools/`.
