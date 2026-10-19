# mini_soupsieve

CSS selectors on an HTML tree, as Soup Sieve 2.9.2 does them. Soup Sieve
is the engine behind Beautiful Soup's `select()`, `select_one()` and
`css.*` calls, which scrapers, crawlers and HTML test suites lean on.
It is ported from the `soupsieve` package vendored here (MIT, see
`LICENSE-soupsieve`). It runs on the parse tree Beautiful Soup 4.15.0
builds with `html.parser` (MIT, see `LICENSE-bs4`), built the same way
as in mini_markdownify, plus bs4's multi-valued attributes (`class`,
`rel`, `headers` and the rest split on whitespace).

**The parser** follows `css_parser.CSSParser`:

- The tokenizer's regexes for every selector piece, run lazily so a
  later syntax error does not hide an earlier one.
- Tags, namespaces, ids, classes, and attribute selectors with all
  seven operators and the `i`/`s` flags, compiled to the same regexes.
- The combinators, including relative selectors under `:has()`.
- `:is()`, `:where()`, `:not()` and `:has()`, with forgiving lists.
- `:nth-*` in every `an+b` form, with `of S`.
- `:lang()`, `:dir()`, `:contains()` and the `-soup-contains*` family.
- Every simple pseudo-class Soup Sieve defines, expanded from its own
  source selectors.
- Custom selectors (`:--name`), with their recursion and redefinition
  errors.
- CSS unescaping and `css_escape`.
- Soup Sieve's `SelectorSyntaxError` messages with their context lines,
  and its ValueErrors, including the nesting limit and `chr()` overflow.
- `repr()` of the compiled `SelectorList`, which the parse jobs print.

**The matcher** follows `css_match.CSSMatch`:

- `select` with a limit, `select_one`, `match`, `closest` and `filter`,
  from any scope element.
- Namespace prefixes on tags and attributes.
- `:root`, `:scope`, `:empty`, `:defined` and the form-state classes
  (`:checked`, `:default`, `:indeterminate`, `:placeholder-shown`,
  `:in-range`, `:out-of-range` with dates, weeks, months and times).
- `:dir()` with the bidi classes of the text, and `:lang()` with
  `<meta http-equiv>` and wildcard ranges.
- Soup Sieve's quirks, kept as they are: the document matches `*` as a
  parent, so `* > html` matches `<html>`; the `:lang` meta cache;
  `:contains` reusing the text it computed for its first list; and
  `nth` using Python's floor division.

Attribute regexes run on mini_re's engine.

The run writes seven pages into `data/pages`:

- A shop category page.
- A signup form with eighteen input types.
- A three-script article with `lang` and `dir` attributes.
- A latency report table.
- A page of edge cases: processing instructions, comments, CDATA,
  custom and namespaced elements, and odd attributes.
- A fragment with no `<html>`.
- A document with one element.

It then runs 470 jobs from `data/jobs.tsv`: 108 parses, 17 escapes, 306
selects, and 39 `select_one`, `match`, `closest`, `filter` and attribute
jobs. One job calls `soupsieve.select()` with `custom=`, which that
helper silently drops. `expected.out` is the output of `oracle.py`,
which runs the same jobs through Soup Sieve itself. `mk.py` writes
`data/`, and `gen_kara.py` prints the data section at the end of
`mini_soupsieve.kara`.
