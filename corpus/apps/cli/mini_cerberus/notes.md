# mini_cerberus

Cerberus 1.3.8 by Nicola Iarocci, the schema validator Python services
use to check request bodies, configuration files and documents (Eve, the
REST framework, is built on it). The port follows the library closely:
- `Validator` with its rules: `type` (the twelve built-in types),
  `required`, `require_all`, `nullable`, `empty`, `min`/`max`,
  `minlength`/`maxlength`, `allowed`, `forbidden`, `contains`, `regex`,
  `dependencies` (lists and value mappings, dotted and `^`-rooted
  paths), `excludes`, `readonly`, `schema` for mappings and sequences,
  `items`, `keysrules`, `valuesrules`, `check_with`, `meta`, and the
  `allof`/`anyof`/`noneof`/`oneof` rules with their `anyof_type`-style
  shortcuts;
- normalization: `rename`, `rename_handler`, `purge_unknown`,
  `purge_readonly`, `default`, `default_setter` (with its retry and its
  circular-dependency error), `coerce` chains, and normalizing inside
  mappings, sequences and `keysrules`/`valuesrules`;
- `allow_unknown` as a flag or as a schema, `ignore_none_values`,
  `update`, `validated`, `normalized`;
- the errors: `BasicErrorHandler`'s messages and its tree of nested
  dicts and lists, error ordering, and the group errors of nested and
  logical rules;
- schema checking: `DefinitionSchema` expanding the shortcuts, checking a
  schema against the rules' own schemas with `SchemaValidator`, and the
  `SchemaError` it raises;
- `schema_registry` and `rules_set_registry`.

`AppValidator` adds what an application would: coercers (`int`,
`float`, `strip`, `lower`, `split`, `yesno`, `date`, `snake`), default
setters (`slug`, `owner`, `author`, `count`) and checks (`even`, `luhn`,
`port`, `sorted`).

`mk.py` writes `data/`. It holds:
- `schemas/`: a signup form, a service configuration, orders that use
  registered `customer` and `address` schemas, a deployment manifest,
  sensor readings, CSV rows, blog posts, tag lists, logical-rule
  corners, products that use registered `money` and `positive` rules
  sets, and a strict schema;
- `docs/`: documents for each schema, one JSON document per line, good
  and bad;
- `broken.jsonl`: 30 schemas, broken in different ways, to check;
- `jobs.tsv`, the jobs, with the validator options each one uses.

Each job prints `== ` and its fields. A document prints `> ` and
itself, then whether it is valid, the errors and the document after
normalization, the way `oracle.py` prints them with `repr`.

`oracle.py` runs the jobs through Cerberus itself (the copy in
`cerberus/`, unchanged) and wrote `expected.out`. `gen_kara.py` writes
the data section.

The quirks are kept. Some examples:
- a coercer's error carries Python's exception text, such as
  `'bool' object has no attribute 'lower'` or
  `invalid literal for int() with base 10: 'eighty'`;
- a field that is null and not nullable gets both its coercion error and
  `null value not allowed`;
- checks still run on a value that already failed `nullable`;
- a rules set named as a string that is not registered expands to
  `None`, which the schema check reports as `null value not allowed`.

Cerberus is under the ISC License (`LICENSE-cerberus.txt`).
