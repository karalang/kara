# mini_tomlkit

TOML editing as tomlkit 0.13.2 does it: a document parses into a tree
that keeps every comment, blank line, indent and original spelling, so
a program can change a value and write the file back with nothing else
moved. tomlkit is what Poetry and many release tools use to edit
`pyproject.toml` and `Cargo.toml` files that people also edit by hand.
The port covers:
- the parser, with its error messages and their line and column;
- the document model: `Container` and `TOMLDocument`, `Table`,
  `InlineTable`, `AoT`, `Array`, `String`, `Integer`, `Float`, `Bool`,
  `DateTime`, `Date`, `Time`, `Whitespace`, `Comment` and `Null`, with
  super tables, dotted keys, and out-of-order tables, which a lookup
  returns as an `OutOfOrderTableProxy`;
- editing in place: setting, replacing (including a value replaced by a
  table, which moves after the plain values), deleting, `append`, `add`,
  `insert`, `setdefault`, `pop` and `update`, `comment()` and
  `indent()`, and an array's `add_line` and `multiline`;
- building: `document`, `table`, `aot`, `inline_table`, `item()`,
  `string()` with its `literal`, `multiline` and `escape` flags, the
  other constructors, `value()`, `key_value()`, `key()`, `dumps()` with
  and without `sort_keys`, and `TOMLFile`, which keeps a file's line
  endings;
- two programs:
  - a release script. `data/repo/release.toml` names a version, a date,
    the crates being released and five manifests. The script sets the
    version in each one (a Cargo workspace, two Cargo crates, a Poetry
    and a PEP 621 `pyproject.toml`), moves the version requirement of
    every path dependency on a released crate to match, writes the
    results to `data/out/repo/`, and adds the release at the top of
    `CHANGELOG.toml`'s `[[releases]]`;
  - a config migration. It copies the service configs in `data/etc/`
    to `data/out/etc/` and moves each one to schema 2 in place: the log
    settings go into a `[logging]` table, the server's `*_timeout`
    strings become `*_timeout_ms` integers, and the `[[upstreams]]`
    array becomes a table of tables named after each upstream. One
    config has CRLF line endings and keeps them; one is already at
    schema 2.

The run writes `data/`, works through the 114 jobs in
`data/jobs.jsonl`, and then runs the two programs. `mk.py` wrote
`data/` (the documents are in `mk_docs.py`).

`oracle.py` runs the same jobs through tomlkit itself and wrote
`expected.out`. It uses the `tomlkit/` package from the 0.13.2 wheel,
unchanged. The message of an `InvalidStringError` is cut before the
set of bad sequences it names, which Python prints in hash order.

The quirks are kept. Some examples:
- The time zone pattern is `[\+|\-]`, so `08:00:00|05:30` reads as
  `+05:30`; and `+infinity` is a float, since only the first four
  characters are checked.
- A date followed by spaces keeps them as the space before its
  comment, so in an array (`[ 2024-01-01 , 2024-06-30 ]`) they are
  lost when the array is written back.
- An error while adding a key at the top level is reported as a
  `ParseError` with a line and column; the same error inside a table
  comes out bare, as `KeyAlreadyPresent` or `TOMLKitError`.
- A missing key's message is shown with quotes around it, since
  `NonExistentKey` is a `KeyError`.
- A `\u` escape is read with `int(x, 16)`, so a sign or an underscore
  among the four digits is accepted.
- `item()` of a list of dicts puts the nested dicts after the other
  keys even when the keys are not sorted.
- `date()` accepts a datetime string and returns a `DateTime`, since a
  datetime is a date.

Not ported: `register_encoder`, custom encoders for other Python types,
pickling and copying of items, integers past 64 bits (`int()` in the
port stops at `i64`), and the item arithmetic (`Integer + 1` returning
an `Integer`).

## License

tomlkit is under the MIT License, in `LICENSE-tomlkit.txt`:

    Copyright (c) 2018 Sébastien Eustace

That covers `tomlkit/`.
