# mini_deepmerge

A port of [deepmerge](https://github.com/toumorokoshi/deepmerge) 3.0.1 (MIT,
Yusuke Tsutsumi) — the library pip's config handling, pydantic-settings, and a
long tail of apps reach for to deep-merge two nested data structures under a
configurable per-type strategy. A `Merger` carries a strategy for each
container type (list / dict / set), a *fallback* strategy for two like-typed
scalars, and a *type-conflict* strategy for two unlike values; `merge(base,
nxt)` walks both in lockstep.

## What it does

`mk.py` writes `data/jobs.tsv`, one job per line:

- `merge <merger> <base> <nxt>` → the merged value, serialized, or `RAISE`.

Values use a tiny JSON-with-sets grammar — `null` / `true` / `false` /
integers / `"strings"` / `[lists]` / `{"k": v}` dicts / `<sets>` — which is
also the grammar the result is serialized back into. `<merger>` names one of
eight strategy configurations:

| name | list | dict | set | fallback | type conflict |
|---|---|---|---|---|---|
| `always` | append | merge | union | override | override |
| `conservative` | append | merge | union | use_existing | use_existing |
| `raise` | append | merge | union | — (raise) | — (raise) |
| `prepend` | prepend | override | union | override | override |
| `unique` | append_unique | merge | union | override | override |
| `intersect` | append | merge | intersect | override | override |
| `setoverride` | append | merge | override | override | override |
| `ifnotempty` | append | merge | union | override | override_if_not_empty |

The first three are deepmerge's own `always_merger`, `conservative_merger`,
and `merge_or_raise` presets. `oracle.py` runs each job through the vendored
`deepmerge` and prints `expected.out`; `mini_deepmerge.kara` reproduces it
byte-for-byte on every backend.

## How the port works

A dynamic value needs no recursive type: values live in a flat **arena** of
nodes indexed by `i64`. A node's tag is `0` null, `1` bool, `2` int, `3` str,
`4` list, `5` dict, `6` set; a scalar carries `ival`/`sval`, and a container
carries a `[kstart, kstart+klen)` range into flat `kids` (child indices) and
`keys` (dict keys) arrays. Nodes are immutable once built, so a merge that
keeps a sub-tree unchanged simply reuses its index — no deep copy.

`value_strategy` mirrors deepmerge's own dispatch: if both values share a
container type it runs that type's strategy (which recurses for `dict merge`);
two like-typed scalars go to the fallback strategy; two unlike values go to
the type-conflict strategy. An empty strategy list (as in `merge_or_raise`)
propagates a `-1` sentinel that surfaces as `RAISE`. `append_unique` and
set membership use a structural `node_eq` (dicts compared order-independently,
exactly as Python's `ExtendedSet` compares them).

Sets are serialized in a canonical order (`null` < `false` < `true` < ints <
strings) so the output never depends on Python's per-process set iteration
order — the one place the reference's own output is non-deterministic.

## Scope

Only hashable scalars go inside a set (matching Python, where a `set` cannot
hold a `dict` or `list`). `override_if_not_empty` treats `None` and any
zero-length sized value (empty string / list / dict / set) as empty and keeps
`base`, while `0` / `false` override — deepmerge's documented rule. Python's
`bool`-is-`int` subclassing (so `1 == True`, and a bool-vs-int pair is a
*fallback* rather than a type conflict) is not exercised; the jobs never mix
the two at one position.

## Running

```bash
karac run mini_deepmerge.kara                 # interpret
karac build --mir mini_deepmerge.kara         # native binary
python3 -s -P -B mk.py                           # regenerate data/jobs.tsv
python3 -I oracle.py                              # regenerate expected.out
python3 -s -P -B gen_kara.py                      # regenerate the embedded jobs block
```
