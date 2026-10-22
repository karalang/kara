# mini_pyfiglet

A port of the FIGlet rendering engine at the heart of
[pyfiglet](https://github.com/pwaller/pyfiglet) 1.0.4 (MIT) — the library that
banner generators, CLI splash screens, build tools and chat bots use to turn a
short string into FIGlet ASCII art. This kata ports `Figlet.renderText` for the
standard font: laying glyphs left to right and smushing their touching edges
together the way pyfiglet does by default.

## What it does

`mk.py` writes `data/jobs.tsv`, one job per line:

    render \t <text>

`oracle.py` renders each `<text>` through the vendored pyfiglet in the standard
font and emits

    == render <text>
    <the six-row FIGlet rendering, trailing spaces and all>

`mini_pyfiglet.kara` reproduces `expected.out` byte-for-byte on every backend.
The inputs are short ASCII strings chosen to exercise the horizontal smushing
rules the standard font turns on — equal-character (`AVA`, `C++`), underscore
(`a_b_c`), hierarchy and bracket-pair (`O(1)`, `x|y`, `12/34`, `[a]{b}`) — plus
spaces between words (`foo bar`, `mix 9`) and the first-character edge case.

## How the port works

The standard font is six rows tall, and each glyph is a fixed grid of rows. The
font is dumped into `figfont.tsv` (one line per code: the code, a tab, then the
six rows separated by `\x1f`) and embedded by `gen_kara.py` as `font_text()`,
parsed at run time so the embedding stays a flat string rather than a large
table constructor. `$` is the font's **hardblank** — a placeholder that renders
as a space but resists being smushed, and is turned into a real space only at
the very end.

Characters are laid into a six-row buffer left to right. Before each glyph,
`smush_amount` measures how far the new glyph may overlap the buffer's right
edge without a collision (the minimum touchable gap across the six rows); each
overlapping column pair is then run through `smush_chars`, pyfiglet's
controlled-overlap rule set, and the non-overlapping tail is appended.

The standard font's layout mode is `24463`; rendering reads its low byte,
`143` = `SM_SMUSH | SM_EQUAL | SM_LOWLINE | SM_HIERARCHY | SM_PAIR`. So
`smush_chars` implements exactly those rules — a space yields to the other
side, a hardblank blocks the smush, equal characters merge, `_` yields to the
box-drawing hierarchy, the hierarchy tiers `|` < `/\` < `[]` < `{}` < `()` <
`<>` resolve, and a bracket pair `[]`/`{}`/`()` smushes to `|` — and omits the
bits the standard font leaves off (no hardblank-smush, no big-X, no kerning).

## Scope

Single-line rendering in the standard font, left-to-right, left-justified, at
the default width 80 (never exceeded by these inputs). The full engine's
width-wrapping, embedded newlines, blank-marker line breaking, centre/right
justification, right-to-left direction, colour output and the non-standard
fonts are out of scope — this kata is the core smushing renderer, not the CLI.
The vendored `pyfiglet/` is trimmed to the single `standard.flf` font it
renders with (the other ~550 fonts are never touched); its `__init__` version
lookup is wrapped so the vendored copy imports without installed dist metadata
(a packaging shim, no behaviour change).

## Running

```bash
karac run mini_pyfiglet.kara                  # interpret
karac build --mir mini_pyfiglet.kara          # native binary
python3 -s -P -B mk.py                            # regenerate data/jobs.tsv
python3 -I oracle.py                               # regenerate expected.out
python3 -s -P -B gen_kara.py                       # regenerate the embedded blocks
```
