# mini_urlparse

URLs as Python 3.13's `urllib.parse` takes them apart and puts them back
together. Every web client, crawler, proxy and log analyzer needs this.
The program ports `urlsplit`, `urlparse`, `urlunsplit`, `urlunparse`,
`urljoin`, `urldefrag`, the quoting functions and the query-string
functions, together with the bracketed-host checks that `urlsplit`
borrows from `ipaddress`. It prints exactly what Python prints,
including the reprs and every `ValueError`.

The run has five parts:
- **split**: each URL in `data/urls.txt` goes through `urlsplit` and
  `urlparse`. The program prints `username`, `password`, `hostname` and
  `port`, the `urlunsplit` round trip and `urldefrag`. The cases cover
  scheme detection (`path:with:colons`, `1http:`), `;params` for the
  schemes that have them, leading control characters and embedded
  tabs and line ends, IPv6 hosts with zones and IPv4 tails, IPvFuture,
  every malformed bracket and bad port, and `mailto:`, `urn:`, `file:`
  and `git+ssh:` URLs.
- **join**: `urljoin` against several bases, including all of RFC 3986
  section 5.4's normal and abnormal examples, and schemes that
  `urljoin` does not resolve.
- **quote**: `quote`, `quote_plus`, `unquote` and `unquote_plus` on
  ASCII, accented, CJK and emoji text. Malformed escapes are kept, and
  bad UTF-8 decodes to one U+FFFD per maximal ill-formed subpart, as
  CPython's decoder does it.
- **query**: `parse_qsl` with blank values, strict parsing and `;` as
  the separator, `parse_qs`, and `urlencode` with `quote_plus`, with
  `quote`, and with `doseq`.
- **report**: reads `data/access.log`, a web server log in the combined
  format. It resolves each request target against the site, flagging
  the `//host/...` target that leaves the site. It canonicalizes request
  and referer URLs (host lower-cased, default port dropped, path
  re-quoted, query sorted, fragment dropped). It counts pages, query
  parameters, referring hosts and search terms, writes
  `data/out/pages.txt` and `data/out/terms.txt`, and reads them back.

Not supported: bytes input, `allow_fragments=False`, `encoding` and
`errors` arguments other than the defaults, `max_num_fields`, and the
NFKC check that `urlsplit` makes on a non-ASCII netloc. All netlocs in
the data are ASCII.

**Oracle.** `expected.out` is the output of `oracle.py`, the same
program written with `urllib.parse`. Run it from this directory after
`mk.py` writes `data/`.

## Edits from mini_urlparse.orig.kara to mini_urlparse.kara

None. `karac check` passed on the first try, and the output matched
`expected.out` on the first run under `__mir-run`, `__mir-llvm-run` and
`__mir-llvm-asan`. No compiler gap was found.

Written 2026-10-15 by the real-world apps thread; karac at eb605b2e5b.
