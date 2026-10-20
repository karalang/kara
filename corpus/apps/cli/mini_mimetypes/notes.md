# mini_mimetypes

Python 3.13's `mimetypes` module, ported from `mimetypes.py`. Web
servers, upload handlers, object-store sync tools and mail clients call
it to turn a file name into a `Content-Type`, and a type back into an
extension.

A `MimeTypes` database has a strict table of registered types and a
lenient one of common types that are not registered. Each table maps
extensions to types and types back to their extensions, in the order
they were added. The database also holds the suffix map, which turns
`.tgz` into `.tar.gz`, and the encodings map for `.gz`, `.Z`, `.bz2`,
`.xz` and `.br`. The port covers:
- `guess_type` over paths and URLs. It runs the parts of
  `urllib.parse.urlparse` that the module needs: the scheme rules, the
  C0 and whitespace stripping, the netloc split with its IPv6 bracket
  checks (on an IPv6 parse that follows `ipaddress`), and the params cut
  for the schemes that have them. `data:` URLs are covered too;
- `guess_file_type`, `guess_extension`, `guess_all_extensions` and
  `add_type`;
- reading mime.types files the way `readfp` splits them, on Python's
  whitespace set, and `read_mime_types`;
- the module-level database: `init()`, its `knownfiles`, and an
  `init(files)` that adds to the database it already has;
- `python -m mimetypes` with getopt's errors and the usage text.

`mk.py` writes `data/`. It holds `jobs.tsv` with 10 jobs of calls and
28 command lines, a hand-written system mime.types, an Apache copy, a
site-local file read leniently, a file with odd whitespace, an nginx
`types` block, and the inputs of eight programs:
- a static file server that picks `Content-type` as http.server's
  `SimpleHTTPRequestHandler` does, then serves precompressed `.br` and
  `.gz` copies by `Accept-Encoding`;
- an nginx `types` block compared with mime.types, written out as a
  mime.types file and read back;
- an upload validator with an allowlist, size limits and a check for
  disguised executables;
- an object store upload plan with `Content-Type`, `Content-Encoding`
  and `Cache-Control` for each object;
- mail attachments typed the way the `email` documentation's example does it;
- content negotiation over `Accept` headers with q-values;
- a CDN access log summed by type;
- a shared-drive inventory that suggests the canonical extension.

`oracle.py` runs the same jobs through the real module, with
`knownfiles` pointed at the data files, and wrote `expected.out`.
`gen_kara.py` writes the data section.

The quirks are kept. Some examples:
- `archive.TGZ` is a gzipped tar, but `archive.tar.GZ` is nothing: the
  suffix map is looked up in lower case and the encodings map is not;
- an extension added in upper case, like Apache's `PSD`, can never be
  found, because the lookup lowercases the extension and the table does not;
- `guess_extension` lowercases the type and the reverse table does not,
  so `Text/X-Mixed` has no extension;
- a one-letter scheme is a drive letter, so `c:report.pdf` is a path;
- `data://image/gif,x` is `/gif`, because the netloc is split off first;
- `http://[::g]/x.png` raises ipaddress's "does not appear to be an IPv4
  or IPv6 address";
- on a path with no scheme, the query is part of the extension, so the
  CDN log has to cut `?v=8f3a` before guessing;
- `python -m mimetypes --e` is `--extension` with nothing to do, and
  `--=x` is "option -- not a unique prefix".

It matched on every backend with no workaround in the port. One parser
gap changed how the tables are declared: a Rust array type `[Str; 16]`
gets only "Expected type, found LeftBracket", with no pointer to
`Array[Str, 16]` (B-2026-10-20-27).
