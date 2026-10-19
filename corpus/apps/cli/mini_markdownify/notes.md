# mini_markdownify

HTML to Markdown, as markdownify 1.2.3 does it. markdownify is the
converter that documentation pipelines, scrapers and LLM ingestion
tools run pages through. It is ported from the `markdownify.py` module
vendored here (MIT, see `LICENSE-markdownify`). It sits on the parse
tree Beautiful Soup 4.15.0 builds with `html.parser` (MIT, see
`LICENSE-bs4`).

**The tree** is built by bs4's `BeautifulSoupHTMLParser` handlers over
mini_htmlparser's port of Python 3.13's `html.parser`, with
`convert_charrefs` off:

- bs4's own numeric references: Windows-1252 for 128 to 159, and U+FFFD
  for 0, surrogates and out-of-range numbers.
- bs4's named-entity table, with `&name` kept as text when unknown.
- Empty-element tags, including `</br>` after `<br>` being dropped.
- Duplicate attributes, where the last value replaces the first.
- Comments, doctypes, CDATA and processing instructions.
- Whitespace-only strings folded to `" "` or `"\n"` outside `<pre>` and
  `<textarea>`.
- `_popToTag`, which closes the most recent open tag of the name and
  ignores an end tag that matches none.

**The converter** follows markdownify's `MarkdownConverter`:

- The whitespace rules around block elements, and the newline collapse
  between children.
- Headings in the underlined, ATX and closed-ATX styles, with `h7` and
  `h0` clamped and `h12x` still a heading.
- Emphasis and strong with `*` or `_`, and `del`, `s`, `sub` and `sup`,
  including symbols written as tags.
- Links with titles, autolinks (escaped underscores included) and
  `default_title`.
- Images, with `keep_inline_images_in`, and videos with posters and
  `<source>` children.
- Code spans with fences one backtick longer than any run inside.
- `<pre>` blocks with `code_language` and the four `strip_pre` modes.
- Block quotes.
- Ordered lists with `start`, and nested lists cycling through the
  `bullets`.
- Definition lists.
- Tables with header rows, `table_infer_header`, `colspan` and
  `thead`/`tbody`.
- `<br>` in the spaces and backslash styles.
- Markdown escaping, with the four `escape_misc` regexes.
- Paragraph wrapping through `textwrap.fill`.
- The `strip` and `convert` tag lists, and the `strip_document` modes.
- markdownify's ValueErrors.

The regexes run on mini_re's engine, and wrapping uses mini_textwrap's
`TextWrapper`.

The run writes six pages into `data/pages`:

- A blog post.
- A CLI reference with tables and definition lists.
- A table-layout email newsletter.
- A rendered GitHub README.
- A Wikipedia-style article.
- A page of edge cases: broken nesting, stray end tags, custom and
  namespaced elements, character references, Markdown specials and
  empty elements.

It then runs 75 jobs: 37 page conversions under 20 option sets from
`data/options.tsv`, and 38 fragments. `expected.out` is the output of
`oracle.py`, which runs the same jobs through markdownify itself.
`mk.py` writes `data/`, and `gen_kara.py` prints the data section at the
end of `mini_markdownify.kara`.
