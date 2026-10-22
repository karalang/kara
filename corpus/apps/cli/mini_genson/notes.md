# mini_genson

A port of the inference core of [genson](https://pypi.org/project/genson/)
1.4.0 (MIT; Jon "Wolverson" Wolverton), the library that **builds a JSON Schema
from example JSON**. Point it at a few sample payloads and it produces a draft
schema you can hand to an API gateway, a data-pipeline validator or a
documentation tool — the inverse of a schema validator.

## What it does

`mk.py` writes `data/jobs.tsv`: one schema-inference job per line. Each line is
a **tab-separated list of JSON sample documents**; the job feeds them all to one
`SchemaBuilder` (via `add_object`) and prints the inferred schema as JSON.

```
42                                  -> {"type": "integer"}
{"a": 1, "b": 2}  {"a": 3}          -> properties a,b; required ["a"]
{"v": 1}  {"v": "x"}                -> v: {"type": ["integer", "string"]}
{"v": 1}  {"v": {"k": 2}}           -> v: {"anyOf": [ ... ]}
```

(each with the `"$schema"` keyword prepended, as the library's default does).

`oracle.py` replays every job through the vendored `genson` library and writes
`expected.out`; `mini_genson.kara` reproduces it byte-for-byte — matching the
library's own `to_json()` (Python's `json.dumps` defaults) exactly — on every
backend.

## How the port works

The port parses each JSON document into a small value tree, then runs genson's
builder over an **arena of schema nodes** (children referenced by index):

- **Strategies in first-seen order.** Each node records which of the six
  strategies — null, boolean, number, string, array, object — it has seen, in
  the order they first appeared. That order drives how `anyOf` branches are laid
  out, exactly as genson's `active_strategies` list does.
- **Number.** An integer infers `"integer"`; once any float is seen at the same
  position it becomes `"number"` — the value itself is never emitted, so no
  float formatting is involved.
- **Arrays** merge every element into one shared *items* node.
- **Objects** union their keys (properties, in first-seen order) and
  **intersect** their key sets into `required` (so a key missing from any sample
  drops out of `required`).
- **`to_schema`** turns a node into a fragment: scalar types that reduce to a
  bare `{"type": …}` are folded into a single sorted `type` list, and anything
  richer (objects, arrays-with-items, or a mix) goes into `anyOf` — matching
  genson's fold precisely. `required` and the `type` list are sorted the way
  Python's `sorted()` orders the strings.

The serializer matches `json.dumps`' default output byte for byte: `", "` and
`": "` separators, and the standard string escapes.

## Scope

- **Inference only (`add_object`).** The `add_schema` path (merging an existing
  schema in) and the `Enum` strategy — which genson only ever selects from an
  input schema, never from a sample — are out of scope.
- **ASCII keys and strings.** `json.dumps` escapes non-ASCII as `\uXXXX`; the
  port keeps keys and string values ASCII (quote, backslash and control-char
  escapes are reproduced), so no `\u` escaping of higher code points is
  exercised.
- Float JSON numbers are inferred as `"number"` exactly as the library does;
  their values never reach the output.

The port was checked against the library over the job list on every backend,
and separately (during development) over 40,000 random JSON sample sets — mixed
scalar types, nested objects and arrays, key unions and required-set
intersections.

The vendored library is pure Python and imports only the standard library.

## Running

```bash
karac run mini_genson.kara               # interpret
karac build --mir mini_genson.kara       # native binary
python3 -s -P -B mk.py                     # regenerate data/jobs.tsv
python3 -I oracle.py                        # regenerate expected.out
python3 -s -P -B gen_kara.py                # regenerate the embedded jobs
```
