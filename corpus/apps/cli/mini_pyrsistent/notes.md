# mini_pyrsistent

A port of the **PVector** of
[pyrsistent](https://github.com/tobgu/pyrsistent) 0.20.0 (MIT, Tobias
Gustafsson). `PVector` is pyrsistent's persistent (immutable) vector — a drop-in
for a Python `list` where every "mutating" call returns a *new* vector and the
old one is untouched, so it is the structure behind undo stacks, snapshotting,
and sharing state between threads without locks. It is a 32-way trie with a
tail buffer (a Python port of Clojure's vector): random access is log₃₂(n) and
appends are amortised O(1), with structural sharing between versions.

## What it does

`mk.py` writes `data/jobs.tsv`, a register machine over PVectors: every creating
op appends one new vector to a numbered register file (`reg k`, in creation
order) and prints `reg <k> len <n>`; query ops print a result. One op per line
(tab-separated) — the full table is in `mk.py`'s docstring, in short:

    iota n / new v…          build a vector (range, or literals)
    app / set / ext / extr   append, set-at-index, extend by values / by a vector
    mset / del / dels / mul   multi-set, delete, slice-delete, repeat
    get / len / idx / cnt     element (negative allowed), length, index, count
    eq / lt                  compare two vectors (by contents)
    rng / list / repr        a window, the whole list, the `pvector([…])` repr

`oracle.py` replays the sequence against the vendored pyrsistent;
`mini_pyrsistent.kara` reproduces `expected.out` byte-for-byte on every backend.
The `iota 1100` vector is the point of the suite: 1100 elements overflow the
32-slot tail into the tree, and past 1024 they grow the trie a whole level, so
the boundary `get`s, `set`s and `rng` windows (around 31/32/33 and
1023/1024/1025) only come out right if the trie descent and the node splits are
correct. The small vectors carry the list/repr/compare/index/delete checks, and
each `set` is followed by reading the *original* back to show it was not mutated.

## How the port works

A node is an `enum Node { Leaf(Vec[i64]), Branch(Vec[Node]) }` — a leaf holds up
to 32 values, a branch up to 32 child nodes — and a `PVec` is
`{ count, shift, root: Node, tail: Vec[i64] }`, exactly pyrsistent's fields. The
trie math is ported from `pyrsistent/_pvector.py` branch for branch:
`pv_get` walks the branches picking child `(i >> level) & 31` and stops at the
leaf's `i & 31`; `pv_append` fills the tail, then on a full tail calls
`create_new_root` (which either `push_tail`s into the existing tree or, when the
root is full, lifts it under a new branch and grows `shift` by 5) with
`new_path` building the spine down to the pushed leaf; `pv_set` rebuilds just the
path from the root to the touched leaf via `do_set`. Because Kāra has the `>>`,
`<<` and `&` operators, the bit arithmetic is written as in the original rather
than as division. Every mutating op is a **functional rebuild** — nodes along
the path are cloned, never mutated — which is why the original vector reads back
unchanged, matching pyrsistent's structural-sharing semantics observably.
`extend`, `delete`, `slice-delete`, `mul`, `index`, `count` and the comparisons
go through the flat list, the way the library itself does.

## Scope

The `PVector` core over `i64` values: construction, `append`, `set`, indexing
(including negatives), `extend`, `mset`, `delete` and slice-`delete`, `len`,
`index`, `count`, repeat (`*`), the `==` / `<` comparisons (by contents) and the
`pvector([…])` repr. The `Evolver` (the batched in-place mutation builder) is a
performance detail with no observable difference and is out of scope; the other
pyrsistent structures — `PMap`, `PSet`, `PBag`, `PDeque`, `PClass`, `PRecord`
and the checked/field types — are separate data structures and out of scope.
Values are `i64`; pyrsistent stores arbitrary objects. The vendored pyrsistent
is pure Python (the package ships an optional C accelerator, `pvectorcmodule.c`,
which `oracle.py` never uses — it imports `python_pvector` directly), so the
oracle runs exactly the trie code the port mirrors.

## Running

```bash
karac run mini_pyrsistent.kara                   # interpret
karac build --mir mini_pyrsistent.kara           # native binary
python3 -s -P -B mk.py                                # regenerate data/jobs.tsv
python3 -I oracle.py                                   # regenerate expected.out
python3 -s -P -B gen_kara.py                           # regenerate the embedded jobs
```
