# mini_json

Python 3.13's `json` module, `loads` and `dumps`, ported from
`Lib/json/` and the C scanner and encoder in `Modules/_json.c`. It is
what reads a `package.json`, a Kubernetes manifest, a GeoJSON file or an
API response, writes them back with a chosen indent, and decodes a JSON
Lines log one record at a time.

The run writes 46 files into `data/`, then runs `data/jobs.tsv`, 71 jobs
with tab-separated fields. Each job prints `== ` and its fields joined
by ` | `, then its output:
- `load F` prints the `repr` of what `json.loads` returns;
- `dump F [opts]` prints `json.dumps(value, **opts)` line by line, after
  encoding it to UTF-8 as writing it to a file would;
- `jsonl F [opts]` loads each non-blank line and prints its line number
  and its dump, or the error;
- `roundtrip F` dumps the value compactly and pretty (indent 3,
  `sort_keys`, `ensure_ascii=False`), loads each dump and dumps it again,
  and prints the lengths and whether the text came back the same.

The options are `indent=N`, `indent=t` (a tab), `sort_keys=1`,
`ensure_ascii=0`, `compact=1` (separators `,` and `:`) and `allow_nan=0`.

The documents are a `package.json`, a GeoJSON collection (points, a line,
a polygon, a null geometry, `-0.0`, `1e-07`), a Kubernetes deployment, two
GitHub-style issues (an emoji, `\r\n` in a body, a zero-width space, a
no-break space, a line separator, a BOM inside a string, a bell, DEL) and
a service config with a repeated key, integers past 64 bits, `1e400`,
`NaN` and `Infinity`, escapes of every kind, astral characters written
raw and as surrogate pairs, lone surrogates and non-Latin keys. The JSON
Lines file is an access log with blank lines and two bad records. The
broken files cover each `JSONDecodeError` message (trailing commas,
missing delimiters, single quotes, control characters, bad escapes, bad
`\u` escapes and surrogate pairs, leading zeros, `+1`, hex, a comment,
`nan`, a BOM, extra data, an error several lines in and one after
non-ASCII text) and the 4300-digit limit on integers.

**The port** follows the C scanner over code points, so error positions
count characters as Python's do, with the line and column computed the
way `JSONDecodeError` does. Strings are vectors of code points, so a lone
surrogate from a `\ud800` escape loads, dumps back as `\ud800`, and makes
the UTF-8 encode of an `ensure_ascii=False` dump fail with Python's
`UnicodeEncodeError`. Integers keep their digits, so a 30-digit id goes
through unchanged; a number with a fraction or exponent is a float, and
`f"{x}"` gives the same text as Python's `repr`. Objects keep their keys
in order and a repeated key keeps its first place and takes its last
value, as `dict()` does. The encoder follows `_make_iterencode`: the
separators, indent levels, empty containers, `sort_keys`, and
`ensure_ascii` with surrogate pairs and lowercase hex. `repr` follows
Python's for `str`, including which characters `str.isprintable` rejects;
that table lists controls, format characters, separators, surrogates and
private use, but not unassigned code points.

Not supported: `object_hook`, `parse_float` and the other hooks,
`skipkeys`, `default`, `strict=False`, `check_circular`, and keys that
are not strings.

**Oracle.** `expected.out` is Python 3.13.16's output for the same jobs,
made by `oracle.py` with the real `json`. Run `mk.py` first; it writes
`data/`. `gen_kara.py` prints the Kāra data section (everything after the
last comment line ending in `data` in `mini_json.kara`) from the same files,
text files as strings and the two that are not plain text as hex.
