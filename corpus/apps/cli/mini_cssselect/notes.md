# mini_cssselect

cssselect 1.2.0, the library that turns CSS selectors into XPath, with
parsel's translator on top. lxml's `cssselect()`, Scrapy's
`response.css()` and parsel all go through it: a spider's
`h3 > a::attr(href)` becomes `descendant-or-self::h3/a/@href`. The port
covers:
- the tokenizer, with escapes (`#\31 23`, `.caf\E9 `), strings, numbers,
  comments and the positions in its error messages;
- the parser: combinators, namespaces, ids, classes, attribute selectors
  and their operators, pseudo-classes and functions, `:not()`, `:has()`,
  `:is()` and `:where()`, pseudo-elements, and the parsed objects'
  `repr()`, `canonical()` and `specificity()`;
- the translators: `GenericTranslator`, `HTMLTranslator` (lower-case names,
  and its own `:checked`, `:link`, `:disabled`, `:enabled` and `:lang()`)
  and parsel's `HTMLTranslator` with `::text` and `::attr()`, with
  `xpath_literal` and the an+b series of `:nth-child()` and friends.

`mk.py` writes `data/`: four spider configs (`field = selector` lines for
books.toscrape.com, quotes.toscrape.com, a job board and a shop), a site
stylesheet, and 45 broken selectors.

The run writes `data/`, then prints for each selector its parsed tree,
canonical form and specificity, and the XPath of each translator or the
error it raises; then a summary per file and the stylesheet's selectors
sorted by specificity, the order the cascade applies them in.

`oracle.py` runs the same through cssselect (the copy in `cssselect/`,
unchanged) and parsel's `csstranslator.py` (in `parsel/`, unchanged) and
wrote `expected.out`. `gen_kara.py` writes the data section at the end of
the program. `parse()`'s fast path for a lone element, id or class is left
out: it builds the same trees as the full parser.

The quirks are kept. Some examples:
- `:is(a,b)` fails with "Expected selector, got <DELIM ')' at 7>": after
  a comma the argument parser takes one more token, which is meant to be
  the space, so `:is(a, b)` works and `:is(a,b)` loses the `b`. At the end
  of the input the same step runs off the token generator, and Python
  turns the StopIteration into "RuntimeError: generator raised
  StopIteration".
- `:has()` parses its argument by joining the token values into a new
  selector string, so `li:has(- a)` parses with a `-` combinator, which
  the translator then has no method for (a KeyError).
- A comment between two spaces leaves two whitespace tokens, so
  `ul /* the menu */ > li` is a syntax error.
- The descendant combinator is canonical as three spaces (`h3   a`), and
  `div:is(.alert)` has the specificity of `.alert` alone: `:is()` takes
  the maximum of its list and forgets the element.
- `li:first_child` works, because the method is looked up with dashes
  turned into underscores; `2N+1` is an invalid series, because only a
  lower-case `n` splits it.
- `:has(> span.remote)` gives `li[@data-job-id][./span][@class ...]`: the
  child relation closes its bracket before the class condition.
- `::attr(xlink:href)` is a syntax error, since a colon cannot be an
  argument.

License: cssselect and parsel are BSD (`LICENSE-cssselect`,
`LICENSE-parsel`).
