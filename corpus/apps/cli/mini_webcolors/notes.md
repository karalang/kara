# mini_webcolors

[webcolors](https://github.com/ubernostrum/webcolors) 24.11.1, the
library for the color names and value formats HTML and CSS define, and a
stylesheet audit built on it.

The port follows webcolors' own code:
- the named colors of HTML 4, CSS2, CSS2.1 and CSS3, including which name
  wins when two share a value (`cyan` over `aqua`, `gray` over `grey`);
- every conversion between names, hex values, integer `rgb()` triplets and
  percentage `rgb()` triplets, with the errors each raises;
- the normalization functions, with Python's `int()`, `float()`,
  `round()` and float `repr` under them, so `"1_0%"`, `" 7 %"`, `"1.e2%"`
  and `"-0.0%"` come out as they do in Python;
- the HTML5 simple color parser and serializer, and the legacy color
  algorithm browsers apply to `bgcolor="chucknorris"`.

The audit reads `data/site.css`, finds the colors in each rule (hex,
`rgb()` with numbers or percentages, names, and `var()` custom
properties), describes each one in hex, `rgb()`, percentages and HSL (as
`colorsys` computes it), checks every text and background pair against
the WCAG contrast ratio, and lists the palette with the nearest CSS3 name
for colors that have none.

The run writes `data/jobs.tsv`, the operations, and `data/site.css`. The
jobs are:
- `names`: the name lists and name lookups for each specification;
- `hex`: 22 hex values through normalization, naming and conversion;
- `rgb`: integer triplets, clipped, named and serialized;
- `percent`: 24 percentage triplets through normalization and
  conversion, and 40 random ones;
- `roundtrip`: every integer channel and every named color through the
  percentage form and back;
- `html5`: the simple parser and 106 values through the legacy algorithm;
- `audit`: the stylesheet.

The output matches `oracle.py`, which runs the same jobs through
webcolors. The Python files are vendored unchanged in `webcolors/`
(BSD-3-Clause, see `LICENSE-webcolors`). Run
`python3 mk.py && python3 oracle.py` to regenerate `expected.out`.
