# mini_anytree

A port of the tree model and tree-rendering of
[anytree](https://github.com/c0fec0de/anytree) 2.13.0 (Apache-2.0, c0fec0de).
`anytree` is the library behind "hold an arbitrary node tree and walk it":
build a tree by attaching nodes to parents, then iterate it in pre-/post-/
level-order, ask a node for its path, ancestors, descendants, siblings, depth
and height, walk from one node to another, and pretty-print the whole thing as
the familiar `├──`/`└──` box drawing (or an ASCII fallback).

## What it does

`mk.py` writes `data/jobs.tsv`: a first block of `n` lines builds **one** tree
by attaching each node under a named parent (empty parent = the root), then the
rest are queries against it, one op per line:

    n <name> <parent>    attach node <name> under <parent> (empty parent = root)
    rt                   RenderTree, ContStyle  (│ ├── └──)  -> the tree, one node per line
    rta                  RenderTree, AsciiStyle (| |-- +-- ) -> the tree
    pre / post / lvl     PreOrder / PostOrder / LevelOrder names of the whole tree
    path <n>             path root..node          -> [a, b, c]
    anc  <n>             ancestors (root..parent)
    desc <n>             descendants (pre-order, excluding the node)
    dep  <n>             depth (edges to the root)
    hgt  <n>             height (edges to the deepest leaf below)
    leaf <n>             is_leaf -> yes | no       root <n>  is_root -> yes | no
    sib  <n>             siblings
    lvs  <n>             leaves under the node (pre-order)
    walk <a> <b>         Walker().walk(a, b) -> up:[...] top:x down:[...]
    find <prefix>        findall(name startswith prefix), pre-order

`oracle.py` replays the sequence against the vendored `anytree`;
`mini_anytree.kara` reproduces `expected.out` byte-for-byte on every backend.
The tree branches three ways under the root and goes three deep, so the walks
exercise real sibling lists and the renders exercise both the "has a later
sibling" vertical-bar case and the last-child corner on every level.

## How the port works

The tree is an **integer-id arena**: three parallel vectors — `names`, `parent`
and `kids` (each node's child-id list, in attach order) — with `root` the id of
the root. Attaching a node appends to the vectors and pushes the new id onto its
parent's `kids`; every query is then an index walk, so no node owns another and
there are no parent back-pointers to keep in sync. `path_ids` walks `parent` to
the root and reverses; `depth`/`ancestors` fall out of it; `height` recurses over
`kids`; pre-/post-order recurse and level-order is a BFS queue — each mirrors
anytree's `PreOrderIter` / `PostOrderIter` / `LevelOrderIter` (all of which
include the start node). `Walker.walk` is the library's algorithm: the common
prefix of the two root-paths gives the turning node, the source tail (reversed)
is the upward leg and the destination tail is the downward leg. `RenderTree`
reproduces anytree's row logic exactly — a `continues` stack of "this ancestor
has a later sibling" flags selects the vertical bar vs blank for each indent
column and the branch glyph (`├──` vs `└──`) for the node's own column.

## Scope

The arena tree plus `RenderTree` (ContStyle and AsciiStyle), the three iterators,
`path` / `ancestors` / `descendants` / `siblings` / `leaves`, `depth` / `height`,
`is_leaf` / `is_root`, `Walker.walk`, and `findall` by a name predicate. Node
names are unique ASCII tokens and values are the names themselves; detaching /
re-parenting, `separator`-based string paths, the `NodeMixin` subclass surface,
`DictImporter`/`JsonImporter` and the other styles are out of scope. The vendored
`anytree` is pure Python importing only the standard library, so `oracle.py`
runs exactly the code the port mirrors.

## Running

```bash
karac run mini_anytree.kara                   # interpret
karac build --mir mini_anytree.kara           # native binary
python3 -s -P -B mk.py                             # regenerate data/jobs.tsv
python3 -I oracle.py                                # regenerate expected.out
python3 -s -P -B gen_kara.py                        # regenerate the embedded jobs
```
