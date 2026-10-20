# mini_xpath

XPath 1.0 as libxml2 2.14 evaluates it (lxml's `xpath()`), and
`xmlstarlet sel` on top of it, over a small XML parser:
- the parser builds the XPath data model: elements with their namespace
  nodes in the order libxml2 walks them (the `xml` namespace, then the
  in-scope declarations, nearest first, in reverse), attributes, text
  with CDATA and entity references merged in, comments and processing
  instructions;
- the expression grammar of XPath 1.0, with the lexical rules that tell
  `*` and `div` apart from names, every axis, node tests with prefixes,
  predicates counted along reverse axes, filter expressions, unions and
  variables;
- the four value types and their conversions as libxml2 does them:
  numbers parsed by its own algorithm (`+5` and `1e3` are NaN) and
  printed by `xmlXPathFormatNumber` (`%d`, a 15-digit `%f`, or a trimmed
  `%.14e` outside 1e-5..1e9), comparisons across node-sets, and the
  error messages lxml raises;
- the 27 core functions, including libxml2's integer arithmetic in
  `substring`, `round` keeping negative zero, and `lang`.

`xmlstarlet sel` options `-N -t -m -s -i -b -v -c -o -n` run as the
stylesheet xmlstarlet writes for them: `xsl:for-each` with stable
`xsl:sort` on text or number keys (NaN first, descending flips the keys
but not the tie-break), `xsl:if`, and its value-of template that prints
every node of a node-set on its own line.

`mk.py` writes `data/`: five documents (a book catalogue, an Atom feed
with Dublin Core and Media RSS, an XHTML page, a Maven POM and a
warehouse inventory with prefixed attributes), 333 queries with their
namespace and variable bindings, and 14 `xmlstarlet sel` jobs.

`oracle.py` runs the queries through lxml, and the jobs through libxslt
with the stylesheets xmlstarlet would generate, and wrote
`expected.out`.

It matched on every backend the first time it ran, once `karac fix` had
added the call-site `mut` markers; no gaps were found.
