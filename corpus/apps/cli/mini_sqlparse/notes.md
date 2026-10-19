# mini_sqlparse

sqlparse 0.5.3 as Python 3.13 runs it. sqlparse is the non-validating SQL
parser behind Django's `sqlmigrate` output, many SQL formatters and editor
plugins, and query-log tools. It splits a script into statements, builds a
loose token tree, and pretty-prints SQL. The lexer is a list of regular
expressions tried in order at each position, so the regex engine of
`mini_re` (Python's `re`, ported from `_parser.py` and sre) is part of this
app. The port covers:
- The lexer. `SQL_REGEX` and the keyword dictionaries of every dialect are
  read out of the package by `gen_tables.py`, in the order the default
  `Lexer` loads them. A word goes through the keyword lookup when its
  pattern says `PROCESS_AS_KEYWORD`, and the first dictionary that holds it
  decides its type.
- The statement splitter, with its `BEGIN`/`END` depth, `DECLARE`, `CASE`,
  `CREATE ... FUNCTION` levels and the T-SQL `GO` lines.
- Grouping, pass by pass in the library's order: comments, brackets,
  parenthesis, `CASE`, `IF`, `FOR`, `BEGIN`, `OVER`, functions, `WHERE`,
  periods, arrays, identifiers, ordering, typecasts, time-zone casts, typed
  literals, operators, comparisons, `AS`, aliases, assignments, comment
  alignment, identifier lists and `VALUES`.
- The token-tree API: names, real names, aliases, parent names, typecasts,
  ordering, wildcards, function parameters and windows, `CASE` arms,
  comparisons, statement types (including the look-through of a CTE), and
  `_pprint_tree`.
- The formatter and `validate_options` with all of its error messages:
  keyword and identifier case, comment stripping, whitespace stripping,
  spaces around operators, `strip_semicolon`, reindent with indent width,
  tabs, `indent_after_first`, `indent_columns`, `wrap_after`, `comma_first`
  and `compact`, the aligned reindent, string truncation, and the Python
  and PHP output wrappers.

The run writes `data/` and then works through the 254 jobs in
`data/jobs.jsonl` over a small corpus: migrations, an analytics report, a
MySQL dump, PL/pgSQL functions, a T-SQL report with `GO` batches, Oracle
hints, a SQLite schema, a Django query log, and some messy, broken,
Unicode and empty files.
- 48 token streams, of files and of short snippets aimed at single lexer
  rules.
- 21 splits, some with `strip_semicolon`, and 21 trees.
- 8 inspections, which print each group of each statement with what the
  API says about it.
- 150 formats. Each of 23 option sets formats two files. 26 bad option
  sets show the validation errors. 14 one-line queries are each formatted
  two ways, and 50 random queries get random option sets.
- Reports of the app's own:
  - a lint of each file in a directory, flagging `SELECT *`, an `UPDATE` or
    `DELETE` with no `WHERE`, and DDL;
  - a query-log summary that fingerprints each query by replacing its
    literals with `?`, then totals the time by fingerprint;
  - the tables a script reads and writes;
  - a split of a dump into one formatted file per statement, under
    `data/out/`, with the size, line count and first line of each.

`oracle.py` runs the same jobs through the package in `sqlparse/`, which
is the 0.5.3 wheel unchanged, and wrote `expected.out`.

sqlparse's quirks are kept. Some examples:
- `utils.imt` tests a single token type as a prefix (`ttype in T.Name`)
  but a tuple of them by equality, since `in` on a tuple compares its
  items. So `group_identifier`, which passes `(T.String.Symbol, T.Name)`,
  leaves `bigint` (a `Name.Builtin`) out of every identifier.
- A group's value is stored when the group is made or extended, so a
  later filter that changes its children leaves the group's own value
  stale.
- With an output format, the statement's tokens become a spent generator,
  so the reindent filter's look at the previous statement sees an empty
  one.
- `0` and `1` pass the boolean options, since `0 in [True, False]`.
- The error for `reindent_aligned` names the value of `reindent`.
- `right_margin` is validated, and then raises `NotImplementedError` from
  its filter.
- `Function.get_window()` on a function with no `OVER` raises
  `AttributeError`, and the inspection prints it.
- `insert into line_items (order_id, sku)` reads `line_items (...)` as a
  function call aliased to `line_items`.

The command-line tool is not ported, and the jobs pass no `encoding`, so
the byte-input path is not ported either. The lexer skips the patterns
that cannot start with the character at hand. `gen_tables.py` works the
first characters out from the parsed patterns, and checks the sets against
every text the jobs lex and against random strings. Case-insensitive
matching uses the simple lowercase mapping that `mini_re` uses.

The port needed no workarounds and added no new ledger rows.

## License

sqlparse is under the BSD 3-clause license in `LICENSE-BSD`:

    Copyright (c) 2016, Andi Albrecht <albrecht.andi@gmail.com>

That covers the package in `sqlparse/`. `AUTHORS` is the package's list of
contributors.
