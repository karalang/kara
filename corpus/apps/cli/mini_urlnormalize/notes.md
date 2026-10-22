# mini_urlnormalize

A port of [url-normalize](https://github.com/niksite/url-normalize) 1.4.3 (MIT,
Nikolay Panov), the library that canonicalises a URL — lower-casing the scheme
and host, stripping a default port, collapsing dot-segments, decoding
unreserved percent-escapes, sorting the query, and dropping `utm_*` tracking
parameters — so two spellings of the same resource compare equal. It is used in
crawlers, caches, dedup pipelines and link shorteners.

## What it does

`mk.py` writes `data/jobs.tsv`, one URL per line. `oracle.py` runs each through
the vendored library (`vendor/url_normalize/`) to produce `expected.out`.
`mini_urlnormalize.kara` reimplements the normalisation pipeline and must match
that output byte-for-byte on every backend.

The port follows the library's pipeline step for step: `provide_scheme`
(default `http` for a bare or protocol-relative host), `generic_url_cleanup`
(shebang `#!` → `?_escaped_fragment_=`, `utm_*` removal, trailing `&? `
cleanup), `split_url` into scheme/userinfo/host/port/path/query/fragment, the
per-part normalisers (host lower-cased and trailing dot stripped, default port
removed per scheme, dot-segments collapsed, empty path → `/`, query sorted,
percent-encoding uppercased with unreserved bytes decoded), and the rebuild.

## Scope

Inputs are ASCII, so NFC normalisation and IDNA host encoding are no-ops — the
boundary url-normalize itself draws for non-ASCII input, matched here by keeping
the corpus ASCII rather than by working around a compiler gap. Percent-encoding
is handled at the byte level (uppercase hex, unreserved decode), which is the
part that carries most of the library's behaviour. The default-charset and
`sort_query` toggles use their library defaults.

## Running

```bash
karac run mini_urlnormalize.kara          # interpret
karac build --mir mini_urlnormalize.kara  # native binary
python3 -s -P -B mk.py                     # regenerate data/jobs.tsv
python3 -I oracle.py                        # regenerate expected.out
```
