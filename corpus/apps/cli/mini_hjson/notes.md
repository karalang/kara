# mini_hjson

hjson-py 3.1.0 by Christian Zangl, the Python reader and writer for
Hjson, the human-friendly config format (comments, quoteless strings and
keys, optional commas, multiline `'''` strings, a root object without
braces). The port follows the package:
- the decoder (`hjson/decoder.py`): `getNext` with `#`, `//` and `/* */`
  comments, `scanstring` with both quote styles and every escape,
  surrogate pairs included, `mlscanstring` with its indent rules,
  `scantfnns` for quoteless strings, `true`/`false`/`null` and numbers
  (`NUMBER_RE`; with `use_decimal` a fraction or exponent gives a
  `Decimal`, which turns back into an int when it is whole and under
  1e10), `scanKeyName`, objects with and without braces, arrays, the
  BOM, repeated keys and every `HjsonDecodeError` message with its line,
  column and character position;
- the Hjson encoder (`hjson/encoderH.py`): which strings stay
  quoteless, which get quotes, which become multiline strings and which
  are escaped (its `NEEDSQUOTES`, `NEEDSESCAPE`, `NEEDSESCAPEML`,
  `STARTSWITHNUMBER`, `STARTSWITHKEYWORD` and `NEEDSESCAPENAME`), key
  names, `indent`, `sort_keys` and `ensure_ascii`;
- the JSON encoder (`hjson/encoder.py`) with `indent`, its separators
  and `ensure_ascii`, and `str(Decimal)` for numbers;
- the command-line tool (`hjson/tool.py`) with `-j`, `-c`, `-h`, `-V`
  and its two usage errors.

`STARTSWITHNUMBER`, `STARTSWITHKEYWORD` and `NUMBER_RE` run on a small
backtracking regex engine with Python's semantics; the other patterns
are character tests and are written out.

`mk.py` writes `data/`. It holds:
- config files as people write them: a service config with comments,
  nested objects, a multiline message and Windows paths, UI
  translations in four languages, CI settings, a `package.json`;
- files for each corner of the syntax: numbers, strings and escapes,
  multiline strings (also with CRLF line ends), a BOM, repeated keys,
  arrays, a comments-only file, a root string, number and quoted
  string, an empty file and a blank one;
- `broken/`, a file for each error, most of them twice: as written, and
  inside braces, since a bad root object without braces falls back to
  a quoteless string and shows no error;
- `jobs.tsv`, the jobs.

An `hjson` job runs the tool and prints what it wrote to stdout, each
line it wrote to stderr after `stderr: `, and the `SystemExit` it
raised. A `dumps` job loads a file and prints `hjson.dumps` or
`hjson.dumpsJSON` with the options named. A `roundtrip` job checks that
dumping, loading and dumping again gives the same text, for Hjson and
for JSON.

`oracle.py` runs the jobs through the package itself (`hjson/`,
unchanged) and wrote `expected.out`. `gen_kara.py` writes the data
section; files that are not plain UTF-8 are stored as hex.

The quirks are kept. Some examples:
- an unclosed `/* */` comment ends in an `IndexError` that the tool
  does not catch;
- a quoteless value runs to the end of the line, so inside braces
  `stages: [ build, test, deploy ]` is an error, and as a whole file it
  is one quoteless string;
- when a file is neither an object without braces nor a single value,
  the error shown is the one from reading it as an object;
- `1.5e2` comes back as the int `150`, and `1e10` as `1E+10`;
- `-0` and `-0.0` are both `0`;
- an invalid control character is reported at the character after it.

hjson-py is under the MIT License, or the Academic Free License 3.0 at
your choice (`LICENSE-hjson.txt`).
