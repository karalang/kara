# mini_argparse

Command-line parsing as Python 3.13's `argparse` does it. Every CLI tool
needs this: options and positionals, subcommands, help screens and
error messages. The program ports argparse's parser, its help formatter
and the parts of `textwrap` the formatter uses. It prints exactly what
argparse prints, including the help text, the errors and the exit
statuses.

The run builds two command-line interfaces and parses each command line
in `data/cases.txt` with them:
- **pkg** is a package manager with global options and six subcommands,
  some with aliases: `install` (`i`, `add`), `remove` (`rm`), `list`,
  `search`, `config` (with its own `get`, `set`, `unset` and `list`)
  and `run`.
- **backup** has two positionals, a required mutually exclusive group,
  an argument group with a description, `nargs=2` with a tuple metavar,
  an optional-value option, and a raw-text epilog.

For each command line the program prints the resulting `Namespace`, or
what was written to stdout and stderr and the exit status.

**Parsing** follows `_parse_known_args`:
- The argument strings are classified into argparse's `O`/`A`/`-`
  pattern and matched against each action's `nargs` regex. The regexes
  are reimplemented as greedy, backtracking item lists.
- Long options can be abbreviated, and an ambiguous abbreviation is an
  error.
- Short flags combine (`-vvv`, `-nl9`), and values can follow directly
  (`-j8`, `-C/srv`) or after `=` (`--jobs=2`).
- `--` ends the options. Negative numbers are values.
- Unknown options are collected and reported by the top-level parser.
- A string default is converted by the action's type when the option
  isn't given.
- Subcommand namespaces are merged into the parent's.
- Mutually exclusive and required arguments are checked, and argparse's
  error messages are reproduced.

**Help** follows `HelpFormatter` at an 80-column terminal:
- Usage lines are wrapped the way argparse wraps them, with mutually
  exclusive groups bracketed.
- Help columns are placed at the 24-column limit.
- Help text is wrapped by a port of `textwrap`, including its splitting
  after the hyphens of compound words.
- Help strings expand `%(default)s`, `%(metavar)s`, `%(const)s` and
  `%(prog)s`.
- Subcommands are listed with their aliases.

Not supported: `parents`, `prefix_chars` other than `-`,
`fromfile_prefix_chars`, `parse_intermixed_args`, `allow_abbrev=False`,
conflict handlers, deprecated arguments, `FileType`, and formatters
other than the default and `RawDescriptionHelpFormatter`.

**Oracle.** `expected.out` is the output of `oracle.py`, which builds
the same two CLIs with argparse (in `parsers.py`) and runs every line of
`data/cases.txt` with `COLUMNS=80`. Run it from this directory after
`mk.py` writes `data/`.

## Edits from mini_argparse.orig.kara to mini_argparse.kara

One edit is for a new compiler gap:
- The mutually exclusive groups were held in `struct Mutex`. That struct
  gets the library `Mutex`'s drop on both MIR backends, so its `Vec` is
  never freed: `__mir-run` reported 126 leaked allocations and exited 3,
  and the compiled program emitted no drop call
  (B-2026-10-15-48). The struct is now `ExclusiveGroup`.

Two edits are for existing rows:
- `parsed.ns.keys[i]` and `st.ns.origin[at]` are chained field receivers,
  which check refuses (B-2026-10-14-70). They are now the `Namespace`
  methods `key`, `val`, `len` and `origin_of`.
- `if let Value.Str(s) = v { .. return .. }` followed by `Ok(v)` is
  called a use after move (B-2026-10-15-32). The match is now on
  `v.clone()`.

The rest were author errors: the `Option[String]` parameters that are
stored needed `own`.

After that, `karac check` passed. The output matched expected.out on
the first run on all three MIR surfaces; only the leak report above
needed the rename.

Written 2026-10-15 by the real-world apps thread; karac at 5c299f966b.
