# mini_glom

A port of [glom](https://github.com/mahmoud/glom) 25.12.0 (BSD-3-Clause,
Mahmoud Hashemi) — the library that pulls values out of nested data and
restructures them with a declarative spec, used in config loading, API-response
shaping and data pipelines. `glom(target, spec)` navigates `target` according
to `spec` and returns the result, or raises a `GlomError`.

## What it does

`mk.py` writes `data/jobs.tsv`, one job per line:

    <spec-dsl> \t <json-target>

The spec DSL — shared by `oracle.py` and the port — encodes the glom specs this
port supports:

- `P <path>` → `glom(t, "<path>")`, dotted path access (`a.b.c`, `users.0.name`);
- `V <json>` → `glom(t, Val(<json>))`, a literal value, ignoring the target;
- `D k=<path>;k2=<path>` → `glom(t, {k: "<path>", ...})`, a dict restructure;
- `I <path>` → `glom(t, ["<path>"])`, map a path over each element of a list;
- `T <outer> | <inner>` → `glom(t, ("<outer>", ["<inner>"]))`, navigate then map;
- `C p1 || p2 [=> <json>]` → `glom(t, Coalesce(p1, p2, [default=<json>]))`.

`oracle.py` runs each job through the vendored `glom` and prints `expected.out`;
`mini_glom.kara` reproduces it byte-for-byte on every backend. A success is
serialized with `json.dumps`' default separators; a failure is glom's final,
most specific error line.

## How the port works

A JSON value lives in a flat **arena** of nodes indexed by `i64` (tag `0` null,
`1` bool, `2` int, `3` str, `4` list, `5` dict). Path access mirrors glom's
three target cases exactly:

- a **dict** is keyed — a miss raises `KeyError`;
- a **list** is indexed by the integer segment — out of range raises `IndexError`;
- any **scalar** falls to attribute access — raising `AttributeError`.

On failure the port rebuilds glom's summary line — `glom.core.PathAccessError:
could not access …` or `glom.core.CoalesceError: …` — including the Python
`repr()` of the failing segment, the full `Path(...)`, and the underlying
exception (whose message, when it contains a `'`, switches to double quotes, as
Python's own `repr` does).

## Scope

The read and restructure forms above. glom's `T`-expression objects, `Spec`,
`Match`/`Check`, the `Assign`/`Delete` mutation API, `Fold`/`Flatten`/`Iter`
streaming, `Call`/`Invoke`, and the multi-line **Target-spec trace** header
printed above the summary line (it renders the Python `repr` of the whole
target) are out of scope — the port reproduces the informative final line, not
the pretty-printed trace. Job strings avoid JSON escapes.

The oracle imports the real `glom`, whose transitive dependencies are `boltons`
and `face`; install them (`pip install glom`) to regenerate `expected.out`. The
vendored `glom/` tree under `vendor/` is the ported library's reference source.

## Running

```bash
karac run mini_glom.kara                    # interpret
karac build --mir mini_glom.kara            # native binary
python3 -s -P -B mk.py                        # regenerate data/jobs.tsv
python3 -I oracle.py                           # regenerate expected.out (needs glom installed)
python3 -s -P -B gen_kara.py                   # regenerate the embedded jobs block
```
