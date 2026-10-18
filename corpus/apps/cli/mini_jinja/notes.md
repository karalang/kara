# mini_jinja

A template engine for a subset of Jinja2, used the way a static-site
generator uses one. The run writes six templates and a blog's data
(`data/site.json`) to disk, reads them back, and renders into `out/`:
- `index.html`, the home page: it extends `base.html`, overrides two
  blocks (one through `super()`), imports the `card` macro, loops over
  the posts with an `if` filter, `loop.first`/`loop.length` and a
  for-`else`, and indents each card with `indent`;
- one `<slug>/index.html` per published post: `import .. as m`,
  `join`/`wordcount`, a macro called with keyword arguments, `is
  defined`, `safe`, `set` with a conditional expression, `capitalize`;
- `digest.txt`, plain text with autoescaping off: `namespace()` and `set
  ns.x` across loop iterations, `rejectattr`/`selectattr`/`map`/`list`/
  `sum`/`dictsort` with tuple unpacking, `"%2d"|format`, `replace`,
  `title`, `round`, `or`, inline `if` without `else`, and `raw`.

`base.html` loops over the navigation and includes `footer.html`, which
slices the commit hash. `macros.html` holds three macros with default
arguments, whitespace control (`{%-`, `-%}`), `sort`, `int`, `round` and
string repetition. Each page is printed after a `== path (N bytes)` line.
Then ten one-line templates are rendered and their output or error is
printed: an unclosed `for`, an unknown filter, an attribute of an
undefined name, a stray `endif` (alone, and as a nesting mistake), a
division by zero, an incomplete expression, too many macro arguments,
an attribute of an integer, and `~` with and without safe markup.

**The engine.** A lexer that applies Jinja's `trim_blocks`,
`lstrip_blocks` and `-`/`+` rules exactly as Jinja's lexer does, a
recursive-descent parser with Jinja's precedence (conditional
expressions, `or`, `and`, `not`, comparisons, `+ -`, `~`, `* / // %`,
`**`, unary minus, then postfix, filters and tests), and a tree-walk
renderer. Values are the JSON types plus markup-safe strings, Undefined
(which carries the message an access through it raises) and macros;
lists and dicts are shared, as in Python. `extends` keeps a chain of
block definitions, child first, which `super()` walks. Macros close over
the scopes where they were defined, so an imported macro sees its own
module. Error messages are Jinja's, including the end-tag bookkeeping
behind "Unexpected end of template" and "You probably made a nesting
mistake". `round` and `%f` round the double's exact decimal value, ties
to even, as Python does (`4.25|round(1)` is `4.2`).

Not supported: `call` blocks, `caller()`, `with`, `filter` blocks,
recursive loops, `loop.cycle`, dict literals, `is` tests with arguments
beyond the seven listed in the source, and line statements. Case
mapping and `\w` are ASCII.

**Oracle.** `expected.out` is Jinja2 3.1.6's output on the same
templates and data, made by `render.py` (run from this directory) with
`trim_blocks`, `lstrip_blocks`, `keep_trailing_newline` and autoescaping
for `.html` and for string templates. The `out/` files the run writes
are byte-identical to the ones `render.py` writes.

## Edits from mini_jinja.orig.kara to mini_jinja.kara

Author errors (the diagnostics were right):
- `sort` and `sum` matched their optional `attribute` argument by value
  inside a loop, which moves it on the first item. They match a `ref`
  binding.

Compiler gaps, worked around and cited in the source:
- B-2026-10-15-37 (new): `karac check` reads a method call on a
  parameter named `ast` (or `compiler`) as a call into the comptime
  module of that name. The syntax-tree parameter is `tree`.
- B-2026-10-15-38 (new): `karac check` takes `f64.parse(t.text)` as a
  move of the field `t.text`. The lexer parses the number from a local.
- B-2026-10-15-20: `karac check` reports a loop-local `name`, read in an
  `if` and then moved into an enum constructor inside a call, as moved
  on the previous iteration. The parser clones the name.
- B-2026-10-15-40 (new): on MIR, a user enum variant named `None` takes
  over every bare `None` of an `Option` ("invalid MIR: Option takes 1
  fields, got 0"). Python's None is `Val.Nil`.

Written 2026-10-15 by the real-world apps thread; karac at 2cec444448.

Reverted 2026-10-15, karac at aec4d15cc0: B-2026-10-15-37 and
B-2026-10-15-40 are fixed, so the syntax tree parameter is `ast` again
and Python's None is `Val.None`. Each of these is the original line
again, and the app still matches on all four MIR surfaces.

## Library migration, 2026-10-18

Thread A's a0ca700228 and acd593fc77 changed the library to match docs/library: a number's `parse` returns `Result[T, ParseError]`, and `split`, `lines`, `split_whitespace` and `trim` give `Str` views (`split` and `lines` are now lazy iterators). Arguments forwarded to a `mut ref` parameter are now marked too. `karac fix` made the edits. The app matches expected.out on all four MIR backends again (karac at a4b0967495).
