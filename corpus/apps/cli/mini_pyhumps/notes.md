# mini_pyhumps

A port of [pyhumps](https://github.com/nficano/humps) 3.8.0 (Unlicense /
public domain, Nick Ficano) — the case-conversion library a JSON
(de)serializer uses to translate API field names between `snake_case`,
`camelCase`, `PascalCase` and `kebab-case`.

## What it does

`mk.py` writes `data/jobs.tsv`, one `op \t identifier` per line. `oracle.py`
runs each identifier through `vendor/humps/` and prints `expected.out`: the
converted string for a conversion op, or `true` / `false` for an `is_*`
predicate. `mini_pyhumps.kara` reimplements every op and must match
byte-for-byte on each backend.

Ops covered:

- **camelize** — snake/kebab → `camelCase` (and the quirk that an acronym
  start like `HTTP_status` stays `HTTPStatus`).
- **decamelize** / **depascalize** — camel/pascal → `snake_case`, with the
  acronym fold that turns `getHTTPResponseCode` into
  `get_http_response_code` and `userID` into `user_id`.
- **pascalize** — snake/kebab → `PascalCase`.
- **kebabize** — camel/pascal → `kebab-case`, and snake → `kebab-case`.
- **dekebabize** — `kebab-case` → `snake_case`.
- **is_camelcase** / **is_snakecase** / **is_pascalcase** / **is_kebabcase** —
  each a round-trip identity check (`s == convert(s)`), including the
  snake/kebab disambiguation.

pyhumps drives these with four regexes — an acronym-folding `ACRONYM_RE`, a
word-splitting `SPLIT_RE`, the camel/pascal first-letter rule, and an
underscore-collapsing `UNDERSCORE_RE`. The port reimplements each as an
explicit byte scan over the identifier, so the control flow is visible while
the output stays identical to the library's, quirks included (a lone leading
`_` survives; `foo_Bar` decamelizes to `foo__bar` with a doubled underscore).

## Scope

Identifiers here are ASCII, so the byte-length and case operations are plain
ASCII case folding. The library's dict- and list-of-dict key walking
(`_process_keys`) and non-ASCII case mapping are the boundaries left out —
this port covers the string API, which is where the regex logic lives.

## Running

```bash
karac run mini_pyhumps.kara            # interpret
karac build --mir mini_pyhumps.kara    # native binary
python3 -s -P -B mk.py                 # regenerate data/jobs.tsv
python3 -I oracle.py                   # regenerate expected.out
```
