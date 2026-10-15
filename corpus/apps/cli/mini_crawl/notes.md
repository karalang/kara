# mini_crawl

A site crawler and link checker: the first stage of a search indexer, or
the link check a docs site runs in CI. It reads a site from `data/site/`
the way a web server serves it, honours the site's robots.txt, follows
links breadth-first to depth 3, and writes `data/out/sitemap.xml` and
`data/out/report.tsv`. It answers what the same crawler written with
Python 3.13's `html.parser`, `urllib.parse` and `urllib.robotparser`
answers, so the program ports all three.

The run first writes the site (27 files) and `data/url-tests.tsv`. Then
it does three things:
- **URL library cases.** It runs the 83 cases in `url-tests.tsv`:
  - `urljoin` on all of RFC 3986's normal and abnormal reference
    examples, plus a few more;
  - `urlsplit` with its `username`, `password`, `hostname` and `port`
    and an `urlunsplit` round trip, over user info, IPv6 hosts, empty and
    bad ports, stripped control characters, and strings that only look
    like schemes;
  - `quote`, `quote_plus`, `unquote`, `unquote_plus`, `parse_qsl` and
    `urlencode`.
- **robots.txt probes.** It checks 21 paths for four user agents and
  prints each agent's `Crawl-delay`, `Request-rate` and the sitemaps.
- **The crawl.** It prints each URL it visits with its status, title,
  base, canonical URL and robots meta tags, then every link on the page
  with what it resolved to and what the crawler did with it. A summary
  lists the broken links with the pages that link to them, the external
  hosts, and the sitemap.

The site has the things crawlers trip over:
- relative links with `..` past the root;
- `<base href>`, `rel="canonical"` and `rel="nofollow"`;
- `<meta name="robots">` with `noindex` and `nofollow`;
- links inside comments and scripts, which don't count;
- unquoted, uppercase and duplicate attributes, and an attribute with no
  value;
- character references in text and in attribute values;
- `mailto:`, `javascript:` and `tel:` links;
- default and bad ports, mixed-case hosts and fragments;
- percent-encoded and raw non-ASCII paths that name the same page;
- `utm_` tracking parameters, which are dropped;
- `;jsessionid` parameters;
- a directory URL without its slash, which gets a 301;
- broken links.

**URLs** are normalized before they are compared:
- the fragment is removed;
- the scheme and host are lowercased, and a default port is dropped;
- the path is re-quoted, so `%7E` and `~` are the same;
- the query is re-encoded without `utm_` parameters.

**robots.txt** is matched as RFC 9309 and `urllib.robotparser` match
it:
- a user agent's own group replaces the `*` group rather than adding to
  it, so this crawler may read `/drafts/` that `*` may not;
- groups for the same agent are merged;
- the longest matching rule wins, and `Allow` wins a tie;
- `*` matches anything and `$` anchors the end;
- paths and patterns are compared percent-normalized;
- a rule with `$` before its end is dropped.

**The HTML tokenizer** follows `html.parser`'s rules:
- start tags, attributes and values;
- raw-text and escapable raw-text elements (`script`, `style`, `iframe`,
  `title`);
- comments, declarations and processing instructions;
- `html.unescape` for text. Named references there may lack their
  semicolon, as in `&copy 2026`. Numeric references follow HTML5's
  replacements.
- for attribute values, numeric references are always decoded and named
  ones only when they are an exact match not followed by `=`. So the
  `&region=eu` in a query string survives, and `&amp;` does not.

Not supported:
- the full HTML5 table of named character references (only the ones
  these pages use are listed; others are left as written);
- IPv6 address validation, and IDNA or non-ASCII host names;
- invalid UTF-8 after percent-decoding, which Python replaces with
  U+FFFD;
- `textarea` and `plaintext` content beyond what the pages contain;
- real HTTP: pages are files, and a request for a directory's
  `index.html` stands in for a server's directory index.

**Oracle.** `expected.out` is the output of `oracle.py`, the same crawler
in Python 3.13.16 with only the standard library. Run it from this
directory after `mk.py` writes `data/`. The files the crawler writes
into `data/out/` are byte-identical to the ones the oracle writes.

## Edits from mini_crawl.orig.kara to mini_crawl.kara

- The robots.txt group struct is `Group`, not `Entry`. `karac check`
  warns that `Entry` shadows a prelude type, and B-2026-10-15-8 and
  B-2026-10-15-41 show that such a shadow can break library code on v2.

There were no author errors and no compiler gaps. The program matched
expected.out on the first run on all three MIR surfaces.

Written 2026-10-15 by the real-world apps thread; karac at 72cc0d4a00.
