# mini_click

A deploy client and three smaller tools built on a port of click 8.5.0
(BSD), the command-line framework:
- the option parser (`parser.py`): short-option clusters (`-vvv`, `-fn 5`,
  `-r3`), `--opt=value`, `--`, interspersed arguments, `nargs` > 1 and
  `nargs=-1` positionals, token normalization, and its usage errors with
  "Did you mean" suggestions from `difflib.get_close_matches`;
- `Command` and `Group` (`core.py`): contexts and their inheritance
  (`obj`, `default_map`, auto env var prefixes, help option names),
  eager parameters, the processing order, value sources (command line,
  environment, default map, default, prompt) and the rule that decides
  which of several options sharing one name wins, `invoke_without_command`,
  chained groups with a result callback, hidden and deprecated commands
  and options;
- the parameter types (`types.py`): `Choice` (case-insensitive), `IntRange`
  and `FloatRange` with open bounds and clamping, `DateTime` (a `strptime`
  for its three formats), `UUID`, `File`, `Path` checks, `Tuple` and
  booleans;
- prompts and confirmations (`termui.py`) as `CliRunner` answers them,
  with stdout and stderr interleaved;
- the help formatter (`formatting.py`): usage lines, wrapped help text
  with `\b` paragraphs, definition lists, short help for command lists,
  and the `[env var: ...; default: ...; range; required]` extras.

`deployctl` is a group with nested groups, a config file read into the
default map by an eager callback, environment variables, prompts and path
checks; `pipeline` is a chained group; `greet` is one command with flag
values, lower-cased tokens and `show_default`; `wrap` exercises the
formatter.

`mk.py` writes `data/`: the config file, a manifest, release notes, a
services directory and `runs.txt`, 164 command lines with their
environment and input. `oracle.py` runs them through click's own
`CliRunner` and wrote `expected.out` (`python3 -s -P oracle.py`), and
`gen_kara.py` prints the data section at the end of `mini_click.kara`.
The text wrapper comes from `mini_textwrap` and the matcher from
`mini_difflib`.
