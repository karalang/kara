# mini_jsonschema

A JSON Schema validator (draft 2020-12) that reports what Python's
jsonschema 4.26 reports. Services validate request bodies and webhooks
against schemas, CI checks config files, Kubernetes manifests and
workflow files before they ship, and package tools check manifests. Each
of those has to say exactly which field is wrong and why, and point at
the right place in a document that may not even be valid JSON.

The inputs in `data/` were made by `mk.py`:
- Five schemas from real kinds of use: an orders API (`$ref`s into
  `$defs`, an `if`/`then`/`else` on the shipping country,
  `dependentRequired`, prices as `multipleOf: 0.01`), a `package.json`
  subset (npm's name pattern, semver, `anyOf` licences, `oneOf` bin
  entries, `patternProperties` dependencies), a Kubernetes Deployment
  subset (DNS names, label keys through `propertyNames`, `contains` with
  `maxContains`, `oneOf` env vars, int-or-percent), a service config
  (`prefixItems` with `items: false`, a `false` schema for a removed key,
  `not`, `const` objects, `uniqueItems` across numbers and booleans) and a
  GitHub Actions workflow subset (`oneOf` triggers, job ids by pattern,
  steps that need `uses` or `run` but not both).
- 22 documents for those schemas, valid and invalid, and 10 that are not
  JSON (a missing comma, a trailing comma, single quotes, a raw newline in
  a string, extra data, an empty file, a bad escape, a leading zero, a
  missing colon and Python's `True`).

The program runs the `validate` commands in `data/jobs.txt`. Each prints
every error with its JSON path, message and schema path, sorted, with the
sub-errors of `anyOf` and `oneOf`, or with `--best`, the single error
jsonschema's `best_match` picks. It has its own JSON decoder with Python's
`json` messages and positions, Python's repr of the values (which every
jsonschema message uses), and a backtracking matcher for the regex subset
the patterns use. It shows jsonschema's own behaviour exactly, including
that `19.99` is not a multiple of `0.01` in floating point, that `1` and
`1.0` are equal but `true` and `1` are not, and that a `false` schema's
error loses its path.

Not supported: remote `$ref`s and `$id`-relative resolution,
`$dynamicRef`, `unevaluatedProperties` and `unevaluatedItems`, format
assertion (jsonschema doesn't assert formats by default either), checking
a schema against the metaschema, and regex features beyond the ones
listed in the source (`\d`, `\w` and `\s` are ASCII here, but Unicode in
Python).

**Oracle.** `expected.out` is the output of `oracle.py`, the same program
in Python without the jsonschema, json or re modules. `real.py` is the
same tool written on the real jsonschema 4.26 and Python's `json` module.
Its output is byte-identical to `expected.out`.

## Edits from mini_jsonschema.orig.kara to mini_jsonschema.kara

Compiler gaps:
- B-2026-10-12-6: `f64.parse` and `i64.parse` return an `Option` on v2,
  not the spec's `Result`, so the number decoder matches `Some`/`None`.
- B-2026-10-14-70: check refuses `v.a.nodes[..]` as a chained field
  receiver in four places, so `resolve`, `errors` and `descend` bind
  `let a = ref v.a;` first.

After that, the output matched `expected.out` on `__mir-run`,
`__mir-llvm-run` and `__mir-llvm-asan` on the first run.

Written 2026-10-15 by the real-world apps thread; karac at 7cb1840d66.
