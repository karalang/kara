# mini_parse

parse 1.22.3 as Python 3.13 runs it. parse is the reverse of
`str.format()`: it reads values back out of a string with a template
such as `{name} is {age:d}`. behave and pytest-bdd match their steps
with it, and log scrapers and config readers use it in place of
hand-written regular expressions. The template becomes a regular
expression, so the regex engine of `mini_re` (Python's `re`, ported from
`_parser.py` and sre) is part of this app. The port covers:
- The template parser. It splits fields from text, escapes the text,
  handles `{{` and `}}`, and tells named fields from positional ones.
  Names with dots, hyphens and `[key]` parts are mangled into group
  names the way the library does it, including its collision rules and
  its `KeyError`. A repeated name becomes a backreference, and
  `RepeatedNameError` is raised when its format differs.
- The format spec: fill, alignment, sign, zero, width, grouping and
  precision.
- Every built-in type:
  - integers in bases 2, 8, 10 and 16, with base detection and the `n`
    type's separators;
  - floats, `Decimal`, `e` and `g` forms, and percentages;
  - letters, and the `\w` `\W` `\s` `\S` `\D` classes that `w`, `s` and
    the others turn into;
  - the date and time types `ti`, `tg`, `ta`, `te`, `th`, `tc` and `tt`,
    with `FixedTzOffset`;
  - strftime directives. These are read through a port of Python's
    `_strptime`: its `TimeRE` patterns, its error messages, `%j`, `%U`
    and `%W`, and `%z` with `datetime.timezone`.
- Custom types with a pattern and a group count, as `with_pattern`
  attaches them.
- `Result`, with its nested names and its spans. Also `parse`, `search`
  with `pos` and `endpos`, and `findall`.

The run writes `data/` and then works through the 946 jobs in
`data/jobs.jsonl`:
- 588 parses, searches and findalls of single strings. Each job prints
  the `Result`, its spans, or the exception.
- 48 templates shown as the expression they become, with
  `repr(Parser)` and the field lists.
- 300 round trips. Python's `format()` writes a random value with a
  random spec, and parse reads it back. Random records are also parsed
  against random named templates.
- A BDD run over two feature files, and a dry run. The runner matches
  each step to the first step definition whose parse pattern fits, as
  behave does. A custom `Money` type reads prices. Scenario Outlines are
  expanded from their examples.
- Two reports over an access log in the combined format. They count
  statuses, methods, paths and days, and find the first and last request
  by UTC time.
- A report over an application log, which finds the slow requests with
  a second template.
- Five scans with `findall`: links in an HTML page, hashtags, quantities
  in a recipe, the keys of an INI file and the TODOs in a source file.

`oracle.py` runs the same jobs through the package in `parse/`, which is
the 1.22.3 wheel unchanged, and wrote `expected.out`.

parse's quirks are kept. Some examples:
- `{:s}` matches whitespace, not a string. `{:w}`, `{:S}` and `{:D}`
  are regex classes too.
- `{:d}` with a width becomes `[0-9]{1,width}`, so `{:3d}` refuses
  `1234`. A sign of space before `0x1f` hides the base, so `' 0x1f'`
  reads as 1.
- The `n` type's `\d` matches Arabic-Indic digits, but `int_convert`
  then strips them, so `'١٢'` raises `ValueError`.
- The month names in the date types are matched case-insensitively but
  looked up case-sensitively. So `21/nov/2011` raises `KeyError: 'nov'`.
  `12:30 am` is not corrected to 00:30, because the check is for `AM`.
- `ti` accepts a lower-case `z` and then fails to read it as an offset.
- A fill of `|` or `*` with `=` alignment goes into the pattern
  unescaped. `parse` reports that as a `NotImplementedError` about
  group names, while `search` lets the `PatternError` through.
- A width larger than the precision, as in `{:>4.3}`, makes a pattern
  `re` refuses.
- A custom type that leaves out its group count shifts every later
  positional field onto one of its own groups. `{:Range} {:d}` on
  `3-6 10` gives `([3, 4, 5, 6], 3)`. The `(:?..)` groups of `%z` do
  the same.
- `{a} {a[b]}` raises `TypeError`, while `{a[b]} {a}` overwrites the
  dictionary.
- A strftime type is not escaped, so the `.` in `%Y.%m.%d` matches any
  character, and `strptime` then rejects what the pattern let through.
- In the BDD run, `the store sells "pear" at $0.75 each` is caught by the
  step defined before the quoted one, so the item is stored with its
  quotes.

Two things that depend on today's date are not ported, and no job uses
them. These are the `ts` type, which assumes the current year, and a
strftime date with no year in it. `strptime` directives that parse has no
pattern for (`%Z`, `%c`, `%x`, `%X`, `%r`, `%G`, `%V` and `%u`) are
reported as bad directives. Case-insensitive matching uses the simple
lowercase mapping that `mini_re` uses. A field name counts as a name when
Rust's `is_alphabetic` accepts its first character, which is close to
Python's `isalpha` but not identical.

The port matched `expected.out` on its first run. It needed no
workarounds and added no new ledger rows.

## License

parse is under the MIT license in `LICENSE-MIT`:

    Copyright (c) 2012-2019 Richard Jones <richard@python.org>

That covers the package in `parse/`.
