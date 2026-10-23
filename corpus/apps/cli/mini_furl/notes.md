# mini_furl

A port of [furl](https://github.com/gruns/furl) 2.1.4 (Unlicense; Ansgar
Grunseid), the URL library HTTP clients, crawlers and API wrappers use to
take a URL apart, change one piece and write it back. A `furl` holds the
scheme, username, password, host, port, a `Path`, a `Query` and a
`Fragment` (itself a path and a query), and each can be added to, set or
removed.

```
> !api data/api.tsv
  https://api.github.com/repos/gruns/furl/issues?state=open&labels=bug,help+wanted&per_page=50
  https://api.github.com/repos/gruns/furl/issues?state=open;labels=bug%2Chelp+wanted;per_page=50;page=2
```

## What it does

`mk.py` writes 188 jobs and 3 programs to `data/jobs.jsonl`, one JSON
array a line, in 13 sections, and the programs' three input files to
`data/`. A job is a URL (or the constructor's keyword arguments) and then
484 operations in all: `add`, `set` and `remove` with their keyword
arguments, `join`, `/`, `copy`, `load`, attribute assignment, the `Path`,
`Query` and `Fragment` methods, `args[...]`, `tostr()` and
`query.encode()` with their quoting options, attribute reads and
`asdict()`. `oracle.py` runs each against the vendored library and prints
the URL after every operation, the value of every read, or
`error <class>: <message>` when the library raises (40 times);
`mini_furl.kara` runs the same jobs.

- **Parsing** covers 74 URLs: every component, default ports for 46
  schemes, unknown and invalid schemes, `mailto:`/`tel:`/`urn:` with no
  netloc, `//` and `///` forms, IPv6 and IPvFuture hosts, userinfo with
  empty parts, percent escapes that are and are not well formed, raw
  spaces, non-ASCII paths and queries, IDNA hosts both ways and a host
  label too long for the idna codec. Twenty more test the limits, and 16
  of them fail: bad ports, malformed brackets and hosts furl refuses.
- **Building** runs furl's own semantics: `join_path_segments` and
  `remove_path_segments`, `set()` putting the URL back when a part
  fails, an absolute path that is read-only once there is a netloc,
  `normalize()` over `posixpath.normpath`, `omdict1D`'s `update` and
  `updateall` (a list is several values, `[]` removes a key), a query
  pair with no `=`, an empty key's unquoted `=`, `dont_quote` limited to
  the characters valid in a query, and a fragment's `?` separator.
- **`canon`** is a crawler's link canonicalizer: it drops `utm_*`,
  `fbclid` and `gclid`, the fragment and dot segments from 18 links and
  counts the distinct pages. **`redirects`** follows five recorded chains
  of `Location` headers with `join()`. **`api`** builds an API client's
  request URLs from a base, a resource path and each call's parameters,
  then a second page with `;` between the parameters.

## How the port works

- **URL syntax** comes from [mini_urlparse](../mini_urlparse), the port
  of Python 3.13's `urllib.parse`: furl splits every URL as if its scheme
  were `http`, so `urlsplit` and `urljoin` behave the same for every
  scheme, then puts the scheme back and tells a missing netloc (`None`)
  from an empty one (`''`). Its `quote` and `unquote` do the escaping.
- **The query** is `omdict1D` as one vector of `(key, value)` items.
  furl only ever reads the items in order or one key's values, so the
  linked item list and key map of orderedmultidict are not needed. A
  value keeps what the caller passed, `None`, a string, an int or a
  bool, and `str()` makes the text when the query is written.
- **Python's quirks are kept where furl's output shows them.** An empty
  path's `isabsolute` is the empty list `[]`, because furl assigns
  `segments and segments[0] == ''`. A URL path's `_force_absolute` sees
  the path's old segments while a new path loads. `copy()` and `/` build
  a new furl from the URL text, so a fragment loses `separator=False`.
  `True == 1` holds when a query value is removed.
- **IDNA** follows the `idna` codec: an ASCII host is checked for labels
  of 64 characters or more and kept, any other gets each non-ASCII label
  as `xn--` and Punycode from [mini_idna](../mini_idna). A host starting
  with `xn--` is decoded when it is set.

## Compiler gaps

None. `karac check` asked for one `own`, two call-site `mut` markers and
one `ref` binding, and the interpreter then matched the oracle byte for
byte on its first run, as did the LLVM, ASan and native builds.

## Scope

- nameprep is the identity, which holds for the lower-case letters the
  non-ASCII hosts here have once furl lowers them, and the `xn--` labels
  decode and round-trip. An invalid one raises in Python and is not
  covered.
- In a path segment, characters past ASCII count as word characters for
  furl's well-formedness check. That changes nothing here: a segment
  with such a character and a percent escape does not occur.
- `strict=True` only adds warnings, and keys are strings.

## Running

```bash
karac run mini_furl.kara                     # interpret
karac build --mir mini_furl.kara             # native binary
python3 -s -P -B mk.py                       # regenerate data/
python3 -I -B oracle.py > expected.out       # regenerate expected.out
python3 -I -B gen_kara.py                    # regenerate the embedded data
```
