# mini_feedparser

A port of [feedparser](https://github.com/kurtmckee/feedparser) 6.0.11
(BSD-2-Clause; Kurt McKee, Mark Pilgrim and contributors), the RSS and Atom
parser that most Python feed readers and aggregators use. It runs on a port
of [sgmllib3k](https://pypi.org/project/sgmllib3k/) 1.0.0 (BSD), the
Python 2 `sgmllib` that feedparser's loose parser, HTML sanitizer and
relative-URI resolver are built on. Four small programs run on top: a
front-page digest, a tag count, an OPML export and a feed health check.

```
> !digest feeds/reader_0.xml feeds/reader_1.xml feeds/reader_2.xml feeds/rss20_blog.xml feeds/atom10_news.xml
  15 entries from 5 feeds, 0 undated
  2024-03-06 23:59:59  Example News     Weather <b>warning</b> <tag:news.example.com,2024:weather>
  2024-03-06 00:15:30  Example News     Council approves <b>new</b> bridge <https://news.example.com/2024/03/bridge>
  ...
> !health ...
  cp1252_mislabeled.xml        rss20    windows-1252  1 entries  bozo CharacterEncodingOverride
  bad_entity.xml               rss20    utf-8         1 entries  bozo SAXParseException
  junk_after.xml               atom10   utf-8         0 entries  bozo SAXParseException
```

## What it does

`mk.py` writes 23 files to `data/` (listed in `data/files.txt`): 21 feeds,
25 date strings and 20 HTML snippets, and `data/jobs.txt` (27 jobs).
`oracle.py` runs each job on the vendored feedparser and prints the result;
`mini_feedparser.kara` runs the same jobs.

- **`parse`** (20 jobs) prints what `feedparser.parse()` returns: `bozo`,
  the encoding, the version, the exception, the headers, the namespaces,
  the feed dict and every entry, keys in insertion order. The feeds are
  RSS 0.91 (Netscape), 0.92, 1.0 and 2.0, Atom 0.3 and 1.0, a podcast with
  iTunes tags, Latin-1, mislabelled cp1252 and BOM-marked files, and seven
  broken ones (an undefined `&nbsp;`, a bare `&`, a truncated file, a
  mismatched tag, an unbound prefix, a second root and an HTML page). Two
  jobs pass a `Content-Location` header, as an HTTP fetch would.
- **`!dates`** runs `_parse_date` on 25 strings.
- **`!sanitize`** and **`!resolve`** run `_sanitize_html` and
  `resolve_relative_uris` on 20 snippets.
- **`!digest`** merges five feeds newest first; **`!tags`** counts
  category terms over six; **`!opml`** writes an OPML subscription list
  and skips feeds that failed; **`!health`** prints one line a feed.

## How the port works

- **Values.** feedparser's `FeedParserDict`s and lists live in arenas, so
  a context such as `entries[-1]` or `feed['image']` is shared the way
  Python shares the object. The key aliases (`channel`, `guid`,
  `description` and the rest) apply on reads and writes, and `deepcopy`
  makes the `*_detail` dicts.
- **The strict path** is the part of expat and `xml.sax.expatreader` that
  feeds reach, with namespaces on: prefix mappings before each start tag,
  `(namespace, local)` names, attribute normalisation, and the error
  messages and positions of `SAXParseException`. Any error sends the
  document to the loose path, as `parse()` does.
- **The loose path** is `SGMLParser.goahead` with the patterns that
  `feedparser/sgml.py` patches in (its end-bracket matcher included), plus
  `_BaseHTMLProcessor.feed`'s rewrites. That also drives the sanitizer
  (allowed elements and attributes, style filtering, unsafe URI schemes)
  and the relative-URI resolver.
- **The mixin** carries the `_start_*` / `_end_*` handlers of the base,
  Dublin Core and iTunes namespaces, the element stack, the `xhtml` div
  trimming, the author and email splitting, tags and `_cp1252` translation.
- **Encodings and dates.** `convert_to_utf8` covers BOMs, the XML
  declaration and the `Content-Location`-only header case; the W3DTF and RFC 822 date handlers give the
  `utctimetuple()` 9-tuples.
- `gen_kara.py` copies the library's own tables (HTML entities, allowed
  elements, attributes and CSS words, namespaces, URI schemes, `_cp1252`)
  from the vendored source.

feedparser's behaviour shows in the output, and the port keeps it:

- `_start_content` pushes `content` twice, so after an Atom `<content>`
  a second `content` frame stays on the element stack for the rest of
  the document.
- `_BaseHTMLProcessor.feed` passes `re.IGNORECASE` as `count`, so only
  the first two `<!` that start no declaration are escaped.
- A feed fetched with a `Content-Location` header and no `Content-Type`
  is `bozo` with `NonXMLContentType: no Content-type specified`, though it
  parses cleanly.
- The sanitizer drops `javascript:` links to `href=""` but keeps the tag.

## Compiler gaps

None. `karac fix` applied the call-site `mut` markers. `karac check` also
refused `Val` without `#[derive(Clone)]`, bare `Val` parameters handed on
as owned values, `format!` and two methods that do not exist
(`String.parse_i64`, `char.from_u32`). After those changes the interpreter
matched the oracle byte for byte, as did the LLVM, ASan and native builds.

## Scope

- Dates: only the W3DTF and RFC 822 handlers. `oracle.py` checks that no
  other handler (ISO 8601, asctime, Perforce, the Greek, Hungarian and
  Korean ones) accepts any date the jobs meet.
- Not ported, and not in the data: the GeoRSS, Media RSS, Podlove,
  Creative Commons and admin namespaces, SVG and MathML in content,
  base64 content, DOCTYPE entity declarations, `_markupbase`'s declaration
  parser, HTTP fetching and chardet.
- Encodings: UTF-8, US-ASCII, ISO-8859-1 and windows-1252.

## Running

```bash
karac run mini_feedparser.kara                    # interpret
karac build --mir mini_feedparser.kara            # native binary
python3 -s -P -B mk.py                            # regenerate data/
python3 -I -B oracle.py > expected.out            # regenerate expected.out
python3 -I -B gen_kara.py                         # regenerate the embedded data
```
