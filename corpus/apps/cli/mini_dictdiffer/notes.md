# mini_dictdiffer

Diffing and patching nested data as dictdiffer 0.9.0 does it.
dictdiffer is the small library that configuration tools, audit logs
and sync jobs use to say what changed between two dicts, lists or sets,
apply that change somewhere else, undo it, and merge two edits of the
same document. The port covers:
- `diff()` with `ignore`, `path_limit` (with `*` keys), `expand`,
  `tolerance`, `absolute_tolerance`, `dot_notation` and `node`, in the
  order its generator yields;
- `patch()`, `swap()`, `revert()` and `dot_lookup()`, with the errors
  Python raises when a patch does not fit the data;
- the three-way `Merger`: `ConflictFinder`, the `Resolver` with actions
  looked up by path (cascading to shorter paths, `WildcardDict` keys
  with `*` and `+`, `NoFurtherResolutionException`), manual picks with
  `continue_run`, and the `Unifier`;
- three programs:
  - a drift check that compares the deployed service configs in
    `data/deployed/` with the desired ones in `data/desired/`, ignoring
    the volatile metadata, patches each deployed file to the desired
    state in `data/out/drift/`, and checks that reverting gives the
    deployed file back;
  - a three-way merge of editor settings (`data/merge/`) with a policy
    file of wildcard paths and the picks a person made for the rest;
  - a schema history: the diffs between five versions of a table
    definition are written to `data/out/changes.jsonl`, read back,
    replayed from the first version to the last, and undone again.

The run writes `data/`, works through the 133 jobs in
`data/jobs.jsonl`, and then runs the three programs. `mk.py` wrote
`data/`. In a job, `{"$set": [...]}` stands for a Python set.

`oracle.py` runs the same jobs and programs through dictdiffer itself
and wrote `expected.out`. It uses the `dictdiffer/` package from the
0.9.0 wheel, unchanged. It prints values with `repr()`, except that a
set's items are sorted, since Python prints them in hash order.

The quirks are kept. Some examples:
- Values are compared with `==` and `math.isclose` at a relative
  tolerance of one machine epsilon, so `True` and `1` are the same, and
  so are `0.1 + 0.2` and `0.3`: the diff is empty although the patched
  value is still not `== 0.3`. Two NaNs are never a change.
- `ignore` only applies to dict keys: ignoring a list index has no
  effect.
- With a `path_limit` a limited key's change is reported with its path
  as a list (`['cfg']`), not dotted.
- `patch()` puts the patch's own value into its result. With a
  `path_limit`, the empty container that the diff adds first is the
  same object the later patches add into, so after a patch the diff
  itself shows it full, as `swap()` then prints.
- A `change` patch on a missing dict key adds the key; a list index in a
  dotted path goes through `int()`, so `tags.01`, `tags. 0` and
  `tags.-1` all work.
- When both sides of a merge make the same change, both patches are
  kept. A change is then applied twice, which is harmless, but the same
  removal twice ends in a `KeyError`.

## License

dictdiffer is under the MIT License, in `LICENSE-dictdiffer.txt`:

    Copyright (C) 2013 Fatih Erikli.
    Copyright (C) 2013, 2014 CERN.

That covers `dictdiffer/`.
