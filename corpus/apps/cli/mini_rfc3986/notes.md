# mini_rfc3986

rfc3986 2.0.0, the strict RFC 3986 URI library (with RFC 3987 IRIs)
that requests, urllib3 and many API clients use for the URI handling
`urllib.parse` does not do. The port covers:
- `URIReference` and `IRIReference`:
  - parsing, and the authority's userinfo, host and port;
  - `is_absolute`, the deprecated `is_valid` with its `require_*`
    flags, normalization and equality, which falls back to comparing
    normalized forms;
  - reference resolution as in RFC 3986 §5.2, strict or not;
  - `copy_with`, and IRI-to-URI encoding with IDNA 2008 hosts
    (strict, STD3 rules, through the idna package).
- `ParseResult` and `ParseResultBytes`:
  - from a string, strict or lenient, normalized or lazily;
  - from parts;
  - `copy_with`, `encode` and `unsplit`, with or without Python's own
    IDNA 2003 `idna` codec for the host.
- `URIBuilder`, with urllib's `quote`, `quote_plus`, `urlencode` and
  `parse_qsl` written out.
- `Validator`: allowed schemes, hosts and ports, forbidding passwords,
  and required and validated components.
- The normalizers: scheme, authority, host (with IPv6 zone IDs), path
  (with dot-segment removal), query, fragment, percent-escape case,
  `encode_component`, and `split_authority`.

The patterns of `rfc3986.misc` (the authority, host, IPv4, IPv6, path,
query, scheme and absolute-URI matchers) run on mini_re, as in
mini_soupsieve. Their source is taken from the vendored modules by
`gen_kara.py`. URI_MATCHER, the regex of appendix B, is written out:
all of its parts are optional, so a scan finds the same groups.
Python's `int()` is written out too, because the builder and the
validator take ports as strings: whitespace, a sign, underscores and
any Unicode decimal digits are accepted, so `" 8080 "`, `"+80"`, `"8_0"`
and `"８０"` are all ports.

The run writes `data/` and then works through the 1195 jobs in
`data/jobs.jsonl`, 4446 steps in all. A job builds a value and calls
methods on it. They come from rfc3986's own test suite:
- the fixtures of `conftest.py`: 20 valid and 7 invalid hosts in up to
  11 URI templates (the IPv6 literals in 4), scheme and host case
  variants;
- the literals of the test modules and others of the same kinds;
- the normal and abnormal examples of RFC 3986 §5.4, resolved against
  `http://a/b/c/d;p?q`, and more bases;
- 22 IRIs (German, Greek, Sinhala and Persian with joiners, Japanese,
  Hebrew, an unassigned code point);
- the builder and validator tests, and each normalizer.

After the jobs, a webhook registry for five tenants is checked against
a policy: https only, ports 443 and 8443, no passwords, a scheme and a
host required, and every component valid. Relative callbacks are
resolved against the tenant's base, and equal endpoints are merged.

`oracle.py` runs the same jobs and the same report through rfc3986
itself and wrote `expected.out`. It uses the Python files of the 2.0.0
sdist in `rfc3986/` and of idna 3.20 in `idna/`, unchanged.

The quirks are kept. Some examples:
- A backslash ends the authority, so
  `https://user:pass@xdavidhu.me\test.corp.google.com:8080/...` has the
  host `xdavidhu.me`, and the backslash is percent-encoded into the
  path.
- `urlparse("http://[::1")` raises `ValueError: not enough values to
  unpack`: the lenient fallback splits the authority on `]`.
- `[::1%eth0]` is a valid host only to the RFC 6874 matcher. The
  normalizer encodes its `%` as `%25`.
- `https://1.2.3/` is valid: three dotted numbers are a registered name.
- `add_port(0)` is allowed, but a port of 0 in a parse result becomes
  `None`.

Licenses: rfc3986 is Apache-2.0 (`LICENSE-rfc3986`). idna is
BSD-3-Clause (`LICENSE-idna`).
