# mini_svgpath

A port of [svg.path](https://pypi.org/project/svg.path/) 6.3 (MIT;
Lennart Regebro and contributors): the library that reads the `d`
attribute of an SVG `<path>` into lines, Bézier curves and elliptical
arcs, and measures them: points along the path, tangents, lengths and
bounding boxes. Plotters, laser cutters and font tools use it to turn a
drawing into moves.

```
> plot svg/icons.svg
  heart: 6 segments, length 57.45235520630508, bbox [2.992223297370734, 3.8615612366938903, 21.00777670262927, 21.0]
  home: 11 segments, length 72.0831891575846, bbox [27.0, 4.0, 45.0, 20.0]
  wrote out/icons.gcode (75 lines, total length 204.44785964256363)
  | ; icons.svg, drawn by svg.path
  | ; heart
  | G0 X12.000 Y21.000
```

## What it does

`mk.py` writes five drawings (icons, a logo, a floor plan, a file of
broken paths and an empty one) to `data/svg/` and a script of 443 steps
to `data/jobs.tsv`. `oracle.py` replays the script through the vendored
library and `mini_svgpath.kara` replays it through the port. Values
print as `repr()` shows them and an exception as
`error <class>: <message>`.

- **The paths of the SVG specification**, parsed, printed back with
  `d()` and boxed, then **measured**: points and tangents along each one
  and its length.
- **The grammar**: every command in upper and lower case, implicit
  repeats, packed numbers (`1.5.5`, `-1-2`, exponents), arc flags written
  without separators, and whitespace and commas in every place the
  grammar allows. **The tokenizer** is called directly too.
- **Broken paths**: text before the first command, a cut-off argument
  list, an arc with a negative radius, a flat arc, non-ASCII text and an
  empty path, each with the library's own error.
- **Lengths to a given error** and minimum depth, **segments one by one**
  (`point`, `tangent`, `length`, `boundingbox`, `repr`), and **points
  beyond the ends** of a segment.
- **Comparing and editing paths**: `==`, and the `MutableSequence`
  methods (`append`, `insert`, item assignment, `del`, `pop`, `remove`,
  `index`, `count`, `in`, `reverse`, slices, `clear`), with paths kept
  under a name sharing one object as Python's names do.
- **A plotter**: for each drawing it finds the `<path>` elements, and for
  each one prints its segment count, length and box, then writes G-code
  under `data/out/` (a rapid move for each `M`, up to 24 feed moves along
  each segment) and prints the file back.

## How the port works

- **A number is `Num`**: an int, a float or a complex, combined as Python
  combines them (an int or float meeting a complex is promoted first, so
  the products round as CPython's do), and printed as `repr()` prints
  each one, including complex numbers' `(3+0j)`, `-0j` and bare `5j`.
- **Formatting is exact.** `%G` and `%.3f` are made from the float's
  exact decimal digits with half-even rounding, and `repr()` of a float
  is the shortest string that reads back.
- **`sum()` is Python 3.13's**: ints first, then compensated summation
  from the first float, which is where several of the library's lengths
  come from.
- **A segment is one struct** with a kind; arcs carry the center, angle
  and sweep `_parameterize()` computes, and the radius scale only when
  the library sets it, so a flat arc fails later with the same
  `AttributeError`.
- **A path keeps the library's caches**: its length, segment lengths and
  fractions are computed on demand and, as in the library, the fractions
  are appended to rather than reset, so a point asked for after an edit
  fails the way the library's does.

## Compiler gaps

None found. `karac check` passed on the first compile; one struct was
renamed from `Args` to `ArgList` because it shadowed the prelude type
(legal, and only a warning). One transcription error in the cubic
tangent (two signs swapped) was the only difference from the oracle;
after it the port matched on every backend.

## Scope

- `float()` is exact for up to 15 significant digits times a power of
  ten up to 22, plus the infinity and zero extremes; the drawings stay
  inside that.
- A path that closes before any move prints `end=None`, as the library
  does, but no arithmetic is done on it. No NaN reaches the output.
- The SVG reader is a small attribute scanner, not an XML parser.

## Running

```bash
karac run mini_svgpath.kara                 # interpret
karac build --mir mini_svgpath.kara         # native binary
python3 -s -P -B mk.py                      # regenerate data/
python3 -I -B oracle.py > expected.out      # regenerate expected.out
python3 -I -B gen_kara.py                   # regenerate the embedded inputs
```
