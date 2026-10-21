# mini_configobj

configobj 5.0.9 by Michael Foord, Nicola Larosa, Eli Courtwright and
Rob Dennis, with its `validate` module: the INI-style config reader
Python services, deploy tools and test runners use for settings files
with nested sections, comments, lists, multi-line values and
interpolation, checked against a configspec. The port covers:
- parsing: section nesting by bracket depth (and dropping back out of
  it), quoted and unquoted values, comma lists, triple-quoted
  multi-line values, initial, per-key, inline and final comments, the
  newline style of the file, and the errors a bad file raises, at once
  with `raise_errors` or collected into "Parsing failed with several
  errors";
- the options `interpolation` (`ConfigParser`'s `%(name)s` and
  `string.Template`'s `$name`, with `DEFAULT` sections, loops and
  missing options), `list_values`, `stringify`, `encoding`,
  `indent_type` and `write_empty_values`;
- lookups, `keys`, `items` and `dict`, and editing: setting scalars,
  lists and whole sections, `rename`, `del`, `pop`, `setdefault`,
  `popitem`, `merge` and `walk`;
- `write`, whole or by section, quoting each value the way `_quote`
  does (and refusing the ones it cannot quote);
- `as_int`, `as_bool`, `as_float` and `as_list`, with Python's `int()`
  and `float()` (underscores, Unicode digits, `inf` and `nan`);
- validation: every built-in check of `validate.py`, its argument
  parsing (`list(...)` arguments, quoted `None`, keyword arguments) and
  the way Python binds those arguments to the check function, defaults,
  `__many__` sections, `copy`, `preserve_errors`, `flatten_errors`,
  `get_extra_values` and `restore_defaults`. `ip_addr` follows glibc's
  `inet_aton`, which is what Python calls.

`mk.py` writes `data/`:
- `etc/`: a web shop's settings, an inventory service's settings and
  its configspec (with good, bad and half-filled copies), files for
  interpolation, quoting, CRLF line ends, Unicode, deep nesting and
  broken syntax, three layers of settings to merge, and the spec files
  the validation jobs load;
- `jobs.tsv`: 368 jobs in 10 groups. Each loads a config with some
  options, prints or changes part of it, writes it out, validates it,
  or runs a check string straight through `Validator.check`.

Each job prints `-- ` and its columns. A Python exception prints `! `,
its class and its message. Values are printed with `repr`, a written
file with its carriage returns shown as `<CR>` and its end as `<EOF>`.

`oracle.py` runs the jobs through configobj and validate themselves
(the copies in `configobj/`, unchanged) and wrote `expected.out`.
`gen_kara.py` writes the generated section at the end of
`mini_configobj.kara`: the text and flags of every regex the two
modules compile, and `data/`.

configobj and validate find everything with `re`, so this carries the
regex engine of `mini_re` (Python 3.13's parser and an sre-style
backtracking matcher) and compiles their own patterns with it. Sections
live in one arena per config, so a section two keys reach is the same
section to both, as in Python.

The quirks are kept. Some examples:
- a value set over an existing section stays in the section list, so it
  is written where the section was;
- `rename` reads the old value through interpolation, so the new key
  holds the interpolated text;
- `walk` with `raise_errors=True` stops at the first bad value and
  keeps the changes it had made;
- `validate` with `stringify=False` keeps the strings, but writes the
  missing defaults as strings (`None` becomes `''`);
- a `VdtParamError` (a `SyntaxError`) or a `TypeError` from binding the
  check's arguments is not caught by `validate`, so it ends the call;
- `string(default='it''s')` gives `it''s`, because the regexes stop at
  the outer quotes.

configobj and validate are under the BSD license in
`LICENSE-configobj.txt`.
