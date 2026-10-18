# md_render

A CommonMark Markdown-to-HTML renderer and the docs-site build around it.
It writes a README, a CHANGELOG and a user guide to `docs/`, renders each
one, and writes `site/<name>.html`. Each page gets heading ids, a table of
contents, and `.md` links rewritten to `.html`. Then it renders 42 small
inputs taken from the corners of the CommonMark spec.

**Blocks.**
- ATX and setext headings, and thematic breaks.
- Indented code, and fenced code with an info string.
- Block quotes and paragraphs.
- Bullet and ordered lists, nested by content column, with the
  tight/loose rule.
- Link reference definitions.

**Inlines.**
- Backslash escapes and code spans.
- Emphasis and strong emphasis, by the spec's delimiter-run algorithm
  with its flanking rules and rule of three.
- Inline, full, collapsed and shortcut links, images and autolinks.
  Destinations are percent-encoded the way markdown-it does it.
- Soft and hard line breaks.

Raw HTML, entity references, tabs and lazy continuation lines are not
supported. The documents avoid them.

**Oracle.** `md_render.py` is a port, and `expected.out` is its output.
The two runs write byte-identical `site/` pages. `check_markdown_it.py`
checks that port against `markdown-it-py`'s CommonMark preset: all 45
inputs (the three documents and the 42 cases) render exactly the same.

## Edits from md_render.orig.kara to md_render.kara

Author errors (the diagnostics were right):
- I called `trim_end_matches(' ')` and `trim_matches(' ')`, which the
  library doesn't have. Two small helpers replace them.

Compiler gaps, worked around and listed in the source header:
- **B-2026-10-15-29 (new):** a struct declared after an enum that has a
  variant of the same name is refused as "already defined in this scope",
  by check and by every v2 surface. Declared before the enum, it is
  accepted. The common `enum Shape { Circle(Circle) }` shape fails the
  same way when the struct comes second. Here `struct Heading` is moved
  above `enum Block`.

Written 2026-10-15 by the real-world apps thread; karac at fee58fc7e2.

## Library migration, 2026-10-18

Thread A's a0ca700228 and acd593fc77 changed the library to match docs/library: a number's `parse` returns `Result[T, ParseError]`, and `split`, `lines`, `split_whitespace` and `trim` give `Str` views (`split` and `lines` are now lazy iterators). Arguments forwarded to a `mut ref` parameter are now marked too. `karac fix` made the edits. The app matches expected.out on all four MIR backends again (karac at a4b0967495).
