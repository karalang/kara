# mini_collections

Python 3.13's `collections` module as generic Kāra types, with
Python's reprs: an insertion-ordered `Dict` (behind `dict` and
`OrderedDict`), `Counter`, `deque`, `ChainMap` and
`functools.lru_cache`'s policy. A few small programs use them:
grouping an access log, `tail -n`, and a cache over a request trace.

The run writes a page of text, an access log and a cache trace into
`data/` and runs `data/jobs.tsv`, 21 jobs with tab-separated fields;
the operations in a job's last field are separated by `|`. Each job
prints `== ` and its fields joined by ` | `, then a line per
operation with its result and the object's repr.

The jobs cover:
- `Counter`: construction, `most_common` with and without `n` (ties
  in insertion order), `total`, missing keys, `elements`, `update`,
  `subtract` to zero and below, `del`, unary `+` and `-`, the
  binary `+`, `-`, `&` and `|`, and the rich comparisons;
- `deque` with no `maxlen`, with 3, 1 and 0: both ends, `extend`
  and `extendleft`, indexing (negative and out of range), `rotate`
  both ways and past the length, `index`, `count`, `remove`,
  `insert` into a full deque, `reverse`, `clear`, and popping an
  empty one;
- `OrderedDict`: `move_to_end` both ways, `popitem` from both ends
  until empty, and the `KeyError`s;
- `lru_cache` with `maxsize` 3, 1, 8 and 0 over one trace: each call's
  hit or miss, `cache_info()`, the evictions and the final order;
- `ChainMap`: lookups through three layers, `in`, `len`, the key
  order of `iter`, writes and deletes that only touch the first map
  (and their `KeyError`s), `new_child` and `parents`;
- a `defaultdict(list)` of paths by status with `Counter.most_common`
  of clients, and `tail -n` as `deque(lines, maxlen=n)`.

**The port.** `Dict[K, V]` keeps entries in a `Vec` with a `Map`
index, since `Map` iterates in random order. `Counter`'s arithmetic
and comparisons follow `collections/__init__.py` loop by loop, and its
repr lists keys in `most_common` order as Python's does. `deque`
follows the C module's semantics (a full deque drops from the other
end; `insert` refuses when full), and `ChainMap`'s key order follows
its `__iter__`. The cache follows `lru_cache`'s bounded wrapper.
`repr` of a string follows `str.__repr__`'s choice of quotes.

Not supported:
- `namedtuple`, `UserDict` and friends, and `Counter`'s in-place
  operators;
- `repr` escapes only backslashes, quotes, `\n`, `\t` and `\r`.

**Oracle.** `expected.out` is Python 3.13.16's output for the same jobs,
made by `oracle.py` with the real `collections` and
`functools.lru_cache`. Run `mk.py` first; it writes `data/`.
`gen_kara.py` prints the Kāra data section from the same files.

## Edits from mini_collections.orig.kara to mini_collections.kara

Author errors:
- A literal brace in an f-string was written `\{`; Kāra writes `{{`.
- A `filter` closure over `iter()` dereferenced its argument twice,
  `**x`; it gets a `ref i64`, so one `*` is right.

Compiler gaps:
- B-2026-10-18-17: `karac check` refuses `*self` on a `ref self`
  receiver, so `impl PyRepr for bool` tests `self` directly.
- B-2026-10-18-3 and B-2026-10-15-20: in `Counter.signed`, a loop
  that moves its key into the result in each of two exclusive branches
  is refused, and so is a clone in one branch with a move in the other.
  Both branches clone.
- B-2026-10-18-18: on the MIR surfaces, `Counter.new()` and
  `Dict.new()` whose type comes only from later use are not lowered.
  Eight sites write the type out.

After those edits the program matched expected.out on both MIR
surfaces. `karac fix` found nothing to fix.

Written 2026-10-18 by the real-world apps thread; karac at ab69f69327.

## Library migration, 2026-10-18

Thread A's a0ca700228 and acd593fc77 changed the library to match docs/library: a number's `parse` returns `Result[T, ParseError]`, and `split`, `lines`, `split_whitespace` and `trim` give `Str` views (`split` and `lines` are now lazy iterators). Arguments forwarded to a `mut ref` parameter are now marked too. `karac fix` made the edits, with one more by hand: The LRU job's keys collect into `Vec[String]` (the `Lru[String, String]` takes `ref String`), and `word_list` binds its collected `Vec[String]` before `return Ok(..)`, because a `collect()` inside `Ok(..)` does not lower on MIR (B-2026-10-18-37). The app matches expected.out on all four MIR backends again (karac at a4b0967495).
