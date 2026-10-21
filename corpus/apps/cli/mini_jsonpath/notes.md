# mini_jsonpath

jsonpath-ng 1.7.0 by Tomas Aparicio and others (it started as Kenn
Knowles's jsonpath-rw): the JSONPath library Python tools use to pull
values out of JSON documents and change them. Configuration tools, API
clients and test suites query responses with it, and it carries the
`jsonpath` command. The port covers:
- both grammars: `jsonpath_ng.parse`, and `jsonpath_ng.ext.parse` with
  filters (`[?(@.price < 10 & @.category == fiction)]`, `=~`), sorting
  (`[\price]`, `[/category,\price]`), arithmetic (`$.a.qty * $.a.price`)
  and the named operators `` `len` ``, `` `keys` ``, `` `path` ``,
  `` `sorted` ``, `` `split(...)` ``, `` `sub(/re/, repl)` `` and
  `` `str()` ``;
- the paths: `$`, fields (`*`, quoted, comma lists), `..`, indexes,
  slices with steps, `where`, `wherenot`, `|`, `&`, `` `this` `` and
  `` `parent` ``, with the full path to each match;
- `find`, `update` (with a value or a function), `update_or_create`
  and `find_or_create` (with the `LIST_KEY` lists they build), and
  `filter`, which change the document in place;
- the lexer's and parser's errors, at the line and column they report;
- the `jsonpath` command (`jsonpath_ng/bin/jsonpath.py`).

`mk.py` writes `data/`:
- `docs/`: a bookstore, a Kubernetes pod list, GitHub issues, shop
  orders, a `package.json`, GeoJSON features, an org chart, host
  metrics, a service config and log lines;
- `jobs.tsv`: 395 jobs. `parse` prints the tree an expression parses
  to; `find` prints each match's full path and value; `update`,
  `updatefn`, `create`, `findcreate` and `filter` print the document
  after the change; `cli` runs the command over some files.

Each job prints `-- ` and its columns. A Python exception prints `! `,
its class and its message. Values are printed with `repr` (with
`LIST_KEY` for the key `update_or_create` files a new list under), and
trees the way the classes' reprs write them (`Union`, `Intersect`,
`Where` and `WhereNot` have no repr, so they are written like `Child`).

`oracle.py` runs the jobs through jsonpath-ng itself (the copy in
`jsonpath_ng/`, unchanged, with the PLY 3.11 it is built on in `ply/`)
and wrote `expected.out`. `gen_kara.py` writes the generated section at
the end of `mini_jsonpath.kara`: the LALR tables PLY builds for the two
grammars, and `data/`.

The parser is PLY's LALR driver over those tables, so precedence,
conflicts and the token an error is reported at are PLY's. The lexer
tries PLY's rules in PLY's order. The documents live in one heap, so a
list two paths reach is the same object to both, as in Python. The
`=~` filter and `sub` use the regex engine of `mini_re` (Python's
`re`).

The quirks are kept. Some examples:
- `foo.bar | foo.baz` groups as `foo.(bar | foo).baz`;
- in the ext grammar, `$.trueValue` is a parse error, because `true`
  lexes as a boolean, and `/` is always a sort direction, so
  `$.a / 2` is a parse error too;
- a backquoted operator drops the backslash of an escape, so
  `` `sub(/(\d+)ms/, \1 ms)` `` looks for `(d+)ms` and writes `1 ms`
  (the jobs also use the doubled form, `\\d`);
- `sub(/ /, _)` matches the empty string, because the regex is
  stripped;
- a filter comparing with a number converts with `int()`, so
  `[?(@.disk > 71)]` matches `"72"` and a `None` raises a `TypeError`;
- `` `sorted` `` on a string gives back the datum, which the next step
  cannot iterate;
- a `where` update applies each match's last step to the whole
  document, and a `where` filter applies it to the match itself.

The jsonpath-ng code is under the Apache License 2.0
(`LICENSE-jsonpath-ng.txt`); its `bin/jsonpath.py` is under the WTFPL,
as its header says. PLY is under the BSD license in `LICENSE-ply.txt`.
