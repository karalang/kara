# mini_pygtrie

A port of the string-keyed trie of
[pygtrie](https://github.com/google/pygtrie) 2.6.2 (Apache-2.0, Google LLC).
`pygtrie.CharTrie` is a **trie** (prefix tree) with a `dict`-like interface: the
data structure behind routing tables, autocompletion, IP longest-prefix
matching and dictionary-style string lookups. Every key shares the storage of
its prefixes, so `longest_prefix` / `shortest_prefix` and "all keys under this
prefix" are the operations it is built for.

## What it does

`mk.py` writes `data/jobs.tsv`, a scripted sequence of operations on one
evolving `CharTrie`; each op prints one line:

    s <key> <val> -> len <n>          set (the new length)
    g <key>       -> <val> | miss     get
    hk <key>      -> yes | no         has_key
    hs <key>      -> yes | no         has_subtrie  (key is a prefix of a key with a value)
    hn <key>      -> 0..3             has_node bit-field (HAS_VALUE=1, HAS_SUBTRIE=2)
    d <key>       -> len <n> | short | miss   delete (+ prune); short = ShortKeyError
    sp <key>      -> <key> <val> | none   shortest_prefix
    lp <key>      -> <key> <val> | none   longest_prefix
    pf <key>      -> [(k v), ...]     prefixes along the path, root-first
    ks <prefix>   -> [k, ...] | nokey keys(prefix), sorted
    ka            -> [k, ...]         all keys, sorted
    l             -> len <n>

`oracle.py` replays the sequence against the vendored `pygtrie.CharTrie`;
`mini_pygtrie.kara` reproduces `expected.out` byte-for-byte on every backend.
The keys share prefixes (`car`/`card`/`care`/`cart`/`cat`, `do`/`dog`/`dodge`,
`a`/`an`/`and`/`ant`) so the trie branches into real sub-nodes, and the sequence
walks the boundaries each method branches on: a value node, a prefix-only node
and an absent node (`has_node`'s four combinations); the delete that prunes a
leaf, the delete of a value sitting on a prefix (its children survive), the
`ShortKeyError` on a prefix-only key, and the plain miss; and `shortest`/
`longest`/`prefixes` with and without a match.

## How the port works

A node is a `struct TNode` holding a value flag and two parallel vectors —
`steps` (the child characters) and `kids` (the child nodes) — kept in **ascending
step order**. pygtrie's own iteration is unordered (its children live in a
`dict`), but `Trie.enable_sorting(True)` makes sibling order deterministic; the
oracle enables it and the port keeps `steps` sorted on insert, so the pre-order
DFS behind `keys` / `ka` is a total order that matches on every backend (and
across languages — unlike a hash-map walk). `set` / `get` / `has_node` /
`delete` / `shortest_prefix` / `longest_prefix` / `prefixes` / `keys(prefix)`
follow the library's `_set_node` / `_get_node` / `has_node` / `_pop_value`
(the empty-node pruning) / `__walk_towards` logic branch for branch. The port
is **functional** — every `set`/`delete` rebuilds the nodes along the path
rather than mutating in place — which is observationally identical to pygtrie:
the keys, values, length, iteration order and the `ShortKeyError` vs `KeyError`
distinction all match.

## Scope

The `CharTrie` core: `set`, `get`, `has_key`, `has_subtrie`, `has_node`,
`delete` (with pruning), `shortest_prefix`, `longest_prefix`, `prefixes`,
`keys(prefix)`, `keys()` and `len`, over string keys and int values.
`StringTrie` (separator-split keys), the tuple-keyed `Trie`, `PrefixSet`, the
subtrie slice-delete (`del t['foo':]`), `merge`/`update`, `traverse` and
pickling are out of scope. The vendored `pygtrie.py` is a single pure-Python
module importing only the standard library, so `oracle.py` runs exactly the
code the port mirrors.

## Running

```bash
karac run mini_pygtrie.kara                   # interpret
karac build --mir mini_pygtrie.kara           # native binary
python3 -s -P -B mk.py                            # regenerate data/jobs.tsv
python3 -I oracle.py                               # regenerate expected.out
python3 -s -P -B gen_kara.py                       # regenerate the embedded jobs
```
