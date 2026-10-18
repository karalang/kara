# mini_nginx

nginx serves a large share of the web, and most of what it does for a
request is decided by its configuration before any file is read. The
program ports that part: `nginx -t`'s parser and messages, how a server
and a location are chosen, the rewrite module, and the static and proxy
handlers, over a real document tree on disk. It prints a trace of each
request that matches `oracle.py`, the same routing written in Python.

The run writes into `data/`:
- `nginx.conf`, which includes `mime.types`, with an upstream of three
  peers and five servers: a `default_server` catch-all that returns
  444, a site with fifteen locations, a `*.example.org` redirect, a
  `mail.*` server, and per-user pages on port 8080 whose `root` takes a
  named capture from a regex `server_name`;
- three broken configs: a missing `;`, a misspelled directive, and a
  file that ends inside a block;
- the document roots, including a `.env` and a `.git/config` that must
  not be served;
- `requests.txt`, 38 requests by port, Host and target.

**Parsing.** Tokens are words, `;`, `{` and `}`; quoted strings take
`\n`, `\t`, `\r` and escaped quotes; `#` starts a comment. A regex with
`{` must be quoted, as in nginx. Each error is nginx's `[emerg]` line,
with the file and line.

**Server.** Within the request's port: an exact name, then the longest
`*.` wildcard, then the longest `.*` wildcard, then the first regex
name that matches, then the `default_server`, then the first server.
The Host header is lowercased and its port dropped.

**URI.** `%XX` is decoded, `//` merged, `.` and `..` resolved, and a
path that climbs above `/` is a 400. Locations see the result.

**Location.** An `=` match wins at once. Otherwise the longest prefix
is remembered; if it is `^~`, it wins. Otherwise the `~` and `~*`
regexes are tried in order and the first match wins, else the prefix.
A regex match sets `$1`..`$9` and its named groups.

**Rewrites.** Server rewrites run, then the chosen location's `rewrite`
and `return` directives in order:
- `last` searches for a location again;
- `break` stays in this location;
- `redirect`, `permanent`, or a replacement that starts with a scheme
  ends the request with a 302 or 301;
- no flag goes on to the next directive.

A replacement with `?` sets new arguments, and the old ones are
appended unless it ends with `?`. More than ten URI changes, from
rewrites or internal redirects, end the request with a 500.

**Content.**
- `proxy_pass` picks the next peer of its upstream, round-robin.
- `try_files` tests each name as a file, or as a directory when it ends
  in `/`. The first hit is served from this location. Otherwise the
  last argument is `=code` or an internal redirect, which runs the
  server rewrites again, as nginx's internal redirects do.
- A URI ending in `/` is served by its first `index` file, which is
  another internal redirect, or is a 403 when it is a directory.
- A file is a 200 with its MIME type from `mime.types`. A directory
  without the `/` is a 301 to the URI with it.
- `alias` replaces the location's prefix, and `root` is prepended.
- A location's `add_header` lines are printed for the success and
  redirect codes.

`mk.py` holds the input files. `gen_kara.py` prints them as the Kāra
data functions in the program. nginx itself is not run: the oracle is
the Python port. Its patterns go through `re`, with `(?<name>` written
as `(?P<name>`.

## Edits from mini_nginx.orig.kara to mini_nginx.kara

All of them were author errors, and none was a compiler gap:
- `for d in inc` borrows the elements. Pushing them needs
  `inc.into_iter()`.
- `done_text` and `done_redirect` store their bare `String` parameter,
  so the parameter needs `own String`.
- `mut` was missing at three call sites that pass a fresh local to a
  `mut ref` parameter.
- `check(b)` discarded its `Option`; it is now `let _ = check(b)`.

## Library migration, 2026-10-18

Thread A's a0ca700228 and acd593fc77 changed the library to match docs/library: a number's `parse` returns `Result[T, ParseError]`, and `split`, `lines`, `split_whitespace` and `trim` give `Str` views (`split` and `lines` are now lazy iterators). Arguments forwarded to a `mut ref` parameter are now marked too. `karac fix` made the edits. The app matches expected.out on all four MIR backends again (karac at a4b0967495).

## Back to the original, 2026-10-18

The rows behind the edits above are fixed, so `mini_nginx.kara` is now exactly what `karac fix` makes of `mini_nginx.orig.kara`. It matches expected.out on all four MIR backends (karac at 4ea06e27b9).
