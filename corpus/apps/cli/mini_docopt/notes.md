# mini_docopt

docopt 0.6.2 as Python 3.13 runs it. docopt builds a command-line
parser from a program's usage message: you write the help text, and
docopt parses argv against it and returns a dict. The port covers:
- Finding the text docopt reads. That is the section that starts at the
  one `usage:` (in any case) and runs to the first blank line, and the
  option descriptions on lines that start with `-`. Defaults come from
  `[default: ...]`.
- The pattern parser. It handles `( )`, `[ ]`, `|`, `...`,
  `[options]`, `<arguments>`, UPPER-CASE arguments, commands, stacked
  short options and `--long=<arg>` forms. It raises the same
  `DocoptLanguageError` messages.
- The argv tokenizer. Long options may be given as unique prefixes, short
  options can be stacked with an argument glued on, `--` ends the
  options, and `options_first` stops at the first argument. Each user
  error gets docopt's message followed by the usage.
- The rewrite that makes an element that can appear twice count (`-vvv`
  gives 3) or collect a list.
- The backtracking matcher, with `Either` choosing the alternative that
  leaves the fewest arguments. Like docopt, it changes the parsed objects
  as it goes. So a value set while trying one alternative can still be
  there when the next is tried. The port keeps its objects in an arena so
  that it sees the same values.

The run writes `data/` and then works through the 329 jobs in
`data/jobs.jsonl`:
- 242 command lines against 25 usage messages. Seventeen come from the
  programs in docopt's `examples/` directory: Naval Fate, the git
  subcommands and the option examples. Eight are made up for this app:
  `docker run`, curl, ssh, tar, `kubectl get`, pip at the top level and
  `pip install`, and a backup tool. Each job prints the dict, the help or
  version text, or the usage error and exit status.
- docopt's git example, 16 times. It parses the top level with
  `options_first` and hands the rest to a subcommand's usage message.
- A Naval Fate session of 18 commands. Each one is parsed and then
  carried out: ships are launched, moved and shot at, and mines are set
  and removed.
- 70 small cases that each test one rule, from language errors to
  repeats inside alternatives.

`oracle.py` runs the same jobs through docopt itself and wrote
`expected.out`. `docopt.py` is the module from the 0.6.2 release,
unchanged, so the oracle does not need it installed.

docopt's quirks are kept. Some examples:
- An option is only described if its line starts with `-` after the
  indent. `Options: -a  All.` on one line describes nothing, so
  `[options]` then has no `-a` in it, and passing `-a` is a usage error.
- `[default: 1] or [default: 2]` gives the default `1] or [default: 2`,
  because the match runs to the last `]` on the line.
- `-u, --user=<user:password> Server user and password` has a single
  space before the description, so all of it is read as option names.
  The option still takes an argument.
- `--help` prints the help even when the usage message never mentions
  it, because an unknown long option still counts as given.
- A short option whose description wants an argument takes the next
  token in the usage, even `)`. The parser then reports `unmatched '('`.
- `(a <x> | b <x>)...` collects `['1', '2', '3']` for `a 1 b 2 a 3`.
  `-v -v` needs exactly two, and `-v` alone is a usage error.

The port matched `expected.out` on its first run. It needed no
workarounds and added no new ledger rows.

## License

docopt is under the MIT license in `LICENSE-MIT`:

    Copyright (c) 2012 Vladimir Keleshev, <vladimir@keleshev.com>

That covers `docopt.py` and the usage messages taken from its examples,
which `mk.py` holds unchanged.
