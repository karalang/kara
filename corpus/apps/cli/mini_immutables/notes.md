# mini_immutables

A port of the pure-Python HAMT of
[immutables](https://github.com/MagicStack/immutables) 0.21 (Apache-2.0,
Yury Selivanov / MagicStack). `immutables.Map` is a **persistent** (immutable,
structurally-shared) hash map implemented as a **Hash Array Mapped Trie** — the
same data structure behind CPython's `contextvars` and the persistent maps of
Clojure and Scala. Every `set`/`delete` returns a *new* map that shares most of
its structure with the old one, so neither mutates the original.

## What it does

`mk.py` writes `data/jobs.tsv`, a scripted sequence of operations on one
evolving map; each op prints one line:

    s <k> <v>  -> len <n>         set (the new length)
    d <k>      -> len <n> | miss  delete (miss if the key is absent)
    g <k>      -> <v> | miss      get
    h <k>      -> yes | no        membership
    l          -> len <n>
    k          -> [k1, k2, ...]   keys in the HAMT's iteration order

`oracle.py` replays the sequence against the vendored `immutables.Map`;
`mini_immutables.kara` reproduces `expected.out` byte-for-byte on every backend.
The sequence fills a tree deep enough to branch into sub-nodes, updates existing
keys (length unchanged), deletes present and absent keys, collapses sub-nodes
back to single leaves, drives the collision path, and snapshots the key order
throughout — the key order is the real test of structural fidelity.

## How the port works

`map.py` is ported statement for statement. A node is a tagged `struct Node`
(bitmap or collision) holding a `Vec[Slot]`, where `Slot` is an enum —
`Leaf(key, val)` or `Child(Node)` — so the trie is an ordinary recursive Kāra
value. The five-bit-per-level trie math (`map_mask`, `map_bitpos`, the SWAR
`map_bitcount` population count, `map_bitindex`) is the library's verbatim, and
`assoc` / `find` / `without` follow its BitmapNode and CollisionNode logic
branch for branch, including the sub-node collapse on delete and the
`CollisionNode → BitmapNode` promotion when hashes diverge.

Keys are non-negative ints. `map_hash` folds a 64-bit hash to 32 bits, so a key
above 2^32 hashes to the same value as the key equal to its fold; the job set
includes such keys to exercise the CollisionNode path (distinct keys that share
a full 32-bit hash). The port is **functional** — every `assoc`/`without`
rebuilds the nodes along the path rather than cloning-on-write in place — which
is observationally identical to the library: the keys, values, length and
iteration order all match.

## Scope

The integer-keyed `Map` core: `set`, `delete`, `get`, membership, length and
structural iteration. The `MapMutation` builder (`m.mutate()`), bulk
`update(...)`, `Map.__eq__`/`__hash__`, and `repr` are out of scope. The
vendored `immutables/map.py` is the pure-Python fallback (the optional C
extension is not built), so `oracle.py` runs exactly the code the port mirrors.

## Running

```bash
karac run mini_immutables.kara                # interpret
karac build --mir mini_immutables.kara        # native binary
python3 -s -P -B mk.py                            # regenerate data/jobs.tsv
python3 -I oracle.py                               # regenerate expected.out
python3 -s -P -B gen_kara.py                       # regenerate the embedded jobs
```
