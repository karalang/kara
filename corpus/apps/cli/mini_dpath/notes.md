# mini_dpath

A port of [dpath](https://github.com/dpath-maintainers/dpath-python) 2.2.0
(MIT, Caleb Case & the dpath maintainers) — the library that addresses nested
dict/list data with filesystem-like glob paths, the way config tooling reaches
deep into a document without a chain of brackets. `search`, `values`, and `get`
each take a glob such as `users/*/name` or `**/age` and walk the document for
the nodes it matches.

## What it does

`mk.py` writes `data/jobs.tsv`, one query per line:

    <op> \t <glob> \t <json-document>

`op` is one of:

- `search` → every `(path, value)` pair whose path matches the glob, in
  dpath's walk order, each rendered `path=value` and joined with ` | ` (or
  `(none)`);
- `values` → just the matching values, in the same order;
- `get` → the single value the glob addresses, or `KeyError` (no match) /
  `ValueError` (more than one match).

`oracle.py` runs each job through the vendored `dpath` and prints
`expected.out`; `mini_dpath.kara` reproduces it byte-for-byte on every backend.

## How the port works

A JSON value needs no recursive type: values live in a flat **arena** of nodes
indexed by `i64` (tag `0` null, `1` bool, `2` int, `3` str, `4` list, `5`
dict). A container node carries a `[kstart, kstart+klen)` range into flat
`kids`/`keys` arrays, so the whole document is a handful of parallel vectors.

Matching has two halves:

- **walk order** — `walk` enumerates all of a node's direct children first (in
  key order; list indices as their decimal string), pushing each onto a
  work list, then recurses into each child's subtree. This is dpath's
  breadth-at-each-node order, and it is what makes `search` and `values`
  deterministic.
- **glob match** — a path is split into segments and compared to the glob
  segment by segment. A single `**` is normalized by padding it to the number
  of `*` segments the path needs (or dropped when the path is one segment
  shorter), after which each segment is an exact key, `*` (any one segment), or
  a `?`/`*` `fnmatch` pattern. `get` runs `search` and demands exactly one hit.

## Scope

Local in-memory documents and the three read ops. No `new`/`set`/`delete`
mutation, no `merge`, no separator other than `/`, and no `afilter` value
predicate. Globs are the plain segment forms (`*`, `?`, `**`, literals); the
regex-segment extension is out of scope. The job strings avoid JSON escapes, so
serialization emits none.

## Running

```bash
karac run mini_dpath.kara                   # interpret
karac build --mir mini_dpath.kara           # native binary
python3 -s -P -B mk.py                       # regenerate data/jobs.tsv
python3 -I oracle.py                          # regenerate expected.out
python3 -s -P -B gen_kara.py                  # regenerate the embedded jobs block
```
