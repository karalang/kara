# mini_dotenv

python-dotenv 1.2.3, the `.env` loader behind Django settings, Flask's
`flask run`, FastAPI's pydantic-settings and most twelve-factor services,
with its `dotenv` command line on click 8.5.0. These parts of
`dotenv/parser.py`, `variables.py`, `main.py` and `cli.py` are ported:
- the parser: `parse_stream` and its `Reader`, with `export` prefixes,
  single and double quotes, escapes, multi-line quoted values, inline
  comments, keys without a value and the error binding for a line that
  does not parse;
- `parse_variables` and the `${NAME}` / `${NAME:-default}` atoms;
- `DotEnv.dict()` with `interpolate` and `override`, `dotenv_values` and
  `load_dotenv`, including the `PYTHON_DOTENV_DISABLED` switch;
- `get_key`, `set_key` (all three quote modes, `export`) and `unset_key`,
  which rewrite the file through a temporary file moved over it;
- the `dotenv list` (simple, json, shell and export formats), `get`, `set`
  and `unset` commands, with click's usage errors for bad choices, bad
  booleans, unknown options, unknown commands and missing arguments.

The library's quirks are kept. Some examples:
- `${A:-${B}}` is not nested: the default is the literal `${B`, and the
  `}` that follows is a literal too.
- `:-` only applies when a name is missing. A variable set to the empty
  string resolves to the empty string.
- `set_key` replaces every binding of the key. In `tricky.env`, `export=3`
  and a bare `export` both parse as the key `export`, so setting it writes
  `export=4` twice.
- A value that runs to the end of a CRLF line keeps the `\r` in some
  forms. Files are read in text mode, so a rewrite normalizes CRLF to LF.
- A `.env` value wins over the environment in `dotenv_values` but loses
  to it in `load_dotenv` unless `override` is set.

Not ported: `find_dotenv` (it walks up from the caller's directory),
`dotenv run` (it execs a program) and keeping a file's mode on rewrite.
The process environment is simulated: each job carries its own `environ`,
which `load` jobs print the changes to.

The run writes `.env` files under `data/env/` (LF, CRLF, CR-only, a BOM
and one without a final newline among them) and `data/jobs.jsonl`. It then
runs the 92 jobs, each printed after `== ` and the job:
- `parse`, `parse_text` and `vars` jobs print the bindings or atoms the
  parser returns;
- `values`, `values_override`, `load` and `merge` jobs print the
  resulting dict or environment, with the warnings python-dotenv logs;
- `edit` jobs copy a file to `data/work/` and run `set`, `unset` and `get`
  operations on it, printing each result and the final file;
- `cli` jobs run the command line and print its output and exit status.

`oracle.py` runs the same jobs through the real library
(`pip install python-dotenv==1.2.3 click==8.5.0`, CPython 3.13, using
click's `CliRunner`) and wrote `expected.out`.

Author errors:
- a parameter named `override`, which is a reserved word in Kāra;
- a parameter named `text` that shadowed the `text()` helper;
- moving bindings out of a `for b in parse_stream(...)` loop, which
  borrows the vector; `.into_iter()` moves them.

Compiler gaps:
- `String` has no `pop()` or `truncate()` (B-2026-10-19-2, Thread A).
  Worked around by slicing off the last byte.

Written 2026-10-19 by the real-world apps thread; karac at b6761f33ac.
