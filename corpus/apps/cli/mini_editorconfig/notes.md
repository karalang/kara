# mini_editorconfig

The EditorConfig core as the Python package `editorconfig` 0.17.1 runs
it on Python 3.13. Editor plugins run its `editorconfig` command to learn
how a file should be indented and saved. The port covers:
- The INI parser. Section names may contain `]`, and `#` and `;` when
  escaped. An option is split at its first `:` or `=`. A ` ;` or ` #`
  starts a comment, `""` is an empty value, and option names are folded to
  lower case. It also strips a UTF-8 BOM, splits lines the way
  `str.splitlines` does, and collects every bad line into one
  `ParsingError`.
- The walk from the file's directory up to `/`. It reads each
  `.editorconfig` (or the name given with `-f`) and stops at the first one
  that sets `root = true`. A setting in a nearer file wins, but the key
  keeps the place it had in the farther file.
- The post-processing. Some values are lowered, `indent_size` becomes
  `tab` when `indent_style = tab` (from version 0.10.0 on, which `-b`
  can set), and `tab_width` and `indent_size` fill each other in.
- The command line, with Python's `getopt` and its messages, and the exit
  status of each error.
- `editorconfig.fnmatch`, which turns a section name into a Python regex.
  It handles `*`, `**`, `?`, `[...]`, `{a,b}` and numeric ranges like
  `{1..20}`. A number with a leading zero never matches a range.
- A small `re`. It compiles that regex the way CPython's parser does, with
  the same error messages and positions, and runs it with backtracking in
  the order `sre` tries things. That order matters for which group catches
  a number.

The run writes `data/` and then works through the 213 jobs in
`data/jobs.jsonl`. `data/root` stands in for `/`, and it holds the
configs of a web app, a monorepo, a Linux-style C tree, a vendored library,
and some broken or awkward files:
- 123 `editorconfig` command lines. They look up files under that tree,
  including `..`, doubled slashes and relative paths. They also try
  `-f`, `-b` with good and bad versions, `-h`, `-v`, unknown and
  incomplete options, and `--`.
- 50 glob patterns chosen for their edge cases, and 40 random ones. Each
  prints its regex, its numeric ranges, and which names match.

`oracle.py` runs the same jobs through the real package and wrote
`expected.out`. For an `editorconfig` job it calls the package's own
`main()` with the real path of `data/root` in front of each absolute path,
and takes that prefix back out of what it prints.

The library's quirks are kept. Some examples:
- `**` translates to `.*[^/]*`. The second `*` is translated again.
- `[*]` is not a set holding `*`. The `*` becomes `[^/]*` inside the
  brackets, so the regex is `[[^/]*]`, which matches `]` and not `*`.
- `{a,{1..3}}` against `a` leaves the number group empty, and the range
  check then fails on `None[0]`. That is an uncaught `TypeError`, and the
  command dies with it.
- A section named `[z-a].txt` reaches `re.compile` as a bad character
  range and escapes as an uncaught `PatternError`. `x[` fails with
  `bad escape \Z`, because the `\Z` that ends every regex lands inside the
  open set.
- A set with a `/` in it is no set. `[a/b]c` matches only the literal
  name `[a/b]c`.
- `-b 0.17.1.dev` is a valid version and `-b 0.17.0-beta` is not. A bad
  `-b` value goes to stderr with status 1, through `sys.exit()`. Parsing,
  path and version errors go to stdout with status 2.
- An argument after the first file name is a file name too, so
  `editorconfig /web/README.md -v` fails with `Input file must be a full
  path name.` after printing the first file.
- A `.editorconfig` that is a directory is skipped, the same way as one
  that does not exist.

The port matched `expected.out` on its first run, and it passes on all
four MIR backends. It needed no workarounds and added no new ledger rows.

## License

The parts of the port that follow `editorconfig`'s `handler.py`,
`__main__.py` and `versiontools.py` are under the BSD license in
`LICENSE.BSD`:

    Copyright (c) 2011-2018 EditorConfig Team, including Hong Xu and Trey Hunner

The parts that follow its `ini.py` and `fnmatch.py`, which are based on
Python's own `ConfigParser` and `fnmatch`, and the regex parser, which
follows CPython's `re/_parser.py`, are under the PSF license in
`LICENSE.PSF`:

    Copyright (c) 2001, 2002, 2003, 2004, 2005, 2006, 2007, 2008, 2009, 2010
    Python Software Foundation; All Rights Reserved

As the PSF license asks, here is a summary of the changes: the code is
rewritten in Kāra. The parser is reduced to what `editorconfig` uses.
`re` is reduced to the syntax `fnmatch.translate` can produce, plus its
error checks, and it runs on a backtracking matcher rather than `sre`.
