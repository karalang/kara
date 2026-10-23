# mini_htmlmin

A port of [htmlmin](https://pypi.org/project/htmlmin/) 0.1.12
(BSD-3-Clause; Dave Mankoff), the pure-Python HTML minifier that web
build steps and Django and Flask apps call to shrink pages before they
are served. It collapses whitespace, drops comments, removes optional
attribute quotes and the values of empty and boolean attributes, leaves
`<pre>`, `<textarea>`, `<script>` and `<style>` alone, and drops closing
tags HTML lets a document omit.

```
> snippet <ul><li>a<li>b</ul><p>x<p>y<div>z</div>
  | <ul><li>a<li>b</ul><p>x<p>y</p><div>z</div>
> snippet <a title='He said "hi"'>1</a><a title="it's">2</a><a title="a'b&quot;c">3</a>
  | <a title='He said "hi"'>1</a><a title="it's">2</a><a title="a'b&#34;c">3</a>
```

## What it does

`mk.py` writes 212 jobs (and 6 section comments) to `data/jobs.tsv`,
seven pages (a blog post, a form, a data table, an HTML email, an app
shell, a broken page and one with CRLF line ends), a small site with a
manifest, and the parser's regular expressions to `data/patterns.txt`.
`oracle.py` runs each job against the vendored library and prints the
result, or `error <class>: <message>` when it raises; `mini_htmlmin.kara`
runs the same jobs. 5 jobs raise.

- **`minify`** runs each page through `htmlmin.minify` with ten option
  sets: the defaults, `remove_comments`, `remove_empty_space`,
  `remove_all_empty_space`, `reduce_boolean_attributes`, kept quotes,
  kept empty attributes, `convert_charrefs=False`, `keep_pre` and custom
  `pre_tags` / `pre_attr`.
- **`snippet`** minifies 79 short documents, each aimed at one rule:
  whitespace inside text, between tags and in `<title>`, conditional and
  `<!--!` comments, the doctype, attribute quoting and escaping,
  `pre-` attributes (including the one the library raises on), self-closing
  tags, boolean attributes, the `pre` attribute and `pre_tags`, CDATA
  content, entity and character references with and without
  `convert_charrefs`, optional closing tags and stray end tags, `lang`
  inheritance, processing instructions and marked sections, unclosed
  constructs, uppercase tags, and non-ASCII and Unicode whitespace.
- **`chunks`** feeds a page to one `Minifier` in pieces of a given size,
  checks the result equals one `minify` call, and reuses the Minifier.
- **`attr_value`**, **`ambiguous`**, **`unescape`** and **`escape_tag`**
  call `escape_attr_value`, `escape_ambiguous_ampersand`, the parser's
  `unescape` and `escape_tag` directly.
- **`cli`** runs the `htmlmin` command with its options on a page and
  shows the file it writes.
- **`site`** is a build step: it minifies the pages a manifest lists
  (each with its own options) into `site/out`, and reports the bytes
  saved per page and in total, skipping a page that is missing.

## How the port works

- **The whole parser stack is ported.** htmlmin's `HTMLMinParser`
  subclasses its own copy of Python's `html.parser.HTMLParser`
  (`python3html`), which sits on `_markupbase.ParserBase`, so `goahead`,
  the `parse_*` methods, marked sections and comments are transcribed
  along with the minifier's handlers.
- **Its regular expressions run on the mini_re engine.** The parser's
  patterns (`interesting_normal`, `locatestarttagend_tolerant`,
  `attrfind_tolerant` and the rest) are written to `data/patterns.txt`
  from the library's own text and compiled by the engine from
  [mini_re](../mini_re), which has Python `re` semantics. One matcher is
  made per `goahead` call and pointed at each pattern in turn, so the
  buffer is not copied for every match. htmlmin's whitespace regexes
  match only ASCII classes and are written as plain scans.
- **Entities come from mini_htmlparser.** `html.escape`, `unescape` and
  the HTML5 entity table are the ones from
  [mini_htmlparser](../mini_htmlparser).
- **The library's quirks are kept.** `HTMLMinParser` turns
  `convert_charrefs` on after the base class has set up, so text keeps
  its references as written while attribute values are unescaped. A
  `pre-` attribute value holding `"` but no `'` raises the library's
  `AttributeError` (it reads `escape.SINGLE_QUOTES`, which does not
  exist), one with both quote kinds logs the library's warning, and an
  unclosed `<script>` loses its content at the end.
- **The command reads lines as codecs does.** `htmlmin.command` feeds the
  input as `str.splitlines(keepends=True)` gives it (so `\r\n`, `\v`,
  `\x1c`-`\x1e`, `\x85` and ` ` all end a line) and writes the
  parser's result without calling `close()`, so text still held back at
  the end of the input is not written. `-H` and
  `--remove-all-empty-space` are accepted and ignored, as in the library.

## Compiler gaps

None new. `karac check` asked for one `.into_iter()` on a `for` loop
that moved its elements, a `ref` binding in a match over an `Option` it
then reused, and a `ref` on a function returning a borrowed field, all
language rules. After that the interpreter matched the oracle byte for
byte on its first run, and the LLVM, ASan and native builds matched too.

## Scope

- Logging is printed as the oracle configures it (`log <level>
  <message>`), rather than through a logging framework.
- `getpos()` line and column tracking is not kept; nothing in htmlmin
  reads it.
- The command's stdin and stdout paths and its `-e` encodings are not
  exercised; inputs and outputs are UTF-8 files.
- The WSGI middleware and the decorator are not ported; they wrap
  `minify` for frameworks.

## Running

```bash
karac run mini_htmlmin.kara                  # interpret
karac build --mir mini_htmlmin.kara          # native binary
python3 -s -P -B mk.py                       # regenerate data/
python3 -I -B oracle.py > expected.out       # regenerate expected.out
python3 -I -B gen_kara.py                    # regenerate the embedded data
```
