# mini_toposort

A port of [toposort](https://pypi.org/project/toposort/) 1.10 (Apache-2.0; True
Blade Systems), the small **topological-sort** library. Given a dependency map —
each item mapped to the set of items it depends on — it returns the items in
dependency order, grouped into levels that can run in parallel. It is the kind of
routine that orders build targets, database migrations, task graphs and package
installs.

## What it does

`mk.py` writes `data/jobs.tsv`: one tab-separated job per line, one of

- `l <graphspec>` — topological sort into **levels**: the first level is every
  item with no dependencies, each later level the items whose dependencies are
  all in earlier levels. Items within a level are sorted and joined with `,`,
  levels joined with ` | `. A circular dependency prints `ERR`.
- `f <graphspec>` — `toposort_flatten(sort=True)`: the single flat order (each
  level sorted, then concatenated), space-joined. A cycle prints `ERR`.

A `<graphspec>` is a `;`-separated list of `key:dep1,dep2` entries (the
dependency list may be empty), e.g. `a:b,c;b:d;c:d;d:`. Items are ASCII tokens
containing none of tab, newline, `;`, `:` or `,`.

`oracle.py` replays each job against the vendored `toposort` library and writes
`expected.out`; `mini_toposort.kara` reproduces it byte-for-byte on every backend.

## How the port works

`mini_toposort.kara` follows `toposort/__init__.py`. It parses the graphspec into
a key list with each key's dependency tokens, then matches the library's two
preparation steps exactly: **self-dependencies are discarded** (`e != item`), and
**items that appear only as a dependency** are added as keys with no dependencies
of their own. Dependencies are resolved to indices into the key list.

The sort then repeats the library's loop: collect every not-yet-emitted item
whose dependencies have all been emitted, emit that level, and mark it done; stop
when no item is ready. If any item is still unemitted at the end, that is a
circular dependency.

The library yields each level as a Python `set`, whose iteration order is
unspecified, so both the `l` output and `toposort_flatten(sort=True)` sort the
items within a level — and this port sorts them the same way, lexicographically by
byte, so the two agree exactly.

## Scope

The port covers both public entry points (`toposort` levels and
`toposort_flatten`) over arbitrary dependency graphs, including the empty graph,
self-dependencies, items that appear only as dependencies, and every cycle shape.

- **Circular-error text.** A cycle is reported with the canonical token `ERR`.
  `CircularDependencyError`'s message lists the leftover items as a formatted
  `dict`/`set` repr whose order is the per-process hash order, so reproducing that
  exact string is not a stable, meaningful surface — the fact of the cycle is.
- **Item type.** Items are ASCII string keys, which is how dependency graphs are
  written in practice (target names, module paths). The library accepts any
  hashable item; non-string items are outside what a text job file can express.

The vendored `toposort` is pure Python with no imports at all, so `oracle.py` runs
exactly the code the port mirrors.

## Running

```bash
karac run mini_toposort.kara                 # interpret
karac build --mir mini_toposort.kara         # native binary
python3 -s -P -B mk.py                        # regenerate data/jobs.tsv
python3 -I oracle.py                           # regenerate expected.out
python3 -s -P -B gen_kara.py                   # regenerate the embedded jobs
```
