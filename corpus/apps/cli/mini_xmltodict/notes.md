# mini_xmltodict

XML to dicts and back, as xmltodict 0.13.0 does it (the version Ubuntu
24.04 ships, with its name-validation patch). API clients use it to read
S3 listings and SOAP responses, config loaders to read Maven POMs and
Android manifests, and feed readers to read RSS. It is ported from
`xmltodict.py` (vendored here, MIT, see `LICENSE-xmltodict`) together
with the parts of expat and Python it rests on.

**parse()** covers every option: `xml_attribs`, `attr_prefix`,
`cdata_key`, `force_cdata`, `cdata_separator`, `strip_whitespace`,
`process_namespaces` with `namespace_separator` and a `namespaces` map,
`force_list` as a flag, a tuple or a function, postprocessors (which see
attributes as well as elements, and may drop keys), `process_comments`
with `comment_key`, `disable_entities` and `dict_constructor=OrderedDict`.
The streaming mode calls back at `item_depth` with the path and the item,
and stops with `ParsingInterrupted` when the callback returns False.
xmltodict's quirks are kept: namespace declarations are attached as an
`xmlns` dict to the next element that has attributes, and with
`force_list=True` even `#text` becomes a list.

**unparse()** covers `pretty`, `indent`, `newl`, `full_document`,
`short_empty_elements`, `encoding`, `attr_prefix`, `cdata_key`,
`namespaces` (where an unknown prefix is dropped, taking the attribute
prefix with it), `expand_iter`, preprocessors, the `@xmlns` dict, every
name check and both root errors. Values go through `str()`, so a nested
list is written as its repr, and a non-string `#text` raises
XMLGenerator's TypeError.

**expat** is mini_xml's parser turned into an event stream. It keeps
expat's messages and positions, and adds what xmltodict sees of expat's
set-up: names with and without namespace processing (`uri:local`, with
any separator), `StartNamespaceDecl` events, duplicate attributes after
expansion, and character data as `buffer_text` delivers it. Because
xmltodict installs a default handler when `disable_entities` is on, a
run of text is split at every entity reference, CDATA marker, comment
and processing instruction, internal entities are dropped from content
(but still expanded in attributes), and an undeclared entity is skipped
when the DOCTYPE names an external subset. `cdata_separator` makes all
of that visible.

Underneath are `xml.sax.saxutils` (`XMLGenerator`, `escape`,
`quoteattr`), `repr()` of dicts, OrderedDicts, lists and strings,
`str.isspace`, `str.strip`, `int()`, `json.load` and
`json.dumps(indent=2)`.

The run writes `data/` (fourteen documents, fifteen broken ones and
fourteen JSON documents), then works through `data/jobs.tsv`: every
document with the defaults, every broken one, 19 option sets over seven
documents, namespace maps, JSON output, streaming, round trips, JSON
documents turned into XML, and five reports:

- an S3 bucket listing summed by prefix and storage class;
- an RSS feed's items with authors, tags, GUIDs and enclosures;
- a GPX ride read point by point in streaming mode, with its climb;
- a SOAP price response read through a namespace map;
- an Android manifest's permissions and launcher activity.

`expected.out` is the output of `oracle.py`, which runs the same jobs
through xmltodict itself. `mk.py` writes `data/`; `gen_kara.py` prints
the data section at the end of `mini_xmltodict.kara`.
