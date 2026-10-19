# mini_markupsafe

[MarkupSafe](https://github.com/pallets/markupsafe) 3.0.4, the string
type Jinja, Flask and Werkzeug autoescape HTML with. A `Markup` is text
already safe to put in a page; anything combined with it, by `+`, `%`,
`format`, `join` or the str methods, is escaped first, so a template
cannot forget to.

The port follows markupsafe's own code and the parts of CPython it leans
on:
- `escape`, `escape_silent`, `soft_str` and the `Markup` constructor, for
  objects with and without `__html__`;
- `+` and `*` from both sides, with Python's operator rules for a str
  subclass;
- `%` through markupsafe's escape helper and CPython's
  `PyUnicode_Format`, including the errors the helper itself causes (`%x`
  and `%c` refuse it, `*` widths fail, `%d` goes through `int()`);
- `format` and `format_map` through markupsafe's `EscapeFormatter` on
  Python's `string.Formatter`: the field parser, attribute and index
  lookups, conversions, nested specs, the `__html_format__` protocol, and
  the format-spec mini-language for str, int and float;
- every str method Markup wraps, with Python's whitespace, line breaks
  and full case mappings, and `unescape` and `striptags` with the HTML5
  entity table.

Floats are formatted exactly from their binary value: `repr`, `%f`, `%e`
and `format` give the same digits as CPython for any double.

The run writes `data/jobs.tsv`, the operations, and `data/unicode.txt`,
what Python's str methods need to know about the characters the jobs
reach. Each operation's arguments are Python expressions over literals
and four constructors: `Markup`, and the application classes `User` (with
`__html__` and `__html_format__`), `Badge` (with `__html__` only) and
`Plain` (with neither). The jobs are:
- `escape`: 38 values through the six conversion functions;
- `concat`: `+` and `*`;
- `percent`: 47 `%` operations, 16 of them errors;
- `format`: 42 `format` and `format_map` calls;
- `methods`: 59 str method calls;
- `striptags`: 23 HTML snippets through `striptags` and `unescape`;
- `thread`: a forum thread rendered the way an autoescaping template
  renders it, with author links, badges, scores and previews;
- `numbers`: prices, measurements and random doubles through `repr`, `%`
  and `format`.

The output matches `oracle.py`, which runs the same jobs through
markupsafe. The Python files are vendored unchanged in `markupsafe/`
(BSD-3-Clause, see `LICENSE-markupsafe`); the C speedups are left out,
so markupsafe uses its pure-Python `_escape_inner`. Run
`python3 mk.py && python3 oracle.py` to regenerate `expected.out`.
