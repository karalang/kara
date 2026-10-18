# mini_xml

The XML layer under RSS readers, Maven builds, SVG tools and sitemap
crawlers, as Python 3.13's `xml.etree.ElementTree` does it over expat.
The program is a whole small ElementTree: a well-formedness parser, the
tree with `text` and `tail`, ElementPath queries and `tostring`.

The run writes five documents and eighteen broken ones into `data/`,
then prints:
- each document's tree, one element per line with its attributes,
  text and tail, and its serialization:
  - **feed.xml**: an RSS feed with the `dc` and Atom namespaces,
    CDATA, entities, a comment and a processing instruction;
  - **pom.xml**: a Maven POM in a default namespace, with `xsi`;
  - **icon.svg**: SVG with `xlink` attributes and non-ASCII text;
  - **config.xml**: a DOCTYPE with internal entities, and attribute
    value normalization of tabs and newlines;
  - **sitemap.xml**: a sitemap with an escaped query string;
- the results of the queries in `data/queries.txt`, run with `find`,
  `findall`, `findtext` and `itertext`;
- the error for each document in `data/bad/`, with expat's message,
  line and column.

**Parsing.** The parser reads bytes and keeps a stack of open elements.
It handles names, character and entity references, internal general
entities from the DOCTYPE (expanded recursively, to a depth limit),
CDATA sections, comments and processing instructions (dropped,
as ElementTree's default builder does), and the XML declaration.
Namespace declarations are scoped to their element, and every tag and
attribute name is expanded to ElementTree's `{uri}local` form. Line
ends are normalized first, and attribute values have their white space
turned into spaces. Errors carry expat's messages (`not well-formed
(invalid token)`, `mismatched tag`, `unbound prefix`, `junk after
document element` and the rest) at the position expat reports, with the
column counted in characters.

**ElementPath** is ported from CPython's `ElementPath.py`: the same
tokenizer, the same selectors (child, `*`, `.`, `..`, `//`, and the
`{*}name`, `{ns}*` and `{*}*` wildcards) and the same predicates (`[@a]`,
`[@a='v']`, `[@a!='v']`, `[tag]`, `[tag='v']`, `[.='v']`, `[n]`,
`[last()]` and `[last()-n]`). A position counts siblings with the same
tag, so `g/*[3]` finds nothing when no tag appears three times.

**tostring** names namespaces as ElementTree does: the prefixes it
knows by name (`dc`, `xsi`, `rdf` and a few more), else `ns0`, `ns1`, ...
in the order the names are first met. The declarations go on the root,
sorted by prefix, and empty elements are written `<tag />`.

Not supported:
- external entities, parameter entities and DTD validation;
- encodings other than UTF-8;
- namespace prefixes in ElementPath (the `namespaces` argument);
- `iterparse`, comments and processing instructions in the tree, and
  `tostring`'s other methods and options.

**Oracle.** `expected.out` is the output of `oracle.py`, which builds
the same report with ElementTree. Run it from this directory after
`mk.py` writes `data/`.

## Edits from mini_xml.orig.kara to mini_xml.kara

Two, both author errors:
- `let name = docs[d];` moved a `String` out of an array; it is now
  `docs[d].clone()` (a string literal in an array is a `String`).
- The parser struct was named `Parser`, which shadows the prelude type
  of that name; it is now `XmlParser`.

After that `karac check` passed, and the program matched expected.out
on the first run on all three MIR surfaces.

Written 2026-10-15 by the real-world apps thread; karac at 35fcd8197c.

## Back to the original, 2026-10-18

The rows behind the edits above are fixed, so `mini_xml.kara` is now exactly what `karac fix` makes of `mini_xml.orig.kara`. It matches expected.out on all four MIR backends (karac at 4ea06e27b9).
