# static_files

A static file server in the shape of nginx's or Caddy's file handler. It
uses real files and a real loopback socket (port 47745).

The program writes a small site to `www/`. The site has HTML, CSS, JS,
JSON, a 7.8 KB access log, a directory without an index, file names that
need escaping (`notes v2.txt`, `café.txt`, `a&b <c>.txt`), dotfiles and
an empty file. It then serves the site over keep-alive connections with
pipelined requests:

- **Validators.** The strong ETag is the body length and CRC-32.
  `If-None-Match` uses the weak comparison and accepts lists and `*`, and
  a match gets 304, for HEAD too.
- **Ranges (RFC 9110).** It handles single ranges, suffix ranges
  (`-120`, a log tail) and open-ended ranges (`7000-`). Several ranges
  get `multipart/byteranges`. It sends 416 with `bytes */len` when no
  range is satisfiable, including any range on an empty file. It honours
  `If-Range`. A syntactically invalid header or a non-`bytes` unit is
  ignored and the whole body is sent. HEAD ignores Range.
- **Paths.** Segments are percent-decoded one at a time and normalised
  (`.`, `..`, empty segments). Climbing above the root is 403, a dotfile is
  404, and an encoded slash, NUL, bad or truncated escape is 400. A
  directory without a trailing slash redirects with its query kept. A
  directory without an `index.html` gets a sorted listing, with names
  HTML-escaped and percent-encoded in the links.
- **Protocol.** HEAD, OPTIONS with `Allow`, 405 for other methods, and
  `Connection: close` (requests after it go unanswered). HTTP/1.0 closes
  by default unless it sends `keep-alive`. A garbage request line, a
  header with no colon, or HTTP/1.1 without `Host` gets 400 and a close.
  An unknown version gets 505.
- **Access log.** Each request is logged. The log is written to
  `access.log` at the end, read back and summarised.

The built-in client parses each response with only the `Content-Length`
it was sent (no body for HEAD, 204 or 304). It prints headers, a CRC of
each body, and every line of HTML and multipart bodies.

Oracle: `static_files.py`. It is a port, run over a real socket.
`expected.out` is its output. The same responses, served from the mirror
behind a normal listener, were checked with curl: a 206 for `-r 0-99`,
304 on the ETag, a 301 redirect to `/docs/`, and 403 for `--path-as-is
/../.env`. Python's `email` parser split the multipart body into the
three expected parts.

## Edits from static_files.orig.kara to static_files.kara

Author errors (the diagnostics were right):
- The constant array type was written `[Str; 10]`; Kāra spells it
  `Array[Str, 10]`.
- `*name` dereferenced a `Str` from iterating an `Array[Str, N]`, which
  is already a value.
- `let levels = ["info", ...]` is an `Array[String, 5]`, because a string
  literal is a `String` by default (design.md § String literals). Indexing
  it would move a `String`. It is now annotated `Array[Str, 5]`.

There were no compiler gaps. The draft passed every MIR surface once the
author errors were fixed.

Written 2026-10-15 by the real-world apps thread; karac at fee58fc7e2.
