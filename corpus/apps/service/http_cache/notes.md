# http_cache

A caching reverse proxy in the shape of Varnish or nginx's `proxy_cache`,
between a client and an origin on loopback (`127.0.0.1:47754` in front of
`127.0.0.1:47753`). It keeps what an origin's `Cache-Control` lets a shared
cache keep and answers each request with an `X-Cache` label:

- **HIT**: a fresh entry, with its `Age` (an origin's own `Age` plus the
  time it has been stored). A client's `If-None-Match` that matches the
  entry's `ETag` gets a 304.
- **MISS** / **PASS**: fetched and stored, or fetched and not, with the
  reason logged: `no-store`, `private`, `Vary: *`, an `Authorization`
  request without `public` or `s-maxage`, a status outside 200, 203, 301,
  404 and 410, no freshness and no validator, or a body over the object
  limit.
- **REVALIDATED** / **UPDATED**: a stale entry (past `s-maxage`, else
  `max-age`, or `no-cache`, or a client's `Cache-Control: no-cache`)
  asked of the origin with `If-None-Match` and `If-Modified-Since`; a 304
  refreshes its headers and freshness, anything else replaces it.
- **STALE** / **ERROR**: the origin failed, and the stale entry answers
  inside its `stale-if-error` window, or not past it, and never under
  `must-revalidate` (a 504).
- **BYPASS**: a client's `Cache-Control: no-store`.
- **PURGE** with the token drops a URL's entries; a successful unsafe
  method (`POST`, `DELETE`) drops its target's (RFC 9111 §4.4).

Each URL keeps one entry per `Vary` variant (`Accept-Language: fr-CA`
and `en`), and the least recently used entries are evicted past a byte
budget. The cache's state is one `Mutex` in a `sync struct`, shared by
the server's connection tasks, and it is never held across an origin
fetch.

**Self-test.** The logic runs first against a fake origin on a fake
`Network` (`with_provider`), with the time of each request chosen, so
every `Age` is exact: freshness, a 304, an origin `Age`, `s-maxage`, a
stored 404, `stale-if-error` inside and past its window, `must-revalidate`
with the origin down, `no-cache`, `Vary`, `Authorization`, eviction under
a 120-byte budget, purges and a `DELETE`.

**Live run.** A real client asks the cache, which asks a real origin whose
pages change or fail on chosen hits. A second later, the one-second
entries are revalidated, updated, served stale or refused. The run prints
the cache's entries, its event log and each origin path's hit count.

Oracle: `http_cache.py`, the same proxy with `http.server` and
`http.client`. `expected.out` is its output. The Kāra client turns a
status of 400 or more into an `Err` carrying only "<url>: status code
<n>" (B-2026-11-09-44), so on the real network an origin's 404 is an
origin failure (the cache answers 502) and so is a 503 (which is what
`stale-if-error` answers); the mirror's client does the same. A 304 comes
back as a response. Under the fake `Network` every status comes back as
it is, so the self-test stores a 404.

## Edits from http_cache.orig.kara to http_cache.kara

By `karac fix`, from machine-applicable diagnostics:
- `[Str; 9]` and `[i64; 5]` are `Array[Str, 9]` and `Array[i64, 5]`.
- Parameters that borrow lose their Rust `ref`; the ones that keep their
  argument take `own` (`text: own String`, `e: own Entry`).
- `req.header(name)` takes a `String`: `name.clone()`, and a `Str`
  element of a `Slice[(Str, Str)]` gets `.to_string()` (B-2026-11-09-95).
- `req.header(name) ?? "" == value` is `req.header(name) ?? ("" == value)`,
  since `??` binds loosest: `(req.header(name) ?? "") == value`
  (B-2026-11-09-94).
- Fields read out of a borrowed `Response` are cloned
  (`r.body.clone()`), and `oldest.unwrap()` on a binding read again is
  `oldest.clone().unwrap()`.
- `.to_string()` on a literal in a `(String, String)` the pair's type
  already makes one is dropped.

By hand:
- A field named `private` cannot be written in Kāra (a structural
  marker, which `r#` does not escape): `is_private`. The diagnostic said
  "Expected identifier, found `:`" at the colon and cascaded past the
  struct literal (B-2026-11-09-92, fixed).
- `collect::<Vec[String]>().join(..)` is Rust's turbofish: the collect is
  bound to a typed `let` first (2 sites), as the diagnostic says.
- `headers: [(Str, Str)]` is Rust's slice type: `Slice[(Str, Str)]`
  (3 sites).
- `{d.max_age:?}`: Debug formatting is reserved; `{d.max_age}` prints an
  `Option` as `Some(60)` / `None` (2 holes). Both were reported at the
  f-string's closing quote (B-2026-11-09-91, fixed).
- `with_label(headers: Vec[..])` moves the pairs out: `own`. The tuple
  `(k, v)` of borrowed elements had no fix (B-2026-11-09-96, open).
- `req.header(..) != Some(PURGE_TOKEN)` compares an `Option[String]` with
  an `Option[Str]`, refused with no remedy (B-2026-11-09-97, open):
  `Some(PURGE_TOKEN.to_string())` (2 sites).
- `s.strip_prefix("\"").and_then(|t| t.strip_suffix("\"")).unwrap_or(s)`
  is refused as returning a borrow of a temporary, though the `Str` views
  `s` (B-2026-11-09-98, open): a `match` on `strip_prefix`.

Compiler gaps this app found:
- A program's own `struct Entry` lost its own methods to the map entry's
  dispatch: "no method 'age' on type 'Entry'" (B-2026-11-09-93, fixed).
- The four diagnostics above marked fixed (B-2026-11-09-91, -92, -94,
  -95), and the three marked open (B-2026-11-09-96, -97, -98).
