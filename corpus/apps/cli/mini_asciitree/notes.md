# mini_asciitree

A port of [asciitree](https://pypi.org/project/asciitree/) 0.3.3 (MIT, Marc
Brinkmann): the renderer that draws a tree as indented ASCII or Unicode
box-drawing art. It is the shape every command-line tool reaches for when it has
a hierarchy to show — `tree`-style file listings, dependency graphs, a parse or
syntax tree, a nested-config dump — and the same left-aligned,
one-label-per-line layout underlies countless `--tree` flags.

## What it does

`mk.py` writes `data/jobs.tsv`: a sequence of trees to render, one node per
line, as three tab-separated fields `<style>\t<depth>\t<label>`. A depth-0 line
starts a new block (and carries that block's drawing style); the lines after it,
until the next depth-0 line, are its descendants — each attached to the most
recent earlier line of smaller depth. `oracle.py` renders each block through the
library; `mini_asciitree.kara` renders it through the port. Both print each
block under a header:

```
=== block 0 (ascii) ===
root
 +-- sub1
 |   +-- leaf1
 |   +-- leaf2
 +-- sub2
 |   +-- leaf3
 +-- sub3
```

The blocks exercise all five box styles — `ascii` (`+-|+`, the library
default), `light` / `heavy` / `double` (single-code-point box glyphs),
`blank` (indentation only) — plus the older `legacy` form (`  +--`), over trees
from a single node to a ten-deep filesystem.

`mini_asciitree.kara` reproduces the stream byte-for-byte on every backend.

## How the port works

The library represents a tree as nested ordered dicts and renders it with a
recursive `LeftAligned.render` that, for each child, draws a *head* line (the
junction glyph + the child's first line) and then *tail* lines (the child's
remaining lines, prefixed with a vertical-bar column for a non-last child or
blank padding for the last). The port reproduces that render exactly:

- The tree is an **arena of parallel `Vec`s** (a label and a child-id list per
  node), built from the depth column with an explicit parent stack, so the
  child order is the file order — never a hash-map walk.
- `render` is the same recursion, returning the node's block of lines; the
  **head / child-tail / last-tail** line builders are transcribed from
  `BoxStyle` and `LegacyStyle` (indent 1, horizontal run 2, label space 1).
- The glyphs are whole **strings concatenated at the `String` level**, so the
  multi-byte Unicode styles (`light`/`heavy`/`double`) render by code point: the
  last-child continuation column is one space because the library pads by
  `len(VERTICAL)`, which is one code point for every style.

The render walks the tree in build order only, so the output is deterministic —
the one precondition for `run == build` — and the whole thing is byte- and
string-level, so the four backends agree by construction.

## Scope

- Labels are **ASCII** and the layout constants are the `BoxStyle` defaults
  (`indent=1`, `horiz_len=2`, `label_space=1`); custom spacing and the
  `KeyArgsConstructor` tuning hooks are out of scope.
- The input is an explicitly-built ordered tree, which is how the library is
  used in practice (you hand it a dict you built). `DictTraversal` over an
  arbitrary mapping would inherit that mapping's iteration order; the port's
  build-order arena is the deterministic case.
- Styles covered: the four `BoxStyle` glyph sets, `BLANK`, and `LegacyStyle`.

The vendored library package imports only the standard library; `vendor/` is
otherwise left untouched.

## Running

```bash
karac run mini_asciitree.kara              # interpret
karac build --mir mini_asciitree.kara      # native binary
python3 -s -P -B mk.py                        # regenerate data/jobs.tsv
python3 -I oracle.py                           # regenerate expected.out
python3 -s -P -B gen_kara.py                   # regenerate the embedded jobs
```
