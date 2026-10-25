# mini_voluptuous

voluptuous 0.15.2 as Python 3.13 runs it. voluptuous validates and
converts data against a schema written as plain Python structures, and
Home Assistant declares every integration's configuration schema with
it. A schema is a dict whose keys are strings, markers (`Required`,
`Optional`, `Remove`, `Exclusive`, `Inclusive`), classes or validators;
a list of alternatives; a literal; a class; or a validator. The port
covers:
- Schema compilation the way the library does it. `required` is read
  while a part is compiled, and `Any`, `All` and `SomeOf` set their own
  for what they hold. Key candidates are tried in `_sort_item` order,
  plain keys first. There are exclusion and inclusion groups, defaults,
  the three `extra` modes, `Extra`, `Self`, `Schema.extend` and
  `Schema.infer`.
- The validators `Any`, `All`, `SomeOf`, `Maybe`, `Msg`, `Coerce`,
  `Boolean`, `IsTrue`, `IsFalse`, `Email`, `Url`, `FqdnUrl`, `IsFile`,
  `IsDir`, `PathExists`, `Match`, `Replace`, `Range`, `Clamp`, `Length`,
  `Datetime`, `Date`, `In`, `NotIn`, `Contains`, `ExactSequence`,
  `Unordered`, `Unique`, `Equal` and `Number`. Also the string helpers
  `Lower`, `Upper`, `Capitalize`, `Title` and `Strip`, and `DefaultTo`,
  `SetTo` and `Literal`.
- Errors as the library builds them. `Invalid` keeps its path and its
  `error_type` ("for dictionary value"), and `str()` prints both.
  `MultipleInvalid` can nest inside another, a path is prepended as an
  error travels up, and `humanize_error` gives its sorted `... Got ...`
  lines.

`Match`, `Replace` and `Email` run on the regex engine of `mini_re`
(Python's `re`, ported from `_parser.py` and sre). `Email` uses the
library's own two regular expressions. `Datetime` and `Date` run on the
`_strptime` port from `mini_parse`, and `Url` on a port of
`urllib.parse.urlsplit`.

The schemas are JSON nodes in `data/schemas.json`. `oracle.py` turns
them into voluptuous objects, and `mini_voluptuous.kara` reads them as
they are. The run writes `data/` and then works through the 451 jobs in
`data/jobs.jsonl`. Each job prints the validated document with its keys
sorted, or the number of errors and `humanize_error`'s lines:

```
job 6 ha_light configs/lights/hall.json: 5 errors
  expected float for dictionary value @ data['transition']. Got 'slow'
  expected int for dictionary value @ data['brightness']. Got 'dim'
  expected int for dictionary value @ data['rgb_color']. Got [10, 20, '30']
  extra keys not allowed @ data['area']. Got 'hallway'
  extra keys not allowed @ data['colour']. Got 'warm'
```

The jobs are:
- Seven Home Assistant light configurations, checked against a light
  platform schema with color modes in an exclusion group and an effect
  and its speed in an inclusion group. Three are also checked against a
  zigbee schema that extends it, and two against copies that allow or
  remove extra keys.
- Five automations, with state, time, numeric and sun triggers and
  service-call and delay actions.
- Five web server configurations, under `required=True`, that point at
  the certificates, the document root and the log directory under
  `data/files/`.
- Three compose files, two menus whose items hold more menus through
  `Self`, two feature flag files, five sign-up forms, three invoices and
  four sensor readings.
- Three schemas inferred from documents, printed, and other documents
  checked against them.
- 412 one-validator jobs that take each validator to its edges.

`oracle.py` runs the same jobs through the package in `voluptuous/`,
which is the 0.15.2 sources unchanged without their tests, and wrote
`expected.out`. Its output is the same over 25 runs, so no set order
leaks into it.

voluptuous's quirks are kept. Some examples:
- `All` and `Any` compile what they hold with their own
  `required=False`. Under the web server's `required=True`, an upstream
  inside `All([upstream], Length(min=1))` has no required keys, while
  the `tls` dict beside it does.
- An `Any` that fails reports its alternative's error with the longest
  path, the first of them on a tie. `Maybe(Url())` on `'dave.example.com'`
  says `not a valid value`, which is the `None` alternative's error.
- `SomeOf` raises its error with no path, so it is reported "for
  dictionary value" with no `@ data[...]`, and `humanize_error` shows the
  whole document. Its message joins the `str()` of each failure, paths
  included. With `max_valid` exceeded and no failures the message is
  empty.
- `ExactSequence` with no `msg` raises `ExactSequenceInvalid(None)`,
  printed as `None`. Errors inside it have no index in their path.
- `Schema([])` on a non-empty list at the top uses the list itself as
  the error's path, so `[1, 'a']` gives `@ data[1]['a']`.
- A required key that is a class or a validator shows in the path as
  its repr: `data[<class 'str'>]`, `data[Match('^x-', msg=None)]`.
- `Remove('x')` only removes `x` when no later key takes it, so with a
  `str` key beside it `{'x': 1}` stays.
- `PathExists('')` says `Not a Path`, not `path does not exist`.
- `Schema.extend` with a plain `'platform'` key replaces
  `Required('platform')`, so the extended schema no longer requires it.
- `Unique` on `[1, True]` finds a duplicate, `Unique` on a string checks
  its characters, and `Unique` on an int reports `'int' object is not
  iterable`.
- `Number` counts the digits of the coefficient, so `'0.00'` has a
  precision of 1.

Not ported: `Object`, sets and tuples (JSON has none), discriminants on
`Any`, custom error classes, and `Number(yield_decimal=True)`. `Url`'s
check of a bracketed IPv6 host is not ported, and no job uses one.
`Decimal` reads ASCII digits only. As in `mini_parse`, `Lower`, `Upper`
and `Title` use the simple case mappings, and `Title` counts a letter as
cased when Rust's `is_alphabetic` accepts it.

## Edits from mini_voluptuous.orig.kara to mini_voluptuous.kara

`karac check` raised 35 errors and `karac fix` applied all 35. 26 were
missing `mut` markers on `mut ref Env` arguments. Five were moves out of
borrowed places: a bare `for` over an owned `Vec` borrows its elements,
so `prepend(x, path)` and the rebuilt `Inv` needed `.clone()`. Three
were matches that moved an `Option` or a `Vec` that was used again, and
the fix bound them as `ref`. One was a fieldless enum parameter that
had to be `own`. All of them were mistakes in the port, not compiler
gaps. After the fix it matched `expected.out` on its first run under the
MIR interpreter, the LLVM backend, the ASan build and the native binary,
with nothing on stderr.

## Running

```bash
python3 -s -P -B mk.py                      # writes data/
python3 -I -B oracle.py > expected.out
karac run mini_voluptuous.kara              # writes data/ itself, removes it at the end
karac build --mir mini_voluptuous.kara && ./mini_voluptuous
```

## License

voluptuous is under the BSD 3-clause license in `LICENSE-BSD`:

    Copyright (c) 2010, Alec Thomas

That covers the package in `voluptuous/`.
