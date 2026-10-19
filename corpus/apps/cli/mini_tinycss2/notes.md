# mini_tinycss2

tinycss2 1.4.0, the CSS Syntax Level 3 parser that WeasyPrint, bleach's
CSS sanitizer and many HTML-to-PDF and e-mail tools use to read
stylesheets. The port covers:
- the tokenizer, with its line and column bookkeeping, escapes, URL and
  unicode-range tokens, and the error tokens (bad string, bad URL, EOF
  in a string or a URL, unmatched brackets);
- the parsers: component values, one component value, declarations
  (with `!important`), declaration lists, blocks contents (declarations
  and nested rules, through tinycss2's `itertools.chain` fallback),
  rules, rule lists and stylesheets;
- serialization, with the token pairs that need a `/**/` between them;
- An+B parsing for `:nth-child()` and friends;
- CSS Color Level 3 and Level 4: keywords, hex, `rgb()`, `hsl()`,
  `hwb()`, `lab()`, `lch()`, `oklab()`, `oklch()` and `color()`, and the
  conversions `Color.to` implements (HSL and HWB to sRGB, Lab and LCH to
  XYZ D50, OKLab and OKLCH to XYZ D65, and those to Lab);
- `parse_stylesheet_bytes`: webencodings' labels, BOM sniffing, the
  `@charset` byte pattern, protocol and environment encodings, with
  UTF-8, UTF-16 and the single-byte codecs decoding with `replace`.

The run writes `data/` and works through the css-parsing-tests suites
(the CC0 test data tinycss2 and rust-cssparser share) the way tinycss2's
own `tests/test_tinycss2.py` does: 26 suites, 4893 cases, each printed
as its JSON and checked against the expected value in the suite. `mk.py`
keeps every third case of the four 1500-case Lab, LCH, OKLab and OKLCH
suites, to keep the interpreter's run short.

Then six stylesheets are audited: a design system, a newsletter's
inlined CSS (with a bad URL and an unterminated string that swallows
the rest of the sheet), a Latin-1 intranet theme with `@charset`, a
KOI8-R catalogue served with that protocol encoding, a UTF-16 export
with a BOM, and an ISO-8859-2 menu read with that environment encoding.
For each rule the audit prints its declarations, the colors in them
(as hex, or as Lab for the Lab family), duplicate properties, WCAG
contrast between `color` and `background-color`, unicode ranges and the
`:nth-child()` arguments with the first indices they match. Each sheet
ends with its counts, custom properties, URLs and a minified copy, and
the run ends with the palette.

`oracle.py` runs the same suites and the same audit through tinycss2
itself and wrote `expected.out`. It uses the Python files of tinycss2
1.4.0 in `tinycss2/` and of webencodings 0.5.1 in `webencodings/`,
unchanged. `sheets/make_bytes.py` wrote the four sheets that are not
UTF-8.

The quirks are kept. Some examples:
- `2n+1` serializes as `2n/**/+1`: a dimension followed by a number is
  one of serializer's bad pairs, so `:nth-child(2n+1)` comes back with a
  comment in it.
- The hash and An+B patterns end in `$`, which also matches before a
  final newline, so `#fff\A ` is a color and `n-1\A ` is An+B.
- In `lch()` and `oklch()` the hue is `none` when the lightness is
  `none`, not the hue.
- `@charset "UTF-16LE";` decodes as UTF-8, and `@Charset` or two spaces
  break the byte pattern.
- `max(0, x)` keeps the 0 for a NaN alpha; `min(1., x)` keeps the 1.
- Python's float `%` takes the divisor's sign, and since 3.12 `sum()`
  of floats is compensated (Neumaier); the OKLab matrices use both.

What the port does not do: CPython keeps a lone surrogate from an
escape like `\D800`, which a Kāra String cannot hold, so the port gives
U+FFFD (no suite case has one); and the hash patterns' `\d` would also
take non-ASCII decimal digits, where the port takes ASCII ones.

Compiler gap found: B-2026-10-19-33. `karac check` accepted
`v.extend(rest)` with `rest` a bare (borrowed) `Vec[J]` parameter, and
the MIR build refused it. `source.orig.kara` is that draft; the program
now declares `rest: own Vec[J]`, which is the correct program, so there
is no workaround to drop.

Licenses: tinycss2 is BSD-3-Clause (`LICENSE-tinycss2`). webencodings
is BSD (`LICENSE-webencodings`). css-parsing-tests is CC0
(`LICENSE-css-parsing-tests`).
