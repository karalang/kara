# mini_jsonref

A port of [jsonref](https://github.com/gazpachoking/jsonref) 1.1.0 (MIT, Chase
Sterling) — the library that resolves JSON Reference objects
(`{"$ref": "#/a/b"}`) inside a JSON document, the way JSON Schema tooling and
OpenAPI / API-spec loaders inline their `$ref`s. `replace_refs` returns a deep
copy of a document with every reference replaced by the data it points at,
resolved against the same document.

## What it does

`mk.py` writes `data/jobs.tsv`, one job per line:

- `resolve <merge> <json-document>` → the document with every local reference
  resolved, serialized, or `ERROR: <message>`.

`<merge>` is the `merge_props` flag: with `1`, a reference object's extra keys
(anything besides `$ref`) are merged into the resolved mapping, the reference's
own keys winning. The resolution runs eagerly — the library's `proxies=False`
mode, which materializes every reference rather than returning a lazy proxy.
`oracle.py` runs each job through the vendored `jsonref` and prints
`expected.out`; `mini_jsonref.kara` reproduces it byte-for-byte on every
backend.

## How the port works

A JSON value needs no recursive type: values live in a flat **arena** of nodes
indexed by `i64` (tag `0` null, `1` bool, `2` int, `3` str, `4` list, `5`
dict). A node is a reference when it is a dict whose `$ref` value is a string.

`resolve` walks the parsed document and rebuilds it:

- a plain dict or list is rebuilt with each child resolved;
- a reference's fragment (`#/…`) is **%-decoded**, split on `/`, and each part
  has its `~1`→`/` and `~0`→`~` escapes expanded, then walked against the
  **original** document — keys by name, array elements by integer index;
- the node it lands on is itself resolved, so a reference to a reference is
  followed through to the end;
- with `merge_props`, a reference's extra keys are layered onto the resolved
  mapping (last writer wins, reference keys appended after the target's).

An `active` stack of in-flight reference nodes, plus the "points at the
reference node itself" check, reproduce the library's two error messages: an
**unresolvable JSON pointer** and a reference that **refers directly to
itself**, each rendered exactly as `JsonRefError` does.

## Scope

Local fragment references only — no remote `http(s)` loader, no JSON Schema
`$id` base-URI changes, and no lazy proxies (the eager `proxies=False` path is
what is ported). A bare `#` pointer and a top-level reference are out of scope:
the library's `proxies=False` path does not materialize either (a top-level
`$ref` is left as an un-dumpable proxy). The job strings avoid JSON escapes, so
serialization emits none.

## Running

```bash
karac run mini_jsonref.kara                 # interpret
karac build --mir mini_jsonref.kara         # native binary
python3 -s -P -B mk.py                         # regenerate data/jobs.tsv
python3 -I oracle.py                            # regenerate expected.out
python3 -s -P -B gen_kara.py                    # regenerate the embedded jobs block
```
