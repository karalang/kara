# mini_nestedtext

A NestedText loader. NestedText is an indentation-based format for settings
and data files whose leaf values are only strings, so nothing in a value is
ever quoted or escaped: a list item is `- text`, a dictionary item
`key: text`, a multi-line string a run of `> text` lines, a key that needs
a colon or several lines a run of `: text` lines, and a short list or
dictionary may be written on one line as `[a, b]` or `{k: v}`. The program
loads a small web shop's settings files (a deploy file, an on-call contact
list, a CI matrix, release notes, a settings file whose keys are
normalized, a host inventory with repeated keys and a file with a mistake
in it), says where some keys and values came from, then runs single cases
through every line kind, the inline forms and every error.

The program writes its own data (`data/files/*.nt`, `data/loads.jsonl` and
`data/cases.jsonl`), runs it and prints a report. `oracle.py` runs the same
data through `nestedtext/nestedtext.py` from nestedtext 3.8 (`vendor/`,
MIT), which formats its errors with inform 1.37 (`vendor/inform/`, MIT),
and is the reference; `mk.py` writes the data and `gen_kara.py` embeds it
in the program. inform imports `arrow` only to time-stamp a log, which the
loader never does, so `vendor/arrow.py` is a stand-in that makes the import
succeed.

## Loading (nestedtext's `loads`)

- The text is split into lines at `\n`, `\r\n` and `\r`, as
  `io.StringIO(text, newline=None)` reads it; a final empty line is not a
  line. No other character ends a line.
- A line's depth is its count of leading spaces. A line that is empty after
  them is blank, one starting `#` a comment; both are skipped everywhere.
  `-` or `- ` starts a list item, `>` or `> ` a string item, `:` or `: ` a
  key item (the value is what follows the two-character tag), `[` or `{`
  an inline list or dictionary (unless the dialect has `i`). Otherwise the
  line is a dictionary item when it fully matches
  `(?P<key>[^\s].*?)\s*:(?:\ (?P<value>.*))?`: the shortest key that starts
  with a character that is not white space and is followed by optional
  white space and a colon that ends the line or is followed by one space
  (the value, possibly empty, is the rest). Anything else is unrecognized.
  White space here is Python's (`str.isspace`).
- A value is a list, a dictionary, a string or an inline value according to
  its first line. An item with no value takes the more deeply indented
  lines after it as its value, or `""` when there are none; a multi-line
  key must have such lines. A multi-line string or key is its lines joined
  with `\n`.
- `top` is `dict`, `list`, `str` or `any`: the document must start with
  that kind of line, and an empty document is `{}`, `[]`, `""` or `None`.
- Inline values are a recursive-descent parse of the line, with the
  module's quirks kept: an item is the text up to the next `{}[],` (and
  `:` for a dictionary key or value) with surrounding white space removed;
  `[ ]` is `[""]` and `{ }` an error; a list item after a false value
  (`[]`, `{}` or `""`) replaces it, so `[[] x]` is `["x"]`; a value inside
  an inline dictionary is located under the dictionary's keys, not the
  item's.
- Duplicate keys (after normalization): `error` (the default) reports
  them, `ignore` keeps the first and skips the line (so an indented value
  under it is an indentation error), `replace` keeps the first one's place
  with the last one's value, and `dedup` is the callback from the `loads()`
  documentation, which renames the second `key` to `key — #2`, the third to
  `key — #3`, counting across the whole document. `normalize` lower-cases a
  key and turns its spaces into underscores.

## Errors (nestedtext's `report`, inform's `Error`)

- An error is `<culprit>: <message>` and an indented codicil. The culprit
  is the 1-based line number, after the source name when there is one.
  The message ends with a full stop (inform's `full_stop`).
- The codicil shows the line as `{lineno:>4} ❬text❭`. When the error has a
  column it also shows the meaningful line before it (unless the error is
  in an inline value) and a `▲` under the column, with tabs in the line
  shown as `→`. An error without a column shows only the line. Codicil
  lines are indented four spaces and have trailing white space removed.
- The messages are the module's, including the wrapped explanation of an
  invalid indent (`textwrap.fill`, 70 columns), the character and Unicode
  name of white space found in an indent, and a duplicate key inside an
  inline dictionary pointing at the item's index in the text, not moved by
  the line's indentation.

## The keymap (`Location`)

- Each key path, and the empty path for the top, maps to where its key and
  value are: a line and a 0-based column of each. A location reports
  `as_tuple(kind)` (0-based line and column), `get_line_numbers(kind,
  sep="-")` (1-based, first-last for a multi-line key or string) and
  `as_line(kind)` (the line and a pointer, no further right than its end).
  The column of a dictionary item's value counts the normalized (or
  renamed) key's length.

## The report

- `== files`, then for each line of `loads.jsonl`, `load <file> as <top>`
  and its options; then the value as `json.dumps(…, ensure_ascii=False,
  indent=2)` and each query as `where <keys> <kind>:` with its location,
  or the error with `  error: ` before each line.
- `== cases`, then for each case `<name>: <json>` (compact), or
  `<name>: error` and the error's lines indented two spaces, then
  `lineno <n>, colno <n or None>`.
