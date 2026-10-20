# mini_marshmallow

marshmallow 4.3.1 by Steven Loria: the library Python services use to
turn the JSON they receive into checked Python values (`load`) and to
turn their values back into JSON (`dump`). Flask, webargs, apispec and
many REST APIs are built on it. The port follows the library closely:
- `Schema` with its options: `only` and `exclude` (dotted names that
  reach into nested schemas), `many`, `load_only`/`dump_only`, `partial`
  (as a flag or as dotted field names), `unknown` (`raise`, `include`,
  `exclude`), and `class Meta` (`fields`, `exclude`, `load_only`,
  `index_errors`, `many`, `unknown`, `dateformat`, `datetimeformat`);
- schema inheritance, `error_messages`, and the `ValueError`s a schema
  raises when it is built or made: invalid `only`/`exclude` names,
  colliding `data_key`s and `attribute`s, a required field with a
  default, a bad `TimeDelta` precision;
- the fields: `Raw`, `String`, `Integer` (`strict`), `Float`
  (`allow_nan`), `Boolean` (its truthy and falsy sets), `UUID`,
  `DateTime` (ISO, timestamps, `strptime` formats), `NaiveDateTime`,
  `AwareDateTime`, `Date`, `Time`, `TimeDelta`, `Url`, `Email`, `List`,
  `Tuple`, `Dict` (key and value fields), `Nested`, `Pluck`, `Enum` (by
  name, by value, by a field's value), `Method`, `Function` and
  `Constant`, each with `data_key`, `attribute` (dotted too),
  `required`, `allow_none`, `load_default`/`dump_default` and their own
  `pre_load`/`post_load` functions;
- the validators: `Range`, `Length`, `OneOf` (with labels), `NoneOf`,
  `ContainsOnly`, `ContainsNoneOf`, `Equal`, `Regexp`, `Email` and
  `URL`, with their messages;
- the hooks: `pre_load`, `post_load`, `pre_dump`, `post_dump` (with
  `pass_collection` and `pass_original`), `validates` and
  `validates_schema` (`skip_on_field_errors`), in marshmallow's order;
- the errors: `ErrorStore` merging messages into one tree, indexes for
  list items, and the valid data a failed load hands back.

The app's schemas add what a service would: hooks that unwrap an
envelope, drop nulls, check passwords match, compute an order total,
add links and slugs, and wrap a dump in an envelope; field functions
(`strip`, `lower`, `upper`, `split_commas`, `cents`); validators
(`even`, `not_blank`); and methods (`display_name`, `full_name`,
`split_name`).

`mk.py` writes `data/`:
- `schemas.json`: the enums and the schemas: addresses, user accounts
  and signups, orders and their items, events, metrics, service
  configs, articles with comment threads, trees, product feeds,
  contacts, legacy records, strict records, envelopes, and four schemas
  marshmallow refuses to build;
- `docs/`: the documents a service would receive, one JSON value per
  line, good and bad (including `NaN`, `Infinity` and a duplicated key);
- `jobs.tsv`: the jobs. `load`, `validate` and `roundtrip` load each
  document (and dump it back); `dump` loads with one schema and dumps
  with another; `init` makes a schema and prints its fields.

Each job prints `-- ` and its first fields. A document prints `> ` and
itself, then `ok` and the loaded value, or `errors` and `valid`; a dump
prints `= ` and the JSON. Values are printed with `repr`, dict keys in
order.

`oracle.py` runs the jobs through marshmallow itself (the copy in
`marshmallow/`, unchanged) and wrote `expected.out`. `gen_kara.py` writes
the data section.

The Email, URL and Regexp validators match with the regex engine of
`mini_re` (Python's `re`), and the temporal fields parse and format with
`mini_datetime`'s port of `fromisoformat`, `strptime` and `strftime`.

The quirks are kept. Some examples:
- a hook's or a field validator's failure removes the field from the
  valid data, but a `post_load` failure hands back what the earlier
  hooks made;
- `Integer` turns `3.9` into `3` and `" 42 "` into `42`, and rejects
  `True`; `TimeDelta` takes `True` as one unit;
- `Enum(by_value=True)` finds `0` for `False` and `1.0` for `1`;
- `UUID` accepts `urn:uuid:` and braces, and a `0x` prefix, because
  Python's `int(hex, 16)` does;
- a many-mode hook given a dict runs once per key.

One difference: the port keeps a `timedelta` in 64-bit microseconds, so
it treats a `TimeDelta` of more than about 104,000 days as invalid,
where Python allows up to 999,999,999 days.

marshmallow is under the MIT License (`LICENSE-marshmallow.txt`).
