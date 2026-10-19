# mini_w3lib

The URL, markup, encoding and header helpers of w3lib 2.5.0, the library
Scrapy and many other crawlers use to make links fetchable, compare them,
read `<base>` and `<meta refresh>`, and work out what encoding a page is
in. It is ported from the `w3lib` package vendored here (BSD-3-Clause,
see `LICENSE-w3lib`).

**w3lib.url**

- `safe_url_string`, on w3lib's own WHATWG-flavoured `urlsplit`. That
  covers the backslash and tab rules, port checks, bracketed IPv6 and
  IPvFuture hosts, and NFKC netloc checks.
- IDNA hosts, through the `idna` codec's nameprep (Unicode 3.2 NFKC with
  stringprep tables B.1, B.2, C and the bidi rules) and Punycode.
- Percent-encoding in the page's codec, with the `percentencode` error
  handler for bytes the codec cannot decode.
- `canonicalize_url`: sorted queries, dot-segment removal, IPv6 host
  normalization and the UTF-8 fallback.
- `safe_download_url`, `url_query_parameter`, `url_query_cleaner` and
  `add_or_replace_parameter(s)`.
- `parse_data_uri` with base64, and file URIs.
- `ipaddress` and `urllib.parse.urljoin`, as the standard library does
  them.

**w3lib.html**

- Entities, with the cp1252 reading of `&#128;` to `&#159;`.
- Tag, comment and tag-with-content removal, on w3lib's regexes.
- `get_base_url` and `get_meta_refresh`, including the byte-level quick
  check and the `max_scan` cut that keeps a tag whole.

**w3lib.encoding**

- BOMs, `Content-Type` charsets (quoted ones too) and the in-body
  `<meta>` and `<?xml?>` prescan.
- `resolve_encoding` over Python's codec registry and alias table, with
  w3lib's WHATWG translations; utf-7 is refused.
- `html_to_unicode`, decoding utf-8, utf-16, utf-32, ascii, latin-1,
  cp1251, cp1252 and iso8859-15 as Python's codecs do. That includes
  the exact `UnicodeDecodeError` and `UnicodeEncodeError` messages.

**w3lib.http**: raw headers to a dict and back, and `basic_auth_header`.

The regexes run on mini_re's engine (as in mini_soupsieve), with each
pattern taken verbatim from the w3lib source. A bytes pattern runs on
the bytes as code points 0 to 255 with the ASCII flag.

The run first executes 2,627 jobs from `data/jobs.jsonl`. Most of these
are calls recorded from w3lib's own test suite (`calls.jsonl`). The rest
are extra calls in `mk.py`: about 90 encoding labels, IDNA hosts, legacy
codecs, data URIs and page heads.

It then crawls a nine-page shop site of raw HTTP responses in
`data/site`. The pages are in utf-8, cp1251 (declared in a meta tag),
cp1252 (declared in the header) and utf-16 with a BOM. The site also has
a meta refresh, an iso-8859-15 XHTML page with `<base>`, a blog page with
a script and a comment, and a 401 page. Each link is joined, stripped of
tracking parameters, and canonicalized before it is queued.

`expected.out` is the output of `oracle.py`, which runs the same jobs
and the same crawl through w3lib itself. `mk.py` writes `data/`, and
`gen_kara.py` prints the data section at the end of `mini_w3lib.kara`:
the files, the codec registry, the charmaps, the entities, the Unicode
data and the regex sources. Calls that need codecs the port does not
carry (gb18030, iso2022_jp, shift_jis, big5hkscs) were left out, along
with calls that depend on the working directory.
