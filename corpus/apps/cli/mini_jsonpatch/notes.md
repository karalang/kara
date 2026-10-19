# mini_jsonpatch

JSON Patch (RFC 6902) and JSON Pointer (RFC 6901) as the Python
libraries `jsonpatch` 1.33 and `jsonpointer` 3.0.0 implement them on
Python 3.13. Tools such as kubectl, Ansible and many REST APIs send
changes to a document in this form. The port covers:
- `apply_patch` with all six operations (add, remove, replace, move,
  copy, test). Every exception the library raises is kept, in the order
  it raises them, including the `TypeError`, `ValueError` and
  `IndexError` that leak out of it.
- `JsonPatch.from_string`, whose multidict hook turns a repeated member
  into a list of its values.
- The pointer functions `resolve_pointer`, `set_pointer`, `contains`,
  `join` and `from_parts`, along with the pointer's parts and its path.
- `make_patch`, with the move detection and the list-index fixups in
  `DiffBuilder`.

Python values follow Python's rules. `1 == 1.0 == True` holds, and dict
equality ignores key order. A string is a sequence, so `/kind/0` reads
the first character of `kind`. A pointer that ends in `/-` resolves to
an `EndOfList` object.

`make_patch` walks a dict's added, removed and common keys through
Python sets, so the order of a patch's operations depends on how CPython
lays out a set of strings. The port carries that layout from
`Objects/setobject.c`. Strings are hashed with SipHash-1-3 under the
all-zero key that `PYTHONHASHSEED=0` leaves. Then come the
linear-probe-then-perturb insertion, the resize thresholds, and the
copy path that `a - b` takes when `a` is more than four times larger.
`oracle.py` pins `PYTHONHASHSEED=0` for this reason. Before the port
was written, the set model was checked against real `set` iteration
order over 3000 random key sets, with every set operation `make_patch`
uses.

The run writes `data/` and then works through the 119 jobs in
`data/jobs.txt`:
- It runs the json-patch-tests conformance suite (`suite/tests.json`
  and `suite/spec_tests.json`, 112 cases; see below). For each case it
  prints the result or the error, and whether that agrees with the
  suite.
- It runs the `jsonpatch` command-line tool over a Kubernetes
  Deployment and a Service with 35 kustomize-style patch files. Some are
  overlays. Others break in a particular way: a failing test, a
  leading-zero index, a bad escape, a move into the value's own child,
  `-` in a replace, an index into a string, a patch that is not a list.
- It feeds four patches with repeated members through `from_string`.
- It runs the `jsondiff` tool over a deployment's six revisions and over
  scalar, list and text documents. Each patch it computes is sent
  through its JSON text and applied back, the way a client would apply
  it, to check that it rebuilds the target.
- It replays the revision history as a chain of patches and tallies the
  operations it used.
- It diffs 70 pairs of documents made by seeded random edits, then
  rebuilds each target from its patch.
- It runs the `jsonpointer` tool, `set_pointer` and the pointer helpers
  over a config whose keys contain `/`, `~`, digits, an empty string and
  non-ASCII text.

`oracle.py` runs the same jobs through the real libraries and wrote
`expected.out`.

The libraries' quirks are kept. Some examples:
- `jsonpatch` disagrees with its own conformance suite in four cases.
  `/foo/01` is accepted as index 1, because the pattern
  `0|[1-9][0-9]*$` matches any string that starts with `0`. Replacing a
  list document with an add at `""` raises `TypeError: '>' not supported
  between instances of 'NoneType' and 'int'`. A case the suite disables
  (duplicate `op` members) applies cleanly.
- `0x1` passes the same pattern and then fails in `int()`, so a remove
  at `/containers/0x1` raises `ValueError: invalid literal for int()
  with base 10: '0x1'`. `0_1` is index 1.
- A failed test prints both values as `str()` does, with their types:
  `10 (<class 'int'>) is not equal to tested value 10 (<class 'str'>)`.
  A test of `3.0` against `3` passes, and so does `False` against `0`.
- Removing from a string raises `'str' object doesn't support item
  deletion`. Removing the root of an int raises `'int' object does not
  support item deletion`, worded differently.
- In two of the 70 random pairs, the patch `make_patch` returns does not
  rebuild the target. In pair 52, a replace inside a list element is
  recorded at the element's old index, but an earlier remove has
  already shifted that element down by one.
- The `jsonpointer` tool crashes with `TypeError: Object of type
  EndOfList is not JSON serializable` on `/http/routes/-`.

The port matched `expected.out` on the first run, on all four MIR
backends, with no workarounds and no new ledger rows.

## The conformance suite

`suite/tests.json` and `suite/spec_tests.json` are copied unchanged from
https://github.com/json-patch/json-patch-tests. Their README says:

    Copyright 2014 The Authors

    Licensed under the Apache License, Version 2.0 (the "License");
    you may not use this file except in compliance with the License.
    You may obtain a copy of the License at

        http://www.apache.org/licenses/LICENSE-2.0

    Unless required by applicable law or agreed to in writing, software
    distributed under the License is distributed on an "AS IS" BASIS,
    WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
    See the License for the specific language governing permissions and
    limitations under the License.
