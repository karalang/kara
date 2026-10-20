# mini_json5

json5 0.17.3, the pure-Python JSON5 library behind `python -m json5`.
JSON5 is the JSON dialect people write by hand: Renovate and pnpm
configs, browser-engine feature lists, Babel configs. It allows
comments, unquoted keys, single quotes, trailing commas, hex numbers
and `Infinity`. The port follows:
- the parser: json5's own PEG parser, which glop generates from
  `json5.g`, rule for rule. So a broken file fails at the same line,
  column and character. Then the walk that turns the parse tree into
  values, using Python's `int()` and `float()` on the number text, and
  the duplicate-key check;
- the encoder, `JSON5Encoder`: indent as a number or a string,
  separators, `sort_keys`, `quote_keys`, `trailing_commas`, the four
  quote styles, `ensure_ascii`, `allow_nan`, and its rule for which keys
  can go unquoted (identifiers that are not reserved words);
- `python -m json5` and its options;
- Python's repr of the values, and the UTF-8 decoding errors.

`mk.py` writes `data/`. It holds:
- config files as projects keep them: a Renovate config, a pnpm
  `package.json5`, a list of runtime feature flags, a Babel config and a
  device inventory with hex serials, integers past 64 bits and
  `Infinity`;
- files that use every corner of the grammar: every escape, identifier
  keys from Unicode letters and `\u` escapes, every number form, every
  kind of whitespace, raw newlines in a string (allowed only with
  `strict=False`), repeated keys, and streams of several values;
- 37 broken files a config reader meets in practice;
- `cases.tsv`, one-line documents, most of them from json5's own tests;
- `jobs.tsv`, the jobs.

Each job prints `== ` and its fields joined by ` | `, then its output:
- `load F [opts]` prints the repr of what `json5.loads` returns, or its
  error. The options are `strict=0`, `trailing=0` (no
  `consume_trailing`) and `dups=0` (no `allow_duplicate_keys`);
- `parse F` reads several values one after another with `json5.parse`
  and `start`, as its docstring suggests, printing where each one ends;
- `cases` loads each line of `cases.tsv`;
- `dump F [opts]` prints `json5.dumps` line by line. The options are
  `indent=N`, `indent=tab`, `indent=s:TEXT`, `compact=1`, `sort_keys=1`,
  `quote_keys=1`, `trailing_commas=0`, `quote_style=...`,
  `ensure_ascii=0` and `allow_nan=0`;
- `roundtrip F` dumps compactly and pretty, loads each dump back, dumps
  it again, and says whether the text came back the same;
- `tool ARGS` runs `python -m json5` and prints its stdout, stderr and
  exit status (or the exception it stops with).

An error message that quotes a raw newline or carriage return is printed
with those two escaped, so it stays on one line. A lone surrogate prints
as `\udXXX`, the way Python's `backslashreplace` writes it.

`oracle.py` runs the jobs through json5 itself (the copy in `json5/`,
unchanged) and wrote `expected.out`. `gen_kara.py` writes the data
section and a table of character classes from Python's `unicodedata`.

The quirks are kept. Some examples:
- An escaped surrogate pair stays two lone surrogates (json5 turns each
  `\uXXXX` into one character), and dumps writes them back the same way.
- `strict` refuses only a raw CR or LF in a string; a bell or an escape
  character passes.
- A number's text is checked only by Python's `float()`, so `1e`, `.`,
  `-.` and `.e5` parse and then fail with `could not convert string to
  float`. And `-0` is the integer `0`.
- An identifier key can be written with `\u` escapes, and the escape
  can give any character, so `{1: 1}` has the key `"1"` and
  `{a b: 1}` has `"a b"`.
- Failed attempts still move the reported error position. Trying a key
  as an identifier reads one character past a digit, so `{1: 1}` fails
  at the `:`, not at the `1`.
- `private`, `this`, `while` and the other reserved words are quoted as
  keys, but `Infinity` is not.
- `indent=0` (or a negative indent) puts every item on its own line
  with no indent, and an empty indent string does the same.

Not ported: `object_hook`, `object_pairs_hook`, `parse_float`,
`parse_int` and `parse_constant`, `skipkeys`, `default`,
`check_circular` and `allow_duplicate_keys` in `dumps`, and keys that
are not strings. A loaded value has only string keys and no cycles, so
none of these change anything here. The tool reads from `-c` or a file,
not stdin, and handles only the options it documents.

License: json5 is Apache-2.0 (`LICENSE-json5.txt`).
