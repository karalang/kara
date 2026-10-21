# mini_mistune

Markdown to HTML as mistune 3.0.2 does it. mistune is the Markdown
parser behind nbconvert (Jupyter's exporter), Sphinx extensions and
many static site and docs tools. The port covers:
- the block parser: ATX and setext headings, indented and fenced code,
  block quotes, ordered and bullet lists (tight and loose), thematic
  breaks, raw HTML blocks, reference link definitions and paragraphs;
- the inline parser: escapes, code spans, emphasis and strong, links
  and images (inline and by reference), autolinks, inline HTML, hard
  and soft line breaks, and its precedence scan, where a rule starting
  inside marked text takes over if it runs past the end;
- the 15 plugins: strikethrough, mark, insert, superscript, subscript,
  footnotes, table, url, abbr, def_list, math, ruby, task_lists,
  spoiler and speedup;
- the directives, in both the reStructuredText (`.. note::`) and the
  fenced (```` ```{note} ````) syntax: admonitions, toc, include, image
  and figure;
- the four renderers: HTML (escaping or not), Markdown, reStructuredText
  and the AST, printed as JSON;
- the toc helpers, `add_toc_hook` and `render_toc_ul`;
- the command line, `python -m mistune`, with its `-m`, `-f`, `-p`,
  `-r`, `--escape`, `--hardwrap`, `-o` and `--version` options.

The parser is driven by mistune's own regular expressions, compiled
by the regular expression engine copied from `mini_re`. Each pattern
is compiled once and each search runs against the text in place.

The run writes `data/` and works through `data/jobs.txt`. It defines 8
configurations and then:
- renders the 75 cases in `data/cases.md` under 7 of them;
- builds a small docs site of 8 files into `data/out/`, with a page
  template and a table of contents from the headings, then reads each
  page back;
- checks every link in the site: external, ok or MISSING;
- runs the command line 10 times.

`oracle.py` runs the same jobs through mistune itself and wrote
`expected.out`. It uses the Python files of the 3.0.2 wheel in
`mistune/`, unchanged. mistune is BSD-3-Clause licensed; see
`LICENSE-mistune.txt`.

The quirks are kept. Some examples:
- `parse_link_text` compares the whole match against `]`, not the
  marker character.
- The list parser makes its narrowed patterns by replacing the first
  `3` in each pattern, wherever it is.
- Footnotes are rendered in a fresh environment, so reference links
  inside a footnote do not resolve.
- The toc directive gives headings ids, and `add_toc_hook` overwrites
  them later.
- The reStructuredText renderer refuses a table, and fails with
  `KeyError: 'prev'` on a block quote inside a list.

Not ported: the `RSTDirective` and `FencedDirective` registration API
for user directives and plugin loading by dotted path.
