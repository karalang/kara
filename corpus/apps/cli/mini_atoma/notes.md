# mini_atoma

A port of [atoma](https://pypi.org/project/atoma/) 0.0.17 (MIT; Nicolas
Le Manchet), the feed parser behind several Python feed readers and
aggregators. It reads RSS 2.0, Atom 1.0, JSON Feed and OPML into plain
objects, and its `simple` module turns any of the three feed formats into
one `Feed` of `Article`s with their attachments.

```
> podcast data/feeds/json/podcast.json
  'Timetable'
  'Episode 42': '42.m4a' 'audio/x-m4a' 89970236 1:50:29
  'Episode 41': '41.m4a' 'audio/x-m4a' True 1 day, 1:01:01
  'Episode 41': '41-bonus.m4a' 'audio/x-m4a' 0 -1 day, 23:59:59
```

## What it does

`mk.py` writes 131 jobs (and 8 section comments) to `data/jobs.tsv`, and
62 input files (feeds, OPML files and a cache index) to `data/`. `oracle.py`
runs each job against the vendored library and prints the objects it
returns field by field, or `error <class>: <message>` when it raises;
`mini_atoma.kara` runs the same jobs. 70 jobs raise.

- **`rss`** (23 jobs) reads a blog with every channel field, a podcast
  with enclosures, a feed of 20 date formats, and the feeds atoma refuses
  or half-reads: no title or link, version 0.91, no channel, a `ttl` that
  is not a number, an image with no URL, an enclosure with no URL, an
  empty `<source>`.
- **`atom`** (10) reads an Atom blog with HTML and XHTML text constructs,
  enclosure links and empty authors, an aggregated feed whose entries
  carry their `<source>`, a feed using a prefix for the Atom namespace,
  and feeds with no id, an unknown text type, a link with no `href` and a
  category with no `term`.
- **`json`** (24) and **`json_bytes`** (4) read JSON Feeds: a blog, a
  podcast, integer ids, missing titles, a list at the top, items that are
  not objects, `tags` that is a string, sizes that are `true` or `1.5`,
  `NaN`, a byte-order mark (refused from a file, accepted from bytes), a
  file that is not UTF-8, and nine documents the decoder refuses.
- **`opml`** (5) reads a subscription list with nested folders and lists
  its feeds, and three that break the parser.
- The **broken and hostile XML** jobs read an unclosed tag, an undefined
  entity, a "billion laughs" DTD, an external entity, a parameter entity,
  an empty file, an HTML page and a missing file.
- **`simple`** (28) and **`simple_bytes`** (4) run `simple_parse_file`
  and `simple_parse_bytes` over most of the above.
- **`date`** (28) runs atoma's date reader on RFC 822, ISO 8601 and free
  text dates.
- **`river`** is a feed reader's front page: it reads an OPML file, finds
  each feed in an HTTP-cache directory, parses it with the simple API and
  lists every article newest first in UTC. **`podcast`** lists a feed's
  episodes and files with the total size and length.

## How the port works

- **XML** is the ElementTree parser from [mini_xml](../mini_xml), which
  turns tags into `{uri}local` names as expat does. atoma's
  `find('feed:title', namespaces=ns)` becomes a child scan for the
  expanded name. atoma parses through defusedxml, which refuses any entity
  declaration with `EntitiesForbidden` before the document is read; the
  port adds that check to the DOCTYPE reader, with the same message.
  Every other parse failure becomes `FeedXMLError`.
- **Dates** go through `parse()` from [mini_dateparse](../mini_dateparse)
  with `fuzzy=True` and atoma's RFC 822 zone table (`UT` is `gettz('GMT')`,
  which is UTC). A date with no zone gets UTC, and one dateutil cannot
  read is `None`.
- **JSON** is read by a strict decoder written for this port: Python's
  whitespace, escapes, number grammar, `NaN` and `Infinity`, and nothing
  after the document. A lookup takes the last value of a duplicated key.
  `json.load` of a file and `json.loads` of bytes differ in one way, the
  byte-order mark, and the port keeps both.
- **Python's behaviour is kept where atoma relies on it.** Some examples:
  an element with no children is false, so `<author/>` is skipped at the
  feed level but not in an entry; `isinstance(True, int)` holds, so a size
  of `true` is the size `True`; `get_int(...) or 88` turns a width of 0
  into 88; iterating a string yields its characters; and calling `.get`
  on a list raises `AttributeError`. `html.unescape` (for Atom HTML
  titles in the simple API) comes from
  [mini_htmlparser](../mini_htmlparser) with the full HTML5 entity table,
  and `urlparse(link).path` is written out for attachment titles,
  including its `;params` split.
- `simple_parse_file` opens the file once for each of its three
  attempts. The port reads it once and runs the three parsers on the same
  bytes, which gives the same results.

## Compiler gaps

None. `karac fix` added an `own`, a call-site `mut` and a
`#[derive(Clone)]`, one `ref` was added by hand, and the interpreter then
matched the oracle byte for byte on its first run, as did the LLVM, ASan
and native builds.

## Scope

- Feeds are UTF-8. expat also reads documents that declare another
  encoding, which mini_xml does not; none of the feeds here does.
- The oracle pins what dateutil reads from the machine: the local zone is
  UTC, "this year" is 2026 and "today" is 2026-10-23. The feeds give a
  full date in every date they parse, so "today" fills nothing.
- JSON integers fit in 64 bits, and the decoder takes no lone surrogate
  escapes.

## Running

```bash
karac run mini_atoma.kara                    # interpret
karac build --mir mini_atoma.kara            # native binary
python3 -s -P -B mk.py                       # regenerate data/
python3 -I -B oracle.py > expected.out       # regenerate expected.out
python3 -I -B gen_kara.py                    # regenerate the embedded data
```
