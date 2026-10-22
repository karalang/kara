# mini_specifiers

A port of [packaging](https://github.com/pypa/packaging) 26.3's
`packaging.specifiers` (Apache-2.0 OR BSD-2-Clause, Donald Stufft &
contributors) — the `SpecifierSet` that resolves every `pip install foo>=1.2,<2`.
A specifier set answers two questions: does a version satisfy a requirement
(`SpecifierSet.contains`), and which of a list of versions do
(`SpecifierSet.filter`).

## What it does

`mk.py` writes `data/jobs.tsv`, one tab-separated job per line:

- `contains <specifier-set> <version> <premode>` → `True` / `False`;
- `filter <specifier-set> <premode> <v1,v2,...>` → the surviving versions, in
  order, space-joined (or `(none)`).

`<premode>` is `none|true|false` — the `prereleases=` argument (`None`
autodetects per PEP 440). `oracle.py` runs each job through the vendored
`packaging` and prints `expected.out`; `mini_specifiers.kara` reproduces it
byte-for-byte on every backend.

## How the port works

Everything rests on a from-scratch PEP 440 `Version`: it parses the epoch
(`N!`), the dotted release, an `a`/`b`/`rc` pre-release (normalizing
`alpha`/`beta`/`c`/`pre`/`preview`), a post-release (normalizing `rev`/`r` and
the implicit `-N` form), and a dev-release, accepting the leading `v`. The
comparison is PEP 440's key: trailing release zeros are ignored (so
`1.2.3 == 1.2.3.0`) and a six-field suffix `(pre, pre_n, post?, post_n, dev?,
dev_n)` orders dev < a < b < rc < final and final < post.

Each operator is then pure range membership, mirroring packaging's internal
range engine:

- `>=` / `<=` / `==` / `!=` are direct key comparisons (`==` is
  zero-insensitive);
- `<V` excludes `V` and — when `V` is final — its own pre-releases
  (bound `V.dev0`); `>V` excludes `V` and its post-releases;
- `==V.*` / `!=V.*` are the half-open prefix range `[V.dev0, V.next.dev0)` and
  its complement;
- `~=V` is `>=V` intersected with the prefix of `V` minus its last component;
- `===V` is an unparsed, case-insensitive string identity.

The pre-release policy sits on top: `contains` admits a pre-release unless
`prereleases=False`; `filter` yields pre-releases only when no final release
matches (buffering them to the end), and a set is pushed into pre mode when any
of its specifiers names a pre-release.

## Scope

Local versions (`1.0+ubuntu1`) are out of scope — they appear in `==`/`!=`
specifiers rarely and need the local-family boundary algebra; such an input
simply fails to parse here, so a standard operator reports no match (which is
also packaging's answer for an unparsable input). The set relations
(`is_subset` / `is_disjoint` / `is_unsatisfiable`, new in 26.1–26.3) are not
ported; `contains` and `filter` are the resolution core.

## Running

```bash
karac run mini_specifiers.kara              # interpret
karac build --mir mini_specifiers.kara      # native binary
python3 -s -P -B mk.py                        # regenerate data/jobs.tsv
python3 -I oracle.py                           # regenerate expected.out
python3 -s -P -B gen_kara.py                   # regenerate the embedded jobs block
```
