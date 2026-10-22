# mini_ordered_set

A port of [ordered-set](https://github.com/rspeer/ordered-set) 4.1.0 (MIT,
Elia Robyn Lake): a **set that remembers insertion order**. It is the structure
you reach for when you want set semantics — O(1)-ish membership, no duplicates,
the usual union/intersection/difference algebra — but the iteration order has to
be stable and first-appearance, not the hash-random order a plain `set` gives.
It shows up wherever a deduplicated-yet-ordered list matters: de-duping a
pipeline's columns while keeping their declared order, building an ordered index
of seen keys, or preserving the order of options a user listed.

## What it does

`mk.py` writes `data/jobs.tsv`: a register machine over `OrderedSet`s. Creating
ops append a new register (printing `reg k = <repr>`, `k` in creation order);
mutating ops change a register in place; query ops print a result. One op per
line (tab-separated):

    new  <v...>           OrderedSet(v...)                  -> reg k = <repr>
    union  <r> <r2> ...   r.union(r2, ...)   (new set)      -> reg k = <repr>
    inter  <r> [<r2>...]  r.intersection(...) (new set)     -> reg k = <repr>
    diff   <r> [<r2>...]  r.difference(...)   (new set)     -> reg k = <repr>
    symdiff <r> <r2>      r.symmetric_difference(r2)        -> reg k = <repr>
    add  <r> <v>          r.add(v)   (in place)             -> returned index
    upd  <r> <v...>       r.update(v...) (in place)         -> last index
    disc <r> <v>          r.discard(v) (in place)           -> <repr> after
    pop  <r>              r.pop()  (last, in place)         -> popped value
    idx  <r> <v>          r.index(v)
    has  <r> <v>          v in r                            -> yes | no
    len  <r>              len(r)
    repr <r>              repr(r)
    list <r>              list(r)                           -> [..]
    rev  <r>              list(reversed(r))                 -> [..]
    sub  <r> <r2>         r.issubset(r2)                    -> yes | no
    super <r> <r2>        r.issuperset(r2)                  -> yes | no
    eq   <r> <r2>         r == r2  (order-sensitive)        -> yes | no

`oracle.py` replays the sequence against the vendored ordered_set;
`mini_ordered_set.kara` reproduces `expected.out` byte-for-byte on every
backend. The whole point of the suite is order: `add` keeps first-appearance
order and returns the *existing* index for a duplicate, `discard` reindexes, and
the set-algebra ops each have their own ordering rule — union by first
appearance across all sets, intersection and difference by the first set's
order, and `symmetric_difference` puts self's uniques before other's. Every one
of those orders is a deterministic, defined output (ordered-set's reason for
existing), not anything hash-ordered, so it is a legitimate A/B surface.

## How the port works

An `OrderedSet` is modelled as `struct OSet { items: Vec[i64] }` — the
insertion-ordered element list — and the program is a register machine over
`regs: Vec[OSet]`. The real library also carries an internal `item -> index`
hash map for O(1) membership, but that map is an **unobservable performance
detail**: it changes how fast `contains`/`index` run, never what any op returns.
So the port does membership and index by scanning `items`, which is observably
identical and keeps the data model a single ordered vector. `add` appends only
on first appearance and returns the found index; `discard` rebuilds `items`
without the value (reindexing everything after it); `pop` drops the last.
Union/intersection/difference build a fresh `OSet` by walking the governing
set's `items` in order and testing membership against the others; `intersection`
and `difference` with no other sets copy, matching the library. `eq` is
order-sensitive (two sets with the same elements in different order are not
equal), as ordered-set documents.

## Scope

The `OrderedSet` over `i64` items: construction, `add`/`update`/`discard`/`pop`,
`index`/`in`/`len`, `repr`, forward and reverse iteration, the four set-algebra
operations, and the subset/superset/equality predicates. ordered-set also
supports slice indexing (`oset[1:3]` returning an `OrderedSet`) and arbitrary
hashable elements; this port fixes the element type to `i64` and the indexing to
single-element `index`, which is the surface its own README examples use. The
vendored `ordered_set/__init__.py` is a single pure-Python module importing only
the standard library, so `oracle.py` runs exactly the code the port mirrors.

## Running

```bash
karac run mini_ordered_set.kara                # interpret
karac build --mir mini_ordered_set.kara        # native binary
python3 -s -P -B mk.py                              # regenerate data/jobs.tsv
python3 -I oracle.py                                 # regenerate expected.out
python3 -s -P -B gen_kara.py                         # regenerate the embedded jobs
```
