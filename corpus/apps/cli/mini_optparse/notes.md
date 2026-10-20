# mini_optparse

Python 3.13's `optparse` module, ported from `Lib/optparse.py`. It is the
older of Python's two command-line parsers, still found in many tools and
scripts, and its behaviour is fixed: it gets no new features.

The port covers:
- `Option`, with its seven constructor checks run in their original order
  and their exact error messages. The messages name the option strings
  set so far, so a bad string in the middle of a list shows only the ones
  before it;
- how an option fills in what it was not given: `store` when there is no
  action, `string` (or `choice`, when there are choices) when there is no
  type, a `dest` taken from the first long option (`--pid-file` becomes
  `pid_file`) or else the first short one, and `nargs=1` for actions that
  take a value;
- `OptionParser` and `OptionGroup` with shared option tables, both
  conflict handlers (`error`, and `resolve`, which removes an option once
  it has lost all its strings), `remove_option`, `has_option`,
  `get_option_group`, `set_defaults`, and the auto-added `--version` and
  `-h/--help`;
- the parser loop: short option clusters (`-vf x.tar`, `-n0o17`), long
  option abbreviations with "no such option" and "ambiguous option"
  errors, `--opt=value`, `nargs` tuples, interspersed arguments or a stop
  at the first argument, and `--`;
- every action: `store`, `store_const`, `store_true`, `store_false`,
  `append`, `append_const`, `count`, `callback`, `help` and `version`;
- the `int`, `long`, `float` and `choice` checkers, with Python's own
  number syntax: `_parse_num`'s radix guess, `int(s, base)` and
  `float(s)` with whitespace, signs, underscores, `inf` and `nan`;
- `get_default_values`, which runs string defaults through the type
  checker at parse time;
- `IndentedHelpFormatter` and `TitledHelpFormatter` on top of the port of
  `textwrap` from mini_textwrap: `%prog` and `%default`, metavars,
  `SUPPRESS_HELP` and `SUPPRESS_USAGE`, `short_first`, the help column,
  and the width taken from `$COLUMNS`;
- `error()`, which prints the usage to stderr and exits with status 2.

`mk.py` writes `data/`: `jobs.tsv` (seven jobs that build parsers, add
options and parse command lines; the format is described at the top of
`mk.py`), and the files for two programs that run on top of `optparse`:
- a backup tool that takes its defaults from `backup.conf`, then plans a
  backup for each command line in `backup_runs.tsv`;
- `vcs`, a git-like dispatcher. It stops at the first argument that is
  not an option and hands the rest to a parser for that command.

The four callbacks the jobs use are written out in both versions: one
that splits a comma-separated value, the `vararg` and `check_order`
callbacks from the `optparse` documentation, and one that prints what
the parser has left to read. Output a parse sends to stdout or stderr is
captured and shown with `|` and `err|` in front, followed by the exit
status.

`oracle.py` runs the same jobs and programs through the real module, and
it wrote `expected.out`.

The quirks are kept. Some examples:
- `-n 017` is 15 and `-n 09` is an error, because a leading 0 means
  octal; `-n 0b` is 0;
- `--=a.tar` is read as the long option `--`, and every long option
  starts with that, so the error lists them all as possible matches;
- `-fv` stores `v` as the file name;
- a string default such as `level=s:ten` is only checked when the next
  parse starts, and the error escapes `parse_args` rather than going
  through `error()`;
- `dry_run = no` in the config file is the non-empty string `'no'`, so
  every backup becomes a dry run;
- a parser with no options at all prints no `Options:` heading;
- `-v` after the command goes to the command's parser in `vcs`, which
  does not know it.

It matched on every backend the first time it ran; no gaps were found.
