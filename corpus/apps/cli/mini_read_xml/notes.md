# mini_read_xml

A Kāra port of [pandas](https://pandas.pydata.org/) 3.0.5's XML I/O
(BSD-3-Clause): `read_xml` and `DataFrame.to_xml`, each with both of its
parsers, lxml 6.1 (libxml2 2.14, XPath 1.0) and the standard library's
`xml.etree.ElementTree` (ElementPath, with minidom for pretty printing).
XML is still how much data arrives: a central bank's daily rates, a
bank's ISO 20022 statement, a shop's order export, OpenStreetMap
extracts, sitemaps and RSS feeds, and the readings a weather service
publishes. `read_xml` turns such a file into a frame, and `to_xml` writes
one back. Jobs come one per line:

```
read ecb_rates.xml xpath=//doc:Cube[@currency] namespaces=doc=http://www.ecb.int/vocabulary/2002-08-01/eurofxref
read orders.xml parser=etree dtype=gift:str,total:float64 parse_dates=placed
read munich.osm iterparse=node:id,lat,lon,k,v
write orders.xml | parser=etree attr_cols=id,status elem_cols=customer,total
```

| Job | The Python call |
|---|---|
| `read` | `pd.read_xml(path, ...)` with `xpath`, `namespaces`, `elems_only`, `attrs_only`, `names`, `dtype`, `parse_dates`, `encoding`, `parser`, `iterparse` and `compression`; it prints the shape, the dtypes, then `to_csv` (`head=n` prints only the first n rows) |
| `write` | `pd.read_xml` with the options before the `|`, then `df.to_xml(...)` with `parser`, `index`, `root_name`, `row_name`, `na_rep`, `attr_cols`, `elem_cols`, `namespaces`, `prefix`, `encoding`, `xml_declaration` and `pretty_print`, printed as returned |

## Running it

The program carries its inputs: it writes `data/` and runs
`data/jobs.txt`, 116 jobs over 16 files. `mk.py` writes them:

- **ecb_rates.xml**, the ECB's reference-rate feed: a `gesmes` envelope
  around a default namespace, three days of six currencies, every value
  an attribute.
- **orders.xml**, a shop's order export: id and status attributes,
  names with entities and accents, a missing total, line items nested a
  level deeper, a note in CDATA, a note with a comment inside it, a
  `true`/`false` gift flag, and comments around and inside the orders.
- **camt053.xml**, an ISO 20022 bank statement (`camt.053`): entries
  with an amount and currency attribute, a credit/debit flag, nested
  status and booking date, references and remittance text.
- **munich.osm**, an OpenStreetMap extract: nodes with coordinates and
  `tag k=… v=…` children, a node with no tags, and a way.
- **sitemap.xml**, also as **sitemap.xml.gz** and in **sitemap.zip**: a
  default namespace with `xhtml:link` alternates in a second one.
- **blog.rss**, an RSS 2.0 feed with the `dc`, `content` (CDATA) and
  `media` namespaces.
- **station_log.xml**, a weather station's readings in ISO-8859-1, with
  an `xml-stylesheet` instruction and the missing values `""`, `NA` and
  `n/a`.
- **catalog.xml**, 400 products, for iterparse.

The other six are the errors people meet: a truncated export, a
mismatched tag, a CSV named as XML, an empty file, a root with no
children, and a file that is not there. Each job prints `$ <job>`, then
what Python prints, or `! ` and the exception pandas raised (36 of the
116).

```
python3 -s -P -B mk.py          # writes data/ and data/jobs.txt
python3 -I -B gen_kara.py       # copies data/ into the end of mini_read_xml.kara
python3 -I -B oracle.py > expected.out
karac run mini_read_xml.kara | diff - expected.out
```

`oracle.py` is the oracle, and it is pandas 3.0.5 on lxml 6.1.3
(libxml2 2.14.6) and Python 3.13.

## What pandas does, and so the port does

These are pandas' answers, kept because `expected.out` is pandas:

- **One level of nesting.** A row is an element's attributes, its own
  text and its children's texts; a child's children are dropped, so
  `items` reads as the whitespace between its `item`s (`"\n      "`),
  and an attribute and a child with the same name share one column.
- **The two parsers read text differently.** lxml's text stops at the
  first comment, so the note `Leave at door — <!-- … --> code 4417 B`
  reads as `Leave at door — `; ElementTree drops comments and reads
  `Leave at door —  code 4417 B`.
- **A default namespace needs a prefix.** `//Cube` finds nothing in the
  ECB feed; `//doc:Cube` with `namespaces=doc=…` does. An undeclared
  prefix is lxml's `XPathEvalError` and pandas' own `SyntaxError` for
  etree.
- **The xpath must select elements.** `//order/@id` is an
  `AttributeError` and `count(//order)` a `TypeError`, from pandas
  calling `.xpath` on what came back.
- **`names` renames by position after the columns are merged**, so a
  missing child shifts every later value one column left (the order
  without a total).
- **`encoding` decodes the file, and the declaration is ignored**, except
  by iterparse, which reads the file as it is on disk. lxml is then
  handed the same name, and libxml2 does not know `latin-1`:
  `LookupError: unknown encoding: 'b'latin-1''`.
- **iterparse keeps the last value of each column** in a row, from any
  depth, and with repeated names it keeps the first instead.
- **Types are inferred as for CSV**: `true`/`false` read as bool, a
  column with a missing value as float, and an all-missing column as
  `float64`; `parse_dates` turns ISO strings into `datetime64[us]`, and
  `Z` into UTC.
- **The two writers write different bytes**: lxml's declaration in single
  quotes, minidom's (etree's pretty printer) in double quotes and always,
  even with `xml_declaration=False`; `<x />` from ElementTree unpretty
  and `<x/>` otherwise; a tab in an attribute as `&#09;` or `&#9;`. With
  `index=True` and `attr_cols`, the index is both an attribute and an
  element.

