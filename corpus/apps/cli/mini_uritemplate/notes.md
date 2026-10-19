# mini_uritemplate

[uritemplate](https://github.com/python-hyper/uritemplate) 4.1.1, the
RFC 6570 URI Template library that API clients use to turn hypermedia
link templates such as `/repos/{owner}/{repo}/issues{/number}{?state,page}`
into URLs, with an API client report built on it.

The port follows uritemplate's own code:
- `URIVariable`'s parsing of an expression: the operator, the names, the
  explode modifier, the prefix (read with `int()`, so `{x: 4}`, `{x:+2}`
  and `{x:-3}` all work, and `{x:y}` raises `ValueError`) and defaults;
- the four expansion methods, for simple and reserved strings, labels and
  path segments, path parameters, and form-style queries, each with its own
  handling of lists, dicts, lists of pairs, empty values and `None`;
- `urllib.parse.quote` with the unreserved and reserved safe sets;
- `URITemplate.expand` and `partial`, and the `variables()` ordered set.

Underneath are the parts of Python it relies on: `str()` of numbers and
booleans, truthiness (so `0` and `False` take a default and `""` does
not), slicing for prefixes and the `TypeError` it raises on a number, and
sorted dict items. The variables come in as JSON, through a small JSON
reader.

The run writes `data/jobs.tsv`, the operations, and the two inputs of the
API client report. The jobs are:
- the RFC 6570 test suite: the `uritemplate-test` fixtures, 234 cases
  with the expansions the suite accepts, including the failure tests
  (uritemplate expands most of them rather than failing);
- edge cases the suite does not reach: numbers, booleans, `None`,
  defaults, prefixes on numbers, negative prefixes, lists of pairs, the
  reserved operators `|`, `!` and `@`, unmatched braces and non-ASCII
  text;
- `parse`, `variables` and `partial` on templates;
- `api`: 44 calls through a forge's link templates from `data/api.json`,
  read from `data/requests.tsv`. Each URL is expanded at once and again
  through a client that bound some variables first with `partial()`; the
  report shows the calls where that changes the URL (`partial` replaces a
  whole expression once any of its variables has a value, so
  `{?q}{&sort,order,page}` bound with `sort` loses `page`), then the calls
  per link and per host.

The output matches `oracle.py`, which runs the same jobs through
uritemplate. The Python files are vendored unchanged in `uritemplate/`
(BSD-3-Clause or Apache-2.0, see `LICENSE-uritemplate`). The test suite in
`suite/` is from
[uri-templates/uritemplate-test](https://github.com/uri-templates/uritemplate-test)
(Apache-2.0), as uritemplate's own tests vendor it. Run
`python3 mk.py && python3 oracle.py` to regenerate `expected.out`.
