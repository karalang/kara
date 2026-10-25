# mini_markdown

A port of [Python-Markdown](https://pypi.org/project/Markdown/) 3.11.2
(BSD-3-Clause; Manfred Stienstra, Yuri Takhteyev, Waylan Limberg and
others), the Markdown-to-HTML library behind MkDocs, with all of its
bundled extensions but the Pygments highlighting. It runs 808 of the
library's own test cases and then builds a small documentation site with
the extensions a site uses. The vendored copy in `vendor/markdown/` is the
package, unchanged.

```
== 390 extensions/test_footnotes:TestFootnotes.test_backlink_title#1 | footnotes
<p>paragraph<sup id="fnref:1"><a class="footnote-ref" href="#fn:1">1</a></sup></p>
<div class="footnote">
<hr />
<ol>
<li id="fn:1">
<p>A Footnote&#160;<a class="footnote-backref" href="#fnref:1" title="Jump back to footnote">&#8617;</a></p>
</li>
</ol>
</div>
...
== site | meta, toc, extra, admonition, codehilite, smarty, wikilinks
-- index.md: Harbor (order 1)
<h1 id="harbor">Harbor<a class="headerlink" href="#harbor" title="Permanent link">&para;</a></h1>
...
-- search
index.html#why-harbor 2 Why Harbor
index.html#quick-start 2 Quick start
...
-- files
out/index.html 4050 bytes fnv 9c415b8c
out/install.html 2331 bytes fnv 65349c80
...
```

## What it does

`extract_cases.py` reads the `assertMarkdownRenders` calls out of the test
suite of the Markdown sdist without running it: it walks each test module's
syntax tree and evaluates the source text, the keyword arguments and the
extension objects from a small set of node kinds. `mk.py` writes those 808
cases to `data/jobs.txt` (708 from the syntax tests, 100 from the legacy
file tests), with each extension's settings, and copies seven pages into
`data/site/`. `oracle.py` runs every job through the vendored package with
Pygments blocked and prints the HTML, plus `md.toc` and `md.toc_tokens`
where the test reads them. Then it builds the site: each page goes through
`meta`, `toc` (permalinks, levels 2 and 3), `extra`, `admonition`,
`codehilite`, `smarty` and `wikilinks`; pages are ordered by their `order`
metadata; the body, the table of contents and the metadata are printed, a
search index is made from the toc tokens, and each page is written to
`data/out/` inside a template with a navigation list, as its size and
FNV-1a hash.

`mini_markdown.kara` is the library, translated module by module:

- **Core**: the whitespace normaliser, the raw-HTML preprocessor (an
  `html.parser` subclass, so CPython 3.13's `html.parser` is ported with
  it), the block parser with its state stack and every block processor, the inline processor with its placeholder stash and all the
  inline patterns, the delimiter processor that pairs `*` and `_` runs into
  `<strong>` and `<em>`, prettify and unescape, and the raw-HTML and
  ampersand postprocessors. ElementTree is a node arena that keeps
  `AtomicString` as a flag and serialises as Markdown's serializer does.
- **Extensions**: abbr, admonition, attr_list, codehilite, def_list, extra,
  fenced_code, footnotes (with reordering and duplicate back-references),
  legacy_attrs, legacy_em, md_in_html, meta, nl2br, sane_lists, smarty,
  tables, toc (slugs, unique ids, nesting, anchors, permalinks, markers,
  `data-toc-label`) and wikilinks.

Regular expressions are Python's: each pattern is compiled from its
Python source by the mini_re engine (a port of `sre`), which this program
includes, and matched over code points; text is UTF-8 with byte offsets,
and the regex layer converts between the two. `html.entities.html5`, the
HTML 4 code point names and an NFKD-to-ASCII table for `toc`'s slugs are
generated into the program by `gen_kara.py` from this Python.

Not ported: Pygments (codehilite takes its no-Pygments path, as the oracle
does with Pygments blocked), the recursion-limit check of the block
parser, entry-point loading of third-party extensions, and the deprecated
`Pattern` and `markdown.util` aliases.

## Edits from mini_markdown.orig.kara to mini_markdown.kara

`karac check` gave 88 errors, most of them repeats of a few causes.
`karac fix` applied 22: `#[derive(Clone)]` on eight types (kept), `: Str`
on four entity bindings for smarty and `.clone()` at their second use,
two `ref` pattern bindings and one `.to_string()` (kept). The `.clone()`s
were dropped: a `Str` is `Copy`, so the move they answer only existed
because the binding was a `String` before the other fix, and `Str.clone()`
is a `String`, which the next check refused (kara B-2026-10-25-19, open).
The rest by hand: 23 byte comparisons written `'#' as u8` became
`'#' as u32 as u8`, as the diagnostic says, and three discarded `Option`s
from an attribute delete became `let _ =`.

Then check was clean, and on the MIR interpreter 806 of the 809 sections
matched the oracle at once. The three that did not were mine: the
emphasis builder tested `el.text is None` with the tree's truthiness test,
so an empty text was filled in again. With `text_none` it matches the
oracle under the MIR interpreter, the LLVM backend, the ASan build and
the native binary, with nothing on stderr.

## Running

```
python3 -I -B extract_cases.py <Markdown-3.11.2 sdist>/tests > cases.json
python3 -s -P -B mk.py            # writes data/jobs.txt and data/site/
python3 -I -B oracle.py > expected.out
python3 -I -B gen_kara.py         # embeds data/ and the tables in mini_markdown.kara
karac run mini_markdown.kara
```

The program writes `data/` from what it carries, reads the jobs back and
writes the site to `data/out/`. About 4.5 minutes on the MIR
interpreter (a debug build of `karac`, past the corpus runner's default
60 s timeout) and 0.2 s built.

## License

Python-Markdown is under the BSD 3-Clause license in `vendor/LICENSE.md`,
which also covers the test cases in `cases.json`:

```
BSD 3-Clause License

Copyright 2007, 2008 The Python Markdown Project (v. 1.7 and later)
Copyright 2004, 2005, 2006 Yuri Takhteyev (v. 0.2-1.6b)
Copyright 2004 Manfred Stienstra (the original version)

Redistribution and use in source and binary forms, with or without
modification, are permitted provided that the following conditions are met:

1. Redistributions of source code must retain the above copyright notice, this
   list of conditions and the following disclaimer.

2. Redistributions in binary form must reproduce the above copyright notice,
   this list of conditions and the following disclaimer in the documentation
   and/or other materials provided with the distribution.

3. Neither the name of the copyright holder nor the names of its
   contributors may be used to endorse or promote products derived from
   this software without specific prior written permission.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS"
AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE
IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE
FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL
DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR
SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER
CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY,
OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
```

The site pages, the job runner, the oracle and the Kāra port are this
repo's.