## Parallel work

Every job runs in its own `TaskGroup` task and the results join in
order, so the transcript is the same on every run and thread count.
The
program matches all 1,808 lines of `expected.out` on `__mir-run`,
`__mir-llvm-run`, `__mir-llvm-asan` (no reports) and a `karac build`
binary, at `KARAC_THREADS=1` and `4`, and on five runs of the auto-par
build (0.04 s real or less).

## The code, ported

About 8,400 lines before the data section, about 6,600 of them reused:

- from **mini_read_excel**: the text helpers, CRC-32, inflate and zip,
  ElementTree's expat-style parser (namespaces, entities, CDATA, its
  error messages with line and column), and the Python parser's type
  inference, `dtype` casts and `to_csv`;
- from **mini_read_sas**: gzip and the codecs (strict UTF-8 with
  Python's messages, latin-1, cp1252, cp1251);
- from **mini_xml**: ElementPath, given the prefix map and default
  namespace it lacked, and Python's `TypeError` for a path that ends
  inside a predicate;
- from **mini_xpath**: the XPath 1.0 data model and evaluator lxml's
  queries run on.

The new code is read_xml itself (`_parse_nodes`, `_iterparse_nodes`, the
path and names checks for each parser, lxml's text and encoding rules,
`parse_dates`) and to_xml's three serializers: ElementTree's `tostring`
with its namespace prefixes, minidom's `toprettyxml`, and libxml2's
formatter, each with its own escaping and character references for the
output encoding.

## The Mend loop

`karac check` reported four errors, all with machine-applicable fixes,
and `karac fix` applied the three edits they call for (an `own`
parameter and two `ref` bindings in a `match`). Before that it had named
three casing errors my rename script made (`p_Sel` for an enum), with
the right names, and two functions I had not copied over. The program
then checked clean and matched `expected.out` on the first run.

## Gaps found

None in the compiler: no workaround in the program.

## What reading XML would need (notes for M4c)

Compared with `docs/library/data.md` in the kara repo, which reads CSV
and Arrow IPC:

- **An XML reader**: `read_xml(path; xpath, namespaces, ...)` beside
  `read_csv`. Banks (ISO 20022), central banks, government open data,
  sitemaps, feeds and many enterprise exports offer nothing else.
- **What a row is.** pandas flattens one level and drops the rest, and
  merges an attribute and a child of one name. The page should say what a
  reader does with deeper levels (an error, a `String` of the subtree, or
  path-named columns such as `items.item.sku`) and with name clashes.
- **A selection language.** pandas has two: lxml's XPath 1.0 and
  ElementPath, a subset with different errors. One is enough; the jobs
  here use descendants, attribute and child-text predicates, positions and
  `local-name()`.
- **Namespaces** are in every real feed. A `namespaces` map is the
  minimum; matching on local names would spare users pandas' "bind a
  prefix to the default namespace" step, which every first use trips on.
- **Types.** XML has none, so every column goes through inference. With
  `data.md`'s null slots a missing value can stay a null `i64` or `bool`
  rather than turning the column into floats or objects as pandas does;
  whitespace-only text (`"\n      "`) should probably read as null.
- **Dates.** Decision Q1 reads dates as `String`, but feeds are full of
  them: `lastmod`, `BookgDt`, OSM's `2023-01-10T00:15:00Z`. A reader
  needs a date and a UTC timestamp type, or a documented string form a
  later parse takes.
- **Encodings.** XML declares its encoding, and the declaration should
  decide, not a parameter that pandas lets contradict it; latin-1 and
  cp1252 files are common.
- **Compression and streaming**: gzip and zip by extension, and an
  iterparse-style scan (`scan_xml` into a `LazyFrame`) for files too large
  to hold, which pandas refuses on compressed input.
- **A writer**: `write_xml(df, path; root, row, attr_cols, elem_cols,
  na_value, namespaces, prefix, encoding, declaration, indent)`, with one
  serialization and character references for what the encoding cannot
  hold.
- **Errors**: a parse error with line and column, an xpath that selects
  nothing or not elements, and a names-length mismatch are each a
  `DataError` the page could name.
