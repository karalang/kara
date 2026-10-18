# mini_htmlparser

Python 3.13's `html.parser.HTMLParser`, `html.escape` and `html.unescape`
ported from `html/parser.py` and `html/__init__.py`, and four programs
built on the parser's events: a link extractor, a text extractor, a
table of contents and an allowlist sanitizer for user comments.

The run writes six pages into `data/pages/` (a blog post, a page of
tokenizer edge cases, a form, a page ending in `<plaintext>`, a user
comment with scripts and event handlers, and a page in German and
Japanese), then runs `data/jobs.tsv`, 84 jobs with tab-separated fields.
Each job prints `== ` and its fields joined by ` | `, then its output.

The jobs cover:
- `events`: every handler call with `getpos()` at the time of the call,
  for the pages and 21 small documents, with `convert_charrefs` on and
  off, fed whole and in chunks of 1, 5, 7 and 64 characters. The pages
  cover bare and quoted attributes, upper-case tags, `<br/>`, empty and
  abruptly closed comments (`<!---->`, `<!-->`, `<!--->`, `--!>`), bogus
  comments (`<!x>`, `</ >`, `</3>`), `</>`, CDATA, processing
  instructions, `<script>` and `<textarea>` content, references with
  and without `;`, invalid code points, and a quote that never closes.
  The small documents each stop inside a construct, so `close()` has to
  flush it;
- `unescape`: the HTML5 rules for named references with no `;`
  (`&ampx`, `&notit;`), numeric references in the Windows-1252 range,
  surrogates, out-of-range numbers, and two-character entities;
- `escape` with `quote` on and off;
- `links`: `href`, `src` and `action` values, the title and the meta
  description;
- `text`: the visible text, without script, style and title, with
  block tags as line breaks and `<pre>` kept as it is;
- `toc`: `h1` to `h3` with their ids;
- `sanitize`: allowed tags and attributes only, `javascript:` links
  removed, `rel="nofollow"` added, script, style, iframe and title
  dropped with their content, and text escaped.

**The port** follows `html/parser.py` line for line: `goahead` with its
end-of-buffer checks for a reference cut in half, `feed`'s pending
buffer and parse threshold, `parse_starttag` with
`check_for_whole_start_tag`, `parse_endtag`, `parse_comment` with the
abrupt closes, `parse_html_declaration`, the bogus-comment rules, and
CDATA and RCDATA content modes. Its regular expressions
(`locatetagend`, `tagfind_tolerant`, `attrfind_tolerant`, `charref`,
`entityref` and the content-mode `interesting` patterns) are written out
as scanners, including the places where `re` backtracks: an attribute
value whose quote never closes, and an entity name cut by the end of the
buffer. The text is a `Vec[char]`, so positions count code points as
Python's do. `html.unescape` and the attribute-value unescaping use the
whole `html.entities.html5` table, all 2231 names, which `gen_kara.py`
writes into the data section.

Not supported: `scripting=True` (`noscript` content), and calling
`_set_support_cdata`.

**Oracle.** `expected.out` is Python 3.13.16's output for the same jobs,
made by `oracle.py` with the real `html.parser`. Run `mk.py` first; it
writes `data/`. `gen_kara.py` prints the Kāra data section from the
same files and from `html.entities.html5`.

## Edits from mini_htmlparser.orig.kara to mini_htmlparser.kara

Author errors:
- Three calls passed a local `String` to a `mut ref String` parameter
  with no `mut` marker. `karac fix` added the markers.
- In the link extractor, `match title { Some(t) => title = ... }`
  moved `title` inside the loop. `karac fix` changed the pattern to
  `Some(ref t)`.
- In the table of contents, `match cur { Some(h) => h.text.push_str(s) }`
  also moved `cur`. `karac fix` changed it to `Some(ref h)`, and check
  then refused the write through `h`; the pattern is now
  `Some(mut ref h)`, as check's second message says.

Compiler gaps:
- B-2026-10-18-27: check still refused the link extractor's arm,
  `Some(ref t) => title = Some(f"{t}{s}")`, saying `t` is used after the
  assignment. It is used only in the value being stored, and both MIR
  surfaces run it. The arm now binds the value first:
  `let u = f"{t}{s}"; title = Some(u);`.
- B-2026-10-18-28: the `ref h` that `karac fix` wrote for the table of
  contents fails check; it should have been `mut ref h`.
- B-2026-10-18-29: with the table as one function of 2231 `insert`
  lines, mir-llvm-asan took 168 s to build and run the program (mir-llvm
  took 16 s), because its build time grows with the square of a
  function's length. `gen_kara.py` now writes the table as nine
  functions of 250 names, and the asan run takes 27 s.

After those edits the program matched expected.out on both MIR surfaces.

Written 2026-10-18 by the real-world apps thread; karac at 79a6c2eb9a.
