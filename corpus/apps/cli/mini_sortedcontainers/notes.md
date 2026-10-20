# mini_sortedcontainers

Grant Jenks's sortedcontainers 2.4.0, the pure-Python sorted list, dict
and set that Python services reach for when they need ordered data:
order books, leaderboards, schedulers, rolling percentiles. The port
follows:
- `SortedList` and `SortedKeyList`: adding and updating, removing by
  value and by position, positions and slices (and `del` of both),
  `bisect_*`, `irange`, `irange_key`, `islice`, `count`, `index`,
  `+`, `*`, the comparisons with a list, and the methods it refuses
  (`append`, `insert`, item assignment, ...) with their messages;
- `SortedDict`: the dict methods, `peekitem`, `popitem` by position,
  the sorted keys, items and values views (indexing, slicing, `del`),
  `|` both ways, and `update`;
- `SortedSet`: the set methods and operators, the in-place updates, and
  positions;
- Python's repr of all of them, and its exceptions and their messages.

The containers keep their values in sublists of about `load` values,
split a sublist when it passes twice the load, merge it into its
neighbour when it falls to half, and build a tree of sublist lengths
(the positional index) the first time a position is asked for. The port
does all of this the way sortedcontainers does, so the sublists, the
index and its offset come out the same, and a trace prints them.

`mk.py` writes `data/`. It holds:
- six command traces that drive a `SortedList` (load 4 and the default
  1000), a `SortedKeyList` keyed by `x % 10` and by `-x`, a
  `SortedDict` and a `SortedSet` through their whole API, printing the
  sublists and the index every 40 commands and checking the invariants
  (`_check`);
- `orders.csv`, limit and market orders and cancels for an order book
  kept as two `SortedDict`s of price levels;
- `scores.txt`, game events for a leaderboard kept as a `SortedKeyList`
  ranked by points, then name;
- `latency.log`, request timings for a monitor that keeps the last 50
  in a `SortedList` for p50, p95 and p99, raises and clears an alert,
  and reports each route;
- `bookings.txt`, bookings, cancellations and free-slot queries for a
  room calendar kept as a `SortedDict` per room;
- `jobs.tsv`, the jobs.

Each job prints `== ` and its fields, then its output. A trace line
prints the command and what it returned, or the exception it raised.

`oracle.py` runs the jobs through sortedcontainers itself (the copy in
`sortedcontainers/`, unchanged) and wrote `expected.out`. A key
function's repr prints as `<function name>`, without its address.
`gen_kara.py` writes the data section.

The quirks are kept. Some examples:
- `SortedList.clear` resets the index's offset and
  `SortedKeyList.clear` does not, so the two print different offsets
  after the same commands.
- `sl + values`, `sl * n`, `copy()`, `|` and the set operators build a
  new container with the default load, whatever the load of the one
  they started from.
- `SortedSet.update` and `difference_update` return the set itself,
  and `SortedDict.popitem` on an empty dict says
  `'popitem(): dictionary is empty'`, quotes and all.
- `SortedList.count` and `index` find the ends of a run of equal values
  by position, which builds the index, where `SortedKeyList` walks the
  run and may not.

sortedcontainers is under the Apache License 2.0
(`LICENSE-sortedcontainers.txt`).
