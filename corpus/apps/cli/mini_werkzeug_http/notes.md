# mini_werkzeug_http

A document store whose HTTP header handling is ported from Werkzeug 3.1.3
(BSD): `http.py`, `sansio/http.py` and the header datastructures.
- `quote_header_value` / `unquote_header_value`, `parse_list_header`,
  `parse_dict_header` and `parse_options_header`, with RFC 2231
  continuations and `charset''` decoding, and `dump_options_header`;
- `parse_accept_header` into `Accept`, `MIMEAccept`, `LanguageAccept` and
  `CharsetAccept`: the stable sort by quality and specificity,
  `quality()`, `in`, `to_header()`, `repr`, `best` and `best_match`, with
  MIME wildcards, language fallback through the primary tag, and charset
  names normalized as `codecs.lookup` names them;
- request and response `Cache-Control`, `Content-Security-Policy` and a
  `HeaderSet` (`Vary`, `Allow`);
- `Range`, `ContentRange` and `IfRange`, with `range_for_length` and
  `make_content_range`;
- `ETags` (strong, weak, `*`) and `generate_etag`, with the frozenset
  order the `repr` shows;
- `parse_date` over `email._parseaddr._parsedate_tz` (two-digit years,
  year 0, named zones, offsets it rejects), `http_date` and `parse_age`;
- `parse_cookie` and `dump_cookie` with their quoting, `Max-Age`,
  `Expires`, `SameSite` and `Partitioned`;
- `is_resource_modified` and `make_conditional`'s 206 / 304 / 412 / 416
  logic.

`ETags.to_header` and `repr` walk Python frozensets, so the order depends
on the str hash. The oracle runs with `PYTHONHASHSEED=0` and the port
computes the same order: SipHash-1-3 with a zero key and CPython's set
probing. An empty quoted tag `""` stores `None`, whose hash is a constant.

The store keeps documents in memory with SHA-1 ETags and serves
conditional GETs, ranges, PUT with `If-Match` / `If-Unmodified-Since`,
OPTIONS, content negotiation, cookie-based preferences, a proxy endpoint
that echoes `Cache-Control`, and a CSP endpoint.

`mk.py` writes `data/`: 221 header cases, 15 cookie cases, 4 documents and
57 HTTP requests. `oracle.py` runs Werkzeug's own functions and wrote
`expected.out` (`PYTHONHASHSEED=0 python3 -s -P oracle.py`). `gen_kara.py`
prints the data section of `mini_werkzeug_http.kara`.

One gap was found: an if-expression whose arm is a bare `None` of
`Option[String]`, passed straight as a call argument, reaches the callee
uninitialized on both MIR backends (B-2026-10-20-31, fixed in kara
8dd3fa3bd1; the app passes it inline again).
